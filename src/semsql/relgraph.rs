//! The relation graph: every entailed edge between named terms, as a TSV of
//! CURIEs.
//!
//! One row per edge, `subject<TAB>predicate<TAB>object`:
//!
//! * `C P D` for every entailed `C ⊑ (P some D)` — the existential restriction
//!   flattened to a direct edge, which is what makes the graph queryable
//!   without pattern-matching an `owl:Restriction`. The closure is *redundant*:
//!   an edge is recorded under every superproperty of the relation and against
//!   every superclass of the filler, so a query for `part_of some cell` finds a
//!   term related to a *kind* of cell by a *sub*-relation of `part_of`.
//! * `C rdfs:subClassOf D` for every entailed subsumption, equivalence included
//!   (as reciprocal edges) and reflexively, so every satisfiable class is its
//!   own subclass.
//! * `I rdf:type C` and `I P D` for named individuals.
//!
//! The edges come from the same EL reasoner the release products are classified
//! with, so property ranges, reflexive properties and property chains all count:
//! a `rdfs:range` on a relation puts the range class on the far end of every
//! edge that relation carries.
//!
//! Rows are written in sorted order, so the file — and the database built from
//! it — is the same bytes for the same input.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use whelk::whelk::model::{ConceptData, ConceptId, RoleId};
use whelk::whelk::reasoner::ReasonerState;

const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const OWL_TOP_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#topObjectProperty";
/// Roles the reasoner mints for a property chain. They are not relations of the
/// ontology, so no edge is recorded under them.
const COMPOSITION_ROLE: &str = "urn:whelk:composition_role:";
/// The OBO namespace, whose terms are written with their own prefix.
const OBO_NS: &str = "http://purl.obolibrary.org/obo/";

/// CURIE-shorten an IRI: the longest matching base in the prefix map wins;
/// failing that, an OBO term is split at its first `_` into prefix and local
/// part; failing that the IRI is written whole, so nothing is silently dropped.
fn curie(ranked: &[(String, String)], iri: &str) -> String {
    for (p, base) in ranked {
        if !base.is_empty() && iri.starts_with(base.as_str()) {
            return format!("{p}:{}", &iri[base.len()..]);
        }
    }
    if let Some(rest) = iri.strip_prefix(OBO_NS) {
        return rest.replacen('_', ":", 1);
    }
    iri.to_string()
}

/// The prefix map the graph is written in: the always-available bindings, then
/// the ontology's own, longest namespace first. `obo` is deliberately left out —
/// an OBO term is written with its own prefix (`CL:0000000`), not as
/// `obo:CL_0000000`.
fn prefix_map(prefixes: &[(String, String)]) -> Vec<(String, String)> {
    let standard: [(&str, &str); 5] = [
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("dc", "http://purl.org/dc/elements/1.1/"),
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ];
    let mut map: Vec<(String, String)> =
        standard.iter().map(|(p, n)| (p.to_string(), n.to_string())).collect();
    for (p, ns) in prefixes.iter().filter(|(p, _)| p != "obo" && p != "prefix") {
        match map.iter_mut().find(|(q, _)| q == p) {
            Some(slot) => slot.1 = ns.clone(),
            None => map.push((p.clone(), ns.clone())),
        }
    }
    map.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    map
}

/// A concept's IRI when it is a named class that can stand at either end of an
/// edge — not ⊤, not ⊥, not an anonymous expression.
fn named_class<'a>(state: &'a ReasonerState, id: ConceptId) -> Option<&'a str> {
    match state.interner.concept_data(id) {
        ConceptData::AtomicConcept(iri) if iri != OWL_THING && iri != OWL_NOTHING => Some(iri),
        _ => None,
    }
}

/// A concept's individual IRI, when it is a nominal.
fn individual<'a>(state: &'a ReasonerState, id: ConceptId) -> Option<&'a str> {
    match state.interner.concept_data(id) {
        ConceptData::Nominal(i) => Some(state.interner.individual_name(*i)),
        _ => None,
    }
}

/// Compute the relation graph of `min_owl` and write it to `out`.
/// The property chains, as `composed role → the (first, second) pairs that
/// compose to it`.
fn chains_by_composed(
    state: &ReasonerState,
) -> std::collections::HashMap<
    RoleId,
    Vec<(RoleId, RoleId)>,
> {
    let mut out: std::collections::HashMap<
        RoleId,
        Vec<(RoleId, RoleId)>,
    > = Default::default();
    for (&first, seconds) in state.role_compositions() {
        for (&second, composed) in seconds {
            for &c in composed.iter() {
                out.entry(c).or_default().push((first, second));
            }
        }
    }
    out
}

/// What a link's target really satisfies, beyond the target itself.
///
/// A link records the filler its axiom named. Where the role declares a range,
/// the filler is conjoined with it, so an edge ends at every class of THAT
/// conjunction — the range class among them, and anything else the conjunction
/// is subsumed by.
///
/// A link under a role a property chain composes to is derived from a pair of
/// links and takes the SECOND one's target, so it satisfies whatever that link's
/// target satisfied. Which pair produced it is not written on the link, but it is
/// there to be found: the link `C --s--> T` came through `r1 ∘ r2 → s` exactly
/// when some `P` has `C --r1--> P` and `P --r2--> T`, and then the narrowing is
/// the one that link carried. Chains of chains follow by the same step.
fn narrowed_targets(
    state: &ReasonerState,
    chains: &std::collections::HashMap<
        RoleId,
        Vec<(RoleId, RoleId)>,
    >,
    subject: ConceptId,
    role: RoleId,
    target: ConceptId,
    seen: &mut std::collections::HashSet<(ConceptId, RoleId, ConceptId)>,
    out: &mut Vec<ConceptId>,
) {
    if !seen.insert((subject, role, target)) {
        return;
    }
    if let Some(range) = state.role_range(role) {
        if let Some(c) =
            state.interner.find_concept(&ConceptData::RoleTarget { range, concept: target })
        {
            out.push(c);
        }
    }
    let Some(pairs) = chains.get(&role) else { return };
    let Some(by_role) = state.links_by_subject().get(&subject) else { return };
    for &(first, second) in pairs {
        let Some(middles) = by_role.get(&first) else { continue };
        for &middle in middles {
            let reaches = state
                .links_by_subject()
                .get(&middle)
                .and_then(|m| m.get(&second))
                .is_some_and(|ts| ts.contains(&target));
            if reaches {
                narrowed_targets(state, chains, middle, second, target, seen, out);
            }
        }
    }
}

pub fn write_tsv(min_owl: &Path, prefixes: &[(String, String)], out: &Path) -> Result<()> {
    let ranked = prefix_map(prefixes);
    let model = crate::io::load(min_owl)?;
    let translated = whelk::whelk::owl::translate_ontology(&model.ont);
    let state = whelk::whelk::reasoner::assert(&translated);

    let mut edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    let subclass = curie(&ranked, RDFS_SUBCLASS_OF);
    let rdf_type = curie(&ranked, RDF_TYPE);

    // --- Subsumption and type edges -----------------------------------------
    // Every satisfiable named class carries an edge from each of its entailed
    // subclasses, from itself, and from each individual it holds.
    for (&sup, subs) in &state.closure_subs_by_superclass {
        let Some(sup_iri) = named_class(&state, sup) else { continue };
        // An unsatisfiable class subsumes everything; its edges would say so.
        if state.is_subclass_of(sup, state.interner.bottom()) {
            continue;
        }
        let object = curie(&ranked, sup_iri);
        edges.insert((object.clone(), subclass.clone(), object.clone()));
        for &sub in subs {
            if let Some(iri) = named_class(&state, sub) {
                edges.insert((curie(&ranked, iri), subclass.clone(), object.clone()));
            } else if let Some(iri) = individual(&state, sub) {
                edges.insert((curie(&ranked, iri), rdf_type.clone(), object.clone()));
            }
        }
    }

    // --- Relation edges ------------------------------------------------------
    // A link `C --r--> T` says `C ⊑ r some T`, so it entails `C ⊑ R some F` for
    // every superproperty `R` of `r` and every superclass `F` of `T`. Both
    // closures are recorded, which is what makes the graph redundant.
    let empty: whelk::whelk::model::HashSet<ConceptId> = Default::default();
    let chains = chains_by_composed(&state);
    for (&subject, by_role) in state.links_by_subject() {
        let subject_iri = match named_class(&state, subject).or_else(|| individual(&state, subject))
        {
            Some(iri) => curie(&ranked, iri),
            None => continue,
        };
        for (&role, targets) in by_role {
            let mut roles: Vec<&str> = vec![state.interner.role_name(role)];
            if let Some(supers) = state.super_roles(role) {
                roles.extend(supers.iter().map(|&r| state.interner.role_name(r)));
            }
            roles.retain(|r| !r.starts_with(COMPOSITION_ROLE) && *r != OWL_TOP_OBJECT_PROPERTY);
            if roles.is_empty() {
                continue;
            }
            let roles: Vec<String> = roles.iter().map(|r| curie(&ranked, r)).collect();
            // A link records the filler the axiom named. Where the relation
            // declares a range, what its targets really satisfy is that filler
            // narrowed by the range, so the edge ends at every class of the
            // narrowed concept — the range class among them.
            for &target in targets {
                let mut ends: Vec<ConceptId> = vec![target];
                let mut visited = std::collections::HashSet::new();
                narrowed_targets(&state, &chains, subject, role, target, &mut visited, &mut ends);
                for c in ends {
                    let fillers = state.closure_subs_by_subclass.get(&c).unwrap_or(&empty);
                    for &filler in fillers {
                        let Some(iri) = named_class(&state, filler) else { continue };
                        let object = curie(&ranked, iri);
                        for r in &roles {
                            edges.insert((subject_iri.clone(), r.clone(), object.clone()));
                        }
                    }
                }
            }
        }
    }

    let mut text = String::new();
    for (s, p, o) in &edges {
        text.push_str(s);
        text.push('\t');
        text.push_str(p);
        text.push('\t');
        text.push_str(o);
        text.push('\n');
    }
    std::fs::write(out, text).with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}
