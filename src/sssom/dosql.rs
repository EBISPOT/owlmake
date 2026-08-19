//! `sssom dosql -Q "<SQL>" <FILE>…` — a SQL query over one or more SSSOM tables.
//!
//! This runs the query through **SQLite**, because that is what the tool being
//! reimplemented runs it through. `run_sql_query` (`sssom/io.py`) hands the query
//! to `pansql.sqldf`, whose engine is `create_engine('sqlite:///:memory:')` — so
//! every semantic that could differ is settled by using the same engine: type
//! affinity, NULL handling, `LIKE`'s case folding, collation, and the fallback by
//! which a DOUBLE-quoted word naming no column degrades to a string literal.
//!
//! That last one is not a curiosity. MONDO ships
//!
//! ```text
//! mappings/mondo.sssom.tsv: tmp/mondo-with-object-labels.sssom.tsv
//!         sssom dosql -Q "SELECT * FROM df WHERE predicate_id IN (\"skos:exactMatch\", \"skos:broadMatch\")" $< -o $@
//! ```
//!
//! and standard SQL reads `"skos:exactMatch"` as an IDENTIFIER — `sqlparser`'s
//! own `SQLiteDialect` returns `Identifier(quote_style: Some('"'))` for it. An
//! engine that implements SQL correctly therefore looks for a column of that
//! name, finds none, and rejects the query. Only SQLite, or a re-implementation
//! of its quirks, accepts it.
//!
//! ## Binding
//!
//! `run_sql_query` binds input *n* to `df{n}` AND to its stemmed filename
//! (`~/dir/my.sssom.tsv` → `my`), and its loop leaves the LAST input's frame
//! bound to the bare name `df` — the binding MONDO's query uses. The aliases are
//! SQLite VIEWs here, so a table is loaded once however many names reach it.
//!
//! ## Typing
//!
//! `to_sql` writes each pandas column with the dtype pandas inferred, so a column
//! of numbers is REAL and the documented `WHERE confidence>0.5` compares
//! numerically rather than lexically. Column typing mirrors that: a column whose
//! every present value parses as a number is REAL, everything else TEXT, and an
//! empty cell is NULL (pandas' NaN).

use anyhow::{Context, Result};

use super::{Mapping, MappingSet};

/// The table name `run_sql_query` binds a file to besides `df{n}`:
/// `re.sub("[.].*", "", Path(fn).stem).lower()`, i.e. the basename up to its
/// FIRST dot, lowercased — `mondo-with-object-labels.sssom.tsv` →
/// `mondo-with-object-labels`.
pub fn stem_binding(path: &std::path::Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match stem.split_once('.') {
        Some((head, _)) => head.to_ascii_lowercase(),
        None => stem.to_ascii_lowercase(),
    }
}

/// Run `query` over `tables` — `(binding, set)` in input order, the same set
/// appearing under each of its names. The result carries the LAST input's
/// `curie_map` and metadata, as `MappingSetDataFrame.with_converter` does.
#[cfg(not(target_arch = "wasm32"))]
pub fn run(query: &str, tables: &[(String, MappingSet)]) -> Result<MappingSet> {
    use rusqlite::types::ValueRef;
    use rusqlite::Connection;

    let conn = Connection::open_in_memory().context("`sssom dosql`: opening an in-memory SQLite")?;

    // One physical table per distinct set; every other binding is a view onto it.
    let mut loaded: Vec<(*const MappingSet, String)> = Vec::new();
    for (name, ms) in tables {
        let ptr = ms as *const MappingSet;
        if let Some((_, first)) = loaded.iter().find(|(p, _)| *p == ptr) {
            conn.execute_batch(&format!(
                "CREATE VIEW {} AS SELECT * FROM {};",
                quote_ident(name),
                quote_ident(first)
            ))
            .with_context(|| format!("`sssom dosql`: binding table `{name}`"))?;
            continue;
        }
        load_table(&conn, name, ms)
            .with_context(|| format!("`sssom dosql`: loading table `{name}`"))?;
        loaded.push((ptr, name.clone()));
    }

    // The result keeps the LAST input's curie_map and metadata.
    let mut out = tables.last().map(|(_, ms)| ms.clone()).unwrap_or_default();
    out.mappings.clear();
    {
        let mut stmt = conn.prepare(query).with_context(|| format!("`sssom dosql`: {query}"))?;
        let columns: Vec<String> = stmt.column_names().into_iter().map(str::to_string).collect();
        let mut rows = stmt.query([]).context("`sssom dosql`: running the query")?;
        while let Some(row) = rows.next()? {
            let mut m: Mapping = Default::default();
            for (i, col) in columns.iter().enumerate() {
                // NULL means the cell is ABSENT, not empty: a mapping carrying
                // the key would claim a value it does not have.
                let cell = match row.get_ref(i)? {
                    ValueRef::Null => continue,
                    ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    ValueRef::Integer(n) => n.to_string(),
                    ValueRef::Real(f) => format_real(f),
                    ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                };
                m.insert(col.clone(), cell);
            }
            out.mappings.push(m);
        }
    }
    out.recompute_columns();
    Ok(out)
}

/// A REAL back to text the way pandas' `to_csv` writes a float: an integral value
/// keeps its `.0` (`1.0`, not `1`).
#[cfg(not(target_arch = "wasm32"))]
fn format_real(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// `to_sql`: one column per slot, in the set's own column order, typed as pandas
/// would have inferred.
#[cfg(not(target_arch = "wasm32"))]
fn load_table(conn: &rusqlite::Connection, name: &str, ms: &MappingSet) -> Result<()> {
    let cols = &ms.columns;
    let decls: Vec<String> =
        cols.iter().map(|c| format!("{} {}", quote_ident(c), sqlite_type(ms, c))).collect();
    conn.execute_batch(&format!("CREATE TABLE {} ({});", quote_ident(name), decls.join(", ")))?;

    let placeholders = vec!["?"; cols.len()].join(", ");
    let sql = format!("INSERT INTO {} VALUES ({placeholders});", quote_ident(name));
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(&sql)?;
        for m in &ms.mappings {
            let values: Vec<Option<&str>> =
                cols.iter().map(|c| m.get(c).map(String::as_str).filter(|v| !v.is_empty())).collect();
            stmt.execute(rusqlite::params_from_iter(values))?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// The dtype pandas infers for a column: numeric when every value it holds is a
/// number, else object (text). An all-empty column is object too.
#[cfg(not(target_arch = "wasm32"))]
fn sqlite_type(ms: &MappingSet, column: &str) -> &'static str {
    let mut any = false;
    for m in &ms.mappings {
        if let Some(v) = m.get(column).map(String::as_str).filter(|v| !v.is_empty()) {
            if v.parse::<f64>().is_err() {
                return "TEXT";
            }
            any = true;
        }
    }
    if any {
        "REAL"
    } else {
        "TEXT"
    }
}

/// A SQLite identifier: double-quoted, inner quotes doubled. SSSOM slot names are
/// tame but a stemmed filename is not — `mondo-with-object-labels` is a binding.
#[cfg(not(target_arch = "wasm32"))]
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// wasm has no bundled SQLite (its C is native-only, as mimalloc's and zstd's
/// are), and a query owlmake cannot run must say so rather than hand back the
/// unfiltered table.
#[cfg(target_arch = "wasm32")]
pub fn run(_query: &str, _tables: &[(String, MappingSet)]) -> Result<MappingSet> {
    anyhow::bail!("`sssom dosql` needs SQLite, which is not built into the wasm target")
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn set(rows: &[(&str, &str)]) -> MappingSet {
        let mut ms = MappingSet::new();
        for (s, p) in rows {
            let mut m: BTreeMap<String, String> = BTreeMap::new();
            m.insert("subject_id".into(), (*s).into());
            m.insert("predicate_id".into(), (*p).into());
            ms.mappings.push(m);
        }
        ms.recompute_columns();
        ms
    }

    /// MONDO's own query. It selects anything at all only because SQLite falls
    /// back from identifier to string literal for a double-quoted word naming no
    /// column — the reason this runs on SQLite and not on a correct SQL engine.
    #[test]
    fn mondos_query_and_its_double_quotes() {
        let ms = set(&[
            ("MONDO:1", "skos:exactMatch"),
            ("MONDO:2", "skos:closeMatch"),
            ("MONDO:3", "skos:broadMatch"),
        ]);
        let out = run(
            r#"SELECT * FROM df WHERE predicate_id IN ("skos:exactMatch", "skos:broadMatch")"#,
            &[("df".to_string(), ms)],
        )
        .unwrap();
        let got: Vec<&str> = out.mappings.iter().map(|m| m["subject_id"].as_str()).collect();
        assert_eq!(got, vec!["MONDO:1", "MONDO:3"]);
    }

    /// The shape `sssom filter` generates.
    #[test]
    fn filters_shape_with_like_and_or() {
        let ms = set(&[("x:1", "p"), ("y:2", "p"), ("z:3", "p")]);
        let out = run(
            "SELECT * FROM df WHERE (subject_id LIKE 'x:%' OR subject_id LIKE 'y:%')",
            &[("df".to_string(), ms)],
        )
        .unwrap();
        assert_eq!(out.mappings.len(), 2);
    }

    /// A numeric column is REAL, so the documented `confidence>0.5` compares as a
    /// number: lexically `"0.5" > "0.45"` is false, numerically it is true.
    #[test]
    fn a_numeric_column_compares_numerically() {
        let mut ms = MappingSet::new();
        for (s, c) in [("a", "0.45"), ("b", "0.5"), ("c", "0.9")] {
            let mut m: BTreeMap<String, String> = BTreeMap::new();
            m.insert("subject_id".into(), s.into());
            m.insert("confidence".into(), c.into());
            ms.mappings.push(m);
        }
        ms.recompute_columns();
        let out = run(
            "SELECT * FROM df WHERE confidence>0.45 ORDER BY confidence",
            &[("df".to_string(), ms)],
        )
        .unwrap();
        let got: Vec<&str> = out.mappings.iter().map(|m| m["subject_id"].as_str()).collect();
        assert_eq!(got, vec!["b", "c"]);
    }

    /// Every binding reaches the same rows, and the join from `dosql`'s own
    /// docstring — which the hand-written subset this replaced had to refuse —
    /// now runs.
    #[test]
    fn bindings_and_a_join() {
        let a = set(&[("A:1", "p")]);
        let b = set(&[("A:1", "q")]);
        let tables = vec![
            ("df1".to_string(), a.clone()),
            ("first".to_string(), a),
            ("df2".to_string(), b.clone()),
            ("second".to_string(), b.clone()),
            ("df".to_string(), b),
        ];
        for t in ["df1", "first", "df2", "second", "df"] {
            let out = run(&format!("SELECT * FROM {t}"), &tables).unwrap();
            assert_eq!(out.mappings.len(), 1, "{t}");
        }
        let out = run(
            "SELECT df1.* FROM df1 INNER JOIN df2 ON df1.subject_id = df2.subject_id",
            &tables,
        )
        .unwrap();
        assert_eq!(out.mappings.len(), 1);
        assert_eq!(out.mappings[0]["predicate_id"], "p");
    }

    /// A query SQLite refuses is an error, not an unfiltered table.
    #[test]
    fn a_bad_query_is_an_error() {
        let ms = set(&[("a", "p")]);
        assert!(run("SELECT * FROM nosuchtable", &[("df".to_string(), ms)]).is_err());
    }

    #[test]
    fn table_binding_is_the_stem_up_to_the_first_dot() {
        assert_eq!(
            stem_binding(std::path::Path::new("/tmp/mondo-with-object-labels.sssom.tsv")),
            "mondo-with-object-labels"
        );
    }
}
