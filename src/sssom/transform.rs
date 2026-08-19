//! A minimal SSSOM/Transform (SSSOM/T) engine — the subset of the DSL that OBO
//! ontology builds, notably UBERON's mapping pipeline, rely on.
//!
//! The DSL is `FILTER -> ACTION`. UBERON uses:
//!   * filters `subject==PREFIX:*`, `object==PREFIX:*` (and `!`, `||`, `&&`),
//!   * actions `invert()`, `include()`, `stop()`.
//!
//! Processing model: each mapping is run through the rule list in order. A
//! matching rule's action fires:
//!   * `invert()` rewrites the mapping in place (subject↔object, predicate
//!     inverted, directional slots swapped) and processing continues,
//!   * `include()` marks the (possibly transformed) mapping for output,
//!   * `stop()` drops the mapping and halts its processing.
//! A mapping never `include()`d is dropped, unless `--include-all` appends a
//! trailing unconditional include.

use crate::sssom::{invert_column_name, MappingSet, INVERSE_PREDICATE_MAP};

/// A SSSOM/T field test. `field` is a mapping slot (`subject`/`object`/
/// `predicate`/…) mapped to its `_id` column; `pattern` may end in `*`.
#[derive(Debug, Clone)]
pub enum Filter {
    /// `FIELD==VALUE` (with optional trailing `*` wildcard on VALUE).
    Eq { column: String, pattern: String },
    Not(Box<Filter>),
    And(Box<Filter>, Box<Filter>),
    Or(Box<Filter>, Box<Filter>),
    /// The always-true filter (a bare `include()` / `--include-all`).
    True,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Invert,
    Include,
    Stop,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub filter: Filter,
    pub action: Action,
}

impl Filter {
    /// Does this filter match `m`? CURIE columns compare on the stored value
    /// (SSSOM sets keep entity references as CURIEs); a trailing `*` is a prefix
    /// match, otherwise equality.
    fn matches(&self, m: &crate::sssom::Mapping) -> bool {
        match self {
            Filter::True => true,
            Filter::Not(f) => !f.matches(m),
            Filter::And(a, b) => a.matches(m) && b.matches(m),
            Filter::Or(a, b) => a.matches(m) || b.matches(m),
            Filter::Eq { column, pattern } => {
                let v = m.get(column).map(String::as_str).unwrap_or("");
                if let Some(prefix) = pattern.strip_suffix('*') {
                    v.starts_with(prefix)
                } else {
                    v == pattern
                }
            }
        }
    }
}

/// Map a SSSOM/T field name to the mapping column it tests.
fn field_column(field: &str) -> String {
    match field {
        "subject" => "subject_id".to_string(),
        "object" => "object_id".to_string(),
        "predicate" => "predicate_id".to_string(),
        "justification" | "mapping_justification" => "mapping_justification".to_string(),
        other => {
            if other.ends_with("_id") || other.contains('_') {
                other.to_string()
            } else {
                format!("{other}_id")
            }
        }
    }
}

/// Invert a single mapping in place: swap subject/object slots (renaming the
/// directional columns via [`invert_column_name`]) and invert the predicate.
/// `None` when the predicate has no declared inverse: such a mapping cannot be
/// inverted, and `invert()` DROPS it rather than emitting it the wrong way round
/// or silently unchanged. Passing it through unchanged left the two
/// `semapv:crossSpeciesCloseMatch` rows FBbt asserts in UBERON's merged mapping
/// set, facing the direction the rule exists to reverse.
pub fn invert_mapping(m: &crate::sssom::Mapping) -> Option<crate::sssom::Mapping> {
    let mut out = crate::sssom::Mapping::new();
    for (k, v) in m {
        let nk = invert_column_name(k).map(str::to_string).unwrap_or_else(|| k.clone());
        if k == "predicate_id" {
            let inv = INVERSE_PREDICATE_MAP
                .iter()
                .find(|(p, _)| *p == v.as_str())
                .map(|(_, q)| q.to_string())?;
            out.insert(nk, inv);
        } else {
            out.insert(nk, v.clone());
        }
    }
    Some(out)
}

/// Run the SSSOM/T rule list over the set's mappings, replacing them with the
/// transformed / retained records. `include_all` appends a trailing
/// unconditional `include()`.
pub fn apply(ms: &mut MappingSet, rules: &[Rule], include_all: bool) {
    let mut effective: Vec<Rule> = rules.to_vec();
    if include_all {
        effective.push(Rule { filter: Filter::True, action: Action::Include });
    }
    // With NO rules at all, sssom-cli is a pure converter and every mapping passes
    // through — only once a rule exists does a mapping have to be `Include`d to
    // survive. Without this, UBERON's second pipeline stage
    // (`sssom-cli --mangle-iris obo --output $@`, which has no rules) dropped all
    // 282 mappings and wrote a 716-byte release artefact.
    if effective.is_empty() {
        return;
    }
    let mut out: Vec<crate::sssom::Mapping> = Vec::new();
    for m in &ms.mappings {
        let mut cur = m.clone();
        let mut included = false;
        for r in &effective {
            if !r.filter.matches(&cur) {
                continue;
            }
            match r.action {
                Action::Invert => match invert_mapping(&cur) {
                    Some(inv) => cur = inv,
                    // Not invertible ⇒ dropped, like an explicit `stop()`.
                    None => {
                        included = false;
                        break;
                    }
                },
                Action::Include => {
                    included = true;
                }
                Action::Stop => {
                    included = false;
                    break;
                }
            }
        }
        if included {
            out.push(cur);
        }
    }
    ms.mappings = out;
    ms.recompute_columns();
}

// ───────────────────────────── rule parsing ─────────────────────────────

/// Parse a `FILTER -> ACTION` rule string.
pub fn parse_rule(s: &str) -> anyhow::Result<Rule> {
    let (lhs, rhs) = s
        .split_once("->")
        .ok_or_else(|| anyhow::anyhow!("SSSOM/T rule missing `->`: {s}"))?;
    let filter = parse_filter(lhs.trim())?;
    let action = parse_action(rhs.trim())?;
    Ok(Rule { filter, action })
}

fn parse_action(s: &str) -> anyhow::Result<Action> {
    let s = s.trim().trim_end_matches(';').trim();
    match s {
        "invert()" | "invert" => Ok(Action::Invert),
        "include()" | "include" => Ok(Action::Include),
        "stop()" | "stop" => Ok(Action::Stop),
        other => anyhow::bail!("unsupported SSSOM/T action: {other}"),
    }
}

/// Parse a filter expression with `||` (lowest precedence), `&&`, `!`, `()`,
/// and `FIELD==VALUE` atoms.
pub fn parse_filter(s: &str) -> anyhow::Result<Filter> {
    let toks = lex_filter(s);
    let mut p = FilterParser { toks: &toks, pos: 0 };
    let f = p.parse_or()?;
    if p.pos != p.toks.len() {
        anyhow::bail!("trailing tokens in SSSOM/T filter: {s}");
    }
    Ok(f)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Or,
    And,
    Not,
    LParen,
    RParen,
    Atom(String),
}

fn lex_filter(s: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut atom = String::new();
    let flush = |atom: &mut String, toks: &mut Vec<Tok>| {
        let a = atom.trim();
        if !a.is_empty() {
            toks.push(Tok::Atom(a.to_string()));
        }
        atom.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '|' if i + 1 < chars.len() && chars[i + 1] == '|' => {
                flush(&mut atom, &mut toks);
                toks.push(Tok::Or);
                i += 2;
            }
            '&' if i + 1 < chars.len() && chars[i + 1] == '&' => {
                flush(&mut atom, &mut toks);
                toks.push(Tok::And);
                i += 2;
            }
            '(' => {
                flush(&mut atom, &mut toks);
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                flush(&mut atom, &mut toks);
                toks.push(Tok::RParen);
                i += 1;
            }
            '!' => {
                flush(&mut atom, &mut toks);
                toks.push(Tok::Not);
                i += 1;
            }
            _ => {
                atom.push(c);
                i += 1;
            }
        }
    }
    flush(&mut atom, &mut toks);
    toks
}

struct FilterParser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl FilterParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn parse_or(&mut self) -> anyhow::Result<Filter> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.pos += 1;
            let right = self.parse_and()?;
            left = Filter::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn parse_and(&mut self) -> anyhow::Result<Filter> {
        let mut left = self.parse_unary()?;
        while matches!(self.peek(), Some(Tok::And)) {
            self.pos += 1;
            let right = self.parse_unary()?;
            left = Filter::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn parse_unary(&mut self) -> anyhow::Result<Filter> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.pos += 1;
            return Ok(Filter::Not(Box::new(self.parse_unary()?)));
        }
        match self.peek() {
            Some(Tok::LParen) => {
                self.pos += 1;
                let f = self.parse_or()?;
                if !matches!(self.peek(), Some(Tok::RParen)) {
                    anyhow::bail!("missing `)` in SSSOM/T filter");
                }
                self.pos += 1;
                Ok(f)
            }
            Some(Tok::Atom(a)) => {
                let a = a.clone();
                self.pos += 1;
                parse_atom(&a)
            }
            other => anyhow::bail!("unexpected token in SSSOM/T filter: {other:?}"),
        }
    }
}

fn parse_atom(a: &str) -> anyhow::Result<Filter> {
    let (field, value) = a
        .split_once("==")
        .ok_or_else(|| anyhow::anyhow!("SSSOM/T atom missing `==`: {a}"))?;
    Ok(Filter::Eq {
        column: field_column(field.trim()),
        pattern: value.trim().to_string(),
    })
}
