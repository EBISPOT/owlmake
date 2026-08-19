//! One invocation must not inherit another's options.
//!
//! `--strict` and `--xml-entities`, and the serialization conventions a plan
//! settles, are process-wide: they are read deep inside the loaders and writers,
//! where threading them through would reach into the serializers themselves.
//! That is sound only because each is established at the START of a run and
//! lasts exactly one run.
//!
//! For the binary that is free — the process exits. A host that calls owlmake
//! in-process does not: the Python package runs every command through
//! `run_argv` in one long-lived process, so without a reset a flag given to one
//! call would go on applying to every later one, and a plan's conventions would
//! outlive the build that chose them. Both are outputs decided by something
//! other than the run that produced them.
//!
//! This lives in its own test binary on purpose. The options under test are
//! process state, so a test sharing a process with others that parse or write
//! ontologies would both perturb them and be perturbed by them.

use owlmake::io::{self, RunOptions};

#[test]
fn an_invocation_starts_from_the_defaults() {
    assert!(!io::run_options().strict, "the process starts unlatched");

    // A run latches its flags on — and, being a latch, never turns them off.
    io::latch_run_options(RunOptions { strict: true, xml_entities: true });
    let latched = io::run_options();
    assert!(latched.strict && latched.xml_entities, "latching is what `activate` does");

    // The next invocation clears them, before it dispatches anything.
    let dir = std::env::temp_dir().join(format!("owlmake_invopts_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let inp = dir.join("in.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\nOntology(<http://x.org/o>\nDeclaration(Class(:A))\n)\n",
    )
    .unwrap();
    let out = dir.join("out.ofn");
    let code = owlmake::cli::run_argv(vec![
        "convert".to_string(),
        "-i".to_string(),
        inp.display().to_string(),
        "-o".to_string(),
        out.display().to_string(),
        "--format".to_string(),
        "ofn".to_string(),
    ]);
    assert_eq!(code, 0, "the second invocation should succeed");

    let after = io::run_options();
    assert!(
        !after.strict && !after.xml_entities,
        "a later invocation inherited the previous one's flags: {after:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
