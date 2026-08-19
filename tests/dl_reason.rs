//! Tests for the SROIQ(D) DL reasoner on entailments the EL reasoner cannot
//! derive: disjunction, universal restrictions and full negation, plus
//! nominals, cardinality restrictions, role chains and the SROIQ role box
//! (reflexive roles, `∃r.Self`, disjoint roles).

use horned_owl::model::{
    Build, ClassExpression as CE, Component, MutableOntology, ObjectPropertyExpression as OPE,
    SubClassOf,
};
use horned_owl::ontology::set::SetOntology;

use owlmake::model::Model;
use owlmake::reason::DlReasoner;

const NS: &str = "http://example.org/";

fn model(components: Vec<Component<horned_owl::model::RcStr>>) -> Model {
    let mut ont = SetOntology::new();
    for c in components {
        ont.insert(c);
    }
    Model::from_parts(ont, owlmake::model::default_prefixes())
}

fn sub(x: CE<horned_owl::model::RcStr>, y: CE<horned_owl::model::RcStr>) -> Component<horned_owl::model::RcStr> {
    Component::SubClassOf(SubClassOf { sub: x, sup: y })
}

#[test]
fn reasoning_by_cases_over_disjunction() {
    // A ⊑ B ⊔ C, B ⊑ D, C ⊑ D  ⟹  A ⊑ D. This needs case analysis over the
    // disjunction — beyond EL, squarely in ALC.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let m = model(vec![
        sub(c("A"), CE::ObjectUnionOf(vec![c("B"), c("C")])),
        sub(c("B"), c("D")),
        sub(c("C"), c("D")),
    ]);
    let r = DlReasoner::classify(&m);
    assert!(r.is_consistent());
    assert!(
        r.is_subsumed(&format!("{NS}A"), &format!("{NS}D")),
        "A ⊑ D must follow by reasoning over the disjunction"
    );
}

#[test]
fn universal_restriction_entailment() {
    // A ⊑ ∀r.B, A ⊑ ∃r.⊤, B ⊑ C  ⟹  A ⊑ ∃r.C  (the r-successor of A is a B,
    // hence a C). Universal restrictions are non-EL.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r_prop = b.object_property(format!("{NS}r"));
    let all = |f: CE<_>| CE::ObjectAllValuesFrom {
        ope: OPE::ObjectProperty(r_prop.clone()),
        bce: Box::new(f),
    };
    let some = |f: CE<_>| CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(r_prop.clone()),
        bce: Box::new(f),
    };
    let m = model(vec![
        sub(c("A"), all(c("B"))),
        sub(c("A"), some(CE::Class(b.class("http://www.w3.org/2002/07/owl#Thing")))),
        sub(c("B"), c("C")),
    ]);
    // A ⊑ ∃r.C
    let target = CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(r_prop.clone()),
        bce: Box::new(c("C")),
    };
    // The query API answers subsumption between *named* classes, so the test runs
    // through a helper class that stands for the anonymous ∃r.C.
    let mut m2 = m;
    // Declare X ≡ ∃r.C by X ⊑ ∃r.C and ∃r.C ⊑ X (so A ⊑ X iff A ⊑ ∃r.C).
    let x = c("X");
    m2.ont.insert(sub(x.clone(), target.clone()));
    m2.ont.insert(sub(target, x.clone()));
    let r2 = DlReasoner::classify(&m2);
    assert!(
        r2.is_subsumed(&format!("{NS}A"), &format!("{NS}X")),
        "A ⊑ ∃r.C must follow from ∀r.B, ∃r.⊤, B ⊑ C"
    );
}

#[test]
fn role_hierarchy_universal() {
    // r ⊑ s, A ⊑ ∃r.B, A ⊑ ∀s.C, B ⊑ ¬C  ⟹  A unsatisfiable.
    // (A's r-successor is also an s-successor, so it must be C and ¬C.)
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let s = b.object_property(format!("{NS}s"));
    let m = model(vec![
        Component::SubObjectPropertyOf(horned_owl::model::SubObjectPropertyOf {
            sub: horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(
                OPE::ObjectProperty(r.clone()),
            ),
            sup: OPE::ObjectProperty(s.clone()),
        }),
        sub(c("A"), CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(r.clone()), bce: Box::new(c("B")) }),
        sub(c("A"), CE::ObjectAllValuesFrom { ope: OPE::ObjectProperty(s.clone()), bce: Box::new(c("C")) }),
        sub(c("B"), CE::ObjectComplementOf(Box::new(c("C")))),
    ]);
    let r2 = DlReasoner::classify(&m);
    assert!(
        r2.unsatisfiable().contains(&format!("{NS}A")),
        "A must be unsatisfiable via the role hierarchy r ⊑ s"
    );
}

#[test]
fn transitive_role_universal_propagation() {
    // r transitive, A ⊑ ∀r.¬D, A ⊑ ∃r.B, B ⊑ ∃r.D  ⟹  A unsatisfiable.
    // The r-r-successor is a D, but transitivity makes it an r-successor of A,
    // which must be ¬D.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let some = |f: CE<_>| CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(r.clone()), bce: Box::new(f) };
    let m = model(vec![
        Component::TransitiveObjectProperty(horned_owl::model::TransitiveObjectProperty(
            OPE::ObjectProperty(r.clone()),
        )),
        sub(c("A"), CE::ObjectAllValuesFrom { ope: OPE::ObjectProperty(r.clone()), bce: Box::new(CE::ObjectComplementOf(Box::new(c("D")))) }),
        sub(c("A"), some(c("B"))),
        sub(c("B"), some(c("D"))),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        rr.unsatisfiable().contains(&format!("{NS}A")),
        "A must be unsatisfiable via transitive ∀-propagation"
    );
}

#[test]
fn min_max_cardinality_conflict() {
    // A ⊑ ≥2 r.C, A ⊑ ≤1 r.C  ⟹  A unsatisfiable (need 2 distinct, allow 1).
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let m = model(vec![
        sub(c("A"), CE::ObjectMinCardinality { n: 2, ope: OPE::ObjectProperty(r.clone()), bce: Box::new(c("C")) }),
        sub(c("A"), CE::ObjectMaxCardinality { n: 1, ope: OPE::ObjectProperty(r.clone()), bce: Box::new(c("C")) }),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        rr.unsatisfiable().contains(&format!("{NS}A")),
        "≥2 r.C ⊓ ≤1 r.C must be unsatisfiable"
    );
}

#[test]
fn functional_merge_with_disjoint_fillers() {
    // A ⊑ ≤1 r.⊤, A ⊑ ∃r.B, A ⊑ ∃r.C, B and C disjoint  ⟹  A unsatisfiable:
    // the two r-successors must merge under ≤1, but B ⊓ C ⊑ ⊥.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let thing = CE::Class(b.class("http://www.w3.org/2002/07/owl#Thing"));
    let some = |f: CE<_>| CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(r.clone()), bce: Box::new(f) };
    let m = model(vec![
        sub(c("A"), CE::ObjectMaxCardinality { n: 1, ope: OPE::ObjectProperty(r.clone()), bce: Box::new(thing) }),
        sub(c("A"), some(c("B"))),
        sub(c("A"), some(c("C"))),
        Component::DisjointClasses(horned_owl::model::DisjointClasses(vec![c("B"), c("C")])),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        rr.unsatisfiable().contains(&format!("{NS}A")),
        "functional r with two disjoint fillers must be unsatisfiable"
    );
}

#[test]
fn functional_merge_consistent_is_satisfiable() {
    // Same but B, C not disjoint  ⟹  A satisfiable (successors merge into {B,C}).
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let thing = CE::Class(b.class("http://www.w3.org/2002/07/owl#Thing"));
    let some = |f: CE<_>| CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(r.clone()), bce: Box::new(f) };
    let m = model(vec![
        sub(c("A"), CE::ObjectMaxCardinality { n: 1, ope: OPE::ObjectProperty(r.clone()), bce: Box::new(thing) }),
        sub(c("A"), some(c("B"))),
        sub(c("A"), some(c("C"))),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        !rr.unsatisfiable().contains(&format!("{NS}A")),
        "functional r with compatible fillers must be satisfiable"
    );
    assert!(rr.is_consistent());
}

#[test]
fn inverse_role_backward_propagation() {
    // A ⊑ ∃r.B, B ⊑ ∀r⁻.C  ⟹  A ⊑ C.
    // (A's r-successor y is a B; B says everything r⁻-reachable is C; A is an
    // r⁻-successor of y since A--r-->y, so A must be C.)
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let m = model(vec![
        sub(c("A"), CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(r.clone()), bce: Box::new(c("B")) }),
        sub(c("B"), CE::ObjectAllValuesFrom { ope: OPE::InverseObjectProperty(r.clone()), bce: Box::new(c("C")) }),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(rr.is_consistent());
    assert!(
        rr.is_subsumed(&format!("{NS}A"), &format!("{NS}C")),
        "A ⊑ C must follow via the inverse role r⁻"
    );
}

#[test]
fn role_chain_universal_propagation() {
    // r1 ∘ r2 ⊑ s, A ⊑ ∃r1.X, X ⊑ ∃r2.B, A ⊑ ∀s.¬B  ⟹  A unsatisfiable.
    // (The r1-then-r2 path reaches a B; via the chain that endpoint is an
    // s-successor of A, which ∀s.¬B forces to be ¬B.)
    use horned_owl::model::{SubObjectPropertyExpression as S, SubObjectPropertyOf};
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r1 = b.object_property(format!("{NS}r1"));
    let r2 = b.object_property(format!("{NS}r2"));
    let s = b.object_property(format!("{NS}s"));
    let some = |p: &horned_owl::model::ObjectProperty<_>, f: CE<_>| CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(p.clone()),
        bce: Box::new(f),
    };
    let m = model(vec![
        Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: S::ObjectPropertyChain(vec![
                OPE::ObjectProperty(r1.clone()),
                OPE::ObjectProperty(r2.clone()),
            ]),
            sup: OPE::ObjectProperty(s.clone()),
        }),
        sub(c("A"), some(&r1, c("X"))),
        sub(c("X"), some(&r2, c("B"))),
        sub(c("A"), CE::ObjectAllValuesFrom { ope: OPE::ObjectProperty(s.clone()), bce: Box::new(CE::ObjectComplementOf(Box::new(c("B")))) }),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        rr.unsatisfiable().contains(&format!("{NS}A")),
        "A must be unsatisfiable via the role chain r1∘r2 ⊑ s"
    );
}

#[test]
fn nominal_singleton_merge() {
    // A ⊑ ∃r.{a}, A ⊑ ∀r.C, A ⊑ ∃s.{a}, A ⊑ ∀s.D, C disjoint D  ⟹  A unsat.
    // Both the r-successor and the s-successor are the *same* individual a, so
    // they must merge — giving a node that is both C and D, a clash. A plain
    // atomic treatment of {a} would (wrongly) keep two separate nodes and miss
    // this; the nominal o-rule catches it.
    use horned_owl::model::{Individual, NamedIndividual};
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let s = b.object_property(format!("{NS}s"));
    let a: Individual<_> = Individual::Named(NamedIndividual(b.iri(format!("{NS}a"))));
    let hasval = |p: &horned_owl::model::ObjectProperty<_>| CE::ObjectHasValue {
        ope: OPE::ObjectProperty(p.clone()),
        i: a.clone(),
    };
    let all = |p: &horned_owl::model::ObjectProperty<_>, f: CE<_>| CE::ObjectAllValuesFrom {
        ope: OPE::ObjectProperty(p.clone()),
        bce: Box::new(f),
    };
    let m = model(vec![
        sub(c("A"), hasval(&r)),
        sub(c("A"), all(&r, c("C"))),
        sub(c("A"), hasval(&s)),
        sub(c("A"), all(&s, c("D"))),
        Component::DisjointClasses(horned_owl::model::DisjointClasses(vec![c("C"), c("D")])),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        rr.unsatisfiable().contains(&format!("{NS}A")),
        "A must be unsatisfiable: the two {{a}} successors merge into C ⊓ D"
    );
}

#[test]
fn nominal_cardinality_inverse_abox_inconsistency() {
    // a r o, b r o, {o} ⊑ ≤1 r⁻.⊤, a ≠ b  ⟹  inconsistent.
    // o has two distinct r-predecessors a, b but allows at most one — the
    // cardinality/inverse/nominal/ABox interaction the SROIQ rules resolve.
    use horned_owl::model::{
        DifferentIndividuals, Individual, NamedIndividual, ObjectPropertyAssertion,
    };
    let b = Build::new_rc();
    let r = b.object_property(format!("{NS}r"));
    let ind = |n: &str| Individual::Named(NamedIndividual(b.iri(format!("{NS}{n}"))));
    let nom = |n: &str| CE::ObjectOneOf(vec![ind(n)]);
    let m = model(vec![
        Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
            ope: OPE::ObjectProperty(r.clone()),
            from: ind("a"),
            to: ind("o"),
        }),
        Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
            ope: OPE::ObjectProperty(r.clone()),
            from: ind("b"),
            to: ind("o"),
        }),
        // {o} ⊑ ≤1 r⁻.⊤
        sub(
            nom("o"),
            CE::ObjectMaxCardinality {
                n: 1,
                ope: OPE::InverseObjectProperty(r.clone()),
                bce: Box::new(CE::Class(b.class("http://www.w3.org/2002/07/owl#Thing"))),
            },
        ),
        Component::DifferentIndividuals(DifferentIndividuals(vec![ind("a"), ind("b")])),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        !rr.is_consistent(),
        "two distinct r-predecessors of o violate ≤1 r⁻.⊤"
    );
}

#[test]
fn negation_unsatisfiability() {
    // A ⊑ B, A ⊑ ¬B  ⟹  A unsatisfiable.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let m = model(vec![
        sub(c("A"), c("B")),
        sub(c("A"), CE::ObjectComplementOf(Box::new(c("B")))),
    ]);
    let r = DlReasoner::classify(&m);
    assert!(
        r.unsatisfiable().contains(&format!("{NS}A")),
        "A must be unsatisfiable"
    );
}

#[test]
fn cardinality_clash_backjumps_over_independent_disjunctions() {
    // EFO's disjunction pattern: a class `A` carries many *independent* binary
    // disjunctions (`A ⊑ Pi ⊔ Qi`) plus a cardinality contradiction
    // (`A ⊑ ≥2 r.⊤ ⊓ ≤1 r.⊤`) that has nothing to do with any disjunct. The
    // OR-rule fires first, so the tableau branches through all K disjunctions
    // before it generates the r-successors and sees the ≤1/≥2 clash. That
    // clash depends on *none* of the disjunction levels, so precise
    // dependency-directed backjumping must unwind straight past all K of them in
    // a single step. If the clash's dependency set is over-approximated, so that
    // it collapses to "depends on everything", backjumping is defeated and the
    // search degrades to chronological backtracking over 2^K disjunction
    // combinations — i.e. it hangs. With K = 64 that is ~1.8e19 leaves: this test
    // only terminates (quickly) if backjumping works.
    const K: usize = 64;
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let thing = || CE::Class(b.class("http://www.w3.org/2002/07/owl#Thing"));
    let mut comps = vec![
        sub(c("A"), CE::ObjectMinCardinality { n: 2, ope: OPE::ObjectProperty(r.clone()), bce: Box::new(thing()) }),
        sub(c("A"), CE::ObjectMaxCardinality { n: 1, ope: OPE::ObjectProperty(r.clone()), bce: Box::new(thing()) }),
    ];
    for i in 0..K {
        comps.push(sub(
            c("A"),
            CE::ObjectUnionOf(vec![c(&format!("P{i}")), c(&format!("Q{i}"))]),
        ));
    }
    let rr = DlReasoner::classify(&model(comps));
    assert!(
        rr.unsatisfiable().contains(&format!("{NS}A")),
        "A is unsatisfiable (≥2 r.⊤ ⊓ ≤1 r.⊤), independent of the {K} disjunctions"
    );
}

#[test]
fn many_disjunctions_satisfiable_stays_fast() {
    // The dual: a class with many independent disjunctions but no contradiction
    // is satisfiable, and must be found so without exploring the combinations —
    // the first disjunct of each clause already witnesses a model.
    const K: usize = 200;
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let mut comps = Vec::new();
    for i in 0..K {
        comps.push(sub(
            c("A"),
            CE::ObjectUnionOf(vec![c(&format!("P{i}")), c(&format!("Q{i}"))]),
        ));
    }
    let rr = DlReasoner::classify(&model(comps));
    assert!(
        !rr.unsatisfiable().contains(&format!("{NS}A")),
        "A with {K} independent disjunctions and no contradiction is satisfiable"
    );
}

#[test]
fn equivalence_clique_keeps_shared_subsumer() {
    // U ≡ V, both U ⊑ R and V ⊑ R, and c ⊑ V. The transitive reduction must not
    // treat the equivalent class V as a proper intermediate "U ⊑ V ⊑ R" and drop
    // the clique's real parent edge {U,V} ⊑ R. EFO depends on this: the
    // PO_0009006 ≡ EFO_0000992 clique, and everything hanging below it, would
    // otherwise lose every super above the clique.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    use horned_owl::model::EquivalentClasses;
    let m = model(vec![
        Component::EquivalentClasses(EquivalentClasses(vec![c("U"), c("V")])),
        sub(c("U"), c("R")),
        sub(c("V"), c("R")),
        sub(c("c"), c("V")),
    ]);
    let r = DlReasoner::classify(&m);
    let short = |s: &str| s.rsplit('/').next().unwrap().to_string();
    let dir: std::collections::BTreeSet<(String, String)> = r
        .direct_subsumptions()
        .into_iter()
        .map(|(a, b)| (short(&a), short(&b)))
        .collect();
    let has = |x: &str, y: &str| dir.contains(&(x.to_string(), y.to_string()));
    // The clique's shared parent edge survives the reduction: both members keep it.
    assert!(has("U", "R"), "U ⊑ R must survive reduction; got {dir:?}");
    assert!(has("V", "R"), "V ⊑ R must survive reduction; got {dir:?}");
    // The equivalence is preserved as mutual subsumption.
    assert!(has("U", "V") && has("V", "U"), "U ≡ V must be preserved; got {dir:?}");
    // c attaches to the clique, and c ⊑ R is correctly *indirect* (reduced away).
    assert!(has("c", "U") || has("c", "V"), "c ⊑ clique must survive; got {dir:?}");
    assert!(!has("c", "R"), "c ⊑ R is indirect and must be reduced away; got {dir:?}");
    // Sanity: the full closure has every super (the reasoner itself is complete).
    let all: std::collections::BTreeSet<(String, String)> = r
        .all_subsumptions()
        .into_iter()
        .map(|(a, b)| (short(&a), short(&b)))
        .collect();
    for (x, y) in [("U", "R"), ("V", "R"), ("c", "R"), ("c", "U")] {
        assert!(all.contains(&(x.to_string(), y.to_string())), "closure missing {x} ⊑ {y}");
    }
}

#[test]
fn object_property_domain_and_range() {
    // Domain(r)=D, Range(r)=E, X ⊑ ≥1 r.F — the EFO PR pattern: a class with a
    // *cardinality* successor plus a domain axiom. EL drops the cardinality, so
    // only the DL path can apply the domain.
    //   X ⊑ ≥1 r.F  with Domain(r,D)  ⟹ X ⊑ D
    //   Range(r,E) constrains the r-successor, not X itself.
    use horned_owl::model::{ObjectPropertyDomain, ObjectPropertyRange};
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let m = model(vec![
        Component::ObjectPropertyDomain(ObjectPropertyDomain {
            ope: OPE::ObjectProperty(r.clone()),
            ce: c("D"),
        }),
        Component::ObjectPropertyRange(ObjectPropertyRange {
            ope: OPE::ObjectProperty(r.clone()),
            ce: c("E"),
        }),
        // X ⊑ ≥1 r.F  (cardinality successor — EL would drop this)
        sub(
            c("X"),
            CE::ObjectMinCardinality { n: 1, ope: OPE::ObjectProperty(r.clone()), bce: Box::new(c("F")) },
        ),
    ]);
    let r2 = DlReasoner::classify(&m);
    assert!(r2.is_consistent());
    // Domain: a node with an r-successor is in D.
    assert!(r2.is_subsumed(&format!("{NS}X"), &format!("{NS}D")), "X ⊑ D via Domain(r,D)");
    // Range applies to the successor; sanity-check it does not wrongly subsume X by E.
    assert!(!r2.is_subsumed(&format!("{NS}X"), &format!("{NS}E")), "X must NOT be ⊑ E (range applies to the filler, not X)");
}

// ---- SROIQ role-box: reflexive / has-self / disjoint roles ----------------

fn has_self(r: &horned_owl::model::ObjectProperty<horned_owl::model::RcStr>) -> CE<horned_owl::model::RcStr> {
    CE::ObjectHasSelf(OPE::ObjectProperty(r.clone()))
}

#[test]
fn reflexive_role_universal_propagation() {
    // Reflexive(r), A ⊑ ∀r.B  ⟹  A ⊑ B.
    // Every node has an r-self-loop, so A's own ∀r.B fires B on itself.
    use horned_owl::model::ReflexiveObjectProperty;
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let m = model(vec![
        Component::ReflexiveObjectProperty(ReflexiveObjectProperty(OPE::ObjectProperty(r.clone()))),
        sub(c("A"), CE::ObjectAllValuesFrom { ope: OPE::ObjectProperty(r.clone()), bce: Box::new(c("B")) }),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(rr.is_consistent());
    assert!(
        rr.is_subsumed(&format!("{NS}A"), &format!("{NS}B")),
        "A ⊑ B must follow from Reflexive(r) + A ⊑ ∀r.B"
    );
}

#[test]
fn has_self_universal_propagation() {
    // A ⊑ ∃r.Self, A ⊑ ∀r.B  ⟹  A ⊑ B (the self is an r-successor).
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let m = model(vec![
        sub(c("A"), has_self(&r)),
        sub(c("A"), CE::ObjectAllValuesFrom { ope: OPE::ObjectProperty(r.clone()), bce: Box::new(c("B")) }),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(rr.is_consistent());
    assert!(
        rr.is_subsumed(&format!("{NS}A"), &format!("{NS}B")),
        "A ⊑ B must follow from A ⊑ ∃r.Self ⊓ ∀r.B"
    );
}

#[test]
fn has_self_irreflexive_clash() {
    // Irreflexive(r), A ⊑ ∃r.Self  ⟹  A unsatisfiable (a self-loop on an
    // irreflexive role is a contradiction).
    use horned_owl::model::IrreflexiveObjectProperty;
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let m = model(vec![
        Component::IrreflexiveObjectProperty(IrreflexiveObjectProperty(OPE::ObjectProperty(r.clone()))),
        sub(c("A"), has_self(&r)),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        rr.unsatisfiable().contains(&format!("{NS}A")),
        "A ⊑ ∃r.Self with Irreflexive(r) must be unsatisfiable"
    );
}

#[test]
fn negative_self_clash() {
    // A ⊑ ∃r.Self, A ⊑ ¬∃r.Self  ⟹  A unsatisfiable.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let m = model(vec![
        sub(c("A"), has_self(&r)),
        sub(c("A"), CE::ObjectComplementOf(Box::new(has_self(&r)))),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        rr.unsatisfiable().contains(&format!("{NS}A")),
        "A ⊑ ∃r.Self ⊓ ¬∃r.Self must be unsatisfiable"
    );
}

#[test]
fn disjoint_object_properties_abox_inconsistency() {
    // Disjoint(r, s), r(a,b), s(a,b)  ⟹  inconsistent (one pair in two disjoint
    // roles).
    use horned_owl::model::{
        DisjointObjectProperties, Individual, NamedIndividual, ObjectPropertyAssertion,
    };
    let b = Build::new_rc();
    let r = b.object_property(format!("{NS}r"));
    let s = b.object_property(format!("{NS}s"));
    let ind = |n: &str| Individual::Named(NamedIndividual(b.iri(format!("{NS}{n}"))));
    let m = model(vec![
        Component::DisjointObjectProperties(DisjointObjectProperties(vec![
            OPE::ObjectProperty(r.clone()),
            OPE::ObjectProperty(s.clone()),
        ])),
        Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
            ope: OPE::ObjectProperty(r.clone()),
            from: ind("a"),
            to: ind("b"),
        }),
        Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
            ope: OPE::ObjectProperty(s.clone()),
            from: ind("a"),
            to: ind("b"),
        }),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        !rr.is_consistent(),
        "r(a,b) ⊓ s(a,b) with Disjoint(r,s) must be inconsistent"
    );
}

#[test]
fn disjoint_object_properties_consistent_when_distinct() {
    // Disjoint(r, s), r(a,b), s(a,c) — different targets, so no clash.
    use horned_owl::model::{
        DisjointObjectProperties, Individual, NamedIndividual, ObjectPropertyAssertion,
    };
    let b = Build::new_rc();
    let r = b.object_property(format!("{NS}r"));
    let s = b.object_property(format!("{NS}s"));
    let ind = |n: &str| Individual::Named(NamedIndividual(b.iri(format!("{NS}{n}"))));
    let m = model(vec![
        Component::DisjointObjectProperties(DisjointObjectProperties(vec![
            OPE::ObjectProperty(r.clone()),
            OPE::ObjectProperty(s.clone()),
        ])),
        Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
            ope: OPE::ObjectProperty(r.clone()),
            from: ind("a"),
            to: ind("b"),
        }),
        Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
            ope: OPE::ObjectProperty(s.clone()),
            from: ind("a"),
            to: ind("c"),
        }),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(
        rr.is_consistent(),
        "distinct targets b≠c are not forced equal, so no disjoint-role clash"
    );
}

#[test]
#[should_panic(expected = "Non-simple")]
fn non_simple_self_is_rejected() {
    // Self on a *non-simple* (transitive) role violates OWL 2 DL's regularity
    // restriction. hermit-rs rejects such an ontology with a "Non-simple property
    // … appears in a Self restriction" error rather than silently weakening it,
    // and the adapter surfaces that as a panic.
    use horned_owl::model::TransitiveObjectProperty;
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let m = model(vec![
        Component::TransitiveObjectProperty(TransitiveObjectProperty(OPE::ObjectProperty(r.clone()))),
        sub(c("A"), has_self(&r)),
    ]);
    DlReasoner::classify(&m).is_consistent();
}

// ---- genus-differentia absorption (∃R.(G⊓D) ⊑ X via a Horn DL-clause) -------

#[test]
fn genus_differentia_nested_subsumption() {
    // X ⊇ ∃R.(G ⊓ ∃S.V).  A ⊑ ∃R.M, M ⊑ G, M ⊑ ∃S.V  ⟹  A ⊑ X.
    // This is exactly the EFO genus-differentia shape that absorption turns into
    // the Horn clause `R(x,y) ∧ G(y) ∧ Q(y) → X(x)` (with Q naming `∃S.V`). Must
    // hold identically with absorption on or off — it is a real entailment.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let s = b.object_property(format!("{NS}s"));
    let some = |p: &horned_owl::model::ObjectProperty<horned_owl::model::RcStr>, f: CE<_>| {
        CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(p.clone()), bce: Box::new(f) }
    };
    // ∃R.(G ⊓ ∃S.V) ⊑ X
    let diff = CE::ObjectIntersectionOf(vec![c("G"), some(&s, c("V"))]);
    let m = model(vec![
        sub(some(&r, diff), c("X")),
        sub(c("A"), some(&r, c("M"))),
        sub(c("M"), c("G")),
        sub(c("M"), some(&s, c("V"))),
    ]);
    let rr = DlReasoner::classify(&m);
    assert!(rr.is_consistent());
    assert!(
        rr.is_subsumed(&format!("{NS}A"), &format!("{NS}X")),
        "A ⊑ X via the genus-differentia clause ∃R.(G ⊓ ∃S.V) ⊑ X"
    );
}
