//! `om semsql make <name>.db` — build an ontology's SQL database.
//!
//! The database is one release artefact projected into SQLite: its triples in
//! `statements`, its reasoned relation graph in `entailed_edge`, and the view
//! layer over both. See [`crate::semsql`] for what each table holds.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args as ClapArgs, Subcommand};

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

#[derive(Subcommand)]
pub enum Sub {
    /// Build `<name>.db` from the `<name>.owl` beside it.
    Make(MakeArgs),
}

#[derive(ClapArgs)]
pub struct MakeArgs {
    /// The database to write (`cl-basic.db`).
    #[arg(value_name = "DB")]
    pub db: PathBuf,
}

/// `om semsql …`, read as one line rather than through the chaining harness.
pub fn main(args: &[String]) -> i32 {
    match run_args(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("semsql: {e:#}");
            1
        }
    }
}

fn run_args(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("make") => {
            let Some(db) = args.get(1) else {
                bail!("make: expected a `<name>.db` target");
            };
            let db = PathBuf::from(db);
            if db.extension().and_then(|e| e.to_str()) != Some("db") {
                bail!("make: expected a `<name>.db` target, got {}", db.display());
            }
            crate::semsql::make(&db)
        }
        Some(other) => bail!("unknown command `{other}` (expected `make`)"),
        None => bail!("expected a command (`make <name>.db`)"),
    }
}
