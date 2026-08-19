//! `unmerge` — remove the axioms of a second ontology from the first, leaving
//! only what the base ontology asserts on its own.

use std::collections::HashSet;
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{AnnotatedComponent, MutableOntology, RcStr};
use horned_owl::ontology::set::SetOntology;

use crate::io;
use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    /// Input ontology files (repeatable). The FIRST is the base; every
    /// subsequent input has its axioms subtracted from it. When a model is
    /// piped in, the piped model is the base and all `-i` inputs are
    /// subtracted.
    #[arg(short, long)]
    pub input: Vec<PathBuf>,
    /// The ontology whose axioms are subtracted from the input (owlmake alias;
    /// equivalent to giving a second `-i`).
    #[arg(long)]
    pub second_input: Option<PathBuf>,
    /// Subtract every ontology matching this glob pattern.
    #[arg(short = 'p', long = "inputs", value_name = "PATTERN")]
    pub inputs: Option<String>,
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
    // Determine the base and the ontologies to subtract. The first `-i` is the
    // base and the rest are subtrahends; if a model is piped in, that is the base
    // and all `-i` inputs are subtracted.
    let mut subtrahends: Vec<PathBuf> = Vec::new();
    let mut base = if let Some(piped) = piped {
        subtrahends.extend(args.input.iter().cloned());
        piped
    } else {
        let mut iter = args.input.iter();
        let first = iter.next().cloned();
        subtrahends.extend(iter.cloned());
        crate::cmd::take_or_load(None, first.as_deref(), &args.common)?
    };
    args.common.apply(&mut base)?;

    if let Some(p) = &args.second_input {
        subtrahends.push(p.clone());
    }
    if let Some(pattern) = &args.inputs {
        subtrahends.extend(crate::cmd::merge::expand_glob(pattern)?);
    }
    if subtrahends.is_empty() {
        anyhow::bail!("unmerge: no ontology to subtract (give a second -i, --second-input, or --inputs)");
    }

    let mut to_remove: HashSet<AnnotatedComponent<RcStr>> = HashSet::new();
    for path in &subtrahends {
        let second = io::load(path)?;
        to_remove.extend(second.ont.iter().cloned());
    }

    let mut ont = SetOntology::new();
    let mut removed = 0usize;
    for ac in base.ont.iter() {
        // Keep ontology metadata regardless; subtract shared logical axioms.
        let is_meta = matches!(
            ac.component,
            horned_owl::model::Component::OntologyID(_)
                | horned_owl::model::Component::DocIRI(_)
        );
        if !is_meta && to_remove.contains(ac) {
            removed += 1;
            continue;
        }
        ont.insert(ac.clone());
    }
    status!("unmerge: removed {removed} shared axiom(s)");

    let mut result = Model::from_parts(ont, base.prefixes);
    crate::cmd::maybe_save(&mut result, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(result))
}
