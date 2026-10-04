//! The RDF/XML writer for every RDF/XML **file** owlmake produces.
//!
//! horned-owl's `pretty_rdf` writer produces valid RDF/XML, but not the layout
//! released OBO ontology files carry, so writing through it would rewrite every
//! line of every release diff. This writer emits that layout, and does so
//! deterministically: entities are grouped by kind and sorted by IRI, and within
//! an entity the logical axioms come first, then the annotation assertions
//! sorted by (property IRI, value).
//!
//! horned's writer is kept for internal transport only (`write_to_ref`: the
//! SPARQL and rename round-trips serialize to a buffer they parse straight back,
//! where byte fidelity buys nothing and costs time).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;

use anyhow::{bail, Result};
use horned_owl::model::{
    AnnotatedComponent, AnnotationValue, Component, Literal, ObjectPropertyExpression as OPE, RcStr,
};

use crate::io::obo::ncname_suffix_index;
use crate::model::Model;

/// The 87-slash rule used inside the section banners.
const RULE: &str = "///////////////////////////////////////////////////////////////////////////////////////";

/// IRI sort key: (namespace, NCName remainder).
pub(crate) fn iri_key(iri: &str) -> (&str, &str) {
    match ncname_suffix_index(iri) {
        Some(i) => (&iri[..i], &iri[i..]),
        None => (iri, ""),
    }
}

const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
/// `annotatedProperty` IRIs for the domain/range edges reified as `<owl:Axiom>`.
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const P_SUB_PROPERTY: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const P_INVERSE_OF: &str = "http://www.w3.org/2002/07/owl#inverseOf";
const P_EQUIV_PROPERTY: &str = "http://www.w3.org/2002/07/owl#equivalentProperty";
const P_PROPERTY_DISJOINT: &str = "http://www.w3.org/2002/07/owl#propertyDisjointWith";
const P_EQUIV_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const OWL_FUNCTIONAL_PROPERTY: &str = "http://www.w3.org/2002/07/owl#FunctionalProperty";
const P_RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const P_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const P_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";

/// An axiom's annotations as `(property IRI, value)` pairs, the shape the
/// reification builders take.
fn ax_anns(ac: &AnnotatedComponent<RcStr>) -> Vec<(String, AnnotationValue<RcStr>)> {
    ac.ann.iter().map(|a| (a.ap.0.as_ref().to_string(), a.av.clone())).collect()
}
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const OWL_NAMED_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#NamedIndividual";
const NEG_PA: &str = "http://www.w3.org/2002/07/owl#NegativePropertyAssertion";
const RDFS_LITERAL: &str = "http://www.w3.org/2000/01/rdf-schema#Literal";

/// XML-escape text content (`&`, `<`, `>`).
pub(crate) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            _ => o.push(c),
        }
    }
    o
}

/// XML-escape an attribute value (adds `"`).
pub(crate) fn esc_attr(s: &str) -> String {
    esc(s).replace('"', "&quot;")
}

/// Abbreviate a property IRI to `prefix:local`: split off the NCName suffix (see
/// [`ncname_split`]) and look the remaining namespace up EXACTLY. Falls back to
/// the full IRI when the namespace is undeclared, which is only ever reachable
/// if [`owlapi_prefixes`] has a gap, since the result is not a well-formed
/// element name.
fn qname(iri: &str, prefixes: &[(String, String)]) -> String {
    let (ns, local) = ncname_split(iri);
    match prefixes.iter().find(|(_, pns)| pns == ns) {
        Some((p, _)) if p.is_empty() => local.to_string(),
        Some((p, _)) => format!("{p}:{local}"),
        None => iri.to_string(),
    }
}

/// Render an annotation value as an RDF/XML child element with 8-space
/// indent: an IRI value is `rdf:resource`, a plain `xsd:string` literal is bare
/// text, a language literal carries `xml:lang`, and a typed literal carries
/// `rdf:datatype`.
fn render_ann(prop_iri: &str, av: &AnnotationValue<RcStr>, prefixes: &[(String, String)]) -> String {
    let q = qname(prop_iri, prefixes);
    match av {
        AnnotationValue::IRI(i) => {
            format!("        <{q} rdf:resource=\"{}\"/>\n", esc_attr(i.as_ref()))
        }
        AnnotationValue::Literal(l) => match l {
            Literal::Simple { literal } => {
                format!("        <{q}>{}</{q}>\n", esc(literal))
            }
            Literal::Language { literal, lang } => {
                format!("        <{q} xml:lang=\"{lang}\">{}</{q}>\n", esc(literal))
            }
            Literal::Datatype { literal, datatype_iri } => {
                if datatype_iri.as_ref() == XSD_STRING {
                    format!("        <{q}>{}</{q}>\n", esc(literal))
                } else {
                    format!(
                        "        <{q} rdf:datatype=\"{}\">{}</{q}>\n",
                        esc_attr(datatype_iri.as_ref()),
                        esc(literal)
                    )
                }
            }
        },
        // An anonymous individual is a node, not a value: the property element
        // holds an empty `rdf:Description`, and whatever the node itself carries
        // is rendered where that node belongs. An SSSOM mapping set in RDF is
        // made of these — its `sssom:mappings` values are the reification nodes
        // of its 51,582 mappings, each rendered again as its own `Axiom` block.
        AnnotationValue::AnonymousIndividual(a) => anon_slot(&q, a.0.as_ref(), "        "),
    }
}

/// The order literals take wherever they are sorted: datatype IRI, then lexical
/// value, then language — or, in an inline-anon document, lexical value, then
/// language, then datatype. A language-tagged literal keys as
/// `rdf:PlainLiteral`; an untyped one keys as whichever of `rdf:PlainLiteral` /
/// `xsd:string` this document's parse produced, see [`plain_datatype`]. Both
/// render bare, but they sort on opposite sides of `xsd:anyURI`.
///
/// Every explicit datatype keys as itself, `xsd:string` included: a literal that
/// reached us as `Datatype{xsd:string}` came from an OBO read (see
/// `io::obo::ann`), where an untyped literal is typed `xsd:string`, and
/// collapsing it onto `plain_datatype()` would order it as though it had come
/// from an OFN/RDF-XML read.
pub(crate) fn literal_key(l: &Literal<RcStr>) -> (String, String, String) {
    match l {
        Literal::Language { literal, lang } => literal_parts_key(literal, lang, RDF_PLAIN_LITERAL),
        Literal::Datatype { literal, datatype_iri } => literal_parts_key(literal, "", datatype_iri.as_ref()),
        Literal::Simple { literal } => literal_parts_key(literal, "", plain_datatype()),
    }
}

/// [`literal_key`] of a literal given by its lexical value, language and the
/// datatype it keys as.
fn literal_parts_key(lexical: &str, lang: &str, datatype: &str) -> (String, String, String) {
    if inline_anon() {
        (lexical.to_string(), lang.to_string(), datatype.to_string())
    } else {
        (datatype.to_string(), lexical.to_string(), lang.to_string())
    }
}

/// Sort key for annotation assertions / ontology annotations, giving a subject's
/// RDF triples their order: by property IRI (namespace, remainder), then the
/// object node — an IRI value (rank 0, compared on its own namespace/remainder)
/// before a literal (rank 1, by lexical value then language tag, so a plain
/// literal precedes a language-tagged one of the same text).
pub(crate) type AnnKey = (String, String, u8, String, String, String);
pub(crate) fn ann_key(prop_iri: &str, av: &AnnotationValue<RcStr>) -> AnnKey {
    let (pns, prem) = iri_key(prop_iri);
    // Literals take `literal_key`'s order.
    let (rank, va, vb, lang) = match av {
        AnnotationValue::IRI(i) => {
            let (n, r) = iri_key(i.as_ref());
            (0u8, n.to_string(), r.to_string(), String::new())
        }
        AnnotationValue::Literal(l) => {
            let (a, b, c) = literal_key(l);
            (1, a, b, c)
        }
        // An anonymous individual keys by its node id. The id is not written —
        // every such value renders as the same empty `rdf:Description` — but two
        // distinct nodes are two distinct values, and a key that ignored the id
        // would let the de-duplication collapse a mapping set's 51,582
        // `sssom:mappings` values into one.
        AnnotationValue::AnonymousIndividual(a) => {
            (2, a.0.as_ref().to_string(), String::new(), String::new())
        }
    };
    (pns.to_string(), prem.to_string(), rank, va, vb, lang)
}

/// The namespace of an IRI: the portion up to and including the last `#`, else
/// up to and including the last `/`.
pub(crate) fn iri_namespace(iri: &str) -> &str {
    if let Some(h) = iri.rfind('#') {
        &iri[..=h]
    } else if let Some(s) = iri.rfind('/') {
        &iri[..=s]
    } else {
        iri
    }
}

/// Split an IRI at its NCName suffix: scan backwards while the characters are
/// NCName characters, remembering the leftmost that could START an NCName;
/// everything from there is the local name. With no such position the whole IRI
/// is the namespace and the local name is empty.
///
/// This is NOT [`iri_namespace`], which splits at the last `#`/`/` and is the
/// rule the sort keys use. The two agree on every ordinary property IRI, and
/// differ exactly where it matters here: OBA's edit file carries
/// `http://www.geneontology.org/formats/oboInOwl#http://purl.org/dc/terms/contributor`,
/// whose local name is `contributor`, not `http://purl.org/dc/terms/contributor`
/// — the latter is not an XML name, so abbreviating against `oboInOwl#` would
/// write a `<oboInOwl:http://purl.org/dc/terms/contributor>` element that no
/// parser can read. Splitting at the NCName suffix yields the `dc/terms/`
/// namespace instead, which is then declared under a generated prefix such as
/// `terms1`.
pub(crate) fn ncname_split(iri: &str) -> (&str, &str) {
    if iri.starts_with("_:") {
        return (iri, "");
    }
    let mut idx: Option<usize> = None;
    for (i, c) in iri.char_indices().rev() {
        if !is_ncname_char(c) {
            break;
        }
        if is_ncname_start(c) {
            idx = Some(i);
        }
    }
    match idx {
        Some(i) => (&iri[..i], &iri[i..]),
        None => (iri, ""),
    }
}

fn is_ncname_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || (c as u32) > 127
}
fn is_ncname_char(c: char) -> bool {
    is_ncname_start(c) || c.is_ascii_digit() || c == '-' || c == '.'
}

/// Generate a prefix for a namespace that has no declared one: scan backwards
/// for the last NCName run, then forwards collecting NCName chars except `.` (so
/// a file-like extension is dropped). Yields e.g. `foaf` from
/// `http://xmlns.com/foaf/0.1/` and `mondo` from
/// `http://purl.obolibrary.org/obo/mondo#`.
fn generate_prefix(ns: &str) -> String {
    let chars: Vec<char> = ns.chars().collect();
    let n = chars.len() as isize;
    let mut start_index: isize = -1;
    let mut i = n - 1;
    while i > -1 {
        let cur = chars[i as usize];
        let is_start = is_ncname_start(cur);
        if is_start || start_index == -1 {
            if is_start {
                start_index = i;
            }
        } else if !is_ncname_char(cur) {
            break;
        }
        i -= 1;
    }
    if start_index == -1 {
        return "p".to_string();
    }
    let si = start_index as usize;
    let mut end_index = si + 1;
    let mut j = si;
    let nn = chars.len();
    while end_index < nn && j < nn {
        let cur = chars[end_index];
        if is_ncname_char(cur) && cur != '.' {
            end_index = j + 1;
        } else {
            break;
        }
        j += 1;
    }
    chars[si..end_index].iter().collect()
}

thread_local! {
    /// Count of anonymous super/equivalent expressions the writer declined to
    /// render because an equal one was already emitted. Must equal the numbering
    /// pass's `reuse_count`: every skipped render is a node genid must NOT have
    /// allocated. A gap between the two IS the counter drift.
    pub static WRITER_SKIPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// `Model::plain_literals_typed` for the document being written: whether an
    /// untyped literal keys as `xsd:string` or as `rdf:PlainLiteral`. Set by
    /// [`try_save`]; consulted by [`ann_key`], which is a free function reached from
    /// a dozen call sites.
    static PLAIN_TYPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// `Model::owlapi_456` for the document being written: render every
    /// anonymous class expression inline at each reference — no blank-node
    /// numbering, no `rdf:nodeID`, an annotated axiom's `owl:annotatedTarget`
    /// carries a full copy of the expression — and stamp the 4.5.6 banner.
    /// Set by [`try_save`]; consulted where an annotated axiom or a general class
    /// axiom would otherwise share a node with its reification.
    static INLINE_ANON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the end of each rendered object — an entity's block with the
    /// nodes that follow it, an annotated IRI's, a general axiom's — is marked
    /// with an `OBJECT_END` comment, for the Turtle writer to read.
    static OBJECT_ENDS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The anonymous individuals of the document being written that appear more
    /// than once (see [`anonymous_appearances`]): each is written by its node id
    /// wherever it is an object. One that appears once is written in place.
    static ANON_REPEATED: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// How many times each anonymous individual appears in the document: as either
/// end of an object property assertion, the object of a negative one, a member
/// of a same- or different-individuals axiom, a value in a class expression or a
/// rule, or the value of an annotation. A subject an assertion is about — of a
/// class assertion, a data property assertion, an annotation assertion — is not
/// an appearance.
fn anonymous_appearances(model: &Model) -> HashMap<String, usize> {
    anonymous_references(model, true)
}

/// The anonymous individuals the document states something ABOUT elsewhere than
/// in a block of their own: every object position an individual takes (a
/// property assertion's object, an annotation value, a class expression's
/// individual, a member of a sameness or difference, a rule argument).
fn anonymous_objects(model: &Model) -> HashSet<String> {
    anonymous_references(model, false).into_keys().collect()
}

/// How many times each anonymous individual appears where an
/// [`anonymous_appearances`] count takes it; the subject of an object property
/// assertion counts only when `assertion_subjects` (its object always does).
fn anonymous_references(model: &Model, assertion_subjects: bool) -> HashMap<String, usize> {
    use horned_owl::model::{AnnotationValue as AV, Atom, IArgument};
    let mut n: HashMap<String, usize> = HashMap::new();
    fn ind(n: &mut HashMap<String, usize>, i: &Individual<RcStr>) {
        if let Individual::Anonymous(a) = i {
            *n.entry(a.0.as_ref().to_string()).or_default() += 1;
        }
    }
    fn ce(n: &mut HashMap<String, usize>, c: &CE<RcStr>) {
        match c {
            CE::ObjectHasValue { i, .. } => ind(n, i),
            CE::ObjectOneOf(v) => v.iter().for_each(|i| ind(n, i)),
            CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => v.iter().for_each(|x| ce(n, x)),
            CE::ObjectComplementOf(x) => ce(n, x),
            CE::ObjectSomeValuesFrom { bce, .. }
            | CE::ObjectAllValuesFrom { bce, .. }
            | CE::ObjectMinCardinality { bce, .. }
            | CE::ObjectMaxCardinality { bce, .. }
            | CE::ObjectExactCardinality { bce, .. } => ce(n, bce),
            _ => {}
        }
    }
    fn ann(n: &mut HashMap<String, usize>, a: &horned_owl::model::Annotation<RcStr>) {
        if let AV::AnonymousIndividual(x) = &a.av {
            *n.entry(x.0.as_ref().to_string()).or_default() += 1;
        }
    }
    fn iarg(n: &mut HashMap<String, usize>, a: &IArgument<RcStr>) {
        if let IArgument::Individual(i) = a {
            ind(n, i);
        }
    }
    for ac in model.ont.iter() {
        for a in &ac.ann {
            ann(&mut n, a);
        }
        match &ac.component {
            Component::OntologyAnnotation(o) => ann(&mut n, &o.0),
            Component::AnnotationAssertion(aa) => ann(&mut n, &aa.ann),
            Component::ObjectPropertyAssertion(a) => {
                // An assertion on an inverse is stated the other way round.
                let (from, to) = match a.ope {
                    OPE::ObjectProperty(_) => (&a.from, &a.to),
                    OPE::InverseObjectProperty(_) => (&a.to, &a.from),
                };
                if assertion_subjects {
                    ind(&mut n, from);
                }
                ind(&mut n, to);
            }
            Component::NegativeObjectPropertyAssertion(a) => ind(&mut n, &a.to),
            Component::SameIndividual(a) => a.0.iter().for_each(|i| ind(&mut n, i)),
            Component::DifferentIndividuals(a) => a.0.iter().for_each(|i| ind(&mut n, i)),
            Component::ClassAssertion(a) => ce(&mut n, &a.ce),
            Component::SubClassOf(a) => {
                ce(&mut n, &a.sub);
                ce(&mut n, &a.sup);
            }
            Component::EquivalentClasses(a) => a.0.iter().for_each(|x| ce(&mut n, x)),
            Component::DisjointClasses(a) => a.0.iter().for_each(|x| ce(&mut n, x)),
            Component::DisjointUnion(a) => a.1.iter().for_each(|x| ce(&mut n, x)),
            Component::ObjectPropertyDomain(a) => ce(&mut n, &a.ce),
            Component::ObjectPropertyRange(a) => ce(&mut n, &a.ce),
            Component::DataPropertyDomain(a) => ce(&mut n, &a.ce),
            Component::HasKey(a) => ce(&mut n, &a.ce),
            Component::Rule(r) => {
                for atom in r.head.iter().chain(r.body.iter()) {
                    match atom {
                        Atom::ClassAtom { pred, arg } => {
                            ce(&mut n, pred);
                            iarg(&mut n, arg);
                        }
                        Atom::ObjectPropertyAtom { args, .. } => {
                            iarg(&mut n, &args.0);
                            iarg(&mut n, &args.1);
                        }
                        Atom::SameIndividualAtom(a, b) | Atom::DifferentIndividualsAtom(a, b) => {
                            iarg(&mut n, a);
                            iarg(&mut n, b);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    n
}

/// An anonymous individual in object position: in place when it appears once,
/// by its node id otherwise.
fn anonymous_object(id: &str, pad: &str) -> String {
    if ANON_REPEATED.with(|r| r.borrow().contains(id)) {
        format!("{pad}<rdf:Description rdf:nodeID=\"{}\"/>\n", esc_attr(id.trim_start_matches("_:")))
    } else {
        format!("{pad}<rdf:Description/>\n")
    }
}

// === Anonymous individuals ===============================================
//
// An anonymous individual's statements are made in the graph that first
// reaches it: the graph of the entity whose statement names it as an object,
// of the general axiom whose class expression holds it, of the ontology whose
// annotation it is, of the rules that name it, or, when every statement naming
// it is about it, a graph of its own in the anonymous section. In that graph
// each statement it is the subject of is nested in its node, and each
// reification or negative assertion about it is a root; in every other graph
// its node is empty.
//
// Where it is an object it is nested, unless the document names it twice or
// more, or the graph names it as the object of two statements. Then it is
// written by id and defined once in the document: after the first block of
// the first graph to name it by id as the object of a statement, with what
// that graph states about it. What a later graph states about it is a block of
// its own, by id, after the first block of that graph naming it.

/// Opens a placeholder line for an anonymous individual in object position,
/// `{indent}\u{E000}{property}\u{E001}{individual}\n`, the property empty for a
/// member of a collection and [`ANON_ROOT`] for a root of its own.
/// [`resolve_anon`] writes it as the graph being written states it.
const ANON_SLOT: char = '\u{E000}';
const ANON_SEP: char = '\u{E001}';
/// A line `\u{E002}{node}\n` gives the block after it the node it is, which
/// orders it among the roots of its graph (see [`keyed_blocks`]).
const ROOT_KEY: char = '\u{E002}';
/// The property of a placeholder for an individual written as a root.
const ANON_ROOT: &str = "\u{E003}";

thread_local! {
    /// The anonymous individuals of the document being written from its model.
    static ANON_DOC: std::cell::RefCell<Option<AnonDoc>> = const { std::cell::RefCell::new(None) };
}

/// One statement an anonymous individual is the subject of: a property element
/// at the indent of a top-level block's body, or the type the node's element is
/// named after.
#[derive(Clone)]
enum AnonEdge {
    Element(String),
    Property(String),
}

/// What the document states about its anonymous individuals, graph by graph.
#[derive(Default)]
struct AnonDoc {
    /// Each individual's node.
    ids: HashMap<String, u64>,
    /// `genid{node}` of each individual, to the individual.
    by_gid: HashMap<String, String>,
    /// The individuals the document names twice or more.
    repeated: HashSet<String>,
    /// For a graph and an individual, how many of the graph's statements have
    /// it as their object.
    objects: HashMap<(String, String), u32>,
    /// For a graph and an individual, the statements of the graph about it, in
    /// the order they are made.
    edges: HashMap<(String, String), Vec<AnonEdge>>,
    /// For a graph, the roots its anonymous individuals add to it, by node.
    roots: HashMap<String, Vec<(u64, String)>>,
    /// For a graph, the definitions of the class expressions its anonymous
    /// individuals name by id.
    defs: HashMap<String, Vec<(String, String)>>,
    /// The individuals whose definition has been written.
    written: HashSet<String>,
    /// For an individual named by id as the object of a statement, the graph
    /// that defines it: the first to name it so.
    definer: HashMap<String, String>,
    /// For a graph and an individual named by id, whether what the graph
    /// states about the individual has been written.
    stated: HashSet<(String, String)>,
    /// The graphs and individuals whose node has been written with what the
    /// graph states about it.
    rendered: std::cell::RefCell<HashSet<(String, String)>>,
    /// The class expressions of `defs` whose definition has been written.
    written_defs: HashSet<String>,
}

/// Whether the document being written states its anonymous individuals from
/// its model.
fn anon_doc_active() -> bool {
    ANON_DOC.with(|d| d.borrow().is_some())
}

/// The anonymous individual `x` as the object of the property element `q`
/// at `pad`.
fn anon_slot(q: &str, x: &str, pad: &str) -> String {
    if anon_doc_active() {
        format!("{pad}{ANON_SLOT}{q}{ANON_SEP}{x}\n")
    } else {
        format!("{pad}<{q}>\n{pad}    <rdf:Description/>\n{pad}</{q}>\n")
    }
}

/// The anonymous individual `x` as a member of a collection at `pad`.
fn anon_item(x: &str, pad: &str) -> String {
    if anon_doc_active() {
        format!("{pad}{ANON_SLOT}{ANON_SEP}{x}\n")
    } else {
        anonymous_object(x, pad)
    }
}

/// An individual as the object of the property element `q` at `pad`.
fn individual_slot(q: &str, i: &Individual<RcStr>, pad: &str) -> String {
    match i {
        Individual::Named(n) => format!("{pad}<{q} rdf:resource=\"{}\"/>\n", esc_attr(n.0.as_ref())),
        Individual::Anonymous(a) => anon_slot(q, a.0.as_ref(), pad),
    }
}

/// The node of the anonymous individual `x`, when the document is written
/// from its model.
fn anon_node(x: &str) -> Option<u64> {
    ANON_DOC.with(|d| d.borrow().as_ref().and_then(|doc| doc.ids.get(x).copied()))
}

/// The roots the anonymous individuals add to `graph`, each after a line
/// naming its node.
fn anon_root_blocks(graph: &str) -> String {
    ANON_DOC.with(|d| {
        let d = d.borrow();
        let Some(doc) = d.as_ref() else { return String::new() };
        doc.roots
            .get(graph)
            .into_iter()
            .flatten()
            .map(|(node, block)| format!("{ROOT_KEY}{node}\n{block}"))
            .collect()
    })
}

/// `text`, top-level blocks of `graph`, with each anonymous individual written
/// as `graph` states it, and each node named by id defined after the first
/// block naming it, in the order they are named.
fn resolve_anon(graph: &str, text: &str) -> String {
    ANON_DOC.with(|d| match d.borrow_mut().as_mut() {
        Some(doc) => doc.resolve(graph, text),
        None => text.to_string(),
    })
}

/// The id a top-level block's own element carries.
fn opening_node_id(block: &str) -> Option<String> {
    let line = block.lines().find(|l| l.starts_with("    <") && !l.starts_with("    <!--"))?;
    let open = line.split_once('>').map_or(line, |(open, _)| open);
    between(open, " rdf:nodeID=\"", "\"").map(str::to_string)
}

/// The nodes `block` names by id, in order, but its own element's.
fn named_node_ids(block: &str) -> Vec<String> {
    let own = opening_node_id(block);
    let mut out = Vec::new();
    let mut first_open = true;
    for line in block.lines() {
        if line.starts_with("    <") && !line.starts_with("    </") && !line.starts_with("    <!--") && first_open {
            first_open = false;
            if own.is_some() {
                continue;
            }
        }
        let mut rest = line;
        while let Some(at) = rest.find("rdf:nodeID=\"") {
            rest = &rest[at + "rdf:nodeID=\"".len()..];
            let id = &rest[..rest.find('"').unwrap_or(0)];
            out.push(id.to_string());
        }
    }
    out
}

/// `text` split into its top-level blocks, each with the lines before it.
fn top_level_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut block = String::new();
    for line in text.split_inclusive('\n') {
        block.push_str(line);
        let top = line.starts_with("    <") && !line.starts_with("    <!--");
        if top && (line.starts_with("    </") || line.trim_end().ends_with("/>")) {
            blocks.push(std::mem::take(&mut block));
        }
    }
    if !block.is_empty() {
        blocks.push(block);
    }
    blocks
}

/// `text` with every line moved from an indent of 8 to one of `indent`.
fn reindent(text: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    text.split_inclusive('\n').map(|line| format!("{pad}{}", line.strip_prefix("        ").unwrap_or(line))).collect()
}

impl AnonDoc {
    fn node_id(&self, x: &str) -> u64 {
        self.ids.get(x).copied().unwrap_or(0)
    }

    /// Whether `graph` names `x` by id.
    fn by_id(&self, graph: &str, x: &str) -> bool {
        self.repeated.contains(x) || self.objects.get(&(graph.to_string(), x.to_string())).copied().unwrap_or(0) >= 2
    }

    fn edge(&mut self, graph: &str, x: &str, edge: AnonEdge) {
        self.edges.entry((graph.to_string(), x.to_string())).or_default().push(edge);
    }

    fn root(&mut self, graph: &str, node: u64, block: String) {
        self.roots.entry(graph.to_string()).or_default().push((node, block));
    }

    /// The node element of `x` at `pad`, with what `graph` states about it.
    fn node(&self, graph: &str, x: &str, pad: &str, id: Option<u64>, open: &mut Vec<String>) -> String {
        self.rendered.borrow_mut().insert((graph.to_string(), x.to_string()));
        let edges = self.edges.get(&(graph.to_string(), x.to_string())).cloned().unwrap_or_default();
        // The node's element is named after the last type it can be named after.
        let named = edges.iter().rposition(|e| matches!(e, AnonEdge::Element(_)));
        let mut element = "rdf:Description".to_string();
        let mut body = String::new();
        for (i, e) in edges.iter().enumerate() {
            match e {
                AnonEdge::Element(t) if Some(i) == named => element = t.clone(),
                AnonEdge::Element(t) => {
                    body.push_str(&format!("{pad}    <rdf:type rdf:resource=\"{}\"/>\n", esc_attr(&qname_iri(t))))
                }
                AnonEdge::Property(text) => body.push_str(&reindent(text, pad.len() + 4)),
            }
        }
        open.push(x.to_string());
        let body = self.expand(graph, &body, open);
        open.pop();
        let attr = id.map(|n| format!(" rdf:nodeID=\"genid{n}\"")).unwrap_or_default();
        if body.is_empty() {
            format!("{pad}<{element}{attr}/>\n")
        } else {
            format!("{pad}<{element}{attr}>\n{body}{pad}</{element}>\n")
        }
    }

    /// `text` with each placeholder written.
    fn expand(&self, graph: &str, text: &str, open: &mut Vec<String>) -> String {
        if !text.contains(ANON_SLOT) {
            return text.to_string();
        }
        let mut out = String::new();
        for line in text.split_inclusive('\n') {
            let Some(at) = line.find(ANON_SLOT) else {
                out.push_str(line);
                continue;
            };
            let pad = &line[..at];
            let rest = line[at + ANON_SLOT.len_utf8()..].trim_end_matches('\n');
            let (q, x) = rest.split_once(ANON_SEP).unwrap_or(("", rest));
            let id = self.node_id(x);
            if q == ANON_ROOT {
                let id = self.repeated.contains(x).then_some(id);
                out.push_str(&self.node(graph, x, pad, id, open));
            } else if self.by_id(graph, x) || open.iter().any(|o| o == x) {
                match q {
                    "" => out.push_str(&format!("{pad}<rdf:Description rdf:nodeID=\"genid{id}\"/>\n")),
                    q => out.push_str(&format!("{pad}<{q} rdf:nodeID=\"genid{id}\"/>\n")),
                }
            } else if q.is_empty() {
                out.push_str(&self.node(graph, x, pad, None, open));
            } else {
                let node = self.node(graph, x, &format!("{pad}    "), None, open);
                out.push_str(&format!("{pad}<{q}>\n{node}{pad}</{q}>\n"));
            }
        }
        out
    }

    fn resolve(&mut self, graph: &str, text: &str) -> String {
        let text = self.expand(graph, text, &mut Vec::new());
        let blocks = top_level_blocks(&text);
        // A root of the graph that carries its own id defines its individual,
        // with what the graph states about it.
        for b in &blocks {
            if let Some(x) = opening_node_id(b).and_then(|g| self.by_gid.get(&g).cloned()) {
                self.written.insert(x.clone());
                self.stated.insert((graph.to_string(), x));
            }
        }
        let defined = |g: &String| self.by_gid.contains_key(g) || self.defs.get(graph).is_some_and(|d| d.iter().any(|(id, _)| id == g));
        if !blocks.iter().any(|b| named_node_ids(b).iter().any(defined)) {
            return text;
        }
        // The definitions already among the blocks, of nodes an earlier block
        // names, move to where the queue puts them.
        let mut named: HashSet<String> = HashSet::new();
        let mut defs: HashMap<String, String> = HashMap::new();
        let mut kept = Vec::new();
        for b in blocks {
            if let Some(g) = opening_node_id(&b).filter(|g| named.contains(g) && !self.by_gid.contains_key(g)) {
                defs.insert(g, b);
                continue;
            }
            named.extend(named_node_ids(&b));
            kept.push(b);
        }
        let own: Vec<(String, String)> = self.defs.get(graph).cloned().unwrap_or_default();
        let mut out = String::new();
        let mut placed: HashSet<String> = HashSet::new();
        for b in kept {
            out.push_str(&b);
            let mut queue: std::collections::VecDeque<String> = named_node_ids(&b).into();
            while let Some(g) = queue.pop_front() {
                let def = match self.by_gid.get(&g).cloned() {
                    // An individual is defined once, in the graph that defines
                    // it, with what that graph states about it. What another
                    // graph states about it is a block of its own there.
                    Some(x) => {
                        let key = (graph.to_string(), x.clone());
                        let define =
                            self.definer.get(&x).is_none_or(|d| d == graph) && !self.written.contains(&x);
                        if !define && (self.stated.contains(&key) || !self.edges.contains_key(&key)) {
                            continue;
                        }
                        if define {
                            self.written.insert(x.clone());
                        }
                        self.stated.insert(key);
                        let id = self.node_id(&x);
                        self.node(graph, &x, "    ", Some(id), &mut Vec::new())
                    }
                    None => match (defs.get(&g), own.iter().find(|(id, _)| *id == g)) {
                        (Some(d), _) if placed.insert(g.clone()) => d.clone(),
                        (None, Some((_, d))) if self.written_defs.insert(g.clone()) => {
                            self.expand(graph, d, &mut Vec::new())
                        }
                        _ => continue,
                    },
                };
                queue.extend(named_node_ids(&def));
                out.push_str(&def);
            }
        }
        out
    }
}

/// The anonymous individuals the document names twice or more: once for each
/// end of an object property assertion, the object of a negative one, each
/// member of a sameness or a difference, each individual in a class expression
/// or a rule, and the value of an annotation assertion, of an ontology
/// annotation or of an annotation's annotation; twice for the value of an
/// axiom's own annotation. What a class, data or negative data assertion is
/// about does not count, nor does an annotation assertion's subject.
fn anonymous_multiples(model: &Model) -> HashSet<String> {
    use horned_owl::model::{Atom, IArgument};
    fn add(n: &mut HashMap<String, u32>, x: &str) {
        *n.entry(x.to_string()).or_default() += 1;
    }
    fn ind(n: &mut HashMap<String, u32>, i: &Individual<RcStr>) {
        if let Individual::Anonymous(a) = i {
            add(n, a.0.as_ref());
        }
    }
    fn ce(n: &mut HashMap<String, u32>, c: &CE<RcStr>) {
        match c {
            CE::ObjectHasValue { i, .. } => ind(n, i),
            CE::ObjectOneOf(v) => v.iter().for_each(|i| ind(n, i)),
            _ => crate::io::genid::sub_expressions(c).into_iter().for_each(|s| ce(n, s)),
        }
    }
    fn value(n: &mut HashMap<String, u32>, av: &AnnotationValue<RcStr>) {
        if let AnnotationValue::AnonymousIndividual(a) = av {
            add(n, a.0.as_ref());
        }
    }
    fn nested(n: &mut HashMap<String, u32>, anns: &BTreeSet<horned_owl::model::Annotation<RcStr>>) {
        for a in anns {
            value(n, &a.av);
            nested(n, &a.ann);
        }
    }
    fn iarg(n: &mut HashMap<String, u32>, a: &IArgument<RcStr>) {
        if let IArgument::Individual(i) = a {
            ind(n, i);
        }
    }
    let mut n: HashMap<String, u32> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::OntologyAnnotation(oa) = &ac.component {
            value(&mut n, &oa.0.av);
            continue;
        }
        for a in &ac.ann {
            value(&mut n, &a.av);
        }
        nested(&mut n, &ac.ann);
        match &ac.component {
            Component::ClassAssertion(a) => ce(&mut n, &a.ce),
            Component::ObjectPropertyAssertion(a) => {
                ind(&mut n, &a.from);
                ind(&mut n, &a.to);
            }
            Component::NegativeObjectPropertyAssertion(a) => ind(&mut n, &a.to),
            Component::SameIndividual(a) => a.0.iter().for_each(|i| ind(&mut n, i)),
            Component::DifferentIndividuals(a) => a.0.iter().for_each(|i| ind(&mut n, i)),
            Component::AnnotationAssertion(a) => value(&mut n, &a.ann.av),
            Component::SubClassOf(a) => {
                ce(&mut n, &a.sub);
                ce(&mut n, &a.sup);
            }
            Component::EquivalentClasses(a) => a.0.iter().for_each(|x| ce(&mut n, x)),
            Component::DisjointClasses(a) => a.0.iter().for_each(|x| ce(&mut n, x)),
            Component::DisjointUnion(a) => a.1.iter().for_each(|x| ce(&mut n, x)),
            Component::ObjectPropertyDomain(a) => ce(&mut n, &a.ce),
            Component::ObjectPropertyRange(a) => ce(&mut n, &a.ce),
            Component::DataPropertyDomain(a) => ce(&mut n, &a.ce),
            Component::HasKey(a) => ce(&mut n, &a.ce),
            Component::Rule(r) => {
                for atom in r.body.iter().chain(r.head.iter()) {
                    match atom {
                        Atom::ClassAtom { pred, arg } => {
                            iarg(&mut n, arg);
                            ce(&mut n, pred);
                        }
                        Atom::ObjectPropertyAtom { args, .. } => {
                            iarg(&mut n, &args.0);
                            iarg(&mut n, &args.1);
                        }
                        Atom::SameIndividualAtom(_, b) => iarg(&mut n, b),
                        Atom::DifferentIndividualsAtom(a, _) => iarg(&mut n, a),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    n.into_iter().filter(|(_, c)| *c > 1).map(|(x, _)| x).collect()
}

/// An `<owl:Axiom>` block reifying the statement of `prop` from `source` to
/// `target` (both complete property element lines), with `anns`.
fn anon_reification(source: &str, prop: &str, target: &str, anns: &str) -> String {
    format!(
        "    <owl:Axiom>\n{source}        <owl:annotatedProperty rdf:resource=\"{}\"/>\n{target}{anns}    </owl:Axiom>\n",
        esc_attr(prop)
    )
}

/// The `owl:annotatedTarget` of a statement whose object is `i`: an
/// anonymous individual by id.
fn individual_target(i: &Individual<RcStr>) -> String {
    match i {
        Individual::Named(n) => format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(n.0.as_ref())),
        Individual::Anonymous(a) => {
            format!("        <owl:annotatedTarget rdf:nodeID=\"genid{}\"/>\n", anon_node(a.0.as_ref()).unwrap_or(0))
        }
    }
}

/// A negative assertion's node, `source` its source individual, `prop` its
/// assertion property and `target` its target, each a complete line.
fn negative_assertion(source: &str, prop: &str, target: &str, anns: &str) -> String {
    format!("    <rdf:Description>\n        <rdf:type rdf:resource=\"{NEG_PA}\"/>\n{source}{prop}{target}{anns}    </rdf:Description>\n")
}

/// The `owl:assertionProperty` of a negative object property assertion.
fn assertion_property(ope: &OPEx<RcStr>) -> String {
    match ope {
        OPEx::ObjectProperty(p) => {
            format!("        <owl:assertionProperty rdf:resource=\"{}\"/>\n", esc_attr(p.0.as_ref()))
        }
        OPEx::InverseObjectProperty(p) => format!(
            "        <owl:assertionProperty>\n            <rdf:Description>\n                <owl:inverseOf rdf:resource=\"{}\"/>\n            </rdf:Description>\n        </owl:assertionProperty>\n",
            esc_attr(p.0.as_ref())
        ),
    }
}

/// The statements about the anonymous individuals of `model`, graph by graph,
/// from the numbering pass `g`. An axiom the layout cannot state that way is
/// added to `left_out`.
fn build_anon_doc<'m>(
    model: &'m Model,
    g: &crate::io::genid::Genids,
    prefixes: &[(String, String)],
    left_out: &mut Vec<&'m AnnotatedComponent<RcStr>>,
) -> AnonDoc {
    use crate::io::genid::{axiom_identity, names_anonymous, GENERAL_GRAPH};
    let mut doc = AnonDoc {
        ids: g.anon_ids.clone(),
        repeated: anonymous_multiples(model),
        objects: g.anon_objects.clone(),
        ..Default::default()
    };
    doc.by_gid = doc.ids.iter().map(|(x, n)| (format!("genid{n}"), x.clone())).collect();
    let mut by_key: HashMap<u64, &AnnotatedComponent<RcStr>> = HashMap::new();
    for ac in model.ont.iter() {
        if names_anonymous(&ac.component) {
            by_key.insert(axiom_identity(ac), ac);
        }
    }
    // An axiom about an anonymous individual that no graph reaches is stated
    // nowhere.
    for key in g.unreached() {
        if let Some(ac) = by_key.get(&key) {
            left_out.push(ac);
        }
    }
    let rdf_type = format!("{RDF_NS}type");
    for key in &g.anon_order {
        let (Some(ac), Some(graph)) = (by_key.get(key).copied(), g.anon_home.get(key)) else { continue };
        let nodes = g.anon_reif.get(key).cloned().unwrap_or_default();
        let node = nodes.first().copied().unwrap_or(0);
        let anns = node_annotations(ac, prefixes);
        let annotated = !ac.ann.is_empty();
        match &ac.component {
            Component::ClassAssertion(ax) => {
                let Individual::Anonymous(a) = &ax.i else { continue };
                let x = a.0.as_ref();
                // The target of an annotated assertion is named by id.
                let target = match &ax.ce {
                    CE::Class(c) if matches!(c.0.as_ref(), "http://www.w3.org/2002/07/owl#Thing" | "http://www.w3.org/2002/07/owl#Nothing") => {
                        doc.edge(graph, x, AnonEdge::Element(qname(c.0.as_ref(), prefixes)));
                        format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref()))
                    }
                    CE::Class(c) => {
                        let edge = format!("        <rdf:type rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref()));
                        doc.edge(graph, x, AnonEdge::Property(edge));
                        format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref()))
                    }
                    ce if !annotated => {
                        let edge = format!("        <rdf:type>\n{}        </rdf:type>\n", render_ce(ce, 12, &Genids::new()));
                        doc.edge(graph, x, AnonEdge::Property(edge));
                        String::new()
                    }
                    ce => match g.anon_ce.get(key) {
                        Some(id) => {
                            let gid = format!("genid{id}");
                            doc.edge(graph, x, AnonEdge::Property(format!("        <rdf:type rdf:nodeID=\"{gid}\"/>\n")));
                            let def = inject_nodeid(&render_ce(ce, 4, &Genids::new()), &gid);
                            doc.defs.entry(graph.clone()).or_default().push((gid.clone(), def));
                            format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n")
                        }
                        None => {
                            left_out.push(ac);
                            continue;
                        }
                    },
                };
                if annotated {
                    let block = anon_reification(&anon_slot("owl:annotatedSource", x, "        "), &rdf_type, &target, &anns);
                    doc.root(graph, node, block);
                }
            }
            Component::ObjectPropertyAssertion(ax) => {
                let (OPEx::ObjectProperty(p), Individual::Anonymous(a)) = (&ax.ope, &ax.from) else {
                    // On an inverse it is stated the other way round, without its
                    // annotations: of a named individual in its own block, of an
                    // anonymous one where it is.
                    let OPEx::InverseObjectProperty(p) = &ax.ope else { continue };
                    match &ax.to {
                        _ if annotated => left_out.push(ac),
                        Individual::Named(n) if n.0.as_ref() == graph => {}
                        Individual::Named(_) => left_out.push(ac),
                        Individual::Anonymous(y) => {
                            let q = qname(p.0.as_ref(), prefixes);
                            doc.edge(graph, y.0.as_ref(), AnonEdge::Property(individual_slot(&q, &ax.from, "        ")));
                        }
                    }
                    continue;
                };
                let x = a.0.as_ref();
                let q = qname(p.0.as_ref(), prefixes);
                doc.edge(graph, x, AnonEdge::Property(individual_slot(&q, &ax.to, "        ")));
                if annotated {
                    let block = anon_reification(
                        &anon_slot("owl:annotatedSource", x, "        "),
                        p.0.as_ref(),
                        &individual_target(&ax.to),
                        &anns,
                    );
                    doc.root(graph, node, block);
                }
            }
            Component::DataPropertyAssertion(ax) => {
                let Individual::Anonymous(a) = &ax.from else { continue };
                let x = a.0.as_ref();
                let q = qname(ax.dp.0.as_ref(), prefixes);
                doc.edge(graph, x, AnonEdge::Property(render_literal_tag(&q, &ax.to, 8)));
                if annotated {
                    let block = anon_reification(
                        &anon_slot("owl:annotatedSource", x, "        "),
                        ax.dp.0.as_ref(),
                        &render_literal_tag("owl:annotatedTarget", &ax.to, 8),
                        &anns,
                    );
                    doc.root(graph, node, block);
                }
            }
            Component::NegativeObjectPropertyAssertion(ax) => {
                let Individual::Anonymous(a) = &ax.from else { continue };
                let block = negative_assertion(
                    &anon_slot("owl:sourceIndividual", a.0.as_ref(), "        "),
                    &assertion_property(&ax.ope),
                    &individual_slot("owl:targetIndividual", &ax.to, "        "),
                    &anns,
                );
                doc.root(graph, node, block);
            }
            Component::NegativeDataPropertyAssertion(ax) => {
                let Individual::Anonymous(a) = &ax.from else { continue };
                let block = negative_assertion(
                    &anon_slot("owl:sourceIndividual", a.0.as_ref(), "        "),
                    &format!("        <owl:assertionProperty rdf:resource=\"{}\"/>\n", esc_attr(ax.dp.0.as_ref())),
                    &render_literal_tag("owl:targetValue", &ax.to, 8),
                    &anns,
                );
                doc.root(graph, node, block);
            }
            Component::AnnotationAssertion(ax) => {
                let horned_owl::model::AnnotationSubject::AnonymousIndividual(a) = &ax.subject else { continue };
                let x = a.0.as_ref();
                let p = ax.ann.ap.0.as_ref();
                doc.edge(graph, x, AnonEdge::Property(render_ann(p, &ax.ann.av, prefixes)));
                if annotated {
                    let target = match &ax.ann.av {
                        AnnotationValue::AnonymousIndividual(v) => {
                            format!("        <owl:annotatedTarget rdf:nodeID=\"genid{}\"/>\n", doc.node_id(v.0.as_ref()))
                        }
                        av => render_target(av),
                    };
                    let block = anon_reification(&anon_slot("owl:annotatedSource", x, "        "), p, &target, &anns);
                    doc.root(graph, node, block);
                }
            }
            Component::SameIndividual(_) | Component::DifferentIndividuals(_) => {
                let (members, pred, same) = match &ac.component {
                    Component::SameIndividual(s) => (&s.0, "http://www.w3.org/2002/07/owl#sameAs", true),
                    Component::DifferentIndividuals(d) => (&d.0, "http://www.w3.org/2002/07/owl#differentFrom", false),
                    _ => unreachable!(),
                };
                if !same && members.len() > 2 {
                    // An `owl:AllDifferent` node of the graph that reaches it: the
                    // general axiom's own, or another's root.
                    if *graph != format!("{GENERAL_GRAPH}{key}") {
                        let named: Vec<String> = slot_members(members).into_iter().flatten().collect();
                        doc.root(graph, node, render_all_different(&named, &anns));
                    }
                    continue;
                }
                let pairs = crate::io::genid::individual_pair_list(members, &ac.ann, same);
                let host = members.iter().min_by(|a, b| crate::io::owlfunc::cmp_individual(a, b));
                let q = qname(pred, prefixes);
                for (i, (a, b)) in pairs.iter().enumerate() {
                    match a {
                        Individual::Anonymous(x) => {
                            doc.edge(graph, x.0.as_ref(), AnonEdge::Property(individual_slot(&q, b, "        ")))
                        }
                        // A named member's pair is stated in the block of the
                        // first member, or in a root block of that member's
                        // graph: the axiom has to be in that graph.
                        Individual::Named(_) => {
                            let host_graph = match host {
                                Some(Individual::Named(h)) => h.0.as_ref() == graph,
                                _ => false,
                            };
                            if !host_graph {
                                left_out.push(ac);
                            }
                        }
                    }
                    if annotated {
                        let source = match a {
                            Individual::Anonymous(x) => anon_slot("owl:annotatedSource", x.0.as_ref(), "        "),
                            Individual::Named(n) => {
                                format!("        <owl:annotatedSource rdf:resource=\"{}\"/>\n", esc_attr(n.0.as_ref()))
                            }
                        };
                        let block = anon_reification(&source, pred, &individual_target(b), &anns);
                        doc.root(graph, nodes.get(i).copied().unwrap_or(0), block);
                    }
                }
            }
            _ => {}
        }
    }
    // The axioms naming an anonymous individual as the object of a statement
    // about a named subject, in the subject's graph.
    for ac in model.ont.iter() {
        if !names_anonymous(&ac.component) || g.is_reachable(axiom_identity(ac)) {
            continue;
        }
        let nodes = g.anon_reif.get(&axiom_identity(ac)).cloned().unwrap_or_default();
        let node = nodes.first().copied().unwrap_or(0);
        let anns = node_annotations(ac, prefixes);
        match &ac.component {
            Component::ObjectPropertyAssertion(ax) => match (&ax.ope, &ax.from, &ax.to) {
                (OPEx::ObjectProperty(p), Individual::Named(s), Individual::Anonymous(_)) => {
                    if !ac.ann.is_empty() {
                        let source = format!("        <owl:annotatedSource rdf:resource=\"{}\"/>\n", esc_attr(s.0.as_ref()));
                        let block = anon_reification(&source, p.0.as_ref(), &individual_target(&ax.to), &anns);
                        doc.root(s.0.as_ref(), node, block);
                    }
                }
                // On an inverse it is stated the other way round, of the
                // anonymous individual, in the graph of the named one: a root
                // of that graph when nothing there names the individual.
                (OPEx::InverseObjectProperty(p), Individual::Named(s), Individual::Anonymous(x)) if ac.ann.is_empty() => {
                    let q = qname(p.0.as_ref(), prefixes);
                    doc.edge(s.0.as_ref(), x.0.as_ref(), AnonEdge::Property(individual_slot(&q, &ax.from, "        ")));
                }
                (OPEx::InverseObjectProperty(_), _, _) => left_out.push(ac),
                _ => {}
            },
            Component::NegativeObjectPropertyAssertion(ax) => {
                if let (Individual::Named(s), Individual::Anonymous(_)) = (&ax.from, &ax.to) {
                    let block = negative_assertion(
                        &format!("        <owl:sourceIndividual rdf:resource=\"{}\"/>\n", esc_attr(s.0.as_ref())),
                        &assertion_property(&ax.ope),
                        &individual_slot("owl:targetIndividual", &ax.to, "        "),
                        &anns,
                    );
                    doc.root(s.0.as_ref(), node, block);
                }
            }
            Component::AnnotationAssertion(ax) => {
                if let (horned_owl::model::AnnotationSubject::IRI(s), AnnotationValue::AnonymousIndividual(v)) =
                    (&ax.subject, &ax.ann.av)
                {
                    if !ac.ann.is_empty() {
                        let source = format!("        <owl:annotatedSource rdf:resource=\"{}\"/>\n", esc_attr(s.as_ref()));
                        let target =
                            format!("        <owl:annotatedTarget rdf:nodeID=\"genid{}\"/>\n", doc.node_id(v.0.as_ref()));
                        let block = anon_reification(&source, ax.ann.ap.0.as_ref(), &target, &anns);
                        doc.root(s.as_ref(), node, block);
                    }
                }
            }
            _ => {}
        }
    }
    // An individual a graph states something about and names as no statement's
    // object is a root of that graph.
    let mut own_roots: Vec<(String, String)> = doc
        .edges
        .keys()
        .filter(|(graph, x)| doc.objects.get(&(graph.clone(), x.clone())).copied().unwrap_or(0) == 0)
        .cloned()
        .collect();
    own_roots.sort();
    for (graph, x) in own_roots {
        let node = doc.node_id(&x);
        doc.root(&graph, node, format!("    {ANON_SLOT}{ANON_ROOT}{ANON_SEP}{x}\n"));
    }
    // An individual named by id is defined by the first graph to name it by id
    // as the object of a statement; what a later graph states about it is a
    // block of its own there.
    let position: HashMap<&str, usize> = g.graphs.iter().enumerate().map(|(i, k)| (k.as_str(), i)).rev().collect();
    let mut first: HashMap<&str, (usize, &str)> = HashMap::new();
    for ((graph, x), n) in &doc.objects {
        if *n > 0 && doc.by_id(graph, x) {
            let at = (position.get(graph.as_str()).copied().unwrap_or(usize::MAX), graph.as_str());
            let e = first.entry(x.as_str()).or_insert(at);
            *e = (*e).min(at);
        }
    }
    let definer: HashMap<String, String> =
        first.into_iter().map(|(x, (_, graph))| (x.to_string(), graph.to_string())).collect();
    doc.definer = definer;
    doc
}

/// Run `f` with the end of every rendered object marked (see `OBJECT_ENDS`).
pub(crate) fn with_object_ends<T>(f: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            OBJECT_ENDS.with(|c| c.set(false));
        }
    }
    OBJECT_ENDS.with(|c| c.set(true));
    let _reset = Reset;
    f()
}

/// Mark the end of a rendered object, when asked to.
fn end_object<W: Write>(w: &mut W) -> Result<()> {
    if OBJECT_ENDS.with(|c| c.get()) {
        writeln!(w, "    <!--{}-->", crate::io::owlapi_ttl::OBJECT_END)?;
    }
    Ok(())
}

/// Whether this document renders anonymous class expressions inline everywhere
/// — see [`crate::model::Model::owlapi_456`].
pub(crate) fn inline_anon() -> bool {
    INLINE_ANON.with(|c| c.get())
}

/// The datatype IRI an untyped literal keys as in this document — see
/// [`crate::model::Model::plain_literals_typed`].
pub(crate) fn plain_datatype() -> &'static str {
    if PLAIN_TYPED.with(|c| c.get()) {
        XSD_STRING
    } else {
        RDF_PLAIN_LITERAL
    }
}

/// `rdf:PlainLiteral`, the datatype an untyped literal keys as unless this
/// document types its untyped literals `xsd:string`.
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// The xmlns block this writer declares for `model`, as `(prefix, namespace)`
/// pairs in document order.
///
/// The set = the input document's format prefixes (`model.rdf_prefixes`, kept
/// verbatim even when unused, e.g. `its`/`swrl`) ∪ entity-derived prefixes for
/// the namespaces of every signature entity + annotation property not already
/// declared (named from the built-in owl/rdfs/rdf/xsd/dc/skos bindings or by
/// `generate_prefix`) ∪ the structural `xml` namespace. Order: by prefix length,
/// then lexicographically.
///
/// Exposed so the executor can stamp it back onto the model after writing: the
/// next build step re-reads the file and picks the prefixes up from its
/// `rdf:RDF` element, and om's in-memory OFN cache stands in for that read.
/// Without it the generated prefixes (`doap`, `protege` — see
/// `Model::closure_ann_ns`) live only in the bytes on disk and every downstream
/// artefact drops them.
pub fn document_prefixes(model: &Model) -> Vec<(String, String)> {
    owlapi_prefixes(model)
}

fn owlapi_prefixes(model: &Model) -> Vec<(String, String)> {
    // namespace -> prefix, seeded with the built-in bindings.
    let mut ns2p: BTreeMap<String, String> = BTreeMap::new();
    for (p, ns) in [
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ("dc", "http://purl.org/dc/elements/1.1/"),
        // NOT `dcterms`. The built-in bindings are owl/rdfs/rdf/xsd/
        // dc(=elements/1.1/)/skos only; `http://purl.org/dc/terms/` goes through
        // `generate_prefix`, which takes the trailing NCName run and so yields
        // `terms` — the prefix released OBO files declare that namespace under
        // when the document format carries none of its own.
        ("oboInOwl", "http://www.geneontology.org/formats/oboInOwl#"),
        ("skos", "http://www.w3.org/2004/02/skos/core#"),
        ("xml", "http://www.w3.org/XML/1998/namespace"),
    ] {
        ns2p.insert(ns.to_string(), p.to_string());
    }

    // Format prefixes from the input document, kept verbatim (prefix -> ns). When
    // the source carried an RDF/XML xmlns it lives in `rdf_prefixes`; but an
    // OBO-sourced `om make` pipeline (mondo-edit.obo → … → mondo.owl) never has an
    // input xmlns, so fall back to `idspaces` (the document's declared prefix set,
    // preserved through the pipeline via carry_meta). Entity-derived namespaces then
    // add the used annotation/property prefixes on top.
    let format_prefixes: Vec<(String, String)> = if model.format_prefixes_cleared {
        // A freshly created ontology carries no INHERITED format prefixes — see
        // `Model::format_prefixes_cleared`. What its own construction bound is a
        // different thing and is declared: a model built from a table brings the
        // table's CURIE prefixes with it (`Model::built_prefixes`).
        model.built_prefixes.clone()
    } else if !model.rdf_prefixes.is_empty() {
        model.rdf_prefixes.clone()
    } else if !model.idspaces.is_empty() {
        // The document's own declarations come FIRST so that, when two prefixes
        // share a namespace, the document's wins the one-prefix-per-namespace race
        // below: MONDO declares `terms` for dc/terms/ while om's built-ins add
        // `dcterms`, and `terms` is the one its releases carry. The rest of the
        // CURIE map follows, minus the explicit `--add-prefixes` context (those
        // are OBO idspaces, not OWL xmlns).
        let explicit: HashSet<&str> =
            model.explicit_prefixes.iter().map(|(p, _)| p.as_str()).collect();
        model
            .idspaces
            .iter()
            .cloned()
            .chain(
                model
                    .prefixes
                    .mappings()
                    .filter(|(p, _)| !explicit.contains(p.as_str()))
                    .map(|(p, ns)| (p.clone(), ns.clone())),
            )
            .collect()
    } else {
        // OBO-sourced pipeline with no input xmlns and no scanned idspaces: use the
        // ontology's CURIE prefix map (the default OBO context — dc, dcterms,
        // oboInOwl, obo, rdf(s), xsd, its, swrl, …) as the format-prefix set,
        // MINUS the explicit `--add-prefixes` context (config/prefixes.jsonld xref
        // prefixes like ICD11/EFO belong to the OBO idspaces, NOT the OWL xmlns).
        let explicit: HashSet<&str> =
            model.explicit_prefixes.iter().map(|(p, _)| p.as_str()).collect();
        model
            .prefixes
            .mappings()
            .filter(|(p, _)| !explicit.contains(p.as_str()))
            .map(|(p, ns)| (p.clone(), ns.clone()))
            .collect()
    };
    // The prefixes the command line adds go on top, a name they bind replacing
    // the source's binding of it.
    let format_prefixes: Vec<(String, String)> = format_prefixes
        .into_iter()
        .filter(|(p, _)| !model.added_prefixes.iter().any(|(a, _)| a == p))
        .chain(model.added_prefixes.iter().cloned())
        .collect();
    let mut out_map: BTreeMap<String, String> = BTreeMap::new();
    let mut declared_ns: HashSet<String> = HashSet::new();
    for (p, ns) in &format_prefixes {
        if p.is_empty() {
            continue;
        }
        // One prefix per namespace: the xmlns block never declares two prefixes
        // for the same IRI (MONDO's map binds both `terms` and `dcterms` to
        // http://purl.org/dc/terms/ but the released mondo.owl declares only
        // `terms`). First declaration wins, matching the format prefix order.
        if declared_ns.contains(ns.as_str()) {
            continue;
        }
        // …and one namespace per prefix. A format map that points an already-taken
        // prefix NAME at some other namespace cannot steal it: om's CURIE map binds
        // `dc` to dc/TERMS/ (that is what `dc:contributor` expands with) while the
        // built-in binding above holds `dc` for dc/elements/1.1/. Letting it
        // through would declare `xmlns:dc="…/dc/terms/"`, the entity sweep below
        // would overwrite the same key with elements/1.1/, and dc/terms/ would be
        // left with NO prefix at all — so the writer would emit the raw IRI as an
        // element name, which is not well-formed XML and which om could not read
        // back. Both are declared instead: `dc` for elements/1.1/ and the
        // generated `terms` for dc/terms/.
        if out_map.contains_key(p) || ns2p.iter().any(|(k, v)| v == p && k != ns) {
            continue;
        }
        ns2p.insert(ns.clone(), p.clone());
        out_map.insert(p.clone(), ns.clone());
        declared_ns.insert(ns.clone());
    }

    // Entity-derived namespaces, collected from ONLY: object properties used in
    // ObjectPropertyAssertion axioms, data properties used in
    // DataPropertyAssertion axioms, and ALL annotation properties in signature.
    // (Classes, individuals and restriction-only object properties are excluded,
    // so xref/gene namespaces like hgnc/ncbigene/nbo are NOT declared.)
    use horned_owl::model::ObjectPropertyExpression as OPE;
    // Each entity type seeds the entity hash with its own prime, folded as
    // `hash = 31*hash + owlapi_iri_hash(iri)`.
    const PRIME_DATA_PROPERTY: u32 = 4073;
    const PRIME_OBJECT_PROPERTY: u32 = 4153;
    const PRIME_ANNOTATION_PROPERTY: u32 = 6067;
    let mut sig_ns: BTreeSet<String> = BTreeSet::new();
    // …and the ENTITIES behind them, because the order they are visited decides
    // which namespace keeps the bare prefix. The entity set is walked in hash-
    // bucket order and each namespace is given a prefix, with a SHARED,
    // monotonically increasing counter appended on a collision. HPO has four
    // `…/obo/chebi/*` namespaces (from `1_STAR`, `2_STAR`, `3_STAR`, and the
    // unnumbered `INN`/`IUPAC_NAME`/…), all computing the prefix `chebi`; its
    // releases carry chebi=…/2, chebi1=…/3, chebi2=…/1, chebi3=…/, which is that
    // bucket order, not the ascending order a sorted sweep gives. Each entry is
    // (entity-type prime for the hash, IRI).
    let mut sig_ents: BTreeSet<(u32, String)> = BTreeSet::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::ObjectPropertyAssertion(opa) => {
                let op = match &opa.ope {
                    OPE::ObjectProperty(p) => p.0.as_ref().to_string(),
                    OPE::InverseObjectProperty(p) => p.0.as_ref().to_string(),
                };
                sig_ns.insert(ncname_split(&op).0.to_string());
                sig_ents.insert((PRIME_OBJECT_PROPERTY, op));
            }
            Component::DataPropertyAssertion(dpa) => {
                sig_ns.insert(ncname_split(dpa.dp.0.as_ref()).0.to_string());
                sig_ents.insert((PRIME_DATA_PROPERTY, dpa.dp.0.as_ref().to_string()));
            }
            _ => {}
        }
        // `ncname_split`, not `iri_namespace`: this namespace has to be the one an
        // element name will be abbreviated against, and the two rules part company
        // on an IRI whose last `#`/`/` segment is not an XML name.
        for iri in crate::sig::annotation_properties(&ac.component) {
            sig_ns.insert(ncname_split(&iri).0.to_string());
            sig_ents.insert((PRIME_ANNOTATION_PROPERTY, iri));
        }
        // …and the properties of the axiom's OWN annotations, which are part of
        // the signature too but are not reachable from the component. EFO's
        // `cl`/`uberon` imports use `sssom:mapping_justification` only inside
        // `owl:Axiom` reifications; without this sweep `xmlns:sssom` goes
        // undeclared and the writer emits the raw IRI as an element name, which
        // is not well-formed XML.
        for a in ac.ann.iter() {
            sig_ns.insert(ncname_split(a.ap.0.as_ref()).0.to_string());
            sig_ents.insert((PRIME_ANNOTATION_PROPERTY, a.ap.0.as_ref().to_string()));
        }
    }
    // …and the same collection runs over the IMPORT CLOSURE, which is how MONDO's
    // `filtered.owl` declares `xmlns:doap`/`xmlns:protege` without using either:
    // the properties are declared in `merged_import.owl` /
    // `omo_import.owl`, imported but not collapsed. Recorded at build time because
    // the writer cannot resolve the catalog itself.
    sig_ns.extend(model.closure_ann_ns.iter().cloned());
    // The structural namespaces every document declares, whatever the ontology
    // contains: `xml`, and the four RDF/OWL vocabularies. They usually arrive via
    // the document format's prefix map, but a fresh ontology has none
    // (`Model::format_prefixes_cleared`) and all five must still be written. The
    // output of MONDO's mondo-simple `filter` shows it: its xmlns block is
    // `xmlns=`, dc, obo, owl, rdf, xml, xsd, foaf, rdfs, skos, mondo, sssom, terms,
    // vocab, oboInOwl: everything but owl/rdf/rdfs/xsd/xml is used by some entity.
    for ns in [
        "http://www.w3.org/XML/1998/namespace",
        "http://www.w3.org/2002/07/owl#",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2000/01/rdf-schema#",
        "http://www.w3.org/2001/XMLSchema#",
    ] {
        sig_ns.insert(ns.to_string());
    }
    // The SWRL vocabularies are declared as a PAIR, and only when the ontology
    // carries at least one rule. Across OBA's release: `oba-full.owl`
    // and `oba.owl` hold 42 rules (they merge `imports/merged_import.owl`, which
    // brings RO's) and declare both `swrl` and `swrlb`; `oba-base.owl` and
    // `oba-basic.owl` hold none and declare neither, as does the whole of MONDO.
    // Neither namespace appears on an entity, so the sweep above cannot find them.
    if model.ont.iter().any(|ac| matches!(ac.component, Component::Rule(_))) {
        sig_ns.insert("http://www.w3.org/2003/11/swrl#".to_string());
        sig_ns.insert("http://www.w3.org/2003/11/swrlb#".to_string());
    }

    // Visit order: the entities first, in hash-bucket order (bucket index over
    // `h ^ (h >> 16)`, masked to the entity set's table size). Namespaces with no
    // entity behind them — the five structural vocabularies, SWRL, and the
    // import-closure properties recorded at build time — cannot be placed in that
    // order, so they follow, sorted; none of them collides, since all are
    // pre-registered in `ns2p`.
    let ordered_ns: Vec<String> = {
        let cap = crate::io::obo::owlapi_set_cap(sig_ents.len());
        let mut buckets: Vec<Vec<&str>> = vec![Vec::new(); cap];
        for (prime, iri) in &sig_ents {
            let h = (*prime as i32)
                .wrapping_mul(31)
                .wrapping_add(crate::io::obo::owlapi_iri_hash(iri)) as u32;
            let spread = h ^ (h >> 16);
            buckets[(spread as usize) & (cap - 1)].push(iri);
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for iri in buckets.into_iter().flatten() {
            let ns = ncname_split(iri).0.to_string();
            if seen.insert(ns.clone()) {
                out.push(ns);
            }
        }
        for ns in &sig_ns {
            if !seen.contains(ns) {
                out.push(ns.clone());
            }
        }
        out
    };
    // One counter shared across every generated prefix, not one per base. A
    // namespace bound only to the empty prefix — the document's default one
    // included — gets a prefix of its own like any other.
    let mut candidate_index = 1u32;
    for ns in &ordered_ns {
        if ns.is_empty() || declared_ns.contains(ns) {
            continue;
        }
        // The prefix for this namespace: its registered name, or `generate_prefix`
        // plus a counter. A registered name another namespace has already taken is
        // not usable either, so fall through to generation — which is how the two
        // dc/terms/ namespaces OBA's edit file carries end up as `terms` and
        // `terms1`.
        let prefix = match ns2p.get(ns) {
            Some(p) if !out_map.contains_key(p) => p.clone(),
            _ => {
                let base = generate_prefix(ns);
                let mut cand = base.clone();
                while out_map.contains_key(&cand)
                    || ns2p.iter().any(|(k2, v)| v == &cand && k2 != ns)
                {
                    cand = format!("{base}{candidate_index}");
                    candidate_index += 1;
                }
                cand
            }
        };
        out_map.insert(prefix.clone(), ns.clone());
        declared_ns.insert(ns.clone());
    }

    let mut out: Vec<(String, String)> = out_map.into_iter().collect();
    out.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The OWL namespace, without its trailing `#` — the `xml:base` a document with
/// no ontology IRI of its own carries.
pub(crate) const OWL_NS_BASE: &str = "http://www.w3.org/2002/07/owl";

/// Rewrite `<owl:X …>`/`</owl:X>` to `<X …>`/`</X>` throughout a finished
/// document.
///
/// Applied only when the OWL namespace is the document's DEFAULT namespace, where
/// the prefixed and unprefixed spellings denote the same element and the reference
/// writer uses the unprefixed one.
///
/// Done here, at the boundary, rather than at each of the ~140 places the writer
/// emits a tag — and deliberately so. The writer keeps ONE internal spelling, which
/// is also the spelling its own reification helpers (`reif_signature`,
/// `nested_key`) scan for; renaming at emission would leave those
/// matching text that no longer exists, and they would fail silently.
///
/// Safe as a textual pass because `<` occurs in well-formed XML only where a tag
/// opens: an attribute value cannot contain it, and character data escapes it as
/// `&lt;`.
fn strip_default_owl_prefix(doc: &str) -> String {
    let mut out = String::with_capacity(doc.len());
    let mut rest = doc;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let after = if let Some(r) = rest.strip_prefix("</owl:") {
            out.push_str("</");
            r
        } else if let Some(r) = rest.strip_prefix("<owl:") {
            out.push('<');
            r
        } else {
            out.push('<');
            &rest[1..]
        };
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Write the RDF/XML header and `owl:Ontology` block. Returns the ontology IRI
/// (for callers that continue with the entity sections).
pub fn write_header_and_ontology<W: Write>(
    model: &Model,
    prefixes: &[(String, String)],
    w: &mut W,
) -> Result<Option<String>> {
    // Ontology id + version + imports + annotations from the components.
    let mut ont_iri: Option<String> = None;
    let mut version_iri: Option<String> = None;
    let mut imports: Vec<String> = Vec::new();
    let mut ont_anns: Vec<(String, AnnotationValue<RcStr>)> = Vec::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::OntologyID(id) => {
                if let Some(i) = &id.iri {
                    ont_iri = Some(i.as_ref().to_string());
                }
                if let Some(v) = &id.viri {
                    version_iri = Some(v.as_ref().to_string());
                }
            }
            Component::Import(im) => imports.push(im.0.as_ref().to_string()),
            Component::OntologyAnnotation(oa) => {
                ont_anns.push((oa.0.ap.0.as_ref().to_string(), oa.0.av.clone()));
            }
            _ => {}
        }
    }

    // With no ontology IRI there is no document namespace to make the default, and
    // the OWL namespace takes the position — `xmlns="…owl#"`, `xml:base="…owl"`.
    // Writing the empty string instead produced `xmlns="#"`, and it is not a
    // cosmetic difference: with OWL as the default namespace every OWL element is
    // written UNPREFIXED, which is what [`strip_default_owl_prefix`] then does.
    // UBERON's fourteen `*-minimal.owl` subsets are all of them — their recipe
    // (`$(SUBSETCMD)`) has no `annotate` step, so none of them has an IRI.
    let oiri = ont_iri.clone().unwrap_or_else(|| OWL_NS_BASE.to_string());

    // XML declaration + rdf:RDF with default namespace, xml:base, then every
    // document prefix on its own line (input order), the last closing the tag.
    write!(w, "<?xml version=\"1.0\"?>\n")?;
    write!(w, "<rdf:RDF xmlns=\"{}#\"\n", esc_attr(&oiri))?;
    write!(w, "     xml:base=\"{}\"", esc_attr(&oiri))?;
    for (p, ns) in prefixes {
        if p.is_empty() {
            continue;
        }
        // XML Names §3 reserves `xml` (bindable only to its own namespace) and
        // `xmlns` (not bindable at all). An OWL prefix map may carry either —
        // EFO's `components/gwas_import.owl` has
        // `Prefix(xml:=<https://www.w3.org/TR/xml#>)` — and emitting one makes the
        // document unreadable to every XML parser, so it is dropped here.
        if p == "xmlns" || (p == "xml" && ns != "http://www.w3.org/XML/1998/namespace") {
            continue;
        }
        write!(w, "\n     xmlns:{p}=\"{}\"", esc_attr(ns))?;
    }
    write!(w, ">\n")?;

    // owl:Ontology block. With no version IRI, no imports and no ontology
    // annotations there is nothing to nest, so the tag closes inline — EFO's
    // import modules are `<owl:Ontology rdf:about="…"/>` on one line.
    let empty_ont = version_iri.is_none() && imports.is_empty() && ont_anns.is_empty();
    let close = if empty_ont { "/>" } else { ">" };
    // An ontology with no IRI is an ANONYMOUS node — the owl namespace serves as
    // xmlns/base default above, but must not become an `rdf:about`.
    let mut ontology = String::new();
    if ont_iri.is_none() || oiri.is_empty() {
        ontology.push_str(&format!("    <owl:Ontology{close}\n"));
    } else {
        ontology.push_str(&format!("    <owl:Ontology rdf:about=\"{}\"{close}\n", esc_attr(&oiri)));
    }
    if let Some(v) = &version_iri {
        ontology.push_str(&format!("        <owl:versionIRI rdf:resource=\"{}\"/>\n", esc_attr(v)));
    }
    // Imports are ordered as IRIs, which is namespace-then-remainder and not the
    // plain string order: the split falls before the longest NCName suffix, so
    // `…/components/2DFTU_HRA_illustrations.owl` — whose local name cannot start
    // an NCName — has namespace `…/components/2` and sorts AFTER every
    // `…/components/` sibling rather than first.
    imports.sort_by(|a, b| crate::owlapi_hash::iri_cmp(a, b));
    for im in &imports {
        ontology.push_str(&format!("        <owl:imports rdf:resource=\"{}\"/>\n", esc_attr(im)));
    }
    ont_anns.sort_by(|a, b| ann_key(&a.0, &a.1).cmp(&ann_key(&b.0, &b.1)));
    for (p, av) in &ont_anns {
        ontology.push_str(&render_ann(p, av, prefixes));
    }
    if !empty_ont {
        ontology.push_str("    </owl:Ontology>\n");
    }
    write!(w, "{}", resolve_anon(crate::io::genid::HEADER_GRAPH, &ontology))?;

    let _ = RDFS_COMMENT;
    Ok(ont_iri)
}

/// Emit a section banner (`// Annotation properties`, …), with the exact leading
/// `    \n\n\n` and the `<!-- //// … //// -->` block.
fn write_banner<W: Write>(w: &mut W, name: &str) -> Result<()> {
    write!(w, "    \n\n\n")?;
    write!(w, "    <!-- \n")?;
    write!(w, "    {RULE}\n")?;
    write!(w, "    //\n")?;
    write!(w, "    // {name}\n")?;
    write!(w, "    //\n")?;
    write!(w, "    {RULE}\n")?;
    write!(w, "     -->\n\n")?;
    Ok(())
}

/// Is `iri` in one of the four built-in OWL/RDF vocabularies? These entities are
/// always known: no synthesised declaration, and no stub section.
fn builtin_ns(iri: &str) -> bool {
    iri.starts_with("http://www.w3.org/2001/XMLSchema#")
        || iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
        || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
        || iri.starts_with("http://www.w3.org/2002/07/owl#")
}

/// An entity-banner comment's text: the IRI is XML-escaped, then every `--` is
/// turned into `&#45;&#45;`, because an XML comment may not contain one. So a
/// source IRI with a query string or a doubled hyphen — GSSO cites Google Books
/// and web.archive URLs with both — cannot be copied through unchanged.
fn esc_comment(iri: &str) -> String {
    esc(iri).replace("--", "&#45;&#45;")
}

/// Emit one entity block: leading `    \n\n\n`, `<!-- IRI -->`, then the element
/// (self-closing when the body is empty), then any `<owl:Axiom>` reifications
/// (`after`) that follow for that entity's annotated annotations.
/// The root blocks that follow `host`'s own: one per later member of an n-ary
/// `SameIndividual` or equivalence, in member order, as an untyped
/// `rdf:Description` — the member's TYPE is stated in its own entity section, not
/// here. Each is followed again by the anonymous roots of `host`'s graph
/// (`anon_roots`: its reifications and negative assertions, not the nodes its
/// edges name by id).
fn write_root_blocks<W: Write>(
    w: &mut W,
    host: &str,
    roots: &BTreeMap<String, Vec<(String, String)>>,
    anon_roots: &str,
) -> Result<()> {
    // One block per member, in IRI order, holding every pair it is the subject
    // of in the host's graph.
    let mut blocks: Vec<(&str, String)> = Vec::new();
    for (member, body) in roots.get(host).into_iter().flatten() {
        if member == host {
            continue;
        }
        match blocks.iter_mut().find(|(m, _)| *m == member.as_str()) {
            Some((_, b)) => b.push_str(body),
            None => blocks.push((member, body.clone())),
        }
    }
    blocks.sort_by(|a, b| iri_key(a.0).cmp(&iri_key(b.0)));
    for (member, body) in blocks {
        write_entity(w, host, "rdf:Description", member, &body, anon_roots)?;
    }
    Ok(())
}

fn write_entity<W: Write>(w: &mut W, graph: &str, elem: &str, iri: &str, body: &str, after: &str) -> Result<()> {
    write!(w, "    \n\n\n")?;
    write!(w, "    <!-- {} -->\n\n", esc_comment(iri))?;
    let block = if body.is_empty() {
        format!("    <{elem} rdf:about=\"{}\"/>\n", esc_attr(iri))
    } else {
        format!("    <{elem} rdf:about=\"{}\">\n{body}    </{elem}>\n", esc_attr(iri))
    };
    if anon_doc_active() {
        write!(w, "{}", resolve_anon(graph, &format!("{block}{after}")))?;
    } else {
        write!(w, "{block}{after}")?;
    }
    end_object(w)
}

/// An `owl:annotatedTarget` value (IRI resource, or literal with the right
/// datatype/lang), 8-space indented.
fn render_target(av: &AnnotationValue<RcStr>) -> String {
    match av {
        AnnotationValue::IRI(i) => {
            format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(i.as_ref()))
        }
        AnnotationValue::Literal(Literal::Simple { literal }) => {
            format!("        <owl:annotatedTarget>{}</owl:annotatedTarget>\n", esc(literal))
        }
        AnnotationValue::Literal(Literal::Language { literal, lang }) => format!(
            "        <owl:annotatedTarget xml:lang=\"{lang}\">{}</owl:annotatedTarget>\n",
            esc(literal)
        ),
        AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri }) => {
            if datatype_iri.as_ref() == XSD_STRING {
                format!("        <owl:annotatedTarget>{}</owl:annotatedTarget>\n", esc(literal))
            } else {
                format!(
                    "        <owl:annotatedTarget rdf:datatype=\"{}\">{}</owl:annotatedTarget>\n",
                    esc_attr(datatype_iri.as_ref()),
                    esc(literal)
                )
            }
        }
        AnnotationValue::AnonymousIndividual(a) => anon_slot("owl:annotatedTarget", a.0.as_ref(), "        "),
    }
}

/// An `<owl:Axiom>` reification of an annotated annotation assertion.
fn render_reification(
    subj: &str,
    prop: &str,
    av: &AnnotationValue<RcStr>,
    nested: &[(String, AnnotationValue<RcStr>)],
    prefixes: &[(String, String)],
) -> String {
    let mut s = String::new();
    s.push_str("    <owl:Axiom>\n");
    s.push_str(&format!(
        "        <owl:annotatedSource rdf:resource=\"{}\"/>\n",
        esc_attr(subj)
    ));
    s.push_str(&format!(
        "        <owl:annotatedProperty rdf:resource=\"{}\"/>\n",
        esc_attr(prop)
    ));
    s.push_str(&render_target(av));
    let mut ns: Vec<&(String, AnnotationValue<RcStr>)> = nested.iter().collect();
    ns.sort_by(|a, b| ann_key(&a.0, &a.1).cmp(&ann_key(&b.0, &b.1)));
    for (p, a) in ns {
        s.push_str(&render_ann(p, a, prefixes));
    }
    s.push_str("    </owl:Axiom>\n");
    s
}

/// Reify an annotated `SubClassOf`/`equivalentClass` edge. `target` is the full
/// `<owl:annotatedTarget .../>` line (resource for a named superclass, nodeID for
/// an anonymous one).
fn edge_reif(
    subj: &str,
    prop: &str,
    target: &str,
    anns: &[(String, AnnotationValue<RcStr>)],
    prefixes: &[(String, String)],
) -> String {
    let mut s = String::from("    <owl:Axiom>\n");
    s.push_str(&format!("        <owl:annotatedSource rdf:resource=\"{}\"/>\n", esc_attr(subj)));
    s.push_str(&format!("        <owl:annotatedProperty rdf:resource=\"{prop}\"/>\n"));
    s.push_str(target);
    let mut ns: Vec<&(String, AnnotationValue<RcStr>)> = anns.iter().collect();
    ns.sort_by(|a, b| ann_key(&a.0, &a.1).cmp(&ann_key(&b.0, &b.1)));
    for (p, av) in ns {
        s.push_str(&render_ann(p, av, prefixes));
    }
    s.push_str("    </owl:Axiom>\n");
    s
}

/// Text between `open` and the next `close`, if present.
fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let a = s.find(open)? + open.len();
    let b = s[a..].find(close)? + a;
    Some(&s[a..b])
}

/// A stable identity for an `<owl:Axiom>` reification block: its annotatedProperty
/// plus a tag+value for its annotatedTarget. The SAME function reads both the
/// reifications this renderer generates and the blocks scanned from a source
/// document, so a generated block can be matched to the source state recorded
/// under its signature (the genid a block's nested nodes were numbered with).
pub(crate) fn reif_signature(block: &str) -> String {
    // An inverse property's own block: the triple after its `owl:inverseOf`.
    if let Some(rest) = block.strip_prefix("    <rdf:Description>\n        <owl:inverseOf rdf:resource=\"") {
        let line = rest.split('\n').nth(1).unwrap_or("").trim_start();
        let tag = line.trim_start_matches('<').split([' ', '>', '/']).next().unwrap_or("");
        let target = match between(line, "rdf:resource=\"", "\"") {
            Some(v) => format!("R\u{1}{v}"),
            None => "A\u{1}".to_string(),
        };
        return format!("INV\u{1}{}\u{1}{target}", qname_iri(tag));
    }
    // A reification whose source is a node nested in it.
    if block.contains("<owl:annotatedSource>\n") {
        return format!("~{}", reif_signature(&block.replacen("<owl:annotatedSource>\n", "", 1)));
    }
    // A negative assertion's node: its property, then its target.
    if block.contains(NEG_PA) {
        let prop = between(block, "<owl:assertionProperty rdf:resource=\"", "\"").unwrap_or("");
        let target = match between(block, "<owl:targetIndividual rdf:resource=\"", "\"") {
            Some(v) => format!("R\u{1}{v}"),
            None => {
                let v = between(block, "<owl:targetValue", "</owl:targetValue>").unwrap_or("");
                format!("L\u{1}{}", v.split_once('>').map(|x| x.1).unwrap_or(""))
            }
        };
        return format!("NPA\u{1}{prop}\u{1}{target}");
    }
    let prop = between(block, "<owl:annotatedProperty rdf:resource=\"", "\"").unwrap_or("");
    let tsig = if let Some(v) = between(block, "<owl:annotatedTarget rdf:resource=\"", "\"") {
        format!("R\u{1}{v}")
    } else if let Some(v) = between(block, "<owl:annotatedTarget rdf:nodeID=\"", "\"") {
        format!("N\u{1}{v}")
    } else if block.contains("<owl:annotatedTarget rdf:parseType=\"Collection\">") {
        let coll =
            between(block, "<owl:annotatedTarget rdf:parseType=\"Collection\">", "</owl:annotatedTarget>").unwrap_or("");
        let mut members = String::new();
        let mut rest = coll;
        while let Some(v) = between(rest, "rdf:about=\"", "\"") {
            members.push_str(v);
            members.push('\u{2}');
            let idx = rest.find("rdf:about=\"").unwrap() + "rdf:about=\"".len() + v.len();
            rest = &rest[idx..];
        }
        format!("C\u{1}{members}")
    } else if let Some(v) = between(block, "<owl:annotatedTarget", "</owl:annotatedTarget>") {
        // Plain/typed/lang literal: skip any attributes up to the closing `>`.
        let text = v.splitn(2, '>').nth(1).unwrap_or("");
        format!("L\u{1}{text}")
    } else {
        String::new()
    };
    format!("{prop}\u{1}{tsig}")
}

/// The IRI of a predicate the layout writes by qualified name.
fn qname_iri(tag: &str) -> String {
    match tag.split_once(':') {
        Some(("rdf", local)) => format!("{RDF_NS}{local}"),
        Some(("rdfs", local)) => format!("http://www.w3.org/2000/01/rdf-schema#{local}"),
        Some(("owl", local)) => format!("http://www.w3.org/2002/07/owl#{local}"),
        _ => tag.to_string(),
    }
}

/// Whether the layout states the annotations of annotations `ac` carries: on
/// the ontology, or on an axiom among `numbered` (by `axiom_identity`). An
/// annotated annotation's value is named or a literal, or, where the document
/// states its anonymous individuals from its model (`anon_model`), any value.
fn nested_annotations_stated(ac: &AnnotatedComponent<RcStr>, numbered: &HashSet<u64>, anon_model: bool) -> bool {
    let stated = |anns: &BTreeSet<horned_owl::model::Annotation<RcStr>>| {
        fn named(anns: &BTreeSet<horned_owl::model::Annotation<RcStr>>) -> bool {
            anns.iter().all(|a| {
                a.ann.is_empty() || (!matches!(a.av, AnnotationValue::AnonymousIndividual(_)) && named(&a.ann))
            })
        }
        anon_model || named(anns)
    };
    let nested = |anns: &BTreeSet<horned_owl::model::Annotation<RcStr>>| anns.iter().any(|a| !a.ann.is_empty());
    match &ac.component {
        Component::OntologyAnnotation(oa) => !nested(&oa.0.ann) || stated(&BTreeSet::from([oa.0.clone()])),
        _ => !nested(&ac.ann) || (numbered.contains(&crate::io::genid::axiom_identity(ac)) && stated(&ac.ann)),
    }
}

/// Order reification `owl:Axiom` blocks as root anonymous nodes: they are emitted
/// sorted on the blank node IRI `_:genidN` — a LEXICOGRAPHIC string sort, so a
/// digit-length boundary (genid99999 → genid100000) reorders the blocks. `reif`
/// gives each axiom's (signature, genid) in creation order; match every block to
/// its genid by consuming successive genids per signature, then sort blocks by
/// the `genidN` string. Unmatched blocks keep their original relative order at
/// the end.
fn order_reifs_by_genid(reifs: &str, reif: Option<&Vec<(String, u64)>>) -> String {
    let Some(reif) = reif else { return reifs.to_string() };
    if reifs.is_empty() {
        return String::new();
    }
    sorted_blocks(keyed_blocks(reifs, reif))
}

/// The top-level blocks of `reifs`, each with the node id `reif` gives its
/// signature, and its place in `reifs`.
fn keyed_blocks(reifs: &str, reif: &[(String, u64)]) -> Vec<(Option<u64>, usize, String)> {
    // A block opens at every line that starts an element at the top level,
    // except a node defined by id, which follows the block that names it.
    // A block after a line naming its node is that node.
    let mut blocks: Vec<(Option<u64>, String)> = Vec::new();
    for line in reifs.split_inclusive('\n') {
        if let Some(node) = line.strip_prefix(ROOT_KEY) {
            blocks.push((node.trim_end().parse().ok(), String::new()));
            continue;
        }
        let opens = line.starts_with("    <")
            && !line.starts_with("    </")
            && !line.split_once('>').is_some_and(|(open, _)| open.contains(" rdf:nodeID=\""));
        match blocks.last_mut() {
            Some((_, b)) if !opens || b.is_empty() => b.push_str(line),
            _ => blocks.push((None, line.to_string())),
        }
    }
    // Per-signature queue of genids in creation order.
    let mut by_sig: std::collections::HashMap<&str, std::collections::VecDeque<u64>> =
        std::collections::HashMap::new();
    for (sig, g) in reif {
        by_sig.entry(sig.as_str()).or_default().push_back(*g);
    }
    blocks
        .into_iter()
        .enumerate()
        .map(|(i, (node, b))| {
            let key = node.or_else(|| {
                let sig = reif_signature(&b);
                by_sig.get_mut(sig.as_str()).and_then(|q| q.pop_front())
            });
            (key, i, b)
        })
        .collect()
}

/// `blocks` in node-id order, joined. The order is the `genidN` string's,
/// compared lexicographically, so shorter numbers with a larger leading digit
/// sort after longer ones; a block with no id keeps its place after the rest.
///
/// NOT a numeric compare, though `NodeID.nextAnonymousIRI` counting from
/// `Integer.MAX_VALUE` makes that look right: padding these to a fixed width
/// (i.e. ordering numerically) takes `uberon_import.owl` from 34 differing
/// lines to 110. The lexicographic shape is reproducing something real; the
/// residual on that file is two subjects whose reification blocks OWLAPI
/// orders differently, and it is NOT this.
fn sorted_blocks(blocks: Vec<(Option<u64>, usize, String)>) -> String {
    let mut keyed: Vec<(Option<String>, usize, String)> =
        blocks.into_iter().map(|(g, i, b)| (g.map(|g| format!("genid{g}")), i, b)).collect();
    keyed.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(&b.1),
    });
    keyed.into_iter().map(|(_, _, b)| b).collect()
}

/// As `order_reifs_by_genid`, for a graph whose reified axioms may have
/// annotations that carry annotations of their own (`nested`, by the axiom's
/// node). Such an axiom's node is named by id, and the roots of the graph in
/// its place are the `owl:Annotation` nodes of its annotations (see
/// `annotation_roots`), each ordered by its own id. The axiom's block itself is
/// returned, by id, to be defined after the first root that names it. The
/// nodes so stated are added to `stated`.
fn order_roots(
    graph: &str,
    reifs: &str,
    reif: Option<&Vec<(String, u64)>>,
    nested: &HashMap<u64, crate::io::genid::NestedAnnotations>,
    stated: &mut HashSet<u64>,
    prefixes: &[(String, String)],
) -> (String, Vec<(String, String)>) {
    let reifs = format!("{reifs}{}", anon_root_blocks(graph));
    let reif = match reif {
        Some(reif) => reif.as_slice(),
        None if reifs.contains(ROOT_KEY) => &[],
        None => return (reifs, Vec::new()),
    };
    if reifs.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut blocks = Vec::new();
    let mut defs = Vec::new();
    for (key, i, block) in keyed_blocks(&reifs, reif) {
        match key.and_then(|g| nested.get(&g).map(|n| (g, n))) {
            Some((g, n)) => {
                stated.insert(g);
                let gid = format!("genid{g}");
                defs.push((gid.clone(), inject_nodeid(&block, &gid)));
                for (id, root) in annotation_roots(g, n, prefixes) {
                    blocks.push((Some(id), i, root));
                }
            }
            // A later pair's node is named by the roots of the first pair's.
            None => match key.filter(|g| nested.values().any(|n| n.sources.contains(g))) {
                Some(g) => {
                    stated.insert(g);
                    let gid = format!("genid{g}");
                    defs.push((gid.clone(), inject_nodeid(&block, &gid)));
                }
                None => blocks.push((key, i, block)),
            },
        }
    }
    (sorted_blocks(blocks), defs)
}

/// The `owl:Annotation` nodes the annotations on the node `node` make, as
/// roots of its graph, with their ids. Each annotation that is annotated is a
/// node of its own whose source is what it annotates — `node`, by id, or the
/// annotation it is on, nested — and the roots are those none of whose own
/// annotations is annotated.
fn annotation_roots(
    node: u64,
    nested: &crate::io::genid::NestedAnnotations,
    prefixes: &[(String, String)],
) -> Vec<(u64, String)> {
    let mut ids = nested.ids.iter().copied();
    let mut roots = Vec::new();
    let source = format!("        <owl:annotatedSource rdf:nodeID=\"genid{node}\"/>\n");
    // The later pairs of an axiom written as pairs annotate the same nodes.
    let more: String =
        nested.sources.iter().map(|n| format!("        <owl:annotatedSource rdf:nodeID=\"genid{n}\"/>\n")).collect();
    annotation_nodes(&nested.anns, &source, &more, &mut ids, &mut roots, prefixes);
    roots
}

/// The roots among the nodes of `anns`, whose source is `source` and, after
/// their own annotations, `more`. The nodes take `ids` in order: an annotation
/// before those on it.
fn annotation_nodes(
    anns: &BTreeSet<horned_owl::model::Annotation<RcStr>>,
    source: &str,
    more: &str,
    ids: &mut impl Iterator<Item = u64>,
    roots: &mut Vec<(u64, String)>,
    prefixes: &[(String, String)],
) {
    let mut sorted: Vec<&horned_owl::model::Annotation<RcStr>> = anns.iter().collect();
    sorted.sort_by(|a, b| {
        a.ap.0
            .as_ref()
            .cmp(b.ap.0.as_ref())
            .then_with(|| crate::io::owlfunc::cmp_annotation_value(&a.av, &b.av))
    });
    for a in sorted {
        if a.ann.is_empty() {
            continue;
        }
        let Some(id) = ids.next() else { return };
        let mut own: Vec<(String, AnnotationValue<RcStr>)> =
            a.ann.iter().map(|x| (x.ap.0.as_ref().to_string(), x.av.clone())).collect();
        own.sort_by_key(|x| ann_key(&x.0, &x.1));
        let mut node = format!(
            "    <owl:Annotation>\n{source}        <owl:annotatedProperty rdf:resource=\"{}\"/>\n{}",
            esc_attr(a.ap.0.as_ref()),
            render_target(&a.av)
        );
        for (p, av) in &own {
            node.push_str(&render_ann(p, av, prefixes));
        }
        node.push_str(more);
        node.push_str("    </owl:Annotation>\n");
        if a.ann.iter().any(|x| !x.ann.is_empty()) {
            let inner: String = node.split_inclusive('\n').map(|line| format!("        {line}")).collect();
            let source = format!("        <owl:annotatedSource>\n{inner}        </owl:annotatedSource>\n");
            annotation_nodes(&a.ann, &source, "", ids, roots, prefixes);
        } else {
            roots.push((id, node));
        }
    }
}

/// Order two annotation lists the way reified `owl:Axiom` blocks sort when the
/// axioms differ only in their annotations: elementwise on (property IRI,
/// value) over the already property-sorted lists, shorter list first on a tie.
/// The model's component set iterates in hash order, so without this tie-break
/// the block order of two same-target annotated axioms is an accident of the
/// hash function.
pub(crate) fn cmp_ann_list(
    a: &[(String, AnnotationValue<RcStr>)],
    b: &[(String, AnnotationValue<RcStr>)],
) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let c = x
            .0
            .cmp(&y.0)
            .then_with(|| crate::io::owlfunc::cmp_annotation_value(&x.1, &y.1));
        if c != std::cmp::Ordering::Equal {
            return c;
        }
    }
    a.len().cmp(&b.len())
}

/// Inject `rdf:nodeID="genidN"` into the first opening tag of a rendered class
/// expression, turning an inline expression into a named blank-node definition.
fn inject_nodeid(rendered: &str, gid: &str) -> String {
    match rendered.find('>') {
        Some(pos) => format!("{} rdf:nodeID=\"{}\"{}", &rendered[..pos], gid, &rendered[pos..]),
        None => rendered.to_string(),
    }
}

const EQUIV_PROP: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const SUB_PROP: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// A class's map from an anonymous expression's structural signature ([`ce_sig`])
/// to the blank-node id (`genidN`) that expression is rendered under.
type Genids = std::collections::HashMap<String, String>;

type Ann = (String, AnnotationValue<RcStr>, Vec<(String, AnnotationValue<RcStr>)>);

use horned_owl::model::{ClassExpression as CE, Individual, ObjectPropertyExpression as OPEx};

/// The `owl:onProperty` slot of a restriction. A NAMED property is a bare
/// `rdf:resource`; an INVERSE one is an anonymous node carrying `owl:inverseOf`
/// — `ope_iri` has no IRI to give for it, so formatting that as the attribute
/// value would write `rdf:resource=""` and lose the property entirely (nbo's
/// `inverse(RO_0000053) some …`).
fn render_on_property(ope: &OPEx<RcStr>, indent: usize) -> String {
    let pad = " ".repeat(indent);
    match ope {
        OPEx::ObjectProperty(p) => {
            format!("{pad}<owl:onProperty rdf:resource=\"{}\"/>\n", esc_attr(p.0.as_ref()))
        }
        OPEx::InverseObjectProperty(p) => format!(
            "{pad}<owl:onProperty>\n{pad}    <rdf:Description>\n{pad}        <owl:inverseOf rdf:resource=\"{}\"/>\n{pad}    </rdf:Description>\n{pad}</owl:onProperty>\n",
            esc_attr(p.0.as_ref())
        ),
    }
}

fn ope_iri(ope: &OPEx<RcStr>) -> Option<String> {
    match ope {
        OPEx::ObjectProperty(p) => Some(p.0.as_ref().to_string()),
        OPEx::InverseObjectProperty(_) => None,
    }
}

/// A class-expression sort key mirroring `render_ce` output order for a set of
/// superclass/operand expressions: named classes (rank 0, by ns/rem) before
/// restrictions (rank 1, by property then filler), encoded as a string so it
/// nests without a recursive type. `\u{1}` separates fields, `\u{0}` ns/rem.
fn ce_key(ce: &CE<RcStr>) -> String {
    match ce {
        // Named class (rdf:resource) first, then owl:Class set operators, then
        // owl:Restriction nodes — the order anonymous superclasses render in.
        CE::Class(c) => {
            let (n, r) = iri_key(c.0.as_ref());
            format!("0\u{1}{n}\u{0}{r}")
        }
        CE::ObjectIntersectionOf(_) => format!("1\u{1}i\u{1}{}", set_key(ce)),
        CE::ObjectUnionOf(_) => format!("1\u{1}u\u{1}{}", set_key(ce)),
        // Restrictions on a property share rank 2 and sort by property, then by
        // class-expression kind (some < all < min < exact < max), then by
        // cardinality value and filler.
        CE::ObjectSomeValuesFrom { ope, bce } => {
            let p = ope_iri(ope).unwrap_or_default();
            let (n, r) = iri_key(&p);
            format!("2\u{1}1\u{1}{n}\u{0}{r}\u{1}{}", ce_key(bce))
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            let p = ope_iri(ope).unwrap_or_default();
            let (n, r) = iri_key(&p);
            format!("2\u{1}2\u{1}{n}\u{0}{r}\u{1}{}", ce_key(bce))
        }
        CE::ObjectMinCardinality { n: card, ope, bce } => {
            let p = ope_iri(ope).unwrap_or_default();
            let (n, r) = iri_key(&p);
            format!("2\u{1}3\u{1}{n}\u{0}{r}\u{1}{card:020}\u{1}{}", ce_key(bce))
        }
        CE::ObjectExactCardinality { n: card, ope, bce } => {
            let p = ope_iri(ope).unwrap_or_default();
            let (n, r) = iri_key(&p);
            format!("2\u{1}4\u{1}{n}\u{0}{r}\u{1}{card:020}\u{1}{}", ce_key(bce))
        }
        CE::ObjectMaxCardinality { n: card, ope, bce } => {
            let p = ope_iri(ope).unwrap_or_default();
            let (n, r) = iri_key(&p);
            format!("2\u{1}5\u{1}{n}\u{0}{r}\u{1}{card:020}\u{1}{}", ce_key(bce))
        }
        // Class-expression kind order: Class < Intersection < Union < Complement
        // < restrictions. Complement sorts by its operand; rank it between union
        // ("1\x01u") and the restrictions ("2").
        CE::ObjectComplementOf(b) => format!("1\u{1}v\u{1}{}", ce_key(b)),
        // `ObjectOneOf` is the last of the object class-expression kinds, after
        // every restriction (sub-ranks 1–5 above), so rank it "2\x019".
        CE::ObjectOneOf(inds) => {
            let mut keys: Vec<String> = inds.iter().map(ind_key).collect();
            keys.sort();
            format!("2\u{1}9\u{1}{}", keys.join("\u{2}"))
        }
        // `ObjectHasValue` is the kind after the three object cardinalities and
        // before the data restrictions, so it ranks between them. Keyed by the
        // property and then the individual, as the data form is keyed by the
        // property and then its literal.
        CE::ObjectHasValue { ope, i } => {
            let p = ope_iri(ope).unwrap_or_default();
            let (n, r) = iri_key(&p);
            format!("2\u{1}6\u{1}{n}\u{0}{r}\u{1}{}", ind_key(i))
        }
        // The DATA restrictions. The kind ordering continues past the object
        // forms, so they rank after `ObjectOneOf` — keyed, like the object ones,
        // by the property they restrict.
        CE::DataSomeValuesFrom { dp, dr } => {
            let (n, r) = iri_key(dp.0.as_ref());
            format!("2\u{1}a\u{1}{n}\u{0}{r}\u{1}{}", dr_key(dr))
        }
        CE::DataAllValuesFrom { dp, dr } => {
            let (n, r) = iri_key(dp.0.as_ref());
            format!("2\u{1}b\u{1}{n}\u{0}{r}\u{1}{}", dr_key(dr))
        }
        CE::DataHasValue { dp, l } => {
            let (n, r) = iri_key(dp.0.as_ref());
            format!("2\u{1}c\u{1}{n}\u{0}{r}\u{1}{}", l.literal())
        }
        // The data cardinalities keep the object ones' relative order — min, then
        // exact, then max — after the other data restrictions.
        CE::DataMinCardinality { n: card, dp, dr } => {
            let (n, r) = iri_key(dp.0.as_ref());
            format!("2\u{1}d\u{1}{n}\u{0}{r}\u{1}{card:020}\u{1}{}", dr_key(dr))
        }
        CE::DataExactCardinality { n: card, dp, dr } => {
            let (n, r) = iri_key(dp.0.as_ref());
            format!("2\u{1}e\u{1}{n}\u{0}{r}\u{1}{card:020}\u{1}{}", dr_key(dr))
        }
        CE::DataMaxCardinality { n: card, dp, dr } => {
            let (n, r) = iri_key(dp.0.as_ref());
            format!("2\u{1}f\u{1}{n}\u{0}{r}\u{1}{card:020}\u{1}{}", dr_key(dr))
        }
        _ => "3".to_string(),
    }
}

/// Sort key for an individual in an `owl:oneOf` list — named individuals by
/// (namespace, remainder) like `ce_key`, anonymous ones after them by node id.
fn ind_key(i: &Individual<RcStr>) -> String {
    match i {
        Individual::Named(n) => {
            let (ns, rem) = iri_key(n.0.as_ref());
            format!("0\u{1}{ns}\u{0}{rem}")
        }
        Individual::Anonymous(a) => format!("1\u{1}{}", a.0.as_ref()),
    }
}

fn set_key(ce: &CE<RcStr>) -> String {
    let ops = match ce {
        CE::ObjectIntersectionOf(o) | CE::ObjectUnionOf(o) => o,
        _ => return String::new(),
    };
    let mut keys: Vec<String> = ops.iter().map(ce_key).collect();
    keys.sort();
    keys.join("\u{2}")
}

/// Render a class expression as RDF/XML at `indent` spaces. A named class
/// used as a restriction filler / operand is emitted by the caller via
/// `rdf:resource`; this handles the anonymous shapes (restrictions, set
/// operators) EFO uses.
fn render_ce(ce: &CE<RcStr>, indent: usize, g: &Genids) -> String {
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 4);
    // A named filler is a bare rdf:resource on the parent; a shared/annotated
    // anonymous one is an `rdf:nodeID` reference; any other anonymous one is a
    // nested element. This helper renders the *value slot* of `on`-style tags.
    let slot = |tag: &str, bce: &CE<RcStr>| -> String {
        match bce {
            CE::Class(c) => format!("{pad2}<owl:{tag} rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref())),
            _ => match g.get(&ce_sig(bce)) {
                Some(gid) => format!("{pad2}<owl:{tag} rdf:nodeID=\"{gid}\"/>\n"),
                None => format!("{pad2}<owl:{tag}>\n{}{pad2}</owl:{tag}>\n", render_ce(bce, indent + 8, g)),
            },
        }
    };
    match ce {
        // A named class in a node position: the callers above render one as a bare
        // `rdf:resource` on the parent, so this is the shape reached only where an
        // expression nests one directly.
        CE::Class(c) => format!("{pad}<owl:Class rdf:about=\"{}\"/>\n", esc_attr(c.0.as_ref())),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            format!(
                "{pad}<owl:Restriction>\n{}{}{pad}</owl:Restriction>\n",
                render_on_property(ope, indent + 4),
                slot("someValuesFrom", bce)
            )
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            format!(
                "{pad}<owl:Restriction>\n{}{}{pad}</owl:Restriction>\n",
                render_on_property(ope, indent + 4),
                slot("allValuesFrom", bce)
            )
        }
        CE::ObjectIntersectionOf(ops) => render_set(ops, "intersectionOf", indent, g),
        CE::ObjectUnionOf(ops) => render_set(ops, "unionOf", indent, g),
        CE::ObjectComplementOf(b) => {
            format!("{pad}<owl:Class>\n{}{pad}</owl:Class>\n", slot("complementOf", b))
        }
        CE::ObjectHasSelf(ope) => {
            format!(
                "{pad}<owl:Restriction>\n{}{pad2}<owl:hasSelf rdf:datatype=\"http://www.w3.org/2001/XMLSchema#boolean\">true</owl:hasSelf>\n{pad}</owl:Restriction>\n",
                render_on_property(ope, indent + 4)
            )
        }
        // `C ⊑ p value <individual>` — the object counterpart of
        // `DataHasValue` below. CL defines 130 of its BICAN cell-set classes as
        // `CL_0000000 and (RO_0015001 value <CS…>)`; without this arm the
        // catch-all renders nothing, taking the whole enclosing
        // `owl:intersectionOf` operand and `rdfs:subClassOf` with it.
        CE::ObjectHasValue { ope, i } => {
            let value = match i {
                Individual::Named(n) => {
                    format!("{pad2}<owl:hasValue rdf:resource=\"{}\"/>\n", esc_attr(n.0.as_ref()))
                }
                Individual::Anonymous(a) if anon_doc_active() => anon_slot("owl:hasValue", a.0.as_ref(), &pad2),
                Individual::Anonymous(a) => {
                    let id = a.0.as_ref();
                    if ANON_REPEATED.with(|r| r.borrow().contains(id)) {
                        format!(
                            "{pad2}<owl:hasValue rdf:nodeID=\"{}\"/>\n",
                            esc_attr(id.trim_start_matches("_:"))
                        )
                    } else {
                        format!(
                            "{pad2}<owl:hasValue>\n{}{pad2}</owl:hasValue>\n",
                            anonymous_object(id, &" ".repeat(indent + 8))
                        )
                    }
                }
            };
            format!(
                "{pad}<owl:Restriction>\n{}{value}{pad}</owl:Restriction>\n",
                render_on_property(ope, indent + 4)
            )
        }
        // `C ≡ {a, b, c}` — an enumeration. IAO's curation-status classes are
        // defined this way; dropping them costs mondo.owl three
        // `<owl:equivalentClass>` blocks.
        CE::ObjectOneOf(inds) => {
            let pad3 = " ".repeat(indent + 8);
            let mut sorted: Vec<&Individual<RcStr>> = inds.iter().collect();
            sorted.sort_by(|a, b| crate::io::owlfunc::cmp_individual(a, b));
            let mut s = format!(
                "{pad}<owl:Class>\n{pad2}<owl:oneOf rdf:parseType=\"Collection\">\n"
            );
            for i in sorted {
                match i {
                    Individual::Named(n) => s.push_str(&format!(
                        "{pad3}<rdf:Description rdf:about=\"{}\"/>\n",
                        esc_attr(n.0.as_ref())
                    )),
                    Individual::Anonymous(a) => s.push_str(&anon_item(a.0.as_ref(), &pad3)),
                }
            }
            s.push_str(&format!("{pad2}</owl:oneOf>\n{pad}</owl:Class>\n"));
            s
        }
        // Data restrictions. `nbo.owl` states `CHEBI_10545 â COB_0000801 value 0`
        // that way; without this arm the whole `rdfs:subClassOf` vanishes.
        CE::DataHasValue { dp, l } => {
            format!(
                "{pad}<owl:Restriction>\n{pad2}<owl:onProperty rdf:resource=\"{}\"/>\n{}{pad}</owl:Restriction>\n",
                esc_attr(dp.0.as_ref()),
                render_literal_tag("owl:hasValue", l, indent + 4)
            )
        }
        CE::DataSomeValuesFrom { dp, dr } => {
            format!(
                "{pad}<owl:Restriction>\n{pad2}<owl:onProperty rdf:resource=\"{}\"/>\n{}{pad}</owl:Restriction>\n",
                esc_attr(dp.0.as_ref()),
                render_data_range_at("owl:someValuesFrom", dr, indent + 4)
            )
        }
        CE::DataAllValuesFrom { dp, dr } => {
            format!(
                "{pad}<owl:Restriction>\n{pad2}<owl:onProperty rdf:resource=\"{}\"/>\n{}{pad}</owl:Restriction>\n",
                esc_attr(dp.0.as_ref()),
                render_data_range_at("owl:allValuesFrom", dr, indent + 4)
            )
        }
        CE::ObjectMinCardinality { n, ope, bce } => render_card(*n, ope, bce, "minQualifiedCardinality", "minCardinality", indent, g),
        CE::ObjectMaxCardinality { n, ope, bce } => render_card(*n, ope, bce, "maxQualifiedCardinality", "maxCardinality", indent, g),
        CE::ObjectExactCardinality { n, ope, bce } => render_card(*n, ope, bce, "qualifiedCardinality", "cardinality", indent, g),
        // A cardinality on a DATA property. `rdfs:Literal` is the range that says
        // "no range": it renders unqualified, exactly as `owl:Thing` does on the
        // object side, and any other range takes the qualified tag plus
        // `owl:onDataRange`. OBI states `has measurement value min 1` this way.
        CE::DataMinCardinality { n, dp, dr } => render_data_card(*n, dp, dr, "minQualifiedCardinality", "minCardinality", indent),
        CE::DataMaxCardinality { n, dp, dr } => render_data_card(*n, dp, dr, "maxQualifiedCardinality", "maxCardinality", indent),
        CE::DataExactCardinality { n, dp, dr } => render_data_card(*n, dp, dr, "qualifiedCardinality", "cardinality", indent),
    }
}

/// Structural signature of a class expression: its map-independent rendering,
/// used to recognise a shared anonymous expression across axioms.
fn ce_sig(ce: &CE<RcStr>) -> String {
    render_ce(ce, 0, &Genids::new())
}

/// The underlying named-property IRI of a chain link (named or inverse).
fn chain_link_iri(ope: &OPE<RcStr>) -> &str {
    match ope {
        OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => p.0.as_ref(),
    }
}

/// A `propertyChainAxiom` member: a named property is `rdf:Description rdf:about`;
/// an inverse property is an anonymous node carrying `owl:inverseOf`.
fn render_chain_link(ope: &OPE<RcStr>) -> String {
    match ope {
        OPE::ObjectProperty(p) => {
            format!("            <rdf:Description rdf:about=\"{}\"/>\n", esc_attr(p.0.as_ref()))
        }
        OPE::InverseObjectProperty(p) => format!(
            "            <rdf:Description>\n                <owl:inverseOf rdf:resource=\"{}\"/>\n            </rdf:Description>\n",
            esc_attr(p.0.as_ref())
        ),
    }
}

/// A member of an equivalence or sameness of three or more members.
enum ChainMember<'a> {
    Named(String),
    /// An anonymous class expression.
    Class(&'a CE<RcStr>),
    /// The inverse of the named property.
    Inverse(String),
}

/// An equivalence or sameness of three or more members: one triple for each
/// consecutive pair of its ordered members, all in the graph of its host. Each
/// named member but the host is a root of that graph, a block of its own after
/// the host's holding the pair it is the subject of, and an anonymous member
/// holds its pair itself. Annotated, every pair is reified, and so every member
/// after the first that is not named is a reification's target, defined by id.
struct Chain<'a> {
    /// The element of each pair's triple.
    tag: &'static str,
    /// Its predicate.
    pred: &'static str,
    members: Vec<ChainMember<'a>>,
    /// The id of each member defined by id.
    ids: Vec<Option<String>>,
    anns: Vec<(String, AnnotationValue<RcStr>)>,
}

impl Chain<'_> {
    /// Member `i`, not named, as a node nested at `indent`, holding its pair.
    fn nested(&self, i: usize, indent: usize) -> String {
        let pad = " ".repeat(indent);
        let pair = self.edge(i, indent + 4);
        match &self.members[i] {
            ChainMember::Class(ce) => insert_before_close(&render_ce(ce, indent, &Genids::new()), &pair),
            ChainMember::Inverse(p) => format!(
                "{pad}<rdf:Description>\n{pad}    <owl:inverseOf rdf:resource=\"{}\"/>\n{pair}{pad}</rdf:Description>\n",
                esc_attr(p)
            ),
            ChainMember::Named(iri) => format!("{pad}<rdf:Description rdf:about=\"{}\"/>\n", esc_attr(iri)),
        }
    }

    /// The triple of the pair member `i` is the subject of, as an edge at
    /// `indent`; nothing for the last member.
    fn edge(&self, i: usize, indent: usize) -> String {
        let pad = " ".repeat(indent);
        let tag = self.tag;
        match (self.members.get(i + 1), self.ids.get(i + 1).cloned().flatten()) {
            (None, _) => String::new(),
            (Some(ChainMember::Named(iri)), _) => format!("{pad}<{tag} rdf:resource=\"{}\"/>\n", esc_attr(iri)),
            (Some(_), Some(g)) => format!("{pad}<{tag} rdf:nodeID=\"{g}\"/>\n"),
            (Some(_), None) => format!("{pad}<{tag}>\n{}{pad}</{tag}>\n", self.nested(i + 1, indent + 4)),
        }
    }

    /// The definition of member `i`, when it is defined by id: the id, and the
    /// node holding its pair.
    fn def(&self, i: usize) -> Option<(String, String)> {
        let g = self.ids.get(i).cloned().flatten()?;
        let node = match &self.members[i] {
            ChainMember::Class(ce) => inject_nodeid(&render_ce(ce, 4, &Genids::new()), &g),
            ChainMember::Inverse(p) => inverse_node_def(&g, p),
            ChainMember::Named(_) => return None,
        };
        let def = insert_before_close(&node, &self.edge(i, 8));
        Some((g, def))
    }

    /// The reification of the pair member `i` is the subject of. A source that
    /// is neither named nor defined by id is nested in it.
    fn reif(&self, i: usize, prefixes: &[(String, String)]) -> String {
        let slot = |role: &str, j: usize| match (&self.members[j], self.ids[j].as_deref()) {
            (ChainMember::Named(iri), _) => format!("        <owl:{role} rdf:resource=\"{}\"/>\n", esc_attr(iri)),
            (_, Some(g)) => format!("        <owl:{role} rdf:nodeID=\"{g}\"/>\n"),
            (_, None) => format!("        <owl:{role}>\n{}        </owl:{role}>\n", self.nested(j, 12)),
        };
        let mut s = String::from("    <owl:Axiom>\n");
        s.push_str(&slot("annotatedSource", i));
        s.push_str(&format!("        <owl:annotatedProperty rdf:resource=\"{}\"/>\n", self.pred));
        s.push_str(&slot("annotatedTarget", i + 1));
        let mut ns: Vec<&(String, AnnotationValue<RcStr>)> = self.anns.iter().collect();
        ns.sort_by_key(|a| ann_key(&a.0, &a.1));
        for (p, av) in ns {
            s.push_str(&render_ann(p, av, prefixes));
        }
        s.push_str("    </owl:Axiom>\n");
        s
    }

    /// What the host's own block does not hold: a root block for each named
    /// member but the host, and, annotated, the reification of every pair from
    /// the one member `first` is the subject of, and the definition of every
    /// member after it defined by id.
    fn place(
        &self,
        host: &str,
        first: usize,
        prefixes: &[(String, String)],
        root_blocks: &mut BTreeMap<String, Vec<(String, String)>>,
        roots: &mut BTreeMap<String, String>,
        defs: &mut BTreeMap<String, Vec<(String, String)>>,
    ) {
        for (i, m) in self.members.iter().enumerate() {
            if let ChainMember::Named(iri) = m {
                if iri != host {
                    root_blocks.entry(host.to_string()).or_default().push((iri.clone(), self.edge(i, 8)));
                }
            }
        }
        if self.anns.is_empty() {
            return;
        }
        for i in first..self.members.len() - 1 {
            roots.entry(host.to_string()).or_default().push_str(&self.reif(i, prefixes));
        }
        for i in first + 1..self.members.len() {
            if let Some(d) = self.def(i) {
                defs.entry(host.to_string()).or_default().push(d);
            }
        }
    }
}

/// Queue the definitions among `defs` of the nodes `text` names by id and no
/// earlier block has placed, in the order it names them.
fn enqueue_named(
    text: &str,
    defs: &[(String, String)],
    placed: &mut HashSet<String>,
    queue: &mut std::collections::VecDeque<usize>,
) {
    let mut rest = text;
    while let Some(at) = rest.find("rdf:nodeID=\"") {
        rest = &rest[at + "rdf:nodeID=\"".len()..];
        let id = &rest[..rest.find('"').unwrap_or(0)];
        if let Some(k) = defs.iter().position(|(g, _)| g == id) {
            if placed.insert(id.to_string()) {
                queue.push_back(k);
            }
        }
    }
}

/// `text`, a run of top-level blocks, with each of `defs` written straight
/// after the first block that names its node by id, and after the definitions
/// that block brings, in the order they are named. A node `text` defines
/// itself keeps its place.
fn place_defs(text: &str, defs: Option<&Vec<(String, String)>>) -> String {
    let Some(defs) = defs.filter(|d| !d.is_empty()) else { return text.to_string() };
    let opening_id = |line: &str| -> Option<String> {
        let open = line.split_once('>').map_or(line, |(open, _)| open);
        between(open, " rdf:nodeID=\"", "\"").map(str::to_string)
    };
    let mut placed: HashSet<String> = text
        .lines()
        .filter(|l| l.starts_with("    <") && !l.starts_with("    </"))
        .filter_map(opening_id)
        .collect();
    let mut out = String::new();
    let mut block = String::new();
    for line in text.split_inclusive('\n') {
        block.push_str(line);
        let top = line.starts_with("    <") && !line.starts_with("    <!--");
        if top && (line.starts_with("    </") || line.trim_end().ends_with("/>")) {
            out.push_str(&block);
            // The definitions this block brings, each followed in turn by those
            // it names.
            let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
            enqueue_named(&block, defs, &mut placed, &mut queue);
            while let Some(k) = queue.pop_front() {
                out.push_str(&defs[k].1);
                enqueue_named(&defs[k].1, defs, &mut placed, &mut queue);
            }
            block.clear();
        }
    }
    out.push_str(&block);
    out
}

/// The first id the numbering pass recorded under `sig` that no earlier
/// lookup has taken.
fn seq_id(seq: Option<&Vec<(String, u64)>>, sig: &str, taken: &mut std::collections::HashSet<u64>) -> Option<u64> {
    let id = seq?.iter().find(|(s, id)| s == sig && !taken.contains(id))?.1;
    taken.insert(id);
    Some(id)
}

/// An inverse property's node defined by id: its `owl:inverseOf` alone.
fn inverse_node_def(gid: &str, inverse_of: &str) -> String {
    format!(
        "    <rdf:Description rdf:nodeID=\"{gid}\">\n        <owl:inverseOf rdf:resource=\"{}\"/>\n    </rdf:Description>\n",
        esc_attr(inverse_of)
    )
}

/// The reification of an annotated axiom about an inverse property. Its source
/// is the inverse's node, nested, holding the axiom's triple (`triple`, at
/// indent 16).
fn inverse_source_reif(
    inverse_of: &str,
    triple: &str,
    pred: &str,
    target: &str,
    anns: &[(String, AnnotationValue<RcStr>)],
    prefixes: &[(String, String)],
) -> String {
    let mut s = String::from("    <owl:Axiom>\n        <owl:annotatedSource>\n            <rdf:Description>\n");
    s.push_str(&format!("                <owl:inverseOf rdf:resource=\"{}\"/>\n", esc_attr(inverse_of)));
    s.push_str(triple);
    s.push_str("            </rdf:Description>\n        </owl:annotatedSource>\n");
    s.push_str(&format!("        <owl:annotatedProperty rdf:resource=\"{pred}\"/>\n"));
    s.push_str(target);
    let mut ns: Vec<&(String, AnnotationValue<RcStr>)> = anns.iter().collect();
    ns.sort_by_key(|a| ann_key(&a.0, &a.1));
    for (p, av) in ns {
        s.push_str(&render_ann(p, av, prefixes));
    }
    s.push_str("    </owl:Axiom>\n");
    s
}

/// The object of an object property axiom's triple.
#[derive(Clone, Copy)]
enum EdgeObject<'a> {
    Property(&'a OPE<RcStr>),
    Class(&'a CE<RcStr>),
    Type(&'static str),
}

/// The triple an object property axiom with an inverse property in it is
/// stated as: its subject, its predicate (as written, and as an IRI), the
/// rank its kind takes among a property's axioms, and its object. An
/// equivalence, inverse or disjointness is stated of its first member in
/// order, named properties first. `None` for an axiom with no inverse in it.
fn inverse_edge(c: &Component<RcStr>) -> Option<(&OPE<RcStr>, &'static str, &'static str, u8, EdgeObject<'_>)> {
    fn inverse(o: &OPE<RcStr>) -> bool {
        matches!(o, OPE::InverseObjectProperty(_))
    }
    fn pair<'a>(
        a: &'a OPE<RcStr>,
        b: &'a OPE<RcStr>,
        tag: &'static str,
        pred: &'static str,
        rank: u8,
    ) -> Option<(&'a OPE<RcStr>, &'static str, &'static str, u8, EdgeObject<'a>)> {
        let key = |o: &'a OPE<RcStr>| match o {
            OPE::ObjectProperty(p) => (false, iri_key(p.0.as_ref())),
            OPE::InverseObjectProperty(p) => (true, iri_key(p.0.as_ref())),
        };
        let (s, o) = if key(a) > key(b) { (b, a) } else { (a, b) };
        (inverse(s) || inverse(o)).then_some((s, tag, pred, rank, EdgeObject::Property(o)))
    }
    fn typed<'a>(
        ope: &'a OPE<RcStr>,
        class: &'static str,
    ) -> Option<(&'a OPE<RcStr>, &'static str, &'static str, u8, EdgeObject<'a>)> {
        inverse(ope).then_some((ope, "rdf:type", P_RDF_TYPE, 15 + char_axiom_rank(class), EdgeObject::Type(class)))
    }
    match c {
        Component::EquivalentObjectProperties(ax) if ax.0.len() == 2 => {
            pair(&ax.0[0], &ax.0[1], "owl:equivalentProperty", P_EQUIV_PROPERTY, 12)
        }
        Component::SubObjectPropertyOf(ax) => match &ax.sub {
            horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(sub) => (inverse(sub)
                || inverse(&ax.sup))
            .then_some((sub, "rdfs:subPropertyOf", P_SUB_PROPERTY, 13, EdgeObject::Property(&ax.sup))),
            horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(_) => None,
        },
        Component::InverseObjectProperties(ax) => pair(&ax.0, &ax.1, "owl:inverseOf", P_INVERSE_OF, 14),
        Component::FunctionalObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#FunctionalProperty"),
        Component::InverseFunctionalObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#InverseFunctionalProperty"),
        Component::SymmetricObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#SymmetricProperty"),
        Component::AsymmetricObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#AsymmetricProperty"),
        Component::TransitiveObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#TransitiveProperty"),
        Component::ReflexiveObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#ReflexiveProperty"),
        Component::IrreflexiveObjectProperty(ax) => typed(&ax.0, "http://www.w3.org/2002/07/owl#IrreflexiveProperty"),
        Component::ObjectPropertyDomain(ax) => {
            inverse(&ax.ope).then_some((&ax.ope, "rdfs:domain", P_DOMAIN, 22, EdgeObject::Class(&ax.ce)))
        }
        Component::ObjectPropertyRange(ax) => {
            inverse(&ax.ope).then_some((&ax.ope, "rdfs:range", P_RANGE, 23, EdgeObject::Class(&ax.ce)))
        }
        Component::DisjointObjectProperties(ax) if ax.0.len() == 2 => {
            pair(&ax.0[0], &ax.0[1], "owl:propertyDisjointWith", P_PROPERTY_DISJOINT, 24)
        }
        _ => None,
    }
}

/// Whether a list an annotated axiom names twice — as the object of its triple
/// and as its reification's target — is one node named by id and defined after
/// the block (Turtle), rather than a collection written in both places
/// (RDF/XML).
fn named_lists() -> bool {
    OBJECT_ENDS.with(|c| c.get())
}

/// A list named by id and defined on its own: its first item, then the rest as
/// a collection. `items` are collection items, at indent 12.
fn list_def(gid: &str, items: &[String]) -> String {
    let mut s = format!("    <rdf:List rdf:nodeID=\"{gid}\">\n");
    if let Some(first) = items.first() {
        match first.strip_prefix("            <rdf:Description rdf:about=\"").and_then(|r| r.strip_suffix("\"/>\n")) {
            Some(iri) => s.push_str(&format!("        <rdf:first rdf:resource=\"{iri}\"/>\n")),
            None => s.push_str(&format!("        <rdf:first>\n{first}        </rdf:first>\n")),
        }
    }
    if items.len() > 1 {
        s.push_str("        <rdf:rest rdf:parseType=\"Collection\">\n");
        s.push_str(&items[1..].concat());
        s.push_str("        </rdf:rest>\n");
    } else {
        s.push_str(&format!("        <rdf:rest rdf:resource=\"{RDF_NS}nil\"/>\n"));
    }
    s.push_str("    </rdf:List>\n");
    s
}

/// A list as the object of `tag` and as an annotated target: by id when it is
/// named (its definition added to `defs`), else a collection in both places.
fn list_slots(tag: &str, items: &[String], gid: Option<&str>, defs: &mut String) -> (String, String) {
    match gid {
        Some(g) => {
            defs.push_str(&list_def(g, items));
            (
                format!("        <{tag} rdf:nodeID=\"{g}\"/>\n"),
                format!("        <owl:annotatedTarget rdf:nodeID=\"{g}\"/>\n"),
            )
        }
        None => {
            let items = items.concat();
            (
                format!("        <{tag} rdf:parseType=\"Collection\">\n{items}        </{tag}>\n"),
                format!("        <owl:annotatedTarget rdf:parseType=\"Collection\">\n{items}        </owl:annotatedTarget>\n"),
            )
        }
    }
}

/// The named members of a list, as the numbering pass records the list by.
fn list_members<'a>(named: impl Iterator<Item = &'a str>) -> String {
    named.map(|iri| format!("{}\u{2}", esc_attr(iri))).collect()
}

/// A key's properties as the items of its collection: the object properties,
/// then the inverse ones, then the data properties, each by IRI. Also the named
/// ones, as the numbering pass records the list by.
fn render_key_members(k: &horned_owl::model::HasKey<RcStr>) -> (Vec<String>, String) {
    use horned_owl::model::PropertyExpression as PE;
    let mut members: Vec<(u8, &str)> = k
        .vpe
        .iter()
        .filter_map(|pe| match pe {
            PE::ObjectPropertyExpression(OPE::ObjectProperty(p)) => Some((0, p.0.as_ref())),
            PE::ObjectPropertyExpression(OPE::InverseObjectProperty(p)) => Some((1, p.0.as_ref())),
            PE::DataProperty(d) => Some((2, d.0.as_ref())),
            PE::AnnotationProperty(_) => None,
        })
        .collect();
    members.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| iri_key(a.1).cmp(&iri_key(b.1))));
    members.dedup();
    let items = members
        .iter()
        .map(|(rank, iri)| match rank {
            1 => format!(
                "            <rdf:Description>\n                <owl:inverseOf rdf:resource=\"{}\"/>\n            </rdf:Description>\n",
                esc_attr(iri)
            ),
            _ => format!("            <rdf:Description rdf:about=\"{}\"/>\n", esc_attr(iri)),
        })
        .collect();
    (items, list_members(members.iter().filter(|(rank, _)| *rank != 1).map(|(_, iri)| *iri)))
}

/// A literal as an element with the given tag, at `indent` spaces — the same
/// shape as `render_ann`'s literal cases, for a value slot inside a restriction.
fn render_literal_tag(tag: &str, l: &Literal<RcStr>, indent: usize) -> String {
    let pad = " ".repeat(indent);
    match l {
        Literal::Simple { literal } => format!("{pad}<{tag}>{}</{tag}>\n", esc(literal)),
        Literal::Language { literal, lang } => {
            format!("{pad}<{tag} xml:lang=\"{lang}\">{}</{tag}>\n", esc(literal))
        }
        Literal::Datatype { literal, datatype_iri } => {
            if datatype_iri.as_ref() == XSD_STRING {
                format!("{pad}<{tag}>{}</{tag}>\n", esc(literal))
            } else {
                format!(
                    "{pad}<{tag} rdf:datatype=\"{}\">{}</{tag}>\n",
                    esc_attr(datatype_iri.as_ref()),
                    esc(literal)
                )
            }
        }
    }
}

/// A data range in a value slot (`rdfs:range`, `owl:someValuesFrom`,
/// `owl:allValuesFrom`) at `indent` spaces.
fn render_data_range_at(tag: &str, dr: &horned_owl::model::DataRange<RcStr>, indent: usize) -> String {
    use horned_owl::model::DataRange as DR;
    let pad = " ".repeat(indent);
    match dr {
        DR::Datatype(d) => format!("{pad}<{tag} rdf:resource=\"{}\"/>\n", esc_attr(d.0.as_ref())),
        other => format!(
            "{pad}<{tag}>\n{}{pad}</{tag}>\n",
            render_datatype_node(other, indent + 4)
        ),
    }
}

/// A list of literals as a nested `rdf:List`, one cell per member.
fn render_literal_list(lits: &[Literal<RcStr>], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 4);
    let mut s = format!("{pad}<rdf:Description>\n");
    s.push_str(&format!("{inner}<rdf:type rdf:resource=\"{RDF_LIST}\"/>\n"));
    match lits.split_first() {
        None => s.push_str(&format!("{inner}<rdf:rest rdf:resource=\"{RDF_NIL}\"/>\n")),
        Some((first, rest)) => {
            s.push_str(&render_literal_tag("rdf:first", first, indent + 4));
            if rest.is_empty() {
                s.push_str(&format!("{inner}<rdf:rest rdf:resource=\"{RDF_NIL}\"/>\n"));
            } else {
                s.push_str(&format!("{inner}<rdf:rest>\n"));
                s.push_str(&render_literal_list(rest, indent + 8));
                s.push_str(&format!("{inner}</rdf:rest>\n"));
            }
        }
    }
    s.push_str(&format!("{pad}</rdf:Description>\n"));
    s
}

/// A DATA property's `rdfs:range` value, and the filler of a data restriction: a
/// named datatype is a bare `rdf:resource`, every other data range nests as an
/// `rdfs:Datatype` node (`owl:onDatatype` + `owl:withRestrictions`, `owl:oneOf`,
/// `owl:unionOf`, `owl:intersectionOf`, `owl:datatypeComplementOf`).
fn render_data_range(tag: &str, dr: &horned_owl::model::DataRange<RcStr>) -> String {
    render_data_range_at(tag, dr, 8)
}

/// The datatypes the language defines: the OWL 2 datatype map, plus the two RDF
/// datatypes and `rdfs:Literal`. Everything else is a datatype the document brings
/// with it, and a document that mentions one renders the stub that types it.
fn is_builtin_datatype(iri: &str) -> bool {
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    let rest = match iri.strip_prefix(XSD) {
        Some(r) => r,
        None => {
            return matches!(
                iri,
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral"
                    | "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral"
                    | "http://www.w3.org/2000/01/rdf-schema#Literal"
                    | "http://www.w3.org/2002/07/owl#real"
                    | "http://www.w3.org/2002/07/owl#rational"
            )
        }
    };
    matches!(
        rest,
        "string"
            | "normalizedString"
            | "token"
            | "language"
            | "Name"
            | "NCName"
            | "NMTOKEN"
            | "decimal"
            | "integer"
            | "nonNegativeInteger"
            | "nonPositiveInteger"
            | "positiveInteger"
            | "negativeInteger"
            | "long"
            | "int"
            | "short"
            | "byte"
            | "unsignedLong"
            | "unsignedInt"
            | "unsignedShort"
            | "unsignedByte"
            | "double"
            | "float"
            | "boolean"
            | "hexBinary"
            | "base64Binary"
            | "anyURI"
            | "dateTime"
            | "dateTimeStamp"
    )
}

/// The `rdfs:Datatype` node for a non-named data range, at `indent` spaces.
fn render_datatype_node(dr: &horned_owl::model::DataRange<RcStr>, indent: usize) -> String {
    use horned_owl::model::DataRange as DR;
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 4);
    let pad3 = " ".repeat(indent + 8);
    let pad4 = " ".repeat(indent + 12);
    // The operands of a union or intersection are a set, in data range order.
    let collection = |tag: &str, items: &[horned_owl::model::DataRange<RcStr>]| -> String {
        let mut items: Vec<&horned_owl::model::DataRange<RcStr>> = items.iter().collect();
        items.sort_by(|a, b| crate::io::owlfunc::cmp_dr(a, b));
        items.dedup();
        let mut s = format!("{pad}<rdfs:Datatype>\n{pad2}<owl:{tag} rdf:parseType=\"Collection\">\n");
        for it in items {
            match it {
                DR::Datatype(d) => s.push_str(&format!(
                    "{pad3}<rdf:Description rdf:about=\"{}\"/>\n",
                    esc_attr(d.0.as_ref())
                )),
                other => s.push_str(&render_datatype_node(other, indent + 8)),
            }
        }
        s.push_str(&format!("{pad2}</owl:{tag}>\n{pad}</rdfs:Datatype>\n"));
        s
    };
    match dr {
        DR::Datatype(d) => {
            format!("{pad}<rdfs:Datatype rdf:about=\"{}\"/>\n", esc_attr(d.0.as_ref()))
        }
        DR::DatatypeRestriction(d, facets) => {
            let mut s = format!(
                "{pad}<rdfs:Datatype>\n{pad2}<owl:onDatatype rdf:resource=\"{}\"/>\n{pad2}<owl:withRestrictions rdf:parseType=\"Collection\">\n",
                esc_attr(d.0.as_ref())
            );
            // The restrictions are a set, by facet and then value.
            let mut facets: Vec<&horned_owl::model::FacetRestriction<RcStr>> = facets.iter().collect();
            facets.sort_by(|a, b| {
                facet_rank(a.f.as_ref()).cmp(&facet_rank(b.f.as_ref())).then_with(|| literal_key(&a.l).cmp(&literal_key(&b.l)))
            });
            facets.dedup();
            for f in facets {
                // Every OWL 2 facet but `rdf:langRange` is in the XSD namespace;
                // the writer declares both, so the qname is fixed.
                let local = f.f.as_ref().rsplit(['#', '/']).next().unwrap_or("").to_string();
                let prefix = if f.f.as_ref().starts_with(RDF_NS) { "rdf" } else { "xsd" };
                s.push_str(&format!("{pad3}<rdf:Description>\n"));
                s.push_str(&render_literal_tag(&format!("{prefix}:{local}"), &f.l, indent + 12));
                let _ = &pad4;
                s.push_str(&format!("{pad3}</rdf:Description>\n"));
            }
            s.push_str(&format!("{pad2}</owl:withRestrictions>\n{pad}</rdfs:Datatype>\n"));
            s
        }
        // A DATA `owl:oneOf` is an explicit `rdf:List`: its members are literals,
        // and `rdf:parseType="Collection"` can only hold resources.
        DR::DataOneOf(lits) => {
            // The values are a set, in literal order.
            let mut lits: Vec<Literal<RcStr>> = lits.clone();
            lits.sort_by_key(literal_key);
            lits.dedup();
            let mut s = format!("{pad}<rdfs:Datatype>\n{pad2}<owl:oneOf>\n");
            s.push_str(&render_literal_list(&lits, indent + 8));
            s.push_str(&format!("{pad2}</owl:oneOf>\n{pad}</rdfs:Datatype>\n"));
            s
        }
        DR::DataUnionOf(items) => collection("unionOf", items),
        DR::DataIntersectionOf(items) => collection("intersectionOf", items),
        DR::DataComplementOf(inner) => match inner.as_ref() {
            DR::Datatype(d) => format!(
                "{pad}<rdfs:Datatype>\n{pad2}<owl:datatypeComplementOf rdf:resource=\"{}\"/>\n{pad}</rdfs:Datatype>\n",
                esc_attr(d.0.as_ref())
            ),
            other => format!(
                "{pad}<rdfs:Datatype>\n{pad2}<owl:datatypeComplementOf>\n{}{pad2}</owl:datatypeComplementOf>\n{pad}</rdfs:Datatype>\n",
                render_datatype_node(other, indent + 8)
            ),
        },
    }
}

/// A facet's position in the order facet restrictions sort in.
pub(crate) fn facet_rank(iri: &str) -> usize {
    const FACETS: [&str; 11] = [
        "length",
        "minLength",
        "maxLength",
        "pattern",
        "minInclusive",
        "minExclusive",
        "maxInclusive",
        "maxExclusive",
        "totalDigits",
        "fractionDigits",
        "langRange",
    ];
    let local = iri.rsplit(['#', '/']).next().unwrap_or("");
    FACETS.iter().position(|f| *f == local).unwrap_or(FACETS.len())
}

/// Sort key for a data range, so multiple ranges on one property render in a
/// stable order (named datatypes by IRI).
fn dr_key(dr: &horned_owl::model::DataRange<RcStr>) -> String {
    use horned_owl::model::DataRange as DR;
    match dr {
        DR::Datatype(d) => {
            let (n, r) = iri_key(d.0.as_ref());
            format!("0\u{1}{n}\u{0}{r}")
        }
        _ => "1".to_string(),
    }
}

/// A data range as the object of `tag` on a property or datatype block, and —
/// when its axiom is annotated — the reification's `owl:annotatedTarget`. A named
/// datatype is a resource in both places. An anonymous range of an annotated
/// axiom is a node of its own: named by the id the numbering pass gave it
/// (`seq`, consumed in allocation order from `pos`) in both places, and defined
/// once after the block (appended to `defs`). An inline-anon document numbers
/// nothing, and nests a copy in both places instead.
fn annotated_data_range(
    tag: &str,
    dr: &horned_owl::model::DataRange<RcStr>,
    anns: &[(String, AnnotationValue<RcStr>)],
    seq: Option<&Vec<(String, u64)>>,
    pos: &mut usize,
    defs: &mut String,
) -> (String, Option<String>) {
    use horned_owl::model::DataRange as DR;
    let edge = render_data_range_at(tag, dr, 8);
    if anns.is_empty() {
        return (edge, None);
    }
    if let DR::Datatype(d) = dr {
        let target = format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(d.0.as_ref()));
        return (edge, Some(target));
    }
    if inline_anon() {
        return (edge, Some(render_data_range_at("owl:annotatedTarget", dr, 8)));
    }
    let sig = crate::io::genid::dr_sig(dr);
    let id = seq
        .and_then(|v| {
            let start = (*pos).min(v.len());
            v[start..].iter().position(|(s, _)| *s == sig).map(|off| {
                *pos = start + off + 1;
                v[*pos - 1].1
            })
        })
        .map(|g| format!("genid{g}"))
        .unwrap_or_default();
    defs.push_str(&inject_nodeid(&render_datatype_node(dr, 4), &id));
    (
        format!("        <{tag} rdf:nodeID=\"{id}\"/>\n"),
        Some(format!("        <owl:annotatedTarget rdf:nodeID=\"{id}\"/>\n")),
    )
}

/// The blank node id the numbering pass gave `owner`'s annotated anonymous
/// `ce`: the next one in allocation order with its signature, else whichever it
/// recorded for that signature.
fn shared_node_id(
    seq: Option<&Vec<(String, u64)>>,
    by_sig: Option<&std::collections::HashMap<String, u64>>,
    ce: &CE<RcStr>,
    pos: &mut usize,
) -> String {
    let sig = crate::io::genid::ce_sig(ce);
    let from_seq = seq.and_then(|v| {
        let start = (*pos).min(v.len());
        v[start..].iter().position(|(s, _)| *s == sig).map(|off| {
            *pos = start + off + 1;
            v[*pos - 1].1
        })
    });
    from_seq
        .or_else(|| by_sig.and_then(|m| m.get(&sig)).copied())
        .map(|g| format!("genid{g}"))
        .unwrap_or_default()
}

/// A property `rdfs:domain`/`rdfs:range` value: a bare `rdf:resource` for a named
/// class, else the class expression nested under the tag (indented 12).
fn render_prop_ce(tag: &str, ce: &CE<RcStr>, g: &Genids) -> String {
    match ce {
        CE::Class(c) => format!("        <{tag} rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref())),
        _ => format!("        <{tag}>\n{}        </{tag}>\n", render_ce(ce, 12, g)),
    }
}

/// The class expressions for one property's domain/range, sorted by `ce_key`, as
/// the writer renders multiple values.
fn sorted_ce(v: Option<&Vec<CE<RcStr>>>) -> Vec<&CE<RcStr>> {
    let mut out: Vec<&CE<RcStr>> = v.map(|xs| xs.iter().collect()).unwrap_or_default();
    out.sort_by(|a, b| ce_key(a).cmp(&ce_key(b)));
    out.dedup_by(|a, b| ce_key(a) == ce_key(b));
    out
}

/// As `sorted_ce`, for domain/range values that carry their axiom annotations.
#[allow(clippy::type_complexity)]
fn sorted_prop_ce(
    v: Option<&Vec<(CE<RcStr>, Vec<(String, AnnotationValue<RcStr>)>)>>,
) -> Vec<&(CE<RcStr>, Vec<(String, AnnotationValue<RcStr>)>)> {
    let mut out: Vec<&(CE<RcStr>, Vec<(String, AnnotationValue<RcStr>)>)> =
        v.map(|xs| xs.iter().collect()).unwrap_or_default();
    out.sort_by(|a, b| ce_key(&a.0).cmp(&ce_key(&b.0)));
    out.dedup_by(|a, b| ce_key(&a.0) == ce_key(&b.0) && a.1 == b.1);
    out
}

/// `owl:Class` wrapping an `intersectionOf`/`unionOf` Collection of operands.
fn render_set(ops: &[CE<RcStr>], tag: &str, indent: usize, g: &Genids) -> String {
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 4);
    let inner = " ".repeat(indent + 8);
    let mut s = format!("{pad}<owl:Class>\n{pad2}<owl:{tag} rdf:parseType=\"Collection\">\n");
    let mut sorted: Vec<&CE<RcStr>> = ops.iter().collect();
    sorted.sort_by(|a, b| ce_key(a).cmp(&ce_key(b)));
    for op in sorted {
        match op {
            CE::Class(c) => s.push_str(&format!("{inner}<rdf:Description rdf:about=\"{}\"/>\n", esc_attr(c.0.as_ref()))),
            _ => match g.get(&ce_sig(op)) {
                Some(gid) => s.push_str(&format!("{inner}<rdf:Description rdf:nodeID=\"{gid}\"/>\n")),
                None => s.push_str(&render_ce(op, indent + 8, g)),
            },
        }
    }
    s.push_str(&format!("{pad2}</owl:{tag}>\n{pad}</owl:Class>\n"));
    s
}

/// The order of the object-property characteristic axioms, which is the order
/// their `rdf:type` triples render in.
fn char_axiom_rank(iri: &str) -> u8 {
    match iri.rsplit('#').next().unwrap_or("") {
        "FunctionalProperty" => 0,
        "InverseFunctionalProperty" => 1,
        "SymmetricProperty" => 2,
        "AsymmetricProperty" => 3,
        "TransitiveProperty" => 4,
        "ReflexiveProperty" => 5,
        "IrreflexiveProperty" => 6,
        _ => 7,
    }
}

const XSD_NNI: &str = "http://www.w3.org/2001/XMLSchema#nonNegativeInteger";

/// A cardinality `owl:Restriction`. With a named filler other than owl:Thing it
/// is qualified (`*QualifiedCardinality` + `onClass`); over owl:Thing it is plain.
fn render_card(n: u32, ope: &OPEx<RcStr>, bce: &CE<RcStr>, qtag: &str, ptag: &str, indent: usize, g: &Genids) -> String {
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 4);
    let mut s = format!(
        "{pad}<owl:Restriction>\n{}",
        render_on_property(ope, indent + 4)
    );
    let is_thing = matches!(bce, CE::Class(c) if c.0.as_ref() == "http://www.w3.org/2002/07/owl#Thing");
    if is_thing {
        s.push_str(&format!("{pad2}<owl:{ptag} rdf:datatype=\"{XSD_NNI}\">{n}</owl:{ptag}>\n"));
    } else {
        s.push_str(&format!("{pad2}<owl:{qtag} rdf:datatype=\"{XSD_NNI}\">{n}</owl:{qtag}>\n"));
        match bce {
            CE::Class(c) => s.push_str(&format!("{pad2}<owl:onClass rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref()))),
            _ => match g.get(&ce_sig(bce)) {
                Some(gid) => s.push_str(&format!("{pad2}<owl:onClass rdf:nodeID=\"{gid}\"/>\n")),
                None => s.push_str(&format!("{pad2}<owl:onClass>\n{}{pad2}</owl:onClass>\n", render_ce(bce, indent + 8, g))),
            },
        }
    }
    s.push_str(&format!("{pad}</owl:Restriction>\n"));
    s
}

/// A data-property cardinality restriction. `rdfs:Literal` as the range is the
/// unqualified form (`owl:minCardinality` and nothing else); any other range is
/// the qualified form, with the count under `owl:<q>QualifiedCardinality` and the
/// range under `owl:onDataRange`. Children follow the order the restriction is
/// built in: the property, the count, then the range.
fn render_data_card(
    n: u32,
    dp: &horned_owl::model::DataProperty<RcStr>,
    dr: &horned_owl::model::DataRange<RcStr>,
    qtag: &str,
    ptag: &str,
    indent: usize,
) -> String {
    use horned_owl::model::DataRange as DR;
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 4);
    let mut s = format!(
        "{pad}<owl:Restriction>\n{pad2}<owl:onProperty rdf:resource=\"{}\"/>\n",
        esc_attr(dp.0.as_ref())
    );
    let unqualified = matches!(dr, DR::Datatype(d) if d.0.as_ref() == RDFS_LITERAL);
    if unqualified {
        s.push_str(&format!("{pad2}<owl:{ptag} rdf:datatype=\"{XSD_NNI}\">{n}</owl:{ptag}>\n"));
    } else {
        s.push_str(&format!("{pad2}<owl:{qtag} rdf:datatype=\"{XSD_NNI}\">{n}</owl:{qtag}>\n"));
        s.push_str(&render_data_range_at("owl:onDataRange", dr, indent + 4));
    }
    s.push_str(&format!("{pad}</owl:Restriction>\n"));
    s
}

/// Insert a child block (already indented) immediately before an anonymous class
/// node's final closing tag — used to hang a `subClassOf`/`disjointWith` edge on a
/// general-axiom subject rendered as an inline `owl:Class`/`owl:Restriction`.
fn insert_before_close(node: &str, ins: &str) -> String {
    let node = node.strip_suffix('\n').unwrap_or(node);
    match node.rfind('\n') {
        Some(i) => format!("{}{}{}\n", &node[..i + 1], ins, &node[i + 1..]),
        None => format!("{ins}{node}\n"),
    }
}

/// An edge (`rdfs:subClassOf` / `owl:disjointWith`) to a class expression, as an
/// 8-space child: bare `rdf:resource` for a named class, else a nested element.
fn edge_to(tag: &str, ce: &CE<RcStr>, g: &Genids) -> String {
    match ce {
        CE::Class(c) => format!("        <{tag} rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref())),
        _ => format!("        <{tag}>\n{}        </{tag}>\n", render_ce(ce, 12, g)),
    }
}

/// A general-class-inclusion `SubClassOf(anonSub, sup)`: the anonymous subclass
/// rendered as its own inline node, carrying an `rdfs:subClassOf` edge to `sup`.
fn render_gci_subclass(sub: &CE<RcStr>, sup: &CE<RcStr>, g: &Genids) -> String {
    insert_before_close(&render_ce(sub, 4, g), &edge_to("rdfs:subClassOf", sup, g))
}

/// A general `EquivalentClasses(a, b)` where BOTH members are anonymous. There is
/// no named class to hang it on, so it belongs in the general-axioms section: the
/// member that sorts first is the subject, carrying an `owl:equivalentClass` edge
/// to the other. UBERON's `uberon_bot.owl` has seven, each a union of `part_of`
/// restrictions equivalent to a third.
fn render_gci_equivalent(a: &CE<RcStr>, b: &CE<RcStr>, g: &Genids) -> String {
    let (host, other) = if ce_key(a) <= ce_key(b) { (a, b) } else { (b, a) };
    insert_before_close(&render_ce(host, 4, g), &edge_to("owl:equivalentClass", other, g))
}

/// An ANNOTATED `EquivalentClasses(anon, anon)` general axiom, reified.
///
/// The unannotated form renders the pair as one inline block, which has nowhere
/// to carry an axiom annotation; reifying gives the annotation a home. Reified
/// exactly as the disjoint and subclass GCIs are: the base triple makes the
/// target appear twice — as the `owl:equivalentClass` object and as
/// `owl:annotatedTarget` — so it is a shared node (`rdf:nodeID`) defined once
/// after the `owl:Axiom`, while the source appears once inline inside
/// `annotatedSource`.
fn render_gci_equivalent_annotated(
    members: &[CE<RcStr>],
    shared: &std::collections::HashMap<String, std::collections::HashMap<String, u64>>,
    anns: &[(String, AnnotationValue<RcStr>)],
    prefixes: &[(String, String)],
) -> String {
    let mut ops: Vec<&CE<RcStr>> = members.iter().collect();
    ops.sort_by(|a, b| crate::io::owlfunc::cmp_ce(a, b));
    let (src, tgt) = (ops[0], ops[1]);
    let no_g = Genids::new();
    // Inline-anon: the edge nests a full copy of the target inside the source
    // block, the annotatedTarget carries another, and no standalone definition
    // follows the axiom.
    let (edge, target, defn) = if inline_anon() {
        (
            format!(
                "                <owl:equivalentClass>\n{}                </owl:equivalentClass>\n",
                render_ce(tgt, 20, &no_g)
            ),
            format!(
                "        <owl:annotatedTarget>\n{}        </owl:annotatedTarget>\n",
                render_ce(tgt, 12, &no_g)
            ),
            String::new(),
        )
    } else {
        let gid = shared
            .get("__general__")
            .and_then(|m| m.get(&crate::io::genid::ce_sig(tgt)))
            .map(|g| format!("genid{g}"))
            .unwrap_or_default();
        (
            format!("                <owl:equivalentClass rdf:nodeID=\"{gid}\"/>\n"),
            format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n"),
            inject_nodeid(&render_ce(tgt, 4, &no_g), &gid),
        )
    };
    let src_block = insert_before_close(&render_ce(src, 12, &no_g), &edge);
    let mut s = String::from("    <owl:Axiom>\n");
    s.push_str("        <owl:annotatedSource>\n");
    s.push_str(&src_block);
    s.push_str("        </owl:annotatedSource>\n");
    s.push_str(&format!("        <owl:annotatedProperty rdf:resource=\"{EQUIV_PROP}\"/>\n"));
    s.push_str(&target);
    let mut ns: Vec<&(String, AnnotationValue<RcStr>)> = anns.iter().collect();
    ns.sort_by(|a, b| ann_key(&a.0, &a.1).cmp(&ann_key(&b.0, &b.1)));
    for (p, av) in ns {
        s.push_str(&render_ann(p, av, prefixes));
    }
    s.push_str("    </owl:Axiom>\n");
    s.push_str(&defn);
    s
}

/// A general `DisjointClasses(a, b)` with an anonymous member: rendered on the
/// member whose expression sorts first, with an `owl:disjointWith` edge to the other.
fn render_gci_disjoint(a: &CE<RcStr>, b: &CE<RcStr>, g: &Genids) -> String {
    // Host the axiom on an anonymous member (a named subject would render empty);
    // when both are anonymous, on the one that sorts first.
    let (host, other) = if matches!(a, CE::Class(_)) {
        (b, a)
    } else if matches!(b, CE::Class(_)) {
        (a, b)
    } else if ce_key(a) <= ce_key(b) {
        (a, b)
    } else {
        (b, a)
    };
    insert_before_close(&render_ce(host, 4, g), &edge_to("owl:disjointWith", other, g))
}

const DISJOINT_PROP: &str = "http://www.w3.org/2002/07/owl#disjointWith";

const SUBCLASS_PROP: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// An ANNOTATED general `SubClassOf(anonSub, sup)` — a GCI. It reifies just as an
/// annotated general disjoint does: the base triple makes `sup` appear twice, as
/// the `rdfs:subClassOf` object and as `owl:annotatedTarget`, so an anonymous
/// `sup` becomes a shared node (`rdf:nodeID`) defined once after the `owl:Axiom`,
/// while the anonymous subclass appears once, inline inside `annotatedSource`.
/// Rendering the GCI as a bare top-level block instead would drop the
/// reification entirely — 130 `owl:Axiom` blocks in UBERON.
fn render_gci_subclass_annotated(
    sub: &CE<RcStr>,
    sup: &CE<RcStr>,
    shared: &std::collections::HashMap<String, std::collections::HashMap<String, u64>>,
    anns: &[(String, AnnotationValue<RcStr>)],
    prefixes: &[(String, String)],
    gid_override: Option<String>,
) -> String {
    let no_g = Genids::new();
    // A named super is a plain resource on both the edge and the target; an
    // anonymous one is the shared blank node.
    let (edge, target, defn) = match sup {
        CE::Class(c) => {
            let iri = esc_attr(c.0.as_ref());
            (
                format!("                <rdfs:subClassOf rdf:resource=\"{iri}\"/>\n"),
                format!("        <owl:annotatedTarget rdf:resource=\"{iri}\"/>\n"),
                String::new(),
            )
        }
        _ if inline_anon() => (
            format!(
                "                <rdfs:subClassOf>\n{}                </rdfs:subClassOf>\n",
                render_ce(sup, 20, &no_g)
            ),
            format!(
                "        <owl:annotatedTarget>\n{}        </owl:annotatedTarget>\n",
                render_ce(sup, 12, &no_g)
            ),
            String::new(),
        ),
        _ => {
            let gid = gid_override.clone().unwrap_or_else(|| {
                shared
                    .get("__general__")
                    .and_then(|m| m.get(&crate::io::genid::ce_sig(sup)))
                    .map(|g| format!("genid{g}"))
                    .unwrap_or_default()
            });
            (
                format!("                <rdfs:subClassOf rdf:nodeID=\"{gid}\"/>\n"),
                format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n"),
                inject_nodeid(&render_ce(sup, 4, &no_g), &gid),
            )
        }
    };
    let src_block = insert_before_close(&render_ce(sub, 12, &no_g), &edge);
    let mut s = String::from("    <owl:Axiom>\n");
    s.push_str("        <owl:annotatedSource>\n");
    s.push_str(&src_block);
    s.push_str("        </owl:annotatedSource>\n");
    s.push_str(&format!("        <owl:annotatedProperty rdf:resource=\"{SUBCLASS_PROP}\"/>\n"));
    s.push_str(&target);
    let mut ns: Vec<&(String, AnnotationValue<RcStr>)> = anns.iter().collect();
    ns.sort_by(|a, b| ann_key(&a.0, &a.1).cmp(&ann_key(&b.0, &b.1)));
    for (p, av) in ns {
        s.push_str(&render_ann(p, av, prefixes));
    }
    s.push_str("    </owl:Axiom>\n");
    s.push_str(&defn);
    s
}

/// An ANNOTATED `DisjointClasses(anon, anon)` general axiom, reified: the base
/// triple `src disjointWith tgt` makes `tgt` appear twice (as the edge
/// object and as the reification's annotatedTarget), so `tgt` is a shared node
/// (`rdf:nodeID`) defined once after the `owl:Axiom`; `src` appears once, inline
/// inside `annotatedSource`. `src`/`tgt` are the operands in cmp_ce order — the
/// same order the genid pass used to assign `tgt`'s shared genid under
/// `"__general__"`.
fn render_gci_disjoint_annotated(
    members: &[CE<RcStr>],
    shared: &std::collections::HashMap<String, std::collections::HashMap<String, u64>>,
    anns: &[(String, AnnotationValue<RcStr>)],
    prefixes: &[(String, String)],
) -> String {
    let mut ops: Vec<&CE<RcStr>> = members.iter().collect();
    ops.sort_by(|a, b| crate::io::owlfunc::cmp_ce(a, b));
    let (src, tgt) = (ops[0], ops[1]);
    let no_g = Genids::new();
    // Inline-anon: the edge nests a full copy of the target inside the source
    // block, the annotatedTarget carries another, and no standalone definition
    // follows the axiom.
    let (edge, target, defn) = if inline_anon() {
        (
            format!(
                "                <owl:disjointWith>\n{}                </owl:disjointWith>\n",
                render_ce(tgt, 20, &no_g)
            ),
            format!(
                "        <owl:annotatedTarget>\n{}        </owl:annotatedTarget>\n",
                render_ce(tgt, 12, &no_g)
            ),
            String::new(),
        )
    } else {
        let gid = shared
            .get("__general__")
            .and_then(|m| m.get(&crate::io::genid::ce_sig(tgt)))
            .map(|g| format!("genid{g}"))
            .unwrap_or_default();
        (
            format!("                <owl:disjointWith rdf:nodeID=\"{gid}\"/>\n"),
            format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n"),
            // Target restriction standalone with nodeID (the shared node's definition).
            inject_nodeid(&render_ce(tgt, 4, &no_g), &gid),
        )
    };
    let src_block = insert_before_close(&render_ce(src, 12, &no_g), &edge);
    let mut s = String::from("    <owl:Axiom>\n");
    s.push_str("        <owl:annotatedSource>\n");
    s.push_str(&src_block);
    s.push_str("        </owl:annotatedSource>\n");
    s.push_str(&format!("        <owl:annotatedProperty rdf:resource=\"{DISJOINT_PROP}\"/>\n"));
    s.push_str(&target);
    let mut ns: Vec<&(String, AnnotationValue<RcStr>)> = anns.iter().collect();
    ns.sort_by(|a, b| ann_key(&a.0, &a.1).cmp(&ann_key(&b.0, &b.1)));
    for (p, av) in ns {
        s.push_str(&render_ann(p, av, prefixes));
    }
    s.push_str("    </owl:Axiom>\n");
    s.push_str(&defn);
    s
}

/// An `owl:AllDisjointClasses` block for a `DisjointClasses` axiom with 3+ members
/// (an nary disjoint has no named subject, so it is a general axiom). The
/// anonymous subject node is rendered inline (`rdf:Description`, no nodeID);
/// members form an rdf:List in class-expression sort order — named members as
/// `rdf:Description rdf:about`, anonymous ones nested.
fn render_all_disjoint(members: &[CE<RcStr>], g: &Genids, anns: &str) -> String {
    let mut ms: Vec<&CE<RcStr>> = members.iter().collect();
    ms.sort_by(|a, b| ce_key(a).cmp(&ce_key(b)));
    let mut s = String::from("    <rdf:Description>\n");
    s.push_str("        <rdf:type rdf:resource=\"http://www.w3.org/2002/07/owl#AllDisjointClasses\"/>\n");
    s.push_str("        <owl:members rdf:parseType=\"Collection\">\n");
    for m in ms {
        match m {
            CE::Class(c) => s.push_str(&format!(
                "            <rdf:Description rdf:about=\"{}\"/>\n",
                esc_attr(c.0.as_ref())
            )),
            _ => s.push_str(&render_ce(m, 12, g)),
        }
    }
    s.push_str("        </owl:members>\n");
    s.push_str(anns);
    s.push_str("    </rdf:Description>\n");
    s
}

/// An axiom's annotations as statements of the node that stands for it, in
/// annotation order.
/// An axiom's annotations, in the order they are written.
fn sorted_anns(ac: &AnnotatedComponent<RcStr>) -> Vec<(String, AnnotationValue<RcStr>)> {
    let mut anns = ax_anns(ac);
    anns.sort_by_key(|a| ann_key(&a.0, &a.1));
    anns
}

fn node_annotations(ac: &AnnotatedComponent<RcStr>, prefixes: &[(String, String)]) -> String {
    sorted_anns(ac).iter().map(|(p, av)| render_ann(p, av, prefixes)).collect()
}

/// An `owl:AllDisjointProperties` block for a disjointness of three or more
/// properties: the axiom's annotations, the type, then the members — named
/// properties by IRI, then inverses.
fn render_all_disjoint_properties(members: &[(Option<String>, String)], anns: &str) -> String {
    let mut ms: Vec<&(Option<String>, String)> = members.iter().collect();
    ms.sort_by(|a, b| a.0.is_some().cmp(&b.0.is_some()).then_with(|| iri_key(&a.1).cmp(&iri_key(&b.1))));
    let mut s = String::from("    <rdf:Description>\n");
    s.push_str(anns);
    s.push_str("        <rdf:type rdf:resource=\"http://www.w3.org/2002/07/owl#AllDisjointProperties\"/>\n");
    s.push_str("        <owl:members rdf:parseType=\"Collection\">\n");
    for (inverse, iri) in ms {
        match inverse {
            None => s.push_str(&format!("            <rdf:Description rdf:about=\"{}\"/>\n", esc_attr(iri))),
            Some(_) => s.push_str(&format!(
                "            <rdf:Description>\n                <owl:inverseOf rdf:resource=\"{}\"/>\n            </rdf:Description>\n",
                esc_attr(iri)
            )),
        }
    }
    s.push_str("        </owl:members>\n");
    s.push_str("    </rdf:Description>\n");
    s
}

/// An individual list in render order — named individuals by IRI, then the
/// anonymous ones (`None`).
/// [`sorted_members`] with each anonymous member as its slot's mark and id,
/// after the named ones in node order.
fn slot_members(inds: &[horned_owl::model::Individual<RcStr>]) -> Vec<Option<String>> {
    let mut v: Vec<Option<String>> = inds
        .iter()
        .map(|i| match i {
            horned_owl::model::Individual::Named(n) => Some(n.0.as_ref().to_string()),
            horned_owl::model::Individual::Anonymous(a) => Some(format!("{ANON_SLOT}{}", a.0.as_ref())),
        })
        .collect();
    v.sort_by(|a, b| identity_key(a).cmp(&identity_key(b)));
    v.dedup();
    v
}

fn sorted_members(inds: &[horned_owl::model::Individual<RcStr>]) -> Vec<Option<String>> {
    let mut v: Vec<Option<String>> = inds
        .iter()
        .map(|i| match i {
            horned_owl::model::Individual::Named(n) => Some(n.0.as_ref().to_string()),
            horned_owl::model::Individual::Anonymous(_) => None,
        })
        .collect();
    v.sort_by(|a, b| member_key(a).cmp(&member_key(b)));
    v
}

/// Sort key for a member of an individual list: named first, by IRI.
/// [`member_key`] of an identity edge's object, an anonymous member, written
/// by its slot, after the named ones in node order.
fn identity_key(m: &Option<String>) -> (u8, (&str, &str)) {
    match m {
        Some(x) if x.starts_with(ANON_SLOT) => (1, (x.as_str(), "")),
        _ => member_key(m),
    }
}

fn member_key(m: &Option<String>) -> (u8, (&str, &str)) {
    match m {
        Some(iri) => (0, iri_key(iri)),
        None => (1, ("", "")),
    }
}

/// An `owl:AllDifferent` / `owl:distinctMembers` block for a `DifferentIndividuals`
/// axiom, members sorted by IRI.
fn render_all_different(members: &[String], anns: &str) -> String {
    let mut ms: Vec<Option<String>> = members.iter().cloned().map(Some).collect();
    ms.sort_by(|a, b| identity_key(a).cmp(&identity_key(b)));
    let mut s = String::from("    <rdf:Description>\n");
    s.push_str("        <rdf:type rdf:resource=\"http://www.w3.org/2002/07/owl#AllDifferent\"/>\n");
    s.push_str("        <owl:distinctMembers rdf:parseType=\"Collection\">\n");
    for m in ms.into_iter().flatten() {
        match m.strip_prefix(ANON_SLOT) {
            Some(x) => s.push_str(&anon_item(x, "            ")),
            None => s.push_str(&format!("            <rdf:Description rdf:about=\"{}\"/>\n", esc_attr(&m))),
        }
    }
    s.push_str("        </owl:distinctMembers>\n");
    s.push_str(anns);
    s.push_str("    </rdf:Description>\n");
    s
}

/// Comparison key for an axiom's nested-annotation list: each nested annotation's
/// `ann_key`, sorted, so two same-(property, value) assertions order by their
/// reified annotations (e.g. two defs differing only in hasDbXref).
fn nested_key(nested: &[(String, AnnotationValue<RcStr>)]) -> Vec<AnnKey> {
    let mut v: Vec<AnnKey> = nested.iter().map(|(p, a)| ann_key(p, a)).collect();
    v.sort();
    v
}

/// Build an entity's annotation-assertion body (sorted by (property, value)) and
/// the `<owl:Axiom>` reifications for any that carry nested annotations.
fn annotation_body(
    iri: &str,
    anns: Option<&Vec<Ann>>,
    frame_order: Option<&Vec<Ann>>,
    prefixes: &[(String, String)],
) -> (String, String) {
    let mut body = String::new();
    let mut after = String::new();
    if let Some(anns) = anns {
        let mut sorted = anns.clone();
        sorted.sort_by(|a, b| {
            ann_key(&a.0, &a.1)
                .cmp(&ann_key(&b.0, &b.1))
                .then_with(|| nested_key(&a.2).cmp(&nested_key(&b.2)))
        });
        // A frame with a plain/`xsd:string` twin writes its assertions in its own
        // order (see [`crate::io::frame_twins`]), and a triple two of them state
        // stands where the first of them does.
        if let Some(order) = frame_order {
            sorted = order.clone();
        }
        // The plain annotation triple is emitted once per distinct (property,
        // value) — RDF triples are a set — even when several axioms assert it with
        // different reified annotations, and even when the axioms differ in a way
        // the TRIPLE cannot express. `IAO_0000115` carries `"definition"` from
        // both the OBO reader (an `xsd:string` literal) and `merged_import.owl`
        // (a plain one); they sort apart, on either side of `"definition"@en`, but
        // render the same triple. The LAST of a duplicated line keeps its
        // position — OBA's released `oba-full.owl` has `@en` first and the bare
        // one after it, which is where the `xsd:string` copy sorts — unless one
        // of the copies is reified: an annotated axiom's triple stands where
        // that axiom sorts, which for a plain literal is ahead of every
        // `xsd:string` one. UBERON's mapping component asserts an xref the edit
        // file also carries, with provenance on its copy; such a class frame
        // takes its order from `frame_order` instead.
        //
        // Two lines are the same triple unless the value is an ANONYMOUS
        // individual: its node identity is not printed — every one renders as the
        // same empty `rdf:Description` — so the node id joins the key, or a
        // mapping set's 51,582 `sssom:mappings` values collapse into one.
        let lines: Vec<String> =
            sorted.iter().map(|(p, av, _)| render_ann(p, av, prefixes)).collect();
        let keys: Vec<String> = sorted
            .iter()
            .zip(&lines)
            .map(|((_, av, _), line)| match av {
                AnnotationValue::AnonymousIndividual(a) => format!("{line}\u{1}{}", a.0.as_ref()),
                _ => line.clone(),
            })
            .collect();
        let reified: Vec<bool> = sorted.iter().map(|(_, _, nested)| !nested.is_empty()).collect();
        for (i, line) in lines.iter().enumerate() {
            let twins = || (0..lines.len()).filter(|&j| j != i && keys[j] == keys[i]);
            let keep = if frame_order.is_some() {
                !twins().any(|j| j < i)
            } else if reified[i] {
                // The first reified copy speaks for all.
                !twins().any(|j| j < i && reified[j])
            } else {
                // The last unreified copy, where no copy is reified.
                !twins().any(|j| reified[j] || j > i)
            };
            if keep {
                body.push_str(line);
            }
        }
        for (p, av, nested) in &sorted {
            if !nested.is_empty() && !(anon_doc_active() && matches!(av, AnnotationValue::AnonymousIndividual(_))) {
                after.push_str(&render_reification(iri, p, av, nested, prefixes));
            }
        }
    }
    (body, after)
}

/// The full RDF/XML document: header, `owl:Ontology` block, the per-kind entity
/// sections, the untyped-annotation catch-all, the general axioms and the rules.
/// Serialize `model` as RDF/XML.
///
/// A document with no ontology IRI puts the OWL namespace in the default position
/// and writes its OWL elements unprefixed, so that case is buffered and rewritten
/// at the end (see [`strip_default_owl_prefix`]). Every other document streams
/// straight out, which is all of them but UBERON's fourteen `*-minimal` subsets.
///
/// When the model holds axioms whose shape this layout cannot state, nothing is
/// written and they come back, in functional syntax.
pub(crate) fn try_save<W: Write>(model: &mut Model, w: &mut W) -> Result<Vec<String>> {
    let has_iri = model.ont.iter().any(|ac| {
        matches!(&ac.component, Component::OntologyID(id) if id.iri.is_some())
    });
    let anonymous = model.ont.iter().any(|ac| !crate::io::genid::referenced_anonymous(ac).is_empty());
    if has_iri && !anonymous {
        return save_inner(model, w);
    }
    let mut buf: Vec<u8> = Vec::new();
    let unstated = save_inner(model, &mut buf)?;
    if !unstated.is_empty() {
        return Ok(unstated);
    }
    let doc = String::from_utf8(buf)
        .map_err(|e| anyhow::anyhow!("RDF/XML output is not valid UTF-8: {e}"))?;
    if anonymous && doc.contains([ANON_SLOT, ANON_SEP, ROOT_KEY]) {
        bail!("RDF/XML writer: an anonymous individual was not written");
    }
    let doc = if has_iri { doc } else { strip_default_owl_prefix(&doc) };
    w.write_all(doc.as_bytes())?;
    Ok(unstated)
}

/// Report a document written without this layout because of `unstated`.
pub(crate) fn warn_unstated(format: &str, unstated: &[String]) {
    status!(
        "WARN: the {format} layout cannot state {} axiom(s); the document is written as its plain RDF mapping instead (first: {})",
        unstated.len(),
        unstated[0]
    );
}

fn save_inner<W: Write>(model: &mut Model, w: &mut W) -> Result<Vec<String>> {
    struct AnonReset;
    impl Drop for AnonReset {
        fn drop(&mut self) {
            ANON_DOC.with(|d| *d.borrow_mut() = None);
        }
    }
    let _anon_reset = AnonReset;
    // Which datatype this document's untyped literals key as; it decides their
    // sort position against typed ones (see `plain_datatype`).
    PLAIN_TYPED.with(|c| c.set(model.plain_literals_typed));
    INLINE_ANON.with(|c| c.set(model.owlapi_456));
    let repeated: std::collections::HashSet<String> =
        anonymous_appearances(model).into_iter().filter(|(_, n)| *n > 1).map(|(id, _)| id).collect();
    ANON_REPEATED.with(|r| *r.borrow_mut() = repeated);
    // The class frames holding a plain/`xsd:string` twin, each with its
    // annotation assertions in the order it writes them; read after the datatype
    // key above is set, which they are ordered by.
    let frame_orders: std::collections::HashMap<String, Vec<Ann>> = crate::io::frame_twins::assertion_orders(model)
        .into_iter()
        .map(|(subject, acs)| {
            let anns = acs
                .into_iter()
                .filter_map(|ac| match &ac.component {
                    Component::AnnotationAssertion(aa) => {
                        Some((aa.ann.ap.0.as_ref().to_string(), aa.ann.av.clone(), ax_anns(ac)))
                    }
                    _ => None,
                })
                .collect();
            (subject, anns)
        })
        .collect();
    // Debug: dump genid pre-pass results for a window of ids
    // (OM_GENID_DEBUG="lo:hi").
    // `OM_MODEL_DEBUG=<substring>`: report the carried-metadata state of the model
    // reaching this writer. Two routes to the same artefact can differ ONLY in
    // this state, and that difference is what moves blank-node numbering, which is
    // what makes the same artefact come out differently along the two routes.
    if let Ok(want) = std::env::var("OM_MODEL_DEBUG") {
        let name = crate::io::out_name();
        if want.is_empty() || name.contains(&want) {
            eprintln!(
                "model[{name}]: shared_anon={} owl_genid_refs={} \
closure_declared={} closure_ann_ns={} materialised_decls={} \
idspaces={} rdf_prefixes={} explicit_prefixes={} plain_typed={} prefixes_cleared={} axioms={}",
                model.shared_anon.len(),
                model.owl_genid_refs.len(),
                model.closure_declared.len(),
                model.closure_ann_ns.len(),
                model.materialised_declarations.len(),
                model.idspaces.len(),
                model.rdf_prefixes.len(),
                model.explicit_prefixes.len(),
                model.plain_literals_typed,
                model.format_prefixes_cleared,
                model.ont.iter().count(),
            );
        }
    }
    if let Ok(spec) = std::env::var("OM_GENID_DEBUG") {
        let (lo, hi) = spec.split_once(':').unwrap_or(("940", "971"));
        let lo: u64 = lo.parse().unwrap_or(940);
        let hi: u64 = hi.parse().unwrap_or(971);
        let g = crate::io::genid::compute(model, lo, hi);
        eprintln!(
            "[genid] evidence: owl_shared_owners={} entries, shared_anon={} entries",
            model.owl_shared_owners.len(),
            model.shared_anon.len()
        );
        eprintln!("[genid] total counter = {}", g.counter);
        eprintln!("[genid] reuse_count = {}", g.reuse_count);
        eprintln!("[genid] reuse_miss  = {} (repeat = {})", g.reuse_miss, g.reuse_miss_repeat);
        eprintln!("[genid] dup_alloc   = {}", g.dup_alloc);
        eprintln!(
            "[genid] clauses: sub_sigs={} thisrun={} carried={} wildcard={} shared_key={} annotated={}",
            g.by_clause[0], g.by_clause[1], g.by_clause[2],
            g.by_clause[3], g.by_clause[4], g.by_clause[5]
        );
        // `OM_GENID_REIF=<owner substring>`: the genid each of that entity's
        // annotated axioms was assigned, in allocation order. The `owl:Axiom`
        // blocks are emitted sorted by the genid STRING, so a pair straddling a
        // digit-length boundary (999/1000) renders in the opposite order — this
        // is how a counter offset becomes visible as two swapped reification
        // blocks with otherwise identical content.
        if let Ok(want) = std::env::var("OM_GENID_REIF") {
            let mut owners: Vec<&String> =
                g.reif.keys().filter(|o| want.is_empty() || o.contains(&want)).collect();
            owners.sort();
            for o in owners {
                let mut v = g.reif[o].clone();
                v.sort_by_key(|(_, id)| *id);
                let shown: Vec<String> = v
                    .iter()
                    .map(|(sig, id)| format!("genid{id}={}", sig.split('\u{1}').next().unwrap_or("")))
                    .collect();
                eprintln!("[reif] {o}: {}", shown.join(" "));
            }
        }
        if let Ok(cap) = std::env::var("OM_GENID_STARTS") {
            let cap: u64 = cap.parse().unwrap_or(600);
            let mut starts: Vec<(&String, &u64)> =
                g.entity_start.iter().filter(|(_, v)| **v < cap).collect();
            starts.sort_by_key(|(_, v)| **v);
            for (o, v) in starts {
                eprintln!("[start] {v} {o}");
            }
        }
        if std::env::var("OM_GENID_REUSED").is_ok() {
            for (o, sigs) in &g.reused {
                eprintln!("[reused] {o}: {} sig(s)", sigs.len());
                for sig in sigs.iter().take(3) {
                    eprintln!("    {}", &sig[..sig.len().min(90)]);
                }
            }
        }
        if std::env::var("OM_GENID_STARTS").is_ok() {
            let mut v: Vec<(&String, &u64)> = g.entity_start.iter().collect();
            v.sort_by_key(|(_, c)| **c);
            let mut out = String::new();
            for (owner, c) in v {
                out.push_str(&format!("{c}\t{owner}\n"));
            }
            std::fs::write("/tmp/om_entity_starts.txt", out).ok();
        }
        if std::env::var("OM_GENID_REUSELOG").is_ok() {
            let mut out = String::new();
            for (owner, sigs) in &g.reused {
                for sig in sigs {
                    out.push_str(&format!("{owner}\t{sig}\n"));
                }
            }
            std::fs::write("/tmp/om_reused.txt", out).ok();
        }
        if std::env::var("OM_GENID_MISSLOG").is_ok() {
            let mut out = String::new();
            for (o, s) in &g.miss_log {
                out.push_str(&format!("{o}\t{s}\n"));
            }
            std::fs::write("/tmp/om_reuse_miss.txt", out).ok();
        }
        if std::env::var("OM_GENID_DUPLOG").is_ok() {
            let mut out = String::new();
            for (o, s) in &g.dup_log {
                out.push_str(&format!("{o}\t{s}\n"));
            }
            std::fs::write("/tmp/om_dup_alloc.txt", out).ok();
        }
        eprintln!(
            "[genid] writer_skips = {}",
            WRITER_SKIPS.with(|c| c.get())
        );
        for (o, id) in &g.reuse_log {
            eprintln!("  reuse {o} -> genid{id}");
        }
        let mut ids: Vec<_> = g.debug.keys().copied().collect();
        ids.sort();
        for id in ids {
            eprintln!("  genid{id} = {}", g.debug[&id]);
        }
        if std::env::var("OM_GENID_ENTITYSTART").is_ok() {
            let mut es: Vec<_> = g.entity_start.iter().collect();
            es.sort_by_key(|(_, v)| **v);
            let mut out = String::new();
            for (k, v) in es {
                out.push_str(&format!("{v}\t{k}\n"));
            }
            std::fs::write("/tmp/om_entity_start.txt", out).ok();
        }
    }

    // Compute the document prefixes once; used for the xmlns block and for
    // abbreviating every property/element qname in the body.
    let hdr_iri: String = model
        .ont
        .iter()
        .find_map(|ac| match &ac.component {
            Component::OntologyID(id) => id.iri.as_ref().map(|i| i.as_ref().to_string()),
            _ => None,
        })
        .unwrap_or_default();
    let mut prefixes = owlapi_prefixes(model);
    // The document namespace is also the DEFAULT one — `xmlns="<ontology IRI>#"`
    // in the header — so an element in it is written unprefixed even where the
    // same namespace also has a named prefix. Binding it first is what makes
    // `qname` reach for the empty prefix ahead of the named one.
    //
    // An ontology with no IRI is left alone: the OWL namespace takes the default
    // position there, and `strip_default_owl_prefix` does that pass over the
    // finished document.
    if !hdr_iri.is_empty() {
        prefixes.insert(0, (String::new(), format!("{hdr_iri}#")));
    }
    let prefixes = prefixes;
    // Compute genid blank-node numbering from the model (replaces the ids scanned
    // from the input document, which are wrong once om re-numbers). An
    // inline-anon document has no numbered nodes at all, so the pass is skipped
    // and every lookup below comes back empty — which is what routes each
    // anonymous expression to its inline rendering.
    let genid_pass = if model.owlapi_456 {
        crate::io::genid::Genids::default()
    } else {
        crate::io::genid::compute(model, 0, 0)
    };
    // Publish which anonymous expressions ended up sharing ONE blank node, so the
    // OFN cache written right after this can carry the fact to the next build step.
    // Without it the identity is lost and the next RDF/XML write renumbers.
    if !model.owlapi_456 {
        model.rdf_shared_anon = genid_pass
            .shared
            .iter()
            .map(|(owner, m)| {
                (
                    owner.clone(),
                    m.keys().map(|sig| crate::io::anon_sig_hash(sig)).collect(),
                )
            })
            .collect();
    }
    let shared_genids = &genid_pass.shared;
    // Per-owner genids in ALLOCATION order, consumed positionally below: two
    // annotated axioms over structurally-equal expressions are two distinct nodes,
    // which the signature-keyed `shared` map collapses into one.
    let shared_seq = &genid_pass.shared_seq;
    let reif_genids = &genid_pass.reif;
    if let Ok(t) = std::env::var("OM_REIF_DUMP") {
        let mut out = String::new();
        for (owner, v) in reif_genids.iter() {
            if owner.contains(&t) {
                for (sig, g) in v {
                    out.push_str(&format!("{owner}\t{g}\t{sig}\n"));
                }
            }
        }
        std::fs::write("/tmp/om_reif_dump.txt", out).ok();
    }
    use horned_owl::model::ObjectPropertyExpression as OPE;
    // Collect declarations by kind, annotation assertions (with nested
    // annotations) by subject, and per-object-property logical axioms.
    let mut ann_props: Vec<String> = Vec::new();
    let mut datatypes: Vec<String> = Vec::new();
    let mut obj_props: Vec<String> = Vec::new();
    let mut data_props: Vec<String> = Vec::new();
    let mut classes: Vec<String> = Vec::new();
    let mut individuals: Vec<String> = Vec::new();
    type SubSup = (CE<RcStr>, Vec<(String, AnnotationValue<RcStr>)>);
    let mut sub_class: BTreeMap<String, Vec<SubSup>> = BTreeMap::new();
    let mut equiv_class: BTreeMap<String, Vec<SubSup>> = BTreeMap::new();
    let mut disjoint_class: BTreeMap<String, Vec<CE<RcStr>>> = BTreeMap::new();
    // Annotated `DisjointClasses(named, anon)` per named class: the anonymous
    // target is a shared blank node and the axiom reifies.
    let mut disjoint_anon: BTreeMap<String, Vec<(CE<RcStr>, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    // Annotated `DisjointClasses(named, named)` reifications, keyed by the class
    // whose section renders the block; the value records the axiom's own
    // (source, target) pair. In an inline-anon document the block renders under
    // BOTH operands — the second copy keeps the first operand as its
    // `annotatedSource` — so the same (source, target, anns) entry appears under
    // each key.
    #[allow(clippy::type_complexity)]
    let mut disjoint_reif: BTreeMap<String, Vec<(String, String, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    // Per class, each disjoint union's collection items, its named members and
    // the axiom's annotations.
    #[allow(clippy::type_complexity)]
    let mut disjoint_union: BTreeMap<String, Vec<(Vec<String>, String, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    // `rdf:type` of a named individual — `ClassAssertion(C, i)`, rendered as the
    // first child of the `<owl:NamedIndividual>` element, before the annotations;
    // IAO's curation-status individuals are all typed this way.
    let mut ind_types: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // …and the ones whose class is an EXPRESSION — `ClassAssertion(anon, i)`,
    // written as a nested `<rdf:type>` element after the named ones. ENVO types
    // `Q2306597` by `BFO_0000051 some ENVO_00000248` and ONS types four of its
    // individuals by restrictions, so a mirror that could not write these would
    // carry fewer assertions than the ontology it stands for.
    let mut ind_types_anon: BTreeMap<String, Vec<SubSup>> = BTreeMap::new();
    // `owl:sameAs` / `owl:differentFrom` edges of a named individual. A nary
    // individual axiom is written PAIRWISE, so only a `DifferentIndividuals` of
    // three or more members ever reaches the `owl:AllDifferent` shape — a binary
    // one becomes a single edge on whichever of the two individuals sorts first.
    // GSSO's source states its 26 pairs as `owl:AllDifferent`, and every one of
    // them serialises back out as `owl:differentFrom`.
    // The object is `None` for an ANONYMOUS individual, written inline as an empty
    // `<rdf:Description/>`; a named one sorts ahead of it, because a named
    // individual ranks before an anonymous one and that rank is compared first.
    let mut ind_identity: BTreeMap<String, Vec<(&'static str, Option<String>)>> = BTreeMap::new();
    /// Property assertions on a named individual: `(property, value, literal)`.
    /// The third field is `None` for an object assertion — whose value is an IRI
    /// and renders as `rdf:resource` — and for a data assertion carries the
    /// element attributes its literal needs: `xml:lang` for a language literal,
    /// `rdf:datatype` for a typed one, and nothing for a plain string. ONS states
    /// three `OBI_0001937` values as `xsd:decimal`, so a bare-text literal would
    /// change what the mirror says.
    type IndProp = (String, String, Option<String>);
    let mut ind_props: BTreeMap<String, Vec<IndProp>> = BTreeMap::new();
    // An annotated property assertion is reified as an `<owl:Axiom>` after the
    // individual, like an annotated `rdf:type`: COHO's `has_data_collection_location`
    // assertions carry the recruitment quote and its source this way. The last
    // field is whether the assertion is on an inverse, stated the other way round.
    type PropReif = (String, String, Option<String>, Vec<(String, AnnotationValue<RcStr>)>, bool);
    let mut prop_reif: BTreeMap<String, Vec<PropReif>> = BTreeMap::new();
    let mut ann_assertions: BTreeMap<String, Vec<Ann>> = BTreeMap::new();
    let mut anon_ind: BTreeMap<String, Vec<Ann>> = BTreeMap::new();
    // `rdf:type` of an anonymous individual — `ClassAssertion(C, _:x)`, the first
    // child of its `<rdf:Description>` block. It is what says the block describes
    // an individual at all: a reader meeting a bare blank node with only
    // annotations on it has nothing to tell it apart from the structure a
    // document leaves behind, and an SSSOM mapping set is exactly this shape.
    let mut anon_ind_types: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut sub_ann_prop: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut sub_obj_prop: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut sub_data_prop: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // A named object property's edges to an inverse property, which is a node of
    // its own, written after the edges of the same kind to named properties:
    // the edge's rank among the property's axioms, its predicate, the property
    // the inverse names, and, for an annotated axiom, the node's id and its
    // definition when the definition follows this property's block.
    type InverseEdge = (u8, &'static str, String, Option<(String, String)>);
    let mut op_edge_inv: BTreeMap<String, Vec<InverseEdge>> = BTreeMap::new();
    // The reification blocks of annotated axioms over inverse properties, keyed
    // by the property whose graph holds them, each followed by the definitions
    // it names by id.
    let mut inv_roots: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // The ids already given out from the numbering pass's records, so two
    // axioms over the same terms take one each.
    let mut taken_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut inverse_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Domain/range values carry their axiom annotations so an annotated one can be
    // reified (`<owl:Axiom>` with `rdfs:domain`/`rdfs:range` as annotatedProperty),
    // which is how RO's IAO_0000116 editor notes on those axioms are written.
    type PropCe = (CE<RcStr>, Vec<(String, AnnotationValue<RcStr>)>);
    let mut op_domain: BTreeMap<String, Vec<PropCe>> = BTreeMap::new();
    let mut op_range: BTreeMap<String, Vec<PropCe>> = BTreeMap::new();
    // A DATA property's domain (a class expression) and range (a data range).
    // Without both, `nbo.owl`'s `COB_0000801` loses them on every round trip —
    // and, since a mirror IS a round trip, so does every module built from it.
    let mut dp_domain: BTreeMap<String, Vec<PropCe>> = BTreeMap::new();
    #[allow(clippy::type_complexity)]
    let mut dp_range: BTreeMap<String, Vec<(horned_owl::model::DataRange<RcStr>, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    // Annotation-property `rdfs:domain`/`rdfs:range` — the value is a bare IRI (a
    // class OR a datatype, e.g. `IAO_0006012 rdfs:range xsd:dateTime`), never a
    // class expression, so these are kept as plain resource IRIs.
    let mut ap_domain: BTreeMap<String, Vec<(String, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    let mut ap_range: BTreeMap<String, Vec<(String, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    let mut op_disjoint: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut op_equiv: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // An axiom whose SUBJECT is an inverse has no named subject to hang off, so it
    // is an anonymous block of its own, written straight after the block of the
    // property that inverse names — the key here: `mirror/stato.owl` carries
    // `EquivalentObjectProperties(inverse(IAO_0000235) inverse(STATO_0000205))`.
    //
    // Every such axiom has its own inverse node, so each is one block holding that
    // node's `owl:inverseOf` and the axiom's triple, in the order the property's
    // axioms take (keyed by axiom kind: equivalence, sub-property, the
    // characteristics, domain, range).
    let mut op_inv: BTreeMap<String, Vec<(u8, String)>> = BTreeMap::new();
    let mut dp_equiv: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dp_disjoint: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dp_char: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    // Per class, each key's collection items, its named members and the axiom's
    // annotations.
    #[allow(clippy::type_complexity)]
    let mut has_key: BTreeMap<String, Vec<(Vec<String>, String, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    type DataRangeAnns = (horned_owl::model::DataRange<RcStr>, Vec<(String, AnnotationValue<RcStr>)>);
    let mut datatype_defs: BTreeMap<String, Vec<DataRangeAnns>> = BTreeMap::new();
    // Keyed by source individual: (0 for an object assertion, 1 for a data one;
    // then the property and target), the block itself, and the axiom's
    // annotations, which order two assertions of the same terms.
    #[allow(clippy::type_complexity)]
    let mut neg_assertions: BTreeMap<String, Vec<(u8, String, String, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    // An n-ary `SameIndividual` or equivalence splits into CONSECUTIVE pairs, and
    // every pair is rendered in the FIRST member's graph. The first member keeps
    // its own pair in its block; each later member is a ROOT of that graph and
    // gets a block of its own straight after, carrying the pair whose subject it
    // is — so the last member's block is bare, and none of them says it again in
    // its own entity section.
    //
    // host -> (member, the member's lines in the host's graph), in member order.
    let mut root_blocks: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    // What the host's own block of such a chain (see `Chain`) does not hold:
    // the reifications of its later pairs, more anonymous roots of the host's
    // graph; the definitions of its members defined by id, each placed after the
    // first block to name it; the member of the host's own pair, nested with the
    // pair that member is the subject of, by host and member; and the edge the
    // definition of that member holds, by its id.
    let mut chain_roots: BTreeMap<String, String> = BTreeMap::new();
    let mut chain_defs: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut chain_nested: HashMap<(String, String), String> = HashMap::new();
    let mut chain_tail: HashMap<String, String> = HashMap::new();
    // The axiom nodes whose annotations' annotations have been stated.
    let mut stated_nested: HashSet<u64> = HashSet::new();
    let has_ontology_iri =
        model.ont.iter().any(|ac| matches!(&ac.component, Component::OntologyID(id) if id.iri.is_some()));
    #[allow(clippy::type_complexity)]
    let mut op_chains: BTreeMap<String, Vec<(Vec<OPE<RcStr>>, Vec<(String, AnnotationValue<RcStr>)>)>> =
        BTreeMap::new();
    let mut op_char: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    /// An annotated axiom whose base triple is an `rdf:type`: a `Declaration`, or a
    /// property characteristic. It reifies like any other annotated axiom, with
    /// `rdf:type` as the annotated property and the OWL type as the target.
    type TypeReif = (&'static str, Vec<(String, AnnotationValue<RcStr>)>);
    let mut type_reif: BTreeMap<String, Vec<TypeReif>> = BTreeMap::new();
    // An annotated axiom whose base triple joins two named terms: one
    // `owl:Axiom` block after its subject's, naming the predicate and the target.
    type EdgeReif = (&'static str, String, Vec<(String, AnnotationValue<RcStr>)>);
    let mut edge_reifs: BTreeMap<String, Vec<EdgeReif>> = BTreeMap::new();
    // A binary property axiom is stated of its first member in order.
    let ordered = |a: String, b: String| if iri_key(&a) <= iri_key(&b) { (a, b) } else { (b, a) };
    // Anonymous-subject "general axioms" (GCIs, anon disjoints, AllDifferent),
    // rendered last as a section sorted by full block text.
    let mut gci_blocks: Vec<(&AnnotatedComponent<RcStr>, String)> = Vec::new();
    // The genid pass walks the general axioms in `cmp_axiom` order and records each
    // annotated one's blank node in `shared_seq["__general__"]`, IN THAT ORDER. Two
    // annotated GCIs can have structurally-equal targets and still be two distinct
    // nodes, which a signature-keyed lookup collapses into one — UBERON's 130
    // general reifications need 124 distinct ids, not the 98 a signature key gives.
    // Consume the ordered list positionally instead, keyed by axiom identity.
    let general_gid: std::collections::HashMap<*const AnnotatedComponent<RcStr>, String> = {
        let mut gen: Vec<&AnnotatedComponent<RcStr>> = model
            .ont
            .iter()
            .filter(|ac| {
                !ac.ann.is_empty()
                    && matches!(&ac.component,
                        Component::SubClassOf(sc)
                            if !matches!(sc.sub, CE::Class(_)) && !matches!(sc.sup, CE::Class(_)))
            })
            .collect();
        gen.sort_by(|a, b| crate::io::genid::cmp_axiom(&a.component, &b.component));
        let seq = shared_seq.get("__general__");
        let mut out = std::collections::HashMap::new();
        let mut k = 0usize;
        for ac in gen {
            if let Some(v) = seq {
                if k < v.len() {
                    out.insert(ac as *const _, format!("genid{}", v[k].1));
                    k += 1;
                }
            }
        }
        out
    };
    let no_g = Genids::new();
    let op_name = |ope: &OPE<RcStr>| match ope {
        OPE::ObjectProperty(p) => Some(p.0.as_ref().to_string()),
        OPE::InverseObjectProperty(_) => None,
    };
    // The axioms of shapes this layout cannot state, in whole or with their
    // annotations. Any one of them stops the write before its first byte.
    let mut left_out: Vec<&AnnotatedComponent<RcStr>> = Vec::new();
    // Anonymous individuals are written from the model.
    let anon_model = model.ont.iter().any(|ac| crate::io::genid::names_anonymous(&ac.component));
    if anon_model {
        // The placeholders the statements are built with need the nodes first.
        let ids = AnonDoc { ids: genid_pass.anon_ids.clone(), ..Default::default() };
        ANON_DOC.with(|d| *d.borrow_mut() = Some(ids));
        let doc = build_anon_doc(model, &genid_pass, &prefixes, &mut left_out);
        ANON_DOC.with(|d| *d.borrow_mut() = Some(doc));
    }
    let annotated = |ac: &AnnotatedComponent<RcStr>| !ac.ann.is_empty();
    // An anonymous individual that is also an object somewhere is stated where it
    // is the object, with everything said about it nested there; this layout
    // gives each such statement a block of its own instead.
    let anon_objects = anonymous_objects(model);
    let nested_anon = |i: &str| anon_objects.contains(i);
    for ac in model.ont.iter() {
        // An annotation of an annotation is stated on the ontology, and on an
        // axiom the numbering pass numbered its nodes for.
        if !nested_annotations_stated(ac, &genid_pass.nested_axioms, anon_model) {
            left_out.push(ac);
        }
        // …and the ontology's are stated of its IRI, so an ontology with none
        // cannot carry them.
        if matches!(&ac.component, Component::OntologyAnnotation(oa) if !oa.0.ann.is_empty()) && !has_ontology_iri {
            left_out.push(ac);
        }
        match &ac.component {
            // An object property axiom with an inverse property in it.
            c if inverse_edge(c).is_some() => {
                let Some((subject, tag, pred, rank, object)) = inverse_edge(c) else { unreachable!() };
                match (subject, object) {
                    // A named subject's edge to an inverse: the inverse is nested
                    // in the edge, or, when the axiom is annotated, named by id.
                    // The axiom is numbered in the graph of the first of the two
                    // properties, which holds the reification; a sub-property axiom
                    // in its sub-property's.
                    (OPE::ObjectProperty(a), EdgeObject::Property(OPE::InverseObjectProperty(b))) => {
                        let (a, b) = (a.0.as_ref().to_string(), b.0.as_ref().to_string());
                        let mut node = None;
                        if annotated(ac) {
                            let owner = if pred == P_SUB_PROPERTY || iri_key(&a) <= iri_key(&b) { &a } else { &b };
                            match seq_id(shared_seq.get(owner), &format!("INV\u{1}{pred}\u{1}{b}"), &mut taken_ids) {
                                Some(id) => {
                                    let gid = format!("genid{id}");
                                    let def = inverse_node_def(&gid, &b);
                                    let target = format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n");
                                    let reif = edge_reif(&a, pred, &target, &ax_anns(ac), &prefixes);
                                    // The definition follows the first block to name the
                                    // node: the subject's own, or the reification when
                                    // that comes first.
                                    if *owner == a {
                                        inv_roots.entry(owner.clone()).or_default().push(reif);
                                        node = Some((gid, def));
                                    } else {
                                        inv_roots.entry(owner.clone()).or_default().push(format!("{reif}{def}"));
                                        node = Some((gid, String::new()));
                                    }
                                }
                                None => left_out.push(ac),
                            }
                        }
                        op_edge_inv.entry(a).or_default().push((rank, tag, b, node));
                    }
                    // An inverse subject is a node of its own: unannotated, a block
                    // of its own after the property it names; annotated, the nested
                    // source of the reification, with an object that is not named
                    // given an id and defined after it.
                    (OPE::InverseObjectProperty(p), object) => {
                        let p = p.0.as_ref().to_string();
                        if !annotated(ac) {
                            let part = match object {
                                EdgeObject::Property(OPE::ObjectProperty(o)) => {
                                    format!("        <{tag} rdf:resource=\"{}\"/>\n", esc_attr(o.0.as_ref()))
                                }
                                EdgeObject::Property(OPE::InverseObjectProperty(o)) => format!(
                                    "        <{tag}>\n            <rdf:Description>\n                <owl:inverseOf rdf:resource=\"{}\"/>\n            </rdf:Description>\n        </{tag}>\n",
                                    esc_attr(o.0.as_ref())
                                ),
                                EdgeObject::Type(t) => format!("        <{tag} rdf:resource=\"{t}\"/>\n"),
                                EdgeObject::Class(ce) => render_prop_ce(tag, ce, &no_g),
                            };
                            op_inv.entry(p).or_default().push((rank, part));
                        } else {
                            let named = match object {
                                EdgeObject::Property(OPE::ObjectProperty(o)) => Some(o.0.as_ref().to_string()),
                                EdgeObject::Type(t) => Some(t.to_string()),
                                EdgeObject::Class(CE::Class(c)) => Some(c.0.as_ref().to_string()),
                                _ => None,
                            };
                            let parts = match (named, object) {
                                (Some(o), _) => Some((
                                    format!("                <{tag} rdf:resource=\"{}\"/>\n", esc_attr(&o)),
                                    format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(&o)),
                                    String::new(),
                                )),
                                (None, EdgeObject::Property(OPE::InverseObjectProperty(o))) => seq_id(
                                    shared_seq.get(&p),
                                    &format!("INV\u{1}{pred}\u{1}{}", o.0.as_ref()),
                                    &mut taken_ids,
                                )
                                .map(|id| {
                                    let gid = format!("genid{id}");
                                    let def = inverse_node_def(&gid, o.0.as_ref());
                                    (gid, def)
                                })
                                .map(|(gid, def)| {
                                    (
                                        format!("                <{tag} rdf:nodeID=\"{gid}\"/>\n"),
                                        format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n"),
                                        def,
                                    )
                                }),
                                (None, EdgeObject::Class(ce)) if !inline_anon() => {
                                    seq_id(shared_seq.get(&p), &crate::io::genid::ce_sig(ce), &mut taken_ids).map(|id| {
                                        let gid = format!("genid{id}");
                                        (
                                            format!("                <{tag} rdf:nodeID=\"{gid}\"/>\n"),
                                            format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n"),
                                            inject_nodeid(&render_ce(ce, 4, &no_g), &gid),
                                        )
                                    })
                                }
                                _ => None,
                            };
                            match parts {
                                Some((triple, target, def)) => {
                                    let reif = inverse_source_reif(&p, &triple, pred, &target, &ax_anns(ac), &prefixes);
                                    inv_roots.entry(p).or_default().push(format!("{reif}{def}"));
                                }
                                None => left_out.push(ac),
                            }
                        }
                    }
                    // `inverse_edge` names no other shape.
                    _ => left_out.push(ac),
                }
            }
            Component::DeclareAnnotationProperty(d) => {
                let iri = d.0 .0.as_ref().to_string();
                if !ac.ann.is_empty() {
                    type_reif.entry(iri.clone()).or_default().push(("http://www.w3.org/2002/07/owl#AnnotationProperty", ax_anns(ac)));
                }
                ann_props.push(iri);
            }
            Component::DeclareDatatype(d) => {
                let iri = d.0 .0.as_ref().to_string();
                if !ac.ann.is_empty() {
                    type_reif.entry(iri.clone()).or_default().push(("http://www.w3.org/2000/01/rdf-schema#Datatype", ax_anns(ac)));
                }
                datatypes.push(iri);
            }
            Component::DeclareObjectProperty(d) => {
                let iri = d.0 .0.as_ref().to_string();
                if !ac.ann.is_empty() {
                    type_reif.entry(iri.clone()).or_default().push(("http://www.w3.org/2002/07/owl#ObjectProperty", ax_anns(ac)));
                }
                obj_props.push(iri);
            }
            Component::DeclareDataProperty(d) => {
                let iri = d.0 .0.as_ref().to_string();
                if !ac.ann.is_empty() {
                    type_reif.entry(iri.clone()).or_default().push(("http://www.w3.org/2002/07/owl#DatatypeProperty", ax_anns(ac)));
                }
                data_props.push(iri);
            }
            Component::DeclareClass(d) => {
                let iri = d.0 .0.as_ref().to_string();
                if !ac.ann.is_empty() {
                    type_reif.entry(iri.clone()).or_default().push(("http://www.w3.org/2002/07/owl#Class", ax_anns(ac)));
                }
                classes.push(iri);
            }
            Component::DeclareNamedIndividual(d) => {
                let iri = d.0 .0.as_ref().to_string();
                if !ac.ann.is_empty() {
                    type_reif.entry(iri.clone()).or_default().push(("http://www.w3.org/2002/07/owl#NamedIndividual", ax_anns(ac)));
                }
                individuals.push(iri);
            }
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    let anns: Vec<(String, AnnotationValue<RcStr>)> =
                        ac.ann.iter().map(|a| (a.ap.0.as_ref().to_string(), a.av.clone())).collect();
                    sub_class.entry(sub.0.as_ref().to_string()).or_default().push((sc.sup.clone(), anns));
                } else if ac.ann.is_empty() {
                    // Anonymous subclass → general class axiom.
                    gci_blocks.push((ac, render_gci_subclass(&sc.sub, &sc.sup, &no_g)));
                } else {
                    let anns: Vec<(String, AnnotationValue<RcStr>)> =
                        ac.ann.iter().map(|x| (x.ap.0.as_ref().to_string(), x.av.clone())).collect();
                    let gid = general_gid.get(&(ac as *const _)).cloned();
                    gci_blocks.push((
                        ac,
                        render_gci_subclass_annotated(
                            &sc.sub,
                            &sc.sup,
                            shared_genids,
                            &anns,
                            &prefixes,
                            gid,
                        ),
                    ));
                }
            }
            Component::DisjointClasses(dc) => {
                if dc.0.len() == 2 {
                    match (&dc.0[0], &dc.0[1]) {
                        (CE::Class(a), CE::Class(b)) => {
                            disjoint_class
                                .entry(a.0.as_ref().to_string())
                                .or_default()
                                .push(dc.0[1].clone());
                            if !ac.ann.is_empty() {
                                let anns: Vec<(String, AnnotationValue<RcStr>)> =
                                    ac.ann.iter().map(|x| (x.ap.0.as_ref().to_string(), x.av.clone())).collect();
                                disjoint_reif
                                    .entry(a.0.as_ref().to_string())
                                    .or_default()
                                    .push((a.0.as_ref().to_string(), b.0.as_ref().to_string(), anns.clone()));
                                if inline_anon() {
                                    disjoint_reif
                                        .entry(b.0.as_ref().to_string())
                                        .or_default()
                                        .push((a.0.as_ref().to_string(), b.0.as_ref().to_string(), anns));
                                }
                            }
                        }
                        // C disjointWith (anonymous) → rendered under the named class,
                        // since a 2-way disjoint belongs to its first (named)
                        // expression. Both-anonymous stays a general axiom.
                        (CE::Class(a), other) | (other, CE::Class(a)) => {
                            disjoint_class
                                .entry(a.0.as_ref().to_string())
                                .or_default()
                                .push(other.clone());
                            // An ANNOTATED one reifies, so its anonymous target is
                            // referenced twice — by the `owl:disjointWith` edge and
                            // by `owl:annotatedTarget` — and becomes a shared node
                            // rather than an inline block.
                            if !ac.ann.is_empty() {
                                let anns: Vec<(String, AnnotationValue<RcStr>)> = ac
                                    .ann
                                    .iter()
                                    .map(|x| (x.ap.0.as_ref().to_string(), x.av.clone()))
                                    .collect();
                                disjoint_anon
                                    .entry(a.0.as_ref().to_string())
                                    .or_default()
                                    .push((other.clone(), anns));
                            }
                        }
                        _ => {
                            if ac.ann.is_empty() {
                                gci_blocks
                                    .push((ac, render_gci_disjoint(&dc.0[0], &dc.0[1], &no_g)));
                            } else {
                                let anns: Vec<(String, AnnotationValue<RcStr>)> = ac
                                    .ann
                                    .iter()
                                    .map(|x| (x.ap.0.as_ref().to_string(), x.av.clone()))
                                    .collect();
                                gci_blocks.push((
                                    ac,
                                    render_gci_disjoint_annotated(
                                        &dc.0,
                                        shared_genids,
                                        &anns,
                                        &prefixes,
                                    ),
                                ));
                            }
                        }
                    }
                } else {
                    // 3+ members: an nary disjoint renders as an
                    // `owl:AllDisjointClasses` general axiom.
                    gci_blocks.push((ac, render_all_disjoint(&dc.0, &no_g, &node_annotations(ac, &prefixes))));
                }
            }
            Component::DisjointUnion(du) => {
                let mut members: Vec<&CE<RcStr>> = du.1.iter().collect();
                members.sort_by(|a, b| match (a, b) {
                    (CE::Class(x), CE::Class(y)) => iri_key(x.0.as_ref()).cmp(&iri_key(y.0.as_ref())),
                    _ => crate::io::owlfunc::cmp_ce(a, b),
                });
                members.dedup();
                let items: Vec<String> = members
                    .iter()
                    .map(|m| match m {
                        CE::Class(c) => format!("            <rdf:Description rdf:about=\"{}\"/>\n", esc_attr(c.0.as_ref())),
                        _ => render_ce(m, 12, &no_g),
                    })
                    .collect();
                let named = list_members(members.iter().filter_map(|m| match m {
                    CE::Class(c) => Some(c.0.as_ref()),
                    _ => None,
                }));
                disjoint_union.entry(du.0 .0.as_ref().to_string()).or_default().push((items, named, ax_anns(ac)));
            }
            Component::DifferentIndividuals(di) => {
                let members = if anon_model { slot_members(&di.0) } else { sorted_members(&di.0) };
                let slot = |m: &Option<String>| m.as_ref().is_some_and(|x| x.starts_with(ANON_SLOT));
                match (members.len(), members.first()) {
                    // A pair of anonymous individuals is stated where the first
                    // of them is.
                    (2, Some(first)) if slot(first) => {}
                    // …and a pair whose first member is named becomes one edge
                    // on it, its reification a root of its graph.
                    (2, Some(Some(subject))) => {
                        if annotated(ac) && !slot(&members[1]) {
                            match &members[1] {
                                Some(object) => edge_reifs.entry(subject.clone()).or_default().push((
                                    "http://www.w3.org/2002/07/owl#differentFrom",
                                    object.clone(),
                                    ax_anns(ac),
                                )),
                                None => left_out.push(ac),
                            }
                        }
                        ind_identity
                            .entry(subject.clone())
                            .or_default()
                            .push(("owl:differentFrom", members[1].clone()))
                    }
                    (2, _) => left_out.push(ac),
                    _ => {
                        if members.iter().any(Option::is_none) {
                            left_out.push(ac);
                        }
                        let named: Vec<String> = members.into_iter().flatten().collect();
                        let identity = crate::io::genid::axiom_identity(ac);
                        let own_graph = format!("{}{identity}", crate::io::genid::GENERAL_GRAPH);
                        let elsewhere = anon_model && genid_pass.anon_home.get(&identity).is_some_and(|g| *g != own_graph);
                        if !named.is_empty() && !elsewhere {
                            gci_blocks.push((ac, render_all_different(&named, &node_annotations(ac, &prefixes))));
                        }
                    }
                }
            }
            Component::SameIndividual(si) => {
                // A binary axiom stays whole; a longer one becomes the CONSECUTIVE
                // pairs of its (sorted) member list.
                let members = sorted_members(&si.0);
                // An annotated pair of named members reifies its one edge, and
                // a longer sameness of named members each of its pairs.
                let all_named = members.iter().all(Option::is_some);
                let named_pair = members.len() == 2 && all_named;
                // A pair with an anonymous member is the anonymous individuals':
                // stated where its first member is, an edge on it when that is
                // named.
                if anon_model && !all_named {
                    let members = slot_members(&si.0);
                    let Some(Some(host)) = members.first().filter(|m| m.as_ref().is_some_and(|h| !h.starts_with(ANON_SLOT)))
                    else {
                        continue;
                    };
                    ind_identity.entry(host.clone()).or_default().push(("owl:sameAs", members[1].clone()));
                    // Every other named member is a root block of the host's
                    // graph holding the pair it is the subject of.
                    for (i, m) in members.iter().enumerate().skip(1) {
                        let Some(m) = m.as_ref().filter(|m| !m.starts_with(ANON_SLOT)) else { continue };
                        let body = match members.get(i + 1).and_then(|n| n.clone()) {
                            Some(next) => match next.strip_prefix(ANON_SLOT) {
                                Some(x) => anon_slot("owl:sameAs", x, "        "),
                                None => format!("        <owl:sameAs rdf:resource=\"{}\"/>\n", esc_attr(&next)),
                            },
                            None => String::new(),
                        };
                        root_blocks.entry(host.clone()).or_default().push((m.clone(), body));
                    }
                    continue;
                }
                if (annotated(ac) && !all_named)
                    || members.first().is_some_and(Option::is_none)
                    || (members.len() > 2 && members.iter().any(Option::is_none))
                {
                    left_out.push(ac);
                }
                if members.len() > 2 && all_named {
                    let named: Vec<String> = members.iter().flatten().cloned().collect();
                    let host = named[0].clone();
                    if annotated(ac) {
                        edge_reifs.entry(host.clone()).or_default().push((
                            "http://www.w3.org/2002/07/owl#sameAs",
                            named[1].clone(),
                            ax_anns(ac),
                        ));
                    }
                    ind_identity.entry(host.clone()).or_default().push(("owl:sameAs", Some(named[1].clone())));
                    let chain = Chain {
                        tag: "owl:sameAs",
                        pred: "http://www.w3.org/2002/07/owl#sameAs",
                        members: named.iter().map(|m| ChainMember::Named(m.clone())).collect(),
                        ids: vec![None; named.len()],
                        anns: ax_anns(ac),
                    };
                    chain.place(&host, 1, &prefixes, &mut root_blocks, &mut chain_roots, &mut chain_defs);
                } else {
                    if annotated(ac) && named_pair {
                        if let (Some(subject), Some(object)) = (&members[0], &members[1]) {
                            edge_reifs.entry(subject.clone()).or_default().push((
                                "http://www.w3.org/2002/07/owl#sameAs",
                                object.clone(),
                                ax_anns(ac),
                            ));
                        }
                    }
                    // The first pair is the host's own; the rest belong to the root
                    // blocks that follow it.
                    if let Some(subject) = members.first().and_then(|m| m.clone()) {
                        if let Some(pair) = members.windows(2).next() {
                            ind_identity
                                .entry(subject.clone())
                                .or_default()
                                .push(("owl:sameAs", pair[1].clone()));
                        }
                        for (i, m) in members.iter().enumerate().skip(1) {
                            let Some(m) = m else { continue };
                            let body = match members.get(i + 1).and_then(|n| n.clone()) {
                                Some(next) => format!(
                                    "        <owl:sameAs rdf:resource=\"{}\"/>\n",
                                    esc_attr(&next)
                                ),
                                None => String::new(),
                            };
                            root_blocks.entry(subject.clone()).or_default().push((m.clone(), body));
                        }
                    }
                }
            }
            Component::EquivalentClasses(eq) => {
                // Three or more members are a chain of consecutive pairs in the
                // graph of the first member when it is named, and a general axiom
                // when none is.
                let members = crate::io::genid::ordered_ces(&eq.0);
                if members.len() > 2 {
                    let anns = ax_anns(ac);
                    let host = match members[0] {
                        CE::Class(c) => Some(c.0.as_ref().to_string()),
                        _ => None,
                    };
                    let owner = host.clone().unwrap_or_else(|| "__general__".to_string());
                    // The numbering pass recorded every anonymous member's id, in
                    // order; the first member is never defined by id.
                    let ids: Vec<Option<String>> = members
                        .iter()
                        .enumerate()
                        .map(|(i, m)| {
                            if matches!(m, CE::Class(_)) || anns.is_empty() || inline_anon() {
                                return None;
                            }
                            let sig = format!("NARY\u{1}{}", crate::io::genid::ce_sig(m));
                            let id = seq_id(shared_seq.get(&owner), &sig, &mut taken_ids);
                            id.filter(|_| i > 0).map(|g| format!("genid{g}"))
                        })
                        .collect();
                    let chain = Chain {
                        tag: "owl:equivalentClass",
                        pred: EQUIV_PROP,
                        members: members
                            .iter()
                            .map(|m| match m {
                                CE::Class(c) => ChainMember::Named(c.0.as_ref().to_string()),
                                ce => ChainMember::Class(ce),
                            })
                            .collect(),
                        ids,
                        anns: anns.clone(),
                    };
                    match host {
                        // The host's own pair is an equivalence of its block.
                        Some(host) => {
                            if !matches!(members[1], CE::Class(_)) {
                                match chain.ids[1].clone() {
                                    Some(g) => {
                                        chain_tail.insert(g, chain.edge(1, 8));
                                    }
                                    None => {
                                        chain_nested.insert((host.clone(), ce_sig(members[1])), chain.nested(1, 12));
                                    }
                                }
                            }
                            equiv_class.entry(host.clone()).or_default().push((members[1].clone(), anns));
                            chain.place(&host, 1, &prefixes, &mut root_blocks, &mut chain_roots, &mut chain_defs);
                        }
                        None => {
                            let block = if anns.is_empty() {
                                chain.nested(0, 4)
                            } else {
                                let reifs: String = (0..members.len() - 1).map(|i| chain.reif(i, &prefixes)).collect();
                                let (roots, mut defs) = order_roots(
                                    "",
                                    &reifs,
                                    reif_genids.get("__general__"),
                                    &genid_pass.nested,
                                    &mut stated_nested,
                                    &prefixes,
                                );
                                defs.extend((1..members.len()).filter_map(|i| chain.def(i)));
                                place_defs(&roots, Some(&defs))
                            };
                            gci_blocks.push((ac, block));
                        }
                    }
                }
                // Binary `A ≡ expr` with a named A → render on A.
                if eq.0.len() == 2 {
                    let anns: Vec<(String, AnnotationValue<RcStr>)> =
                        ac.ann.iter().map(|a| (a.ap.0.as_ref().to_string(), a.av.clone())).collect();
                    if let CE::Class(a) = &eq.0[0] {
                        equiv_class.entry(a.0.as_ref().to_string()).or_default().push((eq.0[1].clone(), anns));
                    } else if let CE::Class(b) = &eq.0[1] {
                        equiv_class.entry(b.0.as_ref().to_string()).or_default().push((eq.0[0].clone(), anns));
                    } else if ac.ann.is_empty() {
                        // Both anonymous: a general axiom, with no entity to host it.
                        gci_blocks.push((ac, render_gci_equivalent(&eq.0[0], &eq.0[1], &no_g)));
                    } else {
                        // …and an ANNOTATED one reifies, which is the only form
                        // with somewhere to put the annotation.
                        gci_blocks.push((
                            ac,
                            render_gci_equivalent_annotated(&eq.0, shared_genids, &anns, &prefixes),
                        ));
                    }
                }
            }
            Component::SubAnnotationPropertyOf(s) => {
                let (sub, sup) = (s.sub.0.as_ref().to_string(), s.sup.0.as_ref().to_string());
                if annotated(ac) {
                    edge_reifs.entry(sub.clone()).or_default().push((P_SUB_PROPERTY, sup.clone(), ax_anns(ac)));
                }
                sub_ann_prop.entry(sub).or_default().push(sup);
            }
            Component::SubObjectPropertyOf(s) => match &s.sub {
                horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(sub) => {
                    if let (Some(sub), Some(sup)) = (op_name(sub), op_name(&s.sup)) {
                        if annotated(ac) {
                            edge_reifs.entry(sub.clone()).or_default().push((P_SUB_PROPERTY, sup.clone(), ax_anns(ac)));
                        }
                        sub_obj_prop.entry(sub).or_default().push(sup)
                    }
                }
                horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(chain) => match &s.sup {
                    OPE::ObjectProperty(sup) => {
                        let links: Vec<OPE<RcStr>> = chain.clone();
                        let anns: Vec<(String, AnnotationValue<RcStr>)> =
                            ac.ann.iter().map(|a| (a.ap.0.as_ref().to_string(), a.av.clone())).collect();
                        op_chains.entry(sup.0.as_ref().to_string()).or_default().push((links, anns));
                    }
                    // A chain whose super-property is an inverse is stated of that
                    // inverse's node: a block of its own, or, annotated, the nested
                    // source of the chain's reification, whose list Turtle names
                    // by id.
                    OPE::InverseObjectProperty(sup) => {
                        let owner = sup.0.as_ref().to_string();
                        if annotated(ac) {
                            let pred = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
                            let items: Vec<String> = chain.iter().map(render_chain_link).collect();
                            let members = list_members(chain.iter().filter_map(|link| match link {
                                OPE::ObjectProperty(p) => Some(p.0.as_ref()),
                                OPE::InverseObjectProperty(_) => None,
                            }));
                            let gid = named_lists()
                                .then(|| seq_id(shared_seq.get(&owner), &format!("LIST\u{1}{pred}\u{1}{members}"), &mut taken_ids))
                                .flatten()
                                .map(|id| format!("genid{id}"));
                            let mut def = String::new();
                            let (edge, target) = list_slots("owl:propertyChainAxiom", &items, gid.as_deref(), &mut def);
                            let triple: String = edge.split_inclusive('\n').map(|line| format!("        {line}")).collect();
                            let reif = inverse_source_reif(&owner, &triple, pred, &target, &ax_anns(ac), &prefixes);
                            inv_roots.entry(owner).or_default().push(format!("{reif}{def}"));
                        } else {
                            let links: String = chain.iter().map(render_chain_link).collect();
                            op_inv.entry(owner).or_default().push((
                                25,
                                format!(
                                    "        <owl:propertyChainAxiom rdf:parseType=\"Collection\">\n{links}        </owl:propertyChainAxiom>\n"
                                ),
                            ));
                        }
                    }
                },
            },
            Component::InverseObjectProperties(iop) => {
                if let (Some(a), Some(b)) = (op_name(&iop.0), op_name(&iop.1)) {
                    let (a, b) = ordered(a, b);
                    if annotated(ac) {
                        edge_reifs.entry(a.clone()).or_default().push((P_INVERSE_OF, b.clone(), ax_anns(ac)));
                    }
                    inverse_of.entry(a).or_default().push(b);
                }
            }
            Component::ClassAssertion(ca) => {
                if match &ca.i {
                    Individual::Named(_) => false,
                    Individual::Anonymous(a) => !anon_model && (annotated(ac) || nested_anon(a.0.as_ref())),
                } {
                    left_out.push(ac);
                }
                match (&ca.i, &ca.ce) {
                    (Individual::Anonymous(_), _) if anon_model => {}
                    (Individual::Named(i), CE::Class(c)) => {
                        let (i, c) = (i.0.as_ref().to_string(), c.0.as_ref().to_string());
                        if annotated(ac) {
                            edge_reifs.entry(i.clone()).or_default().push((P_RDF_TYPE, c.clone(), ax_anns(ac)));
                        }
                        ind_types.entry(i).or_default().push(c)
                    }
                    (Individual::Named(i), ce) => ind_types_anon
                        .entry(i.0.as_ref().to_string())
                        .or_default()
                        .push((ce.clone(), ax_anns(ac))),
                    (Individual::Anonymous(a), CE::Class(c)) => anon_ind_types
                        .entry(a.0.as_ref().to_string())
                        .or_default()
                        .push(c.0.as_ref().to_string()),
                    _ => left_out.push(ac),
                }
            }
            // A property assertion on a named individual renders as a child of its
            // `<owl:NamedIndividual>`, between the `rdf:type`s and the annotations:
            // OBI's software-module individuals carry `IAO_0000136` ('is about')
            // this way.
            Component::ObjectPropertyAssertion(opa) => {
                // An assertion on an inverse is stated of the named property, the
                // other way round, and its annotations reify that statement, a
                // node numbered after every other (`genid::inverse_assertion`).
                let (p, from, to) = match &opa.ope {
                    OPE::ObjectProperty(p) => (p, &opa.from, &opa.to),
                    OPE::InverseObjectProperty(p) => (p, &opa.to, &opa.from),
                };
                if !anon_model && !matches!((from, to), (Individual::Named(_), Individual::Named(_))) {
                    left_out.push(ac);
                }
                // A named individual's assertion of an anonymous one, the
                // object written where the individual's graph writes it.
                if let (true, Individual::Named(s), Individual::Anonymous(o)) = (anon_model, from, to) {
                    ind_props.entry(s.0.as_ref().to_string()).or_default().push((
                        p.0.as_ref().to_string(),
                        o.0.as_ref().to_string(),
                        Some(ANON_SLOT.to_string()),
                    ));
                }
                if let (Individual::Named(s), Individual::Named(o)) = (from, to) {
                    ind_props.entry(s.0.as_ref().to_string()).or_default().push((
                        p.0.as_ref().to_string(),
                        o.0.as_ref().to_string(),
                        None,
                    ));
                    if !ac.ann.is_empty() {
                        prop_reif.entry(s.0.as_ref().to_string()).or_default().push((
                            p.0.as_ref().to_string(),
                            o.0.as_ref().to_string(),
                            None,
                            ax_anns(ac),
                            matches!(opa.ope, OPE::InverseObjectProperty(_)),
                        ));
                    }
                }
            }
            Component::DataPropertyAssertion(dpa) => {
                if !anon_model && !matches!(dpa.from, Individual::Named(_)) {
                    left_out.push(ac);
                }
                if let Individual::Named(s) = &dpa.from {
                    let attrs = match &dpa.to {
                        Literal::Simple { .. } => String::new(),
                        Literal::Language { lang, .. } => format!(" xml:lang=\"{lang}\""),
                        Literal::Datatype { datatype_iri, .. } => {
                            if datatype_iri.as_ref() == XSD_STRING {
                                String::new()
                            } else {
                                format!(" rdf:datatype=\"{}\"", esc_attr(datatype_iri.as_ref()))
                            }
                        }
                    };
                    ind_props.entry(s.0.as_ref().to_string()).or_default().push((
                        dpa.dp.0.as_ref().to_string(),
                        dpa.to.literal().clone(),
                        Some(attrs.clone()),
                    ));
                    if !ac.ann.is_empty() {
                        prop_reif.entry(s.0.as_ref().to_string()).or_default().push((
                            dpa.dp.0.as_ref().to_string(),
                            dpa.to.literal().clone(),
                            Some(attrs),
                            ax_anns(ac),
                            false,
                        ));
                    }
                }
            }
            Component::ObjectPropertyDomain(d) => {
                if let OPE::ObjectProperty(p) = &d.ope {
                    op_domain.entry(p.0.as_ref().to_string()).or_default().push((d.ce.clone(), ax_anns(ac)))
                }
            }
            Component::ObjectPropertyRange(r) => {
                if let OPE::ObjectProperty(p) = &r.ope {
                    op_range.entry(p.0.as_ref().to_string()).or_default().push((r.ce.clone(), ax_anns(ac)))
                }
            }
            Component::SubDataPropertyOf(sp) => {
                let (sub, sup) = (sp.sub.0.as_ref().to_string(), sp.sup.0.as_ref().to_string());
                if annotated(ac) {
                    edge_reifs.entry(sub.clone()).or_default().push((P_SUB_PROPERTY, sup.clone(), ax_anns(ac)));
                }
                sub_data_prop.entry(sub).or_default().push(sup);
            }
            Component::DataPropertyDomain(d) => {
                dp_domain
                    .entry(d.dp.0.as_ref().to_string())
                    .or_default()
                    .push((d.ce.clone(), ax_anns(ac)));
            }
            Component::DataPropertyRange(r) => {
                dp_range
                    .entry(r.dp.0.as_ref().to_string())
                    .or_default()
                    .push((r.dr.clone(), ax_anns(ac)));
            }
            Component::AnnotationPropertyDomain(d) => {
                ap_domain
                    .entry(d.ap.0.as_ref().to_string())
                    .or_default()
                    .push((d.iri.as_ref().to_string(), ax_anns(ac)));
            }
            Component::AnnotationPropertyRange(r) => {
                ap_range
                    .entry(r.ap.0.as_ref().to_string())
                    .or_default()
                    .push((r.iri.as_ref().to_string(), ax_anns(ac)));
            }
            // Two equivalent properties render as `owl:equivalentProperty` on the
            // FIRST of them; a longer chain is left alone, as a longer disjoint
            // set is, because it renders as pairs across several blocks.
            // A negative assertion is its own anonymous node, written after the
            // block of the individual it is about: object assertions first, then
            // data ones, each by property and then by target.
            // Its annotations are statements of that node, after its terms.
            Component::NegativeObjectPropertyAssertion(n) => {
                if !anon_model && !matches!((&n.from, &n.to), (Individual::Named(_), Individual::Named(_))) {
                    left_out.push(ac);
                }
                if let (Individual::Named(src), Individual::Named(tgt)) = (&n.from, &n.to) {
                    // An inverse property is a node of its own, nested in the slot.
                    let (key, prop) = match &n.ope {
                        OPE::ObjectProperty(p) => (
                            p.0.as_ref().to_string(),
                            format!("        <owl:assertionProperty rdf:resource=\"{}\"/>\n", esc_attr(p.0.as_ref())),
                        ),
                        OPE::InverseObjectProperty(p) => (
                            format!("\u{1}{}", p.0.as_ref()),
                            format!(
                                "        <owl:assertionProperty>\n            <rdf:Description>\n                <owl:inverseOf rdf:resource=\"{}\"/>\n            </rdf:Description>\n        </owl:assertionProperty>\n",
                                esc_attr(p.0.as_ref())
                            ),
                        ),
                    };
                    neg_assertions.entry(src.0.as_ref().to_string()).or_default().push((
                        0,
                        format!("{key}\u{0}{}", tgt.0.as_ref()),
                        format!(
                            "    <rdf:Description>\n        <rdf:type rdf:resource=\"{NEG_PA}\"/>\n        <owl:sourceIndividual rdf:resource=\"{}\"/>\n{prop}        <owl:targetIndividual rdf:resource=\"{}\"/>\n{}    </rdf:Description>\n",
                            esc_attr(src.0.as_ref()),
                            esc_attr(tgt.0.as_ref()),
                            node_annotations(ac, &prefixes)
                        ),
                        sorted_anns(ac),
                    ));
                }
            }
            Component::NegativeDataPropertyAssertion(n) => {
                if !anon_model && !matches!(n.from, Individual::Named(_)) {
                    left_out.push(ac);
                }
                if let Individual::Named(src) = &n.from {
                    neg_assertions.entry(src.0.as_ref().to_string()).or_default().push((
                        1,
                        format!("{}\u{0}{}", n.dp.0.as_ref(), n.to.literal()),
                        format!(
                            "    <rdf:Description>\n        <rdf:type rdf:resource=\"{NEG_PA}\"/>\n        <owl:sourceIndividual rdf:resource=\"{}\"/>\n        <owl:assertionProperty rdf:resource=\"{}\"/>\n{}{}    </rdf:Description>\n",
                            esc_attr(src.0.as_ref()),
                            esc_attr(n.dp.0.as_ref()),
                            render_literal_tag("owl:targetValue", &n.to, 8),
                            node_annotations(ac, &prefixes)
                        ),
                        sorted_anns(ac),
                    ));
                }
            }
            Component::EquivalentObjectProperties(e) => {
                let members = crate::io::genid::ordered_opes(&e.0);
                if members.len() > 2 {
                    // A chain of consecutive pairs in the graph of the first
                    // property block to state it: its first member's, when that is
                    // named, or an inverse member's property's, when that comes
                    // first.
                    let anns = ax_anns(ac);
                    let host = crate::io::genid::nary_ope_owner(&e.0).unwrap_or_default();
                    let ids: Vec<Option<String>> = members
                        .iter()
                        .enumerate()
                        .map(|(i, m)| match m {
                            OPE::InverseObjectProperty(p) if !anns.is_empty() => {
                                let sig = format!("NARY\u{1}INV\u{1}{}", p.0.as_ref());
                                let id = seq_id(shared_seq.get(&host), &sig, &mut taken_ids);
                                id.filter(|_| i > 0).map(|g| format!("genid{g}"))
                            }
                            _ => None,
                        })
                        .collect();
                    let chain = Chain {
                        tag: "owl:equivalentProperty",
                        pred: P_EQUIV_PROPERTY,
                        members: members
                            .iter()
                            .map(|m| match m {
                                OPE::ObjectProperty(p) => ChainMember::Named(p.0.as_ref().to_string()),
                                OPE::InverseObjectProperty(p) => ChainMember::Inverse(p.0.as_ref().to_string()),
                            })
                            .collect(),
                        ids,
                        anns: anns.clone(),
                    };
                    match (&chain.members[0], &chain.members[1]) {
                        // The host's own pair is an edge of its block.
                        (ChainMember::Named(first), ChainMember::Named(second)) if *first == host => {
                            if !anns.is_empty() {
                                edge_reifs.entry(host.clone()).or_default().push((P_EQUIV_PROPERTY, second.clone(), anns.clone()));
                            }
                            op_equiv.entry(host.clone()).or_default().push(second.clone());
                            chain.place(&host, 1, &prefixes, &mut root_blocks, &mut chain_roots, &mut chain_defs);
                        }
                        (ChainMember::Named(first), ChainMember::Inverse(second)) if *first == host => {
                            let node = chain.def(1);
                            match node {
                                Some(_) => chain_roots.entry(host.clone()).or_default().push_str(&chain.reif(0, &prefixes)),
                                None => {
                                    chain_nested.insert((host.clone(), format!("INV\u{1}{second}")), chain.nested(1, 12));
                                }
                            }
                            op_edge_inv.entry(host.clone()).or_default().push((12, "owl:equivalentProperty", second.clone(), node));
                            chain.place(&host, 1, &prefixes, &mut root_blocks, &mut chain_roots, &mut chain_defs);
                        }
                        // A host that is no member holds none of the pairs.
                        _ => chain.place(&host, 0, &prefixes, &mut root_blocks, &mut chain_roots, &mut chain_defs),
                    }
                } else if let (Some(a), Some(b)) = (op_name(&e.0[0]), op_name(&e.0[1])) {
                    let (a, b) = ordered(a, b);
                    if annotated(ac) {
                        edge_reifs.entry(a.clone()).or_default().push((P_EQUIV_PROPERTY, b.clone(), ax_anns(ac)));
                    }
                    op_equiv.entry(a).or_default().push(b);
                }
            }
            Component::EquivalentDataProperties(e) => {
                let mut members: Vec<String> = e.0.iter().map(|d| d.0.as_ref().to_string()).collect();
                members.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
                members.dedup();
                if members.len() > 2 {
                    // A chain of consecutive pairs in its first member's graph.
                    let anns = ax_anns(ac);
                    let host = members[0].clone();
                    if !anns.is_empty() {
                        edge_reifs.entry(host.clone()).or_default().push((P_EQUIV_PROPERTY, members[1].clone(), anns.clone()));
                    }
                    dp_equiv.entry(host.clone()).or_default().push(members[1].clone());
                    let chain = Chain {
                        tag: "owl:equivalentProperty",
                        pred: P_EQUIV_PROPERTY,
                        members: members.iter().map(|m| ChainMember::Named(m.clone())).collect(),
                        ids: vec![None; members.len()],
                        anns,
                    };
                    chain.place(&host, 1, &prefixes, &mut root_blocks, &mut chain_roots, &mut chain_defs);
                } else if members.len() == 2 {
                    let (a, b) = (members[0].clone(), members[1].clone());
                    if annotated(ac) {
                        edge_reifs.entry(a.clone()).or_default().push((P_EQUIV_PROPERTY, b.clone(), ax_anns(ac)));
                    }
                    dp_equiv.entry(a).or_default().push(b);
                }
            }
            Component::DisjointDataProperties(d) => {
                if d.0.len() == 2 {
                    let (a, b) = ordered(d.0[0].0.as_ref().to_string(), d.0[1].0.as_ref().to_string());
                    if annotated(ac) {
                        edge_reifs.entry(a.clone()).or_default().push((P_PROPERTY_DISJOINT, b.clone(), ax_anns(ac)));
                    }
                    dp_disjoint.entry(a).or_default().push(b);
                } else {
                    let members: Vec<(Option<String>, String)> =
                        d.0.iter().map(|p| (None, p.0.as_ref().to_string())).collect();
                    gci_blocks.push((ac, render_all_disjoint_properties(&members, &node_annotations(ac, &prefixes))));
                }
            }
            Component::FunctionalDataProperty(p) => {
                let d = p.0 .0.as_ref().to_string();
                if annotated(ac) {
                    type_reif.entry(d.clone()).or_default().push((OWL_FUNCTIONAL_PROPERTY, ax_anns(ac)));
                }
                dp_char.entry(d).or_default().push(OWL_FUNCTIONAL_PROPERTY);
            }
            // A key is one collection in the order the axiom gives: the object
            // properties, then the data properties.
            Component::HasKey(k) if !matches!(k.ce, CE::Class(_)) && ac.ann.is_empty() => {
                // On an anonymous class the key is one more statement of the
                // expression's own node, which stands as a general axiom.
                let key = format!(
                    "        <owl:hasKey rdf:parseType=\"Collection\">\n{}        </owl:hasKey>\n",
                    render_key_members(k).0.concat()
                );
                gci_blocks.push((ac, insert_before_close(&render_ce(&k.ce, 4, &no_g), &key)));
            }
            Component::HasKey(k) => match &k.ce {
                CE::Class(c) => {
                    let (items, members) = render_key_members(k);
                    has_key.entry(c.0.as_ref().to_string()).or_default().push((items, members, ax_anns(ac)))
                }
                // An annotated key on an anonymous class reifies with the
                // expression, stating the key, as its nested source.
                _ => {
                    let (items, members) = render_key_members(k);
                    let pred = "http://www.w3.org/2002/07/owl#hasKey";
                    let gid = named_lists()
                        .then(|| seq_id(shared_seq.get("__general__"), &format!("LIST\u{1}{pred}\u{1}{members}"), &mut taken_ids))
                        .flatten()
                        .map(|id| format!("genid{id}"));
                    let mut defs = String::new();
                    let (edge, target) = list_slots("owl:hasKey", &items, gid.as_deref(), &mut defs);
                    let edge: String = edge.lines().map(|line| format!("        {line}\n")).collect();
                    let source = insert_before_close(&render_ce(&k.ce, 12, &no_g), &edge);
                    gci_blocks.push((
                        ac,
                        format!(
                            "    <owl:Axiom>\n        <owl:annotatedSource>\n{source}        </owl:annotatedSource>\n        <owl:annotatedProperty rdf:resource=\"{pred}\"/>\n{target}{}    </owl:Axiom>\n{defs}",
                            node_annotations(ac, &prefixes)
                        ),
                    ));
                }
            },
            // A datatype's definition hangs off its own block as an
            // `owl:equivalentClass` naming the data range.
            Component::DatatypeDefinition(d) => {
                datatype_defs
                    .entry(d.kind.0.as_ref().to_string())
                    .or_default()
                    .push((d.range.clone(), ax_anns(ac)));
            }
            Component::DisjointObjectProperties(d) => {
                // Pairwise: render `owl:propertyDisjointWith` on the first property.
                if d.0.len() == 2 {
                    if let (Some(a), Some(b)) = (op_name(&d.0[0]), op_name(&d.0[1])) {
                        let (a, b) = ordered(a, b);
                        if annotated(ac) {
                            edge_reifs.entry(a.clone()).or_default().push((P_PROPERTY_DISJOINT, b.clone(), ax_anns(ac)));
                        }
                        op_disjoint.entry(a).or_default().push(b);
                    }
                } else {
                    // Stated of a property an inverse member names, not as a
                    // general axiom.
                    if d.0.iter().any(|m| matches!(m, OPE::InverseObjectProperty(_))) {
                        left_out.push(ac);
                    }
                    let members: Vec<(Option<String>, String)> = d
                        .0
                        .iter()
                        .map(|p| match p {
                            OPE::ObjectProperty(p) => (None, p.0.as_ref().to_string()),
                            OPE::InverseObjectProperty(p) => (Some(String::new()), p.0.as_ref().to_string()),
                        })
                        .collect();
                    gci_blocks.push((ac, render_all_disjoint_properties(&members, &node_annotations(ac, &prefixes))));
                }
            }
            Component::FunctionalObjectProperty(p) => {
                if let Some(n) = op_name(&p.0) {
                    op_char.entry(n.clone()).or_default().push("http://www.w3.org/2002/07/owl#FunctionalProperty");
                    if !ac.ann.is_empty() {
                        type_reif.entry(n).or_default().push((
                            "http://www.w3.org/2002/07/owl#FunctionalProperty",
                            ax_anns(ac),
                        ));
                    }
                }
            }
            Component::InverseFunctionalObjectProperty(p) => {
                if let Some(n) = op_name(&p.0) {
                    op_char.entry(n.clone()).or_default().push("http://www.w3.org/2002/07/owl#InverseFunctionalProperty");
                    if !ac.ann.is_empty() {
                        type_reif.entry(n).or_default().push((
                            "http://www.w3.org/2002/07/owl#InverseFunctionalProperty",
                            ax_anns(ac),
                        ));
                    }
                }
            }
            Component::TransitiveObjectProperty(p) => {
                if let Some(n) = op_name(&p.0) {
                    op_char.entry(n.clone()).or_default().push("http://www.w3.org/2002/07/owl#TransitiveProperty");
                    if !ac.ann.is_empty() {
                        type_reif.entry(n).or_default().push((
                            "http://www.w3.org/2002/07/owl#TransitiveProperty",
                            ax_anns(ac),
                        ));
                    }
                }
            }
            Component::SymmetricObjectProperty(p) => {
                if let Some(n) = op_name(&p.0) {
                    op_char.entry(n.clone()).or_default().push("http://www.w3.org/2002/07/owl#SymmetricProperty");
                    if !ac.ann.is_empty() {
                        type_reif.entry(n).or_default().push((
                            "http://www.w3.org/2002/07/owl#SymmetricProperty",
                            ax_anns(ac),
                        ));
                    }
                }
            }
            Component::AsymmetricObjectProperty(p) => {
                if let Some(n) = op_name(&p.0) {
                    op_char.entry(n.clone()).or_default().push("http://www.w3.org/2002/07/owl#AsymmetricProperty");
                    if !ac.ann.is_empty() {
                        type_reif.entry(n).or_default().push((
                            "http://www.w3.org/2002/07/owl#AsymmetricProperty",
                            ax_anns(ac),
                        ));
                    }
                }
            }
            Component::ReflexiveObjectProperty(p) => {
                if let Some(n) = op_name(&p.0) {
                    op_char.entry(n.clone()).or_default().push("http://www.w3.org/2002/07/owl#ReflexiveProperty");
                    if !ac.ann.is_empty() {
                        type_reif.entry(n).or_default().push((
                            "http://www.w3.org/2002/07/owl#ReflexiveProperty",
                            ax_anns(ac),
                        ));
                    }
                }
            }
            Component::IrreflexiveObjectProperty(p) => {
                if let Some(n) = op_name(&p.0) {
                    op_char.entry(n.clone()).or_default().push("http://www.w3.org/2002/07/owl#IrreflexiveProperty");
                    if !ac.ann.is_empty() {
                        type_reif.entry(n).or_default().push((
                            "http://www.w3.org/2002/07/owl#IrreflexiveProperty",
                            ax_anns(ac),
                        ));
                    }
                }
            }
            Component::AnnotationAssertion(aa) => {
                let nested: Vec<(String, AnnotationValue<RcStr>)> = ac
                    .ann
                    .iter()
                    .map(|a| (a.ap.0.as_ref().to_string(), a.av.clone()))
                    .collect();
                let entry = (aa.ann.ap.0.as_ref().to_string(), aa.ann.av.clone(), nested);
                if !anon_model
                    && ((annotated(ac)
                        && (matches!(aa.subject, horned_owl::model::AnnotationSubject::AnonymousIndividual(_))
                            || matches!(aa.ann.av, AnnotationValue::AnonymousIndividual(_))))
                        || matches!(&aa.subject, horned_owl::model::AnnotationSubject::AnonymousIndividual(a) if nested_anon(a.0.as_ref())))
                {
                    left_out.push(ac);
                }
                match &aa.subject {
                    horned_owl::model::AnnotationSubject::IRI(s) => {
                        ann_assertions.entry(s.as_ref().to_string()).or_default().push(entry);
                    }
                    horned_owl::model::AnnotationSubject::AnonymousIndividual(_) if anon_model => {}
                    horned_owl::model::AnnotationSubject::AnonymousIndividual(a) => {
                        anon_ind.entry(a.0.as_ref().to_string()).or_default().push(entry);
                    }
                }
            }
            _ => {}
        }
    }
    if !left_out.is_empty() {
        let mut seen: HashSet<*const AnnotatedComponent<RcStr>> = HashSet::new();
        left_out.retain(|ac| seen.insert(*ac as *const _));
        return Ok(left_out.iter().map(|ac| crate::io::owlfunc::render_component_line(ac)).collect());
    }
    let ont_iri = write_header_and_ontology(model, &prefixes, w)?;
    // An annotation of the ontology that is annotated is a node of its own
    // after the ontology's, whose source is the ontology. The nodes come in the
    // order a hash set of the header's triples holds their `owl:annotatedSource`
    // triples: by bucket, then as they were added.
    let mut ontology_nested: Vec<&(horned_owl::model::Annotation<RcStr>, Vec<u64>)> =
        genid_pass.ontology_nested.iter().collect();
    if let (Some(iri), true) = (&ont_iri, ontology_nested.len() > 1) {
        fn triples(a: &horned_owl::model::Annotation<RcStr>) -> usize {
            1 + if a.ann.is_empty() { 0 } else { 4 + a.ann.iter().map(triples).sum::<usize>() }
        }
        let header: usize = 1 + model
            .ont
            .iter()
            .map(|ac| match &ac.component {
                Component::OntologyID(id) => usize::from(id.viri.is_some()),
                Component::Import(_) => 1,
                Component::OntologyAnnotation(oa) => triples(&oa.0),
                _ => 0,
            })
            .sum::<usize>();
        let cap = crate::owlapi_hash::java_hashset_capacity(header) as u32;
        let pred = crate::owlapi_hash::iri_hash("http://www.w3.org/2002/07/owl#annotatedSource");
        let object = crate::owlapi_hash::iri_hash(iri);
        let bucket = |node: u64| {
            let subject = crate::owlapi_hash::java_string_hash(&format!("_:genid{node}"));
            let h = subject.wrapping_mul(37).wrapping_add(pred.wrapping_mul(17)).wrapping_add(object) as u32;
            (h ^ (h >> 16)) & (cap - 1)
        };
        ontology_nested.sort_by_key(|(_, ids)| (bucket(ids[0]), ids[0]));
    }
    let mut header_roots = String::new();
    for (a, ids) in ontology_nested {
        let source = match &ont_iri {
            Some(iri) => format!("        <owl:annotatedSource rdf:resource=\"{}\"/>\n", esc_attr(iri)),
            None => String::new(),
        };
        let mut roots = Vec::new();
        annotation_nodes(&BTreeSet::from([a.clone()]), &source, "", &mut ids.iter().copied(), &mut roots, &prefixes);
        for (_, root) in roots {
            header_roots.push_str(&root);
        }
    }
    // Then the roots its anonymous individuals add to it, in node order.
    header_roots.push_str(&sorted_blocks(keyed_blocks(&anon_root_blocks(crate::io::genid::HEADER_GRAPH), &[])));
    write!(w, "{}", resolve_anon(crate::io::genid::HEADER_GRAPH, &header_roots))?;

    // The per-kind entity sections are driven by the ontology's SIGNATURE, not by
    // its `Declaration` axioms, so an entity that is only *referenced* still gets a
    // block — a bare `<owl:ObjectProperty rdf:about="…"/>` when nothing else is
    // said about it. `mondo-base.owl` is built by `remove --select imports`, which
    // strips the import that declared `BFO_0000050`/`BFO_0000051` while
    // `ObjectSomeValuesFrom` references to them survive, so its release carries
    // exactly those two stubs. Union the signature in here (declarations already
    // listed above are deduped by the per-section `sort`/`dedup`).
    //
    // Built-in datatypes are the one exclusion: every plain literal carries
    // `xsd:string`, so the signature always contains it, yet released artefacts
    // have no Datatypes section at all — built-ins are never rendered.
    // The annotation properties this document DECLARES, before the signature is
    // unioned in below — the element choice needs to tell a declared built-in
    // (which has a real `rdf:type` triple) from an undeclared one (which does not).
    let declared_aps: std::collections::HashSet<String> = ann_props.iter().cloned().collect();
    // Likewise for classes: one this document does not declare, whose type triple
    // the import closure therefore supplies, is rendered as an untyped
    // `rdf:Description` (see the Classes section). EFO's edit file carries its 60
    // externally-declared CHEBI/GO/MONDO/CL/OBI classes exactly that way.
    let declared_classes: std::collections::HashSet<String> = classes.iter().cloned().collect();
    // A PUNNED IRI's annotation assertions never attach to a typed block: they are
    // picked up in the trailing `Annotations` section instead — the same place an
    // IRI that is no entity at all is rendered. GSSO puns `GSSO_000699` as both a
    // class and an individual, so rendering its annotations inline would emit every
    // one of them twice AND leave the `Annotations` section short.
    //
    // Punned = an IRI DECLARED under two or more of the six entity kinds. The
    // declarations, not the usage signature: a property-hierarchy axiom that
    // reaches an annotation property from the object-property side puts the IRI
    // in the signature under both kinds, but the document still declares one
    // entity, and that block carries the annotations.
    let mut punned: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let dec = crate::cmd::select::entities(model);
        let mut seen: std::collections::HashSet<&String> = std::collections::HashSet::new();
        for kind in [
            &dec.classes,
            &dec.data_properties,
            &dec.object_properties,
            &dec.annotation_properties,
            &dec.datatypes,
            &dec.individuals,
        ] {
            for iri in kind.iter() {
                if !seen.insert(iri) {
                    punned.insert(iri.clone());
                }
            }
        }
    }
    let punned = &punned;
    {
        let sig = crate::cmd::select::signature_entities(model);
        // A BUILT-IN entity never gets a stub either: an ontology using
        // `rdfs:label`, `rdfs:seeAlso`, `owl:deprecated` and `owl:versionInfo`
        // without declaring any of them renders stubs for none, while its
        // undeclared `IAO_0000115` and `RO_0002200` both get one.
        // `mondo-international.owl` is annotated `owl:versionInfo <date>`, which
        // puts that property in the signature and nowhere else.
        let builtin = |iri: &str| {
            iri.starts_with("http://www.w3.org/2001/XMLSchema#")
                || iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
                || iri.starts_with("http://www.w3.org/2002/07/owl#")
        };
        // A DATATYPE is built-in only if it is one of the datatypes the language
        // itself defines, which is a LIST and not a namespace: `xsd:date`,
        // `xsd:time` and `xsd:gYear` all sit in the XSD namespace and none of them
        // is one. OBI states a creation date that way, so `xsd:date` is in its
        // signature and its module renders the `rdfs:Datatype` stub for it.
        let builtin_dt = |iri: &str| is_builtin_datatype(iri);
        // …except where the IMPORT CLOSURE already declares the entity: adding an
        // (uncollapsed) import that declares `BFO_0000050` makes the stub
        // disappear. This is what keeps `filtered.owl`/`reasoned.owl` stub-free
        // while `mondo-base.owl`, which strips the imports first, gets exactly two.
        let undeclared = |kind: &str, iri: &String| -> bool {
            model.closure_declared.is_empty()
                || !model.closure_declared.contains(&format!("{kind}\u{0}{iri}"))
        };
        // …unless the entity has a BODY here. Every signature entity whose graph is
        // non-empty gets a section; the closure check is only about materialising a
        // *declaration* for one whose graph would otherwise be empty.
        // `IAO_0000115` is declared in `omo_import.owl` AND carries
        // `rdfs:label "definition"` in MONDO's `filtered.owl`, so it must still
        // render — while `IAO_0000231`, closure-declared and bodiless, must not.
        let bodied = |iri: &String| {
            ann_assertions.contains_key(iri)
                || sub_class.contains_key(iri)
                || equiv_class.contains_key(iri)
                || disjoint_class.contains_key(iri)
                || disjoint_union.contains_key(iri)
                || ind_types.contains_key(iri)
                || ind_types_anon.contains_key(iri)
                || sub_ann_prop.contains_key(iri)
                || sub_obj_prop.contains_key(iri)
                || sub_data_prop.contains_key(iri)
                || op_edge_inv.contains_key(iri)
                || inverse_of.contains_key(iri)
                || ap_domain.contains_key(iri)
                || ap_range.contains_key(iri)
        };
        let keep = |kind: &str, i: &String| (undeclared(kind, i) || bodied(i)) && !builtin(i);
        // …with one relaxation, for annotation properties only. A built-in never
        // gets a STUB, but one that carries a body still gets a section: the
        // sections are driven off the signature, and `rdfs:seeAlso` is in
        // `subsets/mondo-rare.owl`'s annotation-property signature (used 462
        // times) while also carrying six annotation assertions of its own, so it
        // belongs in the Annotation Properties section, not the trailing
        // catch-all. Section membership and element choice are INDEPENDENT: the
        // element is `rdf:Description` (see below) because no type triple is
        // synthesised for a built-in — conflating the two gets `mondo-base.owl`
        // wrong.
        let keep_ap = |i: &String| bodied(i) || (undeclared("ap", i) && !builtin(i));
        ann_props.extend(sig.annotation_properties.iter().filter(|i| keep_ap(i)).cloned());
        obj_props.extend(sig.object_properties.iter().filter(|i| keep("op", i)).cloned());
        data_props.extend(sig.data_properties.iter().filter(|i| keep("dp", i)).cloned());
        // Same relaxation as annotation properties, for the same reason: a built-in
        // never gets a STUB, but one carrying a BODY still gets a section.
        // `owl:Nothing` acquires `owl:Nothing ⊑ owl:Nothing` when the ontology
        // mentions it, and the reference writes that as an untyped
        // `rdf:Description` block (see the element choice below).
        let keep_class = |i: &String| bodied(i) || (undeclared("class", i) && !builtin(i));
        classes.extend(sig.classes.iter().filter(|i| keep_class(i)).cloned());
        individuals.extend(sig.individuals.iter().filter(|i| keep("ni", i)).cloned());
        // A datatype the document only ever names as a literal's type is in its
        // signature just the same, and gets the same stub: OBI types a creation date
        // `^^xsd:date` and nowhere declares it.
        datatypes.extend(
            sig.datatypes
                .iter()
                .chain(sig.literal_datatypes.iter())
                .filter(|d| !builtin_dt(d) && undeclared("dt", d))
                .cloned(),
        );
    }

    let prefixes = &prefixes;
    // The `owl:Axiom` blocks for this entity's annotated `rdf:type` axioms — its
    // `Declaration`, and each property characteristic it carries. They reify like
    // any other annotated axiom and sit with the rest of the entity's blocks.
    let type_reifs = |iri: &str| -> String {
        let mut s = String::new();
        // Two axioms that differ only in their annotations are ordered by them.
        let reifs: Vec<&TypeReif> = type_reif.get(iri).into_iter().flatten().collect();
        let first = |t: &str| reifs.iter().position(|r| r.0 == t).unwrap_or(0);
        let mut reifs = reifs.clone();
        reifs.sort_by(|a, b| first(a.0).cmp(&first(b.0)).then_with(|| cmp_ann_list(&a.1, &b.1)));
        for (target, anns) in reifs {
            let t = format!(
                "        <owl:annotatedTarget rdf:resource=\"{}\"/>\n",
                esc_attr(target)
            );
            s.push_str(&edge_reif(iri, P_RDF_TYPE, &t, anns, prefixes));
        }
        s
    };
    // The `owl:Axiom` blocks for this entity's annotated edges (see `edge_reifs`).
    let edge_reif_blocks = |iri: &str| -> String {
        let mut s = String::new();
        // Two axioms that differ only in their annotations are ordered by them.
        let reifs: Vec<&EdgeReif> = edge_reifs.get(iri).into_iter().flatten().collect();
        let first = |p: &str, t: &str| reifs.iter().position(|r| r.0 == p && r.1 == t).unwrap_or(0);
        let mut reifs = reifs.clone();
        reifs.sort_by(|a, b| first(a.0, &a.1).cmp(&first(b.0, &b.1)).then_with(|| cmp_ann_list(&a.2, &b.2)));
        for (pred, target, anns) in reifs {
            let t = format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(target));
            s.push_str(&edge_reif(iri, pred, &t, anns, prefixes));
        }
        s
    };
    // The annotation assertions a TYPED block may carry: none, for a punned IRI.
    let entity_anns = |iri: &str| {
        if punned.contains(iri) {
            None
        } else {
            ann_assertions.get(iri)
        }
    };
    let sorted_res = |m: &BTreeMap<String, Vec<String>>, iri: &str| -> Vec<String> {
        let mut v = m.get(iri).cloned().unwrap_or_default();
        v.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
        v
    };

    // Annotation properties, sorted by IRI (namespace, remainder).
    ann_props.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
    ann_props.dedup();
    if !ann_props.is_empty() {
        write_banner(w, "Annotation properties")?;
    }
    for iri in &ann_props {
        let (mut body, after) = annotation_body(iri, entity_anns(iri), None, prefixes);
        // For an annotation property, SubAnnotationPropertyOf renders after the
        // annotation assertions (unlike a class's SubClassOf, which comes first).
        for sup in sorted_res(&sub_ann_prop, iri) {
            body.push_str(&format!("        <rdfs:subPropertyOf rdf:resource=\"{}\"/>\n", esc_attr(&sup)));
        }
        // Range before domain: an annotation property's axioms take their kind's
        // order, and a range comes before a domain in it.
        let mut ap_reif = String::new();
        for (tag, prop, vals) in [
            ("rdfs:range", P_RANGE, ap_range.get(iri)),
            ("rdfs:domain", P_DOMAIN, ap_domain.get(iri)),
        ] {
            let mut vs: Vec<&(String, Vec<(String, AnnotationValue<RcStr>)>)> =
                vals.map(|v| v.iter().collect()).unwrap_or_default();
            vs.sort_by(|a, b| iri_key(&a.0).cmp(&iri_key(&b.0)));
            vs.dedup_by(|a, b| a.0 == b.0);
            for (target_iri, anns) in vs {
                body.push_str(&format!(
                    "        <{tag} rdf:resource=\"{}\"/>\n",
                    esc_attr(target_iri)
                ));
                if !anns.is_empty() {
                    let target = format!(
                        "        <owl:annotatedTarget rdf:resource=\"{}\"/>\n",
                        esc_attr(target_iri)
                    );
                    ap_reif.push_str(&edge_reif(iri, prop, &target, anns, prefixes));
                }
            }
        }
        let (after, defs) = order_roots(
            iri,
            &format!("{ap_reif}{after}{}{}", type_reifs(iri), edge_reif_blocks(iri)),
            reif_genids.get(iri),
            &genid_pass.nested,
            &mut stated_nested,
            prefixes,
        );
        let after = place_defs(&after, Some(&defs));
        // An undeclared BUILT-IN property has no `rdf:type` triple in the graph —
        // one is never synthesised for a built-in — so its block is an untyped
        // `rdf:Description`, not `owl:AnnotationProperty`.
        let elem = if builtin_ns(iri) && !declared_aps.contains(iri.as_str()) {
            "rdf:Description"
        } else {
            "owl:AnnotationProperty"
        };
        write_entity(w, iri, elem, iri, &body, &after)?;
    }

    // Datatypes.
    datatypes.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
    datatypes.dedup();
    if !datatypes.is_empty() {
        write_banner(w, "Datatypes")?;
    }
    for iri in &datatypes {
        let (abody, ann_after) = annotation_body(iri, entity_anns(iri), None, prefixes);
        let mut body = String::new();
        let mut defs_after = String::new();
        let mut def_reif = String::new();
        let mut seq_pos = 0usize;
        // A definition follows the datatype's annotations, as its kind of axiom
        // does.
        body.push_str(&abody);
        if let Some(defs) = datatype_defs.get(iri) {
            let mut ds: Vec<&DataRangeAnns> = defs.iter().collect();
            ds.sort_by(|a, b| crate::io::owlfunc::cmp_dr(&a.0, &b.0));
            ds.dedup();
            for (dr, anns) in ds {
                let (edge, target) = annotated_data_range(
                    "owl:equivalentClass",
                    dr,
                    anns,
                    shared_seq.get(iri.as_str()),
                    &mut seq_pos,
                    &mut defs_after,
                );
                body.push_str(&edge);
                if let Some(target) = target {
                    def_reif.push_str(&edge_reif(iri, P_EQUIV_CLASS, &target, anns, prefixes));
                }
            }
        }
        let (roots, defs) = order_roots(
            iri,
            &format!("{def_reif}{ann_after}{}", type_reifs(iri)),
            reif_genids.get(iri),
            &genid_pass.nested,
            &mut stated_nested,
            prefixes,
        );
        let after = place_defs(&format!("{defs_after}{roots}"), Some(&defs));
        write_entity(w, iri, "rdfs:Datatype", iri, &body, &after)?;
    }
    // A datatype's block uses the `rdfs:Datatype` element, not an `owl:` one; it
    // is passed to `write_entity` as the element name, like every other section's.

    // Object Properties.
    obj_props.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
    obj_props.dedup();
    if !obj_props.is_empty() {
        write_banner(w, "Object Properties")?;
    }
    for iri in &obj_props {
        let mut body = String::new();
        // The definitions of the nodes this block names by id, in the order it
        // first names them; they follow the block.
        let mut defs = String::new();
        // The edges of one kind to inverse properties, after those to named
        // ones: nested, or named by id when the axiom is annotated.
        let inverse_edges = |rank: u8, body: &mut String, defs: &mut String| {
            let mut edges: Vec<&InverseEdge> =
                op_edge_inv.get(iri).into_iter().flatten().filter(|e| e.0 == rank).collect();
            edges.sort_by(|a, b| iri_key(&a.2).cmp(&iri_key(&b.2)).then_with(|| a.3.is_some().cmp(&b.3.is_some())));
            for (_, tag, inverse_of, node) in edges {
                match node {
                    Some((gid, def)) => {
                        body.push_str(&format!("        <{tag} rdf:nodeID=\"{gid}\"/>\n"));
                        defs.push_str(def);
                    }
                    // A chain's member holds the pair it is the subject of.
                    None => match chain_nested.get(&(iri.clone(), format!("INV\u{1}{inverse_of}"))) {
                        Some(node) => body.push_str(&format!("        <{tag}>\n{node}        </{tag}>\n")),
                        None => body.push_str(&format!(
                            "        <{tag}>\n            <rdf:Description>\n                <owl:inverseOf rdf:resource=\"{}\"/>\n            </rdf:Description>\n        </{tag}>\n",
                            esc_attr(inverse_of)
                        )),
                    },
                }
            }
        };
        // Logical axioms first, in the order a property block takes them:
        // equivalentProperty, subPropertyOf, inverseOf, the `rdf:type`
        // characteristics, domain, range, propertyDisjointWith.
        for eq in sorted_res(&op_equiv, iri) {
            body.push_str(&format!("        <owl:equivalentProperty rdf:resource=\"{}\"/>\n", esc_attr(&eq)));
        }
        inverse_edges(12, &mut body, &mut defs);
        for sup in sorted_res(&sub_obj_prop, iri) {
            body.push_str(&format!("        <rdfs:subPropertyOf rdf:resource=\"{}\"/>\n", esc_attr(&sup)));
        }
        inverse_edges(13, &mut body, &mut defs);
        for inv in sorted_res(&inverse_of, iri) {
            body.push_str(&format!("        <owl:inverseOf rdf:resource=\"{}\"/>\n", esc_attr(&inv)));
        }
        inverse_edges(14, &mut body, &mut defs);
        // Characteristic `rdf:type`s (Transitive/Symmetric/…) render after
        // subPropertyOf/inverseOf and before domain/range.
        if let Some(chars) = op_char.get(iri) {
            let mut c = chars.clone();
            // An entity's axioms order by axiom kind, not by IRI: functional,
            // inverse-functional, symmetric, asymmetric, transitive, reflexive,
            // irreflexive. Sorting the `rdf:type` IRIs alphabetically instead would
            // put RO_0017004's `IrreflexiveProperty` before its `SymmetricProperty`.
            c.sort_by_key(|iri| char_axiom_rank(iri));
            c.dedup();
            for ch in c {
                body.push_str(&format!("        <rdf:type rdf:resource=\"{}\"/>\n", esc_attr(ch)));
            }
        }
        let mut dr_reif = String::new();
        // An annotated anonymous domain or range is a node of its own: the edge
        // and the reification's target both name it by id, and it is defined once,
        // straight after the property. Ids come from the numbering pass in the
        // order it allocated them; an inline-anon document numbers nothing, and
        // nests a copy of the expression in both places instead.
        let mut seq_pos = 0usize;
        for (tag, prop, vals) in [
            ("rdfs:domain", P_DOMAIN, op_domain.get(iri)),
            ("rdfs:range", P_RANGE, op_range.get(iri)),
        ] {
            for (ce, anns) in sorted_prop_ce(vals) {
                let node = (!anns.is_empty() && !matches!(ce, CE::Class(_)) && !inline_anon())
                    .then(|| shared_node_id(shared_seq.get(iri), shared_genids.get(iri), ce, &mut seq_pos));
                match &node {
                    Some(g) => body.push_str(&format!("        <{tag} rdf:nodeID=\"{g}\"/>\n")),
                    None => body.push_str(&render_prop_ce(tag, ce, &no_g)),
                }
                if anns.is_empty() {
                    continue;
                }
                let target = match (ce, &node) {
                    (CE::Class(c), _) => {
                        format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref()))
                    }
                    (_, Some(g)) => {
                        defs.push_str(&inject_nodeid(&render_ce(ce, 4, &no_g), g));
                        format!("        <owl:annotatedTarget rdf:nodeID=\"{g}\"/>\n")
                    }
                    (_, None) => format!(
                        "        <owl:annotatedTarget>\n{}        </owl:annotatedTarget>\n",
                        render_ce(ce, 12, &no_g)
                    ),
                };
                dr_reif.push_str(&edge_reif(iri, prop, &target, anns, prefixes));
            }
        }
        for dj in sorted_res(&op_disjoint, iri) {
            body.push_str(&format!("        <owl:propertyDisjointWith rdf:resource=\"{}\"/>\n", esc_attr(&dj)));
        }
        inverse_edges(24, &mut body, &mut defs);
        let mut chain_reif = String::new();
        if let Some(chains) = op_chains.get(iri) {
            let mut cs = chains.clone();
            cs.sort_by(|a, b| {
                a.0.iter().map(|s| iri_key(chain_link_iri(s))).collect::<Vec<_>>()
                    .cmp(&b.0.iter().map(|s| iri_key(chain_link_iri(s))).collect::<Vec<_>>())
            });
            for (chain, anns) in cs {
                // An annotated chain axiom reifies with the chain as the target.
                let pred = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
                let items: Vec<String> = chain.iter().map(render_chain_link).collect();
                let members = list_members(chain.iter().filter_map(|link| match link {
                    OPE::ObjectProperty(p) => Some(p.0.as_ref()),
                    OPE::InverseObjectProperty(_) => None,
                }));
                let gid = (!anns.is_empty() && named_lists())
                    .then(|| seq_id(shared_seq.get(iri), &format!("LIST\u{1}{pred}\u{1}{members}"), &mut taken_ids))
                    .flatten()
                    .map(|id| format!("genid{id}"));
                let (edge, target) = list_slots("owl:propertyChainAxiom", &items, gid.as_deref(), &mut defs);
                body.push_str(&edge);
                if !anns.is_empty() {
                    chain_reif.push_str(&edge_reif(iri, pred, &target, &anns, prefixes));
                }
            }
        }
        let (abody, ann_after) = annotation_body(iri, entity_anns(iri), None, prefixes);
        body.push_str(&abody);
        // The inverse of this property, as the subject of an unannotated axiom:
        // a block of its own for each axiom.
        let mut inverse_blocks = String::new();
        if let Some(parts) = op_inv.get(iri) {
            let mut ps = parts.clone();
            ps.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            for (_, part) in ps {
                inverse_blocks.push_str(&format!(
                    "    <rdf:Description>\n        <owl:inverseOf rdf:resource=\"{}\"/>\n{part}    </rdf:Description>\n",
                    esc_attr(iri)
                ));
            }
        }
        let inverse_roots: String = inv_roots.get(iri).into_iter().flatten().map(String::as_str).collect();
        let (anon_roots, mut root_defs) = order_roots(
            iri,
            &format!(
                "{dr_reif}{chain_reif}{ann_after}{}{}{inverse_blocks}{inverse_roots}{}",
                type_reifs(iri),
                edge_reif_blocks(iri),
                chain_roots.get(iri).map_or("", String::as_str)
            ),
            reif_genids.get(iri),
            &genid_pass.nested,
            &mut stated_nested,
            prefixes,
        );
        root_defs.extend(chain_defs.get(iri).into_iter().flatten().cloned());
        let after = place_defs(&format!("{defs}{anon_roots}"), Some(&root_defs));
        write_entity(w, iri, "owl:ObjectProperty", iri, &body, &after)?;
        write_root_blocks(w, iri, &root_blocks, &anon_roots)?;
    }

    // Data properties.
    data_props.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
    data_props.dedup();
    if !data_props.is_empty() {
        write_banner(w, "Data properties")?;
        for iri in &data_props {
            let mut body = String::new();
            // Logical axioms first, in the order a property block takes them:
            // equivalentProperty, subPropertyOf, the `rdf:type` characteristics,
            // domain, range, propertyDisjointWith.
            for eq in sorted_res(&dp_equiv, iri) {
                body.push_str(&format!("        <owl:equivalentProperty rdf:resource=\"{}\"/>\n", esc_attr(&eq)));
            }
            for sup in sorted_res(&sub_data_prop, iri) {
                body.push_str(&format!("        <rdfs:subPropertyOf rdf:resource=\"{}\"/>\n", esc_attr(&sup)));
            }
            if let Some(chars) = dp_char.get(iri) {
                let mut c = chars.clone();
                c.sort_by_key(|iri| char_axiom_rank(iri));
                c.dedup();
                for ch in c {
                    body.push_str(&format!("        <rdf:type rdf:resource=\"{}\"/>\n", esc_attr(ch)));
                }
            }
            // An annotated anonymous domain or range is a node of its own, as on
            // an object property.
            let mut dr_reif = String::new();
            let mut dr_defs = String::new();
            let mut seq_pos = 0usize;
            for (ce, anns) in sorted_prop_ce(dp_domain.get(iri)) {
                let node = (!anns.is_empty() && !matches!(ce, CE::Class(_)) && !inline_anon())
                    .then(|| shared_node_id(shared_seq.get(iri), shared_genids.get(iri), ce, &mut seq_pos));
                match &node {
                    Some(g) => body.push_str(&format!("        <rdfs:domain rdf:nodeID=\"{g}\"/>\n")),
                    None => body.push_str(&render_prop_ce("rdfs:domain", ce, &no_g)),
                }
                if anns.is_empty() {
                    continue;
                }
                let target = match (ce, &node) {
                    (CE::Class(c), _) => {
                        format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(c.0.as_ref()))
                    }
                    (_, Some(g)) => {
                        dr_defs.push_str(&inject_nodeid(&render_ce(ce, 4, &no_g), g));
                        format!("        <owl:annotatedTarget rdf:nodeID=\"{g}\"/>\n")
                    }
                    (_, None) => format!(
                        "        <owl:annotatedTarget>\n{}        </owl:annotatedTarget>\n",
                        render_ce(ce, 12, &no_g)
                    ),
                };
                dr_reif.push_str(&edge_reif(iri, P_DOMAIN, &target, anns, prefixes));
            }
            let mut ranges: Vec<&(horned_owl::model::DataRange<RcStr>, Vec<(String, AnnotationValue<RcStr>)>)> =
                dp_range.get(iri).map(|v| v.iter().collect()).unwrap_or_default();
            ranges.sort_by(|a, b| crate::io::owlfunc::cmp_dr(&a.0, &b.0));
            ranges.dedup();
            for (dr, anns) in ranges {
                let (edge, target) =
                    annotated_data_range("rdfs:range", dr, anns, shared_seq.get(iri.as_str()), &mut seq_pos, &mut dr_defs);
                body.push_str(&edge);
                if let Some(target) = target {
                    dr_reif.push_str(&edge_reif(iri, P_RANGE, &target, anns, prefixes));
                }
            }
            for dj in sorted_res(&dp_disjoint, iri) {
                body.push_str(&format!("        <owl:propertyDisjointWith rdf:resource=\"{}\"/>\n", esc_attr(&dj)));
            }
            let (abody, ann_after) = annotation_body(iri, entity_anns(iri), None, prefixes);
            body.push_str(&abody);
            let (anon_roots, root_defs) = order_roots(
                iri,
                &format!(
                    "{dr_reif}{ann_after}{}{}{}",
                    type_reifs(iri),
                    edge_reif_blocks(iri),
                    chain_roots.get(iri).map_or("", String::as_str)
                ),
                reif_genids.get(iri),
                &genid_pass.nested,
                &mut stated_nested,
                prefixes,
            );
            let after = place_defs(&format!("{dr_defs}{anon_roots}"), Some(&root_defs));
            write_entity(w, iri, "owl:DatatypeProperty", iri, &body, &after)?;
            write_root_blocks(w, iri, &root_blocks, &anon_roots)?;
        }
    }

    // Classes.
    classes.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
    classes.dedup();
    if !classes.is_empty() {
        write_banner(w, "Classes")?;
    }
    let no_eq: Vec<SubSup> = Vec::new();
    let no_sub: Vec<SubSup> = Vec::new();
    let no_genids: Vec<String> = Vec::new();
    // A blank node the source shared between SEVERAL classes is defined ONCE and
    // referenced from each of them, so track definitions across the whole render.
    let mut emitted_defs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for iri in &classes {
        let mut body = String::new();
        let mut anon_defs = String::new();
        let mut equiv_reif = String::new();
        let mut sub_reif = String::new();

        let mut eqs: Vec<&SubSup> = equiv_class.get(iri).unwrap_or(&no_eq).iter().collect();
        eqs.sort_by(|a, b| {
            ce_key(&a.0).cmp(&ce_key(&b.0)).then_with(|| cmp_ann_list(&a.1, &b.1))
        });
        let mut sups: Vec<&SubSup> = sub_class.get(iri).unwrap_or(&no_sub).iter().collect();
        sups.sort_by(|a, b| {
            ce_key(&a.0).cmp(&ce_key(&b.0)).then_with(|| cmp_ann_list(&a.1, &b.1))
        });

        // The blank-node ids (`genidN`) for this class's annotated anonymous
        // equivalentClass/subClassOf expressions, in the render order the loop below
        // consumes them (equivalentClass then subClassOf, each ce_key-sorted). Looked
        // up from the computed genid pass by structural signature.
        let _ = &no_genids;
        // Consume this owner's genids in allocation order, falling back to the
        // signature map for anything the ordered list does not cover.
        let mut seq_pos = 0usize;
        // An annotated `DisjointClasses(named, anon)` reifies too, so its anonymous
        // target takes a genid in the same pass — after the equivalences and
        // superclasses, matching both the allocation order (EquivalentClasses 1,
        // SubClassOf 2, DisjointClasses 3) and the order the body renders them.
        let dj_ann: Vec<(CE<RcStr>, Vec<(String, AnnotationValue<RcStr>)>)> = {
            let mut v = disjoint_anon.get(iri.as_str()).cloned().unwrap_or_default();
            v.sort_by(|a, b| crate::io::owlfunc::cmp_ce(&a.0, &b.0));
            v
        };
        let dj_ann_refs: Vec<&(CE<RcStr>, Vec<(String, AnnotationValue<RcStr>)>)> =
            dj_ann.iter().collect();
        let genids: Vec<String> = eqs
            .iter()
            .chain(sups.iter())
            .chain(dj_ann_refs.iter())
            .filter(|(ce, anns)| !matches!(ce, CE::Class(_)) && !anns.is_empty())
            .map(|(ce, _)| {
                let sig = crate::io::genid::ce_sig(ce);
                let from_seq = shared_seq.get(iri).and_then(|v| {
                    let start = seq_pos.min(v.len());
                    v[start..].iter().position(|(s, _)| *s == sig).map(|off| {
                        seq_pos = start + off + 1;
                        v[seq_pos - 1].1
                    })
                });
                from_seq
                    .or_else(|| shared_genids.get(iri).and_then(|m| m.get(&sig)).copied())
                    .map(|g| format!("genid{g}"))
                    .unwrap_or_default()
            })
            .collect();
        let genids = &genids;

        // An anonymous superclass is given a blank node — rendered by `rdf:nodeID`,
        // defined once after the class, reified — exactly when its axiom is
        // ANNOTATED (the reification must point at the node). Genids are consumed
        // positionally in body order (equivalentClass then subClassOf). When an
        // equivalentClass intersection operand is structurally equal to such an
        // annotated node, the same blank node is reused for it — so the operand
        // is emitted as a nodeID reference too. `map` (annotated sig -> genid) drives
        // that operand/filler reuse; `ann_q` supplies each annotated axiom's own
        // genid in render order.
        let reused_sigs = genid_pass.reused.get(iri.as_str());
        let mut map: Genids = Genids::new();
        let mut operand_map: Genids = Genids::new();
        let mut ann_q: Vec<String> = Vec::new();
        let mut defs: Vec<(String, &CE<RcStr>)> = Vec::new();
        let mut gi = 0usize;
        // An inline-anon document numbers nothing: the maps and the genid queue
        // stay empty, so every annotated anonymous expression below takes its
        // inline rendering.
        for (ce, anns) in eqs
            .iter()
            .chain(sups.iter())
            .chain(dj_ann_refs.iter())
            .take_while(|_| !inline_anon())
        {
            if matches!(ce, CE::Class(_)) || anns.is_empty() {
                continue;
            }
            if let Some(g) = genids.get(gi).cloned() {
                gi += 1;
                ann_q.push(g.clone());
                // Only a node the numbering pass actually REUSED earns an entry
                // here: this map is what makes a structurally-equal operand
                // elsewhere render as a reference instead of inline. The annotated
                // axiom's own target renders through `ann_q` positionally and does
                // not need one. Keying every annotated target would make an
                // intersection operand point at an unrelated axiom's node whenever
                // the two happened to share a structure.
                map.entry(ce_sig(ce)).or_insert_with(|| g.clone());
                // `map` answers "did an annotated axiom already emit this structure
                // as `rdf:nodeID`?", which suppresses a plain axiom's duplicate
                // rendering. `operand_map` answers the different question of whether
                // an OPERAND nested inside some other expression should render as a
                // reference to that node instead of inline, and only a node the
                // numbering pass actually reused may do that. One map cannot serve
                // both: every annotated target would become a reference target, so
                // an intersection operand would point at an unrelated axiom's node
                // whenever the two happened to have the same structure.
                // NOTE the two different signatures. `genid::ce_sig` is the pass's
                // Debug-format key, which is what `reused` holds; the local `ce_sig`
                // is the writer's own rendered-tag key, which is what `render_set`
                // looks an operand up by. Testing one against the other silently
                // never matches.
                if reused_sigs.is_some_and(|r: &std::collections::HashSet<String>| {
                    r.contains(&crate::io::genid::ce_sig(ce))
                }) {
                    operand_map.entry(ce_sig(ce)).or_insert_with(|| g.clone());
                }
                defs.push((g, ce));
            }
        }
        let mut aqi = 0usize; // consumes ann_q during the render walk (same order)

        // equivalentClass renders first. Same triple-set rule as subClassOf below:
        // two `EquivalentClasses` axioms over the same target are ONE
        // `C owl:equivalentClass …` triple, and an anonymous target is one blank node
        // keyed on structure. Emitting the edge per axiom instead duplicates it —
        // 149 repeated `<owl:equivalentClass>` intersection definitions in MONDO's
        // `mondo.owl`, e.g. MONDO_0000009's genus-differentia block twice over.
        let mut seen_named_eq: HashSet<&str> = HashSet::new();
        let mut seen_plain_anon_eq: HashSet<String> = HashSet::new();
        for (eq, anns) in &eqs {
            match eq {
                CE::Class(c) => {
                    if seen_named_eq.insert(c.0.as_ref()) {
                        body.push_str(&format!(
                            "        <owl:equivalentClass rdf:resource=\"{}\"/>\n",
                            esc_attr(c.0.as_ref())
                        ));
                    }
                    if !anns.is_empty() {
                        let target = format!(
                            "        <owl:annotatedTarget rdf:resource=\"{}\"/>\n",
                            esc_attr(c.0.as_ref())
                        );
                        equiv_reif.push_str(&edge_reif(iri, EQUIV_PROP, &target, anns, prefixes));
                    }
                }
                _ if !anns.is_empty() => {
                    if inline_anon() {
                        // Inline-anon: every axiom renders its own full inline
                        // edge — two axioms over structurally-equal expressions
                        // are two anonymous nodes — and the reification's
                        // annotatedTarget carries its own copy.
                        let inner = render_ce(eq, 12, &operand_map);
                        if !inner.is_empty() {
                            body.push_str(&format!("        <owl:equivalentClass>\n{inner}        </owl:equivalentClass>\n"));
                        }
                        let target = format!("        <owl:annotatedTarget>\n{inner}        </owl:annotatedTarget>\n");
                        equiv_reif.push_str(&edge_reif(iri, EQUIV_PROP, &target, anns, prefixes));
                    } else if let Some(gid) = ann_q.get(aqi).cloned() {
                        aqi += 1;
                        body.push_str(&format!("        <owl:equivalentClass rdf:nodeID=\"{gid}\"/>\n"));
                        let target = format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n");
                        equiv_reif.push_str(&edge_reif(iri, EQUIV_PROP, &target, anns, prefixes));
                    } else {
                        let inner = render_ce(eq, 12, &operand_map);
                        body.push_str(&format!("        <owl:equivalentClass>\n{inner}        </owl:equivalentClass>\n"));
                    }
                }
                _ => {
                    let sig = ce_sig(eq);
                    if map.contains_key(&sig) {
                        WRITER_SKIPS.with(|c| c.set(c.get() + 1));
                        // An annotated axiom over this expression already emitted the
                        // `rdf:nodeID` edge; that is this axiom's triple too.
                    } else if !seen_plain_anon_eq.insert(sig.clone()) {
                        WRITER_SKIPS.with(|c| c.set(c.get() + 1));
                    } else if true {
                        // A chain's member holds the pair it is the subject of.
                        let inner = match chain_nested.get(&(iri.clone(), sig)) {
                            Some(node) => node.clone(),
                            None => render_ce(eq, 12, &operand_map),
                        };
                        if !inner.is_empty() {
                            body.push_str(&format!("        <owl:equivalentClass>\n{inner}        </owl:equivalentClass>\n"));
                        }
                    }
                }
            }
        }
        // SubClassOf: named superclasses (by IRI) before anonymous restrictions.
        //
        // RDF is a triple SET, so a plain `SubClassOf(C D)` and an annotated one over
        // the same named pair are two OWL axioms but ONE `C rdfs:subClassOf D` triple:
        // the annotation hangs off the separate `owl:Axiom` reification, which is
        // still emitted per annotated axiom. Emitting the edge once per axiom would
        // write the element twice and inflate MONDO's `filtered.owl` by exactly the
        // 44,774 plain twins `span_gaps` adds, taking its 46,487 named edges to
        // 91,261 with the `owl:Axiom` count unchanged at 408,720.
        //
        // Anonymous superclasses collapse the same way, because blank nodes are
        // keyed on the class expression's STRUCTURAL identity: two axioms over an
        // equal `∃R.X` share one blank node, so `C rdfs:subClassOf _:b` is again a
        // single triple. When one of the pair is annotated it owns the `genid` (the
        // reification has to point at it), so the plain twin emits nothing at all
        // rather than a second, inline `<owl:Restriction>` block.
        let mut seen_named_sup: HashSet<&str> = HashSet::new();
        let mut seen_plain_anon: HashSet<String> = HashSet::new();
        let mut seen_ann_gid: HashSet<String> = HashSet::new();
        // Which anonymous superclasses an ANNOTATED `SubClassOf` will render for
        // this class. Only those make a plain twin's triple redundant. A genid
        // minted by an annotated *equivalentClass* does not: `C ≡ ∃R.X` emits
        // `owl:equivalentClass`, never `rdfs:subClassOf`, so the relaxed
        // `C ⊑ ∃R.X` still owes its own edge — pointing at the SHARED node.
        // `om relax --include-subclass-of` on HPO produces exactly this shape,
        // and `hp-base.owl` carries all ten of them.
        let annotated_sub_sigs: HashSet<String> = sups
            .iter()
            .filter(|(ce, anns)| !matches!(ce, CE::Class(_)) && !anns.is_empty())
            .map(|(ce, _)| ce_sig(ce))
            .collect();
        for (sup, anns) in &sups {
            match sup {
                CE::Class(c) => {
                    if seen_named_sup.insert(c.0.as_ref()) {
                        body.push_str(&format!(
                            "        <rdfs:subClassOf rdf:resource=\"{}\"/>\n",
                            esc_attr(c.0.as_ref())
                        ));
                    }
                    if !anns.is_empty() {
                        let target = format!(
                            "        <owl:annotatedTarget rdf:resource=\"{}\"/>\n",
                            esc_attr(c.0.as_ref())
                        );
                        sub_reif.push_str(&edge_reif(iri, SUB_PROP, &target, anns, prefixes));
                    }
                }
                _ if !anns.is_empty() => {
                    if inline_anon() {
                        // Inline-anon: every axiom renders its own full inline
                        // edge — two axioms over structurally-equal expressions
                        // are two anonymous nodes — and the reification's
                        // annotatedTarget carries its own copy.
                        let inner = render_ce(sup, 12, &operand_map);
                        if !inner.is_empty() {
                            body.push_str(&format!("        <rdfs:subClassOf>\n{inner}        </rdfs:subClassOf>\n"));
                        }
                        let target = format!("        <owl:annotatedTarget>\n{inner}        </owl:annotatedTarget>\n");
                        sub_reif.push_str(&edge_reif(iri, SUB_PROP, &target, anns, prefixes));
                    } else if let Some(gid) = ann_q.get(aqi).cloned() {
                        aqi += 1;
                        // Two annotated axioms over one shared node are one RDF
                        // triple: the edge renders once, each axiom keeps its own
                        // reification block pointing at the node.
                        if seen_ann_gid.insert(gid.clone()) {
                            body.push_str(&format!("        <rdfs:subClassOf rdf:nodeID=\"{gid}\"/>\n"));
                        }
                        let target = format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n");
                        sub_reif.push_str(&edge_reif(iri, SUB_PROP, &target, anns, prefixes));
                    } else {
                        let inner = render_ce(sup, 12, &operand_map);
                        body.push_str(&format!("        <rdfs:subClassOf>\n{inner}        </rdfs:subClassOf>\n"));
                    }
                }
                _ => {
                    let sig = ce_sig(sup);
                    if !inline_anon() && annotated_sub_sigs.contains(&sig) {
                        WRITER_SKIPS.with(|c| c.set(c.get() + 1));
                        // An annotated SubClassOf over this same expression already
                        // emitted `<rdfs:subClassOf rdf:nodeID="genidN"/>`; that IS
                        // this axiom's triple too, so emitting an inline block here
                        // would duplicate it (13,763 spurious `<owl:Restriction>`
                        // blocks in MONDO's `filtered.owl` — the plain
                        // `relationship:` twins).
                    } else if !seen_plain_anon.insert(sig.clone()) {
                        WRITER_SKIPS.with(|c| c.set(c.get() + 1));
                    } else if let Some(gid) = map.get(&sig) {
                        // The expression already has a blank node — minted by an
                        // annotated equivalentClass/disjointWith on this same class.
                        // Blank nodes are keyed structurally, so this edge points
                        // at that node rather than inlining a second copy of it.
                        body.push_str(&format!(
                            "        <rdfs:subClassOf rdf:nodeID=\"{gid}\"/>\n"
                        ));
                    } else {
                        let inner = render_ce(sup, 12, &operand_map);
                        if !inner.is_empty() {
                            body.push_str(&format!("        <rdfs:subClassOf>\n{inner}        </rdfs:subClassOf>\n"));
                        }
                    }
                }
            }
        }
        let mut dj_anon_reif = String::new();
        let dj_ann_sigs: HashSet<String> = dj_ann.iter().map(|(ce, _)| ce_sig(ce)).collect();
        for dj in sorted_ce(disjoint_class.get(iri)) {
            if dj_ann_sigs.contains(&ce_sig(dj)) {
                let anns = dj_ann
                    .iter()
                    .find(|(ce, _)| ce_sig(ce) == ce_sig(dj))
                    .map(|(_, a)| a.clone())
                    .unwrap_or_default();
                if inline_anon() {
                    // Inline-anon: full inline edge, and the reification's
                    // annotatedTarget carries its own copy.
                    let inner = render_ce(dj, 12, &no_g);
                    body.push_str(&format!("        <owl:disjointWith>\n{inner}        </owl:disjointWith>\n"));
                    let target = format!("        <owl:annotatedTarget>\n{inner}        </owl:annotatedTarget>\n");
                    dj_anon_reif.push_str(&edge_reif(
                        iri,
                        "http://www.w3.org/2002/07/owl#disjointWith",
                        &target,
                        &anns,
                        prefixes,
                    ));
                    continue;
                }
                if let Some(gid) = ann_q.get(aqi).cloned() {
                    aqi += 1;
                    body.push_str(&format!("        <owl:disjointWith rdf:nodeID=\"{gid}\"/>\n"));
                    let target = format!("        <owl:annotatedTarget rdf:nodeID=\"{gid}\"/>\n");
                    dj_anon_reif.push_str(&edge_reif(
                        iri,
                        "http://www.w3.org/2002/07/owl#disjointWith",
                        &target,
                        &anns,
                        prefixes,
                    ));
                    continue;
                }
            }
            body.push_str(&render_prop_ce("owl:disjointWith", dj, &no_g));
        }
        // An annotated disjoint union is reified with its collection as the
        // target.
        let mut union_reif = String::new();
        // The lists an annotated union or key names by id, defined after the
        // block.
        let mut list_defs = String::new();
        let mut list_id = |pred: &str, members: &str, anns: &[(String, AnnotationValue<RcStr>)]| {
            (!anns.is_empty() && named_lists())
                .then(|| seq_id(shared_seq.get(iri), &format!("LIST\u{1}{pred}\u{1}{members}"), &mut taken_ids))
                .flatten()
                .map(|id| format!("genid{id}"))
        };
        if let Some(unions) = disjoint_union.get(iri) {
            #[allow(clippy::type_complexity)]
            let mut us: Vec<&(Vec<String>, String, Vec<(String, AnnotationValue<RcStr>)>)> = unions.iter().collect();
            us.sort_by(|a, b| a.0.cmp(&b.0));
            for (items, members, anns) in us {
                let pred = "http://www.w3.org/2002/07/owl#disjointUnionOf";
                let gid = list_id(pred, members, anns);
                let (edge, target) = list_slots("owl:disjointUnionOf", items, gid.as_deref(), &mut list_defs);
                body.push_str(&edge);
                if !anns.is_empty() {
                    union_reif.push_str(&edge_reif(iri, pred, &target, anns, prefixes));
                }
            }
        }
        // A key is one `owl:hasKey` collection per axiom, after the class's other
        // logical axioms and before its annotations.
        // An annotated key is reified with its collection as the target.
        let mut key_reif = String::new();
        if let Some(keys) = has_key.get(iri) {
            #[allow(clippy::type_complexity)]
            let mut ks: Vec<&(Vec<String>, String, Vec<(String, AnnotationValue<RcStr>)>)> = keys.iter().collect();
            ks.sort_by(|a, b| a.0.cmp(&b.0));
            for (items, members, anns) in ks {
                let pred = "http://www.w3.org/2002/07/owl#hasKey";
                let gid = list_id(pred, members, anns);
                let (edge, target) = list_slots("owl:hasKey", items, gid.as_deref(), &mut list_defs);
                body.push_str(&edge);
                if !anns.is_empty() {
                    key_reif.push_str(&edge_reif(iri, pred, &target, anns, prefixes));
                }
            }
        }
        let (abody, ann_reif) = annotation_body(iri, entity_anns(iri), frame_orders.get(iri.as_str()), prefixes);
        body.push_str(&abody);
        // Reifications for annotated DisjointClasses edges on this class.
        let mut dj_reif = String::new();
        if let Some(djs) = disjoint_reif.get(iri) {
            let mut sorted = djs.clone();
            sorted.sort_by(|a, b| iri_key(&a.1).cmp(&iri_key(&b.1)).then_with(|| iri_key(&a.0).cmp(&iri_key(&b.0))));
            for (source, target, anns) in sorted {
                let tline = format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(&target));
                dj_reif.push_str(&edge_reif(
                    &source,
                    "http://www.w3.org/2002/07/owl#disjointWith",
                    &tline,
                    &anns,
                    prefixes,
                ));
            }
        }
        // Shared anonymous node definitions are anonymous OBJECTS of the class's
        // edges, so they are not root anonymous nodes: they are emitted from a FIFO
        // queue filled in body-reference order.
        // A node is deferred the FIRST time an edge references it by nodeID, so the
        // emission order is the order the genids first appear in the rendered body
        // (equivalentClass intersection operands, then subClassOf supers, …). Scan
        // the body for `rdf:nodeID="genidN"` occurrences, dedup keeping first, and
        // emit each def in that order. (mondo restrictions are flat, so there is no
        // nested deferral to interleave.)
        let def_by_gid: std::collections::HashMap<&str, &CE<RcStr>> =
            defs.iter().map(|(g, ce)| (g.as_str(), *ce)).collect();
        let mut seen_gid: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut rest = body.as_str();
        while let Some(p) = rest.find("rdf:nodeID=\"") {
            let after_q = &rest[p + "rdf:nodeID=\"".len()..];
            let gid = &after_q[..after_q.find('"').unwrap_or(0)];
            if seen_gid.insert(gid) && emitted_defs.insert(gid.to_string()) {
                if let Some(ce) = def_by_gid.get(gid) {
                    let def = inject_nodeid(&render_ce(ce, 4, &operand_map), gid);
                    match chain_tail.get(gid) {
                        Some(pair) => anon_defs.push_str(&insert_before_close(&def, pair)),
                        None => anon_defs.push_str(&def),
                    }
                }
            }
            rest = after_q;
        }
        let (reifs, mut root_defs) = order_roots(
            iri,
            &format!(
                "{equiv_reif}{sub_reif}{dj_anon_reif}{dj_reif}{union_reif}{key_reif}{ann_reif}{}{}",
                type_reifs(iri),
                chain_roots.get(iri).map_or("", String::as_str)
            ),
            reif_genids.get(iri),
            &genid_pass.nested,
            &mut stated_nested,
            prefixes,
        );
        root_defs.extend(chain_defs.get(iri).into_iter().flatten().cloned());
        let after = place_defs(&format!("{anon_defs}{list_defs}{reifs}"), Some(&root_defs));
        let anon_roots = reifs;
        // Element choice, as for annotation properties above: the
        // `rdf:type owl:Class` triple is written only when nothing else supplies
        // it, and an imported ontology that has the class in signature does supply
        // it. So a class this document references and annotates but does not
        // declare renders as an untyped
        // `rdf:Description` — which is how EFO's edit file holds the CHEBI/GO/MONDO
        // classes it hangs axioms on, and what `mint` must write back.
        let elem = if builtin_ns(iri) && !declared_classes.contains(iri) {
            // A built-in class carries no synthesised type triple, so its block is
            // an untyped `rdf:Description` — the shape `owl:Nothing` renders in.
            "rdf:Description"
        } else if declared_classes.contains(iri)
            || !model.closure_declared.contains(&format!("class\u{0}{iri}"))
        {
            "owl:Class"
        } else {
            "rdf:Description"
        };
        write_entity(w, iri, elem, iri, &body, &after)?;
        write_root_blocks(w, iri, &root_blocks, &anon_roots)?;
    }
    // owl:Thing is a built-in class, and an UNDECLARED entity carrying only
    // annotations belongs in the trailing catch-all like any other: a bare
    // `rdf:Description` among the Annotations, with no per-entity banner. Only a
    // DECLARED owl:Thing is a class like any other, and then its annotations
    // ride on the class block instead.
    const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

    // Individuals: named ones (with per-entity comments), then the anonymous
    // individuals, rendered as bare `rdf:Description` blocks after them.
    individuals.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
    individuals.dedup();
    // The banner belongs to NAMED individuals only. Each section header comes from
    // the corresponding entity list, and the anonymous-individual pass — which runs
    // afterwards — writes none: a document whose only individuals are anonymous
    // gets the blocks and no header, and one `owl:NamedIndividual` is enough to
    // earn it. (EFO's `build/efo.owl` has 19 named individuals, so it does not
    // distinguish the two rules.)
    if !individuals.is_empty() {
        write_banner(w, "Individuals")?;
        for iri in &individuals {
            let (abody, ann_after) = annotation_body(iri, entity_anns(iri), None, prefixes);
            let mut body = String::new();
            // The element names the individual's type where the LANGUAGE supplies
            // one — `owl:Thing` — and `owl:NamedIndividual` drops back to being an
            // `rdf:type` child. An individual typed only by the ontology's own
            // classes keeps `owl:NamedIndividual` as its element and lists those
            // classes as children: OBI does both, one obsolete role typed
            // `owl:Thing` against the rest typed by OBI classes.
            // The declared type comes first, ahead of every asserted class.
            let mut types = sorted_res(&ind_types, iri);
            let elem = match types.iter().position(|t| builtin_ns(t) && t != OWL_NAMED_INDIVIDUAL) {
                Some(i) => {
                    let t = types.remove(i);
                    types.insert(0, OWL_NAMED_INDIVIDUAL.to_string());
                    qname(&t, prefixes)
                }
                None => "owl:NamedIndividual".to_string(),
            };
            for t in types {
                body.push_str(&format!(
                    "        <rdf:type rdf:resource=\"{}\"/>\n",
                    esc_attr(&t)
                ));
            }
            // A named type is a bare `rdf:resource` and sorts ahead of every
            // expression; the expressions follow, in `ce_key` order. An annotated
            // one is a node of its own, as an annotated domain is.
            let mut type_defs = String::new();
            let mut ce_type_reif = String::new();
            let mut seq_pos = 0usize;
            for (ce, anns) in sorted_prop_ce(ind_types_anon.get(iri)) {
                if anns.is_empty() || inline_anon() {
                    let inner = render_ce(ce, 12, &Genids::new());
                    if !inner.is_empty() {
                        body.push_str(&format!("        <rdf:type>\n{inner}        </rdf:type>\n"));
                    }
                    if !anns.is_empty() {
                        let target = format!("        <owl:annotatedTarget>\n{inner}        </owl:annotatedTarget>\n");
                        ce_type_reif.push_str(&edge_reif(iri, P_RDF_TYPE, &target, anns, prefixes));
                    }
                    continue;
                }
                let g = shared_node_id(shared_seq.get(iri.as_str()), shared_genids.get(iri.as_str()), ce, &mut seq_pos);
                body.push_str(&format!("        <rdf:type rdf:nodeID=\"{g}\"/>\n"));
                type_defs.push_str(&inject_nodeid(&render_ce(ce, 4, &no_g), &g));
                let target = format!("        <owl:annotatedTarget rdf:nodeID=\"{g}\"/>\n");
                ce_type_reif.push_str(&edge_reif(iri, P_RDF_TYPE, &target, anns, prefixes));
            }
            // …then the identity edges, ahead of every assertion and annotation.
            if let Some(edges) = ind_identity.get(iri) {
                let mut edges = edges.clone();
                edges.sort_by(|a, b| a.0.cmp(b.0).then_with(|| identity_key(&a.1).cmp(&identity_key(&b.1))));
                for (pred, obj) in edges {
                    match obj {
                        Some(o) if o.starts_with(ANON_SLOT) => {
                            body.push_str(&anon_slot(pred, &o[ANON_SLOT.len_utf8()..], "        "))
                        }
                        Some(o) => body.push_str(&format!(
                            "        <{pred} rdf:resource=\"{}\"/>\n",
                            esc_attr(&o)
                        )),
                        None => body.push_str(&format!(
                            "        <{pred}>\n            <rdf:Description/>\n        </{pred}>\n"
                        )),
                    }
                }
            }
            // Property assertions sit between the types and the annotations, in
            // triple order: property IRI (namespace then remainder), then object.
            if let Some(props) = ind_props.get(iri) {
                // Object assertions before data ones, each by property and then
                // by value — a literal in literal order.
                let anon = ANON_SLOT.to_string();
                let is_data = |lit: &Option<String>| lit.as_ref().is_some_and(|l| *l != anon);
                let value_key = |(_, v, lit): &IndProp| -> (String, String, String) {
                    match lit {
                        None => (v.clone(), String::new(), String::new()),
                        Some(l) if *l == anon => (format!("{}{v}", char::MAX), String::new(), String::new()),
                        Some(attrs) => match (between(attrs, "rdf:datatype=\"", "\""), between(attrs, "xml:lang=\"", "\"")) {
                            (Some(dt), _) => literal_parts_key(v, "", &crate::io::unescape_attr(dt)),
                            (None, Some(lang)) => literal_parts_key(v, lang, RDF_PLAIN_LITERAL),
                            (None, None) => literal_parts_key(v, "", plain_datatype()),
                        },
                    }
                };
                let mut props = props.clone();
                props.sort_by(|a, b| {
                    is_data(&a.2)
                        .cmp(&is_data(&b.2))
                        .then_with(|| iri_key(&a.0).cmp(&iri_key(&b.0)))
                        .then_with(|| value_key(a).cmp(&value_key(b)))
                });
                props.dedup();
                for (p, v, lit) in props {
                    let q = qname(&p, prefixes);
                    match lit {
                        Some(l) if l == anon => body.push_str(&anon_slot(&q, &v, "        ")),
                        Some(attrs) => {
                            body.push_str(&format!("        <{q}{attrs}>{}</{q}>\n", esc(&v)))
                        }
                        None => body.push_str(&format!(
                            "        <{q} rdf:resource=\"{}\"/>\n",
                            esc_attr(&v)
                        )),
                    }
                }
            }
            body.push_str(&abody);
            let mut prop_reifs = String::new();
            if let Some(reifs) = prop_reif.get(iri) {
                // By property and target, as numbered: an assertion on an inverse
                // after the others, and assertions of one statement by their
                // annotations.
                let mut reifs = reifs.clone();
                let ann_list = |anns: &[(String, AnnotationValue<RcStr>)]| {
                    let mut v = anns.to_vec();
                    v.sort_by_key(|x| ann_key(&x.0, &x.1));
                    v
                };
                reifs.sort_by(|a, b| {
                    iri_key(&a.0)
                        .cmp(&iri_key(&b.0))
                        .then_with(|| a.1.cmp(&b.1))
                        .then_with(|| a.4.cmp(&b.4))
                        .then_with(|| cmp_ann_list(&ann_list(&a.3), &ann_list(&b.3)))
                });
                for (p, v, lit, anns, _) in reifs {
                    let t = match lit {
                        Some(attrs) => format!("        <owl:annotatedTarget{attrs}>{}</owl:annotatedTarget>\n", esc(&v)),
                        None => format!("        <owl:annotatedTarget rdf:resource=\"{}\"/>\n", esc_attr(&v)),
                    };
                    prop_reifs.push_str(&edge_reif(iri, &esc_attr(&p), &t, &anns, prefixes));
                }
            }
            // A negative assertion is an anonymous node of the individual's graph,
            // in its place among the reifications.
            let mut negs = String::new();
            if let Some(ns) = neg_assertions.get(iri) {
                let mut ns = ns.clone();
                ns.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| cmp_ann_list(&a.3, &b.3)));
                for (_, _, block, _) in ns {
                    negs.push_str(&block);
                }
            }
            let (anon_roots, root_defs) = order_roots(
                iri,
                &format!(
                    "{ce_type_reif}{ann_after}{}{prop_reifs}{}{negs}{}",
                    type_reifs(iri),
                    edge_reif_blocks(iri),
                    chain_roots.get(iri).map_or("", String::as_str)
                ),
                reif_genids.get(iri),
                &genid_pass.nested,
                &mut stated_nested,
                prefixes,
            );
            let after = place_defs(&format!("{type_defs}{anon_roots}"), Some(&root_defs));
            write_entity(w, iri, &elem, iri, &body, &after)?;
            write_root_blocks(w, iri, &root_blocks, &anon_roots)?;
        }
    }
    // Anonymous individuals are their own pass, OUTSIDE the banner gate above:
    // they run whether or not any NAMED individual put a header there. An
    // anonymous individual every statement naming which is about it is a graph
    // of its own, its roots its node and the reifications and negative
    // assertions about it, in the order the numbering pass met them.
    if anon_model {
        for x in &genid_pass.anon_roots {
            let graph = format!("{}{x}", crate::io::genid::ANON_GRAPH);
            let (roots, defs) = order_roots(&graph, "", None, &genid_pass.nested, &mut stated_nested, prefixes);
            write!(w, "{}", resolve_anon(&graph, &place_defs(&roots, Some(&defs))))?;
        }
    }
    {
        // What the numbering pass did not number, in document order, from the
        // labels scanned off the source — NOT the label's own sort order, which
        // is what a plain walk of the map would give. A `.ofn` naming `_:zzz`,
        // `_:aaa`, `_:mmm` in that order renders in that order; anything the scan
        // did not see keeps a stable place after them.
        let pos = |id: &str| {
            let bare = id.strip_prefix("_:").unwrap_or(id);
            model
                .anon_doc_order
                .iter()
                .position(|l| l == bare)
                .unwrap_or(usize::MAX)
        };
        let mut ids: Vec<&String> = anon_ind.keys().chain(anon_ind_types.keys()).collect();
        ids.sort_by(|a, b| pos(a).cmp(&pos(b)).then_with(|| a.cmp(b)));
        ids.dedup();
        for id in ids {
            let mut body = String::new();
            // The type first, as it is for a named individual. It is also what
            // makes the block readable: without it a bare `<rdf:Description>`
            // carrying only annotations says nothing that distinguishes an
            // individual from leftover structure.
            for c in anon_ind_types.get(id).into_iter().flatten() {
                body.push_str(&format!(
                    "        <rdf:type rdf:resource=\"{}\"/>\n",
                    esc_attr(c)
                ));
            }
            let mut anns = anon_ind.get(id).cloned().unwrap_or_default();
            anns.sort_by(|a, b| ann_key(&a.0, &a.1).cmp(&ann_key(&b.0, &b.1)));
            for (p, av, _) in &anns {
                body.push_str(&render_ann(p, av, prefixes));
            }
            write!(w, "    <rdf:Description>\n{body}    </rdf:Description>\n")?;
        }
    }

    // Annotations: annotation assertions whose subject IRI no typed block carried
    // — the IRI is punned, or it is in no entity signature at all. So both an IRI
    // that is no entity AND one that is two entities land here, under an
    // `rdf:Description` (no leading `<!-- IRI -->` comment).
    let typed: std::collections::HashSet<&String> = ann_props
        .iter()
        .chain(datatypes.iter())
        .chain(obj_props.iter())
        .chain(data_props.iter())
        .chain(classes.iter())
        .chain(individuals.iter())
        .collect();
    let mut untyped: Vec<&String> = ann_assertions
        .keys()
        .filter(|k| {
            // owl:Thing is excluded only when the document DECLARES it, in which
            // case it is a class like any other and its annotations ride on the
            // class block. Undeclared, it belongs here.
            (punned.contains(k.as_str()) || !typed.contains(k))
                && Some(k.as_str()) != ont_iri.as_deref()
                && !(k.as_str() == OWL_THING && classes.iter().any(|c| c == OWL_THING))
        })
        .collect();
    untyped.sort_by(|a, b| iri_key(a).cmp(&iri_key(b)));
    if !untyped.is_empty() {
        write_banner(w, "Annotations")?;
        for iri in &untyped {
            let (body, after) = annotation_body(iri, ann_assertions.get(*iri), None, prefixes);
            let (after, root_defs) =
                order_roots(iri, &after, reif_genids.get(*iri), &genid_pass.nested, &mut stated_nested, prefixes);
            let after = place_defs(&after, Some(&root_defs));
            // rdf:Description block, no per-entity comment or separators.
            let block = format!("    <rdf:Description rdf:about=\"{}\">\n{body}    </rdf:Description>\n", esc_attr(iri));
            write!(w, "{}", resolve_anon(iri, &format!("{block}{after}")))?;
            end_object(w)?;
        }
    }

    // General axioms: anonymous-subject axioms (GCIs, anon disjoints, AllDifferent),
    // rendered last as one section, the blocks emitted back-to-back with no
    // separators.
    if !gci_blocks.is_empty() {
        // The section is ordered by the AXIOM (axiom-kind index, then structure),
        // NOT by rendered text. Sort on the axiom, keeping the rendered block
        // alongside.
        gci_blocks.sort_by(|a, b| crate::io::genid::cmp_axiom(&a.0.component, &b.0.component));
        write_banner(w, "General axioms")?;
        for (ac, b) in &gci_blocks {
            let identity = crate::io::genid::axiom_identity(ac);
            let graph = format!("{}{identity}", crate::io::genid::GENERAL_GRAPH);
            // The anonymous individuals the axiom reaches add their roots to
            // its own.
            if !anon_root_blocks(&graph).is_empty() {
                let root = genid_pass.general_root.get(&identity).copied().unwrap_or(0);
                let (roots, defs) = order_roots(
                    &graph,
                    &format!("{ROOT_KEY}{root}\n{b}"),
                    None,
                    &genid_pass.nested,
                    &mut stated_nested,
                    prefixes,
                );
                write!(w, "{}", resolve_anon(&graph, &place_defs(&roots, Some(&defs))))?;
                end_object(w)?;
                continue;
            }
            // An axiom whose annotations carry annotations of their own is
            // named by id, after the roots those make.
            let nodes = genid_pass.general_nested.get(&identity);
            match nodes.filter(|n| n.iter().any(|node| !stated_nested.contains(node))) {
                Some(nodes) => {
                    let node = nodes[0];
                    let n = &genid_pass.nested[&node];
                    stated_nested.insert(node);
                    stated_nested.extend(n.sources.iter().copied());
                    let gid = format!("genid{node}");
                    let roots: Vec<(Option<u64>, usize, String)> = annotation_roots(node, n, prefixes)
                        .into_iter()
                        .map(|(id, root)| (Some(id), 0, root))
                        .collect();
                    let roots = sorted_blocks(roots);
                    let text = place_defs(&roots, Some(&vec![(gid.clone(), inject_nodeid(b, &gid))]));
                    write!(w, "{}", resolve_anon(&graph, &text))?;
                }
                None => write!(w, "{}", resolve_anon(&graph, b))?,
            }
            end_object(w)?;
        }
    }

    // Rules run after the general axioms — the `Rules` banner, then every SWRL
    // variable as a bare typed `rdf:Description` in first-appearance order across
    // the rules, then the rules themselves.
    write_rules(model, prefixes, &genid_pass.rule_ids, &genid_pass.nested, &mut stated_nested, w)?;

    // Every axiom whose annotations' annotations the layout states has been
    // written with them.
    let unstated = genid_pass
        .nested
        .iter()
        .flat_map(|(node, n)| std::iter::once(node).chain(n.sources.iter()))
        .find(|n| !stated_nested.contains(n));
    if let Some(node) = unstated {
        bail!("RDF/XML writer: the annotations of the annotations on node genid{node} were not written");
    }
    // Every statement about an anonymous individual has been written, in the
    // node of the graph that makes it.
    let unwritten = ANON_DOC.with(|d| {
        let d = d.borrow();
        let doc = d.as_ref()?;
        let rendered = doc.rendered.borrow();
        doc.edges.keys().filter(|k| !rendered.contains(*k)).min().cloned()
    });
    if let Some((graph, x)) = unwritten {
        bail!("RDF/XML writer: what graph {graph:?} states about the anonymous individual {x} was not written");
    }
    write!(w, "</rdf:RDF>\n\n\n\n")?;
    let banner_version = if model.owlapi_456 { "4.5.6" } else { "4.5.29" };
    write!(w, "<!-- Generated by the OWL API (version {banner_version}) https://github.com/owlcs/owlapi -->\n\n")?;
    Ok(Vec::new())
}

// === SWRL rules ==========================================================
//
// `SWRLRule` axioms get their own section, after the general axioms: the `Rules`
// banner, then one `rdf:Description` per variable (in first-appearance order
// across the rules, body before head), then the rules as anonymous `swrl:Imp`
// nodes. Body and head are RDF lists of atoms (`swrl:AtomList` with
// `rdf:first`/`rdf:rest`, terminated by `rdf:nil`), each atom an anonymous node
// typed by its kind.

const SWRL: &str = "http://www.w3.org/2003/11/swrl#";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const RDF_LIST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#List";

/// `<swrl:argument1 rdf:resource="…"/>` for a SWRL individual argument.
fn swrl_iarg(arg: &horned_owl::model::IArgument<RcStr>) -> String {
    use horned_owl::model::{IArgument, Individual};
    match arg {
        IArgument::Variable(v) => v.0.as_ref().to_string(),
        IArgument::Individual(Individual::Named(n)) => n.0.as_ref().to_string(),
        IArgument::Individual(Individual::Anonymous(a)) => a.0.as_ref().to_string(),
    }
}

/// An individual-or-variable argument in its value slot. A variable and a named
/// individual are the IRI they name; an ANONYMOUS individual is a blank node,
/// which nests as a description where the reference would go — empty, or naming
/// its node when the individual appears more than once.
fn swrl_iarg_slot(tag: &str, arg: &horned_owl::model::IArgument<RcStr>, indent: usize) -> String {
    use horned_owl::model::{IArgument, Individual};
    let pad = " ".repeat(indent);
    match arg {
        IArgument::Individual(Individual::Anonymous(a)) if anon_doc_active() => anon_slot(tag, a.0.as_ref(), &pad),
        IArgument::Individual(Individual::Anonymous(a)) => {
            format!("{pad}<{tag}>\n{}{pad}</{tag}>\n", anonymous_object(a.0.as_ref(), &format!("{pad}    ")))
        }
        other => format!("{pad}<{tag} rdf:resource=\"{}\"/>\n", esc_attr(&swrl_iarg(other))),
    }
}

/// A data argument in its value slot: a variable is the IRI it names, a literal
/// is the element's text.
fn swrl_darg_slot(tag: &str, arg: &horned_owl::model::DArgument<RcStr>, indent: usize) -> String {
    use horned_owl::model::DArgument;
    let pad = " ".repeat(indent);
    match arg {
        DArgument::Variable(v) => {
            format!("{pad}<{tag} rdf:resource=\"{}\"/>\n", esc_attr(v.0.as_ref()))
        }
        DArgument::Literal(l) => render_literal_tag(tag, l, indent),
    }
}

/// A built-in atom's `swrl:arguments`: a plain `rdf:List`, not an
/// `swrl:AtomList`, one cell per argument.
fn swrl_darg_list(args: &[horned_owl::model::DArgument<RcStr>], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 4);
    let mut s = format!("{pad}<rdf:Description>\n");
    s.push_str(&format!("{inner}<rdf:type rdf:resource=\"{RDF_LIST}\"/>\n"));
    match args.split_first() {
        None => s.push_str(&format!("{inner}<rdf:rest rdf:resource=\"{RDF_NIL}\"/>\n")),
        Some((first, rest)) => {
            s.push_str(&swrl_darg_slot("rdf:first", first, indent + 4));
            if rest.is_empty() {
                s.push_str(&format!("{inner}<rdf:rest rdf:resource=\"{RDF_NIL}\"/>\n"));
            } else {
                s.push_str(&format!("{inner}<rdf:rest>\n"));
                s.push_str(&swrl_darg_list(rest, indent + 8));
                s.push_str(&format!("{inner}</rdf:rest>\n"));
            }
        }
    }
    s.push_str(&format!("{pad}</rdf:Description>\n"));
    s
}

/// Render one SWRL atom's `rdf:Description` body at `indent` spaces.
fn swrl_atom(atom: &horned_owl::model::Atom<RcStr>, indent: usize) -> String {
    use horned_owl::model::{Atom, ClassExpression as CE, DArgument, ObjectPropertyExpression as OPE};
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 4);
    let mut s = format!("{pad}<rdf:Description>\n");
    let mut ty = |kind: &str, body: &str, s: &mut String| {
        s.push_str(&format!(
            "{inner}<rdf:type rdf:resource=\"{SWRL}{kind}\"/>\n{body}"
        ));
    };
    match atom {
        Atom::ClassAtom { pred, arg } => {
            let predicate = match pred {
                CE::Class(c) => format!(
                    "{inner}<swrl:classPredicate rdf:resource=\"{}\"/>\n",
                    esc_attr(c.0.as_ref())
                ),
                // An anonymous class is the atom's own block, nested where the
                // reference would go — the same shape a class expression takes
                // anywhere else it is not a bare IRI.
                other => format!(
                    "{inner}<swrl:classPredicate>\n{}{inner}</swrl:classPredicate>\n",
                    render_ce(other, indent + 8, &Genids::new())
                ),
            };
            let body = format!("{predicate}{}", swrl_iarg_slot("swrl:argument1", arg, indent + 4));
            ty("ClassAtom", &body, &mut s);
        }
        Atom::ObjectPropertyAtom { pred, args } => {
            // An inverse predicate is its own node, nested where the property
            // would be named.
            let predicate = match pred {
                OPE::ObjectProperty(p) => {
                    format!("{inner}<swrl:propertyPredicate rdf:resource=\"{}\"/>\n", esc_attr(p.0.as_ref()))
                }
                OPE::InverseObjectProperty(p) => format!(
                    "{inner}<swrl:propertyPredicate>\n{inner}    <rdf:Description>\n\
                     {inner}        <owl:inverseOf rdf:resource=\"{}\"/>\n\
                     {inner}    </rdf:Description>\n{inner}</swrl:propertyPredicate>\n",
                    esc_attr(p.0.as_ref())
                ),
            };
            let body = format!(
                "{predicate}{}{}",
                swrl_iarg_slot("swrl:argument1", &args.0, indent + 4),
                swrl_iarg_slot("swrl:argument2", &args.1, indent + 4)
            );
            ty("IndividualPropertyAtom", &body, &mut s);
        }
        Atom::DataPropertyAtom { pred, args } => {
            let arg1 = match &args.0 {
                DArgument::Variable(v) => v.0.as_ref().to_string(),
                DArgument::Literal(l) => l.literal().clone(),
            };
            let body = format!(
                "{inner}<swrl:propertyPredicate rdf:resource=\"{}\"/>\n\
                 {inner}<swrl:argument1 rdf:resource=\"{}\"/>\n\
                 {}",
                esc_attr(pred.0.as_ref()),
                esc_attr(&arg1),
                swrl_darg_slot("swrl:argument2", &args.1, indent + 4)
            );
            ty("DatavaluedPropertyAtom", &body, &mut s);
        }
        Atom::SameIndividualAtom(a, b) => {
            let body = format!(
                "{}{}",
                swrl_iarg_slot("swrl:argument1", a, indent + 4),
                swrl_iarg_slot("swrl:argument2", b, indent + 4)
            );
            ty("SameIndividualAtom", &body, &mut s);
        }
        Atom::DifferentIndividualsAtom(a, b) => {
            let body = format!(
                "{}{}",
                swrl_iarg_slot("swrl:argument1", a, indent + 4),
                swrl_iarg_slot("swrl:argument2", b, indent + 4)
            );
            ty("DifferentIndividualsAtom", &body, &mut s);
        }
        Atom::BuiltInAtom { pred, args } => {
            let body = format!(
                "{inner}<swrl:builtin rdf:resource=\"{}\"/>\n\
                 {inner}<swrl:arguments>\n{}{inner}</swrl:arguments>\n",
                esc_attr(pred.as_ref()),
                swrl_darg_list(args, indent + 8)
            );
            ty("BuiltinAtom", &body, &mut s);
        }
        Atom::DataRangeAtom { pred, arg } => {
            let body = format!(
                "{}{}",
                render_data_range_at("swrl:dataRange", pred, indent + 4),
                swrl_darg_slot("swrl:argument1", arg, indent + 4)
            );
            ty("DataRangeAtom", &body, &mut s);
        }
    }
    s.push_str(&format!("{pad}</rdf:Description>\n"));
    s
}

/// Render a body/head atom list as a nested `swrl:AtomList`.
fn swrl_atom_list(atoms: &[horned_owl::model::Atom<RcStr>], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 4);
    let mut s = format!("{pad}<rdf:Description>\n");
    s.push_str(&format!("{inner}<rdf:type rdf:resource=\"{SWRL}AtomList\"/>\n"));
    match atoms.split_first() {
        None => {
            s.push_str(&format!("{inner}<rdf:rest rdf:resource=\"{RDF_NIL}\"/>\n"));
        }
        Some((first, rest)) => {
            s.push_str(&format!("{inner}<rdf:first>\n"));
            s.push_str(&swrl_atom(first, indent + 8));
            s.push_str(&format!("{inner}</rdf:first>\n"));
            if rest.is_empty() {
                s.push_str(&format!("{inner}<rdf:rest rdf:resource=\"{RDF_NIL}\"/>\n"));
            } else {
                s.push_str(&format!("{inner}<rdf:rest>\n"));
                s.push_str(&swrl_atom_list(rest, indent + 8));
                s.push_str(&format!("{inner}</rdf:rest>\n"));
            }
        }
    }
    s.push_str(&format!("{pad}</rdf:Description>\n"));
    s
}

/// Every SWRL variable IRI an atom mentions, appended to `out` in visit order.
fn swrl_vars_of(atom: &horned_owl::model::Atom<RcStr>, out: &mut Vec<String>) {
    use horned_owl::model::{Atom, DArgument, IArgument};
    let mut iarg = |a: &IArgument<RcStr>, out: &mut Vec<String>| {
        if let IArgument::Variable(v) = a {
            out.push(v.0.as_ref().to_string());
        }
    };
    let darg = |a: &DArgument<RcStr>, out: &mut Vec<String>| {
        if let DArgument::Variable(v) = a {
            out.push(v.0.as_ref().to_string());
        }
    };
    match atom {
        Atom::ClassAtom { arg, .. } => iarg(arg, out),
        Atom::ObjectPropertyAtom { args, .. } => {
            iarg(&args.0, out);
            iarg(&args.1, out);
        }
        Atom::DataPropertyAtom { args, .. } => {
            darg(&args.0, out);
            darg(&args.1, out);
        }
        Atom::SameIndividualAtom(a, b) | Atom::DifferentIndividualsAtom(a, b) => {
            iarg(a, out);
            iarg(b, out);
        }
        Atom::BuiltInAtom { args, .. } => {
            for a in args {
                darg(a, out);
            }
        }
        Atom::DataRangeAtom { arg, .. } => darg(arg, out),
    }
}

/// The type index for a SWRL atom (`SWRL` base 6000), which is the first key of
/// the atom sort order.
fn swrl_atom_index(atom: &horned_owl::model::Atom<RcStr>) -> u32 {
    use horned_owl::model::Atom;
    match atom {
        Atom::ClassAtom { .. } => 6002,
        Atom::DataRangeAtom { .. } => 6003,
        Atom::ObjectPropertyAtom { .. } => 6004,
        Atom::DataPropertyAtom { .. } => 6005,
        Atom::BuiltInAtom { .. } => 6006,
        Atom::SameIndividualAtom(..) => 6010,
        Atom::DifferentIndividualsAtom(..) => 6011,
    }
}

/// The sort key for a SWRL atom: the type index, then the predicate and arguments
/// in order. An argument's own key is its type index (variable 6007 before
/// individual 6008) then its IRI.
fn swrl_atom_key(atom: &horned_owl::model::Atom<RcStr>) -> (u32, String, Vec<(u32, String)>) {
    use horned_owl::model::{Atom, ClassExpression as CE, DArgument, IArgument, Individual,
        ObjectPropertyExpression as OPE};
    let ikey = |a: &IArgument<RcStr>| match a {
        IArgument::Variable(v) => (6007u32, v.0.as_ref().to_string()),
        IArgument::Individual(Individual::Named(n)) => (6008, n.0.as_ref().to_string()),
        IArgument::Individual(Individual::Anonymous(x)) => (6008, x.0.as_ref().to_string()),
    };
    let dkey = |a: &DArgument<RcStr>| match a {
        DArgument::Variable(v) => (6007u32, v.0.as_ref().to_string()),
        DArgument::Literal(l) => (6009, l.literal().clone()),
    };
    let idx = swrl_atom_index(atom);
    match atom {
        Atom::ClassAtom { pred, arg } => {
            let p = match pred {
                CE::Class(c) => c.0.as_ref().to_string(),
                _ => String::new(),
            };
            (idx, p, vec![ikey(arg)])
        }
        Atom::ObjectPropertyAtom { pred, args } => {
            let p = match pred {
                OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => p.0.as_ref().to_string(),
            };
            (idx, p, vec![ikey(&args.0), ikey(&args.1)])
        }
        Atom::DataPropertyAtom { pred, args } => {
            (idx, pred.0.as_ref().to_string(), vec![dkey(&args.0), dkey(&args.1)])
        }
        Atom::SameIndividualAtom(a, b) | Atom::DifferentIndividualsAtom(a, b) => {
            (idx, String::new(), vec![ikey(a), ikey(b)])
        }
        Atom::BuiltInAtom { pred, args } => {
            (idx, pred.as_ref().to_string(), args.iter().map(dkey).collect())
        }
        Atom::DataRangeAtom { arg, .. } => (idx, String::new(), vec![dkey(arg)]),
    }
}

/// A rule's sort key: body then head, each compared as a SET — both sides are
/// sorted first, so the rule's own atom order does NOT decide the comparison.
pub(crate) fn owlapi_rule_key(
    r: &horned_owl::model::Rule<RcStr>,
) -> (Vec<(u32, String, Vec<(u32, String)>)>, Vec<(u32, String, Vec<(u32, String)>)>) {
    let mut body: Vec<_> = r.body.iter().map(swrl_atom_key).collect();
    let mut head: Vec<_> = r.head.iter().map(swrl_atom_key).collect();
    body.sort();
    head.sort();
    (body, head)
}

/// The `Rules` section, or nothing when the ontology holds no SWRL rule.
fn write_rules<W: Write>(
    model: &Model,
    prefixes: &[(String, String)],
    rule_ids: &[u64],
    nested: &HashMap<u64, crate::io::genid::NestedAnnotations>,
    stated: &mut HashSet<u64>,
    w: &mut W,
) -> Result<()> {
    let mut rules: Vec<(&horned_owl::model::Rule<RcStr>, &AnnotatedComponent<RcStr>)> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::Rule(r) => Some((r, ac)),
            _ => None,
        })
        .collect();
    if rules.is_empty() {
        return Ok(());
    }
    // In `owlapi_rule_key` order, not horned-owl's derived `Ord`: the latter
    // compares the body as a LIST in the rule's own atom order, so a rule opening
    // with a `ClassAtom` would sort before one opening with an
    // `ObjectPropertyAtom` whatever their predicates are. The atoms are sorted
    // first here.
    rules.sort_by_key(|(r, _)| owlapi_rule_key(r));
    write_banner(w, "Rules")?;

    // Each rule is an anonymous node, and the section emits the anonymous roots in
    // the order their blank-node ids sort AS STRINGS. The ids run upwards in
    // `owlapi_rule_key` order, so a section whose ids straddle a power of ten comes
    // out rotated: RO's twenty-five rules take genid868…genid1060, and the nine
    // from genid1000 on lead. `rule_ids` is the numbering pass's answer for the
    // same order this list is in, so the two zip positionally.
    if rule_ids.len() != rules.len() {
        bail!(
            "internal: the numbering pass counted {} SWRL rule(s) and the writer has {}",
            rule_ids.len(),
            rules.len()
        );
    }

    let mut vars: Vec<String> = Vec::new();
    for (r, _) in &rules {
        for a in r.body.iter().chain(r.head.iter()) {
            swrl_vars_of(a, &mut vars);
        }
    }
    let mut seen = HashSet::new();
    for v in vars {
        if !seen.insert(v.clone()) {
            continue;
        }
        write!(w, "    <rdf:Description rdf:about=\"{}\">\n", esc_attr(&v))?;
        write!(w, "        <rdf:type rdf:resource=\"{SWRL}Variable\"/>\n")?;
        write!(w, "    </rdf:Description>\n")?;
    }

    // A rule whose annotations carry annotations of their own is named by id,
    // after the first of the roots those make, which take its place.
    let mut blocks: Vec<(Option<u64>, usize, String)> = Vec::new();
    let mut defs: Vec<(String, String)> = Vec::new();
    for (i, ((r, ac), id)) in rules.iter().zip(rule_ids).enumerate() {
        let mut b = String::from("    <rdf:Description>\n");
        // A rule's own annotations come FIRST, before its `rdf:type` — RO labels
        // and comments every rule it ships.
        let mut anns: Vec<_> = ac.ann.iter().collect();
        anns.sort_by(|a, b| ann_key(a.ap.0.as_ref(), &a.av).cmp(&ann_key(b.ap.0.as_ref(), &b.av)));
        for a in anns {
            b.push_str(&render_ann(a.ap.0.as_ref(), &a.av, prefixes));
        }
        b.push_str(&format!("        <rdf:type rdf:resource=\"{SWRL}Imp\"/>\n"));
        b.push_str("        <swrl:body>\n");
        b.push_str(&swrl_atom_list(&r.body, 12));
        b.push_str("        </swrl:body>\n");
        b.push_str("        <swrl:head>\n");
        b.push_str(&swrl_atom_list(&r.head, 12));
        b.push_str("        </swrl:head>\n");
        b.push_str("    </rdf:Description>\n");
        match nested.get(id) {
            Some(n) => {
                stated.insert(*id);
                let gid = format!("genid{id}");
                defs.push((gid.clone(), inject_nodeid(&b, &gid)));
                blocks.extend(annotation_roots(*id, n, prefixes).into_iter().map(|(root_id, root)| (Some(root_id), i, root)));
            }
            None => blocks.push((Some(*id), i, b)),
        }
    }
    // With the roots the anonymous individuals the rules name add to their
    // graph, by node.
    blocks.extend(keyed_blocks(&anon_root_blocks(crate::io::genid::RULES_GRAPH), &[]));
    let text = place_defs(&sorted_blocks(blocks), Some(&defs));
    write!(w, "{}", resolve_anon(crate::io::genid::RULES_GRAPH, &text))?;
    Ok(())
}
