//! Classification of real OBO ontologies by the hermit-rs DL reasoner, pinned
//! axiom-for-axiom.
//!
//! Each case loads a fixture ontology and asserts that owlmake's direct class
//! subsumptions are *exactly* the set its `.golden` file records. The goldens
//! are fixed, hand-checked expectations of the correct hierarchy, never a
//! snapshot of what this code last produced: a failing case means the
//! classification changed and has to be justified, so regenerating a golden
//! from owlmake's own output would leave the suite green while destroying the
//! only thing it checks. A release publishes its inferred hierarchy, and any
//! change in which direct edges the reasoner produces rewrites every downstream
//! artefact, so the goldens pin the whole hierarchy rather than merely checking
//! that classification terminates and stays consistent.
//!
//! Trivial axioms carry no hierarchy information and stay out of the
//! comparison: [`owlmake_inferred`] drops reflexive `X ⊑ X`, the top tautology
//! `X ⊑ owl:Thing` and `owl:Nothing ⊑ X` from owlmake's side, and no golden
//! records such a line, so both sides hold only substantive inferred edges.
//!
//! See `tests/hermit_faithfulness/` for the fixtures. The two here are the
//! smallest and fastest ontologies that still exercise transitive properties,
//! inverses and role chains, so the suite stays quick and runs offline from
//! checked-in files alone.

use std::path::Path;

use owlmake::reason::DlReasoner;

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// owlmake's substantive inferred direct subsumptions for `fixture`, as sorted
/// `"<sub> <sup>"` lines (trivial top/bottom/reflexive axioms removed).
fn owlmake_inferred(fixture: &str) -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/hermit_faithfulness");
    let model = owlmake::io::load(&dir.join(fixture))
        .unwrap_or_else(|e| panic!("load {fixture}: {e}"));
    let reasoner = DlReasoner::classify(&model);
    assert!(reasoner.is_consistent(), "{fixture} should be consistent");
    let mut pairs: Vec<String> = reasoner
        .direct_subsumptions()
        .into_iter()
        .filter(|(sub, sup)| sub != sup && sup != THING && sub != NOTHING)
        .map(|(sub, sup)| format!("{sub} {sup}"))
        .collect();
    pairs.sort();
    pairs.dedup();
    pairs
}

/// The expected direct subsumptions for `fixture`: its `.golden` file holds one
/// `"<sub> <sup>"` line per edge, sorted and deduplicated here to match the
/// shape [`owlmake_inferred`] returns.
fn hermit_golden(fixture: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/hermit_faithfulness")
        .join(format!("{fixture}.golden"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read golden {fixture}: {e}"));
    let mut lines: Vec<String> = text.lines().map(str::to_owned).filter(|l| !l.is_empty()).collect();
    lines.sort();
    lines.dedup();
    lines
}

/// Assert owlmake's classification equals the golden set, reporting the
/// symmetric difference on failure.
fn assert_faithful(fixture: &str) {
    let got = owlmake_inferred(fixture);
    let want = hermit_golden(fixture);
    if got != want {
        let only_hermit: Vec<&String> = want.iter().filter(|x| !got.contains(*x)).collect();
        let only_owlmake: Vec<&String> = got.iter().filter(|x| !want.contains(*x)).collect();
        panic!(
            "{fixture}: classification diverged from HermiT\n  HermiT={} owlmake={}\n  only in HermiT ({}): {:?}\n  only in owlmake ({}): {:?}",
            want.len(),
            got.len(),
            only_hermit.len(),
            &only_hermit[..only_hermit.len().min(20)],
            only_owlmake.len(),
            &only_owlmake[..only_owlmake.len().min(20)],
        );
    }
}

#[test]
fn bfo_matches_hermit() {
    // BFO: foundational ontology with transitive/inverse object properties —
    // exercises the role-automaton construction path.
    assert_faithful("bfo.owl");
}

#[test]
fn bspo_base_matches_hermit() {
    // BSPO base: transitive properties and role chains over ~90 classes, where
    // the role automata have to compose chains correctly.
    assert_faithful("bspo-base.owl");
}
