//! `runoak` — the ontology-access CLI surface a repo's QC targets call by that
//! name. MONDO's `test` runs `runoak -i pronto:mondo.obo ontology-metadata` to
//! assert the released `.obo` parses and to print its version metadata, so
//! owlmake answers to the name directly rather than leaving the check to die as
//! a missing command.
//!
//! Two commands answer to the name: `ontology-metadata` (a `pronto:` OBO input,
//! parsed in full and its header echoed as YAML) and `diff` (two `simpleobo:`
//! inputs compared as KGCL changes — see `runoak_diff`). The metadata is a YAML fragment derived from the
//! document header:
//!
//! ```text
//! owl:versionInfo:
//! - obo:{ontology}/{data-version}{ontology}.owl
//! ```
//!
//! with `{}` for a header that carries no `data-version`. The file is parsed in
//! full first, so a document the reader cannot make sense of fails the check
//! rather than printing metadata for a file nothing could load.

use std::io::BufRead;

pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("runoak: {e}");
            1
        }
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    let mut input: Option<String> = None;
    let mut command: Option<String> = None;
    let mut other_input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut output_type = "yaml".to_string();
    let mut statistics = false;
    let mut group_by: Option<String> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--input" => {
                i += 1;
                input = args.get(i).cloned();
            }
            "-X" | "--other-input" => {
                i += 1;
                other_input = args.get(i).cloned();
            }
            "-o" | "--output" => {
                i += 1;
                output = args.get(i).cloned();
            }
            "-O" | "--output-type" => {
                i += 1;
                output_type = args.get(i).cloned().unwrap_or_default();
            }
            "--statistics" => statistics = true,
            "--group-by-property" => {
                i += 1;
                group_by = args.get(i).cloned();
            }
            other if !other.starts_with('-') && command.is_none() => {
                command = Some(other.to_string());
            }
            other => anyhow::bail!("unsupported option `{other}`"),
        }
        i += 1;
    }
    let Some(input) = input else { anyhow::bail!("no --input given") };
    match command.as_deref() {
        Some("ontology-metadata") => {}
        Some("diff") => {
            let strip = |s: &str| {
                s.strip_prefix("simpleobo:")
                    .or_else(|| s.strip_prefix("pronto:"))
                    .map(str::to_string)
                    .ok_or_else(|| {
                        anyhow::anyhow!("unsupported input selector `{s}` (simpleobo:<file>.obo)")
                    })
            };
            let Some(other) = other_input else { anyhow::bail!("diff needs -X <other>") };
            return super::runoak_diff::run(&super::runoak_diff::DiffArgs {
                old_path: strip(&input)?,
                new_path: strip(&other)?,
                output,
                output_type,
                statistics,
                group_by,
            });
        }
        Some(other) => anyhow::bail!("unsupported command `{other}` (ontology-metadata, diff)"),
        None => anyhow::bail!("no command given"),
    }
    let Some(path) = input.strip_prefix("pronto:") else {
        anyhow::bail!("unsupported input selector `{input}` (only pronto:<file>.obo)");
    };

    // Parse the whole document — the check is that the file LOADS — and read
    // the header tags the metadata is built from.
    let f = std::fs::File::open(path).map_err(|e| anyhow::anyhow!("opening {path}: {e}"))?;
    let _ = crate::io::obo::load(std::io::BufReader::new(f))
        .map_err(|e| anyhow::anyhow!("parsing {path}: {e}"))?;
    let f = std::fs::File::open(path).map_err(|e| anyhow::anyhow!("opening {path}: {e}"))?;
    let mut ontology: Option<String> = None;
    let mut data_version: Option<String> = None;
    for line in std::io::BufReader::new(f).lines() {
        let line = line?;
        // The header ends at the first stanza.
        if line.starts_with('[') {
            break;
        }
        if let Some(v) = line.strip_prefix("ontology:") {
            ontology = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("data-version:") {
            data_version = Some(v.trim().to_string());
        }
    }
    match (ontology, data_version) {
        (Some(ont), Some(ver)) => {
            println!("owl:versionInfo:");
            println!("- obo:{ont}/{ver}{ont}.owl");
        }
        _ => println!("{{}}"),
    }
    Ok(())
}
