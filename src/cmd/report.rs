//! `report` — run the QC profile over an ontology and emit a violations report.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use clap::Args as ClapArgs;

use crate::report::{self, ReportRule, Severity};
use crate::sparql::Queryable;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Report output file. Defaults to stdout. Format follows --format.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Custom reporting profile file: lines of `LEVEL<TAB>rule`, where the rule is
    /// either one of the built-in rule names or `file:<path>` naming a custom
    /// SPARQL query to run as an extra rule. The file REPLACES the default profile
    /// — a rule it omits does not run.
    #[arg(short = 'p', long = "profile", value_name = "FILE")]
    pub profile: Option<PathBuf>,
    /// Output format: tsv (default), csv, html, json or yaml. Defaults to the
    /// --output extension when given.
    #[arg(short = 'f', long = "format")]
    pub format: Option<String>,
    /// Fail if any violation at or above this level is present: ERROR
    /// (default), WARN, INFO, or none.
    #[arg(short = 'F', long = "fail-on", default_value = "ERROR")]
    pub fail_on: String,
    /// Report labels instead of CURIEs for entities.
    #[arg(short = 'l', long = "labels", num_args = 1, default_missing_value = "true")]
    pub labels: Option<bool>,
    /// Print this many violations to the terminal (long-only: the `-P` short is
    /// taken by the global `--prefixes`).
    #[arg(long = "print", default_value_t = 0)]
    pub print: usize,
    /// Base namespace for filtering. REPEATABLE — OBA passes two (`…/OBA_` and
    /// `…/oba`), and a violation is kept if its subject is under ANY of them.
    #[arg(long = "base-iri")]
    pub base_iri: Vec<String>,
    /// Limit the number of violations reported PER RULE.
    #[arg(short = 'L', long = "limit")]
    pub limit: Option<usize>,
    /// Load RDF onto disk via TDB. Accepted for compatibility; owlmake
    /// always evaluates the report in memory.
    #[arg(short = 't', long = "tdb", num_args = 1, default_missing_value = "true")]
    pub tdb: Option<bool>,
    /// TDB directory. No-op (no TDB).
    #[arg(short = 'd', long = "tdb-directory")]
    pub tdb_directory: Option<PathBuf>,
    /// Keep the TDB directory. No-op (no TDB).
    #[arg(short = 'k', long = "keep-tdb-mappings", num_args = 1, default_missing_value = "true")]
    pub keep_tdb_mappings: Option<bool>,
    /// Wrap HTML output in a complete document (default true). Only affects
    /// `--format html`.
    #[arg(long = "standalone", num_args = 1, default_missing_value = "true")]
    pub standalone: Option<bool>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// `--fail-on`: a level, or `none` to disable the gate. Anything else is rejected
/// with `FAIL ON ERROR '%s' is not a valid fail-on level.` — leaving an
/// unrecognised value to silently disable the gate turns a failing QC target
/// green.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FailOn {
    None,
    At(Severity),
}

impl FailOn {
    fn parse(s: &str) -> anyhow::Result<FailOn> {
        if s.trim().eq_ignore_ascii_case("none") {
            return Ok(FailOn::None);
        }
        match Severity::parse(s.trim()) {
            Some(sev) => Ok(FailOn::At(sev)),
            None => bail!("report: FAIL ON ERROR '{s}' is not a valid fail-on level."),
        }
    }
}

/// The output format: `--format` if given, else the OUTPUT PATH'S EXTENSION, else
/// tsv. An unrecognised name is written as TSV.
fn resolve_format(explicit: Option<&str>, output: Option<&Path>) -> String {
    if let Some(f) = explicit {
        return f.trim().to_ascii_lowercase();
    }
    if let Some(ext) = output.and_then(Path::extension) {
        return ext.to_string_lossy().to_ascii_lowercase();
    }
    "tsv".to_string()
}

/// Renders the entity cells of a report.
///
/// Without `--labels` an entity is a CURIE over the BUILT-IN OBO context, not the
/// input document's prefix map — so the report says `dc:title` where the
/// document's own map would give `terms:title`. The document's prefixes are only
/// a fallback for IRIs the context cannot shorten at all, so a document can never
/// re-spell an IRI the context already covers. With `--labels` a label REPLACES
/// the CURIE (in every column, not just the subject), and the CURIE is the
/// fallback for an entity that has no label.
struct ShortForm {
    /// The built-in OBO context: (prefix, namespace), longest namespace first, so
    /// the first match is the most specific one.
    context: Vec<(String, String)>,
    /// The document's own prefixes, consulted ONLY for an IRI the built-in context
    /// cannot shorten at all. `--prefix`/`--prefixes` are folded into the document
    /// map, so this second pass is what keeps them working. It must stay a
    /// fallback and never join the first list: CL's
    /// `http://purl.obolibrary.org/obo/cl#…` has a LONGER match in the document
    /// (`cl:`) than in the context (`obo:`), and released CL reports carry
    /// `obo:cl#cellxgene_subset`.
    document: Vec<(String, String)>,
    labels: Option<HashMap<String, String>>,
    ontology_iri: Option<String>,
}

impl ShortForm {
    fn new(model: &crate::model::Model, labels: Option<HashMap<String, String>>) -> Self {
        let mut context = report::obo_context_prefixes();
        context.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        let mut document: Vec<(String, String)> = model
            .prefixes
            .mappings()
            .filter(|(p, _)| !p.is_empty())
            .map(|(p, ns)| (p.to_string(), ns.to_string()))
            .collect();
        document.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        ShortForm { context, document, labels, ontology_iri: ontology_iri(model) }
    }

    /// The CURIE for an IRI, or the IRI itself when no namespace matches.
    fn curie(&self, iri: &str) -> String {
        for map in [&self.context, &self.document] {
            for (prefix, ns) in map {
                if iri.starts_with(ns.as_str()) {
                    // A substitution, not a prefix strip: every occurrence of the
                    // namespace in the IRI becomes `prefix:`.
                    return iri.replace(ns.as_str(), &format!("{prefix}:"));
                }
            }
        }
        iri.to_string()
    }

    /// A cell that holds an entity: the label if we are using labels and it has
    /// one, else the CURIE.
    fn entity(&self, iri: &str) -> String {
        if let Some(labels) = &self.labels {
            if let Some(l) = labels.get(iri) {
                return l.clone();
            }
        }
        self.curie(iri)
    }

    /// A Subject/Property/Value cell: a value that resolves to a URL is an entity
    /// and is rendered as a label or CURIE, and everything else (a literal, a bare
    /// string) is written as-is.
    fn cell(&self, value: &str) -> String {
        if value.starts_with("http://") || value.starts_with("https://") {
            self.entity(value)
        } else {
            value.to_string()
        }
    }

    /// The Subject column, which has two extra rules: the ontology IRI is kept in
    /// full rather than shortened, and a subject that is not an IRI at all — a
    /// blank node — becomes the string "blank node".
    fn subject(&self, value: &str) -> String {
        if Some(value) == self.ontology_iri.as_deref() {
            return value.to_string();
        }
        if value.starts_with("_:") {
            return "blank node".to_string();
        }
        self.cell(value)
    }
}

/// The ontology IRI of `model`, if it declares one.
fn ontology_iri(model: &crate::model::Model) -> Option<String> {
    use horned_owl::model::Component;
    for ac in model.ont.iter() {
        if let Component::OntologyID(id) = &ac.component {
            if let Some(iri) = &id.iri {
                return Some(iri.as_ref().to_string());
            }
        }
    }
    None
}

/// Render the report as HTML: a Bootstrap-styled table, one `tr` class per
/// violation level, the rule name linked to its published documentation page (a
/// `file:` rule has no page, so no link), and each entity cell linked to its IRI.
/// `standalone` adds the `head`/`body` wrapper.
///
/// Cell text is written unescaped.
fn render_html_report(
    result: &crate::report::ReportResult,
    links: &[[Option<String>; 3]],
    standalone: bool,
) -> String {
    const BOOTSTRAP_CSS: &str =
        "https://stackpath.bootstrapcdn.com/bootstrap/4.5.2/css/bootstrap.min.css";
    let mut sb = String::new();
    if standalone {
        sb.push_str("<head>\n  <link rel=\"stylesheet\" href=\"");
        sb.push_str(BOOTSTRAP_CSS);
        sb.push_str("\">\n</head>\n<body>\n");
    }
    sb.push_str("<table class=\"table table-bordered table-striped\">\n");
    sb.push_str("<thead class=\"bg-dark text-white header-row\">\n<tr>\n");
    for h in ["Level", "Rule Name", "Subject", "Property", "Value"] {
        sb.push_str(&format!("  <th>{h}</th>\n"));
    }
    sb.push_str("</tr>\n</thead>\n");

    let anchor = |text: &str, href: &Option<String>| match href {
        Some(h) => format!("<a href=\"{h}\">{text}</a>"),
        None => text.to_string(),
    };
    for (row, link) in result.rows.iter().zip(links) {
        let tr_class = match row.level {
            Severity::Error => "table-danger",
            Severity::Warn => "table-warning",
            Severity::Info => "table-info",
        };
        sb.push_str(&format!("\t<tr class=\"{tr_class}\">\n"));
        sb.push_str(&format!("\t\t<td>{}</td>\n", row.level.label()));
        sb.push_str(&format!("\t\t<td>{}</td>\n", anchor(&row.rule, &row.rule_url)));
        sb.push_str(&format!("\t\t<td>{}</td>\n", anchor(&row.subject, &link[0])));
        sb.push_str(&format!("\t\t<td>{}</td>\n", anchor(&row.property, &link[1])));
        sb.push_str(&format!("\t\t<td>{}</td>\n", anchor(&row.value, &link[2])));
        sb.push_str("\t</tr>\n");
    }
    sb.push_str("</table>\n");
    if standalone {
        sb.push_str("</body>\n");
    }
    sb
}

/// Build an IRI -> label map from the model (for --labels).
fn label_map(model: &crate::model::Model) -> anyhow::Result<HashMap<String, String>> {
    let q = Queryable::from_model(model)?;
    let table = q.query_table(
        "SELECT ?e ?l WHERE { ?e <http://www.w3.org/2000/01/rdf-schema#label> ?l }",
    )?;
    let mut map = HashMap::new();
    for row in &table.rows {
        if row.len() >= 2 {
            map.insert(row[0].clone(), row[1].clone());
        }
    }
    Ok(map)
}

/// Print `n` violation rows, one per line, joined by the format's separator, under
/// a `First N violations:` heading.
fn print_n_violations(rows: &[Vec<String>], mut n: usize, sep: &str) {
    // `n` is a row count — the heading is printed in addition to the rows it
    // announces. Asking for more rows than exist is not an error: `n` is clamped
    // to the number of rows, so the heading never promises violations the report
    // does not have.
    if rows.len() + 1 <= n {
        n = rows.len();
    }
    println!("\nFirst {n} violations:");
    for row in rows.iter().take(n) {
        println!("{}", row.join(sep));
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    // Validate the switches BEFORE loading the ontology: a mistyped --fail-on or
    // an output format that cannot be written should not cost a full parse, and
    // must never be discovered after the report has already been written.
    let fail_on = FailOn::parse(&args.fail_on)?;
    let format = resolve_format(args.format.as_deref(), args.output.as_deref());
    if args.output.is_none() && matches!(format.as_str(), "html" | "json" | "yaml" | "xlsx") {
        // These formats are only ever written to a file; say so instead of
        // producing nothing.
        bail!("report: --format {format} requires --output (there is no terminal rendering for it)");
    }
    if format == "xlsx" {
        bail!("report: --format xlsx is not implemented; use tsv, csv, html, json or yaml");
    }

    // Whether a previous command in the chain handed us its model — the signal
    // that stdout is not ours to fill with a table (see the print step below).
    let chained = piped.is_some();
    // NOT `take_or_load`: `report` operates over the ontology it was given and
    // leaves the import chain alone, so a curator gets violations for the terms
    // they curate rather than for everything their imports drag in — resolving the
    // closure on OBA's `oba-edit.obo` yields 18,682 ERRORs, nearly all of them
    // imported entities tripping the custom `biological-attribute-child-violation`
    // rule across a 49 MB merged import. To report over the closure, chain an
    // explicit `merge --collapse-import-closure true` in front of it.
    //
    // `verify` is root-only for the same reason (see src/cmd/verify.rs).
    let mut model =
        crate::cmd::take_or_load_no_imports(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // --tdb: materialize a real on-disk dataset (kept with --keep-tdb-mappings);
    // the report itself is still evaluated in memory.
    let tdb = if args.tdb.unwrap_or(false) {
        crate::cmd::materialize_tdb(&model, args.tdb_directory.as_deref(), false)?
    } else {
        None
    };

    // The effective rule set. A `--profile` file REPLACES the default profile
    // rather than layering over it: the default is read only when no path was
    // given, so a rule the file omits does not run at all — and a `file:` rule it
    // names DOES.
    let rules: Vec<ReportRule> = match &args.profile {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("report: reading profile {}", path.display()))?;
            report::parse_profile(&text)?
        }
        None => report::default_profile(),
    };

    let mut result = report::run_report_with_profile(&model, &rules)?;

    // --base-iri: keep only violations whose subject is under one of the base
    // namespaces. The option is repeatable and the namespaces act as a set.
    if !args.base_iri.is_empty() {
        let bases: Vec<String> =
            args.base_iri.iter().map(|b| crate::cmd::select::expand(&model, b)).collect();
        result.rows.retain(|row| {
            let s = crate::cmd::select::expand(&model, &row.subject);
            bases.iter().any(|b| s.starts_with(b))
        });
    }

    // --limit caps violations PER RULE, not per report: the counter restarts for
    // each rule's query, and it counts only the rows that survived the built-in
    // and base-namespace skips. Rows are already grouped by rule, so a per-rule
    // counter over the sequence is exact.
    if let Some(limit) = args.limit {
        let mut rule = String::new();
        let mut seen = 0usize;
        result.rows.retain(|row| {
            if row.rule != rule {
                rule = row.rule.clone();
                seen = 0;
            }
            seen += 1;
            seen <= limit
        });
    }

    // Render entities LAST: `--base-iri` above keys on the full IRI, and the HTML
    // renderer needs it for each cell's link.
    let labels = if args.labels.unwrap_or(false) { Some(label_map(&model)?) } else { None };
    let short = ShortForm::new(&model, labels);
    let mut links: Vec<[Option<String>; 3]> = Vec::with_capacity(result.rows.len());
    for row in &mut result.rows {
        let link = |v: &str| {
            (v.starts_with("http://") || v.starts_with("https://")).then(|| v.to_string())
        };
        links.push([link(&row.subject), link(&row.property), link(&row.value)]);
        row.subject = short.subject(&row.subject);
        row.property = short.cell(&row.property);
        row.value = short.cell(&row.value);
    }

    let rendered = match format.as_str() {
        "html" => render_html_report(&result, &links, args.standalone.unwrap_or(true)),
        "csv" => result.to_csv(),
        "yaml" => result.to_yaml(),
        "json" => result.to_json(),
        // Any other format name is written as TSV.
        _ => result.to_tsv(),
    };
    if let Some(p) = &args.output {
        std::fs::write(p, &rendered)?;
    }

    // The summary block, on stdout, before anything is printed per-violation.
    let errors = result.count_at(Severity::Error);
    let warns = result.count_at(Severity::Warn);
    let infos = result.count_at(Severity::Info);
    if result.rows.is_empty() {
        println!("No violations found.");
    } else {
        println!("Violations: {}", result.rows.len());
        println!("-----------------");
        println!("ERROR:      {errors}");
        println!("WARN:       {warns}");
        println!("INFO:       {infos}");
    }

    // With no output file the whole table is printed instead of written
    // (`print == 0` becomes the row count). Only do that for a STANDALONE
    // `om report`: mid-chain, stdout belongs to the model the chain serializes.
    let sep = if format == "csv" { "," } else { "\t" };
    let mut print = args.print;
    if args.output.is_none() && print == 0 && !chained {
        print = result.rows.len();
    }
    if print > 0 {
        let rows: Vec<Vec<String>> = result
            .rows
            .iter()
            .map(|r| {
                vec![
                    r.level.label().to_string(),
                    r.rule.clone(),
                    r.subject.clone(),
                    r.property.clone(),
                    r.value.clone(),
                ]
            })
            .collect();
        print_n_violations(&rows, print, sep);
    }

    // Remove the on-disk TDB dataset unless --keep-tdb-mappings was given.
    crate::cmd::cleanup_tdb(tdb, args.keep_tdb_mappings.unwrap_or(false));

    if let FailOn::At(threshold) = fail_on {
        let n = result.count_at_least(threshold);
        if n > 0 {
            bail!("report: {n} violation(s) at or above {}", threshold.label());
        }
    }
    Ok(Some(model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_on_rejects_an_unknown_level() {
        assert_eq!(FailOn::parse("ERROR").unwrap(), FailOn::At(Severity::Error));
        assert_eq!(FailOn::parse("warn").unwrap(), FailOn::At(Severity::Warn));
        assert_eq!(FailOn::parse("none").unwrap(), FailOn::None);
        // An unrecognised level is an error, never a silently disabled gate.
        let e = FailOn::parse("errors").unwrap_err().to_string();
        assert!(e.contains("not a valid fail-on level"), "{e}");
    }

    #[test]
    fn format_follows_the_output_extension() {
        assert_eq!(resolve_format(None, None), "tsv");
        assert_eq!(resolve_format(None, Some(Path::new("reports/x-obo-report.tsv"))), "tsv");
        assert_eq!(resolve_format(None, Some(Path::new("reports/mondo-edit-report.html"))), "html");
        assert_eq!(resolve_format(None, Some(Path::new("report.YAML"))), "yaml");
        // --format wins over the extension.
        assert_eq!(resolve_format(Some("CSV"), Some(Path::new("report.tsv"))), "csv");
        // No extension to infer from, so the TSV default stands.
        assert_eq!(resolve_format(None, Some(Path::new("report"))), "tsv");
    }

    #[test]
    fn print_n_violations_matches_robots_off_by_one() {
        // Asking for exactly as many rows as there are prints them all…
        let rows: Vec<Vec<String>> =
            (0..3).map(|i| vec![format!("r{i}"), "x".into()]).collect();
        print_n_violations(&rows, 3, "\t");
        // …and asking for more prints all of them too, never panicking.
        print_n_violations(&rows, 99, "\t");
        print_n_violations(&[], 5, "\t");
    }
}
