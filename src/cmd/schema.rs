//! `schema` — emit the JSON Schema for the build plan.
//!
//! The schema is derived from owlmake's own types and is identical for every
//! plan, so it is not committed per repo. Emit it on demand here when you want
//! editor/CI validation (e.g. `owlmake schema -o owlmake.schema.json`, shared
//! across repos, with a `"$schema"` pointer added to the plan).

use std::path::PathBuf;

use clap::Args as ClapArgs;

use crate::spec;

#[derive(ClapArgs)]
pub struct Args {
    /// Write the schema to this file instead of stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    let text = spec::schema_pretty();
    match &args.output {
        Some(path) => {
            std::fs::write(path, format!("{text}\n"))?;
            status!("wrote the plan JSON Schema to {}", path.display());
        }
        None => println!("{text}"),
    }
    // A leaf utility: pass any piped model straight through.
    Ok(piped)
}
