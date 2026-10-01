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

use owlmake::build::Emulation;
use owlmake::io::{self, RunOptions};

/// The tests here read and write process state, so they take turns.
static PROCESS_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn an_invocation_starts_from_the_defaults() {
    let _turn = PROCESS_STATE.lock().unwrap_or_else(|e| e.into_inner());
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

/// An owlmake process a build starts is told the build's emulation ahead of its
/// command, and runs under it: emulating ODK, it writes an individual as that
/// release does, with no `[Instance]` frame. The invocation after it starts from
/// no emulation again, and arguments that name none are refused.
#[test]
fn an_invocation_runs_under_the_emulation_it_is_started_with() {
    let _turn = PROCESS_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("owlmake_invemu_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let inp = dir.join("in.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\nOntology(<http://x.org/o>\nDeclaration(Class(:A))\n\
         Declaration(NamedIndividual(:a))\nClassAssertion(:A :a)\n)\n",
    )
    .unwrap();
    let convert = |out: &str| -> Vec<String> {
        let out = dir.join(out).display().to_string();
        ["convert", "-i", &inp.display().to_string(), "-o", &out].map(String::from).to_vec()
    };
    let emulation = Emulation { robot: (1, 9, 10), odk: Some((1, 6, 1)) };
    owlmake::build::set_emulation(Some(emulation));
    let mut argv = owlmake::build::emulation_args();
    owlmake::build::set_emulation(None);
    argv.extend(convert("emulated.obo"));
    assert_eq!(owlmake::cli::run_argv(argv), 0, "the emulated invocation should succeed");
    assert_eq!(owlmake::build::emulation(), Some(emulation));
    let emulated = std::fs::read_to_string(dir.join("emulated.obo")).unwrap();
    assert!(!emulated.contains("[Instance]"), "not written as ODK 1.6.1 writes:\n{emulated}");

    assert_eq!(owlmake::cli::run_argv(convert("plain.obo")), 0);
    assert_eq!(owlmake::build::emulation(), None, "a later invocation inherited the emulation");
    let plain = std::fs::read_to_string(dir.join("plain.obo")).unwrap();
    assert!(plain.contains("[Instance]"), "the individual was not written:\n{plain}");

    // Under ROBOT 1.9.8 an SSSOM CURIE resolves through that release's prefix map,
    // and the invocation after it is back on the default one.
    // The maps are compared, not printed: each is several hundred prefixes.
    let default_map = owlmake::sssom::converter::obo_epm();
    let mut argv = vec!["__emulate-robot-version=1.9.8".to_string()];
    argv.extend(convert("robot-1.9.8.obo"));
    assert_eq!(owlmake::cli::run_argv(argv), 0);
    assert!(owlmake::sssom::converter::obo_epm() != default_map, "not the 1.9.8 prefix map");
    assert_eq!(owlmake::cli::run_argv(convert("after.obo")), 0);
    assert!(
        owlmake::sssom::converter::obo_epm() == default_map,
        "a later invocation inherited the 1.9.8 prefix map"
    );

    for bad in ["__emulate-robot-version=1.9", "__emulate-odk-version=1.6.1"] {
        let mut argv = vec![bad.to_string()];
        argv.extend(convert("refused.obo"));
        assert_eq!(owlmake::cli::run_argv(argv), 2, "`{bad}` was not refused");
    }
    assert!(!dir.join("refused.obo").exists());

    let _ = std::fs::remove_dir_all(&dir);
}
