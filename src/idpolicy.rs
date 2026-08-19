//! OBO **ID-policy** files (`<ont>-idranges.owl`) — parse and validate.
//!
//! An OBO repository declares its numeric ID space in one of these files: the IRI
//! stem and digit width new IDs are minted with, and which block of numbers is
//! allocated to whom. The repository's QC checks that file, because a policy with
//! an unowned or overlapping range lets two curators mint the same ID and nothing
//! downstream notices. `om validate-id-ranges` (`src/cmd/validate_id_ranges.rs`)
//! is a thin wrapper over this module, and `om mint` reads the same ranges to
//! find the one it mints from.
//!
//! The file is OWL Manchester syntax, but only a tiny, entirely regular subset of
//! it: a `Prefix:` block, one `Ontology:` frame carrying the `idsfor`/`idprefix`/
//! `iddigits` policy annotations, and one `Datatype: idrange:N` frame per
//! allocated range, each with an `allocatedto:` annotation and an
//! `EquivalentTo: xsd:integer[>= LO , <= HI]` facet. Parsing that subset directly
//! — rather than scraping lines — is what lets the checks below distinguish "this
//! frame has no owner" from "this line is not a facet", which a scraper cannot: a
//! scraper silently ignores everything it fails to recognise, so a policy file
//! missing its `EquivalentTo` blocks, its `allocatedto` annotations, or its
//! `Ontology:` frame entirely would still pass.
//!
//! Deliberately NOT an OWL parse: the policy invariants are all structural, so an
//! OWL round-trip buys nothing here — while a full Manchester parser would reject
//! the loose, hand-maintained whitespace these files carry (see EFO's, which mixes
//! tabs, trailing blanks and stray indentation).

use std::path::Path;

use anyhow::{Context, Result};

/// One allocated ID range: a `Datatype: idrange:N` frame with an `allocatedto:`
/// annotation and an `xsd:integer[…]` facet. Bounds are INCLUSIVE — a `>`/`<`
/// facet is normalised to `>=`/`<=` at parse time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdRange {
    /// The local part of the datatype name (`idrange:7` → `7`).
    pub id: String,
    /// The `allocatedto:` string — the range's owner, and the name `om mint`
    /// selects a range by.
    pub owner: String,
    pub low: i64,
    pub high: i64,
    /// 1-based line of the `Datatype:` frame header, for diagnostics.
    pub line: usize,
}

/// A policy violation, anchored to the line that caused it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// 1-based source line; 0 when the problem concerns the file as a whole.
    pub line: usize,
    pub message: String,
}

impl Violation {
    fn whole(message: impl Into<String>) -> Violation {
        Violation { line: 0, message: message.into() }
    }
    fn at(line: usize, message: impl Into<String>) -> Violation {
        Violation { line, message: message.into() }
    }
    /// `path:line: message` — the `file:line:` prefix editors and CI logs parse.
    pub fn render(&self, path: &Path) -> String {
        if self.line == 0 {
            format!("{}: {}", path.display(), self.message)
        } else {
            format!("{}:{}: {}", path.display(), self.line, self.message)
        }
    }
}

/// A `Datatype: idrange:N` frame as read, before the completeness checks: the
/// pieces that may be MISSING are `Option`, which is the whole point — a range
/// with no owner and a range with no facet are different failures, and both are
/// invisible to a line scraper.
#[derive(Debug)]
struct RawRange {
    id: String,
    line: usize,
    owner: Option<String>,
    /// The parsed inclusive bounds, when an `EquivalentTo` facet was present and
    /// well-formed.
    facet: Option<(i64, i64)>,
    /// An `EquivalentTo` body that was present but unparseable: `(line, text)`.
    bad_facet: Option<(usize, String)>,
}

/// A parsed ID-policy document.
#[derive(Debug, Default)]
pub struct IdPolicy {
    /// `Prefix: name: <IRI>` declarations, in file order (`name` without the `:`).
    pub prefixes: Vec<(String, String)>,
    pub ontology_iri: Option<String>,
    /// `idsfor:` — the ontology the policy allocates for (e.g. `"OBA"`).
    pub idsfor: Option<String>,
    /// `idprefix:` — the IRI stem new IDs are minted under.
    pub idprefix: Option<String>,
    /// `iddigits:` — the width of the numeric ID space, so the largest legal
    /// local ID is `10^iddigits - 1`.
    pub iddigits: Option<u32>,
    /// The complete ranges (owner + bounds both present).
    pub ranges: Vec<IdRange>,
    /// Every `Datatype: idrange:N` frame, complete or not.
    raw: Vec<RawRange>,
    /// Problems found while reading (malformed `Prefix:`/`Ontology:`/`iddigits`).
    structural: Vec<Violation>,
}

/// Which frame the parser is inside. Manchester frames are opened by a keyword at
/// the start of a line and run until the next keyword — blank lines do NOT close
/// one (EFO's file separates a `Datatype:` header from its `Annotations:` block
/// with a blank line), so only a keyword ends a frame.
#[derive(Debug, Clone, PartialEq)]
enum Frame {
    None,
    Ontology,
    /// An `idrange:` datatype frame — index into `IdPolicy::raw`.
    Range(usize),
    /// Any other frame (`Datatype: xsd:integer`, `AnnotationProperty: …`): EFO's
    /// file ends with two bare datatype declarations, which are NOT ID ranges and
    /// must not be checked as if they were.
    Other,
}

/// Which block within the frame the parser is inside.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Block {
    None,
    Annotations,
    EquivalentTo,
}

/// The Manchester frame/section keywords that terminate whatever came before.
/// (Only the ones an ID-policy file can contain; anything else in the file is
/// content of the open block.)
const KEYWORDS: &[&str] = &[
    "Prefix:",
    "Ontology:",
    "Import:",
    "Annotations:",
    "Datatype:",
    "Class:",
    "ObjectProperty:",
    "DataProperty:",
    "AnnotationProperty:",
    "Individual:",
    "DisjointClasses:",
    "EquivalentTo:",
    "SubClassOf:",
];

/// Split a line into `(keyword, rest)` when it opens a Manchester frame/section.
fn keyword_of(line: &str) -> Option<(&'static str, &str)> {
    for kw in KEYWORDS {
        if let Some(rest) = line.strip_prefix(kw) {
            return Some((kw, rest.trim()));
        }
    }
    None
}

/// Parse an ID-policy document. Never fails: anything it cannot read becomes a
/// [`Violation`] in [`check`], because a policy file the checker cannot read is
/// exactly the case a scraper turns into a false PASS.
pub fn parse(text: &str) -> IdPolicy {
    let mut p = IdPolicy::default();
    let mut frame = Frame::None;
    let mut block = Block::None;

    for (idx, raw_line) in text.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw_line.trim();
        // A whole-line `#` comment. NOT an inline strip: every IRI in the file
        // (`…rdf-syntax-ns#`) contains a `#`, and the files open with a
        // `## ID Ranges File` banner.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((kw, rest)) = keyword_of(line) {
            match kw {
                "Prefix:" => {
                    block = Block::None;
                    match parse_prefix(rest) {
                        Some((name, iri)) => p.prefixes.push((name, iri)),
                        None => p.structural.push(Violation::at(
                            lineno,
                            format!("unparseable Prefix declaration: `{line}` (expected `Prefix: name: <IRI>`)"),
                        )),
                    }
                }
                "Ontology:" => {
                    frame = Frame::Ontology;
                    block = Block::None;
                    match angle_iri(rest) {
                        // `Ontology: <iri> <versionIri>` — the first is the IRI.
                        Some(iri) => p.ontology_iri = Some(iri),
                        None if rest.is_empty() => {
                            // Anonymous ontology: legal Manchester, but a policy
                            // file must be identifiable, so `check` reports it.
                        }
                        None => p.structural.push(Violation::at(
                            lineno,
                            format!("unparseable Ontology frame: `{line}` (expected `Ontology: <IRI>`)"),
                        )),
                    }
                }
                "Datatype:" => {
                    block = Block::None;
                    match rest.strip_prefix("idrange:") {
                        Some(id) => {
                            p.raw.push(RawRange {
                                id: id.trim().to_string(),
                                line: lineno,
                                owner: None,
                                facet: None,
                                bad_facet: None,
                            });
                            frame = Frame::Range(p.raw.len() - 1);
                        }
                        None => frame = Frame::Other,
                    }
                }
                "Annotations:" => {
                    block = Block::Annotations;
                    if !rest.is_empty() {
                        take_annotation(&mut p, &frame, lineno, rest);
                    }
                }
                "EquivalentTo:" => {
                    block = Block::EquivalentTo;
                    if !rest.is_empty() {
                        take_facet(&mut p, &frame, lineno, rest);
                    }
                }
                // Any other frame keyword: it closes the previous frame and
                // carries nothing this checker needs.
                _ => {
                    frame = Frame::Other;
                    block = Block::None;
                }
            }
            continue;
        }

        // Content of the open block.
        match block {
            Block::Annotations => take_annotation(&mut p, &frame, lineno, line),
            Block::EquivalentTo => take_facet(&mut p, &frame, lineno, line),
            // No block open, but a frame is. Generated policy files write
            // `Annotations:\n allocatedto: "X"`; the BARE form directly inside
            // the `Datatype:` frame is legal Manchester shorthand and appears in
            // hand-written policies, so it must be read as an annotation of the
            // open frame rather than ignored — otherwise the range loses its
            // owner and `om mint` cannot find a range by name.
            Block::None => {
                if !matches!(frame, Frame::None | Frame::Other)
                    && line.contains(':')
                    && !line.starts_with('<')
                {
                    take_annotation(&mut p, &frame, lineno, line);
                }
            }
        }
    }

    // Promote the complete frames to public ranges.
    p.ranges = p
        .raw
        .iter()
        .filter_map(|r| {
            let (low, high) = r.facet?;
            Some(IdRange {
                id: r.id.clone(),
                owner: r.owner.clone().unwrap_or_default(),
                low,
                high,
                line: r.line,
            })
        })
        .collect();
    p
}

/// `Prefix: name: <IRI>` → `(name, iri)`. The name may be empty (the default
/// prefix, `Prefix: : <IRI>`), and the IRI must be angle-bracketed.
fn parse_prefix(rest: &str) -> Option<(String, String)> {
    let (name, iri) = rest.split_once(':')?;
    let name = name.trim();
    // A prefix name is an XML NCName-ish token; anything with whitespace or an
    // angle bracket in it means the line is malformed, not that we mis-split.
    if name.contains(char::is_whitespace) || name.contains('<') {
        return None;
    }
    Some((name.to_string(), angle_iri(iri.trim())?))
}

/// The first `<…>` IRI in `s`, unbracketed.
fn angle_iri(s: &str) -> Option<String> {
    let inner = s.strip_prefix('<')?;
    let end = inner.find('>')?;
    let iri = &inner[..end];
    if iri.is_empty() {
        return None;
    }
    Some(iri.to_string())
}

/// Read one `prop: value` annotation line into the open frame. Only the four
/// properties the ID policy is made of are recognised; anything else (a
/// `comment:`, say) is carried by the file but means nothing here.
fn take_annotation(p: &mut IdPolicy, frame: &Frame, lineno: usize, line: &str) {
    let Some((prop, value)) = line.split_once(':') else { return };
    let prop = prop.trim();
    // Trailing `,` separates annotations within a frame's `Annotations:` block.
    let value = value.trim().trim_end_matches(',').trim();
    let unquoted = value.trim_matches('"');
    match (frame, prop) {
        (Frame::Range(i), "allocatedto") => {
            if let Some(r) = p.raw.get_mut(*i) {
                r.owner = Some(unquoted.to_string());
            }
        }
        (Frame::Ontology, "idsfor") => p.idsfor = Some(unquoted.to_string()),
        (Frame::Ontology, "idprefix") => p.idprefix = Some(unquoted.to_string()),
        (Frame::Ontology, "iddigits") => match unquoted.parse::<u32>() {
            Ok(n) => p.iddigits = Some(n),
            Err(_) => p.structural.push(Violation::at(
                lineno,
                format!("iddigits is not a number: `{unquoted}`"),
            )),
        },
        _ => {}
    }
}

/// Read one `EquivalentTo:` body line into the open range frame.
fn take_facet(p: &mut IdPolicy, frame: &Frame, lineno: usize, line: &str) {
    let Frame::Range(i) = frame else { return };
    let Some(r) = p.raw.get_mut(*i) else { return };
    match parse_integer_facet(line) {
        Some(bounds) => r.facet = Some(bounds),
        // Record the text so `check` can say "this restriction is unreadable"
        // rather than "this range has none" — the distinction a scraper loses.
        None => {
            if r.facet.is_none() && r.bad_facet.is_none() {
                r.bad_facet = Some((lineno, line.to_string()));
            }
        }
    }
}

/// Extract INCLUSIVE `(low, high)` from a Manchester `xsd:integer[>= LO , <= HI]`
/// facet. `>`/`<` are normalised to `>=`/`<=` (CL's file is written
/// `xsd:integer[>= 1, < 2000]`, UBERON's `[> 1 , <= 499999]`).
///
/// Returns `None` when the line is not a well-formed integer facet — including
/// when a bound is missing, which is a policy violation rather than something to
/// skip past.
pub fn parse_integer_facet(line: &str) -> Option<(i64, i64)> {
    if !line.contains("integer[") {
        return None;
    }
    let inner = line.split('[').nth(1)?.split(']').next()?;
    let mut low = None;
    let mut high = None;
    for part in inner.split(',') {
        let p = part.trim();
        // `>=` before `>` and `<=` before `<`, or the `=` would land in the value.
        if let Some(v) = p.strip_prefix(">=") {
            low = v.trim().parse::<i64>().ok();
        } else if let Some(v) = p.strip_prefix('>') {
            low = v.trim().parse::<i64>().ok().map(|n| n + 1);
        } else if let Some(v) = p.strip_prefix("<=") {
            high = v.trim().parse::<i64>().ok();
        } else if let Some(v) = p.strip_prefix('<') {
            high = v.trim().parse::<i64>().ok().map(|n| n - 1);
        } else {
            return None;
        }
    }
    Some((low?, high?))
}

/// Every complete `Datatype: idrange:N … EquivalentTo: xsd:integer[…]` block, as
/// [`IdRange`]s. The convenience wrapper `om mint` uses to resolve a named range.
pub fn parse_idranges(text: &str) -> Vec<IdRange> {
    parse(text).ranges
}

/// Check every policy invariant, returning the violations in source order.
///
/// The invariants a policy file must satisfy:
///
/// 1. every `Prefix:` line is well-formed, and `idrange:` is declared;
/// 2. the document has an `Ontology:` IRI;
/// 3. the policy header declares `idsfor`, `idprefix` and `iddigits`;
/// 4. at least one range is allocated;
/// 5. every `idrange:` frame carries an `allocatedto:` owner;
/// 6. every `idrange:` frame carries a readable integer restriction;
/// 7. no range ID is declared twice;
/// 8. every range is well-formed (`low <= high`, and `low >= 0`);
/// 9. every range fits the declared ID-space width, and no two overlap.
pub fn check(p: &IdPolicy) -> Vec<Violation> {
    let mut v: Vec<Violation> = p.structural.clone();

    // 1. the prefix block.
    if p.prefixes.is_empty() {
        v.push(Violation::whole("no Prefix: declarations (not an ID-policy file?)"));
    } else if !p.raw.is_empty() && !p.prefixes.iter().any(|(n, _)| n == "idrange") {
        v.push(Violation::whole(
            "the `idrange:` prefix is used by the range datatypes but never declared",
        ));
    }

    // 2. the ontology IRI.
    if p.ontology_iri.is_none() {
        v.push(Violation::whole("no `Ontology: <IRI>` frame"));
    }

    // 3. the policy header. `idprefix` + `iddigits` define the ID space new IDs
    // are minted in, and `idsfor` names the ontology they belong to; without them
    // the file allocates ranges of nothing.
    for (name, present) in [
        ("idsfor", p.idsfor.is_some()),
        ("idprefix", p.idprefix.is_some()),
        ("iddigits", p.iddigits.is_some()),
    ] {
        if !present {
            v.push(Violation::whole(format!(
                "the ontology frame declares no `{name}:` annotation"
            )));
        }
    }

    // 4. something must be allocated.
    if p.raw.is_empty() {
        v.push(Violation::whole("no `Datatype: idrange:N` frames — the policy allocates nothing"));
    }

    // 5-6. per-frame completeness.
    for r in &p.raw {
        match &r.owner {
            None => v.push(Violation::at(
                r.line,
                format!("idrange:{} has no `allocatedto:` annotation", r.id),
            )),
            Some(o) if o.trim().is_empty() => v.push(Violation::at(
                r.line,
                format!("idrange:{} has an empty `allocatedto:` annotation", r.id),
            )),
            Some(_) => {}
        }
        if r.facet.is_none() {
            match &r.bad_facet {
                Some((line, text)) => v.push(Violation::at(
                    *line,
                    format!("idrange:{} has an unreadable datatype restriction: `{text}`", r.id),
                )),
                None => v.push(Violation::at(
                    r.line,
                    format!(
                        "idrange:{} has no `EquivalentTo: xsd:integer[…]` restriction",
                        r.id
                    ),
                )),
            }
        }
    }

    // 7. duplicate range IDs. Two frames with the same name are one datatype in
    // OWL, so the second silently replaces the first — the allocation it records
    // is simply lost.
    for (i, a) in p.raw.iter().enumerate() {
        for b in &p.raw[i + 1..] {
            if a.id == b.id {
                v.push(Violation::at(
                    b.line,
                    format!("idrange:{} is declared twice (first at line {})", b.id, a.line),
                ));
            }
        }
    }

    // 8-9. bounds.
    let max = p.iddigits.map(|d| 10i64.saturating_pow(d) - 1);
    for r in &p.ranges {
        if r.low > r.high {
            v.push(Violation::at(
                r.line,
                format!("idrange:{} ({}) has low {} > high {}", r.id, r.owner, r.low, r.high),
            ));
            continue;
        }
        if r.low < 0 {
            v.push(Violation::at(
                r.line,
                format!("idrange:{} ({}) starts below zero ({})", r.id, r.owner, r.low),
            ));
        }
        if let Some(max) = max {
            if r.high > max {
                v.push(Violation::at(
                    r.line,
                    format!(
                        "idrange:{} ({}) ends at {}, outside the {}-digit ID space (max {})",
                        r.id,
                        r.owner,
                        r.high,
                        p.iddigits.unwrap_or(0),
                        max
                    ),
                ));
            }
        }
    }

    // Overlaps — two owners minting from the same numbers is the failure this
    // file exists to prevent. Ill-formed ranges are skipped (already reported).
    let usable: Vec<&IdRange> = p.ranges.iter().filter(|r| r.low <= r.high).collect();
    for (i, a) in usable.iter().enumerate() {
        for b in &usable[i + 1..] {
            if a.low <= b.high && b.low <= a.high {
                v.push(Violation::at(
                    b.line,
                    format!(
                        "idrange:{} ({}) [{}..{}] overlaps idrange:{} ({}) [{}..{}] at line {}",
                        b.id, b.owner, b.low, b.high, a.id, a.owner, a.low, a.high, a.line
                    ),
                ));
            }
        }
    }

    v.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.message.cmp(&b.message)));
    v.dedup();
    v
}

/// Parse and check in one step.
pub fn check_text(text: &str) -> Vec<Violation> {
    check(&parse(text))
}

/// Parse and check a file on disk.
pub fn check_file(path: &Path) -> Result<Vec<Violation>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading ID-ranges file {}", path.display()))?;
    Ok(check_text(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but VALID policy file, shaped exactly like the real ones.
    const GOOD: &str = r#"## ID Ranges File
Prefix: idrange: <http://purl.obolibrary.org/obo/xx/idrange/>
Prefix: allocatedto: <http://purl.obolibrary.org/obo/IAO_0000597>
Prefix: iddigits: <http://purl.obolibrary.org/obo/IAO_0000596>
Prefix: idprefix: <http://purl.obolibrary.org/obo/IAO_0000599>
Prefix: idsfor: <http://purl.obolibrary.org/obo/IAO_0000598>

Ontology: <http://purl.obolibrary.org/obo/xx/xx-idranges.owl>

Annotations:
    idprefix: "http://purl.obolibrary.org/obo/XX_",
    iddigits: 7,
    idsfor: "XX"

AnnotationProperty: allocatedto:

Datatype: idrange:1
    Annotations:
        allocatedto: "Alice"
    EquivalentTo:
        xsd:integer[>= 1, < 2000]

Datatype: idrange:2

    Annotations:
        allocatedto: "Bob"

    EquivalentTo:
        xsd:integer[>= 2001 , <= 3999]

Datatype: xsd:integer
"#;

    #[test]
    fn good_policy_passes() {
        let p = parse(GOOD);
        assert_eq!(p.ontology_iri.as_deref(), Some("http://purl.obolibrary.org/obo/xx/xx-idranges.owl"));
        assert_eq!(p.idsfor.as_deref(), Some("XX"));
        assert_eq!(p.iddigits, Some(7));
        assert_eq!(p.ranges.len(), 2);
        // `< 2000` is exclusive, so the inclusive high is 1999.
        assert_eq!(p.ranges[0], IdRange {
            id: "1".into(),
            owner: "Alice".into(),
            low: 1,
            high: 1999,
            line: 17,
        });
        assert!(check(&p).is_empty(), "{:?}", check(&p));
    }

    /// A trailing `Datatype: xsd:integer` (EFO's file ends with two) is a plain
    /// declaration, not an unowned ID range.
    #[test]
    fn non_idrange_datatype_is_not_a_range() {
        let p = parse(GOOD);
        assert_eq!(p.raw.len(), 2);
    }

    #[test]
    fn overlap_is_reported() {
        let bad = GOOD.replace(">= 2001 , <= 3999", ">= 1500 , <= 3999");
        let v = check_text(&bad);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("overlaps"), "{:?}", v[0]);
    }

    #[test]
    fn inverted_range_is_reported() {
        let bad = GOOD.replace(">= 2001 , <= 3999", ">= 3999 , <= 2001");
        let v = check_text(&bad);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("low 3999 > high 2001"), "{:?}", v[0]);
    }

    /// The four failures a line-wise scraper reports as PASS: each needs the
    /// parsed structure to see at all.
    #[test]
    fn missing_allocatedto_is_reported() {
        let bad = GOOD.replace("        allocatedto: \"Bob\"\n", "");
        let v = check_text(&bad);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("no `allocatedto:`"), "{:?}", v[0]);
    }

    #[test]
    fn missing_restriction_is_reported() {
        let bad = GOOD.replace("    EquivalentTo:\n        xsd:integer[>= 2001 , <= 3999]\n", "");
        let v = check_text(&bad);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("no `EquivalentTo:"), "{:?}", v[0]);
    }

    #[test]
    fn unreadable_restriction_is_reported() {
        let bad = GOOD.replace("xsd:integer[>= 2001 , <= 3999]", "xsd:integer[>= 2001]");
        let v = check_text(&bad);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("unreadable datatype restriction"), "{:?}", v[0]);
    }

    #[test]
    fn missing_ontology_frame_is_reported() {
        let bad = GOOD.replace("Ontology: <http://purl.obolibrary.org/obo/xx/xx-idranges.owl>\n", "");
        let v = check_text(&bad);
        // No ontology frame ⇒ no ontology IRI and none of its three annotations.
        assert_eq!(v.len(), 4, "{v:?}");
        assert!(v.iter().any(|x| x.message.contains("no `Ontology: <IRI>` frame")));
        assert!(v.iter().any(|x| x.message.contains("`idprefix:`")));
    }

    #[test]
    fn malformed_prefix_is_reported() {
        let bad = GOOD.replace(
            "Prefix: idrange: <http://purl.obolibrary.org/obo/xx/idrange/>",
            "Prefix: idrange: http://purl.obolibrary.org/obo/xx/idrange/",
        );
        let v = check_text(&bad);
        assert!(v.iter().any(|x| x.message.contains("unparseable Prefix declaration")), "{v:?}");
        assert!(v.iter().any(|x| x.message.contains("never declared")), "{v:?}");
    }

    #[test]
    fn duplicate_range_id_is_reported() {
        let bad = GOOD.replace("Datatype: idrange:2", "Datatype: idrange:1");
        let v = check_text(&bad);
        assert!(v.iter().any(|x| x.message.contains("declared twice")), "{v:?}");
    }

    /// `iddigits: 7` caps the ID space at 9999999; a range past it allocates IDs
    /// that cannot be written with the declared prefix.
    #[test]
    fn range_outside_the_id_space_is_reported() {
        let bad = GOOD.replace(">= 2001 , <= 3999", ">= 2001 , <= 10000000");
        let v = check_text(&bad);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("outside the 7-digit ID space"), "{:?}", v[0]);
    }

    /// CL's last range is `[>= 9900000, < 10000000]` — inclusive high 9999999,
    /// which is exactly the top of a 7-digit space and must NOT be flagged.
    #[test]
    fn top_of_the_id_space_is_allowed() {
        let ok = GOOD.replace(">= 2001 , <= 3999", ">= 9900000, < 10000000");
        assert!(check_text(&ok).is_empty(), "{:?}", check_text(&ok));
    }

    #[test]
    fn facet_bounds_are_normalised_to_inclusive() {
        assert_eq!(parse_integer_facet("xsd:integer[>= 1, < 2000]"), Some((1, 1999)));
        assert_eq!(parse_integer_facet("xsd:integer[> 1 , <= 499999]"), Some((2, 499999)));
        // Leading zeros (EFO writes `>= 0020000 , <= 0029999`).
        assert_eq!(parse_integer_facet("xsd:integer[>= 0020000 , <= 0029999]"), Some((20000, 29999)));
        // Not a facet at all, and a half-open one: both unusable.
        assert_eq!(parse_integer_facet("Annotations:"), None);
        assert_eq!(parse_integer_facet("xsd:integer[>= 5]"), None);
    }
}

#[cfg(test)]
mod bare_annotation_tests {
    /// Generated policy files write `Annotations:\n allocatedto: "X"`, but the
    /// bare form directly inside the `Datatype:` frame is legal Manchester
    /// shorthand and appears in hand-written policies. It has to keep the frame
    /// open: treating it as a frame terminator would drop both the owner and the
    /// integer facet, leaving the file with zero ranges and no range for
    /// `om mint` to find.
    #[test]
    fn a_bare_allocatedto_keeps_the_frame_open() {
        let txt = "Prefix: idrange: <http://x/idrange/>\n\
                   Ontology: <http://x/x-idranges.owl>\n\
                   AnnotationProperty: allocatedto:\n\
                   Datatype: idrange:31\n\
                   \x20   allocatedto: \"Automation\"\n\
                   \x20   EquivalentTo:\n\
                   \x20       xsd:integer[>= 20000, < 120000]\n";
        let p = super::parse(txt);
        assert_eq!(p.ranges.len(), 1, "expected one complete range: {:?}", p.raw);
        assert_eq!(p.ranges[0].owner, "Automation");
        assert_eq!((p.ranges[0].low, p.ranges[0].high), (20000, 119999));
    }
}
