//! `fastobo-validator` — the OBO 1.4 syntax/structure checker a repo runs over its
//! released `.obo`.
//!
//! Repos invoke it by that name from their own QC targets — `fastobo-validator
//! hp.obo` is what HPO's `test` target runs — so owlmake answers to it directly
//! rather than leaving the check to fail as a missing command.
//!
//! It deliberately does NOT reuse [`crate::io::obo::load`]. That reader is lenient
//! by design — it recovers from anything it can so a slightly-off file still yields
//! a model — and so accepts documents this must reject: a missing `id`, an unknown
//! stanza type, a line with no colon, an empty `relationship:`, a duplicate `name:`,
//! a one-argument `relationship:`. A validator built on it would pass everything,
//! which is worse than no validator: the check would look green while guarding
//! nothing.
//!
//! So this walks the document itself and applies the structural rules OBO 1.4
//! states. The three progress lines keep the shape build logs already carry — a
//! right-aligned verb then the file — so a curator scanning a log finds the
//! validation where they expect it.

use std::fmt::Write as _;
use std::io::BufRead;
use std::path::Path;
use std::time::Instant;

/// Tags that may appear at most once in a stanza (OBO 1.4 "zero or one").
/// Everything absent from this list — `synonym`, `xref`, `is_a`, `relationship`,
/// `subset`, `intersection_of`, `union_of`, `disjoint_from`, … — may repeat.
const AT_MOST_ONCE: &[&str] = &[
    "id", "name", "namespace", "def", "comment", "is_anonymous", "is_obsolete",
    "created_by", "creation_date", "builtin", "is_cyclic", "is_reflexive",
    "is_symmetric", "is_transitive", "is_anti_symmetric", "is_metadata_tag",
    "is_class_level",
    // Three tags READ as single-valued but are not, so they are deliberately absent:
    // `replaced_by` (an obsolete term may name several replacements) and
    // `domain`/`range`, all of which repeat in released HPO and OBA files. A tag
    // listed here must be single-valued in every document a release can contain, or
    // the check turns a sound build red.
];

/// Tags whose value must name at least two whitespace-separated arguments.
const TWO_ARGS: &[&str] = &["relationship", "intersection_of_relationship"];

/// `fastobo-validator <file>…`, returning the process exit code.
pub fn main(args: &[String]) -> i32 {
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if files.is_empty() {
        eprintln!("error: fastobo-validator needs at least one OBO document to validate");
        return 2;
    }
    let mut bad = false;
    for f in files {
        match validate(Path::new(f)) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("error: could not validate `{f}`\n{e}");
                bad = true;
            }
        }
    }
    i32::from(bad)
}

fn validate(path: &Path) -> Result<(), String> {
    let name = path.display();
    println!("     Parsing `{name}`");
    let started = Instant::now();
    let file = std::fs::File::open(path)
        .map_err(|e| format!("  opening {name}: {e}"))?;
    let problems = check(std::io::BufReader::new(file))?;
    println!("    Finished parsing `{name}` in {:.2}s", started.elapsed().as_secs_f64());
    if !problems.is_empty() {
        let mut s = String::new();
        for (line, msg) in &problems {
            let _ = writeln!(s, "  {name}:{line}: {msg}");
        }
        s.pop();
        return Err(s);
    }
    println!("   Completed validation of `{name}`");
    Ok(())
}

/// Walk the document, returning `(line number, message)` for every violation.
fn check<R: BufRead>(reader: R) -> Result<Vec<(usize, String)>, String> {
    let mut problems: Vec<(usize, String)> = Vec::new();
    // The stanza being read: its type, the line it opened on, and the tags seen.
    let mut stanza: Option<(String, usize)> = None;
    let mut seen: Vec<String> = Vec::new();
    let mut ids = 0usize;

    let mut close = |stanza: &Option<(String, usize)>, ids: usize, problems: &mut Vec<(usize, String)>| {
        if let Some((kind, at)) = stanza {
            if ids == 0 {
                problems.push((*at, format!("[{kind}] stanza has no `id` tag")));
            }
        }
    };

    for (i, line) in reader.lines().enumerate() {
        let lineno = i + 1;
        let line = line.map_err(|e| format!("  reading line {lineno}: {e}"))?;
        let t = line.trim();
        if t.is_empty() || t.starts_with('!') {
            continue;
        }
        if let Some(kind) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            close(&stanza, ids, &mut problems);
            if !matches!(kind, "Term" | "Typedef" | "Instance") {
                problems.push((lineno, format!("unknown stanza type `[{kind}]`")));
            }
            stanza = Some((kind.to_string(), lineno));
            seen.clear();
            ids = 0;
            continue;
        }
        // Every other line is a `tag: value` pair, in the header or a stanza.
        let Some((tag, value)) = t.split_once(':') else {
            problems.push((lineno, format!("expected `tag: value`, found `{t}`")));
            continue;
        };
        let tag = tag.trim();
        // Strip a trailing `! comment`, which is a comment and not part of the value.
        let value = value.split('!').next().unwrap_or("").trim();
        if tag.is_empty() {
            problems.push((lineno, "empty tag name".to_string()));
            continue;
        }
        if stanza.is_none() {
            continue; // header tags carry no stanza-level rules
        }
        if value.is_empty() {
            problems.push((lineno, format!("tag `{tag}` has no value")));
            continue;
        }
        if tag == "id" {
            ids += 1;
        }
        if AT_MOST_ONCE.contains(&tag) && seen.iter().any(|s| s == tag) {
            problems.push((lineno, format!("tag `{tag}` may appear at most once per stanza")));
        }
        if TWO_ARGS.contains(&tag) && value.split_whitespace().count() < 2 {
            problems.push((
                lineno,
                format!("tag `{tag}` needs a relation and a target, found `{value}`"),
            ));
        }
        seen.push(tag.to_string());
    }
    close(&stanza, ids, &mut problems);
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probs(doc: &str) -> Vec<String> {
        check(doc.as_bytes()).unwrap().into_iter().map(|(l, m)| format!("{l}: {m}")).collect()
    }

    #[test]
    fn a_well_formed_document_passes() {
        assert!(probs("format-version: 1.2\n\n[Term]\nid: HP:1\nname: x\nis_a: HP:2 ! y\n").is_empty());
    }

    #[test]
    fn a_stanza_without_an_id_is_rejected() {
        assert_eq!(probs("[Term]\nname: x\n").len(), 1);
    }

    #[test]
    fn an_unknown_stanza_type_is_rejected() {
        assert!(probs("[Bogus]\nid: HP:1\n")[0].contains("unknown stanza type"));
    }

    #[test]
    fn a_line_without_a_colon_is_rejected() {
        assert!(probs("[Term]\nid: HP:1\nthis has no colon\n")[0].contains("expected `tag: value`"));
    }

    #[test]
    fn an_empty_value_is_rejected() {
        assert!(probs("[Term]\nid: HP:1\nrelationship: \n")[0].contains("has no value"));
    }

    #[test]
    fn a_repeated_single_cardinality_tag_is_rejected() {
        assert!(probs("[Term]\nid: HP:1\nname: a\nname: b\n")[0].contains("at most once"));
    }

    #[test]
    fn a_relationship_needs_two_arguments() {
        assert!(probs("[Term]\nid: HP:1\nrelationship: part_of\n")[0].contains("needs a relation"));
    }

    #[test]
    fn a_repeatable_tag_may_repeat() {
        assert!(probs("[Term]\nid: HP:1\nsynonym: \"a\" EXACT []\nsynonym: \"b\" EXACT []\n").is_empty());
    }

    #[test]
    fn a_trailing_comment_is_not_part_of_the_value() {
        assert!(probs("[Term]\nid: HP:1\nis_a: HP:2 ! label\n").is_empty());
    }
}
