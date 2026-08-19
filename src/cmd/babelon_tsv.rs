//! Babelon TSV table operations — the `merge` and `prepare-translation` steps of
//! a translation workflow, which HPO's release drives for its thirteen
//! translations.
//!
//! The table produced here is converted to OWL and released as
//! `hp-international.owl`, and translators keep editing the tables themselves, so
//! the row order, the column set and the empty-cell spelling are all part of the
//! contract:
//!
//!  * Concatenating tables unions the columns — a column missing from one input
//!    is empty for every row of that input. Column ORDER is first-seen: the first
//!    file's header, then any new column in the order a later file introduces it.
//!  * The babelon sort is by `subject_id`, `predicate_id`, `source_value` and is
//!    STABLE, so rows that tie on all three keep their concatenation order.
//!  * Duplicate elimination keeps the FIRST of a set of identical rows.
//!  * `--sort-tables` defaults to TRUE, `--drop-unknown-columns` to FALSE. HPO's
//!    `merge` passes neither, so its output IS sorted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The columns a babelon translation defines, in declaration order.
/// `--drop-unknown-columns` is a membership test against this list, not a
/// reordering: a table keeps its own column order and simply loses whatever is
/// not named here.
pub const TRANSLATION_FIELDS: [&str; 16] = [
    "subject_id",
    "predicate_id",
    "source_value",
    "source_language",
    "translation_value",
    "translation_language",
    "source_version",
    "translation_type",
    "translator",
    "translator_expertise",
    "translation_date",
    "translation_confidence",
    "translation_precision",
    "translation_status",
    "source",
    "comment",
];

/// A babelon table: a column order plus rows keyed by column name. Kept open
/// rather than modelled as a fixed struct, because the format is open — a project
/// may carry extra columns, and `merge` has to keep them unless it is told
/// otherwise.
#[derive(Clone, Debug, Default)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<BTreeMap<String, String>>,
}

impl Table {
    /// Read a TSV. A parsed row holds only the cells that carry a value: a short
    /// row supplies no cell at all, and an empty cell is one of the `NA_VALUES`
    /// spellings, so neither reaches the row map. An absent cell and an empty one
    /// are indistinguishable afterwards, and both are written back as empty.
    pub fn read(path: &Path) -> Result<Table> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading babelon TSV {}", path.display()))?;
        Ok(Table::parse(&text))
    }

    pub fn parse(text: &str) -> Table {
        let mut records = crate::table::read_tsv(text);
        if records.is_empty() {
            return Table::default();
        }
        // babelon reads every table with a bare `pd.read_csv(input, sep="\t")`
        // (`babelon/cli.py`), so the FIRST record is the header, always — there is
        // no sniffing and no positional fallback. A file whose first record is data
        // therefore names no babelon slot, and the first `row["subject_id"]` in
        // `prepare_translation_for_ontology` raises `KeyError`. `prepare_translation`
        // below reproduces that; guessing the columns instead would build an
        // artefact babelon cannot build.
        let columns = records.remove(0);
        let mut rows = Vec::new();
        for cells in records {
            let mut row = BTreeMap::new();
            for (i, c) in columns.iter().enumerate() {
                if let Some(v) = cells.get(i) {
                    // A recognised NA sentinel is a missing value, not text.
                    if NA_VALUES.contains(&v.as_str()) {
                        continue;
                    }
                    row.insert(c.clone(), v.clone());
                }
            }
            rows.push(row);
        }
        Table { columns, rows }
    }

    /// Concatenate another table: rows appended, columns unioned first-seen.
    pub fn concat(&mut self, other: &Table) {
        for c in &other.columns {
            if !self.columns.contains(c) {
                self.columns.push(c.clone());
            }
        }
        self.rows.extend(other.rows.iter().cloned());
    }

    /// The babelon sort order: stable, by subject_id, predicate_id, source_value.
    pub fn sort(&mut self) {
        let key = |r: &BTreeMap<String, String>| {
            (
                r.get("subject_id").cloned().unwrap_or_default(),
                r.get("predicate_id").cloned().unwrap_or_default(),
                r.get("source_value").cloned().unwrap_or_default(),
            )
        };
        self.rows.sort_by_key(key);
    }

    /// Keep only babelon's own fields, in this table's own column order.
    pub fn drop_unknown_columns(&mut self) {
        self.columns.retain(|c| TRANSLATION_FIELDS.contains(&c.as_str()));
    }

    /// Serialise: a header line, then a line per row, missing values written as
    /// the empty string, and a trailing newline.
    pub fn to_tsv(&self) -> String {
        // A column is typed by its contents and written back in that type's
        // spelling. The case that shows is a numeric column holding a blank: the
        // blank makes it a float column, so a cell that arrived as `1` leaves as
        // `1.0` — HPO's Japanese `translation_confidence` is 17k rows of exactly
        // that, and any other spelling rewrites every line of the table.
        let floats: Vec<bool> = self.columns.iter().map(|c| self.is_float_column(c)).collect();
        let mut out = String::new();
        out.push_str(&self.columns.iter().map(|c| quote_field(c)).collect::<Vec<_>>().join("\t"));
        out.push('\n');
        for r in &self.rows {
            let cells: Vec<String> = self
                .columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let v = r.get(c).map(String::as_str).unwrap_or("");
                    if floats[i] {
                        if let Ok(f) = v.trim().parse::<f64>() {
                            return format_float(f);
                        }
                    }
                    quote_field(v)
                })
                .collect();
            out.push_str(&cells.join("\t"));
            out.push('\n');
        }
        out
    }

    /// Whether this column is a float column: every present value is numeric, and
    /// it is not a clean integer column (an integer column with no missing value
    /// stays an integer column and prints without a fractional part).
    fn is_float_column(&self, col: &str) -> bool {
        let mut any_value = false;
        let mut any_blank = false;
        let mut all_int = true;
        for r in &self.rows {
            let v = r.get(col).map(String::as_str).unwrap_or("").trim();
            if v.is_empty() {
                any_blank = true;
                continue;
            }
            if v.parse::<f64>().is_err() {
                return false;
            }
            any_value = true;
            if v.parse::<i64>().is_err() {
                all_int = false;
            }
        }
        any_value && (any_blank || !all_int)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(path, self.to_tsv())
            .with_context(|| format!("writing babelon TSV {}", path.display()))
    }
}


/// The spellings a TSV cell may use for "no value". A cell holding one of these
/// is read as missing, so it is written back EMPTY — HPO's Japanese table stores
/// untranslated values as the literal `NA`, and carrying that spelling through
/// would put `NA` into the released table where it must be blank.
const NA_VALUES: [&str; 19] = [
    "-1.#IND", "1.#QNAN", "1.#IND", "-1.#QNAN", "#N/A N/A", "#N/A", "N/A", "n/a", "NA", "<NA>",
    "#NA", "NULL", "null", "NaN", "-NaN", "nan", "-nan", "", "None",
];

/// The float spelling these tables carry: an integral value keeps a single
/// trailing zero (`1` → `1.0`), anything else takes the shortest round-tripping
/// form.
pub(crate) fn format_float(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

/// Minimal CSV quoting: a field is quoted only when it contains the delimiter,
/// the quote character, or a line break, and an embedded quote is doubled. HPO's
/// French and Spanish tables carry quoted phrases (`"croisés"`); written raw
/// those produce a table that no longer round-trips.
pub(crate) fn quote_field(s: &str) -> String {
    if s.contains('\t') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Merge babelon tables: concatenate the inputs and (by default) sort.
///
/// Identical rows are KEPT. A translation table may legitimately carry the same
/// row twice — HPO's French table has 230 such rows across the thirteen inputs —
/// and the merged table is a concatenation, not a set.
///
/// `--update-translations` replaces the older row for a
/// (source_language, translation_language, subject_id, predicate_id) key with the
/// later file's: rows matching a joined key are dropped from the accumulated
/// table before the later one is concatenated onto it.
pub fn merge(
    inputs: &[PathBuf],
    sort_tables: bool,
    drop_unknown: bool,
    update_translations: bool,
) -> Result<Table> {
    let mut it = inputs.iter();
    let first = it.next().context("babelon merge: no input files")?;
    let mut df = Table::read(first)?;
    for path in it {
        let next = Table::read(path)?;
        if update_translations {
            let keys = ["source_language", "translation_language", "subject_id", "predicate_id"];
            let temp = |r: &BTreeMap<String, String>| -> String {
                keys.iter()
                    .map(|k| r.get(*k).cloned().unwrap_or_else(|| "nan".to_string()))
                    .collect::<Vec<_>>()
                    .join("_")
            };
            let incoming: std::collections::HashSet<String> =
                next.rows.iter().map(temp).collect();
            df.rows.retain(|r| !incoming.contains(&temp(r)));
        }
        df.concat(&next);
    }
    if sort_tables {
        df.sort();
    }
    if drop_unknown {
        df.drop_unknown_columns();
    }
    Ok(df)
}

// ---------------------------------------------------------------------------
// prepare-translation
// ---------------------------------------------------------------------------

/// The term metadata `prepare-translation` consults: a map from predicate CURIE
/// to values, per term, plus the order the terms appear in.
///
/// `rdfs:label` and `IAO:0000115` are overlaid onto each term's map from its name
/// and definition, so those two keys are present whenever the term has a name or
/// a definition, whatever annotation property the source file spells them with.
#[derive(Default, Debug)]
pub struct TermMeta {
    /// The terms to translate: declaration order, obsoletes excluded.
    pub terms: Vec<String>,
    /// Metadata for EVERY term, obsolete ones included — an existing translation
    /// row may still name an obsolete term, and it has to resolve.
    pub meta: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

impl TermMeta {
    /// Read an OBO document: every `[Term]` stanza contributes its `name:` as
    /// `rdfs:label` and the quoted part of its `def:` as `IAO:0000115`.
    ///
    /// Obsolete terms are excluded from [`terms`](Self::terms), because a
    /// translation profile covers only live terms — including them puts hundreds
    /// of `obsolete …` rows into a not-translated report (562 of them in HPO's
    /// German one) for labels no curator will ever translate.
    pub fn from_obo(path: &Path) -> Result<TermMeta> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading ontology {}", path.display()))?;
        let mut out = TermMeta::default();
        let mut id: Option<String> = None;
        let mut in_term = false;
        let mut obsolete: std::collections::HashSet<String> = std::collections::HashSet::new();
        for line in text.lines() {
            let l = line.trim_end();
            if l.starts_with('[') {
                in_term = l == "[Term]";
                id = None;
                continue;
            }
            if !in_term {
                continue;
            }
            if let Some(v) = l.strip_prefix("id: ") {
                let v = v.trim().to_string();
                if !out.meta.contains_key(&v) {
                    out.terms.push(v.clone());
                }
                out.meta.entry(v.clone()).or_default();
                id = Some(v);
            } else if let Some(v) = l.strip_prefix("name: ") {
                if let Some(t) = &id {
                    out.meta.entry(t.clone()).or_default().insert(
                        "rdfs:label".to_string(),
                        vec![unescape_obo(v.trim())],
                    );
                }
            } else if l == "is_obsolete: true" {
                if let Some(t) = &id {
                    obsolete.insert(t.clone());
                }
            } else if let Some(v) = l.strip_prefix("def: ") {
                // `def: "text" [xrefs]` — only the quoted text is the definition.
                if let Some(t) = &id {
                    if let Some(text) = quoted_prefix(v.trim()) {
                        out.meta
                            .entry(t.clone())
                            .or_default()
                            .insert("IAO:0000115".to_string(), vec![text]);
                    }
                }
            }
        }
        out.terms.retain(|t| !obsolete.contains(t));
        Ok(out)
    }

    fn get(&self, term: &str) -> Option<&BTreeMap<String, Vec<String>>> {
        self.meta.get(term)
    }
}

/// The leading `"…"` of an OBO tag value, with the standard escapes undone.
fn quoted_prefix(s: &str) -> Option<String> {
    let rest = s.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            },
            '"' => return Some(out),
            _ => out.push(c),
        }
    }
    None
}

fn unescape_obo(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Compare two source values loosely: strip ASCII punctuation, collapse
/// whitespace, lowercase, compare. Deliberately forgiving — a source value that
/// differs only in punctuation is not treated as a change, though it IS still
/// rewritten to the ontology's spelling.
fn is_equivalent_string(a: &str, b: &str) -> bool {
    fn normalize(s: &str) -> String {
        // The ASCII punctuation set.
        const PUNCT: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
        let stripped: String = s.chars().filter(|c| !PUNCT.contains(*c)).collect();
        stripped.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
    }
    normalize(a) == normalize(b)
}

/// The three tables `prepare-translation` produces.
pub struct Prepared {
    pub profile: Table,
    pub source_changed: Table,
    pub not_translated: Table,
}

/// `prepare-translation`: reconcile a translation table against the current
/// ontology.
///
/// For each existing row whose predicate the term still carries, the ontology's
/// own value replaces `source_value` — always, even when the two are only
/// equivalent — and when they genuinely differ the row is reported as
/// source-changed and (with `--update-translation-status`) flipped to
/// `CANDIDATE`. Rows already marked `NOT_TRANSLATED` are reported and, unless
/// `--include-not-translated`, dropped. Finally every (term, field) pair the
/// table does not already cover is emitted as a `NOT_TRANSLATED` row.
pub fn prepare_translation(
    input: Option<&Path>,
    ontology: &TermMeta,
    language_code: &str,
    fields: &[String],
    terms: Option<&[String]>,
    include_not_translated: bool,
    update_translation_status: bool,
) -> Result<Prepared> {
    let mut df = match input {
        Some(p) => Table::read(p)?,
        None => default_table(),
    };

    // Each collected row carries the column order of the table it came from: a
    // report's columns are first-seen across its rows, so a row has to remember
    // its source table's order. Collecting into a sorted map alone would emit the
    // reports with alphabetised headers.
    // `prepare_translation_for_ontology` indexes each row by name, and a pandas
    // Series raises `KeyError` for a column the frame does not have — so a table
    // missing any of these does not produce blank rows, it stops the build. An
    // EMPTY frame is iterated zero times and never raises, so only a table with
    // rows is checked. MONDO is where this bites: an automated PR (139f4d6)
    // rewrote `src/translations/mondo-jp.babelon.tsv` without its header row, and
    // the ODK's own `babelon prepare-translation` dies on it with
    // `KeyError: 'subject_id'`.
    if !df.rows.is_empty() {
        for col in ["subject_id", "predicate_id", "source_value", "translation_status"] {
            if !df.columns.iter().any(|c| c == col) {
                anyhow::bail!("babelon prepare-translation: no '{col}' column");
            }
        }
    }
    let input_columns = df.columns.clone();
    let mut source_changed_rows: Vec<(Vec<String>, BTreeMap<String, String>)> = Vec::new();
    let mut not_translated_rows: Vec<(Vec<String>, BTreeMap<String, String>)> = Vec::new();
    // (subject_id -> predicates already present), so the pass below only adds
    // what the table does not already cover.
    let mut processed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut remove: Vec<usize> = Vec::new();

    for (index, row) in df.rows.iter_mut().enumerate() {
        let subject_id = row.get("subject_id").cloned().unwrap_or_default();
        let predicate_id = row.get("predicate_id").cloned().unwrap_or_default();
        let source_value = row.get("source_value").cloned().unwrap_or_default();
        let translation_status = row.get("translation_status").cloned().unwrap_or_default();
        let entry = processed.entry(subject_id.clone()).or_default();
        if !entry.contains(&predicate_id) {
            entry.push(predicate_id.clone());
        }
        let term_metadata = ontology.get(&subject_id);
        let has_predicate =
            term_metadata.map(|m| m.contains_key(&predicate_id)).unwrap_or(false);

        if translation_status == "NOT_TRANSLATED" {
            if !include_not_translated {
                remove.push(index);
            }
            // A row marked NOT_TRANSLATED for a predicate the ontology does not
            // have at all is dropped with a warning, not reported.
            if has_predicate {
                not_translated_rows.push((input_columns.clone(), row.clone()));
            }
        }

        if has_predicate {
            let ontology_value = term_metadata
                .and_then(|m| m.get(&predicate_id))
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or_default();
            if !is_equivalent_string(&ontology_value, &source_value) {
                let translation_value = row.get("translation_value").cloned().unwrap_or_default();
                // The row REPORTED as changed is the one before rewriting.
                source_changed_rows.push((input_columns.clone(), row.clone()));
                row.insert("source_value".to_string(), ontology_value);
                let new_status = if translation_value != "NOT_TRANSLATED" {
                    "CANDIDATE"
                } else {
                    "NOT_TRANSLATED"
                };
                if update_translation_status {
                    row.insert("translation_status".to_string(), new_status.to_string());
                }
            } else {
                // Equivalent but perhaps not identical: still take the ontology's
                // spelling, so profiles stay consistent with it.
                row.insert("source_value".to_string(), ontology_value);
            }
        }
    }

    for i in remove.into_iter().rev() {
        df.rows.remove(i);
    }

    // Everything the table does not cover yet becomes an untranslated row.
    let all_terms: Vec<String> = match terms {
        Some(t) => t.to_vec(),
        None => ontology.terms.clone(),
    };
    let mut added: Vec<BTreeMap<String, String>> = Vec::new();
    for term in &all_terms {
        let Some(term_metadata) = ontology.get(term) else { continue };
        for field in fields {
            if processed.get(term).is_some_and(|p| p.contains(field)) {
                continue;
            }
            let Some(values) = term_metadata.get(field) else { continue };
            for source_value in values {
                let mut row = BTreeMap::new();
                row.insert("source_language".to_string(), "en".to_string());
                row.insert("source_value".to_string(), source_value.clone());
                row.insert("subject_id".to_string(), term.clone());
                row.insert("predicate_id".to_string(), field.clone());
                row.insert("translation_language".to_string(), language_code.to_string());
                row.insert("translation_value".to_string(), String::new());
                row.insert("translation_status".to_string(), "NOT_TRANSLATED".to_string());
                // A freshly built row's column order is the order set below.
                const ADDED_COLUMNS: [&str; 7] = [
                    "source_language",
                    "source_value",
                    "subject_id",
                    "predicate_id",
                    "translation_language",
                    "translation_value",
                    "translation_status",
                ];
                let order: Vec<String> = ADDED_COLUMNS.iter().map(|s| s.to_string()).collect();
                added.push(row.clone());
                not_translated_rows.push((order, row));
            }
        }
    }

    if !added.is_empty() && include_not_translated {
        let added_table = Table {
            // A table built from these rows has exactly these columns, in
            // insertion order.
            columns: vec![
                "source_language".into(),
                "source_value".into(),
                "subject_id".into(),
                "predicate_id".into(),
                "translation_language".into(),
                "translation_value".into(),
                "translation_status".into(),
            ],
            rows: added,
        };
        df.concat(&added_table);
    }

    let profile = df;
    let source_changed = table_of(source_changed_rows);
    let not_translated = table_of(not_translated_rows);
    Ok(Prepared { profile, source_changed, not_translated })
}

/// Build a table from collected rows: columns in first-seen key order across the
/// rows. Empty input gives the default babelon header.
fn table_of(rows: Vec<(Vec<String>, BTreeMap<String, String>)>) -> Table {
    if rows.is_empty() {
        return default_table();
    }
    let mut columns: Vec<String> = Vec::new();
    for (order, r) in &rows {
        // A collected row carries EVERY column of its source table, missing ones
        // included, so a blank cell still fixes its column's position. Filtering
        // by presence would move `translation_value` to the end of HPO's Japanese
        // reports, whose first rows have none.
        for k in order.iter().chain(r.keys()) {
            if !columns.contains(k) {
                columns.push(k.clone());
            }
        }
    }
    Table { columns, rows: rows.into_iter().map(|(_, r)| r).collect() }
}

/// The columns a babelon table carries when there are no rows to derive them from.
fn default_table() -> Table {
    Table {
        columns: vec![
            "source_language".into(),
            "source_value".into(),
            "subject_id".into(),
            "predicate_id".into(),
            "translation_language".into(),
            "translation_value".into(),
            "translation_status".into(),
        ],
        rows: Vec::new(),
    }
}
