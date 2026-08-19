//! Reading delimited tables — the one parser every TSV/CSV a build reads goes
//! through.
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
                // A wholly blank line is skipped, not read as an empty row.
                if !(record.len() == 1 && record[0].is_empty()) {
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
