//! `kgx transform` — the OBO Graph JSON → KGX TSV export that gives a release its
//! `<ont>_nodes.tsv` / `<ont>_edges.tsv` pair (and the same pair per subset).
//!
//! The transform is small and entirely data-driven once three facts are pinned
//! down:
//!
//! * **The prefix map in force during a transform has FIVE entries** — biolink,
//!   owlstar, MONARCH, MONARCH_NODE and the empty prefix — with the
//!   `monarch_context` and `obo_context` JSON-LD contexts as fallbacks, and nothing
//!   else. Contraction is a namespace-prefix match, so
//!   `http://identifiers.org/hgnc/10001` stays a full IRI in the output: no map in
//!   play binds a prefix to that namespace — `monarch_context` does bind `HGNC`,
//!   but to the genenames.org namespace, which this IRI does not start with.
//! * **The node columns are every key the records DECLARE; the edge columns only
//!   the keys that actually occur.** Both are ordered by `order_columns`: a fixed
//!   core order, then the rest sorted. A declared node column survives even when
//!   every value in it is empty.
//! * **An edge's `id` is a fresh `urn:uuid:<uuid4>` per run**, so `_edges.tsv` is
//!   not reproducible: two consecutive runs over the same input differ in that
//!   column and in nothing else. `_nodes.tsv` IS reproducible.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;

/// The prefix map in force during a transform (biolink, owlstar, MONARCH,
/// MONARCH_NODE and the empty prefix).
const RUNTIME_PREFIX_MAP: &str = include_str!("../kgx_data/runtime_prefix_map.json");
/// The `monarch_context` JSON-LD context, the first fallback map.
const MONARCH_CONTEXT: &str = include_str!("../kgx_data/monarch_context.json");
/// The `obo_context` JSON-LD context, the second fallback map.
const OBO_CONTEXT: &str = include_str!("../kgx_data/obo_context.json");

#[derive(ClapArgs)]
pub struct Args {
    /// kgx sub-command. Only `transform` is implemented — it is the only one the
    /// release path runs.
    pub action: String,
    /// Input files (kgx takes them positionally after the flags).
    #[arg(short = 'i', long = "input", value_name = "FILE")]
    pub input: Vec<PathBuf>,
    /// Input serialization (`obojson`).
    #[arg(long = "input-format")]
    pub input_format: Option<String>,
    /// Output serialization (`tsv`).
    #[arg(long = "output-format")]
    pub output_format: Option<String>,
    /// Output BASE name: kgx writes `<base>_nodes.tsv` and `<base>_edges.tsv`.
    #[arg(short = 'o', long = "output")]
    pub output: Option<PathBuf>,
    /// Accepted for CLI compatibility; kgx defaults it to `|`.
    #[arg(long = "list-delimiter", default_value = "|")]
    pub list_delimiter: String,
    /// Positional input files.
    #[arg(value_name = "INPUTS")]
    pub inputs: Vec<PathBuf>,
}

/// Pipeline entry point. `kgx` reads and writes files itself, so any piped model
/// passes straight through.
pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> Result<Option<crate::model::Model>> {
    run_args(args)?;
    Ok(piped)
}

pub fn run(args: Args) -> Result<()> {
    run_args(&args)
}

fn run_args(args: &Args) -> Result<()> {
    if args.action != "transform" {
        bail!("kgx: only `transform` is implemented (got `{}`)", args.action);
    }
    let fmt = args.input_format.as_deref().unwrap_or("obojson");
    if fmt != "obojson" {
        bail!("kgx transform: only --input-format obojson is implemented (got `{fmt}`)");
    }
    let out_fmt = args.output_format.as_deref().unwrap_or("tsv");
    if out_fmt != "tsv" {
        bail!("kgx transform: only --output-format tsv is implemented (got `{out_fmt}`)");
    }
    let base = args
        .output
        .clone()
        .ok_or_else(|| anyhow::anyhow!("kgx transform: --output <BASE> is required"))?;
    let mut inputs = args.input.clone();
    inputs.extend(args.inputs.clone());
    if inputs.is_empty() {
        bail!("kgx transform: no input file given");
    }
    transform(&inputs, &base, &args.list_delimiter)
}

/// A prefix map, kept as (prefix, namespace) pairs so `contract_uri` can break
/// ties over the whole set.
type Cmap = Vec<(String, String)>;

fn load_map(src: &str) -> Cmap {
    let v: BTreeMap<String, serde_json::Value> = serde_json::from_str(src).unwrap_or_default();
    v.into_iter()
        .filter_map(|(k, val)| val.as_str().map(|s| (k, s.to_string())))
        .collect()
}

/// Contract a URI against the given prefix maps: every prefix whose namespace the
/// URI starts with yields a candidate, and only the SHORTEST candidates survive.
/// Where several tie on length the lexicographically first wins, so the CURIE a
/// URI contracts to is the same on every run.
fn contract_uri(uri: &str, cmaps: &[&Cmap]) -> Option<String> {
    let mut curies: BTreeSet<String> = BTreeSet::new();
    for cmap in cmaps {
        for (k, v) in cmap.iter() {
            if !v.is_empty() && uri.starts_with(v.as_str()) {
                curies.insert(format!("{k}:{}", &uri[v.len()..]));
            }
        }
    }
    let min = curies.iter().map(|c| c.len()).min()?;
    curies.into_iter().find(|c| c.len() == min)
}

struct Prefixes {
    runtime: Cmap,
    fallback: Vec<Cmap>,
}

impl Prefixes {
    fn new() -> Self {
        Prefixes {
            runtime: load_map(RUNTIME_PREFIX_MAP),
            fallback: vec![load_map(MONARCH_CONTEXT), load_map(OBO_CONTEXT)],
        }
    }
    /// Contract a URI: try the runtime map, else the two fallback contexts, else
    /// return the URI unchanged.
    fn contract(&self, uri: &str) -> String {
        if let Some(c) = contract_uri(uri, &[&self.runtime]) {
            return c;
        }
        let fb: Vec<&Cmap> = self.fallback.iter().collect();
        contract_uri(uri, &fb).unwrap_or_else(|| uri.to_string())
    }
}

/// The edge store built up before exporting — a multi-digraph keyed
/// `"{subject}-{predicate}-{object}"`.
///
/// Two consequences a streaming reading would miss, both visible on MONDO:
/// * adding an edge whose key already exists UPDATES it — the attribute maps are
///   merged — so where a subject/object pair is joined by more than one relation
///   only the LAST survives; every non-`is_a` predicate becomes
///   `biolink:related_to`, so the relation is not part of the key;
/// * the export order is the graph's: by SUBJECT in node-insertion order, then by
///   object in the order that subject first reached it, then by key — not the order
///   the OBO Graph JSON lists the edges in.
#[derive(Default)]
struct EdgeGraph {
    /// EVERY node in insertion order — the JSON nodes first, then each edge's
    /// subject and object as they are first mentioned. Edge export walks this list,
    /// so an edge is emitted at its subject's slot in THIS order, which is not the
    /// same as the order the subjects first appear as subjects: `IAO:0000002` is
    /// only ever a subject, while `BFO:0000054` was already added as some earlier
    /// edge's object.
    node_order: Vec<String>,
    seen_nodes: BTreeSet<String>,
    /// Per subject, its objects in insertion order.
    succ: BTreeMap<String, Vec<String>>,
    /// Per (subject, object), the edge keys in insertion order.
    keys: BTreeMap<(String, String), Vec<String>>,
    /// Per (subject, object, key), the surviving record.
    rows: BTreeMap<(String, String, String), Row>,
}

impl EdgeGraph {
    fn add_edge(&mut self, r: Row) {
        let (s, o) = match (r.get("subject"), r.get("object")) {
            (Some(s), Some(o)) => (s.clone(), o.clone()),
            _ => return,
        };
        let key = format!("{s}-{}-{o}", r.get("predicate").map(String::as_str).unwrap_or(""));
        self.add_node(&s);
        self.add_node(&o);
        let objs = self.succ.entry(s.clone()).or_default();
        if !objs.contains(&o) {
            objs.push(o.clone());
        }
        let ks = self.keys.entry((s.clone(), o.clone())).or_default();
        if !ks.contains(&key) {
            ks.push(key.clone());
        }
        // Re-adding an edge UPDATES the existing record rather than replacing it, so
        // the survivor keeps any key the newcomer does not set — in MONDO there are
        // pairs whose surviving record carries the `meta` of the edge that lost.
        match self.rows.get_mut(&(s.clone(), o.clone(), key.clone())) {
            Some(existing) => existing.extend(r),
            None => {
                self.rows.insert((s, o, key), r);
            }
        }
    }
    /// Record a node in insertion order (idempotent).
    fn add_node(&mut self, id: &str) {
        if self.seen_nodes.insert(id.to_string()) {
            self.node_order.push(id.to_string());
        }
    }
    fn edges_in_order(&self) -> Vec<Row> {
        let mut out = Vec::new();
        for s in &self.node_order {
            for o in self.succ.get(s).into_iter().flatten() {
                for k in self.keys.get(&(s.clone(), o.clone())).into_iter().flatten() {
                    if let Some(r) = self.rows.get(&(s.clone(), o.clone(), k.clone())) {
                        out.push(r.clone());
                    }
                }
            }
        }
        out
    }
}

/// One output record: ordered key → value, values already flattened to strings.
type Row = BTreeMap<String, String>;

/// The TSV column order: the core columns in a fixed order (those that occur),
/// then everything else sorted, then the underscore-prefixed "internal" ones
/// sorted.
fn order_columns(core: &[&str], present: &BTreeSet<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in core {
        if present.contains(*c) {
            out.push((*c).to_string());
        }
    }
    let mut rest: Vec<&String> =
        present.iter().filter(|c| !core.contains(&c.as_str()) && !c.starts_with('_')).collect();
    rest.sort();
    out.extend(rest.into_iter().cloned());
    let mut internal: Vec<&String> =
        present.iter().filter(|c| !core.contains(&c.as_str()) && c.starts_with('_')).collect();
    internal.sort();
    out.extend(internal.into_iter().cloned());
    out
}

/// Make a value fit in a TSV cell: newlines and tabs become spaces and a literal
/// `\"` is dropped. A list is sanitized element-wise, then joined on the delimiter.
fn sanitize(v: &str) -> String {
    v.replace('\n', " ").replace("\\\"", "").replace('\t', " ")
}

/// The Biolink category a node's CURIE prefix implies, defaulting to
/// `biolink:OntologyClass` for every other prefix. The prefix is all that is
/// consulted: deriving a category from the node's `hasOBONamespace` instead names
/// nothing the eight entries below do not already cover for the ontologies
/// released as KGX. The one category a `<ont>_nodes.tsv` carries beyond these is
/// `biolink:NamedThing`, which `transform` gives a node only an edge mentions.
fn category_for_prefix(curie: &str) -> &'static str {
    let prefix = curie.split(':').next().unwrap_or("");
    match prefix {
        "HP" => "biolink:PhenotypicFeature",
        "CHEBI" => "biolink:ChemicalSubstance",
        "MONDO" => "biolink:Disease",
        "UBERON" => "biolink:AnatomicalEntity",
        "SO" => "biolink:SequenceFeature",
        "CL" => "biolink:Cell",
        "PR" => "biolink:Protein",
        "NCBITaxon" => "biolink:OrganismTaxon",
        _ => "biolink:OntologyClass",
    }
}

const SKOS_EXACT_MATCH: &str = "http://www.w3.org/2004/02/skos/core#exactMatch";

fn transform(inputs: &[PathBuf], base: &Path, delim: &str) -> Result<()> {
    let pm = Prefixes::new();
    let mut node_rows: Vec<Row> = Vec::new();
    let mut graph = EdgeGraph::default();
    let mut node_cols: BTreeSet<String> = ["id", "name", "category", "description", "provided_by"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut edge_cols: BTreeSet<String> = [
        "id", "subject", "predicate", "object", "relation", "category", "knowledge_source",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    for input in inputs {
        // The provenance default is the input file's BASENAME.
        let provided_by = input
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| input.display().to_string());
        let text = std::fs::read_to_string(input)
            .with_context(|| format!("reading {}", input.display()))?;
        let doc: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing {} as OBO Graph JSON", input.display()))?;
        let graphs = doc
            .get("graphs")
            .and_then(|g| g.as_array())
            .cloned()
            .unwrap_or_default();
        for g in &graphs {
            for n in g.get("nodes").and_then(|x| x.as_array()).into_iter().flatten() {
                if let Some(r) = read_node(n, &pm, &provided_by, delim, &mut node_cols) {
                    if let Some(id) = r.get("id") {
                        graph.add_node(id);
                    }
                    node_rows.push(r);
                }
            }
        }
        for g in &graphs {
            for e in g.get("edges").and_then(|x| x.as_array()).into_iter().flatten() {
                if let Some(r) = read_edge(e, &pm, &provided_by) {
                    edge_cols.extend(r.keys().cloned());
                    graph.add_edge(r);
                }
            }
        }
    }
    let edge_rows: Vec<Row> = graph.edges_in_order();

    // The transform is NOT streaming: everything is loaded into an in-memory graph
    // and that graph is exported. Adding an edge whose endpoint is not a node
    // creates the node, and such a node carries only the two defaults —
    // `biolink:NamedThing` as its category and `Graph` as its provenance. They land
    // after every real node, in the order the edges first mention them. MONDO leans
    // on this: `mondo.json` describes far fewer nodes than `mondo_nodes.tsv` has
    // rows.
    {
        let have: BTreeSet<String> =
            node_rows.iter().filter_map(|r| r.get("id").cloned()).collect();
        for id in &graph.node_order {
            if !have.contains(id) {
                let mut r = Row::new();
                r.insert("id".to_string(), id.clone());
                r.insert("category".to_string(), "biolink:NamedThing".to_string());
                r.insert("provided_by".to_string(), "Graph".to_string());
                node_cols.extend(r.keys().cloned());
                node_rows.push(r);
            }
        }
    }

    // Node columns are the DECLARED set — a column stays even when every value is
    // empty (see `put_list`). Edge columns are value-driven instead: `category` and
    // `provided_by` are edge defaults that an OBO Graph edge never carries, and
    // neither appears in the output.
    let used_node: BTreeSet<String> = node_cols.clone();
    let used_edge: BTreeSet<String> =
        edge_cols.iter().filter(|c| edge_rows.iter().any(|r| r.contains_key(*c))).cloned().collect();
    let ncols = order_columns(
        &["id", "category", "name", "description", "xref", "provided_by", "synonym"],
        &used_node,
    );
    let ecols = order_columns(
        &["id", "subject", "predicate", "object", "category", "relation", "provided_by"],
        &used_edge,
    );

    write_tsv(&with_suffix(base, "_nodes.tsv"), &ncols, &node_rows)?;
    write_tsv(&with_suffix(base, "_edges.tsv"), &ecols, &edge_rows)?;
    status!(
        "kgx: wrote {} ({} nodes), {} ({} edges)",
        with_suffix(base, "_nodes.tsv").display(),
        node_rows.len(),
        with_suffix(base, "_edges.tsv").display(),
        edge_rows.len()
    );
    Ok(())
}

fn with_suffix(base: &Path, suffix: &str) -> PathBuf {
    let mut s = base.as_os_str().to_string_lossy().to_string();
    s.push_str(suffix);
    PathBuf::from(s)
}

fn write_tsv(path: &Path, cols: &[String], rows: &[Row]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let f = std::fs::File::create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    let mut w = std::io::BufWriter::new(f);
    writeln!(w, "{}", cols.join("\t"))?;
    for r in rows {
        let line: Vec<&str> =
            cols.iter().map(|c| r.get(c).map(String::as_str).unwrap_or("")).collect();
        writeln!(w, "{}", line.join("\t"))?;
    }
    w.flush()?;
    Ok(())
}

/// A list-valued property: DEDUPED AND SORTED, then joined on the delimiter.
///
/// Every list-typed column is sorted, so a node's `synonym`, `xref`, `same_as`,
/// `subsets` and the four scoped synonym columns all come out in ascending order
/// with duplicates collapsed — not in the order the OBO Graph JSON lists them.
fn put_list(row: &mut Row, cols: &mut BTreeSet<String>, key: &str, vals: Vec<String>, delim: &str) {
    // The COLUMN is declared even when the value is empty. A node with any
    // `synonyms` at all contributes all five synonym keys, and the column set is
    // fixed from those KEYS; only the value is dropped when it turns out empty, and
    // that is after the column set is settled. So a graph with only exact synonyms
    // still gets `broad_synonyms` and `narrow_synonyms` columns, both blank.
    cols.insert(key.to_string());
    let mut set: BTreeSet<String> = BTreeSet::new();
    for v in vals {
        set.insert(sanitize(&v));
    }
    if set.is_empty() {
        return;
    }
    let joined = set.into_iter().collect::<Vec<_>>().join(delim);
    if !joined.is_empty() {
        row.insert(key.to_string(), joined);
    }
}

/// Python's `str()` of a JSON value: the rendering a non-list, non-bool property
/// takes in the TSV. An edge's `meta` is a mapping, so the cell holds its Python
/// repr rather than JSON.
fn py_repr(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "None".to_string(),
        serde_json::Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => py_str(s),
        serde_json::Value::Array(a) => {
            format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", "))
        }
        serde_json::Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's `repr` of a string: single quotes, switching to double quotes when the
/// text contains a `'` but no `"`.
fn py_str(s: &str) -> String {
    let esc = |q: char, s: &str| -> String {
        let mut out = String::new();
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                c if c == q => {
                    out.push('\\');
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c => out.push(c),
            }
        }
        out
    };
    if s.contains('\'') && !s.contains('"') {
        format!("\"{}\"", esc('"', s))
    } else {
        format!("'{}'", esc('\'', s))
    }
}

fn put(row: &mut Row, key: &str, val: &str) {
    if !val.is_empty() {
        row.insert(key.to_string(), sanitize(val));
    }
}

/// Synonym `val`s of a given `pred` (all of them when `pred` is None).
fn syns(meta: &serde_json::Value, pred: Option<&str>) -> Vec<String> {
    meta.get("synonyms")
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter(|s| match pred {
                    None => s.get("val").is_some(),
                    Some(p) => s.get("pred").and_then(|x| x.as_str()) == Some(p),
                })
                .filter_map(|s| s.get("val").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn read_node(
    n: &serde_json::Value,
    pm: &Prefixes,
    provided_by: &str,
    delim: &str,
    cols: &mut BTreeSet<String>,
) -> Option<Row> {
    let id = n.get("id")?.as_str()?;
    let curie = pm.contract(id);
    let mut row = Row::new();
    row.insert("id".to_string(), curie.clone());
    if let Some(lbl) = n.get("lbl").and_then(|v| v.as_str()) {
        cols.insert("name".to_string());
        put(&mut row, "name", lbl);
    }
    cols.insert("iri".to_string());
    put(&mut row, "iri", id);
    if let Some(meta) = n.get("meta") {
        if let Some(d) = meta.pointer("/definition/val").and_then(|v| v.as_str()) {
            cols.insert("description".to_string());
            put(&mut row, "description", d);
        }
        // `subsets` keeps only the fragment after `#`.
        let subsets: Vec<String> = meta
            .get("subsets")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|x| match x.split_once('#') {
                        Some((_, frag)) => frag.to_string(),
                        None => x.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if meta.get("subsets").is_some() {
            put_list(&mut row, cols, "subsets", subsets, delim);
        }
        if meta.get("synonyms").is_some() {
            put_list(&mut row, cols, "synonym", syns(meta, None), delim);
            put_list(&mut row, cols, "exact_synonyms", syns(meta, Some("hasExactSynonym")), delim);
            put_list(&mut row, cols, "related_synonyms", syns(meta, Some("hasRelatedSynonym")), delim);
            put_list(&mut row, cols, "broad_synonyms", syns(meta, Some("hasBroadSynonym")), delim);
            put_list(&mut row, cols, "narrow_synonyms", syns(meta, Some("hasNarrowSynonym")), delim);
        }
        let xrefs: Vec<String> = meta
            .get("xrefs")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter().filter_map(|x| x.get("val").and_then(|v| v.as_str()).map(str::to_string)).collect()
            })
            .unwrap_or_default();
        if meta.get("xrefs").is_some() {
            put_list(&mut row, cols, "xref", xrefs, delim);
        }
        if meta.get("deprecated").is_some() {
            cols.insert("deprecated".to_string());
        }
        if meta.get("deprecated").and_then(|v| v.as_bool()) == Some(true) {
            // A boolean is rendered as Python's `str(True)`.
            row.insert("deprecated".to_string(), "True".to_string());
        }
        let same_as: Vec<String> = meta
            .get("basicPropertyValues")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter(|p| p.get("pred").and_then(|x| x.as_str()) == Some(SKOS_EXACT_MATCH))
                    .filter_map(|p| p.get("val").and_then(|v| v.as_str()))
                    .map(|v| pm.contract(v))
                    .collect()
            })
            .unwrap_or_default();
        // Declared unconditionally: the `same_as` column exists as soon as any node
        // has a `meta` block at all, whether or not it holds an exact match.
        put_list(&mut row, cols, "same_as", same_as, delim);
    }
    row.insert("category".to_string(), category_for_prefix(&curie).to_string());
    row.insert("provided_by".to_string(), provided_by.to_string());
    Some(row)
}

fn read_edge(e: &serde_json::Value, pm: &Prefixes, provided_by: &str) -> Option<Row> {
    let sub = e.get("sub")?.as_str()?;
    let pred = e.get("pred")?.as_str()?;
    let obj = e.get("obj")?.as_str()?;
    let mut row = Row::new();
    // A fresh uuid4 per edge. This is the one column the export cannot make
    // reproducible: two runs over the same input differ here and nowhere else.
    row.insert("id".to_string(), format!("urn:uuid:{}", uuid4()));
    row.insert("subject".to_string(), pm.contract(sub));
    if is_iri(pred) {
        // An IRI-shaped predicate gets no Biolink lookup at all: it becomes
        // `biolink:related_to` with the contracted IRI as `relation`. So the spelling
        // decides the mapping — BFO:0000050/51 written as IRIs land here, not in the
        // part_of/has_part mapping the non-IRI branch below gives their bare names.
        row.insert("predicate".to_string(), "biolink:related_to".to_string());
        row.insert("relation".to_string(), pm.contract(pred));
    } else {
        let (p, r) = match pred {
            "is_a" => ("biolink:subclass_of".to_string(), "rdfs:subClassOf".to_string()),
            "has_part" => ("biolink:has_part".to_string(), "BFO:0000051".to_string()),
            "part_of" => ("biolink:part_of".to_string(), "BFO:0000050".to_string()),
            other => (format!("biolink:{}", other.replace(' ', "_")), other.to_string()),
        };
        row.insert("predicate".to_string(), p);
        row.insert("relation".to_string(), r);
    }
    row.insert("object".to_string(), pm.contract(obj));
    // Any other key on the edge record is carried through (`meta`), rendered the
    // way Python's `str()` would render the parsed JSON value.
    if let Some(m) = e.as_object() {
        for (k, v) in m {
            if k == "sub" || k == "pred" || k == "obj" {
                continue;
            }
            match v {
                serde_json::Value::String(s) => put(&mut row, k, s),
                serde_json::Value::Null => {}
                other => put(&mut row, k, &py_repr(other)),
            }
        }
    }
    row.insert("knowledge_source".to_string(), provided_by.to_string());
    Some(row)
}

fn is_iri(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://") || s.starts_with("urn:")
}

/// A random version-4 UUID in the usual 8-4-4-4-12 hex rendering. Not seeded or
/// reproducible — see the module docs.
fn uuid4() -> String {
    let mut b = [0u8; 16];
    getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = |r: &[u8]| r.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!("{}-{}-{}-{}-{}", h(&b[0..4]), h(&b[4..6]), h(&b[6..8]), h(&b[8..10]), h(&b[10..16]))
}

fn getrandom(buf: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(buf).is_ok() {
            return;
        }
    }
    // Fallback: a process-local counter mixed with the address of a stack slot.
    let seed = (&buf as *const _ as usize) as u64;
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    for b in buf.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x & 0xff) as u8;
    }
}
