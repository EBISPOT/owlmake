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

/// The key of the ontology header's graph.
pub const HEADER_GRAPH: &str = "\u{1}header";
/// The prefix of the key of an anonymous individual's own graph.
pub const ANON_GRAPH: &str = "\u{1}anon\u{1}";
/// The prefix of the key of a general axiom's graph, before its identity.
pub const GENERAL_GRAPH: &str = "\u{1}general\u{1}";
/// The key of the rules' graph.
pub const RULES_GRAPH: &str = "\u{1}rules";

// annotatedProperty IRIs for edge reifications, matching the writer's output.
const P_SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const P_EQUIV: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const P_DISJOINT: &str = "http://www.w3.org/2002/07/owl#disjointWith";
const P_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const P_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const P_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const P_SUB_PROPERTY: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const P_INVERSE_OF: &str = "http://www.w3.org/2002/07/owl#inverseOf";
const P_EQUIV_PROPERTY: &str = "http://www.w3.org/2002/07/owl#equivalentProperty";
const P_PROPERTY_DISJOINT: &str = "http://www.w3.org/2002/07/owl#propertyDisjointWith";

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

/// A stable identity for an axiom as a structure, so a step that rebuilds some
/// axioms can name them to the numbering pass (`Model::shared_occurrences`).
/// Every set-valued position — the members of an n-ary axiom, the operands of
/// an intersection or union — is put in one order first, so the identity does
/// not depend on the order a step happened to build them in.
pub fn axiom_identity(ac: &AnnotatedComponent<RcStr>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canonical_component(&ac.component).hash(&mut h);
    ac.ann.hash(&mut h);
    h.finish()
}

/// `ce` with every operand list in signature order, recursively.
fn canonical_ce(ce: &CE<RcStr>) -> CE<RcStr> {
    let sorted = |ops: &Vec<CE<RcStr>>| -> Vec<CE<RcStr>> {
        let mut v: Vec<CE<RcStr>> = ops.iter().map(canonical_ce).collect();
        v.sort_by(|a, b| cmp_ce(a, b));
        v
    };
    match ce {
        CE::ObjectIntersectionOf(ops) => CE::ObjectIntersectionOf(sorted(ops)),
        CE::ObjectUnionOf(ops) => CE::ObjectUnionOf(sorted(ops)),
        CE::ObjectComplementOf(b) => CE::ObjectComplementOf(Box::new(canonical_ce(b))),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            CE::ObjectSomeValuesFrom { ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            CE::ObjectAllValuesFrom { ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectMinCardinality { n, ope, bce } => {
            CE::ObjectMinCardinality { n: *n, ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            CE::ObjectMaxCardinality { n: *n, ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectExactCardinality { n, ope, bce } => {
            CE::ObjectExactCardinality { n: *n, ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        other => other.clone(),
    }
}

/// `c` with every class expression in canonical form and every n-ary member
/// list in signature order.
fn canonical_component(c: &Component<RcStr>) -> Component<RcStr> {
    use horned_owl::model::{DisjointClasses, EquivalentClasses, SubClassOf};
    let sorted = |v: &Vec<CE<RcStr>>| -> Vec<CE<RcStr>> {
        let mut out: Vec<CE<RcStr>> = v.iter().map(canonical_ce).collect();
        out.sort_by(|a, b| cmp_ce(a, b));
        out
    };
    match c {
        Component::SubClassOf(ax) => Component::SubClassOf(SubClassOf {
            sub: canonical_ce(&ax.sub),
            sup: canonical_ce(&ax.sup),
        }),
        Component::EquivalentClasses(ax) => Component::EquivalentClasses(EquivalentClasses(sorted(&ax.0))),
        Component::DisjointClasses(ax) => Component::DisjointClasses(DisjointClasses(sorted(&ax.0))),
        other => other.clone(),
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

/// The group of every recorded node of an axiom, by signature hash: each
/// recorded node takes its own group, and every anonymous node inside it is
/// one object with the same node wherever that node's object reaches, so it
/// takes [`crate::model::descendant_group`] of the node's object. Within an
/// axiom that is numbered from its record a structure occurs once — one that
/// occurs twice makes the whole axiom a copy — so a signature names one node.
pub(crate) fn shared_groups(
    nodes: &[crate::model::SharedNode],
    c: &Component<RcStr>,
) -> HashMap<u64, u64> {
    fn walk(ce: &CE<RcStr>, nodes: &[crate::model::SharedNode], out: &mut HashMap<u64, u64>) {
        if matches!(ce, CE::Class(_)) {
            return;
        }
        let sig = crate::io::anon_sig_hash(&ce_sig(ce));
        if let Some(n) = nodes.iter().find(|n| n.sig == sig) {
            out.insert(sig, n.node_group());
            for d in anonymous_descendants(ce) {
                let ds = crate::io::anon_sig_hash(&ce_sig(d));
                out.insert(ds, crate::model::descendant_group(n.group, ds));
            }
            return;
        }
        for sub in sub_expressions(ce) {
            walk(sub, nodes, out);
        }
    }
    let mut out = HashMap::new();
    for ce in component_class_expressions(c) {
        walk(ce, nodes, &mut out);
    }
    out
}

/// Every anonymous class expression nested inside `ce`, in walk order; `ce`
/// itself is not included.
pub(crate) fn anonymous_descendants(ce: &CE<RcStr>) -> Vec<&CE<RcStr>> {
    let mut out = Vec::new();
    fn walk<'a>(ce: &'a CE<RcStr>, out: &mut Vec<&'a CE<RcStr>>) {
        for sub in sub_expressions(ce) {
            if !matches!(sub, CE::Class(_)) {
                out.push(sub);
                walk(sub, out);
            }
        }
    }
    walk(ce, &mut out);
    out
}

/// The direct class-expression children of a class expression.
pub(crate) fn sub_expressions(ce: &CE<RcStr>) -> Vec<&CE<RcStr>> {
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

/// [`cmp_axiom`], with two axioms that differ only in their annotations ordered
/// by them.
fn cmp_annotated_axiom(a: &AnnotatedComponent<RcStr>, b: &AnnotatedComponent<RcStr>) -> Ordering {
    cmp_axiom(&a.component, &b.component).then_with(|| {
        let list = |anns: &std::collections::BTreeSet<Annotation<RcStr>>| {
            let mut v: Vec<(String, AnnotationValue<RcStr>)> =
                anns.iter().map(|x| (x.ap.0.as_ref().to_string(), x.av.clone())).collect();
            v.sort_by_key(|x| crate::io::owlrdf::ann_key(&x.0, &x.1));
            v
        };
        crate::io::owlrdf::cmp_ann_list(&list(&a.ann), &list(&b.ann))
    })
}

/// The result of the numbering pass.
#[derive(Default)]
pub struct Genids {
    /// Final counter value (total anonymous nodes + 1).
    pub counter: u64,
    /// The anonymous individuals already given their node: one per individual
    /// for the whole document, wherever it first appears.
    seen_anon: std::collections::HashSet<String>,
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
    /// `Model::shared_occurrences`: per axiom, the expressions in it that are
    /// one object with every other recorded occurrence.
    shared_occurrences: HashMap<u64, Vec<crate::model::SharedNode>>,
    /// The shared occurrences of the axiom being translated: signature hash to
    /// group.
    axiom_shared: HashMap<u64, u64>,
    /// Whether `OM_SUBTREE_DEBUG` asks for every shared-occurrence event.
    subtree_debug: bool,
    /// For each shared group translated so far: its node, and the graph
    /// (`graph_seq`) that last reached it.
    subtree_nodes: HashMap<u64, (u64, u64)>,
    /// The graph being translated. Each entity's block is one graph, and so is
    /// each general axiom: an object reached again within a graph is not
    /// translated again, while the next graph re-walks it and re-spends its
    /// list cells around the nodes it already has.
    graph_seq: u64,
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
    /// Signatures whose intern id was minted by a NESTED operand walk (an
    /// equivalence intersection member) rather than by an axiom-level target.
    /// A `SubClassOf` super never takes such a node: each axiom owns its
    /// expression's blank node, so the super mints a fresh one and the operand
    /// keeps rendering inline. Cleared for a signature once an axiom-level
    /// target claims it with its own id.
    operand_minted: std::collections::HashSet<String>,
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
    /// Every reified axiom node whose annotations carry annotations of their
    /// own, with those annotations and the `owl:Annotation` nodes they take.
    pub nested: HashMap<u64, NestedAnnotations>,
    /// The ontology's annotations that carry annotations of their own, each
    /// with the `owl:Annotation` nodes it takes.
    pub ontology_nested: Vec<(Annotation<RcStr>, Vec<u64>)>,
    /// For each general axiom some of whose nodes are in `nested`, by its
    /// `axiom_identity`, those nodes.
    pub general_nested: HashMap<u64, Vec<u64>>,
    /// The `axiom_identity` of every axiom whose annotations of annotations
    /// are in `nested`.
    pub nested_axioms: std::collections::HashSet<u64>,
    /// The `axiom_identity` of the axiom being numbered, when its annotations
    /// carry annotations of their own.
    cur_axiom: Option<u64>,
    /// The axioms about each anonymous individual, in the order they are
    /// numbered where it is first reached: those it is the subject of — a class
    /// assertion, a property assertion or negative assertion, a sameness or
    /// difference it is a member of — then the annotation assertions about it.
    anon_axioms: HashMap<String, Vec<(u64, AnnotatedComponent<RcStr>)>>,
    /// The `axiom_identity` of every axiom in `anon_axioms`.
    reachable: std::collections::HashSet<u64>,
    /// Those of them already numbered.
    reached: std::collections::HashSet<u64>,
    /// The anonymous individuals being reached, innermost last.
    reaching: Vec<String>,
    /// For each axiom in `anon_axioms` (by `axiom_identity`), the graph it is
    /// numbered in: the graph that first reaches its individual.
    pub anon_home: HashMap<u64, String>,
    /// The axioms of `anon_home`, in the order they were numbered.
    pub anon_order: Vec<u64>,
    /// The node of each anonymous individual.
    pub anon_ids: HashMap<String, u64>,
    /// For each axiom that names an anonymous individual (by
    /// `axiom_identity`), the nodes of its reifications, or of the negative
    /// assertion or `owl:AllDifferent` it is, in order.
    pub anon_reif: HashMap<u64, Vec<u64>>,
    /// Whether any axiom names an anonymous individual.
    anon_present: bool,
    /// The graph being numbered: an entity's IRI, [`HEADER_GRAPH`],
    /// [`ANON_GRAPH`] and an individual, [`GENERAL_GRAPH`] and an axiom's
    /// identity, or [`RULES_GRAPH`].
    pub cur_graph: String,
    /// The graphs, in the order they are numbered and written.
    pub graphs: Vec<String>,
    /// For each graph and anonymous individual, how many of the graph's
    /// statements have the individual as their object.
    pub anon_objects: HashMap<(String, String), u32>,
    /// The first node of each general axiom, by its `axiom_identity`: the
    /// root of its graph.
    pub general_root: HashMap<u64, u64>,
    /// The node of the class expression of each annotated class assertion of
    /// an anonymous class about an anonymous individual, by its
    /// `axiom_identity`.
    pub anon_ce: HashMap<u64, u64>,
    /// The axiom being numbered, when it names an anonymous individual.
    cur_anon_axiom: Option<u64>,
    /// The anonymous individuals that are graphs of their own, in the order
    /// the anonymous section states them.
    pub anon_roots: Vec<String>,
    /// The nodes annotated annotations have taken, in the order numbered.
    annotation_nodes: Vec<u64>,
}

/// The annotations on one node that carry annotations of their own: each
/// such annotation is an `owl:Annotation` node, and `ids` are those nodes in
/// the order they were numbered — an annotation before the ones on it, the
/// annotations of a node in order.
#[derive(Clone, Debug, Default)]
pub struct NestedAnnotations {
    pub anns: std::collections::BTreeSet<Annotation<RcStr>>,
    pub ids: Vec<u64>,
    /// The nodes of the later pairs of an axiom written as pairs, in order:
    /// its pairs carry the same annotations, which are one set of nodes, each
    /// annotating every pair.
    pub sources: Vec<u64>,
}

impl Genids {
    /// The axioms about anonymous individuals that no graph reached.
    pub fn unreached(&self) -> impl Iterator<Item = u64> + '_ {
        self.reachable.iter().copied().filter(|k| !self.reached.contains(k))
    }

    /// Whether the axiom `key` is about an anonymous individual, and numbered
    /// where the individual is first reached.
    pub fn is_reachable(&self, key: u64) -> bool {
        self.reachable.contains(&key)
    }

    /// Start numbering the graph `key`.
    fn begin_graph(&mut self, key: String) {
        self.graphs.push(key.clone());
        self.cur_graph = key;
    }

    /// A statement of the current graph has the individual `i` as its object.
    fn object(&mut self, i: &Individual<RcStr>) {
        if let Individual::Anonymous(a) = i {
            self.object_anon(a.0.as_ref());
        }
    }

    fn object_anon(&mut self, x: &str) {
        *self.anon_objects.entry((self.cur_graph.clone(), x.to_string())).or_default() += 1;
    }

    /// The next id for an anonymous node.
    fn fresh(&mut self) -> u64 {
        let v = self.counter;
        self.counter += 1;
        if self.trace.as_deref() == Some(self.cur_owner.as_str()) {
            eprintln!("  [trace {}] genid{v}", self.cur_owner);
        }
        v
    }

    /// The next id for an RDF list cell. A cell belongs to one rendering of one
    /// collection: every rendering takes new cells around whatever nodes it
    /// reuses.
    fn fresh_cell(&mut self) -> u64 {
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
        if let Individual::Anonymous(a) = ind {
            self.translate_anonymous(a.0.as_ref());
        }
    }

    /// An anonymous individual's node, taken where the individual first appears.
    fn translate_anonymous(&mut self, id: &str) {
        if self.seen_anon.insert(id.to_string()) {
            let node = self.fresh();
            self.anon_ids.insert(id.to_string(), node);
        }
    }

    /// Reach the anonymous individual `x` in the graph being numbered: every
    /// axiom about it not yet numbered is numbered here, but `from`, the axiom
    /// that reached it.
    fn reach(&mut self, x: &str, from: Option<u64>) {
        if self.reaching.iter().any(|r| r == x) {
            return;
        }
        let Some(axioms) = self.anon_axioms.get(x).cloned() else { return };
        self.reaching.push(x.to_string());
        let owner = self.cur_owner.clone();
        for (key, ac) in &axioms {
            if Some(*key) != from {
                self.translate_axiom(&owner, ac);
            }
        }
        self.reaching.pop();
    }

    /// An RDF list of class expressions — cells built from the LAST sorted element
    /// to the first; each cell takes an id, then its element is translated. When
    /// `record`, each anonymous operand's genid is recorded for later
    /// `SubClassOf`-super reuse.
    fn translate_ce_list(&mut self, ops: &[CE<RcStr>], record: bool) -> Option<u64> {
        let mut sorted: Vec<&CE<RcStr>> = ops.iter().collect();
        sorted.sort_by(|a, b| cmp_ce(a, b));
        let mut head = None;
        for i in (0..sorted.len()).rev() {
            head = Some(self.fresh_cell()); // list cell
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
                    let sig = ce_sig(sorted[i]);
                    if !self.intern.contains_key(&sig) {
                        self.operand_minted.insert(sig.clone());
                    }
                    self.intern.entry(sig).or_insert(oid);
                }
            }
        }
        head
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
                    self.fresh_cell();
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
            self.fresh_cell(); // list cell
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
        // A shared occurrence is one object however many axioms and owners
        // hold it, so it has one node. The graph that first reaches it
        // translates it; a later graph walks it again and spends its list cells
        // around the nodes it already has; the same graph reaching it twice
        // spends nothing.
        if !self.axiom_shared.is_empty() {
            let h = crate::io::anon_sig_hash(&ce_sig(ce));
            if let Some(&group) = self.axiom_shared.get(&h) {
                if let Some(&(id, graph)) = self.subtree_nodes.get(&group) {
                    if self.subtree_debug {
                        eprintln!(
                            "[subtree] reuse owner={} same_graph={} id={id} sig={}",
                            self.cur_owner,
                            graph == self.graph_seq,
                            &ce_sig(ce)[..ce_sig(ce).len().min(160)]
                        );
                    }
                    if graph != self.graph_seq {
                        // Reached again in a new graph: the node keeps its id,
                        // its own list cells are new, and its children are
                        // reached again in turn — each keeping its node, and
                        // each interned here so that an axiom derived from one
                        // of them resolves to it.
                        self.rewalk_children(ce);
                        self.subtree_nodes.insert(group, (id, self.graph_seq));
                    }
                    return Some(id);
                }
                let id = self.translate_ce_fresh(ce)?;
                if self.subtree_debug {
                    eprintln!(
                        "[subtree] first owner={} id={id} sig={}",
                        self.cur_owner,
                        &ce_sig(ce)[..ce_sig(ce).len().min(160)]
                    );
                }
                self.subtree_nodes.insert(group, (id, self.graph_seq));
                return Some(id);
            }
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
                    // The group's node may already exist under another route: a
                    // BARE member at an earlier owner took it through the span
                    // path (keyed by group and structure), or this owner's own
                    // bare statement translated first and interned it. One
                    // object, one node: take it rather than minting a second.
                    let id = match self.span_intern.get(&(g, ce_sig(ce))) {
                        Some(&id) => {
                            self.spend_list_cells(ce);
                            id
                        }
                        None => match self.intern.get(&ce_sig(ce)) {
                            Some(&id) => id,
                            None => self.translate_ce_fresh(ce)?,
                        },
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
            if let Some(&id) = self.intern.get(&ce_sig(ce)).filter(|_| {
                // An operand-minted node is not a reuse target on structural
                // equality alone — each axiom owns its expression — but identity
                // evidence shares the node: carried provenance (relax made the
                // operand and the derived super one object), or the source
                // document referencing one `rdf:nodeID` from both positions.
                !self.operand_minted.contains(&ce_sig(ce))
                    || self.carried_shared.contains(&crate::io::anon_sig_hash(&ce_sig(ce)))
                    || shared_key(ce)
                        .is_some_and(|k| self.owner_shared_in_source.contains(&k))
            }) {
                if self.trace.as_deref() == Some(self.cur_owner.as_str()) {
                    eprintln!("  [trace {}] REUSE-intern genid{id}", self.cur_owner);
                }
                self.reuse_count += 1;
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

    /// Walk the children of an expression whose own node is kept, exactly as a
    /// fresh translation would, so that each child is reached (and reused or
    /// allocated on its own account) and this level's list cells are spent.
    fn rewalk_children(&mut self, ce: &CE<RcStr>) {
        let record = std::mem::take(&mut self.record_operands);
        match ce {
            CE::ObjectIntersectionOf(ops) => {
                self.translate_ce_list(ops, record);
            }
            CE::ObjectUnionOf(ops) => {
                self.translate_ce_list(ops, false);
            }
            CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => {
                self.translate_ce(bce);
            }
            CE::ObjectComplementOf(b) => {
                self.translate_ce(b);
            }
            CE::ObjectMinCardinality { bce, .. }
            | CE::ObjectMaxCardinality { bce, .. }
            | CE::ObjectExactCardinality { bce, .. } => {
                if !is_thing(bce) {
                    self.translate_ce(bce);
                }
            }
            CE::ObjectOneOf(_) => self.spend_list_cells(ce),
            _ => {}
        }
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
                self.object(i);
                if let Individual::Anonymous(a) = i {
                    self.reach(a.0.as_ref(), None);
                }
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
                let mut members: Vec<&Individual<RcStr>> = inds.iter().collect();
                members.sort_by(|a, b| cmp_individual(a, b));
                for m in &members {
                    self.object(m);
                }
                for m in members {
                    if let Individual::Anonymous(a) = m {
                        self.reach(a.0.as_ref(), None);
                    }
                }
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
    /// Translate a data range, returning its own node's id when it has one.
    fn translate_dr(&mut self, dr: &horned_owl::model::DataRange<RcStr>) -> Option<u64> {
        use horned_owl::model::DataRange as DR;
        match dr {
            DR::Datatype(_) => None,
            DR::DataIntersectionOf(v) | DR::DataUnionOf(v) => {
                let id = self.fresh();
                for d in v {
                    self.fresh_cell();
                    self.translate_dr(d);
                }
                Some(id)
            }
            DR::DataComplementOf(d) => {
                let id = self.fresh();
                self.translate_dr(d);
                Some(id)
            }
            DR::DataOneOf(lits) => {
                let id = self.fresh();
                for _ in lits {
                    self.fresh_cell();
                }
                Some(id)
            }
            DR::DatatypeRestriction(_, facets) => {
                let id = self.fresh();
                for _ in facets {
                    self.fresh_cell();
                    self.fresh();
                }
                Some(id)
            }
        }
    }

    /// The single-triple axiom `owner pred <data range>`: the range's nodes, then
    /// the reification node of an annotated one, recorded with its target — and
    /// for an anonymous range, the range's own id, which the writer names it by.
    fn single_triple_dr_reif(
        &mut self,
        owner: &str,
        dr: &horned_owl::model::DataRange<RcStr>,
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
        pred: &str,
    ) {
        use horned_owl::model::DataRange as DR;
        let id = self.translate_dr(dr);
        if anns.is_empty() {
            return;
        }
        let rid = self.fresh();
        let tsig = match (dr, id) {
            (DR::Datatype(d), _) => format!("R\u{1}{}", crate::io::owlrdf::esc_attr(d.0.as_ref())),
            (_, Some(id)) => format!("N\u{1}genid{id}"),
            (_, None) => String::new(),
        };
        self.reif.entry(self.cur_owner.clone()).or_default().push((format!("{pred}\u{1}{tsig}"), rid));
        if let Some(id) = id {
            self.shared_seq.entry(owner.to_string()).or_default().push((dr_sig(dr), id));
        }
        self.translate_node_annotations(rid, anns);
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
        if let AnnotationValue::AnonymousIndividual(a) = &anno.av {
            self.translate_anonymous(a.0.as_ref());
            self.object_anon(a.0.as_ref());
            if !anno.ann.is_empty() {
                // …and the target of the annotation's own node.
                self.object_anon(a.0.as_ref());
            }
            self.reach(a.0.as_ref(), None);
        }
        // An annotation CAN carry its own annotations — horned's `Annotation` has an
        // `ann` set — and each nesting level reifies as a further `owl:Annotation`
        // node, so each one consumes an id.
        if !anno.ann.is_empty() {
            let id = self.fresh();
            self.annotation_nodes.push(id);
            self.translate_annotations(&anno.ann);
        }
    }

    /// The annotations of the reified axiom `node`, recording those that carry
    /// annotations of their own for the writer.
    fn translate_node_annotations(&mut self, node: u64, anns: &std::collections::BTreeSet<Annotation<RcStr>>) {
        if let Some(axiom) = self.cur_anon_axiom {
            let nodes = self.anon_reif.entry(axiom).or_default();
            if nodes.last() != Some(&node) {
                nodes.push(node);
            }
        }
        let mark = self.annotation_nodes.len();
        self.translate_annotations(anns);
        if self.annotation_nodes.len() > mark {
            let ids = self.annotation_nodes.split_off(mark);
            self.nested.insert(node, NestedAnnotations { anns: anns.clone(), ids, sources: Vec::new() });
            if let Some(axiom) = self.cur_axiom {
                self.nested_axioms.insert(axiom);
            }
        }
    }

    /// The annotations of the node `node` of one pair of an axiom written as
    /// pairs. The pairs carry the same annotations, so the first pair numbers
    /// the nodes of the annotated ones, and each later pair is one more source
    /// of those nodes.
    fn translate_pair_annotations(
        &mut self,
        first: &mut Option<u64>,
        node: u64,
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
    ) {
        match *first {
            None => {
                *first = Some(node);
                self.translate_node_annotations(node, anns);
            }
            Some(f) => match self.nested.get_mut(&f) {
                Some(n) => n.sources.push(node),
                None => self.translate_annotations(anns),
            },
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
                    CE::Class(c) => format!("R\u{1}{}", crate::io::owlrdf::esc_attr(c.0.as_ref())),
                    _ => format!("N\u{1}genid{}", obj_id.unwrap_or(0)),
                };
                self.reif
                    .entry(self.cur_owner.clone())
                    .or_default()
                    .push((format!("{prop}\u{1}{tsig}"), rid));
            }
            self.translate_node_annotations(rid, anns);
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
        operand_minted: Default::default(),
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
        shared_occurrences: model.shared_occurrences.clone(),
        subtree_debug: std::env::var("OM_SUBTREE_DEBUG").is_ok(),
        span_pending: None,
        debug_lo,
        debug_hi,
        trace: std::env::var("OM_GENID_TRACE").ok(),
        ..Default::default()
    };

    // Anonymous individuals are numbered from the model: their axioms are
    // reached from the graphs that name them, and the anonymous section states
    // the rest.
    g.anon_present = model.ont.iter().any(|ac| names_anonymous(&ac.component));
    if g.anon_present {
        // The axioms each anonymous individual is reached with: those it is the
        // subject of, and the samenesses and differences it is a member of, in
        // axiom order; then the annotation assertions about it.
        let mut own: HashMap<String, Vec<&AnnotatedComponent<RcStr>>> = HashMap::new();
        let mut about: HashMap<String, Vec<&AnnotatedComponent<RcStr>>> = HashMap::new();
        for ac in model.ont.iter() {
            let anon = |i: &Individual<RcStr>| match i {
                Individual::Anonymous(a) => Some(a.0.as_ref().to_string()),
                Individual::Named(_) => None,
            };
            let subjects: Vec<String> = match &ac.component {
                Component::ClassAssertion(ax) => anon(&ax.i).into_iter().collect(),
                Component::ObjectPropertyAssertion(ax) => anon(&ax.from).into_iter().collect(),
                Component::DataPropertyAssertion(ax) => anon(&ax.from).into_iter().collect(),
                Component::NegativeObjectPropertyAssertion(ax) => anon(&ax.from).into_iter().collect(),
                Component::NegativeDataPropertyAssertion(ax) => anon(&ax.from).into_iter().collect(),
                Component::SameIndividual(ax) => ax.0.iter().filter_map(anon).collect(),
                Component::DifferentIndividuals(ax) => ax.0.iter().filter_map(anon).collect(),
                Component::AnnotationAssertion(ax) => {
                    if let AnnotationSubject::AnonymousIndividual(a) = &ax.subject {
                        about.entry(a.0.as_ref().to_string()).or_default().push(ac);
                    }
                    Vec::new()
                }
                _ => Vec::new(),
            };
            for x in subjects {
                own.entry(x).or_default().push(ac);
            }
        }
        let individuals: std::collections::BTreeSet<String> = own.keys().chain(about.keys()).cloned().collect();
        for x in individuals {
            let mut axioms = own.remove(&x).unwrap_or_default();
            axioms.sort_by(|a, b| cmp_annotated_axiom(a, b));
            axioms.dedup_by(|a, b| std::ptr::eq(*a, *b));
            let mut assertions = about.remove(&x).unwrap_or_default();
            assertions.sort_by(|a, b| cmp_annotated_axiom(a, b));
            let list: Vec<(u64, AnnotatedComponent<RcStr>)> =
                axioms.into_iter().chain(assertions).map(|ac| (axiom_identity(ac), ac.clone())).collect();
            g.reachable.extend(list.iter().map(|(k, _)| *k));
            g.anon_axioms.insert(x, list);
        }
    }

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
    // `Declaration` axioms (see `owlrdf::try_save`), so the numbering pass must walk
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

    // Ontology header: annotations on the ontology (rarely anonymous), in
    // the order the header states them.
    let mut ont_anns: Vec<&Annotation<RcStr>> = ont_anns
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::OntologyAnnotation(oa) => Some(&oa.0),
            _ => None,
        })
        .collect();
    ont_anns.sort_by(|a, b| {
        crate::io::owlrdf::ann_key(a.ap.0.as_ref(), &a.av).cmp(&crate::io::owlrdf::ann_key(b.ap.0.as_ref(), &b.av))
    });
    g.begin_graph(HEADER_GRAPH.to_string());
    for oa in ont_anns {
        let mark = g.annotation_nodes.len();
        g.translate_annotation(oa);
        if g.annotation_nodes.len() > mark {
            let ids = g.annotation_nodes.split_off(mark);
            g.ontology_nested.push((oa.clone(), ids));
        }
    }

    // A class frame holding a plain/`xsd:string` twin numbers its annotation
    // assertions in the order the frame writes them (see `io::frame_twins`).
    let frame_orders = crate::io::frame_twins::assertion_orders(model);

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
                axioms.sort_by(|a, b| cmp_annotated_axiom(a, b));
                let assertion = |ac: &&AnnotatedComponent<RcStr>| matches!(ac.component, Component::AnnotationAssertion(_));
                if let Some(order) = frame_orders.get(iri.as_str()).filter(|_| axioms.iter().any(assertion)) {
                    axioms.retain(|ac| !assertion(ac));
                    axioms.extend(order.iter().copied());
                }
                g.entity_start.insert(iri.clone(), g.counter);
                if g.subtree_debug {
                    eprintln!("[start] {iri} {}", g.counter);
                }
                g.cur_owner = iri.clone();
                g.begin_graph(iri.clone());
                g.intern.clear();
                g.graph_seq += 1;
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
        // An individual that every statement naming it is about is a graph
        // of its own, of those statements, in node order. A difference of
        // more than two, or of two of which it is the second, is passed
        // over.
        let pos = |id: &str| {
            let bare = id.strip_prefix("_:").unwrap_or(id);
            model.anon_doc_order.iter().position(|l| l == bare).unwrap_or(usize::MAX)
        };
        let mut naming: HashMap<String, Vec<&AnnotatedComponent<RcStr>>> = HashMap::new();
        for ac in model.ont.iter() {
            for x in referenced_anonymous(ac) {
                naming.entry(x).or_default().push(ac);
            }
        }
        let mut ids: Vec<String> = naming.keys().cloned().collect();
        ids.sort_by(|a, b| pos(a).cmp(&pos(b)).then_with(|| a.cmp(b)));
        for x in ids {
            let mut refs = naming[&x].clone();
            refs.sort_by(|a, b| cmp_annotated_axiom(a, b));
            refs.dedup_by(|a, b| std::ptr::eq(*a, *b));
            let mut axioms = Vec::new();
            let mut root = true;
            for ac in refs {
                if let Component::DifferentIndividuals(d) = &ac.component {
                    let first = d.0.iter().min_by(|a, b| cmp_individual(a, b));
                    if d.0.len() != 2 || !matches!(first, Some(Individual::Anonymous(a)) if a.0.as_ref() == x) {
                        continue;
                    }
                }
                if axiom_subject(&ac.component).as_deref() != Some(x.as_str()) {
                    root = false;
                    break;
                }
                axioms.push(ac);
            }
            if !root {
                continue;
            }
            g.begin_graph(format!("{ANON_GRAPH}{x}"));
            g.intern.clear();
            g.graph_seq += 1;
            let before = g.anon_order.len();
            for ac in axioms {
                g.translate_axiom("__anon_individuals__", ac);
            }
            if g.anon_order.len() > before {
                g.anon_roots.push(x);
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
            axioms.sort_by(|a, b| cmp_annotated_axiom(a, b));
            g.entity_start.insert(iri.clone(), g.counter);
            if g.subtree_debug {
                eprintln!("[start] {iri} {}", g.counter);
            }
            g.cur_owner = iri.clone();
            g.begin_graph(iri.clone());
            g.intern.clear();
            g.graph_seq += 1;
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
        g.begin_graph(format!("{GENERAL_GRAPH}{}", axiom_identity(ac)));
        g.intern.clear();
        g.graph_seq += 1;
        g.sub_sigs.clear();
        g.eq_sigs.clear();
        let start = g.counter;
        g.general_root.insert(axiom_identity(ac), start);
        g.translate_axiom("__general__", ac);
        let nodes: Vec<u64> = (start..g.counter).filter(|n| g.nested.contains_key(n)).collect();
        if !nodes.is_empty() {
            g.general_nested.insert(axiom_identity(ac), nodes);
        }
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
        g.begin_graph(RULES_GRAPH.to_string());
        g.intern.clear();
        g.graph_seq += 1;
        for ac in rules {
            if let Component::Rule(r) = &ac.component {
                let id = g.fresh();
                g.rule_ids.push(id);
                g.cur_axiom = ac.ann.iter().any(|a| !a.ann.is_empty()).then(|| axiom_identity(ac));
                g.translate_node_annotations(id, &ac.ann);
                g.translate_atom_list(&r.body);
                g.translate_atom_list(&r.head);
            }
        }
    }

    // An annotated chain whose super-property is an inverse is stated under the
    // property the inverse names, as the nested source of its reification. Its
    // nodes come after every other: the list's cells, then the reification.
    let mut inverse_chains: Vec<&AnnotatedComponent<RcStr>> = model
        .ont
        .iter()
        .filter(|ac| {
            !ac.ann.is_empty()
                && matches!(&ac.component, Component::SubObjectPropertyOf(ax)
                    if matches!(ax.sub, SOPE::ObjectPropertyChain(_)) && matches!(ax.sup, OPE::InverseObjectProperty(_)))
        })
        .collect();
    inverse_chains.sort_by(|a, b| cmp_annotated_axiom(a, b));
    for ac in inverse_chains {
        let Component::SubObjectPropertyOf(ax) = &ac.component else { continue };
        let (SOPE::ObjectPropertyChain(chain), OPE::InverseObjectProperty(sup)) = (&ax.sub, &ax.sup) else {
            continue;
        };
        let owner = sup.0.as_ref().to_string();
        g.cur_owner = owner.clone();
        g.begin_graph(owner.clone());
        g.cur_axiom = ac.ann.iter().any(|a| !a.ann.is_empty()).then(|| axiom_identity(ac));
        g.intern.clear();
        g.graph_seq += 1;
        let head = g.translate_ope_list(chain);
        let rid = g.fresh();
        let members: String = chain
            .iter()
            .filter_map(|m| match m {
                OPE::ObjectProperty(p) => Some(format!("{}\u{2}", crate::io::owlrdf::esc_attr(p.0.as_ref()))),
                OPE::InverseObjectProperty(_) => None,
            })
            .collect();
        let pred = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
        if let Some(head) = head {
            g.shared_seq.entry(owner.clone()).or_default().push((format!("LIST\u{1}{pred}\u{1}{members}"), head));
            g.reif.entry(owner.clone()).or_default().push((format!("~{pred}\u{1}N\u{1}genid{head}"), rid));
        }
        g.reif.entry(owner).or_default().push((format!("~{pred}\u{1}C\u{1}{members}"), rid));
        g.translate_node_annotations(rid, &ac.ann);
    }

    // An annotated assertion on an inverse property between named individuals
    // is stated of the named property, the other way round, in the graph of
    // its subject that way round, and its annotations reify that statement.
    // Its nodes come after every other: the reification, then those its
    // annotations take.
    let mut inverse_assertions: Vec<&AnnotatedComponent<RcStr>> = model
        .ont
        .iter()
        .filter(|ac| !ac.ann.is_empty() && inverse_assertion(&ac.component).is_some())
        .collect();
    inverse_assertions.sort_by(|a, b| cmp_annotated_axiom(a, b));
    for ac in inverse_assertions {
        let Some((p, subject, target)) = inverse_assertion(&ac.component) else { continue };
        let owner = subject.to_string();
        g.cur_owner = owner.clone();
        g.begin_graph(owner.clone());
        g.cur_axiom = ac.ann.iter().any(|a| !a.ann.is_empty()).then(|| axiom_identity(ac));
        g.intern.clear();
        g.graph_seq += 1;
        let rid = g.fresh();
        let sig = format!(
            "{}\u{1}R\u{1}{}",
            crate::io::owlrdf::esc_attr(p),
            crate::io::owlrdf::esc_attr(target)
        );
        g.reif.entry(owner).or_default().push((sig, rid));
        g.translate_node_annotations(rid, &ac.ann);
    }

    g
}

/// An assertion on an inverse property between named individuals, as the
/// statement of the named property it is: (property, subject, target).
fn inverse_assertion(c: &Component<RcStr>) -> Option<(&str, &str, &str)> {
    match c {
        Component::ObjectPropertyAssertion(ax) => match (&ax.ope, &ax.from, &ax.to) {
            (OPE::InverseObjectProperty(p), Individual::Named(target), Individual::Named(subject)) => {
                Some((p.0.as_ref(), subject.0.as_ref(), target.0.as_ref()))
            }
            _ => None,
        },
        _ => None,
    }
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
            (Some(a), Some(b)) => Some(if iri_order(&a, &b) == Ordering::Greater { b } else { a }),
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
    match c {
        Component::DisjointObjectProperties(ax) => ax.0.len() > 2,
        Component::DisjointDataProperties(ax) => ax.0.len() > 2,
        Component::HasKey(ax) => !matches!(ax.ce, CE::Class(_)),
        _ => matches!(
            c,
            Component::SubClassOf(_)
                | Component::EquivalentClasses(_)
                | Component::DisjointClasses(_)
                | Component::DisjointUnion(_)
                | Component::DifferentIndividuals(_)
        ),
    }
}

impl Genids {
    /// Translate one axiom, assigning genids to its anonymous nodes and, for an
    /// annotated axiom with an anonymous CE object, recording the shared genid.
    fn translate_axiom(&mut self, owner: &str, ac: &AnnotatedComponent<RcStr>) {
        // An axiom about an anonymous individual is numbered once, in the graph
        // that first reaches it.
        let anon_key = (self.anon_present && names_anonymous(&ac.component)).then(|| axiom_identity(ac));
        if let Some(key) = anon_key.filter(|k| self.reachable.contains(k)) {
            if !self.reached.insert(key) {
                return;
            }
            self.anon_home.insert(key, self.cur_graph.clone());
            self.anon_order.push(key);
        }
        // An axiom reached from an annotation is numbered inside the axiom
        // that annotation is on, which goes on after it.
        let outer = std::mem::replace(&mut self.cur_anon_axiom, anon_key);
        let outer_axiom = self.cur_axiom;
        self.translate_axiom_nodes(owner, ac);
        self.cur_anon_axiom = None;
        // …and, its own statements made, reaches the anonymous individuals it
        // names.
        if let Some(key) = anon_key {
            for x in reached_individuals(&ac.component) {
                self.reach(&x, Some(key));
            }
        }
        self.cur_anon_axiom = outer;
        self.cur_axiom = outer_axiom;
    }

    fn translate_axiom_nodes(&mut self, owner: &str, ac: &AnnotatedComponent<RcStr>) {
        // An axiom in which one anonymous structure appears twice is copied
        // whole before it is translated, so nothing in it is the object another
        // axiom holds, whatever the record says.
        self.axiom_shared = if self.shared_occurrences.is_empty()
            || has_shared_structure(&ac.component)
        {
            HashMap::new()
        } else {
            self.shared_occurrences
                .get(&axiom_identity(ac))
                .map(|nodes| shared_groups(nodes, &ac.component))
                .unwrap_or_default()
        };
        // A pending group belongs to ONE axiom's translation. A translation that
        // short-circuits before its take() leaves the flag armed, and the next
        // axiom — possibly another OWNER's equivalence — would hand its target
        // the group's node. The SubClassOf arm re-derives both below; every
        // other arm must start clean.
        self.span_pending = None;
        self.cross_pending = None;
        self.cur_axiom = ac.ann.iter().any(|a| !a.ann.is_empty()).then(|| axiom_identity(ac));
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
                // Only evidence-backed sharing may turn a nested operand into a
                // reference: a structural twin reuse (`sub_sigs`, `annotated_sigs`)
                // shares the NODE ID between the twin axioms but the operand of a
                // plain equivalence still renders inline, as its own object.
                if (carried_here || c_star || c_key) && !matches!(ax.sup, CE::Class(_)) {
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
                if self.trace.as_deref() == Some(owner) {
                    eprintln!(
                        "  [trace {owner}] sup={} span_pending={:?} cross={:?} cross_shared={}",
                        &sup_sig[..sup_sig.len().min(60)],
                        self.span_pending,
                        shared_key(&ax.sup)
                            .and_then(|k| self.cross_shared.get(&format!("{owner}\u{1}{k}")).copied()),
                        self.cross_shared.len()
                    );
                }
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
                        self.operand_minted.remove(&sup_sig);
                        self.intern.insert(sup_sig, id);
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
                let members = ordered_ces(&ax.0);
                if members.len() > 2 {
                    self.chain_ce(owner, &members, &ac.ann);
                } else {
                    self.pairwise_ce(owner, &ax.0, &ac.ann, Some(P_EQUIV));
                }
                self.record_operands = false;
            }
            Component::EquivalentObjectProperties(ax) if ordered_opes(&ax.0).len() > 2 => {
                self.chain_ope(owner, &ordered_opes(&ax.0), &ac.ann);
            }
            // Three or more equivalent data properties are their consecutive
            // pairs, taken in the order of a hash set of those pairs.
            Component::EquivalentDataProperties(ax)
                if ax.0.iter().map(|d| d.0.as_ref()).collect::<std::collections::BTreeSet<&str>>().len() > 2 =>
            {
                let mut members: Vec<&horned_owl::model::DataProperty<RcStr>> = ax.0.iter().collect();
                members.sort_by(|a, b| crate::owlapi_hash::iri_cmp(a.0.as_ref(), b.0.as_ref()));
                members.dedup();
                if !ac.ann.is_empty() {
                    let hashes: Vec<i32> = members
                        .windows(2)
                        .map(|w| {
                            let pair = Component::EquivalentDataProperties(horned_owl::model::EquivalentDataProperties(
                                vec![w[0].clone(), w[1].clone()],
                            ));
                            crate::owlapi_hash::axiom_hash(&pair, &ac.ann).unwrap_or(0)
                        })
                        .collect();
                    let mut first = None;
                    for i in crate::owlapi_hash::hashset_order(&hashes) {
                        let rid = self.fresh();
                        let sig = format!(
                            "{P_EQUIV_PROPERTY}\u{1}R\u{1}{}",
                            crate::io::owlrdf::esc_attr(members[i + 1].0.as_ref())
                        );
                        self.reif.entry(self.cur_owner.clone()).or_default().push((sig, rid));
                        self.translate_pair_annotations(&mut first, rid, &ac.ann);
                    }
                }
            }
            Component::DisjointClasses(ax) => {
                if ax.0.len() == 2 {
                    self.pairwise_ce(owner, &ax.0, &ac.ann, Some(P_DISJOINT));
                } else {
                    // AllDisjointClasses: node, then members list, then anns.
                    let node = self.fresh();
                    self.translate_ce_list(&ax.0, false);
                    self.translate_node_annotations(node, &ac.ann);
                }
            }
            Component::DisjointUnion(ax) => {
                // subject = named class, object = list of expressions.
                let head = self.translate_ce_list(&ax.1, false);
                if !ac.ann.is_empty() {
                    // The reification's target is the collection, whose named
                    // members are its signature.
                    let rid = self.fresh();
                    let mut named: Vec<&str> = ax
                        .1
                        .iter()
                        .filter_map(|c| match c {
                            CE::Class(c) => Some(c.0.as_ref()),
                            _ => None,
                        })
                        .collect();
                    named.sort_by(|a, b| iri_order(a, b));
                    named.dedup();
                    let members: String =
                        named.iter().map(|iri| format!("{}\u{2}", crate::io::owlrdf::esc_attr(iri))).collect();
                    let pred = "http://www.w3.org/2002/07/owl#disjointUnionOf";
                    self.record_list(owner, pred, &members, head, rid);
                    self.reif.entry(self.cur_owner.clone()).or_default().push((format!("{pred}\u{1}C\u{1}{members}"), rid));
                    self.translate_node_annotations(rid, &ac.ann);
                }
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
                self.translate_individual(&ax.i);
                if ac.ann.is_empty() {
                    self.translate_ce(&ax.ce);
                } else {
                    // The reification's source.
                    self.object(&ax.i);
                    let named = matches!(ax.i, Individual::Named(_));
                    if let Some(id) =
                        self.single_triple_ce_reif(None, &ax.ce, &ac.ann, false, named.then_some(P_TYPE))
                    {
                        if named {
                            self.record_shared(owner, &ax.ce, id);
                        } else if let Some(axiom) = self.cur_anon_axiom {
                            self.anon_ce.insert(axiom, id);
                        }
                    }
                }
            }
            // Property assertions are single triples, an assertion on an inverse
            // stated the other way round: the subject's node, the object's, then
            // the reification node of an annotated one.
            Component::ObjectPropertyAssertion(ax) => {
                let (from, to) = match &ax.ope {
                    OPE::ObjectProperty(_) => (&ax.from, &ax.to),
                    OPE::InverseObjectProperty(_) => (&ax.to, &ax.from),
                };
                self.translate_individual(from);
                self.translate_individual(to);
                self.object(to);
                // The assertion stated the other way round is a new axiom without
                // the annotations, so only an assertion on a named property
                // reifies here. The reification of one on an inverse takes its
                // nodes after every other (`inverse_assertion`).
                if !ac.ann.is_empty() && matches!(ax.ope, OPE::ObjectProperty(_)) {
                    self.object(from);
                    self.object(to);
                    let rid = self.fresh();
                    if let (Individual::Named(_), Individual::Named(o)) = (from, to) {
                        let p = ope_owner(&ax.ope);
                        let sig = format!(
                            "{}\u{1}R\u{1}{}",
                            crate::io::owlrdf::esc_attr(&p),
                            crate::io::owlrdf::esc_attr(o.0.as_ref())
                        );
                        self.reif.entry(self.cur_owner.clone()).or_default().push((sig, rid));
                    }
                    self.translate_node_annotations(rid, &ac.ann);
                }
            }
            Component::DataPropertyAssertion(ax) => {
                self.translate_individual(&ax.from);
                if !ac.ann.is_empty() {
                    self.object(&ax.from);
                    let rid = self.fresh();
                    if matches!(ax.from, Individual::Named(_)) {
                        let sig = format!(
                            "{}\u{1}L\u{1}{}",
                            crate::io::owlrdf::esc_attr(ax.dp.0.as_ref()),
                            crate::io::owlrdf::esc(ax.to.literal())
                        );
                        self.reif.entry(self.cur_owner.clone()).or_default().push((sig, rid));
                    }
                    self.translate_node_annotations(rid, &ac.ann);
                }
            }
            // A negative assertion is a node of its own, carrying its terms and
            // its annotations.
            Component::NegativeObjectPropertyAssertion(ax) => {
                let id = self.fresh();
                self.object(&ax.from);
                self.object(&ax.to);
                if let (Individual::Named(_), Individual::Named(o)) = (&ax.from, &ax.to) {
                    // An inverse property is nested, so the block names none.
                    let prop = ope_named(&ax.ope).map(|p| crate::io::owlrdf::esc_attr(&p)).unwrap_or_default();
                    let sig = format!("NPA\u{1}{prop}\u{1}R\u{1}{}", crate::io::owlrdf::esc_attr(o.0.as_ref()));
                    self.reif.entry(self.cur_owner.clone()).or_default().push((sig, id));
                }
                self.translate_individual(&ax.from);
                self.translate_ope(&ax.ope);
                self.translate_individual(&ax.to);
                self.translate_node_annotations(id, &ac.ann);
            }
            Component::NegativeDataPropertyAssertion(ax) => {
                let id = self.fresh();
                self.object(&ax.from);
                if matches!(ax.from, Individual::Named(_)) {
                    let sig = format!(
                        "NPA\u{1}{}\u{1}L\u{1}{}",
                        crate::io::owlrdf::esc_attr(ax.dp.0.as_ref()),
                        crate::io::owlrdf::esc(ax.to.literal())
                    );
                    self.reif.entry(self.cur_owner.clone()).or_default().push((sig, id));
                }
                self.translate_individual(&ax.from);
                self.translate_node_annotations(id, &ac.ann);
            }
            Component::ObjectPropertyRange(ax) => self.property_class(owner, &ax.ope, P_RANGE, &ax.ce, &ac.ann),
            Component::ObjectPropertyDomain(ax) => self.property_class(owner, &ax.ope, P_DOMAIN, &ax.ce, &ac.ann),
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
                    let head = self.translate_ope_list(chain);
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
                        let pred = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
                        self.record_list(owner, pred, &members, head, rid);
                        self.reif.entry(self.cur_owner.clone()).or_default().push((format!("{pred}\u{1}C\u{1}{members}"), rid));
                        self.translate_node_annotations(rid, &ac.ann);
                    }
                } else {
                    // `sub rdfs:subPropertyOf super`. Either side may be an
                    // `ObjectInverseOf`, which is an anonymous node of its own —
                    // RO's `RO_0002378 ⊑ inverse(RO_0002376)` is one, and the id
                    // it takes shifts every later node in the document.
                    if let SOPE::ObjectPropertyExpression(sub) = &ax.sub {
                        self.property_edge(owner, sub, P_SUB_PROPERTY, EdgeObject::Property(&ax.sup), &ac.ann);
                    }
                }
            }
            Component::AnnotationAssertion(ax) => {
                // An anonymous subject or value is a node of its own; an annotated
                // assertion reifies to an owl:Axiom node. Record its (property ⊕
                // value) signature so the writer can order the block, unless it
                // names an anonymous individual: that block is ordered by its node
                // (`anon_reif`).
                let anon_subject = match &ax.subject {
                    AnnotationSubject::AnonymousIndividual(a) => Some(a.0.as_ref()),
                    AnnotationSubject::IRI(_) => None,
                };
                let anon_value = match &ax.ann.av {
                    AnnotationValue::AnonymousIndividual(a) => Some(a.0.as_ref()),
                    _ => None,
                };
                if let Some(x) = anon_subject {
                    self.translate_anonymous(x);
                }
                if let Some(x) = anon_value {
                    self.translate_anonymous(x);
                    self.object_anon(x);
                }
                if !ac.ann.is_empty() {
                    // The reification's source and target.
                    for x in anon_subject.into_iter().chain(anon_value) {
                        self.object_anon(x);
                    }
                    let rid = self.fresh();
                    if anon_subject.is_none() && anon_value.is_none() {
                        let prop = crate::io::owlrdf::esc_attr(ax.ann.ap.0.as_ref());
                        let sig = format!("{prop}\u{1}{}", ann_value_tsig(&ax.ann.av));
                        self.reif.entry(self.cur_owner.clone()).or_default().push((sig, rid));
                    }
                    self.translate_node_annotations(rid, &ac.ann);
                }
            }
            // `P rdfs:range <data range>` / `rdfs:domain`: the range may be a whole
            // subtree, and RO's `RO_0002029` is one — an `rdfs:Datatype` node with
            // two `owl:withRestrictions` cells and two facet nodes, five ids in
            // all, so the range itself has to be walked and not just the
            // reification node.
            Component::DataPropertyRange(ax) => {
                self.single_triple_dr_reif(owner, &ax.dr, &ac.ann, P_RANGE);
            }
            Component::DatatypeDefinition(ax) => {
                self.single_triple_dr_reif(owner, &ax.range, &ac.ann, P_EQUIV);
            }
            Component::DataPropertyDomain(ax) => {
                if let Some(id) =
                    self.single_triple_ce_reif(None, &ax.ce, &ac.ann, false, Some(P_DOMAIN))
                {
                    if !ac.ann.is_empty() {
                        self.record_shared(owner, &ax.ce, id);
                    }
                }
            }
            // A key is one list over its properties — object properties, then
            // inverses, then data properties — with a cell per property, after
            // the node of an anonymous class.
            Component::HasKey(ax) => {
                self.translate_ce(&ax.ce);
                let mut props: Vec<&horned_owl::model::PropertyExpression<RcStr>> = ax.vpe.iter().collect();
                props.sort_by(|a, b| key_rank(a).cmp(&key_rank(b)));
                let mut head = None;
                for pe in props.iter().rev() {
                    head = Some(self.fresh_cell());
                    if let horned_owl::model::PropertyExpression::ObjectPropertyExpression(ope) = pe {
                        self.translate_ope(ope);
                    }
                }
                if !ac.ann.is_empty() {
                    // The reification's target is the collection, whose named
                    // members are its signature.
                    let rid = self.fresh();
                    let members: String = props
                        .iter()
                        .filter_map(|pe| match pe {
                            horned_owl::model::PropertyExpression::ObjectPropertyExpression(OPE::ObjectProperty(p)) => {
                                Some(p.0.as_ref())
                            }
                            horned_owl::model::PropertyExpression::DataProperty(d) => Some(d.0.as_ref()),
                            _ => None,
                        })
                        .map(|iri| format!("{}\u{2}", crate::io::owlrdf::esc_attr(iri)))
                        .collect();
                    let pred = "http://www.w3.org/2002/07/owl#hasKey";
                    self.record_list(owner, pred, &members, head, rid);
                    self.reif.entry(self.cur_owner.clone()).or_default().push((format!("{pred}\u{1}C\u{1}{members}"), rid));
                    self.translate_node_annotations(rid, &ac.ann);
                }
            }
            // `AllDisjointProperties`: the axiom's node, then its members list.
            Component::DisjointObjectProperties(ax) if ax.0.len() > 2 => {
                let node = self.fresh();
                let mut members: Vec<&OPE<RcStr>> = ax.0.iter().collect();
                members.sort_by(|a, b| crate::io::owlfunc::cmp_ope(a, b));
                for m in members.iter().rev() {
                    self.fresh_cell();
                    self.translate_ope(m);
                }
                self.translate_node_annotations(node, &ac.ann);
            }
            Component::DisjointDataProperties(ax) if ax.0.len() > 2 => {
                let node = self.fresh();
                for _ in &ax.0 {
                    self.fresh_cell();
                }
                self.translate_node_annotations(node, &ac.ann);
            }
            // A `DifferentIndividuals` of three or more members is one
            // `owl:AllDifferent` node carrying an `owl:distinctMembers` list, so it
            // costs one id for the axiom node plus one per list cell. Two members
            // are written as a single `owl:differentFrom` edge instead, which costs
            // nothing but the reification node an annotated axiom needs.
            Component::DifferentIndividuals(ax) => {
                if ax.0.len() > 2 {
                    let node = self.fresh();
                    self.translate_individual_list(&ax.0);
                    for m in &ax.0 {
                        self.object(m);
                    }
                    self.translate_node_annotations(node, &ac.ann);
                } else {
                    self.individual_pairs(&ax.0, &ac.ann, false, "http://www.w3.org/2002/07/owl#differentFrom");
                }
            }
            // A sameness is one `owl:sameAs` edge per consecutive pair of its
            // members in order, each reified on its own when annotated.
            Component::SameIndividual(ax) => {
                self.individual_pairs(&ax.0, &ac.ann, true, "http://www.w3.org/2002/07/owl#sameAs");
            }
            // Declarations, property characteristics and the binary property
            // axioms: single triples, whose only nodes are the inverse property
            // expressions among their terms — in order, subject first — and the
            // reification node of an annotated one.
            _ => {
                if let Some((subject, pred, object)) = property_triple(&ac.component) {
                    self.property_edge(owner, subject, pred, object, &ac.ann);
                } else {
                    for ope in inverse_terms(&ac.component) {
                        self.translate_ope(ope);
                    }
                    if !ac.ann.is_empty() {
                        let rid = self.fresh();
                        if let Some(sig) = edge_reif_sig(&ac.component) {
                            self.reif.entry(self.cur_owner.clone()).or_default().push((sig, rid));
                        }
                        self.translate_node_annotations(rid, &ac.ann);
                    }
                }
            }
        }
    }

    /// The first cell of a list an annotated axiom names twice, as its object
    /// and as its reification's target: Turtle names the list by that id.
    /// Recorded by the predicate and the named members, and the reification
    /// `rid` under the signature it has when its target is named by that id.
    fn record_list(&mut self, owner: &str, pred: &str, members: &str, head: Option<u64>, rid: u64) {
        if let Some(head) = head {
            self.shared_seq.entry(owner.to_string()).or_default().push((format!("LIST\u{1}{pred}\u{1}{members}"), head));
            self.reif.entry(self.cur_owner.clone()).or_default().push((format!("{pred}\u{1}N\u{1}genid{head}"), rid));
        }
    }

    /// An object property's domain or range: the property's node when it is an
    /// inverse, then the class. An inverse subject's block is a root of the
    /// graph, or, annotated, the reification's nested source; an annotated
    /// anonymous class is named by id.
    fn property_class(
        &mut self,
        owner: &str,
        ope: &OPE<RcStr>,
        pred: &str,
        ce: &CE<RcStr>,
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
    ) {
        let subject_id = matches!(ope, OPE::InverseObjectProperty(_)).then(|| self.fresh());
        let reif_prop = match subject_id {
            Some(_) => format!("~{pred}"),
            None => pred.to_string(),
        };
        let object_id = self.single_triple_ce_reif(None, ce, anns, false, Some(reif_prop.as_str()));
        if !anns.is_empty() {
            if let Some(id) = object_id {
                self.record_shared(owner, ce, id);
            }
        } else if let Some(sid) = subject_id {
            let target = match ce {
                CE::Class(c) => format!("R\u{1}{}", crate::io::owlrdf::esc_attr(c.0.as_ref())),
                _ => "A\u{1}".to_string(),
            };
            self.reif.entry(self.cur_owner.clone()).or_default().push((format!("INV\u{1}{pred}\u{1}{target}"), sid));
        }
    }

    /// An object property axiom's triple: the nodes of its inverse property
    /// terms, subject first, then the reification node of an annotated one. An
    /// inverse subject is a node of its own: unannotated, it is a root of the
    /// graph; annotated, it is the reification's nested source. An inverse
    /// object of an annotated axiom is named by id, so its id is recorded for
    /// the writer.
    fn property_edge(
        &mut self,
        owner: &str,
        subject: &OPE<RcStr>,
        pred: &str,
        object: EdgeObject<'_>,
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
    ) {
        let esc = crate::io::owlrdf::esc_attr;
        let subject_id = matches!(subject, OPE::InverseObjectProperty(_)).then(|| self.fresh());
        let object_id = matches!(object, EdgeObject::Property(OPE::InverseObjectProperty(_))).then(|| self.fresh());
        let named_object = match object {
            EdgeObject::Type(class) => Some(class),
            EdgeObject::Property(OPE::ObjectProperty(p)) => Some(p.0.as_ref()),
            EdgeObject::Property(OPE::InverseObjectProperty(_)) => None,
        };
        let owner_graph = self.cur_owner.clone();
        match (subject_id, anns.is_empty()) {
            (Some(sid), true) => {
                let target = named_object.map(|o| format!("R\u{1}{}", esc(o))).unwrap_or_else(|| "A\u{1}".to_string());
                self.reif.entry(owner_graph).or_default().push((format!("INV\u{1}{pred}\u{1}{target}"), sid));
            }
            (Some(_), false) => {
                let rid = self.fresh();
                if let (EdgeObject::Property(OPE::InverseObjectProperty(q)), Some(oid)) = (object, object_id) {
                    self.shared_seq
                        .entry(owner.to_string())
                        .or_default()
                        .push((format!("INV\u{1}{pred}\u{1}{}", q.0.as_ref()), oid));
                }
                let target = match (named_object, object_id) {
                    (Some(o), _) => format!("R\u{1}{}", esc(o)),
                    (None, Some(oid)) => format!("N\u{1}genid{oid}"),
                    (None, None) => String::new(),
                };
                self.reif.entry(owner_graph).or_default().push((format!("~{pred}\u{1}{target}"), rid));
                self.translate_node_annotations(rid, anns);
            }
            (None, false) => {
                let rid = self.fresh();
                let sig = match (object, object_id) {
                    (EdgeObject::Property(OPE::InverseObjectProperty(q)), Some(oid)) => {
                        self.shared_seq
                            .entry(owner.to_string())
                            .or_default()
                            .push((format!("INV\u{1}{pred}\u{1}{}", q.0.as_ref()), oid));
                        Some(format!("{pred}\u{1}N\u{1}genid{oid}"))
                    }
                    _ => named_object.map(|o| format!("{pred}\u{1}R\u{1}{}", esc(o))),
                };
                if let Some(sig) = sig {
                    self.reif.entry(owner_graph).or_default().push((sig, rid));
                }
                self.translate_node_annotations(rid, anns);
            }
            (None, true) => {}
        }
    }

    /// The `pred` edges between the members of a sameness or difference, in
    /// order: consecutive pairs (`consecutive`) or the one pair of a binary axiom.
    /// Each edge is its subject's node, its object's, and the reification node of
    /// an annotated axiom.
    fn individual_pairs(
        &mut self,
        members: &[Individual<RcStr>],
        anns: &std::collections::BTreeSet<Annotation<RcStr>>,
        consecutive: bool,
        pred: &str,
    ) {
        let pairs = individual_pair_list(members, anns, consecutive);
        let mut first = None;
        for (a, b) in pairs {
            self.translate_individual(a);
            self.translate_individual(b);
            self.object(b);
            if !anns.is_empty() {
                self.object(a);
                self.object(b);
                let rid = self.fresh();
                if let Some(axiom) = self.cur_anon_axiom {
                    self.anon_reif.entry(axiom).or_default().push(rid);
                }
                if let (Individual::Named(_), Individual::Named(o)) = (a, b) {
                    let sig = format!("{pred}\u{1}R\u{1}{}", crate::io::owlrdf::esc_attr(o.0.as_ref()));
                    self.reif.entry(self.cur_owner.clone()).or_default().push((sig, rid));
                }
                self.translate_pair_annotations(&mut first, rid, anns);
            }
        }
    }

    /// An RDF list of individuals — cells from the last sorted element back to
    /// the first, each cell taking an id before its member is translated.
    fn translate_individual_list(&mut self, inds: &[Individual<RcStr>]) {
        let mut sorted: Vec<&Individual<RcStr>> = inds.iter().collect();
        sorted.sort_by(|a, b| cmp_individual(a, b));
        for i in (0..sorted.len()).rev() {
            self.fresh_cell(); // list cell
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
                g.object(i);
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

    /// A property chain's list, cells back to front (a chain keeps its order);
    /// the id of its first cell.
    fn translate_ope_list(&mut self, chain: &[OPE<RcStr>]) -> Option<u64> {
        let mut head = None;
        for i in (0..chain.len()).rev() {
            head = Some(self.fresh()); // list cell
            self.translate_ope(&chain[i]);
        }
        head
    }

    /// An equivalence of three or more classes: one triple for each consecutive
    /// pair of its ordered members, all in this graph, each reified on its own
    /// when the axiom is annotated. A member in two pairs is one node. Every
    /// anonymous member's id is recorded under `NARY⊕sig` for the writer.
    fn chain_ce(&mut self, owner: &str, members: &[&CE<RcStr>], anns: &std::collections::BTreeSet<Annotation<RcStr>>) {
        let record = self.record_operands;
        let mut first = None;
        let first_anon = !matches!(members[0], CE::Class(_));
        if first_anon {
            self.record_operands = record;
            if let Some(id) = self.translate_ce(members[0]) {
                self.record_chain_member(owner, format!("NARY\u{1}{}", ce_sig(members[0])), id);
            }
        }
        for i in 0..members.len() - 1 {
            let object = members[i + 1];
            self.record_operands = record;
            let mut id = None;
            if !matches!(object, CE::Class(_)) {
                let sig = ce_sig(object);
                let reuse = self.eq_sigs.contains(&sig)
                    || self.annotated_sigs.contains(&sig)
                    || self.carried_shared.contains(&crate::io::anon_sig_hash(&sig));
                id = self.translate_ce_maybe_reuse(object, reuse);
                self.eq_sigs.insert(sig.clone());
                if let Some(id) = id {
                    self.intern.entry(sig.clone()).or_insert(id);
                    self.record_chain_member(owner, format!("NARY\u{1}{sig}"), id);
                }
            }
            if anns.is_empty() {
                continue;
            }
            let rid = self.fresh();
            let target = match object {
                CE::Class(c) => format!("R\u{1}{}", crate::io::owlrdf::esc_attr(c.0.as_ref())),
                _ => format!("N\u{1}genid{}", id.unwrap_or(0)),
            };
            // The first member, anonymous, is nested in its reification.
            let nested = if i == 0 && first_anon { "~" } else { "" };
            self.reif.entry(self.cur_owner.clone()).or_default().push((format!("{nested}{P_EQUIV}\u{1}{target}"), rid));
            self.translate_pair_annotations(&mut first, rid, anns);
            // The host's own pair is found as a binary equivalence's target is.
            if i == 0 && !first_anon {
                if let Some(id) = id {
                    self.record_shared(owner, object, id);
                }
            }
        }
    }

    /// An equivalence of three or more object properties, as `chain_ce`: an
    /// inverse member is a node of its own, recorded under `NARY⊕INV⊕iri`.
    fn chain_ope(&mut self, owner: &str, members: &[&OPE<RcStr>], anns: &std::collections::BTreeSet<Annotation<RcStr>>) {
        let node = |g: &mut Self, ope: &OPE<RcStr>| match ope {
            OPE::InverseObjectProperty(p) => {
                let id = g.fresh();
                g.record_chain_member(owner, format!("NARY\u{1}INV\u{1}{}", p.0.as_ref()), id);
                Some(id)
            }
            OPE::ObjectProperty(_) => None,
        };
        let first_anon = node(self, members[0]).is_some();
        let mut first = None;
        for i in 0..members.len() - 1 {
            let object = members[i + 1];
            let id = node(self, object);
            if anns.is_empty() {
                continue;
            }
            let rid = self.fresh();
            let target = match (object, id) {
                (OPE::ObjectProperty(p), _) => format!("R\u{1}{}", crate::io::owlrdf::esc_attr(p.0.as_ref())),
                (_, id) => format!("N\u{1}genid{}", id.unwrap_or(0)),
            };
            let nested = if i == 0 && first_anon { "~" } else { "" };
            self.reif
                .entry(self.cur_owner.clone())
                .or_default()
                .push((format!("{nested}{P_EQUIV_PROPERTY}\u{1}{target}"), rid));
            self.translate_pair_annotations(&mut first, rid, anns);
        }
    }

    fn record_chain_member(&mut self, owner: &str, sig: String, id: u64) {
        self.shared_seq.entry(owner.to_string()).or_default().push((sig, id));
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

/// The members of an n-ary class axiom in order, each once.
pub(crate) fn ordered_ces(members: &[CE<RcStr>]) -> Vec<&CE<RcStr>> {
    let mut v: Vec<&CE<RcStr>> = members.iter().collect();
    v.sort_by(|a, b| cmp_ce(a, b));
    v.dedup();
    v
}

/// The members of an n-ary object property axiom in order, each once.
pub(crate) fn ordered_opes(members: &[OPE<RcStr>]) -> Vec<&OPE<RcStr>> {
    let mut v: Vec<&OPE<RcStr>> = members.iter().collect();
    v.sort_by(|a, b| crate::io::owlfunc::cmp_ope(a, b));
    v.dedup();
    v
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
        // A declaration is its entity's `rdf:type` triple, and reifies there when
        // annotated.
        Component::DeclareClass(d) => Some(d.0 .0.as_ref().to_string()),
        Component::DeclareObjectProperty(d) => Some(d.0 .0.as_ref().to_string()),
        Component::DeclareDataProperty(d) => Some(d.0 .0.as_ref().to_string()),
        Component::DeclareAnnotationProperty(d) => Some(d.0 .0.as_ref().to_string()),
        Component::DeclareNamedIndividual(d) => Some(d.0 .0.as_ref().to_string()),
        Component::DeclareDatatype(d) => Some(d.0 .0.as_ref().to_string()),
        // An axiom about an inverse property is stated under the property the
        // inverse names, as its own node straight after that property's.
        Component::ObjectPropertyRange(ax) => Some(ope_owner(&ax.ope)),
        Component::ObjectPropertyDomain(ax) => Some(ope_owner(&ax.ope)),
        Component::SubObjectPropertyOf(ax) => match &ax.sub {
            SOPE::ObjectPropertyExpression(sub) => Some(ope_owner(sub)),
            SOPE::ObjectPropertyChain(_) => ope_named(&ax.sup),
        },
        Component::TransitiveObjectProperty(ax) => Some(ope_owner(&ax.0)),
        Component::FunctionalObjectProperty(ax) => Some(ope_owner(&ax.0)),
        Component::InverseFunctionalObjectProperty(ax) => Some(ope_owner(&ax.0)),
        Component::SymmetricObjectProperty(ax) => Some(ope_owner(&ax.0)),
        Component::AsymmetricObjectProperty(ax) => Some(ope_owner(&ax.0)),
        Component::ReflexiveObjectProperty(ax) => Some(ope_owner(&ax.0)),
        Component::IrreflexiveObjectProperty(ax) => Some(ope_owner(&ax.0)),
        Component::InverseObjectProperties(ax) => nary_ope_owner(&[ax.0.clone(), ax.1.clone()]),
        Component::EquivalentObjectProperties(ax) => nary_ope_owner(&ax.0),
        Component::DisjointObjectProperties(ax) if ax.0.len() <= 2 => nary_ope_owner(&ax.0),
        Component::SubAnnotationPropertyOf(ax) => Some(ax.sub.0.as_ref().to_string()),
        Component::AnnotationPropertyDomain(ax) => Some(ax.ap.0.as_ref().to_string()),
        Component::AnnotationPropertyRange(ax) => Some(ax.ap.0.as_ref().to_string()),
        Component::HasKey(ax) => match &ax.ce {
            CE::Class(c) => Some(c.0.as_ref().to_string()),
            _ => None,
        },
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
            ax.0.iter().map(|p| p.0.as_ref().to_string()).min_by(|a, b| iri_order(a, b))
        }
        Component::DisjointDataProperties(ax) if ax.0.len() <= 2 => {
            ax.0.iter().map(|p| p.0.as_ref().to_string()).min_by(|a, b| iri_order(a, b))
        }
        Component::DatatypeDefinition(ax) => Some(ax.kind.0.as_ref().to_string()),
        // A class assertion is rendered inside the individual's own block, so it
        // is numbered there. An ANONYMOUS type is a blank node: CL's brain-atlas
        // components assert 199 of them, and leaving them out of the walk left
        // every later blank node numbered 199 too low.
        Component::ClassAssertion(ax) => match &ax.i {
            Individual::Named(i) => Some(i.0.as_ref().to_string()),
            _ => None,
        },
        // Every other individual axiom is stated of its subject — an assertion on
        // an inverse, of its object — and a sameness or a binary difference of
        // its first member, when that member is named.
        Component::ObjectPropertyAssertion(ax) => named_individual(match &ax.ope {
            OPE::ObjectProperty(_) => &ax.from,
            OPE::InverseObjectProperty(_) => &ax.to,
        }),
        Component::DataPropertyAssertion(ax) => named_individual(&ax.from),
        Component::NegativeObjectPropertyAssertion(ax) => named_individual(&ax.from),
        Component::NegativeDataPropertyAssertion(ax) => named_individual(&ax.from),
        Component::SameIndividual(ax) => first_individual(&ax.0),
        Component::DifferentIndividuals(ax) if ax.0.len() == 2 => first_individual(&ax.0),
        _ => None,
    }
}

fn named_individual(i: &Individual<RcStr>) -> Option<String> {
    match i {
        Individual::Named(n) => Some(n.0.as_ref().to_string()),
        Individual::Anonymous(_) => None,
    }
}

/// The first of a set of individuals, in order, when it is named.
fn first_individual(members: &[Individual<RcStr>]) -> Option<String> {
    members.iter().min_by(|a, b| cmp_individual(a, b)).and_then(named_individual)
}

fn ope_named(ope: &OPE<RcStr>) -> Option<String> {
    match ope {
        OPE::ObjectProperty(p) => Some(p.0.as_ref().to_string()),
        _ => None,
    }
}

/// The object property whose block an axiom about `ope` is stated in: the
/// property, or the one an inverse names.
fn ope_owner(ope: &OPE<RcStr>) -> String {
    ope_iri(ope).to_string()
}

/// The IRI of a property expression's named property.
fn ope_iri(ope: &OPE<RcStr>) -> &str {
    match ope {
        OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => p.0.as_ref(),
    }
}

/// The first property block that states a binary or n-ary object property
/// axiom: the block of its first member in order, when that member is named, or
/// of the property any inverse member names — whichever is rendered first.
pub(crate) fn nary_ope_owner(members: &[OPE<RcStr>]) -> Option<String> {
    let mut sorted: Vec<&OPE<RcStr>> = members.iter().collect();
    sorted.sort_by(|a, b| crate::io::owlfunc::cmp_ope(a, b));
    let first = match sorted.first() {
        Some(OPE::ObjectProperty(p)) => Some(p.0.as_ref().to_string()),
        _ => None,
    };
    first
        .into_iter()
        .chain(sorted.iter().filter_map(|m| match m {
            OPE::InverseObjectProperty(p) => Some(p.0.as_ref().to_string()),
            OPE::ObjectProperty(_) => None,
        }))
        .min_by(|a, b| iri_order(a, b))
}

/// The order entity blocks are rendered in: by namespace, then local name.
fn iri_order(a: &str, b: &str) -> Ordering {
    crate::io::owlrdf::iri_key(a).cmp(&crate::io::owlrdf::iri_key(b))
}

/// The object of an object property axiom's triple: another property
/// expression, or the class a characteristic types its subject with.
#[derive(Clone, Copy)]
enum EdgeObject<'a> {
    Property(&'a OPE<RcStr>),
    Type(&'static str),
}

/// The triple a characteristic or a binary object property axiom is stated
/// as: its subject, predicate and object. The subject of an equivalence,
/// inverse or disjointness is its first member in order. A sub-property
/// axiom is numbered in its own arm.
fn property_triple(c: &Component<RcStr>) -> Option<(&OPE<RcStr>, &'static str, EdgeObject<'_>)> {
    // Named properties come first, each kind in IRI order.
    fn key(o: &OPE<RcStr>) -> (bool, (&str, &str)) {
        (matches!(o, OPE::InverseObjectProperty(_)), crate::io::owlrdf::iri_key(ope_iri(o)))
    }
    fn ordered<'a>(a: &'a OPE<RcStr>, b: &'a OPE<RcStr>) -> (&'a OPE<RcStr>, &'a OPE<RcStr>) {
        if key(a) > key(b) { (b, a) } else { (a, b) }
    }
    fn typed<'a>(ope: &'a OPE<RcStr>, class: &'static str) -> Option<(&'a OPE<RcStr>, &'static str, EdgeObject<'a>)> {
        Some((ope, P_TYPE, EdgeObject::Type(class)))
    }
    fn edge<'a>(
        (s, o): (&'a OPE<RcStr>, &'a OPE<RcStr>),
        pred: &'static str,
    ) -> Option<(&'a OPE<RcStr>, &'static str, EdgeObject<'a>)> {
        Some((s, pred, EdgeObject::Property(o)))
    }
    match c {
        Component::FunctionalObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#FunctionalProperty"),
        Component::InverseFunctionalObjectProperty(ax) => {
            typed(&ax.0, "http://www.w3.org/2002/07/owl#InverseFunctionalProperty")
        }
        Component::TransitiveObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#TransitiveProperty"),
        Component::SymmetricObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#SymmetricProperty"),
        Component::AsymmetricObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#AsymmetricProperty"),
        Component::ReflexiveObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#ReflexiveProperty"),
        Component::IrreflexiveObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#IrreflexiveProperty"),
        Component::InverseObjectProperties(ax) => edge(ordered(&ax.0, &ax.1), P_INVERSE_OF),
        Component::EquivalentObjectProperties(ax) if ax.0.len() == 2 => {
            edge(ordered(&ax.0[0], &ax.0[1]), P_EQUIV_PROPERTY)
        }
        Component::DisjointObjectProperties(ax) if ax.0.len() == 2 => {
            edge(ordered(&ax.0[0], &ax.0[1]), P_PROPERTY_DISJOINT)
        }
        _ => None,
    }
}

/// The inverse property expressions among a single-triple axiom's terms, in the
/// order its triple is built: subject first.
fn inverse_terms(c: &Component<RcStr>) -> Vec<&OPE<RcStr>> {
    fn sorted(members: &[OPE<RcStr>]) -> Vec<&OPE<RcStr>> {
        let mut v: Vec<&OPE<RcStr>> = members.iter().collect();
        v.sort_by(|a, b| crate::io::owlfunc::cmp_ope(a, b));
        v
    }
    let terms: Vec<&OPE<RcStr>> = match c {
        Component::TransitiveObjectProperty(ax) => vec![&ax.0],
        Component::FunctionalObjectProperty(ax) => vec![&ax.0],
        Component::InverseFunctionalObjectProperty(ax) => vec![&ax.0],
        Component::SymmetricObjectProperty(ax) => vec![&ax.0],
        Component::AsymmetricObjectProperty(ax) => vec![&ax.0],
        Component::ReflexiveObjectProperty(ax) => vec![&ax.0],
        Component::IrreflexiveObjectProperty(ax) => vec![&ax.0],
        Component::InverseObjectProperties(ax) => {
            let mut v = vec![&ax.0, &ax.1];
            v.sort_by(|a, b| crate::io::owlfunc::cmp_ope(a, b));
            v
        }
        Component::EquivalentObjectProperties(ax) => sorted(&ax.0),
        Component::DisjointObjectProperties(ax) => sorted(&ax.0),
        _ => Vec::new(),
    };
    terms.into_iter().filter(|o| matches!(o, OPE::InverseObjectProperty(_))).collect()
}

/// The position of a key property in the key's list: object properties, then
/// inverse object properties, then data properties.
fn key_rank(pe: &horned_owl::model::PropertyExpression<RcStr>) -> (u8, (&str, &str)) {
    use horned_owl::model::PropertyExpression as PE;
    let key = crate::io::owlrdf::iri_key;
    match pe {
        PE::ObjectPropertyExpression(OPE::ObjectProperty(p)) => (0, key(p.0.as_ref())),
        PE::ObjectPropertyExpression(OPE::InverseObjectProperty(p)) => (1, key(p.0.as_ref())),
        PE::DataProperty(d) => (2, key(d.0.as_ref())),
        PE::AnnotationProperty(a) => (3, key(a.0.as_ref())),
    }
}

/// The signature of an annotated single-triple axiom's reification, as the
/// writer reads it back off the block (`owlrdf::reif_signature`): the annotated
/// property, then the target. Only axioms between named terms have one.
fn edge_reif_sig(c: &Component<RcStr>) -> Option<String> {
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    let edge = |pred: &str, target: &str| {
        Some(format!("{pred}\u{1}R\u{1}{}", crate::io::owlrdf::esc_attr(target)))
    };
    let owl_type = |t: &str| edge(P_TYPE, &format!("{OWL}{t}"));
    let named = |o: &OPE<RcStr>| ope_named(o);
    // The second of two named members, in order.
    let other = |a: &str, b: &str| -> String {
        if iri_order(a, b) == Ordering::Greater { a.to_string() } else { b.to_string() }
    };
    match c {
        Component::DeclareClass(_) => owl_type("Class"),
        Component::DeclareObjectProperty(_) => owl_type("ObjectProperty"),
        Component::DeclareDataProperty(_) => owl_type("DatatypeProperty"),
        Component::DeclareAnnotationProperty(_) => owl_type("AnnotationProperty"),
        Component::DeclareNamedIndividual(_) => owl_type("NamedIndividual"),
        Component::DeclareDatatype(_) => edge(P_TYPE, "http://www.w3.org/2000/01/rdf-schema#Datatype"),
        Component::FunctionalObjectProperty(ax) if named(&ax.0).is_some() => owl_type("FunctionalProperty"),
        Component::InverseFunctionalObjectProperty(ax) if named(&ax.0).is_some() => {
            owl_type("InverseFunctionalProperty")
        }
        Component::TransitiveObjectProperty(ax) if named(&ax.0).is_some() => owl_type("TransitiveProperty"),
        Component::SymmetricObjectProperty(ax) if named(&ax.0).is_some() => owl_type("SymmetricProperty"),
        Component::AsymmetricObjectProperty(ax) if named(&ax.0).is_some() => owl_type("AsymmetricProperty"),
        Component::ReflexiveObjectProperty(ax) if named(&ax.0).is_some() => owl_type("ReflexiveProperty"),
        Component::IrreflexiveObjectProperty(ax) if named(&ax.0).is_some() => owl_type("IrreflexiveProperty"),
        Component::FunctionalDataProperty(_) => owl_type("FunctionalProperty"),
        Component::SubAnnotationPropertyOf(ax) => edge(P_SUB_PROPERTY, ax.sup.0.as_ref()),
        Component::SubDataPropertyOf(ax) => edge(P_SUB_PROPERTY, ax.sup.0.as_ref()),
        Component::SubObjectPropertyOf(ax) => match (&ax.sub, named(&ax.sup)) {
            (SOPE::ObjectPropertyExpression(OPE::ObjectProperty(_)), Some(sup)) => edge(P_SUB_PROPERTY, &sup),
            _ => None,
        },
        Component::InverseObjectProperties(ax) => match (named(&ax.0), named(&ax.1)) {
            (Some(a), Some(b)) => edge(P_INVERSE_OF, &other(&a, &b)),
            _ => None,
        },
        Component::EquivalentObjectProperties(ax) if ax.0.len() == 2 => match (named(&ax.0[0]), named(&ax.0[1])) {
            (Some(a), Some(b)) => edge(P_EQUIV_PROPERTY, &other(&a, &b)),
            _ => None,
        },
        Component::DisjointObjectProperties(ax) if ax.0.len() == 2 => match (named(&ax.0[0]), named(&ax.0[1])) {
            (Some(a), Some(b)) => edge(P_PROPERTY_DISJOINT, &other(&a, &b)),
            _ => None,
        },
        Component::EquivalentDataProperties(ax) if ax.0.len() == 2 => {
            edge(P_EQUIV_PROPERTY, &other(ax.0[0].0.as_ref(), ax.0[1].0.as_ref()))
        }
        Component::DisjointDataProperties(ax) if ax.0.len() == 2 => {
            edge(P_PROPERTY_DISJOINT, &other(ax.0[0].0.as_ref(), ax.0[1].0.as_ref()))
        }
        Component::AnnotationPropertyDomain(ax) => edge(P_DOMAIN, ax.iri.as_ref()),
        Component::AnnotationPropertyRange(ax) => edge(P_RANGE, ax.iri.as_ref()),
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

/// [`ce_sig`] for a data range.
pub fn dr_sig(dr: &horned_owl::model::DataRange<RcStr>) -> String {
    format!("{dr:?}")
}

/// Does `c` name an anonymous individual, anywhere but in its annotations?
pub(crate) fn names_anonymous(c: &Component<RcStr>) -> bool {
    let mut found = Vec::new();
    component_anonymous(c, &mut found);
    !found.is_empty()
}

/// The anonymous individuals `ac` names, its annotations included, each once.
pub(crate) fn referenced_anonymous(ac: &AnnotatedComponent<RcStr>) -> Vec<String> {
    fn anns(a: &std::collections::BTreeSet<Annotation<RcStr>>, out: &mut Vec<String>) {
        for x in a {
            if let AnnotationValue::AnonymousIndividual(i) = &x.av {
                out.push(i.0.as_ref().to_string());
            }
            anns(&x.ann, out);
        }
    }
    let mut out = Vec::new();
    component_anonymous(&ac.component, &mut out);
    if let Component::OntologyAnnotation(oa) = &ac.component {
        anns(&oa.0.ann, &mut out);
    }
    anns(&ac.ann, &mut out);
    let mut seen = std::collections::HashSet::new();
    out.retain(|x| seen.insert(x.clone()));
    out
}

/// The anonymous individuals in `c`'s terms and class expressions.
fn component_anonymous(c: &Component<RcStr>, out: &mut Vec<String>) {
    use horned_owl::model::{Atom, IArgument};
    fn ind(i: &Individual<RcStr>, out: &mut Vec<String>) {
        if let Individual::Anonymous(a) = i {
            out.push(a.0.as_ref().to_string());
        }
    }
    fn ce(c: &CE<RcStr>, out: &mut Vec<String>) {
        match c {
            CE::ObjectHasValue { i, .. } => ind(i, out),
            CE::ObjectOneOf(v) => v.iter().for_each(|i| ind(i, out)),
            _ => sub_expressions(c).into_iter().for_each(|s| ce(s, out)),
        }
    }
    fn iarg(a: &IArgument<RcStr>, out: &mut Vec<String>) {
        if let IArgument::Individual(i) = a {
            ind(i, out);
        }
    }
    match c {
        Component::ClassAssertion(ax) => {
            ind(&ax.i, out);
            ce(&ax.ce, out);
        }
        Component::ObjectPropertyAssertion(ax) => {
            ind(&ax.from, out);
            ind(&ax.to, out);
        }
        Component::NegativeObjectPropertyAssertion(ax) => {
            ind(&ax.from, out);
            ind(&ax.to, out);
        }
        Component::DataPropertyAssertion(ax) => ind(&ax.from, out),
        Component::NegativeDataPropertyAssertion(ax) => ind(&ax.from, out),
        Component::SameIndividual(ax) => ax.0.iter().for_each(|i| ind(i, out)),
        Component::DifferentIndividuals(ax) => ax.0.iter().for_each(|i| ind(i, out)),
        Component::AnnotationAssertion(ax) => {
            if let AnnotationSubject::AnonymousIndividual(a) = &ax.subject {
                out.push(a.0.as_ref().to_string());
            }
            if let AnnotationValue::AnonymousIndividual(a) = &ax.ann.av {
                out.push(a.0.as_ref().to_string());
            }
        }
        Component::OntologyAnnotation(oa) => {
            if let AnnotationValue::AnonymousIndividual(a) = &oa.0.av {
                out.push(a.0.as_ref().to_string());
            }
        }
        Component::Rule(r) => {
            for atom in r.body.iter().chain(r.head.iter()) {
                match atom {
                    Atom::ClassAtom { pred, arg } => {
                        ce(pred, out);
                        iarg(arg, out);
                    }
                    Atom::ObjectPropertyAtom { args, .. } => {
                        iarg(&args.0, out);
                        iarg(&args.1, out);
                    }
                    Atom::SameIndividualAtom(a, b) | Atom::DifferentIndividualsAtom(a, b) => {
                        iarg(a, out);
                        iarg(b, out);
                    }
                    _ => {}
                }
            }
        }
        other => component_class_expressions(other).into_iter().for_each(|x| ce(x, out)),
    }
}

/// The anonymous individual an individual axiom is about, when it is one: a
/// class assertion's individual, an assertion's subject, an annotation
/// assertion's subject, and the first member of a sameness or difference.
fn axiom_subject(c: &Component<RcStr>) -> Option<String> {
    let anon = |i: &Individual<RcStr>| match i {
        Individual::Anonymous(a) => Some(a.0.as_ref().to_string()),
        Individual::Named(_) => None,
    };
    match c {
        Component::ClassAssertion(ax) => anon(&ax.i),
        Component::ObjectPropertyAssertion(ax) => anon(&ax.from),
        Component::DataPropertyAssertion(ax) => anon(&ax.from),
        Component::NegativeObjectPropertyAssertion(ax) => anon(&ax.from),
        Component::NegativeDataPropertyAssertion(ax) => anon(&ax.from),
        Component::SameIndividual(ax) => ax.0.iter().min_by(|a, b| cmp_individual(a, b)).and_then(anon),
        Component::DifferentIndividuals(ax) => ax.0.iter().min_by(|a, b| cmp_individual(a, b)).and_then(anon),
        Component::AnnotationAssertion(ax) => match &ax.subject {
            AnnotationSubject::AnonymousIndividual(a) => Some(a.0.as_ref().to_string()),
            AnnotationSubject::IRI(_) => None,
        },
        _ => None,
    }
}

/// The anonymous individuals an axiom reaches once its own statements are
/// made, in the order it reaches them: a class assertion's individual; an
/// object property assertion's object, then its subject, as it is stated of
/// the named property; a negative assertion's subject, then an object
/// assertion's object; an annotation assertion's value; and every member of a
/// sameness, or of a difference of more than two.
pub(crate) fn reached_individuals(c: &Component<RcStr>) -> Vec<String> {
    let anon = |i: &Individual<RcStr>| match i {
        Individual::Anonymous(a) => Some(a.0.as_ref().to_string()),
        Individual::Named(_) => None,
    };
    let sorted = |v: &[Individual<RcStr>]| -> Vec<String> {
        let mut m: Vec<&Individual<RcStr>> = v.iter().collect();
        m.sort_by(|a, b| cmp_individual(a, b));
        m.dedup();
        m.into_iter().filter_map(anon).collect()
    };
    match c {
        Component::ClassAssertion(ax) => anon(&ax.i).into_iter().collect(),
        Component::ObjectPropertyAssertion(ax) => {
            let (s, o) = match ax.ope {
                OPE::ObjectProperty(_) => (&ax.from, &ax.to),
                OPE::InverseObjectProperty(_) => (&ax.to, &ax.from),
            };
            [o, s].into_iter().filter_map(anon).collect()
        }
        Component::NegativeObjectPropertyAssertion(ax) => [&ax.from, &ax.to].into_iter().filter_map(anon).collect(),
        Component::NegativeDataPropertyAssertion(ax) => anon(&ax.from).into_iter().collect(),
        Component::AnnotationAssertion(ax) => match &ax.ann.av {
            AnnotationValue::AnonymousIndividual(a) => vec![a.0.as_ref().to_string()],
            _ => Vec::new(),
        },
        Component::SameIndividual(ax) => sorted(&ax.0),
        Component::DifferentIndividuals(ax) if ax.0.len() > 2 => sorted(&ax.0),
        _ => Vec::new(),
    }
}

/// The statements a sameness (`consecutive`) or a difference of `members` is
/// written as: the consecutive pairs of its members in order, or, for a
/// difference, its two members; a sameness of three or more takes its pairs in
/// the order of a hash set of them.
pub(crate) fn individual_pair_list<'a>(
    members: &'a [Individual<RcStr>],
    anns: &std::collections::BTreeSet<Annotation<RcStr>>,
    consecutive: bool,
) -> Vec<(&'a Individual<RcStr>, &'a Individual<RcStr>)> {
    let mut sorted: Vec<&Individual<RcStr>> = members.iter().collect();
    sorted.sort_by(|a, b| cmp_individual(a, b));
    sorted.dedup();
    let mut pairs: Vec<(&Individual<RcStr>, &Individual<RcStr>)> = if consecutive || sorted.len() == 2 {
        sorted.windows(2).map(|w| (w[0], w[1])).collect()
    } else {
        Vec::new()
    };
    if pairs.len() > 1 {
        let hashes: Vec<i32> = pairs
            .iter()
            .map(|(a, b)| {
                let pair = Component::SameIndividual(horned_owl::model::SameIndividual(vec![(*a).clone(), (*b).clone()]));
                crate::owlapi_hash::axiom_hash(&pair, anns).unwrap_or(0)
            })
            .collect();
        let order = crate::owlapi_hash::hashset_order(&hashes);
        pairs = order.into_iter().map(|i| pairs[i]).collect();
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(ofn: &str) -> Model {
        let text = format!(
            "Prefix(:=<http://x/>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
             Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
             Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
             Prefix(oio:=<http://www.geneontology.org/formats/oboInOwl#>)\n\
             Ontology(<http://x/o>\n{ofn}\n)\n"
        );
        crate::io::load_from(std::io::Cursor::new(text.into_bytes()), crate::io::Format::Functional)
            .unwrap()
    }

    /// One minted `∃R.D` asserted bare for A and, with an annotation, for B is
    /// one node: B's reification points at the node A's inline copy took, and
    /// no second id is spent for it.
    #[test]
    fn an_annotated_group_member_takes_the_node_a_bare_member_minted() {
        let ofn = "Declaration(Class(:A))\nDeclaration(Class(:B))\nDeclaration(Class(:D))\n\
                   Declaration(ObjectProperty(:r))\n\
                   SubClassOf(:A ObjectSomeValuesFrom(:r :D))\n\
                   SubClassOf(Annotation(oio:source \"x\") :B ObjectSomeValuesFrom(:r :D))";
        let plain = compute(&model(ofn), 0, 0);
        let mut grouped = model(ofn);
        let b = horned_owl::model::Build::new();
        let sup = CE::ObjectSomeValuesFrom {
            ope: OPE::ObjectProperty(b.object_property("http://x/r")),
            bce: Box::new(CE::Class(b.class("http://x/D"))),
        };
        grouped.span_shared.insert(format!("http://x/A\u{1}{}", ce_sig(&sup)), 7);
        grouped.cross_shared.insert("http://x/B\u{1}http://x/r\u{1}http://x/D".to_string(), 7);
        let shared = compute(&grouped, 0, 0);
        assert_eq!(shared.counter, plain.counter - 1);
        let a_node = shared.entity_start["http://x/A"];
        assert_eq!(shared.shared["http://x/B"][&ce_sig(&sup)], a_node);
    }

    /// Two classes defined over the substituted expressions of merged classes:
    /// `Ci ≡ (Ai ⊓ ∃t.T) ⊓ ∃r.(E ⊓ ∃t.T)`, where `Ai ⊓ ∃t.T` is Ai's defining
    /// object and `E ⊓ ∃t.T` is E's, one object for both classes. Each
    /// equivalence repeats `∃t.T`, so it is numbered as a copy; what relax
    /// derives from it holds the objects themselves. `C2 ⊑ ∃r.(E ⊓ ∃t.T)`
    /// reaches E's object and its `∃t.T` again, two nodes C1 already numbered;
    /// `C2 ⊑ ∃t.T` is the `∃t.T` inside A2's object, which nothing else holds.
    #[test]
    fn a_relaxed_conjunct_shares_the_object_it_was_flattened_from() {
        let ofn = "Declaration(Class(:C1))\nDeclaration(Class(:C2))\nDeclaration(Class(:A1))\n\
                   Declaration(Class(:A2))\nDeclaration(Class(:E))\nDeclaration(Class(:T))\n\
                   Declaration(ObjectProperty(:r))\nDeclaration(ObjectProperty(:t))\n\
                   EquivalentClasses(:C1 ObjectIntersectionOf(ObjectIntersectionOf(:A1 ObjectSomeValuesFrom(:t :T)) \
                   ObjectSomeValuesFrom(:r ObjectIntersectionOf(:E ObjectSomeValuesFrom(:t :T)))))\n\
                   EquivalentClasses(:C2 ObjectIntersectionOf(ObjectIntersectionOf(:A2 ObjectSomeValuesFrom(:t :T)) \
                   ObjectSomeValuesFrom(:r ObjectIntersectionOf(:E ObjectSomeValuesFrom(:t :T)))))";
        let numbered = |recorded: bool| {
            let mut m = model(ofn);
            if recorded {
                let defining = |c: &str| -> CE<RcStr> {
                    let b = horned_owl::model::Build::new();
                    CE::ObjectIntersectionOf(vec![
                        CE::Class(b.class(format!("http://x/{c}"))),
                        CE::ObjectSomeValuesFrom {
                            ope: OPE::ObjectProperty(b.object_property("http://x/t")),
                            bce: Box::new(CE::Class(b.class("http://x/T"))),
                        },
                    ])
                };
                let root = |c: &str| crate::model::SharedNode {
                    sig: crate::io::anon_sig_hash(&ce_sig(&defining(c))),
                    group: crate::io::anon_sig_hash(&format!("http://x/{c}")),
                    inside: false,
                };
                let eqs: Vec<AnnotatedComponent<RcStr>> = m
                    .ont
                    .iter()
                    .filter(|ac| matches!(ac.component, Component::EquivalentClasses(_)))
                    .cloned()
                    .collect();
                for ac in eqs {
                    let a = if format!("{:?}", ac.component).contains("http://x/A1") { "A1" } else { "A2" };
                    m.shared_occurrences.insert(axiom_identity(&ac), vec![root(a), root("E")]);
                }
            }
            let m = crate::cmd::relax::relax(m);
            compute(&m, 0, 0)
        };
        let plain = numbered(false);
        let shared = numbered(true);
        assert_eq!(shared.counter, plain.counter - 2);
        assert_eq!(shared.entity_start["http://x/C2"], plain.entity_start["http://x/C2"]);
    }
}
