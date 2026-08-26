//! `genidN` blank-node numbering for the RDF/XML writer.
//!
//! Every anonymous node (class expression, RDF list cell, reified `owl:Axiom`
//! node, anonymous individual) takes an integer id from a single counter starting
//! at 1, in the order the RDF graph is built: per-entity in render order
//! (ontology header → annotation properties → datatypes → object properties →
//! data properties → classes → individuals, each IRI-sorted), each entity's
//! axioms in (axiom-type, structural) order, and within an axiom the triples in a
//! fixed per-construct order. Shared nodes (referenced by more than one triple,
//! e.g. an annotated axiom's anonymous filler) are emitted as
//! `rdf:nodeID="genidN"`; the rest are inlined but STILL consume a counter value,
//! so the whole traversal has to be walked to get any id right.
//!
//! These ids are the ones released OBO RDF/XML files carry, and they are keyed on
//! the whole preceding traversal, so a counter that drifts by one rewrites every
//! blank-node line of every release diff. This module runs that counter and
//! records, for each shared anonymous class expression, the genid the writer must
//! emit for it.

use std::cmp::Ordering;
use std::collections::HashMap;

use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, ClassExpression as CE,
    Component, Individual, Literal, ObjectPropertyExpression as OPE, RcStr,
    SubObjectPropertyExpression as SOPE,
};

use crate::io::owlfunc::{cmp_ce, cmp_component, cmp_individual};
use crate::model::Model;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

// annotatedProperty IRIs for edge reifications, matching the writer's output.
const P_SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const P_EQUIV: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const P_DISJOINT: &str = "http://www.w3.org/2002/07/owl#disjointWith";
const P_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const P_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";

/// annotatedTarget signature for an annotation value, matching the target part of
/// `owlrdf::reif_signature` applied to a rendered reification block: a named IRI
/// becomes `R⊕esc_attr(iri)`, any literal `L⊕esc(lexical)` (datatype/lang dropped,
/// as reif_signature keeps only the text).
fn ann_value_tsig(av: &AnnotationValue<RcStr>) -> String {
    match av {
        AnnotationValue::IRI(i) => format!("R\u{1}{}", crate::io::owlrdf::esc_attr(i.as_ref())),
        AnnotationValue::Literal(Literal::Simple { literal })
        | AnnotationValue::Literal(Literal::Language { literal, .. })
        | AnnotationValue::Literal(Literal::Datatype { literal, .. }) => {
            format!("L\u{1}{}", crate::io::owlrdf::esc(literal))
        }
        AnnotationValue::AnonymousIndividual(_) => String::new(),
    }
}

/// Full axiom-type index (0–38), the first key an entity's axioms are ordered by.
fn full_type_index(c: &Component<RcStr>) -> i32 {
    use Component::*;
    match c {
        DeclareClass(_) | DeclareObjectProperty(_) | DeclareAnnotationProperty(_)
        | DeclareDataProperty(_) | DeclareNamedIndividual(_) | DeclareDatatype(_) => 0,
        EquivalentClasses(_) => 1,
        SubClassOf(_) => 2,
        DisjointClasses(_) => 3,
        DisjointUnion(_) => 4,
        ClassAssertion(_) => 5,
        SameIndividual(_) => 6,
        DifferentIndividuals(_) => 7,
        ObjectPropertyAssertion(_) => 8,
        NegativeObjectPropertyAssertion(_) => 9,
        DataPropertyAssertion(_) => 10,
        NegativeDataPropertyAssertion(_) => 11,
        EquivalentObjectProperties(_) => 12,
        SubObjectPropertyOf(_) => 13,
        InverseObjectProperties(_) => 14,
        FunctionalObjectProperty(_) => 15,
        InverseFunctionalObjectProperty(_) => 16,
        SymmetricObjectProperty(_) => 17,
        AsymmetricObjectProperty(_) => 18,
        TransitiveObjectProperty(_) => 19,
        ReflexiveObjectProperty(_) => 20,
        IrreflexiveObjectProperty(_) => 21,
        ObjectPropertyDomain(_) => 22,
        ObjectPropertyRange(_) => 23,
        DisjointObjectProperties(_) => 24,
        // SubPropertyChainOf is a SubObjectPropertyOf with a chain sub-expression
        // in horned; index 25 is applied in `axiom_index` below.
        EquivalentDataProperties(_) => 26,
        SubDataPropertyOf(_) => 27,
        FunctionalDataProperty(_) => 28,
        DataPropertyDomain(_) => 29,
        DataPropertyRange(_) => 30,
        DisjointDataProperties(_) => 31,
        HasKey(_) => 32,
        Rule(_) => 33,
        AnnotationAssertion(_) => 34,
        SubAnnotationPropertyOf(_) => 35,
        AnnotationPropertyRange(_) => 36,
        AnnotationPropertyDomain(_) => 37,
        DatatypeDefinition(_) => 38,
        _ => 99,
    }
}

/// Axiom-type index, distinguishing a property-chain SubObjectPropertyOf (25).
fn axiom_index(c: &Component<RcStr>) -> i32 {
    if let Component::SubObjectPropertyOf(ax) = c {
        if matches!(ax.sub, SOPE::ObjectPropertyChain(_)) {
            return 25;
        }
    }
    full_type_index(c)
}

/// Does this axiom contain the SAME anonymous class expression (by structural
/// equality) more than once?
///
/// Each occurrence inside such an axiom is a node of its own — repeating a
/// structure within one axiom does not make it one blank node — and those nodes
/// are not reuse targets for any other axiom either, because blank-node identity
/// follows the source object rather than the structure: a `relax`-derived
/// `SubClassOf` built from one of these operands gets a whole fresh subtree.
/// Only the axiom's own class expressions are walked, never its annotations.
pub(crate) fn has_shared_structure(c: &Component<RcStr>) -> bool {
    let mut seen: HashMap<String, u32> = HashMap::new();
    fn walk(ce: &CE<RcStr>, seen: &mut HashMap<String, u32>) {
        if !matches!(ce, CE::Class(_)) {
            *seen.entry(ce_sig(ce)).or_insert(0) += 1;
        }
        for sub in sub_expressions(ce) {
            walk(sub, seen);
        }
    }
    for ce in component_class_expressions(c) {
        walk(ce, &mut seen);
    }
    seen.values().any(|n| *n > 1)
}

/// The direct class-expression children of a class expression.
fn sub_expressions(ce: &CE<RcStr>) -> Vec<&CE<RcStr>> {
    match ce {
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => v.iter().collect(),
        CE::ObjectComplementOf(b) => vec![b],
        CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => vec![bce],
        CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectMaxCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. } => vec![bce],
        _ => Vec::new(),
    }
}

/// The top-level class expressions an axiom carries.
fn component_class_expressions(c: &Component<RcStr>) -> Vec<&CE<RcStr>> {
    match c {
        Component::SubClassOf(ax) => vec![&ax.sub, &ax.sup],
        Component::EquivalentClasses(ax) => ax.0.iter().collect(),
        Component::DisjointClasses(ax) => ax.0.iter().collect(),
        Component::DisjointUnion(du) => du.1.iter().collect(),
        _ => Vec::new(),
    }
}

/// Axiom order within an entity: axiom-type index, then per-type field order.
pub(crate) fn cmp_axiom(a: &Component<RcStr>, b: &Component<RcStr>) -> Ordering {
    axiom_index(a)
        .cmp(&axiom_index(b))
        .then_with(|| cmp_component(a, b))
}

/// The result of the numbering pass.
#[derive(Default)]
pub struct Genids {
    /// Final counter value (total anonymous nodes + 1).
    pub counter: u64,
    /// For each owning entity IRI, the shared anonymous CE fillers of its
    /// annotated axioms, mapped by a structural signature to the emitted genid.
    pub shared: HashMap<String, HashMap<String, u64>>,
    /// The same nodes as `shared`, but as an ORDERED list per owner, in allocation
    /// order. Two annotated axioms over structurally-equal anonymous expressions
    /// are two DISTINCT blank nodes, which a signature-keyed map cannot represent;
    /// the writer consumes this positionally, in the order it renders them
    /// (equivalentClass then subClassOf, each `ce_key`-sorted).
    pub shared_seq: HashMap<String, Vec<(String, u64)>>,
    /// Signatures the pass actually REUSED, per owner — i.e. where two axioms
    /// resolved to one blank node. The writer keys its operand-reference map on
    /// this rather than re-deriving "is this shared?" with its own predicate: the
    /// two must agree exactly, or an operand renders as a reference to a node the
    /// pass never shared (or inline where it did).
    pub reused: HashMap<String, std::collections::HashSet<String>>,
    /// Debug: genid → description, populated only within `debug_lo..debug_hi`.
    pub debug: HashMap<u64, String>,
    debug_lo: u64,
    debug_hi: u64,
    /// If set, the counter value at the start of each owning entity (owner → id).
    pub entity_start: HashMap<String, u64>,
    trace: Option<String>,
    cur_owner: String,
    /// Per-entity interning of anonymous class expressions by structural
    /// signature: a blank node shared between two of a class's axioms (the relax
    /// pattern — a differentia asserted both in the `EquivalentClasses`
    /// intersection and as a `SubClassOf`) is ONE object, so it takes ONE genid
    /// and renders as a shared `rdf:nodeID`. Reset per entity; structurally-equal
    /// CEs in DIFFERENT classes are distinct source nodes.
    intern: HashMap<String, u64>,
    /// Signatures already given a genid by a `SubClassOf` super in this entity.
    /// Two `SubClassOf` axioms over a structurally-equal anonymous super are ONE
    /// blank node — the writer emits the edge once — so the second must reuse
    /// rather than burn a counter value. `span_gaps` makes this routine: it adds a
    /// plain twin of every annotated `∃R.X` super, and allocating for both instead
    /// of reusing drifts the counter ~10,668 ids past the right value over
    /// `mondo-base.owl`, shifting every later genid in the file. Kept separate from
    /// `intern`, which also holds equiv-intersection operands that a BARE super must
    /// NOT reuse.
    sub_sigs: std::collections::HashSet<String>,
    /// Signature hashes that shared ONE blank node in the RDF this model came from,
    /// carried across the OFN cache hop (`Model::shared_anon`). Structural equality
    /// alone cannot decide this — see the note on that field.
    carried_shared: std::collections::HashSet<u64>,
    /// True when THIS owner's RDF source referenced one `rdf:nodeID` twice — see
    /// [`crate::model::Model::owl_shared_owners`]. Without that evidence an
    /// annotated axiom gets its own blank node.
    owner_shared_in_source: std::collections::HashSet<String>,
    /// Whether the model carries ANY source evidence about shared blank nodes.
    /// When it does, absence of evidence for a class means its structurally-equal
    /// expressions really are separate nodes. When it does not — an OBO or
    /// functional-syntax source, which records no blank-node identity at all —
    /// there is nothing to infer from, so the permissive rule stands.
    have_scan_evidence: bool,
    /// Signatures of anonymous class expressions that appear in at least one
    /// ANNOTATED axiom of the entity being translated, collected by a pre-pass.
    ///
    /// One annotated occurrence makes ALL occurrences of that structure share one
    /// node: the annotated axiom reifies, and its `owl:Axiom` block has to point
    /// its `annotatedTarget` at a named `rdf:nodeID`, so the plain twins reference
    /// that node instead of rendering a copy. Where no occurrence is annotated,
    /// each renders inline and takes an id of its own. This cannot be decided
    /// while walking axioms in order — the plain occurrence may come first —
    /// hence the pre-pass.
    annotated_sigs: std::collections::HashSet<String>,
    /// The `equivalentClass` analogue of `sub_sigs`.
    eq_sigs: std::collections::HashSet<String>,
    /// Signatures contributed by an axiom that repeats one structure inside itself
    /// (`has_shared_structure`). Each occurrence is a fresh object with a node of
    /// its own, so no later axiom may reuse them.
    desharded_sigs: std::collections::HashSet<String>,
    /// Signature -> id for the expressions `spanGaps` re-linked from one source
    /// object (see [`crate::model::Model::span_shared`]). NOT reset per entity:
    /// the same object is referenced by several classes, so it takes one id, and
    /// each entity referring to it once still renders it inline.
    /// group id -> the blank node that group's re-links all take.
    /// Keyed by the span gap AND the expression's structure. A gap is re-linked
    /// from one source object, but two DIFFERENT expressions can be re-linked
    /// from the same one; keying on the gap alone handed the second the first's
    /// node, so an equivalence's intersection took a restriction's id and its own
    /// body was never written.
    span_intern: std::collections::HashMap<(u64, String), u64>,
    /// Document-shared structures (`shared_key`) -> the node the FIRST owner
    /// allocated; later owners' references resolve here rather than allocating.
    doc_shared_intern: std::collections::HashMap<String, u64>,
    /// `owner\u{1}signature` of each re-linked superclass -> its group.
    span_shared: std::collections::HashMap<String, u64>,
    /// `owner\u{1}property\u{1}filler` -> group for nodes the source shared across classes.
    cross_shared: std::collections::HashMap<String, u64>,
    /// Group of the re-link currently being translated, if any.
    span_pending: Option<u64>,
    /// Cross-owner group whose member is currently being translated, if any.
    cross_pending: Option<u64>,
    /// group -> its allocated blank node, first member allocates.
    cross_intern: std::collections::HashMap<u64, u64>,
    /// How many times each carried-provenance signature has already been reused in
    /// this entity. `relax` derives ONE super from ONE equivalence operand, so the
    /// pair is one object and one id; a third structurally-equal occurrence is a
    /// separate object. Keyed by signature, reset per entity.
    carried_used: std::collections::HashSet<String>,
    /// True while translating an `EquivalentClasses` object: its direct
    /// intersection operands are recorded into `intern` so a later `SubClassOf`
    /// super can reuse them (the relax pattern). Only equiv operands are
    /// recorded — an inline `SubClassOf` restriction is NOT a reuse target.
    record_operands: bool,
    pub reuse_count: u64,
    /// Ablation: how many times each reuse clause fired (sub_sigs, this-run shared,
    /// carried provenance, wildcard, shared_key, annotated). Clauses overlap.
    pub by_clause: [u64; 6],
    /// Times reuse was REQUESTED but the id was not in `intern`, so a fresh node was
    /// allocated anyway — a reuse gate set without the `intern` entry it resolves
    /// through fails silently this way, doing nothing at all.
    /// A non-zero value here is a bug, and its size should track the counter drift.
    pub reuse_miss: u64,
    /// Of `reuse_miss`, those where this entity had ALREADY allocated a node for the
    /// same signature — i.e. genuine failures, not the legitimate first allocation.
    pub reuse_miss_repeat: u64,
    /// Anonymous expressions allocated a SECOND node within one entity on the
    /// `reuse = false` path — never counted by `reuse_miss`, which only sees
    /// reuse-requested calls. This is where the "a bare super must not reuse an
    /// equiv operand" rule sends things, so it is the one remaining place a
    /// duplicate allocation can happen silently.
    pub dup_alloc: u64,
    /// The duplicate allocations themselves (owner, signature), for ablation.
    pub dup_log: Vec<(String, String)>,
    /// Signatures allocated so far in this entity (reset per entity), to tell a
    /// first-occurrence miss from a repeat one.
    seen_sigs: std::collections::HashSet<String>,
    pub reuse_log: Vec<(String, u64)>,
    /// Diagnostic: (owner, signature) for each reuse request that missed while the
    /// entity had already allocated a node for that structure.
    pub miss_log: Vec<(String, String)>,
    /// For each owning entity IRI, the reified `owl:Axiom` nodes of its annotated
    /// axioms, as (signature, genid) in creation order. The signature matches
    /// `owlrdf::reif_signature` (annotatedProperty ⊕ annotatedTarget) so the writer
    /// can attach each rendered reification block to its genid and sort the blocks
    /// by genid STRING — root anonymous nodes are ordered lexicographically on
    /// `_:genidN`, so a digit-length boundary (`genid10000` before `genid9999`)
    /// reorders the blocks.
    pub reif: HashMap<String, Vec<(String, u64)>>,
    /// The genid of each SWRL rule's `swrl:Imp` node, in the order the rules are
    /// numbered (`owlapi_rule_key`). The Rules section is emitted in the order
    /// those ids sort AS STRINGS, so `genid1000` precedes `genid868` and an id
    /// range crossing a power of ten rotates the whole section — which is why the
    /// writer cannot order the rules without first running this pass.
    pub rule_ids: Vec<u64>,
}

impl Genids {
    fn fresh(&mut self) -> u64 {
        let v = self.counter;
        self.counter += 1;
        if self.trace.as_deref() == Some(self.cur_owner.as_str()) {
            eprintln!("  [trace {}] genid{v}", self.cur_owner);
        }
        v
    }

    fn note(&mut self, id: u64, desc: impl FnOnce() -> String) {
        if id >= self.debug_lo && id < self.debug_hi {
            self.debug.insert(id, desc());
        }
    }
}

/// Is this class expression the named class `owl:Thing`?
fn is_thing(ce: &CE<RcStr>) -> bool {
    matches!(ce, CE::Class(c) if c.0.as_ref() == OWL_THING)
}

impl Genids {
    /// Translate an object-property expression's own node, if anonymous: an
    /// `ObjectInverseOf` becomes a blank node (`_ owl:inverseOf P`).
    fn translate_ope(&mut self, ope: &OPE<RcStr>) {
        if let OPE::InverseObjectProperty(_) = ope {
            self.fresh();
        }
    }

    fn translate_individual(&mut self, ind: &Individual<RcStr>) {
        if let Individual::Anonymous(_) = ind {
            // Anonymous individual node. Its own referencing axioms are rendered
            // in the anonymous-individuals section, not here.
            self.fresh();
        }
    }

    /// An RDF list of class expressions — cells built from the LAST sorted element
    /// to the first; each cell takes an id, then its element is translated. When
    /// `record`, each anonymous operand's genid is recorded for later
    /// `SubClassOf`-super reuse.
    fn translate_ce_list(&mut self, ops: &[CE<RcStr>], record: bool) {
        let mut sorted: Vec<&CE<RcStr>> = ops.iter().collect();
        sorted.sort_by(|a, b| cmp_ce(a, b));
        for i in (0..sorted.len()).rev() {
            self.fresh(); // list cell
            // A conjunction LEAF is a reuse target however deeply it is nested.
            // `relax` flattens nested conjunctions, so `X ≡ A ⊓ (∃r.B ⊓ ∃s.C)`
            // derives `X ⊑ ∃r.B` and `X ⊑ ∃s.C` from the very objects inside the
            // INNER intersection: those are one blank node each, shared with the
            // equivalence. Re-armed per element because translating one consumes
            // the flag, and cleared after so a restriction FILLER — which relax
            // never reaches — cannot record.
            self.record_operands = record;
            let oid = self.translate_ce(sorted[i]);
            self.record_operands = false;
            if record {
                if let Some(oid) = oid {
                    self.intern.entry(ce_sig(sorted[i])).or_insert(oid);
                }
            }
        }
    }

    /// Spend the RDF list cells of an expression whose blank node id was REUSED
    /// rather than freshly allocated.
    ///
    /// A reused id belongs to the expression NODE, which is one object for the
    /// whole document, so re-rendering an expression already seen hands back its
    /// old id. Its RDF list cells are not shared that way: a cell exists only as
    /// part of this rendering of this collection, so the expression is walked
    /// again and every cell takes a NEW id on every entity that renders it.
    /// Skipping them leaves the counter one id per collection cell per entity
    /// behind, invisible until two ids inside one entity straddle a digit-length
    /// boundary (`genid10000` sorts before `genid9999`) and the `owl:Axiom` blocks
    /// come out swapped.
    fn spend_list_cells(&mut self, ce: &CE<RcStr>) {
        match ce {
            CE::ObjectIntersectionOf(ops) | CE::ObjectUnionOf(ops) => {
                // The list walks tail to head, translating each element between
                // cells, so a nested collection interleaves here too.
                let mut sorted: Vec<&CE<RcStr>> = ops.iter().collect();
                sorted.sort_by(|a, b| cmp_ce(a, b));
                for i in (0..sorted.len()).rev() {
                    self.fresh();
                    self.spend_list_cells(sorted[i]);
                }
            }
            CE::ObjectOneOf(inds) => {
                for _ in 0..inds.len() {
                    self.fresh();
                }
            }
            CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => {
                self.spend_list_cells(bce)
            }
            CE::ObjectMinCardinality { bce, .. }
            | CE::ObjectMaxCardinality { bce, .. }
            | CE::ObjectExactCardinality { bce, .. } => {
                if !is_thing(bce) {
                    self.spend_list_cells(bce);
                }
            }
            CE::ObjectComplementOf(b) => self.spend_list_cells(b),
            _ => {}
        }
    }

    fn translate_ind_list(&mut self, inds: &[Individual<RcStr>]) {
        // Individuals in a list are sorted; cells built back-to-front.
        let mut sorted: Vec<&Individual<RcStr>> = inds.iter().collect();
        sorted.sort_by(|a, b| crate::io::owlfunc::cmp_individual(a, b));
        for i in (0..sorted.len()).rev() {
            self.fresh(); // list cell
            self.translate_individual(sorted[i]);
        }
    }

    /// Translate a class expression, assigning this node's id first (if
    /// anonymous), then its children in the graph's triple order. Returns the node
    /// id if anonymous.
    fn translate_ce(&mut self, ce: &CE<RcStr>) -> Option<u64> {
        self.translate_ce_maybe_reuse(ce, false)
    }

    /// Translate a class expression. When `reuse` and this anonymous CE's
    /// structure was already created in this entity (an `EquivalentClasses`
    /// intersection operand — the relax differentia), reuse that genid: the shared
    /// source blank node is ONE object. Otherwise create a fresh node, recording
    /// its signature so a later `SubClassOf` super can reuse it.
    fn translate_ce_maybe_reuse(&mut self, ce: &CE<RcStr>, reuse: bool) -> Option<u64> {
        if matches!(ce, CE::Class(_)) {
            return None;
        }
        // A cross-owner GROUP member (an ANNOTATED axiom whose target is one
        // minted object shared across owners) takes the group's node ahead of
        // every per-owner reuse rule: the per-owner intern may hold a bare
        // twin's own inline node, and the reification must not point there.
        if let Some(g) = self.cross_pending.take() {
            let id = match self.cross_intern.get(&g) {
                Some(&id) => {
                    self.spend_list_cells(ce);
                    id
                }
                None => {
                    // The owner may already hold this structure's node — the
                    // axiom's bare statement translated first and interned it.
                    // One axiom, one node: take it rather than minting a second.
                    let id = match self.intern.get(&ce_sig(ce)) {
                        Some(&id) => id,
                        None => self.translate_ce_fresh(ce)?,
                    };
                    self.cross_intern.insert(g, id);
                    id
                }
            };
            return Some(id);
        }
        if self.trace.as_deref() == Some(self.cur_owner.as_str()) {
            eprintln!("  [trace {}] CE reuse={reuse} {}", self.cur_owner, &ce_sig(ce)[..ce_sig(ce).len().min(70)]);
        }
        if reuse {
            let sig_dbg = ce_sig(ce);
            if self.intern.get(&sig_dbg).is_none() {
                self.reuse_miss += 1;
                if self.seen_sigs.contains(&sig_dbg) {
                    self.reuse_miss_repeat += 1;
                    if self.miss_log.len() < 40000 {
                        self.miss_log.push((self.cur_owner.clone(), sig_dbg.clone()));
                    }
                }
            }
            if let Some(&id) = self.intern.get(&ce_sig(ce)) {
                if self.trace.as_deref() == Some(self.cur_owner.as_str()) {
                    eprintln!("  [trace {}] REUSE-intern genid{id}", self.cur_owner);
                }
                self.reuse_count += 1;
                self.reused
                    .entry(self.cur_owner.clone())
                    .or_default()
                    .insert(ce_sig(ce));
                if self.reuse_log.len() < 20 {
                    self.reuse_log.push((self.cur_owner.clone(), id));
                }
                return Some(id);
            }
            // A node this OWNER's source body shared with itself, already
            // allocated earlier in the same owner.
            //
            // Owner-qualified, and that is the whole point. `shared_key` is
            // `property\u{1}filler` with no owner, and `owner_shared_in_source`
            // means only "this class referenced one `rdf:nodeID` twice INSIDE
            // ITS OWN BODY" — intra-owner evidence. Published under a
            // document-wide key it let a later, different class with the same
            // `∃P.C` structure resolve to the FIRST class's node, merging two
            // distinct source blank nodes. MONDO_0013920 and MONDO_0013921 each
            // share a node with themselves and each carry
            // `predisposes_towards some MONDO_0100198`; the source gives them
            // four distinct `genid`s and owlmake wrote one, costing EFO's
            // `mondo_import.owl` four restrictions and 1,032 bytes that reached
            // `efo.owl` and `efo.obo`.
            //
            // Sharing ACROSS owners is `cross_shared`'s job — it is keyed
            // `owner\u{1}property\u{1}filler` from `scan_cross_owner_shared`,
            // precisely because, as that scan documents, this evidence "can
            // reuse within one class but never across two".
            if let Some(k) = shared_key(ce).map(|k| format!("{}\u{1}{k}", self.cur_owner)) {
                if self.owner_shared_in_source.contains(&k[self.cur_owner.len() + 1..]) {
                    if let Some(&id) = self.doc_shared_intern.get(&k) {
                        if self.trace.as_deref() == Some(self.cur_owner.as_str()) {
                            eprintln!("  [trace {}] REUSE-doc genid{id}", self.cur_owner);
                        }
                        self.reuse_count += 1;
                        self.reused
                            .entry(self.cur_owner.clone())
                            .or_default()
                            .insert(ce_sig(ce));
                        return Some(id);
                    }
                }
            }
        }
        // Take an existing id for this structure without marking it shared, so
        // rendering is unaffected and only the counter moves. An expression
        // `spanGaps` re-linked from ONE source object is one blank node however
        // many classes now carry it. Take the id silently — it is not recorded as
        // "shared", because each entity references it once and so still renders it
        // inline.
        if let Some(g) = self.span_pending.take() {
            if let Some(&id) = self.span_intern.get(&(g, ce_sig(ce))) {
                if self.trace.as_deref() == Some(self.cur_owner.as_str()) {
                    eprintln!("  [trace {}] REUSE-span genid{id}", self.cur_owner);
                }
                self.spend_list_cells(ce);
                return Some(id);
            }
            // A cross-owner group is ONE node whichever member reaches it first,
            // and its two members take different routes: an annotated member
            // interns under `cross_intern` (by group), a bare one here (by group
            // AND structure). Take the group's node when it already has one —
            // otherwise the bare member mints a second id, and that id is then
            // wasted, because the WRITER resolves the edge to the group's node
            // either way. Every later blank node moves along by one, which is the
            // whole of `merged-partonomy`'s renumbering.
            if let Some(&id) = self.cross_intern.get(&g) {
                if self.trace.as_deref() == Some(self.cur_owner.as_str()) {
                    eprintln!("  [trace {}] REUSE-cross-as-span genid{id}", self.cur_owner);
                }
                self.spend_list_cells(ce);
                self.span_intern.insert((g, ce_sig(ce)), id);
                return Some(id);
            }
            let out = self.translate_ce_fresh(ce);
            if let Some(id) = out {
                self.span_intern.insert((g, ce_sig(ce)), id);
            }
            return out;
        }
        // A cross-owner GROUP member: one minted object asserted for several
        // classes, so one blank node, allocated at the first member. Unlike a
        // span re-link the WRITER must know — every member's edge renders as an
        // `rdf:nodeID` reference and the definition is emitted once at the first
        // referencing owner — so the id is published per owner in `group_refs`.
        // It is NOT pushed into `shared_seq`: that is a positional contract over
        // annotated axioms, which the caller's own record keeps.
        self.translate_ce_fresh(ce)
    }

    /// Translate an anonymous CE that has not been interned this entity.
    fn translate_ce_fresh(&mut self, ce: &CE<RcStr>) -> Option<u64> {
        // Reuse-target recording covers THIS expression's own conjuncts. Taking
        // the flag here bounds it to them: everything reached through a
        // restriction filler, a union or a complement is outside the conjunction
        // relax flattens, so it seeds no reuse target.
        let record = std::mem::take(&mut self.record_operands);
        if !matches!(ce, CE::Class(_)) {
            let s = ce_sig(ce);
            if !self.seen_sigs.insert(s.clone()) {
                self.dup_alloc += 1;
                if self.dup_log.len() < 60000 {
                    self.dup_log.push((self.cur_owner.clone(), s));
                }
            }
        }
        match ce {
            CE::Class(_) => None,
            CE::ObjectSomeValuesFrom { ope, bce } | CE::ObjectAllValuesFrom { ope, bce } => {
                let id = self.fresh();
                self.note(id, || format!("Restriction({ce:?})"));
                self.translate_ope(ope);
                self.translate_ce(bce);
                Some(id)
            }
            CE::ObjectHasValue { ope, i } => {
                let id = self.fresh();
                self.translate_ope(ope);
                self.translate_individual(i);
                Some(id)
            }
            CE::ObjectHasSelf(ope) => {
                let id = self.fresh();
                self.translate_ope(ope);
                Some(id)
            }
            CE::ObjectMinCardinality { ope, bce, .. }
            | CE::ObjectMaxCardinality { ope, bce, .. }
            | CE::ObjectExactCardinality { ope, bce, .. } => {
                let id = self.fresh();
                self.translate_ope(ope);
                if !is_thing(bce) {
                    self.translate_ce(bce);
                }
                Some(id)
            }
            CE::ObjectIntersectionOf(ops) => {
                let id = self.fresh();
                self.note(id, || format!("IntersectionOf({} ops)", ops.len()));
                self.translate_ce_list(ops, record);
                Some(id)
            }
            CE::ObjectUnionOf(ops) => {
                let id = self.fresh();
                self.translate_ce_list(ops, false);
                Some(id)
            }
            CE::ObjectComplementOf(b) => {
                let id = self.fresh();
                self.translate_ce(b);
                Some(id)
            }
            CE::ObjectOneOf(inds) => {
                let id = self.fresh();
                self.translate_ind_list(inds);
                Some(id)
            }
            // Data restrictions: the restriction node, then the DATA RANGE, which
            // is a subtree of its own whenever it is not a bare datatype.
            CE::DataSomeValuesFrom { dr, .. } | CE::DataAllValuesFrom { dr, .. } => {
                let id = self.fresh();
                self.translate_dr(dr);
                Some(id)
            }
            CE::DataHasValue { .. } => Some(self.fresh()),
            CE::DataMinCardinality { dr, .. }
            | CE::DataMaxCardinality { dr, .. }
            | CE::DataExactCardinality { dr, .. } => {
                let id = self.fresh();
                self.translate_dr(dr);
                Some(id)
            }
        }
    }

    /// A data range's own anonymous nodes. A bare `Datatype` is an IRI and costs
    /// nothing; every other form is a blank node with a list or a subtree under it.
    ///
    /// A `DatatypeRestriction` is three nodes for one facet — the `rdfs:Datatype`
    /// node, one `owl:withRestrictions` list cell, and the facet's own node — and
    /// all three have to be counted, or every later genid in a document holding
    /// one comes out short by three: twenty of them move the counter by sixty.
    fn translate_dr(&mut self, dr: &horned_owl::model::DataRange<RcStr>) {
        use horned_owl::model::DataRange as DR;
        match dr {
            DR::Datatype(_) => {}
            DR::DataIntersectionOf(v) | DR::DataUnionOf(v) => {
                self.fresh();
                for d in v {
                    self.fresh();
                    self.translate_dr(d);
                }
            }
            DR::DataComplementOf(d) => {
                self.fresh();
                self.translate_dr(d);
            }
            DR::DataOneOf(lits) => {
                self.fresh();
                for _ in lits {
                    self.fresh();
                }
            }
            DR::DatatypeRestriction(_, facets) => {
                self.fresh();
                for _ in facets {
                    self.fresh();
                    self.fresh();
                }
            }
        }
    }

    /// Translate the annotations reified on an axiom/annotation node: each may
    /// carry an anonymous value or nested annotations (a further reified node).
    fn translate_annotations(&mut self, anns: &std::collections::BTreeSet<Annotation<RcStr>>) {
        // Annotations are numbered by property IRI, ties broken on the value. The
        // sort is unconditional, so horned's own BTreeSet order never decides the
        // numbering. That key only approximates the order these ids have to follow:
        // two annotations that tie on property IRI can come out in the other order,
        // and every genid after them drifts.
        let mut sorted: Vec<&Annotation<RcStr>> = anns.iter().collect();
        sorted.sort_by(|a, b| {
            a.ap.0
                .as_ref()
                .cmp(b.ap.0.as_ref())
                .then_with(|| crate::io::owlfunc::cmp_annotation_value(&a.av, &b.av))
        });
        for anno in sorted {
            self.translate_annotation(anno);
        }
    }

    fn translate_annotation(&mut self, anno: &Annotation<RcStr>) {
        // Base triple (subject already mapped). An anonymous-individual value
        // gets a node; nested annotations reify the annotation itself.
        if let AnnotationValue::AnonymousIndividual(_) = &anno.av {
            self.fresh();
        }
        // An annotation CAN carry its own annotations — horned's `Annotation` has an
        // `ann` set — and each nesting level reifies as a further `owl:Annotation`
        // node, so each one consumes an id.
        if !anno.ann.is_empty() {
            self.fresh();
            self.translate_annotations(&anno.ann);
        }
    }

    /// A single-triple axiom: subject node (if anon), then object node (+subtree,
    /// if anon), then — when the axiom is annotated — the reified `owl:Axiom`
    /// node and its annotations. Returns the object CE's id if it was anonymous.
    fn single_triple_ce(
        &mut self,
        subject: Option<&CE<RcStr>>,
        object: &CE<RcStr>,
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
        reuse_object: bool,
    ) -> Option<u64> {
        self.single_triple_ce_reif(subject, object, anns, reuse_object, None)
    }

    /// As `single_triple_ce`, but records the reification node's (signature, genid)
    /// under `cur_owner` when `reif_prop` names the annotatedProperty. The target
    /// signature is derived from the object CE and its genid, matching
    /// `owlrdf::reif_signature` (named class → `R⊕iri`, anon → `N⊕genidN`).
    fn single_triple_ce_reif(
        &mut self,
        subject: Option<&CE<RcStr>>,
        object: &CE<RcStr>,
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
        reuse_object: bool,
        reif_prop: Option<&str>,
    ) -> Option<u64> {
        if let Some(s) = subject {
            self.translate_ce(s);
        }
        let obj_id = self.translate_ce_maybe_reuse(object, reuse_object);
        if !anns.is_empty() {
            let rid = self.fresh(); // owl:Axiom reification node
            if let Some(prop) = reif_prop {
                let tsig = match object {
                    CE::Class(c) => format!("R\u{1}{}", c.0.as_ref()),
                    _ => format!("N\u{1}genid{}", obj_id.unwrap_or(0)),
                };
                self.reif
                    .entry(self.cur_owner.clone())
                    .or_default()
                    .push((format!("{prop}\u{1}{tsig}"), rid));
            }
            self.translate_annotations(anns);
        }
        obj_id
    }
}

/// Run the numbering pass over the model, in the writer's graph-build order.
pub fn compute(model: &Model, debug_lo: u64, debug_hi: u64) -> Genids {
    // An ANONYMOUS ontology is itself a blank node, so it takes the first id and
    // every other node in the document shifts by one. (A 25-line fixture through
    // both writers: the same shared restriction is `genid2` when the ontology is
    // anonymous and `genid1` when it carries an IRI.) One missed id here rewrites
    // every genid in the file AND rotates the reification blocks wherever the
    // string sort crosses a digit-length boundary — `"genid1000" < "genid999"`.
    let anon_ontology = !model.ont.iter().any(|ac| {
        matches!(&ac.component, Component::OntologyID(id) if id.iri.is_some())
    });
    let mut g = Genids {
        counter: if anon_ontology { 2 } else { 1 },
        reuse_miss: 0,
        reuse_miss_repeat: 0,
        dup_alloc: 0,
        by_clause: [0; 6],
        dup_log: Vec::new(),
        miss_log: Vec::new(),
        seen_sigs: Default::default(),
        sub_sigs: Default::default(),
        shared_seq: Default::default(),
        reused: Default::default(),
        owner_shared_in_source: Default::default(),
        have_scan_evidence: false,
        carried_shared: Default::default(),
        annotated_sigs: Default::default(),
        eq_sigs: Default::default(),
        desharded_sigs: Default::default(),
        carried_used: Default::default(),
        span_intern: Default::default(),
        doc_shared_intern: Default::default(),
        span_shared: model.span_shared.clone(),
        cross_shared: model.cross_shared.clone(),
        span_pending: None,
        debug_lo,
        debug_hi,
        trace: std::env::var("OM_GENID_TRACE").ok(),
        ..Default::default()
    };

    // Bucket components by owning entity IRI and by section kind.
    let mut by_entity: HashMap<String, Vec<&AnnotatedComponent<RcStr>>> = HashMap::new();
    let mut ann_props: Vec<String> = Vec::new();
    let mut datatypes: Vec<String> = Vec::new();
    let mut obj_props: Vec<String> = Vec::new();
    let mut data_props: Vec<String> = Vec::new();
    let mut classes: Vec<String> = Vec::new();
    let mut individuals: Vec<String> = Vec::new();
    let mut ont_anns: Vec<&AnnotatedComponent<RcStr>> = Vec::new();
    let mut general: Vec<&AnnotatedComponent<RcStr>> = Vec::new();
    // Entities whose graph is non-empty here — the writer gives each of these a
    // section whether or not it is declared and whether or not it is built-in.
    let mut bodied_entities: std::collections::HashSet<String> = Default::default();

    for ac in model.ont.iter() {
        // Record declared entities for the section lists.
        match &ac.component {
            Component::DeclareAnnotationProperty(d) => ann_props.push(d.0 .0.as_ref().to_string()),
            Component::DeclareDatatype(d) => datatypes.push(d.0 .0.as_ref().to_string()),
            Component::DeclareObjectProperty(d) => obj_props.push(d.0 .0.as_ref().to_string()),
            Component::DeclareDataProperty(d) => data_props.push(d.0 .0.as_ref().to_string()),
            Component::DeclareClass(d) => classes.push(d.0 .0.as_ref().to_string()),
            Component::DeclareNamedIndividual(d) => individuals.push(d.0 .0.as_ref().to_string()),
            Component::OntologyAnnotation(_) => ont_anns.push(ac),
            _ => {}
        }
        if let Some(owner) = body_owner(&ac.component) {
            bodied_entities.insert(owner);
        }
        if let Some(owner) = owner_iri(&ac.component) {
            by_entity.entry(owner).or_default().push(ac);
        } else if is_general_axiom(&ac.component) {
            general.push(ac);
        }
    }
    // The writer's per-kind sections are driven by the SIGNATURE, not by
    // `Declaration` axioms (see `owlrdf::save`), so the numbering pass must walk
    // exactly the same entity list — otherwise a referenced-but-undeclared entity
    // gets rendered without ever having been numbered.
    {
        let sig = crate::cmd::select::signature_entities(model);
        // A BUILT-IN entity never gets a stub either: an ontology using
        // `rdfs:label`, `rdfs:seeAlso`, `owl:deprecated` and `owl:versionInfo`
        // without declaring any of them renders stubs for none, while its
        // undeclared `IAO_0000115` and `RO_0002200` both get one.
        // `mondo-international.owl` is annotated `owl:versionInfo <date>`, which
        // puts that property in the signature and nowhere else.
        let builtin_dt = |iri: &str| {
            iri.starts_with("http://www.w3.org/2001/XMLSchema#")
                || iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
                || iri.starts_with("http://www.w3.org/2002/07/owl#")
        };
        let builtin = builtin_dt;
        let undeclared = |kind: &str, iri: &String| -> bool {
            model.closure_declared.is_empty()
                || !model.closure_declared.contains(&format!("{kind}\u{0}{iri}"))
        };
        // …with one relaxation, for annotation properties and classes, matching the
        // writer's `bodied` test: a built-in never gets a STUB, but one that
        // carries a BODY still gets a section — and a section that is rendered is a
        // section that must be numbered. RO annotates `rdfs:isDefinedBy` with an
        // `IAO_0000589` assertion that is itself annotated, so that block reifies
        // and takes the document's FIRST blank node.
        let bodied = |i: &String| bodied_entities.contains(i);
        let keep_ap = |i: &String| bodied(i) || (undeclared("ap", i) && !builtin(i));
        let keep_class = |i: &String| bodied(i) || (undeclared("class", i) && !builtin(i));
        ann_props.extend(sig.annotation_properties.iter().filter(|i| keep_ap(i)).cloned());
        obj_props.extend(sig.object_properties.iter().filter(|i| undeclared("op", i) && !builtin(i)).cloned());
        data_props.extend(sig.data_properties.iter().filter(|i| undeclared("dp", i) && !builtin(i)).cloned());
        classes.extend(sig.classes.iter().filter(|i| keep_class(i)).cloned());
        individuals.extend(sig.individuals.iter().filter(|i| undeclared("ni", i) && !builtin(i)).cloned());
        datatypes.extend(
            sig.datatypes
                .iter()
                .filter(|d| !builtin_dt(d) && undeclared("dt", d))
                .cloned(),
        );
    }

    // Each section is ordered by its IRI split at the NCName boundary — the same
    // `iri_key` the writer sorts by. The numbering pass has to walk the entities in
    // exactly the order they are rendered in: a byte-wise sort yields the same TOTAL
    // node count but distributes the ids differently across entities, which is enough
    // to put 30 lines of `owl:Axiom` blocks in a different order in a filtered UBERON
    // import whose construct counts are identical either way.
    let by_iri = |a: &String, b: &String| {
        crate::io::owlrdf::iri_key(a).cmp(&crate::io::owlrdf::iri_key(b))
    };
    ann_props.sort_by(by_iri);
    ann_props.dedup();
    datatypes.sort_by(by_iri);
    datatypes.dedup();
    obj_props.sort_by(by_iri);
    obj_props.dedup();
    data_props.sort_by(by_iri);
    data_props.dedup();
    classes.sort_by(by_iri);
    classes.dedup();
    individuals.sort_by(by_iri);
    individuals.dedup();

    // Ontology header: annotations on the ontology (rarely anonymous).
    for ac in &ont_anns {
        if let Component::OntologyAnnotation(oa) = &ac.component {
            g.translate_annotation(&oa.0);
        }
    }

    // Entities in render order; each entity's axioms by axiom-type index, then
    // per-type field order.
    for section in [
        &ann_props,
        &datatypes,
        &obj_props,
        &data_props,
        &classes,
        &individuals,
    ] {
        for iri in section.iter() {
            if let Some(mut axioms) = by_entity.remove(iri) {
                axioms.sort_by(|a, b| cmp_axiom(&a.component, &b.component));
                g.entity_start.insert(iri.clone(), g.counter);
                g.cur_owner = iri.clone();
                g.intern.clear();
                g.sub_sigs.clear();
                g.eq_sigs.clear();
                g.desharded_sigs.clear();
                for ac in &axioms {
                    if has_shared_structure(&ac.component) {
                        for ce in component_class_expressions(&ac.component) {
                            if !matches!(ce, CE::Class(_)) {
                                g.desharded_sigs.insert(ce_sig(ce));
                            }
                        }
                    }
                }
                g.seen_sigs.clear();
                g.annotated_sigs.clear();
                g.carried_used.clear();
                for ac in &axioms {
                    if ac.ann.is_empty() {
                        continue;
                    }
                    match &ac.component {
                        Component::SubClassOf(ax) => {
                            if !matches!(ax.sup, CE::Class(_)) {
                                g.annotated_sigs.insert(ce_sig(&ax.sup));
                            }
                        }
                        // mondo.owl merges the import closure, so it also carries
                        // annotated EquivalentClasses/DisjointClasses over anonymous
                        // expressions; the same one-annotated-occurrence rule applies.
                        Component::EquivalentClasses(ax) => {
                            for m in &ax.0 {
                                if !matches!(m, CE::Class(_)) {
                                    g.annotated_sigs.insert(ce_sig(m));
                                }
                            }
                        }
                        Component::DisjointClasses(ax) => {
                            for m in &ax.0 {
                                if !matches!(m, CE::Class(_)) {
                                    g.annotated_sigs.insert(ce_sig(m));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                g.carried_shared =
                    model.shared_anon.get(iri).cloned().unwrap_or_default();
                g.owner_shared_in_source =
                    model.owl_shared_owners.get(iri).cloned().unwrap_or_default();
                // Evidence exists when the SOURCE could express blank-node
                // identity, not merely when some owner happened to use it — an
                // RDF/XML module that shares nothing still says every occurrence
                // is its own node. The `owl_shared_owners` fallback keeps models
                // that acquired sharing facts without the flag (the OFN cache
                // hop restores `shared_anon`/`#sharedowner` but parses as OFN).
                g.have_scan_evidence =
                    model.rdf_blank_node_identity || !model.owl_shared_owners.is_empty();
                for ac in axioms {
                    if g.trace.as_deref() == Some(iri.as_str()) {
                        eprintln!(
                            "  [trace {}] AXIOM idx={} {:?}",
                            iri,
                            axiom_index(&ac.component),
                            std::mem::discriminant(&ac.component)
                        );
                    }
                    g.translate_axiom(iri, ac);
                }
            }
        }
    }

    // Anonymous individuals come between the entity sections and the general
    // axioms: each is a blank node of its own, and the assertions hung off it are
    // its block's body. One id per individual, taken in the order the writer emits
    // the blocks in.
    g.cur_owner = "__anon_individuals__".to_string();
    {
        let anon_blocks = crate::io::anon_individual_order(
            &model.owl_anon_blocks,
            model.anon_alloc_base,
            model.anon_hash_capacity,
            model.anon_imports_end,
        );
        if !anon_blocks.is_empty() {
            // Replayed verbatim from the source: the block text is the body, so
            // only the individual's own node is numbered here.
            for _ in &anon_blocks {
                g.intern.clear();
                g.fresh();
            }
        } else if model.anon_hash_capacity == 0 {
            let mut by_ind: HashMap<String, Vec<&AnnotatedComponent<RcStr>>> = HashMap::new();
            for ac in model.ont.iter() {
                if let Component::AnnotationAssertion(aa) = &ac.component {
                    if let AnnotationSubject::AnonymousIndividual(a) = &aa.subject {
                        by_ind.entry(a.0.as_ref().to_string()).or_default().push(ac);
                    }
                }
            }
            let pos = |id: &str| {
                let bare = id.strip_prefix("_:").unwrap_or(id);
                model.anon_doc_order.iter().position(|l| l == bare).unwrap_or(usize::MAX)
            };
            let mut ids: Vec<&String> = by_ind.keys().collect();
            ids.sort_by(|a, b| pos(a).cmp(&pos(b)).then_with(|| a.cmp(b)));
            for id in ids {
                g.intern.clear();
                g.fresh();
                for ac in &by_ind[id] {
                    if !ac.ann.is_empty() {
                        g.fresh();
                        g.translate_annotations(&ac.ann);
                    }
                }
            }
        }
    }

    // Annotation assertions on an IRI that no entity block carried — the IRI is
    // punned, or in no signature at all — are their own pass, after the anonymous
    // individuals and before the general axioms. Only an ANNOTATED one takes an
    // id, for its `owl:Axiom` node.
    {
        let typed: std::collections::HashSet<&String> = ann_props
            .iter()
            .chain(datatypes.iter())
            .chain(obj_props.iter())
            .chain(data_props.iter())
            .chain(classes.iter())
            .chain(individuals.iter())
            .collect();
        let ont_iri = model.ont.iter().find_map(|ac| match &ac.component {
            Component::OntologyID(id) => id.iri.as_ref().map(|i| i.as_ref().to_string()),
            _ => None,
        });
        let mut untyped: Vec<String> = by_entity
            .keys()
            .filter(|k| {
                !typed.contains(*k)
                    && Some(k.as_str()) != ont_iri.as_deref()
                    && k.as_str() != OWL_THING
                    && by_entity[*k]
                        .iter()
                        .any(|ac| matches!(ac.component, Component::AnnotationAssertion(_)))
            })
            .cloned()
            .collect();
        untyped.sort_by(|a, b| {
            crate::io::owlrdf::iri_key(a).cmp(&crate::io::owlrdf::iri_key(b))
        });
        for iri in untyped {
            let mut axioms = by_entity.remove(&iri).unwrap_or_default();
            axioms.sort_by(|a, b| cmp_axiom(&a.component, &b.component));
            g.entity_start.insert(iri.clone(), g.counter);
            g.cur_owner = iri.clone();
            g.intern.clear();
            g.sub_sigs.clear();
            g.eq_sigs.clear();
            g.desharded_sigs.clear();
            g.seen_sigs.clear();
            g.annotated_sigs.clear();
            g.carried_used.clear();
            for ac in axioms {
                if matches!(ac.component, Component::AnnotationAssertion(_)) {
                    g.translate_axiom(&iri, ac);
                }
            }
        }
    }

    // General axioms (GCIs, 3+ disjoint, DifferentIndividuals) render last, each
    // numbered as a graph of its own (its own intern), in axiom order.
    general.sort_by(|a, b| cmp_axiom(&a.component, &b.component));
    g.cur_owner = "__general__".to_string();
    for ac in general {
        g.intern.clear();
        g.sub_sigs.clear();
        g.eq_sigs.clear();
        g.translate_axiom("__general__", ac);
    }

    // Rules run last, one graph for the whole section. A rule's own node comes
    // first, then its body and head lists — one cell and one atom node per atom.
    let mut rules: Vec<&AnnotatedComponent<RcStr>> = model
        .ont
        .iter()
        .filter(|ac| matches!(ac.component, Component::Rule(_)))
        .collect();
    if !rules.is_empty() {
        rules.sort_by_key(|ac| match &ac.component {
            Component::Rule(r) => crate::io::owlrdf::owlapi_rule_key(r),
            _ => unreachable!(),
        });
        g.cur_owner = "__rules__".to_string();
        g.intern.clear();
        for ac in rules {
            if let Component::Rule(r) = &ac.component {
                let id = g.fresh();
                g.rule_ids.push(id);
                g.translate_annotations(&ac.ann);
                g.translate_atom_list(&r.body);
                g.translate_atom_list(&r.head);
            }
        }
    }

    g
}

/// The entity whose rendered block this component becomes part of, when it
/// contributes one — the same set of component kinds the writer's `bodied` test
/// covers. A signature entity with a body gets a section whether or not it is
/// declared and whether or not it is built-in, so this is what decides that a
/// built-in like `rdfs:isDefinedBy` is walked.
fn body_owner(c: &Component<RcStr>) -> Option<String> {
    match c {
        Component::AnnotationAssertion(ax) => match &ax.subject {
            AnnotationSubject::IRI(i) => Some(i.as_ref().to_string()),
            _ => None,
        },
        Component::SubClassOf(ax) => match &ax.sub {
            CE::Class(s) => Some(s.0.as_ref().to_string()),
            _ => None,
        },
        Component::EquivalentClasses(ax) if ax.0.len() == 2 => match (&ax.0[0], &ax.0[1]) {
            (CE::Class(a), _) => Some(a.0.as_ref().to_string()),
            (_, CE::Class(b)) => Some(b.0.as_ref().to_string()),
            _ => None,
        },
        Component::DisjointClasses(ax) if ax.0.len() == 2 => match (&ax.0[0], &ax.0[1]) {
            (CE::Class(a), _) => Some(a.0.as_ref().to_string()),
            (_, CE::Class(b)) => Some(b.0.as_ref().to_string()),
            _ => None,
        },
        Component::DisjointUnion(ax) => Some(ax.0 .0.as_ref().to_string()),
        Component::ClassAssertion(ax) => match (&ax.ce, &ax.i) {
            (CE::Class(_), Individual::Named(i)) => Some(i.0.as_ref().to_string()),
            _ => None,
        },
        Component::SubAnnotationPropertyOf(ax) => Some(ax.sub.0.as_ref().to_string()),
        Component::SubObjectPropertyOf(ax) => match &ax.sub {
            SOPE::ObjectPropertyExpression(sub) => ope_named(sub),
            SOPE::ObjectPropertyChain(_) => None,
        },
        Component::InverseObjectProperties(ax) => match (ope_named(&ax.0), ope_named(&ax.1)) {
            (Some(a), Some(_)) => Some(a),
            _ => None,
        },
        Component::AnnotationPropertyDomain(ax) => Some(ax.ap.0.as_ref().to_string()),
        Component::AnnotationPropertyRange(ax) => Some(ax.ap.0.as_ref().to_string()),
        _ => None,
    }
}

/// A component rendered in the general-axioms section (not attributed to any
/// entity): a GCI (anonymous-subclass SubClassOf / all-anonymous Equivalent or
/// Disjoint classes), a 3+-operand DisjointClasses, or DifferentIndividuals.
fn is_general_axiom(c: &Component<RcStr>) -> bool {
    // A domain/range whose property is an `ObjectInverseOf` has no NAMED subject to
    // file it under, so `owner_iri` yields nothing and it belongs in the general
    // section. Without this arm the axiom reaches neither an entity block nor
    // `general`, the pass never walks it, and the blank node its inverse subject
    // needs goes unallocated: every later genid short by one per such axiom.
    if let Component::ObjectPropertyDomain(ax) = c {
        return ope_named(&ax.ope).is_none();
    }
    if let Component::ObjectPropertyRange(ax) = c {
        return ope_named(&ax.ope).is_none();
    }
    matches!(
        c,
        Component::SubClassOf(_)
            | Component::EquivalentClasses(_)
            | Component::DisjointClasses(_)
            | Component::DisjointUnion(_)
            | Component::DifferentIndividuals(_)
    )
}

impl Genids {
    /// Translate one axiom, assigning genids to its anonymous nodes and, for an
    /// annotated axiom with an anonymous CE object, recording the shared genid.
    fn translate_axiom(&mut self, owner: &str, ac: &AnnotatedComponent<RcStr>) {
        match &ac.component {
            Component::SubClassOf(ax) => {
                let sub = if matches!(ax.sub, CE::Class(_)) {
                    None
                } else {
                    Some(&ax.sub)
                };
                // A SubClassOf super reuses an equiv-intersection operand (the
                // same object → one genid, rendered rdf:nodeID in both) ONLY when
                // the subclass is ANNOTATED: the reification has to point at a
                // named node, which is what makes the two axioms share one. Every
                // one of the 2191 shared restrictions in mondo.owl is annotated. A
                // bare super equal to an operand is a distinct object (rendered
                // inline twice), so it must NOT reuse.
                //
                // A plain super also reuses when ANOTHER SubClassOf on this entity
                // already took a node for the same structure: that is one blank node
                // and one edge, so the twin must not consume a second counter value.
                let sup_sig = ce_sig(&ax.sup);
                // Mirror the WRITER exactly. It skips a plain anonymous super whose
                // structure an ANNOTATED axiom already emitted as `rdf:nodeID` — and
                // that map covers annotated `equivalentClass` targets as well as
                // annotated supers. Reusing only from `sub_sigs` leaves the plain
                // super of an annotated EQUIV operand rendering nothing yet still
                // consuming a counter value — allocating without emitting, which is
                // pure drift.
                //
                // An UNANNOTATED equiv operand is still not a reuse target — the
                // writer renders that pair inline twice, as the note above records.
                // `shared` is a THIS-RUN structural map; `carried_shared` is real
                // provenance — structures that were ONE object in the model this was
                // built from (a shared blank node in the source RDF, or an operand
                // `relax` reused as a derived superclass). Structural equality alone
                // is not identity: a class with an annotated `≡ … ⊓ ∃R.F` and an
                // annotated `⊑ ∃R.F` has TWO `owl:Restriction` blocks unless `relax`
                // has run and made them one object. So an ANNOTATED axiom — which
                // reifies, and so needs its own node — may only take another axiom's
                // node on provenance. Two axioms share one blank node only on
                // IDENTITY, and structurally-equal expressions on one entity are
                // separate objects unless an ANNOTATED axiom is involved:
                // annotated+plain and annotated+annotated share one node, while
                // plain+plain (`SubClassOf` + `EquivalentClasses`), an equivalence
                // operand reused as a `relax` super, and a nested intersection
                // operand each render two inline copies.
                //
                // `carried_shared` is the PREVIOUS pass's `shared` map, which records
                // annotated-axiom targets and is matched by STRUCTURAL hash. Left
                // ungated it fires on every plain twin `relax`/`materialize` created
                // — 51,425 of them on `oba-full.owl`, against 3 nodes that are
                // genuinely shared. `owner_shared_in_source` stays ungated: it is
                // real evidence of one blank node shared in the input DOCUMENT, and
                // re-reading that document yields one object for both axioms.
                let carried_here = self
                    .carried_shared
                    .contains(&crate::io::anon_sig_hash(&sup_sig))
                    && !self.carried_used.contains(&sup_sig);
                let shared_here = (ac.ann.is_empty()
                    && self.shared.get(owner).is_some_and(|m| m.contains_key(&sup_sig)))
                    || carried_here
                    || self.owner_shared_in_source.contains("*")
                    || shared_key(&ax.sup)
                        .is_some_and(|k| self.owner_shared_in_source.contains(&k));
                let c_sub = self.sub_sigs.contains(&sup_sig);
                let c_carried = self.carried_shared.contains(&crate::io::anon_sig_hash(&sup_sig));
                let c_star = self.owner_shared_in_source.contains("*");
                let c_key = shared_key(&ax.sup)
                    .is_some_and(|k| self.owner_shared_in_source.contains(&k));
                let c_thisrun = ac.ann.is_empty()
                    && self.shared.get(owner).is_some_and(|m| m.contains_key(&sup_sig));
                let c_ann = ac.ann.is_empty() && self.annotated_sigs.contains(&sup_sig);
                if c_sub { self.by_clause[0] += 1; }
                if c_thisrun { self.by_clause[1] += 1; }
                if carried_here { self.by_clause[2] += 1; }
                if c_star { self.by_clause[3] += 1; }
                if c_key { self.by_clause[4] += 1; }
                if c_ann { self.by_clause[5] += 1; }
                let reuse = !self.desharded_sigs.contains(&sup_sig)
                    && (self.sub_sigs.contains(&sup_sig)
                        || shared_here
                        || (ac.ann.is_empty() && self.annotated_sigs.contains(&sup_sig)));
                // Record the DECISION, not just an intern hit. The writer needs to
                // know this node is shared so a structurally-equal operand renders
                // as a reference to it; whether the id came from `intern` or was
                // freshly allocated here is beside the point.
                if carried_here {
                    self.carried_used.insert(sup_sig.clone());
                }
                if reuse && !matches!(ax.sup, CE::Class(_)) {
                    self.reused.entry(owner.to_string()).or_default().insert(sup_sig.clone());
                }
                // A `spanGaps` re-link shares one blank node with every other
                // re-link made from the same source expression.
                self.span_pending = if self.span_shared.is_empty() {
                    None
                } else {
                    self.span_shared.get(&format!("{}\u{1}{}", owner, sup_sig)).copied()
                };
                // Reset per axiom: an intern-reuse returns before the take(),
                // and a stale pending group must never leak into the NEXT
                // axiom's translation. Only an ANNOTATED member takes the
                // group's node: a reification must point at a labeled node, and
                // structurally-equal reified targets of one minted object share
                // it across owners. A BARE member renders its own inline copy —
                // one anonymous node per edge, fresh numbering — exactly as the
                // reference files do for bare minted edges.
                self.cross_pending = None;
                if self.span_pending.is_none() && !self.cross_shared.is_empty() {
                    if let Some(k) = shared_key(&ax.sup) {
                        let group =
                            self.cross_shared.get(&format!("{}\u{1}{}", owner, k)).copied();
                        // One node however many classes carry it. An ANNOTATED
                        // member must also be LABELLED, because a reification has
                        // to point at a named node; a bare one renders its own
                        // inline copy — each entity references it once — and only
                        // the numbering is shared, which is what `span_pending`
                        // does. EFO's `uberon_import.owl` turns on this: the source
                        // gives UBERON_0000011 and UBERON_0000013 one
                        // `part_of some UBERON_0002410` between them, and taking
                        // two ids there moves every later blank node along by one.
                        if ac.ann.is_empty() {
                            self.span_pending = group;
                        } else {
                            self.cross_pending = group;
                        }
                    }
                }
                if let Some(id) =
                    self.single_triple_ce_reif(sub, &ax.sup, &ac.ann, reuse, Some(P_SUBCLASS))
                {
                    if !matches!(ax.sup, CE::Class(_)) {
                        // Record in BOTH: `sub_sigs` gates whether a bare super may
                        // reuse at all (an equiv operand must not be reused by one),
                        // while `intern` is what `translate_ce_maybe_reuse` actually
                        // looks the id up in. Setting the gate without the entry
                        // leaves `reuse = true` finding nothing and allocating
                        // anyway.
                        self.sub_sigs.insert(sup_sig.clone());
                        self.intern.entry(sup_sig).or_insert(id);
                    }
                    if !ac.ann.is_empty() {
                        self.record_shared(owner, &ax.sup, id);
                    }
                }
            }
            Component::EquivalentClasses(ax) => {
                // An equivalence that repeats one structure inside itself has a
                // fresh object per occurrence, so nothing else may reuse their
                // blank nodes — see `has_shared_structure`.
                let desharded = has_shared_structure(&ac.component);
                self.record_operands = !desharded;
                self.pairwise_ce(owner, &ax.0, &ac.ann, Some(P_EQUIV));
                self.record_operands = false;
            }
            Component::DisjointClasses(ax) => {
                if ax.0.len() == 2 {
                    self.pairwise_ce(owner, &ax.0, &ac.ann, Some(P_DISJOINT));
                } else {
                    // AllDisjointClasses: node, then members list, then anns.
                    self.fresh();
                    self.translate_ce_list(&ax.0, false);
                    self.translate_annotations(&ac.ann);
                }
            }
            Component::DisjointUnion(ax) => {
                // subject = named class, object = list of expressions.
                self.single_triple_list(&ax.1, &ac.ann);
            }
            // `P rdfs:range C` / `P rdfs:domain C`: the SUBJECT is the property
            // expression, and an `ObjectInverseOf` subject is a blank node of its
            // own, allocated before the object — a single-triple axiom resolves its
            // subject node first. Hence the explicit `translate_ope` call: the
            // subject passed to `single_triple_ce_reif` is `None`, so nothing else
            // would allocate that node and every genid after an inverse domain or
            // range would be short by one.
            // `i rdf:type C`: an anonymous C takes a genid (and its nested nodes
            // theirs), a named one takes none.
            Component::ClassAssertion(ax) => {
                self.translate_ce(&ax.ce);
            }
            Component::ObjectPropertyRange(ax) => {
                self.translate_ope(&ax.ope);
                if let Some(id) =
                    self.single_triple_ce_reif(None, &ax.ce, &ac.ann, false, Some(P_RANGE))
                {
                    if !ac.ann.is_empty() {
                        self.record_shared(owner, &ax.ce, id);
                    }
                }
            }
            Component::ObjectPropertyDomain(ax) => {
                self.translate_ope(&ax.ope);
                if let Some(id) =
                    self.single_triple_ce_reif(None, &ax.ce, &ac.ann, false, Some(P_DOMAIN))
                {
                    if !ac.ann.is_empty() {
                        self.record_shared(owner, &ax.ce, id);
                    }
                }
            }
            Component::SubObjectPropertyOf(ax) => {
                if let SOPE::ObjectPropertyChain(chain) = &ax.sub {
                    // superProperty propertyChainAxiom (chain list).
                    let mut only_named = true;
                    for m in chain {
                        if let OPE::InverseObjectProperty(_) = m {
                            only_named = false;
                        }
                    }
                    let _ = only_named;
                    // list of properties (each named → no node, but list cells count)
                    self.translate_ope_list(chain);
                    if !ac.ann.is_empty() {
                        // An annotated chain reifies to an `owl:Axiom` node whose
                        // target is the chain list. Record its signature like an
                        // annotated assertion's, so the writer can place the block
                        // by node id: without it the block has no id to sort on and
                        // falls to the end of the entity, putting RO's annotated
                        // `RO_0002432` chain after its `IAO_0000115` definition,
                        // when axiom-type order — chain (25) before annotation
                        // assertion (34) — puts it first.
                        let rid = self.fresh();
                        let mut members = String::new();
                        for m in chain {
                            // Only a NAMED member carries `rdf:about`; an inverse is
                            // an anonymous node, which `reif_signature` skips.
                            if let OPE::ObjectProperty(p) = m {
                                members.push_str(&crate::io::owlrdf::esc_attr(p.0.as_ref()));
                                members.push('\u{2}');
                            }
                        }
                        let sig = format!(
                            "http://www.w3.org/2002/07/owl#propertyChainAxiom\u{1}C\u{1}{members}"
                        );
                        self.reif.entry(self.cur_owner.clone()).or_default().push((sig, rid));
                        self.translate_annotations(&ac.ann);
                    }
                } else {
                    // `sub rdfs:subPropertyOf super`. Either side may be an
                    // `ObjectInverseOf`, which is an anonymous node of its own —
                    // RO's `RO_0002378 ⊑ inverse(RO_0002376)` is one, and the id
                    // it takes shifts every later node in the document.
                    if let SOPE::ObjectPropertyExpression(sub) = &ax.sub {
                        self.translate_ope(sub);
                    }
                    self.translate_ope(&ax.sup);
                    if !ac.ann.is_empty() {
                        self.fresh();
                        self.translate_annotations(&ac.ann);
                    }
                }
            }
            Component::AnnotationAssertion(ax) => {
                // Annotation-assertion values are literals/IRIs (no CE node); an
                // annotated assertion reifies to an owl:Axiom node. Record its
                // (property ⊕ value) signature so the writer can order the block.
                if !ac.ann.is_empty() {
                    let rid = self.fresh();
                    let prop = crate::io::owlrdf::esc_attr(ax.ann.ap.0.as_ref());
                    let sig = format!("{prop}\u{1}{}", ann_value_tsig(&ax.ann.av));
                    self.reif.entry(self.cur_owner.clone()).or_default().push((sig, rid));
                    self.translate_annotations(&ac.ann);
                }
            }
            // `P rdfs:range <data range>` / `rdfs:domain`: the range may be a whole
            // subtree, and RO's `RO_0002029` is one — an `rdfs:Datatype` node with
            // two `owl:withRestrictions` cells and two facet nodes, five ids in
            // all, so the range itself has to be walked and not just the
            // reification node.
            Component::DataPropertyRange(ax) => {
                self.translate_dr(&ax.dr);
                if !ac.ann.is_empty() {
                    self.fresh();
                    self.translate_annotations(&ac.ann);
                }
            }
            Component::DatatypeDefinition(ax) => {
                self.translate_dr(&ax.range);
                if !ac.ann.is_empty() {
                    self.fresh();
                    self.translate_annotations(&ac.ann);
                }
            }
            // A `DifferentIndividuals` of three or more members is one
            // `owl:AllDifferent` node carrying an `owl:distinctMembers` list, so it
            // costs one id for the axiom node plus one per list cell. Two members
            // are written as a single `owl:differentFrom` edge instead, which costs
            // nothing but the reification node an annotated axiom needs.
            Component::DifferentIndividuals(ax) => {
                if ax.0.len() > 2 {
                    self.fresh();
                    self.translate_individual_list(&ax.0);
                    self.translate_annotations(&ac.ann);
                } else if !ac.ann.is_empty() {
                    self.fresh();
                    self.translate_annotations(&ac.ann);
                }
            }
            // An equivalence between two inverse expressions is its own anonymous
            // block after the frame of the property the first inverse names:
            // `_:a owl:inverseOf P . _:a owl:equivalentProperty _:b . _:b
            // owl:inverseOf Q` — two nodes, subject then object, in the axiom's
            // member order. A named pair renders inside the first property's
            // frame and takes no node.
            Component::EquivalentObjectProperties(ax) => {
                if ax.0.len() == 2 {
                    if let (OPE::InverseObjectProperty(_), OPE::InverseObjectProperty(_)) =
                        (&ax.0[0], &ax.0[1])
                    {
                        self.fresh();
                        self.fresh();
                    }
                }
                if !ac.ann.is_empty() {
                    self.fresh();
                    self.translate_annotations(&ac.ann);
                }
            }
            // Property characteristics / inverse / declarations: named-only, so
            // only a reification node when annotated.
            _ => {
                if !ac.ann.is_empty() {
                    self.fresh();
                    self.translate_annotations(&ac.ann);
                }
            }
        }
    }

    /// An RDF list of individuals — cells from the last sorted element back to
    /// the first, each cell taking an id before its member is translated.
    fn translate_individual_list(&mut self, inds: &[Individual<RcStr>]) {
        let mut sorted: Vec<&Individual<RcStr>> = inds.iter().collect();
        sorted.sort_by(|a, b| cmp_individual(a, b));
        for i in (0..sorted.len()).rev() {
            self.fresh(); // list cell
            self.translate_individual(sorted[i]);
        }
    }

    /// A SWRL body/head atom list: cells from the last atom back to the first,
    /// each cell taking an id before the atom node it carries.
    fn translate_atom_list(&mut self, atoms: &[horned_owl::model::Atom<RcStr>]) {
        for i in (0..atoms.len()).rev() {
            self.fresh(); // list cell
            self.fresh(); // atom node
            self.translate_atom_parts(&atoms[i]);
        }
    }

    /// The anonymous parts an atom's predicate and arguments can carry. Every
    /// atom in an ODK ontology names its predicate and takes variables, so this is
    /// usually nothing; an anonymous class predicate or individual argument is
    /// still a node of its own.
    fn translate_atom_parts(&mut self, atom: &horned_owl::model::Atom<RcStr>) {
        use horned_owl::model::{Atom, DArgument, IArgument};
        let iarg = |g: &mut Self, a: &IArgument<RcStr>| {
            if let IArgument::Individual(i) = a {
                g.translate_individual(i);
            }
        };
        match atom {
            Atom::ClassAtom { pred, arg } => {
                if !matches!(pred, CE::Class(_)) {
                    self.translate_ce(pred);
                }
                iarg(self, arg);
            }
            Atom::DataRangeAtom { pred, arg } => {
                self.translate_dr(pred);
                let _ = arg;
            }
            Atom::ObjectPropertyAtom { pred, args } => {
                self.translate_ope(pred);
                iarg(self, &args.0);
                iarg(self, &args.1);
            }
            Atom::DataPropertyAtom { .. } => {}
            Atom::BuiltInAtom { args, .. } => {
                // `swrl:arguments` is an RDF list, one cell per argument.
                for a in args.iter().rev() {
                    self.fresh();
                    let _: &DArgument<RcStr> = a;
                }
            }
            Atom::SameIndividualAtom(a, b) | Atom::DifferentIndividualsAtom(a, b) => {
                iarg(self, a);
                iarg(self, b);
            }
        }
    }

    fn translate_ope_list(&mut self, chain: &[OPE<RcStr>]) {
        // Property chains are NOT sorted (order-significant); cells back-to-front.
        for i in (0..chain.len()).rev() {
            self.fresh(); // list cell
            self.translate_ope(&chain[i]);
        }
    }

    fn single_triple_list(
        &mut self,
        list: &[CE<RcStr>],
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
    ) {
        self.translate_ce_list(list, false);
        if !anns.is_empty() {
            self.fresh();
            self.translate_annotations(anns);
        }
    }

    /// Pairwise expansion over class expressions: sort, then for each i<j pair emit
    /// a single-triple axiom (subject = ops[i], object = ops[j]).
    fn pairwise_ce(
        &mut self,
        owner: &str,
        ops: &[CE<RcStr>],
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
        reif_prop: Option<&str>,
    ) {
        let mut sorted: Vec<&CE<RcStr>> = ops.iter().collect();
        sorted.sort_by(|a, b| cmp_ce(a, b));
        // Every member of the equivalence is relaxed, so every member's conjuncts
        // are reuse targets. Translating one consumes the flag, so re-arm it.
        let record = self.record_operands;
        for i in 0..sorted.len() {
            for j in (i + 1)..sorted.len() {
                self.record_operands = record;
                let subj = if matches!(sorted[i], CE::Class(_)) {
                    None
                } else {
                    Some(sorted[i])
                };
                // Same rule as SubClassOf supers: two axioms over a structurally-equal
                // object are ONE blank node and ONE triple, so the second must reuse
                // rather than burn a counter value. MONDO carries duplicate
                // `EquivalentClasses` axioms — MONDO_0000009's genus-differentia
                // block twice over — which the writer emits once.
                let objsig = ce_sig(sorted[j]);
                let reuse = self.eq_sigs.contains(&objsig)
                    || self.annotated_sigs.contains(&objsig)
                    || self.carried_shared.contains(&crate::io::anon_sig_hash(&objsig));
                if let Some(id) =
                    self.single_triple_ce_reif(subj, sorted[j], anns, reuse, reif_prop)
                {
                    if !matches!(sorted[j], CE::Class(_)) {
                        self.eq_sigs.insert(objsig.clone());
                        self.intern.entry(objsig).or_insert(id);
                    }
                    if !anns.is_empty() {
                        self.record_shared(owner, sorted[j], id);
                    }
                }
            }
        }
    }

    fn record_shared(&mut self, owner: &str, ce: &CE<RcStr>, id: u64) {
        let sig = ce_sig(ce);
        // Also make it reusable: a shared node is one the writer emits as
        // `rdf:nodeID`, so a later axiom over the same structure renders nothing and
        // must resolve to this id rather than allocate. `intern` is where
        // `translate_ce_maybe_reuse` looks.
        self.intern.entry(sig.clone()).or_insert(id);
        // A node this owner's body shares with itself: publish its id under an
        // OWNER-QUALIFIED key, so the reuse above resolves within this class and
        // cannot reach across to another one (see the note there).
        if let Some(k) = shared_key(ce) {
            if self.owner_shared_in_source.contains(&k) {
                self.doc_shared_intern
                    .entry(format!("{}\u{1}{k}", self.cur_owner))
                    .or_insert(id);
            }
        }
        self.shared_seq.entry(owner.to_string()).or_default().push((sig.clone(), id));
        self.shared.entry(owner.to_string()).or_default().insert(sig, id);
    }
}

/// Owning entity IRI for an axiom: the entity whose block it renders inside.
fn owner_iri(c: &Component<RcStr>) -> Option<String> {
    match c {
        Component::SubClassOf(ax) => match &ax.sub {
            CE::Class(s) => Some(s.0.as_ref().to_string()),
            _ => None,
        },
        Component::EquivalentClasses(ax) => first_named_min(&ax.0),
        Component::DisjointClasses(ax) => {
            if ax.0.len() > 2 {
                None
            } else {
                first_named_min(&ax.0)
            }
        }
        Component::DisjointUnion(ax) => Some(ax.0 .0.as_ref().to_string()),
        Component::ObjectPropertyRange(ax) => ope_named(&ax.ope),
        Component::ObjectPropertyDomain(ax) => ope_named(&ax.ope),
        Component::SubObjectPropertyOf(ax) => match &ax.sub {
            SOPE::ObjectPropertyExpression(OPE::ObjectProperty(p)) => {
                Some(p.0.as_ref().to_string())
            }
            SOPE::ObjectPropertyChain(_) => ope_named(&ax.sup),
            _ => None,
        },
        Component::TransitiveObjectProperty(ax) => ope_named(&ax.0),
        Component::FunctionalObjectProperty(ax) => ope_named(&ax.0),
        Component::InverseFunctionalObjectProperty(ax) => ope_named(&ax.0),
        Component::SymmetricObjectProperty(ax) => ope_named(&ax.0),
        Component::AsymmetricObjectProperty(ax) => ope_named(&ax.0),
        Component::ReflexiveObjectProperty(ax) => ope_named(&ax.0),
        Component::IrreflexiveObjectProperty(ax) => ope_named(&ax.0),
        Component::InverseObjectProperties(ax) => ope_named(&ax.0),
        Component::SubAnnotationPropertyOf(ax) => Some(ax.sub.0.as_ref().to_string()),
        Component::AnnotationAssertion(ax) => match &ax.subject {
            AnnotationSubject::IRI(i) => Some(i.as_ref().to_string()),
            _ => None,
        },
        // DATA properties need arms here too: the writer renders their axioms
        // inside the property's block, but an axiom that falls through to `None`
        // is not a general axiom either, so nothing would walk it. RO's
        // `RO_0002029 rdfs:range` is a five-node datatype restriction that would
        // go uncounted.
        Component::DataPropertyDomain(ax) => Some(ax.dp.0.as_ref().to_string()),
        Component::DataPropertyRange(ax) => Some(ax.dp.0.as_ref().to_string()),
        Component::FunctionalDataProperty(ax) => Some(ax.0 .0.as_ref().to_string()),
        Component::SubDataPropertyOf(ax) => Some(ax.sub.0.as_ref().to_string()),
        Component::EquivalentDataProperties(ax) => {
            ax.0.iter().map(|p| p.0.as_ref().to_string()).min()
        }
        Component::DisjointDataProperties(ax) => {
            ax.0.iter().map(|p| p.0.as_ref().to_string()).min()
        }
        // A pair equivalence renders off the FIRST member: inside its frame when
        // named, as an anonymous two-node block after the frame of the property
        // its inverse names — either way that property's walk numbers it.
        Component::EquivalentObjectProperties(ax) if ax.0.len() == 2 => match &ax.0[0] {
            OPE::ObjectProperty(a) => Some(a.0.as_ref().to_string()),
            OPE::InverseObjectProperty(a) => Some(a.0.as_ref().to_string()),
        },
        Component::DatatypeDefinition(ax) => Some(ax.kind.0.as_ref().to_string()),
        // A class assertion is rendered inside the individual's own block, so it
        // is numbered there. An ANONYMOUS type is a blank node: CL's brain-atlas
        // components assert 199 of them, and leaving them out of the walk left
        // every later blank node numbered 199 too low.
        Component::ClassAssertion(ax) => match &ax.i {
            Individual::Named(i) => Some(i.0.as_ref().to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn ope_named(ope: &OPE<RcStr>) -> Option<String> {
    match ope {
        OPE::ObjectProperty(p) => Some(p.0.as_ref().to_string()),
        _ => None,
    }
}

fn first_named_min(ops: &[CE<RcStr>]) -> Option<String> {
    ops.iter()
        .filter_map(|ce| match ce {
            CE::Class(c) => Some(c.0.as_ref().to_string()),
            _ => None,
        })
        .min()
}

/// The `property\u{1}filler` key used to match a class expression against the
/// repeated `rdf:nodeID`s scanned out of the RDF source. Only the plain
/// `R some NamedClass` shape is keyed — the one OBO restrictions take.
pub(crate) fn shared_key(ce: &CE<RcStr>) -> Option<String> {
    match ce {
        CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(p), bce } => match &**bce {
            CE::Class(c) => Some(format!("{}\u{1}{}", p.0.as_ref(), c.0.as_ref())),
            _ => None,
        },
        _ => None,
    }
}

/// A structural signature for a class expression, stable across the pre-pass and
/// the writer, used to look up a shared node's genid.
pub fn ce_sig(ce: &CE<RcStr>) -> String {
    format!("{ce:?}")
}
