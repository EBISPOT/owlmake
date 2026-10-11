//! Semantic comparison of ontologies by their annotated-component sets.
//!
//! Used both by the `diff` command and by the round-trip test harness. Two
//! ontologies are considered equal when they contain the same set of
//! logical/annotation components, ignoring ordering and ignoring purely
//! locational metadata (the ontology IRI/version and document IRI) which
//! legitimately varies by serialization source.

use std::collections::BTreeMap;

use horned_owl::model::{
    AnnotatedComponent, ClassExpression as CE, Component, DataRange as DR, RcStr,
};

use crate::model::Model;

/// Returns true for components that are document/location metadata rather than
/// ontology content, and therefore should not affect semantic comparison.
fn is_locational(c: &Component<RcStr>) -> bool {
    matches!(c, Component::DocIRI(_) | Component::OntologyID(_))
}

/// Recursively put a data range into canonical form by sorting the operands of
/// its commutative constructors.
fn canon_dr(dr: &mut DR<RcStr>) {
    match dr {
        DR::DataIntersectionOf(v) | DR::DataUnionOf(v) => {
            v.iter_mut().for_each(canon_dr);
            v.sort();
        }
        DR::DataComplementOf(b) => canon_dr(b.as_mut()),
        DR::DataOneOf(v) => v.sort(),
        _ => {}
    }
}

/// Recursively put a class expression into canonical form. `ObjectIntersectionOf`,
/// `ObjectUnionOf` and `ObjectOneOf` are *sets* in OWL 2 — their operand order
/// carries no meaning — but horned-owl stores them as `Vec`, so two equal
/// expressions whose operands were emitted in a different order (a different
/// serialization, or a `relax`/`reduce` rewrite) would otherwise compare unequal.
/// Sort the operands of those constructors after canonicalizing their children.
fn canon_ce(ce: &mut CE<RcStr>) {
    match ce {
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => {
            v.iter_mut().for_each(canon_ce);
            v.sort();
        }
        CE::ObjectComplementOf(b) => canon_ce(b.as_mut()),
        CE::ObjectOneOf(v) => v.sort(),
        CE::ObjectSomeValuesFrom { bce, .. }
        | CE::ObjectAllValuesFrom { bce, .. }
        | CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectMaxCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. } => canon_ce(bce.as_mut()),
        CE::DataSomeValuesFrom { dr, .. } | CE::DataAllValuesFrom { dr, .. } => canon_dr(dr),
        CE::DataMinCardinality { dr, .. }
        | CE::DataMaxCardinality { dr, .. }
        | CE::DataExactCardinality { dr, .. } => canon_dr(dr),
        _ => {}
    }
}

/// Put a component into canonical form so that order-insensitive constructs
/// (set-like class/property/individual lists and the class expressions they
/// contain) compare equal regardless of operand order, following OWL 2's set
/// semantics rather than horned-owl's incidental `Vec` ordering.
fn canon_component(c: &mut Component<RcStr>) {
    match c {
        Component::SubClassOf(a) => {
            canon_ce(&mut a.sup);
            canon_ce(&mut a.sub);
        }
        Component::EquivalentClasses(a) => {
            a.0.iter_mut().for_each(canon_ce);
            a.0.sort();
        }
        Component::DisjointClasses(a) => {
            a.0.iter_mut().for_each(canon_ce);
            a.0.sort();
        }
        Component::DisjointUnion(a) => {
            a.1.iter_mut().for_each(canon_ce);
            a.1.sort();
        }
        Component::ObjectPropertyDomain(a) => canon_ce(&mut a.ce),
        Component::ObjectPropertyRange(a) => canon_ce(&mut a.ce),
        Component::DataPropertyDomain(a) => canon_ce(&mut a.ce),
        Component::DataPropertyRange(a) => canon_dr(&mut a.dr),
        Component::DatatypeDefinition(a) => canon_dr(&mut a.range),
        Component::ClassAssertion(a) => canon_ce(&mut a.ce),
        Component::HasKey(a) => canon_ce(&mut a.ce),
        Component::EquivalentObjectProperties(a) => a.0.sort(),
        Component::DisjointObjectProperties(a) => a.0.sort(),
        Component::EquivalentDataProperties(a) => a.0.sort(),
        Component::DisjointDataProperties(a) => a.0.sort(),
        Component::InverseObjectProperties(a) => {
            if a.1 < a.0 {
                std::mem::swap(&mut a.0, &mut a.1);
            }
        }
        Component::SameIndividual(a) => a.0.sort(),
        Component::DifferentIndividuals(a) => a.0.sort(),
        _ => {}
    }
}

/// The difference between two ontologies: components only in `left` and only in
/// `right`.
pub struct Diff {
    pub only_left: Vec<AnnotatedComponent<RcStr>>,
    pub only_right: Vec<AnnotatedComponent<RcStr>>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.only_left.is_empty() && self.only_right.is_empty()
    }
}

/// Compute the component-level diff between two models. A component compares
/// with a literal typed `xsd:string` as the untyped literal it equals, and is
/// reported as its side states it ([`stated_components`]).
pub fn diff(left: &Model, right: &Model) -> Diff {
    let l = stated_components(left);
    let r = stated_components(right);
    let only = |a: &BTreeMap<_, AnnotatedComponent<RcStr>>, b: &BTreeMap<_, _>| {
        a.iter().filter(|(key, _)| !b.contains_key(*key)).map(|(_, stated)| stated.clone()).collect()
    };
    Diff { only_left: only(&l, &r), only_right: only(&r, &l) }
}

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The comparable components of a model, each keyed by the form it compares
/// in, every literal typed `xsd:string` untyped, to the form the model states
/// it in, every untyped literal typed `xsd:string` where the model's untyped
/// literals are ([`Model::plain_literals_typed`]). Both are in canonical form
/// ([`canon_component`]).
fn stated_components(model: &Model) -> BTreeMap<AnnotatedComponent<RcStr>, AnnotatedComponent<RcStr>> {
    use horned_owl::model::{Build, Literal};
    use horned_owl::visitor::mutable::{VisitMut, WalkMut};

    struct Untype;
    impl VisitMut<RcStr> for Untype {
        fn visit_literal(&mut self, l: &mut Literal<RcStr>) {
            if let Literal::Datatype { literal, datatype_iri } = l {
                if datatype_iri.as_ref() as &str == XSD_STRING {
                    *l = Literal::Simple { literal: std::mem::take(literal) };
                }
            }
        }
    }
    struct Type(horned_owl::model::IRI<RcStr>);
    impl VisitMut<RcStr> for Type {
        fn visit_literal(&mut self, l: &mut Literal<RcStr>) {
            if let Literal::Simple { literal } = l {
                *l = Literal::Datatype { literal: std::mem::take(literal), datatype_iri: self.0.clone() };
            }
        }
    }
    let mut untype = WalkMut::new(Untype);
    let mut typed = model.plain_literals_typed.then(|| WalkMut::new(Type(Build::new().iri(XSD_STRING))));
    model
        .ont
        .iter()
        .filter(|ac| !is_locational(&ac.component))
        .map(|ac| {
            let mut key = ac.clone();
            untype.annotated_component(&mut key);
            canon_component(&mut key.component);
            let mut stated = ac.clone();
            if let Some(typed) = typed.as_mut() {
                typed.annotated_component(&mut stated);
            }
            canon_component(&mut stated.component);
            (key, stated)
        })
        .collect()
}

/// The ontology IRI and version IRI of a model, read from its `OntologyID`
/// component (if any). They stay out of [`stated_components`] (so a comparison of
/// ontology content ignores version stamps); the `diff` command reports them
/// separately via [`ontology_id_change`].
pub fn ontology_id(model: &Model) -> (Option<String>, Option<String>) {
    for ac in model.ont.iter() {
        if let Component::OntologyID(id) = &ac.component {
            let iri = id.iri.as_ref().map(|i| i.as_ref().to_string());
            let ver = id.viri.as_ref().map(|i| i.as_ref().to_string());
            return (iri, ver);
        }
    }
    (None, None)
}

/// A human-readable description of ontology IRI / version IRI differences between
/// two models, or `None` if they match. An ontology ID change is a real
/// difference: it re-identifies the ontology the file claims to be.
pub fn ontology_id_change(left: &Model, right: &Model) -> Option<String> {
    let (li, lv) = ontology_id(left);
    let (ri, rv) = ontology_id(right);
    if li == ri && lv == rv {
        return None;
    }
    let mut s = String::new();
    if li != ri {
        s.push_str(&format!(
            "Ontology IRI changed: {} -> {}\n",
            li.as_deref().unwrap_or("(none)"),
            ri.as_deref().unwrap_or("(none)")
        ));
    }
    if lv != rv {
        s.push_str(&format!(
            "Version IRI changed: {} -> {}\n",
            lv.as_deref().unwrap_or("(none)"),
            rv.as_deref().unwrap_or("(none)")
        ));
    }
    Some(s)
}

/// One component as a plain diff report names it, and as the Markdown report
/// orders it: in the functional writer's
/// [`Simple`](horned_owl::io::ofn::writer::Style::Simple) style, every IRI
/// written in full inside angle brackets except the five built-in prefixes,
/// which are written as CURIEs, and the axiom's annotations inside it.
pub fn describe(ac: &AnnotatedComponent<RcStr>) -> String {
    crate::io::owlfunc::render_component_simple(ac)
}

