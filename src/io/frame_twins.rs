//! Where a class frame writes a value that two of its annotation assertions
//! both state, once as a plain literal and once as an `xsd:string` one.
//!
//! A class frame orders its axioms in two steps. They go one at a time into a
//! red-black search tree: the declaration, then the class's own axioms in hash-set
//! order, then its annotation assertions in hash-set order. The tree's listing is
//! then sorted ([`crate::util::tree_sort`]). The frame writes each axiom's triple
//! in that order, and a triple already written keeps its place. The two twin
//! assertions write the same triple, so it stands wherever the first of them
//! sorts.
//!
//! Every comparison is a consistent order except between the twins. Compared from
//! the plain side, the two values count as equal, and the pair falls back to its
//! axiom annotations. Compared from the `xsd:string` side, the datatypes differ
//! and the `xsd:string` copy is the greater. When the plain copy's annotations
//! also make it the greater, each twin claims to follow the other, and which
//! comes first depends on the tree's shape and the sort's merge path. A twin
//! compared equal to the other on its way into the tree does not enter it, and
//! is not written at all.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

use horned_owl::model::{
    Annotation, AnnotatedComponent, AnnotationSubject, AnnotationValue, ClassExpression as CE, Component,
    Literal, RcStr,
};

use crate::io::owlfunc::{cmp_annotation_value, cmp_ce, cmp_component};
use crate::model::Model;
use crate::owlapi_hash::{annotation_assertion_hash, axiom_hash, hashset_order_of, iri_cmp, subject_assertion_order};
use crate::util::tree_sort::{run_merge_sort, tree_set_order};

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// An annotation assertion's value as a twin sees it: its property, its text,
/// and whether it is the `xsd:string` copy. `None` for any other value.
fn twin_value(c: &Component<RcStr>) -> Option<(&str, &str, &str, bool)> {
    let Component::AnnotationAssertion(aa) = c else { return None };
    let AnnotationSubject::IRI(s) = &aa.subject else { return None };
    let AnnotationValue::Literal(lit) = &aa.ann.av else { return None };
    let (text, typed) = match lit {
        Literal::Simple { literal } => (literal.as_str(), false),
        Literal::Datatype { literal, datatype_iri } if datatype_iri.as_ref() == XSD_STRING => {
            (literal.as_str(), true)
        }
        _ => return None,
    };
    Some((s.as_ref(), aa.ann.ap.0.as_ref(), text, typed))
}

/// Two axiom annotation sets in the order their sorted lists compare: element by
/// element (property, then value), the shorter list first on a tie.
fn cmp_annotation_lists(a: &BTreeSet<Annotation<RcStr>>, b: &BTreeSet<Annotation<RcStr>>) -> Ordering {
    fn sorted(s: &BTreeSet<Annotation<RcStr>>) -> Vec<&Annotation<RcStr>> {
        let mut v: Vec<&Annotation<RcStr>> = s.iter().collect();
        v.sort_by(|x, y| cmp_annotation(x, y));
        v
    }
    let (x, y) = (sorted(a), sorted(b));
    for (p, q) in x.iter().zip(&y) {
        let o = cmp_annotation(p, q);
        if o != Ordering::Equal {
            return o;
        }
    }
    x.len().cmp(&y.len())
}

fn cmp_annotation(a: &Annotation<RcStr>, b: &Annotation<RcStr>) -> Ordering {
    iri_cmp(a.ap.0.as_ref(), b.ap.0.as_ref()).then_with(|| cmp_annotation_value(&a.av, &b.av))
}

/// The frame's comparison of two of its axioms.
fn cmp_axioms(x: &AnnotatedComponent<RcStr>, y: &AnnotatedComponent<RcStr>) -> Ordering {
    if std::ptr::eq(x, y) {
        return Ordering::Equal;
    }
    if let (Some((sx, px, tx, x_typed)), Some((sy, py, ty, y_typed))) =
        (twin_value(&x.component), twin_value(&y.component))
    {
        if sx == sy && px == py && tx == ty && x_typed != y_typed {
            return if x_typed {
                Ordering::Greater
            } else {
                cmp_annotation_lists(&x.ann, &y.ann)
            };
        }
    }
    cmp_component(&x.component, &y.component)
        .then_with(|| cmp_annotation_lists(&x.ann, &y.ann))
        .then_with(|| x.cmp(y))
}

/// The members of an n-ary class axiom in their sorted order.
fn sorted_members(v: &[CE<RcStr>]) -> Vec<&CE<RcStr>> {
    let mut out: Vec<&CE<RcStr>> = v.iter().collect();
    out.sort_by(|a, b| cmp_ce(a, b));
    out.dedup_by(|a, b| cmp_ce(a, b) == Ordering::Equal);
    out
}

fn first_is(v: &[CE<RcStr>], iri: &str) -> bool {
    matches!(sorted_members(v).first(), Some(CE::Class(c)) if c.0.as_ref() == iri)
}

#[derive(Default)]
struct Frame<'a> {
    declaration: Option<&'a AnnotatedComponent<RcStr>>,
    class_axioms: Vec<&'a AnnotatedComponent<RcStr>>,
    assertions: Vec<&'a AnnotatedComponent<RcStr>>,
}

/// For each class whose frame holds a plain/`xsd:string` twin, the frame's
/// annotation assertions in the order the frame writes them.
pub(crate) fn assertion_orders(model: &Model) -> HashMap<String, Vec<&AnnotatedComponent<RcStr>>> {
    let mut seen: HashMap<(&str, &str, &str), (bool, bool)> = HashMap::new();
    let mut assertions_total = 0usize;
    for ac in model.ont.iter() {
        if matches!(ac.component, Component::AnnotationAssertion(_)) {
            assertions_total += 1;
        }
        if let Some((s, p, t, typed)) = twin_value(&ac.component) {
            let e = seen.entry((s, p, t)).or_default();
            if typed {
                e.1 = true;
            } else {
                e.0 = true;
            }
        }
    }
    let twins: HashSet<&str> =
        seen.iter().filter(|(_, (plain, typed))| *plain && *typed).map(|((s, _, _), _)| *s).collect();
    if twins.is_empty() {
        return HashMap::new();
    }

    let mut frames: HashMap<&str, Frame> = HashMap::new();
    let mut is_class: HashSet<&str> = HashSet::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::DeclareClass(d) => {
                let iri: &str = d.0 .0.as_ref();
                if let Some(f) = twins.get(iri).copied() {
                    is_class.insert(f);
                    frames.entry(f).or_default().declaration = Some(ac);
                }
            }
            Component::SubClassOf(x) => {
                if let CE::Class(c) = &x.sub {
                    if let Some(f) = twins.get(c.0.as_ref()).copied() {
                        is_class.insert(f);
                        frames.entry(f).or_default().class_axioms.push(ac);
                    }
                }
            }
            Component::EquivalentClasses(x) => {
                for m in &x.0 {
                    if let CE::Class(c) = m {
                        if let Some(f) = twins.get(c.0.as_ref()).copied() {
                            is_class.insert(f);
                            frames.entry(f).or_default().class_axioms.push(ac);
                        }
                    }
                }
            }
            Component::DisjointClasses(x) => {
                for m in &x.0 {
                    if let CE::Class(c) = m {
                        if let Some(f) = twins.get(c.0.as_ref()).copied() {
                            is_class.insert(f);
                            frames.entry(f).or_default().class_axioms.push(ac);
                        }
                    }
                }
            }
            Component::DisjointUnion(x) => {
                if let Some(f) = twins.get((x.0).0.as_ref()).copied() {
                    is_class.insert(f);
                    frames.entry(f).or_default().class_axioms.push(ac);
                }
            }
            Component::AnnotationAssertion(aa) => {
                if let AnnotationSubject::IRI(s) = &aa.subject {
                    if let Some(f) = twins.get(s.as_ref()).copied() {
                        frames.entry(f).or_default().assertions.push(ac);
                    }
                }
            }
            _ => {}
        }
    }

    let mut out: HashMap<String, Vec<&AnnotatedComponent<RcStr>>> = HashMap::new();
    for subject in &twins {
        if !is_class.contains(subject) {
            continue;
        }
        let Some(frame) = frames.get_mut(subject) else { continue };
        // A class axiom stated twice (an n-ary axiom naming the class twice) is
        // one axiom.
        frame.class_axioms.sort();
        frame.class_axioms.dedup();
        frame.assertions.sort();
        let mut elems: Vec<&AnnotatedComponent<RcStr>> = Vec::new();
        let mut insertion: Vec<usize> = Vec::new();
        if let Some(d) = frame.declaration {
            insertion.push(elems.len());
            elems.push(d);
        }
        let hashes: Vec<i32> =
            frame.class_axioms.iter().map(|ac| axiom_hash(&ac.component, &ac.ann).unwrap_or(0)).collect();
        for i in hashset_order_of(&hashes, hashes.len()) {
            let ac = frame.class_axioms[i];
            let kept = match &ac.component {
                Component::EquivalentClasses(x) => first_is(&x.0, subject),
                Component::DisjointClasses(x) => sorted_members(&x.0).len() <= 2 && first_is(&x.0, subject),
                _ => true,
            };
            if kept {
                insertion.push(elems.len());
                elems.push(ac);
            }
        }
        let hashes: Vec<i32> = frame
            .assertions
            .iter()
            .map(|ac| {
                let Component::AnnotationAssertion(aa) = &ac.component else { unreachable!() };
                annotation_assertion_hash(subject, aa.ann.ap.0.as_ref(), &aa.ann.av, &ac.ann)
            })
            .collect();
        for i in subject_assertion_order(&hashes, hashes.len(), assertions_total) {
            insertion.push(elems.len());
            elems.push(frame.assertions[i]);
        }

        let cmp = |a: usize, b: usize| cmp_axioms(elems[a], elems[b]);
        let mut listing = tree_set_order(insertion, cmp);
        // A merge that finds the comparison inconsistent cannot order the frame
        // at all; the frame then keeps the order the consistent comparison gives.
        if run_merge_sort(&mut listing, cmp).is_err() {
            continue;
        }
        let order: Vec<&AnnotatedComponent<RcStr>> = listing
            .into_iter()
            .map(|i| elems[i])
            .filter(|ac| matches!(ac.component, Component::AnnotationAssertion(_)))
            .collect();
        out.insert(subject.to_string(), order);
    }
    out
}
