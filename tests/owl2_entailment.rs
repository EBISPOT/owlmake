//! W3C OWL 2 **entailment** conformance suite harness.
//!
//! The companion to `owl2_conformance.rs` (which runs the Consistency /
//! Inconsistency tests). This loads every PositiveEntailment / NegativeEntailment
//! test from the official manifest that ships a functional-syntax premise *and*
//! conclusion, and checks them with the SROIQ(D) DL reasoner's entailment checker
//! (`reason::entails`). PositiveEntailment ⇒ premise ⊨ conclusion; Negative ⇒
//! premise ⊭ conclusion. Reports the pass rate and guards it against regression.

use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

use owlmake::io::{self, Format};
use owlmake::reason::entails;

const MANIFEST: &str = "tests/owl2/all.rdf";

/// (kind, premise fs, conclusion fs) for the entailment-style tests.
fn load_cases() -> Vec<(String, String, String)> {
    let text = std::fs::read_to_string(MANIFEST).expect("read manifest");
    // Strip the DOCTYPE oxigraph's RDF/XML parser rejects and expand the four
    // manifest entities (they occur only in manifest markup, never inside the
    // premise/conclusion string literals).
    let text = match (text.find("<!DOCTYPE"), text.find("]>")) {
        (Some(s), Some(e)) if e > s => format!("{}{}", &text[..s], &text[e + 2..]),
        _ => text,
    };
    let text = text
        .replace("&test;", "http://www.w3.org/2007/OWL/testOntology#")
        .replace("&rdf;", "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
        .replace("&rdfs;", "http://www.w3.org/2000/01/rdf-schema#")
        .replace("&owl;", "http://www.w3.org/2002/07/owl#");

    let store = Store::new().unwrap();
    store
        .load_from_slice(RdfParser::from_format(RdfFormat::RdfXml), text.as_bytes())
        .expect("oxigraph parses the preprocessed OWL2 test manifest");

    let query = r#"
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        PREFIX test: <http://www.w3.org/2007/OWL/testOntology#>
        SELECT ?type ?premise ?conclusion WHERE {
            ?t rdf:type ?type ;
               test:fsPremiseOntology ?premise .
            { ?t test:fsConclusionOntology ?conclusion }
            UNION
            { ?t test:fsNonConclusionOntology ?conclusion }
            FILTER(?type = test:PositiveEntailmentTest || ?type = test:NegativeEntailmentTest)
        }
    "#;
    let results = SparqlEvaluator::new()
        .parse_query(query)
        .unwrap()
        .on_store(&store)
        .execute()
        .unwrap();

    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions {
            let sol = sol.unwrap();
            let kind = match sol.get("type") {
                Some(oxigraph::model::Term::NamedNode(n)) => {
                    n.as_str().rsplit('#').next().unwrap_or("").to_string()
                }
                _ => continue,
            };
            let premise = match sol.get("premise") {
                Some(oxigraph::model::Term::Literal(l)) => l.value().to_string(),
                _ => continue,
            };
            let conclusion = match sol.get("conclusion") {
                Some(oxigraph::model::Term::Literal(l)) => l.value().to_string(),
                _ => continue,
            };
            out.push((kind, premise, conclusion));
        }
    }
    out
}

#[test]
fn owl2_entailment_conformance() {
    let cases = load_cases();
    assert!(
        cases.len() >= 20,
        "expected the OWL2 manifest to yield many fs entailment tests, got {}",
        cases.len()
    );

    let is_datatype = |p: &str| {
        p.contains("DataProperty")
            || p.contains("DataSomeValuesFrom")
            || p.contains("DataAllValuesFrom")
            || p.contains("DataHasValue")
            || p.contains("DataOneOf")
            || p.contains("DatatypeRestriction")
            || p.contains("DataMinCardinality")
            || p.contains("DataMaxCardinality")
            || p.contains("DataExactCardinality")
            || p.contains("DataPropertyAssertion")
            || p.contains("DatatypeDefinition")
            || p.contains("DataComplementOf")
    };

    let mut pass = 0usize;
    let mut total = 0usize;
    let mut parse_fail = 0usize;
    let mut fail_datatype = 0usize;
    let mut fail_object = 0usize;

    for (kind, premise, conclusion) in &cases {
        let expect_entailed = kind == "PositiveEntailmentTest";
        let pm = match io::load_from(std::io::Cursor::new(premise.clone().into_bytes()), Format::Functional) {
            Ok(m) => m,
            Err(_) => {
                parse_fail += 1;
                continue;
            }
        };
        let cm = match io::load_from(std::io::Cursor::new(conclusion.clone().into_bytes()), Format::Functional) {
            Ok(m) => m,
            Err(_) => {
                parse_fail += 1;
                continue;
            }
        };
        total += 1;
        let entailed = entails(&pm, &cm);
        if entailed == expect_entailed {
            pass += 1;
        } else if is_datatype(premise) || is_datatype(conclusion) {
            fail_datatype += 1;
            if std::env::var("SHOW_DT").is_ok() {
                eprintln!("--- DATATYPE MISS ({kind}) ---\nPREMISE:\n{premise}\nCONCLUSION:\n{conclusion}\n");
            }
        } else {
            fail_object += 1;
            eprintln!("--- OBJECT-ONLY MISS ({kind}) ---\nPREMISE:\n{premise}\nCONCLUSION:\n{conclusion}\n");
        }
    }

    eprintln!(
        "\n=== OWL2 entailment conformance: {pass}/{total} pass ({:.1}%), {parse_fail} unparsed ===",
        100.0 * pass as f64 / total.max(1) as f64
    );
    eprintln!("    misses: {fail_datatype} datatype-dependent, {fail_object} object-only");

    // The reasoner is sound everywhere and complete on the entire fs entailment
    // suite (object *and* datatype). Lock that in: every premise/conclusion
    // parses, and every entailment is decided correctly.
    assert_eq!(parse_fail, 0, "every OWL2 entailment premise/conclusion should parse");
    assert_eq!(
        fail_object, 0,
        "object-only OWL2 entailment conformance regressed (should be 100%)"
    );
    assert_eq!(
        fail_datatype, 0,
        "datatype OWL2 entailment conformance regressed (should be 100%)"
    );
    assert_eq!(
        pass, total,
        "OWL2 entailment conformance regressed below 100% ({pass}/{total})"
    );
}
