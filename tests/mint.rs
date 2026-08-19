//! `mint` — the `kgcl:mint` operation: rewrite temporary IDs to definitive ones
//! drawn from a named ID range.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use owlmake::cmd::mint::{self, Args};

/// A tiny edit ontology in CL's shape: two temporary `CL_99…` classes (one a
/// subclass of the other), a real root, and one class already sitting on the
/// range floor (`CL_0020000`) so allocation must skip it.
const EDIT: &str = r#"Prefix(:=<http://purl.obolibrary.org/obo/cl.owl#>)
Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Prefix(obo:=<http://purl.obolibrary.org/obo/>)
Ontology(<http://purl.obolibrary.org/obo/cl/cl-edit.owl>
Declaration(Class(obo:CL_0000000))
Declaration(Class(obo:CL_0020000))
Declaration(Class(obo:CL_9900001))
Declaration(Class(obo:CL_9900002))
AnnotationAssertion(rdfs:label obo:CL_9900001 "cell A")
AnnotationAssertion(rdfs:label obo:CL_9900002 "cell B")
SubClassOf(obo:CL_9900001 obo:CL_0000000)
SubClassOf(obo:CL_9900002 obo:CL_9900001)
SubClassOf(obo:CL_0020000 obo:CL_0000000)
)
"#;

const IDRANGES: &str = r#"Prefix: idrange: <http://purl.obolibrary.org/obo/cl/idrange/>
Prefix: allocatedto: <http://purl.obolibrary.org/obo/IAO_0000597>
Ontology: <http://purl.obolibrary.org/obo/cl/cl-idranges.owl>
AnnotationProperty: allocatedto:
Datatype: idrange:31
    allocatedto: "Automation"
    EquivalentTo:
        xsd:integer[>= 20000, < 120000]
"#;

fn mint_edit(edit: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir()
        .join(format!("owlmake_mint_{}_{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("cl-edit.owl");
    std::fs::File::create(&input).unwrap().write_all(edit.as_bytes()).unwrap();
    // The idranges file is auto-detected next to the input.
    std::fs::File::create(dir.join("cl-idranges.owl")).unwrap().write_all(IDRANGES.as_bytes()).unwrap();
    let out = dir.join("out.ofn");

    mint::run(Args {
        input: Some(input),
        output: Some(out.clone()),
        format: Some("ofn".into()),
        temp_id_prefix: "http://purl.obolibrary.org/obo/CL_99".into(),
        id_range_name: "Automation".into(),
        id_ranges: None,
        common: Default::default(),
    })
    .unwrap();

    let text = std::fs::read_to_string(&out).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    text
}

#[test]
fn allocates_definitive_ids_skipping_used_and_rewriting_all_axioms() {
    let out = mint_edit(EDIT);

    // Temporary IDs are gone; the two get the two lowest free IDs in the range,
    // skipping CL_0020000 which is already used.
    assert!(!out.contains("CL_99"), "no temporary IDs should remain:\n{out}");
    assert!(out.contains("CL_0020001"), "first free ID assigned:\n{out}");
    assert!(out.contains("CL_0020002"), "second free ID assigned:\n{out}");

    // Every position is rewritten: labels, declarations, and the subclass edge
    // *between* the two former temp classes. The fixture declares `obo:`, so the
    // writer abbreviates the rewritten IRIs against that prefix.
    assert!(out.contains(r#"AnnotationAssertion(rdfs:label obo:CL_0020001 "cell A")"#), "{out}");
    assert!(out.contains("SubClassOf(obo:CL_0020002 obo:CL_0020001)"), "{out}");
    // The pre-existing floor class is untouched.
    assert!(out.contains("Declaration(Class(obo:CL_0020000))"), "{out}");
}

#[test]
fn no_temporary_ids_is_a_no_op() {
    // An ontology with nothing under the temp prefix mints nothing and round-trips.
    let edit = EDIT.replace("CL_9900001", "CL_0030001").replace("CL_9900002", "CL_0030002");
    let out = mint_edit(&edit);
    assert!(out.contains("CL_0030001") && out.contains("CL_0030002"));
    assert!(!out.contains("CL_0020001"), "nothing should be allocated:\n{out}");
}
