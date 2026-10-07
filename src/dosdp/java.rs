//! The text dialects a DOSDP pattern is written in: its printf text is a
//! `java.util.Formatter` format, its substitutions are `java.util.regex`
//! patterns whose replacements follow `Matcher.appendReplacement`, and the
//! literals of its logical axioms take Java's number syntax and printing
//! ([`crate::java_number`]).
//!
//! Strings are measured in UTF-16 code units wherever the dialect measures
//! them (a `%.3s` precision, a `%5s` width).

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::OnceLock;

mod names;
mod unicode;

// ── java.util.Formatter ─────────────────────────────────────────────────────

/// One piece of a parsed format: text to copy, or a specifier.
enum Piece<'a> {
    Text(&'a str),
    Spec(Spec),
}

/// Which argument a specifier prints.
#[derive(Clone, Copy)]
enum ArgIndex {
    /// `%%` and `%n` print no argument.
    None,
    /// The next argument in order.
    Ordinary,
    /// The argument the previous specifier printed (`%<s`).
    Previous,
    /// `%N$s`: argument N, from 1.
    Explicit(usize),
}

struct Spec {
    text: String,
    index: ArgIndex,
    left_justify: bool,
    uppercase: bool,
    width: Option<usize>,
    precision: Option<usize>,
    conversion: char,
}

/// `fmt` formatted with `args`, every one a string, in `java.util.Formatter`'s
/// syntax: `%[index$][flags][width][.precision]conversion`. `%s` writes its
/// argument, `%b` writes `true`, `%h` the argument's hash code in hex, each
/// upper-cased in its capital form (`%S`); `%%` writes a percent sign and `%n`
/// a newline. Any other conversion takes no string, and like a missing
/// argument or a malformed specifier it is an error naming the specifier.
pub(crate) fn format(fmt: &str, args: &[&str]) -> Result<String, String> {
    let pieces = parse_format(fmt)?;
    let mut out = String::new();
    let mut last: Option<usize> = None;
    let mut ordinary: Option<usize> = None;
    for piece in pieces {
        let spec = match piece {
            Piece::Text(t) => {
                out.push_str(t);
                continue;
            }
            Piece::Spec(spec) => spec,
        };
        let missing = || format!("no argument for the format specifier `{}`", spec.text);
        let arg = match spec.index {
            ArgIndex::None => None,
            ArgIndex::Ordinary => {
                let i = ordinary.map_or(0, |o| o + 1);
                ordinary = Some(i);
                last = Some(i);
                Some(*args.get(i).ok_or_else(missing)?)
            }
            ArgIndex::Previous => Some(*last.and_then(|i| args.get(i)).ok_or_else(missing)?),
            ArgIndex::Explicit(n) => {
                last = Some(n - 1);
                Some(*args.get(n - 1).ok_or_else(missing)?)
            }
        };
        let text = match (spec.conversion, arg) {
            ('%', _) => "%".to_string(),
            ('n', _) => {
                out.push('\n');
                continue;
            }
            ('s', Some(a)) => a.to_string(),
            ('b', Some(_)) => "true".to_string(),
            ('h', Some(a)) => format!("{:x}", string_hash(a) as u32),
            (c, _) => {
                return Err(format!("the format specifier `{}`: a string takes no `{c}` conversion", spec.text))
            }
        };
        let text = match spec.precision {
            Some(p) if utf16_len(&text) > p => utf16_prefix(&text, p),
            _ => text,
        };
        let text = if spec.uppercase { text.to_uppercase() } else { text };
        let pad = spec.width.map_or(0, |w| w.saturating_sub(utf16_len(&text)));
        if spec.left_justify {
            out.push_str(&text);
            out.extend(std::iter::repeat_n(' ', pad));
        } else {
            out.extend(std::iter::repeat_n(' ', pad));
            out.push_str(&text);
        }
    }
    Ok(out)
}

/// The conversions a specifier may name.
fn is_conversion(c: char) -> bool {
    matches!(
        c,
        'b' | 'B' | 'h' | 'H' | 's' | 'S' | 'c' | 'C' | 'd' | 'o' | 'x' | 'X' | 'e' | 'E' | 'g'
            | 'G' | 'f' | 'a' | 'A' | 'n' | '%'
    )
}

fn parse_format(fmt: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut pieces = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        let Some(n) = fmt[i..].find('%').map(|n| i + n) else {
            pieces.push(Piece::Text(&fmt[i..]));
            break;
        };
        if n > i {
            pieces.push(Piece::Text(&fmt[i..n]));
        }
        let rest = &fmt[n + 1..];
        let Some(c) = rest.chars().next() else {
            return Err("the format ends with a lone `%`".to_string());
        };
        if is_conversion(c) {
            let lower = c.to_ascii_lowercase();
            pieces.push(Piece::Spec(Spec {
                text: format!("%{c}"),
                index: if matches!(c, '%' | 'n') { ArgIndex::None } else { ArgIndex::Ordinary },
                left_justify: false,
                uppercase: c.is_ascii_uppercase(),
                width: None,
                precision: None,
                conversion: lower,
            }));
            i = n + 1 + c.len_utf8();
            continue;
        }
        let (spec, len) = parse_specifier(rest).ok_or_else(|| format!("`%{c}` is not a format specifier"))?;
        pieces.push(Piece::Spec(spec?));
        i = n + 1 + len;
    }
    Ok(pieces)
}

/// The specifier at the start of `s` (after its `%`), and its length; none
/// when `s` does not start with one.
fn parse_specifier(s: &str) -> Option<(Result<Spec, String>, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    // `%N$`
    let mut explicit = None;
    let d = digits(0);
    if d > 0 && b.get(d) == Some(&b'$') {
        explicit = Some(&s[..d]);
        i = d + 1;
    }
    let flags_start = i;
    while i < b.len() && b"-#+ 0,(<".contains(&b[i]) {
        i += 1;
    }
    let flags = &s[flags_start..i];
    let w = digits(i);
    let width = (w > 0).then(|| &s[i..i + w]);
    i += w;
    let mut precision = None;
    if b.get(i) == Some(&b'.') && digits(i + 1) > 0 {
        let p = digits(i + 1);
        precision = Some(&s[i + 1..i + 1 + p]);
        i += 1 + p;
    }
    let date_time = matches!(b.get(i), Some(b't' | b'T')) && b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'%');
    if date_time {
        i += 1;
    }
    let c = *b.get(i).filter(|c| c.is_ascii_alphabetic() || **c == b'%')? as char;
    i += 1;
    let text = format!("%{}", &s[..i]);
    let spec = (|| {
        if date_time {
            return Err(format!("the format specifier `{text}`: a string takes no date or time conversion"));
        }
        if !is_conversion(c) {
            return Err(format!("`%{c}` is not a format conversion"));
        }
        let index = match explicit {
            Some(n) => match n.parse::<usize>() {
                Ok(n) if n > 0 => ArgIndex::Explicit(n),
                _ => return Err(format!("the format specifier `{text}` names no argument")),
            },
            None => ArgIndex::Ordinary,
        };
        let mut seen = String::new();
        for f in flags.chars() {
            if seen.contains(f) {
                return Err(format!("the format specifier `{text}` repeats the flag `{f}`"));
            }
            seen.push(f);
        }
        let index = if seen.contains('<') { ArgIndex::Previous } else { index };
        let parse_n = |s: Option<&str>| -> Result<Option<usize>, String> {
            s.map(|s| s.parse::<i32>().map(|n| n as usize).map_err(|_| format!("the format specifier `{text}` is too wide")))
                .transpose()
        };
        let width = parse_n(width)?;
        let precision = parse_n(precision)?;
        let left_justify = seen.contains('-');
        let lower = c.to_ascii_lowercase();
        let bad = |f: char| Err(format!("the format specifier `{text}` takes no `{f}` flag"));
        match lower {
            '%' => {
                if precision.is_some() {
                    return Err(format!("the format specifier `{text}` takes no precision"));
                }
                if let Some(f) = seen.chars().find(|f| *f != '-') {
                    return bad(f);
                }
            }
            'n' => {
                if width.is_some() || precision.is_some() {
                    return Err(format!("the format specifier `{text}` takes no width or precision"));
                }
                if let Some(f) = seen.chars().next() {
                    return bad(f);
                }
            }
            's' | 'b' | 'h' => {
                if let Some(f) = seen.chars().find(|f| "+ 0,(".contains(*f)) {
                    return bad(f);
                }
                // `#` asks a string for an alternate form it does not have.
                if seen.contains('#') {
                    return bad('#');
                }
            }
            _ => {}
        }
        if left_justify && width.is_none() {
            return Err(format!("the format specifier `{text}` left-justifies with no width"));
        }
        Ok(Spec {
            text: text.clone(),
            index: if matches!(lower, '%' | 'n') { ArgIndex::None } else { index },
            left_justify,
            uppercase: c.is_ascii_uppercase(),
            width,
            precision,
            conversion: lower,
        })
    })();
    Some((spec, i))
}

/// The string's hash code: `31 * h + c` over its UTF-16 code units.
pub(crate) fn string_hash(s: &str) -> i32 {
    s.encode_utf16().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The longest prefix of `s` at most `n` UTF-16 code units long.
fn utf16_prefix(s: &str, n: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        used += c.len_utf16();
        if used > n {
            break;
        }
        out.push(c);
    }
    out
}

// ── java.util.regex ─────────────────────────────────────────────────────────

/// A `java.util.regex` pattern, matched as Java matches it: `\w`, `\d`, `\s`
/// and `\b` are ASCII, `.` stops at any line terminator, `$` also matches
/// before a final line terminator, and case-insensitive matching folds ASCII
/// letters only unless the pattern asks for Unicode case (`(?u)`). What a
/// character is, its case and its type in a grapheme cluster are Java 21's
/// (Unicode 15.0), from the tables in `unicode`.
#[derive(Debug)]
pub(crate) struct Regex {
    inner: fancy_regex::Regex,
    groups: usize,
    names: HashMap<String, usize>,
}

/// A match: the text it was found in and each group's span.
pub(crate) struct Match<'t> {
    text: &'t str,
    spans: Vec<Option<(usize, usize)>>,
}

impl Match<'_> {
    /// The text group `i` matched; none when it took no part in the match.
    pub(crate) fn group(&self, i: usize) -> Option<&str> {
        self.spans.get(i).copied().flatten().map(|(s, e)| &self.text[s..e])
    }

    pub(crate) fn start(&self) -> usize {
        self.spans[0].map_or(0, |(s, _)| s)
    }

    pub(crate) fn end(&self) -> usize {
        self.spans[0].map_or(0, |(_, e)| e)
    }
}

impl Regex {
    pub(crate) fn new(pattern: &str) -> Result<Regex, String> {
        let mut t = Translator::new(pattern);
        t.translate()?;
        let mut builder = fancy_regex::RegexBuilder::new(&t.out);
        builder.backtrack_limit(10_000_000);
        for callout in t.callouts {
            match callout {
                Callout::Grapheme => builder.callout(|text, at, _| Ok(grapheme::cluster_end(text, at))),
                Callout::GraphemeBoundary => {
                    builder.zero_width_callout(|text, at, _| grapheme::is_boundary(text, at, LAST_END.get()))
                }
                Callout::From(units) => builder.zero_width_callout(move |text, at, _| Ok(utf16_len(&text[..at]) >= units)),
                Callout::Backref { group, unicode_case } => builder.callout(move |text, at, span| {
                    Ok(span(group).and_then(|(lo, hi)| backref_end(text, at, text.get(lo..hi)?, unicode_case)))
                }),
            };
        }
        let inner =
            builder.build().map_err(|e| format!("`{pattern}` is not a regular expression this reads: {e}"))?;
        Ok(Regex { inner, groups: t.groups, names: t.names })
    }

    /// The number of capturing groups.
    pub(crate) fn group_count(&self) -> usize {
        self.groups
    }

    /// The first match in `text`.
    pub(crate) fn find<'t>(&self, text: &'t str) -> Result<Option<Match<'t>>, String> {
        self.find_from(text, 0, 0)
    }

    /// The first match in `text` that starts at byte `from` or later, where
    /// the matcher's previous match ended at byte `last`.
    fn find_from<'t>(&self, text: &'t str, from: usize, last: usize) -> Result<Option<Match<'t>>, String> {
        LAST_END.set(last);
        let caps = self.inner.captures_from_pos(text, from).map_err(|e| format!("matching `{text}`: {e}"))?;
        Ok(caps.map(|c| Match {
            text,
            spans: (0..=self.groups).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect(),
        }))
    }

    /// Every match in `text`, left to right, each found from where the last
    /// ended (a character further on after an empty match).
    pub(crate) fn find_all<'t>(&self, text: &'t str) -> Result<Vec<Match<'t>>, String> {
        let mut out: Vec<Match<'t>> = Vec::new();
        let mut from = 0;
        while from <= text.len() {
            let last = out.last().map_or(0, Match::end);
            let Some(m) = self.find_from(text, from, last)? else { break };
            from = if m.start() == m.end() {
                m.end() + text[m.end()..].chars().next().map_or(1, char::len_utf8)
            } else {
                m.end()
            };
            out.push(m);
        }
        Ok(out)
    }

    /// `replacement` expanded against match `m` of this pattern: `$N` (the
    /// longest group number this pattern has) and `${name}` stand for what that
    /// group matched, and `\c` for the character `c`.
    pub(crate) fn expand(&self, m: &Match, replacement: &str) -> Result<String, String> {
        let r: Vec<char> = replacement.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < r.len() {
            match r[i] {
                '\\' => {
                    i += 1;
                    let c = *r.get(i).ok_or_else(|| format!("the replacement `{replacement}` ends with `\\`"))?;
                    out.push(c);
                    i += 1;
                }
                '$' => {
                    i += 1;
                    let c = *r
                        .get(i)
                        .ok_or_else(|| format!("the replacement `{replacement}` ends with `$`"))?;
                    let group = if c == '{' {
                        i += 1;
                        let start = i;
                        while i < r.len() && r[i].is_ascii_alphanumeric() {
                            i += 1;
                        }
                        let name: String = r[start..i].iter().collect();
                        if name.is_empty() || r.get(i) != Some(&'}') || name.starts_with(|c: char| c.is_ascii_digit()) {
                            return Err(format!("the replacement `{replacement}` names no group"));
                        }
                        i += 1;
                        *self
                            .names
                            .get(&name)
                            .ok_or_else(|| format!("the replacement `{replacement}`: no group is named `{name}`"))?
                    } else {
                        let mut n = c.to_digit(10).ok_or_else(|| {
                            format!("the replacement `{replacement}`: `${c}` is not a group reference")
                        })? as usize;
                        i += 1;
                        while let Some(d) = r.get(i).and_then(|c| c.to_digit(10)) {
                            let longer = n * 10 + d as usize;
                            if longer > self.groups {
                                break;
                            }
                            n = longer;
                            i += 1;
                        }
                        if n > self.groups {
                            return Err(format!("the replacement `{replacement}`: there is no group {n}"));
                        }
                        n
                    };
                    if let Some(g) = m.group(group) {
                        out.push_str(g);
                    }
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        Ok(out)
    }

    /// `text` with its first match replaced by `replacement` (see
    /// [`expand`](Self::expand)).
    pub(crate) fn replace_first(&self, text: &str, replacement: &str) -> Result<String, String> {
        Ok(match self.find(text)? {
            Some(m) => format!("{}{}{}", &text[..m.start()], self.expand(&m, replacement)?, &text[m.end()..]),
            None => text.to_string(),
        })
    }
}

#[derive(Clone, Copy, Default)]
struct Flags {
    case_insensitive: bool,
    unix_lines: bool,
    multiline: bool,
    dotall: bool,
    unicode_case: bool,
    comments: bool,
    unicode_classes: bool,
}

thread_local! {
    /// Where the matcher's previous match ended, which `\b{g}` measures from,
    /// for the search under way on this thread.
    static LAST_END: Cell<usize> = const { Cell::new(0) };
}

/// Matching a regular expression does not express, done by a callout
/// (`(?C<n>)`, the n-th of a pattern's).
enum Callout {
    /// `\X`: an extended grapheme cluster.
    Grapheme,
    /// `\b{g}`: a grapheme cluster boundary.
    GraphemeBoundary,
    /// A position at least this many UTF-16 code units into the text.
    From(usize),
    /// A back-reference to `group` under case-insensitive matching, in
    /// Unicode case or ASCII.
    Backref { group: usize, unicode_case: bool },
}

/// What a `\` introduces.
enum Escape {
    /// A character: outside a class, one of a run of literal characters;
    /// inside one, a single character or a range's end. A surrogate code
    /// point is one.
    Char(u32),
    /// Any other construct, as `fancy_regex` text; inside a class, class items.
    Text(String),
    /// A construct a callout matches.
    Callout(Callout),
}

/// Rewrites a `java.util.regex` pattern as a `fancy_regex` one with the same
/// matches and the same group numbering.
struct Translator {
    /// The pattern as written, for messages.
    pattern: String,
    /// The pattern with its quotations rewritten (see [`unquote`]).
    src: Vec<char>,
    pos: usize,
    out: String,
    groups: usize,
    names: HashMap<String, usize>,
    flags: Flags,
    /// Flags to restore at each open group's end.
    saved: Vec<Flags>,
    /// Where in `out` the last complete atom begins, for a possessive
    /// quantifier to wrap.
    atom_start: Option<usize>,
    /// The callouts `out` names, in order.
    callouts: Vec<Callout>,
    /// The groups open, the pattern's own first: what their alternatives
    /// hold so far, as Java's nodes.
    frames: Vec<Frame>,
}

const LINE_TERMINATORS: &str = r"\n\r\x{85}\x{2028}\x{2029}";

/// A class that holds no character.
const NOTHING: &str = r"[^\x00-\x{10FFFF}]";

/// Outside `(?U)`, `\b` takes ASCII letters, digits and `_` for word
/// characters, and a nonspacing mark that a letter or digit precedes through a
/// run of nonspacing marks, all in the Basic Multilingual Plane (the mark
/// itself, after a position, in any plane). The texts saying whether the
/// character before a position is one, and whether the one after it is.
struct WordSides {
    before: String,
    no_before: String,
    after: String,
    no_after: String,
}

fn word_sides() -> &'static WordSides {
    static SIDES: OnceLock<WordSides> = OnceLock::new();
    SIDES.get_or_init(|| {
        let base = types(category::L | category::ND).bmp().items();
        let mark = types(category::MN);
        let (mark, bmp_mark) = (mark.items(), mark.bmp().items());
        WordSides {
            before: format!(r"(?<=[0-9A-Za-z_]|[{base}][{bmp_mark}]+)"),
            no_before: format!(r"(?<![0-9A-Za-z_]|[{base}][{bmp_mark}]+)"),
            after: format!(r"(?:(?=[0-9A-Za-z_])|(?=[{mark}])(?<=[{base}][{bmp_mark}]*))"),
            no_after: format!(r"(?![0-9A-Za-z_])(?:(?![{mark}])|(?<![{base}][{bmp_mark}]*))"),
        }
    })
}

/// A word boundary (`\b`), or a place that is none (`\B`), where `word` is
/// what a word character is.
fn word_boundary(word: &Set, boundary: bool) -> String {
    let w = word.items();
    if boundary {
        format!("(?:(?<=[{w}])(?![{w}])|(?<![{w}])(?=[{w}]))")
    } else {
        format!("(?:(?<=[{w}])(?=[{w}])|(?<![{w}])(?![{w}]))")
    }
}

impl Translator {
    fn new(pattern: &str) -> Translator {
        Translator {
            pattern: pattern.to_string(),
            src: unquote(&pattern.chars().collect::<Vec<_>>()),
            pos: 0,
            out: String::new(),
            groups: 0,
            names: HashMap::new(),
            flags: Flags::default(),
            saved: Vec::new(),
            atom_start: None,
            callouts: Vec::new(),
            frames: vec![Frame { kind: FrameKind::Pattern, alternatives: vec![Vec::new()], out_start: 0, src_start: 0 }],
        }
    }

    /// `node` added to the alternative being read.
    fn push_node(&mut self, node: Node) {
        if let Some(alternative) = self.frames.last_mut().and_then(|f| f.alternatives.last_mut()) {
            alternative.push(node);
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("`{}`: {what}", self.pattern))
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.pos += 1;
        c
    }

    /// In comments mode, steps over what separates tokens: ASCII whitespace,
    /// and a `#` comment up to the line terminator that ends it.
    fn skip_comments(&mut self) {
        if !self.flags.comments {
            return;
        }
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r') => self.pos += 1,
                Some('#') => {
                    self.pos += 1;
                    while let Some(c) = self.peek() {
                        if c == '\0' || self.is_line_terminator(c) {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn is_line_terminator(&self, c: char) -> bool {
        if self.flags.unix_lines {
            c == '\n'
        } else {
            matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
        }
    }

    fn translate(&mut self) -> Result<(), String> {
        loop {
            self.skip_comments();
            let Some(c) = self.next() else { break };
            match c {
                '\\' => {
                    let start = self.pos - 1;
                    match self.escape(false)? {
                        Escape::Char(_) => {
                            self.pos = start;
                            self.literals()?;
                        }
                        Escape::Text(text) => {
                            let at = self.out.len();
                            self.out.push_str(&text);
                            self.atom_start = Some(at);
                            self.push_node(match self.src[start + 1] {
                                'b' | 'B' | 'A' | 'z' | 'G' | 'Z' => Node::Empty,
                                'R' => Node::LineEnding,
                                '1'..='9' | 'k' => Node::Backref,
                                _ => Node::Char,
                            });
                        }
                        Escape::Callout(callout) => {
                            let at = self.out.len();
                            self.out.push_str(&format!("(?C{})", self.callouts.len()));
                            self.push_node(match callout {
                                Callout::Grapheme => Node::Grapheme,
                                Callout::Backref { .. } => Node::Backref,
                                Callout::GraphemeBoundary | Callout::From(_) => Node::Empty,
                            });
                            self.callouts.push(callout);
                            self.atom_start = Some(at);
                        }
                    }
                }
                '[' => {
                    let start = self.out.len();
                    let class = self.class()?;
                    self.out.push_str(&class);
                    self.atom_start = Some(start);
                    self.push_node(Node::Char);
                }
                '(' => self.open_group()?,
                ')' => self.close_group()?,
                '.' => {
                    self.push_node(Node::Char);
                    let start = self.out.len();
                    self.out.push_str(&if self.flags.dotall {
                        "(?s:.)".to_string()
                    } else if self.flags.unix_lines {
                        r"[^\n]".to_string()
                    } else {
                        format!("[^{LINE_TERMINATORS}]")
                    });
                    self.atom_start = Some(start);
                }
                '^' => {
                    self.atom_start = Some(self.out.len());
                    self.push_node(Node::Empty);
                    self.out.push_str(match (self.flags.multiline, self.flags.unix_lines) {
                        (false, _) => r"\A",
                        (true, true) => r"(?:\A|(?<=\n)(?!\z))",
                        (true, false) => r"(?:\A|(?<=[\n\x{85}\x{2028}\x{2029}])(?!\z)|(?<=\r)(?!\n)(?!\z))",
                    });
                }
                '$' => {
                    let start = self.out.len();
                    self.push_node(Node::Empty);
                    self.out.push_str(match (self.flags.multiline, self.flags.unix_lines) {
                        (false, true) => r"(?:\z|(?=\n\z))",
                        (false, false) => {
                            r"(?:\z|(?=\r\n\z)|(?<!\r)(?=\n\z)|(?=[\r\x{85}\x{2028}\x{2029}]\z))"
                        }
                        (true, true) => r"(?:\z|(?=\n))",
                        (true, false) => r"(?:\z|(?<!\r)(?=\n)|(?=[\r\x{85}\x{2028}\x{2029}]))",
                    });
                    self.atom_start = Some(start);
                }
                '*' | '+' | '?' => self.quantifier(c.to_string())?,
                '{' => {
                    let start = self.pos;
                    let digits: String = self.take_while(|c| c.is_ascii_digit());
                    if digits.is_empty() {
                        self.pos = start;
                        return self.err("a `{` that starts no repetition");
                    }
                    let mut q = format!("{{{digits}");
                    if self.peek() == Some(',') {
                        self.pos += 1;
                        q.push(',');
                        q.push_str(&self.take_while(|c| c.is_ascii_digit()));
                    }
                    if self.next() != Some('}') {
                        return self.err("an unclosed repetition");
                    }
                    q.push('}');
                    if self.atom_start.is_some() {
                        self.quantifier(q)?;
                    } else if matches!(self.peek(), Some('?' | '+')) {
                        // A repetition of nothing, which matches the empty
                        // string however it repeats.
                        self.pos += 1;
                    }
                }
                '|' => {
                    self.out.push('|');
                    self.atom_start = None;
                    if let Some(frame) = self.frames.last_mut() {
                        frame.alternatives.push(Vec::new());
                    }
                }
                _ => {
                    self.pos -= 1;
                    self.literals()?;
                }
            }
        }
        if !self.saved.is_empty() {
            return self.err("an unclosed group");
        }
        Ok(())
    }

    /// A run of literal characters, the next token its first. A run of one is
    /// matched as a single character, a longer one as a sequence, which
    /// folds case differently (see [`single_set`](Self::single_set)). The
    /// last of several that a quantifier follows is a run of its own.
    fn literals(&mut self) -> Result<(), String> {
        let mut run: Vec<(u32, usize)> = Vec::new();
        loop {
            if !run.is_empty() {
                self.skip_comments();
            }
            let at = self.pos;
            match self.peek() {
                None | Some('$' | '.' | '^' | '(' | '[' | '|' | ')') => break,
                Some('*' | '+' | '?' | '{') => {
                    if run.len() > 1 {
                        if let Some((_, last)) = run.pop() {
                            self.pos = last;
                        }
                    }
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    match self.escape(false)? {
                        Escape::Char(c) => run.push((c, at)),
                        Escape::Text(_) | Escape::Callout(_) => {
                            self.pos = at;
                            break;
                        }
                    }
                }
                Some(c) => {
                    self.pos += 1;
                    run.push((c as u32, at));
                }
            }
        }
        let in_sequence = run.len() > 1;
        for _ in &run {
            self.push_node(Node::Char);
        }
        for (c, _) in run {
            let set = if in_sequence { self.sequence_set(c) } else { self.single_set(c) };
            let items = Self::items(set);
            let start = self.out.len();
            match items.len() {
                0 => self.out.push_str(NOTHING),
                1 => self.out.push_str(&items[0]),
                _ => self.out.push_str(&format!("[{}]", items.concat())),
            }
            self.atom_start = Some(start);
        }
        Ok(())
    }

    /// The characters a single literal `c` matches. Case-insensitively, that
    /// is every character whose folded case (the lowercase of its uppercase)
    /// is `c`'s, where `c`'s uppercase and folded case differ; otherwise `c`
    /// alone. Without Unicode case, only ASCII letters fold, to each other.
    fn single_set(&self, c: u32) -> Vec<u32> {
        if self.flags.case_insensitive {
            if self.flags.unicode_case {
                let upper = case::upper(c);
                let lower = case::lower(upper);
                if upper != lower {
                    return case::folding_to(lower);
                }
            } else {
                return vec![c, ascii_lower(c), ascii_upper(c)];
            }
        }
        vec![c]
    }

    /// The characters a literal `c` matches as one of a sequence:
    /// case-insensitively, every character whose folded case is `c`'s.
    fn sequence_set(&self, c: u32) -> Vec<u32> {
        if self.flags.case_insensitive {
            if self.flags.unicode_case {
                return case::folding_to(case::fold(c));
            }
            let lower = ascii_lower(c);
            return vec![lower, ascii_upper(lower)];
        }
        vec![c]
    }

    /// Each of `set`'s characters once, in order, as a literal or class item;
    /// a surrogate code point, which no text holds, as none.
    fn items(set: Vec<u32>) -> Vec<String> {
        let mut seen: Vec<u32> = Vec::new();
        for c in set {
            if !seen.contains(&c) {
                seen.push(c);
            }
        }
        seen.into_iter().filter_map(char::from_u32).map(escape_char).collect()
    }

    fn take_while(&mut self, f: impl Fn(char) -> bool) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek().filter(|c| f(*c)) {
            s.push(c);
            self.pos += 1;
        }
        s
    }

    fn quantifier(&mut self, q: String) -> Result<(), String> {
        let Some(start) = self.atom_start else { return self.err("a quantifier with nothing to repeat") };
        let Some(mut repeat) = Repeat::of(&q) else { return self.err("a repetition range out of order or too long") };
        repeat.mode = match self.peek() {
            Some('?') => Mode::Lazy,
            Some('+') => Mode::Possessive,
            _ => Mode::Greedy,
        };
        if let Some(alternative) = self.frames.last_mut().and_then(|f| f.alternatives.last_mut()) {
            if let Some(node) = alternative.pop() {
                alternative.push(Node::Repeat(Box::new(node), repeat));
            }
        }
        match self.peek() {
            Some('?') => {
                self.pos += 1;
                self.out.push_str(&q);
                self.out.push('?');
            }
            Some('+') => {
                self.pos += 1;
                self.out.insert_str(start, "(?>");
                self.out.push_str(&q);
                self.out.push(')');
            }
            _ => self.out.push_str(&q),
        }
        // A quantifier repeats no quantifier.
        self.atom_start = None;
        Ok(())
    }

    fn open_group(&mut self) -> Result<(), String> {
        self.saved.push(self.flags);
        let out_start = self.out.len();
        let kind = self.group_opener()?;
        if let Some(kind) = kind {
            self.frames.push(Frame { kind, alternatives: vec![Vec::new()], out_start, src_start: self.pos });
        }
        Ok(())
    }

    /// A group's opening, `(` already read: what kind of group it opens, or
    /// none when it only sets flags.
    fn group_opener(&mut self) -> Result<Option<FrameKind>, String> {
        if self.peek() != Some('?') {
            self.groups += 1;
            self.out.push('(');
            return Ok(Some(FrameKind::Group));
        }
        self.pos += 1;
        match self.next() {
            Some(':') => self.out.push_str("(?:"),
            Some('=') => {
                self.out.push_str("(?=");
                return Ok(Some(FrameKind::LookAhead));
            }
            Some('!') => {
                self.out.push_str("(?!");
                return Ok(Some(FrameKind::LookAhead));
            }
            Some('>') => {
                self.out.push_str("(?>");
                return Ok(Some(FrameKind::Atomic));
            }
            Some('<') => match self.peek() {
                Some('=') => {
                    self.pos += 1;
                    self.out.push_str("(?<=");
                    return Ok(Some(FrameKind::LookBehind { negative: false }));
                }
                Some('!') => {
                    self.pos += 1;
                    self.out.push_str("(?<!");
                    return Ok(Some(FrameKind::LookBehind { negative: true }));
                }
                _ => {
                    let name = self.take_while(|c| c.is_ascii_alphanumeric());
                    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphabetic()) || self.next() != Some('>') {
                        return self.err("a malformed group name");
                    }
                    if self.names.contains_key(&name) {
                        return self.err(&format!("a second group named `{name}`"));
                    }
                    self.groups += 1;
                    self.names.insert(name, self.groups);
                    self.out.push('(');
                }
            },
            _ => {
                // Inline flags: `(?idmsuxU-idmsuxU)` or `(?…:X)`.
                self.pos -= 1;
                let mut on = true;
                loop {
                    let c = self.next();
                    let f = &mut self.flags;
                    match c {
                        Some('i') => f.case_insensitive = on,
                        Some('d') => f.unix_lines = on,
                        Some('m') => f.multiline = on,
                        Some('s') => f.dotall = on,
                        Some('u') => f.unicode_case = on,
                        Some('x') => f.comments = on,
                        Some('U') => {
                            f.unicode_classes = on;
                            f.unicode_case = on;
                        }
                        Some('-') => on = false,
                        Some(')') => {
                            // The flags hold to the end of the enclosing group.
                            let f = self.flags;
                            self.saved.pop();
                            self.flags = f;
                            self.atom_start = None;
                            return Ok(None);
                        }
                        Some(':') => {
                            self.out.push_str("(?:");
                            return Ok(Some(FrameKind::Group));
                        }
                        _ => return self.err("an unknown inline flag"),
                    }
                }
            }
        }
        Ok(Some(FrameKind::Group))
    }

    /// The end of a group, `)` already read.
    fn close_group(&mut self) -> Result<(), String> {
        let Some(f) = self.saved.pop() else { return self.err("an unmatched `)`") };
        self.flags = f;
        let Some(frame) = self.frames.pop().filter(|f| f.kind != FrameKind::Pattern) else {
            return self.err("an unmatched `)`");
        };
        let node = match frame.kind {
            FrameKind::Group | FrameKind::Pattern => Node::Group(frame.alternatives),
            FrameKind::Atomic => Node::Atomic(frame.alternatives),
            FrameKind::LookAhead => Node::Empty,
            FrameKind::LookBehind { negative } => {
                let supplementary = self.src[frame.src_start..].iter().any(|c| *c as u32 > 0xFFFF);
                self.look_behind(frame.out_start, negative, supplementary, Node::Group(frame.alternatives))?;
                Node::Empty
            }
        };
        self.out.push(')');
        self.atom_start = Some(frame.out_start);
        self.push_node(node);
        Ok(())
    }

    /// The look-behind whose `(?<=` or `(?<!` begins at `start` in `out`,
    /// its body behind it, made to try the lengths Java works out `body` can
    /// take: refused with no most Java can count, never matching (a negative
    /// one always) when its least passes its most, and tried from its least
    /// to its most otherwise. Java counts those lengths in UTF-16 code units,
    /// or in code points (`supplementary`) when the pattern from the body on
    /// holds a supplementary character; here they are code points. A most an
    /// int cannot hold leaves the most out, and the look-behind tried only
    /// from as far into the text as that most wrapped round says.
    fn look_behind(&mut self, start: usize, negative: bool, supplementary: bool, body: Node) -> Result<(), String> {
        let mut lengths = Lengths::default();
        study(&[body], &mut lengths);
        if !lengths.max_valid {
            return self.err("a look-behind whose length has no bound");
        }
        let (least, most) = (lengths.min.max(0) as usize, lengths.max);
        let sign = if negative { '!' } else { '=' };
        let opener = if most >= 0 && lengths.min > most {
            // No length to try: kept, so that its groups are, but unreached.
            if negative { format!("(?:(?:(?!)(?<0,{sign}") } else { format!("(?:(?!)(?<0,{sign}") }
        } else if most >= 0 {
            format!("(?<{least},{most}{sign}")
        } else {
            let from = i64::from(most) + (1_i64 << 31);
            if supplementary || from <= 0 {
                format!("(?<{least},{sign}")
            } else {
                let gate = format!("(?C{})", self.callouts.len());
                self.callouts.push(Callout::From(from as usize));
                if negative {
                    format!("(?:(?!{gate})|(?<{least},{sign}")
                } else {
                    format!("(?:{gate}(?<{least},{sign}")
                }
            }
        };
        let closed = opener.starts_with("(?:");
        self.out.replace_range(start..start + 4, &opener);
        if closed {
            self.out.push(')');
            if negative && opener.starts_with("(?:(?:(?!)") {
                self.out.push_str(")?");
            }
        }
        Ok(())
    }

    /// The construct after a `\`, outside (`in_class` false) or inside a
    /// character class.
    fn escape(&mut self, in_class: bool) -> Result<Escape, String> {
        let Some(c) = self.next() else { return self.err("a pattern ending in `\\`") };
        let ascii = !self.flags.unicode_classes;
        let set = |items: &str, negate: bool| {
            Escape::Text(if negate {
                complement(items)
            } else if in_class {
                items.to_string()
            } else {
                format!("[{items}]")
            })
        };
        let of = |chars: Set, negate: bool| {
            let chars = if negate { chars.complement() } else { chars };
            Escape::Text(if in_class { chars.items() } else { chars.class() })
        };
        let text = |s: &str| Escape::Text(s.to_string());
        Ok(match c {
            'w' | 'W' if ascii => set("0-9A-Za-z_", c == 'W'),
            'd' | 'D' if ascii => set("0-9", c == 'D'),
            's' | 'S' if ascii => set(r"\t\n\x0B\x0C\r ", c == 'S'),
            'w' | 'W' => of(word(), c == 'W'),
            'd' | 'D' => of(types(category::ND), c == 'D'),
            's' | 'S' => of(white_space(), c == 'S'),
            'h' | 'H' => set(r" \t\xA0\x{1680}\x{180E}\x{2000}-\x{200A}\x{202F}\x{205F}\x{3000}", c == 'H'),
            'v' | 'V' => set(r"\n\x0B\x0C\r\x{85}\x{2028}\x{2029}", c == 'V'),
            'b' if !in_class && self.grapheme_bound()? => Escape::Callout(Callout::GraphemeBoundary),
            'b' | 'B' if !in_class => Escape::Text(if ascii {
                let w = word_sides();
                if c == 'b' {
                    format!("(?:{}{}|{}{})", w.before, w.no_after, w.no_before, w.after)
                } else {
                    format!("(?:{}{}|{}{})", w.before, w.after, w.no_before, w.no_after)
                }
            } else {
                word_boundary(&word(), c == 'b')
            }),
            'A' | 'z' if !in_class => Escape::Text(format!(r"\{c}")),
            // Matching starts where the text does, so the end of the previous
            // match is its start.
            'G' if !in_class => text(r"\A"),
            'Z' if !in_class => text(if self.flags.unix_lines {
                r"(?:\z|(?=\n\z))"
            } else {
                r"(?:\z|(?=\r\n\z)|(?<!\r)(?=\n\z)|(?=[\r\x{85}\x{2028}\x{2029}]\z))"
            }),
            'R' if !in_class => text(r"(?:\r\n|[\n\x0B\x0C\r\x{85}\x{2028}\x{2029}])"),
            'X' if !in_class => Escape::Callout(Callout::Grapheme),
            't' => Escape::Char(0x09),
            'n' => Escape::Char(0x0A),
            'r' => Escape::Char(0x0D),
            'f' => Escape::Char(0x0C),
            'a' => Escape::Char(0x07),
            'e' => Escape::Char(0x1B),
            '0' => Escape::Char(self.octal()?),
            'x' => Escape::Char(self.hex()?),
            'u' => Escape::Char(self.utf16_escape()?),
            'N' => Escape::Char(self.character_name()?),
            'c' => {
                let Some(x) = self.next() else { return self.err("a `\\c` with no character") };
                Escape::Char(x as u32 ^ 64)
            }
            'p' | 'P' => {
                let name = if self.peek() == Some('{') {
                    self.pos += 1;
                    let n = self.take_while(|c| c != '}');
                    if self.next() != Some('}') {
                        return self.err("an unclosed `\\p{…}`");
                    }
                    n
                } else {
                    self.next().map(String::from).unwrap_or_default()
                };
                let chars = family(&name, self.flags.case_insensitive, self.flags.unicode_classes)
                    .map_or_else(|| self.err(&format!("the character property `{name}`, which names none")), Ok)?;
                of(chars, c == 'P')
            }
            'k' if !in_class => {
                if self.next() != Some('<') {
                    return self.err("a malformed `\\k<name>`");
                }
                let name = self.take_while(|c| c != '>');
                let group = self.names.get(&name).copied();
                match (self.next(), group) {
                    (Some('>'), Some(group)) => self.backref(group),
                    _ => return self.err(&format!("a reference to no group named `{name}`")),
                }
            }
            '1'..='9' if !in_class => {
                let mut n = c.to_digit(10).unwrap_or(0) as usize;
                while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
                    let longer = n * 10 + d as usize;
                    if longer > self.groups {
                        break;
                    }
                    n = longer;
                    self.pos += 1;
                }
                if n > self.groups {
                    // A reference to a group not yet opened never matches.
                    text("(?!)")
                } else {
                    self.backref(n)
                }
            }
            c if c.is_ascii_alphanumeric() => return self.err(&format!("the escape `\\{c}`")),
            c => Escape::Char(c as u32),
        })
    }

    /// A back-reference to `group`: under case-insensitive matching, to the
    /// group's text with each character in either case.
    fn backref(&self, group: usize) -> Escape {
        if self.flags.case_insensitive {
            Escape::Callout(Callout::Backref { group, unicode_case: self.flags.unicode_case })
        } else {
            Escape::Text(format!(r"(?:\{group})"))
        }
    }

    /// After `\b`: whether `{g}` follows, which makes it a grapheme cluster
    /// boundary. In comments mode, space may come before the `{` and before
    /// the `}`.
    fn grapheme_bound(&mut self) -> Result<bool, String> {
        self.skip_comments();
        if self.peek() != Some('{') || self.src.get(self.pos + 1) != Some(&'g') {
            return Ok(false);
        }
        self.pos += 2;
        self.skip_comments();
        if self.next() != Some('}') {
            return self.err("a `\\b{g` that `}` does not close");
        }
        Ok(true)
    }

    /// The code point a `\N{name}` escape names, `\N` already read.
    fn character_name(&mut self) -> Result<u32, String> {
        self.skip_comments();
        if self.next() != Some('{') {
            return self.err("a `\\N` with no `{name}`");
        }
        let start = self.pos;
        loop {
            self.skip_comments();
            if self.next() == Some('}') {
                break;
            }
            if self.pos >= self.src.len() {
                return self.err("an unclosed `\\N{…}`");
            }
        }
        let name: String = self.src[start..self.pos - 1].iter().collect();
        charnames::code_point_of(&name).map_or_else(|| self.err(&format!("the character name `{name}`, which names none")), Ok)
    }

    /// The code point an octal escape `\0n`, `\0nn` or `\0mnn` (`m` at most 3)
    /// names, `\0` already read.
    fn octal(&mut self) -> Result<u32, String> {
        let digits = self.take_while(|c| ('0'..='7').contains(&c));
        let digits = match digits.len() {
            0 => return self.err("an octal escape with no digits"),
            3 if digits.as_bytes()[0] > b'3' => {
                self.pos -= 1;
                digits[..2].to_string()
            }
            n if n > 3 => {
                self.pos -= n - 3;
                digits[..3].to_string()
            }
            _ => digits,
        };
        Ok(u32::from_str_radix(&digits, 8).unwrap_or(0))
    }

    /// The code point a `\xhh` or `\x{h…}` escape names, `\x` already read.
    fn hex(&mut self) -> Result<u32, String> {
        if self.peek() == Some('{') && self.src.get(self.pos + 1).is_some_and(|c| c.is_ascii_hexdigit()) {
            self.pos += 1;
            let hex = self.take_while(|c| c.is_ascii_hexdigit());
            if self.next() != Some('}') {
                return self.err("an unclosed `\\x{…}` escape");
            }
            return match u32::from_str_radix(&hex, 16) {
                Ok(code) if code <= 0x10FFFF => Ok(code),
                _ => self.err("a `\\x{…}` escape beyond the last code point"),
            };
        }
        let hex: String = self.src.iter().skip(self.pos).take(2).collect();
        if hex.len() == 2 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            self.pos += 2;
            return Ok(u32::from_str_radix(&hex, 16).unwrap_or(0));
        }
        self.err("a malformed `\\x` escape")
    }

    /// The code point a `\uhhhh` escape names, `\u` already read: with a
    /// second one that completes a surrogate pair, the pair's.
    fn utf16_escape(&mut self) -> Result<u32, String> {
        let unit = self.four_hex()?;
        if (0xD800..0xDC00).contains(&unit) {
            let at = self.pos;
            if self.next() == Some('\\') && self.next() == Some('u') {
                let low = self.four_hex()?;
                if (0xDC00..0xE000).contains(&low) {
                    return Ok(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00));
                }
            }
            self.pos = at;
        }
        Ok(unit)
    }

    fn four_hex(&mut self) -> Result<u32, String> {
        let hex: String = self.src.iter().skip(self.pos).take(4).collect();
        if hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            self.pos += 4;
            return Ok(u32::from_str_radix(&hex, 16).unwrap_or(0));
        }
        self.err("a malformed `\\u` escape")
    }

    /// A character class, `[` already read, as `fancy_regex` text.
    fn class(&mut self) -> Result<String, String> {
        let negated = self.peek() == Some('^');
        if negated {
            self.pos += 1;
        }
        let mut items = String::new();
        let mut first = true;
        loop {
            self.skip_comments();
            let Some(c) = self.next() else { return self.err("an unclosed character class") };
            match c {
                ']' if !first => break,
                '[' => items.push_str(&self.class()?),
                '&' if self.peek() == Some('&') => {
                    self.pos += 1;
                    // An intersection with nothing on one side is no
                    // intersection: `[a&&]` and `[&&a]` are `[a]`.
                    let right_empty = matches!(self.peek(), Some(']' | '&'));
                    if !items.is_empty() && !right_empty {
                        items.push_str("&&");
                    }
                }
                '\\' => match self.escape(true)? {
                    Escape::Text(text) => items.push_str(&text),
                    Escape::Char(c) => items.push_str(&self.class_char(c)?),
                    Escape::Callout(_) => return self.err("a class that holds more than characters"),
                },
                c => items.push_str(&self.class_char(c as u32)?),
            }
            first = false;
        }
        Ok(if negated { complement(&items) } else { format!("[{items}]") })
    }

    /// A class's character `lo`, already read, or the range it starts.
    fn class_char(&mut self, lo: u32) -> Result<String, String> {
        self.skip_comments();
        let end = self.src.get(self.pos + 1).copied();
        if self.peek() == Some('-') && end != Some('[') && end != Some(']') {
            self.pos += 1;
            self.skip_comments();
            let hi = match self.next() {
                Some('\\') => match self.escape(true)? {
                    Escape::Char(hi) => hi,
                    Escape::Text(_) | Escape::Callout(_) => {
                        return self.err("a character range that ends in a class")
                    }
                },
                Some(c) => c as u32,
                None => return self.err("an unclosed character class"),
            };
            if hi < lo {
                return self.err("a character range that runs backwards");
            }
            return Ok(self.class_range(lo, hi));
        }
        let items = Self::items(self.class_set(lo));
        Ok(if items.is_empty() { NOTHING.to_string() } else { items.concat() })
    }

    /// The characters a class's single character `c` stands for. Up to U+00FF
    /// it is folded case-insensitively to its ASCII other case, or with
    /// Unicode case to its uppercase and lowercase; beyond, and for the
    /// characters whose case reaches past U+00FF (`I`, `K`, `S`, `µ`, `Å`, `ÿ`
    /// and their lowercase), it matches as a single literal does.
    fn class_set(&self, c: u32) -> Vec<u32> {
        let f = self.flags;
        let past_latin1 = f.case_insensitive
            && f.unicode_case
            && matches!(c, 0xFF | 0xB5 | 0x49 | 0x69 | 0x53 | 0x73 | 0x4B | 0x6B | 0xC5 | 0xE5);
        if c >= 0x100 || past_latin1 {
            return self.single_set(c);
        }
        if f.case_insensitive {
            if c < 0x80 {
                return vec![c, ascii_upper(c), ascii_lower(c)];
            }
            if f.unicode_case {
                return vec![c, case::lower(c), case::upper(c)];
            }
        }
        vec![c]
    }

    /// The class items for the range `lo`-`hi` and, case-insensitively, for
    /// every character whose uppercase or folded case falls in it (only ASCII
    /// letters, by their ASCII case, without Unicode case).
    fn class_range(&self, lo: u32, hi: u32) -> String {
        let within = |c: u32| lo <= c && c <= hi;
        let mut items = range_items(lo, hi);
        if self.flags.case_insensitive {
            let extra: Vec<u32> = if self.flags.unicode_case {
                case::cased()
                    .iter()
                    .filter(|(c, upper, folded)| !within(*c) && (within(*upper) || within(*folded)))
                    .map(|(c, _, _)| *c)
                    .collect()
            } else {
                (0..0x80).filter(|&c| !within(c) && (within(ascii_upper(c)) || within(ascii_lower(c)))).collect()
            };
            for c in extra.into_iter().filter_map(char::from_u32) {
                items.push_str(&escape_char(c));
            }
        }
        items
    }
}

/// `src` with each `\Q…\E` quotation replaced by its characters written to be
/// read literally: an ASCII character other than a letter or digit behind a
/// `\`, and a digit that opens a quotation as `\x3` and itself, so that it
/// cannot extend an escape the quotation follows.
fn unquote(src: &[char]) -> Vec<char> {
    let n = src.len();
    let mut i = 0;
    while i + 1 < n {
        if src[i] != '\\' {
            i += 1;
        } else if src[i + 1] != 'Q' {
            i += 2;
        } else {
            break;
        }
    }
    if i + 1 >= n {
        return src.to_vec();
    }
    let mut out = src[..i].to_vec();
    i += 2;
    let (mut in_quote, mut begin_quote) = (true, true);
    while i < n {
        let c = src[i];
        i += 1;
        if !c.is_ascii() || c.is_ascii_alphabetic() {
            out.push(c);
        } else if c.is_ascii_digit() {
            if begin_quote {
                out.extend(['\\', 'x', '3']);
            }
            out.push(c);
        } else if c != '\\' {
            if in_quote {
                out.push('\\');
            }
            out.push(c);
        } else if in_quote {
            if src.get(i) == Some(&'E') {
                i += 1;
                in_quote = false;
            } else {
                out.extend(['\\', '\\']);
            }
        } else if src.get(i) == Some(&'Q') {
            i += 1;
            in_quote = true;
            begin_quote = true;
            continue;
        } else {
            out.push(c);
            if i < n {
                out.push(src[i]);
                i += 1;
            }
        }
        begin_quote = false;
    }
    out
}

/// The class item for the code points `lo`-`hi`, which no text holds the
/// surrogates of.
fn range_items(lo: u32, hi: u32) -> String {
    let lo = if (0xD800..0xE000).contains(&lo) { 0xE000 } else { lo };
    let hi = if (0xD800..0xE000).contains(&hi) { 0xD7FF } else { hi };
    if lo > hi {
        NOTHING.to_string()
    } else {
        format!(r"\x{{{lo:X}}}-\x{{{hi:X}}}")
    }
}

/// The class of every character not among `items`. Written as a difference
/// rather than with `^`, which would count U+D7FF and U+E000 as neighbours of
/// each other when `items` holds both.
fn complement(items: &str) -> String {
    format!(r"[\x00-\x{{10FFFF}}--[{items}]]")
}

fn ascii_lower(c: u32) -> u32 {
    if (0x41..=0x5A).contains(&c) {
        c + 0x20
    } else {
        c
    }
}

fn ascii_upper(c: u32) -> u32 {
    if (0x61..=0x7A).contains(&c) {
        c - 0x20
    } else {
        c
    }
}

fn escape_char(c: char) -> String {
    if c.is_ascii_punctuation() || c == ' ' {
        format!("\\{c}")
    } else if c.is_control() {
        format!(r"\x{{{:X}}}", c as u32)
    } else {
        c.to_string()
    }
}

/// Case mapping one character to one, as `Character.toUpperCase` and
/// `toLowerCase` map it.
mod case {
    use super::unicode::CASES;
    use std::sync::OnceLock;

    fn mapping(c: u32) -> Option<&'static (u32, u32, u32)> {
        CASES.binary_search_by_key(&c, |m| m.0).ok().map(|i| &CASES[i])
    }

    /// The uppercase of `c`.
    pub(super) fn upper(c: u32) -> u32 {
        mapping(c).map_or(c, |m| m.1)
    }

    /// The lowercase of `c`.
    pub(super) fn lower(c: u32) -> u32 {
        mapping(c).map_or(c, |m| m.2)
    }

    /// `c`'s folded case: the lowercase of its uppercase.
    pub(super) fn fold(c: u32) -> u32 {
        lower(upper(c))
    }

    /// Every character whose uppercase or folded case is another character,
    /// with its uppercase and its folded case.
    pub(super) fn cased() -> &'static [(u32, u32, u32)] {
        static CASED: OnceLock<Vec<(u32, u32, u32)>> = OnceLock::new();
        CASED.get_or_init(|| {
            CASES
                .iter()
                .filter_map(|&(c, upper, _)| {
                    let folded = lower(upper);
                    (upper != c || folded != c).then_some((c, upper, folded))
                })
                .collect()
        })
    }

    /// `folded` and every character whose folded case it is.
    pub(super) fn folding_to(folded: u32) -> Vec<u32> {
        let mut out = vec![folded];
        out.extend(cased().iter().filter(|(_, _, f)| *f == folded).map(|(c, _, _)| *c));
        out
    }
}

/// Where a case-insensitive back-reference to `captured` that starts at byte
/// `at` of `text` ends: past a character for each of the group's, each the
/// same or the same folded, to Unicode case (`unicode_case`) or with ASCII
/// letters in lower case.
fn backref_end(text: &str, at: usize, captured: &str, unicode_case: bool) -> Option<usize> {
    let fold = |c: char| if unicode_case { case::fold(c as u32) } else { ascii_lower(c as u32) };
    let mut rest = text.get(at..)?.chars();
    let mut end = at;
    for want in captured.chars() {
        let got = rest.next()?;
        if got != want && fold(got) != fold(want) {
            return None;
        }
        end += got.len_utf8();
    }
    Some(end)
}

// ── Look-behind lengths ─────────────────────────────────────────────────────

/// A group being read, as what its length is worked out from.
struct Frame {
    kind: FrameKind,
    /// Its alternatives so far, each a sequence of nodes.
    alternatives: Vec<Vec<Node>>,
    /// Where in `out` the group begins.
    out_start: usize,
    /// Where in `src` its body begins.
    src_start: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum FrameKind {
    /// The whole pattern.
    Pattern,
    /// A group, capturing or not.
    Group,
    /// An atomic group, `(?>…)`.
    Atomic,
    /// A look-ahead, positive or negative.
    LookAhead,
    LookBehind { negative: bool },
}

/// A construct as Java's matcher has it, so far as its length goes.
#[derive(Clone)]
enum Node {
    /// One character: a literal, a class, `.`, a property.
    Char,
    /// `\R`: one character or two.
    LineEnding,
    /// `\X`: one character at least, and none counted at most.
    Grapheme,
    /// A back-reference, whose length nothing bounds.
    Backref,
    /// What takes no length: an anchor, a boundary, a look-around.
    Empty,
    /// A group's alternatives.
    Group(Vec<Vec<Node>>),
    /// An atomic group's alternatives.
    Atomic(Vec<Vec<Node>>),
    Repeat(Box<Node>, Repeat),
}

/// A quantifier: how many times, written how, and how it backtracks.
#[derive(Clone, Copy)]
struct Repeat {
    min: i32,
    max: i32,
    form: Form,
    mode: Mode,
}

#[derive(Clone, Copy, PartialEq)]
enum Form {
    /// `?`, and `{0,1}`.
    Optional,
    /// `*`, `+` and `{n,}`.
    Open,
    /// `{n}` and `{n,m}`.
    Counted,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Greedy,
    Lazy,
    Possessive,
}

/// The most repetitions Java counts: what an open quantifier stands for.
const MAX_REPS: i32 = i32::MAX;

impl Repeat {
    /// The quantifier `q` (`?`, `*`, `+` or `{…}`), greedy; none for a range
    /// out of order or past an int.
    fn of(q: &str) -> Option<Repeat> {
        let (min, max, form) = match q {
            "?" => (0, 1, Form::Optional),
            "*" => (0, MAX_REPS, Form::Open),
            "+" => (1, MAX_REPS, Form::Open),
            _ => {
                let range = q.strip_prefix('{')?.strip_suffix('}')?;
                match range.split_once(',') {
                    None => {
                        let n = range.parse().ok()?;
                        (n, n, Form::Counted)
                    }
                    Some((n, "")) => (n.parse().ok()?, MAX_REPS, Form::Open),
                    Some((n, m)) => {
                        let (n, m) = (n.parse().ok()?, m.parse().ok()?);
                        (n, m, if (n, m) == (0, 1) { Form::Optional } else { Form::Counted })
                    }
                }
            }
        };
        (min <= max).then_some(Repeat { min, max, form, mode: Mode::Greedy })
    }
}

/// How long Java takes what it has studied to match, in its int arithmetic:
/// the fewest characters, the most, whether the most is known, and whether
/// matching it never backtracks.
#[derive(Clone, Copy)]
struct Lengths {
    min: i32,
    max: i32,
    max_valid: bool,
    deterministic: bool,
}

impl Default for Lengths {
    fn default() -> Lengths {
        Lengths { min: 0, max: 0, max_valid: true, deterministic: true }
    }
}

/// `nodes`, a chain, studied after what `lengths` holds: a group in a chain
/// is its nodes there, and alternatives take the rest of the chain apart.
fn study(nodes: &[Node], lengths: &mut Lengths) {
    for (i, node) in nodes.iter().enumerate() {
        let alternatives = match node {
            Node::Group(alternatives) if alternatives.len() == 1 => {
                let chain: Vec<Node> = alternatives[0].iter().chain(&nodes[i + 1..]).cloned().collect();
                return study(&chain, lengths);
            }
            Node::Group(alternatives) => alternatives.clone(),
            // An optional group is the group or nothing.
            Node::Repeat(inner, r) if r.form == Form::Optional && r.mode != Mode::Possessive => match &**inner {
                Node::Group(alternatives) => vec![vec![Node::Group(alternatives.clone())], Vec::new()],
                _ => {
                    study_one(node, lengths);
                    continue;
                }
            },
            _ => {
                study_one(node, lengths);
                continue;
            }
        };
        let (mut min, mut max, mut max_valid) = (lengths.min, lengths.max, lengths.max_valid);
        let (mut fewest, mut most) = (i32::MAX, -1);
        for alternative in &alternatives {
            let mut each = Lengths::default();
            study(alternative, &mut each);
            fewest = fewest.min(each.min);
            most = most.max(each.max);
            max_valid &= each.max_valid;
        }
        min = min.wrapping_add(fewest);
        max = max.wrapping_add(most);
        *lengths = Lengths::default();
        study(&nodes[i + 1..], lengths);
        lengths.min = lengths.min.wrapping_add(min);
        lengths.max = lengths.max.wrapping_add(max);
        lengths.max_valid &= max_valid;
        lengths.deterministic = false;
        return;
    }
}

/// One node that is no group in a chain, studied after `lengths`.
fn study_one(node: &Node, lengths: &mut Lengths) {
    match node {
        Node::Char => {
            lengths.min = lengths.min.wrapping_add(1);
            lengths.max = lengths.max.wrapping_add(1);
        }
        Node::LineEnding => {
            lengths.min = lengths.min.wrapping_add(1);
            lengths.max = lengths.max.wrapping_add(2);
        }
        Node::Grapheme => {
            lengths.min = lengths.min.wrapping_add(1);
            lengths.deterministic = false;
        }
        Node::Backref => lengths.max_valid = false,
        Node::Empty => {}
        Node::Group(_) | Node::Atomic(_) => study(&[group_of(node)], lengths),
        Node::Repeat(inner, r) => {
            let group = matches!(**inner, Node::Group(_));
            if r.form == Form::Optional {
                let min = lengths.min;
                study(&[group_of(inner)], lengths);
                lengths.min = min;
                lengths.deterministic = false;
            } else if !group && r.form == Form::Open && r.mode == Mode::Greedy && matches!(**inner, Node::Char) {
                lengths.min = lengths.min.wrapping_add(r.min);
                if lengths.max_valid {
                    lengths.max = lengths.max.wrapping_add(MAX_REPS);
                }
                lengths.deterministic = false;
            } else if group && r.mode != Mode::Possessive && !{
                let mut body = Lengths::default();
                study(&[group_of(inner)], &mut body);
                body.deterministic
            } {
                // A repeated group that may backtrack: no most is counted.
                lengths.max_valid = false;
                lengths.deterministic = false;
            } else {
                let (min, max, max_valid, deterministic) =
                    (lengths.min, lengths.max, lengths.max_valid, lengths.deterministic);
                *lengths = Lengths::default();
                study(&[group_of(inner)], lengths);
                let fewest = lengths.min.wrapping_mul(r.min).wrapping_add(min);
                lengths.min = if fewest < min { 0xFFF_FFFF } else { fewest };
                if max_valid && lengths.max_valid {
                    let most = lengths.max.wrapping_mul(r.max).wrapping_add(max);
                    lengths.max = most;
                    lengths.max_valid = most >= max;
                } else {
                    lengths.max_valid = false;
                }
                lengths.deterministic = lengths.deterministic && r.min == r.max && deterministic;
            }
        }
    }
}

/// `node` as a chain of its own: an atomic group's body as a group's.
fn group_of(node: &Node) -> Node {
    match node {
        Node::Atomic(alternatives) => Node::Group(alternatives.clone()),
        node => node.clone(),
    }
}

// ── Characters ──────────────────────────────────────────────────────────────

/// A set of code points: sorted runs, apart from each other.
#[derive(Clone, Default)]
struct Set(Vec<(u32, u32)>);

impl Set {
    /// The code points of `runs`, in any order and overlapping.
    fn of(runs: impl IntoIterator<Item = (u32, u32)>) -> Set {
        let mut runs: Vec<(u32, u32)> = runs.into_iter().collect();
        runs.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(runs.len());
        for (lo, hi) in runs {
            match out.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => out.push((lo, hi)),
            }
        }
        Set(out)
    }

    fn union(&self, other: &Set) -> Set {
        Set::of(self.0.iter().chain(&other.0).copied())
    }

    /// Every code point not in this set.
    fn complement(&self) -> Set {
        let mut out = Vec::new();
        let mut next = 0;
        for &(lo, hi) in &self.0 {
            if lo > next {
                out.push((next, lo - 1));
            }
            next = hi + 1;
        }
        if next <= 0x10FFFF {
            out.push((next, 0x10FFFF));
        }
        Set(out)
    }

    /// The code points of this set in the Basic Multilingual Plane.
    fn bmp(&self) -> Set {
        Set(self.0.iter().filter(|r| r.0 <= 0xFFFF).map(|&(lo, hi)| (lo, hi.min(0xFFFF))).collect())
    }

    /// This set as class items, without the surrogates, which no text holds;
    /// a class that holds nothing when that leaves none.
    fn items(&self) -> String {
        let mut out = String::new();
        for &(lo, hi) in &self.0 {
            for (lo, hi) in [(lo, hi.min(0xD7FF)), (lo.max(0xE000), hi)] {
                if lo < hi {
                    out.push_str(&format!(r"\x{{{lo:X}}}-\x{{{hi:X}}}"));
                } else if lo == hi {
                    out.push_str(&format!(r"\x{{{lo:X}}}"));
                }
            }
        }
        if out.is_empty() {
            NOTHING.to_string()
        } else {
            out
        }
    }

    /// This set as a class.
    fn class(&self) -> String {
        let items = self.items();
        if items == NOTHING {
            items
        } else {
            format!("[{items}]")
        }
    }
}

/// `Character.getType`'s categories, a bit each.
mod category {
    pub(super) const CN: u32 = 1;
    pub(super) const LU: u32 = 1 << 1;
    pub(super) const LL: u32 = 1 << 2;
    pub(super) const LT: u32 = 1 << 3;
    pub(super) const LM: u32 = 1 << 4;
    pub(super) const LO: u32 = 1 << 5;
    pub(super) const MN: u32 = 1 << 6;
    pub(super) const ME: u32 = 1 << 7;
    pub(super) const MC: u32 = 1 << 8;
    pub(super) const ND: u32 = 1 << 9;
    pub(super) const NL: u32 = 1 << 10;
    pub(super) const NO: u32 = 1 << 11;
    pub(super) const ZS: u32 = 1 << 12;
    pub(super) const ZL: u32 = 1 << 13;
    pub(super) const ZP: u32 = 1 << 14;
    pub(super) const CC: u32 = 1 << 15;
    pub(super) const CF: u32 = 1 << 16;
    pub(super) const CO: u32 = 1 << 18;
    pub(super) const CS: u32 = 1 << 19;
    pub(super) const PD: u32 = 1 << 20;
    pub(super) const PS: u32 = 1 << 21;
    pub(super) const PE: u32 = 1 << 22;
    pub(super) const PC: u32 = 1 << 23;
    pub(super) const PO: u32 = 1 << 24;
    pub(super) const SM: u32 = 1 << 25;
    pub(super) const SC: u32 = 1 << 26;
    pub(super) const SK: u32 = 1 << 27;
    pub(super) const SO: u32 = 1 << 28;
    pub(super) const PI: u32 = 1 << 29;
    pub(super) const PF: u32 = 1 << 30;
    pub(super) const L: u32 = LU | LL | LT | LM | LO;
    pub(super) const M: u32 = MN | ME | MC;
    pub(super) const N: u32 = ND | NL | NO;
    pub(super) const Z: u32 = ZS | ZL | ZP;
    pub(super) const C: u32 = CC | CF | CO | CS | CN;
    pub(super) const P: u32 = PD | PS | PE | PC | PO | PI | PF;
    pub(super) const S: u32 = SM | SC | SK | SO;
}

/// The code points whose category is among `mask`.
fn types(mask: u32) -> Set {
    let unassigned = mask & category::CN != 0;
    let mut runs = Vec::new();
    let mut next = 0;
    for &(lo, hi, t) in unicode::CATEGORIES {
        if unassigned && lo > next {
            runs.push((next, lo - 1));
        }
        if mask & (1 << t) != 0 {
            runs.push((lo, hi));
        }
        next = hi + 1;
    }
    if unassigned && next <= 0x10FFFF {
        runs.push((next, 0x10FFFF));
    }
    Set::of(runs)
}

/// Whether `Character.getType` calls `c` unassigned.
fn is_unassigned(c: u32) -> bool {
    let runs = unicode::CATEGORIES;
    runs.get(runs.partition_point(|r| r.1 < c)).is_none_or(|r| r.0 > c)
}

/// The code points one of `Character`'s predicates holds for.
fn holding(runs: &[(u32, u32)]) -> Set {
    Set::of(runs.iter().copied())
}

/// `\p{IsWord}`, and `\w` under `(?U)`.
fn word() -> Set {
    use category::*;
    holding(unicode::ALPHABETIC).union(&types(MN | ME | MC | ND | PC)).union(&Set::of([(0x200C, 0x200D)]))
}

/// `\p{IsWhite_Space}`, and `\s` under `(?U)`.
fn white_space() -> Set {
    types(category::Z).union(&Set::of([(0x09, 0x0D), (0x85, 0x85)]))
}

fn hex_digit() -> Set {
    let digits = [(0x30, 0x39), (0x41, 0x46), (0x61, 0x66), (0xFF10, 0xFF19), (0xFF21, 0xFF26), (0xFF41, 0xFF46)];
    types(category::ND).union(&Set::of(digits))
}

/// Every letter with a case, which is what the case properties name
/// case-insensitively; otherwise `chars`.
fn cased(case_insensitive: bool, chars: Set) -> Set {
    if case_insensitive {
        holding(unicode::LOWER_CASE).union(&holding(unicode::UPPER_CASE)).union(&types(category::LT))
    } else {
        chars
    }
}

// ── Character properties ────────────────────────────────────────────────────

/// What `\p{name}` names under the flags in force: a Unicode script, block or
/// general category, a POSIX class (ASCII, or Unicode under `(?U)` and with an
/// `Is`), or a `java…` property of `Character`. None when the name names
/// nothing.
fn family(name: &str, case_insensitive: bool, unicode_classes: bool) -> Option<Set> {
    if let Some((key, value)) = name.split_once('=') {
        return match key.to_lowercase().as_str() {
            "sc" | "script" => script(value),
            "blk" | "block" => block(value),
            "gc" | "general_category" => property(value, case_insensitive),
            _ => None,
        };
    }
    if let Some(block_name) = name.strip_prefix("In") {
        block(block_name)
    } else if let Some(short) = name.strip_prefix("Is") {
        let upper = short.to_uppercase();
        unicode_property(&upper, case_insensitive)
            .or_else(|| posix(&upper, case_insensitive))
            .or_else(|| property(short, case_insensitive))
            .or_else(|| script(short))
    } else {
        unicode_classes
            .then(|| posix(&name.to_uppercase(), case_insensitive))
            .flatten()
            .or_else(|| property(name, case_insensitive))
    }
}

/// A Unicode property `\p{IsName}` names, `name` in upper case.
fn unicode_property(name: &str, case_insensitive: bool) -> Option<Set> {
    use category::*;
    use unicode::*;
    Some(match name {
        "ALPHABETIC" => holding(ALPHABETIC),
        "ASSIGNED" => types(CN).complement(),
        "CONTROL" => types(CC),
        "EMOJI" => holding(EMOJI),
        "EMOJI_PRESENTATION" => holding(EMOJI_PRESENTATION),
        "EMOJI_MODIFIER" => holding(EMOJI_MODIFIER),
        "EMOJI_MODIFIER_BASE" => holding(EMOJI_MODIFIER_BASE),
        "EMOJI_COMPONENT" => holding(EMOJI_COMPONENT),
        "EXTENDED_PICTOGRAPHIC" => holding(EXTENDED_PICTOGRAPHIC),
        "HEXDIGIT" | "HEX_DIGIT" => hex_digit(),
        "IDEOGRAPHIC" => holding(IDEOGRAPHIC),
        "JOINCONTROL" | "JOIN_CONTROL" => Set::of([(0x200C, 0x200D)]),
        "LETTER" => types(L),
        "LOWERCASE" => cased(case_insensitive, holding(LOWER_CASE)),
        "NONCHARACTERCODEPOINT" | "NONCHARACTER_CODE_POINT" => {
            Set::of((0..=0x10).map(|plane| (plane << 16 | 0xFFFE, plane << 16 | 0xFFFF)).chain([(0xFDD0, 0xFDEF)]))
        }
        "TITLECASE" => cased(case_insensitive, types(LT)),
        "PUNCTUATION" => types(P),
        "UPPERCASE" => cased(case_insensitive, holding(UPPER_CASE)),
        "WHITESPACE" | "WHITE_SPACE" => white_space(),
        "WORD" => word(),
        _ => return None,
    })
}

/// A POSIX class with Unicode members, `name` in upper case.
fn posix(name: &str, case_insensitive: bool) -> Option<Set> {
    use category::*;
    use unicode::*;
    Some(match name {
        "ALPHA" => holding(ALPHABETIC),
        "LOWER" => cased(case_insensitive, holding(LOWER_CASE)),
        "UPPER" => cased(case_insensitive, holding(UPPER_CASE)),
        "SPACE" => white_space(),
        "PUNCT" => types(P),
        "XDIGIT" => hex_digit(),
        "ALNUM" => holding(ALPHABETIC).union(&types(ND)),
        "CNTRL" => types(CC),
        "DIGIT" => types(ND),
        "BLANK" => types(ZS).union(&Set::of([(0x09, 0x09)])),
        "GRAPH" => types(Z | CC | CS | CN).complement(),
        "PRINT" => types(ZL | ZP | CC | CS | CN).complement(),
        _ => return None,
    })
}

/// A general category, an ASCII POSIX class or a `java…` property, by its
/// exact name.
fn property(name: &str, case_insensitive: bool) -> Option<Set> {
    use category::*;
    use unicode::*;
    let ascii = |runs: &[(u32, u32)]| Set::of(runs.iter().copied());
    Some(match name {
        "Lu" | "Ll" | "Lt" if case_insensitive => types(LU | LL | LT),
        "Cn" => types(CN),
        "Lu" => types(LU),
        "Ll" => types(LL),
        "Lt" => types(LT),
        "Lm" => types(LM),
        "Lo" => types(LO),
        "Mn" => types(MN),
        "Me" => types(ME),
        "Mc" => types(MC),
        "Nd" => types(ND),
        "Nl" => types(NL),
        "No" => types(NO),
        "Zs" => types(ZS),
        "Zl" => types(ZL),
        "Zp" => types(ZP),
        "Cc" => types(CC),
        "Cf" => types(CF),
        "Co" => types(CO),
        "Cs" => types(CS),
        "Pd" => types(PD),
        "Ps" => types(PS),
        "Pe" => types(PE),
        "Pc" => types(PC),
        "Po" => types(PO),
        "Sm" => types(SM),
        "Sc" => types(SC),
        "Sk" => types(SK),
        "So" => types(SO),
        "Pi" => types(PI),
        "Pf" => types(PF),
        "L" => types(L),
        "M" => types(M),
        "N" => types(N),
        "Z" => types(Z),
        "C" => types(C),
        "P" => types(P),
        "S" => types(S),
        "LC" => types(LU | LL | LT),
        "LD" => types(L | ND),
        "L1" => ascii(&[(0x00, 0xFF)]),
        "all" => ascii(&[(0x00, 0x10FFFF)]),
        "ASCII" => ascii(&[(0x00, 0x7F)]),
        "Alnum" => ascii(&[(0x30, 0x39), (0x41, 0x5A), (0x61, 0x7A)]),
        "Alpha" => ascii(&[(0x41, 0x5A), (0x61, 0x7A)]),
        "Blank" => ascii(&[(0x09, 0x09), (0x20, 0x20)]),
        "Cntrl" => ascii(&[(0x00, 0x1F), (0x7F, 0x7F)]),
        "Digit" => ascii(&[(0x30, 0x39)]),
        "Graph" => ascii(&[(0x21, 0x7E)]),
        "Lower" if case_insensitive => ascii(&[(0x41, 0x5A), (0x61, 0x7A)]),
        "Lower" => ascii(&[(0x61, 0x7A)]),
        "Print" => ascii(&[(0x20, 0x7E)]),
        "Punct" => ascii(&[(0x21, 0x2F), (0x3A, 0x40), (0x5B, 0x60), (0x7B, 0x7E)]),
        "Space" => ascii(&[(0x09, 0x0D), (0x20, 0x20)]),
        "Upper" if case_insensitive => ascii(&[(0x41, 0x5A), (0x61, 0x7A)]),
        "Upper" => ascii(&[(0x41, 0x5A)]),
        "XDigit" => ascii(&[(0x30, 0x39), (0x41, 0x46), (0x61, 0x66)]),
        "javaLowerCase" => cased(case_insensitive, holding(LOWER_CASE)),
        "javaUpperCase" => cased(case_insensitive, holding(UPPER_CASE)),
        "javaAlphabetic" => holding(ALPHABETIC),
        "javaIdeographic" => holding(IDEOGRAPHIC),
        "javaTitleCase" => cased(case_insensitive, types(LT)),
        "javaDigit" => types(ND),
        "javaDefined" => types(CN).complement(),
        "javaLetter" => types(L),
        "javaLetterOrDigit" => types(L | ND),
        "javaJavaIdentifierStart" => holding(JAVA_IDENTIFIER_START),
        "javaJavaIdentifierPart" => holding(JAVA_IDENTIFIER_PART),
        "javaUnicodeIdentifierStart" => holding(UNICODE_IDENTIFIER_START),
        "javaUnicodeIdentifierPart" => holding(UNICODE_IDENTIFIER_PART),
        "javaIdentifierIgnorable" => holding(IDENTIFIER_IGNORABLE),
        "javaSpaceChar" => types(Z),
        "javaWhitespace" => holding(WHITESPACE),
        "javaISOControl" => ascii(&[(0x00, 0x1F), (0x7F, 0x9F)]),
        "javaMirrored" => holding(MIRRORED),
        _ => return None,
    })
}

/// A Unicode script, by its name or one of its other names, in any case.
fn script(name: &str) -> Option<Set> {
    let upper = name.to_uppercase();
    let index = names::SCRIPTS
        .iter()
        .position(|(_, others)| others.contains(&upper.as_str()))
        .or_else(|| names::SCRIPTS.iter().position(|(script, _)| *script == upper))?;
    Some(Set::of(unicode::SCRIPTS_OF.iter().filter(|r| usize::from(r.2) == index).map(|r| (r.0, r.1))))
}

/// A Unicode block, by any of its names, in any case.
fn block(name: &str) -> Option<Set> {
    let upper = name.to_uppercase();
    let (_, _, range) = names::BLOCKS.iter().find(|(_, names, _)| names.contains(&upper.as_str()))?;
    Some(Set::of(*range))
}

/// Extended grapheme clusters, as `\X` and `\b{g}` find them.
mod grapheme {
    use super::unicode;
    use std::sync::OnceLock;

    // The types `unicode::GRAPHEME_TYPES` gives a character.
    const OTHER: u8 = 0;
    const CR: u8 = 1;
    const LF: u8 = 2;
    const CONTROL: u8 = 3;
    const EXTEND: u8 = 4;
    const ZWJ: u8 = 5;
    const RI: u8 = 6;
    const PREPEND: u8 = 7;
    const SPACING_MARK: u8 = 8;
    const L: u8 = 9;
    const V: u8 = 10;
    const T: u8 = 11;
    const LV: u8 = 12;
    const LVT: u8 = 13;
    const EXTENDED_PICTOGRAPHIC: u8 = 14;
    const TYPES: usize = 15;

    fn type_of(c: char) -> u8 {
        let (c, runs) = (c as u32, unicode::GRAPHEME_TYPES);
        runs.get(runs.partition_point(|r| r.1 < c)).filter(|r| r.0 <= c).map_or(OTHER, |r| r.2)
    }

    /// Whether a cluster ends between a character of type `a` and one of type
    /// `b` after it, by the two alone.
    fn breaks(a: u8, b: u8) -> bool {
        static RULES: OnceLock<[[bool; TYPES]; TYPES]> = OnceLock::new();
        RULES.get_or_init(|| {
            let mut rules = [[true; TYPES]; TYPES];
            // Hangul syllable sequences.
            for (a, b) in [(L, L), (L, V), (L, LV), (L, LVT), (LV, V), (LV, T), (V, V), (V, T), (LVT, T), (T, T)] {
                rules[usize::from(a)][usize::from(b)] = false;
            }
            // Before an extending character, a ZWJ or a spacing mark; after a
            // prepended character.
            for row in rules.iter_mut() {
                for b in [EXTEND, ZWJ, SPACING_MARK] {
                    row[usize::from(b)] = false;
                }
            }
            rules[usize::from(PREPEND)] = [false; TYPES];
            // Around controls, but within CR LF.
            for c in [CR, LF, CONTROL].map(usize::from) {
                rules[c] = [true; TYPES];
                for row in rules.iter_mut() {
                    row[c] = true;
                }
            }
            rules[usize::from(CR)][usize::from(LF)] = false;
            rules
        })[usize::from(a)][usize::from(b)]
    }

    /// Where the cluster that starts at byte `at` of `text` ends: none at the
    /// end of the text. A cluster that starts with a pictograph takes another
    /// after each ZWJ, and regional indicators pair off.
    pub(super) fn cluster_end(text: &str, at: usize) -> Option<usize> {
        let mut chars = text.get(at..)?.char_indices();
        let (_, first) = chars.next()?;
        let mut end = at + first.len_utf8();
        let mut prev = type_of(first);
        let pictographic = prev == EXTENDED_PICTOGRAPHIC;
        let mut regional = usize::from(prev == RI);
        for (i, c) in chars {
            let next = type_of(c);
            let joins = (pictographic && prev == ZWJ && next == EXTENDED_PICTOGRAPHIC)
                || (regional % 2 == 1 && prev == RI && next == RI)
                || !breaks(prev, next);
            if !joins {
                break;
            }
            regional += usize::from(next == RI);
            prev = next;
            end = at + i + c.len_utf8();
        }
        Some(end)
    }

    /// Whether `\b{g}` holds at byte `at` of `text`, the matcher's previous
    /// match having ended at byte `last`: at the start and the end of the
    /// text, and from where the cluster that starts at `last` ends.
    pub(super) fn is_boundary(text: &str, at: usize, last: usize) -> Result<bool, String> {
        if at == 0 || at >= text.len() {
            return Ok(true);
        }
        match cluster_end(text, last) {
            Some(end) => Ok(end <= at),
            None => Err(format!("`\\b{{g}}` measures from the end of `{text}`")),
        }
    }
}

/// The character names `\N{…}` takes, read as `Character.codePointOf` reads
/// them.
mod charnames {
    use super::names;
    use std::collections::{HashMap, HashSet};
    use std::io::Read;
    use std::sync::OnceLock;

    struct Table {
        code_points: HashMap<String, u32>,
        named: HashSet<u32>,
    }

    /// Every character's own name: java/charnames.z, deflated `HEX;NAME`
    /// lines.
    fn table() -> &'static Table {
        static TABLE: OnceLock<Table> = OnceLock::new();
        TABLE.get_or_init(|| {
            let mut text = String::new();
            flate2::read::ZlibDecoder::new(&include_bytes!("java/charnames.z")[..])
                .read_to_string(&mut text)
                .expect("the character names");
            let mut table = Table { code_points: HashMap::new(), named: HashSet::new() };
            for line in text.lines() {
                let (hex, name) = line.split_once(';').expect("a `HEX;NAME` line");
                let cp = u32::from_str_radix(hex, 16).expect("a code point in hex");
                table.code_points.insert(name.to_string(), cp);
                table.named.insert(cp);
            }
            table
        })
    }

    /// The code point `name` names, in any case and with space around it: a
    /// character by its own name, or one that has none by its block's name and
    /// its code point in hex.
    pub(super) fn code_point_of(name: &str) -> Option<u32> {
        let name = name.trim_matches(|c| c <= ' ').to_uppercase();
        let table = table();
        if let Some(&cp) = table.code_points.get(&name) {
            return Some(cp);
        }
        let (_, hex) = name.rsplit_once(' ')?;
        let cp = u32::from_str_radix(hex, 16).ok().filter(|&cp| cp <= 0x10FFFF)?;
        (!table.named.contains(&cp) && unnamed(cp)? == name).then_some(cp)
    }

    /// The name of a character that has none of its own: its block's name,
    /// with `_` as space, and its code point in hex. None for an unassigned
    /// code point.
    fn unnamed(cp: u32) -> Option<String> {
        if super::is_unassigned(cp) {
            return None;
        }
        let (block, _, _) =
            names::BLOCKS.iter().find(|(_, _, range)| range.is_some_and(|(lo, hi)| lo <= cp && cp <= hi))?;
        Some(format!("{} {cp:X}", block.replace('_', " ")))
    }
}

/// The value of `c` as a decimal digit, as `Character.digit(c, 10)` reads it:
/// a decimal digit of any script, in the Basic Multilingual Plane.
pub(crate) fn decimal_digit(c: char) -> Option<u32> {
    let cp = c as u32;
    if cp > 0xFFFF {
        return None;
    }
    let i = unicode::CATEGORIES.partition_point(|&(_, hi, _)| hi < cp);
    match unicode::CATEGORIES.get(i) {
        // Decimal digits come in runs of ten, from zero.
        Some(&(lo, _, t)) if lo <= cp && 1 << t == category::ND => Some((cp - lo) % 10),
        _ => None,
    }
}

/// Whether `c` is whitespace as `Character.isWhitespace` judges it.
pub(crate) fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r' | '\u{1C}'..='\u{1F}' | ' ' | '\u{1680}' | '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200A}'
            | '\u{2028}' | '\u{2029}' | '\u{205F}' | '\u{3000}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_writes_what_java_writes() {
        let args = ["ab", "cd", "efgh", "ij", "x", "y"];
        assert_eq!(
            format("[%5s|%-5s|%.2s|%S|%b|%h|%%|%2$s|%<s]", &args).unwrap(),
            "[   ab|cd   |ef|IJ|true|79|%|cd|cd]"
        );
        assert_eq!(format("50%% of %s%n", &["it"]).unwrap(), "50% of it\n");
        assert_eq!(format("no vars %%", &[]).unwrap(), "no vars %");
        assert!(format("%s and %s", &["one"]).is_err());
        assert!(format("%d", &["1"]).is_err());
        assert!(format("ends %", &[]).is_err());
        assert!(format("%q", &[]).is_err());
        assert!(format("%-s", &["a"]).is_err());
        assert!(format("%05s", &["a"]).is_err());
        assert_eq!(format("%s", &["50%s"]).unwrap(), "50%s");
    }

    #[test]
    fn regex_matches_as_java_matches() {
        let r = Regex::new(r"^(\w+)\s(\d+)$").unwrap();
        let m = r.find("abc 123").unwrap().unwrap();
        assert_eq!(m.group(1), Some("abc"));
        // `\w` and `\d` are ASCII.
        assert!(r.find("é 1").unwrap().is_none());
        assert!(r.find("a ١").unwrap().is_none());
        // `$` matches before a final line terminator; `.` stops at one.
        assert!(Regex::new("a$").unwrap().find("a\n").unwrap().is_some());
        assert!(Regex::new("a.b").unwrap().find("a\rb").unwrap().is_none());
        // Possessive quantifiers, lookaround and backreferences.
        assert!(Regex::new("a*+a").unwrap().find("aaa").unwrap().is_none());
        assert!(Regex::new(r"(?<=x)(a)\1").unwrap().find("xaa").unwrap().is_some());
        // ASCII-only case folding unless `u`.
        assert!(Regex::new("(?i)é").unwrap().find("É").unwrap().is_none());
        assert!(Regex::new("(?iu)é").unwrap().find("É").unwrap().is_some());
        assert!(Regex::new("(?i)[a-c]").unwrap().find("B").unwrap().is_some());
    }

    #[test]
    fn replacement_expands_as_java_expands() {
        let r = Regex::new(r"\\(\d+)").unwrap();
        let m = r.find(r"x\12y").unwrap().unwrap();
        assert_eq!(r.expand(&m, "a$1b").unwrap(), "a12b");
        assert_eq!(r.expand(&m, "$12").unwrap(), "122");
        assert_eq!(r.expand(&m, r"\$0").unwrap(), "$0");
        assert!(r.expand(&m, "cost $").is_err());
        assert!(r.expand(&m, "$2").is_err());
        assert!(r.expand(&m, "${x}").is_err());
    }
}

#[cfg(test)]
mod recorded {
    use super::{Regex, Translator};

    /// A field of the cases file: `\uXXXX` stands for a UTF-16 code unit.
    fn unescape(s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        let mut units: Vec<u16> = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\\' && chars.get(i + 1) == Some(&'u') && i + 6 <= chars.len() {
                let hex: String = chars[i + 2..i + 6].iter().collect();
                if let Ok(unit) = u16::from_str_radix(&hex, 16) {
                    units.push(unit);
                    i += 6;
                    continue;
                }
            }
            let mut buf = [0u16; 2];
            units.extend_from_slice(chars[i].encode_utf16(&mut buf));
            i += 1;
        }
        String::from_utf16_lossy(&units)
    }

    fn utf16_at(text: &str, byte: usize) -> usize {
        text[..byte].encode_utf16().count()
    }

    /// What `pattern` matches on its own of every code point, when it reads as
    /// a class: how many, and the FNV-1a hash of their runs written
    /// `FIRST-LAST,` in hex.
    fn class_digest(pattern: &str) -> String {
        let mut t = Translator::new(pattern);
        if t.translate().is_err() || Regex::new(pattern).is_err() {
            return "E".to_string();
        }
        let hir = match regex_syntax::Parser::new().parse(&t.out) {
            Ok(hir) => hir,
            Err(e) => return format!("unread: {e}"),
        };
        use regex_syntax::hir::{Class, HirKind};
        let ranges: Vec<(u32, u32)> = match hir.kind() {
            HirKind::Class(Class::Unicode(class)) => {
                class.ranges().iter().map(|r| (r.start() as u32, r.end() as u32)).collect()
            }
            HirKind::Class(Class::Bytes(class)) if class.ranges().is_empty() => Vec::new(),
            HirKind::Literal(literal) => match std::str::from_utf8(&literal.0).map(|s| s.chars().collect::<Vec<_>>()) {
                Ok(chars) if chars.len() == 1 => vec![(chars[0] as u32, chars[0] as u32)],
                _ => return format!("not a class: {}", t.out),
            },
            _ => return format!("not a class: {}", t.out),
        };
        let (mut count, mut runs) = (0, String::new());
        for (lo, hi) in ranges {
            for (lo, hi) in [(lo, hi.min(0xD7FF)), (lo.max(0xE000), hi)] {
                if lo <= hi {
                    count += hi - lo + 1;
                    runs.push_str(&format!("{lo:X}-{hi:X},"));
                }
            }
        }
        let hash = runs.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
        format!("{count}\t{hash:016x}")
    }

    /// The recorded cases this reading differs on, by their kind and a text
    /// their pattern holds, and why.
    const DIFFERENT: &[(&str, &str, &str)] = &[
        ("P", r"(?U)\B", "no match starts inside a surrogate pair"),
        ("F", r"(?=\uD83D\uDE00)|.", "no match starts inside a surrogate pair"),
        ("M", r"(?<=\p{So})", "a look-behind counts code points, where Java counts UTF-16 units and starts inside a pair"),
        ("M", r"(?=\X)\X\b{g}", r"`\b{g}` measures from where the previous match ended, not where a look-ahead did"),
    ];

    /// Each recorded pattern's translation leaves the regex crate nothing of
    /// its own to look up: no Unicode property, Perl class or word boundary,
    /// and no case-insensitive flag. What a character is, and its case, come
    /// from Java's tables alone.
    #[test]
    fn translations_name_no_table_of_the_regex_crate() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/java-regex/cases.tsv");
        let text = std::fs::read_to_string(path).expect("the recorded cases");
        let mut patterns: Vec<String> =
            text.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split('\t').nth(1)).map(unescape).collect();
        patterns.sort();
        patterns.dedup();
        let mut named = Vec::new();
        for pattern in &patterns {
            let mut t = Translator::new(pattern);
            if t.translate().is_err() {
                continue;
            }
            let out: Vec<char> = t.out.chars().collect();
            let mut i = 0;
            while i < out.len() {
                let flagged = match out[i] {
                    '\\' => {
                        i += 1;
                        out.get(i).is_some_and(|c| "pPwWdDsSbB".contains(*c))
                    }
                    '(' if out.get(i + 1) == Some(&'?') => {
                        out[i + 2..].iter().take_while(|c| c.is_ascii_alphabetic() || **c == '-').any(|c| *c == 'i')
                    }
                    _ => false,
                };
                if flagged {
                    named.push(format!("{pattern:?} -> {}", t.out));
                    break;
                }
                i += 1;
            }
        }
        assert!(patterns.len() > 3000, "only {} patterns", patterns.len());
        assert!(named.is_empty(), "{} translations name the regex crate's tables:\n{}", named.len(), named.join("\n"));
    }

    /// Every case scripts/gen_java_regex.sh recorded from `java.util.regex`
    /// into tests/fixtures/java-regex/cases.tsv, read as that script writes
    /// it, but those `DIFFERENT` names; and each of those still differs.
    #[test]
    fn patterns_match_as_recorded() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/java-regex/cases.tsv");
        let text = std::fs::read_to_string(path).expect("the recorded cases");
        let mut probes: Vec<String> = Vec::new();
        let mut wrong = Vec::new();
        let mut different = vec![0; DIFFERENT.len()];
        let mut checked = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f[0] == "# probes" {
                probes = f[1..].iter().map(|p| unescape(p)).collect();
                continue;
            }
            if f[0].starts_with('#') {
                continue;
            }
            checked += 1;
            let pattern = unescape(f[1]);
            let regex = Regex::new(&pattern);
            let (got, want) = match f[0] {
                "P" | "D" => {
                    let got = match &regex {
                        Err(_) => "E".to_string(),
                        Ok(r) => probes
                            .chunks(4)
                            .map(|chunk| {
                                let mut nibble = 0u32;
                                for i in 0..4 {
                                    let found = chunk.get(i).is_some_and(|p| {
                                        let input = if f[0] == "D" { p.repeat(2) } else { p.clone() };
                                        matches!(r.find(&input), Ok(Some(_)))
                                    });
                                    nibble = nibble << 1 | u32::from(found);
                                }
                                char::from_digit(nibble, 16).unwrap_or('?')
                            })
                            .collect(),
                    };
                    (got, f[2].to_string())
                }
                "C" => (class_digest(&pattern), f[2..].join("\t")),
                "F" => {
                    let input = unescape(f[2]);
                    let got = match &regex {
                        Err(_) => "E".to_string(),
                        Ok(r) => match r.find_all(&input) {
                            Err(_) => "X".to_string(),
                            Ok(all) if all.is_empty() => "N".to_string(),
                            Ok(all) => all
                                .iter()
                                .map(|m| format!("{},{}", utf16_at(&input, m.start()), utf16_at(&input, m.end())))
                                .collect::<Vec<_>>()
                                .join(";"),
                        },
                    };
                    (got, f[3].to_string())
                }
                "M" => {
                    let input = unescape(f[2]);
                    let got = match &regex {
                        Err(_) => "E".to_string(),
                        Ok(r) => match r.find(&input) {
                            Err(_) => "X".to_string(),
                            Ok(None) => "N".to_string(),
                            Ok(Some(m)) => (0..=r.group_count())
                                .map(|g| match m.spans[g] {
                                    Some((s, e)) => format!("{},{}", utf16_at(&input, s), utf16_at(&input, e)),
                                    None => "-".to_string(),
                                })
                                .collect::<Vec<_>>()
                                .join(";"),
                        },
                    };
                    (got, f[3].to_string())
                }
                "R" => {
                    let (input, replacement) = (unescape(f[2]), unescape(f[3]));
                    let got = match regex.and_then(|r| r.replace_first(&input, &replacement)) {
                        Err(_) => "E".to_string(),
                        Ok(out) => out,
                    };
                    (got, unescape(f[4]))
                }
                kind => panic!("a case of kind `{kind}`"),
            };
            match DIFFERENT.iter().position(|(kind, text, _)| *kind == f[0] && pattern.contains(text)) {
                Some(i) if got != want => different[i] += 1,
                Some(i) => wrong.push(format!("{} {pattern:?} no longer differs ({})", f[0], DIFFERENT[i].2)),
                None if got != want => wrong.push(format!("{} {pattern:?}: recorded {want:?}, got {got:?}", f[0])),
                None => {}
            }
        }
        for (n, (_, text, why)) in different.iter().zip(DIFFERENT) {
            if *n == 0 {
                wrong.push(format!("no recorded case that holds `{text}` differs ({why})"));
            }
        }
        assert!(checked > 4000, "only {checked} cases");
        assert!(wrong.is_empty(), "{} of {checked} cases differ:\n{}", wrong.len(), wrong.join("\n"));
    }
}
