//! Semantic comparison of ontologies by their annotated-component sets.
//!
//! Used both by the `diff` command and by the round-trip test harness. Two
//! ontologies are considered equal when they contain the same set of
//! logical/annotation components, ignoring ordering and ignoring purely
//! locational metadata (the ontology IRI/version and document IRI) which
//! legitimately varies by serialization source.

use std::collections::BTreeSet;

use horned_owl::model::{
    AnnotatedComponent, ClassExpression as CE, Component, DataRange as DR, Kinded, RcStr,
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
/// contain) compare equal regardless of operand order.
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
        Component::ClassAssertion(a) => canon_ce(&mut a.ce),
        Component::HasKey(a) => canon_ce(&mut a.ce),
        Component::EquivalentObjectProperties(a) => a.0.sort(),
        Component::DisjointObjectProperties(a) => a.0.sort(),
        Component::EquivalentDataProperties(a) => a.0.sort(),
        Component::DisjointDataProperties(a) => a.0.sort(),
        Component::SameIndividual(a) => a.0.sort(),
        Component::DifferentIndividuals(a) => a.0.sort(),
        _ => {}
    }
}

/// The comparable component set of a model. Each component is put into canonical
/// form (operands of commutative constructs sorted) so the comparison follows
/// OWL 2's set semantics rather than horned-owl's incidental `Vec` ordering.
pub fn component_set(model: &Model) -> BTreeSet<AnnotatedComponent<RcStr>> {
    model
        .ont
        .iter()
        .filter(|ac| !is_locational(&ac.component))
        .map(|ac| {
            let mut ac = ac.clone();
            canon_component(&mut ac.component);
            ac
        })
        .collect()
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

/// Compute the component-level diff between two models.
pub fn diff(left: &Model, right: &Model) -> Diff {
    let l = component_set(left);
    let r = component_set(right);
    Diff {
        only_left: l.difference(&r).cloned().collect(),
        only_right: r.difference(&l).cloned().collect(),
    }
}

/// The ontology IRI and version IRI of a model, read from its `OntologyID`
/// component (if any). They stay out of [`component_set`] (so a comparison of
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

/// Render a component for human-readable diff output. Produces a Manchester-style
/// description with full IRIs (so `--labels` can still substitute labels), rather
/// than Rust `Debug` output.
pub fn describe(ac: &AnnotatedComponent<RcStr>) -> String {
    let body = render_component(&ac.component);
    if ac.ann.is_empty() {
        body
    } else {
        // Note axiom annotations compactly: an annotated axiom is a different
        // component from the bare one, so the reader has to see which it is.
        format!("{body} [annotated]")
    }
}

fn render_component(c: &Component<RcStr>) -> String {
    use Component::*;
    match c {
        SubClassOf(a) => format!("SubClassOf({} {})", render_ce(&a.sub), render_ce(&a.sup)),
        EquivalentClasses(a) => format!(
            "EquivalentClasses({})",
            a.0.iter().map(render_ce).collect::<Vec<_>>().join(", ")
        ),
        DisjointClasses(a) => format!(
            "DisjointClasses({})",
            a.0.iter().map(render_ce).collect::<Vec<_>>().join(", ")
        ),
        DisjointUnion(a) => format!(
            "DisjointUnion({}, {})",
            iri_str(a.0 .0.as_ref()),
            a.1.iter().map(render_ce).collect::<Vec<_>>().join(", ")
        ),
        SubObjectPropertyOf(a) => {
            format!("SubObjectPropertyOf({} {})", render_sub_ope(&a.sub), render_ope_dbg(&a.sup))
        }
        ClassAssertion(a) => format!(
            "ClassAssertion({} {})",
            render_ce(&a.ce),
            render_individual(&a.i)
        ),
        ObjectPropertyAssertion(a) => format!(
            "ObjectPropertyAssertion({} {} {})",
            render_ope_dbg(&a.ope),
            render_individual(&a.from),
            render_individual(&a.to)
        ),
        AnnotationAssertion(a) => format!(
            "AnnotationAssertion({} {} {})",
            iri_str(a.ann.ap.0.as_ref()),
            render_subject(&a.subject),
            render_annval(&a.ann.av)
        ),
        DeclareClass(d) => format!("Declaration(Class({}))", iri_str(d.0 .0.as_ref())),
        DeclareObjectProperty(d) => {
            format!("Declaration(ObjectProperty({}))", iri_str(d.0 .0.as_ref()))
        }
        DeclareDataProperty(d) => {
            format!("Declaration(DataProperty({}))", iri_str(d.0 .0.as_ref()))
        }
        DeclareAnnotationProperty(d) => format!(
            "Declaration(AnnotationProperty({}))",
            iri_str(d.0 .0.as_ref())
        ),
        DeclareNamedIndividual(d) => {
            format!("Declaration(NamedIndividual({}))", iri_str(d.0 .0.as_ref()))
        }
        DeclareDatatype(d) => format!("Declaration(Datatype({}))", iri_str(d.0 .0.as_ref())),
        // Fall back to the kind name + Debug for the long tail of axiom types.
        other => format!("{:?}: {:?}", c.kind(), other),
    }
}

fn render_ce(ce: &CE<RcStr>) -> String {
    match ce {
        CE::Class(c) => iri_str(c.0.as_ref()),
        CE::ObjectIntersectionOf(v) => {
            format!("({})", v.iter().map(render_ce).collect::<Vec<_>>().join(" and "))
        }
        CE::ObjectUnionOf(v) => {
            format!("({})", v.iter().map(render_ce).collect::<Vec<_>>().join(" or "))
        }
        CE::ObjectComplementOf(b) => format!("not {}", render_ce(b)),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            format!("{} some {}", render_ope_dbg(ope), render_ce(bce))
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            format!("{} only {}", render_ope_dbg(ope), render_ce(bce))
        }
        CE::ObjectHasValue { ope, i } => {
            format!("{} value {}", render_ope_dbg(ope), render_individual(i))
        }
        CE::ObjectMinCardinality { n, ope, bce } => {
            format!("{} min {} {}", render_ope_dbg(ope), n, render_ce(bce))
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            format!("{} max {} {}", render_ope_dbg(ope), n, render_ce(bce))
        }
        CE::ObjectExactCardinality { n, ope, bce } => {
            format!("{} exactly {} {}", render_ope_dbg(ope), n, render_ce(bce))
        }
        CE::ObjectHasSelf(ope) => format!("{} Self", render_ope_dbg(ope)),
        CE::ObjectOneOf(v) => format!(
            "{{{}}}",
            v.iter().map(render_individual).collect::<Vec<_>>().join(", ")
        ),
        other => format!("{other:?}"),
    }
}

fn render_ope_dbg(ope: &horned_owl::model::ObjectPropertyExpression<RcStr>) -> String {
    use horned_owl::model::ObjectPropertyExpression as OPE;
    match ope {
        OPE::ObjectProperty(p) => iri_str(p.0.as_ref()),
        OPE::InverseObjectProperty(p) => format!("inverse {}", iri_str(p.0.as_ref())),
    }
}

fn render_sub_ope(sub: &horned_owl::model::SubObjectPropertyExpression<RcStr>) -> String {
    use horned_owl::model::SubObjectPropertyExpression as SOPE;
    match sub {
        SOPE::ObjectPropertyExpression(ope) => render_ope_dbg(ope),
        SOPE::ObjectPropertyChain(chain) => {
            chain.iter().map(render_ope_dbg).collect::<Vec<_>>().join(" o ")
        }
    }
}

fn render_individual(i: &horned_owl::model::Individual<RcStr>) -> String {
    use horned_owl::model::Individual;
    match i {
        Individual::Named(n) => iri_str(n.0.as_ref()),
        Individual::Anonymous(a) => format!("_:{}", a.0.as_ref()),
    }
}

fn render_subject(s: &horned_owl::model::AnnotationSubject<RcStr>) -> String {
    use horned_owl::model::AnnotationSubject;
    match s {
        AnnotationSubject::IRI(i) => iri_str(i.as_ref()),
        AnnotationSubject::AnonymousIndividual(a) => format!("_:{}", a.0.as_ref()),
    }
}

fn render_annval(av: &horned_owl::model::AnnotationValue<RcStr>) -> String {
    use horned_owl::model::AnnotationValue;
    match av {
        AnnotationValue::Literal(l) => format!("{:?}", literal_str(l)),
        AnnotationValue::IRI(i) => iri_str(i.as_ref()),
        AnnotationValue::AnonymousIndividual(a) => format!("_:{}", a.0.as_ref()),
    }
}

fn literal_str(l: &horned_owl::model::Literal<RcStr>) -> String {
    use horned_owl::model::Literal;
    match l {
        Literal::Simple { literal } => literal.clone(),
        Literal::Language { literal, lang } => format!("{literal}@{lang}"),
        Literal::Datatype { literal, datatype_iri } => {
            format!("{literal}^^{}", iri_str(datatype_iri.as_ref()))
        }
    }
}

/// Keep IRIs full (so `--labels` substitution by full IRI still works), but trim
/// nothing else.
fn iri_str(iri: &str) -> String {
    iri.to_string()
}
