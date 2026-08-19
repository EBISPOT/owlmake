//! `export-prefixes` — dump the ontology's prefix map as a JSON-LD `@context`, so
//! the bindings a document carries can be reused as a prefix map elsewhere.

use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
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
    let mut context = serde_json::Map::new();
    for (prefix, ns) in model.prefixes.mappings() {
        context.insert(
            prefix.clone(),
            serde_json::Value::String(ns.clone()),
        );
    }
    let doc = serde_json::json!({ "@context": context });
    let text = serde_json::to_string_pretty(&doc)?;
    match &args.output {
        Some(p) => std::fs::write(p, text)?,
        None => println!("{text}"),
    }
    Ok(Some(model))
}
