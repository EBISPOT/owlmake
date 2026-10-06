//! A pattern's data table, read into rows.
//!
//! A table is a sequence of records, each a list of cells. A record ends at a
//! line break (`\n`, `\r`, `\r\n`, U+0085, U+2028 or U+2029) outside quotes.
//! The first record is the header; each later one is a row, whose cells are
//! paired with the header's columns as far as both run, a later column of one
//! name taking the cell. A byte-order mark opening a record is skipped.
//!
//! In a TSV table cells are separated by tabs; a cell opening with `"` is
//! quoted up to the next `"` not doubled, and may hold tabs and line breaks.
//! Outside quotes a `\` escapes a following `\` or tab and otherwise stands for
//! itself; inside quotes it escapes `\` or `"`. In a CSV table cells are
//! separated by commas, a doubled `"` is a quote, and a `"` inside an unquoted
//! cell escapes a following `"` or comma and otherwise stands for a `\`.
//!
//! A quoted cell ending in anything but a separator or line break, an escape
//! with nothing after it, a quoted cell still open at the end of the table, and
//! (TSV) an escape opening a quoted cell that escapes nothing make the table
//! malformed.

use anyhow::{anyhow, bail, Result};

/// How a table separates and quotes its cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TableFormat {
    #[default]
    Tsv,
    Csv,
}

impl TableFormat {
    /// `tsv` or `csv`, in any case.
    pub fn parse(s: &str) -> Result<TableFormat> {
        match s.to_lowercase().as_str() {
            "tsv" => Ok(TableFormat::Tsv),
            "csv" => Ok(TableFormat::Csv),
            _ => bail!("`{s}` is not a table format: tsv or csv"),
        }
    }

    /// The extension a table of this format carries.
    pub fn extension(self) -> &'static str {
        match self {
            TableFormat::Tsv => "tsv",
            TableFormat::Csv => "csv",
        }
    }

    fn delimiter(self) -> char {
        match self {
            TableFormat::Tsv => '\t',
            TableFormat::Csv => ',',
        }
    }

    fn escape(self) -> char {
        match self {
            TableFormat::Tsv => '\\',
            TableFormat::Csv => '"',
        }
    }
}

const QUOTE: char = '"';

/// A row: its cells by column name, in the order the columns were paired.
#[derive(Clone, Debug, Default)]
pub(super) struct Row {
    pub(super) cells: Vec<(String, String)>,
}

impl Row {
    pub(super) fn from_record(header: &[String], record: &[String]) -> Row {
        let mut cells: Vec<(String, String)> = Vec::new();
        for (column, cell) in header.iter().zip(record) {
            match cells.iter_mut().find(|(c, _)| c == column) {
                Some(slot) => slot.1 = cell.clone(),
                None => cells.push((column.clone(), cell.clone())),
            }
        }
        Row { cells }
    }

    pub(super) fn get(&self, column: &str) -> Option<&str> {
        self.cells.iter().find(|(c, _)| c == column).map(|(_, v)| v.as_str())
    }
}

/// A generator's table: the text's lines without those holding nothing above
/// U+0020, read as one table; its header and its rows.
pub(super) fn read_generator_table(text: &str, format: TableFormat) -> Result<(Vec<String>, Vec<Row>)> {
    let kept: Vec<&str> = lines(text).filter(|l| !super::render::java_trim(l).is_empty()).collect();
    read_records(&kept.join("\n"), format)
}

/// A table read as it is written: its header and its rows.
pub(super) fn read_table(text: &str, format: TableFormat) -> Result<(Vec<String>, Vec<Row>)> {
    read_records(text, format)
}

/// The lines of `text`, each ended by `\n`, `\r\n` or `\r`.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
        let line = &rest[..end];
        let skip = if rest[end..].starts_with("\r\n") { 2 } else { usize::from(end < rest.len()) };
        rest = &rest[end + skip..];
        Some(line)
    })
}

fn read_records(text: &str, format: TableFormat) -> Result<(Vec<String>, Vec<Row>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut reader = Records { chars: &chars, pos: 0, format };
    let Some(header) = reader.next_record()? else { return Ok((Vec::new(), Vec::new())) };
    let mut rows = Vec::new();
    while let Some(record) = reader.next_record()? {
        rows.push(Row::from_record(&header, &record));
    }
    Ok((header, rows))
}

struct Records<'a> {
    chars: &'a [char],
    pos: usize,
    format: TableFormat,
}

impl Records<'_> {
    /// The next physical line, its line break included.
    fn next_line(&mut self) -> Option<&[char]> {
        let start = self.pos;
        if start >= self.chars.len() {
            return None;
        }
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            self.pos += 1;
            if matches!(c, '\n' | '\u{2028}' | '\u{2029}' | '\u{85}') {
                break;
            }
            if c == '\r' {
                if self.chars.get(self.pos) == Some(&'\n') {
                    self.pos += 1;
                }
                break;
            }
        }
        Some(&self.chars[start..self.pos])
    }

    /// The next record: one line, or as many as a quoted cell runs over.
    fn next_record(&mut self) -> Result<Option<Vec<String>>> {
        let format = self.format;
        let mut pending: Vec<char> = Vec::new();
        loop {
            let Some(line) = self.next_line() else {
                if pending.is_empty() {
                    return Ok(None);
                }
                let record: String = pending.iter().collect();
                bail!("the table ends inside a quoted cell: {record}");
            };
            pending.extend_from_slice(line);
            if let Some(record) = parse_record(&pending, format)? {
                return Ok(Some(record));
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    Field,
    Delimiter,
    End,
    QuoteStart,
    QuoteEnd,
    QuotedField,
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\u{2028}' | '\u{2029}' | '\u{85}')
}

/// The cells of the record `buf` holds, or none while a quoted cell is still
/// open at its end.
fn parse_record(buf: &[char], format: TableFormat) -> Result<Option<Vec<String>>> {
    let (escape, delimiter) = (format.escape(), format.delimiter());
    let malformed = || anyhow!("a malformed table record: {}", buf.iter().collect::<String>());
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut state = State::Start;
    let mut pos = 0;
    let len = buf.len();
    if buf.first() == Some(&'\u{FEFF}') {
        pos += 1;
    }
    // A record ending at `\r`. The `\n` after it is the record's last character,
    // so whether it is stepped over changes nothing.
    let end_record = |fields: &mut Vec<String>, field: &mut String| fields.push(std::mem::take(field));
    while state != State::End && pos < len {
        let c = buf[pos];
        match state {
            State::Start => {
                if c == QUOTE {
                    state = State::QuoteStart;
                    pos += 1;
                } else if c == delimiter {
                    end_record(&mut fields, &mut field);
                    state = State::Delimiter;
                    pos += 1;
                } else if is_line_break(c) || c == '\r' {
                    end_record(&mut fields, &mut field);
                    state = State::End;
                    pos += 1;
                } else {
                    field.push(c);
                    state = State::Field;
                    pos += 1;
                }
            }
            State::Delimiter => {
                if c == QUOTE {
                    state = State::QuoteStart;
                    pos += 1;
                } else if c == escape {
                    match buf.get(pos + 1) {
                        Some(&next) if next == escape || next == delimiter => {
                            field.push(next);
                            state = State::Field;
                            pos += 2;
                        }
                        Some(_) => {
                            field.push('\\');
                            state = State::Field;
                            pos += 1;
                        }
                        None => return Err(malformed()),
                    }
                } else if c == delimiter {
                    end_record(&mut fields, &mut field);
                    state = State::Delimiter;
                    pos += 1;
                } else if is_line_break(c) || c == '\r' {
                    end_record(&mut fields, &mut field);
                    state = State::End;
                    pos += 1;
                } else {
                    field.push(c);
                    state = State::Field;
                    pos += 1;
                }
            }
            State::Field => {
                if c == escape {
                    match buf.get(pos + 1) {
                        Some(&next) if next == escape || next == delimiter => {
                            field.push(next);
                            pos += 2;
                        }
                        Some(_) => {
                            field.push('\\');
                            pos += 1;
                        }
                        None => {
                            state = State::QuoteEnd;
                            pos += 1;
                        }
                    }
                } else if c == delimiter {
                    end_record(&mut fields, &mut field);
                    state = State::Delimiter;
                    pos += 1;
                } else if is_line_break(c) || c == '\r' {
                    end_record(&mut fields, &mut field);
                    state = State::End;
                    pos += 1;
                } else {
                    field.push(c);
                    pos += 1;
                }
            }
            State::QuoteStart | State::QuotedField => {
                if c == escape && escape != QUOTE {
                    match buf.get(pos + 1) {
                        Some(&next) if next == escape || next == QUOTE => {
                            field.push(next);
                            state = State::QuotedField;
                            pos += 2;
                        }
                        Some(&next) if state == State::QuotedField => {
                            field.push(c);
                            field.push(next);
                            pos += 2;
                        }
                        _ => return Err(malformed()),
                    }
                } else if c == QUOTE {
                    if buf.get(pos + 1) == Some(&QUOTE) {
                        field.push(QUOTE);
                        state = State::QuotedField;
                        pos += 2;
                    } else {
                        state = State::QuoteEnd;
                        pos += 1;
                    }
                } else {
                    field.push(c);
                    state = State::QuotedField;
                    pos += 1;
                }
            }
            State::QuoteEnd => {
                if c == delimiter {
                    end_record(&mut fields, &mut field);
                    state = State::Delimiter;
                    pos += 1;
                } else if is_line_break(c) || c == '\r' {
                    end_record(&mut fields, &mut field);
                    state = State::End;
                    pos += 1;
                } else {
                    return Err(malformed());
                }
            }
            State::End => unreachable!("the loop stops at the end of the record"),
        }
    }
    Ok(match state {
        State::Delimiter => {
            fields.push(String::new());
            Some(fields)
        }
        State::QuotedField => None,
        State::Field | State::QuoteEnd => {
            fields.push(field);
            Some(fields)
        }
        State::Start | State::QuoteStart | State::End => Some(fields),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(text: &str, format: TableFormat) -> Vec<Vec<String>> {
        let chars: Vec<char> = text.chars().collect();
        let mut r = Records { chars: &chars, pos: 0, format };
        let mut out = Vec::new();
        while let Some(rec) = r.next_record().unwrap() {
            out.push(rec);
        }
        out
    }

    #[test]
    fn records_split_as_the_reader_splits_them() {
        let tsv = TableFormat::Tsv;
        assert_eq!(records("a\tb\nc\td", tsv), [["a", "b"], ["c", "d"]]);
        // A backslash escapes itself and a tab, and otherwise stands for itself.
        assert_eq!(records("a\\\\b\tc\\\td\te\\f\n", tsv), [["a\\b", "c\td", "e\\f"]]);
        // A backslash ending the table is dropped.
        assert_eq!(records("a\tb\\", tsv), [["a", "b"]]);
        // Quotes: a doubled quote, a tab and a line break inside them.
        assert_eq!(records("\"a\"\"b\tc\nd\"\te\n", tsv), [["a\"b\tc\nd", "e"]]);
        // Inside quotes a backslash escapes a quote or itself, else stays.
        assert_eq!(records("\"a\\\"b\\c\"\n", tsv), [["a\"b\\c"]]);
        // A quote inside an unquoted cell is a character.
        assert_eq!(records("a\"b\tc\n", tsv), [["a\"b", "c"]]);
        // U+2028 ends a record outside quotes.
        assert_eq!(records("a\u{2028}b\n", tsv), [["a"], ["b"]]);
        // A trailing separator gives an empty last cell; an empty line one
        // empty cell.
        assert_eq!(records("a\t\n\nb", tsv), [vec!["a", ""], vec![""], vec!["b"]]);
        // CSV: a quote in an unquoted cell escapes a quote or comma, else is a
        // backslash.
        let csv = TableFormat::Csv;
        assert_eq!(records("a\"\"b,c\",d,e\"f\n", csv), [["a\"b", "c,d", "e\\f"]]);
        assert_eq!(records("\"a,b\",\"c\"\"d\"\n", csv), [["a,b", "c\"d"]]);
    }

    #[test]
    fn malformed_records_are_refused() {
        let chars: Vec<char> = "\"a\"b\tc\n".chars().collect();
        let mut r = Records { chars: &chars, pos: 0, format: TableFormat::Tsv };
        assert!(r.next_record().is_err());
        let chars: Vec<char> = "\"a\tb\n".chars().collect();
        let mut r = Records { chars: &chars, pos: 0, format: TableFormat::Tsv };
        assert!(r.next_record().is_err());
        let chars: Vec<char> = "\"\\x\"\n".chars().collect();
        let mut r = Records { chars: &chars, pos: 0, format: TableFormat::Tsv };
        assert!(r.next_record().is_err());
    }

    #[test]
    fn a_generator_drops_blank_lines() {
        let (header, rows) = read_generator_table("h1\th2\r\n \n\"x\n\n y\"\tz\n", TableFormat::Tsv).unwrap();
        assert_eq!(header, ["h1", "h2"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("h1"), Some("x\n y"));
        assert_eq!(rows[0].get("h2"), Some("z"));
    }
}
