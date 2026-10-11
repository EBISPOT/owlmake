//! What the whelk reasoner derives about named individuals beyond whelk-rs's
//! own saturation: their property values, their equalities, the clashes
//! between their facts, and the SWRL rules over them.
//!
//! Each round reads the facts whelk-rs holds — the links between nominals,
//! `{a} ⊑ ∃p.{b}`, a nominal's self restrictions and the nominals it is
//! subsumed by — and closes them under the property axioms: sub- and
//! equivalent properties, inverses, symmetry, transitivity and chains, and
//! equality, an individual below another's nominal being equal to it. It then
//! derives what those facts force: two individuals an
//! (inverse-)functional property relates to one are equal, and a negative
//! assertion, or an irreflexive, asymmetric or disjoint property, makes the
//! ontology inconsistent. It applies the universal and max-1 restrictions the
//! reasoner reads as classes of their own
//! ([`read_expression`](super::whelk::read_expression)): each value of `p` an
//! individual in `p only C` has is a `C`, and the values of `p` an individual
//! in `p max 1 C` has that are `C`s are equal. Then it fires the rules. Every
//! fact whelk-rs does not hold goes back to it — `{a} ⊑ ∃p.{b}`, `{a} ⊑ {b}`
//! both ways, `{a} ⊑ C`, or `{a} ⊑ owl:Nothing` for a clash — and the next
//! round starts from the saturated result, until a round has nothing to add.
//!
//! The reasoner holds an individual — a member of `owl:Thing`, related to
//! itself by a reflexive property — when an axiom it reads names it. Beside
//! the axioms whelk-rs reads, those are a negative assertion on a named
//! property, and a rule it reads, which holds every individual it names, in
//! its class atoms and the fillers of their restrictions too, whether it fires
//! or not.
//!
//! The property axioms are read over named properties alone: one that names
//! an inverse is not read, but for a disjointness and a property assertion.
//! A rule is read when its atoms are class atoms over a class the reasoner
//! reads, object-property atoms, and same- and different-individual atoms,
//! over variables and named individuals. It fires when its body is not empty
//! and it has no different-individuals atom. A body class atom holds of an
//! individual whose nominal the class subsumes, but for a class that is one
//! individual's nominal, which holds of none; a body property atom of a pair
//! the closed facts relate; a same-individual atom of equal individuals, each
//! individual equal to itself. A head variable the body does not bind fails
//! the rule when it fires, with the message `key not found: Variable(<IRI>)`.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use horned_owl::model::{
    Atom, ClassExpression as CE, Component, IArgument, Individual, ObjectPropertyExpression as OPE, RcStr,
    SubObjectPropertyExpression as SOPE,
};
use whelk::whelk::model::{ConceptData, ConceptId, ConceptInclusion, Interner, RoleId, COMPOSITION_ROLE_PREFIX};
use whelk::whelk::reasoner::ReasonerState;

use crate::model::Model;

/// The namespace of the classes a round asks the reasoner for the members of
/// a class expression with ([`members_of`]); no ontology names a class in it.
const QUERY_NS: &str = "urn:owlmake:member-query#";

/// A fact `role(subject, object)`, the individuals as their nominals.
type Fact = (usize, ConceptId, ConceptId);

/// A property value `(subject, property, object)`, as IRIs.
pub(crate) type PropertyValue = (String, String, String);

/// What [`close`] derives.
pub(crate) struct Closed {
    pub(crate) state: ReasonerState,
    /// The property values of the named individuals, for named properties,
    /// sorted.
    pub(crate) property_values: Vec<PropertyValue>,
    /// Whether what the reasoner applies to every individual it holds names
    /// `owl:Thing`: a rule it reads, a negative assertion, or an irreflexive,
    /// asymmetric or disjoint-properties axiom.
    pub(crate) names_top: bool,
}

/// A universal or max-1 restriction the reasoner reads as a class of its own.
struct Restriction {
    /// The class it is read as.
    atom: ConceptId,
    role: usize,
    filler: ConceptId,
    max_one: bool,
}

/// A property a fact can be of: a named object property, or one standing for
/// the first steps of a chain of more than two.
struct Role {
    /// The property's IRI; `None` for a chain's steps.
    iri: Option<String>,
    /// The role whelk-rs knows the property as.
    whelk: Option<RoleId>,
}

/// The property axioms and the facts about individuals the reasoner reads.
#[derive(Default)]
struct Axioms {
    roles: Vec<Role>,
    ids: HashMap<String, usize>,
    by_whelk: HashMap<RoleId, usize>,
    /// The direct super-properties of each role.
    sub_of: Vec<Vec<usize>>,
    /// The roles each role's facts are facts of, read backwards.
    inverse: Vec<Vec<usize>>,
    /// Each chain step as the first of a pair: the second, and the role the
    /// pair composes to.
    by_first: HashMap<usize, Vec<(usize, usize)>>,
    /// Each chain step as the second of a pair: the first, and the role the
    /// pair composes to.
    by_second: HashMap<usize, Vec<(usize, usize)>>,
    functional: Vec<usize>,
    inverse_functional: Vec<usize>,
    irreflexive: Vec<usize>,
    asymmetric: Vec<usize>,
    /// Each disjointness of two or more properties, its properties as
    /// `(role, inverse)`, each once. A disjointness of one property is none.
    disjoint: Vec<Vec<(usize, bool)>>,
    /// The negative assertions `role(subject, object)`.
    negative: Vec<(usize, String, String)>,
    same: Vec<Vec<String>>,
    rules: Vec<Rule<String, CE<RcStr>>>,
}

/// A rule's argument: a variable, by its IRI, or an individual.
#[derive(Clone)]
enum Term<I> {
    Var(String),
    Ind(I),
}

/// A rule's atom.
#[derive(Clone)]
enum RuleAtom<I, C> {
    Class(C, Term<I>),
    /// `role(subject, object)`; for an inverse property the arguments swapped.
    Property(usize, Term<I>, Term<I>),
    Same(Term<I>, Term<I>),
}

#[derive(Clone)]
struct Rule<I, C> {
    body: Vec<RuleAtom<I, C>>,
    head: Vec<RuleAtom<I, C>>,
    /// The individuals its different-individuals atoms name.
    different: Vec<I>,
    /// Whether it fires: one with an empty body or a different-individuals
    /// atom never does.
    fires: bool,
}

impl Axioms {
    fn role(&mut self, iri: &str) -> usize {
        if let Some(&r) = self.ids.get(iri) {
            return r;
        }
        let r = self.push(Some(iri.to_string()));
        self.ids.insert(iri.to_string(), r);
        r
    }

    fn push(&mut self, iri: Option<String>) -> usize {
        self.roles.push(Role { iri, whelk: None });
        self.sub_of.push(Vec::new());
        self.inverse.push(Vec::new());
        self.roles.len() - 1
    }

    /// The role whelk-rs's `role` is, named `name`.
    fn whelk_role(&mut self, role: RoleId, name: &str) -> usize {
        if let Some(&r) = self.by_whelk.get(&role) {
            return r;
        }
        let r = self.role(name);
        self.roles[r].whelk = Some(role);
        self.by_whelk.insert(role, r);
        r
    }

    /// The named property `ope` is, if it is one.
    fn named(&mut self, ope: &OPE<RcStr>) -> Option<usize> {
        match ope {
            OPE::ObjectProperty(p) => Some(self.role(p.0.as_ref())),
            OPE::InverseObjectProperty(_) => None,
        }
    }

    /// `ope` as `(role, inverse)`.
    fn expression(&mut self, ope: &OPE<RcStr>) -> (usize, bool) {
        match ope {
            OPE::ObjectProperty(p) => (self.role(p.0.as_ref()), false),
            OPE::InverseObjectProperty(p) => (self.role(p.0.as_ref()), true),
        }
    }

    /// A chain `steps ⊑ sup` as compositions of pairs, its first steps
    /// standing for one role each.
    fn chain(&mut self, steps: &[usize], sup: usize) {
        let mut first = steps[0];
        for (i, &second) in steps[1..].iter().enumerate() {
            let composed = if i + 2 == steps.len() { sup } else { self.push(None) };
            self.by_first.entry(first).or_default().push((second, composed));
            self.by_second.entry(second).or_default().push((first, composed));
            first = composed;
        }
    }

    fn read(model: &Model) -> Axioms {
        let mut ax = Axioms::default();
        let named_individual = |i: &Individual<RcStr>| match i {
            Individual::Named(n) => Some(n.0.to_string()),
            Individual::Anonymous(_) => None,
        };
        for ac in crate::reason::owl_axioms(model).iter() {
            match &ac.component {
                Component::SubObjectPropertyOf(s) => {
                    let Some(sup) = ax.named(&s.sup) else { continue };
                    match &s.sub {
                        SOPE::ObjectPropertyExpression(sub) => {
                            if let Some(sub) = ax.named(sub) {
                                ax.sub_of[sub].push(sup);
                            }
                        }
                        SOPE::ObjectPropertyChain(steps) => {
                            let steps: Option<Vec<usize>> = steps.iter().map(|s| ax.named(s)).collect();
                            match steps.as_deref() {
                                Some([only]) => ax.sub_of[*only].push(sup),
                                Some(steps) if steps.len() > 1 => ax.chain(steps, sup),
                                _ => {}
                            }
                        }
                    }
                }
                Component::EquivalentObjectProperties(e) => {
                    let named: Vec<usize> = e.0.iter().filter_map(|p| ax.named(p)).collect();
                    for &a in &named {
                        for &b in &named {
                            if a != b {
                                ax.sub_of[a].push(b);
                            }
                        }
                    }
                }
                Component::InverseObjectProperties(i) => {
                    if let (Some(a), Some(b)) = (ax.named(&i.0), ax.named(&i.1)) {
                        ax.inverse[a].push(b);
                        ax.inverse[b].push(a);
                    }
                }
                Component::SymmetricObjectProperty(s) => {
                    if let Some(p) = ax.named(&s.0) {
                        ax.inverse[p].push(p);
                    }
                }
                Component::TransitiveObjectProperty(t) => {
                    if let Some(p) = ax.named(&t.0) {
                        ax.chain(&[p, p], p);
                    }
                }
                Component::FunctionalObjectProperty(f) => {
                    if let Some(p) = ax.named(&f.0) {
                        ax.functional.push(p);
                    }
                }
                Component::InverseFunctionalObjectProperty(f) => {
                    if let Some(p) = ax.named(&f.0) {
                        ax.inverse_functional.push(p);
                    }
                }
                Component::IrreflexiveObjectProperty(i) => {
                    if let Some(p) = ax.named(&i.0) {
                        ax.irreflexive.push(p);
                    }
                }
                Component::AsymmetricObjectProperty(a) => {
                    if let Some(p) = ax.named(&a.0) {
                        ax.asymmetric.push(p);
                    }
                }
                Component::DisjointObjectProperties(d) => {
                    let mut props: Vec<(usize, bool)> = Vec::new();
                    for p in &d.0 {
                        let p = ax.expression(p);
                        if !props.contains(&p) {
                            props.push(p);
                        }
                    }
                    if props.len() > 1 {
                        ax.disjoint.push(props);
                    }
                }
                Component::NegativeObjectPropertyAssertion(n) => {
                    if let (Some(p), Some(from), Some(to)) =
                        (ax.named(&n.ope), named_individual(&n.from), named_individual(&n.to))
                    {
                        ax.negative.push((p, from, to));
                    }
                }
                Component::SameIndividual(s) => {
                    ax.same.push(s.0.iter().filter_map(named_individual).collect());
                }
                Component::Rule(rule) => {
                    if let Some(rule) = ax.rule(&rule.body, &rule.head) {
                        ax.rules.push(rule);
                    }
                }
                _ => {}
            }
        }
        ax
    }

    /// A rule as the reasoner reads it; `None` for one it does not read.
    fn rule(&mut self, body: &[Atom<RcStr>], head: &[Atom<RcStr>]) -> Option<Rule<String, CE<RcStr>>> {
        let mut different = Vec::new();
        let mut fires = !body.is_empty();
        let mut read = |atoms: &[Atom<RcStr>]| -> Option<Vec<RuleAtom<String, CE<RcStr>>>> {
            let mut out = Vec::new();
            for atom in atoms {
                match atom {
                    Atom::DifferentIndividualsAtom(a, b) => {
                        for t in [argument(a)?, argument(b)?] {
                            if let Term::Ind(i) = t {
                                different.push(i);
                            }
                        }
                        fires = false;
                    }
                    _ => out.push(self.atom(atom)?),
                }
            }
            Some(out)
        };
        let body = read(body)?;
        let head = read(head)?;
        Some(Rule { body, head, different, fires })
    }

    fn atom(&mut self, atom: &Atom<RcStr>) -> Option<RuleAtom<String, CE<RcStr>>> {
        Some(match atom {
            Atom::ClassAtom { pred, arg } => RuleAtom::Class(pred.clone(), argument(arg)?),
            Atom::ObjectPropertyAtom { pred, args } => {
                let (from, to) = (argument(&args.0)?, argument(&args.1)?);
                match self.expression(pred) {
                    (r, false) => RuleAtom::Property(r, from, to),
                    (r, true) => RuleAtom::Property(r, to, from),
                }
            }
            Atom::SameIndividualAtom(a, b) => RuleAtom::Same(argument(a)?, argument(b)?),
            _ => return None,
        })
    }
}

/// A rule's argument as the reasoner reads it; `None` for an anonymous
/// individual.
fn argument(arg: &IArgument<RcStr>) -> Option<Term<String>> {
    match arg {
        IArgument::Variable(v) => Some(Term::Var(v.0.to_string())),
        IArgument::Individual(Individual::Named(n)) => Some(Term::Ind(n.0.to_string())),
        IArgument::Individual(Individual::Anonymous(_)) => None,
    }
}

/// The facts a round has closed, indexed.
#[derive(Default)]
struct Facts {
    all: HashSet<Fact>,
    by_subject: HashMap<(usize, ConceptId), Vec<ConceptId>>,
    by_object: HashMap<(usize, ConceptId), Vec<ConceptId>>,
    by_role: HashMap<usize, Vec<(ConceptId, ConceptId)>>,
}

impl Facts {
    fn insert(&mut self, f: Fact) -> bool {
        if !self.all.insert(f) {
            return false;
        }
        let (r, a, b) = f;
        self.by_subject.entry((r, a)).or_default().push(b);
        self.by_object.entry((r, b)).or_default().push(a);
        self.by_role.entry(r).or_default().push((a, b));
        true
    }

    fn objects(&self, r: usize, a: ConceptId) -> Vec<ConceptId> {
        self.by_subject.get(&(r, a)).cloned().unwrap_or_default()
    }

    fn subjects(&self, r: usize, b: ConceptId) -> Vec<ConceptId> {
        self.by_object.get(&(r, b)).cloned().unwrap_or_default()
    }

    fn pairs(&self, r: usize) -> &[(ConceptId, ConceptId)] {
        self.by_role.get(&r).map_or(&[], Vec::as_slice)
    }
}

/// `state` with every fact about its named individuals saturated in, and
/// those facts as `(subject, property, object)` IRIs, for named properties.
pub(crate) fn close(model: &Model, mut state: ReasonerState) -> Result<Closed> {
    let mut ax = Axioms::read(model);
    let read = super::whelk::restrictions(model);
    let restriction_roles: Vec<usize> = read.iter().map(|r| ax.role(&r.property)).collect();
    for r in 0..ax.roles.len() {
        if let Some(iri) = ax.roles[r].iri.clone() {
            let role = state.interner.intern_role(&iri);
            ax.roles[r].whelk = Some(role);
            ax.by_whelk.insert(role, r);
        }
    }
    let restrictions: Vec<Restriction> = read
        .iter()
        .zip(restriction_roles)
        .filter_map(|(r, role)| {
            let atom = state.interner.intern_concept(ConceptData::AtomicConcept(r.atom.clone()));
            let filler = whelk::whelk::owl::convert_expression(&r.filler, &mut state.interner)?;
            Some(Restriction { atom, role, filler, max_one: r.max_one })
        })
        .collect();
    let fillers: HashMap<ConceptId, ConceptId> = restrictions.iter().map(|r| (r.atom, r.filler)).collect();
    let mut nominal = |iri: &str| {
        let i = state.interner.intern_individual(iri);
        state.interner.intern_concept(ConceptData::Nominal(i))
    };
    let negative: Vec<Fact> = ax.negative.iter().map(|(r, a, b)| (*r, nominal(a), nominal(b))).collect();
    let same: Vec<Vec<ConceptId>> = ax.same.iter().map(|s| s.iter().map(|i| nominal(i)).collect()).collect();
    let rules = bind(&ax.rules, &mut state);
    let (top, bottom) = (state.interner.top(), state.interner.bottom());
    let names_top = !rules.is_empty()
        || !negative.is_empty()
        || !ax.irreflexive.is_empty()
        || !ax.asymmetric.is_empty()
        || !ax.disjoint.is_empty();
    let mut held: Vec<ConceptId> = negative.iter().flat_map(|&(_, a, b)| [a, b]).collect();
    for rule in &rules {
        held.extend(rule.different.iter().copied());
        for atom in rule.body.iter().chain(&rule.head) {
            match atom {
                RuleAtom::Class(c, t) => {
                    held.extend(named_in(&state, &fillers, *c).into_iter().filter(|&n| is_nominal(&state, n)));
                    held.extend(nominal_of(t));
                }
                RuleAtom::Property(_, s, o) | RuleAtom::Same(s, o) => held.extend(nominal_of(s).into_iter().chain(nominal_of(o))),
            }
        }
    }
    let hold: whelk::whelk::model::HashSet<ConceptInclusion> = held
        .into_iter()
        .filter(|&n| !state.is_subclass_of(n, top))
        .map(|n| ConceptInclusion { subclass: n, superclass: top })
        .collect();
    if !hold.is_empty() {
        state = super::whelk::saturate_append(&hold, &state);
    }
    let rules: Vec<Rule<ConceptId, ConceptId>> = rules.into_iter().filter(|r| r.fires).collect();
    let mut asserted: HashSet<ConceptInclusion> = HashSet::new();
    loop {
        let facts = close_facts(&mut ax, &state);
        let mut derived: Vec<Fact> = facts.all.iter().copied().collect();
        let mut equal: Vec<(ConceptId, ConceptId)> = Vec::new();
        for members in &same {
            equal.extend(members.windows(2).map(|w| (w[0], w[1])));
        }
        // An individual below another's nominal is that individual.
        for n in nominals(&state) {
            equal.extend(equals(&state, n).into_iter().filter(|&m| m != n).map(|m| (n, m)));
        }
        for &p in &ax.functional {
            for (&(r, _), objects) in &facts.by_subject {
                if r == p {
                    equal.extend(objects.windows(2).map(|w| (w[0], w[1])));
                }
            }
        }
        for &p in &ax.inverse_functional {
            for (&(r, _), subjects) in &facts.by_object {
                if r == p {
                    equal.extend(subjects.windows(2).map(|w| (w[0], w[1])));
                }
            }
        }
        let clash = clashes(&ax, &facts, &negative);
        let mut kinds: Vec<(ConceptId, ConceptId)> = Vec::new();
        if !restrictions.is_empty() {
            let max_one: Vec<ConceptId> = restrictions.iter().filter(|r| r.max_one).map(|r| r.filler).collect();
            let members = members_of(&max_one, &state);
            for r in &restrictions {
                for subject in below(&state, r.atom) {
                    let values = facts.objects(r.role, subject);
                    if r.max_one {
                        let within: Vec<ConceptId> =
                            values.into_iter().filter(|v| members[&r.filler].contains(v)).collect();
                        equal.extend(within.windows(2).map(|w| (w[0], w[1])));
                    } else {
                        kinds.extend(values.into_iter().map(|v| (v, r.filler)));
                    }
                }
            }
        }
        if !rules.is_empty() {
            let members = members(&rules, &state);
            for rule in &rules {
                fire(rule, &facts, &members, &state, &mut derived, &mut equal, &mut kinds)?;
            }
        }
        let mut add: Vec<ConceptInclusion> = Vec::new();
        for (r, a, b) in derived {
            let Some(role) = ax.roles[r].whelk else { continue };
            let linked =
                state.links_by_subject().get(&a).and_then(|roles| roles.get(&role)).is_some_and(|t| t.contains(&b));
            if !linked {
                let some = state.interner.intern_concept(ConceptData::ExistentialRestriction { role, concept: b });
                add.push(ConceptInclusion { subclass: a, superclass: some });
            }
        }
        for (a, b) in equal {
            if a != b {
                add.push(ConceptInclusion { subclass: a, superclass: b });
                add.push(ConceptInclusion { subclass: b, superclass: a });
            }
        }
        for (a, c) in kinds {
            add.push(ConceptInclusion { subclass: a, superclass: c });
        }
        add.extend(clash.into_iter().map(|a| ConceptInclusion { subclass: a, superclass: bottom }));
        add.retain(|ci| !state.is_subclass_of(ci.subclass, ci.superclass) && asserted.insert(*ci));
        if add.is_empty() {
            let mut out: Vec<PropertyValue> = facts
                .all
                .iter()
                .filter_map(|&(r, a, b)| {
                    let iri = ax.roles[r].iri.clone()?;
                    Some((individual(&state, a)?, iri, individual(&state, b)?))
                })
                .collect();
            out.sort();
            return Ok(Closed { state, property_values: out, names_top });
        }
        let add: whelk::whelk::model::HashSet<ConceptInclusion> = add.into_iter().collect();
        state = super::whelk::saturate_append(&add, &state);
    }
}

/// The IRI of the individual `nominal` stands for.
fn individual(state: &ReasonerState, nominal: ConceptId) -> Option<String> {
    match state.interner.concept_data(nominal) {
        ConceptData::Nominal(i) => Some(state.interner.individual_name(*i).to_string()),
        _ => None,
    }
}

fn is_nominal(state: &ReasonerState, c: ConceptId) -> bool {
    matches!(state.interner.concept_data(c), ConceptData::Nominal(_))
}

/// The individuals `state` holds, as their nominals.
fn nominals(state: &ReasonerState) -> Vec<ConceptId> {
    let mut out: Vec<ConceptId> =
        state.closure_subs_by_subclass.keys().copied().filter(|&c| is_nominal(state, c)).collect();
    out.sort();
    out
}

/// The nominals `nominal` is equal to, itself included.
fn equals(state: &ReasonerState, nominal: ConceptId) -> Vec<ConceptId> {
    let mut out: Vec<ConceptId> = state
        .closure_subs_by_subclass
        .get(&nominal)
        .into_iter()
        .flatten()
        .copied()
        .filter(|&c| c != nominal && is_nominal(state, c))
        .collect();
    out.push(nominal);
    out.sort();
    out
}

/// The facts `state` holds about its individuals, closed under the property
/// axioms and equality.
fn close_facts(ax: &mut Axioms, state: &ReasonerState) -> Facts {
    let mut seeds: Vec<Fact> = Vec::new();
    for (&subject, roles) in state.links_by_subject() {
        if !is_nominal(state, subject) {
            continue;
        }
        for (&role, targets) in roles {
            let name = state.interner.role_name(role);
            if name.starts_with(COMPOSITION_ROLE_PREFIX) {
                continue;
            }
            let r = ax.whelk_role(role, name);
            seeds.extend(targets.iter().filter(|&&t| is_nominal(state, t)).map(|&t| (r, subject, t)));
        }
    }
    for n in nominals(state) {
        for &c in state.closure_subs_by_subclass.get(&n).into_iter().flatten() {
            if let ConceptData::SelfRestriction(role) = state.interner.concept_data(c) {
                let r = ax.whelk_role(*role, state.interner.role_name(*role));
                seeds.push((r, n, n));
            }
        }
    }
    let mut eq: HashMap<ConceptId, Vec<ConceptId>> = HashMap::new();
    let mut facts = Facts::default();
    let mut queue = seeds;
    while let Some((r, a, b)) = queue.pop() {
        let ea = eq.entry(a).or_insert_with(|| equals(state, a)).clone();
        let eb = eq.entry(b).or_insert_with(|| equals(state, b)).clone();
        for &a in &ea {
            for &b in &eb {
                if !facts.insert((r, a, b)) {
                    continue;
                }
                queue.extend(ax.sub_of[r].iter().map(|&s| (s, a, b)));
                queue.extend(ax.inverse[r].iter().map(|&q| (q, b, a)));
                for &(second, composed) in ax.by_first.get(&r).into_iter().flatten() {
                    queue.extend(facts.objects(second, b).into_iter().map(|c| (composed, a, c)));
                }
                for &(first, composed) in ax.by_second.get(&r).into_iter().flatten() {
                    queue.extend(facts.subjects(first, a).into_iter().map(|z| (composed, z, b)));
                }
            }
        }
    }
    facts
}

/// The individuals whose facts clash, one for each clash.
fn clashes(ax: &Axioms, facts: &Facts, negative: &[Fact]) -> Vec<ConceptId> {
    let mut out: Vec<ConceptId> = negative.iter().filter(|f| facts.all.contains(f)).map(|f| f.1).collect();
    for &p in &ax.irreflexive {
        out.extend(facts.pairs(p).iter().filter(|(a, b)| a == b).map(|(a, _)| *a));
    }
    for &p in &ax.asymmetric {
        out.extend(facts.pairs(p).iter().filter(|(a, b)| facts.all.contains(&(p, *b, *a))).map(|(a, _)| *a));
    }
    let holds = |(r, inverse): (usize, bool), a: ConceptId, b: ConceptId| {
        if inverse {
            facts.all.contains(&(r, b, a))
        } else {
            facts.all.contains(&(r, a, b))
        }
    };
    for props in &ax.disjoint {
        for (i, &first) in props.iter().enumerate() {
            for &second in &props[i + 1..] {
                for &(a, b) in facts.pairs(first.0) {
                    let (a, b) = if first.1 { (b, a) } else { (a, b) };
                    if holds(second, a, b) {
                        out.push(a);
                    }
                }
            }
        }
    }
    out
}

/// The rules with their individuals as nominals and their class expressions
/// as the reasoner's concepts. A rule with a class expression the reasoner
/// cannot read is not read at all.
fn bind(rules: &[Rule<String, CE<RcStr>>], state: &mut ReasonerState) -> Vec<Rule<ConceptId, ConceptId>> {
    let mut out = Vec::new();
    'rules: for rule in rules {
        let mut atoms: [Vec<RuleAtom<ConceptId, ConceptId>>; 2] = [Vec::new(), Vec::new()];
        for (side, list) in [&rule.body, &rule.head].into_iter().enumerate() {
            for atom in list {
                let interner = &mut state.interner;
                atoms[side].push(match atom {
                    RuleAtom::Class(ce, t) => {
                        let read = super::whelk::read_expression(ce).unwrap_or_else(|| ce.clone());
                        let Some(c) = whelk::whelk::owl::convert_expression(&read, interner) else {
                            continue 'rules;
                        };
                        RuleAtom::Class(c, term(t, interner))
                    }
                    RuleAtom::Property(r, s, o) => RuleAtom::Property(*r, term(s, interner), term(o, interner)),
                    RuleAtom::Same(a, b) => RuleAtom::Same(term(a, interner), term(b, interner)),
                });
            }
        }
        let [body, head] = atoms;
        let different = rule.different.iter().map(|i| term(&Term::Ind(i.clone()), &mut state.interner)).filter_map(|t| nominal_of(&t)).collect();
        out.push(Rule { body, head, different, fires: rule.fires });
    }
    out
}

/// The nominal `t` is, when it is an individual.
fn nominal_of(t: &Term<ConceptId>) -> Option<ConceptId> {
    match t {
        Term::Ind(n) => Some(*n),
        Term::Var(_) => None,
    }
}

/// `t` with its individual as its nominal.
fn term(t: &Term<String>, interner: &mut Interner) -> Term<ConceptId> {
    match t {
        Term::Var(v) => Term::Var(v.clone()),
        Term::Ind(iri) => {
            let i = interner.intern_individual(iri);
            Term::Ind(interner.intern_concept(ConceptData::Nominal(i)))
        }
    }
}

/// The concepts `c` names, and those the fillers of the restrictions among
/// them name, in turn.
fn named_in(state: &ReasonerState, fillers: &HashMap<ConceptId, ConceptId>, c: ConceptId) -> HashSet<ConceptId> {
    let mut out: HashSet<ConceptId> = HashSet::new();
    let mut todo = vec![c];
    while let Some(c) = todo.pop() {
        for n in state.interner.concept_signature(c) {
            if out.insert(n) {
                todo.extend(fillers.get(&n).copied());
            }
        }
    }
    out
}

/// The individuals the closure puts below `c`, as their nominals, sorted.
fn below(state: &ReasonerState, c: ConceptId) -> Vec<ConceptId> {
    let mut out: Vec<ConceptId> =
        state.closure_subs_by_superclass.get(&c).into_iter().flatten().copied().filter(|&n| is_nominal(state, n)).collect();
    out.sort();
    out
}

/// The members of each class the rules' bodies name ([`members_of`]); a class
/// that is one individual's nominal has none.
fn members(rules: &[Rule<ConceptId, ConceptId>], state: &ReasonerState) -> HashMap<ConceptId, Vec<ConceptId>> {
    let classes: Vec<ConceptId> = rules
        .iter()
        .flat_map(|r| &r.body)
        .filter_map(|a| match a {
            RuleAtom::Class(c, _) => Some(*c),
            _ => None,
        })
        .collect();
    let mut out = members_of(&classes, state);
    for (c, members) in out.iter_mut() {
        if is_nominal(state, *c) {
            members.clear();
        }
    }
    out
}

/// The individuals each of `classes` holds of, as their nominals. A class
/// expression the ontology need not hold any subsumption for is asked of a
/// copy of the state, as the superclass of a class of its own.
fn members_of(classes: &[ConceptId], state: &ReasonerState) -> HashMap<ConceptId, Vec<ConceptId>> {
    let top = state.interner.top();
    let mut classes = classes.to_vec();
    classes.sort();
    classes.dedup();
    let mut out: HashMap<ConceptId, Vec<ConceptId>> = HashMap::new();
    let mut asked: Vec<(ConceptId, ConceptId)> = Vec::new();
    let mut copy = state.clone();
    for c in classes {
        if c == top {
            out.insert(c, nominals(state));
        } else if matches!(state.interner.concept_data(c), ConceptData::AtomicConcept(_) | ConceptData::Nominal(_)) {
            out.insert(c, below(state, c));
        } else {
            let q = copy.interner.intern_concept(ConceptData::AtomicConcept(format!("{QUERY_NS}{}", asked.len())));
            asked.push((c, q));
        }
    }
    if !asked.is_empty() {
        let axioms: whelk::whelk::model::HashSet<ConceptInclusion> =
            asked.iter().map(|&(c, q)| ConceptInclusion { subclass: c, superclass: q }).collect();
        let copy = super::whelk::saturate_append(&axioms, &copy);
        for (c, q) in asked {
            out.insert(c, below(&copy, q));
        }
    }
    out
}

/// Fire `rule` on every binding of its body: its head's property atoms add to
/// `facts`, its same-individual atoms to `equal`, its class atoms to `kinds`.
fn fire(
    rule: &Rule<ConceptId, ConceptId>,
    closed: &Facts,
    members: &HashMap<ConceptId, Vec<ConceptId>>,
    state: &ReasonerState,
    facts: &mut Vec<Fact>,
    equal: &mut Vec<(ConceptId, ConceptId)>,
    kinds: &mut Vec<(ConceptId, ConceptId)>,
) -> Result<()> {
    let mut bindings: Vec<HashMap<String, ConceptId>> = Vec::new();
    matches(&rule.body, &mut HashMap::new(), closed, members, state, &mut bindings);
    for binding in bindings {
        let value = |t: &Term<ConceptId>| -> Result<ConceptId> {
            match t {
                Term::Ind(n) => Ok(*n),
                Term::Var(v) => match binding.get(v) {
                    Some(&n) => Ok(n),
                    None => bail!("key not found: Variable({v})"),
                },
            }
        };
        for atom in &rule.head {
            match atom {
                RuleAtom::Class(c, t) => kinds.push((value(t)?, *c)),
                RuleAtom::Property(r, s, o) => facts.push((*r, value(s)?, value(o)?)),
                RuleAtom::Same(a, b) => equal.push((value(a)?, value(b)?)),
            }
        }
    }
    Ok(())
}

/// Every binding of `atoms`' variables, extending `binding`, under which each
/// atom holds.
fn matches(
    atoms: &[RuleAtom<ConceptId, ConceptId>],
    binding: &mut HashMap<String, ConceptId>,
    facts: &Facts,
    members: &HashMap<ConceptId, Vec<ConceptId>>,
    state: &ReasonerState,
    out: &mut Vec<HashMap<String, ConceptId>>,
) {
    let Some((atom, rest)) = atoms.split_first() else {
        out.push(binding.clone());
        return;
    };
    let bound = |binding: &HashMap<String, ConceptId>, t: &Term<ConceptId>| match t {
        Term::Ind(n) => Some(*n),
        Term::Var(v) => binding.get(v).copied(),
    };
    // Each way the atom holds, as the values of its two arguments; a class
    // atom's second is its first.
    let options: Vec<(ConceptId, ConceptId)> = match atom {
        RuleAtom::Class(c, t) => {
            let all = members.get(c).map_or(&[][..], Vec::as_slice);
            match bound(binding, t) {
                Some(n) => all.iter().filter(|&&m| m == n).map(|&m| (m, m)).collect(),
                None => all.iter().map(|&m| (m, m)).collect(),
            }
        }
        RuleAtom::Property(r, s, o) => match (bound(binding, s), bound(binding, o)) {
            (Some(a), Some(b)) => if facts.all.contains(&(*r, a, b)) { vec![(a, b)] } else { Vec::new() },
            (Some(a), None) => facts.objects(*r, a).into_iter().map(|b| (a, b)).collect(),
            (None, Some(b)) => facts.subjects(*r, b).into_iter().map(|a| (a, b)).collect(),
            (None, None) => facts.pairs(*r).to_vec(),
        },
        RuleAtom::Same(x, y) => match (bound(binding, x), bound(binding, y)) {
            (Some(a), Some(b)) => if equals(state, a).contains(&b) { vec![(a, b)] } else { Vec::new() },
            (Some(a), None) => equals(state, a).into_iter().map(|b| (a, b)).collect(),
            (None, Some(b)) => equals(state, b).into_iter().map(|a| (a, b)).collect(),
            (None, None) => nominals(state).into_iter().flat_map(|a| equals(state, a).into_iter().map(move |b| (a, b))).collect(),
        },
    };
    let (first, second) = match atom {
        RuleAtom::Class(_, t) => (t, t),
        RuleAtom::Property(_, s, o) => (s, o),
        RuleAtom::Same(x, y) => (x, y),
    };
    for (a, b) in options {
        let mut set: Vec<String> = Vec::new();
        let mut consistent = true;
        for (t, value) in [(first, a), (second, b)] {
            if let Term::Var(v) = t {
                match binding.get(v) {
                    Some(&n) if n != value => consistent = false,
                    Some(_) => {}
                    None => {
                        binding.insert(v.clone(), value);
                        set.push(v.clone());
                    }
                }
            }
        }
        if consistent {
            matches(rest, binding, facts, members, state, out);
        }
        for v in set {
            binding.remove(&v);
        }
    }
}
