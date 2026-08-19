//! Smoke test for `owlmake::api` — the stable function surface that the CLI and
//! the (Python/JS) bindings all wrap. Exercises the filesystem-free pipeline
//! over in-memory bytes: parse → reason → serialize, round-tripping the result.

use owlmake::api::{self, ReasonOptions};
use owlmake::io::Format;

const ONT: &[u8] = b"Prefix(:=<http://x.org/>)\n\
Ontology(<http://x.org/o>\n\
Declaration(Class(:A))\n\
Declaration(Class(:B))\n\
Declaration(Class(:C))\n\
EquivalentClasses(:A :B)\n\
SubClassOf(:B :C)\n\
)\n";

#[test]
fn parse_reason_serialize_roundtrip() {
    let model = api::parse(ONT, Format::Functional).expect("parse");
    let n_in = model.len();
    assert!(n_in > 0, "parsed model should be non-empty");

    let reasoned = api::reason(model, "elk", &ReasonOptions::default()).expect("reason");
    let bytes = api::serialize(&reasoned, Format::Functional).expect("serialize");

    // The output re-parses (round-trip stable) and carries at least the inputs.
    let reparsed = api::parse(&bytes, Format::Functional).expect("re-parse output");
    assert!(reparsed.len() >= n_in, "reasoned+serialized output lost axioms");

    let text = String::from_utf8(bytes).expect("utf-8 output");
    assert!(text.contains("SubClassOf"), "expected SubClassOf axioms:\n{text}");
}

#[test]
fn edit_axioms_and_accessors() {
    let mut model = api::parse(ONT, Format::Functional).expect("parse");

    // Structured read accessors over the parsed model.
    let classes = api::classes(&model);
    assert_eq!(classes, vec!["http://x.org/A", "http://x.org/B", "http://x.org/C"]);

    let before = api::axiom_count(&model);

    // Add a new class + subclass edge via an OFN fragment, resolved against the
    // document's own prefixes (`:` → http://x.org/).
    let added = api::add_axioms(&mut model, "Declaration(Class(:D))\nSubClassOf(:D :A)")
        .expect("add_axioms");
    assert_eq!(added, 2, "two fresh axioms should be inserted");
    assert_eq!(api::axiom_count(&model), before + 2);
    assert!(api::classes(&model).contains(&"http://x.org/D".to_string()));

    // Re-adding the same axioms is a no-op (the ontology is a set).
    let again = api::add_axioms(&mut model, "SubClassOf(:D :A)").expect("re-add");
    assert_eq!(again, 0);

    // The named subclass pair is visible, then removable.
    assert!(api::subclass_pairs(&model)
        .contains(&("http://x.org/D".to_string(), "http://x.org/A".to_string())));
    let removed = api::remove_axioms(&mut model, "SubClassOf(:D :A)").expect("remove");
    assert_eq!(removed, 1);
    assert!(!api::subclass_pairs(&model)
        .contains(&("http://x.org/D".to_string(), "http://x.org/A".to_string())));
}

#[test]
fn dl_query_manchester() {
    // Cells that are part-of some brain: only :Neuron (via part_of some :Brain).
    const ONT: &[u8] = b"Prefix(:=<http://x.org/>)\n\
Prefix(po:=<http://x.org/po_>)\n\
Ontology(<http://x.org/o>\n\
Declaration(Class(:Cell))\n\
Declaration(Class(:Neuron))\n\
Declaration(Class(:Brain))\n\
Declaration(ObjectProperty(:part_of))\n\
SubClassOf(:Neuron :Cell)\n\
SubClassOf(:Neuron ObjectSomeValuesFrom(:part_of :Brain))\n\
)\n";
    let model = api::parse(ONT, Format::Functional).expect("parse");

    // DL query: part_of some Brain  ->  Neuron is a descendant.
    let descendants = api::dl_query(&model, "part_of some Brain", "descendants", "elk")
        .expect("dl_query descendants");
    assert!(
        descendants.contains(&"http://x.org/Neuron".to_string()),
        "expected Neuron among descendants, got {descendants:?}"
    );

    // Named-class query: the descendants of Cell include Neuron.
    let subs_of_cell = api::dl_query(&model, "Cell", "descendants", "elk").expect("dl_query Cell");
    assert!(subs_of_cell.contains(&"http://x.org/Neuron".to_string()));

    // Superclasses of Neuron include Cell.
    let supers = api::dl_query(&model, "Neuron", "ancestors", "elk").expect("dl_query ancestors");
    assert!(supers.contains(&"http://x.org/Cell".to_string()), "got {supers:?}");
}
