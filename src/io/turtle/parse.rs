//! Reading Turtle and N-Triples into statements.
//!
//! [`read`] parses a document one statement at a time and hands each to a
//! [`Sink`] in the order the parse completes it. The statement that names an
//! object written `[ … ]` or `( … )` comes before the statements inside it, and
//! a list cell is typed `rdf:List` just before its first `rdf:first` or
//! `rdf:rest` statement, unless the document has typed it so already.
//!
//! The reader files statements in hash tables keyed on node names, so the
//! names blank nodes take decide the order it translates them in, and so the
//! order its anonymous individuals are numbered in. A node the document leaves
//! unlabelled — `[ … ]`, `[]` or a list cell — is `_:genid-nodeid-node1hf7uaq00x<n>`,
//! `n` counting the run's unlabelled nodes from 1. A node labelled `_:l` is
//! `_:genid-nodeid-genid-0123456789ab4cde8f0123456789abcd-l`, a label longer
//! than 32 UTF-16 units replaced by the upper-case hex of its MD5 digest.
//!
//! A prefixed name may use any of [`PREDECLARED`] without the document
//! declaring it.

use std::collections::{HashMap, HashSet};
use std::fmt;

use anyhow::{anyhow, Result};
use md5::{Digest, Md5};

use super::iri::Iri;
use crate::io::rdfxml::parse::Sink;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const RDF_LIST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#List";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
const XSD_DOUBLE: &str = "http://www.w3.org/2001/XMLSchema#double";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";

/// What an unlabelled node's name starts with; its number follows.
const UNLABELLED: &str = "node1hf7uaq00x";
/// What a labelled node's name starts with; its label follows.
const LABELLED: &str = "genid-0123456789ab4cde8f0123456789abcd-";

/// The characters a local name may escape with a backslash.
const LOCAL_ESCAPED: &str = "_~.-!$&'()*+,;=/?#@%";

/// The prefixes a document may use without declaring them, and their
/// namespaces.
const PREDECLARED: [(&str, &str); 55] = [
    ("as", "https://www.w3.org/ns/activitystreams#"),
    ("cat", "http://www.w3.org/ns/dcat#"),
    ("cc", "http://creativecommons.org/ns#"),
    ("cnt", "http://www.w3.org/2008/content#"),
    ("csvw", "http://www.w3.org/ns/csvw#"),
    ("ctag", "http://commontag.org/ns#"),
    ("dc", "http://purl.org/dc/terms/"),
    ("dc11", "http://purl.org/dc/elements/1.1/"),
    ("dcat", "http://www.w3.org/ns/dcat#"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("describedby", "http://www.w3.org/2007/05/powder-s#describedby"),
    ("dqv", "http://www.w3.org/ns/dqv#"),
    ("duv", "https://www.w3.org/TR/vocab-duv#"),
    ("earl", "http://www.w3.org/ns/earl#"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
    ("gldp", "http://www.w3.org/ns/people#"),
    ("gr", "http://purl.org/goodrelations/v1#"),
    ("grddl", "http://www.w3.org/2003/g/data-view#"),
    ("ht", "http://www.w3.org/2006/http#"),
    ("ical", "http://www.w3.org/2002/12/cal/icaltzd#"),
    ("jsonld", "http://www.w3.org/ns/json-ld#"),
    ("ldp", "http://www.w3.org/ns/ldp#"),
    ("license", "http://www.w3.org/1999/xhtml/vocab#license"),
    ("ma", "http://www.w3.org/ns/ma-ont#"),
    ("oa", "http://www.w3.org/ns/oa#"),
    ("odrl", "http://www.w3.org/ns/odrl/2/"),
    ("og", "http://ogp.me/ns#"),
    ("org", "http://www.w3.org/ns/org#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("prov", "http://www.w3.org/ns/prov#"),
    ("ptr", "http://www.w3.org/2009/pointers#"),
    ("qb", "http://purl.org/linked-data/cube#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfa", "http://www.w3.org/ns/rdfa#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("rev", "http://purl.org/stuff/rev#"),
    ("rif", "http://www.w3.org/2007/rif#"),
    ("role", "http://www.w3.org/1999/xhtml/vocab#role"),
    ("rr", "http://www.w3.org/ns/r2rml#"),
    ("schema", "http://schema.org/"),
    ("sd", "http://www.w3.org/ns/sparql-service-description#"),
    ("sioc", "http://rdfs.org/sioc/ns#"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("skosxl", "http://www.w3.org/2008/05/skos-xl#"),
    ("sosa", "http://www.w3.org/ns/sosa/"),
    ("ssn", "http://www.w3.org/ns/ssn/"),
    ("time", "http://www.w3.org/2006/time#"),
    ("v", "http://rdf.data-vocabulary.org/#"),
    ("vcard", "http://www.w3.org/2006/vcard/ns#"),
    ("void", "http://rdfs.org/ns/void#"),
    ("wdr", "http://www.w3.org/2007/05/powder#"),
    ("wdrs", "http://www.w3.org/2007/05/powder-s#"),
    ("xhv", "http://www.w3.org/1999/xhtml/vocab#"),
    ("xml", "http://www.w3.org/XML/1998/namespace"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
];

/// Read the document `text` into `sink`, its relative IRIs resolved against
/// `base`. Its unlabelled nodes are numbered from `*unlabelled`, which is
/// left at the number after the last.
pub(crate) fn read(text: &str, base: Option<&str>, unlabelled: &mut u64, sink: &mut impl Sink) -> Result<()> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut parser = Parser {
        text,
        pos: 0,
        pushback: Vec::new(),
        line: 1,
        base: None,
        namespaces: PREDECLARED.iter().map(|(p, ns)| (p.to_string(), ns.to_string())).collect(),
        subject: None,
        predicate: None,
        unlabelled: *unlabelled,
        typed_lists: HashSet::new(),
        sink,
    };
    if let Some(base) = base {
        parser.base = Some(Iri::lenient(base).map_err(|e| anyhow!("{e}"))?);
    }
    let read = parser.document();
    *unlabelled = parser.unlabelled;
    read
}

/// A subject or object node.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Node {
    Iri(String),
    /// A blank node, by the identifier its name carries.
    Blank(String),
}

impl Node {
    fn name(&self) -> String {
        match self {
            Node::Iri(iri) => iri.clone(),
            Node::Blank(id) => format!("_:genid-nodeid-{id}"),
        }
    }
}

#[derive(Debug)]
enum Value {
    Node(Node),
    Literal { label: String, lang: Option<String>, datatype: Option<String> },
}

impl Value {
    fn typed(label: String, datatype: &str) -> Value {
        Value::Literal { label, lang: None, datatype: Some(datatype.to_string()) }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Node(Node::Iri(iri)) => write!(f, "{iri}"),
            Value::Node(Node::Blank(id)) => write!(f, "_:{id}"),
            Value::Literal { label, lang: Some(lang), .. } => write!(f, "\"{label}\"@{lang}"),
            Value::Literal { label, datatype: Some(d), .. } => write!(f, "\"{label}\"^^<{d}>"),
            Value::Literal { label, .. } => write!(f, "\"{label}\"^^<{XSD_STRING}>"),
        }
    }
}

struct Parser<'t, 's, S: Sink> {
    text: &'t str,
    pos: usize,
    /// Characters put back, the next one to read last.
    pushback: Vec<char>,
    line: usize,
    base: Option<Iri>,
    namespaces: HashMap<String, String>,
    subject: Option<Node>,
    predicate: Option<String>,
    unlabelled: u64,
    /// The subjects already typed `rdf:List`.
    typed_lists: HashSet<Node>,
    sink: &'s mut S,
}

impl<S: Sink> Parser<'_, '_, S> {
    // -- characters -------------------------------------------------------------

    fn read(&mut self) -> Option<char> {
        if let Some(c) = self.pushback.pop() {
            return Some(c);
        }
        let c = self.text[self.pos..].chars().next()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn unread(&mut self, c: Option<char>) {
        if let Some(c) = c {
            self.pushback.push(c);
        }
    }

    fn unread_str(&mut self, s: &str) {
        self.pushback.extend(s.chars().rev());
    }

    fn peek(&mut self) -> Option<char> {
        let c = self.read();
        self.unread(c);
        c
    }

    fn fatal(&self, message: impl fmt::Display) -> anyhow::Error {
        anyhow!("{message} [line {}]", self.line)
    }

    fn eof(&self) -> anyhow::Error {
        anyhow!("Unexpected end of file")
    }

    fn verify(&self, c: Option<char>, expected: &str) -> Result<()> {
        let Some(c) = c else { return Err(self.eof()) };
        if expected.contains(c) {
            return Ok(());
        }
        let alternatives: Vec<String> = expected.chars().map(|e| format!("'{e}'")).collect();
        Err(self.fatal(format!("Expected {}, found '{c}'", alternatives.join(" or "))))
    }

    /// Skip white space and comments, and return the character after them,
    /// left unread.
    fn skip_wsc(&mut self) -> Option<char> {
        let mut c = self.read();
        while let Some(ch) = c {
            if ch == '#' {
                self.comment();
            } else if is_whitespace(ch) {
                if ch == '\n' {
                    self.line += 1;
                }
            } else {
                break;
            }
            c = self.read();
        }
        self.unread(c);
        c
    }

    fn comment(&mut self) {
        let mut c = self.read();
        while c.is_some_and(|ch| ch != '\r' && ch != '\n') {
            c = self.read();
        }
        if c == Some('\n') {
            self.line += 1;
        }
        if c == Some('\r') {
            let next = self.read();
            self.line += 1;
            if next != Some('\n') {
                self.unread(next);
            }
        }
    }

    // -- statements -------------------------------------------------------------

    fn document(&mut self) -> Result<()> {
        while self.skip_wsc().is_some() {
            self.statement()?;
        }
        Ok(())
    }

    fn statement(&mut self) -> Result<()> {
        // A directive is the statement's first word, up to eight UTF-16 units.
        let mut word = String::new();
        let mut units = 0;
        while units < 8 {
            let c = self.read();
            match c {
                Some(ch) if !is_whitespace(ch) => {
                    word.push(ch);
                    units += ch.len_utf16();
                }
                _ => {
                    self.unread(c);
                    break;
                }
            }
        }
        if word.starts_with('@') || word.eq_ignore_ascii_case("prefix") || word.eq_ignore_ascii_case("base") {
            self.directive(&word)?;
            self.skip_wsc();
            if word.starts_with('@') {
                let c = self.read();
                self.verify(c, ".")?;
            }
        } else {
            self.unread_str(&word);
            self.triples()?;
            self.skip_wsc();
            let c = self.read();
            self.verify(c, ".")?;
        }
        Ok(())
    }

    fn directive(&mut self, word: &str) -> Result<()> {
        let after = |keyword: &str, any_case: bool| -> Option<String> {
            let head = word.get(..keyword.len())?;
            let matched = if any_case { head.eq_ignore_ascii_case(keyword) } else { head == keyword };
            matched.then(|| word[keyword.len()..].to_string())
        };
        if let Some(rest) = after("@prefix", false) {
            self.unread_str(&rest);
            return self.prefix_directive();
        }
        if let Some(rest) = after("@base", false) {
            self.unread_str(&rest);
            return self.base_directive();
        }
        if let Some(rest) = after("prefix", true) {
            self.unread_str(&rest);
            return self.prefix_directive();
        }
        if let Some(rest) = after("base", true) {
            self.unread_str(&rest);
            return self.base_directive();
        }
        if after("@prefix", true).is_some() {
            return Err(self.fatal("Cannot strictly support case-insensitive @prefix directive in compliance mode."));
        }
        if after("@base", true).is_some() {
            return Err(self.fatal("Cannot strictly support case-insensitive @base directive in compliance mode."));
        }
        if word.is_empty() {
            return Err(self.fatal("Directive name is missing, expected @prefix or @base"));
        }
        Err(self.fatal(format!("Unknown directive \"{word}\"")))
    }

    fn prefix_directive(&mut self) -> Result<()> {
        self.skip_wsc();
        let mut prefix = String::new();
        loop {
            let c = self.read();
            match c {
                Some(':') => {
                    self.unread(c);
                    break;
                }
                Some(ch) if is_whitespace(ch) => break,
                Some(ch) => prefix.push(ch),
                None => return Err(self.eof()),
            }
        }
        self.skip_wsc();
        let c = self.read();
        self.verify(c, ":")?;
        self.skip_wsc();
        let namespace = self.iri()?;
        self.namespaces.insert(prefix.clone(), namespace.clone());
        self.sink.prefix(&prefix, &namespace);
        Ok(())
    }

    fn base_directive(&mut self) -> Result<()> {
        self.skip_wsc();
        let base = self.iri()?;
        self.base = Some(Iri::lenient(&base).map_err(|e| self.fatal(e))?);
        Ok(())
    }

    fn triples(&mut self) -> Result<()> {
        if self.peek() == Some('[') {
            self.read();
            self.skip_wsc();
            if self.peek() == Some(']') {
                self.read();
                self.subject = Some(self.unlabelled_node());
                self.skip_wsc();
                self.predicate_object_list()?;
            } else {
                self.unread(Some('['));
                self.subject = Some(self.blank_node_property_list()?);
            }
            self.skip_wsc();
            if self.peek() != Some('.') {
                self.predicate_object_list()?;
            }
        } else {
            self.subject = Some(match self.peek() {
                Some('(') => self.collection()?,
                Some('[') => self.blank_node_property_list()?,
                _ => match self.value()? {
                    Value::Node(node) => node,
                    value => return Err(self.fatal(format!("Illegal subject value: {value}"))),
                },
            });
            self.skip_wsc();
            self.predicate_object_list()?;
        }
        self.subject = None;
        self.predicate = None;
        Ok(())
    }

    fn predicate_object_list(&mut self) -> Result<()> {
        self.predicate = Some(self.verb()?);
        self.skip_wsc();
        self.object_list()?;
        while self.skip_wsc() == Some(';') {
            self.read();
            let c = self.skip_wsc();
            if matches!(c, Some('.' | ']' | '}')) {
                break;
            }
            if c == Some(';') {
                continue;
            }
            self.predicate = Some(self.verb()?);
            self.skip_wsc();
            self.object_list()?;
        }
        Ok(())
    }

    fn object_list(&mut self) -> Result<()> {
        self.object()?;
        self.refuse_annotation()?;
        while self.skip_wsc() == Some(',') {
            self.read();
            self.skip_wsc();
            self.object()?;
            self.refuse_annotation()?;
        }
        Ok(())
    }

    fn refuse_annotation(&mut self) -> Result<()> {
        if self.skip_wsc() == Some('{') {
            return Err(self.fatal("An RDF-star annotation {| … |} is not read"));
        }
        Ok(())
    }

    fn verb(&mut self) -> Result<String> {
        let c1 = self.read();
        if c1 == Some('a') {
            let c2 = self.read();
            if c2.is_some_and(is_whitespace) {
                return Ok(RDF_TYPE.to_string());
            }
            self.unread(c2);
        }
        self.unread(c1);
        match self.value()? {
            Value::Node(Node::Iri(iri)) => Ok(iri),
            value => Err(self.fatal(format!("Illegal predicate value: {value}"))),
        }
    }

    fn object(&mut self) -> Result<()> {
        match self.peek() {
            Some('(') => {
                self.collection()?;
            }
            Some('[') => {
                self.blank_node_property_list()?;
            }
            _ => {
                let value = self.value()?;
                self.state(value)?;
            }
        }
        Ok(())
    }

    /// `( … )`: the list's first cell, after stating it as the current
    /// subject's object.
    fn collection(&mut self) -> Result<Node> {
        let c = self.read();
        self.verify(c, "(")?;
        if self.skip_wsc() == Some(')') {
            self.read();
            let nil = Node::Iri(RDF_NIL.to_string());
            self.state(Value::Node(nil.clone()))?;
            return Ok(nil);
        }
        let first = self.unlabelled_node();
        self.state(Value::Node(first.clone()))?;
        let (subject, predicate) = (self.subject.take(), self.predicate.take());
        self.subject = Some(first.clone());
        self.predicate = Some(RDF_FIRST.to_string());
        self.object()?;
        let mut cell = first.clone();
        while self.skip_wsc() != Some(')') {
            let next = self.unlabelled_node();
            self.report(&cell, RDF_REST, Value::Node(next.clone()))?;
            cell = next;
            self.subject = Some(cell.clone());
            self.object()?;
        }
        self.read();
        self.report(&cell, RDF_REST, Value::Node(Node::Iri(RDF_NIL.to_string())))?;
        self.subject = subject;
        self.predicate = predicate;
        Ok(first)
    }

    /// `[ … ]`: its node, after stating it as the current subject's object.
    fn blank_node_property_list(&mut self) -> Result<Node> {
        let c = self.read();
        self.verify(c, "[")?;
        let node = self.unlabelled_node();
        self.state(Value::Node(node.clone()))?;
        self.skip_wsc();
        let c = self.read();
        if c != Some(']') {
            self.unread(c);
            let (subject, predicate) = (self.subject.take(), self.predicate.take());
            self.subject = Some(node.clone());
            self.skip_wsc();
            self.predicate_object_list()?;
            self.skip_wsc();
            let c = self.read();
            self.verify(c, "]")?;
            self.subject = subject;
            self.predicate = predicate;
        }
        Ok(node)
    }

    // -- terms ------------------------------------------------------------------

    fn value(&mut self) -> Result<Value> {
        let (c0, c1) = (self.read(), self.read());
        self.unread(c1);
        self.unread(c0);
        if c0 == Some('<') && c1 == Some('<') {
            return Err(self.fatal("An RDF-star triple << … >> is not read"));
        }
        match c0 {
            Some('<') => Ok(Value::Node(Node::Iri(self.iri()?))),
            Some(c) if c == ':' || is_pn_chars_base(c) => self.prefixed_name_or_boolean(),
            Some('_') => Ok(Value::Node(self.labelled_blank_node()?)),
            Some('"' | '\'') => self.quoted_literal(),
            Some(c) if c.is_ascii_digit() || matches!(c, '.' | '+' | '-') => self.number(),
            None => Err(self.eof()),
            Some(c) => Err(self.fatal(format!("Expected an RDF value here, found '{c}'"))),
        }
    }

    /// `<…>`, resolved against the base.
    fn iri(&mut self) -> Result<String> {
        let c = self.read();
        self.verify(c, "<")?;
        let mut text = String::new();
        loop {
            let Some(c) = self.read() else { return Err(self.eof()) };
            if c == '>' {
                break;
            }
            if c == ' ' {
                return Err(self.fatal("IRI included an unencoded space: '32'"));
            }
            text.push(c);
            if c == '\\' {
                let Some(e) = self.read() else { return Err(self.eof()) };
                if e != 'u' && e != 'U' {
                    return Err(self.fatal(format!("IRI includes string escapes: '\\{}'", e as u32)));
                }
                text.push(e);
            }
        }
        let text = decode_escapes(&text).unwrap_or(text);
        self.resolve(&text)
    }

    /// `spec` as an IRI: resolved against the base when it holds no colon,
    /// and refused unless the result parses.
    fn resolve(&self, spec: &str) -> Result<String> {
        if spec.contains(':') {
            return self.absolute(spec.to_string());
        }
        let Some(base) = &self.base else {
            return Err(self.fatal("Unable to resolve URIs, no base URI has been set"));
        };
        if !spec.is_empty() && !spec.starts_with('#') && base.is_opaque() {
            return Err(self.fatal(format!(
                "Relative URI '{spec}' cannot be resolved using the opaque base URI '{}'",
                base.as_str()
            )));
        }
        let resolved = base.resolve_text(spec).map_err(|e| self.fatal(e))?;
        self.absolute(resolved)
    }

    fn absolute(&self, iri: String) -> Result<String> {
        if let Err(e) = Iri::parse(&iri) {
            return Err(self.fatal(e));
        }
        if !iri.contains(':') {
            return Err(self.fatal(format!("Not a valid (absolute) IRI: {iri}")));
        }
        Ok(iri)
    }

    fn prefixed_name_or_boolean(&mut self) -> Result<Value> {
        let Some(c) = self.read() else { return Err(self.eof()) };
        let namespace = if c == ':' {
            self.namespace("")?
        } else {
            let mut prefix = vec![c];
            let mut previous = c;
            let mut c = self.read();
            while let Some(ch) = c.filter(|&ch| is_pn_chars(ch) || ch == '.') {
                prefix.push(ch);
                previous = ch;
                c = self.read();
            }
            // A prefix does not end in a dot.
            while previous == '.' {
                self.unread(c);
                c = Some(previous);
                prefix.pop();
                previous = *prefix.last().expect("a prefix starts with a letter");
            }
            if c != Some(':') {
                let word: String = prefix.iter().collect();
                if word == "true" || word == "false" {
                    self.unread(c);
                    return Ok(Value::typed(word, XSD_BOOLEAN));
                }
            }
            self.verify(c, ":")?;
            self.namespace(&prefix.iter().collect::<String>())?
        };
        let mut local = String::new();
        let c = self.read();
        if let Some(first) = c.filter(|&ch| is_pn_chars_u(ch) || ch.is_ascii_digit() || matches!(ch, ':' | '\\' | '%')) {
            local.push(if first == '\\' { self.local_escape()? } else { first });
            let mut previous = first;
            let mut c = self.read();
            while let Some(ch) = c.filter(|&ch| is_pn_chars(ch) || matches!(ch, '.' | ':' | '\\' | '%')) {
                local.push(if ch == '\\' { self.local_escape()? } else { ch });
                previous = ch;
                c = self.read();
            }
            self.unread(c);
            // One trailing dot ends the statement rather than the name.
            if previous == '.' {
                self.unread(Some('.'));
                local.pop();
            }
        } else {
            self.unread(c);
        }
        let chars: Vec<char> = local.chars().collect();
        for (i, &ch) in chars.iter().enumerate() {
            if ch == '%' && !(i + 2 < chars.len() && chars[i + 1].is_ascii_hexdigit() && chars[i + 2].is_ascii_hexdigit()) {
                return Err(self.fatal(format!("Found incomplete percent-encoded sequence: {local}")));
            }
        }
        Ok(Value::Node(Node::Iri(self.absolute(format!("{namespace}{local}"))?)))
    }

    fn local_escape(&mut self) -> Result<char> {
        match self.read() {
            Some(c) if LOCAL_ESCAPED.contains(c) => Ok(c),
            Some(c) => Err(anyhow!(
                "found '{c}', expected one of: [_, ~, ., -, !, $, &, ', (, ), *, +, ,, ;, =, /, ?, #, @, %]"
            )),
            None => Err(self.eof()),
        }
    }

    fn namespace(&self, prefix: &str) -> Result<String> {
        if let Some(namespace) = self.namespaces.get(prefix) {
            return Ok(namespace.clone());
        }
        if prefix.is_empty() {
            return Err(self.fatal("Default namespace used but not defined"));
        }
        Err(self.fatal(format!("Namespace prefix '{prefix}' used but not defined")))
    }

    /// `_:label`. A label that starts otherwise than the grammar allows is
    /// read all the same; a dot ends it before white space, `<`, `_` or the
    /// end.
    fn labelled_blank_node(&mut self) -> Result<Node> {
        let c = self.read();
        self.verify(c, "_")?;
        let c = self.read();
        self.verify(c, ":")?;
        let Some(first) = self.read() else { return Err(self.eof()) };
        let mut label = String::from(first);
        let mut c = self.read();
        if !c.is_some_and(is_label_char) {
            self.unread(c);
        }
        while let Some(previous) = c.filter(|&ch| is_label_char(ch)) {
            c = self.read();
            if previous == '.' && (c.is_none() || c.is_some_and(|ch| is_whitespace(ch) || ch == '<' || ch == '_')) {
                self.unread(c);
                self.unread(Some(previous));
                break;
            }
            label.push(previous);
            if !c.is_some_and(is_label_char) {
                self.unread(c);
            }
        }
        let id = if label.encode_utf16().count() > 32 {
            Md5::digest(label.as_bytes()).iter().map(|b| format!("{b:02X}")).collect()
        } else {
            label
        };
        Ok(Node::Blank(format!("{LABELLED}{id}")))
    }

    fn unlabelled_node(&mut self) -> Node {
        let n = self.unlabelled;
        self.unlabelled += 1;
        Node::Blank(format!("{UNLABELLED}{n}"))
    }

    fn quoted_literal(&mut self) -> Result<Value> {
        let label = self.quoted_string()?;
        match self.peek() {
            Some('@') => {
                self.read();
                let Some(first) = self.read() else { return Err(self.eof()) };
                let mut lang = String::from(first);
                let mut c = self.read();
                while let Some(ch) = c.filter(|&ch| !is_whitespace(ch) && !matches!(ch, '.' | ';' | ',' | ')' | ']' | '>')) {
                    lang.push(ch);
                    c = self.read();
                }
                self.unread(c);
                Ok(Value::Literal { label, lang: Some(lang), datatype: None })
            }
            Some('^') => {
                self.read();
                let c = self.read();
                self.verify(c, "^")?;
                self.skip_wsc();
                match self.value()? {
                    Value::Node(Node::Iri(datatype)) if datatype == RDF_LANG_STRING || datatype == XSD_STRING => {
                        Ok(Value::Literal { label, lang: None, datatype: None })
                    }
                    Value::Node(Node::Iri(datatype)) => Ok(Value::typed(label, &datatype)),
                    value => Err(self.fatal(format!("Illegal datatype value: {value}"))),
                }
            }
            _ => Ok(Value::Literal { label, lang: None, datatype: None }),
        }
    }

    /// The text of a string, its escapes decoded where they all decode.
    fn quoted_string(&mut self) -> Result<String> {
        let c1 = self.read();
        self.verify(c1, "\"'")?;
        let quote = c1.unwrap_or('"');
        let (c2, c3) = (self.read(), self.read());
        let text = if c2 == Some(quote) && c3 == Some(quote) {
            self.long_string(quote)?
        } else {
            self.unread(c3);
            self.unread(c2);
            self.short_string(quote)?
        };
        Ok(decode_escapes(&text).unwrap_or(text))
    }

    fn short_string(&mut self, quote: char) -> Result<String> {
        let mut text = String::new();
        loop {
            let Some(c) = self.read() else { return Err(self.eof()) };
            if c == quote {
                return Ok(text);
            }
            if c == '\r' || c == '\n' {
                return Err(self.fatal("Illegal carriage return or new line in literal"));
            }
            text.push(c);
            if c == '\\' {
                let Some(e) = self.read() else { return Err(self.eof()) };
                text.push(e);
            }
        }
    }

    fn long_string(&mut self, quote: char) -> Result<String> {
        let mut text = String::new();
        let mut quotes = 0;
        while quotes < 3 {
            let Some(c) = self.read() else { return Err(self.eof()) };
            quotes = if c == quote { quotes + 1 } else { 0 };
            text.push(c);
            if c == '\n' {
                self.line += 1;
            }
            if c == '\\' {
                let Some(e) = self.read() else { return Err(self.eof()) };
                text.push(e);
            }
        }
        text.truncate(text.len() - 3);
        Ok(text)
    }

    /// An integer, decimal or double, its lexical form as written. An
    /// exponent with no digits takes the character after it.
    fn number(&mut self) -> Result<Value> {
        let mut value = String::new();
        let mut datatype = XSD_INTEGER;
        let mut c = self.read();
        if let Some(sign @ ('+' | '-')) = c {
            value.push(sign);
            c = self.read();
        }
        while let Some(d) = c.filter(char::is_ascii_digit) {
            value.push(d);
            c = self.read();
        }
        if matches!(c, Some('.' | 'e' | 'E')) {
            if c == Some('.') {
                if !self.peek().is_some_and(is_whitespace) {
                    value.push('.');
                    c = self.read();
                    while let Some(d) = c.filter(char::is_ascii_digit) {
                        value.push(d);
                        c = self.read();
                    }
                    if value.len() == 1 {
                        return Err(self.fatal("Object for statement missing"));
                    }
                    datatype = XSD_DECIMAL;
                }
            } else if value.is_empty() {
                return Err(self.fatal("Object for statement missing"));
            }
            if let Some(e @ ('e' | 'E')) = c {
                datatype = XSD_DOUBLE;
                value.push(e);
                c = self.read();
                if let Some(sign @ ('+' | '-')) = c {
                    value.push(sign);
                    c = self.read();
                }
                let Some(after) = c else { return Err(self.fatal("Invalid codepoint -1")) };
                value.push(after);
                c = self.read();
                while let Some(d) = c.filter(char::is_ascii_digit) {
                    value.push(d);
                    c = self.read();
                }
            }
        }
        self.unread(c);
        Ok(Value::typed(value, datatype))
    }

    // -- what the parse states --------------------------------------------------

    /// The current subject and predicate with `object`, when both are set.
    fn state(&mut self, object: Value) -> Result<()> {
        let (Some(subject), Some(predicate)) = (self.subject.clone(), self.predicate.clone()) else {
            return Ok(());
        };
        self.report(&subject, &predicate, object)
    }

    fn report(&mut self, subject: &Node, predicate: &str, object: Value) -> Result<()> {
        if predicate == RDF_FIRST || predicate == RDF_REST {
            if self.typed_lists.insert(subject.clone()) {
                self.emit(subject, RDF_TYPE, &Value::Node(Node::Iri(RDF_LIST.to_string())))?;
            }
        } else if predicate == RDF_TYPE && matches!(&object, Value::Node(Node::Iri(t)) if t == RDF_LIST) {
            self.typed_lists.insert(subject.clone());
        }
        self.emit(subject, predicate, &object)
    }

    fn emit(&mut self, subject: &Node, predicate: &str, object: &Value) -> Result<()> {
        let subject = subject.name();
        match object {
            Value::Node(node) => self.sink.resource(&subject, predicate, &node.name()),
            Value::Literal { label, lang, datatype } => {
                self.sink.literal(&subject, predicate, label, lang.as_deref(), datatype.as_deref())
            }
        }
    }
}

/// `text` with its escapes decoded — `\t \b \n \r \f \" \' \> \\`, `\uXXXX`
/// and `\UXXXXXXXX` — or `None` when one does not decode.
fn decode_escapes(text: &str) -> Option<String> {
    if !text.contains('\\') {
        return Some(text.to_string());
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<u16> = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' {
            let mut buf = [0u16; 2];
            out.extend_from_slice(c.encode_utf16(&mut buf));
            i += 1;
            continue;
        }
        let e = *chars.get(i + 1)?;
        let simple = match e {
            't' => Some('\t'),
            'b' => Some('\u{8}'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            'f' => Some('\u{c}'),
            '"' | '\'' | '>' | '\\' => Some(e),
            _ => None,
        };
        if let Some(s) = simple {
            out.push(s as u16);
            i += 2;
            continue;
        }
        let digits = match e {
            'u' => 4,
            'U' => 8,
            _ => return None,
        };
        if i + digits + 1 >= chars.len() {
            return None;
        }
        let hex: String = chars[i + 2..i + 2 + digits].iter().collect();
        let code = i32::from_str_radix(&hex, 16).ok()?;
        match u32::try_from(code).ok()? {
            unit @ 0..=0xFFFF => out.push(unit as u16),
            cp @ 0x10000..=0x10FFFF => {
                let cp = cp - 0x10000;
                out.push(0xD800 + (cp >> 10) as u16);
                out.push(0xDC00 + (cp & 0x3FF) as u16);
            }
            _ => return None,
        }
        i += digits + 2;
    }
    Some(String::from_utf16_lossy(&out))
}

fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn is_pn_chars_base(c: char) -> bool {
    c.is_ascii_alphabetic()
        || matches!(c as u32,
            0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D | 0x37F..=0x1FFF | 0x200C..=0x200D
            | 0x2070..=0x218F | 0x2C00..=0x2FEF | 0x3001..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD
            | 0x10000..=0xEFFFF)
}

fn is_pn_chars_u(c: char) -> bool {
    is_pn_chars_base(c) || c == '_'
}

fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c) || c.is_ascii_digit() || matches!(c as u32, 0x2D | 0xB7 | 0x300..=0x36F | 0x203F..=0x2040)
}

fn is_label_char(c: char) -> bool {
    is_pn_chars(c) || c == '.'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The statements a sink receives, one line each.
    #[derive(Default)]
    struct Lines(Vec<String>);

    impl Sink for Lines {
        fn next_id(&mut self) -> u64 {
            unreachable!("a Turtle parse names its own nodes")
        }
        fn resource(&mut self, s: &str, p: &str, o: &str) -> Result<()> {
            self.0.push(format!("{s} {p} {o}"));
            Ok(())
        }
        fn literal(&mut self, s: &str, p: &str, value: &str, lang: Option<&str>, datatype: Option<&str>) -> Result<()> {
            self.0.push(format!("{s} {p} {value:?} {lang:?} {datatype:?}"));
            Ok(())
        }
        fn prefix(&mut self, name: &str, namespace: &str) {
            self.0.push(format!("@prefix {name}: {namespace}"));
        }
    }

    fn lines(text: &str) -> Vec<String> {
        let mut sink = Lines::default();
        let mut unlabelled = 1;
        read(text, Some("file:/d/x.ttl"), &mut unlabelled, &mut sink).unwrap();
        sink.0.iter().map(|l| l.replace("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "rdf:")).collect()
    }

    /// The statement naming a `[ … ]` or `( … )` object comes before the
    /// statements inside it, and each list cell is typed `rdf:List` before
    /// its first `rdf:first`.
    #[test]
    fn an_objects_statement_comes_before_the_statements_inside_it() {
        assert_eq!(
            lines("<s> <p> [ <q> \"v\" ] , ( <a> [ <r> <b> ] ) ."),
            [
                "file:/d/s file:/d/p _:genid-nodeid-node1hf7uaq00x1",
                "_:genid-nodeid-node1hf7uaq00x1 file:/d/q \"v\" None None",
                "file:/d/s file:/d/p _:genid-nodeid-node1hf7uaq00x2",
                "_:genid-nodeid-node1hf7uaq00x2 rdf:type rdf:List",
                "_:genid-nodeid-node1hf7uaq00x2 rdf:first file:/d/a",
                "_:genid-nodeid-node1hf7uaq00x2 rdf:rest _:genid-nodeid-node1hf7uaq00x3",
                "_:genid-nodeid-node1hf7uaq00x3 rdf:type rdf:List",
                "_:genid-nodeid-node1hf7uaq00x3 rdf:first _:genid-nodeid-node1hf7uaq00x4",
                "_:genid-nodeid-node1hf7uaq00x4 file:/d/r file:/d/b",
                "_:genid-nodeid-node1hf7uaq00x3 rdf:rest rdf:nil",
            ]
        );
    }

    /// A labelled node carries its label, or the MD5 digest of a label longer
    /// than 32 units; a predeclared prefix needs no declaration.
    #[test]
    fn a_labelled_node_carries_its_label() {
        assert_eq!(
            lines("_:b1 owl:sameAs _:N0123456789abcdef0123456789abcdef ."),
            ["_:genid-nodeid-genid-0123456789ab4cde8f0123456789abcd-b1 http://www.w3.org/2002/07/owl#sameAs \
              _:genid-nodeid-genid-0123456789ab4cde8f0123456789abcd-172290EC509FB4FEB1F6C3FF1F3E535B"]
        );
    }

    /// A relative reference holding a character an IRI may not is
    /// percent-encoded; numbers keep the form they are written in; a dot
    /// ends a prefixed name before the end of the statement.
    #[test]
    fn terms_read_as_written() {
        assert_eq!(
            lines("@prefix : <{x}#> .\n:a :p 1.50, -2, 3e1, true, \"é\\u00e9\"@EN, \"s\"^^xsd:string ."),
            [
                "@prefix : file:/d/%7Bx%7D#",
                "file:/d/%7Bx%7D#a file:/d/%7Bx%7D#p \"1.50\" None Some(\"http://www.w3.org/2001/XMLSchema#decimal\")",
                "file:/d/%7Bx%7D#a file:/d/%7Bx%7D#p \"-2\" None Some(\"http://www.w3.org/2001/XMLSchema#integer\")",
                "file:/d/%7Bx%7D#a file:/d/%7Bx%7D#p \"3e1\" None Some(\"http://www.w3.org/2001/XMLSchema#double\")",
                "file:/d/%7Bx%7D#a file:/d/%7Bx%7D#p \"true\" None Some(\"http://www.w3.org/2001/XMLSchema#boolean\")",
                "file:/d/%7Bx%7D#a file:/d/%7Bx%7D#p \"éé\" Some(\"EN\") None",
                "file:/d/%7Bx%7D#a file:/d/%7Bx%7D#p \"s\" None None",
            ]
        );
    }

    /// An absolute IRI holding a character an IRI may not is refused.
    #[test]
    fn an_absolute_iri_with_a_refused_character_is_an_error() {
        let mut sink = Lines::default();
        let error = read("<http://x.org/{a}> <p> <o> .", None, &mut 1, &mut sink).unwrap_err();
        assert_eq!(error.to_string(), "Unexpected character U+7B at index 13: http://x.org/{a} [line 1]");
    }
}
