//! The natural order of OWL objects, which every sorted section of the OWL/XML
//! and Manchester writers follows.
//!
//! Objects compare by KIND first, then field by field. Each kind has a fixed
//! index: an IRI 0; the entities 1001–1007 (class, object property, inverse
//! object property, data property, named individual, annotation property,
//! anonymous individual); an axiom 2000 plus its axiom-type index; the class
//! expressions 3001–3017; datatypes, data ranges, facet restrictions and
//! literals 4001–4008, except a data union, which ranks 2005; an annotation 5001;
//! rule atoms and arguments 6001–6010.
//!
//! Within a kind:
//!
//! * an IRI compares by namespace, then by local name, split where its longest
//!   NCName suffix starts — so `obo:EX_0000002` and `obo:EX_0000010` order by
//!   `EX_0000002` against `EX_0000010`, but an IRI whose suffix is not an NCName
//!   compares as one namespace;
//! * text compares as UTF-16 code units;
//! * a set-valued field (operands, members, facet restrictions) compares its
//!   members in sorted order, one by one, and then by size — members that compare
//!   equal count once;
//! * a literal compares by datatype, then lexical form, then language tag. An
//!   untyped literal keys as `rdf:PlainLiteral`, or as `xsd:string` in a document
//!   that types its untyped literals (`Model::plain_literals_typed`); a
//!   language-tagged one as `rdf:PlainLiteral`;
//! * two axioms equal on their fields compare by their annotations, sorted.
//!
//! An annotation compares by property and value only: two annotations that
//! differ only in their own annotations are equal here.

use std::cmp::Ordering;

use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, Atom, ClassExpression as CE,
    Component, DArgument, DataRange as DR, FacetRestriction, IArgument, Individual, Literal,
    ObjectPropertyExpression as OPE, PropertyExpression, RcStr, SubObjectPropertyExpression as SOPE,
};
use horned_owl::vocab::Facet;

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// Compare two strings as sequences of UTF-16 code units.
pub fn str_cmp(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.cmp(b);
    }
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Split an IRI into namespace and local name at the start of its longest
/// NCName suffix. With no such suffix the whole IRI is the namespace.
pub fn iri_split(iri: &str) -> (&str, &str) {
    crate::owlapi_hash::iri_split(iri)
}

/// Compare two IRIs: namespace first, then local name.
pub fn iri_cmp(a: &str, b: &str) -> Ordering {
    let (na, ra) = iri_split(a);
    let (nb, rb) = iri_split(b);
    str_cmp(na, nb).then_with(|| str_cmp(ra, rb))
}

/// Sort `items` in place by `cmp` and drop every member that compares equal to
/// the one before it — the members of a sorted set.
pub fn sorted_set<'a, T>(items: impl IntoIterator<Item = &'a T>, cmp: impl Fn(&T, &T) -> Ordering) -> Vec<&'a T>
where
    T: 'a,
{
    let mut v: Vec<&T> = items.into_iter().collect();
    v.sort_by(|a, b| cmp(a, b));
    v.dedup_by(|a, b| cmp(a, b) == Ordering::Equal);
    v
}

/// The natural order, for one document: how its untyped literals key is a
/// property of the document.
#[derive(Clone, Copy, Debug, Default)]
pub struct NaturalOrder {
    /// The document types its untyped literals `xsd:string`.
    pub plain_literals_typed: bool,
}

impl NaturalOrder {
    pub fn new(plain_literals_typed: bool) -> NaturalOrder {
        NaturalOrder { plain_literals_typed }
    }

    /// Compare two sets: members in sorted order, then size.
    fn sets<T>(&self, a: &[T], b: &[T], cmp: impl Fn(&T, &T) -> Ordering) -> Ordering {
        let sa = sorted_set(a, &cmp);
        let sb = sorted_set(b, &cmp);
        for (x, y) in sa.iter().zip(sb.iter()) {
            let c = cmp(x, y);
            if c != Ordering::Equal {
                return c;
            }
        }
        sa.len().cmp(&sb.len())
    }

    /// Compare two lists element by element, then by length.
    fn lists<T>(&self, a: &[T], b: &[T], cmp: impl Fn(&T, &T) -> Ordering) -> Ordering {
        for (x, y) in a.iter().zip(b.iter()) {
            let c = cmp(x, y);
            if c != Ordering::Equal {
                return c;
            }
        }
        a.len().cmp(&b.len())
    }

    /// The datatype IRI a literal keys as.
    pub fn literal_datatype<'a>(&self, l: &'a Literal<RcStr>) -> &'a str {
        match l {
            Literal::Datatype { datatype_iri, .. } => datatype_iri.as_ref(),
            Literal::Language { .. } => RDF_PLAIN_LITERAL,
            Literal::Simple { .. } => {
                if self.plain_literals_typed {
                    XSD_STRING
                } else {
                    RDF_PLAIN_LITERAL
                }
            }
        }
    }

    pub fn literal(&self, a: &Literal<RcStr>, b: &Literal<RcStr>) -> Ordering {
        let lang = |l: &Literal<RcStr>| -> String {
            match l {
                Literal::Language { lang, .. } => lang.clone(),
                _ => String::new(),
            }
        };
        iri_cmp(self.literal_datatype(a), self.literal_datatype(b))
            .then_with(|| str_cmp(a.literal(), b.literal()))
            .then_with(|| str_cmp(&lang(a), &lang(b)))
    }

    /// Named individuals (1005) precede anonymous ones (1007); each compares by
    /// its IRI or node id.
    pub fn individual(&self, a: &Individual<RcStr>, b: &Individual<RcStr>) -> Ordering {
        match (a, b) {
            (Individual::Named(x), Individual::Named(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
            (Individual::Anonymous(x), Individual::Anonymous(y)) => str_cmp(x.0.as_ref(), y.0.as_ref()),
            (Individual::Named(_), Individual::Anonymous(_)) => Ordering::Less,
            (Individual::Anonymous(_), Individual::Named(_)) => Ordering::Greater,
        }
    }

    /// An object property (1002) precedes an inverse (1003), which compares by
    /// the property it inverts.
    pub fn ope(&self, a: &OPE<RcStr>, b: &OPE<RcStr>) -> Ordering {
        match (a, b) {
            (OPE::ObjectProperty(x), OPE::ObjectProperty(y))
            | (OPE::InverseObjectProperty(x), OPE::InverseObjectProperty(y)) => {
                iri_cmp(x.0.as_ref(), y.0.as_ref())
            }
            (OPE::ObjectProperty(_), OPE::InverseObjectProperty(_)) => Ordering::Less,
            (OPE::InverseObjectProperty(_), OPE::ObjectProperty(_)) => Ordering::Greater,
        }
    }

    /// The index of a property expression in a key.
    fn pe_index(pe: &PropertyExpression<RcStr>) -> i32 {
        match pe {
            PropertyExpression::ObjectPropertyExpression(OPE::ObjectProperty(_)) => 1002,
            PropertyExpression::ObjectPropertyExpression(OPE::InverseObjectProperty(_)) => 1003,
            PropertyExpression::DataProperty(_) => 1004,
            PropertyExpression::AnnotationProperty(_) => 1006,
        }
    }

    pub fn property_expression(&self, a: &PropertyExpression<RcStr>, b: &PropertyExpression<RcStr>) -> Ordering {
        Self::pe_index(a).cmp(&Self::pe_index(b)).then_with(|| match (a, b) {
            (PropertyExpression::ObjectPropertyExpression(x), PropertyExpression::ObjectPropertyExpression(y)) => {
                self.ope(x, y)
            }
            (PropertyExpression::DataProperty(x), PropertyExpression::DataProperty(y)) => {
                iri_cmp(x.0.as_ref(), y.0.as_ref())
            }
            (PropertyExpression::AnnotationProperty(x), PropertyExpression::AnnotationProperty(y)) => {
                iri_cmp(x.0.as_ref(), y.0.as_ref())
            }
            _ => Ordering::Equal,
        })
    }

    fn ce_index(ce: &CE<RcStr>) -> i32 {
        match ce {
            CE::Class(_) => 1001,
            CE::ObjectIntersectionOf(_) => 3001,
            CE::ObjectUnionOf(_) => 3002,
            CE::ObjectComplementOf(_) => 3003,
            CE::ObjectOneOf(_) => 3004,
            CE::ObjectSomeValuesFrom { .. } => 3005,
            CE::ObjectAllValuesFrom { .. } => 3006,
            CE::ObjectHasValue { .. } => 3007,
            CE::ObjectMinCardinality { .. } => 3008,
            CE::ObjectExactCardinality { .. } => 3009,
            CE::ObjectMaxCardinality { .. } => 3010,
            CE::ObjectHasSelf(_) => 3011,
            CE::DataSomeValuesFrom { .. } => 3012,
            CE::DataAllValuesFrom { .. } => 3013,
            CE::DataHasValue { .. } => 3014,
            CE::DataMinCardinality { .. } => 3015,
            CE::DataExactCardinality { .. } => 3016,
            CE::DataMaxCardinality { .. } => 3017,
        }
    }

    pub fn ce(&self, a: &CE<RcStr>, b: &CE<RcStr>) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let ti = Self::ce_index(a).cmp(&Self::ce_index(b));
        if ti != Ordering::Equal {
            return ti;
        }
        match (a, b) {
            (CE::Class(x), CE::Class(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
            (CE::ObjectIntersectionOf(x), CE::ObjectIntersectionOf(y))
            | (CE::ObjectUnionOf(x), CE::ObjectUnionOf(y)) => self.sets(x, y, |p, q| self.ce(p, q)),
            (CE::ObjectComplementOf(x), CE::ObjectComplementOf(y)) => self.ce(x, y),
            (CE::ObjectOneOf(x), CE::ObjectOneOf(y)) => self.sets(x, y, |p, q| self.individual(p, q)),
            (CE::ObjectSomeValuesFrom { ope: pa, bce: fa }, CE::ObjectSomeValuesFrom { ope: pb, bce: fb })
            | (CE::ObjectAllValuesFrom { ope: pa, bce: fa }, CE::ObjectAllValuesFrom { ope: pb, bce: fb }) => {
                self.ope(pa, pb).then_with(|| self.ce(fa, fb))
            }
            (CE::ObjectHasValue { ope: pa, i: ia }, CE::ObjectHasValue { ope: pb, i: ib }) => {
                self.ope(pa, pb).then_with(|| self.individual(ia, ib))
            }
            (CE::ObjectHasSelf(x), CE::ObjectHasSelf(y)) => self.ope(x, y),
            (
                CE::ObjectMinCardinality { n: na, ope: pa, bce: fa },
                CE::ObjectMinCardinality { n: nb, ope: pb, bce: fb },
            )
            | (
                CE::ObjectExactCardinality { n: na, ope: pa, bce: fa },
                CE::ObjectExactCardinality { n: nb, ope: pb, bce: fb },
            )
            | (
                CE::ObjectMaxCardinality { n: na, ope: pa, bce: fa },
                CE::ObjectMaxCardinality { n: nb, ope: pb, bce: fb },
            ) => self.ope(pa, pb).then_with(|| na.cmp(nb)).then_with(|| self.ce(fa, fb)),
            (CE::DataSomeValuesFrom { dp: pa, dr: ra }, CE::DataSomeValuesFrom { dp: pb, dr: rb })
            | (CE::DataAllValuesFrom { dp: pa, dr: ra }, CE::DataAllValuesFrom { dp: pb, dr: rb }) => {
                iri_cmp(pa.0.as_ref(), pb.0.as_ref()).then_with(|| self.dr(ra, rb))
            }
            (CE::DataHasValue { dp: pa, l: la }, CE::DataHasValue { dp: pb, l: lb }) => {
                iri_cmp(pa.0.as_ref(), pb.0.as_ref()).then_with(|| self.literal(la, lb))
            }
            (
                CE::DataMinCardinality { n: na, dp: pa, dr: ra },
                CE::DataMinCardinality { n: nb, dp: pb, dr: rb },
            )
            | (
                CE::DataExactCardinality { n: na, dp: pa, dr: ra },
                CE::DataExactCardinality { n: nb, dp: pb, dr: rb },
            )
            | (
                CE::DataMaxCardinality { n: na, dp: pa, dr: ra },
                CE::DataMaxCardinality { n: nb, dp: pb, dr: rb },
            ) => iri_cmp(pa.0.as_ref(), pb.0.as_ref()).then_with(|| na.cmp(nb)).then_with(|| self.dr(ra, rb)),
            _ => Ordering::Equal,
        }
    }

    fn dr_index(dr: &DR<RcStr>) -> i32 {
        match dr {
            DR::Datatype(_) => 4001,
            DR::DataComplementOf(_) => 4002,
            DR::DataOneOf(_) => 4003,
            DR::DataIntersectionOf(_) => 4004,
            DR::DataUnionOf(_) => 2005,
            DR::DatatypeRestriction(_, _) => 4006,
        }
    }

    pub fn dr(&self, a: &DR<RcStr>, b: &DR<RcStr>) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let ti = Self::dr_index(a).cmp(&Self::dr_index(b));
        if ti != Ordering::Equal {
            return ti;
        }
        match (a, b) {
            (DR::Datatype(x), DR::Datatype(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
            (DR::DataComplementOf(x), DR::DataComplementOf(y)) => self.dr(x, y),
            (DR::DataOneOf(x), DR::DataOneOf(y)) => self.sets(x, y, |p, q| self.literal(p, q)),
            (DR::DataIntersectionOf(x), DR::DataIntersectionOf(y)) | (DR::DataUnionOf(x), DR::DataUnionOf(y)) => {
                self.sets(x, y, |p, q| self.dr(p, q))
            }
            (DR::DatatypeRestriction(da, fa), DR::DatatypeRestriction(db, fb)) => iri_cmp(da.0.as_ref(), db.0.as_ref())
                .then_with(|| self.sets(fa, fb, |p, q| self.facet_restriction(p, q))),
            _ => Ordering::Equal,
        }
    }

    /// A facet's position among the facets: length, minLength, maxLength,
    /// pattern, minInclusive, minExclusive, maxInclusive, maxExclusive,
    /// totalDigits, fractionDigits, langRange.
    pub fn facet_index(f: &Facet) -> u8 {
        match f {
            Facet::Length => 0,
            Facet::MinLength => 1,
            Facet::MaxLength => 2,
            Facet::Pattern => 3,
            Facet::MinInclusive => 4,
            Facet::MinExclusive => 5,
            Facet::MaxInclusive => 6,
            Facet::MaxExclusive => 7,
            Facet::TotalDigits => 8,
            Facet::FractionDigits => 9,
            Facet::LangRange => 10,
        }
    }

    pub fn facet_restriction(&self, a: &FacetRestriction<RcStr>, b: &FacetRestriction<RcStr>) -> Ordering {
        Self::facet_index(&a.f).cmp(&Self::facet_index(&b.f)).then_with(|| self.literal(&a.l, &b.l))
    }

    /// An IRI (0) precedes an anonymous individual (1007), which precedes a
    /// literal (4008).
    pub fn annotation_value(&self, a: &AnnotationValue<RcStr>, b: &AnnotationValue<RcStr>) -> Ordering {
        fn rank(v: &AnnotationValue<RcStr>) -> u8 {
            match v {
                AnnotationValue::IRI(_) => 0,
                AnnotationValue::AnonymousIndividual(_) => 1,
                AnnotationValue::Literal(_) => 2,
            }
        }
        match (a, b) {
            (AnnotationValue::IRI(x), AnnotationValue::IRI(y)) => iri_cmp(x.as_ref(), y.as_ref()),
            (AnnotationValue::AnonymousIndividual(x), AnnotationValue::AnonymousIndividual(y)) => {
                str_cmp(x.0.as_ref(), y.0.as_ref())
            }
            (AnnotationValue::Literal(x), AnnotationValue::Literal(y)) => self.literal(x, y),
            _ => rank(a).cmp(&rank(b)),
        }
    }

    pub fn annotation_subject(&self, a: &AnnotationSubject<RcStr>, b: &AnnotationSubject<RcStr>) -> Ordering {
        match (a, b) {
            (AnnotationSubject::IRI(x), AnnotationSubject::IRI(y)) => iri_cmp(x.as_ref(), y.as_ref()),
            (AnnotationSubject::AnonymousIndividual(x), AnnotationSubject::AnonymousIndividual(y)) => {
                str_cmp(x.0.as_ref(), y.0.as_ref())
            }
            (AnnotationSubject::IRI(_), AnnotationSubject::AnonymousIndividual(_)) => Ordering::Less,
            (AnnotationSubject::AnonymousIndividual(_), AnnotationSubject::IRI(_)) => Ordering::Greater,
        }
    }

    /// Property, then value.
    pub fn annotation(&self, a: &Annotation<RcStr>, b: &Annotation<RcStr>) -> Ordering {
        iri_cmp(a.ap.0.as_ref(), b.ap.0.as_ref()).then_with(|| self.annotation_value(&a.av, &b.av))
    }

    /// A set of annotations, sorted.
    pub fn sorted_annotations<'a>(
        &self,
        anns: impl IntoIterator<Item = &'a Annotation<RcStr>>,
    ) -> Vec<&'a Annotation<RcStr>> {
        sorted_set(anns, |a, b| self.annotation(a, b))
    }

    fn iarg(&self, a: &IArgument<RcStr>, b: &IArgument<RcStr>) -> Ordering {
        // A variable (6006) precedes an individual argument (6007).
        match (a, b) {
            (IArgument::Variable(x), IArgument::Variable(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
            (IArgument::Individual(x), IArgument::Individual(y)) => self.individual(x, y),
            (IArgument::Variable(_), IArgument::Individual(_)) => Ordering::Less,
            (IArgument::Individual(_), IArgument::Variable(_)) => Ordering::Greater,
        }
    }

    pub(crate) fn darg(&self, a: &DArgument<RcStr>, b: &DArgument<RcStr>) -> Ordering {
        // A variable (6006) precedes a literal argument (6008).
        match (a, b) {
            (DArgument::Variable(x), DArgument::Variable(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
            (DArgument::Literal(x), DArgument::Literal(y)) => self.literal(x, y),
            (DArgument::Variable(_), DArgument::Literal(_)) => Ordering::Less,
            (DArgument::Literal(_), DArgument::Variable(_)) => Ordering::Greater,
        }
    }

    fn atom_index(a: &Atom<RcStr>) -> i32 {
        match a {
            Atom::ClassAtom { .. } => 6001,
            Atom::DataRangeAtom { .. } => 6002,
            Atom::ObjectPropertyAtom { .. } => 6003,
            Atom::DataPropertyAtom { .. } => 6004,
            Atom::BuiltInAtom { .. } => 6005,
            Atom::SameIndividualAtom(..) => 6009,
            Atom::DifferentIndividualsAtom(..) => 6010,
        }
    }

    pub fn atom(&self, a: &Atom<RcStr>, b: &Atom<RcStr>) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let ti = Self::atom_index(a).cmp(&Self::atom_index(b));
        if ti != Ordering::Equal {
            return ti;
        }
        match (a, b) {
            (Atom::ClassAtom { pred: pa, arg: aa }, Atom::ClassAtom { pred: pb, arg: ab }) => {
                self.ce(pa, pb).then_with(|| self.iarg(aa, ab))
            }
            (Atom::DataRangeAtom { pred: pa, arg: aa }, Atom::DataRangeAtom { pred: pb, arg: ab }) => {
                self.dr(pa, pb).then_with(|| self.darg(aa, ab))
            }
            (Atom::ObjectPropertyAtom { pred: pa, args: aa }, Atom::ObjectPropertyAtom { pred: pb, args: ab }) => self
                .ope(pa, pb)
                .then_with(|| self.iarg(&aa.0, &ab.0))
                .then_with(|| self.iarg(&aa.1, &ab.1)),
            (Atom::DataPropertyAtom { pred: pa, args: aa }, Atom::DataPropertyAtom { pred: pb, args: ab }) => {
                iri_cmp(pa.0.as_ref(), pb.0.as_ref())
                    .then_with(|| self.darg(&aa.0, &ab.0))
                    .then_with(|| self.darg(&aa.1, &ab.1))
            }
            (Atom::BuiltInAtom { pred: pa, args: aa }, Atom::BuiltInAtom { pred: pb, args: ab }) => {
                iri_cmp(pa.as_ref(), pb.as_ref()).then_with(|| self.lists(aa, ab, |p, q| self.darg(p, q)))
            }
            (Atom::SameIndividualAtom(a0, a1), Atom::SameIndividualAtom(b0, b1))
            | (Atom::DifferentIndividualsAtom(a0, a1), Atom::DifferentIndividualsAtom(b0, b1)) => {
                self.iarg(a0, b0).then_with(|| self.iarg(a1, b1))
            }
            _ => Ordering::Equal,
        }
    }

    /// The axiom-type index: declarations 0, then the class axioms, the
    /// assertions, the object- and data-property axioms, keys, rules, and the
    /// annotation axioms, ending with datatype definitions at 38. A
    /// sub-property axiom whose sub-property is a chain is a chain axiom (25).
    /// Components that are not axioms rank after every axiom.
    pub fn axiom_index(c: &Component<RcStr>) -> i32 {
        use Component::*;
        match c {
            DeclareClass(_) | DeclareObjectProperty(_) | DeclareAnnotationProperty(_) | DeclareDataProperty(_)
            | DeclareNamedIndividual(_) | DeclareDatatype(_) => 0,
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
            SubObjectPropertyOf(ax) => {
                if matches!(ax.sub, SOPE::ObjectPropertyChain(_)) {
                    25
                } else {
                    13
                }
            }
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
            OntologyID(_) | DocIRI(_) | Import(_) | OntologyAnnotation(_) => 99,
        }
    }

    /// The index of a declared entity, the key declarations order by.
    fn declared_index(c: &Component<RcStr>) -> Option<(i32, &str)> {
        use Component::*;
        Some(match c {
            DeclareClass(x) => (1001, x.0.as_ref()),
            DeclareObjectProperty(x) => (1002, x.0.as_ref()),
            DeclareDataProperty(x) => (1004, x.0.as_ref()),
            DeclareNamedIndividual(x) => (1005, x.0.as_ref()),
            DeclareAnnotationProperty(x) => (1006, x.0.as_ref()),
            DeclareDatatype(x) => (4001, x.0.as_ref()),
            _ => return None,
        })
    }

    /// Compare two axioms on their fields, not their annotations.
    pub fn component(&self, a: &Component<RcStr>, b: &Component<RcStr>) -> Ordering {
        use Component as C;
        if a == b {
            return Ordering::Equal;
        }
        let ti = Self::axiom_index(a).cmp(&Self::axiom_index(b));
        if ti != Ordering::Equal {
            return ti;
        }
        let iri = |x: &str, y: &str| iri_cmp(x, y);
        match (a, b) {
            _ if Self::declared_index(a).is_some() => {
                let (ia, xa) = Self::declared_index(a).unwrap_or((0, ""));
                let (ib, xb) = Self::declared_index(b).unwrap_or((0, ""));
                ia.cmp(&ib).then_with(|| iri(xa, xb))
            }
            (C::SubClassOf(x), C::SubClassOf(y)) => self.ce(&x.sub, &y.sub).then_with(|| self.ce(&x.sup, &y.sup)),
            (C::EquivalentClasses(x), C::EquivalentClasses(y)) => self.sets(&x.0, &y.0, |p, q| self.ce(p, q)),
            (C::DisjointClasses(x), C::DisjointClasses(y)) => self.sets(&x.0, &y.0, |p, q| self.ce(p, q)),
            (C::DisjointUnion(x), C::DisjointUnion(y)) => {
                iri(x.0 .0.as_ref(), y.0 .0.as_ref()).then_with(|| self.sets(&x.1, &y.1, |p, q| self.ce(p, q)))
            }
            (C::ClassAssertion(x), C::ClassAssertion(y)) => {
                self.individual(&x.i, &y.i).then_with(|| self.ce(&x.ce, &y.ce))
            }
            (C::SameIndividual(x), C::SameIndividual(y)) => self.sets(&x.0, &y.0, |p, q| self.individual(p, q)),
            (C::DifferentIndividuals(x), C::DifferentIndividuals(y)) => {
                self.sets(&x.0, &y.0, |p, q| self.individual(p, q))
            }
            (C::ObjectPropertyAssertion(x), C::ObjectPropertyAssertion(y)) => self
                .individual(&x.from, &y.from)
                .then_with(|| self.ope(&x.ope, &y.ope))
                .then_with(|| self.individual(&x.to, &y.to)),
            (C::NegativeObjectPropertyAssertion(x), C::NegativeObjectPropertyAssertion(y)) => self
                .individual(&x.from, &y.from)
                .then_with(|| self.ope(&x.ope, &y.ope))
                .then_with(|| self.individual(&x.to, &y.to)),
            (C::DataPropertyAssertion(x), C::DataPropertyAssertion(y)) => self
                .individual(&x.from, &y.from)
                .then_with(|| iri(x.dp.0.as_ref(), y.dp.0.as_ref()))
                .then_with(|| self.literal(&x.to, &y.to)),
            (C::NegativeDataPropertyAssertion(x), C::NegativeDataPropertyAssertion(y)) => self
                .individual(&x.from, &y.from)
                .then_with(|| iri(x.dp.0.as_ref(), y.dp.0.as_ref()))
                .then_with(|| self.literal(&x.to, &y.to)),
            (C::EquivalentObjectProperties(x), C::EquivalentObjectProperties(y)) => {
                self.sets(&x.0, &y.0, |p, q| self.ope(p, q))
            }
            (C::DisjointObjectProperties(x), C::DisjointObjectProperties(y)) => {
                self.sets(&x.0, &y.0, |p, q| self.ope(p, q))
            }
            (C::InverseObjectProperties(x), C::InverseObjectProperties(y)) => {
                let xs = [x.0.clone(), x.1.clone()];
                let ys = [y.0.clone(), y.1.clone()];
                self.sets(&xs, &ys, |p, q| self.ope(p, q))
            }
            (C::SubObjectPropertyOf(x), C::SubObjectPropertyOf(y)) => match (&x.sub, &y.sub) {
                (SOPE::ObjectPropertyChain(ca), SOPE::ObjectPropertyChain(cb)) => {
                    self.lists(ca, cb, |p, q| self.ope(p, q)).then_with(|| self.ope(&x.sup, &y.sup))
                }
                (SOPE::ObjectPropertyExpression(sa), SOPE::ObjectPropertyExpression(sb)) => {
                    self.ope(sa, sb).then_with(|| self.ope(&x.sup, &y.sup))
                }
                _ => Ordering::Equal,
            },
            (C::FunctionalObjectProperty(x), C::FunctionalObjectProperty(y)) => self.ope(&x.0, &y.0),
            (C::InverseFunctionalObjectProperty(x), C::InverseFunctionalObjectProperty(y)) => self.ope(&x.0, &y.0),
            (C::SymmetricObjectProperty(x), C::SymmetricObjectProperty(y)) => self.ope(&x.0, &y.0),
            (C::AsymmetricObjectProperty(x), C::AsymmetricObjectProperty(y)) => self.ope(&x.0, &y.0),
            (C::TransitiveObjectProperty(x), C::TransitiveObjectProperty(y)) => self.ope(&x.0, &y.0),
            (C::ReflexiveObjectProperty(x), C::ReflexiveObjectProperty(y)) => self.ope(&x.0, &y.0),
            (C::IrreflexiveObjectProperty(x), C::IrreflexiveObjectProperty(y)) => self.ope(&x.0, &y.0),
            (C::ObjectPropertyDomain(x), C::ObjectPropertyDomain(y)) => {
                self.ope(&x.ope, &y.ope).then_with(|| self.ce(&x.ce, &y.ce))
            }
            (C::ObjectPropertyRange(x), C::ObjectPropertyRange(y)) => {
                self.ope(&x.ope, &y.ope).then_with(|| self.ce(&x.ce, &y.ce))
            }
            (C::EquivalentDataProperties(x), C::EquivalentDataProperties(y)) => {
                self.sets(&x.0, &y.0, |p, q| iri(p.0.as_ref(), q.0.as_ref()))
            }
            (C::DisjointDataProperties(x), C::DisjointDataProperties(y)) => {
                self.sets(&x.0, &y.0, |p, q| iri(p.0.as_ref(), q.0.as_ref()))
            }
            (C::SubDataPropertyOf(x), C::SubDataPropertyOf(y)) => {
                iri(x.sub.0.as_ref(), y.sub.0.as_ref()).then_with(|| iri(x.sup.0.as_ref(), y.sup.0.as_ref()))
            }
            (C::FunctionalDataProperty(x), C::FunctionalDataProperty(y)) => iri(x.0 .0.as_ref(), y.0 .0.as_ref()),
            (C::DataPropertyDomain(x), C::DataPropertyDomain(y)) => {
                iri(x.dp.0.as_ref(), y.dp.0.as_ref()).then_with(|| self.ce(&x.ce, &y.ce))
            }
            (C::DataPropertyRange(x), C::DataPropertyRange(y)) => {
                iri(x.dp.0.as_ref(), y.dp.0.as_ref()).then_with(|| self.dr(&x.dr, &y.dr))
            }
            (C::HasKey(x), C::HasKey(y)) => self
                .ce(&x.ce, &y.ce)
                .then_with(|| self.sets(&x.vpe, &y.vpe, |p, q| self.property_expression(p, q))),
            (C::Rule(x), C::Rule(y)) => self
                .sets(&x.body, &y.body, |p, q| self.atom(p, q))
                .then_with(|| self.sets(&x.head, &y.head, |p, q| self.atom(p, q))),
            (C::AnnotationAssertion(x), C::AnnotationAssertion(y)) => self
                .annotation_subject(&x.subject, &y.subject)
                .then_with(|| iri(x.ann.ap.0.as_ref(), y.ann.ap.0.as_ref()))
                .then_with(|| self.annotation_value(&x.ann.av, &y.ann.av)),
            (C::SubAnnotationPropertyOf(x), C::SubAnnotationPropertyOf(y)) => {
                iri(x.sub.0.as_ref(), y.sub.0.as_ref()).then_with(|| iri(x.sup.0.as_ref(), y.sup.0.as_ref()))
            }
            (C::AnnotationPropertyRange(x), C::AnnotationPropertyRange(y)) => {
                iri(x.ap.0.as_ref(), y.ap.0.as_ref()).then_with(|| iri(x.iri.as_ref(), y.iri.as_ref()))
            }
            (C::AnnotationPropertyDomain(x), C::AnnotationPropertyDomain(y)) => {
                iri(x.ap.0.as_ref(), y.ap.0.as_ref()).then_with(|| iri(x.iri.as_ref(), y.iri.as_ref()))
            }
            (C::DatatypeDefinition(x), C::DatatypeDefinition(y)) => {
                iri(x.kind.0.as_ref(), y.kind.0.as_ref()).then_with(|| self.dr(&x.range, &y.range))
            }
            _ => Ordering::Equal,
        }
    }

    /// Compare two axioms on their fields, then on their annotations.
    pub fn axiom(&self, a: &AnnotatedComponent<RcStr>, b: &AnnotatedComponent<RcStr>) -> Ordering {
        self.component(&a.component, &b.component).then_with(|| {
            let aa = self.sorted_annotations(a.ann.iter());
            let bb = self.sorted_annotations(b.ann.iter());
            for (x, y) in aa.iter().zip(bb.iter()) {
                let c = self.annotation(x, y);
                if c != Ordering::Equal {
                    return c;
                }
            }
            aa.len().cmp(&bb.len())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::Build;

    #[test]
    fn iris_compare_by_namespace_then_local_name() {
        // The NCName split puts `EX_0000002` and `IAO_0000115` in one namespace.
        assert_eq!(
            iri_cmp("http://purl.obolibrary.org/obo/IAO_0000115", "http://purl.obolibrary.org/obo/EX_0000002"),
            Ordering::Greater
        );
        // Namespaces decide before local names: `http://a/b#` < `http://a/b/c#`.
        assert_eq!(iri_cmp("http://a/b#z", "http://a/b/c#a"), Ordering::Less);
        // A suffix that is not an NCName stays in the namespace, so
        // `http://a/0001` is all namespace and sorts after `http://a/` + `b`.
        assert_eq!(iri_cmp("http://a/0001", "http://a/b"), Ordering::Greater);
    }

    #[test]
    fn literals_compare_by_datatype_then_text_then_language() {
        let b = Build::<RcStr>::new();
        let order = NaturalOrder::new(false);
        let plain = Literal::Simple { literal: "b".into() };
        let typed = Literal::Datatype {
            literal: "a".into(),
            datatype_iri: b.iri("http://www.w3.org/2001/XMLSchema#anyURI"),
        };
        // rdf:PlainLiteral's namespace (22-rdf-syntax-ns#) sorts before XMLSchema#.
        assert_eq!(order.literal(&plain, &typed), Ordering::Less);
        let en = Literal::Language { literal: "b".into(), lang: "en".into() };
        assert_eq!(order.literal(&plain, &en), Ordering::Less);
        // Typed as xsd:string, the same plain literal sorts after anyURI.
        assert_eq!(NaturalOrder::new(true).literal(&plain, &typed), Ordering::Greater);
    }

    #[test]
    fn a_named_class_precedes_every_anonymous_expression() {
        let b = Build::<RcStr>::new();
        let order = NaturalOrder::new(false);
        let a = CE::Class(b.class("http://x/Z"));
        let some = CE::ObjectSomeValuesFrom {
            ope: OPE::ObjectProperty(b.object_property("http://x/a")),
            bce: Box::new(CE::Class(b.class("http://x/A"))),
        };
        assert_eq!(order.ce(&a, &some), Ordering::Less);
        let and = CE::ObjectIntersectionOf(vec![some.clone(), a.clone()]);
        let or = CE::ObjectUnionOf(vec![a.clone()]);
        assert_eq!(order.ce(&and, &or), Ordering::Less);
        // Operands compare as a sorted set.
        let and2 = CE::ObjectIntersectionOf(vec![a.clone(), some.clone()]);
        assert_eq!(order.ce(&and, &and2), Ordering::Equal);
    }
}
