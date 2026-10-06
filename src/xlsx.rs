//! A one-sheet Excel workbook (`.xlsx`) of text cells.
//!
//! The workbook is a ZIP package of XML parts: the content types, the package
//! and workbook relationships, the document properties, a styles part with the
//! one default cell style, a shared-strings table, and the sheet `Sheet0`. Every
//! cell holds a shared string — an empty value included — and every cell names
//! the default style. A sheet with rows below the first carries an empty drawing
//! part. Each distinct text is stored once, in the order it first occurs, row by
//! row.
//!
//! The package is the same bytes for the same rows: every entry is stamped
//! 1980-01-01 00:00, and the core properties carry no creation time.

use std::io::Write;

use anyhow::{bail, Result};

/// The most text a cell holds, in UTF-16 code units.
const MAX_CELL_TEXT: usize = 32_767;
/// The most rows and columns a sheet holds.
const MAX_ROWS: usize = 1_048_576;
const MAX_COLUMNS: usize = 16_384;

const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PACKAGE_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// The workbook holding `rows` on its one sheet, the first row first.
pub fn workbook(rows: &[Vec<String>]) -> Result<Vec<u8>> {
    if rows.len() > MAX_ROWS {
        bail!("a sheet holds at most {MAX_ROWS} rows; this table has {}", rows.len());
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width > MAX_COLUMNS {
        bail!("a sheet holds at most {MAX_COLUMNS} columns; this table has {width}");
    }
    for row in rows {
        for cell in row {
            let n = cell.encode_utf16().count();
            if n > MAX_CELL_TEXT {
                bail!("a cell holds at most {MAX_CELL_TEXT} characters; one cell of this table holds {n}");
            }
        }
    }
    let drawing = rows.len() > 1;

    // The shared strings, each once, in first-use order.
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut strings: Vec<&str> = Vec::new();
    let mut count = 0usize;
    let mut sheet_data = String::new();
    for (r, row) in rows.iter().enumerate() {
        sheet_data.push_str(&format!("<row r=\"{}\">", r + 1));
        for (c, cell) in row.iter().enumerate() {
            let i = *index.entry(cell.as_str()).or_insert_with(|| {
                strings.push(cell.as_str());
                strings.len() - 1
            });
            count += 1;
            sheet_data.push_str(&format!(
                "<c r=\"{}{}\" t=\"s\" s=\"0\"><v>{i}</v></c>",
                column_letters(c),
                r + 1
            ));
        }
        sheet_data.push_str("</row>");
    }

    let mut sst = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sst count=\"{count}\" uniqueCount=\"{}\" xmlns=\"{MAIN}\">",
        strings.len()
    );
    for s in &strings {
        sst.push_str(&shared_string(s));
    }
    sst.push_str("</sst>");

    let last = format!("{}{}", column_letters(width.max(1) - 1), rows.len().max(1));
    let sheet = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<worksheet xmlns=\"{MAIN}\"{}><dimension ref=\"A1:{last}\"/>\
         <sheetViews><sheetView workbookViewId=\"0\" tabSelected=\"true\"/></sheetViews>\
         <sheetFormatPr defaultRowHeight=\"15.0\"/><sheetData>{sheet_data}</sheetData>\
         <pageMargins bottom=\"0.75\" footer=\"0.3\" header=\"0.3\" left=\"0.7\" right=\"0.7\" top=\"0.75\"/>{}</worksheet>",
        if drawing { format!(" xmlns:r=\"{REL}\"") } else { String::new() },
        if drawing { "<drawing r:id=\"rId1\"/>" } else { "" },
    );

    let mut types = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default ContentType=\"application/vnd.openxmlformats-package.relationships+xml\" Extension=\"rels\"/>\
         <Default ContentType=\"application/xml\" Extension=\"xml\"/>\
         <Override ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\" PartName=\"/docProps/app.xml\"/>\
         <Override ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\" PartName=\"/docProps/core.xml\"/>",
    );
    if drawing {
        types.push_str(
            "<Override ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\" PartName=\"/xl/drawings/drawing1.xml\"/>",
        );
    }
    types.push_str(
        "<Override ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml\" PartName=\"/xl/sharedStrings.xml\"/>\
         <Override ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\" PartName=\"/xl/styles.xml\"/>\
         <Override ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\" PartName=\"/xl/workbook.xml\"/>\
         <Override ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\" PartName=\"/xl/worksheets/sheet1.xml\"/>\
         </Types>",
    );

    let package_rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"{PACKAGE_REL}\">\
         <Relationship Id=\"rId1\" Target=\"xl/workbook.xml\" Type=\"{REL}/officeDocument\"/>\
         <Relationship Id=\"rId2\" Target=\"docProps/app.xml\" Type=\"{REL}/extended-properties\"/>\
         <Relationship Id=\"rId3\" Target=\"docProps/core.xml\" Type=\"{PACKAGE_REL}/metadata/core-properties\"/>\
         </Relationships>"
    );
    let app = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
               <Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\">\
               <Application>owlmake</Application></Properties>";
    let core = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
                <cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
                xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" \
                xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:creator>owlmake</dc:creator></cp:coreProperties>";
    let drawing_part = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                        <xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\"/>";
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<styleSheet xmlns=\"{MAIN}\"><numFmts count=\"0\"/>\
         <fonts count=\"1\"><font><sz val=\"11.0\"/><color indexed=\"8\"/><name val=\"Calibri\"/><family val=\"2\"/>\
         <scheme val=\"minor\"/></font></fonts><fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill>\
         <fill><patternFill patternType=\"darkGray\"/></fill></fills><borders count=\"1\"><border><left/><right/><top/>\
         <bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" \
         borderId=\"0\"/></cellStyleXfs><cellXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" \
         xfId=\"0\"/></cellXfs></styleSheet>"
    );
    let book = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<workbook xmlns=\"{MAIN}\" xmlns:r=\"{REL}\">\
         <workbookPr date1904=\"false\"/><bookViews><workbookView activeTab=\"0\"/></bookViews>\
         <sheets><sheet name=\"Sheet0\" r:id=\"rId3\" sheetId=\"1\"/></sheets></workbook>"
    );
    let book_rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"{PACKAGE_REL}\">\
         <Relationship Id=\"rId1\" Target=\"sharedStrings.xml\" Type=\"{REL}/sharedStrings\"/>\
         <Relationship Id=\"rId2\" Target=\"styles.xml\" Type=\"{REL}/styles\"/>\
         <Relationship Id=\"rId3\" Target=\"worksheets/sheet1.xml\" Type=\"{REL}/worksheet\"/>\
         </Relationships>"
    );
    let sheet_rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"{PACKAGE_REL}\">\
         <Relationship Id=\"rId1\" Target=\"../drawings/drawing1.xml\" Type=\"{REL}/drawing\"/></Relationships>"
    );

    let mut zip = Zip::default();
    zip.add("[Content_Types].xml", types.as_bytes())?;
    zip.add("_rels/.rels", package_rels.as_bytes())?;
    zip.add("docProps/app.xml", app.as_bytes())?;
    zip.add("docProps/core.xml", core.as_bytes())?;
    if drawing {
        zip.add("xl/drawings/drawing1.xml", drawing_part.as_bytes())?;
    }
    zip.add("xl/sharedStrings.xml", sst.as_bytes())?;
    zip.add("xl/styles.xml", styles.as_bytes())?;
    zip.add("xl/workbook.xml", book.as_bytes())?;
    zip.add("xl/_rels/workbook.xml.rels", book_rels.as_bytes())?;
    zip.add("xl/worksheets/sheet1.xml", sheet.as_bytes())?;
    if drawing {
        zip.add("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels.as_bytes())?;
    }
    Ok(zip.finish())
}

/// A column's letters: `A` … `Z`, `AA` …, from its index counting from 0.
fn column_letters(mut i: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).expect("ASCII")
}

/// One shared-strings entry. `&` and `<` are escaped and a carriage return is
/// written as a character reference; a character XML cannot carry becomes `?`.
/// A text that starts or ends with white space keeps it with
/// `xml:space="preserve"`.
fn shared_string(s: &str) -> String {
    if s.is_empty() {
        return "<si><t/></si>".to_string();
    }
    let space = |c: char| {
        matches!(c, '\t' | '\n' | '\u{B}' | '\u{C}' | '\r' | '\u{1C}'..='\u{1F}')
            || (c.is_whitespace() && !matches!(c, '\u{A0}' | '\u{2007}' | '\u{202F}' | '\u{85}'))
    };
    let preserve = s.starts_with(space) || s.ends_with(space);
    let mut t = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => t.push_str("&amp;"),
            '<' => t.push_str("&lt;"),
            '\r' => t.push_str("&#13;"),
            '\t' | '\n' => t.push(c),
            '\u{0}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}' => t.push('?'),
            _ => t.push(c),
        }
    }
    if preserve {
        format!("<si><t xml:space=\"preserve\">{t}</t></si>")
    } else {
        format!("<si><t>{t}</t></si>")
    }
}

/// A ZIP archive of deflated entries, written in the order they are added.
#[derive(Default)]
struct Zip {
    data: Vec<u8>,
    central: Vec<u8>,
    entries: u16,
}

impl Zip {
    /// 1980-01-01 00:00 in the archive's date and time fields.
    const DATE: u16 = (1 << 5) | 1;
    const TIME: u16 = 0;

    fn add(&mut self, name: &str, content: &[u8]) -> Result<()> {
        let mut crc = flate2::Crc::new();
        crc.update(content);
        let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        deflate.write_all(content)?;
        let packed = deflate.finish()?;
        let offset = self.data.len() as u32;
        let fields = |out: &mut Vec<u8>| {
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed to extract
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&8u16.to_le_bytes()); // deflate
            out.extend_from_slice(&Self::TIME.to_le_bytes());
            out.extend_from_slice(&Self::DATE.to_le_bytes());
            out.extend_from_slice(&crc.sum().to_le_bytes());
            out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(content.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        };
        self.data.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        fields(&mut self.data);
        self.data.extend_from_slice(name.as_bytes());
        self.data.extend_from_slice(&packed);

        self.central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        self.central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        fields(&mut self.central);
        self.central.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        self.central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        self.central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name.as_bytes());
        self.entries += 1;
        Ok(())
    }

    fn finish(mut self) -> Vec<u8> {
        let offset = self.data.len() as u32;
        let size = self.central.len() as u32;
        self.data.extend_from_slice(&self.central);
        self.data.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.data.extend_from_slice(&0u16.to_le_bytes()); // this disk
        self.data.extend_from_slice(&0u16.to_le_bytes()); // disk of the directory
        self.data.extend_from_slice(&self.entries.to_le_bytes());
        self.data.extend_from_slice(&self.entries.to_le_bytes());
        self.data.extend_from_slice(&size.to_le_bytes());
        self.data.extend_from_slice(&offset.to_le_bytes());
        self.data.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_are_lettered_as_a_spreadsheet_letters_them() {
        assert_eq!(column_letters(0), "A");
        assert_eq!(column_letters(25), "Z");
        assert_eq!(column_letters(26), "AA");
        assert_eq!(column_letters(701), "ZZ");
        assert_eq!(column_letters(702), "AAA");
    }

    #[test]
    fn a_shared_string_escapes_what_xml_needs_and_keeps_its_spaces() {
        assert_eq!(shared_string(""), "<si><t/></si>");
        assert_eq!(shared_string("a & <b> \"q\""), "<si><t>a &amp; &lt;b> \"q\"</t></si>");
        assert_eq!(shared_string(" lead"), "<si><t xml:space=\"preserve\"> lead</t></si>");
        assert_eq!(shared_string("nl\nx\r\ny"), "<si><t>nl\nx&#13;\ny</t></si>");
        assert_eq!(shared_string("ctl\u{1}\u{1f} end"), "<si><t>ctl?? end</t></si>");
    }

    /// The same rows make the same bytes.
    #[test]
    fn a_workbook_is_the_same_bytes_every_time() {
        let rows = vec![vec!["ID".to_string(), "LABEL".to_string()], vec!["EX:1".to_string(), String::new()]];
        assert_eq!(workbook(&rows).unwrap(), workbook(&rows).unwrap());
    }
}
