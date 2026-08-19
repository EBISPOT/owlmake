//! Parse a (make-expanded) recipe line — a `robot` command chain or a shell
//! command — into plan [`Step`]s.
//!
//! Ingest only: this turns the command lines an ODK Makefile carries into the
//! plan's own vocabulary ([`crate::plan::step`]). Nothing here runs at build
//! time — by then the steps are in the plan and the Makefile is irrelevant.

pub use crate::plan::step::*;
use crate::build::recipe::FileOp;
use std::path::Path;

const SUBCOMMANDS: &[&str] = &[
    "merge", "reason", "relax", "reduce", "materialize", "remove", "filter", "annotate", "convert",
    "query", "verify", "report", "template", "export", "measure", "extract", "mirror", "repair",
    "rename", "expand", "collapse", "unmerge", "diff", "explain", "validate-profile", "reduce",
    "mireot", "rdfxml-to-json", "python",
];

const BENIGN_SHELL: &[&str] = &[
    "echo", "mv", "cp", "true", ":", "test", "[", "mkdir", "rm", "touch", "cat", "cd", "sort",
    "uniq", "printf", "date", "ls", "tee", "head", "tail", "cut",
    // `!` is sh's negation operator, not a program.
    "!",
    // A version banner: `odk-info` prints tool versions and has no build effect,
    // so classifying it is a plan-time decision recorded as its result. owlmake
    // serves the command word itself, which keeps a repo that spells it inline
    // from being refused.
    "odk-info",
];

/// Shell commands owlmake will actually RUN (by shelling out), as opposed to the
/// benign-and-ignorable ones above. These are standard, dependency-free text
/// processors and shell control constructs that appear in real ODK recipes (e.g.
/// MONDO's `filtered.obo` perl/grep xref pruning). An artefact containing any of
/// these is built by the recipe interpreter ([`super::recipe`]), which decomposes
/// each line and runs these leaf commands through `sh` — with the bundled tools
/// substituted by explicit binary path — so the artefact is genuinely built and
/// not recorded as a gap.
const RUNNABLE_SHELL: &[&str] = &[
    "perl", "grep", "egrep", "fgrep", "sed", "awk", "gawk", "tr", "comm", "join", "paste", "wc",
    "xargs", "dirname", "basename", "gzip", "gunzip", "zcat", "split", "fold", "rev", "tac", "nl",
    // `jq`/`sssom` are normally lifted into dedicated `Step::Jq`/`Step::Sssom`
    // steps before this list is consulted; they remain here as a backstop for
    // odd launchers, and are still served by the bundled engines.
    "jq", "sssom",
    // shell control constructs (a recipe line may begin with one when it spans an
    // `if … ; then … ; fi` or a `for`/`while` loop).
    "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
];

/// Does this command produce a FILE rather than terminal output? A text utility
/// that prints has no build effect; the same utility with a `>` redirect is how
/// a recipe writes its target. `tee` writes one either way.
fn writes_a_file(toks: &[String]) -> bool {
    toks.iter().any(|t| t == ">" || t == ">>") || toks[0] == "tee"
}

/// The make command word, however the recipe spells it (`$(MAKE)` expands to
/// `make`; a path-qualified `/usr/bin/gmake` is the same tool).
fn is_make(tok: &str) -> bool {
    matches!(tok, "make" | "gmake") || tok.ends_with("/make") || tok.ends_with("/gmake")
}

/// A flag that belongs to make itself and has no owlmake counterpart: forcing
/// (the default when a target is named), parallelism, silence, keep-going,
/// directory chatter.
fn is_make_flag(tok: &str) -> bool {
    matches!(
        tok,
        "-B" | "--always-make"
            | "-s" | "--silent" | "--quiet"
            | "-k" | "--keep-going"
            | "-i" | "--ignore-errors"
            | "-r" | "--no-builtin-rules"
            | "-R" | "--no-builtin-variables"
            | "--no-print-directory"
    ) || tok.starts_with("-j")
}

/// A shell step, with the command words owlmake cannot vouch for.
///
/// Vouched: the tools it bundles as PATH shims (`robot`, `jq`, `sssom`, `sed`,
/// `grep`, `comm`), the POSIX text processors and shell control words it knows
/// (`RUNNABLE_SHELL`), and the benign builtins (`BENIGN_SHELL`). Everything else
/// — `git`, `wget`, a project's own script — must exist in the environment, and
/// the plan says which.
pub(crate) fn shell_step(command: String) -> Step {
    let requires = unvouched_tools(&command);
    Step::Shell { command, requires }
}

/// The bundled tools, shimmed onto owlmake's own subcommands at execution.
///
/// This MUST stay in step with `crate::build::recipe::install_shims`: a command
/// word the shim dir serves but this list omits is reported by `unvouched_tools`
/// as a missing external dependency, and the plan's preflight then asks the user
/// to install something owlmake already provides.
pub(crate) const BUNDLED: &[&str] = &[
    "robot", "jq", "arq", "sssom", "sssom-cli", "kgx", "dosdp-tools", "dosdp",
    "owltools", "sed", "grep", "comm", "gzip", "gunzip", "zcat",
    // Helper command words a recipe can spell inline. Nothing else on the machine
    // provides them, so owlmake serves each one from its own implementation.
    "dicer-cli", "check-rdfxml", "odk-info", "sha256sum", "fastobo-validator",
    "simple_pattern_tester.py",
    // The ontology SQL database (`semsql make <name>.db`).
    "semsql",
];

/// Command words in `line` that owlmake cannot vouch for, deduplicated in first
/// appearance order. Each pipeline/sequence segment contributes its own command
/// word, so `git show x | robot convert` reports `git` and not `robot`.
fn unvouched_tools(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in line.split(['|', ';', '&']) {
        let toks = tokenize(seg);
        let Some(word) = toks.iter().find(|t| !is_env_assignment(t)) else { continue };
        // `!` negates a command in POSIX sh (`! grep -q x file`), so it is trimmed
        // off the command word rather than reported as a tool the machine needs.
        let word = word.trim_start_matches(['@', '+', '-', '(', '!']);
        let base = word.rsplit('/').next().unwrap_or(word);
        if base.is_empty()
            || base.starts_with('$')
            || BUNDLED.contains(&base)
            || BENIGN_SHELL.contains(&base)
            || RUNNABLE_SHELL.contains(&base)
            || is_shell_syntax(base)
            || is_python(base)
        {
            continue;
        }
        if !out.iter().any(|o| o == base) {
            out.push(base.to_string());
        }
    }
    out
}

/// Whether a token is shell syntax rather than the name of a program.
///
/// Segmenting a line on `|;&` is right for a pipeline but cuts a compound
/// command mid-construct, so what lands in the "command word" position is often
/// punctuation: a `case` arm's pattern (`*)`, `"")`), a `{ … }` group's braces,
/// or a builtin that terminates the shell rather than running anything. None of
/// them names a tool the machine has to provide, and a plan that says otherwise
/// asks for an install that cannot succeed.
fn is_shell_syntax(tok: &str) -> bool {
    // A `case` arm ends its pattern with `)`; nothing else in command position
    // does. Anything with no alphanumeric at all is pure punctuation.
    if tok.ends_with(')') || !tok.chars().any(|c| c.is_alphanumeric()) {
        return true;
    }
    matches!(
        tok,
        "exit"
            | "return"
            | "break"
            | "continue"
            | "shift"
            | "set"
            | "unset"
            | "export"
            | "eval"
            | "exec"
            | "trap"
            | "read"
            | "local"
            | "source"
            | "time"
            | "until"
            | "select"
            | "function"
    )
}

/// Whether a recipe command is a Python interpreter invocation (`python`,
/// `python3`, or a path ending in one).
fn is_python(tok: &str) -> bool {
    matches!(tok, "python" | "python3")
        || tok.ends_with("/python")
        || tok.ends_with("/python3")
}

/// Parse one shell command line (already variable-expanded) into steps.
pub fn parse_command(cmd: &str, robot_prefix: &str) -> Vec<Step> {
    // A shell `if … then … [else …] fi` construct is one logical command whose
    // internal `;` separators bind the block together — splitting it on `;` would
    // strand an `if` from its `then`/`fi`. Decompose it into a structured
    // [`Step::Branch`] (recursively, for nested `if`s) instead.
    let whole = cmd.trim().trim_start_matches(['@', '+']).trim();
    // A recipe line that is nothing but a shell comment. make passes it to the
    // shell, where it is a no-op — EFO has three, tab-indented section headers
    // sitting inside `trait_reports`' recipe. Recording it as a `shell` step would
    // make the plan claim work it does not do AND list `#` as a tool the machine
    // must provide. Nothing in a plan should be inert.
    if whole.is_empty() || whole.starts_with('#') {
        return vec![Step::Inert(whole.to_string())];
    }
    if whole.starts_with("if ") || whole.starts_with("if[") {
        if let Some(step) = parse_shell_if(whole, robot_prefix) {
            return vec![step];
        }
        // Unparseable control flow — keep it whole and runnable rather than split.
        return vec![shell_step(whole.to_string())];
    }
    // Other control-flow heads (`for`/`while`/`case`/`{`) stay a single shell op.
    if is_shell_block(whole) {
        return vec![shell_step(whole.to_string())];
    }
    // `exit` TERMINATES the shell — it does not merely fail — so a line containing
    // one cannot be decomposed without inverting its meaning. HPO's
    // `grep '^ERROR' hp_report && exit -1 || echo "No errors"` must die with 255
    // when the grep matches; split into steps, the `exit -1` reads as an ordinary
    // failing step and hands control to the `||`, so the check reports "No errors"
    // however many errors the report holds. Keep the line whole and let `sh` apply
    // its own short-circuiting.
    if split_shell(whole).iter().any(|p| {
        let p = p.trim().trim_start_matches(['@', '+', '-']).trim();
        p == "exit" || p.starts_with("exit ")
    }) {
        return vec![shell_step(whole.to_string())];
    }

    let mut steps = Vec::new();
    // `split_shell_seq` records the separator that FOLLOWS each part, so the one
    // that governs a part is its predecessor's. `&&` and `;`
    // need nothing recorded — steps already run in order and abort on failure — but
    // `||` inverts that, and dropping it would turn every error path into an
    // unconditional step. See `Step::Fallback`.
    let seq = split_shell_seq(cmd);
    for (idx, (sub, _)) in seq.iter().enumerate() {
        let after_or = matches!(idx.checked_sub(1).and_then(|p| seq[p].1), Some(ShellSep::Or));
        let sub = sub.as_str();
        // Strip make's per-recipe-line prefixes (`@` silent, `+` always-run) so
        // e.g. `@echo`/`@rm` are recognised as the underlying command.
        let sub = sub.trim().trim_start_matches(['@', '+']).trim();
        if sub.is_empty() {
            continue;
        }
        // Shell no-ops — the `true`/`:` builtins. On their own there is nothing
        // to record; as the tail of `cmd || true` they were folded into the
        // command that precedes them, below.
        if sub == "true" || sub == ":" {
            continue;
        }
        // `cmd || true` says cmd MAY FAIL. Dropping the `true` did not express
        // that — it left `cmd` an ordinary step whose failure aborts the recipe.
        // MONDO's OMIM-gene check is `grep -Ff $< mondo-edit.obo | grep '^xref' >
        // $@ || true`, and grep exits 1 when it matches nothing, which is the
        // PASSING case: the check fails only if the file it writes is non-empty.
        // Keeping the `|| true` in the command hands the semantics to the shell
        // that already runs it, and the plan reads as what happens.
        let tolerated = matches!(seq[idx].1, Some(ShellSep::Or))
            && matches!(seq.get(idx + 1), Some((next, _)) if next.trim() == "true" || next.trim() == ":");
        if tolerated {
            steps.push(shell_step(format!("{sub} || true")));
            continue;
        }
        // The right-hand side of `||` runs only on failure, whatever it parses as,
        // so it is recorded verbatim rather than decomposed.
        if after_or {
            let requires = unvouched_tools(sub);
            steps.push(Step::Fallback { command: sub.to_string(), requires });
            continue;
        }
        let toks = tokenize(sub);
        if toks.is_empty() {
            continue;
        }
        // Strip a leading run of `VAR=value` environment assignments (e.g.
        // UBERON's `OWLTOOLS_MEMORY=20G owltools …`), so the command word that
        // follows is what gets classified. The original `sub` string is kept for
        // any RunShell/Shell step that needs to be replayed verbatim.
        let toks: Vec<String> = {
            let skip = toks
                .iter()
                .take_while(|t| is_env_assignment(t))
                .count();
            if skip > 0 && skip < toks.len() {
                toks[skip..].to_vec()
            } else {
                toks
            }
        };
        // A pipeline (`a | b`) keeps shell semantics and is executed through the
        // shell (with the bundled tools substituted by explicit path), so record
        // it as a single runnable shell step rather than mis-parsing the stages.
        if crate::build::recipe::has_pipe(sub) {
            steps.push(shell_step(sub.to_string()));
            continue;
        }
        if is_robot(&toks, robot_prefix) {
            // A `sssom:` plugin command (e.g. `sssom:xref-extract`) is served by
            // the bundled `owlmake sssom` and writes its own target
            // (`--mapping-file $@`), so record the whole line as a runnable shell
            // step (the interpreter dispatches the bundled tool by explicit path)
            // rather than decomposing it into chained ops.
            if toks.iter().any(|t| t.starts_with("sssom:")) {
                steps.push(shell_step(sub.to_string()));
            } else {
                steps.extend(parse_robot_chain(&toks, robot_prefix));
            }
        } else if toks[0] == "owltools" || toks[0].ends_with("/owltools") {
            steps.extend(parse_owltools(&toks, sub));
        } else if toks[0] == "babelon" || toks[0].ends_with("/babelon") {
            steps.push(parse_babelon(&toks, sub));
        } else if toks[0] == "ontology-release-runner" || toks[0].ends_with("/ontology-release-runner") {
            steps.push(parse_oort(&toks));
        } else if crate::build::recipe::has_shell_substitution(sub) {
            // A command whose text contains `$(…)` or a backtick is not static: its
            // value is whatever the shell computes at run time. Neither a native
            // `FileOp` (which stores the text verbatim) nor `Shell` (which is
            // ignored) can carry that, so run it.
            //
            // EFO's mondo import counts its auto-excluded HGNC terms with
            // `echo "Auto-excluding $(wc -l < …hgnc.txt) HGNC terms…"`. Parsed as a
            // `Print`, it would announce the substitution instead of the count.
            steps.push(shell_step(sub.to_string()));
        } else if let Some(op) = FileOp::parse(&toks) {
            // cp/mv/rm/mkdir/touch → a native, declarative file operation.
            steps.push(Step::File(op));
        } else if toks[0] == "jq" || toks[0].ends_with("/jq") {
            steps.push(Step::Jq(toks[1..].to_vec()));
        } else if toks[0] == "dosdp-tools" || toks[0].ends_with("/dosdp-tools") {
            // `dosdp-tools generate` / `prototype` — DOSDP pattern expansion,
            // served by `owlmake dosdp`, so replay the line with the command word
            // substituted. OBA releases both of its outputs:
            // `patterns/definitions.owl` and `patterns/pattern.owl`.
            steps.push(shell_step(sub.to_string()));
        } else if toks[0] == "kgx" || toks[0].ends_with("/kgx") {
            // `kgx transform` — a KGX graph export, served by `owlmake kgx`, so
            // replay the line with the command word substituted.
            steps.push(shell_step(sub.to_string()));
        } else if toks[0] == "sssom-cli" || toks[0].ends_with("/sssom-cli") {
            // `sssom-cli` — a distinct command word from `sssom`, taking the
            // SSSOM/T grammar; replayed through the shell with the bundled
            // `owlmake sssom-cli` substituted for the command word.
            steps.push(shell_step(sub.to_string()));
        } else if toks[0] == "sssom" || toks[0].ends_with("/sssom") || toks[0].starts_with("sssom:") {
            steps.push(Step::Sssom(toks.clone()));
        } else if is_make(&toks[0]) {
            // Recursive make. The plan is the only instruction set at build time,
            // so a recipe that shells out to `make` has to become owlmake building
            // that target: `make IMP=false reports/x.txt -B` → `om make IMP=false
            // reports/x.txt`. Exactly the `robot`→om rewrite, for the other tool a
            // recipe can invoke.
            //
            // make-only flags are dropped: `-B`/`--always-make` is the default
            // here (a target named on the command line runs its steps), and the
            // rest are about make's own scheduling and output.
            let args: Vec<String> = toks[1..]
                .iter()
                .filter(|t| !is_make_flag(t))
                .cloned()
                .collect();
            steps.push(Step::CliRobot { name: "make".to_string(), args });
        } else if BENIGN_SHELL.contains(&toks[0].as_str()) && !writes_a_file(&toks) {
            steps.push(Step::Inert(sub.to_string()));
        } else if BENIGN_SHELL.contains(&toks[0].as_str()) {
            // …but the same utility with a REDIRECT builds a file, and a plan that
            // calls that benign claims work it does not do. MONDO's
            // `tmp/omim-genes.tsv` ends `tail -n +2 $@ > output_file && mv
            // output_file $@`: recorded as an inert `tail` plus the `mv`, the
            // header row survived, and `grep -Ff` then matched every line of
            // `mondo-edit.obo` containing the word `gene`.
            steps.push(shell_step(sub.to_string()));
        } else if RUNNABLE_SHELL.contains(&toks[0].as_str()) {
            steps.push(shell_step(sub.to_string()));
        } else if is_python(&toks[0]) {
            // A project's custom `python3 …` scripts (e.g. uPheno's
            // `upheno_build.py`) are run by shelling out, exactly like the bundled
            // perl/sed/awk recipe commands above. owlmake ships no Python of its
            // own; whether an interpreter (and the script's deps) is present is an
            // execution-environment concern — if it is missing the replayed recipe
            // line fails with the shell's usual error. Classifying it here keeps
            // the plan machine-independent (it does not probe the local PATH).
            steps.push(shell_step(sub.to_string()));
        } else {
            steps.push(shell_step(sub.to_string()));
        }
    }
    steps
}

/// The optional `true`/`false` value after a flag (`--flag true`); a bare flag takes `default`.
fn bool_arg(it: &mut std::iter::Peekable<std::vec::IntoIter<String>>, default: bool) -> bool {
    match it.peek() {
        Some(v) if !v.starts_with('-') => {
            let v = it.next().unwrap();
            matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        }
        _ => default,
    }
}

/// `babelon merge A.tsv B.tsv … [-o OUT]`. Any flag the recipe leaves out takes
/// its default: `--sort-tables` true, the other two false. HPO passes none of
/// them, so its merged table IS sorted.
fn parse_babelon_merge(toks: Vec<String>, sub: &str) -> Step {
    let mut inputs = Vec::new();
    let mut output = None;
    let mut sort_tables = true;
    let mut drop_unknown_columns = false;
    let mut update_translations = false;
    let mut it = toks.into_iter().peekable();
    while let Some(t) = it.next() {
        match t.as_str() {
            "-o" | "--output" => output = it.next(),
            "--sort-tables" => sort_tables = bool_arg(&mut it, true),
            "--drop-unknown-columns" => drop_unknown_columns = bool_arg(&mut it, true),
            "--update-translations" => update_translations = bool_arg(&mut it, true),
            s if s.starts_with('-') => {
                let _ = bool_arg(&mut it, false);
            }
            _ => inputs.push(t),
        }
    }
    match output {
        Some(output) if !inputs.is_empty() => {
            Step::File(crate::build::recipe::FileOp::BabelonMerge {
                inputs,
                output,
                sort_tables,
                drop_unknown_columns,
                update_translations,
            })
        }
        _ => shell_step(sub.to_string()),
    }
}

/// `babelon prepare-translation IN.tsv --oak-adapter … --language-code … --field …`.
fn parse_babelon_prepare(toks: Vec<String>, sub: &str) -> Step {
    let mut input = None;
    let mut oak_adapter = None;
    let mut language_code = None;
    let mut fields = Vec::new();
    let mut term_list = None;
    let mut output = None;
    let mut output_source_changed = None;
    let mut output_not_translated = None;
    let mut include_not_translated = false;
    let mut update_translation_status = true;
    let mut sort_tables = true;
    let mut drop_unknown_columns = false;
    let mut it = toks.into_iter().peekable();
    while let Some(t) = it.next() {
        match t.as_str() {
            "-o" | "--output" => output = it.next(),
            "--oak-adapter" => oak_adapter = it.next(),
            "--language-code" => language_code = it.next(),
            "--field" => {
                if let Some(f) = it.next() {
                    fields.push(f);
                }
            }
            "--term-list" => term_list = it.next(),
            "--output-source-changed" => output_source_changed = it.next(),
            "--output-not-translated" => output_not_translated = it.next(),
            "--include-not-translated" => include_not_translated = bool_arg(&mut it, true),
            "--update-translation-status" => update_translation_status = bool_arg(&mut it, true),
            "--sort-tables" => sort_tables = bool_arg(&mut it, true),
            "--drop-unknown-columns" => drop_unknown_columns = bool_arg(&mut it, true),
            s if s.starts_with('-') => {
                let _ = bool_arg(&mut it, false);
            }
            _ => {
                if input.is_none() {
                    input = Some(t);
                }
            }
        }
    }
    match (oak_adapter, language_code) {
        (Some(oak_adapter), Some(language_code)) => {
            Step::File(crate::build::recipe::FileOp::BabelonPrepare {
                input,
                oak_adapter,
                language_code,
                fields,
                term_list,
                output,
                output_source_changed,
                output_not_translated,
                include_not_translated,
                update_translation_status,
                sort_tables,
                drop_unknown_columns,
            })
        }
        _ => shell_step(sub.to_string()),
    }
}

/// Map a `babelon convert <tsv>` invocation to [`Op::Babelon`], carrying
/// `--output-format` through so execution knows whether to emit OWL annotation
/// axioms or the JSON table. `merge` and `prepare-translation` get their own
/// steps, and any other subcommand — or a `convert` with no input TSV — stays a
/// shell step.
fn parse_babelon(toks: &[String], sub: &str) -> Step {
    // Skip global flags (`-q`/`-v…`) to find the subcommand.
    let mut rest = toks[1..].iter().skip_while(|t| t.starts_with('-'));
    let subcmd = rest.next().map(|s| s.as_str());
    match subcmd {
        Some("merge") => return parse_babelon_merge(rest.cloned().collect(), sub),
        Some("prepare-translation") => {
            return parse_babelon_prepare(rest.cloned().collect(), sub)
        }
        _ => {}
    }
    if subcmd != Some("convert") {
        return shell_step(sub.to_string());
    }
    // First non-flag token after `convert` is the input TSV.
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut format: Option<String> = None;
    let mut it = rest.peekable();
    while let Some(t) = it.next() {
        match t.as_str() {
            "-o" | "--output" => output = it.next().cloned(),
            "--output-format" => format = it.next().cloned(),
            "--input-format" => { let _ = it.next(); }
            "--drop-unknown-columns" => {
                if it.peek().map(|s| !s.starts_with('-')).unwrap_or(false) { let _ = it.next(); }
            }
            s if s.starts_with('-') => {
                if it.peek().map(|v| !v.starts_with('-')).unwrap_or(false) { let _ = it.next(); }
            }
            _ => { if input.is_none() { input = Some(t.clone()); } }
        }
    }
    match input {
        Some(input) => Step::Op(Op::Babelon { input, output, format }),
        None => shell_step(sub.to_string()),
    }
}

/// Parse an `ontology-release-runner` invocation into an [`OortSpec`].
/// Flags consumed: `--reasoner <name>`, `--outdir <dir>`, `--simple`,
/// `--relaxed`, `--asserted`; the single positional is the source ontology.
/// Other flags (`--no-subsets`, `--allow-equivalent-pairs`, `--allow-overwrite`,
/// `--force`, …) don't change the artefacts this step produces and are ignored.
fn parse_oort(toks: &[String]) -> Step {
    let mut spec = OortSpec { reasoner: "ELK".into(), ..Default::default() };
    let mut it = toks[1..].iter().peekable();
    while let Some(t) = it.next() {
        match t.as_str() {
            "--reasoner" => { if let Some(v) = it.next() { spec.reasoner = v.clone(); } }
            "--outdir" => { if let Some(v) = it.next() { spec.outdir = v.clone(); } }
            "--simple" => spec.simple = true,
            "--relaxed" => spec.relaxed = true,
            "--asserted" => spec.asserted = true,
            // Other release-runner flags (`--no-subsets`,
            // `--allow-equivalent-pairs`, `--allow-overwrite`, `--force`, …) are
            // valueless and don't change the artefacts this step produces.
            s if s.starts_with('-') => {}
            _ => { if spec.input.is_empty() { spec.input = t.clone(); } }
        }
    }
    Step::Oort(spec)
}

/// Map an `owltools` command line to the equivalent owlmake operations. Such a line
/// is a left-to-right chain of operations, so each one becomes its own step; the
/// OBO/OWL output is the artefact's own format (no convert step needed). Operations
/// with no owlmake equivalent are reported as gaps.
fn parse_owltools(toks: &[String], sub: &str) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    // `-o [-f FORMAT] FILE` names a file the line WRITES. MONDO's roundtrip check
    // is `owltools --use-catalog mondo-edit.obo -o -f obo roundtrip.obo.tmp && mv
    // …`, so the write is the whole point of the line and the `mv` that follows
    // has nothing to rename without it.
    let mut out_file: Option<String> = None;
    let mut out_format: Option<String> = None;
    let mut i = 1;
    while i < toks.len() {
        let t = &toks[i];
        if t == "-o" {
            let mut j = i + 1;
            if toks.get(j).is_some_and(|f| f == "-f") {
                out_format = toks.get(j + 1).cloned();
                j += 2;
            }
            out_file = toks.get(j).cloned();
            i = j + 1;
            continue;
        }
        // Positionals (input files) and the remaining short flags are output
        // directives, not operations.
        if !t.starts_with("--") {
            i += 1;
            continue;
        }
        match t.as_str() {
            "--merge-imports-closure" | "--merge-import-closure" => {
                // Resolve and inline the import closure (then drop the imports).
                steps.push(Step::Op(Op::Merge { inputs: vec![], collapse_import_closure: None, restart: false }));
            }
            "--merge-axiom-annotations" => {
                steps.push(Step::Op(Op::Repair {
                    invalid_references: false,
                    merge_axiom_annotations: true,
                }));
            }
            "--extract-ontology-subset" => {
                // Followed (in any order) by `--fill-gaps` and `--subset NAME`.
                let mut subset = String::new();
                let mut fill_gaps = false;
                let mut j = i + 1;
                while j < toks.len() {
                    match toks[j].as_str() {
                        "--fill-gaps" => fill_gaps = true,
                        "--minimal" => fill_gaps = false,
                        "-s" | "--subset" => {
                            if j + 1 < toks.len() {
                                subset = toks[j + 1].clone();
                                j += 1;
                            }
                        }
                        "-u" | "--iri" | "--uri" | "-i" | "--input-file" => {
                            j += 1; // consume this option's value
                        }
                        other if other.starts_with("--extract") || other.starts_with("--make") => break,
                        _ => {}
                    }
                    j += 1;
                }
                i = j;
                steps.push(Step::Op(Op::ExtractOntologySubset { subset, fill_gaps }));
                continue;
            }
            "--extract-mingraph" => {
                steps.push(Step::Op(Op::ExtractMingraph));
            }
            "--remove-axiom-annotations" => {
                steps.push(Step::Op(Op::RemoveAxiomAnnotations));
            }
            "--make-subset-by-properties" => {
                // The property list follows, terminated by `//`, the next `--`
                // operation, or the `-o`/`-f` output directives. `-f`/`--force`
                // and `-n` are flags of this op, not list terminators.
                let mut properties: Vec<String> = Vec::new();
                let mut j = i + 1;
                while j < toks.len() {
                    let tk = &toks[j];
                    if tk == "//" {
                        j += 1;
                        break;
                    }
                    if tk == "-f" || tk == "--force" || tk == "-n" || tk == "--no-remove-dangling" {
                        j += 1;
                        continue;
                    }
                    if tk == "-o" || tk == "--out" || tk.starts_with("--") {
                        break;
                    }
                    properties.push(tk.clone());
                    j += 1;
                }
                i = j;
                steps.push(Step::Op(Op::MakeSubsetByProperties { properties }));
                continue;
            }
            // owlmake's OBO writer already emits property shorthands.
            "--add-obo-shorthand-to-properties" => {}
            "--use-catalog" | "--no-check" | "--silence-elk" => {}
            other => unknown.push(other.to_string()),
        }
        i += 1;
    }
    if !unknown.is_empty() {
        // An owltools line owlmake cannot map WHOLLY is replayed VERBATIM — the
        // `owltools` shim re-execs this binary, so the operations are owlmake's
        // own either way, and the plan then says exactly what runs.
        //
        // Verbatim means as written, not rebuilt from the `--` tokens.
        // Rebuilding drops every operand: MONDO's
        // `owltools --log-error --use-catalog $< --reasoner elk
        //  --merge-equivalence-sets -P MONDO -s MONDO 100 --remove-dangling -o $@`
        // reduces to `owltools --log-error --reasoner --merge-equivalence-sets
        // --remove-dangling` — no input, no reasoner name, no `-P MONDO`, no
        // output — and the plan would claim a check that could not run, which is
        // the failure mode `Step::Inert` exists to prevent.
        //
        // The partial op steps go with it: replaying the line runs them again.
        return vec![shell_step(sub.to_string())];
    }
    if steps.is_empty() {
        match out_file {
            // A pure format conversion: no operations, but it still WRITES the
            // file its `-o` names, in the format its `-f` gives.
            Some(output) => steps.push(Step::Op(Op::Convert {
                format: out_format,
                clean_obo: None,
                output: Some(output),
                add_prefixes: Vec::new(),
            })),
            // Nothing to do and nothing to write — the line only reads.
            None => steps.push(Step::Inert(sub.to_string())),
        }
    }
    steps
}

/// A leading shell environment assignment token, `NAME=value` where `NAME` is a
/// valid identifier (so a real command word like `x=y` never matches unless it
/// precedes the command — callers only test the leading run).
fn is_env_assignment(t: &str) -> bool {
    match t.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

pub(crate) fn is_robot(toks: &[String], robot_prefix: &str) -> bool {
    // The command is a `robot` invocation if it begins with the expanded $(ROBOT)
    // launcher, or its launcher token mentions robot/run.sh, or a known
    // subcommand appears right after a recognisable launcher.
    let prefix_toks: Vec<String> = tokenize(robot_prefix);
    if !prefix_toks.is_empty() && toks.len() >= prefix_toks.len() && toks[..prefix_toks.len()] == prefix_toks[..] {
        return true;
    }
    let first = &toks[0];
    (first.contains("robot") || first.ends_with("run.sh"))
        && toks.iter().any(|t| is_subcommand_token(t))
}

/// A token that begins a new subcommand in a `robot` command line: either a
/// built-in subcommand or a `prefix:command` plugin invocation (e.g.
/// `uberon:merge-species`).
fn is_subcommand_token(t: &str) -> bool {
    SUBCOMMANDS.contains(&t) || is_plugin_cmd(t)
}

/// `prefix:command` where prefix is alphanumeric and command is a lowercase
/// dashed word (distinguishes plugin commands from CURIE option values like
/// `oboInOwl:inSubset` or `RO:0002131`).
fn is_plugin_cmd(t: &str) -> bool {
    match t.split_once(':') {
        Some((p, c)) => {
            !p.is_empty()
                && p.chars().all(|x| x.is_ascii_alphanumeric() || x == '_')
                && c.starts_with(|x: char| x.is_ascii_lowercase())
                && c.chars().all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '-')
        }
        None => false,
    }
}

/// The argument tokens of a `robot` invocation with its launcher stripped — i.e.
/// everything from the first subcommand onward. Used to re-dispatch the chain
/// through the bundled `owlmake` binary, whose own subcommands serve these
/// command lines.
pub(crate) fn robot_subcommand_args(toks: &[String], robot_prefix: &str) -> Vec<String> {
    // Carry the launcher's GLOBAL options through, moved onto the subcommand.
    //
    // They live in two places: inside the launcher itself (MONDO's
    // `ROBOT = robot --catalog $(CATALOG)`) and between the launcher and the first
    // subcommand. `launcher_len` skips everything up to the subcommand, so
    // dropping them turns `robot --catalog catalog-v001.xml merge -i
    // mondo-edit.obo` into `om merge -i mondo-edit.obo` — no catalog, so
    // `resolve_import_closure` falls through to its NETWORK branch and merges the
    // PUBLISHED `merged_import.owl` instead of the repo's committed one. The two
    // differ, so every artefact downstream of the closure — MONDO's
    // `subsets/mondo-rare.*` among them — is built from the wrong imports.
    let prefix_toks = tokenize(robot_prefix);
    let matched = !prefix_toks.is_empty()
        && toks.len() >= prefix_toks.len()
        && toks[..prefix_toks.len()] == prefix_toks[..];
    let mut globals: Vec<String> = Vec::new();
    let mut i = if matched {
        globals.extend(prefix_toks[1..].iter().cloned());
        prefix_toks.len()
    } else {
        // The launcher word itself is not an argument; anything after it and
        // before the subcommand is.
        1.min(toks.len())
    };
    while i < toks.len() && !is_subcommand_token(&toks[i]) {
        globals.push(toks[i].clone());
        i += 1;
    }
    if i >= toks.len() {
        return Vec::new();
    }
    let mut out = vec![toks[i].clone()];
    out.extend(globals);
    out.extend(toks[i + 1..].iter().cloned());
    out
}

/// Index of the first subcommand token (past the `$(ROBOT)` launcher).
fn launcher_len(toks: &[String], robot_prefix: &str) -> usize {
    let prefix_toks = tokenize(robot_prefix);
    let mut i = if !prefix_toks.is_empty() && toks.len() >= prefix_toks.len() && toks[..prefix_toks.len()] == prefix_toks[..] {
        prefix_toks.len()
    } else {
        0
    };
    while i < toks.len() && !is_subcommand_token(&toks[i]) {
        i += 1;
    }
    i
}

fn parse_robot_chain(toks: &[String], robot_prefix: &str) -> Vec<Step> {
    // Skip the launcher prefix: drop tokens until the first subcommand.
    let mut i = launcher_len(toks, robot_prefix);
    let mut steps = Vec::new();
    while i < toks.len() {
        let name = toks[i].clone();
        i += 1;
        // Gather this subcommand's option tokens up to the next subcommand.
        let mut opts: Vec<(String, Vec<String>)> = Vec::new();
        while i < toks.len() && !is_subcommand_token(&toks[i]) {
            let tok = toks[i].clone();
            i += 1;
            if tok.starts_with('-') {
                let arity = option_arity(&name, &tok, toks.get(i));
                let mut vals = Vec::new();
                // Consume exactly `arity` tokens. For fixed-arity options the
                // value may itself look like a subcommand — annotation properties
                // are CURIEs such as `oboInOwl:date`/`rdfs:comment`, which must NOT
                // be mistaken for a `prefix:plugin` subcommand — so do not apply
                // the subcommand guard here (option_arity already returned 0 for a
                // heuristic option whose next token is a flag/subcommand).
                if arity == usize::MAX {
                    // List-valued: take every following token that is neither a
                    // flag nor a subcommand boundary.
                    while i < toks.len()
                        && !toks[i].starts_with('-')
                        && !is_subcommand_token(&toks[i])
                    {
                        vals.push(toks[i].clone());
                        i += 1;
                    }
                } else {
                    for _ in 0..arity {
                        if i < toks.len() {
                            vals.push(toks[i].clone());
                            i += 1;
                        }
                    }
                }
                opts.push((tok, vals));
            }
            // bare positional tokens: ignored (these subcommands rarely use them)
        }
        let step = map_subcommand(&name, &opts);
        // `-O`/`--output-iri` and `-V`/`--version-iri` may appear on many commands,
        // not just `annotate`: EFO's mondo import is
        // `extract … -O http://…/imports/mondo_import.owl`. Only `annotate` models
        // them as part of its own op, so for every other command record the effect
        // as the `annotate` it is equivalent to — otherwise the IRI lives nowhere
        // in the plan, and the module comes out still carrying the *source*
        // ontology's IRI.
        // On `verify`, `-O` is `--output-dir` (verify has no `-o` at all); on every
        // other command it is `--output-iri`. Reading verify's directory as an
        // ontology IRI would record a bogus trailing `annotate --ontology-iri
        // reports/` on every QC target that uses it.
        //
        // `--ontology-iri` is the THIRD spelling, and dropping it was a silent
        // content loss rather than a naming one: `template` takes that spelling
        // and no other, so EFO's `components/subclasses.owl` and
        // `import_replaced_by.owl` — built by `robot template … --ontology-iri
        // http://www.ebi.ac.uk/efo/components/…` — came out with NO ontology IRI
        // at all (`<Ontology/>`), and with the default xmlns falling back to the
        // OWL namespace so every class rendered `<Class>` rather than
        // `<owl:Class>`. Both files are `owl:imports` targets that
        // `catalog-v001.xml` resolves by exactly the IRI that went missing.
        let o_is_output_dir = name == "verify";
        let sets_iri = !matches!(step, Step::Op(Op::Annotate(_))) && !o_is_output_dir;
        // `convert` models its own `--output`; every other command's `-o` is a
        // process boundary (see below).
        let models_own_output = matches!(step, Step::Op(Op::Convert { .. }));
        steps.push(step);
        let find = |a: &str, b: &str| -> Option<String> {
            opts.iter().find(|(k, _)| k == a || k == b).and_then(|(_, v)| v.first().cloned())
        };
        if sets_iri {
            let (ontology_iri, version_iri) = (
                find("--output-iri", "-O").or_else(|| find("--ontology-iri", "--ontology-iri")),
                find("--version-iri", "-V"),
            );
            if ontology_iri.is_some() || version_iri.is_some() {
                steps.push(Step::Op(Op::Annotate(AnnotateSpec {
                    ontology_iri,
                    version_iri,
                    ..Default::default()
                })));
            }
        }
        // `-o` ends a command in the recipe: whatever reads the file next sees it
        // through a serialize/parse round trip. Record that so the pipeline — which
        // otherwise threads the model in memory — performs the round trip too.
        // (The rule's final `-o $@` is dropped by the caller — the pipeline's
        // closing write already is that write.)
        if !models_own_output {
            if let Some(out) = find("--output", "-o") {
                steps.push(Step::Op(Op::RoundTrip { path: out }));
            }
        }
    }
    steps
}

/// The shape of a SPARQL query, which decides both the extension its result
/// file takes when the command line gives no `--format` and whether the result
/// is a graph to merge or a table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryKind {
    /// `CONSTRUCT`/`DESCRIBE` — an RDF graph, written as Turtle.
    Graph,
    /// `SELECT` — a table, written as CSV.
    Table,
    /// `ASK` — a boolean, written as text.
    Boolean,
}

impl QueryKind {
    fn default_format(self) -> &'static str {
        match self {
            QueryKind::Graph => "ttl",
            QueryKind::Table => "csv",
            QueryKind::Boolean => "txt",
        }
    }

    fn builds_graph(self) -> bool {
        self == QueryKind::Graph
    }
}

/// Read a query file and classify its form. Comment lines and the prologue
/// (`PREFIX`/`BASE`) are skipped, so the first form word decides.
///
/// A file that cannot be read is reported as a `SELECT`, the same assumption the
/// name would carry with no other evidence; the step itself then fails when it
/// tries to read the query, which is what running the query file would do.
fn sparql_query_kind(path: &str) -> QueryKind {
    let Ok(text) = std::fs::read_to_string(path) else {
        return QueryKind::Table;
    };
    for raw in text.lines() {
        let line = raw.trim();
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let word = line.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
        match word.as_str() {
            "PREFIX" | "BASE" => continue,
            "CONSTRUCT" | "DESCRIBE" => return QueryKind::Graph,
            "ASK" => return QueryKind::Boolean,
            "SELECT" => return QueryKind::Table,
            _ => continue,
        }
    }
    QueryKind::Table
}

/// Options that take an unbounded list of values. `option_arity` gives an
/// unrecognised flag exactly one value, and `argv()` rebuilds argv from the
/// parsed pairs — so an undeclared list option loses every token after the first.
/// EFO's `sparql_test` passes eleven `--queries` paths; recording one would leave
/// `om sparql_test` running 1 of 11 violation checks and passing.
const MULTI_VALUE: &[(&str, &str)] = &[
    ("verify", "--queries"),
    ("query", "--queries"),
];

fn option_arity(cmd: &str, opt: &str, next: Option<&String>) -> usize {
    // `query --query/--select/--construct FILE OUTPUT` take a file plus a
    // positional output file (ODK always supplies the output).
    if cmd == "query" && matches!(opt, "--query" | "-q" | "--select" | "-s" | "--construct" | "-c") {
        return 2;
    }
    // `annotate` short flags: `-a`/`--annotation` and `-l`/`--link-annotation`
    // each take a property + value pair (`-t`/`--typed-annotation` takes a third
    // datatype, but ODK builds don't use it). `-a` means something else for other
    // commands (`reason --annotate-inferred-axioms`), so gate on the subcommand.
    if cmd == "annotate" && matches!(opt, "-a" | "-l") {
        return 2;
    }
    // A list-valued option consumes every following token that is neither a flag
    // nor a subcommand boundary.
    if MULTI_VALUE.iter().any(|(c, o)| *c == cmd && *o == opt) {
        return usize::MAX;
    }
    // `-O` always takes one value, whatever it means for this subcommand (see
    // `o_is_output_dir` in `parse_robot_chain`).
    if opt == "-O" {
        return 1;
    }
    match opt {
        "--annotation" | "--link-annotation" | "--typed-annotation" => 2,
        "--remove-annotations" => 0,
        // A flag's value is the next token unless it is a *known* subcommand (a
        // real segment boundary). Crucially, do NOT treat a
        // plugin-shaped CURIE here — `--term rdfs:label`, `--term SO:0000704`,
        // `--prefix "oio: …"` — as a boundary; those are values. (Plugin
        // commands only start a segment, which the delimiter in
        // `parse_robot_chain` still recognises via `is_subcommand_token`.)
        _ => match next {
            Some(n) if !n.starts_with('-') && !SUBCOMMANDS.contains(&n.as_str()) => 1,
            _ => 0,
        },
    }
}

fn map_subcommand(name: &str, opts: &[(String, Vec<String>)]) -> Step {
    let val = |key: &str| -> Option<String> {
        opts.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.first().cloned())
    };
    let val2 = |a: &str, b: &str| -> Option<String> { val(a).or_else(|| val(b)) };
    let all = |key: &str| -> Vec<String> {
        opts.iter().filter(|(k, _)| k == key).filter_map(|(_, v)| v.first().cloned()).collect()
    };
    let boolv = |key: &str| -> Option<bool> { val(key).map(|s| s == "true") };
    // Every option token of this invocation, flattened back to argv order, for the
    // `CliRobot` steps that are executed by re-invoking the owlmake binary.
    let argv = || -> Vec<String> {
        opts.iter()
            .flat_map(|(k, v)| std::iter::once(k.clone()).chain(v.iter().cloned()))
            .collect()
    };
    let all2 = |a: &str, b: &str| -> Vec<String> {
        let mut v = all(a);
        v.extend(all(b));
        v
    };

    // `odk:` is a plugin namespace a recipe can spell; owlmake serves those
    // commands itself, so strip the prefix and treat e.g. `odk:normalize` as
    // `normalize`. (Other plugin prefixes — `flybase:`, … — are handled below.)
    let name = name.strip_prefix("odk:").unwrap_or(name);

    // A `prefix:command` plugin invocation → route the bare command to the
    // matching generic built-in (un-prefixed). Unknown plugin commands remain
    // uncovered.
    if let Some((_, bare)) = name.split_once(':') {
        let has = |key: &str| opts.iter().any(|(k, _)| k == key);
        return match bare {
            // uPheno chains this between `merge` and `remove` on the way to
            // `mirror/merged.owl`, so it threads the model rather than running as
            // its own command.
            "extract-upheno-relations" => Step::Op(Op::ExtractUphenoRelations {
                relations: all2("--relation", "-r"),
                terms: all2("--term", "-t"),
                term_files: all2("--term-file", "-T"),
                roots: all2("--root-phenotype", "-p"),
                root_files: all2("--root-phenotype-file", "-P"),
            }),
            "merge-equivalent-sets" => Step::Op(Op::MergeEquivalentSets {
                set_prefix: all2("-s", "--set-prefix"),
                label_prefix: all2("-l", "--label-prefix"),
                definition_prefix: all2("-d", "--definition-prefix"),
            }),
            "merge-species" => Step::Op(Op::MergeSpecies {
                batch_file: val2("--batch-file", "-b"),
                extended: has("--extended-translation") || has("-x"),
                gca_translate: has("--translate-gcas") || has("-g"),
                gca_delete: has("--remove-gcas") || has("-G"),
                remove_declarations: has("--remove-declarations") || has("-d"),
                taxon: val2("--taxon", "-t"),
                suffix: val2("--suffix", "-s"),
                properties: all2("--property", "-p"),
                included: all2("--include-property", "-q"),
            }),
            "rewrite-def" => Step::Op(Op::RewriteDef(RewriteDefSpec {
                sub: has("--sub-definitions") || has("-s"),
                dot: has("--dot-definitions") || has("-d"),
                null_definitions: has("--null-definitions") || has("-D"),
                no_ids: has("--no-ids"),
                include_obsolete: has("--include-obsolete"),
                filter_prefix: val2("--filter-prefix", "-f"),
                add_annotation: all("--add-annotation"),
                add_annotation_iri: all("--add-annotation-iri"),
            })),
            "create-species-subset" => {
                Step::CliRobot { name: name.to_string(), args: argv() }
            }
            // `kgcl:mint` is a pipeline op rather than a CLI re-invocation: EFO's
            // `allocate-definitive-ids` chains it into `convert`, so it has to
            // thread the model on.
            "mint" => Step::Op(Op::Mint {
                temp_id_prefix: val("--temp-id-prefix").unwrap_or_default(),
                id_range_name: val("--id-range-name").unwrap_or_default(),
                id_ranges: val("--id-ranges"),
            }),
            // owlmake has these as CLI commands but not as pipeline ops, so they
            // run from the recipe's command line. OBA releases
            // `reports/oba.owl-obo-report.tsv`, built by `robot report`; EFO's QC
            // and release-diff targets are eight `robot diff` invocations. Each
            // entry is a claim that the command reads its own inputs and writes
            // its own output — a command that THREADS a model (`mint`) must be a
            // real op instead, or the chain it sits in loses the model.
            "report" | "verify" | "validate-profile" | "measure" | "diff" | "export"
            | "export-prefixes" | "explain" | "mirror" => {
                Step::CliRobot { name: name.to_string(), args: argv() }
            }
            _ => Step::UnknownRobot(name.to_string()),
        };
    }

    match name {
        "normalize" => Step::Op(Op::Normalize {
            base_iris: all("--base-iri"),
            subset_decls: boolv("--subset-decls").unwrap_or(true),
            synonym_decls: boolv("--synonym-decls").unwrap_or(true),
            add_source: boolv("--add-source").unwrap_or(false),
        }),
        "merge-equivalent-sets" => Step::Op(Op::MergeEquivalentSets {
            set_prefix: all2("-s", "--set-prefix"),
            label_prefix: all2("-l", "--label-prefix"),
            definition_prefix: all2("-d", "--definition-prefix"),
        }),
        "template" => {
            let mut templates = all2("--template", "-t");
            templates.extend(all("--external-template"));
            let merge = opts
                .iter()
                .any(|(k, _)| k == "--merge-before" || k == "--merge-after");
            // ROBOT's `--prefix "foo: http://bar"` binds a CURIE prefix for the
            // template's own header directives. UBERON's HRA components pass
            // `--prefix "dcterms: http://purl.org/dc/terms/"` and their headers say
            // `AI dcterms:contributor`; dropping the binding left `dcterms:` to the
            // OBO fallback, so 56 assertions per component came out as
            // `obo/dcterms_contributor` instead of `purl.org/dc/terms/contributor`
            // — a different IRI, not a different spelling. The binding is a build
            // input, so it belongs in the plan.
            let prefixes = all2("--prefix", "--add-prefix");
            Step::Op(Op::Template { templates, merge, prefixes })
        }
        "rename" => Step::Op(Op::Rename {
            mappings: val2("--mappings", "-m"),
            prefix_mappings: val2("--prefix-mappings", "-r"),
            allow_missing: boolv("--allow-missing-entities").or(boolv("-M")).unwrap_or(false),
        }),
        "extract" => Step::Op(Op::Extract {
            method: val2("--method", "-m").unwrap_or_else(|| "BOT".into()),
            terms: all2("--term", "-t"),
            term_files: all2("--term-file", "-T"),
            copy_ontology_annotations: boolv("--copy-ontology-annotations").unwrap_or(false),
            individuals: val("--individuals"),
            branch_from_terms: all("--branch-from-term"),
            branch_from_term_files: all("--branch-from-terms"),
        }),
        "collapse" => Step::Op(Op::Collapse {
            precious: {
                let mut v = all2("-r", "--precious");
                v.extend(all("--term"));
                v
            },
            precious_files: {
                let mut v = all2("-R", "--precious-terms");
                v.extend(all("--term-file"));
                v
            },
            threshold: val2("--threshold", "-t").and_then(|t| t.parse().ok()),
        }),
        "expand" => Step::Op(Op::Expand {
            expand_terms: all2("--expand-term", "-t"),
            expand_term_files: all2("--expand-term-file", "-T"),
            no_expand_terms: all2("--no-expand-term", "-n"),
            no_expand_term_files: all2("--no-expand-term-file", "-N"),
        }),
        // ODK `odk:subset` (the `odk:` prefix is stripped above).
        // Two modes. UBERON's fourteen `*-minimal` subsets are QUERY mode —
        // `odk:subset -i $< -r whelk -a true --query "BFO:0000050 some UBERON:…"
        // --query UBERON:… -o $@` — and recording only `--subset` left them with
        // an empty selector, so each built from nothing and reported success.
        // Note `-a` is `--ancestors`, not `--fill-gaps`.
        "subset" => Step::Op(Op::Subset {
            subset: val("--subset").unwrap_or_default(),
            queries: all2("--query", "-q"),
            terms: all2("--term", "-t"),
            term_files: all2("--term-file", "-T"),
            reasoner: val2("--reasoner", "-r"),
            ancestors: val2("--ancestors", "-a").map(|s| s == "true"),
            fill_gaps: boolv("--fill-gaps"),
        }),
        // `-I/--input-iri` is an input like any other, so it belongs in the same
        // list — the plan then NAMES the IRI the build reads. Dropping it left
        // `robot merge -I <url> convert -o tmp/omim.owl` as a bare `op: merge`
        // with no inputs: an inert step that wrote a 658-byte empty ontology, and
        // MONDO's OMIM-gene QC check then grepped `mondo-edit.obo` for the one
        // word its empty report left behind.
        "merge" => Step::Op(Op::Merge {
            inputs: all2("--input", "-i")
                .into_iter()
                .chain(all2("--input-iri", "-I"))
                .collect(),
            collapse_import_closure: boolv("--collapse-import-closure"),
            // Set by the planner for a merge that opens a recipe's second or
            // later command line; one line's parse cannot see where it sits.
            restart: false,
        }),
        // As with `merge`, `-I/--input-iri` names an input: CL subtracts the taxon
        // disjointness axioms with `unmerge -I <url>`, and reading only `-i` left
        // an `unmerge` with nothing to subtract — `cl-plus.owl` kept every
        // disjointness axiom, which is what the step exists to remove.
        "unmerge" => Step::Op(Op::Unmerge {
            second_input: val2("--input", "-i").or_else(|| val2("--input-iri", "-I")),
        }),
        "reason" => Step::Op(Op::Reason {
            reasoner: val2("--reasoner", "-r"),
            equivalent_classes_allowed: val2("--equivalent-classes-allowed", "-e"),
            exclude_tautologies: val2("--exclude-tautologies", "-t"),
            annotate_inferred_axioms: val2("--annotate-inferred-axioms", "-a").map(|s| s == "true"),
            allow_incoherent: boolv("--allow-incoherent"),
            exclude_external_entities: val2("--exclude-external-entities", "-X").map(|s| s == "true"),
            exclude_owl_thing: val2("--exclude-owl-thing", "-T").map(|s| s == "true"),
            remove_redundant_subclass_axioms: val2("--remove-redundant-subclass-axioms", "-s")
                .map(|s| s == "true"),
            create_new_ontology: val2("--create-new-ontology", "-n").map(|s| s == "true"),
            create_new_ontology_with_annotations: val2(
                "--create-new-ontology-with-annotations",
                "-m",
            )
            .map(|s| s == "true"),
            exclude_duplicate_axioms: val2("--exclude-duplicate-axioms", "-x").map(|s| s == "true"),
        }),
        "relax" => Step::Op(Op::Relax {
            include_subclass_of: boolv("--include-subclass-of").unwrap_or(false),
        }),
        "reduce" => Step::Op(Op::Reduce {
            reasoner: val2("--reasoner", "-r"),
            include_subproperties: boolv("--include-subproperties").or_else(|| boolv("-s")),
        }),
        "materialize" => Step::Op(Op::Materialize {
            properties: { let mut p = all("--property"); p.extend(all("-P")); p },
            term_files: { let mut f = all("--term-file"); f.extend(all("-T")); f },
        }),
        "remove" => remove_step(RemoveSpec {
            terms: { let mut t = all("--term"); t.extend(all("-t")); t },
            term_files: { let mut f = all("--term-file"); f.extend(all("-T")); f },
            axioms: all("--axioms"),
            selects: all("--select"),
            base_iri: all("--base-iri"),
            trim: boolv("--trim"),
            preserve_structure: boolv("--preserve-structure"),
            exclude_terms: { let mut e = all("--exclude-term"); e.extend(all("-e")); e },
            exclude_term_files: { let mut e = all("--exclude-terms"); e.extend(all("-E")); e },
            signature: boolv("--signature"),
            drop_axiom_annotations: val("--drop-axiom-annotations").or_else(|| val("-d")),
        }),
        "filter" => filter_step(FilterSpec {
            terms: { let mut t = all("--term"); t.extend(all("-t")); t },
            term_files: { let mut f = all("--term-file"); f.extend(all("-T")); f },
            selects: all("--select"),
            signature: boolv("--signature"),
            trim: boolv("--trim"),
            axioms: all("--axioms"),
            prefixes: { let mut p = all("--prefix"); p.extend(all("--add-prefix")); p },
        }),
        "annotate" => {
            // Collect the property/value pairs for any of the given option
            // spellings (long + short) in recipe order.
            let pairs = |keys: &[&str]| -> Vec<(String, String)> {
                opts.iter()
                    .filter(|(k, _)| keys.contains(&k.as_str()))
                    .filter_map(|(_, v)| {
                        if v.len() == 2 { Some((v[0].clone(), v[1].clone())) } else { None }
                    })
                    .collect()
            };
            Step::Op(Op::Annotate(AnnotateSpec {
                ontology_iri: val2("--ontology-iri", "-O"),
                version_iri: val2("--version-iri", "-V"),
                annotations: pairs(&["--annotation", "-a"]),
                link_annotations: pairs(&["--link-annotation", "-l"]),
                remove_annotations: opts
                    .iter()
                    .any(|(k, _)| k == "--remove-annotations" || k == "-R"),
            }))
        }
        "convert" => Step::Op(Op::Convert {
            format: val2("--format", "-f"),
            clean_obo: val("--clean-obo"),
            output: val2("--output", "-o"),
            add_prefixes: all("--add-prefixes"),
        }),
        "query" => {
            let pairs = |keys: &[&str]| -> Vec<(String, String)> {
                opts.iter()
                    .filter(|(k, _)| keys.contains(&k.as_str()))
                    .filter_map(|(_, v)| {
                        if v.len() >= 2 {
                            Some((v[0].clone(), v[1].clone()))
                        } else {
                            None
                        }
                    })
                    .collect()
            };
            // `--queries Q…` names no output: each query's result goes to
            // `<--output-dir>/<query basename>.<format>`, which is the file the
            // NEXT step reads — MONDO's `tmp/mondo-tags-sparql.ttl` runs five tag
            // queries into `tmp/` and then merges the five `.ttl`s. So the output
            // name is resolved HERE and recorded as an ordinary (query, output)
            // pair; left to run time the plan would name neither the queries nor
            // the files they write.
            let mut queries_selects: Vec<(String, String)> = Vec::new();
            let mut queries_constructs: Vec<(String, String)> = Vec::new();
            {
                let out_dir = val2("--output-dir", "-O").unwrap_or_default();
                let fmt_opt = val2("--format", "-f");
                let listed: Vec<String> = opts
                    .iter()
                    .filter(|(k, _)| k == "--queries" || k == "-Q")
                    .flat_map(|(_, v)| v.iter().cloned())
                    .collect();
                for q in listed {
                    let kind = sparql_query_kind(&q);
                    let fmt = fmt_opt.clone().unwrap_or_else(|| kind.default_format().to_string());
                    let stem = Path::new(&q)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let out = if out_dir.is_empty() {
                        format!("{stem}.{fmt}")
                    } else {
                        format!("{}/{stem}.{fmt}", out_dir.trim_end_matches('/'))
                    };
                    if kind.builds_graph() {
                        queries_constructs.push((q, out));
                    } else {
                        queries_selects.push((q, out));
                    }
                }
            }
            Step::Op(Op::Query {
                updates: all2("--update", "-u"),
                selects: {
                    let mut v = pairs(&["--query", "-q", "--select", "-s"]);
                    v.extend(queries_selects);
                    v
                },
                constructs: {
                    let mut v = pairs(&["--construct", "-c"]);
                    v.extend(queries_constructs);
                    v
                },
                format: val2("--format", "-f"),
                use_graphs: val2("--use-graphs", "-g")
                    .is_some_and(|v| v.eq_ignore_ascii_case("true")),
                // `--tdb`/`--create-tdb`/`--temporary-file` all ask for an on-disk
                // dataset, so any of them sets the same flag: what matters is the
                // on-disk choice, not which spelling requested it, because that is
                // what decides an unordered `SELECT`'s row order (see
                // `Op::Query::tdb`).
                tdb: ["--tdb", "-t", "--create-tdb", "-C", "--temporary-file"]
                    .iter()
                    .any(|k| {
                        val2(k, k).is_some_and(|v| v.eq_ignore_ascii_case("true"))
                            || opts.iter().any(|(o, v)| o == k && v.is_empty())
                    }),
            })
        }
        "repair" => Step::Op(Op::Repair {
            invalid_references: opts.iter().any(|(k, _)| k == "--invalid-references"),
            merge_axiom_annotations: opts
                .iter()
                .any(|(k, _)| k == "--merge-axiom-annotations" || k == "-m"),
        }),
        // …and the same set on the non-chained path.
        // Terminal commands: each reads its own inputs and writes a non-ontology
        // output (a report, a table, a prefix map, a mirror directory), leaving
        // the ontology untouched — so dispatching a fresh `om` subcommand IS the
        // operation, and no model has to thread through.
        "report" | "verify" | "validate-profile" | "measure" | "diff" | "export"
        | "export-prefixes" | "explain" | "mirror" => {
            Step::CliRobot { name: name.to_string(), args: argv() }
        }
        other => Step::UnknownRobot(other.to_string()),
    }
}

/// The shell operator that separates two commands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ShellSep {
    /// `;` — always run the next command.
    Semi,
    /// `&&` — run the next command only if the previous one succeeded.
    And,
    /// `||` — run the next command only if the previous one failed.
    Or,
}

/// Split a recipe line into commands plus the operator that *follows* each
/// (`None` for the last). Honours `;`, `&&` and `||` outside quotes, so callers
/// can reproduce shell short-circuiting (`cmd || true`, `a && b`).
pub(crate) fn split_shell_seq(s: &str) -> Vec<(String, Option<ShellSep>)> {
    let bytes = s.as_bytes();
    let mut parts: Vec<(String, Option<ShellSep>)> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut quote: Option<u8> = None;
    // Nesting depth of `( … )` and `{ … ; }`. A separator inside a group belongs to
    // the GROUP, not to the recipe, and splitting there hands `sh` an unterminated
    // construct. A profile check is exactly this shape —
    // `robot validate-profile … || { cat $@ && exit 1; }` — and cutting it at the
    // `&&` runs `{ cat reports/…` on its own: `syntax error: unexpected end of file`,
    // exit 2, a FAILED check on both HPO and OBA even though the validation itself
    // passed and wrote its report.
    let mut depth: i32 = 0;
    // A `{` opens a group only when it stands as its own word; `${VAR}` and brace
    // expansion must not count. Same for the closing `}`.
    let word_start = |i: usize| -> bool {
        bytes[..i]
            .iter()
            .rev()
            .find(|c| !matches!(c, b' ' | b'\t'))
            .is_none_or(|c| matches!(c, b';' | b'&' | b'|' | b'(' | b'{' | b'\n'))
    };
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => {
                let sep = match b {
                    b'"' | b'\'' | b'`' => {
                        quote = Some(b);
                        None
                    }
                    b'(' => {
                        depth += 1;
                        None
                    }
                    b')' => {
                        depth -= 1;
                        None
                    }
                    b'{' if word_start(i)
                        && matches!(bytes.get(i + 1), Some(b' ' | b'\t' | b'\n')) =>
                    {
                        depth += 1;
                        None
                    }
                    b'}' if word_start(i) => {
                        depth -= 1;
                        None
                    }
                    b'&' if i + 1 < bytes.len() && bytes[i + 1] == b'&' => Some(ShellSep::And),
                    b'|' if i + 1 < bytes.len() && bytes[i + 1] == b'|' => Some(ShellSep::Or),
                    b';' => Some(ShellSep::Semi),
                    _ => None,
                };
                let sep = if depth > 0 { None } else { sep };
                if let Some(sep) = sep {
                    parts.push((s[start..i].to_string(), Some(sep)));
                    i += if matches!(sep, ShellSep::Semi) { 1 } else { 2 };
                    start = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    parts.push((s[start..].to_string(), None));
    parts
}

/// Split a shell command line on top-level `&&`, `;`, `||` (ignoring inside
/// quotes), discarding which operator separated each pair.
pub(crate) fn split_shell(s: &str) -> Vec<String> {
    split_shell_seq(s).into_iter().map(|(p, _)| p).collect()
}

/// Whether a command begins with a shell control-flow head we keep intact
/// (`for`/`while`/`until`/`case`/`{`). `if` is handled separately (decomposed).
fn is_shell_block(cmd: &str) -> bool {
    let first = cmd.split_whitespace().next().unwrap_or("");
    matches!(first, "for" | "while" | "until" | "case" | "{")
}

/// Split a shell line on top-level `;` only (quote-aware), leaving `&&`/`||` and
/// pipes inside each statement — used to segment an `if … then … fi` block.
fn split_semicolons(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let (mut start, mut i, mut quote) = (0usize, 0usize, None::<u8>);
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b';' => {
                    parts.push(s[start..i].to_string());
                    start = i + 1;
                }
                _ => {}
            },
        }
        i += 1;
    }
    parts.push(s[start..].to_string());
    parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// One token of a segmented shell `if` block.
enum IfTok {
    If,
    Then,
    Else,
    Fi,
    Stmt(String),
}

/// Decompose a shell `if … then … [else …] fi` line into a structured
/// [`Step::Branch`], recursing into the bodies (so nested `if`s and the ordinary
/// statements inside each branch are parsed by [`parse_command`] as usual).
/// Returns `None` if the block isn't a well-formed `if … fi`.
fn parse_shell_if(block: &str, robot_prefix: &str) -> Option<Step> {
    let mut toks = Vec::new();
    for seg in split_semicolons(block) {
        // Peel a leading keyword off the segment; the remainder (if any) is the
        // condition test (after `if`) or the first body statement (after `then`).
        let (kw, rest): (Option<IfTok>, &str) = if seg == "fi" {
            (Some(IfTok::Fi), "")
        } else if let Some(r) = seg
            .strip_prefix("else ")
            .or_else(|| (seg == "else").then_some(""))
        {
            // make's line-continuation join leaves ODK's
            // `…; then \` / `echo A ; else \` / `echo B ; fi` as the single
            // segment `else\t\techo "…"`, which an `== "else"` test misses —
            // `config_check` (an `all_odk` prerequisite in OBA, CL and UBERON)
            // would then fail to parse and run as raw shell, with a syntax error.
            (Some(IfTok::Else), r.trim())
        } else if let Some(r) = seg.strip_prefix("if ") {
            (Some(IfTok::If), r.trim())
        } else if let Some(r) = seg.strip_prefix("then ").or_else(|| (seg == "then").then_some("")) {
            (Some(IfTok::Then), r.trim())
        } else {
            (None, seg.as_str())
        };
        match kw {
            Some(IfTok::If) => {
                toks.push(IfTok::If);
                if !rest.is_empty() {
                    toks.push(IfTok::Stmt(rest.to_string()));
                }
            }
            Some(IfTok::Then) => {
                toks.push(IfTok::Then);
                if !rest.is_empty() {
                    toks.push(IfTok::Stmt(rest.to_string()));
                }
            }
            Some(IfTok::Else) => {
                toks.push(IfTok::Else);
                if !rest.is_empty() {
                    toks.push(IfTok::Stmt(rest.to_string()));
                }
            }
            Some(other) => toks.push(other),
            None => toks.push(IfTok::Stmt(rest.to_string())),
        }
    }

    let mut pos = 0usize;
    let step = parse_if_at(&toks, &mut pos, robot_prefix)?;
    // The whole line must be exactly one balanced `if … fi`.
    if pos == toks.len() {
        Some(step)
    } else {
        None
    }
}

/// Parse a single `if` starting at `toks[*pos]`, advancing `*pos` past its `fi`.
fn parse_if_at(toks: &[IfTok], pos: &mut usize, robot_prefix: &str) -> Option<Step> {
    if !matches!(toks.get(*pos), Some(IfTok::If)) {
        return None;
    }
    *pos += 1;
    // Condition: the statement(s) between `if` and `then`.
    let mut cond = String::new();
    while let Some(IfTok::Stmt(s)) = toks.get(*pos) {
        if !cond.is_empty() {
            cond.push_str("; ");
        }
        cond.push_str(s);
        *pos += 1;
    }
    if !matches!(toks.get(*pos), Some(IfTok::Then)) {
        return None;
    }
    *pos += 1;
    let then_steps = parse_if_body(toks, pos, robot_prefix)?;
    let else_steps = if matches!(toks.get(*pos), Some(IfTok::Else)) {
        *pos += 1;
        parse_if_body(toks, pos, robot_prefix)?
    } else {
        Vec::new()
    };
    if !matches!(toks.get(*pos), Some(IfTok::Fi)) {
        return None;
    }
    *pos += 1;
    Some(Step::Branch { condition: Condition::parse(&cond), then_steps, else_steps })
}

/// Parse the statements of a branch body until its terminating `else`/`fi`,
/// recursing into nested `if`s. Leaves `*pos` on the terminating keyword.
fn parse_if_body(toks: &[IfTok], pos: &mut usize, robot_prefix: &str) -> Option<Vec<Step>> {
    let mut steps = Vec::new();
    while let Some(tok) = toks.get(*pos) {
        match tok {
            IfTok::Else | IfTok::Fi => break,
            IfTok::If => steps.push(parse_if_at(toks, pos, robot_prefix)?),
            IfTok::Stmt(s) => {
                steps.extend(parse_command(s, robot_prefix));
                *pos += 1;
            }
            // A stray `then` inside a body is malformed.
            IfTok::Then => return None,
        }
    }
    Some(steps)
}

/// The file a recipe line sends its console output to (`$(ROBOT) … reason > $@`).
///
/// A chained ontology command is recorded as structured steps, and those name
/// only the intermediates the chain writes with `-o`. The redirect is what names
/// the target, so it is carried separately; without it the rule claims to build a
/// file none of its steps mentions.
///
/// Only for a single, unpiped ontology command: any other shape is recorded as a
/// shell step, where the redirect is part of the command line and the shell that
/// replays it applies the redirect itself.
pub(crate) fn chain_stdout_file(cmd: &str, robot_prefix: &str) -> Option<String> {
    if crate::build::recipe::has_pipe(cmd) || split_shell_seq(cmd).len() != 1 {
        return None;
    }
    let toks = tokenize_quoted(cmd.trim().trim_start_matches(['@', '+']).trim());
    let mut dst: Option<String> = None;
    let mut cut = toks.len();
    for (i, (t, quoted)) in toks.iter().enumerate() {
        // A `>` inside quotes is an argument — `--select "<http://…/BFO_*>"` —
        // not a redirection.
        if *quoted {
            continue;
        }
        if (t == ">" || t == ">>") && i + 1 < toks.len() {
            dst = Some(toks[i + 1].0.clone());
            cut = i;
        } else if let Some(rest) = t.strip_prefix(">>").or_else(|| t.strip_prefix('>')) {
            if !rest.is_empty() {
                dst = Some(rest.to_string());
                cut = i;
            }
        }
    }
    let dst = dst?;
    let head: Vec<String> = toks[..cut].iter().map(|(t, _)| t.clone()).collect();
    if head.is_empty() || !is_robot(&head, robot_prefix) || head.iter().any(|t| t.starts_with("sssom:")) {
        return None;
    }
    Some(dst)
}

/// Quote-aware whitespace tokenizer.
pub(crate) fn tokenize(s: &str) -> Vec<String> {
    tokenize_quoted(s).into_iter().map(|(t, _)| t).collect()
}

/// As [`tokenize`], but also reports whether each token contained any QUOTED
/// text. The caller needs this to tell a shell redirection from an argument that
/// merely starts with `<` or `>`: an IRI-pattern selector is written
/// `--select "<http://purl.obolibrary.org/obo/BFO_*>"`, and the quotes are what
/// make it an argument rather than `< http://…` — an input redirection from a
/// file that does not exist. MONDO's `mondo-base.owl` recipe uses exactly that
/// form, so dropping the quoting makes replaying the rule through the shell fail
/// with "No such file or directory".
pub(crate) fn tokenize_quoted(s: &str) -> Vec<(String, bool)> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            // Single quotes take everything literally, backslash included.
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            // Inside double quotes a backslash escapes only `"`, `\`, `$` and a
            // backtick; before anything else it is an ordinary character. MONDO's
            // mappings query is `-Q "SELECT … IN (\"skos:exactMatch\", …)"`, and the
            // inner quotes are what make SQLite read those words as string
            // literals rather than column names — dropped, the query selects
            // nothing and `sssom dosql` fails on an unknown column.
            Some(_) => {
                if c == '"' {
                    quote = None;
                } else if c == '\\' {
                    match chars.peek() {
                        Some(&n) if matches!(n, '"' | '\\' | '$' | '`') => {
                            cur.push(n);
                            chars.next();
                        }
                        _ => cur.push('\\'),
                    }
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    has = true;
                    quoted = true;
                }
                // Unquoted, a backslash escapes whatever follows it.
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                        has = true;
                    }
                }
                c if c.is_whitespace() => {
                    if has {
                        toks.push((std::mem::take(&mut cur), quoted));
                        has = false;
                        quoted = false;
                    }
                }
                _ => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if has {
        toks.push((cur, quoted));
    }
    toks
}

#[cfg(test)]
mod annotate_tests {
    use super::*;

    /// EFO's main product is built by a hand-written (non-ODK) recipe whose
    /// `annotate` step uses the *short* flag spellings and backtick command
    /// substitutions: `-a owl:versionInfo <ver> -a rdfs:comment <date> -O <iri>
    /// -V <version-iri>`. The version/date backticks are evaluated by the
    /// Makefile layer before this point, so the parser has to capture the short
    /// forms (`-a`, `-O`, `-V`) the same way it does the long ones.
    #[test]
    fn efo_short_flag_annotate_is_parsed() {
        let recipe = "robot annotate \
            -a owl:versionInfo 3.90.0 \
            -a rdfs:comment 2026-06-25 \
            -O http://www.ebi.ac.uk/efo/efo.owl \
            -V http://www.ebi.ac.uk/efo/releases/v3.90.0/efo.owl \
            -o build/efo.owl";
        let steps = parse_command(recipe, "robot");
        let spec = steps
            .iter()
            .find_map(|s| match s {
                Step::Op(Op::Annotate(a)) => Some(a),
                _ => None,
            })
            .expect("annotate step should be parsed");

        assert_eq!(
            spec.version_iri.as_deref(),
            Some("http://www.ebi.ac.uk/efo/releases/v3.90.0/efo.owl")
        );
        assert_eq!(spec.ontology_iri.as_deref(), Some("http://www.ebi.ac.uk/efo/efo.owl"));
        assert!(
            spec.annotations.contains(&("owl:versionInfo".into(), "3.90.0".into())),
            "versionInfo annotation missing: {:?}",
            spec.annotations
        );
        assert!(
            spec.annotations.contains(&("rdfs:comment".into(), "2026-06-25".into())),
            "build-date annotation missing: {:?}",
            spec.annotations
        );
    }

    /// The long-form spelling of the same flags parses to the same spec.
    #[test]
    fn long_flag_annotate_still_parsed() {
        let recipe = "robot annotate --annotation owl:versionInfo 2024-01-01 \
            --version-iri http://x/releases/2024-01-01/x.owl --ontology-iri http://x/x.owl";
        let steps = parse_command(recipe, "robot");
        let spec = steps
            .iter()
            .find_map(|s| match s {
                Step::Op(Op::Annotate(a)) => Some(a),
                _ => None,
            })
            .expect("annotate step should be parsed");
        assert_eq!(spec.version_iri.as_deref(), Some("http://x/releases/2024-01-01/x.owl"));
        assert_eq!(spec.ontology_iri.as_deref(), Some("http://x/x.owl"));
        assert_eq!(spec.annotations, vec![("owl:versionInfo".into(), "2024-01-01".into())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::step::Op;

    /// A recipe's `--prefix` binding is what a `--select` CURIE resolves through,
    /// so ingest has to record it. UBERON's `cumbo` term list is
    /// `filter --prefix 'uberon: …/obo/uberon/core#' --select
    /// 'oboInOwl:inSubset=uberon:cumbo'`: with the binding dropped the selector
    /// matched nothing, and since an empty seed means the whole ontology the
    /// export listed all 16,417 terms rather than 14.
    #[test]
    fn filter_records_the_prefix_its_selector_resolves_through() {
        let steps = parse_command(
            "robot filter -i x.owl --prefix 'uberon: http://purl.obolibrary.org/obo/uberon/core#' \
             --select 'oboInOwl:inSubset=uberon:cumbo' -o y.owl",
            "robot",
        );
        let spec = steps
            .iter()
            .find_map(|s| match s {
                Step::Op(Op::Filter(f)) => Some(f),
                Step::Partial { op: Op::Filter(f), .. } => Some(f),
                _ => None,
            })
            .expect("a filter step");
        assert_eq!(spec.selects, vec!["oboInOwl:inSubset=uberon:cumbo"]);
        assert_eq!(
            spec.prefixes,
            vec!["uberon: http://purl.obolibrary.org/obo/uberon/core#"],
            "the --prefix binding must reach the plan"
        );
    }
}
