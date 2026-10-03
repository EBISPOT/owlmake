//! The Manchester syntax (`.omn`) writer.
//!
//! A document is its prefixes, the ontology header (IRI, version IRI, imports,
//! ontology annotations), then one frame per entity of the signature — every
//! annotation property, datatype, object property (followed by a frame for its
//! inverse when axioms are stated about that), data property, class and named
//! individual, each kind sorted by how the entity is written, then the
//! anonymous individuals — and last the axioms no frame holds: n-ary
//! disjointness and equivalence, rules, and general class axioms.
//!
//! A frame lists the entity's annotation assertions and then its sections
//! (`SubClassOf:`, `Domain:`, `Characteristics:`, …). Each axiom in a section
//! is preceded by its own annotations. Within a section, items follow the
//! natural order of the axioms that state them ([`crate::io::natural_order`]).
//!
//! An entity is written by prefixed name when a declared prefix's namespace is
//! the IRI's namespace, or when a declared namespace starts the IRI and leaves a
//! QName; otherwise as `<IRI>`. An IRI that is not an entity — an annotation
//! value, an annotation property's domain — is always written as `<IRI>`.
//!
//! Some axioms have no place in a Manchester document — an annotation assertion
//! on an IRI that names no entity, a general axiom whose two sides are both
//! anonymous — and some sections carry no axiom annotations (`InverseOf:`,
//! `DisjointUnionOf:`, an annotation property's `SubPropertyOf:`/`Domain:`/
//! `Range:`, a datatype's `EquivalentTo:`, `SameAs:`/`DifferentFrom:`). What a
//! write leaves out is reported.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;

use anyhow::Result;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, Atom, ClassExpression as CE,
    Component, DArgument, DataRange as DR, IArgument, Individual, Literal, ObjectPropertyExpression as OPE,
    PropertyExpression, RcStr, SubObjectPropertyExpression as SOPE,
};
use horned_owl::vocab::Facet;

use crate::io::entities::{self, Kind};
use crate::io::natural_order::{iri_split, sorted_set, str_cmp, NaturalOrder};
use crate::model::Model;

type AC = AnnotatedComponent<RcStr>;

/// A keyword, and the test for what is written under it.
type Accept<T> = (&'static str, fn(&T) -> bool);

const OWL_NS: &str = "http://www.w3.org/2002/07/owl#";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
const XSD_FLOAT: &str = "http://www.w3.org/2001/XMLSchema#float";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
const SWRLB_NS: &str = "http://www.w3.org/2003/11/swrlb#";

/// The built-in functions a rule writes as `swrlb:name`.
const SWRL_BUILTINS: &[&str] = &[
    "equal", "notEqual", "lessThan", "lessThanOrEqual", "greaterThan", "greaterThanOrEqual", "add", "subtract",
    "multiply", "divide", "integerDivide", "mod", "pow", "unaryMinus", "unaryPlus", "abs", "ceiling", "floor",
    "round", "roundHalfToEven", "sin", "cos", "tan", "booleanNot", "stringEqualIgnoreCase", "stringConcat",
    "substring", "stringLength", "normalizeSpace", "upperCase", "lowerCase", "translate", "contains",
    "containsIgnoreCase", "startsWith", "endsWith", "substringBefore", "substringAfter", "matchesLax", "replace",
    "tokenize", "yearMonthDuration", "dayTimeDuration", "dateTime", "date", "time", "subtractDates",
    "subtractTimes", "resolveURI", "anyURI", "addYearMonthDurations", "subtractYearMonthDurations",
    "multiplyYearMonthDurations", "divideYearMonthDurations", "addDayTimeDurations", "subtractDayTimeDurations",
    "multiplyDayTimeDurations", "divideDayTimeDurations", "addDayTimeDurationToDateTime",
    "subtractYearMonthDurationFromDateTime", "subtractDayTimeDurationFromDateTime", "addYearMonthDurationToDate",
    "addDayTimeDurationToDate", "subtractYearMonthDurationFromDate", "subtractDayTimeDurationFromDate",
    "addDayTimeDurationToTime", "subtractDayTimeDurationFromTime", "subtractDateTimesYieldingYearMonthDuration",
    "subtractDateTimesYieldingDayTimeDuration",
];

fn utf16_len(s: &str) -> i64 {
    if s.is_ascii() {
        s.len() as i64
    } else {
        s.encode_utf16().count() as i64
    }
}

// === Text ================================================================

/// The text being written, with the column bookkeeping that indentation is
/// measured by: a new line is indented by the tab on top of the stack, and a
/// list continued on new lines can push the column it started at.
struct Out {
    text: String,
    /// Length so far, in UTF-16 code units.
    pos: i64,
    /// Position of the last line break written.
    last_nl: i64,
    tabs: Vec<i64>,
    /// Whether a new line is indented.
    tabbing: bool,
    /// Whether an intersection breaks its line before each `and`.
    wrapping: bool,
}

impl Out {
    fn new() -> Out {
        Out { text: String::new(), pos: 0, last_nl: -1, tabs: vec![0], tabbing: true, wrapping: true }
    }

    fn write(&mut self, s: &str) {
        if let Some(i) = s.find('\n') {
            self.last_nl = self.pos + utf16_len(&s[..i]);
        }
        self.pos += utf16_len(s);
        self.text.push_str(s);
    }

    fn space(&mut self) {
        self.write(" ");
    }

    fn tab(&self) -> i64 {
        self.tabs.last().copied().unwrap_or(0)
    }

    fn newline(&mut self) {
        self.write("\n");
        if self.tabbing {
            let n = self.tab().max(0) as usize;
            self.write(&" ".repeat(n));
        }
    }

    fn push_tab(&mut self, n: i64) {
        self.tabs.push(n);
    }

    fn increment_tab(&mut self, n: i64) {
        let base = self.tab();
        self.tabs.push(base + n);
    }

    fn pop_tab(&mut self) {
        self.tabs.pop();
    }

    /// The column the next character lands in, less one.
    fn indent(&self) -> i64 {
        self.pos - self.last_nl - 2
    }

    /// A keyword between spaces: ` some `.
    fn keyword(&mut self, kw: &str) {
        self.write(" ");
        self.write(kw);
        self.write(" ");
    }

    /// A frame or section keyword and its colon: `SubClassOf: `.
    fn section(&mut self, kw: &str) {
        self.write(kw);
        self.write(":");
        self.space();
    }
}

// === Short forms =========================================================

/// Whether `s` is a QName: an NCName, or two joined by one colon.
fn is_qname(s: &str) -> bool {
    use crate::owlapi_hash::{is_ncname_char, is_ncname_start};
    if s.is_empty() {
        return false;
    }
    let mut colon = false;
    let mut in_name = false;
    for c in s.chars() {
        if c == ':' {
            if colon || !in_name {
                return false;
            }
            colon = true;
            in_name = false;
        } else if !in_name {
            if !is_ncname_start(c) {
                return false;
            }
            in_name = true;
        } else if !is_ncname_char(c) {
            return false;
        }
    }
    true
}

/// How an entity's IRI is written.
struct ShortForms {
    /// Prefix name with its colon → namespace, every declared prefix.
    names: Vec<(String, String)>,
    /// Namespace → `prefix:`; a namespace bound twice keeps the name that comes
    /// later in declaration order.
    by_ns: HashMap<String, String>,
    /// The declared namespaces, shortest first, then alphabetically.
    namespaces: Vec<String>,
}

impl ShortForms {
    fn new(prefixes: &[(String, String)]) -> ShortForms {
        let mut names: Vec<(String, String)> = Vec::new();
        let mut put = |name: String, ns: String| match names.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = ns,
            None => names.push((name, ns)),
        };
        for (p, ns) in [("owl", OWL_NS), ("rdfs", RDFS_NS), ("rdf", RDF_NS), ("xsd", XSD_NS), ("xml", XML_NS)] {
            put(format!("{p}:"), ns.to_string());
        }
        for (p, ns) in prefixes {
            put(format!("{p}:"), ns.clone());
        }
        let mut ordered = names.clone();
        ordered.sort_by(|a, b| utf16_len(&a.0).cmp(&utf16_len(&b.0)).then_with(|| str_cmp(&a.0, &b.0)));
        let mut by_ns = HashMap::new();
        for (name, ns) in &ordered {
            by_ns.insert(ns.clone(), name.clone());
        }
        let mut namespaces: Vec<String> = by_ns.keys().cloned().collect();
        namespaces.sort_by(|a, b| utf16_len(a).cmp(&utf16_len(b)).then_with(|| str_cmp(a, b)));
        ShortForms { names, by_ns, namespaces }
    }

    fn prefixed(&self, iri: &str) -> Option<String> {
        let (ns, local) = iri_split(iri);
        if let Some(p) = self.by_ns.get(ns) {
            return Some(format!("{p}{local}"));
        }
        let mut prefixed = None;
        for ns in &self.namespaces {
            if iri.starts_with(ns.as_str()) && is_qname(&iri[ns.len()..]) {
                prefixed = Some(iri.replace(ns.as_str(), &self.by_ns[ns]));
            }
        }
        prefixed
    }

    /// An entity: prefixed when it can be, without the colon of the default
    /// prefix, otherwise `<IRI>`.
    fn entity(&self, iri: &str) -> String {
        match self.prefixed(iri) {
            Some(sf) if sf != iri => match sf.strip_prefix(':') {
                Some(rest) => rest.to_string(),
                None => sf,
            },
            _ => format!("<{iri}>"),
        }
    }
}

// === Index ===============================================================

/// An object property expression as a key: inverse?, property IRI.
type OpeKey<'m> = (bool, &'m str);

fn ope_key(ope: &OPE<RcStr>) -> OpeKey<'_> {
    match ope {
        OPE::ObjectProperty(p) => (false, p.0.as_ref()),
        OPE::InverseObjectProperty(p) => (true, p.0.as_ref()),
    }
}

/// An individual or annotation subject as a key: anonymous?, IRI or node id.
type IndKey<'m> = (bool, &'m str);

fn ind_key(i: &Individual<RcStr>) -> IndKey<'_> {
    match i {
        Individual::Named(n) => (false, n.0.as_ref()),
        Individual::Anonymous(a) => (true, a.0.as_ref()),
    }
}

/// The axioms each frame is made of, gathered in one pass.
#[derive(Default)]
struct Index<'m> {
    /// Annotation assertions by subject.
    annotations: HashMap<IndKey<'m>, Vec<&'m AC>>,
    declarations: HashMap<(Kind, &'m str), Vec<&'m AC>>,
    /// Class axioms by each named class they are stated about: equivalence and
    /// disjointness by named member, `SubClassOf` by its named subclass,
    /// disjoint unions and keys by their class.
    class: HashMap<&'m str, Vec<&'m AC>>,
    /// Object property axioms by each property expression they are about.
    object: HashMap<OpeKey<'m>, Vec<&'m AC>>,
    /// Property chains by their super property.
    chains: HashMap<OpeKey<'m>, Vec<&'m AC>>,
    data: HashMap<&'m str, Vec<&'m AC>>,
    individual: HashMap<IndKey<'m>, Vec<&'m AC>>,
    datatype: HashMap<&'m str, Vec<&'m AC>>,
    annotation_property: HashMap<&'m str, Vec<&'m AC>>,
    /// Every axiom of each kind that may need a section of its own.
    nary: Vec<&'m AC>,
    rules: Vec<&'m AC>,
    gcis: Vec<&'m AC>,
}

impl<'m> Index<'m> {
    fn build(model: &'m Model) -> Index<'m> {
        use Component as C;
        let mut ix = Index::default();
        for ac in model.ont.iter() {
            match &ac.component {
                C::DeclareClass(e) => ix.declarations.entry((Kind::Class, e.0.as_ref())).or_default().push(ac),
                C::DeclareObjectProperty(e) => {
                    ix.declarations.entry((Kind::ObjectProperty, e.0.as_ref())).or_default().push(ac)
                }
                C::DeclareDataProperty(e) => {
                    ix.declarations.entry((Kind::DataProperty, e.0.as_ref())).or_default().push(ac)
                }
                C::DeclareNamedIndividual(e) => {
                    ix.declarations.entry((Kind::NamedIndividual, e.0.as_ref())).or_default().push(ac)
                }
                C::DeclareAnnotationProperty(e) => {
                    ix.declarations.entry((Kind::AnnotationProperty, e.0.as_ref())).or_default().push(ac)
                }
                C::DeclareDatatype(e) => ix.declarations.entry((Kind::Datatype, e.0.as_ref())).or_default().push(ac),
                C::AnnotationAssertion(ax) => {
                    let key = match &ax.subject {
                        AnnotationSubject::IRI(i) => (false, i.as_ref()),
                        AnnotationSubject::AnonymousIndividual(a) => (true, a.0.as_ref()),
                    };
                    ix.annotations.entry(key).or_default().push(ac);
                }
                C::SubClassOf(ax) => match &ax.sub {
                    CE::Class(c) => ix.class.entry(c.0.as_ref()).or_default().push(ac),
                    _ => ix.gcis.push(ac),
                },
                C::EquivalentClasses(ax) => {
                    for m in &ax.0 {
                        if let CE::Class(c) = m {
                            ix.class.entry(c.0.as_ref()).or_default().push(ac);
                        }
                    }
                    ix.nary.push(ac);
                }
                C::DisjointClasses(ax) => {
                    for m in &ax.0 {
                        if let CE::Class(c) = m {
                            ix.class.entry(c.0.as_ref()).or_default().push(ac);
                        }
                    }
                    ix.nary.push(ac);
                }
                C::DisjointUnion(ax) => ix.class.entry(ax.0 .0.as_ref()).or_default().push(ac),
                C::HasKey(ax) => {
                    if let CE::Class(c) = &ax.ce {
                        ix.class.entry(c.0.as_ref()).or_default().push(ac);
                    }
                }
                C::SubObjectPropertyOf(ax) => match &ax.sub {
                    SOPE::ObjectPropertyExpression(sub) => ix.object.entry(ope_key(sub)).or_default().push(ac),
                    SOPE::ObjectPropertyChain(_) => ix.chains.entry(ope_key(&ax.sup)).or_default().push(ac),
                },
                C::EquivalentObjectProperties(ax) => {
                    for p in &ax.0 {
                        ix.object.entry(ope_key(p)).or_default().push(ac);
                    }
                    ix.nary.push(ac);
                }
                C::DisjointObjectProperties(ax) => {
                    for p in &ax.0 {
                        ix.object.entry(ope_key(p)).or_default().push(ac);
                    }
                    ix.nary.push(ac);
                }
                C::InverseObjectProperties(ax) => {
                    ix.object.entry(ope_key(&ax.0)).or_default().push(ac);
                    if ope_key(&ax.1) != ope_key(&ax.0) {
                        ix.object.entry(ope_key(&ax.1)).or_default().push(ac);
                    }
                }
                C::ObjectPropertyDomain(ax) => ix.object.entry(ope_key(&ax.ope)).or_default().push(ac),
                C::ObjectPropertyRange(ax) => ix.object.entry(ope_key(&ax.ope)).or_default().push(ac),
                C::FunctionalObjectProperty(ax) => ix.object.entry(ope_key(&ax.0)).or_default().push(ac),
                C::InverseFunctionalObjectProperty(ax) => ix.object.entry(ope_key(&ax.0)).or_default().push(ac),
                C::SymmetricObjectProperty(ax) => ix.object.entry(ope_key(&ax.0)).or_default().push(ac),
                C::AsymmetricObjectProperty(ax) => ix.object.entry(ope_key(&ax.0)).or_default().push(ac),
                C::TransitiveObjectProperty(ax) => ix.object.entry(ope_key(&ax.0)).or_default().push(ac),
                C::ReflexiveObjectProperty(ax) => ix.object.entry(ope_key(&ax.0)).or_default().push(ac),
                C::IrreflexiveObjectProperty(ax) => ix.object.entry(ope_key(&ax.0)).or_default().push(ac),
                C::SubDataPropertyOf(ax) => ix.data.entry(ax.sub.0.as_ref()).or_default().push(ac),
                C::EquivalentDataProperties(ax) => {
                    for p in &ax.0 {
                        ix.data.entry(p.0.as_ref()).or_default().push(ac);
                    }
                    ix.nary.push(ac);
                }
                C::DisjointDataProperties(ax) => {
                    for p in &ax.0 {
                        ix.data.entry(p.0.as_ref()).or_default().push(ac);
                    }
                    ix.nary.push(ac);
                }
                C::DataPropertyDomain(ax) => ix.data.entry(ax.dp.0.as_ref()).or_default().push(ac),
                C::DataPropertyRange(ax) => ix.data.entry(ax.dp.0.as_ref()).or_default().push(ac),
                C::FunctionalDataProperty(ax) => ix.data.entry(ax.0 .0.as_ref()).or_default().push(ac),
                C::DatatypeDefinition(ax) => ix.datatype.entry(ax.kind.0.as_ref()).or_default().push(ac),
                C::ClassAssertion(ax) => ix.individual.entry(ind_key(&ax.i)).or_default().push(ac),
                C::ObjectPropertyAssertion(ax) => ix.individual.entry(ind_key(&ax.from)).or_default().push(ac),
                C::NegativeObjectPropertyAssertion(ax) => {
                    ix.individual.entry(ind_key(&ax.from)).or_default().push(ac)
                }
                C::DataPropertyAssertion(ax) => ix.individual.entry(ind_key(&ax.from)).or_default().push(ac),
                C::NegativeDataPropertyAssertion(ax) => {
                    ix.individual.entry(ind_key(&ax.from)).or_default().push(ac)
                }
                C::SameIndividual(ax) => {
                    for i in &ax.0 {
                        ix.individual.entry(ind_key(i)).or_default().push(ac);
                    }
                    ix.nary.push(ac);
                }
                C::DifferentIndividuals(ax) => {
                    for i in &ax.0 {
                        ix.individual.entry(ind_key(i)).or_default().push(ac);
                    }
                    ix.nary.push(ac);
                }
                C::SubAnnotationPropertyOf(ax) => {
                    ix.annotation_property.entry(ax.sub.0.as_ref()).or_default().push(ac)
                }
                C::AnnotationPropertyDomain(ax) => {
                    ix.annotation_property.entry(ax.ap.0.as_ref()).or_default().push(ac)
                }
                C::AnnotationPropertyRange(ax) => {
                    ix.annotation_property.entry(ax.ap.0.as_ref()).or_default().push(ac)
                }
                C::Rule(_) => ix.rules.push(ac),
                C::OntologyID(_) | C::DocIRI(_) | C::Import(_) | C::OntologyAnnotation(_) => {}
            }
        }
        ix
    }
}

/// The node ids of the anonymous individuals `model` refers to, wherever they
/// appear.
fn anonymous_individuals(model: &Model) -> BTreeSet<String> {
    use horned_owl::model::AnonymousIndividual;
    use horned_owl::visitor::immutable::{Visit, Walk};
    struct V(BTreeSet<String>);
    impl Visit<RcStr> for V {
        fn visit_anonymous_individual(&mut self, a: &AnonymousIndividual<RcStr>) {
            self.0.insert(a.0.as_ref().to_string());
        }
    }
    let mut walk = Walk::new(V(BTreeSet::new()));
    for ac in model.ont.iter() {
        walk.annotated_component(ac);
    }
    walk.into_visit().0
}

// === Section items =======================================================

/// One item of a section: what is written for it.
#[derive(PartialEq)]
enum Item<'m> {
    Ce(&'m CE<RcStr>),
    Ope(&'m OPE<RcStr>),
    Dp(&'m str),
    Dr(&'m DR<RcStr>),
    Ann(&'m Annotation<RcStr>),
    Ind(&'m Individual<RcStr>),
    Iri(&'m str),
    Ap(&'m str),
    Text(&'static str),
    Ces(Vec<&'m CE<RcStr>>),
    Opes(Vec<&'m OPE<RcStr>>),
    Dps(Vec<&'m str>),
    Inds(Vec<&'m Individual<RcStr>>),
    Keys(Vec<&'m PropertyExpression<RcStr>>),
    Rule(&'m AC),
}

/// The items of a section, each with the axioms that state it.
struct Entries<'m>(Vec<(Item<'m>, Vec<&'m AC>)>);

impl<'m> Entries<'m> {
    fn new() -> Entries<'m> {
        Entries(Vec::new())
    }
    fn put(&mut self, item: Item<'m>, ax: &'m AC) {
        match self.0.iter_mut().find(|(i, _)| *i == item) {
            Some((_, axs)) => {
                if !axs.iter().any(|a| std::ptr::eq(*a, ax)) {
                    axs.push(ax);
                }
            }
            None => self.0.push((item, vec![ax])),
        }
    }
    fn remove(&mut self, item: &Item<'m>) {
        self.0.retain(|(i, _)| i != item);
    }
}

// === Renderer ============================================================

struct Renderer<'m> {
    out: Out,
    sf: ShortForms,
    order: NaturalOrder,
    model: &'m Model,
    ix: Index<'m>,
    /// The entities a frame is written for, by kind.
    signature: &'m BTreeSet<(Kind, String)>,
    /// The anonymous individuals, by node id.
    anonymous: &'m BTreeSet<String>,
    /// Axioms written, and whether their annotations were.
    written: HashMap<*const AC, bool>,
}

impl<'m> Renderer<'m> {
    fn mark(&mut self, ax: &AC, with_annotations: bool) {
        let e = self.written.entry(ax as *const AC).or_insert(false);
        *e = *e || with_annotations;
    }

    fn sort(&self, axs: &mut Vec<&'m AC>) {
        let order = self.order;
        axs.sort_by(|a, b| order.axiom(a, b));
        axs.dedup_by(|a, b| std::ptr::eq(*a, *b));
    }

    /// The axioms of `list` this filter accepts, sorted.
    fn select(&self, list: Option<&Vec<&'m AC>>, f: impl Fn(&Component<RcStr>) -> bool) -> Vec<&'m AC> {
        let mut v: Vec<&'m AC> = list.map(|l| l.iter().copied().filter(|ac| f(&ac.component)).collect()).unwrap_or_default();
        self.sort(&mut v);
        v
    }

    // --- objects ---------------------------------------------------------

    fn entity(&mut self, iri: &str) {
        let s = self.sf.entity(iri);
        self.out.write(&s);
    }

    fn ope(&mut self, ope: &OPE<RcStr>) {
        match ope {
            OPE::ObjectProperty(p) => self.entity(p.0.as_ref()),
            OPE::InverseObjectProperty(p) => {
                self.out.keyword("inverse");
                self.out.write("(");
                self.entity(p.0.as_ref());
                self.out.write(")");
            }
        }
    }

    fn individual(&mut self, i: &Individual<RcStr>) {
        match i {
            Individual::Named(n) => self.entity(n.0.as_ref()),
            Individual::Anonymous(a) => self.out.write(&entities::node_id(a.0.as_ref())),
        }
    }

    fn literal_datatype<'a>(&self, l: &'a Literal<RcStr>) -> &'a str {
        self.order.literal_datatype(l)
    }

    fn literal(&mut self, l: &Literal<RcStr>) {
        let dt = self.literal_datatype(l).to_string();
        let lex = l.literal();
        if dt == XSD_DECIMAL || dt == XSD_INTEGER || dt == XSD_BOOLEAN {
            self.out.write(lex);
            return;
        }
        if dt == XSD_FLOAT {
            self.out.write(lex);
            self.out.write("f");
            return;
        }
        let indent = self.out.indent();
        self.out.push_tab(indent);
        let mut quoted = String::with_capacity(lex.len() + 2);
        quoted.push('"');
        for c in lex.chars() {
            if c == '"' || c == '\\' {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        self.out.write(&quoted);
        match l {
            Literal::Language { lang, .. } if !lang.is_empty() => {
                self.out.write("@");
                self.out.write(lang);
            }
            _ => {
                if dt != RDF_PLAIN_LITERAL && dt != XSD_STRING {
                    self.out.write("^^");
                    self.entity(&dt);
                }
            }
        }
        self.out.pop_tab();
    }

    fn annotation_value(&mut self, v: &AnnotationValue<RcStr>) {
        match v {
            AnnotationValue::IRI(i) => self.out.write(&format!("<{}>", i.as_ref() as &str)),
            AnnotationValue::Literal(l) => self.literal(l),
            AnnotationValue::AnonymousIndividual(a) => self.out.write(&entities::node_id(a.0.as_ref())),
        }
    }

    /// An annotation: its own annotations, then property and value.
    fn annotation(&mut self, a: &Annotation<RcStr>) {
        self.nested_annotations(a.ann.iter());
        self.entity(a.ap.0.as_ref());
        self.out.space();
        self.annotation_value(&a.av);
    }

    /// The annotations of an annotation or of an assertion, as a block of their
    /// own.
    fn nested_annotations<'a>(&mut self, anns: impl IntoIterator<Item = &'a Annotation<RcStr>>) {
        let order = self.order;
        let anns = order.sorted_annotations(anns);
        if anns.is_empty() {
            return;
        }
        self.out.newline();
        self.out.write("Annotations: ");
        let indent = self.out.indent();
        self.out.push_tab(indent);
        for (i, a) in anns.iter().enumerate() {
            self.annotation(a);
            if i + 1 < anns.len() {
                self.out.write(", ");
                self.out.newline();
            }
        }
        self.out.newline();
        self.out.newline();
        self.out.pop_tab();
    }

    fn ce(&mut self, ce: &CE<RcStr>) {
        let order = self.order;
        match ce {
            CE::Class(c) => self.entity(c.0.as_ref()),
            CE::ObjectIntersectionOf(ops) => {
                let ops = sorted_set(ops, |a, b| order.ce(a, b));
                for (i, op) in ops.iter().enumerate() {
                    if i > 0 {
                        if self.out.wrapping {
                            self.out.newline();
                        }
                        self.out.keyword("and");
                    }
                    self.ce_paren(op);
                }
            }
            CE::ObjectUnionOf(ops) => {
                let ops = sorted_set(ops, |a, b| order.ce(a, b));
                for (i, op) in ops.iter().enumerate() {
                    if i > 0 {
                        self.out.keyword("or");
                    }
                    self.ce_paren(op);
                }
            }
            CE::ObjectComplementOf(op) => {
                self.out.write("not ");
                self.out.write("(");
                self.ce(op);
                self.out.write(")");
            }
            CE::ObjectOneOf(inds) => {
                self.out.write("{");
                let inds = sorted_set(inds, |a, b| order.individual(a, b));
                let indent = self.out.indent();
                self.out.push_tab(indent);
                for (i, ind) in inds.iter().enumerate() {
                    self.individual(ind);
                    if i + 1 < inds.len() {
                        self.out.keyword(",");
                    }
                }
                self.out.pop_tab();
                self.out.write("}");
            }
            CE::ObjectSomeValuesFrom { ope, bce } => self.quantified(ope, "some", bce),
            CE::ObjectAllValuesFrom { ope, bce } => self.quantified(ope, "only", bce),
            CE::ObjectHasValue { ope, i } => {
                self.ope(ope);
                self.out.keyword("value");
                self.individual(i);
            }
            CE::ObjectHasSelf(ope) => {
                self.ope(ope);
                self.out.keyword("Self");
            }
            CE::ObjectMinCardinality { n, ope, bce } => self.object_cardinality(ope, "min", *n, bce),
            CE::ObjectMaxCardinality { n, ope, bce } => self.object_cardinality(ope, "max", *n, bce),
            CE::ObjectExactCardinality { n, ope, bce } => self.object_cardinality(ope, "exactly", *n, bce),
            CE::DataSomeValuesFrom { dp, dr } => {
                self.entity(dp.0.as_ref());
                self.out.keyword("some");
                self.dr(dr);
            }
            CE::DataAllValuesFrom { dp, dr } => {
                self.entity(dp.0.as_ref());
                self.out.keyword("only");
                self.dr(dr);
            }
            CE::DataHasValue { dp, l } => {
                self.entity(dp.0.as_ref());
                self.out.keyword("value");
                self.literal(l);
            }
            CE::DataMinCardinality { n, dp, dr } => self.data_cardinality(dp.0.as_ref(), "min", *n, dr),
            CE::DataMaxCardinality { n, dp, dr } => self.data_cardinality(dp.0.as_ref(), "max", *n, dr),
            CE::DataExactCardinality { n, dp, dr } => self.data_cardinality(dp.0.as_ref(), "exactly", *n, dr),
        }
    }

    fn ce_paren(&mut self, ce: &CE<RcStr>) {
        let anonymous = !matches!(ce, CE::Class(_));
        if anonymous {
            self.out.write("(");
        }
        self.ce(ce);
        if anonymous {
            self.out.write(")");
        }
    }

    fn quantified(&mut self, ope: &OPE<RcStr>, kw: &str, filler: &CE<RcStr>) {
        self.ope(ope);
        self.out.keyword(kw);
        let anonymous = !matches!(filler, CE::Class(_));
        let boolean = matches!(filler, CE::ObjectIntersectionOf(_) | CE::ObjectUnionOf(_));
        if anonymous {
            if boolean {
                self.out.increment_tab(4);
                self.out.newline();
            }
            self.out.write("(");
        }
        self.ce(filler);
        if anonymous {
            self.out.write(")");
            if boolean {
                self.out.pop_tab();
            }
        }
    }

    fn object_cardinality(&mut self, ope: &OPE<RcStr>, kw: &str, n: u32, filler: &CE<RcStr>) {
        self.ope(ope);
        self.out.keyword(kw);
        self.out.write(&n.to_string());
        self.out.space();
        self.ce_paren(filler);
    }

    fn data_cardinality(&mut self, dp: &str, kw: &str, n: u32, filler: &DR<RcStr>) {
        self.entity(dp);
        self.out.keyword(kw);
        self.out.write(&n.to_string());
        self.out.space();
        self.dr(filler);
    }

    fn facet_symbol(f: &Facet) -> &'static str {
        match f {
            Facet::Length => "length",
            Facet::MinLength => "minLength",
            Facet::MaxLength => "maxLength",
            Facet::Pattern => "pattern",
            Facet::MinInclusive => ">=",
            Facet::MinExclusive => ">",
            Facet::MaxInclusive => "<=",
            Facet::MaxExclusive => "<",
            Facet::TotalDigits => "totalDigits",
            Facet::FractionDigits => "fractionDigits",
            Facet::LangRange => "langRange",
        }
    }

    fn dr(&mut self, dr: &DR<RcStr>) {
        let order = self.order;
        match dr {
            DR::Datatype(d) => self.entity(d.0.as_ref()),
            DR::DataComplementOf(op) => {
                self.out.keyword("not");
                if matches!(**op, DR::Datatype(_)) {
                    self.dr(op);
                } else {
                    self.out.write("(");
                    self.dr(op);
                    self.out.write(")");
                }
            }
            DR::DataOneOf(lits) => {
                self.out.write("{");
                let lits = sorted_set(lits, |a, b| order.literal(a, b));
                let indent = self.out.indent();
                self.out.push_tab(indent);
                for (i, l) in lits.iter().enumerate() {
                    self.literal(l);
                    if i + 1 < lits.len() {
                        self.out.keyword(",");
                    }
                }
                self.out.pop_tab();
                self.out.write("}");
            }
            DR::DataIntersectionOf(ops) | DR::DataUnionOf(ops) => {
                let kw = if matches!(dr, DR::DataIntersectionOf(_)) { "and" } else { "or" };
                self.out.write("(");
                let ops = sorted_set(ops, |a, b| order.dr(a, b));
                let indent = self.out.indent();
                self.out.push_tab(indent);
                for (i, op) in ops.iter().enumerate() {
                    self.dr(op);
                    if i + 1 < ops.len() {
                        self.out.keyword(kw);
                    }
                }
                self.out.pop_tab();
                self.out.write(")");
            }
            DR::DatatypeRestriction(dt, facets) => {
                self.entity(dt.0.as_ref());
                self.out.write("[");
                let facets = sorted_set(facets, |a, b| order.facet_restriction(a, b));
                let indent = self.out.indent();
                self.out.push_tab(indent);
                for (i, f) in facets.iter().enumerate() {
                    self.out.write(Self::facet_symbol(&f.f));
                    self.out.space();
                    self.literal(&f.l);
                    if i + 1 < facets.len() {
                        self.out.keyword(",");
                    }
                }
                self.out.pop_tab();
                self.out.write("]");
            }
        }
    }

    fn iarg(&mut self, a: &IArgument<RcStr>) {
        match a {
            IArgument::Individual(i) => self.individual(i),
            IArgument::Variable(v) => self.variable(v.0.as_ref()),
        }
    }

    fn darg(&mut self, a: &DArgument<RcStr>) {
        match a {
            DArgument::Literal(l) => self.literal(l),
            DArgument::Variable(v) => self.variable(v.0.as_ref()),
        }
    }

    /// A rule variable: `?name` in the conventional `urn:swrl#` namespaces,
    /// otherwise `?<IRI>`.
    fn variable(&mut self, iri: &str) {
        self.out.write("?");
        let (ns, local) = iri_split(iri);
        if ns == "urn:swrl:var#" || ns == "urn:swrl#" {
            self.out.write(local);
        } else {
            self.out.write(&format!("<{iri}>"));
        }
    }

    fn atom(&mut self, atom: &Atom<RcStr>) {
        let order = self.order;
        match atom {
            Atom::ClassAtom { pred, arg } => {
                self.ce_paren(pred);
                self.out.write("(");
                self.iarg(arg);
                self.out.write(")");
            }
            Atom::DataRangeAtom { pred, arg } => {
                self.dr(pred);
                self.out.write("(");
                self.darg(arg);
                self.out.write(")");
            }
            Atom::ObjectPropertyAtom { pred, args } => {
                self.ope(pred);
                self.out.write("(");
                self.iarg(&args.0);
                self.out.write(", ");
                self.iarg(&args.1);
                self.out.write(")");
            }
            Atom::DataPropertyAtom { pred, args } => {
                self.entity(pred.0.as_ref());
                self.out.write("(");
                self.darg(&args.0);
                self.out.write(", ");
                self.darg(&args.1);
                self.out.write(")");
            }
            Atom::BuiltInAtom { pred, args } => {
                let iri: &str = pred.as_ref();
                match iri.strip_prefix(SWRLB_NS).filter(|l| SWRL_BUILTINS.contains(l)) {
                    Some(local) => self.out.write(&format!("swrlb:{local}")),
                    None => self.out.write(&format!("<{iri}>")),
                }
                self.out.write("(");
                let mut args: Vec<&DArgument<RcStr>> = args.iter().collect();
                args.sort_by(|a, b| match (a, b) {
                    (DArgument::Variable(x), DArgument::Variable(y)) => {
                        crate::io::natural_order::iri_cmp(x.0.as_ref(), y.0.as_ref())
                    }
                    (DArgument::Literal(x), DArgument::Literal(y)) => order.literal(x, y),
                    (DArgument::Variable(_), DArgument::Literal(_)) => std::cmp::Ordering::Less,
                    (DArgument::Literal(_), DArgument::Variable(_)) => std::cmp::Ordering::Greater,
                });
                for (i, a) in args.iter().enumerate() {
                    self.darg(a);
                    if i + 1 < args.len() {
                        self.out.write(", ");
                    }
                }
                self.out.write(")");
            }
            Atom::SameIndividualAtom(a, b) | Atom::DifferentIndividualsAtom(a, b) => {
                self.out.keyword(if matches!(atom, Atom::SameIndividualAtom(..)) { "SameAs" } else { "DifferentFrom" });
                self.out.write("(");
                self.iarg(a);
                self.out.write(", ");
                self.iarg(b);
                self.out.write(")");
            }
        }
    }

    /// A rule, on one line.
    fn rule(&mut self, ac: &AC) {
        let Component::Rule(rule) = &ac.component else { return };
        let (tabbing, wrapping) = (self.out.tabbing, self.out.wrapping);
        self.out.tabbing = false;
        self.out.wrapping = false;
        for (i, a) in rule.body.iter().enumerate() {
            self.atom(a);
            if i + 1 < rule.body.len() {
                self.out.write(", ");
            }
        }
        self.out.write(" -> ");
        for (i, a) in rule.head.iter().enumerate() {
            self.atom(a);
            if i + 1 < rule.head.len() {
                self.out.write(", ");
            }
        }
        self.out.tabbing = tabbing;
        self.out.wrapping = wrapping;
    }

    fn item(&mut self, item: &Item<'m>, delimiter: &str, newline: bool) {
        match item {
            Item::Ce(c) => self.ce(c),
            Item::Ope(o) => self.ope(o),
            Item::Dp(d) | Item::Ap(d) => self.entity(d),
            Item::Dr(d) => self.dr(d),
            Item::Ann(a) => self.annotation(a),
            Item::Ind(i) => self.individual(i),
            Item::Iri(i) => self.out.write(&format!("<{i}>")),
            Item::Text(t) => self.out.write(t),
            Item::Rule(r) => self.rule(r),
            Item::Ces(v) => {
                let n = v.len();
                for (i, c) in v.iter().enumerate() {
                    self.ce(c);
                    self.delimit(i, n, delimiter, newline);
                }
            }
            Item::Opes(v) => {
                let n = v.len();
                for (i, o) in v.iter().enumerate() {
                    self.ope(o);
                    self.delimit(i, n, delimiter, newline);
                }
            }
            Item::Dps(v) => {
                let n = v.len();
                for (i, d) in v.iter().enumerate() {
                    self.entity(d);
                    self.delimit(i, n, delimiter, newline);
                }
            }
            Item::Inds(v) => {
                let n = v.len();
                for (i, d) in v.iter().enumerate() {
                    self.individual(d);
                    self.delimit(i, n, delimiter, newline);
                }
            }
            Item::Keys(v) => {
                let n = v.len();
                for (i, pe) in v.iter().enumerate() {
                    match pe {
                        PropertyExpression::ObjectPropertyExpression(o) => self.ope(o),
                        PropertyExpression::DataProperty(d) => self.entity(d.0.as_ref()),
                        PropertyExpression::AnnotationProperty(a) => self.entity(a.0.as_ref()),
                    }
                    self.delimit(i, n, delimiter, newline);
                }
            }
        }
    }

    fn delimit(&mut self, i: usize, n: usize, delimiter: &str, newline: bool) {
        if i + 1 < n {
            self.out.write(delimiter);
            if newline {
                self.out.newline();
            }
        }
    }

    // --- sections --------------------------------------------------------

    /// A section whose items carry the annotations of the axioms stating them.
    fn section(&mut self, kw: &str, entries: Entries<'m>, delimiter: &str, newline: bool) {
        if entries.0.is_empty() {
            return;
        }
        self.out.section(kw);
        self.out.increment_tab(4);
        self.out.newline();
        let n = entries.0.len();
        for (i, (item, axioms)) in entries.0.iter().enumerate() {
            for (j, ax) in axioms.iter().enumerate() {
                self.mark(ax, true);
                let order = self.order;
                let anns = order.sorted_annotations(ax.ann.iter());
                if !anns.is_empty() {
                    self.out.increment_tab(4);
                    self.out.newline();
                    self.out.write("Annotations: ");
                    let indent = self.out.indent();
                    self.out.push_tab(indent + 1);
                    for (k, a) in anns.iter().enumerate() {
                        self.annotation(a);
                        if k + 1 < anns.len() {
                            self.out.write(", ");
                            self.out.newline();
                        }
                    }
                    self.out.pop_tab();
                    self.out.pop_tab();
                    self.out.newline();
                }
                self.item(item, delimiter, newline);
                if j + 1 < axioms.len() {
                    self.out.write(",");
                    self.out.newline();
                }
            }
            if i + 1 < n {
                self.out.write(delimiter);
                if newline {
                    self.out.newline();
                }
            }
        }
        self.out.pop_tab();
        self.out.newline();
        self.out.newline();
    }

    /// A section of plain items: the axioms behind them are written without
    /// their annotations.
    fn list(&mut self, kw: &str, items: Vec<Item<'m>>, delimiter: &str, newline: bool) {
        if items.is_empty() {
            return;
        }
        self.out.section(kw);
        self.out.increment_tab(4);
        self.out.newline();
        let n = items.len();
        for (i, item) in items.iter().enumerate() {
            self.item(item, delimiter, newline);
            if i + 1 < n {
                self.out.write(delimiter);
                if newline {
                    self.out.newline();
                }
            }
        }
        self.out.pop_tab();
        self.out.newline();
        self.out.newline();
    }

    // --- frames ----------------------------------------------------------

    /// A frame's opening: keyword, the annotations of the entity's declarations,
    /// the entity, then its annotation assertions.
    fn frame_start(&mut self, kw: &str, entity: Option<(Kind, &'m str)>, write_subject: &dyn Fn(&mut Self)) {
        self.out.section(kw);
        let mut reset = false;
        if let Some((kind, iri)) = entity {
            let decls = self.ix.declarations.get(&(kind, iri)).cloned().unwrap_or_default();
            let mut anns: Vec<&'m Annotation<RcStr>> = Vec::new();
            for d in &decls {
                self.mark(d, true);
                for a in d.ann.iter() {
                    if !anns.contains(&a) {
                        anns.push(a);
                    }
                }
            }
            if !anns.is_empty() {
                // The declarations' annotations are one set: in hash order.
                let hashes: Vec<i32> = anns.iter().map(|a| crate::owlapi_hash::annotation_hash(a)).collect();
                let ordered: Vec<&Annotation<RcStr>> =
                    crate::owlapi_hash::hashset_order(&hashes).into_iter().map(|i| anns[i]).collect();
                self.out.increment_tab(4);
                self.out.newline();
                self.out.write("Annotations: ");
                let indent = self.out.indent();
                self.out.push_tab(indent + 1);
                for (k, a) in ordered.iter().enumerate() {
                    self.annotation(a);
                    if k + 1 < ordered.len() {
                        self.out.write(", ");
                        self.out.newline();
                    }
                }
                self.out.pop_tab();
                self.out.pop_tab();
                self.out.increment_tab(2);
                self.out.newline();
                reset = true;
            }
        }
        write_subject(self);
        if reset {
            self.out.pop_tab();
        }
        self.out.newline();
        self.out.increment_tab(4);
        self.out.newline();
    }

    /// The annotation assertions on a subject.
    fn annotations_of(&mut self, key: IndKey<'m>) {
        let axs = self.select(self.ix.annotations.get(&key), |_| true);
        let mut entries = Entries::new();
        for ax in axs {
            if let Component::AnnotationAssertion(a) = &ax.component {
                entries.put(Item::Ann(&a.ann), ax);
            }
        }
        self.section("Annotations", entries, ",", true);
    }

    fn frame_end(&mut self) {
        self.out.pop_tab();
        self.out.newline();
    }

    fn class_frame(&mut self, iri: &'m str) {
        use Component as C;
        self.frame_start("Class", Some((Kind::Class, iri)), &|r| r.entity(iri));
        self.annotations_of((false, iri));
        let order = self.order;
        let is_cls = |c: &CE<RcStr>| matches!(c, CE::Class(x) if x.0.as_ref() == iri);
        let axs = self.ix.class.get(iri).cloned();

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::EquivalentClasses(e) if distinct_ces(&e.0) == 2)) {
            let C::EquivalentClasses(e) = &ax.component else { continue };
            for other in sorted_set(e.0.iter().filter(|c| !is_cls(c)), |a, b| order.ce(a, b)) {
                entries.put(Item::Ce(other), ax);
            }
        }
        self.section("EquivalentTo", entries, ",", true);

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::SubClassOf(s) if is_cls(&s.sub))) {
            let C::SubClassOf(s) = &ax.component else { continue };
            entries.put(Item::Ce(&s.sup), ax);
        }
        self.section("SubClassOf", entries, ",", true);

        for ax in self.select(axs.as_ref(), |c| matches!(c, C::DisjointUnion(d) if d.0 .0.as_ref() == iri)) {
            let C::DisjointUnion(d) = &ax.component else { continue };
            self.mark(ax, false);
            let items: Vec<Item> = sorted_set(&d.1, |a, b| order.ce(a, b)).into_iter().map(Item::Ce).collect();
            self.list("DisjointUnionOf", items, ", ", false);
        }

        let mut entries = Entries::new();
        // Disjointness of more than two classes is written among the n-ary axioms.
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::DisjointClasses(d) if distinct_ces(&d.0) == 2)) {
            let C::DisjointClasses(d) = &ax.component else { continue };
            if let Some(other) = sorted_set(d.0.iter().filter(|c| !is_cls(c)), |a, b| order.ce(a, b)).first() {
                entries.put(Item::Ce(other), ax);
            }
        }
        self.section("DisjointWith", entries, ", ", false);

        for ax in self.select(axs.as_ref(), |c| matches!(c, C::HasKey(k) if is_cls(&k.ce))) {
            let C::HasKey(k) = &ax.component else { continue };
            let keys = sorted_set(&k.vpe, |a, b| order.property_expression(a, b));
            let mut entries = Entries::new();
            entries.put(Item::Keys(keys), ax);
            self.section("HasKey", entries, ", ", true);
        }
        self.frame_end();
    }

    fn object_property_frame(&mut self, key: OpeKey<'m>) {
        use Component as C;
        let (inverse, iri) = key;
        let entity = if inverse { None } else { Some((Kind::ObjectProperty, iri)) };
        self.frame_start("ObjectProperty", entity, &|r| {
            if inverse {
                r.out.keyword("inverse");
                r.out.write("(");
                r.entity(iri);
                r.out.write(")");
            } else {
                r.entity(iri);
            }
        });
        if !inverse {
            self.annotations_of((false, iri));
        }
        let order = self.order;
        let is_p = |o: &OPE<RcStr>| ope_key(o) == key;
        let axs = self.ix.object.get(&key).cloned();

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| {
            matches!(c, C::SubObjectPropertyOf(s) if matches!(&s.sub, SOPE::ObjectPropertyExpression(o) if is_p(o)))
        }) {
            let C::SubObjectPropertyOf(s) = &ax.component else { continue };
            entries.put(Item::Ope(&s.sup), ax);
        }
        self.section("SubPropertyOf", entries, ",", true);

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::EquivalentObjectProperties(e) if distinct_opes(&e.0) == 2)) {
            let C::EquivalentObjectProperties(e) = &ax.component else { continue };
            if let Some(other) = sorted_set(e.0.iter().filter(|o| !is_p(o)), |a, b| order.ope(a, b)).first() {
                entries.put(Item::Ope(other), ax);
            }
        }
        self.section("EquivalentTo", entries, ",", true);

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::DisjointObjectProperties(e) if distinct_opes(&e.0) == 2)) {
            let C::DisjointObjectProperties(e) = &ax.component else { continue };
            if let Some(other) = sorted_set(e.0.iter().filter(|o| !is_p(o)), |a, b| order.ope(a, b)).first() {
                entries.put(Item::Ope(other), ax);
            }
        }
        self.section("DisjointWith", entries, ",", true);

        let chains = self.ix.chains.get(&key).cloned();
        for ax in self.select(chains.as_ref(), |_| true) {
            let C::SubObjectPropertyOf(s) = &ax.component else { continue };
            let SOPE::ObjectPropertyChain(chain) = &s.sub else { continue };
            let mut entries = Entries::new();
            entries.put(Item::Opes(chain.iter().collect()), ax);
            self.section("SubPropertyChain", entries, " o ", false);
        }

        let mut entries = Entries::new();
        let kinds: [Accept<Component<RcStr>>; 7] = [
            ("Functional", |c| matches!(c, C::FunctionalObjectProperty(_))),
            ("InverseFunctional", |c| matches!(c, C::InverseFunctionalObjectProperty(_))),
            ("Symmetric", |c| matches!(c, C::SymmetricObjectProperty(_))),
            ("Transitive", |c| matches!(c, C::TransitiveObjectProperty(_))),
            ("Reflexive", |c| matches!(c, C::ReflexiveObjectProperty(_))),
            ("Irreflexive", |c| matches!(c, C::IrreflexiveObjectProperty(_))),
            ("Asymmetric", |c| matches!(c, C::AsymmetricObjectProperty(_))),
        ];
        for (name, is_kind) in kinds {
            for ax in self.select(axs.as_ref(), is_kind) {
                entries.put(Item::Text(name), ax);
            }
        }
        self.section("Characteristics", entries, ",", true);

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::ObjectPropertyDomain(d) if is_p(&d.ope))) {
            let C::ObjectPropertyDomain(d) = &ax.component else { continue };
            entries.put(Item::Ce(&d.ce), ax);
        }
        self.section("Domain", entries, ",", true);

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::ObjectPropertyRange(d) if is_p(&d.ope))) {
            let C::ObjectPropertyRange(d) = &ax.component else { continue };
            entries.put(Item::Ce(&d.ce), ax);
        }
        self.section("Range", entries, ",", true);

        let mut inverses: Vec<&'m OPE<RcStr>> = Vec::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::InverseObjectProperties(_))) {
            let C::InverseObjectProperties(i) = &ax.component else { continue };
            self.mark(ax, false);
            inverses.push(if is_p(&i.0) { &i.1 } else { &i.0 });
        }
        let inverses = sorted_set(inverses, |a, b| order.ope(a, b));
        self.list("InverseOf", inverses.into_iter().map(Item::Ope).collect(), ",", true);
        self.frame_end();
    }

    fn data_property_frame(&mut self, iri: &'m str) {
        use Component as C;
        self.frame_start("DataProperty", Some((Kind::DataProperty, iri)), &|r| r.entity(iri));
        self.annotations_of((false, iri));
        let axs = self.ix.data.get(iri).cloned();
        let is_p = |d: &horned_owl::model::DataProperty<RcStr>| d.0.as_ref() == iri;

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::FunctionalDataProperty(_))) {
            entries.put(Item::Text("Functional"), ax);
        }
        self.section("Characteristics", entries, ",", true);

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::DataPropertyDomain(_))) {
            let C::DataPropertyDomain(d) = &ax.component else { continue };
            entries.put(Item::Ce(&d.ce), ax);
        }
        self.section("Domain", entries, ",", true);

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::DataPropertyRange(_))) {
            let C::DataPropertyRange(d) = &ax.component else { continue };
            entries.put(Item::Dr(&d.dr), ax);
        }
        self.section("Range", entries, ",", true);

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::SubDataPropertyOf(s) if is_p(&s.sub))) {
            let C::SubDataPropertyOf(s) = &ax.component else { continue };
            entries.put(Item::Dp(s.sup.0.as_ref()), ax);
        }
        self.section("SubPropertyOf", entries, ",", true);

        for (kw, equivalent) in [("EquivalentTo", true), ("DisjointWith", false)] {
            let mut entries = Entries::new();
            for ax in self.select(axs.as_ref(), |c| match c {
                C::EquivalentDataProperties(e) if equivalent => distinct_dps(&e.0) == 2,
                C::DisjointDataProperties(e) if !equivalent => distinct_dps(&e.0) == 2,
                _ => false,
            }) {
                let members = match &ax.component {
                    C::EquivalentDataProperties(e) => &e.0,
                    C::DisjointDataProperties(e) => &e.0,
                    _ => continue,
                };
                if let Some(other) = members.iter().find(|d| !is_p(d)) {
                    entries.put(Item::Dp(other.0.as_ref()), ax);
                }
            }
            entries.remove(&Item::Dp(iri));
            self.section(kw, entries, ",", true);
        }
        self.frame_end();
    }

    fn individual_frame(&mut self, key: IndKey<'m>, ind: &Individual<RcStr>) {
        use Component as C;
        let entity = if key.0 { None } else { Some((Kind::NamedIndividual, key.1)) };
        let id = key.1;
        let anonymous = key.0;
        self.frame_start("Individual", entity, &|r| {
            if anonymous {
                r.out.write(&entities::node_id(id));
            } else {
                r.entity(id);
            }
        });
        self.annotations_of(key);
        let order = self.order;
        let axs = self.ix.individual.get(&key).cloned();

        let mut entries = Entries::new();
        for ax in self.select(axs.as_ref(), |c| matches!(c, C::ClassAssertion(a) if ind_key(&a.i) == key)) {
            let C::ClassAssertion(a) = &ax.component else { continue };
            entries.put(Item::Ce(&a.ce), ax);
        }
        self.section("Types", entries, ",", true);

        let facts = self.select(axs.as_ref(), |c| match c {
            C::ObjectPropertyAssertion(a) => ind_key(&a.from) == key,
            C::NegativeObjectPropertyAssertion(a) => ind_key(&a.from) == key,
            _ => false,
        });
        let data_facts = self.select(axs.as_ref(), |c| match c {
            C::DataPropertyAssertion(a) => ind_key(&a.from) == key,
            C::NegativeDataPropertyAssertion(a) => ind_key(&a.from) == key,
            _ => false,
        });
        let mut all: Vec<&'m AC> = facts.into_iter().chain(data_facts).collect();
        self.sort(&mut all);
        if !all.is_empty() {
            self.out.section("Facts");
            self.out.space();
            self.out.increment_tab(1);
            self.out.newline();
            let n = all.len();
            for (i, ax) in all.iter().enumerate() {
                self.mark(ax, true);
                let anns: Vec<&Annotation<RcStr>> = order.sorted_annotations(ax.ann.iter());
                if !anns.is_empty() {
                    self.nested_annotations(anns.iter().copied());
                    let indent = self.out.indent();
                    self.out.push_tab(indent + 1);
                }
                match &ax.component {
                    C::ObjectPropertyAssertion(a) => {
                        self.ope(&a.ope);
                        self.out.space();
                        self.out.space();
                        self.individual(&a.to);
                    }
                    C::NegativeObjectPropertyAssertion(a) => {
                        self.out.keyword("not");
                        self.out.space();
                        self.ope(&a.ope);
                        self.out.space();
                        self.out.space();
                        self.individual(&a.to);
                    }
                    C::DataPropertyAssertion(a) => {
                        self.entity(a.dp.0.as_ref());
                        self.out.space();
                        self.out.space();
                        self.literal(&a.to);
                    }
                    C::NegativeDataPropertyAssertion(a) => {
                        self.out.keyword("not");
                        self.out.space();
                        self.entity(a.dp.0.as_ref());
                        self.out.space();
                        self.out.space();
                        self.literal(&a.to);
                    }
                    _ => {}
                }
                if !anns.is_empty() {
                    self.out.pop_tab();
                }
                if i + 1 < n {
                    self.out.write(",");
                    self.out.newline();
                }
            }
            self.out.pop_tab();
            self.out.newline();
            self.out.newline();
        }

        for (kw, same) in [("SameAs", true), ("DifferentFrom", false)] {
            let mut inds: Vec<&'m Individual<RcStr>> = Vec::new();
            for ax in self.select(axs.as_ref(), |c| match c {
                C::SameIndividual(s) if same => true,
                C::DifferentIndividuals(s) if !same => true,
                _ => false,
            }) {
                let members = match &ax.component {
                    C::SameIndividual(s) => &s.0,
                    C::DifferentIndividuals(s) => &s.0,
                    _ => continue,
                };
                if distinct_inds(members) == 2 && ax.ann.is_empty() {
                    self.mark(ax, true);
                    inds.extend(members.iter());
                }
            }
            let inds: Vec<&Individual<RcStr>> =
                sorted_set(inds, |a, b| order.individual(a, b)).into_iter().filter(|i| *i != ind).collect();
            self.list(kw, inds.into_iter().map(Item::Ind).collect(), ",", true);
        }
        self.frame_end();
    }

    fn datatype_frame(&mut self, iri: &'m str) {
        use Component as C;
        self.frame_start("Datatype", Some((Kind::Datatype, iri)), &|r| r.entity(iri));
        self.annotations_of((false, iri));
        let order = self.order;
        let axs = self.ix.datatype.get(iri).cloned();
        let mut ranges: Vec<&'m DR<RcStr>> = Vec::new();
        for ax in self.select(axs.as_ref(), |_| true) {
            let C::DatatypeDefinition(d) = &ax.component else { continue };
            self.mark(ax, false);
            ranges.push(&d.range);
        }
        let ranges = sorted_set(ranges, |a, b| order.dr(a, b));
        self.list("EquivalentTo", ranges.into_iter().map(Item::Dr).collect(), ",", true);
        self.frame_end();
    }

    fn annotation_property_frame(&mut self, iri: &'m str) {
        use Component as C;
        self.frame_start("AnnotationProperty", Some((Kind::AnnotationProperty, iri)), &|r| r.entity(iri));
        self.annotations_of((false, iri));
        let axs = self.ix.annotation_property.get(iri).cloned();
        let mut supers: Vec<&'m str> = Vec::new();
        let mut domains: Vec<&'m str> = Vec::new();
        let mut ranges: Vec<&'m str> = Vec::new();
        for ax in self.select(axs.as_ref(), |_| true) {
            self.mark(ax, false);
            match &ax.component {
                C::SubAnnotationPropertyOf(s) => supers.push(s.sup.0.as_ref()),
                C::AnnotationPropertyDomain(d) => domains.push(d.iri.as_ref()),
                C::AnnotationPropertyRange(r) => ranges.push(r.iri.as_ref()),
                _ => {}
            }
        }
        let sf = &self.sf;
        supers.sort_by(|a, b| str_cmp(&sf.entity(a), &sf.entity(b)));
        supers.dedup();
        domains.sort_by(|a, b| crate::io::natural_order::iri_cmp(a, b));
        domains.dedup();
        ranges.sort_by(|a, b| crate::io::natural_order::iri_cmp(a, b));
        ranges.dedup();
        self.list("SubPropertyOf", supers.into_iter().map(Item::Ap).collect(), ",", true);
        self.list("Domain", domains.into_iter().map(Item::Iri).collect(), ",", true);
        self.list("Range", ranges.into_iter().map(Item::Iri).collect(), ",", true);
        self.frame_end();
    }

    // --- document --------------------------------------------------------

    fn prefixes(&mut self, ontology_iri: Option<&str>) {
        let mut names = self.sf.names.clone();
        names.sort_by(|a, b| str_cmp(&a.0, &b.0));
        let has_default = names.iter().any(|(n, _)| n == ":");
        for (name, ns) in &names {
            self.out.write("Prefix: ");
            self.out.write(name);
            self.out.write(" ");
            self.out.write(&format!("<{ns}>"));
            self.out.newline();
        }
        if !has_default {
            self.out.write("Prefix: : ");
            self.out.write(&format!("<{}>", ontology_iri.unwrap_or("urn:absoluteiri:defaultvalue#")));
            self.out.newline();
        }
        if !names.is_empty() {
            self.out.newline();
            self.out.newline();
        }
    }

    fn header(&mut self, iri: Option<&str>, version: Option<&str>) {
        let order = self.order;
        self.out.write("Ontology:");
        self.out.space();
        if let Some(iri) = iri {
            let indent = self.out.indent();
            self.out.write(&format!("<{iri}>"));
            self.out.newline();
            self.out.push_tab(indent);
            if let Some(v) = version {
                self.out.write(&format!("<{v}>"));
            }
            self.out.pop_tab();
        }
        self.out.newline();
        let mut imports: Vec<&str> = self
            .model
            .ont
            .iter()
            .filter_map(|ac| match &ac.component {
                Component::Import(i) => Some(i.0.as_ref()),
                _ => None,
            })
            .collect();
        imports.sort_by(|a, b| crate::io::natural_order::iri_cmp(a, b));
        imports.dedup();
        for i in imports {
            self.out.write("Import: ");
            self.out.write(&format!("<{i}>"));
            self.out.newline();
        }
        self.out.newline();
        let anns: Vec<&'m Annotation<RcStr>> = self
            .model
            .ont
            .iter()
            .filter_map(|ac| match &ac.component {
                Component::OntologyAnnotation(a) => Some(&a.0),
                _ => None,
            })
            .collect();
        let anns = order.sorted_annotations(anns);
        self.list("Annotations", anns.into_iter().map(Item::Ann).collect(), ",", true);
    }

    /// The n-ary axioms no frame holds.
    fn nary(&mut self) {
        use Component as C;
        let order = self.order;
        let mut nary = self.ix.nary.clone();
        self.sort(&mut nary);
        let sections: [Accept<AC>; 8] = [
            ("DisjointClasses", |ac| matches!(&ac.component, C::DisjointClasses(d) if distinct_ces(&d.0) > 2)),
            ("EquivalentClasses", |ac| matches!(&ac.component, C::EquivalentClasses(d) if distinct_ces(&d.0) > 2)),
            ("DisjointProperties", |ac| {
                matches!(&ac.component, C::DisjointObjectProperties(d) if distinct_opes(&d.0) > 2)
            }),
            ("EquivalentProperties", |ac| {
                matches!(&ac.component, C::EquivalentObjectProperties(d) if distinct_opes(&d.0) > 2)
            }),
            ("DisjointProperties", |ac| matches!(&ac.component, C::DisjointDataProperties(d) if distinct_dps(&d.0) > 2)),
            ("EquivalentProperties", |ac| {
                matches!(&ac.component, C::EquivalentDataProperties(d) if distinct_dps(&d.0) > 2)
            }),
            ("DifferentIndividuals", |ac| {
                matches!(&ac.component, C::DifferentIndividuals(d)
                    if distinct_inds(&d.0) > 2 || (distinct_inds(&d.0) == 2 && !ac.ann.is_empty()))
            }),
            ("SameIndividual", |ac| {
                matches!(&ac.component, C::SameIndividual(d)
                    if distinct_inds(&d.0) > 2 || (distinct_inds(&d.0) == 2 && !ac.ann.is_empty()))
            }),
        ];
        for (kw, accept) in sections {
            for ax in nary.iter().copied().filter(|ac| accept(ac)) {
                let item = match &ax.component {
                    C::DisjointClasses(d) => Item::Ces(sorted_set(&d.0, |a, b| order.ce(a, b))),
                    C::EquivalentClasses(d) => Item::Ces(sorted_set(&d.0, |a, b| order.ce(a, b))),
                    C::DisjointObjectProperties(d) => Item::Opes(sorted_set(&d.0, |a, b| order.ope(a, b))),
                    C::EquivalentObjectProperties(d) => Item::Opes(sorted_set(&d.0, |a, b| order.ope(a, b))),
                    C::DisjointDataProperties(d) => Item::Dps(sorted_dps(&d.0)),
                    C::EquivalentDataProperties(d) => Item::Dps(sorted_dps(&d.0)),
                    C::DifferentIndividuals(d) => Item::Inds(sorted_set(&d.0, |a, b| order.individual(a, b))),
                    C::SameIndividual(d) => Item::Inds(sorted_set(&d.0, |a, b| order.individual(a, b))),
                    _ => continue,
                };
                let mut entries = Entries::new();
                entries.put(item, ax);
                self.section(kw, entries, ",", false);
            }
        }
    }

    fn rules(&mut self) {
        let mut rules = self.ix.rules.clone();
        self.sort(&mut rules);
        for r in rules {
            self.mark(r, r.ann.is_empty());
            self.list("Rule", vec![Item::Rule(r)], ", ", false);
        }
    }

    /// General class axioms, grouped by subclass: the groups in the order a
    /// hash map keyed by the subclass holds them, each frame's tab left open.
    fn gcis(&mut self) {
        use Component as C;
        let order = self.order;
        let mut gcis = self.ix.gcis.clone();
        self.sort(&mut gcis);
        let mut groups: Vec<(&'m CE<RcStr>, Vec<&'m AC>)> = Vec::new();
        for ax in gcis {
            let C::SubClassOf(s) = &ax.component else { continue };
            match groups.iter_mut().find(|(k, _)| *k == &s.sub) {
                Some((_, v)) => v.push(ax),
                None => groups.push((&s.sub, vec![ax])),
            }
        }
        let hashes: Vec<i32> = groups.iter().map(|(k, _)| crate::owlapi_hash::ce_hash(k)).collect();
        let cap = crate::owlapi_hash::java_hashset_capacity(groups.len()) as u32;
        let mut idx: Vec<usize> = (0..groups.len()).collect();
        idx.sort_by_key(|&i| {
            let h = hashes[i] as u32;
            (h ^ (h >> 16)) & (cap - 1)
        });
        for i in idx {
            let (sub, axs) = &groups[i];
            let sub = *sub;
            self.frame_start("Class", None, &|r| r.ce(sub));
            let mut entries = Entries::new();
            let mut axs = axs.clone();
            axs.sort_by(|a, b| order.axiom(a, b));
            for ax in axs {
                let C::SubClassOf(s) = &ax.component else { continue };
                entries.put(Item::Ce(&s.sup), ax);
            }
            self.section("SubClassOf", entries, ",", true);
        }
    }

    fn document(&mut self) {
        let model = self.model;
        let (iri, version) = model
            .ont
            .iter()
            .find_map(|ac| match &ac.component {
                Component::OntologyID(id) => Some((
                    id.iri.as_ref().map(|i| i.as_ref().to_string()),
                    id.viri.as_ref().map(|i| i.as_ref().to_string()),
                )),
                _ => None,
            })
            .unwrap_or((None, None));
        self.prefixes(iri.as_deref());
        self.out.newline();
        self.header(iri.as_deref(), version.as_deref());

        // Each kind's entities, in the order of how they are written.
        let signature = self.signature;
        let of_kind = |r: &Self, k: Kind| -> Vec<&'m str> {
            let mut v: Vec<&'m str> =
                signature.iter().filter(|(kind, _)| *kind == k).map(|(_, iri)| iri.as_str()).collect();
            v.sort_by(|a, b| str_cmp(&r.sf.entity(a), &r.sf.entity(b)));
            v
        };
        for iri in of_kind(self, Kind::AnnotationProperty) {
            self.annotation_property_frame(iri);
        }
        for iri in of_kind(self, Kind::Datatype) {
            self.datatype_frame(iri);
        }
        for iri in of_kind(self, Kind::ObjectProperty) {
            self.object_property_frame((false, iri));
            if self.ix.object.get(&(true, iri)).is_some_and(|v| !v.is_empty()) {
                self.object_property_frame((true, iri));
            }
        }
        for iri in of_kind(self, Kind::DataProperty) {
            self.data_property_frame(iri);
        }
        for iri in of_kind(self, Kind::Class) {
            self.class_frame(iri);
        }
        for iri in of_kind(self, Kind::NamedIndividual) {
            let ind = Individual::Named(horned_owl::model::NamedIndividual(model.build.iri(iri)));
            self.individual_frame((false, iri), &ind);
        }
        for id in self.anonymous {
            let ind = Individual::Anonymous(horned_owl::model::AnonymousIndividual(RcStr::from(id.as_str())));
            self.individual_frame((true, id.as_str()), &ind);
        }
        self.nary();
        self.rules();
        self.gcis();
    }

    /// Report what the document could not hold.
    fn report_losses(&self) {
        let mut lost = 0usize;
        let mut lost_annotations = 0usize;
        let mut example: Option<String> = None;
        for ac in self.model.ont.iter() {
            if matches!(
                ac.component,
                Component::OntologyID(_) | Component::DocIRI(_) | Component::Import(_) | Component::OntologyAnnotation(_)
            ) {
                continue;
            }
            match self.written.get(&(ac as *const AC)) {
                None => {
                    lost += 1;
                    if example.is_none() {
                        example = Some(crate::io::owlfunc::render_component_line(ac));
                    }
                }
                Some(false) if !ac.ann.is_empty() => lost_annotations += 1,
                _ => {}
            }
        }
        if lost > 0 {
            status!(
                "WARN: Manchester syntax cannot hold {lost} axiom(s); they are not written (first: {})",
                example.unwrap_or_default()
            );
        }
        if lost_annotations > 0 {
            status!(
                "WARN: Manchester syntax cannot hold the annotations of {lost_annotations} axiom(s); the axioms are written without them"
            );
        }
    }
}

fn distinct_ces(v: &[CE<RcStr>]) -> usize {
    let mut seen: Vec<&CE<RcStr>> = Vec::new();
    for c in v {
        if !seen.contains(&c) {
            seen.push(c);
        }
    }
    seen.len()
}

fn distinct_opes(v: &[OPE<RcStr>]) -> usize {
    let mut seen: Vec<&OPE<RcStr>> = Vec::new();
    for c in v {
        if !seen.contains(&c) {
            seen.push(c);
        }
    }
    seen.len()
}

fn distinct_dps(v: &[horned_owl::model::DataProperty<RcStr>]) -> usize {
    sorted_dps(v).len()
}

fn sorted_dps(v: &[horned_owl::model::DataProperty<RcStr>]) -> Vec<&str> {
    let mut out: Vec<&str> = v.iter().map(|d| d.0.as_ref()).collect();
    out.sort_by(|a, b| crate::io::natural_order::iri_cmp(a, b));
    out.dedup();
    out
}

fn distinct_inds(v: &[Individual<RcStr>]) -> usize {
    let mut seen: Vec<&Individual<RcStr>> = Vec::new();
    for c in v {
        if !seen.contains(&c) {
            seen.push(c);
        }
    }
    seen.len()
}

/// Write `model` in Manchester syntax, declaring `prefixes` (name → namespace).
pub fn save<W: Write>(model: &Model, prefixes: &[(String, String)], w: &mut W) -> Result<()> {
    let signature = entities::signature(model);
    let anonymous = anonymous_individuals(model);
    let mut r = Renderer {
        out: Out::new(),
        sf: ShortForms::new(prefixes),
        order: NaturalOrder::new(model.plain_literals_typed),
        model,
        ix: Index::build(model),
        signature: &signature,
        anonymous: &anonymous,
        written: HashMap::new(),
    };
    r.document();
    w.write_all(r.out.text.as_bytes())?;
    r.report_losses();
    Ok(())
}
