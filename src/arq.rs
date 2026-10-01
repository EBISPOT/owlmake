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
    if let Some(pretty) = jena_construct(&store, &sparql)? {
        w.write_all(&pretty)?;
        w.flush()?;
        return Ok(());
    }
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


/// A term of a CONSTRUCT template: a variable, a fixed IRI, or one of the
/// template's own blank nodes (a fresh node is minted for it per solution).
#[derive(Debug, Clone)]
enum TTerm {
    Var(String),
    Iri(String),
    Bnode(usize),
}

/// Parse a CONSTRUCT template into triples, in the order they are stated. A
/// bracketed blank-node block emits its own triples first, then the triple
/// that references it. One level of nesting is enough for every recipe this
/// serves; `None` for anything else (a literal, a collection, deeper nesting).
fn parse_template(block: &str, prefixes: &[(String, String)]) -> Option<Vec<(TTerm, TTerm, TTerm)>> {
    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let expand = |t: &str| -> Option<TTerm> {
        if let Some(v) = t.strip_prefix('?').or_else(|| t.strip_prefix('$')) {
            return Some(TTerm::Var(v.to_string()));
        }
        if let Some(i) = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
            return Some(TTerm::Iri(i.to_string()));
        }
        if t == "a" {
            return Some(TTerm::Iri(RDF_TYPE.to_string()));
        }
        let (name, local) = t.split_once(':')?;
        let (_, ns) = prefixes.iter().find(|(n, _)| n == name)?;
        Some(TTerm::Iri(format!("{ns}{local}")))
    };
    // Tokenize: leading `[`s and trailing `]`/`;`/`,`/`.` runs split off each
    // whitespace token, in order.
    let mut tokens: Vec<String> = Vec::new();
    for raw in block.split_whitespace() {
        let mut t = raw;
        while let Some(rest) = t.strip_prefix('[') {
            tokens.push("[".into());
            t = rest;
        }
        let mut ends: Vec<char> = Vec::new();
        while t.len() > 1 || (t.len() == 1 && matches!(t.chars().next(), Some('.' | ';' | ',' | ']'))) {
            let c = t.chars().last().unwrap();
            if matches!(c, '.' | ';' | ',' | ']') && t.len() > 1 {
                ends.push(c);
                t = &t[..t.len() - 1];
            } else if t.len() == 1 && matches!(c, '.' | ';' | ',' | ']') {
                ends.push(c);
                t = "";
                break;
            } else {
                break;
            }
        }
        if !t.is_empty() {
            if t.starts_with('"') {
                return None;
            }
            tokens.push(t.to_string());
        }
        for c in ends.into_iter().rev() {
            tokens.push(c.to_string());
        }
    }

    let mut out: Vec<(TTerm, TTerm, TTerm)> = Vec::new();
    let mut bnodes = 0usize;
    let mut i = 0usize;
    // subject predicateObjectList '.' …, with `[ … ]` as an object only.
    while i < tokens.len() {
        let subject = expand(&tokens[i])?;
        i += 1;
        loop {
            let predicate = expand(tokens.get(i)?)?;
            i += 1;
            loop {
                let tok = tokens.get(i)?;
                let object = if tok == "[" {
                    let b = bnodes;
                    bnodes += 1;
                    i += 1;
                    // The block's own predicate-object list, emitted before the
                    // enclosing triple.
                    loop {
                        let p = expand(tokens.get(i)?)?;
                        i += 1;
                        let o = {
                            let t = tokens.get(i)?;
                            if t == "[" {
                                return None;
                            }
                            expand(t)?
                        };
                        i += 1;
                        out.push((TTerm::Bnode(b), p, o));
                        match tokens.get(i).map(String::as_str) {
                            Some(";") => {
                                i += 1;
                                if tokens.get(i).map(String::as_str) == Some("]") {
                                    i += 1;
                                    break;
                                }
                            }
                            Some("]") => {
                                i += 1;
                                break;
                            }
                            _ => return None,
                        }
                    }
                    TTerm::Bnode(b)
                } else {
                    let o = expand(tok)?;
                    i += 1;
                    o
                };
                out.push((subject.clone(), predicate.clone(), object));
                if tokens.get(i).map(String::as_str) == Some(",") {
                    i += 1;
                    continue;
                }
                break;
            }
            match tokens.get(i).map(String::as_str) {
                Some(";") => {
                    i += 1;
                    // A trailing `;` before `.` is legal.
                    if tokens.get(i).map(String::as_str) == Some(".") {
                        i += 1;
                        break;
                    }
                }
                Some(".") => {
                    i += 1;
                    break;
                }
                None => break,
                _ => return None,
            }
        }
    }
    Some(out)
}

/// The list a `VALUES ?v { <iri> … }` block states, with its variable, in
/// order. `None` when the query has no such block or it holds anything but
/// IRIs.
fn values_list(sparql: &str) -> Option<(String, Vec<String>)> {
    let upper = sparql.to_ascii_uppercase();
    // The keyword stands alone: a name that merely contains the letters (an
    // `owl:someValuesFrom` in the template) is not it.
    let mut at = None;
    let mut from = 0usize;
    while let Some(rel) = upper[from..].find("VALUES") {
        let i = from + rel;
        from = i + 6;
        let before_ok = i == 0
            || !upper.as_bytes()[i - 1].is_ascii_alphanumeric()
                && upper.as_bytes()[i - 1] != b'_';
        let after_ok = upper
            .as_bytes()
            .get(i + 6)
            .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_');
        if before_ok && after_ok {
            at = Some(i);
            break;
        }
    }
    let rest = &sparql[at? + 6..];
    let var = rest.split_whitespace().next()?;
    let var = var.strip_prefix('?').or_else(|| var.strip_prefix('$'))?.to_string();
    let open = rest.find('{')?;
    let close = rest[open..].find('}')? + open;
    let mut list = Vec::new();
    for t in rest[open + 1..close].split_whitespace() {
        let iri = t.strip_prefix('<')?.strip_suffix('>')?;
        list.push(iri.to_string());
    }
    Some((var, list))
}

/// Evaluate a CONSTRUCT whose WHERE is driven by a `VALUES` list and write the
/// graph in the pretty Turtle layout: solutions in the list's order, one fresh
/// blank node per bracketed template block per solution, subjects read back in
/// the graph's slot order. `None` when the query is not this shape, and the
/// caller keeps the plain serializer.
fn jena_construct(store: &Store, sparql: &str) -> Result<Option<Vec<u8>>> {
    use crate::io::jena_ttl::{write_pretty, Dialect, JNode, JenaGraph};
    use oxigraph::model::Term;
    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
    let upper = sparql.to_ascii_uppercase();
    let Some(kw) = upper.find("CONSTRUCT") else { return Ok(None) };
    // The template block, brace-matched.
    let Some(open) = sparql[kw..].find('{').map(|o| o + kw) else { return Ok(None) };
    let mut depth = 0usize;
    let mut close = open;
    for (k, c) in sparql[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = open + k;
                    break;
                }
            }
            _ => {}
        }
    }
    if close <= open {
        return Ok(None);
    }
    let dbg = std::env::var_os("OM_SCAN_DEBUG").is_some();
    let prefixes = crate::sparql::query_prefixes(sparql);
    let Some(template) = parse_template(&sparql[open + 1..close], &prefixes) else {
        if dbg { eprintln!("[arq-ttl] template parse failed"); }
        return Ok(None);
    };
    if dbg { eprintln!("[arq-ttl] template={template:?}"); }
    let Some((dvar, values)) = values_list(sparql) else {
        if dbg { eprintln!("[arq-ttl] no VALUES"); }
        return Ok(None) };
    if dbg { eprintln!("[arq-ttl] values var={dvar} n={}", values.len()); }
    let mut rank: std::collections::HashMap<&str, usize> = Default::default();
    for (k, v) in values.iter().enumerate() {
        rank.entry(v.as_str()).or_insert(k);
    }
    // The same solutions, as a SELECT.
    let select = format!("{}SELECT *{}", &sparql[..kw], &sparql[close + 1..]);
    let QueryResults::Solutions(solutions) =
        store.query(select.as_str()).map_err(|e| anyhow!("query error: {e}"))?
    else {
        return Ok(None);
    };
    let vars: Vec<String> =
        solutions.variables().iter().map(|v| v.as_str().to_string()).collect();
    let Some(dcol) = vars.iter().position(|v| *v == dvar) else { return Ok(None) };
    let mut rows: Vec<Vec<Option<Term>>> = Vec::new();
    for sol in solutions {
        let sol = sol.map_err(|e| anyhow!("solution: {e}"))?;
        rows.push((0..vars.len()).map(|i| sol.get(i).cloned()).collect());
    }
    // Group rows under the driving value, groups in the list's order.
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut group_of: std::collections::HashMap<String, usize> = Default::default();
    for (r, row) in rows.iter().enumerate() {
        let Some(Some(Term::NamedNode(n))) = row.get(dcol) else { return Ok(None) };
        let key = n.as_str();
        let g = match group_of.get(key) {
            Some(&g) => g,
            None => {
                let Some(&k) = rank.get(key) else { return Ok(None) };
                groups.push((k, Vec::new()));
                group_of.insert(key.to_string(), groups.len() - 1);
                groups.len() - 1
            }
        };
        groups[g].1.push(r);
    }
    groups.sort_by_key(|(k, _)| *k);

    let mut graph = JenaGraph::default();
    let mut minted = 0usize;
    for (_, group) in &groups {
        for &r in group {
            let row = &rows[r];
            // The blank nodes this solution mints, by template block.
            let mut fresh: std::collections::HashMap<usize, String> = Default::default();
            let mut node_of = |t: &TTerm| -> Option<Option<JNode>> {
                match t {
                    TTerm::Iri(u) => Some(Some(JNode::Iri(u.clone()))),
                    TTerm::Bnode(b) => {
                        let label = fresh.entry(*b).or_insert_with(|| {
                            let l = format!("b{minted}");
                            minted += 1;
                            l
                        });
                        Some(Some(JNode::Bnode(label.clone())))
                    }
                    TTerm::Var(v) => {
                        let col = vars.iter().position(|c| c == v)?;
                        Some(match &row[col] {
                            Some(Term::NamedNode(n)) => {
                                Some(JNode::Iri(n.as_str().to_string()))
                            }
                            Some(Term::Literal(l)) => Some(JNode::Lit {
                                lex: l.value().to_string(),
                                lang: l.language().map(|s| s.to_string()),
                                dt: (l.language().is_none()
                                    && l.datatype().as_str() != XSD_STRING)
                                    .then(|| l.datatype().as_str().to_string()),
                            }),
                            None => None,
                            _ => return None,
                        })
                    }
                }
            };
            for (ts, tp, to) in &template {
                let (Some(s), Some(p), Some(o)) = (match node_of(ts) {
                    Some(x) => x,
                    None => return Ok(None),
                }, match node_of(tp) {
                    Some(x) => x,
                    None => return Ok(None),
                }, match node_of(to) {
                    Some(x) => x,
                    None => return Ok(None),
                }) else {
                    continue;
                };
                let JNode::Iri(p) = p else { continue };
                if matches!(s, JNode::Lit { .. }) {
                    continue;
                }
                graph.add(s, &p, o);
            }
        }
    }
    if graph.is_empty() {
        if dbg { eprintln!("[arq-ttl] empty graph"); }
        return Ok(None);
    }
    let w = write_pretty(&graph, &prefixes, Dialect::Arq);
    if dbg && w.is_none() { eprintln!("[arq-ttl] writer refused the graph"); }
    Ok(w)
}
