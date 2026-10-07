//! RDF/XML into statements, in the order the document completes them.
//!
//! The parse is a stack of states over the document's elements, and it hands
//! each statement to its [`Sink`] the moment the statement is complete:
//! - a node element states its type when it opens, then its property
//!   attributes;
//! - a property element with `rdf:resource` or `rdf:nodeID` states its triple
//!   when it opens; one with content states it when it closes, after every
//!   statement made inside it;
//! - a collection states each member's opening statements, then the list cell
//!   that holds the member, then the link to that cell.
//!
//! A blank node is named when it is made. `rdf:nodeID="v"` names
//! `_:genid-nodeid-v`, with every `genid` taken out of `v`. Every other blank
//! node takes `_:genid<n>` from the count it shares with the anonymous
//! individuals the reading makes (see [`super::order`]).
//!
//! A property element whose content is text states a literal however many
//! property attributes it has, and those attributes state nothing.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

pub(crate) const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XML_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral";

/// Where the parse sends what it reads.
pub(crate) trait Sink {
    /// The next value of the count blank nodes and anonymous individuals share.
    fn next_id(&mut self) -> u64;
    /// A statement whose object is a node.
    fn resource(&mut self, s: &str, p: &str, o: &str) -> Result<()>;
    /// A statement whose object is a literal.
    fn literal(&mut self, s: &str, p: &str, value: &str, lang: Option<&str>, datatype: Option<&str>) -> Result<()>;
    /// A prefix the document declares: an entity of its internal subset, or a
    /// namespace declaration (the default namespace's name is empty), in the
    /// order the document makes them.
    fn prefix(&mut self, _name: &str, _namespace: &str) {}
}

/// The node `rdf:nodeID="value"` names.
pub(crate) fn node_id(value: &str) -> String {
    if value.starts_with("_:genid-nodeid-") {
        value.to_string()
    } else {
        format!("_:genid-nodeid-{}", value.replace("genid", ""))
    }
}

/// Whether a node is a blank node: `_:` and `genid` somewhere after it.
pub(crate) fn is_blank(node: &str) -> bool {
    node.starts_with("_:") && node.contains("genid")
}

/// An element as the states see it: its name, and its attributes in document
/// order without the namespace declarations.
struct Elem {
    ns: String,
    local: String,
    qname: String,
    atts: Vec<Att>,
}

struct Att {
    ns: String,
    local: String,
    qname: String,
    value: String,
}

impl Elem {
    fn get(&self, ns: &str, local: &str) -> Option<&str> {
        self.atts.iter().find(|a| a.ns == ns && a.local == local).map(|a| a.value.as_str())
    }

    fn uri(&self) -> String {
        format!("{}{}", self.ns, self.local)
    }

    fn is_rdf(&self, local: &str) -> bool {
        self.ns == RDF && self.local == local
    }
}

/// An `rdf:bagID` bag: every statement made under it is reified, and the
/// reification is the bag's next member.
struct Bag {
    iri: String,
    members: u64,
}

/// A node element, or the node a `parseType="Resource"` property makes.
struct Node {
    subject: String,
    bag: Option<Bag>,
    next_li: u64,
}

enum State {
    /// Before the root element.
    Start,
    /// The node elements the root holds.
    NodeList,
    /// A node element, below the property elements it holds.
    Node,
    /// The property elements of node `.0`.
    Props(usize),
    /// A property element naming its object by `rdf:resource` or `rdf:nodeID`.
    Empty,
    /// A property element holding text, or one node element.
    Value { node: usize, prop: String, reif: Option<String>, datatype: Option<String>, text: String, inner: Option<usize> },
    /// A `parseType="Literal"` property element, or one of any unknown parse type.
    XmlLiteral { node: usize, prop: String, reif: Option<String>, depth: usize, content: String, declared: HashSet<String> },
    /// A `parseType="Resource"` property element.
    Resource,
    /// A `parseType="Collection"` property element.
    Collection { node: usize, prop: String, reif: Option<String>, last: Option<String> },
}

pub(crate) struct Parser<'a, S: Sink> {
    sink: &'a mut S,
    states: Vec<State>,
    nodes: Vec<Node>,
    base: Option<String>,
    bases: Vec<Option<String>>,
    lang: Option<String>,
    langs: Vec<Option<String>>,
    entities: HashMap<String, String>,
    /// The namespace declarations of each open element: a prefix (empty for
    /// the default namespace) and what it stands for (empty to undeclare the
    /// default).
    scopes: Vec<Vec<(String, String)>>,
}

impl<'a, S: Sink> Parser<'a, S> {
    pub(crate) fn new(sink: &'a mut S, base: Option<String>) -> Self {
        Parser {
            sink,
            states: vec![State::Start],
            nodes: Vec::new(),
            base,
            bases: Vec::new(),
            lang: None,
            langs: Vec::new(),
            entities: HashMap::new(),
            scopes: Vec::new(),
        }
    }

    /// Read `bytes`, handing every statement to the sink.
    pub(crate) fn parse(mut self, bytes: &[u8]) -> Result<()> {
        let decoded = decode(bytes)?;
        let bytes = &decoded[..];
        // Line ends read as one newline, as XML requires: CRLF and a lone CR.
        let normalized;
        let bytes = if bytes.contains(&b'\r') {
            normalized = normalize_line_ends(bytes);
            &normalized[..]
        } else {
            bytes
        };
        let mut reader = Reader::from_reader(bytes);
        reader.config_mut().expand_empty_elements = true;
        let mut buf = Vec::new();
        loop {
            let event = reader.read_event_into(&mut buf).map_err(|e| anyhow!("RDF/XML parse error: {e}"))?;
            match event {
                Event::Start(e) => {
                    let elem = self.element(&e)?;
                    self.open(elem)?;
                }
                Event::End(e) => {
                    let qname = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    self.close(&qname)?;
                    self.scopes.pop();
                }
                Event::Text(t) => {
                    let raw = std::str::from_utf8(t.as_ref())?;
                    let text = self.unescape(raw)?;
                    self.characters(&text)?;
                }
                Event::CData(c) => {
                    let text = std::str::from_utf8(c.as_ref())?.to_string();
                    self.characters(&text)?;
                }
                Event::DocType(d) => {
                    let text = std::str::from_utf8(d.as_ref())?.to_string();
                    self.doctype(&text)?;
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        Ok(())
    }

    /// The `<!ENTITY name "value">` declarations of the internal subset.
    fn doctype(&mut self, text: &str) -> Result<()> {
        for decl in text.split('<').skip(1) {
            let Some(rest) = decl.strip_prefix("!ENTITY") else { continue };
            let rest = rest.trim_start();
            // A parameter entity serves the DTD alone.
            let parameter = rest.starts_with('%');
            let rest = rest.strip_prefix('%').map(str::trim_start).unwrap_or(rest);
            let Some((name, rest)) = rest.split_once(|c: char| c.is_ascii_whitespace()) else {
                bail!("RDF/XML: an <!ENTITY declaration with no value");
            };
            let rest = rest.trim_start();
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                // An external entity (SYSTEM / PUBLIC) is not read.
                continue;
            };
            let Some((value, _)) = rest[1..].split_once(quote) else {
                bail!("RDF/XML: the <!ENTITY {name} value is not closed");
            };
            if parameter {
                continue;
            }
            let value = self.unescape(value)?;
            self.sink.prefix(name, &value);
            self.entities.entry(name.to_string()).or_insert(value);
        }
        Ok(())
    }

    fn unescape(&self, raw: &str) -> Result<String> {
        let entities = &self.entities;
        quick_xml::escape::unescape_with(raw, |name| {
            quick_xml::escape::resolve_xml_entity(name).or_else(|| entities.get(name).map(String::as_str))
        })
        .map(|c| c.into_owned())
        .map_err(|e| anyhow!("RDF/XML: {e}"))
    }

    /// An element with its names resolved. Its namespace declarations open a
    /// scope that its end closes.
    fn element(&mut self, e: &BytesStart) -> Result<Elem> {
        let qname = String::from_utf8_lossy(e.name().as_ref()).into_owned();
        let mut declared = Vec::new();
        let mut raw_atts = Vec::new();
        for att in e.attributes() {
            let att = att.map_err(|err| anyhow!("RDF/XML attribute of <{qname}>: {err}"))?;
            let key = String::from_utf8_lossy(att.key.as_ref()).into_owned();
            // An attribute value reads its tabs and newlines as spaces; a
            // character reference to one stays what it names.
            let raw = std::str::from_utf8(att.value.as_ref())?;
            let raw: String = raw.chars().map(|c| if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c }).collect();
            let value = self.unescape(&raw)?;
            match key.strip_prefix("xmlns") {
                Some("") => declared.push((String::new(), value)),
                Some(rest) if rest.starts_with(':') => declared.push((rest[1..].to_string(), value)),
                _ => raw_atts.push((key, value)),
            }
        }
        for (prefix, namespace) in &declared {
            self.sink.prefix(prefix, namespace);
        }
        self.scopes.push(declared);
        let (ns, local) = self.name(&qname, true)?;
        let mut atts = Vec::new();
        for (att_qname, value) in raw_atts {
            let (ns, local) = self.name(&att_qname, false)?;
            atts.push(Att { ns, local, qname: att_qname, value });
        }
        Ok(Elem { ns, local, qname, atts })
    }

    /// A qualified name's namespace and local name. An unprefixed element is in
    /// the default namespace; an unprefixed attribute is in none.
    fn name(&self, qname: &str, element: bool) -> Result<(String, String)> {
        let (prefix, local) = match qname.split_once(':') {
            Some((prefix, local)) => (prefix, local),
            None if element => ("", qname),
            None => return Ok((String::new(), qname.to_string())),
        };
        if prefix == "xml" {
            return Ok((XML.to_string(), local.to_string()));
        }
        let bound = self.scopes.iter().rev().flat_map(|scope| scope.iter().rev()).find(|(p, _)| p == prefix);
        match bound {
            Some((_, ns)) => Ok((ns.clone(), local.to_string())),
            None if prefix.is_empty() => Ok((String::new(), local.to_string())),
            None => bail!("RDF/XML: the prefix of <{qname}> ({prefix}) is not declared"),
        }
    }

    // -- base and language ---------------------------------------------------

    fn open(&mut self, e: Elem) -> Result<()> {
        self.bases.push(self.base.clone());
        if let Some(value) = e.get(XML, "base") {
            let resolved = self.resolve_against_base(value);
            self.base = Some(resolved);
        }
        self.langs.push(self.lang.clone());
        if let Some(a) = e.atts.iter().find(|a| a.qname == "xml:lang") {
            self.lang = Some(a.value.clone());
        }
        self.start(e)
    }

    fn close(&mut self, qname: &str) -> Result<()> {
        self.end(qname)?;
        self.base = self.bases.pop().flatten();
        self.lang = self.langs.pop().flatten();
        Ok(())
    }

    /// An IRI reference read against the current base.
    fn resolve(&self, iri: &str) -> String {
        if iri.is_empty() {
            // The document itself: the base without its fragment.
            let Some(base) = &self.base else { return String::new() };
            let (ns, _) = crate::owlapi_hash::iri_split(base);
            if let Some(stripped) = ns.strip_suffix('#') {
                return stripped.to_string();
            }
            return match base.find('#') {
                Some(i) => base[..i].to_string(),
                None => base.clone(),
            };
        }
        self.resolve_against_base(iri)
    }

    fn resolve_against_base(&self, value: &str) -> String {
        if is_blank(value) {
            return value.to_string();
        }
        let value = value.replace(' ', "%20");
        match &self.base {
            Some(base) => uri_resolve(base, &value),
            None => value,
        }
    }

    // -- statements -----------------------------------------------------------

    fn fresh(&mut self) -> String {
        format!("_:genid{}", self.sink.next_id())
    }

    fn resource(&mut self, s: &str, p: &str, o: &str, reif: Option<&str>) -> Result<()> {
        self.sink.resource(s, p, o)?;
        if let Some(r) = reif {
            self.sink.resource(r, RDF_TYPE, &format!("{RDF}statement"))?;
            self.sink.resource(r, &format!("{RDF}subject"), s)?;
            self.sink.resource(r, &format!("{RDF}predicate"), p)?;
            self.sink.resource(r, &format!("{RDF}object"), o)?;
        }
        Ok(())
    }

    fn literal(&mut self, s: &str, p: &str, value: &str, datatype: Option<&str>, reif: Option<&str>) -> Result<()> {
        let lang = self.lang.clone();
        self.sink.literal(s, p, value, lang.as_deref(), datatype)?;
        if let Some(r) = reif {
            self.sink.resource(r, RDF_TYPE, &format!("{RDF}statement"))?;
            self.sink.resource(r, &format!("{RDF}subject"), s)?;
            self.sink.resource(r, &format!("{RDF}predicate"), p)?;
            self.sink.literal(r, &format!("{RDF}object"), value, lang.as_deref(), datatype)?;
        }
        Ok(())
    }

    /// The bag an element's `rdf:bagID` opens, stating its type.
    fn bag(&mut self, e: &Elem) -> Result<Option<Bag>> {
        let Some(id) = e.get(RDF, "bagID") else { return Ok(None) };
        let iri = self.resolve(&format!("#{id}"));
        self.resource(&iri, RDF_TYPE, &format!("{RDF}Bag"), None)?;
        Ok(Some(Bag { iri, members: 1 }))
    }

    /// The reification of a statement made under `bag`: `id`, or a fresh node
    /// when a bag needs one and `id` is none; stated as the bag's next member.
    fn reification(&mut self, bag: &mut Option<Bag>, id: Option<String>) -> Result<Option<String>> {
        let Some(b) = bag else { return Ok(id) };
        let r = match id {
            Some(id) => id,
            None => format!("_:genid{}", self.sink.next_id()),
        };
        let p = format!("{RDF}_{}", b.members);
        b.members += 1;
        let iri = b.iri.clone();
        self.resource(&iri, &p, &r, None)?;
        Ok(Some(r))
    }

    /// A node's reification for a statement a property element makes: the
    /// element's `rdf:ID`, under the node's bag.
    fn node_reification(&mut self, node: usize, e: &Elem) -> Result<Option<String>> {
        let id = e.get(RDF, "ID").map(|v| format!("#{v}"));
        let id = id.map(|v| self.resolve(&v));
        let mut bag = self.nodes[node].bag.take();
        let r = self.reification(&mut bag, id);
        self.nodes[node].bag = bag;
        r
    }

    fn property_iri(&mut self, node: usize, e: &Elem) -> String {
        if e.is_rdf("li") {
            let n = &mut self.nodes[node];
            let p = format!("{RDF}_{}", n.next_li);
            n.next_li += 1;
            p
        } else {
            e.uri()
        }
    }

    /// The statements an element's property attributes make about `subject`.
    fn property_attributes(&mut self, subject: &str, e: &Elem, bag: &mut Option<Bag>) -> Result<()> {
        const NOT_PROPERTIES: [&str; 8] = ["ID", "nodeID", "about", "type", "resource", "parseType", "aboutEach", "aboutEachPrefix"];
        for a in &e.atts {
            if a.ns == XML {
                continue;
            }
            if a.ns == RDF && a.local == "type" {
                let value = self.resolve(&a.value);
                let r = self.reification(bag, None)?;
                self.resource(subject, RDF_TYPE, &value, r.as_deref())?;
            } else if !(a.ns == RDF && (NOT_PROPERTIES.contains(&a.local.as_str()) || a.local == "bagID")) {
                let r = self.reification(bag, None)?;
                let p = format!("{}{}", a.ns, a.local);
                self.literal(subject, &p, &a.value, None, r.as_deref())?;
            }
        }
        Ok(())
    }

    /// The object `rdf:resource` or `rdf:nodeID` names.
    fn named_object(&self, e: &Elem) -> Option<String> {
        if let Some(v) = e.get(RDF, "resource") {
            return Some(self.resolve(v));
        }
        e.get(RDF, "nodeID").map(node_id)
    }

    // -- the states -----------------------------------------------------------

    fn top(&mut self) -> &mut State {
        self.states.last_mut().expect("the state stack holds the document state")
    }

    /// Open a node element: its subject, its type, its property attributes.
    fn node_element(&mut self, e: &Elem) -> Result<usize> {
        let mut subject = None;
        if let Some(v) = e.get(RDF, "ID") {
            subject = Some(self.resolve(&format!("#{v}")));
        }
        if let Some(v) = e.get(RDF, "about") {
            if subject.is_some() {
                bail!("RDF/XML: <{}> has both rdf:ID and rdf:about", e.qname);
            }
            subject = Some(self.resolve(v));
        }
        if let Some(v) = e.get(RDF, "nodeID") {
            if subject.is_some() {
                bail!("RDF/XML: <{}> has rdf:nodeID with rdf:ID or rdf:about", e.qname);
            }
            subject = Some(node_id(v));
        }
        let subject = match subject {
            Some(s) => s,
            None => self.fresh(),
        };
        let mut bag = self.bag(e)?;
        if !e.is_rdf("Description") {
            let r = self.reification(&mut bag, None)?;
            self.resource(&subject, RDF_TYPE, &e.uri(), r.as_deref())?;
        }
        if e.get(RDF, "aboutEach").is_some() || e.get(RDF, "aboutEachPrefix").is_some() {
            bail!("RDF/XML: rdf:aboutEach and rdf:aboutEachPrefix are not read");
        }
        self.property_attributes(&subject, e, &mut bag)?;
        self.nodes.push(Node { subject, bag, next_li: 1 });
        Ok(self.nodes.len() - 1)
    }

    fn start(&mut self, e: Elem) -> Result<()> {
        match self.top() {
            State::Start => {
                self.states.push(State::NodeList);
                // A root that is not rdf:RDF is itself the one node element.
                if !e.is_rdf("RDF") {
                    return self.start(e);
                }
                Ok(())
            }
            State::NodeList => {
                self.states.push(State::Node);
                let node = self.node_element(&e)?;
                self.states.push(State::Props(node));
                Ok(())
            }
            State::Node => unreachable!("a node element's start opens its property list"),
            State::Props(node) => {
                let node = *node;
                self.property_element(node, e)
            }
            State::Empty => bail!("RDF/XML: <{}> inside a property element that names its object", e.qname),
            State::Value { text, datatype, .. } => {
                if text.chars().any(|c| !matches!(c, ' ' | '\n' | '\r' | '\t')) {
                    bail!("RDF/XML: <{}> after text in a property element", e.qname);
                }
                if datatype.is_some() {
                    bail!("RDF/XML: <{}> inside a property element with rdf:datatype", e.qname);
                }
                self.states.push(State::Node);
                let inner = self.node_element(&e)?;
                let at = self.states.len() - 2;
                if let State::Value { inner: slot, .. } = &mut self.states[at] {
                    *slot = Some(inner);
                }
                self.states.push(State::Props(inner));
                Ok(())
            }
            State::XmlLiteral { depth, content, declared, .. } => {
                content.push('<');
                content.push_str(&e.qname);
                if e.local != e.qname && declared.insert(e.ns.clone()) {
                    let prefix = e.qname.split(':').next().unwrap_or("");
                    content.push_str(&format!(" xmlns:{prefix}=\"{}\"", e.ns));
                }
                for a in &e.atts {
                    content.push_str(&format!(" {}=\"{}\"", a.qname, a.value));
                }
                content.push('>');
                *depth += 1;
                Ok(())
            }
            State::Resource => unreachable!("a parseType=Resource element's content opens its own property list"),
            State::Collection { .. } => {
                let at = self.states.len() - 1;
                self.states.push(State::Node);
                let member = self.node_element(&e)?;
                self.states.push(State::Props(member));
                let cell = self.fresh();
                let value = self.nodes[member].subject.clone();
                self.resource(&cell, &format!("{RDF}first"), &value, None)?;
                self.resource(&cell, RDF_TYPE, &format!("{RDF}List"), None)?;
                let State::Collection { node, prop, reif, last } = &self.states[at] else { unreachable!() };
                let (node, prop, reif, last) = (*node, prop.clone(), reif.clone(), last.clone());
                match last {
                    None => {
                        let subject = self.nodes[node].subject.clone();
                        self.resource(&subject, &prop, &cell, reif.as_deref())?;
                    }
                    Some(last) => self.resource(&last, &format!("{RDF}rest"), &cell, None)?,
                }
                if let State::Collection { last, .. } = &mut self.states[at] {
                    *last = Some(cell);
                }
                Ok(())
            }
        }
    }

    fn property_element(&mut self, node: usize, e: Elem) -> Result<()> {
        let parse_type = e.get(RDF, "parseType").map(str::to_string);
        let xml_typed = e.get(RDF, "datatype") == Some(XML_LITERAL);
        if parse_type.as_deref() == Some("Literal") || xml_typed {
            return self.xml_literal_element(node, &e);
        }
        match parse_type.as_deref() {
            Some("Resource") => {
                let prop = self.property_iri(node, &e);
                let reif = self.node_reification(node, &e)?;
                let subject = self.fresh();
                let bag = self.bag(&e)?;
                let parent = self.nodes[node].subject.clone();
                self.resource(&parent, &prop, &subject, reif.as_deref())?;
                self.nodes.push(Node { subject, bag, next_li: 1 });
                let inner = self.nodes.len() - 1;
                self.states.push(State::Resource);
                self.states.push(State::Props(inner));
                Ok(())
            }
            Some("Collection") => {
                let prop = self.property_iri(node, &e);
                let reif = self.node_reification(node, &e)?;
                self.states.push(State::Collection { node, prop, reif, last: None });
                Ok(())
            }
            Some(_) => self.xml_literal_element(node, &e),
            None => match self.named_object(&e) {
                Some(_) => {
                    self.states.push(State::Empty);
                    let prop = self.property_iri(node, &e);
                    let reif = self.node_reification(node, &e)?;
                    let object = self.named_object(&e).expect("checked above");
                    let subject = self.nodes[node].subject.clone();
                    self.resource(&subject, &prop, &object, reif.as_deref())?;
                    let mut bag = self.bag(&e)?;
                    self.property_attributes(&object, &e, &mut bag)
                }
                None => {
                    let prop = self.property_iri(node, &e);
                    let reif = self.node_reification(node, &e)?;
                    let datatype = e.get(RDF, "datatype").map(str::to_string);
                    self.states.push(State::Value { node, prop, reif, datatype, text: String::new(), inner: None });
                    Ok(())
                }
            },
        }
    }

    fn xml_literal_element(&mut self, node: usize, e: &Elem) -> Result<()> {
        let prop = self.property_iri(node, e);
        let reif = self.node_reification(node, e)?;
        self.states.push(State::XmlLiteral { node, prop, reif, depth: 1, content: String::new(), declared: HashSet::new() });
        Ok(())
    }

    fn end(&mut self, qname: &str) -> Result<()> {
        let Some(state) = self.states.pop() else { return Ok(()) };
        match state {
            State::Start | State::Node | State::Empty | State::Resource => Ok(()),
            // The list closes with the element that holds it.
            State::NodeList | State::Props(_) => self.end(qname),
            State::Value { node, prop, reif, datatype, text, inner } => {
                let subject = self.nodes[node].subject.clone();
                match inner {
                    Some(inner) => {
                        let object = self.nodes[inner].subject.clone();
                        self.resource(&subject, &prop, &object, reif.as_deref())
                    }
                    None => self.literal(&subject, &prop, &text, datatype.as_deref(), reif.as_deref()),
                }
            }
            State::XmlLiteral { node, prop, reif, depth, mut content, declared } => {
                if depth == 1 {
                    let subject = self.nodes[node].subject.clone();
                    self.literal(&subject, &prop, &content, Some(XML_LITERAL), reif.as_deref())
                } else {
                    content.push_str("</");
                    content.push_str(qname);
                    content.push('>');
                    self.states.push(State::XmlLiteral { node, prop, reif, depth: depth - 1, content, declared });
                    Ok(())
                }
            }
            State::Collection { node, prop, reif, last } => match last {
                None => {
                    let subject = self.nodes[node].subject.clone();
                    self.resource(&subject, &prop, &format!("{RDF}nil"), reif.as_deref())
                }
                Some(last) => self.resource(&last, &format!("{RDF}rest"), &format!("{RDF}nil"), None),
            },
        }
    }

    fn characters(&mut self, text: &str) -> Result<()> {
        let blank = text.chars().all(|c| matches!(c, ' ' | '\n' | '\r' | '\t'));
        match self.states.last_mut() {
            None => Ok(()),
            Some(State::Value { inner: None, text: t, .. }) => {
                t.push_str(text);
                Ok(())
            }
            Some(State::XmlLiteral { content, .. }) => {
                escape_xml(text, content);
                Ok(())
            }
            Some(State::Empty) => bail!("RDF/XML: text inside a property element that names its object"),
            Some(_) if blank => Ok(()),
            Some(_) => bail!("RDF/XML: unexpected text {:?}", text.trim()),
        }
    }
}

/// The document as UTF-8. A byte-order mark says UTF-8 or UTF-16; otherwise
/// the XML declaration's `encoding` does, UTF-8 when it names none. ISO-8859-1
/// and US-ASCII are read as well; any other encoding is refused.
fn decode(bytes: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return Ok(Cow::Borrowed(rest));
    }
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| -> Result<Cow<'_, [u8]>> {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect();
        let text = String::from_utf16(&units).map_err(|e| anyhow!("RDF/XML: not UTF-16: {e}"))?;
        Ok(Cow::Owned(text.into_bytes()))
    };
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return utf16(rest, u16::from_be_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return utf16(rest, u16::from_le_bytes);
    }
    let declaration = match bytes.strip_prefix(b"<?xml") {
        Some(rest) => {
            let end = rest.windows(2).position(|w| w == b"?>").unwrap_or(0);
            String::from_utf8_lossy(&rest[..end]).into_owned()
        }
        None => String::new(),
    };
    let encoding = declaration.split_once("encoding").and_then(|(_, rest)| {
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        rest[1..].split(quote).next().map(str::to_ascii_lowercase)
    });
    match encoding.as_deref() {
        None | Some("utf-8" | "utf8") => Ok(Cow::Borrowed(bytes)),
        Some("iso-8859-1" | "iso8859-1" | "iso_8859-1" | "latin1" | "l1" | "us-ascii" | "ascii") => {
            if bytes.is_ascii() {
                return Ok(Cow::Borrowed(bytes));
            }
            Ok(Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect::<String>().into_bytes()))
        }
        Some(other) => bail!("RDF/XML: the document's encoding, {other}, is not one this reader reads"),
    }
}

fn normalize_line_ends(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            out.push(b'\n');
            if bytes.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    out
}

/// Text of an XML literal: `<`, `>`, `"`, `&` and `'` as named references.
fn escape_xml(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
}

/// The parts of a hierarchical URI reference.
#[derive(Clone, Default)]
struct UriParts {
    scheme: Option<String>,
    authority: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
    opaque: bool,
}

fn uri_parts(s: &str) -> UriParts {
    let mut p = UriParts::default();
    let mut rest = s;
    if let Some(i) = rest.find('#') {
        p.fragment = Some(rest[i + 1..].to_string());
        rest = &rest[..i];
    }
    if let Some(i) = rest.find(':') {
        let scheme = &rest[..i];
        let valid = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if valid {
            p.scheme = Some(scheme.to_string());
            rest = &rest[i + 1..];
            if !rest.starts_with('/') {
                p.opaque = true;
                p.path = rest.to_string();
                return p;
            }
        }
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?']).unwrap_or(after.len());
        p.authority = Some(after[..end].to_string());
        rest = &after[end..];
    }
    if let Some(i) = rest.find('?') {
        p.query = Some(rest[i + 1..].to_string());
        rest = &rest[..i];
    }
    p.path = rest.to_string();
    p
}

fn uri_string(p: &UriParts) -> String {
    let mut s = String::new();
    if let Some(scheme) = &p.scheme {
        s.push_str(scheme);
        s.push(':');
    }
    if let Some(a) = &p.authority {
        s.push_str("//");
        s.push_str(a);
    }
    s.push_str(&p.path);
    if let Some(q) = &p.query {
        s.push('?');
        s.push_str(q);
    }
    if let Some(f) = &p.fragment {
        s.push('#');
        s.push_str(f);
    }
    s
}

/// `reference` read against `base`, by the rules of RFC 2396 section 5.2:
/// an opaque reference or base gives the reference; a lone fragment replaces
/// the base's; a reference with a scheme stands; otherwise the base's scheme
/// and authority with the reference's path merged into the base's.
pub(crate) fn uri_resolve(base: &str, reference: &str) -> String {
    let b = uri_parts(base);
    let c = uri_parts(reference);
    if c.opaque || b.opaque {
        return reference.to_string();
    }
    if c.scheme.is_none() && c.authority.is_none() && c.path.is_empty() && c.fragment.is_some() && c.query.is_none() {
        if b.fragment.is_some() && b.fragment == c.fragment {
            return base.to_string();
        }
        let mut r = b.clone();
        r.fragment = c.fragment;
        return uri_string(&r);
    }
    if c.scheme.is_some() {
        return reference.to_string();
    }
    let mut r = UriParts { scheme: b.scheme.clone(), query: c.query.clone(), fragment: c.fragment.clone(), ..Default::default() };
    if c.authority.is_none() {
        r.authority = b.authority.clone();
        if c.path.starts_with('/') {
            r.path = c.path.clone();
        } else {
            r.path = resolve_path(&b.path, &c.path, b.scheme.is_some());
        }
    } else {
        r.authority = c.authority.clone();
        r.path = c.path.clone();
    }
    uri_string(&r)
}

fn resolve_path(base: &str, child: &str, absolute: bool) -> String {
    let i = base.rfind('/');
    let path = if child.is_empty() {
        match i {
            Some(i) => base[..=i].to_string(),
            None => String::new(),
        }
    } else {
        match i {
            Some(i) => format!("{}{child}", &base[..=i]),
            None if !absolute => child.to_string(),
            None => format!("/{child}"),
        }
    };
    normalize_path(&path)
}

/// A path with its `.` segments removed and each `..` taking the segment
/// before it, when there is one that is not itself `..`.
fn normalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let body = if absolute { &path[1..] } else { path };
    let segments: Vec<&str> = body.split('/').collect();
    let mut out: Vec<&str> = Vec::new();
    let last = segments.len().saturating_sub(1);
    let mut trailing_slash = false;
    for (k, seg) in segments.iter().enumerate() {
        match *seg {
            "." => {
                if k == last {
                    trailing_slash = true;
                }
            }
            ".." => {
                if out.last().is_some_and(|s| *s != "..") {
                    out.pop();
                    if k == last {
                        trailing_slash = true;
                    }
                } else {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let mut s = if absolute { String::from("/") } else { String::new() };
    s.push_str(&out.join("/"));
    if trailing_slash && !s.ends_with('/') {
        s.push('/');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Record {
        next: u64,
        lines: Vec<String>,
    }

    impl Sink for Record {
        fn next_id(&mut self) -> u64 {
            self.next += 1;
            2_147_483_647 + self.next
        }
        fn resource(&mut self, s: &str, p: &str, o: &str) -> Result<()> {
            self.lines.push(format!("{s} {p} {o}"));
            Ok(())
        }
        fn literal(&mut self, s: &str, p: &str, v: &str, lang: Option<&str>, dt: Option<&str>) -> Result<()> {
            self.lines.push(format!("{s} {p} {v:?} {lang:?} {dt:?}"));
            Ok(())
        }
    }

    fn read(doc: &str) -> Vec<String> {
        read_bytes(doc.as_bytes())
    }

    fn read_bytes(doc: &[u8]) -> Vec<String> {
        let mut r = Record::default();
        Parser::new(&mut r, Some("http://example.org/doc".into())).parse(doc).unwrap();
        r.lines
    }

    const HEAD: &str = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:x="http://x/">"#;

    #[test]
    fn a_nested_node_is_linked_after_its_own_statements() {
        let doc = format!(r#"{HEAD}<x:A rdf:about="http://x/a"><x:p><x:B><x:q>v</x:q></x:B></x:p></x:A></rdf:RDF>"#);
        assert_eq!(
            read(&doc),
            [
                "http://x/a http://www.w3.org/1999/02/22-rdf-syntax-ns#type http://x/A",
                "_:genid2147483648 http://www.w3.org/1999/02/22-rdf-syntax-ns#type http://x/B",
                "_:genid2147483648 http://x/q \"v\" None None",
                "http://x/a http://x/p _:genid2147483648",
            ]
        );
    }

    #[test]
    fn a_collection_member_comes_before_its_cell() {
        let doc = format!(
            r#"{HEAD}<rdf:Description rdf:about="http://x/a"><x:l rdf:parseType="Collection"><rdf:Description/><rdf:Description rdf:nodeID="genid7"/></x:l></rdf:Description></rdf:RDF>"#
        );
        assert_eq!(
            read(&doc),
            [
                "_:genid2147483649 http://www.w3.org/1999/02/22-rdf-syntax-ns#first _:genid2147483648",
                "_:genid2147483649 http://www.w3.org/1999/02/22-rdf-syntax-ns#type http://www.w3.org/1999/02/22-rdf-syntax-ns#List",
                "http://x/a http://x/l _:genid2147483649",
                "_:genid2147483650 http://www.w3.org/1999/02/22-rdf-syntax-ns#first _:genid-nodeid-7",
                "_:genid2147483650 http://www.w3.org/1999/02/22-rdf-syntax-ns#type http://www.w3.org/1999/02/22-rdf-syntax-ns#List",
                "_:genid2147483649 http://www.w3.org/1999/02/22-rdf-syntax-ns#rest _:genid2147483650",
                "_:genid2147483650 http://www.w3.org/1999/02/22-rdf-syntax-ns#rest http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
            ]
        );
    }

    #[test]
    fn rdf_id_resolves_against_the_base_and_reifies() {
        let doc = format!(r##"{HEAD}<rdf:Description rdf:ID="a"><x:p rdf:ID="s" rdf:resource="#b"/></rdf:Description></rdf:RDF>"##);
        assert_eq!(
            read(&doc),
            [
                "http://example.org/doc#a http://x/p http://example.org/doc#b",
                "http://example.org/doc#s http://www.w3.org/1999/02/22-rdf-syntax-ns#type http://www.w3.org/1999/02/22-rdf-syntax-ns#statement",
                "http://example.org/doc#s http://www.w3.org/1999/02/22-rdf-syntax-ns#subject http://example.org/doc#a",
                "http://example.org/doc#s http://www.w3.org/1999/02/22-rdf-syntax-ns#predicate http://x/p",
                "http://example.org/doc#s http://www.w3.org/1999/02/22-rdf-syntax-ns#object http://example.org/doc#b",
            ]
        );
    }

    #[test]
    fn entities_language_and_line_ends() {
        let doc = format!(
            "<?xml version=\"1.0\"?>\r\n<!DOCTYPE rdf:RDF [<!ENTITY x 'http://x/'>]>\r\n{HEAD}<rdf:Description rdf:about=\"&x;a\" xml:lang=\"en\"><x:p>one\r\ntwo</x:p><x:q rdf:datatype=\"&x;t\">3</x:q></rdf:Description></rdf:RDF>"
        );
        assert_eq!(
            read(&doc),
            ["http://x/a http://x/p \"one\\ntwo\" Some(\"en\") None", "http://x/a http://x/q \"3\" Some(\"en\") Some(\"http://x/t\")"]
        );
    }

    /// A namespace declaration reads its entity references as any attribute
    /// does, and an element's own declarations are in scope for its name and
    /// its content only.
    #[test]
    fn namespaces_declared_through_entities_and_on_inner_elements() {
        let doc = r#"<!DOCTYPE rdf:RDF [<!ENTITY food "http://example.org/food#">]>
<rdf:RDF xmlns="&food;" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <Fish rdf:about="&food;cod" xmlns:t="http://example.org/taste#"><t:is rdf:resource="&food;bland"/></Fish>
  <Fish xmlns="http://example.org/other#" rdf:about="&food;hake"/>
  <Fish rdf:about="&food;ling"/>
</rdf:RDF>"#;
        assert_eq!(
            read(doc),
            [
                "http://example.org/food#cod http://www.w3.org/1999/02/22-rdf-syntax-ns#type http://example.org/food#Fish",
                "http://example.org/food#cod http://example.org/taste#is http://example.org/food#bland",
                "http://example.org/food#hake http://www.w3.org/1999/02/22-rdf-syntax-ns#type http://example.org/other#Fish",
                "http://example.org/food#ling http://www.w3.org/1999/02/22-rdf-syntax-ns#type http://example.org/food#Fish",
            ]
        );
    }

    #[test]
    fn a_document_in_latin_1_or_utf_16() {
        let latin1 = b"<?xml version='1.0' encoding='ISO-8859-1'?>\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:x=\"http://x/\"><rdf:Description rdf:about=\"http://x/a\"><x:p>caf\xE9</x:p></rdf:Description></rdf:RDF>";
        let mut r = Record::default();
        Parser::new(&mut r, None).parse(latin1).unwrap();
        assert_eq!(r.lines, ["http://x/a http://x/p \"caf\u{e9}\" None None"]);

        let text = format!("{HEAD}<rdf:Description rdf:about=\"http://x/a\"><x:p>caf\u{e9}</x:p></rdf:Description></rdf:RDF>");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(read_bytes(&utf16), r.lines);

        let other = b"<?xml version='1.0' encoding='Shift_JIS'?><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"/>";
        assert!(Parser::new(&mut Record::default(), None).parse(other).is_err());
    }

    #[test]
    fn property_attributes_on_an_element_with_content_state_nothing() {
        let doc = format!(r#"{HEAD}<rdf:Description rdf:about="http://x/a"><x:p x:q="1"/></rdf:Description></rdf:RDF>"#);
        assert_eq!(read(&doc), ["http://x/a http://x/p \"\" None None"]);
    }

    #[test]
    fn relative_references() {
        assert_eq!(uri_resolve("http://a/b/c/d;p?q", "g"), "http://a/b/c/g");
        assert_eq!(uri_resolve("http://a/b/c/d;p?q", "../g"), "http://a/b/g");
        assert_eq!(uri_resolve("http://a/b/c/d;p?q", "#s"), "http://a/b/c/d;p?q#s");
        assert_eq!(uri_resolve("http://a/b/c/d;p?q", "/g"), "http://a/g");
        assert_eq!(uri_resolve("http://a/b/c/d;p?q", "http://x/y"), "http://x/y");
        assert_eq!(uri_resolve("http://a/b/c/d;p?q", "./"), "http://a/b/c/");
        assert_eq!(uri_resolve("urn:x:y", "#s"), "#s");
    }
}
