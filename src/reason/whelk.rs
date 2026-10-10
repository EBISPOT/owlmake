//! Adapter for the [`whelk`](https://github.com/EBISPOT/whelk-rs) crate — the
//! `--reasoner whelk` backend.
//!
//! whelk-rs is a native Rust OWL 2 EL reasoner. It classifies straight off
//! horned-owl's [`SetOntology`](horned_owl::ontology::set::SetOntology), so we
//! hand it [`Model::ont`] directly and read the saturated subsumption closure
//! back out via
//! [`named_subsumptions`](whelk::whelk::reasoner::ReasonerState::named_subsumptions).
//!
//! The closure whelk exposes is the *full* transitive subsumption relation
//! (every `C ⊑ D` it entails, including reflexive `C ⊑ C`, `C ⊑ owl:Thing`, and
//! `C ⊑ owl:Nothing` for unsatisfiable classes). To present the same results as
//! the built-in EL [`Reasoner`](super::el::Reasoner) — and so feed `reason`'s
//! transitive-reduction/assertion step interchangeably — we re-derive the direct
//! subsumptions, satisfiability, and consistency here using the same rules
//! [`super::el::Reasoner`] applies to its own S-sets.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use horned_owl::model::{
    AnnotatedComponent, ClassExpression as CE, Component, DataRange, Individual, Literal,
    ObjectPropertyExpression as OPE, RcStr,
};
use horned_owl::visitor::immutable::{Visit, Walk};
use whelk::whelk::model::{ConceptData, ConceptId, Interner};
use whelk::whelk::reasoner::ReasonerState;

use crate::model::Model;
use crate::reason::whelk_order;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// The namespace of the probe concepts [`WhelkClassification::unsatisfiable_properties`]
/// adds; no ontology names a class in it.
const PROBE_NS: &str = "urn:owlmake:probe#";
/// The namespace of the classes the reasoner reads a restriction as
/// ([`read_expression`]); no ontology names a class in it.
const ATOM_NS: &str = "urn:owlmake:whelk-atom#";

/// Whether `iri` names one of the classes the reasoner reads a restriction as
/// ([`read_expression`]). No output names one.
pub(crate) fn is_atom(iri: &str) -> bool {
    iri.starts_with(ATOM_NS)
}

/// Whether `id` is a named class: an atomic concept that is not one the
/// reasoner reads a restriction as.
fn is_class(interner: &Interner, id: ConceptId) -> bool {
    matches!(interner.concept_data(id), ConceptData::AtomicConcept(name) if !is_atom(name))
}

/// `model`'s axioms in whelk's normal form, each as the reasoner reads it
/// ([`as_read`]), with the fillers of the restrictions they read as classes
/// ([`read_fillers`]).
pub(crate) fn translate(model: &Model) -> whelk::whelk::model::TranslatedOntology {
    let axioms = crate::reason::owl_axioms(model);
    let read: Vec<Option<Cow<'_, Component<RcStr>>>> = axioms.iter().map(|ac| as_read(&ac.component)).collect();
    if read.iter().all(|r| matches!(r, Some(Cow::Borrowed(_)))) {
        return whelk::whelk::owl::translate_ontology(axioms.as_ref());
    }
    let read: crate::model::Onto = read
        .into_iter()
        .flatten()
        .map(|c| AnnotatedComponent { component: c.into_owned(), ann: Default::default() })
        .collect();
    let mut translated = whelk::whelk::owl::translate_ontology(&read);
    read_fillers(model, &mut translated);
    translated
}

/// `translated` with the filler of each restriction its axioms read as a
/// class ([`object_restriction`]), and of each restriction such a filler
/// names in turn, as a concept the reasoner holds: `F ⊑ F`. The reasoner reads
/// the filler with the axiom, so the individuals it names are held, and a
/// union in it is below every class all its operands are below.
fn read_fillers(model: &Model, translated: &mut whelk::whelk::model::TranslatedOntology) {
    use whelk::whelk::model::ConceptInclusion;
    let restrictions = restrictions(model);
    if restrictions.is_empty() {
        return;
    }
    let fillers: HashMap<&str, &CE<RcStr>> = restrictions.iter().map(|r| (r.atom.as_str(), &r.filler)).collect();
    let interner = &mut translated.interner;
    let mut todo: Vec<ConceptId> = translated
        .concept_inclusions
        .iter()
        .flat_map(|ci| [ci.subclass, ci.superclass])
        .chain(translated.role_ranges.iter().map(|rr| rr.range))
        .flat_map(|c| interner.concept_signature(c))
        .collect();
    let mut seen: HashSet<ConceptId> = HashSet::new();
    let mut held: Vec<ConceptInclusion> = Vec::new();
    while let Some(c) = todo.pop() {
        if !seen.insert(c) {
            continue;
        }
        let ConceptData::AtomicConcept(name) = interner.concept_data(c) else { continue };
        let Some(filler) = fillers.get(name.as_str()).copied() else { continue };
        if let Some(f) = whelk::whelk::owl::convert_expression(filler, interner) {
            held.push(ConceptInclusion { subclass: f, superclass: f });
            todo.extend(interner.concept_signature(f));
        }
    }
    for ci in held {
        translated.concept_inclusions.insert(ci);
    }
}

/// An axiom as the reasoner reads it: `None` for a disjoint union, which it
/// does not read at all — its classes are neither equivalent to the union nor
/// disjoint by it — and for the declaration of a class or an individual, which
/// gives it no concept: a class or an individual only declared is not one the
/// reasoner holds. A difference between named individuals is the disjointness
/// of their nominals, `{a} ⊓ {b} ⊑ owl:Nothing` for each pair. Otherwise the
/// axiom with each class expression read as [`read_expression`] reads it.
fn as_read(c: &Component<RcStr>) -> Option<Cow<'_, Component<RcStr>>> {
    use horned_owl::model::{
        ClassAssertion, DisjointClasses, EquivalentClasses, Individual, ObjectPropertyDomain, ObjectPropertyRange,
        SubClassOf,
    };
    let one = |ce: &CE<RcStr>| read_expression(ce).unwrap_or_else(|| ce.clone());
    let read = match c {
        Component::DisjointUnion(_) | Component::DeclareClass(_) | Component::DeclareNamedIndividual(_) => {
            return None
        }
        Component::DifferentIndividuals(ax) => {
            let nominals: Vec<CE<RcStr>> = ax
                .0
                .iter()
                .filter(|i| matches!(i, Individual::Named(_)))
                .map(|i| CE::ObjectOneOf(vec![i.clone()]))
                .collect();
            if nominals.len() < 2 {
                return None;
            }
            Component::DisjointClasses(DisjointClasses(nominals))
        }
        Component::SubClassOf(ax) if read_expression(&ax.sub).is_some() || read_expression(&ax.sup).is_some() => {
            Component::SubClassOf(SubClassOf { sub: one(&ax.sub), sup: one(&ax.sup) })
        }
        Component::EquivalentClasses(ax) => match read_all(&ax.0) {
            Some(v) => Component::EquivalentClasses(EquivalentClasses(v)),
            None => return Some(Cow::Borrowed(c)),
        },
        Component::DisjointClasses(ax) => match read_all(&ax.0) {
            Some(v) => Component::DisjointClasses(DisjointClasses(v)),
            None => return Some(Cow::Borrowed(c)),
        },
        Component::ObjectPropertyDomain(ax) => match read_expression(&ax.ce) {
            Some(ce) => Component::ObjectPropertyDomain(ObjectPropertyDomain { ope: ax.ope.clone(), ce }),
            None => return Some(Cow::Borrowed(c)),
        },
        Component::ObjectPropertyRange(ax) => match read_expression(&ax.ce) {
            Some(ce) => Component::ObjectPropertyRange(ObjectPropertyRange { ope: ax.ope.clone(), ce }),
            None => return Some(Cow::Borrowed(c)),
        },
        Component::ClassAssertion(ax) => match read_expression(&ax.ce) {
            Some(ce) => Component::ClassAssertion(ClassAssertion { ce, i: ax.i.clone() }),
            None => return Some(Cow::Borrowed(c)),
        },
        _ => return Some(Cow::Borrowed(c)),
    };
    Some(Cow::Owned(read))
}

/// Whether a same- or different-individuals axiom of `model` pairs an
/// anonymous individual with another: its members are a set, so one stated
/// twice is no pair.
fn names_anonymous_equality(model: &Model) -> bool {
    use horned_owl::model::Individual;
    let paired = |v: &[Individual<RcStr>]| {
        let mut members: Vec<&Individual<RcStr>> = v.iter().collect();
        members.sort();
        members.dedup();
        members.len() > 1 && members.iter().any(|i| matches!(i, Individual::Anonymous(_)))
    };
    model.ont.iter().any(|ac| match &ac.component {
        Component::SameIndividual(s) => paired(&s.0),
        Component::DifferentIndividuals(d) => paired(&d.0),
        _ => false,
    })
}

/// A class expression as the reasoner reads it, where whelk-rs would read it
/// otherwise; `None` where the two agree. `p value i` is `p some {i}`; a one-of
/// is the union of its named members, each alone in a one-of; `p max 0 C` is
/// `not (p some C)`. Operands are read the same way.
///
/// A universal or a max-1 restriction ([`object_restriction`]), and a data
/// some-values-from or has-value restriction, is a class of its own, in
/// [`ATOM_NS`]: two are one class when they are the same expression, with
/// operand sets compared as sets and a plain literal as the `xsd:string`
/// literal of its text. So one is below another only as a class is below
/// another: `C ⊑ D` makes nothing of `p only C ⊑ p only D`, nor `xsd:integer`
/// of `xsd:decimal`.
pub(crate) fn read_expression(ce: &CE<RcStr>) -> Option<CE<RcStr>> {
    let one = |ce: &CE<RcStr>| read_expression(ce).unwrap_or_else(|| ce.clone());
    match ce {
        CE::ObjectAllValuesFrom { .. } | CE::ObjectMaxCardinality { n: 1, .. } => {
            object_restriction(ce).map(|r| atom(&r.atom))
        }
        CE::DataSomeValuesFrom { dp, dr } => {
            Some(atom(&format!("{ATOM_NS}some <{}> {}", dp.0.as_ref(), data_range_key(dr))))
        }
        CE::DataHasValue { dp, l } => Some(atom(&format!("{ATOM_NS}value <{}> {}", dp.0.as_ref(), literal_key(l)))),
        CE::ObjectHasValue { ope, i } => {
            Some(CE::ObjectSomeValuesFrom { ope: ope.clone(), bce: Box::new(CE::ObjectOneOf(vec![i.clone()])) })
        }
        CE::ObjectOneOf(v) => {
            let mut named: Vec<&Individual<RcStr>> = v.iter().filter(|i| matches!(i, Individual::Named(_))).collect();
            named.sort();
            named.dedup();
            match named.as_slice() {
                _ if v.len() == 1 => None,
                [] => None,
                [only] => Some(CE::ObjectOneOf(vec![(*only).clone()])),
                many => Some(CE::ObjectUnionOf(many.iter().map(|i| CE::ObjectOneOf(vec![(*i).clone()])).collect())),
            }
        }
        CE::ObjectMaxCardinality { n: 0, ope, bce } => Some(CE::ObjectComplementOf(Box::new(
            CE::ObjectSomeValuesFrom { ope: ope.clone(), bce: Box::new(one(bce)) },
        ))),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            read_expression(bce).map(|bce| CE::ObjectSomeValuesFrom { ope: ope.clone(), bce: Box::new(bce) })
        }
        CE::ObjectComplementOf(b) => read_expression(b).map(|b| CE::ObjectComplementOf(Box::new(b))),
        CE::ObjectIntersectionOf(v) => read_all(v).map(CE::ObjectIntersectionOf),
        CE::ObjectUnionOf(v) => read_all(v).map(CE::ObjectUnionOf),
        _ => None,
    }
}

/// `v` with each member read as [`read_expression`] reads it; `None` where no
/// member is read otherwise.
fn read_all(v: &[CE<RcStr>]) -> Option<Vec<CE<RcStr>>> {
    let read: Vec<Option<CE<RcStr>>> = v.iter().map(read_expression).collect();
    if read.iter().all(Option::is_none) {
        return None;
    }
    Some(read.into_iter().zip(v).map(|(r, ce)| r.unwrap_or_else(|| ce.clone())).collect())
}

/// `ce` with its operands read as [`read_expression`] reads them.
fn read(ce: &CE<RcStr>) -> CE<RcStr> {
    read_expression(ce).unwrap_or_else(|| ce.clone())
}

/// Whether the reasoner reads `ce`, an expression read as [`read_expression`]
/// reads it: a class, a self restriction or an existential restriction over a
/// named property, a non-empty intersection or union, a complement, and a
/// one-of of one named individual, each of expressions it reads.
fn convertible(ce: &CE<RcStr>) -> bool {
    match ce {
        CE::Class(_) | CE::ObjectHasSelf(OPE::ObjectProperty(_)) => true,
        CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(_), bce } => convertible(bce),
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => !v.is_empty() && v.iter().all(convertible),
        CE::ObjectComplementOf(b) => convertible(b),
        CE::ObjectOneOf(v) => matches!(v.as_slice(), [Individual::Named(_)]),
        _ => false,
    }
}

/// A universal or a max-1 restriction the reasoner reads as a class of its
/// own, in [`ATOM_NS`].
pub(crate) struct Restriction {
    /// The IRI of that class.
    pub(crate) atom: String,
    /// Whether it is a max-1 restriction; otherwise it is universal.
    pub(crate) max_one: bool,
    pub(crate) property: String,
    /// The filler, read as [`read_expression`] reads it.
    pub(crate) filler: CE<RcStr>,
}

/// `ce` as the reasoner reads it when it is a universal restriction or a max-1
/// restriction over a named property, with a filler the reasoner reads; `None`
/// for any other expression.
fn object_restriction(ce: &CE<RcStr>) -> Option<Restriction> {
    let (max_one, ope, bce) = match ce {
        CE::ObjectAllValuesFrom { ope, bce } => (false, ope, bce),
        CE::ObjectMaxCardinality { n: 1, ope, bce } => (true, ope, bce),
        _ => return None,
    };
    let OPE::ObjectProperty(p) = ope else { return None };
    let filler = read(bce);
    if !convertible(&filler) {
        return None;
    }
    let property = p.0.as_ref().to_string();
    let kind = if max_one { "max1" } else { "only" };
    let atom = format!("{ATOM_NS}{kind} <{property}> {}", ce_key(&filler));
    Some(Restriction { atom, max_one, property, filler })
}

/// The universal and max-1 restrictions the reasoner reads, wherever `model`
/// names one, each once.
pub(crate) fn restrictions(model: &Model) -> Vec<Restriction> {
    struct Collect(Vec<Restriction>);
    impl Visit<RcStr> for Collect {
        fn visit_class_expression(&mut self, ce: &CE<RcStr>) {
            self.0.extend(object_restriction(ce));
        }
    }
    let mut walk = Walk::new(Collect(Vec::new()));
    for ac in crate::reason::owl_axioms(model).iter() {
        walk.component(&ac.component);
    }
    let mut out = walk.into_visit().0;
    out.sort_by(|a, b| a.atom.cmp(&b.atom));
    out.dedup_by(|a, b| a.atom == b.atom);
    out
}

/// The hash of each restriction `state` holds a class for, by that class
/// ([`whelk_order::restriction_hash`]). A restriction names in its filler only
/// restrictions whose keys are shorter than its own, so taken shortest first,
/// each one's filler is hashed with those it names already in place.
fn restriction_hashes(model: &Model, state: &ReasonerState) -> HashMap<ConceptId, i32> {
    let mut memo: HashMap<ConceptId, i32> = HashMap::new();
    let mut restrictions = restrictions(model);
    restrictions.sort_by_key(|r| r.atom.len());
    let mut interner = state.interner.clone();
    for r in restrictions {
        let Some(atom) = interner.find_concept(&ConceptData::AtomicConcept(r.atom.clone())) else { continue };
        let Some(filler) = whelk::whelk::owl::convert_expression(&r.filler, &mut interner) else { continue };
        let hash = whelk_order::restriction_hash(&interner, r.max_one, &r.property, filler, &mut memo);
        memo.insert(atom, hash);
    }
    memo
}

/// The class named `iri`.
fn atom(iri: &str) -> CE<RcStr> {
    CE::Class(horned_owl::model::Build::new_rc().class(iri))
}

/// `items`, sorted and each once, as one text.
fn set_key(items: impl Iterator<Item = String>) -> String {
    let mut items: Vec<String> = items.collect();
    items.sort();
    items.dedup();
    items.join(" ")
}

/// `ce`, an expression the reasoner reads, as a text that is the same for two
/// expressions exactly when the reasoner takes them for one.
fn ce_key(ce: &CE<RcStr>) -> String {
    let property = |ope: &OPE<RcStr>| match ope {
        OPE::ObjectProperty(p) => format!("<{}>", p.0.as_ref()),
        OPE::InverseObjectProperty(p) => format!("inverse(<{}>)", p.0.as_ref()),
    };
    match ce {
        CE::Class(c) => format!("<{}>", c.0.as_ref()),
        CE::ObjectSomeValuesFrom { ope, bce } => format!("some({} {})", property(ope), ce_key(bce)),
        CE::ObjectHasSelf(ope) => format!("self({})", property(ope)),
        CE::ObjectIntersectionOf(v) => format!("and({})", set_key(v.iter().map(ce_key))),
        CE::ObjectUnionOf(v) => format!("or({})", set_key(v.iter().map(ce_key))),
        CE::ObjectComplementOf(b) => format!("not({})", ce_key(b)),
        CE::ObjectOneOf(v) => format!(
            "one({})",
            set_key(v.iter().map(|i| match i {
                Individual::Named(n) => format!("<{}>", n.0.as_ref()),
                Individual::Anonymous(a) => format!("{a:?}"),
            }))
        ),
        other => format!("{other:?}"),
    }
}

/// `dr` as a text that is the same for two data ranges exactly when the
/// reasoner takes them for one.
fn data_range_key(dr: &DataRange<RcStr>) -> String {
    match dr {
        DataRange::Datatype(d) => format!("<{}>", d.0.as_ref()),
        DataRange::DataIntersectionOf(v) => format!("and({})", set_key(v.iter().map(data_range_key))),
        DataRange::DataUnionOf(v) => format!("or({})", set_key(v.iter().map(data_range_key))),
        DataRange::DataComplementOf(d) => format!("not({})", data_range_key(d)),
        DataRange::DataOneOf(v) => format!("one({})", set_key(v.iter().map(literal_key))),
        DataRange::DatatypeRestriction(d, facets) => format!(
            "restrict(<{}> {})",
            d.0.as_ref(),
            set_key(facets.iter().map(|f| format!("{:?}={}", f.f, literal_key(&f.l))))
        ),
    }
}

/// `l` as a text, a plain literal as the `xsd:string` literal of its text.
fn literal_key(l: &Literal<RcStr>) -> String {
    match l {
        Literal::Simple { literal } => format!("{literal:?}^^<{XSD_STRING}>"),
        Literal::Datatype { literal, datatype_iri } => format!("{literal:?}^^<{}>", datatype_iri.as_ref()),
        Literal::Language { literal, lang } => format!("{literal:?}@{lang}"),
    }
}

/// Whether an axiom the reasoner reads reaches an empty union, which it fails
/// on: in the class of a class assertion or a domain, an operand of an
/// equivalence or a disjointness, the subclass of a subclass axiom, its
/// superclass when the reasoner reads the subclass, and a class in a rule's
/// body. A range, a rule's head and what lies in an expression the reasoner
/// does not read are not reached.
fn reaches_empty_union(model: &Model) -> bool {
    use horned_owl::model::Atom;
    crate::reason::owl_axioms(model).iter().any(|ac| match &ac.component {
        Component::ClassAssertion(ax) => matches!(ax.i, Individual::Named(_)) && empty_union(&ax.ce),
        Component::ObjectPropertyDomain(ax) => matches!(ax.ope, OPE::ObjectProperty(_)) && empty_union(&ax.ce),
        Component::EquivalentClasses(ax) => ax.0.iter().any(empty_union),
        Component::DisjointClasses(ax) => ax.0.iter().any(empty_union),
        Component::SubClassOf(ax) => empty_union(&ax.sub) || (convertible(&read(&ax.sub)) && empty_union(&ax.sup)),
        Component::Rule(rule) => {
            rule.body.iter().any(|atom| matches!(atom, Atom::ClassAtom { pred, .. } if empty_union(pred)))
        }
        _ => false,
    })
}

/// Whether `ce` reaches an empty union through the expressions the reasoner
/// reads.
fn empty_union(ce: &CE<RcStr>) -> bool {
    match ce {
        CE::ObjectUnionOf(v) => v.is_empty() || v.iter().any(empty_union),
        CE::ObjectIntersectionOf(v) => v.iter().any(empty_union),
        CE::ObjectComplementOf(b) => empty_union(b),
        CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(_), bce }
        | CE::ObjectAllValuesFrom { ope: OPE::ObjectProperty(_), bce }
        | CE::ObjectMaxCardinality { n: 0 | 1, ope: OPE::ObjectProperty(_), bce } => empty_union(bce),
        _ => false,
    }
}

/// Saturate `translated`, completing what whelk-rs leaves out ([`complete_bottom`]).
pub(crate) fn saturate(translated: &whelk::whelk::model::TranslatedOntology) -> ReasonerState {
    complete_bottom(whelk::whelk::reasoner::assert(translated))
}

/// Saturate `axioms` onto `state`, completing what whelk-rs leaves out.
pub(crate) fn saturate_append(
    axioms: &whelk::whelk::model::HashSet<whelk::whelk::model::ConceptInclusion>,
    state: &ReasonerState,
) -> ReasonerState {
    complete_bottom(whelk::whelk::reasoner::assert_append(axioms, state))
}

/// `state` with the subject of every link to an unsatisfiable concept
/// unsatisfiable too, and all that follows from that.
///
/// whelk-rs makes a link's subject unsatisfiable when the link is made to a
/// concept already known to be unsatisfiable, and not when the concept becomes
/// unsatisfiable after the link is made. Which comes first follows the order
/// its hash maps iterate in, which differs from run to run, so without this a
/// class could be unsatisfiable in one run and not the next. Each round asserts
/// the subjects the links leave out and saturates again, until a round finds
/// none.
fn complete_bottom(mut state: ReasonerState) -> ReasonerState {
    use whelk::whelk::model::ConceptInclusion;
    loop {
        let bottom = state.interner.bottom();
        let unsatisfiable = |c: &ConceptId| state.closure_subs_by_superclass.get(&bottom).is_some_and(|s| s.contains(c));
        let mut axioms: whelk::whelk::model::HashSet<ConceptInclusion> = Default::default();
        for (subject, roles) in state.links_by_subject() {
            if !unsatisfiable(subject) && roles.values().any(|targets| targets.iter().any(&unsatisfiable)) {
                axioms.insert(ConceptInclusion { subclass: *subject, superclass: bottom });
            }
        }
        if axioms.is_empty() {
            return state;
        }
        state = whelk::whelk::reasoner::assert_append(&axioms, &state);
    }
}

/// A whelk classification, shaped like the built-in EL reasoner's outputs.
pub struct WhelkClassification {
    /// For each named class, the set of its named superclasses in the saturated
    /// closure (includes `owl:Thing`, and `owl:Nothing` when unsatisfiable).
    subs: HashMap<String, HashSet<String>>,
    /// The named classes that appear as a subclass (the closure's domain).
    classes: Vec<String>,
    /// The saturated state. Kept because collapsing an equivalence clique to a
    /// single representative needs the order the reasoner visits a class's
    /// subsumers in, which is a property of the subsumer set as the reasoner
    /// holds it — see [`whelk_order`].
    state: ReasonerState,
    /// The named classes that are equivalent to some other named class. Only a
    /// class with two of these among its superclasses can need a clique
    /// collapsed, and the visit order is reconstructed only for those.
    in_clique: HashSet<String>,
    /// Concept hashes, shared across the classes whose order gets reconstructed.
    hashes: RefCell<HashMap<ConceptId, i32>>,
    /// Whether an axiom the reasoner reads names `owl:Thing`: in a class
    /// expression, the filler of a restriction it reads as a class included;
    /// as a range or a reflexive property; or through what it applies to every
    /// individual it holds ([`super::whelk_individuals::Closed::names_top`]).
    /// Only then is `owl:Thing` among its own subclasses as the reasoner visits
    /// them.
    top_named: bool,
    /// The property values of the named individuals, as `(subject, property,
    /// object)`, sorted ([`whelk_individuals`](super::whelk_individuals)).
    property_values: Vec<super::whelk_individuals::PropertyValue>,
}

impl WhelkClassification {
    /// Translate `model` into whelk's normal form, saturate it with what the
    /// reasoner derives about named individuals, and capture the
    /// named-subsumption closure. Fails on a same- or different-individuals
    /// axiom that pairs an anonymous individual with another, which the
    /// reasoner has no reading of; on an empty union an axiom it reads reaches
    /// ([`reaches_empty_union`]); and where a rule fires with a head variable
    /// its body does not bind.
    pub fn classify(model: &Model) -> anyhow::Result<WhelkClassification> {
        anyhow::ensure!(!names_anonymous_equality(model), "an implementation is missing");
        anyhow::ensure!(!reaches_empty_union(model), "empty.reduceLeft");
        let translated = translate(model);
        let top = translated.interner.top();
        let told_top = !translated.role_ranges.is_empty()
            || translated.concept_inclusions.iter().any(|ci| {
                translated.interner.concept_signature(ci.subclass).contains(&top)
                    || translated.interner.concept_signature(ci.superclass).contains(&top)
            });
        let closed = super::whelk_individuals::close(model, saturate(&translated))?;
        let (state, property_values) = (closed.state, closed.property_values);
        let top_named = told_top || closed.names_top;

        let mut subs: HashMap<String, HashSet<String>> = HashMap::new();
        for (sub, sup) in state.named_subsumptions() {
            if is_atom(sub) || is_atom(sup) {
                continue;
            }
            subs.entry(sub.to_string())
                .or_default()
                .insert(sup.to_string());
        }
        let mut classes: Vec<String> = subs.keys().cloned().collect();
        classes.sort();

        let mut in_clique: HashSet<String> = HashSet::new();
        for (c, sups) in &subs {
            for d in sups {
                if d != c && subs.get(d).is_some_and(|s| s.contains(c)) {
                    in_clique.insert(c.clone());
                    break;
                }
            }
        }

        let hashes = RefCell::new(restriction_hashes(model, &state));
        Ok(WhelkClassification { subs, classes, state, in_clique, hashes, top_named, property_values })
    }

    /// The property values of the named individuals, `(subject, property,
    /// object)`, for the properties in `properties`, which is sorted.
    pub fn object_property_assertions(&self, properties: &[String]) -> Vec<super::whelk_individuals::PropertyValue> {
        self.property_values.iter().filter(|(_, p, _)| properties.binary_search(p).is_ok()).cloned().collect()
    }

    /// Whether the reasoner holds `owl:Thing` as a class of its own: whether
    /// an axiom it reads names it ([`Self::top_named`]). A class it does not
    /// hold it takes to be below `owl:Thing` and nothing else, and so
    /// `owl:Thing` itself, when the reasoner does not hold it.
    pub fn holds_thing(&self) -> bool {
        self.top_named
    }

    /// Where each named subsumer of `c` falls in the order the reasoner visits
    /// them in, as a rank per IRI. The order is over the whole subsumer set —
    /// the anonymous concepts included, since they shape the hash trie the
    /// order comes from — and then narrowed to the named ones.
    fn visit_rank(&self, c: &str) -> HashMap<String, usize> {
        let interner = &self.state.interner;
        let mut rank: HashMap<String, usize> = HashMap::new();
        let Some(id) = interner.find_concept(&ConceptData::AtomicConcept(c.to_string())) else {
            return rank;
        };
        let mut ids: Vec<ConceptId> = self
            .state
            .closure_subs_by_subclass
            .get(&id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let top = interner.top();
        if !ids.contains(&top) {
            ids.push(top);
        }
        let mut hashes = self.hashes.borrow_mut();
        for (i, cid) in whelk_order::visit_order(interner, &ids, &mut hashes)
            .into_iter()
            .enumerate()
        {
            if let ConceptData::AtomicConcept(name) = interner.concept_data(cid) {
                if !is_atom(name) {
                    rank.insert(name.clone(), i);
                }
            }
        }
        rank
    }

    /// Whether `a ⊑ b` holds in the saturated closure.
    fn sub_of(&self, a: &str, b: &str) -> bool {
        self.subs.get(a).is_some_and(|s| s.contains(b))
    }

    /// Whether the closure records `sub ⊑ sup`. A class the closure does not
    /// hold, one named only as a superclass, is below nothing.
    pub fn subsumes(&self, sub: &str, sup: &str) -> bool {
        self.sub_of(sub, sup)
    }

    /// The classes of the top node: `owl:Thing` first, then the classes it is
    /// a subclass of, sorted.
    pub fn top_node(&self) -> Vec<String> {
        let mut rest: Vec<String> = self
            .subs
            .get(OWL_THING)
            .into_iter()
            .flatten()
            .filter(|c| c.as_str() != OWL_THING)
            .cloned()
            .collect();
        rest.sort();
        let mut out = vec![OWL_THING.to_string()];
        out.extend(rest);
        out
    }

    /// The classes directly below `c`, one class for each node. The reasoner
    /// folds over `c`'s subclasses and `owl:Nothing` in the order it visits
    /// them in. A subclass `c` is a subclass of in turn is equivalent to it and
    /// none of them. Any other is kept unless one already kept lies above it,
    /// and each kept one that lies below it is dropped. So of each node the
    /// class met first stands for it, and `owl:Nothing` stays only while
    /// nothing else is kept.
    ///
    /// Five or more subclasses are visited in a hash trie's order. Four or
    /// fewer are visited in the order they were added in, `owl:Nothing` first;
    /// the order the rest were added in is not recorded here, and the trie's
    /// stands in for it.
    pub fn direct_subclasses(&self, c: &str) -> Vec<String> {
        let interner = &self.state.interner;
        let bottom = interner.bottom();
        let top = interner.top();
        let concept = interner.find_concept(&ConceptData::AtomicConcept(c.to_string()));
        let mut ids: Vec<ConceptId> = concept
            .and_then(|id| self.state.closure_subs_by_superclass.get(&id))
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        if concept == Some(top) && !self.top_named {
            ids.retain(|&id| id != top);
        }
        if !ids.contains(&bottom) {
            ids.push(bottom);
        }
        let order = {
            let mut hashes = self.hashes.borrow_mut();
            if ids.len() <= 4 {
                let rest: Vec<ConceptId> = ids.iter().copied().filter(|&id| id != bottom).collect();
                std::iter::once(bottom).chain(whelk_order::visit_order(interner, &rest, &mut hashes)).collect()
            } else {
                whelk_order::visit_order(interner, &ids, &mut hashes)
            }
        };
        // Whether `a ⊑ b` in the closure.
        let below = |a: ConceptId, b: ConceptId| {
            self.state.closure_subs_by_superclass.get(&b).is_some_and(|s| s.contains(&a))
        };
        let mut direct: Vec<ConceptId> = Vec::new();
        for s in order {
            if !is_class(interner, s) || Some(s) == concept {
                continue;
            }
            if concept == Some(bottom) || concept.is_some_and(|id| below(id, s)) {
                continue;
            }
            let mut dropped: Vec<ConceptId> = Vec::new();
            let mut covered = false;
            for &other in &direct {
                if s == bottom || below(s, other) {
                    covered = true;
                    break;
                }
                if other == bottom || below(other, s) {
                    dropped.push(other);
                }
            }
            direct.retain(|d| !dropped.contains(d));
            if !covered {
                direct.push(s);
            }
        }
        direct
            .into_iter()
            .filter_map(|id| match interner.concept_data(id) {
                ConceptData::AtomicConcept(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Each individual the reasoner holds, with its types: for `direct`, the
    /// classes [`Self::direct_subsumers`] keeps of its nominal's subsumers,
    /// visited in the reasoner's order; otherwise every named class the
    /// nominal is subsumed by, and `owl:Thing`. An individual that is only
    /// declared is not held ([`as_read`]).
    pub fn class_assertions(&self, direct: bool) -> Vec<(String, String)> {
        let interner = &self.state.interner;
        let top = interner.top();
        let mut hashes = self.hashes.borrow_mut();
        let mut out = Vec::new();
        for (&nominal, subsumers) in &self.state.closure_subs_by_subclass {
            let ConceptData::Nominal(individual) = interner.concept_data(nominal) else {
                continue;
            };
            let individual = interner.individual_name(*individual);
            let mut ids: Vec<ConceptId> = subsumers.iter().copied().collect();
            if !ids.contains(&top) {
                ids.push(top);
            }
            let types = if direct {
                self.direct_subsumers(nominal, &whelk_order::visit_order(interner, &ids, &mut hashes))
            } else {
                ids
            };
            for t in types {
                if let ConceptData::AtomicConcept(name) = interner.concept_data(t) {
                    if !is_atom(name) {
                        out.push((individual.to_string(), name.clone()));
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The direct subsumers of `concept` among `subsumers`, folded in the
    /// order given. A class below `concept` is its equivalent, not a subsumer.
    /// A class above one already kept is not direct, nor is `owl:Thing` once
    /// anything is kept; a class kept drops those it is below, `owl:Thing`
    /// among them. So of a clique of subsumers, `owl:Thing`'s included, only
    /// the first one met is kept.
    fn direct_subsumers(&self, concept: ConceptId, subsumers: &[ConceptId]) -> Vec<ConceptId> {
        let interner = &self.state.interner;
        let top = interner.top();
        let below = |a: ConceptId, b: ConceptId| self.state.is_subclass_of(a, b);
        let mut direct: Vec<ConceptId> = Vec::new();
        for &s in subsumers {
            if s == concept || !is_class(interner, s) {
                continue;
            }
            if concept == top || below(s, concept) {
                continue;
            }
            let mut dropped = Vec::new();
            let mut kept = true;
            for &other in &direct {
                if s == top || below(other, s) {
                    kept = false;
                    break;
                }
                if other == top || below(s, other) {
                    dropped.push(other);
                }
            }
            direct.retain(|d| !dropped.contains(d));
            if kept {
                direct.push(s);
            }
        }
        direct
    }

    /// Whether the ontology is consistent: no individual, which must exist, is
    /// unsatisfiable. `owl:Thing ⊑ owl:Nothing` alone leaves it consistent,
    /// with every class unsatisfiable.
    pub fn is_consistent(&self) -> bool {
        let bottom = self.state.interner.bottom();
        !self.state.closure_subs_by_superclass.get(&bottom).is_some_and(|subs| {
            subs.iter().any(|&c| matches!(self.state.interner.concept_data(c), ConceptData::Nominal(_)))
        })
    }

    /// The object properties among `properties` for which `∃p.⊤` is
    /// unsatisfiable, sorted. One probe concept `P ⊑ ∃p.⊤` per property is
    /// asserted onto the saturated state and saturated in turn; the state is
    /// persistent, so extending a copy of it costs only the probes.
    pub fn unsatisfiable_properties(&self, properties: &[String]) -> Vec<String> {
        use whelk::whelk::model::ConceptInclusion;
        let mut state = self.state.clone();
        let top = state.interner.top();
        let bottom = state.interner.bottom();
        let mut axioms: whelk::whelk::model::HashSet<ConceptInclusion> = Default::default();
        let mut probes: Vec<(ConceptId, &String)> = Vec::new();
        for (i, p) in properties.iter().enumerate() {
            let role = state.interner.intern_role(p);
            let some = state.interner.intern_concept(ConceptData::ExistentialRestriction { role, concept: top });
            let probe = state.interner.intern_concept(ConceptData::AtomicConcept(format!("{PROBE_NS}{i}")));
            axioms.insert(ConceptInclusion { subclass: probe, superclass: some });
            probes.push((probe, p));
        }
        let saturated = saturate_append(&axioms, &state);
        let mut out: Vec<String> = probes
            .into_iter()
            .filter(|&(probe, _)| saturated.is_subclass_of(probe, bottom))
            .map(|(_, p)| p.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// IRIs of named classes that are unsatisfiable (entail `owl:Nothing`).
    pub fn unsatisfiable(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .subs
            .iter()
            .filter(|(c, sups)| c.as_str() != OWL_NOTHING && sups.contains(OWL_NOTHING))
            .map(|(c, _)| c.clone())
            .collect();
        out.sort();
        out
    }

    /// The **full** named-subsumption closure (every entailed `a ⊑ b` with
    /// `a ≠ b`, excluding ⊤/⊥), matching [`super::el::Reasoner::all_subsumptions`].
    ///
    /// This reads `self.subs` directly rather than deriving from
    /// [`Self::direct_subsumptions`]: that list is a transitive reduction, and it
    /// also *drops* equivalence-clique siblings (the `!equiv` filter below), so it
    /// is a strict subset of the closure and cannot stand in for it. This set is
    /// what `--include-indirect` asserts, and it has to be the same set whichever
    /// EL backend classified.
    pub fn all_subsumptions(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for c in &self.classes {
            if c == OWL_THING || c == OWL_NOTHING {
                continue;
            }
            let Some(sups) = self.subs.get(c) else {
                continue;
            };
            for d in sups {
                if d == c || d == OWL_THING || d == OWL_NOTHING {
                    continue;
                }
                out.push((c.clone(), d.clone()));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The inferred equivalent-class pairs (`c ≡ d`, `c < d` by IRI, excluding
    /// ⊤/⊥) — the mutual-subsumption pairs of the closure. Mirrors
    /// [`super::el::Reasoner::equivalent_class_pairs`], so the equivalence policy
    /// behaves identically whichever EL backend classified.
    pub fn equivalent_class_pairs(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for c in &self.classes {
            if c == OWL_THING || c == OWL_NOTHING {
                continue;
            }
            let Some(sups) = self.subs.get(c) else {
                continue;
            };
            for d in sups {
                if d == c || d == OWL_THING || d == OWL_NOTHING {
                    continue;
                }
                if c < d && self.sub_of(d, c) {
                    out.push((c.clone(), d.clone()));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The transitive reduction of strict subsumption between satisfiable named
    /// classes — the direct `SubClassOf` edges.
    ///
    /// Where two of a class's superclasses are equivalent to each other, one of
    /// them stands for the clique and the rest are dropped: the representative
    /// is the one the reasoner reaches first when it folds over the class's
    /// subsumers, i.e. the earliest in [`whelk_order::visit_order`].
    pub fn direct_subsumptions(&self) -> Vec<(String, String)> {
        let satisfiable = |c: &str| !self.sub_of(c, OWL_NOTHING);
        let equiv = |a: &str, b: &str| a != b && self.sub_of(a, b) && self.sub_of(b, a);

        let mut out: Vec<(String, String)> = Vec::new();
        for c in &self.classes {
            let c = c.as_str();
            if c == OWL_THING || c == OWL_NOTHING || !satisfiable(c) {
                continue;
            }
            let Some(sups) = self.subs.get(c) else {
                continue;
            };
            // Named, satisfiable, proper supers that are not equivalent to `c`
            // (clique siblings are related by an equivalence, not a subsumption
            // edge).
            let supers: Vec<&str> = sups
                .iter()
                .map(String::as_str)
                .filter(|&d| {
                    d != c && d != OWL_THING && d != OWL_NOTHING && satisfiable(d) && !equiv(c, d)
                })
                .collect();
            // A clique among the supers collapses to whichever member the
            // subsumer walk reaches first, so the order is only needed when two
            // of the supers are equivalent to something.
            let rank = (supers.iter().filter(|d| self.in_clique.contains(**d)).count() > 1)
                .then(|| self.visit_rank(c));
            for &d in &supers {
                if let Some(rank) = &rank {
                    let at = |x: &str| rank.get(x).copied().unwrap_or(usize::MAX);
                    if supers.iter().any(|&e| equiv(d, e) && at(e) < at(d)) {
                        continue;
                    }
                }
                // `d` is non-direct if some other super `mid` lies strictly
                // between `c` and `d` (`mid ⊑ d`, not `d ⊑ mid`, and `mid` is a
                // proper intermediate above `c`, i.e. not equivalent to `c`).
                let redundant = supers.iter().any(|&mid| {
                    mid != d
                        && self.sub_of(mid, d)
                        && !self.sub_of(d, mid)
                        && !self.sub_of(mid, c)
                });
                if !redundant {
                    out.push((c.to_string(), d.to_string()));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}
