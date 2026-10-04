//! The order over axioms, class expressions, individuals and the rest that the
//! writers sort by (`cmp_component`, `cmp_ce`, …), and functional syntax for one
//! component on its own (`render_component_line`) and for the OBO `owl-axioms:`
//! clause (`render_owl_axioms`), both written by the functional writer.
//!
//! The order over axioms is a preorder, not a total one: `cmp_component` reports
//! equal for two distinct axioms whose type it does not rank. An output stable
//! across runs therefore does not follow from the comparison alone — it follows
//! from sorting with a STABLE sort, which leaves tied axioms in the order they
//! were handed over.

use std::cmp::Ordering;

use horned_owl::model::{
    AnnotatedComponent, AnnotationValue, Atom, ClassExpression as CE, Component, DataRange as DR, Individual,
    Literal, ObjectPropertyExpression as OPE, RcStr, SubObjectPropertyExpression as SOPE,
};

use crate::owlapi_hash::iri_cmp;

// ---------------------------------------------------------------------------
// Ordering: type indexes and total comparisons
// ---------------------------------------------------------------------------

/// `typeIndex` of a class expression — the primary key when ordering expressions.
///
/// The numbers are positions in one enumeration of the syntax's forms, not a dense
/// sequence of this renderer's own choosing. A named class sits in the 1000s block
/// the named entities share (Class 1001, ObjectProperty 1002, NamedIndividual 1005,
/// AnnotationProperty 1006); the anonymous expressions occupy the 3000s in
/// enumeration order, so an intersection always precedes a union, a union a
/// complement, and so on: the object restrictions run 3005–3011 and the data
/// restrictions carry on from there, 3012–3017, so every data form sorts after
/// every object one.
fn ce_type_index(ce: &CE<RcStr>) -> i32 {
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

fn ope_iri(ope: &OPE<RcStr>) -> &str {
    match ope {
        OPE::ObjectProperty(p) => p.0.as_ref(),
        OPE::InverseObjectProperty(p) => p.0.as_ref(),
    }
}

pub(crate) fn cmp_ope(a: &OPE<RcStr>, b: &OPE<RcStr>) -> Ordering {
    // ObjectProperty typeIndex 1002, inverse 1003.
    let ta = matches!(a, OPE::InverseObjectProperty(_)) as i32;
    let tb = matches!(b, OPE::InverseObjectProperty(_)) as i32;
    ta.cmp(&tb).then_with(|| iri_cmp(ope_iri(a), ope_iri(b)))
}

pub(crate) fn cmp_individual(a: &Individual<RcStr>, b: &Individual<RcStr>) -> Ordering {
    // A named individual (typeIndex 1005) before an anonymous one (1007); named
    // ones by IRI, anonymous ones by node ID.
    match (a, b) {
        (Individual::Named(x), Individual::Named(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
        (Individual::Anonymous(x), Individual::Anonymous(y)) => x.0.as_ref().cmp(y.0.as_ref()),
        (Individual::Named(_), Individual::Anonymous(_)) => Ordering::Less,
        (Individual::Anonymous(_), Individual::Named(_)) => Ordering::Greater,
    }
}

/// The datatype IRI a literal keys as: an explicit one as given, a
/// language-tagged literal as `rdf:PlainLiteral`, and an untyped one as whichever
/// of the two this document's parse produced (the OBO reader marks its own as
/// `xsd:string`).
///
/// The untyped case is deliberate. A literal with no `rdf:datatype` — and equally
/// one carrying a language tag, including the empty tag — is `rdf:PlainLiteral`,
/// which is the shape the RDF/XML and functional readers build. This datatype is
/// compared BEFORE the lexical form, so it decides which reified `owl:Axiom` node
/// takes which genid: mapping untagged literals to `xsd:string` unconditionally
/// reorders tens of thousands of lines of a document such as
/// `hp-international.owl`.
///
/// It is not unconditional the other way either: after `query --update` the
/// ontology has round-tripped through the RDF store and an untyped literal comes
/// back as `xsd:string`. `Model::plain_literals_typed` records that, and honouring
/// it HERE — not only in the writer — is what puts `hp-fr.owl`'s untagged English
/// definition after its `@fr` translation.
fn lit_datatype(l: &Literal<RcStr>) -> &str {
    match l {
        Literal::Datatype { datatype_iri, .. } => datatype_iri.as_ref(),
        Literal::Language { .. } => "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral",
        Literal::Simple { .. } => crate::io::owlrdf::plain_datatype(),
    }
}

fn lit_lang(l: &Literal<RcStr>) -> &str {
    match l {
        Literal::Language { lang, .. } => lang.as_str(),
        _ => "",
    }
}

pub(crate) fn cmp_annotation_value(a: &AnnotationValue<RcStr>, b: &AnnotationValue<RcStr>) -> Ordering {
    fn rank(v: &AnnotationValue<RcStr>) -> i32 {
        match v {
            AnnotationValue::IRI(_) => 0,
            AnnotationValue::AnonymousIndividual(_) => 1,
            AnnotationValue::Literal(_) => 2,
        }
    }
    match (a, b) {
        (AnnotationValue::IRI(x), AnnotationValue::IRI(y)) => iri_cmp(x.as_ref(), y.as_ref()),
        // Two literals compare on their DATATYPE first, then on the lexical form,
        // then on the language. Two literals that render the same can still order
        // differently: `xsd:string` (what the OBO parser builds) sorts after
        // `rdf:PlainLiteral` (what the functional and RDF/XML parsers build), which
        // is what puts a pattern-derived synonym before an edit-file one on the
        // same OBA class.
        (AnnotationValue::Literal(x), AnnotationValue::Literal(y)) => cmp_literal(x, y),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Two literals compare on their DATATYPE first, then on the lexical form, then
/// on the language.
pub(crate) fn cmp_literal(x: &Literal<RcStr>, y: &Literal<RcStr>) -> Ordering {
    iri_cmp(lit_datatype(x), lit_datatype(y))
        .then_with(|| x.literal().cmp(y.literal()))
        .then_with(|| lit_lang(x).cmp(lit_lang(y)))
}

/// Orders class expressions by `typeIndex`, then by their components: a
/// restriction by its property and then its filler or value, an n-ary boolean
/// and a one-of by their members as a set.
pub(crate) fn cmp_ce(a: &CE<RcStr>, b: &CE<RcStr>) -> Ordering {
    let ti = ce_type_index(a).cmp(&ce_type_index(b));
    if ti != Ordering::Equal {
        return ti;
    }
    match (a, b) {
        (CE::Class(x), CE::Class(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
        (CE::ObjectIntersectionOf(x), CE::ObjectIntersectionOf(y))
        | (CE::ObjectUnionOf(x), CE::ObjectUnionOf(y)) => cmp_ce_list(x, y),
        (CE::ObjectComplementOf(x), CE::ObjectComplementOf(y)) => cmp_ce(x, y),
        (CE::ObjectHasSelf(x), CE::ObjectHasSelf(y)) => cmp_ope(x, y),
        (CE::ObjectOneOf(x), CE::ObjectOneOf(y)) => {
            let mut x: Vec<&Individual<RcStr>> = x.iter().collect();
            let mut y: Vec<&Individual<RcStr>> = y.iter().collect();
            x.sort_by(|p, q| cmp_individual(p, q));
            y.sort_by(|p, q| cmp_individual(p, q));
            cmp_sorted(&x, &y, |p, q| cmp_individual(p, q))
        }
        (CE::ObjectHasValue { ope: pa, i: ia }, CE::ObjectHasValue { ope: pb, i: ib }) => {
            cmp_ope(pa, pb).then_with(|| cmp_individual(ia, ib))
        }
        (
            CE::ObjectSomeValuesFrom { ope: pa, bce: fa },
            CE::ObjectSomeValuesFrom { ope: pb, bce: fb },
        )
        | (
            CE::ObjectAllValuesFrom { ope: pa, bce: fa },
            CE::ObjectAllValuesFrom { ope: pb, bce: fb },
        ) => cmp_ope(pa, pb).then_with(|| cmp_ce(fa, fb)),
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
        ) => cmp_ope(pa, pb).then_with(|| na.cmp(nb)).then_with(|| cmp_ce(fa, fb)),
        (CE::DataSomeValuesFrom { dp: pa, dr: ra }, CE::DataSomeValuesFrom { dp: pb, dr: rb })
        | (CE::DataAllValuesFrom { dp: pa, dr: ra }, CE::DataAllValuesFrom { dp: pb, dr: rb }) => {
            iri_cmp(pa.0.as_ref(), pb.0.as_ref()).then_with(|| cmp_dr(ra, rb))
        }
        (CE::DataHasValue { dp: pa, l: la }, CE::DataHasValue { dp: pb, l: lb }) => {
            iri_cmp(pa.0.as_ref(), pb.0.as_ref()).then_with(|| cmp_literal(la, lb))
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
        ) => iri_cmp(pa.0.as_ref(), pb.0.as_ref())
            .then_with(|| na.cmp(nb))
            .then_with(|| cmp_dr(ra, rb)),
        _ => Ordering::Equal,
    }
}

/// Orders data ranges the way class expressions are ordered: by the form's index,
/// then by components. The indices run datatype 4001, complement 4002, one-of
/// 4003, intersection 4004 and restriction 4006, but a union's is 2005, so a
/// union precedes every other data range. The operands of a union or
/// intersection, the values of a one-of and the restrictions of a datatype
/// restriction are sets, compared in their own sorted order.
pub(crate) fn cmp_dr(a: &DR<RcStr>, b: &DR<RcStr>) -> Ordering {
    fn idx(dr: &DR<RcStr>) -> i32 {
        match dr {
            DR::DataUnionOf(_) => 2005,
            DR::Datatype(_) => 4001,
            DR::DataComplementOf(_) => 4002,
            DR::DataOneOf(_) => 4003,
            DR::DataIntersectionOf(_) => 4004,
            DR::DatatypeRestriction(_, _) => 4006,
        }
    }
    let ti = idx(a).cmp(&idx(b));
    if ti != Ordering::Equal {
        return ti;
    }
    match (a, b) {
        (DR::Datatype(x), DR::Datatype(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
        (DR::DataComplementOf(x), DR::DataComplementOf(y)) => cmp_dr(x, y),
        (DR::DataIntersectionOf(x), DR::DataIntersectionOf(y))
        | (DR::DataUnionOf(x), DR::DataUnionOf(y)) => {
            let mut x: Vec<&DR<RcStr>> = x.iter().collect();
            let mut y: Vec<&DR<RcStr>> = y.iter().collect();
            x.sort_by(|p, q| cmp_dr(p, q));
            y.sort_by(|p, q| cmp_dr(p, q));
            cmp_sorted(&x, &y, |p, q| cmp_dr(p, q))
        }
        (DR::DataOneOf(x), DR::DataOneOf(y)) => {
            let mut x: Vec<_> = x.iter().map(crate::io::owlrdf::literal_key).collect();
            let mut y: Vec<_> = y.iter().map(crate::io::owlrdf::literal_key).collect();
            x.sort();
            y.sort();
            cmp_sorted(&x, &y, |p, q| p.cmp(q))
        }
        (DR::DatatypeRestriction(dx, fx), DR::DatatypeRestriction(dy, fy)) => {
            iri_cmp(dx.0.as_ref(), dy.0.as_ref()).then_with(|| {
                let key = |f: &horned_owl::model::FacetRestriction<RcStr>| {
                    (crate::io::owlrdf::facet_rank(f.f.as_ref()), crate::io::owlrdf::literal_key(&f.l))
                };
                let mut x: Vec<_> = fx.iter().map(key).collect();
                let mut y: Vec<_> = fy.iter().map(key).collect();
                x.sort();
                y.sort();
                cmp_sorted(&x, &y, |p, q| p.cmp(q))
            })
        }
        _ => Ordering::Equal,
    }
}

/// Two sorted sets, element by element and then by size.
fn cmp_sorted<T>(a: &[T], b: &[T], cmp: impl Fn(&T, &T) -> Ordering) -> Ordering {
    for (p, q) in a.iter().zip(b.iter()) {
        let c = cmp(p, q);
        if c != Ordering::Equal {
            return c;
        }
    }
    a.len().cmp(&b.len())
}

/// Compare two operand lists element-by-element (they are already stored sorted).
fn cmp_ce_list(a: &[CE<RcStr>], b: &[CE<RcStr>]) -> Ordering {
    let mut av: Vec<&CE<RcStr>> = a.iter().collect();
    let mut bv: Vec<&CE<RcStr>> = b.iter().collect();
    av.sort_by(|x, y| cmp_ce(x, y));
    bv.sort_by(|x, y| cmp_ce(x, y));
    for (x, y) in av.iter().zip(bv.iter()) {
        let c = cmp_ce(x, y);
        if c != Ordering::Equal {
            return c;
        }
    }
    av.len().cmp(&bv.len())
}

/// The rank of an axiom's type — the primary key when ordering axioms.
///
/// Like `ce_type_index`, the numbers are positions in one enumeration of the axiom
/// types rather than a dense sequence, and they are what groups a block's axioms by
/// kind: declarations lead at 0, then the class axioms (1–4), the individual and
/// assertion axioms (7–9), the property axioms (13–23), rules (33), and last the
/// annotation axioms (34–37). The gaps are the positions of types an OBO document's
/// untranslatable set never holds, and they stay reserved so that every rank here
/// keeps its enumeration position: a type added later takes the number its position
/// gives it, never the next free integer, or it groups in the wrong place relative
/// to the types already ranked. Anything still unranked takes 99 and sorts after
/// everything named here.
fn axiom_type_index(c: &Component<RcStr>) -> i32 {
    match c {
        Component::DeclareClass(_)
        | Component::DeclareObjectProperty(_)
        | Component::DeclareAnnotationProperty(_)
        | Component::DeclareDataProperty(_)
        | Component::DeclareNamedIndividual(_)
        | Component::DeclareDatatype(_) => 0,
        Component::EquivalentClasses(_) => 1,
        Component::SubClassOf(_) => 2,
        Component::DisjointClasses(_) => 3,
        Component::DisjointUnion(_) => 4,
        Component::ClassAssertion(_) => 5,
        Component::SameIndividual(_) => 6,
        Component::DifferentIndividuals(_) => 7,
        Component::ObjectPropertyAssertion(_) => 8,
        Component::NegativeObjectPropertyAssertion(_) => 9,
        Component::DataPropertyAssertion(_) => 10,
        Component::NegativeDataPropertyAssertion(_) => 11,
        Component::SubObjectPropertyOf(_) => 13,
        Component::IrreflexiveObjectProperty(_) => 21,
        Component::ObjectPropertyRange(_) => 23,
        Component::Rule(_) => 33,
        Component::AnnotationAssertion(_) => 34,
        Component::SubAnnotationPropertyOf(_) => 35,
        // Annotation-property range and domain continue the annotation block.
        Component::AnnotationPropertyRange(_) => 36,
        Component::AnnotationPropertyDomain(_) => 37,
        _ => 99,
    }
}

/// Orders axioms by the axiom-type rank, then by the axiom's own fields.
///
/// Types with no comparison arm of their own — the declarations, and everything
/// the 99 catch-all collects — compare equal to each other, so this is a
/// preorder, not a total order. Ties keep the order they arrived in, which only a
/// stable sort preserves.
pub(crate) fn cmp_component(a: &Component<RcStr>, b: &Component<RcStr>) -> Ordering {
    let ti = axiom_type_index(a).cmp(&axiom_type_index(b));
    if ti != Ordering::Equal {
        return ti;
    }
    match (a, b) {
        (Component::SubClassOf(x), Component::SubClassOf(y)) => {
            cmp_ce(&x.sub, &y.sub).then_with(|| cmp_ce(&x.sup, &y.sup))
        }
        (Component::EquivalentClasses(x), Component::EquivalentClasses(y)) => cmp_ce_list(&x.0, &y.0),
        (Component::DisjointClasses(x), Component::DisjointClasses(y)) => cmp_ce_list(&x.0, &y.0),
        (Component::DisjointUnion(x), Component::DisjointUnion(y)) => {
            iri_cmp(x.0 .0.as_ref(), y.0 .0.as_ref()).then_with(|| cmp_ce_list(&x.1, &y.1))
        }
        (Component::ObjectPropertyRange(x), Component::ObjectPropertyRange(y)) => {
            cmp_ope(&x.ope, &y.ope).then_with(|| cmp_ce(&x.ce, &y.ce))
        }
        (Component::SubObjectPropertyOf(x), Component::SubObjectPropertyOf(y)) => {
            cmp_sope(&x.sub, &y.sub).then_with(|| cmp_ope(&x.sup, &y.sup))
        }
        (Component::ObjectPropertyAssertion(x), Component::ObjectPropertyAssertion(y)) => {
            cmp_ope(&x.ope, &y.ope)
                .then_with(|| cmp_individual(&x.from, &y.from))
                .then_with(|| cmp_individual(&x.to, &y.to))
        }
        (
            Component::NegativeObjectPropertyAssertion(x),
            Component::NegativeObjectPropertyAssertion(y),
        ) => cmp_ope(&x.ope, &y.ope)
            .then_with(|| cmp_individual(&x.from, &y.from))
            .then_with(|| cmp_individual(&x.to, &y.to)),
        (Component::DataPropertyAssertion(x), Component::DataPropertyAssertion(y)) => iri_cmp(x.dp.0.as_ref(), y.dp.0.as_ref())
            .then_with(|| cmp_individual(&x.from, &y.from))
            .then_with(|| cmp_literal(&x.to, &y.to)),
        (
            Component::NegativeDataPropertyAssertion(x),
            Component::NegativeDataPropertyAssertion(y),
        ) => iri_cmp(x.dp.0.as_ref(), y.dp.0.as_ref())
            .then_with(|| cmp_individual(&x.from, &y.from))
            .then_with(|| cmp_literal(&x.to, &y.to)),
        (Component::ClassAssertion(x), Component::ClassAssertion(y)) => {
            cmp_individual(&x.i, &y.i).then_with(|| cmp_ce(&x.ce, &y.ce))
        }
        (Component::SameIndividual(x), Component::SameIndividual(y)) => {
            let n = x.0.len().min(y.0.len());
            for k in 0..n {
                let o = cmp_individual(&x.0[k], &y.0[k]);
                if o != Ordering::Equal {
                    return o;
                }
            }
            x.0.len().cmp(&y.0.len())
        }
        (Component::IrreflexiveObjectProperty(x), Component::IrreflexiveObjectProperty(y)) => {
            cmp_ope(&x.0, &y.0)
        }
        (Component::DifferentIndividuals(x), Component::DifferentIndividuals(y)) => {
            let n = x.0.len().min(y.0.len());
            for k in 0..n {
                let o = cmp_individual(&x.0[k], &y.0[k]);
                if o != Ordering::Equal {
                    return o;
                }
            }
            x.0.len().cmp(&y.0.len())
        }
        (Component::SubAnnotationPropertyOf(x), Component::SubAnnotationPropertyOf(y)) => {
            iri_cmp(x.sub.0.as_ref(), y.sub.0.as_ref()).then_with(|| iri_cmp(x.sup.0.as_ref(), y.sup.0.as_ref()))
        }
        (Component::AnnotationPropertyRange(x), Component::AnnotationPropertyRange(y)) => {
            iri_cmp(x.ap.0.as_ref(), y.ap.0.as_ref()).then_with(|| iri_cmp(x.iri.as_ref(), y.iri.as_ref()))
        }
        (Component::AnnotationPropertyDomain(x), Component::AnnotationPropertyDomain(y)) => {
            iri_cmp(x.ap.0.as_ref(), y.ap.0.as_ref()).then_with(|| iri_cmp(x.iri.as_ref(), y.iri.as_ref()))
        }
        (Component::AnnotationAssertion(x), Component::AnnotationAssertion(y)) => {
            cmp_annotation_subject(&x.subject, &y.subject)
                .then_with(|| iri_cmp(x.ann.ap.0.as_ref(), y.ann.ap.0.as_ref()))
                .then_with(|| cmp_annotation_value(&x.ann.av, &y.ann.av))
        }
        (Component::Rule(x), Component::Rule(y)) => {
            atom_key(&x.body).cmp(&atom_key(&y.body)).then_with(|| atom_key(&x.head).cmp(&atom_key(&y.head)))
        }
        (Component::ObjectPropertyDomain(x), Component::ObjectPropertyDomain(y)) => {
            cmp_ope(&x.ope, &y.ope).then_with(|| cmp_ce(&x.ce, &y.ce))
        }
        (Component::DataPropertyDomain(x), Component::DataPropertyDomain(y)) => {
            iri_cmp(x.dp.0.as_ref(), y.dp.0.as_ref()).then_with(|| cmp_ce(&x.ce, &y.ce))
        }
        (Component::DataPropertyRange(x), Component::DataPropertyRange(y)) => {
            iri_cmp(x.dp.0.as_ref(), y.dp.0.as_ref()).then_with(|| cmp_dr(&x.dr, &y.dr))
        }
        (Component::DatatypeDefinition(x), Component::DatatypeDefinition(y)) => {
            iri_cmp(x.kind.0.as_ref(), y.kind.0.as_ref()).then_with(|| cmp_dr(&x.range, &y.range))
        }
        (Component::SubDataPropertyOf(x), Component::SubDataPropertyOf(y)) => {
            iri_cmp(x.sub.0.as_ref(), y.sub.0.as_ref()).then_with(|| iri_cmp(x.sup.0.as_ref(), y.sup.0.as_ref()))
        }
        (Component::EquivalentObjectProperties(x), Component::EquivalentObjectProperties(y)) => {
            cmp_ope_set(&x.0, &y.0)
        }
        (Component::DisjointObjectProperties(x), Component::DisjointObjectProperties(y)) => {
            cmp_ope_set(&x.0, &y.0)
        }
        (Component::InverseObjectProperties(x), Component::InverseObjectProperties(y)) => {
            cmp_ope_set(&[x.0.clone(), x.1.clone()], &[y.0.clone(), y.1.clone()])
        }
        (Component::EquivalentDataProperties(x), Component::EquivalentDataProperties(y)) => cmp_dp_set(&x.0, &y.0),
        (Component::DisjointDataProperties(x), Component::DisjointDataProperties(y)) => cmp_dp_set(&x.0, &y.0),
        (Component::FunctionalObjectProperty(x), Component::FunctionalObjectProperty(y)) => cmp_ope(&x.0, &y.0),
        (Component::InverseFunctionalObjectProperty(x), Component::InverseFunctionalObjectProperty(y)) => {
            cmp_ope(&x.0, &y.0)
        }
        (Component::SymmetricObjectProperty(x), Component::SymmetricObjectProperty(y)) => cmp_ope(&x.0, &y.0),
        (Component::AsymmetricObjectProperty(x), Component::AsymmetricObjectProperty(y)) => cmp_ope(&x.0, &y.0),
        (Component::TransitiveObjectProperty(x), Component::TransitiveObjectProperty(y)) => cmp_ope(&x.0, &y.0),
        (Component::ReflexiveObjectProperty(x), Component::ReflexiveObjectProperty(y)) => cmp_ope(&x.0, &y.0),
        (Component::FunctionalDataProperty(x), Component::FunctionalDataProperty(y)) => {
            iri_cmp(x.0 .0.as_ref(), y.0 .0.as_ref())
        }
        _ => Ordering::Equal,
    }
}

/// An IRI subject (typeIndex 0) before an anonymous one (1007); IRIs in IRI
/// order, anonymous subjects by node ID.
fn cmp_annotation_subject(
    a: &horned_owl::model::AnnotationSubject<RcStr>,
    b: &horned_owl::model::AnnotationSubject<RcStr>,
) -> Ordering {
    use horned_owl::model::AnnotationSubject as S;
    match (a, b) {
        (S::IRI(x), S::IRI(y)) => iri_cmp(x.as_ref(), y.as_ref()),
        (S::AnonymousIndividual(x), S::AnonymousIndividual(y)) => x.0.as_ref().cmp(y.0.as_ref()),
        (S::IRI(_), S::AnonymousIndividual(_)) => Ordering::Less,
        (S::AnonymousIndividual(_), S::IRI(_)) => Ordering::Greater,
    }
}

/// Two sets of data properties, each in its own order.
fn cmp_dp_set(a: &[horned_owl::model::DataProperty<RcStr>], b: &[horned_owl::model::DataProperty<RcStr>]) -> Ordering {
    let mut a: Vec<&str> = a.iter().map(|p| p.0.as_ref()).collect();
    let mut b: Vec<&str> = b.iter().map(|p| p.0.as_ref()).collect();
    a.sort_by(|p, q| iri_cmp(p, q));
    b.sort_by(|p, q| iri_cmp(p, q));
    cmp_sorted(&a, &b, |p, q| iri_cmp(p, q))
}

/// Two sets of object property expressions, each in its own order.
fn cmp_ope_set(a: &[OPE<RcStr>], b: &[OPE<RcStr>]) -> Ordering {
    let mut a: Vec<&OPE<RcStr>> = a.iter().collect();
    let mut b: Vec<&OPE<RcStr>> = b.iter().collect();
    a.sort_by(|p, q| cmp_ope(p, q));
    b.sort_by(|p, q| cmp_ope(p, q));
    cmp_sorted(&a, &b, |p, q| cmp_ope(p, q))
}

/// A simple ordering key for a SWRL atom list: the sequence of predicate IRIs.
fn atom_key(atoms: &[Atom<RcStr>]) -> Vec<String> {
    atoms
        .iter()
        .map(|a| match a {
            Atom::ObjectPropertyAtom { pred, .. } => ope_iri(pred).to_string(),
            Atom::ClassAtom { pred: CE::Class(c), .. } => c.0.as_ref().to_string(),
            _ => String::new(),
        })
        .collect()
}

fn cmp_sope(a: &SOPE<RcStr>, b: &SOPE<RcStr>) -> Ordering {
    match (a, b) {
        (SOPE::ObjectPropertyExpression(x), SOPE::ObjectPropertyExpression(y)) => cmp_ope(x, y),
        (SOPE::ObjectPropertyChain(_), SOPE::ObjectPropertyExpression(_)) => Ordering::Greater,
        (SOPE::ObjectPropertyExpression(_), SOPE::ObjectPropertyChain(_)) => Ordering::Less,
        (SOPE::ObjectPropertyChain(x), SOPE::ObjectPropertyChain(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                let c = cmp_ope(p, q);
                if c != Ordering::Equal {
                    return c;
                }
            }
            x.len().cmp(&y.len())
        }
    }
}

/// One component in functional syntax, on one line: its set-valued operands
/// in canonical order, and every IRI in full but those of the five built-in
/// namespaces, which are CURIEs.
pub(crate) fn render_component_line(ac: &AnnotatedComponent<RcStr>) -> String {
    use horned_owl::io::ofn::writer::AsFunctional;
    let component = crate::io::canonical_component(&ac.component).unwrap_or_else(|| ac.component.clone());
    let ac = AnnotatedComponent { component, ann: ac.ann.clone() };
    ac.as_functional_with_prefixes(&crate::io::ofn_prefix_block(&Default::default(), None)).to_string()
}

/// The functional-syntax document the `owl-axioms:` clause carries: the
/// untranslatable axioms as an ontology of their own, with no IRI, each entity
/// they name declared, and the five built-in prefixes, as the functional
/// writer writes any document. Returns `None` if there are none.
pub fn render_owl_axioms(
    untranslatable: &[&AnnotatedComponent<RcStr>],
    plain_literals_typed: bool,
) -> anyhow::Result<Option<String>> {
    use horned_owl::model::MutableOntology;
    if untranslatable.is_empty() {
        return Ok(None);
    }
    let mut ont = crate::model::Onto::new();
    for ac in untranslatable {
        let component = crate::io::canonical_component(&ac.component).unwrap_or_else(|| ac.component.clone());
        ont.insert(AnnotatedComponent { component, ann: ac.ann.clone() });
    }
    let cm: crate::model::CmOnto = ont.into();
    horned_owl::io::ofn::writer::set_plain_literals_typed(plain_literals_typed);
    let prefixes = crate::io::ofn_prefix_block(&Default::default(), None);
    let out = horned_owl::io::ofn::writer::write_with_labels(Vec::new(), &cm, Some(&prefixes), None, None)
        .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"))?;
    Ok(Some(String::from_utf8(out)?))
}

#[cfg(test)]
mod tests {
    use super::render_component_line;
    use horned_owl::model::Kinded;

    /// Every kind of axiom renders as itself on one line, its annotations
    /// included: a kind written as nothing would vanish from a diff report and
    /// leave a warning naming nothing.
    #[test]
    fn every_axiom_kind_renders_on_one_line() {
        let ofn = r#"Prefix(:=<http://example.org/t#>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)
Ontology(<http://example.org/t>
Import(<http://example.org/other>)
Annotation(rdfs:comment "ontology")
Declaration(Class(:A))
Declaration(Class(:B))
Declaration(ObjectProperty(:p))
Declaration(ObjectProperty(:q))
Declaration(DataProperty(:d))
Declaration(DataProperty(:e))
Declaration(AnnotationProperty(:ap))
Declaration(AnnotationProperty(:aq))
Declaration(NamedIndividual(:i))
Declaration(NamedIndividual(:j))
Declaration(Datatype(:t))
SubClassOf(Annotation(rdfs:comment "c") :A ObjectIntersectionOf(:B DataSomeValuesFrom(:d xsd:string)))
EquivalentClasses(:A :B)
DisjointClasses(:A :B)
DisjointUnion(:A :B ObjectComplementOf(:B))
SubObjectPropertyOf(ObjectPropertyChain(:p :q) :p)
EquivalentObjectProperties(:p :q)
DisjointObjectProperties(:p ObjectInverseOf(:q))
InverseObjectProperties(:p :q)
ObjectPropertyDomain(:p :A)
ObjectPropertyRange(:p :A)
FunctionalObjectProperty(:p)
InverseFunctionalObjectProperty(:p)
ReflexiveObjectProperty(:p)
IrreflexiveObjectProperty(:p)
SymmetricObjectProperty(:p)
AsymmetricObjectProperty(:p)
TransitiveObjectProperty(:p)
SubDataPropertyOf(:d :e)
EquivalentDataProperties(:d :e)
DisjointDataProperties(:d :e)
DataPropertyDomain(:d :A)
DataPropertyRange(:d DataUnionOf(xsd:string xsd:integer))
FunctionalDataProperty(:d)
DatatypeDefinition(:t DatatypeRestriction(xsd:integer xsd:minInclusive "1"^^xsd:integer))
HasKey(:A (:p) (:d))
SameIndividual(:i :j)
DifferentIndividuals(:i :j)
ClassAssertion(:A :i)
ObjectPropertyAssertion(:p :i :j)
NegativeObjectPropertyAssertion(:p :i :j)
DataPropertyAssertion(:d :i "x")
NegativeDataPropertyAssertion(:d :i "y")
AnnotationAssertion(Annotation(rdfs:comment "nested") :ap :A "z")
SubAnnotationPropertyOf(:ap :aq)
AnnotationPropertyDomain(:ap :A)
AnnotationPropertyRange(:ap :B)
DLSafeRule(Body(ClassAtom(:A Variable(:v)) DataPropertyAtom(:d Variable(:v) Variable(:w))) Head(ClassAtom(:B Variable(:v))))
)"#;
        let model = crate::io::load_from(std::io::Cursor::new(ofn), crate::io::Format::Functional).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for ac in model.ont.iter() {
            let kind = format!("{:?}", ac.component.kind());
            let line = render_component_line(ac);
            assert!(!line.is_empty() && !line.contains('\n'), "{kind}: {line:?}");
            seen.insert(kind);
        }
        assert!(seen.len() >= 45, "{} kinds: {seen:?}", seen.len());
    }
}
