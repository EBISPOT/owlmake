//! Reading RDF/XML.
//!
//! [`parse`] turns the document into statements in the order it completes
//! them, naming each blank node as it is made, and [`order`] follows the
//! reader's translation of those statements to tell which name each
//! anonymous individual takes.

pub(crate) mod order;
pub(crate) mod parse;

use anyhow::Result;
use oxigraph::model::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Triple};

use order::Term;

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// A document read: its statements, in the order the parse made them, and
/// what reading them names.
pub(crate) struct Read {
    /// The statements, their predicates and objects in the current vocabulary
    /// ([`order::Order::synonym`]). A blank node that is an anonymous
    /// individual is labelled with the name it takes; any other keeps the
    /// parse's label.
    pub statements: Vec<Triple>,
    /// The order its statements are read in, beyond the order the parse made
    /// them: the axiom nodes and `owl:Annotation` nodes, by label, in the order
    /// their annotations are read, and the statements whose literal the
    /// document types `xsd:string`.
    pub order: horned_owl::io::rdf::reader::StatementOrder,
    /// The count after the document: the next blank node's number.
    pub next: u64,
    /// The prefixes the document declares, in the order it declares them: the
    /// entities of its internal subset, then its namespace declarations (the
    /// default namespace's name is empty).
    pub prefixes: Vec<(String, String)>,
}

enum Object {
    Node(Term),
    Literal { value: String, lang: Option<String>, datatype: Option<Term> },
}

/// Read an RDF/XML document, numbering its blank nodes and anonymous
/// individuals from `first_id`, its relative IRIs resolved against `base`, the
/// document's own IRI, where it states no `xml:base`. With `trace`, print each
/// statement to stderr as the parse makes it — `T s p o` for a node object,
/// `L s p "v" @lang ^^datatype` for a literal — and each anonymous
/// individual as it is named, `MINT _:genid<n> for node`.
pub(crate) fn read(bytes: &[u8], first_id: u64, trace: bool, base: Option<String>) -> Result<Read> {
    struct Record {
        order: order::Order,
        statements: Vec<(Term, Term, Object)>,
        /// The subject, predicate and text of every untyped literal statement
        /// and every one typed `xsd:string`, which are one literal: of the two,
        /// the subject's statement is the one the document makes first.
        strings: std::collections::HashSet<(Term, Term, String)>,
        typed_strings: std::collections::HashSet<usize>,
        prefixes: Vec<(String, String)>,
        trace: bool,
    }
    impl parse::Sink for Record {
        fn next_id(&mut self) -> u64 {
            self.order.next_id()
        }
        fn resource(&mut self, s: &str, p: &str, o: &str) -> Result<()> {
            if self.trace {
                eprintln!("T {s} {p} {o}");
            }
            let (st, pt, ot) = (self.order.term(s), self.order.term(p), self.order.term(o));
            let (pt, ot) = (self.order.synonym(pt), self.order.synonym(ot));
            self.statements.push((st, pt, Object::Node(ot)));
            self.order.resource(s, p, o);
            Ok(())
        }
        fn literal(&mut self, s: &str, p: &str, v: &str, lang: Option<&str>, dt: Option<&str>) -> Result<()> {
            if self.trace {
                eprintln!("{}", literal_line(s, p, v, lang, dt));
            }
            let (st, pt) = (self.order.term(s), self.order.term(p));
            let pt = self.order.synonym(pt);
            let datatype = dt.map(|d| self.order.term(d));
            let tag = lang.filter(|l| !l.is_empty()).map(str::to_ascii_lowercase);
            let typed = dt == Some(XSD_STRING);
            let string = typed || (dt.is_none() && tag.is_none());
            if !string || self.strings.insert((st, pt, v.to_string())) {
                if typed {
                    self.typed_strings.insert(self.statements.len());
                }
                self.statements.push((st, pt, Object::Literal { value: v.to_string(), lang: tag, datatype }));
            }
            self.order.literal(s, p, v, lang, dt);
            Ok(())
        }
        fn prefix(&mut self, name: &str, namespace: &str) {
            self.prefixes.push((name.to_string(), namespace.to_string()));
        }
    }
    let mut record = Record {
        order: order::Order::new(first_id, trace),
        statements: Vec::new(),
        strings: Default::default(),
        typed_strings: Default::default(),
        prefixes: Vec::new(),
        trace,
    };
    parse::Parser::new(&mut record, base).parse(bytes)?;
    let names = record.order.end();
    let order = &record.order;
    let label = |t: Term| -> String {
        let name = order.name(t);
        match names.individuals.get(name) {
            Some(n) => format!("genid{n}"),
            None => name.strip_prefix("_:").unwrap_or(name).to_string(),
        }
    };
    let node = |t: Term| -> NamedOrBlankNode {
        let name = order.name(t);
        if name.starts_with("_:") {
            BlankNode::new_unchecked(label(t)).into()
        } else {
            NamedNode::new_unchecked(name).into()
        }
    };
    let statements = record
        .statements
        .iter()
        .map(|(s, p, o)| {
            let object: oxigraph::model::Term = match o {
                Object::Node(o) => node(*o).into(),
                Object::Literal { value, lang, datatype } => match (datatype, lang) {
                    (Some(d), _) => Literal::new_typed_literal(value.clone(), NamedNode::new_unchecked(order.name(*d))).into(),
                    (None, Some(l)) => Literal::new_language_tagged_literal_unchecked(value.clone(), l.clone()).into(),
                    (None, None) => Literal::new_simple_literal(value.clone()).into(),
                },
            };
            Triple::new(node(*s), NamedNode::new_unchecked(order.name(*p)), object)
        })
        .collect();
    let unlabel = |nodes: &[String]| -> Vec<String> {
        nodes.iter().map(|n| n.strip_prefix("_:").unwrap_or(n).to_string()).collect()
    };
    let order = horned_owl::io::rdf::reader::StatementOrder {
        axiom_nodes: unlabel(&names.axiom_nodes),
        annotation_nodes: unlabel(&names.annotation_nodes),
        typed_strings: record.typed_strings,
    };
    Ok(Read { statements, order, next: names.next, prefixes: record.prefixes })
}

fn literal_line(s: &str, p: &str, v: &str, lang: Option<&str>, dt: Option<&str>) -> String {
    let mut line = format!("L {s} {p} \"");
    for c in v.chars() {
        match c {
            '\\' | '"' => {
                line.push('\\');
                line.push(c);
            }
            '\n' => line.push_str("\\n"),
            '\r' => line.push_str("\\r"),
            '\t' => line.push_str("\\t"),
            c => line.push(c),
        }
    }
    line.push('"');
    if let Some(l) = lang {
        line.push_str(&format!(" @{l}"));
    }
    if let Some(d) = dt {
        line.push_str(&format!(" ^^{d}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anonymous individuals are named in the order the reader translates them,
    /// as OWL API 4.5.29 names them: the node typed with a named class as the
    /// parse makes that statement, the other two once the document is complete,
    /// in the hash order of the table of literal statements — the third node
    /// before the second. The parse's own blank nodes draw from the same count.
    #[test]
    fn anonymous_individuals_are_named_in_the_order_they_are_translated() {
        let doc = r#"<?xml version="1.0"?>
<rdf:RDF xmlns="http://example.org/anon#"
     xmlns:owl="http://www.w3.org/2002/07/owl#"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#">
    <owl:Ontology rdf:about="http://example.org/anon"/>
    <owl:DatatypeProperty rdf:about="http://example.org/anon#d"/>
    <owl:Class rdf:about="http://example.org/anon#A"/>
    <rdf:Description>
        <rdf:type rdf:resource="http://example.org/anon#A"/>
    </rdf:Description>
    <rdf:Description>
        <rdfs:label>two</rdfs:label>
    </rdf:Description>
    <rdf:Description>
        <d>3</d>
    </rdf:Description>
</rdf:RDF>
"#;
        let read = read(doc.as_bytes(), 2147483648, false, None).unwrap();
        let subject = |p: &str| -> String {
            let t = read.statements.iter().find(|t| t.predicate.as_str() == p && t.subject.is_blank_node()).unwrap();
            t.subject.to_string()
        };
        assert_eq!(subject("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"), "_:genid2147483649");
        assert_eq!(subject("http://example.org/anon#d"), "_:genid2147483652");
        assert_eq!(subject("http://www.w3.org/2000/01/rdf-schema#label"), "_:genid2147483653");
        assert_eq!(read.next, 2147483654);
    }
}
