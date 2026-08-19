//! W3C OWL 2 conformance suite harness.
//!
//! Loads the official test manifest (`tests/owl2/all.rdf` from
//! <https://www.w3.org/2009/11/owl-test/>) with the embedded SPARQL engine,
//! extracts every Consistency / Inconsistency test that ships a functional-
//! syntax premise, and runs it through the SROIQ DL reasoner. Reports the pass
//! rate and guards it against regression.

use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

use owlmake::io::{self, Format};
use owlmake::reason::DlReasoner;

const MANIFEST: &str = "tests/owl2/all.rdf";

/// (kind, premise functional-syntax text) for the consistency-style tests.
fn load_cases() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(MANIFEST).expect("read manifest");
    // The W3C manifest declares its namespace entities with single quotes in a
    // DOCTYPE that oxigraph's RDF/XML parser rejects. Strip the DOCTYPE and
    // expand the four manifest entities (they occur only in manifest markup,
    // never unescaped inside the premise string literals).
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
        SELECT ?type ?premise WHERE {
            ?t rdf:type ?type ;
               test:fsPremiseOntology ?premise .
            FILTER(?type = test:ConsistencyTest || ?type = test:InconsistencyTest)
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
            out.push((kind, premise));
        }
    }
    out
}

#[test]
fn owl2_consistency_conformance() {
    let cases = load_cases();
    assert!(
        cases.len() >= 50,
        "expected the OWL2 manifest to yield many fs consistency tests, got {}",
        cases.len()
    );

    let mut pass = 0usize;
    let mut total = 0usize;
    let mut parse_fail = 0usize;
    let mut fail_datatype = 0usize;
    let mut fail_object = 0usize;

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
    };

    for (kind, premise) in &cases {
        let expect_consistent = kind == "ConsistencyTest";
        let model = match io::load_from(std::io::Cursor::new(premise.clone().into_bytes()), Format::Functional) {
            Ok(m) => m,
            Err(e) => {
                parse_fail += 1;
                if std::env::var("SHOW_PARSE").is_ok() {
                    eprintln!("--- UNPARSED ({kind}): {e} ---\n{premise}\n");
                }
                continue; // premise uses syntax we don't parse; skip (not a reasoner failure)
            }
        };
        total += 1;
        let consistent = DlReasoner::classify(&model).is_consistent();
        if consistent == expect_consistent {
            pass += 1;
        } else if is_datatype(premise) {
            fail_datatype += 1;
            if std::env::var("SHOW_DT").is_ok() {
                eprintln!("--- DATATYPE MISS ({kind}) ---\n{premise}\n");
            }
        } else {
            fail_object += 1;
            eprintln!("--- OBJECT-ONLY MISS ({kind}) ---\n{premise}\n");
        }
    }

    eprintln!(
        "\n=== OWL2 consistency conformance: {pass}/{total} pass ({:.1}%), {parse_fail} unparsed ===",
        100.0 * pass as f64 / total.max(1) as f64
    );
    eprintln!("    misses: {fail_datatype} datatype-dependent, {fail_object} object-only");
    // Among object-only (datatype-free) tests we should be (near) complete.
    let object_total = total - fail_datatype;
    let object_pass = pass; // all passes are object-decidable
    eprintln!(
        "    object-only conformance: {object_pass}/{object_total} ({:.1}%)",
        100.0 * object_pass as f64 / object_total.max(1) as f64
    );

    // The reasoner is sound everywhere and complete on the entire
    // consistency/inconsistency suite (object *and* datatype). Lock that in:
    // every premise parses, and every case decides correctly.
    assert_eq!(parse_fail, 0, "every OWL2 consistency premise should parse");
    assert_eq!(
        fail_object, 0,
        "object-only OWL2 consistency conformance regressed (should be 100%)"
    );
    assert_eq!(
        fail_datatype, 0,
        "datatype OWL2 consistency conformance regressed (should be 100%)"
    );
    assert_eq!(
        pass, total,
        "OWL2 consistency conformance regressed below 100% ({pass}/{total})"
    );
}
