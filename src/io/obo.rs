//! OBO 1.4 flat-file format support, implementing the OBO ↔ OWL 2 mapping for
//! the constructs used by CL/UBERON/MONDO and the rest of the OBO library.
//!
//! Reader: parses header + `[Term]`/`[Typedef]`/`[Instance]` stanzas into
//! horned-owl axioms.
//! Writer: renders an ontology back to OBO for the OBO-expressible fragment.
//!
//! ID expansion follows the document's `idspace:` declarations, then the standard
//! OBO PURL convention:
//! `PREFIX:LOCAL` ⇄ `http://purl.obolibrary.org/obo/PREFIX_LOCAL`.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::{BufRead, Write};

use anyhow::Result;
use horned_owl::model::{
    Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, AsymmetricObjectProperty,
    Build, ClassAssertion, ClassExpression as CE, Component, DeclareAnnotationProperty,
    DeclareClass, DeclareNamedIndividual, DeclareObjectProperty, DisjointClasses,
    EquivalentClasses, FunctionalObjectProperty, Individual, InverseFunctionalObjectProperty,
    InverseObjectProperties, Literal, MutableOntology, ObjectPropertyAssertion,
    ObjectPropertyDomain, ObjectPropertyExpression as OPE, ObjectPropertyRange,
    ReflexiveObjectProperty, RcStr, SubAnnotationPropertyOf, SubClassOf, SubObjectPropertyOf,
    SymmetricObjectProperty, TransitiveObjectProperty,
};
use horned_owl::ontology::set::SetOntology;
use std::collections::BTreeSet;

use crate::model::{default_prefixes, Model};
use crate::io::natural_order::NaturalOrder;
use crate::owlapi_hash::axiom_annotations_hash;

const OBO_BASE: &str = "http://purl.obolibrary.org/obo/";
const OIO: &str = "http://www.geneontology.org/formats/oboInOwl#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const IAO_DEF: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
const IAO_TERM_REPLACED_BY: &str = "http://purl.obolibrary.org/obo/IAO_0100001";
const IAO_OBSOLESCENCE_REASON: &str = "http://purl.obolibrary.org/obo/IAO_0000231";
const IAO_TERMS_MERGED: &str = "http://purl.obolibrary.org/obo/IAO_0000227";

/// Expand a CURIE to a full IRI string: a declared OBO-style prefix is
/// `http://purl.obolibrary.org/obo/<prefix>_<local>`, and a full IRI stands.
pub fn expand_id(id: &str) -> String {
    let id = id.trim();
    if id.starts_with("http://") || id.starts_with("https://") {
        return id.to_string();
    }
    match id.split_once(':') {
        Some((pre, local)) => format!("{OBO_BASE}{pre}_{local}"),
        None => format!("{OBO_BASE}{id}"),
    }
}

/// Expand an id read from an OBO document to its IRI. A prefixed id whose
/// local part is not canonical — it carries an underscore, a space or a
/// character outside the id alphabet — keeps prefix and local part apart with
/// `#` and escapes the local part.
pub fn expand_obo_id(id: &str) -> String {
    let id = id.trim();
    if id.starts_with("http://") || id.starts_with("https://") {
        return id.to_string();
    }
    match id.split_once(':') {
        // `ncithesaurus:Nuclear_Structure` is `…/obo/ncithesaurus_#Nuclear_Structure`.
        Some((pre, local)) if local.contains('_') => {
            format!("{OBO_BASE}{pre}_#{}", url_encode_local(local))
        }
        Some((pre, local)) => format!("{OBO_BASE}{pre}_{}", url_encode_local(local)),
        None => format!("{OBO_BASE}{id}"),
    }
}

/// The local part of an id as its IRI carries it: letters, digits and `.-*_`
/// stand, a space becomes `_`, and any other character is `%XX`-escaped — a
/// character outside ASCII as the escape of `?`.
fn url_encode_local(local: &str) -> String {
    let mut out = String::with_capacity(local.len());
    for c in local.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' | '*' | '_' => out.push(c),
            ' ' => out.push('_'),
            c if c.is_ascii() => out.push_str(&format!("%{:02X}", c as u32)),
            _ => out.push_str("%3F"),
        }
    }
    out
}

/// Undo [`url_encode_local`] on a canonical id's local part.
fn url_decode_local(local: &str) -> String {
    let bytes = local.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(b) = u8::from_str_radix(&local[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| local.to_string())
}

thread_local! {
    /// The `idspace:` prefix map declared by the OBO document currently being
    /// parsed. It is a thread-local rather than a parameter because the tag
    /// handlers that need it (every id a frame names, and every `qualifier_anns`
    /// call site) sit several layers below `load`, and only the reader ever
    /// touches it.
    static IDSPACES: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
    /// The annotation properties a tag or qualifier of the document being
    /// parsed has introduced; [`load`] declares these and no other annotation
    /// property. A property met only as a `property_value:` predicate, as a
    /// subset or synonym-type id, or as the `rdfs:label` of an xref's
    /// description is named, not introduced.
    static TAG_PROPERTIES: std::cell::RefCell<BTreeSet<String>> =
        const { std::cell::RefCell::new(BTreeSet::new()) };
    /// The first reason the document being parsed cannot be read, met where a
    /// tag is translated; [`load`] fails with it.
    static READ_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    /// The namespace an unprefixed id of the document being parsed lives in:
    /// `http://purl.obolibrary.org/obo/<ontology>#`, the ontology being the
    /// header's `ontology:` id as written, or `TEMP`.
    static DEFAULT_ID_SPACE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// `prop`, recorded as introduced by a tag of the document being parsed.
fn tag_prop(prop: &str) -> &str {
    TAG_PROPERTIES.with(|t| t.borrow_mut().insert(prop.to_string()));
    prop
}

/// Expand an OBO id, honouring the document's `idspace:` declarations before
/// falling back to the OBO PURL convention. Every id the reader expands goes
/// through here, because the writer renders an IRI in a declared non-OBO
/// namespace as a CURIE: COHO's `id: coho:COHO_0000460` must come back as
/// `http://www.ebi.ac.uk/coho/COHO_0000460`, and CL's
/// `{sssom:mapping_justification="…"}` as
/// `https://w3id.org/sssom/mapping_justification`, not
/// `…/obo/sssom_mapping_justification`.
fn expand_curie(id: &str) -> String {
    let id = id.trim();
    if ["http:", "https:", "ftp:", "urn:"].iter().any(|scheme| id.starts_with(scheme)) {
        return id.to_string();
    }
    if let Some(iri) = vocabulary_iri(id) {
        return iri;
    }
    match id.split_once(':') {
        Some((pre, local)) => {
            if let Some(ns) = IDSPACES.with(|m| m.borrow().get(pre).cloned()) {
                return format!("{ns}{local}");
            }
            expand_obo_id(id)
        }
        // An unprefixed id lives in the document's own id space.
        None => format!("{}{}", DEFAULT_ID_SPACE.with(|d| d.borrow().clone()), url_encode_local(id)),
    }
}

/// The IRI an `owl:`, `rdf:`, `rdfs:` or `xsd:` id names when its local name is a
/// term of that vocabulary (`rdfs:seeAlso`, `owl:Thing`, `xsd:string`); any other
/// id in those namespaces expands as an OBO id does.
fn vocabulary_iri(id: &str) -> Option<String> {
    let (prefix, local) = id.split_once(':')?;
    let (ns, terms): (&str, &[&str]) = match prefix {
        "owl" => ("http://www.w3.org/2002/07/owl#", OWL_TERMS),
        "rdf" => ("http://www.w3.org/1999/02/22-rdf-syntax-ns#", RDF_TERMS),
        "rdfs" => ("http://www.w3.org/2000/01/rdf-schema#", RDFS_TERMS),
        "xsd" => ("http://www.w3.org/2001/XMLSchema#", XSD_TERMS),
        _ => return None,
    };
    terms.contains(&local).then(|| format!("{ns}{local}"))
}

/// The terms of the OWL namespace an id names by their prefixed name: the RDF
/// vocabulary, the datatypes and the OWL/XML element names.
const OWL_TERMS: &[&str] = &[
    "AllDifferent", "AllDisjointClasses", "AllDisjointProperties", "Annotation",
    "AnnotationProperty", "AntisymmetricProperty", "AsymmetricProperty", "Axiom", "Class",
    "DataRange", "DataRestriction", "Datatype", "DatatypeProperty", "DeprecatedClass",
    "DeprecatedProperty", "FunctionalDataProperty", "FunctionalObjectProperty",
    "FunctionalProperty", "Individual", "InverseFunctionalProperty", "IrreflexiveProperty",
    "NamedIndividual", "NegativeDataPropertyAssertion", "NegativeObjectPropertyAssertion",
    "NegativePropertyAssertion", "Nothing", "ObjectProperty", "ObjectRestriction", "Ontology",
    "OntologyProperty", "ReflexiveProperty", "Restriction", "SelfRestriction",
    "SymmetricProperty", "Thing", "TransitiveProperty", "allValuesFrom", "annotatedProperty",
    "annotatedSource", "annotatedTarget", "assertionProperty", "backwardCompatibleWith",
    "bottomDataProperty", "bottomObjectProperty", "cardinality", "complementOf",
    "dataPropertyDomain", "dataPropertyRange", "datatypeComplementOf", "declaredAs",
    "deprecated", "differentFrom", "disjointDataProperties", "disjointObjectProperties",
    "disjointUnionOf", "disjointWith", "distinctMembers", "equivalentClass",
    "equivalentDataProperty", "equivalentObjectProperty", "equivalentProperty", "hasKey",
    "hasSelf", "hasValue", "imports", "incompatibleWith", "intersectionOf",
    "inverseObjectPropertyExpression", "inverseOf", "maxCardinality", "maxQualifiedCardinality",
    "members", "minCardinality", "minQualifiedCardinality", "object", "objectPropertyDomain",
    "objectPropertyRange", "onClass", "onDataRange", "onDatatype", "onProperty", "oneOf",
    "predicate", "priorVersion", "propertyChain", "propertyChainAxiom", "propertyDisjointWith",
    "qualifiedCardinality", "sameAs", "someValuesFrom", "sourceIndividual", "subDataPropertyOf",
    "subObjectPropertyOf", "subject", "targetIndividual", "targetValue", "topDataProperty",
    "topObjectProperty", "unionOf", "versionIRI", "versionInfo", "withRestrictions", "real",
    "rational", "DataProperty", "EntityAnnotation", "AnonymousIndividual", "nodeID",
    "annotationURI", "Import", "Label", "Comment", "Documentation", "Literal", "ObjectInverseOf",
    "DataComplementOf", "DataOneOf", "DatatypeRestriction", "FacetRestriction", "DataUnionOf",
    "DataIntersectionOf", "facet", "datatypeIRI", "ObjectIntersectionOf", "ObjectUnionOf",
    "ObjectComplementOf", "ObjectOneOf", "ObjectSomeValuesFrom", "ObjectAllValuesFrom",
    "ObjectHasSelf", "ObjectHasValue", "ObjectMinCardinality", "ObjectExactCardinality",
    "ObjectMaxCardinality", "DataSomeValuesFrom", "DataAllValuesFrom", "DataHasValue",
    "DataMinCardinality", "DataExactCardinality", "DataMaxCardinality", "SubClassOf",
    "EquivalentClasses", "DisjointClasses", "DisjointUnion", "UnionOf", "SubObjectPropertyOf",
    "ObjectPropertyChain", "EquivalentObjectProperties", "DisjointObjectProperties",
    "ObjectPropertyDomain", "ObjectPropertyRange", "InverseObjectProperties",
    "FunctionalObjectProperty", "InverseFunctionalObjectProperty", "SymmetricObjectProperty",
    "AsymmetricObjectProperty", "ReflexiveObjectProperty", "IrreflexiveObjectProperty",
    "TransitiveObjectProperty", "SubDataPropertyOf", "EquivalentDataProperties",
    "DisjointDataProperties", "DataPropertyDomain", "DataPropertyRange", "SameIndividual",
    "DifferentIndividuals", "ClassAssertion", "ObjectPropertyAssertion", "DataPropertyAssertion",
    "HasKey", "Declaration", "AnnotationAssertion", "AnnotationPropertyDomain",
    "AnnotationPropertyRange", "SubAnnotationPropertyOf", "DatatypeDefinition", "Prefix", "name",
    "IRI", "abbreviatedIRI", "AbbreviatedIRI", "DLSafeRule", "Body", "Head", "ClassAtom",
    "DataRangeAtom", "ObjectPropertyAtom", "DataPropertyAtom", "BuiltInAtom", "SameIndividualAtom",
    "DifferentIndividualsAtom", "Variable", "DescriptionGraphRule",
];

/// The terms of the RDF namespace an id names by their prefixed name.
const RDF_TERMS: &[&str] = &[
    "Description", "List", "PlainLiteral", "Property", "XMLLiteral", "first", "langString", "nil",
    "object", "predicate", "rest", "subject", "type",
];

/// The terms of the RDFS namespace an id names by their prefixed name.
const RDFS_TERMS: &[&str] = &[
    "Class", "Datatype", "Literal", "Resource", "comment", "domain", "isDefinedBy", "label",
    "range", "seeAlso", "subClassOf", "subPropertyOf",
];

/// The datatypes of the XML Schema namespace an id names by their prefixed name.
const XSD_TERMS: &[&str] = &[
    "string", "normalizedString", "token", "language", "Name", "NCName", "NMTOKEN", "decimal",
    "integer", "nonNegativeInteger", "nonPositiveInteger", "positiveInteger", "negativeInteger",
    "long", "int", "short", "byte", "unsignedLong", "unsignedInt", "unsignedShort",
    "unsignedByte", "double", "float", "boolean", "hexBinary", "base64Binary", "anyURI",
    "dateTime", "dateTimeStamp",
];

/// The annotation property a tag or a qualifier key names: `name` is
/// `rdfs:label`, `comment` `rdfs:comment`, `is_obsolete` `owl:deprecated`, `def`
/// `IAO:0000115`, `xref` `oboInOwl:hasDbXref` and so on through the OBO
/// annotation vocabulary; an id with a prefix or a scheme expands as an id; any
/// other name is in the oboInOwl namespace. A name with a space names no
/// property, and the read fails ([`load`] reports it).
fn tag_iri(tag: &str) -> String {
    if let Some(iri) = annotation_vocabulary(tag) {
        return iri.to_string();
    }
    if tag.contains(' ') {
        READ_ERROR.with(|e| {
            e.borrow_mut().get_or_insert_with(|| format!("spaces not allowed: '{tag}'"));
        });
    }
    if tag.contains(':') {
        expand_curie(tag)
    } else {
        format!("{OIO}{}", url_encode_local(tag))
    }
}

/// The property each tag of the OBO annotation vocabulary maps to.
fn annotation_vocabulary(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "is_obsolete" => OWL_DEPRECATED,
        "name" => RDFS_LABEL,
        "comment" => RDFS_COMMENT,
        "expand_expression_to" => IAO_EXPAND_EXPRESSION_TO,
        "expand_assertion_to" => IAO_EXPAND_ASSERTION_TO,
        "def" => IAO_DEF,
        "is_anti_symmetric" => IAO_ANTISYMMETRIC,
        "replaced_by" => IAO_TERM_REPLACED_BY,
        "shorthand" => "http://www.geneontology.org/formats/oboInOwl#shorthand",
        "consider" => "http://www.geneontology.org/formats/oboInOwl#consider",
        "id" => "http://www.geneontology.org/formats/oboInOwl#id",
        "created_by" => "http://www.geneontology.org/formats/oboInOwl#created_by",
        "creation_date" => "http://www.geneontology.org/formats/oboInOwl#creation_date",
        "format-version" => "http://www.geneontology.org/formats/oboInOwl#hasOBOFormatVersion",
        "treat-xrefs-as-is_a" => "http://www.geneontology.org/formats/oboInOwl#treat-xrefs-as-is_a",
        "treat-xrefs-as-has-subclass" => {
            "http://www.geneontology.org/formats/oboInOwl#treat-xrefs-as-has-subclass"
        }
        "treat-xrefs-as-relationship" => {
            "http://www.geneontology.org/formats/oboInOwl#treat-xrefs-as-relationship"
        }
        "treat-xrefs-as-genus-differentia" => {
            "http://www.geneontology.org/formats/oboInOwl#treat-xrefs-as-genus-differentia"
        }
        "treat-xrefs-as-reverse-genus-differentia" => {
            "http://www.geneontology.org/formats/oboInOwl#treat-xrefs-as-reverse-genus-differentia"
        }
        "treat-xrefs-as-equivalent" => {
            "http://www.geneontology.org/formats/oboInOwl#treat-xrefs-as-equivalent"
        }
        "namespace" => "http://www.geneontology.org/formats/oboInOwl#hasOBONamespace",
        "xref" => "http://www.geneontology.org/formats/oboInOwl#hasDbXref",
        "alt_id" => "http://www.geneontology.org/formats/oboInOwl#hasAlternativeId",
        "subset" => "http://www.geneontology.org/formats/oboInOwl#inSubset",
        "scope" => "http://www.geneontology.org/formats/oboInOwl#hasScope",
        "BROAD" => "http://www.geneontology.org/formats/oboInOwl#hasBroadSynonym",
        "NARROW" => "http://www.geneontology.org/formats/oboInOwl#hasNarrowSynonym",
        "EXACT" => "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym",
        "RELATED" => "http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym",
        "has_synonym_type" => "http://www.geneontology.org/formats/oboInOwl#hasSynonymType",
        "subsetdef" => "http://www.geneontology.org/formats/oboInOwl#SubsetProperty",
        "synonymtypedef" => "http://www.geneontology.org/formats/oboInOwl#SynonymTypeProperty",
        "namespace-id-rule" => "http://www.geneontology.org/formats/oboInOwl#NamespaceIdRule",
        "logical-definition-view-relation" => {
            "http://www.geneontology.org/formats/oboInOwl#logical-definition-view-relation"
        }
        _ => return None,
    })
}

/// Compress a full IRI to an OBO id where possible (inverse of [`expand_id`]).
pub fn compress_iri(iri: &str) -> String {
    if let Some(rest) = iri.strip_prefix(OBO_BASE) {
        // An ontology-local `#` namespace (e.g. `obo/mondo#disease_has_basis_in`)
        // is written as the bare local name, and the reader re-expands a bare
        // relation/typedef id back into the ontology's `#` namespace. (Emitting
        // `mondo#…` instead would make a reload double-prefix it to
        // `mondo#mondo#…`.)
        if let Some((_, local)) = rest.rsplit_once('#') {
            return local.to_string();
        }
        if let Some(idx) = rest.find('_') {
            // Only treat as PREFIX_LOCAL when the suffix looks like a local id.
            let (pre, local) = rest.split_at(idx);
            let local = &local[1..];
            if !pre.is_empty() && !local.is_empty() && !local.contains('_') {
                return format!("{pre}:{}", url_decode_local(local));
            }
        }
        return rest.to_string();
    }
    iri.to_string()
}

/// Whether OBO `[Instance]` frames are written and read.
///
/// A build that emulates an ODK release does neither, as that release does
/// neither. Its writer spells no individual: a class assertion is dropped and an
/// object property assertion is left to `owl-axioms:`. Its reader stops at the
/// first `[Instance]` frame, reporting the line, and keeps what came before. Every
/// other run writes named individuals as `[Instance]` frames and reads them back.
/// The plan decides it (`Plan::emulate_odk_version`): `build::set_emulation` sets
/// it, in a build and in every owlmake process the build starts, and returns it
/// to this default at the start of every invocation. There is deliberately no
/// environment override: it decides artefact bytes.
static INSTANCE_FRAMES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Set whether OBO `[Instance]` frames are written and read — see the static
/// above.
pub fn set_instance_frames(on: bool) {
    INSTANCE_FRAMES.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn instance_frames() -> bool {
    INSTANCE_FRAMES.load(std::sync::atomic::Ordering::Relaxed)
}

// === Reader ==============================================================

/// One `tag: value` line of the header or of a frame.
struct Clause {
    tag: String,
    /// For a tag read by [`ValueGrammar::Unquoted`] or [`ValueGrammar::Id`], the
    /// value with its escapes resolved; for any other tag, the text after the tag
    /// with its `!` comment removed, which the tag's own rule reads.
    value: String,
    /// The `{key="value"}` qualifiers of a value read by a grammar here, in
    /// document order. Every other tag reads its qualifiers from `value`.
    quals: Vec<(String, String)>,
}

#[derive(Default)]
struct Stanza {
    tags: Vec<Clause>,
}

impl Stanza {
    fn get(&self, key: &str) -> Option<&str> {
        self.tags.iter().find(|c| c.tag == key).map(|c| c.value.as_str())
    }
    fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.clauses(key).map(|c| c.value.as_str())
    }
    fn clauses<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Clause> {
        self.tags.iter().filter(move |c| c.tag == key)
    }
}

/// The tags a `[Term]` (and an `[Instance]`) frame reads by a rule of its own.
/// Any other tag is a custom tag: an unquoted string asserted with the property
/// [`tag_iri`] names.
const TERM_TAGS: &[&str] = &[
    "id", "is_anonymous", "builtin", "is_obsolete", "name", "comment", "created_by", "namespace",
    "alt_id", "is_a", "union_of", "equivalent_to", "disjoint_from", "replaced_by", "consider",
    "def", "subset", "synonym", "xref", "property_value", "intersection_of", "relationship",
    "creation_date",
];

/// The tags a `[Typedef]` frame reads by a rule of its own; any other tag is a
/// custom tag, as in a `[Term]`.
const TYPEDEF_TAGS: &[&str] = &[
    "id", "is_anonymous", "builtin", "is_obsolete", "is_anti_symmetric", "is_cyclic",
    "is_reflexive", "is_symmetric", "is_asymmetric", "is_transitive", "is_functional",
    "is_inverse_functional", "is_metadata_tag", "is_class_level", "name", "comment",
    "created_by", "namespace", "alt_id", "subset", "is_a", "union_of", "equivalent_to",
    "disjoint_from", "replaced_by", "consider", "inverse_of", "transitive_over", "disjoint_over",
    "domain", "range", "def", "synonym", "xref", "property_value", "intersection_of",
    "relationship", "creation_date", "holds_over_chain", "equivalent_to_chain",
    "expand_assertion_to", "expand_expression_to",
];

/// The header tags read by a rule of their own; every other header tag is an
/// unquoted string.
const HEADER_TAGS: &[&str] = &["synonymtypedef", "subsetdef", "date", "property_value", "import", "idspace"];

/// How a tag's value is read off its line.
#[derive(Clone, Copy, PartialEq)]
enum ValueGrammar {
    /// The text up to the first unescaped `!` or `{`, escapes resolved and
    /// trailing white space dropped; then a qualifier block and a `!` comment, and
    /// in a frame one more qualifier block.
    Unquoted,
    /// One id, up to the first unescaped space, `!` or `{`; then a qualifier block
    /// and a `!` comment. `optional` allows the id to be empty.
    Id { optional: bool },
    /// An `import:` IRI: the text up to the first unescaped `!` or `{`; a
    /// qualifier block after it is skipped.
    Import,
    /// The tag's own rule, applied where the tag is translated.
    Own,
}

/// The grammar of `tag` in a frame of `kind`, or in the header when `kind` is
/// `None`.
fn value_grammar(kind: Option<&str>, tag: &str) -> ValueGrammar {
    let own = match kind {
        None => return match tag {
            "import" => ValueGrammar::Import,
            t if HEADER_TAGS.contains(&t) => ValueGrammar::Own,
            _ => ValueGrammar::Unquoted,
        },
        Some("Typedef") => match tag {
            "name" | "comment" | "created_by" => return ValueGrammar::Unquoted,
            "namespace" | "alt_id" | "subset" | "disjoint_over" => {
                return ValueGrammar::Id { optional: false }
            }
            "creation_date" => return ValueGrammar::Id { optional: true },
            t => TYPEDEF_TAGS.contains(&t),
        },
        Some(kind) => match tag {
            "name" | "comment" | "created_by" | "subset" => return ValueGrammar::Unquoted,
            "namespace" | "alt_id" => return ValueGrammar::Id { optional: false },
            "creation_date" => return ValueGrammar::Id { optional: true },
            t => TERM_TAGS.contains(&t) || (kind == "Instance" && t == "instance_of"),
        },
    };
    if own {
        ValueGrammar::Own
    } else {
        ValueGrammar::Unquoted
    }
}

/// The tag a deprecated tag name stands for.
fn current_tag(tag: &str) -> &str {
    match tag {
        "inverse_of_on_instance_level" => "inverse_of",
        "xref_analog" | "xref_unknown" => "xref",
        "instance_level_is_transitive" => "is_transitive",
        _ => tag,
    }
}

/// The synonym scope a deprecated `<scope>_synonym:` tag gives its quoted text.
fn deprecated_synonym_scope(tag: &str) -> Option<&'static str> {
    match tag {
        "exact_synonym" => Some("EXACT"),
        "narrow_synonym" => Some("NARROW"),
        "broad_synonym" => Some("BROAD"),
        "related_synonym" => Some("RELATED"),
        _ => None,
    }
}

/// A position in one line of an OBO document, with the primitives a clause is
/// read by.
struct Cursor<'a> {
    line: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(line: &'a str) -> Self {
        Cursor { line, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.line[self.pos.min(self.line.len())..]
    }

    fn at_end(&self) -> bool {
        self.pos >= self.line.len()
    }

    fn at(&self, c: char) -> bool {
        self.rest().starts_with(c)
    }

    fn consume(&mut self, s: &str) -> bool {
        if !self.at_end() && self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    /// Step over spaces (a tab is not one).
    fn spaces(&mut self) {
        while self.at(' ') {
            self.pos += 1;
        }
    }

    /// The text up to the first character of `stops` that no backslash escapes,
    /// or to the end of the line, with its escapes resolved. A backslash that
    /// ends the line escapes nothing and is refused.
    fn until(&mut self, stops: &str) -> Result<String> {
        let rest = self.rest();
        let mut i = 0;
        let mut escaped = false;
        while let Some(c) = rest[i..].chars().next() {
            if c == '\\' {
                escaped = true;
                match rest[i + 1..].chars().next() {
                    Some(next) => i += 1 + next.len_utf8(),
                    None => anyhow::bail!("a backslash ends the line, escaping nothing"),
                }
                continue;
            }
            if stops.contains(c) {
                break;
            }
            i += c.len_utf8();
        }
        self.pos += i;
        let text = &rest[..i];
        Ok(if escaped { unescape_obo(text) } else { text.to_string() })
    }

    /// [`Cursor::until`], then past the stop character.
    fn through(&mut self, stop: &str) -> Result<String> {
        let text = self.until(stop)?;
        self.pos += 1;
        Ok(text)
    }

    /// A `{key="value", …}` block, when the cursor is at one.
    fn qualifier_block(&mut self, quals: &mut Vec<(String, String)>) -> Result<()> {
        if self.consume("{") {
            self.qualifier(quals)?;
            while self.consume(",") {
                self.qualifier(quals)?;
            }
            self.spaces();
            if !self.consume("}") {
                anyhow::bail!("missing closing '}}' for trailing qualifier block");
            }
        }
        Ok(())
    }

    /// One `key="value"` (or `key=value`) of a qualifier block. The key is the
    /// text before the `=`, spaces after it included.
    fn qualifier(&mut self, quals: &mut Vec<(String, String)>) -> Result<()> {
        self.spaces();
        if !self.rest().contains('=') {
            anyhow::bail!("missing '=' in trailing qualifier block");
        }
        let key = self.through("=")?;
        self.spaces();
        let value = if self.consume("\"") { self.through("\"")? } else { self.until(" ,}")? };
        quals.push((key, value));
        self.spaces();
        Ok(())
    }

    /// A `!` comment, which runs to the end of the line.
    fn comment(&mut self) {
        self.spaces();
        if self.at('!') {
            self.pos = self.line.len();
        }
    }

    /// The end of the line, after any spaces.
    fn end(&mut self) -> Result<()> {
        self.spaces();
        if self.at_end() {
            Ok(())
        } else {
            anyhow::bail!("expected the end of the line but found: {}", self.rest())
        }
    }
}

/// Java's `\s`: the white space trailing an unquoted value that is dropped.
fn trim_java_space(s: &str) -> &str {
    s.trim_end_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r'])
}

/// Read the value of a clause whose text after the tag's `:` is `text`, by
/// `grammar`; `in_frame` is false for a header clause.
fn read_value(text: &str, grammar: ValueGrammar, in_frame: bool) -> Result<(String, Vec<(String, String)>)> {
    let mut c = Cursor::new(text);
    if c.at_end() {
        anyhow::bail!("expected a value after the tag, found the end of the line");
    }
    c.spaces();
    let mut quals = Vec::new();
    let value = match grammar {
        ValueGrammar::Unquoted => {
            let value = trim_java_space(&c.until("!{")?).to_string();
            if c.at('{') {
                c.qualifier_block(&mut quals)?;
            }
            c.comment();
            if in_frame {
                c.spaces();
                c.qualifier_block(&mut quals)?;
                c.comment();
            }
            value
        }
        ValueGrammar::Id { optional } => {
            let value = c.until(" !{")?;
            if value.is_empty() && !optional {
                anyhow::bail!("expected an id");
            }
            c.spaces();
            c.qualifier_block(&mut quals)?;
            c.comment();
            value
        }
        ValueGrammar::Import => {
            let value = trim_java_space(&c.until("!{")?).to_string();
            c.spaces();
            if c.at('{') {
                c.through("}")?;
            }
            c.comment();
            value
        }
        ValueGrammar::Own => unreachable!("a tag with a rule of its own is read by that rule"),
    };
    c.comment();
    c.end()?;
    Ok((value, quals))
}

/// Load an ontology from OBO format.
pub fn load<R: BufRead>(reader: R) -> Result<Model> {
    let b = Build::new();
    let mut ont: SetOntology<RcStr> = SetOntology::new();

    let mut header = Stanza::default();
    let mut stanzas: Vec<(String, Stanza)> = Vec::new();
    let mut current: Option<(String, Stanza)> = None;

    for (n, line) in reader.lines().enumerate() {
        let line = line?;
        let line = strip_comment(&line);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if let Some(s) = current.take() {
                stanzas.push(s);
            }
            let kind = trimmed[1..trimmed.len() - 1].to_string();
            if kind == "Instance" && !instance_frames() {
                eprintln!(
                    "error: `[Instance]` frames are not read by a build that emulates an ODK \
                     release; parsing stopped at line {}",
                    n + 1
                );
                break;
            }
            current = Some((kind, Stanza::default()));
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let kind = current.as_ref().map(|(kind, _)| kind.as_str());
            let mut tag = current_tag(key.trim()).to_string();
            if kind == Some("Typedef") && tag == "is_metadata" {
                tag = "is_metadata_tag".to_string();
            }
            // The text after the tag's `:` as the line has it, `!` comment and all.
            let text = line.trim_start().split_once(':').map_or("", |(_, text)| text);
            let value = value.trim();
            let refuse = |e: anyhow::Error| anyhow::anyhow!("OBO line {}: {e}: {line}", n + 1);
            let deprecated_synonym = deprecated_synonym_scope(&tag).filter(|_| kind.is_some());
            let clause = match (deprecated_synonym, value_grammar(kind, &tag)) {
                // A deprecated `exact_synonym: "…" […]` is a synonym of that scope;
                // its `[…]` list is required.
                (Some(scope), _) if text.trim_start_matches(' ').starts_with('"') => {
                    let Some((_, rest)) = parse_quoted(value) else {
                        return Err(refuse(anyhow::anyhow!("the synonym's quoted text does not end")));
                    };
                    if !rest.trim_start_matches(' ').starts_with('[') {
                        return Err(refuse(anyhow::anyhow!("expected an xref list, or at least an empty list '[]'")));
                    }
                    Clause {
                        tag: "synonym".to_string(),
                        value: format!("{} {scope}{rest}", &value[..value.len() - rest.len()]),
                        quals: Vec::new(),
                    }
                }
                // Unquoted, it is a synonym all the same: an unquoted string with no
                // scope, which reads as RELATED.
                (Some(_), _) => {
                    let (text, quals) = read_value(text, ValueGrammar::Unquoted, true).map_err(refuse)?;
                    Clause { tag: "synonym".to_string(), value: format!("\"{}\" RELATED", escape(&text)), quals }
                }
                (None, ValueGrammar::Own) => Clause { tag, value: value.to_string(), quals: Vec::new() },
                (None, grammar) => {
                    let (value, quals) = read_value(text, grammar, kind.is_some()).map_err(refuse)?;
                    Clause { tag, value, quals }
                }
            };
            match &mut current {
                Some((_, s)) => s.tags.push(clause),
                None => header.tags.push(clause),
            }
        }
    }
    if let Some(s) = current.take() {
        stanzas.push(s);
    }

    // `idspace: PREFIX NAMESPACE [description]` header lines: the document's own
    // CURIE bindings, consulted by `expand_curie` for the rest of the parse.
    TAG_PROPERTIES.with(|t| t.borrow_mut().clear());
    READ_ERROR.with(|e| e.borrow_mut().take());
    IDSPACES.with(|m| {
        let mut m = m.borrow_mut();
        m.clear();
        for line in header.all("idspace") {
            let mut it = line.split_whitespace();
            if let (Some(prefix), Some(ns)) = (it.next(), it.next()) {
                m.insert(prefix.to_string(), ns.to_string());
            }
        }
    });

    // Header → ontology id + annotations. A document with no `ontology:` line
    // is the ontology `TEMP`, and `TEMP` is the idspace its bare local names
    // resolve in.
    let ontology_id: &str = header.get("ontology").unwrap_or("TEMP");
    DEFAULT_ID_SPACE.with(|d| *d.borrow_mut() = format!("{OBO_BASE}{ontology_id}#"));
    {
        let ont_id = ontology_id;
        let iri = if ont_id.starts_with("http") {
            ont_id.to_string()
        } else if ont_id == "TEMP" && header.get("ontology").is_none() {
            format!("{OBO_BASE}TEMP")
        } else {
            format!("{OBO_BASE}{ont_id}.owl")
        };
        // `data-version:` is the version IRI relative to the ontology id (the
        // inverse of the writer's `data_version`), so a release `.obo` keeps its
        // `owl:versionIRI` through an obo→owl conversion.
        let viri = header.get("data-version").map(|dv| {
            let dv = dv.trim();
            if dv.starts_with("http") {
                b.iri(dv.to_string())
            } else if let Some(short) = iri.strip_prefix(OBO_BASE).and_then(|s| s.strip_suffix(".owl")) {
                b.iri(format!("{OBO_BASE}{short}/{dv}/{short}.owl"))
            } else {
                b.iri(format!("{iri}/{dv}"))
            }
        });
        ont.insert(Component::OntologyID(horned_owl::model::OntologyID {
            iri: Some(b.iri(iri)),
            viri,
        }));
    }

    // `import:` header lines → owl:imports, so downstream merge/import-removal
    // (e.g. the release pipeline) sees the document's full import closure.
    for imp in header.all("import") {
        ont.insert(Component::Import(horned_owl::model::Import(b.iri(imp.trim()))));
    }

    // The header `default-namespace` is applied as `hasOBONamespace` to every
    // term/typedef that does not declare its own `namespace`.
    let default_ns = header.get("default-namespace").map(|s| s.to_string());

    let onto_ns_for_defs = Some(format!("{OBO_BASE}{ontology_id}#"));
    // `synonymtypedef:`/`subsetdef:` header lines declare an annotation property
    // that is a sub-property of oboInOwl:SynonymTypeProperty / :SubsetProperty.
    // The quoted description is carried as `rdfs:label` for a synonymtypedef but
    // as `rdfs:comment` for a subsetdef.
    for (tag, parent, descr_prop) in [
        ("synonymtypedef", "SynonymTypeProperty", RDFS_LABEL),
        ("subsetdef", "SubsetProperty", RDFS_COMMENT),
    ] {
        // The tag introduces the oboInOwl parent property; the subset or synonym
        // type it names is only named, so it stays undeclared.
        if header.all(tag).next().is_some() {
            tag_prop(&format!("{OIO}{parent}"));
        }
        for s in header.all(tag) {
            let id = s.split_whitespace().next().unwrap_or(s);
            let iri = if id.contains(':') {
                expand_curie(id)
            } else {
                resolve_local(id, onto_ns_for_defs.as_deref())
            };
            insert_annotated(
                &mut ont,
                Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
                    sub: b.annotation_property(iri.as_str()),
                    sup: b.annotation_property(format!("{OIO}{parent}").as_str()),
                }),
                qualifier_anns(&b, s),
            );
            if let Some(rest) = s.strip_prefix(id) {
                if let Some((name, after)) = parse_quoted(rest.trim()) {
                    assert_ann(&b, &mut ont, &iri, tag_prop(descr_prop), &name);
                    // A synonym type's scope is the synonym property its
                    // synonyms take.
                    let scope = match after.split_whitespace().next() {
                        Some("EXACT") => Some("hasExactSynonym"),
                        Some("RELATED") => Some("hasRelatedSynonym"),
                        Some("NARROW") => Some("hasNarrowSynonym"),
                        Some("BROAD") => Some("hasBroadSynonym"),
                        _ => None,
                    };
                    if let (true, Some(scope)) = (tag == "synonymtypedef", scope) {
                        // `hasScope` and the synonym property it names are both
                        // introduced here.
                        ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
                            subject: AnnotationSubject::IRI(b.iri(iri.as_str())),
                            ann: ann_iri(
                                &b,
                                tag_prop(&format!("{OIO}hasScope")),
                                tag_prop(&format!("{OIO}{scope}")),
                            ),
                        }));
                    }
                }
            }
        }
    }

    // Subset and (local) synonym-type names map to IRIs in the ontology's own
    // namespace, `http://purl.obolibrary.org/obo/<ontology>#<name>` — e.g.
    // `ontology: uberon/core` ⇒ `obo/uberon/core#efo_slim`, and
    // `ontology: http://example.org/b` ⇒ `obo/http://example.org/b#name`. That
    // is the OBO→OWL mapping for a bare local name.
    let onto_ns = Some(format!("{OBO_BASE}{ontology_id}#"));

    // Relation shorthands: a `[Typedef]` whose `id` is a bare name and which has
    // a single `xref` to an ontology term (e.g. `id: disease_has_basis_in_…` +
    // `xref: RO:0004020`) is the OBO shorthand for that property. All uses in
    // `relationship:`/`intersection_of:` resolve to the xref IRI under the OBO
    // relation-shorthand rule, not `obo/<shorthand>`.
    let mut rel_map: HashMap<String, String> = HashMap::new();
    // Metadata-tag properties (`is_metadata_tag: true`) are annotation
    // properties: a `relationship:` using one is an annotation assertion, not a
    // logical existential, and the typedef is declared as an AnnotationProperty.
    let mut metadata_tags: BTreeSet<String> = BTreeSet::new();
    // The ids of the class-level relations (`is_class_level: true`), as written:
    // a relation clause naming one by that id states a value restriction.
    let mut class_level: BTreeSet<String> = BTreeSet::new();
    for (kind, st) in &stanzas {
        if kind == "Typedef" {
            if let Some(id) = st.get("id") {
                if st.get("is_class_level").and_then(|v| v.split_whitespace().next()) == Some("true") {
                    class_level.insert(id.to_string());
                }
                let iri = if id.contains(':') {
                    expand_curie(id)
                } else {
                    let i = typedef_iri(id, st.get("xref"), onto_ns.as_deref());
                    rel_map.insert(id.to_string(), i.clone());
                    i
                };
                if st.get("is_metadata_tag") == Some("true") {
                    metadata_tags.insert(iri);
                }
            }
        }
    }
    // A bare relation name USED in a `relationship:`/`intersection_of:` clause but
    // never DECLARED by a `[Typedef]` is ontology-local: it maps to
    // `obo/<ontology>#<rel>`, not the generic `obo/<rel>` — an undeclared
    // `relationship: undeclared_rel X` in `ontology: mondo` yields
    // `obo/mondo#undeclared_rel`, while a Typedef-with-xref relation resolves to its
    // xref IRI. Seed those into `rel_map` so every resolve_rel call agrees.
    if let Some(ns) = onto_ns.as_deref() {
        for (kind, st) in &stanzas {
            if kind != "Term" && kind != "Instance" {
                continue;
            }
            for tag in ["relationship", "intersection_of"] {
                for v in st.all(tag) {
                    let first = v.split_whitespace().next().unwrap_or("");
                    // Only bare names (a CURIE/IRI resolves on its own), and only
                    // when no Typedef already defined them.
                    if first.is_empty()
                        || first.contains(':')
                        || first.starts_with("http")
                        || rel_map.contains_key(first)
                    {
                        continue;
                    }
                    // `intersection_of: <genus>` (one token) is a class, not a relation.
                    if tag == "intersection_of" && v.split_whitespace().count() < 2 {
                        continue;
                    }
                    rel_map.insert(first.to_string(), format!("{ns}{first}"));
                }
            }
        }
    }

    // The header's annotations, each with the tag it is read from: a header
    // `property_value:`/`remark:` line is an ontology-level annotation, so the
    // primary ontology's header survives a merge.
    let mut stated: Vec<(&str, Annotation<RcStr>)> = Vec::new();
    for pv in header.all("property_value") {
        if let Some((mut ann, quals)) = property_value_annotation(&b, pv, &rel_map, onto_ns.as_deref(), true) {
            ann.ann = quals.into_iter().collect();
            stated.push(("property_value", ann));
        }
    }
    // Every other header tag is an ontology annotation, annotated with the
    // clause's qualifiers: `remark:` an `rdfs:comment`, any other tag the property
    // `tag_iri` names (`format-version` is `oboInOwl:hasOBOFormatVersion`,
    // `saved-by` `oboInOwl:saved-by`). `date:` is the date [`HeaderDate`] reads,
    // written by its pattern, and its qualifiers are text it ignores; every
    // `date:` must be a date, and the first is the ontology's. The tags with
    // rules of their own are read above and below.
    const HEADER_RULES: &[&str] = &[
        "data-version",
        "ontology",
        "import",
        "idspace",
        "subsetdef",
        "synonymtypedef",
        "property_value",
        "owl-axioms",
    ];
    let mut dated = false;
    for c in &header.tags {
        if HEADER_RULES.contains(&c.tag.as_str()) {
            continue;
        }
        if c.tag == "date" {
            let Some(date) = HeaderDate::read(&unescape_obo(&c.value)) else {
                anyhow::bail!("could not read the OBO document: the header date is not of the form dd:MM:yyyy HH:mm: {}", c.value);
            };
            if !std::mem::replace(&mut dated, true) {
                stated.push((c.tag.as_str(), ann(&b, tag_prop(&tag_iri("date")), &date.obo())));
            }
            continue;
        }
        // `remark` names `rdfs:comment` without introducing it.
        let prop = if c.tag == "remark" { RDFS_COMMENT.to_string() } else { tag_prop(&tag_iri(&c.tag)).to_string() };
        let mut a = ann(&b, &prop, &c.value);
        a.ann = quals_anns(&b, &c.quals).into_iter().collect();
        stated.push((c.tag.as_str(), a));
    }
    // The ontology takes them tag by tag, in the order a hash set of the
    // header's tag names iterates them, a tag's in document order, and holds
    // of them what an ontology holds of what it is given.
    let mut names: Vec<&str> = Vec::new();
    for c in &header.tags {
        if !names.contains(&c.tag.as_str()) {
            names.push(&c.tag);
        }
    }
    let hashes: Vec<i32> = names.iter().map(|t| crate::owlapi_hash::java_string_hash(t)).collect();
    let rank: HashMap<&str, usize> =
        crate::owlapi_hash::hashset_order(&hashes).into_iter().enumerate().map(|(r, i)| (names[i], r)).collect();
    stated.sort_by_key(|(tag, _)| rank[tag]);
    for a in crate::owlapi_annotations::hold(stated.into_iter().map(|(_, a)| a).collect()) {
        ont.insert(Component::OntologyAnnotation(horned_owl::model::OntologyAnnotation(a)));
    }
    // The `owl-axioms:` header clause carries, in OWL functional syntax, the axioms
    // OBO has no tag for (ClassAssertion, DifferentIndividuals,
    // IrreflexiveObjectProperty, extra DisjointClasses/SubClassOf, re-declarations,
    // …). Its value, an unquoted string with its escapes resolved, is one
    // functional-syntax `Ontology(…)` document; every axiom in it joins the model,
    // its own name and annotations do not, and a value that does not parse fails
    // the read.
    for oa in header.all("owl-axioms") {
        let mut cfg = horned_owl::io::ParserConfiguration::default();
        cfg.lax = true;
        let (parsed, _): (SetOntology<RcStr>, _) = horned_owl::io::ofn::reader::read(&mut oa.as_bytes(), cfg)
            .map_err(|e| anyhow::anyhow!("the owl-axioms header clause is not an ontology in functional syntax: {e}"))?;
        for ac in parsed {
            if matches!(ac.component, Component::OntologyID(_) | Component::OntologyAnnotation(_)) {
                continue;
            }
            ont.insert(ac);
        }
    }

    for (kind, st) in &stanzas {
        match kind.as_str() {
            "Term" => term_to_owl(&b, &mut ont, st, default_ns.as_deref(), onto_ns.as_deref(), &rel_map, &metadata_tags, &class_level),
            "Typedef" => typedef_to_owl(&b, &mut ont, st, default_ns.as_deref(), onto_ns.as_deref(), &rel_map, &metadata_tags),
            "Instance" => instance_to_owl(&b, &mut ont, st, default_ns.as_deref(), onto_ns.as_deref(), &rel_map, &metadata_tags),
            _ => {}
        }
    }

    if let Some(e) = READ_ERROR.with(|e| e.borrow_mut().take()) {
        anyhow::bail!("could not read the OBO document: {e}");
    }
    declare_tag_properties(&b, &mut ont);

    let mut m = Model::from_parts(ont, default_prefixes());
    // OBO carries no document prefix map, so a model read from OBO must not claim
    // one: every prefix such a document ends up declaring is either a builtin or
    // generated from an entity's namespace. Converting a two-term obo yields an
    // xmlns block of owl/rdf/xml/xsd/rdfs plus a generated `oboInOwl`, with no
    // `obo` (the only IRI in that namespace is a CLASS, and classes do not require
    // a namespace declaration) and no `dc`; its functional syntax declares only
    // `:`/owl/rdf/xml/xsd/rdfs, spelling every other IRI in full.
    // `default_prefixes()` above is still the map used to EXPAND CURIEs
    // while parsing — it is just not the document's own declaration set.
    m.format_prefixes_cleared = true;
    m.obo_source = true;
    // …but the document's own `idspace:` lines ARE its prefix declarations, and an
    // obo→obo trip has to give them back: consulting them only to expand CURIEs
    // during the parse (the thread-local above) and then dropping them would lose
    // every declaration the writer had just made. MONDO's `mondo.obo` is the case —
    // a build step re-reads the written target and re-serialises that model, so a
    // declaration lost on read never comes back.
    let declared: Vec<(String, String)> =
        IDSPACES.with(|m| m.borrow().iter().map(|(p, n)| (p.clone(), n.clone())).collect());
    // They are what this document's own construction bound, so a functional
    // or RDF/XML write of it declares them too.
    for (prefix, ns) in declared {
        let _ = m.prefixes.add_prefix(&prefix, &ns);
        if !m.explicit_prefixes.iter().any(|(p, _)| *p == prefix) {
            m.explicit_prefixes.push((prefix.clone(), ns.clone()));
        }
        if !m.built_prefixes.iter().any(|(p, _)| *p == prefix) {
            m.built_prefixes.push((prefix, ns));
        }
    }
    Ok(m)
}

/// The OBO built-in annotation properties, each with the canonical `rdfs:label`
/// a tag that introduces it gives it (`hasExactSynonym` → "has_exact_synonym").
fn obo_builtin_annotation_properties() -> [(String, &'static str); 31] {
    // Full IRIs so the IAO_* and oboInOwl meta-properties (SubsetProperty …) sit
    // alongside the oboInOwl synonym/xref properties.
    const OBO: &str = "http://purl.obolibrary.org/obo/";
    [
        (format!("{OIO}hasExactSynonym"), "has_exact_synonym"),
        (format!("{OIO}hasNarrowSynonym"), "has_narrow_synonym"),
        (format!("{OIO}hasBroadSynonym"), "has_broad_synonym"),
        (format!("{OIO}hasRelatedSynonym"), "has_related_synonym"),
        (format!("{OIO}hasSynonymType"), "has_synonym_type"),
        (format!("{OIO}hasScope"), "has_scope"),
        (format!("{OIO}hasDbXref"), "database_cross_reference"),
        (format!("{OIO}hasOBONamespace"), "has_obo_namespace"),
        (format!("{OIO}hasOBOFormatVersion"), "has_obo_format_version"),
        (format!("{OIO}hasAlternativeId"), "has_alternative_id"),
        (format!("{OIO}inSubset"), "in_subset"),
        (format!("{OIO}SubsetProperty"), "subset_property"),
        (format!("{OIO}SynonymTypeProperty"), "synonym_type_property"),
        (format!("{OIO}NamespaceIdRule"), "namespace-id-rule"),
        (format!("{OIO}logical-definition-view-relation"), "logical-definition-view-relation"),
        (format!("{OIO}consider"), "consider"),
        (format!("{OIO}shorthand"), "shorthand"),
        (format!("{OIO}id"), "id"),
        (format!("{OIO}created_by"), "created by"),
        (format!("{OIO}creation_date"), "creation date"),
        (format!("{OIO}treat-xrefs-as-is_a"), "treat-xrefs-as-is_a"),
        (format!("{OIO}treat-xrefs-as-has-subclass"), "treat-xrefs-as-has-subclass"),
        (format!("{OIO}treat-xrefs-as-relationship"), "treat-xrefs-as-relationship"),
        (format!("{OIO}treat-xrefs-as-genus-differentia"), "treat-xrefs-as-genus-differentia"),
        (
            format!("{OIO}treat-xrefs-as-reverse-genus-differentia"),
            "treat-xrefs-as-reverse-genus-differentia",
        ),
        (format!("{OIO}treat-xrefs-as-equivalent"), "treat-xrefs-as-equivalent"),
        (format!("{OBO}IAO_0000115"), "definition"),
        (format!("{OBO}IAO_0000424"), "expand expression to"),
        (format!("{OBO}IAO_0000425"), "expand assertion to"),
        (format!("{OBO}IAO_0000427"), "antisymmetric property"),
        (format!("{OBO}IAO_0100001"), "term replaced by"),
    ]
}

/// Declare every annotation property a tag of the document introduced (see
/// `TAG_PROPERTIES`), and give each built-in one among them its canonical
/// `rdfs:label` unless the document labels it itself. Nothing else is declared
/// here: a class named only by `is_a:` or `disjoint_from:`, a relation with no
/// `[Typedef]` frame and a `property_value:` predicate stay undeclared, and a
/// writer declares what the written axioms name.
fn declare_tag_properties(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>) {
    let labels: HashMap<String, &str> = obo_builtin_annotation_properties().into_iter().collect();
    let labelled: HashSet<String> = ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::AnnotationAssertion(ax) if ax.ann.ap.0.as_ref() == RDFS_LABEL => match &ax.subject {
                AnnotationSubject::IRI(i) => Some(i.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    for prop in TAG_PROPERTIES.with(|t| t.borrow().clone()) {
        ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
            b.annotation_property(prop.as_str()),
        )));
        if let Some(label) = labels.get(&prop) {
            if !labelled.contains(&prop) {
                assert_ann(b, ont, &prop, RDFS_LABEL, label);
            }
        }
    }
}

fn strip_comment(line: &str) -> String {
    // OBO `!` comments apply only *outside* quoted strings and when not escaped
    // (`\!`). A naive cut at " ! " would truncate literals like
    // `"… UBERON:0000091 ! bilaminar disc"`, so track quoting and escaping.
    let bytes = line.as_bytes();
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2; // skip escaped char
                continue;
            }
            b'"' => in_quote = !in_quote,
            b'!' if !in_quote => {
                // A comment: trim trailing whitespace before it.
                return line[..i].trim_end().to_string();
            }
            _ => {}
        }
        i += 1;
    }
    line.to_string()
}

/// An OBO-native string annotation.
///
/// `xsd:string`, explicitly, NOT `Literal::Simple`. An untyped, language-free
/// literal has two spellings and they are NOT equal: an OBO read produces
/// `xsd:string`, a functional-syntax or RDF/XML read produces `rdf:PlainLiteral`
/// — see `Model::plain_literals_typed`. Both serialize bare, so the difference is
/// invisible until one subject carries literals from BOTH kinds of source, and
/// then it decides their order: literals compare on the datatype IRI first, and
/// `…/1999/02/22-rdf-syntax-ns#PlainLiteral` sorts before
/// `…/2001/XMLSchema#string`.
///
/// OBA is exactly that case. `oba-full.owl` merges `oba-edit.obo` with its import
/// closure collapsed, so a class can hold one `hasExactSynonym` from the OBO edit
/// file and another from the functional `patterns/definitions.owl`; the
/// pattern-derived one comes first whatever the alphabet says — in a two-file
/// merge of `"aaa from obo"` and `"zzz from ofn"`, the OFN literal leads.
///
/// `Literal::Simple` stays what an OFN/RDF-XML read produces, and every writer
/// already renders an `xsd:string` datatype bare, so nothing else changes.
fn ann(b: &Build<RcStr>, prop: &str, value: &str) -> Annotation<RcStr> {
    Annotation { ann: Default::default(),
        ap: b.annotation_property(prop),
        av: AnnotationValue::Literal(Literal::Datatype {
            literal: value.to_string(),
            datatype_iri: b.iri(XSD_STRING_IRI),
        }),
    }
}

/// An annotation whose value is an IRI (e.g. a synonym-type id).
fn ann_iri(b: &Build<RcStr>, prop: &str, iri: &str) -> Annotation<RcStr> {
    Annotation { ann: Default::default(),
        ap: b.annotation_property(prop),
        av: AnnotationValue::IRI(b.iri(iri)),
    }
}

/// Split a clause's text into what precedes its `{qualifier}` block and the
/// block.
fn split_qualifier_block(v: &str) -> (&str, &str) {
    match qualifier_block_start(v) {
        Some(i) => (v[..i].trim_end(), &v[i..]),
        None => (v, ""),
    }
}

/// A header `date:` value: an instant read by the pattern `dd:MM:yyyy HH:mm`,
/// in UTC.
///
/// Each field is a number with an optional minus sign after any spaces or tabs,
/// the separators stand as they are, and anything after the minutes is
/// ignored. Fields are lenient, carrying out of their range into the next one
/// (`32:13:2021 25:61` is `02:02:2022 02:01`). The calendar is the Julian one
/// before 15 October 1582 and the Gregorian one from then on: a date before
/// 1582 is counted in the Julian calendar (`29:02:1500` is a day of its own), a
/// date of a later year in the Gregorian one, and a date of 1582 in the
/// Gregorian one unless it falls before the change, where the Julian one
/// counts it (`05:10:1582` is `15:10:1582`).
struct HeaderDate {
    /// Milliseconds from 1970-01-01T00:00 UTC.
    millis: i64,
}

/// The fixed day number of 1 January 1970, day 1 being 1 January 1 of the
/// proleptic Gregorian calendar.
const EPOCH_DAY: i64 = 719_163;
/// The fixed day number of 15 October 1582, the first day of the Gregorian
/// calendar.
const GREGORIAN_CHANGE: i64 = 577_736;
const DAY_MILLIS: i64 = 86_400_000;

impl HeaderDate {
    /// `None` when `text` does not begin with a date.
    fn read(text: &str) -> Option<HeaderDate> {
        let mut rest = text;
        let mut fields = [0i32; 5];
        for (i, sep) in [Some(':'), Some(':'), Some(' '), Some(':'), None].into_iter().enumerate() {
            rest = rest.trim_start_matches([' ', '\t']);
            let (negative, digits) = match rest.strip_prefix('-') {
                Some(d) => (true, d),
                None => (false, rest),
            };
            let n = digits.bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 {
                return None;
            }
            // A number beyond 64 bits saturates; the field keeps its low 32 bits.
            let value = digits[..n].parse::<i64>().unwrap_or(i64::MAX);
            fields[i] = (if negative { -value } else { value }) as i32;
            rest = &digits[n..];
            if let Some(sep) = sep {
                rest = rest.strip_prefix(sep)?;
            }
        }
        let [day, month, year, hour, minute] = fields;
        let time = (i64::from(hour) * 60 + i64::from(minute)) * 60_000;
        let carried = time.div_euclid(DAY_MILLIS);
        // The month carries into the year in 32 bits.
        let (mut y, mut m) = (year, month.wrapping_sub(1));
        if m > 11 {
            y = y.wrapping_add(m / 12);
            m %= 12;
        } else if m < 0 {
            y = y.wrapping_add(m.div_euclid(12));
            m = m.rem_euclid(12);
        }
        let (y, m) = (i64::from(y), i64::from(m) + 1);
        let gregorian = carried + days_from_civil(y, m, 1) + EPOCH_DAY + i64::from(day) - 1;
        let julian = carried + julian_fixed_day(y, m, 1) + i64::from(day) - 1;
        let fixed = if year < 1582 || gregorian < GREGORIAN_CHANGE { julian } else { gregorian };
        let millis = (fixed - EPOCH_DAY).wrapping_mul(DAY_MILLIS).wrapping_add(time.rem_euclid(DAY_MILLIS));
        Some(HeaderDate { millis })
    }

    /// The date's year, month, day, day of the week (0 for Sunday), hour and
    /// minute, in the calendar of its day.
    fn fields(&self) -> (i64, i64, i64, usize, i64, i64) {
        let fixed = self.millis.div_euclid(DAY_MILLIS) + EPOCH_DAY;
        let minutes = self.millis.rem_euclid(DAY_MILLIS) / 60_000;
        let (y, m, d) = if fixed >= GREGORIAN_CHANGE {
            civil_from_days(fixed - EPOCH_DAY)
        } else {
            julian_from_fixed_day(fixed)
        };
        (y, m, d, fixed.rem_euclid(7) as usize, minutes / 60, minutes % 60)
    }

    /// The date by its pattern; a year before 1 is the year before Christ it is
    /// (`0000` is `0001`).
    fn obo(&self) -> String {
        let (y, m, d, _, hour, minute) = self.fields();
        let year_of_era = if y <= 0 { 1 - y } else { y };
        format!("{d:02}:{m:02}:{year_of_era:04} {hour:02}:{minute:02}")
    }

    /// The date as `EEE MMM dd HH:mm:ss zzz yyyy` (`Mon Jan 06 09:00:00 UTC
    /// 2020`), the year unpadded: the text that orders the date clauses of a
    /// header.
    fn sort_text(&self) -> String {
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let (y, m, d, weekday, hour, minute) = self.fields();
        let year_of_era = if y <= 0 { 1 - y } else { y };
        format!("{} {} {d:02} {hour:02}:{minute:02}:00 UTC {year_of_era}", DAYS[weekday], MONTHS[(m - 1) as usize])
    }
}

/// The fixed day number (see [`EPOCH_DAY`]) of the given date of the Julian
/// calendar (`month` 1 to 12, `year` 0 the year before 1).
fn julian_fixed_day(year: i64, month: i64, day: i64) -> i64 {
    let mut days = -2 + 365 * (year - 1) + day + (year - 1).div_euclid(4) + (367 * month - 362).div_euclid(12);
    if month > 2 {
        days -= if year.rem_euclid(4) == 0 { 1 } else { 2 };
    }
    days
}

/// The date of the Julian calendar a fixed day number names, as (year, month,
/// day).
fn julian_from_fixed_day(fixed: i64) -> (i64, i64, i64) {
    let year = (4 * (fixed + 1) + 1464).div_euclid(1461);
    let mut prior = fixed - julian_fixed_day(year, 1, 1);
    if fixed >= julian_fixed_day(year, 3, 1) {
        prior += if year.rem_euclid(4) == 0 { 1 } else { 2 };
    }
    let month = (12 * prior + 373).div_euclid(367);
    (year, month, fixed - julian_fixed_day(year, month, 1) + 1)
}

/// The days from 1970-01-01 to the given date of the proleptic Gregorian
/// calendar (`month` 1 to 12).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date `days` after 1970-01-01, as (year, month, day).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn assert_ann(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, prop: &str, value: &str) {
    ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
        subject: AnnotationSubject::IRI(b.iri(subj)),
        ann: ann(b, prop, value),
    }));
}

/// Assert an annotation assertion carrying its own axiom-level annotations
/// (e.g. an OBO `def:`/`synonym:` line's `[dbxref]` list, which the OBO→OWL
/// mapping turns into `oboInOwl:hasDbXref` annotations on the axiom).
fn assert_ann_with(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    prop: &str,
    value: &str,
    axiom_anns: Vec<Annotation<RcStr>>,
) {
    tag_prop(prop);
    if axiom_anns.is_empty() {
        assert_ann(b, ont, subj, prop, value);
        return;
    }
    ont.insert(horned_owl::model::AnnotatedComponent {
        component: Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(subj)),
            ann: ann(b, prop, value),
        }),
        ann: axiom_anns.into_iter().collect(),
    });
}

/// Like [`assert_ann_with`] but with an IRI value (e.g. `consider`/`replaced_by`
/// point at another term, and that pointer is an IRI, not a literal id string).
fn assert_ann_iri_with(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    prop: &str,
    iri: &str,
    axiom_anns: Vec<Annotation<RcStr>>,
) {
    tag_prop(prop);
    if axiom_anns.is_empty() {
        assert_ann_iri(b, ont, subj, prop, iri);
        return;
    }
    ont.insert(horned_owl::model::AnnotatedComponent {
        component: Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(subj)),
            ann: ann_iri(b, prop, iri),
        }),
        ann: axiom_anns.into_iter().collect(),
    });
}

/// Parse the OBO trailing `[xref, xref, ...]` list from the remainder of a
/// `def:`/`synonym:` value (the part after the quoted string). Each entry's id
/// is the leading token (an optional quoted description is ignored). Returns the
/// xref ids in document order.
fn parse_bracket_xrefs(rest: &str) -> Vec<String> {
    let start = match rest.find('[') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let end = match rest[start..].find(']') {
        Some(i) => start + i,
        None => return Vec::new(),
    };
    let inner = &rest[start + 1..end];
    // Split on *unescaped* commas (an `\,` is part of the id, e.g.
    // `ISBN:9004086161\,9789004086166` is a single xref), then take each id up to
    // the first *unescaped* whitespace or `"` (a trailing description) and
    // unescape OBO escapes (`\:`→`:`, `\,`→`,`, …).
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut esc = false;
    for c in inner.chars() {
        if esc {
            cur.push('\\');
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == ',' {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
        .iter()
        .filter_map(|part| {
            // The xref id is everything up to an unescaped `"` (a trailing quoted
            // description), trimmed. Internal whitespace stays part of the id
            // (e.g. the erroneous `PMID: 5466382`), so do NOT split on it.
            let mut id = String::new();
            let mut esc = false;
            for c in part.chars() {
                if esc {
                    id.push(c);
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    break;
                } else {
                    id.push(c);
                }
            }
            let id = id.trim().to_string();
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        })
        .collect()
}

/// A single `xref:` line's trailing quoted description (`xref: CARO:0000030
/// "asexual organism"`) becomes an `rdfs:label` axiom annotation on the
/// `hasDbXref` axiom.
fn xref_label_ann(b: &Build<RcStr>, x: &str) -> Vec<Annotation<RcStr>> {
    let after = x.splitn(2, char::is_whitespace).nth(1).unwrap_or("").trim_start();
    match parse_quoted(after) {
        Some((label, _)) => vec![ann(b, RDFS_LABEL, &label)],
        None => Vec::new(),
    }
}

/// Build `oboInOwl:hasDbXref` axiom annotations for each xref in an OBO
/// `[dbxref]` list found in the remainder `rest` (the text after the quoted
/// string).
fn dbxref_anns(b: &Build<RcStr>, rest: &str) -> Vec<Annotation<RcStr>> {
    parse_bracket_xrefs(rest)
        .iter()
        .map(|x| ann(b, tag_prop(&format!("{OIO}hasDbXref")), x))
        .collect()
}

/// Where the `{…}` qualifier block of a clause's text starts: its first `{`
/// that no backslash escapes and no quoted string holds.
fn qualifier_block_start(rest: &str) -> Option<usize> {
    let mut quoted = false;
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => quoted = !quoted,
            '{' if !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

/// The `(key, value)` pairs of the qualifier block in a clause's text, in
/// document order (a key may repeat, e.g. several `source=`). A block that does
/// not parse fails the read ([`load`] reports it).
fn parse_qualifiers(rest: &str) -> Vec<(String, String)> {
    let mut quals = Vec::new();
    if let Some(start) = qualifier_block_start(rest) {
        if let Err(e) = Cursor::new(&rest[start..]).qualifier_block(&mut quals) {
            READ_ERROR.with(|r| {
                r.borrow_mut().get_or_insert_with(|| format!("{e}: {rest}"));
            });
        }
    }
    quals
}

/// Axiom-level annotations for a tag's trailing `{…}` qualifier block.
fn qualifier_anns(b: &Build<RcStr>, rest: &str) -> Vec<Annotation<RcStr>> {
    quals_anns(b, &parse_qualifiers(rest))
}

/// The axiom annotations `quals` make, each with the property [`tag_iri`] names
/// for its key. The qualifiers that shape the axiom itself are not annotations:
/// `cardinality`/`minCardinality`/`maxCardinality` (`relation_ce` builds the
/// qualified cardinality restriction of an `intersection_of`, and the
/// relationship handler its exact axiom), `all_some`/`all_only`, and
/// `gci_relation`/`gci_filler`, which build a GCI's subject. The snake-case
/// `min_cardinality`/`max_cardinality` are annotations like any other key.
fn quals_anns(b: &Build<RcStr>, quals: &[(String, String)]) -> Vec<Annotation<RcStr>> {
    quals
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "cardinality"
                    | "minCardinality"
                    | "maxCardinality"
                    | "all_some"
                    | "all_only"
                    | "gci_relation"
                    | "gci_filler"
            )
        })
        .map(|(k, v)| ann(b, tag_prop(&tag_iri(k)), v))
        .collect()
}

/// Declare the class a relation is restricted to — the filler of a
/// `relationship:`, of a relation `intersection_of:` or of a GCI — as the
/// document's own: unlike a class named only by `is_a:`, `disjoint_from:`, a
/// genus or a `union_of:`, a filler is declared even where no frame defines it.
fn declare_filler(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, iri: &str) -> horned_owl::model::Class<RcStr> {
    let class = b.class(iri);
    ont.insert(Component::DeclareClass(DeclareClass(class.clone())));
    class
}

/// The class expression a relation clause makes of the relation `rel` and the
/// filler `filler` (ids as written), by the clause's qualifiers `quals`: an
/// exact cardinality above zero; `rel only not filler` for a cardinality or a
/// maximum of zero; the intersection of a minimum and a maximum given together;
/// a minimum or a maximum alone; `rel only filler` under `all_only`, intersected
/// with `rel some filler` under `all_some` too; `rel value filler` for a
/// class-level relation; `rel some filler` otherwise. Of a repeated qualifier
/// the first counts, and a negative cardinality counts as none. The filler is
/// declared a class whichever expression it is in.
fn relation_ce(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    rel: &str,
    filler: &str,
    quals: &[(String, String)],
    rel_map: &HashMap<String, String>,
    class_level: &BTreeSet<String>,
) -> CE<RcStr> {
    let ope = || OPE::ObjectProperty(b.object_property(resolve_rel(rel, rel_map)));
    let filler_iri = expand_curie(filler);
    let class = declare_filler(b, ont, &filler_iri);
    let bce = || Box::new(CE::Class(class.clone()));
    let first = |key: &str| quals.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let count = |key: &str| first(key).map_or(-1, qualifier_int);
    let flag = |key: &str| first(key).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let (exact, min, max) = (count("cardinality"), count("minCardinality"), count("maxCardinality"));
    if exact > 0 {
        CE::ObjectExactCardinality { n: exact as u32, ope: ope(), bce: bce() }
    } else if exact == 0 || max == 0 {
        CE::ObjectAllValuesFrom { ope: ope(), bce: Box::new(CE::ObjectComplementOf(bce())) }
    } else if min > -1 && max > -1 {
        CE::ObjectIntersectionOf(vec![
            CE::ObjectMinCardinality { n: min as u32, ope: ope(), bce: bce() },
            CE::ObjectMaxCardinality { n: max as u32, ope: ope(), bce: bce() },
        ])
    } else if min > -1 {
        CE::ObjectMinCardinality { n: min as u32, ope: ope(), bce: bce() }
    } else if max > -1 {
        CE::ObjectMaxCardinality { n: max as u32, ope: ope(), bce: bce() }
    } else if flag("all_only") && flag("all_some") {
        CE::ObjectIntersectionOf(vec![
            CE::ObjectSomeValuesFrom { ope: ope(), bce: bce() },
            CE::ObjectAllValuesFrom { ope: ope(), bce: bce() },
        ])
    } else if flag("all_only") {
        CE::ObjectAllValuesFrom { ope: ope(), bce: bce() }
    } else if class_level.contains(rel) {
        CE::ObjectHasValue { ope: ope(), i: Individual::Named(b.named_individual(filler_iri.as_str())) }
    } else {
        CE::ObjectSomeValuesFrom { ope: ope(), bce: bce() }
    }
}

/// A cardinality qualifier's value: an integer with an optional sign. Any other
/// value fails the read ([`load`] reports it).
fn qualifier_int(v: &str) -> i32 {
    v.parse().unwrap_or_else(|_| {
        READ_ERROR.with(|e| {
            e.borrow_mut().get_or_insert_with(|| format!("a cardinality is not an integer: \"{v}\""));
        });
        -1
    })
}

/// The subject a term's clause states its axiom of: the term `iri`, or under
/// `gci_relation` and `gci_filler` qualifiers, the term intersected with the
/// relation expression of the two. An empty `gci_relation` names no relation,
/// and one with no `gci_filler` fails the read ([`load`] reports it).
fn clause_subject(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    iri: &str,
    quals: &[(String, String)],
    rel_map: &HashMap<String, String>,
    class_level: &BTreeSet<String>,
) -> CE<RcStr> {
    let first = |key: &str| quals.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let term = CE::Class(b.class(iri));
    let Some(rel) = first("gci_relation").filter(|r| !r.is_empty()) else { return term };
    let Some(filler) = first("gci_filler") else {
        READ_ERROR.with(|e| {
            e.borrow_mut().get_or_insert_with(|| format!("a gci_relation qualifier with no gci_filler: {rel}"));
        });
        return term;
    };
    CE::ObjectIntersectionOf(vec![term, relation_ce(b, ont, rel, filler, &[], rel_map, class_level)])
}

/// Insert a logical axiom carrying axiom-level annotations (e.g. `is_a`/
/// `relationship` lines whose `{source=…}` qualifiers map to annotations).
fn insert_annotated(
    ont: &mut SetOntology<RcStr>,
    component: Component<RcStr>,
    anns: Vec<Annotation<RcStr>>,
) {
    if anns.is_empty() {
        ont.insert(component);
    } else {
        ont.insert(horned_owl::model::AnnotatedComponent {
            component,
            ann: anns.into_iter().collect(),
        });
    }
}

/// Assert an annotation whose value is an IRI (rather than a literal).
fn assert_ann_iri(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, prop: &str, iri: &str) {
    ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
        subject: AnnotationSubject::IRI(b.iri(subj)),
        ann: Annotation { ann: Default::default(),
            ap: b.annotation_property(prop),
            av: AnnotationValue::IRI(b.iri(iri)),
        },
    }));
}

const OIO_BUILTIN: &str = "http://www.geneontology.org/formats/oboInOwl#builtin";
const OIO_IS_ANONYMOUS: &str = "http://www.geneontology.org/formats/oboInOwl#is_anonymous";
const IAO_ANTISYMMETRIC: &str = "http://purl.obolibrary.org/obo/IAO_0000427";

/// Each boolean `tag` of a stanza, `true` or `false`, as an `xsd:boolean`
/// annotation of its property on `subj`; a trailing `{…}` qualifier block
/// annotates the assertion.
fn boolean_tags(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, st: &Stanza, tags: &[(&str, &str)]) {
    for (tag, prop) in tags {
        for v in st.all(tag) {
            boolean_clause(b, ont, subj, v, prop);
        }
    }
}

/// A boolean clause's value `v`, `true` or `false`, as an `xsd:boolean`
/// annotation of `prop` on `subj`, annotated with the clause's qualifiers.
fn boolean_clause(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, v: &str, prop: &str) {
    let Some(value @ ("true" | "false")) = v.split_whitespace().next() else { return };
    insert_annotated(
        ont,
        Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(subj)),
            ann: Annotation {
                ann: Default::default(),
                ap: b.annotation_property(tag_prop(prop)),
                av: AnnotationValue::Literal(Literal::Datatype {
                    literal: value.to_string(),
                    datatype_iri: b.iri(XSD_BOOLEAN),
                }),
            },
        }),
        qualifier_anns(b, v),
    );
}

/// Assert an annotation whose value is a datatyped literal.
fn assert_ann_typed(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    prop: &str,
    value: &str,
    datatype: &str,
) {
    ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
        subject: AnnotationSubject::IRI(b.iri(subj)),
        ann: Annotation { ann: Default::default(),
            ap: b.annotation_property(prop),
            av: AnnotationValue::Literal(Literal::Datatype {
                literal: value.to_string(),
                datatype_iri: b.iri(datatype),
            }),
        },
    }));
}

/// Emit an OBO `property_value:` tag as an annotation assertion. Forms:
///   `property_value: REL "literal" DATATYPE`  → datatyped literal
///   `property_value: REL "literal"`           → plain literal
///   `property_value: REL TARGET_ID`           → IRI value
/// Parse an OBO `property_value` body into its `Annotation` plus any trailing
/// `{…}` qualifier annotations. Used both for term/typedef assertions and for
/// the ontology header (where it becomes an `OntologyAnnotation`).
fn property_value_annotation(
    b: &Build<RcStr>,
    value: &str,
    rel_map: &HashMap<String, String>,
    onto_ns: Option<&str>,
    nl_to_space: bool,
) -> Option<(Annotation<RcStr>, Vec<Annotation<RcStr>>)> {
    let v = value.trim();
    let (rel, rest) = v.split_once(char::is_whitespace).map(|(r, rest)| (r, rest.trim()))?;
    // An undeclared, unprefixed relation (e.g. `seeAlso`) is an ontology-local
    // annotation property `obo/<ontology>#<rel>`, not the generic `obo/<rel>`.
    let prop = rel_map.get(rel).cloned().unwrap_or_else(|| match onto_ns {
        Some(ns) if !rel.contains(':') && !rel.starts_with("http") => format!("{ns}{rel}"),
        _ => expand_curie(rel),
    });
    let anns = qualifier_anns(b, value);
    // `nl_to_space` is the stanza-level flag the callers thread in (a Term and the
    // header pass `true`; a Typedef passes whether the value has a space-adjacent
    // `\n`). An OBO `\n` unescapes to a literal newline in every case — see
    // `parse_quoted_nl` — so it does not change the value built here.
    let av = if let Some((lit, after)) = parse_quoted_nl(rest, nl_to_space) {
        let dt = after.trim().split_whitespace().next().unwrap_or("");
        if dt.is_empty() || dt == "xsd:string" || dt.starts_with('{') {
            // Explicitly `xsd:string`, for the reason spelled out on `ann`.
            AnnotationValue::Literal(Literal::Datatype {
                literal: lit,
                datatype_iri: b.iri(XSD_STRING_IRI),
            })
        } else {
            AnnotationValue::Literal(Literal::Datatype {
                literal: lit,
                datatype_iri: b.iri(expand_datatype(dt)),
            })
        }
    } else {
        let target = rest.split_whitespace().next().unwrap_or(rest);
        AnnotationValue::IRI(b.iri(expand_curie(target)))
    };
    Some((Annotation { ann: Default::default(), ap: b.annotation_property(prop.as_str()), av }, anns))
}

fn assert_property_value(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    value: &str,
    rel_map: &HashMap<String, String>,
    onto_ns: Option<&str>,
    nl_to_space: bool,
) {
    if let Some((ann, anns)) = property_value_annotation(b, value, rel_map, onto_ns, nl_to_space) {
        insert_annotated(
            ont,
            Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(subj)),
                ann,
            }),
            anns,
        );
    }
}

/// Expand a datatype id. Built-in prefixes (`xsd`/`rdf`/`rdfs`/`owl`) map to
/// their standard namespaces; everything else expands as any other id.
fn expand_datatype(dt: &str) -> String {
    match dt.split_once(':') {
        Some(("xsd", local)) => format!("http://www.w3.org/2001/XMLSchema#{local}"),
        Some(("rdf", local)) => format!("http://www.w3.org/1999/02/22-rdf-syntax-ns#{local}"),
        Some(("rdfs", local)) => format!("http://www.w3.org/2000/01/rdf-schema#{local}"),
        Some(("owl", local)) => format!("http://www.w3.org/2002/07/owl#{local}"),
        _ => expand_curie(dt),
    }
}

/// Parse an OBO-quoted string at the start of `s` (which must begin with `"`),
/// honoring `\"`/`\\`/`\n`/`\t` escapes and multi-byte characters. Returns the
/// unescaped content and the remainder after the closing quote.
/// Unescape an OBO string value (`\n`→newline, `\t`→tab, `\W`→space, and a
/// backslash before any other char drops the backslash). Unquoted values such as
/// `comment:` are unescaped too.
fn unescape_obo(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            out.push(match c {
                // An OBO `\n` escape inside a quoted value (def, comment, synonym,
                // property_value) is a LITERAL NEWLINE in the OWL literal, in a
                // Term and a Typedef alike — the text genuinely spans lines, and
                // collapsing it to a space would rewrite the curator's wording.
                'n' => '\n',
                't' => '\t',
                'W' => ' ',
                other => other,
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c);
        }
    }
    if escaped {
        out.push('\\');
    }
    out
}

fn parse_quoted(s: &str) -> Option<(String, &str)> {
    parse_quoted_nl(s, true)
}

/// Like [`parse_quoted`], but taking the `nl_to_space` flag callers thread through
/// for the Term/Typedef distinction. An OBO `\n` escape becomes a literal newline
/// in either stanza kind, so the flag does not change what this parses.
fn parse_quoted_nl(s: &str, nl_to_space: bool) -> Option<(String, &str)> {
    let mut chars = s.char_indices();
    if chars.next().map(|(_, c)| c) != Some('"') {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for (idx, c) in chars {
        if escaped {
            out.push(match c {
                // Always a literal newline — see the note in `unescape_obo`; `\n`
                // is kept in Term AND Typedef quoted values alike, so `nl_to_space`
                // does not select between them.
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some((out, &s[idx + c.len_utf8()..]));
        } else {
            out.push(c);
        }
    }
    None
}

fn synonym_property(value: &str) -> &'static str {
    // synonym: "text" SCOPE [xrefs]
    let after = parse_quoted(value.trim()).map(|(_, a)| a).unwrap_or("");
    let scope = after.trim().split_whitespace().next().unwrap_or("");
    match scope {
        "EXACT" => "hasExactSynonym",
        "NARROW" => "hasNarrowSynonym",
        "BROAD" => "hasBroadSynonym",
        _ => "hasRelatedSynonym",
    }
}

/// Resolve an OBO subset/synonym-type name to an IRI: prefixed ids
/// (`OMO:0003011`) expand normally; bare local names (`efo_slim`, `SENSU`) live
/// in the ontology's own namespace (`onto_ns`).
fn resolve_local(name: &str, onto_ns: Option<&str>) -> String {
    if name.contains(':') {
        expand_curie(name)
    } else if let Some(ns) = onto_ns {
        format!("{ns}{name}")
    } else {
        expand_obo_id(name)
    }
}

/// Resolve an OBO relation reference to its property IRI, honoring relation
/// shorthands (`disease_has_basis_in_dysfunction_of` ⇒ `RO_0004020`).
fn resolve_rel(r: &str, rel_map: &HashMap<String, String>) -> String {
    rel_map.get(r).cloned().unwrap_or_else(|| expand_curie(r))
}

/// The IRI of a `[Typedef]` with a bare (non-CURIE) `id`: its single `xref`
/// (a full IRI as-is, a CURIE expanded) if present, else the OBO default
/// namespace `obo/<id>` — an unprefixed relation id maps to the default OBO PURL
/// prefix plus the id (e.g. `part_of` → `obo/part_of`), the same mapping
/// [`expand_id`]/`resolve_rel` apply to a `relationship:` reference, so the
/// typedef and its uses resolve to one IRI.
fn typedef_iri(id: &str, xref: Option<&str>, onto_ns: Option<&str>) -> String {
    if let Some(x) = xref {
        let x = x.split_whitespace().next().unwrap_or(x);
        if x.starts_with("http") {
            return x.to_string();
        }
        if x.contains(':') {
            return expand_curie(x);
        }
    }
    // A bare-name property with no usable xref is ontology-native: it lives in the
    // ontology's own namespace (`obo/uberon/core#extends_fibers_into`), not the
    // generic `obo/` namespace.
    resolve_local(id, onto_ns)
}

/// The tags a `[Term]` and an `[Instance]` frame read alike, as annotation
/// assertions on the frame's `iri`: its id, names, namespace, provenance,
/// `property_value:`s, definition, comments, synonyms, xrefs, subsets,
/// obsolescence and alternative ids.
#[allow(clippy::too_many_arguments)]
fn frame_annotations_to_owl(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    st: &Stanza,
    iri: &str,
    id: &str,
    default_ns: Option<&str>,
    onto_ns: Option<&str>,
    rel_map: &HashMap<String, String>,
) {
    // Every frame carries its OBO id as an `oboInOwl:id` annotation.
    assert_ann(b, ont, iri, tag_prop(&format!("{OIO}id")), id);

    // EVERY `name:` clause, not just the first. One line is written per
    // `rdfs:label` (see the `name` emission), so a stanza can legitimately carry
    // several: GO:0051705 in MONDO has both "multi-organism behavior" and
    // "obsolete multi-organism behavior". Reading only the first would halve them
    // on an obo→obo trip, which MONDO's build performs — a step re-reads the
    // written target and the artefact's write re-serialises it.
    assert_clauses(b, ont, iri, st, "name", RDFS_LABEL);
    // `hasOBONamespace`: explicit `namespace`, else the header default.
    frame_namespace(b, ont, iri, st, default_ns);
    assert_clauses(b, ont, iri, st, "created_by", &format!("{OIO}created_by"));
    assert_clauses(b, ont, iri, st, "creation_date", &format!("{OIO}creation_date"));
    for pv in st.all("property_value") {
        // A [Term] or [Instance] property_value. The stanza flag is `true` here, but
        // a `\n` in the value unescapes to a literal newline either way.
        assert_property_value(b, ont, iri, pv, rel_map, onto_ns, true);
    }
    if let Some(raw) = st.get("def") {
        if let Some((def, rest)) = parse_quoted(raw.trim()) {
            let mut anns = dbxref_anns(b, rest);
            anns.extend(qualifier_anns(b, rest));
            assert_ann_with(b, ont, iri, IAO_DEF, &def, anns);
        }
    }
    // Every `comment:` line, not just the first — CL has 28 terms carrying two
    // (e.g. alveolar macrophage's marker-set note alongside its morphology note),
    // and taking only the first would drop them on an obo→owl→obo trip.
    assert_clauses(b, ont, iri, st, "comment", RDFS_COMMENT);
    for c in st.clauses("synonym") {
        let syn = c.value.as_str();
        let prop = synonym_property(syn);
        if let Some((text, rest)) = parse_quoted(syn.trim()) {
            // `synonym: "text" SCOPE [TYPEID] [xrefs]` — a synonym-type id may sit
            // between the scope and the `[xref]` list; map it to a
            // `hasSynonymType` annotation (matching the OBO→OWL mapping).
            let mut anns = dbxref_anns(b, rest);
            let before_brackets = rest.split('[').next().unwrap_or("");
            let mut toks = before_brackets.split_whitespace();
            toks.next(); // skip the scope token (EXACT/NARROW/…)
            if let Some(type_id) = toks.next() {
                anns.push(ann_iri(
                    b,
                    tag_prop(&format!("{OIO}hasSynonymType")),
                    &resolve_local(type_id, onto_ns),
                ));
            }
            anns.extend(qualifier_anns(b, rest));
            anns.extend(quals_anns(b, &c.quals));
            assert_ann_with(b, ont, iri, &format!("{OIO}{prop}"), &text, anns);
        }
    }
    for x in st.all("xref") {
        let id = x.split_whitespace().next().unwrap_or(x);
        // OBO escapes in the xref id are unescaped (e.g. an escaped colon
        // `Category\:Embryonic` → `Category:Embryonic`).
        let id = unescape_obo(id);
        let mut anns = xref_label_ann(b, x);
        anns.extend(qualifier_anns(b, x));
        assert_ann_with(b, ont, iri, &format!("{OIO}hasDbXref"), &id, anns);
    }
    frame_subsets(b, ont, iri, st, onto_ns);
    // `is_obsolete:` (possibly with a trailing `{source=…}` qualifier, which
    // becomes an axiom annotation on the owl:deprecated assertion), and the other
    // boolean tags a term can carry.
    boolean_tags(b, ont, iri, st, &[("is_obsolete", OWL_DEPRECATED), ("builtin", OIO_BUILTIN), ("is_anonymous", OIO_IS_ANONYMOUS)]);
    // Obsolescence pointers: a frame's own `replaced_by:`/`consider:` tags point
    // at other entities, and that pointer is an **IRI** (e.g.
    // `<obo/UBERON_0000965>`), not a literal id string.
    for rb in st.all("replaced_by") {
        let t = rb.split_whitespace().next().unwrap_or(rb);
        assert_ann_iri_with(b, ont, iri, IAO_TERM_REPLACED_BY, &expand_curie(t), qualifier_anns(b, rb));
    }
    for c in st.all("consider") {
        let t = c.split_whitespace().next().unwrap_or(c);
        assert_ann_iri_with(b, ont, iri, &format!("{OIO}consider"), &expand_curie(t), qualifier_anns(b, c));
    }
    assert_clauses(b, ont, iri, st, "alt_id", &format!("{OIO}hasAlternativeId"));
}

/// Each `tag` clause of a frame as an annotation assertion of `prop` on `subj`,
/// annotated with the clause's qualifiers.
fn assert_clauses(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, st: &Stanza, tag: &str, prop: &str) {
    for c in st.clauses(tag) {
        assert_ann_with(b, ont, subj, prop, &c.value, quals_anns(b, &c.quals));
    }
}

/// A frame's `oboInOwl:hasOBONamespace`: each `namespace:` clause, or the
/// header's `default-namespace` when it has none.
fn frame_namespace(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, st: &Stanza, default_ns: Option<&str>) {
    let prop = format!("{OIO}hasOBONamespace");
    if st.get("namespace").is_some() {
        assert_clauses(b, ont, subj, st, "namespace", &prop);
    } else if let Some(ns) = default_ns {
        assert_ann(b, ont, subj, tag_prop(&prop), ns);
    }
}

/// Each `subset:` of a frame as an `oboInOwl:inSubset` of the subset's IRI.
fn frame_subsets(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, st: &Stanza, onto_ns: Option<&str>) {
    for c in st.clauses("subset") {
        if c.value.contains(' ') {
            READ_ERROR.with(|e| {
                e.borrow_mut().get_or_insert_with(|| format!("spaces not allowed: '{}'", c.value));
            });
        }
        let sid = c.value.as_str();
        insert_annotated(
            ont,
            Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(subj)),
                ann: ann_iri(b, tag_prop(&format!("{OIO}inSubset")), &resolve_local(sid, onto_ns)),
            }),
            quals_anns(b, &c.quals),
        );
    }
}

/// Each clause of a frame whose tag the frame has no rule for, as an annotation
/// assertion of the property [`tag_iri`] names, annotated with its qualifiers.
fn custom_tags(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, st: &Stanza, own: impl Fn(&str) -> bool) {
    for c in st.tags.iter().filter(|c| !own(&c.tag)) {
        assert_ann_with(b, ont, subj, &tag_iri(&c.tag), &c.value, quals_anns(b, &c.quals));
    }
}

#[allow(clippy::too_many_arguments)]
fn term_to_owl(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    st: &Stanza,
    default_ns: Option<&str>,
    onto_ns: Option<&str>,
    rel_map: &HashMap<String, String>,
    metadata_tags: &BTreeSet<String>,
    class_level: &BTreeSet<String>,
) {
    let id = match st.get("id") {
        Some(i) => i,
        None => return,
    };
    let iri = expand_curie(id);
    ont.insert(Component::DeclareClass(DeclareClass(b.class(iri.clone()))));
    frame_annotations_to_owl(b, ont, st, &iri, id, default_ns, onto_ns, rel_map);
    custom_tags(b, ont, &iri, st, |tag| TERM_TAGS.contains(&tag));
    // Each alt_id is also materialised as its own *deprecated* class, merged
    // into (replaced_by) the primary term with obsolescence reason "terms
    // merged": owl:deprecated + IAO_0100001 + IAO_0000231.
    for a in st.all("alt_id") {
        let t = a.split_whitespace().next().unwrap_or(a);
        let alt = expand_curie(t);
        if alt != *iri {
            ont.insert(Component::DeclareClass(DeclareClass(b.class(alt.as_str()))));
            assert_ann_typed(b, ont, &alt, OWL_DEPRECATED, "true", XSD_BOOLEAN);
            assert_ann_iri(b, ont, &alt, IAO_TERM_REPLACED_BY, &iri);
            assert_ann_iri(b, ont, &alt, IAO_OBSOLESCENCE_REASON, IAO_TERMS_MERGED);
        }
    }

    // Logical axioms.
    for parent in st.all("is_a") {
        let pid = parent.split_whitespace().next().unwrap_or(parent);
        let sub = clause_subject(b, ont, &iri, &parse_qualifiers(parent), rel_map, class_level);
        insert_annotated(
            ont,
            Component::SubClassOf(SubClassOf {
                sub,
                sup: CE::Class(b.class(expand_curie(pid))),
            }),
            qualifier_anns(b, parent),
        );
    }
    // Each `relationship:` line is one axiom: an annotation assertion when the
    // relation is a metadata tag, otherwise the term's subclass axiom of the
    // relation expression the line's qualifiers make, annotated with the rest.
    for rel in st.all("relationship") {
        let mut parts = rel.split_whitespace();
        if let (Some(r), Some(target)) = (parts.next(), parts.next()) {
            let rel_iri = resolve_rel(r, rel_map);
            if metadata_tags.contains(&rel_iri) {
                insert_annotated(
                    ont,
                    Component::AnnotationAssertion(AnnotationAssertion {
                        subject: AnnotationSubject::IRI(b.iri(iri.as_str())),
                        ann: ann_iri(b, &rel_iri, &expand_curie(target)),
                    }),
                    qualifier_anns(b, rel),
                );
            } else {
                let quals = parse_qualifiers(rel);
                let sub = clause_subject(b, ont, &iri, &quals, rel_map, class_level);
                let sup = relation_ce(b, ont, r, target, &quals, rel_map, class_level);
                insert_annotated(ont, Component::SubClassOf(SubClassOf { sub, sup }), quals_anns(b, &quals));
            }
        }
    }
    for d in st.all("disjoint_from") {
        let did = d.split_whitespace().next().unwrap_or(d);
        let sub = clause_subject(b, ont, &iri, &parse_qualifiers(d), rel_map, class_level);
        insert_annotated(
            ont,
            Component::DisjointClasses(DisjointClasses(vec![sub, CE::Class(b.class(expand_curie(did)))])),
            qualifier_anns(b, d),
        );
    }
    // intersection_of lines combine into one EquivalentClasses(X, And(...)).
    let inter: Vec<&str> = st.all("intersection_of").collect();
    if !inter.is_empty() {
        let mut conj: Vec<CE<RcStr>> = Vec::new();
        let mut anns: Vec<Annotation<RcStr>> = Vec::new();
        for line in inter {
            anns.extend(qualifier_anns(b, line));
            // Strip the trailing `{…}` qualifier block before tokenizing operands.
            let body = line.split('{').next().unwrap_or(line);
            let toks: Vec<&str> = body.split_whitespace().collect();
            match toks.as_slice() {
                [genus] => conj.push(CE::Class(b.class(expand_curie(genus)))),
                [rel, filler] => {
                    conj.push(relation_ce(b, ont, rel, filler, &parse_qualifiers(line), rel_map, class_level))
                }
                _ => {}
            }
        }
        if conj.len() >= 2 {
            insert_annotated(
                ont,
                Component::EquivalentClasses(EquivalentClasses(vec![
                    CE::Class(b.class(iri.clone())),
                    CE::ObjectIntersectionOf(conj),
                ])),
                anns,
            );
        }
    }
    // union_of lines combine into one EquivalentClasses(X, Or(...)).
    let union: Vec<&str> = st.all("union_of").collect();
    if union.len() >= 2 {
        let mut disj: Vec<CE<RcStr>> = Vec::new();
        let mut anns: Vec<Annotation<RcStr>> = Vec::new();
        for line in union {
            anns.extend(qualifier_anns(b, line));
            let body = line.split('{').next().unwrap_or(line);
            if let Some(member) = body.split_whitespace().next() {
                disj.push(CE::Class(b.class(expand_curie(member))));
            }
        }
        if disj.len() >= 2 {
            insert_annotated(
                ont,
                Component::EquivalentClasses(EquivalentClasses(vec![
                    CE::Class(b.class(iri.clone())),
                    CE::ObjectUnionOf(disj),
                ])),
                anns,
            );
        }
    }
    for line in st.all("equivalent_to") {
        let eq = line.split_whitespace().next().unwrap_or(line);
        let sub = clause_subject(b, ont, &iri, &parse_qualifiers(line), rel_map, class_level);
        insert_annotated(
            ont,
            Component::EquivalentClasses(EquivalentClasses(vec![sub, CE::Class(b.class(expand_curie(eq)))])),
            qualifier_anns(b, line),
        );
    }
}

/// A `[Typedef]` frame: an annotation property when it is a metadata tag
/// (`is_metadata_tag: true`), an object property otherwise. Both take the
/// annotation tags a `[Term]` takes. An object property's relational tags are
/// axioms (`is_a`, `domain`, `inverse_of`, the chains, a characteristic stated
/// true, …), and its `relationship:` is an annotation assertion when the relation
/// is a metadata tag and nothing otherwise. A metadata tag has no axioms of its
/// own: its `is_a` names nothing, and every other clause is an annotation of the
/// property [`tag_iri`] names for the tag, a relational one valued with the
/// literal of the clause's first id.
fn typedef_to_owl(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    st: &Stanza,
    default_ns: Option<&str>,
    onto_ns: Option<&str>,
    rel_map: &HashMap<String, String>,
    metadata_tags: &BTreeSet<String>,
) {
    let id = match st.get("id") {
        Some(i) => i,
        None => return,
    };
    // Use the same resolved IRI relation usages do (xref / ontology namespace).
    let iri = resolve_rel(id, rel_map);
    let metadata = metadata_tags.contains(&iri);
    if metadata {
        ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
            b.annotation_property(iri.as_str()),
        )));
    } else {
        ont.insert(Component::DeclareObjectProperty(DeclareObjectProperty(
            b.object_property(iri.clone()),
        )));
    }
    assert_ann(b, ont, &iri, tag_prop(&format!("{OIO}id")), id);
    // Every `name:` clause — see the term reader above.
    assert_clauses(b, ont, &iri, st, "name", RDFS_LABEL);
    frame_namespace(b, ont, &iri, st, default_ns);
    frame_subsets(b, ont, &iri, st, onto_ns);
    custom_tags(b, ont, &iri, st, |tag| TYPEDEF_TAGS.contains(&tag));
    // `disjoint_over: R` has no axiom of its own: the id is a literal of the tag's
    // property.
    assert_clauses(b, ont, &iri, st, "disjoint_over", &tag_iri("disjoint_over"));
    if let Some(raw) = st.get("def") {
        if let Some((def, rest)) = parse_quoted(raw.trim()) {
            // Carry the def's trailing `[dbxref]` list (and any `{qualifier}`) as
            // axiom annotations, exactly as terms do: a typedef's IAO_0000115
            // carries them too.
            let mut anns = dbxref_anns(b, rest);
            anns.extend(qualifier_anns(b, rest));
            assert_ann_with(b, ont, &iri, IAO_DEF, &def, anns);
        }
    }
    // Every `comment:` line, not just the first — CL has 28 terms carrying two
    // (e.g. alveolar macrophage's marker-set note alongside its morphology note),
    // and taking only the first would drop them on an obo→owl→obo trip.
    assert_clauses(b, ont, &iri, st, "comment", RDFS_COMMENT);
    // A bare-name id is an OBO `shorthand` *only* when an `xref` remaps it to an
    // external IRI (e.g. `aboral_to` → `BSPO_0015202`): the bare name then aliases
    // that IRI. A bare id with no such xref simply lives in the ontology's own
    // namespace (`<onto>#<id>`) and is the IRI's local name, not a shorthand — so
    // no `oboInOwl:shorthand` is emitted for it.
    let remapped_by_xref = st.get("xref").is_some_and(|x| {
        let x = x.split_whitespace().next().unwrap_or(x);
        x.starts_with("http") || x.contains(':')
    });
    if !id.contains(':') && remapped_by_xref {
        assert_ann(b, ont, &iri, tag_prop(&format!("{OIO}shorthand")), id);
    }
    for x in st.all("xref") {
        let id = unescape_obo(x.split_whitespace().next().unwrap_or(x));
        if !id.is_empty() {
            let mut anns = xref_label_ann(b, x);
            anns.extend(qualifier_anns(b, x));
            assert_ann_with(b, ont, &iri, &format!("{OIO}hasDbXref"), &id, anns);
        }
    }
    boolean_tags(
        b,
        ont,
        &iri,
        st,
        &[
            ("is_metadata_tag", &format!("{OIO}is_metadata_tag")),
            ("is_class_level", &format!("{OIO}is_class_level")),
            ("is_anti_symmetric", IAO_ANTISYMMETRIC),
            ("is_cyclic", &format!("{OIO}is_cyclic")),
        ],
    );
    // A characteristic of an object property is an axiom when it holds; stated
    // not to hold, or of a metadata tag, it is the tag's own annotation.
    for tag in ["is_transitive", "is_symmetric", "is_reflexive", "is_asymmetric", "is_functional", "is_inverse_functional"] {
        for v in st.all(tag) {
            if !metadata && v.split_whitespace().next() == Some("true") {
                let ope = OPE::ObjectProperty(b.object_property(iri.clone()));
                let axiom = match tag {
                    "is_transitive" => Component::TransitiveObjectProperty(TransitiveObjectProperty(ope)),
                    "is_symmetric" => Component::SymmetricObjectProperty(SymmetricObjectProperty(ope)),
                    "is_reflexive" => Component::ReflexiveObjectProperty(ReflexiveObjectProperty(ope)),
                    "is_asymmetric" => Component::AsymmetricObjectProperty(AsymmetricObjectProperty(ope)),
                    "is_functional" => Component::FunctionalObjectProperty(FunctionalObjectProperty(ope)),
                    _ => Component::InverseFunctionalObjectProperty(InverseFunctionalObjectProperty(ope)),
                };
                insert_annotated(ont, axiom, qualifier_anns(b, v));
            } else {
                boolean_clause(b, ont, &iri, v, &format!("{OIO}{tag}"));
            }
        }
    }
    assert_clauses(b, ont, &iri, st, "created_by", &format!("{OIO}created_by"));
    assert_clauses(b, ont, &iri, st, "creation_date", &format!("{OIO}creation_date"));
    for pv in st.all("property_value") {
        // Typedef (object-property) property_value: an OBO `\n` stays a literal
        // newline (e.g. BFO_0000050/51's parthood comment). `space_adjacent_nl`
        // marks a value whose newline sits next to a space (` \n` / `\n `, as in
        // RO_0002410's bulleted causal-relations comment); it reaches
        // `parse_quoted_nl` as the stanza flag, which keeps the newline regardless.
        let space_adjacent_nl = pv.contains(" \\n") || pv.contains("\\n ");
        assert_property_value(b, ont, &iri, pv, rel_map, onto_ns, space_adjacent_nl);
    }
    // Obsolescence on a typedef behaves exactly as on a term: `owl:deprecated true`
    // plus the obsolescence pointers (`replaced_by` → IAO_0100001, `consider` →
    // oboInOwl:consider) on the property. Without this the deprecation metadata the
    // release carries is lost.
    boolean_tags(b, ont, &iri, st, &[("is_obsolete", OWL_DEPRECATED), ("builtin", OIO_BUILTIN), ("is_anonymous", OIO_IS_ANONYMOUS)]);
    for rb in st.all("replaced_by") {
        let t = rb.split_whitespace().next().unwrap_or(rb);
        assert_ann_iri_with(b, ont, &iri, IAO_TERM_REPLACED_BY, &expand_curie(t), qualifier_anns(b, rb));
    }
    for c in st.all("consider") {
        let t = c.split_whitespace().next().unwrap_or(c);
        assert_ann_iri_with(b, ont, &iri, &format!("{OIO}consider"), &expand_curie(t), qualifier_anns(b, c));
    }
    // The OBO macro tags map back to their IAO annotation properties (the value is
    // a quoted Manchester template, so the leading `"` must be consumed).
    for (tag, prop) in [
        ("expand_expression_to", IAO_EXPAND_EXPRESSION_TO),
        ("expand_assertion_to", IAO_EXPAND_ASSERTION_TO),
    ] {
        for v in st.all(tag) {
            if let Some((text, rest)) = parse_quoted(v.trim()) {
                assert_ann_with(b, ont, &iri, prop, &text, dbxref_anns(b, rest));
            }
        }
    }
    // Property synonyms map exactly as term synonyms do.
    for c in st.clauses("synonym") {
        let syn = c.value.as_str();
        let prop = synonym_property(syn);
        if let Some((text, rest)) = parse_quoted(syn.trim()) {
            let mut anns = dbxref_anns(b, rest);
            let before_brackets = rest.split('[').next().unwrap_or("");
            let mut toks = before_brackets.split_whitespace();
            toks.next();
            if let Some(type_id) = toks.next() {
                anns.push(ann_iri(b, tag_prop(&format!("{OIO}hasSynonymType")), &resolve_local(type_id, onto_ns)));
            }
            anns.extend(qualifier_anns(b, rest));
            anns.extend(quals_anns(b, &c.quals));
            assert_ann_with(b, ont, &iri, &format!("{OIO}{prop}"), &text, anns);
        }
    }
    if metadata {
        assert_clauses(b, ont, &iri, st, "alt_id", &format!("{OIO}hasAlternativeId"));
        for tag in [
            "domain", "range", "inverse_of", "transitive_over", "holds_over_chain", "equivalent_to_chain",
            "equivalent_to", "disjoint_from", "intersection_of", "union_of", "relationship",
        ] {
            for v in st.all(tag) {
                let (head, quals) = split_qualifier_block(v);
                if let Some(first) = head.split_whitespace().next() {
                    assert_ann_with(b, ont, &iri, &tag_iri(tag), first, qualifier_anns(b, quals));
                }
            }
        }
        return;
    }
    // Each alt_id of an object property is a deprecated object property of its
    // own, merged into this one.
    for a in st.all("alt_id") {
        let alt = expand_curie(a);
        if alt != iri {
            ont.insert(Component::DeclareObjectProperty(DeclareObjectProperty(b.object_property(alt.as_str()))));
            assert_ann_typed(b, ont, &alt, OWL_DEPRECATED, "true", XSD_BOOLEAN);
            assert_ann_iri(b, ont, &alt, IAO_TERM_REPLACED_BY, &iri);
            assert_ann_iri(b, ont, &alt, IAO_OBSOLESCENCE_REASON, IAO_TERMS_MERGED);
        }
    }
    for parent in st.all("is_a") {
        let parent = parent.split_whitespace().next().unwrap_or(parent);
        ont.insert(Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(
                OPE::ObjectProperty(b.object_property(iri.clone())),
            ),
            sup: OPE::ObjectProperty(b.object_property(resolve_rel(parent, rel_map))),
        }));
    }
    for rel in st.all("relationship") {
        let (head, quals) = split_qualifier_block(rel);
        let mut parts = head.split_whitespace();
        if let (Some(r), Some(target)) = (parts.next(), parts.next()) {
            let rel_iri = resolve_rel(r, rel_map);
            if metadata_tags.contains(&rel_iri) {
                insert_annotated(
                    ont,
                    Component::AnnotationAssertion(AnnotationAssertion {
                        subject: AnnotationSubject::IRI(b.iri(iri.as_str())),
                        ann: ann_iri(b, &rel_iri, &expand_curie(target)),
                    }),
                    qualifier_anns(b, quals),
                );
            }
        }
    }
    // An object property's `equivalent_to:` and `disjoint_from:` relate it to
    // another object property.
    for line in st.all("equivalent_to") {
        let other = line.split_whitespace().next().unwrap_or(line);
        insert_annotated(
            ont,
            Component::EquivalentObjectProperties(horned_owl::model::EquivalentObjectProperties(vec![
                OPE::ObjectProperty(b.object_property(iri.clone())),
                OPE::ObjectProperty(b.object_property(resolve_rel(other, rel_map))),
            ])),
            qualifier_anns(b, line),
        );
    }
    for line in st.all("disjoint_from") {
        let other = line.split_whitespace().next().unwrap_or(line);
        insert_annotated(
            ont,
            Component::DisjointObjectProperties(horned_owl::model::DisjointObjectProperties(vec![
                OPE::ObjectProperty(b.object_property(iri.clone())),
                OPE::ObjectProperty(b.object_property(resolve_rel(other, rel_map))),
            ])),
            qualifier_anns(b, line),
        );
    }
    // A trailing `{qualifier=…}` block on `domain:`/`range:` is axiom annotations,
    // exactly as on `is_a:`/`relationship:` — splitting on whitespace and keeping
    // the first token would throw them away, losing RO's domain/range comments
    // (`{IAO:0000116="This is redundant with the more specific …"}`) on an obo→obo
    // trip. The writer emits them, so the reader has to keep them.
    for d in st.all("domain") {
        let (head, quals) = split_qualifier_block(d);
        let d = head.split_whitespace().next().unwrap_or(head);
        insert_annotated(
            ont,
            Component::ObjectPropertyDomain(ObjectPropertyDomain {
                ope: OPE::ObjectProperty(b.object_property(iri.clone())),
                ce: CE::Class(b.class(expand_curie(d))),
            }),
            qualifier_anns(b, quals),
        );
    }
    for r in st.all("range") {
        let (head, quals) = split_qualifier_block(r);
        let r = head.split_whitespace().next().unwrap_or(head);
        insert_annotated(
            ont,
            Component::ObjectPropertyRange(ObjectPropertyRange {
                ope: OPE::ObjectProperty(b.object_property(iri.clone())),
                ce: CE::Class(b.class(expand_curie(r))),
            }),
            qualifier_anns(b, quals),
        );
    }
    for inv in st.all("inverse_of") {
        let inv = inv.split_whitespace().next().unwrap_or(inv);
        ont.insert(Component::InverseObjectProperties(InverseObjectProperties(
            OPE::ObjectProperty(b.object_property(iri.clone())),
            OPE::ObjectProperty(b.object_property(resolve_rel(inv, rel_map))),
        )));
    }
    // `holds_over_chain: R1 R2` → `R1 ∘ R2 ⊑ this`, a SubObjectPropertyOf over a
    // property chain. `equivalent_to_chain` adds the same axiom (the reverse
    // direction is not OWL-expressible).
    for chain in st.all("holds_over_chain").chain(st.all("equivalent_to_chain")) {
        let links: Vec<OPE<RcStr>> = chain
            .split_whitespace()
            .take_while(|t| !t.starts_with('{') && !t.starts_with('!'))
            .map(|t| OPE::ObjectProperty(b.object_property(resolve_rel(t, rel_map))))
            .collect();
        if links.len() >= 2 {
            ont.insert(Component::SubObjectPropertyOf(SubObjectPropertyOf {
                sub: horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(links),
                sup: OPE::ObjectProperty(b.object_property(iri.clone())),
            }));
        }
    }
    // `transitive_over: R` → `this ∘ R ⊑ this`.
    for to in st.all("transitive_over") {
        let to = to.split_whitespace().next().unwrap_or(to);
        ont.insert(Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(vec![
                OPE::ObjectProperty(b.object_property(iri.clone())),
                OPE::ObjectProperty(b.object_property(resolve_rel(to, rel_map))),
            ]),
            sup: OPE::ObjectProperty(b.object_property(iri.clone())),
        }));
    }
}

/// An `[Instance]` frame is a named individual. `instance_of: C` is
/// `ClassAssertion(C x)` and `relationship: R y` is `ObjectPropertyAssertion(R x
/// y)` — an annotation assertion instead when `R` is a metadata tag, as in a
/// `[Term]` — each carrying its `{…}` qualifiers as axiom annotations. Every other
/// tag reads as it does in a `[Term]`.
fn instance_to_owl(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    st: &Stanza,
    default_ns: Option<&str>,
    onto_ns: Option<&str>,
    rel_map: &HashMap<String, String>,
    metadata_tags: &BTreeSet<String>,
) {
    let Some(id) = st.get("id") else { return };
    let iri = expand_curie(id);
    ont.insert(Component::DeclareNamedIndividual(DeclareNamedIndividual(
        b.named_individual(iri.as_str()),
    )));
    frame_annotations_to_owl(b, ont, st, &iri, id, default_ns, onto_ns, rel_map);
    custom_tags(b, ont, &iri, st, |tag| TERM_TAGS.contains(&tag) || tag == "instance_of");
    for class in st.all("instance_of") {
        let cid = class.split_whitespace().next().unwrap_or(class);
        insert_annotated(
            ont,
            Component::ClassAssertion(ClassAssertion {
                ce: CE::Class(b.class(expand_curie(cid))),
                i: Individual::Named(b.named_individual(iri.as_str())),
            }),
            qualifier_anns(b, class),
        );
    }
    for rel in st.all("relationship") {
        let mut parts = rel.split_whitespace();
        let (Some(r), Some(target)) = (parts.next(), parts.next()) else { continue };
        let rel_iri = resolve_rel(r, rel_map);
        let target_iri = expand_curie(target);
        let component = if metadata_tags.contains(&rel_iri) {
            Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(iri.as_str())),
                ann: ann_iri(b, &rel_iri, &target_iri),
            })
        } else {
            // The frame declares the individual it relates to, as a `[Term]`
            // declares the filler of its `relationship:`.
            ont.insert(Component::DeclareNamedIndividual(DeclareNamedIndividual(
                b.named_individual(target_iri.as_str()),
            )));
            Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
                ope: OPE::ObjectProperty(b.object_property(rel_iri)),
                from: Individual::Named(b.named_individual(iri.as_str())),
                to: Individual::Named(b.named_individual(target_iri)),
            })
        };
        insert_annotated(ont, component, qualifier_anns(b, rel));
    }
}

// === Writer ==============================================================

/// Write an ontology to OBO format. Renders the OBO-expressible fragment;
/// axioms outside it are skipped.
const OWL_DEPRECATED_W: &str = "http://www.w3.org/2002/07/owl#deprecated";
const IAO_REPLACED_BY_W: &str = "http://purl.obolibrary.org/obo/IAO_0100001";
const RDFS_SEEALSO: &str = "http://www.w3.org/2000/01/rdf-schema#seeAlso";
const OWL_NS: &str = "http://www.w3.org/2002/07/owl#";
const IAO_EXPAND_EXPRESSION_TO: &str = "http://purl.obolibrary.org/obo/IAO_0000424";
const IAO_EXPAND_ASSERTION_TO: &str = "http://purl.obolibrary.org/obo/IAO_0000425";

/// Writer-side rendering context: the CURIE prefixes usable in the output plus a
/// record of which ones the body actually referenced, so the header can list the
/// matching `idspace:` lines.
#[derive(Default)]
struct Ctx {
    /// (prefix, namespace), longest namespace first so the most specific wins.
    idspaces: Vec<(String, String)>,
    /// The ontology's `default-namespace` (oboInOwl:default-namespace), if any. A
    /// term/typedef whose own `namespace:` equals it is written WITHOUT the tag —
    /// `namespace:` is emitted only when it differs from the default.
    default_namespace: Option<String>,
    /// OBO relation shorthands (`oboInOwl:shorthand`) keyed by property IRI. OBO
    /// §5.9.3 "Special Rules for Relations": a property carrying one is written
    /// under that name everywhere, so RO_0000057 appears as `has_participant`
    /// throughout CL's released `cl.obo`, never as `RO:0000057`.
    shorthands: HashMap<String, String>,
    /// Property IRIs carrying `oboInOwl:is_metadata_tag`. An annotation assertion
    /// with an IRI value on a [Term] is a `relationship:` iff its property is one of
    /// these, else a `property_value:` — so a property with a shorthand but no
    /// `is_metadata_tag` (CL's `rdfs:seeAlso`, which carries a `seeAlso` shorthand)
    /// stays a `property_value:`.
    metadata_tags: std::collections::HashSet<String>,
    /// Per subject IRI, the subject's `rdfs:label` values in the order the source
    /// document carried them. Two labels can land in the same slot of the
    /// subject's assertion set, and then the one read first is the one the
    /// `! …` comments name.
    label_order: HashMap<String, Vec<String>>,
    used: std::cell::RefCell<BTreeSet<String>>,
    /// `Model::owlapi_456` — this document quotes a `property_value:` literal
    /// only when it must (see [`pv_literal_token`]).
    owlapi_456: bool,
    /// The natural order of the document's objects, in which the axioms the
    /// writer hashes store their sets.
    order: NaturalOrder,
}

/// The OBO identifier of an entity, as the writer spells it: a CURIE in the
/// document's own prefixes, or the full IRI.
pub struct IdCtx(Ctx);

impl IdCtx {
    pub fn new(model: &Model) -> IdCtx {
        IdCtx(Ctx::new(model))
    }

    pub fn id(&self, iri: &str) -> String {
        self.0.id(iri)
    }
}

impl Ctx {
    fn new(model: &Model) -> Ctx {
        // The prefixes an id is shortened with are the ones the source document
        // declared, and, for a cleaned write, the ones the command line adds
        // (`explicit_prefixes`). Anything else falls to `id_impl`'s mechanical
        // id rule: a bare local name, or the full IRI.
        //
        // An OWL source's declarations are its xmlns or `Prefix(…)` bindings
        // (`idspaces`, `rdf_prefixes`), however few: one declaring none beyond
        // the built-in ones abbreviates nothing. A namespace that appears only
        // through a default `xmlns="…"` (cl-full.owl's dc/terms/skos) is not a
        // declaration either. An OBO source's declarations are its `idspace:`
        // lines, which its reader records in `explicit_prefixes`.
        let scanned = !model.idspaces.is_empty() || !model.rdf_prefixes.is_empty();
        let mut idspaces: Vec<(String, String)> = Vec::new();
        let mut keep = |prefix: &str, ns: &str| {
            if !prefix.is_empty()
                && crate::io::idspace_namespace(ns)
                && !idspaces.iter().any(|(p, _)| p == prefix)
            {
                idspaces.push((prefix.to_string(), ns.to_string()));
            }
        };
        if scanned && model.explicit_prefixes.is_empty() {
            for (prefix, ns) in crate::io::declared_idspaces(model) {
                keep(&prefix, &ns);
            }
        } else if model.obo_source && !scanned {
            for (prefix, ns) in &model.explicit_prefixes {
                keep(prefix, ns);
            }
        } else {
            // A prefix the command line adds replaces the source's binding of
            // its name. The source's own follow, every prefix name kept rather
            // than one per namespace: its xmlns or `Prefix(…)` bindings when it
            // has them (carried through a pipeline by the OFN `#rdfxmlns`
            // comment), and otherwise the prefix map a pipeline built. A
            // namespace nothing bound has no prefix to shorten with.
            for (prefix, ns) in &model.explicit_prefixes {
                keep(prefix, ns);
            }
            if model.rdf_prefixes.is_empty() {
                for (prefix, ns) in model.prefixes.mappings() {
                    keep(prefix, ns);
                }
            } else {
                for (prefix, ns) in &model.rdf_prefixes {
                    keep(prefix, ns);
                }
            }
        }
        // Sort by namespace length, longest first (so an IRI matches the most
        // specific namespace — `gwas_trait:` before `efo:`). For two prefixes that
        // share ONE namespace (aliases, e.g. `ICD11` and `icd11.foundation` for
        // `http://id.who.int/icd/entity/`), the LONGEST prefix wins, ties broken by
        // the alphabetically-GREATEST (ASCII): `icd11.foundation` (16) beats `ICD11`
        // (5); `icd10cm` beats `ICD10CM` (both 7, lowercase 'i' > 'I'); `dcterms`
        // beats `terms`. So the tie-break is (prefix length desc, prefix string
        // desc). This only reorders same-namespace aliases; different namespaces of
        // equal length can't both prefix one IRI, so their relative order is
        // immaterial to `id_impl`.
        idspaces.sort_by(|a, b| {
            b.1.len()
                .cmp(&a.1.len())
                .then_with(|| b.0.len().cmp(&a.0.len()))
                .then_with(|| b.0.cmp(&a.0))
        });
        let mut shorthands = HashMap::new();
        let mut metadata_tags: std::collections::HashSet<String> = std::collections::HashSet::new();
        for ac in model.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if aa.ann.ap.0.as_ref() == format!("{OIO}shorthand") {
                    if let (AnnotationSubject::IRI(subj), AnnotationValue::Literal(lit)) =
                        (&aa.subject, &aa.ann.av)
                    {
                        let text = match lit {
                            Literal::Simple { literal } => literal,
                            Literal::Language { literal, .. } => literal,
                            Literal::Datatype { literal, .. } => literal,
                        };
                        shorthands.insert(subj.as_ref().to_string(), text.clone());
                    }
                } else if aa.ann.ap.0.as_ref() == format!("{OIO}is_metadata_tag") {
                    if let AnnotationSubject::IRI(subj) = &aa.subject {
                        metadata_tags.insert(subj.as_ref().to_string());
                    }
                }
            }
        }
        // The ontology-level `oboInOwl:default-namespace` annotation, if present.
        let default_namespace = model.ont.iter().find_map(|ac| {
            if let Component::OntologyAnnotation(oa) = &ac.component {
                if oa.0.ap.0.as_ref() == format!("{OIO}default-namespace") {
                    if let AnnotationValue::Literal(lit) = &oa.0.av {
                        return Some(match lit {
                            Literal::Simple { literal }
                            | Literal::Language { literal, .. }
                            | Literal::Datatype { literal, .. } => literal.clone(),
                        });
                    }
                }
            }
            None
        });
        Ctx {
            idspaces,
            default_namespace,
            shorthands,
            metadata_tags,
            label_order: model.owl_label_order.clone(),
            used: std::cell::RefCell::new(BTreeSet::new()),
            owlapi_456: model.owlapi_456,
            order: model.natural_order(),
        }
    }

    /// The OBO id for an IRI: a declared idspace prefix first, then OBO 1.4 table
    /// 5.9.2 (§"Translation of OWL IRIs to OBO IDs"). This is *not* the inverse of
    /// [`expand_id`] — it deliberately drops namespaces it cannot reconstruct
    /// (`obo/cl#cellxgene_subset` → `cellxgene_subset`), because that is what the
    /// committed CL `cl.obo` contains.
    fn id(&self, iri: &str) -> String {
        self.id_impl(iri, true)
    }
    /// Like `id` but never abbreviates to an `oboInOwl:shorthand`. The *value* of a
    /// `property_value:` is a plain CURIE (`BSPO:0000096`,
    /// `UBPROP:0000113`) even when the referenced entity has a shorthand
    /// (`anterior_to`, `dental_formula`); the predicate position still uses `id`.
    fn curie(&self, iri: &str) -> String {
        self.id_impl(iri, false)
    }
    /// The id a named individual is written under — its own `[Instance]` frame and
    /// every `relationship:` that names it: the OBO id when the reader expands that
    /// back to the same IRI, else the full IRI. Rows 2–4 of table 5.9.2 drop part
    /// of an IRI outside the obo PURL space (`…/resource/Northern_America` would
    /// be `Northern:America`), and an individual written that way comes back as a
    /// different one.
    fn individual_id(&self, iri: &str) -> String {
        let id = self.curie(iri);
        let expands_back = if id.starts_with("http://") || id.starts_with("https://") {
            id == iri
        } else {
            match id.split_once(':') {
                Some((prefix, local)) => match self.idspaces.iter().find(|(p, _)| p == prefix) {
                    Some((_, ns)) => format!("{ns}{local}") == iri,
                    None => expand_obo_id(&id) == iri,
                },
                None => expand_obo_id(&id) == iri,
            }
        };
        if expands_back {
            id
        } else {
            iri.to_string()
        }
    }
    /// The IRI shortened against a namespace the DOCUMENT declares, or `None` when
    /// none covers it. A declared binding is the only thing that may shorten a
    /// value: a prefix invented for the occasion would announce an `idspace:` for a
    /// namespace the document never named.
    fn declared_curie(&self, iri: &str) -> Option<String> {
        for (prefix, ns) in &self.idspaces {
            if let Some(local) = iri.strip_prefix(ns.as_str()) {
                if local.is_empty() {
                    continue;
                }
                self.used.borrow_mut().insert(prefix.clone());
                return Some(format!("{prefix}:{local}"));
            }
        }
        None
    }
    fn id_impl(&self, iri: &str, use_shorthand: bool) -> String {
        if use_shorthand {
            if let Some(sh) = self.shorthands.get(iri) {
                return sh.clone();
            }
        }
        for (prefix, ns) in &self.idspaces {
            if let Some(local) = iri.strip_prefix(ns.as_str()) {
                // An empty local part is allowed: an IRI that *is* a declared
                // namespace shortens to `prefix:` — EFO's annotation property
                // `http://www.ebi.ac.uk/efo/gwas_trait` is written `gwas_trait:`
                // (its own prefix), not `efo:gwas_trait`. idspaces are sorted
                // longest-namespace-first, so this exact match wins over `efo:`.
                self.used.borrow_mut().insert(prefix.clone());
                return format!("{prefix}:{local}");
            }
        }
        // The OWL namespace keeps its prefix even when undeclared (`owl:versionInfo`
        // in every released `.obo`), unlike rdf/rdfs which fall through to the
        // `#`-stripping rule below and render bare (`seeAlso`).
        if let Some(local) = iri.strip_prefix(OWL_NS) {
            if !local.is_empty() && !local.contains('/') {
                return format!("owl:{local}");
            }
        }
        let id = match iri.rfind('/') {
            Some(i) => &iri[i + 1..],
            None => iri,
        };
        // Row 2, NonCanonical-Prefixed-ID: `…/ubprop#_upper_level` → `ubprop:upper_level`.
        if let Some((pre, local)) = id.split_once("#_") {
            if !pre.is_empty() && !local.is_empty() && !local.contains('#') {
                return format!("{pre}:{local}");
            }
            return iri.to_string();
        }
        // Row 3, Unprefixed-ID: `…/obo/cl#cellxgene_subset` → `cellxgene_subset`.
        if let Some((pre, local)) = id.split_once('#') {
            if local.is_empty() || local.contains('#') {
                return iri.to_string();
            }
            // Only the *local* part is percent-decoded (the prefix keeps its
            // `%HH`): `…/Chinois_(R%C3%A9union)` → `Chinois:(Réunion)` but
            // `…/Gourmanch%C3%A9_language` → `Gourmanch%C3%A9:language`.
            return if pre == "_" {
                format!("_:{}", percent_decode(local))
            } else {
                percent_decode(local)
            };
        }
        // Row 4, Canonical-Prefixed-ID: the LAST `_` of the id (the part after the
        // final `/`) splits idspace from local id, and does so for *any* IRI,
        // not just obo-PURLs — `CL_0000540` → `CL:0000540`,
        // `EFO_0008992` → `EFO:0008992` (`http://www.ebi.ac.uk/efo/…`),
        // `DHBA_10333` → `DHBA:10333` (`https://purl.brain-bican.org/…`),
        // `Ontology_extensions` → `Ontology:extensions` (a GO-wiki link). No
        // `idspace:` line is emitted for these; the reader re-splits at the last `_`.
        if let Some((pre, local)) = canonical_prefixed_id(id) {
            return format!("{pre}:{}", percent_decode(local));
        }
        iri.to_string()
    }
}

/// OBO 1.4 §5.9.2 row 4, Canonical-Prefixed-ID: the LAST `_` of an id (the part
/// of an IRI after the final `/`) splits idspace from local id. A
/// single-underscore id always splits (`CL_0000540`, `FOO_baz`,
/// `Ontology_extensions`); a multi-underscore idspace splits only when the local
/// id is numeric (`NCBITaxon_Union_0000030` → `NCBITaxon_Union:0000030`, but
/// `valid_for_gocam` does not split). An id with no `_` (an ORCID, a DOI, a
/// GitHub issue URL) is not an OBO id.
pub(crate) fn canonical_prefixed_id(id: &str) -> Option<(&str, &str)> {
    let p = id.rfind('_')?;
    let (pre, local) = (&id[..p], &id[p + 1..]);
    let single = !pre.contains('_');
    let numeric_local = local.bytes().all(|b| b.is_ascii_digit());
    (!pre.is_empty() && !local.is_empty() && (single || numeric_local)).then_some((pre, local))
}

/// Case-insensitive sort key for the repeated clauses of a tag (`xref:`,
/// `synonym:`, `is_a:` …) — CL's `cl.obo` has `xref: ncithesaurus:…` between
/// `MA:…` and `VHOG:…`, which only a case-folding comparison produces.
///
/// The comparison upper-cases to decide but returns the lower-case difference, so
/// the effective key is `lowercase(uppercase(c))` per single UTF-16 unit.
///
/// Plain `str::to_lowercase` is nearly right, but it is the full Unicode mapping
/// and can EXPAND: `'İ'` (U+0130) becomes `i` + U+0307 COMBINING DOT ABOVE where a
/// char-to-char fold yields just `'i'`. That trailing U+0307 sorts after every
/// ASCII letter, which would put HPO's Turkish `İdrar yolu…` after `Infekce…`
/// instead of before `Infections…` — 3241 `synonym:` lines of
/// `hp-international.obo`.
fn fold(s: &str) -> String {
    /// A length-preserving single-character case map: a multi-char Unicode
    /// expansion is declined (`'ß'.to_uppercase()` is `"SS"`, so `'ß'` is left
    /// as it is).
    fn single(c: char, mut it: impl Iterator<Item = char>) -> char {
        let first = it.next().unwrap_or(c);
        if it.next().is_some() {
            c
        } else {
            first
        }
    }
    s.chars()
        .map(|c| {
            let upper = single(c, c.to_uppercase());
            // The one mapping where declining the expansion is wrong: the
            // single-character lower case of U+0130 is defined as U+0069.
            if upper == '\u{130}' {
                return 'i';
            }
            single(upper, upper.to_lowercase())
        })
        .collect()
}

/// The synonym scope an `oboInOwl:hasScope` value names.
fn synonym_scope(av: &AnnotationValue<RcStr>) -> Option<&'static str> {
    let AnnotationValue::IRI(i) = av else { return None };
    Some(match i.as_ref().strip_prefix(OIO)? {
        "hasExactSynonym" => "EXACT",
        "hasRelatedSynonym" => "RELATED",
        "hasNarrowSynonym" => "NARROW",
        "hasBroadSynonym" => "BROAD",
        _ => return None,
    })
}

/// The OBO tag an `oboInOwl:` boolean annotation property stands for.
fn boolean_tag(local: &str) -> Option<&'static str> {
    Some(match local {
        "builtin" => "builtin",
        "is_anonymous" => "is_anonymous",
        "is_cyclic" => "is_cyclic",
        "is_reflexive" => "is_reflexive",
        "is_symmetric" => "is_symmetric",
        "is_functional" => "is_functional",
        "is_inverse_functional" => "is_inverse_functional",
        "is_asymmetric" => "is_asymmetric",
        _ => return None,
    })
}

/// A conjunct a `relationship:` clause stands for besides its own: its axiom
/// annotations and its extra qualifiers.
type AbsorbedConjunct = (BTreeSet<Annotation<RcStr>>, Vec<(String, String)>);

/// All the OBO-renderable facts about one term/typedef subject, grouped so the
/// stanza can be emitted faithfully (axiom-annotation `[xref]`/`TYPE`/`{qual}`
/// blocks included) and re-read to the same axioms.
#[derive(Default)]
struct SubjData {
    id: Option<String>,
    name: Option<(String, BTreeSet<Annotation<RcStr>>)>,
    // Additional `rdfs:label` values beyond the primary `name`, each with its own
    // axiom annotations. An entity may carry several labels (OBI:0000295 is both
    // "is_input_of" and "is specified input of"); one `name:` line is written per
    // label, sorted, carrying any `{key="…"}` qualifiers (GSSO's translated
    // labels have a `{terms:isReferencedBy="…"}` source).
    extra_names: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// `rdfs:label` values asserted WITHOUT a language tag (a plain/`xsd:string`
    /// literal). An entity's display name — used in the `! <label>` end-of-line
    /// comments — comes from the language-neutral label when one exists, in
    /// preference to any `@en` (etc.) label. RO:0002211 carries both
    /// `"regulates (processual)"` (no tag) and `"regulates"@en`, and the
    /// `! regulates (processual) …` comment comes from the untagged one.
    label_no_lang: BTreeSet<String>,
    /// Every `rdfs:label` axiom on the subject: (value, language tag, axiom
    /// annotations). The `!`-comment name is the label whose axiom lands in the
    /// minimum hash bucket (see [`pick_comment_name`]).
    label_axioms: Vec<(String, Option<String>, BTreeSet<Annotation<RcStr>>)>,
    /// Count of ALL annotation-assertion axioms on the subject — it sizes the hash
    /// table whose bucket order picks the `!`-comment label.
    ann_count: usize,
    namespace: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    def: Option<(String, BTreeSet<Annotation<RcStr>>)>,
    // Additional `IAO:0000115` definitions beyond the first. A term may carry more
    // than one (RO's `overlaps` has two); one `def:` line is written for each,
    // sorted, identical ones collapsed.
    extra_defs: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    comments: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// (scope, text, language tag, axiom annotations).
    ///
    /// The LANGUAGE TAG is part of the axiom hash, and that hash is what buckets
    /// synonym clauses whose values tie. Dropping it hashes every translated
    /// synonym as if untagged, which reorders 5,956 lines of
    /// `hp-international.obo`.
    synonyms: Vec<(String, String, Option<String>, BTreeSet<Annotation<RcStr>>)>,
    xrefs: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// (rendered subset name, RAW annotation value, value-is-IRI, axiom annotations).
    /// The raw value and flag rebuild the assertion's hash, which is what breaks
    /// ties between two `subset:` clauses naming the same subset.
    subsets: Vec<(String, String, bool, BTreeSet<Annotation<RcStr>>)>,
    alt_ids: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    replaced_by: Vec<String>,
    consider: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    created_by: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    creation_date: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    disjoint_over: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// The values of annotations whose property's tag relates the subject to
    /// another entity (`domain`, `inverse_of`, `is_a`, … — see [`RELATION_TAGS`]),
    /// each with its tag and the assertion's axiom annotations.
    tag_values: Vec<(String, String, BTreeSet<Annotation<RcStr>>)>,
    deprecated: bool,
    // Axiom annotations on the `owl:deprecated true` assertion — a `{source=…}`
    // qualifier on the `is_obsolete: true` line.
    deprecated_anns: BTreeSet<Annotation<RcStr>>,
    /// `IAO:0000231` obsolescence reason. `IAO:0000227` ("terms merged") marks the
    /// stub that a primary term's `alt_id:` expands to; see [`fold_alt_ids`].
    obsolescence_reason: Option<String>,
    /// OBO macro expansions (`IAO:0000424`/`IAO:0000425`) — Typedef-only tags,
    /// written as `expand_expression_to: "…" []`, not `property_value:`.
    expand_expression_to: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    expand_assertion_to: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    shorthand: Option<String>,
    is_metadata_tag: bool,
    is_class_level: bool,
    // (predicate-obo, printed value, is_iri, datatype-curie, anns, predicate IRI,
    // value IRI). Both IRIs are kept because the axiom hash that orders clauses
    // tying on predicate AND value is over FULL IRIs, not the CURIEs the clause
    // prints — two values that share a CURIE prefix hash nothing alike.
    property_values: Vec<(
        String,
        String,
        bool,
        Option<String>,
        BTreeSet<Annotation<RcStr>>,
        String,
        String,
    )>,
    // (parent, anns, extra `{gci_*}` qualifiers from a General Class Inclusion,
    // source SubClassOf axiom's hash, which breaks ties between equal clauses)
    is_a: Vec<(String, BTreeSet<Annotation<RcStr>>, Vec<(String, String)>, i32)>,
    // (rel, target, anns, extra `{gci_*}` qualifiers, source axiom hash)
    relationships: Vec<(String, String, BTreeSet<Annotation<RcStr>>, Vec<(String, String)>, i32)>,
    /// For a `relationship:` clause that stands for several conjuncts of one
    /// superclass over its relation and filler, keyed by (rel, target, source
    /// axiom hash): the other conjuncts' axiom annotations and extra qualifiers,
    /// whose qualifiers follow the clause's own (see [`absorb_quals`]).
    absorbed: HashMap<(String, String, i32), Vec<AbsorbedConjunct>>,
    /// An individual's `instance_of:` clauses: each named class it is asserted to
    /// belong to, with that ClassAssertion's axiom annotations.
    instance_of: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// An individual's object property assertions, each a `relationship:` clause
    /// of its [Instance] frame: (relation, target, axiom annotations).
    assertions: Vec<(String, String, BTreeSet<Annotation<RcStr>>)>,
    // Shorthand-property annotations with an IRI value whose routing
    // (`relationship:` in a [Term] vs `property_value:` in a [Typedef]) depends on
    // the subject's stanza type, which is only known at write time. (predicate-obo,
    // value, anns)
    rel_or_pv: Vec<(String, String, BTreeSet<Annotation<RcStr>>, String, String)>,
    // each line's tokens, plus any clause qualifiers (a cardinality bound)
    intersection_of: Vec<(Vec<String>, Vec<(String, String)>, BTreeSet<Annotation<RcStr>>)>,
    union_of: Vec<String>,
    equivalent_to: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    disjoint_from: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    // Typedef-only property axioms.
    domain: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    range: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    inverse_of: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// The characteristic axioms on a named property, by the tag each is written
    /// as (`is_transitive`, `is_functional`, …): one annotation set per axiom,
    /// each axiom its own clause carrying its annotations as qualifiers.
    characteristics: BTreeMap<&'static str, Vec<BTreeSet<Annotation<RcStr>>>>,
    // `oboInOwl:is_transitive` on a Typedef: its value is the
    // `is_transitive:` tag (true *or* false), not a `property_value:`. The axiom
    // (`TransitiveObjectProperty`) covers the true case; this carries an explicit
    // `false` (EFO marks several relations non-transitive) that would otherwise leak.
    transitive_anno: Option<bool>,
    // The other boolean tags an annotation states (`builtin`, `is_anonymous`,
    // `is_anti_symmetric`, `is_cyclic`, a characteristic stated as an annotation,
    // and `is_metadata_tag`/`is_class_level` stated false), by tag: each is
    // written as the tag itself, not as a `property_value:`.
    bool_tags: BTreeMap<&'static str, bool>,
    chains: Vec<(Vec<String>, BTreeSet<Annotation<RcStr>>)>, // holds_over_chain (links, axiom anns)
    sub_property_of: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    // An annotation property declared via a header `subsetdef:`/`synonymtypedef:`
    // (sub-property of oboInOwl:SubsetProperty / SynonymTypeProperty).
    subset_property: bool,
    synonymtype_property: bool,
    /// The axiom annotations of the sub-property axiom that makes this a subset
    /// or a synonym type: the qualifiers of its header line.
    header_def_anns: BTreeSet<Annotation<RcStr>>,
    // A synonym type's `oboInOwl:hasScope`: the scope (`EXACT`, …) of the
    // synonym property it names.
    synonym_scope: Option<&'static str>,
    /// The class has a `[Term]` frame whatever clauses it ends up with: an
    /// axiom whose translation starts on the class's frame names it — a
    /// subclass axiom with the class as subclass, an equivalence of two members
    /// with the class as its named member — even when that axiom goes to the
    /// header's `owl-axioms` and the frame stays empty.
    framed: bool,
}

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

fn ce_is_top_or_bottom(ce: &CE<RcStr>) -> bool {
    matches!(ce, CE::Class(c) if c.0.as_ref() == OWL_THING || c.0.as_ref() == OWL_NOTHING)
}

fn ce_named_class(ce: &CE<RcStr>) -> bool {
    matches!(ce, CE::Class(_))
}

/// A filler has no OBO spelling unless it is a named class.
fn filler_bad(bce: &CE<RcStr>) -> bool {
    ce_is_top_or_bottom(bce) || !ce_named_class(bce)
}

/// Which `SubClassOf` axioms have no OBO clause and go to the `owl-axioms:` bag.
fn subclassof_untranslatable(sub: &CE<RcStr>, sup: &CE<RcStr>) -> bool {
    if ce_is_top_or_bottom(sub) || ce_is_top_or_bottom(sup) {
        return true;
    }
    // GCI reduction: subject must be a named class or a 2-part `C ⊓ ∃R.F`.
    let sub_is_class = match sub {
        CE::Class(_) => true,
        CE::ObjectIntersectionOf(xs) if xs.len() == 2 => {
            let has_c = xs.iter().any(ce_named_class);
            let has_restr = xs.iter().any(|x| matches!(x,
                CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(_), bce }
                    if ce_named_class(bce)));
            has_c && has_restr
        }
        _ => false,
    };
    if !sub_is_class {
        return true;
    }
    match sup {
        CE::Class(_) => false,
        CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => filler_bad(bce),
        CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. }
        | CE::ObjectMaxCardinality { bce, .. } => filler_bad(bce),
        CE::ObjectIntersectionOf(ops) => {
            if ops.is_empty() {
                return true;
            }
            ops.iter().any(|op| match op {
                CE::ObjectSomeValuesFrom { bce, .. }
                | CE::ObjectAllValuesFrom { bce, .. }
                | CE::ObjectMinCardinality { bce, .. }
                | CE::ObjectExactCardinality { bce, .. }
                | CE::ObjectMaxCardinality { bce, .. } => filler_bad(bce),
                _ => true,
            })
        }
        _ => true,
    }
}

fn ec_operand_bad(op: &CE<RcStr>) -> bool {
    match op {
        CE::Class(_) => false,
        CE::ObjectSomeValuesFrom { bce, .. }
        | CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. }
        | CE::ObjectMaxCardinality { bce, .. } => !ce_named_class(bce),
        CE::ObjectAllValuesFrom { bce, .. } => match bce.as_ref() {
            CE::Class(_) => false,
            CE::ObjectComplementOf(inner) => !ce_named_class(inner),
            _ => true,
        },
        // A nested `ObjectIntersectionOf` (all-some / min-max combination) resolves
        // as long as one operand is a restriction with a named filler — that
        // operand is kept and the rest dropped.
        CE::ObjectIntersectionOf(inner) if inner.len() == 2 => !inner.iter().any(|o| match o {
            CE::ObjectMinCardinality { bce, .. }
            | CE::ObjectMaxCardinality { bce, .. }
            | CE::ObjectAllValuesFrom { bce, .. }
            | CE::ObjectSomeValuesFrom { bce, .. } => ce_named_class(bce),
            _ => false,
        }),
        _ => true,
    }
}

fn ec_untranslatable(ops: &[CE<RcStr>]) -> bool {
    if ops.len() != 2 {
        return true;
    }
    if ops.iter().any(ce_is_top_or_bottom) {
        return true;
    }
    let (ce1_named, ce2) = if ce_named_class(&ops[0]) {
        (true, &ops[1])
    } else if ce_named_class(&ops[1]) {
        (true, &ops[0])
    } else {
        (false, &ops[1])
    };
    if !ce1_named {
        return true;
    }
    match ce2 {
        CE::Class(_) => false,
        CE::ObjectUnionOf(list) => list.iter().any(|o| !ce_named_class(o)),
        CE::ObjectIntersectionOf(list) => list.iter().any(ec_operand_bad),
        // `NamedClass ≡ ObjectOneOf(individuals)` (an enumeration — IAO_0000078 ≡
        // {IAO_0000002 … IAO_0000428}) has no OBO spelling, so it is parked in
        // `owl-axioms:` together with the class's Declaration and one for every
        // NamedIndividual in the enumeration. It is what MONDO's
        // `mondo-international.obo` bag holds for IAO_0000078 / IAO_0000225 /
        // IAO_0000409.
        CE::ObjectOneOf(_) => true,
        _ => true,
    }
}

fn dc_untranslatable(ops: &[CE<RcStr>]) -> bool {
    ops.len() != 2 || ops.iter().any(ce_is_top_or_bottom) || !ops.iter().all(ce_named_class)
}

/// Whether `--clean-obo drop-untranslatable-axioms` should DROP this
/// `DisjointClasses`, which is not the same question as whether it belongs in the
/// `owl-axioms:` bag.
///
/// An n-ary one is PARTIALLY translatable: one `disjoint_from:` is written for its
/// first two members in IRI order and the other pairs are lost, so it lands
/// in the bag (MONDO's `mondo-international.obo` bags a four-member FOODON
/// disjointness) AND still contributes its clause (MONDO's `mondo.obo` and OBA's
/// `oba.obo` both carry it). Dropping it would take the clause with it.
fn dc_droppable(ops: &[CE<RcStr>]) -> bool {
    ops.len() < 2 || ops.iter().any(ce_is_top_or_bottom) || !ops.iter().all(ce_named_class)
}

/// The first and last members, in canonical order, of an equivalence or
/// disjointness of object properties that has two or more members, every one a
/// named property; `None` otherwise, when the axiom has no OBO clause.
fn property_ends(members: &[OPE<RcStr>]) -> Option<(String, String)> {
    if members.len() < 2 {
        return None;
    }
    let mut named: Vec<&str> = Vec::new();
    for m in members {
        match m {
            OPE::ObjectProperty(p) if !is_top_or_bottom_property(p.0.as_ref()) => {
                named.push(p.0.as_ref())
            }
            _ => return None,
        }
    }
    named.sort_unstable_by(|a, b| crate::owlapi_hash::iri_cmp(a, b));
    Some((named[0].to_string(), named[named.len() - 1].to_string()))
}

fn opr_untranslatable(ope: &OPE<RcStr>, ce: &CE<RcStr>) -> bool {
    matches!(ope, OPE::InverseObjectProperty(_)) || ce_is_top_or_bottom(ce) || !ce_named_class(ce)
}

/// A subclass axiom with a clause of its own whose superclass, or a conjunct of
/// it, restricts an inverse property: its `relationship:` would have no
/// relation to name, so no OBO document can state it.
fn relates_through_an_inverse(c: &Component<RcStr>) -> bool {
    let Component::SubClassOf(sc) = c else { return false };
    if subclassof_untranslatable(&sc.sub, &sc.sup) {
        return false;
    }
    let inverse = |ce: &CE<RcStr>| {
        matches!(
            ce,
            CE::ObjectSomeValuesFrom { ope: OPE::InverseObjectProperty(_), .. }
                | CE::ObjectAllValuesFrom { ope: OPE::InverseObjectProperty(_), .. }
                | CE::ObjectMinCardinality { ope: OPE::InverseObjectProperty(_), .. }
                | CE::ObjectExactCardinality { ope: OPE::InverseObjectProperty(_), .. }
                | CE::ObjectMaxCardinality { ope: OPE::InverseObjectProperty(_), .. }
        )
    };
    match &sc.sup {
        CE::ObjectIntersectionOf(ops) => ops.iter().any(inverse),
        sup => inverse(sup),
    }
}

fn sop_untranslatable(
    ax: &horned_owl::model::SubObjectPropertyOf<RcStr>,
) -> bool {
    use horned_owl::model::SubObjectPropertyExpression as SOPE;
    let top_or_bottom =
        |o: &OPE<RcStr>| matches!(o, OPE::ObjectProperty(p) if is_top_or_bottom_property(p.0.as_ref()));
    match &ax.sub {
        // A chain becomes `holds_over_chain`/`transitive_over` only when it is
        // exactly two named properties, neither top nor bottom, under a named
        // super-property that is neither; anything else (an inverse element, 3+
        // links) is untranslatable.
        SOPE::ObjectPropertyChain(v) => {
            v.len() != 2
                || v.iter().any(|o| matches!(o, OPE::InverseObjectProperty(_)) || top_or_bottom(o))
                || matches!(ax.sup, OPE::InverseObjectProperty(_))
                || top_or_bottom(&ax.sup)
        }
        // A side that is top or bottom is written nowhere, not even here.
        SOPE::ObjectPropertyExpression(sub) if top_or_bottom(sub) || top_or_bottom(&ax.sup) => false,
        SOPE::ObjectPropertyExpression(OPE::ObjectProperty(_)) => {
            matches!(ax.sup, OPE::InverseObjectProperty(_))
        }
        SOPE::ObjectPropertyExpression(OPE::InverseObjectProperty(_)) => true,
    }
}

/// Axioms that OBO has no tag for. Collected for the `owl-axioms:` header value.
/// The IRIs of a subject's annotation assertions that are "unrelated" to its
/// OBO alt-id role — those become untranslatable. Returns a set of
/// `(subject, property, value-string)` keys identifying them.
fn alt_id_unrelated(model: &Model) -> HashSet<(String, String, String)> {
    use horned_owl::model::AnnotationSubject as AS;
    // Group annotation assertions by subject IRI.
    let mut by_subject: HashMap<String, Vec<&Annotation<RcStr>>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let AS::IRI(s) = &aa.subject {
                by_subject.entry(s.as_ref().to_string()).or_default().push(&aa.ann);
            }
        }
    }
    let av_is_iri = |av: &AnnotationValue<RcStr>| matches!(av, AnnotationValue::IRI(_));
    let av_is_literal = |av: &AnnotationValue<RcStr>| matches!(av, AnnotationValue::Literal(_));
    let av_key = |av: &AnnotationValue<RcStr>| match av {
        AnnotationValue::Literal(l) => l.literal().clone(),
        AnnotationValue::IRI(i) => i.as_ref().to_string(),
        AnnotationValue::AnonymousIndividual(a) => a.0.as_ref().to_string(),
    };
    let mut out = HashSet::new();
    for (subj, anns) in &by_subject {
        let mut is_deprecated = false;
        let mut is_merged = false;
        let mut replaced_by = false;
        for a in anns {
            let p = a.ap.0.as_ref();
            if p == OWL_DEPRECATED {
                is_deprecated = true;
            } else if p == IAO_OBSOLESCENCE_REASON {
                if let AnnotationValue::IRI(i) = &a.av {
                    if i.as_ref() == IAO_TERMS_MERGED {
                        is_merged = true;
                    }
                }
            } else if p == IAO_TERM_REPLACED_BY {
                if av_is_literal(&a.av) || av_is_iri(&a.av) {
                    replaced_by = true;
                }
            }
        }
        if !(replaced_by && is_merged && is_deprecated) {
            continue;
        }
        for a in anns {
            let p = a.ap.0.as_ref();
            let unrelated = if p == OWL_DEPRECATED {
                false
            } else if p == IAO_OBSOLESCENCE_REASON {
                !av_is_iri(&a.av)
            } else if p == IAO_TERM_REPLACED_BY {
                !(av_is_literal(&a.av) || av_is_iri(&a.av))
            } else {
                true
            };
            if unrelated {
                out.insert((subj.clone(), p.to_string(), av_key(&a.av)));
            }
        }
    }
    out
}

/// A clause of a tag whose value is empty, or white space alone, cannot be
/// written as OBO, so the assertion is parked in the `owl-axioms:` bag instead.
/// EFO's IAO_0000115 EFO_0010180 "", hasExactSynonym OBI_0000512 "", and
/// rdfs:comment EFO_0007034 "" are three. A property with no tag is a
/// `property_value:` whatever its value, an empty `IAO_0000117` among them.
fn empty_scalar_clause(aa: &horned_owl::model::AnnotationAssertion<RcStr>) -> bool {
    let blank = matches!(
        &aa.ann.av,
        AnnotationValue::Literal(l) if l.literal().trim().is_empty() && !is_boolean_literal(l)
    );
    blank && annotation_tag(aa.ann.ap.0.as_ref()).is_some()
}

/// An `xsd:boolean` literal, whose clause value is `true` or `false` whatever
/// its lexical form.
fn is_boolean_literal(l: &Literal<RcStr>) -> bool {
    matches!(l, Literal::Datatype { datatype_iri, .. }
        if datatype_iri.as_ref() == "http://www.w3.org/2001/XMLSchema#boolean")
}

/// Every tag the OBO format defines.
const OBO_TAGS: &[&str] = &[
    "format-version", "ontology", "data-version", "date", "saved-by", "auto-generated-by",
    "import", "subsetdef", "synonymtypedef", "default-namespace", "idspace",
    "treat-xrefs-as-equivalent", "treat-xrefs-as-reverse-genus-differentia",
    "treat-xrefs-as-genus-differentia", "treat-xrefs-as-relationship", "treat-xrefs-as-is_a",
    "treat-xrefs-as-has-subclass", "owl-axioms", "remark", "id", "name", "namespace", "alt_id",
    "def", "comment", "subset", "synonym", "xref", "builtin", "property_value", "is_a",
    "intersection_of", "union_of", "equivalent_to", "disjoint_from", "relationship",
    "created_by", "creation_date", "is_obsolete", "replaced_by", "is_anonymous", "domain",
    "range", "is_anti_symmetric", "is_cyclic", "is_reflexive", "is_symmetric", "is_transitive",
    "is_functional", "is_inverse_functional", "transitive_over", "holds_over_chain",
    "equivalent_to_chain", "disjoint_over", "expand_assertion_to", "expand_expression_to",
    "is_class_level", "is_metadata_tag", "consider", "inverse_of", "is_asymmetric",
    "namespace-id-rule", "logical-definition-view-relation", "scope", "has_synonym_type",
    "BROAD", "NARROW", "EXACT", "RELATED",
];

/// The tag a frame clause spells an annotation of `prop` as: its tag by
/// [`annotation_tag`], else its own OBO id when that is a tag (`obo/q#comment`
/// in ontology `q` is `comment`), when that tag is one of [`FRAME_TAGS`] or
/// [`RELATION_TAGS`].
fn frame_tag(ctx: &Ctx, prop: &str) -> Option<&'static str> {
    let tag = match annotation_tag(prop) {
        Some(tag) => tag,
        None if prop.starts_with(OIO) => return None,
        None => {
            let id = ctx.id(prop);
            OBO_TAGS.iter().find(|t| **t == id).copied()?
        }
    };
    (FRAME_TAGS.contains(&tag) || RELATION_TAGS.contains(&tag)).then_some(tag)
}

/// The tags whose frame clauses the writer makes from annotation assertions of
/// their properties.
const FRAME_TAGS: &[&str] = &[
    "name", "def", "comment", "is_obsolete", "replaced_by", "consider", "namespace", "alt_id",
    "subset", "xref", "created_by", "creation_date", "disjoint_over", "is_metadata_tag",
    "is_class_level", "is_transitive", "is_anti_symmetric", "builtin", "is_anonymous", "is_cyclic",
    "is_reflexive", "is_symmetric", "is_functional", "is_inverse_functional", "is_asymmetric",
    "expand_assertion_to", "expand_expression_to", "EXACT", "NARROW", "BROAD", "RELATED",
];

/// The tags that relate a frame's subject to another entity. An annotation of a
/// property with one of these tags is a clause of that tag whose value is the
/// annotation's (an IRI's OBO id).
const RELATION_TAGS: &[&str] = &[
    "is_a", "intersection_of", "union_of", "equivalent_to", "disjoint_from", "relationship", "domain",
    "range", "inverse_of", "transitive_over", "holds_over_chain", "equivalent_to_chain",
];

/// The OBO tag an annotation property is written as, when it has one: its entry
/// in the tag map, else the local name of an `oboInOwl:` property that names a
/// tag. A property with no tag is written as a `property_value:`.
fn annotation_tag(prop: &str) -> Option<&'static str> {
    if let Some(tag) = qualifier_key_tag(prop) {
        return Some(tag);
    }
    let local = match prop.strip_prefix(OBO_BASE) {
        Some("IAO_xref") => return Some("xref"),
        Some("IAO_id") => return Some("id"),
        Some("IAO_namespace") => return Some("namespace"),
        _ => prop.strip_prefix(OIO)?,
    };
    OBO_TAGS.iter().find(|t| **t == local).copied()
}

/// An assertion of a property with no tag whose value is a literal of a datatype
/// outside OWL 2's datatype map: it has no `property_value:` spelling.
fn unspellable_property_value(ann: &Annotation<RcStr>) -> bool {
    let p = ann.ap.0.as_ref();
    annotation_tag(p).is_none() && p != format!("{OIO}shorthand") && unspellable_datatype(&ann.av)
}

/// The subjects whose annotation assertions become clauses: each class and
/// object property the document declares, other than the top and bottom ones;
/// each declared annotation property with an `oboInOwl:is_metadata_tag`
/// assertion; and, where the writer spells [Instance] frames, each named
/// individual. Another subject's assertions are written nowhere — neither as a
/// clause nor in `owl-axioms` — and do not start a frame.
fn translated_subjects(model: &Model) -> HashSet<String> {
    let metadata_tag = format!("{OIO}is_metadata_tag");
    let tagged: HashSet<&str> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::AnnotationAssertion(aa) if aa.ann.ap.0.as_ref() == metadata_tag => {
                match &aa.subject {
                    AnnotationSubject::IRI(s) => Some(s.as_ref()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    let instances = instance_frames();
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        let subject = match &ac.component {
            Component::DeclareClass(c)
                if c.0 .0.as_ref() != OWL_THING && c.0 .0.as_ref() != OWL_NOTHING =>
            {
                Some(c.0 .0.as_ref())
            }
            Component::DeclareObjectProperty(p) if !is_top_or_bottom_property(p.0 .0.as_ref()) => {
                Some(p.0 .0.as_ref())
            }
            Component::DeclareAnnotationProperty(p) if tagged.contains(p.0 .0.as_ref()) => {
                Some(p.0 .0.as_ref())
            }
            Component::DeclareNamedIndividual(i) if instances => Some(i.0 .0.as_ref()),
            Component::ClassAssertion(ca) if instances => match &ca.i {
                Individual::Named(i) => Some(i.0.as_ref()),
                _ => None,
            },
            Component::ObjectPropertyAssertion(opa) if instances => match &opa.from {
                Individual::Named(i) => Some(i.0.as_ref()),
                _ => None,
            },
            _ => None,
        };
        if let Some(s) = subject {
            out.insert(s.to_string());
        }
    }
    out
}

/// The annotation properties whose own assertions describe a header line: the
/// declared ones, and every sub-property of another (a `subsetdef:` takes its
/// description from its property's comment, a `synonymtypedef:` from its
/// label). Their assertions are recorded whether or not they are translated.
fn header_described(model: &Model) -> HashSet<String> {
    model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::DeclareAnnotationProperty(p) => Some(p.0 .0.as_ref().to_string()),
            Component::SubAnnotationPropertyOf(sp) => Some(sp.sub.0.as_ref().to_string()),
            _ => None,
        })
        .collect()
}

/// A literal annotation value whose datatype is outside OWL 2's datatype map —
/// `xsd:date`, a datatype of the document's own — has no `property_value:`
/// spelling.
fn unspellable_datatype(av: &AnnotationValue<RcStr>) -> bool {
    match av {
        AnnotationValue::Literal(Literal::Datatype { datatype_iri, .. }) => {
            !crate::io::entities::is_builtin(crate::io::entities::Kind::Datatype, datatype_iri.as_ref())
        }
        _ => false,
    }
}

fn collect_untranslatable(
    model: &Model,
) -> Vec<&horned_owl::model::AnnotatedComponent<RcStr>> {
    collect_untranslatable_opt(model, false)
}

/// As [`collect_untranslatable`]; `for_drop` asks the narrower question of what
/// `--clean-obo drop-untranslatable-axioms` may REMOVE, which excludes the
/// partially-translatable n-ary `DisjointClasses` (see [`dc_droppable`]).
fn collect_untranslatable_opt(
    model: &Model,
    for_drop: bool,
) -> Vec<&horned_owl::model::AnnotatedComponent<RcStr>> {
    use horned_owl::model::AnnotationSubject as AS;
    let alt_unrelated = alt_id_unrelated(model);
    // Under a declared `logical-definition-view-relation`, every EquivalentClasses
    // axiom is rewritten before anything is decided, so translatability must be
    // judged on the REWRITTEN axiom. Judging the original calls
    // `HP:0000002 ≡ has_part some (…)` untranslatable and lets
    // `--clean-obo drop-untranslatable-axioms` delete it before the writer can
    // unwrap it — every one of `hp-base.obo`'s 12,806 `intersection_of:` lines.
    let view_rel = declares_view_relation(model);
    // A subset's comment is its `subsetdef:` line's description, which the header
    // writes even when it is empty; it is never a frame's `comment:` clause.
    let subset_property = format!("{OIO}SubsetProperty");
    let subsets: HashSet<&str> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::SubAnnotationPropertyOf(ax) if ax.sup.0.as_ref() == subset_property => {
                Some(ax.sub.0.as_ref())
            }
            _ => None,
        })
        .collect();
    let instances = instance_frames();
    let translated = translated_subjects(model);
    let mut out = Vec::new();
    for ac in model.ont.iter() {
        let unt = match &ac.component {
            Component::AnnotationAssertion(aa) => match &aa.subject {
                AS::IRI(s) if !translated.contains(s.as_ref()) => false,
                AS::IRI(s) if aa.ann.ap.0.as_ref() == RDFS_COMMENT && subsets.contains(s.as_ref()) => {
                    false
                }
                AS::IRI(s) => {
                    let vk = match &aa.ann.av {
                        AnnotationValue::Literal(l) => l.literal().clone(),
                        AnnotationValue::IRI(i) => i.as_ref().to_string(),
                        AnnotationValue::AnonymousIndividual(a) => a.0.as_ref().to_string(),
                    };
                    alt_unrelated.contains(&(
                        s.as_ref().to_string(),
                        aa.ann.ap.0.as_ref().to_string(),
                        vk,
                    )) || empty_scalar_clause(aa)
                        || unspellable_property_value(&aa.ann)
                }
                _ => false,
            },
            // An individual's [Instance] frame spells a class assertion as
            // `instance_of:` and an object property assertion as `relationship:`
            // when every entity in it is named. Anything else — a class
            // expression, an anonymous individual, an inverse property — has no
            // OBO spelling. With no [Instance] frames, a class assertion is dropped
            // and an object property assertion has no spelling at all.
            Component::ClassAssertion(ax) => {
                instances && !matches!((&ax.ce, &ax.i), (CE::Class(_), Individual::Named(_)))
            }
            Component::ObjectPropertyAssertion(ax) => {
                !instances
                    || !matches!(
                        (&ax.ope, &ax.from, &ax.to),
                        (OPE::ObjectProperty(_), Individual::Named(_), Individual::Named(_))
                    )
            }
            Component::DisjointUnion(_)
            | Component::IrreflexiveObjectProperty(_)
            | Component::Rule(_)
            | Component::DataPropertyAssertion(_)
            | Component::HasKey(_)
            | Component::SameIndividual(_)
            | Component::DifferentIndividuals(_)
            | Component::NegativeObjectPropertyAssertion(_)
            | Component::NegativeDataPropertyAssertion(_)
            | Component::SubDataPropertyOf(_)
            | Component::DataPropertyDomain(_)
            | Component::DataPropertyRange(_)
            | Component::FunctionalDataProperty(_)
            | Component::EquivalentDataProperties(_)
            | Component::DisjointDataProperties(_)
            | Component::DatatypeDefinition(_)
            | Component::AnnotationPropertyDomain(_)
            | Component::AnnotationPropertyRange(_) => true,
            Component::SubClassOf(ax) => subclassof_untranslatable(&ax.sub, &ax.sup),
            Component::EquivalentClasses(ax) => {
                if view_rel {
                    match &rewrite_logical_definition_view(ac).component {
                        Component::EquivalentClasses(r) => ec_untranslatable(&r.0),
                        _ => ec_untranslatable(&ax.0),
                    }
                } else {
                    ec_untranslatable(&ax.0)
                }
            }
            Component::DisjointClasses(ax) => {
                if for_drop {
                    dc_droppable(&ax.0)
                } else {
                    dc_untranslatable(&ax.0)
                }
            }
            Component::ObjectPropertyRange(ax) => opr_untranslatable(&ax.ope, &ax.ce),
            // A domain on an inverse property, or one that is owl:Thing or
            // owl:Nothing. A class expression as domain is written nowhere.
            Component::ObjectPropertyDomain(ax) => {
                matches!(ax.ope, OPE::InverseObjectProperty(_)) || ce_is_top_or_bottom(&ax.ce)
            }
            // A characteristic, or an inverse pair, on an inverse property.
            Component::TransitiveObjectProperty(ax) => matches!(ax.0, OPE::InverseObjectProperty(_)),
            Component::SymmetricObjectProperty(ax) => matches!(ax.0, OPE::InverseObjectProperty(_)),
            Component::ReflexiveObjectProperty(ax) => matches!(ax.0, OPE::InverseObjectProperty(_)),
            Component::AsymmetricObjectProperty(ax) => matches!(ax.0, OPE::InverseObjectProperty(_)),
            Component::FunctionalObjectProperty(ax) => matches!(ax.0, OPE::InverseObjectProperty(_)),
            Component::InverseFunctionalObjectProperty(ax) => {
                matches!(ax.0, OPE::InverseObjectProperty(_))
            }
            Component::InverseObjectProperties(ax) => {
                !matches!((&ax.0, &ax.1), (OPE::ObjectProperty(_), OPE::ObjectProperty(_)))
            }
            Component::EquivalentObjectProperties(ax) => property_ends(&ax.0).is_none(),
            Component::DisjointObjectProperties(ax) => property_ends(&ax.0).is_none(),
            Component::SubObjectPropertyOf(ax) => sop_untranslatable(ax),
            // A SubAnnotationPropertyOf whose super-property is oboInOwl:SubsetProperty
            // or SynonymTypeProperty becomes a `subsetdef:`/`synonymtypedef:` header
            // line (translatable). Any other super-property (EFO's created_by ⊑
            // dc:creator and skos:prefLabel ⊑ rdfs:label) has no OBO spelling — the
            // sub-property is not a Typedef frame — so it goes in the bag.
            Component::SubAnnotationPropertyOf(ax) => {
                let sup = ax.sup.0.as_ref();
                sup != format!("{OIO}SubsetProperty") && sup != format!("{OIO}SynonymTypeProperty")
            }
            _ => false,
        };
        if unt {
            out.push(ac);
        }
    }
    out
}

/// The set of axioms the OBO writer would divert into the `owl-axioms:` header
/// block — i.e. those `--clean-obo drop-untranslatable-axioms` removes. Owned
/// clones so callers can filter the model against them.
pub fn untranslatable_axioms(
    model: &Model,
) -> HashSet<horned_owl::model::AnnotatedComponent<RcStr>> {
    collect_untranslatable_opt(model, true).into_iter().cloned().collect()
}

/// Whether the ontology declares `oboInOwl:logical-definition-view-relation`.
fn declares_view_relation(model: &Model) -> bool {
    let prop = format!("{OIO}logical-definition-view-relation");
    model.ont.iter().any(|ac| match &ac.component {
        Component::OntologyAnnotation(oa) => oa.0.ap.0.as_ref() == prop,
        _ => false,
    })
}

/// One `EquivalentClasses` axiom under the `logical-definition-view-relation`
/// rewrite — see the call site in `save`. Returns the axiom unchanged when it does
/// not have exactly one named-class operand.
fn rewrite_logical_definition_view(
    ac: &horned_owl::model::AnnotatedComponent<RcStr>,
) -> horned_owl::model::AnnotatedComponent<RcStr> {
    use horned_owl::model::EquivalentClasses;
    let Component::EquivalentClasses(eq) = &ac.component else { return ac.clone() };
    let mut named = 0usize;
    let mut xs: Vec<CE<RcStr>> = Vec::new();
    for x in &eq.0 {
        match x {
            CE::Class(_) => {
                named += 1;
                xs.push(x.clone());
            }
            // The property is only CHECKED against the declared view relation (a
            // mismatch is logged, not acted on), so unwrap whatever it is.
            CE::ObjectSomeValuesFrom { bce, .. } => xs.push((**bce).clone()),
            // Anything else is logged as unexpected and DROPPED, not carried over.
            _ => {}
        }
    }
    if named != 1 {
        return ac.clone();
    }
    // The operands are collected into a set, so equal ones collapse.
    let mut deduped: Vec<CE<RcStr>> = Vec::new();
    for x in xs {
        if !deduped.contains(&x) {
            deduped.push(x);
        }
    }
    // The rewritten axiom is rebuilt with its operands sorted, and the FIRST
    // operand becomes the frame subject, so for two named classes the clause
    // lands on the smaller IRI — `equivalent_to: NBO:0001786` belongs in the
    // NBO:0000313 frame, not the other way round.
    deduped.sort_by(crate::io::owlfunc::cmp_ce);
    horned_owl::model::AnnotatedComponent {
        component: Component::EquivalentClasses(EquivalentClasses(deduped)),
        ann: ac.ann.clone(),
    }
}

/// The token a `property_value:` literal writes. A document written under the
/// OWL API 4.5.6 profile ([`crate::model::Model::owlapi_456`]) quotes a
/// literal only when it must — a value with no space that carries a `:` is
/// written bare, verbatim. Every other document quotes every literal.
fn pv_literal_token(val: &str, owlapi_456: bool) -> String {
    if owlapi_456 && !val.contains(' ') && val.contains(':') {
        val.to_string()
    } else {
        format!("\"{}\"", escape(val))
    }
}

/// A frame may carry at most one of its single-valued tags: a term with two
/// definitions cannot be written as OBO, nor a [Typedef] (each frame is paired
/// with whether it is one) with two domains. The document is refused before a
/// line of it is written, so what the caller finds at the output path is empty.
/// Clauses that come out identical are one clause: a label stated plain, as an
/// `xsd:string` and with a language tag is one `name:`.
fn check_frame_structure<'a>(
    data: &BTreeMap<String, SubjData>,
    frames: impl Iterator<Item = (&'a String, bool)>,
) -> Result<()> {
    fn distinct<'b, T: PartialEq + 'b>(items: impl Iterator<Item = T>) -> usize {
        let mut seen: Vec<T> = Vec::new();
        for item in items {
            if !seen.contains(&item) {
                seen.push(item);
            }
        }
        seen.len()
    }
    for (subj, typedef) in frames {
        let Some(sd) = data.get(subj) else { continue };
        let tag_values = |tag: &'static str| sd.tag_values.iter().filter(move |(t, _, _)| t == tag).map(|(_, v, a)| (v, a));
        // A characteristic is one clause per axiom, plus one for an annotation
        // stating it.
        let characteristic = |tag: &str| {
            let stated = if tag == "is_transitive" {
                sd.transitive_anno.is_some()
            } else {
                sd.bool_tags.contains_key(tag)
            };
            sd.characteristics.get(tag).map_or(0, Vec::len) + usize::from(stated)
        };
        let id = sd.id.as_deref().unwrap_or(subj);
        let typedef_counts = [
            ("domain", distinct(sd.domain.iter().map(|(v, a)| (v, a)).chain(tag_values("domain")))),
            ("range", distinct(sd.range.iter().map(|(v, a)| (v, a)).chain(tag_values("range")))),
        ];
        if let Some((tag, _)) = typedef_counts.iter().find(|(_, n)| typedef && *n > 1) {
            anyhow::bail!(
                "OBO STRUCTURE ERROR Ontology does not conform to OBO structure rules:\n\
                 multiple {tag} tags not allowed. in frame: {id}"
            );
        }
        // An intersection is two or more `intersection_of:` clauses; one alone
        // states nothing a reader can take back.
        if sd.intersection_of.len() + tag_values("intersection_of").count() == 1 {
            anyhow::bail!(
                "OBO STRUCTURE ERROR Ontology does not conform to OBO structure rules:\n\
                 single intersection_of tags are not allowed in frame: {id}"
            );
        }
        let counts = [
            ("name", distinct(sd.name.iter().chain(&sd.extra_names).filter(|(t, _)| !t.is_empty()))),
            ("def", distinct(sd.def.iter().chain(&sd.extra_defs))),
            ("comment", distinct(sd.comments.iter())),
            ("is_reflexive", characteristic("is_reflexive")),
            ("is_symmetric", characteristic("is_symmetric")),
            ("is_transitive", characteristic("is_transitive")),
            ("is_functional", characteristic("is_functional")),
            ("is_inverse_functional", characteristic("is_inverse_functional")),
            ("created_by", distinct(sd.created_by.iter())),
            ("creation_date", distinct(sd.creation_date.iter())),
        ];
        if let Some((tag, _)) = counts.iter().find(|(_, n)| *n > 1) {
            anyhow::bail!(
                "OBO STRUCTURE ERROR Ontology does not conform to OBO structure rules:\n\
                 multiple {tag} tags not allowed. in frame: {id}"
            );
        }
    }
    Ok(())
}

pub fn save<W: Write>(model: &Model, writer: &mut W) -> Result<()> {
    if let Some(ac) = model.ont.iter().find(|ac| relates_through_an_inverse(&ac.component)) {
        anyhow::bail!(
            "the ontology cannot be saved in OBO format: {} relates its class through an inverse \
             property, which a `relationship:` clause has no relation id for",
            crate::io::owlfunc::render_component_line(ac)
        );
    }
    if let Some(ac) = model.ont.iter().find(|ac| {
        matches!(&ac.component, Component::ObjectPropertyRange(r) if matches!(r.ope, OPE::InverseObjectProperty(_)))
    }) {
        anyhow::bail!(
            "the ontology cannot be saved in OBO format: {} states the range of an inverse \
             property, which no [Typedef] stands for",
            crate::io::owlfunc::render_component_line(ac)
        );
    }
    let ctx = Ctx::new(model);
    let mut classes: BTreeSet<String> = BTreeSet::new();
    let mut obj_props: BTreeSet<String> = BTreeSet::new();
    let mut ann_props: BTreeSet<String> = BTreeSet::new();
    let mut individuals: BTreeSet<String> = BTreeSet::new();
    let mut data: BTreeMap<String, SubjData> = BTreeMap::new();
    let mut ont_iri: Option<String> = None;
    let mut ont_version_iri: Option<String> = None;
    let mut header_anns: Vec<(Annotation<RcStr>, BTreeSet<Annotation<RcStr>>)> = Vec::new();
    let mut imports: Vec<String> = Vec::new();

    // Per-subject count of annotation-assertion axioms. A subject's assertions sit
    // in a hash table whose size — hence its bucket order — is fixed by this count,
    // and that order is what breaks ties between clauses of equal value (see
    // `owlapi_aa_bucket`).
    let mut aa_counts: HashMap<String, usize> = HashMap::new();
    // Every `SubClassOf` axiom sits in one ontology-wide hash table; its size (from
    // this total) fixes the bucket order that breaks `is_a:`/`relationship:` clause
    // ties (see `crate::owlapi_hash::axiom_hash`).
    let mut subclass_count: usize = 0;
    // Axioms are consumed one type at a time out of a per-type hash table, so
    // EquivalentClasses axioms arrive in hash-bucket order — which decides which
    // definition reaches a frame FIRST, and therefore whether a later one with an
    // unspellable operand is dropped whole or merely trimmed (see the
    // `ObjectIntersectionOf` arm of `record_ac`). Hold them back and replay them in
    // that order.
    let mut equivs: Vec<&horned_owl::model::AnnotatedComponent<RcStr>> = Vec::new();
    let translated = translated_subjects(model);
    let described = header_described(model);
    for ac in model.ont.iter() {
        let untranslated = match &ac.component {
            Component::AnnotationAssertion(aa) => match &aa.subject {
                AnnotationSubject::IRI(s) => {
                    !translated.contains(s.as_ref()) && !described.contains(s.as_ref())
                }
                _ => true,
            },
            _ => false,
        };
        if matches!(ac.component, Component::EquivalentClasses(_)) {
            equivs.push(ac);
        } else if !untranslated {
            record_ac(ac, &ctx, &mut classes, &mut obj_props, &mut ann_props, &mut individuals, &mut data, &mut ont_iri, &mut ont_version_iri, &mut header_anns, &mut imports);
        }
        match &ac.component {
            Component::AnnotationAssertion(aa) => {
                if let AnnotationSubject::IRI(subj) = &aa.subject {
                    *aa_counts.entry(subj.as_ref().to_string()).or_insert(0) += 1;
                }
            }
            Component::SubClassOf(_) => subclass_count += 1,
            _ => {}
        }
    }
    // When the ontology declares `logical-definition-view-relation`, EVERY
    // EquivalentClasses axiom is rewritten before translation — each
    // `ObjectSomeValuesFrom(p, filler)` operand is replaced by its FILLER, any other
    // anonymous operand is dropped, and the rewrite applies only when exactly one
    // operand is a named class. HPO declares the view relation `has_part`, so
    // `HP:0000002 ≡ has_part some (PATO:0000119 and inheres_in some
    // UBERON:0000468 and …)` becomes `HP:0000002 ≡ (PATO:0000119 and …)` and yields
    // three `intersection_of:` lines; without the rewrite the equivalence is a bare
    // someValuesFrom and no clause comes out of it at all — 12,806 missing lines in
    // `hp-base.obo`.
    //
    // The rewrite precedes the read of the EquivalentClasses axiom set, so the
    // hash-bucket order below is over the REWRITTEN axioms.
    let rewritten: Vec<horned_owl::model::AnnotatedComponent<RcStr>> =
        if declares_view_relation(model) {
            equivs.iter().map(|ac| rewrite_logical_definition_view(ac)).collect()
        } else {
            Vec::new()
        };
    let equivs: Vec<&horned_owl::model::AnnotatedComponent<RcStr>> =
        if rewritten.is_empty() { equivs } else { rewritten.iter().collect() };
    {
        let eq_cap = owlapi_set_cap(equivs.len());
        let mut keyed: Vec<(usize, usize, &horned_owl::model::AnnotatedComponent<RcStr>)> = equivs
            .iter()
            .enumerate()
            .map(|(i, ac)| {
                let Component::EquivalentClasses(eq) = &ac.component else { unreachable!() };
                let h = crate::owlapi_hash::equivalent_classes_hash(&eq.0, &ac.ann, ctx.order) as u32;
                let spread = h ^ (h >> 16);
                ((spread as usize) & (eq_cap - 1), i, *ac)
            })
            .collect();
        keyed.sort_by_key(|(b, i, _)| (*b, *i));
        for (_, _, ac) in keyed {
            record_ac(ac, &ctx, &mut classes, &mut obj_props, &mut ann_props, &mut individuals, &mut data, &mut ont_iri, &mut ont_version_iri, &mut header_anns, &mut imports);
        }
    }
    // A declared class or object property with annotation assertions has a frame
    // whatever its assertions come to: one whose value is an anonymous
    // individual, say, writes no clause.
    for subject in &translated {
        if aa_counts.contains_key(subject) && (classes.contains(subject) || obj_props.contains(subject)) {
            data.entry(subject.clone()).or_default().framed = true;
        }
    }
    let subclass_cap = owlapi_set_cap(subclass_count);
    AA_ALL_CAP.with(|c| c.set(owlapi_set_cap(aa_counts.values().sum())));
    // The header's clauses as the ontology adds them: its imports, its id and
    // version, then its annotations in the ontology's order — by property, then
    // value — which is the order that settles where two tags of one rank fall.
    let mut header: Vec<HeaderClause> = Vec::new();
    for imp in &imports {
        header.push(HeaderClause::new("import", imp, None, imp.clone()));
    }
    {
        // The ontology id strips the OBO PURL base UNCONDITIONALLY and strips a
        // trailing `.owl` only when there is one. Requiring both would leave any
        // non-`.owl` OBO IRI unshortened: HPO's `test_obo` target annotates with
        // `…/obo/test_obo`, and `hp.obo`'s own rule with `…/obo/hp.obo`, which must
        // come out as `ontology: test_obo` / `ontology: hp.obo`. An ANONYMOUS
        // ontology still has the tag, with an empty value: the fourteen
        // `*-minimal.obo` subsets are written from ontologies that carry no IRI, and
        // each has a bare `ontology: ` line.
        let short = match &ont_iri {
            Some(iri) => match iri.strip_prefix(OBO_BASE) {
                Some(rest) => rest.strip_suffix(".owl").unwrap_or(rest).to_string(),
                None => iri.clone(),
            },
            None => String::new(),
        };
        header.push(HeaderClause::new("ontology", &short, None, short.clone()));
    }
    if let Some(dv) = data_version(ont_iri.as_deref(), ont_version_iri.as_deref()) {
        header.push(HeaderClause::new("data-version", &dv, None, dv.clone()));
    }
    header_anns.sort_by(|(a, _), (b, _)| {
        crate::owlapi_hash::iri_cmp(a.ap.0.as_ref(), b.ap.0.as_ref())
            .then_with(|| a.av.cmp(&b.av))
    });
    // A clause equal to one the header already holds is not added again.
    for (ann, quals) in &header_anns {
        let Some(clause) = header_annotation_clause(&ctx, ann, quals, model.owlapi_456) else { continue };
        if !header.iter().any(|c| c.tag == clause.tag && c.key == clause.key && c.text == clause.text) {
            header.push(clause);
        }
    }

    // Alternate ids (the deprecated stubs `alt_id:` expands to, classes and
    // object properties alike) are NOT written as their own stanzas — the reader
    // regenerates them (declaration + owl:deprecated + replaced_by + obsolescence
    // reason) from the primary entity's `alt_id:`. Emitting a stanza would add a
    // spurious `oboInOwl:id`.
    let (alt_classes, alt_targets) = fold_alt_ids(&ctx, &mut data);
    // A merge target that is otherwise undeclared still gets a `[Term]` stanza for
    // its inherited `alt_id:`. An individual's inherited `alt_id:` is written in its
    // [Instance] frame, an object property's in its [Typedef].
    classes.extend(alt_targets.into_iter().filter(|t| !individuals.contains(t) && !obj_props.contains(t)));

    // Two object properties can render to the SAME obo typedef id: RO:0002202 via
    // its `oboInOwl:shorthand` "develops_from" and `bto#develops_from` via its local
    // name. ONE merged `[Typedef]` is emitted. When the extra property carries
    // nothing but a `name:` (bto#develops_from is only labelled "derives from/develops
    // from"), fold that lone name into the content-bearing property and drop it, so a
    // single stanza is written whose two `name:` lines sort together — instead of a
    // second, near-empty duplicate-id stanza.
    {
        let mut by_id: HashMap<String, Vec<String>> = HashMap::new();
        for prop in &obj_props {
            if data.get(prop).map(has_content).unwrap_or(false) {
                by_id.entry(ctx.id(prop)).or_default().push(prop.clone());
            }
        }
        for group in by_id.into_values() {
            if group.len() != 2 {
                continue;
            }
            let a_only = data.get(&group[0]).map(is_name_only).unwrap_or(false);
            let b_only = data.get(&group[1]).map(is_name_only).unwrap_or(false);
            let (primary, secondary) = match (a_only, b_only) {
                (false, true) => (group[0].clone(), group[1].clone()),
                (true, false) => (group[1].clone(), group[0].clone()),
                _ => continue,
            };
            if let Some(sec) = data.remove(&secondary) {
                if let Some(pd) = data.get_mut(&primary) {
                    if let Some(nm) = sec.name {
                        pd.extra_names.push(nm);
                    }
                    pd.extra_names.extend(sec.extra_names);
                    pd.label_axioms.extend(sec.label_axioms);
                    pd.ann_count += sec.ann_count;
                }
            }
        }
    }

    // The label of every subject that has one, keyed by the *rendered* OBO id, so
    // referring clauses can carry the trailing `! label` comment. A property
    // written under its relation shorthand is included: it IS labelled in *value*
    // position (`inverse_of: has_participant ! has participant`, a [Typedef]'s
    // `is_a: transitively_anteriorly_connected_to ! transitively anteriorly
    // connected to`) and unlabelled only as the head of a `RELATION FILLER` clause,
    // which `label_comment_pred` takes care of.
    // A `! label` comment resolves from class and property labels only — never a
    // NamedIndividual's. So a relationship whose value is an individual (uberon's
    // `dc-contributor <ORCID>`, a labelled `owl:NamedIndividual`) gets no comment,
    // while one whose value is a labelled class (CL's `RO:0002292 <ncbigene IRI>`,
    // "expresses LHX6") does. Keep only class/property labels here so
    // `label_comment` naturally omits the individual ones.
    let mut labels: HashMap<String, String> = HashMap::new();
    for (iri, sd) in &data {
        if sd.name.is_some()
            && (classes.contains(iri) || obj_props.contains(iri) || ann_props.contains(iri))
        {
            // The `! label` comment is the `rdfs:label` whose annotation-assertion
            // axiom lands in the minimum hash bucket — the table sized to the
            // subject's total annotation-assertion count. That picks OBI:0000295
            // ("is specified input of"), PR:000003918 ("serum albumin"), part_of
            // and the GSSO multilingual labels.
            if let Some(name) = pick_comment_name(&ctx, iri, sd) {
                labels.insert(ctx.id(iri), name);
            }
        }
    }
    // A clause target with no stanza of its own — an entity declared and
    // labelled by an import — is commented from the closure's labels, while the
    // document imports them. The document's own label wins where both exist.
    let importing = model.ont.iter().any(|ac| matches!(ac.component, horned_owl::model::Component::Import(_)));
    if importing {
        for (iri, label) in &model.banner_labels {
            labels.entry(ctx.id(iri)).or_insert_with(|| label.clone());
        }
    }

    // The body is buffered because the header's `idspace:` lines can only be
    // known once every clause has been rendered (see `Ctx`), and the header is
    // written first.
    let mut body: Vec<u8> = Vec::new();

    // A class declared but with no stanza content (a bare reference, e.g. an
    // imported filler used in a relationship) gets no `[Term]` stanza — it is
    // re-declared from its references on reload, which is why a released `.obo` has
    // far fewer `[Term]` stanzas than declared classes. Emitting a stub stanza
    // would add a spurious `oboInOwl:id`.
    // Stanzas are ordered by their rendered id with a plain case-SENSITIVE
    // comparison — so every CURIE typedef (`BFO:0000066`, `RO:0000052`) precedes
    // every shorthand one (`aboral_to`, `part_of`), because an uppercase letter
    // sorts before a lowercase one. This is not the IRI order a `BTreeSet` gives
    // (`BFO_0000050` = part_of before `BFO_0000066`).
    let by_rendered_id = |set: &BTreeSet<String>| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = set.iter().map(|i| (ctx.id(i), i.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    for (_, class) in by_rendered_id(&classes) {
        if alt_classes.contains(&class) {
            continue;
        }
        match data.get(&class) {
            Some(sd) if has_content(sd) || sd.framed => {
                writeln!(body, "[Term]")?;
                let cap = owlapi_set_cap(aa_counts.get(&class).copied().unwrap_or(0));
                write_stanza(&mut body, &ctx, &labels, &class, Some(sd), Stanza2::Term, cap, subclass_cap)?;
                writeln!(body)?;
            }
            _ => {}
        }
    }
    // Every `[Typedef]` — an object property with content, and an annotation
    // property explicitly tagged `is_metadata_tag` — belongs to ONE id-sorted run,
    // interleaved (`RO:0002131` between `CL:…` and `IAO:…`), not
    // object properties first and metadata tags appended. A bare-referenced object
    // property (a relationship predicate with no typedef content) is re-declared
    // from its use on reload, so it gets no stanza; likewise subset/synonymtype
    // properties, which are header `subsetdef:`/`synonymtypedef:` lines.
    let mut typedefs: Vec<(String, String, Stanza2)> = Vec::new();
    for prop in obj_props.difference(&alt_classes) {
        if data.get(prop).map(|sd| has_content(sd) || sd.framed).unwrap_or(false) {
            typedefs.push((ctx.id(prop), prop.clone(), Stanza2::ObjectProperty));
        }
    }
    for prop in ann_props.difference(&obj_props) {
        let sd = data.get(prop);
        let header_def = sd.map(|s| s.subset_property || s.synonymtype_property).unwrap_or(false);
        let metadata = sd
            .map(|s| s.is_metadata_tag || s.bool_tags.get("is_metadata_tag") == Some(&false))
            .unwrap_or(false);
        if metadata && !header_def {
            typedefs.push((ctx.id(prop), prop.clone(), Stanza2::AnnotationProperty));
        }
    }
    typedefs.sort_by(|a, b| a.0.cmp(&b.0));
    // One stanza per OBO ID, not per IRI. Properties from different namespaces can
    // share a local name — the life-stage ontologies each define `end_dpb`,
    // `has_end_time` and eighteen more — and an OBO id IS the identity, so their
    // stanzas MERGE: the clauses are pooled and sorted within each tag (the
    // reference's single `end_dpb` carries four `namespace:` lines). Every tag
    // pools as a SET — two members with the same `namespace:`, `synonym:`, `xref:`
    // or `alt_id:` yield one line — except `property_value:`, which keeps one line
    // per member, duplicates and all.
    // Rendering is per member and merged as text, so a stanza with no duplicate id
    // is written exactly as before.
    let mut i = 0usize;
    while i < typedefs.len() {
        let mut j = i + 1;
        while j < typedefs.len() && typedefs[j].0 == typedefs[i].0 {
            j += 1;
        }
        writeln!(body, "[Typedef]")?;
        if j - i == 1 {
            let (_, prop, kind) = &typedefs[i];
            let cap = owlapi_set_cap(aa_counts.get(prop).copied().unwrap_or(0));
            write_stanza(&mut body, &ctx, &labels, prop, data.get(prop), *kind, cap, subclass_cap)?;
        } else {
            let mut tags: Vec<String> = Vec::new();
            let mut by_tag: HashMap<String, Vec<String>> = HashMap::new();
            for (_, prop, kind) in &typedefs[i..j] {
                let mut buf: Vec<u8> = Vec::new();
                let cap = owlapi_set_cap(aa_counts.get(prop).copied().unwrap_or(0));
                write_stanza(&mut buf, &ctx, &labels, prop, data.get(prop), *kind, cap, subclass_cap)?;
                for line in String::from_utf8_lossy(&buf).lines() {
                    let tag = line.split_once(':').map(|(t, _)| t.to_string()).unwrap_or_default();
                    if !by_tag.contains_key(&tag) {
                        tags.push(tag.clone());
                    }
                    let repeats = tag == "property_value";
                    let e = by_tag.entry(tag).or_default();
                    if repeats || !e.iter().any(|x| x == line) {
                        e.push(line.to_string());
                    }
                }
            }
            for tag in &tags {
                let mut lines = by_tag.remove(tag).unwrap_or_default();
                // Case-INSENSITIVELY, with a case-sensitive tie-break — the same
                // law every other tag sorts by, so `dog_stages_ontology` precedes
                // `Dpseudobscura_stages_ontology` and both precede `gorilla_…`.
                lines.sort_by(|a, b| fold(a).cmp(&fold(b)).then_with(|| a.cmp(b)));
                for line in lines {
                    writeln!(body, "{line}")?;
                }
            }
        }
        writeln!(body)?;
        i = j;
    }
    // Every named individual with content is an `[Instance]` frame, after the
    // `[Typedef]` run and in the same rendered-id order. An individual that is also
    // a class or a property keeps both frames, each written from its own content;
    // the annotations they share appear in both.
    let mut instance_order: Vec<(String, &String)> =
        individuals.iter().map(|i| (ctx.individual_id(i), i)).collect();
    instance_order.sort();
    for (_, individual) in instance_order {
        match data.get(individual) {
            Some(sd) if instance_has_content(sd) => {
                writeln!(body, "[Instance]")?;
                let cap = owlapi_set_cap(aa_counts.get(individual).copied().unwrap_or(0));
                write_stanza(&mut body, &ctx, &labels, individual, Some(sd), Stanza2::Instance, cap, subclass_cap)?;
                writeln!(body)?;
            }
            _ => {}
        }
    }

    // `subsetdef:` / `synonymtypedef:` header lines (reconstructed from the
    // sub-property-of oboInOwl:SubsetProperty / SynonymTypeProperty axioms plus
    // the property's comment / label).
    // (id, description, the line's qualifier block)
    let mut subsetdefs: Vec<(String, String, String)> = Vec::new();
    let mut syntypedefs: Vec<(String, String, Option<&'static str>, String)> = Vec::new();
    for (iri, sd) in &data {
        if sd.subset_property {
            let descr = sd.comments.first().map(|(t, _)| t.clone()).unwrap_or_default();
            subsetdefs.push((ctx.id(iri), descr, clause_quals(&ctx, &sd.header_def_anns)));
        } else if sd.synonymtype_property {
            let descr = sd.name.as_ref().map(|(t, _)| t.clone()).unwrap_or_default();
            syntypedefs.push((ctx.id(iri), descr, sd.synonym_scope, clause_quals(&ctx, &sd.header_def_anns)));
        }
    }
    subsetdefs.sort_by_key(|(id, d, _)| (fold(id), d.clone()));
    subsetdefs.dedup();
    syntypedefs.sort_by_key(|(id, d, _, _)| (fold(id), d.clone()));
    syntypedefs.dedup();

    if model.obo_structure_check {
        for tag in ["ontology", "format-version", "date", "default-namespace", "saved-by", "auto-generated-by"] {
            if header.iter().filter(|c| c.tag == tag).count() > 1 {
                anyhow::bail!(
                    "OBO STRUCTURE ERROR Ontology does not conform to OBO structure rules:\n\
                     multiple {tag} tags not allowed. in frame: the header"
                );
            }
        }
        check_frame_structure(
            &data,
            classes
                .iter()
                .chain(individuals.iter())
                .map(|s| (s, false))
                .chain(obj_props.iter().chain(ann_props.iter()).map(|s| (s, true))),
        )?;
    }
    // The trailing space is deliberate: `idspace:` has an optional third
    // (quoted description) field, and its separator is always emitted.
    let used = ctx.used.borrow();
    // An OWL document that was read has an xmlns map (`rdf_prefixes`), however few
    // prefixes it declares; one that declares none beyond the built-in ones has
    // been scanned and found to list nothing, which is not the same as there
    // having been no document to scan.
    let scanned = !model.idspaces.is_empty() || !model.rdf_prefixes.is_empty();
    let mut idspaces: Vec<(String, String)> = if !scanned || !model.explicit_prefixes.is_empty()
    {
        // No scanned prefix map (an obo→obo trip, or a pipeline-built model). An
        // `idspace:` line belongs only to a prefix that actually *shortened an id*
        // in the body — a declared-but-unused alias is dropped. CL declares both
        // `terms:` and `dcterms:` for `http://purl.org/dc/terms/` but only ever
        // renders `terms:`, so its header lists `idspace: terms` and not `dcterms`;
        // EFO uses both (`dcterms:` 20k times, `terms:` once) and lists both — hence
        // the filter on `used`.
        // Emit an `idspace:` for every prefix actually used to shorten an id, plus —
        // for a *declared* namespace that no used prefix covers — its first-declared
        // prefix, so the namespace is still represented. This is why CL lists
        // `idspace: swrl` (declared, but never shortening an obo id, and the only
        // prefix for `swrl#`) yet omits `dcterms`/`dce` (declared-but-unused aliases
        // of `dc/terms/` and `dc/elements/`, which `terms`/`dc` already cover).
        // `declared` names get an `idspace:` when their namespace isn't already
        // covered by a *used* prefix; it is the CURIE map plus the document xmlns
        // (`rdf_prefixes`) — so e.g. CL's `sssom`, declared in `cl.owl`'s header but
        // abbreviating nothing (its xref stays a full IRI), still gets a line, while
        // `terms` stays suppressed because `dcterms` covers its namespace.
        //
        // A document declaring only owl/rdf/xsd/rdfs/obo therefore converts to OBO
        // with NO `idspace:` lines at all, and every IRI outside those namespaces
        // stays full.
        let declared: std::collections::HashSet<&str> = model
            .prefixes
            .mappings()
            .map(|(p, _)| p.as_str())
            .chain(model.rdf_prefixes.iter().map(|(p, _)| p.as_str()))
            .collect();
        // Every explicit prefix (`Model::explicit_prefixes`) gets an
        // `idspace:`, whether or not it shortens an id (so mondo's `ICD11`
        // appears with zero references).
        let explicit: std::collections::HashSet<&str> =
            model.explicit_prefixes.iter().map(|(p, _)| p.as_str()).collect();
        let used_ns: std::collections::HashSet<&str> = ctx
            .idspaces
            .iter()
            .filter(|(p, _)| used.contains(p))
            .map(|(_, n)| n.as_str())
            .collect();
        let mut covered_ns: std::collections::HashSet<&str> = std::collections::HashSet::new();
        ctx.idspaces
            .iter()
            .filter(|(p, n)| {
                if used.contains(p) || explicit.contains(p.as_str()) {
                    return true;
                }
                if declared.contains(p.as_str())
                    && !used_ns.contains(n.as_str())
                    && covered_ns.insert(n.as_str())
                {
                    return true;
                }
                false
            })
            .map(|(p, n)| (p.clone(), n.clone()))
            .collect()
    } else {
        // An OWL source: emit the exact prefix set the document declares.
        crate::io::declared_idspaces(model)
    };
    // Header order: case-insensitive by prefix, ties broken by the prefix ASCENDING
    // (so a same-fold pair like `ICD10CM`/`icd10cm` lists uppercase first — the
    // OPPOSITE of the abbreviation tie-break, which prefers the alpha-greatest).
    idspaces.sort_by(|a, b| fold(&a.0).cmp(&fold(&b.0)).then_with(|| a.0.cmp(&b.0)));
    for (prefix, ns) in &idspaces {
        header.push(HeaderClause::new("idspace", prefix, Some(ns), format!("{prefix} {ns} ")));
    }
    drop(used);
    for (id, descr, quals) in &subsetdefs {
        header.push(HeaderClause::new("subsetdef", id, Some(descr), format!("{id} \"{}\"{quals}", escape(descr))));
    }
    for (id, descr, scope, quals) in &syntypedefs {
        let text = match scope {
            Some(scope) => format!("{id} \"{}\" {scope}{quals}", escape(descr)),
            None => format!("{id} \"{}\"{quals}", escape(descr)),
        };
        header.push(HeaderClause::new("synonymtypedef", id, Some(descr), text));
    }
    // The `owl-axioms:` clause — OWL functional syntax carrying the axioms OBO has
    // no tag for, escaped as an unquoted value (a tab stays as it is).
    // `--clean-obo drop-untranslatable-axioms` throws the untranslatable
    // remainder away instead of parking it here, so the clause is omitted
    // entirely (see `Model::obo_drop_untranslatable`).
    {
        let unt =
            if model.obo_drop_untranslatable { Vec::new() } else { collect_untranslatable(model) };
        if let Some(block) = crate::io::owlfunc::render_owl_axioms(&unt, model.plain_literals_typed)? {
            header.push(HeaderClause::new("owl-axioms", &block, None, escape_name(&block)));
        }
    }
    // `format-version` is always 1.2, the version of the document this writer
    // produces, whatever `oboInOwl:hasOBOFormatVersion` the model carries: a 1.4
    // source written back out says 1.2 like every other OBO file.
    writeln!(writer, "format-version: 1.2")?;
    for tag in header_tag_order(&header) {
        if tag == "format-version" {
            continue;
        }
        let mut clauses: Vec<&HeaderClause> = header.iter().filter(|c| c.tag == tag).collect();
        clauses.sort_by(|a, b| {
            clause_value_cmp(&a.key.0, &b.key.0).then_with(|| match (&a.key.1, &b.key.1) {
                (Some(x), Some(y)) => clause_value_cmp(x, y),
                (x, y) => x.is_some().cmp(&y.is_some()),
            })
        });
        for clause in clauses {
            if let Some(text) = &clause.text {
                writeln!(writer, "{tag}: {text}")?;
            }
        }
    }
    writeln!(writer)?;
    writer.write_all(&body)?;
    Ok(())
}

/// One clause of the header frame.
struct HeaderClause {
    tag: String,
    /// The clause's first and second values, which order the clauses of a tag.
    key: (String, Option<String>),
    /// What the line holds after `TAG: `; `None` for a clause written nowhere,
    /// a `property_value:` whose value is an anonymous individual.
    text: Option<String>,
}

impl HeaderClause {
    fn new(tag: &str, first: &str, second: Option<&String>, text: String) -> Self {
        HeaderClause { tag: tag.to_string(), key: (first.to_string(), second.cloned()), text: Some(text) }
    }
}

/// A header tag's rank: the header lists its tags by rank, and a tag with no
/// rank of its own comes after every tag that has one.
fn header_rank(tag: &str) -> u32 {
    match tag {
        "format-version" => 0,
        "data-version" => 10,
        "date" => 15,
        "saved-by" => 20,
        "auto-generated-by" => 25,
        "subsetdef" => 35,
        "synonymtypedef" => 40,
        "default-namespace" => 45,
        "namespace-id-rule" => 46,
        "idspace" => 50,
        "treat-xrefs-as-equivalent" => 55,
        "treat-xrefs-as-genus-differentia" => 60,
        "treat-xrefs-as-relationship" => 65,
        "treat-xrefs-as-is_a" => 70,
        "remark" => 75,
        "import" => 80,
        "ontology" => 85,
        "property_value" => 100,
        "owl-axioms" => 110,
        _ => 10000,
    }
}

/// The header's tags in the order they are written: by rank, and tags of one
/// rank in the order a hash set of every tag the header holds iterates them —
/// by bucket, then by which entered the set first.
fn header_tag_order(header: &[HeaderClause]) -> Vec<&str> {
    let mut tags: Vec<&str> = Vec::new();
    for c in header {
        if !tags.contains(&c.tag.as_str()) {
            tags.push(&c.tag);
        }
    }
    let hashes: Vec<i32> = tags.iter().map(|t| java_hash(t)).collect();
    let mut ordered: Vec<&str> =
        crate::owlapi_hash::hashset_order(&hashes).into_iter().map(|i| tags[i]).collect();
    ordered.sort_by_key(|t| header_rank(t));
    ordered
}

/// Two clause values compared case-insensitively, then case-sensitively.
fn clause_value_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fold(a).cmp(&fold(b)).then_with(|| a.cmp(b))
}

/// The header clause an ontology annotation becomes. A property with a tag is
/// that tag's clause — `rdfs:comment` a `remark:` — qualified by the
/// annotation's own annotations; any other property is a `property_value:`.
/// `None` for an annotation with no clause at all: a blank value of a tag, a
/// literal outside OWL 2's datatype map, a relation shorthand.
fn header_annotation_clause(
    ctx: &Ctx,
    ann: &Annotation<RcStr>,
    quals: &BTreeSet<Annotation<RcStr>>,
    owlapi_456: bool,
) -> Option<HeaderClause> {
    let prop = ann.ap.0.as_ref();
    let tag = match annotation_tag(prop) {
        Some("comment") => Some("remark"),
        tag => tag,
    };
    let Some(tag) = tag else {
        if prop == format!("{OIO}shorthand") || unspellable_datatype(&ann.av) {
            return None;
        }
        let pred = ctx.id(prop);
        let (dbxrefs, _, pieces) = ax_ann_pieces(ctx, quals);
        let rendered = render_quals(&quals_with_xrefs(&dbxrefs, &pieces));
        let (val, is_iri, dt) = ann_value_ctx(ctx, &ann.av);
        let text = match &ann.av {
            AnnotationValue::AnonymousIndividual(_) => None,
            _ if is_iri => Some(format!("{pred} {val}{rendered}")),
            // A literal `property_value` must carry a datatype — a bare quoted
            // value is misread as an IRI. Default to xsd:string.
            _ => Some(format!(
                "{pred} {} {}{rendered}",
                pv_literal_token(&val, owlapi_456),
                dt.unwrap_or_else(|| "xsd:string".into())
            )),
        };
        return Some(HeaderClause { tag: "property_value".into(), key: (pred, Some(val)), text });
    };
    let val = match &ann.av {
        AnnotationValue::Literal(l) if is_boolean_literal(l) => {
            let lexical = l.literal().trim();
            (lexical == "true" || lexical == "1").to_string()
        }
        av => ann_value_ctx(ctx, av).0,
    };
    if val.trim().is_empty() {
        return None;
    }
    // A date is written as [`HeaderDate`] reads it, and one it cannot read is
    // not written. Dates order by their sort text.
    if tag == "date" {
        return HeaderDate::read(&val).map(|date| HeaderClause::new(tag, &date.sort_text(), None, date.obo()));
    }
    let (dbxrefs, syn_type, mut pieces) = ax_ann_pieces(ctx, quals);
    let bracket = |xrefs: &[(String, bool)]| {
        if xrefs.is_empty() {
            String::new()
        } else {
            format!(" {}", render_bracket(xrefs))
        }
    };
    let clause = match tag {
        // A definition's xrefs are its bracket list.
        "def" => {
            let text = format!("{}{}{}", escape_name(&val), bracket(&dbxrefs), render_quals(&quals_with_xrefs_hashset(&[], &pieces)));
            HeaderClause::new(tag, &val, None, text)
        }
        // A synonym is its text and scope, then its type; its xrefs are its
        // bracket list.
        "EXACT" | "NARROW" | "BROAD" | "RELATED" | "synonym" => {
            let mut values = vec![escape_name(&val)];
            let scope = (tag != "synonym").then(|| tag.to_string());
            if let Some(scope) = &scope {
                values.push(scope.clone());
                values.extend(syn_type.map(|t| escape_name(&t)));
            }
            let text = format!("{}{}{}", values.join(" "), bracket(&dbxrefs), render_quals(&quals_with_xrefs_hashset(&[], &pieces)));
            HeaderClause::new("synonym", &val, scope.as_ref(), text)
        }
        // An xref's literal label is its description: `<ID "description">`.
        "xref" => {
            let label = quals
                .iter()
                .filter(|q| q.ap.0.as_ref() == RDFS_LABEL)
                .filter_map(|q| match &q.av {
                    AnnotationValue::Literal(l) => Some(l.literal().trim().to_string()),
                    _ => None,
                })
                .next_back();
            pieces.retain(|q| !(q.0 == RDFS_LABEL && !q.3));
            let xref = match label.filter(|l| !l.is_empty()) {
                Some(l) => format!("<{} \"{l}\">", val.trim()),
                None => val.trim().to_string(),
            };
            let text = format!("{}{}", escape_name(&xref), render_quals(&quals_with_xrefs_hashset(&dbxrefs, &pieces)));
            HeaderClause::new(tag, &val, None, text)
        }
        _ => {
            let text = format!("{}{}", escape_name(&val), render_quals(&quals_with_xrefs_hashset(&dbxrefs, &pieces)));
            HeaderClause::new(tag, &val, None, text)
        }
    };
    Some(clause)
}

/// The `data-version:` header value: the version IRI relative to the ontology id
/// — strip the OBO PURL base, then the leading
/// `<ontology-id>/` and the trailing `/<ontology-id>.owl`. CL's
/// `…/obo/cl/releases/2026-06-08/cl.owl` becomes `releases/2026-06-08`.
fn data_version(ont_iri: Option<&str>, version_iri: Option<&str>) -> Option<String> {
    let v = version_iri?;
    let mut vs = v.strip_prefix(OBO_BASE).unwrap_or(v).to_string();
    // Only OBO-library ontologies carry the `{id}/releases/…/{id}.owl` shape that
    // is shortened against the ontology id; a non-OBO version IRI (EFO's
    // `http://www.ebi.ac.uk/efo/releases/v3.91.0/efo.owl`) is kept verbatim rather
    // than dropping the whole `data-version:` line.
    if let Some(oid) = ont_iri
        .and_then(|iri| iri.strip_prefix(OBO_BASE))
        .and_then(|s| s.strip_suffix(".owl"))
    {
        if let Some(rest) = vs.strip_prefix(&format!("{oid}/")) {
            vs = rest.to_string();
        }
        vs = vs.replace(&format!("/{oid}.owl"), "");
    }
    Some(vs)
}

/// Fold the deprecated "terms merged" stubs into their target's `alt_id:`.
///
/// In OWL, `alt_id: X` on term T is a *separate* deprecated class X carrying
/// `IAO:0100001 replaced_by T` plus obsolescence reason `IAO:0000227` ("terms
/// merged"). Writing OBO again collapses that stub back into T's `alt_id:` and
/// gives it no stanza of its own. Handling only the explicit
/// `oboInOwl:hasAlternativeId` spelling turns CL's 76 merged ids into 76 bogus
/// obsolete `[Term]` stanzas with no `alt_id:` lines at all.
///
/// Returns `(stubs, targets)`: stub IRIs whose stanza must be suppressed, and the
/// merge-target IRIs that gain an `alt_id:` — the latter must be rendered as a
/// `[Term]` even when the target class is otherwise undeclared/contentless (e.g.
/// CHEBI:16422, only referenced as CHEBI:2709's replacement).
fn fold_alt_ids(
    ctx: &Ctx,
    data: &mut BTreeMap<String, SubjData>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut merged: Vec<(String, String)> = Vec::new(); // (target iri, alt id)
    let mut stubs: BTreeSet<String> = BTreeSet::new();
    let mut targets: BTreeSet<String> = BTreeSet::new();
    // A stub that also carries LOGICAL axioms keeps a stanza — of those axioms
    // alone. EMAPA:16045 is a merged stub whose whole content is an
    // `intersection_of` and a `relationship`; folding it away lost both, and the
    // fold takes only the obsolescence bookkeeping with it (its name, synonyms
    // and `is_obsolete:` go too, not just its id).
    let mut logical_only: Vec<String> = Vec::new();
    for (iri, sd) in data.iter() {
        if sd.deprecated
            && sd.obsolescence_reason.as_deref() == Some("IAO:0000227")
            && sd.replaced_by.len() == 1
        {
            merged.push((expand_obo_id(&sd.replaced_by[0]), ctx.id(iri)));
            if sd.is_a.is_empty()
                && sd.relationships.is_empty()
                && sd.intersection_of.is_empty()
                && sd.union_of.is_empty()
                && sd.equivalent_to.is_empty()
                && sd.disjoint_from.is_empty()
            {
                stubs.insert(iri.clone());
            } else {
                logical_only.push(iri.clone());
            }
        }
    }
    for iri in logical_only {
        if let Some(sd) = data.get_mut(&iri) {
            let keep = SubjData {
                id: sd.id.take(),
                is_a: std::mem::take(&mut sd.is_a),
                relationships: std::mem::take(&mut sd.relationships),
                absorbed: std::mem::take(&mut sd.absorbed),
                intersection_of: std::mem::take(&mut sd.intersection_of),
                union_of: std::mem::take(&mut sd.union_of),
                equivalent_to: std::mem::take(&mut sd.equivalent_to),
                disjoint_from: std::mem::take(&mut sd.disjoint_from),
                ..Default::default()
            };
            *sd = keep;
        }
    }
    for (target, alt) in merged {
        targets.insert(target.clone());
        let e = data.entry(target).or_default();
        // The same id can arrive twice — once as an explicit
        // `oboInOwl:hasAlternativeId` on the primary term and once as the merged
        // stub the OBO reader materialises from it — and it is listed once.
        if !e.alt_ids.iter().any(|(a, _)| *a == alt) {
            e.alt_ids.push((alt, BTreeSet::new()));
        }
    }
    // Stubs materialised from an explicit `oboInOwl:hasAlternativeId` are skipped
    // too — but only when they are bare. A class with content of its own keeps
    // its stanza even when another term lists it as an `alt_id:`, and content is
    // not only a name or a definition: EMAPA:16045 is nothing but an
    // `intersection_of` and a `relationship`, and it is still a term.
    let alt_targets: Vec<String> = data
        .values()
        .flat_map(|sd| sd.alt_ids.iter().map(|(a, _)| expand_obo_id(a)))
        .collect();
    for a in alt_targets {
        let is_real = data.get(&a).is_some_and(has_content);
        if !is_real {
            stubs.insert(a);
        }
    }
    (stubs, targets)
}

/// What kind of stanza is being written (controls is_metadata_tag and which
/// property axioms apply).
#[derive(Clone, Copy, PartialEq)]
enum Stanza2 {
    Term,
    ObjectProperty,
    AnnotationProperty,
    Instance,
}

/// Whether a subject carries any OBO stanza content (so it warrants a stanza).
/// A bare reference (only declared, used as an axiom filler) has none.
fn has_content(sd: &SubjData) -> bool {
    sd.id.is_some()
        || sd.name.is_some()
        || !sd.namespace.is_empty()
        || sd.def.is_some()
        || sd.deprecated
        || !sd.comments.is_empty()
        || !sd.synonyms.is_empty()
        || !sd.xrefs.is_empty()
        || !sd.subsets.is_empty()
        || !sd.alt_ids.is_empty()
        || !sd.replaced_by.is_empty()
        || !sd.consider.is_empty()
        || !sd.created_by.is_empty()
        || !sd.creation_date.is_empty()
        || !sd.property_values.is_empty()
        || !sd.is_a.is_empty()
        || !sd.relationships.is_empty()
        || !sd.intersection_of.is_empty()
        || !sd.union_of.is_empty()
        || !sd.equivalent_to.is_empty()
        || !sd.disjoint_from.is_empty()
        || !sd.sub_property_of.is_empty()
        || !sd.domain.is_empty()
        || !sd.range.is_empty()
        || !sd.inverse_of.is_empty()
        || !sd.chains.is_empty()
        || !sd.characteristics.is_empty()
        || sd.is_metadata_tag
        || sd.is_class_level
        || !sd.expand_expression_to.is_empty()
        || !sd.expand_assertion_to.is_empty()
}

/// A subject whose ONLY obo content is one or more `name:` clauses — no id, def,
/// xref, relationship, characteristic, or any other clause. Used to detect the
/// `develops_from` case: two object properties (`RO:0002202` via its shorthand and
/// `bto#develops_from` via its local name) render to the same typedef id, but the
/// bto one carries nothing but a name, so that lone name folds into the single
/// merged `[Typedef]` rather than becoming a second, near-empty stanza.
fn is_name_only(sd: &SubjData) -> bool {
    sd.name.is_some()
        && sd.id.is_none()
        && !sd.deprecated
        && sd.def.is_none()
        && sd.extra_defs.is_empty()
        && sd.namespace.is_empty()
        && sd.comments.is_empty()
        && sd.synonyms.is_empty()
        && sd.xrefs.is_empty()
        && sd.subsets.is_empty()
        && sd.alt_ids.is_empty()
        && sd.replaced_by.is_empty()
        && sd.consider.is_empty()
        && sd.created_by.is_empty()
        && sd.creation_date.is_empty()
        && sd.property_values.is_empty()
        && sd.rel_or_pv.is_empty()
        && sd.is_a.is_empty()
        && sd.relationships.is_empty()
        && sd.intersection_of.is_empty()
        && sd.union_of.is_empty()
        && sd.equivalent_to.is_empty()
        && sd.disjoint_from.is_empty()
        && sd.sub_property_of.is_empty()
        && sd.domain.is_empty()
        && sd.range.is_empty()
        && sd.inverse_of.is_empty()
        && sd.chains.is_empty()
        && sd.characteristics.is_empty()
        && !sd.is_metadata_tag
        && !sd.is_class_level
        && sd.expand_expression_to.is_empty()
        && sd.expand_assertion_to.is_empty()
        && sd.instance_of.is_empty()
        && sd.assertions.is_empty()
}

/// Whether a named individual warrants an [Instance] frame: any of the content a
/// [Term] or [Typedef] frame could carry, or a class or property assertion of its
/// own. An individual that is only declared has none.
fn instance_has_content(sd: &SubjData) -> bool {
    has_content(sd) || !sd.instance_of.is_empty() || !sd.assertions.is_empty()
}

#[allow(clippy::too_many_arguments)]
fn record_ac(
    ac: &horned_owl::model::AnnotatedComponent<RcStr>,
    ctx: &Ctx,
    classes: &mut BTreeSet<String>,
    obj_props: &mut BTreeSet<String>,
    ann_props: &mut BTreeSet<String>,
    individuals: &mut BTreeSet<String>,
    data: &mut BTreeMap<String, SubjData>,
    ont_iri: &mut Option<String>,
    ont_version_iri: &mut Option<String>,
    header_anns: &mut Vec<(Annotation<RcStr>, BTreeSet<Annotation<RcStr>>)>,
    imports: &mut Vec<String>,
) {
    let comp = &ac.component;
    let axanns = &ac.ann;
    match comp {
        Component::OntologyID(id) => {
            if let Some(iri) = &id.iri {
                *ont_iri = Some(iri.as_ref().to_string());
            }
            if let Some(viri) = &id.viri {
                *ont_version_iri = Some(viri.as_ref().to_string());
            }
        }
        // An ontology annotation's own annotations are its clause's qualifiers.
        Component::OntologyAnnotation(oa) => {
            let quals: BTreeSet<Annotation<RcStr>> = oa.0.ann.iter().chain(axanns).cloned().collect();
            header_anns.push((oa.0.clone(), quals));
        }
        Component::Import(i) => {
            imports.push(i.0.as_ref().to_string());
        }
        Component::DeclareClass(dc) => {
            classes.insert(dc.0 .0.as_ref().to_string());
        }
        Component::DeclareObjectProperty(dp) => {
            obj_props.insert(dp.0 .0.as_ref().to_string());
        }
        Component::DeclareAnnotationProperty(ap) => {
            ann_props.insert(ap.0 .0.as_ref().to_string());
        }
        // Individuals are recorded only when the writer spells them as [Instance]
        // frames (see `INSTANCE_FRAMES`); otherwise these three fall through to the
        // arm that ignores them.
        Component::DeclareNamedIndividual(ni) if instance_frames() => {
            individuals.insert(ni.0 .0.as_ref().to_string());
        }
        // A named individual's class assertion is an `instance_of:` clause of its
        // [Instance] frame. `ClassAssertion(owl:Thing x)` is vacuous, as `is_a:
        // owl:Thing` is, and writes nothing.
        Component::ClassAssertion(ca) if instance_frames() => {
            if let (CE::Class(c), Individual::Named(i)) = (&ca.ce, &ca.i) {
                let subject = i.0.as_ref().to_string();
                individuals.insert(subject.clone());
                if c.0.as_ref() != OWL_THING {
                    data.entry(subject)
                        .or_default()
                        .instance_of
                        .push((ctx.id(c.0.as_ref()), axanns.clone()));
                }
            }
        }
        // An assertion between two named individuals is a `relationship:` clause of
        // the subject's [Instance] frame.
        Component::ObjectPropertyAssertion(opa) if instance_frames() => {
            if let (OPE::ObjectProperty(r), Individual::Named(from), Individual::Named(to)) =
                (&opa.ope, &opa.from, &opa.to)
            {
                let subject = from.0.as_ref().to_string();
                individuals.insert(subject.clone());
                data.entry(subject).or_default().assertions.push((
                    ctx.id(r.0.as_ref()),
                    ctx.individual_id(to.0.as_ref()),
                    axanns.clone(),
                ));
            }
        }
        Component::SubClassOf(sc) => {
            // Only a named subject produces a clause. A named subclass is a plain
            // `is_a:`/`relationship:`; a General Class Inclusion whose subclass is
            // `NamedClass ⊓ ∃gci_rel.gci_filler` is written on `NamedClass` with
            // `{gci_relation="…", gci_filler="…"}` qualifiers, in both the `is_a:`
            // and the `relationship:` form. That is the OBO spelling of a
            // subsumption holding only in a given taxon/context — CL:0000163 is an
            // enteroendocrine cell only as part of the stomach, and CL has 136 such
            // axioms; dropping them on write deletes them from an obo→owl→obo round
            // trip.
            let (subject, gci): (Option<String>, Vec<(String, String)>) = match &sc.sub {
                CE::Class(sub) => (Some(sub.0.as_ref().to_string()), Vec::new()),
                CE::ObjectIntersectionOf(parts) => {
                    let named = parts.iter().find_map(|p| match p {
                        CE::Class(c) => Some(c.0.as_ref().to_string()),
                        _ => None,
                    });
                    let restr = parts.iter().find_map(|p| match p {
                        CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(r), bce } => {
                            match bce.as_ref() {
                                CE::Class(f) => {
                                    Some((ctx.id(r.0.as_ref()), ctx.id(f.0.as_ref())))
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    });
                    match (named, restr) {
                        (Some(n), Some((rel, fill))) => (
                            Some(n),
                            vec![
                                ("gci_relation".to_string(), rel),
                                ("gci_filler".to_string(), fill),
                            ],
                        ),
                        _ => (None, Vec::new()),
                    }
                }
                _ => (None, Vec::new()),
            };
            if let Some(s) = subject {
                classes.insert(s.clone());
                let framed = !ce_is_top_or_bottom(&sc.sub) && !ce_is_top_or_bottom(&sc.sup);
                // The whole SubClassOf axiom's hash fixes its position in the
                // per-type axiom table — hence the tie-order of same-value
                // is_a/relationship clauses. Computed once here (full IRIs in hand); a
                // superclass conjunction that splits into several clauses shares it.
                let sc_hash = crate::owlapi_hash::axiom_hash(comp, axanns, ctx.order).unwrap_or_default();
                let e = data.entry(s).or_default();
                e.framed |= framed;
                // The GCI context rides along as extra qualifiers on the line.
                let gci_quals = gci;
                // A conjunction in superclass position is not one clause but
                // several: `C ⊑ (∃has_part.X ⊓ ∃has_part.Y)` is two
                // `relationship:` lines, exactly as if it had been written as two
                // SubClassOf axioms. OBO has no way to say "and" on the right-hand
                // side, so the axiom splits. Dropping it whole would cost CL's
                // `cl.obo` 821 `relationship:` lines, all of CLM's multi-gene
                // NS-forest marker sets.
                // Each queued expression carries whether it was reached by
                // descending into a superclass CONJUNCTION, which decides the
                // `{all_only="true"}` qualifier below.
                let mut queue: std::collections::VecDeque<(&CE<RcStr>, bool)> =
                    std::collections::VecDeque::new();
                // An all-restriction superclass intersection splits
                // (`C ⊑ (∃R.X ⊓ ∃R.Y)` → two `relationship:` lines), but only when
                // EVERY conjunct is translatable. If any operand is untranslatable —
                // a mixed intersection with a *named-class* conjunct (EFO's BTO cell
                // lines, `BTO ⊑ (CL:0000010 ⊓ ∃RO_0000053.MONDO)`), or a restriction
                // with a non-named filler (`HP:0001891 ⊑ (∃RO_0000056.(…complex…) ⊓
                // ∃RO_0000057.CHEBI_18248)`) — the WHOLE axiom goes to the
                // `owl-axioms:` bag and NEITHER conjunct is emitted. Partially
                // splitting it (emitting only the simple operand) would put a
                // spurious `relationship:` line in the frame. `subclassof_
                // untranslatable` is the reachability test.
                if !subclassof_untranslatable(&sc.sub, &sc.sup) {
                    queue.push_back((&sc.sup, false));
                }
                // Clauses this ONE axiom contributes; deduped before they join the
                // frame (see the note on the conjunction branch below).
                let rel_start = e.relationships.len();
                while let Some((sup, in_inter)) = queue.pop_front() {
                    match sup {
                        CE::Class(sup) => {
                            // `C ⊑ owl:Thing` is vacuous, so `is_a: owl:Thing` is
                            // never written.
                            if sup.0.as_ref() != format!("{OWL_NS}Thing") {
                                e.is_a.push((
                                    ctx.id(sup.0.as_ref()),
                                    axanns.clone(),
                                    gci_quals.clone(),
                                    sc_hash,
                                ));
                            }
                        }
                        CE::ObjectSomeValuesFrom { ope, bce } => {
                            if let (OPE::ObjectProperty(r), CE::Class(t)) = (ope, bce.as_ref()) {
                                e.relationships.push((
                                    ctx.id(r.0.as_ref()),
                                    ctx.id(t.0.as_ref()),
                                    axanns.clone(),
                                    gci_quals.clone(),
                                    sc_hash,
                                ));
                            }
                        }
                        // A universal restriction is the same `REL FILLER` clause
                        // flagged `all_only` — BFO's `part_of only continuant`
                        // reads `relationship: BFO:0000050 BFO:0000002
                        // {all_only="true"}`. Cardinality bounds ride along the
                        // same way as in an `intersection_of:` clause.
                        CE::ObjectAllValuesFrom { ope, bce } => {
                            if let (OPE::ObjectProperty(r), CE::Class(t)) = (ope, bce.as_ref()) {
                                let mut q = gci_quals.clone();
                                // …but ONLY when the universal is the whole
                                // superclass. Inside a conjunction every conjunct
                                // is a PLAIN `relationship:` clause:
                                //   C ⊑ (∃R.Y ⊓ ∀R.Z)  →  `R Y` and `R Z`, unqualified.
                                if !in_inter {
                                    q.push(("all_only".to_string(), "true".to_string()));
                                }
                                e.relationships.push((
                                    ctx.id(r.0.as_ref()),
                                    ctx.id(t.0.as_ref()),
                                    axanns.clone(),
                                    q,
                                    sc_hash,
                                ));
                            }
                        }
                        CE::ObjectExactCardinality { .. }
                        | CE::ObjectMinCardinality { .. }
                        | CE::ObjectMaxCardinality { .. } => {
                            if let Some((toks, q)) = ce_to_inter_tokens(ctx, sup) {
                                if toks.len() == 2 {
                                    let mut quals = gci_quals.clone();
                                    quals.extend(q);
                                    e.relationships.push((
                                        toks[0].clone(),
                                        toks[1].clone(),
                                        axanns.clone(),
                                        quals,
                                        sc_hash,
                                    ));
                                }
                            }
                        }
                        CE::ObjectIntersectionOf(parts) => {
                            queue.extend(parts.iter().map(|p| (p, true)))
                        }
                        _ => {}
                    }
                }
                // The conjuncts over one relation and filler are one clause: the
                // first in the conjunction's order (an existential or a universal,
                // then a minimum, an exact and a maximum cardinality) stands, and
                // the others' qualifiers follow its own. MONDO's FOODON_03400229,
                // `⊑ (∃has_member.X ⊓ ∀has_member.X)`, is the single
                // `relationship: RO:0002351 FOODON:03301977`. A separate
                // `SubClassOf(C, ∃R.Y)` axiom still adds its own clause.
                let mut conjuncts = e.relationships.split_off(rel_start);
                conjuncts.sort_by_key(|(_, _, _, q, _)| {
                    q.iter()
                        .find_map(|(k, _)| match k.as_str() {
                            "minCardinality" => Some(1),
                            "cardinality" => Some(2),
                            "maxCardinality" => Some(3),
                            _ => None,
                        })
                        .unwrap_or(0)
                });
                for (r, t, anns, q, hash) in conjuncts {
                    if e.relationships[rel_start..].iter().any(|(r2, t2, ..)| *r2 == r && *t2 == t) {
                        e.absorbed.entry((r, t, hash)).or_default().push((anns, q));
                    } else {
                        e.relationships.push((r, t, anns, q, hash));
                    }
                }
            }
        }
        Component::EquivalentClasses(eq) => {
            // Named class C ≡ expr → intersection_of / union_of / equivalent_to.
            // Only an equivalence of two members has an OBO spelling, and one
            // naming owl:Thing or owl:Nothing has none; either goes whole to the
            // header's `owl-axioms`.
            let named: Vec<&str> = eq.0.iter().filter_map(|m| match m {
                CE::Class(c) => Some(c.0.as_ref()),
                _ => None,
            }).collect();
            let spellable = eq.0.len() == 2 && !eq.0.iter().any(ce_is_top_or_bottom);
            if let Some(subj) = named.first().filter(|_| spellable).map(|s| s.to_string()) {
                classes.insert(subj.clone());
                let e = data.entry(subj).or_default();
                e.framed = true;
                for m in &eq.0 {
                    match m {
                        CE::Class(_) => {} // handled as subject / equivalent_to below
                        CE::ObjectIntersectionOf(parts) => {
                            // All-or-nothing, but only for the FIRST definition on a
                            // frame. An operand OBO cannot spell (a nested anonymous
                            // filler such as CL:0000041's `has_part some (nucleus and
                            // bearer_of some polymorphic)`) is fatal to the *whole*
                            // equivalence — none of its clauses are added — EXCEPT
                            // when the frame already carries `intersection_of:`
                            // clauses from an earlier axiom. Then the unspellable
                            // operand is merely skipped, so the rest of this axiom
                            // still lands. Eight EFO classes have a second definition
                            // whose filler is a union, and its genus line comes out
                            // as a second `intersection_of: EFO:0000408`.
                            let had_inter = !e.intersection_of.is_empty();
                            let mut untranslatable = false;
                            let mut lines: Vec<(Vec<String>, Vec<(String, String)>)> = Vec::new();
                            for p in parts {
                                match ce_to_inter_tokens(ctx, p) {
                                    Some(toks) => lines.push(toks),
                                    None => {
                                        if !had_inter {
                                            untranslatable = true;
                                        }
                                    }
                                }
                            }
                            if !untranslatable {
                                // The axiom annotations belong to every line —
                                // `{xref="PMID:27565351"}` repeats on each
                                // `intersection_of:` of CL:0000754.
                                e.intersection_of.extend(
                                    lines.into_iter().map(|(toks, q)| (toks, q, axanns.clone())),
                                );
                            }
                        }
                        CE::ObjectUnionOf(parts) => {
                            for p in parts {
                                if let CE::Class(c) = p {
                                    e.union_of.push(ctx.id(c.0.as_ref()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Pairwise named equivalences (C ≡ D) → equivalent_to.
                for other in &named[1..] {
                    let e = data.entry(named[0].to_string()).or_default();
                    e.equivalent_to.push((ctx.id(other), axanns.clone()));
                }
            }
        }
        Component::DisjointClasses(dj) => {
            let named: Vec<&str> = dj.0.iter().filter_map(|m| match m {
                CE::Class(c) => Some(c.0.as_ref()),
                _ => None,
            }).collect();
            if named.len() == 2 {
                classes.insert(named[0].to_string());
                let e = data.entry(named[0].to_string()).or_default();
                e.disjoint_from.push((ctx.id(named[1]), axanns.clone()));
            } else if named.len() >= 3 {
                // A nary DisjointClasses maps to a SINGLE `disjoint_from:` clause,
                // on the first two members in IRI order (namespace, then local
                // name) — `DisjointClasses(A B C)` in any input order yields
                // `A disjoint_from B`. The other pairs are dropped, as OBO has no
                // nary disjoint form.
                let mut sorted = named.clone();
                sorted.sort_unstable_by(|a, b| crate::owlapi_hash::iri_cmp(a, b));
                classes.insert(sorted[0].to_string());
                let e = data.entry(sorted[0].to_string()).or_default();
                e.disjoint_from.push((ctx.id(sorted[1]), axanns.clone()));
            }
        }
        Component::AnnotationAssertion(aa) => {
            if let AnnotationSubject::IRI(subj) = &aa.subject {
                record_annotation(ctx, subj.as_ref(), &aa.ann, axanns, data);
            }
        }
        // --- Object-property axioms → Typedef clauses. ---
        // A clause starts the [Typedef] of the property it is written on, declared
        // or not, and carries its axiom's annotations as qualifiers.
        Component::SubObjectPropertyOf(sp) => {
            use horned_owl::model::SubObjectPropertyExpression as SOPE;
            match (&sp.sub, &sp.sup) {
                // `R ⊑ S` is `is_a: S` on R. A side that is owl:topObjectProperty or
                // owl:bottomObjectProperty, or a super-property in the OWL
                // namespace, writes nothing at all — no clause and no `owl-axioms`
                // entry (RO_0015001 in CL's import closure is `⊑
                // owl:topObjectProperty`).
                (SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)), OPE::ObjectProperty(sup)) => {
                    let silent = is_top_or_bottom_property(sub.0.as_ref())
                        || is_top_or_bottom_property(sup.0.as_ref())
                        || sup.0.as_ref().starts_with(OWL_NS);
                    if !silent {
                        let e = typedef_frame(obj_props, data, sub.0.as_ref());
                        e.sub_property_of.push((ctx.id(sup.0.as_ref()), axanns.clone()));
                    }
                }
                // R1∘R2 ⊑ sup → `holds_over_chain`/`transitive_over` on `sup`.
                (SOPE::ObjectPropertyChain(chain), OPE::ObjectProperty(sup)) if !sop_untranslatable(sp) => {
                    let toks: Vec<String> = chain.iter().filter_map(|o| match o {
                        OPE::ObjectProperty(p) => Some(ctx.id(p.0.as_ref())),
                        _ => None,
                    }).collect();
                    let e = typedef_frame(obj_props, data, sup.0.as_ref());
                    e.chains.push((toks, axanns.clone()));
                }
                _ => {}
            }
        }
        Component::SubAnnotationPropertyOf(sp) => {
            // Only a sub-property of oboInOwl:SubsetProperty or
            // SynonymTypeProperty has a spelling (a header `subsetdef:` or
            // `synonymtypedef:` line); any other goes to `owl-axioms`.
            let sup = sp.sup.0.as_ref();
            let e = data.entry(sp.sub.0.as_ref().to_string()).or_default();
            if sup == format!("{OIO}SubsetProperty") {
                e.subset_property = true;
                e.header_def_anns = axanns.clone();
            } else if sup == format!("{OIO}SynonymTypeProperty") {
                e.synonymtype_property = true;
                e.header_def_anns = axanns.clone();
            }
        }
        Component::TransitiveObjectProperty(p) => characteristic(obj_props, data, &p.0, "is_transitive", axanns),
        Component::SymmetricObjectProperty(p) => characteristic(obj_props, data, &p.0, "is_symmetric", axanns),
        Component::ReflexiveObjectProperty(p) => characteristic(obj_props, data, &p.0, "is_reflexive", axanns),
        Component::AsymmetricObjectProperty(p) => characteristic(obj_props, data, &p.0, "is_asymmetric", axanns),
        Component::FunctionalObjectProperty(p) => characteristic(obj_props, data, &p.0, "is_functional", axanns),
        Component::InverseFunctionalObjectProperty(p) => {
            characteristic(obj_props, data, &p.0, "is_inverse_functional", axanns)
        }
        // A domain or range that is a named class is that clause. One that is
        // owl:Thing or owl:Nothing still starts the property's frame, while the
        // axiom itself goes to `owl-axioms`. A class expression as domain writes
        // nothing anywhere.
        Component::ObjectPropertyDomain(pd) => {
            if let (OPE::ObjectProperty(p), CE::Class(c)) = (&pd.ope, &pd.ce) {
                let e = typedef_frame(obj_props, data, p.0.as_ref());
                if !ce_is_top_or_bottom(&pd.ce) {
                    e.domain.push((ctx.id(c.0.as_ref()), axanns.clone()));
                }
            }
        }
        Component::ObjectPropertyRange(pr) => {
            if let (OPE::ObjectProperty(p), CE::Class(c)) = (&pr.ope, &pr.ce) {
                let e = typedef_frame(obj_props, data, p.0.as_ref());
                if !ce_is_top_or_bottom(&pr.ce) {
                    e.range.push((ctx.id(c.0.as_ref()), axanns.clone()));
                }
            }
        }
        Component::InverseObjectProperties(inv) => {
            if let (OPE::ObjectProperty(a), OPE::ObjectProperty(b)) = (&inv.0, &inv.1) {
                let e = typedef_frame(obj_props, data, a.0.as_ref());
                e.inverse_of.push((ctx.id(b.0.as_ref()), axanns.clone()));
            }
        }
        // An equivalence or disjointness of object properties is one clause of
        // its first member's [Typedef], naming its last member: of three or more,
        // the members between are written nowhere. One with an inverse member
        // goes to `owl-axioms:` instead (see `property_ends`).
        Component::EquivalentObjectProperties(eq) => {
            if let Some((first, last)) = property_ends(&eq.0) {
                let e = typedef_frame(obj_props, data, &first);
                e.equivalent_to.push((ctx.id(&last), axanns.clone()));
            }
        }
        Component::DisjointObjectProperties(dj) => {
            if let Some((first, last)) = property_ends(&dj.0) {
                let e = typedef_frame(obj_props, data, &first);
                e.disjoint_from.push((ctx.id(&last), axanns.clone()));
            }
        }
        _ => {}
    }
}

/// The [Typedef] frame of the object property `iri`, started if it has none.
fn typedef_frame<'a>(
    obj_props: &mut BTreeSet<String>,
    data: &'a mut BTreeMap<String, SubjData>,
    iri: &str,
) -> &'a mut SubjData {
    obj_props.insert(iri.to_string());
    let e = data.entry(iri.to_string()).or_default();
    e.framed = true;
    e
}

/// A characteristic axiom on a named property: one `TAG: true` clause of its
/// frame. On an inverse property it goes to `owl-axioms`.
fn characteristic(
    obj_props: &mut BTreeSet<String>,
    data: &mut BTreeMap<String, SubjData>,
    ope: &OPE<RcStr>,
    tag: &'static str,
    axanns: &BTreeSet<Annotation<RcStr>>,
) {
    if let OPE::ObjectProperty(p) = ope {
        let e = typedef_frame(obj_props, data, p.0.as_ref());
        e.characteristics.entry(tag).or_default().push(axanns.clone());
    }
}

/// owl:topObjectProperty or owl:bottomObjectProperty.
fn is_top_or_bottom_property(iri: &str) -> bool {
    iri == "http://www.w3.org/2002/07/owl#topObjectProperty"
        || iri == "http://www.w3.org/2002/07/owl#bottomObjectProperty"
}

/// An `intersection_of` operand → its OBO tokens (`[genus]` or `[rel filler]`).
/// The canonical class-expression sort key: the expression's type index first,
/// then the *raw* IRIs of the property and filler. Used to pick which operand of a
/// nested `ObjectIntersectionOf` survives (the last one), which must not depend on
/// shorthand rendering.
pub(crate) fn owlapi_ce_sort_key(ce: &CE<RcStr>) -> (u8, String, String) {
    let ope_iri = |ope: &OPE<RcStr>| match ope {
        OPE::ObjectProperty(r) => r.0.as_ref().to_string(),
        OPE::InverseObjectProperty(r) => r.0.as_ref().to_string(),
    };
    let bce_iri = |bce: &CE<RcStr>| match bce {
        CE::Class(t) => t.0.as_ref().to_string(),
        _ => String::new(),
    };
    match ce {
        CE::Class(c) => (10, c.0.as_ref().to_string(), String::new()),
        CE::ObjectIntersectionOf(_) => (31, String::new(), String::new()),
        CE::ObjectSomeValuesFrom { ope, bce } => (34, ope_iri(ope), bce_iri(bce)),
        CE::ObjectAllValuesFrom { ope, bce } => (35, ope_iri(ope), bce_iri(bce)),
        CE::ObjectMinCardinality { ope, bce, .. } => (37, ope_iri(ope), bce_iri(bce)),
        CE::ObjectExactCardinality { ope, bce, .. } => (38, ope_iri(ope), bce_iri(bce)),
        CE::ObjectMaxCardinality { ope, bce, .. } => (39, ope_iri(ope), bce_iri(bce)),
        _ => (99, String::new(), String::new()),
    }
}

fn ce_to_inter_tokens(ctx: &Ctx, ce: &CE<RcStr>) -> Option<(Vec<String>, Vec<(String, String)>)> {
    match ce {
        CE::Class(c) => Some((vec![ctx.id(c.0.as_ref())], Vec::new())),
        CE::ObjectSomeValuesFrom { ope, bce } => match bce.as_ref() {
            CE::Class(t) => Some((relation_and(ctx, ope, t.0.as_ref()), Vec::new())),
            _ => None,
        },
        // A cardinality restriction is the same `REL FILLER` clause carrying the
        // bound as a qualifier — UBERON's `zygapophysis ≡ skeletal joint and
        // connects exactly 2 vertebral centrum` is
        // `intersection_of: RO:0002176 UBERON:0001075 {cardinality="2"}`.
        // A universal restriction is the same `REL FILLER` clause flagged
        // `all_only` — OBI:0002076 `≡ material entity and has member only
        // specimen` is `intersection_of: RO:0002351 OBI:0100051
        // {all_only="true"}`. (A universal inside a SUPERCLASS conjunction is
        // rendered unqualified instead; that path is handled separately.)
        // A universal over a complement is the operand's class with
        // `{cardinality="0"}`.
        CE::ObjectAllValuesFrom { ope, bce } => match bce.as_ref() {
            CE::Class(t) => Some((
                relation_and(ctx, ope, t.0.as_ref()),
                vec![("all_only".to_string(), "true".to_string())],
            )),
            CE::ObjectComplementOf(inner) => match inner.as_ref() {
                CE::Class(t) => Some((
                    relation_and(ctx, ope, t.0.as_ref()),
                    vec![("cardinality".to_string(), "0".to_string())],
                )),
                _ => None,
            },
            _ => None,
        },
        CE::ObjectExactCardinality { n, ope, bce } => {
            card_tokens(ctx, ope, bce, "cardinality", *n)
        }
        CE::ObjectMinCardinality { n, ope, bce } => {
            card_tokens(ctx, ope, bce, "minCardinality", *n)
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            card_tokens(ctx, ope, bce, "maxCardinality", *n)
        }
        // A conjunction nested inside a conjunction has no OBO spelling. Exactly one
        // of its operands survives — the last in canonical order, which puts a named
        // class before a restriction and then orders by the rendered tokens — and it
        // is flagged `all_some="true"`; the rest are dropped. MBA:10688's
        // `(paraflocculus and part_of some Mus musculus) and part_of some
        // paraflocculus` comes out as the `part_of Mus musculus` line alone, without
        // the named operand. CL has 15 such terms; treating the nested conjunction
        // as untranslatable instead loses their whole definition, genus included.
        CE::ObjectIntersectionOf(inner) => {
            let mut lines: Vec<(Vec<String>, Vec<(String, String)>, (u8, String, String))> = inner
                .iter()
                .filter_map(|p| ce_to_inter_tokens(ctx, p).map(|(t, q)| (t, q, owlapi_ce_sort_key(p))))
                .collect();
            if lines.len() != inner.len() {
                return None;
            }
            // Order canonically — class-expression type, then the
            // *raw* IRIs of property and filler — and keep the last. This is not the
            // *rendered* token order: `has_part` (BFO:0000051's shorthand) sorts
            // after `RO:0000053`, but BFO_0000051 sorts before RO_0000053, so
            // CL:0008008's `has characteristic striated` (not `has_part sarcomere`)
            // is the surviving operand.
            lines.sort_by(|a, b| a.2.cmp(&b.2));
            let (toks, mut q, _) = lines.pop()?;
            q.push(("all_some".to_string(), "true".to_string()));
            Some((toks, q))
        }
        _ => None,
    }
}

/// Shared shape of the three cardinality restrictions.
fn card_tokens(
    ctx: &Ctx,
    ope: &OPE<RcStr>,
    bce: &CE<RcStr>,
    qual: &str,
    n: u32,
) -> Option<(Vec<String>, Vec<(String, String)>)> {
    let CE::Class(t) = bce else { return None };
    Some((relation_and(ctx, ope, t.0.as_ref()), vec![(qual.to_string(), n.to_string())]))
}

/// A restriction's clause tokens: its relation and then its filler. An inverse
/// property has no id of its own, so its restriction is the filler alone —
/// `intersection_of: X` for `inverse(p) some X`, and the clause no longer says
/// how X is reached.
fn relation_and(ctx: &Ctx, ope: &OPE<RcStr>, filler: &str) -> Vec<String> {
    match ope {
        OPE::ObjectProperty(r) => vec![ctx.id(r.0.as_ref()), ctx.id(filler)],
        OPE::InverseObjectProperty(_) => vec![ctx.id(filler)],
    }
}

/// Sort one `AnnotationAssertion` into the right OBO tag of its subject.
fn record_annotation(
    ctx: &Ctx,
    subj: &str,
    ann: &Annotation<RcStr>,
    axanns: &BTreeSet<Annotation<RcStr>>,
    data: &mut BTreeMap<String, SubjData>,
) {
    // A property whose tag a frame clause spells is that tag's property,
    // whatever IRI names it.
    let canonical;
    let prop = match frame_tag(ctx, ann.ap.0.as_ref()) {
        Some(tag) => {
            canonical = tag_iri(tag);
            canonical.as_str()
        }
        None => ann.ap.0.as_ref(),
    };
    let e = data.entry(subj.to_string()).or_default();
    e.ann_count += 1;
    // A blank value of a tag, and a value no `property_value:` can spell, go to
    // `owl-axioms` instead (see `collect_untranslatable_opt`).
    let blank = matches!(&ann.av, AnnotationValue::Literal(l)
        if l.literal().trim().is_empty() && !is_boolean_literal(l));
    if (blank && annotation_tag(prop).is_some()) || unspellable_property_value(ann) {
        return;
    }
    // An anonymous individual is a `property_value:` with no value, which is
    // never written.
    if matches!(ann.av, AnnotationValue::AnonymousIndividual(_)) && annotation_tag(prop).is_none() {
        return;
    }
    let oio = prop.strip_prefix(OIO);
    let (val, is_iri, dt) = ann_value_ctx(ctx, &ann.av);
    // The value's language tag, kept for the synonym clauses: it is part of the
    // axiom hash that buckets tied clauses (see `SubjData::synonyms`).
    let val_lang: Option<String> = match &ann.av {
        AnnotationValue::Literal(Literal::Language { lang, .. }) => Some(lang.clone()),
        _ => None,
    };
    match prop {
        RDFS_LABEL => {
            // Track language-neutral labels for the `! <label>` comment resolution.
            let lang = match &ann.av {
                horned_owl::model::AnnotationValue::Literal(
                    horned_owl::model::Literal::Language { lang, .. },
                ) => Some(lang.clone()),
                _ => None,
            };
            if lang.is_none() {
                e.label_no_lang.insert(val.clone());
            }
            e.label_axioms.push((val.clone(), lang, axanns.clone()));
            if let Some(old) = e.name.replace((val, axanns.clone())) {
                e.extra_names.push(old);
            }
        }
        IAO_DEF => {
            if let Some(old) = e.def.replace((val, axanns.clone())) {
                e.extra_defs.push(old);
            }
        }
        RDFS_COMMENT => e.comments.push((val, axanns.clone())),
        OWL_DEPRECATED_W => {
            if val == "true" {
                e.deprecated = true;
                e.deprecated_anns = axanns.clone();
            }
        }
        IAO_REPLACED_BY_W => e.replaced_by.push(val),
        _ => match oio {
            Some("id") => e.id = Some(val),
            Some("hasExactSynonym") => e.synonyms.push(("EXACT".into(), val, val_lang.clone(), axanns.clone())),
            Some("hasNarrowSynonym") => e.synonyms.push(("NARROW".into(), val, val_lang.clone(), axanns.clone())),
            Some("hasBroadSynonym") => e.synonyms.push(("BROAD".into(), val, val_lang.clone(), axanns.clone())),
            Some("hasRelatedSynonym") => e.synonyms.push(("RELATED".into(), val, val_lang.clone(), axanns.clone())),
            Some("hasDbXref") => e.xrefs.push((val, axanns.clone())),
            Some("hasOBONamespace") => {
                // A term/typedef may carry several `hasOBONamespace` values — e.g.
                // BSPO's `ventral_to` keeps both "spatial" and the base-merge
                // default "uberon", or CHEBI's carbon monoxide "chebi_ontology" and
                // "protein". Every value that differs from the header default is
                // emitted (see the emit site), so keep them all.
                if !e.namespace.iter().any(|(ns, anns)| *ns == val && anns == axanns) {
                    e.namespace.push((val, axanns.clone()));
                }
            }
            Some("inSubset") => {
                let (raw, _, _, riri) = av_lit_parts(&ann.av);
                e.subsets.push((val, raw, riri, axanns.clone()))
            }
            Some("hasAlternativeId") => e.alt_ids.push((val, axanns.clone())),
            Some("consider") => e.consider.push((val, axanns.clone())),
            Some("created_by") => e.created_by.push((val, axanns.clone())),
            Some("creation_date") => e.creation_date.push((val, axanns.clone())),
            Some("disjoint_over") => e.disjoint_over.push((val, axanns.clone())),
            Some("shorthand") => e.shorthand = Some(val),
            Some("is_metadata_tag") => {
                e.is_metadata_tag = val == "true";
                if val == "false" {
                    e.bool_tags.insert("is_metadata_tag", false);
                }
            }
            Some("is_class_level") => {
                e.is_class_level = val == "true";
                if val == "false" {
                    e.bool_tags.insert("is_class_level", false);
                }
            }
            Some("is_transitive") => e.transitive_anno = Some(val == "true"),
            Some("hasScope") if synonym_scope(&ann.av).is_some() => e.synonym_scope = synonym_scope(&ann.av),
            Some(local) if (val == "true" || val == "false") && boolean_tag(local).is_some() => {
                if let Some(tag) = boolean_tag(local) {
                    e.bool_tags.insert(tag, val == "true");
                }
            }
            Some(tag) if RELATION_TAGS.contains(&tag) => {
                let value = match &ann.av {
                    AnnotationValue::IRI(i) => ctx.id(i.as_ref()),
                    _ => val,
                };
                e.tag_values.push((tag.to_string(), value, axanns.clone()));
            }
            _ => match prop {
                // `IAO:0000231` on a deprecated class records the obsolescence
                // reason so `alt_id:` folding can recognise the "terms merged"
                // stubs — but the tag is NOT otherwise special-cased: it is still
                // emitted as a plain `property_value:` (627 of them in EFO, on
                // deprecated MONDO/EFO terms). A term whose reason makes it a
                // merged stub has its whole stanza dropped later, so emitting the
                // clause here is harmless there and correct everywhere else.
                IAO_OBSOLESCENCE_REASON => {
                    e.obsolescence_reason = Some(val.clone());
                    let (pv_val, val_iri) = match &ann.av {
                        AnnotationValue::IRI(i) => (ctx.curie(i.as_ref()), i.as_ref().to_string()),
                        _ => (val, String::new()),
                    };
                    e.property_values.push((ctx.id(prop), pv_val, is_iri, dt, axanns.clone(), prop.to_string(), val_iri));
                }
                IAO_ANTISYMMETRIC if val == "true" || val == "false" => {
                    e.bool_tags.insert("is_anti_symmetric", val == "true");
                }
                // The OBO macro tags are Typedef-only; on anything else they stay
                // ordinary property_values.
                IAO_EXPAND_EXPRESSION_TO => e.expand_expression_to.push((val, axanns.clone())),
                IAO_EXPAND_ASSERTION_TO => e.expand_assertion_to.push((val, axanns.clone())),
                _ if is_iri && ctx.metadata_tags.contains(prop) => {
                    let (val, val_iri) = match &ann.av {
                        AnnotationValue::IRI(i) => (ctx.curie(i.as_ref()), i.as_ref().to_string()),
                        _ => (val, String::new()),
                    };
                    // An annotation assertion with an IRI value on a [Term] is a
                    // `relationship:` iff the property is a metadata tag (it
                    // carries `oboInOwl:is_metadata_tag`), else a
                    // `property_value:`. In a [Typedef] it is always a
                    // `property_value:`. The subject's stanza type is only known at
                    // write time, so defer the routing. Note: a shorthand alone does
                    // NOT qualify — `rdfs:seeAlso` has a `seeAlso` shorthand but no
                    // `is_metadata_tag`, so it stays a `property_value:`.
                    e.rel_or_pv.push((ctx.id(prop), val, axanns.clone(), prop.to_string(), val_iri));
                }
                _ => {
                    // Everything else is an OBO `property_value:`. An IRI value is a
                    // plain CURIE, never a shorthand (`seeAlso UBPROP:0000113`, not
                    // `seeAlso dental_formula`).
                    let (val, val_iri) = match &ann.av {
                        AnnotationValue::IRI(i) => (ctx.curie(i.as_ref()), i.as_ref().to_string()),
                        _ => (val, String::new()),
                    };
                    e.property_values.push((ctx.id(prop), val, is_iri, dt, axanns.clone(), prop.to_string(), val_iri));
                }
            },
        },
    }
}

/// The OBO id of an IRI-valued `hasDbXref` inside a
/// `{xref="…"}` QUALIFIER. (A `def:`/`synonym:` bracket keeps the IRI whole:
/// ECTO's `def: "…" [https://en.wikipedia.org/wiki/Drop_%28liquid%29]`.)
///
/// Take the part after the last `/`; if that carries a `#`, the fragment is the
/// id; else if it splits on exactly one `_`, the two halves become `PREFIX:LOCAL`;
/// otherwise there is no id and the FULL IRI stands. That is what makes ECTO's
/// `ecto.obo` read
/// `{xref="Properties"}` for `…/wiki/Clay#Properties`,
/// `{xref="Shortwave:radiation"}` for `…/wiki/Shortwave_radiation`, and
/// `{xref="https://orcid.org/0000-0003-4808-4736"}` — unshortened — for an ORCID,
/// whose last segment has neither.
fn xref_identifier(iri: &str) -> String {
    let last = iri.rsplit('/').next().unwrap_or(iri);
    if let Some((_, frag)) = last.split_once('#') {
        return frag.to_string();
    }
    let parts: Vec<&str> = last.split('_').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        return format!("{}:{}", parts[0], parts[1]);
    }
    iri.to_string()
}

/// The part of an `xref:` clause's axiom annotations that distinguishes it from
/// another clause on the same value.
///
/// A literal `rdfs:label` on the axiom is the xref's own description: it is
/// consumed into the xref rather than becoming one of the clause's qualifiers, and
/// two xrefs are equal when their ids are, descriptions notwithstanding. So the
/// description is invisible to the redundancy check while every other qualifier is
/// not.
fn xref_dedup_anns(anns: &BTreeSet<Annotation<RcStr>>) -> BTreeSet<Annotation<RcStr>> {
    anns.iter()
        .filter(|a| {
            !(a.ap.0.as_ref() == "http://www.w3.org/2000/01/rdf-schema#label"
                && matches!(a.av, AnnotationValue::Literal(_)))
        })
        .cloned()
        .collect()
}

/// Render an annotation value: (obo-text, is_iri, datatype-curie).
/// An IRI annotation VALUE outside the obo PURL space, as an OBO clause value.
///
/// Three cases, in order: a non-empty FRAGMENT wins
/// (`…/issues/225#issuecomment-218584934` → `issuecomment-218584934`); else a last
/// path segment shaped like an OBO id becomes one
/// (`http://www.ebi.ac.uk/efo/EFO_0008992` → `EFO:0008992`); else the IRI is
/// written verbatim (`https://w3id.org/semapv/vocab/ManualMappingCuration`,
/// `https://ror.org/03cpe7c52`).
///
/// Deliberately pure string work: asking the render context to shorten an IRI
/// REGISTERS a generated prefix, which then earns an `idspace:` line the reference
/// does not write.
fn obo_iri_value(iri: &str) -> String {
    if let Some((_, frag)) = iri.rsplit_once('#') {
        if !frag.is_empty() {
            return frag.to_string();
        }
    }
    let last = iri.rsplit('/').next().unwrap_or(iri);
    if let Some((pfx, local)) = last.split_once('_') {
        let prefix_ok = !pfx.is_empty()
            && pfx.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && pfx.chars().all(|c| c.is_ascii_alphanumeric());
        if prefix_ok && !local.is_empty() && !local.contains('_') {
            return format!("{pfx}:{local}");
        }
    }
    iri.to_string()
}

fn ann_value_ctx(ctx: &Ctx, av: &AnnotationValue<RcStr>) -> (String, bool, Option<String>) {
    match av {
        AnnotationValue::Literal(lit) => match lit {
            Literal::Simple { literal } => (literal.clone(), false, None),
            Literal::Language { literal, .. } => (literal.clone(), false, None),
            Literal::Datatype { literal, datatype_iri } => {
                let dt = datatype_iri.as_ref();
                // xsd:string is the OBO default, and rdf:PlainLiteral reads as it;
                // omit both. Other XSD datatypes render as `xsd:NAME`, and any other
                // datatype as its full IRI.
                let dt = if dt == "http://www.w3.org/2001/XMLSchema#string"
                    || dt == "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral"
                {
                    None
                } else if let Some(local) = dt.strip_prefix("http://www.w3.org/2001/XMLSchema#") {
                    Some(format!("xsd:{local}"))
                } else {
                    Some(dt.to_string())
                };
                (literal.clone(), false, dt)
            }
        },
        AnnotationValue::IRI(i) => (ctx.id(i.as_ref()), true, None),
        AnnotationValue::AnonymousIndividual(a) => (anon_node_id(a.0.as_ref()), false, None),
    }
}

/// An anonymous individual as a clause value: its node id, `_:genid2147483648`.
fn anon_node_id(label: &str) -> String {
    if label.starts_with("_:") {
        label.to_string()
    } else {
        format!("_:{label}")
    }
}

/// The canonical OBO tag for an annotation property used as a qualifier key.
/// `None` means the property has no canonical tag and falls back to its bare
/// `oboInOwl#` local name or a CURIE. Entries whose tag equals the `oboInOwl#`
/// local name (`consider`, `created_by`, `id`, `shorthand`, `treat-xrefs-*` …) are
/// omitted: the local-name fallback already yields them.
fn qualifier_key_tag(p: &str) -> Option<&'static str> {
    match p {
        "http://purl.obolibrary.org/obo/IAO_0000115" => Some("def"),
        "http://purl.obolibrary.org/obo/IAO_0000424" => Some("expand_expression_to"),
        "http://purl.obolibrary.org/obo/IAO_0000425" => Some("expand_assertion_to"),
        "http://purl.obolibrary.org/obo/IAO_0000427" => Some("is_anti_symmetric"),
        "http://purl.obolibrary.org/obo/IAO_0100001" => Some("replaced_by"),
        "http://www.w3.org/2000/01/rdf-schema#comment" => Some("comment"),
        "http://www.w3.org/2000/01/rdf-schema#label" => Some("name"),
        "http://www.w3.org/2002/07/owl#deprecated" => Some("is_obsolete"),
        _ => p.strip_prefix(OIO).and_then(|l| match l {
            "NamespaceIdRule" => Some("namespace-id-rule"),
            "SubsetProperty" => Some("subsetdef"),
            "SynonymTypeProperty" => Some("synonymtypedef"),
            "hasAlternativeId" => Some("alt_id"),
            "hasBroadSynonym" => Some("BROAD"),
            "hasDbXref" => Some("xref"),
            "hasExactSynonym" => Some("EXACT"),
            "hasNarrowSynonym" => Some("NARROW"),
            "hasOBOFormatVersion" => Some("format-version"),
            "hasOBONamespace" => Some("namespace"),
            "hasRelatedSynonym" => Some("RELATED"),
            "hasScope" => Some("scope"),
            "hasSynonymType" => Some("has_synonym_type"),
            "inSubset" => Some("subset"),
            _ => None,
        }),
    }
}

/// Decompose an axiom-annotation set into the OBO pieces a stanza line carries:
/// the `[dbxref…]` list, an optional synonym-type id, and the residual
/// `{key=value}` qualifiers.
fn ax_ann_pieces(
    ctx: &Ctx,
    anns: &BTreeSet<Annotation<RcStr>>,
) -> (Vec<(String, bool)>, Option<String>, Vec<Qual>) {
    // Each dbxref keeps its value-is-IRI flag: a `hasDbXref` whose value is an IRI
    // (`<https://doi.org/…>`) hashes as an IRI in the `{xref=…}` block order, a
    // literal (`"PMID:…"`) as a literal — CL:4033035's mixed pair depends on it.
    let mut dbxrefs: Vec<(String, bool)> = Vec::new();
    let mut syn_type = None;
    // (annotation property IRI, rendered qualifier name, value, value-is-IRI).
    // The IRI and is-IRI flag feed the hash used to reorder the block.
    let mut quals: Vec<Qual> = Vec::new();
    for a in anns {
        let p = a.ap.0.as_ref();
        // An IRI value that is NOT an OBO PURL is written verbatim, and asking the
        // context to shorten it would REGISTER a generated prefix — which then earns
        // an `idspace:` line the reference does not have (`vocab` for
        // `https://w3id.org/semapv/vocab/`). So the shortening is only attempted for
        // the obo PURL space, where the result is a real OBO id.
        let (val, is_iri, _dt) = match &a.av {
            AnnotationValue::IRI(i) if !i.as_ref().starts_with(OBO_BASE) => {
                // Outside the obo PURL space the value is a CURIE against whichever
                // namespace the document DECLARES — `vocab:ManualMappingCuration`
                // where the header binds `vocab:` to `https://w3id.org/semapv/vocab/`.
                // With no declared namespace covering it the IRI reduces to its
                // FRAGMENT when it has one (`…/issues/225#issuecomment-218584934` →
                // `issuecomment-218584934`) and is written verbatim when it has none.
                let v = ctx
                    .declared_curie(i.as_ref())
                    .unwrap_or_else(|| obo_iri_value(i.as_ref()));
                (v, true, None)
            }
            _ => ann_value_ctx(ctx, &a.av),
        };
        match p {
            _ if p == format!("{OIO}hasDbXref") => {
                // The raw IRI. A `def:`/`synonym:` bracket keeps it verbatim; only
                // the `{xref="…"}` QUALIFIER form is reduced to an OBO id — see
                // `xref_identifier`.
                let xv = match &a.av {
                    AnnotationValue::IRI(i) => i.as_ref().to_string(),
                    _ => val,
                };
                dbxrefs.push((xv, is_iri));
            }
            _ if p == format!("{OIO}hasSynonymType") => syn_type = Some(val),
            // `oboInOwl:all_only` is an internal marker for an all-some
            // (`∀R.F ⊓ ∃R.F`) translation, not an OBO qualifier — it is never
            // written out as `{all_only="true"}`, so skip it.
            _ if p == format!("{OIO}all_only") => {}
            _ => {
                // The remaining annotation properties become `{key="value"}`
                // qualifiers under the canonical OBO tag for them. `rdfs:label`
                // is `name` (CL's CELLxGENE `seeAlso` links all carry one) and
                // `oboInOwl:SynonymTypeProperty` — the *other* spelling of a
                // synonym type, used by CL's `hasRelatedSynonym` axioms — is
                // `synonymtypedef`, not its bare local name.
                // The qualifier key is a canonical
                // OBO tag when the property has one (`def`, `xref`, `scope`,
                // the synonym scopes …), else the bare `oboInOwl#` local name
                // (`source`, `notes`, `created_by` …), else a plain CURIE — never an
                // `oboInOwl:shorthand` (so `editor_note` → `IAO:0000116`,
                // `dc-contributor` → `terms:contributor`).
                let key = if let Some(tag) = qualifier_key_tag(p) {
                    tag.to_string()
                } else if p == RDFS_SEEALSO {
                    "seeAlso".to_string()
                } else if let Some(local) = p.strip_prefix(OIO) {
                    local.to_string()
                } else {
                    ctx.curie(p)
                };
                let (raw, dtf, langf, _) = av_lit_parts(&a.av);
                quals.push((p.to_string(), key, val, is_iri, dtf, langf, raw));
            }
        }
    }
    dbxrefs.sort_by(|a, b| fold(&a.0).cmp(&fold(&b.0)));
    // Default (`property_value`, `relationship`, `is_a`, `intersection_of`) order:
    // these qualifiers come straight from the axiom's *sorted* annotation stream,
    // so ascending by property IRI then value. The xref/comment/def/synonym/subset
    // tags override this with `owlapi_hashset_order`, because their qualifiers pass
    // through a hash set on the way out.
    quals.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    (dbxrefs, syn_type, quals)
}

/// The dbxrefs of an annotation set rendered as `{xref="…"}` qualifiers. Only
/// `def:` and `synonym:` have the bracket-list syntax; every other tag
/// (comment, relationship, property_value, …) carries its provenance as an
/// `xref` qualifier, as in CL's `comment: … {xref="PMID:26106328"}`.
/// One `{name="value"}` qualifier: (annotation property IRI, rendered qualifier
/// name, value, value-is-IRI). The IRI and is-IRI flag exist only to reconstruct
/// the annotation hash; [`plain`] drops them just before rendering.
/// One `{key="value"}` qualifier. The trailing fields exist only to rebuild the
/// annotation hash that decides the block's order; [`plain`] drops
/// them just before rendering. The RAW value is kept alongside the rendered one
/// because the hash is over the full IRI, not the CURIE the qualifier prints
/// (`https://w3id.org/semapv/vocab/LexicalMatching`, not `vocab:LexicalMatching`).
type Qual = (String, String, String, bool, Option<String>, Option<String>, String);

/// Drop the hash-only fields, leaving what `render_quals` prints.
fn plain(quals: &[Qual]) -> Vec<(String, String)> {
    quals.iter().map(|(_, k, v, _, _, _, _)| (k.clone(), v.clone())).collect()
}

// --- Qualifier block ordering --------------------------------------------------
//
// A `{…}` qualifier block comes out of a hash set with no sort applied, so its
// order is hash-bucket order — a pure function of each annotation's hash. The OBO
// spec does not define that order, but released files such as CL's `cl.obo` carry
// it, and a `.obo` whose blocks come out in any other order rewrites lines whose
// content never changed. Reproducing it means reproducing the hash and the bucket
// walk:
//
//   IRI hash               = jhash(namespace) + jhash(remainder)     (NCName split)
//   annotation property    = IRI hash + 188077
//   literal(xsd:string)    = 3231644899 + jhash(value) * 65536
//   annotation             = 31*property hash + value hash + 6064871
//
// where jhash is the 31-multiplier string hash over UTF-16 units. Every seed here
// is part of the on-disk order, so changing one changes every release diff.

/// The 31-multiplier string hash: `s[0]*31^(n-1) + … + s[n-1]`, over UTF-16 code
/// units, wrapping in `i32`.
pub(crate) fn java_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

fn is_xml_name_start(c: u32) -> bool {
    c == b':' as u32
        || (b'A' as u32..=b'Z' as u32).contains(&c)
        || c == b'_' as u32
        || (b'a' as u32..=b'z' as u32).contains(&c)
        || (0xC0..=0xD6).contains(&c)
        || (0xD8..=0xF6).contains(&c)
        || (0xF8..=0x2FF).contains(&c)
        || (0x370..=0x37D).contains(&c)
        || (0x37F..=0x1FFF).contains(&c)
        || (0x200C..=0x200D).contains(&c)
        || (0x2070..=0x218F).contains(&c)
        || (0x2C00..=0x2FEF).contains(&c)
        || (0x3001..=0xD7FF).contains(&c)
        || (0xF900..=0xFDCF).contains(&c)
        || (0xFDF0..=0xFFFD).contains(&c)
        || (0x10000..=0xEFFFF).contains(&c)
}

fn is_xml_name_char(c: u32) -> bool {
    is_xml_name_start(c)
        || c == b'-' as u32
        || c == b'.' as u32
        || (b'0' as u32..=b'9' as u32).contains(&c)
        || c == 0xB7
        || (0x0300..=0x036F).contains(&c)
        || (0x203F..=0x2040).contains(&c)
}

/// The byte index where the local part
/// (NCName suffix) begins, or `None` when the string has no such suffix (the
/// whole string is then the namespace). `:` is not an NCName char.
pub(crate) fn ncname_suffix_index(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() > 1 && b[0] == b'_' && b[1] == b':' {
        return None;
    }
    let mut index = None;
    for (i, ch) in s.char_indices().rev() {
        let cp = ch as u32;
        if cp != ':' as u32 && is_xml_name_start(cp) {
            index = Some(i);
        }
        if !(cp != ':' as u32 && is_xml_name_char(cp)) {
            break;
        }
    }
    index
}

/// The IRI hash: `hash(namespace) + hash(remainder)`, split at the NCName suffix.
pub(crate) fn owlapi_iri_hash(iri: &str) -> i32 {
    match ncname_suffix_index(iri) {
        Some(i) => java_hash(&iri[..i]).wrapping_add(java_hash(&iri[i..])),
        None => java_hash(iri),
    }
}

/// The literal hash for a plain `xsd:string` literal (no language tag) — the only
/// literal kind CL puts in a qualifier.
fn owlapi_literal_hash(value: &str) -> i32 {
    (3231644899u32 as i32).wrapping_add(java_hash(value).wrapping_mul(65536))
}

const XSD_STRING_IRI: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_PLAIN_LITERAL_IRI: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// The literal hash for any literal. `xsd:string` and `rdf:langString` normalize to
/// `rdf:PlainLiteral` before hashing, so a plain or language literal keeps the
/// `owlapi_literal_hash` base; a typed literal shifts the base by its datatype's
/// IRI-hash difference from `rdf:PlainLiteral`; a language tag then folds in as
/// `base*37 + hash(lang)`.
fn owlapi_lit_hash(value: &str, datatype: Option<&str>, lang: Option<&str>) -> i32 {
    // The datatype the literal hashes UNDER, which also decides how the value
    // itself contributes: a typed number contributes the number, so
    // `"20"^^xsd:integer` and `"20"^^xsd:string` do not hash alike.
    let dt = match datatype {
        Some(d) if d != XSD_STRING_IRI => d,
        _ => RDF_PLAIN_LITERAL_IRI,
    };
    let jv = crate::owlapi_hash::literal_payload_hash(value, dt);
    let d = owlapi_iri_hash(dt)
        .wrapping_sub(owlapi_iri_hash(RDF_PLAIN_LITERAL_IRI))
        .wrapping_mul(37);
    let base = (3231644899u32 as i32).wrapping_add(d).wrapping_add(jv);
    match lang {
        Some(l) => base.wrapping_mul(37).wrapping_add(java_hash(l)),
        None => base,
    }
}

/// The `(value, datatype, language)` the literal hash needs from an annotation
/// value; an IRI value returns its IRI in `value` with `is_iri` true.
pub(crate) fn av_lit_parts(av: &AnnotationValue<RcStr>) -> (String, Option<String>, Option<String>, bool) {
    match av {
        AnnotationValue::IRI(i) => (i.as_ref().to_string(), None, None, true),
        AnnotationValue::Literal(Literal::Simple { literal }) => (literal.clone(), None, None, false),
        AnnotationValue::Literal(Literal::Language { literal, lang }) => {
            (literal.clone(), None, Some(lang.clone()), false)
        }
        AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri }) => {
            (literal.clone(), Some(datatype_iri.as_ref().to_string()), None, false)
        }
        _ => (String::new(), None, None, false),
    }
}

/// The hash of an annotation of `prop_iri` whose value is an IRI (`is_iri`) or a
/// literal with the given datatype and language.
pub(crate) fn owlapi_annotation_hash_full(
    prop_iri: &str,
    value: &str,
    datatype: Option<&str>,
    lang: Option<&str>,
    is_iri: bool,
) -> i32 {
    let prop = owlapi_iri_hash(prop_iri).wrapping_add(188077);
    let val = if is_iri {
        owlapi_iri_hash(value)
    } else {
        owlapi_lit_hash(value, datatype, lang)
    };
    31i32.wrapping_mul(prop).wrapping_add(val).wrapping_add(6064871)
}

/// The hash of an annotation-assertion axiom: seed 739, then
/// `hash = 31*hash + component` over subject IRI, annotation property, value, and
/// the annotation-collection hash. A subject's assertion axioms sit in a hash
/// table, and their OBO clauses are written in this hash's bucket order (see
/// [`owlapi_aa_bucket`]).
fn owlapi_aa_axiom_hash(
    subj_iri: &str,
    prop_iri: &str,
    value: &str,
    val_is_iri: bool,
    coll_hash: i32,
) -> i32 {
    owlapi_aa_axiom_hash_full(subj_iri, prop_iri, value, None, None, val_is_iri, coll_hash)
}

/// [`owlapi_aa_axiom_hash`] with the value's datatype/language, so a typed
/// (xsd:decimal) or language-tagged main value hashes exactly.
#[allow(clippy::too_many_arguments)]
fn owlapi_aa_axiom_hash_full(
    subj_iri: &str,
    prop_iri: &str,
    value: &str,
    datatype: Option<&str>,
    lang: Option<&str>,
    val_is_iri: bool,
    coll_hash: i32,
) -> i32 {
    let mut h: i32 = 739;
    h = h.wrapping_mul(31).wrapping_add(owlapi_iri_hash(subj_iri));
    h = h
        .wrapping_mul(31)
        .wrapping_add(owlapi_iri_hash(prop_iri).wrapping_add(188077));
    let vh = if val_is_iri {
        owlapi_iri_hash(value)
    } else {
        owlapi_lit_hash(value, datatype, lang)
    };
    h = h.wrapping_mul(31).wrapping_add(vh);
    h = h.wrapping_mul(31).wrapping_add(coll_hash);
    h
}

/// The hash bucket of one `rdfs:label` annotation-assertion axiom, for a
/// table of `cap` — the axiom hash (subject IRI, `rdfs:label`, the literal
/// value hash for its plain/language kind, then its annotation-collection hash)
/// spread into `cap` buckets.
fn owlapi_label_bucket(subj: &str, value: &str, lang: Option<&str>, coll: i32, cap: usize) -> usize {
    owlapi_aa_bucket(owlapi_label_axiom_hash(subj, value, lang, coll), cap)
}

/// The raw hash of an `rdfs:label` annotation-assertion axiom, before
/// it is spread into a bucket. Split out of [`owlapi_label_bucket`] so a tie inside
/// one bucket can be broken on the finer value.
fn owlapi_label_axiom_hash(subj: &str, value: &str, lang: Option<&str>, coll: i32) -> i32 {
    let mut h: i32 = 739;
    h = h.wrapping_mul(31).wrapping_add(owlapi_iri_hash(subj));
    h = h
        .wrapping_mul(31)
        .wrapping_add(owlapi_iri_hash(RDFS_LABEL).wrapping_add(188077));
    let vh = match lang {
        None => owlapi_literal_hash(value),
        Some(l) => owlapi_literal_hash(value).wrapping_mul(37).wrapping_add(java_hash(l)),
    };
    h = h.wrapping_mul(31).wrapping_add(vh);
    h = h.wrapping_mul(31).wrapping_add(coll);
    h
}

/// The display name for a subject's `! label` comments: the `rdfs:label` whose
/// axiom lands in the minimum bucket of the hash table holding all of the
/// subject's annotation-assertion axioms.
///
/// The bucket pick is applied only when it is unambiguous — every label axiom has
/// no annotations AND the labels fall in distinct buckets (a within-bucket tie is decided by
/// insertion order, which an unordered model cannot recover). That settles the
/// clean multi-label cases (OBI:0000295, PR:000003918, part_of). Otherwise — the
/// multilingual terms with `{source}`-annotated labels (GSSO) whose buckets collide
/// — the tie falls to the un-annotated label, and failing that to the
/// fold-maximum.
fn pick_comment_name(ctx: &Ctx, subj_iri: &str, sd: &SubjData) -> Option<String> {
    if sd.label_axioms.is_empty() {
        return None;
    }
    {
        let cap = owlapi_set_cap(sd.ann_count.max(1));
        let buckets: Vec<usize> = sd
            .label_axioms
            .iter()
            .map(|(v, lang, anns)| {
                owlapi_label_bucket(subj_iri, v, lang.as_deref(), axiom_annotations_hash(anns, ctx.order), cap)
            })
            .collect();
        if std::env::var("OM_LABEL_DEBUG").is_ok() {
            let rows: Vec<String> = sd
                .label_axioms
                .iter()
                .zip(buckets.iter())
                .map(|((v, lang, anns), b)| {
                    let h = owlapi_label_axiom_hash(
                        subj_iri,
                        v,
                        lang.as_deref(),
                        axiom_annotations_hash(anns, ctx.order),
                    );
                    format!(
                        "{b}\u{1}{h}\u{1}{}\u{1}{}\u{1}{}",
                        lang.clone().unwrap_or_default(),
                        anns.len(),
                        v
                    )
                })
                .collect();
            eprintln!(
                "[label]\t{subj_iri}\t{}\t{cap}\t{}",
                sd.ann_count,
                rows.join("\u{2}")
            );
        }
        // The subject's set is filled from the document-wide set of every
        // annotation assertion, so two labels in one bucket of the subject's
        // table stand in that larger table's order.
        let all = AA_ALL_CAP.with(|c| c.get()).max(16);
        let keys: Vec<(usize, usize)> = sd
            .label_axioms
            .iter()
            .zip(buckets.iter())
            .map(|((v, lang, anns), b)| {
                let h = owlapi_label_axiom_hash(subj_iri, v, lang.as_deref(), axiom_annotations_hash(anns, ctx.order));
                (*b, owlapi_aa_bucket(h, all))
            })
            .collect();
        if let Some(min) = keys.iter().min() {
            if keys.iter().filter(|k| *k == min).count() == 1 {
                let i = keys.iter().position(|k| k == min).unwrap();
                return Some(sd.label_axioms[i].0.clone());
            }
        }
        let mut sorted = buckets.clone();
        sorted.sort_unstable();
        sorted.dedup();
        // Unambiguous only: distinct buckets (a same-bucket tie would need the
        // unrecoverable parse-insertion order).
        if sorted.len() == buckets.len() {
            let (i, _) = buckets.iter().enumerate().min_by_key(|(_, b)| **b).unwrap();
            return Some(sd.label_axioms[i].0.clone());
        }
        // Two labels in the SAME bucket. Colliding entries chain in insertion order,
        // and in RDF/XML the plain assertion is parsed inside the class element while an
        // ANNOTATED one is only created once its `owl:Axiom` block is read — so the
        // un-annotated label heads the chain and wins.
        //
        // That is the mechanically justified choice, and it is NOT reliable: over
        // 300 real translated MONDO classes (782 `! …` comments) every non-tied case
        // is correct and only the 13 ties err — 6 of them under this rule, and the
        // OPPOSITE rule ("prefer annotated") gets those 6 right and the other 6
        // wrong, with zero overlap. So neither is the rule; a tie is decided by the
        // true insertion order of ALL the subject's assertions into the chain, which
        // an unordered model cannot reconstruct. Closing the last ~1,286 lines of
        // `mondo-international.obo` means recording per-subject label order at read
        // time, the way `owl_label_order` already records label order.
        // `OM_LABEL_DEBUG=1` prints ann_count / cap / buckets per subject.
        let lo = *sorted.first().unwrap();
        let tied: Vec<usize> = (0..buckets.len()).filter(|&i| buckets[i] == lo).collect();
        if tied.len() > 1 {
            // Colliding assertions chain in the order they were read, and the
            // display name is overwritten as the chain is walked. The source
            // document's order is the record of that; `RO_0002314`'s two labels
            // collide in bucket 14 of a 16-slot table and EFO 3.93's comments take
            // "inheres in part of", written second, over "characteristic of part
            // of".
            if let Some(order) = ctx.label_order.get(subj_iri) {
                let first = tied
                    .iter()
                    .copied()
                    .max_by_key(|&i| {
                        order
                            .iter()
                            .position(|v| *v == sd.label_axioms[i].0)
                            .unwrap_or(usize::MAX)
                    })
                    .unwrap();
                if order.iter().any(|v| *v == sd.label_axioms[first].0) {
                    return Some(sd.label_axioms[first].0.clone());
                }
            }
            let bare: Vec<usize> = tied
                .iter()
                .copied()
                .filter(|&i| sd.label_axioms[i].2.is_empty())
                .collect();
            let cands = if bare.is_empty() { tied } else { bare };
            if cands.len() == 1 {
                return Some(sd.label_axioms[cands[0]].0.clone());
            }
            // A model built in memory carries no document to have read an order
            // from. See the fold-maximum note below.
            return cands
                .iter()
                .map(|&i| &sd.label_axioms[i].0)
                .max_by(|a, b| fold(a).cmp(&fold(b)))
                .cloned();
        }
    }
    // Within-bucket tie: the LAST one wins. `RO_0004024` carries "disease causes
    // disruption of" and "disease disrupts", both landing in bucket 3 of a 16-slot
    // table, and the comment takes whichever is written SECOND — swapping the two
    // `AnnotationAssertion` lines in the input flips the answer. Colliding keys
    // chain in insertion order and the display name is overwritten as the chain is
    // walked, so the last one through wins.
    //
    // owlmake's model is an unordered set, so document order is not recoverable
    // here; the fold-MAXIMUM stands in for it, which is what both observed cases
    // resolve to — "disease disrupts" over "disease causes disruption of" for
    // `mondo.obo`, and the Japanese label over the English one for the ~1,288
    // `! …` comments in `mondo-international.obo` (CJK sorts above ASCII).
    // Preferring a language-NEUTRAL label instead is exactly backwards for the
    // translated MONDO products.
    sd.label_axioms
        .iter()
        .max_by(|a, b| fold(&a.0).cmp(&fold(&b.0)))
        .map(|(v, _, _)| v.clone())
}

/// The bucket an axiom hash falls in, for a table of `cap`
/// (a power of two): `spread(h) & (cap - 1)` with the spreader
/// `h ^ (h >>> 16)`. This is the position in the per-subject assertion-set
/// walk, so clauses left tied by the value comparison (same value,
/// differing only in qualifiers) come out in ascending bucket order.
fn owlapi_aa_bucket(hash: i32, cap: usize) -> usize {
    let h = hash as u32;
    let spread = h ^ (h >> 16);
    (spread as usize) & (cap - 1)
}

thread_local! {
    /// The table size of the set holding EVERY annotation assertion of the
    /// document being written; a subject's own set is filled from it.
    static AA_ALL_CAP: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Where an annotation assertion stands among its subject's: its bucket in
/// the subject's set, and within that bucket its place in the document-wide
/// set the subject's was filled from.
fn aa_set_key(hash: i32, cap: usize) -> u64 {
    let all = AA_ALL_CAP.with(|c| c.get()).max(16);
    ((owlapi_aa_bucket(hash, cap) as u64) << 32) | owlapi_aa_bucket(hash, all) as u64
}

/// Table size of the hash set after `n` incremental adds: start
/// at 16, double whenever the size exceeds 0.75·capacity.
pub(crate) fn owlapi_set_cap(n: usize) -> usize {
    let mut cap = 16usize;
    while n * 4 > cap * 3 {
        cap <<= 1;
    }
    cap
}

/// Reorder a qualifier list into hash-set iteration order for the annotations'
/// hashes. A synthetic qualifier (a writer-invented
/// `gci_*`/`all_some`/`cardinality`, marked by a `\u{FFFF}` sentinel in its IRI
/// slot) is not a real annotation; those are appended after, in name
/// order, since a clause never carries more than one and their order is moot.
fn owlapi_hashset_order(quals: Vec<Qual>) -> Vec<Qual> {
    let (mut synthetic, real): (Vec<Qual>, Vec<Qual>) =
        quals.into_iter().partition(|q| q.0.starts_with('\u{FFFF}'));
    synthetic.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));

    // Insertion order into the set is the list's own order, and that list is the
    // axiom's annotations sorted canonically — for a qualifier, property IRI then
    // value.
    let mut items: Vec<Qual> = real;
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));

    // A set built from a collection pre-sizes to hold it at load factor
    // 0.75, minimum table 16, rounded up to a power of two.
    let need = ((items.len() as f64 / 0.75) as usize + 1).max(16);
    let mut cap = 16usize;
    while cap < need {
        cap <<= 1;
    }
    let mut buckets: Vec<Vec<Qual>> = vec![Vec::new(); cap];
    for q in items {
        let h = owlapi_annotation_hash_full(&q.0, &q.6, q.4.as_deref(), q.5.as_deref(), q.3) as u32;
        let spread = h ^ (h >> 16);
        buckets[(spread as usize) & (cap - 1)].push(q);
    }
    let mut out: Vec<Qual> = buckets.into_iter().flatten().collect();
    out.append(&mut synthetic);
    out
}

/// Merge the leftover `hasDbXref` annotations in as `xref="…"` qualifiers. They
/// take their place by that property's IRI like any other, rather than being
/// appended and the whole block re-sorted by rendered name.
fn quals_with_xrefs(dbxrefs: &[(String, bool)], quals: &[Qual]) -> Vec<(String, String)> {
    let mut out: Vec<Qual> = quals.to_vec();
    for (x, is_iri) in dbxrefs {
        // A `{xref="…"}` qualifier carries the OBO IDENTIFIER of an IRI value, not
        // the IRI — unlike the `def:`/`synonym:` bracket, which keeps it whole.
        let x = if *is_iri { xref_identifier(x) } else { x.clone() };
        out.push((format!("{OIO}hasDbXref"), "xref".to_string(), x.clone(), *is_iri, None, None, x));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    plain(&out)
}

/// Like [`quals_with_xrefs`], but ordering the `{…}` block by hash bucket rather
/// than by name. Used by the tags whose qualifiers pass through a hash set on the
/// way out: `xref`, `comment`, `def`, `synonym`, `subset`.
fn quals_with_xrefs_hashset(dbxrefs: &[(String, bool)], quals: &[Qual]) -> Vec<(String, String)> {
    let mut out: Vec<Qual> = quals.to_vec();
    for (x, is_iri) in dbxrefs {
        // A `{xref="…"}` qualifier carries the OBO IDENTIFIER of an IRI value, not
        // the IRI — unlike the `def:`/`synonym:` bracket, which keeps it whole.
        let x = if *is_iri { xref_identifier(x) } else { x.clone() };
        out.push((format!("{OIO}hasDbXref"), "xref".to_string(), x.clone(), *is_iri, None, None, x));
    }
    plain(&owlapi_hashset_order(out))
}

/// Order a `relationship:`/`is_a:`/`intersection_of:` clause's `{…}` qualifiers
/// by hash bucket, keyed on the qualifier itself:
/// `hash = 31*(31 + hash(key)) + hash(value)`.
/// This is distinct from the annotation ordering used for def/synonym/xref
/// blocks: on a relationship the axiom annotations (e.g. `source`) and the
/// synthesised `gci_relation`/`gci_filler`/`cardinality` qualifiers share one
/// set keyed by that qualifier hash, which is what puts UBERON's
/// `{gci_relation=…, gci_filler=…, source=…}` in the order it has.
fn qualifier_value_hashset_order(pairs: &[(String, String)]) -> Vec<(String, String)> {
    // Within a bucket, *insertion* order is kept (new nodes append to the tail), and
    // `gci_relation` is inserted before `gci_filler`, so a
    // same-bucket collision resolves relation-first — UBERON's
    // `{gci_relation="part_of", gci_filler="NCBITaxon:9443"}` (both bucket 6). Keep
    // `pairs` in caller order; do NOT sort.
    let items: Vec<(String, String)> = pairs.to_vec();
    let need = ((items.len() as f64 / 0.75) as usize + 1).max(16);
    let mut cap = 16usize;
    while cap < need {
        cap <<= 1;
    }
    let mut buckets: Vec<Vec<(String, String)>> = vec![Vec::new(); cap];
    for (k, v) in items {
        let hc = 31i32
            .wrapping_mul(31i32.wrapping_add(java_hash(&k)))
            .wrapping_add(java_hash(&v)) as u32;
        let spread = hc ^ (hc >> 16);
        buckets[(spread as usize) & (cap - 1)].push((k, v));
    }
    buckets.into_iter().flatten().collect()
}

/// Assemble a relationship-style clause's qualifiers — axiom-annotation quals
/// (`quals`), leftover `hasDbXref` (`xref="…"`), and synthesised clause quals
/// (`extra`: `gci_relation`/`gci_filler`/`cardinality`/`all_only`) — and order
/// them with [`qualifier_value_hashset_order`].
fn quals_relationship(dbxrefs: &[(String, bool)], quals: &[Qual], extra: &[(String, String)]) -> Vec<(String, String)> {
    if extra.is_empty() {
        // No synthesised qualifier: the plain relationship/is_a path sorts
        // the axiom-annotation qualifiers (by property IRI, then value).
        return quals_with_xrefs(dbxrefs, quals);
    }
    // A synthesised qualifier (gci_relation/gci_filler, cardinality, all_only)
    // routes the clause through the General-Class-Inclusion path: the
    // synthetic qualifiers come first, ordered among themselves by the qualifier
    // hash, and the axiom-annotation qualifiers (source, xref,
    // …) follow in the plain sorted order.
    let mut out = qualifier_value_hashset_order(extra);
    out.extend(quals_with_xrefs(dbxrefs, quals));
    out
}

/// Append another clause's qualifiers `more` to `quals`, as a clause absorbs
/// the conjuncts it stands for: a minimum or maximum cardinality `quals`
/// already states keeps the lesser minimum and the greater maximum, and every
/// other qualifier is appended, repeats included.
fn absorb_quals(quals: &mut Vec<(String, String)>, more: Vec<(String, String)>) {
    for (k, v) in more {
        let bound = matches!(k.as_str(), "minCardinality" | "maxCardinality");
        match quals.iter_mut().find(|(q, _)| *q == k) {
            Some((_, have)) if bound => {
                if let (Ok(a), Ok(b)) = (have.parse::<i64>(), v.parse::<i64>()) {
                    *have = if k == "minCardinality" { a.min(b) } else { a.max(b) }.to_string();
                }
            }
            _ => quals.push((k, v)),
        }
    }
}

fn render_quals(quals: &[(String, String)]) -> String {
    if quals.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = quals.iter().map(|(k, v)| format!("{k}=\"{}\"", escape(v))).collect();
    format!(" {{{}}}", inner.join(", "))
}

fn render_bracket(dbxrefs: &[(String, bool)]) -> String {
    // Within a `[id, id, …]` list `,` separates ids, `]` ends the list, `"` begins
    // a per-id description and `:` splits idspace from local id, so a literal one
    // inside an id must be escaped — e.g. a URL
    // `…/cerebral_aneurysm_85\,P08772/` and the DOI `doi:10.1023/a\:1018564904170`.
    // Otherwise the round-trip re-parse would split the id on the comma.
    let escaped: Vec<String> = dbxrefs.iter().map(|(x, _)| escape_xref(x)).collect();
    format!("[{}]", escaped.join(", "))
}

/// A reference is annotated with the referent's label as a trailing
/// `! comment` — `is_a: CL:0000393 ! electrically responsive cell`. Every token of
/// the clause that has a label contributes (so a `relationship:` carries both the
/// relation's and the filler's, space-joined); tokens whose referent is unlabelled
/// or external contribute nothing, and if none is labelled there is no comment at
/// all. Only the *reference* tags get one: `xref:`, `subset:`, `alt_id:`,
/// `replaced_by:`, `consider:` and `holds_over_chain:` never do, as CL's `cl.obo`
/// shows.
/// [`label_comment`] for a clause written as `RELATION FILLER`. A relation given
/// by its OBO shorthand is already spelled as its own name, so it is not
/// repeated — `relationship: filtered_through UBERON:0000042 ! serous membrane`,
/// not `… ! filtered through serous membrane`. A relation given as a CURIE
/// (`BFO:0000050`) is labelled like anything else, and a shorthand in *value*
/// position still is (a [Typedef]'s `is_a: transitively_anteriorly_connected_to
/// ! transitively anteriorly connected to`), so this only applies to the head.
fn label_comment_pred(
    labels: &HashMap<String, String>,
    declared: &std::collections::HashSet<&str>,
    toks: &[&str],
) -> String {
    // The relation head contributes its own label to the `! comment` only when the
    // head's name resolves. It never does for a bare
    // shorthand (`part_of`, already spelled as its name), and for a CURIE it does
    // ONLY when the id is a mechanical OBO PURL (`RO:0000087` ⇐ obo/RO_0000087) —
    // NOT one shortened with a *declared* idspace prefix (`obo1:has_role` ⇐
    // obo#has_role, `efo:EFO_0000784` ⇐ .../efo/EFO_0000784), whose label is
    // omitted. `id_impl` only emits a declared-idspace prefix for a non-OBO-PURL
    // namespace, so "prefix is a declared idspace" is exactly "not an OBO PURL id".
    match toks.split_first() {
        // A single token is a value, not a relation head (an `intersection_of:`
        // genus, `efo:EFO_0000324 ! cell type`); label it like any reference.
        Some((_, [])) => label_comment(labels, toks),
        Some((rel, rest)) => {
            // ...and ALSO only when the relation's OBO id is CANONICAL: a mechanical
            // OBO PURL with an all-numeric local part (`BFO:0000050`, `RO:0000087`).
            // A letter-bearing local (`NCIT:R81`, `NCIT:C99999`) is not resolved as a
            // relation head even though it carries an rdfs:label:
            // `relationship: NCIT:R81 T:0000002 ! continuant` labels the target only.
            // (The filler is always name-resolved; this restriction is head-only.)
            let head_labelled = match rel.split_once(':') {
                None => false,
                Some((_p, local)) => {
                    // The all-numeric local part is the whole test. Every case the
                    // note above cites already fails on it — `obo1:has_role`,
                    // `efo:EFO_0000784`, `NCIT:R81` — so the extra "prefix is not a
                    // declared idspace" condition never decided any of them, and it
                    // is wrong where a declared idspace IS the entity's canonical OBO
                    // id: EFO declares `idspace: EFO http://www.ebi.ac.uk/efo/EFO_`,
                    // and `relationship: EFO:0000784 UBERON:0001004` must carry
                    // `! has_disease_location respiratory system`.
                    !local.is_empty() && local.bytes().all(|b| b.is_ascii_digit())
                }
            };
            if head_labelled {
                label_comment(labels, toks)
            } else {
                label_comment(labels, rest)
            }
        }
        None => String::new(),
    }
}

/// Shorten every `<IRI>` inside an OBO macro template to its local name, as
/// `expand_expression_to`/`expand_assertion_to` are written: the stored
/// literal holds full IRIs (`<…/obo/BFO_0000051> some (…)`) but the OBO clause
/// reads `BFO_0000051 some (…)`. Tokens already written bare (`part_of`) and all
/// whitespace, including embedded newlines, are left exactly as they are.
fn shorten_macro_iris(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('>') {
            Some(close) => {
                let iri = &after[..close];
                // Only OBO-PURL IRIs shorten to their bare local id;
                // a non-OBO IRI (owl:Nothing) is kept in full `<…>` form.
                if let Some(local) = iri.strip_prefix("http://purl.obolibrary.org/obo/") {
                    out.push_str(local);
                } else {
                    out.push('<');
                    out.push_str(iri);
                    out.push('>');
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn label_comment(labels: &HashMap<String, String>, toks: &[&str]) -> String {
    // A `! label` is appended for any referenced entity that has
    // one, keyed by its rendered id — including a full-IRI target such as
    // `http://identifiers.org/ncbigene/26468` (an `owl:Class` labelled "LHX6"). A
    // target with no label (an ORCID individual) simply isn't in `labels` and so
    // contributes nothing.
    let names: Vec<&str> = toks
        .iter()
        .filter_map(|t| labels.get(*t).map(|s| s.as_str()))
        .collect();
    if names.is_empty() {
        return String::new();
    }
    // The joined names are trimmed before the `" ! "` is prepended, so
    // leading/trailing whitespace in a LABEL never reaches the file. HPO's French
    // translations carry several — `"Mouvements involontaires "`,
    // `" Retard développemental…"` — and emitting them raw puts a stray space on 72
    // `is_a:` lines of `hp-fr.obo`. The trim strips code points <= U+0020, not
    // Unicode whitespace.
    let joined = names.join(" ");
    let trimmed = joined.trim_matches(|c: char| c <= '\u{20}');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(" ! {trimmed}")
    }
}

/// Emit the lines of one repeated tag, sorted the way OBO orders clauses:
/// case-insensitively on the clause *value*, before the `{qualifier}` block and
/// the `! label` comment are appended. The sort is stable, so equal keys keep the
/// (deterministic) axiom order they were collected in.
/// Like [`write_sorted`], but collapses clauses that render to the SAME line — two
/// synonym axioms differing only by a language tag (`"X"` and `"X"@en`) are one
/// OBO clause. Safe only where distinct clauses always render
/// distinctly (synonyms); a general dedup would swallow other tags' rendering.
fn write_sorted_dedup<W: Write>(writer: &mut W, tag: &str, mut lines: Vec<(String, String)>) -> Result<()> {
    lines.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut seen = std::collections::HashSet::new();
    for (_, line) in lines {
        if seen.insert(line.clone()) {
            writeln!(writer, "{tag}: {line}")?;
        }
    }
    Ok(())
}

fn write_sorted<W: Write>(writer: &mut W, tag: &str, mut lines: Vec<(String, String)>) -> Result<()> {
    // A tag's clauses sort case-INSENSITIVELY (the `a.0` key is case-folded), then
    // break ties case-SENSITIVELY via the rendered line, which leads with the clause
    // value — `"Acetylcholine"` before `"acetylcholine"`. A residue of
    // same-value/different-qualifier clauses (e.g. three
    // `xref: CAS:64-19-7 {source=…}`) is left in the order it arrived in; the tags
    // where that order is load-bearing fold the axiom-set bucket into `a.0`
    // themselves.
    lines.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (_, line) in lines {
        writeln!(writer, "{tag}: {line}")?;
    }
    Ok(())
}

fn write_stanza<W: Write>(
    out: &mut W,
    ctx: &Ctx,
    labels: &HashMap<String, String>,
    iri: &str,
    sd: Option<&SubjData>,
    kind: Stanza2,
    aa_cap: usize,
    subclass_cap: usize,
) -> Result<()> {
    let mut frame: Vec<u8> = Vec::new();
    let writer = &mut frame;
    let typedef = matches!(kind, Stanza2::ObjectProperty | Stanza2::AnnotationProperty);
    // The document's declared idspace prefixes — a relation head shortened with
    // one of these is a non-OBO-PURL CURIE whose label is omitted from a
    // `relationship:`/`intersection_of:` `! comment` (see `label_comment_pred`).
    let declared_prefixes: std::collections::HashSet<&str> =
        ctx.idspaces.iter().map(|(p, _)| p.as_str()).collect();
    let empty = SubjData::default();
    let sd = sd.unwrap_or(&empty);
    // The stanza id is derived from the IRI (plus any relation shorthand) — an
    // `oboInOwl:id` annotation is *not* authoritative and is ignored. CL's
    // cl-base.owl carries five stale `oboInOwl:id "CL:99xxxxx"` from temporary-id
    // terms whose IRIs were long since renumbered; honouring them renames those
    // five stanzas.
    let id = if kind == Stanza2::Instance { ctx.individual_id(iri) } else { ctx.id(iri) };
    let self_ref = id.clone();
    let _ = &sd.id;

    // --- The tags shared by [Term], [Typedef] and [Instance], in OBO's tag order. ---
    // A stanza with no `name:` is commented with the label its entity has in the
    // ontology's imports, the name its references are commented with.
    let named = sd.name.is_some() || !sd.extra_names.is_empty();
    match labels.get(&id).filter(|_| !named) {
        Some(label) => writeln!(writer, "id: {id} ! {label}")?,
        None => writeln!(writer, "id: {id}")?,
    }
    if let Some(v) = sd.bool_tags.get("is_anonymous") {
        writeln!(writer, "is_anonymous: {v}")?;
    }
    // One `name:` clause per distinct `rdfs:label`, ordered by case-folded value,
    // then case-sensitive value, then the axiom-set bucket for value-ties. A
    // single-label entity yields one line; a multi-label one (OBI:0000295's
    // `is_input_of` / `is specified input of`) yields one each, with any qualifiers.
    if sd.name.is_some() || !sd.extra_names.is_empty() {
        // Collect the distinct labels to emit as `name:` clauses — one per distinct
        // `rdfs:label`. A relation carrying both a readable label and its underscore
        // form (`part of` + `part_of`, `results in fission of` +
        // `results_in_fission_of`) yields both `name:` lines: a label equal to the
        // entity's shorthand is NOT suppressed. Labels that render to the same
        // clause (BFO:0000050's `"part of"` and `"part of"@en`) collapse to one via
        // the value+annotation dedup, while OBI:0000295's two genuinely distinct
        // labels stay two.
        let mut items: Vec<(&String, &BTreeSet<Annotation<RcStr>>)> = Vec::new();
        for (t, a) in sd.name.iter().chain(sd.extra_names.iter()) {
            if t.is_empty() {
                continue;
            }
            if items.iter().any(|(t2, a2)| *t2 == t && *a2 == a) {
                continue;
            }
            items.push((t, a));
        }
        // Every distinct label is a `name:` clause. `name:` is single-valued in the
        // OBO *structure*, but that is a cardinality rule the structure check
        // enforces by REFUSING the document, not by silently keeping one clause —
        // so an entity with two genuinely different labels writes two lines, and
        // GSSO:000413's three write three.
        write_sorted(writer, "name", items.into_iter().map(|(text, anns)| {
            let (dbxrefs, syn_type, mut quals) = ax_ann_pieces(ctx, anns);
            // On a `name:` clause, a `hasSynonymType` annotation is a qualifier
            // `{has_synonym_type="…"}`, not the type token a synonym clause uses.
            if let Some(st) = syn_type {
                quals.push((format!("{OIO}hasSynonymType"), "has_synonym_type".to_string(), st.clone(), false, None, None, st));
            }
            let coll = axiom_annotations_hash(anns, ctx.order);
            // The literal's LANGUAGE TAG is part of the axiom hash, so it has to be
            // recovered here — `sd.name`/`sd.extra_names` carry only the text.
            // Without it a `"X"@ja` label hashes as if it were plain, which lands it
            // in the wrong bucket and reverses the two `name:` lines whenever the
            // English and Japanese labels are the SAME STRING and the value
            // comparison therefore ties (MONDO:0019020 "PANDAS", MONDO:0018276
            // "CADDS", …).
            let lang = sd
                .label_axioms
                .iter()
                .find(|(v, _, a)| v == text && a == anns)
                .and_then(|(_, l, _)| l.clone());
            let bucket = aa_set_key(
                owlapi_label_axiom_hash(iri, text, lang.as_deref(), coll), aa_cap);
            let quals = quals_with_xrefs_hashset(&dbxrefs, &quals);
            (
                format!("{}\u{0}{}\u{1}{bucket:020}", fold(text), text),
                format!("{}{}", escape_name(text), render_quals(&quals)),
            )
        }).collect())?;
    }
    // A `namespace:` equal to the header `default-namespace` is suppressed (it is
    // implied); every other value is emitted, sorted by the clause comparison
    // (fold-min).
    let mut nss: Vec<&(String, BTreeSet<Annotation<RcStr>>)> = sd
        .namespace
        .iter()
        .filter(|(ns, _)| ctx.default_namespace.as_deref() != Some(ns.as_str()))
        .collect();
    nss.sort_by(|(a, _), (b, _)| fold(a).cmp(&fold(b)).then_with(|| a.cmp(b)));
    for (ns, anns) in nss {
        writeln!(writer, "namespace: {}{}", escape_name(ns), clause_quals(ctx, anns))?;
    }
    write_sorted(writer, "alt_id", sd.alt_ids.iter().map(|(a, anns)| (fold(a), format!("{a}{}", clause_quals(ctx, anns)))).collect())?;
    // One `def:` clause per distinct IAO:0000115 definition. A term
    // with several (e.g. EFO_0004253's two MeSH-sourced defs, or AfPO defs that share
    // text but differ in `def=` source) yields one line each, ordered by
    // case-folded value, then case-sensitive value, then the
    // axiom-set bucket order for value-tied clauses. An OBO structure
    // check rejects >1 def, but the EFO release converts with checking off, so
    // every distinct definition survives.
    if sd.def.is_some() || !sd.extra_defs.is_empty() {
        // Distinct clauses only: a `"…"` and `"…"@en` pair for the same definition
        // (OBI:0400103's "DNA sequencer" def) renders identically, so collapse to one;
        // AfPO's same-text/different-`def=`-source defs stay two.
        let mut ditems: Vec<(&String, &BTreeSet<Annotation<RcStr>>)> = Vec::new();
        for (t, a) in sd.def.iter().chain(sd.extra_defs.iter()) {
            if t.is_empty() || ditems.iter().any(|(t2, a2)| *t2 == t && *a2 == a) {
                continue;
            }
            ditems.push((t, a));
        }
        write_sorted(writer, "def", ditems.into_iter().map(|(text, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let coll = axiom_annotations_hash(anns, ctx.order);
            let bucket = aa_set_key(owlapi_aa_axiom_hash(iri, IAO_DEF, text, false, coll), aa_cap);
            (
                format!("{}\u{0}{}\u{1}{bucket:020}", fold(text), text),
                format!("\"{}\" {}{}", escape(text), render_bracket(&dbxrefs), render_quals(&plain(&owlapi_hashset_order(quals)))),
            )
        }).collect())?;
    }
    // One `comment:` line per `rdfs:comment` axiom, each carrying its
    // own `{xref=…}`, sorted case-insensitively — CL:4033035's three NS-forest notes
    // and CL:0000055's "define using PATO…" / "Redundant grouping term" come out as
    // separate lines, not one space-joined clause. Identical comments collapse.
    if !sd.comments.is_empty() {
        let mut items: Vec<&(String, BTreeSet<Annotation<RcStr>>)> =
            sd.comments.iter().filter(|(t, _)| !t.is_empty()).collect();
        items.sort_by(|a, b| fold(&a.0).cmp(&fold(&b.0)));
        items.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        for (text, anns) in items {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs_hashset(&dbxrefs, &quals);
            writeln!(writer, "comment: {}{}", escape_unquoted(text), render_quals(&quals))?;
        }
    }
    write_sorted(writer, "subset", sd.subsets.iter().map(|(s, raw, is_iri, anns)| {
        let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
        // Two `subset:` clauses naming the same subset tie on (tag, value), so
        // their order is the order the assertions were consumed — the
        // annotation-assertion bucket order `name:` and `xref:`
        // already use. EFO:0000218 carries `gard_rare` twice, with different
        // `{source=…}` qualifiers, and sorting on the name alone reverses them.
        let coll = axiom_annotations_hash(anns, ctx.order);
        let bucket = aa_set_key(
            owlapi_aa_axiom_hash(iri, &format!("{OIO}inSubset"), raw, *is_iri, coll), aa_cap);
        (
            format!("{}\u{1}{bucket:020}", fold(s)),
            format!("{s}{}", render_quals(&quals_with_xrefs_hashset(&dbxrefs, &quals))),
        )
    }).collect())?;
    write_sorted_dedup(writer, "synonym", sd.synonyms.iter().filter(|(_, t, _, _)| !t.is_empty()).map(|(scope, text, lang, anns)| {
        let (dbxrefs, syn_type, quals) = ax_ann_pieces(ctx, anns);
        let type_tok = syn_type.map(|t| format!("{t} ")).unwrap_or_default();
        // The sort key is the *unquoted* text: `"interstitial cell"` comes
        // before `"interstitial cell of Leydig"`, which comparing the rendered
        // (quoted) lines would reverse. Same-text/same-scope synonyms that differ
        // only in qualifiers tie; break the tie in the axiom-set bucket order
        // (see `owlapi_aa_bucket`).
        //
        // The bucket is the finest position a subject's assertion set records, so
        // two synonyms that share text, scope AND bucket have no order of their
        // own. `write_sorted_dedup` settles those on the rendered line, which
        // costs nothing in fidelity and keeps the file reproducible.
        let syn_prop = match scope.as_str() {
            "EXACT" => format!("{OIO}hasExactSynonym"),
            "NARROW" => format!("{OIO}hasNarrowSynonym"),
            "BROAD" => format!("{OIO}hasBroadSynonym"),
            _ => format!("{OIO}hasRelatedSynonym"),
        };
        let coll = axiom_annotations_hash(anns, ctx.order);
        let bucket = aa_set_key(
            owlapi_aa_axiom_hash_full(iri, &syn_prop, text, None, lang.as_deref(), false, coll), aa_cap);
        (
            format!("{}\u{0}{}\u{0}{}\u{1}{bucket:020}", fold(text), text, scope),
            format!("\"{}\" {} {}{}{}", escape(text), scope, type_tok, render_bracket(&dbxrefs), render_quals(&plain(&owlapi_hashset_order(quals)))),
        )
    }).collect())?;
    // `xref:` clauses that share a value merge, unioning their axiom annotations: a
    // plain `hasDbXref` (from the edit) and an annotated one (a mapping's
    // `{sssom:mapping_justification=…}`) for the same target collapse to one
    // qualified line rather than a bare plus a qualified line.
    //
    // Which of two duplicates survives is decided by the order the subject's
    // annotation assertions are walked in — bucket order, the same order the clause
    // sort keys below already use as a tie-break.
    let mut in_owlapi_order: Vec<(&String, &BTreeSet<Annotation<RcStr>>)> =
        sd.xrefs.iter().map(|(x, a)| (x, a)).collect();
    in_owlapi_order.sort_by_key(|(x, anns)| {
        let coll = axiom_annotations_hash(anns, ctx.order);
        let h = owlapi_aa_axiom_hash(iri, &format!("{OIO}hasDbXref"), x.trim(), false, coll);
        aa_set_key(h, aa_cap)
    });
    let mut merged_xrefs: Vec<(String, BTreeSet<Annotation<RcStr>>)> = Vec::new();
    for (x, anns) in in_owlapi_order {
        // Surrounding whitespace is trimmed off an xref value, so EFO's
        // ` CLO:0001200` (a stray leading space in the source hasDbXref) collapses
        // onto the clean `CLO:0001200` rather than emitting a second `xref:  …` line.
        let x = x.trim();
        // Two `hasDbXref` axioms with the SAME value but DIFFERENT axiom annotations
        // are two distinct `xref:` clauses — EFO's MedDRA:10002449 carries
        // {source="DOID:0111147"} on one and {source="ORDO:86886/e",
        // source="Orphanet:86886"} on another, and both are kept.
        //
        // The one qualifier that does NOT distinguish them is the xref's own
        // description. An `rdfs:label` on the xref axiom folds into the xref itself
        // rather than staying one of the clause's qualifiers, and two xrefs are
        // equal when their idrefs are — the description does not enter it. So the
        // redundancy check sees two equal clauses and drops the later one:
        // GO:0055085's two
        // `Reactome:R-HSA-382556` xrefs, described "ABC-family protein mediated
        // transport" and "ABC-family proteins mediated transport", write one line.
        // The survivor is the first in axiom order, keeping its own description.
        let key = xref_dedup_anns(anns);
        if !merged_xrefs.iter().any(|(v, a)| v == x && xref_dedup_anns(a) == key) {
            merged_xrefs.push((x.to_string(), anns.clone()));
        }
    }
    write_sorted(writer, "xref", merged_xrefs.iter().map(|(x, anns)| {
        let coll = axiom_annotations_hash(anns, ctx.order);
        let bucket = aa_set_key(owlapi_aa_axiom_hash(iri, &format!("{OIO}hasDbXref"), x, false, coll), aa_cap);
        let (dbxrefs, _, mut quals) = ax_ann_pieces(ctx, anns);
        // An xref value may carry a trailing quoted description in the OBO
        // `IDSPACE:LOCAL "description"` form — CHEBI stores the whole thing in one
        // `hasDbXref` literal (`Beilstein:147610 "Beilstein Registry Number"`). Split
        // it so the id is escaped as an id and the description spelled as the
        // trailing quoted string, not `\"…\"` inside the id.
        let (xid, embedded_desc) = xref_id_desc(x);
        // An `rdfs:label` on the xref axiom is likewise the xref's *description*,
        // spelled the same way (`xref: BAMS:1028 "MB"`), not a `{name="…"}` qualifier.
        let label_raw = quals
            .iter()
            .position(|(_, k, _, _, _, _, _)| k == "name")
            .map(|i| quals.remove(i).2)
            // An EMPTY description is no description: `xref: WB:rynl`, not
            // `xref: WB:rynl ""`. The qualifier is still consumed above, so it
            // does not fall through to the trailing `{…}` block either.
            .filter(|v| !v.is_empty());
        let label_tok = match &label_raw {
            Some(v) => format!(" \"{}\"", escape(v)),
            None => String::new(),
        };
        let desc_tok = match embedded_desc {
            Some(d) => format!(" \"{}\"", escape(d)),
            None => label_tok,
        };
        let quals = quals_with_xrefs_hashset(&dbxrefs, &quals);
        // An xref sorts on `idref + ' ' + description`, compared case-INsensitively
        // with a case-sensitive tie-break. The two ways an xref carries a
        // description end up on opposite sides of that space:
        //   * embedded in the `hasDbXref` literal (CHEBI's
        //     `Patent:DE2250327 "Patent"`) it is part of the IDREF, quotes and all,
        //     and the description slot is empty → `Patent:DE2250327 "Patent" null`;
        //   * from an `rdfs:label` on the xref axiom (HP's MedDRA pair) it IS the
        //     description → `MEDDRA:10038077 Rectal prolapse`.
        // So the described CHEBI clause sorts first ('"' < 'n') while the described
        // HP clause sorts second ('n' < 'r') — one rule, opposite outcomes. Keying on
        // the id alone with described-first gets CHEBI right and HP backwards.
        let id_part = match embedded_desc {
            Some(d) => format!("{xid} \"{d}\""),
            None => xid.to_string(),
        };
        let cmp = format!("{id_part} {}", label_raw.as_deref().unwrap_or("null"));
        (
            format!("{}\u{0}{}\u{1}{bucket:020}", fold(&cmp), cmp),
            format!("{}{desc_tok}{}", escape_xref(xid), render_quals(&quals)),
        )
    }).collect())?;
    // `builtin:` follows `xref:` in a [Term], and `holds_over_chain:` follows
    // it; a [Typedef] writes both after `range:`.
    if kind == Stanza2::Term {
        if let Some(v) = sd.bool_tags.get("builtin") {
            writeln!(writer, "builtin: {v}")?;
        }
        write_sorted(writer, "holds_over_chain", tag_value_clauses(ctx, labels, sd, "holds_over_chain"))?;
    }

    // `property_value:` sits after `xref:` in a [Typedef] but after
    // `relationship:` in a [Term] — the two frame types have different tag orders,
    // and CL's `cl.obo` shows both.
    // One `property_value:` clause, keyed and rendered. Both routes into the tag go
    // through this: the assertions that are always a `property_value:`, and — in a
    // [Typedef] — the metadata-tag IRI assertions that a [Term] would have made a
    // `relationship:`. A clause sorts against its neighbours by predicate then value,
    // so the two routes must produce the SAME key shape; a shorter one compares its
    // value against the other's predicate and orders the tag by neither.
    let pv_entry = |pred: &String,
                    val: &String,
                    is_iri: bool,
                    dt: &Option<String>,
                    anns: &BTreeSet<Annotation<RcStr>>,
                    prop_iri: &str,
                    val_iri: &str| {
        let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
        let quals = quals_with_xrefs(&dbxrefs, &quals);
        let line = if is_iri {
            format!("{pred} {val}{}", render_quals(&quals))
        } else {
            let dt_tok = dt.clone().unwrap_or_else(|| "xsd:string".into());
            format!(
                "{pred} {} {dt_tok}{}",
                pv_literal_token(val, ctx.owlapi_456),
                render_quals(&quals)
            )
        };
        // Key on predicate + raw (unquoted) value, so a literal and an IRI value
        // of the same property interleave in value order. Two clauses with the
        // same predicate AND value tie there — UBERON carries `oboInOwl:status
        // "Verified"` twice, once sourced to a ROR id and once to an ORCID — and
        // the tie goes to the axiom-set bucket order, not to whichever qualifier
        // sorts first.
        let coll = axiom_annotations_hash(anns, ctx.order);
        // The value's DATATYPE is part of the axiom hash. A `property_value:` is the
        // one clause whose main value is routinely typed — GSSO's Dewey numbers are
        // `xsd:decimal` — and hashing them as if they were plain puts three clauses
        // that differ only in a qualifier in the wrong order.
        let dt_iri = dt.as_deref().map(expand_datatype);
        // An IRI value hashes as its FULL IRI; `val` is only the CURIE the clause
        // prints, and hashing that puts every IRI-valued clause in the wrong bucket.
        let hash_val = if is_iri { val_iri } else { val.as_str() };
        let bucket = aa_set_key(
            owlapi_aa_axiom_hash_full(
                iri,
                prop_iri,
                hash_val,
                dt_iri.as_deref(),
                None,
                is_iri,
                coll,
            ), aa_cap);
        if std::env::var_os("OM_PV_DEBUG").is_some() {
            eprintln!(
                "[pv]\t{iri}\tcap={aa_cap}\tbucket={bucket}\thash={}\tcoll={coll}\tdt={:?}\t{line}",
                owlapi_aa_axiom_hash_full(iri, prop_iri, hash_val, dt_iri.as_deref(), None, is_iri, coll),
                dt_iri
            );
        }
        // Each of predicate and value compares case-INSENSITIVELY first and, when
        // that ties, case-SENSITIVELY — so `"Olof"` precedes `"olof"`. Only clauses
        // that tie on BOTH fall through to the axiom-set bucket.
        (
            format!(
                "{}\u{0}{pred}\u{0}{}\u{0}{val}\u{1}{bucket:020}",
                fold(pred),
                fold(val)
            ),
            line,
        )
    };
    let mut property_values: Vec<(String, String)> = sd
        .property_values
        .iter()
        .map(|(pred, val, is_iri, dt, anns, prop_iri, val_iri)| {
            pv_entry(pred, val, *is_iri, dt, anns, prop_iri, val_iri)
        })
        .collect();

    if kind == Stanza2::Term {
        // One `is_a` clause per SubClassOf AXIOM — there is no
        // folding on (parent, GCI). Only the annotations of a *single* axiom combine,
        // into that one clause's qualifier list. The four same-parent shapes:
        //
        //   plain + `{source="A"}`          -> two clauses (`{source="A"}`, then bare)
        //   `{source="A"}` + `{source="B"}` -> two clauses
        //   plain + `{is_inferred="true"}`  -> two clauses (is_inferred DOES print)
        //   one axiom, two `source` anns    -> ONE clause `{source="A", source="B"}`
        //
        // Folding these costs MONDO dearly: `remove` re-asserts the hierarchy as
        // plain axioms (see `span_gaps`), so every annotated `is_a:` legitimately
        // has an unannotated twin, and collapsing the pair drops 44,774 lines from
        // `filtered.obo`. If a *reduced* release shows a duplicate parent it should
        // not, the fault is `reduce` leaving a redundant axiom behind — fix it
        // there, not by rewriting the writer's rule.
        let is_a = &sd.is_a;
        write_sorted(writer, "is_a", is_a.iter().map(|(p, anns, gci, hash)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_relationship(&dbxrefs, &quals, gci);
            // The clause comparison keys only on the value (the parent id); the
            // `{gci_*}` qualifiers do not enter it, so same-parent clauses — plain and
            // GCI alike — tie and fall to the SubClassOf axiom-set bucket order.
            let bucket = owlapi_aa_bucket(*hash, subclass_cap);
            let key = format!("{}\u{0}{}\u{1}{bucket:020}", fold(p), p);
            (key, format!("{p}{}{}", render_quals(&quals), label_comment(labels, &[p])))
        }).chain(tag_value_clauses(ctx, labels, sd, "is_a")).collect())?;
        // A genus line (one token) always precedes the differentiae, which are then
        // sorted among themselves — the clause key leads with its argument count.
        // No dedup: clauses append unconditionally, so two DIFFERENT equivalence
        // axioms that share a genus really do write `intersection_of: EFO:0000408`
        // twice (eight EFO classes do). What prevents a spurious repeat is that the
        // axioms themselves live in a set, so one axiom can only ever contribute its
        // operands once.
        let ii: Vec<&(Vec<String>, Vec<(String, String)>, BTreeSet<Annotation<RcStr>>)> =
            sd.intersection_of.iter().collect();
        write_sorted(writer, "intersection_of", ii.into_iter().map(|(toks, extra, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_relationship(&dbxrefs, &quals, extra);
            let refs: Vec<&str> = toks.iter().map(|s| s.as_str()).collect();
            let value = toks.join(" ");
            (
                format!("{}\u{0}{}\u{0}{value}", toks.len(), fold(&value)),
                format!("{value}{}{}", render_quals(&quals), label_comment_pred(labels, &declared_prefixes, &refs)),
            )
        }).chain(tag_value_clauses(ctx, labels, sd, "intersection_of").into_iter().map(|(key, line)| (format!("1\u{0}{key}"), line))).collect())?;
        let union_of: Vec<(String, String)> = sd
            .union_of
            .iter()
            .filter(|_| sd.union_of.len() >= 2)
            .map(|u| (fold(u), format!("{u}{}", label_comment(labels, &[u]))))
            .chain(tag_value_clauses(ctx, labels, sd, "union_of"))
            .collect();
        write_sorted(writer, "union_of", union_of)?;
        write_sorted(writer, "equivalent_to", sd.equivalent_to.iter().map(|(e, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(e), format!("{e}{}{}", render_quals(&quals), label_comment(labels, &[e])))
        }).chain(tag_value_clauses(ctx, labels, sd, "equivalent_to")).collect())?;
        write_sorted(writer, "disjoint_from", sd.disjoint_from.iter().map(|(dj, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(dj), format!("{dj}{}{}", render_quals(&quals), label_comment(labels, &[dj])))
        }).chain(tag_value_clauses(ctx, labels, sd, "disjoint_from")).collect())?;
        // In a [Term], the deferred shorthand-IRI annotations are `relationship:`s.
        let mut all_rels: Vec<(String, String, BTreeSet<Annotation<RcStr>>, Vec<(String, String)>, i32)> =
            sd.relationships.clone();
        // These come from annotation assertions, not `SubClassOf`, so they carry no
        // subclass-axiom bucket; 0 is a stable placeholder (they do not tie on value).
        for (pred, val, anns, _, _) in &sd.rel_or_pv {
            all_rels.push((pred.clone(), val.clone(), anns.clone(), Vec::new(), 0));
        }
        // As with `is_a`, there is one `relationship:` clause per axiom — an
        // asserted `SubClassOf(X, R some Y)` and an annotated one over the same
        // (rel, target) are two clauses, not one folded clause: a plain and a
        // `{source="A"}` existential on the same rel+target render as two lines
        // (plain first here, the order falling out of the SubClassOf axiom-set
        // bucket). The pair only merges when the ontology goes through RDF/XML — see
        // the note on `is_a` above; the OBO writer is the wrong place for it. Folding
        // here costs MONDO's `filtered.obo` 13,763 `relationship:` lines, the
        // anonymous-superclass twins `span_gaps` adds.
        //
        // A cardinality/all_only restriction still keeps its own synthetic qualifier
        // and is its own clause anyway (`has_member Y` vs `has_member Y
        // {cardinality="1"}`, UBERON:0000170).
        let rels = &all_rels;
        write_sorted(writer, "relationship", rels.iter().map(|(r, t, anns, gci, hash)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let mut quals = quals_relationship(&dbxrefs, &quals, gci);
            for (anns, extra) in sd.absorbed.get(&(r.clone(), t.clone(), *hash)).into_iter().flatten() {
                let (dbxrefs, _, more) = ax_ann_pieces(ctx, anns);
                absorb_quals(&mut quals, quals_relationship(&dbxrefs, &more, extra));
            }
            // The clause comparison keys only on the value (`rel target`); the
            // `{gci_*}`/`{all_only}` qualifiers do not enter it, so same rel+target
            // clauses tie and break in the SubClassOf axiom-set bucket order.
            let bucket = owlapi_aa_bucket(*hash, subclass_cap);
            let key = format!("{}\u{0}{r} {t}\u{1}{bucket:020}", fold(&format!("{r} {t}")));
            (key, format!("{r} {t}{}{}", render_quals(&quals), label_comment_pred(labels, &declared_prefixes, &[r, t])))
        }).chain(tag_value_clauses(ctx, labels, sd, "relationship")).collect())?;
        write_sorted(writer, "property_value", property_values)?;
    } else if typedef {
        // In a [Typedef], the deferred shorthand-IRI annotations are `property_value:`s
        // — the same clause as any other, so they take the same key.
        for (pred, val, anns, prop_iri, val_iri) in &sd.rel_or_pv {
            property_values.push(pv_entry(pred, val, true, &None, anns, prop_iri, val_iri));
        }
        write_sorted(writer, "property_value", property_values)?;
        // Each relational tag's clauses come from the property's axioms and from
        // annotations of a property with that tag (`tag_value_clauses`).
        write_sorted(writer, "domain", sd.domain.iter().map(|(d, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(d), format!("{d}{}{}", render_quals(&quals), label_comment(labels, &[d])))
        }).chain(tag_value_clauses(ctx, labels, sd, "domain")).collect())?;
        write_sorted(writer, "range", sd.range.iter().map(|(r, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(r), format!("{r}{}{}", render_quals(&quals), label_comment(labels, &[r])))
        }).chain(tag_value_clauses(ctx, labels, sd, "range")).collect())?;
        if let Some(v) = sd.bool_tags.get("builtin") {
            writeln!(writer, "builtin: {v}")?;
        }
        // A two-link chain headed by the property itself is `transitive_over:`;
        // any other two-link chain is `holds_over_chain:`. Chains of three or more
        // links have no OBO tag at all and belong in `owl-axioms:`; emitting one as
        // `holds_over_chain:` would invent a tag the format does not have and change
        // the axiom's meaning on re-read.
        // One clause per distinct chain, merging the annotations of duplicate chain
        // axioms (a bare and a `{RO:0002582=…}`-annotated twin) into a single
        // qualifier block.
        let mut merged_chains: Vec<(Vec<String>, BTreeSet<Annotation<RcStr>>)> = Vec::new();
        for (links, anns) in &sd.chains {
            if let Some((_, existing)) = merged_chains.iter_mut().find(|(l, _)| l == links) {
                existing.extend(anns.iter().cloned());
            } else {
                merged_chains.push((links.clone(), anns.clone()));
            }
        }
        let mut chains: Vec<(String, String)> = Vec::new();
        let mut transitive_over: Vec<(String, String)> = Vec::new();
        for (chain, anns) in &merged_chains {
            if chain.len() != 2 {
                continue;
            }
            if chain[0] == self_ref || chain[0] == id {
                let t = &chain[1];
                transitive_over.push((fold(t), format!("{t}{}", label_comment(labels, &[t]))));
            } else {
                let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
                let quals = quals_with_xrefs(&dbxrefs, &quals);
                chains.push((fold(&chain.join(" ")), format!("{}{}", chain.join(" "), render_quals(&quals))));
            }
        }
        chains.extend(tag_value_clauses(ctx, labels, sd, "holds_over_chain"));
        write_sorted(writer, "holds_over_chain", chains)?;
        for tag in ["is_anti_symmetric", "is_cyclic"] {
            if let Some(v) = sd.bool_tags.get(tag) {
                writeln!(writer, "{tag}: {v}")?;
            }
        }
        write_sorted(writer, "is_reflexive", characteristic_lines(ctx, sd, "is_reflexive"))?;
        write_sorted(writer, "is_symmetric", characteristic_lines(ctx, sd, "is_symmetric"))?;
        write_sorted(writer, "is_transitive", characteristic_lines(ctx, sd, "is_transitive"))?;
        write_sorted(writer, "is_functional", characteristic_lines(ctx, sd, "is_functional"))?;
        write_sorted(writer, "is_inverse_functional", characteristic_lines(ctx, sd, "is_inverse_functional"))?;
        write_sorted(writer, "is_a", sd.sub_property_of.iter().map(|(sp, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(sp), format!("{sp}{}{}", render_quals(&quals), label_comment(labels, &[sp])))
        }).chain(tag_value_clauses(ctx, labels, sd, "is_a")).collect())?;
        write_sorted(writer, "intersection_of", tag_value_clauses(ctx, labels, sd, "intersection_of"))?;
        write_sorted(writer, "union_of", tag_value_clauses(ctx, labels, sd, "union_of"))?;
        // `equivalent_to:` and `disjoint_from:` follow `is_a:` in a [Typedef], from
        // EquivalentObjectProperties and DisjointObjectProperties.
        write_sorted(writer, "equivalent_to", sd.equivalent_to.iter().map(|(e, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(e), format!("{e}{}{}", render_quals(&quals), label_comment(labels, &[e])))
        }).chain(tag_value_clauses(ctx, labels, sd, "equivalent_to")).collect())?;
        write_sorted(writer, "disjoint_from", sd.disjoint_from.iter().map(|(dj, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(dj), format!("{dj}{}{}", render_quals(&quals), label_comment(labels, &[dj])))
        }).chain(tag_value_clauses(ctx, labels, sd, "disjoint_from")).collect())?;
        write_sorted(writer, "inverse_of", sd.inverse_of.iter().map(|(inv, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(inv), format!("{inv}{}{}", render_quals(&quals), label_comment(labels, &[inv])))
        }).chain(tag_value_clauses(ctx, labels, sd, "inverse_of")).collect())?;
        transitive_over.extend(tag_value_clauses(ctx, labels, sd, "transitive_over"));
        write_sorted(writer, "transitive_over", transitive_over)?;
        write_sorted(writer, "equivalent_to_chain", tag_value_clauses(ctx, labels, sd, "equivalent_to_chain"))?;
        write_sorted(writer, "disjoint_over", disjoint_over_clauses(ctx, labels, sd))?;
        write_sorted(writer, "relationship", tag_value_clauses(ctx, labels, sd, "relationship"))?;
    } else {
        // An [Instance] frame follows the [Term] layout: `instance_of:` where a
        // [Term] writes `is_a:`, then the individual's object property assertions
        // as `relationship:`, then `property_value:`. Each assertion is one clause,
        // its axiom annotations its qualifiers.
        write_sorted(writer, "instance_of", sd.instance_of.iter().map(|(class, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_relationship(&dbxrefs, &quals, &[]);
            (
                format!("{}\u{0}{class}", fold(class)),
                format!("{class}{}{}", render_quals(&quals), label_comment(labels, &[class])),
            )
        }).collect())?;
        write_sorted(writer, "relationship", sd.assertions.iter().map(|(r, t, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_relationship(&dbxrefs, &quals, &[]);
            let value = format!("{r} {t}");
            (
                format!("{}\u{0}{value}", fold(&value)),
                format!("{value}{}{}", render_quals(&quals), label_comment_pred(labels, &declared_prefixes, &[r, t])),
            )
        }).collect())?;
        // `relationship:` in an [Instance] is an object property assertion, so the
        // IRI-valued metadata-tag annotations a [Term] writes there are
        // `property_value:`s here, as in a [Typedef].
        for (pred, val, anns, prop_iri, val_iri) in &sd.rel_or_pv {
            property_values.push(pv_entry(pred, val, true, &None, anns, prop_iri, val_iri));
        }
        write_sorted(writer, "property_value", property_values)?;
    }

    if sd.deprecated {
        let (dbxrefs, _, quals) = ax_ann_pieces(ctx, &sd.deprecated_anns);
        let quals = quals_with_xrefs(&dbxrefs, &quals);
        writeln!(writer, "is_obsolete: true{}", render_quals(&quals))?;
    }
    write_sorted(writer, "replaced_by", sd.replaced_by.iter().map(|r| (fold(r), r.clone())).collect())?;
    write_sorted(writer, "consider", sd.consider.iter().map(|(c, anns)| {
        let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
        // `consider`'s `{source=…}` qualifiers route through the annotation hash set
        // like xref/def/synonym (not the ascending property/value sort), so their
        // block order is hash-bucket order, not alphabetical.
        let quals = quals_with_xrefs_hashset(&dbxrefs, &quals);
        (fold(c), format!("{c}{}", render_quals(&quals)))
    }).collect())?;
    // The clauses of a repeated tag sort by value, so a term with several
    // `created_by:` annotations (EFO_0000001 has three editors; EFO_0004017 two)
    // emits them in string order — not the source/parse order.
    let mut created_by: Vec<&(String, BTreeSet<Annotation<RcStr>>)> = sd.created_by.iter().collect();
    created_by.sort_by(|(a, _), (b, _)| a.cmp(b));
    for (cb, anns) in created_by {
        writeln!(writer, "created_by: {}{}", escape_name(cb), clause_quals(ctx, anns))?;
    }
    for (cd, anns) in &sd.creation_date {
        writeln!(writer, "creation_date: {}{}", escape_name(cd), clause_quals(ctx, anns))?;
    }
    if let Some(sh) = &sd.shorthand {
        let _ = sh; // shorthand is reconstructed from the id+xref; not re-emitted
    }
    if typedef {
        for (v, anns) in &sd.expand_assertion_to {
            let (dbxrefs, _, _) = ax_ann_pieces(ctx, anns);
            writeln!(writer, "expand_assertion_to: \"{}\" {}", escape(&shorten_macro_iris(v)), render_bracket(&dbxrefs))?;
        }
        for (v, anns) in &sd.expand_expression_to {
            let (dbxrefs, _, _) = ax_ann_pieces(ctx, anns);
            writeln!(writer, "expand_expression_to: \"{}\" {}", escape(&shorten_macro_iris(v)), render_bracket(&dbxrefs))?;
        }
    }
    // An annotation property has a [Typedef] only through its
    // `is_metadata_tag` assertion, which is how the reader tells it from an
    // object property.
    if typedef && sd.is_metadata_tag {
        writeln!(writer, "is_metadata_tag: true")?;
    } else if typedef && sd.bool_tags.get("is_metadata_tag") == Some(&false) {
        writeln!(writer, "is_metadata_tag: false")?;
    }
    if typedef && sd.is_class_level {
        writeln!(writer, "is_class_level: true")?;
    } else if typedef && sd.bool_tags.get("is_class_level") == Some(&false) {
        writeln!(writer, "is_class_level: false")?;
    }
    // A tag with no place in the frame's tag order comes last: `is_asymmetric`
    // in a [Typedef], after even `expand_expression_to`, and in a [Term] the
    // typedef tags, in the order a hash set of the frame's tag names iterates
    // them.
    if typedef {
        write_sorted(writer, "is_asymmetric", characteristic_lines(ctx, sd, "is_asymmetric"))?;
    } else {
        let mut last: Vec<(&str, Vec<(String, String)>)> = std::iter::once(("disjoint_over", disjoint_over_clauses(ctx, labels, sd)))
            .chain(
                ["domain", "range", "inverse_of", "transitive_over", "equivalent_to_chain"]
                    .into_iter()
                    .map(|tag| (tag, tag_value_clauses(ctx, labels, sd, tag))),
            )
            .filter(|(_, clauses)| !clauses.is_empty())
            .collect();
        let hashes: Vec<i32> = last.iter().map(|(tag, _)| crate::owlapi_hash::java_string_hash(tag)).collect();
        let order = crate::owlapi_hash::hashset_order_of(&hashes, frame_tag_count(writer) + last.len());
        for i in order {
            let clauses = std::mem::take(&mut last[i].1);
            write_sorted(writer, last[i].0, clauses)?;
        }
    }
    out.write_all(&frame)?;
    Ok(())
}

/// The number of distinct tags among a frame's written clauses.
fn frame_tag_count(frame: &[u8]) -> usize {
    String::from_utf8_lossy(frame)
        .lines()
        .filter_map(|line| line.split_once(':').map(|(tag, _)| tag.to_string()))
        .collect::<HashSet<_>>()
        .len()
}

/// The clauses of `tag` a subject has from annotations of a property with that
/// tag (see [`SubjData::tag_values`]), keyed by value as the tag's other clauses
/// are. Each carries the label of the id it names, as the tag's other clauses
/// do, except a `holds_over_chain:`.
fn tag_value_clauses(ctx: &Ctx, labels: &HashMap<String, String>, sd: &SubjData, tag: &str) -> Vec<(String, String)> {
    sd.tag_values
        .iter()
        .filter(|(t, _, _)| *t == tag)
        .map(|(_, v, anns)| {
            let comment = if tag == "holds_over_chain" { String::new() } else { label_comment(labels, &[v]) };
            (format!("{}\u{0}{v}", fold(v)), format!("{}{}{comment}", escape_name(v), clause_quals(ctx, anns)))
        })
        .collect()
}

/// A subject's `oboInOwl:disjoint_over` assertions, one `disjoint_over:` clause
/// each, keyed by value and carrying the label of the id it names.
fn disjoint_over_clauses(ctx: &Ctx, labels: &HashMap<String, String>, sd: &SubjData) -> Vec<(String, String)> {
    sd.disjoint_over
        .iter()
        .map(|(v, anns)| (fold(v), format!("{}{}{}", escape_name(v), clause_quals(ctx, anns), label_comment(labels, &[v]))))
        .collect()
}

/// The `TAG:` clauses of a characteristic: `true` for each axiom stating it,
/// qualified by that axiom's annotations, or else the value an annotation
/// states.
fn characteristic_lines(ctx: &Ctx, sd: &SubjData, tag: &'static str) -> Vec<(String, String)> {
    match sd.characteristics.get(tag) {
        Some(axioms) => axioms
            .iter()
            .map(|anns| {
                let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
                let quals = quals_with_xrefs(&dbxrefs, &quals);
                ("true".to_string(), format!("true{}", render_quals(&quals)))
            })
            .collect(),
        None => {
            let stated = if tag == "is_transitive" { sd.transitive_anno } else { sd.bool_tags.get(tag).copied() };
            stated.map(|v| (v.to_string(), v.to_string())).into_iter().collect()
        }
    }
}

/// Percent-decode a URI fragment to the string the OBO id is built from —
/// `R%C3%A9union` → `Réunion`. Only complete `%HH` pairs decode; a
/// stray `%` or invalid UTF-8 result is left verbatim. A fragment with no `%` is
/// returned as-is (the common case), so ordinary obo ids pay nothing.
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let hex = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn escape(s: &str) -> String {
    // OBO escaping (reverse of `unescape_obo`) for a *quoted* value — a `def:`
    // text, a `synonym:` text, a literal `property_value:` or a `{key="…"}`
    // qualifier: backslash, quote, newline. An unescaped newline in a value breaks
    // the OBO parser (the continuation is read as a new tag), so it must escape.
    // A literal TAB, though, stays as-is even inside a quoted value — AfPO's
    // coordinate `property_value`s and three EFO `def`s carry real tabs, written
    // verbatim. Braces are *not* escaped here; they only need escaping where they
    // are not already inside quotes.
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Escaping for an unquoted `comment:` value. The qualifier braces escape (they
/// would otherwise start a `{…}` block) but a double quote deliberately does not —
/// CL's `comment: The term "neuroepithelial cell" is used …` is written verbatim,
/// and escaping it makes the round-trip text differ.
fn escape_unquoted(s: &str) -> String {
    // A literal TAB is left as-is: it is only a field delimiter in the `[Term]`
    // header line, never inside a clause value — RO:0002120's macro-expansion
    // comment carries an indented multi-line body with real tabs. Newlines still
    // escape, since a bare one would end the clause.
    s.replace('\\', "\\\\")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('\n', "\\n")
}

/// Escaping for an unquoted value other than a `comment:` (`name:`,
/// `namespace:`, `created_by:`, `owl-axioms:` …): as `escape_unquoted`, plus the
/// double quote.
fn escape_name(s: &str) -> String {
    escape_unquoted(s).replace('"', "\\\"")
}

/// The `{…}` qualifier block of a clause written from an annotation assertion
/// with axiom annotations `anns`.
fn clause_quals(ctx: &Ctx, anns: &BTreeSet<Annotation<RcStr>>) -> String {
    let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
    render_quals(&quals_with_xrefs_hashset(&dbxrefs, &quals))
}

/// Escaping for an xref id, in an `xref:` tag or inside a `[…]` list. `:`
/// separates idspace from local id, `,` separates list entries and `]` ends
/// the list, so every one of those after the leading idspace colon must be
/// escaped — as in CL's `doi:10.1023/a\:1018564904170`.
/// Split an OBO xref value into its id and optional trailing quoted description:
/// `Beilstein:147610 "Beilstein Registry Number"` → (`Beilstein:147610`, `Beilstein
/// Registry Number`). CHEBI stores the description inside the `hasDbXref` literal, so
/// the writer must recover the two parts (the description is spelled as an
/// unescaped trailing quoted string). A value with no ` "…"` suffix is all id.
fn xref_id_desc(x: &str) -> (&str, Option<&str>) {
    if let Some(body) = x.strip_suffix('"') {
        if let Some(pos) = body.find(" \"") {
            return (&x[..pos], Some(&body[pos + 2..]));
        }
    }
    (x, None)
}

/// An xref is split at its FIRST colon and the two halves are escaped separately,
/// so that separator colon comes out bare:
///
/// ```text
/// colonPos = index of first ':'
/// colonPos > 0  =>  escape(prefix) + ':' + escape(local)
/// otherwise     =>  escape(idref)          // whole string, colon included
/// ```
///
/// The guard is `> 0`, not `>= 0`: an xref that BEGINS with a colon has no prefix
/// to split off, so the whole value is escaped and the colon becomes `\:`. HPO's
/// `hp-full.obo` has one such xref — a bare `:` — which must come out as `[\:]`,
/// not `[:]`.
fn escape_xref(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // The separator is the first colon at a position > 0. A value that BEGINS with
    // a colon has no prefix, so no colon in it is a separator and all are escaped.
    let mut separator_taken = s.starts_with(':');
    for c in s.chars() {
        match c {
            ':' if !separator_taken => {
                separator_taken = true;
                out.push(':');
            }
            '\\' | ':' | ',' | ']' | '"' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}
