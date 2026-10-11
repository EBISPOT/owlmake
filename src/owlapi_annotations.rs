//! The annotations an ontology holds of itself.
//!
//! An ontology keeps its annotations in a hash set, and hands every reader a
//! sorted copy of that set. Of two equal annotations the set keeps the first
//! added. Of two the sort finds the same — one property and one value,
//! whatever annotations each carries — the copy keeps the first the hash set
//! iterates, which for two of one hash is the first added. The sort compares
//! an untyped literal the same as the literal of its text typed `xsd:string`,
//! and the typed one after the untyped one, so which of such a pair the copy
//! keeps depends on the path its red-black tree takes, and is found by
//! building that tree.

use std::cmp::Ordering;

use horned_owl::model::{Annotation, AnnotationValue, Component, Literal, MutableOntology, OntologyAnnotation, RcStr};

use crate::model::Onto;
use crate::owlapi_hash::{annotation_hash, iri_cmp};

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// The annotations an ontology holds once `added` are added to it in this
/// order, in the order its readers see them.
pub fn held(added: impl IntoIterator<Item = Annotation<RcStr>>) -> Vec<Annotation<RcStr>> {
    let added: Vec<Annotation<RcStr>> = added.into_iter().collect();
    let order = held_of(&added);
    taken(added, order)
}

/// The annotations a reader's ontology holds of those it reads, in this
/// order: those [`held`] holds once their literals are made
/// ([`crate::model::literal_as_made`]), each as the reader read it, to be made
/// with every other literal it read.
pub fn hold(added: Vec<Annotation<RcStr>>) -> Vec<Annotation<RcStr>> {
    let made: Vec<Annotation<RcStr>> = added.iter().cloned().map(made).collect();
    let order = held_of(&made);
    taken(added, order)
}

/// The members of `added` at the positions `order` names, in its order.
fn taken(added: Vec<Annotation<RcStr>>, order: Vec<usize>) -> Vec<Annotation<RcStr>> {
    let mut added: Vec<Option<Annotation<RcStr>>> = added.into_iter().map(Some).collect();
    order.into_iter().map(|i| added[i].take().expect("each annotation once")).collect()
}

/// The positions in `added` of the annotations [`held`] holds, in its order.
fn held_of(added: &[Annotation<RcStr>]) -> Vec<usize> {
    let mut stored = HashedSet::new(added);
    for i in 0..added.len() {
        stored.add(i);
    }
    let mut sorted = SortedSet::new(added);
    for i in stored.into_iteration() {
        sorted.insert(i);
    }
    sorted.into_sorted()
}

fn made(mut a: Annotation<RcStr>) -> Annotation<RcStr> {
    a.av = match a.av {
        AnnotationValue::Literal(l) => AnnotationValue::Literal(crate::model::literal_as_made(l)),
        av => av,
    };
    a.ann = a.ann.into_iter().map(made).collect();
    a
}

/// The annotations `ont` holds of itself, in the order its readers see them:
/// by property and then by value, an untyped literal before the literal of
/// its text typed `xsd:string`.
pub fn held_by(ont: &Onto) -> Vec<Annotation<RcStr>> {
    let mut held: Vec<Annotation<RcStr>> = ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::OntologyAnnotation(OntologyAnnotation(a)) => Some(a.clone()),
            _ => None,
        })
        .collect();
    held.sort_by(|a, b| {
        iri_cmp(a.ap.0.as_ref(), b.ap.0.as_ref())
            .then_with(|| crate::io::owlfunc::cmp_annotation_value(&a.av, &b.av))
            .then_with(|| a.ann.cmp(&b.ann))
    });
    held
}

/// Add `added` to the annotations `ont` holds of itself, in order, as the
/// ontology adds annotations to those it holds.
pub fn add(ont: &mut Onto, added: impl IntoIterator<Item = Annotation<RcStr>>) {
    let before = held_by(ont);
    for a in &before {
        ont.remove(&Component::OntologyAnnotation(OntologyAnnotation(a.clone())).into());
    }
    for a in held(before.into_iter().chain(added)) {
        ont.insert(Component::OntologyAnnotation(OntologyAnnotation(a)));
    }
}

/// The hash set, of positions in `added`: 32 bins to begin with, twice as
/// many each time it holds three for every four bins, and a bin's members in
/// the order they came.
struct HashedSet<'a> {
    added: &'a [Annotation<RcStr>],
    bins: Vec<Vec<(u32, usize)>>,
    count: usize,
    threshold: usize,
}

impl<'a> HashedSet<'a> {
    fn new(added: &'a [Annotation<RcStr>]) -> Self {
        HashedSet { added, bins: vec![Vec::new(); 32], count: 0, threshold: 24 }
    }

    fn add(&mut self, i: usize) {
        let a = &self.added[i];
        let h = annotation_hash(a) as u32;
        let h = (h ^ (h >> 16)) & 0x7fff_ffff;
        let n = self.bins.len();
        let bin = &mut self.bins[h as usize & (n - 1)];
        if bin.iter().any(|&(eh, e)| eh == h && equal_annotations(a, &self.added[e])) {
            return;
        }
        bin.push((h, i));
        self.count += 1;
        if self.count >= self.threshold {
            self.grow();
        }
    }

    /// Twice the bins. A bin splits by the next bit of its members' hashes:
    /// the members at its end that go one way keep their order, and each
    /// member before them is put at the front of the bin it goes to.
    fn grow(&mut self) {
        let n = self.bins.len();
        let mut next = vec![Vec::new(); 2 * n];
        for (i, bin) in std::mem::take(&mut self.bins).into_iter().enumerate() {
            if bin.is_empty() {
                continue;
            }
            let high = |h: u32| h as usize & n != 0;
            let mut run = bin.len() - 1;
            while run > 0 && high(bin[run - 1].0) == high(bin[run].0) {
                run -= 1;
            }
            let mut lists = (Vec::new(), Vec::new());
            let mut members = bin.into_iter();
            let before: Vec<_> = members.by_ref().take(run).collect();
            let tail: Vec<_> = members.collect();
            if high(tail[0].0) {
                lists.1 = tail;
            } else {
                lists.0 = tail;
            }
            for member in before {
                let list = if high(member.0) { &mut lists.1 } else { &mut lists.0 };
                list.insert(0, member);
            }
            next[i] = lists.0;
            next[i + n] = lists.1;
        }
        self.bins = next;
        self.threshold = 2 * n - ((2 * n) >> 2);
    }

    fn into_iteration(self) -> impl Iterator<Item = usize> {
        self.bins.into_iter().flatten().map(|(_, i)| i)
    }
}

/// The sorted copy, of positions in `added`: a red-black tree each annotation
/// is inserted into, found along the path its comparisons take, and left out
/// where one finds it the same as an annotation already in the tree.
struct SortedSet<'a> {
    added: &'a [Annotation<RcStr>],
    nodes: Vec<Node>,
    root: Option<usize>,
}

struct Node {
    key: usize,
    left: Option<usize>,
    right: Option<usize>,
    parent: Option<usize>,
    red: bool,
}

impl<'a> SortedSet<'a> {
    fn new(added: &'a [Annotation<RcStr>]) -> Self {
        SortedSet { added, nodes: Vec::new(), root: None }
    }

    fn insert(&mut self, key: usize) {
        let Some(mut t) = self.root else {
            self.nodes.push(Node { key, left: None, right: None, parent: None, red: false });
            self.root = Some(0);
            return;
        };
        let (parent, left) = loop {
            match compare_annotations(&self.added[key], &self.added[self.nodes[t].key]) {
                Ordering::Equal => return,
                Ordering::Less => match self.nodes[t].left {
                    Some(l) => t = l,
                    None => break (t, true),
                },
                Ordering::Greater => match self.nodes[t].right {
                    Some(r) => t = r,
                    None => break (t, false),
                },
            }
        };
        let x = self.nodes.len();
        self.nodes.push(Node { key, left: None, right: None, parent: Some(parent), red: true });
        if left {
            self.nodes[parent].left = Some(x);
        } else {
            self.nodes[parent].right = Some(x);
        }
        self.balance(x);
    }

    fn parent(&self, x: Option<usize>) -> Option<usize> {
        x.and_then(|x| self.nodes[x].parent)
    }

    fn left(&self, x: Option<usize>) -> Option<usize> {
        x.and_then(|x| self.nodes[x].left)
    }

    fn right(&self, x: Option<usize>) -> Option<usize> {
        x.and_then(|x| self.nodes[x].right)
    }

    fn is_red(&self, x: Option<usize>) -> bool {
        x.is_some_and(|x| self.nodes[x].red)
    }

    fn paint(&mut self, x: Option<usize>, red: bool) {
        if let Some(x) = x {
            self.nodes[x].red = red;
        }
    }

    /// Restore the tree's balance after `x` is inserted.
    fn balance(&mut self, x: usize) {
        let mut x = Some(x);
        while x.is_some() && x != self.root && self.is_red(self.parent(x)) {
            let p = self.parent(x);
            let g = self.parent(p);
            if p == self.left(g) {
                let y = self.right(g);
                if self.is_red(y) {
                    self.paint(p, false);
                    self.paint(y, false);
                    self.paint(g, true);
                    x = g;
                } else {
                    if x == self.right(p) {
                        x = p;
                        self.rotate_left(x);
                    }
                    let p = self.parent(x);
                    let g = self.parent(p);
                    self.paint(p, false);
                    self.paint(g, true);
                    self.rotate_right(g);
                }
            } else {
                let y = self.left(g);
                if self.is_red(y) {
                    self.paint(p, false);
                    self.paint(y, false);
                    self.paint(g, true);
                    x = g;
                } else {
                    if x == self.left(p) {
                        x = p;
                        self.rotate_right(x);
                    }
                    let p = self.parent(x);
                    let g = self.parent(p);
                    self.paint(p, false);
                    self.paint(g, true);
                    self.rotate_left(g);
                }
            }
        }
        let root = self.root;
        self.paint(root, false);
    }

    fn rotate_left(&mut self, p: Option<usize>) {
        let Some(p) = p else { return };
        let r = self.nodes[p].right.expect("a left rotation has a right child");
        let rl = self.nodes[r].left;
        self.nodes[p].right = rl;
        if let Some(rl) = rl {
            self.nodes[rl].parent = Some(p);
        }
        self.replace_child(p, r);
        self.nodes[r].left = Some(p);
        self.nodes[p].parent = Some(r);
    }

    fn rotate_right(&mut self, p: Option<usize>) {
        let Some(p) = p else { return };
        let l = self.nodes[p].left.expect("a right rotation has a left child");
        let lr = self.nodes[l].right;
        self.nodes[p].left = lr;
        if let Some(lr) = lr {
            self.nodes[lr].parent = Some(p);
        }
        self.replace_child(p, l);
        self.nodes[l].right = Some(p);
        self.nodes[p].parent = Some(l);
    }

    /// Put `c` where `p` hangs from its parent, or at the root.
    fn replace_child(&mut self, p: usize, c: usize) {
        let pp = self.nodes[p].parent;
        self.nodes[c].parent = pp;
        match pp {
            None => self.root = Some(c),
            Some(pp) if self.nodes[pp].left == Some(p) => self.nodes[pp].left = Some(c),
            Some(pp) => self.nodes[pp].right = Some(c),
        }
    }

    fn into_sorted(self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut stack = Vec::new();
        let mut at = self.root;
        while at.is_some() || !stack.is_empty() {
            while let Some(x) = at {
                stack.push(x);
                at = self.nodes[x].left;
            }
            let x = stack.pop().expect("a node to visit");
            order.push(self.nodes[x].key);
            at = self.nodes[x].right;
        }
        order
    }
}

/// `a` against `b` as the sort compares them: the same when they are equal,
/// and otherwise by property and then by value.
fn compare_annotations(a: &Annotation<RcStr>, b: &Annotation<RcStr>) -> Ordering {
    if equal_annotations(a, b) {
        return Ordering::Equal;
    }
    iri_cmp(a.ap.0.as_ref(), b.ap.0.as_ref()).then_with(|| compare_values(&a.av, &b.av))
}

fn compare_values(a: &AnnotationValue<RcStr>, b: &AnnotationValue<RcStr>) -> Ordering {
    fn rank(v: &AnnotationValue<RcStr>) -> u8 {
        match v {
            AnnotationValue::IRI(_) => 0,
            AnnotationValue::AnonymousIndividual(_) => 1,
            AnnotationValue::Literal(_) => 2,
        }
    }
    match (a, b) {
        (AnnotationValue::IRI(x), AnnotationValue::IRI(y)) => iri_cmp(x.as_ref(), y.as_ref()),
        (AnnotationValue::AnonymousIndividual(x), AnnotationValue::AnonymousIndividual(y)) => {
            x.0.as_ref().cmp(y.0.as_ref())
        }
        (AnnotationValue::Literal(x), AnnotationValue::Literal(y)) => compare_literals(x, y),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// `a` against `b` by datatype, text and language. A literal typed
/// `xsd:string` is compared so whatever it is compared with; any other is the
/// same as a literal equal to it.
fn compare_literals(a: &Literal<RcStr>, b: &Literal<RcStr>) -> Ordering {
    if datatype(a) != XSD_STRING && equal_literals(a, b) {
        return Ordering::Equal;
    }
    iri_cmp(datatype(a), datatype(b))
        .then_with(|| a.literal().cmp(b.literal()))
        .then_with(|| language(a).cmp(language(b)))
}

fn datatype(l: &Literal<RcStr>) -> &str {
    match l {
        Literal::Datatype { datatype_iri, .. } => datatype_iri.as_ref(),
        Literal::Simple { .. } | Literal::Language { .. } => RDF_PLAIN_LITERAL,
    }
}

fn language(l: &Literal<RcStr>) -> &str {
    match l {
        Literal::Language { lang, .. } => lang.as_str(),
        _ => "",
    }
}

/// Two annotations are equal when their properties, their values and the
/// annotations each carries are.
fn equal_annotations(a: &Annotation<RcStr>, b: &Annotation<RcStr>) -> bool {
    a.ap == b.ap
        && equal_values(&a.av, &b.av)
        && a.ann.len() == b.ann.len()
        && a.ann.iter().all(|x| b.ann.iter().any(|y| equal_annotations(x, y)))
}

fn equal_values(a: &AnnotationValue<RcStr>, b: &AnnotationValue<RcStr>) -> bool {
    match (a, b) {
        (AnnotationValue::Literal(x), AnnotationValue::Literal(y)) => equal_literals(x, y),
        _ => a == b,
    }
}

/// Two literals are equal when they are the same literal, or one is untyped
/// and the other the literal of its text typed `xsd:string`.
fn equal_literals(a: &Literal<RcStr>, b: &Literal<RcStr>) -> bool {
    let untyped = |l: &Literal<RcStr>| match l {
        Literal::Datatype { literal, datatype_iri } if datatype_iri.as_ref() == XSD_STRING => {
            Some(literal.clone())
        }
        Literal::Simple { literal } => Some(literal.clone()),
        _ => None,
    };
    a == b || untyped(a).is_some_and(|x| untyped(b) == Some(x))
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::Build;

    const COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
    const LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
    const SEE_ALSO: &str = "http://www.w3.org/2000/01/rdf-schema#seeAlso";

    fn plain(s: &str) -> Literal<RcStr> {
        Literal::Simple { literal: s.into() }
    }

    fn typed(b: &Build<RcStr>, s: &str) -> Literal<RcStr> {
        Literal::Datatype { literal: s.into(), datatype_iri: b.iri(XSD_STRING) }
    }

    fn ann(b: &Build<RcStr>, prop: &str, l: Literal<RcStr>, label: Option<&str>) -> Annotation<RcStr> {
        Annotation {
            ap: b.annotation_property(prop),
            av: AnnotationValue::Literal(l),
            ann: label.map(|x| ann(b, LABEL, plain(x), None)).into_iter().collect(),
        }
    }

    /// Each case: what is added in order, and what is held, in its order.
    #[test]
    fn of_one_property_and_value_the_first_added_is_held() {
        let b = Build::new_rc();
        let bare = ann(&b, COMMENT, plain("c"), None);
        let annotated = ann(&b, COMMENT, plain("c"), Some("x"));
        assert_eq!(held([bare.clone(), annotated.clone()]), vec![bare.clone()]);
        assert_eq!(held([annotated.clone(), bare]), vec![annotated]);
    }

    #[test]
    fn an_untyped_literal_after_the_typed_one_is_not_held() {
        let b = Build::new_rc();
        let ta = ann(&b, COMMENT, typed(&b, "e"), Some("x"));
        let pb = ann(&b, COMMENT, plain("e"), None);
        let pa = ann(&b, COMMENT, plain("e"), Some("x"));
        let tb = ann(&b, COMMENT, typed(&b, "e"), None);
        assert_eq!(held([ta.clone(), pb.clone()]), vec![ta.clone()]);
        assert_eq!(held([pa.clone(), tb.clone()]), vec![pa.clone(), tb.clone()]);
        assert_eq!(held([pb.clone(), ta.clone()]), vec![pb, ta]);
        assert_eq!(held([tb.clone(), pa]), vec![tb]);
    }

    /// The hash set iterates `"f"@en`, `"d"`, `"e"`, `"c"`, then the IRI; the
    /// tree's path keeps the untyped `"e"` though the typed one is in it.
    #[test]
    fn the_tree_decides_which_of_an_untyped_and_a_typed_literal_is_held() {
        let b = Build::new_rc();
        let lang = |l: &str| Literal::Language { literal: "f".into(), lang: l.into() };
        let iri = |label: Option<&str>| Annotation {
            ap: b.annotation_property(SEE_ALSO),
            av: AnnotationValue::IRI(b.iri("http://example.org/a")),
            ann: label.map(|x| ann(&b, LABEL, plain(x), None)).into_iter().collect(),
        };
        let added = [
            ann(&b, COMMENT, plain("c"), None),
            ann(&b, COMMENT, typed(&b, "c"), None),
            ann(&b, COMMENT, typed(&b, "d"), None),
            ann(&b, COMMENT, plain("d"), None),
            ann(&b, COMMENT, typed(&b, "e"), Some("x")),
            ann(&b, COMMENT, plain("e"), None),
            ann(&b, COMMENT, lang("en"), None),
            ann(&b, COMMENT, lang("en"), Some("z")),
            iri(None),
            iri(Some("w")),
        ];
        assert_eq!(
            held(added),
            vec![
                ann(&b, COMMENT, plain("c"), None),
                ann(&b, COMMENT, plain("e"), None),
                ann(&b, COMMENT, lang("en"), None),
                ann(&b, COMMENT, typed(&b, "d"), None),
                ann(&b, COMMENT, typed(&b, "e"), Some("x")),
                iri(None),
            ]
        );
    }

    #[test]
    fn annotations_are_hashed_as_the_hash_set_hashes_them() {
        let b = Build::new_rc();
        assert_eq!(annotation_hash(&ann(&b, COMMENT, plain("e"), None)), -1755320762);
        assert_eq!(annotation_hash(&ann(&b, COMMENT, typed(&b, "e"), Some("x"))), -1755320762);
        assert_eq!(
            annotation_hash(&ann(&b, COMMENT, Literal::Language { literal: "f".into(), lang: "en".into() }, None)),
            -1139504421
        );
    }
}
