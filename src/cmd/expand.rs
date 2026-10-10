//! `expand` — expand macro relations.
//!
//! An entity annotated with `OMO:0002000` (defined by construct) carries a SPARQL
//! CONSTRUCT query. The query runs over the ontology's RDF rendering, and the
//! axioms its result states are added to the ontology. No other annotation is a
//! macro: an `IAO:0000424` (expand expression to) template expands nothing.

use std::path::PathBuf;

use anyhow::Context;
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, Component, Literal,
    MutableOntology, RcStr,
};

use crate::cmd::select;
use crate::model::Model;

const OMO_EXPAND_CONSTRUCT: &str = "http://purl.obolibrary.org/obo/OMO_0002000";
const DCT_SOURCE: &str = "http://purl.org/dc/terms/source";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,

    /// Macro property to expand. Repeatable. When any are given, only these
    /// properties' macros are expanded.
    #[arg(short = 't', long = "expand-term", value_name = "TERM")]
    pub expand_term: Vec<String>,
    /// File(s) listing macro properties to expand.
    #[arg(short = 'T', long = "expand-term-file", value_name = "FILE")]
    pub expand_term_file: Vec<PathBuf>,
    /// Macro property to NOT expand. Repeatable.
    #[arg(short = 'n', long = "no-expand-term", value_name = "TERM")]
    pub no_expand_term: Vec<String>,
    /// File(s) listing macro properties to NOT expand.
    #[arg(short = 'N', long = "no-expand-term-file", value_name = "FILE")]
    pub no_expand_term_file: Vec<PathBuf>,
    /// If true, output ontology will only contain the expansions.
    /// `<bool>`.
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true")]
    pub create_new_ontology: Option<bool>,
    /// If true, annotate each expansion axiom with `dct:source <expansion
    /// property>`. `<bool>`.
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true")]
    pub annotate_expansion_axioms: Option<bool>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // Which macro properties to expand: an optional allow-list (--expand-term) and
    // a deny-list (--no-expand-term), both CURIE-expanded.
    let include = select::collect_terms(&model, &args.expand_term, &args.expand_term_file)?;
    let exclude = select::collect_terms(&model, &args.no_expand_term, &args.no_expand_term_file)?;

    // Collect OMO_0002000 SPARQL-CONSTRUCT macros: subject IRI -> query string.
    let mut construct_macros: Vec<(String, String)> = Vec::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == OMO_EXPAND_CONSTRUCT {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    let subj = s.as_ref().to_string();
                    if !include.is_empty() && !include.contains(&subj) {
                        continue;
                    }
                    if exclude.contains(&subj) {
                        continue;
                    }
                    construct_macros.push((subj, literal_text(lit)));
                }
            }
        }
    }

    if construct_macros.is_empty() {
        status!("expand: no OMO:0002000 macros to expand (after term filtering)");
    }

    // Each generated axiom remembers the term whose macro made it (for --annotate-…).
    let mut to_add: Vec<(Component<RcStr>, String)> = Vec::new();

    // Run each OMO_0002000 SPARQL CONSTRUCT against the ontology and fold the
    // resulting triples back in as OWL axioms. A query that does not parse or
    // run fails the command.
    if !construct_macros.is_empty() {
        let q = crate::sparql::Queryable::from_model(&model)?;
        for (subj, query) in &construct_macros {
            let rdf = q
                .construct(query, oxigraph::io::RdfFormat::RdfXml)
                .with_context(|| format!("expand: the OMO:0002000 query of <{subj}>"))?;
            let constructed = parse_constructed(&rdf)
                .with_context(|| format!("expand: reading what the OMO:0002000 query of <{subj}> constructs"))?;
            for ac in constructed.ont.iter() {
                if is_skippable(&ac.component) {
                    continue;
                }
                to_add.push((ac.component.clone(), subj.clone()));
            }
        }
    }
    let create_new = args.create_new_ontology.unwrap_or(false);
    let annotate = args.annotate_expansion_axioms.unwrap_or(false);
    // Build a `dct:source <macro property>` annotation for an expansion axiom.
    // Use a dedicated Build so it does not conflict with mutable inserts into the
    // model below (separate Build instances combine without consequence).
    let ann_build = horned_owl::model::Build::new();
    let make_annotated = |c: Component<RcStr>, src: &str| -> AnnotatedComponent<RcStr> {
        let ann = Annotation { ann: Default::default(),
            ap: ann_build.annotation_property(DCT_SOURCE),
            av: AnnotationValue::IRI(ann_build.iri(src)),
        };
        let mut anns = std::collections::BTreeSet::new();
        anns.insert(ann);
        AnnotatedComponent { component: c, ann: anns }
    };

    if create_new {
        // `--create-new-ontology`: the root gives up its own axioms to take the
        // expansions. Its ontology ID, annotations and imports stay, and so do
        // the axioms its imports lent.
        let own: Vec<AnnotatedComponent<RcStr>> = model
            .ont
            .iter()
            .filter(|ac| {
                !model.imported_components.contains(*ac)
                    && !matches!(
                        ac.component,
                        Component::OntologyID(_)
                            | Component::DocIRI(_)
                            | Component::OntologyAnnotation(_)
                            | Component::Import(_)
                    )
            })
            .cloned()
            .collect();
        for ac in &own {
            model.ont.remove(ac);
        }
    }

    let mut added = 0;
    for (c, src) in to_add {
        let inserted = if annotate {
            model.ont.insert(make_annotated(c, &src))
        } else {
            model.ont.insert(c)
        };
        if inserted {
            added += 1;
        }
    }
    if create_new {
        status!(
            "expand: created new ontology with {added} expanded axiom(s) from {} macro(s)",
            construct_macros.len()
        );
    } else {
        status!(
            "expand: added {added} expanded axiom(s) from {} macro(s)",
            construct_macros.len()
        );
    }

    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Parse RDF/XML bytes (the output of a CONSTRUCT) into a Model by round-tripping
/// through a temp file, so the triples are mapped to OWL axioms by the RDF reader.
fn parse_constructed(rdf: &[u8]) -> anyhow::Result<Model> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = crate::time::SystemTime::now()
        .duration_since(crate::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!("owlmake-expand-{}-{nanos}-{n}.owl", std::process::id()));
    {
        let mut f = std::fs::File::create(&path)?;
        f.write_all(rdf)?;
    }
    let model = crate::io::load(&path);
    let _ = std::fs::remove_file(&path);
    model
}

/// Components from a constructed graph that should not be folded back into the
/// ontology (document/ontology metadata and imports).
fn is_skippable(c: &Component<RcStr>) -> bool {
    matches!(
        c,
        Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::Import(_)
            | Component::OntologyAnnotation(_)
    )
}

fn literal_text(lit: &Literal<RcStr>) -> String {
    match lit {
        Literal::Simple { literal }
        | Literal::Language { literal, .. }
        | Literal::Datatype { literal, .. } => literal.clone(),
    }
}
