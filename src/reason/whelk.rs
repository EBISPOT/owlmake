//! Adapter for the [`whelk`](https://github.com/EBISPOT/whelk-rs) crate — the
//! `--reasoner whelk` backend.
//!
//! whelk-rs is a native Rust OWL 2 EL reasoner. It classifies straight off
//! horned-owl's [`SetOntology`](horned_owl::ontology::set::SetOntology), so we
//! hand it [`Model::ont`] directly and read the saturated subsumption closure
//! back out via
//! [`named_subsumptions`](whelk::whelk::reasoner::ReasonerState::named_subsumptions).
//!
//! The closure whelk exposes is the *full* transitive subsumption relation
//! (every `C ⊑ D` it entails, including reflexive `C ⊑ C`, `C ⊑ owl:Thing`, and
//! `C ⊑ owl:Nothing` for unsatisfiable classes). To present the same results as
//! the built-in EL [`Reasoner`](super::el::Reasoner) — and so feed `reason`'s
//! transitive-reduction/assertion step interchangeably — we re-derive the direct
//! subsumptions, satisfiability, and consistency here using the same rules
//! [`super::el::Reasoner`] applies to its own S-sets.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use whelk::whelk::model::{ConceptData, ConceptId};
use whelk::whelk::reasoner::ReasonerState;

use crate::model::Model;
use crate::reason::whelk_order;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
/// The namespace of the probe concepts [`WhelkClassification::unsatisfiable_properties`]
/// adds; no ontology names a class in it.
const PROBE_NS: &str = "urn:owlmake:probe#";

/// A whelk classification, shaped like the built-in EL reasoner's outputs.
pub struct WhelkClassification {
    /// For each named class, the set of its named superclasses in the saturated
    /// closure (includes `owl:Thing`, and `owl:Nothing` when unsatisfiable).
    subs: HashMap<String, HashSet<String>>,
    /// The named classes that appear as a subclass (the closure's domain).
    classes: Vec<String>,
    /// The saturated state. Kept because collapsing an equivalence clique to a
    /// single representative needs the order the reasoner visits a class's
    /// subsumers in, which is a property of the subsumer set as the reasoner
    /// holds it — see [`whelk_order`].
    state: ReasonerState,
    /// The named classes that are equivalent to some other named class. Only a
    /// class with two of these among its superclasses can need a clique
    /// collapsed, and the visit order is reconstructed only for those.
    in_clique: HashSet<String>,
    /// Concept hashes, shared across the classes whose order gets reconstructed.
    hashes: RefCell<HashMap<ConceptId, i32>>,
    /// Whether an axiom names `owl:Thing`. Only then is it among its own
    /// subclasses as the reasoner visits them.
    top_named: bool,
}

impl WhelkClassification {
    /// Translate `model` into whelk's normal form, saturate, and capture the
    /// named-subsumption closure.
    pub fn classify(model: &Model) -> WhelkClassification {
        let translated = whelk::whelk::owl::translate_ontology(crate::reason::owl_axioms(model).as_ref());
        let top = translated.interner.top();
        let top_named = translated.concept_inclusions.iter().any(|ci| {
            translated.interner.concept_signature(ci.subclass).contains(&top)
                || translated.interner.concept_signature(ci.superclass).contains(&top)
        });
        let state = whelk::whelk::reasoner::assert(&translated);

        let mut subs: HashMap<String, HashSet<String>> = HashMap::new();
        for (sub, sup) in state.named_subsumptions() {
            subs.entry(sub.to_string())
                .or_default()
                .insert(sup.to_string());
        }
        let mut classes: Vec<String> = subs.keys().cloned().collect();
        classes.sort();

        let mut in_clique: HashSet<String> = HashSet::new();
        for (c, sups) in &subs {
            for d in sups {
                if d != c && subs.get(d).is_some_and(|s| s.contains(c)) {
                    in_clique.insert(c.clone());
                    break;
                }
            }
        }

        WhelkClassification {
            subs,
            classes,
            state,
            in_clique,
            hashes: RefCell::new(HashMap::new()),
            top_named,
        }
    }

    /// Where each named subsumer of `c` falls in the order the reasoner visits
    /// them in, as a rank per IRI. The order is over the whole subsumer set —
    /// the anonymous concepts included, since they shape the hash trie the
    /// order comes from — and then narrowed to the named ones.
    fn visit_rank(&self, c: &str) -> HashMap<String, usize> {
        let interner = &self.state.interner;
        let mut rank: HashMap<String, usize> = HashMap::new();
        let Some(id) = interner.find_concept(&ConceptData::AtomicConcept(c.to_string())) else {
            return rank;
        };
        let mut ids: Vec<ConceptId> = self
            .state
            .closure_subs_by_subclass
            .get(&id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let top = interner.top();
        if !ids.contains(&top) {
            ids.push(top);
        }
        let mut hashes = self.hashes.borrow_mut();
        for (i, cid) in whelk_order::visit_order(interner, &ids, &mut hashes)
            .into_iter()
            .enumerate()
        {
            if let ConceptData::AtomicConcept(name) = interner.concept_data(cid) {
                rank.insert(name.clone(), i);
            }
        }
        rank
    }

    /// Whether `a ⊑ b` holds in the saturated closure.
    fn sub_of(&self, a: &str, b: &str) -> bool {
        self.subs.get(a).is_some_and(|s| s.contains(b))
    }

    /// Whether the closure records `sub ⊑ sup`. A class the closure does not
    /// hold, one named only as a superclass, is below nothing.
    pub fn subsumes(&self, sub: &str, sup: &str) -> bool {
        self.sub_of(sub, sup)
    }

    /// The classes of the top node: `owl:Thing` first, then the classes it is
    /// a subclass of, sorted.
    pub fn top_node(&self) -> Vec<String> {
        let mut rest: Vec<String> = self
            .subs
            .get(OWL_THING)
            .into_iter()
            .flatten()
            .filter(|c| c.as_str() != OWL_THING)
            .cloned()
            .collect();
        rest.sort();
        let mut out = vec![OWL_THING.to_string()];
        out.extend(rest);
        out
    }

    /// The classes directly below `c`, one class for each node. The reasoner
    /// folds over `c`'s subclasses and `owl:Nothing` in the order it visits
    /// them in. A subclass `c` is a subclass of in turn is equivalent to it and
    /// none of them. Any other is kept unless one already kept lies above it,
    /// and each kept one that lies below it is dropped. So of each node the
    /// class met first stands for it, and `owl:Nothing` stays only while
    /// nothing else is kept.
    ///
    /// Five or more subclasses are visited in a hash trie's order. Four or
    /// fewer are visited in the order they were added in, `owl:Nothing` first;
    /// the order the rest were added in is not recorded here, and the trie's
    /// stands in for it.
    pub fn direct_subclasses(&self, c: &str) -> Vec<String> {
        let interner = &self.state.interner;
        let bottom = interner.bottom();
        let top = interner.top();
        let concept = interner.find_concept(&ConceptData::AtomicConcept(c.to_string()));
        let mut ids: Vec<ConceptId> = concept
            .and_then(|id| self.state.closure_subs_by_superclass.get(&id))
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        if concept == Some(top) && !self.top_named {
            ids.retain(|&id| id != top);
        }
        if !ids.contains(&bottom) {
            ids.push(bottom);
        }
        let order = {
            let mut hashes = self.hashes.borrow_mut();
            if ids.len() <= 4 {
                let rest: Vec<ConceptId> = ids.iter().copied().filter(|&id| id != bottom).collect();
                std::iter::once(bottom).chain(whelk_order::visit_order(interner, &rest, &mut hashes)).collect()
            } else {
                whelk_order::visit_order(interner, &ids, &mut hashes)
            }
        };
        // Whether `a ⊑ b` in the closure.
        let below = |a: ConceptId, b: ConceptId| {
            self.state.closure_subs_by_superclass.get(&b).is_some_and(|s| s.contains(&a))
        };
        let mut direct: Vec<ConceptId> = Vec::new();
        for s in order {
            if !matches!(interner.concept_data(s), ConceptData::AtomicConcept(_)) || Some(s) == concept {
                continue;
            }
            if concept == Some(bottom) || concept.is_some_and(|id| below(id, s)) {
                continue;
            }
            let mut dropped: Vec<ConceptId> = Vec::new();
            let mut covered = false;
            for &other in &direct {
                if s == bottom || below(s, other) {
                    covered = true;
                    break;
                }
                if other == bottom || below(other, s) {
                    dropped.push(other);
                }
            }
            direct.retain(|d| !dropped.contains(d));
            if !covered {
                direct.push(s);
            }
        }
        direct
            .into_iter()
            .filter_map(|id| match interner.concept_data(id) {
                ConceptData::AtomicConcept(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether the ontology is consistent: no individual, which must exist, is
    /// unsatisfiable. `owl:Thing ⊑ owl:Nothing` alone leaves it consistent,
    /// with every class unsatisfiable.
    pub fn is_consistent(&self) -> bool {
        let bottom = self.state.interner.bottom();
        !self.state.closure_subs_by_superclass.get(&bottom).is_some_and(|subs| {
            subs.iter().any(|&c| matches!(self.state.interner.concept_data(c), ConceptData::Nominal(_)))
        })
    }

    /// The object properties among `properties` for which `∃p.⊤` is
    /// unsatisfiable, sorted. One probe concept `P ⊑ ∃p.⊤` per property is
    /// asserted onto the saturated state and saturated in turn; the state is
    /// persistent, so extending a copy of it costs only the probes.
    pub fn unsatisfiable_properties(&self, properties: &[String]) -> Vec<String> {
        use whelk::whelk::model::ConceptInclusion;
        let mut state = self.state.clone();
        let top = state.interner.top();
        let bottom = state.interner.bottom();
        let mut axioms: whelk::whelk::model::HashSet<ConceptInclusion> = Default::default();
        let mut probes: Vec<(ConceptId, &String)> = Vec::new();
        for (i, p) in properties.iter().enumerate() {
            let role = state.interner.intern_role(p);
            let some = state.interner.intern_concept(ConceptData::ExistentialRestriction { role, concept: top });
            let probe = state.interner.intern_concept(ConceptData::AtomicConcept(format!("{PROBE_NS}{i}")));
            axioms.insert(ConceptInclusion { subclass: probe, superclass: some });
            probes.push((probe, p));
        }
        let saturated = whelk::whelk::reasoner::assert_append(&axioms, &state);
        let mut out: Vec<String> = probes
            .into_iter()
            .filter(|&(probe, _)| saturated.is_subclass_of(probe, bottom))
            .map(|(_, p)| p.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// IRIs of named classes that are unsatisfiable (entail `owl:Nothing`).
    pub fn unsatisfiable(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .subs
            .iter()
            .filter(|(c, sups)| c.as_str() != OWL_NOTHING && sups.contains(OWL_NOTHING))
            .map(|(c, _)| c.clone())
            .collect();
        out.sort();
        out
    }

    /// The **full** named-subsumption closure (every entailed `a ⊑ b` with
    /// `a ≠ b`, excluding ⊤/⊥), matching [`super::el::Reasoner::all_subsumptions`].
    ///
    /// This reads `self.subs` directly rather than deriving from
    /// [`Self::direct_subsumptions`]: that list is a transitive reduction, and it
    /// also *drops* equivalence-clique siblings (the `!equiv` filter below), so it
    /// is a strict subset of the closure and cannot stand in for it. This set is
    /// what `--include-indirect` asserts, and it has to be the same set whichever
    /// EL backend classified.
    pub fn all_subsumptions(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for c in &self.classes {
            if c == OWL_THING || c == OWL_NOTHING {
                continue;
            }
            let Some(sups) = self.subs.get(c) else {
                continue;
            };
            for d in sups {
                if d == c || d == OWL_THING || d == OWL_NOTHING {
                    continue;
                }
                out.push((c.clone(), d.clone()));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The inferred equivalent-class pairs (`c ≡ d`, `c < d` by IRI, excluding
    /// ⊤/⊥) — the mutual-subsumption pairs of the closure. Mirrors
    /// [`super::el::Reasoner::equivalent_class_pairs`], so the equivalence policy
    /// behaves identically whichever EL backend classified.
    pub fn equivalent_class_pairs(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for c in &self.classes {
            if c == OWL_THING || c == OWL_NOTHING {
                continue;
            }
            let Some(sups) = self.subs.get(c) else {
                continue;
            };
            for d in sups {
                if d == c || d == OWL_THING || d == OWL_NOTHING {
                    continue;
                }
                if c < d && self.sub_of(d, c) {
                    out.push((c.clone(), d.clone()));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The transitive reduction of strict subsumption between satisfiable named
    /// classes — the direct `SubClassOf` edges.
    ///
    /// Where two of a class's superclasses are equivalent to each other, one of
    /// them stands for the clique and the rest are dropped: the representative
    /// is the one the reasoner reaches first when it folds over the class's
    /// subsumers, i.e. the earliest in [`whelk_order::visit_order`].
    pub fn direct_subsumptions(&self) -> Vec<(String, String)> {
        let satisfiable = |c: &str| !self.sub_of(c, OWL_NOTHING);
        let equiv = |a: &str, b: &str| a != b && self.sub_of(a, b) && self.sub_of(b, a);

        let mut out: Vec<(String, String)> = Vec::new();
        for c in &self.classes {
            let c = c.as_str();
            if c == OWL_THING || c == OWL_NOTHING || !satisfiable(c) {
                continue;
            }
            let Some(sups) = self.subs.get(c) else {
                continue;
            };
            // Named, satisfiable, proper supers that are not equivalent to `c`
            // (clique siblings are related by an equivalence, not a subsumption
            // edge).
            let supers: Vec<&str> = sups
                .iter()
                .map(String::as_str)
                .filter(|&d| {
                    d != c && d != OWL_THING && d != OWL_NOTHING && satisfiable(d) && !equiv(c, d)
                })
                .collect();
            // A clique among the supers collapses to whichever member the
            // subsumer walk reaches first, so the order is only needed when two
            // of the supers are equivalent to something.
            let rank = (supers.iter().filter(|d| self.in_clique.contains(**d)).count() > 1)
                .then(|| self.visit_rank(c));
            for &d in &supers {
                if let Some(rank) = &rank {
                    let at = |x: &str| rank.get(x).copied().unwrap_or(usize::MAX);
                    if supers.iter().any(|&e| equiv(d, e) && at(e) < at(d)) {
                        continue;
                    }
                }
                // `d` is non-direct if some other super `mid` lies strictly
                // between `c` and `d` (`mid ⊑ d`, not `d ⊑ mid`, and `mid` is a
                // proper intermediate above `c`, i.e. not equivalent to `c`).
                let redundant = supers.iter().any(|&mid| {
                    mid != d
                        && self.sub_of(mid, d)
                        && !self.sub_of(d, mid)
                        && !self.sub_of(mid, c)
                });
                if !redundant {
                    out.push((c.to_string(), d.to_string()));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}
