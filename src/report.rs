//! `om report`: quality-control checks, run as SPARQL over the ontology.
//!
//! The checks are SPARQL queries bundled under `src/report_queries/` and embedded
//! at compile time, together with any custom `file:` rules the profile names, each
//! reported at the severity the profile gives it.
//!
//! The PROFILE IS THE RULE SET. The bundled default profile applies only when
//! `--profile` is absent; otherwise the profile's own lines are the whole rule
//! set. A rule is classified by whether it starts with `file:`: a bare name
//! selects one of the bundled queries, a `file:` entry names a SPARQL file to run
//! as an extra rule. Both kinds are real rules — OBA's `profile.txt` adds three at
//! ERROR, MONDO's twenty-seven — and a name that is neither aborts the run.
//!
//! Each query SELECTs `?entity ?property ?value`. A row is a *violation* when
//! `?entity` is bound and is not RDFS/OWL built-in vocabulary. A query that cannot
//! be parsed or evaluated is an error rather than a skip, and so is a `file:` rule
//! whose `?entity` is unbound, whether in the projection or in one row: a rule
//! dropped quietly contributes nothing to a report that still exits 0. A bundled
//! query is the one exception — every one of them binds `?entity`, so an unbound
//! one there skips the rule (or the row) as defence, degrading the report instead
//! of aborting a build.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::model::Model;
use crate::sparql::Queryable;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    Error,
    Warn,
    Info,
}

impl Severity {
    pub fn label(&self) -> &'static str {
        match self {
            Severity::Error => "ERROR",
            Severity::Warn => "WARN",
            Severity::Info => "INFO",
        }
    }
    pub fn parse(s: &str) -> Option<Severity> {
        match s.to_ascii_uppercase().as_str() {
            "ERROR" => Some(Severity::Error),
            "WARN" => Some(Severity::Warn),
            "INFO" => Some(Severity::Info),
            _ => None,
        }
    }
}

/// The bundled report queries, embedded at compile time. Each name is both the
/// name a profile selects the rule by and the name its violations report under.
const QUERIES: &[(&str, &str)] = &[
    ("annotation_whitespace", include_str!("report_queries/annotation_whitespace.rq")),
    ("deprecated_boolean_datatype", include_str!("report_queries/deprecated_boolean_datatype.rq")),
    ("deprecated_class_reference", include_str!("report_queries/deprecated_class_reference.rq")),
    ("deprecated_property_reference", include_str!("report_queries/deprecated_property_reference.rq")),
    ("duplicate_definition", include_str!("report_queries/duplicate_definition.rq")),
    ("duplicate_exact_synonym", include_str!("report_queries/duplicate_exact_synonym.rq")),
    ("duplicate_label", include_str!("report_queries/duplicate_label.rq")),
    ("duplicate_label_synonym", include_str!("report_queries/duplicate_label_synonym.rq")),
    ("duplicate_scoped_synonym", include_str!("report_queries/duplicate_scoped_synonym.rq")),
    ("equivalent_class_axiom_no_genus", include_str!("report_queries/equivalent_class_axiom_no_genus.rq")),
    ("equivalent_pair", include_str!("report_queries/equivalent_pair.rq")),
    ("illegal_use_of_built_in_vocabulary", include_str!("report_queries/illegal_use_of_built_in_vocabulary.rq")),
    ("invalid_entity_uri", include_str!("report_queries/invalid_entity_uri.rq")),
    ("invalid_xref", include_str!("report_queries/invalid_xref.rq")),
    ("label_formatting", include_str!("report_queries/label_formatting.rq")),
    ("label_whitespace", include_str!("report_queries/label_whitespace.rq")),
    ("lowercase_definition", include_str!("report_queries/lowercase_definition.rq")),
    ("missing_definition", include_str!("report_queries/missing_definition.rq")),
    ("missing_label", include_str!("report_queries/missing_label.rq")),
    ("missing_obsolete_label", include_str!("report_queries/missing_obsolete_label.rq")),
    ("missing_ontology_description", include_str!("report_queries/missing_ontology_description.rq")),
    ("missing_ontology_license", include_str!("report_queries/missing_ontology_license.rq")),
    ("missing_ontology_title", include_str!("report_queries/missing_ontology_title.rq")),
    ("missing_subset_declaration", include_str!("report_queries/missing_subset_declaration.rq")),
    ("missing_superclass", include_str!("report_queries/missing_superclass.rq")),
    ("missing_synonymtype_declaration", include_str!("report_queries/missing_synonymtype_declaration.rq")),
    ("misused_obsolete_label", include_str!("report_queries/misused_obsolete_label.rq")),
    ("misused_replaced_by", include_str!("report_queries/misused_replaced_by.rq")),
    ("multiple_asserted_superclasses", include_str!("report_queries/multiple_asserted_superclasses.rq")),
    ("multiple_definitions", include_str!("report_queries/multiple_definitions.rq")),
    ("multiple_equivalent_class_definitions", include_str!("report_queries/multiple_equivalent_class_definitions.rq")),
    ("multiple_equivalent_classes", include_str!("report_queries/multiple_equivalent_classes.rq")),
    ("multiple_labels", include_str!("report_queries/multiple_labels.rq")),
];

/// The default `report_profile.txt`, embedded at compile time.
const PROFILE: &str = include_str!("report_queries/report_profile.txt");

/// The bundled `obo_context.jsonld`, embedded at compile time. Report CURIEs are
/// shortened with THIS prefix map, not with the input document's — so an entity
/// reports under the same CURIE whatever prefixes the file it came from happens
/// to declare.
const OBO_CONTEXT: &str = include_str!("report_queries/obo_context.jsonld");

/// The base of each bundled rule's published documentation page. A bundled rule's
/// URL is this plus the rule name; a `file:` rule has none.
const RULE_DOC_BASE: &str = "http://robot.obolibrary.org/report_queries/";

/// Where a rule's SPARQL comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleSource {
    /// One of the bundled queries, embedded at compile time.
    Builtin(&'static str),
    /// A `file:` entry in the profile, resolved to a path on disk.
    File(PathBuf),
}

/// One rule of the effective profile: what to run, under what name, at what
/// level.
#[derive(Clone, Debug)]
pub struct ReportRule {
    pub name: String,
    pub severity: Severity,
    pub source: RuleSource,
}

impl ReportRule {
    /// The documentation URL for this rule, or `None` for a `file:` rule, which
    /// is a repo's own query and has no documentation page. Used as the rule
    /// cell's `href` in HTML output.
    pub fn rule_url(&self) -> Option<String> {
        match self.source {
            RuleSource::Builtin(_) => Some(format!("{RULE_DOC_BASE}{}", self.name)),
            RuleSource::File(_) => None,
        }
    }
}

/// The SPARQL of a bundled rule, by name.
pub fn builtin_query(name: &str) -> Option<&'static str> {
    QUERIES.iter().find(|(n, _)| *n == name).map(|(_, q)| *q)
}

/// Parse a profile (lines of `LEVEL<TAB>rule`) into the ordered rule set it
/// describes.
///
/// Each trimmed line splits on TAB — not on any whitespace, because a `file:`
/// rule's path may contain spaces — the level is uppercased, and anything other
/// than INFO, WARN or ERROR fails with `REPORT LEVEL ERROR`. There is no
/// `none`/`PASS` level: a rule is disabled by leaving it out.
///
/// A rule that is neither a `file:` path nor one of the bundled query names is
/// an error (`UNKNOWN REPORT QUERY`); every miss is collected and they are
/// reported together, so a typo in a profile cannot silently disable a check.
pub fn parse_profile(text: &str) -> Result<Vec<ReportRule>> {
    let mut rules: Vec<ReportRule> = Vec::new();
    // A profile is keyed on the RAW rule string, so a repeated rule collapses to
    // one entry at the level of its last line.
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut unknown: Vec<String> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        // A blank line is never a statement about a rule, so skip it instead of
        // failing on its missing level.
        if line.is_empty() {
            continue;
        }
        let Some((level, rule)) = line.split_once('\t') else {
            bail!("report: profile line '{line}' is not '<LEVEL><TAB><rule>'");
        };
        let rule = rule.trim();
        let Some(severity) = Severity::parse(level.trim()) else {
            bail!("report: REPORT LEVEL ERROR '{level}' is not a valid reporting level.");
        };
        let source = if rule.starts_with("file:") {
            RuleSource::File(resolve_file_rule(rule)?)
        } else {
            match builtin_query(rule) {
                Some(q) => RuleSource::Builtin(q),
                None => {
                    unknown.push(rule.to_string());
                    continue;
                }
            }
        };
        let entry = ReportRule { name: rule_name(rule), severity, source };
        match at.get(rule) {
            Some(&i) => rules[i] = entry,
            None => {
                at.insert(rule.to_string(), rules.len());
                rules.push(entry);
            }
        }
    }

    if !unknown.is_empty() {
        bail!(
            "report: UNKNOWN REPORT QUERY one or more rule names ('{}') are not valid default rules",
            unknown.join(", ")
        );
    }
    Ok(rules)
}

/// Resolve a `file:` profile entry to a path on disk: `file:///…` is an absolute
/// URL (percent-decoded), anything else is `file:` + a path taken RELATIVE TO THE
/// PROCESS CWD. The report runs from `src/ontology`, which is what makes MONDO's
/// `file:../sparql/qc/general/qc-reflexive.sparql` resolve.
///
/// A missing file is an error, not a filter: dropping the rule quietly would
/// leave the report claiming a check it never ran.
fn resolve_file_rule(rule: &str) -> Result<PathBuf> {
    let path = if rule.starts_with("file:///") {
        // Keep the leading `/` of the absolute path.
        PathBuf::from(percent_decode(&rule["file://".len()..]))
    } else {
        PathBuf::from(percent_decode(&rule["file:".len()..]))
    };
    if !path.exists() {
        bail!("report: MISSING QUERY ERROR query at '{}' does not exist.", path.display());
    }
    Ok(path)
}

/// The name a rule reports under. A `file:` rule takes its name from its path —
/// the basename with its final extension dropped — so
/// `file:../sparql/qc/general/qc-reflexive.sparql` reports as `qc-reflexive`.
/// A bundled rule has neither `/` nor `.`, so this returns it unchanged.
fn rule_name(rule: &str) -> String {
    let start = rule.rfind('/').map(|i| i + 1).unwrap_or(0);
    let base = &rule[start..];
    match base.rfind('.') {
        Some(i) if i > 0 => base[..i].to_string(),
        _ => base.to_string(),
    }
}

/// Decode `%XX` escapes in a `file:` URL's path (other bytes untouched), so the
/// path that reaches the filesystem is the one the URL denotes.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v as char);
                i += 3;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// The rule set from the bundled profile — what runs when `--profile` is absent.
/// The profile is a compile-time constant and every name in it is covered by
/// `QUERIES` (asserted by `default_profile_parses`), so parsing it cannot fail at
/// runtime.
pub fn default_profile() -> Vec<ReportRule> {
    parse_profile(PROFILE).expect("vendored report_profile.txt names an unknown rule")
}

/// The prefix map report CURIEs are rendered with: the bundled
/// `obo_context.jsonld`. Both JSON-LD term forms count — the plain
/// `"prefix": "namespace"` entries and the
/// `"prefix": {"@id": "…", "@prefix": true}` ones — while the empty key and the
/// `@`-keywords are skipped.
pub fn obo_context_prefixes() -> Vec<(String, String)> {
    let json: serde_json::Value = match serde_json::from_str(OBO_CONTEXT) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let ctx = json.get("@context").unwrap_or(&json);
    let Some(map) = ctx.as_object() else { return Vec::new() };
    let mut out = Vec::with_capacity(map.len());
    for (k, v) in map {
        if k.is_empty() || k.starts_with('@') {
            continue;
        }
        let ns = v.as_str().or_else(|| v.get("@id").and_then(|x| x.as_str()));
        if let Some(ns) = ns {
            out.push((k.clone(), ns.to_string()));
        }
    }
    out
}

pub struct ReportRow {
    pub level: Severity,
    pub rule: String,
    /// Documentation URL for the rule, or `None` for a `file:` rule.
    pub rule_url: Option<String>,
    pub subject: String,
    pub property: String,
    pub value: String,
}

pub struct ReportResult {
    pub rows: Vec<ReportRow>,
}

impl ReportResult {
    pub fn count_at_least(&self, sev: Severity) -> usize {
        self.rows.iter().filter(|r| r.level <= sev).count()
    }

    /// Violations at exactly this level — the summary block prints one line per
    /// level.
    pub fn count_at(&self, sev: Severity) -> usize {
        self.rows.iter().filter(|r| r.level == sev).count()
    }

    /// The report as TSV: Level / Rule Name / Subject / Property / Value.
    pub fn to_tsv(&self) -> String {
        let mut out = String::from("Level\tRule Name\tSubject\tProperty\tValue\n");
        for r in &self.rows {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                tsv_cell(r.level.label()),
                tsv_cell(&r.rule),
                tsv_cell(&r.subject),
                tsv_cell(&r.property),
                tsv_cell(&r.value)
            ));
        }
        out
    }

    /// The same table as CSV, with RFC-4180 quoting.
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        out.push_str(&csv_row(&["Level", "Rule Name", "Subject", "Property", "Value"]));
        for r in &self.rows {
            out.push_str(&csv_row(&[
                r.level.label(),
                &r.rule,
                &r.subject,
                &r.property,
                &r.value,
            ]));
        }
        out
    }

    /// The report as YAML: one block per level that has violations, each listing
    /// its rules and their violations.
    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        for level in [Severity::Error, Severity::Warn, Severity::Info] {
            let rows: Vec<&ReportRow> = self.rows.iter().filter(|r| r.level == level).collect();
            if rows.is_empty() {
                continue;
            }
            out.push_str(&format!("- level: '{}'\n  violations:\n", level.label()));
            let mut rule = "";
            for (i, r) in rows.iter().enumerate() {
                if i == 0 || r.rule != rule {
                    rule = &r.rule;
                    out.push_str(&format!("  - {rule}:\n"));
                }
                out.push_str(&format!("    - subject: \"{}\"\n", r.subject));
                // Property and value are emitted for every violation, including
                // one whose value is a literal rather than an entity: a report of
                // bare subjects is not a report.
                out.push_str(&format!("      property: \"{}\"\n", r.property));
                out.push_str(&format!("      values:\n        - \"{}\"\n", r.value));
            }
        }
        out
    }

    /// The report as JSON: the same level / rule / violation structure as the
    /// YAML above, pretty-printed.
    pub fn to_json(&self) -> String {
        let mut levels = Vec::new();
        for level in [Severity::Error, Severity::Warn, Severity::Info] {
            let rows: Vec<&ReportRow> = self.rows.iter().filter(|r| r.level == level).collect();
            if rows.is_empty() {
                continue;
            }
            let mut by_rule: Vec<(String, Vec<serde_json::Value>)> = Vec::new();
            for r in rows {
                let violation = serde_json::json!({
                    "subject": r.subject,
                    "property": r.property,
                    "values": [r.value],
                });
                match by_rule.last_mut() {
                    Some((name, vs)) if *name == r.rule => vs.push(violation),
                    _ => by_rule.push((r.rule.clone(), vec![violation])),
                }
            }
            let violations: Vec<serde_json::Value> = by_rule
                .into_iter()
                .map(|(name, vs)| {
                    let mut m = serde_json::Map::new();
                    m.insert(name, serde_json::Value::Array(vs));
                    serde_json::Value::Object(m)
                })
                .collect();
            levels.push(serde_json::json!({
                "level": level.label(),
                "violations": violations,
            }));
        }
        serde_json::to_string_pretty(&levels).unwrap_or_else(|_| "[]".to_string())
    }
}

fn csv_row(cells: &[&str]) -> String {
    let escaped: Vec<String> = cells.iter().map(|c| escape_csv(c)).collect();
    let mut s = escaped.join(",");
    s.push('\n');
    s
}

fn escape_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Whether a violation is skipped because its subject is built-in vocabulary.
///
/// Exactly two namespaces count, and they match by SUBSTRING: `/rdf-schema#` and
/// `/owl#`. The RDF namespace is deliberately not among them —
/// `illegal_use_of_built_in_vocabulary` binds `VALUES ?entity { rdf:type }`, so
/// excluding `22-rdf-syntax-ns#` would leave that ERROR-level rule unable to fire
/// at all.
fn is_builtin(entity: &str) -> bool {
    entity.contains("/rdf-schema#") || entity.contains("/owl#")
}

/// Whether a query variable was left UNBOUND in this row. The SPARQL-TSV cell is
/// empty only for an unbound variable — a literal renders as `""` — so it, and
/// not the plain cell, decides.
fn unbound(plain: &str, tsv: Option<&str>) -> bool {
    match tsv {
        Some(t) => t.is_empty(),
        None => plain.is_empty(),
    }
}

/// Render a solution binding as a report cell: a literal is its lexical form,
/// then `@lang` for a language-tagged literal, or `^^<datatype>` for a typed one
/// that is not `xsd:string`. An IRI or blank node renders as itself.
///
/// `plain` is the lexical form (oxigraph's `term_to_string`), `tsv` the same term
/// in SPARQL-TSV form (`"lex"@en`, `"lex"^^<dt>`, `<iri>`), which is where the
/// tag and datatype survive.
fn term_display(plain: &str, tsv: Option<&str>) -> String {
    let Some(tsv) = tsv else { return plain.to_string() };
    if !tsv.starts_with('"') {
        return plain.to_string();
    }
    let Some(close) = tsv.rfind('"') else { return plain.to_string() };
    let suffix = &tsv[close + 1..];
    if let Some(lang) = suffix.strip_prefix('@') {
        return format!("{plain}@{lang}");
    }
    if let Some(dt) = suffix.strip_prefix("^^<").and_then(|d| d.strip_suffix('>')) {
        // The datatype IRI is written bare, without angle brackets, and is
        // omitted entirely for xsd:string (which the TSV form already does).
        return format!("{plain}^^{dt}");
    }
    plain.to_string()
}

/// Run the report over a model, using the default rule set.
pub fn run_report(model: &Model) -> Result<ReportResult> {
    run_report_with_profile(model, &default_profile())
}

/// Run an explicit rule set over a model. Every rule in the set runs — the set
/// IS the profile — and its violations are collected in the order its query
/// returned them.
pub fn run_report_with_profile(model: &Model, rules: &[ReportRule]) -> Result<ReportResult> {
    let q = Queryable::from_model(model)?;
    // One bucket per rule, each holding that rule's violations in query order.
    // Each bucket also records whether its query ordered by `?entity`, which
    // decides how the rows it left tied are settled.
    let mut buckets: Vec<(&ReportRule, bool, Vec<ReportRow>)> = Vec::new();

    for rule in rules {
        let query = match &rule.source {
            RuleSource::Builtin(text) => (*text).to_string(),
            RuleSource::File(path) => std::fs::read_to_string(path)
                .with_context(|| format!("report: reading rule '{}' from {}", rule.name, path.display()))?,
        };

        // A query that cannot be run stops the report. Skipping the rule with a
        // warning would be invisible in both the report and the exit code, which
        // is the whole failure mode for a repo's custom rules.
        let table = q
            .query_table(&query)
            .with_context(|| format!("report: rule '{}' could not be run", rule.name))?;

        let entity_idx = table.columns.iter().position(|c| c == "entity");
        let prop_idx = table.columns.iter().position(|c| c == "property");
        let value_idx = table.columns.iter().position(|c| c == "value");

        let Some(entity_idx) = entity_idx else {
            // A rule that never projects ?entity can report nothing at all.
            if matches!(rule.source, RuleSource::File(_)) {
                bail!("report: MISSING ENTITY BINDING query '{}' must include an '?entity'", rule.name);
            }
            // Every bundled query binds ?entity; keep the skip as defence so a
            // mistake in one of them degrades instead of aborting a build.
            continue;
        };

        let mut rows = Vec::new();
        let mut warned_property = false;
        let mut warned_value = false;
        for (i, row) in table.rows.iter().enumerate() {
            let tsv = table.tsv_rows.get(i);
            let cell = |idx: usize| -> (&str, Option<&str>) {
                (
                    row.get(idx).map(String::as_str).unwrap_or(""),
                    tsv.and_then(|t| t.get(idx)).map(String::as_str),
                )
            };

            let (entity, entity_tsv) = cell(entity_idx);
            if unbound(entity, entity_tsv) {
                // An unbound `?entity` in ANY row is an error, not only a query
                // that never projects the column.
                if matches!(rule.source, RuleSource::File(_)) {
                    bail!(
                        "report: MISSING ENTITY BINDING query '{}' must include an '?entity'",
                        rule.name
                    );
                }
                continue;
            }
            let subject = term_display(entity, entity_tsv);
            if is_builtin(&subject) {
                continue;
            }

            let property = match prop_idx {
                Some(idx) => {
                    let (p, p_tsv) = cell(idx);
                    if unbound(p, p_tsv) {
                        if !warned_property {
                            status!("WARN: '{}' query is missing ?property variable", rule.name);
                            warned_property = true;
                        }
                        String::new()
                    } else {
                        term_display(p, p_tsv)
                    }
                }
                None => {
                    if !warned_property {
                        status!("WARN: '{}' query is missing ?property variable", rule.name);
                        warned_property = true;
                    }
                    String::new()
                }
            };

            let value = match value_idx {
                Some(idx) => {
                    let (v, v_tsv) = cell(idx);
                    if unbound(v, v_tsv) {
                        if !warned_value {
                            status!("WARN: '{}' query is missing ?value variable", rule.name);
                            warned_value = true;
                        }
                        String::new()
                    } else {
                        term_display(v, v_tsv)
                    }
                }
                None => {
                    if !warned_value {
                        status!("WARN: '{}' query is missing ?value variable", rule.name);
                        warned_value = true;
                    }
                    String::new()
                }
            };

            rows.push(ReportRow {
                level: rule.severity,
                rule: rule.name.clone(),
                rule_url: rule.rule_url(),
                subject,
                property,
                value,
            });
        }
        let by_entity = query
            .to_ascii_uppercase()
            .rsplit("ORDER BY")
            .next()
            .is_some_and(|tail| tail.trim().starts_with("?ENTITY"))
            && query.to_ascii_uppercase().contains("ORDER BY");
        buckets.push((rule, by_entity, rows));
    }

    // The ERROR rules come first, then WARN, then INFO, with only the RULE NAMES
    // sorted alphabetically within each level; the violations inside a rule keep
    // the order their query returned them in. Six of the bundled queries do not
    // `ORDER BY ?entity` — three order by `DESC(UCASE(str(?value)))` — so
    // re-sorting every row by subject would rewrite blocks like CL's
    // `duplicate_exact_synonym` into an order its released report does not have.
    buckets.sort_by(|a, b| a.0.severity.cmp(&b.0.severity).then_with(|| a.0.name.cmp(&b.0.name)));
    for (_, by_entity, rows) in buckets.iter_mut() {
        break_order_ties(rows, *by_entity);
    }
    let rows: Vec<ReportRow> = buckets.into_iter().flat_map(|(_, _, rows)| rows).collect();
    Ok(ReportResult { rows })
}

/// A TSV cell: quoted only when it holds the tab separator, a quote or a line
/// break, with an embedded quote doubled. A reported value can contain a newline
/// — MP's `MP:0030295` definition runs across two lines — and writing that bare
/// would split one violation into two rows.
fn tsv_cell(s: &str) -> String {
    if s.contains('\t') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Order the rows a rule's `ORDER BY` leaves TIED.
///
/// A query that orders by `?value` alone leaves every row sharing a value tied,
/// and one that orders by `?entity` leaves every row sharing a subject tied.
/// Either way the tie is settled by the columns the ORDER BY did not name, in
/// the order the report prints them: subject for a value-ordered query, and
/// property then value for an entity-ordered one — so UBERON's `UBERON:0035925`
/// lists its six `hasExactSynonym` duplicates before its six `hasRelatedSynonym`
/// ones, each block by value.
fn break_order_ties(rows: &mut [ReportRow], by_entity: bool) {
    let mut i = 0;
    while i < rows.len() {
        let mut j = i + 1;
        while j < rows.len()
            && if by_entity {
                rows[j].subject == rows[i].subject
            } else {
                rows[j].property == rows[i].property
                    && rows[j].value.to_uppercase() == rows[i].value.to_uppercase()
            }
        {
            j += 1;
        }
        if j - i > 1 {
            if by_entity {
                rows[i..j].sort_by(|a, b| {
                    a.property.cmp(&b.property).then_with(|| a.value.cmp(&b.value))
                });
            } else {
                // Equal subjects break the tie on the value, case-sensitively:
                // two spellings that fold to one duplicate ("MGA1"/"Mga1")
                // order uppercase first.
                rows[i..j].sort_by(|a, b| {
                    a.subject.cmp(&b.subject).then_with(|| a.value.cmp(&b.value))
                });
            }
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_parses() {
        let p = default_profile();
        let level = |name: &str| p.iter().find(|r| r.name == name).map(|r| r.severity);
        // A representative sample of the default profile's entries.
        assert_eq!(level("missing_label"), Some(Severity::Error));
        assert_eq!(level("missing_definition"), Some(Severity::Warn));
        assert_eq!(level("lowercase_definition"), Some(Severity::Info));
        // The default profile covers most, but not all, of the bundled queries
        // (e.g. `multiple_asserted_superclasses` is absent and thus disabled by
        // default). Every entry resolves to a bundled query — an unknown name
        // would have aborted `parse_profile`.
        assert!(p.iter().all(|r| matches!(r.source, RuleSource::Builtin(_))));
        assert_eq!(level("multiple_asserted_superclasses"), None);
    }

    #[test]
    fn profile_splits_on_tab_only() {
        // A `file:` path may contain spaces, so a line splits on TAB alone;
        // splitting on any whitespace would truncate the path and lose the rule.
        let dir = std::env::temp_dir().join("om_report_profile_tab");
        std::fs::create_dir_all(&dir).unwrap();
        let query = dir.join("my rule.sparql");
        std::fs::write(&query, "SELECT ?entity WHERE {}").unwrap();
        let text = format!("ERROR\tfile:{}\n", query.display());
        let rules = parse_profile(&text).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "my rule");
        assert_eq!(rules[0].source, RuleSource::File(query));
        // …and no doc URL, since a repo's own query has no documentation page.
        assert_eq!(rules[0].rule_url(), None);
    }

    #[test]
    fn profile_rejects_bad_entries() {
        // Unknown rule name: UNKNOWN REPORT QUERY, listing every miss.
        let e = parse_profile("ERROR\tmissing_label\nERROR\tno_such_rule\nWARN\talso_bogus\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("UNKNOWN REPORT QUERY"), "{e}");
        assert!(e.contains("no_such_rule") && e.contains("also_bogus"), "{e}");
        // Invalid level: REPORT LEVEL ERROR, not a silent skip.
        let e = parse_profile("BOGUS\tmissing_label\n").unwrap_err().to_string();
        assert!(e.contains("REPORT LEVEL ERROR"), "{e}");
        // `none` is not a reporting level — a rule is disabled by omitting it.
        assert!(parse_profile("none\tmissing_label\n").is_err());
        // A missing `file:` rule is an error, never a silent drop.
        let e = parse_profile("ERROR\tfile:./does-not-exist.sparql\n").unwrap_err().to_string();
        assert!(e.contains("MISSING QUERY ERROR"), "{e}");
    }

    #[test]
    fn file_rule_name_is_the_basename() {
        assert_eq!(rule_name("file:../sparql/qc/general/qc-reflexive.sparql"), "qc-reflexive");
        assert_eq!(rule_name("file:///tmp/a.b/qc-two.pattern.sparql"), "qc-two.pattern");
        assert_eq!(rule_name("missing_label"), "missing_label");
    }

    #[test]
    fn csv_quoting_escapes_specials() {
        assert_eq!(escape_csv("plain"), "plain");
        assert_eq!(escape_csv("a,b"), "\"a,b\"");
        assert_eq!(escape_csv("a\"b"), "\"a\"\"b\"");
        assert_eq!(escape_csv("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn builtin_detection() {
        // RDFS and OWL are skipped — by substring — and NOTHING else.
        assert!(is_builtin("http://www.w3.org/2002/07/owl#Class"));
        assert!(is_builtin("http://www.w3.org/2000/01/rdf-schema#label"));
        // rdf: and xsd: are NOT skipped: `illegal_use_of_built_in_vocabulary`
        // reports on `rdf:type` and can only ever have it as its subject.
        assert!(!is_builtin("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"));
        assert!(!is_builtin("http://www.w3.org/2001/XMLSchema#string"));
        assert!(!is_builtin("http://purl.obolibrary.org/obo/GO_0008150"));
    }

    #[test]
    fn literals_keep_their_tag_and_datatype() {
        assert_eq!(term_display("same", Some("\"same\"@en")), "same@en");
        assert_eq!(term_display("same", Some("\"same\"")), "same");
        assert_eq!(
            term_display("true", Some("\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>")),
            "true^^http://www.w3.org/2001/XMLSchema#boolean"
        );
        // An IRI or blank node is its own rendering.
        let iri = "http://purl.obolibrary.org/obo/GO_0008150";
        assert_eq!(term_display(iri, Some(&format!("<{iri}>"))), iri);
        // A quote inside the lexical form does not confuse the suffix scan.
        assert_eq!(term_display("a\"b", Some("\"a\\\"b\"@en")), "a\"b@en");
    }

    #[test]
    fn unbound_is_an_empty_tsv_cell() {
        assert!(unbound("", Some("")));
        // An empty LITERAL is bound; its TSV form is a pair of quotes.
        assert!(!unbound("", Some("\"\"")));
        assert!(unbound("", None));
    }

    #[test]
    fn obo_context_has_the_prefixes_robot_reports_with() {
        let p = obo_context_prefixes();
        let ns = |prefix: &str| {
            p.iter().find(|(k, _)| k == prefix).map(|(_, v)| v.as_str())
        };
        // Both JSON-LD forms: a plain string value and a {"@id", "@prefix"} object.
        assert_eq!(ns("obo"), Some("http://purl.obolibrary.org/obo/"));
        assert_eq!(ns("OBA"), Some("http://purl.obolibrary.org/obo/OBA_"));
        // A report prints `dc:title`, not `terms:title`, because the context
        // binds `dc` to the DCMI TERMS namespace.
        assert_eq!(ns("dc"), Some("http://purl.org/dc/terms/"));
    }
}
