//! Interpret an existing ODK ontology repository (its `<ont>-odk.yaml`, the
//! generated `Makefile`, and the project `<ont>.Makefile` override) and map the
//! release build onto owlmake operations.
//!
//! Ingest works on a repo *without modifying it*: it reads those files as they
//! stand, resolves the **effective** recipe for each requested release artefact
//! (the override is `include`d last, so its assignments and rules win),
//! translates each chained `robot` subcommand in a recipe into the equivalent
//! owlmake operation, and refuses — with a precise list — anything on the
//! release path it cannot map.
//!
//! A human-readable **plan** is always printed first, so the mapping can be
//! verified before anything runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub(crate) mod makefile;
pub(crate) mod planner;
pub mod builtin;
// Rewrites a repository's files on disk, and reads its XML catalog to do it.
#[cfg(not(target_arch = "wasm32"))]
pub mod update;
pub(crate) mod robot;
pub(crate) mod workflows;

// The executor lives in `crate::build`: a plan is executed the same way whatever
// produced it, so nothing about running a build belongs under ODK ingest.
pub use crate::spec::{save as save_spec, PlanFormat, PLAN_FILE, PLAN_FILE_JSON};


pub use crate::spec::OwlmakeSpec;

/// The subset of `<ont>-odk.yaml` we read. Unknown keys are ignored by serde.
#[derive(Debug, Default, serde::Deserialize)]
pub struct OdkYaml {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub reasoner: Option<String>,
    #[serde(default)]
    pub release_artefacts: Vec<String>,
    #[serde(default)]
    pub primary_release: Option<String>,
    #[serde(default)]
    pub export_formats: Vec<String>,
    #[serde(default)]
    pub import_group: Option<ImportGroup>,
    #[serde(default)]
    pub components: Option<ComponentGroup>,
    #[serde(default)]
    pub use_dosdps: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct ImportGroup {
    #[serde(default)]
    pub use_base_merging: bool,
    #[serde(default)]
    pub exclude_iri_patterns: Vec<String>,
    /// ODK `slme_individuals` (the `extract --individuals` mode): how individuals are
    /// handled in the merged import — `include` (default), `minimal`,
    /// `definitions`, or `exclude`. ECTO uses `exclude` (it is class-level).
    #[serde(default)]
    pub slme_individuals: Option<String>,
    #[serde(default)]
    pub products: Vec<ImportProduct>,
}

/// One ODK import product. Also carried in the build plan (`imports[].product`),
/// so a fresh-import run works from the plan rather than re-reading the yaml.
#[derive(
    Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ImportProduct {
    pub id: String,
    #[serde(default)]
    pub mirror_from: Option<String>,
    #[serde(default)]
    pub make_base: bool,
    #[serde(default)]
    pub use_base: bool,
    #[serde(default)]
    pub base_iris: Vec<String>,
    #[serde(default)]
    pub mirror_type: Option<String>,
    /// ODK `is_large_import`: skipped by the `refresh-imports-excluding-large`
    /// target so the (often huge) source need not be re-mirrored every time.
    #[serde(default, alias = "is_large_import")]
    pub is_large: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct ComponentGroup {
    #[serde(default)]
    pub products: Vec<ComponentProduct>,
}

#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct ComponentProduct {
    pub filename: String,
    #[serde(default)]
    pub use_template: bool,
}

/// A parsed ODK repository rooted at `src/ontology/`.
pub struct OdkRepo {
    /// Targets whose recipe has already run in THIS invocation.
    ///
    /// A target's recipe runs at most once per run however many goals reach it, and
    /// `OdkRepo` is the one value that lives for the whole run — a `build::Repo` is
    /// constructed afresh for every PHASE (`Repo::of_with` per call), so a memo held
    /// there would start empty each time and could not span the QC phase and the
    /// artefact phase. HPO's `test` phase builds `hp.owl` for its profile check and
    /// `test_obo` writes `hp.obo`; without a run-wide memo, rebuilding `hp.owl`
    /// afterwards makes it newer, the release rule re-makes `hp.obo`, and the build
    /// ships its own conversion instead of the product `test_obo` wrote.
    pub built: std::cell::RefCell<std::collections::HashSet<String>>,
    /// Targets whose build FAILED in this invocation.
    ///
    /// Distinct from "not built yet", and the distinction cannot be recovered
    /// from the filesystem — both are an absent file. Under `-k` a failure does
    /// not stop the run, so a later target naming the failed one as a
    /// prerequisite reaches its staleness test with that prerequisite missing;
    /// read as "not newer" it declares the target up to date and whatever is on
    /// disk survives. This is the record that lets the test tell the two apart.
    pub failed: std::cell::RefCell<std::collections::HashSet<String>>,
    /// The `src/ontology` directory.
    pub dir: PathBuf,
    /// The repository root — where `owlmake.json` lives (the nearest `.git`
    /// ancestor, or the parent of `src/ontology`, or `dir` as a last resort).
    pub root: PathBuf,
    pub yaml: OdkYaml,
    pub make: makefile::MakeModel,
    /// When the repo has no Makefile (or ODK config) but does ship an edit
    /// ontology, this holds its filename (relative to `dir`). The plan is then
    /// the canonical stock release applied to that edit file.
    pub edit_file: Option<String>,
    /// True when `edit_file` was *discovered* (a plain `<id>.owl`/`.obo` in a
    /// non-ODK repo) rather than a real `<id>-edit.*`: the plan is the canonical
    /// stock release scaffolded for this ontology.
    pub seeded: bool,
    /// The committed `owlmake.yaml`, when the repository is built from one. Its
    /// targets, imports and switches are threaded into the rules
    /// ([`OwnRules::Stated`]) and the rest of it laid over the plan
    /// ([`OwlmakeSpec::overlay`]); the file is read and never rewritten.
    pub spec: Option<OwlmakeSpec>,
    /// The standard build's configuration, when the rules come from owlmake's
    /// built-in rules for it rather than from a build file: a repository loaded
    /// through [`OdkRepo::load_with_builtin_rules`], or from an `owlmake.yaml`
    /// with the standard build. What [`OdkRepo::configuration_under`] resolves
    /// again under another switch value. `None` for a file that says
    /// `standard_build: false`.
    pub builtin: Option<builtin::Config>,
    /// The command-line assignments that SURVIVED the conditional filter above,
    /// kept so the same configuration can be resolved again under another value
    /// of a switch (see [`OdkRepo::configuration_under`]) and the two models
    /// differ by the switch alone.
    pub seeded_vars: Vec<(String, String)>,
}

/// Resolve a repository's build configuration: the main file, then the override
/// file it includes last, whose assignments and rules therefore win.
///
/// `flags` binds switches to a chosen value before the parse, because the
/// conditionals are evaluated as the file is read and so decide which rules the
/// model even has. It is how the SAME configuration is resolved a second time
/// under the other value of a switch, which is what lets a target carry the
/// recipe from the branch this resolution did not take.
fn parse_configuration(
    main_mk: &Path,
    dir: &Path,
    seeded: &[(String, String)],
    flags: &[(&str, &str)],
) -> Result<makefile::MakeModel> {
    let mut make = makefile::MakeModel::parse_file_with_flags(main_mk, seeded, flags)?;
    // A recipe's shell — including the `$(shell …)`/backtick substitutions
    // evaluated for it — runs with the configuration's own directory as its
    // working directory; record it so relative paths resolve there rather than
    // against the process cwd.
    make.base_dir = Some(dir.to_path_buf());
    let make_id = make.expand("$(ONT)").trim().to_string();
    if !make_id.is_empty() {
        let override_mk = dir.join(format!("{make_id}.Makefile"));
        if override_mk.exists() {
            make.overlay_file(&override_mk)?;
        }
    }
    // The release version is a run input too, so the configuration resolves it to
    // a reference and records what it defaults to.
    make.bind_release_version();
    Ok(make)
}

/// Resolve a repository's build configuration from the rules owlmake supplies:
/// the standard build its configuration implies — or nothing, for a repository
/// whose build is all its own — then the rules the repository wrote itself,
/// which win. The same two layers, in the same order, as [`parse_configuration`]
/// reads from files.
fn configuration(
    standard: Option<&builtin::Config>,
    dir: &Path,
    seeded: &[(String, String)],
    flags: &[(&str, &str)],
    own: OwnRules,
) -> Result<makefile::MakeModel> {
    let mut make = match standard {
        Some(config) => builtin::model(config, dir, seeded, flags)?,
        None => makefile::MakeModel::with_flags(dir, seeded, flags),
    };
    match own {
        OwnRules::None => {}
        OwnRules::File => {
            if let Some(config) = standard {
                let own_rules = dir.join(format!("{}.Makefile", config.id));
                if own_rules.exists() {
                    make.overlay_file(&own_rules)?;
                }
            }
        }
        // A resolved target has no recipe to read — the planner takes its steps as
        // recorded — but the graph still has to know the target and what it needs.
        OwnRules::Stated(spec) => {
            make.phony.extend(spec.phony.iter().cloned());
            // The repository's own switches take the values it gives them, as its
            // own assignments did; what the run bound still wins.
            for (name, value) in &spec.gating_flags {
                if !make.command_line_vars.contains(name) {
                    make.vars.insert(name.clone(), value.clone());
                }
                let value = make.expand(&format!("$({name})")).trim().to_string();
                make.cond_vars.entry(name.clone()).or_insert(value);
            }
            if !spec.transient_targets.is_empty() {
                make.add_rule(makefile::Rule {
                    targets: vec![".INTERMEDIATE".to_string()],
                    prereqs: spec.transient_targets.clone(),
                    order_only: Vec::new(),
                    recipe: Vec::new(),
                    guards: Vec::new(),
                });
            }
            for t in &spec.targets {
                // A target that exists only under a switch makes the switch one
                // of the build's, as the conditional around its rule did.
                for flag in &t.when {
                    make.switch_vars.insert(flag.clone());
                }
                let needs: Vec<String> =
                    t.needs.iter().filter(|n| !t.order_only.contains(n)).cloned().collect();
                let rule = makefile::Rule {
                    targets: vec![t.target.clone()],
                    prereqs: needs,
                    order_only: t.order_only.clone(),
                    recipe: Vec::new(),
                    guards: t.when.clone(),
                };
                if t.extends {
                    // Its prerequisites join the standard target's.
                    make.add_rule(rule);
                } else {
                    // The whole of how the target is built, prerequisites and all.
                    make.rules.insert(t.target.clone(), rule);
                }
            }
        }
    }
    make.bind_release_version();
    Ok(make)
}

/// Where the rules a repository wrote itself come from.
enum OwnRules<'a> {
    /// Nowhere: the standard build alone.
    None,
    /// Its `<id>.Makefile`.
    File,
    /// What its `owlmake.yaml` states: its targets and phony names, the values
    /// of its own switches, and which of its targets are intermediates.
    Stated(&'a OwlmakeSpec),
}

impl OdkRepo {
    /// [`load`](Self::load), with the standard build taken from owlmake's built-in
    /// rules rather than read from a generated Makefile. `path` is the repository
    /// root or its ontology directory.
    pub fn load_with_builtin_rules(path: &Path) -> Result<OdkRepo> {
        Self::load_builtin(path, true)
    }

    /// As [`load_with_builtin_rules`](Self::load_with_builtin_rules), with or
    /// without the rules the repository wrote itself.
    fn load_builtin(path: &Path, own_rules: bool) -> Result<OdkRepo> {
        let dir = resolve_ontology_dir(path)?;
        let yaml_path = find_odk_yaml(&dir)?;
        let text = std::fs::read_to_string(&yaml_path)?;
        let config = builtin::Config::parse_odk(&text)
            .with_context(|| format!("reading {}", yaml_path.display()))?;
        let own = if own_rules { OwnRules::File } else { OwnRules::None };
        let make = configuration(Some(&config), &dir, &[], &[], own)?;
        let mut yaml: OdkYaml = serde_yaml::from_str(&text)?;
        if yaml.release_artefacts.is_empty() {
            yaml.release_artefacts = vec!["base".to_string(), "full".to_string()];
        }
        let root = repo_root(&dir);
        Ok(OdkRepo {
            built: Default::default(),
            failed: Default::default(),
            dir,
            root,
            yaml,
            make,
            edit_file: None,
            seeded: false,
            seeded_vars: Vec::new(),
            spec: None,
            builtin: Some(config),
        })
    }

    /// Locate and load an ODK repo from a path that is either the repo root,
    /// the `src/ontology` directory, or the `-odk.yaml` file itself.
    pub fn load(path: &Path) -> Result<OdkRepo> {
        Self::load_with_vars(path, &[])
    }

    /// [`load`] with command-line variable assignments, seeded before the Makefile
    /// is parsed so its `ifeq`/`ifneq` conditionals see them.
    pub fn load_with_vars(path: &Path, overrides: &[(String, String)]) -> Result<OdkRepo> {
        // Precedence:
        //  1. a real ODK config (`Makefile` / `<id>-odk.yaml`) — regenerate the
        //     plan from it (`owlmake.json` is an output);
        //  2. else a committed `owlmake.json` — the owlmake-native repo, built
        //     straight from that spec as the source of truth;
        //  3. else a bare edit/seed ontology — synthesize the stock release.
        // So `owlmake.json` beats the edit/seed fallback (a `seed`ed repo keeps
        // its hand-authored plan) but never overrides a real ODK build.
        let resolved = resolve_ontology_dir(path);
        let has_odk_config = resolved
            .as_ref()
            .ok()
            .map(|d| d.join("Makefile").exists() || has_odk_yaml(d))
            .unwrap_or(false);
        if !has_odk_config {
            if let Some(spec_path) = find_owlmake_json(path)? {
                return Self::load_from_spec(&spec_path);
            }
        }
        let dir = resolved?;

        let main_mk = dir.join("Makefile");
        if !main_mk.exists() {
            // No Makefile: fall back to a stock release over the edit file, if
            // one is present (many repos commit only `<id>-edit.<fmt>`). The id is
            // taken from the edit filename; the release plan is synthesized in
            // `planner::build`.
            if let Some(edit) = find_edit_file(&dir) {
                // Prefer the repo's OWN `<id>-odk.yaml` when it has one, and
                // synthesize a stock config from the edit FILENAME only when
                // there is none. A repo can ship an ODK yaml without a Makefile,
                // and its declared components, imports and patterns have to reach
                // the plan rather than being dropped for a bare release.
                let yaml = find_odk_yaml(&dir)
                    .ok()
                    .and_then(|p| std::fs::read_to_string(&p).ok())
                    .and_then(|t| serde_yaml::from_str::<OdkYaml>(&t).ok())
                    .unwrap_or_else(|| OdkYaml {
                        id: edit_id(&edit),
                        title: None,
                        reasoner: Some("ELK".into()),
                        release_artefacts: vec!["base".to_string(), "full".to_string()],
                        primary_release: None,
                        export_formats: vec!["owl".into(), "obo".into(), "json".into()],
                        import_group: None,
                        components: None,
                        use_dosdps: false,
                    });
                let root = repo_root(&dir);
                return Ok(OdkRepo {
                    built: Default::default(),
                    failed: Default::default(),
                    dir,
                    root,
                    yaml,
                    make: makefile::MakeModel::default(),
                    edit_file: Some(edit),
                    seeded: false,
                    seeded_vars: Vec::new(),
                    spec: None,
                    builtin: None,
                });
            }
            // No edit file either: a non-ODK repo that just ships its ontology as
            // a plain `<id>.owl`/`.obo` (Protégé/OnToology projects). Treat that
            // file as the seed and synthesize the canonical stock release over
            // it, so the ontology builds without any build files of its own.
            if let Some((id, seed)) = find_seed_file(&dir) {
                status!(
                    "plan: no ODK layout; seeding a stock release from `{seed}` \
                     (as `odk seed` would for a new `{id}` setup)"
                );
                let yaml = OdkYaml {
                    id,
                    title: None,
                    reasoner: Some("ELK".into()),
                    release_artefacts: vec!["base".to_string(), "full".to_string()],
                    primary_release: None,
                    export_formats: vec!["owl".into(), "obo".into(), "json".into()],
                    import_group: None,
                    components: None,
                    use_dosdps: false,
                };
                let root = repo_root(&dir);
                return Ok(OdkRepo {
                    built: Default::default(),
                    failed: Default::default(),
                    dir,
                    root,
                    yaml,
                    make: makefile::MakeModel::default(),
                    edit_file: Some(seed),
                    seeded: true,
                    seeded_vars: Vec::new(),
                    spec: None,
                    builtin: None,
                });
            }
            bail!(
                "no Makefile and no `<id>-edit.{{obo,owl,ofn}}` in {}",
                dir.display()
            );
        }
        // An assignment the configuration's CONDITIONALS consult decides which
        // rules exist, so it cannot come from the invocation: the plan describes
        // the repository, not one caller's switches, and `om make BRI=false`
        // would otherwise generate — and `--regenerate` commit — a plan with the
        // whole bridge section deleted. Such a name is dropped and the
        // configuration resolved again, because which names are conditional is
        // only known once it has been read. The list of dropped names strictly
        // grows, so this settles.
        let mut seeded: Vec<(String, String)> = overrides.to_vec();
        let make = loop {
            let make = parse_configuration(&main_mk, &dir, &seeded, &[])?;
            let before = seeded.len();
            seeded.retain(|(k, _)| !make.cond_vars.contains_key(k));
            if seeded.len() == before {
                break make;
            }
        };

        // The ODK `<id>-odk.yaml` is the preferred config, but many repos (GO,
        // MONDO, …) commit only the generated Makefile. The ODK Makefile encodes
        // everything owlmake needs for a cached run — `$(ONT)`, `$(IMPORTS)`,
        // `$(COMPONENTS)`, `$(MAIN_PRODUCTS)`, `$(REASONER)` — so synthesize the
        // config from it when no yaml is present.
        let mut yaml: OdkYaml = match find_odk_yaml(&dir) {
            Ok(yaml_path) => match serde_yaml::from_str(&std::fs::read_to_string(&yaml_path)?) {
                Ok(y) => y,
                // A malformed `<id>-odk.yaml` (e.g. OGMS's duplicate `import_group`)
                // shouldn't block the build: the generated Makefile encodes the same
                // information, so fall back to synthesizing the config from it.
                Err(e) => {
                    status!(
                        "plan: {} is malformed ({e}); using the Makefile instead",
                        yaml_path.display()
                    );
                    synthesize_yaml_from_make(&make, &dir)
                }
            },
            Err(_) => synthesize_yaml_from_make(&make, &dir),
        };
        // An omitted `release_artefacts` key means `[base, full]`, and the plan
        // has to carry that default: the primary `<id>.owl` is built *from*
        // `<id>-full.owl` (and many repos — e.g. ECTO — override the
        // `<id>-full.owl` recipe itself), so without this the full artefact is
        // never a build target and the primary is produced from a missing input.
        if yaml.release_artefacts.is_empty() {
            yaml.release_artefacts = vec!["base".to_string(), "full".to_string()];
        }

        let root = repo_root(&dir);
        Ok(OdkRepo {
            built: Default::default(),
            failed: Default::default(),
            dir,
            root,
            yaml,
            make,
            edit_file: None,
            seeded: false,
            seeded_vars: seeded,
            spec: None,
            builtin: None,
        })
    }

    /// What the repository at `path` commits as its `owlmake.yaml`: its options,
    /// and what it builds beyond the standard build for them — see
    /// [`OdkRepo::plan_file`].
    pub fn spec_for(path: &Path) -> Result<OwlmakeSpec> {
        let repo = Self::load(path)?;
        let full = repo.plan(&[])?;
        repo.plan_file(&full)
    }

    /// A repository built from its `owlmake.yaml`, with `root` the directory the
    /// file lives in and `dir` the ontology directory. The one way a file is read.
    ///
    /// The file's options resolve to the standard build, or to nothing when it
    /// says `standard_build: false`; its targets, imports and switches are
    /// threaded into those rules, and the rest of what it states is laid over
    /// the plan by [`OdkRepo::plan`]. Every stated variable, and the edit file it
    /// names, is bound before the rules are built, so the rules read them too.
    pub fn from_file(spec: OwlmakeSpec, root: PathBuf, dir: PathBuf) -> Result<OdkRepo> {
        spec.check()?;
        let config = spec
            .standard_build
            .then(|| builtin::Config::from_options(spec.options()))
            .transpose()?;
        // An import stated its own way is one of the standard build's imports,
        // built differently: one the configuration does not list would be a module
        // nothing merges.
        if let Some(config) = &config {
            for import in &spec.imports {
                if !config.import_ids().contains(&import.id.as_str()) {
                    bail!(
                        "`imports` states `{}`, which `import_group` does not list: add it there, \
                         and state under `imports` only how it is built",
                        import.id
                    );
                }
            }
        }
        let mut seeded: Vec<(String, String)> =
            spec.variables.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Some(edit) = &spec.edit_file {
            seeded.push(("SRC".to_string(), edit.clone()));
        }
        let make = configuration(config.as_ref(), &dir, &seeded, &[], OwnRules::Stated(&spec))?;
        // The same options, as the parts of the planner that read the
        // configuration directly hold them.
        let mut yaml: OdkYaml = match &config {
            Some(_) => serde_json::from_value(spec.options())
                .context("reading the standard build's options")?,
            // A build of the repository's own lists every import itself; the
            // products enumerate them for the planner, which takes each import
            // as stated.
            None => {
                let products: Vec<ImportProduct> = spec
                    .imports
                    .iter()
                    .map(|i| {
                        i.product
                            .clone()
                            .unwrap_or_else(|| ImportProduct { id: i.id.clone(), ..Default::default() })
                    })
                    .collect();
                OdkYaml {
                    id: spec.id.clone(),
                    reasoner: Some(spec.reasoner.clone()),
                    import_group: (!products.is_empty() || spec.use_base_merging).then(|| ImportGroup {
                        use_base_merging: spec.use_base_merging,
                        exclude_iri_patterns: spec.exclude_iri_patterns.clone(),
                        slme_individuals: spec.slme_individuals.clone(),
                        products,
                    }),
                    ..Default::default()
                }
            }
        };
        if config.is_some() && yaml.release_artefacts.is_empty() {
            yaml.release_artefacts = vec!["base".to_string(), "full".to_string()];
        }
        Ok(OdkRepo {
            built: Default::default(),
            failed: Default::default(),
            dir,
            root,
            yaml,
            make,
            edit_file: None,
            seeded: false,
            seeded_vars: seeded,
            spec: Some(spec),
            builtin: config,
        })
    }

    fn load_from_spec(spec_path: &Path) -> Result<OdkRepo> {
        let parsed = crate::spec::load(spec_path)?;
        // `owlmake.yaml` is written at the repo ROOT, but every path inside it is
        // relative to the ONTOLOGY DIRECTORY — `src/ontology` in an ODK layout — so
        // the two are resolved separately. Taking the spec file's own directory for
        // both would make a plan-only build of an ODK repo look for `imports/…`,
        // `components/…` and the edit file at the root, find none of them, and
        // report the release uncovered.
        let root = spec_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let dir = {
            let nested = root.join("src/ontology");
            if nested.is_dir() { nested } else { root.clone() }
        };
        status!(
            "make: building from committed {} (no ODK layout found)",
            spec_path.display()
        );
        Self::from_file(parsed, root, dir)
    }

    /// The targets this repository's file states, taken as written in place of
    /// planning the target of that name.
    pub(crate) fn own_targets(&self) -> &[crate::spec::ArtefactSpec] {
        self.spec.as_ref().map(|s| s.targets.as_slice()).unwrap_or(&[])
    }

    /// The imports its file states, each taken as written.
    pub(crate) fn own_imports(&self) -> &[crate::spec::ImportSpec] {
        self.spec.as_ref().map(|s| s.imports.as_slice()).unwrap_or(&[])
    }

    /// The pattern products its file states.
    pub(crate) fn own_dosdp(&self) -> Option<&crate::spec::DosdpSpec> {
        self.spec.as_ref().and_then(|s| s.dosdp.as_ref())
    }

    /// A build of the repository's own: its file states everything and no
    /// standard build underlies it, so nothing is derived from a convention —
    /// not the edit file, not the catalog, not the pattern directory.
    pub(crate) fn own_build(&self) -> bool {
        self.spec.is_some() && self.builtin.is_none()
    }

    /// What this repository commits as its `owlmake.yaml`, given `full`, the plan
    /// its build configuration resolves to: its options, and what it builds
    /// beyond the standard build for them.
    ///
    /// The standard build underlies the file when the repository has a
    /// configuration and the build file generated from it says nothing the
    /// configuration does not, so that the standard build for those options IS its
    /// build. Otherwise — no configuration, or a build file generated by another
    /// release than the one the standard build follows — the file says
    /// `standard_build: false` and states the whole build, and why is reported.
    ///
    /// "Beyond" is measured, not declared: the build is planned from the base the
    /// file names alone, and what differs is stated ([`OwlmakeSpec::state_build`]);
    /// then the file as it stands is resolved and what that still does not derive
    /// is stated too ([`OwlmakeSpec::state_rest`]). That catches what reading the
    /// repository's own rules for targets would miss — a standard target that came
    /// out differently because the repository reassigned a variable it reads — and
    /// leaves out everything the file's own statements imply.
    pub fn plan_file(&self, full: &crate::plan::Plan) -> Result<OwlmakeSpec> {
        let standard = self.standard_build_underlies(full);
        let options = match &standard {
            Some(options) => options.clone(),
            None => {
                let mut only_id = serde_json::Map::new();
                only_id.insert("id".into(), full.id.clone().into());
                only_id
            }
        };
        let mut spec = OwlmakeSpec::for_options(options, standard.is_some())?;
        // The base the file names, with nothing else stated, and the build the
        // repository has: the standard build for its options plus the rules it
        // wrote itself, or the whole of what its build file resolves to.
        let resolve = |spec: &OwlmakeSpec| -> Result<crate::plan::Plan> {
            OdkRepo::from_file(spec.clone(), self.root.clone(), self.dir.clone())?.plan(&[])
        };
        let mine;
        let base;
        let mine = if standard.is_some() {
            mine = Self::load_builtin(&self.dir, true)?.plan(&[])?;
            base = resolve(&spec)?;
            &mine
        } else {
            base = crate::plan::Plan::default();
            full
        };
        spec.state_build(mine, &base);
        let derived = resolve(&spec)?;
        spec.state_rest(mine, &derived);
        Ok(spec)
    }

    /// Whether the standard build underlies this repository's file, and if so
    /// its options under owlmake's names. See [`OdkRepo::plan_file`].
    fn standard_build_underlies(
        &self,
        full: &crate::plan::Plan,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let Some(config_file) = self.config_file_name() else {
            status!("plan: recording the build as the repository's own: it has no configuration");
            return None;
        };
        if !self.dir.join("Makefile").is_file() {
            status!(
                "plan: recording the build as the repository's own: no build file was generated \
                 from {config_file}"
            );
            return None;
        }
        let standard = OdkRepo::load_with_builtin_rules(&self.dir)
            .and_then(|r| r.plan(&[]))
            .map(|plan| builtin::differences_from_generated(full, &plan));
        match standard {
            Ok(differences) if differences.is_empty() => {}
            Ok(differences) => {
                let named: Vec<&str> = differences
                    .iter()
                    .filter_map(|d| d.lines().next())
                    .take(5)
                    .collect();
                status!(
                    "plan: recording the build as the repository's own: the generated build file \
                     differs from the standard build for this configuration in {} place(s) — {}{}",
                    differences.len(),
                    named.join("; "),
                    if differences.len() > named.len() { "; …" } else { "" }
                );
                return None;
            }
            Err(e) => {
                status!("plan: recording the build as the repository's own: {e:#}");
                return None;
            }
        }
        let text = std::fs::read_to_string(self.dir.join(&config_file)).ok()?;
        let raw: serde_json::Value = serde_yaml::from_str(&text).ok()?;
        // The options under owlmake's names: what configures a tool owlmake does
        // not run is left out here, with a note.
        let stated = builtin::from_odk_options(raw, true);
        let mut options = serde_json::Map::new();
        for (key, value) in stated.as_object().into_iter().flatten() {
            // A key that is not an option never had any effect.
            if !builtin::is_option(key) {
                status!("plan: `{key}` is not an option of the standard build; left out");
                continue;
            }
            options.insert(key.clone(), value.clone());
        }
        Some(options)
    }

    /// Whether this repository is built from its committed `owlmake.yaml` rather
    /// than from a build configuration that is still in the tree. Such a file is
    /// read and never rewritten.
    pub fn built_from_file(&self) -> bool {
        self.spec.is_some()
    }

    /// This repository's configuration resolved under a different value of one
    /// switch.
    ///
    /// The plan records the rules of the branch that was taken; this is how it
    /// also records what the OTHER branch says, so a run that flips the switch
    /// has a recipe to run rather than only a target to pin. Returns `None` for a
    /// repository with no build configuration to re-read. A repository built from
    /// its file resolves the file again, which for a build of the repository's
    /// own yields the same targets under either value: the file already carries
    /// both branches, which is the point of writing them down.
    pub fn configuration_under(&self, flag: &str, value: &str) -> Option<makefile::MakeModel> {
        // A repository keeps its own rules in one place or the other: its
        // `owlmake.yaml` once it has one, its `<id>.Makefile` until then.
        if let Some(spec) = &self.spec {
            return configuration(
                self.builtin.as_ref(),
                &self.dir,
                &self.seeded_vars,
                &[(flag, value)],
                OwnRules::Stated(spec),
            )
            .ok();
        }
        if let Some(config) = &self.builtin {
            return configuration(Some(config), &self.dir, &self.seeded_vars, &[(flag, value)], OwnRules::File)
                .ok();
        }
        let main_mk = self.dir.join("Makefile");
        if !main_mk.exists() {
            return None;
        }
        parse_configuration(&main_mk, &self.dir, &self.seeded_vars, &[(flag, value)]).ok()
    }

    /// Build the release plan, mapping every requested artefact and import.
    /// `only` optionally restricts which release artefacts are on the path
    /// (matched by artefact name, e.g. `full`, or target filename, e.g.
    /// `oba.owl`); empty means every configured artefact.
    /// The file name of this repo's ODK configuration, when it has one.
    ///
    /// Ingest reads it; nothing downstream may. It is exposed so that a recipe
    /// condition ABOUT that file can be recognised and decided here, at plan time.
    pub(crate) fn config_file_name(&self) -> Option<String> {
        find_odk_yaml(&self.dir)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
    }

    pub fn plan(&self, only: &[String]) -> Result<crate::plan::Plan> {
        let mut plan = planner::build(self, only)?;
        // The rules have been resolved with the file's targets, imports and
        // switches in them; what the file states beyond those is laid over the
        // result.
        if let Some(spec) = &self.spec {
            spec.overlay(&mut plan);
        }
        Ok(plan)
    }
}

/// Scaffold a starter [`OwlmakeSpec`] for a new ontology `id`: the standard build
/// over `edit`, with its export formats and no SPARQL checks yet, so the result
/// is an immediately-buildable `owlmake.yaml` the author extends by stating
/// options. `dir` is the directory the plan will live in; the edit file need not
/// exist yet.
pub fn seed_spec(id: &str, edit: Option<&str>, _dir: &Path) -> Result<OwlmakeSpec> {
    let edit = edit.map(str::to_string).unwrap_or_else(|| format!("{id}-edit.obo"));
    let format = Path::new(&edit).extension().and_then(|e| e.to_str()).unwrap_or("owl").to_string();
    let mut options = serde_json::Map::new();
    options.insert("id".into(), id.into());
    if format != "owl" {
        options.insert("edit_format".into(), format.clone().into());
    }
    options.insert("export_formats".into(), serde_json::json!(["owl", "obo", "json"]));
    // A new ontology has no queries yet; the checks that need none still run.
    options.insert(
        "report".into(),
        serde_json::json!({ "custom_sparql_checks": [], "custom_sparql_exports": [] }),
    );
    let mut spec = OwlmakeSpec::for_options(options, true)?;
    // The standard build reads `<id>-edit.<format>`; an edit file named
    // otherwise is stated, and every rule reads it instead.
    if edit != format!("{id}-edit.{format}") {
        spec.edit_file = Some(edit);
    }
    Ok(spec)
}

/// Find a committed `owlmake.json` for `path`: the file itself if named so, else
/// `owlmake.json` in the directory or any ancestor (so it resolves from a
/// subdirectory): the walk climbs ancestors and stops at the first one holding a
/// plan file, which need not be the repo root.
fn find_owlmake_json(path: &Path) -> Result<Option<PathBuf>> {
    if path.is_file() {
        let name = path.file_name().unwrap_or_default();
        let is_plan = name == std::ffi::OsStr::new(crate::spec::PLAN_FILE)
            || name == std::ffi::OsStr::new(crate::spec::PLAN_FILE_JSON);
        return Ok(is_plan.then(|| path.to_path_buf()));
    }
    let start = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for ancestor in start.ancestors() {
        if let Some(found) = crate::spec::find_in(ancestor)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Where each `mirror/<id>.owl` actually comes from, read off the repo's own
/// fetch script(s).
///
/// A repo with no `<id>-odk.yaml` and no `mirror-<id>` rule still has to obtain
/// its mirrors somehow, and the convention is a shell script beside the Makefile
/// (EFO's `get_mirrors.sh`). Parsing it is the difference between knowing and
/// guessing: EFO takes HANCESTRO from `raw.githubusercontent.com/EBISPOT/hancestro`
/// and FBbt/OBI/GO from the OBO PURLs, and `$(OBOBASE)/<id>.owl` is right for only
/// some of them.
///
/// Recognises the two spellings a fetch script uses:
///
/// ```text
/// curl -L <url> > mirror/<id>.owl
/// wget -O mirror/<id>.owl <url>
/// ```
///
/// Active lines win. A COMMENTED-OUT fetch is taken only when nothing active
/// writes that mirror, because it is still the repo's own record of where the file
/// came from and it beats a synthesised PURL. EFO is exactly this: its MONDO line
///
/// ```text
/// #curl -L http://purl.obolibrary.org/obo/mondo-base.owl > mirror/mondo.owl
/// ```
///
/// is commented out, while the active MONDO line fetches the FULL product to a
/// different file (`mirror/mondo-owl.owl`). `mirror/mondo.owl` is therefore
/// mondo-BASE, and guessing `$(OBOBASE)/mondo.owl` for it would overwrite the mirror
/// with the full product — dragging MONDO's closure (OMO -> IAO -> BFO) into the BOT
/// module, and BFO's upper-level disjointness with it.
fn scan_mirror_scripts(dir: &Path) -> BTreeMap<String, String> {
    let mut active: BTreeMap<String, String> = BTreeMap::new();
    let mut commented: BTreeMap<String, String> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return active };
    let mut scripts: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sh"))
        .collect();
    scripts.sort();
    for script in scripts {
        let Ok(text) = std::fs::read_to_string(&script) else { continue };
        for line in text.lines() {
            let raw = line.trim();
            let is_comment = raw.starts_with('#');
            let line = raw.trim_start_matches('#').trim();
            // The mirror this line writes, and the URL it reads.
            let toks: Vec<&str> = line.split_whitespace().collect();
            let mirror = toks.iter().find_map(|t| {
                t.trim_start_matches('>')
                    .strip_prefix("mirror/")
                    .and_then(|m| m.strip_suffix(".owl"))
            });
            let Some(id) = mirror else { continue };
            let url = toks.iter().find(|t| t.starts_with("http://") || t.starts_with("https://"));
            let Some(url) = url else { continue };
            let into = if is_comment { &mut commented } else { &mut active };
            // First writer wins, matching the script's own execution order.
            into.entry(id.to_string()).or_insert_with(|| (*url).to_string());
        }
    }
    for (id, url) in commented {
        active.entry(id).or_insert(url);
    }
    active
}

/// Synthesize the [`OdkYaml`] config from the ODK-generated Makefile, for repos
/// that commit only the Makefile (no `<id>-odk.yaml`). Reads the standard ODK
/// variables: `$(ONT)` (id), `$(REASONER)`, `$(IMPORTS)` (import product ids),
/// `$(COMPONENTS)` (component basenames), and `$(MAIN_PRODUCTS)` (the release
/// artefacts, named `$(ONT)` and `$(ONT)-<suffix>`). Sufficient for a cached
/// run, where import modules are read in place rather than re-mirrored.
fn synthesize_yaml_from_make(make: &makefile::MakeModel, dir: &Path) -> OdkYaml {
    let script_mirrors = scan_mirror_scripts(dir);
    let words = |var: &str| -> Vec<String> {
        make.expand(var).split_whitespace().map(|s| s.to_string()).collect()
    };
    let id = make.expand("$(ONT)").trim().to_string();
    let reasoner = {
        let r = make.expand("$(REASONER)").trim().to_string();
        if r.is_empty() { None } else { Some(r) }
    };
    let products: Vec<ImportProduct> = words("$(IMPORTS)")
        .into_iter()
        .map(|id| {
            // A repo that defines its OWN `mirror-<id>` (or `mirror/<id>.owl`) rule
            // is a CUSTOM mirror: replay that recipe rather than guessing
            // `$(OBOBASE)/<id>.owl`. The guess is not merely redundant, it is wrong
            // — MONDO's `mirror-ncbitaxon` fetches `ncbitaxon/subsets/taxslim.owl`
            // and `mirror-chebi` a slim, and each recipe post-processes with
            // `remove --axioms external`. Downloading the full `ncbitaxon.owl`
            // instead is both a different ontology and gigabytes larger.
            let has_rule = make.rule_for(&format!("mirror-{id}")).is_some()
                || make.rule_for(&format!("mirror/{id}.owl")).is_some();
            if has_rule {
                return ImportProduct {
                    id,
                    mirror_type: Some("custom".to_string()),
                    ..Default::default()
                };
            }
            // No rule. This function runs only when there is no `<id>-odk.yaml`, so
            // nothing in the ODK config declares the mirror either — but the repo
            // usually still says where it comes from, in the fetch script the
            // Makefile's comments point at. Read it rather than guessing: EFO's
            // `get_mirrors.sh` names `raw.githubusercontent.com/EBISPOT/hancestro`
            // for HANCESTRO, which `$(OBOBASE)/hancestro.owl` does not resolve to.
            if let Some(url) = script_mirrors.get(&id) {
                return ImportProduct {
                    id,
                    mirror_from: Some(url.clone()),
                    ..Default::default()
                };
            }
            // Not in the script either: something outside the build put the file
            // there, so owlmake must not invent a URL and overwrite it. EFO's
            // MONDO line is commented out —
            //
            //     #curl -L http://purl.obolibrary.org/obo/mondo-base.owl > mirror/mondo.owl
            //
            // the BASE product, while the ACTIVE line fetches the full one to a
            // different file (`mirror/mondo-owl.owl`). Guessing
            // `$(OBOBASE)/mondo.owl` would not just pick the wrong URL, it
            // would OVERWRITE the mirror the repo supplied, dragging MONDO's
            // closure (OMO -> IAO -> BFO) into the BOT module and BFO's upper-level
            // disjointness with it. `custom` already means "obtained by a project
            // script owlmake cannot run — use the committed mirror, and report a
            // gap if it is absent", which is exactly this.
            ImportProduct { id, mirror_type: Some("custom".to_string()), ..Default::default() }
        })
        .collect();
    // MONDO-style "merged import" projects squash every source into a single
    // committed `imports/merged_import.owl` (the Makefile's `IMPORT_ROOTS` /
    // `IMPORT_FILES` point at `merged_import`, not per-source modules). Flag this
    // as base-merging so a cached run reads that one file rather than demanding a
    // module per `$(IMPORTS)` entry that the repo never ships.
    let use_base_merging = make.expand("$(IMPORT_ROOTS)").contains("merged_import")
        || make.expand("$(IMPORT_FILES)").contains("merged_import")
        || make.expand("$(IMPORT_OWL_FILES)").contains("merged_import");
    let components: Vec<ComponentProduct> = words("$(COMPONENTS)")
        .into_iter()
        .map(|c| ComponentProduct { filename: format!("{c}.owl"), use_template: false })
        .collect();
    // MAIN_PRODUCTS = `$(ONT) $(ONT)-base $(ONT)-simple …` → the release artefacts
    // are the `<suffix>`s; the bare `$(ONT)` is the primary (always a candidate).
    let prefix = format!("{id}-");
    let release_artefacts: Vec<String> = words("$(MAIN_PRODUCTS)")
        .into_iter()
        .filter_map(|p| p.strip_prefix(&prefix).map(|s| s.to_string()))
        .collect();

    OdkYaml {
        id,
        title: None,
        reasoner,
        release_artefacts,
        primary_release: None,
        export_formats: vec!["owl".into(), "obo".into(), "json".into()],
        import_group: Some(ImportGroup { products, use_base_merging, ..Default::default() }),
        components: (!components.is_empty()).then_some(ComponentGroup { products: components }),
        use_dosdps: false,
    }
}

/// Locate the repository root that hosts `owlmake.json`: the nearest ancestor of
/// the `src/ontology` directory containing a `.git`, else the grandparent when
/// the dir is the conventional `…/src/ontology`, else the dir itself.
fn repo_root(dir: &Path) -> PathBuf {
    // Resolve to an absolute path first so we can climb the *real* filesystem
    // tree even when `dir` is relative — e.g. `.` when run from inside
    // `src/ontology`, whose `ancestors()` would otherwise stop at `.` and never
    // see the repo root above it. This is what lets `owlmake` invoked from the
    // ontology directory put `owlmake.json` at the repo root.
    let abs = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    for anc in abs.ancestors() {
        if anc.join(".git").exists() {
            return anc.to_path_buf();
        }
    }
    // No `.git` (e.g. a tarball checkout): fall back to the standard ODK layout,
    // taking the repo root as the parent of `src/ontology`.
    if abs.ends_with("src/ontology") {
        if let Some(p) = abs.parent().and_then(Path::parent) {
            return p.to_path_buf();
        }
    }
    abs
}

/// Whether `path` (or its `src/ontology`) holds something owlmake can build — an ODK
/// Makefile/config, an edit ontology, or a seedable `<id>.owl`/`.obo`. Used so a bare
/// `om` outside such a directory can show help instead of erroring.
pub fn has_buildable_setup(path: &Path) -> bool {
    // A committed plan is buildable on its own — a plan-only repo has no ODK
    // layout and may have no discoverable edit file either.
    resolve_ontology_dir(path).is_ok() || matches!(find_owlmake_json(path), Ok(Some(_)))
}

fn resolve_ontology_dir(path: &Path) -> Result<PathBuf> {
    let p = path.to_path_buf();
    if p.is_file() {
        // e.g. .../src/ontology/foo-odk.yaml
        return Ok(p.parent().unwrap_or(Path::new(".")).to_path_buf());
    }
    // A directory: it might already be src/ontology, or a repo root. Accept it
    // if it has an ODK Makefile/config, or — for the stock-release fallback — an
    // edit ontology.
    //
    // `src/ontology` is tried FIRST, because a repo that has one builds from it
    // whatever else sits at the root. uPheno keeps its retired uPheno-1 build at
    // the repo root, and that Makefile names `hp-edit.owl`, `mp-hp-kboom.owl` and
    // a `mirror/` of its own — so taking the root here plans a build the repo
    // stopped running years ago, with none of the release artefacts in it.
    for cand in [p.join("src/ontology"), p.clone()] {
        if cand.join("Makefile").exists() || has_odk_yaml(&cand) || has_edit_file(&cand) {
            return Ok(cand);
        }
    }
    // Last resort: a non-ODK repo that ships only a plain `<id>.owl`/`.obo` —
    // seedable into a stock release (see `OdkRepo::load`).
    for cand in [p.join("src/ontology"), p.clone()] {
        if find_seed_file(&cand).is_some() {
            return Ok(cand);
        }
    }
    bail!(
        "could not find an ODK `src/ontology` directory (with a Makefile, \
         <id>-odk.yaml, or <id>-edit.* file) at or under {}",
        path.display()
    )
}

/// Whether the directory carries a real ODK config (`<id>-odk.yaml`). A repo
/// that has one follows the ODK's conventions, so owlmake's native QC checks
/// apply to it; one that does not spells its own QC and must be taken at its
/// word (see `cmd::make::classify`).
pub(crate) fn has_odk_yaml(dir: &Path) -> bool {
    find_odk_yaml(dir).is_ok()
}

/// The edit ontology in `dir` (`<id>-edit.obo`/`.owl`/`.ofn`), if any. Prefers
/// `.obo`, then `.owl`, then `.ofn`. Returns the filename relative to `dir`.
fn find_edit_file(dir: &Path) -> Option<String> {
    let mut found: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with("-edit.obo") || name.ends_with("-edit.owl") || name.ends_with("-edit.ofn") {
                found.push(name);
            }
        }
    }
    found.sort_by_key(|n| match () {
        _ if n.ends_with(".obo") => 0,
        _ if n.ends_with(".owl") => 1,
        _ => 2,
    });
    found.into_iter().next()
}

fn has_edit_file(dir: &Path) -> bool {
    find_edit_file(dir).is_some()
}

/// Discover a *seed* ontology in a non-ODK repo: an `<id>.owl`/`.obo` (or an
/// `<id>-edit`/`<id>_dev`/`<id>-src` working copy) named after the repo, possibly
/// in a common subdirectory (`src/`, `ontology/`, `current_release/`, …) and in
/// any letter-case. Returns `(id, path-relative-to-dir)`. A dev/edit/source copy
/// is preferred over the compiled product, `.owl` over `.obo`. `<id>-edit.*` at
/// the top level is handled separately upstream.
fn find_seed_file(dir: &Path) -> Option<(String, String)> {
    let id = dir.file_name()?.to_string_lossy().to_lowercase();
    if id.is_empty() {
        return None;
    }
    // Candidate basenames (lowercased), highest priority first. `<id>_owl`/
    // `<id>_obo` cover the SBO-style `SBO_OWL.owl` / `SBO_OBO.obo` convention.
    let mut cands: Vec<String> = Vec::new();
    for stem in [
        format!("{id}-edit"),
        format!("{id}-edits"),
        format!("{id}_edit"),
        format!("{id}_dev"),
        format!("{id}-dev"),
        format!("{id}-src"),
        id.clone(),
        format!("{id}_owl"),
        format!("{id}_obo"),
    ] {
        for ext in ["owl", "obo", "ofn"] {
            cands.push(format!("{stem}.{ext}"));
        }
    }
    let subdirs = ["", "src", "ontology", "src/ontology", "current_release"];
    // Index each candidate directory's files case-insensitively once.
    let present: Vec<(String, std::collections::HashMap<String, String>)> = subdirs
        .iter()
        .filter_map(|sub| {
            let d = if sub.is_empty() { dir.to_path_buf() } else { dir.join(sub) };
            let rd = std::fs::read_dir(&d).ok()?;
            let mut m = std::collections::HashMap::new();
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                let rel = if sub.is_empty() { n.clone() } else { format!("{sub}/{n}") };
                m.insert(n.to_lowercase(), rel);
            }
            Some((sub.to_string(), m))
        })
        .collect();
    // Prefer by filename priority across all directories.
    for c in &cands {
        for (_sub, files) in &present {
            if let Some(rel) = files.get(c) {
                return Some((id.clone(), rel.clone()));
            }
        }
    }
    // Bounded recursive search for an id-named file anywhere in the tree (e.g.
    // pbpko's `Robot/ontologies/pbpko.owl`), but only accept a candidate that
    // matches a *single* path — so an ambiguous repo (stato has several
    // `stato.owl` under dev/) declines rather than guessing.
    let tree = walk_ontology_files(dir, 5);
    for c in &cands {
        let mut hits = tree.iter().filter(|(name, _)| name == c);
        if let Some((_, rel)) = hits.next() {
            if hits.next().is_none() {
                return Some((id.clone(), rel.clone()));
            }
        }
    }
    // Fallback: a single dominant ontology file whose name doesn't match the id
    // (e.g. `xenopus_anatomy.owl` in the `xao` repo). Only fires when a directory
    // holds exactly one plausible ontology stem, after excluding imports,
    // components, base modules, mirrors and catalogs — so it can't pick a
    // component by mistake.
    for (_sub, files) in &present {
        if let Some(rel) = sole_ontology(files) {
            return Some((id.clone(), rel));
        }
    }
    // Content pass: identify by what each file *declares*. Read each ontology
    // file's own ontology id (OBO `ontology:` header / OWL ontology IRI) and pick
    // the one that declares this repo's id — e.g. `dicty_anatomy.obo` declares
    // `ontology: ddanat` among a dozen unrelated `dicty_*` siblings. This is
    // identification, not a filename guess.
    let ext_rank = |rel: &str| match rel.rsplit_once('.').map(|x| x.1) {
        Some("owl") => 0,
        Some("obo") => 1,
        _ => 2,
    };
    let mut declared: Vec<&String> = tree
        .iter()
        .filter(|(_, rel)| read_ontology_id(&dir.join(rel)).as_deref() == Some(id.as_str()))
        .map(|(_, rel)| rel)
        .collect();
    // Prefer `.owl`, then the shallowest path.
    declared.sort_by_key(|rel| (ext_rank(rel), rel.matches('/').count(), rel.len()));
    if let Some(rel) = declared.first() {
        return Some((id.clone(), (*rel).clone()));
    }
    None
}

/// Read a file's *declared* ontology id from its header: the OBO `ontology:`
/// stanza, or the OWL ontology IRI's last path segment (e.g.
/// `…/obo/ddanat.owl` → `ddanat`). Reads only the first 64 KiB — the header is at
/// the top — so it is cheap even for very large ontologies. Lower-cased.
fn read_ontology_id(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0u8; 64 * 1024];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let from_iri = |iri: &str| -> String {
        iri.trim_end_matches('>')
            .rsplit(['/', '#'])
            .next()
            .unwrap_or(iri)
            .trim_end_matches(".owl")
            .trim_end_matches(".obo")
            .to_lowercase()
    };
    for line in text.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("ontology:") {
            let v = v.trim();
            if !v.is_empty() {
                return Some(from_iri(v));
            }
        }
    }
    for marker in ["owl:Ontology rdf:about=\"", "Ontology(<"] {
        if let Some(i) = text.find(marker) {
            let s = &text[i + marker.len()..];
            if let Some(e) = s.find(['"', '>']) {
                let iri = &s[..e];
                if iri.contains("/obo/") || iri.contains("://") {
                    return Some(from_iri(iri));
                }
            }
        }
    }
    None
}

/// Collect ontology files (`.owl`/`.obo`/`.ofn`) under `root` to `max_depth`,
/// skipping VCS and import/module/mirror directories. Returns `(lowercased
/// basename, path-relative-to-root)` pairs.
fn walk_ontology_files(root: &Path, max_depth: usize) -> Vec<(String, String)> {
    fn skip_dir(name: &str) -> bool {
        matches!(
            name,
            ".git" | "imports" | "mirror" | "target" | "extracted_terms"
                | "modules" | "build" | "node_modules" | "tmp"
        )
    }
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if depth < max_depth && !skip_dir(&name) {
                    stack.push((path, depth + 1));
                }
            } else if matches!(
                path.extension().and_then(|x| x.to_str()),
                Some("owl") | Some("obo") | Some("ofn")
            ) {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push((name.to_lowercase(), rel.display().to_string()));
                }
            }
        }
    }
    out
}

/// If a directory (its lowercased `name → relpath` map) holds exactly one
/// plausible ontology — one stem, ignoring imports/components/base/mirror/etc. —
/// return its path (preferring `.owl`, then `.obo`, then `.ofn`).
fn sole_ontology(files: &std::collections::HashMap<String, String>) -> Option<String> {
    let is_noise = |stem: &str| {
        ["import", "module", "mirror", "catalog", "component", "obsolete", "annotations"]
            .iter()
            .any(|w| stem.contains(w))
            || stem.ends_with("-base")
            || stem.ends_with("_base")
    };
    // stem -> {ext -> relpath}
    let mut stems: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    for (name, rel) in files {
        let Some((stem, ext)) = name.rsplit_once('.') else { continue };
        if !matches!(ext, "owl" | "obo" | "ofn") || is_noise(stem) {
            continue;
        }
        stems.entry(stem.to_string()).or_default().insert(ext.to_string(), rel.clone());
    }
    if stems.is_empty() {
        return None;
    }
    let pick = |exts: &std::collections::HashMap<String, String>| -> Option<String> {
        for ext in ["owl", "obo", "ofn"] {
            if let Some(rel) = exts.get(ext) {
                return Some(rel.clone());
            }
        }
        None
    };
    if stems.len() == 1 {
        return pick(stems.values().next()?);
    }
    // More than one stem: only proceed if they are a single *version family* —
    // dated/versioned copies of one ontology (e.g. the rat-genome releases
    // `clinical_measurement_<date>_v2.273.obo`). They must all reduce to the same
    // base once version/date suffixes are stripped; then pick the newest by the
    // date embedded in the filename (tie-broken by version, then name).
    let bases: std::collections::HashSet<String> =
        stems.keys().map(|s| strip_version(s)).collect();
    if bases.len() != 1 {
        return None;
    }
    let best = stems
        .keys()
        .max_by_key(|s| (file_date(s), version_key(s), (*s).clone()))?
        .clone();
    pick(&stems[&best])
}

/// Strip trailing version/date tokens (`_v2.273`, `_20260425`, `_v2026-05-06`)
/// from a filename stem, leaving the ontology's base name.
fn strip_version(stem: &str) -> String {
    let mut s = stem.to_string();
    while let Some(pos) = s.rfind(['_', '-']) {
        let tail = &s[pos + 1..];
        let t = tail.strip_prefix('v').unwrap_or(tail);
        let numeric = !t.is_empty()
            && t.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-')
            && t.chars().any(|c| c.is_ascii_digit());
        if numeric {
            s.truncate(pos);
        } else {
            break;
        }
    }
    s
}

/// The release date embedded in a filename stem as `YYYYMMDD` (0 if none), from
/// either `YYYYMMDD` or `YYYY-MM-DD` forms.
fn file_date(stem: &str) -> u32 {
    let d: Vec<char> = stem.chars().collect();
    let mut best = 0u32;
    let digits = |s: &[char]| -> Option<u32> { s.iter().collect::<String>().parse().ok() };
    let mut i = 0;
    while i + 4 <= d.len() {
        if d[i..i + 4].iter().all(|c| c.is_ascii_digit()) {
            let y = digits(&d[i..i + 4]).unwrap_or(0);
            // optional '-' then MM, optional '-' then DD
            let mut j = i + 4;
            if d.get(j) == Some(&'-') { j += 1; }
            if j + 2 <= d.len() && d[j..j + 2].iter().all(|c| c.is_ascii_digit()) {
                let m = digits(&d[j..j + 2]).unwrap_or(0);
                let mut k = j + 2;
                if d.get(k) == Some(&'-') { k += 1; }
                if k + 2 <= d.len() && d[k..k + 2].iter().all(|c| c.is_ascii_digit()) {
                    let day = digits(&d[k..k + 2]).unwrap_or(0);
                    if (2000..=2100).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&day) {
                        best = best.max(y * 10000 + m * 100 + day);
                    }
                }
            }
        }
        i += 1;
    }
    best
}

/// A numeric version key (`v2.273` → `[2, 273]`) for tie-breaking same-date files.
fn version_key(stem: &str) -> Vec<u32> {
    if let Some(pos) = stem.rfind(['_', '-']) {
        let tail = stem[pos + 1..].strip_prefix('v').unwrap_or(&stem[pos + 1..]);
        if tail.contains('.') {
            return tail.split('.').filter_map(|p| p.parse().ok()).collect();
        }
    }
    Vec::new()
}

/// `<id>` from an edit filename (`apo-edit.obo` → `apo`).
fn edit_id(edit: &str) -> String {
    edit.rsplit_once("-edit.")
        .map(|(id, _)| id.to_string())
        .unwrap_or_else(|| edit.to_string())
}

fn find_odk_yaml(dir: &Path) -> Result<PathBuf> {
    let entries = std::fs::read_dir(dir).map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with("-odk.yaml") || name.ends_with("-odk.yml") {
            found.insert(name, e.path());
        }
    }
    found
        .into_values()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no <id>-odk.yaml found in {}", dir.display()))
}
