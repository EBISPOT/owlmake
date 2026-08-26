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
//!   except axiom annotation sets, which hash to 0 when empty. IRIs hash as
//!   the sum of the Java string hashes of their namespace and NCName-suffix
//!   halves;
//! - the component order (`owl_cmp`) — OWLAPI's `compareTo`: type index first,
//!   then per-type field comparison, with IRIs compared namespace-then-suffix;
//! - the bucket order (`hashset_order`) — Java's HashMap spread
//!   (`h ^ (h >>> 16)`) masked by the table capacity that results from
//!   inserting `n` elements into a default-sized table. Entries in the SAME
//!   bucket have no defined relative order (the container feeding the set
//!   iterates in randomized order), so callers keep their own tie order;
//!   distinct buckets — the overwhelmingly common case — are fully determined.

use std::cmp::Ordering;

use horned_owl::model::{
    Annotation, AnnotationValue, ClassExpression as CE, Individual, Literal,
    ObjectPropertyExpression as OPE, RcStr,
};

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
const P_DATATYPE: i32 = 3911;
const P_OBJECT_PROPERTY: i32 = 4153;
const P_OBJ_INVERSE: i32 = 4241;
const P_NAMED_INDIVIDUAL: i32 = 4663;
/// An `AnnotationProperty` is hashed the same way wherever it appears — as an
/// assertion's property, and as the property of one of the assertion's own
/// annotations.
const P_ANNOTATION_PROPERTY: i32 = 6067;
const P_ANNOTATION_PROPERTY_ENTITY: i32 = P_ANNOTATION_PROPERTY;
const P_ANNOTATION: i32 = 6311;
const P_ANNOTATION_ASSERTION: i32 = 739;

// The type index OWLAPI's compareTo consults before any field comparison.
fn type_index(ce: &CE<RcStr>) -> i32 {
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
        _ => 3999,
    }
}

/// Java `String.hashCode()` — over UTF-16 code units, wrapping i32.
pub fn java_string_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

/// Whether a code point may START an NCName (XML name start minus ':').
fn is_ncname_start(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z' | '_'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// Whether a code point may CONTINUE an NCName.
fn is_ncname_char(c: char) -> bool {
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

fn iri_split(iri: &str) -> (&str, &str) {
    match ncname_suffix_index(iri) {
        Some(i) => (&iri[..i], &iri[i..]),
        None => (iri, ""),
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

fn ope_cmp(a: &OPE<RcStr>, b: &OPE<RcStr>) -> Ordering {
    let idx = |o: &OPE<RcStr>| match o {
        OPE::ObjectProperty(_) => 1002,
        OPE::InverseObjectProperty(_) => 1003,
    };
    idx(a).cmp(&idx(b)).then_with(|| match (a, b) {
        (OPE::ObjectProperty(x), OPE::ObjectProperty(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
        (OPE::InverseObjectProperty(x), OPE::InverseObjectProperty(y)) => {
            iri_cmp(x.0.as_ref(), y.0.as_ref())
        }
        _ => Ordering::Equal,
    })
}

fn ind_cmp(a: &Individual<RcStr>, b: &Individual<RcStr>) -> Ordering {
    match (a, b) {
        (Individual::Named(x), Individual::Named(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
        (Individual::Named(_), Individual::Anonymous(_)) => Ordering::Less,
        (Individual::Anonymous(_), Individual::Named(_)) => Ordering::Greater,
        (Individual::Anonymous(x), Individual::Anonymous(y)) => x.0.as_ref().cmp(y.0.as_ref()),
    }
}

/// The distinct members of a set-valued component, in OWLAPI order — how the
/// axiom/expression stores them.
fn sorted_distinct<'a>(v: &'a [CE<RcStr>]) -> Vec<&'a CE<RcStr>> {
    let mut out: Vec<&CE<RcStr>> = Vec::with_capacity(v.len());
    for c in v {
        if !out.iter().any(|x| **x == *c) {
            out.push(c);
        }
    }
    out.sort_by(|a, b| owl_cmp(a, b));
    out
}

/// OWLAPI `OWLObject.compareTo` over class expressions: type index, then the
/// per-type field comparison.
pub fn owl_cmp(a: &CE<RcStr>, b: &CE<RcStr>) -> Ordering {
    let d = type_index(a).cmp(&type_index(b));
    if d != Ordering::Equal {
        return d;
    }
    match (a, b) {
        (CE::Class(x), CE::Class(y)) => iri_cmp(x.0.as_ref(), y.0.as_ref()),
        (
            CE::ObjectSomeValuesFrom { ope: pa, bce: fa },
            CE::ObjectSomeValuesFrom { ope: pb, bce: fb },
        )
        | (
            CE::ObjectAllValuesFrom { ope: pa, bce: fa },
            CE::ObjectAllValuesFrom { ope: pb, bce: fb },
        ) => ope_cmp(pa, pb).then_with(|| owl_cmp(fa, fb)),
        (CE::ObjectIntersectionOf(va), CE::ObjectIntersectionOf(vb))
        | (CE::ObjectUnionOf(va), CE::ObjectUnionOf(vb)) => {
            // compareSets: element-wise over the sorted sets, then size.
            let sa = sorted_distinct(va);
            let sb = sorted_distinct(vb);
            for (x, y) in sa.iter().zip(sb.iter()) {
                let d = owl_cmp(x, y);
                if d != Ordering::Equal {
                    return d;
                }
            }
            sa.len().cmp(&sb.len())
        }
        (CE::ObjectComplementOf(x), CE::ObjectComplementOf(y)) => owl_cmp(x, y),
        (CE::ObjectHasSelf(x), CE::ObjectHasSelf(y)) => ope_cmp(x, y),
        (CE::ObjectHasValue { ope: pa, i: ia }, CE::ObjectHasValue { ope: pb, i: ib }) => {
            ope_cmp(pa, pb).then_with(|| ind_cmp(ia, ib))
        }
        (
            CE::ObjectMinCardinality { n: na, ope: pa, bce: fa },
            CE::ObjectMinCardinality { n: nb, ope: pb, bce: fb },
        )
        | (
            CE::ObjectMaxCardinality { n: na, ope: pa, bce: fa },
            CE::ObjectMaxCardinality { n: nb, ope: pb, bce: fb },
        )
        | (
            CE::ObjectExactCardinality { n: na, ope: pa, bce: fa },
            CE::ObjectExactCardinality { n: nb, ope: pb, bce: fb },
        ) => ope_cmp(pa, pb).then_with(|| na.cmp(nb)).then_with(|| owl_cmp(fa, fb)),
        (CE::ObjectOneOf(va), CE::ObjectOneOf(vb)) => {
            let mut sa: Vec<&Individual<RcStr>> = va.iter().collect();
            let mut sb: Vec<&Individual<RcStr>> = vb.iter().collect();
            sa.sort_by(|x, y| ind_cmp(x, y));
            sb.sort_by(|x, y| ind_cmp(x, y));
            for (x, y) in sa.iter().zip(sb.iter()) {
                let d = ind_cmp(x, y);
                if d != Ordering::Equal {
                    return d;
                }
            }
            sa.len().cmp(&sb.len())
        }
        _ => Ordering::Equal,
    }
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

fn ind_hash(i: &Individual<RcStr>) -> i32 {
    match i {
        Individual::Named(n) => tag(P_NAMED_INDIVIDUAL, &[iri_hash(n.0.as_ref())]),
        Individual::Anonymous(_) => 0,
    }
}

pub fn ce_hash(ce: &CE<RcStr>) -> i32 {
    match ce {
        CE::Class(c) => tag(P_CLASS, &[iri_hash(c.0.as_ref())]),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            tag(P_OBJ_SOME_VALUES, &[ope_hash(ope), ce_hash(bce)])
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            tag(P_OBJ_ALL_VALUES, &[ope_hash(ope), ce_hash(bce)])
        }
        CE::ObjectIntersectionOf(v) => {
            let hs: Vec<i32> = sorted_distinct(v).iter().map(|c| ce_hash(c)).collect();
            tag(P_OBJ_INTERSECTION, &[list_hash(&hs)])
        }
        CE::ObjectUnionOf(v) => {
            let hs: Vec<i32> = sorted_distinct(v).iter().map(|c| ce_hash(c)).collect();
            tag(P_OBJ_UNION, &[list_hash(&hs)])
        }
        CE::ObjectComplementOf(b) => tag(P_OBJ_COMPLEMENT, &[ce_hash(b)]),
        CE::ObjectExactCardinality { n, ope, bce } => {
            tag(P_OBJ_EXACT_CARD, &[ope_hash(ope), *n as i32, ce_hash(bce)])
        }
        CE::ObjectMinCardinality { n, ope, bce } => {
            tag(P_OBJ_MIN_CARD, &[ope_hash(ope), *n as i32, ce_hash(bce)])
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            tag(P_OBJ_MAX_CARD, &[ope_hash(ope), *n as i32, ce_hash(bce)])
        }
        CE::ObjectHasValue { ope, i } => tag(P_OBJ_HAS_VALUE, &[ope_hash(ope), ind_hash(i)]),
        CE::ObjectHasSelf(ope) => tag(P_OBJ_HAS_SELF, &[ope_hash(ope)]),
        CE::ObjectOneOf(v) => {
            let mut inds: Vec<&Individual<RcStr>> = v.iter().collect();
            inds.sort_by(|x, y| ind_cmp(x, y));
            inds.dedup_by(|x, y| ind_cmp(x, y) == Ordering::Equal);
            let hs: Vec<i32> = inds.iter().map(|i| ind_hash(i)).collect();
            tag(P_OBJ_ONE_OF, &[list_hash(&hs)])
        }
        // Data ranges do not appear in the axioms these orders decide.
        _ => 0,
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
    const PLAIN: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
    let (text, dt): (&str, &str) = match lit {
        Literal::Simple { literal } => (literal, PLAIN),
        Literal::Language { literal, .. } => (literal, PLAIN),
        Literal::Datatype { literal, datatype_iri } => {
            let d = datatype_iri.as_ref();
            if d == "http://www.w3.org/2001/XMLSchema#string" || d == PLAIN {
                (literal, PLAIN)
            } else {
                (literal, d)
            }
        }
    };
    let dt_hash = tag(P_DATATYPE, &[iri_hash(dt)]);
    let payload = literal_payload_hash(text, dt);
    let h = (277i32.wrapping_mul(37).wrapping_add(dt_hash)).wrapping_mul(37).wrapping_add(payload);
    // A language tag is folded in after the payload.
    match lit {
        Literal::Language { lang, .. } => {
            h.wrapping_mul(37).wrapping_add(java_string_hash(lang.as_ref()))
        }
        _ => h,
    }
}

fn annotation_value_hash(av: &AnnotationValue<RcStr>) -> i32 {
    match av {
        AnnotationValue::IRI(iri) => iri_hash(iri.as_ref()),
        AnnotationValue::Literal(l) => literal_hash(l),
        AnnotationValue::AnonymousIndividual(_) => 0,
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
) -> i32 {
    let ann_hash = if anns.is_empty() {
        0
    } else {
        let mut sorted: Vec<&Annotation<RcStr>> = anns.iter().collect();
        sorted.sort_by(|a, b| annotation_cmp(a, b));
        list_hash(&sorted.iter().map(|a| annotation_hash(a)).collect::<Vec<i32>>())
    };
    tag(
        P_ANNOTATION_ASSERTION,
        &[
            iri_hash(subject),
            tag(P_ANNOTATION_PROPERTY_ENTITY, &[iri_hash(property)]),
            annotation_value_hash(value),
            ann_hash,
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

fn annotation_cmp(a: &Annotation<RcStr>, b: &Annotation<RcStr>) -> Ordering {
    iri_cmp(a.ap.0.as_ref(), b.ap.0.as_ref()).then_with(|| {
        let kind = |v: &AnnotationValue<RcStr>| match v {
            AnnotationValue::IRI(_) => 0,
            AnnotationValue::AnonymousIndividual(_) => 1,
            AnnotationValue::Literal(_) => 2,
        };
        kind(&a.av).cmp(&kind(&b.av)).then_with(|| match (&a.av, &b.av) {
            (AnnotationValue::IRI(x), AnnotationValue::IRI(y)) => {
                iri_cmp(x.as_ref(), y.as_ref())
            }
            (AnnotationValue::Literal(x), AnnotationValue::Literal(y)) => {
                let t = |l: &Literal<RcStr>| match l {
                    Literal::Simple { literal }
                    | Literal::Language { literal, .. }
                    | Literal::Datatype { literal, .. } => literal.clone(),
                };
                t(x).cmp(&t(y))
            }
            _ => Ordering::Equal,
        })
    })
}

/// `OWLEquivalentClassesAxiom.hashCode()`: the prime tag over the sorted
/// distinct member list hash and the annotation hash (0 when unannotated,
/// sorted list hash otherwise).
pub fn equivalent_classes_hash(
    members: &[CE<RcStr>],
    anns: &std::collections::BTreeSet<Annotation<RcStr>>,
) -> i32 {
    let member_hashes: Vec<i32> = sorted_distinct(members).iter().map(|c| ce_hash(c)).collect();
    let ann_hash = if anns.is_empty() {
        0
    } else {
        let mut sorted: Vec<&Annotation<RcStr>> = anns.iter().collect();
        sorted.sort_by(|a, b| annotation_cmp(a, b));
        let hs: Vec<i32> = sorted.iter().map(|a| annotation_hash(a)).collect();
        list_hash(&hs)
    };
    tag(P_EQUIVALENT_CLASSES, &[list_hash(&member_hashes), ann_hash])
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

/// `OWLOntologyID.hashCode()` for an ontology named by `iri` and carrying no
/// version IRI: `17 + 37 * (0x598df91c + iriHash)` — the inner term is the
/// guava `Optional.Present` wrapper's hash around the IRI's own.
pub fn ontology_id_hash(iri: &str) -> i32 {
    17i32.wrapping_add(37i32.wrapping_mul(0x598d_f91c_i32.wrapping_add(iri_hash(iri))))
}

/// The table capacity a `java.util.concurrent.ConcurrentHashMap` ends at after
/// `n` insertions: 16 slots, doubling whenever the count REACHES the 0.75
/// threshold — one insertion earlier than `java.util.HashSet`, whose growth
/// [`java_hashset_capacity`] models.
fn java_chm_capacity(n: usize) -> usize {
    let mut cap = 16usize;
    while n >= cap * 3 / 4 {
        cap *= 2;
    }
    cap
}

/// The order in which a functional-syntax label lookup visits the ontologies of
/// a loaded import closure, deciding which document's `rdfs:label` banners an
/// entity when several assert one.
///
/// The lookup walks the ontology MANAGER's set — a `java.util.HashSet` copied
/// out of the manager's registration map (a `ConcurrentHashMap`) — and, for
/// each ontology, that ontology's own imports closure as a `TreeSet` ordered by
/// `OntologyID.toString()`. The first ontology visited that asserts a label for
/// the entity names it. Three orders therefore compose:
///
///   1. registration order — the root first, then each import as it is loaded,
///      depth-first in declaration order (`creation`);
///   2. the `ConcurrentHashMap` values order — ascending table bin over the
///      ontology-ID hashes, registration order within a bin;
///   3. the `HashSet` iteration — ascending bin under ITS capacity, map-values
///      order within a bin.
///
/// The composite walks (3); a named import contributes itself, and the root —
/// whose closure is the whole set — flushes everything not yet visited in
/// `TreeSet` order. `OntologyID.toString()` wraps the IRI as
/// `OntologyID(OntologyIRI(<iri>) VersionIRI(<null>))`, so an ontology sorts
/// AFTER the ontologies whose IRIs extend its own (`>` follows `/`), which
/// places a typical root behind its imports.
///
/// `iris` are the ontology IRIs in registration order (root first); `direct[i]`
/// are the indices each document imports, in declaration order. Returns visit
/// order as indices into `iris`. Verified against ROBOT 1.9.7 (OWLAPI 4.5.29)
/// over the 24-ontology EFO closure: 276 pairwise label probes reproduce all
/// 24 positions.
pub fn ontology_visit_order(iris: &[String], direct: &[Vec<usize>]) -> Vec<usize> {
    let n = iris.len();
    if n == 0 {
        return Vec::new();
    }
    let spread: Vec<u32> = iris
        .iter()
        .map(|iri| {
            let h = ontology_id_hash(iri) as u32;
            h ^ (h >> 16)
        })
        .collect();

    let chm_mask = java_chm_capacity(n) as u32 - 1;
    let mut map_order: Vec<usize> = (0..n).collect();
    map_order.sort_by_key(|&i| spread[i] & chm_mask);

    let hs_mask = java_hashset_capacity(n) as u32 - 1;
    let mut set_order = map_order;
    set_order.sort_by_key(|&i| spread[i] & hs_mask);

    // Reflexive transitive closure per ontology, then each in TreeSet order.
    let tree_key = |i: usize| format!("OntologyID(OntologyIRI(<{}>) VersionIRI(<null>))", iris[i]);
    let mut visited = vec![false; n];
    let mut out = Vec::with_capacity(n);
    for &i in &set_order {
        let mut closure = Vec::new();
        let mut seen = vec![false; n];
        let mut stack = vec![i];
        while let Some(j) = stack.pop() {
            if seen[j] {
                continue;
            }
            seen[j] = true;
            closure.push(j);
            stack.extend(direct[j].iter().copied());
        }
        closure.sort_by_key(|&j| tree_key(j));
        for j in closure {
            if !visited[j] {
                visited[j] = true;
                out.push(j);
            }
        }
    }
    out
}

/// The iteration order of a reasoner NODE's member classes: a size-derived
/// table (capacity for `⌊n/0.75⌋+1`, no 16-slot floor) over the classes'
/// content hashes, with same-bucket ties resolved by the DEFAULT-sized table
/// the members passed through on their way in. Verified against 170/170
/// unambiguous real cliques; the residual tie (same bucket in BOTH tables)
/// falls back to IRI order.
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
        assert_eq!(ce_hash(&cls("CL_0002438")), -267572068);
        assert_eq!(ce_hash(&some("RO_0002162", "NCBITaxon_10090")), 1527442063);
        assert_eq!(
            ce_hash(&CE::ObjectIntersectionOf(vec![
                cls("CL_0000824"),
                some("RO_0002104", "PR_000002977"),
                some("RO_0002162", "NCBITaxon_10090")
            ])),
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
            assert_eq!(equivalent_classes_hash(members, &anns), *want);
        }
        // The runtime-observed iteration skeleton: buckets 0,1,2,2,9 (cap 16).
        let hashes: Vec<i32> = cases.iter().map(|(m, _)| equivalent_classes_hash(m, &anns)).collect();
        let order = hashset_order(&hashes);
        assert_eq!(order[0], 2, "CL_4030101 first (bucket 0)");
        assert_eq!(order[1], 0, "CL_0002438 second (bucket 1)");
        assert_eq!(order[4], 1, "CL_4030100 last (bucket 9)");
    }

    /// Ground truth: ROBOT 1.9.7 (OWLAPI 4.5.29) over EFO's 24-ontology
    /// closure — 276 pairwise-labelled probe classes, whose functional-syntax
    /// banners read out the full visit order. The prefix is HashSet iteration
    /// up to the root; the tail is the root's TreeSet closure flush, with the
    /// root itself last (its `OntologyID(…)` string sorts after every IRI that
    /// extends its own).
    #[test]
    fn ontology_visit_order_matches_owlapi() {
        let base = "http://www.ebi.ac.uk/efo";
        let names = [
            "components/anatomagram_kidney.owl",
            "components/anatomagram_liver.owl",
            "components/anatomagram_lung.owl",
            "components/anatomagram_pancreas.owl",
            "components/anatomagram_placenta.owl",
            "components/efo_equivalent_class_axioms.owl",
            "components/gwas_import.owl",
            "components/import_replaced_by.owl",
            "components/subclasses.owl",
            "imports/chebi_import.owl",
            "imports/cl_import.owl",
            "imports/ecto_import.owl",
            "imports/fbbi_import.owl",
            "imports/fbbt_import.owl",
            "imports/go_import.owl",
            "imports/gsso_import.owl",
            "imports/hancestro_import.owl",
            "imports/hp_import.owl",
            "imports/mondo_import.owl",
            "imports/oba_import.owl",
            "imports/obi_import.owl",
            "imports/pr_import.owl",
            "imports/uberon_import.owl",
        ];
        let mut iris = vec![base.to_string()];
        iris.extend(names.iter().map(|n| format!("{base}/{n}")));
        let mut direct = vec![(1..iris.len()).collect::<Vec<_>>()];
        direct.extend(std::iter::repeat_with(Vec::new).take(names.len()));

        let order = ontology_visit_order(&iris, &direct);
        let got: Vec<&str> = order
            .iter()
            .map(|&i| if i == 0 { "ROOT" } else { names[i - 1] })
            .collect();
        let want = [
            "imports/uberon_import.owl",
            "imports/gsso_import.owl",
            "components/anatomagram_kidney.owl",
            "components/anatomagram_lung.owl",
            "imports/cl_import.owl",
            "components/anatomagram_pancreas.owl",
            "imports/fbbi_import.owl",
            "imports/chebi_import.owl",
            "components/efo_equivalent_class_axioms.owl",
            "components/anatomagram_liver.owl",
            "components/anatomagram_placenta.owl",
            "components/gwas_import.owl",
            "components/import_replaced_by.owl",
            "components/subclasses.owl",
            "imports/ecto_import.owl",
            "imports/fbbt_import.owl",
            "imports/go_import.owl",
            "imports/hancestro_import.owl",
            "imports/hp_import.owl",
            "imports/mondo_import.owl",
            "imports/oba_import.owl",
            "imports/obi_import.owl",
            "imports/pr_import.owl",
            "ROOT",
        ];
        assert_eq!(got, want);
    }
}
