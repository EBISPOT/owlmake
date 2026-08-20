//! `query` — run a SPARQL SELECT/ASK/CONSTRUCT over the ontology.
//!
//! Supports repeatable `--query <FILE> <OUTPUT>`, `--queries <FILE>...`, single
//! `--query`/`--query-string`, multiple `--update <FILE>` (SPARQL UPDATE applied
//! to the model), result `--format` (tsv/csv/json), and `--output-dir`.
//! Evaluation is always in memory; `--tdb` and its companions additionally
//! materialize an on-disk dataset, and `--tdb true` also orders a solution table
//! with no `ORDER BY` by each term's first appearance in the `--input` file.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context};
use clap::Args as ClapArgs;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::GraphNameRef;
use oxigraph::store::Store;

use crate::io::Format;
use crate::sparql::{query_prefixes, QueryOutput, QueryTable, Queryable};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether `--update`'s round trip keeps the document format's prefixes.
///
/// An update rebuilds the document from its triples, so a prefix no triple uses
/// survives only if the document format's prefix map is carried across. Which
/// way a repo wants it is resolved at INGEST and recorded as
/// `Plan::emulate_robot_version` — execution sets this once, from the plan
/// (`build::set_robot_behaviours`). There is deliberately no environment
/// override: it decides artefact bytes.
static UPDATE_KEEPS_PREFIXES: AtomicBool = AtomicBool::new(true);

/// Set whether `--update` keeps the document's prefixes — see the static above.
pub fn set_update_keeps_prefixes(on: bool) {
    UPDATE_KEEPS_PREFIXES.store(on, Ordering::Relaxed);
}

fn update_keeps_prefixes() -> bool {
    UPDATE_KEEPS_PREFIXES.load(Ordering::Relaxed)
}

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Run a SPARQL query. The two-value form takes a query `<FILE>` and an
    /// `<OUTPUT>` file (repeatable); a lone `--query <FILE>` writes the
    /// result to --output / stdout instead.
    #[arg(short = 'q', long, value_names = ["FILE", "OUTPUT"], num_args = 1..=2, conflicts_with = "query_string")]
    pub query: Vec<PathBuf>,
    /// Inline SPARQL query string.
    #[arg(long)]
    pub query_string: Option<String>,
    /// Run a SPARQL CONSTRUCT query (REPEATABLE), writing each constructed RDF
    /// graph to its OUTPUT (or, with a lone trailing FILE, to --output /
    /// stdout). The serialization format follows --format when it names an RDF
    /// syntax (ttl/turtle, nt/ntriples, rdfxml/owl, jsonld), else the OUTPUT
    /// extension, else Turtle.
    #[arg(short = 'c', long = "construct", value_names = ["FILE", "OUTPUT"], num_args = 1..=2)]
    pub construct: Vec<PathBuf>,
    /// Run a SPARQL SELECT query (REPEATABLE), writing each result table to its
    /// OUTPUT (or, with a lone trailing FILE, to --output / stdout). Equivalent
    /// to --query for a SELECT. EFO's `all_reports` and CL's `custom_reports`
    /// pass seven and five pairs.
    #[arg(short = 's', long = "select", value_names = ["FILE", "OUTPUT"], num_args = 1..=2)]
    pub select: Vec<PathBuf>,
    /// Run one or more SPARQL queries `<FILE> <OUTPUT>` (repeatable). Each pair
    /// runs the query in FILE and writes its result table to OUTPUT in the
    /// chosen --format.
    #[arg(long = "query-pair", value_names = ["FILE", "OUTPUT"], num_args = 2)]
    pub query_pairs: Vec<PathBuf>,
    /// Verify/run one or more SPARQL query files (repeatable). Results go to
    /// --output-dir (one file per query) or stdout.
    #[arg(short = 'Q', long = "queries", num_args = 1..)]
    pub queries: Vec<PathBuf>,
    /// Apply one or more SPARQL UPDATE files to the model before output
    /// (repeatable). The updated model is what gets passed along the chain and
    /// saved by --output.
    #[arg(short = 'u', long = "update", num_args = 1..)]
    pub update: Vec<PathBuf>,
    /// Output file for a single query (defaults to stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Directory for query outputs. Used by --queries.
    #[arg(short = 'O', long = "output-dir")]
    pub output_dir: Option<PathBuf>,
    /// Result format: csv, tsv, json, txt, or an RDF syntax for CONSTRUCT. The
    /// default is EMPTY, meaning "not given": an absent `--format` means "derive
    /// it", from the OUTPUT file's extension and then from the query form (see
    /// [`resolve_result_format`]). A hard `tsv` default would silently write TSV
    /// into every `.csv` output. `allow_hyphen_values`, because the next token is
    /// taken whatever it looks like, and OBA's build writes `--format --csv` — a
    /// typo whose `tmp/pre_seed.txt` is expected to come out as CSV.
    #[arg(short, long, default_value = "", allow_hyphen_values = true)]
    pub format: String,
    /// Load imports as named graphs and set the DEFAULT graph to the union of
    /// them all, so the query sees the root ontology plus its whole import
    /// closure; without the flag it sees only the root's own axioms. CL, UBERON
    /// and OBA all use `query -f tsv --use-graphs true` for their SPARQL exports.
    #[arg(short = 'g', long = "use-graphs", num_args = 1, default_missing_value = "true")]
    pub use_graphs: Option<bool>,
    /// Load RDF onto disk via TDB. Accepted for compatibility; owlmake
    /// always evaluates in memory.
    #[arg(short = 't', long = "tdb", num_args = 1, default_missing_value = "true")]
    pub tdb: Option<bool>,
    /// Keep the TDB directory. No-op (no TDB).
    #[arg(short = 'k', long = "keep-tdb-mappings", num_args = 1, default_missing_value = "true")]
    pub keep_tdb_mappings: Option<bool>,
    /// TDB directory. No-op (no TDB).
    #[arg(short = 'd', long = "tdb-directory")]
    pub tdb_directory: Option<PathBuf>,
    /// Create a TDB directory without querying. Accepted for compatibility;
    /// owlmake always evaluates in memory and never creates a TDB store, so
    /// this is a no-op.
    #[arg(short = 'C', long = "create-tdb", num_args = 1, default_missing_value = "true")]
    pub create_tdb: Option<bool>,
    /// Store intermediate --update results in a temporary file to reduce heap
    /// usage. TDB-only; accepted for compatibility and a no-op in the in-
    /// memory engine.
    #[arg(short = 'y', long = "temporary-file", num_args = 1, default_missing_value = "true")]
    pub temporary_file: Option<bool>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

/// How to serialize a SPARQL result table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ResultFormat {
    Tsv,
    Csv,
    Json,
    /// The ASK default: a bare `true`/`false` line — no header, no quoting.
    Txt,
}

/// The marker prefixed to a literal's datatype IRI while it sits in the store.
const LITERAL_MASK: &str = "urn:owlmake:lexical:";

/// The datatypes oxigraph's storage layer encodes as a parsed VALUE rather than
/// as a lexical form (`EncodedTerm::{Float,Double,Integer,Decimal,DateTime,…}`),
/// so that `"0.0"^^xsd:float` comes back out of the store as `"0"^^xsd:float` and
/// `"0"^^xsd:short` as `"0"^^xsd:integer`.
fn value_encoded_datatype(dt: &str) -> bool {
    matches!(
        dt.strip_prefix("http://www.w3.org/2001/XMLSchema#"),
        Some(
            "float"
                | "double"
                | "integer"
                | "byte"
                | "short"
                | "int"
                | "long"
                | "unsignedByte"
                | "unsignedShort"
                | "unsignedInt"
                | "unsignedLong"
                | "positiveInteger"
                | "negativeInteger"
                | "nonPositiveInteger"
                | "nonNegativeInteger"
                | "decimal"
                | "dateTime"
                | "dateTimeStamp"
                | "time"
                | "date"
                | "gYearMonth"
                | "gYear"
                | "gMonthDay"
                | "gDay"
                | "gMonth"
                | "duration"
                | "yearMonthDuration"
                | "dayTimeDuration"
        )
    )
}

/// Load `rdf` into `store` with every literal's exact lexical form and datatype
/// intact: a SPARQL UPDATE that never mentions a literal must leave its spelling
/// alone.
///
/// oxigraph stores the datatypes above as parsed values, so the lexical form is
/// whatever its writer chooses to print — `"0.0"^^xsd:float` becomes `"0"`, and
/// `"0"^^xsd:short` loses `short` for `integer`. A literal whose spelling the
/// store would not give back is therefore stored under a MARKED datatype the
/// encoder has no native form for, so it is kept as a lexical form; the marker
/// comes back off the dumped bytes in [`unmask_literals`]. Literals the store
/// does reproduce exactly are left as they are, so SPARQL keeps its numeric and
/// date semantics for every value that is written canonically.
fn load_preserving_literals(store: &Store, rdf: &[u8]) -> anyhow::Result<()> {
    use oxigraph::model::{Literal, NamedNode, Quad, Term};
    use std::collections::HashMap;

    let probe = Store::new().map_err(|e| anyhow!("store init: {e}"))?;
    let subj = NamedNode::new_unchecked(format!("{LITERAL_MASK}probe"));
    let pred = NamedNode::new_unchecked(format!("{LITERAL_MASK}probe"));
    // (datatype, lexical form) -> does the store give the same spelling back?
    let mut preserved: HashMap<(String, String), bool> = HashMap::new();
    let mut roundtrips = |lex: &str, dt: &str| -> anyhow::Result<bool> {
        let k = (dt.to_string(), lex.to_string());
        if let Some(&hit) = preserved.get(&k) {
            return Ok(hit);
        }
        let lit = Literal::new_typed_literal(lex, NamedNode::new_unchecked(dt));
        let q = Quad::new(subj.clone(), pred.clone(), lit, GraphNameRef::DefaultGraph);
        probe.insert(&q).map_err(|e| anyhow!("literal probe: {e}"))?;
        let back = probe
            .iter()
            .next()
            .transpose()
            .map_err(|e| anyhow!("literal probe: {e}"))?;
        let same = match back.as_ref().map(|q| &q.object) {
            Some(Term::Literal(l)) => l.value() == lex && l.datatype().as_str() == dt,
            _ => false,
        };
        probe.clear().map_err(|e| anyhow!("literal probe: {e}"))?;
        preserved.insert(k, same);
        Ok(same)
    };

    for quad in RdfParser::from_format(RdfFormat::RdfXml).for_slice(rdf) {
        let mut quad = quad.map_err(|e| anyhow!("loading ontology triples: {e}"))?;
        if let Term::Literal(l) = &quad.object {
            let dt = l.datatype().as_str().to_string();
            if value_encoded_datatype(&dt) && !roundtrips(l.value(), &dt)? {
                quad.object = Literal::new_typed_literal(
                    l.value(),
                    NamedNode::new_unchecked(format!("{LITERAL_MASK}{dt}")),
                )
                .into();
            }
        }
        store.insert(&quad).map_err(|e| anyhow!("loading ontology triples: {e}"))?;
    }
    Ok(())
}

/// Strip the datatype marker [`load_preserving_literals`] applied, restoring each
/// masked literal's original datatype IRI in the dumped RDF/XML. Only the
/// `rdf:datatype` attribute is rewritten, so a literal whose own text happens to
/// contain the marker is left alone.
fn unmask_literals(dumped: Vec<u8>) -> Vec<u8> {
    let marked = format!("rdf:datatype=\"{LITERAL_MASK}");
    if !dumped.windows(marked.len()).any(|w| w == marked.as_bytes()) {
        return dumped;
    }
    String::from_utf8_lossy(&dumped)
        .replace(marked.as_str(), "rdf:datatype=\"")
        .into_bytes()
}

/// A `GROUP_CONCAT` column: which output column it is, the separator, and the
/// predicate whose objects it concatenates.
struct ConcatCol {
    col: usize,
    sep: String,
    predicate: String,
}

/// Find each `(GROUP_CONCAT(DISTINCT ?v;SEPARATOR="X") AS ?col)` in the query and
/// the predicate that binds `?v` in the WHERE clause, so the concatenated values
/// can be put into the per-subject hash-bucket order released report files carry
/// (computed by `sparql::jena_order`); a different order would rewrite every such
/// cell in every release diff. Only the simple shape MONDO uses is recognised;
/// anything else is left alone.
fn concat_columns(sparql: &str, columns: &[String]) -> Vec<ConcatCol> {
    let mut out = Vec::new();
    let prefixes = query_prefixes(sparql);
    let mut rest = sparql;
    while let Some(i) = rest.to_ascii_uppercase().find("GROUP_CONCAT") {
        let after = &rest[i + "GROUP_CONCAT".len()..];
        let Some(close) = after.find(')') else { break };
        let inner = &after[..close];
        let tail = &after[close..];
        // the aggregated variable
        let var = inner
            .split(|c: char| c == '(' || c == ';' || c.is_whitespace())
            .find(|t| t.starts_with('?'))
            .map(|t| t.trim_start_matches('?').to_string());
        let sep = inner
            .split_once("SEPARATOR")
            .and_then(|(_, r)| r.split_once('"'))
            .and_then(|(_, r)| r.split_once('"'))
            .map(|(v, _)| v.to_string())
            .unwrap_or_else(|| " ".to_string());
        // `AS ?col`
        let col_name = tail
            .split_once(" AS ")
            .or_else(|| tail.split_once(" as "))
            .and_then(|(_, r)| r.split(|c: char| c == ')' || c.is_whitespace()).find(|t| t.starts_with('?')))
            .map(|t| t.trim_start_matches('?').to_string());
        if let (Some(var), Some(col_name)) = (var, col_name) {
            if let Some(col) = columns.iter().position(|c| *c == col_name) {
                if let Some(p) = predicate_binding(sparql, &var, &prefixes) {
                    out.push(ConcatCol { col, sep: sep.clone(), predicate: p });
                }
            }
        }
        rest = &rest[i + "GROUP_CONCAT".len()..];
    }
    out
}

/// The predicate of the first `?s <pred> ?var` pattern binding `var`.
///
/// The pattern is found by TOKEN, not by line shape: `?cls oio:hasDbXref ?xref`
/// binds `?xref` whether it stands alone, is closed by a `.`/`;`, or sits inside
/// an `OPTIONAL { … }` on one line. Punctuation and grouping braces are stripped
/// from each token before it is compared, and the predicate is the nearest
/// preceding token that is neither.
fn predicate_binding(sparql: &str, var: &str, prefixes: &[(String, String)]) -> Option<String> {
    let needle = format!("?{var}");
    // Only the WHERE clause binds; the projection mentions the variable too.
    let body = match sparql.to_ascii_uppercase().find("WHERE") {
        Some(i) => &sparql[i..],
        None => sparql,
    };
    fn clean(t: &str) -> &str {
        t.trim_matches(|c: char| matches!(c, '.' | ';' | ',' | '{' | '}' | '(' | ')'))
    }
    let toks: Vec<&str> = body.split_whitespace().map(clean).filter(|t| !t.is_empty()).collect();
    for (i, t) in toks.iter().enumerate() {
        if *t != needle || i == 0 {
            continue;
        }
        let p = toks[i - 1];
        if let Some(iri) = p.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
            return Some(iri.to_string());
        }
        if let Some((name, local)) = p.split_once(':') {
            if let Some((_, ns)) = prefixes.iter().find(|(n, _)| n == name) {
                return Some(format!("{ns}{local}"));
            }
        }
    }
    None
}

/// Re-order every `GROUP_CONCAT` cell into the per-subject hash-bucket order.
fn apply_jena_concat_order(table: &mut QueryTable, q: &Queryable, cols: &[ConcatCol]) {
    use crate::sparql::jena_order as jo;
    // The renderer reads `tsv_rows` for TSV and `rows` for CSV, so both need it.
    let n = table.rows.len();
    for r in 0..n {
        let Some(subj) = table.rows[r].first().cloned() else { continue };
        let subj = subj.trim_start_matches('<').trim_end_matches('>').to_string();
        let s = jo::node_hash(&subj);
        // The slot a triple takes depends on the WHOLE bunch: every triple with
        // that subject competes for the same slots, so a concatenated value
        // displaced by an `rdfs:label` that hashed to its slot comes out later than
        // its own hash says. Where the subject cannot be read as a bunch at all,
        // only the concatenated values are placed, which reproduces the order just
        // when nothing else collides with them.
        let bunch_slots: Option<(Vec<(String, String, Option<i32>)>, Vec<usize>)> =
            q.subject_bunch(&subj).map(|b| {
                let hashes: Vec<Option<i32>> = b
                    .iter()
                    .map(|(pred, _, oh)| {
                        oh.map(|oh| jo::triple_hash(s, jo::node_hash(pred), oh))
                    })
                    .collect();
                let order = jo::bunch_order(&hashes);
                let mut rank = vec![usize::MAX; b.len()];
                for (k, &i) in order.iter().enumerate() {
                    rank[i] = k;
                }
                (b, rank)
            });
        let size = q.subject_triple_count(&subj);
        let fallback_cap = jo::capacity_for(size);
        if bunch_slots.is_none() && fallback_cap.is_none() {
            continue;
        }
        for c in cols {
            let p = jo::node_hash(&c.predicate);
            // Derive the permutation from the plain `rows` cell (bare lexical
            // forms), then apply it to both representations.
            let Some(cell) = table.rows[r].get(c.col).cloned() else { continue };
            let parts: Vec<String> =
                cell.split(c.sep.as_str()).map(|x| x.to_string()).collect();
            if parts.len() < 2 {
                continue;
            }
            // The bunch names the concatenated VALUES, so a value that contains
            // the separator — an xref carrying a quoted note — is still one value.
            // Take the order straight from the bunch whenever it accounts for the
            // whole cell; splitting the cell on the separator would cut such a
            // value in half and leave both halves unplaceable.
            let ordered = bunch_slots.as_ref().and_then(|(b, slot_of)| {
                let mut order: Vec<(usize, &str)> = Vec::new();
                for (i, (pred, lex, _)) in b.iter().enumerate() {
                    if *pred != c.predicate {
                        continue;
                    }
                    if slot_of[i] == usize::MAX {
                        return None;
                    }
                    order.push((slot_of[i], lex.as_str()));
                }
                if order.len() < 2 {
                    return None;
                }
                order.sort();
                let joined =
                    order.iter().map(|(_, l)| *l).collect::<Vec<_>>().join(&c.sep);
                same_fragments(&joined, &cell, &c.sep).then_some(joined)
            });
            if let Some(joined) = ordered {
                rewrite_cell(table, r, c.col, &cell, joined);
                continue;
            }
            // Insert with DOWNWARD linear probing, then read the slots out
            // ascending. On a collision the later insert takes the lower slot
            // and so comes FIRST — MONDO:0015812's 6739 and 6749 both hash to
            // 72 at capacity 79, and 6739 has to come out before 6749.
            let mut idx: Vec<usize> = (0..parts.len()).collect();
            if let Some((b, slot_of)) = &bunch_slots {
                let mut order: Vec<(usize, &str)> = b
                    .iter()
                    .enumerate()
                    .filter(|(i, (pred, _, _))| {
                        *pred == c.predicate && slot_of[*i] != usize::MAX
                    })
                    .map(|(i, (_, lex, _))| (slot_of[i], lex.as_str()))
                    .collect();
                order.sort();
                let mut rank: std::collections::HashMap<&str, usize> = Default::default();
                for (k, (_, lex)) in order.iter().enumerate() {
                    rank.entry(lex).or_insert(k);
                }
                idx.sort_by_key(|&i| {
                    (rank.get(parts[i].as_str()).copied().unwrap_or(usize::MAX), i)
                });
            } else {
                let Some(cap) = fallback_cap else { continue };
                let mut taken: std::collections::HashMap<i32, usize> = Default::default();
                let mut assigned: Vec<i32> = vec![0; parts.len()];
                for (i, part) in parts.iter().enumerate() {
                    let mut slot = jo::slot(s, p, jo::node_hash(part), cap);
                    while taken.contains_key(&slot) {
                        slot -= 1;
                        if slot < 0 {
                            slot += cap;
                        }
                    }
                    taken.insert(slot, i);
                    assigned[i] = slot;
                }
                idx.sort_by_key(|&i| (assigned[i], i));
            }
            let joined =
                idx.iter().map(|&i| parts[i].as_str()).collect::<Vec<_>>().join(&c.sep);
            rewrite_cell(table, r, c.col, &cell, joined);
        }
    }
}

/// Put the rows of a plain `SELECT` into the order the graph answers its pattern
/// in.
///
/// A pattern with a bound predicate AND object is answered from the object index:
/// every triple with that object sits in one bunch, and the bunch is read out in
/// slot order. That is the OUTER loop. Each solution it produces binds the
/// pattern's subject, so every remaining pattern on that subject is answered from
/// the subject index — the subject's own bunch, again in slot order. That is the
/// INNER loop, and the two together are the row order.
///
/// Only `?v a <T>` is driven from here: it is the shape whose bunch the document
/// order is recorded for.
fn apply_jena_scan_order(table: &mut QueryTable, q: &Queryable, sparql: &str) -> bool {
    use crate::sparql::jena_order as jo;
    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let dbg = std::env::var_os("OM_SCAN_DEBUG").is_some();
    let prefixes = query_prefixes(sparql);
    let expand = |t: &str| -> Option<String> {
        if let Some(i) = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
            return Some(i.to_string());
        }
        if t == "a" {
            return Some(RDF_TYPE.to_string());
        }
        let (name, local) = t.split_once(':')?;
        let (_, ns) = prefixes.iter().find(|(n, _)| n == name)?;
        Some(format!("{ns}{local}"))
    };
    let is_var = |t: &str| t.starts_with('?') || t.starts_with('$');
    let col_of = |t: &str| -> Option<usize> {
        let v = t.strip_prefix('?').or_else(|| t.strip_prefix('$'))?;
        table.columns.iter().position(|c| c == v)
    };
    let patterns = main_patterns(sparql);
    // The pattern with the most bound terms drives the loop, and `?v a <T>` is the
    // shape whose bunch the document order is recorded for.
    let Some((i, var, obj)) = patterns.iter().enumerate().find_map(|(i, (s, p, o))| {
        let v = s.strip_prefix('?').or_else(|| s.strip_prefix('$'))?;
        (expand(p).as_deref() == Some(RDF_TYPE) && !is_var(o))
            .then(|| (i, v.to_string(), expand(o)))
            .and_then(|(i, v, o)| o.map(|o| (i, v, o)))
    }) else {
        if dbg {
            eprintln!("[scan] no `?v a <T>` pattern to drive");
        }
        return false;
    };
    let Some(col) = table.columns.iter().position(|c| *c == var) else {
        if dbg {
            eprintln!("[scan] {var} is not a column");
        }
        return false;
    };
    let Some(seq) = q.typed_in_order(&obj) else {
        if dbg {
            eprintln!("[scan] no document order for {obj}");
        }
        return false;
    };
    let inner: Vec<InnerPattern> = patterns
        .iter()
        .enumerate()
        .filter(|(j, (s, _, _))| {
            *j != i && s.strip_prefix('?').or_else(|| s.strip_prefix('$')) == Some(var.as_str())
        })
        .filter_map(|(_, (_, p, o))| {
            Some(InnerPattern {
                predicate: expand(p),
                predicate_col: col_of(p),
                object_col: Some(col_of(o)?),
            })
        })
        .collect();
    if dbg {
        eprintln!("[scan] var={var} obj={obj} typed={} inner={}", seq.len(), inner.len());
    }
    if inner.is_empty() {
        return false;
    }
    let p = jo::node_hash(RDF_TYPE);
    let o = jo::node_hash(&obj);
    let hashes: Vec<Option<i32>> = seq
        .iter()
        .map(|s| s.as_ref().map(|s| jo::triple_hash(jo::node_hash(s), p, o)))
        .collect();
    let order = jo::bunch_order(&hashes);
    let mut outer: std::collections::HashMap<&str, usize> = Default::default();
    for (k, &i) in order.iter().enumerate() {
        if let Some(s) = &seq[i] {
            outer.entry(s.as_str()).or_insert(k);
        }
    }
    // The inner rank of every row of one subject, worked out once per subject.
    let mut ranks: Vec<usize> = vec![usize::MAX; table.rows.len()];
    let mut by_subject: std::collections::HashMap<&str, Vec<usize>> = Default::default();
    for (r, row) in table.rows.iter().enumerate() {
        let Some(s) = row.get(col) else { continue };
        by_subject.entry(s.as_str()).or_default().push(r);
    }
    for (subj, rows) in &by_subject {
        let Some(bunch) = q.subject_bunch(subj) else { continue };
        let s = jo::node_hash(subj);
        let hashes: Vec<Option<i32>> = bunch
            .iter()
            .map(|(pred, _, oh)| oh.map(|oh| jo::triple_hash(s, jo::node_hash(pred), oh)))
            .collect();
        let slot_of = {
            let order = jo::bunch_order(&hashes);
            let mut v = vec![usize::MAX; bunch.len()];
            for (k, &i) in order.iter().enumerate() {
                v[i] = k;
            }
            v
        };
        for &r in rows {
            ranks[r] = inner
                .iter()
                .filter_map(|pat| {
                    let want_p = pat.predicate(&table.rows[r]);
                    let want_o = pat.object(&table.rows[r])?;
                    bunch
                        .iter()
                        .enumerate()
                        .find(|(_, (bp, bl, _))| {
                            want_p.is_none_or(|w| w == bp.as_str()) && bl == want_o
                        })
                        .map(|(i, _)| slot_of[i])
                })
                .min()
                .unwrap_or(usize::MAX);
        }
    }
    let alt = filter_disjunction(sparql, &table.columns);
    let alt_rank = |row: &[String]| -> usize {
        let Some((c, terms)) = alt.as_ref() else { return 0 };
        row.get(*c)
            .and_then(|v| terms.iter().position(|t| t == v))
            .unwrap_or(usize::MAX)
    };
    let mut idx: Vec<usize> = (0..table.rows.len()).collect();
    idx.sort_by_key(|&i| {
        let subj = table.rows[i].get(col).map(String::as_str).unwrap_or("");
        (
            alt_rank(&table.rows[i]),
            outer.get(subj).copied().unwrap_or(usize::MAX),
            ranks[i],
            i,
        )
    });
    reorder_rows(table, &idx);
    true
}

/// A `FILTER (?v = <A> || ?v = <B> || …)` over one variable, as the column it
/// binds and the terms in the order the disjunction lists them.
///
/// Such a filter is not evaluated as a filter: each disjunct is folded into the
/// pattern as its own alternative, and the alternatives are answered one after
/// another. So the rows come out grouped by disjunct, in this order, whatever
/// order the underlying triples are in.
fn filter_disjunction(sparql: &str, columns: &[String]) -> Option<(usize, Vec<String>)> {
    let prefixes = query_prefixes(sparql);
    let expand = |t: &str| -> Option<String> {
        if let Some(i) = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
            return Some(i.to_string());
        }
        let (name, local) = t.split_once(':')?;
        let (_, ns) = prefixes.iter().find(|(n, _)| n == name)?;
        Some(format!("{ns}{local}"))
    };
    let upper = sparql.to_ascii_uppercase();
    let mut from = 0usize;
    while let Some(rel) = upper[from..].find("FILTER") {
        let at = from + rel;
        from = at + 6;
        let Some(open) = sparql[at..].find('(').map(|o| o + at) else { continue };
        let mut depth = 0usize;
        let mut close = open;
        for (k, c) in sparql[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
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
            continue;
        }
        let body = &sparql[open + 1..close];
        if !body.contains("||") || !body.contains('=') {
            continue;
        }
        let mut var: Option<&str> = None;
        let mut terms = Vec::new();
        let mut ok = true;
        for alt in body.split("||") {
            let Some((l, r)) = alt.split_once('=') else {
                ok = false;
                break;
            };
            let (l, r) = (l.trim(), r.trim());
            let (v, t) = match (l.starts_with('?') || l.starts_with('$'), r.starts_with('?')) {
                (true, false) => (l, r),
                (false, true) => (r, l),
                _ => {
                    ok = false;
                    break;
                }
            };
            if *var.get_or_insert(v) != v {
                ok = false;
                break;
            }
            match expand(t) {
                Some(iri) => terms.push(iri),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok || terms.len() < 2 {
            continue;
        }
        let name = var?.trim_start_matches(['?', '$']);
        let col = columns.iter().position(|c| c == name)?;
        return Some((col, terms));
    }
    None
}

/// The triple patterns of the query's main pattern block, in the order written,
/// with `;` and `,` continuations expanded.
///
/// Reading stops at the first `OPTIONAL`/`FILTER`/`UNION`/`BIND`/`[`: those add
/// or remove solutions but do not change the loop the rows come out of, and a
/// pattern block that opens one is not one this reproduces.
fn main_patterns(sparql: &str) -> Vec<(String, String, String)> {
    let Some(block) = where_block(sparql) else { return Vec::new() };
    let mut out = Vec::new();
    let mut terms: Vec<String> = Vec::new();
    let mut subject = String::new();
    let mut predicate = String::new();
    for raw in block.split_whitespace() {
        let up = raw.to_ascii_uppercase();
        if up.starts_with("OPTIONAL")
            || up.starts_with("FILTER")
            || up.starts_with("UNION")
            || up.starts_with("BIND")
            || raw.starts_with('[')
        {
            break;
        }
        let mut tok = raw;
        while let Some(rest) = tok.strip_prefix('{') {
            tok = rest;
            terms.clear();
        }
        let mut ends: Vec<char> = Vec::new();
        while tok.len() > 1 {
            let c = tok.chars().last().unwrap();
            if matches!(c, '.' | ';' | ',' | '}') {
                ends.push(c);
                tok = &tok[..tok.len() - 1];
            } else {
                break;
            }
        }
        ends.reverse();
        match tok.chars().next() {
            Some(c) if tok.len() == 1 && matches!(c, '.' | ';' | ',' | '}') => ends.insert(0, c),
            Some(_) => terms.push(tok.to_string()),
            None => {}
        }
        for sep in ends {
            match terms.len() {
                3 => {
                    subject = terms[0].clone();
                    predicate = terms[1].clone();
                    out.push((terms[0].clone(), terms[1].clone(), terms[2].clone()));
                }
                2 => {
                    predicate = terms[0].clone();
                    out.push((subject.clone(), terms[0].clone(), terms[1].clone()));
                }
                1 => out.push((subject.clone(), predicate.clone(), terms[0].clone())),
                _ => {}
            }
            terms.clear();
            if sep == '.' || sep == '}' {
                subject.clear();
                predicate.clear();
            }
        }
    }
    out
}

/// A triple pattern on the driving subject, with its predicate and object each
/// either a fixed IRI or the column a variable binds.
struct InnerPattern {
    predicate: Option<String>,
    predicate_col: Option<usize>,
    object_col: Option<usize>,
}

impl InnerPattern {
    fn predicate<'a>(&'a self, row: &'a [String]) -> Option<&'a str> {
        match (&self.predicate, self.predicate_col) {
            (Some(p), _) => Some(p.as_str()),
            (None, Some(c)) => row.get(c).map(String::as_str),
            _ => None,
        }
    }
    fn object<'a>(&self, row: &'a [String]) -> Option<&'a str> {
        row.get(self.object_col?).map(String::as_str)
    }
}

/// Reorder both representations of a table by a row permutation.
fn reorder_rows(table: &mut QueryTable, idx: &[usize]) {
    table.rows = idx.iter().map(|&i| table.rows[i].clone()).collect();
    if table.tsv_rows.len() == idx.len() {
        table.tsv_rows = idx.iter().map(|&i| table.tsv_rows[i].clone()).collect();
    }
}

/// Do two strings hold the same separated fragments, in some order? The check
/// that a reordering only moved values around.
fn same_fragments(a: &str, b: &str, sep: &str) -> bool {
    let mut x: Vec<&str> = a.split(sep).collect();
    let mut y: Vec<&str> = b.split(sep).collect();
    x.sort_unstable();
    y.sort_unstable();
    x == y
}

/// Put `new_plain` in a cell that currently holds `old_plain`, in both the plain
/// and the quoted representation. Quoting escapes character by character, so the
/// quoted body is the escape of the plain cell wherever it sits in the field.
fn rewrite_cell(
    table: &mut QueryTable,
    r: usize,
    col: usize,
    old_plain: &str,
    new_plain: String,
) {
    if let Some(trow) = table.tsv_rows.get_mut(r) {
        if let Some(tcell) = trow.get(col) {
            let old_body = crate::sparql::tsv_escape(old_plain);
            if let Some(at) = tcell.find(&old_body) {
                let pre = tcell[..at].to_string();
                let post = tcell[at + old_body.len()..].to_string();
                let body = crate::sparql::tsv_escape(&new_plain);
                trow[col] = format!("{pre}{body}{post}");
            }
        }
    }
    table.rows[r][col] = new_plain;
}

/// Does the query carry its own `ORDER BY`? Then the engine's order is the
/// query's business and the `--tdb` load order must not override it.
fn has_order_by(sparql: &str) -> bool {
    let mut in_str = false;
    let lower = sparql.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => in_str = !in_str,
            b'o' if !in_str && lower[i..].starts_with("order") => {
                let rest = lower[i + 5..].trim_start();
                if rest.starts_with("by") {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Order of first appearance of every absolute IRI in `path`, which is the order
/// `--tdb true` puts an unordered solution table into. One pass over the raw
/// bytes — no parse, so the order is the document's own text order; a file it
/// cannot read yields `None`, and the caller then leaves the solution order
/// alone.
fn first_appearance_order(path: &std::path::Path) -> Option<std::collections::HashMap<String, usize>> {
    let bytes = std::fs::read(path).ok()?;
    let mut out: std::collections::HashMap<String, usize> = Default::default();
    let mut n = 0usize;
    let mut i = 0usize;
    while i + 7 < bytes.len() {
        if bytes[i] == b'h' && (bytes[i..].starts_with(b"http://") || bytes[i..].starts_with(b"https://")) {
            let mut j = i;
            while j < bytes.len() {
                let c = bytes[j];
                if c == b'"' || c == b'<' || c == b'>' || c.is_ascii_whitespace() {
                    break;
                }
                j += 1;
            }
            if let Ok(iri) = std::str::from_utf8(&bytes[i..j]) {
                if !out.contains_key(iri) {
                    out.insert(iri.to_string(), n);
                    n += 1;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    Some(out)
}

/// Re-order a solution table into the `--tdb` load order: by the first column's
/// term, using its first appearance in the source document. Stable, and a term
/// the scan never saw sorts last.
fn apply_tdb_order(table: &mut QueryTable, order: &std::collections::HashMap<String, usize>) {
    let key = |row: &Vec<String>| -> usize {
        let Some(first) = row.first() else { return usize::MAX };
        let t = first.trim_start_matches('<').trim_end_matches('>');
        order.get(t).copied().unwrap_or(usize::MAX)
    };
    let mut idx: Vec<usize> = (0..table.rows.len()).collect();
    idx.sort_by_key(|&i| key(&table.rows[i]));
    table.rows = idx.iter().map(|&i| table.rows[i].clone()).collect();
    if table.tsv_rows.len() == idx.len() {
        table.tsv_rows = idx.iter().map(|&i| table.tsv_rows[i].clone()).collect();
    }
}

/// Split a `--query`/`--select`/`--construct` value list into `<FILE> <OUTPUT>`
/// pairs.
///
/// The option is repeatable and EVERY pair runs, not just the first: EFO's
/// `all_reports` passes seven `-s q out` pairs and CL's `custom_reports` five,
/// and each one has to produce its report.
///
/// A LONE trailing `FILE` is the send-the-result-to-`--output`/stdout form, so an
/// odd-length list ends in that shape.
fn query_pairs(v: &[PathBuf]) -> Vec<(&std::path::Path, Option<&std::path::Path>)> {
    let mut out = Vec::new();
    let mut chunks = v.chunks_exact(2);
    for pair in chunks.by_ref() {
        out.push((pair[0].as_path(), Some(pair[1].as_path())));
    }
    if let [lone] = chunks.remainder() {
        out.push((lone.as_path(), None));
    }
    out
}

/// The result format a format name selects for a solution table.
///
/// Only `tsv` names the tab-separated writer; every other name — including an
/// extension that is not a result format at all — is CSV, not TSV. EFO builds
/// `components/efo_terms.txt` from a `--select` with no `--format` written to a
/// `$@.tmp` path, and the file that has to come out is the CRLF-terminated CSV
/// its release carries.
fn parse_result_format(name: &str) -> ResultFormat {
    match name.trim_start_matches('-').to_ascii_lowercase().as_str() {
        "tsv" => ResultFormat::Tsv,
        // owlmake's own addition: a JSON result table.
        "json" | "jsonld" => ResultFormat::Json,
        _ => ResultFormat::Csv,
    }
}

/// Result-format resolution: an explicit `--format` wins; failing that the
/// OUTPUT path's extension; failing that the query form's default — SELECT
/// `csv`, ASK `txt`, CONSTRUCT/DESCRIBE `ttl`.
///
/// The extension step is what keeps `--query q.sparql out.csv` with no
/// `--format` from writing a TSV into a `.csv` file.
fn resolve_result_format(
    explicit: Option<&str>,
    out: Option<&std::path::Path>,
    sparql: &str,
) -> ResultFormat {
    // The query TYPE is settled before the format is consulted: an ASK always
    // prints a bare `true`/`false`, whatever `--format` said.
    if crate::sparql::query_form(sparql) == crate::sparql::QueryForm::Ask {
        return ResultFormat::Txt;
    }
    if let Some(name) = explicit.filter(|s| !s.is_empty()) {
        return parse_result_format(name);
    }
    if let Some(ext) = out.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        return parse_result_format(ext);
    }
    // The query form's default: SELECT -> csv. (A CONSTRUCT never reaches the
    // table renderer — it is serialized as RDF — so only the SELECT default
    // matters.)
    ResultFormat::Csv
}

/// The same resolution for a CONSTRUCT's RDF serialization.
fn resolve_rdf_format(explicit: Option<&str>, out: Option<&std::path::Path>) -> RdfFormat {
    if let Some(name) = explicit.filter(|s| !s.is_empty()) {
        return parse_rdf_format(name);
    }
    match out.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        Some(ext) => parse_rdf_format(ext),
        None => RdfFormat::Turtle, // the query form's default: CONSTRUCT -> ttl
    }
}

/// The file extension a `--queries` result gets in `--output-dir`: the query
/// file's base name plus the result format's name.
fn result_extension(fmt: ResultFormat) -> &'static str {
    match fmt {
        ResultFormat::Csv => "csv",
        ResultFormat::Json => "json",
        ResultFormat::Tsv => "tsv",
        ResultFormat::Txt => "txt",
    }
}

/// The same, for a `--queries` entry that is a CONSTRUCT.
fn rdf_extension(fmt: RdfFormat) -> &'static str {
    match fmt {
        RdfFormat::NTriples => "nt",
        RdfFormat::RdfXml => "xml",
        RdfFormat::JsonLd { .. } => "jsonld",
        _ => "ttl",
    }
}

/// Map a --format name to an RDF serialization for CONSTRUCT output. Falls back
/// to Turtle for table formats (tsv/csv) and unknown names.
fn parse_rdf_format(name: &str) -> RdfFormat {
    match name.to_ascii_lowercase().as_str() {
        "nt" | "ntriples" | "n-triples" => RdfFormat::NTriples,
        // NOT `owl`: only tsv/ttl/jsonld/nt/nq/csv/xml/sxml name a syntax, and
        // anything else falls through to Turtle below. EFO writes its CONSTRUCT
        // to `components/gwas_template.owl`, whose extension is `owl`, and that
        // file is Turtle on disk.
        "rdfxml" | "rdf/xml" | "xml" => RdfFormat::RdfXml,
        "jsonld" | "json-ld" | "json" => RdfFormat::JsonLd {
            profile: oxigraph::io::JsonLdProfileSet::empty(),
        },
        // ttl/turtle and table formats (tsv/csv) default to Turtle.
        _ => RdfFormat::Turtle,
    }
}

/// Render a query table in the requested result format.
fn render_table(table: &crate::sparql::QueryTable, fmt: ResultFormat) -> String {
    match fmt {
        ResultFormat::Csv => table.render(true),
        ResultFormat::Tsv => table.render(false),
        ResultFormat::Json => render_json(table),
        // An ASK table is the single `result` cell; only the boolean is printed.
        ResultFormat::Txt => {
            let v = table.rows.first().and_then(|r| r.first()).map(String::as_str);
            format!("{}\n", v.unwrap_or("false"))
        }
    }
}

/// Put a solution table into its release order, then hand it back for rendering.
///
/// Every query path comes through here — `--query`, `-s`/`--select` and
/// `-Q`/`--queries` alike — so a `GROUP_CONCAT` report reproduces whichever flag
/// ran it: MONDO's `mondo_obsoletioncandidates.tsv` and EFO's
/// `reports/basic-report.tsv` (a `group_concat` over `hasDbXref`, run via `-s`)
/// depend on that.
fn finish_table(
    table: &mut QueryTable,
    q: &Queryable,
    sparql: &str,
    tdb_order: Option<&std::collections::HashMap<String, usize>>,
) {
    // A GROUP BY decides the row order on its own: the groups are accumulated in a
    // hash table keyed by the group binding, and the result is read back out of it
    // in slot order. That happens above the graph, so it stands whether or not the
    // rows came from `--tdb`.
    let grouped = !has_order_by(sparql) && apply_jena_group_order(table, q, sparql);
    // An `ORDER BY` sorts by its conditions and then, for solutions those leave
    // equal, by the whole binding — see `apply_order_by_tiebreak`.
    apply_order_by_tiebreak(table, sparql);
    match tdb_order {
        Some(o) => {
            if !has_order_by(sparql) && !grouped {
                apply_tdb_order(table, o);
            }
        }
        None => {
            // No `--tdb`: the in-memory graph's per-subject hash-bucket order
            // decides how GROUP_CONCAT accumulates.
            let cols = concat_columns(sparql, &table.columns);
            if !cols.is_empty() {
                apply_jena_concat_order(table, q, &cols);
            } else if !grouped {
                // A plain SELECT has no order of its own: the rows come out in the
                // order the graph answers the pattern in.
                apply_jena_scan_order(table, q, sparql);
            }
        }
    }
}

/// The variables a `GROUP BY` groups on, in the order it names them: a bare
/// `?v`, or the `?v` a parenthesised `(<expr> AS ?v)` binds. Empty when the query
/// has no `GROUP BY` — an aggregate without one is a single implicit group and
/// has no order to reproduce.
fn group_by_vars(sparql: &str) -> Vec<String> {
    let lower = sparql.to_ascii_lowercase();
    let bytes = sparql.as_bytes();
    // The clause runs from `GROUP BY` to the next clause keyword or the end of the
    // group it sits in.
    let mut i = 0usize;
    let start = loop {
        let Some(rel) = lower[i..].find("group") else { return Vec::new() };
        let at = i + rel;
        let after = lower[at + 5..].trim_start();
        let prev_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        if prev_ok && after.starts_with("by") {
            let by = lower[at + 5..].find("by").unwrap() + at + 5;
            break by + 2;
        }
        i = at + 5;
    };
    let rest = &sparql[start..];
    let rest_lower = &lower[start..];
    let mut end = rest.len();
    for kw in ["having", "order", "limit", "offset", "values"] {
        let mut from = 0usize;
        while let Some(rel) = rest_lower[from..].find(kw) {
            let at = from + rel;
            let boundary = at == 0
                || !rest.as_bytes()[at - 1].is_ascii_alphanumeric() && rest.as_bytes()[at - 1] != b'_';
            if boundary {
                end = end.min(at);
                break;
            }
            from = at + kw.len();
        }
    }
    // A closing brace ends the clause too (a grouped sub-select).
    if let Some(p) = rest.find('}') {
        end = end.min(p);
    }
    let clause = &rest[..end];

    let mut out: Vec<String> = Vec::new();
    let cb = clause.as_bytes();
    let mut j = 0usize;
    let mut depth = 0usize;
    // Inside a parenthesised item only the variable after `AS` counts; outside one
    // every `?v` does.
    let clause_lower = clause.to_ascii_lowercase();
    while j < cb.len() {
        match cb[j] {
            b'(' => {
                depth += 1;
                j += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                j += 1;
            }
            b'?' | b'$' => {
                let mut k = j + 1;
                while k < cb.len() && (cb[k].is_ascii_alphanumeric() || cb[k] == b'_') {
                    k += 1;
                }
                let name = &clause[j + 1..k];
                if depth == 0 {
                    out.push(name.to_string());
                } else {
                    // Only the binding target — the one preceded by `AS`.
                    let before = clause_lower[..j].trim_end();
                    let is_as = before.ends_with("as")
                        && before[..before.len() - 2]
                            .chars()
                            .next_back()
                            .is_none_or(|c| c.is_whitespace() || c == '(');
                    if is_as {
                        out.push(name.to_string());
                    }
                }
                j = k;
            }
            _ => j += 1,
        }
    }
    out
}

/// The `?v <p> <o>` pattern a BGP is driven by: the first triple pattern whose
/// predicate and object are both concrete. It is the outer loop of the scan, so
/// the order its subjects come back in is the order the whole solution sequence
/// is built in.
fn driving_pattern(sparql: &str) -> Option<(String, String, String)> {
    let prefixes = query_prefixes(sparql);
    let expand = |t: &str| -> Option<String> {
        if let Some(i) = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
            return Some(i.to_string());
        }
        if t == "a" {
            return Some("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string());
        }
        let (name, local) = t.split_once(':')?;
        let (_, ns) = prefixes.iter().find(|(n, _)| n == name)?;
        Some(format!("{ns}{local}"))
    };
    let body = match sparql.to_ascii_uppercase().find("WHERE") {
        Some(i) => &sparql[i + 5..],
        None => return None,
    };
    let toks: Vec<&str> = body.split_whitespace().collect();
    let mut i = 0usize;
    while i + 2 < toks.len() {
        let s = toks[i].trim_matches(|c: char| matches!(c, '{' | '}' | '.' | ';' | ','));
        if let Some(var) = s.strip_prefix('?').or_else(|| s.strip_prefix('$')) {
            let p = toks[i + 1];
            let o = toks[i + 2].trim_end_matches(['.', ';', ',', '}']);
            if !p.starts_with('?') && !p.starts_with('$') && !o.starts_with('?') && !o.starts_with('$')
            {
                if let (Some(pi), Some(oi)) = (expand(p), expand(o)) {
                    return Some((var.to_string(), pi, oi));
                }
            }
        }
        i += 1;
    }
    None
}

/// The `WHERE { … }` block of a query, braces included.
fn where_block(sparql: &str) -> Option<&str> {
    let at = sparql.to_ascii_uppercase().find("WHERE")?;
    let open = sparql[at..].find('{')? + at;
    let bytes = sparql.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&sparql[open..=i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The position each GROUP BY key was first reached in, as a rank.
///
/// The groups are accumulated in insertion order within a bucket, and the
/// insertion order is the order the scan produced them: the driving pattern's
/// subjects come out of their bunch in slot order, so a key's rank is the lowest
/// slot of any solution that carries it. `None` when the query has no driving
/// pattern, or the auxiliary scan cannot be run.
fn group_scan_ranks(
    q: &Queryable,
    sparql: &str,
    vars: &[String],
) -> Option<std::collections::HashMap<Vec<String>, i32>> {
    use crate::sparql::jena_order as jo;
    let (subj_var, pred, obj) = driving_pattern(sparql)?;
    let cap = jo::capacity_for(q.object_triple_count(&obj))?;
    let block = where_block(sparql)?;
    let prologue = {
        let up = sparql.to_ascii_uppercase();
        let at = up.find("SELECT")?;
        &sparql[..at]
    };
    let aux = format!("{prologue}SELECT * WHERE {block}");
    let table = q.query_table(&aux).ok()?;
    let sc = table.columns.iter().position(|c| *c == subj_var)?;
    let mut kc: Vec<usize> = Vec::with_capacity(vars.len());
    for v in vars {
        kc.push(table.columns.iter().position(|c| c == v)?);
    }
    let ph = jo::node_hash(&pred);
    let oh = jo::node_hash(&obj);
    let mut out: std::collections::HashMap<Vec<String>, i32> = Default::default();
    for row in &table.rows {
        let Some(s) = row.get(sc) else { continue };
        let slot = jo::slot(jo::node_hash(s), ph, oh, cap);
        let key: Vec<String> =
            kc.iter().map(|&c| row.get(c).cloned().unwrap_or_default()).collect();
        let e = out.entry(key).or_insert(i32::MAX);
        if slot < *e {
            *e = slot;
        }
    }
    Some(out)
}

/// Put the rows into the order the grouping hash table hands them back: ascending
/// slot, and within a slot the order the groups were first seen.
///
/// Returns whether the order was applied. It is not when the query does not group,
/// when a grouping variable is not projected — the key cannot be rebuilt from the
/// table then — or when a key value is neither an IRI nor a string, whose hash is
/// its lexical form's.
fn apply_jena_group_order(table: &mut QueryTable, q: &Queryable, sparql: &str) -> bool {
    use crate::sparql::jena_order as jo;
    let vars = group_by_vars(sparql);
    if vars.is_empty() || table.rows.len() < 2 || table.tsv_rows.len() != table.rows.len() {
        return false;
    }
    let mut cols: Vec<(String, usize)> = Vec::new();
    for v in &vars {
        let Some(ci) = table.columns.iter().position(|c| c == v) else { return false };
        cols.push((v.clone(), ci));
    }
    let mut hashes: Vec<i32> = Vec::with_capacity(table.rows.len());
    for r in 0..table.rows.len() {
        let mut entries: Vec<(&str, i32)> = Vec::with_capacity(cols.len());
        for (name, ci) in &cols {
            let tsv = table.tsv_rows[r].get(*ci).map(String::as_str).unwrap_or("");
            let val = table.rows[r].get(*ci).map(String::as_str).unwrap_or("");
            // An unbound value is not in the key at all.
            if tsv.is_empty() {
                continue;
            }
            let node = if tsv.starts_with('<') && tsv.ends_with('>') {
                jo::node_hash(val)
            } else if tsv.starts_with('"') {
                // A plain, `xsd:string` or language-tagged literal hashes on its
                // lexical form; any other datatype hashes on its parsed value,
                // which is not recoverable from the table.
                let tail = &tsv[tsv.rfind('"').unwrap_or(0) + 1..];
                if tail.is_empty()
                    || tail.starts_with('@')
                    || tail == "^^<http://www.w3.org/2001/XMLSchema#string>"
                {
                    jo::node_hash(val)
                } else {
                    return false;
                }
            } else {
                return false;
            };
            entries.push((name.as_str(), node));
        }
        hashes.push(jo::binding_hash(&entries));
    }
    // One row per group, so the table holds exactly as many keys as there are rows.
    let cap = jo::group_table_capacity(table.rows.len());
    let slots: Vec<usize> = hashes.iter().map(|h| jo::group_slot(*h, cap)).collect();
    // Two keys in one slot come out in the order they were FIRST REACHED, which is
    // the scan order — not the order the engine happened to return the groups in.
    // The extra scan is only worth its cost when a slot really is shared.
    let shared = {
        let mut seen = std::collections::HashSet::new();
        slots.iter().any(|s| !seen.insert(*s))
    };
    let ranks = if shared { group_scan_ranks(q, sparql, &vars) } else { None };
    let rank_of = |i: usize| -> i32 {
        let Some(r) = ranks.as_ref() else { return i32::MAX };
        let key: Vec<String> =
            cols.iter().map(|(_, c)| table.rows[i].get(*c).cloned().unwrap_or_default()).collect();
        r.get(&key).copied().unwrap_or(i32::MAX)
    };
    let mut idx: Vec<usize> = (0..table.rows.len()).collect();
    idx.sort_by_key(|&i| (slots[i], rank_of(i), i));
    table.rows = idx.iter().map(|&i| table.rows[i].clone()).collect();
    table.tsv_rows = idx.iter().map(|&i| table.tsv_rows[i].clone()).collect();
    true
}

/// The `ORDER BY` conditions, when every one of them is a bare variable
/// (optionally wrapped in `ASC(…)`/`DESC(…)`). `None` when the query has no
/// `ORDER BY`, or when any condition is an expression: the tie-break below reads
/// the sort keys out of the projected columns, and an expression has no column.
fn order_by_conditions(sparql: &str) -> Option<Vec<String>> {
    let lower = sparql.to_ascii_lowercase();
    let bytes = sparql.as_bytes();
    let mut i = 0usize;
    let start = loop {
        let rel = lower[i..].find("order")?;
        let at = i + rel;
        let prev_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let after = lower[at + 5..].trim_start();
        if prev_ok && after.starts_with("by") {
            let by = lower[at + 5..].find("by").unwrap() + at + 5;
            break by + 2;
        }
        i = at + 5;
    };
    let rest = &sparql[start..];
    let rest_lower = &lower[start..];
    let mut end = rest.len();
    for kw in ["limit", "offset", "values"] {
        if let Some(rel) = rest_lower.find(kw) {
            end = end.min(rel);
        }
    }
    if let Some(p) = rest.find('}') {
        end = end.min(p);
    }
    let clause = rest[..end].trim();
    let mut out: Vec<String> = Vec::new();
    let mut t = clause;
    while !t.is_empty() {
        t = t.trim_start();
        if t.is_empty() {
            break;
        }
        let tl = t.to_ascii_lowercase();
        let rest_after = if tl.starts_with("asc(") || tl.starts_with("desc(") {
            let open = t.find('(')?;
            let close = t.find(')')?;
            let inner = t[open + 1..close].trim();
            out.push(bare_var(inner)?);
            &t[close + 1..]
        } else {
            let stop = t.find(char::is_whitespace).unwrap_or(t.len());
            out.push(bare_var(&t[..stop])?);
            &t[stop..]
        };
        t = rest_after;
    }
    (!out.is_empty()).then_some(out)
}

/// `?v` / `$v` as the variable name, `None` for anything else.
fn bare_var(tok: &str) -> Option<String> {
    let name = tok.strip_prefix('?').or_else(|| tok.strip_prefix('$'))?;
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then(|| name.to_string())
}

/// The kind rank an RDF term sorts under: unbound first, then blank nodes, IRIs
/// and literals.
fn term_rank(t: &str) -> u8 {
    if t.is_empty() {
        0
    } else if t.starts_with("_:") {
        1
    } else if t.starts_with('<') {
        2
    } else {
        3
    }
}

/// A literal cell split into (lexical form, language, datatype IRI). A bare
/// number or boolean is the abbreviated form of its XSD datatype.
fn literal_parts(t: &str) -> (String, Option<String>, String) {
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    if let Some(body) = t.strip_prefix('"') {
        let mut lex = String::new();
        let mut it = body.char_indices();
        let mut close = body.len();
        while let Some((i, c)) = it.next() {
            match c {
                '\\' => match it.next().map(|(_, c)| c) {
                    Some('n') => lex.push('\n'),
                    Some('r') => lex.push('\r'),
                    Some('t') => lex.push('\t'),
                    Some(other) => lex.push(other),
                    None => {}
                },
                '"' => {
                    close = i;
                    break;
                }
                c => lex.push(c),
            }
        }
        let tail = &body[(close + 1).min(body.len())..];
        if let Some(lang) = tail.strip_prefix('@') {
            return (lex, Some(lang.to_string()), format!("{XSD}string"));
        }
        if let Some(dt) = tail.strip_prefix("^^<").and_then(|d| d.strip_suffix('>')) {
            return (lex, None, dt.to_string());
        }
        return (lex, None, format!("{XSD}string"));
    }
    let dt = if t == "true" || t == "false" {
        "boolean"
    } else if t.contains(['e', 'E']) {
        "double"
    } else if t.contains('.') {
        "decimal"
    } else {
        "integer"
    };
    (t.to_string(), None, format!("{XSD}{dt}"))
}

/// Two solution cells in RDF-term order: unbound before bound, then blank nodes,
/// IRIs and literals. Literals compare by lexical form, then a plain string
/// before a tagged or typed one, then language tag (case-insensitively first),
/// then datatype IRI.
fn compare_rdf_terms(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (ra, rb) = (term_rank(a), term_rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match ra {
        0 => Ordering::Equal,
        1 => a[2..].cmp(&b[2..]),
        2 => a[1..a.len() - 1].cmp(&b[1..b.len() - 1]),
        _ => {
            if a == b {
                return Ordering::Equal;
            }
            let (la, lga, da) = literal_parts(a);
            let (lb, lgb, db) = literal_parts(b);
            let x = la.cmp(&lb);
            if x != Ordering::Equal {
                return x;
            }
            const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
            let simple = |lang: &Option<String>, dt: &str| lang.is_none() && dt == XSD_STRING;
            if simple(&lga, &da) {
                return Ordering::Less;
            }
            if simple(&lgb, &db) {
                return Ordering::Greater;
            }
            match (&lga, &lgb) {
                (Some(x), Some(y)) => {
                    let ci = x.to_lowercase().cmp(&y.to_lowercase());
                    if ci != Ordering::Equal {
                        return ci;
                    }
                    x.cmp(y)
                }
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => da.cmp(&db),
            }
        }
    }
}

/// Break the ties an `ORDER BY` leaves.
///
/// Sorting is by the conditions the query names, and two solutions equal on all
/// of them are then compared VARIABLE BY VARIABLE, in variable-name order, in
/// RDF-term order — so the sequence is total and does not depend on the order the
/// solutions arrived in. EFO's `reports/obsoletes.tsv` orders by `?cls` alone and
/// has up to three rows per class; without the tie-break their order is whatever
/// the graph handed back.
fn apply_order_by_tiebreak(table: &mut QueryTable, sparql: &str) {
    let Some(conds) = order_by_conditions(sparql) else { return };
    let n = table.rows.len();
    if n < 2 || table.tsv_rows.len() != n {
        return;
    }
    let mut key_cols: Vec<usize> = Vec::with_capacity(conds.len());
    for v in &conds {
        let Some(i) = table.columns.iter().position(|c| c == v) else { return };
        key_cols.push(i);
    }
    // The comparison runs over every variable, in NAME order.
    let mut by_name: Vec<usize> = (0..table.columns.len()).collect();
    by_name.sort_by(|a, b| table.columns[*a].cmp(&table.columns[*b]));

    let cell = |r: usize, c: usize| -> &str {
        table.tsv_rows[r].get(c).map(String::as_str).unwrap_or("")
    };
    let keys: Vec<Vec<&str>> =
        (0..n).map(|r| key_cols.iter().map(|&c| cell(r, c)).collect()).collect();
    let syntactic = |a: usize, b: usize| -> std::cmp::Ordering {
        for &c in &by_name {
            let o = compare_rdf_terms(cell(a, c), cell(b, c));
            if o != std::cmp::Ordering::Equal {
                return o;
            }
        }
        std::cmp::Ordering::Equal
    };
    let mut order: Vec<usize> = (0..n).collect();
    let mut i = 0usize;
    while i < n {
        let mut j = i + 1;
        while j < n && keys[j] == keys[i] {
            j += 1;
        }
        if j - i > 1 {
            order[i..j].sort_by(|&a, &b| syntactic(a, b));
        }
        i = j;
    }
    if order.iter().enumerate().all(|(k, &v)| k == v) {
        return;
    }
    let rows: Vec<Vec<String>> = order.iter().map(|&i| table.rows[i].clone()).collect();
    let tsv: Vec<Vec<String>> = order.iter().map(|&i| table.tsv_rows[i].clone()).collect();
    table.rows = rows;
    table.tsv_rows = tsv;
}

/// Minimal JSON rendering of a result table (array of column->value objects).
fn render_json(table: &crate::sparql::QueryTable) -> String {
    let mut out = String::from("[\n");
    for (ri, row) in table.rows.iter().enumerate() {
        out.push_str("  {");
        for (ci, col) in table.columns.iter().enumerate() {
            if ci > 0 {
                out.push(',');
            }
            let val = row.get(ci).map(String::as_str).unwrap_or("");
            out.push_str(&format!("{}: {}", json_str(col), json_str(val)));
        }
        out.push('}');
        if ri + 1 < table.rows.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    // `-g,--use-graphs true` resolves the `owl:imports` closure and MERGES it into
    // the model before the store is loaded, so the single graph the query sees is
    // the root ontology unioned with everything it imports. Without the flag the
    // root document is loaded on its own and only its own axioms are visible.
    let use_graphs = args.use_graphs.unwrap_or(false);
    let mut model = if use_graphs {
        crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?
    } else {
        crate::cmd::take_or_load_no_imports(piped, args.input.as_deref(), &args.common)?
    };
    args.common.apply(&mut model)?;

    // SPARQL is evaluated in memory, but --tdb/--create-tdb/--temporary-file still
    // materialize a real on-disk dataset (kept afterwards only with
    // --keep-tdb-mappings).
    if use_graphs && crate::progress::verbosity() >= 1 {
        status!("query: --use-graphs: querying the root ontology unioned with its import closure");
    }
    let want_tdb = args.tdb.unwrap_or(false)
        || args.create_tdb.unwrap_or(false)
        || args.temporary_file.unwrap_or(false);
    let tdb = if want_tdb {
        crate::cmd::materialize_tdb(
            &model,
            args.tdb_directory.as_deref(),
            args.temporary_file.unwrap_or(false),
        )?
    } else {
        None
    };

    // Empty means "no `--format` on the command line" (see the field's docs), which
    // is what lets `resolve_result_format` fall back to the output extension and
    // then to the query form.
    let explicit_fmt = Some(args.format.as_str()).filter(|s| !s.is_empty());

    // Apply any SPARQL UPDATE files to the model. This mutates the model that is
    // passed along the chain / saved by --output. We round-trip through an
    // oxigraph store: load triples, apply updates, dump back, reparse.
    if !args.update.is_empty() {
        let mut rdf = Vec::new();
        crate::io::write_to_ref(&model, &mut rdf, Format::RdfXml)?;
        let store = Store::new().map_err(|e| anyhow!("store init: {e}"))?;
        load_preserving_literals(&store, &rdf)?;
        for upath in &args.update {
            let sparql = std::fs::read_to_string(upath)?;
            store
                .update(sparql.as_str())
                .map_err(|e| anyhow!("SPARQL UPDATE error in {}: {e}", upath.display()))?;
        }
        // A STANDALONE `owl:Axiom` reification — one whose asserted triple is
        // absent — still denotes the annotated axiom, so a `--update` that inserts
        // only the reification must still yield it. MONDO's `clingen-labels.ru`
        // does exactly that: it moves an `rdfs:label` to a `hasExactSynonym` by
        // inserting an `owl:Axiom` block and no
        // `?entity oboInOwl:hasExactSynonym ?label` triple, and what has to come
        // out is `AnnotationAssertion(Annotation(hasSynonymType …) hasExactSynonym
        // MONDO_0007057 "…")`. Re-assert the base triple here so the reification
        // re-absorbs on the reparse.
        //
        // "Orphan" can only be decided by node identity, and an ANONYMOUS target —
        // an RDF list, an anonymous class expression — is a different node in the
        // reification than in the assertion it annotates: RDF/XML mints a fresh
        // node per `rdf:parseType="Collection"`. So a reification with a blank-node
        // target whose subject already asserts that property is annotating the
        // existing assertion, not orphaned; re-asserting it would add a second
        // axiom. RO's eight annotated `owl:propertyChainAxiom` edges are exactly
        // this shape.
        store
            .update(
                "PREFIX owl: <http://www.w3.org/2002/07/owl#>\n\
                 INSERT { ?s ?p ?t }\n\
                 WHERE {\n\
                   ?ax a owl:Axiom ;\n\
                       owl:annotatedSource ?s ;\n\
                       owl:annotatedProperty ?p ;\n\
                       owl:annotatedTarget ?t .\n\
                   FILTER NOT EXISTS { ?s ?p ?t }\n\
                   FILTER (!isBlank(?t) || NOT EXISTS { ?s ?p ?any })\n\
                 }",
            )
            .map_err(|e| anyhow!("re-asserting orphan reifications: {e}"))?;
        let mut dumped = Vec::new();
        // RDF/XML is a graph (not dataset) format, so dump the default graph.
        store
            .dump_graph_to_writer(GraphNameRef::DefaultGraph, RdfFormat::RdfXml, &mut dumped)
            .map_err(|e| anyhow!("dumping updated store: {e}"))?;
        let dumped = unmask_literals(dumped);
        let updated = crate::io::load_from(std::io::Cursor::new(dumped), Format::RdfXml)?;
        // Preserve the original prefix map and document metadata for downstream
        // serialization — `from_parts` alone would drop the scanned xmlns/idspaces
        // the RDF/XML and OBO writers need.
        let mut out = crate::model::Model::from_parts(
            updated.ont,
            crate::model::clone_prefixes(&model.prefixes),
        );
        out.carry_meta_from(&model);
        // The round trip retypes every untyped literal: a literal with no datatype
        // comes back out of the store as `xsd:string`, where an OFN / RDF-XML parse
        // gives `rdf:PlainLiteral`. That reorders a subject's triples, because
        // literals sort on the datatype IRI first — `"…7189"` comes before
        // `"…9285"^^xsd:anyURI` while it is plain, and after it once it is
        // `xsd:string`. Set AFTER carry_meta_from, which would copy the input's
        // value.
        out.plain_literals_typed = true;
        // Whether the round trip also drops the document format's prefixes is
        // decided by the one recorded fact `Plan::emulate_robot_version`, resolved at
        // ingest. The boundary is 1.9.9:
        //
        // - Below 1.9.9 the prefix map does not survive the update, which is why
        //   `subsets/mondo-clingen.owl` must not keep the `xmlns:doap` /
        //   `xmlns:protege` it inherits from `mondo-base.owl`.
        // - At 1.9.9 and above it does, including prefixes no entity uses: HPO's
        //   `hp-fr.owl` carries `xmlns:HP` and `xmlns:IAO` out of its first merge
        //   input, `translations/hp-fr.babelon.owl`, and neither appears as a
        //   QName anywhere in the body.
        //
        // The same recorded fact decides how OBO Graphs JSON nests axiom
        // annotations: both artefact conventions changed in the same generation, so
        // `build::set_robot_behaviours` reads that one version and sets both — no
        // repo can end up with the pre-1.9.9 JSON nesting and the post-1.9.9 prefix
        // handling, a combination no release carries. It is a property of the repo
        // and not of whoever invokes owlmake: MONDO pins its build toolchain, so
        // MONDO gets the below-1.9.9 answer whatever image is on the machine.
        //
        // Only the LOSING side is asserted here. Keeping the format's prefixes
        // means the update leaves the prefix map as it found it, so the value
        // `carry_meta_from` already copied stands — writing `false` would undo a
        // preceding `filter`, which builds a new ontology and clears the map for
        // its own reasons. `mondo-simple.owl` runs both, in that order.
        if !update_keeps_prefixes() {
            out.format_prefixes_cleared = true;
        }
        // Restore the ontology's identity. A round-trip through triples loses the
        // distinction between the ontology's `owl:versionIRI` and an ordinary
        // annotation on the ontology node, so the reparse hands back the version IRI
        // as an `OntologyAnnotation`. Drop that stray annotation: left in place it
        // writes a second, stale `<owl:versionIRI>` into `mondo-simple.owl` and a
        // wrong `data-version:` into its OBO.
        {
            use horned_owl::model::{Component, MutableOntology};
            const VERSION_IRI: &str = "http://www.w3.org/2002/07/owl#versionIRI";
            let stray: Vec<_> = out
                .ont
                .iter()
                .filter(|ac| match &ac.component {
                    Component::OntologyAnnotation(oa) => oa.0.ap.0.as_ref() == VERSION_IRI,
                    _ => false,
                })
                .cloned()
                .collect();
            for ac in stray {
                out.ont.remove(&ac);
            }
            // An update does not change the ontology's identity, so the ID before
            // it is the ID after it. Take the input's WHOLE `OntologyID` rather
            // than only filling in a missing one: the round trip rebuilds an ID
            // from the `<ont> a owl:Ontology` triple alone, which carries the
            // ontology IRI but no VERSION IRI, and a present-but-incomplete ID
            // would leave uPheno's `mirror/merged.owl` without the
            // `…/bfo/2019-08-26/bfo.owl` version IRI its first input declares.
            let input_id = model
                .ont
                .iter()
                .find(|ac| matches!(ac.component, Component::OntologyID(_)))
                .cloned();
            if let Some(id) = input_id {
                let stale: Vec<_> = out
                    .ont
                    .iter()
                    .filter(|ac| matches!(ac.component, Component::OntologyID(_)))
                    .cloned()
                    .collect();
                for ac in stale {
                    out.ont.remove(&ac);
                }
                out.ont.insert(id);
            }
        }
        model = out;
    }

    let q = Queryable::from_model(&model)?;

    // `--tdb true` puts a SELECT with no `ORDER BY` into DOCUMENT order rather than
    // the store's own order: rows sort on the first column's term, keyed by where
    // that term first appears in the `--input` file — on MONDO's
    // `release-report.sparql` over `mondo-base.owl`, all 36,079 rows. The key is a
    // byte scan of `--input`, so a model piped in from a previous step has no
    // document to key on and `--tdb true` leaves its solution order untouched.
    // Only built when `--tdb true` is actually given, so no other query is
    // re-ordered.
    let tdb_order: Option<std::collections::HashMap<String, usize>> =
        if args.tdb == Some(true) { args.input.as_deref().and_then(first_appearance_order) } else { None };

    let mut ran_any = false;

    // `-q,--query <FILE> <OUTPUT>` pairs (repeatable), via either the canonical
    // `--query` (when given an OUTPUT) or the explicit `--query-pair`. A lone
    // `--query <FILE>` (single value) is the to-`--output`/stdout form, handled
    // with `--query-string` below.
    let query_as_pairs: &[PathBuf] = if args.query.len() >= 2 { &args.query } else { &[] };
    for pair in args.query_pairs.chunks(2).chain(query_as_pairs.chunks(2)) {
        if pair.len() != 2 {
            bail!("--query expects FILE and OUTPUT");
        }
        let out = pair[1].as_path();
        let sparql = std::fs::read_to_string(&pair[0])
            .with_context(|| format!("reading query {}", pair[0].display()))?;
        // `--query` takes any query form: a CONSTRUCT writes RDF in `--format`,
        // not a solution table (MONDO's `mirror-hgnc` relies on this).
        let output = q
            .run_query(&sparql, resolve_rdf_format(explicit_fmt, Some(out)))
            .with_context(|| format!("running query {}", pair[0].display()))?;
        match output {
            QueryOutput::Graph(rdf) => std::fs::write(out, rdf)?,
            QueryOutput::Table(mut table) => {
                finish_table(&mut table, &q, &sparql, tdb_order.as_ref());
                let fmt = resolve_result_format(explicit_fmt, Some(out), &sparql);
                std::fs::write(out, render_table(&table, fmt))?
            }
        }
        ran_any = true;
    }

    // `-Q,--queries <FILE>...`: each to output-dir or stdout. These have no
    // OUTPUT path, so the format falls back to the query form's default.
    for qpath in &args.queries {
        let sparql = std::fs::read_to_string(qpath)
            .with_context(|| format!("reading query {}", qpath.display()))?;
        let rdf_fmt = resolve_rdf_format(explicit_fmt, None);
        let table_fmt = resolve_result_format(explicit_fmt, None, &sparql);
        let output = q
            .run_query(&sparql, rdf_fmt)
            .with_context(|| format!("running query {}", qpath.display()))?;
        let (rendered, ext) = match output {
            QueryOutput::Graph(rdf) => (rdf, rdf_extension(rdf_fmt)),
            QueryOutput::Table(mut table) => {
                finish_table(&mut table, &q, &sparql, tdb_order.as_ref());
                (render_table(&table, table_fmt).into_bytes(), result_extension(table_fmt))
            }
        };
        match &args.output_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                let stem = qpath
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "query".to_string());
                std::fs::write(dir.join(format!("{stem}.{ext}")), rendered)?;
            }
            None => std::io::Write::write_all(&mut std::io::stdout(), &rendered)?,
        }
        ran_any = true;
    }

    // `-c,--construct <FILE> <OUTPUT>` (repeatable): run each CONSTRUCT and
    // write its RDF graph. A trailing lone FILE goes to --output/stdout.
    for (path, out) in query_pairs(&args.construct) {
        let sparql = std::fs::read_to_string(path)
            .with_context(|| format!("reading query {}", path.display()))?;
        let target = out.or(args.output.as_deref());
        let rdf = q
            .construct(&sparql, resolve_rdf_format(explicit_fmt, target))
            .with_context(|| format!("running query {}", path.display()))?;
        match target {
            Some(p) => std::fs::write(p, rdf)?,
            None => std::io::Write::write_all(&mut std::io::stdout(), &rdf)?,
        }
        ran_any = true;
    }

    // `-s,--select <FILE> <OUTPUT>` (repeatable): run each SELECT and write
    // its result table. Same path as --query for a SELECT query.
    for (path, out) in query_pairs(&args.select) {
        let sparql = std::fs::read_to_string(path)
            .with_context(|| format!("reading query {}", path.display()))?;
        let target = out.or(args.output.as_deref());
        let mut table = q
            .query_table(&sparql)
            .with_context(|| format!("running query {}", path.display()))?;
        finish_table(&mut table, &q, &sparql, tdb_order.as_ref());
        let rendered = render_table(&table, resolve_result_format(explicit_fmt, target, &sparql));
        match target {
            Some(p) => std::fs::write(p, rendered)?,
            None => print!("{rendered}"),
        }
        ran_any = true;
    }

    // Single query (a lone `--query <FILE>`, or an inline string) → --output/stdout.
    let single = match (args.query.as_slice(), &args.query_string) {
        ([path], _) => Some(std::fs::read_to_string(path)?),
        (_, Some(s)) => Some(s.clone()),
        _ => None,
    };
    if let Some(sparql) = single {
        let out = args.output.as_deref();
        let rendered = match q.run_query(&sparql, resolve_rdf_format(explicit_fmt, out))? {
            QueryOutput::Graph(rdf) => rdf,
            QueryOutput::Table(mut table) => {
                finish_table(&mut table, &q, &sparql, tdb_order.as_ref());
                render_table(&table, resolve_result_format(explicit_fmt, out, &sparql)).into_bytes()
            }
        };
        match &args.output {
            Some(p) => std::fs::write(p, rendered)?,
            None => std::io::Write::write_all(&mut std::io::stdout(), &rendered)?,
        }
        ran_any = true;
    }

    if !ran_any && args.update.is_empty() {
        bail!("query requires --query/--query-string, --construct, --select, --query-pair, --queries, or --update");
    }

    // If only updates ran (no query wrote to --output), save the updated model.
    // Only a *single*-form query (no dedicated OUTPUT) consumes --output — which,
    // since `--select`/`--construct` chunk in twos, is an ODD-length list.
    let wrote_output = args.query.len() == 1
        || args.query_string.is_some()
        || args.construct.len() % 2 == 1
        || args.select.len() % 2 == 1;
    if !wrote_output {
        crate::cmd::maybe_save(&mut model, args.output.as_deref(), None)?;
    }

    // Remove the on-disk TDB dataset unless --keep-tdb-mappings was given.
    crate::cmd::cleanup_tdb(tdb, args.keep_tdb_mappings.unwrap_or(false));

    Ok(Some(model))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// EFO's `all_reports` passes SEVEN `-s FILE OUT` pairs and CL's
    /// `custom_reports` five; every pair has to run.
    #[test]
    fn every_select_pair_runs() {
        let v = vec![p("a.sparql"), p("a.tsv"), p("b.sparql"), p("b.tsv"), p("c.sparql"), p("c.tsv")];
        let pairs = query_pairs(&v);
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[2].0, std::path::Path::new("c.sparql"));
        assert_eq!(pairs[2].1, Some(std::path::Path::new("c.tsv")));
    }

    /// An ODD-length list ends in the lone-FILE form — owlmake's own addition on
    /// top of the FILE/OUTPUT pairing, so its shape is owlmake's to change: the
    /// result goes to `--output`/stdout.
    #[test]
    fn a_trailing_lone_file_goes_to_output() {
        let v = vec![p("a.sparql"), p("a.tsv"), p("b.sparql")];
        let pairs = query_pairs(&v);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[1], (std::path::Path::new("b.sparql"), None));

        assert!(query_pairs(&[]).is_empty());
        assert_eq!(query_pairs(&[p("only.sparql")]), vec![(std::path::Path::new("only.sparql"), None)]);
    }

    /// `--format` wins, then the OUTPUT extension, then the query form.
    #[test]
    fn result_format_falls_back_the_way_robot_does() {
        let select = "SELECT ?s WHERE { ?s ?p ?o }";
        let ask = "ASK { ?s ?p ?o }";
        assert_eq!(
            resolve_result_format(Some("tsv"), Some(&p("out.csv")), select),
            ResultFormat::Tsv
        );
        assert_eq!(
            resolve_result_format(None, Some(&p("out.csv")), select),
            ResultFormat::Csv
        );
        assert_eq!(
            resolve_result_format(None, Some(&p("out.tsv")), select),
            ResultFormat::Tsv
        );
        // No OUTPUT: the query form's default — SELECT csv, ASK txt.
        assert_eq!(resolve_result_format(None, None, select), ResultFormat::Csv);
        // An ASK ignores the format entirely.
        assert_eq!(resolve_result_format(None, None, ask), ResultFormat::Txt);
        assert_eq!(resolve_result_format(Some("tsv"), Some(&p("a.tsv")), ask), ResultFormat::Txt);
        // An empty `--format` is "not given" (the clap default).
        assert_eq!(resolve_result_format(Some(""), Some(&p("o.csv")), select), ResultFormat::Csv);
        // OBA's `--format --csv` typo still resolves to CSV.
        assert_eq!(resolve_result_format(Some("--csv"), None, select), ResultFormat::Csv);
        // An extension that names no result format is CSV, not TSV — EFO's
        // `--select … $@.tmp` produces a CRLF file.
        assert_eq!(resolve_result_format(None, Some(&p("efo_terms.txt.tmp")), select), ResultFormat::Csv);
        assert_eq!(resolve_result_format(None, Some(&p("mondo_terms.txt")), select), ResultFormat::Csv);
    }

    #[test]
    fn rdf_format_falls_back_to_the_output_extension_then_turtle() {
        assert_eq!(resolve_rdf_format(None, Some(&p("g.nt"))), RdfFormat::NTriples);
        assert_eq!(resolve_rdf_format(None, Some(&p("g.xml"))), RdfFormat::RdfXml);
        assert_eq!(resolve_rdf_format(None, None), RdfFormat::Turtle);
        // `owl` is NOT one of them: only tsv/ttl/jsonld/nt/nq/csv/xml/sxml name a
        // syntax and anything else falls back to Turtle — which is why EFO's
        // `components/gwas_template.owl`, written by a CONSTRUCT under an `.owl`
        // name, really is Turtle on disk.
        assert_eq!(resolve_rdf_format(None, Some(&p("g.owl"))), RdfFormat::Turtle);
        // MONDO's `mirror-hgnc`: `query --format ttl --query construct-hgnc.sparql
        // mirror/hgnc.owl` — the explicit `--format` must beat the `.owl` extension.
        assert_eq!(resolve_rdf_format(Some("ttl"), Some(&p("mirror/hgnc.owl"))), RdfFormat::Turtle);
    }

    /// An ASK result is written as a bare boolean line, with no header.
    #[test]
    fn ask_renders_as_a_bare_boolean() {
        let t = QueryTable {
            columns: vec!["result".into()],
            rows: vec![vec!["true".into()]],
            tsv_rows: Vec::new(),
            select: false,
        };
        assert_eq!(render_table(&t, ResultFormat::Txt), "true\n");
    }
}
