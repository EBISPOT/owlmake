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

/// Plan in this process as `om` does: a `$(shell …)` in a repository's rules
/// runs owlmake's own `grep`, `sed` and the rest, and this test binary is not
/// owlmake.
fn bundled_tools_as_om() {
    owlmake::build::recipe::run_bundled_tools_as(env!("CARGO_BIN_EXE_om"));
}

/// Lay a fixture out as the repository it describes and return its root. `test`
/// keeps the directories of tests that run side by side apart.
fn repository_for(fixture: &Path, test: &str) -> std::path::PathBuf {
    bundled_tools_as_om();
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
        let spec = OdkRepo::spec_for(&root).expect("writing the standard file");
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

/// A two-class repository that has only ever had an `owlmake.yaml` asking for
/// the standard build, with `targets` as its own; `name` keeps each test's
/// scratch tree apart.
fn tiny_repo(name: &str, targets: &str) -> std::path::PathBuf {
    let mut root = std::env::temp_dir();
    root.push(format!("owlmake_builtin_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        format!(
            "emulate_odk_version: 1.6.1\n\
             id: tiny\n\
             uribase: http://example.org\n\
             edit_format: ofn\n\
             release_artefacts:\n- base\n- full\n\
             export_formats:\n- owl\n- obo\n\
             report:\n  custom_sparql_checks: []\n  custom_sparql_exports: []\n\
             targets:\n{targets}"
        ),
    )
    .unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(:=<http://example.org/tiny/>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/tiny.owl>\n\
         Declaration(Class(<http://example.org/tiny/TINY_0000001>))\n\
         Declaration(Class(<http://example.org/tiny/TINY_0000002>))\n\
         AnnotationAssertion(rdfs:label <http://example.org/tiny/TINY_0000001> \"thing\")\n\
         AnnotationAssertion(rdfs:label <http://example.org/tiny/TINY_0000002> \"small thing\")\n\
         SubClassOf(<http://example.org/tiny/TINY_0000002> <http://example.org/tiny/TINY_0000001>)\n\
         )\n",
    )
    .unwrap();
    root
}

/// `om <args> -C <root>`, with what it printed on both streams.
fn om_in(root: &Path, args: &[&str]) -> (std::process::Output, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_om"))
        .args(args)
        .arg("-C")
        .arg(root)
        .output()
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out, said)
}

/// A repository that has only ever had an `owlmake.yaml`: no Makefile, no
/// configuration of any other kind, nothing to regenerate from. The standard build
/// for its options has to BUILD — the plans agreeing is not the same as the
/// release coming out.
#[test]
fn a_standard_file_builds_a_release() {
    let root = tiny_repo(
        "builds",
        "- target: greeting\n  steps:\n  - op: print\n    message: built-its-own-way\n\
         - target: test\n  extends: true\n  needs:\n  - greeting\n\
         - target: farewell\n  steps:\n  - op: print\n    message: released-its-own-way\n\
         - target: prepare_release\n  extends: true\n  needs:\n  - farewell\n",
    );
    let ont = root.join("src/ontology");
    let om = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_om"))
            .args(args)
            .arg("-C")
            .arg(&root)
            .output()
            .unwrap()
    };

    let out = om(&["make", "tiny.owl", "tiny-base.owl", "tiny.obo"]);
    assert!(out.status.success(), "the build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let full = std::fs::read_to_string(ont.join("tiny.owl")).expect("tiny.owl was built");
    // A second build finds the artefact up to date and leaves it; `-B` runs its
    // recipe again regardless.
    let built = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).unwrap();
    let first = built(&ont.join("tiny.owl"));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = om(&["make", "tiny.owl"]);
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success() && said.contains("`tiny.owl` is up to date"), "{said}");
    assert_eq!(built(&ont.join("tiny.owl")), first, "an up-to-date artefact is left alone");
    let out = om(&["make", "-B", "tiny.owl"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(built(&ont.join("tiny.owl")) > first, "`-B` rebuilds an up-to-date artefact");
    assert!(full.contains("http://example.org/tiny/TINY_0000002"), "the release holds the ontology:\n{full}");
    assert!(
        full.contains("http://example.org/tiny/releases/"),
        "and is stamped with a version IRI under the configured base:\n{full}"
    );
    assert!(ont.join("tiny-base.owl").is_file() && ont.join("tiny.obo").is_file());

    // `test` is the standard target, with the repository's own check joined to it.
    let out = om(&["make", "test"]);
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "the checks failed:\n{said}");
    assert!(said.contains("built-its-own-way"), "the repository's own target ran:\n{said}");
    assert!(said.contains("Finished running all tests successfully"), "and so did the standard ones:\n{said}");

    // `om test` is `om make test` by another name, switches and all.
    let out = om(&["test", "MIR=false"]);
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "`om test MIR=false` failed:\n{said}");
    assert!(said.contains("Finished running all tests successfully"), "{said}");
    // …but it builds `test` and nothing else.
    let out = om(&["test", "tiny.obo"]);
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success() && said.contains("om make test tiny.obo"), "{said}");

    // `prepare_release` is what its plan entry needs — the checks, the artefacts
    // and what the repository added to it — and then the release, published at
    // the root.
    let (out, said) = om_in(&root, &["make", "prepare_release"]);
    assert!(out.status.success(), "the release failed:\n{said}");
    assert!(said.contains("Finished running all tests successfully"), "the release ran the checks:\n{said}");
    assert!(said.contains("released-its-own-way"), "and what the repository added to it:\n{said}");
    assert!(
        root.join("tiny.owl").is_file() && root.join("tiny-base.owl").is_file(),
        "and published the release at the root:\n{said}"
    );

    // The file is the build: nothing rewrote it.
    let file = std::fs::read_to_string(root.join("owlmake.yaml")).unwrap();
    assert!(file.starts_with("emulate_odk_version: 1.6.1\nid: tiny\n"), "{file}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Which output conventions a build emulates is a setting of its own, not a
/// property of which rules build it. Without `emulate_odk_version` the standard
/// build writes under owlmake's own conventions, so the OBO export carries the
/// ontology's individuals as `[Instance]` frames; naming a release emulates its
/// ROBOT, which wrote none — whichever release, since the rules stay owlmake's.
#[test]
fn a_standard_file_without_an_odk_release_writes_instance_frames() {
    let repo = |name: &str, version_line: &str| {
        let mut root = std::env::temp_dir();
        root.push(format!("owlmake_builtin_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ont = root.join("src/ontology");
        std::fs::create_dir_all(&ont).unwrap();
        std::fs::write(
            root.join("owlmake.yaml"),
            format!(
                "{version_line}id: tiny\n\
                 uribase: http://example.org\n\
                 edit_format: ofn\n\
                 release_artefacts:\n- full\n\
                 export_formats:\n- owl\n- obo\n\
                 report:\n  custom_sparql_checks: []\n  custom_sparql_exports: []\n"
            ),
        )
        .unwrap();
        std::fs::write(
            ont.join("tiny-edit.ofn"),
            "Prefix(tiny:=<http://example.org/tiny/>)\n\
             Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
             Ontology(<http://example.org/tiny.owl>\n\
             Declaration(Class(tiny:TINY_0000001))\n\
             Declaration(NamedIndividual(tiny:TINY_0000002))\n\
             AnnotationAssertion(rdfs:label tiny:TINY_0000001 \"cohort\")\n\
             AnnotationAssertion(rdfs:label tiny:TINY_0000002 \"a cohort\")\n\
             ClassAssertion(tiny:TINY_0000001 tiny:TINY_0000002)\n\
             )\n",
        )
        .unwrap();
        root
    };

    let root = repo("noodk", "");
    let (out, said) = om_in(&root, &["make", "tiny.obo"]);
    assert!(out.status.success(), "a file without `emulate_odk_version` builds:\n{said}");
    let obo = std::fs::read_to_string(root.join("src/ontology/tiny.obo")).unwrap();
    assert!(
        obo.contains("[Instance]") && obo.contains("id: tiny:TINY_0000002") && obo.contains("instance_of: tiny:TINY_0000001"),
        "the OBO export carries the individual as an [Instance] frame:\n{obo}"
    );
    let _ = std::fs::remove_dir_all(&root);

    let root = repo("odk", "emulate_odk_version: 1.6.1\n");
    let (out, said) = om_in(&root, &["make", "tiny.obo"]);
    assert!(out.status.success(), "{said}");
    let obo = std::fs::read_to_string(root.join("src/ontology/tiny.obo")).unwrap();
    assert!(!obo.contains("[Instance]"), "under ODK emulation the OBO export has no [Instance] frames:\n{obo}");
    let _ = std::fs::remove_dir_all(&root);

    let root = repo("other", "emulate_odk_version: 1.5.2\n");
    let (out, said) = om_in(&root, &["make", "tiny.obo"]);
    assert!(out.status.success(), "another release's conventions build with the same rules:\n{said}");
    let obo = std::fs::read_to_string(root.join("src/ontology/tiny.obo")).unwrap();
    assert!(!obo.contains("[Instance]"), "under that release's emulation the OBO export has no [Instance] frames:\n{obo}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A release that something it needs failed is built as far as it goes and left
/// unpublished, and the run says so and fails.
#[test]
fn a_release_that_fails_is_not_published() {
    let root = tiny_repo(
        "unpublished",
        "- target: broken\n  steps:\n  - op: shell\n    command: exit 1\n\
         - target: prepare_release\n  extends: true\n  needs:\n  - broken\n",
    );
    let (out, said) = om_in(&root, &["make", "prepare_release"]);
    assert!(!out.status.success(), "a release whose own addition failed succeeded:\n{said}");
    assert!(said.contains("not publishing the release"), "without saying it was not published:\n{said}");
    assert!(root.join("src/ontology/tiny.owl").is_file(), "the artefacts are still built:\n{said}");
    assert!(!root.join("tiny.owl").exists(), "but nothing is published:\n{said}");
    let _ = std::fs::remove_dir_all(&root);
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
    bundled_tools_as_om();
    let mut failed = false;
    for root in repos.split(':').filter(|r| !r.is_empty()).map(Path::new) {
        let before = resolved(&OdkRepo::load_with_builtin_rules(root).expect("loading"));
        let spec = OdkRepo::spec_for(root).expect("writing the standard file");
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
    bundled_tools_as_om();
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

/// The options a repository states by the tool ODK runs them with are written
/// under owlmake's own names, and the ones that configure a tool owlmake does
/// not run are left out. A file that states one of those is refused by name.
#[test]
fn tool_named_options_are_owlmakes_own_in_the_file() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odk-1.6.1/coho");
    let root = repository_for(&fixture, "keys");
    let spec = OdkRepo::spec_for(&root).expect("writing the standard file");
    let file = root.join("owlmake.yaml");
    owlmake::spec::save(&spec, &file).expect("saving owlmake.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\nreport:\n"), "{text}");
    for odk in ["robot_report", "robot_java_args"] {
        assert!(!text.contains(odk), "`{odk}` is ODK's name, not owlmake's:\n{text}");
    }

    let ont = root.join("src/ontology");
    for entry in std::fs::read_dir(&ont).unwrap().flatten() {
        let n = entry.file_name().to_string_lossy().to_string();
        if n == "Makefile" || n.ends_with(".Makefile") || n.ends_with("-odk.yaml") {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }
    OdkRepo::load(&root).expect("the file under owlmake's names loads");
    std::fs::write(&file, format!("{text}robot_java_args: -Xmx8G\n")).unwrap();
    let err = OdkRepo::load(&root).err().map(|e| format!("{e:#}")).unwrap_or_default();
    assert!(err.contains("robot_java_args") && err.contains("JVM"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

/// `release_property_assertions` asks the release's reason step for the
/// entailed object property assertions on the listed properties, and only
/// those; with `reasoner: hermit` that reaches the inverse of an asserted
/// relation.
#[test]
fn a_release_asserts_the_property_assertions_it_asks_for() {
    let mut root = std::env::temp_dir();
    root.push(format!("owlmake_builtin_{}_assertions", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        "emulate_odk_version: 1.6.1\n\
         id: tiny\n\
         uribase: http://example.org\n\
         edit_format: ofn\n\
         reasoner: hermit\n\
         release_artefacts:\n- full\n\
         export_formats:\n- owl\n\
         release_property_assertions:\n- tiny:hasSubCohort\n\
         report:\n  custom_sparql_checks: []\n  custom_sparql_exports: []\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(tiny:=<http://example.org/tiny/>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/tiny.owl>\n\
         Declaration(Class(tiny:TINY_0000001))\n\
         Declaration(ObjectProperty(tiny:hasSubCohort))\n\
         Declaration(ObjectProperty(tiny:isSubCohortOf))\n\
         Declaration(ObjectProperty(tiny:partOf))\n\
         Declaration(NamedIndividual(tiny:TINY_0000002))\n\
         Declaration(NamedIndividual(tiny:TINY_0000003))\n\
         Declaration(NamedIndividual(tiny:TINY_0000004))\n\
         AnnotationAssertion(rdfs:label tiny:TINY_0000001 \"cohort\")\n\
         InverseObjectProperties(tiny:hasSubCohort tiny:isSubCohortOf)\n\
         TransitiveObjectProperty(tiny:partOf)\n\
         ClassAssertion(tiny:TINY_0000001 tiny:TINY_0000002)\n\
         ClassAssertion(tiny:TINY_0000001 tiny:TINY_0000003)\n\
         ObjectPropertyAssertion(tiny:isSubCohortOf tiny:TINY_0000003 tiny:TINY_0000002)\n\
         ObjectPropertyAssertion(tiny:partOf tiny:TINY_0000002 tiny:TINY_0000003)\n\
         ObjectPropertyAssertion(tiny:partOf tiny:TINY_0000003 tiny:TINY_0000004)\n\
         )\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_om"))
        .args(["make", "tiny.owl", "-C"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(out.status.success(), "the build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let full = owlmake::io::load(&ont.join("tiny.owl")).expect("tiny.owl was built");
    let assertions: Vec<(String, String, String)> = full
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            horned_owl::model::Component::ObjectPropertyAssertion(a) => match (&a.ope, &a.from, &a.to) {
                (
                    horned_owl::model::ObjectPropertyExpression::ObjectProperty(p),
                    horned_owl::model::Individual::Named(f),
                    horned_owl::model::Individual::Named(t),
                ) => Some((p.0.to_string(), f.0.to_string(), t.0.to_string())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let t = |p: &str, f: &str, t: &str| {
        (
            format!("http://example.org/tiny/{p}"),
            format!("http://example.org/tiny/TINY_000000{f}"),
            format!("http://example.org/tiny/TINY_000000{t}"),
        )
    };
    assert!(
        assertions.contains(&t("hasSubCohort", "2", "3")),
        "the release carries the inverse assertion on the listed property: {assertions:?}"
    );
    assert!(
        !assertions.contains(&t("partOf", "2", "4")),
        "the closure of the unlisted transitive property is not asserted: {assertions:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// With the imports kept, each import module is a stage of the release, and the
/// stage closes on what it did: a module on disk is kept, and an absent one is
/// built from its pipeline this once — or refused, when the run pinned the
/// imports with `IMP=false`. A line after the stages counts both, so the log says
/// whether any module changed.
#[test]
fn a_kept_import_stage_says_whether_it_rebuilt_the_module() {
    let mut root = std::env::temp_dir();
    root.push(format!("owlmake_builtin_{}_kept_imports", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    for dir in ["imports", "mirror"] {
        std::fs::create_dir_all(ont.join(dir)).unwrap();
    }
    std::fs::create_dir_all(root.join("src/sparql")).unwrap();
    let file = |imports: &str| {
        format!(
            "emulate_odk_version: 1.6.1\n\
             id: tiny\n\
             uribase: http://example.org\n\
             edit_format: ofn\n\
             release_artefacts:\n- full\n\
             export_formats:\n- owl\n\
             import_group:\n{imports}  products:\n  - id: x\n\
             report:\n  custom_sparql_checks: []\n  custom_sparql_exports: []\n"
        )
    };
    std::fs::write(root.join("owlmake.yaml"), file("")).unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/tiny.owl>\n\
         Declaration(Class(<http://example.org/tiny/TINY_0000001>))\n\
         SubClassOf(<http://example.org/tiny/TINY_0000001> <http://example.org/x/X_1>)\n\
         )\n",
    )
    .unwrap();
    // The mirror is on disk and every run pins it, so nothing is fetched.
    std::fs::write(
        ont.join("mirror/x.owl"),
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/x.owl>\n\
         Declaration(Class(<http://example.org/x/X_1>))\n\
         Declaration(Class(<http://example.org/x/X_2>))\n\
         SubClassOf(<http://example.org/x/X_1> <http://example.org/x/X_2>)\n\
         AnnotationAssertion(rdfs:label <http://example.org/x/X_1> \"x one\")\n\
         )\n",
    )
    .unwrap();
    std::fs::write(ont.join("imports/x_terms.txt"), "http://example.org/x/X_1\n").unwrap();
    std::fs::write(
        root.join("src/sparql/terms.sparql"),
        "SELECT DISTINCT ?term WHERE { { ?s ?p ?term . } UNION { ?term ?p2 ?o . } FILTER(isIRI(?term)) }\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         </catalog>\n",
    )
    .unwrap();
    let om = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_om"))
            .args(args)
            .args(["MIR=false", "-C"])
            .arg(&root)
            .env_remove("OWLMAKE_PROGRESS")
            .env_remove("OWLMAKE_COLOR")
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stderr).to_string())
    };
    let module = ont.join("imports/x_import.owl");

    // Pinned and absent: nothing may build it.
    let (ok, said) = om(&["make", "tiny.owl", "IMP=false"]);
    assert!(!ok && said.contains("pinned by IMP=false but is not present"), "{said}");
    assert!(!module.exists(), "a module pinned by IMP=false was built");

    // Kept by default and absent: built this once, and the log says so.
    let (ok, said) = om(&["make", "tiny.owl", "--imports", "cached"]);
    assert!(ok, "the build failed:\n{said}");
    assert!(said.contains("✓ rebuilt (") && said.contains("imports: 0 kept, 1 rebuilt"), "{said}");
    let built = std::fs::read(&module).expect("the absent module was built");
    let stamp = std::fs::metadata(&module).and_then(|m| m.modified()).unwrap();

    // On disk: kept as it is.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (ok, said) = om(&["make", "tiny.owl", "IMP=false"]);
    assert!(ok, "the build failed:\n{said}");
    assert!(said.contains("✓ kept (") && said.contains("imports: 1 kept, 0 rebuilt"), "{said}");
    assert!(!said.contains("✓ rebuilt"), "{said}");
    assert_eq!(std::fs::read(&module).unwrap(), built, "a kept module was rewritten");
    assert_eq!(std::fs::metadata(&module).and_then(|m| m.modified()).unwrap(), stamp);

    // A merged import is the one module the release reads, and it is kept the
    // same way.
    std::fs::write(root.join("owlmake.yaml"), file("  use_base_merging: true\n")).unwrap();
    std::fs::copy(ont.join("mirror/x.owl"), ont.join("imports/merged_import.owl")).unwrap();
    let (ok, said) = om(&["make", "tiny.owl", "IMP=false"]);
    assert!(ok, "the build failed:\n{said}");
    assert!(said.contains("imports: `imports/merged_import.owl` kept"), "{said}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A rule of the repository's own that needs `.FORCE` is covered from
/// `owlmake.yaml` alone. UBERON fetches the mapping sets it merges into its
/// released one again on every build this way; `.FORCE` names no file, and the
/// standard build declares it phony, so the release that reaches those rules is
/// not refused for want of it.
#[test]
fn a_force_prerequisite_is_covered_from_the_file_alone() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odk-1.6.1/own-force");
    let root = repository_for(&fixture, "force");
    let spec = OdkRepo::spec_for(&root).expect("writing the standard file");
    owlmake::spec::save(&spec, &root.join("owlmake.yaml")).expect("saving owlmake.yaml");
    let ont = root.join("src/ontology");
    for name in ["Makefile", "force.Makefile", "force-odk.yaml"] {
        std::fs::remove_file(ont.join(name)).unwrap();
    }
    let plan = OdkRepo::load(&root)
        .expect("loading from owlmake.yaml alone")
        .plan(&[])
        .expect("planning");
    let forced: Vec<String> =
        plan.blocking_gaps().into_iter().filter(|g| g.contains(".FORCE")).collect();
    assert!(forced.is_empty(), "the release is refused for want of `.FORCE`: {forced:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A `$(shell …)` in a repository's own rules runs owlmake's own tools when the
/// repository is planned in this process, as it does under `om`. UBERON lists
/// the bridges a merge reads with `ls … | grep -v …`; the merge reads what that
/// lists.
#[test]
fn a_shell_substitution_runs_the_bundled_tools() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odk-1.6.1/own-shell");
    let root = repository_for(&fixture, "shell");
    let plan = OdkRepo::load_with_builtin_rules(&root)
        .expect("loading the built-in rules")
        .plan(&[])
        .expect("planning");
    let pieces = plan
        .prerequisites
        .iter()
        .chain(plan.artefacts.iter())
        .find(|a| a.target == "tmp/pieces.owl")
        .expect("the merge of the pieces is planned");
    assert_eq!(pieces.needs, ["pieces/a.owl", "pieces/b.owl"], "what `ls … | grep -v …` lists");
    let _ = std::fs::remove_dir_all(&root);
}

/// A two-class repository built from the `owlmake.yaml` given, in a scratch
/// tree of its own; `name` keeps each test's tree apart.
fn file_repo(name: &str, file: &str) -> std::path::PathBuf {
    let mut root = std::env::temp_dir();
    root.push(format!("owlmake_builtin_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(root.join("owlmake.yaml"), file).unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(:=<http://example.org/tiny/>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/tiny.owl>\n\
         Declaration(Class(<http://example.org/tiny/TINY_0000001>))\n\
         Declaration(Class(<http://example.org/tiny/TINY_0000002>))\n\
         AnnotationAssertion(rdfs:label <http://example.org/tiny/TINY_0000001> \"thing\")\n\
         SubClassOf(<http://example.org/tiny/TINY_0000002> <http://example.org/tiny/TINY_0000001>)\n\
         )\n",
    )
    .unwrap();
    root
}

const TINY_OPTIONS: &str = "id: tiny\n\
    uribase: http://example.org\n\
    edit_format: ofn\n\
    release_artefacts:\n- full\n\
    export_formats:\n- owl\n\
    report:\n  custom_sparql_checks: []\n  custom_sparql_exports: []\n";

/// A file states what the standard build would otherwise derive — the release
/// version, the ontology IRI — and keeps the standard build. Every standard
/// target is still there, the release is stamped with the stated version, and
/// the plan names the stated IRI.
#[test]
fn a_file_states_its_version_and_iri_over_the_standard_build() {
    let root = file_repo(
        "overrides",
        &format!(
            "{TINY_OPTIONS}version: '2001-02-03'\n\
             ontology_iri: http://example.org/tiny-as-named.owl\n"
        ),
    );
    let (out, said) = om_in(&root, &["make", "--list-targets"]);
    assert!(out.status.success(), "{said}");
    for standard in ["tiny-full.owl", "test", "prepare_release", "reason_test"] {
        assert!(
            said.lines().any(|l| l == standard),
            "the standard target `{standard}` is missing: stating a version lost the standard build:\n{said}"
        );
    }
    let (out, said) = om_in(&root, &["make", "--plan-only"]);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("http://example.org/tiny-as-named.owl"), "the stated IRI is not the plan's:\n{said}");
    let (out, said) = om_in(&root, &["make", "tiny.owl"]);
    assert!(out.status.success(), "the build failed:\n{said}");
    let full = std::fs::read_to_string(root.join("src/ontology/tiny.owl")).unwrap();
    assert!(
        full.contains("http://example.org/tiny/releases/2001-02-03/tiny.owl"),
        "the release is not stamped with the stated version:\n{full}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// What a file cannot say is refused by name: an option of the standard build in
/// a build of the repository's own, a build of its own that leaves its identity
/// out, the merged import stated beside `import_group`, an import stated its own
/// way that the standard build does not have, and a key nothing reads.
#[test]
fn a_file_is_refused_for_what_its_base_cannot_honour() {
    let cases: [(&str, &str, &[&str]); 5] = [
        (
            "own-with-option",
            "use_builtin_rules: false\nid: tiny\nversion: '1'\nontology_iri: http://example.org/tiny.owl\n\
             reasoner: ELK\nrelease_artefacts:\n- full\n",
            &["release_artefacts", "use_builtin_rules"],
        ),
        (
            "own-without-version",
            "use_builtin_rules: false\nid: tiny\nontology_iri: http://example.org/tiny.owl\nreasoner: ELK\n",
            &["version", "use_builtin_rules: false"],
        ),
        ("merged-beside-group", &format!("{TINY_OPTIONS}use_base_merging: true\n"), &["use_base_merging", "import_group"]),
        (
            "import-not-listed",
            &format!(
                "{TINY_OPTIONS}imports:\n- id: zzz\n  source: http://example.org/zzz.owl\n\
                 \x20 output: src/ontology/imports/zzz_import.owl\n"
            ),
            &["zzz", "import_group"],
        ),
        ("old-key", &format!("{TINY_OPTIONS}artefacts: []\n"), &["artefacts"]),
    ];
    for (name, file, words) in cases {
        let root = file_repo(name, file);
        let err = OdkRepo::load(&root).err().map(|e| format!("{e:#}")).unwrap_or_default();
        for w in words {
            assert!(err.contains(w), "{name}: the refusal does not name `{w}`: {err:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
