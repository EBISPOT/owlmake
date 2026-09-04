//! Execute a [`Plan`] by chaining owlmake's existing commands.
//!
//! This is the ONLY path to a build. The plan is the instruction set; where it
//! came from — resolved by [`crate::odk`] from an existing repository layout, or
//! committed as `owlmake.yaml` — is not visible from here and must not be. Each
//! artefact is built by threading temporary functional-syntax files through its
//! mapped steps, so every step runs the same command implementation the CLI does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cmd;

pub mod recipe;

use crate::plan::{ArtefactPlan, Plan};
use crate::plan::step::{Op, Step};
use crate::odk::OdkRepo;

/// Everything the executor is allowed to read: the repository's directories and
/// the plan.
///
/// Deliberately NOT an [`OdkRepo`]. The plan is the build's only instruction
/// set — a legacy build file is an input to *planning*, never to execution — and
/// a type with no route to ingest's view of the repository enforces that at
/// compile time rather than by convention. Whatever the executor needs has to be
/// recorded in the plan at ingest ([`Plan::variables`], [`Plan::prerequisites`]);
/// if it is not there the build fails loudly, instead of quietly reading a file
/// the repository need not still have.
pub struct Repo<'a> {
    /// The `src/ontology` directory.
    pub dir: PathBuf,
    /// The repository root.
    pub root: PathBuf,
    pub plan: &'a Plan,
    /// `VAR=value` assignments given on THIS invocation's command line.
    ///
    /// A command-line assignment reaches the environment of every shelled-out
    /// recipe, which is what lets a repo's CI pass `ROBOT_ENV='…'` into a QC run.
    /// They are carried here and applied with `Command::env` at each spawn instead
    /// of being written into owlmake's own process environment with
    /// `std::env::set_var`, which would make one run's invocation visible to every
    /// later read in the process — including jq's `$ENV` — and is a bypass channel
    /// around the plan by construction.
    pub run_env: Vec<(String, String)>,
    /// `-B`: run every step even when its target is up to date.
    pub always_make: bool,
    /// Whether this run refreshes the mirrors. FALSE means the mirror rules — the
    /// phony `mirror-%` and the `$(MIRRORDIR)/%.owl` copy — are not in play at all
    /// and the mirrors already on disk stand. The plan records those rules
    /// unconditionally, so execution is where the switch has to be honoured.
    pub refresh_mirrors: bool,
    /// Whether this run rebuilds the import modules, the same way. FALSE means the
    /// module rules are not in play and the committed `imports/*_import.owl` stand.
    /// The plan records those rules unconditionally, so execution is where the
    /// switch is honoured — and it must be, because MONDO's merged-import rule is
    /// one of them: a plain `om make mondo-simple.owl` would otherwise follow it
    /// into rebuilding `mirror/merged.owl` from mirrors a release build never
    /// downloads.
    pub refresh_imports: bool,
    /// The mirrors / imports group was pinned EXPLICITLY this run (`MIR=false`,
    /// `IMP=false`, `--keep`). See [`ExecOpts::mirrors_pinned`]: it decides what
    /// a pin means for a file that is absent.
    pub mirrors_pinned: bool,
    pub imports_pinned: bool,
    /// ODK `PAT`. FALSE means the DOSDP products are the committed ones, and the
    /// import seed's pattern half is extracted from `definitions.owl` rather than
    /// from the per-pattern term files — a different derivation, not a skipped
    /// one, so execution has to know which of the two applies.
    pub regenerate_patterns: bool,
    /// The refresh groups this run KEEPS — the plan's own groups beyond mirrors
    /// and imports, which have their own fields above. A target named by one is
    /// pinned exactly as `MIR=false` pins a mirror.
    pub kept_groups: Vec<crate::plan::RefreshGroup>,
    /// Targets whose recipe has already run in THIS invocation.
    ///
    /// A target is built at most once per run, however many paths reach it. One
    /// `seen` set per entry point — `run_target_recipe_planned` and each
    /// `ensure_built` — is not enough: a phony reached twice would then run twice,
    /// and HPO's `mirror-<id>` is reached once as a target and again as
    /// `mirror/<id>.owl`'s prerequisite, which would download all eleven mirrors
    /// TWICE (24 fetches).
    /// Borrowed from [`crate::odk::OdkRepo`], which lives for the whole run — a
    /// `Repo` is rebuilt per phase and could not carry this itself.
    pub built: &'a std::cell::RefCell<std::collections::HashSet<String>>,
    /// Targets whose build FAILED in this invocation — see [`crate::odk::OdkRepo`].
    /// A target naming one of these as a prerequisite cannot be up to date,
    /// however old the file sharing its name is.
    pub failed: &'a std::cell::RefCell<std::collections::HashSet<String>>,
    /// This run's `--assume-new` files (see [`ExecOpts::assume_new`]).
    pub assume_new: Vec<String>,
    /// Where release artefacts are written — the repo root for an ODK layout.
    ///
    /// The ODK builds each artefact IN `src/ontology` and copies it to the root
    /// at release time (`prepare_release_direct` is an `rsync -R`); owlmake
    /// writes it to the output dir directly. So a target's own file may be in
    /// either place, and a later phase that looks only in `dir` reads a built
    /// artefact as missing and runs its recipe again. MONDO's `mondo_edges.tsv`
    /// is where that showed: `all_artefacts` re-ran `kgx transform … mondo.json`
    /// after the release phase, and `mondo.json` was no longer under
    /// `src/ontology` to read.
    pub output_dir: PathBuf,
}

impl<'a> Repo<'a> {
    /// The executor's view of an ingested repo: its directories, plus the plan
    /// generated from it. This constructor is the single seam between the two.
    fn of(repo: &'a OdkRepo, plan: &'a Plan) -> Repo<'a> {
        Repo {
            dir: repo.dir.clone(),
            root: repo.root.clone(),
            plan,
            run_env: Vec::new(),
            always_make: false,
            refresh_mirrors: true,
            refresh_imports: true,
            mirrors_pinned: false,
            imports_pinned: false,
            regenerate_patterns: true,
            kept_groups: Vec::new(),
            built: &repo.built,
            failed: &repo.failed,
            assume_new: Vec::new(),
            output_dir: repo.root.clone(),
        }
    }

    /// As [`Repo::of`], carrying this run's command-line variable assignments
    /// and `-B`.
    fn of_with(repo: &'a OdkRepo, plan: &'a Plan, opts: &ExecOpts) -> Repo<'a> {
        Repo {
            dir: repo.dir.clone(),
            root: repo.root.clone(),
            plan,
            run_env: opts.run_env.clone(),
            always_make: opts.always_make,
            refresh_mirrors: opts.refresh_mirrors,
            refresh_imports: matches!(opts.imports_mode, ImportsMode::Fresh),
            mirrors_pinned: opts.mirrors_pinned,
            imports_pinned: opts.imports_pinned,
            regenerate_patterns: opts.patterns_mode == PatternsMode::Regenerate,
            kept_groups: plan
                .refresh_groups
                .iter()
                .filter(|g| opts.kept_groups.iter().any(|k| *k == g.name))
                .cloned()
                .collect(),
            built: &repo.built,
            failed: &repo.failed,
            assume_new: opts.assume_new.clone(),
            output_dir: opts.output_dir.clone(),
        }
    }

    /// Where `target`'s own file is, if it exists.
    ///
    /// Four spellings reach here for one file, and all four have to resolve or a
    /// built artefact reads as missing. The plan names paths relative to the plan
    /// FILE (`src/ontology/mondo_edges.tsv`) while a recipe names them relative to
    /// the build directory (`mondo_edges.tsv`), and owlmake writes a release
    /// artefact to the OUTPUT dir where the ODK leaves it in `src/ontology` — so
    /// the plan-relative spelling joined onto the output dir gives
    /// `<root>/src/ontology/…`, which is nowhere.
    fn target_file(&self, target: &str) -> Option<PathBuf> {
        // `src/ontology/mondo_edges.tsv` → `mondo_edges.tsv`, so it can be looked
        // for where the release actually put it.
        let stripped = self
            .dir
            .strip_prefix(&self.root)
            .ok()
            .and_then(|d| Path::new(target).strip_prefix(d).ok())
            .map(|p| self.output_dir.join(p));
        let candidates = [
            Some(self.dir.join(target)),
            Some(self.output_dir.join(target)),
            stripped,
            Some(self.root.join(target)),
        ];
        candidates.into_iter().flatten().find(|p| p.exists())
    }

    /// A build-time variable the plan recorded (`SRC`, `OTHER_SRC`, `ROBOT`,
    /// `OBOBASE`, …); `""` when the plan does not carry it.
    fn var(&self, name: &str) -> &str {
        self.plan.variables.get(name).map(String::as_str).unwrap_or("").trim()
    }

    /// The scratch directory the plan's `TMPDIR` names, resolved under the
    /// ontology directory. `tmp` is the value assumed when the plan carries no
    /// `TMPDIR`, and it is the ONLY case in which the recorded value is not used:
    /// `all_pattern_terms.txt` and the per-pattern seed files are written here and
    /// read back by the import-seed rules, so writing them anywhere other than
    /// where the plan says would leave those rules reading a stale file or none.
    fn tmp_dir(&self) -> PathBuf {
        self.dir.join(match self.var("TMPDIR") {
            "" => "tmp",
            v => v,
        })
    }

    /// The plan entry that builds `target`, over the recorded prerequisites and
    /// then the release artefacts. This is the only rule lookup there is: the
    /// answer comes from the plan, or there is no answer.
    fn target(&self, name: &str) -> Option<&'a ArtefactPlan> {
        self.plan
            .prerequisites
            .iter()
            .chain(self.plan.artefacts.iter())
            .find(|a| a.target == name && !a.missing_rule)
    }
}

/// A target that exists only while the mirrors are being refreshed — so with them
/// pinned it is not a target at all and the mirror on disk stands.
///
/// Two shapes: the phony `mirror-<id>` that fetches upstream into
/// `$(TMPDIR)/mirror-<id>.owl`, and the `$(MIRRORDIR)/%.owl` rule that copies that
/// over the pinned `mirror/<id>.owl`.
/// The plan's `mirrors` refresh group names the second but not the first, so both
/// are matched here — skipping only the copy still re-downloads.
fn is_mirror_target(repo: &Repo, target: &str) -> bool {
    if repo.plan.imports.iter().any(|i| target == format!("mirror-{}", i.id)) {
        return true;
    }
    repo.plan
        .refresh_groups
        .iter()
        .filter(|g| g.name == "mirrors")
        .any(|g| g.targets.iter().any(|t| t == target))
}

/// The switch that PINS `target` for this run, if any.
///
/// A pinned target's rule is not in play: under the switch the run turned off it
/// does not exist, so the file on disk is final and nothing may rebuild it or
/// fetch anything on its behalf. Every group answers the same question, so they
/// are asked here in one place and the answer names the switch, which is what a
/// message about a pinned file has to say.
fn pinned_by(repo: &Repo, target: &str) -> Option<Pin> {
    if !repo.refresh_mirrors && is_mirror_target(repo, target) {
        return Some(Pin { flag: "MIR".into(), group: "mirrors".into(), explicit: repo.mirrors_pinned });
    }
    if !repo.refresh_imports && is_import_target(repo, target) {
        return Some(Pin { flag: "IMP".into(), group: "imports".into(), explicit: repo.imports_pinned });
    }
    // A group of a repository's own invention is a plain list of targets, so
    // membership is the whole test. Compared by filename as well as by path,
    // because a recipe names `bridge/x.owl` where the plan names
    // `src/ontology/bridge/x.owl`.
    let same = |a: &str| {
        a == target
            || (Path::new(a).file_name().is_some()
                && Path::new(a).file_name() == Path::new(target).file_name())
    };
    repo.kept_groups
        .iter()
        .find(|g| g.targets.iter().any(|t| same(t)))
        // A repo-invented group reaches `kept_groups` through one resolution for
        // the run, so default and explicit are not distinguished here; treated
        // as explicit, which keeps the strict answer for its absent files.
        .map(|g| Pin { flag: g.flag.clone(), group: g.name.clone(), explicit: true })
}

/// Why a target is pinned: the switch that turned its rules off, and the group
/// name that turns them back on.
struct Pin {
    flag: String,
    group: String,
    /// Stated by the caller for THIS run, as against the group's default.
    explicit: bool,
}

impl std::fmt::Display for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}=false", self.flag)
    }
}

/// The import product whose module `target` names, when the plan records that
/// product's own pipeline (`ImportPlan::steps`). The plan spells outputs
/// repo-relative (`src/ontology/imports/x_import.owl`) while a caller names them
/// from the ontology directory (`imports/x_import.owl`), so the match is on whole
/// trailing path components, not on the file name alone — `mirror/x.owl` must not
/// pass for `imports/x.owl`.
pub fn import_module_for<'p>(plan: &'p Plan, target: &str) -> Option<&'p crate::plan::ImportPlan> {
    let same = |a: &str| a == target || Path::new(a).ends_with(target) || Path::new(target).ends_with(a);
    plan.imports.iter().find(|i| !i.steps.is_empty() && same(&i.output))
}

/// A target that is built only while the import modules are being rebuilt — the
/// merged import and every per-product module, in the plan's own spelling.
fn is_import_target(repo: &Repo, target: &str) -> bool {
    let same = |a: &str| {
        a == target
            || Path::new(a).file_name() == Path::new(target).file_name()
                && Path::new(a).file_name().is_some()
    };
    repo.plan.merged_import.as_deref().is_some_and(same)
        || repo.plan.imports.iter().any(|i| same(&i.output))
        // …and whatever else the plan's `imports` group names. `is_mirror_target`
        // already reads its group; this one did not, so a repo whose import
        // products are NOT ODK import modules had nothing pinned by `IMP=false`.
        // UBERON's `imports/local-*.owl` are built from mirrors by an ordinary
        // pattern rule, and with the mirrors pinned but the products not, they were
        // rebuilt from a mirror that is not there — 477-byte stubs in place of
        // 58 MB of FBbt.
        || repo
            .plan
            .refresh_groups
            .iter()
            .filter(|g| g.name == "imports")
            .any(|g| g.targets.iter().any(|t| same(t)))
}

/// Run a file operation with the build's view of where artefacts live.
///
/// Release artefacts are written to the output directory as they are built, so
/// a copy whose recipe path names the build directory (`rsync -R ecto.owl …`
/// from `src/ontology`) may find its source only at the published location. A
/// source that is absent at its recipe path but is a plan target published to
/// the output directory is read from there — and when that published file IS
/// the copy's destination, the copy is already complete and does nothing.
fn run_file_op(repo: &Repo, op: &recipe::FileOp) -> Result<()> {
    use recipe::FileOp;
    if let FileOp::Copy { src, dst, recursive, relative } = op {
        let mut remaining: Vec<String> = Vec::new();
        for s in src {
            if repo.dir.join(s).exists() {
                remaining.push(s.clone());
                continue;
            }
            let Some(published) = repo.target_file(s).filter(|p| p.is_file()) else {
                // Not published either: keep it for the plain run, whose error
                // names the missing file.
                remaining.push(s.clone());
                continue;
            };
            let d = repo.dir.join(dst);
            let dest = if *relative {
                d.join(s)
            } else if d.is_dir() {
                d.join(Path::new(s).file_name().unwrap_or_default())
            } else {
                d.clone()
            };
            if dest.exists() && published.canonicalize().ok() == dest.canonicalize().ok() {
                continue;
            }
            if let Some(par) = dest.parent() {
                std::fs::create_dir_all(par)?;
            }
            std::fs::copy(&published, &dest).with_context(|| {
                format!("cp {} {}", published.display(), dest.display())
            })?;
        }
        if remaining.is_empty() {
            return Ok(());
        }
        return FileOp::Copy {
            src: remaining,
            dst: dst.clone(),
            recursive: *recursive,
            relative: *relative,
        }
        .run(&repo.dir);
    }
    op.run(&repo.dir)
}

/// Whether a missing prerequisite is an INTERMEDIATE the requesting target does
/// not need built.
///
/// A file only a pattern-rule chain names (`ArtefactPlan::intermediate`) is not
/// created just because it is absent: the chain runs only when the target that
/// needs it is out of date with respect to the intermediate's own
/// prerequisites. ECTO's component stamps are the shape —
/// `components/<x>.owl: tmp/stamp-component-<x>.owl`, with the stamp made by
/// `touch` from nothing: a build whose components are present must create no
/// stamps, and building one first would put it newer than its component and run
/// the component recipe off the back of a file the build itself invented.
/// `-B` overrides this like every other up-to-date rule.
fn skip_missing_intermediate(repo: &Repo, target: &str, need: &str) -> bool {
    if repo.always_make {
        return false;
    }
    let Some(na) = repo.target(need) else { return false };
    if !na.intermediate || repo.dir.join(need).exists() {
        return false;
    }
    let Some(tm) = repo
        .target_file(target)
        .filter(|p| p.is_file())
        .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
    else {
        return false;
    };
    // Is the target out of date against the intermediate's own prerequisites?
    // Walked transitively through further missing intermediates; anything
    // unresolvable builds the chain rather than silently skipping it.
    fn out_of_date(
        repo: &Repo,
        tm: std::time::SystemTime,
        a: &crate::plan::ArtefactPlan,
        depth: usize,
    ) -> bool {
        if depth == 0 {
            return true;
        }
        for p in a.needs.iter().filter(|n| !a.order_only.contains(n)) {
            match std::fs::metadata(repo.dir.join(p)).and_then(|m| m.modified()) {
                Ok(m) => {
                    if m > tm {
                        return true;
                    }
                }
                Err(_) => match repo.target(p) {
                    Some(pa) if pa.intermediate => {
                        if out_of_date(repo, tm, pa, depth - 1) {
                            return true;
                        }
                    }
                    Some(_) => return true,
                    None => {}
                },
            }
        }
        false
    }
    !out_of_date(repo, tm, na, 8)
}

/// The up-to-date test: does `target` exist and is no prerequisite newer?
///
/// A prerequisite that is absent cannot make the target stale — it is not newer
/// than anything. Absence means the file genuinely does not exist, not that it is
/// still to be built: the prerequisites passed here are committed source files
/// (the plan's DOSDP template yamls), which no rule produces.
fn is_newer_than_all(target: &Path, prereqs: &[PathBuf]) -> bool {
    let Ok(t) = std::fs::metadata(target).and_then(|m| m.modified()) else { return false };
    !prereqs.iter().any(|p| {
        std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|pm| pm > t)
    })
}

/// The import whose mirror this path is, if any — `<MIRRORDIR>/<id>.owl` for one
/// of the plan's import products.
fn mirror_import_for<'p>(repo: &Repo<'p>, path: &str) -> Option<&'p crate::plan::ImportPlan> {
    let dir = {
        let d = repo.var("MIRRORDIR");
        if d.is_empty() { "mirror".to_string() } else { d.to_string() }
    };
    // Compared as PATHS, with a leading `./` normalized away — `components()`
    // alone keeps that one. The recorded `$(MIRRORDIR)` is the configuration's
    // own spelling — EFO writes `./mirror` — while a rule's prerequisite says
    // `mirror/mondo.owl`, and only as paths do the two meet.
    use std::path::Component;
    let parts = |p: &str| {
        std::path::Path::new(p)
            .components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| c.as_os_str().to_os_string())
            .collect::<Vec<_>>()
    };
    let want = parts(path);
    repo.plan.imports.iter().find(|i| parts(&format!("{dir}/{}.owl", i.id)) == want)
}

/// Whether a path names one of the DOSDP products owlmake writes natively
/// (`write_pattern_seed_files` / `regenerate_patterns_planned`), and which
/// therefore has no plan rule to build it.
///
/// The per-pattern products are the PLAN's — one `.ofn` and one `.txt` beside each
/// data table it names — not a directory convention: a repo may run the generator
/// over more than one data directory, and only the plan says which.
fn is_native_pattern_product(repo: &Repo, p: &str) -> bool {
    let name = Path::new(p).file_name().and_then(|s| s.to_str()).unwrap_or("");
    if matches!(
        name,
        "all_pattern_terms.txt" | "pattern_owl_seed.txt" | "pattern.owl" | "definitions.owl"
    ) {
        return true;
    }
    let Some(dosdp) = repo.plan.dosdp.as_ref() else { return false };
    let stem = Path::new(p).with_extension("");
    (p.ends_with(".txt") || p.ends_with(".ofn"))
        && dosdp
            .patterns
            .iter()
            .any(|pat| Path::new(&pat.data).with_extension("") == stem)
}

/// Split `"PROP VALUE"` annotation specs (as `rewrite-def` accepts them) on the
/// first space into `(property, value)` pairs.
fn split_pairs(specs: &[String]) -> Vec<(String, String)> {
    specs
        .iter()
        .filter_map(|s| s.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect()
}

#[derive(Clone, Copy)]
pub enum ImportsMode {
    Cached,
    Fresh,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PatternsMode {
    /// Regenerate `patterns/definitions.owl` from the DOSDP patterns (`PAT=true`,
    /// the default).
    Regenerate,
    /// Reuse the committed `patterns/definitions.owl` (`PAT=false`).
    Cached,
}

pub struct ExecOpts {
    pub imports_mode: ImportsMode,
    pub patterns_mode: PatternsMode,
    /// `MIR`: whether to re-download each import's mirror before rebuilding the
    /// module. Independent of [`ImportsMode`] — mirror downloads and module rules
    /// are separate switches, and a repo has plenty of both (MONDO: 27 mirror
    /// rules, 32 module rules) — so `IMP=true MIR=false` means "rebuild the
    /// modules from the mirrors already on disk". Folding the two into one mode
    /// would make that combination impossible to ask for.
    pub refresh_mirrors: bool,
    /// The run PINNED the imports group explicitly (`IMP=false`, `--keep
    /// imports`). An entry point whose whole meaning is "rebuild the imports"
    /// — `all_imports`, `refresh-imports` — may upgrade a SILENT caller to a
    /// rebuild, but it must not overrule this one: GNU make resolves `make
    /// IMP=false all_imports` as nothing to do, because under `IMP=false` the
    /// module rules are not defined. Forcing the rebuild regardless re-mirrored
    /// every upstream and overwrote the committed merged import.
    pub imports_pinned: bool,
    /// The run pinned the MIRRORS group explicitly (`MIR=false`, `--keep
    /// mirrors`), as against the plan's `default: keep`. The two pins hold
    /// different promises for a file that is ABSENT: an explicit pin was stated
    /// about this run and an absent file under it is an error, while a group
    /// default pins the content of files that exist — a target nothing ever
    /// committed (EFO gitignores `imports/mondo_import.owl` and its mirror) has
    /// no content to pin, and every fresh clone must build it once.
    pub mirrors_pinned: bool,
    /// The plan's OTHER refresh groups that this run keeps, by name — everything
    /// beyond `mirrors`/`imports`/`patterns`, whose own fields above carry them.
    /// A kept group's targets are not in play: their rules exist only under the
    /// switch the run turned off, so the files on disk are final.
    pub kept_groups: Vec<String>,
    pub output_dir: PathBuf,
    /// This invocation's `VAR=value` assignments (see [`Repo::run_env`]).
    pub run_env: Vec<(String, String)>,
    /// `-B`/`--always-make`.
    pub always_make: bool,
    /// `-k`/`--keep-going`: when an artefact fails, carry on with the ones that do
    /// not depend on it and fail at the end. A release is not all-or-nothing —
    /// uPheno's `upheno-old-model.owl` is built from
    /// `purl.obolibrary.org/obo/upheno/metazoa.owl`, whose `releases/latest`
    /// asset is gone, so without this one dead URL costs the other 24 products.
    pub keep_going: bool,
    /// `-W`/`--assume-new`: files to treat as just modified. A target depending
    /// on one runs its recipe; the file itself is neither rebuilt nor touched.
    pub assume_new: Vec<String>,
}

/// Build the plan. The ingested repo supplies its directories and nothing else —
/// from here down, [`Repo`] is the whole world.
pub fn execute(repo: &OdkRepo, plan: &Plan, opts: &ExecOpts) -> Result<()> {
    // `of_with`, not `of`: the run's options have to reach the executor. `of`
    // hardcodes `always_make: false` and `refresh_mirrors: true`, which would
    // leave `-B` inert and make `MIR=false` unable to pin the mirrors — a
    // `MIR=false` build would then re-download every mirror and overwrite the
    // pinned copies with the result.
    execute_plan(&Repo::of_with(repo, plan, opts), plan, opts)
}

// Every entry point below takes the run's `ExecOpts` for the same reason
// `execute` does: `-B` and `MIR=false` are this run's inputs, and an entry point
// that builds its `Repo` with `of` silently discards them — `om make -B <target>`
// would go through `run_target_recipe` and report the target up to date.

/// Rebuild every import module from upstream (the `refresh-imports` target).
pub fn refresh_imports(
    repo: &OdkRepo,
    plan: &Plan,
    exclude_large: bool,
    opts: &ExecOpts,
) -> Result<()> {
    // `refresh-imports` IS the request to rebuild the modules, whatever
    // `--imports` says — the same reason `refresh_imports_planned` treats the
    // mirrors as refreshed by definition here. Without this the `IMP=false`
    // pinning below would skip the very target that was asked for, and the run
    // would report success having rebuilt nothing.
    let mut r = Repo::of_with(repo, plan, opts);
    r.refresh_imports = !opts.imports_pinned;
    refresh_imports_planned(&r, plan, exclude_large)
}

/// Regenerate the DOSDP pattern products.
pub fn regenerate_patterns(repo: &OdkRepo, plan: &Plan, opts: &ExecOpts) -> Result<bool> {
    regenerate_patterns_planned(&Repo::of_with(repo, plan, opts), plan)
}

/// Rebuild every individual import module from its recorded pipeline.
pub fn build_all_imports(repo: &OdkRepo, plan: &Plan, opts: &ExecOpts) -> Result<()> {
    // As `refresh_imports`: naming `all_imports` is asking for the modules —
    // unless the same command line pinned them, which outranks the request.
    let mut r = Repo::of_with(repo, plan, opts);
    r.refresh_imports = !opts.imports_pinned;
    build_all_imports_planned(&r, plan)
}

/// Refresh ONE mirror, named either as its file (`mirror/<id>.owl`) or as the
/// phony that fetches it (`mirror-<id>`).
///
/// The mirrors carry no plan RULE — the executor builds each from its import's
/// own `source` and `mirror_steps` — so without this the only way to ask for one
/// is to ask for everything that reads it.
pub fn refresh_one_mirror(repo: &OdkRepo, plan: &Plan, id: &str, opts: &ExecOpts) -> Result<()> {
    let r = Repo::of_with(repo, plan, opts);
    let Some(imp) = plan.imports.iter().find(|i| i.id == id) else {
        bail!("no import `{id}` in the plan");
    };
    ensure_mirror(&r, imp, r.refresh_mirrors).map(|_| ())
}

/// Build one import module by its product's recorded pipeline — `om make
/// imports/<id>_import.owl`. The plan may also carry the replayed rule for the
/// same file; the product's steps are the plan's statement of how the module is
/// built, so they are what runs (see `refresh_imports_planned`).
pub fn build_import_module(repo: &OdkRepo, plan: &Plan, id: &str, opts: &ExecOpts) -> Result<()> {
    let r = Repo::of_with(repo, plan, opts);
    let Some(imp) = plan.imports.iter().find(|i| i.id == id) else {
        bail!("no import `{id}` in the plan");
    };
    if opts.imports_pinned {
        status!("make: `{}` pinned (IMP=false)", imp.output);
        return Ok(());
    }
    let catalog = load_catalog_planned(&r);
    let work = r.output_dir.join(".owlmake-odk-tmp");
    std::fs::create_dir_all(&work)?;
    build_one_import(&r, plan, imp, &catalog, &work)
        .with_context(|| format!("rebuilding import module `{}`", imp.id))?;
    r.built.borrow_mut().insert(imp.output.clone());
    Ok(())
}

/// Run a target the plan defines. A target the plan does not define is an error:
/// there is nothing else to fall back to.
pub fn run_target_recipe(
    repo: &OdkRepo,
    plan: &Plan,
    target: &str,
    opts: &ExecOpts,
) -> Result<()> {
    run_target_recipe_planned(&Repo::of_with(repo, plan, opts), target)
}

/// The `emulate_robot_version` boundary at which both version-dependent byte behaviours
/// flip: axiom annotations start nesting in OBO Graphs JSON, and a SPARQL update
/// starts inheriting the document's prefixes.
const ROBOT_1_9_9: (u32, u32, u32) = (1, 9, 9);

/// Apply the byte-level output conventions a repo's existing releases were made
/// under, from the one plan field that records which side of the boundary it is
/// on.
///
/// All three flip at the same boundary, so all three read the same recorded fact.
/// Deciding them separately would give a repo the older JSON nesting together with
/// the newer prefix handling — a combination no release carries, and one that puts
/// an `xmlns:doap`/`xmlns:protege` into `subsets/mondo-clingen.owl` that MONDO's own
/// releases do not have.
///
/// The third is the OBO extended prefix map. It is a data asset rather than a
/// behaviour, but it belongs here for the same reason: which map is in hand decides
/// how every CURIE in every SSSOM artefact resolves, and the two differ by 388
/// prefixes. A version emulated with the other version's map is not approximately
/// right, it is a different answer.
pub fn set_robot_behaviours(plan: &Plan) {
    let post_1_9_9 = plan.emulate_robot_version >= ROBOT_1_9_9;
    crate::io::obograph::set_nest_axiom_anns(post_1_9_9);
    crate::cmd::query::set_update_keeps_prefixes(post_1_9_9);
    crate::sssom::converter::set_obo_epm(plan.emulate_robot_version);
}

fn execute_plan(repo: &Repo, plan: &Plan, opts: &ExecOpts) -> Result<()> {
    use crate::progress::Stage;

    // The three byte-affecting global switches are set ONCE, here, from the plan.
    // The plan is their only writer on the build path: reachable from the
    // environment or from whichever subcommand ran last, they would let the same
    // plan produce different bytes depending on ambient state.
    set_robot_behaviours(plan);
    crate::io::set_run_options(crate::io::RunOptions {
        strict: plan.strict,
        xml_entities: plan.xml_entities,
    });

    std::fs::create_dir_all(&opts.output_dir)?;
    let tmp = opts.output_dir.join(".owlmake-odk-tmp");
    // The `.owlmake-odk-tmp` directory caches built intermediates as `.ofn`
    // (read back by downstream artefacts via `resolve_input`). Those caches are
    // only valid for the exact `owlmake` binary that produced them: a fix to any
    // build op would otherwise be silently masked by a stale cache left by an
    // older binary — a UBERON `uberon-edit.ofn` holding incoherent reasoning
    // survives the very fix that corrects it. Stamp the cache directory with this
    // binary's identity and wipe it whenever the stamp does not match.
    invalidate_stale_intermediate_cache(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let catalog = load_catalog_planned(repo);

    // Build artefacts source-fed first, then those fed by another artefact (e.g.
    // `<id>.owl` ⟵ `<id>-full.owl`), so inputs exist when needed.
    set_planned_targets(plan);
    let order = artefact_order(plan);
    let buildable: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| {
            let a = &plan.artefacts[i];
            !a.missing_rule && a.gaps.is_empty()
        })
        .collect();

    // --- Patterns (DOSDP → definitions.owl) --------------------------------
    // `PAT=true` (the default): regenerate patterns/definitions.owl from the
    // DOSDP patterns + their TSV data using owlmake's own dosdp engine, so a bare
    // `owlmake` build reflects pattern edits. `--patterns cached` reuses the
    // committed file (`PAT=false`).
    //
    // BEFORE the imports, because the import seed reads what this writes: the
    // import seed is the pre-seed plus `$(TMPDIR)/all_pattern_terms.txt`, and that
    // comes from the DOSDP term files and `pattern.owl`. Regenerating afterwards
    // would seed the merged import from the PREVIOUS release's patterns.
    if opts.patterns_mode == PatternsMode::Regenerate {
        regenerate_patterns_planned(repo, plan)
            .with_context(|| "regenerating patterns/definitions.owl")?;
    } else if let Some(dosdp) = plan.dosdp.as_ref() {
        write_pattern_terms_from_definitions(repo, dosdp)
            .with_context(|| "extracting the pattern seed from the committed definitions.owl")?;
    }

    // The cached per-product import path is shown as one numbered stage each;
    // the `fresh`/merged paths are a single bulk step.
    let stage_imports =
        matches!(opts.imports_mode, ImportsMode::Cached) && plan.merged_import.is_none();
    let total = buildable.len() + if stage_imports { plan.imports.len() } else { 0 };
    let mut idx = 0usize;

    // --- Imports -----------------------------------------------------------
    if stage_imports {
        for imp in &plan.imports {
            idx += 1;
            let (head, detail) = imp.describe(&repo.dir);
            let stage = Stage::start(idx, total, &head, &detail, None);
            let res = (|| -> Result<()> {
                if !repo.dir.join(&imp.output).exists() {
                    build_one_import(repo, plan, imp, &catalog, &tmp)?;
                }
                Ok(())
            })();
            match res {
                Ok(()) => stage.finish_ok(),
                Err(e) => {
                    stage.finish_err();
                    return Err(e).with_context(|| format!("import `{}`", imp.id));
                }
            }
        }
    } else {
        prepare_imports(repo, plan, opts)?;
    }

    // --- Prerequisites -----------------------------------------------------
    // Generated files the artefacts depend on: plugin installs, filter seeds
    // (`tmp/simple_seed.txt`), tag subsets, generated SSSOM components. These come
    // from `plan.prerequisites`, resolved once at plan time, so nothing here reads
    // anything but the plan. They are built in the order planned (dependencies
    // first).
    //
    // A failure is fatal. As a warning the run carried on past, it would let CL
    // ship `cl-simple.owl` and `cl-basic.owl` built without their filter seed —
    // 20 MB wrong, with the build reporting success and nothing saying so.
    // Indexed over the artefacts TOO, in `Repo::target`'s own order: an artefact
    // is a perfectly ordinary prerequisite of another target, and one missing from
    // this map is silently not built — `ensure_prerequisite` finds no entry and
    // returns. `tmp/merged-hp-edit.ofn` is an artefact of its own AND the input of
    // `tmp/ontologyterms.txt`: absent from the map, the seed's build finds it
    // missing and fabricates it by converting its input verbatim, leaving a file
    // that is both wrong and newer than its input — which its real `remove
    // --select imports` + `merge` step then skips as up to date.
    // Artefacts first so a same-named prerequisite entry overwrites it, keeping the
    // precedence `Repo::target` uses (prerequisites, then artefacts).
    let prereq_index: std::collections::HashMap<&str, &crate::plan::ArtefactPlan> = plan
        .artefacts
        .iter()
        .chain(plan.prerequisites.iter())
        .filter(|a| !a.missing_rule)
        .map(|p| (p.target.as_str(), p))
        .collect();
    let mut prereq_done: std::collections::HashSet<String> = std::collections::HashSet::new();

    // --- Release artefacts -------------------------------------------------
    let mut failed: Vec<String> = Vec::new();
    for &i in &buildable {
        idx += 1;
        let a = &plan.artefacts[i];
        let (head, detail) = crate::plan::describe_artefact(a);
        let stage = Stage::start(idx, total, &head, &detail, None);
        let out = opts.output_dir.join(&a.target);
        // A pin outranks everything below, the prerequisite walk included:
        // under `MIR=false` the rule that would rebuild this artefact
        // does not exist, so the file on disk stands however stale its
        // prerequisites look — and nothing may fetch those prerequisites on its
        // behalf. UBERON's `../mappings/biomappings.sssom.tsv` is a release
        // artefact whose recipe (a live re-fetch and filter) is defined inside
        // `ifeq ($(strip $(MIR)),true)`; unpinned, every `MIR=false` build
        // shipped a fresh fetch of a mapping set the reference left committed.
        // A pinned file that is absent is an error naming the switch.
        let pinned = pinned_by(repo, &a.target).filter(|p| {
            // A DEFAULT pin holds only for a file that exists — see
            // `run_target_recipe_inner`, which decides the same question for a
            // recipe target. An explicit pin holds either way.
            p.explicit || repo.dir.join(&a.target).is_file()
        });
        if let Some(switch) = pinned {
            if repo.dir.join(&a.target).is_file() {
                status!("make: `{}` pinned ({switch})", a.target);
            } else {
                // An absent pinned INTERMEDIATE is simply never visited: under
                // the switch its rule does not exist, and nothing here demands
                // it — a consumer that does reaches `ensure_prerequisite`,
                // which raises the error naming the switch. Failing this stage
                // instead turned a fetch nothing needed into a failed release.
                status!("make: `{}` pinned ({switch}), absent — nothing demands it", a.target);
            }
            stage.finish_ok();
            continue;
        }
        // Resolve this artefact's prerequisites now rather than in one pass up
        // front: a prerequisite may itself depend on an *artefact*
        // (`subsets/%-tags.ofn` is built from `cl-full.owl`), so it can only be
        // built once the artefacts ahead of it in the plan exist.
        let prereqs = (|| -> Result<()> {
            for need in &a.needs {
                ensure_prerequisite(
                    repo,
                    need,
                    &prereq_index,
                    &mut prereq_done,
                    &catalog,
                    &tmp,
                    opts,
                )?;
            }
            Ok(())
        })();
        if let Err(e) = prereqs {
            stage.finish_err();
            // `-k` applies to a PREREQUISITE failure exactly as it does to a
            // recipe failure below. Returning unconditionally here meant one
            // unbuildable artefact still took the whole release down: UBERON's
            // `composite-metazoan-basic.owl` fails, and the run died on the next
            // target that needs it — abandoning the 111 subset artefacts that do
            // not depend on it at all. GNU make `-k` builds them.
            if !opts.keep_going {
                return Err(e).with_context(|| format!("building {}", a.target));
            }
            status!("make: *** [{}] {e:#}", a.target);
            repo.failed.borrow_mut().insert(a.target.clone());
            failed.push(a.target.clone());
            continue;
        }
        // The staleness rule: a target that already exists and is NEWER than
        // every one of its prerequisites is up to date, and its recipe is not run.
        //
        // On a fresh clone this changes nothing — every generated prerequisite is
        // rebuilt first and so is newer. It matters for a COMMITTED artefact whose
        // prerequisite arrives older, which is exactly MONDO's
        // `reports/mondo_base_last_release-report.tsv`: its input
        // `tmp/mondo-lastbase.owl` is downloaded and `wget` stamps the server's
        // `Last-Modified` (months ago) onto it, so the committed report stands and
        // the release ships that. Rebuilding it produces a different report, and
        // with it two different release-diff TSVs.
        // A target's recipe runs at most ONCE per invocation, however many goals
        // reach it. HPO's `test` phase gets there first for two targets:
        // `test_obo` writes `hp.obo` as a side effect (`grep -v ^owl-axioms
        // test.tmp.obo > hp.obo`) and the OWL2 DL profile check builds `hp.owl`.
        // The build then arrives at `all_assets` with both already made, finds
        // `hp.obo` NEWER than `hp.owl`, and ships the `test_obo` product as the
        // release `hp.obo`.
        //
        // Rebuilding `hp.owl` here would invert that mtime pair, firing the
        // ordinary `$(ONT).obo: $(ONT).owl` rule over it — the release would then
        // carry `ontology: hp.obo` instead of `ontology: test_obo`, a stray
        // `owl:versionInfo`, and 16 Typedef stanzas `test.owl`'s filtered term
        // list excludes.
        if memo_has(repo, &a.target) {
            stage.finish_ok();
            continue;
        }
        // A prerequisite that FAILED is not a prerequisite that is merely absent.
        // Both look identical to the staleness test below — no file to stat — and
        // reading the failure as "not newer" declares the target up to date, so
        // whatever bytes happen to be on disk are published as this run's output.
        // EFO's `components/legal_diseases.txt` is the case: its input
        // `disease_to_phenotype_merged.owl` cannot be built (its own upstream
        // serves a 404 page where an ontology should be), and om reported the
        // target up to date and kept a file from a previous run. GNU make says
        // `Target 'x' not remade because of errors`, and P5 says the same: a
        // declared file that is missing is an error, not a filter.
        if let Some(bad) = a.needs.iter().find(|n| repo.failed.borrow().contains(*n)) {
            stage.finish_err();
            let e = anyhow::anyhow!(
                "not remade because of errors: prerequisite `{bad}` failed in this run"
            );
            if !opts.keep_going {
                return Err(e).with_context(|| format!("building {}", a.target));
            }
            status!("make: *** [{}] {e:#}", a.target);
            repo.failed.borrow_mut().insert(a.target.clone());
            failed.push(a.target.clone());
            continue;
        }
        // A phony target names no file, so it is out of date however old the file
        // that happens to share its name is.
        if !plan.is_phony(&a.target)
            && is_up_to_date(&out, &a.needs, &a.order_only, a.input.as_deref(), &opts.output_dir)
        {
            stage.finish_ok();
            continue;
        }
        // Every artefact goes through the one step pipeline. A shell step
        // (perl/grep/sed, an `if`/`for` construct, a `jq`/`sssom` call, …) runs
        // where it sits, so an otherwise-native artefact keeps the in-memory path
        // for every step around it.
        match run_artefact(repo, a, &catalog, &tmp, &opts.output_dir, &out) {
            Ok(()) => {
                mirror_into_ontology_dir(repo, &a.target, &out);
                repo.built.borrow_mut().insert(a.target.clone());
                stage.finish_ok()
            }
            Err(e) => {
                stage.finish_err();
                if !opts.keep_going {
                    return Err(e).with_context(|| format!("building {}", a.target));
                }
                status!("make: *** [{}] {e:#}", a.target);
                repo.failed.borrow_mut().insert(a.target.clone());
                failed.push(a.target.clone());
            }
        }
    }
    if !failed.is_empty() {
        bail!("{} target(s) failed: {}", failed.len(), failed.join(", "));
    }

    // Note any artefacts the plan couldn't cover (already reported as gaps).
    for &i in &order {
        let a = &plan.artefacts[i];
        if a.missing_rule || !a.gaps.is_empty() {
            status!("make: skipping {} (not fully covered)", a.target);
        }
    }
    Ok(())
}

/// Topologically order artefact indices so every artefact is built after the
/// artefacts that produce what it needs. Handles chains deeper than one
/// (e.g. `mondo-international.owl ⟵ mondo.owl ⟵ reasoned.owl`) and inputs shared
/// by several artefacts (`reasoned.owl` feeds both `mondo.owl` and
/// `mondo-base.owl`).
///
/// EVERY prerequisite counts, not just the pipeline input. `ensure_prerequisite`
/// only knows about `plan.prerequisites`, so an artefact needed by another
/// artefact has to be ordered ahead of it here or it is never built in time:
/// MONDO's `reports/mondo_release_diff_changed_terms.tsv` takes
/// `reports/mondo_base_last_release-report.tsv` as its `$<` and
/// `reports/mondo_obsoletioncandidates.tsv` — an artefact in its own right —
/// only as a further prerequisite, so following `input` alone leaves the python
/// script reading a file that does not exist yet.
fn artefact_order(plan: &Plan) -> Vec<usize> {
    let n = plan.artefacts.len();
    // A prerequisite names an artefact when it IS that target, or ends with it
    // after a path separator — never on a bare suffix, which would tie
    // `imports/x.owl` to an unrelated `x.owl`.
    let names = |need: &str, target: &str| {
        need == target || need.strip_suffix(target).is_some_and(|p| p.ends_with('/'))
    };
    // deps[i] = every artefact producing something artefact i needs.
    let deps: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            let a = &plan.artefacts[i];
            let mut v: Vec<usize> = a
                .needs
                .iter()
                .chain(a.input.iter())
                .filter_map(|need| {
                    plan.artefacts
                        .iter()
                        .position(|b| b.target != a.target && names(need, &b.target))
                })
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect();

    let mut state = vec![0u8; n]; // 0=unvisited, 1=on-stack, 2=done
    let mut order = Vec::with_capacity(n);
    for s in 0..n {
        if state[s] == 2 {
            continue;
        }
        state[s] = 1;
        // (artefact, how many of its dependencies have been pushed)
        let mut stack: Vec<(usize, usize)> = vec![(s, 0)];
        while let Some((i, k)) = stack.pop() {
            if let Some(&d) = deps[i].get(k) {
                stack.push((i, k + 1));
                // state[d]==1 → dependency cycle; leave it to be emitted by the
                // frame already on the stack rather than looping.
                if state[d] == 0 {
                    state[d] = 1;
                    stack.push((d, 0));
                }
                continue;
            }
            state[i] = 2;
            order.push(i);
        }
    }
    order
}

fn prepare_imports(repo: &Repo, plan: &Plan, opts: &ExecOpts) -> Result<()> {
    match opts.imports_mode {
        ImportsMode::Cached => {
            // Prefer the committed import modules in place. Any the release needs
            // but that are not committed (the `imports/*_import.owl` are build
            // artefacts and often git-ignored) are built on demand here —
            // download the product's mirror and run its pipeline — rather than
            // forcing the user to re-mirror *every* import with `--imports fresh`.
            let catalog = load_catalog_planned(repo);
            let work = opts.output_dir.join(".owlmake-odk-tmp");
            std::fs::create_dir_all(&work)?;
            for imp in &plan.imports {
                let p = repo.dir.join(&imp.output);
                if !p.exists() && plan.merged_import.is_none() {
                    eprintln!(
                        "import: cached module {} missing ({}); building it from upstream",
                        imp.id,
                        p.display()
                    );
                    build_one_import(repo, plan, imp, &catalog, &work).with_context(|| {
                        format!("building missing import module `{}`", imp.id)
                    })?;
                }
            }
            if let Some(m) = &plan.merged_import {
                let p = repo.dir.join(m);
                if !p.exists() {
                    bail!("cached merged import {} missing; use --imports fresh", p.display());
                }
            }
            Ok(())
        }
        // `Fresh` is a FRESHNESS REQUEST over the `imports` group, not a choice
        // of builder. The plan's own targets come first: whatever the repo
        // recorded in `ImportPlan::steps` is what runs, so a repo that spells its
        // own import recipe out (EFO extracts a BOT module and then filters it)
        // gets that module rather than a synthesized one under the source
        // ontology's IRI.
        //
        // `build_imports_fresh` synthesizes the standard mirror→module pipeline
        // from each product's flags alone, and so ignores every recorded step. It
        // is the fallback for a plan whose imports carry no pipeline of their own,
        // where there is nothing to replay.
        ImportsMode::Fresh => rebuild_imports_from_plan(repo, plan, opts),
    }
}

/// Rebuild the `imports` group from the PLAN.
///
/// Each member is built the way the plan says to build it: an entry that is one
/// of the plan's import products runs that product's recorded pipeline
/// (`ImportPlan::steps`, via [`build_one_import`]); anything else in the group is
/// an ordinary planned target and is replayed as one. Only when the plan records
/// no import pipeline at all does the synthesized builder run.
fn rebuild_imports_from_plan(repo: &Repo, plan: &Plan, opts: &ExecOpts) -> Result<()> {
    // A repo whose own rules build the import chain: replay them.
    if has_import_rules(repo) {
        return refresh_imports_planned(repo, plan, false);
    }

    let recorded = plan.imports.iter().any(|i| !i.steps.is_empty());
    if !recorded {
        // Nothing to replay — the products carry only their configured flags, so
        // the standard mirror→module pipeline is synthesized from them. This is
        // the ONE case that builder is for, and saying so is the point of the
        // branch.
        return build_imports_fresh(repo, plan, false, opts.refresh_mirrors);
    }

    let catalog = load_catalog_planned(repo);
    let work = opts.output_dir.join(".owlmake-odk-tmp");
    std::fs::create_dir_all(&work)?;

    let group: Vec<String> = plan
        .refresh_groups
        .iter()
        .find(|g| g.name == "imports")
        .map(|g| g.targets.clone())
        .unwrap_or_default();
    // With a MERGED import configured, the per-import modules are not inputs to
    // anything: `imports/merged_import.owl` is extracted from the merged MIRROR
    // over the whole seed, and HPO says so directly — `IMPORT_ROOTS =
    // $(IMPORTDIR)/merged_import`. Rebuilding them anyway reaches HPO's bespoke
    // `imports/nbo_import.owl` rule, whose `imports/nbo_terms_combined.txt`
    // prerequisite does not exist and has no rule — nothing builds that module,
    // because nothing consumes it. Refresh them only when they are the product.
    // Matched on the file NAME: the plan spells the merged import repo-relative
    // while the refresh group carries the normalised path, so comparing the two
    // verbatim would skip every target, the wanted one included.
    let merged_name = plan
        .merged_import
        .as_deref()
        .and_then(|m| Path::new(m).file_name().map(|s| s.to_os_string()));
    let mut seen = std::collections::HashSet::new();
    for target in &group {
        if merged_name.is_some()
            && Path::new(target).file_name().map(|s| s.to_os_string()) != merged_name
        {
            continue;
        }
        match plan.imports.iter().find(|i| &i.output == target) {
            // The merged module is no product's output — it is the whole merged
            // pipeline (merge every mirror, drop the excluded IRIs, extract the
            // ⊥-module over the union seed). Without this arm the filtered group
            // matches nothing that can be built and `--rebuild imports` leaves
            // `imports/merged_import.owl` exactly as it found it.
            None if merged_name.is_some() => {
                return build_imports_fresh(repo, plan, false, opts.refresh_mirrors)
            }
            Some(imp) => build_one_import(repo, plan, imp, &catalog, &work)
                .with_context(|| format!("rebuilding import module `{}`", imp.id))?,
            // A group member the plan builds as an ordinary target (EFO records
            // fourteen of these).
            None if repo.target(target).is_some() => {
                run_target_recipe_inner(repo, target, &mut seen)?
            }
            None => {}
        }
    }
    // Products the group did not name (a plan that records no `refresh_groups`,
    // or an import with no rule of its own) still have to be built — except under
    // base merging, where the group names the merged module alone precisely
    // because no per-product module is ever written.
    for imp in &plan.imports {
        if merged_name.is_none() && !group.contains(&imp.output) {
            build_one_import(repo, plan, imp, &catalog, &work)
                .with_context(|| format!("rebuilding import module `{}`", imp.id))?;
        }
    }
    Ok(())
}

/// `refresh-imports` (and `refresh-imports-excluding-large`): rebuild the import
/// modules from upstream. owlmake mirrors and extracts in-process, so nothing but
/// a repo's own scripts ever leaves the process. `exclude_large` skips the
/// products the plan flags as large.
fn refresh_imports_planned(repo: &Repo, plan: &Plan, exclude_large: bool) -> Result<()> {
    // The repo's own import chain (`all_imports` → `imports/*_import.owl` →
    // `mirror/merged.owl` → `mirror-<id>`) is replayed from the plan, which
    // recorded each module's real pipeline at ingest (`ImportPlan::steps`). The
    // synthesized path below is only for a plan whose imports carry no recipe of
    // their own: it seeds the module from the committed `*_terms.txt` plus the
    // edit signature rather than from a recipe's `merged_terms_combined.txt`, and
    // runs no `remove --select` chain, so it approximates a recipe the plan does
    // not have.
    if has_import_rules(repo) {
        // `refresh-imports-excluding-large` is spelled in a repo's own rules as an
        // `IMP_LARGE` conditional the mirror recipes test. Ingest flattens those
        // conditionals, so the exclusion is applied here instead — over the
        // plan's own `is_large` flag, against the module each sub-target builds.
        let large: Vec<&str> = plan
            .imports
            .iter()
            .filter(|i| i.product.as_ref().is_some_and(|p| p.is_large))
            .map(|i| i.output.as_str())
            .collect();
        let mut seen = std::collections::HashSet::new();
        let catalog = load_catalog_planned(repo);
        let work = repo.output_dir.join(".owlmake-odk-tmp");
        std::fs::create_dir_all(&work)?;
        for sub in &repo.target("all_imports").expect("checked by has_import_rules").needs {
            if exclude_large && large.contains(&sub.as_str()) {
                status!("import: skipping large import `{sub}`");
                continue;
            }
            // A module the plan records as an import PRODUCT is built by the
            // product's own pipeline (`ImportPlan::steps`): that is the pipeline
            // `--plan-only` shows and the one a curator edits. The plan may also
            // carry the replayed Makefile rule for the same file. The two agree on
            // the day the plan is generated and diverge the moment the product is
            // edited — EFO set its OBA filter to `trim: false` in the product and
            // the rebuilt module came out byte-identical, because this loop
            // replayed the recorded rule and never ran the product's steps.
            if let Some(imp) = import_module_for(plan, sub) {
                if seen.insert(sub.clone()) && !memo_has(repo, sub) {
                    build_one_import(repo, plan, imp, &catalog, &work)
                        .with_context(|| format!("rebuilding import module `{}`", imp.id))?;
                    repo.built.borrow_mut().insert(sub.clone());
                }
                continue;
            }
            run_target_recipe_inner(repo, sub, &mut seen)?;
        }
        return Ok(());
    }
    // `refresh-imports` IS the request to re-mirror from upstream, so the mirrors
    // are refreshed by definition here.
    //
    // A plan whose products carry their own recorded pipelines but no replayed
    // rules — EFO once its duplicated import targets are gone — is built from
    // those pipelines: the synthesized builder is only for products that carry
    // none, as the comment above says.
    if plan.imports.iter().any(|i| !i.steps.is_empty()) {
        let catalog = load_catalog_planned(repo);
        let work = repo.output_dir.join(".owlmake-odk-tmp");
        std::fs::create_dir_all(&work)?;
        for imp in &plan.imports {
            if exclude_large && imp.product.as_ref().is_some_and(|p| p.is_large) {
                status!("import: skipping large import `{}`", imp.output);
                continue;
            }
            if imp.steps.is_empty() {
                continue;
            }
            // Re-mirrored by definition — unless the same command line pinned the
            // mirrors (`MIR=false`, ODK's `no_mirror_refresh_imports`), which
            // outranks the request exactly as a pinned import does above.
            ensure_mirror(repo, imp, !repo.mirrors_pinned)?;
            build_one_import(repo, plan, imp, &catalog, &work)
                .with_context(|| format!("rebuilding import module `{}`", imp.id))?;
            repo.built.borrow_mut().insert(imp.output.clone());
        }
        return Ok(());
    }
    build_imports_fresh(repo, plan, exclude_large, true)
}

/// Does the plan record the import chain as buildable targets? Rebuilding
/// `all_imports` is only meaningful when the aggregate has prerequisites and the
/// plan knows how to build each of them.
fn has_import_rules(repo: &Repo) -> bool {
    let Some(agg) = repo.target("all_imports") else { return false };
    !agg.needs.is_empty() && agg.needs.iter().all(|p| repo.target(p).is_some())
}

/// Find the edit ontology (the DOSDP pipeline's `--ontology=EDIT_PREPROCESSED`).
fn find_edit_ontology(repo: &Repo, plan: &Plan) -> Result<PathBuf> {
    if let Some(p) = edit_file(repo) {
        return Ok(p);
    }
    for ext in ["obo", "owl", "ofn", "ttl", "omn"] {
        let p = repo.dir.join(format!("{}-edit.{ext}", plan.id));
        if p.exists() {
            return Ok(p);
        }
    }
    bail!("no edit ontology found for `{}` (looked for {}-edit.*)", plan.id, plan.id);
}

/// `patterns` (`PAT=true`): regenerate `patterns/definitions.owl` from the repo's
/// DOSDP patterns + their `data/default/*.tsv` tables, using owlmake's own dosdp
/// engine. Per-pattern `generate` (with the edit ontology supplying labels /
/// permutation values) → merge → annotate(ontology+version IRI). Returns
/// `Ok(false)` when the repo has no DOSDP pattern set (nothing to do).
fn regenerate_patterns_planned(repo: &Repo, plan: &Plan) -> Result<bool> {
    // The pattern SET comes from the plan, never from an `is_dir()` probe of
    // `../patterns/{dosdp-patterns,data/default}` and a `read_dir`: a repo whose
    // layout differed would then silently build nothing while the step reported
    // success. `None` means the plan makes no statement about patterns; an empty
    // set means ingest looked and found none.
    let Some(dosdp) = plan.dosdp.as_ref() else {
        return Ok(false);
    };
    if dosdp.patterns.is_empty() {
        return Ok(false);
    }
    let names: Vec<String> = dosdp.patterns.iter().map(|p| p.name.clone()).collect();

    // `definitions.owl`'s prerequisites are the per-pattern modules beside each
    // data table, and each module's are its template and that table. owlmake
    // writes the modules below, so the rule carries its own up-to-date answer on
    // disk: a fresh clone has no module and regenerates, while a second request
    // inside one build finds every module older than the `definitions.owl` just
    // written and leaves it alone.
    //
    // Both halves matter. A build reaches the patterns twice — once for the
    // import seed, once for the release — and regenerating the second time reads
    // an import closure the first pass did not have, so a filler whose label
    // lives in the refreshed import resolves where it did not before. The
    // release is built from the first file and the published asset is the
    // second, which is a release that does not match its own inputs.
    let out = repo.dir.join(&dosdp.output);
    let modules: Vec<PathBuf> =
        dosdp.patterns.iter().map(|p| repo.dir.join(&p.data).with_extension("ofn")).collect();
    let modules_current = modules.iter().zip(&dosdp.patterns).all(|(m, p)| {
        m.exists()
            && is_newer_than_all(m, &[repo.dir.join(&p.template), repo.dir.join(&p.data)])
    });
    if !repo.always_make && modules_current && is_newer_than_all(&out, &modules) {
        status!("make: `{}` is up to date", out.display());
        // The seed files live under `tmp/`, which a build may have cleaned, so
        // they are written whether or not the definitions were.
        write_pattern_seed_files(repo, dosdp)?;
        return Ok(true);
    }

    status!("make: regenerating patterns/definitions.owl from {} DOSDP pattern(s)", names.len());
    // Labels + the permutation annotation index come from the edit ontology AND
    // its import closure (resolved via the catalog) — the fillers' labels live in
    // the imports, so without the closure they cannot be resolved at all.
    let edit = find_edit_ontology(repo, plan)?;
    let catalog = load_catalog_planned(repo);
    let edit_model = crate::io::load(&edit)?;
    let closure = load_closure(&edit_model, &repo.dir, &catalog)?;
    let mut sources: Vec<&crate::model::Model> = vec![&edit_model];
    if let Some(c) = &closure {
        sources.push(c);
    }
    // `$(OTHER_SRC)` too — for OBA that is `patterns/definitions.owl` itself, and
    // feeding it in is how a filler whose only label a DOSDP pattern generated
    // resolves at all: `OBA:0000099` has no `name:` in the edit file, and
    // "membrane potential trait" is its own `defined_class_name` from a pattern
    // row. Reading only the edit ontology and its catalog closure writes the bare
    // IRI into every definition and synonym that references such a class.
    // The pattern step is what WRITES `definitions.owl`, so it cannot also
    // require it — the one legitimate exception to "declared means required"
    // (a step cannot require what it exists to produce).
    let definitions = plan.dosdp.as_ref().map(|d| d.output.clone()).unwrap_or_default();
    let other_models: Vec<crate::model::Model> =
        other_src(repo, &[definitions.as_str()], &repo.dir.join(".owlmake-odk-tmp"))?
            .into_iter()
            .filter_map(|p| crate::io::load(&p).ok())
            .collect();
    sources.extend(other_models.iter());
    let (labels, index) = crate::dosdp::ontology_context_from_models(&sources);
    // The repo's custom CURIE prefixes, read from the prefix files the PLAN names
    // (`config/prefixes.yaml` and the like).
    // A named file that cannot be read or parsed is an error: the generated
    // definitions would silently carry bare IRIs instead of CURIEs.
    let mut extra_prefixes: Vec<(String, String)> = Vec::new();
    for pf in &dosdp.prefixes {
        let pfile = repo.dir.join(pf);
        let text = std::fs::read_to_string(&pfile)
            .with_context(|| format!("reading DOSDP prefixes {}", pfile.display()))?;
        let map: std::collections::BTreeMap<String, String> = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing DOSDP prefixes {}", pfile.display()))?;
        extra_prefixes.extend(map);
    }
    let mut defs = empty_model();
    // Each data DIRECTORY is one generator invocation, and each invocation numbers
    // its own modules from `urn:unnamed:ontology#ont1`. HPO runs two — `data/default`
    // restricted to logical axioms, `data/full` unrestricted — and merges both.
    let mut batch_dir: Option<std::path::PathBuf> = None;
    let mut batch_index = 0usize;
    for pattern in dosdp.patterns.iter() {
        let dir_of = Path::new(&pattern.data).parent().map(Path::to_path_buf);
        if batch_dir.as_deref() != dir_of.as_deref() {
            batch_dir = dir_of;
            batch_index = 0;
        }
        batch_index += 1;
        let i = batch_index - 1;
        // The generator's options are the PLAN's, not this executor's defaults:
        // `--restrict-axioms-to logical` is why MP's `definitions.owl` carries
        // 3,052 equivalences and not one annotation assertion.
        let gopts = crate::dosdp::GenerateOptions {
            annotation_index: index.clone(),
            extra_prefixes: extra_prefixes.clone(),
            restrict_axioms: pattern
                .restrict_axioms
                .as_deref()
                .map(crate::dosdp::Restrict::parse)
                .unwrap_or_default(),
            restrict_axioms_column: pattern.restrict_axioms_column.clone(),
            add_axiom_source_annotation: pattern.add_axiom_source_annotation,
            axiom_source_annotation_property: pattern.axiom_source_annotation_property.clone(),
            generate_defined_class: pattern.generate_defined_class,
            ..Default::default()
        };
        let name = &pattern.name;
        let pat = std::fs::read_to_string(repo.dir.join(&pattern.template))
            .with_context(|| format!("reading DOSDP template {}", pattern.template))?;
        let data = std::fs::read_to_string(repo.dir.join(&pattern.data))
            .with_context(|| format!("reading DOSDP data {}", pattern.data))?;
        let m = crate::dosdp::generate_with(&pat, &data, &labels, &gopts)
            .with_context(|| format!("dosdp pattern `{name}`"))?;
        // The per-pattern module, beside its data table. It is what
        // `definitions.owl` is merged from, so it belongs on disk: the
        // up-to-date test above has nothing to compare against otherwise, and a
        // repo that inspects the modules after a build finds them.
        //
        // Each module is an unnamed ontology numbered by its position in the
        // batch — `urn:unnamed:ontology#ont1` for the first — and, like the
        // prototype beside it, spells `^^xsd:string` out where the definitions
        // leave it implicit. The IRI does not reach `definitions.owl`: that is
        // annotated with its own, so the module keeps its own identity here.
        let iri = format!("urn:unnamed:ontology#ont{}", i + 1);
        let mut module = crate::cmd::annotate::annotate(
            crate::dosdp::typed_as_xsd_string(m.clone()),
            Some(&iri),
            None,
            &[],
            &[],
            false,
        )?;
        // `:` is the module's own IRI VERBATIM. The default the writer derives
        // from an ontology IRI carries a trailing `#`, so the binding is put in
        // first and the derived set copied over it minus its own `:` — and the
        // model is marked as carrying its own prefixes, which the generator's
        // output is not, or the writer derives the set again and the `#` returns.
        let mut pm = horned_owl::curie::PrefixMapping::default();
        let _ = pm.add_prefix("", &iri);
        for (p, ns) in crate::io::robot_ofn_prefixes(&module).mappings() {
            if !p.is_empty() {
                let _ = pm.add_prefix(p, ns);
            }
        }
        module.prefixes = pm;
        module.format_prefixes_cleared = false;
        horned_owl::io::ofn::writer::set_write_xsd_string(true);
        let wrote = crate::io::save_as(
            &mut module,
            &repo.dir.join(&pattern.data).with_extension("ofn"),
            crate::io::Format::Functional,
        );
        horned_owl::io::ofn::writer::set_write_xsd_string(false);
        wrote.with_context(|| format!("writing the `{name}` pattern module"))?;
        merge_model_into(&mut defs, m);
    }

    // What the repo does with the merged products — its own `definitions.owl`
    // pipeline, `dosdp.steps`. The two IRI stamps are in there; so is any
    // post-processing the repo adds, such as OBA's `query --update`.
    let steps: Vec<crate::plan::step::Step> =
        dosdp.steps.iter().cloned().map(crate::spec::StepEntry::into_step).collect();
    let work = repo.dir.join(".owlmake-odk-tmp");
    std::fs::create_dir_all(&work).ok();
    let mut defs = run_steps(repo, &steps, defs, &catalog, &work, Some(&dosdp.output), true, None)?;
    // `definitions.owl` declares only the default prefixes (`:` bound to the
    // ontology IRI plus `owl`/`rdf`/`xml`/`xsd`/`rdfs`), so OBO entity IRIs render
    // in full. That is the shape released pattern files carry; changing it would
    // rewrite every line of every release diff.
    defs.prefixes = crate::io::robot_ofn_prefixes(&defs);

    // The rule writes `-o definitions.ofn && mv definitions.ofn definitions.owl`,
    // so the released `definitions.owl` is OWL **Functional Syntax** under a `.owl`
    // name — not RDF/XML. This matters beyond byte-format: a DOSDP pattern can emit both a
    // plain annotation assertion and an identically-valued *axiom-annotated* one
    // (e.g. an `exact_synonym` whose `value:` column coincides with a computed
    // synonym carrying an `xref`), and RDF/XML's reification model cannot represent
    // both — the plain triple is absorbed into the annotated axiom on round-trip.
    // Functional syntax keeps them distinct, so both survive into the release.
    // The plan names the output too, so it is not re-derived from a convention.
    let out = repo.dir.join(&dosdp.output);
    crate::io::save_as(&mut defs, &out, crate::io::Format::Functional)?;
    // The staged write the pipeline ends on is the artefact on its way to its
    // published name — `definitions.ofn` in `src/ontology`, renamed to
    // `../patterns/definitions.owl`. The rename is a MOVE: nothing is left at the
    // source. Writing the destination and leaving the staged file behind puts a
    // second copy of the artefact in the ontology directory, under a name nothing
    // in the plan ever reads again.
    for path in staged_writes(&steps) {
        let staged = repo.dir.join(&path);
        if staged != out {
            let _ = std::fs::remove_file(&staged);
        }
    }
    write_pattern_seed_files(repo, &dosdp)?;
    Ok(true)
}

/// Every path a step stages the model through, branches included.
fn staged_writes(steps: &[crate::plan::step::Step]) -> Vec<String> {
    use crate::plan::step::{Op, Step};
    let mut out = Vec::new();
    for step in steps {
        match step.effective() {
            Step::Branch { then_steps, else_steps, .. } => {
                out.extend(staged_writes(then_steps));
                out.extend(staged_writes(else_steps));
            }
            Step::Op(Op::RoundTrip { path, .. })
            | Step::Partial { op: Op::RoundTrip { path, .. }, .. } => {
                out.push(path.clone())
            }
            _ => {}
        }
    }
    out
}

/// The pattern SEED files, written natively alongside `definitions.owl`.
///
/// owlmake owns the DOSDP engine, so it owns these too — and having removed their
/// rules from the plan (`native_pattern_targets`) it has to produce them, because
/// the import seed is `cat $(PRESEED) $(TMPDIR)/all_pattern_terms.txt | sort |
/// uniq` and the import ⊥-module is extracted over the result. Without them the
/// seed loses every term the PATTERNS name — 317 on HPO — and
/// `merged_import.owl` comes out ~62,000 lines short.
///
/// `pattern_owl_seed.txt` is CSV query output, so it carries the `term` header and
/// CRLF line endings; `all_pattern_terms.txt` is a plain `cat | sort | uniq` of
/// the per-pattern term files and that one.
fn write_pattern_seed_files(repo: &Repo, dosdp: &crate::spec::DosdpSpec) -> Result<()> {
    let Some(dir) = Path::new(&dosdp.output).parent().map(|p| repo.dir.join(p)) else {
        return Ok(());
    };
    let tmp = repo.tmp_dir();
    std::fs::create_dir_all(&tmp).ok();

    // `pattern_owl_seed.txt` is `query --use-graphs true -f csv -i pattern.owl
    // --query ../sparql/terms.sparql`, so RUN that query rather than
    // approximating it with the signature: the row ORDER is the query engine's,
    // not lexicographic, and the output is a CSV with a `term` header and CRLF
    // endings. `pattern.owl` itself is the DOSDP prototype of the whole template
    // directory, merged under `urn:unnamed:ontology#ont1`.
    //
    // The template SET is the plan's (`dosdp.templates` — every yaml in the
    // directory, a superset of the patterns that have a data table), not a
    // `read_dir` of the directory.
    let mut yamls: Vec<PathBuf> = if dosdp.templates.is_empty() {
        dosdp.patterns.iter().map(|p| repo.dir.join(&p.template)).collect()
    } else {
        dosdp.templates.iter().map(|t| repo.dir.join(t)).collect()
    };
    yamls.sort();
    // The up-to-date rule governs the native pattern path too.
    // `$(PATTERNDIR)/pattern.owl: $(ALL_PATTERN_FILES)` — in a fresh clone every
    // file carries the checkout mtime, so no template is NEWER than the committed
    // `pattern.owl` and it stands. Rewriting it anyway publishes a different
    // release artefact and — because `tmp/pattern_owl_seed.txt` is a query over
    // it — puts 48 further terms into the import seed, which moves every one of
    // the nine import modules.
    let pattern_owl = dir.join("pattern.owl");
    let mut proto = if !repo.always_make && is_newer_than_all(&pattern_owl, &yamls) {
        status!("make: `{}` is up to date", pattern_owl.display());
        crate::io::load(&pattern_owl)
            .with_context(|| format!("reading {}", pattern_owl.display()))?
    } else {
        let labels = std::collections::HashMap::new();
        let mut proto = crate::model::Model::default();
        for y in &yamls {
            let Ok(text) = std::fs::read_to_string(y) else { continue };
            let Ok(m) = crate::dosdp::prototype(&text, &labels) else { continue };
            merge_model_into(&mut proto, m);
        }
        let mut proto = crate::cmd::annotate::annotate(
            proto,
            Some("urn:unnamed:ontology#ont1"),
            None,
            &[],
            &[],
            false,
        )?;
        proto.prefixes = crate::io::robot_ofn_prefixes(&proto);
        let _ = proto.prefixes.add_prefix("", "urn:unnamed:ontology#ont1");
        // FUNCTIONAL syntax, whatever the `.owl` extension says: the prototype
        // ontology is written in functional syntax, so the committed
        // `pattern.owl` opens `Prefix(:=<urn:unnamed:ontology#ont1>)`.
        //
        // And with `^^xsd:string` spelled out, which is how a prototype renders a
        // string literal even though the datatype is implicit everywhere else. The
        // switch is process-wide, so turn it off again immediately: the
        // `definitions.owl` written beside it must not gain the datatype. (`om
        // dosdp prototype` does the same thing on the CLI path; this is the build
        // path.)
        horned_owl::io::ofn::writer::set_write_xsd_string(true);
        let wrote = crate::io::save_as(&mut proto, &pattern_owl, crate::io::Format::Functional);
        horned_owl::io::ofn::writer::set_write_xsd_string(false);
        wrote?;
        // The seed is a query over the FILE (`query -i $(PATTERNDIR)/pattern.owl`),
        // and reading it back is not a formality: a template that declares no
        // `pattern_iri` is named by a bare filename, which the prototype holds as a
        // relative IRI and a reader resolves against the default prefix. Querying
        // the in-memory prototype would seed `entity_homeostasis_trait.yaml` where
        // the file yields `urn:unnamed:ontology#ont1entity_homeostasis_trait.yaml`.
        crate::io::load(&pattern_owl)
            .with_context(|| format!("reading {}", pattern_owl.display()))?
    };
    let _ = &mut proto;

    let sparql_dir = repo.dir.join(match repo.var("SPARQLDIR") {
        "" => "../sparql",
        v => v,
    });
    let query = std::fs::read_to_string(sparql_dir.join("terms.sparql"))
        .with_context(|| format!("reading {}", sparql_dir.join("terms.sparql").display()))?;
    let table = crate::sparql::Queryable::from_model(&proto)?.query_table(&query)?;
    let mut owl_seed = String::from("term\r\n");
    for row in &table.rows {
        if let Some(v) = row.first() {
            owl_seed.push_str(v);
            owl_seed.push_str("\r\n");
        }
    }
    write_target(&tmp.join("pattern_owl_seed.txt"), owl_seed.as_bytes())?;

    // Per-pattern term files, then the union. The pairs are the plan's
    // (`dosdp.patterns`), so which patterns contribute is a plan question rather
    // than whatever `read_dir` happens to find.
    let mut all: Vec<String> = owl_seed.lines().map(|l| format!("{l}\r")).collect();
    for pattern in &dosdp.patterns {
        let data = repo.dir.join(&pattern.data);
        let yaml = std::fs::read_to_string(repo.dir.join(&pattern.template))
            .with_context(|| format!("reading DOSDP template {}", pattern.template))?;
        let tsv = std::fs::read_to_string(&data)
            .with_context(|| format!("reading DOSDP data {}", pattern.data))?;
        let terms = crate::dosdp::terms(&yaml, &tsv)
            .with_context(|| format!("dosdp terms for `{}`", pattern.name))?;
        let body: String = terms.iter().map(|t| format!("{t}\n")).collect();
        write_target(&data.with_extension("txt"), body.as_bytes())?;
        all.extend(terms);
    }
    all.sort();
    all.dedup();
    let body: String = all.iter().map(|t| format!("{t}\n")).collect();
    write_target(&tmp.join("all_pattern_terms.txt"), body.as_bytes())?;
    Ok(())
}

/// Write `bytes` to `path` only when they are not already there.
///
/// A file whose content is what this build would write IS up to date, and giving
/// it a new modification time makes everything downstream of it stale. The pattern
/// seed files are derived from `patterns/definitions.owl` on every run, and
/// `tmp/seed.txt` — and the 44 MB merged import behind it — is built from them, so
/// rewriting them unconditionally means a second build of an unchanged tree
/// re-extracts the whole import module.
fn write_target(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).map(|old| old == bytes).unwrap_or(false) {
        return Ok(());
    }
    std::fs::write(path, bytes)
}

/// The pattern half of the import seed when patterns are NOT regenerated
/// (`PAT=false`).
///
/// `$(TMPDIR)/all_pattern_terms.txt` is still written in that case, derived from
/// the COMMITTED `definitions.owl` rather than from the per-pattern term files.
/// The two derivations do not agree, so which one runs decides the import seed;
/// with neither, `cat $(PRESEED) $(TMPDIR)/all_pattern_terms.txt` yields a seed
/// missing every pattern term, and uPheno's `merged_import.owl` comes out 2,155
/// lines short.
fn write_pattern_terms_from_definitions(repo: &Repo, dosdp: &crate::spec::DosdpSpec) -> Result<()> {
    let defs = repo.dir.join(&dosdp.output);
    if !defs.exists() {
        return Ok(());
    }
    // The query is the plan's: ingest read it off the rule the `PAT=false` branch
    // defines, so a plan-only repo still knows how the seed is derived. No
    // recorded query means the repo has no such rule and there is nothing to run.
    let Some(rel_query) = dosdp.cached_seed_query.as_deref() else {
        return Ok(());
    };
    let tmp = repo.tmp_dir();
    std::fs::create_dir_all(&tmp).ok();
    let qpath = repo.dir.join(rel_query);
    let query = std::fs::read_to_string(&qpath)
        .with_context(|| format!("reading {}", qpath.display()))?;
    let model = crate::io::load(&defs).with_context(|| format!("reading {}", defs.display()))?;
    let table = crate::sparql::Queryable::from_model(&model)?.query_table(&query)?;
    // `robot query -f csv` output: a `term` header and CRLF endings.
    let mut body = String::from("term\r\n");
    for row in &table.rows {
        if let Some(v) = row.first() {
            body.push_str(v);
            body.push_str("\r\n");
        }
    }
    write_target(&tmp.join("all_pattern_terms.txt"), body.as_bytes())?;
    Ok(())
}

/// `all_imports`: (re)build every individual `imports/<id>_import.owl` module the
/// release declares, from upstream mirrors.
fn build_all_imports_planned(repo: &Repo, plan: &Plan) -> Result<()> {
    // A repo with rules of its own is built exactly as `refresh-imports` builds
    // it: each module the plan records as an import PRODUCT runs the product's
    // pipeline, and anything else `all_imports` needs is replayed as the rule the
    // plan recorded. Replaying the `all_imports` target itself would walk its
    // prerequisites through the recorded rules alone and never run a product's
    // steps — so an edit to the product (EFO's `trim: false` on the OBA filter)
    // changed nothing. The synthesized builder below derives the standard
    // mirror -> module pipeline from the products' flags, which is right for a
    // repo with no explicit rules and wrong for one that spells its own out: EFO
    // extracts a BOT module and then filters it, and synthesizing that instead
    // yields a different module under the source ontology's IRI.
    if has_import_rules(repo) {
        return refresh_imports_planned(repo, plan, false);
    }
    if plan.imports.is_empty() {
        status!("make: no import products configured");
    }
    // Under base merging there are no per-product modules: `IMPORT_ROOTS` is
    // `merged_import` alone, and `$(IMPORT_FILES)` — the release asset — is that
    // one file. Building each product's `imports/<id>_import.owl` instead writes
    // eighteen files nothing reads and leaves the one file the release does read
    // untouched, so `all_imports` refreshes nothing.
    if plan.merged_import.is_some() {
        return build_imports_fresh(repo, plan, false, repo.refresh_mirrors);
    }
    let catalog = load_catalog_planned(repo);
    let work = repo.dir.join(".owlmake-odk-tmp");
    std::fs::create_dir_all(&work)?;
    // With a MERGED import configured, it is the only product — the same rule
    // `refresh_imports` applies. HPO says so directly (`IMPORT_ROOTS =
    // $(IMPORTDIR)/merged_import`), and its `all_imports` resolves to exactly one
    // file. Building the per-import modules anyway reaches HPO's bespoke
    // `imports/nbo_import.owl` rule, whose `imports/nbo_terms_combined.txt`
    // prerequisite does not exist and has no rule — nothing builds that module,
    // so `all_imports` would fail on a file the repo has no reason to possess.
    // It is NOT one of `plan.imports` — those are the ten per-import modules — so it
    // is built by its own rule, like any other target.
    if let Some(merged) = plan.merged_import.as_deref() {
        run_target_recipe_planned(repo, merged)
            .with_context(|| format!("building the merged import `{merged}`"))?;
        // P5: the product has to exist. Filtering the per-import modules out and
        // then finding no plan entry for the merged one would return Ok having
        // built nothing at all — `all_imports` exiting 0 with no imports.
        let out = repo.dir.join(merged);
        let out = if out.exists() { out } else { repo.root.join(merged) };
        if !out.exists() {
            bail!("`all_imports` produced no `{merged}`");
        }
        return Ok(());
    }
    for imp in &plan.imports {
        build_one_import(repo, plan, imp, &catalog, &work)
            .with_context(|| format!("building import module `{}`", imp.id))?;
    }
    Ok(())
}

/// Run an arbitrary target the plan names, from its recorded steps (the dynamic
/// dispatch path for phony targets like `test`/`sparql_test` and any custom
/// target). Recursively builds prerequisites that have their own rules first,
/// then runs this target's steps one by one through owlmake's native
/// decomposition. Uncovered steps surface as the same errors the release path
/// raises.
fn run_target_recipe_planned(repo: &Repo, target: &str) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    run_target_recipe_inner(repo, target, &mut seen)
}

/// Whether the per-run memo already holds `target`, allowing for the two SPELLINGS
/// the same file has: the plan names paths relative to the plan file
/// (`src/ontology/hp.owl`) while a recipe names them relative to the build
/// directory (`hp.owl`). Matching the strings verbatim would make the QC phase's
/// `hp.owl` and the artefact phase's `src/ontology/hp.owl` different keys, so the
/// recipe would run twice — putting `hp.owl` NEWER than the `hp.obo` that
/// `test_obo` wrote, so the release rule replaces the file the release ships.
///
/// Suffix equality rather than file-name equality, so `subsets/hp.obo` and `hp.obo`
/// stay distinct.
fn memo_has(repo: &Repo, target: &str) -> bool {
    // The only spelling difference a memo entry may have is the REPO-DIR prefix
    // (`src/ontology/mondo_edges.tsv` vs `mondo_edges.tsv` — the same file named
    // two ways).
    let dir_rel = repo
        .dir
        .strip_prefix(&repo.root)
        .ok()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    repo.built.borrow().iter().any(|m| same_target(m, target, &dir_rel))
}

/// Whether two target spellings name the same file.
///
/// Matching on ANY trailing path segment made `tmp/composite-metazoan.owl` and
/// `composite-metazoan.owl` the same target — and they are two different files,
/// the second built FROM the first. Every UBERON artefact of that shape
/// (`uberon.owl`, `collected-*`, `composite-*`) was therefore reported
/// `✓ done (0:00)` the moment its `tmp/` input had been made, and never written:
/// 18 of the 133 release artefacts went missing with the build reporting success.
fn same_target(a: &str, b: &str, dir_rel: &str) -> bool {
    if a == b {
        return true;
    }
    if dir_rel.is_empty() {
        return false;
    }
    a == format!("{dir_rel}/{b}") || b == format!("{dir_rel}/{a}")
}

/// Whether this run `--assume-new`s the file `name` names (either spelling —
/// plan-relative or build-directory-relative).
fn assumed_new(repo: &Repo, name: &str) -> bool {
    if repo.assume_new.is_empty() {
        return false;
    }
    let dir_rel = repo
        .dir
        .strip_prefix(&repo.root)
        .ok()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    repo.assume_new.iter().any(|w| same_target(w, name, &dir_rel))
}

/// A prerequisite that is an import PRODUCT has no rule of its own: the
/// product's recorded pipeline builds it. A module on disk stands, as any kept
/// (`IMP`) target does; an absent one is built this once — unless the run pinned
/// the imports explicitly, in which case there is nothing to build it from. EFO
/// gitignores `imports/mondo_import.owl`, so on a fresh checkout `build/efo.owl`
/// needs a module nothing has written yet.
fn build_import_prerequisite(
    repo: &Repo,
    imp: &crate::plan::ImportPlan,
    name: &str,
    seen: &mut std::collections::HashSet<String>,
) -> Result<()> {
    if !seen.insert(name.to_string()) || memo_has(repo, name) {
        return Ok(());
    }
    let present = repo.dir.join(name).exists() || repo.root.join(&imp.output).exists();
    if present && !repo.refresh_imports {
        return Ok(());
    }
    if !present && repo.imports_pinned {
        bail!(
            "`{name}` is an import module pinned by IMP=false but is not present. \
             Under IMP=false nothing builds it — re-run with IMP=true (or `--rebuild imports`)"
        );
    }
    if !present {
        status!("make: `{name}` is kept by default (IMP) but absent — building it from its pipeline this once");
    }
    let catalog = load_catalog_planned(repo);
    let work = repo.output_dir.join(".owlmake-odk-tmp");
    std::fs::create_dir_all(&work)?;
    build_one_import(repo, repo.plan, imp, &catalog, &work)
        .with_context(|| format!("building import module `{}`", imp.id))?;
    repo.built.borrow_mut().insert(name.to_string());
    Ok(())
}

fn run_target_recipe_inner(
    repo: &Repo,
    target: &str,
    seen: &mut std::collections::HashSet<String>,
) -> Result<()> {
    // Per-run, not per-entry-point: see `Repo::built`. `seen` still guards the
    // local recursion, but the run-wide set is what makes a target's recipe run
    // once however many callers reach it.
    if !seen.insert(target.to_string()) || memo_has(repo, target) {
        return Ok(());
    }
    repo.built.borrow_mut().insert(target.to_string());
    let a = repo.target(target).ok_or_else(|| {
        anyhow::anyhow!(
            "no rule to make target `{target}`: the plan defines no such target. \
             Regenerate the plan if the target is new — execution never consults a Makefile"
        )
    })?;
    // A switch the run turned off — `MIR=false`, `IMP=false`, `BRI=false` —
    // deletes the rules of its group, so the files of that group on disk are
    // final.
    //
    // DECIDED HERE, before the prerequisite walk below — not after it. A pinned
    // target still drags its whole prerequisite closure if the pin is checked
    // late, and for UBERON that closure is `mirror/merged.owl` and the fifteen
    // upstream mirrors under it: `om make IMP=false MIR=false all_assets`
    // downloaded 124 MB of `pr_slim.owl` before ever reaching a pin that would
    // have said "pinned". `ensure_prerequisite` already decides it in this
    // position and says why; this path never got the same treatment.
    //
    // The file need NOT already exist. Under `MIR=false` GNU make has no rule for
    // an absent mirror and stops with "No rule to make target"; downloading it
    // instead is a silent substitution of a different input (P5), and it is how a
    // `MIR=false` run came to overwrite pinned copies with today's upstream.
    if let Some(pin) = pinned_by(repo, target) {
        if repo.dir.join(target).exists() {
            status!("make: `{target}` pinned ({pin})");
            return Ok(());
        }
        if pin.explicit {
            bail!(
                "`{target}` is pinned by {pin} but is not present. \
                 Under {pin} the rules that build it do not exist, so there is nothing to \
                 build it from — re-run with {}=true (or `--rebuild {}`) to rebuild it",
                pin.flag,
                pin.group,
            );
        }
        // Kept only by the group's DEFAULT, and absent. The default pins the
        // content of a file that exists; a target nothing committed (EFO
        // gitignores `imports/mondo_import.owl`) has no content to pin, and
        // refusing it would leave every fresh clone — CI first among them —
        // unable to build at all. Said out loud, so the run reads as what it
        // did; an explicit `{flag}=false` still refuses above.
        status!(
            "make: `{target}` is kept by default ({pin}) but absent — building it this once",
            pin = pin
        );
    }

    // An AGGREGATE target — prerequisites plus nothing but bookkeeping (EFO's
    // `qc: sparql_test all_reports label_synonym_dup_check check_mondo_obsoletes`,
    // a `test:` roll-up and its `echo "Finished…"`) — runs KEEP-GOING, reporting
    // every member.
    //
    // Fail-fast is wrong for a QC roll-up specifically: stopping at the first
    // failing check tells a curator that `sparql_test` found something and
    // nothing at all about the other three, so each fix-and-rerun cycle
    // rediscovers one problem. Ordinary targets keep fail-fast, because there a
    // later step genuinely depends on an earlier one having succeeded.
    let aggregate = !a.needs.is_empty()
        && a.steps.iter().all(|s| {
            matches!(s, Step::Inert(_))
                || matches!(s, Step::File(op) if matches!(op, recipe::FileOp::Print { .. }))
        });

    // Build prerequisites the plan knows how to build (phony sub-targets like
    // `sparql_test`, or file targets); plain source files are left for the steps
    // to consume.
    let mut failures: Vec<(String, anyhow::Error)> = Vec::new();
    for pre in &a.needs {
        // `all_robot_plugins` provisions plugin JARs (kgcl, uberon, …) into a
        // plugin directory. owlmake implements every one of those commands itself
        // (`kgcl:mint`→`mint`, `uberon:*`, …) and loads no JARs — skip the target
        // (and its `*.jar` copy sub-rules) rather than shell out to a
        // download/copy whose product nothing in this build would ever load.
        if pre == "all_robot_plugins" || pre.ends_with(".jar") {
            continue;
        }
        if repo.target(pre).is_none() {
            // No rule of its own — but an import PRODUCT is built by its recorded
            // pipeline, and a release that needs the module cannot wait for one.
            if let Some(imp) = import_module_for(repo.plan, pre) {
                build_import_prerequisite(repo, imp, pre, seen)?;
            }
            continue;
        }
        // An `--assume-new` file is treated as just modified: dependents run,
        // and the file itself is neither rebuilt nor touched.
        if assumed_new(repo, pre) {
            continue;
        }
        if skip_missing_intermediate(repo, target, pre) {
            continue;
        }
        if !aggregate {
            run_target_recipe_inner(repo, pre, seen)?;
            continue;
        }
        // Already visited via another path: it ran, and reporting it twice would
        // be a lie about how much work this target did.
        if seen.contains(pre) {
            status!("[ ok ] {pre} (already run)");
            continue;
        }
        match run_target_recipe_inner(repo, pre, seen) {
            Ok(()) => status!("[PASS] {pre}"),
            Err(e) => {
                status!("[FAIL] {pre}: {e:#}");
                failures.push((pre.clone(), e));
            }
        }
    }
    if !failures.is_empty() {
        let names: Vec<&str> = failures.iter().map(|(n, _)| n.as_str()).collect();
        status!("─────────────────────────────────────────────");
        bail!(
            "`{target}`: {} of {} check(s) failed: {}",
            failures.len(),
            a.needs.len(),
            names.join(", ")
        );
    }
    // A prerequisite that neither exists nor has a rule refuses the target
    // BEFORE its recipe runs — running it anyway would litter the recipe's
    // redirect and staging files on a build that can only fail. ECTO's
    // `old_modules/%.omn` needs `syns.json`, which is absent and has no rule:
    // the target fails here, with the missing file named, and writes nothing.
    for pre in &a.needs {
        if pre == "all_robot_plugins" || pre.ends_with(".jar") {
            continue;
        }
        if repo.dir.join(pre).exists()
            || repo.target(pre).is_some()
            || repo.plan.is_phony(pre)
            || recipe::is_served_image_asset(pre)
            || assumed_new(repo, pre)
            || is_native_pattern_product(repo, pre)
            || mirror_import_for(repo, pre).is_some()
        {
            continue;
        }
        bail!("no rule to make target `{pre}`, needed by `{target}`");
    }

    // Patterns are native, so their products carry no plan rule — but a target
    // that NEEDS one still has to find it there. `execute_plan` runs the pattern
    // stage up front; a single-target invocation (`om make tmp/seed.txt`) does
    // not, and would build a seed missing all 317 pattern terms. Run it on
    // demand, once per run (`repo.built`). A product the plan DOES carry a rule
    // for is that rule's to build, not the pattern stage's.
    let ruled = |n: &str| {
        repo.plan
            .artefacts
            .iter()
            .chain(repo.plan.prerequisites.iter())
            .any(|a| a.target == n && !a.missing_rule)
    };
    if a.needs
        .iter()
        .any(|n| is_native_pattern_product(repo, n) && !ruled(n) && !repo.dir.join(n).exists())
    {
        if repo.built.borrow_mut().insert("\u{1}patterns".to_string()) {
            if repo.regenerate_patterns {
                regenerate_patterns_planned(repo, repo.plan)
                    .with_context(|| "regenerating the DOSDP pattern products")?;
            } else if let Some(dosdp) = repo.plan.dosdp.as_ref() {
                write_pattern_terms_from_definitions(repo, dosdp).with_context(|| {
                    "extracting the pattern seed from the committed definitions.owl"
                })?;
            }
        }
    }

    // Mirrors are the same shape: `imports/<id>_import.owl: mirror/<id>.owl` is
    // what an import rule looks like, and `mirror/<id>.owl` carries no plan rule
    // because the import's own `source` + `mirror_steps` ARE the mirror. Build it
    // here, or the rule below runs its whole pipeline over a file that is not
    // there — producing nine import modules holding nothing but their version IRI.
    for pre in &a.needs {
        if let Some(imp) = mirror_import_for(repo, pre) {
            ensure_mirror(repo, imp, repo.refresh_mirrors)
                .with_context(|| format!("building prerequisite `{pre}` of `{target}`"))?;
        }
    }

    // A step owlmake cannot model is a planning failure, not a build failure:
    // say so before running anything, with the gaps the planner recorded. (The
    // release path refuses the same way, via `Plan::blocking_gaps`.)
    if !a.gaps.is_empty() {
        bail!(
            "cannot run target `{target}`: {}",
            a.gaps.join("; ")
        );
    }
    // The up-to-date rule, on the step-replay path. The artefact loop uses
    // `is_up_to_date` and the prerequisite walk uses `prereq_is_newer`; without the
    // same check here a replayed target runs its recipe unconditionally, and a
    // repo's own CI invocation (`om make test IMP=false PAT=false MIR=false`)
    // rebuilds the entire release path on every QC run.
    //
    // A phony target names no file and is always out of date; a file target is
    // skipped when it exists and is newer than every prerequisite that is itself
    // a file. `-B`/`--always-make` overrides that.
    // Asking for a group to be rebuilt is asking for its recipes to run, not for
    // them to be considered: `--rebuild imports` says "even where their outputs
    // are present", so a module already on disk is no answer.
    let forced = repo.always_make
        || (repo.refresh_imports && is_import_target(repo, target))
        || (repo.refresh_mirrors && is_mirror_target(repo, target));
    if !a.steps.is_empty() && !forced && !repo.plan.is_phony(target) {
        if let Some(out) = repo.target_file(target).filter(|p| p.is_file()) {
            let out_mtime = std::fs::metadata(&out).and_then(|m| m.modified()).ok();
            let newer = a.needs.iter().any(|pre| assumed_new(repo, pre)) || a.needs.iter().any(|pre| {
                let p = repo.target_file(pre).unwrap_or_else(|| repo.dir.join(pre));
                match (std::fs::metadata(&p).and_then(|m| m.modified()).ok(), out_mtime) {
                    (Some(pm), Some(om)) => pm > om,
                    _ => false,
                }
            });
            if !newer {
                status!("make: `{target}` is up to date");
                return Ok(());
            }
        }
    }

    // Run this target's own steps (a no-op for pure aggregate targets that only
    // declare prerequisites). The steps were expanded at ingest — `$@`/`$<`/`$^`
    // and any `$(eval)` are already resolved — so running them needs no further
    // variable expansion.
    if !a.steps.is_empty() {
        status!("make: running target `{target}`");
        let catalog = load_catalog_planned(repo);
        let work = repo.dir.join(".owlmake-odk-tmp");
        std::fs::create_dir_all(&work)?;
        let out = repo.dir.join(target);
        run_artefact(repo, a, &catalog, &work, &repo.dir, &out)
            .with_context(|| format!("running target `{target}`"))?;
    } else if a.needs.is_empty() {
        // A plan may legitimately name an empty target, but it must SAY so: a
        // silent success here is indistinguishable from a check that ran.
        status!("make: target `{target}` has nothing to do (no recipe, no prerequisites)");
    }
    Ok(())
}

/// Rebuild `imports/merged_import.owl` from upstream: download each product's
/// mirror, reduce make-base/base-iris products to their base module, merge, drop
/// excluded IRIs, then extract a ⊥-locality module over the seed signature
/// (the committed `imports/*_terms.txt` plus the edit ontology's own signature).
/// Mirrors are cached under `mirror/`.
/// The ontology IRI and version IRI to stamp on the merged import module.
///
/// A plan may name the IRI outright (`merged_import_iri`): EFO's modules live
/// under `http://www.ebi.ac.uk/efo/imports/` while its plan carries the OBO PURL
/// as `ontology_iri`. Otherwise it is derived from `ontology_iri` and the
/// module's path — the path of the *document*: without the compression suffix a
/// gzipped module carries, and relative to the ontology directory even when the
/// plan spells it from the repository root.
fn merged_import_iris(
    ontology_iri: &str,
    version: &str,
    explicit: Option<&str>,
    rel: &str,
    dir_rel: &str,
) -> (String, String) {
    let ontbase = ontology_iri.strip_suffix(".owl").unwrap_or(ontology_iri);
    let doc = rel.strip_suffix(".gz").unwrap_or(rel);
    let doc = if dir_rel.is_empty() { doc } else { doc.strip_prefix(&format!("{dir_rel}/")).unwrap_or(doc) };
    let iri = explicit.map(str::to_string).unwrap_or_else(|| format!("{ontbase}/{doc}"));
    let ver = format!("{ontbase}/releases/{version}/{doc}");
    (iri, ver)
}

fn build_imports_fresh(
    repo: &Repo,
    plan: &Plan,
    exclude_large: bool,
    refresh_mirrors: bool,
) -> Result<()> {
    use crate::extract::{self, Method};

    // The plan's `$(MIRRORDIR)` when the repo sets one; the conventional
    // `mirror/` otherwise.
    let mirror_dir = {
        let d = repo.var("MIRRORDIR");
        repo.dir.join(if d.is_empty() { "mirror" } else { d })
    };
    let import_dir = repo.dir.join("imports");
    std::fs::create_dir_all(&mirror_dir)?;
    std::fs::create_dir_all(&import_dir)?;
    let obobase = {
        let b = repo.var("OBOBASE");
        if b.is_empty() { "http://purl.obolibrary.org/obo".to_string() } else { b.to_string() }
    };

    // The import products, as the plan recorded them (`ImportPlan::product`) —
    // the repo's import-group configuration is an ingest-time input, not a
    // build-time one.
    let products: Vec<crate::odk::ImportProduct> =
        plan.imports.iter().filter_map(|i| i.product.clone()).collect();

    let mut merged = empty_model();
    for p in &products {
        // `refresh-imports-excluding-large` skips the products flagged large.
        if exclude_large && p.is_large {
            status!("import: skipping large import {} (excluding-large)", p.id);
            continue;
        }
        // Custom mirrors have no plain download URL — the project supplies a
        // `mirror-<id>` recipe instead. A cached import module short-circuits it,
        // which matters because the project script that would rebuild it is
        // exactly what owlmake cannot synthesize; otherwise run that recipe (with
        // `$(ROBOT)` resolving to the owlmake binary) to produce
        // mirror/<id>.owl, then process it like any other mirror.
        if p.mirror_type.as_deref() == Some("custom") {
            // The module may be committed gzipped (`<id>_import.owl.gz`: EFO's PR
            // module is over GitHub's file-size limit as plain RDF/XML), and
            // `io::load` reads either form. Missing the `.gz` here silently fell
            // through to the raw mirror, skipping the custom recipe's own steps
            // (PR's `rename-terms`/`remove-terms`) for the merged import.
            let cached = [format!("{}_import.owl", p.id), format!("{}_import.owl.gz", p.id)]
                .into_iter()
                .map(|f| import_dir.join(f))
                .find(|c| c.exists());
            if let Some(cached) = cached {
                status!("import: {} (custom) — using cached {}", p.id, cached.display());
                merge_file_into(&mut merged, &cached)?;
                continue;
            }
        }
        // One mirror builder for every path: `ensure_mirror` runs the import's own
        // `source` + `mirror_steps`, which is what a mirror IS.
        let imp = plan
            .imports
            .iter()
            .find(|i| i.id == p.id)
            .ok_or_else(|| anyhow::anyhow!("import product `{}` has no plan entry", p.id))?;
        let dest = ensure_mirror(repo, imp, refresh_mirrors)?;
        let mut m = crate::io::load(&dest)?;
        // Base extraction (`remove --axioms external --base-iri …`) happens at
        // mirror time for `make_base` products ONLY, which is what each source's
        // own `mirror-<id>` rule asks for: a `make_base` source (e.g. PR's slim,
        // STATO) is reduced to its base, while a source that is already a
        // `-base.owl` (go-base, uberon-base, …) is fetched and converted
        // verbatim. Stripping external axioms from an already-base file drops the
        // cross-ontology bridge axioms it legitimately keeps (e.g.
        // `SubClassOf(GO_x, ∃RO.CHEBI_y)`), which then fail to pull those external
        // terms into the ⊥-module — so gate on `make_base`, not on `base_iris`
        // (which non-`make_base` imports like `go` also set, purely as metadata).
        if p.make_base {
            let bases: Vec<String> = if p.base_iris.is_empty() {
                vec![format!("{obobase}/{}_", p.id.to_uppercase())]
            } else {
                p.base_iris.clone()
            };
            m = crate::cmd::remove::remove(m, &[], &[], &[], &["external".into()], &bases)?;
        }
        merge_model_into(&mut merged, m);
    }

    // Seed: committed *_terms.txt plus the edit ontology's signature.
    let seed = import_seed(repo, plan)?;
    status!("import: extracting ⊥-module over {} seed terms", seed.len());
    // Honour the plan's `slme_individuals` policy (`extract --individuals`).
    // ECTO sets `exclude`, so imported individuals — and any now-degenerate
    // Same/DifferentIndividuals axioms left when their peers fall outside the
    // module — are dropped rather than emitted as invalid single-arg axioms.
    let mut opts = extract::ExtractOptions::default();
    if let Some(spec) = &plan.slme_individuals {
        match extract::Individuals::parse(spec) {
            Some(ind) => opts.individuals = ind,
            None => status!("import: warning: unknown slme_individuals `{spec}`, using include"),
        }
    }
    let mut module = extract::extract_with(&merged, &seed, Method::Bot, &opts);
    drop(merged);

    // Drop excluded IRIs (the plan's `exclude_iri_patterns`, e.g. `<…/GOCHE_*>`)
    // from the MODULE, which is where the ODK recipe runs its `remove` chain
    // (`merge … extract … $(foreach x,$(EXCLUDE_IRIS),remove --select "$(x)")`).
    // Removing from the merged mirror instead — six million axioms for EFO,
    // one structure-preserving pass per pattern — took nine minutes for sixteen
    // patterns; on the module it is a few seconds.
    module = drop_excluded(module, plan)?;

    // The merged-import rule post-processes the ⊥-module before writing it:
    //
    // 1. `remove --term <ANNOTATION_PROPERTIES> --term-file <seed> --select
    //    complement --select annotation-properties` — strip every annotation
    //    property NOT in the keep set (the four configured `ANNOTATION_PROPERTIES`
    //    plus every seed term). Import-only properties (`created_by`,
    //    `hasAlternativeId`, …) are not declared in the edit/components, so they
    //    are absent from the seed and get removed here — otherwise they leak onto
    //    imported GO/UBERON/PR terms downstream. Properties the edit *does* declare
    //    (hasDbXref, the synonym properties, comment) are in the seed and stay.
    let mut keep: std::collections::HashSet<String> =
        ["rdfs:label", "IAO:0000115", "oboInOwl:is_metadata_tag", "OMO:0002000"]
            .iter()
            .map(|s| crate::cmd::babelon::expand_curie(s))
            .collect();
    keep.extend(seed.iter().cloned());
    module = strip_import_annotation_properties(module, &keep);
    // 2. `normalize --subset-decls true --synonym-decls true`.
    module = crate::cmd::normalize::normalize_with(
        module,
        &crate::cmd::normalize::NormalizeOptions {
            subset_decls: true,
            synonym_decls: true,
            add_source: false,
            ..Default::default()
        },
    );
    // 3. `repair --merge-axiom-annotations true` — and NOTHING else.
    //    `--invalid-references` defaults to FALSE and the rule does not pass it,
    //    so no reference repair runs at all; and even when it does, repair
    //    ignores dangling references, so one is never a violation it acts on.
    //    Removing dangling annotation assertions here would drop the three that
    //    carry ENVO_01001862 into the module — they belong there, under no entity
    //    banner, because their subject is an annotation subject and not a
    //    declared entity.
    module = crate::cmd::repair::repair_with(
        module,
        &crate::cmd::repair::RepairOptions {
            invalid_references: false,
            merge_axiom_annotations: true,
        },
    );

    // The plan NAMES the merged import; `imports/merged_import.owl` is only the
    // conventional spelling. A repo that sets `$(IMPORTDIR)` or renames the product
    // keeps its module somewhere else, and writing to the default would leave that
    // one stale.
    let rel = plan.merged_import.as_deref().unwrap_or("imports/merged_import.owl");
    let out = repo.dir.join(rel);
    let dir_rel = repo
        .dir
        .strip_prefix(&repo.root)
        .ok()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (mi_iri, mi_ver) =
        merged_import_iris(&plan.ontology_iri, &plan.version, plan.merged_import_iri.as_deref(), rel, &dir_rel);
    let annos =
        vec!["http://www.w3.org/2002/07/owl#versionInfo".to_string(), plan.version.clone()];
    let mut module =
        crate::cmd::annotate::annotate(module, Some(&mi_iri), Some(&mi_ver), &annos, &[], false)?;
    // `extract` builds a NEW ontology whose document format declares nothing — the
    // rule `extract::run` applies on the CLI path has to be applied on this
    // synthesized one too, or the module keeps the merged mirror's prefix map and
    // abbreviates every OBO IRI (`obo:CHEBI_10545`) an extracted module spells out
    // in full. Such a module opens with `:` bound to the ontology IRI plus
    // owl/rdf/xml/xsd/rdfs, and nothing else.
    module.format_prefixes_cleared = true;
    module.prefixes = crate::io::robot_ofn_prefixes(&module);
    crate::io::save_as(&mut module, &out, crate::io::Format::Functional)?;
    status!("import: wrote {} ({} components)", out.display(), module.ont.iter().count());
    Ok(())
}

/// Build a single import module on demand by running its declared pipeline
/// (`imp.steps`): download/cache the source mirror, then apply the recorded
/// operations — `extract` over the recorded seed term file(s), then any
/// `filter`/`remove`/`rename` the repo's import recipe specifies (per-import
/// excludes, the PR→EFO rename, oba's extra seed term, …).
///
/// Shell steps (e.g. MONDO's dynamic HGNC auto-exclude) run in place, between
/// the native ops around them, with `$(ROBOT)` resolving to the owlmake binary —
/// so the dynamic step runs without the rest of the pipeline giving up on being
/// native.
fn build_one_import(
    repo: &Repo,
    plan: &Plan,
    imp: &crate::plan::ImportPlan,
    catalog: &BTreeMap<String, PathBuf>,
    work: &Path,
) -> Result<()> {
    use crate::plan::step::{Op, Step};

    let out = repo.dir.join(&imp.output);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let src_path = ensure_mirror(repo, imp, repo.refresh_mirrors)?;

    let mut model = crate::io::load(&src_path)?;

    // Drop globally-excluded IRIs (the plan's `exclude_iri_patterns`, e.g.
    // `<…/GOCHE_*>`).
    model = drop_excluded(model, plan)?;

    // Fallback seed: an extract that declares no seed term file is seeded from the
    // edit ontology's signature, which is what an import with no committed terms
    // file relies on. Materialized as a temp term file injected into the first
    // extract.
    let needs_editsig = imp.seed_term_files().is_empty()
        && imp
            .steps
            .iter()
            .any(|s| matches!(s, Step::Op(Op::Extract { .. }) | Step::Op(Op::Filter(_))));
    let editsig = if needs_editsig { write_edit_signature(repo, work, &imp.id)? } else { None };

    // Inject the edit-signature seed into the first extract/filter, if needed.
    let steps: Vec<Step> = match &editsig {
        Some(f) => inject_editsig_seed(&imp.steps, f),
        None => imp.steps.clone(),
    };
    model = run_steps(repo, &steps, model, catalog, work, Some(&imp.output), true, Some(&src_path))
        .with_context(|| format!("building import {}", imp.id))?;

    crate::io::save_as(&mut model, &out, crate::io::Format::RdfXml)?;
    // The stage files this pipeline threaded the model through carried it from
    // one step to the next and nothing reads them again; the built tree holds the
    // module, not the ⊥-module it was filtered from.
    for step in &steps {
        if let Step::Op(Op::RoundTrip { path }) = step {
            remove_transient(repo, path);
        }
    }
    if !crate::progress::stage_active() {
        status!(
            "import: built {} → {} ({} components)",
            imp.id,
            out.display(),
            model.ont.iter().count()
        );
    }
    Ok(())
}


/// Do two paths name the same file, one written from the repository root and one
/// from the build directory?
fn same_path(a: &str, b: &str) -> bool {
    a == b
        || a.strip_suffix(b).is_some_and(|p| p.ends_with('/'))
        || b.strip_suffix(a).is_some_and(|p| p.ends_with('/'))
}

/// Remove a stage file the plan lists as transient, if this is one.
///
/// The build writes it on its way to something else and does not keep it: it is
/// reached only through a pattern, so nothing names it and nothing reads it once
/// the chain is done. A path the plan does not list is left alone.
fn remove_transient(repo: &Repo, path: &str) {
    if !repo.plan.transient_targets.iter().any(|t| same_path(t, path)) {
        return;
    }
    let file = repo.target_file(path).unwrap_or_else(|| repo.dir.join(path));
    let _ = std::fs::remove_file(file);
}

/// Remove the transient targets this run BUILT, once every target that needed
/// them is done.
///
/// A run that did not remake one leaves it where it found it: the build removes
/// what it wrote, not what was already there.
pub fn sweep_transients(repo: &crate::odk::OdkRepo, plan: &Plan, goals: &[String]) {
    let built = repo.built.borrow();
    for target in &plan.transient_targets {
        if !built.iter().any(|b| same_path(b, target)) {
            continue;
        }
        // An intermediate the CALLER ASKED FOR is not an intermediate. GNU make
        // deletes the files it made only on its way to something else; a file
        // named on the command line is a goal, and a goal is never swept. Without
        // this, `om make imports/chebi_bot.owl` wrote the 39 MB file, deleted it,
        // and exited 0 — nine of EFO's targets producing nothing while reporting
        // success, and no way to inspect an intermediate at all.
        if goals.iter().any(|g| same_path(g, target)) {
            continue;
        }
        for base in [&repo.root, &repo.dir] {
            let p = base.join(target);
            if p.is_file() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Whether a step runs outside the in-memory op pipeline — the command-line
/// launchers and raw shell. Such a step is executed **where it sits** by
/// [`run_shell_step`] and does not disqualify the rule around it, so a pipeline
/// that is native apart from one `sed` keeps every other step native.
fn is_shell_step(s: &crate::plan::step::Step) -> bool {
    use crate::plan::step::Step;
    matches!(
        s,
        Step::Shell { .. }
            | Step::Jq(_)
            | Step::Sssom(_)
            | Step::OwlmakeCli { .. }
    )
}

/// Execute one out-of-pipeline step. `RunShell`/`UnsupportedShell` recorded a
/// command line, decomposed here exactly as a recipe line would be (the
/// `robot`/`jq`/`sssom` command words resolving to the owlmake binary, file ops
/// run natively, only genuine text processors reaching `sh`). `Jq`/`Sssom`/
/// `OwlmakeCli` recorded argv tokens, so they invoke the binary directly.
///
/// `UnsupportedShell` is "unsupported" only in the sense that owlmake has no
/// native engine for the command word and does not probe the PATH at plan time —
/// the line is still there and the shell can still run it, which is why a plan
/// needs no `recipe` fallback to be executable.
/// Point a command's plan-named paths at where the build actually published them.
///
/// A release artefact is written to the OUTPUT directory, so the location the
/// plan names for it — `src/ontology/oba-base.owl` — no longer holds the file by
/// the time a later target reads it. That is what `Repo::target_file` is for, and
/// a model-threading step already gets it; a recorded command line did not, so
/// `check_rdfxml_%: %` — a check whose whole recipe is `check-rdfxml <the
/// artefact>` — could not run at all on the two artefacts published before it.
///
/// Only a token the plan names as a TARGET is rewritten, and only when the
/// plan-named location is empty: a path that resolves where the recipe says is
/// left exactly as written, and a name the plan does not build is not a path this
/// may move.
///
/// A DESTINATION is never rewritten — the token after `>`/`>>`/`2>` or after
/// `-o`/`--output`. Resolution exists so a command can READ a file the build
/// published elsewhere; where a command WRITES is the recipe's choice, and a
/// repository that commits a previous release beside the build has the same name
/// standing for two different files. `grep -v ^owl-axioms: … > subsets/x.obo`
/// must land on the artefact the plan names, not on the committed copy.
fn resolve_published_targets(repo: &Repo, cmd: &str) -> String {
    // Rebuilt with the ORIGINAL separators: a command line's whitespace is part of
    // it, and runs of spaces inside a quoted argument carry meaning. MONDO's
    // release-diff step is `sed -i 's/  */ /g' …` — collapse each run of spaces to
    // one — and a pattern respelled with a single space is ` *`, which matches the
    // empty string everywhere and spaces out the whole file instead.
    let mut out = String::with_capacity(cmd.len());
    let mut rest = cmd;
    let mut next_is_destination = false;
    while !rest.is_empty() {
        let ws = rest.find(|c: char| !c.is_whitespace()).unwrap_or(rest.len());
        out.push_str(&rest[..ws]);
        rest = &rest[ws..];
        if rest.is_empty() {
            break;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let tok = &rest[..end];
        rest = &rest[end..];
        let is_destination = std::mem::replace(
            &mut next_is_destination,
            matches!(tok, ">" | ">>" | "2>") || is_output_flag(tok),
        );
        if is_destination
            || tok.starts_with('>')
            || tok.starts_with('-')
            || repo.target(tok).is_none()
            || repo.dir.join(tok).exists()
        {
            out.push_str(tok);
        } else {
            match repo.target_file(tok) {
                Some(p) => out.push_str(&p.display().to_string()),
                None => out.push_str(tok),
            }
        }
    }
    out
}

/// One token of a recorded command line, resolved as `resolve_published_targets`
/// resolves a whole line — for the recorded argv of a `robot` step, where the same
/// artefact appears as an argument. OBA's `release-diff.md` rule is
/// `diff --left tmp/current-release.owl --right oba.owl`, and its `oba.owl` is a
/// released artefact: with nothing at the plan-named location the diff cannot open
/// it, and the whole check fails on a release that is otherwise complete.
fn resolve_published_token(repo: &Repo, tok: &str) -> String {
    if tok.starts_with('-') || repo.target(tok).is_none() || repo.dir.join(tok).exists() {
        return tok.to_string();
    }
    match repo.target_file(tok) {
        Some(p) => p.display().to_string(),
        None => tok.to_string(),
    }
}

/// Resolve a recorded argv, leaving every OUTPUT path alone.
///
/// Following an artefact to where it was published is about reading it. An
/// output argument says where this step WRITES, and the file it names is the one
/// the step is here to make — so a copy of it somewhere else is not a better
/// answer, it is a different file. MP commits `reports/mp-edit.owl-obo-report.tsv`
/// at the repository root as well as building it under `src/ontology`, and
/// redirecting the `-o` onto the committed copy left the target it was asked for
/// unwritten.
fn resolve_published_argv(repo: &Repo, args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let mut writes_next = false;
    for a in args {
        out.push(if writes_next { a.clone() } else { resolve_published_token(repo, a) });
        writes_next = is_output_flag(a);
    }
    out
}

/// The flags whose value is a path this step writes rather than reads.
fn is_output_flag(tok: &str) -> bool {
    matches!(tok, "-o" | "--output" | "-O" | "--output-dir" | "--outfile")
}

fn run_shell_step(repo: &Repo, step: &Step) -> Result<()> {
    let robot_prefix = repo.var("ROBOT").to_string();
    let args_run = |args: &[String]| recipe::run_owlmake_args(args, &repo.dir);
    match step {
        Step::Shell { command: cmd, .. } => {
            let cmd = &resolve_published_targets(repo, cmd);
            recipe::run_step_command(cmd, &repo.dir, &robot_prefix)
                .with_context(|| format!("step: {cmd}"))
        }
        Step::Jq(args) => {
            let mut argv = vec!["jq".to_string()];
            argv.extend_from_slice(args);
            args_run(&argv).with_context(|| format!("step: jq {}", args.join(" ")))
        }
        // `Sssom` records its own launcher token (`sssom` / `sssom:<cmd>`).
        Step::Sssom(args) => {
            args_run(args).with_context(|| format!("step: {}", args.join(" ")))
        }
        // A `OwlmakeCli` outside a model pipeline (no ontology to thread) just runs.
        Step::OwlmakeCli { name, args } => {
            let mut argv = vec![name.clone()];
            argv.extend(resolve_published_argv(repo, args));
            args_run(&argv).with_context(|| format!("step: robot {name}"))
        }
        other => bail!("internal: {} is not an out-of-pipeline step", other.label()),
    }
}

/// Run a `OwlmakeCli` step *inside* a model pipeline.
///
/// owlmake exposes these as chained CLI commands that thread a model — UBERON's
/// `create-species-subset` prunes the ontology and the recipe's `reason … relax …
/// convert` continue from its result — so firing it for its side effects alone
/// would silently drop it out of the chain. Instead the in-flight model is written
/// to a temp file the command reads via `--input`, and its `--output` is captured
/// back into the pipeline.
///
/// A step that already names its own `--output` is terminal (`report`, `verify`
/// and `measure` write a report file, not an ontology): its options are left
/// exactly as recorded and the model passes through unchanged.
fn run_cli_robot_step(
    repo: &Repo,
    name: &str,
    args: &[String],
    model: crate::model::Model,
    work: &Path,
    // The file the threaded model was loaded from, if any. A recorded `-i` that
    // names it is the rule's own `$<` and is served from memory; one that names
    // anything else is a command line reading its OWN input, and must read it.
    pipeline_input: Option<&Path>,
) -> Result<crate::model::Model> {
    let mut model = model;
    // Terminality is a property of the COMMAND, not of a flag scan. These write
    // their own non-ontology output (a report, a diff, a directory of query
    // results) and pass the model through untouched; several have no `--output`
    // at all, so scanning for `-o` mis-classifies them and appends one. `verify`
    // is the case that matters: a `sparql_test` rule is `verify -i <src>
    // --queries … -O $(REPORTDIR)`, and `-O` does not match the scan, so
    // `--output <tmp>` would be appended to a command that has no such option and
    // clap would exit 2 — the QC check could not run at all.
    // `explain` is NOT terminal: the model it hands the next command is the
    // ontology of its justification axioms (empty when nothing needed
    // explaining), so the chain file it writes through the appended `--output`
    // is exactly that ontology and the pass-through would be wrong.
    const TERMINAL_COMMANDS: &[&str] = &[
        "report",
        "verify",
        "measure",
        "validate-profile",
        "diff",
        "export",
        "export-prefixes",
        "mirror",
        "check-rdfxml",
        "validate-id-ranges",
        "validate-patterns",
        // A sub-make runs targets for their side effects; it threads no model
        // and takes no `--input`/`--output` of its own.
        "make",
    ];
    let terminal = TERMINAL_COMMANDS.contains(&name)
        || args.iter().any(|a| a == "-o" || a == "--output");

    let piped_in = work.join(format!("{name}-chain-in.ofn"));
    crate::io::save_as(&mut model, &piped_in, crate::io::Format::Functional)?;

    // The chain files are named relative to OWLMAKE's working directory (`work`
    // is `output_dir`/`repo.dir` joined with the temp dir), but the command runs
    // with `repo.dir` as ITS working directory — so a relative path handed over
    // as an argument is resolved against a different base and does not exist.
    // EFO's `sparql_test` is the case: owlmake wrote 162 MB to
    // `./src/ontology/.owlmake-odk-tmp/verify-chain-in.ofn` and then told a child
    // running in `src/ontology` to open that same string, which resolves to
    // `src/ontology/src/ontology/…`. The QC gate could not run at all.
    //
    // Absolute paths cross the boundary unambiguously, whatever either base is.
    let arg_path = |p: &Path| -> String {
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().map(|c| c.join(p)).unwrap_or_else(|_| p.to_path_buf())
        };
        abs.display().to_string()
    };

    // Point `--input` at the in-flight model (replacing the recipe's own `$<`,
    // which is only correct for the first step of a rule).
    let mut argv = vec![name.to_string()];
    let mut i = 0;
    let mut saw_input = false;
    // Whether a recorded `-i <path>` is the rule's own pipeline input. A rule with
    // more than one command line can name a different one — CL's `sparql_test` is
    // `verify -i $(SRCMERGED) …` followed by `verify -i cl-full.owl …` — and
    // serving that from the threaded model runs the first check twice and never
    // runs the second. With no pipeline input at all, the model IS the input.
    let threaded = pipeline_input.and_then(|p| p.canonicalize().ok());
    let is_threaded = |tok: &str| match &threaded {
        None => true,
        Some(t) => {
            let rel = tok.strip_prefix("src/ontology/").unwrap_or(tok);
            repo.dir.join(rel).canonicalize().ok().as_ref() == Some(t)
        }
    };
    while i < args.len() {
        if args[i] == "-i" || args[i] == "--input" {
            let recorded = args.get(i + 1).cloned().unwrap_or_default();
            if is_threaded(&recorded) {
                argv.push("--input".to_string());
                argv.push(arg_path(&piped_in));
            } else {
                argv.push("--input".to_string());
                argv.push(resolve_published_token(repo, &recorded));
            }
            saw_input = true;
            i += 2; // drop the recorded path
            continue;
        }
        // An output path names what this step writes; it is not resolved to a
        // copy of the artefact somewhere else.
        argv.push(if i > 0 && is_output_flag(&args[i - 1]) {
            args[i].clone()
        } else {
            resolve_published_token(repo, &args[i])
        });
        i += 1;
    }
    // A sub-make reads no ontology at all — handing it the chain file would be
    // an argument it does not take.
    if !saw_input && name != "make" {
        argv.push("--input".to_string());
        argv.push(arg_path(&piped_in));
    }

    let piped_out = work.join(format!("{name}-chain-out.ofn"));
    if !terminal {
        let _ = std::fs::remove_file(&piped_out);
        argv.push("--output".to_string());
        argv.push(arg_path(&piped_out));
    }

    // The handed-over model is written as the root document alone: its
    // `Import(…)` declarations survive, the axioms they stand for do not. The
    // child therefore has to resolve the same closure the parent did, and the
    // repo's catalog is what resolves it — the chain file sits in owlmake's own
    // working directory, so there is no importing-file neighbour to fall back on.
    if !argv.iter().any(|a| a == "--catalog") {
        if let Some(cat) = repo.plan.catalog_file.as_deref() {
            let p = repo.dir.join(cat);
            if p.exists() {
                argv.push("--catalog".to_string());
                argv.push(arg_path(&p));
            }
        }
    }

    recipe::run_owlmake_args(&argv, &repo.dir)
        .with_context(|| format!("step: robot {name}"))?;

    if terminal || !piped_out.exists() {
        return Ok(model);
    }
    crate::io::load(&piped_out)
}

/// The command text a step would run, for deciding whether it touches a given
/// file. `Op`/`File`/`Branch` steps have no command line of their own.
fn step_command_text(step: &Step) -> Option<String> {
    match step {
        Step::Shell { command: c, .. } => Some(c.clone()),
        Step::Jq(args) | Step::Sssom(args) => Some(args.join(" ")),
        Step::OwlmakeCli { name, args } => Some(format!("{name} {}", args.join(" "))),
        _ => None,
    }
}

/// Run an out-of-pipeline step from *inside* a model pipeline.
///
/// The pipeline holds the ontology in memory while a shell command works on
/// files. When the command names the rule's own target, the model is flushed to
/// it first and re-read afterwards, so a `perl`/`sed` pass can sit between two
/// native ops instead of forcing the whole rule out to a recipe replay. When the
/// command only touches side files — which is the common case, e.g. EFO's mondo
/// import deriving its HGNC exclusion list — nothing is serialized and the model
/// passes through untouched.
/// The files a command REDIRECTS to (`> f`, `>> f`, `-o f`-style redirects are not
/// included — only shell redirections, which are what create a file the command
/// itself does not read).
fn redirect_targets(cmd: &str) -> impl Iterator<Item = &str> {
    let mut out: Vec<&str> = Vec::new();
    let bytes = cmd.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'>' {
            // `>` or `>>`, then optional space, then the destination token.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'>' {
                j += 1;
            }
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() && bytes[j] != b';' {
                j += 1;
            }
            if start < j {
                out.push(&cmd[start..j]);
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out.into_iter()
}

/// The tokens a command reads — everything except its redirect destinations.
fn read_operands(cmd: &str) -> impl Iterator<Item = &str> + '_ {
    let dests: Vec<&str> = redirect_targets(cmd).collect();
    cmd.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '(' | ')' | ';' | ',' | '<' | '>' | '|' | '`'))
        .filter(move |t| !t.is_empty() && !dests.contains(t))
}

fn run_shell_step_in_pipeline(
    repo: &Repo,
    step: &Step,
    model: crate::model::Model,
    target: Option<&str>,
    work: &Path,
    // Whether the model was loaded FROM the target and no op has touched it since,
    // in which case the pre-write below would only re-render what is already there
    // — through owlmake's writer rather than the bytes the previous shell step
    // produced. MONDO's second `filtered.obo` perl reads `$@`, and re-rendering it
    // would hand that perl owlmake's OBO output instead of the first perl's.
    model_on_disk: bool,
    pipeline_input: Option<&Path>,
) -> Result<crate::model::Model> {
    let mut model = model;
    // A chained command step threads the model rather than touching files.
    if let Step::OwlmakeCli { name, args } = step {
        return run_cli_robot_step(repo, name, args, model, work, pipeline_input);
    }
    let touches_target = match (target, step_command_text(step)) {
        // Only an ontology target can be round-tripped; a rule writing a `.tsv`
        // has no model on disk for the command to read or rewrite.
        //
        // The target must appear as a whole token, not as a substring: EFO's mondo
        // import derives `imports/mondo_import.owl.hgnc.tsv`, which *contains* the
        // target `imports/mondo_import.owl` and would otherwise force a 340 MB
        // write-and-reparse round trip on every build for a command that only
        // touches a side file.
        (Some(t), Some(cmd)) if crate::io::Format::from_path(Path::new(t)).is_ok() => {
            let base = Path::new(t).file_name().and_then(|s| s.to_str()).unwrap_or(t);
            let names = |tok: &str| tok == t || tok == base;
            // …and the command must READ the target, not merely write it. MONDO's
            // `mondo.obo` step is `grep -v ^owl-axioms $@.tmp.obo > $@`: the target
            // appears only as the redirect DESTINATION. Round-tripping it there is
            // both pointless and lossy — writing the model out, running the grep,
            // re-parsing the result and re-serialising it exposes every read-side
            // gap in the released file, costing `mondo.obo` its 35 `idspace:` lines,
            // GO:0051705's second `name:`, RO's domain/range `{IAO:0000116=…}`
            // qualifiers and FOODON's literal `replaced_by`.
            // Nothing re-parses here; the grep is a text filter over a file the
            // preceding convert already finished writing.
            let redirect_only = redirect_targets(&cmd).any(names)
                && !read_operands(&cmd).any(names);
            !redirect_only
                && cmd
                    .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '(' | ')' | ';' | ',' | '<' | '>' | '|' | '`'))
                    .any(names)
        }
        _ => false,
    };
    if !touches_target {
        run_shell_step(repo, step)?;
        return Ok(model);
    }
    let path = repo.dir.join(target.expect("target present when touched"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !model_on_disk {
        crate::io::save(&mut model, &path)?;
    }
    run_shell_step(repo, step)?;
    // The command may have rewritten the target in place; if it did not, this
    // re-reads exactly what was just written.
    crate::io::load(&path)
}

/// Evaluate a structured branch condition. Native file tests use `std::fs`
/// (paths relative to the ontology dir); a non-native `Shell` test is evaluated
/// by `sh` as a last resort.
fn eval_condition(repo: &Repo, cond: &crate::plan::step::Condition) -> bool {
    use crate::plan::step::Condition;
    let p = |f: &str| repo.dir.join(f);
    match cond {
        Condition::FileExists(f) => p(f).exists(),
        Condition::FileNonEmpty(f) => std::fs::metadata(p(f)).map(|m| m.len() > 0).unwrap_or(false),
        Condition::FileMissing(f) => !p(f).exists(),
        Condition::DirExists(f) => p(f).is_dir(),
        Condition::Shell(c) => std::process::Command::new("sh")
            .arg("-c")
            .arg(c)
            .current_dir(&repo.dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
    }
}

/// Run a step pipeline over `model`, threading the model through each `Op` and
/// recursing into `Branch` steps (the condition is evaluated natively; the
/// matching body runs in the same pipeline). Out-of-pipeline steps run in place
/// via [`run_shell_step_in_pipeline`], so one shell line does not cost the rule
/// its native execution. `target` is the rule's output path (relative to the
/// ontology dir), needed only so a shell step that rewrites it round-trips.
/// `OM_DUMP_STEPS=<substring>`: write the PIPED model after each step of a
/// matching target, as `<OM_DUMP_DIR>/<n>-<file>`. A step can be byte-perfect on a
/// FILE and behave differently on a piped model — re-reading an RDF/XML
/// intermediate creates declarations the piped model never had (242 object
/// properties in the `-basic` chain) — so a stepwise reproduction from disk does
/// not prove the pipeline.
fn dump_step(target: &str, model: &crate::model::Model) {
    let Ok(want) = std::env::var("OM_DUMP_STEPS") else { return };
    if !target.contains(want.trim()) {
        return;
    }
    let dir = std::env::var("OM_DUMP_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let base = Path::new(target)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "target".to_string());
    let n = DUMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = Path::new(&dir).join(format!("{n:02}-{base}"));
    let mut m = model.clone();
    let _ = crate::io::save(&mut m, &path);
    crate::status!("step dump: {}", path.display());
}

/// Sequence number for `OM_DUMP_STEPS` dumps.
static DUMP_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

fn run_steps(
    repo: &Repo,
    steps: &[Step],
    model: crate::model::Model,
    catalog: &BTreeMap<String, PathBuf>,
    work: &Path,
    target: Option<&str>,
    // Whether the caller serializes the returned model to the target afterwards.
    // When it does, a closing `mv $@.tmp $@` really is the output bookkeeping that
    // write stands in for; when it does not, nothing else will put the file there.
    writes_model_after: bool,
    // The file the model handed in was loaded from, if any — see `apply_op`.
    pipeline_input: Option<&Path>,
) -> Result<crate::model::Model> {
    // A `Fallback` runs only when what precedes it failed (shell `||`). Split there
    // and recurse: the head is the "try" block, and its failure — not its success —
    // is what reaches the fallback. Rare enough that cloning the model to keep a
    // continuation available costs nothing on the common path.
    if let Some(pos) = steps.iter().position(|s| matches!(s, Step::Fallback { .. })) {
        let (head, rest) = steps.split_at(pos);
        let Step::Fallback { command, .. } = &rest[0] else { unreachable!() };
        let spare = model.clone();
        let after = match run_steps(
            repo, head, model, catalog, work, target, writes_model_after, pipeline_input,
        ) {
            Ok(m) => m,
            Err(head_err) => {
                // The fallback IS the error path, so its own exit status decides the
                // outcome: a `|| { cat $@ && exit 1; }` re-raises, while a
                // `|| <recovery>` that succeeds means the recipe line succeeded.
                recipe::run_step_command(command, &repo.dir, &repo.var("ROBOT"))
                    .map_err(|_| head_err)?;
                spare
            }
        };
        return run_steps(
            repo, &rest[1..], after, catalog, work, target, writes_model_after, pipeline_input,
        );
    }
    let mut model = model;
    let mut model_on_disk = false;
    let mut staged_by_shell = false;
    // The file the CURRENT invocation's model was loaded from. It changes at every
    // `Boundary`, so it is state rather than a parameter: the ops after a boundary
    // must see the new invocation's input, not the one the caller opened with.
    let mut pipe: Option<PathBuf> = pipeline_input.map(|p| p.to_path_buf());
    for (step_ix, step) in steps.iter().enumerate() {
        match step {
            // A new invocation. It shares nothing with the last but files, so the
            // model it starts from is its OWN `--input` — never what the previous
            // command line happened to leave in memory.
            Step::Boundary { input } => {
                // A new command line is a new run, so its blank nodes number from
                // the start again.
                crate::io::reset_anon_counter();
                match input.as_deref() {
                    Some(first) if first.starts_with("http://") || first.starts_with("https://") => {
                        model = crate::io::load_iri(first, None)?;
                        pipe = None;
                    }
                    Some(first) => {
                        if resolve_repo_file(repo, first, work).is_none()
                            && repo.target(first).is_some()
                        {
                            let mut s = std::collections::HashSet::new();
                            run_target_recipe_inner(repo, first, &mut s)
                                .with_context(|| format!("building invocation input {first}"))?;
                        }
                        let p = resolve_repo_file(repo, first, work).with_context(|| {
                            format!("invocation input `{first}` does not exist and cannot be built")
                        })?;
                        model = crate::io::load(&p)?;
                        pipe = Some(p);
                    }
                    // An invocation that names no input of its own starts from
                    // nothing; its steps build the model themselves.
                    None => {
                        model = crate::model::Model::new();
                        pipe = None;
                    }
                }
                model_on_disk = false;
                staged_by_shell = false;
            }
            Step::Op(op) | Step::Partial { op, .. } => {
                model = apply_op(repo, op, model, catalog, work, None, pipe.as_deref())?;
                if let Some(t) = target {
                    dump_step(t, &model);
                }
                write_step_output(repo, op, &mut model, target)?;
                model_on_disk = false;
                staged_by_shell = false;
            }
            Step::Branch { condition, then_steps, else_steps } => {
                let body = if eval_condition(repo, condition) { then_steps } else { else_steps };
                model =
                    run_steps(repo, body, model, catalog, work, target, writes_model_after, pipe.as_deref())?;
            }
            // `cmd || true`. The model is kept aside so a failed step leaves the
            // pipeline exactly as it was rather than half-applied, and the failure
            // is REPORTED: the recipe tolerates it, which is not a reason for the
            // build to be quiet about having done less than it says.
            Step::MayFail(inner) => {
                let spare = model.clone();
                model = match run_steps(
                    repo,
                    std::slice::from_ref(inner.as_ref()),
                    model,
                    catalog,
                    work,
                    target,
                    writes_model_after,
                    pipe.as_deref(),
                ) {
                    Ok(m) => m,
                    Err(e) => {
                        status!("(tolerated) {}: {e:#}", inner.label());
                        spare
                    }
                };
            }
            // Side-effect file ops (append/sort/print) genuinely mutate the
            // filesystem later steps read, so run them; output-bookkeeping ops
            // (cp/mv of the target) are no-ops here — the model write handles them.
            //
            // A `cp`/`mv` that does NOT name this target is neither: it is work the
            // rule asked for and nothing else performs. EFO's
            // `release: … ; cp $^ $(RELEASEDIR)` publishes the four artefacts to
            // the repository root; falling off the end of this match would drop
            // it, and the build would produce every artefact and ship none of them.
            Step::File(op) if !op.is_side_effect() && !names_target(op, target) => {
                run_file_op(repo, op)?;
            }
            Step::File(op) => {
                if op.is_side_effect() {
                    run_file_op(repo, op)?;
                } else if let Some(dst) =
                    staged_target(repo, op, target).filter(|_| staged_by_shell || !writes_model_after)
                {
                    // A `cp`/`mv` onto the target is output bookkeeping the final
                    // model write stands in for — but only when it is the rule's
                    // LAST word on the subject. MONDO's `filtered.obo` recipe runs
                    // two perl filters, each `… > $@.tmp && mv $@.tmp $@`, and only
                    // THEN an op pipeline reading `$@`. Skipping those two moves
                    // leaves the pipeline threading the model it loaded from `$<`,
                    // so every later op runs on the unfiltered source and the 7,910
                    // `xref:`s the perl deletes come back — one of them,
                    // `ICD10CM:C85\,7`, making the `skos.ttl` built from it invalid
                    // Turtle.
                    //
                    // So when the shell really did stage a file, perform the move
                    // and re-read the target: what is on disk is now what the rest
                    // of the recipe operates on. A closing `mv $@.tmp $@` over a
                    // temp file the pipeline never wrote still falls through to the
                    // final write.
                    run_file_op(repo, op)?;
                    // Re-read only where a later op will operate on it. The
                    // re-read exists to hand the REST of the recipe what is now on
                    // disk; with nothing left to hand it to, a target whose
                    // extension merely looks like an ontology must not be parsed as
                    // one. `tmp/obo.epm.json` is a prefix map that a `.json` reader
                    // rejects, and the recipe that copies it has no later op.
                    let later_op = steps[step_ix + 1..]
                        .iter()
                        .any(|s| matches!(s, Step::Op(_) | Step::Partial { .. }));
                    if later_op && crate::io::Format::from_path(Path::new(&dst)).is_ok() {
                        model = crate::io::load(&repo.dir.join(&dst))
                            .with_context(|| format!("re-reading {dst} after a staged move"))?;
                        model_on_disk = true;
                    }
                }
            }
            Step::Inert(_) => {} // no observable effect; never reaches a plan
            Step::UnsupportedSubcommand(name) => {
                bail!("recipe names the ontology subcommand `{name}`, which owlmake does not implement")
            }
            s if is_shell_step(s) => {
                model = run_shell_step_in_pipeline(
                    repo, s, model, target, work, model_on_disk, pipeline_input,
                )?;
                model_on_disk = false;
                staged_by_shell = true;
            }
            // A recorded subcommand owlmake cannot run fails by NAME, not as an
            // "internal" error: the plan says exactly which command the recipe
            // wanted, and that is the message the failure has to carry.
            Step::UnsupportedSubcommand(name) => {
                bail!("unsupported ontology subcommand `{name}`: this step has no owlmake implementation")
            }
            other => bail!("internal: uncovered step reached executor: {}", other.label()),
        }
    }
    Ok(model)
}

/// Return a copy of `steps` with the edit-signature term file appended to the
/// seed of the first `extract`/`filter` op (the fallback seeding).
fn inject_editsig_seed(steps: &[Step], editsig: &str) -> Vec<Step> {
    use crate::plan::step::Op;
    let mut out = Vec::with_capacity(steps.len());
    let mut injected = false;
    for step in steps {
        if injected {
            out.push(step.clone());
            continue;
        }
        match step {
            Step::Op(Op::Extract {
                method, terms, term_files, copy_ontology_annotations, individuals,
                branch_from_terms, branch_from_term_files,
            }) => {
                injected = true;
                let mut tf = term_files.clone();
                tf.push(editsig.to_string());
                out.push(Step::Op(Op::Extract {
                    method: method.clone(),
                    terms: terms.clone(),
                    term_files: tf,
                    copy_ontology_annotations: *copy_ontology_annotations,
                    individuals: individuals.clone(),
                    branch_from_terms: branch_from_terms.clone(),
                    branch_from_term_files: branch_from_term_files.clone(),
                }));
            }
            Step::Op(Op::Filter(spec)) => {
                injected = true;
                let mut spec = spec.clone();
                spec.term_files.push(editsig.to_string());
                out.push(Step::Op(Op::Filter(spec)));
            }
            _ => out.push(step.clone()),
        }
    }
    out
}

/// The directory mirrors live in — the plan's `$(MIRRORDIR)` when the repo sets
/// one, the conventional `mirror/` otherwise.
fn mirror_dir(repo: &Repo) -> PathBuf {
    let d = repo.var("MIRRORDIR");
    repo.dir.join(if d.is_empty() { "mirror" } else { d })
}

/// Produce `mirror/<id>.owl` for one import, and return its path.
///
/// This is the ONLY thing that makes a mirror. `mirror/<id>.owl` carries no plan
/// rule of its own (see `native_mirror_targets`); what a mirror IS lives on the
/// import: `source` says where the bytes come from and `mirror_steps` says what
/// the repo does to them before they become the mirror. A mirror is not a plain
/// download — the mirror rule runs the file through a `convert`, and a
/// `make_base` product through `remove --axioms external --base-iri <base>` —
/// so writing the wire bytes straight to `mirror/<id>.owl` builds the module from
/// the wrong ontology.
///
/// `refresh` is this run's `MIR`: false pins whatever is on disk, true re-runs the
/// pipeline. Each mirror is built at most once per run.
fn ensure_mirror(repo: &Repo, imp: &crate::plan::ImportPlan, refresh: bool) -> Result<PathBuf> {
    let mirror_dir = mirror_dir(repo);
    std::fs::create_dir_all(&mirror_dir)?;
    let dest = mirror_dir.join(format!("{}.owl", imp.id));
    // A `<custom mirror script>` source means only "there is no single URL to
    // GET" — it says nothing about whether the plan recorded the recipe. When it
    // did, those steps are an ordinary model pipeline and belong on the common
    // path below: MONDO's `mirror-uberon` is `fetch` → `convert` → `remove` →
    // `round-trip` → `mv` → `cp`, and running that list as if every entry were a
    // shell command dies on `internal: convert[owl] is not an out-of-pipeline
    // step`. Short-circuiting here would also skip the `refresh` check, so
    // `MIR=true` would silently keep whatever `mirror/<id>.owl` is already on disk.
    let custom = imp.source == "<custom mirror script>";

    // Already made this run, or pinned by `MIR=false` — either way it is final.
    let once = format!("\u{1}mirror:{}", imp.id);
    if dest.exists() && (!refresh || repo.built.borrow().contains(&once)) {
        if !refresh && !crate::progress::stage_active() {
            status!("mirror: {} pinned (MIR=false), reusing {}", imp.id, dest.display());
        }
        return Ok(dest);
    }
    // `MIR=false` pins the mirrors whether or not they are on disk. Gating the pin
    // above on `dest.exists()` meant a repo that commits NO mirrors — UBERON —
    // fell straight through to the download, so `MIR=false` fetched every upstream
    // while the ODK, whose mirror rules do not exist under that flag, fetched
    // nothing. A pinned input that is absent is an error, not a licence to go and
    // get a different one (P5).
    if !refresh {
        if repo.mirrors_pinned {
            bail!(
                "mirror `{}` is pinned by MIR=false but {} is not present. \
                 Under MIR=false the mirror rules do not exist, so there is nothing to fetch it \
                 with — re-run with MIR=true (or `--rebuild mirrors`) to download it",
                imp.id,
                dest.display()
            );
        }
        // Kept by the group's default and absent: there is no pinned copy to
        // build against, so the fetch is the only way any consumer proceeds.
        // The explicit `MIR=false` above still refuses — that pin was stated
        // about this run (P5) — and the fetch announces itself.
        status!("make: mirror `{}` is kept by default but absent — fetching it this once", imp.id);
    }
    if !repo.built.borrow_mut().insert(once) && dest.exists() {
        return Ok(dest);
    }
    if !imp.mirror_steps.is_empty() {
        run_mirror_pipeline(repo, imp, &dest)?;
        if !dest.exists() {
            bail!(
                "import `{}`: the plan's mirror steps produced no {}",
                imp.id,
                dest.display()
            );
        }
        return Ok(dest);
    }
    if custom {
        // No recorded steps: the recipe may still be reachable as planned
        // targets (`mirror/<id>.owl` and its phony `mirror-<id>`).
        run_custom_mirror(repo, &imp.id, &imp.mirror_steps)?;
        if !dest.exists() {
            bail!(
                "import `{}` custom mirror recipe produced no mirror/{}.owl (and no cached {})",
                imp.id, imp.id, imp.output
            );
        }
        return Ok(dest);
    }
    if !crate::progress::stage_active() {
        status!("mirror: downloading {} ← {}", imp.id, imp.source);
    }
    let bytes = http_get(&imp.source)?;
    std::fs::write(&dest, &bytes)?;
    Ok(dest)
}

/// Run an import's recorded `mirror_steps` — the repo's own mirror recipe — to
/// produce `mirror/<id>.owl`.
///
/// The steps are an ordinary model pipeline with one twist: they OPEN with the
/// file ops that fetch what the pipeline then reads (`curl … -o
/// tmp/<id>-download.owl`), so the input cannot be resolved before running them.
/// Run those first, take the last file they wrote as the pipeline's input — and
/// where there is none, the recipe read the IRI directly (`convert -I <url>`),
/// which the plan records as the import's `source`.
fn run_mirror_pipeline(repo: &Repo, imp: &crate::plan::ImportPlan, dest: &Path) -> Result<()> {
    use crate::build::recipe::FileOp;

    let catalog = load_catalog_planned(repo);
    let work = repo.dir.join(".owlmake-odk-tmp");
    std::fs::create_dir_all(&work)?;

    // What the recipe reads but does not make: build it from its own rule first.
    // `mirror-hgnc` reads `mirror/hgnc_gene.nt` and `mirror-ncbigene` reads
    // `mirror/ncbi_gene.nt`, each a `curl … | gzip -d` in a rule of its own.
    for input in &imp.mirror_inputs {
        // `mirror_inputs` is stored repo-root-relative (the plan rebases paths);
        // target names are relative to the ontology dir, which is where both the
        // rule lookup and the file live. `src/sparql/x` is `../sparql/x` from here.
        let rel: String = match input.strip_prefix("src/ontology/") {
            Some(r) => r.to_string(),
            None => match input.strip_prefix("src/") {
                Some(r) => format!("../{r}"),
                None => input.clone(),
            },
        };
        if !repo.dir.join(&rel).exists() {
            ensure_built(repo, &rel, 8)
                .with_context(|| format!("import `{}`: building mirror input {rel}", imp.id))?;
        }
    }

    let mut rest = imp.mirror_steps.as_slice();
    let mut fetched: Option<PathBuf> = None;
    while let Some(Step::File(op)) = rest.first() {
        run_file_op(repo, op)?;
        if let FileOp::Fetch { dst, .. } = op {
            fetched = Some(repo.dir.join(dst));
        }
        rest = &rest[1..];
    }

    // A recipe that threads no model at all — MONDO's `mirror-ncbigene` is one
    // SPARQL line over the raw triples and one `cp` — has no pipeline input to
    // resolve. Run it as it stands rather than inventing one.
    if rest.iter().all(|s| matches!(s, Step::Shell { .. } | Step::File(_))) {
        for step in rest {
            match step {
                Step::File(op) => run_file_op(repo, op)?,
                s => run_shell_step(repo, s)?,
            }
        }
        return Ok(());
    }

    let src = match fetched {
        Some(p) => p,
        None => {
            // A recipe that fetches nothing may still NAME its input:
            // `mirror-hgnc` opens `merge -i mirror/hgnc_gene.nt`, built just
            // above from `mirror_inputs`. Read it, and let `pipeline_input` stop
            // the `merge` re-reading the same file.
            if let Some(Step::Op(crate::plan::step::Op::Merge { inputs, .. })) = rest.first() {
                let Some(first) = inputs.first() else {
                    bail!("import `{}`: its mirror recipe merges nothing", imp.id)
                };
                let rel = first.strip_prefix("src/ontology/").unwrap_or(first);
                let p = repo.dir.join(rel);
                if !p.exists() {
                    bail!("import `{}`: its mirror recipe reads {rel}, which nothing made", imp.id);
                }
                p
            } else if imp.source == "<custom mirror script>" {
                bail!(
                    "import `{}`: its mirror steps fetch nothing and it has no source IRI, \
                     so there is no input to build mirror/{}.owl from",
                    imp.id, imp.id
                );
            } else {
                if !crate::progress::stage_active() {
                    status!("mirror: {} \u{2190} {}", imp.id, imp.source);
                }
                let bytes = http_get(&imp.source)?;
                let p = work.join(format!("{}-download.owl", imp.id));
                std::fs::write(&p, &bytes)?;
                p
            }
        }
    };
    let model = crate::io::load(&src)?;
    let rel = dest.strip_prefix(&repo.dir).unwrap_or(dest).to_string_lossy().to_string();
    // `writes_model_after: false` — the recipe's own closing `cp
    // tmp/mirror-<id>.owl mirror/<id>.owl` is what puts the file there, and
    // nothing else will.
    run_steps(repo, rest, model, &catalog, &work, Some(&rel), false, Some(&src))
        .with_context(|| format!("building mirror for import `{}`", imp.id))?;
    Ok(())
}

/// Produce `mirror/<id>.owl` for a `mirror_type: custom` import by running the
/// project's own mirror recipe (with `$(ROBOT)` resolving to the owlmake binary),
/// exactly as `om make MIR=true mirror/<id>.owl` would. Such a rule is
/// conventionally named `$(MIRRORDIR)/<id>.owl` (ingest records it
/// unconditionally, flattening the `ifeq ($(MIR),true)` guard), typically with a
/// phony `mirror-<id>` prerequisite that does the download and processing;
/// `ensure_built` builds that prereq first, then runs the copy rule.
fn run_custom_mirror(repo: &Repo, id: &str, recorded: &[Step]) -> Result<()> {
    let target = format!("mirror/{id}.owl");
    // The steps the plan recorded for this mirror, which is the whole point of
    // `imports[].mirror_steps`: a custom mirror is a project script owlmake
    // cannot synthesize from the product's flags. It threads no model (its
    // commands name their own `-i`/`-o`), so every step carries a full command
    // line and is run as one.
    if !recorded.is_empty() {
        status!("mirror: {id} (custom) — running the recorded mirror steps");
        for step in recorded {
            match step {
                Step::Shell { command: _, .. } | Step::File(_) => {
                    if let Step::File(op) = step {
                        run_file_op(repo, op)?;
                    }
                }
                s => run_shell_step(repo, s)?,
            }
        }
        return Ok(());
    }
    // Otherwise the plan may carry the mirror as ordinary targets: the rule is
    // conventionally named `$(MIRRORDIR)/<id>.owl` with a phony `mirror-<id>`
    // prerequisite doing the download and processing.
    let phony = format!("mirror-{id}");
    if repo.target(&target).is_some() || repo.target(&phony).is_some() {
        status!("mirror: {id} (custom) — running planned mirror target `{target}`");
        ensure_built(repo, &target, 8)?;
        // Some projects only define the phony rule (which writes the mirror
        // itself); run it directly if the file target didn't materialize one.
        if !repo.dir.join(&target).exists() && repo.target(&phony).is_some() {
            run_target_recipe_planned(repo, &phony)?;
        }
        return Ok(());
    }
    bail!(
        "import `{id}` uses a custom mirror but the plan records no steps for it and no `{target}` / `{phony}` target; provide a cached mirror/{id}.owl"
    )
}

/// Write the edit ontology's signature (one IRI per line) to a temp term file
/// under `work`, returning its absolute path for use as an extract seed. Returns
/// `None` when the repo has no readable edit ontology.
fn write_edit_signature(repo: &Repo, work: &Path, id: &str) -> Result<Option<String>> {
    let Some(srcfile) = edit_file(repo) else { return Ok(None) };
    let Ok(m) = crate::io::load(&srcfile) else { return Ok(None) };
    let mut seed = std::collections::BTreeSet::new();
    for ac in m.ont.iter() {
        seed.extend(crate::sig::signature(&ac.component));
    }
    std::fs::create_dir_all(work)?;
    let path = work.join(format!("{id}_editsig_terms.txt"));
    std::fs::write(&path, seed.into_iter().collect::<Vec<_>>().join("\n"))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    crate::io::http_get(url)
}

pub(crate) fn empty_model() -> crate::model::Model {
    crate::model::Model::from_parts(
        horned_owl::ontology::set::SetOntology::new(),
        crate::model::default_prefixes(),
    )
}

fn merge_model_into(model: &mut crate::model::Model, other: crate::model::Model) {
    use horned_owl::model::{Component, MutableOntology};
    for ac in other.ont.iter() {
        if matches!(
            ac.component,
            Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_)
        ) {
            continue;
        }
        model.ont.insert(ac.clone());
    }
    for (prefix, value) in other.prefixes.mappings() {
        let _ = model.prefixes.add_prefix(prefix, value);
    }
    // RDF/XML sources surface their `xmlns:` bindings in `idspaces`, not the
    // formal prefix map — carry those too, so a component's non-OBO prefix (e.g.
    // a brain-atlas taxonomy `CCN20230722`, or `swrl`) survives the merge and can
    // reach the OBO `idspace:` header.
    for (prefix, ns) in &other.idspaces {
        let _ = model.prefixes.add_prefix(prefix, ns);
    }
}

/// Apply the plan's `exclude_iri_patterns` — the `remove --select "<…/GOCHE_*>"`
/// chain the import recipe runs, one `remove` per pattern, with the command's own
/// defaults (`--trim true --preserve-structure true`).
///
/// This is `remove`, not a prefix filter over every IRI a component mentions, and
/// the two differ in both directions:
///
///  * `remove` resolves the pattern against the ontology's SIGNATURE, so an IRI
///    that names no entity selects nothing. FoodOn's `hasDbXref` values point at
///    DRON IRIs the merged mirror never declares, and a textual prefix match
///    would take 32 assertions the module has to keep.
///  * `--preserve-structure` is on by default, so removing a class BRIDGES the
///    hierarchy across it (`spanGaps`): every FoodOn class under the excluded
///    `PO_`/`CL_`/`OBI_` scaffolding is re-attached to the surviving superclasses
///    of what was removed. That is 168 `SubClassOf` axioms in ECTO's merged
///    import that a filter cannot produce at all, because they are in no input.
fn drop_excluded(model: crate::model::Model, plan: &Plan) -> Result<crate::model::Model> {
    // One `remove` over the union of the patterns: every pattern selects
    // against the same signature, and `--preserve-structure` bridges each
    // removed class to its surviving superclasses whether the set is removed
    // in one pass or sixteen, so the result is the chain's result at a
    // sixteenth of the cost.
    let patterns: Vec<String> = plan
        .exclude_iri_patterns
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if patterns.is_empty() {
        return Ok(model);
    }
    crate::cmd::remove::remove(model, &[], &[], &patterns, &[], &[])
}

/// The merged-import seed: the union of every import's declared seed term file
/// (resolved to its committed source, e.g. `iri_dependencies/<id>_terms.txt`)
/// plus the signature of the edit ontology (what the release actually imports).
/// Driven by the plan rather than a directory glob, so a stale `*_terms.txt` for
/// a product no longer configured can't leak into the seed.
fn import_seed(repo: &Repo, plan: &Plan) -> Result<std::collections::HashSet<String>> {
    let mut seed = std::collections::HashSet::new();
    for imp in &plan.imports {
        for tf in imp.seed_term_files() {
            for line in std::fs::read_to_string(repo.dir.join(&tf)).unwrap_or_default().lines() {
                // A `*_terms.txt` lists seed terms as OBO CURIEs (`GO:0000279`),
                // not IRIs — `extract --term-file` expands them via the prefix
                // map. Accepting only `http…` here silently drops every committed
                // seed term, so whole branches (and their transitive ⊥-module
                // closure) are never pulled in.
                let Some(t) = crate::cmd::select::term_line(line) else { continue };
                let t = t.trim_start_matches('<').trim_end_matches('>');
                seed.insert(crate::cmd::babelon::expand_curie(t));
            }
        }
    }
    // The import ⊥-module is seeded from `pre_seed.txt`, the signature of
    // `SRCMERGED` = the edit ontology **plus every `$(OTHER_SRC)` component**
    // (patterns/definitions.owl, mappings.owl, the *_upper_slim / subset
    // components, …). Those components reference external UBERON/GO/PR/… terms
    // (e.g. mappings.owl's xref targets, definitions.owl's DOSDP fillers) that
    // must be pulled into the module — seeding from the edit file alone leaves
    // whole branches (and everything they transitively pull) out.
    let mut sources: Vec<PathBuf> = Vec::new();
    if let Some(src) = edit_file(repo) {
        sources.push(src);
    }
    sources.extend(other_src(repo, &[], &repo.dir.join(".owlmake-odk-tmp"))?);
    for (i, src) in sources.iter().enumerate() {
        // A merge keeps the first input and folds every later one INTO it, so with
        // `--include-annotations false` (the default) the FIRST input keeps its
        // own ontology annotations and every later one loses them. `SRCMERGED`
        // puts the edit file first, so `terms.sparql` sees the
        // edit's header — and seeds `dc:creator`, `dc:title`, `dcterms:license`,
        // `oboInOwl:default-namespace`, `IAO_0000700` … which is what keeps their
        // declarations in `merged_import.owl` through the annotation-property
        // strip. A merged COMPONENT's header (definitions.owl's `dc:type
        // IAO_8000001` base-module tag) is dropped and must not be seeded.
        let is_primary = i == 0;
        if let Ok(m) = crate::io::load(src) {
            for ac in m.ont.iter() {
                if matches!(
                    ac.component,
                    horned_owl::model::Component::OntologyID(_)
                        | horned_owl::model::Component::DocIRI(_)
                ) {
                    continue;
                }
                if matches!(ac.component, horned_owl::model::Component::OntologyAnnotation(_))
                    && !is_primary
                {
                    continue;
                }
                // `all_iris`, not `signature`: `terms.sparql` seeds every
                // referenced IRI, including annotation-value targets (xrefs,
                // mappings) the logical signature omits.
                seed.extend(crate::sig::all_iris(&ac.component));
                // `all_iris` walks the bare component, not the axiom annotations
                // on the wrapper. srcmerged carries a declaration for every used
                // annotation property (so `terms.sparql` seeds it), which keeps a
                // property used only as an axiom annotation in the edit —
                // `oboInOwl:source` on xrefs/synonyms — out of the import strip.
                for a in ac.ann.iter() {
                    seed.insert(a.ap.0.as_ref().to_string());
                    if let horned_owl::model::AnnotationValue::IRI(i) = &a.av {
                        seed.insert(i.as_ref().to_string());
                    }
                }
            }
        }
    }
    seed.extend(pattern_seed_terms(repo));
    // `terms.sparql` selects TRIPLE SUBJECTS and OBJECTS, never predicates, so an
    // annotation property reaches the seed through its DECLARATION's `rdf:type`
    // triple — and `$(SRCMERGED)` is written with a synthesized declaration for
    // every signature entity EXCEPT a built-in one. So a built-in property used
    // only as a predicate, and not declared by hand, is in no triple's subject or
    // object position and is NOT seeded: ECTO's `components/obsoletes.owl` uses
    // `owl:deprecated` that way, and seeding it would keep
    // `AnnotationAssertion(owl:deprecated ONS_0000094 "true")` that the
    // annotation-property strip removes. `rdfs:label` and `rdfs:comment` are
    // built-in too and DO stay — `ecto-edit.owl` declares them.
    let declared = declared_in_sources(&sources);
    seed.retain(|t| !BUILT_IN_ANNOTATION_PROPERTIES.contains(&t.as_str()) || declared.contains(t));
    if let Ok(path) = std::env::var("OM_DUMP_SEED") {
        let mut sorted: Vec<&String> = seed.iter().collect();
        sorted.sort();
        let body: String = sorted.iter().map(|s| format!("{s}\n")).collect();
        let _ = std::fs::write(path, body);
    }
    Ok(seed)
}

/// The OWL 2 built-in annotation properties: part of the reserved vocabulary, so
/// no declaration is ever synthesized for them on write.
const BUILT_IN_ANNOTATION_PROPERTIES: &[&str] = &[
    "http://www.w3.org/2000/01/rdf-schema#label",
    "http://www.w3.org/2000/01/rdf-schema#comment",
    "http://www.w3.org/2000/01/rdf-schema#seeAlso",
    "http://www.w3.org/2000/01/rdf-schema#isDefinedBy",
    "http://www.w3.org/2002/07/owl#versionInfo",
    "http://www.w3.org/2002/07/owl#backwardCompatibleWith",
    "http://www.w3.org/2002/07/owl#priorVersion",
    "http://www.w3.org/2002/07/owl#incompatibleWith",
    "http://www.w3.org/2002/07/owl#deprecated",
];

/// Every entity these sources DECLARE outright (as against merely reference).
fn declared_in_sources(sources: &[PathBuf]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for src in sources {
        let Ok(m) = crate::io::load(src) else { continue };
        for ac in m.ont.iter() {
            if let horned_owl::model::Component::DeclareAnnotationProperty(d) = &ac.component {
                out.insert(d.0 .0.as_ref().to_string());
            }
        }
    }
    out
}

/// The DOSDP half of the import seed. `$(IMPORTSEED)` is `$(PRESEED)` **plus
/// `$(TMPDIR)/all_pattern_terms.txt`**, and under the default `PAT = true` that
/// second file is
///
/// ```text
/// cat $(DOSDP_TERM_FILES_DEFAULT) $(DOSDP_TERM_FILES_FULL) $(TMPDIR)/pattern_owl_seed.txt
/// ```
///
/// `pattern_owl_seed.txt` is `terms.sparql` run over `$(PATTERNDIR)/pattern.owl`,
/// the DOSDP prototype rendered from the whole `dosdp-patterns/`
/// directory. So the seed carries every class and relation the *patterns
/// themselves* name — the PATO qualities, the BFO/RO relations, the GO/UBERON
/// ranges — whether or not any data row has instantiated them yet.
///
/// Seeding from the edit file alone leaves all of that out. On HPO that is 313
/// terms, and their absence both drops `BFO_0000001` from `merged_import.owl`
/// and leaves stray CHEBI branches in it, because a ⊥-module over a different
/// signature is a different module — not a subset of the right one.
fn pattern_seed_terms(repo: &Repo) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let dir = match repo.var("PATTERNDIR") {
        "" => repo.dir.join("../patterns"),
        v => repo.dir.join(v),
    };
    let templates = dir.join("dosdp-patterns");
    if !templates.is_dir() {
        return out;
    }
    // `prototype --template=<dir>` renders every pattern in the directory into
    // one ontology; the seed only wants that ontology's signature, so render
    // them one at a time and union the IRIs rather than building pattern.owl.
    let labels = std::collections::HashMap::new();
    // The template SET and the (template, table) pairs below are the PLAN's; the
    // directory sweep is only for a repo whose plan carries no DOSDP section at
    // all.
    let dosdp = repo.plan.dosdp.as_ref();
    let mut yamls: Vec<PathBuf> = match dosdp.filter(|d| !d.templates.is_empty()) {
        Some(d) => d.templates.iter().map(|t| repo.dir.join(t)).collect(),
        None => std::fs::read_dir(&templates)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("yaml"))
            .collect(),
    };
    yamls.sort();
    for y in &yamls {
        let Ok(text) = std::fs::read_to_string(y) else { continue };
        let Ok(m) = crate::dosdp::prototype(&text, &labels) else { continue };
        for ac in m.ont.iter() {
            if matches!(
                ac.component,
                horned_owl::model::Component::OntologyAnnotation(_)
                    | horned_owl::model::Component::OntologyID(_)
                    | horned_owl::model::Component::DocIRI(_)
            ) {
                continue;
            }
            out.extend(crate::sig::all_iris(&ac.component));
        }
    }
    // `$(DOSDP_TERM_FILES_*)` — the term list of each data table, which adds the
    // fillers the rows actually reference (and the minted classes). One pair per
    // module the plan names, across every data directory the generator runs over.
    let pairs: Vec<(PathBuf, PathBuf)> = match dosdp {
        Some(d) => d
            .patterns
            .iter()
            .map(|pat| (repo.dir.join(&pat.template), repo.dir.join(&pat.data)))
            .collect(),
        None => std::fs::read_dir(dir.join("data/default"))
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("tsv"))
            .filter_map(|p| {
                let stem = p.file_stem()?.to_str()?.to_string();
                Some((templates.join(format!("{stem}.yaml")), p))
            })
            .collect(),
    };
    for (tpl, data) in pairs {
        let Ok(yaml) = std::fs::read_to_string(&tpl) else { continue };
        let Ok(tsv) = std::fs::read_to_string(&data) else { continue };
        if let Ok(terms) = crate::dosdp::terms(&yaml, &tsv) {
            out.extend(terms);
        }
    }
    out
}

/// The import rule's `remove --select complement --select annotation-properties`
/// over the ⊥-module: drop every annotation property NOT in `keep` — its
/// declaration and its annotation assertions — while leaving every other axiom
/// (and its axiom annotations) intact.
///
/// Applies the `--trim true --preserve-structure true` defaults:
///
/// - An annotation property's declaration is dropped.
/// - An `AnnotationAssertion` is dropped when its own property is removed OR it
///   carries a removed axiom annotation — the whole assertion goes (an
///   annotation has no logical structure to preserve).
/// - A *logical* axiom carrying a removed axiom annotation keeps its bare form
///   but loses ALL its annotations: the annotated axiom is removed, then
///   `--preserve-structure` re-adds the unannotated one. So a `{notes=…,
///   source=…}` on a `relationship`/`is_a` (notes removed) collapses to the
///   plain clause — not `{source=…}`.
/// - A logical axiom whose annotations are all kept is left untouched.
///
/// Hand-rolled rather than routed through `cmd::remove`: the removal set is fixed
/// by the explicit `keep` set of entity IRIs the caller hands in (the configured
/// `ANNOTATION_PROPERTIES` plus every seed term) rather than resolved from
/// `--select` groups, and the rules above — together with the subject-side rule
/// below — are the whole specification of what an import module may carry.
/// Folding this into the general selector machinery has to reproduce every one of
/// them.
fn strip_import_annotation_properties(
    model: crate::model::Model,
    keep: &std::collections::HashSet<String>,
) -> crate::model::Model {
    use horned_owl::model::{AnnotationValue, ClassExpression, Component, MutableOntology};
    let prefixes = crate::model::clone_prefixes(&model.prefixes);
    // The annotation properties being removed — every one in the signature that is
    // not kept. `--select complement --select annotation-properties` removes the
    // ENTITY, so an axiom is taken whenever the property appears anywhere in its
    // signature, not only as an assertion's own property: a
    // `SubAnnotationPropertyOf(uberon/core#HOMOLOGY, SynonymTypeProperty)` goes, and
    // so does a synonym whose `hasSynonymType` VALUE names it. Matching on the
    // property alone would leave all sixteen `uberon/core#HOMOLOGY` occurrences in
    // `merged_import.owl`, and they would reach hp-full/hp/hp-international.
    let removed: std::collections::HashSet<String> = crate::cmd::select::signature_entities(&model)
        .annotation_properties
        .into_iter()
        .filter(|p| !keep.contains(p))
        .collect();
    let ann_hits_removed = |a: &horned_owl::model::Annotation<crate::model::Str>| {
        removed.contains(a.ap.0.as_ref())
            || matches!(&a.av, AnnotationValue::IRI(i) if removed.contains(i.as_ref()))
    };
    let mut out = horned_owl::ontology::set::SetOntology::new();
    for ac in model.ont.iter() {
        let has_removed_ann =
            ac.ann.iter().any(|a| !keep.contains(a.ap.0.as_ref()) || ann_hits_removed(a));
        match &ac.component {
            Component::DeclareAnnotationProperty(d) => {
                if keep.contains(d.0 .0.as_ref()) {
                    out.insert(ac.clone());
                }
            }
            // Annotation assertions have no logical structure to preserve — a
            // removed own-property, a removed axiom annotation, OR a SUBJECT that
            // is itself one of the removed properties drops the whole assertion.
            // The SUBJECT is tested first, so `IAO_0000116`'s own label and
            // definition go with the property; keying on the assertion's property
            // alone would leave seven such assertions about `IAO_0000116`,
            // `RO_0002581` and `RO_0002582` in the module.
            Component::AnnotationAssertion(aa) => {
                let subject_removed = matches!(
                    &aa.subject,
                    horned_owl::model::AnnotationSubject::IRI(i) if removed.contains(i.as_ref())
                );
                if keep.contains(aa.ann.ap.0.as_ref()) && !has_removed_ann && !subject_removed {
                    out.insert(ac.clone());
                }
            }
            // A named-subclass `SubClassOf` is the one shape `--preserve-structure`
            // keeps: the annotated axiom is removed and re-added bare. A GCI
            // (anonymous subclass), property chain, equivalence, disjointness, … has
            // no named subclass link to preserve, so with a removed annotation it is
            // dropped whole.
            Component::SubClassOf(sc) if has_removed_ann => {
                if matches!(sc.sub, ClassExpression::Class(_)) {
                    let mut ac2 = ac.clone();
                    ac2.ann.clear();
                    out.insert(ac2);
                }
            }
            // A sub/super-property axiom naming a removed property goes with it.
            Component::SubAnnotationPropertyOf(sp)
                if removed.contains(sp.sub.0.as_ref()) || removed.contains(sp.sup.0.as_ref()) => {}
            _ if has_removed_ann => {}
            _ => {
                out.insert(ac.clone());
            }
        }
    }
    crate::model::Model::from_parts(out, prefixes)
}

/// Whether one of the steps writes the artefact's own target file.
///
/// `query -c <query> $@` is the shape: a CONSTRUCT writes its OUTPUT and leaves
/// the threaded ontology untouched, so the rule's product IS the constructed
/// graph and the recipe has no `-o` at all. Serializing the model
/// afterwards would overwrite EFO's `components/gwas_template.owl` with the
/// template output it was built from, losing all 20,047 constructed triples and
/// with them every `gwas_trait` assertion in `components/gwas_import.owl`.
fn step_writes_target(steps: &[Step], target: &str) -> bool {
    let written = step_built_paths(steps);
    steps.iter().any(|s| match s {
        Step::Op(Op::Query { constructs, selects, .. })
        | Step::Partial { op: Op::Query { constructs, selects, .. }, .. } => constructs
            .iter()
            .chain(selects.iter())
            .any(|(_, out)| out == target),
        // `babelon convert --output-format json` writes the babelon PROFILE — a
        // table, not an ontology — so the model write that normally stands in for
        // a step's output would replace it with OBO Graphs JSON.
        Step::Op(Op::Babelon { output, format, .. })
        | Step::Partial { op: Op::Babelon { output, format, .. }, .. } => {
            format.as_deref() == Some("json") && output.as_deref() == Some(target)
        }
        // A plugin command that names the target as one of its OWN output files
        // has already written it, so the model write must not go over the top.
        //
        // UBERON's `subsets/%-view.owl subsets/%-tags.ofn:` is one multi-target
        // rule whose recipe writes the second file with
        // `--write-tags-to subsets/human-tags.ofn`. Planned as two artefacts, the
        // `-tags.ofn` one ran the recipe (correctly writing 13k lines of
        // `oboInOwl:inSubset` assertions) and then serialized the in-flight SUBSET
        // over it: 267,196 lines of full ontology — Declarations, SubClassOf,
        // DisjointClasses — where ROBOT writes tags alone. `uberon.owl` merges
        // both tags files, so the 10x bloat reached every subset built from it.
        Step::OwlmakeCli { args, .. } => args
            .windows(2)
            .any(|w| matches!(w[0].as_str(), "--write-tags-to" | "--bridge-file" | "-o" | "--output")
                && w[1] == target),
        // A fetch straight to `$@` IS the artefact — the bytes off the wire are
        // what the rule produces, and there is no model behind them to serialize.
        // MONDO's `tmp/mondo-lastbase.owl` is a bare
        // `mkdir -p tmp && wget <the last release> -O $@`, and the release-diff
        // reports are read out of it: written over with an empty ontology, the
        // diff sees no previous release and calls all 22,000 terms new.
        Step::File(crate::build::recipe::FileOp::Fetch { dst, .. }) => dst == target,
        // A `mv`/`cp` onto the target whose SOURCE an earlier step of this same
        // recipe wrote is a real move: the file it names exists because the recipe
        // put it there, and moving it is how the target gets its content.
        //
        // That is what separates it from output bookkeeping. The common shape,
        // `--output $@.tmp.owl && mv $@.tmp.owl $@`, has its `-o` elided at ingest
        // because the temp file and the target want the same serialization — no
        // step writes `$@.tmp.owl`, the model write puts the bytes straight on the
        // target, and the move is a no-op standing in for it. But where the two
        // want DIFFERENT serializations, ingest keeps the write: ECTO's
        // `merge -i $(SRC) reason -o $@.owl && mv $@.owl $@` builds
        // `tmp/ecto-quick.obo` out of a genuine RDF/XML round trip through
        // `tmp/ecto-quick.obo.owl`. Read as bookkeeping, that produced 27 MB of
        // correct RDF/XML and then replaced it with OBO inferred from the target's
        // extension.
        Step::File(
            crate::build::recipe::FileOp::Move { src, dst }
            | crate::build::recipe::FileOp::Copy { src, dst, .. },
        ) => {
            same_path(dst, target) && src.iter().any(|s| written.iter().any(|w| same_path(w, s)))
        }
        _ => false,
    })
}

/// The paths this recipe's steps write by building something, as against by
/// moving or copying a file that already exists. What a later `mv`/`cp` onto the
/// target would be moving, if the recipe made it.
fn step_built_paths(steps: &[Step]) -> Vec<String> {
    let mut out = Vec::new();
    for s in steps {
        match s.effective() {
            Step::Op(Op::RoundTrip { path, .. }) | Step::Partial { op: Op::RoundTrip { path, .. }, .. } => {
                out.push(path.clone());
            }
            Step::Op(Op::Query { constructs, selects, .. })
            | Step::Partial { op: Op::Query { constructs, selects, .. }, .. } => {
                out.extend(constructs.iter().chain(selects.iter()).map(|(_, o)| o.clone()));
            }
            Step::Op(Op::Babelon { output: Some(o), .. })
            | Step::Partial { op: Op::Babelon { output: Some(o), .. }, .. } => out.push(o.clone()),
            Step::OwlmakeCli { args, .. } => {
                for w in args.windows(2) {
                    if matches!(w[0].as_str(), "-o" | "--output" | "--write-tags-to" | "--bridge-file") {
                        out.push(w[1].clone());
                    }
                }
            }
            Step::File(crate::build::recipe::FileOp::Fetch { dst, .. }) => out.push(dst.clone()),
            // A shell line's `> FILE` redirect is the file that line builds.
            Step::Shell { .. } | Step::Fallback { .. } => {
                out.extend(crate::plan::gaps::recipe_outputs(std::slice::from_ref(s)));
            }
            _ => {}
        }
    }
    out
}

fn run_artefact(
    repo: &Repo,
    a: &crate::plan::ArtefactPlan,
    catalog: &BTreeMap<String, PathBuf>,
    work: &Path,
    out_dir: &Path,
    out: &Path,
) -> Result<()> {
    // A rule that threads no ontology runs its steps for their side effects alone.
    // Four shapes qualify, and none of them has a model to serialize at the end:
    //  * no input at all (`$<` empty) — MONDO's `tmp/mondo-lastbase.owl` is a bare
    //    `mkdir -p tmp && wget <the last release> -O $@`;
    //  * a non-ontology input — MONDO's `reports/mondo_release_diff_changed_terms.tsv`
    //    hands `reports/mondo_base_last_release-report.tsv` to a python script;
    //  * a non-ontology target — `query -f tsv --query … $@`, where the query step
    //    writes the file itself (the input, if it is OWL, is still loaded for it);
    //  * no step that threads a model, whatever the two file types say. MONDO's
    //    `skos.ttl: filtered.obo` is `../utils/mk-skos.pl $< > $@`: OBO in, Turtle
    //    out, both formats owlmake reads and writes, so the first three tests all
    //    pass; on the model path the perl script runs and the threaded model is then
    //    serialized straight over its 15 MB of output — 267 MB of Turtle the next
    //    step cannot parse. Built alone it looks correct, because `filtered.obo`
    //    does not exist yet and the missing input takes this branch.
    // A recipe that redirects its console output builds its target out of what it
    // prints, so the file exists from the moment the command starts — empty when
    // nothing is printed, which is the ordinary outcome of a check that passes.
    // owlmake's ops report on stderr, so the file records the same thing the
    // redirect does: that the recipe ran to the end.
    if let Some(dst) = a.stdout_file.as_deref() {
        let dst = repo.dir.join(dst);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&dst, b"")
            .with_context(|| format!("creating {} for `{}`", dst.display(), a.target))?;
    }
    let input_path =
        a.input.as_deref().and_then(|i| resolve_input(repo, Some(i), out_dir, work, &a.target).ok());
    // …but an input the plan can BUILD and that is still missing is a failed
    // prerequisite, not "this rule has no input". The cases above are all rules
    // whose input the plan never claims to make; discarding the error for those
    // too meant a target whose prerequisite had just failed carried on from an
    // EMPTY model and reported success.
    //
    // EFO under `-k`: `build/efo.owl` failed on an unresolvable catalog import,
    // and `build/efo.json` then wrote 202 bytes of `"nodes": [], "edges": []`
    // and `build/efo-base.owl` a 995-byte header, both logged `✓ done`. A
    // release that ships a well-formed empty product is worse than one that
    // stops, and `make` does not build a target whose prerequisite failed.
    if input_path.is_none() {
        if let Some(i) = a.input.as_deref() {
            let names_ontology = crate::io::Format::from_path(Path::new(i)).is_ok();
            // …unless the input IS the target, which is not a prerequisite at all
            // but a file this recipe's own earlier step writes (`wget … -O $@` /
            // `git show … > $@`, then `merge -i $@`). Nothing has built it yet
            // because nothing was supposed to.
            let self_input = i == a.target;
            let not_built_this_run =
                assumed_new(repo, i) || skip_missing_intermediate(repo, &a.target, i);
            // A mirror is as buildable as a planned target — `resolve_input`
            // fetches it — so reaching here with one means that fetch FAILED,
            // and carrying on from an empty model would bury the failure.
            let buildable = repo.target(i).is_some() || mirror_import_for(repo, i).is_some();
            if names_ontology && !self_input && !not_built_this_run && buildable {
                bail!(
                    "input `{i}` of `{}` was not built — its rule failed or was skipped, \
                     so there is nothing to build `{}` from",
                    a.target,
                    a.target
                );
            }
        }
    }
    let input_is_ontology =
        input_path.as_deref().is_some_and(|p| crate::io::Format::from_path(p).is_ok());
    let target_is_ontology = crate::io::Format::from_path(Path::new(&a.target)).is_ok();
    // Whether this rule has an ontology of its own to serialize at the end.
    //
    // A `cp`/`mv` of the target is output bookkeeping the final write stands in
    // for, so a rule made only of those still needs that write. A SIDE-EFFECT file
    // op is not: `touch $@` asks for an empty file and `wget … -O $@` writes the
    // download, and serializing a model over either replaces what the recipe
    // built with an ontology it never asked for.
    //
    // This target's recipe is its own run, so its blank nodes number from the
    // start: the ids an artefact carries are the same whether it is built alone
    // or after twenty others. Reset HERE, before either branch and past the
    // prerequisite walk — a prerequisite built on the way in is a run of its own
    // and leaves the counter wherever its last parse did.
    crate::io::reset_anon_counter();
    let threads_model = a.steps.iter().any(|s| match s {
        Step::File(op) => !op.is_side_effect(),
        Step::Op(_) | Step::Partial { .. } | Step::OwlmakeCli { .. } => true,
        _ => false,
    });
    // A *source* op (`babelon convert`) reads a non-OWL input — `$<` is a TSV — and
    // builds the model itself, so `!input_is_ontology` must NOT divert it into the
    // branch below. That branch writes the target only when it does not already
    // exist, and every `translations/%.babelon.owl` IS committed — so taking it
    // would keep the checked-in copy and publish a stale translation, leaving
    // HPO's `hp-fr.babelon.owl` on its committed `versionInfo` and pre-refresh
    // `source_value`s after a build that reports making it.
    // …but a `babelon convert --output-format json` step is NOT a model source:
    // it writes the babelon table and threads nothing, so it belongs in the
    // side-effect branch with `step_writes_target`.
    let starts_from_source = matches!(
        a.steps.first(),
        Some(Step::Op(Op::Babelon { format, .. })) if format.as_deref() != Some("json")
    );
    if !starts_from_source
        && (!input_is_ontology
            || !target_is_ontology
            || !threads_model
            || step_writes_target(&a.steps, &a.target))
    {
        // A rule whose whole recipe is file operations never looks at an ontology,
        // so do not parse one: `release: … ; cp $^ $(RELEASEDIR)` names
        // `build/efo.owl` as its first prerequisite and was reading all 349 MB of
        // it to copy four files.
        let needs_model = a
            .steps
            .iter()
            .any(|s| !matches!(s, Step::File(_) | Step::Inert(_)));
        let mut model = match &input_path {
            Some(p) if input_is_ontology && needs_model => crate::io::load(p)?,
            _ => crate::model::Model::new(),
        };
        // Same closure-label rule as the artefact path: a functional write
        // banners each entity with the label it carries ANYWHERE in the closure
        // (`normalize_src` re-serialises the edit file, whose pattern classes
        // are labelled only by the imported definitions module). The read spends
        // no blank-node ids — see the artefact path.
        if model.banner_labels.is_empty() && writes_functional_syntax(&a.steps) {
            let mark = crate::io::anon_counter();
            // The document's identity at write time: the last version IRI a step
            // of this pipeline sets, if any.
            let write_version = a.steps.iter().rev().find_map(|s| match s {
                Step::Op(Op::Annotate(sp))
                | Step::Partial { op: Op::Annotate(sp), .. } => sp.version_iri.clone(),
                _ => None,
            });
            model.banner_labels =
                closure_banner_labels(&model, &repo.dir, catalog, write_version.as_deref());
            crate::io::set_anon_counter(mark);
        }
        // Named the way `Op::Merge` will name it, so `merge -i $<` recognises the
        // file the model already holds however either side spelled the token.
        let threaded_from = input_path
            .as_ref()
            .filter(|_| input_is_ontology)
            .and(a.input.as_deref())
            .and_then(|t| resolve_repo_file(repo, t, work));
        // Whether this rule ends by serialising its own model over the target —
        // decided before the steps run, because it is what tells `run_steps` that a
        // closing `mv $@.tmp $@` is the output bookkeeping that write stands in for.
        //
        // Usually the steps wrote `$@` themselves. But a rule can reach here for
        // want of an INPUT while still building a model of its own: a target whose
        // prerequisite is commented out (`#$(SRC)`) gets its ontology from
        // `merge -i $(SRC)`, so `$<` is empty and the pipeline nonetheless has
        // something to serialize. Write it rather than reporting a rule that
        // produced nothing.
        //
        // Only when the STEPS built it. A rule whose recipe threads no model has
        // nothing of its own to write — the model here is just its input, and
        // saving that copies the input under the target's name. `check_rdfxml_%:
        // %` is `check-rdfxml $<`: a check that creates no file, and whose target
        // ends in `.owl`, so it would leave a 41 MB copy of `<id>-full.owl` at
        // `check_rdfxml_<id>-full.owl` in the repository every build.
        //
        // What decides it is whether a STEP already wrote the target, not whether
        // the target happens to be on disk: `translations/%.synonyms.owl` is
        // `template --template %.synonyms.tsv … convert -f owl --output $@`, a
        // model built from a TSV, and every one of those files is committed — so
        // a presence test skips the write and the release ships the previous
        // release's translation under this release's version IRI.
        // …and only when there is a model to write. A recipe made entirely of file
        // operations threads nothing: `needs_model` is false, so the model here is
        // the empty one, and serializing it over the target replaces whatever the
        // recipe produced with an empty ontology.
        //
        // …and never for a phony target. A phony names no file, so the model a
        // rule like `component-download-<x>.owl` threads belongs wherever its own
        // `--output` puts it, and writing it under the target's name as well leaves
        // a file the build configuration says does not exist.
        let writes_model_after = target_is_ontology
            && threads_model
            && needs_model
            && !repo.plan.is_phony(&a.target)
            && !step_writes_target(&a.steps, &a.target);
        let mut model = run_steps(
            repo,
            &a.steps,
            model,
            catalog,
            work,
            Some(&a.target),
            writes_model_after,
            threaded_from.as_deref(),
        )
        .with_context(|| format!("building {}", a.target))?;
        if writes_model_after {
            let dest = repo.dir.join(&a.target);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            match recipe_format(a) {
                Some(f) => crate::io::save_as(&mut model, &dest, f),
                None => crate::io::save(&mut model, &dest),
            }
            .with_context(|| format!("building {}", a.target))?;
            clear_staging(repo, a);
        }
        return surface_produced(repo, &a.target, &a.steps, work, out);
    }
    // A *source* op (e.g. `babelon convert`) reads a non-OWL input (`$<` is a
    // TSV) and produces the model itself — so skip the OWL load and start empty.
    let mut threaded_from: Option<PathBuf> = None;
    let mut model = if starts_from_source {
        crate::model::Model::new()
    } else {
        // Load the pipeline input (`$<`) once, then thread the model through every
        // step in memory — no temp-file re-parsing between steps.
        let input = resolve_input(repo, a.input.as_deref(), out_dir, work, &a.target)?;
        let mut m = crate::io::load(&input)?;
        // A functional document banners each entity with the label it carries
        // ANYWHERE in the closure — an edit file that only DECLARES a class still
        // names it by the label its imported pattern module asserts. The closure is
        // read for its labels alone and dropped; nothing here changes an axiom.
        //
        // Only for a rule that writes functional syntax: reading the closure costs
        // a load of every imported document, and no other serialization has these
        // banners to fill in.
        if m.banner_labels.is_empty() && writes_functional_syntax(&a.steps) {
            // …and it spends no blank-node ids either. The documents are read for
            // their labels and dropped, so the ids their anonymous individuals
            // would take are ids this artefact never writes; leaving the count
            // where the read pushed it numbers the artefact's OWN nodes from after
            // a whole closure that is not in it.
            let mark = crate::io::anon_counter();
            // The document's identity at write time: the last version IRI a step
            // of this pipeline sets, if any.
            let write_version = a.steps.iter().rev().find_map(|s| match s {
                Step::Op(Op::Annotate(sp))
                | Step::Partial { op: Op::Annotate(sp), .. } => sp.version_iri.clone(),
                _ => None,
            });
            m.banner_labels =
                closure_banner_labels(&m, &repo.dir, catalog, write_version.as_deref());
            crate::io::set_anon_counter(mark);
        }
        threaded_from = a.input.as_deref().and_then(|t| resolve_repo_file(repo, t, work)).or(Some(input));
        m
    };

    // Import closure as a read-only reasoning context. MONDO's `reasoned.owl` is
    // built from `filtered.owl`, whose `owl:imports` are kept as declarations
    // (`merge --collapse-import-closure false`): reason/relax/reduce run over the
    // root ontology with the imports loaded only for *entailment*, and the
    // imported axioms are merged in *verbatim* only at the later collapsing merge
    // (`mondo.owl`). So while the model still declares (uncollapsed) imports,
    // reason/reduce treat them as a closure rather than folding them into — and
    // reducing — the root. Loaded lazily and cached.
    let mut closure: Option<crate::model::Model> = None;
    let mut closure_loaded = false;

    // `reason -X` (exclude-external-entities) keeps no inferred axioms about
    // imported classes. With an uncollapsed closure, reason/relax/reduce run over
    // the union, and then — only if the recipe asked for `-X` — inferred axioms
    // whose subject is an imported class are stripped once reduce is done (doing
    // it before relax/reduce perturbs what those steps produce).
    let exclude_external = a.steps.iter().any(|s| {
        matches!(
            s,
            Step::Op(Op::Reason { exclude_external_entities: Some(true), .. })
        )
    });
    // The ROOT ontology's signature (every entity it declares OR references),
    // used as the "internal" set for `-X` (exclude-external-entities). An inferred
    // `CHEBI ⊑ BFO_…` is kept when CHEBI is in the root signature (a MONDO
    // definition references it) and dropped when CHEBI exists only in the
    // import closure. Using the full signature — not just axiom subjects — is what
    // distinguishes a referenced import class (kept) from a closure-only one
    // (dropped); `crate::sig::signature` excludes annotation properties, which is
    // fine since we only test class subjects.
    let internal_classes: std::collections::HashSet<String> = if exclude_external {
        let mut s = std::collections::HashSet::new();
        for ac in model.ont.iter() {
            s.extend(crate::sig::signature(&ac.component));
        }
        s
    } else {
        std::collections::HashSet::new()
    };

    let mut model_on_disk = false;
    // See `run_steps`: this loop always writes the model at the end, so a `mv`/`cp`
    // onto the target counts only when a SHELL step staged the file.
    let mut staged_by_shell = false;
    // …unless a shell step REDIRECTED into the target, in which case that file is
    // the artefact and the model write below would clobber it. MONDO's `mondo.obo`
    // ends `grep -v ^owl-axioms $@.tmp.obo > $@`, whose whole job is to drop a line
    // the model still holds — serialising the model over it would put
    // `owl-axioms:` straight back.
    let mut shell_wrote_target = false;
    for step in &a.steps {
        let op = match step {
            Step::Op(op) => op,
            // A new tool invocation: it shares nothing with the last but files, so
            // the model it works on is its OWN input, re-read from disk. The
            // artefact pipeline reaches the same boundaries the prerequisite
            // pipeline does — MONDO's `mondo-international.owl` builds
            // `../translations/mondo-jp.babelon.owl`, whose recipe has one — and
            // without this arm the step fell through to the catch-all and failed
            // the target outright.
            Step::Boundary { input } => {
                crate::io::reset_anon_counter();
                match input.as_deref() {
                    // A boundary whose input is an IRI is fetched, exactly as the
                    // prerequisite loop does. Both loops walk the same step list,
                    // so a case handled in one and missed in the other is a
                    // run-time failure on whichever repo has a recipe of that
                    // shape — `--input-iri` on an artefact's own pipeline.
                    Some(first) if first.starts_with("http://") || first.starts_with("https://") => {
                        model = crate::io::load_iri(first, None)?;
                        threaded_from = None;
                    }
                    Some(first) => {
                        let p = resolve_repo_file(repo, first, work).with_context(|| {
                            format!("invocation input `{first}` of `{}` does not exist", a.target)
                        })?;
                        model = crate::io::load(&p)?;
                        threaded_from = Some(p);
                    }
                    None => {
                        model = crate::model::Model::new();
                        threaded_from = None;
                    }
                }
                model_on_disk = false;
                staged_by_shell = false;
                continue;
            }
            // benign (echo/cat) or a file op — the in-memory pipeline writes the
            // output itself, so output-bookkeeping ops are no-ops here; only
            // genuine side-effect file ops (append/sort/print) actually run.
            Step::Inert(_) => continue,
            Step::File(op) => {
                if op.is_side_effect() {
                    run_file_op(repo, op)?;
                } else if let Some(dst) =
                    staged_target(repo, op, Some(&a.target)).filter(|_| staged_by_shell)
                {
                    // See `staged_target`: a mid-recipe `mv $@.tmp $@` over a file
                    // a shell step really produced is not bookkeeping — the rest of
                    // the recipe reads it.
                    run_file_op(repo, op)?;
                    if crate::io::Format::from_path(Path::new(&dst)).is_ok() {
                        model = crate::io::load(&repo.dir.join(&dst))
                            .with_context(|| format!("re-reading {dst} after a staged move"))?;
                        model_on_disk = true;
                    }
                }
                continue;
            }
            // A branch: decide on the (natively-evaluated) condition and run the
            // matching body in the same model pipeline.
            Step::Branch { condition, then_steps, else_steps } => {
                let body = if eval_condition(repo, condition) { then_steps } else { else_steps };
                model = run_steps(
                    repo,
                    body,
                    model,
                    catalog,
                    work,
                    Some(&a.target),
                    true,
                    threaded_from.as_deref(),
                )?;
                continue;
            }
            // `cmd || true` — handed to `run_steps`, which owns the one
            // implementation of tolerating a failure.
            Step::MayFail(_) => {
                model = run_steps(
                    repo,
                    std::slice::from_ref(step),
                    model,
                    catalog,
                    work,
                    Some(&a.target),
                    true,
                    threaded_from.as_deref(),
                )?;
                model_on_disk = false;
                continue;
            }
            // An out-of-pipeline step (perl/sed/jq/`report`/…) runs where it
            // sits, so the ops around it stay native.
            s if is_shell_step(s) => {
                if let Some(cmd) = step_command_text(s) {
                    let base = Path::new(&a.target)
                        .file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or(a.target.as_str());
                    if redirect_targets(&cmd).any(|d| d == a.target || d == base) {
                        shell_wrote_target = true;
                    }
                }
                model = run_shell_step_in_pipeline(
                    repo,
                    s,
                    model,
                    Some(&a.target),
                    work,
                    model_on_disk,
                    threaded_from.as_deref(),
                )?;
                model_on_disk = false;
                staged_by_shell = true;
                continue;
            }
            Step::UnsupportedSubcommand(name) => {
                bail!("recipe names the ontology subcommand `{name}`, which owlmake does not implement")
            }
            _ => bail!("internal: uncovered step reached executor: {}", step.label()),
        };
        // Use the import closure only when the model still carries uncollapsed
        // import declarations at a reason/reduce step.
        let use_closure = matches!(op, Op::Reason { .. } | Op::Reduce { .. }) && model_has_imports(&model);
        if use_closure && !closure_loaded {
            closure = load_closure(&model, &repo.dir, catalog)?;
            closure_loaded = true;
        }
        let cl = if use_closure { closure.as_ref() } else { None };
        model = apply_op(repo, op, model, catalog, work, cl, threaded_from.as_deref())?;
        dump_step(&a.target, &model);
        write_step_output(repo, op, &mut model, Some(&a.target))?;
        model_on_disk = false;
        staged_by_shell = false;
        if exclude_external && matches!(op, Op::Reduce { .. }) {
            model = strip_external_subject_axioms(model, &internal_classes);
        }
    }

    // Resolve the import closure BEFORE materialising declarations: a property the
    // closure already declares must NOT get a fresh Declaration here, and the
    // writer needs the same set to decide whether a referenced entity gets a stub.
    let write_owlrdf = a.target.ends_with(".owl");
    if write_owlrdf && model_has_imports(&model) {
        if !closure_loaded {
            closure = load_closure(&model, &repo.dir, catalog)?;
            closure_loaded = true;
        }
        if let Some(cl) = &closure {
            model.closure_ann_ns = annotation_property_namespaces(cl);
            model.closure_declared = closure_declared_entities(cl);
            // A functional document names each entity's section after its label,
            // and an entity the root only REFERENCES is labelled by the ontology
            // that declares it. `oba-edit.obo` gives `OBA:0000003` no `name:` at
            // all — the label is in the `patterns/definitions.owl` it imports — so
            // without the closure every such section falls back to the IRI.
            model.banner_labels = closure_labels(cl);
        }
        withdraw_materialised_declarations(&mut model);
    }
    // An OBO document comments every clause target that has a label — and a
    // target the root only references (a GO process in a `relationship:`, a
    // BFO class in an `is_a:`) is labelled by the ontology that declares it.
    // MONDO's `filtered.obo` keeps its four imports, so its 34,000 clause
    // comments come from the closure.
    if a.target.ends_with(".obo") && model_has_imports(&model) {
        if !closure_loaded {
            closure = load_closure(&model, &repo.dir, catalog)?;
            closure_loaded = true;
        }
        if let Some(cl) = &closure {
            model.banner_labels = closure_labels(cl);
        }
    }

    // Every referenced entity is declared: an annotation property a merged non-OBO
    // file introduced (skos:exactMatch …) must get a Declaration.
    declare_used_annotation_properties(&mut model);

    // horned-owl keeps set-valued operands (`ObjectIntersectionOf`,
    // `EquivalentClasses`, …) in a Vec, so two axioms that differ only in the
    // ORDER of a set operand — one axiom in OWL — can both survive as distinct
    // values here. Canonicalise the order and let
    // `SetOntology` collapse them — otherwise the equivalence, its blank node and
    // its `owl:Axiom` reification are each rendered twice.
    crate::io::normalize_set_operands(&mut model);

    // Write the final result in the artefact's format (from its extension).
    // Targets can name a subdirectory (e.g. `tmp/<id>-preprocess.owl`); ensure it
    // exists under the output directory.
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // Use the release RDF/XML writer (owlrdf.rs) for EVERY `.owl` target, release
    // products AND intermediates. An intermediate's prefix set propagates
    // downstream (filtered.owl → reasoned.owl → mondo.owl, carried through the OFN
    // cache's `#rdfxmlns`), so using the default writer for intermediates leaks a
    // different prefix choice (`dcterms` where the released files carry `terms`)
    // into the release artefacts. Non-`.owl` targets (`.obo`, `.json`, `.ofn`) are
    // unaffected.
    // That writer's namespace manager collects the entities that need a namespace
    // across the import closure as well as the root, so an ontology whose imports
    // are kept uncollapsed still declares its closure's annotation-property
    // prefixes. That is the whole reason MONDO's `filtered.owl` carries
    // `xmlns:doap` (from `merged_import.owl`) and `xmlns:protege` (from
    // `omo_import.owl`) without a single triple using either — and, since each step
    // re-reads the previous file's prefix map, why every downstream artefact
    // carries them too. Both that and `closure_declared` are recorded above, before
    // declarations are materialised.
    let explicit_fmt = recipe_format(a);
    // A shell step already produced the artefact by redirection — that file IS the
    // output, and re-serialising the model would undo whatever the command did.
    if shell_wrote_target && repo.dir.join(&a.target).exists() {
        return surface_produced(repo, &a.target, &a.steps, work, out);
    }
    // Same for a recipe with NO PIPELINE OP in it at all — only file operations
    // and shell commands. Those produce the target themselves and leave no model
    // to serialise, so writing one replaces whatever they wrote. A stamped
    // component's rule is `test -f $@ || touch $@`, and writing the (empty) pipeline
    // model here REPLACED `components/bridge.owl` and `components/ecto-xrefs.owl`
    // with an ontology carrying neither their IRI nor their version IRI — repo
    // content destroyed by a build that only meant to touch a file.
    // This holds whether or not the target already exists. A rule with no
    // pipeline op owns its own output, and where it deliberately writes NOTHING
    // there is no model to put there. A checking rule of the shape
    // `check_rdfxml_%: %`, whose recipe reads the PREREQUISITE and creates no
    // file, is always out of date and simply runs again; writing the threaded
    // model would leave a copy of the artefact it had just checked at every such
    // target.
    let no_model_op = !a.steps.is_empty()
        && !a.steps.iter().any(|s| matches!(s, Step::Op(_) | Step::Partial { .. }));
    if no_model_op {
        return surface_produced(repo, &a.target, &a.steps, work, out);
    }
    // A DECLARED phony target gets no file. This write never asked, and a `.PHONY`
    // target whose recipe threads a model was handed one anyway — materialised as
    // a real file named after something that was never meant to be one, which a
    // later rule then reads as though the build had produced it.
    //
    // The declared list ONLY, deliberately, and not the shape test that
    // `names_a_file` also applies. The two questions are not the same question,
    // and they are not symmetric in what a wrong answer costs:
    //
    //   * DEMANDING a file (the post-condition below) may be waived on a bare
    //     name, because a rule that legitimately writes nothing must not fail.
    //     Being lenient there costs a missed check.
    //   * SKIPPING a write may not. ECTO's `mre` is a bare name — no directory,
    //     no extension, absent from `.PHONY` — whose recipe is
    //     `filter -i $(SRC) -T tmp/mre_seed.txt -o $@`, so the terminal write IS
    //     how the target is produced. Judging it by shape here would skip that
    //     write and destroy the output rather than merely fail to check for it.
    if repo.plan.is_phony(&a.target) {
        return surface_produced(repo, &a.target, &a.steps, work, out);
    }
    // A side-effect-only rule writes the files its steps name and nothing at
    // the target path: no target file exists afterwards, the rule is simply
    // always out of date, and materialising the pipeline model here would
    // create an artefact the build never meant to produce.
    if a.side_effect_only {
        return Ok(());
    }
    let write_res = match explicit_fmt.or_else(|| crate::io::Format::from_path(out).ok()) {
        Some(f) => crate::io::save_as(&mut model, out, f),
        None => crate::io::save(&mut model, out),
    };
    write_res?;
    clear_staging(repo, a);
    // The cache below stands in for re-reading the file we just wrote, so for an
    // RDF/XML target it has to model what that read would return — see
    // `collapse_rdf_roundtrip`.
    if write_owlrdf {
        // The next step re-reads this file, so the model must carry the xmlns block
        // we just wrote — including prefixes that only the writer derives (the
        // import closure's `doap`/`protege`). Otherwise they exist on disk and
        // nowhere else, and every downstream artefact silently drops them.
        model.rdf_prefixes = crate::io::owlrdf::document_prefixes(&model);
        // An RDF/XML document has no notion of a `--add-prefixes` context: the file
        // we just wrote declares exactly `rdf_prefixes` and nothing else, so the
        // next step, which re-reads it, cannot see MONDO's `config/prefixes.jsonld`
        // set. Carrying them through the cache gives `mondo-international.obo` 26
        // `idspace:` lines its input never declared — and, because those prefixes
        // then shorten xrefs, ~80,000 differing `property_value:` lines with them.
        // (`tmp/mondo.owl.ofn` is a `.ofn` target, so it keeps its own prefixes —
        // which is right: a functional-syntax file declares them.)
        model.explicit_prefixes.clear();
        // …and the CURIE map itself becomes exactly what the file declares. The OBO
        // writer lists an `idspace:` for every prefix the document declared, so
        // leaving MONDO's `config/prefixes.jsonld` set in the map would put it back
        // in through the back door. CURIEs a later step names
        // (`--term MONDO:0700097`) still resolve: `select::expand` falls back to
        // the OBO convention.
        {
            let mut p = horned_owl::curie::PrefixMapping::default();
            for (name, ns) in &model.rdf_prefixes {
                let _ = p.add_prefix(name, ns);
            }
            model.prefixes = p;
        }
        collapse_rdf_roundtrip(&mut model);
    }
    // Cache a faithful OFN copy so a downstream artefact fed by this one reads it
    // without round-tripping through the (fragile) RDF/XML reader. The cache name
    // APPENDS `.ofn` to the full target name (`mondo.owl.ofn`, `mondo.obo.ofn`) —
    // `with_extension` would collapse `mondo.owl` and `mondo.obo` to the same
    // `mondo.ofn`, so building mondo.obo (whose obo round-trip drops the IAO
    // curation-status individuals) would clobber mondo.owl's cache and strip those
    // nodes from mondo.json.
    if let Some(name) = Path::new(&a.target).file_name() {
        // …unless the model carries blank-node identity the OFN cannot express:
        // a consumer replaying such a cache gets the axioms and loses the
        // sharing, splitting every shared node back into per-owner copies. The
        // consumer then reads the written RDF/XML, whose scan recovers it.
        if model.cross_shared.is_empty() && model.owl_shared_owners.is_empty() {
            let cache = work.join(format!("{}.ofn", name.to_string_lossy()));
            let _ = crate::io::save_as(&mut model, &cache, crate::io::Format::Functional);
        }
    }
    Ok(())
}

/// Drop every plain axiom that has an annotated twin, modelling what re-reading
/// the RDF/XML we just wrote would return.
///
/// In RDF an axiom and its annotated twin share ONE set of triples: the plain
/// triple carries the assertion and the annotation hangs off a separate
/// `owl:Axiom` reification. Reading that back yields the ANNOTATED axiom alone.
/// For a `SubClassOf(A B)` plus its `Annotation(is_inferred "true")` twin:
///
///   convert -i x.ofn -f obo               -> 2 `is_a:` clauses (both axioms)
///   convert -f owl, then convert -f obo   -> 1 clause (the annotated one)
///
/// A pipeline whose every hop is a real file read gets this collapse for free.
/// owlmake deliberately skips the RDF/XML reader between steps and passes an OFN
/// cache instead, so it must apply the rule explicitly — otherwise the plain twins
/// `span_gaps` legitimately adds to `filtered.obo` survive into `reasoned.owl`,
/// `mondo-base.owl` and `mondo.json`, which carry only the annotated axiom.
///
/// Two axioms annotated DIFFERENTLY are untouched: each keeps its own
/// reification, so both survive the round-trip.
/// Put back the literal datatypes an RDF/XML round trip erases.
///
/// Only `xsd:string` is affected: every other datatype is written out explicitly
/// and survives. Keyed on the axiom with its literals normalised away, so an
/// axiom that is otherwise unchanged gets its original literal forms back.
fn restore_literal_datatypes(back: &mut crate::model::Model, before: &crate::model::Model) {
    use horned_owl::model::{AnnotationValue, Component, Literal, MutableOntology};
    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
    // Subject+property+text of every annotation assertion that carried an explicit
    // `xsd:string` before the write.
    let mut typed: std::collections::HashSet<(String, String, String)> = Default::default();
    for ac in before.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let (horned_owl::model::AnnotationSubject::IRI(s), AnnotationValue::Literal(l)) =
                (&aa.subject, &aa.ann.av)
            {
                if matches!(l, Literal::Datatype { datatype_iri, .. } if datatype_iri.as_ref() == XSD_STRING)
                {
                    typed.insert((
                        s.as_ref().to_string(),
                        aa.ann.ap.0.as_ref().to_string(),
                        l.literal().clone(),
                    ));
                }
            }
        }
    }
    if typed.is_empty() {
        return;
    }
    let build = horned_owl::model::Build::new_rc();
    let mut fixed = Vec::new();
    for ac in back.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let (horned_owl::model::AnnotationSubject::IRI(s), AnnotationValue::Literal(l)) =
                (&aa.subject, &aa.ann.av)
            {
                if matches!(l, Literal::Simple { .. })
                    && typed.contains(&(
                        s.as_ref().to_string(),
                        aa.ann.ap.0.as_ref().to_string(),
                        l.literal().clone(),
                    ))
                {
                    let mut new_ac = ac.clone();
                    if let Component::AnnotationAssertion(a) = &mut new_ac.component {
                        a.ann.av = AnnotationValue::Literal(Literal::Datatype {
                            literal: l.literal().clone(),
                            datatype_iri: build.iri(XSD_STRING),
                        });
                    }
                    fixed.push((ac.clone(), new_ac));
                }
            }
        }
    }
    for (old, new) in fixed {
        back.ont.remove(&old);
        back.ont.insert(new);
    }
}

fn collapse_rdf_roundtrip(model: &mut crate::model::Model) {
    use horned_owl::model::{AnnotatedComponent, Component, MutableOntology, RcStr};
    use std::collections::HashSet;

    let annotated: HashSet<Component<RcStr>> = model
        .ont
        .iter()
        .filter(|ac| !ac.ann.is_empty())
        .map(|ac| ac.component.clone())
        .collect();
    if annotated.is_empty() {
        return;
    }
    let redundant: Vec<AnnotatedComponent<RcStr>> = model
        .ont
        .iter()
        .filter(|ac| ac.ann.is_empty() && annotated.contains(&ac.component))
        .cloned()
        .collect();
    for r in redundant {
        model.ont.remove(&r);
    }
}

/// Surface a target a rule wrote itself. Steps run in the ontology directory (so
/// their relative `$<`/`$@` paths resolve), which is not necessarily where the
/// build wants the artefact: copy it to `out`, and cache an OFN keyed by basename
/// so a downstream artefact's `resolve_input` picks it up without an RDF
/// round-trip. Used by rules that thread no model of their own.
///
/// A target that is not a filename at all is PHONY, and nothing is guaranteed
/// beyond its recipe having run. MONDO's `report-base-query-%` is the shape:
/// `report-base-query-obsoletioncandidates-withcomment` is a pattern target whose
/// recipe writes `reports/report-base-$*.tsv`, and the rule that consumes it
/// (`reports/mondo_obsoletioncandidates.tsv`) copies that file, not the target.
/// Nothing named after the target is ever created, so requiring one fails the
/// release build at artefact 23 of 35.
///
/// Two things say a target names no file, and both are needed. The plan's
/// `phony` list is authoritative for the names a repo declared, and covers the
/// ones that look exactly like paths — `component-download-<x>.owl`. Shape
/// covers what the list cannot: a pattern rule's expansions are never declared
/// literally, and there a name with no directory component and no extension
/// cannot be a file. Anything that neither rule excuses must still appear — a
/// rule that silently produced nothing is a real failure, and that is what this
/// check is for.
///
/// Unless the rule's own steps write nothing anywhere. A recipe carries no
/// post-condition, and a repo may override a rule precisely to turn a check off —
/// a recipe that reports its own skip on stdout and creates no file. Demanding
/// the file there would fail a check the repo deliberately declined, so the steps
/// decide; they come from the plan, so a plan-only build reaches the same
/// answer.
fn surface_produced(
    repo: &Repo,
    target: &str,
    steps: &[Step],
    work: &Path,
    out: &Path,
) -> Result<()> {
    let produced = repo.dir.join(target);
    if !produced.exists() {
        if !repo.plan.names_a_file(target) {
            return Ok(());
        }
        // A PHONY target names no file, whatever its name looks like, so there is
        // nothing to find here. CL's component refresh is four phony rules called
        // `component-download-<x>.owl`, each writing its download into `tmp/`.
        if repo.plan.phony.iter().any(|p| p == target) {
            return Ok(());
        }
        // A step that cannot name the target cannot have written it. There is no
        // post-condition on a recipe: a rule whose commands create nothing is
        // simply always out of date, so an absent target is not a failure. A rule
        // whose whole recipe is `echo "SKIP $@"`, and a checking rule that reads
        // its prerequisite and writes nowhere, are both of that shape.
        let name = Path::new(target).file_name().and_then(|s| s.to_str()).unwrap_or(target);
        let writes_nothing = !steps.is_empty()
            && steps.iter().all(|s| match s {
                Step::File(recipe::FileOp::Print { dst: None, .. }) => true,
                Step::Shell { command, .. } => !command.contains(name),
                _ => false,
            });
        if writes_nothing {
            return Ok(());
        }
        bail!("steps completed but did not produce {}", produced.display());
    }
    if produced != *out {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::copy(&produced, out)
            .with_context(|| format!("copying {} to {}", produced.display(), out.display()))?;
    }
    // The `.ofn` cache is an OPTIMISATION: it saves a later step an RDF/XML round
    // trip over a file this run already has as a model. It is not worth loading a
    // bulk triple dump to build one. MONDO mirrors `ncbi_gene.nt` (3.2 GB, ~33M
    // triples) and `hgnc_gene.nt`, and mapping the big one to axioms just to cache
    // it takes the process past 15 GB — for a file whose only consumer is a SPARQL
    // query run straight over the triples, which never builds a model. N-Triples is
    // exactly the shape such bulk mirrors come in, so it is the right thing to
    // except.
    let bulk_triples = matches!(
        crate::io::Format::from_path(Path::new(target)),
        Ok(crate::io::Format::NTriples)
    );
    if let Some(name) = Path::new(target).file_name() {
        if !bulk_triples {
            if let Ok(mut m) = crate::io::load(&produced) {
                // Functional syntax cannot express blank-node identity, so a
                // product whose RDF carries sharing evidence (cross-owner nodes,
                // shared reification targets) must NOT be replayed from the
                // cache: a consumer that loads the `.ofn` gets the axioms and
                // loses the identity, and every shared node it re-renders
                // splits back into per-owner copies. Loading the real RDF/XML
                // re-scans the evidence; the round trip is the price.
                if m.cross_shared.is_empty() && m.owl_shared_owners.is_empty() {
                    let cache = work.join(Path::new(name).with_extension("ofn"));
                    let _ = crate::io::save_as(&mut m, &cache, crate::io::Format::Functional);
                }
            }
        }
    }
    Ok(())
}

/// Build a target on demand from its plan rule when the file is missing,
/// recursively building any prerequisites that have their own rules first, then
/// running the rule's own steps (so `robot`/`jq`/`sssom`/`python3` recipe lines
/// all run). Returns whether the file exists afterwards. A no-op when the file is
/// already present or there is no rule for it; bounded by `depth`.
///
/// This is what lets owlmake produce *generated* prerequisites — e.g. uPheno's
/// SSSOM mapping components, built by `sssom`/`python3` rules — rather than
/// assuming every component ships pre-built.
fn ensure_built(repo: &Repo, rel: &str, depth: usize) -> Result<bool> {
    if repo.dir.join(rel).exists() {
        return Ok(true);
    }
    if depth == 0 {
        return Ok(false);
    }
    let Some(a) = repo.target(rel) else {
        return Ok(false); // the plan cannot build it
    };
    for pre in &a.needs {
        if !repo.dir.join(pre).exists() && repo.target(pre).is_some() {
            ensure_built(repo, pre, depth - 1)?;
        }
    }
    status!("make:   building prerequisite {rel}");
    let mut seen = std::collections::HashSet::new();
    run_target_recipe_inner(repo, rel, &mut seen)?;
    Ok(repo.dir.join(rel).exists())
}

/// Build `target` if it is missing, having first built whatever it needs — the
/// rebuild rule, driven entirely from the plan's recorded dependency edges.
///
/// The "if it is missing" is what keeps a cached build cheap: CL ships
/// `imports/merged_import.owl`, so the walk stops there and never descends into
/// the fourteen upstream mirrors that would otherwise be re-downloaded on every
/// run. A phony target (no file ever appears) runs each time.
fn ensure_prerequisite(
    repo: &Repo,
    target: &str,
    by_target: &std::collections::HashMap<&str, &crate::plan::ArtefactPlan>,
    done: &mut std::collections::HashSet<String>,
    catalog: &BTreeMap<String, PathBuf>,
    work: &Path,
    opts: &ExecOpts,
) -> Result<()> {
    if done.contains(target) {
        return Ok(());
    }
    // A target whose recipe already ran — or was already judged up to date — earlier
    // in THIS invocation is settled and is never revisited. HPO reaches `hp.obo`
    // three times in one build: `test_obo` writes it as a redirect side effect,
    // `fastobo: hp.obo` finds it up to date, and
    // `translations/hp-fr-preprocessed.babelon.tsv` names it as a prerequisite.
    // Unmemoised, that third visit re-runs the `$(ONT).obo` rule and replaces the
    // file the release ships — a different ontology id and 16 extra Typedef
    // stanzas. The artefact loop already skips its own targets on this memo;
    // prerequisites need the same rule.
    if memo_has(repo, target) {
        return Ok(());
    }
    done.insert(target.to_string());
    repo.built.borrow_mut().insert(target.to_string());
    // A pinned target's rules are not in play, so the file on disk is final and
    // the walk must not descend through it. It has to be
    // decided HERE, before the recursion below — `imports/merged_import.owl` is
    // committed and up to date, but its own prerequisite `mirror/merged.owl` is
    // not, so recursing first sends a plain release build off to rebuild the
    // merged mirror from 23 mirrors it never downloads.
    if repo.target_file(target).is_some() && pinned_by(repo, target).is_some() {
        return Ok(());
    }
    let Some(p) = by_target.get(target) else {
        // Not a generated file (a committed source, or a target with no recipe).
        return Ok(());
    };
    // Every prerequisite is brought up to date FIRST, and only then are mtimes
    // compared. Testing the target before its prerequisites exist reads a rule that
    // has not run as a rule that need not run.
    //
    // MP commits `src/translations/mp-ja.babelon.owl`, and the file it is built
    // from — `mp-ja-preprocessed.babelon.tsv` — is generated. On a fresh clone the
    // committed copy is present and the tsv is not, so comparing first finds nothing
    // newer and skips the rule; `mp-international.owl` then merges a translation
    // set eight months stale, with 893 obsolete terms labelled `"OBSOLETE"@ja` that
    // the release must not carry. Building the tsv first makes it newer, so the
    // translation is regenerated every run.
    for need in &p.needs {
        if assumed_new(repo, need) {
            continue;
        }
        if skip_missing_intermediate(repo, target, need) {
            continue;
        }
        ensure_prerequisite(repo, need, by_target, done, catalog, work, opts)?;
    }
    if repo.target_file(target).is_some()
        && !prereq_is_newer(repo, target, by_target, &opts.output_dir)
    {
        return Ok(());
    }
    build_prerequisite(repo, p, catalog, work, &opts.output_dir)
        .with_context(|| format!("building prerequisite {target}"))
}

/// Does any EXISTING prerequisite of `target` post-date it? The other rebuild
/// condition, alongside the target being absent.
///
/// Existence alone is not enough: MONDO COMMITS
/// `reports/mondo_base_current_release-report.tsv`, so on a fresh clone it is
/// always present; without this test it is never regenerated and the release-diff
/// reports are computed from a stale copy (`MONDO:0001315` still labelled
/// "neurocirculatory asthenia" where the `mondo-base.owl` built moments earlier
/// says "orthostatic intolerance").
///
/// A prerequisite that does not exist contributes nothing here — but only
/// because `ensure_prerequisite` has already built every prerequisite that has a
/// rule, so anything still absent is one nothing can build. Absence is not
/// a licence to skip the rule: read that way, MP's committed `mp-ja.babelon.owl`
/// never gets rebuilt.
fn prereq_is_newer(
    repo: &Repo,
    target: &str,
    by_target: &std::collections::HashMap<&str, &crate::plan::ArtefactPlan>,
    output_dir: &Path,
) -> bool {
    let Some(p) = by_target.get(target) else { return false };
    let Some(t) = repo
        .target_file(target)
        .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
    else {
        return false;
    };
    for need in p.needs.iter().map(String::as_str).chain(p.input.as_deref()) {
        if assumed_new(repo, need) {
            return true;
        }
        // A release artefact is written to the OUTPUT dir, not the ontology dir,
        // so look in both — `mondo-base.owl` lives at the repo root by the time
        // this report's turn comes.
        let candidates = [
            repo.dir.join(need),
            output_dir.join(need),
            Path::new(need).file_name().map(|n| output_dir.join(n)).unwrap_or_default(),
        ];
        for c in candidates {
            if let Ok(m) = std::fs::metadata(&c).and_then(|m| m.modified()) {
                if m > t {
                    return true;
                }
            }
        }
    }
    false
}

/// Build one planned prerequisite, from its recorded steps alone.
///
/// The step list is the plan's: shell lines were expanded (`$@`/`$<`/`$^`/`$*`
/// resolved, variables substituted) when the plan was built, so they run
/// verbatim. A prerequisite made entirely of native operations goes
/// through the same in-memory pipeline as an artefact; one containing a genuine
/// shell step (a `curl` plugin install, a perl/grep pass) has its recorded lines
/// executed directly.
fn build_prerequisite(
    repo: &Repo,
    p: &crate::plan::ArtefactPlan,
    catalog: &BTreeMap<String, PathBuf>,
    work: &Path,
    output_dir: &Path,
) -> Result<()> {
    // The same refusal as the recipe path: a prerequisite that neither exists
    // nor has a rule fails the target BEFORE its recipe runs, so a doomed
    // recipe's redirect and staging files are never created.
    for pre in &p.needs {
        if pre == "all_robot_plugins" || pre.ends_with(".jar") {
            continue;
        }
        if repo.dir.join(pre).exists()
            || repo.target(pre).is_some()
            || repo.plan.is_phony(pre)
            || recipe::is_served_image_asset(pre)
            || assumed_new(repo, pre)
            || is_native_pattern_product(repo, pre)
            || mirror_import_for(repo, pre).is_some()
        {
            continue;
        }
        bail!("no rule to make target `{pre}`, needed by `{}`", p.target);
    }
    status!("make:   building prerequisite {}", p.target);
    // A target's directory is not created for it, and recipes usually don't
    // create it either — they rely on some earlier rule having made it. That
    // earlier rule is often conditional on what is already on disk: a
    // `$(ROBOT_PLUGINS_DIRECTORY)/%.jar` pattern rule does the `mkdir -p`, but it
    // only fires for a jar already sitting under `/tools/robot-plugins`, which an
    // ordinary checkout does not have — so the explicit `flybase.jar`/`uberon.jar`
    // rules run `curl -o tmp/plugins/…` into a directory nothing created. Make the
    // target's parent first; it is always safe and removes a whole class of
    // failures where one rule assumed another had already made the directory.
    if let Some(parent) = Path::new(&p.target).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(repo.dir.join(parent)).ok();
        }
    }
    // A prerequisite is built by the same step pipeline as an artefact, which
    // already handles the shapes that thread no model: no resolvable input (CL's
    // `component-download-%.owl` rules pull the ontology straight from a URL), a
    // non-ontology input, or a non-ontology target (`tmp/simple_seed.txt` is a
    // `query -f csv … out.txt` whose query step writes the file itself).
    // A grouping target has no steps: its prerequisites were built by the caller
    // and there is nothing else to do. Handing it to the artefact pipeline would
    // ask that pipeline to write a file named after a target that names no file.
    if p.steps.is_empty() {
        return Ok(());
    }
    let out = repo.dir.join(&p.target);
    run_artefact(repo, p, catalog, work, output_dir, &out)
}

/// Leave a release artefact in `src/ontology` as well as in the output dir.
///
/// A release conventionally builds each artefact in `src/ontology` and copies it
/// to the repo root, so both copies exist for the rest of the run. Writing
/// straight to the output dir leaves the `src/ontology` one absent, and every
/// later recipe naming the artefact by its plain name resolves it relative to
/// `src/ontology` and finds nothing: MONDO's `test_nomerge` is
/// `owltools … mondo.owl …` and its `mondo_edges.tsv` is
/// `kgx transform … mondo.json`, and both fail on a file they just built.
///
/// A hard link, so the second copy costs an inode and not a gigabyte; a copy
/// where the filesystem refuses one. Best-effort: failing to mirror is not a
/// build failure, the artefact itself is written either way.
fn mirror_into_ontology_dir(repo: &Repo, target: &str, out: &Path) {
    let here = repo.dir.join(target);
    if here == *out || !out.is_file() {
        return;
    }
    if let Some(parent) = here.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&here);
    if std::fs::hard_link(out, &here).is_err() {
        let _ = std::fs::copy(out, &here);
    }
}

/// make's up-to-date test: `target` exists and its mtime is at least that of every
/// prerequisite that exists. A rule with NO prerequisites is up to date as soon as
/// its file exists — make remakes such a target only when the file is absent, and
/// that absence is the `metadata` miss below, so a bare `wget` rule still runs on a
/// tree that lacks the file. Requiring a prerequisite instead re-ran every one of
/// them: MONDO commits `reports/source-versions.tsv`, a release artefact that is
/// nothing but `wget` of a live upstream file, and re-fetching it replaced the
/// committed revision with whatever upstream published today — the ODK's own run
/// says `'reports/source-versions.tsv' is up to date`.
fn is_up_to_date(
    target: &Path,
    needs: &[String],
    order_only: &[String],
    input: Option<&str>,
    dir: &Path,
) -> bool {
    let Ok(t) = std::fs::metadata(target).and_then(|m| m.modified()) else {
        return false;
    };
    for need in needs.iter().map(String::as_str).chain(input) {
        // An order-only prerequisite decides WHEN a rule may run, never whether it
        // must: it is built first if it is missing, and its age says nothing about
        // the target's. `tmp/stamp-component-<x>.owl: | $(TMPDIR)` is the shape,
        // and `tmp/` is touched by every file written into it — so comparing it
        // would leave the stamp forever older than its own directory.
        if order_only.iter().any(|o| o == need) {
            continue;
        }
        let p = dir.join(need);
        let Ok(m) = std::fs::metadata(&p).and_then(|m| m.modified()) else {
            continue; // a prerequisite that does not exist yet will be built
        };
        if m > t {
            return false;
        }
    }
    true
}

/// Whether the model carries (uncollapsed) `owl:imports` declarations.
/// Declare every annotation property used (in an assertion, axiom/ontology
/// annotation, or sub-property axiom) but not yet declared — a released ontology
/// carries a Declaration for every signature entity. owlmake's OBO reader already
/// does this for OBO inputs, but properties introduced by a merged non-OBO file
/// (MONDO's `skos.ttl`: skos:exactMatch/closeMatch/…) would otherwise stay
/// undeclared.
fn declare_used_annotation_properties(model: &mut crate::model::Model) {
    use horned_owl::model::{Component, DeclareAnnotationProperty, MutableOntology};
    // Well-known annotation properties that owlmake's RDF/Turtle reader otherwise
    // types as object properties when they appear in a plain `s p o` triple with an
    // IRI object (MONDO's skos.ttl uses skos:exactMatch/closeMatch/…). They are
    // annotation properties and need an AnnotationProperty declaration, without
    // which the assertion triples do not round-trip.
    const KNOWN_ANN: &[&str] = &[
        "http://www.w3.org/2004/02/skos/core#exactMatch",
        "http://www.w3.org/2004/02/skos/core#closeMatch",
        "http://www.w3.org/2004/02/skos/core#broadMatch",
        "http://www.w3.org/2004/02/skos/core#narrowMatch",
        "http://www.w3.org/2004/02/skos/core#relatedMatch",
    ];
    let mut used: std::collections::BTreeSet<String> = Default::default();
    let mut declared: std::collections::BTreeSet<String> = Default::default();
    for ac in model.ont.iter() {
        for a in ac.ann.iter() {
            used.insert(a.ap.0.to_string());
        }
        match &ac.component {
            Component::AnnotationAssertion(ax) => {
                used.insert(ax.ann.ap.0.to_string());
            }
            Component::SubAnnotationPropertyOf(s) => {
                used.insert(s.sub.0.to_string());
                used.insert(s.sup.0.to_string());
            }
            Component::DeclareAnnotationProperty(d) => {
                declared.insert(d.0 .0.to_string());
            }
            Component::ObjectPropertyAssertion(opa) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &opa.ope {
                    let iri = p.0.to_string();
                    if KNOWN_ANN.contains(&iri.as_str()) {
                        used.insert(iri);
                    }
                }
            }
            _ => {}
        }
    }
    // The OWL 2 built-in annotation properties are part of the reserved
    // vocabulary, so no declaration is emitted for them even when they are used
    // (otherwise ECTO gains spurious rdfs:seeAlso / rdfs:isDefinedBy /
    // owl:deprecated declarations that belong in no release).
    const BUILTIN_ANN: &[&str] = &[
        "http://www.w3.org/2000/01/rdf-schema#label",
        "http://www.w3.org/2000/01/rdf-schema#comment",
        "http://www.w3.org/2000/01/rdf-schema#seeAlso",
        "http://www.w3.org/2000/01/rdf-schema#isDefinedBy",
        "http://www.w3.org/2002/07/owl#deprecated",
        "http://www.w3.org/2002/07/owl#versionInfo",
        "http://www.w3.org/2002/07/owl#priorVersion",
        "http://www.w3.org/2002/07/owl#backwardCompatibleWith",
        "http://www.w3.org/2002/07/owl#incompatibleWith",
    ];
    let b: horned_owl::model::Build<horned_owl::model::RcStr> = horned_owl::model::Build::new();
    for ap in used.difference(&declared) {
        if BUILTIN_ANN.contains(&ap.as_str()) {
            continue;
        }
        // Nor when the IMPORT CLOSURE already declares it. An ontology is read
        // together with its imports, so an `IAO_0000231` declared in
        // `omo_import.owl` needs no declaration in `filtered.owl`; adding one puts
        // a bare `<owl:AnnotationProperty rdf:about="…"/>` stub in every
        // intermediate.
        if model.closure_declared.contains(&format!("ap\u{0}{ap}")) {
            continue;
        }
        model.ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
            b.annotation_property(ap.as_str()),
        )));
    }
}

fn model_has_imports(model: &crate::model::Model) -> bool {
    use horned_owl::model::Component;
    model.ont.iter().any(|ac| matches!(ac.component, Component::Import(_)))
}

/// Load the model's import closure (resolved via the catalog) into one model,
/// to be used as a read-only reasoning context. None when no imports are declared.
/// The distinct namespaces of every annotation property in `model`'s signature.
///
/// Of the imported entity kinds, ONLY annotation properties contribute an
/// `xmlns` declaration to an RDF/XML output — an
/// imported ObjectProperty, DataProperty, Datatype, Class or NamedIndividual in
/// its own namespace contributes nothing. That asymmetry comes from RDF/XML
/// itself: an annotation property becomes an XML *element name* and so needs a
/// prefix, while every other entity appears only inside an `rdf:about` /
/// `rdf:resource` attribute as a full IRI.
pub(crate) fn annotation_property_namespaces(model: &crate::model::Model) -> Vec<String> {
    let mut ns: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for ac in model.ont.iter() {
        for iri in crate::sig::annotation_properties(&ac.component) {
            // The same split the writer abbreviates element names with — see
            // `owlrdf::ncname_split`.
            ns.insert(crate::io::owlrdf::ncname_split(&iri).0.to_string());
        }
    }
    ns.into_iter().collect()
}

/// Every entity in `model`'s SIGNATURE, keyed `kind\0IRI` — the set the RDF/XML
/// writer consults before materialising a bare declaration stub for a
/// referenced-but-undeclared entity (see `Model::closure_declared`).
///
/// The signature, not the declarations: an entity that appears anywhere in a
/// transitively imported ontology's signature is left for that ontology to
/// declare, whether or not it declares it. MONDO relies on the difference:
/// `omo_import.owl` declares `IAO_0000231` and friends, but `RO_0002175`,
/// `RO_0004001`, `RO_0004004` and `foaf:homepage` are only USED in the closure —
/// and none of them may get a stub. Keying off declarations alone puts those stubs
/// in `filtered.owl`/`reasoned.owl`, and from there into `tmp/simple_seed.txt`
/// (whose query asks for `?cls a owl:AnnotationProperty`), which keeps axioms
/// `filter` must drop from `mondo-simple.owl`.
/// `entity IRI → rdfs:label` across a resolved import closure, for the banner
/// comment a functional document heads each entity's section with.
///
/// The FIRST label an entity carries wins, which is how a subject with several
/// is settled everywhere else here.
fn closure_labels(
    model: &crate::model::Model,
) -> std::collections::HashMap<String, String> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Component, Literal};
    const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
    let mut labels = std::collections::HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() != RDFS_LABEL {
                continue;
            }
            if let (AnnotationSubject::IRI(subj), AnnotationValue::Literal(lit)) =
                (&aa.subject, &aa.ann.av)
            {
                let text = match lit {
                    Literal::Simple { literal }
                    | Literal::Language { literal, .. }
                    | Literal::Datatype { literal, .. } => literal.clone(),
                };
                labels.entry(subj.as_ref().to_string()).or_insert(text);
            }
        }
    }
    labels
}

pub(crate) fn closure_declared_entities(
    model: &crate::model::Model,
) -> std::collections::HashSet<String> {
    let e = crate::cmd::select::signature_entities(model);
    let mut out = std::collections::HashSet::new();
    for (kind, set) in [
        ("class", &e.classes),
        ("op", &e.object_properties),
        ("dp", &e.data_properties),
        ("ap", &e.annotation_properties),
        ("ni", &e.individuals),
        ("dt", &e.datatypes),
    ] {
        for iri in set {
            out.insert(format!("{kind}\u{0}{iri}"));
        }
    }
    out
}

/// Withdraw the declarations owlmake's OBO reader SYNTHESISED for entities the
/// import closure already has (see `Model::materialised_declarations`).
///
/// An OBO file carries no declaration for a property that appears only as a
/// `property_value:` predicate; one is materialised when the document is written,
/// and must be withheld when any imported ontology has the entity in its
/// signature. owlmake materialises at read time instead, so it has to withdraw
/// those again once the closure is known. On MONDO that is
/// 11 annotation properties, 3 object properties and 4 classes; retaining their
/// stubs puts `IAO_0000231` & co. in `tmp/simple_seed.txt` (its query asks for
/// `?cls a owl:AnnotationProperty`) and keeps axioms `filter` must drop.
fn withdraw_materialised_declarations(model: &mut crate::model::Model) {
    use horned_owl::model::{Component, MutableOntology};
    if model.materialised_declarations.is_empty() || model.closure_declared.is_empty() {
        return;
    }
    // Which classes are in question is settled at read time: a class named as the
    // FILLER of a `relationship:` is declared by the document outright and never
    // reaches the materialised set, so it keeps its stub whatever the closure holds,
    // and an OBO-sourced ontology still stubs all 8,087 `identifiers.org/hgnc/*`
    // classes. A class named only as a PLAIN operand — of `is_a:`,
    // `disjoint_from:`, a bare `intersection_of:` or a `union_of:` — is left to the
    // signature, so an imported ontology that types it suppresses the stub.
    // Properties work the same way: an annotation property named by a
    // `property_value:` predicate, and an object property used in a `relationship:`
    // with no `[Typedef]` frame, get no declaration from the OBO translation.
    //
    // A BUILT-IN is never withdrawn. The property behind every OBO *tag* is declared,
    // which is where `rdfs:label`, `rdfs:comment` and `owl:deprecated` come from;
    // those are genuine, and the writer's signature path skips built-ins, so
    // withdrawing them loses the section entirely.
    let builtin = |iri: &str| {
        iri.starts_with("http://www.w3.org/2001/XMLSchema#")
            || iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
            || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
            || iri.starts_with("http://www.w3.org/2002/07/owl#")
    };
    let doomed: Vec<_> = model
        .ont
        .iter()
        .filter(|ac| {
            let (key, iri) = match &ac.component {
                Component::DeclareClass(d) => {
                    (format!("class\u{0}{}", d.0 .0.as_ref()), d.0 .0.as_ref())
                }
                Component::DeclareObjectProperty(d) => {
                    (format!("op\u{0}{}", d.0 .0.as_ref()), d.0 .0.as_ref())
                }
                Component::DeclareAnnotationProperty(d) => {
                    (format!("ap\u{0}{}", d.0 .0.as_ref()), d.0 .0.as_ref())
                }
                _ => return false,
            };
            !builtin(iri)
                && model.materialised_declarations.contains(&key)
                && model.closure_declared.contains(&key)
        })
        .cloned()
        .collect();
    for ac in doomed {
        model.ont.remove(&ac);
    }
}

pub(crate) fn load_closure(
    model: &crate::model::Model,
    dir: &Path,
    catalog: &BTreeMap<String, PathBuf>,
) -> Result<Option<crate::model::Model>> {
    // The build reaches the closure here rather than through
    // `resolve_import_closure`, so it reports itself here too — otherwise
    // `OM_IMPORT_DEBUG` is silent on this path while the closure IS loaded, and
    // that silence reads as proof it is not.
    let debug = std::env::var("OM_IMPORT_DEBUG").is_ok();
    if !model_has_imports(model) {
        if debug {
            eprintln!("[import] load_closure: model declares no imports, no closure loaded");
        }
        return Ok(None);
    }
    if debug {
        eprintln!("[import] load_closure: building the closure for entity typing");
    }
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut closure = empty_model();
    for f in import_closure_of_model(model, dir, catalog, &mut seen)? {
        merge_file_into(&mut closure, &f)?;
    }
    Ok(Some(closure))
}

/// `root` ∪ `closure` axioms (skipping the closure's ontology identity and its
/// own import declarations). Used to give reason/reduce the full entailment set.
pub(crate) fn union_with_closure(root: &crate::model::Model, closure: &crate::model::Model) -> crate::model::Model {
    use horned_owl::model::{Component, MutableOntology};
    let mut union = empty_model();
    union.prefixes = root.prefixes.clone();
    for ac in root.ont.iter() {
        union.ont.insert(ac.clone());
    }
    for ac in closure.ont.iter() {
        if matches!(
            ac.component,
            Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_)
                | Component::Import(_)
        ) {
            continue;
        }
        union.ont.insert(ac.clone());
    }
    union
}

/// Reason `root` with `closure` as a read-only entailment context. The inferred
/// axioms are asserted into `root` only — the imported axioms are never folded in
/// (they re-enter verbatim at the later collapsing merge). This is what `reason`
/// means over an ontology whose `owl:imports` are loaded but not merged.
fn reason_with_closure(
    root: crate::model::Model,
    closure: &crate::model::Model,
    reasoner: &str,
    opts: &cmd::reason::ReasonOptions,
) -> Result<crate::model::Model> {
    use horned_owl::model::{Component, MutableOntology};
    // Reason over root + the import closure, then assert the inferred axioms
    // into the root only (the imported axioms re-enter verbatim at a later
    // collapsing merge). NB: `-X` does NOT drop inferred axioms about imported
    // classes at this point — MONDO's `reasoned.owl` keeps inferred
    // `CHEBI ⊑ BFO_…` and the like.
    //
    // What counts as "inferred" follows `--exclude-duplicate-axioms`: with it,
    // an axiom the union already asserts is a duplicate and stays out; without
    // it, every generated inference lands in the root even when the closure
    // asserts the same axiom — MONDO's `mondo-tags-reasoned.owl` carries
    // 18,000 `hgnc ⊑ SO_0000704` edges re-asserted from its uncollapsed
    // imports exactly that way.
    let union = union_with_closure(&root, closure);
    let mut out = root;
    if opts.exclude_duplicate_axioms {
        let before: std::collections::HashSet<_> = union.ont.iter().cloned().collect();
        let reasoned = cmd::reason::reason_with(union, reasoner, opts)?;
        for ac in reasoned.ont.iter() {
            if !before.contains(ac) {
                out.ont.insert(ac.clone());
            }
        }
    } else {
        let mut iopts = opts.clone();
        iopts.create_new_ontology = true;
        iopts.create_new_ontology_with_annotations = false;
        let inferred = cmd::reason::reason_with(union, reasoner, &iopts)?;
        for ac in inferred.ont.iter() {
            if matches!(
                ac.component,
                Component::SubClassOf(_)
                    | Component::EquivalentClasses(_)
                    | Component::ClassAssertion(_)
            ) {
                out.ont.insert(ac.clone());
            }
        }
        // The redundant-subclass sweep runs against the merged result, and the
        // inferred model carries exactly the direct pairs it needs: an
        // un-annotated asserted `C ⊑ X` that is not a proper direct super goes,
        // the same rule the non-closure path applies to its own merge.
        if opts.remove_redundant_subclass_axioms {
            use horned_owl::model::ClassExpression as CE;
            let mut direct_set: std::collections::HashSet<(String, String)> = Default::default();
            for ac in inferred.ont.iter() {
                if let Component::SubClassOf(sc) = &ac.component {
                    if let (CE::Class(c), CE::Class(x)) = (&sc.sub, &sc.sup) {
                        direct_set
                            .insert((c.0.as_ref().to_string(), x.0.as_ref().to_string()));
                    }
                }
            }
            const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
            const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
            let doomed: Vec<_> = out
                .ont
                .iter()
                .filter(|ac| {
                    if !ac.ann.is_empty() {
                        return false;
                    }
                    match &ac.component {
                        Component::SubClassOf(sc) => match (&sc.sub, &sc.sup) {
                            (CE::Class(c), CE::Class(x)) => {
                                let (c, x) = (c.0.as_ref(), x.0.as_ref());
                                let proper = direct_set
                                    .contains(&(c.to_string(), x.to_string()))
                                    && !direct_set.contains(&(x.to_string(), c.to_string()));
                                x != OWL_THING && x != OWL_NOTHING && !proper
                            }
                            _ => false,
                        },
                        _ => false,
                    }
                })
                .cloned()
                .collect();
            for ac in doomed {
                out.ont.remove(&ac);
            }
        }
    }
    Ok(out)
}

/// Reduce `root` using `closure` as a read-only entailment context, removing only
/// redundant *root* axioms. The imported axioms are kept verbatim: a reduce over
/// an import closure never reduces the closure.
fn reduce_with_closure(
    root: crate::model::Model,
    closure: &crate::model::Model,
    include_subproperties: bool,
) -> crate::model::Model {
    use horned_owl::model::MutableOntology;
    let union = union_with_closure(&root, closure);
    let root_set: std::collections::HashSet<_> = root.ont.iter().cloned().collect();
    let reduced = cmd::reduce::reduce_with_opts(&union, false, false, include_subproperties);
    let mut out = empty_model();
    out.prefixes = root.prefixes.clone();
    // The root's document state — blank-node sharing recorded by relax above
    // all — describes axioms that survive into `out`; rebuilding from an empty
    // model without it makes every relax-shared node render as two copies.
    out.carry_meta_from(&root);
    for ac in reduced.ont.iter() {
        if root_set.contains(ac) {
            out.ont.insert(ac.clone());
        }
    }
    out
}

/// Remove inferred subsumption axioms ALL of whose entities are external to the
/// root ontology — `reason -X` (exclude-external-entities). An axiom is kept
/// if it mentions any `internal` entity (the root signature); only fully-external
/// inferences (e.g. `CHEBI ⊑ BFO_…` where neither is referenced by MONDO) are
/// dropped, while a mixed `HP_… ⊑ MONDO_…` (internal superclass) is kept. The root
/// (filtered.owl) asserts no subsumptions about purely-imported classes, so these
/// are exactly the over-asserted inferences; the imported axioms re-enter verbatim
/// at the later collapsing merge.
fn strip_external_subject_axioms(
    model: crate::model::Model,
    internal: &std::collections::HashSet<String>,
) -> crate::model::Model {
    use horned_owl::model::{Component, MutableOntology};
    let fully_external = |c: &Component<horned_owl::model::RcStr>| -> bool {
        if !matches!(c, Component::SubClassOf(_) | Component::EquivalentClasses(_)) {
            return false;
        }
        let sig = crate::sig::signature(c);
        !sig.is_empty() && !sig.iter().any(|e| internal.contains(e))
    };
    let mut out = empty_model();
    out.prefixes = model.prefixes.clone();
    out.carry_meta_from(&model);
    for ac in model.ont.iter() {
        if !fully_external(&ac.component) {
            out.ont.insert(ac.clone());
        }
    }
    out
}

/// Apply one mapped operation to the in-memory model. `closure`, when present, is
/// the import closure used as a read-only reasoning context (see `run_artefact`).
/// The destination of a `cp`/`mv` that lands on the artefact target from sources
/// a shell step actually produced — i.e. one the model pipeline must honour rather
/// than leave to its own final write. `None` for every other file op, including a
/// move whose source is not on disk (a closing `mv $@.tmp $@` the pipeline never
/// staged).
/// Whether a `cp`/`mv` writes the rule's own target — i.e. whether it is output
/// bookkeeping [`staged_target`] may stand in for, rather than a copy the recipe
/// wants performed for its own sake.
fn names_target(op: &recipe::FileOp, target: Option<&str>) -> bool {
    use recipe::FileOp;
    let dst = match op {
        FileOp::Copy { dst, .. } | FileOp::Move { dst, .. } => dst,
        // Anything else reaching the non-side-effect arm is not a file write at
        // all; leave it to the existing path.
        _ => return true,
    };
    target.is_some_and(|t| Path::new(t).file_name() == Path::new(dst).file_name())
}

/// The format the recipe states for the file it is building, which WINS over the
/// target's extension exactly as it does for the `convert` command itself. A rule
/// may write functional syntax to a `.owl` name — `convert --input $< --format ofn
/// --output $@` — and writing RDF/XML there would make the next step fail to
/// re-read it.
///
/// Only a `convert` producing THIS file counts. A recipe can build two outputs at
/// once — one rule writing the artefact with `annotate … -o $@` and a second file
/// with `convert -f ofn -o tmp/<id>.owl.ofn` — and taking the last explicit format
/// would write the artefact in functional syntax.
///
/// "The file being built" includes the STAGING file a closing `mv`/`cp` renames
/// onto the target: a rule ending `convert -f ofn --output $@.tmp.owl && mv
/// $@.tmp.owl $@` states functional syntax for its target even though the convert
/// never names it. Matching file names alone would write every such artefact as
/// RDF/XML.
fn recipe_format(a: &crate::plan::ArtefactPlan) -> Option<crate::io::Format> {
    let name_of = |p: &str| Path::new(p).file_name().map(|s| s.to_os_string());
    let mut built: Vec<Option<std::ffi::OsString>> = vec![name_of(&a.target)];
    for s in &a.steps {
        let Step::File(recipe::FileOp::Copy { src, dst, .. } | recipe::FileOp::Move { src, dst }) =
            s
        else {
            continue;
        };
        if name_of(dst) == built[0] {
            built.extend(src.iter().map(|p| name_of(p)));
        }
    }
    a.steps.iter().rev().find_map(|s| match s {
        Step::Op(Op::Convert { format: Some(f), output, .. })
            if output.is_none()
                || built.contains(&output.as_deref().and_then(|o| name_of(o))) =>
        {
            crate::io::Format::from_name(f).ok()
        }
        _ => None,
    })
}

/// Delete the staging file a closing `mv $@.tmp $@` would have consumed.
///
/// The model write puts the content at the destination, but a `mv` also leaves
/// NOTHING at the source, and the staging file is still there from the step's own
/// `-o`. Removing it is what the recipe asks for.
fn clear_staging(repo: &Repo, a: &crate::plan::ArtefactPlan) {
    for step in &a.steps {
        let Step::File(recipe::FileOp::Move { src, dst }) = step else { continue };
        if Path::new(&a.target).file_name() != Path::new(dst).file_name() {
            continue;
        }
        for s in src {
            let _ = std::fs::remove_file(repo.dir.join(s));
        }
    }
}

fn staged_target(
    repo: &Repo,
    op: &recipe::FileOp,
    target: Option<&str>,
) -> Option<String> {
    use recipe::FileOp;
    let (src, dst) = match op {
        FileOp::Copy { src, dst, .. } | FileOp::Move { src, dst } => (src, dst),
        _ => return None,
    };
    let t = target?;
    if Path::new(t).file_name() != Path::new(dst).file_name() {
        return None;
    }
    // Every source must be there to move. The destination does NOT have to be an
    // ontology: MONDO's `reports/mondo_obsoletioncandidates.tsv` rule is
    // `cp reports/report-base-…tsv $@` followed by three `sed -i $@`, so gating on
    // a readable model would skip the `cp` and leave every `sed` failing on a file
    // that was never created. The caller re-reads the model only when it can.
    // A source owlmake serves from its own bytes counts as present. It exists at
    // the reference image's path rather than this machine's, and the copy
    // materialises it when it runs — so asking the filesystem alone skips the
    // step that would have created the file.
    let available =
        |s: &String| repo.dir.join(s).exists() || recipe::is_served_image_asset(s);
    if src.is_empty() || !src.iter().all(available) {
        return None;
    }
    Some(dst.clone())
}

thread_local! {
    /// Every target the plan builds as an artefact of its own. A step's `-o` that
    /// names one of these must NOT be written here: it is a CO-TARGET of a
    /// multi-output rule (MONDO's `$(ONT).owl tmp/mondo.owl.ofn: reasoned.owl`),
    /// and writing it early makes it newer than its prerequisites, so the artefact
    /// that would have built it properly is skipped as up to date. That costs
    /// `tmp/mondo.owl.ofn` its `#idspaces`/`#explicit-prefixes` cache markers and
    /// `mondo.obo` 29 of its 35 `idspace:` lines.
    static PLANNED_TARGETS: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Record the plan's own artefact targets for [`write_step_output`].
fn set_planned_targets(plan: &Plan) {
    let names: std::collections::HashSet<String> = plan
        .artefacts
        .iter()
        .map(|a| a.target.clone())
        .chain(plan.prerequisites.iter().map(|p| p.target.clone()))
        .collect();
    PLANNED_TARGETS.with(|t| *t.borrow_mut() = names);
}

fn is_planned_target(path: &str) -> bool {
    PLANNED_TARGETS.with(|t| {
        let t = t.borrow();
        t.contains(path)
            || Path::new(path)
                .file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|base| t.iter().any(|p| p == base || p.ends_with(&format!("/{base}"))))
    })
}

/// Write a step's own `-o/--output` when it names a file OTHER than the artefact.
///
/// A rule can produce more than one file. MONDO's `mondo.obo` recipe is
/// `annotate … convert -f obo -o $@.tmp.obo && grep -v ^owl-axioms $@.tmp.obo > $@`
/// — the `convert` writes a temp file and the shell filters it into the target —
/// and its `$(ONT).owl tmp/mondo.owl.ofn` rule writes two artefacts outright. So a
/// step's recorded `output` names a file to WRITE, not merely a hint about the
/// artefact's FORMAT: nothing else creates it, and without this write the `grep`
/// exits 2 on a file that was never written, failing the release build at
/// `mondo.obo` and again at `subsets/mondo-rare.obo`.
///
/// The artefact's own write still happens at the end of the pipeline, so a step
/// naming the target itself is left to it — writing here as well would serialize
/// the model twice, and the pipeline's final write is the one that carries the
/// artefact's format and its `write_owlrdf` handling.
fn write_step_output(
    repo: &Repo,
    op: &Op,
    model: &mut crate::model::Model,
    target: Option<&str>,
) -> Result<()> {
    let Op::Convert { format, output: Some(path), .. } = op else {
        return Ok(());
    };
    let same_file = |a: &str, b: &str| Path::new(a).file_name() == Path::new(b).file_name();
    if target.is_some_and(|t| same_file(t, path)) || is_planned_target(path) {
        return Ok(());
    }
    let out = repo.dir.join(path);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // The step's `--format` wins over the name, exactly as it does for the
    // `convert` command: OBA writes functional syntax to a `.owl` path.
    let fmt = format
        .as_deref()
        .and_then(|f| crate::io::Format::from_name(f).ok())
        .or_else(|| crate::io::Format::from_path(&out).ok());
    // What the closure declares decides what this document must stub, exactly as
    // it does for a target's own closing write. A mirror recipe is the case:
    // `convert -I …/taxslim-disjoint-over-in-taxon.owl -o tmp/mirror-<id>.owl`
    // states nothing but disjointness over taxa that `taxslim.owl` declares, and
    // every one of them belongs to the import rather than to this document.
    if matches!(fmt, Some(crate::io::Format::RdfXml) | None) && model_has_imports(model) {
        let catalog = load_catalog_planned(repo);
        if let Some(cl) = load_closure(model, &repo.dir, &catalog)? {
            model.closure_ann_ns = annotation_property_namespaces(&cl);
            model.closure_declared = closure_declared_entities(&cl);
        }
        withdraw_materialised_declarations(model);
    }
    // An OBO write comments every clause target that has a label, and a target
    // the root only references is labelled by the ontology that declares it —
    // the closure's labels fill the map exactly as they do for a target's own
    // closing write.
    if matches!(fmt, Some(crate::io::Format::Obo)) && model_has_imports(model) {
        let catalog = load_catalog_planned(repo);
        if let Some(cl) = load_closure(model, &repo.dir, &catalog)? {
            model.banner_labels = closure_labels(&cl);
        }
    }
    match fmt {
        Some(f) => crate::io::save_as(model, &out, f),
        None => crate::io::save(model, &out),
    }
    .with_context(|| format!("writing step output {}", out.display()))
}

fn apply_op(
    repo: &Repo,
    op: &Op,
    model: crate::model::Model,
    catalog: &BTreeMap<String, PathBuf>,
    work: &Path,
    closure: Option<&crate::model::Model>,
    // The file the threaded model was loaded from, if any. `merge -i $<` names
    // the SAME file the pipeline already read, and it must be read once — see
    // `Op::Merge` below.
    pipeline_input: Option<&Path>,
) -> Result<crate::model::Model> {
    let mut model = model;
    if std::env::var_os("OM_PIPE_DEBUG").is_some() {
        eprintln!("[pipe] {:?} in: shared_anon={} owners", std::mem::discriminant(op), model.shared_anon.len());
    }
    Ok(match op {
        Op::Merge { inputs, collapse_import_closure } => {
            let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
            // The pipeline input is ALREADY this model. `merge -i $<` is one read of
            // one file; owlmake loads `$<` to start the chain and then reaches this
            // op with `$<` still listed as an input, so merging it again would read
            // it twice. The axioms and prefixes survive that (both go into sets),
            // but the blank-node accounting does not: `carry_shared_anon` adds the
            // secondary's `anon_alloc_total` to the target's `anon_alloc_base`, so
            // the file's own allocations would be added to its own base and every
            // anonymous individual numbered from the wrong origin. EFO's
            // `build/efo.owl` would order its fourteen obsolescence blocks at base
            // 641861 (= 588211 imports + efo-edit's own 53650) instead of 588211.
            // Compared canonically: the chain resolves `$<` through `resolve_input`
            // (which may hand back an OFN cache) and this op through
            // `resolve_repo_file`, so the two spellings of one file need not match.
            let threaded = pipeline_input.and_then(|p| p.canonicalize().ok());
            let is_threaded = |p: &Path| {
                threaded.is_some() && p.canonicalize().ok() == threaded
            };
            // Merge each explicit `--input` file's own axioms into the model. A
            // missing input that has a rule of its own (e.g. MONDO's `skos.ttl`,
            // built by a perl script) is built now, before it is read.
            for inp in inputs {
                // `-I/--input-iri`: the input is fetched, not opened. It reaches
                // here in the same list because it is an input like any other, and
                // the plan has to NAME it either way.
                if inp.starts_with("http://") || inp.starts_with("https://") {
                    let other = crate::io::load_iri(inp, None)?;
                    crate::cmd::merge::merge_into(
                        &mut model,
                        &other,
                        &crate::cmd::merge::MergeOptions::default(),
                    );
                    continue;
                }
                if resolve_repo_file(repo, inp, work).is_none() && repo.target(inp).is_some() {
                    let mut seen = std::collections::HashSet::new();
                    run_target_recipe_inner(repo, inp, &mut seen)
                        .with_context(|| format!("building merge prerequisite {inp}"))?;
                }
                // An input that still cannot be resolved is a HARD error. Skipping
                // it would drop a whole branch of the ontology and still exit 0 —
                // the artefact would look built and be silently incomplete.
                let Some(p) = resolve_repo_file(repo, inp, work) else {
                    bail!(
                        "merge input `{inp}` does not exist and cannot be built \
                         (it is not a prerequisite of this target and has no rule)"
                    );
                };
                if is_threaded(&p) {
                    seen.insert(p.clone());
                } else if seen.insert(p.clone()) {
                    merge_file_into(&mut model, &p)?;
                }
            }
            if *collapse_import_closure == Some(false) {
                // Keep imports as declarations and do NOT merge their axioms — they
                // stay a read-only reasoning closure (`--collapse-import-closure
                // false`, MONDO's `filtered.owl`).
                model
            } else {
                // Default `merge`: resolve every input's import closure plus
                // the model's own `owl:imports`, merge them verbatim, then drop the
                // import declarations.
                for inp in inputs {
                    if let Some(p) = resolve_repo_file(repo, inp, work) {
                        for f in import_closure(&p, &repo.dir, catalog, &mut seen)? {
                            merge_file_into_as(&mut model, &f, MergeRole::Import)?;
                        }
                    }
                }
                for f in import_closure_of_model(&model, &repo.dir, catalog, &mut seen)? {
                    merge_file_into_as(&mut model, &f, MergeRole::Import)?;
                }
                drop_imports(model)
            }
        }
        Op::Unmerge { second_input } => {
            match second_input.as_deref() {
                None => model,
                // `-I/--input-iri`: the ontology to subtract is fetched, not
                // opened — CL removes the taxon disjointness axioms this way.
                Some(si) if si.starts_with("http://") || si.starts_with("https://") => {
                    let other = crate::io::load_iri(si, None)?;
                    unmerge_model(model, &other)
                }
                Some(si) => {
                    if resolve_repo_file(repo, si, work).is_none() && repo.target(si).is_some() {
                        let mut seen = std::collections::HashSet::new();
                        run_target_recipe_inner(repo, si, &mut seen)
                            .with_context(|| format!("building unmerge input {si}"))?;
                    }
                    // An input that cannot be resolved is a HARD error, as for
                    // `merge`: subtracting nothing leaves every axiom the step
                    // exists to remove, and exits 0.
                    let Some(p) = resolve_repo_file(repo, si, work) else {
                        bail!("unmerge input `{si}` does not exist");
                    };
                    let other = crate::io::load(&p)?;
                    unmerge_model(model, &other)
                }
            }
        }
        Op::Reason {
            reasoner,
            equivalent_classes_allowed,
            exclude_tautologies,
            annotate_inferred_axioms,
            allow_incoherent,
            exclude_external_entities,
            exclude_owl_thing,
            remove_redundant_subclass_axioms,
            create_new_ontology,
            create_new_ontology_with_annotations,
            exclude_duplicate_axioms,
        } => {
            let reasoner = reasoner.as_deref().unwrap_or("elk");
            // Honour the recipe's reason flags rather than hardcoding them. A
            // standard release target reasons with `--exclude-tautologies
            // structural --equivalent-classes-allowed asserted-only`, but custom
            // builds differ (EFO sets `asserted-only` and NO `--exclude-tautologies`,
            // so its release keeps `X ⊑ owl:Thing` axioms that a forced `structural`
            // would strip). An absent flag falls back to the command's own default
            // (true for remove-redundant, false for the rest, none for
            // exclude-tautologies, all for equivalent-classes-allowed).
            let ropts = cmd::reason::ReasonOptions {
                annotate_inferred_axioms: annotate_inferred_axioms.unwrap_or(false),
                // Default false: a release built over an incoherent ontology is
                // silently wrong, not obviously wrong. Everything under
                // an unsatisfiable class is redundant, so the pipeline's `reduce`
                // deletes its hierarchy and the run still reports success.
                allow_incoherent: allow_incoherent.unwrap_or(false),
                equivalent_classes_allowed: equivalent_classes_allowed
                    .clone()
                    .unwrap_or_else(|| "all".to_string()),
                exclude_tautologies: exclude_tautologies.clone(),
                remove_redundant_subclass_axioms: remove_redundant_subclass_axioms.unwrap_or(true),
                create_new_ontology: create_new_ontology.unwrap_or(false),
                create_new_ontology_with_annotations: create_new_ontology_with_annotations
                    .unwrap_or(false),
                exclude_duplicate_axioms: exclude_duplicate_axioms.unwrap_or(false),
                exclude_external_entities: exclude_external_entities.unwrap_or(false),
                exclude_owl_thing: exclude_owl_thing.unwrap_or(false),
                ..Default::default()
            };
            match closure {
                None => cmd::reason::reason_with(model, reasoner, &ropts)?,
                Some(c) => reason_with_closure(model, c, reasoner, &ropts)?,
            }
        }
        Op::Relax { include_subclass_of } => cmd::relax::relax_with(
            model,
            &cmd::relax::RelaxOptions {
                include_subclass_of: *include_subclass_of,
                ..Default::default()
            },
        ),
        Op::Reduce { include_subproperties, .. } => {
            // `reduce` defaults --include-subproperties to false; honour an
            // explicit recipe value (e.g. UBERON's `--include-subproperties true`).
            let subprops = include_subproperties.unwrap_or(false);
            match closure {
                None => cmd::reduce::reduce_with_opts(&model, false, false, subprops),
                Some(c) => reduce_with_closure(model, c, subprops),
            }
        }
        Op::Materialize { properties, term_files } => {
            let mut raw = properties.clone();
            for tf in term_files {
                raw.extend(read_terms(&repo.dir.join(tf))?);
            }
            let props: std::collections::HashSet<String> =
                raw.iter().map(|t| cmd::select::expand(&model, t)).collect();
            cmd::materialize::materialize(model, &props)
        }
        Op::Remove(spec) => {
            let tf = resolve_term_files(repo, &spec.term_files, work)?;
            let opts = cmd::remove::TermOptions {
                trim: spec.trim,
                preserve_structure: spec.preserve_structure,
                // `--exclude-term` names what must SURVIVE the removal.
                exclude_term: spec.exclude_terms.clone(),
                exclude_terms: resolve_term_files(repo, &spec.exclude_term_files, work)?,
                signature: spec.signature,
                drop_axiom_annotations: spec.drop_axiom_annotations.clone(),
                ..Default::default()
            };
            cmd::remove::remove_with(
                model,
                &spec.terms,
                &tf,
                &spec.selects,
                &spec.axioms,
                &spec.base_iri,
                &opts,
            )?
        }
        Op::Filter(spec) => {
            let tf = resolve_term_files(repo, &spec.term_files, work)?;
            // `filter` RUNS spanGaps unless `--preserve-structure false`: the flag
            // defaults to true, and gap-spanning is part of what it produces.
            // Without it a filtered ontology loses the `rdfs:subClassOf` chain
            // through dropped intermediates, and MONDO's
            // `tmp/rare-seed-entities.txt` — whose query is
            // `?cls rdfs:subClassOf+ MONDO_0000001` — comes out 129 entries short,
            // taking `subsets/mondo-rare.owl` with it.
            //
            // `trim` carries its usual meaning here (`partial = !trim`), which is
            // what `filter_with` implements; `signature` keeps the
            // `-simple`/`-basic` mapping. For MONDO's two filters the two agree:
            // mondo-simple is `--trim true --signature true` (complete match) and
            // the rare seed is `--trim false` (any-entity).
            let opts = cmd::remove::TermOptions {
                trim: spec.trim,
                signature: spec.signature,
                preserve_structure: Some(true),
                ..Default::default()
            };
            // `--axioms` was dropped on the floor here: UBERON's
            // `composite-*-basic.owl` is `filter --axioms "subclass equivalent
            // annotation"`, and passing `&[]` kept every axiom type.
            // The recipe's `--prefix` bindings decide what a `--select` CURIE
            // resolves to, so they have to be on the model before the selector is
            // read. UBERON's `cumbo` list selects `oboInOwl:inSubset=uberon:cumbo`,
            // where `uberon:` is bound by the recipe to `…/obo/uberon/core#` and by
            // nothing else.
            if !spec.prefixes.is_empty() {
                let common = cmd::CommonArgs {
                    add_prefix: spec.prefixes.clone(),
                    ..Default::default()
                };
                common.apply(&mut model)?;
            }
            cmd::filter::filter_with(model, &spec.terms, &tf, &spec.selects, &spec.axioms, &[], &opts)?
        }
        Op::Annotate(spec) => {
            let mut annotation = Vec::new();
            for (p, v) in &spec.annotations {
                annotation.push(p.clone());
                annotation.push(v.clone());
            }
            let mut link_annotation = Vec::new();
            for (p, v) in &spec.link_annotations {
                link_annotation.push(p.clone());
                link_annotation.push(v.clone());
            }
            cmd::annotate::annotate(
                model,
                spec.ontology_iri.as_deref(),
                spec.version_iri.as_deref(),
                &annotation,
                &link_annotation,
                spec.remove_annotations,
            )?
        }
        Op::Repair { invalid_references, merge_axiom_annotations } => cmd::repair::repair_with(
            model,
            &cmd::repair::RepairOptions {
                invalid_references: *invalid_references,
                merge_axiom_annotations: *merge_axiom_annotations,
            },
        ),
        Op::Template { templates, merge, prefixes } => {
            let rp = |s: &str| repo.dir.join(s);
            // The recipe's `--prefix` bindings resolve the template's header
            // CURIEs, so they have to reach the command that reads the header.
            let common =
                cmd::CommonArgs { add_prefix: prefixes.clone(), ..Default::default() };
            let targs = cmd::template::Args {
                template: templates.iter().map(|t| rp(t)).collect(),
                input: None,
                output: None,
                format: None,
                force: Some(true),
                errors: None,
                external_template: vec![],
                ontology_iri: None,
                version_iri: None,
                merge_before: Some(*merge),
                merge_after: None,
                ancestors: None,
                include_annotations: None,
                collapse_import_closure: None,
                common,
            };
            cmd::template::step(Some(model), &targs)?.unwrap_or_else(crate::model::Model::new)
        }
        Op::Rename { mappings, prefix_mappings, allow_missing } => {
            let rp = |s: &String| repo.dir.join(s);
            let rargs = cmd::rename::Args {
                input: None,
                output: None,
                format: None,
                mapping: vec![],
                mappings: mappings.as_ref().map(rp),
                allow_missing_entities: Some(*allow_missing),
                allow_duplicates: None,
                prefix_mappings: prefix_mappings.as_ref().map(rp),
                common: Default::default(),
            };
            cmd::rename::step(Some(model), &rargs)?.unwrap_or_else(crate::model::Model::new)
        }
        Op::Extract {
            method, terms, term_files, copy_ontology_annotations, individuals,
            branch_from_terms, branch_from_term_files,
        } => {
            let rp = |s: &String| repo.dir.join(s);
            let eargs = cmd::extract::Args {
                input: None,
                output: None,
                format: None,
                method: method.clone(),
                term: terms.clone(),
                term_file: term_files.iter().map(rp).collect(),
                upper_term: vec![],
                upper_terms: vec![],
                lower_term: vec![],
                lower_terms: vec![],
                branch_from_term: branch_from_terms.clone(),
                branch_from_terms: branch_from_term_files.iter().map(rp).collect(),
                copy_ontology_annotations: Some(*copy_ontology_annotations),
                annotate_with_source: None,
                individuals: individuals.clone().unwrap_or_else(|| "include".into()),
                imports: "include".into(),
                intermediates: "all".into(),
                sources: None,
                force: Some(true),
                output_iri: None,
                common: Default::default(),
            };
            cmd::extract::step(Some(model), &eargs)?.unwrap_or_else(crate::model::Model::new)
        }
        Op::ExtractUphenoRelations { relations, terms, term_files, roots, root_files } => {
            crate::cmd::extract_upheno_relations::apply(
                &mut model,
                &repo.dir,
                relations,
                terms,
                term_files,
                roots,
                root_files,
            )?;
            model
        }
        Op::Mint { temp_id_prefix, id_range_name, id_ranges } => {
            // The writer must not materialise declaration stubs for classes the
            // imports already declare, so hand it the closure signature — the
            // standalone command does the same from its own `-i`, which a threaded
            // model does not have.
            if let Some(cl) = closure {
                model.closure_declared = closure_declared_entities(cl);
            } else if let Some(cl) = load_closure(&model, &repo.dir, catalog)? {
                model.closure_declared = closure_declared_entities(&cl);
            }
            // The plan NAMES the ID-ranges file; execution never globs the ontology
            // directory for `*-idranges.owl` when the op leaves it unset, which
            // would be a filesystem probe deciding a build input. Ingest resolves
            // it, and an op it cannot resolve is recorded as `Step::Unresolved` and
            // blocks its target instead of reaching here.
            let ranges = id_ranges.clone().map(|r| repo.dir.join(r));
            let args = cmd::mint::Args {
                input: None,
                output: None,
                format: None,
                temp_id_prefix: temp_id_prefix.clone(),
                id_range_name: id_range_name.clone(),
                id_ranges: ranges,
                common: Default::default(),
            };
            cmd::mint::step(Some(model), &args)?
                .expect("mint returns the model it was piped")
        }
        Op::AddPrefix { prefixes } => {
            // `"foo: http://bar"` — the spelling ROBOT's global option takes. The
            // binding goes on the model, so the document written at the end of the
            // chain declares it even where nothing references it: an `xmlns:obo`
            // the reference emits and owlmake did not is a byte difference, and a
            // binding a later step DOES resolve a CURIE against is a different IRI.
            //
            // Both maps: an RDF/XML document's `xmlns` block is written from
            // `idspaces` — the verbatim bindings its source carried — and the
            // formal prefix map is consulted only where there are none. A binding
            // that reaches one and not the other declares itself in some output
            // formats and not others.
            for spec in prefixes {
                if let Some((name, ns)) = spec.split_once(':') {
                    let (name, ns) = (name.trim(), ns.trim());
                    let _ = model.prefixes.add_prefix(name, ns);
                    if !model.idspaces.iter().any(|(p, _)| p == name) {
                        model.idspaces.push((name.to_string(), ns.to_string()));
                    }
                    // …including the format prefixes an RDF/XML source carried:
                    // those take precedence over both maps above when the xmlns
                    // block is written, so a binding that reached only the others
                    // would be declared in every output format except the one this
                    // recipe writes.
                    if !model.rdf_prefixes.iter().any(|(p, _)| p == name) {
                        model.rdf_prefixes.push((name.to_string(), ns.to_string()));
                    }
                }
            }
            model
        }
        Op::Normalize { base_iris, subset_decls, synonym_decls, add_source } => {
            // A recorded `--base-iri` NARROWS nothing: the namespaces it names are
            // declared IN ADDITION to the built-in OBO / EFO / Biolink ones.
            let opts = cmd::normalize::NormalizeOptions {
                base_iris: cmd::normalize::NormalizeOptions::default()
                    .base_iris
                    .into_iter()
                    .chain(base_iris.iter().cloned())
                    .collect(),
                subset_decls: *subset_decls,
                synonym_decls: *synonym_decls,
                add_source: *add_source,
            };
            cmd::normalize::normalize_with(model, &opts)
        }
        Op::Babelon { input, output, format } => {
            // Source op: read the translation TSV (relative to the ontology dir)
            // and emit OWL annotation axioms, discarding any piped model.
            let path = repo.dir.join(input);
            // …unless the recipe asked for the babelon JSON profile, which is the
            // TABLE, not an ontology. It threads no model, so the step writes its
            // own output and hands the pipeline an empty one.
            if format.as_deref() == Some("json") {
                let table = cmd::babelon_tsv::Table::read(&path)?;
                let dst = repo.dir.join(output.as_deref().unwrap_or_default());
                if let Some(d) = dst.parent() {
                    std::fs::create_dir_all(d).ok();
                }
                std::fs::write(&dst, cmd::babelon::babelon_json(&table))?;
                return Ok(model);
            }
            let mut m = cmd::babelon::convert_file(&path)?;
            // Write the recipe's own `-o` as well as threading the model: babelon
            // runs as a process of its own in a recipe, so a following `merge -i`
            // reads that file rather than continuing a pipeline.
            if let Some(out) = output {
                let dst = repo.dir.join(out);
                if let Some(d) = dst.parent() {
                    std::fs::create_dir_all(d).ok();
                }
                // The recipe's `--output-format owl` is RDF/XML, which is what the
                // `.tmp` extension would not tell us on its own.
                crate::io::save_as(&mut m, &dst, crate::io::Format::RdfXml)?;
            }
            m
        }
        Op::Collapse { precious, precious_files, threshold } => {
            let cargs = cmd::collapse::Args {
                input: None,
                output: None,
                format: None,
                term: vec![],
                term_file: vec![],
                precious: precious.clone(),
                precious_terms: precious_files.iter().map(PathBuf::from).collect(),
                threshold: *threshold,
                common: Default::default(),
            };
            cmd::collapse::step(Some(model), &cargs)?.unwrap_or_else(crate::model::Model::new)
        }
        Op::Expand { expand_terms, expand_term_files, no_expand_terms, no_expand_term_files } => {
            let rp = |s: &String| repo.dir.join(s).to_string_lossy().into_owned();
            let eargs = cmd::expand::Args {
                input: None,
                output: None,
                format: None,
                expand_term: expand_terms.clone(),
                expand_term_file: expand_term_files.iter().map(|f| rp(f).into()).collect(),
                no_expand_term: no_expand_terms.clone(),
                no_expand_term_file: no_expand_term_files.iter().map(|f| rp(f).into()).collect(),
                create_new_ontology: None,
                annotate_expansion_axioms: None,
                common: Default::default(),
            };
            cmd::expand::step(Some(model), &eargs)?.unwrap_or_else(crate::model::Model::new)
        }
        // `cmd::subset::step` picks the mode: query mode when any of
        // `queries`/`terms`/`term_files` is present, inSubset mode otherwise. The
        // old call went straight to the inSubset entry point, so a recorded query
        // could not have run even once it was in the plan.
        Op::Subset { subset, queries, terms, term_files, reasoner, ancestors, fill_gaps } => {
            let rp = |s: &String| repo.dir.join(s);
            let sargs = cmd::subset::Args {
                input: None,
                output: None,
                format: None,
                subset: (!subset.is_empty()).then(|| subset.clone()),
                query: queries.clone(),
                ancestors: *ancestors,
                reasoner: reasoner.clone(),
                term: terms.clone(),
                term_file: term_files.iter().map(rp).collect(),
                fill_gaps: *fill_gaps,
                common: Default::default(),
            };
            cmd::subset::step(Some(model), &sargs)?.unwrap_or_else(crate::model::Model::new)
        }
        Op::ExtractOntologySubset { subset, fill_gaps } => {
            cmd::owltools_ops::extract_ontology_subset(model, subset, *fill_gaps)
        }
        Op::ExtractMingraph => cmd::owltools_ops::extract_mingraph(model),
        Op::RemoveAxiomAnnotations => cmd::owltools_ops::remove_axiom_annotations(model),
        Op::MakeSubsetByProperties { properties } => {
            cmd::owltools_ops::make_subset_by_properties(model, properties)
        }
        Op::MergeSpecies {
            batch_file, extended, gca_translate, gca_delete, remove_declarations,
            taxon, suffix, properties, included,
        } => {
            use cmd::merge_species as ms;
            let ops = if let Some(bf) = batch_file {
                // The plan names ONE source of merge operations and execution
                // reads that one. A batch file the plan names and cannot find is
                // a hard error, not a cue to derive the table a different way
                // from `config/taxa.yaml` — a file's presence must not pick
                // between two derivations.
                let p = repo.dir.join(bf);
                ms::parse_batch(&std::fs::read_to_string(&p).with_context(|| {
                    format!("merge-species batch file {} is missing", p.display())
                })?)
            } else if let Some(t) = taxon {
                let props = if properties.is_empty() {
                    vec![cmd::babelon::expand_curie("BFO:0000050")]
                } else {
                    properties.clone()
                };
                vec![ms::MergeOp {
                    taxon: cmd::babelon::expand_curie(t),
                    label: suffix.clone().unwrap_or_else(|| "species specific".into()),
                    link_properties: props,
                    included_properties: included.clone(),
                }]
            } else {
                vec![]
            };
            let gca_mode = if *gca_translate {
                ms::GcaMode::Translate
            } else if *gca_delete {
                ms::GcaMode::Delete
            } else {
                ms::GcaMode::Original
            };
            ms::merge_species(
                model,
                &ops,
                &ms::Options {
                    extended_translation: *extended,
                    gca_mode,
                    remove_declarations: *remove_declarations,
                },
            )?
        }
        Op::SimpleSubset { ont_id } => cmd::oort::simple_subset(model, ont_id),
        Op::RewriteDef(spec) => {
            // Build label/definition lookup tables from the edit ontology plus its
            // import closure (a SUB placeholder's target, or a differentia filler's
            // label, usually lives in an import) — but rewrite (and output) only the
            // edit ontology itself; imports stay declared, not merged.
            let mut maps = cmd::rewrite_def::Maps::default();
            maps.collect(&model);
            let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
            for f in import_closure_of_model(&model, &repo.dir, catalog, &mut seen)? {
                if let Ok(imp) = crate::io::load(&f) {
                    maps.collect(&imp);
                }
            }
            let opts = cmd::rewrite_def::RewriteOptions {
                sub: spec.sub,
                dot: spec.dot,
                null_definitions: spec.null_definitions,
                include_ids: !spec.no_ids,
                include_obsolete: spec.include_obsolete,
                filter_prefix: spec.filter_prefix.clone(),
                add_annotation: split_pairs(&spec.add_annotation),
                add_annotation_iri: split_pairs(&spec.add_annotation_iri),
            };
            cmd::rewrite_def::rewrite_with_maps(model, &maps, &opts)
        }
        Op::MergeEquivalentSets { set_prefix, label_prefix, definition_prefix } => {
            use crate::cmd::merge_equivalent_sets as mes;
            mes::merge_equivalent_sets(
                model,
                &mes::parse_prio(set_prefix),
                &mes::parse_prio(label_prefix),
                &mes::parse_prio(definition_prefix),
            )?
        }
        Op::Query { updates, selects, constructs, format, use_graphs, tdb } => {
            // Recipe paths are relative to the ontology dir.
            let rp = |s: &str| repo.dir.join(s);
            let mut m = model;
            // `--use-graphs true` loads the import closure as named graphs and
            // makes the default graph their UNION, so the query sees the whole
            // closure. The pipeline hands over the root ontology alone; union it
            // in here, over the catalog the plan names.
            if *use_graphs {
                if let Some(cl) = load_closure(&m, &repo.dir, catalog)? {
                    m = union_with_closure(&m, &cl);
                }
            }
            // Updates (transform the model) + SELECTs (write result tables) in one
            // pass; owlmake's query engine handles both.
            if !updates.is_empty() || !selects.is_empty() {
                // Under `--tdb`, row order is each term's first appearance in the
                // INPUT DOCUMENT, so the query step needs the file the pipeline
                // read — not to load from (the model is already threaded in) but
                // to scan. Left `None`, that ordering would hold for a bare
                // `om query -i mondo-base.owl` and not on the build path, where
                // `reports/mondo_base_current_release-report.tsv` would come out as
                // the same 36,080 rows permuted.
                let qargs = cmd::query::Args {
                    input: if *tdb { pipeline_input.map(|p| p.to_path_buf()) } else { None },
                    query: vec![],
                    query_string: None,
                    construct: vec![],
                    select: vec![],
                    query_pairs: selects.iter().flat_map(|(f, o)| [rp(f), rp(o)]).collect(),
                    queries: vec![],
                    update: updates.iter().map(|u| rp(u)).collect(),
                    output: None,
                    output_dir: None,
                    // A recipe that names no `--format` leaves the choice to the
                    // OUTPUT PATH: an empty format is "not given", and resolution
                    // falls through to the output's extension. A recipe whose
                    // whole query step is `query --select $*.sparql $@` therefore
                    // writes TSV to a `.tsv` and CSV to a `.csv`. An extension that
                    // is not a result-format name — `$@.tmp` — resolves to CSV.
                    format: format.clone().unwrap_or_default(),
                    use_graphs: None,
                    tdb: Some(*tdb),
                    keep_tdb_mappings: None,
                    tdb_directory: None,
                    create_tdb: None,
                    temporary_file: None,
                    common: Default::default(),
                };
                m = cmd::query::step(Some(m), &qargs)?.unwrap_or_else(crate::model::Model::new);
            }
            // CONSTRUCTs (write an RDF graph each); model is unchanged.
            for (f, o) in constructs {
                let qargs = cmd::query::Args {
                    input: None,
                    query: vec![],
                    query_string: None,
                    construct: vec![rp(f)],
                    select: vec![],
                    query_pairs: vec![],
                    queries: vec![],
                    update: vec![],
                    output: Some(rp(o)),
                    output_dir: None,
                    format: format.clone().unwrap_or_else(|| "ttl".into()),
                    use_graphs: None,
                    tdb: None,
                    keep_tdb_mappings: None,
                    tdb_directory: None,
                    create_tdb: None,
                    temporary_file: None,
                    common: Default::default(),
                };
                m = cmd::query::step(Some(m), &qargs)?.unwrap_or_else(crate::model::Model::new);
            }
            m
        }
        // Format is applied by the final write. `--clean-obo` transforms the
        // model up-front (drop GCI/untranslatable axioms, merge comments) when
        // the artefact is written as OBO — CL's cl.obo uses
        // `--clean-obo 'simple merge-comments'`.
        // Write the model out and read it straight back, reproducing the round trip
        // a recipe's `-o` performs between two processes. The write is the point —
        // an RDF/XML round trip is not identity.
        Op::RoundTrip { path, .. } => {
            let out = repo.dir.join(path);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut model = model;
            crate::io::save(&mut model, &out)
                .with_context(|| format!("round-tripping through {}", out.display()))?;
            let mut back = crate::io::load(&out)
                .with_context(|| format!("re-reading {}", out.display()))?;
            // An RDF/XML write drops an explicit `^^xsd:string` (it is the implicit
            // datatype), so re-reading turns every `Datatype{xsd:string}` literal
            // into a plain one — and a subject's annotations are ordered by their
            // literals' DATATYPE first, with one subject's triples otherwise kept
            // in insertion order. A recipe that never re-parses keeps the
            // `xsd:string` an OBO-sourced literal carries and orders it AFTER a
            // plain one from an RDF/XML import. Restore the literal
            // forms from the model that was written: OBA:1000001's two
            // `hasExactSynonym`s are the case, and ~1030 of `oba-full.owl`'s 1048
            // differing lines are this.
            restore_literal_datatypes(&mut back, &model);
            // The re-read is here to model what the NEXT process would see of the
            // axioms (see `collapse_rdf_roundtrip`), not to restart the pipeline: a
            // recipe's `-o $@.tmp.owl && mv` never re-parses at all, so everything
            // owlmake tracks ALONGSIDE the axioms has to survive it.
            // `plain_literals_typed` is the one that shows: MONDO's
            // `mondo-simple.owl` chain ends `… query --update … annotate`, and that
            // `--update` is what makes an untyped literal `xsd:string` rather than
            // `rdf:PlainLiteral` — which decides whether the typed or the untyped
            // `IAO_0000233` sorts first. Re-reading RDF/XML resets the flag and
            // would put the pair back the wrong way round.
            back.carry_meta_from(&model);
            back
        }
        Op::Convert { clean_obo, format, add_prefixes, .. } => {
            let mut model = model;
            // `--add-prefixes FILE`: fold each JSON-LD context's prefixes into
            // the model's map so the OFN/OBO output declares AND abbreviates with
            // them (and a cached-OFN re-read round-trips, e.g. `Orphanet:377788`).
            for file in add_prefixes {
                let path = repo.dir.join(file);
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        let ctx = json.get("@context").unwrap_or(&json);
                        if let Some(map) = ctx.as_object() {
                            for (k, v) in map {
                                // JSON-LD values are a bare namespace string or a
                                // `{"@id": "...", "@prefix": true}` object.
                                let ns =
                                    v.as_str().or_else(|| v.get("@id").and_then(|x| x.as_str()));
                                if let Some(ns) = ns {
                                    let _ = model.prefixes.add_prefix(k, ns);
                                    // Track as explicit so the OBO writer emits an
                                    // `idspace:` for each, regardless of use.
                                    if !model.explicit_prefixes.iter().any(|(p, _)| p == k) {
                                        model.explicit_prefixes.push((k.clone(), ns.to_string()));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(spec) = clean_obo {
                // clean-obo only applies to OBO output. The `convert` step's own
                // `-f obo` (or the artefact writing `.obo`) marks that; CL's cl.obo
                // recipe is `convert -f obo --clean-obo 'simple merge-comments'`.
                if format.as_deref() == Some("obo") {
                    cmd::convert::apply_clean_obo(&mut model, spec);
                }
            }
            model
        }
    })
}

/// Resolve a recipe file token to a concrete path (ontology dir, with a `.ofn`
/// fallback for built intermediates, then the working dir by basename).
fn resolve_repo_file(repo: &Repo, rel: &str, work: &Path) -> Option<PathBuf> {
    let p = repo.dir.join(rel);
    if p.exists() {
        return Some(p);
    }
    let ofn = p.with_extension("ofn");
    if ofn.exists() {
        return Some(ofn);
    }
    if let Some(name) = Path::new(rel).file_name() {
        for d in [work, repo.dir.as_path()] {
            let q = d.join(name);
            if q.exists() {
                return Some(q);
            }
            // run_artefact caches each built intermediate as a basename `.ofn`
            // (e.g. `tmp/dpo-preprocess.owl` → `<work>/dpo-preprocess.ofn`); reuse
            // it rather than rebuilding the prerequisite.
            let ofn = d.join(Path::new(name).with_extension("ofn"));
            if ofn.exists() {
                return Some(ofn);
            }
        }
    }
    None
}

/// Fetch an `owl:imports` IRI the catalog does not cover, caching it under the
/// work directory, and return the local path. Announced, because resolving an
/// import off the network rather than out of the repo is worth seeing in a log.
fn fetch_import_iri(iri: &str, dir: &Path) -> Result<PathBuf> {
    let cache_dir = dir.join(".owlmake-odk-tmp").join("imports-cache");
    std::fs::create_dir_all(&cache_dir)?;
    let name: String = iri
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    let path = cache_dir.join(name);
    if path.exists() {
        return Ok(path);
    }
    status!("import: owl:imports <{iri}> (network — not in the catalog)");
    let bytes = http_get(iri)?;
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

/// Files imported (`owl:imports`) by the current model, resolved via the catalog.
/// Does this recipe write OWL functional syntax — the one serialization whose
/// per-entity banners carry labels?
fn writes_functional_syntax(steps: &[Step]) -> bool {
    steps.iter().any(|s| {
        matches!(
            s,
            Step::Op(Op::Convert { format: Some(f), .. })
                | Step::Partial { op: Op::Convert { format: Some(f), .. }, .. }
            if matches!(crate::io::Format::from_name(f), Ok(crate::io::Format::Functional))
        )
    })
}

/// The banner label set for a document with an import closure. Each document —
/// the pipeline input and every file its closure resolves to — settles its own
/// candidates by the one per-document rule (`cmd::rdfs_labels`); between
/// documents, the first one in `owlapi_hash::ontology_set_order` that labels a
/// subject names it. The input document's identity is the one it will be
/// WRITTEN under: `write_version_iri` (the version a later step of the same
/// pipeline sets) overrides the version it was read with, so a banner pick
/// tracks the run's release date. Best-effort — a closure file that cannot be
/// read contributes nothing, and banners fall back to the entity IRI.
fn closure_banner_labels(
    model: &crate::model::Model,
    dir: &Path,
    catalog: &BTreeMap<String, PathBuf>,
    write_version_iri: Option<&str>,
) -> std::collections::HashMap<String, String> {
    let main_id = model_ontology_id(model);
    let main_version = write_version_iri.map(str::to_string).or(main_id.1);
    if std::env::var("OM_BANNER_DEBUG").is_ok() {
        eprintln!("[banner] input document id={:?} write version={:?}", main_id.0, main_version);
    }
    let mut docs: Vec<(i32, std::collections::HashMap<String, String>)> = vec![(
        crate::owlapi_hash::ontology_id_hash(main_id.0.as_deref(), main_version.as_deref()),
        crate::cmd::rdfs_labels(model),
    )];
    let mut seen = std::collections::HashSet::new();
    if let Ok(files) = import_closure_of_model(model, dir, catalog, &mut seen) {
        for f in &files {
            let Ok(m) = crate::io::load(f) else { continue };
            let (iri, ver) = model_ontology_id(&m);
            docs.push((
                crate::owlapi_hash::ontology_id_hash(iri.as_deref(), ver.as_deref()),
                crate::cmd::rdfs_labels(&m),
            ));
        }
    }
    let hashes: Vec<i32> = docs.iter().map(|(h, _)| *h).collect();
    let mut out = std::collections::HashMap::new();
    for i in crate::owlapi_hash::ontology_set_order(&hashes) {
        if std::env::var("OM_BANNER_DEBUG").is_ok() {
            eprintln!("[banner] doc#{i} id-hash={} labels={}", hashes[i], docs[i].1.len());
        }
        for (subj, label) in &docs[i].1 {
            out.entry(subj.clone()).or_insert_with(|| label.clone());
        }
    }
    out
}

/// The ontology IRI and version IRI a model's document identifies itself by.
fn model_ontology_id(model: &crate::model::Model) -> (Option<String>, Option<String>) {
    use horned_owl::model::Component;
    for ac in model.ont.iter() {
        if let Component::OntologyID(id) = &ac.component {
            return (
                id.iri.as_ref().map(|i| i.to_string()),
                id.viri.as_ref().map(|v| v.to_string()),
            );
        }
    }
    (None, None)
}

fn import_closure_of_model(
    model: &crate::model::Model,
    dir: &Path,
    catalog: &BTreeMap<String, PathBuf>,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Result<Vec<PathBuf>> {
    use horned_owl::model::Component;
    let mut out = Vec::new();
    let iris: Vec<String> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::Import(i) => Some(i.0.to_string()),
            _ => None,
        })
        .collect();
    for iri in iris {
        // The plan's catalog map is the answer. `default_local` — a bare
        // basename probe of the sibling directory — is a filesystem convention
        // that matches neither the catalog nor the `/obo/` PURL rule, i.e. a
        // third resolution policy discovered at build time.
        match catalog.get(&iri).cloned() {
            Some(p) => {
                if seen.insert(p.clone()) {
                    if p.exists() {
                        out.push(p.clone());
                        out.extend(import_closure(&p, dir, catalog, seen)?);
                    } else {
                        bail!(
                            "`owl:imports <{iri}>` maps to {} in the catalog, which does not exist",
                            p.display()
                        );
                    }
                }
            }
            // Not in the catalog, so the next move is the IRI itself — and that
            // is not hypothetical: `mirror/mfomd.owl` is the one MONDO mirror
            // carrying `owl:imports`, and merging the mirrors means fetching
            // `MF.owl`, `ogms.owl` and two `MF/internal/*.owl` over the network.
            // Refusing them leaves `mirror/merged.owl` unbuildable and the whole
            // MFOMD closure missing from the import module.
            //
            // Still never SILENT: an import skipped without a word would leave the
            // reasoner and every QC check running over a smaller ontology than the
            // repo declares — the one difference no downstream check can detect. A
            // fetch is announced, and a fetch that fails is fatal.
            None => {
                let cached = fetch_import_iri(&iri, dir).with_context(|| {
                    format!(
                        "`owl:imports <{iri}>` has no entry in the plan's `catalog_file` and \
                         could not be fetched. Add it to catalog-v001.xml (and regenerate the \
                         plan if the repo has a Makefile)."
                    )
                })?;
                if seen.insert(cached.clone()) {
                    out.push(cached.clone());
                    out.extend(import_closure(&cached, dir, catalog, seen)?);
                }
            }
        }
    }
    Ok(out)
}

/// Drop `owl:imports` declarations (collapsed by the merge that read them).
fn drop_imports(model: crate::model::Model) -> crate::model::Model {
    use horned_owl::model::Component;
    crate::cmd::select::retain(model, |c| !matches!(c, Component::Import(_)))
}

/// Remove every axiom present in `other` from `model` (`unmerge`).
///
/// Defers to [`crate::cmd::unmerge::subtract`] so a plan step and `om unmerge`
/// cannot drift apart: this was a second implementation, and it both subtracted
/// the base's own ontology identity and matched on the bare component.
fn unmerge_model(model: crate::model::Model, other: &crate::model::Model) -> crate::model::Model {
    let rm: std::collections::HashSet<_> = other.ont.iter().cloned().collect();
    crate::cmd::unmerge::subtract(model, &rm).0
}

/// Merge a file's axioms into `model` (skipping the file's own ontology
/// identity/annotations, as a merge does for every secondary input).
pub(crate) fn merge_file_into(model: &mut crate::model::Model, path: &Path) -> Result<()> {
    merge_file_into_as(model, path, MergeRole::Input)
}

/// What a merged file IS to the target, which decides whether its blank-node
/// allocations move the target's base — see `cmd::merge::charge_import_allocations`.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum MergeRole {
    /// A secondary `--input`: parsed AFTER the primary, so it charges nothing.
    Input,
    /// A member of the import closure: parsed from the header, so it charges.
    Import,
}

pub(crate) fn merge_file_into_as(
    model: &mut crate::model::Model,
    path: &Path,
    role: MergeRole,
) -> Result<()> {
    use horned_owl::model::{Component, MutableOntology};
    // A merge prerequisite may be a *stamp* — an empty marker `touch`ed by a
    // rule whose real outputs are written elsewhere (UBERON's `collected-%.owl`
    // merges `$^`, which includes the 0-byte `tmp/bridges` stamp). It carries no
    // axioms, so skip it rather than failing to determine its format.
    if crate::io::is_empty_ontology_file(path) {
        return Ok(());
    }
    let other = crate::io::load(path)?;
    for ac in other.ont.iter() {
        if matches!(
            ac.component,
            Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_)
        ) {
            continue;
        }
        model.ont.insert(ac.clone());
    }
    for (prefix, value) in other.prefixes.mappings() {
        let _ = model.prefixes.add_prefix(prefix, value);
    }
    // RDF/XML inputs surface their `xmlns:` bindings in `idspaces`, not the formal
    // prefix map — carry those so a component's non-OBO prefix (e.g. the
    // `CCN20230722` brain-atlas taxonomy, `swrl`) reaches the OBO `idspace:` header.
    for (prefix, ns) in &other.idspaces {
        let _ = model.prefixes.add_prefix(prefix, ns);
    }
    crate::cmd::merge::carry_shared_anon(model, &other);
    if role == MergeRole::Import {
        crate::cmd::merge::charge_import_allocations(model, &other);
    }
    Ok(())
}

/// Resolve a recipe input token to a concrete file, building it from its rule
/// if it is a (convert-only) intermediate such as `EDIT_PREPROCESSED`.
fn resolve_input(
    repo: &Repo,
    input: Option<&str>,
    out_dir: &Path,
    work: &Path,
    target: &str,
) -> Result<PathBuf> {
    let inp = input.ok_or_else(|| anyhow::anyhow!("recipe has no input ($<)"))?;
    // A real file in the ontology dir WINS over the OFN cache. The cache is more
    // faithful than an RDF/XML round trip — that is exactly the problem. Each
    // recipe runs as a separate process reading the `.owl` from disk, so the next
    // step sees only what RDF/XML preserved; handing it the richer cached model
    // instead changes the result. `subsets/mondo-rare.owl`, whose recipe
    // reads `mondo-base.owl`, is the case: via the cache the model carries 32 extra
    // axioms and mondo-base's `shared_anon` (9,780 entries), which shifts blank-node
    // numbering and displaces eight `<owl:Axiom>` reification blocks. Re-parsing
    // `mondo-base.owl` from disk is what makes the artefact right.
    // An input that NAMES an `.ofn` (MONDO's `tmp/mondo.owl.ofn`) still resolves to
    // the cache below — there the OFN is the recipe's own declared intermediate.
    // …and an `.ofn` the plan builds as an ARTEFACT of its own wins too. MONDO's
    // `tmp/mondo.owl.ofn` is a co-target of the `mondo.owl` rule but has its own
    // entry, and its recipe carries `convert --add-prefixes config/prefixes.jsonld`
    // — so the file on disk holds 35 explicit prefixes. The cache below is keyed by
    // BASENAME, and `mondo.owl`'s own cache is written as `mondo.owl.ofn`: exactly
    // this input's filename. `mondo.obo` would otherwise read mondo.owl's cache
    // instead, lose the `#explicit-prefixes` marker, and ship 6 `idspace:` lines
    // where the release carries 35 — 159,626 differing lines.
    if !inp.ends_with(".ofn") || is_planned_target(inp) {
        let direct = repo.dir.join(inp);
        if direct.exists() {
            return Ok(direct);
        }
    }
    // Otherwise prefer a cached OFN of a previously-built artefact (avoids the
    // RDF/XML reader). The cache name is `<full-target-name>.ofn` (see the write in
    // `run_artefact`); an input that is itself an `.ofn` (e.g. `tmp/mondo.owl.ofn`,
    // mondo.owl's serialised form) maps to `<its-basename>` — i.e. mondo.owl's
    // `mondo.owl.ofn` cache — rather than a doubled `.ofn.ofn`.
    if let Some(name) = Path::new(inp).file_name() {
        let name = name.to_string_lossy();
        let cache_name = if name.ends_with(".ofn") {
            name.to_string()
        } else {
            format!("{name}.ofn")
        };
        let cache = work.join(cache_name);
        if cache.exists() {
            return Ok(cache);
        }
    }
    let cand_dir = repo.dir.join(inp);
    if cand_dir.exists() {
        return Ok(cand_dir);
    }
    // An artefact built into the output dir (e.g. <id>.owl ⟵ <id>-full.owl).
    if let Some(b) = Path::new(inp).file_name().map(|f| out_dir.join(f)) {
        if b.exists() {
            return Ok(b);
        }
    }
    // A planned intermediate (e.g. tmp/<id>-preprocess.owl): build it from the
    // plan's entry. Anything but a pure `convert` has to run its own steps —
    // converting the input verbatim writes a file that is wrong AND newer than
    // its input, so the target's real step later finds it up to date and skips
    // it. `tmp/merged-hp-edit.ofn` is `remove --select imports` + `merge`: taken
    // as a convert it would keep the two `owl:imports` the recipe exists to drop,
    // and an import-bearing ontology is exactly the case where a referenced entity
    // is left to its import to declare — so the file would lose the 2192
    // declarations the merged one carries, leaving every seed and artefact drawn
    // from it short by the imported properties.
    // A recipe whose input IS its own target names a file the recipe itself
    // creates, not a prerequisite: EFO's
    //
    // ```text
    // tmp/efo-master.owl:
    //     git show master:src/ontology/efo-edit.owl > $@
    //     robot --catalog catalog-v001.xml merge -i $@ -o $@.owl && mv $@.owl $@
    // ```
    //
    // has no prerequisites at all — the first line writes `$@` and the second
    // reads it back. Treating `-i $@` as something to build re-enters the rule
    // that is already running, and because this path bypasses the run-wide memo
    // in `ensure_prerequisite` the recursion is unbounded: the process dies of a
    // stack overflow rather than reporting anything.
    //
    // The file is absent at this point only because the step that writes it has
    // not run yet, so there is nothing to resolve and the caller's own steps must
    // produce it.
    if inp == target {
        bail!(
            "`{target}` names itself as its recipe input, and the step that writes it has not run yet"
        );
    }
    if let Some(planned) = repo.target(inp) {
        // An `--assume-new` input is treated as just modified and is never
        // rebuilt; a pattern-chain intermediate the target does not need stays
        // uncreated. Either way the recipe runs without the file.
        if assumed_new(repo, inp) || skip_missing_intermediate(repo, target, inp) {
            bail!("input `{inp}` is not built for this run (assume-new or unneeded intermediate)");
        }
        let pure_convert = planned
            .steps
            .iter()
            .all(|s| matches!(s, Step::Op(Op::Convert { .. })));
        if !pure_convert {
            let catalog = load_catalog(&repo.dir);
            build_prerequisite(repo, planned, &catalog, work, out_dir)
                .with_context(|| format!("building pipeline input `{inp}`"))?;
            if cand_dir.exists() {
                return Ok(cand_dir);
            }
        }
        if let Some(src) = planned.input.as_ref() {
            let src_path = repo.dir.join(src);
            if src_path.exists() {
                let out = cand_dir;
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                // Convert preserves content; format from the rule's --format or
                // the output extension. We use OFN for fidelity/speed.
                let tmp_ofn = out.with_extension("ofn");
                cmd::convert::run(cmd::convert::Args {
                    input: Some(src_path),
                    output: Some(tmp_ofn.clone()),
                    format: Some("ofn".into()),
                    check: None,
                    clean_obo: None,
                    common: Default::default(),
                })?;
                return Ok(tmp_ofn);
            }
        }
    }
    // A mirror carries no plan rule of its own — the import's `source` and
    // `mirror_steps` ARE the mirror — so it resolves through the mirror
    // machinery, which fetches an absent one or refuses under an explicit
    // `MIR=false`, exactly as it does for the import pipeline.
    if let Some(imp) = mirror_import_for(repo, inp) {
        return ensure_mirror(repo, imp, repo.refresh_mirrors);
    }
    bail!(
        "could not resolve pipeline input `{inp}` (no file in {} or {}, and no buildable rule)",
        repo.dir.display(),
        out_dir.display()
    )
}

/// The edit ontology file (`<id>-edit.<fmt>`), used to recover the import
/// closure when the pipeline input is a preprocessed copy.
/// The plan names the edit ontology or there is none. Execution never tries
/// `<id>-edit.{obo,owl,ofn,ttl}` in extension order: that is a filename
/// convention, and resolving conventions is ingest's job.
///
/// `is_file`, not `exists`: a plan recording an empty path would otherwise
/// resolve to the ontology DIRECTORY, which exists, and hand `io::load` a
/// directory to parse.
fn edit_file(repo: &Repo) -> Option<PathBuf> {
    let rel = repo.plan.edit_file.as_deref()?;
    let p = repo.dir.join(rel);
    p.is_file().then_some(p)
}

/// As [`edit_file`], but a plan that names a file which is not there is an
/// error rather than a silent `None`.
fn require_edit_file(repo: &Repo) -> Result<PathBuf> {
    match repo.plan.edit_file.as_deref() {
        None => bail!("this plan names no edit ontology (`edit_file`)"),
        Some(rel) => {
            let p = repo.dir.join(rel);
            if p.is_file() {
                Ok(p)
            } else {
                bail!(
                    "the plan's `edit_file: {rel}` is not a file ({}); execution never falls back to `<id>-edit.*`",
                    p.display()
                )
            }
        }
    }
}

/// The `$(OTHER_SRC)` components the plan declares, as concrete paths.
///
/// A declared component that is absent is BUILT when the plan knows how, and is
/// an error by name when it does not. It is never silently skipped: filtering on
/// existence at the call sites would let a component the repo declared drop out
/// of a merge, and the artefact would come out quietly short.
///
/// `skip` names components this caller is itself producing — the pattern step
/// writes `$(PATTERNDIR)/definitions.owl`, which is also an `OTHER_SRC` entry, so
/// requiring it there would deadlock a first build.
fn other_src(repo: &Repo, skip: &[&str], work: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for other in repo.var("OTHER_SRC").split_whitespace() {
        if skip.iter().any(|s| *s == other) {
            continue;
        }
        let p = repo.dir.join(other);
        if p.exists() {
            out.push(p);
            continue;
        }
        if repo.target(other).is_some() {
            ensure_built(repo, other, 8)?;
            if p.exists() {
                out.push(p);
                continue;
            }
        }
        bail!(
            "component `{other}` is declared in OTHER_SRC but is absent and the plan has no rule to build it ({})",
            p.display()
        );
    }
    Ok(out)
}

/// Resolve `--term-file` arguments to concrete paths, building a generated
/// SPARQL seed (e.g. `simple_seed.txt`) from its rule when it isn't present.
fn resolve_term_files(repo: &Repo, files: &[String], work: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for f in files {
        let p = repo.dir.join(f);
        if p.exists() {
            out.push(p);
        } else if repo.target(f).is_some() {
            out.push(build_seed(repo, f, work)?);
        } else {
            bail!("term file `{f}` does not exist ({}) and has no rule to build it", p.display());
        }
    }
    Ok(out)
}

/// Build a generated term seed (the `SIMPLESEED`/`ONTOLOGYTERMS` pattern):
/// run every SPARQL query referenced by the seed's rule (and its query-bearing
/// prerequisites) over the merged source, union the results with any literal
/// IRIs the recipe appends (`echo "<iri>" >> $@`), and write one IRI per line.
fn build_seed(repo: &Repo, seed_rel: &str, work: &Path) -> Result<PathBuf> {
    use crate::sparql::Queryable;
    use std::collections::BTreeSet;

    let planned = repo.target(seed_rel).expect("checked by the caller");

    // The query input is the merged source (`SRCMERGED`): the edit ontology
    // (imports dropped) plus the components/patterns in `$(OTHER_SRC)`.
    let srcmerged = build_srcmerged(repo, work)?;
    let model = crate::io::load(&srcmerged)?;
    let q = Queryable::from_model(&model)?;

    // Gather query files from this target's recorded steps and those of any
    // planned prerequisite that runs one. The steps were expanded at ingest, so
    // the paths here are concrete.
    let mut query_files: Vec<PathBuf> = Vec::new();
    let mut scan = |a: &ArtefactPlan, query_files: &mut Vec<PathBuf>| {
        for qf in a.steps.iter().flat_map(step_query_files) {
            let p = repo.dir.join(&qf);
            if p.exists() && !query_files.contains(&p) {
                query_files.push(p);
            }
        }
    };
    scan(planned, &mut query_files);
    for prereq in &planned.needs {
        if let Some(pr) = repo.target(prereq) {
            scan(pr, &mut query_files);
        }
    }

    // Literal IRIs appended by the recipe (e.g. SubsetProperty/SynonymTypeProperty).
    let echoed: Vec<String> = planned
        .steps
        .iter()
        .flat_map(step_shell_text)
        .flat_map(|l| extract_echo_iris(&l))
        .collect();

    // A seed is DERIVED from what its rule runs. A rule with no recipe runs
    // nothing, and there is nothing to derive it from: ECTO's `tmp/mre_seed.txt:`
    // is an empty rule, which make treats as made without creating a file. Writing
    // an empty seed here would answer a missing term file with one that selects
    // nothing, filtering the artefact down to nothing instead of saying the file
    // is absent — a declared file that is missing is an error, not a filter.
    if query_files.is_empty() && echoed.is_empty() {
        bail!(
            "term file `{seed_rel}` does not exist and nothing derives it: its rule runs no \
             query and names no terms, so no file is produced"
        );
    }

    let mut terms: BTreeSet<String> = BTreeSet::new();
    for qf in &query_files {
        let sparql = std::fs::read_to_string(qf).with_context(|| format!("reading {}", qf.display()))?;
        let table = q.query_table(&sparql).with_context(|| format!("running {}", qf.display()))?;
        for row in &table.rows {
            for cell in row {
                let t = cell.trim().trim_start_matches('<').trim_end_matches('>').trim();
                if t.starts_with("http") {
                    terms.insert(t.to_string());
                }
            }
        }
    }
    for iri in echoed {
        terms.insert(iri);
    }

    let out = work.join(Path::new(seed_rel).file_name().unwrap_or(std::ffi::OsStr::new("seed.txt")));
    std::fs::write(&out, terms.into_iter().collect::<Vec<_>>().join("\n") + "\n")?;
    status!("import:   built seed {} ({} terms)", out.display(), std::fs::read_to_string(&out)?.lines().count());
    Ok(out)
}

/// Build the merged source used for seeding: the edit ontology plus
/// `$(OTHER_SRC)`. The OBO edit loader drops `import:` lines, so the result is
/// already import-free — the same ontology a `remove --select imports` + `merge`
/// produces.
fn build_srcmerged(repo: &Repo, work: &Path) -> Result<PathBuf> {
    let mut inputs = Vec::new();
    if let Some(src) = edit_file(repo) {
        inputs.push(src);
    }
    inputs.extend(other_src(repo, &[], work)?);
    let out = work.join("srcmerged.ofn");
    cmd::merge::run(cmd::merge::Args {
        inputs,
        input_globs: Vec::new(),
        output: Some(out.clone()),
        format: None,
        include_annotations: None,
        collapse_import_closure: None,
        annotate_defined_by: None,
        annotate_derived_from: None,
        common: Default::default(),
    })?;
    Ok(out)
}

/// The shell command text a recorded step carries, if any. The plan stores these
/// already expanded — every variable and automatic variable was resolved at
/// ingest — which is why the seed builder reads the plan and nothing else.
fn step_shell_text(step: &Step) -> Option<String> {
    match step {
        Step::Shell { command: c, .. } => Some(c.clone()),
        Step::OwlmakeCli { name, args } => Some(format!("{name} {}", args.join(" "))),
        _ => None,
    }
}

/// The SPARQL query files a recorded step runs: named outright by a mapped
/// `query` op, or spelled out in a shell step's command text.
fn step_query_files(step: &Step) -> Vec<String> {
    match step {
        Step::Op(Op::Query { updates, selects, constructs, .. })
        | Step::Partial { op: Op::Query { updates, selects, constructs, .. }, .. } => updates
            .iter()
            .cloned()
            .chain(selects.iter().map(|(q, _)| q.clone()))
            .chain(constructs.iter().map(|(q, _)| q.clone()))
            .collect(),
        other => step_shell_text(other).map(|l| extract_query_files(&l)).unwrap_or_default(),
    }
}

/// Extract `--query <file>` arguments from an (expanded) recipe line.
fn extract_query_files(line: &str) -> Vec<String> {
    let toks: Vec<&str> = line.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if (*t == "--query" || *t == "-q") && i + 1 < toks.len() {
            out.push(toks[i + 1].to_string());
        }
    }
    out
}

/// Extract IRIs from `echo "<iri>" >> ...` recipe lines.
fn extract_echo_iris(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let trimmed = line.trim_start();
    if trimmed.starts_with("echo ") {
        // Pull the first quoted or bare token that looks like an IRI.
        for piece in trimmed.split(['"', '\'', ' ']) {
            let p = piece.trim();
            if p.starts_with("http://") || p.starts_with("https://") {
                out.push(p.to_string());
            }
        }
    }
    out
}

fn read_terms(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text.lines().filter_map(crate::cmd::select::term_line).map(str::to_string).collect())
}

/// Parse `catalog-v001.xml`: import-IRI → local file path.
/// A short identity for the running `owlmake` binary: its path plus modification
/// time. Any recompile changes the mtime, so this distinguishes builds even when
/// the crate version string does not.
fn binary_stamp() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(&p).ok().map(|m| (p, m)))
        .and_then(|(p, m)| m.modified().ok().map(|t| format!("{}::{:?}", p.display(), t)))
        .unwrap_or_else(|| format!("owlmake {}", env!("CARGO_PKG_VERSION")))
}

/// Wipe the intermediate-cache directory when it was written by a different
/// `owlmake` binary. The `.ofn` caches under it are reused verbatim by
/// `resolve_input`, so a cache from an older binary would mask a later fix; keying
/// on the binary's identity forces such caches to be rebuilt exactly once.
fn invalidate_stale_intermediate_cache(tmp: &Path) {
    let stamp_file = tmp.join(".owlmake-binary-stamp");
    let current = binary_stamp();
    let matches = std::fs::read_to_string(&stamp_file).ok().as_deref() == Some(current.as_str());
    if matches {
        return;
    }
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(tmp);
    }
    if std::fs::create_dir_all(tmp).is_ok() {
        let _ = std::fs::write(&stamp_file, current);
    }
}

/// The `owl:imports` resolution map for this build.
///
/// The PLAN names the catalog file; this reads its contents. Naming it in the
/// plan makes this the only catalog reader on the build path — rather than a
/// hard-coded `catalog-v001.xml` with a fallback policy of its own — while
/// leaving the file itself where Protégé and curators maintain it: adding an
/// `owl:imports` and its catalog line is ordinary curation and must not require
/// regenerating the plan.
///
/// The plan's own import products are overlaid afterwards, so a seeded repo with
/// no catalog at all still resolves the modules it builds — but only where it
/// really builds them. Under a merged import (`merged_import`) the per-product
/// modules are not written at all, so their `output` paths name files that never
/// exist; overlaying those would answer `owl:imports <source>` with a missing
/// file and stop the build, where the source IRI's own resolution is the answer.
pub(crate) fn load_catalog_planned(repo: &Repo) -> BTreeMap<String, PathBuf> {
    let mut map = match repo.plan.catalog_file.as_deref() {
        Some(rel) => read_catalog(&repo.dir, &repo.dir.join(rel)),
        None => BTreeMap::new(),
    };
    if repo.plan.merged_import.is_none() {
        for imp in &repo.plan.imports {
            if !imp.output.is_empty() {
                map.entry(imp.source.clone()).or_insert_with(|| repo.dir.join(&imp.output));
            }
        }
    }
    map
}

pub(crate) fn load_catalog(dir: &Path) -> BTreeMap<String, PathBuf> {
    read_catalog(dir, &dir.join("catalog-v001.xml"))
}

fn read_catalog(dir: &Path, path: &Path) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            if let (Some(n0), Some(u0)) = (line.find("name=\""), line.find("uri=\"")) {
                let name = &line[n0 + 6..];
                let name = &name[..name.find('"').unwrap_or(0)];
                let uri = &line[u0 + 5..];
                let uri = &uri[..uri.find('"').unwrap_or(0)];
                if !name.is_empty() && !uri.is_empty() {
                    // Catalog `uri`s are URL-encoded per the XML-catalog spec
                    // (Protégé writes e.g. `Ontorat%20input/x.owl`); decode so the
                    // path matches what is actually on disk.
                    let cand = dir.join(percent_decode(uri));
                    // A messy catalog (Protégé's "Imports Wizard") can map the same
                    // IRI to several files; prefer one that actually exists on disk.
                    let keep = cand.exists()
                        || !map.get(name).map(|p: &PathBuf| p.exists()).unwrap_or(false);
                    if keep {
                        map.insert(name.to_string(), cand);
                    }
                }
            }
        }
    }
    map
}

/// Decode `%XX` percent-escapes in a catalog URI path (other bytes untouched).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve the import closure of `file` to local paths via the catalog.
fn import_closure(
    file: &Path,
    dir: &Path,
    catalog: &BTreeMap<String, PathBuf>,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![file.to_path_buf()];
    while let Some(f) = stack.pop() {
        let text = match std::fs::read_to_string(&f) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for iri in import_iris(&text) {
            let local = catalog.get(&iri).cloned();
            if let Some(p) = local {
                if seen.insert(p.clone()) && p.exists() {
                    out.push(p.clone());
                    stack.push(p);
                }
            }
        }
    }
    Ok(out)
}

/// Extract import IRIs from OBO (`import:`) or OWL/OFN (`owl:imports`/`Import(...)`).
fn import_iris(text: &str) -> Vec<String> {
    let mut v = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("import:") {
            v.push(rest.trim().to_string());
        } else if let Some(i) = t.find("owl:imports rdf:resource=\"") {
            let s = &t[i + 25..];
            if let Some(e) = s.find('"') {
                v.push(s[..e].to_string());
            }
        } else if let Some(i) = t.find("Import(<") {
            let s = &t[i + 8..];
            if let Some(e) = s.find('>') {
                v.push(s[..e].to_string());
            }
        }
    }
    v
}

#[allow(dead_code)]
fn default_local(iri: &str, dir: &Path) -> Option<PathBuf> {
    // e.g. http://purl.obolibrary.org/obo/oba/imports/merged_import.owl
    //   →  imports/merged_import.owl  under the ontology dir.
    iri.rsplit_once("/obo/").and_then(|(_, tail)| {
        tail.split_once('/').map(|(_, rel)| dir.join(rel))
    })
}

#[cfg(test)]
mod aggregate_tests {
    use super::*;
    use crate::plan::step::Op;

    fn target(needs: &[&str], steps: Vec<Step>) -> ArtefactPlan {
        ArtefactPlan {
            target: "qc".into(),
            input: None,
            needs: needs.iter().map(|s| s.to_string()).collect(),
            order_only: vec![],
            steps,
            gaps: vec![],
            missing_rule: false,
            side_effect_only: false,
            stdout_file: None,
            intermediate: false,
            branches: vec![],
        }
    }

    /// The predicate `run_target_recipe_inner` uses to decide keep-going. Kept in
    /// step with it by construction: if the rule there changes, this must too.
    fn is_aggregate(a: &ArtefactPlan) -> bool {
        !a.needs.is_empty()
            && a.steps.iter().all(|s| {
                matches!(s, Step::Inert(_))
                    || matches!(s, Step::File(op) if matches!(op, recipe::FileOp::Print { .. }))
            })
    }

    /// EFO's `qc: sparql_test all_reports label_synonym_dup_check
    /// check_mondo_obsoletes` — prerequisites, no recipe of its own.
    #[test]
    fn a_bare_roll_up_is_an_aggregate() {
        assert!(is_aggregate(&target(
            &["sparql_test", "all_reports", "label_synonym_dup_check"],
            vec![]
        )));
    }

    /// A `test:` roll-up ends with `echo "Finished running all tests
    /// successfully."`, which ingest records as a `FileOp::Print`. Requiring an
    /// EMPTY step list would exclude the very target keep-going exists for.
    #[test]
    fn a_roll_up_whose_only_step_is_a_banner_is_still_an_aggregate() {
        let banner = Step::File(recipe::FileOp::Print {
            message: "Finished running all tests successfully.".into(),
            dst: None,
            append: false,
            newline: true,
        });
        assert!(is_aggregate(&target(&["reason_test", "sparql_test"], vec![banner])));
    }

    /// A target that does real work is NOT an aggregate: there a later step
    /// depends on an earlier one having succeeded, so fail-fast is right.
    #[test]
    fn a_target_with_real_steps_keeps_fail_fast() {
        let work = Step::Op(Op::Relax { include_subclass_of: false });
        assert!(!is_aggregate(&target(&["a.owl"], vec![work])));
    }

    /// No prerequisites means nothing to roll up.
    #[test]
    fn a_leaf_target_is_not_an_aggregate() {
        assert!(!is_aggregate(&target(&[], vec![])));
    }
}

#[cfg(test)]
mod merged_import_iri_tests {
    use super::merged_import_iris;

    #[test]
    fn merged_import_iri_names_the_document_not_the_file() {
        let (iri, ver) = merged_import_iris(
            "http://purl.obolibrary.org/obo/efo.owl",
            "4.0.0",
            None,
            "src/ontology/imports/merged_import.owl.gz",
            "src/ontology",
        );
        assert_eq!(iri, "http://purl.obolibrary.org/obo/efo/imports/merged_import.owl");
        assert_eq!(ver, "http://purl.obolibrary.org/obo/efo/releases/4.0.0/imports/merged_import.owl");
        let (iri, _) = merged_import_iris(
            "http://purl.obolibrary.org/obo/efo.owl",
            "4.0.0",
            Some("http://www.ebi.ac.uk/efo/imports/merged_import.owl"),
            "src/ontology/imports/merged_import.owl.gz",
            "src/ontology",
        );
        assert_eq!(iri, "http://www.ebi.ac.uk/efo/imports/merged_import.owl");
        let (iri, _) = merged_import_iris("http://purl.obolibrary.org/obo/x.owl", "1", None, "imports/merged_import.owl", "");
        assert_eq!(iri, "http://purl.obolibrary.org/obo/x/imports/merged_import.owl");
    }
}
