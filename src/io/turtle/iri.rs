//! IRIs as the Turtle reader reads them.
//!
//! An IRI is parsed into its parts — scheme, user information, host, port,
//! path, query and fragment — by the grammar of RFC 3987, applied strictly by
//! [`Iri::parse`]: every IRI a document names must parse. A relative reference
//! is read leniently by [`Iri::lenient`]: each character the grammar refuses is
//! percent-encoded, the first refused one first, until the reference parses.
//! [`Iri::resolve`] resolves a reference against a base by RFC 3986, removing
//! dot segments its own way.

use std::fmt;

/// An IRI, parsed.
#[derive(Clone, Debug)]
pub(crate) struct Iri {
    text: String,
    scheme: Option<String>,
    user_info: Option<String>,
    host: Option<String>,
    port: Option<u32>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

/// Why a text is not an IRI, and the index of the character it stopped at.
#[derive(Debug)]
pub(crate) struct IriError {
    pub reason: String,
    pub index: usize,
    pub input: String,
}

impl fmt::Display for IriError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at index {}: {}", self.reason, self.index, self.input)
    }
}

impl std::error::Error for IriError {}

/// The end of the text, and a NUL within it.
const EOF: char = '\0';

impl Iri {
    /// Parse `text`, refusing it if any character falls outside the grammar.
    pub(crate) fn parse(text: &str) -> Result<Iri, IriError> {
        let chars: Vec<char> = text.chars().collect();
        let mut p = Parse { iri: &chars, pos: 0 };
        let mut scheme = p.scheme();
        if scheme.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("jar")) {
            let inner = p.scheme().unwrap_or_else(|| "null".into());
            scheme = Some(format!("{}:{inner}", scheme.unwrap_or_default()));
        }
        let (mut user_info, mut host, mut port, mut path) = (None, None, None, None);
        let peek = p.peek();
        if peek == '/' && p.peek_at(1) == '/' {
            p.advance(2);
            if chars.contains(&'@') {
                user_info = p.user_info()?;
            }
            host = Some(p.host(scheme.as_deref())?);
            if p.peek() == ':' {
                p.advance(1);
                let digits = p.member(digit, '/');
                if !digits.is_empty() {
                    let n = digits.parse::<i32>().map_err(|_| p.error("Invalid port"))?;
                    port = Some(n as u32);
                }
            }
            if !matches!(p.peek(), '/' | '?' | '#' | EOF) {
                return Err(p.error("absolute or empty path expected"));
            }
            path = Some(p.pct_encoded(fchar, '?', '#')?);
        } else if matches!(peek, '/' | '?' | '#' | EOF)
            || peek == '%'
            || (peek != ':' && pchar(peek))
            || (scheme.is_some() && peek == ':')
        {
            path = Some(p.pct_encoded(fchar, '?', '#')?);
        }
        let mut query = None;
        if p.peek() == '?' {
            p.advance(1);
            query = Some(p.pct_encoded(qchar, '#', EOF)?);
        }
        let mut fragment = None;
        if p.peek() == '#' {
            p.advance(1);
            fragment = Some(p.pct_encoded(fchar, '#', EOF)?);
        }
        if p.pos != chars.len() {
            return Err(p.error("Unexpected character"));
        }
        Ok(Iri { text: text.to_string(), scheme, user_info, host, port, path: path.unwrap_or_default(), query, fragment })
    }

    /// Parse `text`, percent-encoding each character the grammar refuses: the
    /// first refused character is encoded and the text parsed again, until it
    /// parses or the parse stops no later than it did before.
    pub(crate) fn lenient(text: &str) -> Result<Iri, IriError> {
        let first = match Iri::parse(text) {
            Ok(iri) => return Ok(iri),
            Err(e) => e,
        };
        let mut chars: Vec<char> = text.chars().collect();
        let mut problem = first.index;
        loop {
            let Some(&c) = chars.get(problem) else { return Err(first) };
            let encoded = if c == ' ' { "%20".to_string() } else { form_encode(c) };
            chars.splice(problem..problem + 1, encoded.chars());
            let again: String = chars.iter().collect();
            match Iri::parse(&again) {
                Ok(iri) => return Ok(iri),
                Err(e) if e.index <= problem => return Err(first),
                Err(e) => problem = e.index,
            }
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether the IRI has a scheme.
    pub(crate) fn is_absolute(&self) -> bool {
        self.scheme.is_some()
    }

    /// Whether the IRI has a scheme and a path that does not start at the
    /// root: `urn:x`, `mailto:a@b`.
    pub(crate) fn is_opaque(&self) -> bool {
        self.scheme.is_some() && !self.path.is_empty() && !self.path.starts_with('/')
    }

    /// `reference`, read leniently, resolved against this IRI.
    pub(crate) fn resolve_text(&self, reference: &str) -> Result<String, IriError> {
        Ok(self.resolve(&Iri::lenient(reference)?).text)
    }

    /// `relative` resolved against this IRI.
    pub(crate) fn resolve(&self, relative: &Iri) -> Iri {
        if relative.is_absolute() {
            return relative.clone();
        }
        let base = |path: String, query: Option<String>, fragment: Option<String>| {
            Iri::build(self.scheme.clone(), self.user_info.clone(), self.host.clone(), self.port, path, query, fragment)
        };
        if relative.host.is_none() && relative.query.is_none() && relative.path.is_empty() {
            return base(self.path.clone(), self.query.clone(), relative.fragment.clone());
        }
        if relative.host.is_none() && relative.path.is_empty() {
            return base(self.path.clone(), relative.query.clone(), relative.fragment.clone());
        }
        let (user_info, host, port, mut path, merged) = if relative.host.is_some() {
            (relative.user_info.clone(), relative.host.clone(), relative.port, relative.path.clone(), false)
        } else if relative.path.starts_with('/') {
            (self.user_info.clone(), self.host.clone(), self.port, relative.path.clone(), false)
        } else {
            let mut dir = self.path.clone();
            if !dir.ends_with('/') {
                dir.truncate(dir.rfind('/').map_or(0, |i| i + 1));
            }
            if dir.is_empty() {
                dir.push('/');
            }
            (self.user_info.clone(), self.host.clone(), self.port, dir + &relative.path, true)
        };
        if merged || path.contains("/./") || path.contains("/../") {
            path = remove_dot_segments(&path);
        }
        Iri::build(self.scheme.clone(), user_info, host, port, path, relative.query.clone(), relative.fragment.clone())
    }

    fn build(
        scheme: Option<String>,
        user_info: Option<String>,
        host: Option<String>,
        port: Option<u32>,
        path: String,
        query: Option<String>,
        fragment: Option<String>,
    ) -> Iri {
        let mut text = String::new();
        if let Some(s) = &scheme {
            text.push_str(s);
            text.push(':');
        }
        if let Some(h) = &host {
            text.push_str("//");
            if let Some(u) = &user_info {
                text.push_str(u);
                text.push('@');
            }
            text.push_str(h);
            if let Some(p) = port {
                text.push_str(&format!(":{p}"));
            }
        }
        text.push_str(&path);
        if let Some(q) = &query {
            text.push('?');
            text.push_str(q);
        }
        if let Some(f) = &fragment {
            text.push('#');
            text.push_str(f);
        }
        Iri { text, scheme, user_info, host, port, path, query, fragment }
    }
}

/// A character as an HTML form encodes it: letters, digits and `.-*_` as they
/// are, every other character as the percent-encoded bytes of its UTF-8.
fn form_encode(c: char) -> String {
    if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | '_') {
        return c.to_string();
    }
    let mut buf = [0u8; 4];
    c.encode_utf8(&mut buf).bytes().map(|b| format!("%{b:02X}")).collect()
}

/// `path` with its `.` and `..` segments taken out.
fn remove_dot_segments(path: &str) -> String {
    let mut path = path.replace("/./", "/");
    if let Some(rest) = path.strip_prefix("./") {
        path = rest.to_string();
    }
    if path.ends_with("/.") {
        path.pop();
    }
    if !path.contains("/../") && !path.ends_with("/..") {
        return path;
    }
    // The segments between slashes, less the empty ones at the end.
    let mut segments: Vec<&str> = path.split('/').collect();
    while segments.last() == Some(&"") {
        segments.pop();
    }
    if path.starts_with('/') && !segments.is_empty() {
        segments.remove(0);
    }
    let mut last_removed = false;
    let mut i = 1;
    while i < segments.len() {
        if segments[i] != ".." {
            i += 1;
            continue;
        }
        if segments[i - 1] == ".." {
            i += 2;
            continue;
        }
        if i == segments.len() - 1 {
            last_removed = true;
        }
        segments.remove(i);
        segments.remove(i - 1);
        if i > 1 {
            i -= 1;
        }
    }
    while segments.first().is_some_and(|s| *s == ".." || *s == ".") {
        segments.remove(0);
    }
    let mut out = String::with_capacity(path.len());
    if path.starts_with('/') {
        out.push('/');
    }
    if let Some((last, rest)) = segments.split_last() {
        for s in rest {
            out.push_str(s);
            out.push('/');
        }
        out.push_str(last);
        if path.ends_with('/') || last_removed {
            out.push('/');
        }
    }
    out
}

struct Parse<'a> {
    iri: &'a [char],
    pos: usize,
}

impl Parse<'_> {
    fn peek(&self) -> char {
        self.iri.get(self.pos).copied().unwrap_or(EOF)
    }

    fn peek_at(&self, ahead: isize) -> char {
        let at = self.pos as isize + ahead;
        if at < 0 {
            return EOF;
        }
        self.iri.get(at as usize).copied().unwrap_or(EOF)
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    fn error(&self, reason: &str) -> IriError {
        let reason = match self.iri.get(self.pos) {
            Some(&c) => format!("{reason} U+{:X}", c as u32),
            None => reason.to_string(),
        };
        IriError { reason, index: self.pos, input: self.iri.iter().collect() }
    }

    fn scheme(&mut self) -> Option<String> {
        if !alpha(self.peek()) {
            return None;
        }
        let start = self.pos;
        let scheme = self.member(schar, ':');
        if self.peek() == ':' {
            self.advance(1);
            return Some(scheme);
        }
        self.pos = start;
        None
    }

    fn user_info(&mut self) -> Result<Option<String>, IriError> {
        let start = self.pos;
        let info = self.pct_encoded(uchar, '@', '/')?;
        if self.peek() == '@' {
            self.advance(1);
            return Ok(Some(info));
        }
        self.pos = start;
        Ok(None)
    }

    fn host(&mut self, scheme: Option<&str>) -> Result<String, IriError> {
        let start = self.pos;
        if self.peek() == '[' {
            self.advance(1);
            self.member(uchar, ']');
            if self.peek() == ']' {
                self.advance(1);
                return Ok(self.text(start));
            }
            return Err(self.error("Invalid host IP address"));
        }
        if !digit(self.peek()) {
            return self.pct_encoded(hchar, ':', '/');
        }
        // Four dotted octets, or failing that a name.
        let mut failed = None;
        for i in 0..4 {
            let octet = self.member(digit, '.');
            if !octet.parse::<i32>().is_ok_and(|o| (0..=255).contains(&o)) {
                failed = Some(self.error("Invalid IPv4 address"));
                break;
            }
            if self.peek() == '.' {
                self.advance(1);
                continue;
            }
            if i == 3 && matches!(self.peek(), EOF | ':' | '/') {
                continue;
            }
            failed = Some(self.error("Invalid IPv4 address"));
            break;
        }
        let Some(failed) = failed else { return Ok(self.text(start)) };
        self.pos = start;
        let host = self.pct_encoded(hchar, ':', '/')?;
        let web = scheme.is_some_and(|s| is_scheme(s, "http") || is_scheme(s, "https"));
        if web && !self.top_level_domain_valid(start) {
            return Err(failed);
        }
        Ok(host)
    }

    /// Whether the host's last label, back from here to the last `.`, is
    /// letters alone.
    fn top_level_domain_valid(&self, host_start: usize) -> bool {
        let mut step: isize = 0;
        let mut illegal = false;
        while self.pos as isize + step > host_start as isize {
            step -= 1;
            let c = self.peek_at(step);
            if c == '.' {
                return !illegal;
            }
            if !alpha(c) {
                illegal = true;
            }
        }
        true
    }

    fn pct_encoded(&mut self, set: fn(char) -> bool, end1: char, end2: char) -> Result<String, IriError> {
        let start = self.pos;
        loop {
            let c = self.peek();
            if c == EOF || c == end1 || c == end2 {
                break;
            }
            if c.is_ascii_alphanumeric() {
                self.advance(1);
                continue;
            }
            if c == '%' {
                if hexdig(self.peek_at(1)) && hexdig(self.peek_at(2)) {
                    self.advance(3);
                    continue;
                }
                return Err(self.error("Illegal percent encoding"));
            }
            if !set(c) {
                break;
            }
            self.advance(1);
        }
        Ok(self.text(start))
    }

    fn member(&mut self, set: fn(char) -> bool, end: char) -> String {
        let start = self.pos;
        loop {
            let c = self.peek();
            if c == EOF || c == end || !set(c) {
                break;
            }
            self.advance(1);
        }
        self.text(start)
    }

    fn text(&self, start: usize) -> String {
        self.iri[start..self.pos.min(self.iri.len())].iter().collect()
    }
}

/// Whether `scheme` is `name`, or `jar:` over `name`, in any case.
fn is_scheme(scheme: &str, name: &str) -> bool {
    scheme.eq_ignore_ascii_case(name) || (scheme.find(':') == Some(3) && scheme.eq_ignore_ascii_case(&format!("jar:{name}")))
}

fn alpha(c: char) -> bool {
    c.is_ascii_alphabetic()
}

fn digit(c: char) -> bool {
    c.is_ascii_digit()
}

fn hexdig(c: char) -> bool {
    c.is_ascii_hexdigit()
}

fn ucschar(c: char) -> bool {
    let c = c as u32;
    match c {
        0xA0..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFEF => true,
        0x10000..=0xDFFFD => c & 0xFFFF <= 0xFFFD,
        0xE1000..=0xEFFFD => true,
        _ => false,
    }
}

fn iprivate(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

fn sub_delim(c: char) -> bool {
    matches!(c, '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=')
}

fn unreserved(c: char) -> bool {
    alpha(c) || digit(c) || matches!(c, '-' | '.' | '_' | '~') || ucschar(c)
}

fn schar(c: char) -> bool {
    alpha(c) || digit(c) || matches!(c, '+' | '-' | '.')
}

fn uchar(c: char) -> bool {
    unreserved(c) || sub_delim(c) || c == ':'
}

fn hchar(c: char) -> bool {
    unreserved(c) || sub_delim(c)
}

fn pchar(c: char) -> bool {
    unreserved(c) || sub_delim(c) || matches!(c, ':' | '@')
}

fn qchar(c: char) -> bool {
    pchar(c) || iprivate(c) || matches!(c, '/' | '?')
}

fn fchar(c: char) -> bool {
    pchar(c) || matches!(c, '/' | '?')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(base: &str, reference: &str) -> String {
        Iri::lenient(base).unwrap().resolve_text(reference).unwrap()
    }

    /// The examples of RFC 3986 section 5.4, against its base.
    #[test]
    fn references_resolve_as_rfc_3986_resolves_them() {
        let base = "http://a/b/c/d;p?q";
        for (reference, expected) in [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            (";x", "http://a/b/c/;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
        ] {
            assert_eq!(resolve(base, reference), expected, "{reference}");
        }
    }

    /// A character the grammar refuses in a relative reference is
    /// percent-encoded, each one in turn; an absolute IRI holding one does not
    /// parse.
    #[test]
    fn a_refused_character_is_encoded_in_a_reference_and_refused_in_an_iri() {
        assert_eq!(resolve("file:/f/x.ttl", "{fixtures}/y.ttl#"), "file:/f/%7Bfixtures%7D/y.ttl#");
        assert_eq!(resolve("file:/f/x.ttl", "a b|c"), "file:/f/a%20b%7Cc");
        assert_eq!(resolve("file:/f/x.ttl", "é"), "file:/f/é");
        let refused = Iri::parse("http://x.org/{a}#").unwrap_err();
        assert_eq!(refused.to_string(), "Unexpected character U+7B at index 13: http://x.org/{a}#");
    }

    /// An opaque IRI has a scheme and a path that does not start at the root.
    #[test]
    fn an_iri_is_opaque_when_its_path_does_not_start_at_the_root() {
        assert!(Iri::parse("urn:x:y").unwrap().is_opaque());
        assert!(!Iri::parse("file:/x").unwrap().is_opaque());
        assert!(!Iri::parse("#x").unwrap().is_opaque());
    }
}
