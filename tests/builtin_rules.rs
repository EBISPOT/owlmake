//! **The oracle for the built-in rules.**
//!
//! For any configuration, the built-in rules must resolve to the same plan as
//! ingesting the Makefile that was generated for it. The generated file carries
//! no information of its own — it is a function of the configuration — so any
//! difference between the two plans is a defect in the built-in rules, found
//! without anyone having to re-derive a recipe by eye.
//!
//! The repositories compared are real ones. Name them in `OM_ORACLE_REPOS`
//! (colon-separated paths, each a repository root holding `src/ontology`); with
//! the variable unset the comparison has nothing to run over and says so.

use std::collections::BTreeMap;
use std::path::Path;

use owlmake::odk::OdkRepo;
use owlmake::spec::OwlmakeSpec;

/// Text a shell reads is the same text however its words are spaced, and a
/// generated file pads its lists with runs of blanks.
fn squeeze(v: &mut serde_yaml::Value) {
    match v {
        serde_yaml::Value::Mapping(m) => {
            for (k, v) in m.iter_mut() {
                match (k.as_str(), &mut *v) {
                    (Some("command" | "message"), serde_yaml::Value::String(s)) => {
                        *s = s.split_whitespace().collect::<Vec<_>>().join(" ");
                    }
                    _ => squeeze(v),
                }
            }
        }
        serde_yaml::Value::Sequence(items) => items.iter_mut().for_each(squeeze),
        _ => {}
    }
}

/// A plan as comparable data: every top-level field, with the two target lists
/// re-keyed by target name so a difference names the target it is in.
fn resolved(repo: &OdkRepo) -> BTreeMap<String, serde_yaml::Value> {
    let plan = repo.plan(&[]).expect("planning");
    let spec = serde_yaml::to_value(OwlmakeSpec::from_plan(&plan)).expect("serializing");
    let serde_yaml::Value::Mapping(fields) = spec else { panic!("a plan is a mapping") };
    let mut out = BTreeMap::new();
    for (key, value) in fields {
        let key = key.as_str().expect("string key").to_string();
        match (key.as_str(), value) {
            ("prerequisites" | "artefacts", serde_yaml::Value::Sequence(targets)) => {
                for t in targets {
                    let name = t
                        .get("target")
                        .and_then(|n| n.as_str())
                        .expect("a target has a name")
                        .to_string();
                    out.insert(format!("target {name}"), t);
                }
            }
            (_, value) => {
                out.insert(format!("field {key}"), value);
            }
        }
    }
    out
}

fn compare(root: &Path) -> Vec<String> {
    let mut ingested = resolved(&OdkRepo::load(root).expect("loading the generated Makefile"));
    let mut builtin =
        resolved(&OdkRepo::load_with_builtin_rules(root).expect("loading the built-in rules"));
    ingested.values_mut().chain(builtin.values_mut()).for_each(squeeze);
    // What a generated file holds that rules built as data have no counterpart
    // for: the `.FORCE` idiom, and the emptiness tests it wraps recipes in — the
    // built-in rules decide those as they are built, so only SWITCHES gate them.
    if let Some(serde_yaml::Value::Sequence(phony)) = ingested.get_mut("field phony") {
        phony.retain(|t| t.as_str() != Some(".FORCE"));
    }
    if let (Some(serde_yaml::Value::Mapping(theirs)), Some(serde_yaml::Value::Mapping(ours))) =
        (ingested.get_mut("field gating_flags"), builtin.get("field gating_flags"))
    {
        let switches: Vec<serde_yaml::Value> = ours.keys().cloned().collect();
        theirs.retain(|k, _| switches.contains(k));
    }
    let mut problems = Vec::new();
    let show = |v: &serde_yaml::Value| serde_yaml::to_string(v).unwrap_or_default();
    for (name, want) in &ingested {
        match builtin.get(name) {
            None => problems.push(format!("MISSING from the built-in rules: {name}")),
            Some(got) if got != want => problems.push(format!(
                "DIFFERS: {name}\n--- ingested\n{}--- built-in\n{}",
                show(want),
                show(got)
            )),
            Some(_) => {}
        }
    }
    for name in builtin.keys().filter(|n| !ingested.contains_key(*n)) {
        problems.push(format!("ONLY in the built-in rules: {name}"));
    }
    problems
}

#[test]
fn builtin_rules_resolve_to_the_ingested_plan() {
    let Ok(repos) = std::env::var("OM_ORACLE_REPOS") else {
        eprintln!("OM_ORACLE_REPOS is unset: no repositories to compare");
        return;
    };
    let mut failed = false;
    for root in repos.split(':').filter(|r| !r.is_empty()) {
        let problems = compare(Path::new(root));
        eprintln!("== {root}: {} difference(s)", problems.len());
        for p in &problems {
            eprintln!("{p}\n");
        }
        failed |= !problems.is_empty();
    }
    assert!(!failed, "the built-in rules and the ingested plans differ; see above");
}
