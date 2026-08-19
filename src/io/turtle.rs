//! Turtle support, bridged through the oxigraph triple store: the ontology is
//! moved between Turtle and RDF/XML (which horned-owl reads/writes) via an
//! in-memory store. This reuses the pure-Rust oxrdf serializers.

use std::io::{BufRead, Write};

use anyhow::{anyhow, Result};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::GraphNameRef;
use oxigraph::store::Store;

use crate::io::Format;
use crate::model::Model;

/// Write a model as Turtle.
pub fn save<W: Write>(model: &Model, writer: &mut W) -> Result<()> {
    save_as(model, writer, RdfFormat::Turtle)
}

/// [`save`] in an arbitrary line-based RDF syntax (Turtle or N-Triples).
pub fn save_as<W: Write>(model: &Model, writer: &mut W, fmt: RdfFormat) -> Result<()> {
    let mut rdf = Vec::new();
    crate::io::write_to_ref(model, &mut rdf, Format::RdfXml)?;
    let store = Store::new().map_err(|e| anyhow!("store: {e}"))?;
    store
        .load_from_slice(RdfParser::from_format(RdfFormat::RdfXml), &rdf)
        .map_err(|e| anyhow!("loading triples: {e}"))?;
    store
        .dump_graph_to_writer(GraphNameRef::DefaultGraph, fmt, writer)
        .map_err(|e| anyhow!("serializing {fmt:?}: {e}"))?;
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
    let store = Store::new().map_err(|e| anyhow!("store: {e}"))?;
    store
        .load_from_slice(RdfParser::from_format(fmt), &buf)
        .map_err(|e| anyhow!("parsing Turtle: {e}"))?;
    add_missing_class_expression_types(&store)?;
    let mut rdf = Vec::new();
    store
        .dump_graph_to_writer(GraphNameRef::DefaultGraph, RdfFormat::RdfXml, &mut rdf)
        .map_err(|e| anyhow!("re-serializing as RDF/XML: {e}"))?;
    let mut model = crate::io::load_from(std::io::Cursor::new(rdf), Format::RdfXml)?;
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
    model.rdf_prefixes = src_prefixes.clone();
    model.idspaces = src_prefixes;
    Ok(model)
}

/// Type an anonymous class expression that the document left untyped.
///
/// `rdfs:subClassOf [ owl:onProperty …; owl:someValuesFrom … ]` with no
/// `rdf:type owl:Restriction` is legal RDF, but horned-owl's parser needs the type
/// stated and drops the axiom without it — so the type is inferred here from the
/// characteristic predicates. MONDO's `mirror/ncbigene.owl`, which owlmake's own
/// `arq` CONSTRUCT builds over the downloaded N-Triples mirror ([`crate::arq`]),
/// writes every taxon restriction that way, and the 756
/// `SubClassOf(<ncbigene/…> ObjectSomeValuesFrom(RO_0002162 NCBITaxon_…))` axioms
/// it carries would otherwise not reach `imports/merged_import.owl`.
fn add_missing_class_expression_types(store: &Store) -> Result<()> {
    use oxigraph::model::{NamedNodeRef, Quad, Term};

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    // Predicate → the `rdf:type` its subject must carry. `owl:onProperty` marks a
    // restriction; the set operators mark an anonymous class.
    let rules: [(&str, &str); 5] = [
        ("onProperty", "Restriction"),
        ("unionOf", "Class"),
        ("intersectionOf", "Class"),
        ("complementOf", "Class"),
        ("oneOf", "Class"),
    ];
    let type_pred = NamedNodeRef::new(RDF_TYPE).map_err(|e| anyhow!("{e}"))?;
    let mut missing: Vec<Quad> = Vec::new();
    for (pred, ty) in rules {
        let pred_iri = format!("{OWL}{pred}");
        let type_iri = format!("{OWL}{ty}");
        let p = NamedNodeRef::new(&pred_iri).map_err(|e| anyhow!("{e}"))?;
        let t = NamedNodeRef::new(&type_iri).map_err(|e| anyhow!("{e}"))?;
        for q in store.quads_for_pattern(None, Some(p), None, None) {
            let q = q.map_err(|e| anyhow!("scanning triples: {e}"))?;
            let subj = q.subject;
            let already = store
                .quads_for_pattern(Some(subj.as_ref()), Some(type_pred), None, None)
                .filter_map(|r| r.ok())
                .any(|r| matches!(&r.object, Term::NamedNode(n) if n.as_str().starts_with(OWL)));
            if !already {
                missing.push(Quad::new(subj, t, t, GraphNameRef::DefaultGraph));
            }
        }
    }
    for q in missing {
        let subject = q.subject;
        store
            .insert(&Quad::new(subject, type_pred, q.object, GraphNameRef::DefaultGraph))
            .map_err(|e| anyhow!("inserting rdf:type: {e}"))?;
    }
    Ok(())
}

/// The `@prefix p: <ns> .` (and SPARQL-style `PREFIX p: <ns>`) declarations of a
/// Turtle document, in source order. A document that declares none — MONDO's
/// `skos.ttl` is plain triples with full IRIs — yields an empty set, which is
/// exactly right: it contributes no prefixes to the merge.
fn scan_turtle_prefixes(bytes: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        let rest = if let Some(r) = t.strip_prefix("@prefix") {
            r
        } else if t.len() >= 6 && t[..6].eq_ignore_ascii_case("prefix") {
            &t[6..]
        } else {
            continue;
        };
        let rest = rest.trim_start();
        // The prefix NAME comes first, so the first `:` ends it — the colons inside
        // the namespace IRI come later. An empty name is the default `@prefix :`.
        let Some(colon) = rest.find(':') else { continue };
        let name = rest[..colon].trim().to_string();
        let Some(open) = rest[colon..].find('<') else { continue };
        let after = &rest[colon + open + 1..];
        let Some(close) = after.find('>') else { continue };
        out.push((name, after[..close].to_string()));
    }
    out
}
