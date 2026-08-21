//! SPARQL querying over an ontology, backed by the pure-Rust oxigraph engine.
//!
//! The ontology is serialized to RDF/XML and loaded into an in-memory store,
//! then queries are evaluated against it. This powers `query`, `verify` and
//! `report`.

use anyhow::{anyhow, bail, Result};
use oxigraph::io::RdfParser;
use oxigraph::model::Term;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

use crate::io::Format;
use crate::model::Model;

/// An in-memory RDF store loaded from an ontology.

/// The Jena node hash of a literal: `label.hashCode() * 31`, where the label
/// hashes its PARSED value, not its lexical form.
///
/// A language tag, `xsd:string` and `xsd:anyURI` all keep the lexical string; a
/// boolean is `Boolean.hashCode`; the integer family narrows to the smallest of
/// `Integer`/`Long` that holds it; `xsd:decimal` strips trailing fractional zeros
/// and becomes an integer when nothing is left after the point. `None` for
/// anything else — a value whose hash cannot be reproduced leaves the cell alone.
fn literal_value_hash(value: &str, datatype: &str, has_lang: bool) -> Option<i32> {
    use crate::sparql::jena_order as jo;
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    if has_lang {
        return Some(jo::node_hash(value));
    }
    let local = datatype.strip_prefix(XSD)?;
    let int_hash = |v: i128| -> Option<i32> {
        // `suitableInteger`: an `Integer` when it fits strictly inside the int
        // range, else a `Long`.
        if v > i32::MIN as i128 && v < i32::MAX as i128 {
            Some(v as i32)
        } else {
            let l = i64::try_from(v).ok()?;
            Some(((l ^ ((l as u64) >> 32) as i64) & 0xffff_ffff) as i32)
        }
    };
    let h = match local {
        "string" | "anyURI" | "normalizedString" | "token" | "language" | "Name" | "NCName" => {
            return Some(jo::node_hash(value))
        }
        "boolean" => match value {
            "true" | "1" => 1231i32,
            "false" | "0" => 1237i32,
            _ => return None,
        },
        "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
        | "nonPositiveInteger" | "negativeInteger" | "positiveInteger" | "unsignedInt"
        | "unsignedLong" | "unsignedShort" | "unsignedByte" => {
            int_hash(value.trim().parse::<i128>().ok()?)?
        }
        "decimal" => {
            let t = value.trim();
            let (neg, body) = match t.strip_prefix('-') {
                Some(b) => (true, b),
                None => (false, t.strip_prefix('+').unwrap_or(t)),
            };
            let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
            let frac = frac_part.trim_end_matches('0');
            if frac.is_empty() {
                let v: i128 = format!("{int_part}").parse().ok()?;
                int_hash(if neg { -v } else { v })?
            } else {
                let unscaled: i128 = format!("{int_part}{frac}").parse().ok()?;
                let unscaled = if neg { -unscaled } else { unscaled };
                let scale = frac.len() as i32;
                // `BigDecimal.hashCode` is `31 * unscaledValue.hashCode() + scale`,
                // and a `BigInteger` that fits one magnitude word hashes as that
                // word, signed.
                let mag = unscaled.unsigned_abs();
                if mag > u32::MAX as u128 {
                    return None;
                }
                let mut ih = (mag as u32) as i32;
                if unscaled < 0 {
                    ih = ih.wrapping_neg();
                }
                ih.wrapping_mul(31).wrapping_add(scale)
            }
        }
        // A date-time parses to its FIELDS — year, month, day, hour, minute,
        // second, fractional part and its scale, and the zone marker — and hashes
        // over that array. Only the zoned-UTC form is reproduced; an offset zone
        // would have to be normalised into the fields first.
        "dateTime" => {
            let b = value.as_bytes();
            if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T'
                || b[13] != b':' || b[16] != b':' || b[19] != b'Z'
            {
                return None;
            }
            let num = |a: usize, z: usize| value[a..z].parse::<i32>().ok();
            let fields = [
                num(0, 4)?,
                num(5, 7)?,
                num(8, 10)?,
                num(11, 13)?,
                num(14, 16)?,
                num(17, 19)?,
                0,
                b'Z' as i32,
                0,
            ];
            let mut arr: i32 = 1;
            for f in fields {
                arr = arr.wrapping_mul(31).wrapping_add(f);
            }
            31i32.wrapping_add(arr)
        }
        _ => return None,
    };
    Some(h.wrapping_mul(31))
}

/// The subjects an RDF/XML document types with each `rdf:type` object, in
/// document order.
///
/// A typed node element (`<owl:Class rdf:about="…">`) states the type triple at
/// its own position; an anonymous one (`<owl:Class>` nested in a restriction)
/// states it too, and is recorded as `None` because its label is not
/// reproducible. Node elements sit at even nesting depth and property elements at
/// odd, which the writer's four-space indentation makes readable directly.
/// The value of `key="…"` on a start tag, with XML entities resolved.
fn tag_attr(tag: &str, key: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(rel) = tag[from..].find(key) {
        let at = from + rel;
        from = at + key.len();
        // The name must stand alone: `rdf:about` must not match `rdf:aboutEach`,
        // and it must start an attribute rather than end another one.
        let before_ok = at == 0 || tag[..at].ends_with([' ', '\t', '\n', '\r']);
        let rest = &tag[at + key.len()..];
        let rest = rest.trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let rest = rest[1..].trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let val = &rest[1..];
        let end = val.find(quote)?;
        return Some(unescape_xml(&val[..end]));
    }
    None
}

/// The five XML entities, which is all an IRI or a prefix declaration can carry.
fn unescape_xml(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Every triple with a named object, in document order, grouped by object.
///
/// Striping again: node elements sit at odd depth and property elements at even,
/// so a property element's subject is the node element one level above it. That
/// enclosing node is tracked per level — a restriction nested inside a class
/// introduces its own blank subject, and the class's own properties resume under
/// the class once it closes.
fn scan_object_order(rdf: &[u8]) -> ObjectOrder {
    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    enum Frame {
        Node(u32),
        Prop(u32),
    }
    let text = String::from_utf8_lossy(rdf);
    let mut out = ObjectOrder::default();
    let type_id = out.intern(RDF_TYPE);
    let mut ns: std::collections::HashMap<String, String> = Default::default();
    let mut stack: Vec<Frame> = Vec::new();
    let mut blank_no = 0usize;
    let expand = |ns: &std::collections::HashMap<String, String>, qname: &str| -> Option<String> {
        let (pre, local) = qname.split_once(':')?;
        ns.get(pre).map(|base| format!("{base}{local}"))
    };
    let mut i = 0usize;
    while let Some(rel) = text[i..].find('<') {
        let open = i + rel;
        let Some(rel_end) = text[open..].find('>') else { break };
        let close = open + rel_end;
        let tag = &text[open + 1..close];
        i = close + 1;
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if tag.starts_with('/') {
            stack.pop();
            continue;
        }
        let empty = tag.ends_with('/');
        let name = tag.split([' ', '\t', '\n', '\r', '/']).next().unwrap_or("");
        let element_depth = stack.len();
        if element_depth == 0 {
            for part in tag.split_whitespace() {
                if let Some(rest) = part.strip_prefix("xmlns:") {
                    if let Some((pre, val)) = rest.split_once("=\"") {
                        if let Some(val) = val.split('"').next() {
                            ns.insert(pre.to_string(), unescape_xml(val));
                        }
                    }
                }
            }
            if !empty {
                // A placeholder, so the document's own children sit at odd depth.
                stack.push(Frame::Prop(type_id));
            }
            continue;
        }
        if element_depth % 2 == 1 {
            // A node element. Its `rdf:about` is its subject; without one it is a
            // blank node, which names no subject but still fills a slot.
            let subject = match tag_attr(tag, "rdf:about") {
                Some(iri) => out.intern(&iri),
                // A blank node: named for where the document introduces it, so the
                // same document always gives the same one the same name.
                None => {
                    let name = match tag_attr(tag, "rdf:nodeID") {
                        Some(id) => format!("_:{id}"),
                        None => {
                            blank_no += 1;
                            format!("_:n{blank_no}")
                        }
                    };
                    out.intern(&name)
                }
            };
            // The element's own name types it, unless it is `rdf:Description`.
            if name != "rdf:Description" {
                if let Some(t) = expand(&ns, name) {
                    let o = out.intern(&t);
                    out.push(o, subject, type_id);
                }
            }
            // Nested inside a property element, it is that property's object — but
            // only a NAMED object is indexed here.
            if !is_blank_name(out.name(subject)) && stack.len() >= 2 {
                if let (Frame::Prop(p), Frame::Node(s)) =
                    (&stack[stack.len() - 1], &stack[stack.len() - 2])
                {
                    out.push(subject, *s, *p);
                }
            }
            if !empty {
                stack.push(Frame::Node(subject));
            }
        } else {
            // A property element. `rdf:resource` is the whole triple; anything else
            // is a literal or a nested node, and a literal has no named object.
            let pred = match expand(&ns, name) {
                Some(p) => out.intern(&p),
                None => out.intern(name),
            };
            if let Some(r) = tag_attr(tag, "rdf:resource") {
                if let Some(Frame::Node(s)) = stack.last() {
                    let o = out.intern(&r);
                    out.push(o, *s, pred);
                }
            }
            if !empty {
                stack.push(Frame::Prop(pred));
            }
        }
    }
    out
}

fn scan_type_order(rdf: &[u8]) -> std::collections::HashMap<String, Vec<Option<String>>> {
    let text = String::from_utf8_lossy(rdf);
    let mut out: std::collections::HashMap<String, Vec<Option<String>>> =
        std::collections::HashMap::new();
    let mut ns: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // Striping: the document element is depth 0, node elements sit at odd depth and
    // property elements at even, so a start tag's own depth says which it is.
    let mut depth = 0usize;
    let mut subject: Option<String> = None;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = text[i..].find('<') {
        let open = i + rel;
        let Some(rel_end) = text[open..].find('>') else { break };
        let close = open + rel_end;
        let tag = &text[open + 1..close];
        i = close + 1;
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            let _ = name;
            depth = depth.saturating_sub(1);
            continue;
        }
        let empty = tag.ends_with('/');
        let name = tag.split([' ', '\t', '\n', '\r', '/']).next().unwrap_or("");
        let element_depth = depth;
        if !empty {
            depth += 1;
        }
        if element_depth == 0 {
            for part in tag.split_whitespace() {
                if let Some(rest) = part.strip_prefix("xmlns:") {
                    if let Some((pre, val)) = rest.split_once("=\"") {
                        if let Some(val) = val.split('"').next() {
                            ns.insert(pre.to_string(), val.to_string());
                        }
                    }
                }
            }
            continue;
        }
        if element_depth % 2 == 1 {
            // A node element. Its own name is its type, unless it is the untyped
            // `rdf:Description`.
            subject = attr_value(tag, "rdf:about=\"");
            if name == "rdf:Description" {
                continue;
            }
            let Some((pre, local)) = name.split_once(':') else { continue };
            let Some(base) = ns.get(pre) else { continue };
            out.entry(format!("{base}{local}")).or_default().push(subject.clone());
        } else if name == "rdf:type" {
            if let Some(t) = attr_value(tag, "rdf:resource=\"") {
                out.entry(t).or_default().push(subject.clone());
            }
        }
    }
    let _ = bytes;
    out
}

/// The value of an attribute in a start tag, with XML entities resolved.
fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let at = tag.find(attr)? + attr.len();
    let end = tag[at..].find('"')?;
    Some(
        tag[at..at + end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

pub struct Queryable {
    store: Store,
    /// Per `rdf:type` object IRI, the subjects the source document typed with it,
    /// in the order the document states them — `None` for a blank node, whose
    /// label is not reproducible but which still fills a slot.
    ///
    /// A query whose driving pattern is `?x a <T>` reads that type's triples out of
    /// the store in one bunch, and the bunch's layout follows the order they went
    /// in. Nothing else recovers that order: the store answers by value, not by
    /// position.
    type_order: std::collections::HashMap<String, Vec<Option<String>>>,
    /// Every triple with a NAMED object, grouped by that object, in the order the
    /// document states them — what a pattern with a bound object is answered from.
    object_order: ObjectOrder,
}

/// Whether an interned name stands for a blank node rather than an IRI.
fn is_blank_name(name: &str) -> bool {
    name.starts_with("_:")
}

/// The document's triples that have a NAMED object, grouped by that object and
/// kept in the order the document states them.
///
/// The graph answers `(?, p, o)` from the object index: every triple with `o` as
/// its object sits in one bunch, and the bunch is read back in slot order, which
/// follows the order the triples went in. The store answers by value and cannot
/// recover that order, so it is recorded here at load.
///
/// Nodes are interned. A blank-node subject keeps its own identity here — two
/// axioms annotating the same thing state two different triples — but its label is
/// minted fresh by the parser and is not reproducible, so it fills a slot without
/// naming a subject.
#[derive(Default)]
struct ObjectOrder {
    names: Vec<String>,
    ids: std::collections::HashMap<String, u32>,
    /// object id -> (subject id, predicate id), in document order
    bunches: std::collections::HashMap<u32, Vec<(u32, u32)>>,
}

impl ObjectOrder {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(id) = self.ids.get(s) {
            return *id;
        }
        let id = self.names.len() as u32;
        self.names.push(s.to_string());
        self.ids.insert(s.to_string(), id);
        id
    }

    fn name(&self, id: u32) -> &str {
        &self.names[id as usize]
    }

    fn push(&mut self, object: u32, subject: u32, predicate: u32) {
        self.bunches.entry(object).or_default().push((subject, predicate));
    }

    fn bunch(&self, object: &str) -> Option<&[(u32, u32)]> {
        let id = self.ids.get(object)?;
        self.bunches.get(id).map(|v| v.as_slice())
    }
}

impl Queryable {
    /// Build a queryable store from a model by serializing to RDF/XML and
    /// loading it.
    ///
    /// The RDF/XML writer materialises a declaration triple for an entity the
    /// ontology only REFERENCES — an undeclared property used in a restriction, or
    /// named as a property-chain member. That is right for a self-contained
    /// ontology, but wrong once the ontology has IMPORTS: the entity's type is
    /// declared in the import closure, so the root graph must carry no such
    /// triple of its own.
    ///
    /// That difference is not academic. MONDO's `tmp/simple_seed.txt` is built by
    /// `query --query simple-seed.sparql -i reasoned.owl`, whose first clauses are
    /// `?cls a owl:AnnotationProperty` / `owl:ObjectProperty`. `reasoned.owl` keeps
    /// its four imports and mentions `BFO_0000050`/`BFO_0000051` only as
    /// `owl:propertyChainAxiom` members, so a synthesised type triple would put two
    /// object properties into the seed that the ontology does not declare. A seed
    /// carrying them then keeps axioms `filter` should drop, all the way into
    /// `mondo-simple.owl`.
    pub fn from_model(model: &Model) -> Result<Queryable> {
        let mut rdf = Vec::new();
        crate::io::write_to_ref(model, &mut rdf, Format::RdfXml)?;
        let store = Store::new().map_err(|e| anyhow!("store init: {e}"))?;
        // The parser names an unlabelled blank node for itself, and those names are
        // drawn fresh every run — so a query that returns one would answer
        // differently each time it ran. Number them by the order the document
        // introduces them instead: the same document always gets the same names.
        {
            use oxigraph::model::{BlankNode, Quad, Term};
            let mut names: std::collections::HashMap<String, BlankNode> = Default::default();
            let mut next = 0usize;
            let mut rename = |b: &BlankNode| -> BlankNode {
                if let Some(n) = names.get(b.as_str()) {
                    return n.clone();
                }
                let n = BlankNode::new_unchecked(format!("b{next}"));
                next += 1;
                names.insert(b.as_str().to_string(), n.clone());
                n
            };
            for quad in RdfParser::from_format(oxigraph::io::RdfFormat::RdfXml).for_slice(&rdf) {
                let q = quad.map_err(|e| anyhow!("loading ontology triples: {e}"))?;
                let subject = match q.subject {
                    oxigraph::model::NamedOrBlankNode::BlankNode(b) => rename(&b).into(),
                    s => s,
                };
                let object = match q.object {
                    Term::BlankNode(b) => Term::BlankNode(rename(&b)),
                    o => o,
                };
                store
                    .insert(&Quad::new(subject, q.predicate, object, q.graph_name))
                    .map_err(|e| anyhow!("loading ontology triples: {e}"))?;
            }
        }
        let q = Queryable {
            store,
            type_order: scan_type_order(&rdf),
            object_order: scan_object_order(&rdf),
        };
        q.drop_synthesised_types(model)?;
        Ok(q)
    }

    /// Remove the writer's synthesised `rdf:type` triples for entities the model
    /// does not itself declare, when the ontology has imports — see `from_model`.
    fn drop_synthesised_types(&self, model: &Model) -> Result<()> {
        use horned_owl::model::Component;
        use std::collections::HashSet;
        if !model.ont.iter().any(|ac| matches!(ac.component, Component::Import(_))) {
            return Ok(());
        }
        // EVERY entity the model does not declare itself, not just the ones whose
        // sole appearance is a property chain. Whether an entity is missing a type
        // is a question about the whole IMPORT CLOSURE — so for an ontology with
        // imports nothing referenced from the closure gets a type triple, however
        // it is referenced.
        //
        // The criterion is DECLAREDNESS, not where the entity appears: the subject
        // of a synthesised type triple that the model carries no `Declare*` for is
        // dropped however else it occurs, including as the subject of axioms of
        // its own. Beyond the property-chain pair that catches 14 in MONDO's
        // `reasoned.owl` — `IAO_0000231`, `IAO_0000233`, `IAO_0000589`,
        // `IAO_0000700`, `IAO_0006012`, four `RO_*`, three `dc:*`,
        // `dcterms:license`, `foaf:homepage` — entities used there only as a
        // predicate, with their declarations in the closure. A seed carrying them
        // keeps annotation assertions `filter` must drop, and `mondo-simple.owl`
        // differs by exactly those.
        let mut declared: HashSet<String> = HashSet::new();
        for ac in model.ont.iter() {
            let iri = match &ac.component {
                Component::DeclareClass(d) => d.0 .0.as_ref().to_string(),
                Component::DeclareObjectProperty(d) => d.0 .0.as_ref().to_string(),
                Component::DeclareDataProperty(d) => d.0 .0.as_ref().to_string(),
                Component::DeclareAnnotationProperty(d) => d.0 .0.as_ref().to_string(),
                Component::DeclareNamedIndividual(d) => d.0 .0.as_ref().to_string(),
                Component::DeclareDatatype(d) => d.0 .0.as_ref().to_string(),
                _ => continue,
            };
            declared.insert(iri);
        }
        const TYPES: [&str; 6] = [
            "http://www.w3.org/2002/07/owl#Class",
            "http://www.w3.org/2002/07/owl#ObjectProperty",
            "http://www.w3.org/2002/07/owl#DatatypeProperty",
            "http://www.w3.org/2002/07/owl#AnnotationProperty",
            "http://www.w3.org/2002/07/owl#NamedIndividual",
            "http://www.w3.org/2000/01/rdf-schema#Datatype",
        ];
        let mut doomed = Vec::new();
        for t in TYPES {
            let obj = oxigraph::model::NamedNode::new(t)
                .map_err(|e| anyhow!("bad type IRI {t}: {e}"))?;
            for q in self.store.quads_for_pattern(
                None,
                Some(oxigraph::model::vocab::rdf::TYPE),
                Some((&obj).into()),
                None,
            ) {
                let q = q.map_err(|e| anyhow!("scanning type triples: {e}"))?;
                if let oxigraph::model::Subject::NamedNode(n) = &q.subject {
                    if !declared.contains(n.as_str()) {
                        doomed.push(q);
                    }
                }
            }
        }
        for q in doomed {
            self.store.remove(&q).map_err(|e| anyhow!("removing type triple: {e}"))?;
        }
        Ok(())
    }

    /// Execute a SPARQL query, returning a tabular rendering. ASK returns one
    /// row with `true`/`false`; SELECT returns the projected bindings; CONSTRUCT
    /// returns the triples as N-Triples.
    pub fn query_table(&self, sparql: &str) -> Result<QueryTable> {
        let results = SparqlEvaluator::new()
            .parse_query(&prepare(sparql)?)
            .map_err(|e| anyhow!("SPARQL parse error: {e}"))?
            .on_store(&self.store)
            .execute()
            .map_err(|e| anyhow!("SPARQL error: {e}"))?;
        match results {
            QueryResults::Boolean(b) => Ok(QueryTable {
                columns: vec!["result".to_string()],
                rows: vec![vec![b.to_string()]],
                tsv_rows: Vec::new(),
                select: false,
            }),
            QueryResults::Solutions(solutions) => {
                let columns: Vec<String> = solutions
                    .variables()
                    .iter()
                    .map(|v| v.as_str().to_string())
                    .collect();
                let mut rows = Vec::new();
                let mut tsv_rows = Vec::new();
                for sol in solutions {
                    let sol = sol.map_err(|e| anyhow!("solution error: {e}"))?;
                    let row = columns
                        .iter()
                        .map(|c| {
                            sol.get(c.as_str())
                                .map(term_to_string)
                                .unwrap_or_default()
                        })
                        .collect();
                    let tsv = columns
                        .iter()
                        .map(|c| sol.get(c.as_str()).map(term_to_tsv).unwrap_or_default())
                        .collect();
                    rows.push(row);
                    tsv_rows.push(tsv);
                }
                Ok(QueryTable { columns, rows, tsv_rows, select: true })
            }
            QueryResults::Graph(triples) => {
                let mut rows = Vec::new();
                for t in triples {
                    let t = t.map_err(|e| anyhow!("triple error: {e}"))?;
                    rows.push(vec![
                        term_to_string(&t.subject.into()),
                        t.predicate.to_string(),
                        term_to_string(&t.object),
                    ]);
                }
                Ok(QueryTable {
                    columns: vec!["subject".into(), "predicate".into(), "object".into()],
                    rows,
                    tsv_rows: Vec::new(),
                    select: false,
                })
            }
        }
    }

    /// Number of solution rows a query returns (for `verify`-style checks).
    pub fn count(&self, sparql: &str) -> Result<usize> {
        Ok(self.query_table(sparql)?.rows.len())
    }

    /// How many quads in the store have `subject` as their subject — the count
    /// that fixes the capacity, and so the slot order, [`jena_order`] computes for
    /// that subject's multi-value cells.
    pub fn subject_triple_count(&self, subject: &str) -> usize {
        let Ok(n) = oxigraph::model::NamedNodeRef::new(subject) else { return 0 };
        self.store
            .quads_for_pattern(Some(n.into()), None, None, None)
            .filter(|q| q.is_ok())
            .count()
    }

    /// How many quads in the store have `object` as their object — the count that
    /// fixes the capacity of the bunch a `?v <p> <o>` pattern scans, and so the
    /// order it hands its subjects back in.
    pub fn object_triple_count(&self, object: &str) -> usize {
        let Ok(n) = oxigraph::model::NamedNodeRef::new(object) else { return 0 };
        self.store
            .quads_for_pattern(None, None, Some(n.into()), None)
            .filter(|q| q.is_ok())
            .count()
    }

    /// Every triple with `subject`, in the order the document writes them, as
    /// (predicate IRI, object lexical form, object node hash) — the whole bunch the
    /// store keeps for that subject, which is what decides the slot order of any
    /// one of its triples.
    ///
    /// The order is the entity's own render order: its type triple, then its named
    /// superclasses, then its annotations by property and value. `None` when an
    /// object's hash is not computable — a blank node, whose label is minted per
    /// run, or a typed literal whose hash keys on the parsed value rather than the
    /// lexical form.
    /// The subjects the source document typed with `type_iri`, in document order.
    /// See [`Queryable::type_order`].
    pub fn typed_in_order(&self, type_iri: &str) -> Option<&[Option<String>]> {
        self.type_order.get(type_iri).map(|v| v.as_slice())
    }

    /// The subjects of `(?, predicate, object)`, in the order the graph answers
    /// that pattern. A triple whose subject is a blank node has no reproducible
    /// hash: it holds its place in the bunch but names no subject.
    pub fn object_subjects(&self, object: &str, predicate: &str) -> Option<Vec<String>> {
        use crate::sparql::jena_order as jo;
        let recorded = self.object_order.bunch(object)?;
        // The graph holds each triple once. A document that states the same triple
        // twice adds it once, so the bunch is one shorter for it — which is the
        // difference between a flat array and a hash table at the boundary, and a
        // different order either side of it.
        let mut seen: std::collections::HashSet<(u32, u32)> = Default::default();
        let mut bunch: Vec<(u32, u32)> = Vec::with_capacity(recorded.len());
        for t in recorded {
            if seen.insert(*t) {
                bunch.push(*t);
            }
        }
        if std::env::var("OM_BUNCH_DEBUG").ok().as_deref() == Some(object) {
            eprintln!(
                "[bunch] {object}: {} triples ({} stated), in the order recorded",
                bunch.len(),
                recorded.len()
            );
            for (k, (s, p)) in bunch.iter().enumerate() {
                eprintln!(
                    "  {k}\t{}\t{}",
                    self.object_order.name(*s),
                    self.object_order.name(*p)
                );
            }
        }
        let oh = jo::node_hash(object);
        let hashes: Vec<Option<i32>> = bunch
            .iter()
            .map(|(s, p)| {
                let sn = self.object_order.name(*s);
                (!is_blank_name(sn)).then(|| {
                    jo::triple_hash(jo::node_hash(sn), jo::node_hash(self.object_order.name(*p)), oh)
                })
            })
            .collect();
        Some(
            jo::bunch_order(&hashes)
                .into_iter()
                .filter(|&i| self.object_order.name(bunch[i].1) == predicate)
                .map(|i| self.object_order.name(bunch[i].0).to_string())
                .collect(),
        )
    }

    /// The nodes an arbitrary-length path `?v <pred>* <root>` binds, in the order
    /// it binds them.
    ///
    /// The path is walked backwards from `root`: a node is emitted when it is
    /// first reached, and each of its own reachers is then walked in turn, depth
    /// first — so a node always precedes everything that only it reaches.
    pub fn path_order(&self, root: &str, predicate: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = Default::default();
        let mut stack = vec![root.to_string()];
        while let Some(node) = stack.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            out.push(node.clone());
            if let Some(reachers) = self.object_subjects(&node, predicate) {
                for r in reachers.into_iter().rev() {
                    stack.push(r);
                }
            }
        }
        out
    }

    pub fn subject_bunch(&self, subject: &str) -> Option<Vec<(String, String, Option<i32>)>> {
        use crate::sparql::jena_order as jo;
        const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
        const SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
        const SUBPROP: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
        const EQUIV: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
        const DISJOINT: &str = "http://www.w3.org/2002/07/owl#disjointWith";
        let n = oxigraph::model::NamedNodeRef::new(subject).ok()?;
        let mut out: Vec<(u8, String, bool, String, Option<i32>)> = Vec::new();
        for q in self.store.quads_for_pattern(Some(n.into()), None, None, None) {
            let q = q.ok()?;
            let pred = q.predicate.as_str().to_string();
            // An object whose hash cannot be reproduced — an anonymous class
            // expression, a literal of a datatype whose value is not modelled —
            // still holds its place. It fills a slot, so it decides when the bunch
            // stops being a flat array and when the table grows, even though which
            // slot it takes is unknown. Anonymous objects follow the named ones
            // under the same property, as they are written.
            let (anon, lex, hash) = match &q.object {
                Term::NamedNode(o) => {
                    (false, o.as_str().to_string(), Some(jo::node_hash(o.as_str())))
                }
                Term::BlankNode(b) => (true, b.as_str().to_string(), None),
                Term::Literal(l) => {
                    let dt = l.datatype();
                    let vh = literal_value_hash(l.value(), dt.as_str(), l.language().is_some());
                    (false, l.value().to_string(), vh)
                }
                _ => return None,
            };
            // The entity's axioms are written in their own order — its declaration,
            // then its class axioms, then everything else — and only within the last
            // group does the property decide.
            let rank = match pred.as_str() {
                RDF_TYPE => 0u8,
                EQUIV => 1,
                SUBCLASS | SUBPROP => 2,
                DISJOINT => 3,
                _ => 4,
            };
            out.push((rank, pred, anon, lex, hash));
        }
        out.sort();
        Some(out.into_iter().map(|(_, p, _, l, h)| (p, l, h)).collect())
    }

    /// Dump the evaluation plan a query compiles to, when `OWLMAKE_EXPLAIN_SPARQL`
    /// is set. The plan decides whether an `OPTIONAL` is evaluated once per left
    /// solution with its variables bound, or once in full with them unbound — a
    /// difference of minutes against seconds on a large ontology, and not visible
    /// from the query text.
    pub fn explain_if_asked(&self, sparql: &str) {
        if std::env::var_os("OWLMAKE_EXPLAIN_SPARQL").is_none() {
            return;
        }
        let Ok(prepared) = prepare(sparql) else { return };
        let Ok(query) = SparqlEvaluator::new().parse_query(&prepared) else { return };
        let (results, explanation) = query.on_store(&self.store).explain();
        // The plan is only finalised once the solutions have been consumed, and a
        // query worth explaining is one too slow to run — so drop them unread and
        // report the plan the optimizer chose.
        drop(results);
        let mut buf = Vec::new();
        if explanation.write_in_json(&mut buf).is_ok() {
            status!("sparql plan: {}", String::from_utf8_lossy(&buf));
        }
    }

    /// Run a query of ANY form and return either a solution table or a serialized
    /// graph. `query --query <FILE> <OUT>` accepts every query form and writes
    /// whatever the form produces — a CONSTRUCT there yields RDF in `--format`, not
    /// a three-column table. MONDO's `mirror-hgnc` is
    /// `merge -i hgnc_gene.nt query --format ttl --query construct-hgnc.sparql
    /// mirror/hgnc.owl`; rendering that as a table would write a TSV with a
    /// `subject predicate object` header where the next step reads Turtle.
    pub fn run_query(
        &self,
        sparql: &str,
        rdf_fmt: oxigraph::io::RdfFormat,
    ) -> Result<QueryOutput> {
        self.explain_if_asked(sparql);
        let results = SparqlEvaluator::new()
            .parse_query(&prepare(sparql)?)
            .map_err(|e| anyhow!("SPARQL parse error: {e}"))?
            .on_store(&self.store)
            .execute()
            .map_err(|e| anyhow!("SPARQL error: {e}"))?;
        if let QueryResults::Graph(triples) = results {
            // A constructed graph takes its prefix map from the QUERY, and the
            // serializer writes that map into the file, so the prefixes survive
            // into whatever reads it back. uPheno's `components/upheno-bridge.owl`
            // is one of these graphs re-serialised, and `semapv:` reaches its
            // `Prefix(…)` block only because the construct's prologue declared it.
            let mut ser = oxigraph::io::RdfSerializer::from_format(rdf_fmt);
            for (name, iri) in query_prefixes(sparql) {
                ser = ser
                    .with_prefix(&name, &iri)
                    .map_err(|e| anyhow!("PREFIX {name}: <{iri}>: {e}"))?;
            }
            let mut serializer = ser.for_writer(Vec::new());
            for t in triples {
                let t = t.map_err(|e| anyhow!("triple error: {e}"))?;
                serializer.serialize_triple(&t).map_err(|e| anyhow!("serializing: {e}"))?;
            }
            return Ok(QueryOutput::Graph(
                serializer.finish().map_err(|e| anyhow!("finishing: {e}"))?,
            ));
        }
        // Not a graph query — re-run through the table path (cheap: the store is
        // in memory and the query has already been validated).
        Ok(QueryOutput::Table(self.query_table(sparql)?))
    }

    /// Run a SPARQL CONSTRUCT query and serialize the resulting graph as RDF in
    /// `fmt` (Turtle by default). Errors if the query is not a CONSTRUCT/DESCRIBE
    /// (i.e. does not yield a graph).
    pub fn construct(&self, sparql: &str, fmt: oxigraph::io::RdfFormat) -> Result<Vec<u8>> {
        let results = SparqlEvaluator::new()
            .parse_query(&prepare(sparql)?)
            .map_err(|e| anyhow!("SPARQL parse error: {e}"))?
            .on_store(&self.store)
            .execute()
            .map_err(|e| anyhow!("SPARQL error: {e}"))?;
        match results {
            QueryResults::Graph(triples) => {
                // A constructed graph takes its prefix map from the QUERY, and the
                // serializer writes that map into the file — so the prefixes
                // survive into whatever reads the file back. EFO's `gwas_trait.ru`
                // is where this shows: `components/gwas_import.owl` is that graph
                // re-serialised, and its `Prefix(…)` block is the query's prologue.
                let mut ser = oxigraph::io::RdfSerializer::from_format(fmt);
                for (name, iri) in query_prefixes(sparql) {
                    ser = ser
                        .with_prefix(&name, &iri)
                        .map_err(|e| anyhow!("PREFIX {name}: <{iri}>: {e}"))?;
                }
                let mut serializer = ser.for_writer(Vec::new());
                for t in triples {
                    let t = t.map_err(|e| anyhow!("triple error: {e}"))?;
                    serializer
                        .serialize_triple(&t)
                        .map_err(|e| anyhow!("serialize error: {e}"))?;
                }
                serializer.finish().map_err(|e| anyhow!("serialize finish: {e}"))
            }
            _ => bail!("CONSTRUCT query expected (the query does not produce a graph)"),
        }
    }
}

/// The `PREFIX name: <iri>` declarations of a query's prologue, in order.
///
/// Only the prologue — everything up to the first query form — so a `PREFIX`
/// inside a string literal in the WHERE clause cannot be mistaken for one.
pub(crate) fn query_prefixes(sparql: &str) -> Vec<(String, String)> {
    let head = {
        let upper = sparql.to_ascii_uppercase();
        ["CONSTRUCT", "SELECT", "ASK", "DESCRIBE", "INSERT", "DELETE"]
            .iter()
            .filter_map(|k| upper.find(k))
            .min()
            .map_or(sparql, |i| &sparql[..i])
    };
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?i)\bPREFIX\s+([A-Za-z_][A-Za-z0-9_.\-]*)?\s*:\s*<([^>]*)>").unwrap()
    });
    re.captures_iter(head)
        .map(|c| {
            (
                c.get(1).map_or(String::new(), |m| m.as_str().to_string()),
                c[2].to_string(),
            )
        })
        .collect()
}

fn term_to_string(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
        Term::Literal(l) => l.value().to_string(),
        #[allow(unreachable_patterns)]
        other => other.to_string(),
    }
}

/// SPARQL-TSV rendering of a term, as `query -f tsv` writes it: an IRI in angle
/// brackets, a blank node as `_:label`, a literal quoted with its language tag or
/// datatype. `-f csv` stays BARE — the CSV form is the lexical value only, which
/// is what MONDO's `tmp/simple_seed.txt` relies on.
///
/// A numeric or boolean literal drops the quotes and the `^^<datatype>` when its
/// lexical form is *already* a legal Turtle abbreviation. That is not cosmetic:
/// EFO's `reports/class-count-by-prefix.tsv` has `"MA"\t2`, not
/// `"MA"\t"2"^^<…#integer>`, and MONDO's
/// `report-reason-paper-ct-xrefs-by-source.tsv` likewise ends `"UMLS"\t16093`.
/// Every `COUNT()` column in the 7 EFO and 10 MONDO committed report TSVs churns
/// on this one rule. `xsd:anyURI` keeps its datatype (MONDO's
/// `report-obsoletioncandidates-withcomment.tsv` shows
/// `"https://…"^^<http://www.w3.org/2001/XMLSchema#anyURI>`), so the abbreviation
/// list is exactly: integer, decimal, double, boolean.
/// The body of a quoted literal in a solution table: a per-character escape, so
/// escaping a concatenation is the concatenation of the escapes.
pub(crate) fn tsv_escape(v: &str) -> String {
    v.chars()
        .map(|c| match c {
            '\\' => "\\\\".to_string(),
            '"' => "\\\"".to_string(),
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            c => c.to_string(),
        })
        .collect()
}

fn term_to_tsv(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => format!("<{}>", n.as_str()),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
        Term::Literal(l) => {
            if l.language().is_none() {
                if let Some(bare) = jena_bare_literal(l.value(), l.datatype().as_str()) {
                    return bare.to_string();
                }
            }
            let esc = tsv_escape(l.value());
            match l.language() {
                Some(lang) => format!("\"{esc}\"@{lang}"),
                None => {
                    let dt = l.datatype();
                    if dt.as_str() == "http://www.w3.org/2001/XMLSchema#string" {
                        format!("\"{esc}\"")
                    } else {
                        format!("\"{esc}\"^^<{}>", dt.as_str())
                    }
                }
            }
        }
        #[allow(unreachable_patterns)]
        other => other.to_string(),
    }
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// The lexical form, if this typed literal may be written without quotes or a
/// datatype — the four special cases. Each one only applies when the lexical form
/// round-trips as the Turtle abbreviation for its datatype, so a non-canonical
/// `"1e0"^^xsd:integer` keeps its full form.
fn jena_bare_literal<'a>(lex: &'a str, datatype: &str) -> Option<&'a str> {
    let Some(local) = datatype.strip_prefix(XSD) else { return None };
    match local {
        // An optional sign then at least one digit. The derived integer types
        // (int, long, nonNegativeInteger, …) share that lexical space and
        // abbreviate the same way, so a `COUNT()` typed as any of them comes out
        // bare.
        "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
        | "nonPositiveInteger" | "negativeInteger" | "positiveInteger" | "unsignedInt"
        | "unsignedLong" | "unsignedShort" | "unsignedByte" => {
            let digits = lex.strip_prefix(['+', '-']).unwrap_or(lex);
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(lex)
        }
        // `xsd:decimal` abbreviates only WITH a '.' — bare `2` would read back as
        // an integer, so the dot has to be there before the datatype is dropped.
        "decimal" => {
            let body = lex.strip_prefix(['+', '-']).unwrap_or(lex);
            let Some((int, frac)) = body.split_once('.') else { return None };
            let ok = !(int.is_empty() && frac.is_empty())
                && int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit());
            ok.then_some(lex)
        }
        // …and `xsd:double` only WITH an exponent, for the same reason.
        "double" => {
            (lex.contains(['e', 'E']) && lex.parse::<f64>().is_ok()).then_some(lex)
        }
        "boolean" => matches!(lex, "true" | "false").then_some(lex),
        _ => None,
    }
}


/// The order a multi-value report cell emits its values in.
///
/// A `GROUP_CONCAT` cell is ordered neither by document position nor by sort, and
/// oxigraph accumulates the aggregate in an order of its own. The order committed
/// report TSVs carry is the one a hash table holding a subject's triples yields
/// when read out slot by slot, so this module computes that order arithmetically
/// and the `query` command permutes the finished cell into it. Emitting it keeps a
/// report diff to real content changes instead of a reshuffle of every
/// multi-value cell.
///
/// The subject's triple count fixes a capacity, and a value lands in slot
/// `(triple_hash * 127 & 0x7fffffff) % capacity`, where
/// `triple_hash = (s >> 1) ^ p ^ (o << 1)` over `node_hash = label_hash * 31` and
/// `label_hash` is the `h = h * 31 + byte` string hash — arithmetic shifts and
/// i32 wrapping throughout. Capacity starts at `next_size(9 / 0.5)` = 19 with
/// `threshold = capacity / 2` and moves to `next_size(capacity * 2)` each time the
/// size exceeds it — 19, 79, 307, 617 …
///
/// Every constant here — the prime table, the `* 127`, the `* 31`, the i32
/// wrapping — is pinned by the order the released files already have; none of
/// them is a tunable. Under nine triples there is no capacity at all:
/// `capacity_for` returns `None` and the cell keeps the order the aggregate
/// produced.
pub mod jena_order {
    /// The string hash the ordering is built on. The same one an axiom hash uses:
    /// `h = h * 31 + unit`, i32 wrapping, over UTF-16 code units.
    pub use crate::owlapi_hash::java_string_hash;

    /// The hash of a node whose label is `label`: the string hash times 31.
    pub fn node_hash(label: &str) -> i32 {
        java_string_hash(label).wrapping_mul(31)
    }

    /// The hash of a triple, from its three node hashes.
    pub fn triple_hash(s: i32, p: i32, o: i32) -> i32 {
        (s >> 1) ^ p ^ (o.wrapping_shl(1))
    }

    /// The first capacity in the prime table strictly greater than `at_least`.
    pub fn next_size(at_least: i32) -> i32 {
        const PRIMES: &[i32] = &[
            7, 19, 37, 79, 149, 307, 617, 1237, 2477, 4957, 9923, 19_853, 39_709, 79_423,
            158_849, 317_701, 635_413, 1_270_849, 2_541_701, 5_083_423,
        ];
        for p in PRIMES {
            if *p > at_least {
                return *p;
            }
        }
        *PRIMES.last().unwrap()
    }

    /// The capacity `size` triples end up under, replaying the growth schedule.
    ///
    /// `None` at nine or fewer: a bunch that small is a flat ARRAY, never a hash
    /// table — the table is built when a tenth triple arrives, out of the nine
    /// already there — so there is no capacity to place values in and the order is
    /// the array's, which is the reverse of the order the triples were added.
    pub fn capacity_for(size: usize) -> Option<i32> {
        if size < 10 {
            return None;
        }
        let mut cap = next_size((9.0 / 0.5) as i32);
        let mut threshold = (cap as f64 * 0.5) as usize;
        let mut n = 9usize;
        while n < size {
            n += 1;
            if n > threshold {
                cap = next_size(cap.saturating_mul(2));
                threshold = (cap as f64 * 0.5) as usize;
            }
        }
        Some(cap)
    }

    /// The order a subject's (or object's) triples are read back in, given their
    /// hashes in the order they were ADDED.
    ///
    /// Under ten triples the bunch is a flat array, read back last-added-first.
    /// From ten it is a hash table: the array is drained into a table sized for
    /// nine, in that same last-added-first order, the tenth triple then tips it
    /// over its threshold, and each growth REHASHES by walking the old table's
    /// slots in ascending order. Reading is slot by slot, ascending.
    ///
    /// Returned as indices into `hashes`, in iteration order.
    pub fn bunch_order(hashes: &[Option<i32>]) -> Vec<usize> {
        let n = hashes.len();
        if n < 10 {
            return (0..n).rev().filter(|i| hashes[*i].is_some()).collect();
        }
        let mut cap = next_size((9.0 / 0.5) as i32);
        let mut threshold = (cap as f64 * 0.5) as usize;
        let mut keys: Vec<Option<usize>> = vec![None; cap as usize];
        let mut size = 0usize;

        fn find_slot(keys: &[Option<usize>], cap: i32, h: i32) -> usize {
            let mut i = (h.wrapping_mul(127) & 0x7fff_ffff) % cap;
            while keys[i as usize].is_some() {
                i -= 1;
                if i < 0 {
                    i += cap;
                }
            }
            i as usize
        }

        let mut add = |keys: &mut Vec<Option<usize>>,
                       cap: &mut i32,
                       threshold: &mut usize,
                       size: &mut usize,
                       idx: usize| {
            if let Some(h) = hashes[idx] {
                let slot = find_slot(keys, *cap, h);
                keys[slot] = Some(idx);
            }
            *size += 1;
            if *size > *threshold {
                let old = std::mem::take(keys);
                *cap = next_size(cap.saturating_mul(2));
                *threshold = (*cap as f64 * 0.5) as usize;
                *keys = vec![None; *cap as usize];
                for cell in old.into_iter().flatten() {
                    let Some(h) = hashes[cell] else { continue };
                    let s = find_slot(keys, *cap, h);
                    keys[s] = Some(cell);
                }
            }
        };

        // The flat array is drained last-added-first.
        for i in (0..9).rev() {
            add(&mut keys, &mut cap, &mut threshold, &mut size, i);
        }
        for i in 9..n {
            add(&mut keys, &mut cap, &mut threshold, &mut size, i);
        }
        keys.into_iter().flatten().collect()
    }

    /// The slot a triple occupies in a bunch of the given capacity.
    pub fn slot(s: i32, p: i32, o: i32, capacity: i32) -> i32 {
        slot_of_hash(s, p, o, capacity)
    }

    /// [`slot`] from node hashes that are already computed.
    pub fn slot_of_hash(s: i32, p: i32, o: i32, capacity: i32) -> i32 {
        let h = triple_hash(s, p, o).wrapping_mul(127);
        (h & 0x7fff_ffff) % capacity
    }

    /// The hash of a solution binding over `(variable name, node hash)` pairs —
    /// the key a GROUP BY groups on. Seeded, then XOR-folded, so the order the
    /// variables are visited in does not matter.
    pub fn binding_hash(entries: &[(&str, i32)]) -> i32 {
        let mut h: i32 = 0xC0;
        for (var, node) in entries {
            h ^= java_string_hash(var).wrapping_mul(31);
            h ^= *node;
        }
        h
    }

    /// The table `n` distinct keys end up in, for a map that starts out sized for
    /// eight and doubles whenever three quarters of its slots are spoken for.
    pub fn group_table_capacity(n: usize) -> usize {
        let mut cap = 16usize;
        while n > cap * 3 / 4 {
            cap *= 2;
        }
        cap
    }

    /// The slot a key of hash `h` occupies in a table of `capacity` slots: the
    /// high half of the hash folded into the low half, masked to the table.
    pub fn group_slot(h: i32, capacity: usize) -> usize {
        let spread = h ^ ((h as u32) >> 16) as i32;
        (spread as usize) & (capacity - 1)
    }
}

/// What a query of arbitrary form produced.
pub enum QueryOutput {
    /// A SELECT/ASK solution table.
    Table(QueryTable),
    /// A CONSTRUCT/DESCRIBE graph, already serialized.
    Graph(Vec<u8>),
}


/// Wrap a bare variable inside `GROUP_CONCAT(…)` in `STR(…)`.
///
/// SPARQL 1.1 leaves GROUP_CONCAT over a non-string literal to the
/// implementation, and oxigraph makes the whole aggregate UNBOUND rather than
/// concatenating the lexical form. Curated queries are written expecting the
/// lexical form: MONDO's `obsoletioncandidates-withcomment.sparql` concatenates
/// `IAO:0000233` values, which are `xsd:anyURI`, so without this rewrite all 420
/// issue links come out blank instead of populating the column. Rewriting
/// `GROUP_CONCAT(DISTINCT ?issue_)` to `GROUP_CONCAT(DISTINCT STR(?issue_))`
/// makes the lexical form explicit.
///
/// Deliberately narrow: only a bare `?var`/`$var` as the sole argument is
/// rewritten (optionally after `DISTINCT`, optionally before `;SEPARATOR=…`).
/// Any more complex expression is left exactly as written. One behaviour
/// difference remains and is accepted: `DISTINCT` then dedupes on the lexical
/// form rather than on the term, so two values differing only in datatype
/// collapse. No MONDO query has such a pair.
pub fn coerce_group_concat(sparql: &str) -> String {
    let bytes = sparql.as_bytes();
    let lower = sparql.to_ascii_lowercase();
    let mut out = String::with_capacity(sparql.len() + 32);
    let mut i = 0usize;
    while let Some(rel) = lower[i..].find("group_concat") {
        let start = i + rel;
        // Must be a whole token, not the tail of a longer name.
        let prev_ok = start == 0 || !is_name_char(bytes[start - 1]);
        let mut j = start + "group_concat".len();
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if !prev_ok || j >= bytes.len() || bytes[j] != b'(' {
            out.push_str(&sparql[i..start + "group_concat".len()]);
            i = start + "group_concat".len();
            continue;
        }
        j += 1; // past '('
        let arg_open = j;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        // Optional DISTINCT.
        if lower[j..].starts_with("distinct") {
            let after = j + "distinct".len();
            if after < bytes.len() && (bytes[after].is_ascii_whitespace() || bytes[after] == b'?' || bytes[after] == b'$') {
                j = after;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
            }
        }
        let var_start = j;
        if j < bytes.len() && (bytes[j] == b'?' || bytes[j] == b'$') {
            j += 1;
            while j < bytes.len() && is_name_char(bytes[j]) {
                j += 1;
            }
            let var_end = j;
            let mut k = j;
            while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            // The variable must be the WHOLE argument: `)` or the separator follows.
            if var_end > var_start + 1 && k < bytes.len() && (bytes[k] == b')' || bytes[k] == b';') {
                out.push_str(&sparql[i..arg_open]);
                out.push_str(&sparql[arg_open..var_start]);
                out.push_str("STR(");
                out.push_str(&sparql[var_start..var_end]);
                out.push(')');
                i = var_end;
                continue;
            }
        }
        out.push_str(&sparql[i..arg_open]);
        i = arg_open;
    }
    out.push_str(&sparql[i..]);
    out
}

fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Everything a query string goes through before oxigraph parses it.
///
/// Two textual rewrites accept query shapes curated repo queries rely on but
/// oxigraph reads strictly, and each has its own failure mode without them:
/// without [`coerce_group_concat`] the rows still come back but the aggregate is
/// UNBOUND, so the column is blank in every one; without
/// [`coerce_having_aliases`] the comparison errors, the group is filtered out,
/// and the query returns no rows at all. One pre-flight,
/// [`check_regex_patterns`], converts a regex oxigraph cannot compile from a
/// silent empty result into a hard error. Composed here rather than at each call
/// site so that `query_table`, `run_query`, `construct` and `check_query` cannot
/// drift apart.
fn prepare(sparql: &str) -> Result<String> {
    // The regex rewrite runs FIRST so the pre-flight compiles what will actually
    // be evaluated.
    let sparql = javaify_regex_classes(sparql);
    check_regex_patterns(&sparql)?;
    Ok(coerce_having_aliases(&coerce_group_concat(&sparql)))
}

/// Rewrite `\s`, `\d` and `\w` (and their complements) in every REGEX/REPLACE
/// pattern to the ASCII sets `java.util.regex` gives them.
///
/// SPARQL's `REGEX` is XPath 2.0's, whose `\s` is `[ \t\n\x0B\f\r]` — ASCII
/// only. Rust's `regex` crate is Unicode-aware by default, so its `\s` also
/// matches NO-BREAK SPACE and the rest of `\p{White_Space}`.
///
/// That is not a nicety. The `annotation_whitespace` report rule is
/// `FILTER REGEX(str(?value), "[\s\r\n]+$")`, MONDO's `profile.txt` promotes it
/// to ERROR, and MONDO:0019990 carries a synonym ending in U+00A0. Under the
/// Unicode reading that synonym is a violation and `make test` fails on an
/// ontology whose whitespace is clean.
fn javaify_regex_classes(sparql: &str) -> String {
    let b = sparql.as_bytes();
    let lower = sparql.to_ascii_lowercase();
    let mask = code_mask(sparql);
    // (start, end) of each pattern argument, in source order.
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    for i in 0..b.len() {
        let is_regex = keyword_at(&lower, &mask, i, "regex");
        if !is_regex && !keyword_at(&lower, &mask, i, "replace") {
            continue;
        }
        let name_len = if is_regex { 5 } else { 7 };
        let Some(open) = (i + name_len..b.len()).find(|&k| !b[k].is_ascii_whitespace()) else {
            continue;
        };
        if !mask[open] || b[open] != b'(' {
            continue;
        }
        let Some(close) = matching(b, &mask, open, b')') else { continue };
        let args = split_args(sparql, &mask, open + 1, close);
        let Some(arg) = args.get(1) else { continue };
        let trimmed = arg.trim();
        let Some(pattern) = sparql_string_value(trimmed) else { continue };
        let Some(rewritten) = ascii_classes(&pattern) else { continue };
        let off = trimmed.as_ptr() as usize - sparql.as_ptr() as usize;
        spans.push((off, off + trimmed.len(), quote_sparql_string(&rewritten)));
    }
    if spans.is_empty() {
        return sparql.to_string();
    }
    let mut out = String::with_capacity(sparql.len() + 64);
    let mut at = 0usize;
    for (start, end, text) in spans {
        if start < at {
            continue; // nested/overlapping — leave it alone
        }
        out.push_str(&sparql[at..start]);
        out.push_str(&text);
        at = end;
    }
    out.push_str(&sparql[at..]);
    out
}

/// The pattern with the ASCII shorthand classes substituted, or `None` when it
/// uses none of them (so the source text is left byte-for-byte alone).
fn ascii_classes(pattern: &str) -> Option<String> {
    const SPACE: &str = r" \t\n\x0B\f\r";
    const DIGIT: &str = "0-9";
    const WORD: &str = "a-zA-Z_0-9";
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut in_class = false;
    let mut changed = false;
    let mut cs = pattern.chars().peekable();
    while let Some(c) = cs.next() {
        if c == '\\' {
            let Some(&n) = cs.peek() else {
                out.push(c);
                break;
            };
            let set = match n {
                's' => Some((SPACE, false)),
                'S' => Some((SPACE, true)),
                'd' => Some((DIGIT, false)),
                'D' => Some((DIGIT, true)),
                'w' => Some((WORD, false)),
                'W' => Some((WORD, true)),
                _ => None,
            };
            match set {
                Some((body, negated)) => {
                    cs.next();
                    changed = true;
                    // Inside a class a positive set is spliced bare; a NEGATED one
                    // has to stay a class of its own, which the regex crate reads
                    // as a nested class.
                    if in_class && !negated {
                        out.push_str(body);
                    } else {
                        out.push('[');
                        if negated {
                            out.push('^');
                        }
                        out.push_str(body);
                        out.push(']');
                    }
                }
                None => {
                    out.push(c);
                    out.push(n);
                    cs.next();
                }
            }
            continue;
        }
        if c == '[' && !in_class {
            in_class = true;
        } else if c == ']' && in_class {
            in_class = false;
        }
        out.push(c);
    }
    changed.then_some(out)
}

/// Wrap `s` as a SPARQL double-quoted string literal.
fn quote_sparql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Lexical scaffolding shared by the textual rewrites
// ---------------------------------------------------------------------------

/// Which byte positions of `sparql` are *code* — i.e. outside a string literal,
/// an `<IRI>` and a `#` comment.
///
/// Every rewrite below consults this, so a keyword or variable that happens to
/// appear inside a literal is never touched. IRIs are recognised BEFORE comments
/// on purpose: `<http://www.w3.org/2000/01/rdf-schema#label>` contains a `#`, and
/// treating that as a comment start would blank the rest of the line.
fn code_mask(sparql: &str) -> Vec<bool> {
    let b = sparql.as_bytes();
    let mut mask = vec![true; b.len()];
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'<' => {
                // IRIREF: `<` … `>` containing none of the excluded characters.
                // Anything else is the `<` comparison operator, left as code.
                let mut j = i + 1;
                let mut closed = false;
                while j < b.len() {
                    match b[j] {
                        b'>' => {
                            closed = true;
                            break;
                        }
                        b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | b'\\' => break,
                        c if c.is_ascii_whitespace() => break,
                        _ => j += 1,
                    }
                }
                if closed {
                    mask[i..=j].fill(false);
                    i = j + 1;
                } else {
                    i += 1;
                }
            }
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    mask[i] = false;
                    i += 1;
                }
            }
            q @ (b'"' | b'\'') => {
                let long = i + 2 < b.len() && b[i + 1] == q && b[i + 2] == q;
                let mut j = i + if long { 3 } else { 1 };
                while j < b.len() {
                    if b[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if b[j] == q {
                        if !long {
                            j += 1;
                            break;
                        }
                        if j + 2 < b.len() && b[j + 1] == q && b[j + 2] == q {
                            j += 3;
                            break;
                        }
                    }
                    if !long && b[j] == b'\n' {
                        break; // unterminated short string; stop at the line end
                    }
                    j += 1;
                }
                let end = j.min(b.len());
                mask[i..end].fill(false);
                i = end;
            }
            _ => i += 1,
        }
    }
    mask
}

/// Characters that can be part of a SPARQL name, for keyword word-boundary
/// tests. `:` and `?`/`$` are included so `ex:having` and `?order` are not
/// mistaken for the keywords.
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'?' | b'$')
}

/// Is the lowercased keyword `kw` present at code position `i`, as a whole token?
///
/// Callers walk BYTES, so `i` can land inside a multi-byte character (a non-ASCII
/// prefixed name, say); `is_char_boundary` keeps the slice below from panicking.
fn keyword_at(lower: &str, mask: &[bool], i: usize, kw: &str) -> bool {
    if !mask.get(i).copied().unwrap_or(false)
        || !lower.is_char_boundary(i)
        || !lower[i..].starts_with(kw)
    {
        return false;
    }
    let b = lower.as_bytes();
    let prev_ok = i == 0 || !is_ident_char(b[i - 1]);
    let after = i + kw.len();
    let next_ok = after >= b.len() || !is_ident_char(b[after]);
    prev_ok && next_ok
}

/// The index of the delimiter closing the one at `open` (`{`/`}` or `(`/`)`),
/// counting only code positions.
fn matching(b: &[u8], mask: &[bool], open: usize, close_ch: u8) -> Option<usize> {
    let open_ch = b[open];
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate().skip(open) {
        if !mask[i] {
            continue;
        }
        if c == open_ch {
            depth += 1;
        } else if c == close_ch {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Apply non-overlapping `(start, end, replacement)` edits to `s`.
fn apply_edits(s: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    if edits.is_empty() {
        return s.to_string();
    }
    edits.sort_by_key(|(a, _, _)| *a);
    let mut out = String::with_capacity(s.len() + 32);
    let mut cursor = 0usize;
    for (a, b, rep) in edits {
        if a < cursor {
            continue; // overlapping edit; keep the earlier one
        }
        out.push_str(&s[cursor..a]);
        out.push_str(&rep);
        cursor = b;
    }
    out.push_str(&s[cursor..]);
    out
}

// ---------------------------------------------------------------------------
// HAVING / ORDER BY over a SELECT-expression alias
// ---------------------------------------------------------------------------

/// Inline a SELECT-expression alias where the same query level's `HAVING` (or
/// `ORDER BY`) refers to it: `(COUNT(?x) AS ?n) … HAVING (?n > 1)` becomes
/// `… HAVING ((COUNT(?x)) > 1)`.
///
/// SPARQL 1.1 evaluates HAVING before the projection's `Extend`, so strictly
/// `?n` is unbound there and oxigraph treats it that way: the comparison raises
/// an expression error, SPARQL turns "error" into "the group is filtered out",
/// and the query returns ZERO ROWS — a QC check that passes by doing nothing.
/// Curated queries are written to the lenient reading, where the alias resolves
/// against the SELECT clause.
///
/// This is exactly the shape of CL's and UBERON's
/// `label-synonym-polysemy-violation.sparql` (a live SPARQL validation check in
/// both) and of EFO's `multiple-label-violation.sparql`. ORDER BY gets the same
/// treatment, for the same reason.
///
/// Aliases are resolved PER QUERY LEVEL: a subquery's aliases are only inlined
/// into that subquery's own modifiers, never into the enclosing query's.
pub fn coerce_having_aliases(sparql: &str) -> String {
    let bytes = sparql.as_bytes();
    let lower = sparql.to_ascii_lowercase();
    let mask = code_mask(sparql);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    let mut i = 0usize;
    while i < bytes.len() {
        if !keyword_at(&lower, &mask, i, "select") {
            i += 1;
            continue;
        }
        let after = i + "select".len();
        i = after;

        // The projection runs from here to the dataset/WHERE clause.
        let Some(proj_end) = (after..bytes.len()).find(|&k| {
            mask[k]
                && (bytes[k] == b'{'
                    || keyword_at(&lower, &mask, k, "where")
                    || keyword_at(&lower, &mask, k, "from"))
        }) else {
            continue;
        };
        let aliases = select_aliases(sparql, &lower, &mask, after, proj_end);
        if aliases.is_empty() {
            continue;
        }

        // The group after the projection is this level's WHERE clause; the
        // solution modifiers run from its closing brace to the end of the level.
        let Some(open) = (proj_end..bytes.len()).find(|&k| mask[k] && bytes[k] == b'{') else {
            continue;
        };
        let Some(close) = matching(bytes, &mask, open, b'}') else { continue };
        // A level ends at the brace that closes the group enclosing it (a
        // subquery), or at the end of the string (the outermost query). Inline
        // data (`… VALUES ?x { … }`) also ends the modifier region.
        let end = (close + 1..bytes.len())
            .find(|&k| mask[k] && (bytes[k] == b'}' || bytes[k] == b'{'))
            .unwrap_or(bytes.len());
        // Only from the first HAVING/ORDER onward: GROUP BY sits before them and
        // must keep its own variables. LIMIT/OFFSET take integers, so including
        // them costs nothing.
        let Some(start) = (close + 1..end).find(|&k| {
            keyword_at(&lower, &mask, k, "having") || keyword_at(&lower, &mask, k, "order")
        }) else {
            continue;
        };
        collect_alias_edits(bytes, &mask, start, end, &aliases, &mut edits);
    }
    apply_edits(sparql, edits)
}

/// The `(<expr> AS ?alias)` bindings of one SELECT clause, as `(alias, expr)`.
fn select_aliases(
    sparql: &str,
    lower: &str,
    mask: &[bool],
    start: usize,
    end: usize,
) -> Vec<(String, String)> {
    let b = sparql.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        if !mask[i] || b[i] != b'(' {
            i += 1;
            continue;
        }
        let Some(close) = matching(b, mask, i, b')') else { break };
        if close >= end {
            break;
        }
        // The LAST top-level `AS` in the group separates expression from alias.
        let mut depth = 0i32;
        let mut as_at = None;
        for k in i + 1..close {
            if !mask[k] {
                continue;
            }
            match b[k] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ if depth == 0 && keyword_at(lower, mask, k, "as") => as_at = Some(k),
                _ => {}
            }
        }
        if let Some(as_at) = as_at {
            let expr = sparql[i + 1..as_at].trim();
            let rest = &sparql[as_at + 2..close];
            let name = rest.trim();
            if !expr.is_empty()
                && (name.starts_with('?') || name.starts_with('$'))
                && name[1..].bytes().all(is_name_char)
                && name.len() > 1
            {
                out.push((name[1..].to_string(), expr.to_string()));
            }
        }
        i = close + 1;
    }
    out
}

/// Record a substitution for every `?alias`/`$alias` occurrence in `[start, end)`.
fn collect_alias_edits(
    b: &[u8],
    mask: &[bool],
    start: usize,
    end: usize,
    aliases: &[(String, String)],
    edits: &mut Vec<(usize, usize, String)>,
) {
    let mut i = start;
    while i < end {
        if !mask[i] || (b[i] != b'?' && b[i] != b'$') {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < end && is_name_char(b[j]) {
            j += 1;
        }
        let name = std::str::from_utf8(&b[i + 1..j]).unwrap_or("");
        if let Some((_, expr)) = aliases.iter().find(|(a, _)| a == name) {
            edits.push((i, j, format!("({expr})")));
        }
        i = j.max(i + 1);
    }
}

// ---------------------------------------------------------------------------
// Regex pre-flight
// ---------------------------------------------------------------------------

/// Reject a query whose literal `REGEX(`/`REPLACE(` pattern Rust's regex engine
/// cannot compile.
///
/// oxigraph builds the pattern with the `regex` crate, which has no lookahead,
/// no lookbehind and no backreferences. On a compile failure spareval's
/// `compile_pattern` returns `None`, the expression yields an error, and SPARQL's
/// error semantics turn that into "this row is filtered out" — so the query
/// quietly returns nothing at all.
///
/// MONDO's `src/sparql/reports/subclass-axioms-only-supported-by-ordo.sparql`
/// carries `FILTER regex(str(?source2), "^(?!Orphanet:)")`, a negative lookahead,
/// so that report would otherwise "pass" by matching nothing. Failing loudly is
/// the correct direction: a check that cannot run must not report success.
pub fn check_regex_patterns(sparql: &str) -> Result<()> {
    let b = sparql.as_bytes();
    let lower = sparql.to_ascii_lowercase();
    let mask = code_mask(sparql);
    for i in 0..b.len() {
        // REGEX(text, pattern [, flags]) and REPLACE(arg, pattern, repl [, flags]).
        let flag_arg = if keyword_at(&lower, &mask, i, "regex") {
            2
        } else if keyword_at(&lower, &mask, i, "replace") {
            3
        } else {
            continue;
        };
        let name_len = if flag_arg == 2 { 5 } else { 7 };
        let Some(open) = (i + name_len..b.len()).find(|&k| !b[k].is_ascii_whitespace()) else {
            continue;
        };
        if !mask[open] || b[open] != b'(' {
            continue;
        }
        let Some(close) = matching(b, &mask, open, b')') else { continue };
        let args = split_args(sparql, &mask, open + 1, close);
        let Some(pattern) = args.get(1).and_then(|a| sparql_string_value(a.trim())) else {
            continue; // not a literal — nothing to pre-flight
        };
        let flags = args.get(flag_arg).and_then(|a| sparql_string_value(a.trim()));
        if let Err(why) = compile_like_spareval(&pattern, flags.as_deref()) {
            bail!(
                "unsupported regular expression {pattern:?} in SPARQL {}: {why}. \
                 Rust's regex engine (which is what evaluates SPARQL here) has no lookahead, \
                 lookbehind or backreferences. Rewrite the pattern — a negative lookahead like \
                 \"^(?!X)\" is `FILTER(!STRSTARTS(str(?v), \"X\"))`. Leaving it in place would \
                 make the check silently match nothing and report success.",
                if flag_arg == 2 { "REGEX()" } else { "REPLACE()" }
            );
        }
    }
    Ok(())
}

/// Split a bracketed argument list into its top-level comma-separated parts.
fn split_args<'a>(sparql: &'a str, mask: &[bool], start: usize, end: usize) -> Vec<&'a str> {
    let b = sparql.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut from = start;
    for k in start..end {
        if !mask[k] {
            continue;
        }
        match b[k] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&sparql[from..k]);
                from = k + 1;
            }
            _ => {}
        }
    }
    out.push(&sparql[from..end]);
    out
}

/// The value of a SPARQL string literal, or `None` if `s` is not one.
fn sparql_string_value(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let q = *b.first()?;
    if q != b'"' && q != b'\'' {
        return None;
    }
    let long = b.len() >= 6 && b[1] == q && b[2] == q;
    let open = if long { 3 } else { 1 };
    let closing = if long { s.len().checked_sub(3)? } else { s.len().checked_sub(1)? };
    if closing < open || !s[closing..].bytes().all(|c| c == q) {
        return None;
    }
    let mut out = String::new();
    let body = s[open..closing].as_bytes();
    let mut i = 0usize;
    while i < body.len() {
        if body[i] == b'\\' && i + 1 < body.len() {
            let c = body[i + 1];
            i += 2;
            out.push(match c {
                b't' => '\t',
                b'n' => '\n',
                b'r' => '\r',
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                other => other as char,
            });
            continue;
        }
        // Multi-byte characters pass through untouched.
        let start = i;
        i += 1;
        while i < body.len() && (body[i] & 0xC0) == 0x80 {
            i += 1;
        }
        out.push_str(std::str::from_utf8(&body[start..i]).unwrap_or(""));
    }
    Some(out)
}

/// Compile `pattern` exactly the way spareval's `compile_pattern` does, so the
/// pre-flight accepts precisely what the evaluator will accept.
fn compile_like_spareval(pattern: &str, flags: Option<&str>) -> std::result::Result<(), String> {
    // The size limit the evaluator itself builds patterns with.
    const REGEX_SIZE_LIMIT: usize = 1_000_000;
    let flags = flags.unwrap_or_default();
    let escaped;
    let pattern = if flags.contains('q') {
        escaped = regex::escape(pattern);
        escaped.as_str()
    } else {
        pattern
    };
    let mut builder = regex::RegexBuilder::new(pattern);
    builder.size_limit(REGEX_SIZE_LIMIT);
    for flag in flags.chars() {
        match flag {
            's' => {
                builder.dot_matches_new_line(true);
            }
            'm' => {
                builder.multi_line(true);
            }
            'i' => {
                builder.case_insensitive(true);
            }
            'x' => {
                builder.ignore_whitespace(true);
            }
            'q' => {}
            other => return Err(format!("unsupported regex flag {other:?}")),
        }
    }
    builder.build().map(|_| ()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Query form
// ---------------------------------------------------------------------------

/// The four SPARQL query forms, which settle the default output format when
/// neither `--format` nor the output path's extension does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QueryForm {
    Select,
    Ask,
    Construct,
    Describe,
}

/// The form of `sparql`, found by skipping the prologue. Detected textually so a
/// query that fails to parse still gets a sensible output extension, and via
/// [`code_mask`] so a `SELECT` inside a comment or literal is ignored.
pub fn query_form(sparql: &str) -> QueryForm {
    let lower = sparql.to_ascii_lowercase();
    let mask = code_mask(sparql);
    for i in 0..sparql.len() {
        if keyword_at(&lower, &mask, i, "select") {
            return QueryForm::Select;
        }
        if keyword_at(&lower, &mask, i, "ask") {
            return QueryForm::Ask;
        }
        if keyword_at(&lower, &mask, i, "construct") {
            return QueryForm::Construct;
        }
        if keyword_at(&lower, &mask, i, "describe") {
            return QueryForm::Describe;
        }
    }
    QueryForm::Select
}

/// A simple tabular query result.
pub struct QueryTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// The same cells in SPARQL-TSV form (see [`term_to_tsv`]). Empty when the
    /// result did not come from a SELECT — and also when a SELECT matched
    /// nothing, which is why [`QueryTable::select`] exists separately.
    pub tsv_rows: Vec<Vec<String>>,
    /// Whether the result is a SELECT solution sequence. `render` needs this
    /// independently of `tsv_rows`: a SELECT that matched NOTHING still gets the
    /// SPARQL-TSV header (`?var`), whereas an ASK/CONSTRUCT table never does.
    /// Keying off `tsv_rows.is_empty()` alone would silently degrade every
    /// zero-row report to the CSV-style header.
    pub select: bool,
}

impl QueryTable {
    /// The rows as `{column: value}` maps — the record form the language
    /// bindings turn into DataFrames. Empty cells are omitted from a row's map.
    pub fn records(&self) -> Vec<std::collections::BTreeMap<String, String>> {
        self.rows
            .iter()
            .map(|row| {
                self.columns
                    .iter()
                    .zip(row)
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(c, v)| (c.clone(), v.clone()))
                    .collect()
            })
            .collect()
    }

    /// Render as TSV (default) or CSV.
    pub fn render(&self, csv: bool) -> String {
        let sep = if csv { "," } else { "\t" };
        // CSV output terminates every line with CRLF (RFC 4180); TSV output uses a
        // bare LF. So OBA's `tmp/simple_seed.txt` and `tmp/ontologyterms.txt` —
        // both built by `query -f csv` and then `cat | sort | uniq` — are CRLF
        // files, while every `-f tsv` report is not.
        let nl = if csv { "\r\n" } else { "\n" };
        let mut out = String::new();
        if csv || !self.select {
            out.push_str(&self.columns.join(sep));
        } else {
            // The SPARQL-TSV header names each variable with its `?`. MONDO's
            // report recipes strip it again with `sed -i 's/[?]//g'`, which only
            // makes sense because it is there.
            let hdr: Vec<String> = self.columns.iter().map(|c| format!("?{c}")).collect();
            out.push_str(&hdr.join(sep));
        }
        out.push_str(nl);
        let rows = if csv || !self.select { &self.rows } else { &self.tsv_rows };
        for row in rows {
            let cells: Vec<String> = if csv {
                row.iter().map(|c| escape_csv(c)).collect()
            } else {
                row.clone()
            };
            out.push_str(&cells.join(sep));
            out.push_str(nl);
        }
        out
    }
}

fn escape_csv(s: &str) -> String {
    // CSV quoting: quote on `"`, `,`, CR or LF, doubling any `"`.
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Validate that the SPARQL string is parseable (so callers can fail fast).
pub fn check_query(sparql: &str) -> Result<()> {
    match SparqlEvaluator::new().parse_query(&prepare(sparql)?) {
        Ok(_) => Ok(()),
        Err(e) => bail!("SPARQL parse error: {e}"),
    }
}

#[cfg(test)]
mod group_concat_tests {
    use super::coerce_group_concat;

    #[test]
    fn wraps_a_bare_variable() {
        let q = "SELECT (GROUP_CONCAT(DISTINCT ?issue_;SEPARATOR=\"|\") AS ?issue) WHERE {}";
        assert!(coerce_group_concat(q).contains("GROUP_CONCAT(DISTINCT STR(?issue_);SEPARATOR=\"|\")"));
    }

    #[test]
    fn wraps_without_distinct_or_separator() {
        assert_eq!(
            coerce_group_concat("(GROUP_CONCAT(?x) AS ?y)"),
            "(GROUP_CONCAT(STR(?x)) AS ?y)"
        );
    }

    #[test]
    fn leaves_a_complex_argument_alone() {
        let q = "(GROUP_CONCAT(CONCAT(?a, ?b)) AS ?y)";
        assert_eq!(coerce_group_concat(q), q);
        let q2 = "(GROUP_CONCAT(STR(?a)) AS ?y)";
        assert_eq!(coerce_group_concat(q2), q2);
    }

    #[test]
    fn leaves_other_aggregates_and_names_alone() {
        let q = "SELECT (COUNT(?x) AS ?n) (SUM(?y) AS ?s) WHERE { ?x ?p ?group_concat_thing }";
        assert_eq!(coerce_group_concat(q), q);
    }

    #[test]
    fn handles_several_in_one_query() {
        let out = coerce_group_concat("(GROUP_CONCAT(DISTINCT ?a;SEPARATOR=\"|\") AS ?x) (GROUP_CONCAT(DISTINCT ?b;SEPARATOR=\"|\") AS ?y)");
        assert!(out.contains("STR(?a)") && out.contains("STR(?b)"));
    }
}

#[cfg(test)]
mod having_alias_tests {
    use super::*;

    /// The exact shape of CL's / UBERON's `label-synonym-polysemy-violation.sparql`
    /// (a live SPARQL validation check in both): the alias is bound in a SUBQUERY's
    /// projection and referenced by that subquery's HAVING.
    #[test]
    fn inlines_a_subquery_having_alias() {
        let q = r#"PREFIX owl: <http://www.w3.org/2002/07/owl#>
SELECT ?entity ?property ?value WHERE {
  { SELECT DISTINCT ?iname (COUNT(DISTINCT ?entity) AS ?cnt) WHERE {
      ?entity ?property ?name .
      BIND(UCASE((?name)) AS ?iname)
    } GROUP BY ?iname HAVING (?cnt > 1)
  }
  ?entity ?property ?value .
} ORDER BY ?entity"#;
        let out = coerce_having_aliases(q);
        assert!(
            out.contains("HAVING ((COUNT(DISTINCT ?entity)) > 1)"),
            "alias not inlined: {out}"
        );
        // The OUTER query has no aliases, so its ORDER BY is untouched.
        assert!(out.ends_with("ORDER BY ?entity"));
        // The projection itself keeps the alias — only the modifiers are rewritten.
        assert!(out.contains("(COUNT(DISTINCT ?entity) AS ?cnt)"));
        // And the rewrite is still a legal query.
        assert!(SparqlEvaluator::new().parse_query(&out).is_ok(), "{out}");
    }

    /// EFO's `multiple-label-violation.sparql`: top-level query, lowercase `as`,
    /// a space between the aggregate name and its bracket.
    #[test]
    fn inlines_a_top_level_having_alias() {
        let q = "SELECT ?cls (COUNT (DISTINCT ?label) as ?label_count)\nWHERE\n{\n  ?cls \
                 <http://www.w3.org/2000/01/rdf-schema#label> ?label .\n}\nGROUP BY ?cls \
                 ?label\nHAVING (?label_count > 1)";
        let out = coerce_having_aliases(q);
        assert!(out.ends_with("HAVING ((COUNT (DISTINCT ?label)) > 1)"), "{out}");
        // GROUP BY sits before HAVING and must keep its own variables.
        assert!(out.contains("GROUP BY ?cls ?label"));
        assert!(SparqlEvaluator::new().parse_query(&out).is_ok(), "{out}");
    }

    #[test]
    fn inlines_into_order_by_too() {
        let q = "SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?s ORDER BY DESC(?n)";
        let out = coerce_having_aliases(q);
        assert!(out.contains("DESC((COUNT(?o)))"), "{out}");
    }

    /// A subquery alias must not leak into the ENCLOSING level's modifiers.
    #[test]
    fn keeps_levels_apart() {
        let q = "SELECT ?s WHERE { { SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s ?p ?o } \
                 GROUP BY ?s HAVING (?n > 1) } } ORDER BY ?n";
        let out = coerce_having_aliases(q);
        assert!(out.contains("HAVING ((COUNT(?o)) > 1)"), "{out}");
        assert!(out.ends_with("ORDER BY ?n"), "{out}");
    }

    /// The scanners walk bytes, so a multi-byte character anywhere in the query
    /// must not put a slice mid-character.
    #[test]
    fn survives_non_ascii_text() {
        let q = "SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s ?p \"β-catenin — αβ\" . ?s ?p ?o } \
                 GROUP BY ?s HAVING (?n > 1)";
        let out = coerce_having_aliases(q);
        assert!(out.contains("HAVING ((COUNT(?o)) > 1)"), "{out}");
        assert!(check_regex_patterns(q).is_ok());
        assert_eq!(query_form(q), QueryForm::Select);
    }

    #[test]
    fn leaves_a_plain_query_alone() {
        for q in [
            "SELECT ?s WHERE { ?s ?p ?o } ORDER BY ?s",
            "SELECT * WHERE { ?s ?p ?o }",
            // `HAVING` inside a literal, and a `#` inside an IRI, must not confuse
            // the scanner.
            "SELECT ?s WHERE { ?s <http://x.org/a#b> \"HAVING (?n > 1)\" }",
        ] {
            assert_eq!(coerce_having_aliases(q), q);
        }
    }

    /// Evaluated end to end: without the rewrite this returns no rows at all.
    #[test]
    fn having_alias_actually_yields_rows() {
        let store = Store::new().unwrap();
        store
            .load_from_slice(
                RdfParser::from_format(oxigraph::io::RdfFormat::NTriples),
                b"<http://x/1> <http://x/p> \"a\" .\n\
                  <http://x/1> <http://x/p> \"b\" .\n\
                  <http://x/2> <http://x/p> \"c\" .\n" as &[u8],
            )
            .unwrap();
        let q = Queryable { store, type_order: Default::default() };
        let sparql = "SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s <http://x/p> ?o } \
                      GROUP BY ?s HAVING (?n > 1)";
        assert_eq!(q.count(sparql).unwrap(), 1);
    }
}

#[cfg(test)]
mod regex_preflight_tests {
    use super::check_regex_patterns;

    /// The negative lookahead MONDO's `subclass-axioms-only-supported-by-ordo.sparql` carries.
    #[test]
    fn rejects_a_lookahead() {
        let q = "SELECT ?x WHERE { ?x ?p ?source2 FILTER regex(str(?source2), \"^(?!Orphanet:)\") }";
        let e = check_regex_patterns(q).unwrap_err().to_string();
        assert!(e.contains("^(?!Orphanet:)"), "{e}");
        assert!(e.contains("lookahead"), "{e}");
    }

    #[test]
    fn accepts_the_patterns_these_repos_actually_use() {
        for q in [
            "FILTER (isIRI(?t) && regex(str(?t), \"^http://purl.obolibrary.org/obo/MONDO_\"))",
            "FILTER regex(?label, \"^obsolete\", \"i\")",
            "BIND(REPLACE(str(?x), \"[.]owl$\", \"\") AS ?y)",
            // `q` escapes the pattern wholesale, so even a lookahead is fine.
            "FILTER regex(?x, \"^(?!a)\", \"q\")",
            // A non-literal pattern cannot be pre-flighted; it must not error.
            "FILTER regex(?x, ?pattern)",
        ] {
            assert!(check_regex_patterns(q).is_ok(), "{q}");
        }
    }

    /// A pattern that only appears inside a comment or a literal is not a pattern.
    #[test]
    fn ignores_non_code_occurrences() {
        assert!(check_regex_patterns("# regex(?x, \"^(?!a)\")\nSELECT * WHERE {}").is_ok());
        assert!(check_regex_patterns("SELECT ?x WHERE { ?s ?p \"regex(?x, ^(?!a))\" }").is_ok());
    }
}

#[cfg(test)]
mod tsv_literal_tests {
    use super::*;

    fn tsv(value: &str, dt: &str) -> String {
        let lit = oxigraph::model::Literal::new_typed_literal(
            value,
            oxigraph::model::NamedNode::new(dt).unwrap(),
        );
        term_to_tsv(&Term::Literal(lit))
    }

    /// EFO `reports/class-count-by-prefix.tsv` and MONDO
    /// `report-reason-paper-ct-xrefs-by-source.tsv` both end their rows with a
    /// BARE count.
    #[test]
    fn numeric_and_boolean_literals_lose_their_datatype() {
        assert_eq!(tsv("2", "http://www.w3.org/2001/XMLSchema#integer"), "2");
        assert_eq!(tsv("-17", "http://www.w3.org/2001/XMLSchema#integer"), "-17");
        assert_eq!(tsv("16093", "http://www.w3.org/2001/XMLSchema#int"), "16093");
        assert_eq!(tsv("1.5", "http://www.w3.org/2001/XMLSchema#decimal"), "1.5");
        assert_eq!(tsv("1e3", "http://www.w3.org/2001/XMLSchema#double"), "1e3");
        assert_eq!(tsv("true", "http://www.w3.org/2001/XMLSchema#boolean"), "true");
    }

    /// …but only when the lexical form is a legal Turtle abbreviation, and never
    /// for a datatype that has no short form at all. MONDO's
    /// `report-obsoletioncandidates-withcomment.tsv` keeps `^^<…#anyURI>`.
    #[test]
    fn everything_else_keeps_its_form() {
        assert_eq!(
            tsv("https://x/1", "http://www.w3.org/2001/XMLSchema#anyURI"),
            "\"https://x/1\"^^<http://www.w3.org/2001/XMLSchema#anyURI>"
        );
        // decimal without a '.' and double without an exponent are not abbreviated
        // (they would read back as an integer).
        assert_eq!(
            tsv("2", "http://www.w3.org/2001/XMLSchema#decimal"),
            "\"2\"^^<http://www.w3.org/2001/XMLSchema#decimal>"
        );
        assert_eq!(
            tsv("1.5", "http://www.w3.org/2001/XMLSchema#double"),
            "\"1.5\"^^<http://www.w3.org/2001/XMLSchema#double>"
        );
        assert_eq!(
            tsv("TRUE", "http://www.w3.org/2001/XMLSchema#boolean"),
            "\"TRUE\"^^<http://www.w3.org/2001/XMLSchema#boolean>"
        );
        // xsd:string stays quoted but drops the datatype (EFO basic-report.tsv).
        assert_eq!(tsv("a b", "http://www.w3.org/2001/XMLSchema#string"), "\"a b\"");
    }

    /// A SELECT that matched nothing still gets the `?var` TSV header — the
    /// header is a property of the query form, not of the row count.
    #[test]
    fn zero_row_select_keeps_the_sparql_tsv_header() {
        let empty = QueryTable {
            columns: vec!["cls".into(), "n".into()],
            rows: Vec::new(),
            tsv_rows: Vec::new(),
            select: true,
        };
        assert_eq!(empty.render(false), "?cls\t?n\n");
        assert_eq!(empty.render(true), "cls,n\r\n");
    }
}

#[cfg(test)]
mod query_form_tests {
    use super::{query_form, QueryForm};

    #[test]
    fn skips_the_prologue_comments_and_literals() {
        assert_eq!(
            query_form("# a SELECT comment\nPREFIX x: <http://x/>\nASK { ?s ?p ?o }"),
            QueryForm::Ask
        );
        assert_eq!(
            query_form("PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\nCONSTRUCT { ?s ?p ?o } WHERE {}"),
            QueryForm::Construct
        );
        assert_eq!(query_form("SELECT ?s WHERE { ?s ?p ?o }"), QueryForm::Select);
        assert_eq!(query_form("DESCRIBE <http://x/1>"), QueryForm::Describe);
    }
}

#[cfg(test)]
mod jena_order_tests {
    use super::jena_order::*;

    #[test]
    fn java_string_hash_matches_the_jvm() {
        // The hashes of "", "a" and "abc" under `h = h * 31 + unit`.
        assert_eq!(java_string_hash(""), 0);
        assert_eq!(java_string_hash("a"), 97);
        assert_eq!(java_string_hash("abc"), 96354);
        // A code unit, not a byte: an en dash counts once, not three times, and
        // a character above the Basic Multilingual Plane counts as its two
        // surrogates.
        assert_eq!(java_string_hash("\u{2013}"), 8211);
        assert_eq!(java_string_hash("a\u{2013}b"), 347856);
        assert_eq!(java_string_hash("\u{1d49c}"), 1772295);
    }

    #[test]
    fn capacity_follows_jena_growth() {
        assert_eq!(capacity_for(8), None); // still a flat array
        assert_eq!(capacity_for(9), None); // nine still fits the array
        // The tenth triple builds the table out of the nine already there, and
        // arriving into a table sized for nine grows it at once.
        assert_eq!(capacity_for(10), Some(79));
        assert_eq!(capacity_for(39), Some(79));
        assert_eq!(capacity_for(40), Some(307));
        assert_eq!(capacity_for(153), Some(307));
        assert_eq!(capacity_for(154), Some(617));
    }

    /// The case a committed report exercises: MONDO_0008002's three
    /// `IAO:0000233` values come out 9764, 5219, 9285 — neither document order
    /// nor sorted, but ascending by slot at capacity 79.
    #[test]
    fn reproduces_jenas_group_concat_order() {
        let s = node_hash("http://purl.obolibrary.org/obo/MONDO_0008002");
        let p = node_hash("http://purl.obolibrary.org/obo/IAO_0000233");
        let base = "https://github.com/monarch-initiative/mondo/issues/";
        let mut v: Vec<(&str, i32)> = ["5219", "9285", "9764"]
            .iter()
            .map(|n| (*n, slot(s, p, node_hash(&format!("{base}{n}")), 79)))
            .collect();
        v.sort_by_key(|(_, k)| *k);
        assert_eq!(v.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec!["9764", "5219", "9285"]);
    }
}

#[cfg(test)]
mod regex_class_tests {
    use super::*;

    /// `FILTER REGEX(str(?v), "[\s\r\n]+$")` must select the literal ending in
    /// an ASCII space and NOT the one ending in U+00A0. Rust's `regex` matches
    /// both unless `\s` is spelled out as the ASCII set.
    #[test]
    fn backslash_s_is_ascii_only() {
        let pattern = ascii_classes(r"[\s\r\n]+$").expect("rewritten");
        assert_eq!(pattern, r"[ \t\n\x0B\f\r\r\n]+$");
        let re = regex::Regex::new(&pattern).unwrap();
        assert!(re.is_match("trailing "));
        assert!(!re.is_match("trailing\u{a0}"));
        // …and the rewrite reaches the query text.
        let q = r#"SELECT ?s WHERE { FILTER REGEX(str(?v), "[\\s\\r\\n]+$") }"#;
        assert!(javaify_regex_classes(q).contains(r"\\t\\n\\x0B"), "{}", javaify_regex_classes(q));
    }

    /// A pattern with none of the shorthand classes is left alone byte for byte.
    #[test]
    fn untouched_when_no_shorthand() {
        let q = r#"SELECT ?s WHERE { FILTER REGEX(?v, "^MONDO:[0-9]+$") }"#;
        assert_eq!(javaify_regex_classes(q), q);
    }
}
