//! Turtle and N-Triples input, and line-based RDF output, bridged through
//! oxigraph's parsers and serializers: the ontology is moved between those
//! syntaxes and RDF/XML, which horned-owl reads and writes. Turtle output is
//! laid out by `owlapi_ttl`, which falls back to [`save_plain`].

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};

use anyhow::{anyhow, Result};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{BlankNode, NamedOrBlankNode, Term, Triple};

use crate::io::Format;
use crate::model::Model;

/// Write a model as N-Triples.
pub fn save_ntriples<W: Write>(model: &Model, writer: &mut W) -> Result<()> {
    save_lines(model, &[], writer, RdfFormat::NTriples)
}

/// Write a model as Turtle one statement at a time, declaring `prefixes`
/// (name → namespace) and writing every IRI one of them covers as a prefixed
/// name.
pub fn save_plain<W: Write>(model: &Model, prefixes: &[(String, String)], writer: &mut W) -> Result<()> {
    save_lines(model, prefixes, writer, RdfFormat::Turtle)
}

/// Every triple of the model's RDF mapping, each once, in a fixed order: a
/// blank node is labelled by the order the mapping first names it, and the
/// triples are sorted.
pub(crate) fn mapped_triples(model: &Model) -> Result<Vec<Triple>> {
    let mut rdf = Vec::new();
    crate::io::write_to_ref(model, &mut rdf, Format::RdfXml)?;
    let mut labels: HashMap<BlankNode, BlankNode> = HashMap::new();
    let mut label = |node: BlankNode| {
        let next = labels.len();
        labels.entry(node).or_insert_with(|| BlankNode::new_unchecked(format!("b{next}"))).clone()
    };
    let mut triples = Vec::new();
    for quad in RdfParser::from_format(RdfFormat::RdfXml).for_slice(&rdf) {
        let quad = quad.map_err(|e| anyhow!("loading triples: {e}"))?;
        let subject = match quad.subject {
            NamedOrBlankNode::BlankNode(b) => NamedOrBlankNode::BlankNode(label(b)),
            named => named,
        };
        let object = match quad.object {
            Term::BlankNode(b) => Term::BlankNode(label(b)),
            other => other,
        };
        triples.push(Triple::new(subject, quad.predicate, object));
    }
    triples.sort_by_cached_key(|t| t.to_string());
    triples.dedup();
    Ok(triples)
}

fn save_lines<W: Write>(model: &Model, prefixes: &[(String, String)], writer: &mut W, fmt: RdfFormat) -> Result<()> {
    let mut serializer = RdfSerializer::from_format(fmt);
    for (name, ns) in prefixes {
        serializer = serializer
            .with_prefix(name.as_str(), ns.as_str())
            .map_err(|e| anyhow!("prefix {name}: <{ns}>: {e}"))?;
    }
    let mut out = serializer.for_writer(writer);
    for triple in &mapped_triples(model)? {
        out.serialize_triple(triple).map_err(|e| anyhow!("serializing {fmt:?}: {e}"))?;
    }
    out.finish().map_err(|e| anyhow!("serializing {fmt:?}: {e}"))?;
    Ok(())
}

/// Load a model from Turtle.
pub fn load<R: BufRead>(reader: R) -> Result<Model> {
    load_as(reader, RdfFormat::Turtle)
}

/// [`load`] in an arbitrary line-based RDF syntax. N-Triples is a syntactic subset
/// of Turtle, but oxigraph's parsers are strict, so the caller passes the format
/// the file actually is — MONDO mirrors `hgnc_gene.nt` and `ncbi_gene.nt`.
pub fn load_as<R: BufRead>(mut reader: R, fmt: RdfFormat) -> Result<Model> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    // The statements in the order the document makes them, every blank node
    // named for the order the document first mentions it in.
    let mut made: Vec<Triple> = Vec::new();
    let mut names: HashMap<BlankNode, BlankNode> = HashMap::new();
    let mut name = |b: &BlankNode| -> BlankNode {
        let next = names.len() + 1;
        names.entry(b.clone()).or_insert_with(|| BlankNode::new_unchecked(format!("b{next}"))).clone()
    };
    // A relative IRI resolves against the document's own IRI.
    let mut parser = RdfParser::from_format(fmt);
    if let Some(base) = crate::io::document_iri() {
        parser = parser.with_base_iri(base.as_str()).map_err(|e| anyhow!("document IRI {base}: {e}"))?;
    }
    for quad in parser.for_slice(&buf) {
        let mut triple = Triple::from(quad.map_err(|e| anyhow!("parsing Turtle: {e}"))?);
        if let NamedOrBlankNode::BlankNode(b) = &triple.subject {
            triple.subject = NamedOrBlankNode::BlankNode(name(b));
        }
        if let Term::BlankNode(b) = &triple.object {
            triple.object = Term::BlankNode(name(b));
        }
        made.push(triple);
    }
    // Each statement once, where the document first makes it.
    let first: Vec<bool> = {
        let mut seen: HashSet<&Triple> = HashSet::with_capacity(made.len());
        made.iter().map(|t| seen.insert(t)).collect()
    };
    let statements: Vec<Triple> = made.into_iter().zip(first).filter_map(|(t, first)| first.then_some(t)).collect();
    // Re-serialised in that order, as the document wrote each literal, so the
    // parse meets the document's blank nodes in the order the document does.
    let mut rdf = Vec::new();
    let mut ser = RdfSerializer::from_format(RdfFormat::RdfXml).for_writer(&mut rdf);
    for t in &statements {
        ser.serialize_triple(t).map_err(|e| anyhow!("re-serializing as RDF/XML: {e}"))?;
    }
    ser.finish().map_err(|e| anyhow!("re-serializing as RDF/XML: {e}"))?;
    drop(statements);
    let base = crate::io::anon_counter();
    let mut model = super::load_from_raw(std::io::Cursor::new(rdf), Format::RdfXml)?;
    // A Turtle document's blank nodes take no ids of their own: its anonymous
    // individuals are numbered from where the count stood, in the order the
    // parse met them, and the count goes on after them.
    if let Some(ids) = crate::io::numbered_individuals(&model.ont) {
        let next = crate::io::renumber_individuals(&mut model.ont, ids, base);
        crate::io::set_anon_counter(next);
    }
    // That RDF/XML is oxigraph's own re-serialisation, not a source document, so its
    // xmlns block is oxigraph's invention — `oxrdfxml`'s writer unconditionally
    // declares `xmlns:its="http://www.w3.org/2005/11/its"` (for RDF 1.2 base
    // direction) alongside prefixes it derives from the data. `load_from` scans that
    // block as if it were the document's own, so taking it would plant a phantom `its`
    // in every artefact downstream of MONDO's `skos.ttl`: it would reach the xmlns of
    // filtered.owl/reasoned.owl/mondo.owl/mondo-base.owl and the `idspace:` block of
    // mondo.obo, all declaring a prefix no triple in the graph uses. Take the prefix
    // set from the Turtle source instead — those are the document's own bindings.
    let src_prefixes = scan_turtle_prefixes(&buf);
    // …and it BECOMES the formal prefix map, which is what a functional-syntax or
    // OBO write declares. A CONSTRUCT's output carries the QUERY's prefixes, and
    // EFO's `components/gwas_import.owl` IS that graph re-serialised — so
    // `gwas_trait:` and the query's `xml:` rebinding have to reach the `Prefix(…)`
    // block.
    //
    // The source's declarations REPLACE the map rather than adding to it, because a
    // Turtle input's prefix map is its `@prefix` lines over the five builtin
    // bindings, and nothing else. Merging into owlmake's OBO-family default map
    // would put `dc`, `terms`, `obo` and `oboInOwl` into every functional file
    // written from a Turtle source — `gwas_import.owl` would declare `dc` and
    // `terms`, which no triple in the graph uses. Replacing also lets the source's
    // own binding win, so the query's `xml:` rebinding survives.
    {
        let mut pm = horned_owl::curie::PrefixMapping::default();
        for (p, ns) in [
            ("owl", "http://www.w3.org/2002/07/owl#"),
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
            ("xsd", "http://www.w3.org/2001/XMLSchema#"),
            ("xml", "http://www.w3.org/XML/1998/namespace"),
        ] {
            let _ = pm.add_prefix(p, ns);
        }
        for (p, ns) in &src_prefixes {
            let _ = pm.add_prefix(p, ns);
        }
        model.prefixes = pm;
    }
    // The `idspace:` set is the document's prefixes less the namespaces no
    // idspace may name, each namespace once — as for an RDF/XML source's
    // `xmlns:` bindings.
    let mut idspaces: Vec<(String, String)> = Vec::new();
    for (p, ns) in &src_prefixes {
        if !p.is_empty() && crate::io::idspace_namespace(ns) && !idspaces.iter().any(|(_, n)| n == ns) {
            idspaces.push((p.clone(), ns.clone()));
        }
    }
    model.idspaces = idspaces;
    model.rdf_prefixes = src_prefixes;
    // An untyped literal in Turtle or N-Triples is an `xsd:string`.
    model.plain_literals_typed = true;
    Ok(model)
}

/// The `@prefix p: <ns> .` (and SPARQL-style `PREFIX p: <ns>`) declarations of a
/// Turtle document, in source order. Only the document's own directives count:
/// the same text inside a string literal, an IRI or a comment declares nothing
/// (RO quotes Turtle that binds `in_taxon:` and `part_of:` in its annotations). A
/// document that declares none — MONDO's `skos.ttl` is plain triples with full
/// IRIs — yields an empty set, which is exactly right: it contributes no prefixes
/// to the merge.
fn scan_turtle_prefixes(bytes: &[u8]) -> Vec<(String, String)> {
    let start = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
    let mut out = Vec::new();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            // A comment runs to the end of its line.
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'<' => i = skip_iri(bytes, i),
            b'"' | b'\'' => i = skip_string(bytes, i),
            // An escaped character of a local name (`ex:o\'brien`) is part of it.
            b'\\' => i += 2,
            _ => {
                let token_start = i == start || matches!(bytes[i - 1], b' ' | b'\t' | b'\r' | b'\n' | b'.' | b'>');
                match token_start.then(|| prefix_directive(&bytes[i..])).flatten() {
                    Some((name, namespace, len)) => {
                        out.push((name, namespace));
                        i += len;
                    }
                    None => i += 1,
                }
            }
        }
    }
    out
}

/// The index just past the IRI `<…>` that starts at `i`.
fn skip_iri(bytes: &[u8], i: usize) -> usize {
    bytes[i + 1..].iter().position(|&c| c == b'>').map_or(bytes.len(), |p| i + p + 2)
}

/// The index just past the string literal that starts at `i`: `"…"` or `'…'`,
/// or the long forms `"""…"""` and `'''…'''`, which may span lines and hold
/// the quote character itself. A backslash escapes the character after it.
fn skip_string(bytes: &[u8], i: usize) -> usize {
    let q = bytes[i];
    let delimiter: &[u8] = if bytes[i..].starts_with(&[q, q, q]) { &bytes[i..i + 3] } else { &bytes[i..i + 1] };
    let mut j = i + delimiter.len();
    while j < bytes.len() {
        if bytes[j] == b'\\' {
            j += 2;
        } else if bytes[j..].starts_with(delimiter) {
            return j + delimiter.len();
        } else {
            j += 1;
        }
    }
    bytes.len()
}

/// The prefix directive `rest` starts with — `@prefix p: <ns>`, or `PREFIX p:
/// <ns>` in any case — as its name, its namespace and the bytes it takes.
fn prefix_directive(rest: &[u8]) -> Option<(String, String, usize)> {
    let keyword = if rest.starts_with(b"@prefix") {
        7
    } else if rest.len() >= 6 && rest[..6].eq_ignore_ascii_case(b"prefix") {
        6
    } else {
        return None;
    };
    // The keyword is a token of its own: `prefix:x` is a prefixed name.
    let skip_space = |mut j: usize| -> Option<usize> {
        while rest.get(j)?.is_ascii_whitespace() {
            j += 1;
        }
        Some(j)
    };
    if !rest.get(keyword)?.is_ascii_whitespace() {
        return None;
    }
    // The prefix NAME ends at its colon; the colons inside the namespace IRI
    // come later. An empty name is the default `@prefix :`.
    let name_start = skip_space(keyword)?;
    let colon = name_start + rest[name_start..].iter().position(|&c| c == b':' || c.is_ascii_whitespace())?;
    if rest[colon] != b':' {
        return None;
    }
    let open = skip_space(colon + 1)?;
    if rest[open] != b'<' {
        return None;
    }
    let close = open + 1 + rest[open + 1..].iter().position(|&c| c == b'>')?;
    Some((
        String::from_utf8_lossy(&rest[name_start..colon]).into_owned(),
        String::from_utf8_lossy(&rest[open + 1..close]).into_owned(),
        close + 1,
    ))
}
