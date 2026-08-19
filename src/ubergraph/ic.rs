//! Information-content metrics, computed over the redundant graph.
//!
//! For every term `t` (any subject in the graph):
//! - `referenceCount(t)` = number of triples with `t` as **object**;
//! - `subClassOfReferenceCount(t)` = number of `rdfs:subClassOf` triples with
//!   `t` as object;
//! - `maxIC = -ln(1/total)`, `scale = 100/maxIC`;
//! - `normalizedInformationContent(t) = -ln(refCount/total) * scale`;
//! - `normalizedSubClassInformationContent(t) = -ln(scRefCount/total) * scale`.
//!
//! Emits the N-Triples lines written to `information-content.nt` and loaded
//! into the ontology named graph:
//! - `t reasoner:normalizedInformationContent "ic"^^xsd:decimal`
//! - `t reasoner:normalizedSubClassInformationContent "ic"^^xsd:decimal`
//! - `t reasoner:referenceCount "n"^^xsd:integer`
//!
//! Because the redundant graph contains the reflexive `t rdfs:subClassOf t` for
//! every class, every term references itself, so counts are ≥ 1 and the
//! logarithms are well-defined.

use std::collections::HashMap;

use crate::ubergraph::{iri, IriTriple};

/// Per-term reference counts and the total term count.
struct Counts {
    total: usize,
    reference: HashMap<String, usize>,
    subclass_reference: HashMap<String, usize>,
}

fn compute(rdf: &[IriTriple]) -> Counts {
    use std::collections::HashSet;
    // A term is anything appearing as a subject in the graph.
    let mut terms: HashSet<&str> = HashSet::new();
    for (s, _, _) in rdf {
        terms.insert(s.as_str());
    }
    // referenceCount(t) = #triples with t as object (t must be a term).
    let mut reference: HashMap<String, usize> = HashMap::new();
    let mut subclass_reference: HashMap<String, usize> = HashMap::new();
    for (_, p, o) in rdf {
        if terms.contains(o.as_str()) {
            *reference.entry(o.clone()).or_insert(0) += 1;
            if p == iri::RDFS_SUBCLASS_OF {
                *subclass_reference.entry(o.clone()).or_insert(0) += 1;
            }
        }
    }
    Counts {
        total: terms.len(),
        reference,
        subclass_reference,
    }
}

/// Compute the information-content triples as N-Triples lines (sorted).
pub fn information_content(rdf: &[IriTriple]) -> Vec<String> {
    let c = compute(rdf);
    if c.total == 0 {
        return Vec::new();
    }
    let total = c.total as f64;
    let max_ic = -(1.0 / total).ln(); // = ln(total)
    let scale = if max_ic != 0.0 { 100.0 / max_ic } else { 0.0 };

    let mut lines: Vec<String> = Vec::new();
    for (t, &count) in &c.reference {
        let ic = -((count as f64) / total).ln() * scale;
        lines.push(format!(
            "<{t}> <{}> \"{}\"^^<{}> .",
            iri::NORMALIZED_IC,
            format_g6(ic),
            iri::XSD_DECIMAL
        ));
        // referenceCount as an integer literal.
        lines.push(format!(
            "<{t}> <{}> \"{count}\"^^<http://www.w3.org/2001/XMLSchema#integer> .",
            iri::REFERENCE_COUNT
        ));
    }
    for (t, &count) in &c.subclass_reference {
        let ic = -((count as f64) / total).ln() * scale;
        lines.push(format!(
            "<{t}> <{}> \"{}\"^^<{}> .",
            iri::NORMALIZED_SUBCLASS_IC,
            format_g6(ic),
            iri::XSD_DECIMAL
        ));
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Per-term reference-based information content over `rdf` (the redundant
/// graph): for each term, `(term, referenceCount, normalizedInformationContent)`,
/// sorted by term. This is the same `normalizedInformationContent` value
/// `information_content` serializes, exposed so callers (e.g. `owlmake
/// information-content --relations`) can attach it as OWL annotations instead of
/// writing triples. Unlike the subclass-only structural IC, `referenceCount`
/// here counts references across *all* relation edges, existential restrictions
/// included.
pub fn reference_ic(rdf: &[IriTriple]) -> Vec<(String, usize, f64)> {
    let c = compute(rdf);
    if c.total == 0 {
        return Vec::new();
    }
    let total = c.total as f64;
    let max_ic = -(1.0 / total).ln(); // = ln(total)
    let scale = if max_ic != 0.0 { 100.0 / max_ic } else { 0.0 };
    let mut out: Vec<(String, usize, f64)> = c
        .reference
        .iter()
        .map(|(t, &n)| (t.clone(), n, -((n as f64) / total).ln() * scale))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Format a float as printf `%.6g`: six significant digits, trailing zeros
/// stripped, scientific notation outside `[1e-4, 1e6)`. This is the lexical
/// form the `xsd:decimal` information-content literals carry — a fixed
/// precision keeps them short and keeps identical inputs producing identical
/// bytes.
pub fn format_g6(x: f64) -> String {
    const P: i32 = 6;
    if x == 0.0 || !x.is_finite() {
        return if x.is_finite() { "0".to_string() } else { x.to_string() };
    }
    let neg = x < 0.0;
    let ax = x.abs();
    // True decimal exponent via a scientific rendering with P-1 fractional digits.
    let sci = format!("{:.*e}", (P - 1) as usize, ax);
    let (mant, e) = {
        let mut it = sci.splitn(2, 'e');
        let m = it.next().unwrap().to_string();
        let e: i32 = it.next().unwrap().parse().unwrap();
        (m, e)
    };
    let body = if e >= -4 && e < P {
        let prec = (P - 1 - e).max(0) as usize;
        strip_trailing_zeros(&format!("{:.*}", prec, ax))
    } else {
        let m = strip_trailing_zeros(&mant);
        format!("{m}e{}{:02}", if e < 0 { "-" } else { "+" }, e.abs())
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

fn strip_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str, p: &str, o: &str) -> IriTriple {
        (s.into(), p.into(), o.into())
    }

    #[test]
    fn g6_matches_printf_semantics() {
        assert_eq!(format_g6(100.0), "100");
        assert_eq!(format_g6(0.0), "0");
        assert_eq!(format_g6(0.5), "0.5");
        assert_eq!(format_g6(45.6789), "45.6789");
        // 6 significant digits, rounded.
        assert_eq!(format_g6(12.3456789), "12.3457");
        assert_eq!(format_g6(1.0 / 3.0), "0.333333");
    }

    #[test]
    fn counts_and_ic_are_well_defined_with_reflexive_edges() {
        let sc = iri::RDFS_SUBCLASS_OF;
        // 3 classes, A ⊑ B ⊑ C plus reflexive edges (as the redundant graph has).
        let rdf = vec![
            t("A", sc, "A"),
            t("B", sc, "B"),
            t("C", sc, "C"),
            t("A", sc, "B"),
            t("A", sc, "C"),
            t("B", sc, "C"),
        ];
        let c = compute(&rdf);
        assert_eq!(c.total, 3);
        // C is referenced by A, B and itself = 3; A only by itself = 1.
        assert_eq!(c.reference["C"], 3);
        assert_eq!(c.reference["A"], 1);

        let lines = information_content(&rdf);
        // A references itself once → ic = -ln(1/3)*100/ln(3) = 100 (the max).
        assert!(lines.iter().any(|l| l.starts_with("<A>")
            && l.contains(iri::NORMALIZED_IC)
            && l.contains("\"100\"")));
        // referenceCount(C) = 3 emitted as integer.
        assert!(lines.iter().any(|l| l.contains("<C>")
            && l.contains(iri::REFERENCE_COUNT)
            && l.contains("\"3\"^^")));
    }
}
