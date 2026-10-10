//! `export-prefixes` — write the prefixes a command line reads CURIEs with
//! ([`crate::context`]) as a JSON-LD `@context`: the built-in map, or the
//! `--prefixes` file in its place, with what `--add-prefixes`, `--prefix` and
//! `--add-prefix` bind.

use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
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
    let mut context = piped.as_ref().map(|m| m.context.clone()).unwrap_or_default();
    args.common.bind(&mut context)?;
    let text = context_document(&context.entries());
    match &args.output {
        Some(p) => std::fs::write(p, text)?,
        None => println!("{text}"),
    }
    Ok(piped)
}

/// The bindings as a JSON-LD context document: each binding on a line of its
/// own, `"name" : "namespace"`, and `{ }` for none.
fn context_document(entries: &[(String, String)]) -> String {
    if entries.is_empty() {
        return "{ }".to_string();
    }
    let lines: Vec<String> =
        entries.iter().map(|(name, ns)| format!("    {} : {}", json_string(name), json_string(ns))).collect();
    format!("{{\n  \"@context\" : {{\n{}\n  }}\n}}", lines.join(",\n"))
}

/// `s` as a JSON string: quoted, with `"`, `\` and control characters escaped.
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
            c if c < ' ' => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
