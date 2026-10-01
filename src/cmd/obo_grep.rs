//! `obo-grep` — keep the stanzas of an OBO file that match a regex.
//!
//! An OBO file is a header followed by stanzas separated by blank lines, and
//! editors filter it at that grain: *the Term stanzas*, *everything that is not
//! obsolete*, *the stanzas with an id in this list*. The input is read as text,
//! so a file that is not yet valid OBO — a reasoner's raw output, a stanza
//! stream half-way through a pipeline — filters as readily as a finished one,
//! and what is kept is kept byte for byte.
//!
//! Records are cut at `\n\n`, and each keeps its separator. The first record
//! that does not open with `[` is the header: it is written through unchanged
//! (unless `--noheader` or `--count`) and never matched. Every other record is
//! tested against the regex as a whole, so a pattern may span a stanza's lines,
//! and is written if it matches — or, under `--neg`, if it does not. `--count`
//! writes the number of records kept instead of the records. Input files are
//! read in the order given, with `-` for standard input.
//!
//! `obo-grep.pl` is this command's older name, and a recipe that calls the
//! script by that name, bare or by path, reaches this implementation.

use std::io::{Read, Write};

use anyhow::{Context, Result};
use regex::Regex;

struct Args {
    regex: String,
    negate: bool,
    count: bool,
    noheader: bool,
    files: Vec<String>,
}

fn usage() -> String {
    format!(
        "obo-grep (owlmake {}) — keep the stanzas of an OBO file that match a regex\n\n\
         Usage: obo-grep [--noheader] [--neg] [--count] [-r REGEX | --regexp-file FILE] OBO-FILE...\n\n\
         Options:\n  \
         -r, --regexp REGEX   keep the stanzas REGEX matches\n      \
         --regexp-file FILE   keep the stanzas whose id is one of FILE's lines\n  \
         -v, --neg            keep the stanzas that do NOT match\n  \
         -c, --count          print how many stanzas were kept instead of them\n      \
         --noheader           do not write the header through\n\n\
         A file named `-` is standard input.",
        env!("CARGO_PKG_VERSION")
    )
}

fn parse(args: &[String]) -> Result<Option<Args>> {
    let mut out = Args { regex: String::new(), negate: false, count: false, noheader: false, files: Vec::new() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(None);
            }
            "-r" | "--regexp" => {
                out.regex = it.next().context("-r needs a regex")?.clone();
            }
            "--regexp-file" => {
                let f = it.next().context("--regexp-file needs a file")?;
                let text = std::fs::read_to_string(f).with_context(|| format!("reading {f}"))?;
                let ids: Vec<&str> = text.lines().collect();
                out.regex = format!("id: ({})\n", ids.join("|"));
            }
            "-c" | "--count" => out.count = true,
            "--noheader" => out.noheader = true,
            "-v" | "--neg" => out.negate = true,
            // `--idfile` is accepted for the older script's sake and names
            // nothing this command reads.
            "--idfile" => {
                it.next();
            }
            t => out.files.push(t.to_string()),
        }
    }
    Ok(Some(out))
}

/// The records of `text`: each run up to and including a `\n\n`, and whatever
/// follows the last one.
fn records(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("\n\n") {
        let (rec, tail) = rest.split_at(i + 2);
        out.push(rec);
        rest = tail;
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

pub fn run(args: &[String], stdin: &mut dyn Read, stdout: &mut dyn Write) -> Result<()> {
    let Some(a) = parse(args)? else { return Ok(()) };
    let re = Regex::new(&a.regex).with_context(|| format!("regex `{}`", a.regex))?;
    let mut kept = 0usize;
    for f in &a.files {
        let text = if f == "-" {
            let mut s = String::new();
            stdin.read_to_string(&mut s).context("reading standard input")?;
            s
        } else {
            std::fs::read_to_string(f).with_context(|| format!("cannot open {f}"))?
        };
        let mut header_seen = false;
        for rec in records(&text) {
            if !header_seen && !rec.starts_with('[') {
                if !a.noheader && !a.count {
                    stdout.write_all(rec.as_bytes())?;
                }
                header_seen = true;
                continue;
            }
            if re.is_match(rec) != a.negate {
                kept += 1;
                if !a.count {
                    stdout.write_all(rec.as_bytes())?;
                }
            }
        }
    }
    if a.count {
        writeln!(stdout, "{kept}")?;
    }
    Ok(())
}

pub fn main(args: &[String]) -> i32 {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    match run(args, &mut std::io::stdin().lock(), &mut out).and_then(|()| Ok(out.flush()?)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("obo-grep: {e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBO: &str = "format-version: 1.2\nontology: x\n\n[Term]\nid: X:1\nname: one\nis_a: X:2\n\n[Term]\nid: X:2\nname: two\n\n[Typedef]\nid: part_of\n";

    fn grep(args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut out = Vec::new();
        run(&args, &mut OBO.as_bytes(), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn keeps_the_header_and_the_matching_stanzas() {
        assert_eq!(
            grep(&["-r", "Term", "-"]),
            "format-version: 1.2\nontology: x\n\n[Term]\nid: X:1\nname: one\nis_a: X:2\n\n[Term]\nid: X:2\nname: two\n\n"
        );
    }

    #[test]
    fn negates_counts_and_drops_the_header() {
        assert_eq!(grep(&["--neg", "-r", "is_a:", "--noheader", "-"]), "[Term]\nid: X:2\nname: two\n\n[Typedef]\nid: part_of\n");
        assert_eq!(grep(&["-c", "-r", "id: X:", "-"]), "2\n");
    }

    /// A pattern is matched against the whole stanza, so an alternation with
    /// `|` in it is one pattern and may reach across lines.
    #[test]
    fn a_pattern_spans_the_stanza() {
        assert_eq!(grep(&["--noheader", "-r", "(name: two|nothing)", "-"]), "[Term]\nid: X:2\nname: two\n\n");
        assert_eq!(grep(&["--noheader", "-r", "one\nis_a", "-"]), "[Term]\nid: X:1\nname: one\nis_a: X:2\n\n");
    }
}
