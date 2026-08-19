//! Conformance of the EL reasoner over the OWL 2 EL classification suite in
//! `tests/elk`.
//!
//! Each `tests/elk/<Name>.owl` is a small OWL 2 EL ontology isolating one
//! construct, and `<Name>.taxonomy` states the classification it must have:
//! direct SubClassOf edges, cycles collapsed to EquivalentClasses,
//! unsatisfiable classes grouped with owl:Nothing. We classify each input and
//! require the canonical taxonomy to equal the one its `.taxonomy` states, so a
//! gap in a single construct surfaces as a named failure instead of as a wrong
//! edge deep inside some real ontology.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use horned_owl::model::{ClassExpression as CE, Component};

use owlmake::io::{self, Format};
use owlmake::model::Model;
use owlmake::reason::{
    el::{merge_overlapping_groups, ClassTaxonomy},
    Reasoner,
};

const ELK_DIR: &str = "tests/elk";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

fn class_iri(ce: &CE<horned_owl::model::RcStr>) -> Option<String> {
    match ce {
        CE::Class(c) => Some(c.0.to_string()),
        _ => None,
    }
}

/// Build the canonical taxonomy directly from an expected `.taxonomy` model.
fn expected_taxonomy(model: &Model) -> ClassTaxonomy {
    let mut equivalences: BTreeSet<BTreeSet<String>> = BTreeSet::new();
    let mut iri_to_rep: HashMap<String, String> = HashMap::new();

    for ac in model.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            let iris: BTreeSet<String> = eq.0.iter().filter_map(class_iri).collect();
            if iris.len() >= 2 {
                let rep = iris.iter().min().unwrap().clone();
                for i in &iris {
                    iri_to_rep.insert(i.clone(), rep.clone());
                }
                equivalences.insert(iris);
            }
        }
    }

    let rep = |iri: &str| iri_to_rep.get(iri).cloned().unwrap_or_else(|| iri.to_string());
    let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (Some(a), Some(b)) = (class_iri(&sc.sub), class_iri(&sc.sup)) {
                let (ra, rb) = (rep(&a), rep(&b));
                let touches_reserved = [&ra, &rb]
                    .iter()
                    .any(|x| *x == OWL_THING || *x == OWL_NOTHING);
                if ra != rb && !touches_reserved {
                    edges.insert((ra, rb));
                }
            }
        }
    }

    ClassTaxonomy {
        equivalences: merge_overlapping_groups(equivalences),
        edges,
    }
}

fn names() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(ELK_DIR)
        .expect("elk dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.extension().map(|x| x == "owl").unwrap_or(false) {
                p.file_stem().map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();
    v.sort();
    v
}

fn p(name: &str, ext: &str) -> PathBuf {
    Path::new(ELK_DIR).join(format!("{name}.{ext}"))
}

/// Percent-encode the second and later `#` inside any `<...>` IRI. The
/// `tests/elk` fixtures carry technically-invalid IRIs like
/// `<http://x#a#taxonomy>` for the ontology id; this normalizes them so a
/// strict parser accepts them. The ontology id is ignored in taxonomy
/// comparison regardless.
fn sanitize_iris(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_iri = false;
    let mut seen_hash = false;
    for ch in text.chars() {
        match ch {
            '<' => {
                in_iri = true;
                seen_hash = false;
                out.push(ch);
            }
            '>' => {
                in_iri = false;
                out.push(ch);
            }
            '#' if in_iri => {
                if seen_hash {
                    out.push_str("%23");
                } else {
                    seen_hash = true;
                    out.push('#');
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Load a `tests/elk` fixture (always OWL Functional Syntax), with IRI sanitization.
fn load_functional(path: &Path) -> Result<Model, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let sanitized = sanitize_iris(&text);
    io::load_from(std::io::Cursor::new(sanitized.into_bytes()), Format::Functional)
        .map_err(|e| format!("{e:#}"))
}

#[test]
fn elk_classification_conformance() {
    let mut pass = 0usize;
    let mut total = 0usize;
    let mut fails: Vec<String> = Vec::new();

    for name in names() {
        total += 1;
        let result = (|| -> Result<(), String> {
            let input = load_functional(&p(&name, "owl")).map_err(|e| format!("load input: {e}"))?;
            let expected_model =
                load_functional(&p(&name, "taxonomy")).map_err(|e| format!("load taxonomy: {e}"))?;
            let got = Reasoner::classify(&input).taxonomy();
            let want = expected_taxonomy(&expected_model);
            if got == want {
                Ok(())
            } else {
                let eq_missing: Vec<_> = want.equivalences.difference(&got.equivalences).collect();
                let eq_extra: Vec<_> = got.equivalences.difference(&want.equivalences).collect();
                let ed_missing: Vec<_> = want.edges.difference(&got.edges).collect();
                let ed_extra: Vec<_> = got.edges.difference(&want.edges).collect();
                Err(format!(
                    "eq missing {eq_missing:?} extra {eq_extra:?}; edges missing {ed_missing:?} extra {ed_extra:?}"
                ))
            }
        })();
        match result {
            Ok(()) => pass += 1,
            Err(e) => fails.push(format!("  {name}: {e}")),
        }
    }

    eprintln!(
        "\n=== ELK classification conformance: {pass}/{total} pass ({:.1}%) ===",
        100.0 * pass as f64 / total.max(1) as f64
    );
    for f in &fails {
        eprintln!("{f}");
    }

    // Every fixture in the suite must classify exactly as its `.taxonomy` states.
    assert_eq!(pass, total, "ELK conformance regressed ({pass}/{total})");
}
