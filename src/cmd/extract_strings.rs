//! `extract-strings` — extract per-entity label / exact-synonym strings for
//! embedding, in the exact byte form the embedding index keys on.
//!
//! A row's SHA-1 is the identity its embedding is stored and looked up under, so
//! both stages below are byte-exact: one differing byte is a miss, not a near
//! match.
//!
//!  * the annotation walk — the label set (default properties rdfs:label,
//!    dc/elements title, dc/terms title, skos:prefLabel; `--label-property`
//!    *replaces* that set) plus raw `oboInOwl:hasExactSynonym`, with the short-form
//!    fallback when a term has no English/untagged label.
//!  * row preparation — cl100k_base tokenize, truncate to the first 500 tokens,
//!    SHA-1 the (truncated) string, then map `\t \n \r`→space to form
//!    `text_to_embed`.
//!
//! An entity is embedded iff it is *defining* for this ontology: a pure IRI prefix
//! match against the ontology's base URIs — here the `--base-iri` values plus
//! `obo/<preferred-prefix>_`. Because embeddings are built one ontology at a time
//! this stays a purely local test (no cross-ontology aggregation needed).
//!
//! Output is the per-string table (one row per label/synonym), matching the
//! embedding parquet's columns minus `embedding`. Definitions are not embedded, and
//! only literal `hasExactSynonym` (not the unified synonym field or other scopes).
//!
//! `--names` writes a second table beside it: every name each defining entity
//! has — its labels and its synonyms of every scope — one row per name, with the
//! scope (`label`, `exact`, `related`, `broad`, `narrow`), the synonym type the
//! assertion is annotated with, and the language tag. These are the names as the
//! ontology states them, for a reader that wants all of them: nothing is
//! truncated or hashed, and the names an embedding index leaves out are here.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{AnnotationSubject, AnnotationValue, ClassExpression as CE, Component, Literal};
use sha1::Digest;
use tiktoken_rs::{cl100k_base, CoreBPE};
use crate::model::XSD_BOOLEAN;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const DC11_TITLE: &str = "http://purl.org/dc/elements/1.1/title";
const DCTERMS_TITLE: &str = "http://purl.org/dc/terms/title";
const SKOS_PREFLABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
const HAS_EXACT_SYNONYM: &str = "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym";
const HAS_RELATED_SYNONYM: &str = "http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym";
const HAS_BROAD_SYNONYM: &str = "http://www.geneontology.org/formats/oboInOwl#hasBroadSynonym";
const HAS_NARROW_SYNONYM: &str = "http://www.geneontology.org/formats/oboInOwl#hasNarrowSynonym";
const HAS_SYNONYM_TYPE: &str = "http://www.geneontology.org/formats/oboInOwl#hasSynonymType";
const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";
const OBO_PURL: &str = "http://purl.obolibrary.org/obo/";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Output file. Defaults to stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output format: `tsv` (default) or `parquet`.
    #[arg(short, long, default_value = "tsv")]
    pub format: String,
    /// Ontology id for the `ontology_id` column and `pk` (e.g. `ecto`).
    #[arg(long = "ontology-id", value_name = "ID")]
    pub ontology_id: String,
    /// Base IRI(s) whose entities are "defining" (embedded): an entity is included
    /// iff its IRI starts with one of these. Repeatable — these are the base IRIs
    /// that define internal/owned terms.
    #[arg(long = "base-iri", value_name = "IRI")]
    pub base_iri: Vec<String>,
    /// OBO preferred prefix (e.g. `ECTO`); adds `obo/<PREFIX>_` as a base IRI and is
    /// used for the shortForm fallback.
    #[arg(long = "preferred-prefix", value_name = "PFX")]
    pub preferred_prefix: Option<String>,
    /// Annotation property IRI(s) to treat as labels. If given, **replaces** the
    /// default set (rdfs:label, dc:title, dcterms:title, skos:prefLabel), matching
    /// OLS `LabelAnnotator`. Repeatable.
    #[arg(long = "label-property", value_name = "IRI")]
    pub label_property: Vec<String>,
    /// Also write the names table to this file: every label and every synonym of
    /// every scope of each defining entity, one row per name, with its scope
    /// (`label`, `exact`, `related`, `broad`, `narrow`), synonym type and language.
    #[arg(long = "names", value_name = "FILE")]
    pub names: Option<PathBuf>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// What kind of name a name is: a label, or a synonym of one of the four scopes.
/// The order is the order an entity's names are listed in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    Label,
    Exact,
    Related,
    Broad,
    Narrow,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::Label => "label",
            Scope::Exact => "exact",
            Scope::Related => "related",
            Scope::Broad => "broad",
            Scope::Narrow => "narrow",
        }
    }

    /// The scope a synonym property asserts, if it is one.
    fn of_synonym(property: &str) -> Option<Scope> {
        match property {
            HAS_EXACT_SYNONYM => Some(Scope::Exact),
            HAS_RELATED_SYNONYM => Some(Scope::Related),
            HAS_BROAD_SYNONYM => Some(Scope::Broad),
            HAS_NARROW_SYNONYM => Some(Scope::Narrow),
            _ => None,
        }
    }
}

/// One name of an entity, as the names table lists it. The derived order —
/// scope, then value, language and synonym type — is the row order within an
/// entity.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Name {
    scope: Scope,
    value: String,
    lang: String,
    /// The value of the assertion's `oboInOwl:hasSynonymType` annotation; empty
    /// when it has none.
    synonym_type: String,
}

/// Per-entity strings gathered from the annotation walk.
#[derive(Default)]
struct Entity {
    entity_type: &'static str,
    /// (value, language-tag) for each label-property literal.
    labels: Vec<(String, String)>,
    /// Raw `hasExactSynonym` literal values.
    synonyms: Vec<String>,
    /// Every label and synonym, gathered only for the names table.
    names: Vec<Name>,
    /// `owl:deprecated true` — emitted as the `is_obsolete` column, which the tagger
    /// dictionary carries so a match on an obsolete term can be told apart.
    deprecated: bool,
}

impl Entity {
    fn set_type(&mut self, t: &'static str) {
        // class wins over property/individual if an entity is (ab)used as several;
        // in practice a well-formed ontology declares each once.
        if self.entity_type.is_empty() {
            self.entity_type = t;
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // Base IRIs defining "owned" entities = --base-iri plus obo/<preferred-prefix>_.
    let mut base_iris: Vec<String> = args.base_iri.clone();
    if let Some(pp) = &args.preferred_prefix {
        base_iris.push(format!("{OBO_PURL}{pp}_"));
    }
    if base_iris.is_empty() {
        bail!("extract-strings: no defining namespace — provide --base-iri and/or --preferred-prefix");
    }

    // Label properties: --label-property replaces the default set rather than adding
    // to it, so a caller can embed one property alone.
    let label_props: HashSet<String> = if args.label_property.is_empty() {
        [RDFS_LABEL, DC11_TITLE, DCTERMS_TITLE, SKOS_PREFLABEL]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        args.label_property.iter().cloned().collect()
    };

    let ents = collect_entities(&model, &label_props, args.names.is_some());

    if !args.format.eq_ignore_ascii_case("tsv") {
        bail!("extract-strings: --format {} not yet supported (only tsv)", args.format);
    }
    // Stream rows to output (or stdout) to keep memory bounded.
    let mut w: Box<dyn Write> = match args.output.as_deref() {
        Some(p) => Box::new(BufWriter::new(File::create(p)?)),
        None => Box::new(BufWriter::new(std::io::stdout())),
    };
    let (total, defining, rows_written) =
        write_strings(&ents, &args.ontology_id, &base_iris, &mut w)?;
    w.flush()?;
    status!("extract-strings: {total} entities, {defining} defining, {rows_written} strings");
    if let Some(path) = args.names.as_deref() {
        let mut w = BufWriter::new(File::create(path)?);
        let names_written = write_names(&ents, &args.ontology_id, &base_iris, &mut w)?;
        w.flush()?;
        status!("extract-strings: {names_written} names");
    }
    Ok(Some(model))
}

/// Walk the ontology once, collecting per-entity labels / exact-synonyms /
/// deprecation, keyed by IRI (BTree → deterministic, IRI-sorted output). With
/// `names`, every label and every synonym of every scope is kept as well, for the
/// names table.
fn collect_entities(
    model: &crate::model::Model,
    label_props: &HashSet<String>,
    names: bool,
) -> BTreeMap<String, Entity> {
    let mut ents: BTreeMap<String, Entity> = BTreeMap::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::DeclareClass(dc) => {
                ents.entry(dc.0 .0.as_ref().to_string()).or_default().set_type("class");
            }
            Component::DeclareObjectProperty(p) => {
                ents.entry(p.0 .0.as_ref().to_string()).or_default().set_type("property");
            }
            Component::DeclareDataProperty(p) => {
                ents.entry(p.0 .0.as_ref().to_string()).or_default().set_type("property");
            }
            Component::DeclareAnnotationProperty(p) => {
                ents.entry(p.0 .0.as_ref().to_string()).or_default().set_type("property");
            }
            Component::DeclareNamedIndividual(i) => {
                ents.entry(i.0 .0.as_ref().to_string()).or_default().set_type("individual");
            }
            // A class used only as a SubClassOf subject may lack an explicit
            // Declaration; treat it as a class as `export` does.
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    ents.entry(sub.0.as_ref().to_string()).or_default().set_type("class");
                }
            }
            Component::AnnotationAssertion(aa) => {
                let AnnotationSubject::IRI(subj) = &aa.subject else { continue };
                let prop = aa.ann.ap.0.as_ref();
                // owl:deprecated → is_obsolete column. A TYPED boolean marks
                // deprecation; an untyped `"true"`, or one carrying a language
                // tag, is a string that happens to spell it and marks nothing.
                if prop == OWL_DEPRECATED {
                    if let AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri }) =
                        &aa.ann.av
                    {
                        if literal == "true" && datatype_iri.as_ref() == XSD_BOOLEAN {
                            ents.entry(subj.as_ref().to_string()).or_default().deprecated = true;
                        }
                    }
                    continue;
                }
                let scope = if label_props.contains(prop) {
                    Scope::Label
                } else {
                    match Scope::of_synonym(prop) {
                        Some(scope) => scope,
                        None => continue,
                    }
                };
                // The strings table holds labels and exact synonyms; the other
                // scopes are names only.
                let embedded = matches!(scope, Scope::Label | Scope::Exact);
                if !embedded && !names {
                    continue;
                }
                // Labels/synonyms are literals; skip IRI/anonymous annotation values.
                let AnnotationValue::Literal(lit) = &aa.ann.av else { continue };
                let (value, lang) = match lit {
                    Literal::Simple { literal } => (literal.clone(), String::new()),
                    Literal::Language { literal, lang } => (literal.clone(), lang.clone()),
                    Literal::Datatype { literal, .. } => (literal.clone(), String::new()),
                };
                let ent = ents.entry(subj.as_ref().to_string()).or_default();
                if names {
                    // A synonym's type is an annotation on the assertion itself.
                    let synonym_type = ac
                        .ann
                        .iter()
                        .filter(|a| a.ap.0.as_ref() == HAS_SYNONYM_TYPE)
                        .find_map(|a| match &a.av {
                            AnnotationValue::IRI(iri) => Some(iri.as_ref().to_string()),
                            AnnotationValue::Literal(
                                Literal::Simple { literal }
                                | Literal::Language { literal, .. }
                                | Literal::Datatype { literal, .. },
                            ) => Some(literal.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    ent.names.push(Name {
                        scope,
                        value: value.clone(),
                        lang: validate_language(&lang),
                        synonym_type,
                    });
                }
                match scope {
                    Scope::Label => ent.labels.push((value, lang)),
                    Scope::Exact => ent.synonyms.push(value),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    ents
}

/// Write the strings TSV (header + one row per label/exact-synonym of each defining
/// entity) to `w`. Returns (entities-seen, defining, rows-written).
fn write_strings<W: Write>(
    ents: &BTreeMap<String, Entity>,
    ontology_id: &str,
    base_iris: &[String],
    w: &mut W,
) -> Result<(u64, u64, u64)> {
    let tokenizer = cl100k_base().expect("cl100k_base tokenizer");
    writeln!(
        w,
        "pk\tontology_id\tentity_type\tiri\tlabel\thash\ttext_to_embed\tstring_type\tcurated_from_source\tcurated_from_subject_categories\tis_obsolete"
    )?;

    let mut total: u64 = 0;
    let mut defining: u64 = 0;
    let mut rows_written: u64 = 0;

    for (iri, ent) in ents {
        // Only entities with a known type (i.e. that appear as class/property/
        // individual); an IRI seen only as an annotation subject is not an entity
        // of this ontology and gets no row.
        if ent.entity_type.is_empty() {
            continue;
        }
        total += 1;
        if !base_iris.iter().any(|b| iri.starts_with(b)) {
            continue;
        }
        defining += 1;

        // All label literals are embedded (every language); the short form is added
        // only when there is no English/untagged label, so an entity labelled only
        // in other languages still has a string a plain query can hit.
        let has_english = ent.labels.iter().any(|(_, lang)| {
            let v = validate_language(lang);
            v.is_empty() || v == "en"
        });
        let mut label_values: Vec<String> = ent.labels.iter().map(|(v, _)| v.clone()).collect();
        if !has_english {
            label_values.push(short_form(iri, base_iris));
        }

        // `label` column: the lexicographically-first label value, so a multi-label
        // entity picks the same one on every run. Display metadata only — the
        // embedded strings and their hashes below are what the index keys on.
        let label_col = {
            let mut sorted = label_values.clone();
            sorted.sort();
            sorted
                .first()
                .cloned()
                .unwrap_or_default()
                .replace(['\t', '\n', '\r'], " ")
        };

        let pk = format!("{}:{}:{}", ontology_id, ent.entity_type, iri);
        let obsolete = if ent.deprecated { "true" } else { "" };

        // One LABEL row per label value and per exact-synonym.
        for text in label_values.iter().chain(ent.synonyms.iter()) {
            let Some((hash, document)) = make_row(text, &tokenizer) else { continue };
            writeln!(
                w,
                "{pk}\t{ontology_id}\t{}\t{iri}\t{label_col}\t{hash}\t{document}\tLABEL\t\t\t{obsolete}",
                ent.entity_type
            )?;
            rows_written += 1;
        }
    }
    Ok((total, defining, rows_written))
}

/// Write the names TSV (header + one row per name of each defining entity) to `w`,
/// and return how many rows that was. The entities are the ones the strings table
/// has rows for; an entity's names are in `Name` order, each distinct name once.
fn write_names<W: Write>(
    ents: &BTreeMap<String, Entity>,
    ontology_id: &str,
    base_iris: &[String],
    w: &mut W,
) -> Result<u64> {
    writeln!(w, "ontology_id\tentity_type\tiri\tname\tscope\tsynonym_type\tlang\tis_obsolete")?;
    let flat = |text: &str| text.replace(['\t', '\n', '\r'], " ");
    let mut rows_written: u64 = 0;
    for (iri, ent) in ents {
        if ent.entity_type.is_empty() || !base_iris.iter().any(|b| iri.starts_with(b)) {
            continue;
        }
        let obsolete = if ent.deprecated { "true" } else { "" };
        let mut names: Vec<&Name> = ent.names.iter().filter(|n| !n.value.trim().is_empty()).collect();
        names.sort();
        names.dedup();
        for name in names {
            writeln!(
                w,
                "{ontology_id}\t{}\t{iri}\t{}\t{}\t{}\t{}\t{obsolete}",
                ent.entity_type,
                flat(&name.value),
                name.scope.as_str(),
                flat(&name.synonym_type),
                name.lang,
            )?;
            rows_written += 1;
        }
    }
    Ok(rows_written)
}

/// The strings TSV as bytes, for in-process reuse (e.g. `om map`'s transient tagger
/// build from an ontology).
pub(crate) fn extract_tsv(
    model: &crate::model::Model,
    ontology_id: &str,
    base_iris: &[String],
    label_props: &HashSet<String>,
) -> Result<Vec<u8>> {
    let ents = collect_entities(model, label_props, false);
    let mut buf = Vec::new();
    write_strings(&ents, ontology_id, base_iris, &mut buf)?;
    Ok(buf)
}

/// The default label-property set: rdfs:label, dc/elements title, dc/terms title
/// and skos:prefLabel.
pub(crate) fn default_label_props() -> HashSet<String> {
    [RDFS_LABEL, DC11_TITLE, DCTERMS_TITLE, SKOS_PREFLABEL]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Compose the base-IRI set from `--base-iri` values plus `obo/<preferred-prefix>_`.
pub(crate) fn base_iris(base_iri: &[String], preferred_prefix: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = base_iri.to_vec();
    if let Some(pp) = preferred_prefix {
        v.push(format!("{OBO_PURL}{pp}_"));
    }
    v
}

/// Lowercase SHA-1 hex digest — the id an embedded string is stored under.
fn compute_sha1(doc: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(doc.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Prepare one embedding string: tokenize with cl100k_base, truncate to the first
/// 500 tokens, hash the truncated text, then flatten `\t \n \r` to spaces so the
/// value survives a TSV column. The hash is taken *before* the flattening, so the
/// order of those two steps is part of the row's identity. Returns
/// `(hash, text_to_embed)`, or `None` when the string tokenizes to nothing.
fn make_row(text: &str, tokenizer: &CoreBPE) -> Option<(String, String)> {
    let mut document = text.to_string();

    let mut tokens: Vec<String> = tokenizer
        .split_by_token_iter(&document, false)
        .map(|result| result.unwrap_or_else(|err| panic!("Tokenization error: {err}")))
        .collect();

    if tokens.is_empty() {
        return None;
    }

    if tokens.len() > 500 {
        tokens.truncate(500);
        document = tokens.join("");
    }

    let hash = compute_sha1(&document);
    let document = document.replace(['\t', '\n', '\r'], " ");
    Some((hash, document))
}

/// Language-tag validation: keep a tag only if it is `[A-Za-z0-9-]+` and ≤ 10
/// chars, else treat it as untagged (empty) — so a malformed tag counts as an
/// English/untagged label rather than triggering the short-form fallback.
fn validate_language(lang: &str) -> String {
    if !lang.is_empty()
        && lang.len() <= 10
        && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        lang.to_string()
    } else {
        String::new()
    }
}

/// The short form used by the no-English-label fallback: the OBO CURIE for
/// obo-PURL terms (`…/ECTO_0000001` → `ECTO:0000001`), else the longest matching
/// base-IRI stripped, else the local name after the last `/` or `#`.
///
/// Only reached for terms lacking any English/untagged label, so it supplies a
/// last-resort identifier string to embed. An ontology that publishes its own
/// short-form pattern is not honoured yet — for now the IRI alone decides the form.
fn short_form(iri: &str, base_iris: &[String]) -> String {
    if let Some(local) = iri.strip_prefix(OBO_PURL) {
        if !local.contains('/') {
            return local.replacen('_', ":", 1);
        }
    }
    let stripped = base_iris
        .iter()
        .filter(|b| iri.starts_with(*b))
        .max_by_key(|b| b.len())
        .map(|b| &iri[b.len()..]);
    if let Some(s) = stripped {
        return s.to_string();
    }
    iri.rsplit(['/', '#']).next().unwrap_or(iri).to_string()
}
