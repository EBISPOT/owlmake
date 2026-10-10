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
    AnnotatedComponent, Atom, ClassExpression as CE, Component, DArgument, DataProperty, DataRange as DR, IArgument,
    Individual, Literal, ObjectPropertyExpression as OPE, PropertyExpression, RcStr, SubObjectPropertyExpression as SOPE,
    Variable,
};

use crate::io::natural_order::{iri_cmp, sorted_set, NaturalOrder};
use crate::owlapi_hash::axiom_hash;
use crate::sig::kind;

type Ax = AnnotatedComponent<RcStr>;
type Entity = (u8, String);

const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
const XSD_FLOAT: &str = "http://www.w3.org/2001/XMLSchema#float";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

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

// ---------------------------------------------------------------------------
// Manchester syntax with markdown links.

struct Renderer<'a> {
    labels: &'a HashMap<String, String>,
    /// The order of the examined ontology's objects, which sorts the members of
    /// a set the renderer writes.
    order: NaturalOrder,
}

impl Renderer<'_> {
    fn link(&self, iri: &str) -> String {
        let label = self.labels.get(iri).cloned().unwrap_or_else(|| crate::owlapi_hash::iri_short_form(iri));
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

    fn dp(&self, p: &DataProperty<RcStr>, out: &mut String) {
        out.push_str(&self.link(p.0.as_ref()));
    }

    /// A literal: an `xsd:decimal`, `xsd:integer` or `xsd:boolean` as written,
    /// an `xsd:float` with an `f`, and anything else quoted, with its language
    /// tag or, unless it is plain or `xsd:string`, its datatype.
    fn literal(&self, l: &Literal<RcStr>, out: &mut String) {
        let dt = self.order.literal_datatype(l);
        let lex = l.literal();
        if dt == XSD_DECIMAL || dt == XSD_INTEGER || dt == XSD_BOOLEAN {
            out.push_str(lex);
            return;
        }
        if dt == XSD_FLOAT {
            out.push_str(lex);
            out.push('f');
            return;
        }
        out.push('"');
        for c in lex.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        match l {
            Literal::Language { lang, .. } if !lang.is_empty() => {
                out.push('@');
                out.push_str(lang);
            }
            _ if dt != RDF_PLAIN_LITERAL && dt != XSD_STRING => {
                out.push_str("^^");
                out.push_str(&self.link(dt));
            }
            _ => {}
        }
    }

    fn data_range(&self, d: &DR<RcStr>, out: &mut String) {
        match d {
            DR::Datatype(t) => out.push_str(&self.link(t.0.as_ref())),
            DR::DataComplementOf(op) => {
                out.push_str(" not ");
                if matches!(**op, DR::Datatype(_)) {
                    self.data_range(op, out);
                } else {
                    out.push('(');
                    self.data_range(op, out);
                    out.push(')');
                }
            }
            DR::DataOneOf(lits) => {
                out.push('{');
                for (i, l) in sorted_set(lits, |a, b| self.order.literal(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(" , ");
                    }
                    self.literal(l, out);
                }
                out.push('}');
            }
            DR::DataIntersectionOf(ops) | DR::DataUnionOf(ops) => {
                let word = if matches!(d, DR::DataIntersectionOf(_)) { " and " } else { " or " };
                out.push('(');
                for (i, op) in sorted_set(ops, |a, b| self.order.dr(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(word);
                    }
                    self.data_range(op, out);
                }
                out.push(')');
            }
            DR::DatatypeRestriction(t, facets) => {
                out.push_str(&self.link(t.0.as_ref()));
                out.push('[');
                for (i, f) in sorted_set(facets, |a, b| self.order.facet_restriction(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(" , ");
                    }
                    out.push_str(crate::io::manchester_write::facet_symbol(&f.f));
                    out.push(' ');
                    self.literal(&f.l, out);
                }
                out.push(']');
            }
        }
    }

    fn data_restriction(&self, p: &DataProperty<RcStr>, keyword: &str, n: Option<u32>, filler: &DR<RcStr>, out: &mut String) {
        self.dp(p, out);
        out.push(' ');
        out.push_str(keyword);
        out.push(' ');
        if let Some(n) = n {
            out.push_str(&n.to_string());
            out.push(' ');
        }
        self.data_range(filler, out);
    }

    /// Members of a set axiom: `a word b` for two, otherwise a section listing
    /// them, `lead` before its keyword.
    fn members(&self, items: Vec<String>, word: Option<&str>, lead: &str, section: &str) -> String {
        match (word, items.as_slice()) {
            (Some(word), [a, b]) => format!("{a} {word} {b}"),
            _ => format!("{lead}{section}: {}", items.join(", ")),
        }
    }

    fn opes(&self, v: &[OPE<RcStr>]) -> Vec<String> {
        sorted_set(v, |a, b| self.order.ope(a, b))
            .into_iter()
            .map(|o| {
                let mut s = String::new();
                self.ope(o, &mut s);
                s
            })
            .collect()
    }

    fn dps(&self, v: &[DataProperty<RcStr>]) -> Vec<String> {
        sorted_set(v, |a, b| iri_cmp(a.0.as_ref(), b.0.as_ref()))
            .into_iter()
            .map(|p| self.link(p.0.as_ref()))
            .collect()
    }

    fn inds(&self, v: &[Individual<RcStr>]) -> Vec<String> {
        sorted_set(v, |a, b| self.order.individual(a, b))
            .into_iter()
            .map(|i| {
                let mut s = String::new();
                self.ind(i, &mut s);
                s
            })
            .collect()
    }

    fn ind(&self, i: &Individual<RcStr>, out: &mut String) {
        match i {
            Individual::Named(n) => out.push_str(&self.link(n.0.as_ref())),
            Individual::Anonymous(a) => out.push_str(&crate::io::entities::node_id(a.0.as_ref())),
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
                for (i, op) in sorted_set(ops, |a, b| self.order.ce(a, b)).into_iter().enumerate() {
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
                v.sort_by(|a, b| self.order.individual(a, b));
                out.push('{');
                for (i, x) in v.into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(" , ");
                    }
                    self.ind(x, out);
                }
                out.push('}');
            }
            CE::DataSomeValuesFrom { dp, dr } => self.data_restriction(dp, "some", None, dr, out),
            CE::DataAllValuesFrom { dp, dr } => self.data_restriction(dp, "only", None, dr, out),
            CE::DataHasValue { dp, l } => {
                self.dp(dp, out);
                out.push_str(" value ");
                self.literal(l, out);
            }
            CE::DataMinCardinality { n, dp, dr } => self.data_restriction(dp, "min", Some(*n), dr, out),
            CE::DataMaxCardinality { n, dp, dr } => self.data_restriction(dp, "max", Some(*n), dr, out),
            CE::DataExactCardinality { n, dp, dr } => self.data_restriction(dp, "exactly", Some(*n), dr, out),
        }
    }

    fn pair(&self, members: &[CE<RcStr>], binary: &str, nary: &str) -> String {
        let sorted = sorted_set(members, |a, b| self.order.ce(a, b));
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
            C::NegativeObjectPropertyAssertion(x) => {
                out.push_str(" not (");
                self.ind(&x.from, &mut out);
                out.push(' ');
                self.ope(&x.ope, &mut out);
                out.push(' ');
                self.ind(&x.to, &mut out);
                out.push(')');
            }
            C::DataPropertyAssertion(x) => {
                self.ind(&x.from, &mut out);
                out.push(' ');
                self.dp(&x.dp, &mut out);
                out.push(' ');
                self.literal(&x.to, &mut out);
            }
            C::NegativeDataPropertyAssertion(x) => {
                out.push_str(" not (");
                self.ind(&x.from, &mut out);
                out.push(' ');
                self.dp(&x.dp, &mut out);
                out.push(' ');
                self.literal(&x.to, &mut out);
                out.push(')');
            }
            C::EquivalentObjectProperties(x) => {
                out = self.members(self.opes(&x.0), Some("EquivalentTo"), " ", "EquivalentProperties")
            }
            C::DisjointObjectProperties(x) => {
                out = self.members(self.opes(&x.0), Some("DisjointWith"), " ", "DisjointProperties")
            }
            C::EquivalentDataProperties(x) => out = self.members(self.dps(&x.0), None, "", "EquivalentProperties"),
            C::DisjointDataProperties(x) => {
                out = self.members(self.dps(&x.0), Some("DisjointWith"), " ", "DisjointProperties")
            }
            C::SameIndividual(x) => out = self.members(self.inds(&x.0), Some("SameAs"), " ", "SameIndividual"),
            C::DifferentIndividuals(x) => {
                out = self.members(self.inds(&x.0), Some("DifferentFrom"), " ", "DifferentIndividuals")
            }
            C::SubDataPropertyOf(x) => {
                self.dp(&x.sub, &mut out);
                out.push_str(" SubPropertyOf: ");
                self.dp(&x.sup, &mut out);
            }
            C::FunctionalDataProperty(x) => {
                out.push_str(" Functional: ");
                self.dp(&x.0, &mut out);
            }
            C::DataPropertyDomain(x) => {
                self.dp(&x.dp, &mut out);
                out.push_str(" Domain ");
                self.ce(&x.ce, &mut out);
            }
            C::DataPropertyRange(x) => {
                self.dp(&x.dp, &mut out);
                out.push_str(" Range: ");
                self.data_range(&x.dr, &mut out);
            }
            C::DisjointUnion(x) => {
                out.push_str(&self.link(x.0 .0.as_ref()));
                out.push_str(" DisjointUnionOf ");
                for (i, m) in sorted_set(&x.1, |a, b| self.order.ce(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.ce(m, &mut out);
                }
            }
            // The key's object properties and then its data properties, each
            // list sorted, with nothing between the two lists.
            C::HasKey(x) => {
                self.ce(&x.ce, &mut out);
                out.push_str(" HasKey ");
                let mut opes = Vec::new();
                let mut dps = Vec::new();
                for p in &x.vpe {
                    match p {
                        PropertyExpression::ObjectPropertyExpression(o) => opes.push(o.clone()),
                        PropertyExpression::DataProperty(d) => dps.push(d.clone()),
                        PropertyExpression::AnnotationProperty(_) => {}
                    }
                }
                out.push_str(&self.opes(&opes).join(" , "));
                out.push_str(&self.dps(&dps).join(" , "));
            }
            // A datatype definition is written as nothing at all.
            C::DatatypeDefinition(_) => {}
            C::Rule(r) => {
                self.atoms(&r.body, &mut out);
                out.push_str(" -> ");
                self.atoms(&r.head, &mut out);
            }
            other => out.push_str(&format!("{other:?}")),
        }
        out
    }

    /// A rule's atoms, in the order the rule lists them.
    fn atoms(&self, atoms: &[Atom<RcStr>], out: &mut String) {
        for (i, a) in atoms.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            self.atom(a, out);
        }
    }

    fn atom(&self, a: &Atom<RcStr>, out: &mut String) {
        let pair = |out: &mut String, x: &IArgument<RcStr>, y: &IArgument<RcStr>| {
            out.push('(');
            self.iarg(x, out);
            out.push_str(", ");
            self.iarg(y, out);
            out.push(')');
        };
        match a {
            Atom::ClassAtom { pred, arg } => {
                if let CE::Class(_) = pred {
                    self.ce(pred, out);
                } else {
                    out.push('(');
                    self.ce(pred, out);
                    out.push(')');
                }
                out.push('(');
                self.iarg(arg, out);
                out.push(')');
            }
            Atom::DataRangeAtom { pred, arg } => {
                self.data_range(pred, out);
                out.push('(');
                self.darg(arg, out);
                out.push(')');
            }
            Atom::ObjectPropertyAtom { pred, args } => {
                self.ope(pred, out);
                pair(out, &args.0, &args.1);
            }
            Atom::DataPropertyAtom { pred, args } => {
                self.dp(pred, out);
                out.push('(');
                self.darg(&args.0, out);
                out.push_str(", ");
                self.darg(&args.1, out);
                out.push(')');
            }
            // A built-in of the SWRL vocabulary goes by its swrlb: name, any
            // other by its IRI; its arguments are sorted.
            Atom::BuiltInAtom { pred, args } => {
                match pred.as_ref().strip_prefix(SWRLB) {
                    Some(name) if SWRL_BUILT_INS.contains(&name) => {
                        out.push_str("swrlb:");
                        out.push_str(name);
                    }
                    _ => {
                        out.push('<');
                        out.push_str(pred.as_ref());
                        out.push('>');
                    }
                }
                out.push('(');
                for (i, d) in sorted_set(args, |x, y| self.order.darg(x, y)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.darg(d, out);
                }
                out.push(')');
            }
            Atom::SameIndividualAtom(x, y) => {
                out.push_str(" SameAs ");
                pair(out, x, y);
            }
            Atom::DifferentIndividualsAtom(x, y) => {
                out.push_str(" DifferentFrom ");
                pair(out, x, y);
            }
        }
    }

    fn iarg(&self, a: &IArgument<RcStr>, out: &mut String) {
        match a {
            IArgument::Individual(i) => self.ind(i, out),
            IArgument::Variable(v) => variable(v, out),
        }
    }

    fn darg(&self, a: &DArgument<RcStr>, out: &mut String) {
        match a {
            DArgument::Literal(l) => self.literal(l, out),
            DArgument::Variable(v) => variable(v, out),
        }
    }
}

/// A rule variable: `?` and its local name when it lies in a SWRL variable
/// namespace, otherwise `?` and its IRI in angle brackets.
fn variable(v: &Variable<RcStr>, out: &mut String) {
    out.push('?');
    let (ns, local) = crate::owlapi_hash::iri_split(v.0.as_ref());
    if ns == "urn:swrl:var#" || ns == "urn:swrl#" {
        out.push_str(local);
    } else {
        out.push('<');
        out.push_str(v.0.as_ref());
        out.push('>');
    }
}

/// The namespace of the SWRL built-ins.
const SWRLB: &str = "http://www.w3.org/2003/11/swrlb#";

/// The built-ins of the SWRL vocabulary, by their names in [`SWRLB`].
const SWRL_BUILT_INS: [&str; 69] = [
    "equal", "notEqual", "lessThan", "lessThanOrEqual", "greaterThan", "greaterThanOrEqual", "add",
    "subtract", "multiply", "divide", "integerDivide", "mod", "pow", "unaryMinus", "unaryPlus", "abs",
    "ceiling", "floor", "round", "roundHalfToEven", "sin", "cos", "tan", "booleanNot",
    "stringEqualIgnoreCase", "stringConcat", "substring", "stringLength", "normalizeSpace", "upperCase",
    "lowerCase", "translate", "contains", "containsIgnoreCase", "startsWith", "endsWith", "substringBefore",
    "substringAfter", "matchesLax", "replace", "tokenize", "yearMonthDuration", "dayTimeDuration", "dateTime",
    "date", "time", "subtractDates", "subtractTimes", "resolveURI", "anyURI", "addYearMonthDurations",
    "subtractYearMonthDurations", "multiplyYearMonthDurations", "divideYearMonthDurations",
    "addDayTimeDurations", "subtractDayTimeDurations", "multiplyDayTimeDurations", "divideDayTimeDurations",
    "addDayTimeDurationToDateTime", "subtractYearMonthDurationFromDateTime",
    "subtractDayTimeDurationFromDateTime", "addYearMonthDurationToDate", "addDayTimeDurationToDate",
    "subtractYearMonthDurationFromDate", "subtractDayTimeDurationFromDate", "addDayTimeDurationToTime",
    "subtractDayTimeDurationFromTime", "subtractDateTimesYieldingYearMonthDuration",
    "subtractDateTimesYieldingDayTimeDuration",
];

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
fn rhs_entities(c: &Component<RcStr>, order: NaturalOrder) -> Vec<Entity> {
    use Component as C;
    let sig_ce = |ce: &CE<RcStr>| -> Vec<Entity> { signature_order(ce, order) };
    let sig_dp = |p: &DataProperty<RcStr>| -> Entity { (kind::DATA_PROPERTY, p.0.to_string()) };
    let sig_ind = |i: &Individual<RcStr>| -> Option<Entity> {
        match i {
            Individual::Named(n) => Some((kind::NAMED_INDIVIDUAL, n.0.to_string())),
            Individual::Anonymous(_) => None,
        }
    };
    let sig_ope = |o: &OPE<RcStr>| -> Vec<Entity> {
        match o {
            OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => vec![(kind::OBJECT_PROPERTY, p.0.to_string())],
        }
    };
    let mut adds: Vec<Entity> = Vec::new();
    match c {
        C::SubClassOf(x) if matches!(x.sub, CE::Class(_)) => adds.extend(sig_ce(&x.sup)),
        C::DisjointClasses(_) | C::EquivalentClasses(_) => {
            for m in sorted_set(class_members(c).unwrap_or(&[]), |a, b| order.ce(a, b)) {
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
        C::DataPropertyDomain(x) => adds.extend(sig_ce(&x.ce)),
        C::DataPropertyRange(x) => adds.extend(range_datatypes(&x.dr, order)),
        C::EquivalentDataProperties(x) => adds.extend(x.0.iter().map(sig_dp)),
        C::DisjointDataProperties(x) => adds.extend(x.0.iter().map(sig_dp)),
        C::SubDataPropertyOf(x) => adds.push(sig_dp(&x.sup)),
        C::SameIndividual(x) => adds.extend(sorted_set(&x.0, |a, b| order.individual(a, b)).into_iter().filter_map(sig_ind)),
        C::DifferentIndividuals(x) => {
            adds.extend(sorted_set(&x.0, |a, b| order.individual(a, b)).into_iter().filter_map(sig_ind))
        }
        C::DataPropertyAssertion(x) => adds.extend(sig_ind(&x.from)),
        C::HasKey(x) => {
            if let CE::Class(c) = &x.ce {
                adds.push((kind::CLASS, c.0.to_string()));
            }
        }
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
fn signature_order(ce: &CE<RcStr>, order: NaturalOrder) -> Vec<Entity> {
    fn visit(ce: &CE<RcStr>, order: NaturalOrder, out: &mut Vec<Entity>) {
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
                for op in sorted_set(ops, |a, b| order.ce(a, b)) {
                    visit(op, order, out);
                }
            }
            CE::ObjectComplementOf(b) => visit(b, order, out),
            CE::ObjectSomeValuesFrom { ope, bce } | CE::ObjectAllValuesFrom { ope, bce } => {
                push(prop(ope));
                visit(bce, order, out);
            }
            CE::ObjectMinCardinality { ope, bce, .. }
            | CE::ObjectMaxCardinality { ope, bce, .. }
            | CE::ObjectExactCardinality { ope, bce, .. } => {
                push(prop(ope));
                visit(bce, order, out);
            }
            CE::ObjectHasValue { ope, i } => {
                push(prop(ope));
                if let Individual::Named(n) = i {
                    push((kind::NAMED_INDIVIDUAL, n.0.to_string()));
                }
            }
            CE::ObjectHasSelf(ope) => push(prop(ope)),
            CE::DataSomeValuesFrom { dp, dr }
            | CE::DataAllValuesFrom { dp, dr }
            | CE::DataMinCardinality { dp, dr, .. }
            | CE::DataMaxCardinality { dp, dr, .. }
            | CE::DataExactCardinality { dp, dr, .. } => {
                push((kind::DATA_PROPERTY, dp.0.to_string()));
                for t in range_datatypes(dr, order) {
                    push(t);
                }
            }
            CE::DataHasValue { dp, l } => {
                push((kind::DATA_PROPERTY, dp.0.to_string()));
                push((kind::DATATYPE, order.literal_datatype(l).to_string()));
            }
            CE::ObjectOneOf(inds) => {
                for i in inds {
                    if let Individual::Named(n) = i {
                        push((kind::NAMED_INDIVIDUAL, n.0.to_string()));
                    }
                }
            }
        }
    }
    let mut v = Vec::new();
    visit(ce, order, &mut v);
    let hashes: Vec<i32> = v.iter().map(entity_hash).collect();
    hashset_order(&hashes, grown_capacity(v.len())).into_iter().map(|i| v[i].clone()).collect()
}

/// The datatypes a data range names, in visiting order: a datatype itself, the
/// operands of a combination in their sorted order, a literal's datatype, and a
/// restriction's datatype before its facet values'.
fn range_datatypes(dr: &DR<RcStr>, order: NaturalOrder) -> Vec<Entity> {
    fn visit(dr: &DR<RcStr>, order: NaturalOrder, out: &mut Vec<Entity>) {
        let mut push = |iri: &str| {
            let e = (kind::DATATYPE, iri.to_string());
            if !out.contains(&e) {
                out.push(e);
            }
        };
        match dr {
            DR::Datatype(t) => push(t.0.as_ref()),
            DR::DataComplementOf(op) => visit(op, order, out),
            DR::DataIntersectionOf(ops) | DR::DataUnionOf(ops) => {
                for op in sorted_set(ops, |a, b| order.dr(a, b)) {
                    visit(op, order, out);
                }
            }
            DR::DataOneOf(lits) => {
                for l in sorted_set(lits, |a, b| order.literal(a, b)) {
                    push(order.literal_datatype(l));
                }
            }
            DR::DatatypeRestriction(t, facets) => {
                push(t.0.as_ref());
                for f in sorted_set(facets, |a, b| order.facet_restriction(a, b)) {
                    push(order.literal_datatype(&f.l));
                }
            }
        }
    }
    let mut out = Vec::new();
    visit(dr, order, &mut out);
    out
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
        kind::DATA_PROPERTY => {
            // A linked set: domains, equivalences, disjointness, ranges, the
            // characteristic, then the property's superproperty axioms.
            let is_dp = |p: &DataProperty<RcStr>| p.0.as_ref() == iri;
            let mut out: Vec<usize> = Vec::new();
            for pass in 0..6 {
                for (i, ac) in axioms.iter().enumerate() {
                    let hit = match (&ac.component, pass) {
                        (C::DataPropertyDomain(x), 0) => is_dp(&x.dp),
                        (C::EquivalentDataProperties(x), 1) => x.0.iter().any(is_dp),
                        (C::DisjointDataProperties(x), 2) => x.0.iter().any(is_dp),
                        (C::DataPropertyRange(x), 3) => is_dp(&x.dp),
                        (C::FunctionalDataProperty(x), 4) => is_dp(&x.0),
                        (C::SubDataPropertyOf(x), 5) => is_dp(&x.sub),
                        _ => false,
                    };
                    if hit && !out.contains(&i) {
                        out.push(i);
                    }
                }
            }
            out
        }
        kind::NAMED_INDIVIDUAL => {
            // A linked set: class assertions, object, data, negative object and
            // negative data property assertions, then sameness and difference.
            let mut out: Vec<usize> = Vec::new();
            for pass in 0..7 {
                for (i, ac) in axioms.iter().enumerate() {
                    let hit = match (&ac.component, pass) {
                        (C::ClassAssertion(x), 0) => is_ind(&x.i),
                        (C::ObjectPropertyAssertion(x), 1) => is_ind(&x.from),
                        (C::DataPropertyAssertion(x), 2) => is_ind(&x.from),
                        (C::NegativeObjectPropertyAssertion(x), 3) => is_ind(&x.from),
                        (C::NegativeDataPropertyAssertion(x), 4) => is_ind(&x.from),
                        (C::SameIndividual(x), 5) => x.0.iter().any(is_ind),
                        (C::DifferentIndividuals(x), 6) => x.0.iter().any(is_ind),
                        _ => false,
                    };
                    if hit && !out.contains(&i) {
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
    order: NaturalOrder,
    /// The justification's axiom that is the entailment itself, unannotated:
    /// the root holds it, so the tree never does.
    entailment: Option<usize>,
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
            if self.consumed.contains(&ax) || mapped.contains(&ax) || path.contains(&ax) || self.entailment == Some(ax) {
                continue;
            }
            mapped.insert(ax);
            self.consumed.insert(ax);
            let child = self.add_child(node, ax);
            for r in rhs_entities(&self.axioms[ax].component, self.order) {
                self.insert_children(&r, child);
            }
        }
        self.sort_children(node);
    }

    fn sort_children(&mut self, node: usize) {
        let mut kids = std::mem::take(&mut self.nodes[node].children);
        let nodes = &self.nodes;
        let axioms = &self.axioms;
        let order = self.order;
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
                return match order.ce(&p.sup, &q.sup) {
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
    let b = horned_owl::model::Build::new();
    let entailment = Component::SubClassOf(horned_owl::model::SubClassOf {
        sub: CE::Class(b.class(ex.sub.as_str())),
        sup: CE::Class(b.class(ex.sup.as_str())),
    });
    let entailed = axioms.iter().position(|ac| ac.component == entailment && ac.ann.is_empty());
    let mut o = Orderer {
        axioms,
        order: r.order,
        entailment: entailed,
        nodes: Vec::new(),
        consumed: HashSet::new(),
        mapped: HashMap::new(),
    };
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
        if !in_tree.contains(&a) && o.entailment != Some(a) {
            o.add_child(0, a);
        }
    }
    let entailment = r.axiom(&entailment);
    let mut out = String::new();
    render_tree(r, &o, &entailment, 0, 0, &mut out);
    out
}

/// The whole report: the explanations in the order their set iterates, then
/// the axiom impact summary.
pub(crate) fn report(
    explained: &[Explained],
    labels: &HashMap<String, String>,
    prov: &Provenance,
    order: NaturalOrder,
) -> String {
    let r = Renderer { labels, order };
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
    let ontology_of = |ac: &Ax| -> (String, String) {
        let iri = if prov.imported.contains(ac) { prov.import.clone() } else { prov.root.clone() };
        match iri {
            Some(i) => (crate::owlapi_hash::iri_short_form(&i), i),
            None => ("O1".to_string(), "unknown.iri".to_string()),
        }
    };
    out.push_str("\n\n# Axiom Impact \n");
    let mut used: Vec<(String, String)> = Vec::new();
    for level in levels {
        let mut group: Vec<&Ax> = counts.iter().filter(|(_, n)| *n == level).map(|(a, _)| a).collect();
        group.sort_by(|a, b| r.order.component(&a.component, &b.component));
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
