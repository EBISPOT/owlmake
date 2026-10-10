//! Turtle and N-Triples input, and line-based RDF output. A document is read
//! by [`parse`] into the statements the RDF reader translates; output goes
//! through oxigraph's serializers. Turtle output is laid out by `owlapi_ttl`,
//! which falls back to [`save_plain`].

mod iri;
mod parse;

use std::collections::HashMap;
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

/// [`load`] for a Turtle or N-Triples document, `fmt` naming which in an
/// error: N-Triples is read as the Turtle it is a subset of. The document's
/// relative IRIs resolve against its own IRI, its anonymous individuals are
/// numbered on from the run's counter, and its unlabelled blank nodes from
/// the run's count of them.
pub fn load_as<R: BufRead>(mut reader: R, fmt: RdfFormat) -> Result<Model> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    let syntax = if fmt == RdfFormat::NTriples { "N-Triples" } else { "Turtle" };
    let trace = std::env::var_os("OM_RDF_TRACE").is_some();
    let mut statements = crate::io::rdfxml::Statements::new(crate::io::anon_counter(), trace);
    let mut unlabelled = crate::io::unlabelled_nodes();
    let base = crate::io::document_iri();
    let parsed = parse::read(&String::from_utf8_lossy(&buf), base.as_deref(), &mut unlabelled, &mut statements);
    crate::io::set_unlabelled_nodes(unlabelled);
    parsed.map_err(|e| anyhow!("parsing {syntax}: {e}"))?;
    let read = statements.finish();
    let src_prefixes = read.prefixes.clone();
    let mut model = crate::io::model_from_statements(read, syntax)?;
    // The document's prefix declarations ARE the formal prefix map, which is
    // what a functional-syntax or OBO write declares. A CONSTRUCT's output
    // carries the QUERY's prefixes, and EFO's `components/gwas_import.owl` IS
    // that graph re-serialised — so `gwas_trait:` and the query's `xml:`
    // rebinding have to reach the `Prefix(…)` block.
    //
    // The declarations REPLACE the map rather than adding to it, because a
    // Turtle input's prefix map is its `@prefix` lines over the five builtin
    // bindings, and nothing else. Merging into owlmake's OBO-family default map
    // would put `dc`, `terms`, `obo` and `oboInOwl` into every functional file
    // written from a Turtle source — `gwas_import.owl` would declare `dc` and
    // `terms`, which no triple in the graph uses. Replacing also lets the
    // source's own binding win, so the query's `xml:` rebinding survives. A
    // document that declares none — MONDO's `skos.ttl` is plain triples with
    // full IRIs — keeps the five.
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
