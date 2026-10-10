//! The entities an ontology document mentions, by kind: what the OWL/XML
//! writer declares when the ontology did not, and what the Manchester writer
//! writes a frame for.
//!
//! The signature is every entity an axiom or an ontology annotation refers to —
//! in the axiom itself, in its annotations at any depth, and the datatype of
//! every literal (an untyped literal's being `rdf:PlainLiteral`, or `xsd:string`
//! in a document that types them). An annotation assertion's subject and an IRI
//! annotation value are IRIs, not entities, and are not in it.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

use horned_owl::model::{
    AnnotationProperty, Class, Component, DataProperty, Datatype, Literal, NamedIndividual,
    ObjectProperty, RcStr,
};
use horned_owl::visitor::immutable::{Visit, Walk};

use crate::model::Model;

const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// The label an entity is named by: the first literal among its `rdfs:label`
/// assertions, in the order its assertions are held; an IRI value stands until
/// a literal follows it.
pub(crate) enum HeldLabel<'m> {
    Literal(&'m str),
    Iri(&'m str),
}

impl HeldLabel<'_> {
    /// The label as a label provider shows it: a literal's text, or the short
    /// form of an IRI.
    pub(crate) fn short_form(&self) -> String {
        match self {
            HeldLabel::Literal(text) => text.to_string(),
            HeldLabel::Iri(iri) => crate::owlapi_hash::iri_short_form(iri),
        }
    }
}

/// The label of every entity of `model` that has one (see [`HeldLabel`]).
///
/// An entity's labels are taken in the iteration order of its own set of
/// annotation assertions: by the axioms' hashes, not by document order and not
/// by the values. That set is sized by how many annotation assertions the
/// entity carries, and two of its members in one bucket stand in the order the
/// ontology's set of every annotation assertion holds them. `oboInOwl:hasDbXref`
/// carries both "database_cross_reference" and "has cross-reference", and the
/// order decides which one names it in every artefact.
///
/// Two labels in one bucket at both levels stand in the order the document
/// added them, which it does not record; the lexically smaller is taken first,
/// so the pick is the same from one run to the next.
pub(crate) fn held_labels(model: &Model) -> HashMap<&str, HeldLabel<'_>> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Component};
    const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
    let mut assertions = 0usize;
    let mut per_subject: HashMap<&str, usize> = HashMap::new();
    let mut candidates: HashMap<&str, Vec<(i32, &AnnotationValue<RcStr>)>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            assertions += 1;
            if let AnnotationSubject::IRI(s) = &aa.subject {
                *per_subject.entry(s.as_ref()).or_default() += 1;
                if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                    candidates.entry(s.as_ref()).or_default().push((
                        crate::owlapi_hash::annotation_assertion_hash(
                            s.as_ref(),
                            RDFS_LABEL,
                            &aa.ann.av,
                            &ac.ann,
                            model.natural_order(),
                        ),
                        &aa.ann.av,
                    ));
                }
            }
        }
    }
    let text = |av: &AnnotationValue<RcStr>| -> String {
        match av {
            AnnotationValue::Literal(l) => l.literal().to_string(),
            AnnotationValue::IRI(iri) => iri.to_string(),
            AnnotationValue::AnonymousIndividual(a) => a.0.to_string(),
        }
    };
    let mut labels: HashMap<&str, HeldLabel> = HashMap::new();
    for (subject, mut cands) in candidates {
        cands.sort_by_key(|c| text(c.1));
        let ordered: Vec<usize> = if cands.len() == 1 {
            vec![0]
        } else {
            let hashes: Vec<i32> = cands.iter().map(|c| c.0).collect();
            crate::owlapi_hash::subject_assertion_order(&hashes, per_subject[subject], assertions)
        };
        let mut found: Option<HeldLabel> = None;
        for i in ordered {
            match cands[i].1 {
                AnnotationValue::Literal(l) => {
                    found = Some(HeldLabel::Literal(l.literal().as_str()));
                    break;
                }
                AnnotationValue::IRI(iri) => found = Some(HeldLabel::Iri(iri.as_ref())),
                AnnotationValue::AnonymousIndividual(_) => {}
            }
        }
        if let Some(f) = found {
            labels.insert(subject, f);
        }
    }
    labels
}

/// The node ID an anonymous individual is written with: its label, after `_:`.
pub fn node_id(label: &str) -> Cow<'_, str> {
    if label.starts_with("_:") {
        Cow::Borrowed(label)
    } else {
        Cow::Owned(format!("_:{label}"))
    }
}

/// The kind of a named entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Class,
    ObjectProperty,
    DataProperty,
    NamedIndividual,
    AnnotationProperty,
    Datatype,
}

impl Kind {
    /// The kind's index in the natural order of OWL objects.
    pub fn index(self) -> i32 {
        match self {
            Kind::Class => 1001,
            Kind::ObjectProperty => 1002,
            Kind::DataProperty => 1004,
            Kind::NamedIndividual => 1005,
            Kind::AnnotationProperty => 1006,
            Kind::Datatype => 4001,
        }
    }

    /// The kind as horned-owl names it.
    pub fn named(self) -> horned_owl::model::NamedOWLEntityKind {
        use horned_owl::model::NamedOWLEntityKind as N;
        match self {
            Kind::Class => N::Class,
            Kind::ObjectProperty => N::ObjectProperty,
            Kind::DataProperty => N::DataProperty,
            Kind::NamedIndividual => N::NamedIndividual,
            Kind::AnnotationProperty => N::AnnotationProperty,
            Kind::Datatype => N::Datatype,
        }
    }

    /// The seed of an entity's content hash: `seed * 31 + hash(IRI)`.
    fn hash_seed(self) -> i32 {
        match self {
            Kind::Class => 2293,
            Kind::ObjectProperty => 4153,
            Kind::DataProperty => 4073,
            Kind::NamedIndividual => 4327,
            Kind::AnnotationProperty => 6067,
            Kind::Datatype => 3911,
        }
    }

    /// An entity's content hash, which places it in a hash set.
    pub fn hash(self, iri: &str) -> i32 {
        self.hash_seed()
            .wrapping_mul(31)
            .wrapping_add(crate::owlapi_hash::iri_hash(iri))
    }
}

/// Whether an entity is part of the OWL, RDF or XSD vocabulary itself — the
/// top and bottom entities, the built-in annotation properties, and the datatypes
/// of the OWL 2 datatype map. A built-in entity is never declared.
pub fn is_builtin(kind: Kind, iri: &str) -> bool {
    let local = |ns: &str| iri.strip_prefix(ns);
    match kind {
        Kind::Class => matches!(local(OWL), Some("Thing" | "Nothing")),
        Kind::ObjectProperty => matches!(local(OWL), Some("topObjectProperty" | "bottomObjectProperty")),
        Kind::DataProperty => matches!(local(OWL), Some("topDataProperty" | "bottomDataProperty")),
        Kind::NamedIndividual => false,
        Kind::AnnotationProperty => {
            matches!(local(RDFS), Some("label" | "comment" | "seeAlso" | "isDefinedBy"))
                || matches!(
                    local(OWL),
                    Some("versionInfo" | "backwardCompatibleWith" | "priorVersion" | "incompatibleWith" | "deprecated")
                )
        }
        Kind::Datatype => {
            matches!(local(RDF), Some("XMLLiteral" | "PlainLiteral" | "langString"))
                || matches!(local(RDFS), Some("Literal"))
                || matches!(local(OWL), Some("real" | "rational"))
                || matches!(
                    local(XSD),
                    Some(
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
                )
        }
    }
}

struct Collect {
    plain_typed: bool,
    out: BTreeSet<(Kind, String)>,
}

impl Visit<RcStr> for Collect {
    fn visit_class(&mut self, e: &Class<RcStr>) {
        self.out.insert((Kind::Class, e.0.as_ref().to_string()));
    }
    fn visit_object_property(&mut self, e: &ObjectProperty<RcStr>) {
        self.out.insert((Kind::ObjectProperty, e.0.as_ref().to_string()));
    }
    fn visit_data_property(&mut self, e: &DataProperty<RcStr>) {
        self.out.insert((Kind::DataProperty, e.0.as_ref().to_string()));
    }
    fn visit_named_individual(&mut self, e: &NamedIndividual<RcStr>) {
        self.out.insert((Kind::NamedIndividual, e.0.as_ref().to_string()));
    }
    fn visit_annotation_property(&mut self, e: &AnnotationProperty<RcStr>) {
        self.out.insert((Kind::AnnotationProperty, e.0.as_ref().to_string()));
    }
    fn visit_datatype(&mut self, e: &Datatype<RcStr>) {
        self.out.insert((Kind::Datatype, e.0.as_ref().to_string()));
    }
    fn visit_literal(&mut self, l: &Literal<RcStr>) {
        let dt = match l {
            Literal::Datatype { datatype_iri, .. } => datatype_iri.as_ref().to_string(),
            Literal::Language { .. } => format!("{RDF}PlainLiteral"),
            Literal::Simple { .. } if self.plain_typed => format!("{XSD}string"),
            Literal::Simple { .. } => format!("{RDF}PlainLiteral"),
        };
        self.out.insert((Kind::Datatype, dt));
    }
}

/// The entities `model` mentions, each with its kind. An IRI used as two kinds
/// is two entities.
pub fn signature(model: &Model) -> BTreeSet<(Kind, String)> {
    signature_of(model, model.ont.iter())
}

/// The entities the root document of `model` mentions, leaving out what only
/// its imports lend.
pub fn root_signature(model: &Model) -> BTreeSet<(Kind, String)> {
    signature_of(model, model.ont.iter().filter(|ac| !model.imported_components.contains(*ac)))
}

fn signature_of<'a>(
    model: &Model,
    components: impl Iterator<Item = &'a horned_owl::model::AnnotatedComponent<RcStr>>,
) -> BTreeSet<(Kind, String)> {
    let mut walk = Walk::new(Collect { plain_typed: model.plain_literals_typed, out: BTreeSet::new() });
    for ac in components {
        match &ac.component {
            Component::OntologyID(_) | Component::DocIRI(_) | Component::Import(_) => {}
            _ => walk.annotated_component(ac),
        }
    }
    walk.into_visit().out
}

/// The entity a declaration declares.
pub fn declaration(c: &Component<RcStr>) -> Option<(Kind, &str)> {
    match c {
        Component::DeclareClass(e) => Some((Kind::Class, e.0.as_ref())),
        Component::DeclareObjectProperty(e) => Some((Kind::ObjectProperty, e.0.as_ref())),
        Component::DeclareDataProperty(e) => Some((Kind::DataProperty, e.0.as_ref())),
        Component::DeclareNamedIndividual(e) => Some((Kind::NamedIndividual, e.0.as_ref())),
        Component::DeclareAnnotationProperty(e) => Some((Kind::AnnotationProperty, e.0.as_ref())),
        Component::DeclareDatatype(e) => Some((Kind::Datatype, e.0.as_ref())),
        _ => None,
    }
}

/// The entities `model` declares itself.
pub fn declared(model: &Model) -> HashSet<(Kind, String)> {
    model
        .ont
        .iter()
        .filter_map(|ac| declaration(&ac.component))
        .map(|(kind, iri)| (kind, iri.to_string()))
        .collect()
}

/// IRIs whose entities cannot be declared together: an IRI that is two of
/// object, data and annotation property, or both a class and a datatype.
/// Individuals never count.
pub fn illegal_punnings(signature: &BTreeSet<(Kind, String)>) -> HashSet<String> {
    let mut kinds: HashMap<&str, Vec<Kind>> = HashMap::new();
    for (k, iri) in signature {
        if *k != Kind::NamedIndividual {
            kinds.entry(iri.as_str()).or_default().push(*k);
        }
    }
    kinds
        .into_iter()
        .filter(|(_, ks)| {
            let has = |k: Kind| ks.contains(&k);
            (has(Kind::ObjectProperty) && has(Kind::AnnotationProperty))
                || (has(Kind::DataProperty) && has(Kind::AnnotationProperty))
                || (has(Kind::DataProperty) && has(Kind::ObjectProperty))
                || (has(Kind::Datatype) && has(Kind::Class))
        })
        .map(|(iri, _)| iri.to_string())
        .collect()
}

/// The `kind\0IRI` key of an entity in `Model::imports_closure`.
pub(crate) fn closure_key(kind: Kind, iri: &str) -> String {
    format!("{}\0{iri}", key_name(kind))
}

/// The kind a [`closure_key`] names.
fn key_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Class => "class",
        Kind::ObjectProperty => "op",
        Kind::DataProperty => "dp",
        Kind::NamedIndividual => "ni",
        Kind::AnnotationProperty => "ap",
        Kind::Datatype => "dt",
    }
}

/// The entity a [`closure_key`] names.
fn key_entity(key: &str) -> Option<(Kind, String)> {
    let (name, iri) = key.split_once('\0')?;
    let kind = [
        Kind::Class,
        Kind::ObjectProperty,
        Kind::DataProperty,
        Kind::NamedIndividual,
        Kind::AnnotationProperty,
        Kind::Datatype,
    ]
    .into_iter()
    .find(|k| key_name(*k) == name)?;
    Some((kind, iri.to_string()))
}

/// Whether a document written from `model` in RDF/XML or Turtle has to state
/// the type of `iri` as a `kind` itself: the entity is not built in, the
/// ontology does not declare it (`declared`, from [`declared`]), and no
/// ontology it imports has it in its signature.
pub fn missing_type(model: &Model, declared: &HashSet<(Kind, String)>, kind: Kind, iri: &str) -> bool {
    !is_builtin(kind, iri)
        && !declared.contains(&(kind, iri.to_string()))
        && !model.imports_have(&closure_key(kind, iri))
}

/// The entities a document written from `model` in functional syntax or
/// OWL/XML declares although the ontology does not: every entity of the
/// ontology's signature and its imports closure's that is not built in, not
/// illegally punned across the two, and declared by neither. An ontology that
/// imports declares none while its closure is unread.
///
/// They come in the order a hash set built from that signature holds them: by
/// bucket, the table sized for the signature (at least 16 slots, a power of
/// two holding it at 3/4 load), ties in natural order.
pub fn missing_declarations(model: &Model) -> Vec<(Kind, String)> {
    use horned_owl::model::Component;
    let mut sig = signature(model);
    let mut declared = declared(model);
    let imports = model.ont.iter().any(|ac| matches!(ac.component, Component::Import(_)));
    if imports {
        let Some(closure) = &model.imports_closure else {
            return Vec::new();
        };
        sig.extend(closure.signature.iter().filter_map(|k| key_entity(k)));
        declared.extend(closure.declared.iter().filter_map(|k| key_entity(k)));
    }
    let illegal = illegal_punnings(&sig);
    let mut cap = 16usize;
    let want = ((sig.len() as f64) / 0.75) as usize + 1;
    while cap < want {
        cap <<= 1;
    }
    let bucket = |k: Kind, iri: &str| -> u32 {
        let h = k.hash(iri) as u32;
        (h ^ (h >> 16)) & (cap as u32 - 1)
    };
    let mut out: Vec<(Kind, String)> = sig
        .into_iter()
        .filter(|(k, iri)| {
            !is_builtin(*k, iri) && !declared.contains(&(*k, iri.clone())) && !illegal.contains(iri)
        })
        .collect();
    out.sort_by(|(ka, a), (kb, b)| {
        bucket(*ka, a)
            .cmp(&bucket(*kb, b))
            .then_with(|| ka.index().cmp(&kb.index()))
            .then_with(|| crate::io::natural_order::iri_cmp(a, b))
    });
    out
}
