//! `explain --axiom`: the axiom read as a Manchester-syntax axiom whose names
//! are the names the ontology gives its classes.
//!
//! The text is split into tokens as the Manchester tokenizer splits it, and a
//! name is looked up as the quoted-entity checker looks one up: among the
//! names of the ontology's classes as written, then without a single quote at
//! either end, then without a double quote either; else as the IRI the command
//! line's context ([`crate::context`]) makes of it. A class goes by the short
//! form of its IRI and by each `rdfs:label` it has.
//!
//! What is explained is a subsumption between two named classes, either of
//! them in parentheses, and the keyword is `SubClassOf`, with or without its
//! colon, in any case. An axiom of another kind, or with a class expression on
//! either side, is refused.

use std::collections::HashMap;

use anyhow::bail;
use horned_owl::model::{AnnotationSubject, AnnotationValue, Component};

use crate::io::entities::Kind;
use crate::model::Model;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// The subclass and superclass of the `SubClassOf` axiom `axiom` states, as
/// IRIs.
pub(crate) fn subsumption(model: &Model, axiom: &str) -> anyhow::Result<(String, String)> {
    let tokens = tokens(axiom)?;
    let names = Names::of(model);
    let mut reader = Reader { tokens: &tokens, at: 0, names: &names, model };
    let sub = reader.operand()?;
    match reader.next() {
        Some(k) if either_form(k, "SubClassOf") => {}
        Some(k) if either_form(k, "EquivalentTo") || either_form(k, "DisjointWith") => {
            bail!("--axiom: only a SubClassOf axiom is explained, not {k}")
        }
        Some(k) => bail!("--axiom: encountered {k} where SubClassOf was expected"),
        None => bail!("--axiom: SubClassOf is missing"),
    }
    let sup = reader.operand()?;
    if let Some(t) = reader.next() {
        bail!("--axiom: encountered {t} after the superclass, where the axiom ends");
    }
    Ok((sub, sup))
}

/// Whether `token` is `keyword`, or `keyword` with a colon after it, in any
/// case.
fn either_form(token: &str, keyword: &str) -> bool {
    token.eq_ignore_ascii_case(keyword)
        || token.strip_suffix(':').is_some_and(|t| t.eq_ignore_ascii_case(keyword))
}

struct Reader<'a> {
    tokens: &'a [String],
    at: usize,
    names: &'a Names,
    model: &'a Model,
}

impl<'a> Reader<'a> {
    fn next(&mut self) -> Option<&'a str> {
        let t = self.tokens.get(self.at)?;
        self.at += 1;
        Some(t)
    }

    /// A named class, alone or in parentheses, that no `and`, `that` or `or`
    /// goes on from.
    fn operand(&mut self) -> anyhow::Result<String> {
        let Some(token) = self.next() else {
            bail!("--axiom: a class is missing at its end")
        };
        let iri = if let Some(iri) = self.names.class(self.model, token) {
            iri
        } else if token == "(" {
            let iri = self.operand()?;
            match self.next() {
                Some(")") => iri,
                Some(t) => bail!("--axiom: encountered {t} where ) was expected"),
                None => bail!("--axiom: ) is missing"),
            }
        } else if ["not", "inverse"].iter().any(|k| token.eq_ignore_ascii_case(k)) || token == "{" {
            bail!("--axiom: {token} starts a class expression, and only an axiom between two named classes is explained")
        } else {
            bail!("--axiom: {token} names no class")
        };
        if let Some(t) = self.tokens.get(self.at) {
            if ["and", "that", "or"].iter().any(|k| t.eq_ignore_ascii_case(k)) {
                bail!("--axiom: {t} makes a class expression of the class before it, and only an axiom between two named classes is explained")
            }
        }
        Ok(iri)
    }
}

/// The tokens of `text` as the Manchester tokenizer makes them: runs of
/// characters between white space (space, tab, line feed, carriage return) and
/// the delimiters `( ) [ ] { } , ^ > = ?`, each delimiter a token of its own and
/// `@` the start of one; a quoted name `'…'` or a string `"…"`, quotes included,
/// where a backslash keeps a quote or a backslash after it as it is; a `<…>`
/// with no white space in it, which drops the token it interrupts; a backslash
/// outside quotes standing for the character after it, which then opens no
/// quote; and a `#` or `*` starting a comment that runs to the end of the line.
fn tokens(text: &str) -> anyhow::Result<Vec<String>> {
    fn end(token: &mut String, out: &mut Vec<String>) {
        if !token.is_empty() {
            out.push(std::mem::take(token));
        }
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut token = String::new();
    let mut pos = 0;
    let mut last = ' ';
    while pos < chars.len() {
        let mut c = chars[pos];
        pos += 1;
        if c == '\\' {
            last = c;
            let Some(&escaped) = chars.get(pos) else {
                bail!("--axiom ends in a backslash")
            };
            c = escaped;
            pos += 1;
        }
        if (c == '"' || c == '\'') && last != '\\' {
            token.push(c);
            while pos < chars.len() {
                let ch = chars[pos];
                pos += 1;
                if ch == '\\' {
                    // A backslash escapes only a character something follows.
                    if pos + 1 < chars.len() {
                        let escaped = chars[pos];
                        pos += 1;
                        if !matches!(escaped, '"' | '\'' | '\\') {
                            token.push(ch);
                        }
                        token.push(escaped);
                    } else {
                        token.push('\\');
                    }
                } else {
                    token.push(ch);
                    if ch == c {
                        break;
                    }
                }
            }
            end(&mut token, &mut out);
        } else if c == '<' {
            token = String::from("<");
            let start = pos;
            while pos < chars.len() {
                let ch = chars[pos];
                pos += 1;
                if crate::dosdp::java::is_whitespace(ch) {
                    // Not an IRI: `<` alone, and the text after it read again.
                    pos = start;
                    token = String::from("<");
                    end(&mut token, &mut out);
                    break;
                }
                token.push(ch);
                if ch == '>' {
                    end(&mut token, &mut out);
                    break;
                }
            }
        } else if matches!(c, ' ' | '\n' | '\r' | '\t') {
            end(&mut token, &mut out);
        } else if matches!(c, '#' | '*') {
            end(&mut token, &mut out);
            while pos < chars.len() {
                pos += 1;
                if chars[pos - 1] == '\n' {
                    break;
                }
            }
        } else if "()[],{}^@>=?".contains(c) {
            end(&mut token, &mut out);
            token.push(c);
            if c != '@' {
                end(&mut token, &mut out);
            }
        } else {
            token.push(c);
        }
        last = c;
    }
    end(&mut token, &mut out);
    Ok(out)
}

/// The names of the ontology's classes, each with the IRI of the class it
/// names. A class goes by the short form of its IRI and by each `rdfs:label`
/// it has, and the classes are taken in the order a hash set of the
/// ontology's entities iterates in, so that of two classes with one name the
/// later has it.
struct Names {
    classes: HashMap<String, String>,
}

impl Names {
    fn of(model: &Model) -> Names {
        let signature = crate::io::entities::signature(model);
        let cap = crate::cmd::explain_blackbox::presized_capacity(16, signature.len());
        let bucket = |iri: &str| crate::cmd::explain_blackbox::bucket(crate::owlapi_hash::class_hash(iri), cap);
        let mut order: Vec<&str> =
            signature.iter().filter(|(k, _)| *k == Kind::Class).map(|(_, iri)| iri.as_str()).collect();
        order.sort_by(|a, b| bucket(a).cmp(&bucket(b)).then_with(|| crate::owlapi_hash::iri_cmp(a, b)));
        let mut labels: HashMap<&str, Vec<&str>> = HashMap::new();
        for ac in model.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(l)) = (&aa.subject, &aa.ann.av) {
                    if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                        labels.entry(s.as_ref()).or_default().push(l.literal());
                    }
                }
            }
        }
        let mut classes = HashMap::new();
        for c in order {
            classes.insert(crate::owlapi_hash::iri_short_form(c), c.to_string());
            for l in labels.get(c).into_iter().flatten() {
                classes.insert(l.to_string(), c.to_string());
            }
        }
        Names { classes }
    }

    /// The IRI of the class `name` names.
    fn class(&self, model: &Model, name: &str) -> Option<String> {
        let unquoted = |name: &'_ str, quote: char| {
            let name = crate::java_number::trim(name);
            let name = name.strip_prefix(quote).unwrap_or(name);
            name.strip_suffix(quote).unwrap_or(name).to_string()
        };
        let single = unquoted(name, '\'');
        let double = unquoted(&single, '"');
        let named = [name, single.as_str(), double.as_str()].into_iter().find_map(|n| self.classes.get(n).cloned());
        named.or_else(|| model.context.iri(name))
    }
}

#[cfg(test)]
mod tests {
    use super::{tokens, Names};

    /// Of two classes with one name, the one later in the order a hash set of
    /// the ontology's entities iterates in has it. Each `C_k` of the fixture is
    /// one class's short form and another's label; the class each goes to is
    /// the one ROBOT 1.9.11's quoted-entity checker gives it.
    #[test]
    fn a_name_two_classes_share_goes_to_the_one_robot_gives_it() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/robot-1.9.11/explain-name-collisions.ofn");
        let model = crate::io::load(&path).unwrap();
        let names = Names::of(&model);
        let robot = [
            36, 2, 34, 4, 9, 6, 7, 15, 9, 10, 11, 12, 13, 14, 15, 16, 13, 18, 19, 8, 35, 30, 23, 32, 19, 26, 38, 37, 1,
            16, 14, 33, 33, 34, 35, 36, 37, 18, 39, 3,
        ];
        for (k, class) in robot.iter().enumerate() {
            let name = format!("C_{}", k + 1);
            let want = format!("http://purl.obolibrary.org/obo/C_{class}");
            assert_eq!(names.class(&model, &name).as_deref(), Some(want.as_str()), "{name}");
        }
    }

    #[test]
    fn tokens_split_as_the_manchester_tokenizer_splits() {
        let t = |s: &str| tokens(s).unwrap();
        assert_eq!(t("'alpha one' SubClassOf: beta"), ["'alpha one'", "SubClassOf:", "beta"]);
        assert_eq!(t("(X_1) SubClassOf X_4 # a comment"), ["(", "X_1", ")", "SubClassOf", "X_4"]);
        assert_eq!(t("\"a \\\"b\\\"\" c"), ["\"a \"b\"\"", "c"]);
        assert_eq!(t("ab<http://x.org/y> c"), ["<http://x.org/y>", "c"]);
        assert_eq!(t("a <b c>"), ["a", "<", "b", "c", ">"]);
        assert_eq!(t("a*b c\nd"), ["a", "d"]);
        assert_eq!(t("x@en y"), ["x", "@en", "y"]);
        assert_eq!(t("\\'a b"), ["'a", "b"]);
    }
}
