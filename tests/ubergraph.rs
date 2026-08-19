//! End-to-end tests for the Ubergraph pipeline: classify a small ontology,
//! materialize the redundant relation graph, prune it to the non-redundant
//! graph, and check both against hand-computed expectations. The invariants
//! asserted here — full closure, non-redundant ⊆ redundant, no reflexive edges
//! after pruning — are the ones that must hold at full OBO release scale.

use std::collections::HashSet;

use horned_owl::model::{
    Build, ClassExpression as CE, Component, MutableOntology,
    ObjectPropertyExpression as OPE, SubClassOf, SubObjectPropertyExpression as SOPE,
    SubObjectPropertyOf, TransitiveObjectProperty,
};
use horned_owl::ontology::set::SetOntology;

use owlmake::model::Model;
use owlmake::reason::Reasoner;
use owlmake::ubergraph::{edges, ic, iri, prune};

const OBO: &str = "http://purl.obolibrary.org/obo/";

fn model(components: Vec<Component<horned_owl::model::RcStr>>) -> Model {
    let mut ont: SetOntology<_> = SetOntology::new();
    for c in components {
        ont.insert(c);
    }
    Model::from_parts(ont, owlmake::model::default_prefixes())
}

/// X_3 ⊑ X_2 ⊑ X_1, Y_2 ⊑ Y_1, X_3 ⊑ part_of some Y_2, part_of transitive,
/// in_deep_part_of ⊑ part_of.
fn fixture() -> (Model, HashSet<String>) {
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{OBO}{n}")));
    let op = |n: &str| OPE::ObjectProperty(b.object_property(format!("{OBO}{n}")));
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(SubClassOf { sub: x, sup: y });

    let part_of = format!("{OBO}part_of");
    let comps = vec![
        sub(c("X_2"), c("X_1")),
        sub(c("X_3"), c("X_2")),
        sub(c("Y_2"), c("Y_1")),
        Component::SubClassOf(SubClassOf {
            sub: c("X_3"),
            sup: CE::ObjectSomeValuesFrom {
                ope: op("part_of"),
                bce: Box::new(c("Y_2")),
            },
        }),
        Component::TransitiveObjectProperty(TransitiveObjectProperty(op("part_of"))),
        Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sup: op("part_of"),
            sub: SOPE::ObjectPropertyExpression(op("in_deep_part_of")),
        }),
    ];
    let mut transitive = HashSet::new();
    transitive.insert(part_of);
    (model(comps), transitive)
}

#[test]
fn redundant_graph_is_the_full_closure() {
    let (m, _) = fixture();
    let r = Reasoner::classify(&m);
    let g = edges::redundant_graph(&r, &HashSet::new());

    let sc = iri::RDFS_SUBCLASS_OF.to_string();
    let part_of = format!("{OBO}part_of");
    let t = |s: &str, p: &str, o: &str| (format!("{OBO}{s}"), p.to_string(), format!("{OBO}{o}"));

    // Both the direct and the inherited (Y_2 ⊑ Y_1) existential edges.
    assert!(g.contains(&t("X_3", &part_of, "Y_2")));
    assert!(g.contains(&t("X_3", &part_of, "Y_1")));
    // Full subclass closure including the transitive X_3 ⊑ X_1.
    assert!(g.contains(&t("X_3", &sc, "X_1")));
    assert!(g.contains(&t("X_3", &sc, "X_2")));
    assert!(g.contains(&t("X_2", &sc, "X_1")));
    // Reflexive self-edges for every class.
    assert!(g.contains(&t("X_1", &sc, "X_1")));
    assert!(g.contains(&t("Y_2", &sc, "Y_2")));
    // 11 edges total (2 property + 4 strict subclass + 5 reflexive).
    assert_eq!(g.len(), 11, "graph = {g:#?}");
}

#[test]
fn nonredundant_prunes_inherited_transitive_and_reflexive() {
    let (m, transitive) = fixture();
    let r = Reasoner::classify(&m);
    let redundant = edges::redundant_graph(&r, &HashSet::new());
    let subprop = vec![(format!("{OBO}in_deep_part_of"), format!("{OBO}part_of"))];
    let nr = prune::nonredundant(&redundant, &transitive, &subprop);

    let sc = iri::RDFS_SUBCLASS_OF.to_string();
    let part_of = format!("{OBO}part_of");
    let t = |s: &str, p: &str, o: &str| (format!("{OBO}{s}"), p.to_string(), format!("{OBO}{o}"));

    // Kept: most-specific property edge + direct subsumptions only.
    assert!(nr.contains(&t("X_3", &part_of, "Y_2")));
    assert!(nr.contains(&t("X_3", &sc, "X_2")));
    assert!(nr.contains(&t("X_2", &sc, "X_1")));
    assert!(nr.contains(&t("Y_2", &sc, "Y_1")));
    // Pruned: inherited edge, transitive subsumption, reflexives.
    assert!(!nr.contains(&t("X_3", &part_of, "Y_1")));
    assert!(!nr.contains(&t("X_3", &sc, "X_1")));
    assert!(nr.iter().all(|(s, _, o)| s != o), "no reflexive edges survive");
    assert_eq!(nr.len(), 4, "graph = {nr:#?}");

    // Non-redundant is a subset of redundant (the key structural invariant).
    let red: HashSet<_> = redundant.iter().collect();
    assert!(nr.iter().all(|e| red.contains(e)));
}

#[test]
fn information_content_is_well_defined_and_ordered() {
    let (m, _) = fixture();
    let r = Reasoner::classify(&m);
    let redundant = edges::redundant_graph(&r, &HashSet::new());
    let lines = ic::information_content(&redundant);

    // Every class gets an IC and a reference count.
    let x1_ic = lines
        .iter()
        .find(|l| l.contains("X_1>") && l.contains(iri::NORMALIZED_IC))
        .expect("X_1 has IC");
    let x3_ic = lines
        .iter()
        .find(|l| l.contains("X_3>") && l.contains(iri::NORMALIZED_IC))
        .expect("X_3 has IC");
    // The leaf X_3 (referenced only by itself) is maximally informative (100);
    // the root X_1 (referenced by all of X_1/X_2/X_3) is less informative.
    assert!(x3_ic.contains("\"100\""), "X_3 IC = {x3_ic}");
    assert!(!x1_ic.contains("\"100\""), "X_1 IC = {x1_ic}");
}
