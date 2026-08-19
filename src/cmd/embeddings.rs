//! `embeddings` — build, update, average, semantic-similarity, and search over
//! ontology term embeddings, in the parquet layout OLS4 loads.
//!
//! Subcommands (see [`Sub`]):
//!  * `embed`   — take the `extract-strings` table, embed each unique string via the
//!               OpenAI embeddings API, and write the term/embedding parquet. With
//!               `--existing` it reuses vectors for unchanged strings, keyed on the
//!               content hash, so only genuinely new strings reach the API.
//!  * `average` — one averaged, L2-normalised vector per term (drops CURATION rows).
//!  * `semsim`  — all-pairs cosine between two ontologies' term vectors,
//!               thresholded.
//!  * `search`  — nearest terms to a query string (embedded) or a known term (`--like`).
//!
//! Vectors are stored/read from the 11-column parquet (`embedding: List<Float32>`).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;

use arrow_array::{Array, ArrayRef, Float32Array, Float64Array, RecordBatch, StringArray};
use arrow_array::builder::{Float32Builder, ListBuilder};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use rayon::prelude::*;

pub(crate) const DEFAULT_MODEL: &str = "text-embedding-3-large";
pub(crate) const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const BATCH_SIZE: usize = 2000;
const MAX_RETRIES: u32 = 100;
const INITIAL_BACKOFF_MS: u64 = 1000;
const MAX_BACKOFF_MS: u64 = 32000;
/// Column order of the embedding parquet, as OLS4 loads it (`ecto.parquet`).
const COLUMNS: [&str; 10] = [
    "pk",
    "ontology_id",
    "entity_type",
    "iri",
    "label",
    "hash",
    "text_to_embed",
    "string_type",
    "curated_from_source",
    "curated_from_subject_categories",
];
/// Rows per output parquet record batch (bounds writer memory).
const WRITE_CHUNK: usize = 4096;

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Sub,
}

#[derive(clap::Subcommand)]
pub enum Sub {
    /// Embed the `extract-strings` table into the term/embedding parquet.
    Embed(EmbedArgs),
    /// Average per-term vectors (drop CURATION, mean, L2-normalise).
    Average(AverageArgs),
    /// All-pairs cosine similarity between two ontologies, thresholded (TSV out).
    Semsim(SemsimArgs),
    /// Find the nearest terms to a query string or a known term.
    Search(SearchArgs),
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> Result<Option<crate::model::Model>> {
    match &args.cmd {
        Sub::Embed(a) => embed(a)?,
        Sub::Average(a) => average(a)?,
        Sub::Semsim(a) => semsim(a)?,
        Sub::Search(a) => search(a)?,
    }
    // Side-effect commands: pass any piped model through unchanged.
    Ok(piped)
}

// ---------------------------------------------------------------------------
// In-memory row
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub(crate) struct Row {
    pub(crate) pk: String,
    pub(crate) ontology_id: String,
    pub(crate) entity_type: String,
    pub(crate) iri: String,
    pub(crate) label: String,
    pub(crate) hash: String,
    pub(crate) text_to_embed: String,
    pub(crate) string_type: String,
    pub(crate) curated_from_source: String,
    pub(crate) curated_from_subject_categories: String,
    pub(crate) embedding: Option<Vec<f32>>,
}

// ===========================================================================
// embed
// ===========================================================================

#[derive(ClapArgs)]
pub struct EmbedArgs {
    /// Input: the `extract-strings` table (`.tsv`) or an embedding parquet.
    #[arg(short, long)]
    pub input: PathBuf,
    /// Output embedding parquet.
    #[arg(short, long)]
    pub output: PathBuf,
    /// Existing embedding parquet to reuse vectors from (incremental update): a
    /// string whose content hash is already present is not re-embedded.
    #[arg(long)]
    pub existing: Option<PathBuf>,
    /// Embedding model. Defaults to text-embedding-3-large (matches OLS `ecto.parquet`).
    #[arg(long, default_value = DEFAULT_MODEL)]
    pub model: String,
    /// OpenAI (or compatible) API base URL.
    #[arg(long = "base-url", default_value = DEFAULT_BASE_URL)]
    pub base_url: String,
    /// API key. Falls back to $OPENAI_API_KEY, then $OWLMAKE_EMBEDDINGS_API_KEY.
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
    /// Inputs per API request.
    #[arg(long = "batch-size", default_value_t = BATCH_SIZE)]
    pub batch_size: usize,
}

fn embed(args: &EmbedArgs) -> Result<()> {
    let mut rows = read_rows_any(&args.input)?;

    // Seed the hash→vector cache from --existing, then from any embeddings already
    // present in the input (so re-running is idempotent).
    let mut cache: HashMap<String, Vec<f32>> = HashMap::new();
    if let Some(prev) = &args.existing {
        for (h, v) in read_hash_vectors(prev)? {
            cache.insert(h, v);
        }
    }
    for r in &rows {
        if let Some(v) = &r.embedding {
            cache.entry(r.hash.clone()).or_insert_with(|| v.clone());
        }
    }

    // Unique strings still needing an embedding.
    let mut seen = HashSet::new();
    let mut to_embed: Vec<(String, String)> = Vec::new();
    for r in &rows {
        if cache.contains_key(&r.hash) {
            continue;
        }
        if seen.insert(r.hash.clone()) {
            to_embed.push((r.hash.clone(), r.text_to_embed.clone()));
        }
    }

    let cached = rows.len() - rows.iter().filter(|r| !cache.contains_key(&r.hash)).count();
    status!(
        "embed: {} rows, {} cached, {} unique strings to embed",
        rows.len(),
        cached,
        to_embed.len()
    );

    if !to_embed.is_empty() {
        let key = resolve_api_key(args.api_key.as_deref())?;
        let base = args.base_url.trim_end_matches('/');
        let url = format!("{base}/embeddings");
        let texts: Vec<String> = to_embed.iter().map(|(_, t)| t.clone()).collect();
        let vectors = embed_texts(&url, &key, &args.model, &texts, args.batch_size)?;
        for ((h, _), v) in to_embed.iter().zip(vectors) {
            cache.insert(h.clone(), v);
        }
    }

    // Join vectors back onto every row.
    for r in &mut rows {
        r.embedding = Some(
            cache
                .get(&r.hash)
                .cloned()
                .with_context(|| format!("no embedding for hash {} (internal error)", r.hash))?,
        );
    }

    write_parquet(&args.output, &rows)?;
    status!("embed: wrote {} rows to {}", rows.len(), args.output.display());
    Ok(())
}

pub(crate) fn resolve_api_key(flag: Option<&str>) -> Result<String> {
    if let Some(k) = flag {
        return Ok(k.to_string());
    }
    for var in ["OPENAI_API_KEY", "OWLMAKE_EMBEDDINGS_API_KEY"] {
        if let Ok(k) = std::env::var(var) {
            if !k.is_empty() {
                return Ok(k);
            }
        }
    }
    bail!("no API key: pass --api-key or set OPENAI_API_KEY / OWLMAKE_EMBEDDINGS_API_KEY")
}

/// Sanitise a string before embedding: strip
/// `[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]`, and replace an emptied string with
/// `[empty]` so every row still has a text to embed.
fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|&c| {
            let b = c as u32;
            !(b <= 0x08 || b == 0x0b || b == 0x0c || (0x0e..=0x1f).contains(&b) || b == 0x7f)
        })
        .collect();
    if cleaned.is_empty() {
        "[empty]".to_string()
    } else {
        cleaned
    }
}

/// Embed `texts` in order: batched requests, exponential backoff on rate limits
/// and server errors, and a halving split on 400 so one oversized batch cannot
/// lose the run.
pub(crate) fn embed_texts(
    url: &str,
    key: &str,
    model: &str,
    texts: &[String],
    batch_size: usize,
) -> Result<Vec<Vec<f32>>> {
    let sanitized: Vec<String> = texts.iter().map(|t| sanitize(t)).collect();
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(sanitized.len());
    let total = sanitized.len();
    let batch_size = batch_size.max(1);
    for (n, chunk) in sanitized.chunks(batch_size).enumerate() {
        let done = n * batch_size;
        status!("embed: requesting {}..{} / {}", done, done + chunk.len(), total);
        embed_single_batch(url, key, model, chunk, &mut out)?;
    }
    Ok(out)
}

fn embed_single_batch(
    url: &str,
    key: &str,
    model: &str,
    batch: &[String],
    out: &mut Vec<Vec<f32>>,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let mut backoff = INITIAL_BACKOFF_MS;
    let mut retries = 0u32;
    loop {
        let body = serde_json::json!({ "model": model, "input": batch });
        let bytes = serde_json::to_vec(&body)?;
        let (code, resp) = crate::io::http_post_json(url, Some(key), &bytes)?;
        match code {
            200 => {
                let parsed: EmbResponse = serde_json::from_slice(&resp)
                    .context("parsing embeddings API response")?;
                let mut data = parsed.data;
                data.sort_by_key(|d| d.index);
                if data.len() != batch.len() {
                    bail!("API returned {} vectors for {} inputs", data.len(), batch.len());
                }
                out.extend(data.into_iter().map(|d| d.embedding));
                return Ok(());
            }
            // Payload too large / invalid: split the batch and recurse to singletons.
            400 => {
                if batch.len() == 1 {
                    bail!(
                        "400 BadRequest embedding a single string: {}",
                        String::from_utf8_lossy(&resp)
                    );
                }
                let half = batch.len() / 2;
                status!("embed: 400 on batch of {}, splitting {}+{}", batch.len(), half, batch.len() - half);
                embed_single_batch(url, key, model, &batch[..half], out)?;
                embed_single_batch(url, key, model, &batch[half..], out)?;
                return Ok(());
            }
            // Rate-limit / server error: exponential backoff.
            429 | 500..=599 => {
                if retries >= MAX_RETRIES {
                    bail!("HTTP {code} after {MAX_RETRIES} retries: {}", String::from_utf8_lossy(&resp));
                }
                status!("embed: HTTP {code}, retrying in {}ms (attempt {})", backoff, retries + 1);
                std::thread::sleep(std::time::Duration::from_millis(backoff));
                backoff = (backoff * 2).min(MAX_BACKOFF_MS);
                retries += 1;
            }
            other => bail!("HTTP {other}: {}", String::from_utf8_lossy(&resp)),
        }
    }
}

#[derive(serde::Deserialize)]
struct EmbResponse {
    data: Vec<EmbDatum>,
}
#[derive(serde::Deserialize)]
struct EmbDatum {
    embedding: Vec<f32>,
    index: usize,
}

// ===========================================================================
// average
// ===========================================================================

#[derive(ClapArgs)]
pub struct AverageArgs {
    /// Input embedding parquet (per-string vectors).
    #[arg(short, long)]
    pub input: PathBuf,
    /// Output parquet (one averaged, unit-normalised vector per term).
    #[arg(short, long)]
    pub output: PathBuf,
}

fn average(args: &AverageArgs) -> Result<()> {
    let rows = read_rows(&args.input)?;

    // Group by pk in first-seen order; CURATION rows do not contribute to a term's
    // averaged vector.
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (Row, Vec<f32>, usize)> = HashMap::new();
    let mut dim = 0usize;
    for r in rows {
        if r.string_type == "CURATION" {
            continue;
        }
        let Some(emb) = &r.embedding else { continue };
        if dim == 0 {
            dim = emb.len();
        } else if emb.len() != dim {
            bail!("inconsistent embedding dim: {} vs {}", emb.len(), dim);
        }
        match groups.get_mut(&r.pk) {
            Some((_, sum, count)) => {
                for (s, x) in sum.iter_mut().zip(emb) {
                    *s += *x;
                }
                *count += 1;
            }
            None => {
                order.push(r.pk.clone());
                groups.insert(r.pk.clone(), (r.clone(), emb.clone(), 1));
            }
        }
    }

    let mut out = Vec::with_capacity(order.len());
    for pk in &order {
        let (mut meta, sum, count) = groups.remove(pk).unwrap();
        let mut avg: Vec<f32> = sum.iter().map(|s| s / count as f32).collect();
        let norm: f32 = avg.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm = if norm == 0.0 { 1.0 } else { norm };
        for x in &mut avg {
            *x /= norm;
        }
        // Per-term row: keep metadata, blank the per-string fields.
        meta.hash.clear();
        meta.text_to_embed.clear();
        meta.string_type = "AVERAGE".to_string();
        meta.curated_from_source.clear();
        meta.curated_from_subject_categories.clear();
        meta.embedding = Some(avg);
        out.push(meta);
    }

    write_parquet(&args.output, &out)?;
    status!("average: {} terms → {}", out.len(), args.output.display());
    Ok(())
}

// ===========================================================================
// semsim
// ===========================================================================

#[derive(ClapArgs)]
pub struct SemsimArgs {
    /// Input per-term embedding parquet (typically the `average` output).
    #[arg(short, long)]
    pub input: PathBuf,
    /// Subject ontology id (OLS `--a`).
    #[arg(long = "subject-ontology", visible_alias = "a")]
    pub subject_ontology: String,
    /// Object ontology id (OLS `--b`).
    #[arg(long = "object-ontology", visible_alias = "b")]
    pub object_ontology: String,
    /// Inclusive cosine threshold.
    #[arg(long)]
    pub threshold: f32,
    /// Output TSV. Defaults to stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Skip the pair-count guardrail (allow very large all-pairs runs).
    #[arg(long)]
    pub force: bool,
}

struct Item {
    iri: String,
    label: String,
    emb: Vec<f32>,
    norm: f32,
}

fn semsim(args: &SemsimArgs) -> Result<()> {
    let rows = read_rows(&args.input)?;
    let load = |ont: &str| -> Vec<Item> {
        rows.iter()
            .filter(|r| r.ontology_id == ont && r.embedding.is_some())
            .map(|r| {
                let emb = r.embedding.clone().unwrap();
                let norm = l2_norm(&emb);
                Item { iri: r.iri.clone(), label: r.label.clone(), emb, norm }
            })
            .collect()
    };
    let items_a = load(&args.subject_ontology);
    let items_b = load(&args.object_ontology);
    if items_a.is_empty() || items_b.is_empty() {
        bail!(
            "semsim: no vectors for {} ({}) or {} ({})",
            args.subject_ontology,
            items_a.len(),
            args.object_ontology,
            items_b.len()
        );
    }

    let pairs = items_a.len() as u128 * items_b.len() as u128;
    status!(
        "semsim: {} × {} = {} pairs (threshold {})",
        items_a.len(),
        items_b.len(),
        pairs,
        args.threshold
    );
    const GUARD: u128 = 2_000_000_000;
    if pairs > GUARD && !args.force {
        bail!(
            "semsim: {pairs} pairs exceeds the {GUARD} guardrail — reduce the set, use a \
             PCA-reduced parquet, or pass --force"
        );
    }

    // All-pairs cosine, parallel over subjects.
    let mut w: Box<dyn std::io::Write> = match &args.output {
        Some(p) => Box::new(std::io::BufWriter::new(File::create(p)?)),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };
    writeln!(w, "subject_id\tsubject_label\tobject_id\tobject_label\tcosine_similarity")?;

    let blocks: Vec<String> = items_a
        .par_iter()
        .map(|a| {
            let mut matches: Vec<(f32, &Item)> = items_b
                .iter()
                .filter_map(|b| {
                    let denom = a.norm * b.norm;
                    if denom == 0.0 {
                        return None;
                    }
                    let cos = dot(&a.emb, &b.emb) / denom;
                    if cos >= args.threshold {
                        Some((cos, b))
                    } else {
                        None
                    }
                })
                .collect();
            matches.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut s = String::new();
            for (cos, b) in matches {
                s.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{:.6}\n",
                    a.iri, a.label, b.iri, b.label, cos
                ));
            }
            s
        })
        .collect();

    let mut n = 0u64;
    for block in blocks {
        n += block.matches('\n').count() as u64;
        w.write_all(block.as_bytes())?;
    }
    w.flush()?;
    status!("semsim: {n} pairs ≥ {}", args.threshold);
    Ok(())
}

// ===========================================================================
// search
// ===========================================================================

#[derive(ClapArgs)]
pub struct SearchArgs {
    /// Input embedding parquet to search.
    #[arg(short, long)]
    pub input: PathBuf,
    /// Query text (embedded with --model). Mutually exclusive with --like.
    #[arg(long)]
    pub query: Option<String>,
    /// Use the stored vector of this entity IRI as the query (offline; nearest
    /// neighbours of a known term).
    #[arg(long)]
    pub like: Option<String>,
    /// Number of terms to return.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    /// Output format: table (default) or json.
    #[arg(short, long, default_value = "table")]
    pub format: String,
    /// Embedding model for --query.
    #[arg(long, default_value = DEFAULT_MODEL)]
    pub model: String,
    #[arg(long = "base-url", default_value = DEFAULT_BASE_URL)]
    pub base_url: String,
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
}

fn search(args: &SearchArgs) -> Result<()> {
    let rows = read_rows(&args.input)?;

    // Resolve the query vector.
    let query: Vec<f32> = match (&args.query, &args.like) {
        (Some(_), Some(_)) => bail!("search: pass only one of --query / --like"),
        (Some(text), None) => {
            let key = resolve_api_key(args.api_key.as_deref())?;
            let base = args.base_url.trim_end_matches('/');
            let url = format!("{base}/embeddings");
            embed_texts(&url, &key, &args.model, std::slice::from_ref(text), 1)?
                .into_iter()
                .next()
                .context("no query embedding returned")?
        }
        (None, Some(iri)) => rows
            .iter()
            .find(|r| &r.iri == iri && r.embedding.is_some())
            .and_then(|r| r.embedding.clone())
            .with_context(|| format!("no embedded term with iri {iri}"))?,
        (None, None) => bail!("search: provide --query <text> or --like <iri>"),
    };
    let qnorm = l2_norm(&query);
    if qnorm == 0.0 {
        bail!("search: query vector has zero norm");
    }

    // Best cosine per term (pk), then top-k.
    let mut best: HashMap<String, (f32, String, String, String)> = HashMap::new();
    for r in &rows {
        let Some(emb) = &r.embedding else { continue };
        let denom = qnorm * l2_norm(emb);
        if denom == 0.0 {
            continue;
        }
        let cos = dot(&query, emb) / denom;
        let e = best.entry(r.pk.clone()).or_insert((f32::MIN, String::new(), String::new(), String::new()));
        if cos > e.0 {
            *e = (cos, r.iri.clone(), r.label.clone(), r.text_to_embed.clone());
        }
    }
    let mut ranked: Vec<(f32, String, String, String)> = best.into_values().collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(args.limit);

    if args.format.eq_ignore_ascii_case("json") {
        let arr: Vec<serde_json::Value> = ranked
            .iter()
            .map(|(score, iri, label, matched)| {
                serde_json::json!({ "score": score, "iri": iri, "label": label, "matched": matched })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else {
        for (score, iri, label, matched) in &ranked {
            println!("{score:.4}\t{iri}\t{label}\t{matched}");
        }
    }
    Ok(())
}

// ===========================================================================
// vector math
// ===========================================================================

pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
pub(crate) fn l2_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

// ===========================================================================
// parquet / tsv I/O
// ===========================================================================

/// Read `input` as either the `extract-strings` TSV or an embedding parquet.
fn read_rows_any(input: &Path) -> Result<Vec<Row>> {
    let is_parquet = input
        .extension()
        .map(|e| e.eq_ignore_ascii_case("parquet"))
        .unwrap_or(false);
    if is_parquet {
        read_rows(input)
    } else {
        read_tsv(input)
    }
}

/// Parse the `extract-strings` TSV (header-named columns; embedding absent).
fn read_tsv(path: &Path) -> Result<Vec<Row>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().context("empty TSV")?.split('\t').collect();
    let idx = |name: &str| header.iter().position(|h| *h == name);
    let (i_pk, i_ont, i_type, i_iri, i_label, i_hash, i_text, i_stype) = (
        idx("pk"),
        idx("ontology_id"),
        idx("entity_type"),
        idx("iri"),
        idx("label"),
        idx("hash").context("TSV missing 'hash' column")?,
        idx("text_to_embed").context("TSV missing 'text_to_embed' column")?,
        idx("string_type"),
    );
    let i_src = idx("curated_from_source");
    let i_cat = idx("curated_from_subject_categories");
    let get = |f: &[&str], i: Option<usize>| i.and_then(|i| f.get(i)).unwrap_or(&"").to_string();

    let mut rows = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        rows.push(Row {
            pk: get(&f, i_pk),
            ontology_id: get(&f, i_ont),
            entity_type: get(&f, i_type),
            iri: get(&f, i_iri),
            label: get(&f, i_label),
            hash: f.get(i_hash).unwrap_or(&"").to_string(),
            text_to_embed: f.get(i_text).unwrap_or(&"").to_string(),
            string_type: get(&f, i_stype),
            curated_from_source: get(&f, i_src),
            curated_from_subject_categories: get(&f, i_cat),
            embedding: None,
        });
    }
    Ok(rows)
}

/// Read all rows (with embeddings) from an embedding parquet.
pub(crate) fn read_rows(path: &Path) -> Result<Vec<Row>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?
        .with_batch_size(WRITE_CHUNK)
        .build()?;
    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch?;
        append_batch(&batch, &mut rows)?;
    }
    Ok(rows)
}

/// Read just (hash → vector) from an embedding parquet (lighter than full rows).
fn read_hash_vectors(path: &Path) -> Result<Vec<(String, Vec<f32>)>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?
        .with_batch_size(WRITE_CHUNK)
        .build()?;
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch?;
        let hashes = str_col(&batch, "hash")?;
        let embs = emb_col(&batch, "embedding")?;
        for i in 0..batch.num_rows() {
            if let Some(v) = &embs[i] {
                out.push((hashes[i].clone(), v.clone()));
            }
        }
    }
    Ok(out)
}

fn append_batch(batch: &RecordBatch, rows: &mut Vec<Row>) -> Result<()> {
    let pk = str_col(batch, "pk")?;
    let ont = opt_str_col(batch, "ontology_id");
    let typ = opt_str_col(batch, "entity_type");
    let iri = opt_str_col(batch, "iri");
    let label = opt_str_col(batch, "label");
    let hash = opt_str_col(batch, "hash");
    let text = opt_str_col(batch, "text_to_embed");
    let stype = opt_str_col(batch, "string_type");
    let src = opt_str_col(batch, "curated_from_source");
    let cat = opt_str_col(batch, "curated_from_subject_categories");
    let embs = emb_col(batch, "embedding")?;
    let g = |c: &Option<Vec<String>>, i: usize| c.as_ref().map(|v| v[i].clone()).unwrap_or_default();
    for i in 0..batch.num_rows() {
        rows.push(Row {
            pk: pk[i].clone(),
            ontology_id: g(&ont, i),
            entity_type: g(&typ, i),
            iri: g(&iri, i),
            label: g(&label, i),
            hash: g(&hash, i),
            text_to_embed: g(&text, i),
            string_type: g(&stype, i),
            curated_from_source: g(&src, i),
            curated_from_subject_categories: g(&cat, i),
            embedding: embs[i].clone(),
        });
    }
    Ok(())
}

fn str_col(batch: &RecordBatch, name: &str) -> Result<Vec<String>> {
    opt_str_col(batch, name).with_context(|| format!("parquet missing column '{name}'"))
}

fn opt_str_col(batch: &RecordBatch, name: &str) -> Option<Vec<String>> {
    let col = batch.column_by_name(name)?;
    let arr = col.as_any().downcast_ref::<StringArray>()?;
    Some(
        (0..arr.len())
            .map(|i| if arr.is_null(i) { String::new() } else { arr.value(i).to_string() })
            .collect(),
    )
}

fn emb_col(batch: &RecordBatch, name: &str) -> Result<Vec<Option<Vec<f32>>>> {
    let Some(col) = batch.column_by_name(name) else {
        return Ok(vec![None; batch.num_rows()]);
    };
    read_embeddings(col)
}

/// Extract a List/LargeList/FixedSizeList column of Float32/Float64 into per-row
/// f32 vectors. Every parquet owlmake writes — per-string or averaged — carries
/// `List<Float32>` (see `output_schema`), but an embedding parquet handed to it
/// may use any of arrow's three list layouts and either float width, so all of
/// them are read rather than rejected.
fn read_embeddings(col: &ArrayRef) -> Result<Vec<Option<Vec<f32>>>> {
    use arrow_array::{FixedSizeListArray, LargeListArray, ListArray};
    let n = col.len();
    let mut out = Vec::with_capacity(n);
    match col.data_type() {
        DataType::List(_) => {
            let la = col.as_any().downcast_ref::<ListArray>().unwrap();
            for i in 0..n {
                out.push(if la.is_null(i) { None } else { Some(values_f32(&la.value(i))?) });
            }
        }
        DataType::LargeList(_) => {
            let la = col.as_any().downcast_ref::<LargeListArray>().unwrap();
            for i in 0..n {
                out.push(if la.is_null(i) { None } else { Some(values_f32(&la.value(i))?) });
            }
        }
        DataType::FixedSizeList(_, _) => {
            let la = col.as_any().downcast_ref::<FixedSizeListArray>().unwrap();
            for i in 0..n {
                out.push(if la.is_null(i) { None } else { Some(values_f32(&la.value(i))?) });
            }
        }
        other => bail!("unexpected embedding column type {other:?}"),
    }
    Ok(out)
}

fn values_f32(arr: &ArrayRef) -> Result<Vec<f32>> {
    match arr.data_type() {
        DataType::Float32 => Ok(arr.as_any().downcast_ref::<Float32Array>().unwrap().values().to_vec()),
        DataType::Float64 => Ok(arr
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .values()
            .iter()
            .map(|&x| x as f32)
            .collect()),
        other => bail!("unexpected embedding value type {other:?}"),
    }
}

/// Output schema: the 10 string columns + `embedding: List<Float32>`.
fn output_schema() -> Arc<Schema> {
    let mut fields: Vec<Field> = COLUMNS.iter().map(|c| Field::new(*c, DataType::Utf8, true)).collect();
    fields.push(Field::new(
        "embedding",
        DataType::List(Arc::new(Field::new("item", DataType::Float32, true))),
        true,
    ));
    Arc::new(Schema::new(fields))
}

/// Write rows to a ZSTD-compressed parquet in the OLS-compatible 11-column schema,
/// chunked into record batches to bound memory.
fn write_parquet(path: &Path, rows: &[Row]) -> Result<()> {
    let schema = output_schema();
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build();
    let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(props))?;

    for chunk in rows.chunks(WRITE_CHUNK) {
        let mut cols: Vec<ArrayRef> = Vec::with_capacity(COLUMNS.len() + 1);
        let pick = |sel: &dyn Fn(&Row) -> &str| -> ArrayRef {
            Arc::new(StringArray::from(chunk.iter().map(|r| sel(r).to_string()).collect::<Vec<_>>()))
        };
        cols.push(pick(&|r| r.pk.as_str()));
        cols.push(pick(&|r| r.ontology_id.as_str()));
        cols.push(pick(&|r| r.entity_type.as_str()));
        cols.push(pick(&|r| r.iri.as_str()));
        cols.push(pick(&|r| r.label.as_str()));
        cols.push(pick(&|r| r.hash.as_str()));
        cols.push(pick(&|r| r.text_to_embed.as_str()));
        cols.push(pick(&|r| r.string_type.as_str()));
        cols.push(pick(&|r| r.curated_from_source.as_str()));
        cols.push(pick(&|r| r.curated_from_subject_categories.as_str()));

        let mut lb = ListBuilder::new(Float32Builder::new());
        for r in chunk {
            match &r.embedding {
                Some(v) => {
                    lb.values().append_slice(v);
                    lb.append(true);
                }
                None => lb.append(false),
            }
        }
        cols.push(Arc::new(lb.finish()));

        let batch = RecordBatch::try_new(schema.clone(), cols)?;
        writer.write(&batch)?;
    }
    writer.close()?;
    Ok(())
}
