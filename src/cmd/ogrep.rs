//! `ogrep` — print every axiom that mentions a matching entity.
//!
//! The shorthand for what ontology editors reach for constantly and currently
//! spell as `filter --term <full IRI> --trim false`: *show me this term, and
//! everything that refers to it*. It works on whatever owlmake can read, not
//! only OBO.
//!
//! The pattern is matched against entity IRIs and, unless `--iri-only`, against
//! their annotation values — so a label or a synonym finds the term as readily
//! as an ID does. `EFO:0007045` matches `…/EFO_0007045` too: an OBO-style CURIE
//! and the underscore form of the same ID are the same question, and requiring
//! the caller to know which one this ontology's prefix map happens to accept is
//! the papercut this command exists to remove.
//!
//! Output is OBO stanzas on stdout by default: for reading a term, that is the
//! shape the answer is wanted in. `-f ofn` (or any other format) gives the
//! lossless view when the axioms are more than OBO can say.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    /// Regex matched against entity IRIs and (unless `--iri-only`) their
    /// annotation values. Case-insensitive unless `--case-sensitive`.
    #[arg(value_name = "PATTERN")]
    pub pattern: String,
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Output file. Defaults to stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output format (default `obo`).
    #[arg(short, long)]
    pub format: Option<String>,
    /// Match only entity IRIs, not labels/synonyms/definitions.
    #[arg(long = "iri-only")]
    pub iri_only: bool,
    /// Match case-sensitively.
    #[arg(long = "case-sensitive")]
    pub case_sensitive: bool,
    /// Print only the matched entities' own axioms, not everything that refers
    /// to them.
    #[arg(long = "self-only")]
    pub self_only: bool,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Component, Literal};

    // One document, imports excluded: an edit file's imports are not what "where
    // is this term mentioned" is asking about.
    let mut model = crate::cmd::take_or_load_no_imports(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    let re = build_regex(&args.pattern, args.case_sensitive)?;
    let mut matched: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        for iri in crate::sig::signature(&ac.component) {
            if re.is_match(&iri) {
                matched.insert(iri);
            }
        }
        if args.iri_only {
            continue;
        }
        if let Component::AnnotationAssertion(aa) = &ac.component {
            let text = match &aa.ann.av {
                AnnotationValue::Literal(
                    Literal::Simple { literal }
                    | Literal::Language { literal, .. }
                    | Literal::Datatype { literal, .. },
                ) => literal.as_str(),
                _ => continue,
            };
            if re.is_match(text) {
                if let AnnotationSubject::IRI(subj) = &aa.subject {
                    matched.insert(subj.as_ref().to_string());
                }
            }
        }
    }

    if matched.is_empty() {
        anyhow::bail!("ogrep: nothing matches `{}`", args.pattern);
    }
    let mut terms: Vec<String> = matched.into_iter().collect();
    terms.sort();

    // `--trim false` is what makes this a grep rather than an extract: keep every
    // axiom in which a matched entity appears, including those whose subject is
    // some *other* term that refers to it.
    let fargs = crate::cmd::filter::Args {
        input: None,
        output: None,
        format: None,
        term: terms,
        term_file: vec![],
        select: if args.self_only { vec!["self".into(), "annotations".into()] } else { vec![] },
        axioms: vec![],
        preserve_structure: Some(false),
        trim: Some(args.self_only),
        exclude_term: vec![],
        exclude_terms: vec![],
        include_term: vec![],
        include_terms: vec![],
        drop_axiom_annotations: None,
        signature: None,
        allow_punning: None,
        base_iri: vec![],
        ontology_iri: None,
        common: Default::default(),
    };
    let mut out = crate::cmd::filter::step(Some(model), &fargs)?
        .expect("filter returns the model it was piped");

    match args.output.as_deref() {
        Some(path) => {
            let fmt = crate::cmd::resolve_format(args.format.as_deref().or(Some("obo")), path)?;
            crate::io::save_as(&mut out, path, fmt)?;
        }
        None => {
            let fmt = crate::io::Format::from_name(args.format.as_deref().unwrap_or("obo"))?;
            let mut buf = Vec::new();
            crate::io::write_to(&mut out, &mut buf, fmt)?;
            print!("{}", String::from_utf8_lossy(&buf));
        }
    }
    Ok(Some(out))
}

/// The pattern as a regex, with an OBO-style CURIE also matching the underscore
/// form of the same ID (`EFO:0007045` → `EFO[:_]0007045`), so a caller need not
/// know which spelling this document's prefix map uses.
fn build_regex(pattern: &str, case_sensitive: bool) -> Result<regex::Regex> {
    let expanded = match pattern.split_once(':') {
        Some((prefix, local))
            if !prefix.is_empty()
                && prefix.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && local.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            format!("{}[:_]{}", regex::escape(prefix), regex::escape(local))
        }
        _ => pattern.to_string(),
    };
    regex::RegexBuilder::new(&expanded)
        .case_insensitive(!case_sensitive)
        .build()
        .with_context(|| format!("ogrep: `{pattern}` is not a valid regex"))
}
