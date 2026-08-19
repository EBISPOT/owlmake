//! A small `grep` command line over ripgrep's pure-Rust libraries
//! (`grep-regex` / `grep-searcher` / `grep-printer`).
//!
//! This is deliberately *not* uutils' `uu_grep`: that crate links the
//! Oniguruma C library, which would forfeit the single-static-binary,
//! no-C-toolchain property. ripgrep's libraries are pure Rust and the `regex`
//! engine is already in this crate's dependency tree.
//!
//! The patterns use Rust regular-expression syntax (ERE-like). The `-E`/`-G`/`-P`
//! flavour flags are accepted for compatibility but do not switch engines, and
//! `-F`/`--fixed-strings` matches literally. No build recipe owlmake has had to
//! run invokes `grep` — `sed` and `comm` carry that traffic — so this surface is
//! here to keep a recipe that does from being a wall, and for ad-hoc
//! `owlmake grep`.

use std::io::{self, BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use grep::printer::{StandardBuilder, SummaryBuilder, SummaryKind};
use grep::regex::{RegexMatcher, RegexMatcherBuilder};
use grep::searcher::{BinaryDetection, SearcherBuilder};

/// Entry point. `args` are the arguments after the `grep` word. Returns the
/// grep exit code: 0 if any line matched, 1 if none, 2 on error.
pub fn main(args: &[String]) -> i32 {
    let opts = match Options::parse(args) {
        Ok(Some(o)) => o,
        // A help/usage request already printed; succeed.
        Ok(None) => return 0,
        Err(e) => {
            eprintln!("grep: {e}");
            return 2;
        }
    };
    match run(&opts) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("grep: {e}");
            2
        }
    }
}

#[derive(Default)]
struct Options {
    patterns: Vec<String>,
    files: Vec<PathBuf>,
    ignore_case: bool,
    invert: bool,
    line_number: bool,
    count: bool,
    word: bool,
    fixed: bool,
    whole_line: bool,
    files_with_matches: bool,
    files_without_match: bool,
    quiet: bool,
    only_matching: bool,
    recursive: bool,
    no_filename: bool,
    with_filename: bool,
}

impl Options {
    /// Parse a grep-style argv. Returns `Ok(None)` when a usage/help message was
    /// printed and the caller should simply exit successfully.
    fn parse(args: &[String]) -> Result<Option<Options>> {
        let mut o = Options::default();
        let mut have_pattern_flag = false; // -e / -f seen
        let mut operands: Vec<String> = Vec::new();
        let mut only_operands = false; // after `--`
        let mut it = args.iter().cloned().peekable();

        while let Some(arg) = it.next() {
            if only_operands {
                operands.push(arg);
                continue;
            }
            if arg == "--" {
                only_operands = true;
            } else if let Some(long) = arg.strip_prefix("--") {
                let (name, val) = match long.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_string())),
                    None => (long, None),
                };
                match name {
                    "help" => {
                        print_usage();
                        return Ok(None);
                    }
                    "ignore-case" => o.ignore_case = true,
                    "invert-match" => o.invert = true,
                    "line-number" => o.line_number = true,
                    "count" => o.count = true,
                    "word-regexp" => o.word = true,
                    "fixed-strings" => o.fixed = true,
                    "line-regexp" => o.whole_line = true,
                    "files-with-matches" => o.files_with_matches = true,
                    "files-without-match" => o.files_without_match = true,
                    "quiet" | "silent" => o.quiet = true,
                    "only-matching" => o.only_matching = true,
                    "recursive" | "dereference-recursive" => o.recursive = true,
                    "no-filename" => o.no_filename = true,
                    "with-filename" => o.with_filename = true,
                    // Regex-flavour / output flags accepted for compatibility.
                    "extended-regexp" | "basic-regexp" | "perl-regexp" => {}
                    "color" | "colour" => {} // always rendered without colour
                    "regexp" => {
                        let v = val.clone().or_else(|| it.next()).context("--regexp needs a value")?;
                        o.patterns.push(v);
                        have_pattern_flag = true;
                    }
                    "file" => {
                        let v = val.clone().or_else(|| it.next()).context("--file needs a value")?;
                        read_pattern_file(&v, &mut o.patterns)?;
                        have_pattern_flag = true;
                    }
                    other => bail!("unrecognized option '--{other}'"),
                }
            } else if arg.starts_with('-') && arg.len() > 1 {
                // A bundle of short flags, e.g. `-in`. `-e`/`-f` consume the rest
                // of the token (or the next argument) as their value.
                let chars: Vec<char> = arg[1..].chars().collect();
                let mut idx = 0;
                while idx < chars.len() {
                    let c = chars[idx];
                    match c {
                        'i' => o.ignore_case = true,
                        'v' => o.invert = true,
                        'n' => o.line_number = true,
                        'c' => o.count = true,
                        'w' => o.word = true,
                        'F' => o.fixed = true,
                        'x' => o.whole_line = true,
                        'l' => o.files_with_matches = true,
                        'L' => o.files_without_match = true,
                        'q' => o.quiet = true,
                        'o' => o.only_matching = true,
                        'r' | 'R' => o.recursive = true,
                        'h' => o.no_filename = true,
                        'H' => o.with_filename = true,
                        'E' | 'G' | 'P' => {} // flavour flags: no-op
                        'e' | 'f' => {
                            let rest: String = chars[idx + 1..].iter().collect();
                            let v = if rest.is_empty() {
                                it.next().with_context(|| format!("-{c} needs a value"))?
                            } else {
                                rest
                            };
                            if c == 'e' {
                                o.patterns.push(v);
                            } else {
                                read_pattern_file(&v, &mut o.patterns)?;
                            }
                            have_pattern_flag = true;
                            break; // consumed the remainder of this token
                        }
                        other => bail!("invalid option -- '{other}'"),
                    }
                    idx += 1;
                }
            } else {
                operands.push(arg);
            }
        }

        // Without an explicit -e/-f/--regexp, the first operand is the pattern.
        if !have_pattern_flag {
            if operands.is_empty() {
                bail!("no pattern given");
            }
            o.patterns.push(operands.remove(0));
        }
        if o.patterns.is_empty() {
            bail!("no pattern given");
        }
        o.files = operands.into_iter().map(PathBuf::from).collect();
        Ok(Some(o))
    }

    /// Whether to prefix output lines with the file name.
    fn show_filename(&self, n_inputs: usize) -> bool {
        if self.no_filename {
            false
        } else {
            self.with_filename || n_inputs > 1 || self.recursive
        }
    }

    /// The summary printer mode, if any (`-q`/`-l`/`-L`/`-c`), else `None` for
    /// the normal line-oriented printer.
    fn summary_kind(&self) -> Option<SummaryKind> {
        if self.quiet {
            Some(SummaryKind::QuietWithMatch)
        } else if self.files_with_matches {
            Some(SummaryKind::PathWithMatch)
        } else if self.files_without_match {
            Some(SummaryKind::PathWithoutMatch)
        } else if self.count {
            Some(SummaryKind::Count)
        } else {
            None
        }
    }

    /// Expand the operand paths, walking directories when `-r` is set. An empty
    /// result means: read standard input.
    fn collect_inputs(&self) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for f in &self.files {
            if self.recursive && f.is_dir() {
                walk(f, &mut out)?;
            } else {
                out.push(f.clone());
            }
        }
        Ok(out)
    }
}

/// Read newline-separated patterns from a file (`-f`).
fn read_pattern_file(path: &str, out: &mut Vec<String>) -> Result<()> {
    let f = std::fs::File::open(path).with_context(|| format!("opening pattern file {path}"))?;
    for line in io::BufReader::new(f).lines() {
        out.push(line?);
    }
    Ok(())
}

/// Recursively collect regular files under `dir`.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn build_matcher(o: &Options) -> Result<RegexMatcher> {
    RegexMatcherBuilder::new()
        .case_insensitive(o.ignore_case)
        .word(o.word)
        .fixed_strings(o.fixed)
        .whole_line(o.whole_line)
        .line_terminator(Some(b'\n'))
        .multi_line(false)
        .build_many(&o.patterns)
        .map_err(|e| anyhow!("bad pattern: {e}"))
}

fn run(o: &Options) -> Result<bool> {
    let matcher = build_matcher(o)?;
    let mut searcher = SearcherBuilder::new()
        .line_number(o.line_number)
        .invert_match(o.invert)
        .binary_detection(BinaryDetection::quit(0))
        .multi_line(false)
        .build();

    let inputs = o.collect_inputs()?;
    let show_filename = o.show_filename(inputs.len());

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut found = false;

    // `has_match` lives on the per-search *sink*, and the search consumes a
    // `Sink` — but grep-searcher implements `Sink for &mut S`, so we pass a
    // reborrow and can still read the sink's match flag afterwards.
    if let Some(kind) = o.summary_kind() {
        // `-l`/`-L` are *about* file names, so always print them; `-c` follows
        // the normal rule (prefix the count with the file name only when there
        // is more than one input or `-H` was given).
        let summary_path = match kind {
            SummaryKind::PathWithMatch | SummaryKind::PathWithoutMatch => true,
            _ => show_filename,
        };
        let mut printer = SummaryBuilder::new()
            .kind(kind)
            .path(summary_path)
            .build_no_color(&mut out);
        if inputs.is_empty() {
            let mut sink = printer.sink(&matcher);
            searcher
                .search_reader(&matcher, io::stdin().lock(), &mut sink)
                .context("searching standard input")?;
            found |= sink.has_match();
        } else {
            for path in &inputs {
                let mut sink = printer.sink_with_path(&matcher, path);
                searcher
                    .search_path(&matcher, path, &mut sink)
                    .with_context(|| format!("searching {}", path.display()))?;
                found |= sink.has_match();
            }
        }
    } else {
        let mut printer = StandardBuilder::new()
            .path(show_filename)
            .heading(false)
            .only_matching(o.only_matching)
            .build_no_color(&mut out);
        if inputs.is_empty() {
            let mut sink = printer.sink(&matcher);
            searcher
                .search_reader(&matcher, io::stdin().lock(), &mut sink)
                .context("searching standard input")?;
            found |= sink.has_match();
        } else {
            for path in &inputs {
                let mut sink = printer.sink_with_path(&matcher, path);
                searcher
                    .search_path(&matcher, path, &mut sink)
                    .with_context(|| format!("searching {}", path.display()))?;
                found |= sink.has_match();
            }
        }
    }

    out.flush().ok();
    Ok(found)
}

fn print_usage() {
    println!(
        "Usage: grep [OPTION]... PATTERN [FILE]...\n\
         Search for PATTERN in each FILE (or standard input).\n\n\
         Options: -i -v -n -c -w -F -x -l -L -q -o -r/-R -h -H -e PAT -f FILE\n\
         Patterns use Rust regular-expression syntax."
    );
}
