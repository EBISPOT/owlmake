//! `text-tagger` — build the flat Aho-Corasick term DB and run the line-oriented
//! tagger over it.
//!
//! `om text-tagger build` reads an `extract-strings` TSV (stdin or `--input`) and
//! writes the DB in the flat layout [`crate::tag::ac`] documents. That layout
//! carries no magic or version field, so the byte sequence itself is the
//! interchange contract in both directions: a DB written here loads in any
//! consumer, and a published DB loads here.
//!
//! `om text-tagger stream` loads a DB (raw `.bin` or the published `.bin.gz`) and
//! tags stdin line by line: one non-empty text line in → one JSON line out,
//! flushed per line so the process can be driven as a long-lived co-process.
//! A consumer such as OLS runs `om text-tagger stream <db> [--delimiters …]`,
//! writes text to its stdin and reads one JSON response back per line.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crate::tag::{self, AnnotateResponse};

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Sub,
}

#[derive(clap::Subcommand)]
pub enum Sub {
    /// Build the tagger DB from an `extract-strings` TSV (stdin or `--input`).
    Build(BuildArgs),
    /// Tag text line-by-line from stdin against a DB (raw `.bin` or `.bin.gz`).
    /// (OLS calls this mode `cli`.)
    Stream(StreamArgs),
}

#[derive(ClapArgs)]
pub struct BuildArgs {
    /// Output DB path.
    #[arg(short, long, default_value = "text_tagger_db.bin")]
    pub output: PathBuf,
    /// Input TSV (defaults to stdin).
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Minimum match-key length in bytes.
    #[arg(long = "min-len", default_value_t = tag::DEFAULT_MIN_LEN)]
    pub min_len: usize,
}

#[derive(ClapArgs)]
pub struct StreamArgs {
    /// DB path (raw `.bin` or gzip `.bin.gz`); defaults to `text_tagger_db.bin`.
    ///
    /// There is deliberately no `$text_tagger_db_PATH` fallback: an environment
    /// variable naming an INPUT FILE lets ambient state decide what a step reads,
    /// which is the one thing a plan exists to pin down.
    pub db: Option<PathBuf>,
    /// Word-boundary characters; a match is kept only if bounded by one (or text edges).
    #[arg(long)]
    pub delimiters: Option<String>,
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> Result<Option<crate::model::Model>> {
    match &args.cmd {
        Sub::Build(a) => build(a)?,
        Sub::Stream(a) => stream(a)?,
    }
    Ok(piped)
}

fn build(args: &BuildArgs) -> Result<()> {
    let ac = match &args.input {
        Some(p) => {
            let f = File::open(p).with_context(|| format!("opening {}", p.display()))?;
            tag::build_from_tsv(BufReader::new(f), args.min_len)?
        }
        None => {
            let stdin = std::io::stdin();
            tag::build_from_tsv(stdin.lock(), args.min_len)?
        }
    };
    let mut out = BufWriter::new(
        File::create(&args.output).with_context(|| format!("creating {}", args.output.display()))?,
    );
    out.write_all(&ac.buf)?;
    out.flush()?;
    status!(
        "text-tagger: wrote {} bytes to {}",
        ac.buf.len(),
        args.output.display()
    );
    Ok(())
}

fn stream(args: &StreamArgs) -> Result<()> {
    let db_path = args
        .db
        .clone()
        .unwrap_or_else(|| PathBuf::from("text_tagger_db.bin"));

    let raw = std::fs::read(&db_path).with_context(|| format!("reading {}", db_path.display()))?;
    let buf = maybe_gunzip(raw)?;
    status!("text-tagger: loaded {} bytes from {}", buf.len(), db_path.display());

    let delimiters: Option<Vec<u8>> = args.delimiters.as_ref().map(|s| s.bytes().collect());
    let ac = tag::ac::NerAc::from_buf(buf)
        .with_context(|| format!("loading tagger DB {}", db_path.display()))?;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let text = line?;
        if text.is_empty() {
            continue;
        }
        let entities = tag::annotate_text(&ac, &text, delimiters.as_deref());
        let resp = AnnotateResponse { entities };
        serde_json::to_writer(&mut out, &resp)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    Ok(())
}

/// Gunzip if the bytes carry the gzip magic (so the published `text_tagger_db.bin.gz`
/// loads directly); otherwise pass through.
fn maybe_gunzip(bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_end(&mut out)
            .context("gunzipping tagger DB")?;
        Ok(out)
    } else {
        Ok(bytes)
    }
}
