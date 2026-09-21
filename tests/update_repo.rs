//! **The oracle for `update_repo`.**
//!
//! Each fixture under `tests/fixtures/odk-<version>-update/` is a repository
//! before ODK's own `odk.py update` ran on it (`before/`, its configuration
//! carried as an `owlmake.yaml`) and the files that command left (`after/`),
//! written by `scripts/gen_odk_update_fixture.sh` — never by hand. owlmake's
//! `update_repo` has to leave the same bytes, and nothing else.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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

fn files(root: &Path) -> BTreeSet<PathBuf> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeSet<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&root.join("src"), root, &mut out);
    out
}

fn om(root: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_om")).args(args).arg("-C").arg(root).output().unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let mut root = std::env::temp_dir();
    root.push(format!("owlmake_update_repo_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

#[test]
fn update_repo_leaves_what_odk_leaves() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odk-1.6.1-update");
    let mut names: Vec<_> = std::fs::read_dir(&fixtures)
        .expect("the fixture directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("before/owlmake.yaml").exists())
        .collect();
    names.sort();
    assert!(names.len() >= 4, "fixtures under {}", fixtures.display());
    let mut problems = Vec::new();
    for fixture in &names {
        let name = fixture.file_name().unwrap().to_string_lossy().to_string();
        let root = scratch(&name);
        copy_tree(&fixture.join("before"), &root);
        let out = om(&root, &["update-repo"]);
        assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));

        let expected = files(&fixture.join("after"));
        for file in &expected {
            let (want, got) = (std::fs::read(fixture.join("after").join(file)).unwrap(), std::fs::read(root.join(file)));
            if got.as_ref().ok() != Some(&want) {
                problems.push(format!("{name}: {} differs from what ODK wrote", file.display()));
            }
        }
        let untouched = files(&fixture.join("before"));
        for file in files(&root) {
            if !expected.contains(&file) && !untouched.contains(&file) {
                problems.push(format!("{name}: {} was written and ODK writes no such file", file.display()));
            }
        }

        // A second run has nothing left to do.
        let before: Vec<_> = files(&root).into_iter().map(|f| (std::fs::read(root.join(&f)).unwrap(), f)).collect();
        assert!(om(&root, &["make", "update_repo"]).status.success());
        for (bytes, file) in before {
            if std::fs::read(root.join(&file)).unwrap() != bytes {
                problems.push(format!("{name}: {} changed on a second run", file.display()));
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// What a repository has made its own is kept: a module, a component or a
/// template that exists is not replaced by its placeholder.
#[test]
fn update_repo_keeps_what_is_there() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odk-1.6.1-update/sink/before");
    let root = scratch("keeps");
    copy_tree(&fixture, &root);
    let mine = [
        "src/ontology/components/plain.owl",
        "src/ontology/imports/ro_terms.txt",
        "src/templates/one.tsv",
        "src/ontology/sink-idranges.owl",
    ];
    for file in mine {
        std::fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
        std::fs::write(root.join(file), "the repository's own\n").unwrap();
    }
    assert!(om(&root, &["update-repo"]).status.success());
    for file in mine {
        assert_eq!(std::fs::read_to_string(root.join(file)).unwrap(), "the repository's own\n", "{file}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A repository whose build is a generated Makefile is ODK's to update.
#[test]
fn update_repo_is_for_a_standard_file() {
    let root = scratch("not_standard");
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(ont.join("foo-odk.yaml"), "id: foo\n").unwrap();
    std::fs::write(ont.join("Makefile"), "foo.owl: foo-edit.owl\n\trobot merge -i $< -o $@\n").unwrap();
    std::fs::write(ont.join("foo-edit.owl"), "Ontology(<http://example.org/foo.owl>)\n").unwrap();
    let out = om(&root, &["make", "update_repo"]);
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success() && said.contains("not built from one"), "{said}");
    assert!(!ont.join("../sparql").exists(), "and nothing was written");
    let _ = std::fs::remove_dir_all(&root);
}
