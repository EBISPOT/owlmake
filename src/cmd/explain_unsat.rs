//! Which unsatisfiable classes `explain -M unsatisfiability` explains, and the
//! list `--unsatisfiable list` writes.
//!
//! - `root`: the unsatisfiable classes no other unsatisfiable class's told
//!   definition explains. A class depends on the unsatisfiable classes named in
//!   its told superclasses and equivalents ([`dependencies`]); a class with none
//!   is a root, and so is every class of a cycle of dependencies.
//! - `most_general`: the unsatisfiable classes with no unsatisfiable class among
//!   their told superclasses, climbed through the structural class hierarchy
//!   ([`Hierarchy`]). Every class with no told subclass is a told superclass of
//!   `owl:Nothing` there, so a class whose told superclasses reach `owl:Nothing`
//!   climbs back to itself: that climb never ends, and is an error.
//! - `list`: each unsatisfiable class's CURIE in the built-in OBO context.
//!
//! Both searches for cycles number the classes they meet by their depth in the
//! search, so a cycle entered at a deeper class than one it already holds is
//! cut in two. Classes are visited, and explained, in the order the Java hash
//! sets holding them iterate in ([`passed_through`]).

use std::collections::{HashMap, HashSet};

use horned_owl::model::{ClassExpression as CE, Component, RcStr};

use crate::cmd::explain_blackbox::{bucket, copy_capacity, presized_capacity};
use crate::io::natural_order::{iri_cmp, str_cmp};
use crate::model::Model;
use crate::owlapi_hash::class_hash;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// `classes` in the order a Java hash set of them iterates in, when they came
/// to it through hash sets of `caps` slots in turn, the last being its own: by
/// the bucket of each class's hash in the last table, a tie by the bucket in
/// the table before, and so on; a tie in every table by IRI.
fn passed_through<'a>(classes: impl IntoIterator<Item = &'a str>, caps: &[usize]) -> Vec<&'a str> {
    let mut v: Vec<&str> = classes.into_iter().collect();
    v.sort_by(|a, b| {
        let (ha, hb) = (class_hash(a), class_hash(b));
        caps.iter()
            .rev()
            .map(|&cap| bucket(ha, cap).cmp(&bucket(hb, cap)))
            .find(|o| o.is_ne())
            .unwrap_or_else(|| iri_cmp(a, b))
    });
    v.dedup();
    v
}

/// The table a reasoner's node of `n` classes holds them in: four slots,
/// doubling as it fills.
fn node_capacity(n: usize) -> usize {
    presized_capacity(4, n)
}

/// The cycles of `parents` among `keys`, as a depth-first search from each key
/// not yet reached finds them: a class is numbered by its depth in the search
/// that meets it, and a class whose lowest reachable number is its own closes a
/// cycle of the classes above it on the search's stack. Only cycles of more than
/// one class count.
fn cycles<'a>(keys: &[&'a str], parents: &HashMap<&'a str, Vec<&'a str>>) -> Vec<HashSet<&'a str>> {
    struct Search<'a, 'p> {
        parents: &'p HashMap<&'a str, Vec<&'a str>>,
        index: HashMap<&'a str, usize>,
        low: HashMap<&'a str, usize>,
        stack: Vec<&'a str>,
        on_stack: HashSet<&'a str>,
    }
    impl<'a> Search<'a, '_> {
        fn visit(&mut self, c: &'a str, depth: usize, reached: &mut HashSet<&'a str>, out: &mut Vec<HashSet<&'a str>>) {
            reached.insert(c);
            self.index.insert(c, depth);
            self.low.insert(c, depth);
            self.stack.push(c);
            self.on_stack.insert(c);
            let parents = self.parents;
            for &p in parents.get(c).into_iter().flatten() {
                if !self.index.contains_key(p) {
                    self.visit(p, depth + 1, reached, out);
                    let l = self.low[c].min(self.low[p]);
                    self.low.insert(c, l);
                } else if self.on_stack.contains(p) {
                    let l = self.low[c].min(self.index[p]);
                    self.low.insert(c, l);
                }
            }
            if self.low[c] == self.index[c] {
                let mut cycle = HashSet::new();
                while let Some(x) = self.stack.pop() {
                    self.on_stack.remove(x);
                    cycle.insert(x);
                    if x == c {
                        break;
                    }
                }
                if cycle.len() > 1 {
                    out.push(cycle);
                }
            }
        }
    }
    let mut reached = HashSet::new();
    let mut out = Vec::new();
    for &k in keys {
        if reached.contains(k) {
            continue;
        }
        let mut search = Search {
            parents,
            index: HashMap::new(),
            low: HashMap::new(),
            stack: Vec::new(),
            on_stack: HashSet::new(),
        };
        search.visit(k, 0, &mut reached, &mut out);
    }
    out
}

/// The class expressions each named class is told to be a subclass of or
/// equivalent to: the superclass of each `SubClassOf` whose subclass it is, and
/// the other members of each `EquivalentClasses` it is a member of.
fn told_definitions(model: &Model) -> HashMap<&str, Vec<&CE<RcStr>>> {
    let mut out: HashMap<&str, Vec<&CE<RcStr>>> = HashMap::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(sc) => {
                if let CE::Class(c) = &sc.sub {
                    out.entry(c.0.as_ref()).or_default().push(&sc.sup);
                }
            }
            Component::EquivalentClasses(eq) => {
                for m in &eq.0 {
                    if let CE::Class(c) = m {
                        out.entry(c.0.as_ref()).or_default().extend(eq.0.iter().filter(|o| *o != m));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The unsatisfiable classes a class told to be below `ce` depends on: a named
/// class that is unsatisfiable; the unsatisfiable named filler of an existential,
/// universal, minimum or exact restriction, or what an anonymous filler depends
/// on; what each conjunct of an intersection depends on; and, when no disjunct
/// of a union is satisfiable, every disjunct. A complement, an enumeration, a
/// maximum, a value or self restriction and a data restriction depend on
/// nothing.
fn dependencies<'a>(
    ce: &'a CE<RcStr>,
    unsat: &dyn Fn(&str) -> bool,
    satisfiable: &mut dyn FnMut(&CE<RcStr>) -> bool,
    out: &mut Vec<&'a str>,
) {
    let filler = |f: &'a CE<RcStr>, satisfiable: &mut dyn FnMut(&CE<RcStr>) -> bool, out: &mut Vec<&'a str>| match f {
        CE::Class(c) => {
            if unsat(c.0.as_ref()) {
                out.push(c.0.as_ref());
            }
        }
        _ => dependencies(f, unsat, satisfiable, out),
    };
    match ce {
        CE::Class(c) => {
            if unsat(c.0.as_ref()) {
                out.push(c.0.as_ref());
            }
        }
        CE::ObjectSomeValuesFrom { bce, .. }
        | CE::ObjectAllValuesFrom { bce, .. }
        | CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. } => filler(bce, satisfiable, out),
        CE::ObjectIntersectionOf(ops) => {
            for op in ops {
                filler(op, satisfiable, out);
            }
        }
        CE::ObjectUnionOf(ops) => {
            // The named disjuncts are the cheap ones to ask about, and a set of
            // class expressions lists them first.
            let named_satisfiable =
                ops.iter().any(|op| matches!(op, CE::Class(c) if !unsat(c.0.as_ref())));
            if named_satisfiable || ops.iter().any(|op| !matches!(op, CE::Class(_)) && satisfiable(op)) {
                return;
            }
            for op in ops {
                match op {
                    CE::Class(c) => out.push(c.0.as_ref()),
                    _ => dependencies(op, unsat, satisfiable, out),
                }
            }
        }
        _ => {}
    }
}

/// The root unsatisfiable classes of `unsat`, in the order they are
/// explained: the classes with no dependency in the order the reasoner's node
/// lists them, then the classes of each cycle, as the hash set gathering them
/// iterates. `satisfiable` decides an anonymous class expression; it is asked
/// only about the anonymous disjuncts of a union whose named disjuncts are all
/// unsatisfiable.
pub(crate) fn roots(model: &Model, unsat: &[String], satisfiable: &mut dyn FnMut(&CE<RcStr>) -> bool) -> Vec<String> {
    let unsat_set: HashSet<&str> = unsat.iter().map(String::as_str).collect();
    let is_unsat = |c: &str| c == OWL_NOTHING || unsat_set.contains(c);
    let told = told_definitions(model);
    // The reasoner's node of unsatisfiable classes, owl:Nothing among them, and
    // the hash map keyed by them in the node's order.
    let mut classes: Vec<&str> = unsat_set.iter().copied().collect();
    classes.push(OWL_NOTHING);
    let node_cap = node_capacity(classes.len());
    let node = passed_through(classes.iter().copied(), &[node_cap]);
    let keys = passed_through(node.iter().copied(), &[node_cap, presized_capacity(16, node.len())]);
    let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut candidates: Vec<&str> = Vec::new();
    for &c in &node {
        let mut deps = Vec::new();
        for ce in told.get(c).into_iter().flatten() {
            dependencies(ce, &is_unsat, satisfiable, &mut deps);
        }
        let n = deps.iter().collect::<HashSet<_>>().len();
        let deps = passed_through(deps, &[presized_capacity(16, n), copy_capacity(n)]);
        if deps.is_empty() {
            candidates.push(c);
        }
        parents.insert(c, deps);
    }
    let held = candidates.len();
    candidates.retain(|&c| c != OWL_NOTHING);
    let mut in_cycles: Vec<&str> = Vec::new();
    for cycle in cycles(&keys, &parents) {
        let mut members: Vec<&str> = cycle.into_iter().collect();
        members.sort_by(|a, b| iri_cmp(a, b));
        for c in members {
            if !candidates.contains(&c) && !in_cycles.contains(&c) {
                in_cycles.push(c);
            }
        }
    }
    // The set's table grows to its largest size: every candidate, owl:Nothing
    // among them, or what is left once owl:Nothing goes and the cycles come in.
    let cap = presized_capacity(16, held.max(candidates.len() + in_cycles.len()));
    let rank: HashMap<&str, usize> = candidates.iter().chain(&in_cycles).enumerate().map(|(i, &c)| (c, i)).collect();
    let mut out: Vec<&str> = rank.keys().copied().collect();
    out.sort_by_key(|c| (bucket(class_hash(c), cap), rank[c]));
    out.into_iter().map(str::to_string).collect()
}

/// The structural class hierarchy: each class's told parents, the nodes the
/// cycles among them make, and the classes with no told subclass.
struct Hierarchy<'a> {
    parents: HashMap<&'a str, Vec<&'a str>>,
    /// The node each class of a cycle belongs to; any other class is a node of
    /// its own.
    node: HashMap<&'a str, usize>,
    nodes: Vec<HashSet<&'a str>>,
    /// The classes `owl:Nothing` has as its parents: those with no told
    /// subclass, outside `owl:Nothing`'s own node, and the cycles none of whose
    /// classes has a told subclass outside it.
    leaves: Vec<&'a str>,
}

impl<'a> Hierarchy<'a> {
    fn of(model: &'a Model) -> Hierarchy<'a> {
        let parents = crate::cmd::reason::told_parents(model);
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut signature: HashSet<&str> = HashSet::new();
        for ac in model.ont.iter() {
            if let Component::DeclareClass(dc) = &ac.component {
                signature.insert(dc.0 .0.as_ref());
            }
        }
        for (&c, ps) in &parents {
            signature.insert(c);
            for &p in ps {
                signature.insert(p);
                children.entry(p).or_default().push(c);
            }
        }
        let keys = passed_through(signature.iter().copied(), &[presized_capacity(16, signature.len())]);
        let ordered: HashMap<&str, Vec<&str>> = parents
            .iter()
            .map(|(&c, ps)| {
                let n = ps.iter().collect::<HashSet<_>>().len();
                (c, passed_through(ps.iter().copied(), &[presized_capacity(16, n)]))
            })
            .collect();
        let found = cycles(&keys, &ordered);
        let mut node = HashMap::new();
        let mut nodes = Vec::new();
        for cycle in found {
            let i = nodes.len();
            for &c in &cycle {
                node.insert(c, i);
            }
            nodes.push(cycle);
        }
        let bottom: HashSet<&str> = node.get(OWL_NOTHING).map_or_else(|| HashSet::from([OWL_NOTHING]), |&i| nodes[i].clone());
        let mut leaves: Vec<&str> = keys
            .iter()
            .copied()
            .filter(|c| children.get(c).is_none_or(|ch| ch.is_empty() || ch.contains(&OWL_NOTHING)))
            .filter(|c| !bottom.contains(c))
            .collect();
        for cycle in &nodes {
            if cycle.contains(OWL_THING) || cycle.contains(OWL_NOTHING) {
                continue;
            }
            let below_only_itself = cycle.iter().all(|c| {
                children.get(c).into_iter().flatten().all(|ch| cycle.contains(ch) || bottom.contains(ch))
            });
            if below_only_itself {
                leaves.extend(cycle.iter().copied());
            }
        }
        Hierarchy { parents: ordered, node, nodes, leaves }
    }

    /// The classes of `c`'s node.
    fn members(&self, c: &'a str) -> Vec<&'a str> {
        match self.node.get(c) {
            Some(&i) => self.nodes[i].iter().copied().collect(),
            None => vec![c],
        }
    }

    /// A node's identity: the index of its cycle, or the class itself.
    fn key(&self, c: &'a str) -> (usize, &'a str) {
        match self.node.get(c) {
            Some(&i) => (i, ""),
            None => (usize::MAX, c),
        }
    }

    /// The parents of `c`'s node: its classes' told parents outside it, and for
    /// `owl:Nothing`'s node the classes with no told subclass. `owl:Thing`'s
    /// node has none.
    fn node_parents(&self, c: &'a str) -> Vec<&'a str> {
        let members = self.members(c);
        if members.contains(&OWL_THING) {
            return Vec::new();
        }
        let mut out: Vec<&str> = members
            .iter()
            .flat_map(|m| self.parents.get(m).into_iter().flatten().copied())
            .filter(|p| !members.contains(p))
            .collect();
        if members.contains(&OWL_NOTHING) {
            out.extend(self.leaves.iter().copied());
        }
        out
    }

    /// Whether climbing from `c`'s node reaches a node holding a class `hit`
    /// picks out, `c`'s own node aside; an error when the climb comes back to a
    /// node it is still climbing from, where it would never end.
    fn reaches(
        &self,
        c: &'a str,
        hit: &dyn Fn(&str) -> bool,
        state: &mut HashMap<(usize, &'a str), Option<bool>>,
    ) -> Result<bool, &'a str> {
        let k = self.key(c);
        match state.get(&k) {
            Some(Some(found)) => return Ok(*found),
            Some(None) => return Err(c),
            None => {}
        }
        state.insert(k, None);
        let mut found = false;
        for p in self.node_parents(c) {
            found |= self.members(p).iter().any(|m| hit(m));
            found |= self.reaches(p, hit, state)?;
        }
        state.insert(k, Some(found));
        Ok(found)
    }
}

/// The most general unsatisfiable classes of `unsat`: those with no
/// unsatisfiable class among their structural superclasses, in the order the
/// hash set gathering them iterates. An error names a class whose climb through
/// its superclasses never ends.
pub(crate) fn most_general(model: &Model, unsat: &[String]) -> anyhow::Result<Vec<String>> {
    let hierarchy = Hierarchy::of(model);
    let unsat_set: HashSet<&str> = unsat.iter().map(String::as_str).collect();
    // The reasoner's node, owl:Nothing among it, copied without owl:Nothing and
    // copied again.
    let n = unsat_set.len();
    let caps = [node_capacity(n + 1), copy_capacity(n + 1), copy_capacity(n)];
    let order = passed_through(unsat_set.iter().copied(), &caps);
    let mut state = HashMap::new();
    let mut general = Vec::new();
    for &c in &order {
        match hierarchy.reaches(c, &|m| unsat_set.contains(m), &mut state) {
            Ok(false) => general.push(c),
            Ok(true) => {}
            Err(at) => anyhow::bail!(
                "--unsatisfiable most_general: climbing the told superclasses of {c} never ends: it comes back to \
                 {at}, as a class whose told superclasses reach owl:Nothing does, owl:Nothing's own being every class \
                 with no told subclass"
            ),
        }
    }
    let mut caps = caps.to_vec();
    caps.push(presized_capacity(16, general.len()));
    Ok(passed_through(general, &caps).into_iter().map(str::to_string).collect())
}

/// The lines `--unsatisfiable list` writes: each class's CURIE in the built-in
/// OBO context, by the longest namespace its IRI begins with — every occurrence
/// of that namespace in the IRI made the prefix — or its IRI when none does;
/// sorted.
pub(crate) fn curie_list(unsat: &[String]) -> Vec<String> {
    let mut prefixes: Vec<&(String, String)> = crate::report::obo_context_prefixes().iter().collect();
    prefixes.sort_by_key(|(_, ns)| std::cmp::Reverse(ns.len()));
    let mut out: Vec<String> = unsat
        .iter()
        .map(|iri| match prefixes.iter().find(|(_, ns)| iri.starts_with(ns.as_str())) {
            Some((name, ns)) => iri.replace(ns.as_str(), &format!("{name}:")),
            None => iri.clone(),
        })
        .collect();
    out.sort_by(|a, b| str_cmp(a, b));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cycle_entered_below_a_class_it_holds_is_cut_in_two() {
        // x → p → r → x and x → q → r: one cycle of four. The search meets r at
        // depth 2 under p, and q at depth 1, so q's way back through r numbers
        // no lower than q itself and q closes a cycle of its own.
        let parents: HashMap<&str, Vec<&str>> =
            HashMap::from([("x", vec!["p", "q"]), ("p", vec!["r"]), ("q", vec!["r"]), ("r", vec!["x"])]);
        let found = cycles(&["x"], &parents);
        assert_eq!(found, vec![HashSet::from(["x", "p", "r"])]);
    }
}
