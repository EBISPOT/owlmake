//! OBO Graphs JSON support — the graph serialization used across the OBO
//! ecosystem (geneontology/obographs). An ontology is rendered as a set of
//! nodes (entities with metadata) and edges (subClassOf as `is_a`, and
//! existential relationships as property edges).
//!
//! The writer emits the layout OBO Graphs consumers expect: entities grouped by
//! kind and sorted by (namespace, NCName remainder), `" : "` key separators and
//! bracketed arrays, and the obographs field order. Every list is totally
//! ordered, so a release diff shows real content changes and never a reshuffle.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use anyhow::Result;
use horned_owl::model::{
    AnnotationSubject, AnnotationValue, Build, ClassExpression as CE, Component, DeclareClass,
    Individual, Literal, MutableOntology, ObjectPropertyExpression as OPE, RcStr, SubClassOf,
};
use horned_owl::ontology::set::SetOntology;
use serde::{Deserialize, Serialize};

use crate::io::obo::{expand_id, ncname_suffix_index};
use crate::model::{default_prefixes, Model};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether per-element axiom annotations (an xref's / synonym's / definition's /
/// property-value's `source`, etc.) are nested as that element's own `meta`. It is
/// a byte-level convention a repo's existing releases either carry or do not:
/// ECTO's `ecto.json` carries the nested `meta`, while OBA's and MONDO's `.json`s
/// do not and would gain two million spurious lines with it on. Ingest resolves
/// which convention a repo builds under and records it as `Plan::robot_version`;
/// execution sets this once from the plan.
///
/// There is deliberately no environment override: an ambient variable that beat
/// the plan-derived value would be an input deciding artefact bytes from outside
/// the plan.
static NEST_AXIOM_ANNS: AtomicBool = AtomicBool::new(false);

/// Set whether nested axiom-annotation `meta` is emitted — see `NEST_AXIOM_ANNS`.
pub fn set_nest_axiom_anns(on: bool) {
    NEST_AXIOM_ANNS.store(on, Ordering::Relaxed);
}

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const IAO_DEF: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const OIO: &str = "http://www.geneontology.org/formats/oboInOwl#";
const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";

/// Split an IRI into (namespace, remainder) at its XML NCName-suffix boundary.
/// Entities compare on this pair rather than on the raw IRI string, which is what
/// orders both the obographs node list and the RDF/XML entity list.
fn ns_rem(iri: &str) -> (&str, &str) {
    match ncname_suffix_index(iri) {
        Some(i) => (&iri[..i], &iri[i..]),
        None => (iri, ""),
    }
}

// ---------------------------------------------------------------------------
// obographs data model (serde field order == emitted field order)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct GraphDoc {
    graphs: Vec<Graph>,
}

#[derive(Serialize, Deserialize, Default)]
struct Graph {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<GraphMeta>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    edges: Vec<Edge>,
    #[serde(rename = "equivalentNodesSets", default, skip_serializing_if = "Vec::is_empty")]
    equivalent_nodes_sets: Vec<EquivalentNodesSet>,
    #[serde(rename = "logicalDefinitionAxioms", default, skip_serializing_if = "Vec::is_empty")]
    logical_definition_axioms: Vec<LogicalDefinition>,
    #[serde(rename = "domainRangeAxioms", default, skip_serializing_if = "Vec::is_empty")]
    domain_range_axioms: Vec<DomainRangeAxiom>,
    #[serde(rename = "propertyChainAxioms", default, skip_serializing_if = "Vec::is_empty")]
    property_chain_axioms: Vec<PropertyChainAxiom>,
}

#[derive(Serialize, Deserialize, Default)]
struct PropertyChainAxiom {
    #[serde(rename = "predicateId")]
    predicate_id: String,
    #[serde(rename = "chainPredicateIds")]
    chain_predicate_ids: Vec<String>,
}

/// One `domainRangeAxioms` entry (per predicate). Field order matches obographs.
#[derive(Serialize, Deserialize, Default)]
struct DomainRangeAxiom {
    #[serde(rename = "predicateId")]
    predicate_id: String,
    #[serde(rename = "domainClassIds", default, skip_serializing_if = "Vec::is_empty")]
    domain_class_ids: Vec<String>,
    #[serde(rename = "rangeClassIds", default, skip_serializing_if = "Vec::is_empty")]
    range_class_ids: Vec<String>,
    #[serde(rename = "allValuesFromEdges", default, skip_serializing_if = "Vec::is_empty")]
    all_values_from_edges: Vec<Edge>,
}

/// Accumulator for a predicate's domain/range/allValuesFrom data during the pass.
#[derive(Default)]
struct DrEntry {
    domain_class_ids: Vec<String>,
    range_class_ids: Vec<String>,
    all_values_from_edges: Vec<Edge>,
}


#[derive(Serialize, Deserialize, Default)]
struct GraphMeta {
    #[serde(rename = "basicPropertyValues", default, skip_serializing_if = "Vec::is_empty")]
    basic_property_values: Vec<Bpv>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    version: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Node {
    id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    lbl: String,
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    node_type: String,
    #[serde(rename = "propertyType", default, skip_serializing_if = "String::is_empty")]
    property_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Meta>,
}

#[derive(Serialize, Deserialize, Default)]
struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    definition: Option<Definition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    subsets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    synonyms: Vec<Synonym>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    xrefs: Vec<Xref>,
    #[serde(rename = "basicPropertyValues", default, skip_serializing_if = "Vec::is_empty")]
    basic_property_values: Vec<Bpv>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    deprecated: bool,
}

impl Meta {
    fn is_empty(&self) -> bool {
        self.definition.is_none()
            && self.comments.is_empty()
            && self.subsets.is_empty()
            && self.synonyms.is_empty()
            && self.xrefs.is_empty()
            && self.basic_property_values.is_empty()
            && !self.deprecated
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Definition {
    // A missing definition value is absent from the JSON rather than empty,
    // leaving `{ }`; skip an empty value so it never appears as `"val" : ""`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    val: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    xrefs: Vec<String>,
    /// Axiom annotations (`oboInOwl:source`, …) on the definition assertion,
    /// emitted as the definition's own `meta.basicPropertyValues`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<EdgeMeta>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Synonym {
    #[serde(rename = "synonymType", default, skip_serializing_if = "String::is_empty")]
    synonym_type: String,
    pred: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    val: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    xrefs: Vec<String>,
    /// Non-xref/non-synonymType axiom annotations (e.g. `OMO_0002001`), emitted
    /// as the synonym's own `meta.basicPropertyValues`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<EdgeMeta>,
    /// The underlying axiom's annotation-list comparison key, which breaks the tie
    /// between two synonyms sharing a (pred, val) so their order is total.
    #[serde(skip)]
    ann_key: AnnKey,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Xref {
    val: String,
    /// Axiom annotations (`oboInOwl:source`, …) on the `hasDbXref` assertion,
    /// emitted as the xref's own `meta.basicPropertyValues`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<EdgeMeta>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Bpv {
    pred: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    val: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    xrefs: Vec<String>,
    /// Axiom annotations (`oboInOwl:source`, …) on this property-value assertion,
    /// emitted as the value's own `meta.basicPropertyValues`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<EdgeMeta>,
    #[serde(skip)]
    val_is_iri: bool,
    /// The literal value's datatype IRI (empty for an IRI value). Two typed
    /// values under one predicate order by datatype before lexical form.
    #[serde(skip)]
    datatype: String,
    /// True for a plain `xsd:string` literal value, false for an IRI or a typed
    /// literal (e.g. `xsd:anyURI`). Within one predicate a plain-string value
    /// sorts before a typed one.
    #[serde(skip)]
    plain: bool,
}

/// The `(property, value)` sort key for a basicPropertyValue: both the
/// property IRI and (when the value is an IRI) the value compare on their
/// (namespace, NCName remainder) split, so `.../Languages_of_Mauritius` orders
/// before `.../ISO_3166-2:MU` (its namespace, cut at the `:`, is longer). Within
/// a property an IRI value sorts before a literal, and a plain `xsd:string`
/// literal before a typed one (e.g. an `xsd:anyURI` term-tracker URL).
fn bpv_key(b: &Bpv) -> (String, String, u8, String, String, String) {
    let (pns, prem) = ns_rem(&b.pred);
    let (vns, vrem) = if b.val_is_iri {
        let (a, c) = ns_rem(&b.val);
        (a.to_string(), c.to_string())
    } else {
        (b.val.clone(), String::new())
    };
    (
        pns.to_string(),
        prem.to_string(),
        (!b.val_is_iri) as u8,
        b.datatype.clone(),
        vns,
        vrem,
    )
}

#[derive(Serialize, Deserialize, Default)]
struct Edge {
    sub: String,
    pred: String,
    obj: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<EdgeMeta>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct EdgeMeta {
    // Before `basicPropertyValues`, the order these two also take in a `Meta`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    xrefs: Vec<Xref>,
    #[serde(rename = "basicPropertyValues", default, skip_serializing_if = "Vec::is_empty")]
    basic_property_values: Vec<Bpv>,
}

/// An edge (SubClassOf axiom) carries `basicPropertyValues` from its axiom
/// annotations (`oboInOwl:source`, etc.).
fn edge_meta(
    anns: &std::collections::BTreeSet<horned_owl::model::Annotation<RcStr>>,
) -> Option<EdgeMeta> {
    // `hasDbXref` is surfaced through the dedicated `xrefs` list, exactly as it is
    // on a node's meta — an `is_a` carrying `Annotation(hasDbXref "PMID:3972225")`
    // comes out as `"xrefs": [{"val": "PMID:3972225"}]`, not as a
    // `basicPropertyValues` entry naming the property.
    let hasdbxref = format!("{OIO}hasDbXref");
    let xrefs: Vec<Xref> = anns
        .iter()
        .filter(|a| a.ap.0.as_ref() == hasdbxref)
        .filter_map(|a| match &a.av {
            AnnotationValue::Literal(l) => Some(Xref { val: literal_text(l), meta: None }),
            AnnotationValue::IRI(i) => Some(Xref { val: i.as_ref().to_string(), meta: None }),
            _ => None,
        })
        .collect();
    let mut bpv: Vec<Bpv> = anns
        .iter()
        .filter(|a| a.ap.0.as_ref() != hasdbxref)
        .filter_map(|a| {
            let (val, is_iri) = match &a.av {
                AnnotationValue::Literal(l) => (literal_text(l), false),
                AnnotationValue::IRI(i) => (i.as_ref().to_string(), true),
                _ => return None,
            };
            let plain = is_xsd_string(&a.av);
            Some(Bpv { pred: a.ap.0.as_ref().to_string(), val, xrefs: vec![], meta: None, val_is_iri: is_iri, datatype: value_datatype(&a.av), plain })
        })
        .collect();
    if bpv.is_empty() && xrefs.is_empty() {
        return None;
    }
    bpv.sort_by(|a, b| bpv_key(a).cmp(&bpv_key(b)));
    Some(EdgeMeta { xrefs, basic_property_values: bpv })
}

/// Axiom annotations nested as a definition's / synonym's / xref's /
/// property-value's own `meta.basicPropertyValues`. Same conversion as
/// `edge_meta`, but `hasDbXref` (consumed into the element's `xrefs` list) and
/// `hasSynonymType` (consumed into a synonym's `synonymType`) are dropped, since
/// those reach the reader through the dedicated fields rather than as a bpv.
fn nested_meta(
    anns: &std::collections::BTreeSet<horned_owl::model::Annotation<RcStr>>,
) -> Option<EdgeMeta> {
    if !NEST_AXIOM_ANNS.load(Ordering::Relaxed) {
        return None;
    }
    let hasdbxref = format!("{OIO}hasDbXref");
    let hassyntype = format!("{OIO}hasSynonymType");
    let mut bpv: Vec<Bpv> = anns
        .iter()
        .filter(|a| {
            let p = a.ap.0.as_ref();
            p != hasdbxref && p != hassyntype
        })
        .filter_map(|a| {
            let (val, is_iri) = match &a.av {
                AnnotationValue::Literal(l) => (literal_text(l), false),
                AnnotationValue::IRI(i) => (i.as_ref().to_string(), true),
                _ => return None,
            };
            let plain = is_xsd_string(&a.av);
            Some(Bpv {
                pred: a.ap.0.as_ref().to_string(),
                val,
                xrefs: vec![],
                meta: None,
                val_is_iri: is_iri,
                datatype: value_datatype(&a.av),
                plain,
            })
        })
        .collect();
    if bpv.is_empty() {
        return None;
    }
    bpv.sort_by(|a, b| bpv_key(a).cmp(&bpv_key(b)));
    Some(EdgeMeta { xrefs: Vec::new(), basic_property_values: bpv })
}

#[derive(Serialize, Deserialize, Default)]
struct EquivalentNodesSet {
    #[serde(rename = "representativeNodeId")]
    representative_node_id: String,
    #[serde(rename = "nodeIds")]
    node_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct LogicalDefinition {
    #[serde(rename = "definedClassId")]
    defined_class_id: String,
    #[serde(rename = "genusIds", default, skip_serializing_if = "Vec::is_empty")]
    genus_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    restrictions: Vec<Restriction>,
    /// Sort tie-break only: `definedClassId` alone is not a total order, because one
    /// class can carry several equivalence axioms — EFO has classes with a genus-only
    /// definition alongside ones carrying restrictions. The axioms' class-expression
    /// lists, compared element by element, decide between them.
    #[serde(skip)]
    order: Vec<CE<RcStr>>,
}

#[derive(Serialize, Deserialize, Default)]
struct Restriction {
    #[serde(rename = "propertyId")]
    property_id: String,
    #[serde(rename = "fillerId")]
    filler_id: String,
}

/// The synonym-scope predicates, mapped to their obographs short name.
fn synonym_pred(iri: &str) -> Option<&'static str> {
    match iri.strip_prefix(OIO) {
        Some("hasExactSynonym") => Some("hasExactSynonym"),
        Some("hasRelatedSynonym") => Some("hasRelatedSynonym"),
        Some("hasBroadSynonym") => Some("hasBroadSynonym"),
        Some("hasNarrowSynonym") => Some("hasNarrowSynonym"),
        _ => None,
    }
}

/// The `hasDbXref` values annotating an axiom (a definition's or synonym's
/// source references live on the annotation assertion itself).
/// A comparison key for an axiom's annotation list: element-wise over the sorted
/// annotations, then by length. Each annotation is `(prop_ns, prop_rem,
/// value_rank, value_a, value_b)`: the property splits on its NCName suffix like
/// any IRI; an IRI value (rank 0) sorts before a literal value (rank 1) and
/// splits the same way, a literal keeps its text in `value_a`. Rust's `Vec`
/// ordering over that gives the list a total order, so two synonyms with the same
/// (predicate, value) come out ordered by their xref/synonymType annotations
/// rather than in whatever order the axiom set iterated.
type AnnKey = Vec<(String, String, u8, String, String)>;

fn ann_sort_key(anns: &std::collections::BTreeSet<horned_owl::model::Annotation<RcStr>>) -> AnnKey {
    let mut v: AnnKey = anns
        .iter()
        .map(|a| {
            let (pns, prem) = ns_rem(a.ap.0.as_ref());
            let (rank, va, vb) = match &a.av {
                AnnotationValue::IRI(i) => {
                    let (n, r) = ns_rem(i.as_ref());
                    (0u8, n.to_string(), r.to_string())
                }
                AnnotationValue::Literal(l) => (1u8, literal_text(l), String::new()),
                _ => (2u8, String::new(), String::new()),
            };
            (pns.to_string(), prem.to_string(), rank, va, vb)
        })
        .collect();
    v.sort();
    v
}

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// Whether an annotation value sorts in the "string-like" bpv group — a plain
/// (`Simple`) literal, one explicitly typed `xsd:string`, or a language-tagged
/// literal. Within a predicate these sort together by value; only a genuinely
/// non-string typed literal (e.g. `xsd:anyURI`) sorts after them.
/// A literal value's datatype IRI; empty for an IRI value.
fn value_datatype(av: &AnnotationValue<RcStr>) -> String {
    match av {
        AnnotationValue::Literal(Literal::Datatype { datatype_iri, .. }) => {
            datatype_iri.as_ref().to_string()
        }
        // A literal written with no datatype is a plain one, and sorts ahead of
        // every `xsd:` type — including an explicitly `xsd:string`-typed literal
        // with the same text.
        AnnotationValue::Literal(Literal::Language { .. })
        | AnnotationValue::Literal(Literal::Simple { .. }) => {
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral".to_string()
        }
        _ => String::new(),
    }
}

fn is_xsd_string(av: &AnnotationValue<RcStr>) -> bool {
    match av {
        AnnotationValue::Literal(Literal::Simple { .. }) => true,
        AnnotationValue::Literal(Literal::Language { .. }) => true,
        AnnotationValue::Literal(Literal::Datatype { datatype_iri, .. }) => {
            datatype_iri.as_ref() == XSD_STRING
        }
        _ => false,
    }
}

/// An xref's annotation signature for ordering: its nested `basicPropertyValues`
/// under the same key the values themselves sort by, so an IRI-valued source
/// ranks before a literal one and the comparison within a kind is exact. Ordering
/// them as plain strings instead cannot satisfy both of EFO's cases at once —
/// `http://icb…` before `ISBN:…` (IRI before literal) and `MONDO:i2s` before
/// `i2s` (exact, not case-folded).
fn xref_meta_key(x: &Xref) -> Vec<(String, String, u8, String, String, String)> {
    let mut parts: Vec<_> = x
        .meta
        .as_ref()
        .map(|m| m.basic_property_values.iter().map(bpv_key).collect())
        .unwrap_or_default();
    parts.sort();
    parts
}

fn axiom_xrefs(anns: &std::collections::BTreeSet<horned_owl::model::Annotation<RcStr>>) -> Vec<String> {
    let mut out: Vec<(bool, String)> = anns
        .iter()
        .filter(|a| a.ap.0.as_ref() == format!("{OIO}hasDbXref"))
        .filter_map(|a| match &a.av {
            AnnotationValue::Literal(l) => Some((false, literal_text(l))),
            // A `hasDbXref` may be IRI-valued (`rdf:resource="…orcid…"`).
            AnnotationValue::IRI(i) => Some((true, i.as_ref().to_string())),
            _ => None,
        })
        .collect();
    // Annotation values order IRI-first, then literal; within each, by value — so
    // an ORCID `rdf:resource` xref precedes `ISBN:…`/`PMID:…` literals.
    //
    // An IRI compares as NAMESPACE then remainder, split at the NCName boundary,
    // not as a raw string. MP:0014518's definition carries
    // `…/10.1161/circ.105.9.e5` and `…/10.1161/01.CIR.0000132478.60674.D`: the
    // second's remainder starts with a digit, so its namespace runs on to
    // `…/10.1161/01.` and the shorter namespace sorts first, which is the opposite
    // of what comparing the two IRIs as strings gives.
    out.sort_by(|a, b| {
        (!a.0).cmp(&(!b.0)).then_with(|| {
            if a.0 && b.0 {
                crate::io::owlrdf::iri_key(&a.1).cmp(&crate::io::owlrdf::iri_key(&b.1))
            } else {
                a.1.cmp(&b.1)
            }
        })
    });
    out.into_iter().map(|(_, v)| v).collect()
}

#[derive(Default)]
struct EntityData {
    label: Option<String>,
    /// Comparison key `(value, annotation-list)` of the axiom the current `label`
    /// came from — a node carries one `lbl`, so of several `rdfs:label` axioms the
    /// one kept is the maximum by this key.
    label_key: (String, AnnKey),
    definition: Option<Definition>,
    /// Same idea for the winning `IAO_0000115` definition axiom.
    def_key: (String, AnnKey),
    comments: Vec<String>,
    subsets: Vec<String>,
    synonyms: Vec<Synonym>,
    xrefs: Vec<Xref>,
    deprecated: bool,
    bpv: Vec<Bpv>,
}


#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Class,
    ObjectProperty,
    Individual,
    AnnotationProperty,
    DataProperty,
}

fn kind_rank(k: Kind) -> u8 {
    // Nodes group by entity kind in this order: class < object property < data
    // property < named individual < annotation property; referenced-only
    // (typeless) IRIs come last.
    match k {
        Kind::Class => 0,
        Kind::ObjectProperty => 1,
        Kind::DataProperty => 2,
        Kind::Individual => 3,
        Kind::AnnotationProperty => 4,
    }
}

fn node_type_str(k: Kind) -> &'static str {
    match k {
        Kind::Class => "CLASS",
        Kind::ObjectProperty | Kind::AnnotationProperty | Kind::DataProperty => "PROPERTY",
        Kind::Individual => "INDIVIDUAL",
    }
}

fn property_type_str(k: Kind) -> &'static str {
    match k {
        Kind::ObjectProperty => "OBJECT",
        Kind::DataProperty => "DATA",
        Kind::AnnotationProperty => "ANNOTATION",
        _ => "",
    }
}

/// Write an ontology to OBO Graphs JSON. Nodes, edges and every axiom list are
/// totally ordered, so the same model always writes the same bytes.
pub fn save<W: Write>(model: &Model, writer: &mut W) -> Result<()> {
    let mut graph_id = String::new();
    let mut version = String::new();
    let mut kinds: BTreeMap<String, Kind> = BTreeMap::new();
    let mut data: BTreeMap<String, EntityData> = BTreeMap::new();
    let mut referenced: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut edges: Vec<Edge> = Vec::new();
    // `ObjectPropertyAssertion` edges: their own group, spliced in between the
    // class edges and `subPropertyOf` (OBI's `IAO_0000136` assertions sit after
    // the last `type` edge and before the first `subPropertyOf` one).
    let mut prop_edges: Vec<Edge> = Vec::new();
    let mut logical_defs: Vec<LogicalDefinition> = Vec::new();
    let mut equivalent_nodes_sets: Vec<EquivalentNodesSet> = Vec::new();
    let mut property_chain_axioms: Vec<PropertyChainAxiom> = Vec::new();
    let mut graph_bpv: Vec<Bpv> = Vec::new();
    // domainRangeAxioms: per-predicate domains (ObjectPropertyDomain), ranges
    // (ObjectPropertyRange) and allValuesFromEdges (SubClassOf → ObjectAllValues
    // From), collected per predicate IRI and ordered on the way out.
    let mut dr_map: BTreeMap<String, DrEntry> = BTreeMap::new();

    let ent = |data: &mut BTreeMap<String, EntityData>, iri: &str| -> () {
        data.entry(iri.to_string()).or_default();
    };
    let _ = ent;

    // A node whose CLASS kind comes only from being the named subclass of a
    // `SubClassOf` — nothing declares it and it is not in the signature — sits in a
    // tier of its own, after every entity the ontology names and before the
    // referenced-only typeless ones. `owl:Nothing` in a graph asserting
    // `SubClassOf(owl:Nothing owl:Nothing)` is the case that shows it.
    let mut subclass_typed: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Every entity the ontology names outright: in the signature, or carrying a
    // `Declaration`. A `subclass_typed` IRI absent from this is the tier-1 case.
    let mut named: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Node kinds come from the ontology SIGNATURE, not from `Declaration` axioms,
    // the same rule the RDF/XML writer applies. MONDO's `mondo-base.json` needs
    // `BFO_0000050`/`BFO_0000051` as `"type": "PROPERTY"` nodes even though the
    // step's `remove --select imports` stripped the import that declared them.
    // Seed the kinds from the signature first so a later real `Declaration` still
    // wins. Built-ins are excluded on the same rule as there.
    {
        let sig = crate::cmd::select::signature_entities(model);
        let builtin = |iri: &str| {
            iri.starts_with("http://www.w3.org/2001/XMLSchema#")
                || iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
                || iri.starts_with("http://www.w3.org/2002/07/owl#")
        };
        for (set, kind) in [
            (&sig.object_properties, Kind::ObjectProperty),
            (&sig.annotation_properties, Kind::AnnotationProperty),
            (&sig.data_properties, Kind::DataProperty),
        ] {
            for iri in set {
                if !builtin(iri) {
                    kinds.entry(iri.clone()).or_insert(kind);
                    named.insert(iri.clone());
                }
            }
        }
    }

    for ac in model.ont.iter() {
        match &ac.component {
            Component::OntologyID(id) => {
                if let Some(iri) = &id.iri {
                    graph_id = iri.as_ref().to_string();
                }
                if let Some(v) = &id.viri {
                    version = v.as_ref().to_string();
                }
            }
            Component::DeclareClass(d) => {
                kinds.insert(d.0 .0.as_ref().to_string(), Kind::Class);
                named.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareObjectProperty(d) => {
                kinds.insert(d.0 .0.as_ref().to_string(), Kind::ObjectProperty);
                named.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareAnnotationProperty(d) => {
                kinds
                    .entry(d.0 .0.as_ref().to_string())
                    .or_insert(Kind::AnnotationProperty);
                named.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareDataProperty(d) => {
                kinds
                    .entry(d.0 .0.as_ref().to_string())
                    .or_insert(Kind::DataProperty);
                named.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareNamedIndividual(d) => {
                kinds.insert(d.0 .0.as_ref().to_string(), Kind::Individual);
                named.insert(d.0 .0.as_ref().to_string());
            }
            Component::AnnotationAssertion(aa) => {
                let subj = match &aa.subject {
                    AnnotationSubject::IRI(s) => s.as_ref().to_string(),
                    _ => continue,
                };
                let prop = aa.ann.ap.0.as_ref().to_string();
                let e = data.entry(subj.clone()).or_default();
                // A definition's / synonym's source references are the
                // `hasDbXref` annotations ON THE AXIOM (the AnnotatedComponent),
                // not annotations nested on the value.
                let ax_xrefs = axiom_xrefs(&ac.ann);
                let (val_lit, val_is_iri) = match &aa.ann.av {
                    AnnotationValue::Literal(l) => (Some(literal_text(l)), false),
                    AnnotationValue::IRI(i) => (Some(i.as_ref().to_string()), true),
                    _ => (None, false),
                };
                let val_plain = is_xsd_string(&aa.ann.av);
                let val_datatype = value_datatype(&aa.ann.av);
                if prop == RDFS_LABEL {
                    if let Some(v) = val_lit {
                        // A node carries a single `lbl`, so of several `rdfs:label`
                        // axioms the one kept is the maximum by (value,
                        // annotation-list).
                        let key = (v.clone(), ann_sort_key(&ac.ann));
                        if e.label.is_none() || key > e.label_key {
                            e.label = Some(v);
                            e.label_key = key;
                        }
                    }
                } else if prop == IAO_DEF {
                    if let Some(v) = val_lit {
                        // A definition may be reified by several owl:Axiom blocks
                        // with different `hasDbXref` sources; a node carries a
                        // single definition, so the one kept is the maximum by
                        // (value, annotation-list).
                        let key = (v.clone(), ann_sort_key(&ac.ann));
                        if e.definition.is_none() || key > e.def_key {
                            e.definition = Some(Definition { val: v, xrefs: ax_xrefs, meta: nested_meta(&ac.ann) });
                            e.def_key = key;
                        }
                    }
                } else if let Some(sp) = synonym_pred(&prop) {
                    // Only a LITERAL synonym is a synonym. An IRI-valued
                    // `hasExactSynonym` — ECTO has one pointing at IAO_0000122 —
                    // falls through to `basicPropertyValues`, exactly as an
                    // IRI-valued `hasDbXref` does.
                    if val_is_iri {
                        if let Some(v) = val_lit {
                            e.bpv.push(Bpv {
                                pred: prop,
                                val: v,
                                xrefs: vec![],
                                meta: nested_meta(&ac.ann),
                                val_is_iri,
                                datatype: val_datatype.clone(),
                                plain: val_plain,
                            });
                        }
                    } else if let Some(v) = val_lit {
                        // An axiom may carry several `hasSynonymType` annotations,
                        // but a synonym has a single `synonymType` field, so the
                        // one kept is the maximum value.
                        let syn_type = ac
                            .ann
                            .iter()
                            .filter(|a| a.ap.0.as_ref() == format!("{OIO}hasSynonymType"))
                            .filter_map(|a| match &a.av {
                                AnnotationValue::IRI(i) => Some(i.as_ref().to_string()),
                                AnnotationValue::Literal(l) => Some(literal_text(l)),
                                _ => None,
                            })
                            .max()
                            .unwrap_or_default();
                        let ann_key = ann_sort_key(&ac.ann);
                        e.synonyms.push(Synonym {
                            synonym_type: syn_type,
                            pred: sp.to_string(),
                            val: v,
                            xrefs: ax_xrefs,
                            meta: nested_meta(&ac.ann),
                            ann_key,
                        });
                    }
                } else if prop == format!("{OIO}hasDbXref") {
                    // Only a LITERAL `hasDbXref` is an xref. An IRI-valued one
                    // (ECTO carries `<http://sweetontology.net/realmSoil/Permafrost>`
                    // on 715 classes) is not a literal, so it is recorded as a
                    // `basicPropertyValues` entry instead of an xref.
                    if let Some(v) = val_lit {
                        if val_is_iri {
                            e.bpv.push(Bpv {
                                pred: prop,
                                val: v,
                                xrefs: vec![],
                                meta: nested_meta(&ac.ann),
                                val_is_iri,
                                datatype: val_datatype.clone(),
                                plain: val_plain,
                            });
                        } else {
                            e.xrefs.push(Xref { val: v, meta: nested_meta(&ac.ann) });
                        }
                    }
                } else if prop == RDFS_COMMENT {
                    if let Some(v) = val_lit {
                        e.comments.push(v);
                    }
                } else if prop == format!("{OIO}inSubset") {
                    // An IRI-valued subset is its IRI; a literal-valued one is
                    // rendered as the literal's text, quoted.
                    if let Some(v) = val_lit {
                        e.subsets.push(if val_is_iri { v } else { format!("\"{v}\"") });
                    }
                } else if prop == OWL_DEPRECATED {
                    if matches!(val_lit.as_deref(), Some("true")) {
                        e.deprecated = true;
                    }
                } else if prop == format!("{OIO}id") {
                    // The OBO id is dropped: it is redundant with the node id.
                } else {
                    // A basicPropertyValue carries no xrefs of its own.
                    if let Some(v) = val_lit {
                        e.bpv.push(Bpv { pred: prop, val: v, xrefs: vec![], meta: nested_meta(&ac.ann), val_is_iri, datatype: val_datatype, plain: val_plain });
                    }
                }
            }
            Component::OntologyAnnotation(oa) => {
                let (val, is_iri) = match &oa.0.av {
                    AnnotationValue::Literal(l) => (literal_text(l), false),
                    AnnotationValue::IRI(i) => (i.as_ref().to_string(), true),
                    _ => continue,
                };
                graph_bpv.push(Bpv {
                    pred: oa.0.ap.0.as_ref().to_string(),
                    val,
                    xrefs: vec![],
                    meta: None,
                    val_is_iri: is_iri,
                    datatype: value_datatype(&oa.0.av),
                    plain: is_xsd_string(&oa.0.av),
                });
            }
            // An edge END is not a node. `FromOwl.generateGraph` types the NAMED
            // subclass of a `SubClassOf` and skips the axiom outright when the
            // subclass is anonymous; its `addEdge` never touches the node set, so
            // a superclass, a restriction's filler and an edge's predicate become
            // nodes only if something else — a declaration, an assertion, an
            // annotation — introduces them. Adding them here gave `owl:Nothing` a
            // node in the graphs where it is only ever an anonymous class's
            // superclass, and the reference has none.
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    let s = sub.0.as_ref().to_string();
                    // The named subclass is TYPED a class by the axiom itself, not
                    // merely referenced by it — `owl:Nothing` is a `CLASS` node in a
                    // graph that asserts `SubClassOf(owl:Nothing owl:Nothing)`, with
                    // nothing declaring it.
                    kinds.entry(s.clone()).or_insert(Kind::Class);
                    subclass_typed.insert(s.clone());
                    referenced.insert(s.clone());
                    match &sc.sup {
                        CE::Class(sup) => {
                            edges.push(Edge {
                                sub: s,
                                pred: "is_a".to_string(),
                                obj: sup.0.as_ref().to_string(),
                                meta: edge_meta(&ac.ann),
                            });
                        }
                        CE::ObjectSomeValuesFrom { ope, bce } => {
                            if let (OPE::ObjectProperty(r), CE::Class(t)) = (ope, bce.as_ref()) {
                                edges.push(Edge {
                                    sub: s,
                                    pred: r.0.as_ref().to_string(),
                                    obj: t.0.as_ref().to_string(),
                                    meta: edge_meta(&ac.ann),
                                });
                            }
                        }
                        // A universal restriction `A ⊑ ∀p.B` is an
                        // `allValuesFromEdge` under the predicate `p`.
                        CE::ObjectAllValuesFrom { ope, bce } => {
                            if let (OPE::ObjectProperty(r), CE::Class(t)) = (ope, bce.as_ref()) {
                                let p = r.0.as_ref().to_string();
                                dr_map.entry(p.clone()).or_default().all_values_from_edges.push(Edge {
                                    sub: s,
                                    pred: p,
                                    obj: t.0.as_ref().to_string(),
                                    meta: None,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            // `ClassAssertion(C i)` → a `type` edge (`{"sub": i, "pred": "type",
            // "obj": C}`). MONDO's IAO curation-status individuals (IAO_0000002 a
            // IAO_0000078, …) arrive via the OMO import.
            // `ObjectPropertyAssertion(p, a, b)` → an edge `a --p--> b`.
            Component::ObjectPropertyAssertion(opa) => {
                if let (OPE::ObjectProperty(p), Individual::Named(s), Individual::Named(o)) =
                    (&opa.ope, &opa.from, &opa.to)
                {
                    referenced.insert(s.0.as_ref().to_string());
                    referenced.insert(p.0.as_ref().to_string());
                    referenced.insert(o.0.as_ref().to_string());
                    prop_edges.push(Edge {
                        sub: s.0.as_ref().to_string(),
                        pred: p.0.as_ref().to_string(),
                        obj: o.0.as_ref().to_string(),
                        meta: None,
                    });
                }
            }
            Component::ClassAssertion(ca) => {
                if let (CE::Class(c), Individual::Named(i)) = (&ca.ce, &ca.i) {
                    let s = i.0.as_ref().to_string();
                    referenced.insert(s.clone());
                    referenced.insert(c.0.as_ref().to_string());
                    edges.push(Edge {
                        sub: s,
                        pred: "type".to_string(),
                        obj: c.0.as_ref().to_string(),
                        meta: edge_meta(&ac.ann),
                    });
                }
            }
            Component::EquivalentClasses(eq) => {
                // All-named equivalence → equivalentNodesSet; a defined class
                // ≡ genus ⊓ ∃p.filler ⊓ … → logicalDefinitionAxiom.
                if eq.0.iter().all(|c| matches!(c, CE::Class(_))) && eq.0.len() >= 2 {
                    let mut ids: Vec<String> = eq
                        .0
                        .iter()
                        .filter_map(|c| match c {
                            CE::Class(c) => Some(c.0.as_ref().to_string()),
                            _ => None,
                        })
                        .collect();
                    ids.sort();
                    equivalent_nodes_sets.push(EquivalentNodesSet {
                        representative_node_id: ids[0].clone(),
                        node_ids: ids,
                    });
                } else if let Some(mut ld) = logical_definition(&eq.0) {
                    let mut key: Vec<CE<RcStr>> = eq.0.iter().cloned().collect();
                    key.sort_by(crate::io::owlfunc::cmp_ce);
                    ld.order = key;
                    logical_defs.push(ld);
                }
            }
            Component::SubObjectPropertyOf(sp) => {
                match &sp.sub {
                    // A property chain `p1 ∘ p2 ⊑ q` → propertyChainAxiom.
                    horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(chain) => {
                        if let OPE::ObjectProperty(sup) = &sp.sup {
                            // A propertyChainAxiom carries only plain named
                            // properties. A chain with an `ObjectInverseOf(…)` member
                            // (e.g. `inverse(part_of) ∘ part_of ⊑ overlaps`) has no
                            // representation, and a shortened chain would assert
                            // something the ontology does not — so emit the axiom ONLY
                            // when EVERY member is a named object property.
                            let chain_ids: Vec<String> = chain
                                .iter()
                                .filter_map(|c| match c {
                                    OPE::ObjectProperty(p) => Some(p.0.as_ref().to_string()),
                                    _ => None,
                                })
                                .collect();
                            if chain_ids.len() == chain.len() {
                                property_chain_axioms.push(PropertyChainAxiom {
                                    predicate_id: sup.0.as_ref().to_string(),
                                    chain_predicate_ids: chain_ids,
                                });
                            }
                        }
                    }
                    // A simple `p ⊑ q` → a `subPropertyOf` edge.
                    horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(
                        OPE::ObjectProperty(sub),
                    ) => {
                        if let OPE::ObjectProperty(sup) = &sp.sup {
                            edges.push(Edge {
                                sub: sub.0.as_ref().to_string(),
                                pred: "subPropertyOf".to_string(),
                                obj: sup.0.as_ref().to_string(),
                                meta: edge_meta(&ac.ann),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Component::InverseObjectProperties(iop) => {
                // `InverseObjectProperties(p, q)` → an `inverseOf` edge.
                if let (OPE::ObjectProperty(sub), OPE::ObjectProperty(obj)) = (&iop.0, &iop.1) {
                    edges.push(Edge {
                        sub: sub.0.as_ref().to_string(),
                        pred: "inverseOf".to_string(),
                        obj: obj.0.as_ref().to_string(),
                        meta: edge_meta(&ac.ann),
                    });
                }
            }
            Component::ObjectPropertyDomain(d) => {
                if let (OPE::ObjectProperty(p), CE::Class(c)) = (&d.ope, &d.ce) {
                    let p = p.0.as_ref().to_string();
                    referenced.insert(p.clone());
                    referenced.insert(c.0.as_ref().to_string());
                    dr_map.entry(p).or_default().domain_class_ids.push(c.0.as_ref().to_string());
                }
            }
            Component::ObjectPropertyRange(r) => {
                if let (OPE::ObjectProperty(p), CE::Class(c)) = (&r.ope, &r.ce) {
                    let p = p.0.as_ref().to_string();
                    referenced.insert(p.clone());
                    referenced.insert(c.0.as_ref().to_string());
                    dr_map.entry(p).or_default().range_class_ids.push(c.0.as_ref().to_string());
                }
            }
            _ => {}
        }
    }

    // Every declared entity, plus every entity carrying annotations, plus every
    // referenced entity, is a node. Kind defaults to Class for a referenced-only
    // IRI (obographs' `type` is then omitted → typeless).
    // `owl:Thing` carries no exception. Being an edge's target does not make it a
    // node — but nothing about the implicit top is special here, and an annotation
    // assertion on it gives it a node of its own, typeless, since a built-in takes
    // no kind from the signature.
    let mut all: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    all.extend(kinds.keys().cloned());
    all.extend(data.keys().cloned());
    all.extend(referenced.iter().cloned());

    // (tier, kind_rank, ns, rem, node). Three tiers: everything the ontology names,
    // then the classes only a `SubClassOf` typed, then the referenced-only typeless.
    let mut nodes: Vec<(u8, u8, String, String, Node)> = Vec::new();
    for iri in &all {
        let kind = kinds.get(iri).copied();
        let (rank, type_str, prop_type) = match kind {
            Some(k) => (
                kind_rank(k),
                node_type_str(k).to_string(),
                property_type_str(k).to_string(),
            ),
            None => (5, String::new(), String::new()), // referenced-only → typeless, last
        };
        let tier = match kind {
            None => 2,
            Some(_) if subclass_typed.contains(iri) && !named.contains(iri) => 1,
            Some(_) => 0,
        };
        let d = data.get(iri);
        let meta = d.map(build_meta).filter(|m| !m.is_empty());
        let lbl = d.and_then(|d| d.label.clone()).unwrap_or_default();
        let (ns, rem) = ns_rem(iri);
        nodes.push((
            tier,
            rank,
            ns.to_string(),
            rem.to_string(),
            Node {
                id: iri.clone(),
                lbl,
                node_type: type_str,
                property_type: prop_type,
                meta,
            },
        ));
    }
    // A `SubClassOf` edge sorts by its subject's place in the KIND-ranked order —
    // the tier plays no part. `owl:Nothing` is last in the node list and its
    // `is_a` edge is not last among the edges; it sits where a class in the `owl#`
    // namespace belongs. So the two orders are taken separately.
    let mut edge_order: Vec<&(u8, u8, String, String, Node)> = nodes.iter().collect();
    edge_order.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)).then_with(|| a.3.cmp(&b.3)));
    let node_pos: BTreeMap<&str, usize> =
        edge_order.iter().enumerate().map(|(i, n)| (n.4.id.as_str(), i)).collect();
    let node_pos: BTreeMap<String, usize> =
        node_pos.into_iter().map(|(k, v)| (k.to_string(), v)).collect();

    nodes.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    let nodes: Vec<Node> = nodes.into_iter().map(|(_, _, _, _, n)| n).collect();

    // Edges group by the kind of axiom they came from: first every
    // SubClassOf-derived edge (`is_a` and property restrictions), then
    // `subPropertyOf` (SubObjectPropertyOf), then `inverseOf`
    // (InverseObjectProperties). Within the SubClassOf group an edge sorts by
    // subject (node order), then the superclass expression — a named `is_a`
    // superclass before a restriction — then by (pred, obj). The property-axiom
    // groups sort by their (subject, object) IRIs.
    let edge_group = |pred: &str| -> u8 {
        match pred {
            "subPropertyOf" => 1,
            "inverseOf" => 2,
            _ => 0,
        }
    };
    edges.sort_by(|a, b| {
        let ga = edge_group(&a.pred);
        let gb = edge_group(&b.pred);
        if ga != gb {
            return ga.cmp(&gb);
        }
        if ga == 0 {
            let pa = node_pos.get(a.sub.as_str()).copied().unwrap_or(usize::MAX);
            let pb = node_pos.get(b.sub.as_str()).copied().unwrap_or(usize::MAX);
            pa.cmp(&pb)
                .then_with(|| (a.pred != "is_a").cmp(&(b.pred != "is_a")))
                .then_with(|| a.pred.cmp(&b.pred))
                .then_with(|| a.obj.cmp(&b.obj))
                // Two edges can share (sub, pred, obj) and differ only in their
                // metadata — the asserted edge and its `is_inferred` twin. The bare
                // one comes first; without this the pair's order is whatever the
                // sort happened to leave (2,333 such pairs in `human-view.json`,
                // om splitting them roughly half and half).
                .then_with(|| a.meta.is_some().cmp(&b.meta.is_some()))
        } else {
            ns_rem(&a.sub)
                .cmp(&ns_rem(&b.sub))
                .then_with(|| ns_rem(&a.obj).cmp(&ns_rem(&b.obj)))
        }
    });

    if !prop_edges.is_empty() {
        prop_edges.sort_by(|a, b| {
            ns_rem(&a.sub)
                .cmp(&ns_rem(&b.sub))
                .then_with(|| ns_rem(&a.pred).cmp(&ns_rem(&b.pred)))
                .then_with(|| ns_rem(&a.obj).cmp(&ns_rem(&b.obj)))
        });
        let at = edges.iter().position(|e| edge_group(&e.pred) != 0).unwrap_or(edges.len());
        edges.splice(at..at, prop_edges);
    }

    // These axiom lists order by their defining/representative IRI, on the same
    // (namespace, remainder) key as entities.
    let nrk = |s: &str| -> (String, String) {
        let (a, b) = ns_rem(s);
        (a.to_string(), b.to_string())
    };
    // Two distinct EquivalentClasses axioms (e.g. differing only in annotations, or
    // one from the edit file and one from the `owl-axioms:` block) can yield the same
    // logicalDefinitionAxiom; the list carries each distinct definition once, so
    // dedupe on full content (definedClassId + genusIds + restrictions), keeping
    // first order.
    {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        logical_defs.retain(|ld| {
            let key = format!(
                "{}|{}|{}",
                ld.defined_class_id,
                ld.genus_ids.join(","),
                ld.restrictions
                    .iter()
                    .map(|r| format!("{}={}", r.property_id, r.filler_id))
                    .collect::<Vec<_>>()
                    .join(";")
            );
            seen.insert(key)
        });
    }
    logical_defs.sort_by(|a, b| {
        nrk(&a.defined_class_id).cmp(&nrk(&b.defined_class_id)).then_with(|| {
            for (x, y) in a.order.iter().zip(b.order.iter()) {
                let c = crate::io::owlfunc::cmp_ce(x, y);
                if c != std::cmp::Ordering::Equal {
                    return c;
                }
            }
            a.order.len().cmp(&b.order.len())
        })
    });
    equivalent_nodes_sets
        .sort_by(|a, b| nrk(&a.representative_node_id).cmp(&nrk(&b.representative_node_id)));
    // A propertyChainAxiom sorts on its property chain first, then on the
    // super-property, each IRI on the (namespace, remainder) key.
    let chain_key = |c: &PropertyChainAxiom| -> (Vec<(String, String)>, (String, String)) {
        (c.chain_predicate_ids.iter().map(|s| nrk(s)).collect(), nrk(&c.predicate_id))
    };
    property_chain_axioms.sort_by(|a, b| chain_key(a).cmp(&chain_key(b)));
    // A propertyChainAxiom records only the predicate and the chain, so two
    // SubPropertyChainOf axioms differing only in annotations collapse to one.
    property_chain_axioms
        .dedup_by(|a, b| a.predicate_id == b.predicate_id && a.chain_predicate_ids == b.chain_predicate_ids);

    // domainRangeAxioms appear in the order each predicate is first met while
    // walking the axioms in sorted order. `SubClassOf` (holding the `∀p.C`
    // restrictions → allValuesFromEdges) sorts ahead of
    // `ObjectPropertyDomain`/`ObjectPropertyRange`, so every predicate with an
    // allValuesFromEdge is encountered — and emitted — before any predicate with
    // only a domain/range, the former ordered by the least of their edges'
    // (subject, object) IRIs and the latter by the predicate IRI.
    let mut domain_range_axioms: Vec<DomainRangeAxiom> = dr_map
        .into_iter()
        .map(|(pred, mut e)| {
            e.all_values_from_edges.sort_by(|a, b| {
                ns_rem(&a.sub).cmp(&ns_rem(&b.sub)).then_with(|| ns_rem(&a.obj).cmp(&ns_rem(&b.obj)))
            });
            DomainRangeAxiom {
                predicate_id: pred,
                domain_class_ids: e.domain_class_ids,
                range_class_ids: e.range_class_ids,
                all_values_from_edges: e.all_values_from_edges,
            }
        })
        .collect();
    // First encounter is set by the earliest-sorting kind among a predicate's
    // contributing axioms: SubClassOf (allValuesFrom) < ObjectPropertyDomain <
    // ObjectPropertyRange. So predicates with an allValuesFromEdge come first
    // (ordered by their least edge (sub, obj)), then domain-bearing predicates,
    // then range-only predicates — the latter two by predicate IRI.
    let dr_key = |dra: &DomainRangeAxiom| -> (u8, (String, String), (String, String)) {
        if let Some(e) = dra.all_values_from_edges.first() {
            (0, nrk(&e.sub), nrk(&e.obj))
        } else if !dra.domain_class_ids.is_empty() {
            (1, nrk(&dra.predicate_id), (String::new(), String::new()))
        } else {
            (2, nrk(&dra.predicate_id), (String::new(), String::new()))
        }
    };
    domain_range_axioms.sort_by(|a, b| dr_key(a).cmp(&dr_key(b)));

    let meta = if graph_bpv.is_empty() && version.is_empty() {
        None
    } else {
        graph_bpv.sort_by(|a, b| bpv_key(a).cmp(&bpv_key(b)));
        Some(GraphMeta { basic_property_values: graph_bpv, version })
    };

    let doc = GraphDoc {
        graphs: vec![Graph {
            id: graph_id,
            meta,
            nodes,
            edges,
            equivalent_nodes_sets,
            logical_definition_axioms: logical_defs,
            domain_range_axioms,
            property_chain_axioms,
        }],
    };

    // The OBO Graphs pretty-print layout, not serde_json's.
    let mut ser = serde_json::Serializer::with_formatter(&mut *writer, JacksonFormatter::default());
    doc.serialize(&mut ser)?;
    Ok(())
}

fn build_meta(d: &EntityData) -> Meta {
    let mut synonyms = d.synonyms.clone();
    // Synonyms come out by (predicate, value, annotation-list) — the last being
    // the element-wise comparison over the axiom's xref/synonymType annotations.
    synonyms.sort_by(|a, b| {
        a.pred
            .cmp(&b.pred)
            .then_with(|| a.val.cmp(&b.val))
            // An ANNOTATED synonym precedes an otherwise-identical bare one. HPO
            // asserts the same text twice — once carrying `hasSynonymType`
            // (`hp#layperson`, `hp#abbreviation`, `hp#allelic_requirement`) and once
            // plain — and its released `hp-international.json` puts the typed member
            // first in all 7641 such adjacent pairs. A bare `Vec` compare gets this
            // backwards, since Rust (like any lexicographic list compare) orders the
            // EMPTY annotation key first.
            .then_with(|| a.ann_key.is_empty().cmp(&b.ann_key.is_empty()))
            .then_with(|| a.ann_key.cmp(&b.ann_key))
    });
    let mut xrefs: Vec<Xref> = d.xrefs.clone();
    // Two `hasDbXref` axioms can share a value and differ only in their axiom
    // annotations — CHEBI records `CAS:70458-96-7` once per source. Sorting on the
    // value alone leaves those in whatever order the axiom set iterated, so break
    // the tie on the annotation key, which ranks an IRI-valued source before a
    // literal one and compares the rest exactly.
    xrefs.sort_by(|a, b| a.val.cmp(&b.val).then_with(|| xref_meta_key(a).cmp(&xref_meta_key(b))));
    let mut bpv = d.bpv.clone();
    bpv.sort_by(|a, b| bpv_key(a).cmp(&bpv_key(b)));
    let mut subsets = d.subsets.clone();
    // Subsets sort an IRI value (rank 0) on its (namespace, remainder) key before
    // a literal value (rank 1, a `"…"` string).
    subsets.sort_by(|a, b| {
        let key = |s: &str| -> (u8, String, String) {
            if s.starts_with('"') {
                (1, s.to_string(), String::new())
            } else {
                let (n, r) = ns_rem(s);
                (0, n.to_string(), r.to_string())
            }
        };
        key(a).cmp(&key(b))
    });
    let mut comments = d.comments.clone();
    comments.sort();
    Meta {
        definition: d.definition.as_ref().map(|df| Definition {
            val: df.val.clone(),
            xrefs: df.xrefs.clone(),
            meta: df.meta.clone(),
        }),
        comments,
        subsets,
        synonyms,
        xrefs,
        basic_property_values: bpv,
        deprecated: d.deprecated,
    }
}

/// Turn an `EquivalentClasses` clique `[C, genus ⊓ ∃p.f ⊓ …]` into an obographs
/// logicalDefinitionAxiom, when it has that genus-differentia shape.
fn logical_definition(members: &[CE<RcStr>]) -> Option<LogicalDefinition> {
    if members.len() != 2 {
        return None;
    }
    let (named, expr) = match (&members[0], &members[1]) {
        (CE::Class(c), other) => (c, other),
        (other, CE::Class(c)) => (c, other),
        _ => return None,
    };
    let conjs: Vec<&CE<RcStr>> = match expr {
        CE::ObjectIntersectionOf(v) => v.iter().collect(),
        _ => return None,
    };
    let mut genus_ids = Vec::new();
    let mut restrictions = Vec::new();
    // A conjunct the logicalDefinitionAxiom shape cannot express is handled two
    // ways, and the difference is which one. A `someValuesFrom` over a COMPLEX
    // filler is simply dropped and the rest of the definition still stands —
    // HP_0000532, every one of whose conjuncts is `∃R.(nested intersection)`,
    // comes out as a bare `{"definedClassId": …}` with neither genus nor
    // restrictions. Any OTHER complex conjunct (a nested intersection, a
    // cardinality, a union) abandons the whole axiom: OBA_2045455 is
    // `(PATO_0001470 ⊓ ∃… ⊓ ∃…) ⊓ ∃RO_0002314.UBERON_0001062` and nothing is
    // emitted for it, not the one expressible restriction.
    for c in conjs {
        match c {
            CE::Class(g) => genus_ids.push(g.0.as_ref().to_string()),
            CE::ObjectSomeValuesFrom { ope, bce } => {
                if let (OPE::ObjectProperty(p), CE::Class(f)) = (ope, bce.as_ref()) {
                    restrictions.push(Restriction {
                        property_id: p.0.as_ref().to_string(),
                        filler_id: f.0.as_ref().to_string(),
                    });
                }
            }
            _ => return None,
        }
    }
    genus_ids.sort_by(|a, b| ns_rem(a).cmp(&ns_rem(b)));
    restrictions.sort_by(|a, b| {
        ns_rem(&a.property_id)
            .cmp(&ns_rem(&b.property_id))
            .then_with(|| ns_rem(&a.filler_id).cmp(&ns_rem(&b.filler_id)))
    });
    Some(LogicalDefinition {
        defined_class_id: named.0.as_ref().to_string(),
        genus_ids,
        restrictions,
        order: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// The serde_json formatter for the OBO Graphs pretty-print layout.
//
// Differences from serde_json's PrettyFormatter: object field separator is
// `" : "`; an array adds no newlines of its own, only spaces inside its brackets
// (`[ a, b ]`); only object nesting drives indentation (arrays are transparent).
// ---------------------------------------------------------------------------
struct JacksonFormatter {
    depth: usize,
    has_value: bool,
}
impl Default for JacksonFormatter {
    fn default() -> Self {
        JacksonFormatter { depth: 0, has_value: false }
    }
}
impl JacksonFormatter {
    fn indent<W: ?Sized + Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b"\n")?;
        for _ in 0..self.depth {
            w.write_all(b"  ")?;
        }
        Ok(())
    }
}
impl serde_json::ser::Formatter for JacksonFormatter {
    fn begin_object<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        w.write_all(b"{")
    }
    fn end_object<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.depth -= 1;
        if self.has_value {
            self.indent(w)?;
        } else {
            w.write_all(b" ")?;
        }
        self.has_value = true;
        w.write_all(b"}")
    }
    fn begin_object_key<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if !first {
            w.write_all(b",")?;
        }
        self.indent(w)
    }
    fn begin_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b" : ")
    }
    fn end_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.has_value = true;
        Ok(())
    }
    fn begin_array<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.has_value = false;
        w.write_all(b"[")
    }
    fn end_array<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.has_value = true;
        w.write_all(b" ]")
    }
    fn begin_array_value<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if first {
            w.write_all(b" ")
        } else {
            w.write_all(b", ")
        }
    }
    fn end_array_value<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.has_value = true;
        Ok(())
    }
}

/// Load an ontology from OBO Graphs JSON.
pub fn load<R: BufRead>(reader: R) -> Result<Model> {
    let doc: GraphDoc = serde_json::from_reader(reader)?;
    let b = Build::new();
    let mut ont: SetOntology<RcStr> = SetOntology::new();

    for graph in &doc.graphs {
        if !graph.id.is_empty() {
            ont.insert(Component::OntologyID(horned_owl::model::OntologyID {
                iri: Some(b.iri(graph.id.clone())),
                viri: None,
            }));
        }
        for node in &graph.nodes {
            let iri = normalize(&node.id);
            if node.node_type == "PROPERTY" {
                ont.insert(Component::DeclareObjectProperty(
                    horned_owl::model::DeclareObjectProperty(b.object_property(iri.clone())),
                ));
            } else {
                ont.insert(Component::DeclareClass(DeclareClass(b.class(iri.clone()))));
            }
            if !node.lbl.is_empty() {
                assert_label(&b, &mut ont, &iri, RDFS_LABEL, &node.lbl);
            }
            if let Some(meta) = &node.meta {
                if let Some(def) = &meta.definition {
                    assert_label(&b, &mut ont, &iri, IAO_DEF, &def.val);
                }
            }
        }
        for edge in &graph.edges {
            let sub = normalize(&edge.sub);
            let obj = normalize(&edge.obj);
            if edge.pred == "is_a" || edge.pred == "rdfs:subClassOf" {
                ont.insert(Component::SubClassOf(SubClassOf {
                    sub: CE::Class(b.class(sub)),
                    sup: CE::Class(b.class(obj)),
                }));
            } else {
                let pred = normalize(&edge.pred);
                ont.insert(Component::SubClassOf(SubClassOf {
                    sub: CE::Class(b.class(sub)),
                    sup: CE::ObjectSomeValuesFrom {
                        ope: OPE::ObjectProperty(b.object_property(pred)),
                        bce: Box::new(CE::Class(b.class(obj))),
                    },
                }));
            }
        }
    }
    Ok(Model::from_parts(ont, default_prefixes()))
}

/// OBO Graphs ids may be full IRIs or OBO CURIEs; normalize to a full IRI.
fn normalize(id: &str) -> String {
    if id.starts_with("http://") || id.starts_with("https://") {
        id.to_string()
    } else {
        expand_id(id)
    }
}

fn assert_label(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, prop: &str, value: &str) {
    ont.insert(Component::AnnotationAssertion(
        horned_owl::model::AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(subj)),
            ann: horned_owl::model::Annotation { ann: Default::default(),
                ap: b.annotation_property(prop),
                av: AnnotationValue::Literal(Literal::Simple {
                    literal: value.to_string(),
                }),
            },
        },
    ));
}

fn literal_text(lit: &Literal<RcStr>) -> String {
    match lit {
        Literal::Simple { literal }
        | Literal::Language { literal, .. }
        | Literal::Datatype { literal, .. } => literal.clone(),
    }
}
