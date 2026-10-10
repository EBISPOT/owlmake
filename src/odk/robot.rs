//! Parse a (make-expanded) recipe line — a `robot` command chain or a shell
//! command — into plan [`Step`]s.
//!
//! Ingest only: this turns the command lines an ODK Makefile carries into the
//! plan's own vocabulary ([`crate::plan::step`]). Nothing here runs at build
//! time — by then the steps are in the plan and the Makefile is irrelevant.

pub use crate::plan::step::*;
use crate::build::recipe::FileOp;
use crate::cmd::Switch;
use std::path::Path;

const SUBCOMMANDS: &[&str] = &[
    "merge", "reason", "relax", "reduce", "materialize", "remove", "filter", "annotate", "convert",
    "query", "verify", "report", "template", "export", "measure", "extract", "mirror", "repair",
    "rename", "expand", "collapse", "unmerge", "diff", "explain", "validate-profile", "reduce",
    "mireot", "rdfxml-to-json", "python",
    // owlmake's own names for the commands a recipe may also spell `odk:<name>`.
    "normalize", "subset", "check-align",
];

/// Commands that read their own inputs and write an output that is not the
/// ontology flowing through the chain — a report, a table, a prefix map, a mirror
/// directory — so running the command line IS the operation. A command that
/// threads a model on (`mint`) has to be a pipeline op instead, or the chain it
/// sits in loses the model.
const TERMINAL_COMMANDS: &[&str] = &[
    "report", "verify", "validate-profile", "measure", "diff", "export", "export-prefixes",
    "explain", "mirror", "check-align",
];

const BENIGN_SHELL: &[&str] = &[
    "echo", "mv", "cp", "true", ":", "test", "[", "mkdir", "rm", "touch", "cat", "cd", "pwd",
    "sort", "uniq", "printf", "date", "ls", "tee", "head", "tail", "cut",
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
    // owlmake under its own name, which is how the standard build's recipes
    // spell a command line.
    "om",
    // A recipe's own `make`, which builds from this repository's plan.
    "make",
    "robot", "jq", "arq", "sssom", "sssom-cli", "kgx", "dosdp-tools", "dosdp",
    "owltools", "sed", "grep", "comm", "gzip", "gunzip", "zcat",
    // Helper command words a recipe can spell inline. Nothing else on the machine
    // provides them, so owlmake serves each one from its own implementation.
    "dicer-cli", "check-rdfxml", "odk-info", "sha256sum", "fastobo-validator",
    "simple_pattern_tester.py", "runoak",
    // The OBO stanza filter, under the name of the script repositories call.
    "obo-grep.pl", "obo-grep",
    // The ontology SQL database (`semsql make <name>.db`).
    "semsql",
    // Helpers of ODK's own that the standard build's recipes name.
    "tsvalid", "context2csv", "make-release-assets.py",
];

/// Command words in `line` that owlmake cannot vouch for, deduplicated in first
/// appearance order. Each pipeline/sequence segment contributes its own command
/// word, so `git show x | robot convert` reports `git` and not `robot`.
fn unvouched_tools(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in split_commands(line) {
        let toks = tokenize(&seg);
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
        // A program named by path is a file of the repository (`../scripts/x.pl`),
        // and the plan names it as the path it is, so the build can see whether it
        // is there. A bare word is a program the machine provides on PATH.
        let name = if word.contains('/') { word } else { base };
        if !out.iter().any(|o| o == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// The simple commands of a line, split at every unquoted `|`, `;`, `&` or
/// newline. Quoted text is one word whatever it contains: a regex argument
/// `"(is_a|intersection_of):"` is an argument, not two commands, and splitting
/// inside it would read `intersection_of` as a program and swallow the rest of
/// the line into the unbalanced quote that follows.
fn split_commands(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if q == '"' && c == '\\' {
                    // An escaped `"` does not close the string.
                    if let Some(&n) = chars.peek() {
                        cur.push(c);
                        cur.push(n);
                        chars.next();
                        continue;
                    }
                }
                cur.push(c);
            }
            None => match c {
                '"' | '\'' | '`' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\\' => {
                    cur.push(c);
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                '|' | ';' | '&' | '\n' => out.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
    }
    out.push(cur);
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
    STATEFUL_BUILTINS.contains(&tok)
        || matches!(tok, "exit" | "return" | "break" | "continue" | "time" | "until" | "select")
}

/// The shell builtins whose effect outlives the command: the working directory,
/// variables, options, traps, functions, jobs and the shell's own limits. A part
/// of a line that runs one changes what the parts after it see.
const STATEFUL_BUILTINS: &[&str] = &[
    "cd", "pushd", "popd", "export", "unset", "readonly", "declare", "typeset", "local", "let",
    "set", "shopt", "umask", "ulimit", "alias", "unalias", ".", "source", "eval", "exec", "trap",
    "shift", "read", "mapfile", "readarray", "getopts", "hash", "enable", "wait", "bg", "fg",
    "jobs", "disown", "function",
];

/// Whether a recipe command is a Python interpreter invocation (`python`,
/// `python3`, or a path ending in one).
fn is_python(tok: &str) -> bool {
    matches!(tok, "python" | "python3")
        || tok.ends_with("/python")
        || tok.ends_with("/python3")
}

/// Whether owlmake performs this step itself, with no command line involved.
///
/// The test for whether a parse is worth keeping over the text it came from: a
/// step that ends up shelling out anyway has been decomposed for nothing, and
/// decomposing costs the shell's own short-circuiting and redirection.
fn runs_without_a_shell(s: &Step) -> bool {
    match s {
        Step::Op(_) | Step::Partial { .. } | Step::Boundary { .. } => true,
        Step::File(_) | Step::Jq(_) | Step::Sssom(_) | Step::OwlmakeCli { .. } => true,
        // ROBOT's refusal fails the step as the command line would, and an
        // option owlmake does not read refuses it in the plan; neither needs a
        // shell.
        Step::Refused { .. } | Step::UnsupportedOptions { .. } => true,
        Step::MayFail(inner) => runs_without_a_shell(inner),
        // `Shell`/`Fallback` are command lines by definition; an unsupported
        // subcommand is one owlmake has no implementation for, and `Branch`/`Oort`
        // are replayed rather than threaded. `Inert` alone is not work to keep.
        Step::Shell { .. }
        | Step::Fallback { .. }
        | Step::UnsupportedSubcommand(_)
        | Step::Branch { .. }
        | Step::Oort(_)
        | Step::Inert(_) => false,
    }
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
    // A line whose parts share the shell they run in stays one command, run by
    // one shell (see [`shares_shell_state`]).
    if shares_shell_state(whole) {
        return vec![shell_step(whole.to_string())];
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

    let mut steps = Vec::new();
    // Whether an earlier part of THIS command was itself a robot invocation. A
    // later one that names its own `--input` opens a new pipeline over that
    // file: a separate process shares nothing with the last but files, exactly
    // as a later recipe LINE does. MONDO's chebi mirror is `convert -I …
    // -o tmp.owl && remove -i tmp.owl …` on one line: the remove re-reads the
    // file the convert wrote, and the re-read is what hands the document's
    // declared xmlns block to the final writer.
    let mut saw_robot_part = false;
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
        // `cmd || true` says cmd MAY FAIL. Dropping the `true` would not express
        // that — it would leave `cmd` an ordinary step whose failure aborts the
        // recipe — so the tolerance is recorded as `may_fail` on the step itself.
        //
        // Tolerating a failure says nothing about what the command IS, so the
        // command is still parsed. A plan is the whole build once the recipe it
        // came from is gone, and an ontology command belongs in it as the op
        // owlmake runs — EFO's mondo import excludes HGNC terms with a tolerated
        // `query`, which the plan names as `op: query` like any other.
        //
        // Only when the parse is fully native, though. MONDO's OMIM-gene check is
        // `grep -Ff $< mondo-edit.obo | grep '^xref' > $@ || true` — a pipeline of
        // text tools, where grep exits 1 on no match and that is the PASSING case.
        // Nothing is gained by taking it apart, so it stays one command and the
        // shell that runs it applies its own semantics.
        let tolerated = matches!(seq[idx].1, Some(ShellSep::Or))
            && matches!(seq.get(idx + 1), Some((next, _)) if next.trim() == "true" || next.trim() == ":");
        if tolerated {
            let parsed = parse_command(sub, robot_prefix);
            if !parsed.is_empty() && parsed.iter().all(runs_without_a_shell) {
                steps.extend(parsed.into_iter().map(|s| Step::MayFail(Box::new(s))));
            } else {
                steps.push(shell_step(format!("{sub} || true")));
            }
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
        // A command whose text holds a shell expansion is not static: its value
        // is whatever the shell computes at run time. Neither an op nor a native
        // `FileOp`, which keep the text as written, can carry that, so run it.
        //
        // EFO's mondo import counts its auto-excluded HGNC terms with
        // `echo "Auto-excluding $(wc -l < …hgnc.txt) HGNC terms…"`. Parsed as a
        // `Print`, it would announce the substitution instead of the count.
        if crate::build::recipe::has_shell_expansion(sub) {
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
                let words = program_words(sub);
                let skip = words.iter().take_while(|t| is_env_assignment(t)).count();
                let mut chain = parse_robot_chain(&words[skip..], robot_prefix);
                if saw_robot_part && !chain.is_empty() {
                    if let Some(input) = super::planner::first_robot_input(sub, robot_prefix) {
                        chain.insert(0, Step::Boundary { input: Some(input) });
                    }
                }
                saw_robot_part = true;
                steps.extend(chain);
            }
        } else if toks[0] == "owltools" || toks[0].ends_with("/owltools") {
            steps.extend(parse_owltools(&toks, sub));
        } else if toks[0] == "babelon" || toks[0].ends_with("/babelon") {
            steps.push(parse_babelon(&toks, sub));
        } else if toks[0] == "ontology-release-runner" || toks[0].ends_with("/ontology-release-runner") {
            steps.push(parse_oort(&toks));
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
            steps.push(Step::OwlmakeCli { name: "make".to_string(), args });
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
    let mut i = 1;
    while i < toks.len() {
        let t = &toks[i];
        if t == "-o" {
            let mut j = i + 1;
            if toks.get(j).is_some_and(|f| f == "-f") {
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
                steps.push(Step::Op(Op::plain_merge(vec![])));
            }
            "--merge-axiom-annotations" => {
                steps.push(Step::Op(Op::Repair {
                    invalid_references: false,
                    merge_axiom_annotations: true,
                    annotation_properties: vec![],
                    annotation_properties_file: None,
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
            // A pure load-and-save: no operations, but the line still WRITES the
            // file its `-o` names, in the format its `-f` gives — through the
            // owltools emulation, whose writers differ from `convert`'s (the
            // inline-anonymous-node RDF/XML profile, and the property_value
            // quoting of its OBO output). Replayed verbatim so the plan says
            // exactly what runs.
            Some(_) => return vec![shell_step(sub.to_string())],
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

/// The prefix options a command line states, as [`Op::Prefixes`] records them.
#[derive(Clone, Default, PartialEq)]
struct PrefixOptions {
    prefixes: Option<String>,
    noprefixes: bool,
    add_prefixes: Vec<String>,
    prefix: Vec<String>,
    add_prefix: Vec<String>,
}

impl PrefixOptions {
    /// Record `option`, with the `value` after it, when it is one of the prefix
    /// options of `command` (`""` before any command); whether it was. A ROBOT
    /// command's options come by their long names ([`super::robot_cli`]).
    /// Before any command `-p` and `-P` are `--prefix` and `--prefixes`, and on
    /// a plugin's command `-P` is.
    fn take(&mut self, command: &str, option: &str, value: Option<&String>) -> bool {
        fn push(list: &mut Vec<String>, value: &str) {
            if !list.iter().any(|v| v == value) {
                list.push(value.to_string());
            }
        }
        match (option, value) {
            ("--noprefixes", _) => self.noprefixes = true,
            ("--prefixes" | "-P", Some(v)) => {
                self.prefixes.get_or_insert_with(|| v.clone());
            }
            ("--add-prefixes", Some(v)) => push(&mut self.add_prefixes, v),
            ("--prefix", Some(v)) => push(&mut self.prefix, v),
            ("-p", Some(v)) if command.is_empty() => push(&mut self.prefix, v),
            ("--add-prefix", Some(v)) => push(&mut self.add_prefix, v),
            _ => return false,
        }
        true
    }

    /// These options with `own` after them, as a command given both reads them:
    /// the first `--prefixes` file, and every binding of each.
    fn then(&self, own: &PrefixOptions) -> PrefixOptions {
        let mut out = self.clone();
        if out.prefixes.is_none() {
            out.prefixes = own.prefixes.clone();
        }
        out.noprefixes |= own.noprefixes;
        for (list, more) in [
            (&mut out.add_prefixes, &own.add_prefixes),
            (&mut out.prefix, &own.prefix),
            (&mut out.add_prefix, &own.add_prefix),
        ] {
            for v in more {
                if !list.contains(v) {
                    list.push(v.clone());
                }
            }
        }
        out
    }

    fn step(&self) -> Step {
        Step::Op(Op::Prefixes {
            prefixes: self.prefixes.clone(),
            noprefixes: self.noprefixes,
            add_prefixes: self.add_prefixes.clone(),
            prefix: self.prefix.clone(),
            add_prefix: self.add_prefix.clone(),
        })
    }
}

fn parse_robot_chain(toks: &[String], robot_prefix: &str) -> Vec<Step> {
    // Skip the launcher prefix: drop tokens until the first subcommand…
    let mut i = launcher_len(toks, robot_prefix);
    let mut steps = Vec::new();
    // …but not its PREFIX OPTIONS. What is stated before any subcommand is given
    // to every command of the chain, and each command reads its CURIEs with those
    // and its own ([`Op::Prefixes`]): CL's `components/hra_subset.owl` is
    // `robot --add-prefix "obo: …" annotate …` and declares `xmlns:obo`, and a
    // repository that uses its context runs every command as
    // `robot --add-prefixes config/context.json …`.
    let mut chain = PrefixOptions::default();
    {
        let mut launcher: Vec<String> = tokenize(robot_prefix);
        launcher.extend(toks[..i].iter().cloned());
        let mut k = 0;
        while k < launcher.len() {
            let option = launcher[k].clone();
            k += 1;
            if chain.take("", &option, launcher.get(k)) && option != "--noprefixes" {
                k += 1;
            }
        }
    }
    // The options the model's context is made of as the next command starts.
    let mut in_force = PrefixOptions::default();
    // Whether an input has been named yet: the chain's first is the input of
    // the rule ([`super::planner::first_robot_input`]).
    let mut input_named = false;
    // A command named by the rest of a token the previous command read part of.
    let mut pending: Option<String> = None;
    // Whether the command is the chain's first, which has an ontology only
    // when it names an input.
    let mut first_command = true;
    while i < toks.len() || pending.is_some() {
        let name = match pending.take() {
            Some(name) => name,
            None => {
                i += 1;
                toks[i - 1].clone()
            }
        };
        // Gather this subcommand's option tokens up to the next subcommand. A
        // ROBOT command reads its own as ROBOT reads them, and runs only when
        // what follows names the next command.
        let mut opts: Vec<(String, Vec<String>)> = Vec::new();
        if let Some(options) = super::robot_cli::command(&name) {
            let parsed = match super::robot_cli::parse(&options, &toks[i..]) {
                Ok(parsed) => parsed,
                Err(message) => {
                    steps.push(Step::Refused { message });
                    return steps;
                }
            };
            i += parsed.consumed;
            opts = parsed.options;
            let next = parsed.next.clone().or_else(|| toks.get(i).cloned());
            if let Some(next) = next.filter(|n| !is_subcommand_token(n)) {
                steps.push(Step::Refused { message: format!("UNKNOWN ARG ERROR unknown command or option: {next}") });
                return steps;
            }
            pending = parsed.next;
        }
        while super::robot_cli::command(&name).is_none() && i < toks.len() && !is_subcommand_token(&toks[i]) {
            let tok = toks[i].clone();
            i += 1;
            // A token that is no option and no option's value is one more thing
            // the command is given, which its step has to read.
            let arity = if tok.starts_with('-') { option_arity(&tok, toks.get(i)) } else { 0 };
            let vals: Vec<String> = toks[i..(i + arity).min(toks.len())].to_vec();
            i += vals.len();
            opts.push((tok, vals));
        }
        let (mut step, read) = map_subcommand(&name, &opts);
        if let Step::Refused { .. } = step {
            steps.push(step);
            return steps;
        }
        // The options the command is given that the plan cannot carry out.
        let mut unread: Vec<String> = Vec::new();
        if let Step::Op(Op::Template { merge, collapse_import_closure, .. }) = &mut step {
            let given = |key: &str| opts.iter().any(|(k, _)| k == key);
            // A merge goes into the chain's ontology, which the chain's first
            // command has only when it names an input.
            if *merge && first_command && !given("--input") && !given("--input-iri") {
                let switch = if given("--merge-after") { "--merge-after" } else { "--merge-before" };
                steps.push(Step::Refused { message: format!("template: {switch} has no input ontology to merge into") });
                return steps;
            }
            // `--merge-after` writes the generated axioms alone where `-o` says,
            // and goes on with the merge. Ending the chain, what it writes is
            // what the chain yields, and the merge goes nowhere. Part way
            // through, it would write one ontology and go on with another, which
            // no step does.
            if given("--merge-after") {
                if let Some((_, out)) = opts.iter().find(|(k, _)| k == "--output") {
                    if pending.is_some() || i < toks.len() {
                        unread.push("--merge-after".to_string());
                        unread.push(std::iter::once("--output".to_string()).chain(out.iter().cloned()).collect::<Vec<_>>().join(" "));
                    } else {
                        *merge = false;
                        *collapse_import_closure = false;
                    }
                }
            }
        }
        first_command = false;
        // A command reads its CURIEs with the chain's prefix options and its own,
        // and the command after it with the chain's alone.
        let mut own = PrefixOptions::default();
        for (option, values) in &opts {
            own.take(&name, option, values.first());
        }
        let wanted = chain.then(&own);
        if wanted != in_force {
            steps.push(wanted.step());
            in_force = wanted;
        }
        // `-O`/`--output-iri`, `--ontology-iri` and `-V`/`--version-iri` name the
        // IRIs of the ontology a command writes, on many commands besides
        // `annotate`: `extract … -O …`, `template … --ontology-iri …`. One the
        // step does not read itself is recorded as the `annotate` it is
        // equivalent to, after the step; `annotate` reads its own, and `repair`
        // reads `--output-iri` and keeps the ontology's IRI. On `verify`, `-O` is
        // `--output-dir` and names no IRI.
        let o_is_output_dir = name == "verify";
        let sets_iri = |option: &str| {
            !o_is_output_dir && matches!(option, "--output-iri" | "-O" | "--ontology-iri" | "--version-iri" | "-V")
        };
        // `convert` models its own `--output`; every other command's `-o` is a
        // process boundary (see below).
        let models_own_output = matches!(step, Step::Op(Op::Convert { .. }));
        // An option the command is given that neither its step nor the chain
        // reads is something the plan cannot do: it refuses the step by name.
        for ((option, values), read) in opts.iter().zip(&read) {
            let input = matches!(option.as_str(), "--input" | "-i" | "--input-iri" | "-I");
            let read_by_chain = PrefixOptions::default().take(&name, option, values.first())
                || matches!(
                    option.as_str(),
                    "--output" | "-o" | "--verbose" | "-v" | "--very-verbose" | "-vv" | "--very-very-verbose" | "-vvv"
                )
                || (input && !input_named)
                || sets_iri(option);
            input_named |= input;
            if !read && !read_by_chain {
                unread.push(std::iter::once(option.clone()).chain(values.iter().cloned()).collect::<Vec<_>>().join(" "));
            }
        }
        if !unread.is_empty() {
            steps.push(Step::UnsupportedOptions { command: name.clone(), options: unread });
        }
        steps.push(step);
        let find = |a: &str, b: &str| -> Option<String> {
            opts.iter().find(|(k, _)| k == a || k == b).and_then(|(_, v)| v.first().cloned())
        };
        let unread_value = |keys: &[&str]| -> Option<String> {
            opts.iter()
                .zip(&read)
                .find(|((k, _), read)| !**read && sets_iri(k) && keys.contains(&k.as_str()))
                .and_then(|((_, v), _)| v.first().cloned())
        };
        let (ontology_iri, version_iri) = (
            unread_value(&["--output-iri", "-O"]).or_else(|| unread_value(&["--ontology-iri"])),
            unread_value(&["--version-iri", "-V"]),
        );
        if ontology_iri.is_some() || version_iri.is_some() {
            steps.push(Step::Op(Op::Annotate(AnnotateSpec { ontology_iri, version_iri, ..Default::default() })));
        }
        // `-o` writes the file where the command stands, and the chain goes on
        // with the model. (The rule's final `-o $@` is dropped by the caller —
        // the pipeline's closing write already is that write.)
        if !models_own_output {
            if let Some(out) = find("--output", "-o") {
                steps.push(Step::Op(Op::Write { path: out }));
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

/// How many values option `opt` of a command ROBOT does not have takes — a
/// plugin's, whose options only the plugin knows: one, the token after it,
/// unless that is another option or names a command. `-O` always takes one.
fn option_arity(opt: &str, next: Option<&String>) -> usize {
    match opt {
        "-O" => 1,
        "--remove-annotations" | "--noprefixes" => 0,
        // Do NOT treat a plugin-shaped CURIE here — `--term rdfs:label`,
        // `--term SO:0000704`, `--prefix "oio: …"` — as a boundary; those are
        // values. (Plugin commands only start a segment, which the delimiter in
        // `parse_robot_chain` still recognises via `is_subcommand_token`.)
        _ => match next {
            Some(n) if !n.starts_with('-') && !SUBCOMMANDS.contains(&n.as_str()) => 1,
            _ => 0,
        },
    }
}

/// The step a command's options map to, and which of `opts` the mapping read.
fn map_subcommand(name: &str, opts: &[(String, Vec<String>)]) -> (Step, Vec<bool>) {
    let read = std::cell::RefCell::new(vec![false; opts.len()]);
    // The values of every occurrence of the options `keys` names, in recipe
    // order, each marked as read.
    let take = |keys: &[&str]| -> Vec<&Vec<String>> {
        let mut out = Vec::new();
        for (i, (k, v)) in opts.iter().enumerate() {
            if keys.contains(&k.as_str()) {
                read.borrow_mut()[i] = true;
                out.push(v);
            }
        }
        out
    };
    let has = |key: &str| !take(&[key]).is_empty();
    let val = |key: &str| -> Option<String> { take(&[key]).first().and_then(|v| v.first().cloned()) };
    // An option spelt long or short: its first value in recipe order, as the
    // command reads one value, and every value in recipe order.
    let val2 = |a: &str, b: &str| -> Option<String> { take(&[a, b]).first().and_then(|v| v.first().cloned()) };
    let all = |key: &str| -> Vec<String> { take(&[key]).into_iter().filter_map(|v| v.first().cloned()).collect() };
    let boolv = |key: &str| -> Option<bool> { val(key).map(|s| s == "true") };
    // The message of the first switch the command reads that is neither `true`
    // nor `false`: the command fails on it.
    let refusal: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    // Switch `long`, read where the command reads it: its value, `None` when
    // it is not given or is refused. The step reads it when `used`, and one it
    // does not use is read all the same when it asks for `default`, which is
    // what the step does without it.
    let switch = |long: &str, used: bool, default: bool| -> Option<bool> {
        let value = opts.iter().find(|(k, _)| k == long).and_then(|(_, v)| v.first().cloned())?;
        match crate::cmd::read_bool(long.trim_start_matches('-'), &value) {
            Ok(on) => {
                if used || on == default {
                    take(&[long]);
                }
                Some(on)
            }
            Err(message) => {
                refusal.borrow_mut().get_or_insert(message);
                None
            }
        }
    };
    // A switch the command reads as on for `true` or `yes` in any case and
    // off for anything else, as [`switch`] reads it otherwise.
    let lenient = |long: &str, used: bool, default: bool| -> Option<bool> {
        let value = opts.iter().find(|(k, _)| k == long).and_then(|(_, v)| v.first().cloned())?;
        let on = crate::cmd::option_is_true(&value);
        if used || on == default {
            take(&[long]);
        }
        Some(on)
    };
    // Every option token of this invocation, flattened back to argv order, for the
    // `OwlmakeCli` steps that are executed by re-invoking the owlmake binary.
    let argv = || -> Vec<String> {
        read.borrow_mut().fill(true);
        opts.iter()
            .flat_map(|(k, v)| std::iter::once(k.clone()).chain(v.iter().cloned()))
            .collect()
    };
    let all2 = |a: &str, b: &str| -> Vec<String> {
        take(&[a, b]).into_iter().filter_map(|v| v.first().cloned()).collect()
    };
    // The options `remove` and `filter` share.
    let selection = || SelectionSpec {
        terms: all2("--term", "-t"),
        term_files: all2("--term-file", "-T"),
        include_terms: all2("--include-term", "-n"),
        include_term_files: all2("--include-terms", "-N"),
        exclude_terms: all2("--exclude-term", "-e"),
        exclude_term_files: all2("--exclude-terms", "-E"),
        selects: all2("--select", "-s"),
        axioms: all2("--axioms", "-a"),
        base_iri: all("--base-iri"),
        // Each switch as the recipe gives it: the step reads it where the
        // command does, which for all but `--allow-punning` is only once
        // something is selected.
        trim: val2("--trim", "-r").map(|s| Switch::parse(&s)),
        signature: val2("--signature", "-S").map(|s| Switch::parse(&s)),
        preserve_structure: val2("--preserve-structure", "-p").map(|s| Switch::parse(&s)),
        allow_punning: val("--allow-punning").map(|s| Switch::parse(&s)),
        drop_axiom_annotations: all2("--drop-axiom-annotations", "-d"),
    };

    // `odk:` is a plugin namespace a recipe can spell; owlmake serves those
    // commands itself, so strip the prefix and treat e.g. `odk:normalize` as
    // `normalize`. (Other plugin prefixes — `flybase:`, … — are handled below.)
    let name = name.strip_prefix("odk:").unwrap_or(name);

    // A `prefix:command` plugin invocation → route the bare command to the
    // matching generic built-in (un-prefixed). Unknown plugin commands remain
    // uncovered.
    if let Some((_, bare)) = name.split_once(':') {
        let step = match bare {
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
                Step::OwlmakeCli { name: name.to_string(), args: argv() }
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
            name if TERMINAL_COMMANDS.contains(&name) => {
                Step::OwlmakeCli { name: name.to_string(), args: argv() }
            }
            _ => Step::UnsupportedSubcommand(name.to_string()),
        };
        return (step, read.into_inner());
    }

    let step = match name {
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
            let (before, after) = (has("--merge-before"), has("--merge-after"));
            let merge = before || after;
            let force = lenient("--force", true, false).unwrap_or(false);
            // A merge's input keeps its own IRIs: the ones the command names are
            // read and set nothing. `--include-annotations` adds the ontology
            // annotations of the generated axioms' ontology, which has none.
            if merge {
                take(&["--ontology-iri", "--version-iri"]);
            }
            switch("--include-annotations", true, false);
            let collapse_import_closure = switch("--collapse-import-closure", true, false).unwrap_or(false) && merge;
            let ancestors = has("--ancestors");
            if before && after {
                Step::Refused { message: "MERGE ERROR merge-before and merge-after cannot be combined".into() }
            } else {
                Step::Op(Op::Template { templates, merge, collapse_import_closure, ancestors, force })
            }
        }
        "rename" => Step::Op(Op::Rename {
            mappings: val2("--mappings", "-m"),
            mapping: take(&["--mapping"])
                .into_iter()
                .filter_map(|v| match v.as_slice() {
                    [old, new] => Some((old.clone(), new.clone())),
                    _ => None,
                })
                .collect(),
            prefix_mappings: val2("--prefix-mappings", "-r"),
            allow_missing: switch("--allow-missing-entities", true, false).unwrap_or(false),
            allow_duplicates: switch("--allow-duplicates", true, false).unwrap_or(false),
        }),
        "extract" => {
            let method = val2("--method", "-m").unwrap_or_else(|| "BOT".into());
            // MIREOT names no module IRI and neither trims nor annotates its
            // module as a locality module's options do: given, they are read
            // and set nothing.
            if method.eq_ignore_ascii_case("MIREOT") {
                take(&["--output-iri", "--annotate-with-source", "--sources", "--imports"]);
            }
            Step::Op(Op::Extract {
                method,
                terms: all2("--term", "-t"),
                term_files: all2("--term-file", "-T"),
                copy_ontology_annotations: switch("--copy-ontology-annotations", true, false).unwrap_or(false),
                individuals: val("--individuals"),
                branch_from_terms: all("--branch-from-term"),
                branch_from_term_files: all("--branch-from-terms"),
                lower_terms: all("--lower-term"),
                lower_term_files: all("--lower-terms"),
                upper_terms: all("--upper-term"),
                upper_term_files: all("--upper-terms"),
                intermediates: val("--intermediates"),
                // Read as ROBOT reads it: `true` or `yes`, in any case.
                force: val2("--force", "-f").is_some_and(|v| crate::cmd::option_is_true(&v)),
            })
        }
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
            threshold: val2("--threshold", "-t"),
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
        //
        // `--inputs` names the files a pattern matches when the command runs, and
        // a plan names every file it reads, so it stays an option no step reads.
        "merge" => Step::Op(Op::Merge {
            inputs: all2("--input", "-i")
                .into_iter()
                .chain(all2("--input-iri", "-I"))
                .collect(),
            collapse_import_closure: switch("--collapse-import-closure", true, true),
            include_annotations: switch("--include-annotations", true, false) == Some(true),
            annotate_derived_from: switch("--annotate-derived-from", true, false) == Some(true),
            annotate_defined_by: switch("--annotate-defined-by", true, false) == Some(true),
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
            annotate_inferred_axioms: lenient("--annotate-inferred-axioms", true, false),
            allow_incoherent: boolv("--allow-incoherent"),
            exclude_external_entities: lenient("--exclude-external-entities", true, false),
            exclude_owl_thing: lenient("--exclude-owl-thing", true, false),
            remove_redundant_subclass_axioms: lenient("--remove-redundant-subclass-axioms", true, false),
            create_new_ontology: lenient("--create-new-ontology", true, false),
            create_new_ontology_with_annotations: lenient("--create-new-ontology-with-annotations", true, false),
            exclude_duplicate_axioms: {
                lenient("--include-indirect", false, false);
                lenient("--preserve-annotated-axioms", false, false);
                lenient("--exclude-duplicate-axioms", true, false)
            },
            axiom_generators: { let mut g = all("--axiom-generators"); g.extend(all("-A")); g },
            properties: all("--properties"),
        }),
        "relax" => Step::Op(Op::Relax {
            include_subclass_of: {
                switch("--enforce-obo-format", false, false);
                switch("--exclude-named-classes", false, true);
                switch("--include-subclass-of", true, false).unwrap_or(false)
            },
        }),
        "reduce" => {
            let on = |long: &str, short: &str| val2(long, short).is_some_and(|v| crate::cmd::option_is_true(&v));
            Step::Op(Op::Reduce {
                reasoner: val2("--reasoner", "-r"),
                include_subproperties: val2("--include-subproperties", "-s").map(|v| crate::cmd::option_is_true(&v)),
                preserve_annotated_axioms: on("--preserve-annotated-axioms", "-p"),
                named_classes_only: on("--named-classes-only", "-c"),
            })
        }
        "materialize" => Step::Op(Op::Materialize {
            reasoner: {
                // ROBOT's materialize takes these and reads neither.
                take(&["--annotate-inferred-axioms", "-a", "--remove-redundant-subclass-axioms", "-s"]);
                val2("--reasoner", "-r")
            },
            create_new_ontology: lenient("--create-new-ontology", true, false),
            properties: { let mut p = all("--term"); p.extend(all("-t")); p },
            term_files: { let mut f = all("--term-file"); f.extend(all("-T")); f },
        }),
        "remove" => remove_step(selection()),
        // `-O`/`--ontology-iri` is recorded as the `annotate` that follows (below).
        "filter" => filter_step(selection()),
        "annotate" => {
            // The values of every occurrence of an option, spelt long or short, in
            // recipe order.
            let values = |long: &str, short: &str| -> Vec<&Vec<String>> { take(&[long, short]) };
            let pairs = |long: &str, short: &str| -> Vec<(String, String)> {
                values(long, short)
                    .into_iter()
                    .filter_map(|v| match v.as_slice() {
                        [p, x] => Some((p.clone(), x.clone())),
                        _ => None,
                    })
                    .collect()
            };
            let triples = |long: &str, short: &str| -> Vec<(String, String, String)> {
                values(long, short)
                    .into_iter()
                    .filter_map(|v| match v.as_slice() {
                        [p, x, y] => Some((p.clone(), x.clone(), y.clone())),
                        _ => None,
                    })
                    .collect()
            };
            let remove_annotations = !take(&["--remove-annotations", "-R"]).is_empty();
            let interpolate = switch("--interpolate", true, false).unwrap_or(false);
            let annotate_derived_from = switch("--annotate-derived-from", true, false).unwrap_or(false);
            let annotate_defined_by = switch("--annotate-defined-by", true, false).unwrap_or(false);
            Step::Op(Op::Annotate(AnnotateSpec {
                ontology_iri: val2("--ontology-iri", "-O"),
                version_iri: val2("--version-iri", "-V"),
                annotations: pairs("--annotation", "-a"),
                link_annotations: pairs("--link-annotation", "-k"),
                language_annotations: triples("--language-annotation", "-l"),
                typed_annotations: triples("--typed-annotation", "-t"),
                axiom_annotations: values("--axiom-annotation", "-x").into_iter().flatten().cloned().collect(),
                annotation_files: all2("--annotation-file", "-A"),
                remove_annotations,
                interpolate,
                annotate_defined_by,
                annotate_derived_from,
            }))
        }
        "convert" => Step::Op(Op::Convert {
            format: val2("--format", "-f"),
            clean_obo: val("--clean-obo"),
            output: val2("--output", "-o"),
            check: switch("--check", true, true),
        }),
        "query" => {
            let pairs = |keys: &[&str]| -> Vec<(String, String)> {
                take(keys)
                    .into_iter()
                    .filter_map(|v| if v.len() >= 2 { Some((v[0].clone(), v[1].clone())) } else { None })
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
                let listed: Vec<String> = take(&["--queries", "-Q"]).into_iter().flatten().cloned().collect();
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
            // The switches as the command reads them: with an update,
            // `--temporary-file` alone; otherwise `--create-tdb`, then unless
            // that is on `--tdb`, then `--keep-tdb-mappings` on disk (`--tdb` or
            // a `--tdb-directory`) or `--use-graphs` in memory. A switch it does
            // not read is left alone, whatever it says. `tdb` records any
            // on-disk dataset, which decides an unordered `SELECT`'s row order
            // (see `Op::Query::tdb`).
            let given = |key: &str| opts.iter().any(|(k, _)| k == key);
            let ignored = |keys: &[&str]| {
                take(keys);
            };
            let (use_graphs, tdb) = if given("--update") {
                ignored(&["--create-tdb", "--tdb", "--keep-tdb-mappings", "--use-graphs"]);
                (false, switch("--temporary-file", true, false) == Some(true))
            } else if switch("--create-tdb", true, false) == Some(true) {
                ignored(&["--tdb", "--keep-tdb-mappings", "--use-graphs", "--temporary-file"]);
                (false, true)
            } else if switch("--tdb", true, false) == Some(true) || given("--tdb-directory") {
                switch("--keep-tdb-mappings", false, false);
                ignored(&["--use-graphs", "--temporary-file"]);
                (false, true)
            } else {
                ignored(&["--keep-tdb-mappings", "--temporary-file"]);
                (switch("--use-graphs", true, false) == Some(true), false)
            };
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
                use_graphs,
                tdb,
            })
        }
        "repair" => {
            // Read and not used: the ontology keeps its IRI.
            take(&["--output-iri"]);
            let repairs = crate::cmd::repair::RepairOptions::from_switches(
                switch("--invalid-references", true, false),
                switch("--merge-axiom-annotations", true, false),
            );
            Step::Op(Op::Repair {
                invalid_references: repairs.invalid_references,
                merge_axiom_annotations: repairs.merge_axiom_annotations,
                annotation_properties: all2("--annotation-property", "-a"),
                annotation_properties_file: val2("--annotation-properties-file", "-A"),
            })
        }
        // …and the same set on the non-chained path.
        // Terminal commands: each reads its own inputs and writes a non-ontology
        // output (a report, a table, a prefix map, a mirror directory), leaving
        // the ontology untouched — so dispatching a fresh `om` subcommand IS the
        // operation, and no model has to thread through.
        name if TERMINAL_COMMANDS.contains(&name) => {
            Step::OwlmakeCli { name: name.to_string(), args: argv() }
        }
        other => Step::UnsupportedSubcommand(other.to_string()),
    };
    if let Some(message) = refusal.into_inner() {
        return (Step::Refused { message }, read.into_inner());
    }
    (step, read.into_inner())
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

/// Whether a line has to run as ONE shell, as make runs every recipe line,
/// rather than part by part.
///
/// Its parts share the shell they run in. A part that `exit`s ends that shell, so
/// nothing after it runs: HPO's `grep '^ERROR' hp_report && exit -1 || echo "No
/// errors"` must die with 255 when the grep matches, where the `exit -1` taken as
/// an ordinary failing part would hand control to the `||`. A part that changes
/// the shell's state — `cd`, an assignment, `export`, `set`, `umask`, a function
/// definition — changes what every part after it sees, and a part that reads
/// `$?` or `$!` reads what the part before it left. A job started with `&` is the
/// shell's to wait for.
pub(crate) fn shares_shell_state(line: &str) -> bool {
    let line = line.trim().trim_start_matches(['@', '+', '-']);
    let parts = split_shell_seq(line);
    if parts.iter().any(|(p, _)| {
        let p = p.trim().trim_start_matches(['@', '+', '-']).trim();
        p == "exit" || p.starts_with("exit ")
    }) {
        return true;
    }
    let scan = ShellScan::of(line);
    scan.background
        || parts.len() > 1
            && (scan.reads_status
                || scan.defines_function
                || scan.assigns
                || scan.commands.iter().any(|c| STATEFUL_BUILTINS.contains(&c.as_str())))
}

/// What a scan of a shell line finds at its command positions.
#[derive(Default)]
struct ShellScan {
    /// The command word of each simple command, unquoted, after the
    /// assignments and redirections before it and the reserved words that lead
    /// into it.
    commands: Vec<String>,
    /// A simple command that is nothing but assignments, which set variables
    /// of the shell itself.
    assigns: bool,
    /// A function definition, `name () …`.
    defines_function: bool,
    /// A `$?` or `$!` outside single quotes.
    reads_status: bool,
    /// A command run in the background with `&`.
    background: bool,
}

impl ShellScan {
    fn of(line: &str) -> ShellScan {
        let mut scan = ShellScan::default();
        // The line as words and operators, quotes removed from the words.
        enum Tok {
            Word(String),
            Op(&'static str),
        }
        let mut toks: Vec<Tok> = Vec::new();
        let chars: Vec<char> = line.chars().collect();
        let mut word = String::new();
        let mut in_word = false;
        let mut i = 0;
        // The text of a `$(…)`, `${…}` or backquoted substitution, read whole.
        let substitution = |i: &mut usize, open: char, close: char, word: &mut String| {
            let mut depth = 0;
            while *i < chars.len() {
                let c = chars[*i];
                word.push(c);
                *i += 1;
                if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
        };
        while i < chars.len() {
            let c = chars[i];
            match c {
                '\\' => {
                    if let Some(&n) = chars.get(i + 1) {
                        word.push(n);
                    }
                    in_word = true;
                    i += 2;
                }
                '\'' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '\'' {
                        word.push(chars[i]);
                        i += 1;
                    }
                    in_word = true;
                    i += 1;
                }
                '"' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        if chars[i] == '\\' && i + 1 < chars.len() {
                            i += 1;
                        } else if chars[i] == '$' && matches!(chars.get(i + 1), Some('?' | '!')) {
                            scan.reads_status = true;
                        }
                        word.push(chars[i]);
                        i += 1;
                    }
                    in_word = true;
                    i += 1;
                }
                '`' => {
                    substitution(&mut i, '`', '`', &mut word);
                    in_word = true;
                }
                '$' if matches!(chars.get(i + 1), Some('(')) => {
                    word.push('$');
                    i += 1;
                    substitution(&mut i, '(', ')', &mut word);
                    in_word = true;
                }
                '$' if matches!(chars.get(i + 1), Some('{')) => {
                    word.push('$');
                    i += 1;
                    substitution(&mut i, '{', '}', &mut word);
                    in_word = true;
                }
                '$' if matches!(chars.get(i + 1), Some('?' | '!')) => {
                    scan.reads_status = true;
                    word.push(c);
                    in_word = true;
                    i += 1;
                }
                ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>' => {
                    // A redirection with a descriptor is one word: `2>&1`, `2>/dev/null`.
                    let redirect = matches!(c, '<' | '>')
                        || c == '&' && matches!(chars.get(i + 1), Some('>'))
                        || c == '&' && matches!(i.checked_sub(1).map(|p| chars[p]), Some('<' | '>'));
                    if redirect {
                        // `cmd>out`: the redirection starts a word of its own.
                        if in_word && !word.chars().all(|c| c.is_ascii_digit() || c == '<' || c == '>' || c == '&') {
                            toks.push(Tok::Word(std::mem::take(&mut word)));
                        }
                        word.push(c);
                        in_word = true;
                        i += 1;
                        continue;
                    }
                    if in_word {
                        toks.push(Tok::Word(std::mem::take(&mut word)));
                        in_word = false;
                    }
                    let next = chars.get(i + 1).copied();
                    let (op, len): (&'static str, usize) = match (c, next) {
                        (' ' | '\t', _) => ("", 1),
                        ('&', Some('&')) => ("&&", 2),
                        ('|', Some('|')) => ("||", 2),
                        (';', Some(';')) => (";;", 2),
                        ('&', _) => ("&", 1),
                        ('|', _) => ("|", 1),
                        (';', _) => (";", 1),
                        ('\n', _) => ("\n", 1),
                        ('(', _) => ("(", 1),
                        _ => (")", 1),
                    };
                    if !op.is_empty() {
                        toks.push(Tok::Op(op));
                    }
                    i += len;
                }
                _ => {
                    word.push(c);
                    in_word = true;
                    i += 1;
                }
            }
        }
        if in_word {
            toks.push(Tok::Word(word));
        }

        // Walk the commands: at a command position, step over assignments,
        // redirections and the reserved words that lead into a command.
        let is_redirection = |w: &str| {
            let w = w.trim_start_matches(|c: char| c.is_ascii_digit());
            w.starts_with('<') || w.starts_with('>') || w.starts_with("&>")
        };
        let mut at_command = true;
        let mut only_assignments = false;
        let mut skip_target = false;
        let mut last_command: Option<usize> = None;
        for (n, tok) in toks.iter().enumerate() {
            match tok {
                Tok::Op(op) => {
                    if only_assignments {
                        scan.assigns = true;
                    }
                    if *op == "&" {
                        scan.background = true;
                    }
                    // `name ( )`: a function definition.
                    if *op == "("
                        && last_command == Some(n.wrapping_sub(1))
                        && matches!(toks.get(n + 1), Some(Tok::Op(")")))
                    {
                        scan.defines_function = true;
                    }
                    at_command = true;
                    only_assignments = false;
                    skip_target = false;
                }
                Tok::Word(w) => {
                    if skip_target {
                        skip_target = false;
                        continue;
                    }
                    if is_redirection(w) {
                        // A bare operator takes the next word as its target.
                        skip_target = w.trim_start_matches(|c: char| c.is_ascii_digit())
                            .trim_start_matches(['<', '>', '&', '|'])
                            .is_empty();
                        continue;
                    }
                    if !at_command {
                        continue;
                    }
                    if is_env_assignment(w) {
                        only_assignments = true;
                        continue;
                    }
                    if matches!(
                        w.as_str(),
                        "!" | "{" | "}" | "if" | "then" | "else" | "elif" | "while" | "until"
                            | "do" | "time" | "command" | "builtin"
                    ) {
                        continue;
                    }
                    scan.commands.push(w.clone());
                    last_command = Some(n);
                    at_command = false;
                    only_assignments = false;
                }
            }
        }
        if only_assignments {
            scan.assigns = true;
        }
        scan
    }
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

/// The words of a command line as the program it runs is given them: the
/// shell takes the redirections (`2>/dev/null`, `2>&1`, `> $@`) for itself.
fn program_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut target = false;
    for (word, quoted) in tokenize_quoted(cmd) {
        if std::mem::take(&mut target) {
            continue;
        }
        let operator = word.trim_start_matches(|c: char| c.is_ascii_digit());
        if !quoted && (operator.starts_with(['<', '>']) || operator.starts_with("&>")) {
            // A bare operator takes the next word as its target.
            target = operator.trim_start_matches(['<', '>', '&', '|']).is_empty();
            continue;
        }
        words.push(word);
    }
    words
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
        let filter = steps
            .iter()
            .position(|s| matches!(s, Step::Op(Op::Filter(_)) | Step::Partial { op: Op::Filter(_), .. }))
            .expect("a filter step");
        assert!(
            matches!(
                &steps[..filter],
                [.., Step::Op(Op::Prefixes { prefix, .. })]
                    if prefix == &["uberon: http://purl.obolibrary.org/obo/uberon/core#"]
            ),
            "the --prefix binding must reach the plan, before the filter: {steps:?}"
        );
    }

    /// Every option of an `annotate` command line reaches its step, each with as
    /// many values as ROBOT reads for it: `-k` is the link annotation and `-l`
    /// the language annotation, and `-x` takes three values, read in pairs
    /// across its occurrences.
    #[test]
    fn annotate_records_every_option() {
        let steps = parse_command(
            "robot annotate -i in.owl -a rdfs:comment c -k rdfs:seeAlso ex:y -l rdfs:label chien fr \
             -t rdfs:comment 5 xsd:integer -x rdfs:comment x rdfs:comment -x y dc:source s \
             -A extra.owl -e true -d true -f true -R -O http://example.org/x.owl -o out.owl",
            "robot",
        );
        let a = steps
            .iter()
            .find_map(|s| match s {
                Step::Op(Op::Annotate(a)) => Some(a.clone()),
                _ => None,
            })
            .expect("an annotate step");
        let pair = |p: &str, v: &str| (p.to_string(), v.to_string());
        let triple = |p: &str, v: &str, x: &str| (p.to_string(), v.to_string(), x.to_string());
        assert_eq!(a.annotations, vec![pair("rdfs:comment", "c")]);
        assert_eq!(a.link_annotations, vec![pair("rdfs:seeAlso", "ex:y")]);
        assert_eq!(a.language_annotations, vec![triple("rdfs:label", "chien", "fr")]);
        assert_eq!(a.typed_annotations, vec![triple("rdfs:comment", "5", "xsd:integer")]);
        assert_eq!(a.axiom_annotations, ["rdfs:comment", "x", "rdfs:comment", "y", "dc:source", "s"]);
        assert_eq!(a.annotation_files, ["extra.owl"]);
        assert!(a.interpolate && a.annotate_defined_by && a.annotate_derived_from && a.remove_annotations);
        assert_eq!(a.ontology_iri.as_deref(), Some("http://example.org/x.owl"));
    }

    /// A template's `--force true` reaches the plan, and its absence does too.
    #[test]
    fn template_records_whether_it_is_forced() {
        let forced = |cmd: &str| {
            parse_command(cmd, "robot")
                .iter()
                .find_map(|s| match s {
                    Step::Op(Op::Template { force, .. }) => Some(*force),
                    _ => None,
                })
                .expect("a template step")
        };
        assert!(forced("robot template --template t.tsv --force true -o x.owl"));
        assert!(forced("robot template --template t.tsv -f true -o x.owl"));
        assert!(!forced("robot template --template t.tsv -o x.owl"));
        assert!(!forced("robot template --template t.tsv --force false -o x.owl"));
    }

    /// A template's merge switches reach the plan as ROBOT 1.9.11 runs them.
    /// `--merge-before`, and a `--merge-after` that writes nothing, merge, and
    /// the IRIs the command names set nothing then. A `--merge-after` that
    /// ends its chain writes the generated axioms alone; part way through one,
    /// it would write one ontology and go on with another, which the plan
    /// refuses by name. The two switches together, a merge with no input and a
    /// value after a switch fail as ROBOT's do.
    #[test]
    fn mireot_and_ancestors_are_recorded_as_robot_reads_them() {
        let extract = |cmd: &str| -> Op {
            parse_command(cmd, "robot")
                .iter()
                .find_map(|s| match s {
                    Step::Op(op @ Op::Extract { .. }) => Some(op.clone()),
                    _ => None,
                })
                .expect("an extract step")
        };
        let cmd = "robot extract -i i.owl --method MIREOT --lower-terms l.txt --lower-term X:1 --upper-term X:2 \
                   --upper-terms u.txt -b X:3 -B b.txt --intermediates none -O http://x/o --annotate-with-source true \
                   --sources s.txt --imports exclude -o x.owl";
        let Op::Extract {
            lower_terms, lower_term_files, upper_terms, upper_term_files, branch_from_terms, branch_from_term_files,
            intermediates, ..
        } = extract(cmd)
        else {
            unreachable!()
        };
        assert_eq!(
            (lower_terms, lower_term_files, upper_terms, upper_term_files, branch_from_terms, branch_from_term_files),
            (
                vec!["X:1".to_string()],
                vec!["l.txt".to_string()],
                vec!["X:2".to_string()],
                vec!["u.txt".to_string()],
                vec!["X:3".to_string()],
                vec!["b.txt".to_string()]
            )
        );
        assert_eq!(intermediates.as_deref(), Some("none"));
        // MIREOT names no module IRI, and the options that trim or annotate a
        // locality module set nothing: read, and nothing left over.
        let steps = parse_command(cmd, "robot");
        assert!(steps.iter().all(|s| s.unrunnable_gaps().is_empty()), "{steps:?}");
        assert!(!steps.iter().any(|s| matches!(s, Step::Op(Op::Annotate(_)))), "{steps:?}");
        let bot = parse_command("robot extract -i i.owl --method BOT -T t.txt -O http://x/o --imports exclude -o x.owl", "robot");
        assert!(bot.iter().any(|s| matches!(s, Step::Op(Op::Annotate(_)))), "{bot:?}");
        assert!(bot.iter().any(|s| matches!(s, Step::UnsupportedOptions { .. })), "{bot:?}");
        let ancestors = |cmd: &str| {
            parse_command(cmd, "robot").iter().find_map(|s| match s {
                Step::Op(Op::Template { ancestors, .. }) => Some(*ancestors),
                _ => None,
            })
        };
        assert_eq!(ancestors("robot template -i i.owl -t t.tsv --ancestors -o x.owl"), Some(true));
        assert_eq!(ancestors("robot template -i i.owl -t t.tsv -a -o x.owl"), Some(true));
        assert_eq!(ancestors("robot template -i i.owl -t t.tsv -o x.owl"), Some(false));
        assert!(
            parse_command("robot template -i i.owl -t t.tsv --ancestors true -o x.owl", "robot")
                .iter()
                .any(|s| matches!(s, Step::Refused { message } if message.contains("UNKNOWN ARG ERROR"))),
            "--ancestors true was read"
        );
    }

    #[test]
    fn template_records_its_merge_as_robot_runs_it() {
        let template = |cmd: &str| -> (bool, bool) {
            parse_command(cmd, "robot")
                .iter()
                .find_map(|s| match s {
                    Step::Op(Op::Template { merge, collapse_import_closure, .. }) => {
                        Some((*merge, *collapse_import_closure))
                    }
                    _ => None,
                })
                .expect("a template step")
        };
        assert_eq!(template("robot template -i i.owl -t t.tsv --merge-before -o x.owl"), (true, false));
        assert_eq!(template("robot template -i i.owl -t t.tsv -m --collapse-import-closure true -o x.owl"), (true, true));
        assert_eq!(
            template("robot template -i i.owl -t t.tsv --merge-after annotate --annotation rdfs:comment c -o x.owl"),
            (true, false)
        );
        assert_eq!(
            template("robot template -i i.owl -t t.tsv --merge-after --collapse-import-closure true -o x.owl"),
            (false, false)
        );
        assert_eq!(template("robot template -i i.owl -t t.tsv --collapse-import-closure true -o x.owl"), (false, false));
        assert_eq!(template("robot convert -i i.owl template -t t.tsv --merge-before -o x.owl"), (true, false));
        let annotates = |cmd: &str| parse_command(cmd, "robot").iter().any(|s| matches!(s, Step::Op(Op::Annotate(_))));
        assert!(!annotates("robot template -i i.owl -t t.tsv --merge-before -O http://x/o -V http://x/v -o x.owl"));
        assert!(!annotates("robot template -i i.owl -t t.tsv -M -O http://x/o -o x.owl"));
        assert!(annotates("robot template -i i.owl -t t.tsv -O http://x/o -o x.owl"));
        let steps = parse_command(
            "robot template -i i.owl -t t.tsv -m --include-annotations true --collapse-import-closure false -o x.owl",
            "robot",
        );
        assert!(steps.iter().all(|s| s.unrunnable_gaps().is_empty()), "{steps:?}");
        let refused = |cmd: &str| -> String {
            parse_command(cmd, "robot")
                .iter()
                .find_map(|s| match s {
                    Step::Refused { message } => Some(message.clone()),
                    Step::UnsupportedOptions { options, .. } => Some(options.join(", ")),
                    _ => None,
                })
                .unwrap_or_default()
        };
        assert_eq!(
            refused("robot template -i i.owl -t t.tsv --merge-before --merge-after -o x.owl"),
            "MERGE ERROR merge-before and merge-after cannot be combined"
        );
        assert_eq!(
            refused("robot template -t t.tsv --merge-before -o x.owl"),
            "template: --merge-before has no input ontology to merge into"
        );
        assert_eq!(
            refused("robot template -i i.owl -t t.tsv --merge-before true -o x.owl"),
            "UNKNOWN ARG ERROR unknown command or option: true"
        );
        assert_eq!(
            refused("robot template -i i.owl -t t.tsv --merge-after -o mid.owl annotate --annotation rdfs:comment c -o x.owl"),
            "--merge-after, --output mid.owl"
        );
    }

    /// A materialize step keeps the reasoner it checks the ontology with, and
    /// whether it only checks it (`-n/--create-new-ontology`).
    #[test]
    fn materialize_records_its_reasoner_and_whether_it_keeps_the_input() {
        let step = |cmd: &str| {
            parse_command(cmd, "robot")
                .iter()
                .find_map(|s| match s {
                    Step::Op(Op::Materialize { reasoner, create_new_ontology, properties, .. }) => {
                        Some((reasoner.clone(), *create_new_ontology, properties.clone()))
                    }
                    _ => None,
                })
                .expect("a materialize step")
        };
        assert_eq!(
            step("robot materialize --reasoner hermit --create-new-ontology true --term BFO:0000050 -o x.owl"),
            (Some("hermit".to_string()), Some(true), vec!["BFO:0000050".to_string()])
        );
        assert_eq!(step("robot materialize -r WHELK -n false -o x.owl"), (Some("WHELK".to_string()), Some(false), vec![]));
        assert_eq!(step("robot materialize --term BFO:0000050 -o x.owl").0, None);
    }

    /// A collapse step records its threshold as the recipe writes it, so one
    /// that is not an integer of at least 2 fails the step rather than
    /// collapsing at the default.
    #[test]
    fn collapse_records_its_threshold_as_written() {
        let threshold = |cmd: &str| {
            parse_command(cmd, "robot")
                .iter()
                .find_map(|s| match s {
                    Step::Op(Op::Collapse { threshold, .. }) => Some(threshold.clone()),
                    _ => None,
                })
                .expect("a collapse step")
        };
        assert_eq!(threshold("robot collapse --threshold 3 -o x.owl"), Some("3".to_string()));
        assert_eq!(threshold("robot collapse -t x -o x.owl"), Some("x".to_string()));
        assert_eq!(threshold("robot collapse --precious EX:1 -o x.owl"), None);
    }

    /// Each command of a chain reads its CURIEs with the prefix options stated
    /// before the first command and its own, and the command after it with the
    /// first alone. `-p` is `--prefix` only where the command has no `-p` of its
    /// own: on `remove` it is `--preserve-structure`.
    #[test]
    fn each_command_reads_with_the_chains_prefix_options_and_its_own() {
        let steps = parse_command(
            "robot --add-prefixes config/context.json template --add-prefixes extra.json \
             -p 'zz: http://example.org/zz#' --template t.tsv \
             annotate --ontology-iri http://example.org/t.owl \
             remove -p false --term zz:A -o t.owl",
            "robot",
        );
        let at = |is: fn(&Op) -> bool| {
            steps
                .iter()
                .position(|s| matches!(s, Step::Op(op) | Step::Partial { op, .. } if is(op)))
                .expect("the command's step")
        };
        let template = at(|op| matches!(op, Op::Template { .. }));
        let annotate = at(|op| matches!(op, Op::Annotate(_)));
        let prefixes: Vec<(usize, Vec<String>, Vec<String>)> = steps
            .iter()
            .enumerate()
            .filter_map(|(k, s)| match s {
                Step::Op(Op::Prefixes { add_prefixes, prefix, .. }) => {
                    Some((k, add_prefixes.clone(), prefix.clone()))
                }
                _ => None,
            })
            .collect();
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            prefixes,
            vec![
                (
                    template - 1,
                    strings(&["config/context.json", "extra.json"]),
                    strings(&["zz: http://example.org/zz#"])
                ),
                (annotate - 1, strings(&["config/context.json"]), vec![]),
            ],
            "{steps:?}"
        );
    }

    /// `filter --axioms internal` judges each axiom's subjects against the
    /// `--base-iri` namespaces, so the plan carries them: without them no axiom
    /// is internal, and `--axioms external` takes every one.
    #[test]
    fn filter_records_its_base_iris() {
        let steps = parse_command(
            "robot filter -i x.owl --base-iri http://purl.obolibrary.org/obo/X_ \
             --base-iri http://purl.obolibrary.org/obo/x# --axioms internal -o y.owl",
            "robot",
        );
        let spec = steps
            .iter()
            .find_map(|s| match s {
                Step::Op(Op::Filter(f)) => Some(f),
                _ => None,
            })
            .expect("a filter step owlmake runs");
        assert_eq!(
            spec.base_iri,
            vec!["http://purl.obolibrary.org/obo/X_", "http://purl.obolibrary.org/obo/x#"]
        );
    }

    /// Every option `remove` and `filter` share reaches the step, spelt long or
    /// short, its values in recipe order; a filter's `-O` is the annotate after
    /// it.
    #[test]
    fn remove_and_filter_record_every_option() {
        let options = "-t A --term B -T t.txt -n C --include-term D -N i.txt --include-terms j.txt \
                       -e E --exclude-term F -E e.txt -s parents --select 'self children' -a subclass \
                       --axioms logical --base-iri http://example.org/x_ -r false -S true -p false \
                       --allow-punning true -d rdfs:comment --drop-axiom-annotations 'IAO:0000115=~^x'";
        let want = SelectionSpec {
            terms: vec!["A".into(), "B".into()],
            term_files: vec!["t.txt".into()],
            include_terms: vec!["C".into(), "D".into()],
            include_term_files: vec!["i.txt".into(), "j.txt".into()],
            exclude_terms: vec!["E".into(), "F".into()],
            exclude_term_files: vec!["e.txt".into()],
            selects: vec!["parents".into(), "self children".into()],
            axioms: vec!["subclass".into(), "logical".into()],
            base_iri: vec!["http://example.org/x_".into()],
            trim: Some(false.into()),
            signature: Some(true.into()),
            preserve_structure: Some(false.into()),
            allow_punning: Some(true.into()),
            drop_axiom_annotations: vec!["rdfs:comment".into(), "IAO:0000115=~^x".into()],
        };
        let steps = parse_command(&format!("robot remove -i x.owl {options} -o y.owl"), "robot");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Remove(spec)) if spec == &want)),
            "{steps:?}"
        );
        let steps = parse_command(&format!("robot filter -i x.owl {options} -O http://example.org/y.owl -o y.owl"), "robot");
        let filter = steps
            .iter()
            .position(|s| matches!(s, Step::Op(Op::Filter(spec)) if spec == &want))
            .unwrap_or_else(|| panic!("{steps:?}"));
        assert!(
            matches!(&steps[filter + 1], Step::Op(Op::Annotate(a))
                if a.ontology_iri.as_deref() == Some("http://example.org/y.owl")),
            "{steps:?}"
        );
    }

    /// A command reads its options as the line gives them: a token that is no
    /// option is a value (`--format --csv`), a switch of extract or reduce is on
    /// for `true` or `yes` in any case and off for anything else, and a
    /// redirection is the shell's.
    #[test]
    fn a_command_reads_its_options_as_the_line_gives_them() {
        let steps = parse_command("robot query --input x.owl --format --csv --query q.sparql out.csv 2>/dev/null", "robot");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Query { format, selects, .. })
                if format.as_deref() == Some("--csv") && selects == &[("q.sparql".to_string(), "out.csv".to_string())])),
            "{steps:?}"
        );
        for (value, on) in [("Yes", true), (" TRUE ", true), ("on", false)] {
            let line = format!("robot extract -i x.owl --method BOT -T t.txt --force '{value}' -o y.owl");
            let steps = parse_command(&line, "robot");
            assert!(steps.iter().any(|s| matches!(s, Step::Op(Op::Extract { force, .. }) if *force == on)), "{line}: {steps:?}");
        }
        let steps = parse_command("robot reduce -i x.owl -p yes --named-classes-only TRUE -s True -o y.owl", "robot");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Reduce {
                preserve_annotated_axioms: true,
                named_classes_only: true,
                include_subproperties: Some(true),
                ..
            }))),
            "{steps:?}"
        );
        let steps = parse_command("robot reduce -i x.owl -o y.owl", "robot");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Reduce {
                preserve_annotated_axioms: false,
                named_classes_only: false,
                include_subproperties: None,
                ..
            }))),
            "{steps:?}"
        );
        let steps = parse_command(
            "robot merge -i a.owl -I http://example.org/b.owl -d true -a true --annotate-derived-from false -o y.owl",
            "robot",
        );
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Merge {
                inputs,
                collapse_import_closure: None,
                include_annotations: true,
                annotate_defined_by: true,
                annotate_derived_from: false,
            }) if inputs == &["a.owl", "http://example.org/b.owl"])),
            "{steps:?}"
        );
        // The files a pattern matches are known when the command runs, and the
        // plan must name them, so the pattern is a gap.
        let steps = parse_command("robot merge -i a.owl --inputs 'parts/*.owl' -o y.owl", "robot");
        let gaps: Vec<String> = steps.iter().flat_map(Step::unrunnable_gaps).collect();
        assert_eq!(gaps, ["unsupported option `merge --inputs parts/*.owl`"], "{steps:?}");
    }

    /// An option a command is given that its step does not read is a gap that
    /// names it, and a line its commands cannot read is refused with the
    /// message that says why.
    #[test]
    fn an_option_no_step_reads_is_named_and_an_unreadable_line_refused() {
        let steps = parse_command("robot reason -i x.owl --include-indirect true -D unsat.txt -o y.owl", "robot");
        let gaps: Vec<String> = steps.iter().flat_map(Step::unrunnable_gaps).collect();
        assert_eq!(
            gaps,
            [
                "unsupported option `reason --include-indirect true`",
                "unsupported option `reason --dump-unsatisfiable unsat.txt`"
            ],
            "{steps:?}"
        );
        for (line, message) in [
            ("robot convert -i x.owl --no-check -o y.obo", "UNKNOWN ARG ERROR unknown command or option: --no-check"),
            ("robot convert -i x.owl y.obo", "UNKNOWN ARG ERROR unknown command or option: y.obo"),
            ("robot remove -i x.owl --term", "Missing argument for option: t"),
        ] {
            let steps = parse_command(line, "robot");
            assert!(
                matches!(steps.last(), Some(Step::Refused { message: m }) if m == message),
                "{line}: {steps:?}"
            );
        }
        // owlmake's own options are read with ROBOT's.
        let steps = parse_command("robot reason -i x.owl --axiom-generators PropertyAssertion --properties ex:p -o y.owl", "robot");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Reason { properties, .. }) if properties == &["ex:p"])),
            "{steps:?}"
        );
        assert!(steps.iter().all(|s| s.unrunnable_gaps().is_empty()), "{steps:?}");
    }

    /// A switch is read as its command reads it. One the command reads as
    /// `true` or `false` exactly refuses any other value where the command
    /// reads it, and the first such switch the command reads names the
    /// refusal; `remove` and `filter` read theirs only once something is
    /// selected, so their steps carry the text. One read leniently is on for
    /// `true` or `yes` in any case and off for anything else.
    #[test]
    fn a_switch_is_read_as_its_command_reads_it() {
        let refused = |line: &str| match parse_command(line, "robot").last() {
            Some(Step::Refused { message }) => message.clone(),
            other => panic!("{line}: expected a refusal, got {other:?}"),
        };
        for (line, switch) in [
            ("robot annotate -i x.owl --annotation rdfs:comment c --interpolate yes -o y.owl", "interpolate"),
            ("robot merge -i a.owl --collapse-import-closure TRUE -o y.owl", "collapse-import-closure"),
            ("robot merge -i a.owl --annotate-defined-by True --include-annotations 1 -o y.owl", "include-annotations"),
            ("robot convert -i x.owl --check FALSE -o y.obo", "check"),
            ("robot extract -i x.owl -m BOT -t ex:A --copy-ontology-annotations yes -o y.owl", "copy-ontology-annotations"),
            ("robot relax -i x.owl --exclude-named-classes 1 -o y.owl", "exclude-named-classes"),
            ("robot rename -i x.owl --mappings m.tsv --allow-duplicates yes -o y.owl", "allow-duplicates"),
            ("robot repair -i x.owl --invalid-references yes -o y.owl", "invalid-references"),
            ("robot template -t t.tsv --include-annotations nope -o y.owl", "include-annotations"),
            ("robot query -i x.owl --use-graphs True --query q.rq out.csv", "use-graphs"),
            ("robot query -i x.owl --update u.ru --temporary-file yes -o y.owl", "temporary-file"),
            ("robot query -i x.owl --tdb true --keep-tdb-mappings yes --query q.rq out.csv", "keep-tdb-mappings"),
        ] {
            assert_eq!(refused(line), format!("BOOLEAN VALUE ERROR arg for {switch} must be true or false"), "{line}");
        }
        // Where the command does not read a switch, the switch says nothing.
        for line in [
            "robot query -i x.owl --tdb true --use-graphs nope --query q.rq out.csv",
            "robot query -i x.owl --update u.ru --use-graphs nope --tdb maybe -o y.owl",
        ] {
            let steps = parse_command(line, "robot");
            assert!(steps.iter().all(|s| !matches!(s, Step::Refused { .. }) && s.unrunnable_gaps().is_empty()), "{line}: {steps:?}");
        }
        let steps = parse_command("robot query -i x.owl --tdb true --use-graphs true --query q.rq out.csv", "robot");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Query { use_graphs: false, tdb: true, .. }))),
            "{steps:?}"
        );
        let steps = parse_command("robot repair -i x.owl --invalid-references false --merge-axiom-annotations true -o y.owl", "robot");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Repair { invalid_references: false, merge_axiom_annotations: true, .. }))),
            "{steps:?}"
        );
        // `repair` migrates whenever it does not merge, carries the annotation
        // properties it moves, and keeps the ontology's IRI whatever
        // `--output-iri` says.
        let steps = parse_command(
            "robot repair -i x.owl -r false -a oboInOwl:hasDbXref --annotation-property rdfs:comment -A props.txt \
             -O http://example.org/y.owl -o y.owl",
            "robot",
        );
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Repair {
                invalid_references: true,
                merge_axiom_annotations: false,
                annotation_properties,
                annotation_properties_file: Some(file),
            }) if annotation_properties == &["oboInOwl:hasDbXref", "rdfs:comment"] && file == "props.txt")),
            "{steps:?}"
        );
        assert!(
            !steps.iter().any(|s| matches!(s, Step::Op(Op::Annotate(_)) | Step::UnsupportedOptions { .. })),
            "{steps:?}"
        );
        // `remove` and `filter` carry the text to where they read it.
        let steps = parse_command("robot remove -i x.owl --term ex:A --trim TRUE --allow-punning yes -o y.owl", "robot");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Remove(spec))
                if spec.trim == Some(Switch::Text("TRUE".into())) && spec.allow_punning == Some(Switch::Text("yes".into())))),
            "{steps:?}"
        );
        // Read leniently.
        let steps = parse_command(
            "robot reason -i x.owl --exclude-owl-thing Yes --annotate-inferred-axioms nope --create-new-ontology TRUE -o y.owl",
            "robot",
        );
        assert!(
            steps.iter().any(|s| matches!(s, Step::Op(Op::Reason {
                exclude_owl_thing: Some(true),
                annotate_inferred_axioms: Some(false),
                create_new_ontology: Some(true),
                ..
            }))),
            "{steps:?}"
        );
        let steps = parse_command("robot template -t t.tsv --force YES -o y.owl", "robot");
        assert!(steps.iter().any(|s| matches!(s, Step::Op(Op::Template { force: true, .. }))), "{steps:?}");
        // A switch no step reads is no gap when it asks for what the step does
        // without it.
        let steps = parse_command("robot relax -i x.owl --exclude-named-classes true --enforce-obo-format false -o y.owl", "robot");
        assert!(steps.iter().all(|s| s.unrunnable_gaps().is_empty()), "{steps:?}");
        let gaps: Vec<String> = parse_command("robot relax -i x.owl --enforce-obo-format true -o y.owl", "robot")
            .iter()
            .flat_map(Step::unrunnable_gaps)
            .collect();
        assert_eq!(gaps, ["unsupported option `relax --enforce-obo-format true`"]);
    }

    /// A tolerated ontology command is an op that may fail, not a command line.
    ///
    /// EFO's mondo import excludes HGNC terms with a query whose failure the
    /// recipe walks past. The redirection and the `|| true` around it are the
    /// shell's, and neither changes what the command does, so both are read and
    /// the query itself is what the plan names.
    #[test]
    fn a_tolerated_robot_command_is_parsed_and_marked_may_fail() {
        let steps = parse_command(
            "bin/robot query -i imports/mondo_import.owl.tmp.owl \
             -q ../sparql/hgnc_terms.sparql imports/mondo_import.owl.hgnc.tsv \
             2>/dev/null || true",
            "bin/robot",
        );
        let inner = steps
            .iter()
            .find_map(|s| match s {
                Step::MayFail(inner) => Some(inner.as_ref()),
                _ => None,
            })
            .expect("the tolerated command is a step that may fail");
        match inner {
            Step::Op(Op::Query { selects, .. }) => assert_eq!(
                selects,
                &[(
                    "../sparql/hgnc_terms.sparql".to_string(),
                    "imports/mondo_import.owl.hgnc.tsv".to_string()
                )]
            ),
            other => panic!("expected a native query, got {other:?}"),
        }
        assert!(
            !steps.iter().any(|s| matches!(s, Step::Shell { .. })),
            "nothing is left for a shell — and so nothing names a robot binary"
        );
    }

    /// A tolerated command owlmake does NOT implement stays one shell command.
    ///
    /// MONDO's OMIM-gene check is a pipeline of text tools whose `grep` exits 1
    /// when it matches nothing — the passing case. Taking it apart would buy
    /// nothing and cost the shell's own short-circuiting.
    #[test]
    fn a_tolerated_shell_pipeline_is_left_whole() {
        let steps = parse_command(
            "grep -Ff $< mondo-edit.obo | grep '^xref' > omim.txt || true",
            "robot",
        );
        assert!(
            matches!(steps.as_slice(), [Step::Shell { command, .. }] if command.ends_with("|| true")),
            "expected one tolerated shell command, got {steps:?}"
        );
    }
}

#[cfg(test)]
mod shell_state_tests {
    use super::*;

    /// A part that changes the shell, or reads what an earlier part left, ties
    /// the line to one shell.
    #[test]
    fn parts_that_share_the_shell_are_seen() {
        for line in [
            "mkdir -p sub && cd sub && pwd > ../where.txt",
            "mkdir -p sub; cd sub; pwd > ../out.txt",
            "export x=1 && sh -c 'echo x=$x' > out.txt",
            "x=1; echo $x > out.txt",
            "false; echo $? > out.txt",
            "umask 077 && touch secret",
            "set -e; false; touch never",
            "f() { touch $1; }; f made",
            "{ cd sub; } && touch here",
            "-cd sub && touch here",
            "sleep 1 & touch a",
            "grep '^ERROR' hp_report && exit -1 || echo \"No errors\"",
            "command cd sub && ls",
        ] {
            assert!(shares_shell_state(line), "{line}");
        }
    }

    /// Parts that only run commands do not, whatever their arguments say.
    #[test]
    fn independent_parts_are_not() {
        for line in [
            "mkdir -p a/b && touch a/b/c.txt ; touch a/d.txt",
            "robot convert -i x.owl -o y.owl && cp y.owl z.owl",
            "grep -v cd notes.txt > out.txt || true",
            "echo cd export set > words.txt && cat words.txt",
            "X=1 make y && touch done",
            "robot report -i x.owl --fail-on none -o r.tsv 2>&1 > log.txt && cat log.txt",
            "( cd sub && make x )",
            "cd sub",
            "echo '$?' > literal.txt && cat literal.txt",
        ] {
            assert!(!shares_shell_state(line), "{line}");
        }
    }

    /// A command holding a shell expansion is recorded as the command line it
    /// is, whatever its program: an op or a native file operation would keep
    /// `$X` as written.
    #[test]
    fn ingest_leaves_an_expansion_to_the_shell() {
        for line in ["echo $HOME > home.txt", "robot convert -i $SRC -o out.owl"] {
            match parse_command(line, "robot").as_slice() {
                [Step::Shell { command, .. }] => assert_eq!(command, line),
                other => panic!("{line}: expected one shell step, got {other:?}"),
            }
        }
        assert!(matches!(parse_command("echo '$HOME' > literal.txt", "robot").as_slice(), [Step::File(_)]));
    }

    /// Ingest keeps such a line one shell step, so the plan does not split what
    /// the shell would have shared.
    #[test]
    fn ingest_keeps_a_shared_line_whole() {
        let line = "cd src/patterns && make test";
        match parse_command(line, "robot").as_slice() {
            [Step::Shell { command, requires }] => {
                assert_eq!(command, line);
                assert!(requires.is_empty(), "{requires:?}");
            }
            other => panic!("expected one shell step, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod requires_tests {
    use super::*;

    fn requires(cmd: &str) -> Vec<String> {
        match shell_step(cmd.to_string()) {
            Step::Shell { requires, .. } => requires,
            other => panic!("expected a shell step, got {other:?}"),
        }
    }

    /// A quoted regex is one argument however many `|` it holds. UBERON's
    /// orphan report pipes four stanza-filter calls, two of them over
    /// alternations, and the only thing the machine has to provide is the script
    /// — and when the script is `obo-grep.pl`, owlmake provides that too.
    #[test]
    fn a_quoted_alternation_is_not_a_pipeline() {
        let cmd = r#"../scripts/obo-filter.pl --neg -r "(is_a|intersection_of|is_obsolete):" uberon.obo |  ../scripts/obo-filter.pl -r Term - |  ../scripts/obo-filter.pl --neg -r "id: UBERON:(0001062|0000000)" - |  ../scripts/obo-filter.pl -r Term - > reports/uberon-orphans.tmp"#;
        assert_eq!(requires(cmd), vec!["../scripts/obo-filter.pl"]);
        assert!(requires(&cmd.replace("obo-filter.pl", "obo-grep.pl")).is_empty());
        let cmd = "(egrep '^(id|name):'  reports/uberon-orphans.tmp > reports/uberon-orphans || echo ok)";
        assert!(requires(cmd).is_empty(), "{:?}", requires(cmd));
    }

    /// Each simple command contributes its own program word, and a program on
    /// PATH is named bare.
    #[test]
    fn every_command_of_a_pipeline_is_seen() {
        assert_eq!(requires("git show HEAD:x.owl | robot convert -o y.owl"), vec!["git"]);
        assert_eq!(requires("wget -O a b && gzip -d a; curl x"), vec!["wget", "curl"]);
        assert_eq!(requires("FOO=1 ./run.sh | sort"), vec!["./run.sh"]);
    }
}
