//! Tests for the DL reasoner's entailment checker and instance-reasoning API
//! (`reason::entails`, `is_instance`, `types`, `instances`), plus the axiom
//! handlers that keep it complete over the SROIQ(D) fragment the DL reasoner
//! decides (symmetric properties, disjoint unions, data-property domains,
//! finite-datatype cardinality).

use horned_owl::model::{
    Build, Class, ClassAssertion, ClassExpression as CE, Component, DataProperty,
    DataPropertyDomain, DisjointUnion, Individual, MutableOntology, NamedIndividual,
    ObjectPropertyAssertion, ObjectPropertyExpression as OPE, SubClassOf,
    SymmetricObjectProperty,
};
use horned_owl::ontology::set::SetOntology;

use owlmake::model::Model;
use owlmake::reason::{entails, instances, is_instance, types};

const NS: &str = "http://example.org/";

fn model(components: Vec<Component<horned_owl::model::RcStr>>) -> Model {
    let mut ont = SetOntology::new();
    for c in components {
        ont.insert(c);
    }
    Model::from_parts(ont, owlmake::model::default_prefixes())
}

fn cls(n: &str) -> Class<horned_owl::model::RcStr> {
    Build::new_rc().class(format!("{NS}{n}"))
}
fn ce(n: &str) -> CE<horned_owl::model::RcStr> {
    CE::Class(cls(n))
}
fn ind(n: &str) -> Individual<horned_owl::model::RcStr> {
    Individual::Named(NamedIndividual(Build::new_rc().iri(format!("{NS}{n}"))))
}
fn sub(x: CE<horned_owl::model::RcStr>, y: CE<horned_owl::model::RcStr>) -> Component<horned_owl::model::RcStr> {
    Component::SubClassOf(SubClassOf { sub: x, sup: y })
}

#[test]
fn entails_transitive_subclass() {
    // Premise: A ⊑ B, B ⊑ C. Conclusion: A ⊑ C is entailed; C ⊑ A is not.
    let premise = model(vec![sub(ce("A"), ce("B")), sub(ce("B"), ce("C"))]);
    let yes = model(vec![sub(ce("A"), ce("C"))]);
    let no = model(vec![sub(ce("C"), ce("A"))]);
    assert!(entails(&premise, &yes), "A ⊑ C follows transitively");
    assert!(!entails(&premise, &no), "C ⊑ A does not follow");
}

#[test]
fn entails_instance_of() {
    // A ⊑ B, a : A  ⟹  a : B.
    let premise = model(vec![
        sub(ce("A"), ce("B")),
        Component::ClassAssertion(ClassAssertion { ce: ce("A"), i: ind("a") }),
    ]);
    let conc = model(vec![Component::ClassAssertion(ClassAssertion {
        ce: ce("B"),
        i: ind("a"),
    })]);
    assert!(entails(&premise, &conc));
    assert!(is_instance(&premise, &format!("{NS}a"), &format!("{NS}B")));
    assert!(!is_instance(&premise, &format!("{NS}a"), &format!("{NS}C")));
    assert!(types(&premise, &format!("{NS}a")).contains(&format!("{NS}B")));
    assert!(instances(&premise, &format!("{NS}B")).contains(&format!("{NS}a")));
}

#[test]
fn symmetric_property_is_complete() {
    // Premise: r symmetric, a r b. Conclusion: b r a is entailed — symmetry has
    // to be enforced on the assertions, not merely declared on the property.
    let r = Build::new_rc().object_property(format!("{NS}r"));
    let premise = model(vec![
        Component::SymmetricObjectProperty(SymmetricObjectProperty(OPE::ObjectProperty(r.clone()))),
        Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
            ope: OPE::ObjectProperty(r.clone()),
            from: ind("a"),
            to: ind("b"),
        }),
    ]);
    let conc = model(vec![Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
        ope: OPE::ObjectProperty(r.clone()),
        from: ind("b"),
        to: ind("a"),
    })]);
    assert!(entails(&premise, &conc), "b r a follows from symmetry of r");
}

#[test]
fn disjoint_union_classifies_and_is_disjoint() {
    // DisjointUnion(Animal, {Cat, Dog}): Cat ⊑ Animal, and Cat ⊓ Dog is empty.
    let premise = model(vec![Component::DisjointUnion(DisjointUnion(
        cls("Animal"),
        vec![ce("Cat"), ce("Dog")],
    ))]);
    assert!(entails(&premise, &model(vec![sub(ce("Cat"), ce("Animal"))])));
    assert!(entails(
        &premise,
        &model(vec![Component::DisjointClasses(horned_owl::model::DisjointClasses(
            vec![ce("Cat"), ce("Dog")]
        ))])
    ));
}

#[test]
fn data_property_domain_entails_membership() {
    // Domain(hasAge) = Person, a hasAge 5  ⟹  a : Person.
    let b = Build::new_rc();
    let dp = DataProperty(b.iri(format!("{NS}hasAge")));
    let lit = horned_owl::model::Literal::Datatype {
        literal: "5".to_string(),
        datatype_iri: b.iri("http://www.w3.org/2001/XMLSchema#integer"),
    };
    let premise = model(vec![
        Component::DataPropertyDomain(DataPropertyDomain { dp: dp.clone(), ce: ce("Person") }),
        Component::DataPropertyAssertion(horned_owl::model::DataPropertyAssertion {
            dp: dp.clone(),
            from: ind("a"),
            to: lit,
        }),
    ]);
    let conc = model(vec![Component::ClassAssertion(ClassAssertion {
        ce: ce("Person"),
        i: ind("a"),
    })]);
    assert!(entails(&premise, &conc), "a : Person follows from the data-property domain");
}
