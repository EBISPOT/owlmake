//! **The oracle for the built-in rules.**
//!
//! For any configuration, the built-in rules must resolve to the same plan as
//! ingesting the Makefile that was generated for it. The generated file carries
//! no information of its own — it is a function of the configuration — so any
//! difference between the two plans is a defect in the built-in rules, found
//! without anyone having to re-derive a recipe by eye.
//!
//! Two sources of configurations:
//!
//! * `tests/fixtures/odk-<version>/<name>/` — a `config.yaml`, the `Makefile`
//!   that ODK release really generated for it, and optionally the rules a
//!   repository wrote itself (`own.Makefile`) and files it holds (`tree/`, laid
//!   out from the repository root). Add one for any option with
//!   `scripts/gen_odk_fixture.sh`; it runs ODK's own generator, so the expected
//!   side of the comparison is never written by hand.
//! * real repositories, named in `OM_ORACLE_REPOS` (colon-separated roots, each
//!   holding `src/ontology`), for the shapes only a working repository has.

use std::collections::BTreeMap;
use std::path::Path;

use owlmake::odk::OdkRepo;

fn resolved(repo: &OdkRepo) -> BTreeMap<String, serde_json::Value> {
    owlmake::odk::builtin::comparable(&repo.plan(&[]).expect("planning"))
}

fn compare(root: &Path) -> Vec<String> {
    let generated = OdkRepo::load(root).expect("loading the generated Makefile");
    let builtin = OdkRepo::load_with_builtin_rules(root).expect("loading the built-in rules");
    owlmake::odk::builtin::differences_from_generated(
        &generated.plan(&[]).expect("planning"),
        &builtin.plan(&[]).expect("planning"),
    )
}

/// Lay a fixture out as the repository it describes and return its root. `test`
/// keeps the directories of tests that run side by side apart.
fn repository_for(fixture: &Path, test: &str) -> std::path::PathBuf {
    let config = std::fs::read_to_string(fixture.join("config.yaml")).expect("config.yaml");
    let parsed: serde_yaml::Value = serde_yaml::from_str(&config).expect("a YAML configuration");
    let id = parsed.get("id").and_then(|i| i.as_str()).expect("a configuration names its id");
    let mut root = std::env::temp_dir();
    root.push(format!(
        "owlmake_builtin_{}_{test}_{}",
        std::process::id(),
        fixture.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(ont.join(format!("{id}-odk.yaml")), config).unwrap();
    std::fs::copy(fixture.join("Makefile"), ont.join("Makefile")).unwrap();
    if fixture.join("own.Makefile").exists() {
        std::fs::copy(fixture.join("own.Makefile"), ont.join(format!("{id}.Makefile"))).unwrap();
    }
    // Files the rules read off the repository — pattern and data tables, which
    // the pattern rules find by listing directories.
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let (src, dst) = (entry.path(), to.join(entry.file_name()));
            if src.is_dir() {
                copy_tree(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }
    if fixture.join("tree").is_dir() {
        copy_tree(&fixture.join("tree"), &root);
    }
    root
}

#[test]
fn builtin_rules_resolve_to_what_odk_generates() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odk-1.6.1");
    let mut names: Vec<_> = std::fs::read_dir(&fixtures)
        .expect("the fixture directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("config.yaml").exists())
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no fixtures under {}", fixtures.display());
    let mut failed = Vec::new();
    for fixture in &names {
        let root = repository_for(fixture, "oracle");
        let problems = compare(&root);
        let _ = std::fs::remove_dir_all(&root);
        let name = fixture.file_name().unwrap().to_string_lossy().to_string();
        eprintln!("== fixture {name}: {} difference(s)", problems.len());
        for p in &problems {
            eprintln!("{p}\n");
        }
        if !problems.is_empty() {
            failed.push(name);
        }
    }
    assert!(failed.is_empty(), "fixtures whose plans differ: {failed:?}");
}

use owlmake::odk::builtin::differences;

/// **The acceptance test for `owlmake.yaml`.** Write the file a repository
/// commits, take its Makefile, its own rules and its configuration OUT of the
/// tree, and plan again from the file alone: the plan must be the one the
/// repository had. The file holds only the repository's options and what it
/// builds in a way of its own, so this is also the proof that nothing else was
/// needed.
#[test]
fn a_standard_file_alone_resolves_to_the_same_plan() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odk-1.6.1");
    let mut names: Vec<_> = std::fs::read_dir(&fixtures)
        .expect("the fixture directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("config.yaml").exists())
        .collect();
    names.sort();
    let mut failed = Vec::new();
    for fixture in &names {
        let name = fixture.file_name().unwrap().to_string_lossy().to_string();
        let root = repository_for(fixture, "standard");
        let before = resolved(&OdkRepo::load_with_builtin_rules(&root).expect("loading"));
        let spec = OdkRepo::standard_spec(&root).expect("writing the standard file");
        let file = root.join("owlmake.yaml");
        owlmake::spec::save(&spec, &file).expect("saving owlmake.yaml");
        let lines = std::fs::read_to_string(&file).unwrap().lines().count();
        // Kept for a reader who wants to see what a repository would commit.
        if let Ok(dir) = std::env::var("OM_KEEP_STANDARD_FILES") {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::copy(&file, Path::new(&dir).join(format!("{name}.owlmake.yaml")));
        }

        let ont = root.join("src/ontology");
        for entry in std::fs::read_dir(&ont).unwrap().flatten() {
            let n = entry.file_name().to_string_lossy().to_string();
            if n == "Makefile" || n.ends_with(".Makefile") || n.ends_with("-odk.yaml") {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        let after = resolved(&OdkRepo::load(&root).expect("loading from owlmake.yaml alone"));
        let problems = differences(&before, &after, ("with its build files", "from owlmake.yaml alone"));
        eprintln!("== standard file {name}: {lines} lines, {} difference(s)", problems.len());
        for p in &problems {
            eprintln!("{p}\n");
        }
        if !problems.is_empty() {
            failed.push(name);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    assert!(failed.is_empty(), "fixtures whose plan changed: {failed:?}");
}

/// A working repository laid out again WITHOUT its build files: every entry of
/// its `src/` linked into a scratch tree, except the Makefile, the repository's
/// own rules and its configuration. Real repositories are too large to copy and
/// must not be touched.
fn without_build_files(root: &Path) -> std::path::PathBuf {
    let mut scratch = std::env::temp_dir();
    scratch.push(format!(
        "owlmake_builtin_{}_real_{}",
        std::process::id(),
        root.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    let link = |from: &Path, to: &Path| std::os::unix::fs::symlink(from, to).unwrap();
    let ont = scratch.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    for entry in std::fs::read_dir(root.join("src")).unwrap().flatten() {
        if entry.file_name() != "ontology" {
            link(&entry.path(), &scratch.join("src").join(entry.file_name()));
        }
    }
    for entry in std::fs::read_dir(root.join("src/ontology")).unwrap().flatten() {
        let n = entry.file_name().to_string_lossy().to_string();
        if n == "Makefile" || n.ends_with(".Makefile") || n.ends_with("-odk.yaml") {
            continue;
        }
        link(&entry.path(), &ont.join(&n));
    }
    scratch
}

/// The same acceptance test over working repositories, which hold what no
/// fixture does: rules of their own for an import, a mirror, the pattern merge.
#[test]
fn a_standard_file_alone_resolves_a_real_repository() {
    let Ok(repos) = std::env::var("OM_ORACLE_REPOS") else {
        eprintln!("OM_ORACLE_REPOS is unset: no repositories to compare");
        return;
    };
    let mut failed = false;
    for root in repos.split(':').filter(|r| !r.is_empty()).map(Path::new) {
        let before = resolved(&OdkRepo::load_with_builtin_rules(root).expect("loading"));
        let spec = OdkRepo::standard_spec(root).expect("writing the standard file");
        let scratch = without_build_files(root);
        let file = scratch.join("owlmake.yaml");
        owlmake::spec::save(&spec, &file).expect("saving owlmake.yaml");
        let lines = std::fs::read_to_string(&file).unwrap().lines().count();
        if let Ok(dir) = std::env::var("OM_KEEP_STANDARD_FILES") {
            let _ = std::fs::create_dir_all(&dir);
            let name = root.file_name().unwrap().to_string_lossy();
            let _ = std::fs::copy(&file, Path::new(&dir).join(format!("{name}.owlmake.yaml")));
        }
        let after = resolved(&OdkRepo::load(&scratch).expect("loading from owlmake.yaml alone"));
        let problems = differences(&before, &after, ("with its build files", "from owlmake.yaml alone"));
        eprintln!("== standard file {}: {lines} lines, {} difference(s)", root.display(), problems.len());
        for p in &problems {
            eprintln!("{p}\n");
        }
        failed |= !problems.is_empty();
        let _ = std::fs::remove_dir_all(&scratch);
    }
    assert!(!failed, "a repository's plan changed; see above");
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
