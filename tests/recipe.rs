//! End-to-end tests for the recipe interpreter ([`owlmake::build::recipe`]): a
//! recipe line, with its variables already expanded, is decomposed and executed
//! in-process. A tool name such as `robot` or `jq` at command position is
//! rewritten to the owlmake binary's matching subcommand, and file operations
//! run natively, so such a name never resolves to a system `robot` or `jq`. A
//! script the recipe shells out to reaches those same subcommands through a
//! shim directory prepended to the shell's `PATH`.

use std::path::{Path, PathBuf};

use owlmake::build::recipe;

const BIN: &str = env!("CARGO_BIN_EXE_om");

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owlmake_recipe_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(line: &str, dir: &Path) {
    recipe::run_line(line, dir, Path::new(BIN), "robot", &[]).expect(line);
}

/// A small mixed recipe: a `robot` chain (dispatched to the owlmake binary), a
/// native file copy, and a `jq` pipeline with a redirect (shell-rewritten to
/// owlmake's own `jq`). All three must run end-to-end.
#[test]
fn mixed_recipe_runs_end_to_end() {
    let dir = workdir("mixed");
    std::fs::write(
        dir.join("in.ofn"),
        "Prefix(:=<http://x/>)\nOntology(\nDeclaration(Class(:A))\nDeclaration(Class(:B))\nSubClassOf(:A :B)\n)\n",
    )
    .unwrap();

    // `robot` chain → owlmake binary (file in / file out, no PATH dependency).
    run("robot convert -i in.ofn -o out.ofn --format ofn", &dir);
    assert!(dir.join("out.ofn").is_file(), "robot convert produced no output");
    assert!(std::fs::read_to_string(dir.join("out.ofn")).unwrap().contains("SubClassOf"));

    // Native file copy (no shell).
    run("cp out.ofn final.ofn", &dir);
    assert_eq!(
        std::fs::read_to_string(dir.join("final.ofn")).unwrap(),
        std::fs::read_to_string(dir.join("out.ofn")).unwrap()
    );

    // jq pipeline with a redirect → owlmake's own `jq` via explicit-path rewrite.
    run("echo '{\"v\":42}' | jq -r .v > num.txt", &dir);
    assert_eq!(std::fs::read_to_string(dir.join("num.txt")).unwrap().trim(), "42");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A recipe that shells out to an *external script* which itself calls bare
/// `jq` must still resolve `jq` to owlmake's own — via the shim dir the
/// interpreter prepends to the shell's PATH (the case the in-line rewrite can't
/// cover, since the call lives inside the script, not the recipe line).
#[test]
fn external_script_resolves_bundled_tool_via_shim() {
    let dir = workdir("shim");
    std::fs::write(
        dir.join("nested.sh"),
        "#!/bin/sh\necho '{\"k\":7}' | jq -r .k > out.txt\n",
    )
    .unwrap();
    // The shim dir is prepended to PATH, so it wins even if a system jq exists.
    run("sh nested.sh", &dir);
    assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap().trim(), "7");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `&&`/`;`-separated commands run in sequence, and a leading `-` makes a failing
/// line non-fatal (the ignore-errors prefix a recipe line may carry).
#[test]
fn sequencing_and_ignore_errors() {
    let dir = workdir("seq");
    run("mkdir -p a/b && touch a/b/c.txt ; touch a/d.txt", &dir);
    assert!(dir.join("a/b/c.txt").exists());
    assert!(dir.join("a/d.txt").exists());

    // A bare failing line aborts…
    assert!(recipe::run_line("rm does-not-exist", &dir, Path::new(BIN), "robot", &[]).is_err());
    // …but with the `-` prefix it is ignored.
    recipe::run_line("-rm does-not-exist", &dir, Path::new(BIN), "robot", &[]).unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `make` the shell reaches inside a control construct — UBERON's
/// `if [ ! -f mirror/ncbitaxondisjoints.owl ]; then make mirror/ncbitaxondisjoints.owl
/// MIR=true IMP=true ; fi` — is owlmake's own, and builds the target from the
/// repository's plan. There is no Makefile for any other `make` to read.
#[test]
fn a_make_inside_a_shell_construct_builds_from_the_plan() {
    let root = workdir("submake");
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        "emulate_odk_version: 1.6.1\n\
         id: tiny\n\
         uribase: http://example.org\n\
         edit_format: ofn\n\
         targets:\n\
         - target: src/ontology/made.txt\n\
         \x20 steps:\n\
         \x20 - op: print\n\
         \x20   message: made-by-the-plan\n\
         \x20   dst: src/ontology/made.txt\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(:=<http://example.org/tiny/>)\nOntology(<http://example.org/tiny.owl>\n\
         Declaration(Class(<http://example.org/TINY_0000001>))\n)\n",
    )
    .unwrap();
    run("if [ ! -f made.txt ]; then make made.txt ; fi && cp made.txt copy.txt", &ont);
    assert_eq!(
        std::fs::read_to_string(ont.join("copy.txt")).unwrap().trim(),
        "made-by-the-plan"
    );
    let _ = std::fs::remove_dir_all(&root);
}


/// A plan-only repository whose one target is the shell command `command`, run
/// with `om make linked.owl` in its ontology directory next to `source.owl`.
fn build_linked(name: &str, command: &str) -> (PathBuf, bool) {
    let root = workdir(name);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        format!(
            "emulate_odk_version: 1.6.1\n\
             id: tiny\n\
             uribase: http://example.org\n\
             edit_format: ofn\n\
             targets:\n\
             - target: src/ontology/linked.owl\n\
             \x20 steps:\n\
             \x20 - op: shell\n\
             \x20   command: {command}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(:=<http://example.org/tiny/>)\nOntology(<http://example.org/tiny.owl>\n)\n",
    )
    .unwrap();
    std::fs::write(ont.join("source.owl"), SOURCE).unwrap();
    let ok = std::process::Command::new(BIN)
        .args(["make", "linked.owl"])
        .current_dir(&ont)
        .output()
        .expect("running om")
        .status
        .success();
    (root, ok)
}

const SOURCE: &str = "Prefix(:=<http://example.org/src/>)\nOntology(<http://example.org/src.owl>\n\
                      Declaration(Class(:A))\n)\n";

/// A rule whose command names its target and has no ontology before it — it
/// writes the target itself — leaves nothing behind when the command fails, so
/// the next build does not find the target made.
#[test]
fn a_failed_command_leaves_no_target_behind() {
    let (root, ok) = build_linked("failed-link", "false && ln -f -s source.owl linked.owl");
    assert!(!ok, "the build succeeded");
    let linked = root.join("src/ontology/linked.owl");
    assert!(
        std::fs::symlink_metadata(&linked).is_err(),
        "a failed command left `linked.owl` behind"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// …and when it succeeds, what it made is the target: the link, with the file it
/// links to untouched.
#[test]
fn a_command_that_makes_its_target_keeps_what_it_made() {
    let (root, ok) = build_linked("made-link", "ln -f -s source.owl linked.owl");
    assert!(ok, "the build failed");
    let ont = root.join("src/ontology");
    assert_eq!(std::fs::read_link(ont.join("linked.owl")).unwrap(), Path::new("source.owl"));
    assert_eq!(std::fs::read_to_string(ont.join("source.owl")).unwrap(), SOURCE);
    let _ = std::fs::remove_dir_all(&root);
}
