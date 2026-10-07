//! The order the reader translates a document in, and the names the
//! anonymous individuals in it take.
//!
//! The reader takes the parse's statements one at a time. It translates some at
//! once: declarations, list cells, a node typed with a named class,
//! `owl:sameAs` and `owl:differentFrom`. It files the rest under their subject
//! in hash tables keyed on IRIs, and when the document is complete it
//! translates what it filed:
//! 1. the SWRL rules, in the hash order of their nodes;
//! 2. every `rdfs:range` statement;
//! 3. every statement whose predicate is outside the built-in vocabularies —
//!    property assertions and annotations;
//! 4. the reifications and the other axiom nodes (`owl:Axiom`,
//!    `owl:AllDifferent`, `owl:AllDisjointClasses`, `owl:AllDisjointProperties`,
//!    `owl:NegativePropertyAssertion`);
//! 5. every statement still filed, resources then literals;
//! 6. `owl:inverseOf`.
//!
//! Each pass walks the tables in their own iteration order: the subjects in
//! hash order, then each subject's predicates, then the objects of each. Hash
//! order is that of a `java.util.HashMap` holding the keys: by bucket over the
//! table its largest size grew it to, and in insertion order within a bucket
//! ([`JMap`]). An IRI hashes as the string hashes of its namespace and local
//! name added together; a blank node hashes as its whole label.
//!
//! An anonymous individual is named the first time anything translates it, and
//! takes the next value of the count the parse's own blank nodes draw from. So
//! its name records when it was first translated: during the parse, at the
//! statement that typed it with a named class, or in the pass and table
//! position that first reached it.
//!
//! A translated statement is consumed; a pass meets only what earlier passes
//! left. What a pass meets also depends on what the reader had learned by then
//! of each IRI — which are classes, which object, data or annotation
//! properties — so that is kept as the reader keeps it, including what it
//! infers from where an IRI is used.
//!
//! The order is computed for one document alone: an imported document is not
//! read in the middle of the one that imports it, and what it declares does not
//! inform this one.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use super::parse::{self, Sink};
use crate::owlapi_hash::{iri_split, java_hashset_capacity, java_string_hash};

pub(crate) type Term = u32;
type Lit = u32;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const SWRL: &str = "http://www.w3.org/2003/11/swrl#";
const SWRLB: &str = "http://www.w3.org/2003/11/swrlb#";

macro_rules! vocabulary {
    ($($field:ident = $ns:ident $local:literal,)*) => {
        /// The vocabulary the reader dispatches on.
        struct Vocab { $($field: Term,)* }
        impl Vocab {
            fn new(terms: &mut Terms) -> Vocab {
                Vocab { $($field: terms.id(&format!("{}{}", $ns, $local)),)* }
            }
        }
    };
}

vocabulary! {
    rdf_type = RDF "type", rdf_first = RDF "first", rdf_rest = RDF "rest", rdf_nil = RDF "nil",
    rdf_list = RDF "List", rdf_property = RDF "Property", rdf_subject = RDF "subject",
    rdf_predicate = RDF "predicate", rdf_object = RDF "object",
    rdfs_subclassof = RDFS "subClassOf", rdfs_subpropertyof = RDFS "subPropertyOf", rdfs_domain = RDFS "domain",
    rdfs_range = RDFS "range", rdfs_class = RDFS "Class", rdfs_datatype = RDFS "Datatype",
    rdfs_label = RDFS "label", rdfs_comment = RDFS "comment", rdfs_seealso = RDFS "seeAlso",
    rdfs_isdefinedby = RDFS "isDefinedBy",
    owl_class = OWL "Class", owl_thing = OWL "Thing", owl_nothing = OWL "Nothing", owl_restriction = OWL "Restriction",
    owl_ontology = OWL "Ontology", owl_axiom = OWL "Axiom", owl_annotation = OWL "Annotation",
    owl_alldifferent = OWL "AllDifferent", owl_alldisjointclasses = OWL "AllDisjointClasses",
    owl_alldisjointproperties = OWL "AllDisjointProperties",
    owl_negativepropertyassertion = OWL "NegativePropertyAssertion", owl_namedindividual = OWL "NamedIndividual",
    owl_objectproperty = OWL "ObjectProperty", owl_datatypeproperty = OWL "DatatypeProperty",
    owl_annotationproperty = OWL "AnnotationProperty", owl_ontologyproperty = OWL "OntologyProperty",
    owl_functionalproperty = OWL "FunctionalProperty", owl_inversefunctionalproperty = OWL "InverseFunctionalProperty",
    owl_transitiveproperty = OWL "TransitiveProperty", owl_symmetricproperty = OWL "SymmetricProperty",
    owl_asymmetricproperty = OWL "AsymmetricProperty", owl_reflexiveproperty = OWL "ReflexiveProperty",
    owl_irreflexiveproperty = OWL "IrreflexiveProperty", owl_deprecatedclass = OWL "DeprecatedClass",
    owl_deprecatedproperty = OWL "DeprecatedProperty", owl_selfrestriction = OWL "SelfRestriction",
    owl_sameas = OWL "sameAs", owl_differentfrom = OWL "differentFrom", owl_disjointunionof = OWL "disjointUnionOf",
    owl_disjointwith = OWL "disjointWith", owl_equivalentclass = OWL "equivalentClass",
    owl_equivalentproperty = OWL "equivalentProperty", owl_imports = OWL "imports",
    owl_intersectionof = OWL "intersectionOf", owl_unionof = OWL "unionOf", owl_complementof = OWL "complementOf",
    owl_oneof = OWL "oneOf", owl_somevaluesfrom = OWL "someValuesFrom", owl_allvaluesfrom = OWL "allValuesFrom",
    owl_hasvalue = OWL "hasValue", owl_hasself = OWL "hasSelf", owl_onproperty = OWL "onProperty",
    owl_onclass = OWL "onClass", owl_ondatarange = OWL "onDataRange",
    owl_datatypecomplementof = OWL "datatypeComplementOf", owl_declaredas = OWL "declaredAs",
    owl_haskey = OWL "hasKey", owl_versioniri = OWL "versionIRI", owl_propertychainaxiom = OWL "propertyChainAxiom",
    owl_propertychain = OWL "propertyChain",
    owl_annotatedsource = OWL "annotatedSource", owl_annotatedproperty = OWL "annotatedProperty",
    owl_annotatedtarget = OWL "annotatedTarget", owl_propertydisjointwith = OWL "propertyDisjointWith",
    owl_inverseof = OWL "inverseOf", owl_members = OWL "members", owl_distinctmembers = OWL "distinctMembers",
    owl_sourceindividual = OWL "sourceIndividual", owl_assertionproperty = OWL "assertionProperty",
    owl_targetindividual = OWL "targetIndividual", owl_targetvalue = OWL "targetValue",
    owl_ondatatype = OWL "onDatatype", owl_withrestrictions = OWL "withRestrictions",
    owl_mincardinality = OWL "minCardinality", owl_maxcardinality = OWL "maxCardinality",
    owl_cardinality = OWL "cardinality", owl_minqualifiedcardinality = OWL "minQualifiedCardinality",
    owl_maxqualifiedcardinality = OWL "maxQualifiedCardinality", owl_qualifiedcardinality = OWL "qualifiedCardinality",
    owl_versioninfo = OWL "versionInfo", owl_priorversion = OWL "priorVersion", owl_deprecated = OWL "deprecated",
    owl_backwardcompatiblewith = OWL "backwardCompatibleWith", owl_incompatiblewith = OWL "incompatibleWith",
    owl_topobjectproperty = OWL "topObjectProperty", owl_bottomobjectproperty = OWL "bottomObjectProperty",
    owl_topdataproperty = OWL "topDataProperty", owl_bottomdataproperty = OWL "bottomDataProperty",
    xsd_string = XSD "string",
    swrl_imp = SWRL "Imp", swrl_head = SWRL "head", swrl_body = SWRL "body", swrl_atomlist = SWRL "AtomList",
    swrl_classatom = SWRL "ClassAtom", swrl_individualpropertyatom = SWRL "IndividualPropertyAtom",
    swrl_datavaluedpropertyatom = SWRL "DatavaluedPropertyAtom", swrl_sameindividualatom = SWRL "SameIndividualAtom",
    swrl_differentindividualsatom = SWRL "DifferentIndividualsAtom", swrl_builtinatom = SWRL "BuiltinAtom",
    swrl_datarangeatom = SWRL "DataRangeAtom", swrl_variable = SWRL "Variable", swrl_builtin_type = SWRL "Builtin",
    swrl_argument1 = SWRL "argument1", swrl_argument2 = SWRL "argument2", swrl_arguments = SWRL "arguments",
    swrl_classpredicate = SWRL "classPredicate", swrl_propertypredicate = SWRL "propertyPredicate",
    swrl_datarange = SWRL "dataRange", swrl_builtin = SWRL "builtin",
}

/// The datatypes every document starts out knowing: the OWL 2 datatypes and
/// the XML Schema vocabulary.
const DATATYPES: &[&str] = &[
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString",
    "http://www.w3.org/2000/01/rdf-schema#Literal",
    "http://www.w3.org/2002/07/owl#real",
    "http://www.w3.org/2002/07/owl#rational",
    "http://www.w3.org/2001/XMLSchema#anyType",
    "http://www.w3.org/2001/XMLSchema#anySimpleType",
    "http://www.w3.org/2001/XMLSchema#string",
    "http://www.w3.org/2001/XMLSchema#integer",
    "http://www.w3.org/2001/XMLSchema#long",
    "http://www.w3.org/2001/XMLSchema#int",
    "http://www.w3.org/2001/XMLSchema#short",
    "http://www.w3.org/2001/XMLSchema#byte",
    "http://www.w3.org/2001/XMLSchema#decimal",
    "http://www.w3.org/2001/XMLSchema#float",
    "http://www.w3.org/2001/XMLSchema#boolean",
    "http://www.w3.org/2001/XMLSchema#double",
    "http://www.w3.org/2001/XMLSchema#nonPositiveInteger",
    "http://www.w3.org/2001/XMLSchema#positiveInteger",
    "http://www.w3.org/2001/XMLSchema#negativeInteger",
    "http://www.w3.org/2001/XMLSchema#nonNegativeInteger",
    "http://www.w3.org/2001/XMLSchema#unsignedLong",
    "http://www.w3.org/2001/XMLSchema#unsignedInt",
    "http://www.w3.org/2001/XMLSchema#unsignedShort",
    "http://www.w3.org/2001/XMLSchema#unsignedByte",
    "http://www.w3.org/2001/XMLSchema#normalizedString",
    "http://www.w3.org/2001/XMLSchema#token",
    "http://www.w3.org/2001/XMLSchema#language",
    "http://www.w3.org/2001/XMLSchema#Name",
    "http://www.w3.org/2001/XMLSchema#NCName",
    "http://www.w3.org/2001/XMLSchema#NMTOKEN",
    "http://www.w3.org/2001/XMLSchema#hexBinary",
    "http://www.w3.org/2001/XMLSchema#base64Binary",
    "http://www.w3.org/2001/XMLSchema#anyURI",
    "http://www.w3.org/2001/XMLSchema#dateTime",
    "http://www.w3.org/2001/XMLSchema#dateTimeStamp",
    "http://www.w3.org/2001/XMLSchema#duration",
    "http://www.w3.org/2001/XMLSchema#dayTimeDuration",
    "http://www.w3.org/2001/XMLSchema#yearMonthDuration",
    "http://www.w3.org/2001/XMLSchema#time",
    "http://www.w3.org/2001/XMLSchema#date",
    "http://www.w3.org/2001/XMLSchema#gYearMonth",
    "http://www.w3.org/2001/XMLSchema#gYear",
    "http://www.w3.org/2001/XMLSchema#gMonthDay",
    "http://www.w3.org/2001/XMLSchema#gDay",
    "http://www.w3.org/2001/XMLSchema#gMonth",
    "http://www.w3.org/2001/XMLSchema#NOTATION",
    "http://www.w3.org/2001/XMLSchema#QName",
    "http://www.w3.org/2001/XMLSchema#ID",
    "http://www.w3.org/2001/XMLSchema#IDREF",
    "http://www.w3.org/2001/XMLSchema#IDREFS",
    "http://www.w3.org/2001/XMLSchema#ENTITY",
    "http://www.w3.org/2001/XMLSchema#ENTITIES",
    "http://www.w3.org/2001/XMLSchema#NMTOKENS",
];

/// The facets, in the order a facet list's item is searched for one.
const FACETS: &[&str] = &[
    "http://www.w3.org/2001/XMLSchema#length",
    "http://www.w3.org/2001/XMLSchema#minLength",
    "http://www.w3.org/2001/XMLSchema#maxLength",
    "http://www.w3.org/2001/XMLSchema#pattern",
    "http://www.w3.org/2001/XMLSchema#minInclusive",
    "http://www.w3.org/2001/XMLSchema#minExclusive",
    "http://www.w3.org/2001/XMLSchema#maxInclusive",
    "http://www.w3.org/2001/XMLSchema#maxExclusive",
    "http://www.w3.org/2001/XMLSchema#totalDigits",
    "http://www.w3.org/2001/XMLSchema#fractionDigits",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#langRange",
];

/// The local names of the OWL vocabulary that the OWL 1.1 and draft OWL 2
/// namespaces read as, by namespace.
const LEGACY_NAMES: &[(&str, &str)] = &[
    (OWL, "Thing"), (OWL, "Nothing"), (OWL, "Class"), (OWL, "Ontology"), (OWL, "imports"), (OWL, "versionIRI"),
    (OWL, "versionInfo"), (OWL, "equivalentClass"), (OWL, "ObjectProperty"), (OWL, "DatatypeProperty"),
    (OWL, "FunctionalProperty"), (OWL, "AsymmetricProperty"), (OWL, "SymmetricProperty"), (OWL, "Restriction"),
    (OWL, "onProperty"), (OWL, "intersectionOf"), (OWL, "unionOf"), (OWL, "allValuesFrom"), (OWL, "someValuesFrom"),
    (OWL, "hasValue"), (OWL, "disjointWith"), (OWL, "oneOf"), (OWL, "hasSelf"), (OWL, "disjointUnionOf"),
    (OWL, "minCardinality"), (OWL, "cardinality"), (OWL, "qualifiedCardinality"), (OWL, "AnnotationProperty"),
    (OWL, "Annotation"), (OWL, "Individual"), (OWL, "NamedIndividual"), (OWL, "Datatype"), (RDFS, "subClassOf"),
    (RDFS, "subPropertyOf"), (RDF, "type"), (RDF, "nil"), (RDF, "rest"), (RDF, "first"), (RDF, "List"),
    (OWL, "maxCardinality"), (RDFS, "label"), (RDFS, "comment"), (RDFS, "seeAlso"), (RDFS, "isDefinedBy"),
    (RDFS, "Resource"), (RDFS, "Literal"), (RDFS, "Datatype"), (OWL, "TransitiveProperty"),
    (OWL, "ReflexiveProperty"), (OWL, "IrreflexiveProperty"), (OWL, "inverseOf"), (OWL, "complementOf"),
    (OWL, "datatypeComplementOf"), (OWL, "AllDifferent"), (OWL, "distinctMembers"), (OWL, "sameAs"),
    (OWL, "differentFrom"), (OWL, "DeprecatedProperty"), (OWL, "equivalentProperty"), (OWL, "DeprecatedClass"),
    (OWL, "DataRange"), (RDFS, "domain"), (RDFS, "range"), (RDFS, "Class"), (RDF, "Property"),
    (OWL, "priorVersion"), (OWL, "deprecated"), (OWL, "incompatibleWith"), (OWL, "propertyDisjointWith"),
    (OWL, "onClass"), (OWL, "onDataRange"), (OWL, "onDatatype"), (OWL, "withRestrictions"), (OWL, "Axiom"),
    (OWL, "propertyChainAxiom"), (OWL, "AllDisjointClasses"), (OWL, "members"), (OWL, "AllDisjointProperties"),
    (OWL, "topObjectProperty"), (OWL, "bottomObjectProperty"), (OWL, "topDataProperty"),
    (OWL, "bottomDataProperty"), (OWL, "hasKey"), (OWL, "annotatedSource"), (OWL, "annotatedProperty"),
    (OWL, "annotatedTarget"), (OWL, "sourceIndividual"), (OWL, "assertionProperty"), (OWL, "targetIndividual"),
    (OWL, "targetValue"), (OWL, "InverseFunctionalProperty"), (OWL, "minQualifiedCardinality"),
    (OWL, "maxQualifiedCardinality"), (OWL, "NegativePropertyAssertion"), (RDF, "langString"),
    (RDF, "PlainLiteral"), (RDF, "Description"), (RDF, "XMLLiteral"), (OWL, "backwardCompatibleWith"),
];

/// The terms of a document: every IRI and blank node it names.
#[derive(Default)]
struct Terms {
    ids: HashMap<Box<str>, Term>,
    names: Vec<Box<str>>,
    hashes: Vec<i32>,
    blank: Vec<bool>,
    reserved: Vec<bool>,
}

impl Terms {
    fn id(&mut self, name: &str) -> Term {
        if let Some(&t) = self.ids.get(name) {
            return t;
        }
        let t = self.names.len() as Term;
        // A blank node's label is never split: it hashes whole.
        let (ns, local) = if name.starts_with("_:") { (name, "") } else { iri_split(name) };
        self.hashes.push(java_string_hash(ns).wrapping_add(java_string_hash(local)));
        self.reserved.push([OWL, RDF, RDFS, XSD].contains(&ns));
        self.blank.push(parse::is_blank(name));
        self.names.push(name.into());
        self.ids.insert(name.into(), t);
        t
    }
}

/// The keys of a `java.util.HashMap` and the order it iterates them in: by
/// bucket over the table the map grew to at its largest, and in insertion
/// order within a bucket.
struct JMap<V> {
    slots: Vec<Term>,
    index: HashMap<Term, (usize, V)>,
    max: usize,
}

impl<V> Default for JMap<V> {
    fn default() -> Self {
        JMap { slots: Vec::new(), index: HashMap::new(), max: 0 }
    }
}

impl<V> JMap<V> {
    fn get(&self, k: Term) -> Option<&V> {
        self.index.get(&k).map(|(_, v)| v)
    }

    fn get_mut(&mut self, k: Term) -> Option<&mut V> {
        self.index.get_mut(&k).map(|(_, v)| v)
    }

    fn contains(&self, k: Term) -> bool {
        self.index.contains_key(&k)
    }

    fn insert_with(&mut self, k: Term, f: impl FnOnce() -> V) -> &mut V {
        if !self.index.contains_key(&k) {
            self.index.insert(k, (self.slots.len(), f()));
            self.slots.push(k);
            self.max = self.max.max(self.index.len());
        }
        &mut self.index.get_mut(&k).expect("inserted").1
    }

    fn remove(&mut self, k: Term) -> Option<V> {
        self.index.remove(&k).map(|(_, v)| v)
    }

    fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    fn order(&self, hashes: &[i32]) -> Vec<Term> {
        let mask = java_hashset_capacity(self.max) as u32 - 1;
        let mut live: Vec<(u32, Term)> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(i, k)| self.index.get(k).is_some_and(|(slot, _)| slot == i))
            .map(|(_, &k)| {
                let h = hashes[k as usize] as u32;
                ((h ^ (h >> 16)) & mask, k)
            })
            .collect();
        live.sort_by_key(|(bucket, _)| *bucket);
        live.into_iter().map(|(_, k)| k).collect()
    }

    fn first(&self, hashes: &[i32]) -> Option<Term> {
        self.order(hashes).first().copied()
    }
}

type Objects = JMap<()>;

/// The type handlers the reader dispatches `rdf:type` statements to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TypeKind {
    OntologyProperty,
    Asymmetric,
    Class,
    ObjectProperty,
    DataProperty,
    Datatype,
    Functional,
    InverseFunctional,
    Irreflexive,
    Reflexive,
    Symmetric,
    Transitive,
    Restriction,
    List,
    AnnotationProperty,
    DeprecatedClass,
    DeprecatedProperty,
    Ontology,
    RdfsClass,
    SelfRestriction,
    Property,
    NamedIndividual,
    Annotation,
    SwrlAtomList,
    SwrlBuiltinAtom,
    SwrlBuiltin,
    SwrlClassAtom,
    SwrlDataRangeAtom,
    SwrlDataValuedPropertyAtom,
    SwrlDifferentIndividualsAtom,
    SwrlImp,
    SwrlIndividualPropertyAtom,
    SwrlSameIndividualAtom,
    SwrlVariable,
}

/// The node types whose statements stand for an axiom.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AxiomKind {
    Axiom,
    AllDifferent,
    AllDisjointClasses,
    AllDisjointProperties,
    NegativePropertyAssertion,
}

/// The handlers the reader dispatches statements to by predicate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PredKind {
    DifferentFrom,
    DisjointUnion,
    DisjointWith,
    EquivalentClass,
    EquivalentProperty,
    Domain,
    Range,
    SameAs,
    SubClassOf,
    SubPropertyOf,
    Imports,
    IntersectionOf,
    UnionOf,
    ComplementOf,
    OneOf,
    SomeValuesFrom,
    AllValuesFrom,
    Rest,
    First,
    DeclaredAs,
    HasKey,
    VersionIri,
    PropertyChainAxiom,
    Annotated,
    PropertyDisjointWith,
    InverseOf,
    OnProperty,
    OnClass,
    OnDataRange,
    DatatypeComplementOf,
}

/// A literal's identity: lexical form, datatype, language.
#[derive(Clone, PartialEq, Eq, Hash)]
struct LitKey {
    value: Box<str>,
    datatype: Term,
    lang: Box<str>,
}

/// A cardinality restriction: its cardinality predicate, whether it is
/// qualified, and the predicate naming its filler.
type Cardinality = (Term, bool, Term);

/// What reading a document made of its anonymous individuals and axiom nodes.
pub(crate) struct Names {
    /// Each anonymous individual's blank node, with the number its name takes.
    pub individuals: HashMap<String, u64>,
    /// The axiom nodes, in the order the reader translated them.
    pub axiom_nodes: Vec<String>,
    /// The `owl:Annotation` nodes, in the order the reader translated them.
    pub annotation_nodes: Vec<String>,
    /// The count after the document: the next blank node's number.
    pub next: u64,
}

pub(crate) struct Order {
    terms: Terms,
    v: Vocab,
    lits: Vec<LitKey>,
    lit_ids: HashMap<LitKey, Lit>,
    synonyms: HashMap<Term, Term>,
    builtin_types: HashMap<Term, TypeKind>,
    axiom_types: HashMap<Term, AxiomKind>,
    predicates: HashMap<Term, PredKind>,
    builtin_annotation_properties: HashSet<Term>,
    facets: Vec<Term>,
    object_cardinalities: Vec<Cardinality>,
    data_cardinalities: Vec<Cardinality>,

    // what the reader has filed
    res: JMap<JMap<Objects>>,
    lit: JMap<JMap<Vec<Lit>>>,
    single: HashMap<Term, HashMap<Term, Term>>,
    first_res: HashMap<Term, Term>,
    first_lit: HashMap<Term, Lit>,
    rest: HashMap<Term, Term>,

    // what the reader knows of each term
    class_expressions: HashSet<Term>,
    object_properties: HashSet<Term>,
    data_properties: HashSet<Term>,
    annotation_properties: HashSet<Term>,
    data_ranges: HashSet<Term>,
    individuals: HashSet<Term>,
    restrictions: HashSet<Term>,
    axioms: HashSet<Term>,
    annotations: HashSet<Term>,
    ontologies: HashSet<Term>,
    swrl_rules: JMap<()>,
    swrl_individual_property_atoms: HashSet<Term>,
    swrl_data_valued_property_atoms: HashSet<Term>,
    swrl_class_atoms: HashSet<Term>,
    swrl_data_range_atoms: HashSet<Term>,
    swrl_builtin_atoms: HashSet<Term>,
    swrl_variables: HashSet<Term>,
    swrl_same_as_atoms: HashSet<Term>,
    swrl_different_from_atoms: HashSet<Term>,
    annotated: HashMap<Term, JMap<()>>,
    /// The `owl:Annotation` nodes, in the order their annotations are read.
    annotation_order: Vec<Term>,
    annotation_seen: HashSet<Term>,
    remapped: HashMap<Term, Term>,
    translated_classes: HashSet<Term>,
    translated_properties: HashSet<Term>,
    shared_lists: HashSet<Term>,
    parsed_all: bool,
    inverse_axioms: bool,

    // what reading names
    next: u64,
    named: HashMap<Term, u64>,
    axiom_nodes: Vec<Term>,
    trace: bool,
}

impl Order {
    pub(crate) fn new(first_id: u64, trace: bool) -> Order {
        let mut terms = Terms::default();
        let v = Vocab::new(&mut terms);
        let mut o = Order {
            terms,
            v,
            lits: Vec::new(),
            lit_ids: HashMap::new(),
            synonyms: HashMap::new(),
            builtin_types: HashMap::new(),
            axiom_types: HashMap::new(),
            predicates: HashMap::new(),
            builtin_annotation_properties: HashSet::new(),
            facets: Vec::new(),
            object_cardinalities: Vec::new(),
            data_cardinalities: Vec::new(),
            res: JMap::default(),
            lit: JMap::default(),
            single: HashMap::new(),
            first_res: HashMap::new(),
            first_lit: HashMap::new(),
            rest: HashMap::new(),
            class_expressions: HashSet::new(),
            object_properties: HashSet::new(),
            data_properties: HashSet::new(),
            annotation_properties: HashSet::new(),
            data_ranges: HashSet::new(),
            individuals: HashSet::new(),
            restrictions: HashSet::new(),
            axioms: HashSet::new(),
            annotations: HashSet::new(),
            ontologies: HashSet::new(),
            swrl_rules: JMap::default(),
            swrl_individual_property_atoms: HashSet::new(),
            swrl_data_valued_property_atoms: HashSet::new(),
            swrl_class_atoms: HashSet::new(),
            swrl_data_range_atoms: HashSet::new(),
            swrl_builtin_atoms: HashSet::new(),
            swrl_variables: HashSet::new(),
            swrl_same_as_atoms: HashSet::new(),
            swrl_different_from_atoms: HashSet::new(),
            annotated: HashMap::new(),
            annotation_order: Vec::new(),
            annotation_seen: HashSet::new(),
            remapped: HashMap::new(),
            translated_classes: HashSet::new(),
            translated_properties: HashSet::new(),
            shared_lists: HashSet::new(),
            parsed_all: false,
            inverse_axioms: false,
            next: first_id,
            named: HashMap::new(),
            axiom_nodes: Vec::new(),
            trace,
        };
        o.setup();
        o
    }

    fn setup(&mut self) {
        let v = &self.v;
        for (t, k) in [
            (v.owl_ontologyproperty, TypeKind::OntologyProperty),
            (v.owl_asymmetricproperty, TypeKind::Asymmetric),
            (v.owl_class, TypeKind::Class),
            (v.owl_objectproperty, TypeKind::ObjectProperty),
            (v.owl_datatypeproperty, TypeKind::DataProperty),
            (v.rdfs_datatype, TypeKind::Datatype),
            (v.owl_functionalproperty, TypeKind::Functional),
            (v.owl_inversefunctionalproperty, TypeKind::InverseFunctional),
            (v.owl_irreflexiveproperty, TypeKind::Irreflexive),
            (v.owl_reflexiveproperty, TypeKind::Reflexive),
            (v.owl_symmetricproperty, TypeKind::Symmetric),
            (v.owl_transitiveproperty, TypeKind::Transitive),
            (v.owl_restriction, TypeKind::Restriction),
            (v.rdf_list, TypeKind::List),
            (v.owl_annotationproperty, TypeKind::AnnotationProperty),
            (v.owl_deprecatedclass, TypeKind::DeprecatedClass),
            (v.owl_deprecatedproperty, TypeKind::DeprecatedProperty),
            (v.owl_ontology, TypeKind::Ontology),
            (v.rdfs_class, TypeKind::RdfsClass),
            (v.owl_selfrestriction, TypeKind::SelfRestriction),
            (v.rdf_property, TypeKind::Property),
            (v.owl_namedindividual, TypeKind::NamedIndividual),
            (v.owl_annotation, TypeKind::Annotation),
            (v.swrl_atomlist, TypeKind::SwrlAtomList),
            (v.swrl_builtinatom, TypeKind::SwrlBuiltinAtom),
            (v.swrl_builtin_type, TypeKind::SwrlBuiltin),
            (v.swrl_classatom, TypeKind::SwrlClassAtom),
            (v.swrl_datarangeatom, TypeKind::SwrlDataRangeAtom),
            (v.swrl_datavaluedpropertyatom, TypeKind::SwrlDataValuedPropertyAtom),
            (v.swrl_differentindividualsatom, TypeKind::SwrlDifferentIndividualsAtom),
            (v.swrl_imp, TypeKind::SwrlImp),
            (v.swrl_individualpropertyatom, TypeKind::SwrlIndividualPropertyAtom),
            (v.swrl_sameindividualatom, TypeKind::SwrlSameIndividualAtom),
            (v.swrl_variable, TypeKind::SwrlVariable),
        ] {
            self.builtin_types.insert(t, k);
        }
        for (t, k) in [
            (v.owl_axiom, AxiomKind::Axiom),
            (v.owl_alldifferent, AxiomKind::AllDifferent),
            (v.owl_alldisjointclasses, AxiomKind::AllDisjointClasses),
            (v.owl_alldisjointproperties, AxiomKind::AllDisjointProperties),
            (v.owl_negativepropertyassertion, AxiomKind::NegativePropertyAssertion),
        ] {
            self.axiom_types.insert(t, k);
        }
        for (t, k) in [
            (v.owl_differentfrom, PredKind::DifferentFrom),
            (v.owl_disjointunionof, PredKind::DisjointUnion),
            (v.owl_disjointwith, PredKind::DisjointWith),
            (v.owl_equivalentclass, PredKind::EquivalentClass),
            (v.owl_equivalentproperty, PredKind::EquivalentProperty),
            (v.rdfs_domain, PredKind::Domain),
            (v.rdfs_range, PredKind::Range),
            (v.owl_sameas, PredKind::SameAs),
            (v.rdfs_subclassof, PredKind::SubClassOf),
            (v.rdfs_subpropertyof, PredKind::SubPropertyOf),
            (v.owl_imports, PredKind::Imports),
            (v.owl_intersectionof, PredKind::IntersectionOf),
            (v.owl_unionof, PredKind::UnionOf),
            (v.owl_complementof, PredKind::ComplementOf),
            (v.owl_oneof, PredKind::OneOf),
            (v.owl_somevaluesfrom, PredKind::SomeValuesFrom),
            (v.owl_allvaluesfrom, PredKind::AllValuesFrom),
            (v.rdf_rest, PredKind::Rest),
            (v.rdf_first, PredKind::First),
            (v.owl_declaredas, PredKind::DeclaredAs),
            (v.owl_haskey, PredKind::HasKey),
            (v.owl_versioniri, PredKind::VersionIri),
            (v.owl_propertychainaxiom, PredKind::PropertyChainAxiom),
            (v.owl_annotatedsource, PredKind::Annotated),
            (v.owl_annotatedproperty, PredKind::Annotated),
            (v.owl_annotatedtarget, PredKind::Annotated),
            (v.owl_propertydisjointwith, PredKind::PropertyDisjointWith),
            (v.owl_inverseof, PredKind::InverseOf),
            (v.owl_onproperty, PredKind::OnProperty),
            (v.owl_onclass, PredKind::OnClass),
            (v.owl_ondatarange, PredKind::OnDataRange),
            (v.owl_datatypecomplementof, PredKind::DatatypeComplementOf),
        ] {
            self.predicates.insert(t, k);
        }
        let builtin_aps = [
            v.rdfs_label,
            v.rdfs_comment,
            v.rdfs_seealso,
            v.rdfs_isdefinedby,
            v.owl_deprecated,
            v.owl_versioninfo,
            v.owl_priorversion,
            v.owl_backwardcompatiblewith,
            v.owl_incompatiblewith,
        ];
        self.builtin_annotation_properties.extend(builtin_aps);
        self.annotation_properties.extend(builtin_aps);
        let single = [v.owl_onproperty, v.owl_somevaluesfrom, v.owl_allvaluesfrom, v.owl_onclass, v.owl_ondatarange];
        for p in single {
            self.single.insert(p, HashMap::new());
        }
        self.class_expressions.extend([v.owl_thing, v.owl_nothing]);
        self.object_properties.extend([v.owl_topobjectproperty, v.owl_bottomobjectproperty]);
        self.data_properties.extend([v.owl_topdataproperty, v.owl_bottomdataproperty]);
        // In the order their restrictions are tried.
        let kinds = [
            (v.owl_minqualifiedcardinality, true),
            (v.owl_maxqualifiedcardinality, true),
            (v.owl_qualifiedcardinality, true),
            (v.owl_mincardinality, false),
            (v.owl_maxcardinality, false),
            (v.owl_cardinality, false),
        ];
        self.object_cardinalities = kinds.iter().map(|&(c, q)| (c, q, v.owl_onclass)).collect();
        self.data_cardinalities = kinds.iter().map(|&(c, q)| (c, q, v.owl_ondatarange)).collect();
        // Older vocabularies read as the current one.
        let owl = OWL.to_string();
        let mut synonyms = vec![(format!("{owl}valuesFrom"), format!("{owl}onClass"))];
        const DAML: &str = "http://www.daml.org/2001/03/daml+oil#";
        for (from, to) in [
            ("subClassOf", "http://www.w3.org/2000/01/rdf-schema#subClassOf"),
            ("imports", "http://www.w3.org/2002/07/owl#imports"),
            ("range", "http://www.w3.org/2000/01/rdf-schema#range"),
            ("hasValue", "http://www.w3.org/2002/07/owl#hasValue"),
            ("type", "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            ("domain", "http://www.w3.org/2000/01/rdf-schema#domain"),
            ("versionInfo", "http://www.w3.org/2002/07/owl#versionInfo"),
            ("comment", "http://www.w3.org/2000/01/rdf-schema#comment"),
            ("onProperty", "http://www.w3.org/2002/07/owl#onProperty"),
            ("toClass", "http://www.w3.org/2002/07/owl#allValuesFrom"),
            ("hasClass", "http://www.w3.org/2002/07/owl#someValuesFrom"),
            ("Restriction", "http://www.w3.org/2002/07/owl#Restriction"),
            ("Class", "http://www.w3.org/2002/07/owl#Class"),
            ("Thing", "http://www.w3.org/2002/07/owl#Thing"),
            ("Nothing", "http://www.w3.org/2002/07/owl#Nothing"),
            ("minCardinality", "http://www.w3.org/2002/07/owl#minCardinality"),
            ("cardinality", "http://www.w3.org/2002/07/owl#cardinality"),
            ("maxCardinality", "http://www.w3.org/2002/07/owl#maxCardinality"),
            ("inverseOf", "http://www.w3.org/2002/07/owl#inverseOf"),
            ("samePropertyAs", "http://www.w3.org/2002/07/owl#equivalentProperty"),
            ("hasClassQ", "http://www.w3.org/2002/07/owl#onClass"),
            ("cardinalityQ", "http://www.w3.org/2002/07/owl#cardinality"),
            ("maxCardinalityQ", "http://www.w3.org/2002/07/owl#maxCardinality"),
            ("minCardinalityQ", "http://www.w3.org/2002/07/owl#minCardinality"),
            ("complementOf", "http://www.w3.org/2002/07/owl#complementOf"),
            ("unionOf", "http://www.w3.org/2002/07/owl#unionOf"),
            ("intersectionOf", "http://www.w3.org/2002/07/owl#intersectionOf"),
            ("label", "http://www.w3.org/2000/01/rdf-schema#label"),
            ("ObjectProperty", "http://www.w3.org/2002/07/owl#ObjectProperty"),
            ("DatatypeProperty", "http://www.w3.org/2002/07/owl#DatatypeProperty"),
        ] {
            synonyms.push((format!("{DAML}{from}"), to.to_string()));
        }
        for (ns, local) in LEGACY_NAMES {
            for legacy in ["http://www.w3.org/2006/12/owl2#", "http://www.w3.org/2006/12/owl11#"] {
                synonyms.push((format!("{legacy}{local}"), format!("{ns}{local}")));
            }
        }
        for facet in FACETS {
            let local = facet.rsplit('#').next().unwrap_or("");
            for legacy in [OWL, "http://www.w3.org/2006/12/owl11#", "http://www.w3.org/2006/12/owl2#"] {
                synonyms.push((format!("{legacy}{local}"), facet.to_string()));
            }
        }
        for (from, to) in [
            ("NegativeDataPropertyAssertion", "NegativePropertyAssertion"),
            ("NegativeObjectPropertyAssertion", "NegativePropertyAssertion"),
            ("subject", "annotatedSource"),
            ("predicate", "annotatedProperty"),
            ("object", "annotatedTarget"),
            ("cardinalityType", "onClass"),
            ("dataComplementOf", "complementOf"),
            ("AntisymmetricProperty", "AsymmetricProperty"),
            ("FunctionalDataProperty", "FunctionalProperty"),
            ("FunctionalObjectProperty", "FunctionalProperty"),
            ("ObjectRestriction", "Restriction"),
            ("DataRestriction", "Restriction"),
            ("disjointDataProperties", "propertyDisjointWith"),
            ("disjointObjectProperties", "propertyDisjointWith"),
            ("equivalentDataProperty", "equivalentProperty"),
            ("equivalentObjectProperty", "equivalentProperty"),
        ] {
            synonyms.push((format!("{owl}{from}"), format!("{owl}{to}")));
        }
        for (from, to) in [
            ("subDataPropertyOf", "http://www.w3.org/2000/01/rdf-schema#subPropertyOf"),
            ("subObjectPropertyOf", "http://www.w3.org/2000/01/rdf-schema#subPropertyOf"),
            ("objectPropertyRange", "http://www.w3.org/2000/01/rdf-schema#range"),
            ("dataPropertyRange", "http://www.w3.org/2000/01/rdf-schema#range"),
            ("objectPropertyDomain", "http://www.w3.org/2000/01/rdf-schema#domain"),
            ("dataPropertyDomain", "http://www.w3.org/2000/01/rdf-schema#domain"),
            ("DataRange", "http://www.w3.org/2000/01/rdf-schema#Datatype"),
        ] {
            synonyms.push((format!("{owl}{from}"), to.to_string()));
        }
        for (from, to) in synonyms {
            let (f, t) = (self.terms.id(&from), self.terms.id(&to));
            self.synonyms.insert(f, t);
        }
        for d in DATATYPES {
            let t = self.terms.id(d);
            self.data_ranges.insert(t);
        }
        self.facets = FACETS.iter().map(|f| self.terms.id(f)).collect();
    }

    // -- the parse's statements, as they come ---------------------------------

    pub(crate) fn term(&mut self, name: &str) -> Term {
        self.terms.id(name)
    }

    pub(crate) fn name(&self, t: Term) -> &str {
        &self.terms.names[t as usize]
    }

    /// The term the reader takes `t` as: a term of an older vocabulary — OWL
    /// 1.1, DAML+OIL, deprecated OWL 2 names — as the current one.
    pub(crate) fn synonym(&self, t: Term) -> Term {
        self.synonyms.get(&t).copied().unwrap_or(t)
    }

    fn remap_subject(&self, t: Term) -> Term {
        self.remapped.get(&t).copied().unwrap_or(t)
    }

    pub(crate) fn next_id(&mut self) -> u64 {
        let n = self.next;
        self.next += 1;
        n
    }

    pub(crate) fn resource(&mut self, s: &str, p: &str, o: &str) {
        let s = self.terms.id(s);
        let s = self.remap_subject(s);
        let p = self.terms.id(p);
        let p = self.synonym(p);
        let o = self.terms.id(o);
        let o = self.synonym(o);
        self.stream_resource(s, p, o);
    }

    pub(crate) fn literal(&mut self, s: &str, p: &str, value: &str, lang: Option<&str>, datatype: Option<&str>) {
        let s = self.terms.id(s);
        let s = self.remap_subject(s);
        let p = self.terms.id(p);
        let p = self.synonym(p);
        let l = self.lit(value, lang, datatype);
        if p == self.v.rdf_first {
            self.first_lit.insert(s, l);
        } else {
            self.add_lit(s, p, l);
        }
    }

    fn lit(&mut self, value: &str, lang: Option<&str>, datatype: Option<&str>) -> Lit {
        let (datatype, lang) = match datatype {
            Some(d) => (self.terms.id(d), String::new()),
            None => (self.v.xsd_string, lang.unwrap_or("").to_ascii_lowercase()),
        };
        let key = LitKey { value: value.into(), datatype, lang: lang.into() };
        if let Some(&l) = self.lit_ids.get(&key) {
            return l;
        }
        let l = self.lits.len() as Lit;
        self.lits.push(key.clone());
        self.lit_ids.insert(key, l);
        l
    }

    fn stream_resource(&mut self, s: Term, p: Term, o: Term) {
        let v = &self.v;
        let mut consumed = false;
        if p == v.rdf_type {
            if let Some(&kind) = self.builtin_types.get(&o) {
                if self.type_can_stream(kind, s, p) {
                    self.type_handle(kind, s, p, o);
                    consumed = true;
                }
            } else if !self.axiom_types.contains_key(&o) {
                self.individuals.insert(s);
                // A named class, or owl:Thing, types an individual at once.
                self.class_expressions.insert(o);
                let streams = !self.blank(o) && (!self.reserved(o) || o == self.v.owl_thing);
                if streams {
                    self.translate_class(o);
                    self.individual(s);
                    self.consume(s, p, o);
                    consumed = true;
                }
            } else {
                self.axioms.insert(s);
            }
        } else if let Some(&kind) = self.predicates.get(&p) {
            if self.pred_can_stream(kind, s, p, o) {
                self.pred_handle(kind, s, p, o);
                consumed = true;
            }
        }
        if !consumed {
            self.add_res(s, p, o);
        }
    }

    fn type_can_stream(&mut self, kind: TypeKind, s: Term, p: Term) -> bool {
        let named = !self.blank(s);
        match kind {
            TypeKind::Asymmetric | TypeKind::Irreflexive | TypeKind::Reflexive => {
                self.object_properties.insert(s);
                named
            }
            TypeKind::Functional => false,
            TypeKind::InverseFunctional | TypeKind::Transitive => {
                let op = self.v.owl_objectproperty;
                self.handle_resource(s, p, op);
                named
            }
            TypeKind::Symmetric => {
                if named {
                    let op = self.v.owl_objectproperty;
                    self.handle_resource(s, p, op);
                }
                self.object_properties.insert(s);
                named
            }
            _ => true,
        }
    }

    fn type_handle(&mut self, kind: TypeKind, s: Term, p: Term, o: Term) {
        match kind {
            TypeKind::OntologyProperty => {
                self.consume(s, p, o);
                let ap = self.v.owl_annotationproperty;
                self.handle_resource(s, p, ap);
            }
            TypeKind::Asymmetric | TypeKind::Irreflexive | TypeKind::Reflexive | TypeKind::Symmetric => {
                if self.object_properties.contains(&s) {
                    self.translate_property(s);
                    self.consume(s, p, o);
                }
            }
            TypeKind::Class => {
                self.class_expressions.insert(s);
            }
            TypeKind::ObjectProperty => {
                self.object_properties.insert(s);
            }
            TypeKind::DataProperty => {
                self.data_properties.insert(s);
            }
            TypeKind::Datatype => {
                self.data_ranges.insert(s);
            }
            TypeKind::Functional => {
                if self.object_properties.contains(&s) {
                    self.translate_property(s);
                    self.consume(s, p, o);
                }
                if self.data_properties.contains(&s) {
                    self.consume(s, p, o);
                }
            }
            TypeKind::InverseFunctional => {
                if self.object_properties.contains(&s) {
                    self.translate_property(s);
                    self.consume(s, p, o);
                }
            }
            TypeKind::Transitive => {
                self.translate_property(s);
                self.consume(s, p, o);
            }
            TypeKind::Restriction => {
                self.consume(s, p, o);
                self.restrictions.insert(s);
                self.class_expressions.insert(s);
            }
            TypeKind::List | TypeKind::DeprecatedProperty | TypeKind::Property | TypeKind::SwrlAtomList
            | TypeKind::SwrlBuiltin => {
                self.consume(s, p, o);
            }
            TypeKind::AnnotationProperty => {
                if !self.blank(s) {
                    self.consume(s, p, o);
                }
                self.annotation_properties.insert(s);
            }
            TypeKind::DeprecatedClass => {
                self.class_expressions.insert(s);
                self.consume(s, p, o);
            }
            TypeKind::Ontology => {
                self.consume(s, p, o);
                self.ontologies.insert(s);
            }
            TypeKind::RdfsClass => {
                self.class_expressions.insert(s);
                self.consume(s, p, o);
                let c = self.v.owl_class;
                self.handle_resource(s, p, c);
            }
            TypeKind::SelfRestriction => {
                self.consume(s, p, o);
                self.restrictions.insert(s);
                let has_self = self.v.owl_hasself;
                let t = self.lit("true", None, Some("http://www.w3.org/2001/XMLSchema#boolean"));
                self.add_lit(s, has_self, t);
            }
            TypeKind::NamedIndividual => {
                self.individuals.insert(s);
            }
            TypeKind::Annotation => {
                self.annotations.insert(s);
            }
            TypeKind::SwrlBuiltinAtom => {
                self.swrl_builtin_atoms.insert(s);
                self.consume(s, p, o);
            }
            TypeKind::SwrlClassAtom => {
                self.swrl_class_atoms.insert(s);
                self.consume(s, p, o);
            }
            TypeKind::SwrlDataRangeAtom => {
                self.swrl_data_range_atoms.insert(s);
                self.consume(s, p, o);
            }
            TypeKind::SwrlDataValuedPropertyAtom => {
                self.consume(s, p, o);
                self.swrl_data_valued_property_atoms.insert(s);
            }
            TypeKind::SwrlDifferentIndividualsAtom => {
                self.swrl_different_from_atoms.insert(s);
                self.consume(s, p, o);
            }
            TypeKind::SwrlImp => {
                let r = self.remap(s);
                self.consume(r, p, o);
                self.swrl_rules.insert_with(r, || ());
            }
            TypeKind::SwrlIndividualPropertyAtom => {
                self.consume(s, p, o);
                self.swrl_individual_property_atoms.insert(s);
            }
            TypeKind::SwrlSameIndividualAtom => {
                self.swrl_same_as_atoms.insert(s);
                self.consume(s, p, o);
            }
            TypeKind::SwrlVariable => {
                self.swrl_variables.insert(s);
                self.consume(s, p, o);
            }
        }
    }

    /// A rule named by an IRI is read as a blank node: one fresh node per IRI.
    fn remap(&mut self, t: Term) -> Term {
        if self.blank(t) {
            return t;
        }
        if let Some(&r) = self.remapped.get(&t) {
            return r;
        }
        let label = format!("_:genid{}", self.next_id());
        let r = self.terms.id(&label);
        self.remapped.insert(t, r);
        r
    }

    fn infer_types(&mut self, s: Term, o: Term) {
        if self.class_expressions.contains(&o) {
            self.class_expressions.insert(s);
        } else if self.data_ranges.contains(&o) {
            self.data_ranges.insert(s);
        } else if self.class_expressions.contains(&s) {
            self.class_expressions.insert(o);
        } else if self.data_ranges.contains(&s) {
            self.data_ranges.insert(o);
        }
    }

    fn both_classes(&self, s: Term, o: Term) -> bool {
        self.class_expressions.contains(&s) && self.class_expressions.contains(&o)
    }

    fn both_data_ranges(&self, s: Term, o: Term) -> bool {
        self.data_ranges.contains(&s) && self.data_ranges.contains(&o)
    }

    fn pred_can_stream(&mut self, kind: PredKind, s: Term, p: Term, o: Term) -> bool {
        let anonymous = self.blank(s) || self.blank(o);
        match kind {
            PredKind::DifferentFrom
            | PredKind::SameAs
            | PredKind::Imports
            | PredKind::Rest
            | PredKind::First
            | PredKind::DeclaredAs
            | PredKind::VersionIri => true,
            PredKind::DisjointUnion | PredKind::HasKey => {
                self.class_expressions.insert(s);
                false
            }
            PredKind::DisjointWith => {
                self.class_expressions.insert(s);
                self.class_expressions.insert(o);
                !anonymous && self.both_classes(s, o)
            }
            PredKind::EquivalentClass => {
                self.infer_types(s, o);
                !anonymous && (self.both_classes(s, o) || self.both_data_ranges(s, o))
            }
            PredKind::EquivalentProperty | PredKind::Domain | PredKind::UnionOf | PredKind::OneOf => false,
            PredKind::Range | PredKind::PropertyDisjointWith => {
                self.infer_types(s, o);
                false
            }
            PredKind::SubClassOf => {
                self.class_expressions.insert(s);
                self.class_expressions.insert(o);
                !anonymous
            }
            PredKind::SubPropertyOf => {
                if self.object_properties.contains(&o) {
                    self.object_properties.insert(s);
                } else if self.data_properties.contains(&o) {
                    self.data_properties.insert(o);
                } else if self.is_annotation_property(o) {
                    self.annotation_properties.insert(s);
                } else if self.object_properties.contains(&s) {
                    self.object_properties.insert(o);
                } else if self.data_properties.contains(&s) {
                    self.data_properties.insert(o);
                } else if self.is_annotation_property(s) {
                    self.annotation_properties.insert(o);
                }
                false
            }
            PredKind::IntersectionOf => {
                if self.class_expressions.contains(&s) {
                    self.class_expressions.insert(o);
                } else if self.class_expressions.contains(&o) {
                    self.class_expressions.insert(s);
                } else if self.data_ranges.contains(&s) {
                    self.data_ranges.insert(o);
                } else if self.data_ranges.contains(&o) {
                    self.data_ranges.insert(s);
                }
                false
            }
            PredKind::ComplementOf => {
                self.class_expressions.insert(s);
                self.class_expressions.insert(o);
                false
            }
            PredKind::SomeValuesFrom => {
                self.some_values_from(s, o);
                false
            }
            PredKind::AllValuesFrom => {
                self.restrictions.insert(s);
                let property = self.resource_object(s, self.v.owl_onproperty, false);
                if let Some(property) = property {
                    if (!self.blank(o) || self.translated_classes.contains(&o)) && self.object_property_only(property) {
                        self.class_expressions.insert(o);
                        self.add_res(s, p, o);
                        self.translate_class(s);
                        return true;
                    }
                }
                false
            }
            PredKind::PropertyChainAxiom => {
                self.object_properties.insert(o);
                false
            }
            PredKind::Annotated => {
                self.annotated.entry(o).or_default().insert_with(s, || ());
                self.annotated_declaration(s);
                false
            }
            PredKind::InverseOf => {
                self.object_properties.insert(s);
                self.object_properties.insert(o);
                false
            }
            PredKind::OnProperty => {
                self.restrictions.insert(s);
                false
            }
            PredKind::OnClass => {
                self.class_expressions.insert(o);
                false
            }
            PredKind::OnDataRange => {
                self.data_ranges.insert(o);
                false
            }
            PredKind::DatatypeComplementOf => {
                self.data_ranges.insert(s);
                self.data_ranges.insert(o);
                false
            }
        }
    }

    fn some_values_from(&mut self, s: Term, o: Term) {
        self.restrictions.insert(s);
        if self.data_ranges.contains(&o) {
            if let Some(property) = self.resource_object(s, self.v.owl_onproperty, false) {
                self.data_properties.insert(property);
            }
        }
    }

    /// A reification of an entity declaration declares the entity at once.
    fn annotated_declaration(&mut self, node: Term) {
        let v = &self.v;
        let (ap, at, asrc, ty) = (v.owl_annotatedproperty, v.owl_annotatedtarget, v.owl_annotatedsource, v.rdf_type);
        let entity_types = [
            v.owl_class,
            v.owl_objectproperty,
            v.owl_datatypeproperty,
            v.owl_annotationproperty,
            v.rdfs_datatype,
            v.owl_namedindividual,
        ];
        if self.resource_object(node, ap, false) != Some(ty) {
            return;
        }
        let Some(target) = self.resource_object(node, at, false) else { return };
        let Some(source) = self.resource_object(node, asrc, false) else { return };
        if entity_types.contains(&target) {
            self.handle_resource(source, ty, target);
        }
    }

    // -- what the reader keeps ------------------------------------------------

    fn blank(&self, t: Term) -> bool {
        self.terms.blank[t as usize]
    }

    fn reserved(&self, t: Term) -> bool {
        self.terms.reserved[t as usize]
    }

    fn add_res(&mut self, s: Term, p: Term, o: Term) {
        if let Some(map) = self.single.get_mut(&p) {
            map.insert(s, o);
            return;
        }
        self.res.insert_with(s, JMap::default).insert_with(p, JMap::default).insert_with(o, || ());
    }

    fn add_lit(&mut self, s: Term, p: Term, l: Lit) {
        let objects = self.lit.insert_with(s, JMap::default).insert_with(p, Vec::new);
        if !objects.contains(&l) {
            objects.push(l);
        }
    }

    /// The first object of `s p`, consumed when `consume` says so.
    fn resource_object(&mut self, s: Term, p: Term, consume: bool) -> Option<Term> {
        if let Some(map) = self.single.get_mut(&p) {
            let o = map.get(&s).copied();
            if consume {
                map.remove(&s);
            }
            return o;
        }
        let hashes = &self.terms.hashes;
        let objects = self.res.get_mut(s)?.get_mut(p)?;
        let o = objects.first(hashes)?;
        if consume {
            objects.remove(o);
        }
        Some(o)
    }

    fn literal_object(&mut self, s: Term, p: Term, consume: bool) -> Option<Lit> {
        let map = self.lit.get_mut(s)?;
        let objects = map.get_mut(p)?;
        let l = *objects.first()?;
        if consume {
            objects.remove(0);
            if objects.is_empty() {
                map.remove(p);
            }
        }
        Some(l)
    }

    /// Consume the statement `s p o`.
    fn consume(&mut self, s: Term, p: Term, o: Term) {
        if let Some(map) = self.single.get_mut(&p) {
            map.remove(&s);
            return;
        }
        if let Some(objects) = self.res.get_mut(s).and_then(|m| m.get_mut(p)) {
            objects.remove(o);
        }
    }

    fn consume_literal(&mut self, s: Term, p: Term, l: Lit) {
        let Some(map) = self.lit.get_mut(s) else { return };
        let Some(objects) = map.get_mut(p) else { return };
        if let Some(i) = objects.iter().position(|x| *x == l) {
            objects.remove(i);
            if objects.is_empty() {
                map.remove(p);
                if map.is_empty() {
                    self.lit.remove(s);
                }
            }
        }
    }

    fn has_predicate(&self, s: Term, p: Term) -> bool {
        if let Some(map) = self.single.get(&p) {
            return map.contains_key(&s);
        }
        self.res.get(s).is_some_and(|m| m.contains(p)) || self.lit.get(s).is_some_and(|m| m.contains(p))
    }

    /// The predicates `s` has statements under, in the order a hash set
    /// filled with them iterates.
    fn predicates_of(&self, s: Term) -> Vec<Term> {
        let mut set: JMap<()> = JMap::default();
        if let Some(m) = self.res.get(s) {
            for p in m.order(&self.terms.hashes) {
                set.insert_with(p, || ());
            }
        }
        if let Some(m) = self.lit.get(s) {
            for p in m.order(&self.terms.hashes) {
                set.insert_with(p, || ());
            }
        }
        set.order(&self.terms.hashes)
    }

    fn is_annotation_property(&self, t: Term) -> bool {
        self.annotation_properties.contains(&t) || self.builtin_annotation_properties.contains(&t)
    }

    fn object_property_only(&self, t: Term) -> bool {
        self.object_properties.contains(&t) && !self.data_properties.contains(&t) && !self.annotation_properties.contains(&t)
    }

    fn data_property_only(&self, t: Term) -> bool {
        self.data_properties.contains(&t) && !self.object_properties.contains(&t) && !self.annotation_properties.contains(&t)
    }

    fn annotation_property_only(&self, t: Term) -> bool {
        self.annotation_properties.contains(&t) && !self.object_properties.contains(&t) && !self.data_properties.contains(&t)
    }

    fn class_lax(&self, t: Term) -> bool {
        self.class_expressions.contains(&t) || (self.parsed_all && !self.data_ranges.contains(&t))
    }

    fn data_range_lax(&self, t: Term) -> bool {
        self.parsed_all && self.data_ranges.contains(&t)
    }

    fn is_general(&self, p: Term) -> bool {
        let name = self.name(p);
        !self.reserved(p)
            || self.builtin_annotation_properties.contains(&p)
            || name.starts_with(SWRL)
            || name.starts_with(SWRLB)
    }

    // -- naming ---------------------------------------------------------------

    /// The individual a node names: a blank node takes the next number the
    /// first time it is met.
    fn individual(&mut self, t: Term) {
        if !self.blank(t) || self.named.contains_key(&t) {
            return;
        }
        let n = self.next_id();
        self.named.insert(t, n);
        if self.trace {
            eprintln!("MINT _:genid{n} for {}", self.name(t));
        }
    }

    // -- translating a statement ----------------------------------------------

    /// Translate the statement `s p o` wherever it is met after the parse.
    fn handle_resource(&mut self, s: Term, p: Term, o: Term) {
        if p == self.v.rdf_type {
            if let Some(&kind) = self.builtin_types.get(&o) {
                self.type_handle(kind, s, p, o);
            } else if !self.axiom_types.contains_key(&o) {
                self.individual(s);
                self.translate_class(o);
                self.consume(s, p, o);
            }
            return;
        }
        if let Some(&kind) = self.predicates.get(&p) {
            if self.pred_can_handle(kind, s, o) {
                self.pred_handle(kind, s, p, o);
                return;
            }
        }
        if self.object_assertion_applies(p) {
            self.object_assertion(s, p, o);
        } else if self.annotation_assertion_applies(s, p) {
            self.annotation_assertion(s, p, o);
        }
    }

    fn object_assertion_applies(&self, p: Term) -> bool {
        self.object_properties.contains(&p) && !self.annotation_property_only(p)
    }

    fn object_assertion(&mut self, s: Term, p: Term, o: Term) {
        if self.object_properties.contains(&p) {
            self.consume(s, p, o);
            self.translate_property(p);
            self.individual(s);
            self.individual(o);
        }
    }

    fn annotation_assertion_applies(&self, s: Term, p: Term) -> bool {
        !self.axioms.contains(&s)
            && !self.annotations.contains(&s)
            && (self.builtin_annotation_properties.contains(&p) || !self.reserved(p))
    }

    fn annotation_assertion(&mut self, s: Term, p: Term, o: Term) {
        // The value is named before the subject.
        self.individual(o);
        self.individual(s);
        self.consume(s, p, o);
    }

    fn handle_literal(&mut self, s: Term, p: Term, l: Lit) {
        if self.data_assertion_applies(p) {
            self.individual(s);
            self.consume_literal(s, p, l);
        } else if p == self.v.rdf_first {
            self.first_lit.insert(s, l);
            self.consume_literal(s, p, l);
        } else if self.annotation_literal_applies(s, p) {
            self.annotation_literal(s, p, l);
        }
    }

    fn data_assertion_applies(&self, p: Term) -> bool {
        self.data_properties.contains(&p) && !self.is_annotation_property(p)
    }

    fn annotation_literal_applies(&self, s: Term, p: Term) -> bool {
        if self.axioms.contains(&s) || self.annotations.contains(&s) {
            return false;
        }
        if self.is_annotation_property(p) {
            return true;
        }
        if !self.blank(s) {
            return self.class_lax(s)
                || self.data_range_lax(s)
                || self.object_properties.contains(&s)
                || self.data_properties.contains(&s);
        }
        true
    }

    fn annotation_literal(&mut self, s: Term, p: Term, l: Lit) {
        self.individual(s);
        if self.ontologies.contains(&s) {
            self.translate_annotations(s);
        }
        self.consume_literal(s, p, l);
    }

    fn pred_can_handle(&mut self, kind: PredKind, s: Term, o: Term) -> bool {
        match kind {
            PredKind::VersionIri => return true,
            PredKind::OnClass | PredKind::OnDataRange | PredKind::DatatypeComplementOf => return false,
            _ => {}
        }
        self.infer_types(s, o);
        match kind {
            PredKind::DisjointUnion => !self.blank(s) && self.class_expressions.contains(&s),
            PredKind::DisjointWith | PredKind::SubClassOf => self.both_classes(s, o),
            PredKind::EquivalentClass => {
                self.infer_types(s, o);
                self.both_classes(s, o) || self.both_data_ranges(s, o)
            }
            PredKind::IntersectionOf | PredKind::UnionOf | PredKind::ComplementOf | PredKind::OneOf => !self.blank(s),
            PredKind::PropertyDisjointWith => {
                self.infer_types(s, o);
                (self.object_properties.contains(&s) && self.object_properties.contains(&o))
                    || (self.data_properties.contains(&s) && self.data_properties.contains(&o))
            }
            PredKind::InverseOf => self.object_properties.contains(&s) && self.object_properties.contains(&o),
            _ => true,
        }
    }

    fn pred_handle(&mut self, kind: PredKind, s: Term, p: Term, o: Term) {
        match kind {
            PredKind::DifferentFrom | PredKind::SameAs => {
                self.individual(s);
                self.individual(o);
                self.consume(s, p, o);
            }
            PredKind::DisjointUnion => {
                if !self.blank(s) {
                    self.translate_class(s);
                    self.translate_class_list(o);
                    self.consume(s, p, o);
                }
            }
            PredKind::DisjointWith => {
                self.translate_class(s);
                self.translate_class(o);
                self.consume(s, p, o);
            }
            PredKind::EquivalentClass => {
                if self.class_lax(s) && self.class_lax(o) {
                    self.translate_class(s);
                    self.translate_class(o);
                    self.consume(s, p, o);
                } else if self.data_range_lax(s) || self.data_range_lax(o) {
                    self.translate_data_range(o);
                    self.consume(s, p, o);
                }
            }
            PredKind::EquivalentProperty => {
                if self.object_properties.contains(&s) && self.object_properties.contains(&o) {
                    self.translate_property(s);
                    self.translate_property(o);
                    self.consume(s, p, o);
                }
                if self.data_properties.contains(&s) && self.data_properties.contains(&o) {
                    self.consume(s, p, o);
                }
            }
            PredKind::Domain => {
                if self.object_properties.contains(&s) {
                    self.translate_property(s);
                    self.translate_class(o);
                    self.consume(s, p, o);
                } else if self.data_properties.contains(&s) {
                    self.translate_class(o);
                    self.consume(s, p, o);
                } else if self.is_annotation_property(s) && !self.blank(o) {
                    self.consume(s, p, o);
                } else {
                    self.annotation_properties.insert(s);
                    self.consume(s, p, o);
                }
            }
            PredKind::Range => self.range(s, p, o),
            PredKind::SubClassOf => {
                if self.class_lax(s) && self.class_lax(o) {
                    self.translate_class(s);
                    self.translate_class(o);
                    self.consume(s, p, o);
                }
            }
            PredKind::SubPropertyOf => self.sub_property(s, p, o),
            PredKind::Imports => {
                self.consume(s, p, o);
                self.ontologies.insert(s);
                self.ontologies.insert(o);
            }
            PredKind::IntersectionOf | PredKind::UnionOf | PredKind::ComplementOf | PredKind::OneOf => {
                self.consume(s, p, o);
                self.translate_class(s);
                match kind {
                    PredKind::IntersectionOf | PredKind::UnionOf => self.translate_class_list(o),
                    PredKind::ComplementOf => self.translate_class(o),
                    _ => self.translate_individual_list(o),
                }
            }
            PredKind::SomeValuesFrom => self.some_values_from(s, o),
            PredKind::Rest => {
                if o != self.v.rdf_nil {
                    self.rest.insert(s, o);
                }
                self.consume(s, p, o);
            }
            PredKind::First => {
                self.first_res.insert(s, o);
                self.consume(s, p, o);
            }
            PredKind::HasKey => {
                if self.class_expressions.contains(&s) {
                    self.consume(s, p, o);
                    self.translate_class(s);
                    self.translate_list(o, Item::Key);
                }
            }
            PredKind::VersionIri => self.consume(s, p, o),
            PredKind::PropertyChainAxiom => {
                self.translate_property(s);
                self.translate_list(o, Item::ObjectProperty);
                self.consume(s, p, o);
            }
            PredKind::PropertyDisjointWith => {
                if self.data_properties.contains(&s) && self.data_properties.contains(&o) {
                    self.consume(s, p, o);
                }
                if self.object_properties.contains(&s) && self.object_properties.contains(&o) {
                    self.translate_property(s);
                    self.translate_property(o);
                    self.consume(s, p, o);
                }
            }
            PredKind::InverseOf => {
                if self.inverse_axioms && self.object_properties.contains(&s) && self.object_properties.contains(&o) {
                    self.translate_property(s);
                    self.translate_property(o);
                    self.consume(s, p, o);
                }
            }
            PredKind::DeclaredAs
            | PredKind::AllValuesFrom
            | PredKind::Annotated
            | PredKind::OnProperty
            | PredKind::OnClass
            | PredKind::OnDataRange
            | PredKind::DatatypeComplementOf => {}
        }
    }

    fn range(&mut self, s: Term, p: Term, o: Term) {
        let object_range = |this: &mut Self| {
            this.translate_property(s);
            this.translate_class(o);
            this.consume(s, p, o);
        };
        let data_range = |this: &mut Self| {
            this.translate_data_range(o);
            this.consume(s, p, o);
        };
        if self.object_property_only(s) && self.class_expressions.contains(&o) {
            object_range(self);
        } else if self.data_property_only(s) && self.data_ranges.contains(&o) {
            data_range(self);
        } else if (self.is_annotation_property(s) || self.annotation_property_only(s)) && !self.blank(o) {
            self.consume(s, p, o);
        } else if self.class_lax(o) {
            self.object_properties.insert(s);
            object_range(self);
        } else if self.data_range_lax(o) {
            self.data_properties.insert(s);
            data_range(self);
        } else if self.object_properties.contains(&s) {
            object_range(self);
        } else if self.data_properties.contains(&s) {
            data_range(self);
        } else {
            self.annotation_properties.insert(s);
            self.consume(s, p, o);
        }
    }

    fn sub_property(&mut self, s: Term, p: Term, o: Term) {
        let (chain, first) = (self.v.owl_propertychain, self.v.rdf_first);
        if self.has_predicate(s, chain) {
            if let Some(list) = self.resource_object(s, chain, true) {
                self.translate_list(list, Item::ObjectProperty);
            }
            self.translate_property(o);
            self.consume(s, p, o);
        } else if self.has_predicate(s, first) {
            self.translate_list(s, Item::ObjectProperty);
            self.translate_property(o);
            self.consume(s, p, o);
        } else if self.object_properties.contains(&s) && self.object_properties.contains(&o) {
            self.translate_property(s);
            self.translate_property(o);
            self.consume(s, p, o);
        } else if self.data_properties.contains(&s) && self.data_properties.contains(&o) {
            self.consume(s, p, o);
        } else {
            if self.object_properties.contains(&o) {
                self.translate_property(s);
                self.translate_property(o);
            }
            self.consume(s, p, o);
        }
    }

    // -- the end of the document ----------------------------------------------

    pub(crate) fn end(&mut self) -> Names {
        if self.trace {
            eprintln!("END-MODEL");
        }
        self.parsed_all = true;
        for rule in self.swrl_rules.order(&self.terms.hashes) {
            self.translate_rule(rule);
        }
        let range = self.v.rdfs_range;
        // Ranges first, every statement inferring its terms' kinds on the way.
        self.each_resource(|this, s, p, o| {
            this.infer_types(s, o);
            if p == range {
                this.range(s, p, o);
            }
        });
        // Property assertions and annotations.
        self.each_resource(|this, s, p, o| {
            if this.is_general(p) {
                if this.object_assertion_applies(p) {
                    this.object_assertion(s, p, o);
                } else if this.annotation_assertion_applies(s, p) {
                    this.annotation_assertion(s, p, o);
                }
            }
        });
        self.each_literal(|this, s, p, l| {
            if this.is_general(p) {
                if this.data_assertion_applies(p) {
                    this.individual(s);
                    this.consume_literal(s, p, l);
                } else if p == this.v.rdf_first {
                    this.first_lit.insert(s, l);
                    this.consume_literal(s, p, l);
                } else if this.annotation_literal_applies(s, p) {
                    this.annotation_literal(s, p, l);
                }
            }
        });
        // The axiom nodes.
        let ty = self.v.rdf_type;
        self.each_resource(|this, s, p, o| {
            if p == ty {
                if let Some(&kind) = this.axiom_types.get(&o) {
                    this.axiom_node(kind, s, p, o);
                }
            }
        });
        // Everything left.
        self.each_resource(|this, s, p, o| this.handle_resource(s, p, o));
        self.each_literal(|this, s, p, l| this.handle_literal(s, p, l));
        self.inverse_axioms = true;
        let inverse = self.v.owl_inverseof;
        self.each_resource(|this, s, p, o| {
            if p == inverse && this.object_properties.contains(&s) && this.object_properties.contains(&o) {
                this.pred_handle(PredKind::InverseOf, s, p, o);
            }
        });
        Names {
            individuals: self.named.iter().map(|(t, n)| (self.name(*t).to_string(), *n)).collect(),
            axiom_nodes: self.axiom_nodes.iter().map(|t| self.name(*t).to_string()).collect(),
            annotation_nodes: self.annotation_order.iter().map(|t| self.name(*t).to_string()).collect(),
            next: self.next,
        }
    }

    /// Walk the filed resource statements: subjects, then each subject's
    /// predicates, in hash order, then a copy of each predicate's objects.
    fn each_resource(&mut self, mut f: impl FnMut(&mut Self, Term, Term, Term)) {
        let subjects = self.res.order(&self.terms.hashes);
        for s in subjects {
            let predicates = match self.res.get(s) {
                Some(m) => m.order(&self.terms.hashes),
                None => continue,
            };
            for p in predicates {
                let objects = match self.res.get(s).and_then(|m| m.get(p)) {
                    Some(objects) => objects.order(&self.terms.hashes),
                    None => continue,
                };
                for o in objects {
                    f(self, s, p, o);
                }
            }
        }
    }

    fn each_literal(&mut self, mut f: impl FnMut(&mut Self, Term, Term, Lit)) {
        let subjects = self.lit.order(&self.terms.hashes);
        for s in subjects {
            let predicates = match self.lit.get(s) {
                Some(m) => m.order(&self.terms.hashes),
                None => continue,
            };
            for p in predicates {
                let objects: Vec<Lit> = self.lit.get(s).and_then(|m| m.get(p)).cloned().unwrap_or_default();
                for l in objects {
                    f(self, s, p, l);
                }
            }
        }
    }

    fn axiom_node(&mut self, kind: AxiomKind, s: Term, p: Term, o: Term) {
        let v = &self.v;
        match kind {
            AxiomKind::Axiom => {
                let (asrc, aprop, atgt) = (v.owl_annotatedsource, v.owl_annotatedproperty, v.owl_annotatedtarget);
                let (rsub, rpred, robj, chain) = (v.rdf_subject, v.rdf_predicate, v.rdf_object, v.owl_propertychain);
                let source = self.resource_object(s, asrc, true).or_else(|| self.resource_object(s, rsub, true));
                let property = self.resource_object(s, aprop, true).or_else(|| self.resource_object(s, rpred, true));
                let target = self
                    .resource_object(s, atgt, true)
                    .or_else(|| self.resource_object(s, robj, true))
                    .or_else(|| self.resource_object(s, chain, true));
                let target_literal = match target {
                    Some(_) => None,
                    None => self.literal_object(s, atgt, true).or_else(|| self.literal_object(s, robj, true)),
                };
                if let (Some(source), Some(property)) = (source, property) {
                    self.axiom_nodes.push(s);
                    self.translate_annotations(s);
                    if let Some(target) = target {
                        self.handle_resource(source, property, target);
                    } else if let Some(l) = target_literal {
                        self.handle_literal(source, property, l);
                    }
                }
                self.consume(s, p, o);
            }
            AxiomKind::AllDifferent => {
                let (members, distinct) = (v.owl_members, v.owl_distinctmembers);
                if self.resource_object(s, members, false).is_none() && self.resource_object(s, distinct, false).is_none() {
                    return;
                }
                let list = self.resource_object(s, members, true).or_else(|| self.resource_object(s, distinct, true));
                if let Some(list) = list {
                    self.axiom_nodes.push(s);
                    self.translate_individual_list(list);
                    self.translate_annotations(s);
                    self.consume(s, p, o);
                }
            }
            AxiomKind::AllDisjointClasses => {
                let members = v.owl_members;
                if self.resource_object(s, members, false).is_none() {
                    return;
                }
                if let Some(list) = self.resource_object(s, members, true) {
                    self.axiom_nodes.push(s);
                    self.translate_class_list(list);
                    self.translate_annotations(s);
                    self.consume(s, p, o);
                }
            }
            AxiomKind::AllDisjointProperties => {
                let members = v.owl_members;
                self.axiom_nodes.push(s);
                self.consume(s, p, o);
                if let Some(list) = self.resource_object(s, members, true) {
                    let first = self.first_res.get(&list).copied();
                    let object = first.is_some_and(|f| self.object_properties.contains(&f));
                    self.translate_annotations(s);
                    if object {
                        self.translate_list(list, Item::ObjectProperty);
                    } else {
                        self.translate_list(list, Item::Ignore);
                    }
                }
            }
            AxiomKind::NegativePropertyAssertion => {
                let (si, ap, ti, tv) =
                    (v.owl_sourceindividual, v.owl_assertionproperty, v.owl_targetindividual, v.owl_targetvalue);
                let (rsub, rpred, robj) = (v.rdf_subject, v.rdf_predicate, v.rdf_object);
                let source = self.resource_object(s, si, true).or_else(|| self.resource_object(s, rsub, true));
                let property = self.resource_object(s, ap, true).or_else(|| self.resource_object(s, rpred, true));
                let target = self.resource_object(s, ti, true);
                let target_literal = match target {
                    Some(_) => None,
                    None => self.literal_object(s, tv, true),
                };
                let (target, target_literal) = match (target, target_literal) {
                    (None, None) => {
                        let t = self.resource_object(s, robj, true);
                        let l = if t.is_none() { self.literal_object(s, robj, true) } else { None };
                        (t, l)
                    }
                    other => other,
                };
                let (Some(source), Some(property)) = (source, property) else { return };
                self.axiom_nodes.push(s);
                self.translate_annotations(s);
                if target_literal.is_some() {
                    self.individual(source);
                    self.consume(s, p, o);
                } else if let Some(target) = target {
                    self.individual(source);
                    self.translate_property(property);
                    self.individual(target);
                    self.consume(s, p, o);
                }
            }
        }
    }

    // -- translating ----------------------------------------------------------

    /// Translate the annotations on `node`: first those on the annotation
    /// nodes that annotate it, then its own, predicate by predicate in hash
    /// order, node values before literal ones.
    fn translate_annotations(&mut self, node: Term) {
        let annotating = self.annotated.get(&node).map(|set| set.order(&self.terms.hashes)).unwrap_or_default();
        for a in annotating {
            if self.annotation_seen.insert(a) {
                self.annotation_order.push(a);
            }
            self.translate_annotations(a);
        }
        for p in self.predicates_of(node) {
            let is_ap = self.is_annotation_property(p)
                || (!self.object_properties.contains(&p) && !self.data_properties.contains(&p) && !self.reserved(p));
            if !is_ap {
                continue;
            }
            while let Some(value) = self.resource_object(node, p, true) {
                self.individual(value);
            }
            while let Some(l) = self.literal_object(node, p, true) {
                self.consume_literal(node, p, l);
            }
        }
    }

    fn translate_property(&mut self, t: Term) {
        if !self.translated_properties.insert(t) {
            return;
        }
        if self.blank(t) {
            if let Some(inverse) = self.resource_object(t, self.v.owl_inverseof, true) {
                self.translate_property(inverse);
            }
            self.object_properties.insert(t);
        }
    }

    fn translate_class(&mut self, n: Term) {
        if !self.translated_classes.insert(n) {
            return;
        }
        let v = &self.v;
        if !self.blank(n) {
            return;
        }
        let (inter, union, compl, oneof) = (v.owl_intersectionof, v.owl_unionof, v.owl_complementof, v.owl_oneof);
        let (svf, avf, hasvalue, onp, hasself) =
            (v.owl_somevaluesfrom, v.owl_allvaluesfrom, v.owl_hasvalue, v.owl_onproperty, v.owl_hasself);
        if self.resource_object(n, inter, false).is_some() {
            if let Some(list) = self.resource_object(n, inter, true) {
                self.translate_class_list(list);
            }
            return;
        }
        if self.resource_object(n, union, false).is_some() {
            if let Some(list) = self.resource_object(n, union, true) {
                self.translate_class_list(list);
            }
            return;
        }
        if self.resource_object(n, compl, false).is_some() && self.class_lax(n) {
            if let Some(c) = self.resource_object(n, compl, true) {
                self.translate_class(c);
            }
            return;
        }
        if self.resource_object(n, oneof, false).is_some() {
            if let Some(list) = self.resource_object(n, oneof, true) {
                self.translate_individual_list(list);
            }
            return;
        }
        for filler in [svf, avf] {
            let object = self.resource_object(n, filler, false);
            let on = self.resource_object(n, onp, false).is_some();
            if (object.is_some_and(|f| self.class_lax(f)) && on) || (self.object_properties.contains(&n) && object.is_some()) {
                if let Some(prop) = self.resource_object(n, onp, true) {
                    self.translate_property(prop);
                }
                if let Some(f) = self.resource_object(n, filler, true) {
                    self.translate_class(f);
                }
                return;
            }
        }
        let on = self.resource_object(n, onp, false);
        if on.is_some() && self.resource_object(n, hasvalue, false).is_some() {
            if let Some(value) = self.resource_object(n, hasvalue, true) {
                self.individual(value);
            }
            if let Some(prop) = self.resource_object(n, onp, true) {
                self.translate_property(prop);
            }
            return;
        }
        if on.is_some() && self.literal_object(n, hasself, false).is_some() {
            self.literal_object(n, hasself, true);
            if let Some(prop) = self.resource_object(n, onp, true) {
                self.translate_property(prop);
            }
            return;
        }
        for (cardinality, qualified, filler) in self.object_cardinalities.clone() {
            let on_object = on.is_some_and(|p| self.object_properties.contains(&p));
            let filler_ok = !qualified || self.resource_object(n, filler, false).is_some_and(|f| self.class_lax(f));
            if self.non_negative_integer(n, cardinality) && on_object && filler_ok {
                self.literal_object(n, cardinality, true);
                if let Some(prop) = self.resource_object(n, onp, true) {
                    self.translate_property(prop);
                }
                if let Some(f) = self.resource_object(n, filler, true) {
                    self.translate_class(f);
                }
                return;
            }
        }
        for filler in [svf, avf] {
            let object = self.resource_object(n, filler, false);
            let on_data = on.is_some_and(|p| self.data_properties.contains(&p));
            if (object.is_some_and(|f| self.data_range_lax(f)) && on.is_some()) || (on_data && object.is_some()) {
                self.resource_object(n, onp, true);
                if let Some(f) = self.resource_object(n, filler, true) {
                    self.translate_data_range(f);
                }
                return;
            }
        }
        if self.literal_object(n, hasvalue, false).is_some() {
            self.literal_object(n, hasvalue, true);
            self.resource_object(n, onp, true);
            return;
        }
        for (cardinality, qualified, filler) in self.data_cardinalities.clone() {
            let on_data = on.is_some_and(|p| self.data_properties.contains(&p));
            let filler_ok = !qualified || self.resource_object(n, filler, false).is_some_and(|f| self.data_range_lax(f));
            if self.non_negative_integer(n, cardinality) && on_data && filler_ok {
                self.literal_object(n, cardinality, true);
                self.resource_object(n, onp, true);
                if let Some(f) = self.resource_object(n, filler, true) {
                    self.translate_data_range(f);
                }
                return;
            }
        }
    }

    fn non_negative_integer(&mut self, n: Term, p: Term) -> bool {
        let Some(l) = self.literal_object(n, p, false) else { return false };
        let value = self.lits[l as usize].value.trim();
        let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    }

    fn translate_data_range(&mut self, n: Term) {
        let v = &self.v;
        if !self.blank(n) && self.data_ranges.contains(&n) {
            return;
        }
        let (inter, union, dcompl, compl, oneof) =
            (v.owl_intersectionof, v.owl_unionof, v.owl_datatypecomplementof, v.owl_complementof, v.owl_oneof);
        let (ondt, withr) = (v.owl_ondatatype, v.owl_withrestrictions);
        if let Some(list) = self.resource_object(n, inter, true) {
            self.translate_list(list, Item::DataRange);
            return;
        }
        if let Some(list) = self.resource_object(n, union, true) {
            self.translate_list(list, Item::DataRange);
            return;
        }
        let complement = self.resource_object(n, dcompl, true).or_else(|| self.resource_object(n, compl, true));
        if let Some(c) = complement {
            self.translate_data_range(c);
            return;
        }
        if let Some(list) = self.resource_object(n, oneof, true) {
            self.translate_list(list, Item::Ignore);
            return;
        }
        if let Some(dt) = self.resource_object(n, ondt, true) {
            if self.blank(dt) {
                return;
            }
            self.translate_data_range(dt);
            if let Some(list) = self.resource_object(n, withr, true) {
                self.translate_list(list, Item::Facet);
            } else {
                for facet in self.facets.clone() {
                    while let Some(l) = self.literal_object(n, facet, true) {
                        self.consume_literal(n, facet, l);
                    }
                }
            }
        }
    }

    fn translate_class_list(&mut self, list: Term) {
        self.translate_list(list, Item::Class);
    }

    fn translate_individual_list(&mut self, list: Term) {
        self.translate_list(list, Item::Individual);
    }

    /// Walk an RDF list from `list`, consuming its cells and translating each
    /// member. A list named by `rdf:nodeID` is walked once.
    fn translate_list(&mut self, list: Term, item: Item) {
        let shared = self.name(list).contains("_:genid-nodeid-");
        if shared && !self.shared_lists.insert(list) {
            return;
        }
        let mut current = Some(list);
        while let Some(cell) = current {
            if let Some(first) = self.first_res.remove(&cell) {
                match item {
                    Item::Class => {
                        self.class_expressions.insert(first);
                        self.translate_class(first);
                    }
                    Item::Individual => self.individual(first),
                    Item::ObjectProperty => {
                        self.object_properties.insert(first);
                        self.translate_property(first);
                    }
                    Item::DataRange => self.translate_data_range(first),
                    Item::Facet => {
                        for facet in self.facets.clone() {
                            if self.literal_object(first, facet, true).is_some() {
                                break;
                            }
                        }
                    }
                    Item::Atom => self.translate_atom(first),
                    Item::Key | Item::Ignore => {}
                }
            }
            current = self.rest.remove(&cell);
        }
    }

    fn translate_rule(&mut self, rule: Term) {
        let r = self.remap(rule);
        for p in self.predicates_of(r) {
            if self.is_annotation_property(p) {
                while self.literal_object(r, p, true).is_some() {}
            }
        }
        let (head, body) = (self.v.swrl_head, self.v.swrl_body);
        if let Some(list) = self.resource_object(r, head, true) {
            self.translate_list(list, Item::Atom);
        }
        if let Some(list) = self.resource_object(r, body, true) {
            self.translate_list(list, Item::Atom);
        }
    }

    fn translate_atom(&mut self, a: Term) {
        let v = &self.v;
        let (arg1, arg2) = (v.swrl_argument1, v.swrl_argument2);
        if self.swrl_builtin_atoms.contains(&a) {
            let (b, args) = (v.swrl_builtin, v.swrl_arguments);
            self.resource_object(a, b, true);
            if let Some(list) = self.resource_object(a, args, true) {
                self.translate_list(list, Item::Ignore);
            }
        } else if self.swrl_class_atoms.contains(&a) {
            let cp = v.swrl_classpredicate;
            self.atom_individual(a, arg1);
            if let Some(c) = self.resource_object(a, cp, true) {
                self.translate_class(c);
            }
        } else if self.swrl_data_range_atoms.contains(&a) {
            let dr = v.swrl_datarange;
            self.atom_data(a, arg1);
            if let Some(d) = self.resource_object(a, dr, true) {
                self.translate_data_range(d);
            }
        } else if self.swrl_data_valued_property_atoms.contains(&a) {
            let pp = v.swrl_propertypredicate;
            self.atom_individual(a, arg1);
            self.atom_data(a, arg2);
            self.resource_object(a, pp, true);
        } else if self.swrl_individual_property_atoms.contains(&a) {
            let pp = v.swrl_propertypredicate;
            self.atom_individual(a, arg1);
            self.atom_individual(a, arg2);
            if let Some(prop) = self.resource_object(a, pp, true) {
                self.translate_property(prop);
            }
        } else if self.swrl_same_as_atoms.contains(&a) || self.swrl_different_from_atoms.contains(&a) {
            self.atom_individual(a, arg1);
            self.atom_individual(a, arg2);
        }
    }

    fn atom_individual(&mut self, a: Term, p: Term) {
        if let Some(arg) = self.resource_object(a, p, true) {
            if !self.swrl_variables.contains(&arg) {
                self.individual(arg);
            }
        }
    }

    fn atom_data(&mut self, a: Term, p: Term) {
        if self.resource_object(a, p, true).is_none() {
            self.literal_object(a, p, true);
        }
    }
}

/// What a list's members are read as.
#[derive(Clone, Copy)]
enum Item {
    Class,
    Individual,
    ObjectProperty,
    DataRange,
    Facet,
    Atom,
    Key,
    Ignore,
}

impl Sink for Order {
    fn next_id(&mut self) -> u64 {
        Order::next_id(self)
    }

    fn resource(&mut self, s: &str, p: &str, o: &str) -> Result<()> {
        Order::resource(self, s, p, o);
        Ok(())
    }

    fn literal(&mut self, s: &str, p: &str, value: &str, lang: Option<&str>, datatype: Option<&str>) -> Result<()> {
        Order::literal(self, s, p, value, lang, datatype);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order_of(keys: &[String], map: &JMap<()>) -> String {
        let hashes: Vec<i32> = keys.iter().map(|k| java_string_hash(k)).collect();
        map.order(&hashes).into_iter().map(|k| keys[k as usize].as_str()).collect::<Vec<_>>().join(" ")
    }

    /// A table iterates its keys as a `java.util.HashMap<String, _>` holding
    /// them does on Java 21: by bucket over the table at its largest, in
    /// insertion order within a bucket (`Aa` and `BB` share a hash), and a key
    /// removed and put back at the end of its bucket.
    #[test]
    fn a_table_iterates_as_a_java_hash_map() {
        let keys: Vec<String> = (0..30).map(|i| format!("k{i}")).chain(["Aa".into(), "BB".into()]).collect();
        let mut map = JMap::default();
        for k in 0..30 {
            map.insert_with(k, || ());
        }
        map.remove(3);
        map.remove(10);
        for k in [3, 30, 31] {
            map.insert_with(k, || ());
        }
        assert_eq!(
            order_of(&keys, &map),
            "Aa BB k11 k13 k12 k15 k14 k17 k16 k19 k18 k0 k1 k2 k20 k3 k4 k5 k22 k6 k21 k7 k24 k8 k23 k9 k26 k25 k28 k27 k29"
        );

        let keys: Vec<String> = (0..13).map(|i| format!("n{i}")).collect();
        let mut map = JMap::default();
        for k in 0..13 {
            map.insert_with(k, || ());
        }
        for k in 0..10 {
            map.remove(k);
        }
        map.insert_with(0, || ());
        assert_eq!(order_of(&keys, &map), "n0 n10 n12 n11");
    }
}
