//! `mirror` — download the ontologies an input's `owl:imports` name into a local
//! directory and write an XML catalog binding each import IRI to its local copy,
//! so a later run resolves those imports from disk instead of the network.
//!
//! Only the input document's own imports are fetched: the downloaded copies are
//! not themselves parsed, so an import of an import is left to resolve over the
//! network. A download that fails is reported and its IRI left out of the catalog,
//! which leaves that import unmirrored rather than failing the run.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::Component;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Directory to write mirrored imports into.
    #[arg(short, long, default_value = "mirror")]
    pub directory: PathBuf,
    /// Output catalog path (XML catalog v001).
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
    std::fs::create_dir_all(&args.directory)?;

    let imports: Vec<String> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::Import(iri) => Some(iri.0.as_ref().to_string()),
            _ => None,
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    if imports.is_empty() {
        status!("mirror: no owl:imports found");
    }

    let mut catalog_entries: Vec<(String, String)> = Vec::new();
    for iri in &imports {
        let file_name = sanitize(iri);
        let dest = args.directory.join(&file_name);
        status!("mirror: downloading {iri}");
        match download(iri) {
            Ok(bytes) => {
                std::fs::write(&dest, &bytes)?;
                catalog_entries.push((iri.clone(), file_name));
                status!("  -> {} ({} bytes)", dest.display(), bytes.len());
            }
            Err(e) => status!("  ! failed: {e}"),
        }
    }

    let catalog = build_catalog(&catalog_entries);
    let catalog_path = args
        .output
        .clone()
        .unwrap_or_else(|| args.directory.join("catalog-v001.xml"));
    let mut f = std::fs::File::create(&catalog_path)?;
    f.write_all(catalog.as_bytes())?;
    status!(
        "mirror: {} import(s) mirrored, catalog at {}",
        catalog_entries.len(),
        catalog_path.display()
    );
    Ok(Some(model))
}

fn download(url: &str) -> Result<Vec<u8>> {
    crate::io::http_get(url)
}

fn sanitize(iri: &str) -> String {
    let base = iri.rsplit('/').next().unwrap_or(iri);
    let base = if base.is_empty() { "import" } else { base };
    let mut name: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if !name.ends_with(".owl") && !name.ends_with(".obo") && !name.ends_with(".ttl") {
        name.push_str(".owl");
    }
    name
}

fn build_catalog(entries: &[(String, String)]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n",
    );
    for (iri, file) in entries {
        s.push_str(&format!(
            "    <uri name=\"{}\" uri=\"{}\"/>\n",
            xml_escape(iri),
            xml_escape(file)
        ));
    }
    s.push_str("</catalog>\n");
    s
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;")
}

