//! Derive a [`Plan`] from an ingested ODK repository.
//!
//! This is where a `<id>-odk.yaml`, the generated `Makefile` and the project
//! override become the plan: rules are resolved to their effective recipe,
//! prerequisites and `$(eval)` assignments are expanded, and each `$(ROBOT)`
//! invocation is mapped to a plan step. Everything make-shaped is resolved HERE,
//! at plan time, because the executor cannot read a Makefile.

use std::path::Path;

use anyhow::Result;

use crate::plan::step::{Condition, Op, Step};
use crate::plan::{ArtefactPlan, ImportPlan, Plan};

use super::makefile::{Autos, MakeModel};
use super::robot;
use super::workflows::Version;
use super::OdkRepo;

/// The newest artefact-format generation owlmake's version-gated output
/// behaviours know about, used when a repo pins none.
const CURRENT_ROBOT: Version = (1, 9, 10);

pub fn build(repo: &OdkRepo, only: &[String]) -> Result<Plan> {
    if repo.edit_file.is_some() {
        return Ok(build_edit_only(repo, only));
    }
    let make = &repo.make;
    let id = repo.yaml.id.clone();
    // Not the version itself — a REFERENCE to the plan's one `version` field, the
    // same thing `$(VERSION)` now expands to. The field carries the default; the
    // run supplies the release date and every string built here picks it up.
    let version = crate::plan::VERSION_REF;
    let ontbase = {
        let v = make.expand("$(ONTBASE)");
        if v.trim().is_empty() {
            format!("http://purl.obolibrary.org/obo/{id}")
        } else {
            v.trim().to_string()
        }
    };
    let reasoner = {
        let v = make.expand("$(REASONER)");
        if v.trim().is_empty() {
            repo.yaml.reasoner.clone().unwrap_or_else(|| "ELK".into())
        } else {
            v.trim().to_string()
        }
    };

    // --- Imports -----------------------------------------------------------
    let ig = repo.yaml.import_group.as_ref();
    let use_base_merging = ig.map(|g| g.use_base_merging).unwrap_or(false);
    let exclude_iri_patterns = ig.map(|g| g.exclude_iri_patterns.clone()).unwrap_or_default();
    let slme_individuals = ig.and_then(|g| g.slme_individuals.clone());
    // With base-merging, the committed `merged_import.owl` already contains every
    // product, so individual mirrors/modules need not be (re)built for a cached
    // run and their gaps don't block the release.
    let merged_cached = use_base_merging && repo.dir.join("imports/merged_import.owl").exists();
    let obobase = {
        let b = make.expand("$(OBOBASE)");
        if b.trim().is_empty() { "http://purl.obolibrary.org/obo".to_string() } else { b.trim().to_string() }
    };
    let mut imports = Vec::new();
    if let Some(g) = ig {
        for p in &g.products {
            let source = if p.mirror_type.as_deref() == Some("custom") {
                // A custom mirror usually has no single URL, but where its recipe
                // reads one directly the plan must name it — see
                // `mirror_input_iri`.
                mirror_input_iri(repo, &p.id)
                    .unwrap_or_else(|| "<custom mirror script>".to_string())
            } else if let Some(m) = &p.mirror_from {
                m.clone()
            } else if p.use_base {
                // `use_base` mirrors the upstream `-base` product directly (no local
                // base-reduction step needed).
                format!("{obobase}/{0}/{0}-base.owl", p.id)
            } else {
                format!("{obobase}/{}.owl", p.id)
            };
            let output = format!("imports/{}_import.owl", p.id);
            let cached = repo.dir.join(&output).exists() || merged_cached;
            let mut gaps = Vec::new();
            if p.mirror_type.as_deref() == Some("custom") && !cached {
                // A custom mirror is NOT beyond owlmake: `run_custom_mirror` replays
                // the project's own `mirror/<id>.owl` / `mirror-<id>` recipe with
                // `$(ROBOT)` rebound to owlmake, which is how MONDO's mirrors — a
                // curl plus `convert` and `remove --axioms external`, all of them
                // commands owlmake implements — get built. Report a gap only where
                // that would actually fail: nothing cached on disk AND no recipe to
                // replay. A gap on every custom mirror would refuse whole builds
                // over a recipe owlmake runs perfectly well.
                // Ask what the EXECUTOR asks: it runs `mirror_steps` if the plan
                // records any, and otherwise looks the mirror up as a planned
                // target — both of which come from the plan, never from the
                // Makefile. Testing `make.rule_for` here would wrongly report a gap
                // for a repo built from a committed `owlmake.yaml` with no Makefile
                // at all, which is a supported mode.
                let mirror_file = repo.dir.join(format!("mirror/{}.owl", p.id));
                let has_recipe = !mirror_steps(repo, &p.id).is_empty();
                if !mirror_file.exists() && !has_recipe {
                    gaps.push(format!(
                        "custom mirror for `{0}`: no cached imports/{0}_import.owl, no mirror/{0}.owl,                          and no `mirror/{0}.owl` or `mirror-{0}` rule to replay",
                        p.id
                    ));
                }
            }
            // Build the mirror→module pipeline. Prefer the repo's own Makefile
            // recipe (so per-import excludes / renames / extra seed terms are
            // captured faithfully); fall back to the canonical BOT/make-base
            // pipeline synthesized from the product's flags.
            let steps = import_pipeline(repo, p, &obobase);
            imports.push(ImportPlan {
                id: p.id.clone(),
                source,
                output,
                steps,
                // Filled in below, once `robot_prefix` is known.
                cached,
                gaps,
                product: Some(p.clone()),
                mirror_steps: mirror_steps(repo, &p.id),
                mirror_inputs: mirror_inputs(repo, &p.id),
            });
        }
    }
    let merged_import = use_base_merging.then(|| "imports/merged_import.owl".to_string());

    let components: Vec<String> = declared_components(repo);

    // --- Release artefacts -------------------------------------------------
    // Candidate targets: each release artefact `<id>-<art>.owl`, the primary
    // `<id>.owl`, and the configured export formats of the primary.
    let mut candidates: Vec<String> = Vec::new();
    for art in &repo.yaml.release_artefacts {
        // ODK names a `custom-<x>` artefact as `<x>.owl` (no ontology prefix);
        // ordinary artefacts are `<id>-<x>.owl`.
        candidates.push(match art.strip_prefix("custom-") {
            Some(x) => format!("{x}.owl"),
            None => format!("{id}-{art}.owl"),
        });
    }
    candidates.push(format!("{id}.owl"));
    for fmt in &repo.yaml.export_formats {
        let t = format!("{id}.{fmt}");
        if !candidates.contains(&t) {
            candidates.push(t);
        }
    }
    // …and every release artefact in every export format. ODK releases each
    // product in each configured format — MONDO's `MAIN_FILES` is
    // `$(foreach n,$(MAIN_PRODUCTS), $(n).owl $(n).obo $(n).json)` — so
    // `mondo-base.obo`, `mondo-simple.json`, … are release artefacts too. Each one
    // has to be a candidate: a format left out falls through to Makefile replay,
    // which rebuilds the prerequisite `.owl` with the plain writer and clobbers the
    // one the plan had just written.
    // A candidate with no Makefile rule is dropped below, so this only ever adds
    // targets the repo really builds.
    for art in &repo.yaml.release_artefacts {
        let base = match art.strip_prefix("custom-") {
            Some(x) => x.to_string(),
            None => format!("{id}-{art}"),
        };
        for fmt in &repo.yaml.export_formats {
            let t = format!("{base}.{fmt}");
            if !candidates.contains(&t) {
                candidates.push(t);
            }
        }
    }
    // …every entry of `$(MAIN_FILES)`, which is not always
    // `$(MAIN_PRODUCTS) × $(FORMATS)`: MONDO appends `$(ONT)_nodes.tsv
    // $(ONT)_edges.tsv` (its kgx graph export) by hand.
    for t in make
        .expand("$(MAIN_FILES)")
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>()
    {
        if !candidates.contains(&t) {
            candidates.push(t);
        }
    }

    // …and the RELEASE REPORTS. `$(ASSETS)` / `$(RELEASE_ASSETS)` are the canonical
    // ODK "what gets released" lists, but they also carry `$(IMPORT_FILES)`, which
    // the planner handles through its own import machinery — so take the report
    // variable directly. It expands to nothing in a repo that does not define it.
    for t in make
        .expand("$(REPORT_FILES_RELEASE)")
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>()
    {
        if !candidates.contains(&t) {
            candidates.push(t);
        }
    }

    // …and everything else the repo itself calls a release asset. `$(ASSETS)` is
    // the canonical ODK list, and repos put things there that no fixed variable
    // name would find: OBA releases `$(PATTERN_RELEASE_FILES)` (its DOSDP
    // `patterns/definitions.owl` and `patterns/pattern.owl`) and `$(REPORT_FILES)`
    // (`reports/oba.owl-obo-report.tsv`), neither of which is `MAIN_FILES`,
    // `SUBSET_FILES` or MONDO's `REPORT_FILES_RELEASE`. Reading `$(ASSETS)`
    // directly covers any repo's own list. `$(IMPORT_FILES)` is dropped — the
    // planner has its own import machinery for those.
    {
        let imports: Vec<String> = make
            .expand("$(IMPORT_FILES)")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        for t in make
            .expand("$(ASSETS)")
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
        {
            if !imports.contains(&t) && !candidates.contains(&t) {
                candidates.push(t);
            }
        }
    }

    // …and the SUBSET products. `SUBSET_ROOTS`/`SUBSET_FILES` are ODK-standard
    // (`SUBSET_FILES = $(foreach n,$(SUBSET_ROOTS), $(foreach f,$(FORMATS_INCL_TSV),
    // $(n).$(f)))` in the ODK Makefile template, driven by `subset_group.products`),
    // and they are part of `$(ASSETS)` — i.e. release artefacts. MONDO declares them
    // by hand (`SUBSETS = mondo-rare mondo-clingen`) because it has no ODK yaml.
    //
    // They have to be PLANNED, not left to Makefile replay: replay rebuilds each
    // prerequisite through the generic path, which does not reproduce the release
    // plan's own output — asking for `subsets/mondo-clingen.owl` alone would rewrite
    // `filtered.owl`, `reasoned.owl` and `mondo-base.owl` at sizes the release plan
    // never produces, clobbering three artefacts the plan had already built.
    for t in make
        .expand("$(SUBSET_FILES)")
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>()
    {
        if !candidates.contains(&t) {
            candidates.push(t);
        }
    }

    // Fallback for hand-written (non-ODK) Makefiles. The candidates above are
    // all top-level `<id>.*` / `<id>-<art>.*` names derived from `$(MAIN_PRODUCTS)`
    // (or the `[base, full]` default). A bespoke Makefile like EFO's defines no
    // such rules — it builds its release products under `$(BUILDDIR)` (e.g.
    // `build/efo.owl`, `build/efo-base.owl`) and names them only as the
    // prerequisites of its `release:` target. When *none* of the standard
    // candidates resolves to a rule, take the `release:` prerequisites that do
    // as the authoritative artefact set (the MakeModel keys rules by their
    // expanded path, so `build/efo.owl` resolves directly).
    if !candidates.iter().any(|t| make.rule_for(t).is_some()) {
        if let Some((rel, _)) = make.rule_for("release") {
            let prereqs: Vec<String> = rel
                .prereqs
                .iter()
                .filter(|p| make.rule_for(p).is_some())
                .cloned()
                .collect();
            if !prereqs.is_empty() {
                candidates = prereqs;
            }
        }
    }
    // Apply the `--artefact` filter: an entry matches by target filename, by
    // `<id>-<entry>.owl`, or by `<id>.<entry>`.
    let matches = |target: &str| -> bool {
        only.is_empty()
            || only.iter().any(|o| {
                target == o || *target == format!("{id}-{o}.owl") || *target == format!("{id}.{o}")
            })
    };
    let mut targets: Vec<(String, bool)> = candidates
        .into_iter()
        .filter(|t| matches(t))
        .map(|t| (t, true))
        .collect();

    // A named target that is not a release product but has a rule of its own is
    // an on-path intermediate — `tmp/<id>-preprocess.owl`, `components/<x>.owl`,
    // MONDO's `reasoned.owl`. The plan records those (the walk below adds them
    // for a full release), so they can be NAMED; naming one on its own has to
    // plan it, or a target the plan carries could not be run.
    for o in only {
        if !targets.iter().any(|(t, _)| t == o) && make.rule_for(o).is_some() {
            targets.push((o.clone(), true));
        }
    }

    // Transitively pull in intermediate targets referenced as a pipeline input
    // (`$<`) that have their own Makefile rule — e.g. MONDO's `reasoned.owl` (the
    // merge→reason→relax→reduce product the primary `mondo.owl` is built from), or
    // ECTO's `<id>-full.owl`. They are not release artefacts themselves but must
    // be built before whatever consumes them, and their coverage gaps block the
    // release just the same. The walk stops at source files (no rule).
    {
        use std::collections::{HashSet, VecDeque};
        let mut seen: HashSet<String> = targets.iter().map(|(t, _)| t.clone()).collect();
        let mut queue: VecDeque<String> = targets.iter().map(|(t, _)| t.clone()).collect();
        while let Some(t) = queue.pop_front() {
            if let Some((rule, stem)) = make.rule_for(&t) {
                if let Some(input) = rule.prereqs.first() {
                    // Substitute the pattern stem so a pattern rule's prereq names a
                    // concrete file (`tmp/composite-%.owl`'s prereq `tmp/collected-%.owl`
                    // → `tmp/collected-lifestages.owl`), not the literal `%` template.
                    let input = match &stem {
                        Some(s) => input.replace('%', s),
                        None => input.clone(),
                    };
                    if !input.contains('%')
                        && !seen.contains(&input)
                        && make.rule_for(&input).is_some()
                    {
                        seen.insert(input.clone());
                        targets.push((input.clone(), true));
                        queue.push_back(input);
                    }
                }
            }
        }
    }
    // A pattern-rule target (`tmp/collected-%.owl`) is a template, never a concrete
    // release artefact — drop any that slipped in.
    let targets: Vec<_> = targets.into_iter().filter(|(t, _)| !t.contains('%')).collect();

    let robot_prefix = {
        let p = make.expand("$(ROBOT)");
        if p.trim().is_empty() { "robot".to_string() } else { p.trim().to_string() }
    };

    // Components are normally shipped pre-built and merged as-is. But some repos
    // (e.g. uPheno) *build* their components through custom Makefile rules that
    // call tooling owlmake can't reproduce (`python3 …`). When a component is not
    // present on disk and has such a rule, surface those steps as real gaps —
    // otherwise the plan would silently "pass" over a build it cannot perform.
    let mut component_gaps: Vec<String> = Vec::new();
    for c in &components {
        if repo.dir.join(c).exists() {
            continue; // shipped pre-built — nothing to build
        }
        for gap in component_build_gaps(make, &robot_prefix, c.as_str()) {
            component_gaps.push(format!("component {c}: {gap}"));
        }
    }

    let mut artefacts = Vec::new();
    for (target, on_path) in targets {
        match make.rule_for(&target) {
            Some(_) => {
                artefacts.push(
                    plan_rule(repo, make, &robot_prefix, &target, on_path)
                        .expect("rule_for matched immediately above"),
                );
            }
            None => {
                // No rule for this target. If it is a *defaulted* candidate
                // (`base`/`full`/an export format the repo doesn't actually
                // build — common in minimal pre-ODK Makefiles that only build
                // `<id>.owl`), it is simply not part of this repo's release:
                // drop it rather than flag a phantom gap. Only when the user
                // explicitly named it (`--artefact`) is a missing rule an error.
                if only.is_empty() {
                    continue;
                }
                artefacts.push(ArtefactPlan {
                    target,
                    input: None,
                    needs: vec![],
                    order_only: vec![],
                    steps: vec![],
                    gaps: vec![],
                    missing_rule: true,
                    stdout_file: None,
                    branches: vec![],
                });
            }
        }
    }

    rewrite_oort(&mut artefacts, &id, version, &ontbase);

    // Everything the artefacts depend on, resolved *now*, at plan time, into
    // ordinary step lists. The Makefile is read here and nowhere else: execution
    // works purely from the plan (and so from `owlmake.json`), which is the whole
    // point of owlmake replacing make rather than driving it. Anything that can
    // only be expressed as a shell command is recorded as one.
    // Pattern products are built natively (see `native_pattern_targets`), so they
    // must not survive as ARTEFACTS either — the transitive `$<` walk above pulls
    // `patterns/definitions.owl`, `patterns/pattern.owl` and the per-pattern
    // `data/*/*.ofn` in as intermediates, and a planned recipe for them competes
    // with the DOSDP engine that `PatternsMode` drives.
    let mut native_patterns = native_pattern_targets(make, &repo.dir);
    native_patterns.extend(native_import_targets(make, &imports, merged_import.as_deref()));
    let artefacts: Vec<ArtefactPlan> =
        artefacts.into_iter().filter(|a| !native_patterns.contains(&a.target)).collect();

    let mut prerequisites = plan_prerequisites(repo, make, &robot_prefix, &artefacts, &imports, merged_import.as_deref());

    // Term-file gaps, once, over the finished target set — the loader's formula
    // exactly (`crate::plan::gaps`), so the two paths cannot disagree about what
    // a plan can build. That target set is what makes the answer the plan's OWN: a
    // generated seed such as `tmp/simple_seed.txt` is absent on a clean checkout,
    // and a build with nothing but the plan would otherwise have to ask the
    // Makefile whether the seed is buildable — and answer "no" for a file the very
    // same plan builds.
    let mut artefacts = artefacts;

    // For every target a switch guards, what the OTHER branch of that conditional
    // builds it by. Done here, over the finished target set, because a branch is
    // only worth recording for a target the plan actually has.
    attach_branches(repo, make, &robot_prefix, &mut artefacts, &mut prerequisites);
    {
        let mut planned: std::collections::HashSet<String> = artefacts
            .iter()
            .chain(prerequisites.iter())
            .filter(|a| !a.missing_rule)
            .map(|a| a.target.clone())
            .collect();
        // The pattern products carry no plan rule because owlmake writes them
        // natively (`native_pattern_targets`), but they ARE produced — so a rule
        // that consumes one is not a gap. `$(IMPORTSEED)` names
        // `all_pattern_terms.txt`, and without this the plan cannot run
        // `tmp/seed.txt` at all.
        planned.extend(native_patterns.iter().cloned());
        // Same for the mirrors: `mirror/<id>.owl` carries no rule because the
        // executor builds it from the import's own `source` + `mirror_steps`, but
        // it IS built — and every ODK import rule names it as a prerequisite.
        planned.extend(native_mirror_targets(make, &imports));
        // `.PHONY` names, so a prerequisite that is a marker rather than a file
        // is not read as a missing input.
        let phony: std::collections::HashSet<String> = make.phony.iter().cloned().collect();
        // A file some OTHER rule's recipe writes is produced by the plan even
        // though no rule NAMES it — see `recipe_outputs`.
        for a in artefacts.iter().chain(prerequisites.iter()) {
            planned.extend(crate::plan::gaps::recipe_outputs(&a.steps));
        }
        for a in artefacts.iter_mut().chain(prerequisites.iter_mut()) {
            a.gaps.extend(crate::plan::gaps::term_file_gaps(&repo.dir, &a.steps, &planned));
            a.gaps.extend(crate::plan::gaps::prerequisite_gaps(
                &repo.dir, &a.needs, &planned, &phony,
            ));
        }
    }

    // Every variable the plan needed has now been expanded, so any Makefile
    // function this parser does not implement has been met by here. Such a
    // reference expands to the empty string, which is indistinguishable from an
    // unset variable and loses whatever it computed WITHOUT failing — a plan that
    // quietly does less is the one thing this must never produce.
    let unknown = make.unknown_functions.borrow();
    if !unknown.is_empty() {
        let names: Vec<&str> = unknown.iter().map(String::as_str).collect();
        anyhow::bail!(
            "unimplemented Makefile function(s): {} — each expands to nothing, so \
             the plan would silently omit whatever they compute",
            names.join(", ")
        );
    }
    drop(unknown);

    // `$(eval)` is the one function whose whole effect is a SIDE effect, so the
    // empty expansion above cannot speak for it. In a recipe it is resolved at
    // ingest (`parse_eval_assignment`), which is where ODK uses it — `$(eval
    // TERM_ID := $(TERM_appendicular))` ahead of each of UBERON's fourteen
    // `$(SUBSETCMD)` subsets. In a VARIABLE DEFINITION nothing resolves it: the
    // variables it would define are never defined, and every later reference to
    // them expands to nothing. That is exactly the silent-shortfall this refuses.
    let eval_vars: Vec<&str> = make
        .vars
        .iter()
        .filter(|(_, v)| v.contains("$(eval") || v.contains("${eval"))
        .map(|(k, _)| k.as_str())
        .collect();
    if !eval_vars.is_empty() {
        let mut names: Vec<&str> = eval_vars;
        names.sort_unstable();
        anyhow::bail!(
            "`$(eval)` in the definition of {} — owlmake resolves an `$(eval)` \
             assignment in a RECIPE, but not one that defines variables at parse \
             time, so whatever it defines would silently be empty",
            names.join(", ")
        );
    }

    // The paths the plan produces WITHOUT a rule of its own, recorded because
    // nothing downstream can work them out. A rule naming `mirror/cl.owl` or
    // `tmp/all_pattern_terms.txt` as a prerequisite is satisfied — owlmake writes
    // both natively — but a loaded plan sees only a path that no artefact and no
    // prerequisite targets, and would report every one of them as a missing
    // input. They are derived here from the Makefile, so here is where they are
    // written down.
    let native_targets = {
        let mut v: Vec<String> = native_patterns
            .iter()
            .cloned()
            .chain(native_mirror_targets(make, &imports))
            .collect();
        v.sort();
        v.dedup();
        v
    };

    // Recorded while the recipes above were expanded: a backtick that reads the
    // version out of a file names it here. Path fields are `repo.dir`-relative,
    // as `edit_file` and `catalog_file` are, so it is carried as written.
    let version_file = make.version_file.borrow().clone();

    let mut plan = Plan {
        // What the repo itself states. A repo running the image's own tool names
        // its ODK release and nothing else; one shipping its own names the tool.
        emulate_odk_version: odk_declared_version(&repo.root, make),
        native_targets,
        // What a bare `owlmake` builds: the repo's default goal, RESOLVED to the
        // targets it names — because after the Makefile is deleted nothing else
        // knows that EFO's `all` meant "…and then run the QC".
        refresh_groups: refresh_groups(make, &imports, merged_import.as_deref(), &artefacts, &prerequisites),
        // Every variable a conditional consulted, with the value that selected
        // the branch whose rules are recorded above.
        gating_flags: make.cond_vars.clone(),
        default_targets: default_targets(repo, make, &artefacts, &prerequisites),
        phony: {
            // Sorted: a HashSet's iteration order is not stable, and this is
            // serialized into a committed plan — an unsorted list would make
            // `owlmake.yaml` differ between two runs over the same repo.
            let mut v: Vec<String> = make.phony.iter().cloned().collect();
            v.sort();
            v
        },
        transient_targets: transient_targets(make, &imports, &artefacts, &prerequisites),
        // `$(SRC)` when the Makefile sets it, else the ODK `<id>-edit.*`
        // convention applied ONCE, here — not by the executor probing extensions
        // in order at build time.
        edit_file: {
            let src = make.expand("$(SRC)").trim().to_string();
            if !src.is_empty() {
                Some(src)
            } else {
                crate::odk::find_edit_file(&repo.dir)
            }
        },
        catalog_file: catalog_file(&repo.dir),
        emulate_robot_version: emulate_robot_version(&repo.root, make),
        // The global `--strict` / `-x` flags, as the repo's own `$(ROBOT)` launcher
        // declares them: they change which axioms survive a parse and the bytes
        // of every RDF/XML artefact, so they are recorded rather than left to
        // whoever invokes the build.
        strict: robot_global_flag(make, &["--strict"]),
        xml_entities: robot_global_flag(make, &["-x", "--xml-entities"]),
        dosdp: {
            let dir = make.expand("$(PATTERNDIR)");
            let dir = dir.trim();
            plan_dosdp(
                &repo.dir,
                if dir.is_empty() { "../patterns" } else { dir },
                Some(make),
                &ontbase,
                &version,
            )
        },
        id,
        // The DEFAULT release version. Every other string that needs it holds
        // `{version}`, a reference to this field, so the run can supply a
        // different date without the plan being regenerated.
        //
        // A configuration that reads the version out of a file gets that file's
        // CURRENT contents as the default and the file itself as `version_file`,
        // which the run re-reads. Read here rather than at expansion time because
        // the recipes that mention the file have all been expanded by now.
        version: version_file
            .as_deref()
            .and_then(|f| read_version_file(&repo.dir, f))
            .unwrap_or_else(|| make.version_default.clone()),
        version_file: version_file.clone(),
        ontology_iri: format!("{ontbase}.owl"),
        reasoner,
        use_base_merging,
        exclude_iri_patterns,
        slme_individuals,
        imports,
        merged_import,
        components,
        variables: exec_variables(repo),
        component_gaps,
        prerequisites,
        artefacts,
    };
    drop_unspelled_robot_launcher(&mut plan);
    Ok(plan)
}

/// Drop the recorded `ROBOT` launcher when no step in the plan spells it.
///
/// The launcher travels with a plan so that a step which IS a command line can
/// have owlmake put in its place at the command position. A plan whose ontology
/// commands are all ops has no such step, and carrying the launcher there would
/// have the plan name a program the build never runs — and name a path the
/// repository need not contain.
///
/// Decided against the plan's own serialized form, so no step can be missed:
/// whatever container a step sits in, the launcher is kept if the plan says it
/// anywhere, and kept if the question cannot be answered at all.
fn drop_unspelled_robot_launcher(plan: &mut Plan) {
    let Some(launcher) = plan.variables.remove("ROBOT") else { return };
    let spelled = serde_yaml::to_string(&crate::spec::OwlmakeSpec::from_plan(plan))
        .map(|text| text.contains(&launcher))
        .unwrap_or(true);
    if spelled {
        plan.variables.insert("ROBOT".to_string(), launcher);
    }
}

/// Turn one Makefile rule into an [`ArtefactPlan`]: expand the automatic
/// variables (`$@`/`$<`/`$^`/`$*`) against the rule's own target and
/// stem-substituted prerequisites, then map each recipe line to steps. Returns
/// `None` when the target has no rule.
fn plan_rule(
    repo: &OdkRepo,
    make: &super::makefile::MakeModel,
    robot_prefix: &str,
    target: &str,
    on_release_path: bool,
) -> Option<ArtefactPlan> {
    let (rule, stem) = make.rule_for(target)?;
    let mut autos = Autos::default();
    autos.set("@", target);
    // For a pattern rule, make substitutes the stem for `%` in the prerequisites
    // before they reach `$<`/`$^` — AND, under `.SECONDEXPANSION`, expands what
    // the stem substitution exposes. `expanded_prereqs` is the one place that
    // does both, and it is already what the plan's `needs` is built from; doing
    // the `%` replacement by hand here bound `$^` to a DIFFERENT, unexpanded
    // list. UBERON's `$(TMPDIR)/collected-%.owl` lists `$$(COLLECTED_$$*_SOURCES)`
    // and its recipe is `merge $(foreach src,$^,-i $(src))`, so the literal
    // `$(COLLECTED_$*_SOURCES)` was recorded as a merge INPUT and the build died
    // with "merge input `$(COLLECTED_$*_SOURCES)` does not exist" — while `needs`
    // beside it named all fifteen real files.
    //
    // The automatic variables read the NORMAL prerequisites only — an order-only
    // one is a build-order edge, not an input. uPheno's
    // `$(IMPORTSEED): $(PRESEED) $(TMPDIR)/all_pattern_terms.txt | $(TMPDIR)` runs
    // `cat $^ | sort | uniq > $@`, where the directory in `$^` makes it
    // `cat: tmp: Is a directory` and the import seed loses the pattern terms.
    // `needs` below takes the full set, order-only included, because the graph
    // wants those edges.
    let prereqs: Vec<String> = expanded_prereqs_opt(repo, rule, stem.as_deref(), target, false);
    if let Some(first) = prereqs.first() {
        autos.set("<", first);
    }
    autos.set("^", &prereqs.join(" "));
    if let Some(s) = &stem {
        autos.set("*", s);
    }
    // A recorded recipe says HOW to build this target; WHETHER to build it is the
    // executor's decision, taken from the run's modes. So the ODK build-mode flags
    // are true here by construction — if the flag that gates this target were off,
    // owlmake would not be building it — and freezing them into a condition writes
    // one run's switches into a plan meant to be portable: planned under `MIR=false`,
    // MONDO's recipes would record the permanently-dead `[ false = true ]`, with
    // every fetch beneath it unreachable however the plan was later invoked.
    let prereq_input = prereqs.first().cloned();
    // What the recipe's FIRST invocation opens its pipeline with, which is what
    // the pipeline actually reads. A rule's first prerequisite is a dependency
    // edge; only a recipe that spells `$<` also makes it the input.
    //
    // uPheno's mappings component is `components/upheno-mappings.owl: $(SRC)
    // …sssom.owl` with a recipe of `merge -i …sssom.owl -i …sssom.owl`. Opening
    // from `$(SRC)` there merged the edit ontology's whole import closure —
    // including the component's own previous build — into its replacement: 57 MB
    // of mapping sets became 195 MB, and every artefact above it grew with them.
    //
    // The mirror rules are the other side of it: `mirror-<id>: | $(TMPDIR)` has
    // no normal prerequisite at all, and its recipe curls the ontology and then
    // names the download explicitly. With no input the executor would thread an
    // EMPTY model through `remove`, leaving every `tmp/mirror-*.owl` a stub.
    let mut recipe_input: Option<String> = None;
    let mut saw_robot_line = false;
    let mut extra_needs: Vec<String> = Vec::new();
    let mut steps = Vec::new();
    // Where the recipe redirects its console output, when the redirect is the
    // only thing that names the target.
    let mut stdout_file: Option<String> = None;
    // Each command line is its own invocation. The first opens the pipeline from
    // the rule's input; every later one opens a new pipeline over its own
    // `--input`, so a `merge` that leads such a line REPLACES the model rather
    // than adding to it.
    let mut command_lines = 0usize;
    for line in &rule.recipe {
        // A recipe-time `$(eval VAR := VALUE)` assignment produces no command. It
        // is resolved HERE, at ingest, so the recorded step carries the real command
        // line and execution needs no make — UBERON's `$(SUBSETCMD)` targets set
        // `TERM_ID` this way before each invocation.
        if let Some((name, value)) = parse_eval_assignment(line.trim(), make, &autos) {
            autos.set(&name, &value);
            continue;
        }
        let expanded = make.expand_with(line, &autos);
        // The pipeline opens with the first ROBOT invocation's own input. A line
        // that runs something else — `$(MAKE) …`, `sssom convert …` — threads no
        // model, so the scan continues past it; the first robot line that names
        // no input is where the rule's own `$<` comes in, and the scan stops
        // there rather than reaching a later line's `--input`.
        if !saw_robot_line && is_robot_line(&expanded, robot_prefix) {
            saw_robot_line = true;
            recipe_input = first_robot_input(&expanded, robot_prefix);
        }
        if stdout_file.is_none() {
            stdout_file = robot::chain_stdout_file(&expanded, robot_prefix);
        }
        let mut line_steps = recorded_steps(&expanded, robot_prefix);
        // A later line that is ITSELF a robot invocation naming its OWN input opens
        // a new pipeline over that input: a separate process shares nothing with
        // the last but files. Recording the boundary only for a line that happens
        // to open with `merge --input` left every other opening op reading
        // whatever the previous line had in memory — a CONSTRUCT's whole source
        // ontology in place of the construct, or a query run against the wrong
        // file entirely.
        //
        // Both conditions are load-bearing, and each was learnt from a build that
        // came out wrong without it:
        //
        //  * a NON-robot line is a separate process too, but it neither takes nor
        //    leaves an in-memory ontology, so the model in hand must SURVIVE it.
        //    MONDO's `filtered.obo` is two `perl -ne …` filters and then a robot
        //    invocation; resetting at the perl lines discarded the model the rest
        //    of the recipe works on, growing four release artefacts by ~17 MB and
        //    failing two outright. A shell line that rewrites the target on disk is
        //    already handled downstream, by re-reading what it staged.
        //  * a robot line naming NO input of its own continues from what it was
        //    given, so it is not a boundary either. Every case that motivated this
        //    — uPheno's bridge, OBA's PATO construct, EFO's legal_diseases — names
        //    its input explicitly.
        if command_lines > 0 && !line_steps.is_empty() && is_robot_line(&expanded, robot_prefix) {
            if let Some(opens_with) = first_robot_input(&expanded, robot_prefix) {
                line_steps.insert(0, Step::Boundary { input: Some(opens_with) });
            }
        }
        // A line that opens over a REMOTE ontology is a boundary on ANY line,
        // including the first: an IRI cannot become the rule's `$<`, which is
        // resolved to a path, so the boundary is how the plan names it.
        if !line_steps.is_empty() && is_robot_line(&expanded, robot_prefix) {
            if let Some(iri) = first_robot_iri_input(&expanded, robot_prefix) {
                if !matches!(line_steps.first(), Some(Step::Boundary { .. })) {
                    line_steps.insert(0, Step::Boundary { input: Some(iri) });
                }
            }
        }
        // A `mint` line that names no `--id-ranges` takes the single
        // `*-idranges.owl` beside the edit file, which is how the recipe's ROBOT
        // resolves it. Resolve it HERE so the plan NAMES the file: leaving it
        // unset made execution glob for it, and it globbed the directory `om` was
        // launched from rather than the ontology directory, so EFO's
        // `allocate-definitive-ids` — which CI runs unattended on every merge to
        // master — failed with "no *-idranges.owl file found in .".
        for step in &mut line_steps {
            if let Step::Op(Op::Mint { id_ranges, .. }) = step {
                if id_ranges.is_none() {
                    *id_ranges = idranges_beside_edit_file(&repo.dir);
                }
            }
        }
        if !line_steps.is_empty() {
            command_lines += 1;
        }
        steps.extend(line_steps);
    }
    let mut input = recipe_input.or(prereq_input);
    // A recipe whose ontology input is the target itself writes that file in an
    // earlier step of the same recipe — `git show master:… > $@` ahead of
    // `merge -i $@ reason -o $@.owl`. The rule PRODUCES its input, so recording it
    // as one asks execution to build the target in order to build the target.
    if input.as_deref() == Some(target) {
        input = None;
    }
    drop_target_round_trip(&mut steps, target);
    // A recipe that is nothing but recursive make is an aggregate wearing a
    // disguise: `feature_diff: make reports/a.txt -B; make reports/b.txt -B` says
    // "these two targets", and saying it as a dependency beats spawning owlmake
    // twice to say it.
    //
    // Only when no assignment can change what the sub-make would see. `ifeq
    // ($(IMP),true)` decides whether a repo's import rules EXIST, so
    // `$(MAKE) IMP=true all_imports` is not the same request as `all_imports` and
    // must stay a step. An assignment to a variable no conditional consults —
    // EFO's `IMP=false PAT=false`, inherited from the ODK and referenced nowhere
    // in its Makefile — cannot.
    let all_make = !steps.is_empty()
        && steps.iter().all(|s| matches!(s, Step::OwlmakeCli { name, .. } if name == "make"));
    if all_make {
        let mut targets = Vec::new();
        let mut flattenable = true;
        for s in &steps {
            let Step::OwlmakeCli { args, .. } = s else { continue };
            for a in args {
                match a.split_once('=') {
                    Some((name, _)) => {
                        if make.cond_vars.contains_key(name.trim()) {
                            flattenable = false;
                        }
                    }
                    None => targets.push(a.clone()),
                }
            }
        }
        if flattenable && !targets.is_empty() {
            let already = expanded_prereqs(repo, rule, stem.as_deref(), target);
            for t in targets {
                if !already.contains(&t) {
                    extra_needs.push(t);
                }
            }
            steps.clear();
        }
    }
    resolve_build_config_branches(&mut steps, repo);
    // Step-level gaps only. Term-file gaps need the full set of planned targets,
    // which does not exist yet, so they are added once at the end of `build` —
    // by `crate::plan::gaps::term_file_gaps`, the same function the loader calls,
    // so a plan cannot report different gaps depending on how it was obtained.
    let gaps: Vec<String> = steps.iter().flat_map(|s| s.gaps()).collect();
    // The order-only half of the same expansion. It is recorded separately because
    // the two answer different questions: everything here is a dependency the
    // graph must satisfy, and none of it decides whether this target is stale.
    let order_only: Vec<String> = {
        let all = expanded_prereqs(repo, rule, stem.as_deref(), target);
        let normal = expanded_prereqs_opt(repo, rule, stem.as_deref(), target, false);
        all.into_iter().filter(|p| !normal.contains(p)).collect()
    };
    Some(ArtefactPlan {
        target: target.to_string(),
        input,
        needs: {
            let mut n = expanded_prereqs(repo, rule, stem.as_deref(), target);
            n.extend(extra_needs);
            n
        },
        order_only,
        steps,
        gaps,
        missing_rule: false,
        stdout_file,
        // Filled in once every target is known, by `attach_branches`.
        branches: Vec::new(),
    })
}

/// What a bare `owlmake` (and a bare `owlmake make`) builds.
///
/// The default goal is the first target of the first explicit rule; its MEANING is
/// that rule's prerequisites. EFO's is
/// `all: all_imports all_gwas all_components release qc` — a release AND its QC —
/// so a bare build that stopped at the release artefacts would silently skip the
/// checks, under a workflow still called `ontology_qc`.
///
/// Resolved here, at plan time, because after the Makefile is deleted nothing
/// else knows what `all` meant. Members are pruned to the ones a default build
/// should actually run on its own:
///
///   * import modules and anything reachable from an artefact's `needs` are
///     dropped — the release pipeline already governs them, and EFO's `all`
///     otherwise expands to fourteen `imports/%_import.owl` whose mirrors are
///     gitignored, so a bare `om` would start re-downloading upstream ontologies;
///   * the ODK aggregate names are dropped for the same reason;
///   * a member the plan cannot name is dropped with a note, since a plan that
///     advertises a default build it cannot perform is the "nothing in a plan
///     should be inert" failure.
///
/// Falls back to the release artefacts when the repo declares no goal.
fn default_targets(
    repo: &OdkRepo,
    make: &super::makefile::MakeModel,
    artefacts: &[ArtefactPlan],
    prerequisites: &[ArtefactPlan],
) -> Vec<String> {
    use std::collections::HashSet;

    let release: Vec<String> = artefacts.iter().map(|a| a.target.clone()).collect();
    let Some(mut goal) = make.default_goal.clone() else { return release };
    // Follow a pure ALIAS to the goal that actually lists the work: ODK's
    // `all: all_odk`, whose own prerequisites are the ordered phases
    // (`odkversion config_check test custom_reports all_assets`). Stopping at
    // `all` would hide that ordering and emit the artefacts first.
    for _ in 0..8 {
        match make.rules.get(&goal) {
            Some(r) if r.recipe.is_empty() && r.prereqs.len() == 1 => {
                let next = r.prereqs[0].clone();
                if make.rules.contains_key(&next) {
                    goal = next;
                    continue;
                }
                break;
            }
            _ => break,
        }
    }
    let Some(rule) = make.rules.get(&goal) else { return release };

    // Targets the release build already covers, so a default build must not run
    // them a second time (or, worse, run them FIRST and rebuild the world).
    let mut covered: HashSet<&str> = HashSet::new();
    for a in artefacts {
        covered.insert(a.target.as_str());
        for n in &a.needs {
            covered.insert(n.as_str());
        }
    }
    const AGGREGATES: &[&str] = &[
        "all_imports",
        "refresh_imports",
        "no_mirror_refresh_imports",
        "refresh_imports_excluding_large",
        "patterns",
        "dosdp",
        "prepare_release",
        "prepare_release_fast",
        "all_assets",
    ];

    let nameable: HashSet<&str> = artefacts
        .iter()
        .chain(prerequisites.iter())
        .filter(|a| !a.missing_rule)
        .map(|a| a.target.as_str())
        .collect();

    // `expanded_prereqs` — NOT `make.expand` — because prerequisites are already
    // expanded at parse time and this is the spelling every other target string
    // in the plan uses (`ArtefactPlan::needs`). A one-token difference would drop
    // members silently.
    let members = expanded_prereqs(repo, rule, None, &goal);

    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pushed_release = false;
    for m in members {
        let norm = m.replace('-', "_");
        // `all_assets` IS the release set, so expand it IN PLACE rather than
        // dropping it. A goal's prerequisites run left to right —
        // `odkversion config_check test custom_reports all_assets` — so `test`
        // precedes the artefacts, and HPO depends on that: its `test` includes
        // `test_obo`, whose recipe redirects into `hp.obo` as a SIDE EFFECT
        // (`… > hp.obo`). Building the artefacts first would let the release rule
        // write `hp.obo` and `test_obo` overwrite it; in the declared order
        // `all_assets` finds `hp.obo` already newer than its prerequisite.
        if norm == "all_assets" {
            if !pushed_release {
                pushed_release = true;
                for r in &release {
                    if seen.insert(r.clone()) {
                        out.push(r.clone());
                    }
                }
            }
            // `all_assets` is `$(ASSETS) check_rdfxml_assets` — the release set
            // AND the checks over it. Expanding it to the artefacts alone leaves
            // the checks named nowhere, and a bare build then stops at the
            // artefacts with its QC silently skipped. Carry the members the
            // release set does not already cover.
            if let Some(r) = make.rules.get(&m) {
                for extra in expanded_prereqs(repo, r, None, &m) {
                    if covered.contains(extra.as_str()) || !nameable.contains(extra.as_str()) {
                        continue;
                    }
                    if seen.insert(extra.clone()) {
                        out.push(extra);
                    }
                }
            }
            continue;
        }
        if AGGREGATES.contains(&norm.as_str()) {
            continue;
        }
        // The release member expands to the artefact set, once, in place.
        if norm == "release" || norm == "all" {
            if !pushed_release {
                pushed_release = true;
                for r in &release {
                    if seen.insert(r.clone()) {
                        out.push(r.clone());
                    }
                }
            }
            // …and the member's OWN recipe still has to run, after them. EFO's
            // `release: … ; cp $^ $(RELEASEDIR)` publishes the four artefacts to
            // the repository root, which is where the released files actually
            // live — expanding the target to its prerequisites and stopping there
            // would build all four and ship none of them.
            if prerequisites.iter().any(|p| p.target == m && !p.steps.is_empty())
                && seen.insert(m.clone())
            {
                out.push(m);
            }
            continue;
        }
        if covered.contains(m.as_str()) {
            continue;
        }
        if !nameable.contains(m.as_str()) {
            status!("make: default goal `{goal}` names `{m}`, which the plan cannot build — skipped");
            continue;
        }
        if seen.insert(m.clone()) {
            out.push(m);
        }
    }
    // A goal that named only covered work still means "build the release".
    let mut out = if pushed_release {
        out
    } else {
        let mut with_release = release.clone();
        with_release.extend(out.into_iter().filter(|t| !release.contains(t)));
        with_release
    };
    // …and the GOAL'S OWN recipe runs last, after the work it names. The alias
    // walk above stops at the first rule that has a recipe, so on a Makefile whose
    // `all: release` has a single prerequisite it lands ON `release` — the members
    // are then the four artefacts and `release`'s own `cp $^ $(RELEASEDIR)` is
    // named nowhere. Such a build would produce every artefact and publish none of
    // them. (EFO's `all` lists five prerequisites and carries no recipe, so it does
    // not reach this; a stock `all: release` does.)
    if prerequisites.iter().any(|p| p.target == goal && !p.steps.is_empty())
        && !out.contains(&goal)
    {
        out.push(goal);
    }
    out
}

/// The components the repo declares, as paths RELATIVE TO THE ONTOLOGY DIRECTORY.
///
/// An ODK yaml and a `$(COMPONENTS)` variable both name a component by bare
/// filename (`subclasses.owl`), while the file itself lives in `components/` —
/// and the rule that builds it is `components/%.owl`. Recording the bare name
/// would put a path in the plan that does not exist, which is exactly the plan
/// naming something it cannot read.
///
/// Declared-first matters, and discovery only supplements: a repo whose
/// component sits somewhere other than `components/` would have it silently
/// dropped, because answering by `read_dir` says "what is on disk", not "what did
/// the repo ask for".
fn declared_components(repo: &OdkRepo) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let declared: Vec<String> = repo
        .yaml
        .components
        .as_ref()
        .map(|c| c.products.iter().map(|p| p.filename.clone()).collect())
        .unwrap_or_default();
    for c in &declared {
        let nested = format!("components/{c}");
        // An undeclared-on-disk component still enters the plan under the path
        // its rule would build, so `component_gaps` can report what is missing.
        out.push(if repo.dir.join(c).exists() && !repo.dir.join(&nested).exists() {
            c.clone()
        } else {
            nested
        });
    }
    out
}

/// The artefact-format generation this repo's own build resolves to, as the repo
/// itself declares it — in its generated Makefile, in `run.sh`, or in the
/// workflow that installs its toolchain.
///
/// The pin decides artefact BYTES in more than one place, and 1.9.9 is the
/// boundary in each: at or above it, OBO Graphs JSON carries per-element nested
/// axiom-annotation `meta`, and `query --update` keeps the document's prefixes
/// (an `xmlns:` line per unused prefix in every artefact downstream of one). A
/// repo's committed releases carry the shape of the generation it pins, so the
/// question is asked once here and both behaviours read the answer off the plan.
///
/// A repo answers it in one of two ways.
///
/// An ODK repo takes it from the image. Usually its generated Makefile says
/// which in `ODK_VERSION_MAKEFILE`; MONDO's does not, and instead names
/// `odkfull:v1.6` in `src/ontology/run.sh` and as the `container:` of all
/// thirteen of its workflows — the same statement, made somewhere else, so
/// `odk_image_version` reads it rather than falling through to a default that
/// happens to agree.
///
/// A repo with a hand-written Makefile names its own, and where it names it is
/// `.github/workflows/`. EFO installs v1.9.7 there — the same generation its
/// `ROBOT = ../../bin/robot` launcher points at — so `efo.json` carries no
/// nested `meta`.
/// The ODK release the repo itself declares, if it declares one.
///
/// A repo built by the image states its release — in the Makefile's
/// `ODK_VERSION_MAKEFILE`, in `run.sh.conf`, or as the `container:` of its
/// workflows. A repo with a hand-written Makefile that launches its own tool
/// states no release at all, and `None` is the honest answer: what it emulates is
/// a tool version, recorded separately.
fn odk_declared_version(root: &Path, make: &super::makefile::MakeModel) -> Option<Version> {
    use super::workflows::odk_image_version;
    let var = |name: &str| make.expand(&format!("$({name})")).trim().to_string();
    if var("ANNOTATE_ONTOLOGY_VERSION").is_empty() {
        return None;
    }
    let declared = var("ODK_VERSION_MAKEFILE");
    let declared = declared.trim().trim_start_matches('v');
    let parts: Vec<u32> = declared.split('.').filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [maj, min, patch, ..] => Some((*maj, *min, *patch)),
        [maj, min] => Some((*maj, *min, 0)),
        _ => odk_image_version(root),
    }
}

fn emulate_robot_version(root: &Path, make: &super::makefile::MakeModel) -> Version {
    use super::workflows::{ci_robot_version, odk_image_version, odk_robot_version};
    let var = |name: &str| make.expand(&format!("$({name})")).trim().to_string();
    if var("ANNOTATE_ONTOLOGY_VERSION").is_empty() {
        // Not ODK-generated: the repo's CI is the statement of which version runs.
        // Absent one, assume the current generation.
        return ci_robot_version(root).unwrap_or(CURRENT_ROBOT);
    }
    let declared = var("ODK_VERSION_MAKEFILE");
    let declared = declared.trim().trim_start_matches('v');
    let parts: Vec<u32> = declared.split('.').filter_map(|p| p.parse().ok()).collect();
    let odk = match parts.as_slice() {
        [maj, min, patch, ..] => Some((*maj, *min, *patch)),
        [maj, min] => Some((*maj, *min, 0)),
        // The Makefile does not say. The repo still does, in `run.sh` and in the
        // `container:` its workflows name — and if it says nothing anywhere, the
        // ODK it would be built with today is the current one.
        _ => odk_image_version(root),
    };
    odk.map_or(CURRENT_ROBOT, odk_robot_version)
}

/// Whether the repo's `$(ROBOT)` launcher carries one of the given global flags.
fn robot_global_flag(make: &super::makefile::MakeModel, names: &[&str]) -> bool {
    let launcher = make.expand("$(ROBOT)");
    launcher.split_whitespace().any(|t| names.contains(&t))
}

/// The rebuild switches this repo exposes, and the plan targets each covers.
///
/// ODK gates whole rule sets on `IMP`, `MIR` and `PAT`. Ingest resolves the
/// Makefile with all three TRUE (see `MakeModel::WORKFLOW_FLAGS`) so the plan is
/// a pure function of the repo, and records here what each switch would have
/// covered — because after the Makefile is deleted nothing else knows that `IMP`
/// existed, let alone which targets it gated.
///
/// A group with no targets is not recorded: a switch that gates nothing is inert
/// plan content, and EFO is the case (its mirrors come from `get_mirrors.sh`, so
/// it has no `mirrors` group at all).
fn refresh_groups(
    make: &super::makefile::MakeModel,
    imports: &[ImportPlan],
    merged_import: Option<&str>,
    artefacts: &[ArtefactPlan],
    prerequisites: &[ArtefactPlan],
) -> Vec<crate::plan::RefreshGroup> {
    use crate::plan::{Freshness, RefreshGroup};
    let mirrordir = mirror_dir(make);

    // Every import owlmake can actually re-fetch, at the path it fetches into.
    //
    // NOT `make.rule_for(...)`: a repo does not need a mirror RULE for owlmake to
    // know where its mirrors come from. EFO has none — it fetches with a
    // `get_mirrors.sh` beside the Makefile, which ingest reads
    // (`crate::odk::scan_mirror_scripts`), and that is why the plan records
    // `mondo <- .../mondo/mondo-base.owl` rather than a guessed PURL. Keying the
    // group on make rules would throw that knowledge away and leave EFO with no
    // `mirrors` group at all, refusing `--rebuild mirrors` on a repo whose
    // mirrors owlmake can fetch perfectly well.
    //
    // Every import, however its mirror is obtained. A custom mirror is fetched by
    // replaying the repo's own recipe rather than by a single GET, and `MIR` gates
    // it exactly as it gates a URL one — so it belongs in the list that enumerates
    // what the switch reaches. ECTO's `mirror/foodon.owl` is the case: eighteen
    // imports, one of them custom, and `--rebuild mirrors` covers all eighteen.
    let mut mirrors: Vec<String> = imports
        .iter()
        .map(|i| format!("{mirrordir}/{}.owl", i.id))
        .collect();
    let merged_mirror = format!("{mirrordir}/merged.owl");
    if make.rule_for(&merged_mirror).is_some() {
        mirrors.push(merged_mirror);
    }

    // A repo may define mirrors of its OWN, outside the ODK import machinery.
    // UBERON has 19 (`mirror-ma`, `mirror-fbbt`, …) feeding its composite
    // pipeline, and guards each in the RECIPE — `if [ $(MIR) = true ] && [ $(IMP)
    // = true ]; then curl …` — not with an `ifeq`. Ingest binds the workflow flags
    // to `true`, so that guard cannot survive into the plan as a condition; the
    // refresh group is what has to carry it.
    //
    // Without them `MIR=false` re-fetched every one and then rebuilt the committed
    // `imports/local-*.owl` they feed, and `tmp/collected-metazoan.owl` came out
    // with 79,070 of its 141,502 classes — FBbt 963 against 293,992.
    for a in prerequisites {
        let t = &a.target;
        let is_mirror = t.starts_with("mirror-")
            || Path::new(t).parent().is_some_and(|p| p == Path::new(&mirrordir));
        if is_mirror && !mirrors.contains(t) {
            mirrors.push(t.clone());
        }
    }

    // …and rules gated by an `ifeq` on MIR itself, whatever their path. UBERON's
    // mapping-set mirrors (`../mappings/fbbt.sssom.tsv`, `biomappings` and its
    // `tmp/` fetch) live inside `ifeq ($(strip $(MIR)),true)`: under `MIR=false`
    // GNU make has no rule for them and the committed files stand. Ingest binds
    // `MIR=true`, so the guard survives only as `Rule::workflow_guards`, and the
    // group carries it from there. Without this, `MIR=false` re-fetched the
    // 22 MB biomappings set live and rebuilt a release artefact the reference
    // left untouched — content that drifts with every fetch.
    for a in artefacts.iter().chain(prerequisites) {
        // Plan targets are repo-rooted (`src/mappings/…`); rules are keyed as the
        // Makefile names them, relative to `src/ontology` (`../mappings/…`).
        // Look the rule up under both spellings.
        let t = a.target.as_str();
        let makefile_name = t
            .strip_prefix("src/ontology/")
            .map(str::to_string)
            .or_else(|| t.strip_prefix("src/").map(|rest| format!("../{rest}")));
        let mir_guarded = [Some(t.to_string()), makefile_name]
            .into_iter()
            .flatten()
            .any(|name| {
                make.rule_for(&name)
                    .is_some_and(|(r, _)| r.guards.iter().any(|g| g == "MIR"))
            });
        if mir_guarded && !mirrors.contains(&a.target) {
            mirrors.push(a.target.clone());
        }
    }

    // Under base merging the per-product modules are never written — ODK's
    // `IMPORT_ROOTS` is `merged_import` alone — so listing them here would make
    // `--rebuild imports` name 18 files the build does not produce.
    let mut import_targets: Vec<String> = match merged_import {
        Some(_) => Vec::new(),
        None => imports.iter().map(|i| i.output.clone()).filter(|o| !o.is_empty()).collect(),
    };
    if let Some(m) = merged_import {
        import_targets.push(m.to_string());
    }

    // …and anything built FROM a mirror. UBERON's `imports/local-%.owl:
    // mirror/%.owl` is an ordinary pattern rule, and under `MIR=false` the
    // `mirror/%.owl` rule does not exist — so GNU make finds the pattern
    // INAPPLICABLE (a pattern rule whose prerequisites can be neither found nor
    // made does not fire) and the committed `imports/local-*.owl` stands. Pinning
    // the mirror alone is not enough: without this the local files were rebuilt
    // from a mirror that is not there.
    for a in prerequisites {
        let from_mirror = a
            .needs
            .iter()
            .any(|n| Path::new(n).parent().is_some_and(|p| p == Path::new(&mirrordir)));
        if from_mirror && !import_targets.contains(&a.target) {
            import_targets.push(a.target.clone());
        }
    }

    let pattern_targets: Vec<String> = prerequisites
        .iter()
        .map(|a| a.target.clone())
        .filter(|t| t.contains("pattern") || t.ends_with("definitions.owl"))
        .collect();

    // Whether an ORDINARY build re-fetches the mirrors — a different question
    // from whether owlmake CAN, which is what membership above answers.
    //
    // ODK writes a `$(MIRRORDIR)/%.owl:` rule whose recipe downloads, and the
    // release depends on it, so `make all` re-fetches and `Rebuild` is what the
    // reference does. EFO has no mirror rule at all: its mirrors come from a
    // `get_mirrors.sh` that no target invokes, so `make all` builds from the
    // committed cache and the default is `Keep`.
    //
    // Recording the wrong one silently changes what a release CONTAINS, because
    // the mirrors are its upstream input — a `Rebuild` default on EFO rebuilt
    // every import module against whatever upstream had published that morning,
    // where the reference build used the cache on disk.
    let build_fetches_mirrors = mirrors.iter().any(|m| {
        // Group targets are mirrordir-relative as built above; rules are keyed as
        // the Makefile names them. Look under both spellings.
        let makefile_name = m.strip_prefix("src/ontology/").unwrap_or(m);
        make.rule_for(m).is_some() || make.rule_for(makefile_name).is_some()
    });
    let mirrors_default =
        if build_fetches_mirrors { Freshness::Rebuild } else { Freshness::Keep };

    // The import modules are extracted FROM the mirrors, so an ordinary build
    // rebuilds them exactly when it re-fetches those: ECTO's Makefile defaults
    // `MIR = true` and `IMP = true`, so `make all` downloads the mirrors and the
    // merged module — older than what it is extracted from — is rebuilt against
    // them. EFO downloads nothing, so its committed modules are up to date and
    // stand.
    //
    // Recording `keep` unconditionally made a bare build re-fetch every mirror
    // and then publish a release from the committed import anyway: fresh inputs
    // on disk, none of them in the artefacts.
    let imports_default = if build_fetches_mirrors && !import_targets.is_empty() {
        Freshness::Rebuild
    } else {
        Freshness::Keep
    };

    let mut groups: Vec<RefreshGroup> = [
        ("mirrors", "MIR", mirrors, mirrors_default),
        ("imports", "IMP", import_targets, imports_default),
        ("patterns", "PAT", pattern_targets, Freshness::Rebuild),
    ]
    .into_iter()
    .filter(|(_, _, t, _)| !t.is_empty())
    .map(|(name, flag, targets, default)| RefreshGroup {
        name: name.to_string(),
        flag: flag.to_string(),
        targets,
        default,
    })
    .collect();
    groups.extend(switch_groups(make, artefacts, prerequisites, &groups));
    groups
}

/// Record, for every target whose recipe is guarded by a switch, what the OTHER
/// branch of that conditional says.
///
/// A conditional is resolved once, so the plan holds the rules of the branch
/// taken. That is only half the configuration, and the missing half is not
/// always "nothing": UBERON's bridges section closes with
///
/// ```text
/// else # BRI=false
/// $(TMPDIR)/bridges:
///     touch $@
/// endif
/// ```
///
/// so under `BRI=false` the stamp is CREATED, not merely left alone. Pinning it
/// is right for the four targets whose rules simply cease to exist and wrong for
/// this one, and which of the two applies is a fact about each target rather than
/// about the switch. So it is recorded per target: a `branches` entry where the
/// other branch defines a recipe, and nothing where it defines no rule — the
/// refresh group already says the file stands.
///
/// The other branch is read by resolving the SAME configuration again with the
/// switch bound to its other value, so the two models differ by that binding and
/// by nothing else. A recipe identical in both branches is not a branch at all
/// (the rule simply sits inside the conditional), so it is not recorded.
fn attach_branches(
    repo: &OdkRepo,
    make: &super::makefile::MakeModel,
    robot_prefix: &str,
    artefacts: &mut [ArtefactPlan],
    prerequisites: &mut [ArtefactPlan],
) {
    // Only switches that actually guard a planned target can have a branch, and
    // each costs one more resolution of the configuration, so the set is
    // collected first.
    let mut flags: Vec<String> = Vec::new();
    for a in artefacts.iter().chain(prerequisites.iter()) {
        let Some((rule, _)) = make.rule_for(&a.target) else { continue };
        for g in &rule.guards {
            if make.switch_vars.contains(g) && !flags.contains(g) {
                flags.push(g.clone());
            }
        }
    }
    for flag in flags {
        let configured = make.cond_vars.get(&flag).map(String::as_str).unwrap_or("");
        let other = if crate::plan::is_on(configured) { "false" } else { "true" };
        let Some(alt) = repo.configuration_under(&flag, other) else { continue };
        for a in artefacts.iter_mut().chain(prerequisites.iter_mut()) {
            let guarded = make
                .rule_for(&a.target)
                .is_some_and(|(r, _)| r.guards.iter().any(|g| *g == flag));
            if !guarded {
                continue;
            }
            // `plan_rule` is the same seam ingest used for the branch that was
            // taken, so a branch's steps are built exactly as the recorded ones
            // were and cannot drift from them.
            let Some(alt_plan) = plan_rule(repo, &alt, robot_prefix, &a.target, false) else {
                // No rule on the other side: the target is not built under that
                // value, which is what its refresh group already says.
                continue;
            };
            if !crate::spec::steps_differ(&alt_plan.steps, &a.steps) && alt_plan.needs == a.needs {
                continue;
            }
            a.branches.push(crate::plan::Branch {
                flag: flag.clone(),
                value: other.to_string(),
                input: alt_plan.input,
                needs: alt_plan.needs,
                steps: alt_plan.steps,
            });
        }
    }
}

/// The switch groups above are the three the ODK layout always has; a repository
/// invents its own, and they are found the same way.
///
/// A switch is a variable a conditional tests against a boolean word
/// (`MakeModel::switch_vars`), and its group is the targets whose RECIPE exists
/// only inside that conditional. UBERON's `BRI` is the case: it guards the rules
/// that regenerate the bridges — `tmp/bridges.rules`, `tmp/bridges` and the two
/// externally-curated `bridge/uberon-bridge-to-*.owl` — and under `BRI=false`
/// none of them exists and the committed `bridge/*.owl` are used as they stand.
/// That is "refresh this group or keep it", the same question the other three
/// answer, so it is recorded as the same kind of answer.
///
/// The default is what the configuration's own value says: UBERON sets
/// `BRI = true`, so an ordinary build regenerates and only a run that says
/// `BRI=false` keeps.
///
/// A switch that guards no target is not recorded — `GH_ACTION` selects between
/// two values of a variable, so there is nothing for a run to pin, and a group
/// with no targets is plan content that does nothing.
fn switch_groups(
    make: &super::makefile::MakeModel,
    artefacts: &[ArtefactPlan],
    prerequisites: &[ArtefactPlan],
    already: &[crate::plan::RefreshGroup],
) -> Vec<crate::plan::RefreshGroup> {
    use crate::plan::{Freshness, RefreshGroup};
    let mut out: Vec<RefreshGroup> = Vec::new();
    for flag in &make.switch_vars {
        if already.iter().any(|g| &g.flag == flag) {
            continue;
        }
        let guarded = |a: &ArtefactPlan| {
            let name = a.target.strip_prefix("src/ontology/").unwrap_or(&a.target);
            make.rule_for(&a.target)
                .or_else(|| make.rule_for(name))
                .is_some_and(|(r, _)| r.guards.iter().any(|g| g == flag))
        };
        let targets: Vec<String> = artefacts
            .iter()
            .chain(prerequisites.iter())
            .filter(|a| guarded(a))
            .map(|a| a.target.clone())
            .collect();
        if targets.is_empty() {
            continue;
        }
        let configured = make.cond_vars.get(flag).map(String::as_str).unwrap_or("");
        let default = match configured.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Freshness::Rebuild,
            _ => Freshness::Keep,
        };
        out.push(RefreshGroup { name: switch_group_name(flag), flag: flag.clone(), targets, default });
    }
    out
}

/// What a switch's group is CALLED. The abbreviations are the build
/// configuration's; the group name is what a person types, so the three whose
/// meaning is established are spelled out and anything else keeps its own name,
/// lowercased. (`MIR`, `IMP` and `PAT` never reach here — their groups are built
/// above, from more than the guards alone.)
///
/// A name in this table is not a promise that the group EXISTS. A switch earns a
/// group by gating a rule, and `IMP_LARGE` is the case where none is earned:
/// UBERON assigns it and passes it to sub-makes, but no conditional consults it
/// and no recipe expands it, so there is nothing in that repository for a group
/// to cover and none is recorded. `IMP_LARGE=false` there reaches recipe
/// environments and changes nothing owlmake does — which is what it changes for
/// the reference build too, whose meaning lives in the NAME of
/// `refresh-imports-excluding-large` and reaches owlmake that way. A repository
/// that does gate rules on it gets a `large-imports` group like any other switch.
fn switch_group_name(flag: &str) -> String {
    match flag {
        "BRI" => "bridges".to_string(),
        "COMP" => "components".to_string(),
        "IMP_LARGE" => "large-imports".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

/// The repo's `owl:imports` catalog, if it has one. ODK's filename, applied at
/// plan time so execution has a NAME rather than a convention to re-derive.
fn catalog_file(dir: &std::path::Path) -> Option<String> {
    dir.join("catalog-v001.xml")
        .is_file()
        .then(|| "catalog-v001.xml".to_string())
}

/// The query the ODK runs over the COMMITTED `definitions.owl` to seed the
/// import when patterns are not regenerated (`PAT=false`).
///
/// Read from the rule that branch defines for `$(TMPDIR)/all_pattern_terms.txt`
/// — the else half of `ifeq ($(PAT),true)`, which ingest's own parse (every flag
/// TRUE) never sees.
fn cached_pattern_seed_query(make: &super::makefile::MakeModel) -> Option<String> {
    let base = make.base_dir.as_ref()?;
    let main_mk = base.join("Makefile");
    let mut alt =
        super::makefile::MakeModel::parse_file_with_flags(&main_mk, &[], &[("PAT", "false")])
            .ok()?;
    alt.base_dir = Some(base.clone());
    let id = alt.expand("$(ONT)").trim().to_string();
    if !id.is_empty() {
        let over = base.join(format!("{id}.Makefile"));
        if over.exists() {
            alt.overlay_file(&over).ok()?;
        }
    }
    let tmpdir = {
        let d = alt.expand("$(TMPDIR)").trim().to_string();
        if d.is_empty() { "tmp".to_string() } else { d }
    };
    let rule = alt.rules.get(&format!("{tmpdir}/all_pattern_terms.txt"))?;
    // `$(ROBOT) query --use-graphs true -f csv -i $< --query $(SPARQLDIR)/terms.sparql $@`
    for line in &rule.recipe {
        let expanded = alt.expand(line);
        let toks: Vec<&str> = expanded.split_whitespace().collect();
        if let Some(i) = toks.iter().position(|t| *t == "--query" || *t == "-q") {
            if let Some(q) = toks.get(i + 1) {
                return Some(q.to_string());
            }
        }
    }
    None
}

/// Enumerate the DOSDP pattern set, once, at plan time.
///
/// `patterns` is what gets BUILT — a template paired with a data table, which is
/// ODK's `DOSDP_PATTERN_NAMES_DEFAULT = $(patsubst %.tsv,%,$(notdir $(wildcard
/// $(PATTERNDIR)/data/default/*.tsv)))`. `templates` is every YAML in the pattern
/// directory, which is a SUPERSET: `dosdp validate` covers templates that have no
/// data yet, and recording only the paired set would let validation check a
/// subset while reporting success.
fn plan_dosdp(
    dir: &std::path::Path,
    pattern_dir: &str,
    make: Option<&super::makefile::MakeModel>,
    ontbase: &str,
    version: &str,
) -> Option<crate::spec::DosdpSpec> {
    use crate::spec::{DosdpPattern, DosdpSpec};
    let root = dir.join(pattern_dir);
    let tpl_dir = root.join("dosdp-patterns");
    let data_dir = root.join("data").join("default");
    if !tpl_dir.is_dir() {
        return None;
    }
    let rel = |p: &std::path::Path| -> String {
        p.strip_prefix(dir).unwrap_or(p).to_string_lossy().replace('\\', "/")
    };
    let mut templates: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&tpl_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "yaml" || x == "yml") {
                templates.push(rel(&p));
            }
        }
    }
    templates.sort();

    // The data directories the generator is run over, in the order
    // `definitions.owl` merges them, each with the options of ITS invocation. A
    // repo with no `definitions.owl` rule to read has the stock single pipeline
    // over `data/default`.
    let pipelines = make
        .map(|m| dosdp_pipelines(m, dir, pattern_dir))
        .filter(|p: &Vec<(std::path::PathBuf, DosdpGenerateOptions)>| !p.is_empty())
        .unwrap_or_else(|| vec![(data_dir.clone(), DosdpGenerateOptions::default())]);

    let mut patterns: Vec<DosdpPattern> = Vec::new();
    for (data_dir, opts) in &pipelines {
        let mut batch: Vec<DosdpPattern> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(data_dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("tsv") {
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
                let tpl = tpl_dir.join(format!("{stem}.yaml"));
                if tpl.is_file() {
                    batch.push(DosdpPattern {
                        name: stem.to_string(),
                        template: rel(&tpl),
                        data: rel(&p),
                        restrict_axioms: opts.restrict_axioms.clone(),
                        restrict_axioms_column: opts.restrict_axioms_column.clone(),
                        add_axiom_source_annotation: opts.add_axiom_source_annotation,
                        axiom_source_annotation_property: opts
                            .axiom_source_annotation_property
                            .clone(),
                        generate_defined_class: opts.generate_defined_class,
                    });
                }
            }
        }
        batch.sort_by(|a, b| a.name.cmp(&b.name));
        patterns.extend(batch);
    }

    let prefixes = ["config/prefixes.yaml"]
        .into_iter()
        .filter(|f| dir.join(f).is_file())
        .map(str::to_string)
        .collect();

    let output = rel(&root.join("definitions.owl"));
    let steps = dosdp_merge_steps(make, pattern_dir, ontbase, version);

    Some(DosdpSpec {
        output,
        prefixes,
        cached_seed_query: make.and_then(cached_pattern_seed_query),
        patterns,
        steps,
        templates,
    })
}

/// The data directories `definitions.owl` is merged from, each paired with the
/// options of the `generate` invocation that fills it.
///
/// Read off the `definitions.owl` rule's own prerequisites — `$(DOSDP_OWL_FILES_*)`,
/// already expanded — rather than off every rule that mentions the generator: a
/// repository may run it for something else entirely (HPO's
/// `tmp/norm_patterns.ofn` is a curation pipeline over `tmp/norm_patterns`), and
/// those products are not part of the release.
fn dosdp_pipelines(
    make: &super::makefile::MakeModel,
    dir: &Path,
    pattern_dir: &str,
) -> Vec<(std::path::PathBuf, DosdpGenerateOptions)> {
    let mut out: Vec<(std::path::PathBuf, DosdpGenerateOptions)> = Vec::new();
    let Some(rule) = make.rules.get(&format!("{pattern_dir}/definitions.owl")) else {
        return out;
    };
    for pre in &rule.prereqs {
        for tok in make.expand(pre).split_whitespace() {
            if !tok.ends_with(".ofn") {
                continue;
            }
            let Some(parent) = Path::new(tok).parent() else { continue };
            let data_dir = dir.join(parent);
            if out.iter().any(|(d, _)| d == &data_dir) {
                continue;
            }
            // The options are the ones the rule that WRITES this module passes.
            let opts = make
                .rules
                .get(tok)
                .map(|r| dosdp_generate_options(make, &[r]))
                .unwrap_or_default();
            out.push((data_dir, opts));
        }
    }
    out
}

/// What the repo does to the merged per-pattern products, recorded from its
/// `definitions.owl` rule.
///
/// The generator writes one `.ofn` per pattern and the rule merges them; every
/// step AFTER that merge is the repo's own, and the plan has to carry it. OBA's
/// rule inserts `query --update ../sparql/postprocess-definitions.ru` between the
/// merge and the `annotate`s, and the update rewrites ~1,900 definitions.
///
/// The leading `merge` is not recorded: its inputs are `$^`, the products this
/// very step generates, so execution merges what it just built rather than a
/// frozen file list. The closing `-o definitions.ofn && mv definitions.ofn $@` is
/// not recorded either — writing the output is what the caller does with the
/// model it gets back.
///
/// With no Makefile to read (an edit-only repo), the two IRI stamps are all there
/// is, and they are written into the plan HERE so that execution has one path,
/// not a recorded one and a derived one.
fn dosdp_merge_steps(
    make: Option<&super::makefile::MakeModel>,
    pattern_dir: &str,
    ontbase: &str,
    version: &str,
) -> Vec<crate::spec::StepEntry> {
    use crate::plan::step::{AnnotateSpec, Op, Step};
    let recorded = make.and_then(|m| {
        let rule = m.rules.get(&format!("{pattern_dir}/definitions.owl"))?;
        let robot_prefix = {
            let p = m.expand("$(ROBOT)");
            if p.trim().is_empty() { "robot".to_string() } else { p.trim().to_string() }
        };
        let mut steps: Vec<Step> = Vec::new();
        for line in &rule.recipe {
            steps.extend(recorded_steps(&m.expand(line), &robot_prefix));
        }
        // The merge the recipe opens with, and the output bookkeeping it closes
        // with, are the caller's business.
        if matches!(steps.first(), Some(Step::Op(Op::Merge { .. }))) {
            steps.remove(0);
        }
        while matches!(steps.last(), Some(Step::File(_)) | Some(Step::Op(Op::RoundTrip { .. }))) {
            steps.pop();
        }
        (!steps.is_empty()).then_some(steps)
    });
    let steps = recorded.unwrap_or_else(|| {
        vec![
            Step::Op(Op::Annotate(AnnotateSpec {
                ontology_iri: Some(format!("{ontbase}/patterns/definitions.owl")),
                ..Default::default()
            })),
            Step::Op(Op::Annotate(AnnotateSpec {
                version_iri: Some(format!(
                    "{ontbase}/releases/{version}/patterns/definitions.owl"
                )),
                annotations: vec![(
                    "http://www.w3.org/2002/07/owl#versionInfo".to_string(),
                    version.to_string(),
                )],
                ..Default::default()
            })),
        ]
    });
    steps.iter().map(crate::spec::StepEntry::from_step).collect()
}

/// The pattern-generator options a repo's own `generate` rule passes, read off
/// that recipe. ODK spells them in `dosdp_tools_options` and bakes them into the
/// `$(DOSDP_OWL_FILES_DEFAULT)` recipe, which is where they are unambiguous —
/// already expanded, and attached to the invocation that actually runs.
#[derive(Default)]
struct DosdpGenerateOptions {
    restrict_axioms: Option<String>,
    restrict_axioms_column: Option<String>,
    add_axiom_source_annotation: bool,
    axiom_source_annotation_property: Option<String>,
    generate_defined_class: bool,
}

fn dosdp_generate_options(
    make: &super::makefile::MakeModel,
    rules: &[&super::makefile::Rule],
) -> DosdpGenerateOptions {
    let mut out = DosdpGenerateOptions::default();
    let mut rules: Vec<&super::makefile::Rule> = rules.to_vec();
    // Deterministic: `rules` may come from a HashMap, and two rules could both
    // invoke the generator.
    rules.sort_by(|a, b| a.targets.cmp(&b.targets));
    for rule in rules {
        for line in &rule.recipe {
            let expanded = make.expand(line);
            if !expanded.contains(" generate") || !expanded.contains("dosdp") {
                continue;
            }
            let toks: Vec<String> = super::robot::tokenize(&expanded);
            let mut it = toks.iter().peekable();
            while let Some(t) = it.next() {
                let (name, inline): (&str, Option<String>) = match t.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_string())),
                    None => (t.as_str(), None),
                };
                let mut value = || {
                    inline.clone().or_else(|| it.next().cloned())
                };
                let truthy = |v: Option<String>| {
                    v.as_deref().map(|s| !matches!(s, "false" | "0")).unwrap_or(true)
                };
                match name {
                    "--restrict-axioms-to" => out.restrict_axioms = value(),
                    "--restrict-axioms-column" => out.restrict_axioms_column = value(),
                    "--add-axiom-source-annotation" => {
                        out.add_axiom_source_annotation = truthy(value())
                    }
                    "--axiom-source-annotation-property" => {
                        out.axiom_source_annotation_property = value()
                    }
                    "--generate-defined-class" => out.generate_defined_class = truthy(value()),
                    _ => {}
                }
            }
        }
    }
    out
}

/// The import modules owlmake builds NATIVELY: each `ImportPlan`'s own product,
/// and the merged import. `build_imports_fresh` extracts the ⊥-module over
/// `import_seed` (the committed `*_terms.txt` plus the edit signature plus the
/// DOSDP pattern terms) and applies the recorded `exclude_iri_patterns` — so
/// replaying the `merged_import.owl` recipe as well is a second implementation of
/// the same product, and the one that wins is whichever runs last. Two
/// implementations racing over one artefact is not a build anyone can reason
/// about, so only one of them is kept.
pub(crate) fn native_import_targets(
    make: &super::makefile::MakeModel,
    imports: &[ImportPlan],
    merged_import: Option<&str>,
) -> std::collections::HashSet<String> {
    let mut out: std::collections::HashSet<String> = Default::default();
    let mut add = |t: String| {
        if let Some(rest) = t.strip_prefix("../") {
            out.insert(format!("src/{rest}"));
        }
        out.insert(format!("src/ontology/{t}"));
        out.insert(t);
    };
    // The per-import `<id>_import.owl` rules stay: they carry the
    // `<id>_terms*.txt` prerequisites into the plan, and dropping the modules
    // orphans those rules, leaving the native build without its term files.
    //
    // The MERGED import does NOT. Its rule is a second implementation of what
    // `build_imports_fresh` already does, and the two do not agree — replaying
    // ECTO's recipe writes a visibly larger module than the seed-driven native
    // build. Excluding it leaves the plan naming a merged import with no target
    // that produces it, and that gap is closed by NAME: `cmd::make::classify` maps
    // the merged import's own path to `all_imports`, so `om make
    // imports/merged_import.owl` runs the native builder and `om make all_imports
    // IMP=true` builds the one product ODK's `$(IMPORT_ROOTS)` names.
    //
    // …EXCEPT where the recipe is not a second implementation but the ONLY one.
    // MONDO extracts its merged import from `mirror/merged.owl` — every mirror
    // merged, then `remove --axioms equivalent --preserve-structure false` — over
    // `merged_terms_combined.txt`. The native builder does neither: it seeds from
    // the committed `*_terms.txt` plus the edit signature, a far wider term set,
    // and never drops the equivalences, so it would write a module several times
    // the size of the one MONDO releases. Only the repo's own recipe expresses
    // that module, so the test is what the rule READS: a merged mirror means the
    // repo builds its module from the whole merged upstream.
    //
    // Determinism caveat: replaying that recipe is byte-stable for a GIVEN set of
    // mirrors, not across refreshes of them. Re-fetch the mirrors and the module
    // comes back with the same content but with its handful of ontology-level
    // `Annotation(dc:source …)` lines in a different order, so a release diff over
    // a mirror refresh shows those lines moving.
    let _ = imports;
    if let Some(m) = merged_import {
        if !reads_merged_mirror(make, m) {
            add(m.to_string());
        }
    }
    out
}

/// Does `target`'s rule read `$(MIRRORDIR)/merged.owl`? — see
/// `native_import_targets`.
fn reads_merged_mirror(make: &super::makefile::MakeModel, target: &str) -> bool {
    let mirrordir = mirror_dir(make);
    let merged = format!("{mirrordir}/merged.owl");
    for name in [target.to_string(), format!("imports/{}", Path::new(target).file_name().and_then(|s| s.to_str()).unwrap_or(target))] {
        if let Some((rule, _)) = make.rule_for(&name) {
            if rule.prereqs.iter().any(|p| p.trim() == merged) {
                return true;
            }
        }
    }
    false
}

/// The repo's mirror directory, spelled ONE way.
///
/// `./mirror` and `mirror` name the same directory, and the plan has to settle on
/// one: writing a path rebases it to the plan file's directory and reading rebases
/// it back, so an un-normalised `MIRRORDIR` gives a repo two names for one target —
/// one from the build configuration, one from the committed plan. A target has a
/// single name whichever the build reads.
fn mirror_dir(make: &super::makefile::MakeModel) -> String {
    let d = make.expand("$(MIRRORDIR)");
    let d = d.trim();
    let d = d.strip_prefix("./").unwrap_or(d).trim_end_matches('/');
    if d.is_empty() { "mirror".to_string() } else { d.to_string() }
}

/// The mirror targets the executor serves natively, in the plan's own spelling.
///
/// A mirror is `ImportPlan::source` plus `ImportPlan::mirror_steps`, so the
/// `mirror-<id>` phony and the `$(MIRRORDIR)/%.owl` copy rule are not recorded as
/// rules of their own — recording them too would be a second implementation of
/// the same thing, and would let a `MIR=false` build re-download every mirror and
/// overwrite the pinned copies.
///
/// They are still PRODUCED, so a rule that names one as a prerequisite is not a
/// gap: `imports/<id>_import.owl: mirror/<id>.owl` is every ODK import rule, and
/// treating that prerequisite as unbuildable would make the whole import chain
/// unrunnable from the plan.
pub(crate) fn native_mirror_targets(
    make: &super::makefile::MakeModel,
    imports: &[ImportPlan],
) -> std::collections::HashSet<String> {
    let mirrordir = mirror_dir(make);
    imports
        .iter()
        .flat_map(|i| [format!("mirror-{}", i.id), format!("{mirrordir}/{}.owl", i.id)])
        .collect()
}

/// The pattern products owlmake builds NATIVELY, so they are not plan content.
///
/// owlmake has its own DOSDP engine (`crate::dosdp`) and `PatternsMode` — ODK
/// `PAT` — is the switch that drives it. Ingesting the ODK's rules for these would
/// make the build depend on which side of `ifeq ($(PAT),true)` was parsed rather
/// than on the native option, which is the mirror plumbing's failure mode too.
///
/// Both spellings are registered: rules name these relative to `src/ontology`
/// (`../patterns/…`) while the plan records them repo-relative (`src/patterns/…`).
pub(crate) fn native_pattern_targets(
    make: &super::makefile::MakeModel,
    ontology_dir: &std::path::Path,
) -> std::collections::HashSet<String> {
    let dir = |var: &str, dflt: &str| {
        let d = make.expand(var);
        let d = d.trim().to_string();
        if d.is_empty() { dflt.to_string() } else { d }
    };
    let patterndir = dir("$(PATTERNDIR)", "../patterns");
    let tmpdir = dir("$(TMPDIR)", "tmp");
    // A repo with no pattern directory runs no DOSDP pipeline and claims none of
    // its targets: every path the plan names is a file the build can produce, and
    // `--list-targets` offers only what is buildable.
    if !ontology_dir.join(&patterndir).is_dir() {
        return Default::default();
    }
    let mut out: std::collections::HashSet<String> = Default::default();
    let mut add = |t: String| {
        if let Some(rest) = t.strip_prefix("../") {
            out.insert(format!("src/{rest}"));
        }
        out.insert(format!("src/ontology/{t}"));
        out.insert(t);
    };
    add(format!("{patterndir}/definitions.owl"));
    // `pattern.owl` is owlmake's to write natively only while the repo still
    // builds it with the stock rule, a `prototype` expansion over the pattern
    // directory. uPheno OVERRIDES that rule — `merge … remove … reason … reduce
    // … annotate` over a DIFFERENT template directory (`patterns/dosdp-dev`),
    // and outside the `PAT` conditional, so it runs whether or not patterns are
    // regenerated. Claiming it left the plan with no rule for it, and the
    // COMMITTED file shipped as the release artefact: 76 lines out, still
    // carrying the previous release's version IRI.
    let pattern_owl = format!("{patterndir}/pattern.owl");
    let built_by_dosdp = match make.rules.get(&pattern_owl) {
        None => true,
        Some(r) => r.recipe.iter().any(|l| make.expand(l).contains("dosdp-tools")),
    };
    if built_by_dosdp {
        add(pattern_owl);
    }
    add(format!("{tmpdir}/pattern_owl_seed.txt"));
    add(format!("{tmpdir}/all_pattern_terms.txt"));
    let data: Vec<String> = make
        .rules
        .keys()
        // Any data directory the repo runs the generator over, not a fixed pair
        // of names: `definitions.owl` merges one batch per directory.
        .filter(|t| {
            t.starts_with(&format!("{patterndir}/data/"))
                && (t.ends_with(".ofn") || t.ends_with(".txt"))
        })
        .cloned()
        .collect();
    for t in data {
        add(t);
    }
    out
}

/// Walk the prerequisite graph of every artefact and import, and plan each
/// generated file that has a rule of its own — the ODK's `all_robot_plugins`,
/// `tmp/simple_seed.txt`, `subsets/<x>-tags.ofn`, generated SSSOM components, and
/// so on. Discovering and replaying these from the Makefile *during* execution
/// would mean `owlmake.json` did not describe the build it claims to, and would
/// leave a prerequisite that fails to build as a warning the run carries on past
/// — which is how CL comes to ship `cl-simple.owl`/`cl-basic.owl` built without
/// their filter seed.
///
/// Returned in dependency order — deepest first — so executing the list front to
/// back satisfies every dependency before it is needed. Targets that are already
/// artefacts are skipped (they are built by the artefact loop), as are phony
/// targets with no recipe.
fn plan_prerequisites(
    repo: &OdkRepo,
    make: &super::makefile::MakeModel,
    robot_prefix: &str,
    artefacts: &[ArtefactPlan],
    imports: &[ImportPlan],
    merged_import: Option<&str>,
) -> Vec<ArtefactPlan> {
    use std::collections::HashSet;

    let is_artefact: HashSet<&str> = artefacts.iter().map(|a| a.target.as_str()).collect();
    let mut planned: HashSet<String> = HashSet::new();
    let mut out: Vec<ArtefactPlan> = Vec::new();
    // A pathological or cyclic graph must not spin: bound total work, and bound
    // recursion depth.
    let mut budget = 4000usize;

    // The ODK's mirror PLUMBING is not plan content. A mirror is `source` plus
    // `mirror_steps`, both recorded on the ImportPlan, and the executor fetches
    // and processes it natively — so the `mirror-<id>` phony, its
    // `$(TMPDIR)/<id>-download.owl`, and the `$(MIRRORDIR)/%.owl` copy rule would
    // be a SECOND implementation of the same thing. Keeping both would let a
    // `MIR=false` build re-download every mirror and overwrite the pinned copies:
    // the native path pins them, replayed rules do not.
    let mut native: HashSet<String> = native_mirror_targets(make, imports);

    native.extend(native_pattern_targets(make, &repo.dir));
    native.extend(native_import_targets(make, imports, merged_import));

    // Post-order DFS: a target is pushed only after everything it needs.
    fn visit(
        repo: &OdkRepo,
        make: &super::makefile::MakeModel,
        robot_prefix: &str,
        target: &str,
        depth: usize,
        is_artefact: &HashSet<&str>,
        native: &HashSet<String>,
        planned: &mut HashSet<String>,
        out: &mut Vec<ArtefactPlan>,
        budget: &mut usize,
    ) {
        // Served natively from the ImportPlan — see the note where `native` is built.
        if native.contains(target) {
            return;
        }
        if depth == 0 || *budget == 0 || planned.contains(target) || is_artefact.contains(target) {
            if std::env::var("OM_PLAN_DEBUG").is_ok() {
                eprintln!(
                    "plan-skip:  {target}  depth0={} budget0={} already={} artefact={}",
                    depth == 0,
                    *budget == 0,
                    planned.contains(target),
                    is_artefact.contains(target)
                );
            }
            return;
        }
        *budget -= 1;
        planned.insert(target.to_string());
        let Some((rule, stem)) = make.rule_for(target) else { return };
        let prereqs = expanded_prereqs(repo, rule, stem.as_deref(), target);
        for pre in &prereqs {
            visit(repo, make, robot_prefix, pre, depth - 1, is_artefact, native, planned, out, budget);
        }
        // A rule with no recipe still gets an entry. It contributes nothing to
        // RUN — `qc: sparql_test all_reports …` is pure grouping, and its
        // prerequisites have already been visited above — but the plan is the
        // only way to NAME a target now, and one that is not written down cannot
        // be asked for: without the entry `om qc` fails with "no rule to make
        // target" while the four targets it groups are all present.
        let planned_ok = plan_rule(repo, make, robot_prefix, target, false);
        if std::env::var("OM_PLAN_DEBUG").is_ok() {
            eprintln!(
                "plan-visit: {target}  prereqs={prereqs:?}  plan_rule={}",
                if planned_ok.is_some() { "Some" } else { "None" }
            );
        }
        if let Some(p) = planned_ok {
            out.push(p);
        }
    }

    // A mirror rule's own prerequisites are roots too. `mirror-<id>` itself is
    // native plumbing (the import carries it as `mirror_steps`), so the DFS never
    // descends through it — and MONDO's `mirror-ncbigene` needs
    // `../sparql/construct/construct-ncbigene.sparql`, a GENERATED file whose rule
    // (`awk` the ncbigene terms out of `merged_terms_combined.txt` into a query
    // template) is reachable from nowhere else. Without this the plan would record
    // that the mirror reads the file but not how to make it, and the refresh would
    // die on `No such file or directory`.
    let mirror_roots: Vec<String> = imports
        .iter()
        .flat_map(|i| [format!("mirror-{}", i.id), format!("mirror/{}.owl", i.id)])
        .filter_map(|t| make.rule_for(&t).map(|(r, s)| (t, r, s)))
        .flat_map(|(t, rule, stem)| expanded_prereqs(repo, rule, stem.as_deref(), &t))
        .collect();

    let roots = artefacts
        .iter()
        .map(|a| a.target.clone())
        .chain(imports.iter().map(|i| i.output.clone()))
        .chain(mirror_roots)
        .collect::<Vec<_>>();
    for root in roots {
        let Some((rule, stem)) = make.rule_for(&root) else { continue };
        for pre in expanded_prereqs(repo, rule, stem.as_deref(), &root) {
            visit(
                repo,
                make,
                robot_prefix,
                &pre,
                64,
                &is_artefact,
                &native,
                &mut planned,
                &mut out,
                &mut budget,
            );
        }
    }

    // Then every other explicit rule the repo defines, so that running one
    // (`om test`, `om sparql_test`, a project's own `greet`) needs only the plan.
    // Without this the plan would describe just the release path, and every other
    // target would become unrunnable the moment the Makefile is deleted — the
    // executor has no second source to fall back on, by design.
    // `.PHONY`, `.PRECIOUS`, `.SECONDARY`, … declare things ABOUT targets; they
    // are not targets themselves, and recording one would produce a plan entry
    // whose "prerequisites" were the phony list.
    let mut others: Vec<&String> =
        make.rules.keys().filter(|t| !t.starts_with('.')).collect();
    others.sort();
    for target in others {
        visit(
            repo,
            make,
            robot_prefix,
            target,
            64,
            &is_artefact,
            &native,
            &mut planned,
            &mut out,
            &mut budget,
        );
    }

    // …and every pattern rule, instantiated at the stems its prerequisites
    // admit. A pattern rule offers a FAMILY of named targets (`dump_<id>`,
    // `<id>_terms_in_src`), and an instance is buildable whenever its
    // substituted prerequisites can be produced — so the plan names those
    // instances, or the whole family is unrunnable from the plan alone. Stems
    // are discovered by matching each rule's `%`-bearing prerequisites against
    // the repo's files and the mirrors the plan itself fetches; an instance is
    // recorded only when every prerequisite resolves — an existing file, a
    // plan-fetched mirror, or a target some rule can build, recursively. A
    // pattern rule with no `%`-bearing prerequisite is skipped: its stem
    // universe is unbounded, so there is no set of instances to name.
    {
        use std::collections::BTreeSet;
        let mirror_names: HashSet<String> =
            imports.iter().map(|i| format!("mirror/{}.owl", i.id)).collect();
        let mut candidates: BTreeSet<String> = BTreeSet::new();
        for r in &make.pattern_rules {
            for tp in &r.targets {
                if !tp.contains('%') {
                    continue;
                }
                for w in expanded_prereqs(repo, r, Some("%"), tp) {
                    if !w.contains('%') {
                        continue;
                    }
                    candidates.extend(glob_stems(&repo.dir, &w));
                    for m in &mirror_names {
                        if let Some(stem) = super::makefile::match_pattern(&w, m) {
                            candidates.insert(stem);
                        }
                    }
                }
            }
        }
        fn satisfiable(
            repo: &OdkRepo,
            make: &super::makefile::MakeModel,
            mirrors: &HashSet<String>,
            target: &str,
            depth: usize,
            budget: &mut usize,
        ) -> bool {
            if *budget == 0 || depth == 0 {
                return false;
            }
            *budget -= 1;
            if mirrors.contains(target) || repo.dir.join(target).exists() {
                return true;
            }
            let Some((rule, stem)) = make.rule_for(target) else { return false };
            expanded_prereqs(repo, rule, stem.as_deref(), target)
                .iter()
                .all(|p| satisfiable(repo, make, mirrors, p, depth - 1, budget))
        }
        let mut instances: BTreeSet<String> = BTreeSet::new();
        for r in &make.pattern_rules {
            for tp in &r.targets {
                if !tp.contains('%') {
                    continue;
                }
                let wild = expanded_prereqs(repo, r, Some("%"), tp)
                    .iter()
                    .any(|p| p.contains('%'));
                if !wild {
                    continue;
                }
                for stem in &candidates {
                    let concrete = tp.replace('%', stem);
                    if make.rules.contains_key(&concrete)
                        || planned.contains(&concrete)
                        || is_artefact.contains(concrete.as_str())
                        || native.contains(&concrete)
                    {
                        continue;
                    }
                    let Some((rule, rstem)) = make.rule_for(&concrete) else { continue };
                    let mut fuel = 512usize;
                    let ok = expanded_prereqs(repo, rule, rstem.as_deref(), &concrete)
                        .iter()
                        .all(|p| satisfiable(repo, make, &mirror_names, p, 12, &mut fuel));
                    if ok {
                        instances.insert(concrete);
                    }
                }
            }
        }
        for t in instances {
            visit(
                repo,
                make,
                robot_prefix,
                &t,
                64,
                &is_artefact,
                &native,
                &mut planned,
                &mut out,
                &mut budget,
            );
        }
    }
    out
}

/// The stems under which a `%`-bearing path pattern matches an existing file:
/// the pattern's directory is scanned and each entry matching the prefix and
/// suffix around the `%` yields its stem. A pattern whose `%` would have to
/// span a directory separator matches nothing.
fn glob_stems(dir: &Path, pattern: &str) -> Vec<String> {
    let Some((pre, suf)) = pattern.split_once('%') else { return Vec::new() };
    if suf.contains('/') {
        return Vec::new();
    }
    let cut = pre.rfind('/').map(|i| i + 1).unwrap_or(0);
    let scan = dir.join(&pre[..cut]);
    let name_pre = &pre[cut..];
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(scan) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.len() > name_pre.len() + suf.len()
                && name.starts_with(name_pre)
                && name.ends_with(suf)
            {
                out.push(name[name_pre.len()..name.len() - suf.len()].to_string());
            }
        }
    }
    out.sort();
    out
}

/// Inspect the Makefile rule that builds a declared `component` (and its
/// transitive prerequisites) for recipe steps owlmake can't reproduce, returning
/// distinct gap descriptions. The ODK yaml lists a bare filename, but the rule
/// target is usually `$(COMPONENTSDIR)/<filename>`; we try the common forms. The
/// walk is bounded so a pathological dependency graph can't blow up.
fn component_build_gaps(make: &super::makefile::MakeModel, robot_prefix: &str, filename: &str) -> Vec<String> {
    use std::collections::{HashSet, VecDeque};
    let compdir = make.expand("$(COMPONENTSDIR)");
    let compdir = compdir.trim();
    let candidates = [
        filename.to_string(),
        format!("components/{filename}"),
        if compdir.is_empty() { String::new() } else { format!("{compdir}/{filename}") },
    ];
    let Some(start) = candidates
        .iter()
        .find(|t| !t.is_empty() && make.rule_for(t.as_str()).is_some())
        .cloned()
    else {
        return Vec::new();
    };

    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(start.clone());
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(start);
    let mut gaps: Vec<String> = Vec::new();
    let mut budget = 2000usize;
    while let Some(target) = queue.pop_front() {
        let Some((rule, stem)) = make.rule_for(&target) else { continue };
        let mut autos = Autos::default();
        autos.set("@", &target);
        let prereqs: Vec<String> = match &stem {
            Some(s) => rule.prereqs.iter().map(|p| p.replace('%', s)).collect(),
            None => rule.prereqs.clone(),
        };
        if let Some(first) = prereqs.first() {
            autos.set("<", first);
        }
        autos.set("^", &prereqs.join(" "));
        if let Some(s) = &stem {
            autos.set("*", s);
        }
        // As in `plan_rule`: an import module's own recipe runs only when owlmake
        // has already decided to build imports, so its `$(IMP)`/`$(MIR)` guard is
        // answered by that decision, not by the plan.
        for line in &rule.recipe {
            if budget == 0 {
                break;
            }
            budget -= 1;
            // Same `$(eval VAR := VALUE)` handling as `plan_rule`: consume the
            // assignment rather than expanding it. Expanding it instead reports
            // `eval` as an unimplemented function and refuses the whole plan —
            // UBERON's fourteen `$(SUBSETCMD)` subsets set `TERM_ID` this way.
            if let Some((name, value)) = parse_eval_assignment(line.trim(), make, &autos) {
                autos.set(&name, &value);
                continue;
            }
            let expanded = make.expand_with(line, &autos);
            for step in recorded_steps(&expanded, robot_prefix) {
                for g in step.gaps() {
                    if !gaps.contains(&g) {
                        gaps.push(g);
                    }
                }
            }
        }
        for p in prereqs {
            if seen.len() < 500 && !seen.contains(&p) && make.rule_for(&p).is_some() {
                seen.insert(p.clone());
                queue.push_back(p);
            }
        }
    }
    gaps
}

/// Collapse a directory-producing release rule into direct artefacts.
///
/// A repo can build its release variants through a rule that writes a whole
/// directory — `oort: $(SRC); ontology-release-runner … --outdir oort` — whose
/// products are then pulled out by `<id>-simple.owl: oort; cp
/// oort/<id>-simple.owl <id>-simple.owl` rules. owlmake has no directory
/// artefacts, so each such consumer is rewritten into a single [`Op::Oort`] built
/// directly from the source ontology, and the now-absorbed `oort` target is
/// dropped. The variant (main/relaxed/simple) is read from the consumer's own
/// filename suffix.
fn rewrite_oort(artefacts: &mut Vec<ArtefactPlan>, id: &str, version: &str, ontbase: &str) {
    use crate::cmd::oort::Variant;
    use super::robot::{AnnotateSpec, Op, OortSpec, RemoveSpec};
    use std::collections::HashMap;

    let mut oort_targets: HashMap<String, OortSpec> = HashMap::new();
    for a in artefacts.iter() {
        for s in &a.steps {
            if let Step::Oort(spec) = s {
                oort_targets.insert(a.target.clone(), spec.clone());
            }
        }
    }
    if oort_targets.is_empty() {
        return;
    }

    for a in artefacts.iter_mut() {
        let Some(spec) = a.input.as_ref().and_then(|i| oort_targets.get(i)) else {
            continue;
        };
        let variant = Variant::classify(&a.target);
        let reasoner = if spec.reasoner.is_empty() { "ELK".to_string() } else { spec.reasoner.clone() };

        // Each variant is an ordinary op sequence: resolve the import closure,
        // relax equivalence definitions into existentials, assert inferred
        // subsumptions (no owl:Thing tautologies; equivalent named pairs allowed),
        // drop redundant subclass axioms. Relaxed/simple then remove equivalence
        // axioms; simple additionally keeps only native ID-space classes.
        let mut steps = vec![
            Step::Op(Op::Merge { inputs: vec![], collapse_import_closure: None }),
            Step::Op(Op::Relax { include_subclass_of: false }),
            Step::Op(Op::Reason {
                reasoner: Some(reasoner),
                equivalent_classes_allowed: Some("all".into()),
                exclude_tautologies: Some("structural".into()),
                annotate_inferred_axioms: None,
                allow_incoherent: None,
                exclude_external_entities: None,
                exclude_owl_thing: None,
                remove_redundant_subclass_axioms: None,
                create_new_ontology: None,
                create_new_ontology_with_annotations: None,
                exclude_duplicate_axioms: None,
            }),
            Step::Op(Op::Reduce { reasoner: None, include_subproperties: None }),
        ];
        if matches!(variant, Variant::Relaxed | Variant::Simple) {
            steps.push(Step::Op(Op::Remove(RemoveSpec {
                axioms: vec!["equivalent".into()],
                ..Default::default()
            })));
        }
        if variant == Variant::Simple {
            steps.push(Step::Op(Op::SimpleSubset { ont_id: id.to_string() }));
        }
        steps.push(Step::Op(Op::Annotate(AnnotateSpec {
            ontology_iri: Some(format!("{ontbase}/{}", a.target)),
            version_iri: Some(format!("{ontbase}/releases/{version}/{}", a.target)),
            ..Default::default()
        })));

        a.steps = steps;
        a.input = Some(spec.input.clone());
        a.gaps.clear();
    }

    // Drop the now-absorbed `oort` directory target(s): their products are built
    // directly by the rewritten consumers above.
    artefacts.retain(|a| !oort_targets.contains_key(&a.target));
}

/// Build the conventional release plan for a repo that ships only an edit
/// ontology (no Makefile/config): the primary `<id>.owl` is `merge → reason →
/// relax → reduce → annotate`, and `<id>-base.owl` is `merge → reason →
/// remove-external → annotate`, plus the configured exports. Declared imports of
/// the edit that cannot be resolved (no catalog entry / local module) are
/// surfaced as gaps rather than silently dropped, so a thin "release" is flagged,
/// not faked.
fn build_edit_only(repo: &OdkRepo, only: &[String]) -> Plan {
    use super::robot::{AnnotateSpec, Op, RemoveSpec};

    let id = repo.yaml.id.clone();
    let edit = repo.edit_file.clone().unwrap_or_default();
    // A reference to the plan's `version` field, not a date: see `build`.
    let version = crate::plan::VERSION_REF;
    let ontbase = format!("http://purl.obolibrary.org/obo/{id}");
    let reasoner = repo.yaml.reasoner.clone().unwrap_or_else(|| "ELK".into());

    // Components. The ODK yaml DECLARES them, so that list is authoritative;
    // the `components/` directory scan only supplements it, for a repo that
    // ships component files without listing them.
    //
    // Declared-first matters: a repo whose component sits somewhere other than
    // `components/` would have it silently dropped, because discovery-by-`read_dir`
    // answers "what is on disk", not "what did the repo ask for".
    let mut components: Vec<String> = declared_components(repo);
    if let Ok(rd) = std::fs::read_dir(repo.dir.join("components")) {
        let mut found: Vec<String> = Vec::new();
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.ends_with(".owl") || n.ends_with(".ofn") || n.ends_with(".obo") {
                found.push(format!("components/{n}"));
            }
        }
        found.sort();
        for f in found {
            if !components.contains(&f) {
                components.push(f);
            }
        }
    }

    // The DOSDP products are a source of the release, exactly as an ODK Makefile
    // puts `$(PATTERNDIR)/definitions.owl` in `$(OTHER_SRC)`. Without this the
    // pattern step would write `definitions.owl` and nothing would merge it.
    let dosdp = plan_dosdp(&repo.dir, "../patterns", None, &ontbase, &version);
    if let Some(d) = &dosdp {
        if !d.patterns.is_empty() && !components.contains(&d.output) {
            components.push(d.output.clone());
        }
    }

    let imports = edit_only_imports(repo, &edit);

    let reason = || Op::Reason {
        reasoner: Some(reasoner.clone()),
        equivalent_classes_allowed: Some("asserted-only".into()),
        exclude_tautologies: Some("structural".into()),
        annotate_inferred_axioms: None,
        allow_incoherent: None,
        exclude_external_entities: None,
        exclude_owl_thing: None,
        remove_redundant_subclass_axioms: None,
        create_new_ontology: None,
        create_new_ontology_with_annotations: None,
        exclude_duplicate_axioms: None,
    };
    let merge = || Op::Merge { inputs: components.clone(), collapse_import_closure: None };
    let ann = |art: &str| {
        Op::Annotate(AnnotateSpec {
            ontology_iri: Some(format!("{ontbase}/{art}")),
            version_iri: Some(format!("{ontbase}/releases/{version}/{art}")),
            ..Default::default()
        })
    };
    // The primary `<id>.owl` keeps the bare ontology IRI `obo/<id>.owl`.
    let ann_primary = || {
        Op::Annotate(AnnotateSpec {
            ontology_iri: Some(format!("{ontbase}.owl")),
            version_iri: Some(format!("{ontbase}/releases/{version}/{id}.owl")),
            ..Default::default()
        })
    };

    let full_target = format!("{id}.owl");
    let base_target = format!("{id}-base.owl");

    let matches = |target: &str| -> bool {
        only.is_empty()
            || only.iter().any(|o| {
                target == o || *target == format!("{id}-{o}.owl") || *target == format!("{id}.{o}")
            })
    };

    let mut artefacts: Vec<ArtefactPlan> = Vec::new();

    // Primary / full release.
    artefacts.push(ArtefactPlan {
        target: full_target.clone(),
        input: Some(edit.clone()),
        needs: vec![],
        order_only: vec![],
        steps: vec![
            Step::Op(merge()),
            Step::Op(reason()),
            Step::Op(Op::Relax { include_subclass_of: false }),
            Step::Op(Op::Reduce { reasoner: Some(reasoner.clone()), include_subproperties: None }),
            Step::Op(ann_primary()),
        ],
        gaps: vec![],
        missing_rule: false,
        stdout_file: None,
        // A repo with no build configuration has no conditionals to branch on.
        branches: Vec::new(),
    });

    // Base release: independent of imports' axioms (external axioms removed).
    let base_prefix = format!("http://purl.obolibrary.org/obo/{}_", id.to_uppercase());
    artefacts.push(ArtefactPlan {
        target: base_target.clone(),
        input: Some(edit.clone()),
        needs: vec![],
        order_only: vec![],
        steps: vec![
            Step::Op(merge()),
            Step::Op(reason()),
            Step::Op(Op::Remove(RemoveSpec {
                axioms: vec!["external".into()],
                base_iri: vec![base_prefix],
                trim: Some(false),
                ..Default::default()
            })),
            Step::Op(ann(&base_target)),
        ],
        gaps: vec![],
        missing_rule: false,
        stdout_file: None,
        // A repo with no build configuration has no conditionals to branch on.
        branches: Vec::new(),
    });

    // Exports of the primary.
    for fmt in &repo.yaml.export_formats {
        if fmt == "owl" {
            continue;
        }
        let target = format!("{id}.{fmt}");
        artefacts.push(ArtefactPlan {
            target: target.clone(),
            input: Some(full_target.clone()),
        needs: vec![],
        order_only: vec![],
        steps: vec![Step::Op(Op::Convert { format: Some(fmt.clone()), clean_obo: None, output: None, add_prefixes: vec![] })],
            gaps: vec![],
            missing_rule: false,
            stdout_file: None,
            branches: Vec::new(),
        });
    }

    // `only` restriction: membership of `plan.artefacts` is itself the statement
    // that an artefact is on the release path, so the filter is applied directly
    // rather than recorded per artefact and applied later.
    artefacts.retain(|a| matches(&a.target));

    Plan {
        // No Makefile was read, so no backtick named a version file: an ODK yaml
        // states its version directly, and `version` below is that literal.
        version_file: None,
        // No Makefile was read, so the repo stated no ODK release here; the
        // current tool generation below is the honest default.
        emulate_odk_version: None,
        // A repo with no Makefile has no conditional rule sets, so it exposes no
        // rebuild switches: an empty list is the honest statement, and
        // `--rebuild <anything>` on it is a hard error rather than a silent no-op.
        refresh_groups: Vec::new(),
        // …and no conditionals either, so no switch can be assigned at all.
        gating_flags: std::collections::BTreeMap::new(),
        default_targets: artefacts.iter().map(|a| a.target.clone()).collect(),
        phony: Vec::new(),
        // A spec repo states its targets outright; none is reached only through a
        // pattern, so none is a stage file the build throws away.
        transient_targets: Vec::new(),
        // No Makefile means no DOSDP or mirror rules to have been superseded, so
        // nothing here is built by an engine rather than by a recorded rule.
        native_targets: Vec::new(),
        // Never `Some("")`: an empty relative path joins to the ontology
        // DIRECTORY, which exists, so `is_file()` guards downstream would still
        // have been handed a directory to parse.
        edit_file: repo
            .edit_file
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        catalog_file: catalog_file(&repo.dir),
        emulate_robot_version: CURRENT_ROBOT,
        strict: false,
        xml_entities: false,
        dosdp,
        id,
        // A repo that ships only an edit ontology names no version, so it
        // releases under the date of the build unless the run says otherwise.
        version: crate::plan::VERSION_TODAY.to_string(),
        ontology_iri: format!("{ontbase}.owl"),
        reasoner,
        use_base_merging: false,
        exclude_iri_patterns: vec![],
        slme_individuals: None,
        imports,
        merged_import: None,
        components,
        variables: std::collections::BTreeMap::new(),
        component_gaps: Vec::new(),
        prerequisites: Vec::new(),
        artefacts,
    }
}

/// Makefile variables whose values are consumed at *execution* time and so must
/// travel with the plan. Empty values are not recorded (they carry no more
/// information than their absence).
pub(crate) const EXEC_VARS: [&str; 7] = [
    // `SRC` is recorded for recipes that still expand `$(SRC)`; the edit file the
    // BUILD reads is `Plan::edit_file`, resolved at ingest.
    "SRC",
    "OTHER_SRC",
    "ROBOT",
    "OBOBASE",
    "ODK_VERSION_MAKEFILE",
    // Read when deciding the obographs `meta` shape — a build-time question, so
    // a plan-only repo has to carry the answer.
    "ANNOTATE_ONTOLOGY_VERSION",
    // Where mirrors live. A hard-coded `mirror/` in the executor would write the
    // downloads of a repo that sets `$(MIRRORDIR)` somewhere its recipes do not
    // look — build configuration decided by a string constant instead of the plan.
    "MIRRORDIR",
];

/// The IRI a mirror recipe reads DIRECTLY, if it reads one.
///
/// Most ODK mirror rules download to a file first (`curl … -o mirror/<id>.owl`),
/// and that fetch is recorded as a step. Some read the network straight into a
/// `robot` command instead — MONDO's is `robot convert -I
/// http://purl.obolibrary.org/obo/ncbitaxon/subsets/taxslim.owl` — and there the
/// input is a bare `-I` on a step whose recorded form carries only its output.
/// Recording it as the import's `source` is what `run_mirror_pipeline` already
/// expects ("where there is none, the recipe read the IRI directly … which the
/// plan records as the import's `source`"); leaving it out leaves the executor
/// with a pipeline and nothing to feed it.
fn mirror_input_iri(repo: &OdkRepo, id: &str) -> Option<String> {
    for target in [format!("mirror-{id}"), format!("mirror/{id}.owl")] {
        let Some((rule, stem)) = repo.make.rule_for(&target) else { continue };
        let mut autos = Autos::default();
        autos.set("@", &target);
        if let Some(first) = rule.prereqs.first() {
            autos.set("<", first);
        }
        autos.set("^", &rule.prereqs.join(" "));
        if let Some(s) = &stem {
            autos.set("*", s);
        }
        for line in &rule.recipe {
            // As in `plan_rule`: an `$(eval)` assignment is consumed, not expanded.
            if let Some((name, value)) = parse_eval_assignment(line.trim(), &repo.make, &autos) {
                autos.set(&name, &value);
                continue;
            }
            let expanded = repo.make.expand_with(line, &autos);
            let toks: Vec<&str> = expanded.split_whitespace().collect();
            for (i, t) in toks.iter().enumerate() {
                if (*t == "-I" || *t == "--input-iri") && i + 1 < toks.len() {
                    let v = toks[i + 1];
                    if v.starts_with("http://") || v.starts_with("https://") {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// The files a mirror rule reads that some OTHER rule makes — see
/// `ImportPlan::mirror_inputs`.
///
/// The phony `mirror-<id>` prerequisite of `mirror/<id>.owl` is the recipe
/// itself, not an input, so only prerequisites that look like paths are kept.
fn mirror_inputs(repo: &OdkRepo, id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for target in [format!("mirror-{id}"), format!("mirror/{id}.owl")] {
        let Some((rule, _)) = repo.make.rule_for(&target) else { continue };
        for p in &rule.prereqs {
            let p = p.trim();
            if p.is_empty() || p.starts_with("mirror-") || !p.contains('/') {
                continue;
            }
            // Only what another rule actually makes; an order-only `| $(TMPDIR)`
            // directory is not an input.
            if repo.make.rule_for(p).is_some() && !out.iter().any(|q| q == p) {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// The steps of a custom mirror rule (`mirror-<id>` or `mirror/<id>.owl`), so a
/// plan-only build can still run the project's own mirror script. Empty when the
/// repo has no such rule.
///
/// A mirror rule threads no model between its commands — each names its own
/// `-i`/`-o`, and its downloads write files — so every expanded line is parsed on
/// its own terms and the recorded steps carry the paths the recipe named, rather
/// than being chained into one pipeline that would drop them.
fn mirror_steps(repo: &OdkRepo, id: &str) -> Vec<Step> {
    let robot_prefix = repo.make.expand("$(ROBOT)").trim().to_string();
    let mut steps = Vec::new();
    // `mirror/<id>.owl: mirror-<id>`, so the PREREQUISITE's recipe (the fetch and
    // its post-processing) runs before the target's (compare-and-copy). Both are
    // collected: returning on the first match would record only the copy and drop
    // the fetch entirely.
    for target in [format!("mirror-{id}"), format!("mirror/{id}.owl")] {
        let Some((rule, stem)) = repo.make.rule_for(&target) else { continue };
        let mut autos = Autos::default();
        autos.set("@", &target);
        if let Some(first) = rule.prereqs.first() {
            autos.set("<", first);
        }
        autos.set("^", &rule.prereqs.join(" "));
        // The pattern stem. Without it MONDO's copy guard
        // `[ -f $(TMPDIR)/mirror-$*.owl ]` would expand to `tmp/mirror-.owl`, a
        // file that never exists — so the test would silently fail and the mirror
        // would never be updated.
        if let Some(s) = &stem {
            autos.set("*", s);
        }
        // Inside a mirror recipe the ODK build-mode flags are TRUE by construction.
        // These steps are recorded as `imports[].mirror_steps`, and the executor
        // reaches them only from `build_imports_fresh` — i.e. `--imports fresh`.
        // Their position in the plan already says "this is what refreshing a mirror
        // does", so the recipe's own `if [ $(MIR) = true ] && [ $(IMP) = true ]`
        // guard is asking a question the plan has answered. Expanding it with
        // whatever flags happened to be set while PLANNING would bake one run's
        // switches into a plan meant to be portable: generated under `MIR=false` it
        // would freeze to `[ false = true ]`, and every fetch beneath it would be
        // unreachable however the plan was later invoked.
        for line in &rule.recipe {
            let expanded = repo.make.expand_with(line, &autos);
            if expanded.trim().is_empty() {
                continue;
            }
            // Parse into OPS, like every other recipe: MONDO's mirror rules are a
            // `curl`, a `robot convert`, a `robot remove --axioms external` and an
            // `mv`, inside an `if [ $(MIR) = true ]` guard — all of which owlmake
            // runs natively. Recording them as opaque shell would make a mirror
            // "custom" in practice as well as in name.
            steps.extend(recorded_steps(&expanded, &robot_prefix));
        }
    }
    steps
}

/// Whether a recipe line invokes robot at all — as against running a script, a
/// sub-make, or one of the other tools a recipe reaches for.
fn is_robot_line(cmd: &str, robot_prefix: &str) -> bool {
    cmd.split(['|', ';', '&']).any(|seg| {
        let toks = robot::tokenize(seg);
        !toks.is_empty() && robot::is_robot(&toks, robot_prefix)
    })
}

/// The REMOTE ontology a robot line opens over (`--input-iri` / `-I`).
///
/// Kept apart from [`first_robot_input`] because the two answers are used
/// differently: a file input becomes the rule's `$<` and is resolved to a path,
/// an IRI cannot be and instead opens the line's pipeline as a boundary.
///
/// Recording it is what puts the fetch in the plan: a rule with no prerequisites
/// has no other input, so the line's own IRI is the only thing that says what the
/// pipeline opens over.
pub(super) fn first_robot_iri_input(cmd: &str, robot_prefix: &str) -> Option<String> {
    for seg in cmd.split(['|', ';', '&']) {
        let toks = robot::tokenize(seg);
        if toks.is_empty() || !robot::is_robot(&toks, robot_prefix) {
            continue;
        }
        let mut it = toks.iter();
        while let Some(t) = it.next() {
            let v = match t.as_str() {
                "-I" | "--input-iri" => it.next().cloned(),
                _ => t.strip_prefix("--input-iri=").map(str::to_string),
            };
            let Some(v) = v else { continue };
            if v.starts_with("http://") || v.starts_with("https://") {
                return Some(v);
            }
        }
    }
    None
}

/// The file the first `robot` invocation on this recipe line reads (`-i`/`--input`),
/// which is what opens the pipeline for the rule it belongs to.
///
/// Only a robot command counts — `sed -i` is an in-place edit, not an input — and
/// only a path: a `--input` naming an http(s) IRI loads over the network, and is
/// not a file the executor can thread a model from. That case is
/// [`first_robot_iri_input`].
pub(super) fn first_robot_input(cmd: &str, robot_prefix: &str) -> Option<String> {
    for seg in cmd.split(['|', ';', '&']) {
        let toks = robot::tokenize(seg);
        // `is_robot` indexes `toks[0]`, and splitting on `&` yields empty segments
        // for the `&&` in `curl … && robot remove …`.
        if toks.is_empty() || !robot::is_robot(&toks, robot_prefix) {
            continue;
        }
        let mut it = toks.iter();
        while let Some(t) = it.next() {
            let v = match t.as_str() {
                "-i" | "--input" => it.next(),
                _ => t.strip_prefix("--input=").map(|_| t),
            };
            let Some(v) = v else { continue };
            let v = v.strip_prefix("--input=").unwrap_or(v);
            if v.starts_with("http://") || v.starts_with("https://") || v.starts_with('-') {
                continue;
            }
            return Some(v.to_string());
        }
    }
    None
}

/// The steps a recipe line contributes to the plan.
///
/// A plan describes what a build DOES, so a step that never runs has no business
/// being in one. `Step::Shell` is exactly that: the recipe bookkeeping owlmake's
/// in-memory pipeline subsumes — an `echo`, a `cd`, the `mv $@.tmp $@` whose move
/// the pipeline's closing write already performed — and the executor skips every
/// one of them. Recording them would make the plan claim work it does not do, and
/// make `shell` read as the inert op while `unsupported-shell`, which runs, reads
/// as the broken one.
///
/// Nothing that CHANGES a file is dropped here: `cp`/`mv`/`rm`/`touch` and every
/// redirect (`cat a > b`, `echo x >> f`) are parsed into structured `file` ops
/// before the benign classification is reached, and those run.
fn recorded_steps(cmd: &str, robot_prefix: &str) -> Vec<Step> {
    let mut steps = fold_static_branches(robot::parse_command(cmd, robot_prefix));
    drop_inert(&mut steps);
    steps
}

/// Remove the steps the executor would skip, inside conditional branches too.
fn drop_inert(steps: &mut Vec<Step>) {
    steps.retain(|s| !matches!(s, Step::Inert(_)));
    for s in steps.iter_mut() {
        if let Step::Branch { then_steps, else_steps, .. } = s {
            drop_inert(then_steps);
            drop_inert(else_steps);
        }
    }
}

/// Replace every branch whose condition is already decided by the body that will
/// run, recursively. What is left is what the step actually does.
///
/// The ODK build-mode switches are the conditions this must NOT decide: they are
/// not properties of the ontology but of the run. owlmake spells them
/// `--imports cached|fresh` and `--patterns regenerate|cached` and resolves them
/// when it executes, so ingest expands their guards as taken rather than freezing
/// one run's answer into the plan.
fn fold_static_branches(steps: Vec<Step>) -> Vec<Step> {
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        match step {
            Step::Branch { condition, then_steps, else_steps } => match condition.static_value() {
                Some(true) => out.extend(fold_static_branches(then_steps)),
                Some(false) => out.extend(fold_static_branches(else_steps)),
                None => out.push(Step::Branch {
                    condition,
                    then_steps: fold_static_branches(then_steps),
                    else_steps: fold_static_branches(else_steps),
                }),
            },
            s => out.push(s),
        }
    }
    out
}

/// Decide, here, every branch whose condition reads the repository's BUILD
/// CONFIGURATION, and keep only the body that runs.
///
/// `config_check` hashes the repo's ODK config and compares the digest with the
/// one the build was generated from. That file is precisely what the plan
/// replaces, so a plan-only repo can never answer the test: left as a branch it
/// takes the else arm and announces a configuration drift that has not happened.
/// Ingest has the file in front of it, so the answer is a build-configuration
/// read like any other — resolved now, recorded as its consequence.
///
/// Only a condition naming that file is decided. A condition about anything else
/// — a build output, a run input — is left standing, because those are answered
/// when the build runs and not before.
fn resolve_build_config_branches(steps: &mut Vec<Step>, repo: &OdkRepo) {
    let Some(config) = repo.config_file_name() else { return };
    let mut out: Vec<Step> = Vec::with_capacity(steps.len());
    for step in steps.drain(..) {
        match step {
            Step::Branch { condition, mut then_steps, mut else_steps } => {
                resolve_build_config_branches(&mut then_steps, repo);
                resolve_build_config_branches(&mut else_steps, repo);
                let Condition::Shell(raw) = &condition else {
                    out.push(Step::Branch { condition, then_steps, else_steps });
                    continue;
                };
                if !raw.contains(&config) {
                    out.push(Step::Branch { condition, then_steps, else_steps });
                    continue;
                }
                match std::process::Command::new("sh")
                    .arg("-c")
                    .arg(raw.as_str())
                    .current_dir(&repo.dir)
                    .status()
                {
                    Ok(st) if st.success() => out.extend(then_steps),
                    Ok(_) => out.extend(else_steps),
                    // The shell itself would not run. Leave the branch alone
                    // rather than record an answer nothing produced.
                    Err(_) => out.push(Step::Branch { condition, then_steps, else_steps }),
                }
            }
            s => out.push(s),
        }
    }
    *steps = out;
}

pub(crate) fn exec_variables(repo: &OdkRepo) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for name in EXEC_VARS {
        let v = repo.make.expand(&format!("$({name})"));
        let v = v.trim();
        if !v.is_empty() {
            out.insert(name.to_string(), v.to_string());
        }
    }
    out
}

/// Declared imports of the edit ontology, each with whether it resolves to a
/// local module (via `catalog-v001.xml` or a conventional path). Unresolved ones
/// carry a gap so the release is honestly flagged as incomplete.
fn edit_only_imports(repo: &OdkRepo, edit: &str) -> Vec<ImportPlan> {
    let text = std::fs::read_to_string(repo.dir.join(edit)).unwrap_or_default();
    let mut iris: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("import:") {
            // OBO header `import: <IRI>`
            let iri = rest.trim().split_whitespace().next().unwrap_or("").to_string();
            if !iri.is_empty() {
                iris.push(iri);
            }
        } else if let Some(rest) = l.strip_prefix("Import(") {
            // OWL functional `Import(<IRI>)`
            let iri = rest.trim_start_matches('<').trim_end_matches(')').trim_end_matches('>');
            if !iri.is_empty() {
                iris.push(iri.to_string());
            }
        } else if l.contains("owl:imports") {
            // RDF/XML `<owl:imports rdf:resource="IRI"/>`
            if let Some(p) = l.find("rdf:resource=\"") {
                let rest = &l[p + 14..];
                if let Some(end) = rest.find('"') {
                    iris.push(rest[..end].to_string());
                }
            }
        }
    }
    iris.sort();
    iris.dedup();

    let catalog = crate::build::load_catalog(&repo.dir);
    iris.into_iter()
        .map(|iri| {
            let resolved = resolve_import(&iri, &catalog, &repo.dir);
            let short = iri.rsplit('/').next().unwrap_or(&iri).to_string();
            let gaps = if resolved.is_some() {
                vec![]
            } else {
                vec!["declared import is not resolvable (no catalog entry or local module); the release would be missing it".to_string()]
            };
            // A declared `owl:imports` is resolved to an existing local module and
            // merged as-is — there is no mirror→module pipeline to run.
            ImportPlan {
                id: short,
                source: iri,
                output: resolved.clone().unwrap_or_default(),
                steps: Vec::new(),
                cached: resolved.is_some(),
                gaps,
                product: None,
                mirror_steps: Vec::new(),
                mirror_inputs: Vec::new(),
            }
        })
        .collect()
}

/// Resolve an import IRI to a local module path: via the catalog, or a
/// conventional location (the IRI's trailing `imports/<x>` or basename under the
/// ontology dir). Returns the repo-relative path string when found on disk.
fn resolve_import(
    iri: &str,
    catalog: &std::collections::BTreeMap<String, std::path::PathBuf>,
    dir: &std::path::Path,
) -> Option<String> {
    if let Some(p) = catalog.get(iri) {
        if p.exists() {
            return p.strip_prefix(dir).ok().map(|r| r.display().to_string());
        }
    }
    // Fallback: trailing `imports/<file>` of the IRI, then the bare basename.
    let base = iri.rsplit('/').next().unwrap_or("");
    for cand in [format!("imports/{base}"), base.to_string()] {
        if !cand.is_empty() && dir.join(&cand).exists() {
            return Some(cand);
        }
    }
    None
}

/// Build the mirror→module pipeline for one import product.
///
/// Walks the Makefile rule chain `imports/<id>_import.owl ← … ← mirror/<id>.owl`,
/// parsing each recipe into native steps so per-import excludes / renames / extra
/// seed terms are captured faithfully, and resolves every `--term-file` to its
/// committed *source* (an EFO `imports/<id>_terms.txt` is itself a `cat | sort |
/// uniq` of `iri_dependencies/<id>_terms.txt` + `iri_dependencies/efo-relations.txt`,
/// so the plan records those sources, never the derived copy — the path guessing
/// happens once, here). When the repo defines no such recipe, the canonical
/// BOT/make-base pipeline is synthesized from the product's flags.
fn import_pipeline(repo: &OdkRepo, p: &super::ImportProduct, obobase: &str) -> Vec<Step> {
    let make = &repo.make;
    let robot_prefix = {
        let p = make.expand("$(ROBOT)");
        if p.trim().is_empty() { "robot".to_string() } else { p.trim().to_string() }
    };

    // Collect the rule chain from the module target back toward the mirror,
    // following the first prerequisite (`$<`) while it is itself an `imports/…`
    // intermediate with its own rule (e.g. `imports/<id>_bot.owl`).
    let target = format!("imports/{}_import.owl", p.id);
    let mut visited = std::collections::HashSet::new();
    let mut cur = target.clone();
    let mut chain: Vec<(Vec<String>, Vec<String>, String, Option<String>)> = Vec::new();
    while visited.insert(cur.clone()) && chain.len() < 16 {
        let Some((rule, stem)) = make.rule_for(&cur) else { break };
        let prereqs: Vec<String> = match &stem {
            Some(s) => rule.prereqs.iter().map(|q| q.replace('%', s)).collect(),
            None => rule.prereqs.clone(),
        };
        chain.push((rule.recipe.clone(), prereqs.clone(), cur.clone(), stem.clone()));
        match prereqs.first() {
            Some(inp)
                if inp.starts_with("imports/")
                    && inp.ends_with(".owl")
                    && make.rule_for(inp).is_some() =>
            {
                cur = inp.clone();
            }
            _ => break,
        }
    }

    // No recipe at all → synthesize the canonical pipeline from product flags.
    if chain.is_empty() {
        return synth_import_pipeline(repo, p, obobase);
    }

    // Parse recipes mirror-side first (the chain was collected target-first).
    chain.reverse();
    let mut steps = Vec::new();
    for (recipe, prereqs, tgt, stem) in &chain {
        let mut autos = Autos::default();
        autos.set("@", tgt);
        if let Some(first) = prereqs.first() {
            autos.set("<", first);
        }
        autos.set("^", &prereqs.join(" "));
        if let Some(s) = stem {
            autos.set("*", s);
        }
        // Same reasoning as `plan_rule`: this chain builds an import module, so the
        // flags that gate importing are settled before it is reached.
        for line in recipe {
            let expanded = make.expand_with(line, &autos);
            steps.extend(recorded_steps(&expanded, &robot_prefix));
        }
    }
    drop_target_round_trip(&mut steps, &target);
    resolve_seed_paths(make, repo, &mut steps);
    steps
}

/// Whether a file the build writes on the way to something else is left behind.
///
/// A path the build configuration reaches only through a pattern exists to carry
/// one step's output to the next, and goes once the chain that needed it is done.
/// Naming it outright keeps it: another rule reads it, or it is declared precious.
/// EFO's `imports/uberon_bot.owl` is read by a QC target and `imports/oba_bot.owl`
/// by the OBA module's own rule, so those two survive a release while the other
/// nine ⊥-modules do not.
fn kept_after_build(make: &MakeModel, path: &str) -> bool {
    // The plan writes a path from the repository root and the build configuration
    // from its own directory, so compare on whole trailing components.
    let same = |a: &str| {
        a == path
            || a.strip_suffix(path).is_some_and(|p| p.ends_with('/'))
            || path.strip_suffix(a).is_some_and(|p| p.ends_with('/'))
    };
    if make.rules.keys().any(|k| same(k)) {
        return true;
    }
    let names = |list: &[String]| {
        list.iter().any(|p| make.expand(p).split_whitespace().any(same))
    };
    if make.rules.values().any(|r| names(&r.prereqs) || names(&r.order_only)) {
        return true;
    }
    // `.PRECIOUS` and `.SECONDARY` both keep what they cover, whether they name a
    // file or a pattern. `.SECONDARY` is the one that says "these are
    // intermediates, but do not delete them", so a build configuration that
    // declares it is asking for exactly the file the sweep would remove.
    [".PRECIOUS", ".SECONDARY"].iter().any(|special| {
    make.rules.get(*special).is_some_and(|r| {
        r.prereqs.iter().any(|p| {
            make.expand(p).split_whitespace().any(|t| {
                super::makefile::match_pattern(t, path).is_some()
                    || path
                        .rsplit_once('/')
                        .is_some_and(|(_, base)| super::makefile::match_pattern(t, base).is_some())
            })
        })
    })
    })
}

/// Every path the build writes on its way to something else and does not keep:
/// the stage files an import pipeline threads the model through, and the targets
/// the build configuration reaches only through a pattern.
fn transient_targets(
    make: &MakeModel,
    imports: &[ImportPlan],
    artefacts: &[ArtefactPlan],
    prerequisites: &[ArtefactPlan],
) -> Vec<String> {
    let mut out: std::collections::BTreeSet<String> = Default::default();
    for imp in imports {
        for step in &imp.steps {
            if let Step::Op(Op::RoundTrip { path }) = step {
                if !kept_after_build(make, path) {
                    out.insert(path.clone());
                }
            }
        }
    }
    for a in artefacts.iter().chain(prerequisites) {
        if !kept_after_build(make, &a.target) {
            out.insert(a.target.clone());
        }
    }
    out.into_iter().collect()
}

/// The canonical import pipeline when the repo ships no per-import recipe: an
/// optional base-reduction (`make-base` / `base-iris` keep only the source's own
/// axioms) followed by a ⊥-locality (BOT) module over the product's seed terms.
fn synth_import_pipeline(repo: &OdkRepo, p: &super::ImportProduct, obobase: &str) -> Vec<Step> {
    use super::robot::{Op, RemoveSpec};
    let mut steps = Vec::new();
    if p.make_base || !p.base_iris.is_empty() {
        let base_iri = if p.base_iris.is_empty() {
            vec![format!("{obobase}/{}_", p.id.to_uppercase())]
        } else {
            p.base_iris.clone()
        };
        steps.push(Step::Op(Op::Remove(RemoveSpec {
            terms: vec![],
            term_files: vec![],
            selects: vec![],
            axioms: vec!["external".into()],
            base_iri,
            trim: None,
            preserve_structure: None,
            exclude_terms: vec![],
            exclude_term_files: vec![],
            signature: None,
            drop_axiom_annotations: None,
        })));
    }
    // Seed from the product's committed terms file, resolved to its source.
    let mut term_files = Vec::new();
    let conventional = format!("imports/{}_terms.txt", p.id);
    if repo.dir.join(&conventional).exists() || repo.make.rule_for(&conventional).is_some() {
        term_files.push(conventional);
    }
    steps.push(Step::Op(Op::Extract {
        method: "BOT".into(),
        terms: vec![],
        term_files,
        copy_ontology_annotations: false,
        individuals: None,
        branch_from_terms: vec![],
        branch_from_term_files: vec![],
    }));
    resolve_seed_paths(&repo.make, repo, &mut steps);
    steps
}

/// Rewrite every `--term-file` path in `steps` to its committed source(s): when a
/// referenced term file is itself a Makefile target whose recipe is pure text
/// shuffling (`cat`/`sort`/`uniq`/`cp`), replace it with that rule's
/// prerequisites (one level of indirection — exactly the EFO
/// `imports/<id>_terms.txt ← iri_dependencies/…` case). Stops at any file built by
/// a real tool (a SPARQL query step), which is a genuine source we record as-is,
/// and at any `cat` whose sources do not all end in a newline — there the derived
/// file is not the union of its parts and only the derived file is faithful.
fn resolve_seed_paths(make: &super::makefile::MakeModel, repo: &OdkRepo, steps: &mut [Step]) {
    use super::robot::Op;
    let resolve = |path: &str| -> Vec<String> {
        let Some((rule, stem)) = make.rule_for(path) else { return vec![path.to_string()] };
        let is_text_only = !rule.recipe.is_empty()
            && rule.recipe.iter().all(|line| {
                line.split_whitespace().next().is_none_or(|w| {
                    matches!(w, "cat" | "sort" | "uniq" | "cp" | "tac" | "tee" | "grep" | "sed")
                })
            });
        if !is_text_only {
            return vec![path.to_string()];
        }
        let sources: Vec<String> = match &stem {
            Some(s) => rule.prereqs.iter().map(|q| q.replace('%', s)).collect(),
            None => rule.prereqs.clone(),
        };
        // `cat` concatenates BYTES. A source that does not end in a newline is
        // glued to the first line of the next one, and the term that got swallowed
        // is not a term at all. EFO's `iri_dependencies/hp_terms.txt` ends without
        // a newline, so its real seed file holds
        // `…/HP_0020045BFO:0000050` — one unusable line where the two sources read
        // separately would look like two valid terms, and `hp_import.owl` would
        // gain a part_of block EFO's own releases do not carry. Where the
        // concatenation is not line-faithful, keep the derived file: its bytes are
        // the seed the extraction reads.
        let last = sources.len().saturating_sub(1);
        for (i, src) in sources.iter().enumerate() {
            if i == last {
                break;
            }
            match std::fs::read(repo.dir.join(src)) {
                Ok(b) if b.last() == Some(&b'\n') => {}
                // Unreadable is treated as unsafe too: better the derived file.
                _ => return vec![path.to_string()],
            }
        }
        sources
    };
    let map = |files: &Vec<String>| -> Vec<String> {
        let mut out = Vec::new();
        for f in files {
            for r in resolve(f) {
                if !out.contains(&r) {
                    out.push(r);
                }
            }
        }
        out
    };
    for step in steps {
        let op = match step {
            Step::Op(op) | Step::Partial { op, .. } => op,
            _ => continue,
        };
        match op {
            Op::Extract { term_files, .. } => *term_files = map(term_files),
            Op::Filter(spec) => spec.term_files = map(&spec.term_files),
            Op::Remove(spec) => spec.term_files = map(&spec.term_files),
            Op::Materialize { term_files, .. } => *term_files = map(term_files),
            _ => {}
        }
    }
}

// --- Human stage descriptions -------------------------------------------------

/// Drop the round-trip a rule's own final `-o $@` implies: the pipeline's closing
/// write already performs it, and materializing the target twice would send it
/// through one serialization more than the recipe asks for.
fn drop_target_round_trip(steps: &mut Vec<Step>, target: &str) {
    let name = std::path::Path::new(target).file_name().map(|s| s.to_os_string());
    steps.retain(|s| match s {
        Step::Op(robot::Op::RoundTrip { path, .. }) => {
            std::path::Path::new(path).file_name().map(|s| s.to_os_string()) != name
        }
        _ => true,
    });
}

/// Parse a recipe line that is a single `$(eval VAR <op> VALUE)` (or `${…}`)
/// assignment, returning the variable name and its expanded value. Returns `None`
/// for any line that is not such an assignment. Only the simple-variable
/// assignment form `eval` is used for in ODK recipes is handled; the value is
/// expanded immediately (`:=`/`=` are treated alike here, which suffices because
/// the value is consumed right away by the next recipe line).
pub(super) fn parse_eval_assignment(
    line: &str,
    make: &super::makefile::MakeModel,
    autos: &Autos,
) -> Option<(String, String)> {
    let body = line.strip_prefix("$(eval").or_else(|| line.strip_prefix("${eval"))?;
    // Strip the single trailing close bracket of the eval call and surrounding
    // whitespace; any inner `$(...)` keeps its own balanced brackets.
    let body = body.trim();
    let body = body.strip_suffix(')').or_else(|| body.strip_suffix('}'))?.trim();
    // Split on the assignment operator (`:=`, `::=`, `?=`, `+=`, or `=`).
    let (name, val) = ["::=", ":=", "?=", "+=", "="].iter().find_map(|op| {
        body.split_once(*op).map(|(n, v)| (n.trim(), v.trim()))
    })?;
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    Some((name.to_string(), make.expand_with(val, autos).trim().to_string()))
}

/// Expand a rule's prerequisites for a concrete `target` (with optional pattern
/// `stem`). This applies the pattern stem (`%`) AND a *second expansion* pass
/// (`.SECONDEXPANSION`): a prerequisite such as UBERON's
/// `$$(COLLECTED_$$*_SOURCES)` survives the first ingest pass as the literal
/// `$(COLLECTED_$*_SOURCES)` and is only resolved here, with `$*`/`$@` bound, into
/// the concrete file list it names. A single prerequisite token may expand to
/// several whitespace-separated files (a computed variable holding a list), so the
/// result is flattened.
pub(super) fn expanded_prereqs(
    repo: &OdkRepo,
    rule: &super::makefile::Rule,
    stem: Option<&str>,
    target: &str,
) -> Vec<String> {
    expanded_prereqs_opt(repo, rule, stem, target, true)
}

/// As [`expanded_prereqs`], with `order_only` deciding whether the order-only
/// prerequisites are included. They are dependencies, so the build graph wants
/// them; `$^` and `$<` do NOT — those automatic variables omit them. OBA's
/// `$(IMPORTSEED): $(PRESEED) $(TMPDIR)/all_pattern_terms.txt | $(TMPDIR)` runs
/// `cat $^ | sort | uniq > $@`, and including `tmp` would make it `cat: tmp: Is a
/// directory`.
pub(super) fn expanded_prereqs_opt(
    repo: &OdkRepo,
    rule: &super::makefile::Rule,
    stem: Option<&str>,
    target: &str,
    order_only: bool,
) -> Vec<String> {
    let mut autos = Autos::default();
    autos.set("@", target);
    if let Some(s) = stem {
        autos.set("*", s);
    }
    // Order-only prerequisites are dependencies too — they just don't feed
    // `$^`/`$<`. Callers that build the graph need both; callers that expand the
    // automatic variables use `rule.prereqs` directly.
    rule.prereqs
        .iter()
        .chain(rule.order_only.iter().filter(|_| order_only))
        .flat_map(|p| {
            let p = match stem {
                Some(s) => p.replace('%', s),
                None => p.clone(),
            };
            // Re-expand: a no-op for ordinary prerequisites (already fully expanded
            // at ingest, no `$` left), and the deferred second expansion for
            // `.SECONDEXPANSION` ones.
            repo.make
                .expand_with(&p, &autos)
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The release version a repo's version file holds right now, trimmed.
///
/// `rel` is `repo.dir`-relative, the convention every path field in the plan
/// follows. An unreadable or empty file yields `None`, which leaves the recorded
/// default in place rather than stamping a release with an empty version.
fn read_version_file(dir: &std::path::Path, rel: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(rel)).ok()?;
    let v = text.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// The single `*-idranges.owl` beside the edit file, as a `repo.dir`-relative
/// name — the ID policy `mint` draws definitive IDs from.
///
/// `None` when there is no such file, or more than one and so no single answer:
/// the op then reaches execution unresolved and fails naming the problem, rather
/// than picking one of several ID policies.
pub(super) fn idranges_beside_edit_file(dir: &std::path::Path) -> Option<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with("-idranges.owl"))
        .collect();
    found.sort();
    (found.len() == 1).then(|| found.remove(0))
}
