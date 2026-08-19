//! `verify` — run SPARQL queries as QC checks; each query must return zero
//! rows, otherwise the offending rows are reported and the command fails.
//!
//! This is a repository's constraint-query QC: the ontology keeps a set of
//! `<check>-violation.sparql` queries, each written so that any row it returns is
//! a violation, and the release build runs every one of them over the merged
//! product, writing one report per query under `reports/`. OBA, CL, UBERON and
//! EFO (eleven queries) all check themselves this way.

use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::Args as ClapArgs;

use crate::sparql::Queryable;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// SPARQL constraint query files. Each must return no rows to pass.
    #[arg(short = 'q', long = "queries", required = true, num_args = 1..)]
    pub queries: Vec<PathBuf>,
    /// Directory to write per-query violation reports.
    #[arg(short = 'O', long = "output-dir")]
    pub output_dir: Option<PathBuf>,
    /// Accepted for CLI compatibility and IGNORED.
    ///
    /// `verify` has no format choice: every violation report is CSV, whatever
    /// this says.
    #[arg(long = "format", hide = true)]
    pub format: Option<String>,
    /// Logging level at which a non-empty result causes failure.
    /// `none`/`false` reports violations without failing the command.
    #[arg(short = 'F', long = "fail-on-violation")]
    pub fail_on_violation: Option<String>,
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
    // NOT `take_or_load`: the queried dataset is the document's own axioms, with
    // the `owl:imports` closure left unresolved. A `-violation.sparql` check is
    // written against the terms the repository curates; merging the closure in
    // first would make every check range over imported terms too — EFO's
    // `no-dangling` and `id-length` checks would then fire across the whole
    // MONDO/UBERON closure.
    let mut model =
        crate::cmd::take_or_load_no_imports(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let q = Queryable::from_model(&model)?;

    // Whether to actually fail the command on violations. The default is to fail
    // on any violation; `--fail-on-violation none|false` only reports.
    let fail = match args.fail_on_violation.as_deref() {
        Some(s) if s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("false") => false,
        _ => true,
    };

    let mut total_violations = 0usize;
    for qpath in &args.queries {
        let sparql = std::fs::read_to_string(qpath)
            .with_context(|| format!("reading query {}", qpath.display()))?;
        let table = q
            .query_table(&sparql)
            .with_context(|| format!("running query {}", qpath.display()))?;
        // The rule name is the query path exactly as it appeared on the command
        // line, not its basename, so a failing check in a CI log names the file
        // to open.
        let rule = qpath.display();
        if table.rows.is_empty() {
            status!("PASS Rule {rule}: 0 violation(s)");
            continue;
        }
        total_violations += table.rows.len();
        status!("FAIL Rule {rule}: {} violation(s)", table.rows.len());
        // The WHOLE result set goes to stderr as CSV — no head. Truncating would
        // hide the tail of every failing check from CI logs.
        let rendered = table.render(true);
        for line in rendered.lines() {
            status!("    {line}");
        }
        if let Some(dir) = &args.output_dir {
            std::fs::create_dir_all(dir)?;
            // The report is named for the query's stem, so
            // `…/nolabels-violation.sparql` becomes `nolabels-violation.csv`.
            // Those are the names EFO commits under `src/ontology/reports/`
            // (`no-dangling-violation.csv`, `nolabels-violation.csv`), CRLF and
            // all, so a rerun rewrites the committed file in place.
            let stem = qpath
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "query".to_string());
            std::fs::write(dir.join(format!("{stem}.csv")), &rendered)?;
        }
    }

    if total_violations > 0 && fail {
        bail!("verify failed: {total_violations} total violation(s)");
    }
    if total_violations > 0 {
        status!("verify: {total_violations} violation(s) (not failing per --fail-on-violation)");
    }
    Ok(Some(model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a two-file ontology where the ROOT declares one class and the
    /// IMPORT declares another, plus a catalog wiring them together.
    fn fixture(dir: &std::path::Path) -> PathBuf {
        let imported = dir.join("imported.owl");
        std::fs::write(
            &imported,
            r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Ontology rdf:about="http://example.org/imported.owl"/>
  <owl:Class rdf:about="http://example.org/IMPORTED"/>
</rdf:RDF>
"#,
        )
        .unwrap();
        let root = dir.join("root.owl");
        std::fs::write(
            &root,
            r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Ontology rdf:about="http://example.org/root.owl">
    <owl:imports rdf:resource="http://example.org/imported.owl"/>
  </owl:Ontology>
  <owl:Class rdf:about="http://example.org/ROOT"/>
</rdf:RDF>
"#,
        )
        .unwrap();
        let mut cat = std::fs::File::create(dir.join("catalog-v001.xml")).unwrap();
        write!(
            cat,
            r#"<?xml version="1.0"?>
<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog">
  <uri name="http://example.org/imported.owl" uri="imported.owl"/>
</catalog>
"#
        )
        .unwrap();
        root
    }

    /// `verify` must query the ROOT ontology only. A check written against the
    /// edit file would otherwise flag every imported term.
    #[test]
    fn does_not_query_the_import_closure() {
        let dir = std::env::temp_dir().join(format!("owlmake-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = fixture(&dir);
        let query = dir.join("everything-violation.sparql");
        std::fs::write(
            &query,
            "PREFIX owl: <http://www.w3.org/2002/07/owl#>\n\
             SELECT ?cls WHERE { ?cls a owl:Class }",
        )
        .unwrap();

        let args = Args {
            input: Some(root),
            queries: vec![query],
            output_dir: Some(dir.join("reports")),
            format: None,
            fail_on_violation: Some("none".into()),
            common: Default::default(),
        };
        step(None, &args).unwrap();

        // The report is named for the query's stem, with a `.csv` extension.
        let out = dir.join("reports").join("everything-violation.csv");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("http://example.org/ROOT"), "{text}");
        assert!(
            !text.contains("http://example.org/IMPORTED"),
            "the import closure leaked into the verify dataset:\n{text}"
        );
        // CSV results: a bare header (no `?`) and CRLF line endings.
        assert!(text.starts_with("cls\r\n"), "{text:?}");
    }
}
