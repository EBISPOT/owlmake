//! Round-trip tests for the OBO Graphs JSON, Manchester and Turtle bridges.

use horned_owl::model::{
    AnnotationAssertion, AnnotationSubject, AnnotationValue, Build, ClassExpression as CE, Component,
    DeclareClass, Literal, MutableOntology, SubClassOf,
};
use horned_owl::ontology::set::SetOntology;

use owlmake::io::{self, Format};
use owlmake::model::Model;

const NS: &str = "http://purl.obolibrary.org/obo/";

fn sample() -> Model {
    let b = Build::new();
    let mut ont = SetOntology::new();
    let a = b.class(format!("{NS}X_1"));
    let c = b.class(format!("{NS}X_2"));
    ont.insert(Component::DeclareClass(DeclareClass(a.clone())));
    ont.insert(Component::DeclareClass(DeclareClass(c.clone())));
    ont.insert(Component::SubClassOf(SubClassOf {
        sub: CE::Class(a.clone()),
        sup: CE::Class(c.clone()),
    }));
    ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
        subject: AnnotationSubject::IRI(b.iri(format!("{NS}X_1"))),
        ann: horned_owl::model::Annotation {
            ap: b.annotation_property("http://www.w3.org/2000/01/rdf-schema#label"),
            av: AnnotationValue::Literal(Literal::Simple {
                literal: "alpha".to_string(),
            }),
            ann: Default::default(),
        },
    }));
    Model::from_parts(ont, owlmake::model::default_prefixes())
}

fn classes(m: &Model) -> usize {
    m.ont
        .iter()
        .filter(|ac| matches!(ac.component, Component::DeclareClass(_)))
        .count()
}

fn subclass_edges(m: &Model) -> usize {
    m.ont
        .iter()
        .filter(|ac| matches!(ac.component, Component::SubClassOf(_)))
        .count()
}

/// The tests below all parse documents, and every parse draws blank-node ids
/// from one process-global counter (`io::reset_anon_counter` /
/// `ANON_COUNTER`). Run in parallel, a round trip in one test can take an id
/// between another test's reset and its read, so the id assertions race — rarely,
/// but a full `cargo test` did lose it once. The counter is process state by
/// design (one run, one sequence), so the tests take turns instead.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn obograph_json_roundtrip() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let m = sample();
    let mut json = Vec::new();
    io::write_to_ref(&m, &mut json, Format::OboGraph).unwrap();
    let text = String::from_utf8(json.clone()).unwrap();
    assert!(text.contains("\"nodes\""));
    assert!(text.contains("alpha"));

    let m2 = io::load_from(std::io::Cursor::new(json), Format::OboGraph).unwrap();
    assert_eq!(classes(&m2), 2);
    assert_eq!(subclass_edges(&m2), 1);
}

#[test]
fn manchester_roundtrip() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // Add an existential so the Manchester `some` parser is exercised.
    let b = Build::new();
    let mut m = sample();
    m.ont.insert(Component::DeclareObjectProperty(
        horned_owl::model::DeclareObjectProperty(b.object_property(format!("{NS}r"))),
    ));
    m.ont.insert(Component::SubClassOf(SubClassOf {
        sub: CE::Class(b.class(format!("{NS}X_1"))),
        sup: CE::ObjectSomeValuesFrom {
            ope: horned_owl::model::ObjectPropertyExpression::ObjectProperty(
                b.object_property(format!("{NS}r")),
            ),
            bce: Box::new(CE::Class(b.class(format!("{NS}X_2")))),
        },
    }));

    let mut omn = Vec::new();
    io::write_to_ref(&m, &mut omn, Format::Manchester).unwrap();
    let text = String::from_utf8(omn.clone()).unwrap();
    assert!(text.contains("Class:"));
    assert!(text.contains("some"));

    let m2 = io::load_from(std::io::Cursor::new(omn), Format::Manchester).unwrap();
    assert_eq!(classes(&m2), 2);
    // both the named is_a and the existential subclass survive
    assert!(subclass_edges(&m2) >= 2, "expected the is_a and existential edges");
}

#[test]
fn turtle_roundtrip() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let m = sample();
    let mut ttl = Vec::new();
    io::write_to_ref(&m, &mut ttl, Format::Turtle).unwrap();
    let m2 = io::load_from(std::io::Cursor::new(ttl), Format::Turtle).unwrap();
    assert_eq!(classes(&m2), 2);
    assert_eq!(subclass_edges(&m2), 1);
}

/// Blank-node ids are local to the document that states them, and every parse
/// re-mints them from 2^31 in first-mention order.
///
/// Two documents that both say `_:x` name two individuals, not one, so a merge of
/// an ontology with a copy of one of its own imports has to keep both copies of
/// every anonymous axiom. The label a document carried is never the label it is
/// read back under: `_:zzz` mentioned first becomes `_:genid2147483648` however it
/// sorts, and a `_:` inside a string literal is text, not a node id.
#[test]
fn a_functional_documents_node_ids_are_reminted_in_first_mention_order() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const DOC: &str = concat!(
        "Prefix(:=<http://ex.org/>)\n",
        "Ontology(<http://ex.org/anon.owl>\n",
        "Declaration(Class(:A))\n",
        "Declaration(AnnotationProperty(:p))\n",
        "AnnotationAssertion(:p :A _:zzz)\n",
        "AnnotationAssertion(rdfs:label _:zzz \"second mentioned first\")\n",
        "AnnotationAssertion(:p :A _:aaa)\n",
        "AnnotationAssertion(rdfs:label _:aaa \"quoted _:notalabel here\")\n",
        ")\n"
    );

    let read = || {
        io::reset_anon_counter();
        io::load_from(std::io::Cursor::new(DOC.as_bytes().to_vec()), Format::Functional).unwrap()
    };
    let mut m = read();
    let mut out = Vec::new();
    io::write_to_ref(&m, &mut out, Format::Functional).unwrap();
    let text = String::from_utf8(out).unwrap();

    assert!(
        text.contains("_:genid2147483648)") && text.contains("_:genid2147483648 \"second mentioned first\""),
        "the first label mentioned takes the first id:\n{text}"
    );
    assert!(
        text.contains("_:genid2147483649)") && text.contains("_:genid2147483649 \"quoted"),
        "the second label mentioned takes the second id:\n{text}"
    );
    assert!(
        text.contains("\"quoted _:notalabel here\""),
        "a `_:` inside a string literal is text, not a node id:\n{text}"
    );
    assert!(!text.contains("_:zzz") && !text.contains("_:aaa"), "source labels survived:\n{text}");

    // A second read in the same process continues the counter — one run, one
    // sequence — which is what makes a merge of two documents keep them apart.
    let m2 = io::load_from(std::io::Cursor::new(DOC.as_bytes().to_vec()), Format::Functional)
        .unwrap();
    let mut out2 = Vec::new();
    io::write_to_ref(&m2, &mut out2, Format::Functional).unwrap();
    let text2 = String::from_utf8(out2).unwrap();
    assert!(
        text2.contains("_:genid2147483650)") && text2.contains("_:genid2147483651)"),
        "a second parse takes the next ids, not the same ones:\n{text2}"
    );

    // …and the counter reset makes the read reproducible.
    m = read();
    let mut out3 = Vec::new();
    io::write_to_ref(&m, &mut out3, Format::Functional).unwrap();
    assert_eq!(text, String::from_utf8(out3).unwrap());
}

/// An equivalence between two INVERSE properties has no named subject to hang
/// off, so it is written as its own anonymous block, right after the block of
/// the property the first inverse names. Dropped instead, `mirror/stato.owl`
/// loses `EquivalentObjectProperties(inverse(IAO_0000235)
/// inverse(STATO_0000205))` — and so does every mirror merged from it.
#[test]
fn an_equivalence_between_two_inverses_survives_rdfxml() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use horned_owl::model::{
        EquivalentObjectProperties, ObjectPropertyExpression as OPE,
    };
    let b = Build::new();
    let mut ont = SetOntology::new();
    let p = b.object_property(format!("{NS}P_1"));
    let q = b.object_property(format!("{NS}P_2"));
    ont.insert(Component::DeclareObjectProperty(horned_owl::model::DeclareObjectProperty(
        p.clone(),
    )));
    ont.insert(Component::DeclareObjectProperty(horned_owl::model::DeclareObjectProperty(
        q.clone(),
    )));
    ont.insert(Component::EquivalentObjectProperties(EquivalentObjectProperties(vec![
        OPE::InverseObjectProperty(p.clone()),
        OPE::InverseObjectProperty(q.clone()),
    ])));
    let mut model = Model::from_parts(ont, owlmake::model::default_prefixes());

    let dir = std::env::temp_dir().join(format!("om-inveq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.owl");
    io::save_as(&mut model, &path, Format::RdfXml).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    // The element is unprefixed here: an ontology with no IRI of its own leaves
    // OWL as the document's default namespace.
    assert!(
        text.contains("<equivalentProperty>"),
        "the anonymous equivalence block is missing:\n{text}"
    );

    let back = io::load(&path).unwrap();
    let kept = back.ont.iter().any(|ac| {
        matches!(&ac.component, Component::EquivalentObjectProperties(e)
            if e.0.len() == 2
                && matches!(&e.0[0], OPE::InverseObjectProperty(_))
                && matches!(&e.0[1], OPE::InverseObjectProperty(_)))
    });
    assert!(kept, "the axiom did not survive the round trip:\n{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
