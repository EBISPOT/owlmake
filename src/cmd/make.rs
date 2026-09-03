//! `make` — resolve an ontology repo's plan (a committed `owlmake.yaml`, or one
//! regenerated from the repo's build configuration by ingest) and run the
//! requested targets, without modifying the repo. Build targets are given
//! positionally, the repo directory with `-C`.
//!
//! This module also backs the curated build commands surfaced at the top level
//! (`prepare-release`, `refresh-imports`, `all-imports`, `test`): they are thin
//! wrappers over the same routing, so `owlmake refresh-imports` and
//! `owlmake make refresh-imports` behave identically.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;

use crate::model::Model;
use crate::build::{self, ExecOpts, ImportsMode, PatternsMode};
use crate::odk::{OdkRepo, OwlmakeSpec};
use crate::plan::Plan;
use crate::spec;

#[derive(ClapArgs)]
pub struct Args {
    /// Targets to build: e.g. `owlmake oba.owl oba-base.owl oba.obo`. As well as
    /// release artefacts (matched by filename `oba.owl`, artefact name `base`, or
    /// export format `obo`), the standard build targets are accepted here too
    /// (`refresh-imports`, `prepare-release`, `test`, …), as is any other target
    /// the repository defines. With no targets, every artefact configured in the
    /// repo is built.
    #[arg(value_name = "TARGET")]
    pub targets: Vec<String>,
    /// `-B`/`--always-make`: run every target's recipe even when its output is
    /// newer than its prerequisites. Execution applies the up-to-date test to
    /// recipe targets, so without this there is no way to force a rebuild
    /// (MONDO's CI passes `-B`).
    #[arg(short = 'B', long = "always-make")]
    pub always_make: bool,

    /// `-k`/`--keep-going`: when a target fails, carry on with the targets that
    /// do not depend on it, then exit non-zero. A release with one unbuildable
    /// artefact still produces the rest.
    #[arg(short = 'k', long = "keep-going")]
    pub keep_going: bool,

    /// `-W`/`--assume-new`: pretend the named file was just modified. Targets
    /// that depend on it run their recipes; the file itself is neither rebuilt
    /// nor touched. This is how `recreate-components` forces the component
    /// recipes over their stamps.
    #[arg(short = 'W', long = "assume-new", value_name = "FILE")]
    pub assume_new: Vec<String>,

    /// Directory of the ontology repo to build (its root, the `src/ontology`
    /// directory, or the `<id>-odk.yaml` file). Defaults to the current
    /// directory.
    #[arg(short = 'C', long = "directory", visible_alias = "repo", value_name = "DIR", default_value = ".")]
    pub repo: PathBuf,

    /// Rebuild these target groups even where their outputs are present, e.g.
    /// `--rebuild imports --rebuild mirrors`.
    ///
    /// The groups a repo exposes, and the targets each covers, are recorded in
    /// the plan (`refresh_groups`) and listed by `--plan-only`. ODK's
    /// `IMP=`/`MIR=`/`PAT=` spellings are accepted as positional tokens too.
    #[arg(long = "rebuild", value_name = "GROUP", value_delimiter = ',')]
    pub rebuild: Vec<String>,

    /// Reuse these groups' existing outputs instead of rebuilding them.
    #[arg(long = "keep", value_name = "GROUP", value_delimiter = ',')]
    pub keep: Vec<String>,

    /// Print every target this repo can build, one per line, and exit.
    ///
    /// The only stable machine-readable view of the runnable target surface:
    /// `--plan-only` prints the plan's Display, which never enumerates
    /// `prerequisites` by name, so the recipe targets — the whole point of
    /// recording them — were invisible. `tests/plan_only.rs` compares this
    /// between a repo with its Makefile and the same repo without one.
    #[arg(long)]
    pub list_targets: bool,

    /// Only print the plan; do not build anything. (Still writes the plan file
    /// when it is being generated, so the plan can be checked in.)
    #[arg(long)]
    pub plan_only: bool,

    /// Serialization of the generated plan: `yaml` (default, `owlmake.yaml`) or
    /// `json` (`owlmake.json`). Both are accepted when building; if a repo
    /// commits both they must describe the same build.
    #[arg(long, default_value = "yaml", value_name = "FORMAT")]
    pub plan_format: String,

    /// Rewrite the committed plan from the repository's build configuration.
    ///
    /// Without this a build never writes over a plan that is already committed:
    /// it checks that the plan still matches the build configuration and fails
    /// if it does not, because a stale plan is the whole build as soon as the
    /// Makefile is deleted. This is how you update it after changing the build.
    #[arg(long)]
    pub regenerate: bool,

    /// How to obtain import modules: `cached` (reuse committed mirror/import
    /// files; default) or `fresh` (rebuild them).
    ///
    /// `--imports fresh` is the older spelling of `--rebuild imports`, and means
    /// the same thing: rebuild the `imports` group from the plan's own recorded
    /// pipelines. It no longer selects a different import BUILDER.
    ///
    /// With neither spelling given the answer comes from the PLAN, which records
    /// whether an ordinary build of this repo rebuilds its import modules
    /// (`refresh_groups`, set by ingest from the repo's own `IMP` default).
    #[arg(long)]
    pub imports: Option<String>,

    /// How to obtain `patterns/definitions.owl`: `regenerate` (rebuild from the
    /// DOSDP patterns with owlmake's dosdp engine, `PAT=true`; default) or
    /// `cached` (reuse the committed file, `PAT=false`).
    #[arg(long, default_value = "regenerate")]
    pub patterns: String,

    /// Output directory for built artefacts (default: the ontology directory).
    #[arg(short, long)]
    pub output_dir: Option<PathBuf>,

    /// Restrict to specific release artefacts (repeatable). The flag form of the
    /// positional targets above — matched by artefact name (e.g. `full`), target
    /// filename (e.g. `oba.owl`), or export format (e.g. `obo`); combined with any
    /// positional targets.
    #[arg(short = 'a', long = "artefact")]
    pub artefacts: Vec<String>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Shared options for the curated build commands that produce artefacts
/// (`prepare-release`).
#[derive(ClapArgs)]
pub struct TargetArgs {
    #[arg(short = 'C', long = "directory", visible_alias = "repo", value_name = "DIR", default_value = ".")]
    pub repo: PathBuf,
    /// Output directory for built artefacts (default: the ontology directory).
    #[arg(short, long)]
    pub output_dir: Option<PathBuf>,
    /// How to obtain import modules: `cached` (default) or `fresh`.
    #[arg(long, default_value = "cached")]
    pub imports: String,
    /// How to obtain `patterns/definitions.owl`: `regenerate` (`PAT=true`;
    /// default) or `cached` (`PAT=false`).
    #[arg(long, default_value = "regenerate")]
    pub patterns: String,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Options for `refresh-imports`.
#[derive(ClapArgs)]
pub struct RefreshArgs {
    #[arg(short = 'C', long = "directory", visible_alias = "repo", value_name = "DIR", default_value = ".")]
    pub repo: PathBuf,
    /// Skip imports flagged `is_large_import` (`refresh-imports-excluding-large`).
    #[arg(long)]
    pub exclude_large: bool,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Options for the curated commands that need only a repo directory
/// (`all-imports`, `test`).
#[derive(ClapArgs)]
pub struct RepoArgs {
    #[arg(short = 'C', long = "directory", visible_alias = "repo", value_name = "DIR", default_value = ".")]
    pub repo: PathBuf,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

/// How a requested target maps onto owlmake's handling.
enum Kind {
    /// A release artefact (built by the native pipeline).
    Artefact,
    /// `all` / `prepare_release` — build every artefact.
    Release,
    /// `refresh-imports` family — rebuild import modules natively.
    RefreshImports { exclude_large: bool },
    /// `all_imports` — rebuild every individual import module.
    AllImports,
    /// `patterns` / `dosdp` — regenerate `definitions.owl` from the DOSDP patterns.
    Patterns,
    /// One mirror, by file (`mirror/<id>.owl`) or by the phony that fetches it
    /// (`mirror-<id>`). The payload is the import id.
    Mirror(String),
    /// One import module (`imports/<id>_import.owl`), built by its product's
    /// recorded pipeline rather than by the replayed rule the plan may also
    /// carry for the same file. The payload is the import id.
    ImportModule(String),
    /// Any other target the plan defines — QC targets included — run by
    /// executing the recipe the plan records for it.
    Recipe,
    /// A repo-scaffolding or housekeeping target owlmake deliberately does not
    /// run; the payload says why.
    Unsupported(&'static str),
    /// No matching artefact, special target, or plan rule.
    Unknown,
}

/// Every mirror target, in the two spellings [`mirror_target_id`] accepts. Named
/// off the `mirrors` refresh group, which is where the plan writes down what the
/// mirror switch covers.
fn mirror_targets(plan: &Plan) -> Vec<String> {
    let Some(g) = plan.refresh_groups.iter().find(|g| g.name == "mirrors") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for t in &g.targets {
        if let Some(id) = mirror_target_id(plan, t) {
            out.push(t.clone());
            out.push(format!("mirror-{id}"));
        }
    }
    out
}

/// The DOSDP products the pattern stage writes, in the plan's spelling. They
/// carry no plan rule (owlmake's own engine makes them), so every place that
/// enumerates buildable targets has to add them back.
fn pattern_product_targets(plan: &Plan) -> Vec<String> {
    let Some(d) = plan.dosdp.as_ref() else { return Vec::new() };
    let mut out = vec![d.output.clone()];
    if let Some(p) = std::path::Path::new(&d.output).parent() {
        out.push(p.join("pattern.owl").to_string_lossy().into_owned());
    }
    // The per-pattern products, one pair per pattern: the `.ofn` module the
    // generator writes beside its data table, and the `.txt` term file the seed
    // is built from. They are files the build produces and a caller can name, so
    // they belong in every enumeration of the target surface — `definitions.owl`
    // alone hides fifty-eight of them.
    for p in &d.patterns {
        let data = std::path::Path::new(&p.data);
        for ext in ["ofn", "txt"] {
            out.push(data.with_extension(ext).to_string_lossy().into_owned());
        }
    }
    out
}

/// The import id `target` names, when it is one of that import's mirror targets:
/// `mirror-<id>`, or `<mirrordir>/<id>.owl` under whichever directory the plan's
/// `mirrors` group puts them in. The merged mirror is NOT one of these — it has a
/// rule of its own and runs as an ordinary recipe.
fn mirror_target_id(plan: &Plan, target: &str) -> Option<String> {
    let name = std::path::Path::new(target).file_name()?.to_str()?;
    let id = match name.strip_prefix("mirror-") {
        Some(rest) if target == name => rest.to_string(),
        _ => name.strip_suffix(".owl")?.to_string(),
    };
    if id == "merged" {
        return None;
    }
    plan.imports.iter().any(|i| i.id == id).then_some(id)
}

/// Whether `target` names one of the DOSDP products the pattern stage writes —
/// `definitions.owl` (the plan's `dosdp.output`) or the `pattern.owl` beside it.
fn is_pattern_product(plan: &Plan, target: &str) -> bool {
    let Some(d) = plan.dosdp.as_ref() else { return false };
    if target == d.output {
        return true;
    }
    std::path::Path::new(&d.output)
        .parent()
        .map(|p| p.join("pattern.owl"))
        .is_some_and(|p| p.to_string_lossy() == target)
}

/// Whether `target` names one of the plan's release artefacts (by filename,
/// artefact name, or export format).
fn is_artefact(plan: &Plan, target: &str) -> bool {
    let id = &plan.id;
    let forms = [target.to_string(), format!("{id}-{target}.owl"), format!("{id}.{target}")];
    plan.artefacts.iter().any(|a| forms.iter().any(|f| *f == a.target))
}

fn classify(plan: &Plan, target: &str) -> Kind {
    if is_artefact(plan, target) {
        return Kind::Artefact;
    }
    // The DOSDP products are written by owlmake's own engine and so carry no plan
    // RULE — but they are release artefacts, and a target that cannot be named
    // cannot be asked for, so they are classified here: `om make
    // ../patterns/definitions.owl` runs the pattern stage that produces it.
    // Only when the plan carries no rule for it, though: a repo may build its
    // own `pattern.owl` beside the DOSDP output — MONDO reasons and reduces one
    // from `pattern-with-imports.owl` — and that recorded recipe wins.
    if is_pattern_product(plan, target) && !plan_target(plan, target) {
        return Kind::Patterns;
    }
    // The mirrors are the same shape as the pattern products: the executor builds
    // each from its import's `source` and `mirror_steps`, so they carry no plan
    // rule — and a target that cannot be named cannot be run. `om make
    // mirror/envo.owl` and `om make mirror-envo` both fetch that one mirror,
    // which is what makes a single mirror replayable on its own.
    if let Some(id) = mirror_target_id(plan, target) {
        return Kind::Mirror(id);
    }
    // The merged import is the same shape: a release asset whose path the plan
    // names and whose rule it deliberately does not carry, because owlmake builds
    // the whole merged pipeline natively. Under base merging that pipeline IS
    // `all_imports`.
    //
    // Only when the plan carries NO rule for it, though. A repo whose merged
    // import reads `mirror/merged.owl` — UBERON, MONDO — HAS a recorded recipe
    // (`native_import_targets` deliberately does not claim it), and routing that
    // to `all_imports` sent it to a builder that discards `IMP`: `om make
    // IMP=false MIR=false imports/merged_import.owl` re-downloaded every mirror
    // and OVERWROTE the committed merged import, where GNU make — whose module
    // and mirror rules do not exist under those flags — does nothing at all.
    if plan.merged_import.as_deref() == Some(target) && !plan_target(plan, target) {
        return Kind::AllImports;
    }
    // An import module the plan records as a product runs the product's own
    // pipeline. The plan may carry the replayed rule for the same file too, and
    // `Kind::Recipe` would run that instead — the rule agrees with the product
    // only until the product is edited.
    if let Some(imp) = crate::build::import_module_for(plan, target) {
        return Kind::ImportModule(imp.id.clone());
    }
    // The well-known phony target names are underscore-separated; owlmake accepts
    // the dashed form too, so normalize before matching them.
    match target.replace('-', "_").as_str() {
        "all" | "prepare_release" | "prepare_release_fast" => Kind::Release,
        "refresh_imports" | "no_mirror_refresh_imports" => Kind::RefreshImports { exclude_large: false },
        "refresh_imports_excluding_large" => Kind::RefreshImports { exclude_large: true },
        "all_imports" => Kind::AllImports,
        "patterns" | "dosdp" => Kind::Patterns,
        "update_repo" => Kind::Unsupported(
            "regenerates the ODK scaffolding from a downloaded ODK release; owlmake does not manage ODK setup",
        ),
        // `clean` runs from its recorded recipe like any other target: the
        // recipe's own realpath guard keeps the removals inside the repo.
        "seed" | "seed_via_docker" => {
            Kind::Unsupported("bootstraps a new ODK repository; use the ODK seed tooling")
        }
        // Everything else the PLAN defines. There is no QC special case: a QC
        // target is a target, its recipe is recorded like any other, and it runs
        // through the same path. Intercepting the well-known QC target names and
        // running a fixed list of checks instead would recognise one set of
        // variable spellings and nothing else — EFO's eleven `VCHECKS` would come
        // out as `[SKIP] no SPARQL_VALIDATION_CHECKS configured`, a check that
        // passes by doing nothing. Whether a target is QC is not a question
        // dispatch needs to answer.
        _ if plan_target(plan, target) => Kind::Recipe,
        _ => Kind::Unknown,
    }
}

pub fn step(_piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    // Positional `VAR=value` tokens are variable assignments, not targets.
    // Splitting them out here lets `owlmake make` — and the `make` PATH shim in
    // the Docker image — accept a repo's existing CI invocation verbatim
    // (`make test IMP=false PAT=false MIR=false`, `make ROBOT_ENV=… cl-base.owl`),
    // so CL's qc.yml/diff.yml need no changes.
    // `--strict` / `--xml-entities` change what a BUILD produces, so they are
    // plan state, not invocation state — and `execute_plan` sets both from the
    // plan. Accepting them here and then overwriting them would be a silent
    // "do less", so refuse instead of ignoring.
    if args.common.strict || args.common.xml_entities {
        bail!(
            "`--strict` / `--xml-entities` change what a build produces, so they are plan state: \
             set `strict: true` / `xml_entities: true` in owlmake.yaml (they still work on \
             `om convert`, `om merge` and the other single-command entry points)"
        );
    }
    let (targets, make_vars) = partition_make_args(&args.targets);
    // The assignments are NOT written into owlmake's own environment. They belong
    // to the recipes that read them, so they are carried on `ExecOpts` and applied
    // per spawn — a process-global `set_var` would make this run's invocation
    // visible to every later read in the process (jq's `$ENV` among them) and leak
    // into unrelated work in the same process.

    // The assignments are seeded BEFORE the build configuration is parsed: they
    // override every assignment the configuration makes itself, and its
    // conditionals are evaluated during the parse. OBA gates its whole import
    // section on `IMP`, so with `IMP=false` those rules must never exist.
    let repo = OdkRepo::load_with_vars(&args.repo, &make_vars.all)?;
    let plan_format = spec::PlanFormat::parse(&args.plan_format)?;
    let plan_write = if args.regenerate { PlanWrite::Regenerate } else { PlanWrite::Check };
    let full_plan =
        obtain_plan(&repo, plan_format, plan_write, make_vars.version.as_deref(), make_vars.today.as_deref(), make_vars.clock.as_deref())?;
    // The plan's recorded `emulate_robot_version` selects the two version-dependent byte
    // behaviours — whether OBO Graphs JSON nests axiom-annotation `meta`, and
    // whether a SPARQL update inherits the document's prefixes. Read from the
    // plan, so a plan-only repo produces the same bytes as the repo it came from.
    crate::build::set_robot_behaviours(&full_plan);

    // A switch this plan cannot vary is REFUSED, not ignored. Which rules exist
    // under `ifeq ($(BRI),true)` was decided when the plan was made, and the plan
    // holds one branch's rules; accepting a switch it cannot honour and building
    // the recorded branch anyway would be a build quietly doing something other
    // than what was asked. A switch the plan DECLARES as a refresh group is a
    // run input and is resolved below, whichever value it is given.
    for (name, value) in &make_vars.all {
        if full_plan.refresh_groups.iter().any(|g| &g.flag == name) {
            continue;
        }
        let Some(planned) = full_plan.gating_flags.get(name) else { continue };
        if value.trim() == planned {
            continue;
        }
        bail!(
            "`{name}={value}` asks for a build this plan does not describe: it was resolved \
             with `{name}={}`, and which rules exist depends on that.\n\
             The switches this plan can vary are: {}",
            if planned.is_empty() { "(unset)" } else { planned },
            if full_plan.refresh_groups.is_empty() {
                "(none)".to_string()
            } else {
                full_plan
                    .refresh_groups
                    .iter()
                    .map(|g| g.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        );
    }

    // An `IMP=`/`MIR=`/`PAT=` assignment overrides the flag default; otherwise the
    // `--imports`/`--patterns` flags apply.
    // `IMP` and `MIR` are INDEPENDENT switches: `IMP` decides whether the import
    // modules are rebuilt, `MIR` whether their mirrors are re-downloaded first.
    // Repos gate the two separately, and a mirror recipe typically asks for both,
    // so `MIR=false` must not force `Cached` over an explicit `IMP=true`:
    // `IMP=true MIR=false` — rebuild the modules from the mirrors already on disk —
    // is how imports are reworked offline, and has to stay expressible.
    // `--rebuild imports` / `--keep imports` are the group spellings of
    // `--imports fresh` / `--imports cached`, so they steer the import machinery
    // the same way — naming the group and having the modules reused anyway is the
    // switch not reaching the step it gates.
    //
    // With none of them given the answer comes from the PLAN, exactly as it does
    // for the mirrors below and for every switch of the repository's own: the
    // `imports` group records whether an ordinary build of this repo rebuilds its
    // modules, and MP's says it does. A hardcoded `cached` here is the plan not
    // reaching the step it gates.
    let plan_rebuilds_imports = full_plan
        .refresh_groups
        .iter()
        .find(|g| g.name == "imports")
        .map(|g| matches!(g.default, crate::plan::Freshness::Rebuild))
        .unwrap_or(false);
    let flag_imports = args.imports.as_deref().map(parse_imports).transpose()?;
    let imports_mode = match (make_vars.imports, make_vars.mir) {
        (Some(m), _) => m,
        // `MIR=true` alone still implies the import path runs.
        (None, Some(true)) => ImportsMode::Fresh,
        (None, _) => match flag_imports {
            Some(m) => m,
            // `--rebuild mirrors` implies it for the reason `MIR=true` does: a
            // re-fetched mirror is a new input to the module built from it.
            None if args.rebuild.iter().any(|g| g == "imports" || g == "mirrors") => {
                ImportsMode::Fresh
            }
            None if args.keep.iter().any(|g| g == "imports") => ImportsMode::Cached,
            None if plan_rebuilds_imports => ImportsMode::Fresh,
            None => ImportsMode::Cached,
        },
    };
    // `MIR=` is the assignment spelling, `--rebuild mirrors` / `--keep mirrors`
    // the group spelling. Both are resolved here and carried into `ExecOpts` — a
    // switch the plan declares has to reach the step it gates, not merely be
    // validated against the plan's groups.
    //
    // With none of them given the answer comes from the PLAN, which records
    // whether an ordinary build of this repo re-fetches its mirrors
    // (`refresh_groups`, set by ingest). It used to be a hardcoded `true`, and
    // that is the plan not reaching the step it gates: EFO declares
    // `mirrors: default: keep` because no rule of its Makefile downloads
    // anything, and owlmake re-fetched all fourteen anyway — building the
    // release against whatever upstream had published that morning rather than
    // against the committed cache the reference build uses.
    //
    // A plan with no `mirrors` group has nothing to refresh, so the value is
    // moot; `true` keeps the older behaviour for anything that reaches here
    // without one.
    let plan_refreshes_mirrors = full_plan
        .refresh_groups
        .iter()
        .find(|g| g.name == "mirrors")
        .map(|g| matches!(g.default, crate::plan::Freshness::Rebuild))
        .unwrap_or(true);
    let refresh_mirrors = match make_vars.mir {
        Some(v) => v,
        None if args.rebuild.iter().any(|g| g == "mirrors") => true,
        None if args.keep.iter().any(|g| g == "mirrors") => false,
        None => plan_refreshes_mirrors,
    };
    let patterns_mode = match make_vars.patterns {
        Some(m) => m,
        None => parse_patterns(&args.patterns)?,
    };
    // Which recipe each target takes, decided from those same switches.
    //
    // A target whose recipe differs by branch carries both, so a run that flips a
    // switch RUNS the other branch instead of finding the target pinned. This is
    // the only place the two meet, and it is before every read of the plan below
    // — `--plan-only` prints what would run, and a phase builds it.
    let switches: Vec<(String, String)> = {
        let onoff = |on: bool| if on { "true".to_string() } else { "false".to_string() };
        let mut v = vec![
            ("MIR".to_string(), onoff(refresh_mirrors)),
            ("IMP".to_string(), onoff(matches!(imports_mode, ImportsMode::Fresh))),
            ("PAT".to_string(), onoff(patterns_mode == PatternsMode::Regenerate)),
        ];
        // …and every switch of the repository's own, resolved the same way: the
        // assignment as the configuration spells it, then `--rebuild`/`--keep`,
        // then what the plan says an ordinary build does.
        for g in &full_plan.refresh_groups {
            if v.iter().any(|(f, _)| f == &g.flag) || g.flag.is_empty() {
                continue;
            }
            let assigned =
                make_vars.all.iter().rev().find(|(n, _)| n == &g.flag).map(|(_, x)| crate::plan::is_on(x));
            let on = match assigned {
                Some(x) => x,
                None if args.rebuild.iter().any(|r| r == &g.name) => true,
                None if args.keep.iter().any(|k| k == &g.name) => false,
                None => matches!(g.default, crate::plan::Freshness::Rebuild),
            };
            v.push((g.flag.clone(), onoff(on)));
        }
        v
    };
    let full_plan = spec::bind_switches(full_plan, &switches);

    if args.list_targets {
        let mut names: Vec<String> = full_plan
            .artefacts
            .iter()
            .chain(full_plan.prerequisites.iter())
            .filter(|a| !a.missing_rule)
            .map(|a| a.target.clone())
            .chain(pattern_product_targets(&full_plan))
            .chain(full_plan.merged_import.clone())
            .chain(mirror_targets(&full_plan))
            .collect();
        names.sort();
        names.dedup();
        for n in names {
            println!("{n}");
        }
        return Ok(None);
    }


    // Selected targets = positional targets plus any `--artefact` flags. No
    // targets means the plan's `default_targets`, resolved HERE, before anything
    // branches — so a bare `owlmake`, a bare `owlmake make` and
    // `owlmake <those targets>` are literally the same code path. What the
    // default means was decided at plan time (EFO's `all` ends in `qc`); nothing
    // downstream re-derives it.
    let mut selection: Vec<String> =
        targets.iter().chain(args.artefacts.iter()).cloned().collect();
    let defaulted = selection.is_empty();
    if defaulted {
        selection = full_plan.default_targets.clone();
        if selection.is_empty() {
            bail!(
                "this plan names no default targets and none were given\navailable targets: {}",
                known_targets(&full_plan)
            );
        }
    }
    // `release`/`all` names the artefact set: splice it in rather than handling
    // it in the runner, so every index of `selection` is covered by exactly one
    // phase below. Left for the runner, a `Kind::Release` sitting at the artefact
    // split index would be dropped and `om make prepare_release` would silently
    // do nothing.
    let mut publish = defaulted;
    {
        let mut spliced: Vec<String> = Vec::with_capacity(selection.len());
        for t in selection {
            // A repo that DEFINES `all` means its own members by it, and each of
            // them is spliced in to be classified and dispatched exactly as if
            // it had been named on the command line — so `all_imports` reaches
            // the import builder ("naming `all_imports` is asking for the
            // modules"), `qc` runs its recipe, `release` does its copy.
            //
            // EFO's is `all: all_imports all_gwas all_components release qc`.
            // Treated as a synonym for `prepare_release` it collapsed to the
            // artefact set: fourteen imports and four release products built,
            // and the components, the GWAS component, the release copy and every
            // QC check silently not run. Only a repo with no `all` of its own
            // falls through to the artefact set below.
            if t.replace('-', "_") == "all" {
                if let Some(a) =
                    full_plan.artefacts.iter().chain(full_plan.prerequisites.iter())
                        .find(|a| a.target == t && !a.missing_rule)
                {
                    if !a.needs.is_empty() {
                        for m in &a.needs {
                            if !spliced.contains(m) {
                                spliced.push(m.clone());
                            }
                        }
                        continue;
                    }
                }
            }
            if matches!(classify(&full_plan, &t), Kind::Release) {
                publish = true;
                for a in full_plan.artefacts.iter().filter(|a| !a.missing_rule) {
                    if !spliced.contains(&a.target) {
                        spliced.push(a.target.clone());
                    }
                }
            } else if !spliced.contains(&t) {
                spliced.push(t);
            }
        }
        selection = spliced;
    }

    // Classify and validate every selector up front so a typo or an unsupported
    // target fails clearly rather than as a confusing empty plan.
    let kinds: Vec<(String, Kind)> =
        selection.iter().map(|t| (t.clone(), classify(&full_plan, t))).collect();
    for (t, kind) in &kinds {
        match kind {
            Kind::Unknown => bail!(
                "no rule to make target `{t}`\navailable targets: {}",
                known_targets(&full_plan)
            ),
            Kind::Unsupported(why) => {
                bail!("`{t}` is an ODK-infrastructure target owlmake does not run ({why})")
            }
            _ => {}
        }
    }

    if args.plan_only {
        let artefacts: Vec<String> =
            kinds.iter().filter(|(_, k)| matches!(k, Kind::Artefact)).map(|(t, _)| t.clone()).collect();
        let plan = if selection.is_empty() || (artefacts.is_empty() && !kinds.is_empty()) {
            full_plan
        } else {
            spec::bind_switches(
                bind_run_version(&repo, &repo.plan(&artefacts)?, make_vars.version.as_deref(), make_vars.today.as_deref(), make_vars.clock.as_deref())?,
                &switches,
            )
        };
        println!("{plan}");
        for (t, kind) in &kinds {
            if !matches!(kind, Kind::Artefact | Kind::Release) {
                status!("make: `{t}` would run as a non-artefact target");
            }
        }
        return Ok(None);
    }

    // The resolved build-mode switches are NOT published to the process
    // environment. An environment variable that changes what a build produces is a
    // bypass channel around the plan, and a value re-derived for the environment
    // can disagree with the one owlmake acted on — `imports_mode` ignores `MIR`
    // when `IMP` is given, so `IMP=true MIR=false` would advertise `MIR=true` while
    // owlmake pins the mirrors. A script that needs the value gets it as an
    // explicit `VAR=value` on the command line, which `run_env` carries to the
    // spawn.
    // `--rebuild`/`--keep` name groups the PLAN declares. Asking to rebuild a
    // group the plan does not have is a hard error — it is a request the plan
    // cannot satisfy. Asking to KEEP one is a no-op with a note: preserving
    // something the plan cannot rebuild is trivially satisfied, and refusing it
    // would break `MIR=false` on a repo that declares no such group at all.
    // (EFO does declare a `mirrors` group — owlmake knows its URLs from
    // `get_mirrors.sh` — but with `default: keep`, since no make rule fetches
    // them. Membership says what CAN be re-fetched, the default says whether an
    // ordinary build DOES.)
    // `--rebuild imports` and `--imports fresh` are one request.
    let mut rebuild_groups = args.rebuild.clone();
    if matches!(imports_mode, ImportsMode::Fresh)
        && !rebuild_groups.iter().any(|g| g == "imports")
        && full_plan.refresh_groups.iter().any(|g| g.name == "imports")
    {
        rebuild_groups.push("imports".to_string());
    }
    for g in &args.rebuild {
        if !full_plan.refresh_groups.iter().any(|r| &r.name == g) {
            bail!(
                "this plan declares no `{g}` group to rebuild\navailable groups: {}",
                if full_plan.refresh_groups.is_empty() {
                    "(none)".to_string()
                } else {
                    full_plan.refresh_groups.iter().map(|r| r.name.clone()).collect::<Vec<_>>().join(", ")
                }
            );
        }
    }
    for g in &args.keep {
        if !full_plan.refresh_groups.iter().any(|r| &r.name == g) {
            status!("make: nothing named `{g}` in this plan to keep");
        }
    }

    // Every OTHER group the plan declares — the ones a repository invented, like
    // UBERON's `bridges` — that this run KEEPS. `mirrors`, `imports` and
    // `patterns` are above; they steer the import machinery as well as the pins,
    // so they keep their own fields rather than becoming names in a list.
    //
    // Read off the same resolution the branch selection used, so a switch cannot
    // pick one branch and pin under the other.
    let kept_groups: Vec<String> = full_plan
        .refresh_groups
        .iter()
        .filter(|g| !["mirrors", "imports", "patterns"].contains(&g.name.as_str()))
        .filter(|g| {
            switches.iter().any(|(f, v)| *f == g.flag && !crate::plan::is_on(v))
        })
        .map(|g| g.name.clone())
        .collect();
    for g in &kept_groups {
        status!("make: keeping the `{g}` group — its targets are used as they stand");
    }

    let output_dir = args.output_dir.clone().unwrap_or_else(|| repo.dir.clone());

    // One selection, run in three phases so the repo's declared order is kept
    // while the release artefacts still share a single merge/reason pipeline:
    //   1. non-artefact targets that PRECEDE the first artefact;
    //   2. every artefact, together;
    //   3. everything after it.
    // A plan whose `default_targets` end in a QC target therefore runs the release
    // and then the QC, in that order, which is what the repo asked for.
    let artefacts: Vec<String> =
        kinds.iter().filter(|(_, k)| matches!(k, Kind::Artefact)).map(|(t, _)| t.clone()).collect();
    let split = kinds.iter().position(|(_, k)| matches!(k, Kind::Artefact));

    // This run's inputs, shared by every phase: a non-artefact target has to see
    // `-B` and the `VAR=value` assignments exactly as the artefact path does.
    // `IMP=false` / `--keep imports` is the caller pinning the imports group for
    // THIS run. It has to travel with the other run inputs, because the entry
    // points that mean "rebuild the imports" would otherwise overrule it.
    let imports_pinned = matches!(make_vars.imports, Some(ImportsMode::Cached))
        || args.keep.iter().any(|g| g == "imports");
    let mirrors_pinned =
        make_vars.mir == Some(false) || args.keep.iter().any(|g| g == "mirrors");
    let run_opts = ExecOpts {
        imports_mode,
        patterns_mode,
        refresh_mirrors,
        imports_pinned,
        mirrors_pinned,
        kept_groups,
        output_dir: output_dir.clone(),
        run_env: make_vars.env.clone(),
        always_make: args.always_make,
        keep_going: args.keep_going,
        assume_new: args.assume_new.clone(),
    };
    let run_one = |t: &str, kind: &Kind, repo: &OdkRepo, plan: &Plan| -> Result<()> {
        match kind {
            Kind::Artefact | Kind::Release => Ok(()), // phase 2 / spliced above
            Kind::RefreshImports { exclude_large } => {
                build::refresh_imports(repo, plan, *exclude_large, &run_opts)
            }
            Kind::AllImports => build::build_all_imports(repo, plan, &run_opts),
            Kind::Patterns => {
                if !build::regenerate_patterns(repo, plan, &run_opts)? {
                    status!("make: no DOSDP patterns configured");
                }
                Ok(())
            }
            Kind::Mirror(id) => build::refresh_one_mirror(repo, plan, id, &run_opts),
            Kind::ImportModule(id) => build::build_import_module(repo, plan, id, &run_opts),
            Kind::Recipe => build::run_target_recipe(repo, plan, t, &run_opts),
            Kind::Unsupported(_) | Kind::Unknown => unreachable!(),
        }
    };

    // A QC failure is REPORTED and the build carries on to the artefacts, exiting
    // non-zero at the end. The phase order still matters — HPO's `test` writes
    // `hp.obo` as a side effect and must precede `all_assets` — but a check that
    // fails is not evidence that the RELEASE cannot be built, and aborting on one
    // would leave `om make` unable to produce any artefact on a repo with a single
    // failing check (HPO: `hp_error`, `fastobo`; OBA: `check_children_oba`).
    let mut failures: Vec<String> = Vec::new();
    let mut report = |t: &str, r: Result<()>| match r {
        Ok(()) => {}
        Err(e) => {
            status!("make: [FAIL] {t}: {e:#}");
            failures.push(t.to_string());
        }
    };

    let before = split.unwrap_or(kinds.len());
    for (t, kind) in kinds.iter().take(before) {
        let r = run_one(t, kind, &repo, &full_plan);
        report(t, r);
    }
    // Under `-k` an artefact failure is reported and the targets AFTER the
    // artefacts still run: they are separate targets, and the ones that do not
    // depend on the failed artefact are buildable. Stopping here skipped
    // `check_rdfxml_assets`, the `.db.gz` products and `release_diff` — declared
    // default targets — because one unrelated artefact could not be built, and
    // the run reported only the artefact, so the skip was invisible.
    let mut artefact_err: Option<anyhow::Error> = None;
    if !artefacts.is_empty() {
        // A defaulted build hands `build_plan` the FULL plan; a named subset gets
        // a subset plan. Partial replay of a full artefact set rewrites artefacts
        // the full plan produces, at the wrong sizes.
        let r = if defaulted {
            build_plan(&repo, &full_plan, &run_opts, publish)
        } else {
            let plan = spec::bind_switches(
                bind_run_version(&repo, &repo.plan(&artefacts)?, make_vars.version.as_deref(), make_vars.today.as_deref(), make_vars.clock.as_deref())?,
                &switches,
            );
            build_plan(&repo, &plan, &run_opts, publish)
        };
        match r {
            Ok(()) => {}
            Err(e) if args.keep_going => {
                status!("make: [FAIL] {e:#}");
                artefact_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    for (t, kind) in kinds.iter().skip(before) {
        let r = run_one(t, kind, &repo, &full_plan);
        report(t, r);
    }
    // Whatever this run wrote on its way to something else and does not keep —
    // excluding the targets the caller actually asked for, which are goals rather
    // than intermediates however the plan classifies them.
    let goals: Vec<String> = kinds.iter().map(|(t, _)| t.clone()).collect();
    build::sweep_transients(&repo, &full_plan, &goals);
    if !failures.is_empty() || artefact_err.is_some() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(e) = &artefact_err {
            parts.push(format!("{e:#}"));
        }
        if !failures.is_empty() {
            parts.push(format!("{} target(s) failed: {}", failures.len(), failures.join(", ")));
        }
        bail!("{}", parts.join("; "));
    }
    Ok(None)
}

// --- Curated build commands -----------------------------------------------

/// `prepare-release` / `all`: build every release artefact.
pub fn prepare_release(a: &TargetArgs) -> Result<()> {
    let repo = OdkRepo::load(&a.repo)?;
    // A release never rewrites the committed plan: `--regenerate` is a
    // deliberate, separate act, not something a build does on the way past.
    let plan = obtain_plan(&repo, spec::PlanFormat::Yaml, PlanWrite::Check, None, None, None)?;
    let plan = {
        let switches = default_switches(&plan);
        spec::bind_switches(plan, &switches)
    };
    let output_dir = a.output_dir.clone().unwrap_or_else(|| repo.dir.clone());
    let imports_mode = parse_imports(&a.imports)?;
    let opts = ExecOpts {
        imports_mode,
        patterns_mode: parse_patterns(&a.patterns)?,
        refresh_mirrors: true,
        // `--imports cached` on this command IS the caller pinning them.
        imports_pinned: matches!(imports_mode, ImportsMode::Cached),
        mirrors_pinned: false,
        // This command takes no switches, so every group does what the plan says.
        kept_groups: default_kept_groups(&plan),
        output_dir,
        run_env: Vec::new(),
        always_make: false,
        keep_going: false,
        assume_new: Vec::new(),
    };
    build_plan(&repo, &plan, &opts, true)?;
    // A curated whole-release command names no individual target, so nothing here
    // is a goal in the command-line sense and everything transient is swept.
    build::sweep_transients(&repo, &plan, &[]);
    Ok(())
}

/// `refresh-imports`: rebuild import modules from upstream (native).
pub fn refresh_imports(a: &RefreshArgs) -> Result<()> {
    let repo = OdkRepo::load(&a.repo)?;
    let plan = bind_run_version(&repo, &repo.plan(&[])?, None, None, None)?;
    let plan = {
        let switches = default_switches(&plan);
        spec::bind_switches(plan, &switches)
    };
    build::refresh_imports(&repo, &plan, a.exclude_large, &default_exec_opts(&repo))
}

/// `all-imports`: rebuild every individual import module from upstream.
pub fn all_imports(a: &RepoArgs) -> Result<()> {
    let repo = OdkRepo::load(&a.repo)?;
    let plan = bind_run_version(&repo, &repo.plan(&[])?, None, None, None)?;
    let plan = {
        let switches = default_switches(&plan);
        spec::bind_switches(plan, &switches)
    };
    build::build_all_imports(&repo, &plan, &default_exec_opts(&repo))
}

/// `test` / `qc`: run the repository's own QC target, from the plan.
///
/// There is no built-in QC pipeline: whatever the repo declares under this name
/// is what runs, exactly as `owlmake make test` would. The checks those recipes
/// ask for are `om report`, `om reason` and `om verify`, so a QC run needs
/// nothing beyond the `om` binary itself.
pub fn test(a: &RepoArgs) -> Result<()> {
    let repo = OdkRepo::load(&a.repo)?;
    let plan = obtain_plan(&repo, spec::PlanFormat::Yaml, PlanWrite::Check, None, None, None)?;
    let plan = {
        let switches = default_switches(&plan);
        spec::bind_switches(plan, &switches)
    };
    let name = ["test", "qc"]
        .into_iter()
        .find(|n| plan_target(&plan, n))
        .ok_or_else(|| anyhow::anyhow!(
            "this repo declares no `test` or `qc` target\navailable targets: {}",
            known_targets(&plan)
        ))?;
    build::run_target_recipe(&repo, &plan, name, &default_exec_opts(&repo))
}

/// The run inputs of a curated build command, which takes none of its own.
fn default_exec_opts(repo: &OdkRepo) -> ExecOpts {
    ExecOpts {
        imports_mode: ImportsMode::Cached,
        patterns_mode: PatternsMode::Regenerate,
        refresh_mirrors: true,
        // These commands take no flags, so the caller has pinned nothing: an
        // `all-imports`/`refresh-imports` entry point still means "rebuild".
        imports_pinned: false,
        mirrors_pinned: false,
        kept_groups: Vec::new(),
        output_dir: repo.dir.clone(),
        run_env: Vec::new(),
        always_make: false,
        keep_going: false,
        assume_new: Vec::new(),
    }
}

// --- helpers --------------------------------------------------------------

/// Whether the plan's `ODK_VERSION_MAKEFILE` variable — the generator version a
/// generated build configuration stamps into itself, which ingest copies into
/// `plan.variables` — reads as at least v1.6.1, the point from which OBO Graphs
/// output nests axiom-annotation `meta`. MONDO records no version at all and OBA
/// records `v1.6`; ECTO records `v1.6.1`.
fn odk_at_least_1_6_1(plan: &Plan) -> bool {
    // A repo whose build configuration was not generated declares no
    // `ANNOTATE_ONTOLOGY_VERSION`, so nothing here pins it to the older un-nested
    // shape and it falls through to the current behaviour: nested.
    //
    // That fallback is coarser than the answer the build actually uses, and nothing
    // in the tree calls this: the nesting switch is set from `plan.emulate_robot_version`,
    // which ingest resolves from the repo's CI when the build configuration is
    // hand-written. EFO pins v1.9.7 there, so `efo.json` carries no nested `meta`
    // even though the rule below would answer "nested" for it. MONDO's configuration
    // is generated but records no version, so it stays un-nested either way.
    let var = |name: &str| plan.variables.get(name).map(String::as_str).unwrap_or("").trim();
    if var("ANNOTATE_ONTOLOGY_VERSION").is_empty() {
        return true;
    }
    let v = var("ODK_VERSION_MAKEFILE");
    let v = v.trim().trim_start_matches('v');
    let parts: Vec<u32> = v.split('.').filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [maj, min, patch, ..] => (*maj, *min, *patch) >= (1, 6, 1),
        [maj, min] => (*maj, *min) > (1, 6),
        _ => false,
    }
}

/// What to do about a committed plan file when the repo ALSO has a build
/// configuration the plan can be regenerated from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlanWrite {
    /// Check that the committed file is what ingest produces, and fail if it is
    /// not. The default for every build.
    Check,
    /// Overwrite it with what ingest produces (`--regenerate`).
    Regenerate,
}

/// Obtain the full plan. When the repo carries a committed plan file, that file
/// IS the plan: it is read as input and never rewritten. Otherwise the plan is
/// regenerated from the repo's build configuration and saved.
fn obtain_plan(
    repo: &OdkRepo,
    format: spec::PlanFormat,
    write: PlanWrite,
    version: Option<&str>,
    today: Option<&str>,
    clock: Option<&str>,
) -> Result<Plan> {
    let plan = if repo.spec.is_some() {
        repo.plan(&[])?
    } else {
        regen_plan(repo, format, write)?
    };
    bind_run_version(repo, &plan, version, today, clock)
}

/// The switch values an entry point that takes none resolves to: whatever the
/// plan says an ordinary build of this repository does. A curated command still
/// has to CHOOSE, because a target whose recipe differs by branch has no recipe
/// until one is chosen.
fn default_switches(plan: &Plan) -> Vec<(String, String)> {
    plan.refresh_groups
        .iter()
        .filter(|g| !g.flag.is_empty())
        .map(|g| {
            let on = matches!(g.default, crate::plan::Freshness::Rebuild);
            (g.flag.clone(), if on { "true".to_string() } else { "false".to_string() })
        })
        .collect()
}

/// Resolve this run's release version against the plan's default and bind it in.
///
/// A plan is written with the version as a reference, so it is committed once and
/// released on any date; a plan that is about to RUN needs the date itself. This
/// is the one place the two meet, and it runs after the plan is written — the
/// file on disk keeps the reference.
fn bind_run_version(
    repo: &OdkRepo,
    plan: &Plan,
    requested: Option<&str>,
    today: Option<&str>,
    clock: Option<&str>,
) -> Result<Plan> {
    // A plan that names a version FILE is asking for the version the file holds
    // on the day of the build, not the one it held when the plan was written —
    // bumping it is ordinary curation, and it must not require regenerating the
    // plan. An explicit `VERSION=` still wins, and a file that has gone missing
    // falls back to the default the plan recorded rather than failing the build.
    let from_file = requested.is_none().then(|| plan.version_file.as_deref()).flatten().and_then(
        |rel| {
            let text = std::fs::read_to_string(repo.dir.join(rel)).ok()?;
            let v = text.trim().to_string();
            (!v.is_empty()).then_some(v)
        },
    );
    let version = match from_file {
        Some(v) => v,
        None => crate::plan::release_version(&plan.version, requested),
    };
    spec::bind_version(plan, &version, today, clock, &repo.dir)
}

/// Regenerate the plan (at the repo root) from the repo's build configuration and
/// return the full plan. Where the plan is generated this way it is purely an
/// output — the configuration it was derived from is the single source of truth.
///
/// A plan that is ALREADY committed is not overwritten, though: it is checked
/// against what ingest produces, and a disagreement is an error. See
/// [`check_committed_plan`] for why that has to be louder than a rewrite.
fn regen_plan(repo: &OdkRepo, format: spec::PlanFormat, write: PlanWrite) -> Result<Plan> {
    let plan_path = repo.root.join(format.file_name());
    let full = repo.plan(&[])?;
    let spec = OwlmakeSpec::from_plan(&full);
    if write == PlanWrite::Check && plan_path.is_file() {
        check_committed_plan(&plan_path, &spec)?;
        return Ok(full);
    }
    spec::save(&spec, &plan_path)?;
    status!("make: regenerated {} from the build config", plan_path.display());
    // Keep an already-committed plan in the OTHER spelling in step. Leaving it
    // stale would make the next build fail the "both files, same build" check for
    // no reason the user did anything about.
    let other = repo.root.join(match format {
        spec::PlanFormat::Yaml => spec::PLAN_FILE_JSON,
        spec::PlanFormat::Json => spec::PLAN_FILE,
    });
    if other.is_file() {
        spec::save(&spec, &other)?;
        status!("make: regenerated {} alongside it", other.display());
    }
    status!("make: (run `owlmake schema` for the JSON Schema to validate it against)");
    Ok(full)
}

/// A committed plan and the build configuration it was generated from must
/// describe the SAME build. This fails the build when they do not.
///
/// The plan is a pure function of the build configuration, so the two can only
/// disagree because the plan is stale. That is invisible while the
/// configuration is still in the tree — the build regenerates and uses the
/// fresh plan — and then becomes the WHOLE build the moment the Makefile is
/// deleted, which is the point of committing a plan at all. Overwriting it
/// instead would hide the same thing a second way: the plan a reviewer reads
/// would never be the plan that ran, and a migration could look finished while
/// the committed file still described a different build.
///
/// Both sides are resolved under the SAME configuration — the repository's own,
/// never the caller's — so nothing a run asks for can make the plan look stale.
/// A release date is a run input, and the plan states it as a default and refers
/// to it; a build on a different day, with different switches, compares equal.
fn check_committed_plan(path: &Path, fresh: &OwlmakeSpec) -> Result<()> {
    let committed = spec::load(path).with_context(|| {
        format!(
            "the committed {} cannot be read, so it cannot be the build that runs \
             (regenerate it with `om make --regenerate`)",
            path.display()
        )
    })?;
    // Compare what each plan MEANS to the build, by putting the fresh one
    // through the same write-then-read cycle the committed one has already been
    // through. Writing rebases a path to the plan file's directory and reading
    // rebases it back, and that pair is a NORMAL FORM rather than an identity:
    // the Makefile's `MIRRORDIR=./mirror` is written `src/ontology/mirror` and
    // read back as `mirror`, which no longer looks like a path and so is left
    // alone by any later rebase. All three spell the same directory. Comparing
    // the raw in-memory forms would therefore report a difference for every
    // such value, on a plan owlmake itself had just written.
    let fresh = round_trip(fresh, path)?;

    let mut old = serde_json::to_value(&committed)?;
    let mut new = serde_json::to_value(&fresh)?;
    // A plan that names a `version_file` records the version as a DEFAULT read
    // from that file, and the run re-reads it. Bumping the file for a release is
    // therefore a run input, and a run input must never make the plan stale: the
    // repo a "regenerate it" instruction points at is exactly what the plan
    // exists to replace, so answering that to `3.92.0 -> 3.93.0` would demand a
    // plan rewrite for every release. The two plans still have to agree on WHICH
    // file the version comes from, which is what the comparison keeps.
    if fresh.version_file.is_some() && committed.version_file == fresh.version_file {
        for v in [&mut old, &mut new] {
            if let Some(obj) = v.as_object_mut() {
                obj.remove("version");
            }
        }
    }
    if old == new {
        return Ok(());
    }

    let mut diffs = Vec::new();
    value_diffs(&old, &new, String::new(), &mut diffs, 25);
    let shown = diffs.len();
    let listing: String =
        diffs.iter().map(|d| format!("  {d}\n")).collect::<Vec<_>>().concat();
    bail!(
        "the committed {} is not what this repository's build configuration generates.\n\n\
         {listing}{}\n\
         The plan is generated from the build configuration, so this means the committed plan \
         is stale — and it is the plan that builds once the build configuration is gone. \
         Building from the regenerated plan while leaving the committed one in place would \
         make the file under review a different build from the one that ran.\n\n\
         Run `om make --regenerate` to rewrite it from the build configuration, check the diff, \
         and commit it.",
        path.display(),
        if shown == 25 { "  … (truncated)\n" } else { "" },
    )
}

/// Put a freshly generated plan through the write-then-read cycle a committed
/// one has been through, so the two can be compared in the same normal form.
///
/// The temp file sits BESIDE the real plan on purpose: writing rebases every
/// path against the plan file's own directory, so a scratch file anywhere else
/// would normalize to a different answer and the comparison would be of two
/// different things.
fn round_trip(fresh: &OwlmakeSpec, path: &Path) -> Result<OwlmakeSpec> {
    let ext = match spec::PlanFormat::of_path(path) {
        spec::PlanFormat::Json => "json",
        spec::PlanFormat::Yaml => "yaml",
    };
    let tmp = path.with_file_name(format!(".owlmake-plan-check.{}.{ext}", std::process::id()));
    spec::save(fresh, &tmp)?;
    let loaded = spec::load(&tmp);
    let _ = std::fs::remove_file(&tmp);
    loaded.context("internal: a freshly generated plan did not read back")
}

/// Collect up to `limit` human-readable paths at which two plans differ.
/// `old` is the committed file, `new` what the build configuration generates.
fn value_diffs(
    old: &serde_json::Value,
    new: &serde_json::Value,
    at: String,
    out: &mut Vec<String>,
    limit: usize,
) {
    use serde_json::Value;
    if out.len() >= limit {
        return;
    }
    let here = |k: &str| if at.is_empty() { k.to_string() } else { format!("{at}.{k}") };
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                match (a.get(k), b.get(k)) {
                    (Some(x), Some(y)) => value_diffs(x, y, here(k), out, limit),
                    (Some(x), None) => {
                        out.push(format!("{}: committed plan has {}, the build config has no such field", here(k), brief(x)))
                    }
                    (None, Some(y)) => {
                        out.push(format!("{}: missing from the committed plan (build config gives {})", here(k), brief(y)))
                    }
                    (None, None) => {}
                }
                if out.len() >= limit {
                    return;
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                value_diffs(x, y, format!("{at}[{i}]"), out, limit);
                if out.len() >= limit {
                    return;
                }
            }
        }
        // Different lengths: name the entries that moved rather than only the
        // counts. "a 14-entry list, build config gives a 16-entry list" says
        // something changed but never what, which is the one thing the reader
        // needs to judge whether the plan or the build configuration is wrong.
        (Value::Array(a), Value::Array(b)) => {
            let only = |x: &Vec<Value>, y: &Vec<Value>| -> Vec<String> {
                x.iter().filter(|v| !y.contains(v)).take(5).map(brief).collect()
            };
            let (dropped, added) = (only(a, b), only(b, a));
            out.push(format!(
                "{at}: {} entries in the committed plan, {} from the build config{}{}",
                a.len(),
                b.len(),
                if dropped.is_empty() { String::new() } else { format!("; only committed: {}", dropped.join(", ")) },
                if added.is_empty() { String::new() } else { format!("; only from the build config: {}", added.join(", ")) },
            ));
        }
        _ if old != new => {
            out.push(format!("{at}: committed plan has {}, build config gives {}", brief(old), brief(new)))
        }
        _ => {}
    }
}

/// A value short enough to sit in an error message.
fn brief(v: &serde_json::Value) -> String {
    let mut s = match v {
        serde_json::Value::String(s) => format!("`{s}`"),
        serde_json::Value::Array(a) => format!("a {}-entry list", a.len()),
        serde_json::Value::Object(o) => format!("{} field(s)", o.len()),
        other => other.to_string(),
    };
    if s.chars().count() > 90 {
        s = format!("{}…", s.chars().take(89).collect::<String>());
    }
    s
}

/// The groups an entry point that takes no switches keeps: whichever the plan
/// says an ordinary build does not refresh.
fn default_kept_groups(plan: &Plan) -> Vec<String> {
    plan.refresh_groups
        .iter()
        .filter(|g| !["mirrors", "imports", "patterns"].contains(&g.name.as_str()))
        .filter(|g| matches!(g.default, crate::plan::Freshness::Keep))
        .map(|g| g.name.clone())
        .collect()
}

/// `VAR=value` assignments pulled out of the positional target list.
#[derive(Default)]
struct MakeVars {
    imports: Option<ImportsMode>,
    /// `MIR` as the user actually spelled it. `MIR` and `IMP` are independent:
    /// `IMP` says whether to rebuild the import MODULES, `MIR` whether to refresh
    /// the mirror DOWNLOADS. A single `ImportsMode` cannot carry both, so the
    /// spelling is kept here: an explicit `MIR=false` must win for the DOWNLOADS
    /// even when `IMP=true` has already selected `Fresh` for the modules, so that
    /// `IMP=true MIR=false` rebuilds the modules from the mirrors already on disk
    /// rather than replacing EFO's pinned mirrors with current upstream releases
    /// mid-build.
    mir: Option<bool>,
    patterns: Option<PatternsMode>,
    /// The release version this run stamps (`TODAY=`/`VERSION=`). The plan
    /// carries a default and refers to it from every string that needs it, so a
    /// caller can release the SAME plan under any date.
    version: Option<String>,
    /// The CALENDAR DATE this run stamps (`TODAY=`), which is a different fact
    /// from the release version even though ODK's `TODAY=` supplies both. A repo
    /// can release version `3.93.0` on any day, and `VERSION=3.93.0` says nothing
    /// about the date — so only `TODAY=` sets this, and a run that names neither
    /// falls back to the clock.
    today: Option<String>,
    clock: Option<String>,
    /// Other `VAR=value` assignments, placed in each recipe's spawn environment
    /// (e.g. `ROBOT_ENV`, `ROBOT_JAVA_ARGS`) for any recipe target that reads them.
    env: Vec<(String, String)>,
    /// EVERY assignment, verbatim, to be bound in the build configuration's own
    /// variable table when it is parsed. A command-line assignment overrides the
    /// configuration's own default, and recipes are full of guards that read it —
    /// a mirror rule downloads only when both `MIR` and `IMP` are `true`.
    /// Interpreting `IMP=false` only as owlmake's own imports mode would leave
    /// `$(IMP)` expanding to the configured default `true`, so
    /// `om make IMP=false MIR=false` would re-mirror all twenty of OBA's imports
    /// where nothing at all should happen.
    all: Vec<(String, String)>,
}

/// Split `VAR=value` tokens out of the positional arguments, returning the real
/// build targets and the parsed variable assignments. A token is an assignment
/// when it is `NAME=…` with `NAME` a variable identifier (leading letter or `_`,
/// then alphanumerics/`_`); everything else is a target. Only the first `=`
/// separates name from value, so `ROBOT_ENV=ROBOT_JAVA_ARGS=-Xmx6G` binds
/// `ROBOT_ENV` to `ROBOT_JAVA_ARGS=-Xmx6G`.
fn partition_make_args(raw: &[String]) -> (Vec<String>, MakeVars) {
    let mut targets = Vec::new();
    let mut vars = MakeVars::default();
    for tok in raw {
        if let Some(eq) = tok.find('=') {
            let name = &tok[..eq];
            let is_ident = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if is_ident {
                apply_make_var(&mut vars, name, &tok[eq + 1..]);
                continue;
            }
        }
        targets.push(tok.clone());
    }
    (targets, vars)
}

fn apply_make_var(vars: &mut MakeVars, name: &str, value: &str) {
    vars.all.push((name.to_string(), value.to_string()));
    let truthy = matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on");
    // A workflow flag drives owlmake's own switch below, but make ALSO exports
    // every command-line variable to recipe environments (manual §5.7.2) — and
    // the ODK's recipes rely on it, recursing through `$(MAKE) … PAT=false` and
    // running repo scripts that read the flags. Swallowing them left a shelled-out
    // recipe seeing whatever the Makefile's own defaults said instead of what this
    // run asked for.
    if crate::odk::makefile::MakeModel::WORKFLOW_FLAGS.contains(&name) {
        vars.env.push((name.to_string(), value.to_string()));
    }
    match name {
        // `IMP`: whether to (re)build import modules from their upstream
        // mirrors. IMP=false → reuse the committed imports (`--imports cached`);
        // IMP=true → re-mirror from upstream (`--imports fresh`).
        "IMP" => vars.imports = Some(if truthy { ImportsMode::Fresh } else { ImportsMode::Cached }),
        // `MIR`: whether to refresh the mirror downloads — and nothing else.
        // It used to fill in the import mode too, so a positional `MIR=false`
        // pinned the modules as well: `om make imports/obi_import.owl --rebuild
        // imports MIR=false` reported the module "pinned (IMP=false)" and rebuilt
        // nothing, and `--imports fresh MIR=false` likewise. `IMP` and `MIR` are
        // independent switches, and what `MIR` implies for the import mode
        // (`MIR=true` alone still runs the import path) is decided where the mode
        // is resolved, with the flags and the plan in view.
        "MIR" => vars.mir = Some(truthy),
        // `PAT`: whether to regenerate `patterns/definitions.owl` from the
        // DOSDP patterns (PAT=true) or reuse the committed file (PAT=false).
        "PAT" => {
            vars.patterns =
                Some(if truthy { PatternsMode::Regenerate } else { PatternsMode::Cached })
        }
        // The release version. It is not a build-configuration read: the plan
        // states it in one field and refers to it everywhere else, and this
        // replaces the field's default for this run.
        "TODAY" | "VERSION" => {
            vars.version = Some(value.trim().to_string());
            // `TODAY=` is the run's calendar date as well as its version;
            // `VERSION=` is only the version and leaves the date to the clock.
            if name == "TODAY" {
                vars.today = Some(value.trim().to_string());
            }
            vars.env.push((name.to_string(), value.to_string()));
        }
        // The calendar day for the recipes that read the clock itself rather
        // than the release date. It defaults to the day the build runs; naming
        // it makes such a build reproducible on any later day.
        "CLOCK" => vars.clock = Some(value.trim().to_string()),
        // Anything else (ROBOT_ENV, ROBOT_JAVA_ARGS, …) is placed in the spawn
        // environment of the recipe targets that read it. owlmake's own steps read
        // none of these values themselves; they are carried through untouched so a
        // repo's existing CI invocation still works.
        _ => vars.env.push((name.to_string(), value.to_string())),
    }
}

fn parse_imports(s: &str) -> Result<ImportsMode> {
    match s {
        "cached" => Ok(ImportsMode::Cached),
        "fresh" => Ok(ImportsMode::Fresh),
        other => bail!("unknown --imports mode `{other}` (expected `cached` or `fresh`)"),
    }
}

fn parse_patterns(s: &str) -> Result<PatternsMode> {
    match s {
        "regenerate" | "fresh" | "true" => Ok(PatternsMode::Regenerate),
        "cached" | "false" => Ok(PatternsMode::Cached),
        other => bail!("unknown --patterns mode `{other}` (expected `regenerate` or `cached`)"),
    }
}

/// Execute a plan, refusing to run a partial release (any uncovered step on the
/// release path is a hard error — the printed plan shows exactly what and why).
/// When `publish` is set (a full release), the built products are moved to the
/// repository root, where a release consumer expects to find them.
fn build_plan(repo: &OdkRepo, plan: &Plan, opts: &ExecOpts, publish: bool) -> Result<()> {
    let gaps = plan.blocking_gaps();
    if !gaps.is_empty() {
        // Say WHAT is wrong, here, in the message that stops the build. A count
        // and an invitation to re-run under another flag makes the reader work
        // for something already known — and the commonest gap of all is a named
        // input that is simply not there, which is one line to read and one file
        // to restore.
        bail!(
            "{} step(s) on the release path are not covered by owlmake:\n  {}",
            gaps.len(),
            gaps.join("\n  ")
        );
    }
    status!("make: executing release plan for `{}`", plan.id);
    build::execute(repo, plan, opts)?;
    // Publish to the repo root only for a full release built in the default
    // location; an explicit --output-dir means "put the products here", so honour it.
    if publish && opts.output_dir == repo.dir && repo.root != repo.dir {
        publish_release(repo, plan, &opts.output_dir)?;
    }
    status!("make: done.");
    Ok(())
}

/// Publish the built release products to the repository root: a release is
/// published from the root, not from `src/ontology`, so each product gains a
/// second name there and keeps the one it was built under. Only on-release-path
/// artefacts are published; intermediates (`tmp/`, `imports/`, …) are not.
fn publish_release(repo: &OdkRepo, plan: &Plan, output_dir: &Path) -> Result<()> {
    let mut n = 0;
    for a in &plan.artefacts {
        if a.missing_rule {
            continue;
        }
        // Only top-level release products are published to the repo root.
        // On-path intermediates live in subdirs (`tmp/<id>-preprocess.owl`,
        // `components/…`, `imports/…`) — they aren't release artefacts, and their
        // destination dir wouldn't exist under the root, so skip anything with a `/`.
        if a.target.contains('/') {
            continue;
        }
        let src = output_dir.join(&a.target);
        if !src.exists() {
            continue;
        }
        let dst = repo.root.join(&a.target);
        // The build-directory copy STAYS: later recipes name a release artefact by
        // its plain name and resolve it against the build directory — a check that
        // reads `mondo.owl` after the release step must find the file the same run
        // just built. The published name is a hard link where the filesystem allows
        // one, so it costs an inode rather than another 254 MB.
        //
        // Unlink the destination before linking. The destination may already BE the
        // source under its other name, and writing through it would empty both.
        let _ = std::fs::remove_file(&dst);
        if std::fs::hard_link(&src, &dst).is_err() {
            std::fs::copy(&src, &dst)
                .with_context(|| format!("publishing {} to {}", a.target, repo.root.display()))?;
        }
        n += 1;
    }
    if n > 0 {
        status!("make: published {n} release file(s) to {}", repo.root.display());
    }
    Ok(())
}

fn known_targets(plan: &Plan) -> String {
    let mut names: Vec<String> = plan
        .artefacts
        .iter()
        .chain(plan.prerequisites.iter())
        .filter(|a| !a.missing_rule)
        .map(|a| a.target.clone())
        .chain(pattern_product_targets(plan))
        .chain(plan.merged_import.clone())
        .collect();
    names.sort();
    names.dedup();
    names.join(", ")
}

/// Whether the plan defines `target` as something it can build.
fn plan_target(plan: &Plan, target: &str) -> bool {
    plan.artefacts
        .iter()
        .chain(plan.prerequisites.iter())
        .any(|a| a.target == target && !a.missing_rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(args: &[&str]) -> (Vec<String>, MakeVars) {
        partition_make_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    /// `MIR=false` pins the mirrors and says nothing about the modules: the
    /// import mode stays unset so `--rebuild imports` / `--imports fresh` can
    /// decide it. It used to fill in `Cached`, and the flags were overridden.
    #[test]
    fn mir_alone_does_not_decide_the_import_mode() {
        let (targets, vars) = split(&["imports/obi_import.owl", "MIR=false"]);
        assert_eq!(targets, vec!["imports/obi_import.owl"]);
        assert_eq!(vars.mir, Some(false));
        assert!(vars.imports.is_none(), "MIR=false must not pin the imports");
        let (_, vars) = split(&["MIR=true"]);
        assert!(vars.imports.is_none(), "MIR=true's implication is resolved with the flags, not here");
        let (_, vars) = split(&["IMP=true", "MIR=false"]);
        assert!(matches!(vars.imports, Some(ImportsMode::Fresh)));
    }

    /// CL's `qc.yml`: `make ROBOT_ENV='ROBOT_JAVA_ARGS=-Xmx6G' test IMP=false PAT=false MIR=false`.
    #[test]
    fn qc_invocation_splits_vars_from_the_test_target() {
        let (targets, vars) = split(&["ROBOT_ENV=ROBOT_JAVA_ARGS=-Xmx6G", "test", "IMP=false", "PAT=false", "MIR=false"]);
        assert_eq!(targets, vec!["test"]);
        assert!(matches!(vars.imports, Some(ImportsMode::Cached)));
        assert!(matches!(vars.patterns, Some(PatternsMode::Cached)));
        // The nested `=` stays in the value, exactly as make binds it — and the
        // workflow flags are exported alongside it, because make exports every
        // command-line variable to recipe environments and the ODK's recipes
        // recurse through `$(MAKE) … PAT=false`.
        assert_eq!(
            vars.env,
            vec![
                ("ROBOT_ENV".to_string(), "ROBOT_JAVA_ARGS=-Xmx6G".to_string()),
                ("IMP".to_string(), "false".to_string()),
                ("PAT".to_string(), "false".to_string()),
                ("MIR".to_string(), "false".to_string()),
            ]
        );
    }

    /// CL's `diff.yml`: `make IMP=FALSE PAT=FALSE MIR=FALSE cl-base.owl` (uppercase FALSE).
    #[test]
    fn diff_invocation_keeps_the_artefact_target() {
        let (targets, vars) = split(&["IMP=FALSE", "PAT=FALSE", "MIR=FALSE", "cl-base.owl"]);
        assert_eq!(targets, vec!["cl-base.owl"]);
        assert!(matches!(vars.imports, Some(ImportsMode::Cached)));
        assert!(matches!(vars.patterns, Some(PatternsMode::Cached)));
        // Exported as make would, with the value as written.
        assert_eq!(
            vars.env,
            vec![
                ("IMP".to_string(), "FALSE".to_string()),
                ("PAT".to_string(), "FALSE".to_string()),
                ("MIR".to_string(), "FALSE".to_string()),
            ]
        );
    }

    #[test]
    fn truthy_values_select_fresh_and_regenerate() {
        let (_, vars) = split(&["IMP=true", "PAT=true"]);
        assert!(matches!(vars.imports, Some(ImportsMode::Fresh)));
        assert!(matches!(vars.patterns, Some(PatternsMode::Regenerate)));
    }

    #[test]
    fn imp_takes_precedence_over_mir_regardless_of_order() {
        // IMP is the import control; MIR pins or refreshes the mirrors and is
        // never read as an import mode, whichever order the two arrive in.
        let (_, a) = split(&["MIR=true", "IMP=false"]);
        assert!(matches!(a.imports, Some(ImportsMode::Cached)));
        assert_eq!(a.mir, Some(true));
        let (_, b) = split(&["IMP=false", "MIR=true"]);
        assert!(matches!(b.imports, Some(ImportsMode::Cached)));
        // MIR alone leaves the import mode to the flags and the plan.
        let (_, c) = split(&["MIR=false"]);
        assert!(c.imports.is_none());
        assert_eq!(c.mir, Some(false));
    }

    #[test]
    fn a_token_whose_name_is_not_a_make_identifier_stays_a_target() {
        // A leading digit is not a valid variable name, so `1file=x` is a
        // target, not an assignment. A plain artefact name is untouched.
        let (targets, vars) = split(&["cl.owl", "1file=x"]);
        assert_eq!(targets, vec!["cl.owl", "1file=x"]);
        assert!(vars.imports.is_none() && vars.patterns.is_none() && vars.env.is_empty());
    }
}
