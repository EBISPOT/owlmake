//! OWLAPI-compatible content hash codes and java.util.HashSet iteration order.
//!
//! Several merge operations resolve "which axiom's mapping wins" by iterating a
//! set of axioms and letting the last write win. The iteration order that
//! decides those winners is the order of a `java.util.HashSet` populated with
//! OWLAPI axiom objects: ascending bucket index, where the bucket is computed
//! from the axiom's OWLAPI content hash code. Reproducing that order needs
//! three pieces, all here:
//!
//! - the hash codes (`equivalent_classes_hash` and the expression hashes under
//!   it) — a prime-tagged polynomial over the axiom's components. Set-valued
//!   components are stored SORTED and hashed as Java lists (seed 1, ordered),
//!   except that an empty one hashes to 0 and the individuals of a one-of
//!   hash as a set (the sum). IRIs hash as the sum of the Java string hashes
//!   of their namespace and NCName-suffix halves;
//! - the component order — the natural order ([`NaturalOrder`]) of the
//!   document the object comes from, which sorts a set's members as OWLAPI
//!   stores them; where a document types its untyped literals `xsd:string`,
//!   its literals, and the annotations and class expressions holding them,
//!   sort differently;
//! - the bucket order (`hashset_order`) — Java's HashMap spread
//!   (`h ^ (h >>> 16)`) masked by the table capacity that results from
//!   inserting `n` elements into a default-sized table. Entries in the SAME
//!   bucket have no defined relative order (the container feeding the set
//!   iterates in randomized order), so callers keep their own tie order;
//!   distinct buckets — the overwhelmingly common case — are fully determined.

use std::cmp::Ordering;

use horned_owl::model::{
    Annotation, AnnotationValue, ClassExpression as CE, DataRange as DR, FacetRestriction, Individual, Literal,
    ObjectPropertyExpression as OPE, PropertyExpression, RcStr,
};

use crate::io::natural_order::NaturalOrder;

const MULT: i32 = 31;

// The prime tag for each component kind.
const P_EQUIVALENT_CLASSES: i32 = 811;
const P_CLASS: i32 = 2293;
const P_OBJ_ALL_VALUES: i32 = 2833;
const P_OBJ_COMPLEMENT: i32 = 2909;
const P_OBJ_EXACT_CARD: i32 = 3001;
const P_OBJ_INTERSECTION: i32 = 3083;
const P_OBJ_MAX_CARD: i32 = 3187;
const P_OBJ_MIN_CARD: i32 = 3259;
const P_OBJ_ONE_OF: i32 = 3343;
const P_OBJ_HAS_SELF: i32 = 3433;
const P_OBJ_SOME_VALUES: i32 = 3517;
const P_OBJ_UNION: i32 = 3581;
const P_OBJ_HAS_VALUE: i32 = 3659;
const P_DATA_COMPLEMENT: i32 = 3733;
const P_DATA_ONE_OF: i32 = 3823;
const P_DATATYPE: i32 = 3911;
const P_DATATYPE_RESTRICTION: i32 = 4001;
const P_FACET_RESTRICTION: i32 = 4421;
const P_DATA_INTERSECTION: i32 = 5861;
const P_DATA_UNION: i32 = 5953;
const P_OBJECT_PROPERTY: i32 = 4153;
const P_OBJ_INVERSE: i32 = 4241;
const P_NAMED_INDIVIDUAL: i32 = 4327;
/// An `AnnotationProperty` is hashed the same way wherever it appears — as an
/// assertion's property, and as the property of one of the assertion's own
/// annotations.
const P_ANNOTATION_PROPERTY: i32 = 6067;
const P_ANNOTATION_PROPERTY_ENTITY: i32 = P_ANNOTATION_PROPERTY;
const P_ANNOTATION: i32 = 6311;
const P_ANNOTATION_ASSERTION: i32 = 739;

/// Java `String.hashCode()` — over UTF-16 code units, wrapping i32.
pub fn java_string_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

/// Whether a code point may START an NCName (XML name start minus ':').
pub(crate) fn is_ncname_start(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z' | '_'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// Whether a code point may CONTINUE an NCName.
pub(crate) fn is_ncname_char(c: char) -> bool {
    is_ncname_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// Where an IRI splits into namespace + NCName suffix: the start of the longest
/// suffix that is a valid NCName (scanning back, remembering the last start
/// char, stopping at the first non-NCName char).
fn ncname_suffix_index(s: &str) -> Option<usize> {
    let mut index = None;
    for (i, c) in s.char_indices().rev() {
        if is_ncname_start(c) {
            index = Some(i);
        }
        if !is_ncname_char(c) {
            break;
        }
    }
    index
}

pub(crate) fn iri_split(iri: &str) -> (&str, &str) {
    match ncname_suffix_index(iri) {
        Some(i) => (&iri[..i], &iri[i..]),
        None => (iri, ""),
    }
}

/// OWLAPI `IRI.getShortForm()`: the NCName suffix; without one, what follows
/// the last `/` when something does; else the whole IRI in angle brackets.
pub(crate) fn iri_short_form(iri: &str) -> String {
    let (ns, rem) = iri_split(iri);
    if !rem.is_empty() {
        return rem.to_string();
    }
    match ns.rfind('/') {
        Some(i) if i + 1 < ns.len() => ns[i + 1..].to_string(),
        _ => format!("<{iri}>"),
    }
}

/// OWLAPI `IRI.hashCode()`: the namespace and remainder halves are hashed as
/// Java strings and SUMMED (not concatenated — the split point matters).
pub fn iri_hash(iri: &str) -> i32 {
    let (ns, rem) = iri_split(iri);
    java_string_hash(ns).wrapping_add(java_string_hash(rem))
}

/// OWLAPI `IRI.compareTo`: namespace first, then remainder.
pub fn iri_cmp(a: &str, b: &str) -> Ordering {
    let (na, ra) = iri_split(a);
    let (nb, rb) = iri_split(b);
    na.cmp(nb).then_with(|| ra.cmp(rb))
}

fn tag(prime: i32, parts: &[i32]) -> i32 {
    let mut h = prime;
    for p in parts {
        h = h.wrapping_mul(MULT).wrapping_add(*p);
    }
    h
}

/// Java `List.hashCode()`: seed 1, ordered polynomial.
fn list_hash(hashes: &[i32]) -> i32 {
    let mut h: i32 = 1;
    for &q in hashes {
        h = h.wrapping_mul(31).wrapping_add(q);
    }
    h
}

pub fn ope_hash(ope: &OPE<RcStr>) -> i32 {
    match ope {
        OPE::ObjectProperty(p) => tag(P_OBJECT_PROPERTY, &[iri_hash(p.0.as_ref())]),
        OPE::InverseObjectProperty(p) => {
            tag(P_OBJ_INVERSE, &[tag(P_OBJECT_PROPERTY, &[iri_hash(p.0.as_ref())])])
        }
    }
}

/// An individual's hash. A named one is tagged like any other entity; an
/// anonymous one is the string hash of its node id (`_:genid…`), untagged.
pub(crate) fn individual_hash(i: &Individual<RcStr>) -> i32 {
    match i {
        Individual::Named(n) => tag(P_NAMED_INDIVIDUAL, &[iri_hash(n.0.as_ref())]),
        Individual::Anonymous(a) => java_string_hash(&crate::io::entities::node_id(a.0.as_ref())),
    }
}

/// The hash of a set of class expressions as OWLAPI stores one: the list hash
/// of its distinct members in the document's natural order, or 0 when it has
/// none.
pub fn ce_set_hash(v: &[CE<RcStr>], order: NaturalOrder) -> i32 {
    sorted_list_hash(v, |ce| ce_hash(ce, order), |a, b| order.ce(a, b))
}

/// A class expression's hash. Its operands and the literals under it are sets,
/// stored in `order`, the natural order of the document it comes from.
pub fn ce_hash(ce: &CE<RcStr>, order: NaturalOrder) -> i32 {
    match ce {
        CE::Class(c) => tag(P_CLASS, &[iri_hash(c.0.as_ref())]),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            tag(P_OBJ_SOME_VALUES, &[ope_hash(ope), ce_hash(bce, order)])
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            tag(P_OBJ_ALL_VALUES, &[ope_hash(ope), ce_hash(bce, order)])
        }
        CE::ObjectIntersectionOf(v) => tag(P_OBJ_INTERSECTION, &[ce_set_hash(v, order)]),
        CE::ObjectUnionOf(v) => tag(P_OBJ_UNION, &[ce_set_hash(v, order)]),
        CE::ObjectComplementOf(b) => tag(P_OBJ_COMPLEMENT, &[ce_hash(b, order)]),
        CE::ObjectExactCardinality { n, ope, bce } => {
            tag(P_OBJ_EXACT_CARD, &[ope_hash(ope), *n as i32, ce_hash(bce, order)])
        }
        CE::ObjectMinCardinality { n, ope, bce } => {
            tag(P_OBJ_MIN_CARD, &[ope_hash(ope), *n as i32, ce_hash(bce, order)])
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            tag(P_OBJ_MAX_CARD, &[ope_hash(ope), *n as i32, ce_hash(bce, order)])
        }
        CE::ObjectHasValue { ope, i } => tag(P_OBJ_HAS_VALUE, &[ope_hash(ope), individual_hash(i)]),
        CE::ObjectHasSelf(ope) => tag(P_OBJ_HAS_SELF, &[ope_hash(ope)]),
        // The individuals are a set, not a sorted list, so they hash as one: the
        // sum of the distinct members' hashes.
        CE::ObjectOneOf(v) => {
            let mut inds: Vec<&Individual<RcStr>> = v.iter().collect();
            inds.sort_by(|x, y| order.individual(x, y));
            inds.dedup_by(|x, y| order.individual(x, y) == Ordering::Equal);
            let sum = inds.iter().map(|i| individual_hash(i)).fold(0i32, |acc, h| acc.wrapping_add(h));
            tag(P_OBJ_ONE_OF, &[sum])
        }
        CE::DataSomeValuesFrom { dp, dr } => tag(2689, &[data_property_hash(dp), data_range_hash(dr, order)]),
        CE::DataAllValuesFrom { dp, dr } => tag(2371, &[data_property_hash(dp), data_range_hash(dr, order)]),
        CE::DataHasValue { dp, l } => tag(2749, &[data_property_hash(dp), literal_hash(l)]),
        CE::DataExactCardinality { n, dp, dr } => {
            tag(2437, &[data_property_hash(dp), *n as i32, data_range_hash(dr, order)])
        }
        CE::DataMaxCardinality { n, dp, dr } => {
            tag(2539, &[data_property_hash(dp), *n as i32, data_range_hash(dr, order)])
        }
        CE::DataMinCardinality { n, dp, dr } => {
            tag(2621, &[data_property_hash(dp), *n as i32, data_range_hash(dr, order)])
        }
    }
}

/// A literal's contribution to its own hash: `n * 65536`, where `n` is the
/// value the literal's datatype reads out of the lexical form — the parsed
/// number for `xsd:integer`/`xsd:double`/`xsd:float`, 1/0 for `xsd:boolean` —
/// and the Java string hash of the lexical form for every other datatype.
///
/// So `"007"^^xsd:integer` contributes 7, not the hash of `"007"`. A lexical
/// form the datatype cannot read as a number — an integer too wide for 32 bits —
/// falls back to the string hash, exactly as an untyped literal would.
pub(crate) fn literal_payload_hash(text: &str, datatype: &str) -> i32 {
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    let string_payload = || java_string_hash(text).wrapping_mul(65536);
    match datatype.strip_prefix(XSD) {
        Some("integer") => match text.parse::<i32>() {
            Ok(n) => n.wrapping_mul(65536),
            Err(_) => string_payload(),
        },
        Some("boolean") => i32::from(text.eq_ignore_ascii_case("true")).wrapping_mul(65536),
        // A fractional value is SCALED and then narrowed, not narrowed and then
        // scaled, so 2.5 contributes 163840 rather than 2·65536.
        Some("double") => match text.parse::<f64>() {
            Ok(d) => (d * 65536.0) as i32,
            Err(_) => string_payload(),
        },
        Some("float") => match text.parse::<f32>() {
            Ok(f) => (f * 65536.0f32) as i32,
            Err(_) => string_payload(),
        },
        _ => string_payload(),
    }
}

/// `OWLLiteralImpl.hashCode()`: `(277*37 + datatype.hash)*37 + payload`, where
/// a plain/xsd:string/langString literal normalizes its datatype to
/// `rdf:PlainLiteral` and the payload is [`literal_payload_hash`].
fn literal_hash(lit: &Literal<RcStr>) -> i32 {
    match lit {
        Literal::Simple { literal } => literal_parts_hash(literal, None, None),
        Literal::Language { literal, lang } => literal_parts_hash(literal, None, Some(lang)),
        Literal::Datatype { literal, datatype_iri } => literal_parts_hash(literal, Some(datatype_iri.as_ref()), None),
    }
}

/// [`literal_hash`] of the literal of `text` typed `datatype`, untyped where
/// that is `None`, and tagged `lang` where that is given.
pub(crate) fn literal_parts_hash(text: &str, datatype: Option<&str>, lang: Option<&str>) -> i32 {
    const PLAIN: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
    let dt = match datatype {
        Some(d) if d != "http://www.w3.org/2001/XMLSchema#string" => d,
        _ => PLAIN,
    };
    let dt_hash = tag(P_DATATYPE, &[iri_hash(dt)]);
    let payload = literal_payload_hash(text, dt);
    let h = (277i32.wrapping_mul(37).wrapping_add(dt_hash)).wrapping_mul(37).wrapping_add(payload);
    // A language tag is folded in after the payload.
    match lang {
        Some(lang) => h.wrapping_mul(37).wrapping_add(java_string_hash(lang)),
        None => h,
    }
}

fn annotation_value_hash(av: &AnnotationValue<RcStr>) -> i32 {
    match av {
        AnnotationValue::IRI(iri) => iri_hash(iri.as_ref()),
        AnnotationValue::Literal(l) => literal_hash(l),
        AnnotationValue::AnonymousIndividual(a) => java_string_hash(&crate::io::entities::node_id(a.0.as_ref())),
    }
}

/// The hash of an `AnnotationAssertion`, over subject, property, value and
/// annotations in that order. Ordering a subject's assertions by this hash is
/// what decides which of several competing labels an entity is exported with.
pub fn annotation_assertion_hash(
    subject: &str,
    property: &str,
    value: &AnnotationValue<RcStr>,
    anns: &std::collections::BTreeSet<Annotation<RcStr>>,
    order: NaturalOrder,
) -> i32 {
    tag(
        P_ANNOTATION_ASSERTION,
        &[
            iri_hash(subject),
            tag(P_ANNOTATION_PROPERTY_ENTITY, &[iri_hash(property)]),
            annotation_value_hash(value),
            axiom_annotations_hash(anns, order),
        ],
    )
}

pub fn annotation_hash(a: &Annotation<RcStr>) -> i32 {
    tag(
        P_ANNOTATION,
        &[
            tag(P_ANNOTATION_PROPERTY, &[iri_hash(a.ap.0.as_ref())]),
            annotation_value_hash(&a.av),
        ],
    )
}

/// The hash of an axiom's annotation set: 0 when it has none, else the list
/// hash of its annotations sorted by property and then value, a literal value
/// by datatype, lexical form and language — the datatype an untyped literal
/// keys as being the document's (`order`).
pub(crate) fn axiom_annotations_hash(anns: &std::collections::BTreeSet<Annotation<RcStr>>, order: NaturalOrder) -> i32 {
    if anns.is_empty() {
        return 0;
    }
    let sorted = order.sorted_annotations(anns);
    list_hash(&sorted.iter().map(|a| annotation_hash(a)).collect::<Vec<i32>>())
}

fn data_property_hash(dp: &horned_owl::model::DataProperty<RcStr>) -> i32 {
    entity_hash(crate::sig::kind::DATA_PROPERTY, dp.0.as_ref())
}

fn datatype_hash(iri: &str) -> i32 {
    tag(P_DATATYPE, &[iri_hash(iri)])
}

/// A data range's hash. Its operands, values and facet restrictions are sets,
/// stored in `order`, the natural order of the document it comes from.
pub(crate) fn data_range_hash(dr: &DR<RcStr>, order: NaturalOrder) -> i32 {
    let hash = |r: &DR<RcStr>| data_range_hash(r, order);
    match dr {
        DR::Datatype(d) => datatype_hash(d.0.as_ref()),
        DR::DataComplementOf(r) => tag(P_DATA_COMPLEMENT, &[hash(r)]),
        DR::DataOneOf(v) => tag(P_DATA_ONE_OF, &[sorted_list_hash(v, literal_hash, |x, y| order.literal(x, y))]),
        DR::DataIntersectionOf(v) => tag(P_DATA_INTERSECTION, &[sorted_list_hash(v, hash, |x, y| order.dr(x, y))]),
        DR::DataUnionOf(v) => tag(P_DATA_UNION, &[sorted_list_hash(v, hash, |x, y| order.dr(x, y))]),
        DR::DatatypeRestriction(d, fs) => tag(
            P_DATATYPE_RESTRICTION,
            &[
                datatype_hash(d.0.as_ref()),
                sorted_list_hash(fs, facet_restriction_hash, |x, y| order.facet_restriction(x, y)),
            ],
        ),
    }
}

/// A facet restriction's hash, over its facet and its value. OWLAPI hashes the
/// facet by object identity, which no document determines; the facet's
/// position among the facets stands in for it.
fn facet_restriction_hash(fr: &FacetRestriction<RcStr>) -> i32 {
    tag(P_FACET_RESTRICTION, &[i32::from(NaturalOrder::facet_index(&fr.f)), literal_hash(&fr.l)])
}

/// The hash of a set OWLAPI stores sorted: the list hash of its distinct
/// members' hashes in `cmp` order, or 0 when it has none.
fn sorted_list_hash<T>(items: &[T], hash: impl Fn(&T) -> i32, cmp: impl Fn(&T, &T) -> Ordering) -> i32 {
    if items.is_empty() {
        return 0;
    }
    let mut refs: Vec<&T> = items.iter().collect();
    refs.sort_by(|a, b| cmp(a, b));
    refs.dedup_by(|a, b| cmp(a, b) == Ordering::Equal);
    list_hash(&refs.iter().map(|x| hash(x)).collect::<Vec<i32>>())
}

fn swrl_iarg_hash(a: &horned_owl::model::IArgument<RcStr>) -> i32 {
    match a {
        horned_owl::model::IArgument::Individual(i) => tag(5189, &[individual_hash(i)]),
        horned_owl::model::IArgument::Variable(v) => tag(5099, &[iri_hash(v.0.as_ref())]),
    }
}

fn swrl_darg_hash(a: &horned_owl::model::DArgument<RcStr>) -> i32 {
    match a {
        horned_owl::model::DArgument::Literal(l) => tag(5281, &[literal_hash(l)]),
        horned_owl::model::DArgument::Variable(v) => tag(5099, &[iri_hash(v.0.as_ref())]),
    }
}

/// `SWRLAtom.hashCode()`.
pub(crate) fn swrl_atom_hash(atom: &horned_owl::model::Atom<RcStr>, order: NaturalOrder) -> i32 {
    use horned_owl::model::Atom;
    match atom {
        Atom::ClassAtom { pred, arg } => tag(4663, &[swrl_iarg_hash(arg), ce_hash(pred, order)]),
        Atom::DataRangeAtom { pred, arg } => tag(4759, &[swrl_darg_hash(arg), data_range_hash(pred, order)]),
        Atom::ObjectPropertyAtom { pred, args } => {
            tag(4861, &[swrl_iarg_hash(&args.0), swrl_iarg_hash(&args.1), ope_hash(pred)])
        }
        Atom::DataPropertyAtom { pred, args } => {
            tag(4943, &[swrl_iarg_hash(&args.0), swrl_darg_hash(&args.1), data_property_hash(pred)])
        }
        Atom::BuiltInAtom { pred, args } => tag(
            5009,
            &[list_hash(&args.iter().map(swrl_darg_hash).collect::<Vec<i32>>()), iri_hash(pred.as_ref())],
        ),
        Atom::DifferentIndividualsAtom(a, b) => tag(5393, &[swrl_iarg_hash(a), swrl_iarg_hash(b)]),
        Atom::SameIndividualAtom(a, b) => tag(5449, &[swrl_iarg_hash(a), swrl_iarg_hash(b)]),
    }
}

/// `OWLAxiom.hashCode()` for a logical axiom or a declaration: the axiom
/// kind's prime tag over its components and its annotation set, the sets among
/// them stored in `order`, the natural order of the document the axiom comes
/// from. `None` for a component that is neither.
pub fn axiom_hash(
    c: &horned_owl::model::Component<RcStr>,
    anns: &std::collections::BTreeSet<Annotation<RcStr>>,
    order: NaturalOrder,
) -> Option<i32> {
    use horned_owl::model::{Component as C, SubObjectPropertyExpression as SOPE};
    let a = axiom_annotations_hash(anns, order);
    let ce_hash = |ce: &CE<RcStr>| ce_hash(ce, order);
    let data_range_hash = |dr: &DR<RcStr>| data_range_hash(dr, order);
    let ces = |v: &[CE<RcStr>]| ce_set_hash(v, order);
    let opes = |v: &[OPE<RcStr>]| sorted_list_hash(v, ope_hash, |x, y| order.ope(x, y));
    let inds = |v: &[Individual<RcStr>]| sorted_list_hash(v, individual_hash, |x, y| order.individual(x, y));
    let dps = |v: &[horned_owl::model::DataProperty<RcStr>]| {
        sorted_list_hash(v, data_property_hash, |x, y| iri_cmp(x.0.as_ref(), y.0.as_ref()))
    };
    Some(match c {
        C::DeclareClass(x) => tag(353, &[tag(P_CLASS, &[iri_hash(x.0.as_ref())]), a]),
        C::DeclareObjectProperty(x) => tag(353, &[tag(P_OBJECT_PROPERTY, &[iri_hash(x.0.as_ref())]), a]),
        C::DeclareDataProperty(x) => tag(353, &[data_property_hash(&x.0), a]),
        C::DeclareNamedIndividual(x) => tag(353, &[tag(P_NAMED_INDIVIDUAL, &[iri_hash(x.0.as_ref())]), a]),
        C::DeclareAnnotationProperty(x) => tag(353, &[tag(P_ANNOTATION_PROPERTY, &[iri_hash(x.0.as_ref())]), a]),
        C::DeclareDatatype(x) => tag(353, &[datatype_hash(x.0.as_ref()), a]),
        C::SubClassOf(x) => tag(2063, &[ce_hash(&x.sub), ce_hash(&x.sup), a]),
        C::EquivalentClasses(x) => tag(P_EQUIVALENT_CLASSES, &[ces(&x.0), a]),
        C::DisjointClasses(x) => tag(467, &[ces(&x.0), a]),
        C::DisjointUnion(x) => tag(661, &[tag(P_CLASS, &[iri_hash((x.0).0.as_ref())]), ces(&x.1), a]),
        C::SubObjectPropertyOf(x) => match &x.sub {
            SOPE::ObjectPropertyExpression(sub) => tag(1823, &[ope_hash(sub), ope_hash(&x.sup), a]),
            SOPE::ObjectPropertyChain(chain) => tag(
                1597,
                &[list_hash(&chain.iter().map(ope_hash).collect::<Vec<i32>>()), ope_hash(&x.sup), a],
            ),
        },
        C::EquivalentObjectProperties(x) => tag(947, &[opes(&x.0), a]),
        C::DisjointObjectProperties(x) => tag(607, &[opes(&x.0), a]),
        C::InverseObjectProperties(x) => {
            1229i32
                .wrapping_mul(MULT)
                .wrapping_add(ope_hash(&x.0))
                .wrapping_add(ope_hash(&x.1))
                .wrapping_mul(MULT)
                .wrapping_add(a)
        }
        C::ObjectPropertyDomain(x) => tag(1663, &[ope_hash(&x.ope), ce_hash(&x.ce), a]),
        C::ObjectPropertyRange(x) => tag(1741, &[ope_hash(&x.ope), ce_hash(&x.ce), a]),
        C::FunctionalObjectProperty(x) => tag(1087, &[ope_hash(&x.0), a]),
        C::InverseFunctionalObjectProperty(x) => tag(1153, &[ope_hash(&x.0), a]),
        C::ReflexiveObjectProperty(x) => tag(1901, &[ope_hash(&x.0), a]),
        C::IrreflexiveObjectProperty(x) => tag(1297, &[ope_hash(&x.0), a]),
        C::SymmetricObjectProperty(x) => tag(2131, &[ope_hash(&x.0), a]),
        C::AsymmetricObjectProperty(x) => tag(37, &[ope_hash(&x.0), a]),
        C::TransitiveObjectProperty(x) => tag(2221, &[ope_hash(&x.0), a]),
        C::SubDataPropertyOf(x) => tag(283, &[data_property_hash(&x.sub), data_property_hash(&x.sup), a]),
        C::EquivalentDataProperties(x) => tag(877, &[dps(&x.0), a]),
        C::DisjointDataProperties(x) => tag(547, &[dps(&x.0), a]),
        C::DataPropertyDomain(x) => tag(179, &[data_property_hash(&x.dp), ce_hash(&x.ce), a]),
        C::DataPropertyRange(x) => tag(233, &[data_property_hash(&x.dp), data_range_hash(&x.dr), a]),
        C::FunctionalDataProperty(x) => tag(1019, &[data_property_hash(&x.0), a]),
        C::SameIndividual(x) => tag(1993, &[inds(&x.0), a]),
        C::DifferentIndividuals(x) => tag(419, &[inds(&x.0), a]),
        C::ClassAssertion(x) => tag(73, &[individual_hash(&x.i), ce_hash(&x.ce), a]),
        C::ObjectPropertyAssertion(x) => tag(1523, &[individual_hash(&x.from), ope_hash(&x.ope), individual_hash(&x.to), a]),
        C::NegativeObjectPropertyAssertion(x) => {
            tag(1453, &[individual_hash(&x.from), ope_hash(&x.ope), individual_hash(&x.to), a])
        }
        C::DataPropertyAssertion(x) => {
            tag(127, &[individual_hash(&x.from), data_property_hash(&x.dp), literal_hash(&x.to), a])
        }
        C::NegativeDataPropertyAssertion(x) => {
            tag(1381, &[individual_hash(&x.from), data_property_hash(&x.dp), literal_hash(&x.to), a])
        }
        C::Rule(r) => {
            // A rule's body and head are insertion-ordered sets, hashed as sets.
            let atoms = |v: &[horned_owl::model::Atom<RcStr>]| {
                v.iter().map(|atom| swrl_atom_hash(atom, order)).fold(0i32, |acc, h| acc.wrapping_add(h))
            };
            tag(4591, &[atoms(&r.body), atoms(&r.head)])
        }
        // A key and a datatype definition hash without their annotations.
        C::HasKey(x) => {
            let pe_hash = |pe: &PropertyExpression<RcStr>| match pe {
                PropertyExpression::ObjectPropertyExpression(o) => ope_hash(o),
                PropertyExpression::DataProperty(d) => data_property_hash(d),
                PropertyExpression::AnnotationProperty(ap) => tag(P_ANNOTATION_PROPERTY, &[iri_hash(ap.0.as_ref())]),
            };
            tag(5527, &[ce_hash(&x.ce), sorted_list_hash(&x.vpe, pe_hash, |p, q| order.property_expression(p, q))])
        }
        C::DatatypeDefinition(x) => tag(6373, &[datatype_hash(x.kind.0.as_ref()), data_range_hash(&x.range)]),
        _ => return None,
    })
}

/// `OWLEquivalentClassesAxiom.hashCode()`: the prime tag over the sorted
/// distinct member list hash and the annotation hash.
pub fn equivalent_classes_hash(
    members: &[CE<RcStr>],
    anns: &std::collections::BTreeSet<Annotation<RcStr>>,
    order: NaturalOrder,
) -> i32 {
    tag(P_EQUIVALENT_CLASSES, &[ce_set_hash(members, order), axiom_annotations_hash(anns, order)])
}

/// The hash of a named individual.
pub fn named_individual_hash(iri: &str) -> i32 {
    tag(P_NAMED_INDIVIDUAL, &[iri_hash(iri)])
}

/// The hash of a named object property.
pub fn object_property_hash(iri: &str) -> i32 {
    tag(P_OBJECT_PROPERTY, &[iri_hash(iri)])
}

/// The hash of the entity of `kind` (one of [`crate::sig::kind`]) named `iri`.
pub(crate) fn entity_hash(kind: u8, iri: &str) -> i32 {
    use crate::sig::kind;
    let prime = match kind {
        kind::CLASS => P_CLASS,
        kind::OBJECT_PROPERTY => P_OBJECT_PROPERTY,
        kind::DATA_PROPERTY => 4073,
        kind::ANNOTATION_PROPERTY => P_ANNOTATION_PROPERTY_ENTITY,
        kind::NAMED_INDIVIDUAL => P_NAMED_INDIVIDUAL,
        _ => P_DATATYPE,
    };
    tag(prime, &[iri_hash(iri)])
}

/// The capacities a Trove 3 hash table takes, in the order its source lists
/// them; [`trove_next_prime`] searches them sorted.
const TROVE_PRIMES: [i32; 245] = [
    i32::MAX,
    5, 11, 23, 47, 97, 197, 397, 797, 1597, 3203, 6421, 12853, 25717, 51437, 102877, 205759,
    411527, 823117, 1646237, 3292489, 6584983, 13169977, 26339969, 52679969, 105359939,
    210719881, 421439783, 842879579, 1685759167,
    433, 877, 1759, 3527, 7057, 14143, 28289, 56591, 113189, 226379, 452759, 905551, 1811107,
    3622219, 7244441, 14488931, 28977863, 57955739, 115911563, 231823147, 463646329, 927292699,
    1854585413,
    953, 1907, 3821, 7643, 15287, 30577, 61169, 122347, 244703, 489407, 978821, 1957651, 3915341,
    7830701, 15661423, 31322867, 62645741, 125291483, 250582987, 501165979, 1002331963,
    2004663929,
    1039, 2081, 4177, 8363, 16729, 33461, 66923, 133853, 267713, 535481, 1070981, 2141977, 4283963,
    8567929, 17135863, 34271747, 68543509, 137087021, 274174111, 548348231, 1096696463,
    31, 67, 137, 277, 557, 1117, 2237, 4481, 8963, 17929, 35863, 71741, 143483, 286973, 573953,
    1147921, 2295859, 4591721, 9183457, 18366923, 36733847, 73467739, 146935499, 293871013,
    587742049, 1175484103,
    599, 1201, 2411, 4831, 9677, 19373, 38747, 77509, 155027, 310081, 620171, 1240361, 2480729,
    4961459, 9922933, 19845871, 39691759, 79383533, 158767069, 317534141, 635068283, 1270136683,
    311, 631, 1277, 2557, 5119, 10243, 20507, 41017, 82037, 164089, 328213, 656429, 1312867,
    2625761, 5251529, 10503061, 21006137, 42012281, 84024581, 168049163, 336098327, 672196673,
    1344393353,
    3, 7, 17, 37, 79, 163, 331, 673, 1361, 2729, 5471, 10949, 21911, 43853, 87719, 175447, 350899,
    701819, 1403641, 2807303, 5614657, 11229331, 22458671, 44917381, 89834777, 179669557,
    359339171, 718678369, 1437356741,
    43, 89, 179, 359, 719, 1439, 2879, 5779, 11579, 23159, 46327, 92657, 185323, 370661, 741337,
    1482707, 2965421, 5930887, 11861791, 23723597, 47447201, 94894427, 189788857, 379577741,
    759155483, 1518310967,
    379, 761, 1523, 3049, 6101, 12203, 24407, 48817, 97649, 195311, 390647, 781301, 1562611,
    3125257, 6250537, 12501169, 25002389, 50004791, 100009607, 200019221, 400038451, 800076929,
    1600153859,
];

/// The smallest Trove capacity at least `desired`.
fn trove_next_prime(desired: i64) -> usize {
    static SORTED: std::sync::OnceLock<Vec<i32>> = std::sync::OnceLock::new();
    let sorted = SORTED.get_or_init(|| {
        let mut v = TROVE_PRIMES.to_vec();
        v.sort_unstable();
        v
    });
    let i = sorted.partition_point(|&p| (p as i64) < desired);
    sorted[i.min(sorted.len() - 1)] as usize
}

/// The iteration order of a Trove 3 `THashSet` filled by one `addAll` per batch,
/// each batch's elements in the order given as `(hash, key)`; an element equal
/// to one already held is not added again.
///
/// The table is open-addressed over a prime capacity with load factor 0.5,
/// starting from room for ten. Before a batch it grows, if the batch could
/// overfill it, to the prime for `(batch + held) / 0.5 + 1`; past the load
/// factor it grows to the prime for twice its capacity. Growing reinserts from
/// the last slot down. A collision probes backwards by `1 + hash % (capacity -
/// 2)`, and iteration runs from the last slot to the first.
pub fn trove_set_order<K: Clone + PartialEq>(batches: &[Vec<(i32, K)>]) -> Vec<K> {
    // Where `key` lands in `slots`, or `None` when an equal key is held.
    fn insert<K: PartialEq>(slots: &mut [Option<(i32, K)>], hash: i32, key: K) -> Option<usize> {
        let h = (hash & 0x7fff_ffff) as usize;
        let len = slots.len();
        let mut index = h % len;
        let probe = 1 + h % (len - 2);
        loop {
            match &slots[index] {
                None => {
                    slots[index] = Some((hash, key));
                    return Some(index);
                }
                Some((_, held)) if *held == key => return None,
                Some(_) => {
                    index = if index >= probe { index - probe } else { index + len - probe };
                }
            }
        }
    }
    fn rehash<K: PartialEq>(slots: &mut Vec<Option<(i32, K)>>, capacity: usize) {
        let old = std::mem::replace(slots, (0..capacity).map(|_| None).collect());
        for (hash, key) in old.into_iter().rev().flatten() {
            insert(slots, hash, key);
        }
    }
    // `min(capacity - 1, ⌊capacity × 0.5⌋)` elements before the table grows.
    let max_size = |capacity: usize| (capacity - 1).min(capacity / 2);

    let mut slots: Vec<Option<(i32, K)>> = (0..trove_next_prime(20)).map(|_| None).collect();
    let mut size = 0usize;
    for batch in batches {
        if batch.len() + size > max_size(slots.len()) {
            let want = (size as i64 + 1).max(2 * (batch.len() + size) as i64 + 1);
            rehash(&mut slots, trove_next_prime(want));
        }
        for (hash, key) in batch {
            if insert(&mut slots, *hash, key.clone()).is_some() {
                size += 1;
                if size > max_size(slots.len()) {
                    let doubled = trove_next_prime(2 * slots.len() as i64);
                    rehash(&mut slots, doubled);
                }
            }
        }
    }
    slots.into_iter().rev().flatten().map(|(_, k)| k).collect()
}

/// The capacity a `java.util.HashSet` ends at after inserting `n` elements
/// one-by-one into a default-sized table (16 slots, load factor 0.75, doubling
/// whenever the count exceeds the threshold).
pub fn java_hashset_capacity(n: usize) -> usize {
    let mut cap = 16usize;
    while n > cap * 3 / 4 {
        cap *= 2;
    }
    cap
}

/// The iteration order of a `java.util.HashSet` holding elements with these
/// hash codes: ascending bucket index (Java's spread `h ^ (h >>> 16)` masked by
/// capacity), input order within a bucket. Returns indices into `hashes`.
///
/// `total` is the number of elements the SET holds — pass it when the caller
/// orders a filtered subset of a larger set: the bucket mask comes from the
/// full set's capacity, and a mask from the subset count reorders exactly the
/// pairs whose buckets differ only above it.
pub fn hashset_order_of(hashes: &[i32], total: usize) -> Vec<usize> {
    let cap = java_hashset_capacity(total) as u32;
    let mut idx: Vec<usize> = (0..hashes.len()).collect();
    idx.sort_by_key(|&i| {
        let h = hashes[i] as u32;
        (h ^ (h >> 16)) & (cap - 1)
    });
    idx
}

pub fn hashset_order(hashes: &[i32]) -> Vec<usize> {
    hashset_order_of(hashes, hashes.len())
}

/// The iteration order of one subject's annotation assertions. The subject's
/// set is filled from the ontology's set of every annotation assertion, so
/// two of its members in one bucket stand in the order that larger set holds
/// them: `subject_total` sizes the subject's set, `ontology_total` the
/// ontology's. Returns indices into `hashes`.
pub fn subject_assertion_order(hashes: &[i32], subject_total: usize, ontology_total: usize) -> Vec<usize> {
    let sub_cap = java_hashset_capacity(subject_total) as u32;
    let all_cap = java_hashset_capacity(ontology_total) as u32;
    let bucket = |h: i32, cap: u32| {
        let h = h as u32;
        (h ^ (h >> 16)) & (cap - 1)
    };
    let mut idx: Vec<usize> = (0..hashes.len()).collect();
    idx.sort_by_key(|&i| (bucket(hashes[i], sub_cap), bucket(hashes[i], all_cap)));
    idx
}

/// The hash of an ontology's identity: `17 + 37·present(iri) [+ 37·present(version)]`,
/// where `present(x)` wraps an IRI hash the way an occupied optional does
/// (`0x598df91c + hash`). A document with no ontology IRI contributes the bare
/// seed, so anonymous documents tie and keep their input order.
pub fn ontology_id_hash(iri: Option<&str>, version: Option<&str>) -> i32 {
    const PRESENT: i32 = 0x598df91cu32 as i32;
    let mut h: i32 = 17;
    if let Some(i) = iri {
        h = h.wrapping_add(37i32.wrapping_mul(PRESENT.wrapping_add(iri_hash(i))));
    }
    if let Some(v) = version {
        h = h.wrapping_add(37i32.wrapping_mul(PRESENT.wrapping_add(iri_hash(v))));
    }
    h
}

/// The order a set of loaded documents is consulted in when a banner label is
/// looked up across all of them: a default-sized table over the documents'
/// [`ontology_id_hash`]es, same-bucket ties separated by the doubled and
/// re-doubled tables, and finally input order. The pipeline input document sits
/// wherever its identity hashes — a version IRI carrying the release date moves
/// it, so the pick an entity's banner gets is a function of the run's date.
pub fn ontology_set_order(hashes: &[i32]) -> Vec<usize> {
    let base = java_hashset_capacity(hashes.len()) as u32;
    let mut idx: Vec<usize> = (0..hashes.len()).collect();
    idx.sort_by_key(|&i| {
        let h = hashes[i] as u32;
        let s = h ^ (h >> 16);
        (s & (base - 1), s & (base * 2 - 1), s & (base * 4 - 1))
    });
    idx
}

/// The iteration order of a reasoner NODE's member classes: a size-derived
/// table (capacity for `⌊n/0.75⌋+1`, no 16-slot floor) over the classes'
/// content hashes, with same-bucket ties resolved by the DEFAULT-sized table
/// the members passed through on their way in. Verified against 170/170
/// unambiguous real cliques; the residual tie (same bucket in BOTH tables)
/// falls back to IRI order.
/// The hash of a named class, as a set of classes keys it.
pub fn class_hash(iri: &str) -> i32 {
    tag(P_CLASS, &[iri_hash(iri)])
}

pub fn class_node_order(iris: &[String]) -> Vec<usize> {
    let mut cap = 1usize;
    let want = iris.len() * 4 / 3 + 1;
    while cap < want {
        cap <<= 1;
    }
    let mut idx: Vec<usize> = (0..iris.len()).collect();
    let keys: Vec<(u32, u32, &String)> = iris
        .iter()
        .map(|iri| {
            let h = tag(P_CLASS, &[iri_hash(iri)]) as u32;
            let s = h ^ (h >> 16);
            (s & (cap as u32 - 1), s & 15, iri)
        })
        .collect();
    idx.sort_by(|&a, &b| keys[a].cmp(&keys[b]));
    idx
}

#[cfg(test)]
mod tests {

    /// Every logical axiom kind and declaration hashes exactly as OWLAPI 4.5.29
    /// hashes it: the fixture's expected values are `OWLAxiom.hashCode()` as
    /// printed by OWLAPI over the same document. The Turtle copy holds the same
    /// axioms with its untyped literals typed `xsd:string`, which changes the
    /// order its annotation sets and literal sets are hashed in.
    #[test]
    fn axiom_hashes_are_owlapis() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/owlapi-hash");
        for (doc, hashes) in [("axioms.ofn", "axioms.java-hashes"), ("axioms.ttl", "axioms.ttl.java-hashes")] {
            let model = crate::io::load(&dir.join(doc)).unwrap();
            let order = model.natural_order();
            let mut ours: Vec<(i32, String)> = model
                .ont
                .iter()
                .filter_map(|ac| axiom_hash(&ac.component, &ac.ann, order).map(|h| (h, format!("{:?}", ac.component))))
                .collect();
            ours.sort();
            let want: Vec<i32> =
                std::fs::read_to_string(dir.join(hashes)).unwrap().lines().map(|l| l.trim().parse().unwrap()).collect();
            let missing: Vec<&i32> = want.iter().filter(|h| !ours.iter().any(|(o, _)| o == *h)).collect();
            let extra: Vec<&(i32, String)> = ours.iter().filter(|(o, _)| !want.contains(o)).collect();
            assert!(
                missing.is_empty() && extra.is_empty() && ours.len() == want.len(),
                "{doc}: OWLAPI hashes with no match: {missing:?}\nours with no match:\n{}",
                extra.iter().map(|(h, d)| format!("  {h}  {}", &d[..d.len().min(160)])).collect::<Vec<_>>().join("\n")
            );
        }
    }

    use super::*;
    use horned_owl::model::Build;

    /// Ground truth: `OWLLiteral.hashCode()` read off the OWLAPI runtime, minus
    /// the `(277*37 + datatype.hash)*37` base, for each datatype that reads its
    /// lexical form as a number.
    #[test]
    fn literal_payload_matches_owlapi() {
        let xsd = "http://www.w3.org/2001/XMLSchema#";
        let int = format!("{xsd}integer");
        assert_eq!(literal_payload_hash("20", &int), 20 * 65536);
        assert_eq!(literal_payload_hash("0", &int), 0);
        assert_eq!(literal_payload_hash("-7", &int), -7 * 65536);
        // A leading zero is not the canonical form, but the datatype still reads
        // the number out of it.
        assert_eq!(literal_payload_hash("007", &int), 7 * 65536);
        // …and one too wide for 32 bits falls back to the string hash.
        assert_eq!(
            literal_payload_hash("99999999999999999999", &int),
            java_string_hash("99999999999999999999").wrapping_mul(65536)
        );
        assert_eq!(literal_payload_hash("true", &format!("{xsd}boolean")), 65536);
        assert_eq!(literal_payload_hash("false", &format!("{xsd}boolean")), 0);
        assert_eq!(literal_payload_hash("2.5", &format!("{xsd}double")), 163840);
        assert_eq!(literal_payload_hash("2.5", &format!("{xsd}float")), 163840);
        // Every other datatype hashes the lexical form.
        assert_eq!(
            literal_payload_hash("306.764", &format!("{xsd}decimal")),
            java_string_hash("306.764").wrapping_mul(65536)
        );
    }

    // Ground truth from the OWLAPI 4.5.29 runtime (HashProbe/PartProbe).
    #[test]
    fn axiom_hashes_match_owlapi() {
        let b: Build<RcStr> = Build::new();
        let obo = "http://purl.obolibrary.org/obo/";
        let cls = |n: &str| CE::Class(b.class(format!("{obo}{n}")));
        let some = |p: &str, n: &str| CE::ObjectSomeValuesFrom {
            ope: OPE::ObjectProperty(b.object_property(format!("{obo}{p}"))),
            bce: Box::new(cls(n)),
        };
        // Component ground truth.
        assert_eq!(iri_hash("http://purl.obolibrary.org/obo/CL_0002438"), -267643151);
        assert_eq!(ce_hash(&cls("CL_0002438"), NaturalOrder::default()), -267572068);
        assert_eq!(ce_hash(&some("RO_0002162", "NCBITaxon_10090"), NaturalOrder::default()), 1527442063);
        assert_eq!(
            ce_hash(&CE::ObjectIntersectionOf(vec![
                cls("CL_0000824"),
                some("RO_0002104", "PR_000002977"),
                some("RO_0002162", "NCBITaxon_10090")
            ]), NaturalOrder::default()),
            1075431256
        );
        let anns = std::collections::BTreeSet::new();
        let cases: Vec<(Vec<CE<RcStr>>, i32)> = vec![
            (
                vec![cls("CL_0002438"), CE::ObjectIntersectionOf(vec![
                    cls("CL_0000824"), some("RO_0002104", "PR_000002977"), some("RO_0002162", "NCBITaxon_10090")])],
                -459279858,
            ),
            (
                vec![cls("CL_4030100"), CE::ObjectIntersectionOf(vec![
                    cls("CL_0002438"), some("RO_0002104", "PR_000001402"), some("RO_0002162", "NCBITaxon_10090")])],
                836328048,
            ),
            (
                vec![cls("CL_4030101"), CE::ObjectIntersectionOf(vec![
                    cls("CL_0000824"), some("RO_0002162", "NCBITaxon_10090")])],
                846363906,
            ),
            (
                vec![cls("CL_4030102"), CE::ObjectIntersectionOf(vec![
                    cls("CL_4030100"), some("RO_0002162", "NCBITaxon_10090")])],
                567497281,
            ),
            (
                vec![cls("CL_4030103"), CE::ObjectIntersectionOf(vec![
                    cls("CL_0002438"), some("RO_0002104", "PR_000002977"), some("RO_0002162", "NCBITaxon_10090")])],
                869791029,
            ),
        ];
        for (members, want) in &cases {
            assert_eq!(equivalent_classes_hash(members, &anns, NaturalOrder::default()), *want);
        }
        // The runtime-observed iteration skeleton: buckets 0,1,2,2,9 (cap 16).
        let hashes: Vec<i32> = cases.iter().map(|(m, _)| equivalent_classes_hash(m, &anns, NaturalOrder::default())).collect();
        let order = hashset_order(&hashes);
        assert_eq!(order[0], 2, "CL_4030101 first (bucket 0)");
        assert_eq!(order[1], 0, "CL_0002438 second (bucket 1)");
        assert_eq!(order[4], 1, "CL_4030100 last (bucket 9)");
    }

    /// Ground truth from the OWLAPI 4.5.29 runtime: `hashCode()` of each
    /// expression, built with `OWLDataFactory`.
    #[test]
    fn individual_hashes_match_owlapi() {
        let b: Build<RcStr> = Build::new();
        let t = "http://example.org/t#";
        let p = || OPE::ObjectProperty(b.object_property(format!("{t}p")));
        let named = |n: &str| Individual::Named(b.named_individual(format!("{t}{n}")));
        let value = |i: Individual<RcStr>| CE::ObjectHasValue { ope: p(), i };
        assert_eq!(ce_hash(&value(named("a1")), NaturalOrder::default()), 2015939837);
        assert_eq!(ce_hash(&value(named("a23")), NaturalOrder::default()), 2016031599);
        // A one-of's individuals hash as a set: the order they are given in and
        // a repeated member change nothing.
        assert_eq!(ce_hash(&CE::ObjectOneOf(vec![named("a1"), named("a2")]), NaturalOrder::default()), -1216281020);
        assert_eq!(ce_hash(&CE::ObjectOneOf(vec![named("a2"), named("a1"), named("a2")]), NaturalOrder::default()), -1216281020);
        // An anonymous individual hashes as its node id, however the id is spelled.
        let anon = |id: &str| Individual::Anonymous(horned_owl::model::AnonymousIndividual(RcStr::from(id)));
        assert_eq!(ce_hash(&value(anon("_:genid2147483648")), NaturalOrder::default()), 575983105);
        assert_eq!(ce_hash(&value(anon("genid2147483648")), NaturalOrder::default()), 575983105);
    }
}
