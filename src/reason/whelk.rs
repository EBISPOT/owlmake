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
}

impl WhelkClassification {
    /// Translate `model` into whelk's normal form, saturate, and capture the
    /// named-subsumption closure.
    pub fn classify(model: &Model) -> WhelkClassification {
        let translated = whelk::whelk::owl::translate_ontology(&model.ont);
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

    /// Whether the ontology is consistent: inconsistency surfaces as `owl:Thing`
    /// becoming unsatisfiable (`owl:Thing ⊑ owl:Nothing`).
    pub fn is_consistent(&self) -> bool {
        !self.sub_of(OWL_THING, OWL_NOTHING)
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
