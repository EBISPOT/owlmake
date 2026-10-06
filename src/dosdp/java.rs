//! The text dialects a DOSDP pattern is written in: its printf text is a
//! `java.util.Formatter` format, its substitutions are `java.util.regex`
//! patterns whose replacements follow `Matcher.appendReplacement`, and the
//! literals of its logical axioms take Java's number syntax and printing.
//!
//! Strings are measured in UTF-16 code units wherever the dialect measures
//! them (a `%.3s` precision, a `%5s` width).

use std::collections::HashMap;

mod names;

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
/// letters only unless the pattern asks for Unicode case (`(?u)`). It refuses
/// `\X` and `\N{…}`, and a case-insensitive back-reference matches text of
/// its own length in UTF-8, in any case.
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
        let inner = fancy_regex::RegexBuilder::new(&t.out)
            .backtrack_limit(10_000_000)
            .build()
            .map_err(|e| format!("`{pattern}` is not a regular expression this reads: {e}"))?;
        Ok(Regex { inner, groups: t.groups, names: t.names })
    }

    /// The number of capturing groups.
    pub(crate) fn group_count(&self) -> usize {
        self.groups
    }

    /// The first match in `text`.
    pub(crate) fn find<'t>(&self, text: &'t str) -> Result<Option<Match<'t>>, String> {
        let caps = self.inner.captures(text).map_err(|e| format!("matching `{text}`: {e}"))?;
        Ok(caps.map(|c| Match {
            text,
            spans: (0..=self.groups).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect(),
        }))
    }

    /// Every match in `text`, left to right, each starting where the last
    /// ended (one further on after an empty match).
    pub(crate) fn find_all<'t>(&self, text: &'t str) -> Result<Vec<Match<'t>>, String> {
        let mut out = Vec::new();
        for c in self.inner.captures_iter(text) {
            let c = c.map_err(|e| format!("matching `{text}`: {e}"))?;
            out.push(Match {
                text,
                spans: (0..=self.groups).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect(),
            });
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

/// What a `\` introduces.
enum Escape {
    /// A character: outside a class, one of a run of literal characters;
    /// inside one, a single character or a range's end. A surrogate code
    /// point is one.
    Char(u32),
    /// Any other construct, as `fancy_regex` text; inside a class, class items.
    Text(String),
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
}

const LINE_TERMINATORS: &str = r"\n\r\x{85}\x{2028}\x{2029}";

/// A class that holds no character.
const NOTHING: &str = r"[^\x00-\x{10FFFF}]";

/// Outside `(?U)`, `\b` takes ASCII letters, digits and `_` for word
/// characters, and a nonspacing mark that follows a letter or digit (of any
/// script) through a run of marks. Whether the character before a position
/// is one, and whether the one after it is.
const WORD_BEFORE: &str = r"(?<=[0-9A-Za-z_]|[\p{L}\p{Nd}]\p{Mn}+)";
const NO_WORD_BEFORE: &str = r"(?<![0-9A-Za-z_]|[\p{L}\p{Nd}]\p{Mn}+)";
const WORD_AFTER: &str = r"(?:(?=[0-9A-Za-z_])|(?=\p{Mn})(?<=[\p{L}\p{Nd}]\p{Mn}*))";
const NO_WORD_AFTER: &str = r"(?![0-9A-Za-z_])(?:(?!\p{Mn})|(?<![\p{L}\p{Nd}]\p{Mn}*))";

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
                        }
                    }
                }
                '[' => {
                    let start = self.out.len();
                    let class = self.class()?;
                    self.out.push_str(&class);
                    self.atom_start = Some(start);
                }
                '(' => self.open_group()?,
                ')' => {
                    let Some(f) = self.saved.pop() else { return self.err("an unmatched `)`") };
                    self.flags = f;
                    self.out.push(')');
                    // The atom is the group: find its `(` by balance.
                    self.atom_start = Some(self.group_start());
                }
                '.' => {
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
                    self.out.push_str(match (self.flags.multiline, self.flags.unix_lines) {
                        (false, _) => r"\A",
                        (true, true) => r"(?:\A|(?<=\n)(?!\z))",
                        (true, false) => r"(?:\A|(?<=[\n\x{85}\x{2028}\x{2029}])(?!\z)|(?<=\r)(?!\n)(?!\z))",
                    });
                    self.atom_start = None;
                }
                '$' => {
                    self.out.push_str(match (self.flags.multiline, self.flags.unix_lines) {
                        (false, true) => r"(?:\z|(?=\n\z))",
                        (false, false) => {
                            r"(?:\z|(?=\r\n\z)|(?<!\r)(?=\n\z)|(?=[\r\x{85}\x{2028}\x{2029}]\z))"
                        }
                        (true, true) => r"(?:\z|(?=\n))",
                        (true, false) => r"(?:\z|(?<!\r)(?=\n)|(?=[\r\x{85}\x{2028}\x{2029}]))",
                    });
                    self.atom_start = None;
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
                        Escape::Text(_) => {
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

    /// The start in `out` of the group just closed.
    fn group_start(&self) -> usize {
        let bytes = self.out.as_bytes();
        let mut depth = 0usize;
        let mut i = bytes.len();
        while i > 0 {
            i -= 1;
            let escaped = i > 0 && {
                let mut n = 0;
                let mut j = i;
                while j > 0 && bytes[j - 1] == b'\\' {
                    n += 1;
                    j -= 1;
                }
                n % 2 == 1
            };
            if escaped {
                continue;
            }
            match bytes[i] {
                b')' => depth += 1,
                b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
        }
        0
    }

    fn quantifier(&mut self, q: String) -> Result<(), String> {
        let Some(start) = self.atom_start else { return self.err("a quantifier with nothing to repeat") };
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
        if self.peek() != Some('?') {
            self.groups += 1;
            self.out.push('(');
            return Ok(());
        }
        self.pos += 1;
        match self.next() {
            Some(':') => self.out.push_str("(?:"),
            Some('=') => self.out.push_str("(?="),
            Some('!') => self.out.push_str("(?!"),
            Some('>') => self.out.push_str("(?>"),
            Some('<') => match self.peek() {
                Some('=') => {
                    self.pos += 1;
                    self.out.push_str("(?<=");
                }
                Some('!') => {
                    self.pos += 1;
                    self.out.push_str("(?<!");
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
                    self.names.insert(name.clone(), self.groups);
                    self.out.push_str(&format!("(?<{name}>"));
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
                            return Ok(());
                        }
                        Some(':') => {
                            self.out.push_str("(?:");
                            return Ok(());
                        }
                        _ => return self.err("an unknown inline flag"),
                    }
                }
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
        let text = |s: &str| Escape::Text(s.to_string());
        Ok(match c {
            'w' | 'W' if ascii => set("0-9A-Za-z_", c == 'W'),
            'd' | 'D' if ascii => set("0-9", c == 'D'),
            's' | 'S' if ascii => set(r"\t\n\x0B\x0C\r ", c == 'S'),
            'w' | 'W' | 'd' | 'D' | 's' | 'S' => {
                let class = format!(r"\{c}");
                Escape::Text(if in_class { class } else { format!("[{class}]") })
            }
            'h' | 'H' => set(r" \t\xA0\x{1680}\x{180E}\x{2000}-\x{200A}\x{202F}\x{205F}\x{3000}", c == 'H'),
            'v' | 'V' => set(r"\n\x0B\x0C\r\x{85}\x{2028}\x{2029}", c == 'V'),
            'b' | 'B' if !in_class => Escape::Text(if ascii {
                if c == 'b' {
                    format!("(?:{WORD_BEFORE}{NO_WORD_AFTER}|{NO_WORD_BEFORE}{WORD_AFTER})")
                } else {
                    format!("(?:{WORD_BEFORE}{WORD_AFTER}|{NO_WORD_BEFORE}{NO_WORD_AFTER})")
                }
            } else {
                format!(r"\{c}")
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
            't' => Escape::Char(0x09),
            'n' => Escape::Char(0x0A),
            'r' => Escape::Char(0x0D),
            'f' => Escape::Char(0x0C),
            'a' => Escape::Char(0x07),
            'e' => Escape::Char(0x1B),
            '0' => Escape::Char(self.octal()?),
            'x' => Escape::Char(self.hex()?),
            'u' => Escape::Char(self.utf16_escape()?),
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
                let items = family(&name, self.flags.case_insensitive, self.flags.unicode_classes)
                    .map_or_else(|| self.err(&format!("the character property `{name}`, which names none")), Ok)?;
                set(&items, c == 'P')
            }
            'k' if !in_class => {
                if self.next() != Some('<') {
                    return self.err("a malformed `\\k<name>`");
                }
                let name = self.take_while(|c| c != '>');
                if self.next() != Some('>') || !self.names.contains_key(&name) {
                    return self.err(&format!("a reference to no group named `{name}`"));
                }
                Escape::Text(self.backref(&format!(r"\k<{name}>")))
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
                    Escape::Text(self.backref(&format!(r"\{n}")))
                }
            }
            c if c.is_ascii_alphanumeric() => return self.err(&format!("the escape `\\{c}`")),
            c => Escape::Char(c as u32),
        })
    }

    /// A back-reference: under case-insensitive matching, to the group's text
    /// in any case.
    fn backref(&self, reference: &str) -> String {
        if self.flags.case_insensitive {
            format!("(?i:{reference})")
        } else {
            format!("(?:{reference})")
        }
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
                    Escape::Text(_) => return self.err("a character range that ends in a class"),
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
    use std::sync::OnceLock;

    /// The uppercase of `c`.
    pub(super) fn upper(c: u32) -> u32 {
        match c {
            // A capital with prosgegrammeni: these map to two characters in
            // full, to one here.
            0x1F80..=0x1F87 | 0x1F90..=0x1F97 | 0x1FA0..=0x1FA7 => c + 8,
            0x1FB3 | 0x1FC3 | 0x1FF3 => c + 9,
            _ => one(char::from_u32(c).map(char::to_uppercase)).unwrap_or(c),
        }
    }

    /// The lowercase of `c`.
    pub(super) fn lower(c: u32) -> u32 {
        match c {
            0x130 => 0x69,
            _ => one(char::from_u32(c).map(char::to_lowercase)).unwrap_or(c),
        }
    }

    /// `c`'s folded case: the lowercase of its uppercase.
    pub(super) fn fold(c: u32) -> u32 {
        lower(upper(c))
    }

    /// The one character `chars` holds; none if it holds several.
    fn one(chars: Option<impl Iterator<Item = char>>) -> Option<u32> {
        let mut chars = chars?;
        let c = chars.next()?;
        chars.next().is_none().then_some(c as u32)
    }

    /// Every character whose uppercase or folded case is another character,
    /// with its uppercase and its folded case.
    pub(super) fn cased() -> &'static [(u32, u32, u32)] {
        static CASED: OnceLock<Vec<(u32, u32, u32)>> = OnceLock::new();
        CASED.get_or_init(|| {
            // No character past the first two planes has a case.
            (0..0x20000)
                .filter_map(|c| {
                    let (upper, folded) = (upper(c), fold(c));
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

// ── Character properties ────────────────────────────────────────────────────

/// What `\p{name}` names, as class items, under the flags in force: a Unicode
/// script, block or general category, a POSIX class (ASCII, or Unicode under
/// `(?U)` and with an `Is`), or a `java…` property of `Character`. None when
/// the name names nothing.
fn family(name: &str, case_insensitive: bool, unicode_classes: bool) -> Option<String> {
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

/// Every letter with a case, which is what the case properties name
/// case-insensitively.
const ANY_CASE: &str = r"\p{Lowercase}\p{Uppercase}\p{Lt}";
const WHITE_SPACE: &str = r"\p{Z}\t\n\x0B\x0C\r\x{85}";
const HEX_DIGIT: &str = r"\p{Nd}0-9A-Fa-f\x{FF10}-\x{FF19}\x{FF21}-\x{FF26}\x{FF41}-\x{FF46}";
const IDENTIFIER_IGNORABLE: &str = r"\x00-\x08\x0E-\x1B\x7F-\x9F\p{Cf}";

fn cased(case_insensitive: bool, items: &str) -> String {
    (if case_insensitive { ANY_CASE } else { items }).to_string()
}

/// A Unicode property `\p{IsName}` names, `name` in upper case.
fn unicode_property(name: &str, case_insensitive: bool) -> Option<String> {
    Some(match name {
        "ALPHABETIC" => r"\p{Alphabetic}".into(),
        "ASSIGNED" => r"\P{Cn}".into(),
        "CONTROL" => r"\p{Cc}".into(),
        "EMOJI" => r"\p{Emoji}".into(),
        "EMOJI_PRESENTATION" => r"\p{Emoji_Presentation}".into(),
        "EMOJI_MODIFIER" => r"\p{Emoji_Modifier}".into(),
        "EMOJI_MODIFIER_BASE" => r"\p{Emoji_Modifier_Base}".into(),
        "EMOJI_COMPONENT" => r"\p{Emoji_Component}".into(),
        "EXTENDED_PICTOGRAPHIC" => r"\p{Extended_Pictographic}".into(),
        "HEXDIGIT" | "HEX_DIGIT" => HEX_DIGIT.into(),
        "IDEOGRAPHIC" => r"\p{Ideographic}".into(),
        "JOINCONTROL" | "JOIN_CONTROL" => r"\x{200C}\x{200D}".into(),
        "LETTER" => r"\p{L}".into(),
        "LOWERCASE" => cased(case_insensitive, r"\p{Lowercase}"),
        "NONCHARACTERCODEPOINT" | "NONCHARACTER_CODE_POINT" => r"\p{Noncharacter_Code_Point}".into(),
        "TITLECASE" => cased(case_insensitive, r"\p{Lt}"),
        "PUNCTUATION" => r"\p{P}".into(),
        "UPPERCASE" => cased(case_insensitive, r"\p{Uppercase}"),
        "WHITESPACE" | "WHITE_SPACE" => WHITE_SPACE.into(),
        "WORD" => r"\p{Alphabetic}\p{M}\p{Nd}\p{Pc}\x{200C}\x{200D}".into(),
        _ => return None,
    })
}

/// A POSIX class with Unicode members, `name` in upper case.
fn posix(name: &str, case_insensitive: bool) -> Option<String> {
    Some(match name {
        "ALPHA" => r"\p{Alphabetic}".into(),
        "LOWER" => cased(case_insensitive, r"\p{Lowercase}"),
        "UPPER" => cased(case_insensitive, r"\p{Uppercase}"),
        "SPACE" => WHITE_SPACE.into(),
        "PUNCT" => r"\p{P}".into(),
        "XDIGIT" => HEX_DIGIT.into(),
        "ALNUM" => r"\p{Alphabetic}\p{Nd}".into(),
        "CNTRL" => r"\p{Cc}".into(),
        "DIGIT" => r"\p{Nd}".into(),
        "BLANK" => r"\p{Zs}\t".into(),
        "GRAPH" => complement(r"\p{Z}\p{Cc}\p{Cn}"),
        "PRINT" => complement(r"\p{Zl}\p{Zp}\p{Cc}\p{Cn}"),
        _ => return None,
    })
}

/// A general category, an ASCII POSIX class or a `java…` property, by its
/// exact name.
fn property(name: &str, case_insensitive: bool) -> Option<String> {
    Some(match name {
        "Lu" | "Ll" | "Lt" if case_insensitive => r"\p{Lu}\p{Ll}\p{Lt}".into(),
        "Cn" | "Lu" | "Ll" | "Lt" | "Lm" | "Lo" | "Mn" | "Me" | "Mc" | "Nd" | "Nl" | "No" | "Zs" | "Zl" | "Zp"
        | "Cc" | "Cf" | "Co" | "Pd" | "Ps" | "Pe" | "Pc" | "Po" | "Sm" | "Sc" | "Sk" | "So" | "Pi" | "Pf" | "L"
        | "M" | "N" | "Z" | "P" | "S" => format!(r"\p{{{name}}}"),
        // No text holds a surrogate.
        "Cs" => NOTHING.into(),
        "C" => r"\p{Cc}\p{Cf}\p{Co}\p{Cn}".into(),
        "LC" => r"\p{Lu}\p{Ll}\p{Lt}".into(),
        "LD" => r"\p{L}\p{Nd}".into(),
        "L1" => r"\x00-\xFF".into(),
        "all" => r"\x00-\x{10FFFF}".into(),
        "ASCII" => r"\x00-\x7F".into(),
        "Alnum" => "0-9A-Za-z".into(),
        "Alpha" => "A-Za-z".into(),
        "Blank" => r"\x20\t".into(),
        "Cntrl" => r"\x00-\x1F\x7F".into(),
        "Digit" => "0-9".into(),
        "Graph" => "!-~".into(),
        "Lower" => (if case_insensitive { "A-Za-z" } else { "a-z" }).into(),
        "Print" => r"\x20-~".into(),
        "Punct" => r"!-/:-@\[-`\{-~".into(),
        "Space" => r"\t\n\x0B\x0C\r\x20".into(),
        "Upper" => (if case_insensitive { "A-Za-z" } else { "A-Z" }).into(),
        "XDigit" => "0-9A-Fa-f".into(),
        "javaLowerCase" => cased(case_insensitive, r"\p{Lowercase}"),
        "javaUpperCase" => cased(case_insensitive, r"\p{Uppercase}"),
        "javaAlphabetic" => r"\p{Alphabetic}".into(),
        "javaIdeographic" => r"\p{Ideographic}".into(),
        "javaTitleCase" => cased(case_insensitive, r"\p{Lt}"),
        "javaDigit" => r"\p{Nd}".into(),
        "javaDefined" => r"\P{Cn}".into(),
        "javaLetter" => r"\p{L}".into(),
        "javaLetterOrDigit" => r"\p{L}\p{Nd}".into(),
        "javaJavaIdentifierStart" => r"\p{L}\p{Nl}\p{Sc}\p{Pc}".into(),
        "javaJavaIdentifierPart" => format!(r"\p{{L}}\p{{Nl}}\p{{Sc}}\p{{Pc}}\p{{Nd}}\p{{Mc}}\p{{Mn}}{IDENTIFIER_IGNORABLE}"),
        "javaUnicodeIdentifierStart" => r"\p{ID_Start}\x{2E2F}".into(),
        "javaUnicodeIdentifierPart" => format!(r"\p{{ID_Continue}}\x{{2E2F}}{IDENTIFIER_IGNORABLE}"),
        "javaIdentifierIgnorable" => IDENTIFIER_IGNORABLE.into(),
        "javaSpaceChar" => r"\p{Z}".into(),
        "javaWhitespace" => r"\t-\r\x1C-\x1F[\p{Z}--[\xA0\x{2007}\x{202F}]]".into(),
        "javaISOControl" => r"\x00-\x1F\x7F-\x9F".into(),
        "javaMirrored" => r"\p{Bidi_Mirrored}".into(),
        _ => return None,
    })
}

/// A Unicode script, by its name or one of its other names, in any case.
fn script(name: &str) -> Option<String> {
    let upper = name.to_uppercase();
    let (script, _) = names::SCRIPTS
        .iter()
        .find(|(_, others)| others.contains(&upper.as_str()))
        .or_else(|| names::SCRIPTS.iter().find(|(script, _)| *script == upper))?;
    Some(match *script {
        // The script of every code point no script claims.
        "UNKNOWN" => r"\p{Cn}\p{Co}".into(),
        script => format!(r"\p{{sc={script}}}"),
    })
}

/// A Unicode block, by any of its names, in any case.
fn block(name: &str) -> Option<String> {
    let upper = name.to_uppercase();
    let (_, range) = names::BLOCKS.iter().find(|(names, _)| names.contains(&upper.as_str()))?;
    Some(match range {
        Some((lo, hi)) => range_items(*lo, *hi),
        None => NOTHING.into(),
    })
}

// ── Numbers ─────────────────────────────────────────────────────────────────

/// `s` as an `int`, as `Integer.parseInt` reads one: an optional sign and
/// decimal digits, in range.
pub(crate) fn parse_int(s: &str) -> Option<i32> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let body = s.strip_prefix('+').unwrap_or(s);
    body.parse::<i32>().ok()
}

/// Whether `s` is a floating-point number as `Double.parseDouble` reads one,
/// and its text with any `f`/`d` suffix removed, for Rust to read.
fn java_float_text(s: &str) -> Option<String> {
    let t = s.trim_matches(|c: char| c <= ' ');
    let (sign, body) = match t.strip_prefix(['+', '-']) {
        Some(b) => (&t[..1], b),
        None => ("", t),
    };
    if body == "NaN" || body == "Infinity" {
        return Some(format!("{sign}{}", if body == "NaN" { "NaN" } else { "inf" }));
    }
    let body = body.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(body);
    let lower = body.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        return hex_float(hex).map(|v| format!("{sign}{v:e}"));
    }
    let (mantissa, exponent) = match lower.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (lower.as_str(), None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int.is_empty() && frac.is_empty()
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    if let Some(e) = exponent {
        let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    Some(format!("{sign}{}{}", if int.is_empty() { "0" } else { int }, &mantissa[int.len()..])
        + &exponent.map(|e| format!("e{e}")).unwrap_or_default())
}

/// A hexadecimal floating-point body (after `0x`): hex digits with an optional
/// point, then a binary exponent `p±N`.
fn hex_float(s: &str) -> Option<f64> {
    let (m, e) = s.split_once('p')?;
    let e: i32 = e.parse().ok()?;
    let (int, frac) = m.split_once('.').unwrap_or((m, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let mut v = 0f64;
    for c in int.chars() {
        v = v * 16.0 + c.to_digit(16)? as f64;
    }
    let mut scale = 1.0 / 16.0;
    for c in frac.chars() {
        v += c.to_digit(16)? as f64 * scale;
        scale /= 16.0;
    }
    Some(v * 2f64.powi(e))
}

/// Whether `Double.parseDouble` reads `s`.
pub(crate) fn is_double(s: &str) -> bool {
    java_float_text(s).is_some_and(|t| t.parse::<f64>().is_ok())
}

/// `s` read as `Float.parseFloat` reads it.
pub(crate) fn parse_float(s: &str) -> Option<f32> {
    java_float_text(s)?.parse::<f32>().ok()
}

/// `s` read as `Double.parseDouble` reads it.
pub(crate) fn parse_double(s: &str) -> Option<f64> {
    java_float_text(s)?.parse::<f64>().ok()
}

/// `v` printed as `Float.toString` prints it.
pub(crate) fn float_to_string(v: f32) -> String {
    java_decimal(v.is_nan(), v.is_infinite(), v.is_sign_negative(), v == 0.0, &format!("{:e}", v.abs()))
}

/// `v` printed as `Double.toString` prints it.
pub(crate) fn double_to_string(v: f64) -> String {
    java_decimal(v.is_nan(), v.is_infinite(), v.is_sign_negative(), v == 0.0, &format!("{:e}", v.abs()))
}

/// The shortest digits of a finite number (`sci` as Rust's `{:e}` prints its
/// magnitude) in Java's layout: plain from 10⁻³ up to 10⁷, else `d.dddE±n`,
/// with at least one digit after the point.
fn java_decimal(nan: bool, infinite: bool, negative: bool, zero: bool, sci: &str) -> String {
    if nan {
        return "NaN".to_string();
    }
    let sign = if negative { "-" } else { "" };
    if infinite {
        return format!("{sign}Infinity");
    }
    if zero {
        return format!("{sign}0.0");
    }
    let (m, e) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = e.parse().unwrap_or(0);
    let digits: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
    let body = if (-3..7).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            let int: String = digits.chars().chain(std::iter::repeat('0')).take(int_len).collect();
            let frac = if digits.len() > int_len { &digits[int_len..] } else { "0" };
            format!("{int}.{frac}")
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let frac = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{}.{frac}E{exp}", &digits[..1])
    };
    format!("{sign}{body}")
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

    #[test]
    fn numbers_print_as_java_prints_them() {
        assert_eq!(parse_int("+5"), Some(5));
        assert_eq!(parse_int("007"), Some(7));
        assert_eq!(parse_int("3000000000"), None);
        assert!(is_double("1e5") && is_double("1.5d") && is_double(".5") && !is_double("inf"));
        assert_eq!(float_to_string(1.5), "1.5");
        assert_eq!(float_to_string(1.0), "1.0");
        assert_eq!(float_to_string(1e10), "1.0E10");
        assert_eq!(float_to_string(0.0001), "1.0E-4");
        assert_eq!(float_to_string(0.001), "0.001");
        assert_eq!(double_to_string(1234567.0), "1234567.0");
        assert_eq!(double_to_string(12345678.0), "1.2345678E7");
    }
}

#[cfg(test)]
mod recorded {
    use super::Regex;

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

    /// The recorded cases this reading differs on, by a text their pattern
    /// holds, and why.
    const DIFFERENT: &[(&str, &str)] = &[
        (r"\X", r"a grapheme cluster `\X` is refused"),
        (r"\N{", r"a character named `\N{…}` is refused"),
        (r"(?U)\B", "no match starts inside a surrogate pair"),
        (r"(?i)(é)\1", "a back-reference folds the case of other than ASCII letters without `u`"),
        (r"(?iu)(k)\1", "a back-reference matches only text of its own length in UTF-8"),
    ];

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
                "M" => {
                    let input = unescape(f[2]);
                    let got = match &regex {
                        Err(_) => "E".to_string(),
                        Ok(r) => match r.find(&input) {
                            Err(e) => format!("failed: {e}"),
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
            match DIFFERENT.iter().position(|(text, _)| pattern.contains(text)) {
                Some(i) if got != want => different[i] += 1,
                Some(i) => wrong.push(format!("{} {pattern:?} no longer differs ({})", f[0], DIFFERENT[i].1)),
                None if got != want => wrong.push(format!("{} {pattern:?}: recorded {want:?}, got {got:?}", f[0])),
                None => {}
            }
        }
        for (n, (text, why)) in different.iter().zip(DIFFERENT) {
            assert!(*n > 0, "no recorded case holds `{text}` ({why})");
        }
        assert!(checked > 4000, "only {checked} cases");
        assert!(wrong.is_empty(), "{} of {checked} cases differ:\n{}", wrong.len(), wrong.join("\n"));
    }
}
