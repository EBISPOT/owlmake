//! The OBO `owl-axioms:` header value: the axioms OBO tags cannot express,
//! serialised in OWL functional syntax so an OBO write loses nothing.
//!
//! The rendering covers the fragment OBO ontologies exercise: entity-grouped
//! output with `####` section banners and `# Type: <IRI> (<label>)` per-entity
//! comments, declarations grouped by `typeIndex` then IRI, axioms in the order
//! `cmp_component` defines, then leftover axioms, all wrapped in `Ontology( … )`
//! under five fixed `Prefix(…)` lines.
//!
//! That order is a preorder, not a total one: `cmp_component` reports equal for two
//! distinct axioms whose type it does not rank, and `cmp_ce` does the same for two
//! distinct expressions of a form it does not compare. A header stable across runs
//! therefore does not follow from the comparison alone — it follows from sorting
//! with a STABLE sort, which leaves tied axioms in the order they were handed over,
//! and from dropping axioms whose rendered text repeats. Both are load-bearing: an
//! unstable sort here would reshuffle emitted headers for no change in content.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};

use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, Atom, ClassExpression as CE,
    Component, DataRange as DR, IArgument, Individual, Literal, ObjectPropertyExpression as OPE,
    RcStr, SubObjectPropertyExpression as SOPE,
};

const OWL_NS: &str = "http://www.w3.org/2002/07/owl#";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_PLAIN: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
const RDF_LANGSTRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";

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
    ta.cmp(&tb).then_with(|| ope_iri(a).cmp(ope_iri(b)))
}

pub(crate) fn cmp_individual(a: &Individual<RcStr>, b: &Individual<RcStr>) -> Ordering {
    // Anonymous typeIndex differs from named; both compare by string form.
    let ta = matches!(a, Individual::Anonymous(_)) as i32;
    let tb = matches!(b, Individual::Anonymous(_)) as i32;
    ta.cmp(&tb).then_with(|| (a as &str).cmp(b as &str))
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
        (AnnotationValue::IRI(x), AnnotationValue::IRI(y)) => x.as_ref().cmp(y.as_ref()),
        // Two literals compare on their DATATYPE first, then on the lexical form,
        // then on the language. Two literals that render the same can still order
        // differently: `xsd:string` (what the OBO parser builds) sorts after
        // `rdf:PlainLiteral` (what the functional and RDF/XML parsers build), which
        // is what puts a pattern-derived synonym before an edit-file one on the
        // same OBA class.
        (AnnotationValue::Literal(x), AnnotationValue::Literal(y)) => lit_datatype(x)
            .cmp(&lit_datatype(y))
            .then_with(|| x.literal().cmp(y.literal()))
            .then_with(|| lit_lang(x).cmp(&lit_lang(y))),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Orders class expressions by `typeIndex`, then by their components.
///
/// Forms with no comparison arm of their own — `ObjectOneOf`, `ObjectHasValue`, and
/// everything the 3999 catch-all collects — compare equal to each other, so this is
/// a preorder, not a total order. Ties keep the order they arrived in, which only a
/// stable sort preserves.
pub(crate) fn cmp_ce(a: &CE<RcStr>, b: &CE<RcStr>) -> Ordering {
    let ti = ce_type_index(a).cmp(&ce_type_index(b));
    if ti != Ordering::Equal {
        return ti;
    }
    match (a, b) {
        (CE::Class(x), CE::Class(y)) => x.0.as_ref().cmp(y.0.as_ref()),
        (CE::ObjectIntersectionOf(x), CE::ObjectIntersectionOf(y))
        | (CE::ObjectUnionOf(x), CE::ObjectUnionOf(y)) => cmp_ce_list(x, y),
        (CE::ObjectComplementOf(x), CE::ObjectComplementOf(y)) => cmp_ce(x, y),
        (CE::ObjectHasSelf(x), CE::ObjectHasSelf(y)) => cmp_ope(x, y),
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
            pa.0.as_ref().cmp(pb.0.as_ref()).then_with(|| cmp_dr(ra, rb))
        }
        (CE::DataHasValue { dp: pa, l: la }, CE::DataHasValue { dp: pb, l: lb }) => pa
            .0
            .as_ref()
            .cmp(pb.0.as_ref())
            .then_with(|| lit_datatype(la).cmp(lit_datatype(lb)))
            .then_with(|| la.literal().cmp(lb.literal())),
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
        ) => pa
            .0
            .as_ref()
            .cmp(pb.0.as_ref())
            .then_with(|| na.cmp(nb))
            .then_with(|| cmp_dr(ra, rb)),
        _ => Ordering::Equal,
    }
}

/// Orders data ranges the way class expressions are ordered: by the form's index,
/// then by components. A named datatype is an ENTITY, so it takes the entity block's
/// 1004 and precedes every anonymous form; the anonymous forms follow in enumeration
/// order. Forms with no arm of their own compare equal, so this is a preorder.
pub(crate) fn cmp_dr(a: &DR<RcStr>, b: &DR<RcStr>) -> Ordering {
    fn idx(dr: &DR<RcStr>) -> i32 {
        match dr {
            DR::Datatype(_) => 1004,
            DR::DataComplementOf(_) => 4002,
            DR::DataOneOf(_) => 4003,
            DR::DataIntersectionOf(_) => 4004,
            DR::DataUnionOf(_) => 4005,
            DR::DatatypeRestriction(_, _) => 4006,
        }
    }
    let ti = idx(a).cmp(&idx(b));
    if ti != Ordering::Equal {
        return ti;
    }
    match (a, b) {
        (DR::Datatype(x), DR::Datatype(y)) => x.0.as_ref().cmp(y.0.as_ref()),
        (DR::DataComplementOf(x), DR::DataComplementOf(y)) => cmp_dr(x, y),
        (DR::DataIntersectionOf(x), DR::DataIntersectionOf(y))
        | (DR::DataUnionOf(x), DR::DataUnionOf(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                let c = cmp_dr(p, q);
                if c != Ordering::Equal {
                    return c;
                }
            }
            x.len().cmp(&y.len())
        }
        _ => Ordering::Equal,
    }
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
        Component::DifferentIndividuals(_) => 7,
        Component::ObjectPropertyAssertion(_) => 8,
        Component::NegativeObjectPropertyAssertion(_) => 9,
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
/// Types with no comparison arm of their own — the declarations, the negative
/// assertions, and everything the 99 catch-all collects — compare equal to each
/// other, so this is a preorder, not a total order. Ties keep the order they arrived
/// in, which only a stable sort preserves.
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
            x.0 .0.as_ref().cmp(y.0 .0.as_ref()).then_with(|| cmp_ce_list(&x.1, &y.1))
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
            x.sub.0.as_ref().cmp(y.sub.0.as_ref()).then_with(|| x.sup.0.as_ref().cmp(y.sup.0.as_ref()))
        }
        (Component::AnnotationPropertyRange(x), Component::AnnotationPropertyRange(y)) => {
            x.ap.0.as_ref().cmp(y.ap.0.as_ref()).then_with(|| x.iri.as_ref().cmp(y.iri.as_ref()))
        }
        (Component::AnnotationPropertyDomain(x), Component::AnnotationPropertyDomain(y)) => {
            x.ap.0.as_ref().cmp(y.ap.0.as_ref()).then_with(|| x.iri.as_ref().cmp(y.iri.as_ref()))
        }
        (Component::AnnotationAssertion(x), Component::AnnotationAssertion(y)) => {
            (x.subject.as_ref() as &str)
                .cmp(y.subject.as_ref())
                .then_with(|| x.ann.ap.0.as_ref().cmp(y.ann.ap.0.as_ref()))
                .then_with(|| cmp_annotation_value(&x.ann.av, &y.ann.av))
        }
        (Component::Rule(x), Component::Rule(y)) => {
            atom_key(&x.body).cmp(&atom_key(&y.body)).then_with(|| atom_key(&x.head).cmp(&atom_key(&y.head)))
        }
        _ => Ordering::Equal,
    }
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

// ---------------------------------------------------------------------------
// Entity kinds and grouping
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum EKind {
    Class,
    ObjectProperty,
    DataProperty,
    Datatype,
    NamedIndividual,
    AnnotationProperty,
}

/// The entity (IRI, kind) under whose section an axiom is rendered, or `None`
/// for the leftover section: an axiom belongs to the entity it is stated about.
fn owner_entity(c: &Component<RcStr>) -> Option<(String, EKind)> {
    match c {
        Component::SubClassOf(ax) => match &ax.sub {
            CE::Class(s) => Some((s.0.as_ref().to_string(), EKind::Class)),
            _ => None, // GCI (anonymous subclass) → general axiom → leftover
        },
        Component::EquivalentClasses(ax) => ax
            .0
            .iter()
            .filter_map(named_class)
            .min()
            .map(|i| (i, EKind::Class)),
        Component::DisjointClasses(ax) => {
            if ax.0.len() > 2 {
                None // rendered in leftover
            } else {
                ax.0.iter().filter_map(named_class).min().map(|i| (i, EKind::Class))
            }
        }
        Component::DisjointUnion(ax) => Some((ax.0 .0.as_ref().to_string(), EKind::Class)),
        Component::ObjectPropertyRange(ax) => match &ax.ope {
            OPE::ObjectProperty(p) => Some((p.0.as_ref().to_string(), EKind::ObjectProperty)),
            _ => None,
        },
        Component::SubObjectPropertyOf(ax) => match &ax.sub {
            SOPE::ObjectPropertyExpression(OPE::ObjectProperty(p)) => {
                Some((p.0.as_ref().to_string(), EKind::ObjectProperty))
            }
            _ => None,
        },
        Component::IrreflexiveObjectProperty(ax) => match &ax.0 {
            OPE::ObjectProperty(p) => Some((p.0.as_ref().to_string(), EKind::ObjectProperty)),
            _ => None,
        },
        Component::ObjectPropertyAssertion(ax) => match &ax.from {
            Individual::Named(i) => Some((i.0.as_ref().to_string(), EKind::NamedIndividual)),
            _ => None,
        },
        // A SubAnnotationPropertyOf renders under its SUB-property's section (EFO's
        // created_by ⊑ dc:creator and skos:prefLabel ⊑ rdfs:label sit under the
        // Annotation Properties banner).
        Component::SubAnnotationPropertyOf(ax) => {
            Some((ax.sub.0.as_ref().to_string(), EKind::AnnotationProperty))
        }
        // …and so do the annotation-property domain/range axioms, which OBO cannot
        // express at all (MONDO's `AnnotationPropertyRange(IAO_0006012 xsd:dateTime)`
        // sits under `# Annotation Property: IAO_0006012`).
        Component::AnnotationPropertyRange(ax) => {
            Some((ax.ap.0.as_ref().to_string(), EKind::AnnotationProperty))
        }
        Component::AnnotationPropertyDomain(ax) => {
            Some((ax.ap.0.as_ref().to_string(), EKind::AnnotationProperty))
        }
        // An AnnotationAssertion is grouped by the section loop against its subject
        // IRI directly (see `ann_subject`), so `owner_entity` leaves it `None` here.
        _ => None,
    }
}

/// rdf/rdfs/owl/xsd/xml properties are builtin — they are never declared.
fn is_builtin_prop(iri: &str) -> bool {
    iri.starts_with(RDF_NS)
        || iri.starts_with(RDFS_NS)
        || iri.starts_with(OWL_NS)
        || iri.starts_with(XSD_NS)
        || iri.starts_with(XML_NS)
}

fn named_class(ce: &CE<RcStr>) -> Option<String> {
    match ce {
        CE::Class(c) => Some(c.0.as_ref().to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Signature
// ---------------------------------------------------------------------------

/// Collect the referenced entities of the untranslatable axioms, split by kind.
/// (Every entity in the signature gets a `Declaration(…)`.)
struct Sig {
    classes: BTreeSet<String>,
    oprops: BTreeSet<String>,
    inds: BTreeSet<String>,
    aprops: BTreeSet<String>,
    datatypes: BTreeSet<String>,
}

fn signature(axioms: &[&AnnotatedComponent<RcStr>]) -> Sig {
    let mut s = Sig {
        classes: BTreeSet::new(),
        oprops: BTreeSet::new(),
        inds: BTreeSet::new(),
        aprops: BTreeSet::new(),
        datatypes: BTreeSet::new(),
    };
    for ac in axioms {
        sig_component(&ac.component, &mut s.classes, &mut s.oprops, &mut s.inds, &mut s.aprops);
        if let Component::AnnotationAssertion(aa) = &ac.component {
            sig_dt_value(&aa.ann.av, &mut s.datatypes);
        }
        for a in &ac.ann {
            sig_annotation(a, &mut s.classes, &mut s.oprops, &mut s.inds, &mut s.aprops);
            sig_dt_value(&a.av, &mut s.datatypes);
        }
    }
    s
}

/// Every literal's datatype counts towards the signature, which makes the
/// (undeclared, builtin) Datatypes section non-empty and so emit its trailing
/// blank line. Only the set's non-emptiness matters, so a representative datatype
/// per literal kind suffices.
fn sig_dt_value(av: &AnnotationValue<RcStr>, dt: &mut BTreeSet<String>) {
    if let AnnotationValue::Literal(l) = av {
        let d = match l {
            Literal::Simple { .. } => "http://www.w3.org/2001/XMLSchema#string",
            Literal::Language { .. } => "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString",
            Literal::Datatype { datatype_iri, .. } => datatype_iri.as_ref(),
        };
        dt.insert(d.to_string());
    }
}

fn sig_ce(ce: &CE<RcStr>, cl: &mut BTreeSet<String>, op: &mut BTreeSet<String>, ind: &mut BTreeSet<String>) {
    match ce {
        CE::Class(c) => {
            // owl:Thing / owl:Nothing are builtin — never declared.
            if !c.0.as_ref().starts_with(OWL_NS) {
                cl.insert(c.0.as_ref().to_string());
            }
        }
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => {
            v.iter().for_each(|x| sig_ce(x, cl, op, ind))
        }
        CE::ObjectComplementOf(x) => sig_ce(x, cl, op, ind),
        CE::ObjectSomeValuesFrom { ope, bce } | CE::ObjectAllValuesFrom { ope, bce } => {
            sig_ope(ope, op);
            sig_ce(bce, cl, op, ind);
        }
        CE::ObjectMinCardinality { ope, bce, .. }
        | CE::ObjectExactCardinality { ope, bce, .. }
        | CE::ObjectMaxCardinality { ope, bce, .. } => {
            sig_ope(ope, op);
            sig_ce(bce, cl, op, ind);
        }
        CE::ObjectHasValue { ope, i } => {
            sig_ope(ope, op);
            if let Individual::Named(n) = i {
                ind.insert(n.0.as_ref().to_string());
            }
        }
        CE::ObjectHasSelf(ope) => sig_ope(ope, op),
        CE::ObjectOneOf(v) => v.iter().for_each(|i| {
            if let Individual::Named(n) = i {
                ind.insert(n.0.as_ref().to_string());
            }
        }),
        _ => {}
    }
}

fn sig_ope(ope: &OPE<RcStr>, op: &mut BTreeSet<String>) {
    op.insert(ope_iri(ope).to_string());
}

fn sig_atom(
    atom: &Atom<RcStr>,
    cl: &mut BTreeSet<String>,
    op: &mut BTreeSet<String>,
    ind: &mut BTreeSet<String>,
) {
    match atom {
        Atom::ObjectPropertyAtom { pred, args } => {
            sig_ope(pred, op);
            for a in [&args.0, &args.1] {
                if let IArgument::Individual(Individual::Named(n)) = a {
                    ind.insert(n.0.as_ref().to_string());
                }
            }
        }
        Atom::ClassAtom { pred, arg } => {
            sig_ce(pred, cl, op, ind);
            if let IArgument::Individual(Individual::Named(n)) = arg {
                ind.insert(n.0.as_ref().to_string());
            }
        }
        _ => {}
    }
}

fn sig_annotation(
    a: &Annotation<RcStr>,
    _cl: &mut BTreeSet<String>,
    _op: &mut BTreeSet<String>,
    _ind: &mut BTreeSet<String>,
    ap: &mut BTreeSet<String>,
) {
    if !is_builtin_prop(a.ap.0.as_ref()) {
        ap.insert(a.ap.0.as_ref().to_string());
    }
}

fn sig_component(
    c: &Component<RcStr>,
    cl: &mut BTreeSet<String>,
    op: &mut BTreeSet<String>,
    ind: &mut BTreeSet<String>,
    ap: &mut BTreeSet<String>,
) {
    match c {
        Component::SubClassOf(ax) => {
            sig_ce(&ax.sub, cl, op, ind);
            sig_ce(&ax.sup, cl, op, ind);
        }
        Component::EquivalentClasses(ax) => ax.0.iter().for_each(|x| sig_ce(x, cl, op, ind)),
        Component::DisjointClasses(ax) => ax.0.iter().for_each(|x| sig_ce(x, cl, op, ind)),
        Component::DisjointUnion(ax) => {
            cl.insert(ax.0 .0.as_ref().to_string());
            ax.1.iter().for_each(|x| sig_ce(x, cl, op, ind));
        }
        Component::ObjectPropertyRange(ax) => {
            sig_ope(&ax.ope, op);
            sig_ce(&ax.ce, cl, op, ind);
        }
        Component::SubObjectPropertyOf(ax) => {
            match &ax.sub {
                SOPE::ObjectPropertyExpression(o) => sig_ope(o, op),
                SOPE::ObjectPropertyChain(v) => v.iter().for_each(|o| sig_ope(o, op)),
            }
            sig_ope(&ax.sup, op);
        }
        Component::ObjectPropertyAssertion(ax) => {
            sig_ope(&ax.ope, op);
            if let Individual::Named(n) = &ax.from {
                ind.insert(n.0.as_ref().to_string());
            }
            if let Individual::Named(n) = &ax.to {
                ind.insert(n.0.as_ref().to_string());
            }
        }
        Component::IrreflexiveObjectProperty(ax) => sig_ope(&ax.0, op),
        Component::DifferentIndividuals(ax) => {
            for i in &ax.0 {
                if let Individual::Named(n) = i {
                    ind.insert(n.0.as_ref().to_string());
                }
            }
        }
        Component::SubAnnotationPropertyOf(ax) => {
            if !is_builtin_prop(ax.sub.0.as_ref()) {
                ap.insert(ax.sub.0.as_ref().to_string());
            }
            if !is_builtin_prop(ax.sup.0.as_ref()) {
                ap.insert(ax.sup.0.as_ref().to_string());
            }
        }
        Component::AnnotationPropertyRange(ax) => {
            if !is_builtin_prop(ax.ap.0.as_ref()) {
                ap.insert(ax.ap.0.as_ref().to_string());
            }
        }
        Component::AnnotationPropertyDomain(ax) => {
            if !is_builtin_prop(ax.ap.0.as_ref()) {
                ap.insert(ax.ap.0.as_ref().to_string());
            }
        }
        Component::Rule(r) => {
            for atom in r.body.iter().chain(r.head.iter()) {
                sig_atom(atom, cl, op, ind);
            }
        }
        Component::AnnotationAssertion(ax) => {
            // Only the annotation property is a declared entity; the subject is a
            // bare IRI and the value (IRI/literal) is not a typed entity.
            if !is_builtin_prop(ax.ann.ap.0.as_ref()) {
                ap.insert(ax.ann.ap.0.as_ref().to_string());
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

struct Renderer<'a> {
    out: String,
    labels: &'a std::collections::HashMap<String, String>,
    focused: Option<String>,
}

impl<'a> Renderer<'a> {
    fn w(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn iri(&mut self, iri: &str) {
        // Only owl/rdf/rdfs/xsd/xml get a CURIE; every other IRI is written in full.
        let short = if let Some(l) = iri.strip_prefix(OWL_NS) {
            Some(format!("owl:{l}"))
        } else if let Some(l) = iri.strip_prefix(RDF_NS) {
            Some(format!("rdf:{l}"))
        } else if let Some(l) = iri.strip_prefix(RDFS_NS) {
            Some(format!("rdfs:{l}"))
        } else if let Some(l) = iri.strip_prefix(XSD_NS) {
            Some(format!("xsd:{l}"))
        } else if let Some(l) = iri.strip_prefix(&format!("{XML_NS}#")) {
            Some(format!("xml:{l}"))
        } else {
            None
        };
        match short {
            Some(q) if !q.ends_with(':') => self.w(&q),
            _ => {
                self.w("<");
                self.w(iri);
                self.w(">");
            }
        }
    }

    fn ope(&mut self, ope: &OPE<RcStr>) {
        match ope {
            OPE::ObjectProperty(p) => self.iri(p.0.as_ref()),
            OPE::InverseObjectProperty(p) => {
                self.w("ObjectInverseOf(");
                self.iri(p.0.as_ref());
                self.w(")");
            }
        }
    }

    fn individual(&mut self, i: &Individual<RcStr>) {
        match i {
            Individual::Named(n) => self.iri(n.0.as_ref()),
            Individual::Anonymous(a) => self.w(&format!("_:{}", a.0.as_ref())),
        }
    }

    fn ce(&mut self, ce: &CE<RcStr>) {
        match ce {
            CE::Class(c) => self.iri(c.0.as_ref()),
            CE::ObjectIntersectionOf(v) => {
                self.w("ObjectIntersectionOf(");
                self.ce_list(v);
                self.w(")");
            }
            CE::ObjectUnionOf(v) => {
                self.w("ObjectUnionOf(");
                self.ce_list(v);
                self.w(")");
            }
            CE::ObjectComplementOf(x) => {
                self.w("ObjectComplementOf(");
                self.ce(x);
                self.w(")");
            }
            CE::ObjectSomeValuesFrom { ope, bce } => {
                self.w("ObjectSomeValuesFrom(");
                self.ope(ope);
                self.w(" ");
                self.ce(bce);
                self.w(")");
            }
            CE::ObjectAllValuesFrom { ope, bce } => {
                self.w("ObjectAllValuesFrom(");
                self.ope(ope);
                self.w(" ");
                self.ce(bce);
                self.w(")");
            }
            CE::ObjectHasSelf(ope) => {
                self.w("ObjectHasSelf(");
                self.ope(ope);
                self.w(")");
            }
            CE::ObjectHasValue { ope, i } => {
                self.w("ObjectHasValue(");
                self.ope(ope);
                self.w(" ");
                self.individual(i);
                self.w(")");
            }
            CE::ObjectMinCardinality { n, ope, bce } => self.card("ObjectMinCardinality", *n, ope, bce),
            CE::ObjectExactCardinality { n, ope, bce } => {
                self.card("ObjectExactCardinality", *n, ope, bce)
            }
            CE::ObjectMaxCardinality { n, ope, bce } => self.card("ObjectMaxCardinality", *n, ope, bce),
            CE::ObjectOneOf(v) => {
                self.w("ObjectOneOf(");
                for (k, i) in v.iter().enumerate() {
                    if k > 0 {
                        self.w(" ");
                    }
                    self.individual(i);
                }
                self.w(")");
            }
            _ => {}
        }
    }

    /// The atoms of a rule's body or head, space-separated. A collection of exactly
    /// two renders its members swapped (second, first); one atom, or three or more,
    /// renders in stored order. The swap is the order released files carry, so
    /// changing it would rewrite every 2-atom body and head in every release diff.
    fn atom_collection(&mut self, atoms: &[Atom<RcStr>]) {
        match atoms.len() {
            0 => {}
            1 => self.atom(&atoms[0]),
            2 => {
                self.atom(&atoms[1]);
                self.w(" ");
                self.atom(&atoms[0]);
            }
            _ => {
                for (k, a) in atoms.iter().enumerate() {
                    if k > 0 {
                        self.w(" ");
                    }
                    self.atom(a);
                }
            }
        }
    }

    fn atom(&mut self, atom: &Atom<RcStr>) {
        match atom {
            Atom::ObjectPropertyAtom { pred, args } => {
                self.w("ObjectPropertyAtom(");
                self.ope(pred);
                self.w(" ");
                self.iarg(&args.0);
                self.w(" ");
                self.iarg(&args.1);
                self.w(")");
            }
            Atom::ClassAtom { pred, arg } => {
                self.w("ClassAtom(");
                self.ce(pred);
                self.w(" ");
                self.iarg(arg);
                self.w(")");
            }
            _ => {}
        }
    }

    fn iarg(&mut self, a: &IArgument<RcStr>) {
        match a {
            IArgument::Individual(i) => self.individual(i),
            IArgument::Variable(v) => {
                self.w("Variable(");
                self.iri(v.0.as_ref());
                self.w(")");
            }
        }
    }

    fn card(&mut self, tag: &str, n: u32, ope: &OPE<RcStr>, bce: &CE<RcStr>) {
        self.w(tag);
        self.w("(");
        self.w(&n.to_string());
        self.w(" ");
        self.ope(ope);
        // An owl:Thing filler is implicit and omitted.
        if !matches!(bce, CE::Class(c) if c.0.as_ref() == format!("{OWL_NS}Thing")) {
            self.w(" ");
            self.ce(bce);
        }
        self.w(")");
    }

    /// An operand list — operands in `cmp_ce` order, space-separated.
    fn ce_list(&mut self, v: &[CE<RcStr>]) {
        let mut items: Vec<&CE<RcStr>> = v.iter().collect();
        items.sort_by(|a, b| cmp_ce(a, b));
        for (k, ce) in items.iter().enumerate() {
            if k > 0 {
                self.w(" ");
            }
            self.ce(ce);
        }
    }

    fn literal(&mut self, lit: &Literal<RcStr>) {
        match lit {
            Literal::Simple { literal } => {
                self.w(&escape_str(literal));
            }
            Literal::Language { literal, lang } => {
                self.w(&escape_str(literal));
                self.w("@");
                self.w(lang);
            }
            Literal::Datatype { literal, datatype_iri } => {
                let dt = datatype_iri.as_ref();
                self.w(&escape_str(literal));
                if dt != XSD_STRING && dt != RDF_PLAIN && dt != RDF_LANGSTRING {
                    self.w("^^");
                    self.iri(dt);
                }
            }
        }
    }

    fn annotation(&mut self, a: &Annotation<RcStr>) {
        self.w("Annotation(");
        self.iri(a.ap.0.as_ref());
        self.w(" ");
        match &a.av {
            AnnotationValue::Literal(l) => self.literal(l),
            AnnotationValue::IRI(i) => self.iri(i.as_ref()),
            AnnotationValue::AnonymousIndividual(x) => self.w(&format!("_:{}", x.0.as_ref())),
        }
        self.w(")");
    }

    /// The sorted annotations of an axiom, each followed by a space.
    fn axiom_annotations(&mut self, anns: &BTreeSet<Annotation<RcStr>>) {
        let mut v: Vec<&Annotation<RcStr>> = anns.iter().collect();
        v.sort();
        for a in v {
            self.annotation(a);
            self.w(" ");
        }
    }

    /// Render one axiom in functional syntax (no trailing newline).
    fn axiom(&mut self, ac: &AnnotatedComponent<RcStr>) {
        let anns = &ac.ann;
        match &ac.component {
            Component::DeclareClass(d) => self.decl("Class", d.0 .0.as_ref(), anns),
            Component::DeclareObjectProperty(d) => self.decl("ObjectProperty", d.0 .0.as_ref(), anns),
            Component::DeclareNamedIndividual(d) => {
                self.decl("NamedIndividual", d.0 .0.as_ref(), anns)
            }
            Component::DeclareAnnotationProperty(d) => {
                self.decl("AnnotationProperty", d.0 .0.as_ref(), anns)
            }
            Component::DeclareDataProperty(d) => self.decl("DataProperty", d.0 .0.as_ref(), anns),
            Component::DeclareDatatype(d) => self.decl("Datatype", d.0 .0.as_ref(), anns),
            Component::SubClassOf(ax) => {
                self.w("SubClassOf(");
                self.axiom_annotations(anns);
                self.ce(&ax.sub);
                self.w(" ");
                self.ce(&ax.sup);
                self.w(")");
            }
            Component::EquivalentClasses(ax) => {
                self.w("EquivalentClasses(");
                self.axiom_annotations(anns);
                self.ce_list(&ax.0);
                self.w(")");
            }
            Component::DisjointClasses(ax) => {
                self.w("DisjointClasses(");
                self.axiom_annotations(anns);
                self.ce_list(&ax.0);
                self.w(")");
            }
            Component::DisjointUnion(ax) => {
                self.w("DisjointUnion(");
                self.axiom_annotations(anns);
                self.iri(ax.0 .0.as_ref());
                self.w(" ");
                self.ce_list(&ax.1);
                self.w(")");
            }
            Component::ObjectPropertyRange(ax) => {
                self.w("ObjectPropertyRange(");
                self.axiom_annotations(anns);
                self.ope(&ax.ope);
                self.w(" ");
                self.ce(&ax.ce);
                self.w(")");
            }
            Component::SubObjectPropertyOf(ax) => {
                self.w("SubObjectPropertyOf(");
                self.axiom_annotations(anns);
                match &ax.sub {
                    SOPE::ObjectPropertyExpression(o) => self.ope(o),
                    SOPE::ObjectPropertyChain(v) => {
                        self.w("ObjectPropertyChain(");
                        for (k, o) in v.iter().enumerate() {
                            if k > 0 {
                                self.w(" ");
                            }
                            self.ope(o);
                        }
                        self.w(")");
                    }
                }
                self.w(" ");
                self.ope(&ax.sup);
                self.w(")");
            }
            Component::ObjectPropertyAssertion(ax) => {
                self.w("ObjectPropertyAssertion(");
                self.axiom_annotations(anns);
                self.ope(&ax.ope);
                self.w(" ");
                self.individual(&ax.from);
                self.w(" ");
                self.individual(&ax.to);
                self.w(")");
            }
            Component::IrreflexiveObjectProperty(ax) => {
                self.w("IrreflexiveObjectProperty(");
                self.axiom_annotations(anns);
                self.ope(&ax.0);
                self.w(")");
            }
            Component::DifferentIndividuals(ax) => {
                self.w("DifferentIndividuals(");
                self.axiom_annotations(anns);
                for (n, i) in ax.0.iter().enumerate() {
                    if n > 0 {
                        self.w(" ");
                    }
                    self.individual(i);
                }
                self.w(")");
            }
            Component::SubAnnotationPropertyOf(ax) => {
                self.w("SubAnnotationPropertyOf(");
                self.axiom_annotations(anns);
                self.iri(ax.sub.0.as_ref());
                self.w(" ");
                self.iri(ax.sup.0.as_ref());
                self.w(")");
            }
            Component::AnnotationPropertyRange(ax) => {
                self.w("AnnotationPropertyRange(");
                self.axiom_annotations(anns);
                self.iri(ax.ap.0.as_ref());
                self.w(" ");
                self.iri(ax.iri.as_ref());
                self.w(")");
            }
            Component::AnnotationPropertyDomain(ax) => {
                self.w("AnnotationPropertyDomain(");
                self.axiom_annotations(anns);
                self.iri(ax.ap.0.as_ref());
                self.w(" ");
                self.iri(ax.iri.as_ref());
                self.w(")");
            }
            Component::Rule(r) => {
                self.w("DLSafeRule(");
                self.axiom_annotations(anns);
                self.w("Body(");
                self.atom_collection(&r.body);
                self.w(")Head(");
                self.atom_collection(&r.head);
                self.w("))");
            }
            Component::AnnotationAssertion(ax) => {
                self.w("AnnotationAssertion(");
                self.axiom_annotations(anns);
                self.iri(ax.ann.ap.0.as_ref());
                self.w(" ");
                match &ax.subject {
                    AnnotationSubject::IRI(i) => self.iri(i.as_ref()),
                    AnnotationSubject::AnonymousIndividual(a) => {
                        self.w(&format!("_:{}", a.0.as_ref()))
                    }
                }
                self.w(" ");
                match &ax.ann.av {
                    AnnotationValue::Literal(l) => self.literal(l),
                    AnnotationValue::IRI(i) => self.iri(i.as_ref()),
                    AnnotationValue::AnonymousIndividual(x) => {
                        self.w(&format!("_:{}", x.0.as_ref()))
                    }
                }
                self.w(")");
            }
            _ => {}
        }
    }

    fn decl(&mut self, kind: &str, iri: &str, anns: &BTreeSet<Annotation<RcStr>>) {
        self.w("Declaration(");
        self.axiom_annotations(anns);
        self.w(kind);
        self.w("(");
        self.iri(iri);
        self.w(")");
        self.w(")");
    }

    fn entity_comment(&mut self, ekind: &str, iri: &str) {
        self.w("# ");
        self.w(ekind);
        self.w(": <");
        self.w(iri);
        self.w("> (");
        match self.labels.get(iri) {
            Some(l) => {
                let l = l.replace('\n', "\n# ");
                self.w(&l);
            }
            None => {
                self.w("<");
                self.w(iri);
                self.w(">");
            }
        }
        self.w(")\n");
    }
}

fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// One component in functional syntax, standing on its own — for a report that
/// lists components a line at a time rather than assembling a document.
///
/// An ontology annotation is written the way the header writes it,
/// `Annotation(<property> <value>)`: on its own line there is no `Ontology(` for
/// it to sit inside, and the axiom renderer passes it over for that reason.
pub(crate) fn render_component_line(ac: &AnnotatedComponent<RcStr>) -> String {
    let labels = std::collections::HashMap::new();
    let mut r = Renderer { out: String::new(), labels: &labels, focused: None };
    match &ac.component {
        Component::OntologyAnnotation(a) => r.annotation(&a.0),
        _ => r.axiom(ac),
    }
    r.out
}

/// Build the functional-syntax `owl-axioms:` value (before OBO escaping) from the
/// untranslatable axioms. Returns `None` if there are none.
pub fn render_owl_axioms(
    untranslatable: &[&AnnotatedComponent<RcStr>],
    labels: &std::collections::HashMap<String, String>,
) -> Option<String> {
    if untranslatable.is_empty() {
        return None;
    }
    // Dedup axioms that render identically: horned-owl keeps both operand orders of
    // a symmetric axiom (a `DisjointClasses(A B)` and a `DisjointClasses(B A)`),
    // which say the same thing and must be written once.
    let mut seen: HashSet<String> = HashSet::new();
    let mut deduped: Vec<&AnnotatedComponent<RcStr>> = Vec::new();
    for ac in untranslatable {
        let mut tmp = Renderer { out: String::new(), labels, focused: None };
        tmp.axiom(ac);
        if seen.insert(tmp.out) {
            deduped.push(ac);
        }
    }
    let untranslatable: &[&AnnotatedComponent<RcStr>] = &deduped;
    let Sig { classes, oprops, inds, aprops, datatypes } = signature(untranslatable);
    let dprops: BTreeSet<String> = BTreeSet::new();

    let mut r = Renderer { out: String::new(), labels, focused: None };
    // The five builtin prefixes in fixed order, then blank line, Ontology(.
    r.w("Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n");
    r.w("Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)\n");
    r.w("Prefix(xml:=<http://www.w3.org/XML/1998/namespace>)\n");
    r.w("Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n");
    r.w("Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n");
    r.w("\n\nOntology(\n");

    // Declarations: entities in typeIndex order (Class 1001, ObjectProperty 1002,
    // NamedIndividual 1005, AnnotationProperty 1006), IRI-sorted within.
    for iri in &classes {
        r.w("Declaration(Class(");
        r.iri(iri);
        r.w("))\n");
    }
    for iri in &oprops {
        r.w("Declaration(ObjectProperty(");
        r.iri(iri);
        r.w("))\n");
    }
    for iri in &inds {
        r.w("Declaration(NamedIndividual(");
        r.iri(iri);
        r.w("))\n");
    }
    for iri in &aprops {
        r.w("Declaration(AnnotationProperty(");
        r.iri(iri);
        r.w("))\n");
    }

    // Partition axioms into entity buckets and leftover.
    let mut written: HashSet<usize> = HashSet::new();
    let mut sections: Vec<(EKind, &BTreeSet<String>, &str, &str)> = vec![
        (EKind::AnnotationProperty, &aprops, "Annotation Properties", "Annotation Property"),
        (EKind::ObjectProperty, &oprops, "Object Properties", "Object Property"),
        (EKind::DataProperty, &dprops, "Data Properties", "Data Property"),
        (EKind::Datatype, &datatypes, "Datatypes", "Datatype"),
        (EKind::Class, &classes, "Classes", "Class"),
        (EKind::NamedIndividual, &inds, "Named Individuals", "Individual"),
    ];
    // owners: axiom index -> (iri, kind)
    let owners: Vec<Option<(String, EKind)>> =
        untranslatable.iter().map(|ac| owner_entity(&ac.component)).collect();
    // An AnnotationAssertion is written under its subject-entity's section —
    // CHEBI_64208's `hasDbXref`s under `# Class: CHEBI_64208` — when the subject IRI
    // is a signature entity; the remainder (subject not declared) fall to the
    // leftover block. Keyed by subject IRI, matched against whichever section's
    // entity set contains it.
    let ann_subjects: Vec<Option<String>> = untranslatable
        .iter()
        .map(|ac| match &ac.component {
            Component::AnnotationAssertion(aa) => match &aa.subject {
                AnnotationSubject::IRI(i) => Some(i.as_ref().to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();

    // Axioms that make an entity's block NON-empty without ever being printed in it.
    // A `DifferentIndividuals` axiom, and a `DisjointClasses` with more than two
    // operands, is never rendered inside an entity block — it falls through to the
    // trailing leftover block — but it still counts when deciding whether the block
    // is written at all, because that emptiness test runs over the entity's unfiltered
    // axiom set. So an entity whose only axioms are of those two kinds gets a
    // `# Class: …` / `# Individual: …` comment with nothing under it. That is MONDO's
    // case: `FOODON_034121115` & co. appear only in 3+-way `DisjointClasses`, and
    // `IAO_0000120`…`IAO_0000428` only in one `DifferentIndividuals`.
    let mut mention_only: HashSet<String> = HashSet::new();
    for ac in untranslatable {
        match &ac.component {
            Component::DifferentIndividuals(ax) => {
                for i in &ax.0 {
                    if let Individual::Named(n) = i {
                        mention_only.insert(n.0.as_ref().to_string());
                    }
                }
            }
            Component::DisjointClasses(ax) if ax.0.len() > 2 => {
                for ce in &ax.0 {
                    if let CE::Class(c) = ce {
                        mention_only.insert(c.0.as_ref().to_string());
                    }
                }
            }
            _ => {}
        }
    }

    for (kind, entities, banner, etype) in sections.drain(..) {
        // A section runs (and emits a trailing blank) only for a non-empty entity
        // set — even when no entity has renderable content, which is what gives the
        // blank line after the declarations block (the annotation-property set).
        if entities.is_empty() {
            continue;
        }
        let mut wrote_banner = false;
        for ent in entities {
            // axioms owned by this entity, not yet written
            let mut group: Vec<usize> = Vec::new();
            for (i, o) in owners.iter().enumerate() {
                if written.contains(&i) {
                    continue;
                }
                if let Some((oiri, okind)) = o {
                    if *okind == kind && oiri == ent {
                        group.push(i);
                        continue;
                    }
                }
                // AnnotationAssertion attaches to the section whose entity is its subject.
                if ann_subjects[i].as_deref() == Some(ent.as_str()) {
                    group.push(i);
                }
            }
            // annotation assertions on this entity render as annotations, separate.
            let has_axioms = group
                .iter()
                .any(|&i| !matches!(untranslatable[i].component, Component::AnnotationAssertion(_)));
            let has_anns = group
                .iter()
                .any(|&i| matches!(untranslatable[i].component, Component::AnnotationAssertion(_)));
            // An entity with neither axioms nor annotation assertions is SKIPPED
            // outright, whatever its kind, and the banner is written lazily by the
            // first entity that survives. So a document whose only individuals occur
            // inside an `ObjectOneOf` gets no `#   Named Individuals` section at all.
            if !has_axioms && !has_anns && !mention_only.contains(ent) {
                continue;
            }
            if !wrote_banner {
                r.w("############################\n#   ");
                r.w(banner);
                r.w("\n############################\n\n");
                wrote_banner = true;
            }
            r.entity_comment(etype, ent);
            r.w("\n");
            r.focused = Some(ent.clone());
            // annotation assertions first (sorted), then the axioms (sorted),
            // excluding DisjointClasses with >2 operands.
            let mut ann_ids: Vec<usize> = group
                .iter()
                .cloned()
                .filter(|&i| matches!(untranslatable[i].component, Component::AnnotationAssertion(_)))
                .collect();
            ann_ids.sort_by(|&a, &b| cmp_component(&untranslatable[a].component, &untranslatable[b].component));
            for i in ann_ids {
                r.axiom(untranslatable[i]);
                r.w("\n");
                written.insert(i);
            }
            let mut ax_ids: Vec<usize> = group
                .iter()
                .cloned()
                .filter(|&i| {
                    !matches!(untranslatable[i].component, Component::AnnotationAssertion(_))
                        && !matches!(&untranslatable[i].component,
                            Component::DisjointClasses(d) if d.0.len() > 2)
                })
                .collect();
            ax_ids.sort_by(|&a, &b| cmp_component(&untranslatable[a].component, &untranslatable[b].component));
            for i in ax_ids {
                r.axiom(untranslatable[i]);
                r.w("\n");
                written.insert(i);
            }
            r.w("\n");
        }
        // Trailing blank line closing the section.
        r.w("\n");
    }

    // Leftover axioms (DisjointClasses>2, GCI, SWRL rules), sorted.
    let mut leftover: Vec<usize> = (0..untranslatable.len()).filter(|i| !written.contains(i)).collect();
    leftover.sort_by(|&a, &b| cmp_component(&untranslatable[a].component, &untranslatable[b].component));
    r.focused = None;
    for i in leftover {
        r.axiom(untranslatable[i]);
        r.w("\n");
    }

    r.w(")");
    Some(r.out)
}
