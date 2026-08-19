//! Text tagger — dictionary recognition of ontology terms in free text.
//!
//! [`ac`] holds the flat Aho-Corasick automaton and its on-disk DB format. This
//! module is the layer around it: the per-key value packing
//! (`label ␞ iri ␞ ontology_id ␞ string_type ␞ source ␞ categories ␞ is_obsolete`),
//! the [`Entity`] output record, the [`annotate_text`] tagging function, and the TSV
//! [`build_from_tsv`] path — reused by both `om text-tagger` and `om map`'s
//! text-annotation mode.

pub mod ac;

use std::io::BufRead;

use ac::{NerAc, NerAcBuilder, RECORD_SEP, UNIT_SEP};
use serde::Serialize;

/// A tagged span. The JSON serialization is the tagger's interchange shape: fields in
/// this declared order, and any field a term carries no value for left out of the
/// object entirely rather than emitted — the three `Option`s when they are `None`,
/// `is_obsolete` when it is false.
#[derive(Serialize, Clone, Debug)]
pub struct Entity {
    pub start: usize,
    pub end: usize,
    pub term_label: String,
    pub term_iri: String,
    pub ontology_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub string_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_categories: Option<Vec<String>>,
    #[serde(skip_serializing_if = "is_false")]
    pub is_obsolete: bool,
}

fn is_false(v: &bool) -> bool {
    !v
}

#[derive(Serialize)]
pub struct AnnotateResponse {
    pub entities: Vec<Entity>,
}

/// Pack one term's metadata into the `RECORD_SEP`-delimited value string the DB stores
/// against a match key.
pub fn pack_value(
    label: &str,
    iri: &str,
    ontology_id: &str,
    string_type: &str,
    source: &str,
    categories: &str,
    is_obsolete: &str,
) -> String {
    format!(
        "{}{}{}{}{}{}{}{}{}{}{}{}{}",
        label,
        RECORD_SEP,
        iri,
        RECORD_SEP,
        ontology_id,
        RECORD_SEP,
        string_type,
        RECORD_SEP,
        source,
        RECORD_SEP,
        categories,
        RECORD_SEP,
        is_obsolete
    )
}

/// Tag `text` and unpack matches into [`Entity`] records: one `Entity` per term record
/// (`UNIT_SEP`-split), fields from the `splitn(7, RECORD_SEP)` value,
/// `subject_categories` `|`-split.
pub fn annotate_text(ac: &NerAc, text: &str, delimiters: Option<&[u8]>) -> Vec<Entity> {
    ac.find_all_matches(text, delimiters)
        .into_iter()
        .flat_map(|m| {
            let start = m.start;
            let end = m.end;
            m.value
                .split(UNIT_SEP)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .into_iter()
                .map(move |record| {
                    let parts: Vec<&str> = record.splitn(7, RECORD_SEP).collect();
                    let term_label = parts.first().unwrap_or(&"").to_string();
                    let term_iri = parts.get(1).unwrap_or(&"").to_string();
                    let ontology_id = parts.get(2).unwrap_or(&"").to_string();
                    let string_type = parts
                        .get(3)
                        .and_then(|s| if s.is_empty() { None } else { Some(s.to_string()) });
                    let source = parts
                        .get(4)
                        .and_then(|s| if s.is_empty() { None } else { Some(s.to_string()) });
                    let subject_categories = parts.get(5).and_then(|s| {
                        if s.is_empty() {
                            None
                        } else {
                            Some(s.split('|').map(|x| x.to_string()).collect())
                        }
                    });
                    let is_obsolete = parts.get(6).map_or(false, |s| *s == "true");
                    Entity {
                        start,
                        end,
                        term_label,
                        term_iri,
                        ontology_id,
                        string_type,
                        source,
                        subject_categories,
                        is_obsolete,
                    }
                })
        })
        .collect()
}

/// Default minimum match-key length (bytes). This is a pin, not a tuning knob: it
/// decides which rows of a TSV become match keys at all, so it is the value that keeps
/// a DB owlmake builds interchangeable with a published one built from the same TSV.
pub const DEFAULT_MIN_LEN: usize = 3;

/// Build a tagger automaton from an `extract-strings`-shaped TSV. Column selection is
/// by header name; the match key is `text_to_embed` if present else `label`; rows
/// shorter than `min_len` (match-key bytes) or missing key/iri are skipped. Insertion
/// order (hence the serialized pattern indices) follows the input row order, so the
/// same TSV yields the same bytes.
pub fn build_from_tsv<R: BufRead>(reader: R, min_len: usize) -> anyhow::Result<NerAc> {
    let mut lines = reader.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty input – expected a TSV header line"))??;
    let headers: Vec<&str> = header_line.split('\t').collect();

    let col = |name: &str| -> anyhow::Result<usize> {
        headers
            .iter()
            .position(|h| *h == name)
            .ok_or_else(|| anyhow::anyhow!("required column '{name}' not found in header"))
    };

    let idx_ontology_id = col("ontology_id")?;
    let idx_label = col("label")?;
    let idx_iri = col("iri")?;
    let idx_match_key = headers
        .iter()
        .position(|h| *h == "text_to_embed")
        .unwrap_or(idx_label);

    let idx_string_type = headers.iter().position(|h| *h == "string_type");
    let idx_curated_source = headers.iter().position(|h| *h == "curated_from_source");
    let idx_curated_categories = headers
        .iter()
        .position(|h| *h == "curated_from_subject_categories");
    let idx_is_obsolete = headers.iter().position(|h| *h == "is_obsolete");

    let min_cols = [idx_ontology_id, idx_label, idx_iri, idx_match_key]
        .into_iter()
        .max()
        .unwrap()
        + 1;

    let mut builder = NerAcBuilder::new();
    for line_result in lines {
        let line = line_result?;
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < min_cols {
            continue;
        }
        let label = fields[idx_label];
        let iri = fields[idx_iri];
        let ontology_id = fields[idx_ontology_id];
        let match_key = fields[idx_match_key];

        if match_key.is_empty() || iri.is_empty() || match_key.len() < min_len {
            continue;
        }

        let get = |i: Option<usize>| i.and_then(|i| fields.get(i)).copied().unwrap_or("");
        let value = pack_value(
            label,
            iri,
            ontology_id,
            get(idx_string_type),
            get(idx_curated_source),
            get(idx_curated_categories),
            get(idx_is_obsolete),
        );
        builder.add_entry(match_key, &value);
    }

    Ok(builder.build())
}
