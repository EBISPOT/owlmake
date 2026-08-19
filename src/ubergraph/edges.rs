//! The "redundant" graph: the full entailed closure of existential relations
//! and subclass edges, emitted as direct RDF triples
//! (`subjectClass predicateIRI objectClass`) rather than OWL restrictions.
//!
//! Collapsing each entailed existential restriction to a single direct triple
//! is what makes the graph queryable with plain SPARQL — a consumer follows
//! `part_of` as an edge instead of pattern-matching an `owl:Restriction`. The
//! edges are read straight off owlmake's EL
//! [`Reasoner`](crate::reason::Reasoner), which has already computed the
//! closure; `owl:Thing` and `owl:Nothing` are never edge endpoints.
//!
//! Emitted triples:
//! - `C <property> D` for every entailed `C ⊑ (property some D)` (redundant,
//!   i.e. *all* named `D`, not only the most-specific).
//! - `C rdfs:subClassOf D` for every entailed subsumption, **including** the
//!   reflexive `C rdfs:subClassOf C` for every satisfiable named class.
//!   [`crate::ubergraph::prune`] drops those reflexive edges again when it
//!   reduces the graph, and [`crate::ubergraph::ic`] depends on them: every
//!   class references itself, so no reference count is zero and the logarithms
//!   stay defined. Equivalent classes appear as subsumptions in both
//!   directions.

use std::collections::HashSet;

use crate::reason::Reasoner;
use crate::ubergraph::{iri, IriTriple};

/// Build the redundant graph (property edges + subclass closure) from a
/// classified reasoner. `props` restricts the object properties materialized
/// over; an empty set materializes over all of them.
pub fn redundant_graph(reasoner: &Reasoner, props: &HashSet<String>) -> Vec<IriTriple> {
    let mut out: Vec<IriTriple> = Vec::new();

    // Existential relation edges (full closure).
    for (c, r, d) in reasoner.materialize_all(props) {
        out.push((c, r, d));
    }

    // Subclass closure: every entailed strict subsumption (both directions for
    // equivalents, since `all_subsumptions` returns each entailed pair).
    for (sub, sup) in reasoner.all_subsumptions() {
        out.push((sub, iri::RDFS_SUBCLASS_OF.to_string(), sup));
    }

    // Reflexive self-edges for every satisfiable named class.
    for c in reasoner.satisfiable_named_classes() {
        out.push((c.clone(), iri::RDFS_SUBCLASS_OF.to_string(), c));
    }

    out.sort();
    out.dedup();
    out
}
