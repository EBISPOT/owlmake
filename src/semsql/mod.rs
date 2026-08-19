//! The ontology SQL database: `semsql make <name>.db`.
//!
//! The database is a queryable projection of one release artefact. Two tables
//! carry the content and every view is derived from them:
//!
//! * `statements` — the ontology's RDF triples, CURIE-shortened, one row per
//!   triple, grouped into *stanzas* (see [`rdftab`]);
//! * `entailed_edge` — the reasoned relation graph: every entailed subclass and
//!   existential edge between named terms, as a direct `subject predicate
//!   object` row rather than an OWL restriction.
//!
//! The build is: reduce the artefact to the axioms the relation graph needs,
//! compute the graph, create the schema, load the prefixes, load the triples,
//! load the graph, index. Everything is owlmake's own — the ontology
//! operations, the reasoner, and SQLite itself, which is statically linked.
//!
//! # What is not reproducible here
//!
//! The row ORDER of `entailed_edge` is the order the graph was computed in, and
//! a SQLite file records insertion order in its page layout. owlmake computes
//! the graph deterministically, so its own database is byte-stable run to run;
//! a database built by other tooling is not comparable byte for byte, only table
//! by table.

pub mod rdftab;
pub mod relgraph;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::Connection;

/// The table and view definitions. Every column and view name in here is part of
/// the database's contract with its consumers, so the schema is stored verbatim
/// rather than generated.
const SCHEMA_SQL: &str = include_str!("data/semsql.sql");
/// The indexes, applied after the content is loaded so the loads are not
/// re-indexed per row.
const INDEXES_SQL: &str = include_str!("data/all-indexes.sql");
/// The CURIE prefix map the triples are shortened against, and the `prefix`
/// table's content.
const PREFIXES_CSV: &str = include_str!("data/prefixes.csv");
/// Object properties left out of the relation graph: relations whose entailed
/// closure is large and of no use to a term browser (`RO:0002410` causally
/// related to, and its kin).
const EXCLUDE_TERMS: &str = include_str!("data/exclude-terms.txt");

/// `semsql make <name>.db`: build the database for the artefact of the same
/// stem. `<name>.owl` must sit beside it.
pub fn make(db: &Path) -> Result<()> {
    let stem = db
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|_| db.extension().and_then(|e| e.to_str()) == Some("db"))
        .ok_or_else(|| anyhow::anyhow!("`semsql make` needs a `<name>.db` target, got {}", db.display()))?
        .to_string();
    let dir = db.parent().unwrap_or(Path::new(".")).to_path_buf();
    let owl = dir.join(format!("{stem}.owl"));
    if !owl.exists() {
        bail!("`semsql make {}` needs {}, which does not exist", db.display(), owl.display());
    }

    let prefixes = prefix_rows();

    // 1. The reduced ontology the relation graph is computed over: no
    //    equivalences, disjointness, annotations, assertions or type axioms, and
    //    none of the excluded relations.
    let min_owl = dir.join(format!("{stem}-min.owl"));
    write_min_ontology(&owl, &min_owl)?;

    // 2. The relation graph itself.
    let rg_tsv = dir.join(format!("{stem}-relation-graph.tsv"));
    relgraph::write_tsv(&min_owl, &prefixes, &rg_tsv)?;

    // 3. The database.
    let tmp = dir.join(format!("{stem}.db.tmp"));
    let _ = std::fs::remove_file(&tmp);
    build_db(&tmp, &owl, &rg_tsv, &prefixes)?;

    // 4. The relation graph is kept beside the database, compressed.
    gzip_file(&rg_tsv)?;
    let _ = std::fs::remove_file(&rg_tsv);
    let _ = std::fs::remove_file(&min_owl);

    std::fs::rename(&tmp, db)
        .with_context(|| format!("renaming {} to {}", tmp.display(), db.display()))?;
    crate::status!("semsql: wrote {}", db.display());
    Ok(())
}

/// The `prefix` table's rows, in file order — the CURIE map the whole database
/// is written in. The first row is the CSV header, which is a row of the table
/// like any other.
pub(crate) fn prefix_rows() -> Vec<(String, String)> {
    PREFIXES_CSV
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_once(',').map(|(p, b)| (p.to_string(), b.to_string())))
        .collect()
}

/// Reduce the artefact to the axioms the relation graph is computed from: the
/// subsumptions and existential restrictions, with the excluded relations out.
fn write_min_ontology(src: &Path, dst: &Path) -> Result<()> {
    let model = crate::io::load(src)?;
    let axioms: Vec<String> = ["equivalent", "disjoint", "annotation", "abox", "type"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let model = crate::cmd::remove::remove_with(
        model,
        &[],
        &[],
        &[],
        &axioms,
        &[],
        &crate::cmd::remove::TermOptions::default(),
    )?;
    let opts = crate::cmd::remove::TermOptions {
        exclude_term: EXCLUDE_TERMS
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect(),
        ..Default::default()
    };
    let mut model = crate::cmd::filter::filter_with(model, &[], &[], &[], &[], &[], &opts)?;
    crate::io::save(&mut model, dst)?;
    Ok(())
}

/// Assemble the database: schema, prefixes, triples, relation graph, indexes.
fn build_db(
    db: &Path,
    owl: &Path,
    rg_tsv: &Path,
    prefixes: &[(String, String)],
) -> Result<()> {
    let conn = Connection::open(db)
        .with_context(|| format!("creating {}", db.display()))?;
    conn.execute_batch(SCHEMA_SQL).context("applying the database schema")?;

    {
        let tx = conn.unchecked_transaction()?;
        {
            let mut ins = tx.prepare("INSERT INTO prefix VALUES (?1, ?2)")?;
            for (p, b) in prefixes {
                ins.execute((p, b))?;
            }
        }
        tx.commit()?;
    }

    rdftab::load(&conn, owl, prefixes)?;

    {
        let tx = conn.unchecked_transaction()?;
        {
            let mut ins = tx.prepare("INSERT INTO entailed_edge VALUES (?1, ?2, ?3)")?;
            let tsv = std::fs::read_to_string(rg_tsv)
                .with_context(|| format!("reading {}", rg_tsv.display()))?;
            for line in tsv.lines() {
                if line.is_empty() {
                    continue;
                }
                let mut f = line.split('\t');
                let (s, p, o) = (f.next().unwrap_or(""), f.next().unwrap_or(""), f.next().unwrap_or(""));
                ins.execute((s, p, o))?;
            }
        }
        tx.commit()?;
    }

    conn.execute_batch(INDEXES_SQL).context("creating the indexes")?;
    // The named graph a triple came from. Nothing populates it yet; the column
    // is part of the `statements` contract, so it is always present.
    conn.execute_batch("ALTER TABLE statements ADD COLUMN graph TEXT;")
        .context("adding the statements.graph column")?;
    conn.close().map_err(|(_, e)| e).context("closing the database")?;
    Ok(())
}

/// gzip a file in place, leaving `<path>.gz`.
fn gzip_file(path: &Path) -> Result<PathBuf> {
    use std::io::Write;
    let out = PathBuf::from(format!("{}.gz", path.display()));
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let f = std::fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
    // A gzip header carries a modification time, and writing the clock into it
    // would make two builds of the same database differ in their first bytes.
    let header = flate2::GzBuilder::new().mtime(0);
    let mut enc = header.write(f, flate2::Compression::default());
    enc.write_all(&data)?;
    enc.finish()?;
    Ok(out)
}
