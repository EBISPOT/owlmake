//! Turtle in owlmake's sectioned layout: a prefix block and `@base`, the
//! ontology header, entity sections under banner comments with one `###  IRI`
//! block per entity, then the annotated IRIs, the general axioms and the rules,
//! and a closing comment.
//!
//! That walk over the ontology's RDF graph — which subjects, in which order,
//! each with its triples in the order they were translated, which blank nodes
//! nest and which carry ids — is exactly the one the RDF/XML writer (`owlrdf`)
//! makes. So this module renders the writer's own output: it reads the RDF/XML
//! back, with the end of each rendered object marked (RDF/XML does not show it,
//! and Turtle puts a blank line there), and writes every node as Turtle.
//!
//! RDF/XML names a node element after one of the node's types and lists the
//! other triples in order. Turtle lists that type with the rest, where it was
//! translated: after any other declared type of a named subject, after the
//! `owl:intersectionOf` of an anonymous intersection, among the asserted classes
//! when it is `owl:Thing` or `owl:Nothing`, and first everywhere else.
//!
//! A model holding an axiom whose shape the RDF/XML layout cannot state has no
//! such walk to follow: it is written one statement at a time
//! ([`crate::io::turtle::save_plain`]), with a warning naming the axiom.

use std::cmp::Ordering;

use anyhow::{anyhow, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::NsReader;

use crate::model::Model;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_DESCRIPTION: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#Description";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// The comment `owlrdf` writes where a rendered object ends, when asked to.
pub(crate) const OBJECT_END: &str = "om:object-end";

/// The built-in prefixes every document declares.
const BUILTIN_PREFIXES: [(&str, &str); 5] = [
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("xml", "http://www.w3.org/XML/1998/namespace"),
];

/// The types a node element is named after.
const ELEMENT_TYPES: [&str; 12] = [
    "http://www.w3.org/2002/07/owl#Class",
    "http://www.w3.org/2002/07/owl#ObjectProperty",
    "http://www.w3.org/2002/07/owl#DatatypeProperty",
    "http://www.w3.org/2002/07/owl#AnnotationProperty",
    "http://www.w3.org/2002/07/owl#Restriction",
    "http://www.w3.org/2002/07/owl#Thing",
    "http://www.w3.org/2002/07/owl#Nothing",
    "http://www.w3.org/2002/07/owl#Ontology",
    "http://www.w3.org/2002/07/owl#NamedIndividual",
    "http://www.w3.org/2000/01/rdf-schema#Datatype",
    "http://www.w3.org/2002/07/owl#Axiom",
    "http://www.w3.org/2002/07/owl#Annotation",
];

/// A triple's object.
enum Obj {
    Iri(String),
    Literal { text: String, lang: Option<String>, datatype: Option<String> },
    /// A blank node written in place.
    Node(Node),
    /// A list of resources, written as a collection.
    List(Vec<Obj>),
    /// A blank node written by its id (`_:genid…`) and stated on its own.
    Id(String),
}

enum Subject {
    Iri(String),
    Id(String),
    Blank,
}

struct Node {
    subject: Subject,
    triples: Vec<(String, Obj)>,
}

/// One step of the rendered document.
enum Item {
    Banner(String),
    /// The `###  IRI` line before an entity's block.
    Comment(String),
    /// The end of a rendered object.
    End,
    Node(Node),
}

/// Write `model` as Turtle. `prefixes` are the document's prefixes other than the
/// default one, shortest name first.
pub fn save<W: std::io::Write>(model: &mut Model, prefixes: &[(String, String)], w: &mut W) -> Result<()> {
    let mut xml = Vec::new();
    let unstated = crate::io::owlrdf::with_object_ends(|| crate::io::owlrdf::try_save(model, &mut xml))?;
    if !unstated.is_empty() {
        crate::io::owlrdf::warn_unstated("Turtle", &unstated);
        return crate::io::turtle::save_plain(model, prefixes, w);
    }
    let xml = String::from_utf8(xml).map_err(|e| anyhow!("RDF/XML output is not valid UTF-8: {e}"))?;
    let (items, trailer) = parse(&xml)?;
    let ontology_iri = crate::cmd::merge::ontology_iri(model);

    let pm = PrefixManager::new(ontology_iri.as_deref(), prefixes);
    let mut out = Out { buf: String::new(), len: 0, last_nl: 0, tabs: Vec::new(), level: 0, pm, ontology_iri };
    for (name, ns) in out.pm.names.clone() {
        out.write("@prefix ");
        out.write(&name);
        out.write(" ");
        out.write("<");
        out.write(&ns);
        out.write(">");
        out.write(" .");
        out.newline();
    }
    out.write("@base ");
    out.write("<");
    let base = out.ontology_iri.clone().unwrap_or_else(|| OWL.to_string());
    out.write(&base);
    out.write("> .\n\n");
    for item in &items {
        match item {
            Item::Banner(name) => {
                // Written past the column count, as the banner always is.
                out.buf.push_str("#################################################################\n#    ");
                out.buf.push_str(name);
                out.buf.push_str("\n#################################################################\n\n");
            }
            Item::Comment(iri) => out.comment(iri),
            Item::End => out.newline(),
            Item::Node(node) => out.render(node),
        }
    }
    out.comment(&trailer);
    w.write_all(out.buf.as_bytes())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reading the RDF/XML back
// ---------------------------------------------------------------------------

/// The document's items, and the text of its closing comment.
fn parse(xml: &str) -> Result<(Vec<Item>, String)> {
    let mut r = NsReader::from_str(xml);
    let mut items = Vec::new();
    let mut trailer = String::new();
    let mut depth = 0usize;
    let mut commented = false;
    loop {
        match r.read_event()? {
            Event::Start(e) if depth == 0 => {
                let _ = e;
                depth = 1;
            }
            Event::Start(e) => {
                let node = node(&mut r, &e, false)?;
                push_node(&mut items, node, &mut commented);
            }
            Event::Empty(e) if depth == 1 => {
                let node = node(&mut r, &e, true)?;
                push_node(&mut items, node, &mut commented);
            }
            Event::End(_) => depth = 0,
            Event::Comment(c) => {
                let text = String::from_utf8_lossy(&c.into_inner()).into_owned();
                if depth == 0 {
                    trailer = text.trim().to_string();
                } else if text.trim() == OBJECT_END {
                    items.push(Item::End);
                } else if let Some(name) = banner_name(&text) {
                    items.push(Item::Banner(name));
                } else {
                    // The entity's IRI is read off its element, which spells it
                    // without the comment's escaping.
                    commented = true;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((items, trailer))
}

fn push_node(items: &mut Vec<Item>, node: Node, commented: &mut bool) {
    if std::mem::take(commented) {
        if let Subject::Iri(iri) = &node.subject {
            items.push(Item::Comment(iri.clone()));
        }
    }
    items.push(Item::Node(node));
}

/// The section a banner comment opens.
fn banner_name(text: &str) -> Option<String> {
    if !text.contains("////") {
        return None;
    }
    text.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("// ").map(|n| n.trim().to_string()))
}

/// The IRI an element or attribute name stands for.
fn resolve(r: &NsReader<&[u8]>, name: QName, attribute: bool) -> String {
    let (ns, local) = if attribute { r.resolve_attribute(name) } else { r.resolve_element(name) };
    let local = String::from_utf8_lossy(local.as_ref()).into_owned();
    match ns {
        ResolveResult::Bound(ns) => format!("{}{local}", String::from_utf8_lossy(ns.as_ref())),
        _ => local,
    }
}

#[derive(Default)]
struct Attrs {
    about: Option<String>,
    resource: Option<String>,
    node_id: Option<String>,
    datatype: Option<String>,
    lang: Option<String>,
    parse_type: Option<String>,
}

fn attrs(r: &NsReader<&[u8]>, e: &BytesStart) -> Result<Attrs> {
    let mut a = Attrs::default();
    for attr in e.attributes() {
        let attr = attr?;
        let name = resolve(r, attr.key, true);
        let value = attr.unescape_value()?.into_owned();
        match name.strip_prefix(RDF) {
            Some("about") => a.about = Some(value),
            Some("resource") => a.resource = Some(value),
            Some("nodeID") => a.node_id = Some(value),
            Some("datatype") => a.datatype = Some(value),
            Some("parseType") => a.parse_type = Some(value),
            _ if name == format!("{XML_NS}lang") => a.lang = Some(value),
            _ => {}
        }
    }
    Ok(a)
}

/// A node element and everything inside it.
fn node(r: &mut NsReader<&[u8]>, e: &BytesStart, empty: bool) -> Result<Node> {
    let element = resolve(r, e.name(), false);
    let a = attrs(r, e)?;
    let subject = match (a.about, a.node_id) {
        (Some(iri), _) => Subject::Iri(iri),
        (None, Some(id)) => Subject::Id(format!("_:{id}")),
        (None, None) => Subject::Blank,
    };
    let mut triples = Vec::new();
    if !empty {
        loop {
            match r.read_event()? {
                Event::Start(p) => triples.push(property(r, &p, false)?),
                Event::Empty(p) => triples.push(property(r, &p, true)?),
                Event::End(_) => break,
                Event::Eof => return Err(anyhow!("RDF/XML ends inside a node element")),
                _ => {}
            }
        }
    }
    if element != RDF_DESCRIPTION {
        place_type(&subject, &element, &mut triples);
    }
    Ok(Node { subject, triples })
}

/// A property element: the predicate and its object.
fn property(r: &mut NsReader<&[u8]>, e: &BytesStart, empty: bool) -> Result<(String, Obj)> {
    let pred = resolve(r, e.name(), false);
    let a = attrs(r, e)?;
    let fixed = match (&a.resource, &a.node_id) {
        (Some(iri), _) => Some(Obj::Iri(iri.clone())),
        (None, Some(id)) => Some(Obj::Id(format!("_:{id}"))),
        _ => None,
    };
    if let Some(obj) = fixed {
        if !empty {
            skip_to_end(r)?;
        }
        return Ok((pred, obj));
    }
    let literal = |text: String| Obj::Literal { text, lang: a.lang.clone(), datatype: a.datatype.clone() };
    if empty {
        return Ok((pred, literal(String::new())));
    }
    if a.parse_type.as_deref() == Some("Collection") {
        let mut items = Vec::new();
        loop {
            match r.read_event()? {
                Event::Start(n) => items.push(item(node(r, &n, false)?)),
                Event::Empty(n) => items.push(item(node(r, &n, true)?)),
                Event::End(_) => break,
                Event::Eof => return Err(anyhow!("RDF/XML ends inside a collection")),
                _ => {}
            }
        }
        return Ok((pred, Obj::List(items)));
    }
    let mut text = String::new();
    let mut nested = None;
    loop {
        match r.read_event()? {
            Event::Text(t) => text.push_str(&t.unescape()?),
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t.into_inner())),
            Event::Start(n) => nested = Some(item(node(r, &n, false)?)),
            Event::Empty(n) => nested = Some(item(node(r, &n, true)?)),
            Event::End(_) => break,
            Event::Eof => return Err(anyhow!("RDF/XML ends inside a property element")),
            _ => {}
        }
    }
    Ok((pred, nested.unwrap_or_else(|| literal(text))))
}

/// A node in object position: a named one is its IRI, one with an id its id.
fn item(node: Node) -> Obj {
    match node.subject {
        Subject::Iri(iri) if node.triples.is_empty() => Obj::Iri(iri),
        Subject::Id(id) if node.triples.is_empty() => Obj::Id(id),
        _ => Obj::Node(node),
    }
}

fn skip_to_end(r: &mut NsReader<&[u8]>) -> Result<()> {
    let mut depth = 0usize;
    loop {
        match r.read_event()? {
            Event::Start(_) => depth += 1,
            Event::End(_) if depth == 0 => return Ok(()),
            Event::End(_) => depth -= 1,
            Event::Eof => return Err(anyhow!("RDF/XML ends inside an element")),
            _ => {}
        }
    }
}

/// Put the type a node element is named after among the node's other triples,
/// where it was translated.
fn place_type(subject: &Subject, ty: &str, triples: &mut Vec<(String, Obj)>) {
    let is_type = |t: &(String, Obj)| t.0 == RDF_TYPE;
    let type_iri = |t: &(String, Obj)| match &t.1 {
        Obj::Iri(i) if t.0 == RDF_TYPE => Some(i.clone()),
        _ => None,
    };
    let declared = |t: &(String, Obj)| {
        type_iri(t).is_some_and(|i| ELEMENT_TYPES.contains(&i.as_str()) && !is_class_assertion_type(&i))
    };
    let at = if is_class_assertion_type(ty) {
        // Asserted: after the declared types, among the other asserted named
        // classes in IRI order, ahead of the asserted expressions.
        let mut i = triples.iter().take_while(|t| declared(t)).count();
        while i < triples.len()
            && is_type(&triples[i])
            && type_iri(&triples[i])
                .is_some_and(|c| crate::io::natural_order::iri_cmp(&c, ty) == Ordering::Less)
        {
            i += 1;
        }
        i
    } else if matches!(subject, Subject::Iri(_)) {
        // Declared: the last of the subject's declared types.
        triples.iter().take_while(|t| declared(t)).count()
    } else if ty == format!("{OWL}Class") && triples.first().is_some_and(|t| t.0 == format!("{OWL}intersectionOf")) {
        1
    } else {
        0
    };
    triples.insert(at, (RDF_TYPE.to_string(), Obj::Iri(ty.to_string())));
}

fn is_class_assertion_type(ty: &str) -> bool {
    ty == "http://www.w3.org/2002/07/owl#Thing" || ty == "http://www.w3.org/2002/07/owl#Nothing"
}

// ---------------------------------------------------------------------------
// Writing Turtle
// ---------------------------------------------------------------------------

/// The document's prefixes.
struct PrefixManager {
    /// Every declared prefix name (with its colon) and namespace, in the order
    /// the prefix block lists them: shortest name first, then alphabetically.
    names: Vec<(String, String)>,
    /// Namespace to prefix name, the last binding of a namespace winning, in
    /// the same order over namespaces.
    reverse: Vec<(String, String)>,
}

impl PrefixManager {
    fn new(ontology_iri: Option<&str>, prefixes: &[(String, String)]) -> PrefixManager {
        let mut bindings: Vec<(String, String)> = BUILTIN_PREFIXES
            .iter()
            .map(|(p, ns)| (format!("{p}:"), ns.to_string()))
            .collect();
        if let Some(iri) = ontology_iri {
            bindings.push((":".to_string(), with_terminating_hash(iri)));
        }
        bindings.extend(prefixes.iter().filter(|(p, _)| !p.is_empty()).map(|(p, ns)| (format!("{p}:"), ns.clone())));
        let mut names: Vec<(String, String)> = Vec::new();
        let mut reverse: Vec<(String, String)> = Vec::new();
        for (name, ns) in bindings {
            match names.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = ns.clone(),
                None => names.push((name.clone(), ns.clone())),
            }
            match reverse.iter_mut().find(|(n, _)| *n == ns) {
                Some(slot) => slot.1 = name,
                None => reverse.push((ns, name)),
            }
        }
        names.sort_by(|a, b| length_then_text(&a.0, &b.0));
        reverse.sort_by(|a, b| length_then_text(&a.0, &b.0));
        PrefixManager { names, reverse }
    }

    /// The prefixed name of `iri`: with the prefix bound to its namespace, else
    /// with the longest bound namespace it extends by a qualified name.
    fn prefixed(&self, iri: &str) -> Option<String> {
        let (ns, local) = crate::io::natural_order::iri_split(iri);
        if let Some((_, name)) = self.reverse.iter().find(|(n, _)| n == ns) {
            return Some(format!("{name}{local}"));
        }
        let mut prefixed = None;
        for (ns, name) in &self.reverse {
            if iri.starts_with(ns.as_str()) && is_qname(&iri[ns.len()..]) {
                prefixed = Some(iri.replace(ns.as_str(), name));
            }
        }
        if prefixed.is_some() {
            return prefixed;
        }
        // An IRI with no separator in it at all splits at any namespace it extends.
        let mut by_ns: Vec<&(String, String)> = self.names.iter().collect();
        by_ns.sort_by(|a, b| crate::io::natural_order::str_cmp(&b.1, &a.1));
        by_ns
            .into_iter()
            .find(|(_, ns)| iri.starts_with(ns.as_str()) && no_separators(iri))
            .map(|(name, ns)| format!("{name}{}", &iri[ns.len()..]))
    }
}

/// Order by UTF-16 length, then by text.
fn length_then_text(a: &str, b: &str) -> Ordering {
    a.encode_utf16()
        .count()
        .cmp(&b.encode_utf16().count())
        .then_with(|| crate::io::natural_order::str_cmp(a, b))
}

fn with_terminating_hash(iri: &str) -> String {
    if iri.ends_with('/') || iri.ends_with('#') || iri.contains('#') {
        iri.to_string()
    } else {
        format!("{iri}#")
    }
}

fn no_separators(s: &str) -> bool {
    !s.chars().any(|c| "~.-!$&()*+,;=/?#@%_".contains(c))
}

fn xml_name_start(c: char) -> bool {
    matches!(c as u32,
        58 | 65..=90 | 95 | 97..=122 | 192..=214 | 216..=246 | 248..=767 | 880..=893
        | 895..=8191 | 8204..=8205 | 8304..=8591 | 11264..=12271 | 12289..=55295
        | 63744..=64975 | 65008..=65533 | 65536..=983039)
}

fn xml_name_char(c: char) -> bool {
    xml_name_start(c) || matches!(c as u32, 45 | 46 | 48..=57 | 183 | 768..=879 | 8255..=8256)
}

/// Whether `s` is a qualified name: a name, or two joined by one colon.
fn is_qname(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut found_colon = false;
    let mut in_name = false;
    for c in s.chars() {
        if c == ':' {
            if found_colon || !in_name {
                return false;
            }
            found_colon = true;
            in_name = false;
        } else if !in_name {
            if !xml_name_start(c) {
                return false;
            }
            in_name = true;
        } else if !xml_name_char(c) {
            return false;
        }
    }
    true
}

/// The text written so far, with the column bookkeeping the layout aligns by.
struct Out {
    buf: String,
    /// The UTF-16 length of everything written through `write`.
    len: usize,
    /// Where `write` last saw a newline: the first one in the last text that
    /// held one.
    last_nl: usize,
    tabs: Vec<usize>,
    level: usize,
    pm: PrefixManager,
    ontology_iri: Option<String>,
}

impl Out {
    fn write(&mut self, s: &str) {
        if let Some(i) = s.find('\n') {
            self.last_nl = self.len + s[..i].encode_utf16().count();
        }
        self.buf.push_str(s);
        self.len += s.encode_utf16().count();
    }

    fn indent(&self) -> usize {
        self.len - self.last_nl
    }

    fn push_tab(&mut self) {
        let i = self.indent();
        self.tabs.push(i);
    }

    fn pop_tab(&mut self) {
        self.tabs.pop();
    }

    fn newline(&mut self) {
        self.write("\n");
        let tab = self.tabs.last().copied().unwrap_or(0);
        for _ in 1..tab {
            self.write(" ");
        }
    }

    fn comment(&mut self, text: &str) {
        self.write("###  ");
        self.write(text);
        self.newline();
    }

    fn iri(&mut self, iri: &str) {
        if iri.starts_with("_:") {
            self.write(iri);
            return;
        }
        if self.ontology_iri.as_deref() != Some(iri) {
            if let Some(name) = self.pm.prefixed(iri).filter(|n| !n.ends_with('.')) {
                self.write(&name);
                return;
            }
        }
        self.write("<");
        self.write(iri);
        self.write(">");
    }

    fn literal(&mut self, text: &str, lang: Option<&str>, datatype: Option<&str>) {
        // A plain literal is stated without a datatype.
        let datatype = datatype.filter(|dt| *dt != format!("{RDF}PlainLiteral"));
        if let Some(dt) = datatype {
            if dt == format!("{XSD}integer") || dt == format!("{XSD}decimal") {
                self.write(text);
                return;
            }
        }
        let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
        let quote = if escaped.contains('\n') { "\"\"\"" } else { "\"" };
        self.write(quote);
        self.write(&escaped);
        self.write(quote);
        if let Some(lang) = lang {
            self.write("@");
            self.write(lang);
        } else if let Some(dt) = datatype.filter(|dt| *dt != format!("{XSD}string")) {
            self.write("^^");
            self.iri(dt);
        }
    }

    fn render(&mut self, node: &Node) {
        self.level += 1;
        let mut last: Option<&str> = None;
        for (pred, obj) in &node.triples {
            match last {
                Some(p) if p == pred => {
                    self.write(" ,");
                    self.newline();
                    self.object(obj);
                }
                Some(_) => {
                    self.write(" ;");
                    self.pop_tab();
                    self.newline();
                    self.iri(pred);
                    self.write(" ");
                    self.push_tab();
                    self.object(obj);
                }
                None => {
                    match &node.subject {
                        Subject::Iri(s) => {
                            self.iri(s);
                            self.write(" ");
                        }
                        Subject::Id(id) => {
                            self.write(id);
                            self.write(" ");
                        }
                        Subject::Blank => {
                            self.push_tab();
                            self.write("[");
                            self.write(" ");
                        }
                    }
                    self.push_tab();
                    self.iri(pred);
                    self.write(" ");
                    self.push_tab();
                    self.object(obj);
                }
            }
            last = Some(pred);
        }
        match &node.subject {
            Subject::Iri(_) => {
                self.pop_tab();
                self.pop_tab();
            }
            Subject::Id(_) => {
                self.pop_tab();
                self.pop_tab();
                self.pop_tab();
            }
            Subject::Blank => {
                self.pop_tab();
                self.pop_tab();
                if node.triples.is_empty() {
                    self.write("[ ");
                } else {
                    self.newline();
                }
                self.write("]");
                self.pop_tab();
            }
        }
        if self.level == 1 && !node.triples.is_empty() {
            self.write(" .\n\n");
        }
        self.level -= 1;
    }

    fn object(&mut self, obj: &Obj) {
        match obj {
            Obj::Iri(iri) => self.iri(iri),
            Obj::Id(id) => self.write(id),
            Obj::Literal { text, lang, datatype } => self.literal(text, lang.as_deref(), datatype.as_deref()),
            Obj::Node(node) => {
                self.push_tab();
                self.render(node);
                self.pop_tab();
            }
            Obj::List(items) => {
                self.push_tab();
                self.push_tab();
                self.write("(");
                self.write(" ");
                self.push_tab();
                for (i, it) in items.iter().enumerate() {
                    self.object(it);
                    if i + 1 < items.len() {
                        self.newline();
                    }
                }
                self.pop_tab();
                self.newline();
                self.write(")");
                self.pop_tab();
                self.pop_tab();
            }
        }
    }
}
