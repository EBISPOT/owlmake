//! Ubergraph.
//!
//! Builds the Ubergraph products: a merged, reasoned RDF graph of the OBO
//! ontologies, plus the *materialized existential-relation graph* (its defining
//! feature) in two forms — a **redundant** closure and a **non-redundant**
//! transitive reduction — each loaded into a named graph.
//!
//! Each product and the code that computes it:
//!
//! | product | computed by |
//! |---|---|
//! | `ontologies-merged.{ofn,ttl}` | `cmd::{merge,remove,unmerge}` + EL [`Reasoner`](crate::reason::Reasoner): merge, remove disjointness, remove `owl:Nothing`, unmerge, reason |
//! | `properties-redundant.nt` | [`edges::redundant_graph`] (the EL closure as direct triples) |
//! | `properties-nonredundant.nt` | [`prune`] |
//! | `information-content.nt` | [`ic`] |
//! | `is_defined_by.nt` | [`is_defined_by`] |
//! | `ubergraph.nq` (named-graph N-Quads) + biolink categories | [`assemble`] (oxigraph) |
//!
//! The dataset is emitted as portable N-Quads (`ubergraph.nq`), which carry the
//! complete graph content; no binary database image is written.

pub mod assemble;
pub mod edges;
pub mod ic;
pub mod prune;

/// A triple whose subject, predicate and object are all IRIs — the shape of
/// every edge in the property/subclass graphs ubergraph materializes.
pub type IriTriple = (String, String, String);

/// Named-graph IRIs for the Ubergraph products.
///
/// A graph IRI is a provenance claim, so a locally built graph is stamped with
/// owlmake's own namespace by default rather than with the namespace of a
/// published service, which would assert that service produced it. The base is
/// configurable via `om ubergraph --graph-prefix` (set it to
/// `http://reasoner.renci.org` for the graph names the published Ubergraph
/// dataset carries); the constants below are the default.
pub mod graph {
    /// Default base IRI for the ubergraph named graphs. Override with
    /// `om ubergraph --graph-prefix <IRI>`.
    pub const DEFAULT_PREFIX: &str = "https://www.ebi.ac.uk/spot/owlmake/ubergraph";
    pub const ONTOLOGY: &str = "https://www.ebi.ac.uk/spot/owlmake/ubergraph/ontology";
    pub const REDUNDANT: &str = "https://www.ebi.ac.uk/spot/owlmake/ubergraph/redundant";
    pub const NONREDUNDANT: &str = "https://www.ebi.ac.uk/spot/owlmake/ubergraph/nonredundant";
    pub const BIOLINK: &str = "https://biolink.github.io/biolink-model/";

    /// The three core named graphs resolved from a configurable `--graph-prefix`.
    /// (`BIOLINK` is the biolink model's own namespace and is not configurable.)
    pub struct Names {
        pub ontology: String,
        pub redundant: String,
        pub nonredundant: String,
    }

    impl Names {
        /// Resolve `<prefix>/ontology`, `<prefix>/redundant` and
        /// `<prefix>/nonredundant` from a base prefix (a trailing `/` is ignored).
        pub fn from_prefix(prefix: &str) -> Self {
            let p = prefix.trim_end_matches('/');
            Names {
                ontology: format!("{p}/ontology"),
                redundant: format!("{p}/redundant"),
                nonredundant: format!("{p}/nonredundant"),
            }
        }
    }
}

/// Well-known predicate/term IRIs used across the pipeline.
pub mod iri {
    pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    pub const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
    pub const RDFS_SUBPROPERTY_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
    pub const RDFS_IS_DEFINED_BY: &str = "http://www.w3.org/2000/01/rdf-schema#isDefinedBy";
    pub const OWL_TRANSITIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";
    pub const REFERENCE_COUNT: &str = "https://www.ebi.ac.uk/spot/owlmake/vocab/referenceCount";
    pub const NORMALIZED_IC: &str =
        "https://www.ebi.ac.uk/spot/owlmake/vocab/normalizedInformationContent";
    pub const NORMALIZED_SUBCLASS_IC: &str =
        "https://www.ebi.ac.uk/spot/owlmake/vocab/normalizedSubClassInformationContent";
    pub const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
    pub const OBO_PREFIX: &str = "http://purl.obolibrary.org/obo/";
}

/// Serialize IRI triples as sorted N-Triples (`<s> <p> <o> .`). Sorting makes
/// the output deterministic, so a diff between two builds shows real content
/// changes and never a reshuffle.
pub fn iri_triples_to_ntriples(triples: &[IriTriple]) -> String {
    let mut lines: Vec<String> = triples
        .iter()
        .map(|(s, p, o)| format!("<{s}> <{p}> <{o}> ."))
        .collect();
    lines.sort_unstable();
    lines.dedup();
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// `is_defined_by.nt`: for every OBO IRI appearing as a subject, assert
/// `iri rdfs:isDefinedBy obo:<ontology>.owl`, where `<ontology>` is the
/// lower-cased ID with its final `_<local>` stripped (`CL_0000000` → `cl`,
/// `GO_0008150` → `go`). The mapping is computed directly over the subject set,
/// so no SPARQL pass over the dataset is needed.
pub fn is_defined_by(subjects: impl IntoIterator<Item = String>) -> Vec<IriTriple> {
    use std::collections::BTreeSet;
    let mut out: BTreeSet<IriTriple> = BTreeSet::new();
    for s in subjects {
        if let Some(rest) = s.strip_prefix(iri::OBO_PREFIX) {
            // Take the part after the OBO prefix, drop the trailing "_[^_]+",
            // lower-case what is left.
            let trimmed = match rest.rfind('_') {
                Some(i) => &rest[..i],
                None => rest,
            };
            if trimmed.is_empty() {
                continue;
            }
            let ont = trimmed.to_ascii_lowercase();
            let iri = format!("{}{ont}.owl", iri::OBO_PREFIX);
            out.insert((s.clone(), iri::RDFS_IS_DEFINED_BY.to_string(), iri));
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_defined_by_strips_local_id_and_lowercases() {
        let subs = vec![
            "http://purl.obolibrary.org/obo/CL_0000000".to_string(),
            "http://purl.obolibrary.org/obo/GO_0008150".to_string(),
            "http://example.org/not_obo".to_string(),
        ];
        let got = is_defined_by(subs);
        assert!(got.contains(&(
            "http://purl.obolibrary.org/obo/CL_0000000".into(),
            iri::RDFS_IS_DEFINED_BY.into(),
            "http://purl.obolibrary.org/obo/cl.owl".into()
        )));
        assert!(got.contains(&(
            "http://purl.obolibrary.org/obo/GO_0008150".into(),
            iri::RDFS_IS_DEFINED_BY.into(),
            "http://purl.obolibrary.org/obo/go.owl".into()
        )));
        // non-OBO subjects are skipped
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn ntriples_are_sorted_and_deduped() {
        let t = vec![
            ("http://b".into(), "http://p".into(), "http://o".into()),
            ("http://a".into(), "http://p".into(), "http://o".into()),
            ("http://a".into(), "http://p".into(), "http://o".into()),
        ];
        let nt = iri_triples_to_ntriples(&t);
        assert_eq!(
            nt,
            "<http://a> <http://p> <http://o> .\n<http://b> <http://p> <http://o> .\n"
        );
    }
}
