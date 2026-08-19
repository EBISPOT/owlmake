//! Non-redundant graph — the reduction of the redundant graph to the edges that
//! carry information no other edge already implies.
//!
//! Given the redundant graph (`rdf`) plus the ontology's transitive properties
//! and (transitively closed) sub-property relation, an edge `(s, p, o)` is
//! *redundant* when any of these hold:
//!
//! 1. a strictly-more-specific object exists: `rdf(s,p,other)`, `s≠other`,
//!    `¬equiv(s,other)`, `subClassOf(other,o)`;
//! 2. a super-class of `s` already has the edge: `subClassOf(s,other)`,
//!    `rdf(other,p,o)`, `other≠o`, `¬equiv(other,o)`;
//! 3. transitivity short-circuits it: `transitive(p)`, `rdf(s,p,other)`,
//!    `other≠o`, `rdf(other,p,o)`;
//! 4. a sub-property already states it: `subPropertyOf(sub,p)`, `rdf(s,sub,o)`;
//! 5. it is the reflexive `s rdfs:subClassOf s`.
//!
//! Here `equivalent` and `subClassOf` are derived *from the redundant graph's
//! own `rdfs:subClassOf` triples*: that graph already carries the full
//! subsumption closure, so the reduction needs no further reasoning. The
//! non-redundant graph is `rdf` minus every redundant edge.

use std::collections::{HashMap, HashSet};

use crate::ubergraph::{iri, IriTriple};

/// Compute the non-redundant graph from the redundant graph.
///
/// * `transitive` — IRIs of transitive object properties, taken from the merged
///   ontology's `TransitiveObjectProperty` axioms.
/// * `subproperty_closure` — `(sub, super)` pairs of `rdfs:subPropertyOf`,
///   already transitively closed and reflexive-free.
pub fn nonredundant(
    rdf: &[IriTriple],
    transitive: &HashSet<String>,
    subproperty_closure: &[(String, String)],
) -> Vec<IriTriple> {
    let sc = iri::RDFS_SUBCLASS_OF;

    // Triple membership + indexes.
    let triples: HashSet<&IriTriple> = rdf.iter().collect();
    let mut by_sp: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
    for (s, p, o) in rdf {
        by_sp.entry((s, p)).or_default().push(o);
    }

    // Derived equivalence and subClassOf, from the redundant graph's subclass
    // triples: x and y are equivalent when both `x ⊑ y` and `y ⊑ x` are present
    // and x != y.
    let mut subclass_pairs: HashSet<(&str, &str)> = HashSet::new();
    for (s, p, o) in rdf {
        if p == sc {
            subclass_pairs.insert((s, o));
        }
    }
    let equiv = |x: &str, y: &str| -> bool {
        x != y && subclass_pairs.contains(&(x, y)) && subclass_pairs.contains(&(y, x))
    };
    // Derived subClassOf: (sub,super) with sub!=super and not equivalent.
    // sub_of[o] = subclasses of o; sup_of[s] = superclasses of s.
    let mut sub_of: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut sup_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for &(s, o) in &subclass_pairs {
        if s != o && !equiv(s, o) {
            sub_of.entry(o).or_default().push(s);
            sup_of.entry(s).or_default().push(o);
        }
    }

    // sub-properties of a given property p (sub strictly below p).
    let mut subprops_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for (sub, sup) in subproperty_closure {
        subprops_of
            .entry(sup.as_str())
            .or_default()
            .push(sub.as_str());
    }

    let empty: Vec<&str> = Vec::new();
    let is_redundant = |s: &str, p: &str, o: &str| -> bool {
        // Rule 5: reflexive subClassOf.
        if p == sc && s == o {
            return true;
        }
        // Rule 1: a more-specific object `other` (other ⊑ o) is also reached.
        for &other in by_sp.get(&(s, p)).unwrap_or(&empty) {
            if other != s && !equiv(s, other) && sub_of.get(o).map_or(false, |v| v.contains(&other))
            {
                // subClassOf(other, o) holds (other in subclasses of o).
                return true;
            }
        }
        // Rule 2: a super-class `other` of s already has edge (other,p,o).
        for &other in sup_of.get(s).unwrap_or(&empty) {
            if other != o
                && !equiv(other, o)
                && triples.contains(&(other.to_string(), p.to_string(), o.to_string()))
            {
                return true;
            }
        }
        // Rule 3: transitive short-circuit via some intermediate `other`.
        if transitive.contains(p) {
            for &other in by_sp.get(&(s, p)).unwrap_or(&empty) {
                if other != o
                    && triples.contains(&(other.to_string(), p.to_string(), o.to_string()))
                {
                    return true;
                }
            }
        }
        // Rule 4: a sub-property already states (s, sub, o).
        for &sub in subprops_of.get(p).unwrap_or(&empty) {
            if triples.contains(&(s.to_string(), sub.to_string(), o.to_string())) {
                return true;
            }
        }
        false
    };

    let mut out: Vec<IriTriple> = rdf
        .iter()
        .filter(|(s, p, o)| !is_redundant(s, p, o))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str, p: &str, o: &str) -> IriTriple {
        (s.into(), p.into(), o.into())
    }
    const SC: &str = iri::RDFS_SUBCLASS_OF;

    #[test]
    fn reflexive_subclass_is_pruned() {
        let rdf = vec![t("A", SC, "A"), t("A", SC, "B")];
        let nr = nonredundant(&rdf, &HashSet::new(), &[]);
        assert!(!nr.contains(&t("A", SC, "A")));
        assert!(nr.contains(&t("A", SC, "B")));
    }

    #[test]
    fn transitive_subclass_closure_is_reduced() {
        // A ⊑ B ⊑ C, with the redundant A ⊑ C present.
        let rdf = vec![
            t("A", SC, "B"),
            t("B", SC, "C"),
            t("A", SC, "C"),
            t("A", SC, "A"),
            t("B", SC, "B"),
            t("C", SC, "C"),
        ];
        let nr = nonredundant(&rdf, &HashSet::new(), &[]);
        // A ⊑ C is redundant (rule 1: A ⊑ B, B ⊑ C) — dropped.
        assert!(!nr.contains(&t("A", SC, "C")));
        assert!(nr.contains(&t("A", SC, "B")));
        assert!(nr.contains(&t("B", SC, "C")));
    }

    #[test]
    fn inherited_property_edge_is_pruned() {
        // B ⊑ A; both A and B have `part_of X`. B's is redundant (rule 2).
        let p = "http://example.org/part_of";
        let rdf = vec![
            t("B", SC, "A"),
            t("A", p, "X"),
            t("B", p, "X"),
        ];
        let nr = nonredundant(&rdf, &HashSet::new(), &[]);
        assert!(nr.contains(&t("A", p, "X")));
        assert!(!nr.contains(&t("B", p, "X")));
    }

    #[test]
    fn more_specific_object_prunes_general_edge() {
        // A part_of Y, A part_of Z, Z ⊑ Y. The edge to Y is redundant (rule 1).
        let p = "http://example.org/part_of";
        let rdf = vec![t("A", p, "Y"), t("A", p, "Z"), t("Z", SC, "Y")];
        let nr = nonredundant(&rdf, &HashSet::new(), &[]);
        assert!(nr.contains(&t("A", p, "Z")));
        assert!(!nr.contains(&t("A", p, "Y")));
    }

    #[test]
    fn subproperty_edge_prunes_superproperty_edge() {
        // s sub o and s sup o, with sub ⊑ sup. The sup edge is redundant (rule 4).
        let sub = "http://example.org/sub";
        let sup = "http://example.org/sup";
        let rdf = vec![t("S", sub, "O"), t("S", sup, "O")];
        let nr = nonredundant(&rdf, &HashSet::new(), &[(sub.into(), sup.into())]);
        assert!(nr.contains(&t("S", sub, "O")));
        assert!(!nr.contains(&t("S", sup, "O")));
    }
}
