//! `babelon` — convert a [Babelon](https://github.com/monarch-initiative/babelon)
//! translation TSV into OWL annotation axioms, so a translated ontology (HPO's
//! `hp-fr.owl` and the `*.babelon.owl` it merges) is built from the translation
//! tables alone.
//!
//! For every translation row:
//!
//!  * a direct `AnnotationAssertion(<predicate_id> <subject_id> "<value>"@<lang>)`,
//!  * carrying the Babelon provenance as *axiom annotations* — one
//!    `babelon:<column>` annotation per remaining (non-empty) TSV column
//!    (`source_language`, `source_value`, `translation_language`,
//!    `translation_status`, …),
//!  * plus `Declaration(AnnotationProperty(..))` for each Babelon provenance
//!    property and for `IAO:0000115` / `oboInOwl:hasExactSynonym` when used as a
//!    predicate — a fixed pair, not every predicate that is not an OWL built-in:
//!    a row asserting `oboInOwl:hasRelatedSynonym` or `skos:altLabel` gets no
//!    declaration.
//!
//! `subject_id`/`predicate_id`/`translation_value` map to
//! `owl:annotatedSource`/`owl:annotatedProperty`/`owl:annotatedTarget`; every
//! other column becomes a `babelon:` annotation. CURIEs are expanded with the
//! standard OBO prefix map (unknown prefixes → `obo/<PREFIX>_<local>`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, AnnotatedComponent, Build,
    Component, Literal, MutableOntology,
};

use crate::model::Model;

const BABELON_NS: &str = "https://w3id.org/babelon/";
const IAO_DEFINITION: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const HAS_EXACT_SYNONYM: &str = "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym";

/// Columns that map to the reified `owl:annotated{Source,Property,Target}` of the
/// assertion rather than to a `babelon:` provenance annotation.
const SUBJECT_COL: &str = "subject_id";
const PREDICATE_COL: &str = "predicate_id";
const VALUE_COL: &str = "translation_value";
const LANG_COL: &str = "translation_language";

#[derive(ClapArgs)]
pub struct Args {
    /// Subcommand: `convert` (the default), `merge` or `prepare-translation`.
    /// Accepted positionally so an existing recipe line replays verbatim.
    #[arg(value_name = "SUBCOMMAND")]
    pub subcommand: Option<String>,
    /// Positional inputs (`merge` takes several; `prepare-translation` one).
    #[arg(value_name = "INPUTS")]
    pub inputs: Vec<PathBuf>,
    /// `prepare-translation`: adapter handle for the ontology (`pronto:hp.obo`).
    #[arg(long = "oak-adapter")]
    pub oak_adapter: Option<String>,
    /// `prepare-translation`: the language being translated into.
    #[arg(long = "language-code")]
    pub language_code: Option<String>,
    /// `prepare-translation`: predicate to translate. Repeatable.
    #[arg(long = "field")]
    pub field: Vec<String>,
    /// `prepare-translation`: file listing the terms to cover (default: all).
    #[arg(long = "term-list")]
    pub term_list: Option<PathBuf>,
    /// `prepare-translation`: where to write rows whose source value moved.
    #[arg(long = "output-source-changed")]
    pub output_source_changed: Option<PathBuf>,
    /// `prepare-translation`: where to write the untranslated rows.
    #[arg(long = "output-not-translated")]
    pub output_not_translated: Option<PathBuf>,
    /// `prepare-translation`: keep untranslated rows in the profile.
    #[arg(long = "include-not-translated", num_args = 1, default_missing_value = "true")]
    pub include_not_translated: Option<bool>,
    /// `prepare-translation`: flip a changed row's status to CANDIDATE.
    #[arg(long = "update-translation-status", num_args = 1, default_missing_value = "true")]
    pub update_translation_status: Option<bool>,
    /// Sort output tables (babelon default: true).
    #[arg(long = "sort-tables", num_args = 1, default_missing_value = "true")]
    pub sort_tables: Option<bool>,
    /// Drop columns outside the babelon schema (babelon default: false).
    #[arg(long = "drop-unknown-columns", num_args = 1, default_missing_value = "true")]
    pub drop_unknown_columns: Option<bool>,
    /// Merge duplicate translations, later files winning (babelon default: false).
    #[arg(long = "update-translations", num_args = 1, default_missing_value = "true")]
    pub update_translations: Option<bool>,
    /// The Babelon translation TSV to convert.
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Output file.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output format (accepted for babelon-CLI compatibility; only `owl` is emitted).
    #[arg(long = "output-format")]
    pub output_format: Option<String>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(_piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    use crate::cmd::babelon_tsv as tsv;
    // Defaults when a recipe leaves the table-shaping flags unset.
    let sort_tables = args.sort_tables.unwrap_or(true);
    let drop_unknown = args.drop_unknown_columns.unwrap_or(false);

    match args.subcommand.as_deref() {
        Some("merge") => {
            let table = tsv::merge(
                &args.inputs,
                sort_tables,
                drop_unknown,
                args.update_translations.unwrap_or(false),
            )?;
            match args.output.as_deref() {
                Some(o) => table.write(o)?,
                None => print!("{}", table.to_tsv()),
            }
            return Ok(None);
        }
        Some("prepare-translation") => {
            // `--oak-adapter <scheme>:<file>`: the handle names the scheme and the
            // ontology document. Two schemes name an OBO document — `pronto:` and
            // `simpleobo:` — and owlmake parses the file itself either way, so both
            // are accepted; MONDO reaches `mondo-international.owl` through
            // `simpleobo:mondo-simple.obo`. Any other scheme names a source owlmake
            // does not read, and is rejected rather than silently ignored.
            let adapter = args
                .oak_adapter
                .as_deref()
                .context("babelon prepare-translation: missing --oak-adapter")?;
            let (scheme, path) = adapter
                .split_once(':')
                .context("babelon prepare-translation: --oak-adapter is `<scheme>:<path>`")?;
            if scheme != "pronto" && scheme != "simpleobo" {
                anyhow::bail!(
                    "babelon prepare-translation: unsupported OAK adapter `{scheme}`                      (owlmake reads `pronto:<file.obo>` and `simpleobo:<file.obo>`)"
                );
            }
            let ontology = tsv::TermMeta::from_obo(Path::new(path))?;
            let terms = match &args.term_list {
                Some(p) => Some(
                    std::fs::read_to_string(p)
                        .with_context(|| format!("reading term list {}", p.display()))?
                        .lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>(),
                ),
                None => None,
            };
            let input = args.inputs.first().cloned().or_else(|| args.input.clone());
            let prepared = tsv::prepare_translation(
                input.as_deref(),
                &ontology,
                args.language_code.as_deref().unwrap_or(""),
                &args.field,
                terms.as_deref(),
                args.include_not_translated.unwrap_or(false),
                args.update_translation_status.unwrap_or(true),
            )?;
            // Every output table goes through the same sort/drop/write, and is
            // skipped when no path was given for it.
            for (table, path) in [
                (prepared.profile, args.output.as_deref()),
                (prepared.source_changed, args.output_source_changed.as_deref()),
                (prepared.not_translated, args.output_not_translated.as_deref()),
            ] {
                let Some(path) = path else { continue };
                let mut t = table;
                if sort_tables {
                    t.sort();
                }
                if drop_unknown {
                    t.drop_unknown_columns();
                }
                t.write(path)?;
            }
            return Ok(None);
        }
        _ => {}
    }

    // Default: `convert`.
    let input = args
        .input
        .as_deref()
        .or_else(|| args.inputs.first().map(|p| p.as_path()))
        .context("babelon: missing input TSV")?;
    // `--output-format json` selects the other conversion: the table as the Babelon
    // JSON profile, not an ontology. So `mp-all.babelon.json` carries the profile
    // and not the OBO Graphs document a `.json` output name would otherwise imply.
    if args.output_format.as_deref() == Some("json") {
        let table = crate::cmd::babelon_tsv::Table::read(input)?;
        let json = babelon_json(&table);
        match args.output.as_deref() {
            Some(o) => std::fs::write(o, json)
                .with_context(|| format!("writing {}", o.display()))?,
            None => print!("{json}"),
        }
        return Ok(None);
    }
    let mut model = convert_file(input)?;
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), Some("owl"))?;
    Ok(Some(model))
}

/// The Babelon JSON profile: `{"translations": [ … ]}`.
///
/// The profile's layout is fixed: two-space indent, ASCII-only (every non-ASCII
/// character a `\uXXXX` escape), keys in the SCHEMA's slot order rather than the
/// table's column order, and a slot with no value omitted entirely. No trailing
/// newline.
///
/// A value is written in its TABLE spelling, not its parsed one: the profile is
/// produced by writing the table back out and reading it as records, and the
/// reader does not undo the quoting the writer applies — so a value carrying a
/// quote, a tab or a line break appears quoted, with its own quotes doubled.
/// `HP:0000565`'s French definition reads
/// `"Une forme … (""croisés"") …"` in the profile and
/// `Une forme … ("croisés") …` in the table.
pub fn babelon_json(table: &crate::cmd::babelon_tsv::Table) -> String {
    let mut out = String::from("{\n  \"translations\": [\n");
    for (i, row) in table.rows.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("    {\n");
        let mut first = true;
        for field in crate::cmd::babelon_tsv::TRANSLATION_FIELDS {
            let Some(v) = row.get(field).map(|s| s.as_str()).filter(|s| !s.is_empty()) else {
                continue;
            };
            if !first {
                out.push_str(",\n");
            }
            first = false;
            out.push_str("      \"");
            out.push_str(field);
            out.push_str("\": ");
            // `translation_confidence` is the one slot whose schema range is a
            // number, so it renders unquoted.
            if field == CONFIDENCE_COL && v.parse::<f64>().is_ok() {
                out.push_str(v);
            } else {
                out.push_str(&json_string(&crate::cmd::babelon_tsv::quote_field(v)));
            }
        }
        out.push_str("\n    }");
    }
    out.push_str("\n  ]\n}");
    out
}

/// A JSON string literal in the profile's ASCII-only escaping: control characters
/// and every non-ASCII code point as `\uXXXX` (surrogate pairs above the BMP).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// Convert a babelon TSV file at `path` into a fresh [`Model`].
/// The one babelon slot whose schema range is not string-shaped (`range: double`).
const CONFIDENCE_COL: &str = "translation_confidence";
const XSD_DOUBLE: &str = "http://www.w3.org/2001/XMLSchema#double";

pub fn convert_file(path: &Path) -> Result<Model> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading babelon TSV {}", path.display()))?;
    Ok(convert_tsv(&text))
}

/// Convert babelon TSV `text` into a [`Model`] of annotation axioms.
pub fn convert_tsv(text: &str) -> Model {
    let mut model = Model::new();
    // `convert` builds a NEW ontology from the TSV rows, so it has a FRESH
    // document format: its xmlns block is the built-in namespaces plus the ones the
    // entities actually use, not owlmake's CURIE map. Carrying the CURIE map would
    // declare an unused `xmlns:dc` in `hp-fr.babelon.owl`, and a merge keeps the
    // FIRST input's document format — so the stray binding would reach `hp-fr.owl`
    // and land as `idspace: dc` in `hp-fr.obo`. Same treatment as `template`/
    // `extract`/`filter`/`dosdp`.
    model.format_prefixes_cleared = true;
    let b: Build<crate::model::Str> = Build::new();

    let mut lines = text.lines();
    let header: Vec<String> = match lines.next() {
        Some(h) => h.split('\t').map(|s| s.trim().to_string()).collect(),
        None => return model,
    };
    let col = |row: &[&str], name: &str| -> Option<String> {
        header
            .iter()
            .position(|h| h == name)
            .and_then(|i| row.get(i))
            // NOT trimmed: the cell IS the translation, and HPO's translation TSVs
            // carry values with leading/trailing whitespace, so trimming would alter
            // the annotation text in `hp-fr.babelon.owl` and in every artefact that
            // merges it. Only the HEADER names are trimmed (above).
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };

    // Annotation properties to declare: the Babelon provenance properties, plus the
    // fixed pair of predicates (`IAO:0000115`, `oboInOwl:hasExactSynonym`) that a
    // row declares when it asserts one.
    let mut decls: BTreeSet<String> = BTreeSet::new();
    let mut rows = 0usize;

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        let (Some(subject), Some(predicate), Some(value)) = (
            col(&cells, SUBJECT_COL),
            col(&cells, PREDICATE_COL),
            col(&cells, VALUE_COL),
        ) else {
            continue;
        };
        let lang = col(&cells, LANG_COL);

        for (p, ns) in [curie_binding(&subject), curie_binding(&predicate)].into_iter().flatten() {
            if !model.built_prefixes.iter().any(|(q, _)| *q == p) {
                model.built_prefixes.push((p, ns));
            }
        }
        let subject_iri = b.iri(expand_curie(&subject));
        let predicate_iri = expand_curie(&predicate);
        let ap = b.annotation_property(predicate_iri.as_str());

        let av = match &lang {
            Some(l) => AnnotationValue::Literal(Literal::Language {
                literal: value.clone(),
                lang: l.clone(),
            }),
            None => AnnotationValue::Literal(Literal::Simple { literal: value.clone() }),
        };

        // Axiom (provenance) annotations: every remaining non-empty column.
        let mut anns: BTreeSet<Annotation<crate::model::Str>> = BTreeSet::new();
        for (i, h) in header.iter().enumerate() {
            if matches!(h.as_str(), SUBJECT_COL | PREDICATE_COL | VALUE_COL) {
                continue;
            }
            // Verbatim, as above — this is the provenance columns
            // (`source_value`, `comment`, …).
            let Some(v) = cells.get(i).copied().filter(|s| !s.is_empty()) else {
                continue;
            };
            let prop = format!("{BABELON_NS}{h}");
            decls.insert(prop.clone());
            // A provenance literal is typed from its slot's declared RANGE.
            // `translation_confidence` is the only slot in `babelon.yaml` that is
            // not a string, an enum or an EntityReference — it is `range: double` —
            // so it alone carries `rdf:datatype="…XMLSchema#double"`; written bare,
            // every confidence annotation in `hp-international.owl` would be an
            // untyped literal instead.
            let av = if h == CONFIDENCE_COL {
                AnnotationValue::Literal(Literal::Datatype {
                    literal: v
                        .trim()
                        .parse::<f64>()
                        .map(crate::cmd::babelon_tsv::format_float)
                        .unwrap_or_else(|_| v.to_string()),
                    datatype_iri: b.iri(XSD_DOUBLE),
                })
            } else {
                AnnotationValue::Literal(Literal::Simple { literal: v.to_string() })
            };
            anns.insert(Annotation {
                ann: Default::default(),
                ap: b.annotation_property(prop.as_str()),
                av,
            });
        }

        // The two predicates that carry a declaration when a row asserts them. It
        // is this fixed pair, not a rule about built-ins: any other predicate that
        // is not an OWL built-in is asserted without a declaration.
        if predicate_iri == IAO_DEFINITION || predicate_iri == HAS_EXACT_SYNONYM {
            decls.insert(predicate_iri.clone());
        }

        let comp = Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(subject_iri),
            ann: Annotation { ann: Default::default(), ap, av },
        });
        model.ont.insert(AnnotatedComponent { component: comp, ann: anns });
        rows += 1;
    }

    for d in &decls {
        let ap = b.annotation_property(d.as_str());
        model
            .ont
            .insert(Component::DeclareAnnotationProperty(horned_owl::model::DeclareAnnotationProperty(ap)));
    }

    status!("babelon: converted {rows} translation row(s)");
    model
}

/// The prefix binding a CURIE brings that the built-in namespaces do not already
/// cover: `HP:0000001` binds `HP` to `http://purl.obolibrary.org/obo/HP_`.
///
/// `None` for a full IRI, for a bare string, and for the prefixes
/// [`expand_curie`] already knows — those namespaces are declared under their own
/// built-in names, so binding them again would only take the name away.
fn curie_binding(curie: &str) -> Option<(String, String)> {
    if curie.starts_with("http://") || curie.starts_with("https://") {
        return None;
    }
    let (p, _) = curie.split_once(':')?;
    if p.is_empty() || expand_curie(&format!("{p}:x")) != format!("http://purl.obolibrary.org/obo/{p}_x")
    {
        return None;
    }
    Some((p.to_string(), format!("http://purl.obolibrary.org/obo/{p}_")))
}

/// Expand a CURIE with the standard OBO prefix map. Unknown prefixes are treated
/// as OBO ontology IDs (`HP:0001` → `obo/HP_0001`). Full IRIs pass through.
pub fn expand_curie(curie: &str) -> String {
    if curie.starts_with("http://") || curie.starts_with("https://") {
        return curie.to_string();
    }
    let Some((p, l)) = curie.split_once(':') else {
        return curie.to_string();
    };
    let base = match p {
        "rdf" => "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "rdfs" => "http://www.w3.org/2000/01/rdf-schema#",
        "owl" => "http://www.w3.org/2002/07/owl#",
        "xsd" => "http://www.w3.org/2001/XMLSchema#",
        "dc" => "http://purl.org/dc/elements/1.1/",
        "dcterms" => "http://purl.org/dc/terms/",
        "skos" => "http://www.w3.org/2004/02/skos/core#",
        "oboInOwl" | "oio" => "http://www.geneontology.org/formats/oboInOwl#",
        "obo" => "http://purl.obolibrary.org/obo/",
        "babelon" => BABELON_NS,
        _ => return format!("http://purl.obolibrary.org/obo/{p}_{l}"),
    };
    format!("{base}{l}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::{ComponentKind, Kinded};

    const TSV: &str = "source_language\ttranslation_language\tsubject_id\tpredicate_id\tsource_value\ttranslation_value\ttranslation_status
en\tcs\tHP:0000001\trdfs:label\tAll\tVše\tOFFICIAL
en\tcs\tHP:0000002\tIAO:0000115\tDefinition.\tDefinice.\tOFFICIAL";

    #[test]
    fn curie_expansion() {
        assert_eq!(expand_curie("HP:0000001"), "http://purl.obolibrary.org/obo/HP_0000001");
        assert_eq!(expand_curie("IAO:0000115"), "http://purl.obolibrary.org/obo/IAO_0000115");
        assert_eq!(expand_curie("rdfs:label"), "http://www.w3.org/2000/01/rdf-schema#label");
        assert_eq!(expand_curie("http://x/y"), "http://x/y");
    }

    #[test]
    fn converts_rows_to_annotation_assertions() {
        let model = convert_tsv(TSV);

        // One AnnotationAssertion per row, each carrying four babelon provenance
        // axiom annotations (source_language/source_value/translation_language/
        // translation_status).
        let mut assertions = 0usize;
        for ac in model.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                assertions += 1;
                assert!(matches!(&aa.ann.av, AnnotationValue::Literal(Literal::Language { lang, .. }) if lang == "cs"));
                assert_eq!(ac.ann.len(), 4, "expected 4 provenance annotations");
                assert!(ac.ann.iter().all(|a| a.ap.0.as_ref().starts_with(BABELON_NS)));
            }
        }
        assert_eq!(assertions, 2);

        // IAO:0000115 and the four babelon properties are declared (rdfs:label is
        // not one of the two predicates declared on use, so it contributes none).
        let decls: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| ac.component.kind() == ComponentKind::DeclareAnnotationProperty)
            .collect();
        assert_eq!(decls.len(), 5);
    }
}
