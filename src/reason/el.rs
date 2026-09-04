//! An OWL 2 EL reasoner using the completion-rule algorithm of Baader, Brandt
//! and Lutz ("Pushing the EL Envelope", IJCAI 2005), refined with the
//! composed-vs-decomposed subsumer split and per-context partitioning that keep
//! it tractable on ontologies the size of MONDO and phenio.
//!
//! The pipeline is:
//!   1. Intern classes and object properties to integer ids.
//!   2. Structurally normalize all supported TBox/RBox axioms into EL normal
//!      form, introducing fresh concept names where necessary.
//!   3. Saturate the completion sets S(C) (subsumers) and R(r) (role links)
//!      with an agenda-driven application of rules CR1–CR7.
//!   4. Read the classification off S, and detect unsatisfiable classes /
//!      inconsistency via the bottom concept ⊥.
//!
//! Axioms outside OWL 2 EL (unions, complements, cardinalities, universals)
//! are ignored and reported via [`Reasoner::ignored`].

use std::collections::BTreeSet;

// Fast non-cryptographic hashing for the integer-keyed completion structures —
// a large win over SipHash in the saturation hot loops.
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use horned_owl::model::{
    AnnotatedComponent, ClassExpression as CE, Component, Individual, ObjectPropertyExpression as OPE, RcStr,
    SubObjectPropertyExpression as SOPE,
};

use crate::model::Model;

/// Concept-name id. `TOP` and `BOT` are reserved.
pub type CId = u32;
/// Object-property (role) id.
pub type RId = u32;

pub const TOP: CId = 0;
pub const BOT: CId = 1;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const OWL_BOTTOM_OP: &str = "http://www.w3.org/2002/07/owl#bottomObjectProperty";
const OWL_TOP_OP: &str = "http://www.w3.org/2002/07/owl#topObjectProperty";

/// EL normal-form general concept inclusions.
#[derive(Debug, Clone, Copy)]
enum Nf {
    /// A ⊑ B
    Sub(CId, CId),
    /// A1 ⊓ A2 ⊑ B
    And(CId, CId, CId),
    /// A ⊑ ∃r.B
    SomeSup(CId, RId, CId),
    /// ∃r.A ⊑ B
    SomeSub(RId, CId, CId),
    /// `C ⊑ A` where C is a conjunction and A one of its conjuncts (the
    /// *decomposition* of C). Unlike `Sub`, this is applied only in C's own
    /// context — because everywhere else C is *composed* (added by the And rule
    /// only when its conjuncts are already subsumers), so decomposing it back to
    /// those conjuncts would be pure redundant work. This is the composed-vs-
    /// decomposed subsumer split.
    Decomp(CId, CId),
}

/// Axiom indexes, immutable during saturation.
#[derive(Default)]
struct Axioms {
    /// A ⊑ B, indexed by A.
    sub: HashMap<CId, Vec<CId>>,
    /// Conjunction decomposition C ⊑ conjunct, indexed by C. Applied only when C
    /// is added to its OWN context (the composed-vs-decomposed split).
    decomp: HashMap<CId, Vec<CId>>,
    /// A1 ⊓ A2 ⊑ B: stored under both conjuncts as (other, sup).
    conj: HashMap<CId, Vec<(CId, CId)>>,
    /// A ⊑ ∃r.B, indexed by A.
    some_sup: HashMap<CId, Vec<(RId, CId)>>,
    /// ∃r.A ⊑ B, indexed by (r, A).
    some_sub: HashMap<(RId, CId), Vec<CId>>,
    /// Immediate super-roles: r ⊑ s.
    role_sub: HashMap<RId, Vec<RId>>,
    /// Role chains r1 ∘ r2 ⊑ r3, indexed by first role r1.
    chain_by_first: HashMap<RId, Vec<(RId, RId)>>,
    /// Role chains r1 ∘ r2 ⊑ r3, indexed by second role r2.
    chain_by_second: HashMap<RId, Vec<(RId, RId)>>,
    /// Roles that occur as the SECOND role of some chain (the keys of
    /// `chain_by_second`). Forward links are only ever read by the chain rule for
    /// these roles, so the parallel engine only stores/sends `Fwd` for them —
    /// skipping forward links for every other role (the vast majority, including
    /// reflexive/top self-loops), a large memory + message saving.
    chain_second_roles: HashSet<RId>,
    /// Roles that appear as the role of some `∃r.A ⊑ B` axiom. CR4 can only
    /// fire on links of these roles, so links of other roles skip the
    /// expensive scan of the successor's subsumer set.
    some_sub_roles: HashSet<RId>,
    /// For each such role r, the distinct fillers A appearing in `∃r.A ⊑ B`.
    /// CR4 on a link (x,y) can iterate whichever is smaller — this set or
    /// S(y) — rather than always scanning (and cloning) the whole of S(y).
    some_sub_fillers: HashMap<RId, Vec<CId>>,
    /// The set of *all* fillers A appearing in any `∃r.A ⊑ B` axiom. When a
    /// subsumer `d` enters S(x), CR4's predecessor scan can only do useful work
    /// if `d` is one of these — which is rarely the case (most subsumers are
    /// ordinary named ancestors). Gating the scan on membership here skips
    /// billions of no-op predecessor iterations on large ontologies.
    filler_concepts: HashSet<CId>,
    /// Self-restriction concept → role: when this concept enters `S(X)`, the
    /// `r`-self-loop `R(X,X)` holds (local reflexivity, `X ⊑ ∃r.Self`).
    self_role: HashMap<CId, RId>,
    /// Role → self-restriction concept: when an `r`-self-loop `R(X,X)` exists,
    /// `X` is an instance of `∃r.Self`.
    role_self: HashMap<RId, CId>,
    /// Disjunction concept → its disjuncts (for union elimination).
    union_members: HashMap<CId, Vec<CId>>,
    /// Disjunct → the union concepts it is a member of.
    member_unions: HashMap<CId, Vec<CId>>,
    /// Whether any `∃r.Self` / union concepts exist at all. Most ontologies have
    /// neither, so these flags let the saturation hot loop skip the per-subsumer
    /// self/union map lookups entirely (each otherwise a hash probe that misses).
    has_self: bool,
    has_unions: bool,
    /// Whether any `∃r.A ⊑ B` axiom exists (CR4). When none do, the per-link CR4
    /// scan is skipped wholesale.
    some_sub_any: bool,
}

/// Mutable saturation state.
struct State {
    /// S(C): the set of concept names subsuming C, indexed by C.
    s: Vec<HashSet<CId>>,
    /// (r, X) -> {Y : (X,Y) ∈ R(r)}. This set is also the link dedupe — a
    /// separate `(r,x,y)` hash set is redundant (it stored every link a third
    /// time), so `add_link` checks/uses this directly.
    r_succ: HashMap<(RId, CId), HashSet<CId>>,
    /// Y -> {(r, X) : (X,Y) ∈ R(r)} — predecessors of Y across all roles.
    r_pred: HashMap<CId, Vec<(RId, CId)>>,
    /// Union concept u -> classes `Y` known to satisfy `Y ⊑ u` (so a derived
    /// `u ⊑ C` can be propagated as `Y ⊑ C`).
    union_subs: HashMap<CId, Vec<CId>>,
    /// Pending work.
    agenda: Vec<Work>,
    /// Reusable scratch buffer for snapshotting a subsumer set during CR4,
    /// avoiding a fresh allocation per link. Held here so its capacity persists
    /// across rule firings; always `mem::take`-n out before use and put back.
    scratch: Vec<CId>,
}

enum Work {
    /// D was newly added to S(X).
    Sub(CId, CId),
    /// D (a conjunction) was newly added to S(X) by the And rule — i.e. *composed*
    /// from conjuncts already in S(X). Such an addition must NOT be decomposed
    /// back to those conjuncts (they are present), unlike a `Sub` arrival via a
    /// told/injected edge. This is the composed-vs-decomposed subsumer split.
    SubC(CId, CId),
    /// (X,Y) was newly added to R(r).
    Link(RId, CId, CId),
}

/// A fully classified EL ontology.
pub struct Reasoner {
    /// IRI string for each concept name id (fresh names are anonymous).
    class_iri: Vec<Option<String>>,
    iri_to_cid: HashMap<String, CId>,
    /// IRI string for each role id (aux roles have synthetic names).
    role_iri: Vec<String>,
    state: State,
    /// Number of input axioms ignored as outside OWL 2 EL.
    ignored: usize,
    /// Ids that correspond to real named classes (not fresh/auxiliary).
    named: Vec<CId>,
    /// Ids that stand for asserted individuals (nominals). An unsatisfiable
    /// individual makes the whole ontology inconsistent.
    individuals: Vec<CId>,
}

/// Enable/disable the union-elimination completion rule for the current thread.
/// The rule is sound but lies outside the plain EL completion calculus, so the
/// default `--reasoner elk` — what CL, UBERON and MONDO release under unless a
/// build asks for another reasoner — leaves it OFF, keeping those taxonomies as
/// published; `--reasoner owlmake` turns it ON. Set before `Reasoner::classify`;
/// persists for subsequent same-thread classifications (e.g. a pipeline's
/// `reduce` after `reason`).
pub fn set_whelk_mode(on: bool) {
    WHELK_MODE.with(|m| m.set(on));
}

impl Reasoner {
    /// Classify the TBox + RBox of `model`.
    pub fn classify(model: &Model) -> Reasoner {
        // Arm the memory safety valve before any large structure is built, so the
        // reasoner can never drive the whole machine into the OOM-killer.
        let _mem_guard = spawn_mem_watchdog();
        let t0 = crate::time::Instant::now();
        let timing = std::env::var_os("OWLMAKE_TIMING").is_some();
        let b = Self::normalize(model, t0, timing);
        b.finish(timing, t0)
    }

    /// Like [`Reasoner::classify`] but takes *ownership* of the model and frees it
    /// **before** saturating. Normalization is the only phase that reads the
    /// model; saturation works purely on the interned integer indexes. Dropping
    /// the parsed model (≈12 GB on phenio) before the ~60 s saturation cuts peak
    /// RSS by that much. Use only when the caller no longer needs the model
    /// (reasoning-only / fresh-output modes).
    pub fn classify_consume(model: Model) -> Reasoner {
        let _mem_guard = spawn_mem_watchdog();
        let t0 = crate::time::Instant::now();
        let timing = std::env::var_os("OWLMAKE_TIMING").is_some();
        // `normalize` returns a fully-owned `Builder` (interned ids + normal
        // forms), borrowing the model only through the local `comps` Vec which is
        // dropped on return — so the model can be released here, before saturating.
        let b = Self::normalize(&model, t0, timing);
        if timing {
            status!("el: freeing parsed model before saturation (RSS {} MB)", vmrss_mb());
        }
        drop(model);
        if timing {
            status!("el: parsed model freed (RSS now {} MB)", vmrss_mb());
        }
        b.finish(timing, t0)
    }

    /// Build the normalized [`Builder`] (interning + RBox/TBox normal forms) from
    /// a model. Shared by [`Reasoner::classify`] and [`Reasoner::classify_consume`];
    /// the returned `Builder` is fully owned and holds no reference to `model`.
    fn normalize(model: &Model, t0: crate::time::Instant, timing: bool) -> Builder {
        let mut b = Builder::new();
        // Capture the elk-vs-owlmake mode once: it decides whether an axiom with
        // a non-EL sub-expression is dropped whole (elk) or has its EL part
        // salvaged (owlmake).
        b.whelk = WHELK_MODE.with(|m| m.get());
        b.intern_class(OWL_THING); // -> TOP (0)
        b.intern_class(OWL_NOTHING); // -> BOT (1)
        debug_assert_eq!(b.iri_to_cid[OWL_THING], TOP);
        debug_assert_eq!(b.iri_to_cid[OWL_NOTHING], BOT);
        // Intern the special object properties eagerly so the RBox-phase
        // computation of bottom/top roles sees them even when they only occur
        // inside TBox class expressions.
        b.intern_role(OWL_TOP_OP);
        b.intern_role(OWL_BOTTOM_OP);

        // `SetOntology` iterates in `HashSet` order, which varies run-to-run.
        // Only the non-confluent WHELK union-elimination rule depends on axiom
        // order for *reproducibility*; plain EL is confluent, so its output is
        // identical regardless of order (the parallel engine already processes in
        // nondeterministic order). Sorting millions of components structurally is
        // very slow (≈90 s on phenio), so do it ONLY in WHELK mode.
        let mut comps: Vec<&AnnotatedComponent<RcStr>> = model.ont.iter().collect();
        if WHELK_MODE.with(|m| m.get()) {
            comps.sort();
        }

        // Declare all named classes so they are classified even if they appear
        // in no axiom.
        for ac in &comps {
            if let Component::DeclareClass(dc) = &ac.component {
                b.intern_class(dc.0.0.as_ref());
            }
        }

        // Pass 1: role box. Pass 2: class box (after effective ranges known).
        // A single "normalize" bar spans both passes (TBox is the bulk) so the
        // gap between the parse bar and the saturate heartbeat isn't blank.
        let mut bar = crate::progress::Progress::new("normalize", (comps.len() as u64) * 2);
        for (i, ac) in comps.iter().enumerate() {
            b.process_rbox(&ac.component);
            if i % 50_000 == 0 {
                bar.set(i as u64);
            }
        }
        b.compute_effective_ranges();
        let base = comps.len() as u64;
        for (i, ac) in comps.iter().enumerate() {
            b.process_tbox(&ac.component);
            if i % 50_000 == 0 {
                bar.set(base + i as u64);
            }
        }
        bar.finish(base * 2);
        if timing {
            status!(
                "el: normalize {:.1}s  ({} classes, {} roles, {} normal forms)",
                t0.elapsed().as_secs_f64(),
                b.class_iri.len(),
                b.role_iri.len(),
                b.nfs.len()
            );
        }
        b
    }

    /// IRIs of named classes that are unsatisfiable (entail ⊥).
    pub fn unsatisfiable(&self) -> Vec<String> {
        let mut out = Vec::new();
        for &c in &self.named {
            if c != BOT && self.state.s[c as usize].contains(&BOT) {
                if let Some(iri) = &self.class_iri[c as usize] {
                    out.push(iri.clone());
                }
            }
        }
        out.sort();
        out
    }

    /// Whether the ontology is consistent. Inconsistency surfaces either as ⊤
    /// being unsatisfiable, or as an asserted individual (which must exist)
    /// being unsatisfiable.
    pub fn is_consistent(&self) -> bool {
        if self.state.s[TOP as usize].contains(&BOT) {
            return false;
        }
        !self
            .individuals
            .iter()
            .any(|&i| self.state.s[i as usize].contains(&BOT))
    }

    /// Number of ignored (non-EL) axioms.
    pub fn ignored(&self) -> usize {
        self.ignored
    }

    /// Materialize the *full* (redundant) existential closure: for each named
    /// class C and object property R in `props`, return **every** named D such
    /// that `C ⊑ (R some D)` is entailed — not only the most-specific ones.
    /// This is the un-pruned closure the "redundant" graph carries. If `props`
    /// is empty, all properties are considered. Reflexive property self-edges
    /// (`C R C`) are omitted; the redundant graph records reflexivity only as
    /// `C rdfs:subClassOf C` (see [`Reasoner::satisfiable_named_classes`]).
    pub fn materialize_all(
        &self,
        props: &std::collections::HashSet<String>,
    ) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        let named_class = |c: CId| {
            c != TOP && c != BOT && self.class_iri[c as usize].is_some() && self.named.contains(&c)
        };
        for ((r, x), ys) in &self.state.r_succ {
            let r_iri = match self.role_iri.get(*r as usize) {
                Some(iri)
                    if !iri.starts_with("__owlmake_aux_role_")
                        && iri != "http://www.w3.org/2002/07/owl#topObjectProperty" =>
                {
                    iri
                }
                _ => continue,
            };
            if !props.is_empty() && !props.contains(r_iri) {
                continue;
            }
            if !named_class(*x) {
                continue;
            }
            for &y in ys {
                for &d in &self.state.s[y as usize] {
                    if !named_class(d) || d == *x {
                        continue;
                    }
                    out.push((
                        self.class_iri[*x as usize].clone().unwrap(),
                        r_iri.clone(),
                        self.class_iri[d as usize].clone().unwrap(),
                    ));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Every satisfiable named class IRI (excluding ⊤/⊥). Used to emit reflexive
    /// `C rdfs:subClassOf C` edges in the "redundant" graph.
    pub fn satisfiable_named_classes(&self) -> Vec<String> {
        let mut out = Vec::new();
        for &c in &self.named {
            if c == TOP || c == BOT || self.state.s[c as usize].contains(&BOT) {
                continue;
            }
            if let Some(iri) = &self.class_iri[c as usize] {
                out.push(iri.clone());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Is `sub` ⊑ `sup` entailed (both given as IRIs)?
    pub fn is_subsumed(&self, sub: &str, sup: &str) -> bool {
        match (self.iri_to_cid.get(sub), self.iri_to_cid.get(sup)) {
            (Some(&a), Some(&b)) => self.state.s[a as usize].contains(&b),
            _ => false,
        }
    }

    /// Inferred *direct* class assertions: for every asserted individual (treated
    /// as a singleton nominal), the most-specific named classes it is entailed to
    /// be an instance of — its direct types. Returns (individual_iri, class_iri)
    /// pairs.
    pub fn class_assertions(&self) -> Vec<(String, String)> {
        let named: HashSet<CId> = self.named.iter().copied().collect();
        let mut out = Vec::new();
        for &i in &self.individuals {
            let ind_iri = match &self.class_iri[i as usize] {
                Some(s) => s.clone(),
                None => continue,
            };
            // Named-class subsumers of the nominal (its inferred types), excluding
            // ⊤/⊥ and itself.
            let types: Vec<CId> = self.state.s[i as usize]
                .iter()
                .copied()
                .filter(|&d| d != TOP && d != BOT && d != i && named.contains(&d))
                .collect();
            // Keep only the most-specific (direct) types: drop D if some other type
            // E is a *strict* subclass of D (E ⊑ D and not D ⊑ E). The strictness
            // guard is essential — without it two *equivalent* direct types C ≡ D
            // each dominate the other and both would be dropped, leaving the
            // individual with no type. Equivalent direct types are all emitted,
            // so every member of a direct-type equivalence clique is asserted
            // rather than one representative standing in for the rest.
            for &d in &types {
                let dominated = types.iter().any(|&e| {
                    e != d
                        && self.state.s[e as usize].contains(&d)
                        && !self.state.s[d as usize].contains(&e)
                });
                if !dominated {
                    if let Some(di) = &self.class_iri[d as usize] {
                        out.push((ind_iri.clone(), di.clone()));
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// All entailed subsumptions `sub ⊑ sup` between distinct named classes,
    /// excluding ⊤/⊥ and tautologies. Returns (sub_iri, sup_iri) pairs.
    pub fn all_subsumptions(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for &c in &self.named {
            if c == TOP || c == BOT {
                continue;
            }
            let ci = match &self.class_iri[c as usize] {
                Some(i) => i,
                None => continue,
            };
            for &d in &self.state.s[c as usize] {
                if d == c || d == TOP || d == BOT {
                    continue;
                }
                if let Some(di) = &self.class_iri[d as usize] {
                    out.push((ci.clone(), di.clone()));
                }
            }
        }
        out.sort();
        out
    }

    /// The inferred *equivalent-class* pairs — named `c ≡ d` with `c < d` by IRI,
    /// excluding ⊤/⊥. Reads the saturated S-sets directly rather than filtering
    /// [`Reasoner::all_subsumptions`]: the full closure is O(n·ancestors) entries
    /// (gigabytes on phenio), and CL, OBA and UBERON all reason with
    /// `--equivalent-classes-allowed asserted-only`, so every one of their
    /// release builds asks for this — it must not put them on the full-closure
    /// path. Here the scan is O(n·|S(c)|) with no Vec built.
    pub fn equivalent_class_pairs(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for &c in &self.named {
            if c == TOP || c == BOT {
                continue;
            }
            let ci = match &self.class_iri[c as usize] {
                Some(i) => i,
                None => continue,
            };
            for &d in &self.state.s[c as usize] {
                if d == c || d == TOP || d == BOT {
                    continue;
                }
                let Some(di) = &self.class_iri[d as usize] else {
                    continue;
                };
                // Emit each clique pair once, ordered by IRI (`c < d`), and only
                // when the converse subsumption also holds.
                if ci < di && self.state.s[d as usize].contains(&c) {
                    out.push((ci.clone(), di.clone()));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The direct (immediate) named superclasses of each named class — the
    /// transitive reduction of the inferred subsumption hierarchy. Returns
    /// (sub_iri, direct_sup_iri) pairs. This is what `reason` asserts.
    pub fn direct_subsumptions(&self) -> Vec<(String, String)> {
        // Transitive reduction is O(n · supers²); on a large ontology that is the
        // dominant post-saturation cost, so fan it out over the named classes
        // (every class is independent and all reads are immutable).
        let t0 = crate::time::Instant::now();
        let timing = std::env::var("OWLMAKE_TIMING").is_ok();
        let named: &[CId] = &self.named;
        let nworkers = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(1)
            .min(named.len().max(1));
        let chunk = named.len().div_ceil(nworkers).max(1);
        // Single worker: run inline rather than spawning. Besides skipping the
        // thread overhead on a one-core host, this is the path the wasm build
        // takes — `std::thread` has no runtime support there (spawn traps), and
        // `available_parallelism` reports 1, so `nworkers` is always 1 on wasm.
        let mut out: Vec<(String, String)> = if nworkers <= 1 {
            self.direct_subsumptions_chunk(named)
        } else {
            std::thread::scope(|scope| {
                let handles: Vec<_> = named
                    .chunks(chunk)
                    .map(|ck| scope.spawn(move || self.direct_subsumptions_chunk(ck)))
                    .collect();
                handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
            })
        };
        out.sort();
        out.dedup();
        if timing {
            status!("el: direct-subsumptions {:.1}s ({} edges)", t0.elapsed().as_secs_f64(), out.len());
        }
        out
    }

    /// Transitive reduction for one slice of named classes (run per worker).
    fn direct_subsumptions_chunk(&self, classes: &[CId]) -> Vec<(String, String)> {
        let satisfiable = |c: CId| !self.state.s[c as usize].contains(&BOT);
        let named_sup = |c: CId, d: CId| {
            d != c
                && d != TOP
                && d != BOT
                && self.class_iri[d as usize].is_some()
                && satisfiable(d)
        };
        // `d` is a direct super of `c` unless some other super `mid` lies strictly
        // between (`mid ⊑ d` and not `d ⊑ mid`).
        let mut out = Vec::new();
        for &c in classes {
            if c == TOP || c == BOT || !satisfiable(c) || self.class_iri[c as usize].is_none() {
                continue;
            }
            // A super `d` that is equivalent to `c` is a clique sibling of `c` —
            // their relationship is the `EquivalentClasses` axiom, not a
            // subsumption edge — so it is not a direct super and is filtered out
            // of `supers` here.
            let equiv = |a: CId, b: CId| {
                a != b && self.state.s[a as usize].contains(&b) && self.state.s[b as usize].contains(&a)
            };
            let supers: Vec<CId> = self.state.s[c as usize]
                .iter()
                .copied()
                .filter(|&d| named_sup(c, d) && !equiv(c, d))
                .collect();
            for &d in &supers {
                // Emit an edge to EVERY member of a direct-super equivalence
                // clique, not just one representative: `c ⊑ D` is asserted for
                // every `D` in the direct-superclass clique, so e.g. a protein
                // `PR_x ⊑ PR_000000001` (≡ CHEBI_36080) yields edges to *both*
                // PR_000000001 and CHEBI_36080. The clique members are kept
                // distinct here because the redundancy test below only hides `d`
                // behind a *strict* intermediate (`!s[d] ⊇ mid`), never behind a
                // mutually-equivalent sibling.
                let redundant = supers.iter().any(|&mid| {
                    mid != d
                        && self.state.s[mid as usize].contains(&d)
                        && !self.state.s[d as usize].contains(&mid)
                        // `mid` must be a *proper* intermediate, i.e. strictly
                        // above `c`. If `mid ≡ c` (a subsumption cycle / equivalent
                        // class), it is the same node as `c`, not an intermediate,
                        // and must not hide `c`'s real direct supers.
                        && !self.state.s[mid as usize].contains(&c)
                });
                if !redundant {
                    out.push((
                        self.class_iri[c as usize].clone().unwrap(),
                        self.class_iri[d as usize].clone().unwrap(),
                    ));
                }
            }
        }
        out
    }
}

/// A canonical class taxonomy — a classification reduced to a form two of them
/// can be compared for equality in, independent of how the equivalences and
/// edges were written down.
///
/// `equivalences` holds every non-trivial equivalence group: groups of two or
/// more mutually-subsuming named classes, plus the unsatisfiable group (which
/// includes `owl:Nothing`) and any classes equivalent to `owl:Thing` (which
/// includes `owl:Thing`). `edges` holds the transitive reduction of strict
/// subsumption between satisfiable, non-top groups, with each group identified
/// by the lexicographically smallest IRI of its members.
#[derive(Debug, PartialEq, Eq)]
pub struct ClassTaxonomy {
    pub equivalences: BTreeSet<BTreeSet<String>>,
    pub edges: BTreeSet<(String, String)>,
}

impl Reasoner {
    /// Compute the canonical class taxonomy (equivalence groups + direct edges).
    pub fn taxonomy(&self) -> ClassTaxonomy {
        // Named classes excluding the reserved TOP/BOT ids.
        let names: Vec<CId> = self
            .named
            .iter()
            .copied()
            .filter(|&c| c != TOP && c != BOT && self.class_iri[c as usize].is_some())
            .collect();
        let iri = |c: CId| self.class_iri[c as usize].clone().unwrap();

        // Inconsistent ontology: every class collapses with owl:Thing and
        // owl:Nothing into a single equivalence group, no edges.
        if !self.is_consistent() {
            let mut all: BTreeSet<String> = names.iter().copied().map(iri).collect();
            all.insert(OWL_THING.to_string());
            all.insert(OWL_NOTHING.to_string());
            let mut equivalences = BTreeSet::new();
            equivalences.insert(all);
            return ClassTaxonomy {
                equivalences,
                edges: BTreeSet::new(),
            };
        }
        let sat = |c: CId| !self.state.s[c as usize].contains(&BOT);
        let subsumed = |a: CId, b: CId| self.state.s[a as usize].contains(&b);

        // Partition satisfiable classes into equivalence groups.
        let sat_names: Vec<CId> = names.iter().copied().filter(|&c| sat(c)).collect();
        let mut group_of: HashMap<CId, usize> = HashMap::default();
        let mut groups: Vec<Vec<CId>> = Vec::new();
        for &c in &sat_names {
            let mut placed = false;
            for (gi, g) in groups.iter().enumerate() {
                let rep = g[0];
                if subsumed(c, rep) && subsumed(rep, c) {
                    group_of.insert(c, gi);
                    placed = true;
                    break;
                }
            }
            if !placed {
                group_of.insert(c, groups.len());
                groups.push(vec![c]);
            } else {
                let gi = group_of[&c];
                groups[gi].push(c);
            }
        }

        // Representative IRI of each group = min member IRI.
        let rep_iri = |g: &[CId]| g.iter().map(|&c| iri(c)).min().unwrap();

        // Determine the top group (classes equivalent to owl:Thing): a class A
        // with TOP ⊑ A.
        let top_members: BTreeSet<String> = sat_names
            .iter()
            .copied()
            .filter(|&c| subsumed(TOP, c))
            .map(iri)
            .collect();

        let mut equivalences: BTreeSet<BTreeSet<String>> = BTreeSet::new();

        // Non-trivial equivalence groups among satisfiable classes.
        for g in &groups {
            // Skip a group that is exactly the top group (handled below).
            let set: BTreeSet<String> = g.iter().map(|&c| iri(c)).collect();
            if g.len() >= 2 && !is_top_group(&set, &top_members) {
                equivalences.insert(set);
            }
        }
        if !top_members.is_empty() {
            let mut set = top_members.clone();
            set.insert(OWL_THING.to_string());
            equivalences.insert(set);
        }

        // Unsatisfiable group.
        let unsat: BTreeSet<String> = names
            .iter()
            .copied()
            .filter(|&c| !sat(c))
            .map(iri)
            .collect();
        if !unsat.is_empty() {
            let mut set = unsat;
            set.insert(OWL_NOTHING.to_string());
            equivalences.insert(set);
        }

        // Direct edges between satisfiable, non-top groups.
        // Exclude the top group from being a sub or, as an endpoint, omit it.
        let group_reps: Vec<(usize, String)> = groups
            .iter()
            .enumerate()
            .map(|(gi, g)| (gi, rep_iri(g)))
            .collect();
        let is_top = |gi: usize| {
            let set: BTreeSet<String> = groups[gi].iter().map(|&c| iri(c)).collect();
            is_top_group(&set, &top_members)
        };

        let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
        for (gi, grep) in &group_reps {
            if is_top(*gi) {
                continue;
            }
            // Super-groups: gj such that members of gi ⊑ members of gj, gi != gj.
            let sub_rep = groups[*gi][0];
            let supers: Vec<usize> = group_reps
                .iter()
                .filter(|(gj, _)| {
                    *gj != *gi && !is_top(*gj) && subsumed(sub_rep, groups[*gj][0])
                })
                .map(|(gj, _)| *gj)
                .collect();
            // Transitive reduction: keep gj that has no other super gk strictly
            // between gi and gj (gi ⊑ gk ⊑ gj, gk != gj, gk != gi).
            for &gj in &supers {
                let target = groups[gj][0];
                let redundant = supers.iter().any(|&gk| {
                    gk != gj && subsumed(groups[gk][0], target)
                });
                if !redundant {
                    edges.insert((grep.clone(), rep_iri(&groups[gj])));
                }
            }
        }

        ClassTaxonomy {
            equivalences: merge_overlapping_groups(equivalences),
            edges,
        }
    }
}

/// Merge equivalence groups that share any member into one, so that a taxonomy
/// represented as several pairwise `EquivalentClasses(owl:Nothing, X)` axioms
/// compares equal to one `EquivalentClasses(owl:Nothing, A, B, C)`.
pub fn merge_overlapping_groups(groups: BTreeSet<BTreeSet<String>>) -> BTreeSet<BTreeSet<String>> {
    let mut merged: Vec<BTreeSet<String>> = Vec::new();
    for g in groups {
        let mut hits: Vec<usize> = merged
            .iter()
            .enumerate()
            .filter(|(_, m)| m.iter().any(|x| g.contains(x)))
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            merged.push(g);
        } else {
            // Merge g and all hit groups into the first hit; remove the rest.
            hits.sort_unstable();
            let target = hits[0];
            merged[target].extend(g);
            for &h in hits[1..].iter().rev() {
                let taken = merged.remove(h);
                merged[target].extend(taken);
            }
        }
    }
    // A second pass in case merges created new overlaps.
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for i in 0..merged.len() {
            for j in (i + 1)..merged.len() {
                if merged[i].iter().any(|x| merged[j].contains(x)) {
                    let taken = merged.remove(j);
                    merged[i].extend(taken);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    merged.into_iter().collect()
}

fn is_top_group(set: &BTreeSet<String>, top_members: &BTreeSet<String>) -> bool {
    !top_members.is_empty() && set == top_members
}

/// Builds normalized axioms while interning entities, then saturates.
struct Builder {
    class_iri: Vec<Option<String>>,
    iri_to_cid: HashMap<String, CId>,
    role_iri: Vec<String>,
    role_to_rid: HashMap<String, RId>,
    nfs: Vec<Nf>,
    role_sub: Vec<(RId, RId)>,
    chains: Vec<(RId, RId, RId)>,
    /// Roles declared transitive (see `Reasoner::transitive`).
    transitive: HashSet<RId>,
    /// Self-restriction concepts: `(c, r)` where `c` is the opaque concept for
    /// `∃r.Self` (local reflexivity). Used to seed/detect `r`-self-loops.
    self_roles: Vec<(CId, RId)>,
    /// Disjunctions: `(u, [d1..dn])` where `u` is the concept for `d1 ⊔ … ⊔ dn`.
    /// Enables the union-elimination rule (`u ⊑ C` when every `di ⊑ C`).
    unions: Vec<(CId, Vec<CId>)>,
    /// Object-property ranges: range(r) = C.
    ranges: Vec<(RId, CId)>,
    /// Effective range of each role: union of the ranges of the role and all
    /// its super-roles (computed after the RBox pass).
    eff_range: HashMap<RId, Vec<CId>>,
    /// Roles that are sub-roles of owl:bottomObjectProperty (the empty role):
    /// any `∃r.C` over such a role is unsatisfiable.
    bottom_roles: HashSet<RId>,
    /// Roles that are owl:topObjectProperty (the universal role).
    top_roles: HashSet<RId>,
    /// Roles asserted reflexive.
    reflexive: Vec<RId>,
    /// Ids that name a genuine class (used in class position), as opposed to
    /// ids interned only to stand for an individual (a nominal).
    seen_as_class: HashSet<CId>,
    /// Ids that stand for individuals (nominals).
    individuals: HashSet<CId>,
    /// Hash-consing of complex class expressions to their concept-name id, so
    /// structurally identical sub-expressions share a defined name (this is
    /// what lets `A ≡ E` and `B ≡ E` entail `A ≡ B`).
    expr_memo: HashMap<String, CId>,
    ignored: usize,
    /// Whether the more-complete `owlmake` reasoner is in force, rather than the
    /// conservative `--reasoner elk` mode (the default). Captured from the
    /// thread-local [`WHELK_MODE`] at the start of [`Reasoner::classify`]. When
    /// `false` (elk mode) a class axiom containing any construct outside the
    /// indexable EL fragment (`ObjectAllValuesFrom`, cardinalities, data
    /// existentials/universals, inverse-in-class-expression) is dropped *whole*
    /// at the first such sub-expression, so the EL conjuncts of e.g.
    /// `A ⊑ B ⊓ ∀r.C` are not kept. The `owlmake` reasoner (`true`) instead
    /// salvages the EL part (`A ⊑ B`) — sound and strictly more complete.
    whelk: bool,
    /// Origin tag per concept id (parallel to `class_iri`), for OWLMAKE_TIMING
    /// memory diagnostics only: 0 named, 1 ∃some, 2 ⊓conj, 3 ⊔union, 4 ¬compl,
    /// 5 opaque(self/data).
    kind: Vec<u8>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            class_iri: Vec::new(),
            kind: Vec::new(),
            iri_to_cid: HashMap::default(),
            role_iri: Vec::new(),
            role_to_rid: HashMap::default(),
            nfs: Vec::new(),
            role_sub: Vec::new(),
            chains: Vec::new(),
            transitive: HashSet::default(),
            self_roles: Vec::new(),
            unions: Vec::new(),
            ranges: Vec::new(),
            eff_range: HashMap::default(),
            bottom_roles: HashSet::default(),
            top_roles: HashSet::default(),
            reflexive: Vec::new(),
            seen_as_class: HashSet::default(),
            individuals: HashSet::default(),
            expr_memo: HashMap::default(),
            ignored: 0,
            whelk: false,
        }
    }

    /// Intern an IRI to a concept-name id (no class/individual tagging).
    fn intern_entity(&mut self, iri: &str) -> CId {
        if let Some(&id) = self.iri_to_cid.get(iri) {
            return id;
        }
        let id = self.class_iri.len() as CId;
        self.class_iri.push(Some(iri.to_string()));
        self.kind.push(0);
        self.iri_to_cid.insert(iri.to_string(), id);
        id
    }

    /// Intern an IRI used as a genuine class.
    fn intern_class(&mut self, iri: &str) -> CId {
        let id = self.intern_entity(iri);
        self.seen_as_class.insert(id);
        id
    }

    /// A fresh, anonymous concept name used during normalization. `kind` is an
    /// origin tag for diagnostics (see [`Builder::kind`]).
    fn fresh_class(&mut self, kind: u8) -> CId {
        let id = self.class_iri.len() as CId;
        self.class_iri.push(None);
        self.kind.push(kind);
        id
    }

    fn intern_role(&mut self, iri: &str) -> RId {
        if let Some(&id) = self.role_to_rid.get(iri) {
            return id;
        }
        let id = self.role_iri.len() as RId;
        self.role_iri.push(iri.to_string());
        self.role_to_rid.insert(iri.to_string(), id);
        id
    }

    /// Resolve an object-property expression to a role id; inverse properties
    /// are not in EL and are rejected (caller treats as ignored).
    fn role_of(&mut self, ope: &OPE<RcStr>) -> Option<RId> {
        match ope {
            OPE::ObjectProperty(op) => Some(self.intern_role(op.0.as_ref())),
            OPE::InverseObjectProperty(_) => None,
        }
    }

    /// First pass: the role box (property hierarchy, chains, transitivity,
    /// reflexivity, ranges). Must run before the TBox pass so existential
    /// fillers can be augmented with effective ranges.
    fn process_rbox(&mut self, comp: &Component<RcStr>) {
        match comp {
            Component::EquivalentObjectProperties(ax) => {
                let ids: Vec<Option<RId>> = ax.0.iter().map(|o| self.role_of(o)).collect();
                for w in ids.windows(2) {
                    if let (Some(a), Some(b)) = (w[0], w[1]) {
                        self.role_sub.push((a, b));
                        self.role_sub.push((b, a));
                    }
                }
            }
            Component::SubObjectPropertyOf(ax) => match &ax.sub {
                SOPE::ObjectPropertyChain(chain) => {
                    let sup = match self.role_of(&ax.sup) {
                        Some(r) => r,
                        None => {
                            self.ignored += 1;
                            return;
                        }
                    };
                    let rs: Option<Vec<RId>> = chain.iter().map(|o| self.role_of(o)).collect();
                    match rs {
                        // r1 ∘ r2 ∘ ... ⊑ s — decompose left-associatively with
                        // fresh intermediate roles for chains longer than 2.
                        Some(rs) if rs.len() >= 2 => self.add_chain(&rs, sup),
                        Some(rs) if rs.len() == 1 => self.role_sub.push((rs[0], sup)),
                        _ => self.ignored += 1,
                    }
                }
                SOPE::ObjectPropertyExpression(ope) => {
                    match (self.role_of(ope), self.role_of(&ax.sup)) {
                        (Some(sub), Some(sup)) => self.role_sub.push((sub, sup)),
                        _ => self.ignored += 1,
                    }
                }
            },
            Component::TransitiveObjectProperty(ax) => match self.role_of(&ax.0) {
                Some(r) => {
                    self.chains.push((r, r, r));
                    self.transitive.insert(r);
                }
                None => self.ignored += 1,
            },
            Component::ReflexiveObjectProperty(ax) => match self.role_of(&ax.0) {
                Some(r) => self.reflexive.push(r),
                None => self.ignored += 1,
            },
            Component::ObjectPropertyRange(ax) => {
                // In elk mode a range whose class uses a non-EL construct is
                // dropped whole rather than having its EL part salvaged.
                if !self.whelk && elk_poison(&ax.ce) {
                    self.ignored += 1;
                    return;
                }
                // range(r) = C: every r-successor is a C.
                match (self.role_of(&ax.ope), self.flatten(&ax.ce)) {
                    (Some(r), Some(c)) => self.ranges.push((r, c)),
                    _ => self.ignored += 1,
                }
            }
            _ => {}
        }
    }

    /// Compute, for each role, the union of the ranges of itself and all its
    /// super-roles. Run between the RBox and TBox passes.
    fn compute_effective_ranges(&mut self) {
        let n = self.role_iri.len();
        let supers = transitive_role_closure(&self.role_sub, n);
        let mut ranges_of: HashMap<RId, Vec<CId>> = HashMap::default();
        for &(r, c) in &self.ranges {
            ranges_of.entry(r).or_default().push(c);
        }
        let mut eff: HashMap<RId, Vec<CId>> = HashMap::default();
        for r in 0..n as RId {
            let mut v = Vec::new();
            for &s in &supers[r as usize] {
                if let Some(cs) = ranges_of.get(&s) {
                    v.extend(cs);
                }
            }
            v.sort_unstable();
            v.dedup();
            if !v.is_empty() {
                eff.insert(r, v);
            }
        }
        self.eff_range = eff;

        // Roles that are sub-roles of owl:bottomObjectProperty are empty.
        if let Some(&bot_rid) = self.role_to_rid.get(OWL_BOTTOM_OP) {
            for r in 0..n as RId {
                if supers[r as usize].contains(&bot_rid) {
                    self.bottom_roles.insert(r);
                }
            }
        }
        // owl:topObjectProperty is the universal role: model it as reflexive
        // (so `C ⊑ ∃top.C`) plus a link from ⊤ to each individual (so a
        // non-empty filler forces `⊤ ⊑ ∃top.filler`). The (⊤, a) links are
        // seeded in `finish`.
        if let Some(&top_rid) = self.role_to_rid.get(OWL_TOP_OP) {
            self.top_roles.insert(top_rid);
            self.reflexive.push(top_rid);
        }
    }

    /// Second pass: the class box (subsumptions, equivalences, disjointness,
    /// domains).
    fn process_tbox(&mut self, comp: &Component<RcStr>) {
        // In elk mode, an axiom with any class expression outside the indexable
        // EL fragment is dropped WHOLE — its EL part is not kept, so e.g.
        // `A ⊑ B ⊓ ∀r.C` contributes nothing. The `owlmake` reasoner instead
        // falls through and salvages the EL conjuncts below (sound, more
        // complete).
        if !self.whelk && tbox_component_poison(comp) {
            self.ignored += 1;
            return;
        }
        match comp {
            Component::SubClassOf(ax) => self.add_gci(&ax.sub, &ax.sup),
            Component::EquivalentClasses(ax) => {
                let v = &ax.0;
                for w in v.windows(2) {
                    self.add_gci(&w[0], &w[1]);
                    self.add_gci(&w[1], &w[0]);
                }
            }
            Component::DisjointClasses(ax) => self.add_disjoint(&ax.0),
            Component::DisjointUnion(ax) => {
                // DisjointUnion(D; C1..Cn): the Ci are pairwise disjoint and each
                // Ci ⊑ D. (The D ⊑ C1 ⊔ ... ⊔ Cn direction is not EL and is
                // omitted; it is not needed for the EL-entailed subsumptions.)
                let d = self.intern_class(ax.0 .0.as_ref());
                self.add_disjoint(&ax.1);
                for member in &ax.1 {
                    if let Some(c) = self.flatten(member) {
                        self.nfs.push(Nf::Sub(c, d));
                    }
                }
            }
            Component::ObjectPropertyDomain(ax) => {
                // domain(r) = C  ⟺  ∃r.⊤ ⊑ C
                match (self.role_of(&ax.ope), self.flatten(&ax.ce)) {
                    (Some(r), Some(c)) => self.nfs.push(Nf::SomeSub(r, TOP, c)),
                    _ => self.ignored += 1,
                }
            }
            // ABox: individuals are treated as singleton nominal concepts.
            Component::ClassAssertion(ax) => {
                if let Some(n) = self.individual_concept(&ax.i) {
                    self.normalize_sup(n, &ax.ce);
                }
            }
            Component::ObjectPropertyAssertion(ax) => {
                if let (Some(r), Some(from), Some(to)) = (
                    self.role_of(&ax.ope),
                    self.individual_concept(&ax.from),
                    self.individual_concept(&ax.to),
                ) {
                    let f = self.augmented_filler(r, to);
                    self.nfs.push(Nf::SomeSup(from, r, f));
                }
            }
            Component::SameIndividual(ax) => {
                let ids: Vec<Option<CId>> =
                    ax.0.iter().map(|i| self.individual_concept(i)).collect();
                for w in ids.windows(2) {
                    if let (Some(a), Some(b)) = (w[0], w[1]) {
                        self.nfs.push(Nf::Sub(a, b));
                        self.nfs.push(Nf::Sub(b, a));
                    }
                }
            }
            Component::DifferentIndividuals(ax) => {
                let ids: Vec<Option<CId>> =
                    ax.0.iter().map(|i| self.individual_concept(i)).collect();
                for i in 0..ids.len() {
                    for j in (i + 1)..ids.len() {
                        if let (Some(a), Some(b)) = (ids[i], ids[j]) {
                            self.nfs.push(Nf::And(a, b, BOT));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Pairwise disjointness C_i ⊓ C_j ⊑ ⊥ over a list of class expressions.
    fn add_disjoint(&mut self, members: &[CE<RcStr>]) {
        let ids: Vec<Option<CId>> = members.iter().map(|c| self.flatten(c)).collect();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                if let (Some(a), Some(b)) = (ids[i], ids[j]) {
                    self.nfs.push(Nf::And(a, b, BOT));
                }
            }
        }
    }

    /// Add a general C ⊑ D, normalizing both sides. Complex left-hand
    /// expressions (unions, nominals, complements, …) are reduced to a single
    /// interned concept by [`Self::flatten`], which is sound for the EL
    /// fragment and preserves the structural sharing of identical
    /// sub-expressions.
    fn add_gci(&mut self, sub: &CE<RcStr>, sup: &CE<RcStr>) {
        let l = match self.flatten(sub) {
            Some(c) => c,
            None => {
                self.ignored += 1;
                return;
            }
        };
        self.normalize_sup(l, sup);
    }

    /// Emit normal forms for `lhs ⊑ rhs`, where `lhs` is already an atom.
    fn normalize_sup(&mut self, lhs: CId, rhs: &CE<RcStr>) {
        match rhs {
            CE::Class(c) => {
                let r = self.intern_class(c.0.as_ref());
                self.nfs.push(Nf::Sub(lhs, r));
            }
            _ if is_thing(rhs) => { /* C ⊑ ⊤ is trivial */ }
            _ if is_nothing(rhs) => self.nfs.push(Nf::Sub(lhs, BOT)),
            CE::ObjectIntersectionOf(parts) => {
                // C ⊑ D1 ⊓ D2  ⟺  C ⊑ D1, C ⊑ D2
                for p in parts {
                    self.normalize_sup(lhs, p);
                }
            }
            CE::ObjectSomeValuesFrom { ope, bce } => {
                let r = match self.role_of(ope) {
                    Some(r) => r,
                    None => {
                        self.ignored += 1;
                        return;
                    }
                };
                // ∃r.C over the empty role is unsatisfiable.
                if self.bottom_roles.contains(&r) {
                    self.nfs.push(Nf::Sub(lhs, BOT));
                    return;
                }
                let filler = match self.flatten(bce) {
                    Some(f) => f,
                    None => {
                        self.ignored += 1;
                        return;
                    }
                };
                let f = self.augmented_filler(r, filler);
                self.nfs.push(Nf::SomeSup(lhs, r, f));
            }
            CE::ObjectHasValue { ope, i } => {
                if let (Some(r), Some(f0)) = (self.role_of(ope), self.individual_concept(i)) {
                    let f = self.augmented_filler(r, f0);
                    self.nfs.push(Nf::SomeSup(lhs, r, f));
                } else {
                    self.ignored += 1;
                }
            }
            // Everything else (complement, union, nominals, has-self,
            // data-has-value, …) is reduced to an interned concept and added as
            // an atomic superclass.
            other => match self.flatten(other) {
                Some(r) => self.nfs.push(Nf::Sub(lhs, r)),
                None => self.ignored += 1,
            },
        }
    }

    /// Reduce a (sub-position) class expression to a single concept-name id,
    /// emitting auxiliary normal forms for nested structure. Returns None for
    /// non-EL expressions.
    fn flatten(&mut self, ce: &CE<RcStr>) -> Option<CId> {
        match ce {
            _ if is_thing(ce) => Some(TOP),
            _ if is_nothing(ce) => Some(BOT),
            CE::Class(c) => Some(self.intern_class(c.0.as_ref())),
            CE::ObjectIntersectionOf(parts) => {
                let mut atoms: Vec<CId> = Vec::with_capacity(parts.len());
                for p in parts {
                    atoms.push(self.flatten(p)?);
                }
                Some(self.intern_conjunction(&atoms))
            }
            CE::ObjectSomeValuesFrom { ope, bce } => {
                let r = self.role_of(ope)?;
                if self.bottom_roles.contains(&r) {
                    return Some(BOT); // ∃(empty role).C ≡ ⊥
                }
                let filler = self.flatten(bce)?;
                Some(self.intern_some(r, filler))
            }
            CE::ObjectHasValue { ope, i } => {
                let r = self.role_of(ope)?;
                let filler = self.individual_concept(i)?;
                Some(self.intern_some(r, filler))
            }
            CE::ObjectOneOf(inds) if inds.len() == 1 => self.individual_concept(&inds[0]),
            CE::ObjectOneOf(inds) => {
                // {a, b, ...} ≡ {a} ⊔ {b} ⊔ ...
                let mut ids = Vec::with_capacity(inds.len());
                for i in inds {
                    ids.push(self.individual_concept(i)?);
                }
                Some(self.intern_union(&ids))
            }
            // Structural handling: identical complex expressions are interned to
            // one shared concept, with the sound EL-safe axioms.
            CE::ObjectUnionOf(parts) => {
                let mut ids = Vec::with_capacity(parts.len());
                for p in parts {
                    ids.push(self.flatten(p)?);
                }
                Some(self.intern_union(&ids))
            }
            CE::ObjectComplementOf(inner) => {
                let i = self.flatten(inner)?;
                Some(self.intern_complement(i))
            }
            CE::ObjectHasSelf(ope) => {
                let r = self.role_of(ope)?;
                let c = self.intern_opaque(format!("SELF:{r}"));
                // Record the concept ↔ role link so local reflexivity (`∃r.Self`)
                // seeds and detects `r`-self-loops during saturation.
                if !self.self_roles.iter().any(|&(cc, rr)| cc == c && rr == r) {
                    self.self_roles.push((c, r));
                }
                Some(c)
            }
            CE::DataHasValue { dp, l } => {
                Some(self.intern_opaque(format!("DHV:{}:{}", dp.0.as_ref(), literal_key(l))))
            }
            CE::DataSomeValuesFrom { dp, dr } => {
                Some(self.intern_opaque(format!("DSV:{}:{:?}", dp.0.as_ref(), dr)))
            }
            _ => None, // cardinalities, all-values: not EL
        }
    }

    /// Intern (or reuse) an opaque shared concept for a complex expression that
    /// EL does not reason into, keyed by its structure. Identical expressions
    /// map to the same concept — structural equivalence, which is what lets
    /// `A ≡ E` and `B ≡ E` entail `A ≡ B`.
    fn intern_opaque(&mut self, key: String) -> CId {
        if let Some(&x) = self.expr_memo.get(&key) {
            return x;
        }
        let x = self.fresh_class(5);
        self.expr_memo.insert(key, x);
        x
    }

    /// Intern a disjunction as a shared concept U with `disjunct ⊑ U` for each
    /// disjunct (sound: `X ⊑ X ⊔ Y`). U is otherwise opaque.
    fn intern_union(&mut self, disjuncts: &[CId]) -> CId {
        let mut ids = disjuncts.to_vec();
        ids.sort_unstable();
        ids.dedup();
        if ids.contains(&TOP) {
            return TOP; // X ⊔ ⊤ = ⊤
        }
        ids.retain(|&x| x != BOT); // ⊥ is the identity of ⊔
        if ids.is_empty() {
            return BOT;
        }
        if ids.len() == 1 {
            return ids[0];
        }
        let key = format!("OR:{ids:?}");
        if let Some(&u) = self.expr_memo.get(&key) {
            return u;
        }
        let u = self.fresh_class(3);
        for &d in &ids {
            self.nfs.push(Nf::Sub(d, u));
        }
        // Record members for the union-elimination rule (`u ⊑ C` iff every
        // disjunct `d ⊑ C`), which lets `X ≡ A ⊔ B` with `A ⊑ C`, `B ⊑ C` entail
        // `X ⊑ C`.
        self.unions.push((u, ids.clone()));
        self.expr_memo.insert(key, u);
        u
    }

    /// Intern a complement as a shared concept N with `N ⊓ inner ⊑ ⊥` (sound:
    /// `¬D ⊓ D ⊑ ⊥`). N is otherwise opaque.
    fn intern_complement(&mut self, inner: CId) -> CId {
        let key = format!("NOT:{inner}");
        if let Some(&n) = self.expr_memo.get(&key) {
            return n;
        }
        let n = self.fresh_class(4);
        self.nfs.push(Nf::And(n, inner, BOT));
        self.expr_memo.insert(key, n);
        n
    }

    /// Introduce (or reuse) a defined concept name X with X ≡ ∃r.filler. The
    /// filler is augmented with r's effective range, so range(r)=C turns
    /// ∃r.D into ∃r.(D ⊓ C) at the successor.
    fn intern_some(&mut self, r: RId, filler: CId) -> CId {
        let f = self.augmented_filler(r, filler);
        let key = format!("SOME:{r}:{f}");
        if let Some(&x) = self.expr_memo.get(&key) {
            return x;
        }
        let x = self.fresh_class(1);
        // `x ≡ ∃r.filler`. The *super* form augments the successor with `range(r)`
        // (sound: every r-filler satisfies the range). The *sub* form (`∃r.filler
        // ⊑ x`) must match on the UN-augmented filler — otherwise a role link
        // produced by a property chain (whose successor is not range-augmented)
        // fails to trigger it, dropping entailments like
        // `X ⊑ ∃located_in.C` via `located_in ∘ part_of ⊑ located_in`.
        self.nfs.push(Nf::SomeSup(x, r, f));
        self.nfs.push(Nf::SomeSub(r, filler, x));
        self.expr_memo.insert(key, x);
        x
    }

    /// Augment an existential filler with the effective range of the role:
    /// returns a concept `C` with `C ⊑ filler ⊓ range(r)`. Returns the filler
    /// unchanged if r has no range.
    ///
    /// Crucially this is a ONE-WAY conjunction (`C ⊑ each conjunct` only, no
    /// `conjuncts ⊑ C` recognition rule). The augmented filler is used solely as
    /// the SUCCESSOR node of an existential `∃r.C`; its job is to put `filler`
    /// and `range` into the successor's subsumer set (which `C ⊑ …` does),
    /// triggering every applicable `∃r.A ⊑ T` recognition keyed on those
    /// individual fillers. The reverse recognition `filler ⊓ range ⊑ C` is never
    /// needed — `C` is never matched in sub-position — and adding it (as a full
    /// `intern_conjunction` would) fires `C` into the S-set of every class that
    /// is both `filler` and a `range` — some 97% of all S-entries, and the
    /// dominant memory cost. If `filler ⊓ range` genuinely appears in sub
    /// position elsewhere, that occurrence is interned by `intern_conjunction`
    /// with its own (bidirectional) `And`, so recognition still fires there.
    fn augmented_filler(&mut self, r: RId, filler: CId) -> CId {
        let rng = match self.eff_range.get(&r) {
            Some(v) if !v.is_empty() => v.clone(),
            _ => return filler,
        };
        let mut atoms = vec![filler];
        atoms.extend(rng);
        atoms.sort_unstable();
        atoms.dedup();
        atoms.retain(|&x| x != TOP);
        if atoms.contains(&BOT) {
            return BOT;
        }
        match atoms.len() {
            0 => TOP,
            1 => atoms[0],
            _ => {
                let key = format!("FILL:{atoms:?}");
                if let Some(&x) = self.expr_memo.get(&key) {
                    return x;
                }
                let c = self.fresh_class(2);
                for &a in &atoms {
                    self.nfs.push(Nf::Sub(c, a)); // C ⊑ a  (one-way only)
                }
                self.expr_memo.insert(key, c);
                c
            }
        }
    }

    /// Introduce (or reuse) a defined concept name X equivalent to the
    /// conjunction of `atoms` (X ≡ A1 ⊓ ... ⊓ An), with structural sharing.
    fn intern_conjunction(&mut self, atoms: &[CId]) -> CId {
        let mut ids: Vec<CId> = atoms.to_vec();
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|&x| x != TOP); // ⊤ is the identity of ⊓
        if ids.contains(&BOT) {
            return BOT;
        }
        if ids.is_empty() {
            return TOP;
        }
        if ids.len() == 1 {
            return ids[0];
        }
        let key = format!("AND:{ids:?}");
        if let Some(&x) = self.expr_memo.get(&key) {
            return x;
        }
        // Fold left, introducing X with X ≡ A1 ⊓ ... ⊓ An: X ⊑ each atom and
        // (atoms) ⊑ X via binary conjunction normal forms.
        let mut acc = ids[0];
        for &next in &ids[1..] {
            let fresh = self.fresh_class(2);
            // The conjunct edges are DECOMPOSITION (applied only in `fresh`'s own
            // context); everywhere else `fresh` is composed by the And, so its
            // conjuncts are already present.
            self.nfs.push(Nf::Decomp(fresh, acc));
            self.nfs.push(Nf::Decomp(fresh, next));
            self.nfs.push(Nf::And(acc, next, fresh));
            acc = fresh;
        }
        self.expr_memo.insert(key, acc);
        acc
    }

    /// Treat a named individual as a singleton nominal concept name. The id is
    /// tagged as an individual so it does not surface as a class in the
    /// taxonomy.
    fn individual_concept(&mut self, i: &Individual<RcStr>) -> Option<CId> {
        match i {
            Individual::Named(n) => {
                let id = self.intern_entity(n.0.as_ref());
                self.individuals.insert(id);
                Some(id)
            }
            Individual::Anonymous(_) => None,
        }
    }

    /// Decompose a role chain r1∘...∘rn ⊑ s into binary chains using fresh
    /// intermediate roles.
    fn add_chain(&mut self, rs: &[RId], sup: RId) {
        if rs.len() == 2 {
            self.chains.push((rs[0], rs[1], sup));
            return;
        }
        // r1 ∘ r2 ⊑ f ; f ∘ r3 ∘ ... ⊑ sup
        let fresh = self.fresh_role();
        self.chains.push((rs[0], rs[1], fresh));
        let mut rest = vec![fresh];
        rest.extend_from_slice(&rs[2..]);
        self.add_chain(&rest, sup);
    }

    fn fresh_role(&mut self) -> RId {
        let id = self.role_iri.len() as RId;
        self.role_iri.push(format!("__owlmake_aux_role_{id}"));
        id
    }

    /// Inline "pass-through" conjunction concepts so they are never materialized
    /// as subsumers — the central conjunction optimization. The fresh concept
    /// `C` introduced for a definition body `a ⊓ b` (with `a ⊓ b ⊑ C` and
    /// `C ⊑ targets`) whose ONLY uses are being that one conjunction's result and
    /// flowing to those targets is removed, and each target fired directly via
    /// `a ⊓ b ⊑ target`. The defined class's equivalence and the structural
    /// sharing that lets `A ≡ E`, `B ≡ E` entail `A ≡ B` are preserved, because
    /// both directions route through the conjuncts, not `C`. This eliminates the
    /// dominant S-set bloat on definition-heavy ontologies (phenio: billions of
    /// conjunction-subsumer entries → ~none): a binary `B ⊓ ∃r.f ⊑ A` rule fires
    /// `A` directly, with no intermediate concept in between.
    fn inline_passthrough_conjunctions(&mut self, timing: bool) {
        let n = self.class_iri.len();
        let mut and_def: Vec<Option<(CId, CId)>> = vec![None; n];
        let mut and_count: Vec<u8> = vec![0u8; n];
        let mut disq: Vec<bool> = vec![false; n];
        // A concept is disqualified from inlining if it is used anywhere other
        // than as the result of its single defining `And` and the source of
        // `Sub` edges: as a conjunct, an existential filler/source, a `Sub`
        // *target* (something is ⊑ it — covers conjunction-to-conjunction chains),
        // a union member, or a self concept.
        for nf in &self.nfs {
            match *nf {
                Nf::Sub(_, t) => disq[t as usize] = true,
                Nf::And(a, b, c) => {
                    disq[a as usize] = true;
                    disq[b as usize] = true;
                    let ci = c as usize;
                    and_count[ci] = and_count[ci].saturating_add(1);
                    and_def[ci] = Some((a, b));
                }
                Nf::SomeSup(s, _, f) => {
                    disq[s as usize] = true;
                    disq[f as usize] = true;
                }
                Nf::SomeSub(_, a, b) => {
                    disq[a as usize] = true;
                    disq[b as usize] = true;
                }
                // A conjunction's own decomposition edge — the conjunct is already
                // disqualified via the matching `And`, so nothing to mark here.
                Nf::Decomp(_, _) => {}
            }
        }
        for (u, members) in &self.unions {
            disq[*u as usize] = true;
            for &m in members {
                disq[m as usize] = true;
            }
        }
        for &(c, _) in &self.self_roles {
            disq[c as usize] = true;
        }
        // Inlinable: an auxiliary conjunction concept (kind 2), the result of
        // exactly one `And`, used nowhere else.
        let inlinable: Vec<bool> = (0..n)
            .map(|c| self.kind[c] == 2 && and_count[c] == 1 && !disq[c])
            .collect();
        let n_inline = inlinable.iter().filter(|&&x| x).count();
        if n_inline == 0 {
            return;
        }
        // Rewrite: drop each inlinable concept's defining `And` and outgoing
        // `Sub`s, emitting `a ⊓ b ⊑ target` for each real (non-conjunct) target.
        // Inlinable concepts are never referenced by surviving normal forms (they
        // are disqualified the moment they appear as a conjunct/filler/target),
        // so no dangling references remain.
        let mut out = Vec::with_capacity(self.nfs.len());
        for nf in &self.nfs {
            match *nf {
                Nf::And(_, _, c) if inlinable[c as usize] => {}
                Nf::Decomp(c, _) if inlinable[c as usize] => {}
                Nf::Sub(s, t) if inlinable[s as usize] => {
                    let (a, b) = and_def[s as usize].unwrap();
                    if t != a && t != b {
                        out.push(Nf::And(a, b, t));
                    }
                }
                other => out.push(other),
            }
        }
        self.nfs = out;
        if timing {
            status!("el: inlined {n_inline} pass-through conjunction concept(s)");
        }
    }

    fn finish(mut self, timing: bool, t0: crate::time::Instant) -> Reasoner {
        // Conjunction optimization: don't materialize "definition" conjunction
        // concepts as subsumers; fire their targets directly.
        self.inline_passthrough_conjunctions(timing);
        let n_classes = self.class_iri.len();

        // Build axiom indexes.
        let mut ax = Axioms::default();
        for nf in &self.nfs {
            match *nf {
                Nf::Sub(a, b) => ax.sub.entry(a).or_default().push(b),
                Nf::And(a, b, c) => {
                    ax.conj.entry(a).or_default().push((b, c));
                    ax.conj.entry(b).or_default().push((a, c));
                }
                Nf::SomeSup(a, r, b) => ax.some_sup.entry(a).or_default().push((r, b)),
                Nf::SomeSub(r, a, b) => {
                    ax.some_sub.entry((r, a)).or_default().push(b);
                    ax.some_sub_roles.insert(r);
                    ax.some_sub_fillers.entry(r).or_default().push(a);
                    ax.filler_concepts.insert(a);
                }
                Nf::Decomp(c, a) => ax.decomp.entry(c).or_default().push(a),
            }
        }
        for v in ax.some_sub_fillers.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        ax.has_self = !self.self_roles.is_empty();
        ax.has_unions = !self.unions.is_empty();
        ax.some_sub_any = !ax.some_sub.is_empty();
        // Reflexive-transitive closure of the role hierarchy so CR6 can add all
        // super-roles directly.
        let role_super_closure = transitive_role_closure(&self.role_sub, self.role_iri.len());
        for (r, supers) in role_super_closure.iter().enumerate() {
            for &s in supers {
                ax.role_sub.entry(r as RId).or_default().push(s);
            }
        }
        for &(r1, r2, r3) in &self.chains {
            ax.chain_by_first.entry(r1).or_default().push((r2, r3));
            ax.chain_by_second.entry(r2).or_default().push((r1, r3));
            ax.chain_second_roles.insert(r2);
        }
        for &(c, r) in &self.self_roles {
            ax.self_role.insert(c, r);
            ax.role_self.insert(r, c);
        }
        for (u, members) in &self.unions {
            ax.union_members.insert(*u, members.clone());
            for &m in members {
                ax.member_unions.entry(m).or_default().push(*u);
            }
        }

        let mut state = State {
            s: Vec::new(),
            r_succ: HashMap::default(),
            r_pred: HashMap::default(),
            union_subs: HashMap::default(),
            agenda: Vec::new(),
            scratch: Vec::new(),
        };

        // Choose the saturation engine. The parallel engine reproduces the serial
        // result exactly (confluent Horn EL), so it is used for any large
        // ontology — except under WHELK union-elimination, which is non-confluent
        // and stays serial. `OWLMAKE_PARALLEL=0|1` forces the choice.
        let whelk = WHELK_MODE.with(|m| m.get());
        // Worker count. Peak memory grows with the parallel frontier (≈ worker
        // count × ontology size), so cap workers by the RAM actually available so
        // a big ontology can't OOM. `OWLMAKE_WORKERS` overrides outright.
        let cores = std::thread::available_parallelism().map(|p| p.get()).unwrap_or(1);
        let workers = std::env::var("OWLMAKE_WORKERS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&w| w >= 1)
            .unwrap_or_else(|| worker_budget(cores, n_classes));
        // The engine is chosen from the ONTOLOGY, deterministically. There is no
        // `OWLMAKE_PARALLEL` override: the parallel saturation reproduces the
        // serial result exactly only for confluent Horn EL, so an environment
        // variable selecting between them let ambient state decide what a build
        // INFERS — the one class of difference a plan can never account for. If
        // the two engines can disagree on an input owlmake accepts, that is a bug
        // to fix, not a knob to expose.
        let parallel = !whelk && workers > 1 && n_classes >= 20_000;

        if timing {
            status!(
                "el: index+seed {:.1}s, saturating… ({}) [RSS {} MB at saturation start]",
                t0.elapsed().as_secs_f64(),
                if parallel { format!("parallel ×{workers}") } else { "serial".to_string() },
                vmrss_mb(),
            );
        }
        let t_sat = crate::time::Instant::now();
        if parallel {
            let individuals: Vec<CId> = {
                let mut v: Vec<CId> = self.individuals.iter().copied().collect();
                v.sort_unstable();
                v
            };
            let (s, r_succ) = saturate_parallel(
                &ax,
                n_classes,
                &self.reflexive,
                &self.eff_range,
                &self.top_roles,
                &individuals,
                workers,
            );
            state.s = s;
            state.r_succ = r_succ;
        } else {
            // Serial seeding: S(C) = {C, ⊤}, reflexive self-loops (+ their range),
            // and the universal role's ⊤→individual links; then run to fixpoint.
            state.s = vec![HashSet::default(); n_classes];
            for c in 0..n_classes as CId {
                state.s[c as usize].insert(c);
                state.s[c as usize].insert(TOP);
                state.agenda.push(Work::Sub(c, c));
                if c != TOP {
                    state.agenda.push(Work::Sub(c, TOP));
                }
            }
            for &r in &self.reflexive {
                let rng = self.eff_range.get(&r).cloned().unwrap_or_default();
                for c in 0..n_classes as CId {
                    add_link(&mut state, r, c, c);
                    for &rc in &rng {
                        add_sub(&mut state, c, rc);
                    }
                }
            }
            for &rt in &self.top_roles {
                for &a in &self.individuals {
                    add_link(&mut state, rt, TOP, a);
                }
            }
            saturate(&ax, &mut state);
        }
        if timing {
            let s_entries: usize = state.s.iter().map(|s| s.len()).sum();
            let links: usize = state.r_succ.values().map(|v| v.len()).sum();
            let pred_entries: usize = state.r_pred.values().map(|v| v.len()).sum();
            let n_aux = self.class_iri.iter().filter(|i| i.is_none()).count();
            // Tally S-entries by the ORIGIN KIND of the subsumer, to pinpoint the
            // memory driver (named / ∃some / ⊓conj / ⊔union / ¬compl / opaque).
            let mut by_kind = [0usize; 6];
            for set in state.s.iter() {
                for &d in set.iter() {
                    by_kind[self.kind[d as usize] as usize] += 1;
                }
            }
            status!(
                "el: S-entries by subsumer kind — named {} | some {} | conj {} | union {} | compl {} | opaque {}",
                by_kind[0], by_kind[1], by_kind[2], by_kind[3], by_kind[4], by_kind[5],
            );
            // Payload byte estimates only (exclude hashbrown control bytes /
            // load-factor slack, so real RSS is roughly 2–3× the S figure).
            status!(
                "el: saturate {:.1}s (total {:.1}s)\n     {} concepts ({} aux), {} S-entries, {} role links\n     mem≈ S {}MB, r_succ {}MB, r_pred {}MB",
                t_sat.elapsed().as_secs_f64(),
                t0.elapsed().as_secs_f64(),
                n_classes,
                n_aux,
                s_entries,
                links,
                s_entries * 4 / 1_048_576,
                links * 4 / 1_048_576,
                pred_entries * 8 / 1_048_576,
            );
        }

        // Compact-representation analysis (OWLMAKE_ANALYZE): the per-context spread
        // of conjunction-subsumer ids decides whether a 2-level bitset wins big or
        // backfires. For each context, count the distinct 1024-id blocks its
        // conjunction (kind==2) subsumers touch; a 2-level bitset costs ~128 B per
        // touched block (a 1024-bit leaf) + the per-context index. Compare that to
        // the current FxHashSet (~6.3 B/entry) and an open-addressing set (~5 B).
        if std::env::var_os("OWLMAKE_ANALYZE").is_some() {
            let mut conj_entries: u64 = 0;
            let mut total_blocks: u64 = 0; // Σ distinct 1024-id blocks per context
            let mut max_blocks = 0usize;
            let mut ctx_with_conj = 0u64;
            let mut blocks = std::collections::HashSet::new();
            for set in state.s.iter() {
                blocks.clear();
                let mut n = 0u64;
                for &d in set.iter() {
                    if self.kind[d as usize] == 2 {
                        n += 1;
                        blocks.insert(d >> 10);
                    }
                }
                if n > 0 {
                    ctx_with_conj += 1;
                    conj_entries += n;
                    total_blocks += blocks.len() as u64;
                    max_blocks = max_blocks.max(blocks.len());
                }
            }
            let leaf_bytes = total_blocks * 128; // 1024-bit leaves
            let idx_bytes = total_blocks * 8 + ctx_with_conj * 24; // ptr index + per-ctx vec
            let bitset_mb = (leaf_bytes + idx_bytes) / 1_048_576;
            let hashset_mb = conj_entries * 63 / 10 / 1_048_576; // ~6.3 B/entry
            let oa_mb = conj_entries * 5 / 1_048_576; // open-addressing ~5 B/entry
            let avg_fill = if total_blocks > 0 { conj_entries as f64 / total_blocks as f64 } else { 0.0 };
            status!(
                "el: ANALYZE conj-subsumers {} across {} contexts\n     \
                 distinct 1024-blocks: total {} (avg {:.1}/ctx, max {}), avg fill {:.1}/block (of 1024)\n     \
                 est conj-store MB:  FxHashSet ~{} | open-addressing ~{} | 2-level-bitset ~{}",
                conj_entries, ctx_with_conj,
                total_blocks, total_blocks as f64 / ctx_with_conj.max(1) as f64, max_blocks, avg_fill,
                hashset_mb, oa_mb, bitset_mb,
            );
        }

        let named: Vec<CId> = (0..n_classes as CId)
            .filter(|&c| self.class_iri[c as usize].is_some() && self.seen_as_class.contains(&c))
            .collect();
        let mut individuals: Vec<CId> = self.individuals.iter().copied().collect();
        individuals.sort_unstable();

        Reasoner {
            class_iri: self.class_iri,
            iri_to_cid: self.iri_to_cid,
            role_iri: self.role_iri,
            state,
            ignored: self.ignored,
            named,
            individuals,
        }
    }
}

/// Drain the agenda, applying completion rules until fixpoint.
fn saturate(ax: &Axioms, st: &mut State) {
    while let Some(work) = st.agenda.pop() {
        match work {
            Work::Sub(x, d) => apply_sub(ax, st, x, d, false),
            Work::SubC(x, d) => apply_sub(ax, st, x, d, true),
            Work::Link(r, x, y) => apply_link(ax, st, r, x, y),
        }
    }
}

// === Parallel saturation =================================================
//
// A concurrent, lock-free-state implementation of the same completion rules,
// partitioned by context. Each concept `c` is a *context* owned by worker
// `c % n_workers`; that worker has EXCLUSIVE access to the context's subsumer
// set `S(c)`, its backward/forward role links, and its union-sub list — so those
// are touched with no locks. Every completion rule whose conclusion lands in a
// *different* context is delivered as a message to that context's inbox instead
// of touching its state. Because OWL 2 EL completion is Horn (confluent), the
// saturated `S`-sets are independent of message order, so the classification is
// identical to the serial reasoner regardless of worker count or scheduling, so
// a release built on a 64-core host and one built on a laptop infer the same
// taxonomy. (WHELK union-elimination is non-confluent and stays on the serial
// path.)

/// This process's resident set size in MiB (Linux `VmRSS`), or 0 off Linux.
fn vmrss_mb() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("VmRSS:")
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|kb| kb.parse::<usize>().ok())
                    .map(|kb| kb / 1024)
            })
        })
        .unwrap_or(0)
}

/// Linux `MemAvailable` in MiB (the kernel's estimate of allocatable memory
/// without swapping), or `None` off Linux / on parse failure.
fn mem_available_mib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

/// Linux `MemAvailable` in GiB, or `None` off Linux / on parse failure.
fn mem_available_gib() -> Option<usize> {
    mem_available_mib().map(|m| (m / 1024) as usize)
}

/// Spawn (at most once per process) a background memory watchdog that aborts the
/// whole process *cleanly* if free system memory falls below a floor — so a large
/// classification can never drive the machine into the kernel OOM-killer, which
/// would take down unrelated processes (an editor, other jobs, the desktop).
///
/// The reasoner's peak RSS scales with the ontology (phenio ≈ 51 GB) and there is
/// no algorithmic cap, so the only robust guarantee is to stop *before* exhausting
/// RAM rather than after. The watchdog runs independently of which saturation
/// engine is active (serial / parallel / whelk) and cannot be starved by the
/// saturation hot loop because it lives on its own thread. Aborting loses the
/// in-progress run, but that is strictly better than the OOM-killer reaping the
/// rest of the machine.
///
/// Tunables (env): `OWLMAKE_MEM_FLOOR_GIB` sets the floor in GiB (default 6, a
/// desktop-safe margin that keeps an editor/UI responsive rather than swapping
/// right up to the abort; `0` disables the guard entirely). Off Linux (no
/// `MemAvailable`) the guard is a no-op.
/// Number of classifications in flight. The watchdog thread lives for the
/// process, but it only aborts while this is non-zero: EFO's CI showed a
/// `verify` step parsing a 500 MB release AFTER the reasoner had finished, on a
/// 16 GB runner, and the still-armed watchdog killed a job that was nowhere near
/// an OOM.
static MEM_GUARD_ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Held by a classification for as long as it runs; dropping it disarms the
/// watchdog once the last classification is over.
pub struct MemGuard(());
impl Drop for MemGuard {
    fn drop(&mut self) {
        MEM_GUARD_ACTIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn spawn_mem_watchdog() -> MemGuard {
    MEM_GUARD_ACTIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let floor_gib: u64 = std::env::var("OWLMAKE_MEM_FLOOR_GIB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6);
        if floor_gib == 0 {
            return;
        }
        // Bail out if we can't even read memory (non-Linux): nothing to guard on.
        if mem_available_mib().is_none() {
            return;
        }
        let floor_mib = floor_gib * 1024;
        std::thread::spawn(move || loop {
            // Poll often: the saturator can allocate gigabytes per second, so a
            // long interval could overshoot the floor between checks.
            std::thread::sleep(std::time::Duration::from_millis(100));
            if MEM_GUARD_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                continue;
            }
            if let Some(avail) = mem_available_mib() {
                if avail < floor_mib {
                    status!(
                        "\nowlmake: ABORTING — system free memory fell to {:.1} GiB, below the \
                         {floor_gib} GiB safety floor.\n  The EL reasoner was about to exhaust RAM; \
                         it is aborting now to protect the rest of the machine from the OOM-killer.\n  \
                         Re-run with fewer workers (OWLMAKE_WORKERS=N), free memory, or use a larger \
                         machine. Adjust/disable this guard with OWLMAKE_MEM_FLOOR_GIB (0 disables).",
                        avail as f64 / 1024.0,
                    );
                    // Flush is implicit for stderr; exit immediately so no further
                    // allocation happens. 137 = 128 + SIGKILL, the conventional
                    // "killed for memory" code.
                    std::process::exit(137);
                }
            }
        });
    });
    MemGuard(())
}

/// Choose a worker count. The parallel engine's peak memory is dominated by the
/// saturation *wavefront* (≈ the real work) and the shared `S`-sets/links, which
/// are essentially independent of the worker count — so throttling workers loses
/// speed without saving much memory. We therefore use all cores by default, only
/// backing off when `MemAvailable` is too low to even hold the working set.
/// `OWLMAKE_WORKERS` overrides outright.
fn worker_budget(cores: usize, _n_classes: usize) -> usize {
    match mem_available_gib() {
        // Critically low memory at start: don't pile on threads.
        Some(g) if g < 6 => 1,
        Some(g) if g < 12 => cores.min(4),
        _ => cores,
    }
}

/// A completion conclusion addressed to the context named by its first field.
#[derive(Clone, Copy)]
enum PMsg {
    /// Add subsumer `d` to S(c).
    Sub(CId, CId),
    /// Add subsumer `d` to S(c) as a *composed* conjunction (added by the And
    /// rule, so its conjuncts are already in S(c)) — must not be decomposed.
    SubC(CId, CId),
    /// Backward link: `x →r c` (c is x's r-successor). Fires CR4/CR5/chains at c.
    Back(CId, RId, CId),
    /// Forward link: `c →r z`. Fires role-chain rules where c is the middle node.
    Fwd(CId, RId, CId),
    /// `x ⊑ d` where d is a union concept: register x and replay S(d) to it.
    UnionSub(CId, CId),
}

#[inline]
fn pmsg_target(m: &PMsg) -> CId {
    match *m {
        PMsg::Sub(c, _)
        | PMsg::SubC(c, _)
        | PMsg::Back(c, _, _)
        | PMsg::Fwd(c, _, _)
        | PMsg::UnionSub(c, _) => c,
    }
}

/// A context's role links, bucketed BY ROLE.
///
/// The role chain rule (CR7) needs exactly the links of one role: for a chain
/// `r1 ∘ r2 ⊑ r3` reaching a middle node, it pairs that node's `r1`-predecessors
/// with its `r2`-successors. Bucketing makes that a lookup rather than a scan of
/// every link at the node, which matters most where it is most expensive — a hub
/// node carrying millions of links, under an ontology whose chains are the
/// `r ∘ part_of ⊑ r` shape that propagates along a whole partonomy.
///
/// It also lets the CR4 predecessor scan hoist its `∃r.d ⊑ e` probe out of the
/// per-predecessor loop: the role is fixed for a whole bucket, so the probe runs
/// once per role rather than once per predecessor.
type LinkMap = HashMap<RId, HashSet<CId>>;

/// Per-context mutable state, indexed by concept id. Each cell is only ever
/// accessed by the single worker that owns that index (`idx % n == worker`), so
/// the `UnsafeCell` aliasing is sound despite `Sync`.
struct Shared {
    s: Vec<std::cell::UnsafeCell<HashSet<CId>>>,
    back: Vec<std::cell::UnsafeCell<LinkMap>>,
    fwd: Vec<std::cell::UnsafeCell<LinkMap>>,
    usubs: Vec<std::cell::UnsafeCell<HashSet<CId>>>,
    /// CR4 acceleration: for each context c, the existential *fillers* currently
    /// in S(c) (i.e. `S(c) ∩ filler_concepts`). When a new link `x →r c` arrives,
    /// CR4 must fire `∃r.a ⊑ e` for every filler `a ∈ S(c)` — but S(c) is
    /// conjunction-dominated (~3,230 subsumers, 99% of which are never fillers),
    /// so scanning all of S(c) per link would cost ~60% of saturation time (a cold
    /// `some_sub` probe per subsumer). This caches just the handful of fillers, so
    /// the per-link scan is over `fillsubs[c]` (small) instead of S(c) (huge).
    fillsubs: Vec<std::cell::UnsafeCell<Vec<CId>>>,
}
// SAFETY: workers partition the index space and only ever dereference cells they
// own; no cell is accessed by two threads.
unsafe impl Sync for Shared {}

impl Shared {
    fn new(n: usize) -> Self {
        let mk = || (0..n).map(|_| std::cell::UnsafeCell::new(HashSet::default())).collect();
        let mkl = || (0..n).map(|_| std::cell::UnsafeCell::new(LinkMap::default())).collect();
        Shared {
            s: mk(),
            back: mkl(),
            fwd: mkl(),
            usubs: (0..n).map(|_| std::cell::UnsafeCell::new(HashSet::default())).collect(),
            fillsubs: (0..n).map(|_| std::cell::UnsafeCell::new(Vec::new())).collect(),
        }
    }
}

/// Classify in parallel. Returns the saturated `S`-sets and a `r_succ` map (rebuilt
/// from forward links, for `materialize`). Mirrors the seeding done in `finish`.
#[allow(clippy::too_many_arguments)]
fn saturate_parallel(
    ax: &Axioms,
    n_classes: usize,
    reflexive: &[RId],
    eff_range: &HashMap<RId, Vec<CId>>,
    top_roles: &HashSet<RId>,
    individuals: &[CId],
    n_workers: usize,
) -> (Vec<HashSet<CId>>, HashMap<(RId, CId), HashSet<CId>>) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::Mutex;

    let n = n_workers.max(1);
    let shared = Shared::new(n_classes);
    let inboxes: Vec<Mutex<Vec<PMsg>>> = (0..n).map(|_| Mutex::new(Vec::new())).collect();
    let pending = AtomicUsize::new(0);
    let idle = AtomicUsize::new(0);
    let done = AtomicBool::new(false);
    // Count of subsumer entries derived so far, for the live progress heartbeat.
    let derived = AtomicUsize::new(0);
    // Count of role links (`Back`/`Fwd`) derived so far. Reported alongside
    // `derived` because the two grow independently: a role-chain closure can run
    // away in link space while the subsumer count sits still, and a heartbeat
    // showing only subsumers reads as "converged" when it is anything but.
    let linked = AtomicUsize::new(0);

    // Seed: route each initial conclusion to the inbox of its owning worker.
    // `role_sub` holds every role's reflexive-transitive super-set (incl. itself),
    // so this is never empty for a real role id.
    let role_supers = |r: RId| ax.role_sub.get(&r).map(|v| v.as_slice()).unwrap_or(&[]);
    if !matches!(std::env::var("OWLMAKE_PROGRESS").ok().as_deref(), Some("0")) {
        status!("seed: building initial frontier ({n_classes} contexts, ×{n} workers)…");
    }
    {
        let mut guards: Vec<_> = inboxes.iter().map(|m| m.lock().unwrap()).collect();
        let mut push = |m: PMsg| {
            guards[pmsg_target(&m) as usize % n].push(m);
        };
        // S(c) ⊇ {c, ⊤}
        for c in 0..n_classes as CId {
            push(PMsg::Sub(c, c));
            if c != TOP {
                push(PMsg::Sub(c, TOP));
            }
        }
        // Reflexive roles: (c,c) ∈ R(r) for every c, plus range(r) ⊑ c.
        for &r in reflexive {
            let rng = eff_range.get(&r).cloned().unwrap_or_default();
            for c in 0..n_classes as CId {
                for &s in role_supers(r) {
                    push(PMsg::Back(c, s, c));
                    if ax.chain_second_roles.contains(&s) {
                        push(PMsg::Fwd(c, s, c));
                    }
                }
                for &rc in &rng {
                    push(PMsg::Sub(c, rc));
                }
            }
        }
        // Universal role: ⊤ →rt a for every individual a.
        for &rt in top_roles {
            for &a in individuals {
                for &s in role_supers(rt) {
                    push(PMsg::Back(a, s, TOP));
                    if ax.chain_second_roles.contains(&s) {
                        push(PMsg::Fwd(TOP, s, a));
                    }
                }
            }
        }
        let total: usize = guards.iter().map(|g| g.len()).sum();
        pending.store(total, SeqCst);
    }

    std::thread::scope(|scope| {
        // Progress reporter: a heartbeat of subsumers derived / rate / elapsed,
        // until the workers reach fixpoint. Self-silences unless OWLMAKE_PROGRESS.
        {
            let derived = &derived;
            let linked = &linked;
            let pending = &pending;
            let done = &done;
            scope.spawn(move || {
                use std::sync::atomic::Ordering::Relaxed;
                let mut bar = crate::progress::Progress::new("saturate", 0);
                let start = crate::time::Instant::now();
                let (mut last_d, mut last_t) = (0u64, start);
                while !done.load(SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let d = derived.load(Relaxed) as u64;
                    let l = linked.load(Relaxed) as u64;
                    let q = pending.load(SeqCst) as u64;
                    let now = crate::time::Instant::now();
                    let dt = now.duration_since(last_t).as_secs_f64().max(1e-3);
                    let inst = (d.saturating_sub(last_d)) as f64 / dt / 1.0e6; // M/s
                    last_d = d;
                    last_t = now;
                    // No fixed total (fixpoint): show derived, instantaneous rate,
                    // and the remaining work queue (→0 ⇒ converged).
                    bar.line(&format!(
                        "saturate: {:.0}M sub  {:.0}M link  {:.1}M/s  queue {:.1}M  {:.0}s",
                        d as f64 / 1.0e6,
                        l as f64 / 1.0e6,
                        inst,
                        q as f64 / 1.0e6,
                        start.elapsed().as_secs_f64(),
                    ));
                }
                let d = derived.load(Relaxed) as u64;
                let l = linked.load(Relaxed) as u64;
                bar.line(&format!(
                    "saturate: {:.0}M sub  {:.0}M link  done in {:.0}s",
                    d as f64 / 1.0e6,
                    l as f64 / 1.0e6,
                    start.elapsed().as_secs_f64()
                ));
                eprintln!();
            });
        }
        for w in 0..n {
            let shared = &shared;
            let inboxes = &inboxes;
            let pending = &pending;
            let idle = &idle;
            let done = &done;
            let derived = &derived;
            let linked = &linked;
            scope.spawn(move || {
                use std::sync::atomic::Ordering::Relaxed;
                let mut inserted: usize = 0;
                let mut links: usize = 0;
                let mut local: Vec<PMsg> = Vec::new();
                // Per-target outbox buffers: cross-worker conclusions accumulate here
                // and are flushed in bulk (one inbox lock + one `pending` update per
                // peer per flush), instead of locking per message.
                let mut out: Vec<Vec<PMsg>> = (0..n).map(|_| Vec::new()).collect();
                let flush = |out: &mut [Vec<PMsg>]| {
                    for (t, buf) in out.iter_mut().enumerate() {
                        if buf.is_empty() {
                            continue;
                        }
                        pending.fetch_add(buf.len(), SeqCst);
                        let mut g = inboxes[t].lock().unwrap();
                        if g.is_empty() {
                            std::mem::swap(&mut *g, buf);
                        } else {
                            g.append(buf);
                        }
                        buf.clear();
                    }
                };
                loop {
                    // Drain the local stack (own-context conclusions) in BOUNDED
                    // chunks, flushing outboxes and publishing progress between
                    // chunks. Draining it fully before flushing would hoard
                    // cross-context messages (unbounded outbox memory) and starve
                    // peer workers — fatal on a large ontology.
                    const CHUNK: usize = 65536;
                    // Fold the inbox into the local stack once it exceeds this,
                    // rather than waiting for the stack to empty.
                    //
                    // The stack is LIFO, so a conclusion is consumed close to where
                    // it was produced and the frontier stays small. An inbox drained
                    // only when `local` empties is a FIFO its owner may not touch for
                    // the whole run while its peers pour into it, letting production
                    // run unboundedly ahead of consumption. Folding at chunk
                    // boundaries puts every conclusion under the same depth-first
                    // discipline and caps the backlog at one fold's worth per worker.
                    const FOLD_AT: usize = 1 << 20;
                    while !local.is_empty() {
                        {
                            let mut g = inboxes[w].lock().unwrap();
                            if g.len() >= FOLD_AT {
                                pending.fetch_sub(g.len(), SeqCst);
                                local.append(&mut g);
                            }
                        }
                        let mut steps = 0;
                        while steps < CHUNK {
                            match local.pop() {
                                Some(m) => {
                                    process_pmsg(
                                        m, w, n, shared, ax, &mut local, &mut out, &mut inserted,
                                        &mut links,
                                    );
                                    steps += 1;
                                }
                                None => break,
                            }
                        }
                        flush(&mut out);
                        if inserted > 0 {
                            derived.fetch_add(inserted, Relaxed);
                            inserted = 0;
                        }
                        if links > 0 {
                            linked.fetch_add(links, Relaxed);
                            links = 0;
                        }
                    }
                    // Refill from this worker's inbox.
                    let batch = std::mem::take(&mut *inboxes[w].lock().unwrap());
                    if !batch.is_empty() {
                        pending.fetch_sub(batch.len(), SeqCst);
                        local = batch;
                        continue;
                    }
                    // Inbox empty (and outboxes flushed): watch for work / quiescence.
                    idle.fetch_add(1, SeqCst);
                    loop {
                        if done.load(SeqCst) {
                            return;
                        }
                        // All workers idle and no message in flight ⇒ fixpoint reached.
                        if pending.load(SeqCst) == 0 && idle.load(SeqCst) == n {
                            done.store(true, SeqCst);
                            return;
                        }
                        let b = std::mem::take(&mut *inboxes[w].lock().unwrap());
                        if !b.is_empty() {
                            idle.fetch_sub(1, SeqCst);
                            pending.fetch_sub(b.len(), SeqCst);
                            local = b;
                            break;
                        }
                        std::thread::yield_now();
                    }
                }
            });
        }
    });

    if std::env::var_os("OWLMAKE_TIMING").is_some() {
        let mut total = 0usize;
        let mut maxf = 0usize;
        let mut cap = 0usize;
        for c in &shared.fillsubs {
            let v = unsafe { &*c.get() };
            total += v.len();
            cap += v.capacity();
            maxf = maxf.max(v.len());
        }
        status!(
            "el: fillsubs {} entries (max {}/ctx), payload ~{}MB, capacity ~{}MB",
            total, maxf, total * 4 / 1_048_576, cap * 4 / 1_048_576
        );
    }

    // Move S-sets out of the cells and rebuild r_succ by inverting the backward
    // links. The backward store carries every link of every role — the forward
    // store holds only the roles the chain rule consumes — and `materialize`
    // reads r_succ for arbitrary roles, so the rebuild must come from the
    // complete side.
    let s: Vec<HashSet<CId>> = shared.s.into_iter().map(|c| c.into_inner()).collect();
    let mut r_succ: HashMap<(RId, CId), HashSet<CId>> = HashMap::default();
    for (c, cell) in shared.back.into_iter().enumerate() {
        for (r, xs) in cell.into_inner() {
            for x in xs {
                r_succ.entry((r, x)).or_default().insert(c as CId);
            }
        }
    }
    (s, r_succ)
}

/// Apply one completion message to its (owner-exclusive) context, emitting
/// follow-on conclusions to `local` (same worker) or to peer inboxes.
fn process_pmsg(
    m: PMsg,
    w: usize,
    n: usize,
    shared: &Shared,
    ax: &Axioms,
    local: &mut Vec<PMsg>,
    out: &mut [Vec<PMsg>],
    inserted: &mut usize,
    links: &mut usize,
) {
    macro_rules! emit {
        ($msg:expr) => {{
            let mm = $msg;
            let t = pmsg_target(&mm) as usize % n;
            if t == w {
                local.push(mm);
            } else {
                out[t].push(mm);
            }
        }};
    }
    // Create link x →r z, expanding to all super-roles (CR6), as Back/Fwd pairs.
    macro_rules! emit_link {
        ($x:expr, $r:expr, $z:expr) => {{
            let (lx, lr, lz) = ($x, $r, $z);
            let sup: &[RId] = ax.role_sub.get(&lr).map(|v| v.as_slice()).unwrap_or(std::slice::from_ref(&lr));
            for &s in sup {
                emit!(PMsg::Back(lz, s, lx));
                // Forward links are only consumed by the chain rule for roles that
                // are some chain's second role; skip them otherwise.
                if ax.chain_second_roles.contains(&s) {
                    emit!(PMsg::Fwd(lx, s, lz));
                }
            }
        }};
    }
    match m {
        PMsg::Sub(c, d) | PMsg::SubC(c, d) => {
            // `composed` ⇒ d is a conjunction added by the And rule, conjuncts
            // already in S(c); otherwise d arrived via a told/injected edge (or
            // the seed) and may need decomposing.
            let composed = matches!(m, PMsg::SubC(..));
            let ci = c as usize;
            let s_c = unsafe { &mut *shared.s[ci].get() };
            // Unsatisfiable-context short-circuit: once ⊥ ∈ S(c), stop
            // accumulating c's other subsumers (⊥ still propagates).
            if d != BOT && s_c.contains(&BOT) {
                return;
            }
            if !s_c.insert(d) {
                return;
            }
            *inserted += 1;
            // CR1: d ⊑ e
            if let Some(es) = ax.sub.get(&d) {
                for &e in es {
                    emit!(PMsg::Sub(c, e));
                }
            }
            // Conjunction decomposition: fire unless d was *composed* here (in
            // which case its conjuncts are already present). A told/injected
            // conjunction (e.g. an augmented existential filler `FILL ⊑ a⊓b`)
            // arrives WITHOUT its conjuncts, so it must be decomposed — skipped
            // here, a disjoint filler conjunction goes undetected.
            if !composed {
                if let Some(cs) = ax.decomp.get(&d) {
                    for &cc in cs {
                        emit!(PMsg::Sub(c, cc));
                    }
                }
            }
            // CR2: d ⊓ d2 ⊑ e, d2 ∈ S(c) — e is composed, so emit SubC.
            if let Some(pairs) = ax.conj.get(&d) {
                for &(d2, e) in pairs {
                    if s_c.contains(&d2) {
                        emit!(PMsg::SubC(c, e));
                    }
                }
            }
            // CR3: d ⊑ ∃r.e ⇒ link c →r e
            if let Some(rs) = ax.some_sup.get(&d) {
                for &(r, e) in rs {
                    emit_link!(c, r, e);
                }
            }
            // Local reflexivity: d ≡ ∃r.Self ⇒ self-loop c →r c
            if ax.has_self {
                if let Some(&r) = ax.self_role.get(&d) {
                    emit_link!(c, r, c);
                }
            }
            // CR4/CR5 with c as the successor (filler): a new subsumer d of c
            // propagates to predecessors via `∃r.d ⊑ e` (and ⊥ unconditionally).
            let d_is_filler = ax.filler_concepts.contains(&d);
            if d_is_filler {
                // Cache d as a filler of c so CR4-on-new-link can scan just the
                // fillers of S(c) instead of all of S(c). (d was just freshly
                // inserted into S(c), so it isn't already here — no dedup needed.)
                let fs = unsafe { &mut *shared.fillsubs[ci].get() };
                fs.push(d);
            }
            if d == BOT || d_is_filler {
                let back_c = unsafe { &*shared.back[ci].get() };
                for (&r, xs) in back_c.iter() {
                    // `∃r.d ⊑ e` depends only on the role, so it is probed once
                    // per bucket rather than once per predecessor.
                    let es: &[CId] = if d_is_filler {
                        ax.some_sub.get(&(r, d)).map(|v| v.as_slice()).unwrap_or(&[])
                    } else {
                        &[]
                    };
                    if d != BOT && es.is_empty() {
                        continue;
                    }
                    for &x in xs.iter() {
                        if d == BOT {
                            emit!(PMsg::Sub(x, BOT));
                        }
                        for &e in es {
                            emit!(PMsg::Sub(x, e));
                        }
                    }
                }
            }
            // Union propagation (non-elimination): record c under union d, and if
            // c is itself a union, replay its new subsumer to all its subs.
            if ax.has_unions {
                if ax.union_members.contains_key(&d) {
                    emit!(PMsg::UnionSub(d, c));
                }
                if ax.union_members.contains_key(&c) {
                    let us = unsafe { &*shared.usubs[ci].get() };
                    for &x in us.iter() {
                        emit!(PMsg::Sub(x, d));
                    }
                }
            }
        }
        PMsg::Back(c, r, x) => {
            let ci = c as usize;
            let back_c = unsafe { &mut *shared.back[ci].get() };
            if !back_c.entry(r).or_default().insert(x) {
                return;
            }
            *links += 1;
            let s_c = unsafe { &*shared.s[ci].get() };
            // CR5: ⊥ ∈ S(c) ⇒ ⊥ ∈ S(x)
            if s_c.contains(&BOT) {
                emit!(PMsg::Sub(x, BOT));
            }
            // CR4: for each filler C' ∈ S(c) with ∃r.C' ⊑ e, add e to S(x). Iterate
            // the smaller of (a) this role's fillers, or (b) the fillers actually in
            // S(c), cached in `fillsubs[c]`. (b) is what makes this affordable: a
            // fallback that scans ALL of the conjunction-dominated S(c) (~3,230
            // entries, 99% non-fillers) per link costs ≈60% of saturation time,
            // while `fillsubs[c]` holds only the handful of real fillers.
            if ax.some_sub_any {
                if let Some(fillers) = ax.some_sub_fillers.get(&r) {
                    let fs = unsafe { &*shared.fillsubs[ci].get() };
                    if fillers.len() <= fs.len() {
                        for &a in fillers {
                            if s_c.contains(&a) {
                                if let Some(es) = ax.some_sub.get(&(r, a)) {
                                    for &e in es {
                                        emit!(PMsg::Sub(x, e));
                                    }
                                }
                            }
                        }
                    } else {
                        for &a in fs.iter() {
                            if let Some(es) = ax.some_sub.get(&(r, a)) {
                                for &e in es {
                                    emit!(PMsg::Sub(x, e));
                                }
                            }
                        }
                    }
                }
            }
            // Self-loop ⇒ c is an instance of ∃r.Self.
            if x == c && ax.has_self {
                if let Some(&k) = ax.role_self.get(&r) {
                    emit!(PMsg::Sub(c, k));
                }
            }
            // CR7 (c is the chain's middle node, x →r c is the first edge).
            if let Some(chs) = ax.chain_by_first.get(&r) {
                let fwd_c = unsafe { &*shared.fwd[ci].get() };
                for &(r2, r3) in chs {
                    if let Some(zs) = fwd_c.get(&r2) {
                        for &z in zs.iter() {
                            emit_link!(x, r3, z);
                        }
                    }
                }
            }
        }
        PMsg::Fwd(c, r, z) => {
            let ci = c as usize;
            let fwd_c = unsafe { &mut *shared.fwd[ci].get() };
            if !fwd_c.entry(r).or_default().insert(z) {
                return;
            }
            *links += 1;
            // CR7 (c is the middle node, c →r z is the second edge).
            if let Some(chs) = ax.chain_by_second.get(&r) {
                let back_c = unsafe { &*shared.back[ci].get() };
                for &(r1, r3) in chs {
                    if let Some(xs) = back_c.get(&r1) {
                        for &x in xs.iter() {
                            emit_link!(x, r3, z);
                        }
                    }
                }
            }
        }
        PMsg::UnionSub(d, x) => {
            let di = d as usize;
            let us = unsafe { &mut *shared.usubs[di].get() };
            if !us.insert(x) {
                return;
            }
            let s_d = unsafe { &*shared.s[di].get() };
            for &e in s_d.iter() {
                emit!(PMsg::Sub(x, e));
            }
        }
    }
}

thread_local! {
    /// Whether the union-elimination rule (`u ⊑ C` when every disjunct `⊑ C`) is
    /// enabled. It lies outside the plain EL completion calculus, so the `elk`
    /// reasoner leaves it off — with it on, a union-valued domain/range axiom
    /// can pull a class under a primitive super and shift a published taxonomy.
    /// The `owlmake` reasoner turns it on.
    static WHELK_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Try to add d to S(x); enqueue if newly added.
fn add_sub(st: &mut State, x: CId, d: CId) {
    // Unsatisfiable-context short-circuit: once ⊥ ∈ S(x), x is unsatisfiable and
    // every concept subsumes it, so its other subsumers are irrelevant to the
    // taxonomy — stop accumulating them. ⊥ itself is still added and still
    // propagates to predecessors (CR5).
    if d != BOT && st.s[x as usize].contains(&BOT) {
        return;
    }
    if st.s[x as usize].insert(d) {
        st.agenda.push(Work::Sub(x, d));
    }
}

/// Like [`add_sub`] but marks d as *composed* (added by the And rule), so its
/// decomposition rules are not re-applied — conjuncts are already present.
fn add_sub_composed(st: &mut State, x: CId, d: CId) {
    if d != BOT && st.s[x as usize].contains(&BOT) {
        return;
    }
    if st.s[x as usize].insert(d) {
        st.agenda.push(Work::SubC(x, d));
    }
}

/// Try to add (x,y) to R(r); update indexes and enqueue if newly added.
fn add_link(st: &mut State, r: RId, x: CId, y: CId) {
    if st.r_succ.entry((r, x)).or_default().insert(y) {
        st.r_pred.entry(y).or_default().push((r, x));
        st.agenda.push(Work::Link(r, x, y));
    }
}

/// Handle "d newly in S(x)". `ax` (immutable axiom indexes) and `st` (mutable
/// saturation state) are disjoint borrows, so the index `Vec`s are iterated by
/// reference without cloning.
fn apply_sub(ax: &Axioms, st: &mut State, x: CId, d: CId, composed: bool) {
    // CR1: d ⊑ e
    if let Some(es) = ax.sub.get(&d) {
        for &e in es {
            add_sub(st, x, e);
        }
    }
    // Conjunction decomposition: fire unless d was *composed* into S(x) by the
    // And rule (CR2), in which case its conjuncts are already present. A
    // told/injected conjunction — e.g. an augmented existential filler
    // `FILL ⊑ a⊓b` — arrives WITHOUT its conjuncts, so it must be decomposed.
    if !composed {
        if let Some(cs) = ax.decomp.get(&d) {
            for &c in cs {
                add_sub(st, x, c);
            }
        }
    }
    // CR2: d ⊓ d2 ⊑ e, with d2 already in S(x) — e is composed.
    if let Some(pairs) = ax.conj.get(&d) {
        for &(d2, e) in pairs {
            if st.s[x as usize].contains(&d2) {
                add_sub_composed(st, x, e);
            }
        }
    }
    // CR3: d ⊑ ∃r.e  ⟹  link (x,e) in R(r)
    if let Some(rs) = ax.some_sup.get(&d) {
        for &(r, e) in rs {
            add_link(st, r, x, e);
        }
    }
    // Self (local reflexivity): d ≡ ∃r.Self and d ∈ S(x) ⟹ the r-self-loop
    // R(x,x) holds. Skipped wholesale when the ontology has no `∃r.Self`.
    if ax.has_self {
        if let Some(&r) = ax.self_role.get(&d) {
            add_link(st, r, x, x);
        }
    }
    // All union handling is skipped wholesale when the ontology has no unions
    // (the common case) — three per-subsumer map probes that would always miss.
    if ax.has_unions {
        // Union elimination: `x` (a disjunct) gained subsumer `d`. For each union
        // `u = x ⊔ …`, if every disjunct now has `d` in its S-set, then `u ⊑ d`.
        // Sound, but outside the plain EL calculus — gated to WHELK mode so the
        // `elk` reasoner leaves published taxonomies unchanged.
        if WHELK_MODE.with(|m| m.get()) {
            if let Some(us) = ax.member_unions.get(&x) {
                for &u in us.clone().iter() {
                    if let Some(members) = ax.union_members.get(&u) {
                        if members.iter().all(|&m| st.s[m as usize].contains(&d)) {
                            add_sub(st, u, d);
                        }
                    }
                }
            }
        }
        // Track subclasses of a union concept so a derived `u ⊑ C` propagates:
        // when `x ⊑ d` and `d` is a union, record `x` under `d`. Because union
        // elimination can add subsumers to `S(d)` *before* `x ⊑ d` is discovered,
        // also inherit `d`'s already-derived subsumers here — otherwise the
        // propagation is order-dependent and a union's named equivalent (e.g. a
        // class `≡ A ⊔ B`) silently misses subsumers that both disjuncts have.
        if ax.union_members.contains_key(&d) {
            st.union_subs.entry(d).or_default().push(x);
            let d_supers: Vec<CId> = st.s[d as usize].iter().copied().collect();
            for e in d_supers {
                add_sub(st, x, e);
            }
        }
        // `x` itself is a union and gained subsumer `d` ⟹ every `Y ⊑ x` is `⊑ d`.
        if ax.union_members.contains_key(&x) {
            if let Some(subs) = st.union_subs.get(&x).cloned() {
                for y in subs {
                    add_sub(st, y, d);
                }
            }
        }
    }
    // CR4 (x is the filler Y): for each predecessor (r, w) with (w,x) ∈ R(r),
    // and axiom ∃r.d ⊑ e, add e to S(w).
    // CR5: if d == ⊥, propagate ⊥ to predecessors.
    //
    // The scan only does useful work when `d` is ⊥ (CR5 propagation) or `d` is
    // an existential-sub filler (some `∃r.d ⊑ e` exists). For the overwhelming
    // majority of subsumers — ordinary named ancestors that are never an
    // existential filler — both fail, so the whole predecessor loop is skipped.
    let d_is_filler = ax.filler_concepts.contains(&d);
    if d == BOT || d_is_filler {
        // The predecessor list of x only grows via `add_link`, which `add_sub`
        // never calls — so it is stable here and is iterated by index without a
        // per-call clone (`r_pred[x]` is re-borrowed each step just to copy out
        // the Copy tuple, releasing the borrow before `add_sub` mutates state).
        let n_preds = st.r_pred.get(&x).map_or(0, |v| v.len());
        for i in 0..n_preds {
            let (r, w) = st.r_pred[&x][i];
            if d == BOT {
                add_sub(st, w, BOT);
            }
            if d_is_filler {
                if let Some(es) = ax.some_sub.get(&(r, d)) {
                    for &e in es {
                        add_sub(st, w, e);
                    }
                }
            }
        }
    }
}

/// Handle "(x,y) newly in R(r)".
fn apply_link(ax: &Axioms, st: &mut State, r: RId, x: CId, y: CId) {
    // CR4: for each C' ∈ S(y) and axiom ∃r.C' ⊑ e, add e to S(x). This can only
    // fire when r is used in some `∃r.A ⊑ B` axiom, so skip otherwise (the common
    // case for relation links in OBO). Iterate whichever is smaller: the role's
    // filler set (from the immutable axiom index — no allocation, no snapshot) or
    // a snapshot of S(y). The former wins overwhelmingly, since a role has few
    // distinct existential-sub fillers while S(y) is a large subsumer closure.
    if ax.some_sub_roles.contains(&r) {
        let sy_len = st.s[y as usize].len();
        match ax.some_sub_fillers.get(&r) {
            Some(fillers) if fillers.len() <= sy_len => {
                for &a in fillers {
                    if st.s[y as usize].contains(&a) {
                        if let Some(es) = ax.some_sub.get(&(r, a)) {
                            for &e in es {
                                add_sub(st, x, e);
                            }
                        }
                    }
                }
            }
            _ => {
                let mut sy = std::mem::take(&mut st.scratch);
                sy.clear();
                sy.extend(st.s[y as usize].iter().copied());
                for &cprime in &sy {
                    if let Some(es) = ax.some_sub.get(&(r, cprime)) {
                        for &e in es {
                            add_sub(st, x, e);
                        }
                    }
                }
                st.scratch = sy;
            }
        }
    }
    // CR5: if ⊥ ∈ S(y), add ⊥ to S(x).
    if st.s[y as usize].contains(&BOT) {
        add_sub(st, x, BOT);
    }
    // Self (local reflexivity): an r-self-loop R(x,x) makes x an instance of
    // `∃r.Self`.
    if x == y {
        if let Some(&c) = ax.role_self.get(&r) {
            add_sub(st, x, c);
        }
    }
    // CR6: r ⊑ s ⟹ (x,y) ∈ R(s). role_sub holds the reflexive-transitive
    // closure, so add all supers (excluding r itself).
    if let Some(supers) = ax.role_sub.get(&r) {
        for &s in supers {
            if s != r {
                add_link(st, s, x, y);
            }
        }
    }
    // CR7: chains. r as first role: (y,z) ∈ R(r2) ⟹ (x,z) ∈ R(r3). The
    // successor set is a `HashSet`, so snapshot it into the reusable scratch
    // buffer before mutating links (chains are minority work; the alloc is reused).
    if let Some(chs) = ax.chain_by_first.get(&r) {
        let mut buf = std::mem::take(&mut st.scratch);
        for &(r2, r3) in chs {
            buf.clear();
            if let Some(zs) = st.r_succ.get(&(r2, y)) {
                buf.extend(zs.iter().copied());
            }
            for &z in &buf {
                add_link(st, r3, x, z);
            }
        }
        st.scratch = buf;
    }
    // r as second role: (w,x) ∈ R(r1) ⟹ (w,y) ∈ R(r3). `r_pred` is still a
    // `Vec`, so iterate by index: `add_link` only appends, leaving indices
    // `0..n` (snapshotted length) valid; later appends fire via their own work.
    if let Some(chs) = ax.chain_by_second.get(&r) {
        for &(r1, r3) in chs {
            let n = st.r_pred.get(&x).map_or(0, |v| v.len());
            for i in 0..n {
                let (rr, w) = st.r_pred[&x][i];
                if rr == r1 {
                    add_link(st, r3, w, y);
                }
            }
        }
    }
}

/// Reflexive-transitive closure of immediate role inclusions: returns, for each
/// role, the set of all super-roles (including itself).
fn transitive_role_closure(sub: &[(RId, RId)], n: usize) -> Vec<HashSet<RId>> {
    let mut imm: Vec<HashSet<RId>> = vec![HashSet::default(); n];
    for &(r, s) in sub {
        imm[r as usize].insert(s);
    }
    let mut closure: Vec<HashSet<RId>> = (0..n)
        .map(|r| {
            let mut set = HashSet::default();
            set.insert(r as RId);
            set
        })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for r in 0..n {
            let current: Vec<RId> = closure[r].iter().copied().collect();
            for s in current {
                let supers: Vec<RId> = imm[s as usize].iter().copied().collect();
                for sup in supers {
                    if closure[r].insert(sup) {
                        changed = true;
                    }
                }
            }
        }
    }
    closure
}

/// A canonical key for a literal, used to intern DataHasValue concepts.
fn literal_key(l: &horned_owl::model::Literal<RcStr>) -> String {
    use horned_owl::model::Literal;
    match l {
        Literal::Simple { literal } => format!("s:{literal}"),
        Literal::Language { literal, lang } => format!("l:{lang}:{literal}"),
        Literal::Datatype {
            literal,
            datatype_iri,
        } => format!("d:{}:{literal}", datatype_iri.as_ref()),
    }
}

fn is_thing(ce: &CE<RcStr>) -> bool {
    matches!(ce, CE::Class(c) if c.0.as_ref() == OWL_THING)
}
fn is_nothing(ce: &CE<RcStr>) -> bool {
    matches!(ce, CE::Class(c) if c.0.as_ref() == OWL_NOTHING)
}

/// Whether an object-property expression is an inverse. An inverse inside a
/// class expression is outside the indexable EL fragment, so in elk mode the
/// whole containing axiom is dropped.
fn ope_inverse(ope: &OPE<RcStr>) -> bool {
    matches!(ope, OPE::InverseObjectProperty(_))
}

/// Whether a class expression contains any construct outside the indexable EL
/// fragment (on encountering one, elk mode drops the entire containing axiom
/// rather than keeping its EL part). The constructs that *are* indexable —
/// classes, ⊓, ⊔, ¬, ∃ (incl. has-value/has-self over a non-inverse property),
/// nominals over named individuals, and `DataHasValue` — return `false`
/// (recursing into their sub-expressions). Everything else (`∀`, object/data
/// cardinalities, `DataSomeValuesFrom`/`DataAllValuesFrom`, inverse properties,
/// anonymous individuals, and any other non-EL construct) is "poison".
///
/// This is only consulted in elk mode; the `owlmake` reasoner keeps salvaging
/// the EL parts (which is sound and strictly more complete). See [`Builder::whelk`].
fn elk_poison(ce: &CE<RcStr>) -> bool {
    use horned_owl::model::Individual;
    match ce {
        CE::Class(_) => false,
        CE::ObjectIntersectionOf(ps) | CE::ObjectUnionOf(ps) => ps.iter().any(elk_poison),
        CE::ObjectComplementOf(inner) => elk_poison(inner),
        CE::ObjectSomeValuesFrom { ope, bce } => ope_inverse(ope) || elk_poison(bce),
        CE::ObjectHasValue { ope, i } => ope_inverse(ope) || matches!(i, Individual::Anonymous(_)),
        CE::ObjectHasSelf(ope) => ope_inverse(ope),
        CE::ObjectOneOf(inds) => inds.iter().any(|i| matches!(i, Individual::Anonymous(_))),
        CE::DataHasValue { .. } => false,
        // ∀, cardinalities, data existentials/universals, and anything else are
        // outside the indexable EL fragment ⇒ elk mode aborts the whole axiom.
        _ => true,
    }
}

/// Whether any class expression of a (TBox) class axiom is poison
/// ([`elk_poison`]), so that elk mode drops the whole axiom rather than keeping
/// its EL part.
fn tbox_component_poison(comp: &Component<RcStr>) -> bool {
    match comp {
        Component::SubClassOf(ax) => elk_poison(&ax.sub) || elk_poison(&ax.sup),
        Component::EquivalentClasses(ax) => ax.0.iter().any(elk_poison),
        Component::DisjointClasses(ax) => ax.0.iter().any(elk_poison),
        Component::DisjointUnion(ax) => ax.1.iter().any(elk_poison),
        Component::ObjectPropertyDomain(ax) => elk_poison(&ax.ce),
        Component::ClassAssertion(ax) => elk_poison(&ax.ce),
        _ => false,
    }
}
