//! OBO Graphs JSON support — the graph serialization used across the OBO
//! ecosystem (geneontology/obographs). An ontology is rendered as a set of
//! nodes (entities with metadata) and edges (subClassOf as `is_a`, and
//! existential relationships as property edges).
//!
//! The writer builds one graph per ontology of the imports closure, each from
//! that ontology's axioms taken in their sorted order (see [`generate_graph`]),
//! so the same ontology always writes the same bytes. The layout is the OBO
//! Graphs one: `" : "` key separators, bracketed arrays and the obographs
//! field order.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};

use anyhow::Result;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, Build, ClassExpression as CE, Component,
    DeclareClass, Individual, Literal, MutableOntology, ObjectPropertyExpression as OPE, RcStr, SubClassOf,
    SubObjectPropertyExpression as SOPE,
};
use horned_owl::ontology::set::SetOntology;
use serde::{Deserialize, Serialize};

use crate::io::obo::expand_id;
use crate::io::owlfunc::cmp_ce;
use crate::model::{default_prefixes, Model, XSD_BOOLEAN};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether per-element axiom annotations (an xref's / synonym's / definition's /
/// property-value's `source`, etc.) are nested as that element's own `meta`. It is
/// a byte-level convention a repo's existing releases either carry or do not:
/// ECTO's `ecto.json` carries the nested `meta`, while OBA's and MONDO's `.json`s
/// do not and would gain two million spurious lines with it on. Ingest resolves
/// which convention a repo builds under and records it as `Plan::emulate_robot_version`;
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
    meta: Option<Meta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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

/// One `domainRangeAxioms` entry: a predicate's domains, ranges and the
/// universal restrictions on it, each held once.
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

/// The metadata of a graph, a node, an edge or a property value.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    version: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    deprecated: bool,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
struct Definition {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    val: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    xrefs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Box<Meta>>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
struct Synonym {
    #[serde(rename = "synonymType", default, skip_serializing_if = "String::is_empty")]
    synonym_type: String,
    pred: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    val: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    xrefs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Box<Meta>>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
struct Xref {
    val: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Box<Meta>>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
struct Bpv {
    pred: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    val: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    xrefs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Box<Meta>>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
struct Edge {
    sub: String,
    pred: String,
    obj: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Meta>,
}

#[derive(Serialize, Deserialize, Default)]
struct EquivalentNodesSet {
    #[serde(rename = "representativeNodeId")]
    representative_node_id: String,
    #[serde(rename = "nodeIds")]
    node_ids: Vec<String>,
    /// The equivalence axiom's annotations, under the nested-`meta` convention
    /// (see `NEST_AXIOM_ANNS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Meta>,
}

#[derive(Serialize, Deserialize, Default)]
struct LogicalDefinition {
    #[serde(rename = "definedClassId")]
    defined_class_id: String,
    #[serde(rename = "genusIds", default, skip_serializing_if = "Vec::is_empty")]
    genus_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    restrictions: Vec<Restriction>,
}

#[derive(Serialize, Deserialize, Default)]
struct Restriction {
    #[serde(rename = "propertyId")]
    property_id: String,
    #[serde(rename = "fillerId")]
    filler_id: String,
}

// ---------------------------------------------------------------------------
// Building a graph
// ---------------------------------------------------------------------------

const OIO_ID: &str = "http://www.geneontology.org/formats/oboInOwl#id";
const HAS_DB_XREF: &str = "http://www.geneontology.org/formats/oboInOwl#hasDbXref";
const IN_SUBSET: &str = "http://www.geneontology.org/formats/oboInOwl#inSubset";
const HAS_SYNONYM_TYPE: &str = "http://www.geneontology.org/formats/oboInOwl#hasSynonymType";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// A node's `type` and `propertyType`.
type NodeType = (&'static str, &'static str);
const CLASS: NodeType = ("CLASS", "");
const INDIVIDUAL: NodeType = ("INDIVIDUAL", "");
const OBJECT_PROPERTY: NodeType = ("PROPERTY", "OBJECT");
const DATA_PROPERTY: NodeType = ("PROPERTY", "DATA");
const ANNOTATION_PROPERTY: NodeType = ("PROPERTY", "ANNOTATION");

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

/// One graph as it is built. A node takes its place when something first names
/// it and keeps the last type and label given to it; the edges and every axiom
/// list stand in the order they are added.
#[derive(Default)]
struct GraphBuilder {
    node_ids: Vec<String>,
    named: HashSet<String>,
    node_types: HashMap<String, NodeType>,
    labels: HashMap<String, String>,
    metas: HashMap<String, Meta>,
    edges: Vec<Edge>,
    equivalent_nodes_sets: Vec<EquivalentNodesSet>,
    logical_definitions: Vec<LogicalDefinition>,
    domain_range: Vec<DomainRangeAxiom>,
    domain_range_of: HashMap<String, usize>,
    property_chains: Vec<PropertyChainAxiom>,
}

impl GraphBuilder {
    fn add_node(&mut self, id: &str) {
        if self.named.insert(id.to_string()) {
            self.node_ids.push(id.to_string());
        }
    }

    fn set_type(&mut self, id: &str, node_type: NodeType) {
        self.add_node(id);
        self.node_types.insert(id.to_string(), node_type);
    }

    fn set_label(&mut self, id: &str, label: String) {
        self.add_node(id);
        self.labels.insert(id.to_string(), label);
    }

    /// The metadata of a node, naming the node.
    fn meta(&mut self, id: &str) -> &mut Meta {
        self.add_node(id);
        self.metas.entry(id.to_string()).or_default()
    }

    fn add_edge(&mut self, sub: &str, pred: &str, obj: &str, meta: &Meta) {
        self.edges.push(edge(sub, pred, obj, meta));
    }

    fn domain_range(&mut self, predicate: &str) -> &mut DomainRangeAxiom {
        let at = match self.domain_range_of.get(predicate) {
            Some(&at) => at,
            None => {
                self.domain_range.push(DomainRangeAxiom { predicate_id: predicate.to_string(), ..Default::default() });
                self.domain_range_of.insert(predicate.to_string(), self.domain_range.len() - 1);
                self.domain_range.len() - 1
            }
        };
        &mut self.domain_range[at]
    }

    fn build(mut self, id: String, meta: Option<Meta>) -> Graph {
        let nodes = self
            .node_ids
            .iter()
            .map(|n| {
                let (node_type, property_type) = self.node_types.get(n).copied().unwrap_or(("", ""));
                Node {
                    id: n.clone(),
                    lbl: self.labels.remove(n).unwrap_or_default(),
                    node_type: node_type.to_string(),
                    property_type: if node_type == "PROPERTY" { property_type.to_string() } else { String::new() },
                    meta: self.metas.remove(n).and_then(non_empty),
                }
            })
            .collect();
        Graph {
            id,
            meta,
            nodes,
            edges: self.edges,
            equivalent_nodes_sets: self.equivalent_nodes_sets,
            logical_definition_axioms: self.logical_definitions,
            domain_range_axioms: self.domain_range,
            property_chain_axioms: self.property_chains,
        }
    }
}

fn non_empty(meta: Meta) -> Option<Meta> {
    if meta == Meta::default() {
        None
    } else {
        Some(meta)
    }
}

fn edge(sub: &str, pred: &str, obj: &str, meta: &Meta) -> Edge {
    Edge { sub: sub.to_string(), pred: pred.to_string(), obj: obj.to_string(), meta: non_empty(meta.clone()) }
}

fn push_once(ids: &mut Vec<String>, id: &str) {
    if !ids.iter().any(|i| i == id) {
        ids.push(id.to_string());
    }
}

/// The id an individual's node takes: its IRI, or the node id of an anonymous
/// individual (`_:genid…`).
fn individual_id(i: &Individual<RcStr>) -> String {
    match i {
        Individual::Named(n) => n.0.as_ref().to_string(),
        Individual::Anonymous(a) => anonymous_id(a.0.as_ref()),
    }
}

fn anonymous_id(label: &str) -> String {
    if label.starts_with("_:") {
        label.to_string()
    } else {
        format!("_:{label}")
    }
}

/// An annotation value as a property value's `val`: an IRI, a literal's lexical
/// form, or an anonymous individual's node id.
fn value_text(av: &AnnotationValue<RcStr>) -> String {
    match av {
        AnnotationValue::IRI(i) => i.as_ref().to_string(),
        AnnotationValue::Literal(l) => literal_text(l),
        AnnotationValue::AnonymousIndividual(a) => anonymous_id(a.0.as_ref()),
    }
}

/// An annotation value as a subset or a synonym type names it: an IRI or an
/// anonymous individual's node id as it stands, a literal quoted, with its
/// language or, unless it is a plain literal, its datatype.
fn value_rendering(av: &AnnotationValue<RcStr>) -> String {
    let quoted = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let datatype = |iri: &str| -> String {
        for (prefix, ns) in [
            ("owl", "http://www.w3.org/2002/07/owl#"),
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
            ("xml", "http://www.w3.org/XML/1998/namespace"),
            ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ] {
            if let Some(local) = iri.strip_prefix(ns) {
                return format!("{prefix}:{local}");
            }
        }
        format!("<{iri}>")
    };
    match av {
        AnnotationValue::Literal(Literal::Language { literal, lang }) => format!("{}@{lang}", quoted(literal)),
        AnnotationValue::Literal(Literal::Simple { literal }) => {
            let dt = crate::io::owlrdf::plain_datatype();
            if dt == RDF_PLAIN_LITERAL {
                quoted(literal)
            } else {
                format!("{}^^{}", quoted(literal), datatype(dt))
            }
        }
        AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri }) => {
            if datatype_iri.as_ref() == RDF_PLAIN_LITERAL {
                quoted(literal)
            } else {
                format!("{}^^{}", quoted(literal), datatype(datatype_iri.as_ref()))
            }
        }
        _ => value_text(av),
    }
}

/// Whether an annotation marks what it annotates deprecated: `owl:deprecated`
/// with the boolean `true`. Any other value is a property value like any other.
fn is_deprecation(a: &Annotation<RcStr>) -> bool {
    a.ap.0.as_ref() == OWL_DEPRECATED
        && matches!(&a.av, AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri })
            if datatype_iri.as_ref() == XSD_BOOLEAN && literal.eq_ignore_ascii_case("true"))
}

/// Annotations in their order: by property, then by value.
fn sorted_annotations<'a>(anns: impl IntoIterator<Item = &'a Annotation<RcStr>>) -> Vec<&'a Annotation<RcStr>> {
    let mut sorted: Vec<&Annotation<RcStr>> = anns.into_iter().collect();
    sorted.sort_by(|a, b| {
        crate::owlapi_hash::iri_cmp(a.ap.0.as_ref(), b.ap.0.as_ref())
            .then_with(|| crate::io::owlfunc::cmp_annotation_value(&a.av, &b.av))
    });
    sorted
}

/// What annotations say about the axiom, node or ontology they annotate: an
/// `owl:deprecated true` deprecates it, a `hasDbXref` is an xref, an
/// `inSubset` or `hasSynonymType` a subset, and any other annotation a
/// property value.
fn annotations_meta<'a>(anns: impl IntoIterator<Item = &'a Annotation<RcStr>>) -> Meta {
    let mut meta = Meta::default();
    for a in sorted_annotations(anns) {
        let val = value_text(&a.av);
        let prop = a.ap.0.as_ref();
        if is_deprecation(a) {
            meta.deprecated = true;
        } else if prop == HAS_DB_XREF {
            meta.xrefs.push(Xref { val, meta: None });
        } else if prop == IN_SUBSET || prop == HAS_SYNONYM_TYPE {
            meta.subsets.push(val);
        } else {
            meta.basic_property_values.push(Bpv { pred: prop.to_string(), val, xrefs: Vec::new(), meta: None });
        }
    }
    meta
}

/// The metadata a node's definition, xref, synonym or property value carries
/// from the annotations of its assertion: their property values, under the
/// nested-`meta` convention (see `NEST_AXIOM_ANNS`).
fn value_meta(meta: &Meta) -> Option<Box<Meta>> {
    if !NEST_AXIOM_ANNS.load(Ordering::Relaxed) || meta.basic_property_values.is_empty() {
        return None;
    }
    Some(Box::new(Meta { basic_property_values: meta.basic_property_values.clone(), ..Default::default() }))
}

/// A defined class's logical definition from the operands of the intersection
/// it is equivalent to: a named operand is a genus and an existential
/// restriction on a named property to a named class a restriction. An
/// existential restriction of any other shape is left out and the definition
/// stands without it; an operand of any other kind leaves no definition.
fn logical_definition(defined: &str, operands: &[CE<RcStr>]) -> Option<LogicalDefinition> {
    let mut sorted: Vec<&CE<RcStr>> = operands.iter().collect();
    sorted.sort_by(|a, b| cmp_ce(a, b));
    sorted.dedup();
    let mut genus_ids = Vec::new();
    let mut restrictions = Vec::new();
    for operand in sorted {
        match operand {
            CE::Class(c) => genus_ids.push(c.0.as_ref().to_string()),
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
    Some(LogicalDefinition { defined_class_id: defined.to_string(), genus_ids, restrictions })
}

/// What an annotation assertion on `subject` writes into its node.
fn annotate(g: &mut GraphBuilder, subject: &str, ann: &Annotation<RcStr>, axiom: &AnnotatedComponent<RcStr>, meta: &Meta) {
    let prop = ann.ap.0.as_ref();
    let lexical = match &ann.av {
        AnnotationValue::Literal(l) => Some(literal_text(l)),
        _ => None,
    };
    let xrefs = || meta.xrefs.iter().map(|x| x.val.clone()).collect::<Vec<_>>();
    match (prop, lexical) {
        (RDFS_LABEL, Some(v)) => g.set_label(subject, v),
        (IAO_DEF, Some(v)) => {
            g.meta(subject).definition = Some(Definition { val: v, xrefs: xrefs(), meta: value_meta(meta) });
        }
        (HAS_DB_XREF, Some(v)) => g.meta(subject).xrefs.push(Xref { val: v, meta: value_meta(meta) }),
        _ if is_deprecation(ann) => g.meta(subject).deprecated = true,
        (RDFS_COMMENT, Some(v)) => g.meta(subject).comments.push(v),
        (OIO_ID, _) => {}
        (IN_SUBSET, _) => g.meta(subject).subsets.push(value_rendering(&ann.av)),
        (p, Some(v)) if synonym_pred(p).is_some() => {
            // Of several synonym types, the last in annotation order is the one
            // the synonym has.
            let synonym_type = sorted_annotations(&axiom.ann)
                .into_iter()
                .rfind(|a| a.ap.0.as_ref() == HAS_SYNONYM_TYPE)
                .map(|a| value_rendering(&a.av))
                .unwrap_or_default();
            let synonym = Synonym {
                synonym_type,
                pred: synonym_pred(p).unwrap_or_default().to_string(),
                val: v,
                xrefs: xrefs(),
                meta: value_meta(meta),
            };
            g.meta(subject).synonyms.push(synonym);
        }
        _ => {
            let bpv = Bpv { pred: prop.to_string(), val: value_text(&ann.av), xrefs: Vec::new(), meta: value_meta(meta) };
            g.meta(subject).basic_property_values.push(bpv);
        }
    }
}

/// Why an axiom keeps the ontology from being written as a graph, if it does:
/// an annotation of it whose value is an anonymous individual, which no `meta`
/// entry can hold, or a domain or range given to an inverse property, which
/// names no predicate.
fn unwritable_in_a_graph(ac: &AnnotatedComponent<RcStr>) -> Option<&'static str> {
    match &ac.component {
        Component::OntologyAnnotation(oa) if matches!(oa.0.av, AnnotationValue::AnonymousIndividual(_)) => {
            Some("annotates the ontology with an anonymous individual")
        }
        Component::ObjectPropertyDomain(d)
            if matches!(d.ope, OPE::InverseObjectProperty(_)) && matches!(d.ce, CE::Class(_)) =>
        {
            Some("states the domain of an inverse property")
        }
        Component::ObjectPropertyRange(r)
            if matches!(r.ope, OPE::InverseObjectProperty(_)) && matches!(r.ce, CE::Class(_)) =>
        {
            Some("states the range of an inverse property")
        }
        _ if ac.ann.iter().any(|a| matches!(a.av, AnnotationValue::AnonymousIndividual(_))) => {
            Some("is annotated with an anonymous individual")
        }
        _ => None,
    }
}

/// The graph of one ontology. Its axioms are taken in their sorted order, and
/// each adds what it says to the graph:
///
/// - a declaration types its entity's node;
/// - `SubClassOf` with a named subclass types it a class and is an `is_a` edge
///   to a named superclass, an edge on the property of an existential
///   restriction to a named class, or an `allValuesFromEdges` entry of a
///   universal one;
/// - a class assertion of a named class is a `type` edge, and names both
///   nodes; an object property assertion is an edge on its property, and names
///   its subject's node;
/// - an equivalence of named classes is an `equivalentNodesSets` entry, and
///   one of a named class and an intersection a logical definition;
/// - a sub-property, inverse pair or chain, domain or range of named
///   properties is an edge or an axiom entry;
/// - an annotation assertion on an IRI writes into that node.
///
/// Nothing else is written.
fn generate_graph(model: &Model) -> Graph {
    let mut graph_id = String::new();
    let mut version = String::new();
    let mut ontology_annotations: Vec<&Annotation<RcStr>> = Vec::new();
    let mut axioms: Vec<&AnnotatedComponent<RcStr>> = Vec::new();
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
            Component::OntologyAnnotation(oa) => ontology_annotations.push(&oa.0),
            Component::DocIRI(_) | Component::Import(_) => {}
            _ => axioms.push(ac),
        }
    }
    axioms.sort_by(|a, b| crate::io::genid::cmp_annotated_axiom(a, b));

    let mut g = GraphBuilder::default();
    for ac in axioms {
        let meta = annotations_meta(&ac.ann);
        match &ac.component {
            Component::DeclareClass(d) => g.set_type(d.0 .0.as_ref(), CLASS),
            Component::DeclareObjectProperty(d) => g.set_type(d.0 .0.as_ref(), OBJECT_PROPERTY),
            Component::DeclareDataProperty(d) => g.set_type(d.0 .0.as_ref(), DATA_PROPERTY),
            Component::DeclareAnnotationProperty(d) => g.set_type(d.0 .0.as_ref(), ANNOTATION_PROPERTY),
            Component::DeclareNamedIndividual(d) => g.set_type(d.0 .0.as_ref(), INDIVIDUAL),
            Component::SubClassOf(sc) => {
                let CE::Class(sub) = &sc.sub else { continue };
                let sub = sub.0.as_ref();
                g.set_type(sub, CLASS);
                match &sc.sup {
                    CE::Class(sup) => g.add_edge(sub, "is_a", sup.0.as_ref(), &meta),
                    CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(p), bce } => {
                        if let CE::Class(filler) = bce.as_ref() {
                            g.add_edge(sub, p.0.as_ref(), filler.0.as_ref(), &meta);
                        }
                    }
                    CE::ObjectAllValuesFrom { ope: OPE::ObjectProperty(p), bce } => {
                        if let CE::Class(filler) = bce.as_ref() {
                            let p = p.0.as_ref();
                            let e = edge(sub, p, filler.0.as_ref(), &meta);
                            let entry = g.domain_range(p);
                            if !entry.all_values_from_edges.contains(&e) {
                                entry.all_values_from_edges.push(e);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Component::ClassAssertion(ca) => {
                if let CE::Class(c) = &ca.ce {
                    let i = individual_id(&ca.i);
                    g.add_edge(&i, "type", c.0.as_ref(), &meta);
                    g.add_node(&i);
                    g.add_node(c.0.as_ref());
                }
            }
            Component::ObjectPropertyAssertion(opa) => {
                let from = individual_id(&opa.from);
                if let OPE::ObjectProperty(p) = &opa.ope {
                    g.add_edge(&from, p.0.as_ref(), &individual_id(&opa.to), &meta);
                }
                g.add_node(&from);
            }
            Component::EquivalentClasses(eq) => {
                let mut members: Vec<&CE<RcStr>> = eq.0.iter().collect();
                members.sort_by(|a, b| cmp_ce(a, b));
                members.dedup();
                let named: Vec<&str> = members
                    .iter()
                    .filter_map(|m| match m {
                        CE::Class(c) => Some(c.0.as_ref()),
                        _ => None,
                    })
                    .collect();
                let anonymous: Vec<&CE<RcStr>> = members.iter().copied().filter(|m| !matches!(m, CE::Class(_))).collect();
                if anonymous.is_empty() {
                    let Some(first) = named.first() else { continue };
                    g.equivalent_nodes_sets.push(EquivalentNodesSet {
                        representative_node_id: first.to_string(),
                        node_ids: named.iter().map(|n| n.to_string()).collect(),
                        meta: if NEST_AXIOM_ANNS.load(Ordering::Relaxed) { non_empty(meta.clone()) } else { None },
                    });
                } else if let ([defined], [CE::ObjectIntersectionOf(operands)]) = (named.as_slice(), anonymous.as_slice()) {
                    if let Some(ld) = logical_definition(defined, operands) {
                        g.logical_definitions.push(ld);
                    }
                }
            }
            Component::SubObjectPropertyOf(sp) => match (&sp.sub, &sp.sup) {
                (SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)), OPE::ObjectProperty(sup)) => {
                    g.add_edge(sub.0.as_ref(), "subPropertyOf", sup.0.as_ref(), &meta);
                }
                (SOPE::ObjectPropertyChain(chain), OPE::ObjectProperty(sup)) => {
                    let ids: Option<Vec<String>> = chain
                        .iter()
                        .map(|p| match p {
                            OPE::ObjectProperty(p) => Some(p.0.as_ref().to_string()),
                            OPE::InverseObjectProperty(_) => None,
                        })
                        .collect();
                    if let Some(ids) = ids {
                        g.property_chains.push(PropertyChainAxiom {
                            predicate_id: sup.0.as_ref().to_string(),
                            chain_predicate_ids: ids,
                        });
                    }
                }
                _ => {}
            },
            Component::InverseObjectProperties(iop) => {
                if let (OPE::ObjectProperty(p), OPE::ObjectProperty(q)) = (&iop.0, &iop.1) {
                    g.add_edge(p.0.as_ref(), "inverseOf", q.0.as_ref(), &meta);
                }
            }
            Component::ObjectPropertyDomain(d) => {
                if let (OPE::ObjectProperty(p), CE::Class(c)) = (&d.ope, &d.ce) {
                    push_once(&mut g.domain_range(p.0.as_ref()).domain_class_ids, c.0.as_ref());
                }
            }
            Component::ObjectPropertyRange(r) => {
                if let (OPE::ObjectProperty(p), CE::Class(c)) = (&r.ope, &r.ce) {
                    push_once(&mut g.domain_range(p.0.as_ref()).range_class_ids, c.0.as_ref());
                }
            }
            Component::AnnotationAssertion(aa) => {
                if let AnnotationSubject::IRI(subject) = &aa.subject {
                    annotate(&mut g, subject.as_ref(), &aa.ann, ac, &meta);
                }
            }
            _ => {}
        }
    }
    let mut graph_meta = annotations_meta(ontology_annotations);
    graph_meta.version = version;
    g.build(graph_id, non_empty(graph_meta))
}

/// The order of the graphs of an imports closure: by ontology id, as its text
/// `OntologyID(OntologyIRI(<iri>) VersionIRI(<version>))` sorts, an anonymous
/// ontology first.
fn graph_order_key(model: &Model) -> (bool, Vec<u16>) {
    let id = model.ont.iter().find_map(|ac| match &ac.component {
        Component::OntologyID(id) => id.iri.as_ref().map(|iri| (iri, id.viri.as_ref())),
        _ => None,
    });
    match id {
        Some((iri, version)) => {
            let text = format!(
                "OntologyID(OntologyIRI(<{}>) VersionIRI(<{}>))",
                iri.as_ref(),
                version.map_or("null", |v| v.as_ref())
            );
            (true, text.encode_utf16().collect())
        }
        None => (false, Vec::new()),
    }
}

/// Write an ontology to OBO Graphs JSON: a graph for the ontology and one for
/// each ontology of its imports closure (`imports`), in the order of their
/// ontology ids.
pub fn save<W: Write>(model: &Model, writer: &mut W) -> Result<()> {
    save_closure(model, &[], writer)
}

/// [`save`], with the documents of the ontology's imports closure.
pub fn save_closure<W: Write>(model: &Model, imports: &[Model], writer: &mut W) -> Result<()> {
    let mut ontologies: Vec<&Model> = std::iter::once(model).chain(imports.iter()).collect();
    for ont in &ontologies {
        if let Some((ac, why)) = ont.ont.iter().find_map(|ac| unwritable_in_a_graph(ac).map(|why| (ac, why))) {
            anyhow::bail!(
                "the ontology cannot be written as an OBO graph: {} {why}",
                crate::io::owlfunc::render_component_line(ac)
            );
        }
    }
    ontologies.sort_by_cached_key(|m| graph_order_key(m));
    let doc = GraphDoc { graphs: ontologies.into_iter().map(generate_graph).collect() };

    // The OBO Graphs pretty-print layout, not serde_json's.
    let mut ser = serde_json::Serializer::with_formatter(&mut *writer, JacksonFormatter::default());
    doc.serialize(&mut ser)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The serde_json formatter for the OBO Graphs pretty-print layout.
//
// Differences from serde_json's PrettyFormatter: object field separator is
// `" : "`; an array adds no newlines of its own, only spaces inside its brackets
// (`[ a, b ]`); only object nesting drives indentation (arrays are transparent).
// In a string, a control character is escaped in upper-case hex (`\u001F`), and
// a character outside the Basic Multilingual Plane as its two UTF-16 surrogates
// (`\uD83D\uDE00`).
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
    fn write_string_fragment<W: ?Sized + Write>(&mut self, w: &mut W, fragment: &str) -> std::io::Result<()> {
        let mut start = 0;
        for (i, c) in fragment.char_indices() {
            if u32::from(c) > 0xFFFF {
                w.write_all(&fragment.as_bytes()[start..i])?;
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    write!(w, "\\u{unit:04X}")?;
                }
                start = i + c.len_utf8();
            }
        }
        w.write_all(&fragment.as_bytes()[start..])
    }
    fn write_char_escape<W: ?Sized + Write>(
        &mut self,
        w: &mut W,
        escape: serde_json::ser::CharEscape,
    ) -> std::io::Result<()> {
        use serde_json::ser::CharEscape;
        match escape {
            CharEscape::Quote => w.write_all(b"\\\""),
            CharEscape::ReverseSolidus => w.write_all(b"\\\\"),
            CharEscape::Solidus => w.write_all(b"\\/"),
            CharEscape::Backspace => w.write_all(b"\\b"),
            CharEscape::FormFeed => w.write_all(b"\\f"),
            CharEscape::LineFeed => w.write_all(b"\\n"),
            CharEscape::CarriageReturn => w.write_all(b"\\r"),
            CharEscape::Tab => w.write_all(b"\\t"),
            CharEscape::AsciiControl(byte) => write!(w, "\\u{byte:04X}"),
        }
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
