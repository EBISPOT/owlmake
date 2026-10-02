//! One justification for `sub ⊑ sup`, found by black-box search.
//!
//! The search runs over the logical axioms of the ontology in four stages:
//!
//! 1. **Module.** The ⊥⊤*-module of the axioms for the entailment's signature.
//! 2. **Expansion.** Starting from the definitions of the entailment's own
//!    entities, the set grows in rounds — each round adds the definitions of
//!    every entity named by the axioms it already holds — until it entails the
//!    entailment. Disjointness axioms enter only once their first member is
//!    defined, and if the rounds run dry, the disjointness axioms of every
//!    class reached and then every referencing axiom are added.
//! 3. **Order.** The expansion is held as a set of axioms keyed by their
//!    content hash ([`crate::owlapi_hash::axiom_hash`]): its members stand in
//!    the order of the table they hash into, and members that share a slot
//!    stand in the order of the larger table they were copied out of. With
//!    them stands a naming axiom `Entailment… ⊑ sub` that the entailment checks
//!    introduce; it takes a place in the order but never a justification.
//! 4. **Contraction.** Divide and conquer over that order: keep whichever half
//!    still entails, else contract each half against the other.
//!
//! When the expansion holds several justifications, the order decides which
//! one is found, so every stage is exact about which axioms take part and in
//! what order.

use std::collections::{HashMap, HashSet};

use horned_owl::model::{
    AnnotatedComponent, Build, ClassExpression as CE, Component, Individual,
    ObjectPropertyExpression as OPE, RcStr, SubClassOf, SubObjectPropertyExpression as SOPE,
};

use crate::sig::kind;

type Ax = AnnotatedComponent<RcStr>;
type Entity = (u8, String);

const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The IRI of the class the entailment checks name the entailment with. The
/// name carries a clock reading, so its hash — and with it the naming axiom's
/// place among the others — is whatever the clock said; this is one reading.
const NAMING_CLASS: &str = "Entailment1790963877324";

/// Whether an axiom is logical: everything but declarations and annotation
/// axioms.
pub(crate) fn is_logical(c: &Component<RcStr>) -> bool {
    !matches!(
        c,
        Component::DeclareClass(_)
            | Component::DeclareObjectProperty(_)
            | Component::DeclareDataProperty(_)
            | Component::DeclareAnnotationProperty(_)
            | Component::DeclareNamedIndividual(_)
            | Component::DeclareDatatype(_)
            | Component::AnnotationAssertion(_)
            | Component::SubAnnotationPropertyOf(_)
            | Component::AnnotationPropertyDomain(_)
            | Component::AnnotationPropertyRange(_)
            | Component::OntologyAnnotation(_)
            | Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::Import(_)
    )
}

/// The module's axioms, indexed the way the expansion looks them up.
struct Index {
    /// Each axiom's entities, without repeats.
    sigs: Vec<Vec<Entity>>,
    /// An entity's definitions: for a class, the subclass axioms it is the
    /// subclass of, and the equivalence, disjointness and disjoint-union axioms
    /// it is a named member of; for a property, its characteristics, domains,
    /// ranges, the axioms it is a sub-property of, and the equivalence,
    /// disjointness and inverse axioms it belongs to; for an individual, the
    /// assertions about it and the property assertions naming it as object.
    defs: HashMap<Entity, Vec<usize>>,
    /// The disjointness axioms each class is a named member of.
    disjoint_by_class: HashMap<String, Vec<usize>>,
    /// Every axiom an entity occurs in.
    referencing: HashMap<Entity, Vec<usize>>,
}

impl Index {
    fn new(axioms: &[&Ax]) -> Index {
        let mut ix = Index {
            sigs: Vec::with_capacity(axioms.len()),
            defs: HashMap::new(),
            disjoint_by_class: HashMap::new(),
            referencing: HashMap::new(),
        };
        for (i, ac) in axioms.iter().enumerate() {
            let mut sig = crate::sig::typed_signature(&ac.component);
            for (_, iri) in ac.ann.iter().map(|a| (kind::ANNOTATION_PROPERTY, a.ap.0.to_string())) {
                sig.push((kind::ANNOTATION_PROPERTY, iri));
            }
            sig.sort();
            sig.dedup();
            for e in &sig {
                ix.referencing.entry(e.clone()).or_default().push(i);
            }
            ix.sigs.push(sig);
            for key in definition_keys(&ac.component) {
                ix.defs.entry(key).or_default().push(i);
            }
            if let Component::DisjointClasses(d) = &ac.component {
                for m in &d.0 {
                    if let CE::Class(c) = m {
                        ix.disjoint_by_class.entry(c.0.to_string()).or_default().push(i);
                    }
                }
            }
        }
        for v in ix.defs.values_mut() {
            v.dedup();
        }
        ix
    }

    fn defs(&self, e: &Entity) -> &[usize] {
        self.defs.get(e).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The entities an axiom is one of the definitions of.
fn definition_keys(c: &Component<RcStr>) -> Vec<Entity> {
    let class = |ce: &CE<RcStr>| match ce {
        CE::Class(c) => Some((kind::CLASS, c.0.to_string())),
        _ => None,
    };
    let prop = |o: &OPE<RcStr>| match o {
        OPE::ObjectProperty(p) => Some((kind::OBJECT_PROPERTY, p.0.to_string())),
        OPE::InverseObjectProperty(_) => None,
    };
    let ind = |i: &Individual<RcStr>| match i {
        Individual::Named(n) => Some((kind::NAMED_INDIVIDUAL, n.0.to_string())),
        Individual::Anonymous(_) => None,
    };
    let dp = |d: &horned_owl::model::DataProperty<RcStr>| (kind::DATA_PROPERTY, d.0.to_string());
    use Component as C;
    match c {
        C::SubClassOf(x) => class(&x.sub).into_iter().collect(),
        C::EquivalentClasses(x) => x.0.iter().filter_map(class).collect(),
        C::DisjointClasses(x) => x.0.iter().filter_map(class).collect(),
        C::DisjointUnion(x) => vec![(kind::CLASS, (x.0).0.to_string())],
        C::AsymmetricObjectProperty(x) => prop(&x.0).into_iter().collect(),
        C::ReflexiveObjectProperty(x) => prop(&x.0).into_iter().collect(),
        C::SymmetricObjectProperty(x) => prop(&x.0).into_iter().collect(),
        C::IrreflexiveObjectProperty(x) => prop(&x.0).into_iter().collect(),
        C::TransitiveObjectProperty(x) => prop(&x.0).into_iter().collect(),
        C::InverseFunctionalObjectProperty(x) => prop(&x.0).into_iter().collect(),
        C::FunctionalObjectProperty(x) => prop(&x.0).into_iter().collect(),
        C::InverseObjectProperties(x) => [prop(&x.0), prop(&x.1)].into_iter().flatten().collect(),
        C::ObjectPropertyDomain(x) => prop(&x.ope).into_iter().collect(),
        C::ObjectPropertyRange(x) => prop(&x.ope).into_iter().collect(),
        C::EquivalentObjectProperties(x) => x.0.iter().filter_map(prop).collect(),
        C::DisjointObjectProperties(x) => x.0.iter().filter_map(prop).collect(),
        C::SubObjectPropertyOf(x) => match &x.sub {
            SOPE::ObjectPropertyExpression(o) => prop(o).into_iter().collect(),
            SOPE::ObjectPropertyChain(_) => Vec::new(),
        },
        C::DataPropertyDomain(x) => vec![dp(&x.dp)],
        C::DataPropertyRange(x) => vec![dp(&x.dp)],
        C::FunctionalDataProperty(x) => vec![dp(&x.0)],
        C::SubDataPropertyOf(x) => vec![dp(&x.sub)],
        C::EquivalentDataProperties(x) => x.0.iter().map(dp).collect(),
        C::DisjointDataProperties(x) => x.0.iter().map(dp).collect(),
        C::ClassAssertion(x) => ind(&x.i).into_iter().collect(),
        C::ObjectPropertyAssertion(x) => [ind(&x.from), ind(&x.to)].into_iter().flatten().collect(),
        C::NegativeObjectPropertyAssertion(x) => ind(&x.from).into_iter().collect(),
        C::DataPropertyAssertion(x) => ind(&x.from).into_iter().collect(),
        C::NegativeDataPropertyAssertion(x) => ind(&x.from).into_iter().collect(),
        C::SameIndividual(x) => x.0.iter().filter_map(ind).collect(),
        C::DifferentIndividuals(x) => x.0.iter().filter_map(ind).collect(),
        _ => Vec::new(),
    }
}

fn is_abox(c: &Component<RcStr>) -> bool {
    matches!(
        c,
        Component::ClassAssertion(_)
            | Component::ObjectPropertyAssertion(_)
            | Component::DataPropertyAssertion(_)
            | Component::SameIndividual(_)
            | Component::DifferentIndividuals(_)
    )
}

/// The members of a disjointness axiom in their stored order, which decides
/// which member's definition admits the axiom to the expansion.
fn first_disjoint_member(c: &Component<RcStr>) -> Option<&CE<RcStr>> {
    match c {
        Component::DisjointClasses(d) => {
            let mut members: Vec<&CE<RcStr>> = d.0.iter().collect();
            members.sort_by(|a, b| crate::owlapi_hash::owl_cmp(a, b));
            members.first().copied()
        }
        _ => None,
    }
}

/// A Java `HashMap` table size for a set copied from `n` elements.
fn copy_capacity(n: usize) -> usize {
    let want = std::cmp::max((n as f64 / 0.75) as usize + 1, 16);
    want.next_power_of_two()
}

fn bucket(h: i32, cap: usize) -> usize {
    let h = h as u32;
    ((h ^ (h >> 16)) as usize) & (cap - 1)
}

/// One justification of `sub ⊑ sup` among `axioms`, or `None` when the
/// axioms do not entail it. `entails` decides whether a set of axioms
/// entails it.
pub(crate) fn justification(
    axioms: &[Ax],
    sub: &str,
    sup: &str,
    entails: &dyn Fn(&[&Ax]) -> bool,
) -> Option<Vec<Ax>> {
    let working: Vec<&Ax> = axioms.iter().filter(|ac| is_logical(&ac.component)).collect();
    let b = Build::new();
    let entailment = Component::SubClassOf(SubClassOf {
        sub: CE::Class(b.class(sub)),
        sup: CE::Class(b.class(sup)),
    });
    if let Some(ac) = working.iter().find(|ac| ac.component == entailment && ac.ann.is_empty()) {
        return Some(vec![(*ac).clone()]);
    }

    // 1. The ⊥⊤*-module for the entailment's signature.
    let comps: Vec<Component<RcStr>> = working.iter().map(|ac| ac.component.clone()).collect();
    let seed: HashSet<String> = [sub.to_string(), sup.to_string()].into_iter().collect();
    let module: Vec<&Ax> = crate::extract::star_module_indices(&comps, &seed)
        .into_iter()
        .map(|i| working[i])
        .collect();
    if !entails(&module) {
        return None;
    }
    let ix = Index::new(&module);

    // 2. Expansion.
    let expanded = expand(&module, &ix, sub, sup, entails)?;

    // 3. The order the expansion is contracted in: the table it was copied
    // into, then the larger table it was copied out of, which first held the
    // whole module.
    let naming = AnnotatedComponent {
        component: Component::SubClassOf(SubClassOf {
            sub: CE::Class(b.class(NAMING_CLASS)),
            sup: CE::Class(b.class(sub)),
        }),
        ann: Default::default(),
    };
    let mut members: Vec<&Ax> = expanded.iter().map(|&i| module[i]).collect();
    members.push(&naming);
    let outer = crate::owlapi_hash::java_hashset_capacity(module.len() + 1);
    let inner = copy_capacity(members.len());
    let hash = |ac: &Ax| crate::owlapi_hash::axiom_hash(&ac.component, &ac.ann).unwrap_or(0);
    let mut keyed: Vec<(usize, usize, i32, &Ax)> =
        members.iter().map(|ac| (bucket(hash(ac), inner), bucket(hash(ac), outer), hash(ac), *ac)).collect();
    keyed.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let order: Vec<&Ax> = keyed.into_iter().map(|k| k.3).collect();

    // 4. Contraction.
    let found = contract(&[], &order, entails);
    Some(found.into_iter().filter(|ac| !std::ptr::eq(*ac, &naming)).cloned().collect())
}

/// The expansion: the indices (into the module) of the axioms it holds when
/// they first entail the entailment, or `None` if they never do.
fn expand(
    module: &[&Ax],
    ix: &Index,
    sub: &str,
    sup: &str,
    entails: &dyn Fn(&[&Ax]) -> bool,
) -> Option<Vec<usize>> {
    let check = |set: &HashSet<usize>| -> bool {
        let axioms: Vec<&Ax> = set.iter().map(|&i| module[i]).collect();
        entails(&axioms)
    };
    let signature: Vec<Entity> = vec![(kind::CLASS, sub.to_string()), (kind::CLASS, sup.to_string())];
    let mut expansion: HashSet<usize> = HashSet::new();
    let mut expanded: HashSet<Entity> = HashSet::new();
    for e in &signature {
        expanded.insert(e.clone());
        expansion.extend(ix.defs(e).iter().copied());
    }
    let mut disjoints: Vec<usize> = Vec::new();
    let mut disjoint_seen: HashSet<usize> = HashSet::new();
    let mut expansion_sig: HashSet<Entity> = HashSet::new();
    // The declarations of the signature's entities stand in the set from the
    // start, so the count the rounds compare against starts two higher.
    let mut size = usize::MAX;
    while size != expansion.len() {
        size = expansion.len();
        let mut combined = expansion.clone();
        for &d in &disjoints {
            let admitted = match first_disjoint_member(&module[d].component) {
                Some(CE::Class(c)) => expansion_sig.contains(&(kind::CLASS, c.0.to_string())),
                Some(_) => true,
                None => false,
            };
            if admitted {
                combined.insert(d);
            }
        }
        if check(&combined) {
            return Some(combined.into_iter().collect());
        }
        let snapshot: Vec<usize> = expansion.iter().copied().collect();
        for i in snapshot {
            for e in &ix.sigs[i] {
                if expanded.contains(e) {
                    continue;
                }
                for &j in ix.defs(e) {
                    let c = &module[j].component;
                    if matches!(c, Component::DisjointClasses(_)) {
                        if disjoint_seen.insert(j) {
                            disjoints.push(j);
                        }
                    } else if !is_abox(c) {
                        expansion.insert(j);
                        expansion_sig.extend(ix.sigs[j].iter().cloned());
                    }
                }
                expanded.insert(e.clone());
            }
        }
    }
    for e in expansion_sig.iter().filter(|e| e.0 == kind::CLASS) {
        if let Some(v) = ix.disjoint_by_class.get(&e.1) {
            expansion.extend(v.iter().copied());
        }
    }
    if check(&expansion) {
        return Some(expansion.into_iter().collect());
    }
    loop {
        let before = expansion.len();
        let snapshot: Vec<usize> = expansion.iter().copied().collect();
        for i in snapshot {
            for e in &ix.sigs[i] {
                if let Some(v) = ix.referencing.get(e) {
                    expansion.extend(v.iter().copied());
                }
            }
        }
        if check(&expansion) {
            return Some(expansion.into_iter().collect());
        }
        if expansion.len() == before || expansion.len() == module.len() {
            return None;
        }
    }
}

/// Divide-and-conquer contraction of `o` against the fixed part `s`.
fn contract<'a>(s: &[&'a Ax], o: &[&'a Ax], entails: &dyn Fn(&[&Ax]) -> bool) -> Vec<&'a Ax> {
    if o.len() == 1 {
        return o.to_vec();
    }
    let half = o.len() / 2;
    let (s1, s2) = o.split_at(half);
    let with = |extra: &[&'a Ax]| -> Vec<&'a Ax> {
        let mut v = s.to_vec();
        v.extend_from_slice(extra);
        v
    };
    if entails(&with(s1)) {
        return contract(s, s1, entails);
    }
    if entails(&with(s2)) {
        return contract(s, s2, entails);
    }
    let s1p = contract(&with(s2), s1, entails);
    let s2p = contract(&with(&s1p), s2, entails);
    let mut out = s1p;
    out.extend(s2p);
    out
}
