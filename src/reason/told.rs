//! The told class hierarchy, as the structural reasoner holds it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::model::Model;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The told class hierarchy. A class's parents are the classes [the
/// structural reasoner](crate::cmd::reason) is told it is a subclass of; the
/// classes of a told cycle make one node, any other class a node of its own.
/// The top node holds `owl:Thing`, the bottom node `owl:Nothing`.
pub(crate) struct Told {
    parents: HashMap<String, Vec<String>>,
    children: HashMap<String, Vec<String>>,
    /// The node of each class in a told cycle.
    cycle: HashMap<String, Rc<Vec<String>>>,
    pub(crate) top: Rc<Vec<String>>,
    bottom: Rc<Vec<String>>,
    /// The classes directly below the top node: those told no parent but
    /// `owl:Thing`'s node, and each cycle told none outside itself.
    below_top: HashSet<String>,
    /// The classes directly above the bottom node: those told no child but
    /// `owl:Nothing`'s node, and each cycle told none outside itself.
    above_bottom: HashSet<String>,
    signature: Vec<String>,
    /// Each node's ancestors, as they are asked for.
    ancestors: RefCell<HashMap<String, Rc<HashSet<String>>>>,
}

impl Told {
    pub(crate) fn of(model: &Model) -> Told {
        let mut parents: HashMap<String, Vec<String>> = HashMap::new();
        for (c, ps) in crate::cmd::reason::told_parents(model) {
            let entry = parents.entry(c.to_string()).or_default();
            for p in ps {
                if !entry.iter().any(|e| e == p) {
                    entry.push(p.to_string());
                }
            }
        }
        let mut signature: Vec<String> = model
            .ont
            .iter()
            .flat_map(|ac| crate::sig::typed_signature(&ac.component))
            .filter(|(k, _)| *k == crate::sig::kind::CLASS)
            .map(|(_, iri)| iri)
            .chain([OWL_THING.to_string(), OWL_NOTHING.to_string()])
            .collect();
        signature.sort();
        signature.dedup();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        for (c, ps) in &parents {
            for p in ps {
                children.entry(p.clone()).or_default().push(c.clone());
            }
        }
        let mut cycle: HashMap<String, Rc<Vec<String>>> = HashMap::new();
        for scc in strongly_connected(&signature, &parents) {
            if scc.len() > 1 {
                let scc = Rc::new(scc);
                for c in scc.iter() {
                    cycle.insert(c.clone(), scc.clone());
                }
            }
        }
        let node = |c: &str| -> Rc<Vec<String>> {
            cycle.get(c).cloned().unwrap_or_else(|| Rc::new(vec![c.to_string()]))
        };
        let top = node(OWL_THING);
        let bottom = node(OWL_NOTHING);
        let no_parents: Vec<String> = Vec::new();
        let mut below_top: HashSet<String> = HashSet::new();
        let mut above_bottom: HashSet<String> = HashSet::new();
        for c in &signature {
            let ps = parents.get(c).unwrap_or(&no_parents);
            if ps.is_empty() || ps.iter().any(|p| p == OWL_THING) {
                below_top.insert(c.clone());
            }
            let cs = children.get(c).unwrap_or(&no_parents);
            if cs.is_empty() || cs.iter().any(|k| k == OWL_NOTHING) {
                above_bottom.insert(c.clone());
            }
        }
        let mut cycles: Vec<&Rc<Vec<String>>> = cycle.values().collect();
        cycles.sort();
        cycles.dedup();
        for scc in cycles {
            if scc.iter().any(|c| c == OWL_THING || c == OWL_NOTHING) {
                continue;
            }
            let outside = |links: &HashMap<String, Vec<String>>, end: &Rc<Vec<String>>| {
                scc.iter().any(|c| {
                    links.get(c).into_iter().flatten().any(|l| !scc.contains(l) && !end.contains(l))
                })
            };
            if !outside(&parents, &top) {
                below_top.extend(scc.iter().cloned());
            }
            if !outside(&children, &bottom) {
                above_bottom.extend(scc.iter().cloned());
            }
        }
        below_top.retain(|c| !top.contains(c));
        above_bottom.retain(|c| !bottom.contains(c));
        Told {
            parents,
            children,
            cycle,
            top,
            bottom,
            below_top,
            above_bottom,
            signature,
            ancestors: RefCell::new(HashMap::new()),
        }
    }

    pub(crate) fn node(&self, c: &str) -> Rc<Vec<String>> {
        self.cycle.get(c).cloned().unwrap_or_else(|| Rc::new(vec![c.to_string()]))
    }

    /// The nodes directly above `c`'s: the node of each class its classes are
    /// told a subclass of outside it, none of them reduced away, or the top
    /// node when they are told none. The top node has none above it.
    pub(crate) fn super_nodes(&self, c: &str) -> Vec<Rc<Vec<String>>> {
        let start = self.node(c);
        if start.iter().any(|m| self.top.contains(m)) {
            return Vec::new();
        }
        let mut out: Vec<Rc<Vec<String>>> = Vec::new();
        for m in start.iter() {
            for p in self.parents.get(m).into_iter().flatten() {
                if start.contains(p) {
                    continue;
                }
                let n = self.node(p);
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

    /// The types the structural reasoner gives an individual told it is an
    /// instance of `asserted`: the node of each class, and under `!direct`
    /// every class above it, the top node's included.
    pub(crate) fn types(&self, asserted: &[String], direct: bool) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in asserted {
            out.extend(self.node(c).iter().cloned());
            if !direct {
                out.extend(self.ancestors_of(c).iter().cloned());
                if !self.top.contains(c) {
                    out.extend(self.top.iter().cloned());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The classes of the nodes above `c`'s, as the told parents reach them:
    /// the walk goes no higher than the top node, and from the bottom node it
    /// reaches every class.
    pub(crate) fn ancestors_of(&self, c: &str) -> Rc<HashSet<String>> {
        let start = self.node(c);
        if start.iter().any(|m| self.top.contains(m)) {
            return Rc::new(HashSet::new());
        }
        if let Some(found) = self.ancestors.borrow().get(&start[0]) {
            return found.clone();
        }
        let mut out: HashSet<String> = HashSet::new();
        let mut pending: Vec<String> = start.to_vec();
        let mut walked: HashSet<String> = HashSet::new();
        while let Some(x) = pending.pop() {
            if !walked.insert(x.clone()) {
                continue;
            }
            if self.top.contains(&x) {
                continue;
            }
            if self.bottom.contains(&x) {
                out.extend(self.signature.iter().filter(|s| !self.bottom.contains(s)).cloned());
                continue;
            }
            for p in self.parents.get(&x).into_iter().flatten() {
                if start.contains(p) {
                    continue;
                }
                for m in self.node(p).iter() {
                    out.insert(m.clone());
                    pending.push(m.clone());
                }
            }
        }
        out.retain(|m| !start.contains(m));
        let out = Rc::new(out);
        self.ancestors.borrow_mut().insert(start[0].clone(), out.clone());
        out
    }

    /// Whether `x` lies strictly above `y`: every node lies above the bottom
    /// node, the top node above every other, and otherwise `x` is among the
    /// classes `y`'s told parents reach.
    pub(crate) fn above(&self, y: &str, x: &str) -> bool {
        if y == x {
            return false;
        }
        if self.bottom.iter().any(|c| c == y) {
            return !self.bottom.iter().any(|c| c == x);
        }
        let in_top = |c: &str| self.top.iter().any(|t| t == c);
        if in_top(x) {
            return !in_top(y);
        }
        self.ancestors_of(y).contains(x)
    }

    /// The nodes directly below `node`: those of its classes' told children,
    /// with the classes directly below the top node under the top node, and
    /// the bottom node under a node with a class directly above it.
    pub(crate) fn sub_nodes(&self, node: &[String]) -> Vec<Vec<String>> {
        if node.iter().any(|c| self.bottom.contains(c)) {
            return Vec::new();
        }
        let mut below: Vec<&String> = node
            .iter()
            .flat_map(|c| self.children.get(c).into_iter().flatten())
            .filter(|k| !node.contains(k))
            .collect();
        if node.iter().any(|c| self.top.contains(c)) {
            let mut extra: Vec<&String> = self.below_top.iter().collect();
            extra.sort();
            below.extend(extra);
        }
        let mut out: Vec<Vec<String>> = Vec::new();
        if node.iter().any(|c| self.above_bottom.contains(c)) {
            out.push(self.bottom.to_vec());
        }
        for c in below {
            let members = self.node(c).to_vec();
            if !out.contains(&members) {
                out.push(members);
            }
        }
        out
    }
}

/// The strongly connected components of the graph `edges` makes over
/// `nodes`, each sorted.
pub(crate) fn strongly_connected(nodes: &[String], edges: &HashMap<String, Vec<String>>) -> Vec<Vec<String>> {
    struct State<'a> {
        index: HashMap<&'a str, usize>,
        low: HashMap<&'a str, usize>,
        on_stack: HashSet<&'a str>,
        stack: Vec<&'a str>,
        next: usize,
        out: Vec<Vec<String>>,
    }
    let none: Vec<String> = Vec::new();
    let mut st = State {
        index: HashMap::new(),
        low: HashMap::new(),
        on_stack: HashSet::new(),
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    for root in nodes {
        if st.index.contains_key(root.as_str()) {
            continue;
        }
        // An explicit stack of (node, next edge to follow).
        let mut work: Vec<(&str, usize)> = vec![(root.as_str(), 0)];
        while let Some(&mut (v, ref mut i)) = work.last_mut() {
            if *i == 0 && !st.index.contains_key(v) {
                st.index.insert(v, st.next);
                st.low.insert(v, st.next);
                st.next += 1;
                st.stack.push(v);
                st.on_stack.insert(v);
            }
            let out_edges = edges.get(v).unwrap_or(&none);
            if *i < out_edges.len() {
                let w = out_edges[*i].as_str();
                *i += 1;
                if !st.index.contains_key(w) {
                    work.push((w, 0));
                } else if st.on_stack.contains(w) {
                    let lw = st.index[w];
                    let lv = st.low[v];
                    st.low.insert(v, lv.min(lw));
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                let lv = st.low[v];
                let lp = st.low[parent];
                st.low.insert(parent, lp.min(lv));
            }
            if st.low[v] == st.index[v] {
                let mut scc: Vec<String> = Vec::new();
                while let Some(w) = st.stack.pop() {
                    st.on_stack.remove(w);
                    scc.push(w.to_string());
                    if w == v {
                        break;
                    }
                }
                scc.sort();
                st.out.push(scc);
            }
        }
    }
    st.out
}
