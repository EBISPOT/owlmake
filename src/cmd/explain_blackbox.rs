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

use crate::io::natural_order::NaturalOrder;
use crate::sig::kind;

type Ax = AnnotatedComponent<RcStr>;
type Entity = (u8, String);

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The IRI of the class the entailment checks name the entailment with. The
/// name carries a clock reading, so its hash — and with it the naming axiom's
/// place among the others — is whatever the clock said; this is one reading.
const NAMING_CLASS: &str = "Entailment1790963877324";

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
fn first_disjoint_member(c: &Component<RcStr>, order: NaturalOrder) -> Option<&CE<RcStr>> {
    match c {
        Component::DisjointClasses(d) => {
            let mut members: Vec<&CE<RcStr>> = d.0.iter().collect();
            members.sort_by(|a, b| order.ce(a, b));
            members.first().copied()
        }
        _ => None,
    }
}

/// A Java `HashMap` table size for a set copied from `n` elements.
pub(crate) fn copy_capacity(n: usize) -> usize {
    let want = std::cmp::max((n as f64 / 0.75) as usize + 1, 16);
    want.next_power_of_two()
}

pub(crate) fn bucket(h: i32, cap: usize) -> usize {
    let h = h as u32;
    ((h ^ (h >> 16)) as usize) & (cap - 1)
}

/// The table size of a hash set made with room for `initial` members and then
/// given `n`: a power of two that doubles whenever the set holds more than
/// three quarters of it.
pub(crate) fn presized_capacity(initial: usize, n: usize) -> usize {
    let mut cap = initial.max(1).next_power_of_two();
    while n > cap * 3 / 4 {
        cap *= 2;
    }
    cap
}

/// OWL API's axiom types, in the order its set of them is filled in. The set
/// is a hash set keyed by the type's name, and an ontology lists its logical
/// axioms type by type in the order that set iterates in.
const AXIOM_TYPES: [&str; 39] = [
    "SubClassOf",
    "EquivalentClasses",
    "DisjointClasses",
    "ClassAssertion",
    "SameIndividual",
    "DifferentIndividuals",
    "ObjectPropertyAssertion",
    "NegativeObjectPropertyAssertion",
    "DataPropertyAssertion",
    "NegativeDataPropertyAssertion",
    "ObjectPropertyDomain",
    "ObjectPropertyRange",
    "DisjointObjectProperties",
    "SubObjectPropertyOf",
    "EquivalentObjectProperties",
    "InverseObjectProperties",
    "SubPropertyChainOf",
    "FunctionalObjectProperty",
    "InverseFunctionalObjectProperty",
    "SymmetricObjectProperty",
    "AsymmetricObjectProperty",
    "TransitiveObjectProperty",
    "ReflexiveObjectProperty",
    "IrrefexiveObjectProperty",
    "DataPropertyDomain",
    "DataPropertyRange",
    "DisjointDataProperties",
    "SubDataPropertyOf",
    "EquivalentDataProperties",
    "FunctionalDataProperty",
    "DatatypeDefinition",
    "DisjointUnion",
    "Declaration",
    "Rule",
    "AnnotationAssertion",
    "SubAnnotationPropertyOf",
    "AnnotationPropertyDomain",
    "AnnotationPropertyRangeOf",
    "HasKey",
];

/// The name of `c`'s axiom type, as [`AXIOM_TYPES`] spells it.
fn axiom_type_name(c: &Component<RcStr>) -> &'static str {
    use Component as C;
    match c {
        C::SubClassOf(_) => "SubClassOf",
        C::EquivalentClasses(_) => "EquivalentClasses",
        C::DisjointClasses(_) => "DisjointClasses",
        C::DisjointUnion(_) => "DisjointUnion",
        C::ClassAssertion(_) => "ClassAssertion",
        C::SameIndividual(_) => "SameIndividual",
        C::DifferentIndividuals(_) => "DifferentIndividuals",
        C::ObjectPropertyAssertion(_) => "ObjectPropertyAssertion",
        C::NegativeObjectPropertyAssertion(_) => "NegativeObjectPropertyAssertion",
        C::DataPropertyAssertion(_) => "DataPropertyAssertion",
        C::NegativeDataPropertyAssertion(_) => "NegativeDataPropertyAssertion",
        C::ObjectPropertyDomain(_) => "ObjectPropertyDomain",
        C::ObjectPropertyRange(_) => "ObjectPropertyRange",
        C::DisjointObjectProperties(_) => "DisjointObjectProperties",
        C::SubObjectPropertyOf(x) => match x.sub {
            SOPE::ObjectPropertyChain(_) => "SubPropertyChainOf",
            SOPE::ObjectPropertyExpression(_) => "SubObjectPropertyOf",
        },
        C::EquivalentObjectProperties(_) => "EquivalentObjectProperties",
        C::InverseObjectProperties(_) => "InverseObjectProperties",
        C::FunctionalObjectProperty(_) => "FunctionalObjectProperty",
        C::InverseFunctionalObjectProperty(_) => "InverseFunctionalObjectProperty",
        C::SymmetricObjectProperty(_) => "SymmetricObjectProperty",
        C::AsymmetricObjectProperty(_) => "AsymmetricObjectProperty",
        C::TransitiveObjectProperty(_) => "TransitiveObjectProperty",
        C::ReflexiveObjectProperty(_) => "ReflexiveObjectProperty",
        C::IrreflexiveObjectProperty(_) => "IrrefexiveObjectProperty",
        C::DataPropertyDomain(_) => "DataPropertyDomain",
        C::DataPropertyRange(_) => "DataPropertyRange",
        C::DisjointDataProperties(_) => "DisjointDataProperties",
        C::SubDataPropertyOf(_) => "SubDataPropertyOf",
        C::EquivalentDataProperties(_) => "EquivalentDataProperties",
        C::FunctionalDataProperty(_) => "FunctionalDataProperty",
        C::DatatypeDefinition(_) => "DatatypeDefinition",
        C::HasKey(_) => "HasKey",
        C::Rule(_) => "Rule",
        C::AnnotationAssertion(_) => "AnnotationAssertion",
        C::SubAnnotationPropertyOf(_) => "SubAnnotationPropertyOf",
        C::AnnotationPropertyDomain(_) => "AnnotationPropertyDomain",
        C::AnnotationPropertyRange(_) => "AnnotationPropertyRangeOf",
        _ => "Declaration",
    }
}

/// Each axiom type's place in the order an ontology lists its logical axioms
/// in: the iteration order of a hash set copied from [`AXIOM_TYPES`].
fn axiom_type_ranks() -> HashMap<&'static str, usize> {
    let cap = copy_capacity(AXIOM_TYPES.len());
    let mut order: Vec<usize> = (0..AXIOM_TYPES.len()).collect();
    order.sort_by_key(|&i| bucket(crate::owlapi_hash::java_string_hash(AXIOM_TYPES[i]), cap));
    order.into_iter().enumerate().map(|(rank, i)| (AXIOM_TYPES[i], rank)).collect()
}

/// The axioms an inconsistency search holds: a hash set whose table never
/// shrinks, which the hitting-set tree empties of a path's axioms and then
/// refills. It iterates by bucket, and within a bucket in the order its members
/// went in, so an axiom put back goes to the end of its bucket.
struct WorkingSet<'h> {
    hashes: &'h [i32],
    cap: usize,
    /// When each axiom last went in, or `None` while it is out.
    stamp: Vec<Option<usize>>,
    next: usize,
}

impl WorkingSet<'_> {
    fn contains(&self, i: usize) -> bool {
        self.stamp[i].is_some()
    }

    fn remove(&mut self, i: usize) {
        self.stamp[i] = None;
    }

    fn add(&mut self, i: usize) {
        if self.stamp[i].is_none() {
            self.stamp[i] = Some(self.next);
            self.next += 1;
        }
    }

    fn order(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.stamp.len()).filter(|&i| self.contains(i)).collect();
        v.sort_by_key(|&i| (bucket(self.hashes[i], self.cap), self.stamp[i]));
        v
    }
}

/// The order a hash set copied from `list` and then given `last` iterates in.
fn copied_then_added(list: &[usize], last: usize, hashes: &[i32]) -> Vec<usize> {
    let mut cap = copy_capacity(list.len());
    if list.len() + 1 > cap * 3 / 4 {
        cap *= 2;
    }
    let mut v: Vec<usize> = list.iter().copied().chain([last]).collect();
    v.sort_by_key(|&i| bucket(hashes[i], cap));
    v
}

/// The order an explanation's set of axioms iterates in, from the list the
/// contraction returned them in: the set is copied from that list.
fn explanation_order(list: &[usize], hashes: &[i32]) -> Vec<usize> {
    let cap = copy_capacity(list.len());
    let mut v = list.to_vec();
    v.sort_by_key(|&i| bucket(hashes[i], cap));
    v
}

/// Up to `limit` (at least one) justifications of the inconsistency of
/// `axioms`, none when they are consistent. `entails` decides whether a set of
/// axioms is inconsistent; `imported` tells an axiom of an import from one of
/// the root ontology, which states `root_logical` logical axioms itself.
///
/// The search holds every logical axiom. One justification is the whole set
/// contracted by divide and conquer, in the order the set iterates in, unless
/// the set asserts `owl:Thing ⊑ owl:Nothing`, which is then its own. The set
/// is a copy of one made with room for the root's logical axioms and filled
/// with the root's and then the imports', each listed type by type.
///
/// Further justifications come from a breadth-first hitting-set tree. A node's
/// branches are the axioms of its justification, in the order that
/// justification's set iterates in; a branch takes its path's axioms out of
/// the search, reuses the first justification found so far that misses all of
/// them, and otherwise looks for a new one, then puts the path back. The
/// justifications found so far are kept in a list that turns round each time
/// one is added. A branch whose path a closed path is part of, or that was
/// taken before, is skipped; one that finds no justification closes its path.
///
/// The justifications come back in the order they were found, each in the
/// order its contraction returned it.
pub(crate) fn inconsistency_justifications(
    axioms: &[Ax],
    root_logical: usize,
    limit: usize,
    imported: &dyn Fn(&Ax) -> bool,
    entails: &dyn Fn(&[&Ax]) -> bool,
    order: NaturalOrder,
) -> Vec<Vec<Ax>> {
    let working: Vec<&Ax> = axioms.iter().filter(|ac| crate::cmd::select::is_logical(&ac.component)).collect();
    let n = working.len();
    let hashes: Vec<i32> =
        working.iter().map(|ac| crate::owlapi_hash::axiom_hash(&ac.component, &ac.ann, order).unwrap_or(0)).collect();
    let index: HashMap<*const Ax, usize> = working.iter().enumerate().map(|(i, ac)| (*ac as *const Ax, i)).collect();

    // The order the set made with room for the root's axioms iterates in is
    // the order they went into the copy. Within a type, the order an ontology
    // lists its axioms in depends on how it was read, and past three axioms of
    // one type on a seed drawn afresh each run; the hash stands in for it.
    let ranks = axiom_type_ranks();
    let filled = presized_capacity(root_logical, n);
    let mut listed: Vec<usize> = (0..n).collect();
    listed.sort_by_key(|&i| {
        (bucket(hashes[i], filled), imported(working[i]), ranks[axiom_type_name(&working[i].component)], hashes[i])
    });
    let mut ws = WorkingSet { hashes: &hashes, cap: copy_capacity(n), stamp: vec![None; n], next: 0 };
    for i in listed {
        ws.add(i);
    }

    let b = Build::new();
    let entailment = Component::SubClassOf(SubClassOf {
        sub: CE::Class(b.class(OWL_THING)),
        sup: CE::Class(b.class(OWL_NOTHING)),
    });
    let asserted = working.iter().position(|ac| ac.component == entailment && ac.ann.is_empty());
    let find = |ws: &WorkingSet| -> Option<Vec<usize>> {
        if let Some(a) = asserted.filter(|&a| ws.contains(a)) {
            return Some(vec![a]);
        }
        let members: Vec<&Ax> = ws.order().into_iter().map(|i| working[i]).collect();
        if !entails(&members) {
            return None;
        }
        Some(contract(&[], &members, entails).into_iter().map(|ac| index[&(ac as *const Ax)]).collect())
    };
    let finish = |found: Vec<Vec<usize>>| -> Vec<Vec<Ax>> {
        found.into_iter().map(|j| j.into_iter().map(|i| working[i].clone()).collect()).collect()
    };

    let Some(first) = find(&ws) else {
        return Vec::new();
    };
    let mut found: Vec<Vec<usize>> = vec![first];
    if limit <= 1 {
        return finish(found);
    }
    let mut listed_found: Vec<usize> = vec![0];
    // A node: its justification, and the axioms on the edges from the root.
    let mut queue: std::collections::VecDeque<(usize, Vec<usize>)> = [(0, Vec::new())].into();
    let mut closed: Vec<std::collections::BTreeSet<usize>> = Vec::new();
    let mut explored: HashSet<std::collections::BTreeSet<usize>> = HashSet::new();
    while let Some((node, path)) = queue.pop_front() {
        for ax in explanation_order(&found[node], &hashes) {
            let removed = copied_then_added(&path, ax, &hashes);
            let key: std::collections::BTreeSet<usize> = removed.iter().copied().collect();
            if closed.iter().any(|c| c.is_subset(&key)) || !explored.insert(key.clone()) {
                continue;
            }
            for &a in &removed {
                ws.remove(a);
            }
            let mut next = listed_found.iter().copied().find(|&e| found[e].iter().all(|a| !key.contains(a)));
            if next.is_none() {
                if let Some(j) = find(&ws) {
                    let mut set = j.clone();
                    set.sort_unstable();
                    next = Some(match found.iter().position(|f| {
                        let mut g = f.clone();
                        g.sort_unstable();
                        g == set
                    }) {
                        Some(e) => e,
                        None => {
                            found.push(j);
                            listed_found.push(found.len() - 1);
                            listed_found.reverse();
                            found.len() - 1
                        }
                    });
                    if found.len() == limit {
                        return finish(found);
                    }
                }
            }
            match next {
                Some(e) => {
                    let mut child = path.clone();
                    child.push(ax);
                    queue.push_back((e, child));
                }
                None => closed.push(key),
            }
            for &a in &removed {
                ws.add(a);
            }
        }
    }
    finish(found)
}

/// One justification of `sub ⊑ sup` among `axioms`, or `None` when the
/// axioms do not entail it. `entails` decides whether a set of axioms
/// entails it.
pub(crate) fn justification(
    axioms: &[Ax],
    sub: &str,
    sup: &str,
    entails: &dyn Fn(&[&Ax]) -> bool,
    order: NaturalOrder,
) -> Option<Vec<Ax>> {
    let working: Vec<&Ax> = axioms.iter().filter(|ac| crate::cmd::select::is_logical(&ac.component)).collect();
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
    let expanded = expand(&module, &ix, sub, sup, entails, order)?;

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
    let hash = |ac: &Ax| crate::owlapi_hash::axiom_hash(&ac.component, &ac.ann, order).unwrap_or(0);
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
    order: NaturalOrder,
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
            let admitted = match first_disjoint_member(&module[d].component, order) {
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
        // The declarations of the entailment's own entities stand in the set,
        // so the axioms referring to them are taken in with the rest.
        let held = signature.iter().chain(snapshot.iter().flat_map(|&i| ix.sigs[i].iter()));
        for e in held {
            if let Some(v) = ix.referencing.get(e) {
                expansion.extend(v.iter().copied());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The order OWL API 4.5's `AxiomType.AXIOM_TYPES` iterates in, as the
    /// running library prints it.
    #[test]
    fn axiom_types_rank_in_the_order_owl_api_iterates_them() {
        let expected = "TransitiveObjectProperty FunctionalObjectProperty SameIndividual ReflexiveObjectProperty \
            SubObjectPropertyOf SymmetricObjectProperty SubDataPropertyOf DataPropertyRange DisjointUnion SubClassOf \
            ObjectPropertyRange DataPropertyAssertion AnnotationAssertion DisjointObjectProperties \
            EquivalentObjectProperties SubAnnotationPropertyOf DisjointClasses DataPropertyDomain \
            FunctionalDataProperty Declaration InverseObjectProperties InverseFunctionalObjectProperty \
            IrrefexiveObjectProperty ObjectPropertyAssertion EquivalentClasses DatatypeDefinition \
            DisjointDataProperties SubPropertyChainOf AnnotationPropertyRangeOf ClassAssertion HasKey \
            NegativeObjectPropertyAssertion AsymmetricObjectProperty NegativeDataPropertyAssertion \
            ObjectPropertyDomain EquivalentDataProperties AnnotationPropertyDomain Rule DifferentIndividuals";
        let ranks = axiom_type_ranks();
        let mut names: Vec<&str> = AXIOM_TYPES.to_vec();
        names.sort_by_key(|n| ranks[n]);
        assert_eq!(names.join(" "), expected.split_whitespace().collect::<Vec<_>>().join(" "));
    }
}
