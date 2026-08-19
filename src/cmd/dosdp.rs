//! `dosdp` — generate OWL from a DOSDP pattern and a TSV data table, filling the
//! pattern once per data row.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{AnnotationSubject, AnnotationValue, Component, Literal, MutableOntology};

use crate::dosdp;
use crate::io;

#[derive(ClapArgs)]
pub struct Args {
    /// DOSDP pattern YAML file.
    #[arg(long)]
    pub pattern: PathBuf,
    /// TSV data table (one row per generated class).
    #[arg(long)]
    pub data: PathBuf,
    /// Optional ontology to source filler labels from for name/def text.
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,

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
    let pattern = std::fs::read_to_string(&args.pattern)?;
    let data = std::fs::read_to_string(&args.data)?;

    // Build a label lookup from the optional input ontology.
    let mut labels: HashMap<String, String> = HashMap::new();
    if let Some(path) = &args.input {
        let model = io::load(path)?;
        for ac in model.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if aa.ann.ap.0.as_ref() == "http://www.w3.org/2000/01/rdf-schema#label" {
                    if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                        (&aa.subject, &aa.ann.av)
                    {
                        let text = match lit {
                            Literal::Simple { literal }
                            | Literal::Language { literal, .. }
                            | Literal::Datatype { literal, .. } => literal.clone(),
                        };
                        labels.insert(s.as_ref().to_string(), text);
                    }
                }
            }
        }
    }

    let generated = dosdp::generate(&pattern, &data, &labels)?;
    let n = generated.ont.iter().count();
    status!("dosdp: generated {n} component(s)");

    // When chained, fold the generated axioms into the piped ontology; otherwise
    // the generated ontology is the result.
    let mut result = match piped {
        Some(mut model) => {
            args.common.apply(&mut model)?;
            for ac in generated.ont.iter() {
                model.ont.insert(ac.clone());
            }
            model
        }
        None => {
            let mut generated = generated;
            args.common.apply(&mut generated)?;
            generated
        }
    };

    crate::cmd::maybe_save(&mut result, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(result))
}
