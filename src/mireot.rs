//! MIREOT: terms taken out of an ontology with their ancestors up to given
//! upper terms, or with their descendants, and nothing else about them.
//!
//! A term brings its declaration and its annotation assertions — the input's
//! own, each with its annotations, and with the property it asserts declared
//! when nothing the module holds names that property yet — and the hierarchy
//! axioms joining it to the terms it brings in turn. A datatype brings
//! nothing, and `owl:Thing` is never declared.
//!
//! - **Ancestors** are read over the input and every ontology it imports, as
//!   the structural reasoner reads them. A class's direct superclasses are the
//!   classes of each node its told parents fall in, none reduced away; a class
//!   told none outside its own node sits under `owl:Thing`, and the classes of
//!   its own node are no superclass of it. Object properties are told by
//!   sub-property, equivalence and inverse axioms, data properties by
//!   sub-property axioms alone; either, told none, sits under its top
//!   property, and an object property told only inverses sits under nothing.
//!   Annotation properties climb their asserted super-properties and have no
//!   top. A climb stops at an upper term, and every upper term is brought.
//! - **Descendants** are the input's own asserted named subclasses and data
//!   sub-properties; an annotation property's are read over its imports too.
//!   An object property's walk follows its asserted super-properties instead,
//!   and states each step downward, `SubObjectPropertyOf(super, sub)`.
//! - Under [`Intermediates::Minimal`] each of the two modules — the climbs',
//!   the descents' — is collapsed at threshold 2 on its own, the climbs'
//!   keeping the lower and upper terms, the descents' the branch terms. Under
//!   [`Intermediates::None`] a walk states one axiom from where it starts to
//!   the term that ends it — each upper term a climb reaches, each leaf a
//!   descent reaches — and brings the annotations of the term one step before
//!   that end, not the leaf's own; a climb that reaches no upper term states
//!   nothing. A descent under `None` follows an object property's
//!   sub-properties.
//!
//! A walk that meets a cycle it cannot leave — a cycle of annotation
//! properties above a climb, any asserted cycle below a descent — has no end,
//! and fails.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use anyhow::{bail, Result};
use horned_owl::model::{
    AnnotatedComponent, AnnotationSubject, Build, ClassExpression as CE, Component, DeclareAnnotationProperty,
    DeclareClass, DeclareDataProperty, DeclareNamedIndividual, DeclareObjectProperty, MutableOntology,
    ObjectPropertyExpression as OPE, RcStr, SubAnnotationPropertyOf, SubClassOf, SubDataPropertyOf,
    SubObjectPropertyExpression, SubObjectPropertyOf,
};

use crate::extract::Intermediates;
use crate::io::natural_order::NaturalOrder;
use crate::model::{Model, Onto};
use crate::owlapi_hash;
use crate::reason::told::Told;
use crate::sig::kind;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const TOP_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#topObjectProperty";
const TOP_DATA_PROPERTY: &str = "http://www.w3.org/2002/07/owl#topDataProperty";
/// `rdfs:label`: the one property whose assertions a template's ancestors bring.
pub(crate) const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
/// Marks the inverse of an object property among a property hierarchy's keys;
/// no IRI holds it.
const INVERSE: char = '\u{1}';

/// An entity: one of the [`kind`]s, and its IRI.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Term {
    pub kind: u8,
    pub iri: String,
}

/// The terms of a module, and what they bring.
pub(crate) struct Spec<'t> {
    /// The terms whose ancestors the module holds.
    pub lower: &'t [Term],
    /// The terms a climb stops at.
    pub upper: &'t [Term],
    /// The terms whose descendants the module holds.
    pub branch: &'t [Term],
    /// The one annotation property whose assertions a term brings, or every
    /// property's.
    pub only_annotations: Option<&'t str>,
    pub intermediates: Intermediates,
}

/// The ontology a module is taken from: its own components, read with those
/// of its imports closure.
pub(crate) struct Source<'a> {
    closure: &'a Model,
    root: Vec<&'a AnnotatedComponent<RcStr>>,
    /// The kinds of entity each IRI of the root's signature is.
    kinds: HashMap<String, u8>,
    /// How the root's sets are ordered, for the hashes that order its indexes.
    order: NaturalOrder,
}

impl<'a> Source<'a> {
    /// `root`'s own components, read with `closure`, which holds them and those
    /// of every ontology `root` imports. A model its imports are merged into is
    /// its own closure, and its own components are those the imports did not
    /// lend it.
    pub(crate) fn new(closure: &'a Model, root: &'a Model) -> Source<'a> {
        let mut own: Vec<&AnnotatedComponent<RcStr>> =
            root.ont.iter().filter(|ac| !root.imported_components.contains(*ac)).collect();
        own.sort();
        let mut kinds: HashMap<String, u8> = HashMap::new();
        for ac in &own {
            for (k, iri) in crate::sig::typed_signature(&ac.component) {
                *kinds.entry(iri).or_default() |= k;
            }
            for a in ac.ann.iter() {
                *kinds.entry(a.ap.0.to_string()).or_default() |= kind::ANNOTATION_PROPERTY;
            }
        }
        Source { closure, root: own, kinds, order: root.natural_order() }
    }

    /// The entities `iris` name in the root's signature: each of every kind it
    /// is there, but not the individual of an IRI that is also a class. An IRI
    /// the root's signature does not have names none.
    pub(crate) fn resolve<'i>(&self, iris: impl IntoIterator<Item = &'i String>) -> Vec<Term> {
        let mut out = Vec::new();
        for iri in iris {
            let Some(&kinds) = self.kinds.get(iri) else { continue };
            for k in [
                kind::CLASS,
                kind::OBJECT_PROPERTY,
                kind::DATA_PROPERTY,
                kind::ANNOTATION_PROPERTY,
                kind::NAMED_INDIVIDUAL,
                kind::DATATYPE,
            ] {
                if kinds & k == 0 || (k == kind::NAMED_INDIVIDUAL && kinds & kind::CLASS != 0) {
                    continue;
                }
                out.push(Term { kind: k, iri: iri.clone() });
            }
        }
        out
    }

    /// Whether the root's signature has `term`.
    pub(crate) fn has(&self, term: &Term) -> bool {
        self.kinds.get(&term.iri).is_some_and(|k| k & term.kind != 0)
    }

    /// The root's own components.
    pub(crate) fn root(&self) -> &[&'a AnnotatedComponent<RcStr>] {
        &self.root
    }
}

/// The closure `model` is read with when it is not `model` itself: `model`
/// with the documents its imports closure read. A model its imports are
/// merged into, or that imports nothing, is its own closure.
pub(crate) fn closure_of(model: &Model) -> Result<Option<Model>> {
    let documents = crate::io::closure_documents(model, "the ancestors its imports give cannot be read")?;
    if documents.is_empty() {
        return Ok(None);
    }
    let mut ont = Onto::new();
    for ac in model.ont.iter().chain(documents.iter().flat_map(|d| d.ont.iter())) {
        ont.insert(ac.clone());
    }
    Ok(Some(Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes))))
}

/// The module of `spec`'s terms in `source`, without an ontology IRI.
///
/// The climbs and the descents build modules of their own, joined at the
/// end; the climbs' module brings the upper terms first. Each walks its terms,
/// a term's parents and children, and a term's annotation assertions in the
/// order the input's hash sets hold them — which decides, of a built-in
/// annotation property, whether a module declares it: an assertion brings a
/// declaration of its property only when nothing the module holds names that
/// property yet.
pub(crate) fn module(source: &Source, spec: &Spec) -> Result<Onto> {
    let index = Index::new(source, spec);
    let none = spec.intermediates == Intermediates::None;
    let mut climbs = Module::default();
    for term in in_set_order(spec.upper) {
        climbs.bring(&index, &term);
    }
    for term in in_set_order(spec.lower) {
        climbs.bring(&index, &term);
        if none {
            climbs.span_up(&index, &term)?;
        } else {
            climbs.climb(&index, &term)?;
        }
    }
    let mut descents = Module::default();
    for term in in_set_order(spec.branch) {
        descents.bring(&index, &term);
        if none {
            descents.span_down(&index, &term)?;
        } else {
            descents.descend(&index, &term)?;
        }
    }
    let minimal = spec.intermediates == Intermediates::Minimal;
    let mut out = if minimal { collapsed(climbs.out, spec.lower.iter().chain(spec.upper))? } else { climbs.out };
    let descents = if minimal { collapsed(descents.out, spec.branch.iter())? } else { descents.out };
    for ac in descents.iter() {
        out.insert(ac.clone());
    }
    Ok(out)
}

/// `module` collapsed at threshold 2, keeping the `precious` terms.
fn collapsed<'t>(module: Onto, precious: impl Iterator<Item = &'t Term>) -> Result<Onto> {
    let precious: HashSet<String> = precious.map(|t| t.iri.clone()).collect();
    let (collapsed, _) = crate::cmd::collapse::collapse(Model::from_parts(module, Default::default()), 2, &precious)?;
    Ok(collapsed.ont)
}

/// `terms` in the order a hash set of them holds them.
fn in_set_order(terms: &[Term]) -> Vec<Term> {
    let mut terms = terms.to_vec();
    terms.sort_by(|a, b| (a.kind, &a.iri).cmp(&(b.kind, &b.iri)));
    terms.dedup();
    let hashes: Vec<i32> = terms.iter().map(|t| owlapi_hash::entity_hash(t.kind, &t.iri)).collect();
    owlapi_hash::hashset_order(&hashes).into_iter().map(|i| terms[i].clone()).collect()
}

/// A told property hierarchy: each property's told parents, and the nodes its
/// told cycles make.
struct Properties {
    parents: HashMap<String, Vec<String>>,
    cycle: HashMap<String, Rc<Vec<String>>>,
    top: Rc<Vec<String>>,
}

impl Properties {
    /// The hierarchy each `(sub, super)` pair of `told` is an edge of, under
    /// the property `top`.
    fn new(told: Vec<(String, String)>, top: &str) -> Properties {
        let mut parents: HashMap<String, Vec<String>> = HashMap::new();
        let mut keys: Vec<String> = vec![top.to_string()];
        for (sub, sup) in told {
            keys.push(sub.clone());
            keys.push(sup.clone());
            let entry = parents.entry(sub).or_default();
            if !entry.contains(&sup) {
                entry.push(sup);
            }
        }
        keys.sort();
        keys.dedup();
        let mut cycle: HashMap<String, Rc<Vec<String>>> = HashMap::new();
        for scc in crate::reason::told::strongly_connected(&keys, &parents) {
            if scc.len() > 1 {
                let scc = Rc::new(scc);
                for k in scc.iter() {
                    cycle.insert(k.clone(), scc.clone());
                }
            }
        }
        let top = cycle.get(top).cloned().unwrap_or_else(|| Rc::new(vec![top.to_string()]));
        Properties { parents, cycle, top }
    }

    fn node(&self, p: &str) -> Rc<Vec<String>> {
        self.cycle.get(p).cloned().unwrap_or_else(|| Rc::new(vec![p.to_string()]))
    }

    /// The nodes directly above `p`'s: the node of each told parent of its
    /// members outside it, or the top node when they are told none. The top
    /// node has none above it.
    fn super_nodes(&self, p: &str) -> Vec<Rc<Vec<String>>> {
        let start = self.node(p);
        if start.iter().any(|m| self.top.contains(m)) {
            return Vec::new();
        }
        let mut out: Vec<Rc<Vec<String>>> = Vec::new();
        for m in start.iter() {
            for q in self.parents.get(m).into_iter().flatten() {
                if start.contains(q) {
                    continue;
                }
                let n = self.node(q);
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        if out.is_empty() {
            out.push(self.top.clone());
        }
        out
    }
}

/// An object property expression as a key of the object property hierarchy.
fn key(ope: &OPE<RcStr>) -> String {
    match ope {
        OPE::ObjectProperty(p) => p.0.to_string(),
        OPE::InverseObjectProperty(p) => format!("{INVERSE}{}", p.0),
    }
}

/// The key of the inverse of the property `k` keys.
fn inverse(k: &str) -> String {
    match k.strip_prefix(INVERSE) {
        Some(named) => named.to_string(),
        None => format!("{INVERSE}{k}"),
    }
}

/// An axiom of a [`Grouped`] group, with the value it gives the group.
type Member<'a> = (&'a AnnotatedComponent<RcStr>, Option<String>);

/// Groups of axioms by key, each group as the input's index holds it: the
/// axioms in a hash set of their hashes, filled from the set of every axiom of
/// their kind. The value each axiom gives the group goes with it; an axiom
/// that gives none still counts towards the group's size.
struct Grouped<'a> {
    groups: HashMap<String, Vec<Member<'a>>>,
    /// How many axioms of the kind the input holds in all.
    total: usize,
}

impl<'a> Grouped<'a> {
    fn new() -> Self {
        Grouped { groups: HashMap::new(), total: 0 }
    }

    fn add(&mut self, key: &str, ac: &'a AnnotatedComponent<RcStr>, value: Option<String>) {
        self.groups.entry(key.to_string()).or_default().push((ac, value));
    }

    /// Each group's values, in the order its set holds its axioms.
    fn ordered(self, order: NaturalOrder) -> HashMap<String, Vec<Member<'a>>> {
        let total = self.total;
        self.groups
            .into_iter()
            .map(|(key, mut group)| {
                group.sort();
                let hashes: Vec<i32> = group.iter().map(|(ac, _)| hash_of(ac, order)).collect();
                let ordered = owlapi_hash::subject_assertion_order(&hashes, group.len(), total)
                    .into_iter()
                    .map(|i| group[i].clone())
                    .collect();
                (key, ordered)
            })
            .collect()
    }
}

/// The hash the input's indexes hold `ac` by.
fn hash_of(ac: &AnnotatedComponent<RcStr>, order: NaturalOrder) -> i32 {
    match &ac.component {
        Component::AnnotationAssertion(aa) => {
            let subject = match &aa.subject {
                AnnotationSubject::IRI(s) => s.to_string(),
                AnnotationSubject::AnonymousIndividual(a) => a.0.to_string(),
            };
            owlapi_hash::annotation_assertion_hash(&subject, aa.ann.ap.0.as_ref(), &aa.ann.av, &ac.ann, order)
        }
        c => owlapi_hash::axiom_hash(c, &ac.ann, order).unwrap_or_default(),
    }
}

/// What the walks read of the input, each in the order the input holds it.
struct Index<'a> {
    /// The classes' told hierarchy over the closure.
    classes: Told,
    objects: Properties,
    data: Properties,
    /// Each annotation property's asserted super-properties, and
    /// sub-properties, over the closure.
    annotation_parents: HashMap<String, Vec<String>>,
    annotation_children: HashMap<String, Vec<String>>,
    /// The root's asserted named hierarchy: each class's subclasses, each data
    /// property's sub-properties, each object property's sub-properties and
    /// super-properties.
    class_children: HashMap<String, Vec<String>>,
    data_children: HashMap<String, Vec<String>>,
    object_children: HashMap<String, Vec<String>>,
    object_parents: HashMap<String, Vec<String>>,
    /// The root's annotation assertions, by subject IRI.
    assertions: HashMap<String, Vec<&'a AnnotatedComponent<RcStr>>>,
    upper: HashSet<Term>,
    only: Option<String>,
}

impl<'a> Index<'a> {
    fn new(source: &Source<'a>, spec: &Spec) -> Index<'a> {
        let mut object_told: Vec<(String, String)> = Vec::new();
        // An object property told a parent tells its inverse the parent's
        // inverse.
        let mut tell = |sub: String, sup: String| {
            object_told.push((inverse(&sub), inverse(&sup)));
            object_told.push((sub, sup));
        };
        let mut data_told: Vec<(String, String)> = Vec::new();
        let mut annotation_parents: HashMap<String, Vec<String>> = HashMap::new();
        let mut annotation_children = Grouped::new();
        let mut closure: Vec<&AnnotatedComponent<RcStr>> = source.closure.ont.iter().collect();
        closure.sort();
        for ac in closure {
            match &ac.component {
                Component::SubObjectPropertyOf(SubObjectPropertyOf {
                    sub: SubObjectPropertyExpression::ObjectPropertyExpression(sub),
                    sup,
                }) => tell(key(sub), key(sup)),
                Component::EquivalentObjectProperties(eq) => {
                    for a in &eq.0 {
                        for b in eq.0.iter().filter(|b| *b != a) {
                            tell(key(a), key(b));
                        }
                    }
                }
                // Each property of an inverse pair is the other's inverse.
                Component::InverseObjectProperties(pair) => {
                    let (p, r) = (key(&pair.0), key(&pair.1));
                    tell(p.clone(), inverse(&r));
                    tell(inverse(&r), p);
                }
                Component::SubDataPropertyOf(sp) => data_told.push((sp.sub.0.to_string(), sp.sup.0.to_string())),
                Component::SubAnnotationPropertyOf(sp) => {
                    let (sub, sup) = (sp.sub.0.to_string(), sp.sup.0.to_string());
                    annotation_parents.entry(sub.clone()).or_default().push(sup.clone());
                    annotation_children.add(&sup, ac, Some(sub));
                    annotation_children.total += 1;
                }
                _ => {}
            }
        }
        let mut class_children = Grouped::new();
        let mut data_children = Grouped::new();
        let mut object_children = Grouped::new();
        let mut object_parents = Grouped::new();
        let mut assertions = Grouped::new();
        for ac in source.root.iter().copied() {
            match &ac.component {
                Component::AnnotationAssertion(aa) => {
                    assertions.total += 1;
                    if let AnnotationSubject::IRI(s) = &aa.subject {
                        assertions.add(s, ac, None);
                    }
                }
                Component::SubClassOf(sc) => {
                    class_children.total += 1;
                    if let CE::Class(sup) = &sc.sup {
                        let sub = match &sc.sub {
                            CE::Class(sub) => Some(sub.0.to_string()),
                            _ => None,
                        };
                        class_children.add(&sup.0, ac, sub);
                    }
                }
                Component::SubDataPropertyOf(sp) => {
                    data_children.total += 1;
                    data_children.add(&sp.sup.0, ac, Some(sp.sub.0.to_string()));
                }
                Component::SubObjectPropertyOf(sp) => {
                    object_children.total += 1;
                    object_parents.total += 1;
                    let sub = match &sp.sub {
                        SubObjectPropertyExpression::ObjectPropertyExpression(OPE::ObjectProperty(p)) => {
                            Some(p.0.to_string())
                        }
                        _ => None,
                    };
                    let sup = match &sp.sup {
                        OPE::ObjectProperty(p) => Some(p.0.to_string()),
                        OPE::InverseObjectProperty(_) => None,
                    };
                    if let Some(sup) = &sup {
                        object_children.add(sup, ac, sub.clone());
                    }
                    if let Some(sub) = &sub {
                        object_parents.add(sub, ac, sup);
                    }
                }
                _ => {}
            }
        }
        let order = source.order;
        let values = |grouped: Grouped<'a>| -> HashMap<String, Vec<String>> {
            grouped
                .ordered(order)
                .into_iter()
                .map(|(key, group)| (key, group.into_iter().filter_map(|(_, v)| v).collect()))
                .collect()
        };
        Index {
            classes: Told::of(source.closure),
            objects: Properties::new(object_told, TOP_OBJECT_PROPERTY),
            data: Properties::new(data_told, TOP_DATA_PROPERTY),
            annotation_parents,
            annotation_children: values(annotation_children),
            class_children: values(class_children),
            data_children: values(data_children),
            object_children: values(object_children),
            object_parents: values(object_parents),
            assertions: assertions
                .ordered(order)
                .into_iter()
                .map(|(key, group)| (key, group.into_iter().map(|(ac, _)| ac).collect()))
                .collect(),
            upper: spec.upper.iter().cloned().collect(),
            only: spec.only_annotations.map(str::to_string),
        }
    }

    /// The direct named parents of `term`, as the climbs read them, in the
    /// order a hash set of them holds them.
    fn parents(&self, term: &Term) -> Vec<String> {
        let flatten = |nodes: Vec<Rc<Vec<String>>>| -> Vec<String> {
            nodes.iter().flat_map(|n| n.iter().filter(|m| !m.starts_with(INVERSE)).cloned()).collect()
        };
        let parents = match term.kind {
            kind::CLASS => flatten(self.classes.super_nodes(&term.iri)),
            kind::OBJECT_PROPERTY => flatten(self.objects.super_nodes(&term.iri)),
            kind::DATA_PROPERTY => flatten(self.data.super_nodes(&term.iri)),
            kind::ANNOTATION_PROPERTY => self.annotation_parents.get(&term.iri).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        let terms: Vec<Term> = parents.into_iter().map(|iri| Term { kind: term.kind, iri }).collect();
        in_set_order(&terms).into_iter().map(|t| t.iri).collect()
    }

    /// The terms a descent of kind `of` goes on to from `iri`: its children, or
    /// under `upward` an object property's asserted super-properties.
    fn children(&self, of: u8, iri: &str, upward: bool) -> &[String] {
        let map = match of {
            kind::CLASS => &self.class_children,
            kind::DATA_PROPERTY => &self.data_children,
            kind::ANNOTATION_PROPERTY => &self.annotation_children,
            kind::OBJECT_PROPERTY if upward => &self.object_parents,
            kind::OBJECT_PROPERTY => &self.object_children,
            _ => return &[],
        };
        map.get(iri).map(Vec::as_slice).unwrap_or_default()
    }

    /// Fail when the climb from `term` meets a cycle it cannot leave, which an
    /// annotation property's can.
    fn climb_ends(&self, term: &Term) -> Result<()> {
        if term.kind != kind::ANNOTATION_PROPERTY {
            return Ok(());
        }
        let next = |n: &str| self.annotation_parents.get(n).cloned().unwrap_or_default();
        let stop = |n: &str| self.upper.contains(&Term { kind: term.kind, iri: n.to_string() });
        if let Some(at) = cycle_from(&term.iri, next, stop) {
            bail!(
                "MIREOT: the annotation properties above <{}> make a cycle through <{at}>, so its ancestors never end",
                term.iri
            );
        }
        Ok(())
    }

    /// Fail when the descent from `term` meets a cycle, which it cannot leave.
    fn descent_ends(&self, term: &Term, upward: bool) -> Result<()> {
        let next = |n: &str| self.children(term.kind, n, upward).to_vec();
        if let Some(at) = cycle_from(&term.iri, next, |_| false) {
            bail!("MIREOT: the hierarchy below <{}> makes a cycle through <{at}>, so its descendants never end", term.iri);
        }
        Ok(())
    }
}

/// A module as its walks build it.
#[derive(Default)]
struct Module {
    out: Onto,
    /// The annotation properties what the module holds names.
    properties: HashSet<String>,
    /// The terms whose ancestors are in `out`.
    climbed: HashSet<Term>,
}

impl Module {
    fn insert(&mut self, ac: AnnotatedComponent<RcStr>) {
        self.properties.extend(crate::sig::annotation_properties(&ac.component));
        self.properties.extend(ac.ann.iter().map(|a| a.ap.0.to_string()));
        self.out.insert(ac);
    }

    /// Bring `term`: its declaration and its annotations. A datatype brings
    /// nothing.
    fn bring(&mut self, index: &Index, term: &Term) {
        if term.kind == kind::DATATYPE {
            return;
        }
        self.declare(term);
        self.bring_annotations(index, &term.iri);
    }

    fn declare(&mut self, term: &Term) {
        let b: Build<RcStr> = Build::new();
        let iri = term.iri.as_str();
        let declaration = match term.kind {
            kind::CLASS if iri == OWL_THING => return,
            kind::CLASS => Component::DeclareClass(DeclareClass(b.class(iri))),
            kind::OBJECT_PROPERTY => Component::DeclareObjectProperty(DeclareObjectProperty(b.object_property(iri))),
            kind::DATA_PROPERTY => Component::DeclareDataProperty(DeclareDataProperty(b.data_property(iri))),
            kind::ANNOTATION_PROPERTY => {
                Component::DeclareAnnotationProperty(DeclareAnnotationProperty(b.annotation_property(iri)))
            }
            kind::NAMED_INDIVIDUAL => Component::DeclareNamedIndividual(DeclareNamedIndividual(b.named_individual(iri))),
            _ => return,
        };
        self.insert(declaration.into());
    }

    /// Bring the root's annotation assertions about `iri`, each with a
    /// declaration of its property when nothing the module holds names it.
    fn bring_annotations(&mut self, index: &Index, iri: &str) {
        for ac in index.assertions.get(iri).into_iter().flatten() {
            let Component::AnnotationAssertion(aa) = &ac.component else { continue };
            let property = aa.ann.ap.0.as_ref();
            if index.only.as_deref().is_some_and(|only| property != only) {
                continue;
            }
            if !self.properties.contains(property) {
                self.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(aa.ann.ap.clone())).into());
            }
            self.insert((*ac).clone());
        }
    }

    /// `term`'s ancestors up to the upper terms, each with what it brings,
    /// depth first.
    fn climb(&mut self, index: &Index, term: &Term) -> Result<()> {
        index.climb_ends(term)?;
        let mut path: Vec<(Term, Vec<String>, usize)> = Vec::new();
        let enter = |module: &mut Module, path: &mut Vec<(Term, Vec<String>, usize)>, t: Term| {
            if !index.upper.contains(&t) && module.climbed.insert(t.clone()) {
                let parents = index.parents(&t);
                path.push((t, parents, 0));
            }
        };
        enter(self, &mut path, term.clone());
        while let Some((t, parents, next)) = path.last_mut() {
            let Some(parent) = parents.get(*next).cloned() else {
                path.pop();
                continue;
            };
            *next += 1;
            let t = t.clone();
            self.insert(below(t.kind, &t.iri, &parent).into());
            let parent = Term { kind: t.kind, iri: parent };
            self.bring(index, &parent);
            enter(self, &mut path, parent);
        }
        Ok(())
    }

    /// Under `none`: `term` below each upper term a climb from it reaches,
    /// with the annotations of the term just below each.
    fn span_up(&mut self, index: &Index, term: &Term) -> Result<()> {
        if index.upper.contains(term) {
            return Ok(());
        }
        index.climb_ends(term)?;
        let mut seen: HashSet<String> = HashSet::from([term.iri.clone()]);
        let mut path: Vec<(String, Vec<String>, usize)> =
            vec![(term.iri.clone(), index.parents(term), 0)];
        while let Some((n, parents, next)) = path.last_mut() {
            let Some(parent) = parents.get(*next).cloned() else {
                path.pop();
                continue;
            };
            *next += 1;
            let n = n.clone();
            let reached = Term { kind: term.kind, iri: parent };
            if index.upper.contains(&reached) {
                self.insert(below(term.kind, &term.iri, &reached.iri).into());
                self.bring_annotations(index, &n);
            } else if seen.insert(reached.iri.clone()) {
                let parents = index.parents(&reached);
                path.push((reached.iri, parents, 0));
            }
        }
        Ok(())
    }

    /// `term`'s descendants, each with what it brings, depth first.
    fn descend(&mut self, index: &Index, term: &Term) -> Result<()> {
        index.descent_ends(term, true)?;
        let mut seen: HashSet<String> = HashSet::from([term.iri.clone()]);
        let mut path: Vec<(String, usize)> = vec![(term.iri.clone(), 0)];
        while let Some((n, next)) = path.last_mut() {
            let Some(child) = index.children(term.kind, n, true).get(*next).cloned() else {
                path.pop();
                continue;
            };
            *next += 1;
            let n = n.clone();
            self.insert(below(term.kind, &child, &n).into());
            self.bring(index, &Term { kind: term.kind, iri: child.clone() });
            if seen.insert(child.clone()) {
                path.push((child, 0));
            }
        }
        Ok(())
    }

    /// Under `none`: each leaf below `term` directly below it, with the
    /// annotations of the term just above the leaf.
    fn span_down(&mut self, index: &Index, term: &Term) -> Result<()> {
        index.descent_ends(term, false)?;
        let mut seen: HashSet<String> = HashSet::from([term.iri.clone()]);
        let mut path: Vec<(String, usize)> = vec![(term.iri.clone(), 0)];
        while let Some((n, next)) = path.last_mut() {
            let Some(child) = index.children(term.kind, n, false).get(*next).cloned() else {
                path.pop();
                continue;
            };
            *next += 1;
            let n = n.clone();
            if index.children(term.kind, &child, false).is_empty() {
                self.insert(below(term.kind, &child, &term.iri).into());
                self.declare(&Term { kind: term.kind, iri: child });
                self.bring_annotations(index, &n);
            } else if seen.insert(child.clone()) {
                path.push((child, 0));
            }
        }
        Ok(())
    }
}

/// The axiom stating that `sub` is below `sup`, both of kind `of`.
fn below(of: u8, sub: &str, sup: &str) -> Component<RcStr> {
    let b: Build<RcStr> = Build::new();
    match of {
        kind::CLASS => Component::SubClassOf(SubClassOf { sub: CE::Class(b.class(sub)), sup: CE::Class(b.class(sup)) }),
        kind::OBJECT_PROPERTY => Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: SubObjectPropertyExpression::ObjectPropertyExpression(OPE::ObjectProperty(b.object_property(sub))),
            sup: OPE::ObjectProperty(b.object_property(sup)),
        }),
        kind::DATA_PROPERTY => {
            Component::SubDataPropertyOf(SubDataPropertyOf { sub: b.data_property(sub), sup: b.data_property(sup) })
        }
        _ => Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
            sub: b.annotation_property(sub),
            sup: b.annotation_property(sup),
        }),
    }
}

/// A term a walk from `start` along `next` reaches again while still walking
/// from it, going no further than any term `stop` holds; `None` when every
/// walk ends.
fn cycle_from(start: &str, next: impl Fn(&str) -> Vec<String>, stop: impl Fn(&str) -> bool) -> Option<String> {
    // Each term's state: on the path being walked (`false`), or done (`true`).
    let mut state: HashMap<String, bool> = HashMap::from([(start.to_string(), false)]);
    let mut path: Vec<(String, Vec<String>)> = vec![(start.to_string(), next(start))];
    while let Some((term, rest)) = path.last_mut() {
        let Some(n) = rest.pop() else {
            state.insert(term.clone(), true);
            path.pop();
            continue;
        };
        if stop(&n) {
            continue;
        }
        match state.get(&n) {
            Some(false) => return Some(n),
            Some(true) => {}
            None => {
                state.insert(n.clone(), false);
                let after = next(&n);
                path.push((n, after));
            }
        }
    }
    None
}
