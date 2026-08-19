//! `om check-rdfxml <FILE>…` — verify that a file really is parseable RDF/XML.
//!
//! Every main product is gated on this before it ships: a release that does not parse
//! is not a release, and the failure has to surface while the build can still stop
//! rather than in a consumer's loader. Ontologies such as OBA, CL and UBERON run the
//! check over each product as part of assembling their assets.
//!
//! The check is exactly "does an RDF/XML parser accept this file", which owlmake
//! answers with the parser it already depends on and uses for SPARQL and `arq`
//! (`src/sparql.rs`, `src/arq.rs`): oxigraph's streaming `RdfParser`. The quads are
//! drained and discarded — nothing is built in memory, so a 300 MB release product
//! costs a constant amount of RAM. The first rejection is reported with the parser's
//! own message (line/column included) and the process exits 1.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::Args as ClapArgs;
use oxigraph::io::{RdfFormat, RdfParser};

use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    /// The RDF/XML file(s) to check.
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Same, spelled as an option, so the command reads like every other owlmake
    /// command when a recipe is replayed through the CLI.
    #[arg(short = 'i', long = "input", value_name = "FILE")]
    pub input: Vec<PathBuf>,
}

/// A side-output command: it validates a file on disk and never touches the
/// in-flight ontology, so a chained model passes straight through.
pub fn step(model: Option<Model>, a: &Args) -> Result<Option<Model>> {
    run(a)?;
    Ok(model)
}

pub fn run(a: &Args) -> Result<()> {
    let files: Vec<PathBuf> = a.input.iter().chain(a.files.iter()).cloned().collect();
    if files.is_empty() {
        bail!("check-rdfxml: no input file given");
    }
    for path in &files {
        check(path)?;
    }
    Ok(())
}

/// Stream-parse one file as RDF/XML, draining every quad. Errors carry the
/// parser's message verbatim — that is the diagnostic value of the check.
pub fn check(path: &Path) -> Result<()> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("check-rdfxml: opening {}", path.display()))?;
    let reader = std::io::BufReader::new(file);
    parse_rdfxml(reader, path)
        .with_context(|| format!("check-rdfxml: {} is not valid RDF/XML", path.display()))
}

fn parse_rdfxml(reader: impl BufRead, path: &Path) -> Result<()> {
    // A base IRI is what a parser handed a document URL would have: RDF/XML may
    // carry relative IRIs (`rdf:about="#foo"`), and without a base those are a
    // parse error rather than the resolvable references they are on disk. The file's
    // own location is that base, so the check accepts exactly what a loader reading
    // the file from disk would accept.
    // A path that cannot be made into an IRI (non-UTF-8, say) just means no base —
    // a file with only absolute IRIs still parses.
    let parser = file_url(path)
        .and_then(|base| RdfParser::from_format(RdfFormat::RdfXml).with_base_iri(base).ok())
        .unwrap_or_else(|| RdfParser::from_format(RdfFormat::RdfXml));
    let mut quads = 0u64;
    for quad in parser.for_reader(reader) {
        // Flattened to its message: `RdfParseError`'s own `Display` already
        // includes the underlying XML error, so keeping it as a `source` would
        // print the same sentence twice under anyhow's `{:#}`.
        quad.map_err(|e| anyhow!("{e}"))?;
        quads += 1;
    }
    status!("check-rdfxml: {} OK ({quads} triples)", path.display());
    Ok(())
}

/// `file://` URL for `path`, absolutised against the CWD when needed.
fn file_url(path: &Path) -> Option<String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    Some(format!("file://{}", abs.to_str()?))
}

/// Entry point for the `check-rdfxml` PATH shim (`check-rdfxml <FILE>`).
pub fn main(args: &[String]) -> i32 {
    let mut files: Vec<PathBuf> = Vec::new();
    for tok in args {
        match tok.as_str() {
            "--help" | "-h" => {
                println!(
                    "check-rdfxml (owlmake {}) — verify a file parses as RDF/XML\n\n\
                     Usage: check-rdfxml <FILE>...",
                    env!("CARGO_PKG_VERSION")
                );
                return 0;
            }
            "--version" | "-V" => {
                println!("owlmake {} (native check-rdfxml)", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            t => files.push(PathBuf::from(t)),
        }
    }
    match run(&Args { files, input: Vec::new() }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("owlmake-rdfxml-{}-{name}", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    const OK: &str = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#"
         xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Ontology rdf:about="http://example.org/o"/>
  <owl:Class rdf:about="http://example.org/A">
    <rdfs:label>A</rdfs:label>
  </owl:Class>
</rdf:RDF>
"#;

    #[test]
    fn well_formed_rdfxml_passes() {
        let p = write("ok.owl", OK);
        assert!(check(&p).is_ok());
        let _ = std::fs::remove_file(p);
    }

    /// The failure this gate exists to catch: a truncated release product.
    /// A truncated file is well-formed for the first N bytes, so only a real
    /// parse — not a stat or a grep — rejects it.
    #[test]
    fn truncated_rdfxml_fails() {
        let p = write("truncated.owl", &OK[..OK.len() / 2]);
        let err = check(&p).unwrap_err();
        assert!(format!("{err:#}").contains("not valid RDF/XML"), "{err:#}");
        let _ = std::fs::remove_file(p);
    }

    /// Well-formed XML that is not RDF: the element is not in any namespace, so
    /// the RDF/XML parser rejects it.
    #[test]
    fn non_rdf_xml_fails() {
        let p = write("plain.xml", "<?xml version=\"1.0\"?>\n<hello><world/></hello>\n");
        assert!(check(&p).is_err());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn missing_file_fails() {
        assert_eq!(main(&["/nonexistent/owlmake-check-rdfxml.owl".to_string()]), 1);
    }

    #[test]
    fn no_input_is_an_error() {
        assert!(run(&Args { files: Vec::new(), input: Vec::new() }).is_err());
    }
}
