//! `arq` — the SPARQL CLI, evaluating a query over RDF files on oxigraph.
//!
//! MONDO's `mirror-ncbigene` recipe is the reason this exists:
//!
//! ```text
//! arq --data=mirror/ncbi_gene.nt --query=../sparql/construct/construct-ncbigene.sparql > tmp/mirror-ncbigene.owl
//! ```
//!
//! Refreshing an import that is built this way means evaluating a CONSTRUCT over
//! a downloaded N-Triples mirror, so `om make` has to do it itself.
//!
//! Scope is what those recipes use: `--data` (repeatable), `--query`, and the
//! CONSTRUCT/SELECT/ASK result forms. A CONSTRUCT is written as Turtle by
//! default; a recipe that redirects it into a `.owl` mirror still loads, because
//! the ontology loader sniffs a `.owl` file's leading bytes rather than trusting
//! the extension.
//!
//! The exact Turtle layout is not load-bearing: the output is merged into
//! `mirror/merged.owl` and then BOT-extracted, so it is re-parsed as RDF and its
//! blank nodes are renumbered regardless. Semantic equivalence is what has to
//! hold.

use std::io::Write;

use anyhow::{anyhow, bail, Result};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::GraphNameRef;
use oxigraph::sparql::QueryResults;
use oxigraph::store::Store;

/// Entry point for `owlmake arq …`. Like the other bundled CLIs it takes the
/// arguments after the command word and returns a process exit code: 0 on
/// success, 1 with a diagnostic on stderr.
pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("arq: {e}");
            1
        }
    }
}

/// Guess the RDF syntax from a file extension, for a `--data` given without an
/// explicit `--syntax`.
fn format_for(path: &str) -> Result<RdfFormat> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    Ok(match ext.as_str() {
        "nt" => RdfFormat::NTriples,
        "ttl" | "turtle" => RdfFormat::Turtle,
        "nq" => RdfFormat::NQuads,
        "trig" => RdfFormat::TriG,
        "n3" => RdfFormat::N3,
        "rdf" | "owl" | "xml" | "rdfxml" => RdfFormat::RdfXml,
        other => bail!("cannot infer RDF syntax from extension {other:?} ({path})"),
    })
}

fn run(args: &[String]) -> Result<()> {
    let mut data: Vec<String> = Vec::new();
    let mut query_file: Option<String> = None;
    let mut query_text: Option<String> = None;
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        let mut take_next = |i: &mut usize| -> Result<String> {
            *i += 1;
            args.get(*i).cloned().ok_or_else(|| anyhow!("{a} expects a value"))
        };
        match a {
            _ if a.starts_with("--data=") => data.push(a["--data=".len()..].to_string()),
            _ if a.starts_with("--query=") => query_file = Some(a["--query=".len()..].to_string()),
            // Separated-value spellings: `--graph` also names data, and the query
            // file is spelled `--query`, `-q` or `--file`.
            "--data" | "--graph" => data.push(take_next(&mut i)?),
            "--query" | "-q" | "--file" => query_file = Some(take_next(&mut i)?),
            "-e" | "--exec" => query_text = Some(take_next(&mut i)?),
            "--version" => {
                println!("owlmake arq (Apache Jena arq-compatible)");
                return Ok(());
            }
            // Result-format and verbosity flags recipes never vary from the default.
            "--results" | "--out" | "--syntax" | "--time" | "--quiet" | "-v" => {}
            _ if a.starts_with('-') => {}
            // A bare argument is the query file when none was named.
            _ if query_file.is_none() && query_text.is_none() => {
                query_file = Some(a.to_string())
            }
            _ => {}
        }
        i += 1;
    }

    let sparql = match (&query_file, &query_text) {
        (Some(f), _) => std::fs::read_to_string(f)
            .map_err(|e| anyhow!("reading query {f}: {e}"))?,
        (None, Some(t)) => t.clone(),
        (None, None) => bail!("no query given (--query FILE or -e TEXT)"),
    };

    let store = Store::new().map_err(|e| anyhow!("store init: {e}"))?;
    for d in &data {
        let fmt = format_for(d)?;
        let f = std::fs::File::open(d).map_err(|e| anyhow!("opening --data {d}: {e}"))?;
        store
            .load_from_reader(RdfParser::from_format(fmt), std::io::BufReader::new(f))
            .map_err(|e| anyhow!("loading {d}: {e}"))?;
    }

    let out = std::io::stdout();
    let mut w = out.lock();
    match store.query(sparql.as_str()).map_err(|e| anyhow!("query error: {e}"))? {
        // A CONSTRUCT/DESCRIBE is serialised as Turtle by default. Round-trip the
        // constructed graph through a scratch store so oxigraph's Turtle writer can
        // emit it with prefixes, ready for the merge step that reads it back.
        QueryResults::Graph(triples) => {
            let tmp = Store::new().map_err(|e| anyhow!("store init: {e}"))?;
            for t in triples {
                let t = t.map_err(|e| anyhow!("constructed triple: {e}"))?;
                tmp.insert(t.as_ref().in_graph(GraphNameRef::DefaultGraph))
                    .map_err(|e| anyhow!("collecting constructed triple: {e}"))?;
            }
            let mut buf = Vec::new();
            tmp.dump_graph_to_writer(GraphNameRef::DefaultGraph, RdfFormat::Turtle, &mut buf)
                .map_err(|e| anyhow!("serialising Turtle: {e}"))?;
            w.write_all(&buf)?;
        }
        QueryResults::Boolean(b) => {
            writeln!(w, "{b}")?;
        }
        QueryResults::Solutions(solutions) => {
            // Tab-separated, the shape recipes consume when they pipe a SELECT.
            let mut printed_header = false;
            for s in solutions {
                let s = s.map_err(|e| anyhow!("solution: {e}"))?;
                if !printed_header {
                    let cols: Vec<String> =
                        s.variables().iter().map(|v| format!("?{}", v.as_str())).collect();
                    writeln!(w, "{}", cols.join("\t"))?;
                    printed_header = true;
                }
                let vals: Vec<String> = s
                    .iter()
                    .map(|(_, term)| term.to_string())
                    .collect();
                writeln!(w, "{}", vals.join("\t"))?;
            }
        }
    }
    w.flush()?;
    Ok(())
}
