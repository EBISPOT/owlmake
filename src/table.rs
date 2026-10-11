//! Delimited tables — the one parser every TSV/CSV a build reads goes through,
//! and the one quoting rule every TSV/CSV a command writes follows.
//!
//! The delimiter is not the whole grammar: a field may be QUOTED, and a quoted
//! field carries the delimiter, line breaks and doubled quotes as data. HPO's
//! `translations/hp-cs.synonyms.tsv` holds `"""Ghost teeth"""` — one field whose
//! value is `"Ghost teeth"` — and reading it by splitting on the tab alone puts
//! six quote characters into a released synonym.

/// Read a delimited table with CSV quoting: a field that OPENS with `"` runs to
/// the closing `"`, `""` inside it is a literal quote, and the delimiter and line
/// breaks inside it are data. A blank line yields no record.
pub fn read(text: &str, delim: char) -> Vec<Vec<String>> {
    read_records(text, delim, false)
}

/// [`read`], where a blank line is a record of one empty field.
pub fn read_with_blank_lines(text: &str, delim: char) -> Vec<Vec<String>> {
    read_records(text, delim, true)
}

fn read_records(text: &str, delim: char, blank_lines: bool) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => in_quotes = true,
            _ if c == delim => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                if blank_lines || !(record.len() == 1 && record[0].is_empty()) {
                    records.push(std::mem::take(&mut record));
                } else {
                    record.clear();
                }
            }
            _ => field.push(c),
        }
    }
    if any && (!field.is_empty() || !record.is_empty()) {
        record.push(field);
        if !(record.len() == 1 && record[0].is_empty()) {
            records.push(record);
        }
    }
    records
}

/// [`read`] with a tab delimiter.
pub fn read_tsv(text: &str) -> Vec<Vec<String>> {
    read(text, '\t')
}

/// One field as a table writes it: quoted when it holds the delimiter, a quote
/// or a line break, with each quote inside doubled. Any other field is written
/// as it is, leading and trailing spaces and control characters included — so a
/// tab in a value never moves the cells after it into the wrong columns.
pub fn field(s: &str, delim: char) -> std::borrow::Cow<'_, str> {
    if s.contains(delim) || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\"")).into()
    } else {
        s.into()
    }
}

/// One record as a table writes it: its fields, each quoted by [`field`], joined
/// by the delimiter and ended by a line feed.
pub fn record<S: AsRef<str>>(fields: &[S], delim: char) -> String {
    let mut out = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(delim);
        }
        out.push_str(&field(f.as_ref(), delim));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field is quoted for its delimiter, a quote or a line break, and for
    /// nothing else: a tab in a CSV field and a comma in a TSV one stand bare.
    #[test]
    fn a_field_is_quoted_for_its_delimiter_a_quote_or_a_line_break() {
        assert_eq!(field("plain", ','), "plain");
        assert_eq!(field("a,b", ','), "\"a,b\"");
        assert_eq!(field("a,b", '\t'), "a,b");
        assert_eq!(field("a\tb", '\t'), "\"a\tb\"");
        assert_eq!(field("a\tb", ','), "a\tb");
        assert_eq!(field("a\"b", '\t'), "\"a\"\"b\"");
        assert_eq!(field("a\nb", ','), "\"a\nb\"");
        assert_eq!(field("a\rb", '\t'), "\"a\rb\"");
        assert_eq!(field(" lead", '\t'), " lead");
        assert_eq!(record(&["x", "y\tz", ""], '\t'), "x\t\"y\tz\"\t\n");
    }

    /// What the writer quotes, the reader reads back as the one field it was.
    #[test]
    fn a_written_record_reads_back() {
        let fields = ["say \"hi\"", "tab\there", "line\nbreak", "", "plain"];
        let text = record(&fields, '\t');
        assert_eq!(read_tsv(&text), vec![fields.iter().map(|f| f.to_string()).collect::<Vec<_>>()]);
    }
}
