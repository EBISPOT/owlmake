//! `seed` — scaffold a starter `owlmake.yaml` for a new ontology, so a repo can
//! begin with a plan instead of writing one by hand. The generated plan is the
//! stock release (primary + base + obo/json over an edit file) and is
//! immediately buildable with `owlmake` once the edit ontology is in place.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;

use crate::model::Model;
use crate::odk;

#[derive(ClapArgs)]
pub struct Args {
    /// Ontology short id, e.g. `oba` (used for IRIs and artefact filenames).
    #[arg(long)]
    pub id: String,
    /// The edit ontology the release is built from (default `<id>-edit.obo`).
    #[arg(long)]
    pub edit: Option<String>,
    /// Where to write the scaffolded plan (default `owlmake.yaml`; a `.json`
    /// suffix writes JSON instead).
    #[arg(short, long, default_value = odk::PLAN_FILE)]
    pub output: PathBuf,
    /// Overwrite an existing file.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(_piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    if args.output.exists() && !args.force {
        bail!("{} already exists (use --force to overwrite)", args.output.display());
    }
    let dir = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let spec = odk::seed_spec(&args.id, args.edit.as_deref(), &dir)?;
    odk::save_spec(&spec, &args.output)?;

    let edit = args.edit.clone().unwrap_or_else(|| format!("{}-edit.obo", args.id));
    status!("seed: wrote {} for `{}`", args.output.display(), args.id);
    status!("seed: next, add your edit ontology `{edit}`, then run `owlmake`.");
    Ok(None)
}
