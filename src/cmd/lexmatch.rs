//! `lexmatch` — lexical matching between ontology entities → SSSOM.
//!
//! Builds a lexical index keyed by normalized strings of labels/synonyms, then emits an
//! SSSOM mapping for every pair of entities sharing a normalized key. Normalization is a
//! pipeline (`CaseNormalization` → `WhitespaceNormalization`, plus opt-in
//! `--add-pipeline-step`); confidence is the base-2 `inverse_logit` of a per-predicate
//! weight sum drawn from a rules file, and a pair no rule fires on maps as
//! `skos:closeMatch` @ 0.5.
//!
//! Output goes through owlmake's SSSOM writer, so the TSV carries the metadata block,
//! slot order and condensation an SSSOM consumer expects.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{AnnotationSubject, AnnotationValue, Component, Literal, MutableOntology};
use serde::Deserialize;

use crate::io::obo::compress_iri;
use crate::sssom::{self, MappingSet};

/// Annotation properties that supply a matchable alias → the match-field CURIE recorded
/// for a mapping found through that property.
const ALIAS_PROPS: &[(&str, &str)] = &[
    ("http://www.w3.org/2000/01/rdf-schema#label", "rdfs:label"),
    ("http://www.w3.org/2004/02/skos/core#prefLabel", "skos:prefLabel"),
    ("http://www.geneontology.org/formats/oboInOwl#hasExactSynonym", "oio:hasExactSynonym"),
    ("http://www.geneontology.org/formats/oboInOwl#hasBroadSynonym", "oio:hasBroadSynonym"),
    ("http://www.geneontology.org/formats/oboInOwl#hasNarrowSynonym", "oio:hasNarrowSynonym"),
    ("http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym", "oio:hasRelatedSynonym"),
];
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const SKOS_CLOSE_MATCH: &str = "skos:closeMatch";
const LEXICAL_MATCHING: &str = "semapv:LexicalMatching";
const OIO_NS: &str = "http://www.geneontology.org/formats/oboInOwl#";
/// Prefixes always declared in the `curie_map`, whether or not a mapping uses them.
const BUILTIN_PREFIXES: &[(&str, &str)] = &[
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("semapv", "https://w3id.org/semapv/vocab/"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("sssom", "https://w3id.org/sssom/"),
];
const DEFAULT_LICENSE: &str = "https://w3id.org/sssom/license/unspecified";

#[derive(ClapArgs)]
pub struct Args {
    /// First ontology.
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Second ontology to also match against. Merged into one index.
    #[arg(short = 'a', long = "add")]
    pub add: Vec<PathBuf>,
    /// Output SSSOM TSV (default stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Rules file (mapping_rules_datamodel YAML). Without it, every match is
    /// `skos:closeMatch` @ 0.5 (the default).
    #[arg(short = 'R', long = "rules-file")]
    pub rules_file: Option<PathBuf>,
    /// Append a normalization step, e.g. `WordOrderNormalization` (repeatable).
    #[arg(long = "add-pipeline-step")]
    pub add_pipeline_step: Vec<String>,
    /// Do not report matches between entities of the same id space (prefix).
    #[arg(long = "exclude-self-matches")]
    pub exclude_self_matches: bool,
    /// `mapping_tool` cell — the tool credited with producing the mapping set.
    /// Defaults to `owlmake`; set it to match a set produced elsewhere.
    #[arg(long = "mapping-tool", default_value = "owlmake")]
    pub mapping_tool: String,
    /// `mapping_set_id` metadata. A fresh id is generated each run, so set this
    /// when a mapping set needs a stable identifier across runs.
    #[arg(long = "mapping-set-id")]
    pub mapping_set_id: Option<String>,
    /// `license` metadata.
    #[arg(long, default_value = DEFAULT_LICENSE)]
    pub license: String,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> Result<Option<crate::model::Model>> {
    // Load the primary model (piped or --input) then merge any --add ontologies.
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    for extra in &args.add {
        let m = crate::cmd::take_or_load(None, Some(extra), &args.common)?;
        for ac in m.ont.iter() {
            model.ont.insert(ac.clone());
        }
    }

    let ruleset = match &args.rules_file {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("reading rules file {}", p.display()))?;
            serde_yaml::from_str(&text).context("parsing rules file")?
        }
        None => RuleCollection::default(),
    };

    let pipeline = Pipeline::new(&args.add_pipeline_step)?;

    // --- Build the lexical index: normalized key → relationships, plus labels. ---
    let mut groupings: BTreeMap<String, Vec<Rel>> = BTreeMap::new();
    let mut labels: HashMap<String, String> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            let AnnotationSubject::IRI(subj) = &aa.subject else { continue };
            let prop = aa.ann.ap.0.as_ref();
            let AnnotationValue::Literal(lit) = &aa.ann.av else { continue };
            let value = match lit {
                Literal::Simple { literal }
                | Literal::Language { literal, .. }
                | Literal::Datatype { literal, .. } => literal.as_str(),
            };
            if prop == RDFS_LABEL {
                labels.entry(subj.as_ref().to_string()).or_insert_with(|| value.to_string());
            }
            let Some((_, field)) = ALIAS_PROPS.iter().find(|(iri, _)| *iri == prop) else {
                continue;
            };
            let key = pipeline.normalize(value);
            if key.is_empty() {
                continue;
            }
            groupings.entry(key).or_default().push(Rel {
                predicate: field.to_string(),
                element: subj.as_ref().to_string(),
            });
        }
    }

    // --- Generate mappings for keys shared by ≥2 distinct entities. ---
    let mut mappings: Vec<sssom::Mapping> = Vec::new();
    let mut prefixes_used: BTreeMap<String, String> = BTreeMap::new();
    for (key, rels) in &groupings {
        let mut by_element: BTreeMap<&str, Vec<&Rel>> = BTreeMap::new();
        for r in rels {
            by_element.entry(r.element.as_str()).or_default().push(r);
        }
        if by_element.len() < 2 {
            continue;
        }
        let elements: Vec<&str> = by_element.keys().copied().collect();
        for &e1 in &elements {
            for &e2 in &elements {
                if e1 >= e2 {
                    continue; // one direction; drop diagonal + reciprocal
                }
                if args.exclude_self_matches && id_space(e1) == id_space(e2) {
                    continue;
                }
                for r1 in &by_element[e1] {
                    for r2 in &by_element[e2] {
                        let (predicate, confidence) =
                            infer(&r1.predicate, &r2.predicate, &ruleset);
                        let m = build_mapping(
                            e1,
                            e2,
                            &predicate,
                            confidence,
                            key,
                            &r1.predicate,
                            &r2.predicate,
                            &labels,
                            &args.mapping_tool,
                            &mut prefixes_used,
                        );
                        if let Some(min) = ruleset.minimum_confidence {
                            if confidence < min {
                                continue;
                            }
                        }
                        mappings.push(m);
                    }
                }
            }
        }
    }

    // --- Assemble the mapping set + write SSSOM. ---
    let mut ms = MappingSet::default();
    // curie_map = standard builtins + prefixes actually used (subject/object + oio if
    // any synonym field matched), so every CURIE in the table resolves from the header.
    for (p, n) in BUILTIN_PREFIXES {
        prefixes_used.entry(p.to_string()).or_insert_with(|| n.to_string());
    }
    if mappings.iter().any(|m| {
        m.get("subject_match_field").map(|f| f.starts_with("oio:")).unwrap_or(false)
            || m.get("object_match_field").map(|f| f.starts_with("oio:")).unwrap_or(false)
    }) {
        prefixes_used.entry("oio".into()).or_insert_with(|| OIO_NS.to_string());
    }
    ms.curie_map = prefixes_used;

    // Set-level metadata (license always; mapping_set_id when pinned). condense()
    // will lift the constant mapping_tool / *_match_field slots up here too.
    ms.metadata
        .insert("license".into(), serde_yaml::Value::String(args.license.clone()));
    if let Some(id) = &args.mapping_set_id {
        ms.metadata
            .insert("mapping_set_id".into(), serde_yaml::Value::String(id.clone()));
    }

    // Columns = the SLOT_ORDER-ordered subset actually populated.
    let mut present: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for m in &mappings {
        for k in m.keys() {
            present.insert(k.clone());
        }
    }
    ms.columns = sssom::SLOT_ORDER
        .iter()
        .filter(|s| present.contains(**s))
        .map(|s| s.to_string())
        .collect();
    ms.mappings = mappings;

    // condense: propagate constant slots to metadata; sort: order rows (subject_id)
    // and columns, so the same inputs always yield the same bytes.
    let out = sssom::io::write_table(&ms, '\t', true, true)?;
    match &args.output {
        Some(p) => std::fs::write(p, out)?,
        None => print!("{out}"),
    }
    status!("lexmatch: {} mappings", ms.mappings.len());
    Ok(Some(model))
}

struct Rel {
    predicate: String,
    element: String,
}

/// The id-space (prefix) of a CURIE-or-IRI, for `--exclude-self-matches`.
fn id_space(iri: &str) -> String {
    let c = compress_iri(iri);
    c.split(':').next().unwrap_or("").to_string()
}

#[allow(clippy::too_many_arguments)]
fn build_mapping(
    e1: &str,
    e2: &str,
    predicate: &str,
    confidence: f64,
    key: &str,
    f1: &str,
    f2: &str,
    labels: &HashMap<String, String>,
    mapping_tool: &str,
    prefixes: &mut BTreeMap<String, String>,
) -> sssom::Mapping {
    let s_curie = curie(e1, prefixes);
    let o_curie = curie(e2, prefixes);
    let mut m: sssom::Mapping = BTreeMap::new();
    m.insert("subject_id".into(), s_curie);
    if let Some(l) = labels.get(e1) {
        m.insert("subject_label".into(), l.clone());
    }
    m.insert("predicate_id".into(), predicate.to_string());
    m.insert("object_id".into(), o_curie);
    if let Some(l) = labels.get(e2) {
        m.insert("object_label".into(), l.clone());
    }
    m.insert("mapping_justification".into(), LEXICAL_MATCHING.to_string());
    m.insert("mapping_tool".into(), mapping_tool.to_string());
    m.insert("confidence".into(), fmt_conf(confidence));
    m.insert("subject_match_field".into(), f1.to_string());
    m.insert("object_match_field".into(), f2.to_string());
    m.insert("match_string".into(), key.to_string());
    m
}

/// Format a confidence for the TSV: a plain decimal float with trailing zeros trimmed.
fn fmt_conf(c: f64) -> String {
    let s = format!("{c:.12}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// CURIE-condense an IRI and record its prefix→namespace in `prefixes`.
fn curie(iri: &str, prefixes: &mut BTreeMap<String, String>) -> String {
    let c = compress_iri(iri);
    if let Some((prefix, local)) = c.split_once(':') {
        // Only treat as a CURIE if it actually shortened the IRI.
        if c != iri && !local.starts_with("//") {
            let ns = &iri[..iri.len() - local.len()];
            prefixes.entry(prefix.to_string()).or_insert_with(|| ns.to_string());
            return c;
        }
    }
    iri.to_string()
}

// ===========================================================================
// Normalization pipeline
// ===========================================================================

struct Pipeline {
    steps: Vec<Step>,
}

enum Step {
    Case,
    Whitespace,
    WordOrder,
}

impl Pipeline {
    fn new(add_steps: &[String]) -> Result<Self> {
        let mut steps = vec![Step::Case, Step::Whitespace];
        for s in add_steps {
            match s.as_str() {
                "WordOrderNormalization" => steps.push(Step::WordOrder),
                "CaseNormalization" => steps.push(Step::Case),
                "WhitespaceNormalization" => steps.push(Step::Whitespace),
                other => anyhow::bail!("unsupported pipeline step '{other}'"),
            }
        }
        Ok(Pipeline { steps })
    }

    fn normalize(&self, term: &str) -> String {
        let mut t = term.to_string();
        for step in &self.steps {
            t = match step {
                Step::Case => t.to_lowercase(),
                // Strip, then collapse runs of ≥2 *literal spaces* (not tabs/newlines).
                Step::Whitespace => collapse_spaces(t.trim()),
                Step::WordOrder => {
                    let mut toks: Vec<String> = t
                        .split_whitespace()
                        .map(|x| x.trim_end_matches([',', ';']).to_string())
                        .filter(|x| !matches!(x.as_str(), "of" | "the" | "a" | "an"))
                        .collect();
                    toks.sort();
                    toks.join(" ")
                }
            };
        }
        t
    }
}

/// Collapse runs of ≥2 spaces to one.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    let mut run = 0usize;
    for ch in s.chars() {
        if ch == ' ' {
            run += 1;
            prev_space = true;
        } else {
            if prev_space {
                out.push(' ');
                prev_space = false;
            }
            let _ = run;
            run = 0;
            out.push(ch);
        }
    }
    if prev_space {
        out.push(' ');
    }
    out
}

// ===========================================================================
// Rules engine + confidence
// ===========================================================================

#[derive(Deserialize, Default)]
struct RuleCollection {
    #[serde(default)]
    rules: Vec<Rule>,
    #[serde(default)]
    minimum_confidence: Option<f64>,
}

#[derive(Deserialize)]
struct Rule {
    #[serde(default)]
    preconditions: Precondition,
    #[serde(default)]
    postconditions: Postcondition,
}

#[derive(Deserialize, Default)]
struct Precondition {
    #[serde(default)]
    subject_match_field_one_of: Vec<String>,
    #[serde(default)]
    object_match_field_one_of: Vec<String>,
}

#[derive(Deserialize, Default)]
struct Postcondition {
    predicate_id: Option<String>,
    #[serde(default)]
    weight: f64,
}

/// Accumulate per-predicate weights over the rules that fire, pick the highest-weight
/// predicate, and turn that weight into a confidence with the base-2 `inverse_logit`.
/// With no rule firing the pair maps as `skos:closeMatch` @ 0.5.
fn infer(f1: &str, f2: &str, ruleset: &RuleCollection) -> (String, f64) {
    let mut weightmap: HashMap<String, f64> = HashMap::new();
    let mut best_pred = SKOS_CLOSE_MATCH.to_string();
    let mut best_weight: Option<f64> = None;
    for rule in &ruleset.rules {
        if !precondition_holds(&rule.preconditions, f1, f2) {
            continue;
        }
        let pred = rule
            .postconditions
            .predicate_id
            .clone()
            .unwrap_or_else(|| best_pred.clone());
        let w = weightmap.entry(pred.clone()).or_insert(0.0);
        *w += rule.postconditions.weight;
        let acc = *w;
        if best_weight.is_none() || acc > best_weight.unwrap() {
            best_weight = Some(acc);
            best_pred = pred;
        }
    }
    let bw = best_weight.unwrap_or(0.0);
    (best_pred, inverse_logit(bw))
}

fn precondition_holds(pre: &Precondition, f1: &str, f2: &str) -> bool {
    if !pre.subject_match_field_one_of.is_empty()
        && !pre.subject_match_field_one_of.iter().any(|x| x == f1)
    {
        return false;
    }
    if !pre.object_match_field_one_of.is_empty()
        && !pre.object_match_field_one_of.iter().any(|x| x == f2)
    {
        return false;
    }
    true
}

/// Base-2 logistic turning a rule weight into a confidence: `1 / (1 + 2^(-weight))`.
fn inverse_logit(weight: f64) -> f64 {
    1.0 / (1.0 + 2f64.powf(-weight))
}
