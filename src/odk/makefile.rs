//! A focused reader for the Makefile language: enough of it to resolve ODK
//! release recipes faithfully — variables (`=`, `:=`, `?=`, `+=`), explicit and
//! pattern rules, line continuations, and the handful of functions ODK
//! Makefiles use (`patsubst`, `subst`, `foreach`, `addprefix`, `addsuffix`,
//! `wildcard`, `shell`). The override file is overlaid last so its definitions
//! win, which is the position `include <id>.Makefile` at the end of the standard
//! Makefile gives it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Rule {
    pub targets: Vec<String>,
    /// Normal prerequisites — what `$^` expands to, and whose first element is
    /// `$<`. Excludes anything after the `|` separator.
    pub prereqs: Vec<String>,
    /// Order-only prerequisites: everything after `|` in `target: normal | oo`.
    /// They must still be built before the target, but they are deliberately not
    /// part of `$^`/`$<` — ODK uses them for directory targets (`… | tmp`) and
    /// for `all_robot_plugins`. Splitting them off here also keeps the separator
    /// itself, which is punctuation and never a filename, out of `prereqs`.
    pub order_only: Vec<String>,
    /// Raw (unexpanded) recipe lines.
    pub recipe: Vec<String>,
    /// The variables consulted by the `ifeq`/`ifneq` conditionals enclosing the
    /// rule line that supplied this rule's RECIPE.
    ///
    /// Ingest evaluates those conditionals with every workflow flag bound true,
    /// so a recipe recorded from inside one exists only under that binding —
    /// flipping the flag selects the other branch and this recipe with it. The
    /// refresh groups carry that: a `BRI`-guarded target is pinned by
    /// `BRI=false`, which demands the file exist and rebuilds nothing, exactly as
    /// the branch that is not recorded leaves the committed file alone.
    pub guards: Vec<String>,
}

#[derive(Debug, Default)]
pub struct MakeModel {
    /// Raw variable values (recursive `=` semantics: expanded on demand).
    pub vars: HashMap<String, String>,
    /// Names bound on the COMMAND LINE. A command-line binding overrides every
    /// in-file assignment, so an `IMP=true` line in the Makefile must not undo a
    /// `make IMP=false` on the command line.
    pub command_line_vars: std::collections::HashSet<String>,
    /// Targets declared `.PHONY`: they name no file, so they are always out of
    /// date. Recorded at ingest and carried in the plan, so execution can apply
    /// the up-to-date rule to real file targets without treating a phony
    /// aggregate as satisfied by a same-named file.
    pub phony: std::collections::HashSet<String>,
    /// The repo's default goal: the first explicit, non-pattern target the
    /// Makefile declares, and so the target a bare build makes. EFO's is
    /// `all: all_imports all_gwas all_components release qc` — a release AND its
    /// QC — so a bare build that stopped at the release artefacts would quietly
    /// stop running the checks.
    pub default_goal: Option<String>,
    /// Variables this Makefile's CONDITIONALS consult (`ifeq ($(IMP),true)`),
    /// each with the value in force when the first conditional consulted it.
    ///
    /// Setting one of these changes which rules exist at all, so a recursive
    /// `make IMP=true …` that assigns one cannot be flattened into a plain
    /// dependency — the sub-make would be reading a different Makefile. Recorded
    /// during the parse because conditionals are evaluated and discarded there:
    /// nothing downstream can tell that `IMP` ever mattered. The VALUES go into
    /// the plan, which is how a build with no configuration left can still tell a
    /// switch it cannot honour from a name that means nothing.
    pub cond_vars: std::collections::BTreeMap<String, String>,
    /// The subset of [`cond_vars`] a conditional compares against a BOOLEAN word
    /// — `ifeq ($(BRI),true)`. Those are the configuration's switches, and each
    /// one that guards a rule becomes a refresh group a run can pin.
    ///
    /// The distinction is what keeps `ifneq ($(SPARQL_VALIDATION_QUERIES),)` out:
    /// that asks whether a list is EMPTY, and "keep the queries" is not a request
    /// anyone can make. A switch is a question with two answers; a presence test
    /// is a fact about the repository.
    ///
    /// [`cond_vars`]: MakeModel::cond_vars
    pub switch_vars: std::collections::BTreeSet<String>,
    /// The release version this configuration produces when the run says nothing:
    /// either a literal it pins, or [`VERSION_TODAY`] when it is the date of the
    /// build. Set by [`MakeModel::bind_release_version`].
    ///
    /// [`VERSION_TODAY`]: crate::plan::VERSION_TODAY
    pub version_default: String,
    /// The file a backtick substitution reads the release version out of,
    /// relative to [`base_dir`](Self::base_dir). EFO stamps
    /// `` v`cat version.txt` `` into every release version IRI.
    ///
    /// Recorded during expansion, because that is the only place the command is
    /// seen, and behind a `RefCell` because expansion runs behind `&self`. It
    /// reaches the plan as [`Plan::version_file`], so a run reads the version the
    /// file holds NOW rather than the one it held when the plan was written.
    ///
    /// [`Plan::version_file`]: crate::plan::Plan::version_file
    pub version_file: std::cell::RefCell<Option<String>>,
    /// Explicit rules keyed by (already-expanded) target name; last wins.
    pub rules: HashMap<String, Rule>,
    /// Pattern rules in declaration order; last matching wins.
    pub pattern_rules: Vec<Rule>,
    /// Directory the Makefile lives in (`src/ontology`). Recipes — and therefore
    /// `$(shell …)`/backtick command substitutions — are evaluated from here,
    /// because it is the directory their relative paths are written against
    /// (`cat version.txt` resolves against it, not against wherever `om` happened
    /// to be invoked). `None` falls back to the process cwd (edit-only / spec
    /// repos that have no Makefile recipes).
    pub base_dir: Option<PathBuf>,
    /// `$(name …)` references whose `name` is not a function this parser
    /// implements.
    ///
    /// A `$(…)` holding a space can only be a function call — no real Makefile
    /// names a variable that way — so treating an unimplemented one as a variable
    /// reference would silently yield the empty string and the build would go on
    /// having quietly lost whatever it was meant to compute. A release-artefact
    /// list built through such a call comes out empty, and a release that names no
    /// products succeeds having built none. Collected during expansion and
    /// reported by the caller, which is the only place that can fail.
    pub unknown_functions: std::cell::RefCell<std::collections::BTreeSet<String>>,
}

impl MakeModel {
    pub fn parse_file(path: &Path) -> Result<MakeModel> {
        Self::parse_file_with(path, &[])
    }

    /// [`parse_file`] with COMMAND-LINE variable assignments seeded before
    /// parsing.
    ///
    /// A command-line assignment overrides every in-file one and is in effect
    /// from the start, which matters because the conditionals are evaluated
    /// while parsing: OBA wraps its import rules in `ifeq ($(IMP),true)`, its
    /// mirror rules in `ifeq ($(MIR),true)` and its patterns in
    /// `ifeq ($(PAT),true)`. With `IMP=false MIR=false` those rules are never
    /// DEFINED, so `imports/merged_import.owl` is a plain source file. Binding
    /// the overrides only after the parse would leave the rules defined, and the
    /// build would re-mirror all twenty of OBA's imports when the run's own
    /// switches ask for none.
    ///
    /// ODK's own workflow-control flags are the exception: see [`WORKFLOW_FLAGS`].
    ///
    /// [`parse_file`]: MakeModel::parse_file
    /// [`WORKFLOW_FLAGS`]: MakeModel::WORKFLOW_FLAGS
    pub fn parse_file_with(path: &Path, overrides: &[(String, String)]) -> Result<MakeModel> {
        Self::parse_impl(path, overrides, &[])
    }

    /// Parse with some workflow flags bound to values other than `true`.
    ///
    /// Ingest resolves the repo with every flag TRUE so the plan is a function of
    /// the repo alone. A flag whose FALSE branch defines a DIFFERENT recipe for
    /// the same target — uPheno's `tmp/all_pattern_terms.txt` — is invisible to
    /// that parse, so ingest reads it here and records the consequence in the
    /// plan rather than leaving execution to guess it.
    pub fn parse_file_with_flags(
        path: &Path,
        overrides: &[(String, String)],
        flags: &[(&str, &str)],
    ) -> Result<MakeModel> {
        Self::parse_impl(path, overrides, flags)
    }

    /// ODK's workflow-control flags, bound TRUE before the parse, always.
    ///
    /// They gate whole rule sets (`ifeq ($(IMP),true)`), so the value in force
    /// during the parse decides which rules EXIST. Seeding them from the command
    /// line would make the plan a function of the invocation: `om make test
    /// IMP=false` would generate a plan with the import rules deleted — and
    /// `regen_plan` WRITES the plan it generates to `owlmake.yaml`, which would
    /// turn one run's switches into committed repo configuration.
    ///
    /// Bound true unconditionally, ingest is a pure function of the repo's
    /// committed files, and `om make IMP=false` and `om make` produce a
    /// byte-identical plan. Whether to BUILD a gated target is then a run
    /// decision, taken from `refresh_groups` (see `crate::plan::RefreshGroup`).
    pub const WORKFLOW_FLAGS: [&'static str; 5] = ["MIR", "IMP", "PAT", "COMP", "IMP_LARGE"];

    /// The release version, under whatever name the configuration gives it.
    ///
    /// It is a run input, so like a workflow flag it must not reach the parse.
    /// The configuration binds `TODAY ?= $(shell date …)`, and `?=` leaves an
    /// existing binding alone — so a value seeded here survives the parse and is
    /// expanded into every rule target built from it, freezing one release date
    /// into the plan: on MONDO, `sources/$(TODAY)/doid.owl` becomes
    /// `sources/2026-08-19/doid.owl` in 113 places. The plan then describes one
    /// date and no other, and a run under any other date is told its plan is
    /// stale — an instruction a repo with no build configuration cannot follow.
    ///
    /// Left out of the parse, `bind_release_version` resolves the version to
    /// [`VERSION_REF`] afterwards and the plan carries one field the run reads.
    ///
    /// [`VERSION_REF`]: crate::plan::VERSION_REF
    pub const RELEASE_VERSION_VARS: [&'static str; 2] = ["TODAY", "VERSION"];

    /// Whether a command-line variable is a run input rather than configuration,
    /// and so must not be bound while the configuration is parsed.
    fn is_run_input(name: &str) -> bool {
        Self::WORKFLOW_FLAGS.contains(&name) || Self::RELEASE_VERSION_VARS.contains(&name)
    }

    /// Resolve `$(VERSION)` to a REFERENCE, and record what it defaults to.
    ///
    /// Called once the whole configuration is parsed (the override file may
    /// redefine `VERSION`). `version_default` is what a run that says nothing
    /// gets; afterwards `$(VERSION)` and `$(TODAY)` both expand to
    /// [`VERSION_REF`], so the version reaches the plan as a reference to one
    /// field rather than as a date frozen into every string built from it.
    ///
    /// A configuration whose version is neither pinned nor derived from `TODAY`
    /// — one that calls `date` itself — still resolves to a fixed string here.
    ///
    /// [`VERSION_REF`]: crate::plan::VERSION_REF
    pub fn bind_release_version(&mut self) {
        // Probe first: bind `TODAY` to a string nothing else can produce and
        // expand `$(VERSION)`. The probe comes back exactly when the version is
        // date-derived, which answers the question without guessing from the
        // shape of a date — and it survives `VERSION = v$(TODAY)`, whose default
        // is then `v{today}`.
        const PROBE: &str = "\u{1}owlmake-today\u{1}";
        self.vars.insert("TODAY".to_string(), PROBE.to_string());
        self.command_line_vars.insert("TODAY".to_string());
        let configured = self.expand("$(VERSION)").trim().to_string();
        self.version_default = if configured.is_empty() {
            // A configuration that names no version releases under today's date.
            crate::plan::VERSION_TODAY.to_string()
        } else {
            configured.replace(PROBE, crate::plan::VERSION_TODAY)
        };
        for name in ["VERSION", "TODAY"] {
            self.vars.insert(name.to_string(), crate::plan::VERSION_REF.to_string());
            self.command_line_vars.insert(name.to_string());
        }
    }

    fn parse_impl(
        path: &Path,
        overrides: &[(String, String)],
        flags: &[(&str, &str)],
    ) -> Result<MakeModel> {
        let mut m = MakeModel::default();
        for f in Self::WORKFLOW_FLAGS {
            match flags.iter().find(|(k, _)| *k == f) {
                // An explicitly requested value has to WIN over the file's own
                // `PAT = true`, which is an ordinary assignment and would
                // otherwise overwrite it as the parse reaches it. That is what
                // make's command-line variables do, so bind it as one.
                Some((_, v)) => {
                    m.vars.insert(f.to_string(), v.to_string());
                    m.command_line_vars.insert(f.to_string());
                }
                None => {
                    m.vars.insert(f.to_string(), "true".to_string());
                }
            }
        }
        // A switch of the repository's OWN invention is bound the same way. It is
        // not in the list above, so it keeps whatever value the configuration
        // gives it unless this resolution asked for another — which is how the
        // recipe of a branch the plan did not take is read.
        for (k, v) in flags.iter().filter(|(k, _)| !Self::is_run_input(k)) {
            m.vars.insert(k.to_string(), v.to_string());
            m.command_line_vars.insert(k.to_string());
        }
        // `MAKE` is the variable a recipe recurses through rather than naming a
        // binary (`$(MAKE) IMP=true … all_imports -B` is the ODK's own
        // `refresh-imports`). It has to be bound: left undefined it expands to
        // nothing, turning such a line into a bare variable assignment that maps
        // to no step at all. Ingest resolves the command word to owlmake's own
        // `make`, the same rewrite it applies to the other tool words a recipe
        // can name (see `robot::parse_command`).
        m.vars.insert("MAKE".to_string(), "make".to_string());
        // A Makefile's relative paths are relative to its own directory, and the
        // parse itself resolves some: `ifeq` conditionals, and the `$(wildcard …)`
        // in a rule's prerequisite list. Bind the base directory BEFORE reading
        // the file, or those expansions answer against whatever directory the
        // process happens to have been launched from — and a repo planned from
        // its root would lose every DOSDP pattern named
        // `$(wildcard ../patterns/data/default/*.tsv)`.
        m.base_dir = path.parent().map(|d| d.to_path_buf());
        for (k, v) in overrides {
            // A run input on the command line does NOT reach the parse: a
            // workflow flag would change which rules exist, and the release
            // version would be frozen into every path built from it. The plan is
            // a function of the repo, not of the invocation; both are honoured at
            // BUILD time instead.
            if Self::is_run_input(&k) {
                continue;
            }
            m.vars.insert(k.clone(), v.clone());
            m.command_line_vars.insert(k.clone());
        }
        m.ingest(&std::fs::read_to_string(path)?)?;
        Ok(m)
    }

    pub fn overlay_file(&mut self, path: &Path) -> Result<()> {
        self.ingest(&std::fs::read_to_string(path)?)
    }

    fn ingest(&mut self, text: &str) -> Result<()> {
        let logical = join_continuations(text);
        // Makefile conditional directives (`ifeq`/`ifneq`/`ifdef`/`ifndef` …
        // `else` … `endif`) evaluated against the variable table as it stands at
        // that point in the file. ODK relies on this:
        // e.g. the `ifeq ($(MIR),true)` mirror rules are gated by the in-file
        // default `MIR = true` (set earlier in the same Makefile), and the
        // `ifeq ($(ODK_DEBUG),yes)` block is inactive because `ODK_DEBUG` is
        // unset. Flattening both branches would wrongly ingest inactive rules
        // (and, "last wins", let them clobber the active ones).
        let mut cond: Vec<CondFrame> = Vec::new();
        let mut i = 0;
        while i < logical.len() {
            let raw = &logical[i];
            i += 1;
            // Recipe lines begin with a tab; they are attached to the previous
            // rule below, so a bare recipe line here is stray — skip.
            if raw.starts_with('\t') {
                continue;
            }
            let line = strip_comment(raw);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Conditional directives are handled first so the branch stack stays
            // balanced even inside a currently-inactive region (nested `ifeq`s).
            if let Some(dir) = parse_conditional(trimmed) {
                if let Directive::If(k) | Directive::ElseIf(k) = &dir {
                    // The value in force HERE is the one that selected the branch
                    // whose rules the plan records, so it is captured here and not
                    // re-read after the parse, when a later assignment may have
                    // moved it.
                    let seen: Vec<(String, String)> = Self::cond_var_names(k)
                        .into_iter()
                        .map(|n| {
                            let v = self.expand(&format!("$({n})")).trim().to_string();
                            (n, v)
                        })
                        .collect();
                    let is_switch = self.tests_a_boolean(k);
                    for (name, value) in seen {
                        if is_switch {
                            self.switch_vars.insert(name.clone());
                        }
                        self.cond_vars.entry(name).or_insert(value);
                    }
                }
                self.apply_conditional(dir, &mut cond);
                continue;
            }
            let active = cond.iter().all(|c| c.active);
            // A `define VAR … endef` multi-line variable. The block body is
            // a *variable value*, not rules — this is essential because ODK's
            // `define data`/help block contains lines like `* test: …`,
            // `* reason_test: …` that would otherwise be parsed as (empty-recipe)
            // rules and, being later in the file, clobber the real
            // `test`/`reason_test`/`odkversion`/`validate_idranges`/… rules
            // ("last wins"). Consume everything through the matching `endef`
            // regardless of `active` (so the body is never mis-parsed as rules);
            // only record the variable when the branch is active.
            if trimmed == "define"
                || trimmed.starts_with("define ")
                || trimmed.starts_with("override define ")
            {
                let header = trimmed
                    .strip_prefix("override ")
                    .unwrap_or(trimmed)
                    .strip_prefix("define")
                    .unwrap_or("")
                    .trim();
                // The variable name is the first token (an optional assignment
                // operator and initial value may follow on the same line).
                let name = header
                    .split([' ', '\t', '=', ':'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let mut body: Vec<String> = Vec::new();
                while i < logical.len() {
                    let l = &logical[i];
                    i += 1;
                    if l.trim() == "endef" {
                        break;
                    }
                    body.push(l.clone());
                }
                if active && !name.is_empty() {
                    // Store as a recursive-style variable (raw text). Not consumed
                    // by owlmake, but recorded so `$(…)` expansion resolves it.
                    self.vars.insert(name, body.join("\n"));
                }
                continue;
            }
            if !active {
                continue;
            }
            // include directives are resolved by the caller (override overlay);
            // ignore here.
            if trimmed.starts_with("include ") || trimmed.starts_with("-include ") {
                continue;
            }
            if let Some((name, op, val)) = parse_assignment(&line) {
                self.apply_assignment(name, op, val);
                continue;
            }
            if let Some(colon) = rule_colon(&line) {
                let targets_s = line[..colon].trim().to_string();
                let rest = line[colon + 1..].trim().to_string();
                // Collect the following recipe lines (tab-prefixed) — THROUGH any
                // make conditional that guards them. ODK puts the whole body of
                // `sparql_test` and `custom_reports` inside an
                // `ifneq ($(SPARQL_VALIDATION_QUERIES),)` that opens on the line
                // directly beneath the target, so a collector that stopped at the
                // first non-tab line would record `steps: []` and drop the
                // recipe's command line on the floor — a plan that silently says
                // the check does nothing.
                //
                // Conditionals met here are the FILE's, not the recipe's: they
                // are evaluated as the file is parsed, and a recipe simply
                // continues across them. So they are applied to the enclosing
                // stack, and a tab line joins the recipe only while every frame
                // is active. A blank line is skipped; anything else ends the
                // recipe.
                //
                // The stack has to be the enclosing one, never a fresh one local
                // to the recipe. The ODK's `custom_reports` body is guarded by
                // `ifneq ($(SPARQL_EXPORTS_ARGS),)`, and once that closes a local
                // stack would go on to swallow the NEXT block's opening `ifeq` as
                // if it were the recipe's own: the enclosing stack would never
                // open that block, its `else` would be ignored as unmatched, and
                // BOTH branches would be ingested. uPheno's
                // `tmp/all_pattern_terms.txt` would then carry the `PAT=false`
                // recipe over the union of both branches' prerequisites — an
                // unbuildable target whose import seed loses every pattern term.
                // The guards are the frames in force at the RULE LINE itself,
                // captured before the collector below walks on: a recipe that
                // ends at its block's `endif` has that frame popped from `cond`
                // during collection, and reading the stack afterwards would
                // record the last rule of every guarded block as unguarded.
                let rule_guards: Vec<String> = {
                    let mut g: Vec<String> = Vec::new();
                    for f in cond.iter().flat_map(|c| c.flags.iter()) {
                        if !g.contains(f) {
                            g.push(f.clone());
                        }
                    }
                    g
                };
                let mut recipe = Vec::new();
                while i < logical.len() {
                    let l = &logical[i];
                    if let Some(stripped) = l.strip_prefix('\t') {
                        if cond.iter().all(|c| c.active) {
                            recipe.push(stripped.to_string());
                        }
                        i += 1;
                        continue;
                    }
                    let lt = strip_comment(l);
                    let lt = lt.trim();
                    if lt.is_empty() {
                        i += 1;
                        continue;
                    }
                    if let Some(dir) = parse_conditional(lt) {
                        if let Directive::If(k) | Directive::ElseIf(k) = &dir {
                            // The value in force HERE is the one that selected the branch
                            // whose rules the plan records, so it is captured here and not
                            // re-read after the parse, when a later assignment may have
                            // moved it.
                            let seen: Vec<(String, String)> = Self::cond_var_names(k)
                                .into_iter()
                                .map(|n| {
                                    let v = self.expand(&format!("$({n})")).trim().to_string();
                                    (n, v)
                                })
                                .collect();
                            let is_switch = self.tests_a_boolean(k);
                            for (name, value) in seen {
                                if is_switch {
                                    self.switch_vars.insert(name.clone());
                                }
                                self.cond_vars.entry(name).or_insert(value);
                            }
                        }
                        self.apply_conditional(dir, &mut cond);
                        i += 1;
                        continue;
                    }
                    break;
                }
                // Expand the target/prereq lists now (they rarely depend on
                // automatic variables).
                let targets: Vec<String> = self
                    .expand(&targets_s)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                // `target: normal… | order-only…` — split on the separator
                // token. `|` is punctuation, never a filename.
                let all: Vec<String> =
                    self.expand(&rest).split_whitespace().map(str::to_string).collect();
                let bar = all.iter().position(|t| t == "|");
                let (prereqs, order_only) = match bar {
                    Some(i) => (all[..i].to_vec(), all[i + 1..].to_vec()),
                    None => (all, Vec::new()),
                };
                let rule = Rule {
                    targets: targets.clone(),
                    prereqs,
                    order_only,
                    recipe,
                    guards: rule_guards,
                };
                // `.PHONY: a b c` declares targets that name no file, so they are
                // always out of date. Recorded so the plan can carry the set and
                // execution can apply the staleness rule to the rest instead of
                // rebuilding the whole release path on every QC run.
                if targets.iter().any(|t| t == ".PHONY") {
                    self.phony.extend(rule.prereqs.iter().cloned());
                }
                if targets.iter().any(|t| t.contains('%')) {
                    self.pattern_rules.push(rule);
                } else {
                    for t in &targets {
                        // The default goal is the first target of the first
                        // explicit rule. A dot-target is skipped only when it
                        // ALSO contains no slash, so `.PHONY` is skipped but
                        // `../patterns/foo.owl` is not.
                        if self.default_goal.is_none()
                            && !(t.starts_with('.') && !t.contains('/'))
                        {
                            self.default_goal = Some(t.clone());
                        }
                        // MERGE, do not replace. Prerequisites accumulate across
                        // every explicit rule for a target, and a single recipe
                        // is kept ("last one wins", with a warning). Every ODK
                        // Makefile ends with `include <ont>.Makefile`, and those
                        // override files extend `test:` with repo checks — OBA's
                        // adds one line, `test: check_children_oba`. Replacing
                        // would let that line DELETE the whole seven-member ODK
                        // QC pipeline from the plan, leaving `om test` to run two
                        // repo greps and report success.
                        match self.rules.entry(t.clone()) {
                            std::collections::hash_map::Entry::Vacant(e) => {
                                e.insert(rule.clone());
                            }
                            std::collections::hash_map::Entry::Occupied(mut e) => {
                                let old = e.get_mut();
                                // Which END the later rule's prerequisites join
                                // depends on whether it also carries a RECIPE.
                                // For `t: a a2` followed by `t: b b2`:
                                //   later rule has a recipe  → `$^ = b b2 a a2`
                                //   later rule has none      → `$^ = a a2 b b2`
                                // An overriding recipe relinks the target and its
                                // own prerequisites lead; a bare prerequisite line
                                // just accumulates in the order the file is read.
                                //
                                // Both halves matter here. Prepending is what makes
                                // `$<` for HPO's `hp.owl` the edit file rather
                                // than the earlier rule's
                                // `hp-simple-non-classified.owl`, so the release
                                // is built from the whole edit file and not from a
                                // reduced subset. And appending is what keeps
                                // `test:` in ODK order, so the profile check
                                // builds `hp.owl` BEFORE `test_obo` writes
                                // `hp.obo`; reversed, `hp.obo` is older than
                                // `hp.owl`, the release rule re-makes it, and the
                                // shipped file is the release conversion instead
                                // of the `test_obo` product the ODK actually
                                // publishes.
                                let overrides = !rule.recipe.is_empty();
                                let join = |mut lead: Vec<String>, rest: Vec<String>| {
                                    for p in rest {
                                        if !lead.contains(&p) {
                                            lead.push(p);
                                        }
                                    }
                                    lead
                                };
                                let (a, b) = (rule.prereqs.clone(), old.prereqs.split_off(0));
                                old.prereqs = if overrides { join(a, b) } else { join(b, a) };
                                let (a, b) = (rule.order_only.clone(), old.order_only.split_off(0));
                                old.order_only = if overrides { join(a, b) } else { join(b, a) };
                                // The guards follow the RECIPE. When a later rule
                                // overrides the recipe from inside a guarded
                                // block, the recipe the plan will run exists only
                                // under that flag — UBERON's base rule for
                                // `../mappings/biomappings.sssom.tsv` is an
                                // unguarded `test -f $@`, and its override file
                                // replaces it with a fetch pipeline inside
                                // `ifeq ($(strip $(MIR)),true)`. A prerequisite-
                                // only line changes no recipe and so no guard.
                                if overrides {
                                    old.guards = rule.guards.clone();
                                }
                                if !rule.recipe.is_empty() {
                                    // Dot-targets (`.PHONY`, `.PRECIOUS`) are
                                    // declarations, repeated freely throughout a
                                    // Makefile; only a real target's recipe being
                                    // replaced is worth reporting.
                                    if !old.recipe.is_empty() && !t.starts_with('.') {
                                        status!(
                                            "make: warning: overriding recipe for target `{t}`"
                                        );
                                    }
                                    old.recipe = rule.recipe.clone();
                                }
                                for tg in &rule.targets {
                                    if !old.targets.contains(tg) {
                                        old.targets.push(tg.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Evaluate a parsed conditional operand against the current variable table.
    fn eval_cond(&self, k: &CondKind) -> bool {
        match k {
            CondKind::Eq(a, b) => self.expand(a).trim() == self.expand(b).trim(),
            CondKind::Ne(a, b) => self.expand(a).trim() != self.expand(b).trim(),
            // `ifdef` is true iff the variable has a non-empty value: a variable
            // set to the empty string counts as "not defined" for this test.
            CondKind::Def(v) => !self.expand(&format!("$({v})")).trim().is_empty(),
            CondKind::Ndef(v) => self.expand(&format!("$({v})")).trim().is_empty(),
        }
    }

    /// Whether a conditional tests its variable against a boolean word, which is
    /// what makes that variable a SWITCH rather than something the repository
    /// happens to have set. `ifeq ($(BRI),true)` is a switch; `ifneq
    /// ($(SPARQL_VALIDATION_QUERIES),)` is a presence test.
    fn tests_a_boolean(&self, k: &CondKind) -> bool {
        let boolean = |s: &str| {
            matches!(
                self.expand(s).trim().to_ascii_lowercase().as_str(),
                "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"
            )
        };
        match k {
            CondKind::Eq(a, b) | CondKind::Ne(a, b) => boolean(a) || boolean(b),
            // `ifdef`/`ifndef` ask whether a variable is set at all, which is a
            // fact about the repository and not a question with two answers.
            CondKind::Def(_) | CondKind::Ndef(_) => false,
        }
    }

    /// The variable names a conditional consults, for [`MakeModel::cond_vars`].
    fn cond_var_names(k: &CondKind) -> Vec<String> {
        let mut out = Vec::new();
        // A plain reference is `$(NAME)` where NAME runs straight to the closing
        // delimiter; anything else after the name is a function call
        // (`$(strip $(MIR))`), whose ARGUMENTS may hold further references — so
        // the scan takes the identifier run after each `$(` and keeps walking,
        // which reaches references at any nesting depth.
        let mut scan = |text: &str| {
            let bytes = text.as_bytes();
            let mut i = 0;
            while i + 1 < bytes.len() {
                if bytes[i] == b'$' && (bytes[i + 1] == b'(' || bytes[i + 1] == b'{') {
                    let start = i + 2;
                    let mut end = start;
                    while end < bytes.len()
                        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                    {
                        end += 1;
                    }
                    if end > start && end < bytes.len() && (bytes[end] == b')' || bytes[end] == b'}')
                    {
                        out.push(text[start..end].to_string());
                    }
                    i = start;
                } else {
                    i += 1;
                }
            }
        };
        match k {
            CondKind::Eq(a, b) | CondKind::Ne(a, b) => {
                scan(a);
                scan(b);
            }
            CondKind::Def(v) | CondKind::Ndef(v) => out.push(v.trim().to_string()),
        }
        out
    }

    /// Apply a conditional directive to the branch stack. A new `if*` is only
    /// evaluated when its enclosing branch is active; `else`/`else if*` only
    /// activate when the enclosing branch is active and no prior branch of this
    /// conditional has been taken.
    fn apply_conditional(&self, dir: Directive, stack: &mut Vec<CondFrame>) {
        // EVERY variable the condition consults, not just the five ODK names: a
        // repository invents its own switches (`BRI` gates UBERON's whole bridge
        // section) and a rule guarded by one is exactly as conditional as a rule
        // guarded by `MIR`. Narrowing this to a fixed list left such a switch with
        // no targets to pin, so the plan could not declare it.
        let guard_names = |k: &CondKind| -> Vec<String> { Self::cond_var_names(k) };
        match dir {
            Directive::If(k) => {
                let parent_active = stack.iter().all(|c| c.active);
                let flags = guard_names(&k);
                let val = parent_active && self.eval_cond(&k);
                // When the parent is inactive, mark `taken` so no nested `else`
                // spuriously activates; the frame stays inactive regardless.
                stack.push(CondFrame { active: val, taken: val || !parent_active, flags });
            }
            Directive::ElseIf(k) => {
                let n = stack.len();
                if n == 0 {
                    return; // malformed `else` without `if`; ignore
                }
                let parent_active = stack[..n - 1].iter().all(|c| c.active);
                let top = &mut stack[n - 1];
                // The branch chain is gated by every flag any of its conditions
                // consults, so the frame's set grows along the chain.
                for f in guard_names(&k) {
                    if !top.flags.contains(&f) {
                        top.flags.push(f);
                    }
                }
                if parent_active && !top.taken {
                    let val = self.eval_cond(&k);
                    top.active = val;
                    top.taken |= val;
                } else {
                    top.active = false;
                }
            }
            Directive::Else => {
                let n = stack.len();
                if n == 0 {
                    return;
                }
                let parent_active = stack[..n - 1].iter().all(|c| c.active);
                let top = &mut stack[n - 1];
                if parent_active && !top.taken {
                    top.active = true;
                    top.taken = true;
                } else {
                    top.active = false;
                }
            }
            Directive::Endif => {
                stack.pop();
            }
        }
    }

    fn apply_assignment(&mut self, name: &str, op: &str, val: &str) {
        // A command-line assignment overrides every in-file one, whatever the
        // operator (`=`, `:=`, `?=`, `+=`).
        if self.command_line_vars.contains(name) {
            return;
        }
        match op {
            "?=" => {
                self.vars.entry(name.to_string()).or_insert_with(|| val.to_string());
            }
            "+=" => {
                let entry = self.vars.entry(name.to_string()).or_default();
                if entry.is_empty() {
                    *entry = val.to_string();
                } else {
                    entry.push(' ');
                    entry.push_str(val);
                }
            }
            ":=" | "::=" => {
                let expanded = self.expand(val);
                self.vars.insert(name.to_string(), expanded);
            }
            _ => {
                // Recursive `=`: store raw, expand on use.
                self.vars.insert(name.to_string(), val.to_string());
            }
        }
    }

    /// Look up the effective rule for a concrete target: explicit first, then
    /// the last matching pattern rule.
    pub fn rule_for<'a>(&'a self, target: &str) -> Option<(&'a Rule, Option<String>)> {
        if let Some(r) = self.rules.get(target) {
            return Some((r, None));
        }
        for r in self.pattern_rules.iter().rev() {
            for t in &r.targets {
                if let Some(stem) = match_pattern(t, target) {
                    return Some((r, Some(stem)));
                }
            }
        }
        None
    }

    /// Expand `$(VAR)`/`${VAR}`, automatic variables, and supported functions.
    pub fn expand(&self, s: &str) -> String {
        self.expand_with(s, &Autos::default())
    }

    pub fn expand_with(&self, s: &str, autos: &Autos) -> String {
        eval_backticks(&self.expand_inner(s, autos, 0), self.base_dir.as_deref(), &self.version_file)
    }

    fn expand_inner(&self, s: &str, autos: &Autos, depth: usize) -> String {
        if depth > 64 || !s.contains('$') {
            return s.to_string();
        }
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                let c = bytes[i + 1];
                if c == b'(' || c == b'{' {
                    let (inner, end) = read_balanced(s, i + 1);
                    out.push_str(&self.eval_ref(&inner, autos, depth));
                    i = end;
                    continue;
                } else if c == b'$' {
                    // `$$` is the escape for a literal `$`, so a single `$`
                    // reaches the shell (e.g. `grep -v '^$$'`, `$$(wc -l …)`).
                    out.push('$');
                    i += 2;
                    continue;
                } else {
                    // Automatic single-char variable.
                    out.push_str(autos.get((c as char).to_string().as_str()).unwrap_or(""));
                    i += 2;
                    continue;
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    /// Evaluate the contents of a `$(...)`: a variable reference, automatic
    /// variable, or function call.
    fn eval_ref(&self, inner: &str, autos: &Autos, depth: usize) -> String {
        // Detect a function call on the RAW text — do *not* pre-expand, because
        // `foreach` binds a loop variable that its body references.
        if let Some((name, args)) = inner.split_once(char::is_whitespace) {
            if is_make_function(name) {
                return self.eval_function(name, args, autos, depth);
            }
            if looks_like_function_call(name) {
                self.unknown_functions.borrow_mut().insert(name.to_string());
            }
        }
        // Variable reference (the name itself may be computed, e.g. `$($(x))`).
        let name = self.expand_inner(inner, autos, depth + 1);
        if let Some(v) = autos.get(&name) {
            return v.to_string();
        }
        match self.vars.get(&name) {
            Some(raw) => self.expand_inner(raw, autos, depth + 1),
            None => String::new(),
        }
    }

    fn eval_function(&self, name: &str, args: &str, autos: &Autos, depth: usize) -> String {
        // `foreach VAR,LIST,BODY`: expand LIST now, BODY once per item with VAR
        // bound (BODY must NOT be pre-expanded).
        if name == "foreach" {
            let a = split_top_commas(args);
            if a.len() == 3 {
                let var = self.expand_inner(a[0], autos, depth + 1).trim().to_string();
                let list = self.expand_inner(a[1], autos, depth + 1);
                let body = a[2];
                return list
                    .split_whitespace()
                    .map(|w| {
                        let mut sub = autos.clone();
                        sub.set(&var, w);
                        self.expand_inner(body, &sub, depth + 1)
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            return String::new();
        }
        // All other functions: expand their arguments first.
        let eargs = self.expand_inner(args, autos, depth + 1);
        let a = split_top_commas(&eargs);
        match name {
            // `sort` also REMOVES DUPLICATES, so `$(sort owl obo json owl)` names
            // three formats rather than four. It takes one list, so commas in it
            // are ordinary characters.
            //
            // The resulting ORDER is load-bearing too: it drives `MAIN_PRODUCTS` →
            // `MAIN_FILES` → the `rsync` argument order in `copy_release_files`, so
            // it is part of P4. And the chain it feeds is wider than uPheno's
            // products — with `sort` expanding to nothing, UBERON's `SUBSET_FILES`
            // was empty too, so `all_subsets` had no prerequisites and its 92
            // subset release artefacts could not be NAMED, let alone built.
            // `$(eval …)` expands to the empty string — its whole effect is the
            // side effect of evaluating its argument as makefile text. So the empty
            // result here is CORRECT, not a missing implementation, and reporting it
            // as unimplemented refused every plan for a repo that uses it: ODK spells
            // its per-target variables `$(eval TERM_ID := …)` in a recipe, which is
            // resolved at ingest by `planner::parse_eval_assignment`.
            //
            // What is NOT implemented is the side effect in a VARIABLE DEFINITION,
            // where the empty value is all there is — and that is caught separately,
            // against `vars`, so this arm cannot hide it.
            "eval" => String::new(),
            "sort" => {
                let mut words: Vec<&str> = eargs.split_whitespace().collect();
                words.sort_unstable();
                words.dedup();
                words.join(" ")
            }
            "subst" => {
                if a.len() == 3 {
                    a[2].replace(a[0].trim(), a[1].trim())
                } else {
                    String::new()
                }
            }
            "patsubst" => {
                if a.len() == 3 {
                    let (pat, repl) = (a[0].trim(), a[1].trim());
                    a[2]
                        .split_whitespace()
                        .map(|w| patsubst_one(pat, repl, w))
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    String::new()
                }
            }
            // The affix keeps its TRAILING whitespace: only the whitespace before
            // a function's first argument is stripped, never inside or after one.
            // So the prefix in `merge $(addprefix -i , $^)` is `-i ` WITH the
            // space, and the expansion is `-i a -i b -i c` — a list of flags and
            // their values. Trimmed it would read `-ia -ib -ic`, which is not
            // `-i <file>`, and such a merge would be planned with none of its
            // inputs.
            "addprefix" => {
                if a.len() == 2 {
                    let pre = a[0].trim_start();
                    a[1].split_whitespace().map(|w| format!("{pre}{w}")).collect::<Vec<_>>().join(" ")
                } else { String::new() }
            }
            "addsuffix" => {
                if a.len() == 2 {
                    let suf = a[0].trim_start();
                    a[1].split_whitespace().map(|w| format!("{w}{suf}")).collect::<Vec<_>>().join(" ")
                } else { String::new() }
            }
            "filter" | "filter-out" => {
                if a.len() == 2 {
                    let pats: Vec<&str> = a[0].split_whitespace().collect();
                    let keep_if_match = name == "filter";
                    a[1].split_whitespace()
                        .filter(|w| pats.iter().any(|p| match_pattern(p, w).is_some()) == keep_if_match)
                        .collect::<Vec<_>>()
                        .join(" ")
                } else { String::new() }
            }
            "strip" => eargs.split_whitespace().collect::<Vec<_>>().join(" "),
            "dir" => eargs.split_whitespace().map(|w| match w.rfind('/') { Some(i) => &w[..=i], None => "./" }).collect::<Vec<_>>().join(" "),
            "notdir" => eargs.split_whitespace().map(|w| match w.rfind('/') { Some(i) => &w[i+1..], None => w }).collect::<Vec<_>>().join(" "),
            "basename" => eargs.split_whitespace().map(|w| match w.rfind('.') { Some(i) => &w[..i], None => w }).collect::<Vec<_>>().join(" "),
            "wildcard" => {
                let mut out = Vec::new();
                for pat in eargs.split_whitespace() {
                    out.extend(glob_simple(pat, self.base_dir.as_deref()));
                }
                out.join(" ")
            }
            // Word selection. HPO's DOSDP seed rule picks its inputs positionally
            // — `$(DOSDPT) terms --infile=$(word 2, $^) --template=$< …` — so the
            // term-file recipe only gets an `--infile=` path at all once `word`
            // expands.
            "word" => {
                let (n, rest) = eargs.split_once(',').unwrap_or((eargs.as_str(), ""));
                match n.trim().parse::<usize>() {
                    // Words are indexed from 1; out of range is empty.
                    Ok(i) if i >= 1 => {
                        rest.split_whitespace().nth(i - 1).unwrap_or("").to_string()
                    }
                    _ => String::new(),
                }
            }
            "wordlist" => {
                let mut it = eargs.splitn(3, ',');
                let s = it.next().unwrap_or("").trim().parse::<usize>().unwrap_or(0);
                let e = it.next().unwrap_or("").trim().parse::<usize>().unwrap_or(0);
                let rest = it.next().unwrap_or("");
                if s == 0 || e < s {
                    String::new()
                } else {
                    rest.split_whitespace()
                        .skip(s - 1)
                        .take(e - s + 1)
                        .collect::<Vec<_>>()
                        .join(" ")
                }
            }
            "words" => eargs.split_whitespace().count().to_string(),
            "firstword" => eargs.split_whitespace().next().unwrap_or("").to_string(),
            "lastword" => eargs.split_whitespace().next_back().unwrap_or("").to_string(),
            "shell" => run_shell(eargs.trim(), self.base_dir.as_deref(), &self.version_file),
            _ => String::new(),
        }
    }
}

/// Automatic variables ($@, $<, $^, $*) and `foreach` loop variables.
#[derive(Debug, Default, Clone)]
pub struct Autos {
    map: HashMap<String, String>,
}

impl Autos {
    pub fn set(&mut self, k: &str, v: &str) {
        self.map.insert(k.to_string(), v.to_string());
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.map.get(k).map(|s| s.as_str())
    }
}

/// Names recognised as Makefile functions (vs. variable references).
fn is_make_function(name: &str) -> bool {
    matches!(
        name,
        "patsubst" | "subst" | "foreach" | "addprefix" | "addsuffix" | "wildcard" | "shell"
            | "dir" | "notdir" | "basename" | "strip" | "filter" | "filter-out"
            | "word" | "wordlist" | "words" | "firstword" | "lastword" | "sort" | "eval"
    )
}

/// Whether a `$(…)` body looks like a call to a function this parser does not
/// implement — a leading bare word followed by a space. Anything holding a `$` in
/// that word is a computed variable name (`$($(x)) …`), not a call.
fn looks_like_function_call(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('$')
        && name.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

fn join_continuations(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cont = false;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if cont {
            // Continuation: collapse leading whitespace to a single space.
            cur.push(' ');
            cur.push_str(line.trim_start());
        } else {
            cur = line.to_string();
        }
        if cur.ends_with('\\') {
            cur.pop();
            cont = true;
        } else {
            out.push(std::mem::take(&mut cur));
            cont = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One frame of the conditional-directive stack: `active` is whether this
/// branch's body is currently in effect; `taken` records that some branch of
/// this `if…else…endif` has already been selected (so later `else`s don't fire).
/// `flags` holds the workflow flags the conditional consults — every branch of
/// the conditional is gated by them, `else` included.
#[derive(Debug, Clone)]
struct CondFrame {
    active: bool,
    taken: bool,
    flags: Vec<String>,
}

/// A parsed conditional directive. Operands are kept raw (unexpanded); the
/// variable table is consulted at evaluation time via `MakeModel::eval_cond`.
#[derive(Debug)]
enum Directive {
    If(CondKind),
    ElseIf(CondKind),
    Else,
    Endif,
}

#[derive(Debug)]
enum CondKind {
    Eq(String, String),
    Ne(String, String),
    Def(String),
    Ndef(String),
}

/// Parse a non-recipe line as a Makefile conditional directive, or `None` if it
/// is not one. Handles `ifeq`/`ifneq` (both `(a,b)` and quoted `"a" "b"` forms),
/// `ifdef`/`ifndef`, `else` (bare and `else if…`), and `endif`.
fn parse_conditional(trimmed: &str) -> Option<Directive> {
    if trimmed == "endif" {
        return Some(Directive::Endif);
    }
    if trimmed == "else" {
        return Some(Directive::Else);
    }
    if let Some(rest) = trimmed.strip_prefix("else ") {
        // `else if…` chains; a bare `else <junk>` degrades to a plain else.
        return Some(parse_cond_kind(rest.trim()).map_or(Directive::Else, Directive::ElseIf));
    }
    parse_cond_kind(trimmed).map(Directive::If)
}

/// Parse the `if*` keyword and operands of a conditional (no leading `else`).
fn parse_cond_kind(s: &str) -> Option<CondKind> {
    // `ifeq`/`ifneq` must be followed by `(` or a quote (after optional space).
    for (kw, eq) in [("ifeq", true), ("ifneq", false)] {
        if let Some(rest) = s.strip_prefix(kw) {
            let rest = rest.trim_start();
            if rest.starts_with('(') || rest.starts_with('"') || rest.starts_with('\'') {
                let (a, b) = parse_eq_operands(rest)?;
                return Some(if eq { CondKind::Eq(a, b) } else { CondKind::Ne(a, b) });
            }
            return None;
        }
    }
    // `ifdef`/`ifndef VAR` — the operand is a bare variable name.
    for (kw, def) in [("ifdef", true), ("ifndef", false)] {
        if let Some(rest) = s.strip_prefix(kw) {
            let rest = rest.trim();
            // Require a real separator so `ifdefx` isn't mistaken for `ifdef x`.
            if rest.is_empty() || s.as_bytes()[kw.len()] != b' ' && s.as_bytes()[kw.len()] != b'\t' {
                return None;
            }
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            if name.is_empty() {
                return None;
            }
            return Some(if def { CondKind::Def(name) } else { CondKind::Ndef(name) });
        }
    }
    None
}

/// Parse the two operands of an `ifeq`/`ifneq`: either `(a,b)` (comma split at
/// the top paren level) or two quoted strings `"a" "b"` / `'a' 'b'`.
fn parse_eq_operands(s: &str) -> Option<(String, String)> {
    if let Some(inner) = s.strip_prefix('(') {
        // Take up to the matching close paren; split on the first top-level comma.
        let mut depth = 0usize;
        let mut comma = None;
        let mut end = None;
        for (idx, c) in inner.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    if depth == 0 {
                        end = Some(idx);
                        break;
                    }
                    depth -= 1;
                }
                ',' if depth == 0 && comma.is_none() => comma = Some(idx),
                _ => {}
            }
        }
        let comma = comma?;
        let end = end?;
        if end < comma {
            return None;
        }
        let a = inner[..comma].trim().to_string();
        let b = inner[comma + 1..end].trim().to_string();
        return Some((a, b));
    }
    // Quoted form: two quote-delimited tokens separated by whitespace.
    let a_q = s.chars().next()?;
    let rest = &s[a_q.len_utf8()..];
    let a_end = rest.find(a_q)?;
    let a = rest[..a_end].to_string();
    let after = rest[a_end + a_q.len_utf8()..].trim_start();
    let b_q = after.chars().next()?;
    if b_q != '"' && b_q != '\'' {
        return None;
    }
    let brest = &after[b_q.len_utf8()..];
    let b_end = brest.find(b_q)?;
    Some((a, brest[..b_end].to_string()))
}

fn strip_comment(line: &str) -> String {
    // A '#' starts a comment unless an ODD number of backslashes precedes it, and
    // recognising that escape CONSUMES one of them: the value of
    // `SL_PREFIXES="PREFIX owl: <http://www.w3.org/2002/07/owl\#>"` holds `owl#`,
    // and a query built from it is what reaches the SPARQL endpoint — `owl\#` is
    // not an IRI.
    //
    // Only non-recipe lines are stripped — every call site passes an assignment,
    // rule or directive line — so a recipe line keeps any '#' it carries and hands
    // it to the command intact.
    let mut out = String::new();
    let mut backslashes = 0usize;
    for c in line.chars() {
        if c == '#' {
            if backslashes % 2 == 0 {
                break;
            }
            // Odd run: this '#' is literal. One backslash pays for the escape; any
            // others stand, so `\\\#` keeps a backslash and the '#'.
            out.pop();
            out.push('#');
            backslashes = 0;
            continue;
        }
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
        out.push(c);
    }
    out
}

fn parse_assignment(line: &str) -> Option<(&str, &str, &str)> {
    let mut trimmed = line.trim_start();
    // An assignment may carry `export`/`override` modifiers, and they may be
    // combined (`export override FOO = bar`). Strip them and parse the
    // assignment underneath, or the whole line is silently ignored: the ODK's
    // `export ROBOT_PLUGINS_DIRECTORY=$(TMPDIR)/plugins` would leave that
    // variable undefined, so `$(ROBOT_PLUGINS_DIRECTORY)/flybase.jar` expands to
    // `/flybase.jar` and the plugin install writes at the filesystem root.
    //
    // A bare `export FOO` (no operator) exports an existing variable and defines
    // nothing; it falls through to `None` below, which is correct.
    loop {
        let rest = trimmed
            .strip_prefix("export")
            .or_else(|| trimmed.strip_prefix("override"));
        match rest {
            // Require whitespace after the keyword so `exportFOO=1` and
            // `override_x = 1` stay ordinary variable names.
            Some(r) if r.starts_with(char::is_whitespace) => trimmed = r.trim_start(),
            _ => break,
        }
    }
    // Find the assignment operator before any ':' that would make it a rule.
    // Operators: ::=, :=, ?=, +=, =
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    // variable name
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' {
            i += 1;
        } else {
            break;
        }
    }
    if i == 0 {
        return None;
    }
    let name = &trimmed[..i];
    let after = trimmed[i..].trim_start();
    let off = trimmed.len() - after.len();
    for op in ["::=", ":=", "?=", "+=", "="] {
        if after.starts_with(op) {
            // Reject `:=` confusion with rules: assignment ops here are explicit.
            let val = after[op.len()..].trim();
            // The name scan above stops at the first character a variable name
            // cannot contain, so a second word (`FOO BAR = baz`) leaves `after`
            // starting at `BAR`, which matches no operator and is therefore not
            // an assignment.
            let _ = off;
            return Some((name, op, val));
        }
    }
    None
}

/// Index of the rule-separating ':' (not part of `:=` and not a Windows drive).
fn rule_colon(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b':' if depth == 0 => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    return None; // := assignment
                }
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Read a balanced `(...)`/`{...}` starting at `open` (index of the bracket).
/// Returns (inner, index just past the close bracket).
fn read_balanced(s: &str, open: usize) -> (String, usize) {
    let bytes = s.as_bytes();
    let (oc, cc) = if bytes[open] == b'(' { (b'(', b')') } else { (b'{', b'}') };
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        if bytes[i] == oc {
            depth += 1;
        } else if bytes[i] == cc {
            depth -= 1;
            if depth == 0 {
                return (s[open + 1..i].to_string(), i + 1);
            }
        }
        i += 1;
    }
    (s[open + 1..].to_string(), bytes.len())
}

/// Split on top-level commas (commas not inside nested `$(...)`).
fn split_top_commas(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

fn patsubst_one(pat: &str, repl: &str, word: &str) -> String {
    if let Some(stem) = match_pattern(pat, word) {
        repl.replace('%', &stem)
    } else {
        word.to_string()
    }
}

/// Match a `%` pattern against a word, returning the stem if it matches.
pub(crate) fn match_pattern(pat: &str, word: &str) -> Option<String> {
    match pat.split_once('%') {
        Some((pre, suf)) => {
            if word.len() >= pre.len() + suf.len()
                && word.starts_with(pre)
                && word.ends_with(suf)
            {
                Some(word[pre.len()..word.len() - suf.len()].to_string())
            } else {
                None
            }
        }
        None => (pat == word).then(|| String::new()),
    }
}

fn glob_simple(pat: &str, base_dir: Option<&Path>) -> Vec<String> {
    // Only handle `dir/*.ext` and exact paths; good enough for plan purposes.
    //
    // A pattern is relative to the directory the Makefile runs in, not to the
    // process cwd — `om` is pointed at a repo with `-C` and may be launched from
    // anywhere, so `$(wildcard ../patterns/data/default/*.tsv)` has to name the
    // same files either way. Resolving against the cwd instead silently expands
    // to nothing from a repo root, and a plan generated there would drop every
    // DOSDP pattern while still claiming to be the build.
    let at = |p: &str| match base_dir {
        Some(d) => d.join(p),
        None => PathBuf::from(p),
    };
    if !pat.contains('*') {
        return if at(pat).exists() { vec![pat.to_string()] } else { vec![] };
    }
    let (dir, file) = match pat.rfind('/') {
        Some(i) => (&pat[..i], &pat[i + 1..]),
        None => (".", pat),
    };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(at(dir)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if match_pattern(&file.replace('*', "%"), &name).is_some() {
                out.push(format!("{dir}/{name}"));
            }
        }
    }
    out.sort();
    out
}

/// Evaluate backtick command substitutions (`` `cmd` ``) in an expanded recipe
/// string. A recipe line decomposed into owlmake's in-memory pipeline never
/// passes through a shell, so the substitution has to happen during expansion —
/// otherwise an `annotate -V .../`date +%Y-%m-%d`/…` version IRI would carry a
/// literal backtick. Evaluating here keeps the in-memory and shell-replay paths
/// consistent (idempotent for the shell-replay path, which would just re-run an
/// already-substituted line).
fn eval_backticks(
    s: &str,
    base_dir: Option<&Path>,
    version_file: &std::cell::RefCell<Option<String>>,
) -> String {
    if !s.contains('`') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('`') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('`') {
            Some(close) => {
                out.push_str(&run_shell(&after[..close], base_dir, version_file));
                rest = &after[close + 1..];
            }
            None => {
                // Unbalanced backtick: emit the remainder verbatim.
                out.push('`');
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn run_shell(
    cmd: &str,
    base_dir: Option<&Path>,
    version_file: &std::cell::RefCell<Option<String>>,
) -> String {
    // A command that reads the calendar date is a run input, not a value to
    // freeze: it resolves to [`VERSION_CLOCK`], which the run binds to the day
    // it builds on — the shell's answer, which a `TODAY=` assignment does not
    // reach. uPheno's `../patterns/pattern-merged.owl` stamps
    // `annotate -V $(ONTBASE)/releases/`date +%Y-%m-%d`/…`, and running the
    // command here wrote the planning day's date into the plan, so every later
    // build published that same version IRI.
    //
    // [`VERSION_CLOCK`]: crate::plan::VERSION_CLOCK
    if is_today_command(cmd) {
        return crate::plan::VERSION_CLOCK.to_string();
    }
    // A command that reads the release version out of a file is a run input for
    // the same reason: the file is repo content a curator edits for each release,
    // so its CONTENTS are data the run reads, not a value to freeze. EFO stamps
    // `` v`cat version.txt` `` into four version IRIs and two `owl:versionInfo`
    // annotations; running the command here wrote 3.92.0 into all six, and the
    // file was named by nothing, so bumping it to 3.93.0 neither changed the
    // artefacts nor made them out of date.
    if let Some(file) = version_file_command(cmd) {
        *version_file.borrow_mut() = Some(file.to_string());
        return crate::plan::VERSION_REF.to_string();
    }
    // A `$(shell …)` expansion may itself call `jq`/`sssom`; substitute the
    // bundled tools named directly in it by explicit binary path, and put the
    // shim dir on PATH for any nested script call — exactly as the recipe
    // interpreter does for recipe lines.
    let exe = crate::build::recipe::owlmake_exe();
    let rewritten = crate::build::recipe::rewrite_tools(cmd, &exe, "");
    let mut sh = std::process::Command::new("sh");
    sh.arg("-c").arg(&rewritten);
    // Run from the Makefile's directory: relative paths in the substitution
    // (`cat version.txt`, `ls imports/*.owl`) are written against that
    // directory, so they must resolve against it and not against the process
    // cwd `om` was launched from.
    if let Some(dir) = base_dir {
        sh.current_dir(dir);
    }
    crate::build::recipe::prepend_tool_path(&mut sh, &exe);
    sh.output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().replace('\n', " "))
        .unwrap_or_default()
}

/// Whether `cmd` is `date` printing the day as `YYYY-MM-DD`, in any of the
/// quotings a Makefile writes it in.
///
/// Only the bare day: a command that also prints the time — ODK's
/// `date +'%d:%m:%Y %H:%M'` — is not a release version, and a plan that
/// referred to it would resolve to a different string on every run.
/// The file `cmd` reads the release version out of, if reading that file is all
/// it does — `cat version.txt`, and the `tr`/`echo` dressings that mean the same.
///
/// Only a bare read qualifies. A substitution that computes something from a file
/// is not a reference to the file's contents, and freezing its result is right.
fn version_file_command(cmd: &str) -> Option<&str> {
    let mut words = cmd.split_whitespace();
    if words.next() != Some("cat") {
        return None;
    }
    let path = words.next()?;
    // `cat a b` concatenates two files; that is not a version reference.
    words.next().is_none().then_some(path.trim_matches(['\'', '"']))
}

fn is_today_command(cmd: &str) -> bool {
    let mut words = cmd.split_whitespace();
    if words.next() != Some("date") {
        return false;
    }
    let Some(fmt) = words.next() else { return false };
    words.next().is_none() && fmt.trim_matches(['\'', '"']) == "+%Y-%m-%d"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guarded_recipe_does_not_swallow_the_next_conditional() {
        // ODK guards `custom_reports`' body with `ifneq ($(SPARQL_EXPORTS_ARGS),)`.
        // Once that closes, the block that FOLLOWS belongs to the file, and its
        // `else` branch must stay out of the plan.
        let mut m = MakeModel::default();
        m.vars.insert("PAT".into(), "true".into());
        m.ingest(concat!(
            "PAT = true\n",
            "EXPORTS =\n",
            "custom_reports: edit.owl | reports\n",
            "ifneq ($(EXPORTS),)\n",
            "\trobot query $(EXPORTS)\n",
            "endif\n",
            "\n",
            "ifeq ($(PAT),true)\n",
            "tmp/x.txt: a b c\n",
            "\techo true > $@\n",
            "\n",
            "else # PAT=false\n",
            "tmp/x.txt: d\n",
            "\techo false > $@\n",
            "\n",
            "endif\n",
        ))
        .unwrap();
        let r = m.rules.get("tmp/x.txt").expect("the active branch's rule");
        assert_eq!(r.prereqs, vec!["a", "b", "c"]);
        assert_eq!(r.recipe, vec!["echo true > $@"]);
    }

    #[test]
    fn an_inactive_branchs_rules_are_not_ingested() {
        let mut m = MakeModel::default();
        m.ingest(concat!(
            "X = no\n",
            "ifeq ($(X),yes)\n",
            "only-if-yes: a\n",
            "\techo yes\n",
            "else\n",
            "only-if-no: b\n",
            "\techo no\n",
            "endif\n",
            "after: c\n",
            "\techo after\n",
        ))
        .unwrap();
        assert!(!m.rules.contains_key("only-if-yes"));
        assert_eq!(m.rules.get("only-if-no").unwrap().prereqs, vec!["b"]);
        assert_eq!(m.rules.get("after").unwrap().recipe, vec!["echo after"]);
    }

    #[test]
    fn dollar_dollar_is_literal_dollar() {
        let m = MakeModel::default();
        // `$$` → a single literal `$` for the shell.
        assert_eq!(m.expand("grep -v '^$$'"), "grep -v '^$'");
        assert_eq!(m.expand("echo $$(wc -l < f)"), "echo $(wc -l < f)");
    }

    /// The ODK template's release-asset chain is built entirely out of `$(sort)`.
    /// Without it every one of these expanded to the empty string, so UBERON's
    /// `all_main` and `all_subsets` had no prerequisites and its 92 `SUBSET_FILES`
    /// could not be named. `$(sort)` both orders AND de-duplicates.
    #[test]
    fn sort_orders_and_deduplicates() {
        let m = MakeModel::default();
        assert_eq!(m.expand("$(sort c a b a)"), "a b c");
        assert_eq!(m.expand("$(sort b a)"), "a b");
    }

    /// `$(eval …)` expands to the empty string — that IS GNU make's semantics, not
    /// a missing implementation, so it must not be reported as an unimplemented
    /// function. ODK writes `$(eval VAR := …)` as a recipe line, which ingest
    /// resolves separately; reporting it here refused every UBERON plan outright.
    #[test]
    fn eval_expands_empty_and_is_not_an_unknown_function() {
        let mut m = MakeModel::default();
        assert_eq!(m.expand("$(eval X := 1)"), "");
        assert!(
            m.unknown_functions.borrow().is_empty(),
            "`eval` was reported unimplemented: {:?}",
            m.unknown_functions.borrow()
        );
        // A genuinely unknown function still is.
        assert_eq!(m.expand("$(notafunction a b)"), "");
        assert!(m.unknown_functions.borrow().contains("notafunction"));
    }

    #[test]
    fn sort_orders_and_dedups() {
        let mut m = MakeModel::default();
        assert_eq!(m.expand("$(sort  owl obo json owl)"), "json obo owl");
        assert_eq!(m.expand("$(sort b a c a)"), "a b c");
        assert_eq!(m.expand("$(sort)"), "");

        // The ODK chain itself: FORMATS → FORMATS_INCL_TSV → SUBSET_FILES.
        let mut m = MakeModel::default();
        m.ingest(concat!(
            "SUBSETDIR = subsets\n",
            "FORMATS = $(sort  owl obo json owl)\n",
            "FORMATS_INCL_TSV = $(sort $(FORMATS) tsv)\n",
            "SUBSETS = human-view amniote-view\n",
            "SUBSET_ROOTS = $(patsubst %, $(SUBSETDIR)/%, $(SUBSETS))\n",
            "SUBSET_FILES = $(foreach n,$(SUBSET_ROOTS), $(foreach f,$(FORMATS_INCL_TSV), $(n).$(f)))\n",
        ))
        .unwrap();
        assert_eq!(m.expand("$(FORMATS_INCL_TSV)"), "json obo owl tsv");
        assert_eq!(
            m.expand("$(SUBSET_FILES)").split_whitespace().collect::<Vec<_>>(),
            vec![
                "subsets/human-view.json",
                "subsets/human-view.obo",
                "subsets/human-view.owl",
                "subsets/human-view.tsv",
                "subsets/amniote-view.json",
                "subsets/amniote-view.obo",
                "subsets/amniote-view.owl",
                "subsets/amniote-view.tsv",
            ]
        );
    }

    #[test]
    fn auto_vars_caret_and_first() {
        let mut autos = Autos::default();
        autos.set("@", "imports/mondo_terms.txt");
        autos.set("<", "iri_dependencies/mondo_terms.txt");
        autos.set("^", "iri_dependencies/mondo_terms.txt iri_dependencies/efo-relations.txt");
        let m = MakeModel::default();
        assert_eq!(
            m.expand_with("cat $^ > $@", &autos),
            "cat iri_dependencies/mondo_terms.txt iri_dependencies/efo-relations.txt > imports/mondo_terms.txt"
        );
    }

    /// A backtick substitution that is EVALUATED must resolve its relative files
    /// against the Makefile's directory, not the process cwd `om` was launched
    /// from. Without `base_dir` the `cat` finds no file and the substitution
    /// collapses to the empty string.
    ///
    /// Two files, so this stays an evaluated substitution: a bare one-file read
    /// is a version reference and resolves without running anything
    /// (`a_version_read_from_a_file_is_a_reference_not_a_value`).
    #[test]
    fn backticks_resolve_relative_to_base_dir() {
        let dir = std::env::temp_dir().join(format!("owlmake_mk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("version.txt"), "3.90.0\n").unwrap();
        std::fs::write(dir.join("suffix.txt"), "rc1\n").unwrap();

        let mut m = MakeModel::default();
        m.base_dir = Some(dir.clone());
        assert_eq!(
            m.expand("http://www.ebi.ac.uk/efo/releases/v`cat version.txt suffix.txt`/efo.owl"),
            "http://www.ebi.ac.uk/efo/releases/v3.90.0 rc1/efo.owl"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A backtick that reads the version out of a FILE resolves to a reference to
    /// that file, not to the version it happens to hold at plan time. EFO's
    /// `` v`cat version.txt` `` reached six release strings; freezing it there
    /// meant a curator could bump `version.txt` to 3.93.0 and get 3.92.0
    /// artefacts, with nothing naming the file to make them out of date.
    #[test]
    fn a_version_read_from_a_file_is_a_reference_not_a_value() {
        let dir = std::env::temp_dir().join(format!("owlmake_vf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("version.txt"), "3.92.0\n").unwrap();

        let mut m = MakeModel::default();
        m.base_dir = Some(dir.clone());
        assert_eq!(
            m.expand("http://www.ebi.ac.uk/efo/releases/v`cat version.txt`/efo.owl"),
            format!("http://www.ebi.ac.uk/efo/releases/v{}/efo.owl", crate::plan::VERSION_REF)
        );
        // …and the file is named, so the plan can carry it and the run re-read it.
        assert_eq!(m.version_file.borrow().as_deref(), Some("version.txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A robot line that opens over `--input-iri` names a remote input, and the
    /// plan carries it: the local `--input` scan skips anything beginning `http`,
    /// so an IRI is the separate answer. A rule with no prerequisites has no
    /// other input, and a query over the previous release is exactly that shape.
    #[test]
    fn a_remote_input_is_an_input() {
        let iri = "https://github.com/EBISPOT/efo/releases/download/current/efo.owl";
        let line = format!("robot query --input-iri {iri} --select q.sparql out.tmp");
        assert_eq!(super::super::planner::first_robot_iri_input(&line, "robot"), Some(iri.into()));
        assert_eq!(
            super::super::planner::first_robot_iri_input(
                &format!("robot query --input-iri={iri} --select q.sparql out.tmp"),
                "robot"
            ),
            Some(iri.into())
        );
        // A local input is not an IRI input, and vice versa: the two answers are
        // used differently, one as `$<` and one as a pipeline boundary.
        assert_eq!(
            super::super::planner::first_robot_iri_input("robot query -i build/efo.owl", "robot"),
            None
        );
        assert_eq!(super::super::planner::first_robot_input(&line, "robot"), None);
    }

    /// `mint` takes its ID policy from the single `*-idranges.owl` beside the
    /// edit file, and the PLAN has to name it: when it did not, execution globbed
    /// for one and globbed the wrong directory, so EFO's `allocate-definitive-ids`
    /// died with "no *-idranges.owl file found in .". Two candidates is no answer
    /// — minting from the wrong ID policy is worse than not minting.
    #[test]
    fn idranges_is_resolved_when_there_is_exactly_one() {
        let dir = std::env::temp_dir().join(format!("owlmake_ir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(super::super::planner::idranges_beside_edit_file(&dir), None);

        std::fs::write(dir.join("efo-idranges.owl"), "").unwrap();
        assert_eq!(
            super::super::planner::idranges_beside_edit_file(&dir).as_deref(),
            Some("efo-idranges.owl")
        );

        std::fs::write(dir.join("cl-idranges.owl"), "").unwrap();
        assert_eq!(super::super::planner::idranges_beside_edit_file(&dir), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only a BARE read is a version reference. `cat a b` concatenates two files
    /// and `wc -l < f` computes from one; neither is "the version lives here", so
    /// both keep running at plan time.
    #[test]
    fn a_computed_substitution_is_still_a_value() {
        assert_eq!(version_file_command("cat version.txt"), Some("version.txt"));
        assert_eq!(version_file_command("cat 'version.txt'"), Some("version.txt"));
        assert_eq!(version_file_command("cat a.txt b.txt"), None);
        assert_eq!(version_file_command("wc -l < version.txt"), None);
        assert_eq!(version_file_command("date +%Y-%m-%d"), None);
    }

    /// `$(wildcard …)` resolves against the Makefile's directory too. CL's DOSDP
    /// file lists are `$(wildcard $(PATTERNDIR)/data/default/*.tsv)` with
    /// `PATTERNDIR=../patterns`, so a plan generated from anywhere but
    /// `src/ontology` would expand them to nothing and describe a build with no
    /// patterns in it.
    #[test]
    fn wildcard_resolves_relative_to_base_dir() {
        let dir = std::env::temp_dir().join(format!("owlmake_wc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/ontology")).unwrap();
        std::fs::create_dir_all(dir.join("src/patterns/data/default")).unwrap();
        std::fs::write(dir.join("src/patterns/data/default/a.tsv"), "").unwrap();
        std::fs::write(dir.join("src/patterns/data/default/b.tsv"), "").unwrap();

        let mut m = MakeModel::default();
        m.base_dir = Some(dir.join("src/ontology"));
        assert_eq!(
            m.expand("$(wildcard ../patterns/data/default/*.tsv)"),
            "../patterns/data/default/a.tsv ../patterns/data/default/b.tsv"
        );
        assert_eq!(m.expand("$(wildcard ../patterns/data/default/a.tsv)"), "../patterns/data/default/a.tsv");
        assert_eq!(m.expand("$(wildcard ../patterns/data/default/zz.tsv)"), "");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An assignment carrying `export`/`override` modifiers must still define the
    /// variable. The ODK exports its plugin directory this way
    /// (`export ROBOT_PLUGINS_DIRECTORY=$(TMPDIR)/plugins`); ignore the line and
    /// it stays empty, so `$(ROBOT_PLUGINS_DIRECTORY)/flybase.jar` resolves to
    /// `/flybase.jar` at the filesystem root and the plugin install has nowhere
    /// writable to go. A bare `export FOO` defines nothing, and a variable whose
    /// own name merely starts with `export`/`override` is untouched.
    #[test]
    fn export_and_override_modifiers_still_assign() {
        let mut m = MakeModel::default();
        m.ingest(
            "TMPDIR = tmp\n\
             export ROBOT_PLUGINS_DIRECTORY=$(TMPDIR)/plugins\n\
             override FOO := bar\n\
             export override BAZ := qux\n\
             exported = kept\n\
             overridden = kept-too\n\
             export ALREADY\n",
        )
        .unwrap();
        assert_eq!(m.expand("$(ROBOT_PLUGINS_DIRECTORY)/flybase.jar"), "tmp/plugins/flybase.jar");
        assert_eq!(m.expand("$(FOO)"), "bar");
        assert_eq!(m.expand("$(BAZ)"), "qux");
        assert_eq!(m.expand("$(exported)"), "kept");
        assert_eq!(m.expand("$(overridden)"), "kept-too");
        assert_eq!(m.expand("$(ALREADY)"), "");
    }

    /// `target: normal… | order-only…` — the `|` is a separator, never a file.
    /// Left in `prereqs` it would be the rule's first prerequisite, so `$<` (the
    /// pipeline input) would be the literal string `|` — CL's
    /// `component-download-%.owl: | $(TMPDIR)` offers nothing else there, and the
    /// step has no resolvable input. Order-only prerequisites are still
    /// dependencies, so they are kept — just separately.
    #[test]
    fn order_only_prereqs_are_split_out() {
        let mut m = MakeModel::default();
        m.ingest("out.owl: in.owl extra.owl | tmp plugins\n\techo hi\n").unwrap();
        let (rule, _) = m.rule_for("out.owl").unwrap();
        assert_eq!(rule.prereqs, vec!["in.owl", "extra.owl"]);
        assert_eq!(rule.order_only, vec!["tmp", "plugins"]);
    }
}

#[cfg(test)]
mod oba_shape_tests {
    use super::*;

    /// Prerequisites ACCUMULATE across every explicit rule for a target. Every
    /// ODK Makefile ends with `include <ont>.Makefile`, and those override files
    /// extend `test:` — OBA's adds `check_children_oba`. Replacing instead of
    /// merging would delete the ODK QC pipeline from the plan.
    #[test]
    fn an_included_rule_extends_rather_than_replaces() {
        let dir = std::env::temp_dir().join(format!("om_mk_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("Makefile");
        std::fs::write(
            &main,
            "test: odkversion reason_test sparql_test\n\techo done\n\n.PHONY: test\n",
        )
        .unwrap();
        let ovr = dir.join("ont.Makefile");
        std::fs::write(&ovr, "test: check_children\n").unwrap();

        let mut m = MakeModel::parse_file(&main).unwrap();
        m.overlay_file(&ovr).unwrap();
        let r = m.rules.get("test").expect("test rule");
        for want in ["odkversion", "reason_test", "sparql_test", "check_children"] {
            assert!(r.prereqs.iter().any(|p| p == want), "lost `{want}`: {:?}", r.prereqs);
        }
        assert_eq!(r.recipe, vec!["echo done".to_string()], "the base recipe must survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ODK puts the body of `sparql_test` and `custom_reports` inside an
    /// `ifneq (…)` that opens on the line directly beneath the target. A recipe
    /// collector that stops at the first non-tab line records `steps: []` and the
    /// recipe's only command line is silently dropped.
    #[test]
    fn a_recipe_guarded_by_a_conditional_is_collected() {
        let dir = std::env::temp_dir().join(format!("om_mk2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("Makefile");
        std::fs::write(
            &main,
            "QUERIES = a.sparql b.sparql\n\
             sparql_test: src.owl\n\
             ifneq ($(QUERIES),)\n  \n\trobot verify -i src.owl --queries $(QUERIES)\n\
             endif\n",
        )
        .unwrap();
        let m = MakeModel::parse_file(&main).unwrap();
        let r = m.rules.get("sparql_test").expect("sparql_test rule");
        assert_eq!(
            r.recipe,
            vec!["robot verify -i src.owl --queries $(QUERIES)".to_string()],
            "the guarded recipe line was dropped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `sort` orders AND de-duplicates, and a format list depends on both:
    /// `$(sort owl obo json owl)` names `owl` twice and must yield three formats.
    /// A nested `foreach` over that list is how a release artefact set is built,
    /// so an empty result there names no artefact at all.
    #[test]
    fn sort_orders_and_deduplicates_so_the_release_products_expand() {
        let dir = std::env::temp_dir().join(format!("om_mk3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("Makefile");
        std::fs::write(
            &main,
            "FORMATS = $(sort  owl obo json owl)\n\
             PRODUCTS = $(sort b a)\n\
             MAIN_FILES = $(foreach n,$(PRODUCTS), $(foreach f,$(FORMATS), $(n).$(f)))\n",
        )
        .unwrap();
        let m = MakeModel::parse_file(&main).unwrap();
        assert_eq!(m.expand("$(FORMATS)"), "json obo owl");
        assert_eq!(
            m.expand("$(MAIN_FILES)").split_whitespace().collect::<Vec<_>>(),
            vec!["a.json", "a.obo", "a.owl", "b.json", "b.obo", "b.owl"],
        );
        assert!(m.unknown_functions.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `$(…)` holding a space is a function call; if the parser does not know
    /// the name it expands to nothing, which is indistinguishable from an unset
    /// variable. Record it so ingest can refuse rather than plan a build that
    /// quietly computes less.
    #[test]
    fn an_unimplemented_function_is_recorded_rather_than_silently_empty() {
        let dir = std::env::temp_dir().join(format!("om_mk4_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("Makefile");
        std::fs::write(&main, "X = $(guess a,b)\n").unwrap();
        let m = MakeModel::parse_file(&main).unwrap();
        assert_eq!(m.expand("$(X)"), "");
        assert!(m.unknown_functions.borrow().contains("guess"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only the whitespace BEFORE a function's first argument is stripped, so an
    /// affix keeps its trailing space. `merge $(addprefix -i , $^)` depends on it:
    /// the prefix is `-i `, and the expansion is a list of flags and their values.
    /// Trimmed, it would read `-ia -ib`, which is not `-i <file>`.
    #[test]
    fn addprefix_keeps_the_trailing_space_that_separates_a_flag_from_its_value() {
        let dir = std::env::temp_dir().join(format!("om_mk5_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("Makefile");
        std::fs::write(&main, "SRCS = a.owl b.owl\nARGS = $(addprefix -i , $(SRCS))\nSUF = $(addsuffix .owl, a b)\n").unwrap();
        let m = MakeModel::parse_file(&main).unwrap();
        assert_eq!(m.expand("$(ARGS)"), "-i a.owl -i b.owl");
        assert_eq!(m.expand("$(SUF)"), "a.owl b.owl");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
