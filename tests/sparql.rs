//! SPARQL query/verify over an ontology via the embedded oxigraph engine.

use horned_owl::model::{Build, ClassExpression as CE, Component, MutableOntology, SubClassOf};
use horned_owl::ontology::set::SetOntology;

use owlmake::model::Model;
use owlmake::sparql::Queryable;

const NS: &str = "http://example.org/";

fn model() -> Model {
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let mut ont = SetOntology::new();
    ont.insert(Component::SubClassOf(SubClassOf { sub: c("A"), sup: c("B") }));
    ont.insert(Component::SubClassOf(SubClassOf { sub: c("B"), sup: c("C") }));
    Model::from_parts(ont, owlmake::model::default_prefixes())
}

#[test]
fn select_subclass_edges() {
    let q = Queryable::from_model(&model()).unwrap();
    let table = q
        .query_table(
            "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             SELECT ?a ?b WHERE { ?a rdfs:subClassOf ?b }",
        )
        .unwrap();
    assert_eq!(table.rows.len(), 2, "two asserted subClassOf edges");
    assert!(table.columns.contains(&"a".to_string()));
}

#[test]
fn ask_and_count() {
    let q = Queryable::from_model(&model()).unwrap();
    let ask = q
        .query_table(
            "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             ASK { <http://example.org/A> rdfs:subClassOf <http://example.org/B> }",
        )
        .unwrap();
    assert_eq!(ask.rows[0][0], "true");

    let n = q
        .count("PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                SELECT ?a WHERE { ?a rdfs:subClassOf ?b }")
        .unwrap();
    assert_eq!(n, 2);
}
