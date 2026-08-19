//! `om validate-id-ranges <FILE>` — check an OBO ID-policy file
//! (`<ont>-idranges.owl`), and the `dicer-cli policy` spelling a repository's QC
//! recipes reach it by.
//!
//! OBA and CL run the check from their `test` target as
//!
//! ```text
//! dicer-cli policy --assume-manchester --show-owlapi-error <ont>-idranges.owl
//! ```
//!
//! so that argv has to be accepted verbatim. [`dicer_main`] takes it and runs the
//! checker in [`crate::idpolicy`]; the two flags are accepted and ignored, since
//! both select an OWL-loading and error-rendering path owlmake has no counterpart
//! for — it parses the small Manchester subset these files use directly (see the
//! module docs on `idpolicy`).

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;

use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    /// The ID-policy file(s) to check (`<ont>-idranges.owl`).
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Same, spelled as an option (`-i`), so the command reads like every other
    /// owlmake command in a recipe.
    #[arg(short = 'i', long = "input", value_name = "FILE")]
    pub input: Vec<PathBuf>,

    /// Accepted for `dicer-cli policy` compatibility and ignored: it selects a
    /// Manchester loading path, which is the only syntax owlmake reads anyway.
    #[arg(long = "assume-manchester", hide = true)]
    pub assume_manchester: bool,

    /// Accepted for `dicer-cli policy` compatibility and ignored: owlmake reports
    /// its own diagnostics rather than a loader stack trace.
    #[arg(long = "show-owlapi-error", hide = true)]
    pub show_owlapi_error: bool,
}

impl Args {
    fn targets(&self) -> Vec<PathBuf> {
        self.input.iter().chain(self.files.iter()).cloned().collect()
    }
}

/// A side-output command: it reports and never rewrites the ontology, so a
/// chained model passes straight through.
pub fn step(model: Option<Model>, a: &Args) -> Result<Option<Model>> {
    run(a)?;
    Ok(model)
}

pub fn run(a: &Args) -> Result<()> {
    let files = a.targets();
    if files.is_empty() {
        bail!("validate-id-ranges: no ID-ranges file given");
    }
    let mut failed = 0usize;
    for path in &files {
        // A QC recipe may guard the call with `if [ -f … ]`, but the command can
        // also be run directly; a missing file is an error, not a silent pass.
        let violations = crate::idpolicy::check_file(path)?;
        if violations.is_empty() {
            let n = crate::idpolicy::parse(&std::fs::read_to_string(path)?).ranges.len();
            status!("validate-id-ranges: {} — {n} range(s), policy OK", path.display());
            continue;
        }
        failed += 1;
        for v in &violations {
            eprintln!("{}", v.render(path));
        }
        eprintln!(
            "validate-id-ranges: {} — {} policy violation(s)",
            path.display(),
            violations.len()
        );
    }
    if failed > 0 {
        bail!("{failed} ID-policy file(s) failed validation");
    }
    Ok(())
}

/// Entry point for the `dicer-cli` command name: `dicer-cli policy [flags] <FILE>`.
///
/// Only `policy` is implemented — it is the sole subcommand a repository's QC
/// asks for under this name. Anything else fails loudly rather than pretending to
/// have run: a check that cannot run must fail, never pass silently.
pub fn dicer_main(args: &[String]) -> i32 {
    match dicer_run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("dicer-cli: {e:#}");
            1
        }
    }
}

fn dicer_run(args: &[String]) -> Result<()> {
    let Some((sub, rest)) = args.split_first() else {
        bail!("usage: dicer-cli policy [--assume-manchester] [--show-owlapi-error] <FILE>");
    };
    match sub.as_str() {
        "--version" | "-V" => {
            println!(
                "owlmake {} (native dicer-cli-compatible ID-policy checker)",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        "--help" | "-h" | "help" => {
            println!(
                "dicer-cli (owlmake {}) — OBO ID-policy checking\n\n\
                 Usage: dicer-cli policy [options] <idranges.owl>...\n\n\
                 Options:\n\
                 \x20 --assume-manchester    accepted and ignored (owlmake reads Manchester)\n\
                 \x20 --show-owlapi-error    accepted and ignored (owlmake has no OWL API)\n",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        "policy" => {}
        other => bail!(
            "unsupported subcommand `{other}` — owlmake implements `dicer-cli policy` \
             (the one the ODK Makefiles use); see `om validate-id-ranges --help`"
        ),
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for tok in rest {
        match tok.as_str() {
            // Loader and error-rendering switches: they name nothing owlmake's
            // checker has, so they change nothing here.
            "--assume-manchester" | "--show-owlapi-error" => {}
            t if t.starts_with('-') => bail!("unrecognised dicer-cli policy option `{t}`"),
            t => files.push(PathBuf::from(t)),
        }
    }
    run(&Args {
        files,
        input: Vec::new(),
        assume_manchester: false,
        show_owlapi_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(files: Vec<&str>) -> Args {
        Args {
            files: files.into_iter().map(PathBuf::from).collect(),
            input: Vec::new(),
            assume_manchester: false,
            show_owlapi_error: false,
        }
    }

    const GOOD: &str = "\
Prefix: idrange: <http://x/idrange/>
Ontology: <http://x/x-idranges.owl>
Annotations:
    idprefix: \"http://x/X_\",
    iddigits: 7,
    idsfor: \"X\"
Datatype: idrange:1
    Annotations:
        allocatedto: \"Alice\"
    EquivalentTo:
        xsd:integer[>= 1, < 2000]
";

    fn write(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("owlmake-idpolicy-{}-{name}", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn valid_file_passes() {
        let p = write("good.owl", GOOD);
        assert!(run(&args(vec![p.to_str().unwrap()])).is_ok());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn broken_file_fails() {
        // Two ranges allocating the same numbers to different people.
        let body = format!(
            "{GOOD}Datatype: idrange:2\n    Annotations:\n        allocatedto: \"Bob\"\n    \
             EquivalentTo:\n        xsd:integer[>= 500, <= 2500]\n"
        );
        let p = write("overlap.owl", &body);
        assert!(run(&args(vec![p.to_str().unwrap()])).is_err());
        let _ = std::fs::remove_file(p);
    }

    /// The exact invocation a QC recipe uses: the two flags must be swallowed,
    /// not mistaken for file names.
    #[test]
    fn dicer_shim_accepts_the_odk_invocation() {
        let p = write("shim.owl", GOOD);
        let argv: Vec<String> = ["policy", "--assume-manchester", "--show-owlapi-error"]
            .iter()
            .map(|s| s.to_string())
            .chain(std::iter::once(p.display().to_string()))
            .collect();
        assert_eq!(dicer_main(&argv), 0);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn dicer_shim_rejects_unknown_subcommands() {
        assert_eq!(dicer_main(&["mint".to_string()]), 1);
    }
}
