//! `import` — generate an import module for the external terms an ontology uses.
//!
//! Computes the set of external terms referenced by an edit ontology, then
//! extracts a locality-based module for those terms from a source ontology.
//! This is `signature(edit) ∩ source → extract(source)`: the import carries
//! exactly the source terms the edit file mentions, and regenerating it as the
//! edit file changes keeps the release from importing whole source ontologies.

use std::collections::HashSet;
use std::path::PathBuf;

use clap::Args as ClapArgs;

use crate::extract::{self, Method};
use crate::io;
use crate::sig;

#[derive(ClapArgs)]
pub struct Args {
    /// The edit ontology whose referenced terms seed the module.
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// The source ontology to extract the module from.
    #[arg(short, long)]
    pub source: PathBuf,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Extraction method: BOT (default), TOP, or STAR.
    #[arg(long, default_value = "BOT")]
    pub method: String,
    /// Also include terms from this seed file (one IRI/CURIE per line).
    #[arg(long)]
    pub term_file: Option<PathBuf>,

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
    let mut edit = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut edit)?;
    let source = io::load(&args.source)?;

    // Seed = every entity the edit ontology references.
    let mut seed: HashSet<String> = HashSet::new();
    for ac in edit.ont.iter() {
        seed.extend(sig::signature(&ac.component));
    }
    if let Some(path) = &args.term_file {
        let text = std::fs::read_to_string(path)?;
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                seed.insert(crate::cmd::select::expand(&source, line));
            }
        }
    }

    let method = Method::parse(&args.method)
        .ok_or_else(|| anyhow::anyhow!("unknown extract method: {}", args.method))?;
    let mut module = extract::extract(&source, &seed, method);
    status!(
        "import: seeded {} term(s), module has {} components",
        seed.len(),
        module.ont.iter().count()
    );

    crate::cmd::maybe_save(&mut module, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(module))
}
