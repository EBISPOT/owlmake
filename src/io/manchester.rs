//! OWL 2 Manchester Syntax (`.omn`) reading, and the class-expression parser
//! the DOSDP engine and `explain` use.
//!
//! A document is parsed whole: prefixes, the ontology header, every frame and
//! section, axiom annotations, rules. A frame's list states one axiom per item —
//! `DisjointWith: B, C` under `A` is `DisjointClasses(A B)` and
//! `DisjointClasses(A C)` — and the members of an n-ary axiom are a set, so the
//! same relation stated from both of its frames is one axiom. The writer is
//! [`crate::io::manchester_write`].

use std::io::BufRead;

use anyhow::Result;
use horned_owl::io::ParserConfiguration;
use horned_owl::model::{
    AnnotatedComponent, AnonymousIndividual, Build, ClassExpression as CE, Component, DataProperty, DifferentIndividuals,
    DisjointClasses, DisjointDataProperties, DisjointObjectProperties, EquivalentClasses,
    EquivalentDataProperties, EquivalentObjectProperties, Individual, InverseObjectProperties,
    MutableOntology, ObjectPropertyExpression as OPE, RcStr, SameIndividual,
};
use horned_owl::ontology::set::SetOntology;
use horned_owl::visitor::mutable::{VisitMut, WalkMut};

use crate::io::natural_order::{iri_cmp, NaturalOrder};
use crate::model::{Model, Onto};

// === Reader ==============================================================

/// Load an ontology from Manchester Syntax.
///
/// Node ids are local to the document: every `_:label` is re-minted, in order of
/// first mention, from the blank-node counter, exactly as a functional-syntax
/// read does.
pub fn load<R: BufRead>(mut reader: R, cfg: ParserConfiguration<RcStr>) -> Result<Model> {
    let mut text = String::new();
    reader.read_to_string(&mut text)?;
    let (text, labels) = remint_node_ids(&text);
    let (ont, prefixes): (Onto, horned_owl::curie::PrefixMapping) =
        horned_owl::io::omn::read(text.as_bytes(), cfg)
            .map_err(|e| anyhow::anyhow!("Manchester syntax parse error: {e}"))?;
    let declared: Vec<(String, String)> = prefixes.mappings().map(|(p, ns)| (p.clone(), ns.clone())).collect();
    let mut model = Model::from_parts(normalise(ont), prefixes);
    // The document's `Prefix:` declarations are its format prefixes, which a
    // write in another format carries over.
    model.rdf_prefixes = declared;
    model.anon_doc_order = labels;
    Ok(model)
}

/// The byte spans of the `_:label` node ids a Manchester document states, in
/// document order. A `_:` inside a quoted string or a full IRI is not one, nor
/// is one that continues a name; a `<` followed by a space or `=` is a facet,
/// not the start of an IRI.
fn node_id_spans(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let name_byte = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':');
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'<' if i + 1 < b.len() && !matches!(b[i + 1], b' ' | b'=' | b'\t' | b'\n' | b'\r') => {
                while i < b.len() && b[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            b'_' if i + 1 < b.len() && b[i + 1] == b':' && (i == 0 || !name_byte(b[i - 1])) => {
                let s = i;
                let mut e = s + 2;
                while e < b.len() && (b[e].is_ascii_alphanumeric() || matches!(b[e], b'_' | b'-' | b'.')) {
                    e += 1;
                }
                while e > s + 2 && b[e - 1] == b'.' {
                    e -= 1;
                }
                if e > s + 2 {
                    out.push((s, e));
                }
                i = e.max(s + 2);
            }
            _ => i += 1,
        }
    }
    out
}

/// Re-mint a Manchester document's node ids, returning the rewritten document
/// and its new labels in first-mention order.
fn remint_node_ids(text: &str) -> (String, Vec<String>) {
    let spans = node_id_spans(text);
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for &(s, e) in &spans {
        let n = index.len();
        index.entry(&text[s + 2..e]).or_insert(n);
    }
    let base = super::mint_anon_ids(index.len());
    let labels: Vec<String> = (0..index.len()).map(|k| format!("genid{}", base + k as u64)).collect();
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for &(s, e) in &spans {
        out.push_str(&text[last..s]);
        out.push_str("_:");
        out.push_str(&labels[index[&text[s + 2..e]]]);
        last = e;
    }
    out.push_str(&text[last..]);
    (out, labels)
}

/// Puts `_:` back on the node ids the parser strips it from, so an anonymous
/// individual read from Manchester is the one a functional-syntax read of the
/// same id gives.
struct NodeIds;

impl VisitMut<RcStr> for NodeIds {
    fn visit_anonymous_individual(&mut self, a: &mut AnonymousIndividual<RcStr>) {
        if !a.0.starts_with("_:") {
            a.0 = RcStr::from(format!("_:{}", &*a.0));
        }
    }
}

/// `ont` with every node id spelled with its `_:`, and the members of every
/// n-ary axiom in natural order, each once.
fn normalise(ont: Onto) -> Onto {
    use Component as C;
    let order = NaturalOrder::default();
    fn members<T: Clone>(v: &[T], cmp: impl Fn(&T, &T) -> std::cmp::Ordering) -> Vec<T> {
        crate::io::natural_order::sorted_set(v, cmp).into_iter().cloned().collect()
    }
    let mut walk = WalkMut::new(NodeIds);
    let mut out: Onto = SetOntology::new();
    for mut ac in ont {
        walk.annotated_component(&mut ac);
        let AnnotatedComponent { component, ann } = ac;
        let component = match component {
            C::EquivalentClasses(x) => C::EquivalentClasses(EquivalentClasses(members(&x.0, |a, b| order.ce(a, b)))),
            C::DisjointClasses(x) => C::DisjointClasses(DisjointClasses(members(&x.0, |a, b| order.ce(a, b)))),
            C::EquivalentObjectProperties(x) => {
                C::EquivalentObjectProperties(EquivalentObjectProperties(members(&x.0, |a, b| order.ope(a, b))))
            }
            C::DisjointObjectProperties(x) => {
                C::DisjointObjectProperties(DisjointObjectProperties(members(&x.0, |a, b| order.ope(a, b))))
            }
            C::EquivalentDataProperties(x) => C::EquivalentDataProperties(EquivalentDataProperties(members(
                &x.0,
                |a: &DataProperty<RcStr>, b| iri_cmp(a.0.as_ref(), b.0.as_ref()),
            ))),
            C::DisjointDataProperties(x) => C::DisjointDataProperties(DisjointDataProperties(members(
                &x.0,
                |a: &DataProperty<RcStr>, b| iri_cmp(a.0.as_ref(), b.0.as_ref()),
            ))),
            C::SameIndividual(x) => C::SameIndividual(SameIndividual(members(&x.0, |a, b| order.individual(a, b)))),
            C::DifferentIndividuals(x) => {
                C::DifferentIndividuals(DifferentIndividuals(members(&x.0, |a, b| order.individual(a, b))))
            }
            C::InverseObjectProperties(InverseObjectProperties(p, q)) => {
                if order.ope(&p, &q) == std::cmp::Ordering::Greater {
                    C::InverseObjectProperties(InverseObjectProperties(q, p))
                } else {
                    C::InverseObjectProperties(InverseObjectProperties(p, q))
                }
            }
            other => other,
        };
        out.insert(AnnotatedComponent { component, ann });
    }
    out
}

// --- Class-expression parser (recursive descent) ------------------------

fn parse_ce(b: &Build<RcStr>, prefixes: &horned_owl::curie::PrefixMapping, s: &str) -> Option<CE<RcStr>> {
    let toks = tokenize(s);
    let mut p = Parser { toks, pos: 0, b, prefixes };
    let ce = p.parse_or()?;
    Some(ce)
}

/// Parse a Manchester class expression from a string (public entry point,
/// reused by the DOSDP pattern engine).
pub fn parse_class_expression(
    b: &Build<RcStr>,
    prefixes: &horned_owl::curie::PrefixMapping,
    s: &str,
) -> Option<CE<RcStr>> {
    parse_ce(b, prefixes, s)
}

struct Parser<'a> {
    toks: Vec<String>,
    pos: usize,
    b: &'a Build<RcStr>,
    prefixes: &'a horned_owl::curie::PrefixMapping,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&str> {
        self.toks.get(self.pos).map(|s| s.as_str())
    }
    fn next(&mut self) -> Option<String> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn parse_or(&mut self) -> Option<CE<RcStr>> {
        let mut parts = vec![self.parse_and()?];
        while self.peek() == Some("or") {
            self.next();
            parts.push(self.parse_and()?);
        }
        if parts.len() == 1 {
            Some(parts.pop().unwrap())
        } else {
            Some(CE::ObjectUnionOf(parts))
        }
    }
    fn parse_and(&mut self) -> Option<CE<RcStr>> {
        let mut parts = vec![self.parse_primary()?];
        while self.peek() == Some("and") {
            self.next();
            parts.push(self.parse_primary()?);
        }
        if parts.len() == 1 {
            Some(parts.pop().unwrap())
        } else {
            Some(CE::ObjectIntersectionOf(parts))
        }
    }
    fn parse_primary(&mut self) -> Option<CE<RcStr>> {
        let t = self.next()?;
        match t.as_str() {
            "(" => {
                let inner = self.parse_or()?;
                if self.peek() == Some(")") {
                    self.next();
                }
                Some(inner)
            }
            "not" => Some(CE::ObjectComplementOf(Box::new(self.parse_primary()?))),
            _ => {
                // `t` is a property or class name. Look ahead for some/only/value.
                match self.peek() {
                    Some("some") => {
                        self.next();
                        let filler = self.parse_primary()?;
                        Some(CE::ObjectSomeValuesFrom {
                            ope: OPE::ObjectProperty(self.b.object_property(resolve(self.prefixes, &t))),
                            bce: Box::new(filler),
                        })
                    }
                    Some("only") => {
                        self.next();
                        let filler = self.parse_primary()?;
                        Some(CE::ObjectAllValuesFrom {
                            ope: OPE::ObjectProperty(self.b.object_property(resolve(self.prefixes, &t))),
                            bce: Box::new(filler),
                        })
                    }
                    Some("value") => {
                        self.next();
                        let ind = self.next()?;
                        Some(CE::ObjectHasValue {
                            ope: OPE::ObjectProperty(self.b.object_property(resolve(self.prefixes, &t))),
                            i: Individual::Named(
                                self.b.named_individual(resolve(self.prefixes, &ind)),
                            ),
                        })
                    }
                    _ => Some(CE::Class(self.b.class(resolve(self.prefixes, &t)))),
                }
            }
        }
    }
}

/// Tokenize a Manchester class expression: parentheses, `<iri>`, and words.
fn tokenize(s: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' => {
                chars.next();
            }
            '(' | ')' => {
                toks.push(c.to_string());
                chars.next();
            }
            '<' => {
                let mut iri = String::from("<");
                chars.next();
                for d in chars.by_ref() {
                    iri.push(d);
                    if d == '>' {
                        break;
                    }
                }
                toks.push(iri);
            }
            _ => {
                let mut word = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_whitespace() || d == '(' || d == ')' {
                        break;
                    }
                    word.push(d);
                    chars.next();
                }
                toks.push(word);
            }
        }
    }
    toks
}

/// Resolve a Manchester token (`<iri>` or `prefix:local`) to a full IRI.
fn resolve(prefixes: &horned_owl::curie::PrefixMapping, tok: &str) -> String {
    let tok = tok.trim();
    if let Some(inner) = tok.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
        return inner.to_string();
    }
    if let Ok(expanded) = prefixes.expand_curie_string(tok) {
        return expanded;
    }
    crate::io::obo::expand_id(tok)
}
