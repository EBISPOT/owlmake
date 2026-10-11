//! The order the EL reasoner lists its bottom node in.
//!
//! The classes to classify are queued in the order the reasoner's index first
//! names them. The index takes the ontology's declarations and logical axioms in
//! the iteration order of a hash set over all of the ontology's axioms, keyed on
//! [`axiom_hash`]. Within one axiom it converts the parts left to right: subclass
//! before superclass, a restriction's property before its filler, and the members
//! of an n-ary expression or axiom in the document's natural order
//! ([`NaturalOrder`]). It stops at the first construct outside the EL profile. So
//! a class that an axiom names only after such a construct is not queued by that
//! axiom.
//!
//! Each unsatisfiable class enters the bottom node in queue order. The node holds
//! its members in a concurrent hash table keyed on the IRI's string hash
//! ([`ConcurrentTable`]). Listing the node copies the table's members through
//! three hash sets of classes, and each copy reorders by its own buckets
//! ([`class_set_pass`]).

use std::collections::{HashMap, HashSet};

use horned_owl::model::{
    AnnotatedComponent, ClassExpression as CE, Component, IRI, Individual, ObjectPropertyExpression as OPE,
    RcStr,
};

use crate::model::Onto;
use crate::io::natural_order::NaturalOrder;
use crate::owlapi_hash::{axiom_hash, class_hash, java_hashset_capacity, java_string_hash};

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The index stopped converting an axiom at a construct outside the profile.
struct Unsupported;

type Walk = Result<(), Unsupported>;

/// Whether a component is an axiom of the ontology, rather than its header.
fn is_axiom(c: &Component<RcStr>) -> bool {
    !matches!(
        c,
        Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::Import(_)
            | Component::OntologyAnnotation(_)
    )
}

/// Whether the index takes an axiom at all: declarations and logical axioms.
fn is_indexed(c: &Component<RcStr>) -> bool {
    is_axiom(c)
        && !matches!(
            c,
            Component::AnnotationAssertion(_)
                | Component::SubAnnotationPropertyOf(_)
                | Component::AnnotationPropertyDomain(_)
                | Component::AnnotationPropertyRange(_)
        )
}

/// Members of an n-ary expression or axiom in the order the index converts
/// them: sorted in the document's natural order, duplicates dropped.
fn sorted_members(v: &[CE<RcStr>], order: NaturalOrder) -> Vec<&CE<RcStr>> {
    let mut out: Vec<&CE<RcStr>> = v.iter().collect();
    out.sort_by(|a, b| order.ce(a, b));
    out.dedup_by(|a, b| order.ce(a, b).is_eq());
    out
}

struct Queue {
    seen: HashSet<IRI<RcStr>>,
    order: Vec<IRI<RcStr>>,
    natural: NaturalOrder,
}

impl Queue {
    fn class(&mut self, iri: &IRI<RcStr>) {
        let s: &str = iri.as_ref();
        if s == OWL_THING || s == OWL_NOTHING {
            return;
        }
        if self.seen.insert(iri.clone()) {
            self.order.push(iri.clone());
        }
    }

    fn property(&mut self, ope: &OPE<RcStr>) -> Walk {
        match ope {
            OPE::ObjectProperty(_) => Ok(()),
            OPE::InverseObjectProperty(_) => Err(Unsupported),
        }
    }

    fn individual(&mut self, i: &Individual<RcStr>) -> Walk {
        match i {
            Individual::Named(_) => Ok(()),
            Individual::Anonymous(_) => Err(Unsupported),
        }
    }

    fn individuals(&mut self, v: &[Individual<RcStr>]) -> Walk {
        for i in v {
            self.individual(i)?;
        }
        Ok(())
    }

    fn members(&mut self, v: &[CE<RcStr>]) -> Walk {
        for m in sorted_members(v, self.natural) {
            self.ce(m)?;
        }
        Ok(())
    }

    fn ce(&mut self, ce: &CE<RcStr>) -> Walk {
        match ce {
            CE::Class(c) => {
                self.class(&c.0);
                Ok(())
            }
            CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => self.members(v),
            CE::ObjectComplementOf(b) => self.ce(b),
            CE::ObjectOneOf(v) => self.individuals(v),
            CE::ObjectSomeValuesFrom { ope, bce } => {
                self.property(ope)?;
                self.ce(bce)
            }
            CE::ObjectHasValue { ope, i } => {
                self.property(ope)?;
                self.individual(i)
            }
            CE::ObjectHasSelf(ope) => self.property(ope),
            CE::DataHasValue { .. } => Ok(()),
            CE::ObjectAllValuesFrom { .. }
            | CE::ObjectMinCardinality { .. }
            | CE::ObjectMaxCardinality { .. }
            | CE::ObjectExactCardinality { .. }
            | CE::DataSomeValuesFrom { .. }
            | CE::DataAllValuesFrom { .. }
            | CE::DataMinCardinality { .. }
            | CE::DataMaxCardinality { .. }
            | CE::DataExactCardinality { .. } => Err(Unsupported),
        }
    }

    fn axiom(&mut self, c: &Component<RcStr>) -> Walk {
        match c {
            Component::DeclareClass(x) => {
                self.class(&x.0 .0);
                Ok(())
            }
            Component::SubClassOf(x) => {
                self.ce(&x.sub)?;
                self.ce(&x.sup)
            }
            Component::EquivalentClasses(x) => self.members(&x.0),
            Component::DisjointClasses(x) => self.members(&x.0),
            Component::DisjointUnion(x) => {
                self.members(&x.1)?;
                self.class(&(x.0).0);
                Ok(())
            }
            Component::ClassAssertion(x) => {
                self.individual(&x.i)?;
                self.ce(&x.ce)
            }
            Component::ObjectPropertyDomain(x) => {
                self.property(&x.ope)?;
                self.ce(&x.ce)
            }
            Component::ObjectPropertyRange(x) => {
                self.property(&x.ope)?;
                self.ce(&x.ce)
            }
            Component::ObjectPropertyAssertion(x) => {
                self.individual(&x.from)?;
                self.property(&x.ope)?;
                self.individual(&x.to)
            }
            Component::SameIndividual(x) => self.individuals(&x.0),
            Component::DifferentIndividuals(x) => self.individuals(&x.0),
            Component::SubObjectPropertyOf(_)
            | Component::EquivalentObjectProperties(_)
            | Component::TransitiveObjectProperty(_)
            | Component::ReflexiveObjectProperty(_) => Ok(()),
            _ => Err(Unsupported),
        }
    }
}

/// The ontology's declarations and logical axioms in the order the index takes
/// them, each with its bucket: the bucket of [`axiom_hash`] in a hash set sized
/// for every axiom of the ontology. Two in one bucket are taken in hash order,
/// then in component order, so the order is a function of the ontology and its
/// natural order alone.
pub fn index_order(ont: &Onto, order: NaturalOrder) -> Vec<(u32, &AnnotatedComponent<RcStr>)> {
    let total = ont.iter().filter(|ac| is_axiom(&ac.component)).count();
    let cap = java_hashset_capacity(total) as u32;
    let mut indexed: Vec<(u32, i32, &AnnotatedComponent<RcStr>)> = ont
        .iter()
        .filter(|ac| is_indexed(&ac.component))
        .filter_map(|ac| {
            let h = axiom_hash(&ac.component, &ac.ann, order)?;
            let u = h as u32;
            Some(((u ^ (u >> 16)) & (cap - 1), h, ac))
        })
        .collect();
    indexed.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then_with(|| a.2.cmp(b.2)));
    indexed.into_iter().map(|(bucket, _, ac)| (bucket, ac)).collect()
}

/// The classes `axioms` name, in the order the index first meets them.
pub fn queue_of<'a>(axioms: impl IntoIterator<Item = &'a Component<RcStr>>, order: NaturalOrder) -> Vec<IRI<RcStr>> {
    let mut q = Queue { seen: HashSet::new(), order: Vec::new(), natural: order };
    for c in axioms {
        // An axiom cut short still queued what it named before the cut.
        let _ = q.axiom(c);
    }
    q.order
}

/// The ontology's classes in the order the reasoner queues them for
/// classification, its sets stored in `order`.
pub fn class_queue(ont: &Onto, order: NaturalOrder) -> Vec<IRI<RcStr>> {
    queue_of(index_order(ont, order).into_iter().map(|(_, ac)| &ac.component), order)
}

/// A concurrent hash table filled from one thread: power-of-two bins of
/// `(h ^ (h >>> 16)) & 0x7fffffff`, 16 bins at the first insertion, doubling once
/// the count reaches three quarters of the bins. A bin is a chain in arrival
/// order, and a doubling moves the chain's last run whole and the nodes ahead of
/// it one at a time to the front of their new chain, so that part comes out
/// reversed. A chain of eight in a table of 64 or more bins becomes a tree bin,
/// which takes new members at the front and keeps its order when split; a long
/// chain in a smaller table doubles the table instead.
pub struct ConcurrentTable {
    bins: Vec<Vec<(u32, usize)>>,
    tree: Vec<bool>,
    count: usize,
    size_ctl: usize,
}

const TREEIFY_THRESHOLD: usize = 8;
const UNTREEIFY_THRESHOLD: usize = 6;
const MIN_TREEIFY_CAPACITY: usize = 64;

impl Default for ConcurrentTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ConcurrentTable {
    pub fn new() -> Self {
        ConcurrentTable { bins: Vec::new(), tree: Vec::new(), count: 0, size_ctl: 0 }
    }

    fn spread(h: i32) -> u32 {
        let u = h as u32;
        (u ^ (u >> 16)) & 0x7fff_ffff
    }

    /// Insert key `key` with hash `h`, unless a key equal to it is present.
    pub fn insert(&mut self, h: i32, key: usize, eq: impl Fn(usize) -> bool) {
        if self.bins.is_empty() {
            self.bins = vec![Vec::new(); 16];
            self.tree = vec![false; 16];
            self.size_ctl = 12;
        }
        let hs = Self::spread(h);
        let n = self.bins.len();
        let i = (hs as usize) & (n - 1);
        if self.bins[i].iter().any(|&(eh, ek)| eh == hs && eq(ek)) {
            return;
        }
        if self.bins[i].is_empty() {
            self.bins[i].push((hs, key));
        } else if self.tree[i] {
            self.bins[i].insert(0, (hs, key));
        } else {
            let before = self.bins[i].len();
            self.bins[i].push((hs, key));
            if before >= TREEIFY_THRESHOLD {
                if n < MIN_TREEIFY_CAPACITY {
                    self.presize(n << 1);
                } else {
                    self.tree[i] = true;
                }
            }
        }
        self.count += 1;
        while self.count >= self.size_ctl && self.bins.len() < (1 << 30) {
            self.transfer();
        }
    }

    fn presize(&mut self, size: usize) {
        let want = (size + (size >> 1) + 1).next_power_of_two();
        while want > self.size_ctl && self.bins.len() < (1 << 30) {
            self.transfer();
        }
    }

    fn transfer(&mut self) {
        let n = self.bins.len();
        let bit = n as u32;
        let mut bins = vec![Vec::new(); n * 2];
        let mut tree = vec![false; n * 2];
        for i in 0..n {
            let chain = std::mem::take(&mut self.bins[i]);
            if chain.is_empty() {
                continue;
            }
            if self.tree[i] {
                let (lo, hi): (Vec<_>, Vec<_>) = chain.into_iter().partition(|e| e.0 & bit == 0);
                tree[i] = lo.len() > UNTREEIFY_THRESHOLD;
                tree[i + n] = hi.len() > UNTREEIFY_THRESHOLD;
                bins[i] = lo;
                bins[i + n] = hi;
                continue;
            }
            let mut last_run = 0;
            let mut run_bit = chain[0].0 & bit;
            for (k, e) in chain.iter().enumerate().skip(1) {
                if e.0 & bit != run_bit {
                    run_bit = e.0 & bit;
                    last_run = k;
                }
            }
            let mut lo: Vec<(u32, usize)> = Vec::new();
            let mut hi: Vec<(u32, usize)> = Vec::new();
            for &e in chain[..last_run].iter().rev() {
                if e.0 & bit == 0 {
                    lo.push(e);
                } else {
                    hi.push(e);
                }
            }
            let tail = &chain[last_run..];
            if run_bit == 0 {
                lo.extend_from_slice(tail);
            } else {
                hi.extend_from_slice(tail);
            }
            bins[i] = lo;
            bins[i + n] = hi;
        }
        self.bins = bins;
        self.tree = tree;
        self.size_ctl = (n << 1) - (n >> 1);
    }

    /// The keys in iteration order: bins in index order, each chain front first.
    pub fn keys(&self) -> Vec<usize> {
        self.bins.iter().flat_map(|b| b.iter().map(|&(_, k)| k)).collect()
    }
}

/// One copy of `items` into a hash set of classes with `cap` buckets: bucket
/// order, the incoming order within a bucket.
fn class_set_pass(items: &mut [(i32, usize)], cap: usize) {
    items.sort_by_key(|&(h, _)| {
        let u = h as u32;
        (u ^ (u >> 16)) as usize & (cap - 1)
    });
}

/// Unsatisfiable classes in the order the bottom node lists them. `queue` is
/// [`class_queue`] of the ontology classified; an unsatisfiable class it does not
/// name follows the queued ones in IRI order.
pub fn bottom_node_order(queue: &[IRI<RcStr>], unsat: &[String]) -> Vec<String> {
    let wanted: HashSet<&str> = unsat.iter().map(String::as_str).collect();
    let mut members: Vec<String> = vec![OWL_NOTHING.to_string()];
    let mut placed: HashSet<&str> = HashSet::new();
    for c in queue {
        let s: &str = c.as_ref();
        if wanted.contains(s) && placed.insert(s) {
            members.push(s.to_string());
        }
    }
    let mut rest: Vec<&String> = unsat.iter().filter(|u| !placed.contains(u.as_str())).collect();
    rest.sort();
    rest.dedup();
    members.extend(rest.into_iter().cloned());

    let mut table = ConcurrentTable::new();
    let index: HashMap<&str, usize> = members.iter().enumerate().map(|(i, m)| (m.as_str(), i)).collect();
    for (i, m) in members.iter().enumerate() {
        table.insert(java_string_hash(m), i, |k| k == index[m.as_str()]);
    }
    let mut items: Vec<(i32, usize)> =
        table.keys().into_iter().map(|k| (class_hash(&members[k]), k)).collect();
    let m = items.len();
    // The node's own set, filled from the table; the node's copy of it, grown
    // from four buckets; and the copy sized for its source that drops the
    // bottom class.
    class_set_pass(&mut items, java_hashset_capacity(m));
    let mut grown = 4;
    while m > grown * 3 / 4 {
        grown *= 2;
    }
    class_set_pass(&mut items, grown);
    let sized = ((m as f32 / 0.75) as usize + 1).max(16).next_power_of_two();
    class_set_pass(&mut items, sized);
    items.into_iter().map(|(_, k)| members[k].clone()).filter(|s| s != OWL_NOTHING).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/elk-order")
    }

    fn lines(name: &str) -> Vec<String> {
        std::fs::read_to_string(fixtures().join(name)).unwrap().lines().map(str::to_string).collect()
    }

    /// The table iterates as `java.util.concurrent.ConcurrentHashMap` does when
    /// one thread fills it: `table-orders.txt` holds that map's `values()` after
    /// each `# after N` insertions of `table-keys.txt`, printed by Java 11. The
    /// keys include dense identifier runs, whose hashes differ in single bits,
    /// and blocks of strings with one shared hash, which grow chains of eight
    /// while the table is small and tree bins once it is not.
    #[test]
    fn concurrent_table_iterates_as_the_jdk_map() {
        let keys = lines("table-keys.txt");
        let mut marks: Vec<(usize, Vec<String>)> = Vec::new();
        for l in lines("table-orders.txt") {
            if let Some(n) = l.strip_prefix("# after ") {
                marks.push((n.parse().unwrap(), Vec::new()));
            } else {
                marks.last_mut().unwrap().1.push(l);
            }
        }
        let mut table = ConcurrentTable::new();
        let mut checked = 0;
        for (i, k) in keys.iter().enumerate() {
            table.insert(java_string_hash(k), i, |j| keys[j] == *k);
            if let Some((_, want)) = marks.iter().find(|(n, _)| *n == i + 1) {
                let got: Vec<&String> = table.keys().into_iter().map(|j| &keys[j]).collect();
                assert_eq!(got, want.iter().collect::<Vec<_>>(), "order after {} insertions", i + 1);
                checked += 1;
            }
        }
        assert_eq!(checked, marks.len());
    }

    /// The axioms of `bottom-order.ofn` in the order the reasoner took them, one
    /// per line of `bottom-order.load.txt`, as it traced them.
    fn traced_load() -> Vec<Component<RcStr>> {
        lines("bottom-order.load.txt")
            .iter()
            .map(|l| {
                let doc = format!("Ontology(<http://example.org/one>\n{l}\n)\n");
                let model = crate::api::parse(doc.as_bytes(), crate::io::Format::Functional).unwrap();
                let mut axioms: Vec<Component<RcStr>> =
                    model.ont.iter().map(|ac| ac.component.clone()).filter(is_axiom).collect();
                assert_eq!(axioms.len(), 1, "{l}");
                axioms.pop().unwrap()
            })
            .collect()
    }

    /// The index takes the axioms bucket by bucket: the traced load order and
    /// ours pass through the same buckets in the same sequence. Within a bucket
    /// the reasoner's own order varies from run to run, and `bottom-order.ofn`
    /// has no bucket holding two axioms that each name a class not named before
    /// it, so its queue does not depend on that order.
    #[test]
    fn index_order_takes_the_axioms_bucket_by_bucket() {
        let model = crate::io::load(&fixtures().join("bottom-order.ofn")).unwrap();
        let ours = index_order(&model.ont, model.natural_order());
        let traced = traced_load();
        let total = model.ont.iter().filter(|ac| is_axiom(&ac.component)).count();
        let cap = java_hashset_capacity(total) as u32;
        let traced_buckets: Vec<u32> = traced
            .iter()
            .map(|c| {
                let u = axiom_hash(c, &Default::default(), model.natural_order()).unwrap() as u32;
                (u ^ (u >> 16)) & (cap - 1)
            })
            .collect();
        let our_buckets: Vec<u32> = ours.iter().map(|(b, _)| *b).collect();
        assert_eq!(our_buckets, traced_buckets);
        // The same axioms: the trace writes an n-ary axiom's members sorted,
        // the document in its own order, and the hash does not see the order.
        let order = model.natural_order();
        let mut a: Vec<i32> = ours.iter().map(|(_, ac)| axiom_hash(&ac.component, &ac.ann, order).unwrap()).collect();
        let mut b: Vec<i32> = traced.iter().map(|c| axiom_hash(c, &Default::default(), order).unwrap()).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    /// Fed the axioms in the order the reasoner took them, the walk queues the
    /// classes in the order the reasoner traced as it queued them; and the
    /// ontology's own queue is that order too. `bottom-order.ofn` stops at
    /// universal, cardinality and inverse-property constructs both before and
    /// after the classes it names.
    #[test]
    fn class_queue_is_the_order_the_index_names_classes() {
        let want = lines("bottom-order.queue.txt");
        let model = crate::io::load(&fixtures().join("bottom-order.ofn")).unwrap();
        let walked: Vec<String> =
            queue_of(traced_load().iter(), model.natural_order()).iter().map(|i| i.to_string()).collect();
        assert_eq!(walked, want);
        let queued: Vec<String> = class_queue(&model.ont, model.natural_order()).iter().map(|i| i.to_string()).collect();
        assert_eq!(queued, want);
    }
}
