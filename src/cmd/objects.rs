//! The objects `filter` and `remove` select, and how an axiom is judged against
//! a selection — theirs, or the classes `collapse` takes out.
//!
//! An object is a named entity of one kind, an anonymous class expression, an
//! anonymous individual, an inverse object property, a data range, a bare IRI,
//! or one direction of an equivalence of data properties. A command starts from
//! the entities its terms name — with none, from every object the ontology's
//! axioms name — and maps that set through each `--select` group in turn: a
//! group's selectors each map the set, and the group is the union of what they
//! give. Every axiom is then judged by its objects, all of them or any of them
//! selected, or by the IRIs it names alone under `--signature true`.
//!
//! Only the ontology's own axioms are read: what an import lends is neither
//! selected from nor judged.

use std::cell::OnceCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{bail, Result};
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, Atom, ClassExpression, Component,
    DArgument, DataRange, IArgument, Individual, Literal, ObjectPropertyExpression, PropertyExpression, RcStr,
    SubObjectPropertyExpression, IRI,
};

use crate::cmd::select;
use crate::io::entities::Kind;
use crate::model::Model;

type CE = ClassExpression<RcStr>;
type Ope = ObjectPropertyExpression<RcStr>;
type Ac = AnnotatedComponent<RcStr>;

const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// One object of an ontology.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Obj {
    /// A named entity of one kind.
    Entity(Kind, RcStr),
    /// An anonymous class expression.
    Expression(CE),
    /// An anonymous individual, by its node id.
    Anonymous(RcStr),
    /// An inverse object property, `ObjectInverseOf(p)`.
    Inverse(RcStr),
    /// An anonymous data range.
    Range(DataRange<RcStr>),
    /// An IRI that names no entity: an annotation property's domain or range.
    Iri(RcStr),
    /// One direction of an equivalence of data properties, `sub ⊑ sup`.
    SubDataProperty(RcStr, RcStr),
}

impl Obj {
    /// The IRI of a named entity.
    pub fn named(&self) -> Option<&str> {
        match self {
            Obj::Entity(_, iri) => Some(iri),
            _ => None,
        }
    }
}

fn rc(iri: &IRI<RcStr>) -> RcStr {
    RcStr::from(iri)
}

fn class_object(ce: &CE) -> Obj {
    match ce {
        ClassExpression::Class(c) => Obj::Entity(Kind::Class, rc(&c.0)),
        other => Obj::Expression(other.clone()),
    }
}

fn property_object(ope: &Ope) -> Obj {
    match ope {
        Ope::ObjectProperty(p) => Obj::Entity(Kind::ObjectProperty, rc(&p.0)),
        Ope::InverseObjectProperty(p) => Obj::Inverse(rc(&p.0)),
    }
}

fn individual_object(i: &Individual<RcStr>) -> Obj {
    match i {
        Individual::Named(n) => Obj::Entity(Kind::NamedIndividual, rc(&n.0)),
        Individual::Anonymous(a) => Obj::Anonymous(a.0.clone()),
    }
}

fn range_object(dr: &DataRange<RcStr>) -> Obj {
    match dr {
        DataRange::Datatype(d) => Obj::Entity(Kind::Datatype, rc(&d.0)),
        other => Obj::Range(other.clone()),
    }
}

// ── signatures ──────────────────────────────────────────────────────────────

/// Collects the entities a component names: every named entity it mentions,
/// and the datatype of each literal ([`literal_datatype`]) except the value of
/// an annotation assertion. An annotation assertion's subject and IRI value,
/// and an annotation property's domain and range, are IRIs, not entities.
struct Signature<'a> {
    out: &'a mut Vec<Obj>,
    /// The datatype an untyped literal has in this document.
    plain: &'static str,
    /// Whether a data has-value's literal names its datatype: it does in an
    /// axiom's signature, and not in that of a class expression on its own.
    has_value_datatypes: bool,
}

impl Signature<'_> {
    fn entity(&mut self, kind: Kind, iri: &IRI<RcStr>) {
        self.out.push(Obj::Entity(kind, rc(iri)));
    }

    fn literal(&mut self, l: &Literal<RcStr>) {
        self.out.push(Obj::Entity(Kind::Datatype, RcStr::from(literal_datatype(l, self.plain).as_str())));
    }

    fn property(&mut self, ope: &Ope) {
        match ope {
            Ope::ObjectProperty(p) | Ope::InverseObjectProperty(p) => self.entity(Kind::ObjectProperty, &p.0),
        }
    }

    fn individual(&mut self, i: &Individual<RcStr>) {
        if let Individual::Named(n) = i {
            self.entity(Kind::NamedIndividual, &n.0);
        }
    }

    fn class(&mut self, ce: &CE) {
        use ClassExpression as C;
        match ce {
            C::Class(c) => self.entity(Kind::Class, &c.0),
            C::ObjectIntersectionOf(v) | C::ObjectUnionOf(v) => v.iter().for_each(|x| self.class(x)),
            C::ObjectComplementOf(x) => self.class(x),
            C::ObjectOneOf(v) => v.iter().for_each(|i| self.individual(i)),
            C::ObjectSomeValuesFrom { ope, bce } | C::ObjectAllValuesFrom { ope, bce } => {
                self.property(ope);
                self.class(bce);
            }
            C::ObjectHasValue { ope, i } => {
                self.property(ope);
                self.individual(i);
            }
            C::ObjectHasSelf(ope) => self.property(ope),
            C::ObjectMinCardinality { ope, bce, .. }
            | C::ObjectMaxCardinality { ope, bce, .. }
            | C::ObjectExactCardinality { ope, bce, .. } => {
                self.property(ope);
                self.class(bce);
            }
            C::DataSomeValuesFrom { dp, dr } | C::DataAllValuesFrom { dp, dr } => {
                self.entity(Kind::DataProperty, &dp.0);
                self.range(dr);
            }
            C::DataHasValue { dp, l } => {
                self.entity(Kind::DataProperty, &dp.0);
                if self.has_value_datatypes {
                    self.literal(l);
                }
            }
            C::DataMinCardinality { dp, dr, .. }
            | C::DataMaxCardinality { dp, dr, .. }
            | C::DataExactCardinality { dp, dr, .. } => {
                self.entity(Kind::DataProperty, &dp.0);
                self.range(dr);
            }
        }
    }

    fn range(&mut self, dr: &DataRange<RcStr>) {
        match dr {
            DataRange::Datatype(d) => self.entity(Kind::Datatype, &d.0),
            DataRange::DataIntersectionOf(v) | DataRange::DataUnionOf(v) => v.iter().for_each(|x| self.range(x)),
            DataRange::DataComplementOf(x) => self.range(x),
            DataRange::DataOneOf(v) => v.iter().for_each(|l| self.literal(l)),
            DataRange::DatatypeRestriction(d, facets) => {
                self.entity(Kind::Datatype, &d.0);
                facets.iter().for_each(|f| self.literal(&f.l));
            }
        }
    }

    fn annotation(&mut self, a: &Annotation<RcStr>) {
        self.entity(Kind::AnnotationProperty, &a.ap.0);
        if let AnnotationValue::Literal(l) = &a.av {
            self.literal(l);
        }
        a.ann.iter().for_each(|x| self.annotation(x));
    }

    fn iargument(&mut self, a: &IArgument<RcStr>) {
        if let IArgument::Individual(i) = a {
            self.individual(i);
        }
    }

    fn dargument(&mut self, a: &DArgument<RcStr>) {
        if let DArgument::Literal(l) = a {
            self.literal(l);
        }
    }

    fn atom(&mut self, atom: &Atom<RcStr>) {
        match atom {
            Atom::BuiltInAtom { args, .. } => args.iter().for_each(|a| self.dargument(a)),
            Atom::ClassAtom { pred, arg } => {
                self.class(pred);
                self.iargument(arg);
            }
            Atom::DataPropertyAtom { pred, args } => {
                self.entity(Kind::DataProperty, &pred.0);
                self.dargument(&args.0);
                self.dargument(&args.1);
            }
            Atom::DataRangeAtom { pred, arg } => {
                self.range(pred);
                self.dargument(arg);
            }
            Atom::DifferentIndividualsAtom(a, b) | Atom::SameIndividualAtom(a, b) => {
                self.iargument(a);
                self.iargument(b);
            }
            Atom::ObjectPropertyAtom { pred, args } => {
                self.property(pred);
                self.iargument(&args.0);
                self.iargument(&args.1);
            }
        }
    }

    fn component(&mut self, c: &Component<RcStr>) {
        use Component as X;
        match c {
            X::DeclareClass(d) => self.entity(Kind::Class, &d.0 .0),
            X::DeclareObjectProperty(d) => self.entity(Kind::ObjectProperty, &d.0 .0),
            X::DeclareAnnotationProperty(d) => self.entity(Kind::AnnotationProperty, &d.0 .0),
            X::DeclareDataProperty(d) => self.entity(Kind::DataProperty, &d.0 .0),
            X::DeclareNamedIndividual(d) => self.entity(Kind::NamedIndividual, &d.0 .0),
            X::DeclareDatatype(d) => self.entity(Kind::Datatype, &d.0 .0),
            X::SubClassOf(a) => {
                self.class(&a.sub);
                self.class(&a.sup);
            }
            X::EquivalentClasses(a) => a.0.iter().for_each(|x| self.class(x)),
            X::DisjointClasses(a) => a.0.iter().for_each(|x| self.class(x)),
            X::DisjointUnion(a) => {
                self.entity(Kind::Class, &a.0 .0);
                a.1.iter().for_each(|x| self.class(x));
            }
            X::SubObjectPropertyOf(a) => {
                match &a.sub {
                    SubObjectPropertyExpression::ObjectPropertyChain(v) => v.iter().for_each(|p| self.property(p)),
                    SubObjectPropertyExpression::ObjectPropertyExpression(p) => self.property(p),
                }
                self.property(&a.sup);
            }
            X::EquivalentObjectProperties(a) => a.0.iter().for_each(|p| self.property(p)),
            X::DisjointObjectProperties(a) => a.0.iter().for_each(|p| self.property(p)),
            X::InverseObjectProperties(a) => {
                self.property(&a.0);
                self.property(&a.1);
            }
            X::ObjectPropertyDomain(a) => {
                self.property(&a.ope);
                self.class(&a.ce);
            }
            X::ObjectPropertyRange(a) => {
                self.property(&a.ope);
                self.class(&a.ce);
            }
            X::FunctionalObjectProperty(a) => self.property(&a.0),
            X::InverseFunctionalObjectProperty(a) => self.property(&a.0),
            X::ReflexiveObjectProperty(a) => self.property(&a.0),
            X::IrreflexiveObjectProperty(a) => self.property(&a.0),
            X::SymmetricObjectProperty(a) => self.property(&a.0),
            X::AsymmetricObjectProperty(a) => self.property(&a.0),
            X::TransitiveObjectProperty(a) => self.property(&a.0),
            X::SubDataPropertyOf(a) => {
                self.entity(Kind::DataProperty, &a.sub.0);
                self.entity(Kind::DataProperty, &a.sup.0);
            }
            X::EquivalentDataProperties(a) => a.0.iter().for_each(|d| self.entity(Kind::DataProperty, &d.0)),
            X::DisjointDataProperties(a) => a.0.iter().for_each(|d| self.entity(Kind::DataProperty, &d.0)),
            X::DataPropertyDomain(a) => {
                self.entity(Kind::DataProperty, &a.dp.0);
                self.class(&a.ce);
            }
            X::DataPropertyRange(a) => {
                self.entity(Kind::DataProperty, &a.dp.0);
                self.range(&a.dr);
            }
            X::FunctionalDataProperty(a) => self.entity(Kind::DataProperty, &a.0 .0),
            X::DatatypeDefinition(a) => {
                self.entity(Kind::Datatype, &a.kind.0);
                self.range(&a.range);
            }
            X::HasKey(a) => {
                self.class(&a.ce);
                for pe in &a.vpe {
                    match pe {
                        PropertyExpression::ObjectPropertyExpression(p) => self.property(p),
                        PropertyExpression::DataProperty(d) => self.entity(Kind::DataProperty, &d.0),
                        PropertyExpression::AnnotationProperty(p) => self.entity(Kind::AnnotationProperty, &p.0),
                    }
                }
            }
            X::SameIndividual(a) => a.0.iter().for_each(|i| self.individual(i)),
            X::DifferentIndividuals(a) => a.0.iter().for_each(|i| self.individual(i)),
            X::ClassAssertion(a) => {
                self.class(&a.ce);
                self.individual(&a.i);
            }
            X::ObjectPropertyAssertion(a) => {
                self.property(&a.ope);
                self.individual(&a.from);
                self.individual(&a.to);
            }
            X::NegativeObjectPropertyAssertion(a) => {
                self.property(&a.ope);
                self.individual(&a.from);
                self.individual(&a.to);
            }
            X::DataPropertyAssertion(a) => {
                self.entity(Kind::DataProperty, &a.dp.0);
                self.individual(&a.from);
                self.literal(&a.to);
            }
            X::NegativeDataPropertyAssertion(a) => {
                self.entity(Kind::DataProperty, &a.dp.0);
                self.individual(&a.from);
                self.literal(&a.to);
            }
            X::AnnotationAssertion(a) => self.entity(Kind::AnnotationProperty, &a.ann.ap.0),
            X::SubAnnotationPropertyOf(a) => {
                self.entity(Kind::AnnotationProperty, &a.sub.0);
                self.entity(Kind::AnnotationProperty, &a.sup.0);
            }
            X::AnnotationPropertyDomain(a) => self.entity(Kind::AnnotationProperty, &a.ap.0),
            X::AnnotationPropertyRange(a) => self.entity(Kind::AnnotationProperty, &a.ap.0),
            X::Rule(r) => r.body.iter().chain(&r.head).for_each(|a| self.atom(a)),
            X::OntologyAnnotation(a) => self.annotation(&a.0),
            X::Import(_) | X::OntologyID(_) | X::DocIRI(_) => {}
        }
    }
}

/// The entities `component` names, and those its annotations name when given.
fn signature(
    component: &Component<RcStr>,
    annotations: Option<&BTreeSet<Annotation<RcStr>>>,
    plain: &'static str,
) -> Vec<Obj> {
    let mut out = Vec::new();
    let mut sig = Signature { out: &mut out, plain, has_value_datatypes: true };
    sig.component(component);
    for a in annotations.into_iter().flatten() {
        sig.annotation(a);
    }
    out
}

/// The IRIs an axiom names, for a test that looks at named objects alone: an
/// annotation assertion's property, its subject and its value where they are
/// IRIs; for any other axiom, the IRIs of its signature.
fn signature_iris(component: &Component<RcStr>, plain: &'static str) -> Vec<RcStr> {
    if let Component::AnnotationAssertion(a) = component {
        let mut out = vec![rc(&a.ann.ap.0)];
        if let AnnotationSubject::IRI(s) = &a.subject {
            out.push(rc(s));
        }
        if let AnnotationValue::IRI(v) = &a.ann.av {
            out.push(rc(v));
        }
        return out;
    }
    signature(component, None, plain)
        .into_iter()
        .filter_map(|o| match o {
            Obj::Entity(_, iri) => Some(iri),
            _ => None,
        })
        .collect()
}

/// Every class expression in `ce`, itself included.
fn nested_classes(ce: &CE, out: &mut Vec<Obj>) {
    use ClassExpression as C;
    out.push(class_object(ce));
    match ce {
        C::ObjectIntersectionOf(v) | C::ObjectUnionOf(v) => v.iter().for_each(|x| nested_classes(x, out)),
        C::ObjectComplementOf(x) => nested_classes(x, out),
        C::ObjectSomeValuesFrom { bce, .. }
        | C::ObjectAllValuesFrom { bce, .. }
        | C::ObjectMinCardinality { bce, .. }
        | C::ObjectMaxCardinality { bce, .. }
        | C::ObjectExactCardinality { bce, .. } => nested_classes(bce, out),
        _ => {}
    }
}

/// The objects of an axiom: its signature, and the things an axiom of its kind
/// is about that the signature does not hold — a class assertion's class
/// expression, an n-ary class axiom's operands, a subclass axiom's two sides,
/// an object property axiom's nested class expressions, an assertion's
/// anonymous individuals, a key's class and property expressions, and the two
/// directions of an equivalence of data properties. With `annotations`, what
/// the axiom's annotations name too.
pub fn axiom_objects(
    component: &Component<RcStr>,
    annotations: Option<&BTreeSet<Annotation<RcStr>>>,
    plain: &'static str,
) -> Vec<Obj> {
    use Component as X;
    let mut out = signature(component, annotations, plain);
    let anonymous = |i: &Individual<RcStr>| matches!(i, Individual::Anonymous(_));
    match component {
        X::ClassAssertion(a) => out.push(class_object(&a.ce)),
        X::DisjointUnion(a) => out.extend(a.1.iter().map(class_object)),
        X::EquivalentDataProperties(a) => {
            for (i, sub) in a.0.iter().enumerate() {
                for (j, sup) in a.0.iter().enumerate() {
                    if i != j {
                        out.push(Obj::SubDataProperty(rc(&sub.0), rc(&sup.0)));
                    }
                }
            }
        }
        X::EquivalentClasses(a) => out.extend(a.0.iter().map(class_object)),
        X::DisjointClasses(a) => out.extend(a.0.iter().map(class_object)),
        X::SameIndividual(a) => out.extend(a.0.iter().filter(|i| anonymous(i)).map(individual_object)),
        X::DifferentIndividuals(a) => out.extend(a.0.iter().map(individual_object)),
        X::NegativeObjectPropertyAssertion(a) => {
            out.extend([&a.from, &a.to].into_iter().filter(|i| anonymous(i)).map(individual_object))
        }
        X::ObjectPropertyAssertion(a) => {
            out.extend([&a.from, &a.to].into_iter().filter(|i| anonymous(i)).map(individual_object))
        }
        X::ObjectPropertyDomain(a) => nested_classes(&a.ce, &mut out),
        X::ObjectPropertyRange(a) => nested_classes(&a.ce, &mut out),
        X::SubClassOf(a) => {
            out.push(class_object(&a.sup));
            out.push(class_object(&a.sub));
        }
        X::HasKey(a) => {
            out.push(class_object(&a.ce));
            for pe in &a.vpe {
                out.push(match pe {
                    PropertyExpression::ObjectPropertyExpression(p) => property_object(p),
                    PropertyExpression::DataProperty(d) => Obj::Entity(Kind::DataProperty, rc(&d.0)),
                    PropertyExpression::AnnotationProperty(p) => Obj::Entity(Kind::AnnotationProperty, rc(&p.0)),
                });
            }
        }
        _ => {}
    }
    out
}

/// The entities a class expression names on its own: as in an axiom's
/// signature, save that the literal of a data has-value names no datatype.
pub fn expression_entities(ce: &CE, plain: &'static str) -> Vec<Obj> {
    let mut out = Vec::new();
    Signature { out: &mut out, plain, has_value_datatypes: false }.class(ce);
    out
}

/// Whether a component is an axiom: neither the ontology's name, its document
/// IRI, an import nor an ontology annotation.
fn is_axiom(c: &Component<RcStr>) -> bool {
    !matches!(c, Component::OntologyID(_) | Component::DocIRI(_) | Component::Import(_) | Component::OntologyAnnotation(_))
}

// ── the selection ───────────────────────────────────────────────────────────

/// Lookups over the ontology's own axioms, by what each axiom is about.
#[derive(Default)]
struct Index {
    supers: HashMap<RcStr, Vec<CE>>,
    subs: HashMap<RcStr, Vec<CE>>,
    equivalents: HashMap<RcStr, Vec<CE>>,
    op_supers: HashMap<Ope, Vec<Ope>>,
    op_subs: HashMap<Ope, Vec<Ope>>,
    op_equivalents: HashMap<Ope, Vec<Ope>>,
    dp_supers: HashMap<RcStr, Vec<RcStr>>,
    dp_subs: HashMap<RcStr, Vec<RcStr>>,
    dp_equivalents: HashMap<RcStr, Vec<RcStr>>,
    ap_supers: HashMap<RcStr, Vec<RcStr>>,
    ap_subs: HashMap<RcStr, Vec<RcStr>>,
    op_domains: HashMap<Ope, Vec<CE>>,
    op_ranges: HashMap<Ope, Vec<CE>>,
    dp_domains: HashMap<RcStr, Vec<CE>>,
    dp_ranges: HashMap<RcStr, Vec<DataRange<RcStr>>>,
    ap_domains: HashMap<RcStr, Vec<RcStr>>,
    ap_ranges: HashMap<RcStr, Vec<RcStr>>,
    types: HashMap<Individual<RcStr>, Vec<CE>>,
    instances: HashMap<RcStr, Vec<Individual<RcStr>>>,
    /// Annotation assertions, as indices into the axioms, by subject IRI.
    assertions: HashMap<RcStr, Vec<usize>>,
}

/// The objects of an ontology and what can be selected from them.
pub struct Selection<'m> {
    model: &'m Model,
    /// The datatype an untyped literal has in this document.
    plain: &'static str,
    axioms: Vec<&'m Ac>,
    index: OnceCell<Index>,
    objects: OnceCell<HashSet<Obj>>,
    kinds: OnceCell<HashMap<RcStr, Vec<Kind>>>,
    about: OnceCell<HashMap<Obj, Vec<usize>>>,
}

impl<'m> Selection<'m> {
    pub fn new(model: &'m Model) -> Self {
        let axioms = model
            .ont
            .iter()
            .filter(|ac| is_axiom(&ac.component) && !model.imported_components.contains(*ac))
            .collect();
        Selection {
            model,
            plain: plain_datatype(model),
            axioms,
            index: OnceCell::new(),
            objects: OnceCell::new(),
            kinds: OnceCell::new(),
            about: OnceCell::new(),
        }
    }

    /// The ontology's own axioms.
    pub fn axioms(&self) -> &[&'m Ac] {
        &self.axioms
    }

    /// Every object the ontology's axioms name, their annotations included.
    pub fn objects(&self) -> &HashSet<Obj> {
        self.objects.get_or_init(|| {
            let mut out = HashSet::new();
            for ac in &self.axioms {
                out.extend(axiom_objects(&ac.component, Some(&ac.ann), self.plain));
            }
            out
        })
    }

    /// The kinds each IRI of the ontology's signature names, its header
    /// annotations included.
    fn kinds(&self) -> &HashMap<RcStr, Vec<Kind>> {
        self.kinds.get_or_init(|| {
            let mut out: HashMap<RcStr, Vec<Kind>> = HashMap::new();
            let header = self.model.ont.iter().filter(|ac| matches!(ac.component, Component::OntologyAnnotation(_)));
            for ac in self.axioms.iter().copied().chain(header) {
                for o in signature(&ac.component, Some(&ac.ann), self.plain) {
                    if let Obj::Entity(kind, iri) = o {
                        let kinds = out.entry(iri).or_default();
                        if !kinds.contains(&kind) {
                            kinds.push(kind);
                        }
                    }
                }
            }
            out
        })
    }

    /// The entities `iris` name. An IRI names the entity of the ontology's
    /// signature it is the IRI of; one that names entities of several kinds
    /// names all of them under `allow_punning` and none otherwise, and one the
    /// ontology does not name names nothing.
    pub fn entities(&self, iris: &HashSet<String>, allow_punning: bool) -> HashSet<Obj> {
        let mut out = HashSet::new();
        for iri in iris {
            let Some(kinds) = self.kinds().get(iri.as_str()) else { continue };
            if kinds.len() == 1 || allow_punning {
                out.extend(kinds.iter().map(|k| Obj::Entity(*k, RcStr::from(iri.as_str()))));
            }
        }
        out
    }

    fn index(&self) -> &Index {
        self.index.get_or_init(|| {
            use Component as X;
            let mut ix = Index::default();
            for (n, ac) in self.axioms.iter().enumerate() {
                match &ac.component {
                    X::SubClassOf(a) => {
                        if let ClassExpression::Class(c) = &a.sub {
                            ix.supers.entry(rc(&c.0)).or_default().push(a.sup.clone());
                        }
                        if let ClassExpression::Class(c) = &a.sup {
                            ix.subs.entry(rc(&c.0)).or_default().push(a.sub.clone());
                        }
                    }
                    X::EquivalentClasses(a) => {
                        for ce in &a.0 {
                            if let ClassExpression::Class(c) = ce {
                                let others = a.0.iter().filter(|x| *x != ce).cloned();
                                ix.equivalents.entry(rc(&c.0)).or_default().extend(others);
                            }
                        }
                    }
                    X::SubObjectPropertyOf(a) => {
                        if let SubObjectPropertyExpression::ObjectPropertyExpression(sub) = &a.sub {
                            ix.op_supers.entry(sub.clone()).or_default().push(a.sup.clone());
                            ix.op_subs.entry(a.sup.clone()).or_default().push(sub.clone());
                        }
                    }
                    X::EquivalentObjectProperties(a) => {
                        for p in &a.0 {
                            let others = a.0.iter().filter(|x| *x != p).cloned();
                            ix.op_equivalents.entry(p.clone()).or_default().extend(others);
                        }
                    }
                    X::SubDataPropertyOf(a) => {
                        ix.dp_supers.entry(rc(&a.sub.0)).or_default().push(rc(&a.sup.0));
                        ix.dp_subs.entry(rc(&a.sup.0)).or_default().push(rc(&a.sub.0));
                    }
                    X::EquivalentDataProperties(a) => {
                        for d in &a.0 {
                            let others = a.0.iter().filter(|x| *x != d).map(|x| rc(&x.0));
                            ix.dp_equivalents.entry(rc(&d.0)).or_default().extend(others);
                        }
                    }
                    X::SubAnnotationPropertyOf(a) => {
                        ix.ap_supers.entry(rc(&a.sub.0)).or_default().push(rc(&a.sup.0));
                        ix.ap_subs.entry(rc(&a.sup.0)).or_default().push(rc(&a.sub.0));
                    }
                    X::ObjectPropertyDomain(a) => ix.op_domains.entry(a.ope.clone()).or_default().push(a.ce.clone()),
                    X::ObjectPropertyRange(a) => ix.op_ranges.entry(a.ope.clone()).or_default().push(a.ce.clone()),
                    X::DataPropertyDomain(a) => ix.dp_domains.entry(rc(&a.dp.0)).or_default().push(a.ce.clone()),
                    X::DataPropertyRange(a) => ix.dp_ranges.entry(rc(&a.dp.0)).or_default().push(a.dr.clone()),
                    X::AnnotationPropertyDomain(a) => ix.ap_domains.entry(rc(&a.ap.0)).or_default().push(rc(&a.iri)),
                    X::AnnotationPropertyRange(a) => ix.ap_ranges.entry(rc(&a.ap.0)).or_default().push(rc(&a.iri)),
                    X::ClassAssertion(a) => {
                        ix.types.entry(a.i.clone()).or_default().push(a.ce.clone());
                        if let ClassExpression::Class(c) = &a.ce {
                            ix.instances.entry(rc(&c.0)).or_default().push(a.i.clone());
                        }
                    }
                    X::AnnotationAssertion(a) => {
                        if let AnnotationSubject::IRI(s) = &a.subject {
                            ix.assertions.entry(rc(s)).or_default().push(n);
                        }
                    }
                    _ => {}
                }
            }
            ix
        })
    }

    /// The axioms each entity has as its own: those an entity of its kind is
    /// the subject of.
    fn about(&self) -> &HashMap<Obj, Vec<usize>> {
        self.about.get_or_init(|| {
            use Component as X;
            let class = |c: &CE| match c {
                ClassExpression::Class(c) => Some(Obj::Entity(Kind::Class, rc(&c.0))),
                _ => None,
            };
            let property = |p: &Ope| match p {
                Ope::ObjectProperty(q) => Some(Obj::Entity(Kind::ObjectProperty, rc(&q.0))),
                Ope::InverseObjectProperty(_) => None,
            };
            let data = |d: &horned_owl::model::DataProperty<RcStr>| Obj::Entity(Kind::DataProperty, rc(&d.0));
            let named = |i: &Individual<RcStr>| match i {
                Individual::Named(n) => Some(Obj::Entity(Kind::NamedIndividual, rc(&n.0))),
                _ => None,
            };
            let mut out: HashMap<Obj, Vec<usize>> = HashMap::new();
            for (n, ac) in self.axioms.iter().enumerate() {
                let owners: Vec<Obj> = match &ac.component {
                    X::SubClassOf(a) => class(&a.sub).into_iter().collect(),
                    X::EquivalentClasses(a) => a.0.iter().filter_map(class).collect(),
                    X::DisjointClasses(a) => a.0.iter().filter_map(class).collect(),
                    X::DisjointUnion(a) => vec![Obj::Entity(Kind::Class, rc(&a.0 .0))],
                    X::SubObjectPropertyOf(a) => match &a.sub {
                        SubObjectPropertyExpression::ObjectPropertyExpression(p) => property(p).into_iter().collect(),
                        _ => Vec::new(),
                    },
                    X::EquivalentObjectProperties(a) => a.0.iter().filter_map(property).collect(),
                    X::DisjointObjectProperties(a) => a.0.iter().filter_map(property).collect(),
                    X::InverseObjectProperties(a) => [&a.0, &a.1].into_iter().filter_map(property).collect(),
                    X::ObjectPropertyDomain(a) => property(&a.ope).into_iter().collect(),
                    X::ObjectPropertyRange(a) => property(&a.ope).into_iter().collect(),
                    X::FunctionalObjectProperty(a) => property(&a.0).into_iter().collect(),
                    X::InverseFunctionalObjectProperty(a) => property(&a.0).into_iter().collect(),
                    X::ReflexiveObjectProperty(a) => property(&a.0).into_iter().collect(),
                    X::IrreflexiveObjectProperty(a) => property(&a.0).into_iter().collect(),
                    X::SymmetricObjectProperty(a) => property(&a.0).into_iter().collect(),
                    X::AsymmetricObjectProperty(a) => property(&a.0).into_iter().collect(),
                    X::TransitiveObjectProperty(a) => property(&a.0).into_iter().collect(),
                    X::SubDataPropertyOf(a) => vec![data(&a.sub)],
                    X::EquivalentDataProperties(a) => a.0.iter().map(data).collect(),
                    X::DisjointDataProperties(a) => a.0.iter().map(data).collect(),
                    X::DataPropertyDomain(a) => vec![data(&a.dp)],
                    X::DataPropertyRange(a) => vec![data(&a.dp)],
                    X::FunctionalDataProperty(a) => vec![data(&a.0)],
                    X::SubAnnotationPropertyOf(a) => vec![Obj::Entity(Kind::AnnotationProperty, rc(&a.sub.0))],
                    X::AnnotationPropertyDomain(a) => vec![Obj::Entity(Kind::AnnotationProperty, rc(&a.ap.0))],
                    X::AnnotationPropertyRange(a) => vec![Obj::Entity(Kind::AnnotationProperty, rc(&a.ap.0))],
                    X::DatatypeDefinition(a) => vec![Obj::Entity(Kind::Datatype, rc(&a.kind.0))],
                    X::ClassAssertion(a) => named(&a.i).into_iter().collect(),
                    X::ObjectPropertyAssertion(a) => named(&a.from).into_iter().collect(),
                    X::NegativeObjectPropertyAssertion(a) => named(&a.from).into_iter().collect(),
                    X::DataPropertyAssertion(a) => named(&a.from).into_iter().collect(),
                    X::NegativeDataPropertyAssertion(a) => named(&a.from).into_iter().collect(),
                    X::SameIndividual(a) => a.0.iter().filter_map(named).collect(),
                    X::DifferentIndividuals(a) => a.0.iter().filter_map(named).collect(),
                    _ => Vec::new(),
                };
                for o in owners {
                    out.entry(o).or_default().push(n);
                }
            }
            out
        })
    }

    /// Map `objects` through each group in turn.
    pub fn select_groups(&self, mut objects: HashSet<Obj>, groups: &[Vec<String>]) -> Result<HashSet<Obj>> {
        for group in groups {
            objects = self.select_group(&objects, group)?;
        }
        Ok(objects)
    }

    /// What one group selects from `objects`: the union of what each of its
    /// selectors selects, or `objects` itself for an empty group.
    fn select_group(&self, objects: &HashSet<Obj>, group: &[String]) -> Result<HashSet<Obj>> {
        if group.is_empty() {
            return Ok(objects.clone());
        }
        let mut out = HashSet::new();
        for selector in group {
            out.extend(self.select(objects, selector)?);
        }
        Ok(out)
    }

    /// What one selector selects from `objects`.
    fn select(&self, objects: &HashSet<Obj>, selector: &str) -> Result<HashSet<Obj>> {
        let ix = || self.index();
        let kind_of = |kind: Kind| -> HashSet<Obj> {
            objects.iter().filter(|o| matches!(o, Obj::Entity(k, _) if *k == kind)).cloned().collect()
        };
        Ok(match selector {
            "ancestors" => self.ancestors(objects),
            "anonymous" => {
                objects.iter().filter(|o| matches!(o, Obj::Expression(_) | Obj::Anonymous(_))).cloned().collect()
            }
            "annotation-properties" => kind_of(Kind::AnnotationProperty),
            // The children of a class, an object property and a data property; an
            // annotation property has none here.
            "children" => {
                let mut out = HashSet::new();
                for o in objects {
                    match o {
                        Obj::Entity(Kind::Class, c) => {
                            out.extend(ix().subs.get(c).into_iter().flatten().map(class_object))
                        }
                        Obj::Entity(Kind::ObjectProperty, p) => {
                            let p = self.object_property(p);
                            out.extend(ix().op_subs.get(&p).into_iter().flatten().map(property_object))
                        }
                        Obj::Entity(Kind::DataProperty, d) => out.extend(
                            ix().dp_subs.get(d).into_iter().flatten().map(|x| Obj::Entity(Kind::DataProperty, x.clone())),
                        ),
                        _ => {}
                    }
                }
                out
            }
            "classes" => kind_of(Kind::Class),
            "complement" => self.objects().difference(objects).cloned().collect(),
            "data-properties" => kind_of(Kind::DataProperty),
            "descendants" => self.descendants(objects),
            "equivalents" => {
                let mut out = HashSet::new();
                for o in objects {
                    match o {
                        Obj::Entity(Kind::Class, c) => {
                            out.extend(ix().equivalents.get(c).into_iter().flatten().map(class_object))
                        }
                        Obj::Entity(Kind::DataProperty, d) => out.extend(
                            ix().dp_equivalents
                                .get(d)
                                .into_iter()
                                .flatten()
                                .map(|x| Obj::Entity(Kind::DataProperty, x.clone())),
                        ),
                        Obj::Entity(Kind::ObjectProperty, p) => {
                            let p = self.object_property(p);
                            out.extend(ix().op_equivalents.get(&p).into_iter().flatten().map(property_object))
                        }
                        _ => {}
                    }
                }
                out
            }
            "individuals" => objects
                .iter()
                .filter(|o| matches!(o, Obj::Entity(Kind::NamedIndividual, _) | Obj::Anonymous(_)))
                .cloned()
                .collect(),
            "instances" => {
                let mut out = HashSet::new();
                for o in objects {
                    if let Obj::Entity(Kind::Class, c) = o {
                        out.extend(ix().instances.get(c).into_iter().flatten().map(individual_object));
                    }
                }
                out
            }
            "named" => objects.iter().filter(|o| matches!(o, Obj::Entity(..))).cloned().collect(),
            "object-properties" => kind_of(Kind::ObjectProperty),
            // The parents of a class, an object property and a data property; an
            // annotation property has none here.
            "parents" => {
                let mut out = HashSet::new();
                for o in objects {
                    match o {
                        Obj::Entity(Kind::Class, c) => {
                            out.extend(ix().supers.get(c).into_iter().flatten().map(class_object))
                        }
                        Obj::Entity(Kind::ObjectProperty, p) => {
                            let p = self.object_property(p);
                            out.extend(ix().op_supers.get(&p).into_iter().flatten().map(property_object))
                        }
                        Obj::Entity(Kind::DataProperty, d) => out.extend(
                            ix().dp_supers.get(d).into_iter().flatten().map(|x| Obj::Entity(Kind::DataProperty, x.clone())),
                        ),
                        _ => {}
                    }
                }
                out
            }
            "properties" => objects
                .iter()
                .filter(|o| {
                    matches!(
                        o,
                        Obj::Entity(Kind::ObjectProperty | Kind::DataProperty | Kind::AnnotationProperty, _)
                    )
                })
                .cloned()
                .collect(),
            "self" => objects.clone(),
            "types" => {
                let mut out = HashSet::new();
                for o in objects {
                    let individual = match o {
                        Obj::Entity(Kind::NamedIndividual, i) => {
                            Individual::Named(self.model.build.named_individual(i.as_ref()))
                        }
                        Obj::Anonymous(a) => Individual::Anonymous(horned_owl::model::AnonymousIndividual(a.clone())),
                        _ => continue,
                    };
                    out.extend(ix().types.get(&individual).into_iter().flatten().map(class_object));
                }
                out
            }
            "ranges" => {
                let mut out = HashSet::new();
                for o in objects {
                    match o {
                        Obj::Entity(Kind::AnnotationProperty, p) => {
                            out.extend(ix().ap_ranges.get(p).into_iter().flatten().map(|i| Obj::Iri(i.clone())))
                        }
                        Obj::Entity(Kind::DataProperty, d) => {
                            out.extend(ix().dp_ranges.get(d).into_iter().flatten().map(range_object))
                        }
                        Obj::Entity(Kind::ObjectProperty, p) => {
                            let p = self.object_property(p);
                            out.extend(ix().op_ranges.get(&p).into_iter().flatten().map(class_object))
                        }
                        _ => {}
                    }
                }
                out
            }
            "domains" => {
                let mut out = HashSet::new();
                for o in objects {
                    match o {
                        Obj::Entity(Kind::AnnotationProperty, p) => {
                            out.extend(ix().ap_domains.get(p).into_iter().flatten().map(|i| Obj::Iri(i.clone())))
                        }
                        Obj::Entity(Kind::DataProperty, d) => {
                            out.extend(ix().dp_domains.get(d).into_iter().flatten().map(class_object))
                        }
                        Obj::Entity(Kind::ObjectProperty, p) => {
                            let p = self.object_property(p);
                            out.extend(ix().op_domains.get(&p).into_iter().flatten().map(class_object))
                        }
                        _ => {}
                    }
                }
                out
            }
            s if s.contains('=') => self.select_annotated(objects, s)?,
            s if s.contains('<') && s[s.find('<').unwrap()..].contains('>') => select_iri(objects, s)?,
            s if s.contains(':') => {
                let mut parts = s.split(':');
                let prefix = parts.next().unwrap_or_default();
                let Some(pattern) = parts.next() else {
                    bail!("the selector '{s}' names no pattern after its prefix");
                };
                match self.model.context.namespace(prefix) {
                    Some(ns) => select_iri(objects, &format!("{ns}{pattern}"))?,
                    None => {
                        status!("warning: prefix '{prefix}' is not a loaded prefix and will be ignored");
                        objects.clone()
                    }
                }
            }
            s => {
                status!("error: {s} is not a valid selector and will be ignored");
                HashSet::new()
            }
        })
    }

    fn object_property(&self, iri: &str) -> Ope {
        Ope::ObjectProperty(self.model.build.object_property(iri))
    }

    /// Each class's ancestors — every superclass expression, and the named ones'
    /// own, up to `owl:Thing` — and each property's.
    fn ancestors(&self, objects: &HashSet<Obj>) -> HashSet<Obj> {
        let ix = self.index();
        let mut out: HashSet<Obj> = HashSet::new();
        for o in objects {
            match o {
                Obj::Entity(Kind::Class, c) => {
                    let mut stack = vec![c.clone()];
                    while let Some(c) = stack.pop() {
                        for sup in ix.supers.get(&c).into_iter().flatten() {
                            let obj = class_object(sup);
                            if let (ClassExpression::Class(named), false) = (sup, out.contains(&obj)) {
                                out.insert(obj);
                                if named.0.as_ref() != format!("{OWL}Thing") {
                                    stack.push(rc(&named.0));
                                }
                            } else if !matches!(sup, ClassExpression::Class(_)) {
                                out.insert(obj);
                            }
                        }
                    }
                }
                Obj::Entity(Kind::AnnotationProperty, p) => self.property_ancestors(&ix.ap_supers, Kind::AnnotationProperty, p, None, &mut out),
                Obj::Entity(Kind::DataProperty, d) => {
                    self.property_ancestors(&ix.dp_supers, Kind::DataProperty, d, Some(&format!("{OWL}topDataProperty")), &mut out)
                }
                Obj::Entity(Kind::ObjectProperty, p) => {
                    let top = format!("{OWL}topObjectProperty");
                    let mut seen: HashSet<RcStr> = HashSet::new();
                    let mut stack = vec![p.clone()];
                    while let Some(p) = stack.pop() {
                        for sup in ix.op_supers.get(&self.object_property(&p)).into_iter().flatten() {
                            out.insert(property_object(sup));
                            if let Ope::ObjectProperty(q) = sup {
                                if q.0.as_ref() != top && seen.insert(rc(&q.0)) {
                                    stack.push(rc(&q.0));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The named super-properties of `p`, and theirs, up to `top`.
    fn property_ancestors(
        &self,
        supers: &HashMap<RcStr, Vec<RcStr>>,
        kind: Kind,
        p: &RcStr,
        top: Option<&str>,
        out: &mut HashSet<Obj>,
    ) {
        let mut seen: HashSet<RcStr> = HashSet::new();
        let mut stack = vec![p.clone()];
        while let Some(p) = stack.pop() {
            for sup in supers.get(&p).into_iter().flatten() {
                out.insert(Obj::Entity(kind, sup.clone()));
                if Some(sup.as_ref()) != top && seen.insert(sup.clone()) {
                    stack.push(sup.clone());
                }
            }
        }
    }

    /// Each class's descendants — every subclass expression, and the named
    /// ones' own — and each property's. An annotation property's direct
    /// sub-properties are followed by the ancestors of each one that has
    /// sub-properties of its own.
    fn descendants(&self, objects: &HashSet<Obj>) -> HashSet<Obj> {
        let ix = self.index();
        let mut out: HashSet<Obj> = HashSet::new();
        for o in objects {
            match o {
                Obj::Entity(Kind::Class, c) => {
                    let mut stack = vec![c.clone()];
                    while let Some(c) = stack.pop() {
                        for sub in ix.subs.get(&c).into_iter().flatten() {
                            let obj = class_object(sub);
                            match sub {
                                ClassExpression::Class(named) => {
                                    if out.insert(obj) && ix.subs.contains_key(named.0.as_ref()) {
                                        stack.push(rc(&named.0));
                                    }
                                }
                                _ => {
                                    out.insert(obj);
                                }
                            }
                        }
                    }
                }
                Obj::Entity(Kind::AnnotationProperty, p) => {
                    for sub in ix.ap_subs.get(p).into_iter().flatten() {
                        out.insert(Obj::Entity(Kind::AnnotationProperty, sub.clone()));
                        if ix.ap_subs.contains_key(sub) {
                            self.property_ancestors(&ix.ap_supers, Kind::AnnotationProperty, sub, None, &mut out);
                        }
                    }
                }
                Obj::Entity(Kind::DataProperty, d) => {
                    let mut seen: HashSet<RcStr> = HashSet::new();
                    let mut stack = vec![d.clone()];
                    while let Some(d) = stack.pop() {
                        for sub in ix.dp_subs.get(&d).into_iter().flatten() {
                            out.insert(Obj::Entity(Kind::DataProperty, sub.clone()));
                            if ix.dp_subs.contains_key(sub) && seen.insert(sub.clone()) {
                                stack.push(sub.clone());
                            }
                        }
                    }
                }
                Obj::Entity(Kind::ObjectProperty, p) => {
                    let mut seen: HashSet<RcStr> = HashSet::new();
                    let mut stack = vec![p.clone()];
                    while let Some(p) = stack.pop() {
                        for sub in ix.op_subs.get(&self.object_property(&p)).into_iter().flatten() {
                            out.insert(property_object(sub));
                            if let Ope::ObjectProperty(q) = sub {
                                if ix.op_subs.contains_key(sub) && seen.insert(rc(&q.0)) {
                                    stack.push(rc(&q.0));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The entities of `objects` carrying an annotation `PROP=VALUE` names:
    /// asserted on the entity, or annotating one of the entity's own axioms.
    fn select_annotated(&self, objects: &HashSet<Obj>, selector: &str) -> Result<HashSet<Obj>> {
        let wanted = self.annotations(selector)?;
        let ix = self.index();
        let about = self.about();
        let mut out = HashSet::new();
        for o in objects {
            let Obj::Entity(_, iri) = o else { continue };
            let asserted = ix.assertions.get(iri).into_iter().flatten().any(|n| match &self.axioms[*n].component {
                Component::AnnotationAssertion(a) => {
                    wanted.contains(&(rc(&a.ann.ap.0), value_key(&a.ann.av)))
                }
                _ => false,
            });
            let annotating = about.get(o).into_iter().flatten().any(|n| {
                self.axioms[*n].ann.iter().any(|a| wanted.contains(&(rc(&a.ap.0), value_key(&a.av))))
            });
            if asserted || annotating {
                out.insert(o.clone());
            }
        }
        Ok(out)
    }

    /// The annotations a `PROP=VALUE` selector names, as property and value. A
    /// value is a language tag (`@en`: every literal of the property in that
    /// language), a datatype (`^^xsd:date`: every literal of the property of
    /// that type), an IRI in angle brackets, a pattern (`~'regex'`: every
    /// literal of the property it matches whole), a quoted literal, or else a
    /// CURIE or IRI.
    fn annotations(&self, selector: &str) -> Result<HashSet<(RcStr, ValueKey)>> {
        let mut parts = selector.split('=');
        let property = parts.next().unwrap_or_default();
        let value = parts.next().unwrap_or_default();
        let Some(property) = select::iri(self.model, property) else {
            bail!("INVALID IRI ERROR annotation property \"{property}\" is not a valid CURIE or IRI");
        };
        let property = RcStr::from(property.as_str());
        let iri_value = |iri: &str| (0u8, iri.to_string(), String::new());
        let of_property = |test: &dyn Fn(&Literal<RcStr>) -> bool| -> HashSet<(RcStr, ValueKey)> {
            self.axioms
                .iter()
                .filter_map(|ac| match &ac.component {
                    Component::AnnotationAssertion(a) if a.ann.ap.0.as_ref() == property.as_ref() => match &a.ann.av {
                        AnnotationValue::Literal(l) if test(l) => {
                            Some((property.clone(), value_key(&a.ann.av)))
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        };
        if let Some(lang) = value.strip_prefix('@') {
            return Ok(of_property(&|l| match l {
                Literal::Language { lang: tag, .. } => tag.eq_ignore_ascii_case(lang.trim()),
                _ => lang.trim().is_empty(),
            }));
        }
        if let Some(datatype) = value.strip_prefix("^^") {
            let datatype = datatype.replace(['<', '>'], "");
            let datatype = select::iri(self.model, &datatype).unwrap_or(datatype);
            return Ok(of_property(&|l| literal_datatype(l, self.plain) == datatype));
        }
        if value.contains('<') && value.contains('>') && !value.contains("^^") {
            let inner = value.get(1..value.len().saturating_sub(1)).unwrap_or_default();
            let Some(iri) = select::iri(self.model, inner) else {
                bail!("INVALID IRI ERROR annotation value (IRI) \"{inner}\" is not a valid CURIE or IRI");
            };
            return Ok(HashSet::from([(property, iri_value(&iri))]));
        }
        if value.contains("~'") {
            let pattern = value.split('\'').nth(1).unwrap_or_default();
            let regex = crate::dosdp::java::Regex::new(&format!("\\A(?:{pattern})\\z"))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(of_property(&|l| regex.find(l.literal()).ok().flatten().is_some()));
        }
        if value.contains('\'') {
            let literal = self.quoted_literal(value)?;
            return Ok(HashSet::from([(property, value_key(&AnnotationValue::Literal(literal)))]));
        }
        let Some(iri) = select::iri(self.model, value) else {
            bail!("INVALID IRI ERROR annotation value (CURIE) \"{value}\" is not a valid CURIE or IRI");
        };
        Ok(HashSet::from([(property, iri_value(&iri))]))
    }

    /// The literal a quoted selector value names: `'text'^^type`, `'text'@lang`,
    /// or the text with its quotes dropped. A boolean, double, float or integer
    /// is read as that type and written as the type writes it.
    fn quoted_literal(&self, value: &str) -> Result<Literal<RcStr>> {
        let b = &self.model.build;
        let typed = |text: String, datatype: &str| Literal::Datatype { literal: text, datatype_iri: b.iri(datatype) };
        if let Some((content, datatype)) = value.strip_prefix('\'').and_then(|v| v.rsplit_once("'^^")) {
            let mut id = datatype.to_string();
            if id.starts_with('<') && id.ends_with('>') {
                id = id.replace(['<', '>'], "");
            }
            let Some(datatype) = select::iri(self.model, &id) else {
                bail!("INVALID IRI ERROR datatype \"{id}\" is not a valid CURIE or IRI");
            };
            let wrong = |kind: &str| anyhow::anyhow!("LITERAL VALUE ERROR {id} is not a {kind} value");
            return Ok(match datatype.strip_prefix(XSD) {
                Some("boolean") => {
                    let v = if content.eq_ignore_ascii_case("true") {
                        "true"
                    } else if content.eq_ignore_ascii_case("false") {
                        "false"
                    } else {
                        return Err(wrong("boolean"));
                    };
                    typed(v.to_string(), &datatype)
                }
                Some("double") => {
                    let v = crate::java_number::parse_double(content).ok_or_else(|| wrong("double"))?;
                    typed(crate::java_number::double_to_string(v), &datatype)
                }
                Some("float") => {
                    let v = crate::java_number::parse_float(content).ok_or_else(|| wrong("float"))?;
                    typed(crate::java_number::float_to_string(v), &datatype)
                }
                Some("integer") => {
                    let v = crate::java_number::parse_int(content).ok_or_else(|| wrong("integer"))?;
                    typed(v.to_string(), &datatype)
                }
                _ => typed(content.to_string(), &datatype),
            });
        }
        if let Some((content, lang)) = value.strip_prefix('\'').and_then(|v| v.rsplit_once("'@")) {
            return Ok(Literal::Language { literal: content.to_string(), lang: lang.to_string() });
        }
        Ok(typed(value.replace('\'', ""), &format!("{XSD}string")))
    }

    /// The annotation assertions on each named class, property, datatype and
    /// named individual of `objects`.
    pub fn annotation_axioms(&self, objects: &HashSet<Obj>) -> Vec<Ac> {
        let ix = self.index();
        let mut out = Vec::new();
        for o in objects {
            if let Obj::Entity(_, iri) = o {
                out.extend(ix.assertions.get(iri).into_iter().flatten().map(|n| self.axioms[*n].clone()));
            }
        }
        out
    }
}

/// An annotation value as it compares: an IRI, an anonymous individual, or a
/// literal by its lexical form and its language or datatype. A literal with
/// neither is the `xsd:string` literal of its text, whatever datatype the
/// document gives it in a signature.
type ValueKey = (u8, String, String);

fn value_key(value: &AnnotationValue<RcStr>) -> ValueKey {
    match value {
        AnnotationValue::IRI(i) => (0, i.to_string(), String::new()),
        AnnotationValue::AnonymousIndividual(a) => (1, a.0.to_string(), String::new()),
        AnnotationValue::Literal(Literal::Language { literal, lang }) => (3, literal.clone(), lang.to_ascii_lowercase()),
        AnnotationValue::Literal(Literal::Simple { literal }) => (2, literal.clone(), format!("{XSD}string")),
        AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri }) => {
            (2, literal.clone(), datatype_iri.to_string())
        }
    }
}

/// The datatype a literal has: its own; `rdf:PlainLiteral` for one with a
/// language tag; for an untyped one, the document's ([`plain_datatype`]).
fn literal_datatype(l: &Literal<RcStr>, plain: &str) -> String {
    match l {
        Literal::Datatype { datatype_iri, .. } => datatype_iri.to_string(),
        Literal::Language { .. } => RDF_PLAIN_LITERAL.to_string(),
        Literal::Simple { .. } => plain.to_string(),
    }
}

/// The datatype an untyped literal has in `model`: `xsd:string` where the
/// document types its untyped literals so, `rdf:PlainLiteral` otherwise.
pub fn plain_datatype(model: &Model) -> &'static str {
    if model.plain_literals_typed {
        "http://www.w3.org/2001/XMLSchema#string"
    } else {
        RDF_PLAIN_LITERAL
    }
}

/// The entities of `objects` whose IRI matches a wildcard pattern: `*` is any
/// run and `?` any one character or none, a `.` is itself, and a pattern
/// starting `~` is a regular expression. Angle brackets around the pattern are
/// dropped.
fn select_iri(objects: &HashSet<Obj>, selector: &str) -> Result<HashSet<Obj>> {
    if !(selector.contains('~') || selector.contains('*') || selector.contains('?')) {
        bail!("INVALID IRI PATTERN ERROR the pattern '{selector}' must contain at least one wildcard character.");
    }
    let mut pattern = selector;
    if pattern.starts_with('<') && pattern.ends_with('>') {
        pattern = &pattern[1..pattern.len() - 1];
    }
    let regex = match pattern.strip_prefix('~') {
        Some(raw) => raw.to_string(),
        None => pattern.replace('.', "\\.").replace('?', ".?").replace('*', ".*"),
    };
    let regex =
        crate::dosdp::java::Regex::new(&format!("\\A(?:{regex})\\z")).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut out = HashSet::new();
    for o in objects {
        if let Obj::Entity(_, iri) = o {
            if regex.find(iri).map_err(|e| anyhow::anyhow!("{e}"))?.is_some() {
                out.insert(o.clone());
            }
        }
    }
    Ok(out)
}

// ── selectors and axiom types ───────────────────────────────────────────────

/// One `--select` value's selectors: each run of non-space characters, a
/// `PROP=…'…'…` selector whole however many spaces its quotes hold, and no
/// quote standing alone.
pub fn split_selects(value: &str) -> Vec<String> {
    let re = crate::dosdp::java::Regex::new(r"([^\s]+=.*'[^']+'[^\s']*|[^\s']+)").expect("a fixed pattern");
    re.find_all(value)
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m.group(1).map(|s| s.trim().to_string()))
        .collect()
}

/// The `--axioms` values, each split at its spaces, or `all` when none is given.
pub fn axiom_selectors(values: &[String]) -> Vec<String> {
    if values.is_empty() {
        return vec!["all".to_string()];
    }
    values.iter().flat_map(|v| v.split(' ')).map(str::to_string).collect()
}

/// Whether the `--axioms` values name a selector that takes axioms whatever the
/// objects: `internal`, `external` or a tautology test.
fn has_axiom_selector(selectors: &[String]) -> bool {
    selectors
        .iter()
        .any(|s| s.eq_ignore_ascii_case("internal") || s.eq_ignore_ascii_case("external") || s.contains("tautologies"))
}

/// The namespace `--axioms` asks for first: `internal` or `external`.
pub fn namespace_flags(selectors: &[String]) -> (bool, bool) {
    for s in selectors {
        if s.eq_ignore_ascii_case("internal") {
            return (true, false);
        }
        if s.eq_ignore_ascii_case("external") {
            return (false, true);
        }
    }
    (false, false)
}

/// The base namespaces `--base-iri` names: a value holding a `:` as written,
/// any other as the namespace of the prefix it names.
pub fn base_namespaces(model: &Model, values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for v in values {
        if v.contains(':') {
            out.push(v.clone());
        } else if let Some(ns) = model.context.namespace(v) {
            out.push(ns.to_string());
        } else {
            status!("error: unknown prefix: '{v}'");
        }
    }
    out
}

/// Whether `component` has an axiom type `selector` names: `all`, `logical`,
/// `annotation`, `subclass`, `subproperty`, `equivalent`, `disjoint`, `type`,
/// `abox`, `tbox`, `rbox` or `declaration`, in any case, or one axiom type by
/// its object-model name.
fn has_type(component: &Component<RcStr>, selector: &str) -> Result<bool> {
    let group = selector.to_ascii_lowercase();
    let group = group.as_str();
    if matches!(
        group,
        "all" | "logical" | "annotation" | "subclass" | "subproperty" | "equivalent" | "disjoint" | "type" | "abox"
            | "tbox" | "rbox" | "declaration"
    ) {
        return Ok(select::axiom_in_category(component, group, &[]));
    }
    if select::is_axiom_category(selector) && !matches!(group, "internal" | "external" | "structural-tautologies") {
        return Ok(select::axiom_in_category(component, selector, &[]));
    }
    bail!("AXIOM TYPE ERROR {selector} is not a valid axiom type")
}

/// Whether an axiom is a structural tautology: a subclass axiom whose
/// superclass is `owl:Thing`, whose subclass is `owl:Nothing` or whose two sides
/// are one; an equivalence of fewer than two classes; an assertion of
/// `owl:Thing`, `owl:topObjectProperty` or `owl:topDataProperty`; or the
/// declaration of a built-in entity.
fn is_structural_tautology(component: &Component<RcStr>) -> bool {
    use Component as X;
    let is = |ce: &CE, name: &str| matches!(ce, ClassExpression::Class(c) if c.0.as_ref() == format!("{OWL}{name}"));
    match component {
        X::SubClassOf(a) => is(&a.sup, "Thing") || is(&a.sub, "Nothing") || a.sub == a.sup,
        X::EquivalentClasses(a) => a.0.iter().collect::<HashSet<_>>().len() < 2,
        X::ClassAssertion(a) => is(&a.ce, "Thing"),
        X::ObjectPropertyAssertion(a) => {
            matches!(&a.ope, Ope::ObjectProperty(p) if p.0.as_ref() == format!("{OWL}topObjectProperty"))
        }
        X::DataPropertyAssertion(a) => a.dp.0.as_ref() == format!("{OWL}topDataProperty"),
        _ => match crate::io::entities::declaration(component) {
            Some((kind, iri)) => is_builtin(kind, iri),
            None => false,
        },
    }
}

/// Whether an entity is one of the OWL 2 vocabulary's own.
fn is_builtin(kind: Kind, iri: &str) -> bool {
    const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    match kind {
        Kind::Class => iri == format!("{OWL}Thing") || iri == format!("{OWL}Nothing"),
        Kind::ObjectProperty => iri == format!("{OWL}topObjectProperty") || iri == format!("{OWL}bottomObjectProperty"),
        Kind::DataProperty => iri == format!("{OWL}topDataProperty") || iri == format!("{OWL}bottomDataProperty"),
        Kind::AnnotationProperty => {
            [
                format!("{RDFS}label"),
                format!("{RDFS}comment"),
                format!("{RDFS}seeAlso"),
                format!("{RDFS}isDefinedBy"),
                format!("{OWL}deprecated"),
                format!("{OWL}versionInfo"),
                format!("{OWL}priorVersion"),
                format!("{OWL}backwardCompatibleWith"),
                format!("{OWL}incompatibleWith"),
            ]
            .iter()
            .any(|b| b == iri)
        }
        Kind::Datatype => {
            iri == format!("{RDFS}Literal")
                || iri == format!("{RDF}PlainLiteral")
                || iri == format!("{RDF}XMLLiteral")
                || iri == format!("{RDF}langString")
                || iri == format!("{OWL}real")
                || iri == format!("{OWL}rational")
                || iri.strip_prefix(XSD).is_some_and(|local| {
                    matches!(
                        local,
                        "string" | "normalizedString" | "token" | "language" | "Name" | "NCName" | "NMTOKEN"
                            | "boolean" | "decimal" | "integer" | "nonNegativeInteger" | "nonPositiveInteger"
                            | "positiveInteger" | "negativeInteger" | "long" | "int" | "short" | "byte"
                            | "unsignedLong" | "unsignedInt" | "unsignedShort" | "unsignedByte" | "double"
                            | "float" | "hexBinary" | "base64Binary" | "anyURI" | "dateTime" | "dateTimeStamp"
                    )
                })
        }
        Kind::NamedIndividual => false,
    }
}

// ── judging axioms ──────────────────────────────────────────────────────────

/// How axioms are judged against a selection.
pub struct Judge<'a> {
    /// The `--axioms` values, split ([`axiom_selectors`]).
    pub selectors: &'a [String],
    /// The base namespaces `internal` and `external` judge an axiom's subjects by.
    pub base: &'a [String],
    /// Any selected object takes an axiom; otherwise every one must be selected.
    pub partial: bool,
    /// Judge by the IRIs an axiom names, not by its objects.
    pub named_only: bool,
    /// Whether an annotation's value is one of the objects of the axiom it is
    /// on, or of the annotation assertion it states, where any selected object
    /// takes an axiom.
    pub annotation_values: bool,
    /// The datatype an untyped literal has in the document ([`plain_datatype`]).
    pub plain: &'static str,
}

/// The axioms of `axioms` `judge` takes for `objects`. An axiom whose objects
/// are selected but whose annotations are not comes out without its
/// annotations, where every object must be selected.
pub fn judge_axioms(axioms: &[&Ac], objects: &HashSet<Obj>, judge: &Judge) -> Result<Vec<Ac>> {
    let iris: HashSet<&str> = objects.iter().filter_map(Obj::named).collect();
    let mut out: Vec<Ac> = Vec::new();
    let mut internal = false;
    let mut external = false;
    for selector in judge.selectors {
        if selector.eq_ignore_ascii_case("internal") {
            if external {
                status!("error: ignoring 'internal' axiom selector - 'internal' and 'external' together will remove all axioms");
                continue;
            }
            out.extend(axioms.iter().filter(|ac| select::axiom_in_category(&ac.component, "internal", judge.base)).map(|ac| (*ac).clone()));
            internal = true;
            continue;
        }
        if selector.eq_ignore_ascii_case("external") {
            if internal {
                status!("error: ignoring 'external' axiom selector - 'internal' and 'external' together will remove all axioms");
            }
            out.extend(axioms.iter().filter(|ac| select::axiom_in_category(&ac.component, "external", judge.base)).map(|ac| (*ac).clone()));
            external = true;
            continue;
        }
        if selector.eq_ignore_ascii_case("structural-tautologies") {
            out.extend(axioms.iter().filter(|ac| is_structural_tautology(&ac.component)).map(|ac| (*ac).clone()));
            continue;
        }
        if selector.eq_ignore_ascii_case("tautologies") {
            bail!("--axioms tautologies asks a reasoner whether each axiom holds of every ontology, which owlmake does not run; --axioms structural-tautologies tests their form");
        }
        for ac in axioms {
            if !has_type(&ac.component, selector)? {
                continue;
            }
            match (judge.partial, judge.named_only) {
                (false, false) => complete(ac, objects, &iris, judge.plain, &mut out),
                (false, true) => complete_named(ac, &iris, judge.plain, &mut out),
                (true, false) => {
                    if partial(ac, objects, &iris, judge.annotation_values, judge.plain) {
                        out.push((*ac).clone());
                    }
                }
                (true, true) => {
                    if partial_named(ac, &iris, judge.plain) {
                        out.push((*ac).clone());
                    }
                }
            }
        }
    }
    Ok(out)
}

/// An annotation's value as an object of a selection, where it can be one.
fn value_selected(value: &AnnotationValue<RcStr>, objects: &HashSet<Obj>, iris: &HashSet<&str>) -> bool {
    match value {
        AnnotationValue::IRI(i) => iris.contains(i.as_ref()) || objects.contains(&Obj::Iri(rc(i))),
        AnnotationValue::AnonymousIndividual(a) => objects.contains(&Obj::Anonymous(a.0.clone())),
        AnnotationValue::Literal(_) => false,
    }
}

/// Whether an annotation subject is selected.
fn subject_selected(subject: &AnnotationSubject<RcStr>, objects: &HashSet<Obj>, iris: &HashSet<&str>) -> bool {
    match subject {
        AnnotationSubject::IRI(i) => iris.contains(i.as_ref()),
        AnnotationSubject::AnonymousIndividual(a) => objects.contains(&Obj::Anonymous(a.0.clone())),
    }
}

fn property_selected(ap: &horned_owl::model::AnnotationProperty<RcStr>, objects: &HashSet<Obj>, iris: &HashSet<&str>) -> bool {
    iris.contains(ap.0.as_ref()) || objects.contains(&Obj::Entity(Kind::AnnotationProperty, rc(&ap.0)))
}

/// Every object of the axiom selected: kept whole when each of its annotations
/// has its property and its value selected, and without them otherwise. An
/// annotation assertion's objects are its subject and property, and its value
/// where that is not a literal.
fn complete(ac: &Ac, objects: &HashSet<Obj>, iris: &HashSet<&str>, plain: &'static str, out: &mut Vec<Ac>) {
    let matched = match &ac.component {
        Component::AnnotationAssertion(a) => {
            subject_selected(&a.subject, objects, iris)
                && iris.contains(a.ann.ap.0.as_ref())
                && (matches!(a.ann.av, AnnotationValue::Literal(_)) || value_selected(&a.ann.av, objects, iris))
        }
        c => axiom_objects(c, None, plain).iter().all(|o| objects.contains(o)),
    };
    if !matched {
        return;
    }
    let annotated = ac
        .ann
        .iter()
        .all(|a| property_selected(&a.ap, objects, iris) && value_selected(&a.av, objects, iris));
    if annotated {
        out.push(ac.clone());
    } else {
        out.push(AnnotatedComponent { component: ac.component.clone(), ann: BTreeSet::new() });
    }
}

/// Every IRI the axiom names selected: kept whole when its annotations'
/// properties and IRI values are too, and without its annotations otherwise.
fn complete_named(ac: &Ac, iris: &HashSet<&str>, plain: &'static str, out: &mut Vec<Ac>) {
    let names = signature_iris(&ac.component, plain);
    let unannotated = names.iter().all(|i| iris.contains(i.as_ref()));
    let annotations_named = ac.ann.iter().all(|a| {
        iris.contains(a.ap.0.as_ref())
            && match &a.av {
                AnnotationValue::IRI(i) => iris.contains(i.as_ref()),
                _ => true,
            }
    });
    if unannotated && annotations_named {
        out.push(ac.clone());
    } else if unannotated {
        out.push(AnnotatedComponent { component: ac.component.clone(), ann: BTreeSet::new() });
    }
}

/// Any object of the axiom or of its annotations selected.
fn partial(
    ac: &Ac,
    objects: &HashSet<Obj>,
    iris: &HashSet<&str>,
    annotation_values: bool,
    plain: &'static str,
) -> bool {
    let axiom = match &ac.component {
        Component::AnnotationAssertion(a) => {
            subject_selected(&a.subject, objects, iris)
                || iris.contains(a.ann.ap.0.as_ref())
                || (annotation_values && value_selected(&a.ann.av, objects, iris))
        }
        c => axiom_objects(c, None, plain).iter().any(|o| objects.contains(o)),
    };
    axiom
        || ac.ann.iter().any(|a| {
            property_selected(&a.ap, objects, iris) || (annotation_values && value_selected(&a.av, objects, iris))
        })
}

/// Any IRI the axiom or its annotations name selected.
fn partial_named(ac: &Ac, iris: &HashSet<&str>, plain: &'static str) -> bool {
    signature_iris(&ac.component, plain).iter().any(|i| iris.contains(i.as_ref()))
        || ac.ann.iter().any(|a| {
            iris.contains(a.ap.0.as_ref())
                || matches!(&a.av, AnnotationValue::IRI(i) if iris.contains(i.as_ref()))
        })
}

// ── the object set a command starts from ────────────────────────────────────

/// What a command selects with, beyond its terms.
pub struct Request<'a> {
    /// The IRIs `--term` and `--term-file` name.
    pub terms: &'a HashSet<String>,
    /// The IRIs `--include-term(s)` and `--exclude-term(s)` name.
    pub include: &'a HashSet<String>,
    pub exclude: &'a HashSet<String>,
    /// The `--select` groups, after the command has taken its own keywords out.
    pub groups: &'a [Vec<String>],
    /// Whether `--select` was given at all.
    pub selected: bool,
    /// The `--axioms` values, split.
    pub axiom_selectors: &'a [String],
    pub allow_punning: bool,
}

/// The objects a command acts on: the entities its terms name — or, when no
/// term names an IRI, every object of the ontology — mapped through its
/// `--select` groups, with the included entities added and the excluded ones
/// taken out. A term list that names only IRIs the ontology does not use
/// selects nothing, and so does a `--select` that leaves no group; neither
/// rule applies when `--axioms` names `internal`, `external` or a tautology
/// test, which take axioms whatever the objects.
pub fn objects(sel: &Selection, req: &Request) -> Result<HashSet<Obj>> {
    let mut objects = HashSet::new();
    if !req.terms.is_empty() {
        objects = sel.entities(req.terms, req.allow_punning);
    }
    let by_axioms = has_axiom_selector(req.axiom_selectors);
    if req.selected && req.groups.is_empty() && objects.is_empty() && !by_axioms {
        return Ok(objects);
    }
    if objects.is_empty() && !req.terms.is_empty() && !by_axioms {
        return Ok(objects);
    }
    if objects.is_empty() {
        objects = sel.objects().clone();
    }
    let mut related = sel.select_groups(objects, req.groups)?;
    let include = sel.entities(req.include, req.allow_punning);
    related.extend(include.iter().cloned());
    for e in sel.entities(req.exclude, req.allow_punning) {
        related.remove(&e);
    }
    related.extend(include);
    Ok(related)
}

/// The IRIs of the named objects of `objects`.
pub fn named_iris(objects: &HashSet<Obj>) -> HashSet<String> {
    objects.iter().filter_map(Obj::named).map(str::to_string).collect()
}

/// The IRIs of the entities the ontology's axioms name, the imports' too when
/// `imports`: what OWL API's `containsEntityInSignature` asks of an IRI.
pub fn signature_entity_iris(model: &Model, imports: bool) -> HashSet<String> {
    let plain = plain_datatype(model);
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if !imports && model.imported_components.contains(ac) {
            continue;
        }
        for object in signature(&ac.component, Some(&ac.ann), plain) {
            if let Obj::Entity(_, iri) = object {
                out.insert(iri.to_string());
            }
        }
    }
    out
}

/// Whether an axiom stays under `--axioms internal` or `external`, the first
/// named: its subject inside the base namespaces, or outside them.
pub fn in_namespace(axiom: &Component<RcStr>, internal: bool, external: bool, base: &[String]) -> bool {
    let inside = select::axiom_in_category(axiom, "internal", base);
    !(internal && !inside) && !(external && inside)
}
