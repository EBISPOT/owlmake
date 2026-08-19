//! **The acceptance test for owlmake's whole purpose.**
//!
//! owlmake exists so that an ontology repository can delete its `Makefile` and
//! its ODK files and still build. A green test suite does not demonstrate that;
//! only this does: plan the repo, MOVE THE BUILD FILES OUT OF THE TREE, and run
//! the same things again.
//!
//! The fixture deliberately carries the shapes a plan is likeliest to lose,
//! because a fixture that exercises none of them proves nothing:
//!
//!   * a default goal that is MORE than the release (`all: … release qc`), so a
//!     bare build that stops at the release artefacts leaves the QC unrun while
//!     still reporting success;
//!   * an `include`d second Makefile that EXTENDS a target: its prerequisites
//!     must MERGE with the main file's, since replacing them drops a whole
//!     multi-member QC pipeline (OBA's `test` has seven members);
//!   * a recipe body wrapped in `ifneq … endif`, which a line-wise reader drops
//!     whole;
//!   * a non-default `SPARQLDIR` and a multi-value `--queries`, which a
//!     single-value flag parser records as one query;
//!   * an `<id>-edit.ofn` (not `.owl`), so the extension-order probe cannot be
//!     what finds it;
//!   * a `catalog-v001.xml`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_om"))
}

fn scratch(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("owlmake_planonly_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Build the fixture repo and return its root.
fn fixture(root: &Path) {
    let ont = root.join("src/ontology");

    // A tiny edit ontology, in OFN — NOT `.owl`, so nothing can find it by
    // probing extensions in ODK's conventional order.
    write(
        &ont.join("tiny-edit.ofn"),
        "Prefix(:=<http://example.org/tiny/>)\n\
         Ontology(<http://example.org/tiny.owl>\n\
         Declaration(Class(<http://example.org/tiny/TINY_0000001>))\n\
         Declaration(Class(<http://example.org/tiny/TINY_0000002>))\n\
         SubClassOf(<http://example.org/tiny/TINY_0000002> <http://example.org/tiny/TINY_0000001>)\n\
         )\n",
    );

    // A bridge that the main line depends on UNCONDITIONALLY while only its own
    // rule is guarded — UBERON's shape, where `$(POSTPROCESS_SRC)` names
    // `bridge/uberon-bridge-to-bfo.owl` whether or not the bridges are refreshed.
    // Keeping the group must leave this file alone, not drop it from the graph.
    write(
        &ont.join("bridge/tiny-bridge.owl"),
        "Prefix(:=<http://example.org/bridge/>)\n\
         Ontology(<http://example.org/tiny-bridge.owl>\n\
         Declaration(Class(<http://example.org/bridge/COMMITTED>))\n\
         )\n",
    );

    write(&ont.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         </catalog>\n");

    // A violation query that must return no rows, in a NON-default directory.
    write(
        &root.join("src/sparql/dangling-violation.sparql"),
        "SELECT ?entity ?property ?value WHERE {\n\
         ?entity <http://www.w3.org/2000/01/rdf-schema#subClassOf> ?value .\n\
         FILTER(?entity = <http://example.org/nope>)\n\
         BIND(\"x\" AS ?property)\n}\n",
    );
    write(
        &root.join("src/sparql/orphan-violation.sparql"),
        "SELECT ?entity ?property ?value WHERE {\n\
         ?entity <http://www.w3.org/2000/01/rdf-schema#label> ?value .\n\
         FILTER(?entity = <http://example.org/nope>)\n\
         BIND(\"x\" AS ?property)\n}\n",
    );

    // The main Makefile. `all` is MORE than the release.
    write(
        &ont.join("Makefile"),
        "ONT = tiny\n\
         SRC = tiny-edit.ofn\n\
         SPARQLDIR = ../sparql\n\
         VCHECKS = dangling orphan\n\
         VQUERIES = $(foreach V,$(VCHECKS),$(SPARQLDIR)/$V-violation.sparql)\n\
         ROBOT = robot\n\
         TODAY ?= $(shell date +%Y-%m-%d)\n\
         VERSION = $(TODAY)\n\
         BRI = true\n\
         DEBUG = false\n\
         \n\
         all: release qc\n\
         \n\
         .PHONY: all qc release sparql_test greet\n\
         \n\
         release: tiny.owl\n\
         \n\
         tiny.owl: $(SRC) bridge/tiny-bridge.owl stamp-bridges\n\
         \t$(ROBOT) annotate -i $< --version-iri http://example.org/tiny/releases/$(VERSION)/tiny.owl -o $@\n\
         \n\
         ifeq ($(BRI),true)\n\
         bridge/tiny-bridge.owl: $(SRC)\n\
         \t$(ROBOT) convert -i $< -o $@\n\
         stamp-bridges: bridge/tiny-bridge.owl\n\
         \techo regenerated > $@\n\
         else\n\
         stamp-bridges:\n\
         \ttouch $@\n\
         endif\n\
         \n\
         ifeq ($(DEBUG),true)\n\
         EXTRA = --debug\n\
         else\n\
         EXTRA =\n\
         endif\n\
         \n\
         qc: sparql_test\n\
         \n\
         sparql_test: $(SRC)\n\
         ifneq ($(VQUERIES),)\n\
         \t$(ROBOT) verify -i $< --queries $(VQUERIES) -O reports/\n\
         endif\n\
         \n\
         include tiny.Makefile\n",
    );

    // The override file EXTENDS `qc`: its prerequisites must merge with the
    // main Makefile's, not replace them.
    write(
        &ont.join("tiny.Makefile"),
        "qc: greet\n\
         \n\
         greet:\n\
         \t@echo hello from the override makefile\n",
    );
}

/// Snapshot every target the plan can name, as the build sees them.
fn list_targets(dir: &Path) -> String {
    let out = bin().args(["make", "--list-targets", "-C"]).arg(dir).output().unwrap();
    assert!(
        out.status.success(),
        "--list-targets failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn a_repo_can_delete_its_makefile_and_still_build() {
    let root = scratch("repo");
    fixture(&root);
    let ont = root.join("src/ontology");

    // ---- A: with the build files present -------------------------------
    let plan_out = bin().args(["make", "--plan-only", "-C"]).arg(&ont).output().unwrap();
    assert!(
        plan_out.status.success(),
        "planning failed:\n{}",
        String::from_utf8_lossy(&plan_out.stderr)
    );
    let plan_file = root.join("owlmake.yaml");
    assert!(plan_file.is_file(), "expected a generated {}", plan_file.display());
    let plan_text = std::fs::read_to_string(&plan_file).unwrap();

    // The plan must record what the Makefile SAID, not a subset of it.
    assert!(
        plan_text.contains("dangling-violation.sparql")
            && plan_text.contains("orphan-violation.sparql"),
        "both --queries values must survive ingest (a list-valued ROBOT flag used to keep one):\n{plan_text}"
    );
    assert!(
        plan_text.contains("greet"),
        "an included Makefile that extends a target must MERGE, not replace it:\n{plan_text}"
    );
    assert!(
        plan_text.contains("tiny-edit.ofn"),
        "the edit file must be named by the plan, not found by probing extensions:\n{plan_text}"
    );
    assert!(
        plan_text.contains("default_targets"),
        "the resolved default goal must be recorded — after the Makefile is gone \
         nothing else knows `all` meant `release qc`:\n{plan_text}"
    );

    let targets_a = list_targets(&ont);
    for want in ["qc", "sparql_test", "greet", "tiny.owl"] {
        assert!(targets_a.contains(want), "target `{want}` missing from:\n{targets_a}");
    }

    // ---- move the build files OUT OF THE TREE --------------------------
    let stash = scratch("stash");
    std::fs::create_dir_all(&stash).unwrap();
    std::fs::rename(ont.join("Makefile"), stash.join("Makefile")).unwrap();
    std::fs::rename(ont.join("tiny.Makefile"), stash.join("tiny.Makefile")).unwrap();
    assert!(!ont.join("Makefile").exists());

    // ---- B: from the plan alone ----------------------------------------
    let targets_b = list_targets(&ont);
    assert_eq!(
        targets_a, targets_b,
        "the runnable target surface changed when the Makefile was removed"
    );

    // A named target still runs.
    let greet = bin().args(["make", "greet", "-C"]).arg(&ont).output().unwrap();
    assert!(
        greet.status.success(),
        "`om make greet` failed with no Makefile:\n{}",
        String::from_utf8_lossy(&greet.stderr)
    );

    // The repo's own QC target still runs — and it is `qc`, not a built-in.
    let qc = bin().args(["make", "qc", "-C"]).arg(&ont).output().unwrap();
    assert!(
        qc.status.success(),
        "`om make qc` failed with no Makefile:\n{}",
        String::from_utf8_lossy(&qc.stderr)
    );

    // A bare build runs the DEFAULT GOAL, which is the release *and* the QC.
    let bare = bin().args(["make", "-C"]).arg(&ont).output().unwrap();
    assert!(
        bare.status.success(),
        "a bare build failed with no Makefile:\n{}",
        String::from_utf8_lossy(&bare.stderr)
    );
    assert!(
        ont.join("tiny.owl").is_file() || root.join("tiny.owl").is_file(),
        "the bare build produced no release artefact"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&stash);
}

/// A plan is a pure function of the repo's committed files. It must NOT depend
/// on the invocation: a caller can pass `IMP=false PAT=false MIR=false` for one
/// run, and those variables gate whole rule sets — so seeding them into the
/// parse would decide which rules exist, and the plan, being written to
/// `owlmake.yaml`, would carry one run's switches as committed repository
/// configuration.
#[test]
fn command_line_switches_do_not_change_the_generated_plan() {
    let root = scratch("modes");
    fixture(&root);
    let ont = root.join("src/ontology");

    assert!(bin().args(["make", "--plan-only", "-C"]).arg(&ont).output().unwrap().status.success());
    let plain = std::fs::read_to_string(root.join("owlmake.yaml")).unwrap();

    assert!(bin()
        .args(["make", "--plan-only", "IMP=false", "PAT=false", "MIR=false", "-C"])
        .arg(&ont)
        .output()
        .unwrap()
        .status
        .success());
    let switched = std::fs::read_to_string(root.join("owlmake.yaml")).unwrap();

    assert_eq!(
        plain, switched,
        "`IMP=false PAT=false MIR=false` changed the generated plan. Those flags gate whole rule \
         sets, so seeding them into the parse makes the plan a function of the invocation — and \
         the plan is then written to disk."
    );

    // Nor does a switch of the repo's OWN invention. `BRI` gates a rule block
    // here exactly as `IMP` gates one in a generated build configuration, and it
    // is the plan that says which value it was resolved under.
    assert!(bin()
        .args(["make", "--plan-only", "BRI=true", "-C"])
        .arg(&ont)
        .output()
        .unwrap()
        .status
        .success());
    let repo_switch = std::fs::read_to_string(root.join("owlmake.yaml")).unwrap();
    assert_eq!(
        plain, repo_switch,
        "a repo's own conditional variable reached the parse and changed the generated plan"
    );
    assert!(
        plain.contains("gating_flags") && plain.contains("BRI"),
        "the plan must declare the switches it CANNOT vary, or a repo with no build \
         configuration cannot tell `BRI=false` from a typo:\n{plain}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The acceptance test applied to run inputs.**
///
/// The release date is a per-run choice, so the same plan has to release under
/// any date — with the build configuration gone, and without being regenerated,
/// because there would be nothing left to regenerate it from.
#[test]
fn a_plan_only_repo_takes_its_release_date_from_the_run() {
    let root = scratch("dated");
    fixture(&root);
    let ont = root.join("src/ontology");

    assert!(bin().args(["make", "--plan-only", "-C"]).arg(&ont).output().unwrap().status.success());
    let plan_text = std::fs::read_to_string(root.join("owlmake.yaml")).unwrap();
    assert!(
        plan_text.contains("version: '{today}'"),
        "a repo whose version is the build date must record that, not one day's date:\n{plan_text}"
    );
    assert!(
        plan_text.contains("releases/{version}/tiny.owl"),
        "the version must appear as a reference to the plan's one field, never expanded \
         into the strings built from it:\n{plan_text}"
    );

    let stash = scratch("dated_stash");
    std::fs::create_dir_all(&stash).unwrap();
    std::fs::rename(ont.join("Makefile"), stash.join("Makefile")).unwrap();
    std::fs::rename(ont.join("tiny.Makefile"), stash.join("tiny.Makefile")).unwrap();

    let out = bin()
        .args(["make", "tiny.owl", "TODAY=2001-02-03", "-C"])
        .arg(&ont)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "`TODAY=2001-02-03` failed with no build configuration:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let built = std::fs::read_to_string(ont.join("tiny.owl"))
        .or_else(|_| std::fs::read_to_string(root.join("tiny.owl")))
        .expect("the build produced no tiny.owl");
    assert!(
        built.contains("releases/2001-02-03/tiny.owl"),
        "the run's release date did not reach the artefact:\n{built}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("owlmake.yaml")).unwrap(),
        plan_text,
        "asking for a different release date rewrote the plan"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&stash);
}

/// **The acceptance test applied to a repository's own switch.**
///
/// `BRI` guards the rules that REGENERATE the bridges. Under `BRI=false` those
/// rules do not exist and the committed bridge files are used as they stand —
/// the same question `mirrors`, `imports` and `patterns` answer, so the plan
/// declares it as the same kind of group and a run pins it without the plan
/// being regenerated.
///
/// The bridge is a prerequisite of the main line whether or not it is refreshed,
/// so keeping the group has to leave the file alone rather than drop it from the
/// graph — the build still has to produce `tiny.owl` from it.
#[test]
fn a_plan_only_repo_can_keep_a_switched_group() {
    let root = scratch("bridges");
    fixture(&root);
    let ont = root.join("src/ontology");

    assert!(bin().args(["make", "--plan-only", "-C"]).arg(&ont).output().unwrap().status.success());
    let plan_text = std::fs::read_to_string(root.join("owlmake.yaml")).unwrap();
    assert!(
        plan_text.contains("name: bridges") && plan_text.contains("flag: BRI"),
        "a switch of the repo's own invention must be declared as a group, or a repo with \
         no build configuration cannot be told to keep it:\n{plan_text}"
    );

    let stash = scratch("bridges_stash");
    std::fs::create_dir_all(&stash).unwrap();
    std::fs::rename(ont.join("Makefile"), stash.join("Makefile")).unwrap();
    std::fs::rename(ont.join("tiny.Makefile"), stash.join("tiny.Makefile")).unwrap();

    let bridge = ont.join("bridge/tiny-bridge.owl");
    let committed = std::fs::read(&bridge).unwrap();

    // Make the bridge genuinely OUT OF DATE before anything is asked to keep it.
    // Left up to date, every assertion below passes on a build that simply had
    // nothing to do, and the pin would never be what was tested.
    let edit = ont.join("tiny-edit.ofn");
    let edit_text = std::fs::read(&edit).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&edit, &edit_text).unwrap();

    let out = bin().args(["make", "tiny.owl", "BRI=false", "-C"]).arg(&ont).output().unwrap();
    assert!(
        out.status.success(),
        "`BRI=false` failed with no build configuration:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&bridge).unwrap(),
        committed,
        "`BRI=false` regenerated the bridge it was told to keep"
    );
    assert!(
        ont.join("tiny.owl").is_file() || root.join("tiny.owl").is_file(),
        "keeping the bridges dropped the main line: `tiny.owl` depends on the bridge \
         whether or not it is refreshed"
    );

    // …and `--keep bridges` is the same request under the plan's own spelling.
    std::fs::write(&bridge, &committed).unwrap();
    let by_name =
        bin().args(["make", "tiny.owl", "--keep", "bridges", "-C"]).arg(&ont).output().unwrap();
    assert!(
        by_name.status.success(),
        "`--keep bridges` failed:\n{}",
        String::from_utf8_lossy(&by_name.stderr)
    );
    assert_eq!(
        std::fs::read(&bridge).unwrap(),
        committed,
        "`--keep bridges` regenerated the bridge"
    );

    // The other direction: the configuration sets `BRI = true`, so the group's
    // recorded default is to refresh, and a run that says nothing regenerates.
    // Without this the test would pass on a build that never refreshes anything.
    //
    // The bridge is still the stale committed one; `tiny.owl` is not, having been
    // built twice above, so the edit file is touched again to give this run the
    // same work the two pinned runs declined to do.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&edit, &edit_text).unwrap();
    let refreshed = bin().args(["make", "tiny.owl", "-C"]).arg(&ont).output().unwrap();
    assert!(
        refreshed.status.success(),
        "a default build failed:\n{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    assert_ne!(
        std::fs::read(&bridge).unwrap(),
        committed,
        "the plan records `bridges: rebuild`, so a run that says nothing must refresh them"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&stash);
}

/// **Both branches of a flag-guarded conditional are executable plan content.**
///
/// A conditional is resolved once, so the plan holds the rules of the branch
/// taken. Where the branch NOT taken defines its own recipe for the same target,
/// recording only the taken one leaves a run input deciding which recipe exists —
/// the original fault, in a smaller place. `stamp-bridges` is built by a
/// conversion under `BRI=true` and by `touch` under `BRI=false`, and the plan has
/// to carry both.
#[test]
fn a_plan_only_repo_runs_the_other_branch_of_a_conditional() {
    let root = scratch("branches");
    fixture(&root);
    let ont = root.join("src/ontology");

    assert!(bin().args(["make", "--plan-only", "-C"]).arg(&ont).output().unwrap().status.success());
    let plan_text = std::fs::read_to_string(root.join("owlmake.yaml")).unwrap();
    assert!(
        plan_text.contains("branches:") && plan_text.contains("flag: BRI"),
        "the recipe the other branch defines must be recorded, or flipping the switch \
         leaves the target with no rule at all:\n{plan_text}"
    );

    let stash = scratch("branches_stash");
    std::fs::create_dir_all(&stash).unwrap();
    std::fs::rename(ont.join("Makefile"), stash.join("Makefile")).unwrap();
    std::fs::rename(ont.join("tiny.Makefile"), stash.join("tiny.Makefile")).unwrap();

    // Delete what the FALSE branch would create, so only that branch's recipe
    // running can put it back.
    let stamp = ont.join("stamp-bridges");
    let _ = std::fs::remove_file(&stamp);
    assert!(!stamp.exists());

    let off = bin().args(["make", "tiny.owl", "BRI=false", "-C"]).arg(&ont).output().unwrap();
    assert!(
        off.status.success(),
        "`BRI=false` failed with no build configuration:\n{}",
        String::from_utf8_lossy(&off.stderr)
    );
    assert!(
        stamp.exists(),
        "the `BRI=false` branch defines `touch $@` for this target; owlmake pinned it \
         instead of running it, so a build that ODK completes cannot start"
    );
    assert!(
        std::fs::read(&stamp).unwrap().is_empty(),
        "`touch` is the FALSE branch's recipe; a non-empty file means the true branch ran"
    );

    // …and the branch that WAS taken still works, from the same plan. `tiny.owl`
    // goes too: a prerequisite is built when something that needs it is missing,
    // and leaving the product in place would leave this run with nothing to do.
    std::fs::remove_file(&stamp).unwrap();
    let _ = std::fs::remove_file(ont.join("tiny.owl"));
    let _ = std::fs::remove_file(root.join("tiny.owl"));
    let on = bin().args(["make", "tiny.owl", "BRI=true", "-C"]).arg(&ont).output().unwrap();
    assert!(
        on.status.success(),
        "`BRI=true` failed:\n{}",
        String::from_utf8_lossy(&on.stderr)
    );
    let rebuilt = std::fs::read_to_string(&stamp).expect("the BRI=true recipe produced nothing");
    assert_eq!(
        rebuilt.trim(),
        "regenerated",
        "under `BRI=true` this target has its own recipe, not the other branch's `touch`"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&stash);
}

/// A switch the plan cannot honour is refused, not ignored.
///
/// `DEBUG` selects between two values of a variable, so it guards no target and
/// there is nothing for a run to pin — the plan holds the rules of one branch and
/// can offer no other. Accepting `DEBUG=true` and building the recorded branch
/// anyway would be the build quietly doing something other than what it was
/// asked, and after the build configuration is gone the plan is the only thing
/// that knows `DEBUG` was ever a switch at all.
#[test]
fn a_switch_the_plan_cannot_vary_is_refused() {
    let root = scratch("switches");
    fixture(&root);
    let ont = root.join("src/ontology");

    assert!(bin().args(["make", "--plan-only", "-C"]).arg(&ont).output().unwrap().status.success());
    let stash = scratch("switches_stash");
    std::fs::create_dir_all(&stash).unwrap();
    std::fs::rename(ont.join("Makefile"), stash.join("Makefile")).unwrap();
    std::fs::rename(ont.join("tiny.Makefile"), stash.join("tiny.Makefile")).unwrap();

    let refused = bin().args(["make", "tiny.owl", "DEBUG=true", "-C"]).arg(&ont).output().unwrap();
    assert!(
        !refused.status.success(),
        "`DEBUG=true` was accepted, and the build ran the branch the plan happens to hold"
    );
    let said = String::from_utf8_lossy(&refused.stderr).to_string()
        + &String::from_utf8_lossy(&refused.stdout);
    assert!(said.contains("DEBUG"), "the refusal must name the switch:\n{said}");

    // The value the plan WAS resolved under is not a change, so it still builds.
    let agreed = bin().args(["make", "tiny.owl", "DEBUG=false", "-C"]).arg(&ont).output().unwrap();
    assert!(
        agreed.status.success(),
        "`DEBUG=false` is the value the plan describes and must be a no-op:\n{}",
        String::from_utf8_lossy(&agreed.stderr)
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&stash);
}

/// A declared input that is GONE is an error, and it is an error on the plan-only
/// side too.
///
/// This is the failure the plan is meant to make impossible, in the one
/// configuration where nothing else can catch it. `tiny.owl` is built from
/// `tiny-edit.ofn`; delete the source and leave the old product standing, and
/// every mtime comparison has nothing to compare against — an absent file is not
/// newer than anything — so the build reads a rule that cannot run as a rule that
/// need not run, reports success, and ships a stale artefact.
///
/// The missing-input check that catches this is derived from the repo at ingest.
/// Deriving it only there is what leaves this gap: with the Makefile deleted the
/// plan is all there is, so the check has to be re-derived when the plan is
/// LOADED — over the same target set, including the paths owlmake builds from its
/// own engines rather than from a recorded rule (`native_targets`), or a mirror
/// would read as an input nothing can produce.
#[test]
fn a_deleted_source_fails_the_build_that_has_only_the_plan() {
    let root = scratch("missing-input");
    fixture(&root);
    let ont = root.join("src/ontology");

    let plan_out = bin().args(["make", "--plan-only", "-C"]).arg(&ont).output().unwrap();
    assert!(
        plan_out.status.success(),
        "planning failed:\n{}",
        String::from_utf8_lossy(&plan_out.stderr)
    );

    // Build once with everything present, so the product exists and is NEWER
    // than anything left in the tree.
    let first = bin().args(["make", "tiny.owl", "-C"]).arg(&ont).output().unwrap();
    assert!(
        first.status.success(),
        "the initial build failed:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );

    // Now the repo is plan-only, and the edit file the plan names is gone.
    let stash = scratch("missing-input-stash");
    std::fs::create_dir_all(&stash).unwrap();
    std::fs::rename(ont.join("Makefile"), stash.join("Makefile")).unwrap();
    std::fs::rename(ont.join("tiny.Makefile"), stash.join("tiny.Makefile")).unwrap();
    std::fs::remove_file(ont.join("tiny-edit.ofn")).unwrap();

    let out = bin().args(["make", "tiny.owl", "-C"]).arg(&ont).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "a build whose declared source is missing reported success:\n{err}"
    );
    assert!(
        err.contains("tiny-edit.ofn"),
        "the error must NAME the missing input:\n{err}"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&stash);
}
