//! Loading an ontology's RDF triples into the `statements` table.
//!
//! The table is the RDF/XML document, one row per triple, with three things the
//! raw triple stream does not carry:
//!
//! * **CURIEs.** Every IRI is shortened against the `prefix` table — the longest
//!   matching base wins — so `http://purl.obolibrary.org/obo/CL_0000000` is
//!   stored as `CL:0000000`. An IRI under no known base is stored in angle
//!   brackets.
//! * **Stanzas.** Each top-level element of the document is one stanza, and every
//!   triple it produces — including the blank-node trees hanging off it — carries
//!   the stanza's subject in `statements.stanza`. That is what lets a query pull
//!   a term's whole description, class expressions included, with one indexed
//!   lookup.
//! * **Row order.** A stanza's rows are written innermost-first: the triples are
//!   collected as the element is read and flushed in reverse when it closes, so a
//!   class's own assertions follow the anonymous expressions they are built from.
//!
//! The parser is the RDF/XML grammar itself (node elements, property elements,
//! `rdf:parseType` resource/collection/literal, property attributes, reification
//! by `rdf:ID`), because the stanza boundary is an XML fact — where an element of
//! `rdf:RDF` ends — and no triple-level API exposes it.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::NsReader;
use rusqlite::Connection;

const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDF_ABOUT: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#about";
const RDF_DATATYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#datatype";
const RDF_DESCRIPTION: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#Description";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_ID: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#ID";
const RDF_LI: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#li";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const RDF_NODE_ID: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nodeID";
const RDF_OBJECT: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#object";
const RDF_PARSE_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#parseType";
const RDF_PREDICATE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#predicate";
const RDF_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#RDF";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_RESOURCE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#resource";
const RDF_STATEMENT: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#Statement";
const RDF_SUBJECT: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#subject";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_XML_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral";
const OWL_ANNOTATED_SOURCE: &str = "http://www.w3.org/2002/07/owl#annotatedSource";

/// One `statements` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub stanza: String,
    pub subject: String,
    pub predicate: String,
    pub object: Option<String>,
    pub value: Option<String>,
    pub datatype: Option<String>,
    pub language: Option<String>,
}

/// A node: an IRI or a blank node, already CURIE-shortened.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Iri(String),
    Blank(String),
}

impl Node {
    fn is_iri(&self) -> bool {
        matches!(self, Node::Iri(_))
    }
}

/// The object of a triple.
#[derive(Debug, Clone)]
enum Obj {
    Node(Node),
    Literal { value: String, datatype: Option<String>, language: Option<String> },
}

/// Read `owl` and insert its triples into `statements`.
pub fn load(conn: &Connection, owl: &Path, prefixes: &[(String, String)]) -> Result<()> {
    let rows = parse_file(owl, prefixes)?;
    let tx = conn.unchecked_transaction()?;
    {
        let mut ins = tx.prepare(
            "INSERT INTO statements (stanza,subject,predicate,object,value,datatype,language) \
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
        )?;
        for r in &rows {
            ins.execute((
                &r.stanza,
                &r.subject,
                &r.predicate,
                &r.object,
                &r.value,
                &r.datatype,
                &r.language,
            ))?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// The `statements` rows of an RDF/XML file, in insertion order.
pub fn parse_file(owl: &Path, prefixes: &[(String, String)]) -> Result<Vec<Row>> {
    let text = std::fs::read_to_string(owl)
        .with_context(|| format!("reading {}", owl.display()))?;
    // The stanza is an RDF/XML fact — where a top-level element of the document
    // ends — so the table can only be built from RDF/XML. Say so rather than
    // reading another syntax as XML and filling the table with nothing.
    if !text.trim_start().starts_with('<') {
        bail!(
            "{} is not RDF/XML; the statements table is built from the RDF/XML serialization",
            owl.display()
        );
    }
    parse_str(&text, prefixes)
}

/// Bases sorted longest-first, so the most specific prefix wins.
fn ranked(prefixes: &[(String, String)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = prefixes.to_vec();
    v.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    v
}

/// The CURIE for an IRI, or the IRI in angle brackets when no base matches.
fn shorten(ranked: &[(String, String)], iri: &str) -> String {
    for (p, base) in ranked {
        if !base.is_empty() && iri.starts_with(base.as_str()) {
            return iri.replace(base.as_str(), &format!("{p}:"));
        }
    }
    format!("<{iri}>")
}

/// `riog` + an eight-digit counter, one per node element and per anonymous
/// object, in document order.
#[derive(Default)]
struct Bnodes(usize);

impl Bnodes {
    fn next(&mut self) -> String {
        self.0 += 1;
        format!("riog{:08}", self.0)
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum ParseType {
    Default,
    Collection,
    Literal,
    Resource,
    Other,
}

enum State {
    Doc,
    Rdf,
    NodeElt { subject: Node, li: usize },
    PropertyElt {
        iri: String,
        subject: Node,
        object: Option<ObjSlot>,
        id_attr: Option<String>,
        datatype: Option<String>,
        language: Option<String>,
    },
    Collection {
        iri: String,
        subject: Node,
        objects: Vec<Node>,
        id_attr: Option<String>,
    },
    XmlLiteral {
        iri: String,
        subject: Node,
        writer: quick_xml::Writer<Vec<u8>>,
        id_attr: Option<String>,
        emit: bool,
    },
}

enum ObjSlot {
    Node(Node),
    Text(String),
}

struct Parser<'a> {
    ranked: Vec<(String, String)>,
    bnodes: Bnodes,
    state: Vec<State>,
    depth: usize,
    literal_depth: usize,
    /// The triples of the stanza being read, in parse order.
    pending: Vec<(Node, String, Obj)>,
    /// The stanza subject, as the document has revealed it so far.
    stanza: String,
    rows: &'a mut Vec<Row>,
    /// `xml:base`/`xml:lang` in scope, innermost last.
    base: Vec<Option<String>>,
    lang: Vec<Option<String>>,
}

/// Parse an RDF/XML document into `statements` rows.
pub fn parse_str(text: &str, prefixes: &[(String, String)]) -> Result<Vec<Row>> {
    let mut rows: Vec<Row> = Vec::new();
    {
        let mut p = Parser {
            ranked: ranked(prefixes),
            bnodes: Bnodes::default(),
            state: vec![State::Doc],
            depth: 0,
            literal_depth: 0,
            pending: Vec::new(),
            stanza: String::new(),
            rows: &mut rows,
            base: vec![None],
            lang: vec![None],
        };
        p.run(text)?;
    }
    Ok(rows)
}

impl Parser<'_> {
    fn run(&mut self, text: &str) -> Result<()> {
        let mut reader = NsReader::from_str(text);
        reader.config_mut().expand_empty_elements = true;
        reader.config_mut().trim_text(true);
        loop {
            // The event borrows the reader, so the namespace binding is resolved
            // into an owned name before the element itself is read.
            match reader.read_resolved_event() {
                Ok((ns, Event::Start(e))) => {
                    let ns = match ns {
                        ResolveResult::Bound(n) => {
                            Some(std::str::from_utf8(n.as_ref())?.to_string())
                        }
                        _ => None,
                    };
                    let name = format!(
                        "{}{}",
                        ns.unwrap_or_default(),
                        std::str::from_utf8(e.local_name().as_ref())?
                    );
                    let attrs = self.attr_names(&reader, &e)?;
                    self.start(&e, name, attrs)?;
                }
                Ok((_, Event::Text(e))) => self.on_text(&e)?,
                Ok((_, Event::End(e))) => self.end(&e)?,
                Ok((_, Event::Eof)) => break,
                Ok(_) => {}
                Err(e) => bail!("RDF/XML parse error: {e}"),
            }
        }
        Ok(())
    }

    /// The expanded name of each of an element's attributes, in document order.
    fn attr_names(&self, r: &NsReader<&[u8]>, e: &BytesStart<'_>) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for a in e.attributes() {
            let a: Attribute<'_> = a.map_err(|e| anyhow::anyhow!("{e}"))?;
            let (ns, local) = r.resolve_attribute(a.key);
            out.push(match ns {
                ResolveResult::Bound(n) => {
                    format!("{}{}", std::str::from_utf8(n.as_ref())?, std::str::from_utf8(local.as_ref())?)
                }
                _ => std::str::from_utf8(local.as_ref())?.to_string(),
            });
        }
        Ok(out)
    }
}

impl Parser<'_> {
    /// A node's text in the table: a CURIE for an IRI, `_:id` for a blank node.
    fn text(&self, n: &Node) -> String {
        match n {
            Node::Iri(iri) => shorten(&self.ranked, iri),
            Node::Blank(b) => format!("_:{b}"),
        }
    }

    /// Record a triple of the stanza being read, and let it name the stanza.
    ///
    /// The stanza is the last named subject the block produced; a block whose
    /// subjects are all anonymous — an `owl:Axiom` reifying an assertion — takes
    /// its name from the first `owl:annotatedSource`/`rdf:subject` it carries
    /// instead, which is the term the axiom is about.
    fn emit(&mut self, subject: Node, predicate: String, object: Obj) {
        if subject.is_iri() {
            self.stanza = self.text(&subject);
        } else if self.stanza.is_empty()
            && (predicate == OWL_ANNOTATED_SOURCE || predicate == RDF_SUBJECT)
        {
            if let Obj::Node(o @ Node::Iri(_)) = &object {
                self.stanza = self.text(o);
            }
        }
        self.pending.push((subject, predicate, object));
    }

    /// Write out the stanza that has just closed, innermost triple first.
    ///
    /// A block that named no stanza — every subject anonymous, and no
    /// `owl:annotatedSource` to point at a term — is named after the predicate of
    /// its outermost triple, which is the last one collected and so the first one
    /// written. A bare `owl:disjointWith` between two anonymous class
    /// expressions is the case: it belongs to no term, and the predicate is what
    /// there is to call it.
    fn flush(&mut self) {
        while let Some((subject, predicate, object)) = self.pending.pop() {
            let subj = self.text(&subject);
            if self.stanza.is_empty() {
                self.stanza = shorten(&self.ranked, &predicate);
            }
            let (object, value, datatype, language) = match object {
                Obj::Node(n) => (Some(self.text(&n)), None, None, None),
                Obj::Literal { value, datatype, language } => (
                    None,
                    Some(value),
                    datatype.map(|d| shorten(&self.ranked, &d)),
                    language,
                ),
            };
            let row = Row {
                stanza: self.stanza.clone(),
                subject: subj,
                predicate: shorten(&self.ranked, &predicate),
                object,
                value,
                datatype,
                language,
            };
            self.rows.push(row);
        }
        self.stanza.clear();
    }

    /// The four triples that describe a triple, for a property element carrying
    /// `rdf:ID`.
    fn reify(&mut self, id: &str, subject: &Node, predicate: &str, object: &Obj) {
        let st = Node::Iri(id.to_string());
        self.emit(st.clone(), RDF_TYPE.into(), Obj::Node(Node::Iri(RDF_STATEMENT.into())));
        self.emit(st.clone(), RDF_SUBJECT.into(), Obj::Node(subject.clone()));
        self.emit(st.clone(), RDF_PREDICATE.into(), Obj::Node(Node::Iri(predicate.to_string())));
        self.emit(st, RDF_OBJECT.into(), object.clone());
    }

    /// Attribute-shaped properties on a node or property element
    /// (`<owl:Class rdfs:label="x"/>`), which assert plain literals.
    fn property_attrs(&mut self, subject: &Node, attrs: Vec<(String, String)>, lang: &Option<String>) {
        for (p, v) in attrs {
            self.emit(
                subject.clone(),
                p,
                Obj::Literal { value: v, datatype: None, language: lang.clone() },
            );
        }
    }

    fn start(&mut self, e: &BytesStart<'_>, iri: String, names: Vec<String>) -> Result<()> {
        self.depth += 1;

        // Inside `rdf:parseType="Literal"`, the markup IS the value: copy it
        // through rather than reading it as RDF.
        if matches!(self.state.last(), Some(State::XmlLiteral { .. })) || self.literal_depth > 0 {
            if let Some(State::XmlLiteral { writer, .. }) = self.state.last_mut() {
                writer
                    .write_event(Event::Start(e.clone()))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            self.literal_depth += 1;
            return Ok(());
        }

        let mut language = self.lang.last().cloned().flatten();
        let mut base = self.base.last().cloned().flatten();
        let mut id_attr: Option<String> = None;
        let mut node_id: Option<String> = None;
        let mut about: Option<String> = None;
        let mut resource: Option<String> = None;
        let mut datatype: Option<String> = None;
        let mut type_attr: Option<String> = None;
        let mut parse_type = ParseType::Default;
        let mut attrs: Vec<(String, String)> = Vec::new();

        for (i, a) in e.attributes().enumerate() {
            let a = a.map_err(|e| anyhow::anyhow!("{e}"))?;
            let key = a.key.as_ref().to_vec();
            let val = a.unescape_value().map_err(|e| anyhow::anyhow!("{e}"))?.into_owned();
            if key == b"xml:lang" {
                language = Some(val.to_ascii_lowercase());
                continue;
            }
            if key == b"xml:base" {
                base = Some(val);
                continue;
            }
            if key.starts_with(b"xml") {
                continue;
            }
            let name = names[i].clone();
            match name.as_str() {
                RDF_ID => id_attr = Some(format!("#{val}")),
                RDF_NODE_ID => node_id = Some(val),
                RDF_ABOUT => about = Some(val),
                RDF_RESOURCE => resource = Some(val),
                RDF_DATATYPE => datatype = Some(val),
                RDF_TYPE => type_attr = Some(val),
                RDF_PARSE_TYPE => {
                    parse_type = match val.as_str() {
                        "Collection" => ParseType::Collection,
                        "Literal" => ParseType::Literal,
                        "Resource" => ParseType::Resource,
                        _ => ParseType::Other,
                    }
                }
                _ => attrs.push((name, val)),
            }
        }

        let id_attr = id_attr.map(|i| resolve(base.as_deref(), &i));
        let about = about.map(|i| resolve(base.as_deref(), &i));
        let resource = resource.map(|i| resolve(base.as_deref(), &i));
        let datatype = datatype.map(|i| resolve(base.as_deref(), &i));
        let type_attr = type_attr.map(|i| resolve(base.as_deref(), &i));
        self.base.push(base);
        self.lang.push(language.clone());

        let production = match self.state.last() {
            Some(State::Doc) => Production::Rdf,
            Some(State::Rdf) => Production::NodeElt,
            Some(State::NodeElt { subject, .. }) => Production::PropertyElt(subject.clone()),
            Some(State::PropertyElt { .. }) | Some(State::Collection { .. }) => Production::NodeElt,
            Some(State::XmlLiteral { .. }) | None => {
                bail!("unbalanced RDF/XML at <{iri}>")
            }
        };

        let new_state = match production {
            Production::Rdf if iri == RDF_RDF => State::Rdf,
            Production::Rdf | Production::NodeElt => {
                self.node_elt(&iri, id_attr, node_id, about, type_attr, attrs, &language)
            }
            Production::PropertyElt(subject) => {
                let iri = if iri == RDF_LI {
                    if let Some(State::NodeElt { li, .. }) = self.state.last_mut() {
                        *li += 1;
                        format!("{RDF_NS}_{li}")
                    } else {
                        bail!("rdf:li outside a node element")
                    }
                } else {
                    iri
                };
                match parse_type {
                    ParseType::Default => {
                        if resource.is_some() || node_id.is_some() || !attrs.is_empty() {
                            let object = match (resource, node_id) {
                                (Some(r), None) => Node::Iri(r),
                                (None, Some(n)) => Node::Blank(n),
                                (None, None) => Node::Blank(self.bnodes.next()),
                                (Some(_), Some(_)) => {
                                    bail!("both rdf:resource and rdf:nodeID on one property")
                                }
                            };
                            self.property_attrs(&object, attrs, &language);
                            if let Some(t) = type_attr {
                                self.emit(
                                    object.clone(),
                                    RDF_TYPE.into(),
                                    Obj::Node(Node::Iri(t)),
                                );
                            }
                            State::PropertyElt {
                                iri,
                                subject,
                                object: Some(ObjSlot::Node(object)),
                                id_attr,
                                datatype,
                                language,
                            }
                        } else {
                            State::PropertyElt {
                                iri,
                                subject,
                                object: None,
                                id_attr,
                                datatype,
                                language,
                            }
                        }
                    }
                    ParseType::Literal => State::XmlLiteral {
                        iri,
                        subject,
                        writer: quick_xml::Writer::new(Vec::new()),
                        id_attr,
                        emit: true,
                    },
                    ParseType::Other => State::XmlLiteral {
                        iri,
                        subject,
                        writer: quick_xml::Writer::new(Vec::new()),
                        id_attr,
                        emit: false,
                    },
                    ParseType::Resource => {
                        let object = Node::Blank(self.bnodes.next());
                        let obj = Obj::Node(object.clone());
                        if let Some(id) = &id_attr {
                            let id = id.clone();
                            self.reify(&id, &subject, &iri, &obj);
                        }
                        self.emit(subject, iri, obj);
                        State::NodeElt { subject: object, li: 0 }
                    }
                    ParseType::Collection => State::Collection {
                        iri,
                        subject,
                        objects: Vec::new(),
                        id_attr,
                    },
                }
            }
        };
        self.state.push(new_state);
        Ok(())
    }

    /// A node element: `<owl:Class rdf:about="…">`, `<rdf:Description>`, or an
    /// anonymous `<owl:Restriction>`.
    fn node_elt(
        &mut self,
        iri: &str,
        id_attr: Option<String>,
        node_id: Option<String>,
        about: Option<String>,
        type_attr: Option<String>,
        attrs: Vec<(String, String)>,
        language: &Option<String>,
    ) -> State {
        // One id per node element, named or not, so the anonymous ones are
        // numbered by their position in the document.
        let fresh = self.bnodes.next();
        let subject = match (&id_attr, &node_id, &about) {
            (Some(i), None, None) => Node::Iri(i.clone()),
            (None, Some(n), None) => Node::Blank(n.clone()),
            (None, None, Some(a)) => Node::Iri(a.clone()),
            _ => Node::Blank(fresh),
        };
        self.property_attrs(&subject, attrs, language);
        if let Some(t) = type_attr {
            self.emit(subject.clone(), RDF_TYPE.into(), Obj::Node(Node::Iri(t)));
        }
        if iri != RDF_DESCRIPTION {
            self.emit(subject.clone(), RDF_TYPE.into(), Obj::Node(Node::Iri(iri.to_string())));
        }
        State::NodeElt { subject, li: 0 }
    }

    fn on_text(&mut self, e: &quick_xml::events::BytesText<'_>) -> Result<()> {
        match self.state.last_mut() {
            Some(State::PropertyElt { object, .. }) => {
                let t = e.unescape().map_err(|e| anyhow::anyhow!("{e}"))?.into_owned();
                *object = Some(ObjSlot::Text(t));
                Ok(())
            }
            Some(State::XmlLiteral { writer, .. }) => {
                writer
                    .write_event(Event::Text(e.clone()))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn end(&mut self, end: &quick_xml::events::BytesEnd<'_>) -> Result<()> {
        if self.literal_depth > 0 {
            if let Some(State::XmlLiteral { writer, .. }) = self.state.last_mut() {
                writer
                    .write_event(Event::End(end.clone()))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            self.literal_depth -= 1;
            self.depth -= 1;
            return Ok(());
        }
        if let Some(state) = self.state.pop() {
            self.end_state(state);
        }
        self.base.pop();
        self.lang.pop();
        self.depth -= 1;
        if self.depth == 1 {
            self.flush();
        }
        Ok(())
    }

    fn end_state(&mut self, state: State) {
        match state {
            State::PropertyElt { iri, subject, object, id_attr, datatype, language } => {
                let obj = match object {
                    Some(ObjSlot::Node(n)) => Obj::Node(n),
                    Some(ObjSlot::Text(t)) => literal(t, datatype, language),
                    None => literal(String::new(), datatype, language),
                };
                if let Some(id) = &id_attr {
                    let id = id.clone();
                    self.reify(&id, &subject, &iri, &obj);
                }
                self.emit(subject, iri, obj);
            }
            State::Collection { iri, subject, objects, id_attr } => {
                // The list is built from the tail back, so the cell nearest
                // `rdf:nil` is allocated first.
                let mut current = Node::Iri(RDF_NIL.to_string());
                for object in objects.iter().rev() {
                    let cell = Node::Blank(self.bnodes.next());
                    self.emit(cell.clone(), RDF_FIRST.into(), Obj::Node(object.clone()));
                    self.emit(cell.clone(), RDF_REST.into(), Obj::Node(current));
                    current = cell;
                }
                let obj = Obj::Node(current);
                if let Some(id) = &id_attr {
                    let id = id.clone();
                    self.reify(&id, &subject, &iri, &obj);
                }
                self.emit(subject, iri, obj);
            }
            State::XmlLiteral { iri, subject, writer, id_attr, emit } => {
                if emit {
                    let obj = Obj::Literal {
                        value: String::from_utf8_lossy(&writer.into_inner()).into_owned(),
                        datatype: Some(RDF_XML_LITERAL.to_string()),
                        language: None,
                    };
                    if let Some(id) = &id_attr {
                        let id = id.clone();
                        self.reify(&id, &subject, &iri, &obj);
                    }
                    self.emit(subject, iri, obj);
                }
            }
            State::NodeElt { subject, .. } => match self.state.last_mut() {
                Some(State::PropertyElt { object, .. }) => {
                    *object = Some(ObjSlot::Node(subject))
                }
                Some(State::Collection { objects, .. }) => objects.push(subject),
                _ => {}
            },
            State::Doc | State::Rdf => {}
        }
    }
}

enum Production {
    Rdf,
    NodeElt,
    PropertyElt(Node),
}

/// A literal's three shapes: typed, language-tagged, or plain.
fn literal(value: String, datatype: Option<String>, language: Option<String>) -> Obj {
    if datatype.is_some() {
        Obj::Literal { value, datatype, language: None }
    } else if language.is_some() {
        Obj::Literal { value, datatype: None, language }
    } else {
        Obj::Literal { value, datatype: None, language: None }
    }
}

/// Resolve an IRI reference against `xml:base`. Absolute references pass
/// through; a fragment or relative reference is joined to the base.
fn resolve(base: Option<&str>, iri: &str) -> String {
    let Some(base) = base else { return iri.to_string() };
    if iri.is_empty() {
        return base.to_string();
    }
    if iri.contains(':') && !iri.starts_with('#') && !iri.starts_with('/') {
        // Already absolute (a scheme before any path separator).
        let scheme_end = iri.find(':').unwrap();
        if !iri[..scheme_end].contains(['/', '#', '?']) {
            return iri.to_string();
        }
    }
    if let Some(frag) = iri.strip_prefix('#') {
        let stem = base.split('#').next().unwrap_or(base);
        return format!("{stem}#{frag}");
    }
    let stem = base.split(['#', '?']).next().unwrap_or(base);
    if let Some(rest) = iri.strip_prefix('/') {
        // Root-relative: keep the authority.
        if let Some(pos) = stem.find("//").map(|i| i + 2) {
            if let Some(slash) = stem[pos..].find('/') {
                return format!("{}/{rest}", &stem[..pos + slash]);
            }
        }
        return format!("{stem}/{rest}");
    }
    match stem.rfind('/') {
        Some(i) => format!("{}/{iri}", &stem[..i]),
        None => iri.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONTOLOGY: &str = r#"<?xml version="1.0"?>
<rdf:RDF
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:foaf="http://xmlns.com/foaf/0.1/"
     xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#"
     xmlns:owl="http://www.w3.org/2002/07/owl#"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
  >
  <owl:Ontology rdf:about="http://purl.obolibrary.org/obo/cl/cl-plus-zfa.owl">
    <foaf:homepage rdf:datatype="http://www.w3.org/2001/XMLSchema#anyURI">http://purl.obolibrary.org/obo/uberon/bridge/collected-zebrafish</foaf:homepage>
    <rdfs:seeAlso rdf:datatype="http://www.w3.org/2001/XMLSchema#anyURI">http://genomebiology.com/2012/13/1/R5</rdfs:seeAlso>
    <dc:title xml:lang="en">CL importer for CL+ZFA</dc:title>
    <owl:imports rdf:resource="http://purl.obolibrary.org/obo/cl.owl"/>
  </owl:Ontology>
</rdf:RDF>
"#;

    /// A stanza is written innermost-first: the triples are collected as the
    /// element is read and flushed in reverse when it closes, so the element's
    /// own `rdf:type` — asserted first — comes out last. Every IRI is a CURIE,
    /// and a typed literal keeps its datatype while a tagged one keeps its
    /// language.
    #[test]
    fn a_stanza_comes_out_in_reverse_and_shortened() {
        let rows = parse_str(ONTOLOGY, &crate::semsql::prefix_rows()).unwrap();
        let got: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r.stanza.as_str(),
                    r.predicate.as_str(),
                    r.object.as_deref().or(r.value.as_deref()).unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("obo:cl/cl-plus-zfa.owl", "owl:imports", "obo:cl.owl"),
                ("obo:cl/cl-plus-zfa.owl", "dce:title", "CL importer for CL+ZFA"),
                (
                    "obo:cl/cl-plus-zfa.owl",
                    "rdfs:seeAlso",
                    "http://genomebiology.com/2012/13/1/R5"
                ),
                (
                    "obo:cl/cl-plus-zfa.owl",
                    "foaf:homepage",
                    "http://purl.obolibrary.org/obo/uberon/bridge/collected-zebrafish"
                ),
                ("obo:cl/cl-plus-zfa.owl", "rdf:type", "owl:Ontology"),
            ]
        );
        assert_eq!(rows[1].language.as_deref(), Some("en"));
        assert_eq!(rows[2].datatype.as_deref(), Some("xsd:anyURI"));
        assert!(rows.iter().all(|r| r.subject == "obo:cl/cl-plus-zfa.owl"));
    }

    /// An anonymous class expression is numbered by its position in the
    /// document, and every triple it produces belongs to the term's stanza —
    /// which is what makes a term's whole description one indexed lookup.
    #[test]
    fn a_class_expression_stays_in_its_terms_stanza() {
        let doc = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:owl="http://www.w3.org/2002/07/owl#"
         xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#">
  <owl:Class rdf:about="http://purl.obolibrary.org/obo/CL_0000000">
    <rdfs:subClassOf>
      <owl:Restriction>
        <owl:onProperty rdf:resource="http://purl.obolibrary.org/obo/BFO_0000050"/>
        <owl:someValuesFrom rdf:resource="http://purl.obolibrary.org/obo/UBERON_0000061"/>
      </owl:Restriction>
    </rdfs:subClassOf>
  </owl:Class>
</rdf:RDF>
"#;
        let rows = parse_str(doc, &crate::semsql::prefix_rows()).unwrap();
        assert!(rows.iter().all(|r| r.stanza == "CL:0000000"), "{rows:#?}");
        let expr = "_:riog00000002";
        assert_eq!(
            rows.iter()
                .map(|r| (r.subject.as_str(), r.predicate.as_str(), r.object.as_deref().unwrap_or("")))
                .collect::<Vec<_>>(),
            vec![
                ("CL:0000000", "rdfs:subClassOf", expr),
                (expr, "owl:someValuesFrom", "UBERON:0000061"),
                (expr, "owl:onProperty", "BFO:0000050"),
                (expr, "rdf:type", "owl:Restriction"),
                ("CL:0000000", "rdf:type", "owl:Class"),
            ]
        );
    }
}
