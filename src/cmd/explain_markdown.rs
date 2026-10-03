//! The markdown report `explain --explanation` writes.
//!
//! Each explanation is a tree under a `## entailment ##` heading. The tree
//! grows from the entailment's subject: an entity's defining axioms become its
//! children, and each child's right-hand-side entities are expanded in turn, so
//! the justification reads as a chain of reasoning; axioms the walk never
//! reaches hang off the root at the end. Every axiom is written in Manchester
//! syntax with each entity as a `[label](IRI)` link.
//!
//! After the explanations comes the axiom impact summary: the justification
//! axioms grouped by how many explanations use them, most-used first, each
//! group sorted, each axiom tagged with the ontology it comes from; and last
//! the list of those ontologies.
//!
//! Wherever the order of the report is a set's iteration order, it is the order
//! of the `java.util.HashSet` holding those objects
//! ([`crate::owlapi_hash`]), and sorts with inconsistent comparators are run
//! exactly as Java's small-array TimSort runs them.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use horned_owl::model::{
    AnnotatedComponent, ClassExpression as CE, Component, Individual,
    ObjectPropertyExpression as OPE, RcStr, SubObjectPropertyExpression as SOPE,
};

use crate::owlapi_hash::{axiom_hash, owl_cmp};
use crate::sig::kind;

type Ax = AnnotatedComponent<RcStr>;
type Entity = (u8, String);

const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// One explanation: the entailment `sub ⊑ sup` and its justification, in the
/// order the justification search left it.
pub(crate) struct Explained {
    pub sub: String,
    pub sup: String,
    pub axioms: Vec<Ax>,
}

/// Where the explained axioms come from: the root ontology's IRI, and the one
/// import whose axioms are `imported`, when there is one.
pub(crate) struct Provenance {
    pub root: Option<String>,
    pub import: Option<String>,
    pub imported: HashSet<Ax>,
}

// ---------------------------------------------------------------------------
// Java's sort, for the comparators that are not total orders.

/// `List.sort` as Java runs it on a list shorter than 32: the leading run is
/// found (and reversed if descending), then the rest is binary-inserted. For a
/// comparator that is not a total order the result depends on exactly these
/// steps. Longer lists sort stably by the comparator.
fn java_sort<T>(v: &mut Vec<T>, cmp: &dyn Fn(&T, &T) -> i32) {
    let n = v.len();
    if n < 2 {
        return;
    }
    if n >= 32 {
        v.sort_by(|a, b| cmp(a, b).cmp(&0));
        return;
    }
    // The leading run.
    let mut run = 1;
    if cmp(&v[1], &v[0]) < 0 {
        run = 2;
        while run < n && cmp(&v[run], &v[run - 1]) < 0 {
            run += 1;
        }
        v[..run].reverse();
    } else {
        run = 2;
        while run < n && cmp(&v[run], &v[run - 1]) >= 0 {
            run += 1;
        }
    }
    // Binary insertion of the rest.
    for start in run..n {
        let (mut left, mut right) = (0usize, start);
        while left < right {
            let mid = (left + right) >> 1;
            if cmp(&v[start], &v[mid]) < 0 {
                right = mid;
            } else {
                left = mid + 1;
            }
        }
        let p = v.remove(start);
        v.insert(left, p);
    }
}

// ---------------------------------------------------------------------------
// java.util.HashSet order.

fn spread(h: i32) -> u32 {
    let h = h as u32;
    h ^ (h >> 16)
}

/// The capacity a `HashSet` built empty and filled one by one ends at.
fn grown_capacity(n: usize) -> usize {
    crate::owlapi_hash::java_hashset_capacity(n)
}

/// The capacity of a `HashSet` copied from a collection of `n`.
fn copied_capacity(n: usize) -> usize {
    std::cmp::max((n as f64 / 0.75) as usize + 1, 16).next_power_of_two()
}

/// The order a `HashSet` of these hashes iterates in, given the order the
/// members were added in (`0..hashes.len()`) and its capacity.
fn hashset_order(hashes: &[i32], cap: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..hashes.len()).collect();
    idx.sort_by_key(|&i| (spread(hashes[i]) as usize) & (cap - 1));
    idx
}

fn entity_hash(e: &Entity) -> i32 {
    let tag = |p: i32, iri: &str| p.wrapping_mul(31).wrapping_add(crate::owlapi_hash::iri_hash(iri));
    match e.0 {
        kind::CLASS => tag(2293, &e.1),
        kind::OBJECT_PROPERTY => tag(4153, &e.1),
        kind::DATA_PROPERTY => tag(4073, &e.1),
        kind::NAMED_INDIVIDUAL => tag(4327, &e.1),
        kind::DATATYPE => tag(3911, &e.1),
        _ => tag(6067, &e.1),
    }
}

fn ax_hash(ac: &Ax) -> i32 {
    axiom_hash(&ac.component, &ac.ann).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// OWLAPI's object order.

fn axiom_type_index(c: &Component<RcStr>) -> i32 {
    use Component as C;
    2000 + match c {
        C::DeclareClass(_)
        | C::DeclareObjectProperty(_)
        | C::DeclareDataProperty(_)
        | C::DeclareNamedIndividual(_)
        | C::DeclareAnnotationProperty(_)
        | C::DeclareDatatype(_) => 0,
        C::EquivalentClasses(_) => 1,
        C::SubClassOf(_) => 2,
        C::DisjointClasses(_) => 3,
        C::DisjointUnion(_) => 4,
        C::ClassAssertion(_) => 5,
        C::SameIndividual(_) => 6,
        C::DifferentIndividuals(_) => 7,
        C::ObjectPropertyAssertion(_) => 8,
        C::NegativeObjectPropertyAssertion(_) => 9,
        C::DataPropertyAssertion(_) => 10,
        C::NegativeDataPropertyAssertion(_) => 11,
        C::EquivalentObjectProperties(_) => 12,
        C::SubObjectPropertyOf(x) => match x.sub {
            SOPE::ObjectPropertyExpression(_) => 13,
            SOPE::ObjectPropertyChain(_) => 25,
        },
        C::InverseObjectProperties(_) => 14,
        C::FunctionalObjectProperty(_) => 15,
        C::InverseFunctionalObjectProperty(_) => 16,
        C::SymmetricObjectProperty(_) => 17,
        C::AsymmetricObjectProperty(_) => 18,
        C::TransitiveObjectProperty(_) => 19,
        C::ReflexiveObjectProperty(_) => 20,
        C::IrreflexiveObjectProperty(_) => 21,
        C::ObjectPropertyDomain(_) => 22,
        C::ObjectPropertyRange(_) => 23,
        C::DisjointObjectProperties(_) => 24,
        C::EquivalentDataProperties(_) => 26,
        C::SubDataPropertyOf(_) => 27,
        C::FunctionalDataProperty(_) => 28,
        C::DataPropertyDomain(_) => 29,
        C::DataPropertyRange(_) => 30,
        C::DisjointDataProperties(_) => 31,
        C::HasKey(_) => 32,
        C::Rule(_) => 33,
        C::AnnotationAssertion(_) => 34,
        C::SubAnnotationPropertyOf(_) => 35,
        C::AnnotationPropertyRange(_) => 36,
        C::AnnotationPropertyDomain(_) => 37,
        C::DatatypeDefinition(_) => 38,
        _ => 99,
    }
}

fn ope_cmp(a: &OPE<RcStr>, b: &OPE<RcStr>) -> Ordering {
    let idx = |o: &OPE<RcStr>| match o {
        OPE::ObjectProperty(_) => 1002,
        OPE::InverseObjectProperty(_) => 1003,
    };
    idx(a).cmp(&idx(b)).then_with(|| {
        let iri = |o: &OPE<RcStr>| match o {
            OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => p.0.to_string(),
        };
        crate::owlapi_hash::iri_cmp(&iri(a), &iri(b))
    })
}

fn ind_cmp(a: &Individual<RcStr>, b: &Individual<RcStr>) -> Ordering {
    match (a, b) {
        (Individual::Named(x), Individual::Named(y)) => crate::owlapi_hash::iri_cmp(x.0.as_ref(), y.0.as_ref()),
        (Individual::Named(_), Individual::Anonymous(_)) => Ordering::Less,
        (Individual::Anonymous(_), Individual::Named(_)) => Ordering::Greater,
        (Individual::Anonymous(x), Individual::Anonymous(y)) => x.0.as_ref().cmp(y.0.as_ref()),
    }
}

fn sorted_ces(v: &[CE<RcStr>]) -> Vec<&CE<RcStr>> {
    let mut out: Vec<&CE<RcStr>> = Vec::new();
    for c in v {
        if !out.iter().any(|x| **x == *c) {
            out.push(c);
        }
    }
    out.sort_by(|a, b| owl_cmp(a, b));
    out
}

fn list_cmp<T>(a: &[T], b: &[T], cmp: impl Fn(&T, &T) -> Ordering) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let d = cmp(x, y);
        if d != Ordering::Equal {
            return d;
        }
    }
    a.len().cmp(&b.len())
}

/// The members of a class equivalence or disjointness.
fn class_members(c: &Component<RcStr>) -> Option<&[CE<RcStr>]> {
    match c {
        Component::EquivalentClasses(x) => Some(&x.0),
        Component::DisjointClasses(x) => Some(&x.0),
        _ => None,
    }
}

/// The members of a property equivalence or disjointness.
fn property_members(c: &Component<RcStr>) -> Option<&[OPE<RcStr>]> {
    match c {
        Component::EquivalentObjectProperties(x) => Some(&x.0),
        Component::DisjointObjectProperties(x) => Some(&x.0),
        _ => None,
    }
}

/// The property of an object property characteristic axiom.
fn characteristic(c: &Component<RcStr>) -> Option<&OPE<RcStr>> {
    use Component as C;
    match c {
        C::TransitiveObjectProperty(x) => Some(&x.0),
        C::FunctionalObjectProperty(x) => Some(&x.0),
        C::InverseFunctionalObjectProperty(x) => Some(&x.0),
        C::SymmetricObjectProperty(x) => Some(&x.0),
        C::AsymmetricObjectProperty(x) => Some(&x.0),
        C::ReflexiveObjectProperty(x) => Some(&x.0),
        C::IrreflexiveObjectProperty(x) => Some(&x.0),
        _ => None,
    }
}

/// OWLAPI's `OWLAxiom.compareTo`: axiom type first, then the axiom's fields.
fn axiom_cmp(a: &Ax, b: &Ax) -> Ordering {
    use Component as C;
    let d = axiom_type_index(&a.component).cmp(&axiom_type_index(&b.component));
    if d != Ordering::Equal {
        return d;
    }
    if let (Some(x), Some(y)) = (class_members(&a.component), class_members(&b.component)) {
        return list_cmp(&sorted_ces(x), &sorted_ces(y), |p, q| owl_cmp(p, q));
    }
    if let (Some(x), Some(y)) = (characteristic(&a.component), characteristic(&b.component)) {
        return ope_cmp(x, y);
    }
    match (&a.component, &b.component) {
        (C::SubClassOf(x), C::SubClassOf(y)) => owl_cmp(&x.sub, &y.sub).then_with(|| owl_cmp(&x.sup, &y.sup)),
        (C::SubObjectPropertyOf(x), C::SubObjectPropertyOf(y)) => {
            let sub_cmp = match (&x.sub, &y.sub) {
                (SOPE::ObjectPropertyExpression(p), SOPE::ObjectPropertyExpression(q)) => ope_cmp(p, q),
                (SOPE::ObjectPropertyChain(p), SOPE::ObjectPropertyChain(q)) => list_cmp(p, q, ope_cmp),
                _ => Ordering::Equal,
            };
            sub_cmp.then_with(|| ope_cmp(&x.sup, &y.sup))
        }
        (C::ObjectPropertyDomain(x), C::ObjectPropertyDomain(y)) => {
            ope_cmp(&x.ope, &y.ope).then_with(|| owl_cmp(&x.ce, &y.ce))
        }
        (C::ObjectPropertyRange(x), C::ObjectPropertyRange(y)) => {
            ope_cmp(&x.ope, &y.ope).then_with(|| owl_cmp(&x.ce, &y.ce))
        }
        (C::InverseObjectProperties(x), C::InverseObjectProperties(y)) => {
            ope_cmp(&x.0, &y.0).then_with(|| ope_cmp(&x.1, &y.1))
        }
        (C::ClassAssertion(x), C::ClassAssertion(y)) => ind_cmp(&x.i, &y.i).then_with(|| owl_cmp(&x.ce, &y.ce)),
        (C::ObjectPropertyAssertion(x), C::ObjectPropertyAssertion(y)) => ind_cmp(&x.from, &y.from)
            .then_with(|| ope_cmp(&x.ope, &y.ope))
            .then_with(|| ind_cmp(&x.to, &y.to)),
        _ => format!("{:?}", a.component).cmp(&format!("{:?}", b.component)),
    }
}

// ---------------------------------------------------------------------------
// Manchester syntax with markdown links.

struct Renderer<'a> {
    labels: &'a HashMap<String, String>,
}

fn short_form(iri: &str) -> String {
    let (ns, rem) = crate::owlapi_hash::iri_split(iri);
    if !rem.is_empty() {
        rem.to_string()
    } else {
        ns.trim_end_matches(['/', '#']).rsplit(['/', '#']).next().unwrap_or(ns).to_string()
    }
}

impl Renderer<'_> {
    fn link(&self, iri: &str) -> String {
        let label = self.labels.get(iri).cloned().unwrap_or_else(|| short_form(iri));
        format!("[{label}]({iri})")
    }

    fn ope(&self, o: &OPE<RcStr>, out: &mut String) {
        match o {
            OPE::ObjectProperty(p) => out.push_str(&self.link(p.0.as_ref())),
            OPE::InverseObjectProperty(p) => {
                out.push_str(" inverse (");
                out.push_str(&self.link(p.0.as_ref()));
                out.push(')');
            }
        }
    }

    fn ind(&self, i: &Individual<RcStr>, out: &mut String) {
        match i {
            Individual::Named(n) => out.push_str(&self.link(n.0.as_ref())),
            Individual::Anonymous(a) => out.push_str(a.0.as_ref()),
        }
    }

    fn operand(&self, c: &CE<RcStr>, out: &mut String) {
        if matches!(c, CE::Class(_)) {
            self.ce(c, out);
        } else {
            out.push('(');
            self.ce(c, out);
            out.push(')');
        }
    }

    fn restriction(&self, o: &OPE<RcStr>, keyword: &str, filler: &CE<RcStr>, out: &mut String) {
        self.ope(o, out);
        out.push(' ');
        out.push_str(keyword);
        out.push(' ');
        match filler {
            CE::Class(_) => self.ce(filler, out),
            CE::ObjectIntersectionOf(_) | CE::ObjectUnionOf(_) => {
                out.push_str("\n(");
                self.ce(filler, out);
                out.push(')');
            }
            _ => {
                out.push('(');
                self.ce(filler, out);
                out.push(')');
            }
        }
    }

    fn cardinality(&self, o: &OPE<RcStr>, keyword: &str, n: u32, filler: &CE<RcStr>, out: &mut String) {
        self.ope(o, out);
        out.push(' ');
        out.push_str(keyword);
        out.push(' ');
        out.push_str(&n.to_string());
        out.push(' ');
        self.operand(filler, out);
    }

    fn ce(&self, c: &CE<RcStr>, out: &mut String) {
        match c {
            CE::Class(x) => out.push_str(&self.link(x.0.as_ref())),
            CE::ObjectIntersectionOf(ops) | CE::ObjectUnionOf(ops) => {
                let word = if matches!(c, CE::ObjectIntersectionOf(_)) { " and " } else { " or " };
                for (i, op) in sorted_ces(ops).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(word);
                    }
                    self.operand(op, out);
                }
            }
            CE::ObjectComplementOf(b) => {
                out.push_str("not (");
                self.ce(b, out);
                out.push(')');
            }
            CE::ObjectSomeValuesFrom { ope, bce } => self.restriction(ope, "some", bce, out),
            CE::ObjectAllValuesFrom { ope, bce } => self.restriction(ope, "only", bce, out),
            CE::ObjectHasValue { ope, i } => {
                self.ope(ope, out);
                out.push_str(" value ");
                self.ind(i, out);
            }
            CE::ObjectMinCardinality { n, ope, bce } => self.cardinality(ope, "min", *n, bce, out),
            CE::ObjectMaxCardinality { n, ope, bce } => self.cardinality(ope, "max", *n, bce, out),
            CE::ObjectExactCardinality { n, ope, bce } => self.cardinality(ope, "exactly", *n, bce, out),
            CE::ObjectHasSelf(ope) => {
                self.ope(ope, out);
                out.push_str(" Self");
            }
            CE::ObjectOneOf(inds) => {
                let mut v: Vec<&Individual<RcStr>> = inds.iter().collect();
                v.sort_by(|a, b| ind_cmp(a, b));
                out.push('{');
                for (i, x) in v.into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(" , ");
                    }
                    self.ind(x, out);
                }
                out.push('}');
            }
            _ => out.push_str(&format!("{c:?}")),
        }
    }

    fn pair(&self, members: &[CE<RcStr>], binary: &str, nary: &str) -> String {
        let sorted = sorted_ces(members);
        let mut out = String::new();
        if sorted.len() == 2 {
            self.ce(sorted[0], &mut out);
            out.push(' ');
            out.push_str(binary);
            out.push(' ');
            self.ce(sorted[1], &mut out);
        } else {
            out.push(' ');
            out.push_str(nary);
            out.push_str(": ");
            for (i, c) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                self.ce(c, &mut out);
            }
        }
        out
    }

    fn axiom(&self, c: &Component<RcStr>) -> String {
        use Component as C;
        let mut out = String::new();
        let section = |out: &mut String, word: &str, o: &OPE<RcStr>| {
            out.push(' ');
            out.push_str(word);
            out.push_str(": ");
            self.ope(o, out);
        };
        match c {
            C::SubClassOf(x) => {
                self.ce(&x.sub, &mut out);
                out.push_str(" SubClassOf ");
                self.ce(&x.sup, &mut out);
            }
            C::EquivalentClasses(x) => out = self.pair(&x.0, "EquivalentTo", "EquivalentClasses"),
            C::DisjointClasses(x) => out = self.pair(&x.0, "DisjointWith", "DisjointClasses"),
            C::SubObjectPropertyOf(x) => {
                match &x.sub {
                    SOPE::ObjectPropertyExpression(o) => self.ope(o, &mut out),
                    SOPE::ObjectPropertyChain(chain) => {
                        for (i, o) in chain.iter().enumerate() {
                            if i > 0 {
                                out.push_str(" o ");
                            }
                            self.ope(o, &mut out);
                        }
                    }
                }
                out.push_str(" SubPropertyOf: ");
                self.ope(&x.sup, &mut out);
            }
            C::TransitiveObjectProperty(x) => section(&mut out, "Transitive", &x.0),
            C::FunctionalObjectProperty(x) => section(&mut out, "Functional", &x.0),
            C::InverseFunctionalObjectProperty(x) => section(&mut out, "InverseFunctional", &x.0),
            C::SymmetricObjectProperty(x) => section(&mut out, "Symmetric", &x.0),
            C::AsymmetricObjectProperty(x) => section(&mut out, "Asymmetric", &x.0),
            C::ReflexiveObjectProperty(x) => section(&mut out, "Reflexive", &x.0),
            C::IrreflexiveObjectProperty(x) => section(&mut out, "Irreflexive", &x.0),
            C::InverseObjectProperties(x) => {
                self.ope(&x.0, &mut out);
                out.push_str(" InverseOf ");
                self.ope(&x.1, &mut out);
            }
            C::ObjectPropertyDomain(x) => {
                self.ope(&x.ope, &mut out);
                out.push_str(" Domain ");
                self.ce(&x.ce, &mut out);
            }
            C::ObjectPropertyRange(x) => {
                self.ope(&x.ope, &mut out);
                out.push_str(" Range ");
                self.ce(&x.ce, &mut out);
            }
            C::ClassAssertion(x) => {
                self.ind(&x.i, &mut out);
                out.push_str(" Type ");
                self.ce(&x.ce, &mut out);
            }
            C::ObjectPropertyAssertion(x) => {
                self.ind(&x.from, &mut out);
                out.push(' ');
                self.ope(&x.ope, &mut out);
                out.push(' ');
                self.ind(&x.to, &mut out);
            }
            other => out.push_str(&format!("{other:?}")),
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The explanation tree.

struct Node {
    /// The axiom, or `None` for the root, which holds the entailment.
    ax: Option<usize>,
    children: Vec<usize>,
    parent: Option<usize>,
}

/// The right-hand-side entities of an axiom: the entities its defining side
/// names, in the order the set that collects them iterates.
fn rhs_entities(c: &Component<RcStr>) -> Vec<Entity> {
    use Component as C;
    let sig_ce = |ce: &CE<RcStr>| -> Vec<Entity> { signature_order(ce) };
    let sig_ope = |o: &OPE<RcStr>| -> Vec<Entity> {
        match o {
            OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => vec![(kind::OBJECT_PROPERTY, p.0.to_string())],
        }
    };
    let mut adds: Vec<Entity> = Vec::new();
    match c {
        C::SubClassOf(x) if matches!(x.sub, CE::Class(_)) => adds.extend(sig_ce(&x.sup)),
        C::DisjointClasses(_) | C::EquivalentClasses(_) => {
            for m in sorted_ces(class_members(c).unwrap_or(&[])) {
                adds.extend(sig_ce(m));
            }
        }
        C::ObjectPropertyDomain(x) => adds.extend(sig_ce(&x.ce)),
        C::ObjectPropertyRange(x) => adds.extend(sig_ce(&x.ce)),
        C::EquivalentObjectProperties(_) | C::DisjointObjectProperties(_) => {
            for o in property_members(c).unwrap_or(&[]) {
                adds.extend(sig_ope(o));
            }
        }
        C::SubObjectPropertyOf(x) if matches!(x.sub, SOPE::ObjectPropertyExpression(_)) => {
            adds.extend(sig_ope(&x.sup))
        }
        C::InverseObjectProperties(x) => {
            adds.extend(sig_ope(&x.0));
            adds.extend(sig_ope(&x.1));
        }
        C::ClassAssertion(x) if matches!(x.i, Individual::Named(_)) => adds.extend(sig_ce(&x.ce)),
        _ => {}
    }
    // A HashSet of entities, filled in that order.
    let mut distinct: Vec<Entity> = Vec::new();
    for e in adds {
        if !distinct.contains(&e) {
            distinct.push(e);
        }
    }
    let hashes: Vec<i32> = distinct.iter().map(entity_hash).collect();
    let order = hashset_order(&hashes, grown_capacity(distinct.len()));
    let mut v: Vec<Entity> = order.into_iter().map(|i| distinct[i].clone()).collect();
    let is_property = |e: &Entity| matches!(e.0, kind::OBJECT_PROPERTY | kind::DATA_PROPERTY | kind::ANNOTATION_PROPERTY);
    java_sort(&mut v, &|a, b| {
        if is_property(a) {
            -1
        } else if a == b {
            0
        } else {
            1
        }
    });
    v
}

/// The entities of a class expression in the order its signature set
/// iterates: collected in visiting order — operands in their stored order, a
/// restriction's property before its filler — into a set of entities.
fn signature_order(ce: &CE<RcStr>) -> Vec<Entity> {
    fn visit(ce: &CE<RcStr>, out: &mut Vec<Entity>) {
        let mut push = |e: Entity| {
            if !out.contains(&e) {
                out.push(e);
            }
        };
        let prop = |o: &OPE<RcStr>| match o {
            OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => (kind::OBJECT_PROPERTY, p.0.to_string()),
        };
        match ce {
            CE::Class(c) => push((kind::CLASS, c.0.to_string())),
            CE::ObjectIntersectionOf(ops) | CE::ObjectUnionOf(ops) => {
                for op in sorted_ces(ops) {
                    visit(op, out);
                }
            }
            CE::ObjectComplementOf(b) => visit(b, out),
            CE::ObjectSomeValuesFrom { ope, bce } | CE::ObjectAllValuesFrom { ope, bce } => {
                push(prop(ope));
                visit(bce, out);
            }
            CE::ObjectMinCardinality { ope, bce, .. }
            | CE::ObjectMaxCardinality { ope, bce, .. }
            | CE::ObjectExactCardinality { ope, bce, .. } => {
                push(prop(ope));
                visit(bce, out);
            }
            CE::ObjectHasValue { ope, i } => {
                push(prop(ope));
                if let Individual::Named(n) = i {
                    push((kind::NAMED_INDIVIDUAL, n.0.to_string()));
                }
            }
            CE::ObjectHasSelf(ope) => push(prop(ope)),
            CE::ObjectOneOf(inds) => {
                for i in inds {
                    if let Individual::Named(n) = i {
                        push((kind::NAMED_INDIVIDUAL, n.0.to_string()));
                    }
                }
            }
            _ => {
                for (k, iri) in crate::sig::typed_signature(&Component::SubClassOf(horned_owl::model::SubClassOf {
                    sub: ce.clone(),
                    sup: ce.clone(),
                })) {
                    push((k, iri));
                }
            }
        }
    }
    let mut v = Vec::new();
    visit(ce, &mut v);
    let hashes: Vec<i32> = v.iter().map(entity_hash).collect();
    hashset_order(&hashes, grown_capacity(v.len())).into_iter().map(|i| v[i].clone()).collect()
}

/// An entity's axioms among the justification's, in the order the
/// justification's ontology returns them.
fn entity_axioms(axioms: &[&Ax], e: &Entity) -> Vec<usize> {
    use Component as C;
    let iri = e.1.as_str();
    let is_class = |ce: &CE<RcStr>| matches!(ce, CE::Class(c) if c.0.as_ref() == iri);
    let is_prop = |o: &OPE<RcStr>| matches!(o, OPE::ObjectProperty(p) if p.0.as_ref() == iri);
    let is_ind = |i: &Individual<RcStr>| matches!(i, Individual::Named(n) if n.0.as_ref() == iri);
    match e.0 {
        kind::CLASS => {
            // The class's equivalences, then its subclass axioms, then its
            // disjointness axioms, gathered into a set.
            let mut groups: Vec<usize> = Vec::new();
            for pass in 0..4 {
                for (i, ac) in axioms.iter().enumerate() {
                    let hit = match (&ac.component, pass) {
                        (C::EquivalentClasses(x), 0) => x.0.iter().any(is_class),
                        (C::SubClassOf(x), 1) => is_class(&x.sub),
                        (C::DisjointClasses(x), 2) => x.0.iter().any(is_class),
                        (C::DisjointUnion(x), 3) => (x.0).0.as_ref() == iri,
                        _ => false,
                    };
                    if hit {
                        groups.push(i);
                    }
                }
            }
            let hashes: Vec<i32> = groups.iter().map(|&i| ax_hash(axioms[i])).collect();
            hashset_order(&hashes, grown_capacity(groups.len())).into_iter().map(|k| groups[k]).collect()
        }
        kind::OBJECT_PROPERTY => {
            let mut groups: Vec<usize> = Vec::new();
            for pass in 0..13 {
                for (i, ac) in axioms.iter().enumerate() {
                    let hit = match (&ac.component, pass) {
                        (C::AsymmetricObjectProperty(x), 0) => is_prop(&x.0),
                        (C::ReflexiveObjectProperty(x), 1) => is_prop(&x.0),
                        (C::SymmetricObjectProperty(x), 2) => is_prop(&x.0),
                        (C::IrreflexiveObjectProperty(x), 3) => is_prop(&x.0),
                        (C::TransitiveObjectProperty(x), 4) => is_prop(&x.0),
                        (C::InverseFunctionalObjectProperty(x), 5) => is_prop(&x.0),
                        (C::FunctionalObjectProperty(x), 6) => is_prop(&x.0),
                        (C::InverseObjectProperties(x), 7) => is_prop(&x.0) || is_prop(&x.1),
                        (C::ObjectPropertyDomain(x), 8) => is_prop(&x.ope),
                        (C::EquivalentObjectProperties(x), 9) => x.0.iter().any(is_prop),
                        (C::DisjointObjectProperties(x), 10) => x.0.iter().any(is_prop),
                        (C::ObjectPropertyRange(x), 11) => is_prop(&x.ope),
                        (C::SubObjectPropertyOf(x), 12) => {
                            matches!(&x.sub, SOPE::ObjectPropertyExpression(o) if is_prop(o))
                        }
                        _ => false,
                    };
                    if hit && !groups.contains(&i) {
                        groups.push(i);
                    }
                }
            }
            let hashes: Vec<i32> = groups.iter().map(|&i| ax_hash(axioms[i])).collect();
            // A set created for fifty.
            hashset_order(&hashes, 64).into_iter().map(|k| groups[k]).collect()
        }
        kind::NAMED_INDIVIDUAL => {
            let mut out: Vec<usize> = Vec::new();
            for pass in 0..2 {
                for (i, ac) in axioms.iter().enumerate() {
                    let hit = match (&ac.component, pass) {
                        (C::ClassAssertion(x), 0) => is_ind(&x.i),
                        (C::ObjectPropertyAssertion(x), 1) => is_ind(&x.from),
                        _ => false,
                    };
                    if hit {
                        out.push(i);
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn is_property_axiom(c: &Component<RcStr>) -> bool {
    use Component as C;
    matches!(
        c,
        C::SubObjectPropertyOf(_)
            | C::EquivalentObjectProperties(_)
            | C::DisjointObjectProperties(_)
            | C::InverseObjectProperties(_)
            | C::ObjectPropertyDomain(_)
            | C::ObjectPropertyRange(_)
            | C::FunctionalObjectProperty(_)
            | C::InverseFunctionalObjectProperty(_)
            | C::SymmetricObjectProperty(_)
            | C::AsymmetricObjectProperty(_)
            | C::TransitiveObjectProperty(_)
            | C::ReflexiveObjectProperty(_)
            | C::IrreflexiveObjectProperty(_)
            | C::SubDataPropertyOf(_)
            | C::EquivalentDataProperties(_)
            | C::DisjointDataProperties(_)
            | C::DataPropertyDomain(_)
            | C::DataPropertyRange(_)
            | C::FunctionalDataProperty(_)
    )
}

struct Orderer<'a> {
    axioms: Vec<&'a Ax>,
    nodes: Vec<Node>,
    consumed: HashSet<usize>,
    mapped: HashMap<Entity, HashSet<usize>>,
}

impl<'a> Orderer<'a> {
    fn path_axioms(&self, node: usize) -> HashSet<usize> {
        let mut out = HashSet::new();
        let mut n = Some(node);
        while let Some(i) = n {
            if let Some(a) = self.nodes[i].ax {
                out.insert(a);
            }
            n = self.nodes[i].parent;
        }
        out
    }

    fn add_child(&mut self, parent: usize, ax: usize) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node { ax: Some(ax), children: Vec::new(), parent: Some(parent) });
        self.nodes[parent].children.push(id);
        id
    }

    fn insert_children(&mut self, e: &Entity, node: usize) {
        let path = self.path_axioms(node);
        for ax in entity_axioms(&self.axioms, e) {
            if matches!(self.axioms[ax].component, Component::DisjointClasses(_)) {
                continue;
            }
            let mapped = self.mapped.entry(e.clone()).or_default();
            if self.consumed.contains(&ax) || mapped.contains(&ax) || path.contains(&ax) {
                continue;
            }
            mapped.insert(ax);
            self.consumed.insert(ax);
            let child = self.add_child(node, ax);
            for r in rhs_entities(&self.axioms[ax].component) {
                self.insert_children(&r, child);
            }
        }
        self.sort_children(node);
    }

    fn sort_children(&mut self, node: usize) {
        let mut kids = std::mem::take(&mut self.nodes[node].children);
        let nodes = &self.nodes;
        let axioms = &self.axioms;
        java_sort(&mut kids, &|a, b| {
            let (x, y) = (&axioms[nodes[*a].ax.unwrap()].component, &axioms[nodes[*b].ax.unwrap()].component);
            if matches!(x, Component::EquivalentClasses(_)) {
                return 1;
            }
            if matches!(y, Component::EquivalentClasses(_)) {
                return -1;
            }
            if is_property_axiom(x) {
                return -1;
            }
            let leaf = |n: usize| if nodes[n].children.is_empty() { 1 } else { 0 };
            let diff = leaf(*a) - leaf(*b);
            if diff != 0 {
                return diff;
            }
            if let (Component::SubClassOf(p), Component::SubClassOf(q)) = (x, y) {
                return match owl_cmp(&p.sup, &q.sup) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                };
            }
            1
        });
        self.nodes[node].children = kids;
    }
}

/// The justification's axioms in the order its set iterates: copied twice
/// into sets sized for it, from the order the search left them in.
fn justification_order(axioms: &[Ax]) -> Vec<usize> {
    let hashes: Vec<i32> = axioms.iter().map(ax_hash).collect();
    hashset_order(&hashes, copied_capacity(axioms.len()))
}

fn render_tree(r: &Renderer, o: &Orderer, entailment: &str, node: usize, depth: usize, out: &mut String) {
    match o.nodes[node].ax {
        None => {
            out.push_str("## ");
            out.push_str(entailment);
            out.push_str(" ##\n");
        }
        Some(a) => {
            for _ in 0..depth {
                out.push_str("  ");
            }
            out.push_str("- ");
            out.push_str(&r.axiom(&o.axioms[a].component));
        }
    }
    if !o.nodes[node].children.is_empty() {
        out.push('\n');
    }
    for (i, &c) in o.nodes[node].children.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_tree(r, o, entailment, c, depth + 1, out);
    }
}

fn render_explanation(r: &Renderer, ex: &Explained) -> String {
    let order = justification_order(&ex.axioms);
    let axioms: Vec<&Ax> = order.iter().map(|&i| &ex.axioms[i]).collect();
    let mut o = Orderer { axioms, nodes: Vec::new(), consumed: HashSet::new(), mapped: HashMap::new() };
    o.nodes.push(Node { ax: None, children: Vec::new(), parent: None });
    let source: Entity = (kind::CLASS, ex.sub.clone());
    o.insert_children(&source, 0);
    let mut in_tree: HashSet<usize> = HashSet::new();
    for n in &o.nodes {
        if let Some(a) = n.ax {
            in_tree.insert(a);
        }
    }
    // What the walk never reached hangs off the root, in the justification's
    // order. (Only an entailment with a named superclass has axioms that sort
    // last among them, and an unsatisfiability has none.)
    for a in 0..o.axioms.len() {
        if !in_tree.contains(&a) {
            o.add_child(0, a);
        }
    }
    let b = horned_owl::model::Build::new();
    let entailment = r.axiom(&Component::SubClassOf(horned_owl::model::SubClassOf {
        sub: CE::Class(b.class(ex.sub.as_str())),
        sup: CE::Class(b.class(ex.sup.as_str())),
    }));
    let mut out = String::new();
    render_tree(r, &o, &entailment, 0, 0, &mut out);
    out
}

/// The whole report: the explanations in the order their set iterates, then
/// the axiom impact summary.
pub(crate) fn report(explained: &[Explained], labels: &HashMap<String, String>, prov: &Provenance) -> String {
    let r = Renderer { labels };
    if explained.is_empty() {
        return "No explanations found.".to_string();
    }
    // The set of explanations, keyed by entailment plus justification.
    let b = horned_owl::model::Build::new();
    let hashes: Vec<i32> = explained
        .iter()
        .map(|e| {
            let ent = Component::SubClassOf(horned_owl::model::SubClassOf {
                sub: CE::Class(b.class(e.sub.as_str())),
                sup: CE::Class(b.class(e.sup.as_str())),
            });
            e.axioms.iter().fold(axiom_hash(&ent, &Default::default()).unwrap_or(0), |acc, ac| {
                acc.wrapping_add(ax_hash(ac))
            })
        })
        .collect();
    let order = hashset_order(&hashes, grown_capacity(explained.len()));
    let mut out: String = order
        .iter()
        .map(|&i| render_explanation(&r, &explained[i]))
        .collect::<Vec<_>>()
        .join("\n\n\n");

    // The axiom impact summary.
    let mut counts: Vec<(Ax, usize)> = Vec::new();
    for e in explained {
        for ac in &e.axioms {
            match counts.iter_mut().find(|(a, _)| a == ac) {
                Some((_, n)) => *n += 1,
                None => counts.push((ac.clone(), 1)),
            }
        }
    }
    let mut levels: Vec<usize> = counts.iter().map(|(_, n)| *n).collect();
    levels.sort_unstable_by(|a, b| b.cmp(a));
    levels.dedup();
    let short = |iri: &str| short_form(iri);
    let ontology_of = |ac: &Ax| -> (String, String) {
        let iri = if prov.imported.contains(ac) { prov.import.clone() } else { prov.root.clone() };
        match iri {
            Some(i) => (short(&i), i),
            None => ("O1".to_string(), "unknown.iri".to_string()),
        }
    };
    out.push_str("\n\n# Axiom Impact \n");
    let mut used: Vec<(String, String)> = Vec::new();
    for level in levels {
        let mut group: Vec<&Ax> = counts.iter().filter(|(_, n)| *n == level).map(|(a, _)| a).collect();
        group.sort_by(|a, b| axiom_cmp(a, b));
        out.push_str(&format!("## Axioms used {level} times\n"));
        for ac in group {
            let (abbrev, iri) = ontology_of(ac);
            if !used.iter().any(|(_, i)| *i == iri) {
                used.push((abbrev.clone(), iri));
            }
            out.push_str(&format!("- {} [{}]\n", r.axiom(&ac.component), abbrev));
        }
        out.push('\n');
    }
    out.push_str("\n\n# Ontologies used: \n");
    let ids: Vec<i32> = used.iter().map(|(_, i)| crate::owlapi_hash::ontology_id_hash(Some(i), None)).collect();
    for k in crate::owlapi_hash::ontology_set_order(&ids) {
        out.push_str(&format!("- {} ({})\n", used[k].0, used[k].1));
    }
    out
}
