//! Tests for module extraction and term-selection commands on small,
//! hand-built ontologies with known expected results.

use std::collections::HashSet;

use horned_owl::model::{Build, ClassExpression as CE, Component, MutableOntology, SubClassOf};
use horned_owl::ontology::set::SetOntology;

use owlmake::extract::{self, Method};
use owlmake::model::Model;

const NS: &str = "http://example.org/";

fn sub(b: &Build<horned_owl::model::RcStr>, x: &str, y: &str) -> Component<horned_owl::model::RcStr> {
    Component::SubClassOf(SubClassOf {
        sub: CE::Class(b.class(format!("{NS}{x}"))),
        sup: CE::Class(b.class(format!("{NS}{y}"))),
    })
}

fn model(components: Vec<Component<horned_owl::model::RcStr>>) -> Model {
    let mut ont = SetOntology::new();
    for c in components {
        ont.insert(c);
    }
    Model::from_parts(ont, owlmake::model::default_prefixes())
}

fn has_sub(m: &Model, x: &str, y: &str) -> bool {
    m.ont.iter().any(|ac| match &ac.component {
        Component::SubClassOf(sc) => match (&sc.sub, &sc.sup) {
            (CE::Class(a), CE::Class(b)) => {
                a.0.as_ref() == format!("{NS}{x}") && b.0.as_ref() == format!("{NS}{y}")
            }
            _ => false,
        },
        _ => false,
    })
}

#[test]
fn bot_module_keeps_dependencies_drops_unrelated() {
    // A ⊑ B ⊑ C, and an unrelated X ⊑ Y. The ⊥-module for {A} keeps the
    // dependency chain and drops X ⊑ Y.
    let b = Build::new_rc();
    let m = model(vec![sub(&b, "A", "B"), sub(&b, "B", "C"), sub(&b, "X", "Y")]);
    let seed: HashSet<String> = [format!("{NS}A")].into_iter().collect();

    let module = extract::extract(&m, &seed, Method::Bot);
    assert!(has_sub(&module, "A", "B"), "module must keep A ⊑ B");
    assert!(has_sub(&module, "B", "C"), "module must keep B ⊑ C");
    assert!(!has_sub(&module, "X", "Y"), "module must drop unrelated X ⊑ Y");
}

#[test]
fn star_module_is_subset_of_bot_and_drops_unrelated() {
    // STAR (⊥⊤*) is never larger than BOT and still excludes axioms unrelated
    // to the seed signature.
    let b = Build::new_rc();
    let comps = vec![
        sub(&b, "A", "B"),
        sub(&b, "B", "C"),
        sub(&b, "C", "D"),
        sub(&b, "P", "Q"),
    ];
    let m = model(comps);
    let count = |mm: &Model| {
        mm.ont
            .iter()
            .filter(|ac| matches!(ac.component, Component::SubClassOf(_)))
            .count()
    };

    // Singleton seed: STAR is a (possibly strict) subset of BOT, unrelated dropped.
    let seed_a: HashSet<String> = [format!("{NS}A")].into_iter().collect();
    let bot = extract::extract(&m, &seed_a, Method::Bot);
    let star = extract::extract(&m, &seed_a, Method::Star);
    assert!(count(&star) <= count(&bot));
    assert!(has_sub(&bot, "A", "B"), "BOT keeps superclass dependencies");
    assert!(!has_sub(&star, "P", "Q"));
    assert!(!has_sub(&bot, "P", "Q"));

    // Widening the seed to both chain endpoints still drops the unrelated branch.
    let seed_ac: HashSet<String> = [format!("{NS}A"), format!("{NS}C")].into_iter().collect();
    let star_ac = extract::extract(&m, &seed_ac, Method::Star);
    assert!(!has_sub(&star_ac, "P", "Q"));
}

#[test]
fn import_module_pulls_referenced_terms_from_source() {
    // Source: a small "ontology" with a hierarchy. Edit references only B.
    let b = Build::new_rc();
    let source = model(vec![
        sub(&b, "A", "B"),
        sub(&b, "B", "C"),
        sub(&b, "P", "Q"),
    ]);
    // Seed = the terms an edit ontology references (just B here).
    let seed: HashSet<String> = [format!("{NS}B")].into_iter().collect();
    let module = extract::extract(&source, &seed, Method::Bot);
    // The ⊥-module for {B} keeps B's superclass dependency and drops the
    // unrelated P/Q branch.
    assert!(has_sub(&module, "B", "C"), "module keeps B ⊑ C");
    assert!(!has_sub(&module, "P", "Q"), "unrelated P ⊑ Q dropped");
}

#[test]
fn mireot_keeps_hierarchy_up_to_upper_term() {
    // A ⊑ B ⊑ C ⊑ D. MIREOT lower {A}, upper {C} keeps A⊑B, B⊑C, stops at C.
    let b = Build::new_rc();
    let m = model(vec![
        sub(&b, "A", "B"),
        sub(&b, "B", "C"),
        sub(&b, "C", "D"),
    ]);
    let lower: HashSet<String> = [format!("{NS}A")].into_iter().collect();
    let upper: HashSet<String> = [format!("{NS}C")].into_iter().collect();

    let module = extract::mireot(&m, &lower, &upper);
    assert!(has_sub(&module, "A", "B"));
    assert!(has_sub(&module, "B", "C"));
    assert!(!has_sub(&module, "C", "D"), "MIREOT must stop at the upper term");
}
