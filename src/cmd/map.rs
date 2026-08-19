//! `map` — hybrid string→term mapping over a lexical, a semantic and a fuzzy
//! channel. Every candidate comes from the ontology data itself; there is no
//! curated-mappings channel.
//!
//! Each channel stamps an absolute confidence — one scale, so scores from different
//! channels are directly comparable and no weighted or rank-based fusion is needed
//! — and a suppression cascade then ranks them:
//!  * **lexical** — the text tagger run over the whole query: full coverage → 1.0
//!    (label) / 0.9 (synonym); partial → coverage·0.89.
//!    Dictionary from `--tagger-db` (published `.bin.gz` or `om text-tagger build`) or
//!    a transient in-memory build from `--input <ontology>`.
//!  * **semantic** — embed the query (same path as `om embeddings`) and cosine over an
//!    `--embeddings` parquet; score `(cos+1)/2`, gated ≥ `--min-similarity`, ·0.89.
//!  * **fuzzy** (`--fuzzy`) — token-Jaccard over term text, ·0.70.
//!
//! Cascade: keep-best-embedding → drop-weak-when-best-strong → dedup-by-IRI →
//! best-per-target-ontology → priority tie-break → sort desc, truncate `--limit`.
//! With `--top-n N` the suppression steps are skipped to return a ranked shortlist
//! (dedup-by-IRI only, capped at N).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;

use crate::cmd::embeddings;
use crate::cmd::extract_strings;
use crate::tag;

#[derive(ClapArgs)]
pub struct Args {
    /// File of query strings, one per line (a trailing tab-separated column, e.g. a
    /// property type, is ignored).
    #[arg(long)]
    pub queries: PathBuf,

    // --- lexical (tagger) dictionary source: one of these ---
    /// Prebuilt tagger DB (raw `.bin` or published `.bin.gz`).
    #[arg(long = "tagger-db")]
    pub tagger_db: Option<PathBuf>,
    /// Ontology to build a transient in-memory tagger from (when no --tagger-db).
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Ontology id for the transient build (`--input`).
    #[arg(long = "ontology-id")]
    pub ontology_id: Option<String>,
    /// Defining base IRI(s) for the transient build. Repeatable.
    #[arg(long = "base-iri")]
    pub base_iri: Vec<String>,
    /// OBO preferred prefix for the transient build.
    #[arg(long = "preferred-prefix")]
    pub preferred_prefix: Option<String>,
    /// Word-boundary delimiter characters for the tagger.
    #[arg(long)]
    pub delimiters: Option<String>,
    /// Drop matches contained within a longer match (OLS `includeSubstrings=false`).
    #[arg(long = "no-substrings")]
    pub no_substrings: bool,

    // --- semantic (embedding) channel ---
    /// Embedding parquet (`om embeddings build`); enables the semantic channel.
    #[arg(long)]
    pub embeddings: Option<PathBuf>,
    /// Embedding model for the query.
    #[arg(long, default_value = embeddings::DEFAULT_MODEL)]
    pub model: String,
    #[arg(long = "base-url", default_value = embeddings::DEFAULT_BASE_URL)]
    pub base_url: String,
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
    /// Minimum embedding score (`(cos+1)/2`) to keep a semantic hit.
    #[arg(long = "min-similarity", default_value_t = 0.7)]
    pub min_similarity: f32,

    // --- fuzzy channel ---
    /// Enable the token-Jaccard fuzzy channel (owlmake extension; needs a term source).
    #[arg(long)]
    pub fuzzy: bool,

    // --- shared ---
    /// Target ontology id(s), priority-ordered; filters and tie-breaks results.
    #[arg(long = "target-ontology")]
    pub target_ontology: Vec<String>,
    /// Max candidates per query.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    /// Return the top-N ranked candidates instead of zooma's single-best dedup.
    /// Skips the suppression cascade (keepBestEmbedding / suppressWeak /
    /// keepBestPerTargetOntology) so a query can yield several ranked candidates
    /// per ontology — useful when a downstream stage selects among them. Caps
    /// output at N (overriding --limit). Default: off (zooma parity).
    #[arg(long = "top-n")]
    pub top_n: Option<usize>,
    /// Output format: table (default) or json.
    #[arg(short, long, default_value = "table")]
    pub format: String,
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> Result<Option<crate::model::Model>> {
    run(args)?;
    Ok(piped)
}

/// A ranked mapping candidate.
#[derive(Clone)]
struct Candidate {
    iri: String,
    label: String,
    ontology_id: String,
    confidence: f32,
    channel: &'static str,
    matched: String,
}

fn run(args: &Args) -> Result<()> {
    // Queries.
    let qtext = std::fs::read_to_string(&args.queries)
        .with_context(|| format!("reading {}", args.queries.display()))?;
    let queries: Vec<String> = qtext
        .lines()
        .map(|l| l.split('\t').next().unwrap_or("").trim_end_matches('\r').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if queries.is_empty() {
        bail!("map: no queries in {}", args.queries.display());
    }

    // Lexical dictionary (tagger) — optional: skipped when no tagger source is
    // given but an --embeddings parquet is (semantic-/fuzzy-only mapping).
    let ac: Option<tag::ac::NerAc> = if args.tagger_db.is_some() || args.input.is_some() {
        Some(load_tagger(args)?)
    } else if args.embeddings.is_none() {
        bail!("map: provide a lexical source (--tagger-db or --input) and/or an --embeddings parquet");
    } else {
        None
    };
    let delimiters: Option<Vec<u8>> = args.delimiters.as_ref().map(|s| s.bytes().collect());

    // Term rows for the semantic + fuzzy channels (from the embeddings parquet).
    let term_rows: Vec<embeddings::Row> = match &args.embeddings {
        Some(p) => embeddings::read_rows(p)?,
        None => Vec::new(),
    };

    // Embed all queries once (semantic channel), if enabled.
    let query_vecs: Option<Vec<Vec<f32>>> = if args.embeddings.is_some() {
        let key = embeddings::resolve_api_key(args.api_key.as_deref())?;
        let base = args.base_url.trim_end_matches('/');
        let url = format!("{base}/embeddings");
        Some(embeddings::embed_texts(&url, &key, &args.model, &queries, 2000)?)
    } else {
        None
    };

    // Precompute term norms + a fuzzy token index if needed.
    let term_norms: Vec<f32> = term_rows
        .iter()
        .map(|r| r.embedding.as_deref().map(embeddings::l2_norm).unwrap_or(0.0))
        .collect();
    let fuzzy_index = if args.fuzzy {
        Some(build_fuzzy_index(&term_rows))
    } else {
        None
    };

    let targets: HashSet<&str> = args.target_ontology.iter().map(|s| s.as_str()).collect();

    let mut all_results: Vec<serde_json::Value> = Vec::new();
    for (qi, query) in queries.iter().enumerate() {
        let mut cands: Vec<Candidate> = Vec::new();

        // --- lexical channel (tagger over the whole query) ---
        if let Some(ac) = &ac {
            let mut spans = tag::annotate_text(ac, query, delimiters.as_deref());
            if args.no_substrings {
                remove_substrings(&mut spans);
            }
            let qlen = query.len() as f32;
            for e in spans {
                let coverage = if qlen > 0.0 {
                    (e.end - e.start) as f32 / qlen
                } else {
                    0.0
                };
                let is_synonym = e
                    .string_type
                    .as_deref()
                    .map(|t| !t.eq_ignore_ascii_case("LABEL"))
                    .unwrap_or(false);
                let confidence = if coverage >= 1.0 {
                    if is_synonym {
                        0.90
                    } else {
                        1.00
                    }
                } else {
                    coverage * 0.89
                };
                cands.push(Candidate {
                    iri: e.term_iri,
                    label: e.term_label,
                    ontology_id: e.ontology_id,
                    confidence,
                    channel: "lexical",
                    matched: query[e.start..e.end].to_string(),
                });
            }
        }

        // --- semantic channel ---
        if let Some(qvecs) = &query_vecs {
            let qv = &qvecs[qi];
            let qn = embeddings::l2_norm(qv);
            if qn > 0.0 {
                // best score per pk
                let mut best: HashMap<&str, (f32, &embeddings::Row)> = HashMap::new();
                for (i, r) in term_rows.iter().enumerate() {
                    let Some(emb) = &r.embedding else { continue };
                    let denom = qn * term_norms[i];
                    if denom == 0.0 {
                        continue;
                    }
                    let cos = embeddings::dot(qv, emb) / denom;
                    let score = (cos + 1.0) / 2.0;
                    let e = best.entry(&r.pk).or_insert((f32::MIN, r));
                    if score > e.0 {
                        *e = (score, r);
                    }
                }
                for (score, r) in best.into_values() {
                    if score < args.min_similarity {
                        continue;
                    }
                    cands.push(Candidate {
                        iri: r.iri.clone(),
                        label: r.label.clone(),
                        ontology_id: r.ontology_id.clone(),
                        confidence: score * 0.89,
                        channel: "semantic",
                        matched: r.text_to_embed.clone(),
                    });
                }
            }
        }

        // --- fuzzy channel ---
        if let Some(idx) = &fuzzy_index {
            for c in fuzzy_matches(query, idx, &term_rows) {
                cands.push(c);
            }
        }

        // --- fuse: cascade + rank ---
        let ranked = fuse(cands, &targets, &args.target_ontology, args.limit, args.top_n);

        if args.format.eq_ignore_ascii_case("json") {
            all_results.push(serde_json::json!({
                "query": query,
                "candidates": ranked.iter().map(|c| serde_json::json!({
                    "confidence": c.confidence,
                    "iri": c.iri,
                    "label": c.label,
                    "ontology_id": c.ontology_id,
                    "channel": c.channel,
                    "matched": c.matched,
                })).collect::<Vec<_>>(),
            }));
        } else {
            for c in &ranked {
                println!(
                    "{}\t{:.4}\t{}\t{}\t{}\t{}",
                    query, c.confidence, c.iri, c.label, c.ontology_id, c.channel
                );
            }
        }
    }

    if args.format.eq_ignore_ascii_case("json") {
        println!("{}", serde_json::to_string_pretty(&all_results)?);
    }
    Ok(())
}

/// Load the tagger: `--tagger-db` (raw/gz) or a transient in-memory build from `--input`.
fn load_tagger(args: &Args) -> Result<tag::ac::NerAc> {
    if let Some(db) = &args.tagger_db {
        let raw = std::fs::read(db).with_context(|| format!("reading {}", db.display()))?;
        let buf = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
            use std::io::Read as _;
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut out)?;
            out
        } else {
            raw
        };
        return tag::ac::NerAc::from_buf(buf)
            .with_context(|| format!("loading tagger DB {}", db.display()));
    }

    // Transient build from an ontology.
    let input = args
        .input
        .as_deref()
        .context("map: provide --tagger-db or --input <ontology>")?;
    let ontology_id = args
        .ontology_id
        .as_deref()
        .context("map: --input requires --ontology-id")?;
    let base = extract_strings::base_iris(&args.base_iri, args.preferred_prefix.as_deref());
    if base.is_empty() {
        bail!("map: --input needs --base-iri and/or --preferred-prefix");
    }
    let common = crate::cmd::CommonArgs::default();
    let mut model = crate::cmd::take_or_load(None, Some(input), &common)?;
    common.apply(&mut model)?;
    let tsv = extract_strings::extract_tsv(
        &model,
        ontology_id,
        &base,
        &extract_strings::default_label_props(),
    )?;
    let ac = tag::build_from_tsv(std::io::Cursor::new(tsv), tag::DEFAULT_MIN_LEN)?;
    Ok(ac)
}

/// Token inverted index: token → indices into `rows` (over `text_to_embed`).
fn build_fuzzy_index(rows: &[embeddings::Row]) -> HashMap<String, Vec<usize>> {
    let mut idx: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        for tok in tokenize(&r.text_to_embed) {
            idx.entry(tok).or_default().push(i);
        }
    }
    idx
}

fn fuzzy_matches(
    query: &str,
    idx: &HashMap<String, Vec<usize>>,
    rows: &[embeddings::Row],
) -> Vec<Candidate> {
    let qtokens: HashSet<String> = tokenize(query).into_iter().collect();
    if qtokens.is_empty() {
        return Vec::new();
    }
    // Gather candidate rows sharing ≥1 token.
    let mut cand_rows: HashSet<usize> = HashSet::new();
    for t in &qtokens {
        if let Some(v) = idx.get(t) {
            cand_rows.extend(v.iter().copied());
        }
    }
    // Best Jaccard per pk.
    let mut best: HashMap<&str, (f32, &embeddings::Row)> = HashMap::new();
    for i in cand_rows {
        let r = &rows[i];
        let rtokens: HashSet<String> = tokenize(&r.text_to_embed).into_iter().collect();
        let inter = qtokens.intersection(&rtokens).count();
        if inter == 0 {
            continue;
        }
        let union = qtokens.len() + rtokens.len() - inter;
        let jaccard = inter as f32 / union as f32;
        let e = best.entry(&r.pk).or_insert((0.0, r));
        if jaccard > e.0 {
            *e = (jaccard, r);
        }
    }
    best.into_values()
        .filter(|(j, _)| *j > 0.0)
        .map(|(j, r)| Candidate {
            iri: r.iri.clone(),
            label: r.label.clone(),
            ontology_id: r.ontology_id.clone(),
            confidence: 0.70 * j,
            channel: "fuzzy",
            matched: r.text_to_embed.clone(),
        })
        .collect()
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Suppression cascade + rank. When `top_n` is set, the single-best suppression
/// steps (best-embedding, weak-tail, best-per-target-ontology) are skipped so
/// several ranked candidates per ontology survive; output is capped at `top_n`
/// (else `limit`).
fn fuse(
    mut cands: Vec<Candidate>,
    targets: &HashSet<&str>,
    target_order: &[String],
    limit: usize,
    top_n: Option<usize>,
) -> Vec<Candidate> {
    if cands.is_empty() {
        return cands;
    }
    // Hard ontology filter when targets are set.
    if !targets.is_empty() {
        cands.retain(|c| targets.contains(c.ontology_id.as_str()));
        if cands.is_empty() {
            return cands;
        }
    }

    let shortlist = top_n.is_some();

    if !shortlist {
        // Best embedding: among semantic results, keep only the top (ties within 0.005).
        let best_sem = cands
            .iter()
            .filter(|c| c.channel == "semantic")
            .map(|c| c.confidence)
            .fold(f32::MIN, f32::max);
        if best_sem > f32::MIN {
            cands.retain(|c| c.channel != "semantic" || c.confidence >= best_sem - 0.005);
        }

        // Weak tail: when the best is strong, drop the long tail below it.
        let best = cands.iter().map(|c| c.confidence).fold(f32::MIN, f32::max);
        let gap = if targets.is_empty() { 0.2 } else { 0.05 };
        if best >= 0.7 {
            cands.retain(|c| c.confidence >= best - gap);
        }
    }

    // Dedup by IRI: keep the highest-confidence candidate per IRI.
    let mut by_iri: HashMap<String, Candidate> = HashMap::new();
    for c in cands {
        match by_iri.get(&c.iri) {
            Some(existing) if existing.confidence >= c.confidence => {}
            _ => {
                by_iri.insert(c.iri.clone(), c);
            }
        }
    }
    let mut cands: Vec<Candidate> = by_iri.into_values().collect();

    // Best per target ontology (single-best mode only; a shortlist keeps several).
    if !shortlist && !targets.is_empty() {
        let mut best_per: HashMap<String, Candidate> = HashMap::new();
        for c in cands {
            match best_per.get(&c.ontology_id) {
                Some(existing) if existing.confidence >= c.confidence => {}
                _ => {
                    best_per.insert(c.ontology_id.clone(), c);
                }
            }
        }
        cands = best_per.into_values().collect();
    }

    // Rank: confidence desc, then target-ontology priority.
    let prio = |ont: &str| -> usize {
        target_order
            .iter()
            .position(|t| t == ont)
            .unwrap_or(usize::MAX)
    };
    cands.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| prio(&a.ontology_id).cmp(&prio(&b.ontology_id)))
            .then_with(|| a.iri.cmp(&b.iri))
    });
    cands.truncate(top_n.unwrap_or(limit));
    cands
}

/// Drop tagger spans wholly contained within a longer span (`--no-substrings`), so
/// only the most specific match over a stretch of text survives.
fn remove_substrings(spans: &mut Vec<tag::Entity>) {
    let ranges: Vec<(usize, usize)> = spans.iter().map(|e| (e.start, e.end)).collect();
    let mut keep = vec![true; spans.len()];
    for i in 0..spans.len() {
        for j in 0..spans.len() {
            if i == j {
                continue;
            }
            // j strictly contains i (and is longer)
            let (si, ei) = ranges[i];
            let (sj, ej) = ranges[j];
            if sj <= si && ei <= ej && (ej - sj) > (ei - si) {
                keep[i] = false;
                break;
            }
        }
    }
    let mut it = keep.iter();
    spans.retain(|_| *it.next().unwrap());
}
