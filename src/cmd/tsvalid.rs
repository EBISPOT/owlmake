//! `tsvalid <FILE>` — lint a TSV file, as the `tsvalid` tool (0.0.5) does.
//!
//! The standard build runs it over a mapping set before validating it
//! (`tsvalid x.sssom.tsv --comment "#"`) and over pattern tables
//! (`validate-tsv`). Nothing else on a machine provides it, so owlmake answers to
//! the name.
//!
//! The checks, by the code a `--skip` names (a code or a pattern matching one):
//!
//! | code | what |
//! |------|------|
//! | E0   | the file is not in the `--encoding` |
//! | E1   | a carriage return in a line |
//! | E2   | a cell starts with whitespace |
//! | E3   | a cell ends with whitespace |
//! | E4   | a line's tab count is not the header's |
//! | E5   | an empty line (other than the last) |
//! | E6   | a header cell is empty |
//! | E8   | a column with no value in it at all |
//! | E9   | the file does not end in a newline |
//! | E10  | two header cells are the same |
//! | W1   | a character outside printable ASCII |
//!
//! A line starting with the `--comment` string is not checked. Findings go to
//! standard error, one a line, as `<file>:<line>:<column>: <code>: <message>`
//! behind the `ERROR:root:` or `WARNING:root:` a Python logger puts there; only
//! the `--summary` goes to standard output. Findings do not fail the command
//! unless `--fail` is given, which stops at the first. Both are as the tool has
//! them, and the second is why ODK's `validate-tsv`, which tests whether
//! standard output is empty, passes whatever the file holds.
//!
//! Where the tool's bookkeeping shows in its output it is kept: the empty-line
//! check runs one line late and leaves the line number behind it, so a finding
//! made after the last line may name the line before; and an empty column is
//! always reported at column 0.

use std::path::Path;

struct Check {
    id: &'static str,
    code: &'static str,
    name: &'static str,
}

const ENCODING: Check = Check { id: "fileEncodingCheck", code: "E0", name: "Unexpected File Encoding" };
const LINE_BREAK: Check = Check { id: "unexpectedLineBreakEncoding", code: "E1", name: "Line break encoding" };
const LEADING: Check = Check { id: "leadingWhitespaceCheck", code: "E2", name: "Redundant leading whitespace" };
const TRAILING: Check = Check { id: "trailingWhitespaceCheck", code: "E3", name: "Redundant trailing whitespace" };
const TABS: Check = Check { id: "numberOfTabsCheck", code: "E4", name: "Wrong number of tabs" };
const EMPTY_LINE: Check = Check { id: "emptyLine", code: "E5", name: "Empty line" };
const HEADER_VALUE: Check = Check { id: "missingValueInHeader", code: "E6", name: "Missing value in header" };
const EMPTY_COLUMN: Check = Check { id: "emptyColumn", code: "E8", name: "Empty column" };
const LAST_ROW: Check = Check { id: "emptyLastRow", code: "E9", name: "Empty last row" };
const HEADER_DUPLICATE: Check = Check { id: "duplicateValueInHeaderRow", code: "E10", name: "Duplicate value in header" };
const NON_ASCII: Check = Check { id: "nonAsciiCharacterCheck", code: "W1", name: "Non ASCII character in cell." };

fn message(check: &Check, line: usize, column: usize) -> String {
    match check.code {
        "E0" => format!("Invalid file encoding {line}."),
        "E1" => format!("Invalid line break in line {line}."),
        "E2" => format!("Redundant leading whitespace in column {column} at line number {line}."),
        "E3" => format!("Redundant trailing whitespace in column {column} at line number {line}."),
        "E4" => format!("Number of tabs in line {line} does not match tabs in header."),
        "E5" => format!("Empty line {line}."),
        "E6" => format!("Header row has missing values, line {line}."),
        "E8" => format!("TSV file contains empty column at column index {column}."),
        "E9" => "Last row in file should be empty.".to_string(),
        "E10" => format!("Header row has duplicate values, line {line}."),
        _ => format!("Non ASCII character in column {column} at line number {line}."),
    }
}

struct Run<'a> {
    file: &'a str,
    encoding: &'a str,
    skip: Vec<regex::Regex>,
    fail: bool,
    line: usize,
    column: usize,
    /// Findings per check, in the order each was first made.
    summary: Vec<(&'static Check, usize)>,
    exception: Option<&'static str>,
    /// The first line that is not a comment: the header.
    first_row: Option<usize>,
    /// The header's tab count, which every line is held to.
    tab_count: Option<usize>,
    /// Per column, how often a value was seen in it. The tool counts a cell once
    /// for each of its three cell checks, which only shows in what `--fail`
    /// prints; an empty column is one that stays at zero.
    values_in_column: Vec<usize>,
    /// Set once the last line has been read.
    last_line: Option<usize>,
}

/// A finding under `--fail`: the run stops there.
struct Failed;

impl Run<'_> {
    fn check(&mut self, check: &'static Check, failed: bool) -> Result<(), Failed> {
        if !failed || self.skip.iter().any(|s| s.is_match(check.code)) {
            return Ok(());
        }
        let mut text = message(check, self.line, self.column);
        if let Some(e) = self.exception {
            text = format!("{text} Exception: \"{e}\".");
        }
        let level = if check.code.starts_with('E') { "ERROR" } else { "WARNING" };
        eprintln!("{level}:root:{}:{}:{}: {}: {text}", self.file, self.line, self.column, check.code);
        match self.summary.iter_mut().find(|(c, _)| c.id == check.id) {
            Some((_, n)) => *n += 1,
            None => self.summary.push((check, 1)),
        }
        if self.fail {
            eprintln!("ERROR:root:tsvalid: Validation failed: {}", self.state(check));
            return Err(Failed);
        }
        Ok(())
    }
}

impl Run<'_> {
    /// Everything the run knows at the finding that stopped it, which is what
    /// the tool prints under `--fail`: its working state, as Python writes a
    /// dictionary.
    fn state(&self, check: &Check) -> String {
        let quoted = |s: &str| {
            if s.contains('\'') && !s.contains('"') {
                format!("\"{}\"", s.replace('\\', "\\\\"))
            } else {
                format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
            }
        };
        let counts: Vec<String> =
            self.values_in_column.iter().enumerate().map(|(i, n)| format!("'{i}': {n}")).collect();
        let mut entries = vec![
            format!("'line_number': {}", self.line),
            format!("'column': {}", self.column),
            format!("'filename': {}", quoted(self.file)),
            format!("'encoding': {}", quoted(self.encoding)),
            format!("'column_values_empty': {{{}}}", counts.join(", ")),
        ];
        // In the order the tool comes to know each.
        if let Some(e) = self.exception {
            entries.push(format!("'exception': {}", quoted(e)));
        }
        if let Some(n) = self.first_row {
            entries.push(format!("'first_row': {n}"));
        }
        if let Some(n) = self.tab_count {
            entries.push(format!("'tab_count': {n}"));
        }
        if let Some(n) = self.last_line {
            entries.push(format!("'last_line': {n}"));
        }
        entries.push(format!("'error_id': '{}'", check.id));
        entries.push(format!("'error_code': '{}'", check.code));
        entries.push(format!("'error_message': {}", quoted(&message(check, self.line, self.column))));
        entries.push(format!("'error_name': '{}'", check.name));
        entries.push(format!("'summary': {{'{}': {{'count': 1, 'error_code': '{}'}}}}", check.id, check.code));
        format!("{{{}}}", entries.join(", "))
    }
}

/// Whitespace as the tool's patterns see it, which takes in the four ASCII
/// separator controls as well.
fn space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// What Python's `string.printable` holds.
fn printable(c: char) -> bool {
    c.is_ascii_graphic() || matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

fn validate(run: &mut Run, text: Option<&str>, comment: Option<&str>) -> Result<(), Failed> {
    let Some(text) = text else {
        run.exception = Some("There is no stream, likely an error?");
        return run.check(&ENCODING, true);
    };
    let mut previous = String::new();
    let mut last_raw = "";
    let mut number = 0;
    for raw in text.split_inclusive('\n') {
        number += 1;
        run.line = number;
        run.column = 0;
        let line = raw.replace(['\n', '\r'], "");
        if !comment.is_some_and(|c| !c.is_empty() && line.starts_with(c)) {
            let first = *run.first_row.get_or_insert(number) == number;
            for (idx, cell) in line.split('\t').enumerate() {
                run.column = idx + 1;
                let findings = [
                    (&LEADING, cell.starts_with(space)),
                    (&TRAILING, cell.ends_with(space)),
                    (&NON_ASCII, !cell.chars().all(printable)),
                ];
                for (check, found) in findings {
                    run.check(check, found)?;
                    if run.values_in_column.len() <= idx {
                        run.values_in_column.resize(idx + 1, 0);
                    }
                    if !cell.trim_matches(space).is_empty() {
                        run.values_in_column[idx] += 1;
                    }
                }
                if first {
                    run.check(&HEADER_VALUE, cell.trim_matches(space).is_empty())?;
                }
            }
            run.column = 0;
            let tabs = line.matches('\t').count();
            match run.tab_count {
                None => run.tab_count = Some(tabs),
                Some(expected) => run.check(&TABS, tabs != expected)?,
            }
            if first {
                let cells: Vec<&str> = line.split('\t').collect();
                let distinct: std::collections::HashSet<&str> = cells.iter().copied().collect();
                run.check(&HEADER_DUPLICATE, distinct.len() != cells.len())?;
            }
            run.check(&LINE_BREAK, raw.contains('\r'))?;
            if number > 1 {
                run.line = number - 1;
                run.check(&EMPTY_LINE, previous.trim_matches(space).is_empty())?;
            }
        }
        previous = line;
        last_raw = raw;
    }
    run.column = 0;
    run.last_line = Some(run.line);
    run.check(&LAST_ROW, !last_raw.ends_with('\n'))?;
    for idx in 0..run.values_in_column.len() {
        run.check(&EMPTY_COLUMN, run.values_in_column[idx] == 0)?;
    }
    Ok(())
}

/// The check's name as the summary spells it: `emptyLine` is `empty Line`.
fn spelled_out(id: &str) -> String {
    let mut out = String::new();
    for c in id.chars() {
        if c.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode(bytes: &[u8], encoding: &str) -> Option<String> {
    match encoding.to_ascii_lowercase().replace('_', "-").as_str() {
        "utf-8" | "utf8" => String::from_utf8(bytes.to_vec()).ok(),
        "ascii" | "us-ascii" => bytes.is_ascii().then(|| String::from_utf8_lossy(bytes).into_owned()),
        "latin-1" | "latin1" | "iso-8859-1" => Some(bytes.iter().map(|b| *b as char).collect()),
        _ => None,
    }
}

pub fn main(argv: &[String]) -> i32 {
    let mut input: Option<&str> = None;
    let mut skip = Vec::new();
    let mut encoding = "utf-8";
    let mut comment: Option<&str> = None;
    let (mut summary, mut fail) = (false, false);
    let mut i = 0;
    while i < argv.len() {
        let (name, inline) = match argv[i].split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (argv[i].as_str(), None),
        };
        let mut value = |i: &mut usize| -> Option<&str> {
            inline.or_else(|| {
                *i += 1;
                argv.get(*i).map(String::as_str)
            })
        };
        match name {
            "--summary" => summary = true,
            "--fail" => fail = true,
            "--skip" | "--encoding" | "--comment" => {
                let Some(v) = value(&mut i) else {
                    eprintln!("Error: Option '{name}' requires an argument.");
                    return 2;
                };
                match name {
                    "--skip" => match regex::Regex::new(&format!("^(?:{v})$")) {
                        Ok(r) => skip.push(r),
                        Err(e) => {
                            eprintln!("Error: bad --skip pattern '{v}': {e}");
                            return 2;
                        }
                    },
                    "--encoding" => encoding = v,
                    _ => comment = Some(v),
                }
            }
            "-h" | "--help" => {
                println!(
                    "Usage: tsvalid [OPTIONS] INPUT\n\n  Validate a tsv file.\n\nOptions:\n  \
                     --skip TEXT      Skip a check by its code, or every check whose code a\n                   \
                     pattern matches (`--skip E2`, `--skip 'W.*'`).\n  \
                     --encoding TEXT  The file's encoding (default utf-8).\n  \
                     --comment TEXT   Lines starting with this are not checked.\n  \
                     --summary        Print a summary of the findings.\n  \
                     --fail           Stop with an error at the first finding."
                );
                return 0;
            }
            other if other.starts_with("--") => {
                eprintln!("Error: No such option: {other}");
                return 2;
            }
            other => input = Some(other),
        }
        i += 1;
    }
    let Some(input) = input else {
        eprintln!("Error: Missing argument 'INPUT'.");
        return 2;
    };
    let bytes = match std::fs::read(Path::new(input)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("tsvalid: {input}: {e}");
            return 1;
        }
    };
    let text = decode(&bytes, encoding);
    let mut run = Run {
        file: input,
        encoding,
        skip,
        fail,
        line: 0,
        column: 0,
        summary: Vec::new(),
        exception: None,
        first_row: None,
        tab_count: None,
        values_in_column: Vec::new(),
        last_line: None,
    };
    if validate(&mut run, text.as_deref(), comment).is_err() {
        return 1;
    }
    if summary && !run.summary.is_empty() {
        println!("\n##### TSValid Summary #####");
        for (check, count) in &run.summary {
            println!("\nError: {}\n * count: {count}\n * error_code: {}", spelled_out(check.id), check.code);
        }
    }
    0
}
