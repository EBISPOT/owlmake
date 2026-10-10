//! A ROBOT command line read as ROBOT 1.9.11 reads it. Each command takes the
//! common options and its own ([`COMMANDS`]), and its tokens are read as
//! Apache Commons CLI 1.4's `DefaultParser` reads them when it stops at the
//! first token that is no option: that token names the next command of the
//! chain.

use std::collections::HashMap;

/// How many values an option takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arity {
    Flag,
    Values(usize),
    /// As many as follow, and at least one.
    Unlimited,
}

/// One option: its short and long names, and how many values it takes.
#[derive(Debug)]
pub(crate) struct OptionDef {
    pub short: Option<&'static str>,
    pub long: Option<&'static str>,
    pub arity: Arity,
}

impl OptionDef {
    /// The spelling an option is recorded under: `--long`, or `-short` for an
    /// option with no long name.
    pub(crate) fn name(&self) -> String {
        match (self.long, self.short) {
            (Some(l), _) => format!("--{l}"),
            (None, Some(s)) => format!("-{s}"),
            (None, None) => String::new(),
        }
    }

    /// The name an error names the option by: its short name, else its long.
    fn key(&self) -> &'static str {
        self.short.or(self.long).unwrap_or_default()
    }
}

/// A command's options as Commons CLI holds them: each under its short name
/// (its long one where it has none) and under its long name, a later option
/// taking a name over from an earlier one.
pub(crate) struct Options {
    defs: Vec<&'static OptionDef>,
    by_key: HashMap<&'static str, usize>,
    /// Long names in the order first added, each with the option it names now.
    by_long: Vec<(&'static str, usize)>,
}

impl Options {
    fn new(defs: impl Iterator<Item = &'static OptionDef>) -> Options {
        let mut options = Options { defs: Vec::new(), by_key: HashMap::new(), by_long: Vec::new() };
        for def in defs {
            let index = options.defs.len();
            options.defs.push(def);
            if let Some(long) = def.long {
                match options.by_long.iter_mut().find(|(l, _)| *l == long) {
                    Some(entry) => entry.1 = index,
                    None => options.by_long.push((long, index)),
                }
            }
            options.by_key.insert(def.key(), index);
        }
        options
    }

    fn long(&self, name: &str) -> Option<usize> {
        self.by_long.iter().find(|(l, _)| *l == name).map(|(_, i)| *i)
    }

    fn has_short(&self, name: &str) -> bool {
        self.by_key.contains_key(name)
    }

    /// The option `name` names, looked up as a short name before a long one.
    fn get(&self, name: &str) -> Option<usize> {
        let name = strip_hyphens(name);
        self.by_key.get(name).copied().or_else(|| self.long(name))
    }

    /// The long names `token` matches: itself, where it is one, else every
    /// long name it begins.
    fn matching(&self, token: &str) -> Vec<&'static str> {
        let name = strip_hyphens(token);
        if self.long(name).is_some() {
            return self.by_long.iter().filter(|(l, _)| *l == name).map(|(l, _)| *l).collect();
        }
        self.by_long.iter().filter(|(l, _)| l.starts_with(name)).map(|(l, _)| *l).collect()
    }

    /// The longest long name, of at least two characters and short of the
    /// whole, that `token` begins with.
    fn long_prefix(&self, token: &str) -> Option<&'static str> {
        let t = strip_hyphens(token);
        let chars: Vec<(usize, char)> = t.char_indices().collect();
        (2..chars.len().saturating_sub(1)).rev().find_map(|n| {
            let prefix = &t[..chars.get(n).map_or(t.len(), |(b, _)| *b)];
            self.by_long.iter().find(|(l, _)| *l == prefix).map(|(l, _)| *l)
        })
    }

    fn is_short_option(&self, token: &str) -> bool {
        let mut chars = token.chars();
        chars.next() == Some('-') && chars.next().is_some_and(|c| self.has_short(&c.to_string()))
    }

    fn is_long_option(&self, token: &str) -> bool {
        if !token.starts_with('-') || token.chars().count() == 1 {
            return false;
        }
        let name = token.split_once('=').map_or(token, |(n, _)| n);
        !self.matching(name).is_empty() || (self.long_prefix(token).is_some() && !token.starts_with("--"))
    }

    fn is_argument(&self, token: &str) -> bool {
        !(self.is_long_option(token) || self.is_short_option(token)) || is_number(token)
    }

    fn takes_values(&self, index: usize) -> bool {
        self.defs[index].arity != Arity::Flag
    }

    /// Whether an option given `values` so far takes another, and whether it
    /// must.
    fn accepts(&self, index: usize, values: &[String]) -> bool {
        match self.defs[index].arity {
            Arity::Flag => false,
            Arity::Values(n) => values.len() < n,
            Arity::Unlimited => true,
        }
    }

    fn requires(&self, index: usize, values: &[String]) -> bool {
        match self.defs[index].arity {
            Arity::Flag => false,
            Arity::Values(n) => values.len() < n,
            Arity::Unlimited => values.is_empty(),
        }
    }
}

/// `-x` or `--x` without its hyphens.
fn strip_hyphens(token: &str) -> &str {
    token.strip_prefix("--").or_else(|| token.strip_prefix('-')).unwrap_or(token)
}

/// Whether `token` reads as a Java `double`.
fn is_number(token: &str) -> bool {
    let t = token.trim();
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    if matches!(t, "Infinity" | "NaN") {
        return true;
    }
    let t = t.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(t);
    let (mantissa, exponent) = match t.find(['e', 'E']) {
        Some(at) => (&t[..at], Some(&t[at + 1..])),
        None => (t, None),
    };
    let digits = mantissa.chars().filter(char::is_ascii_digit).count();
    let mantissa_ok = digits > 0
        && mantissa.chars().all(|c| c.is_ascii_digit() || c == '.')
        && mantissa.matches('.').count() <= 1;
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
    });
    mantissa_ok && exponent_ok
}

/// A value as Commons CLI takes it: one pair of enclosing double quotes, with
/// none between them, removed.
fn unquote(value: &str) -> String {
    match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(inner) if value.len() > 1 && !inner.contains('"') => inner.to_string(),
        _ => value.to_string(),
    }
}

/// The options of ROBOT command `name` (in any case), with the common ones
/// before its own and owlmake's ([`EXTRA`]) after them; none for a command
/// ROBOT does not have.
pub(crate) fn command(name: &str) -> Option<Options> {
    let name = name.trim().to_lowercase();
    let extra = EXTRA.iter().filter(|(n, _)| *n == name).flat_map(|(_, defs)| defs.iter());
    COMMANDS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, own)| Options::new(COMMON.iter().chain(own.iter()).chain(extra)))
}

/// What a command's tokens give: each option it names, by [`OptionDef::name`],
/// with its values, and how many tokens they took. A token that is no option
/// ends them; it names the next command, or, when only part of a token was
/// read, `next` is the part that does.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Parsed {
    pub options: Vec<(String, Vec<String>)>,
    pub consumed: usize,
    pub next: Option<String>,
}

/// One token read as options.
enum Read {
    /// The options it names, each with the values given in the token, and
    /// whether the last may still take the tokens after it as values.
    Options(Vec<(usize, Vec<String>)>, bool),
    /// No option of the command: the tokens end here, after the options the
    /// token named first, the next command being the text given.
    End(Vec<(usize, Vec<String>)>, String),
}

impl Read {
    fn one(index: usize, values: Vec<String>) -> Read {
        let open = values.is_empty();
        Read::Options(vec![(index, values)], open)
    }

    fn end(token: &str) -> Read {
        Read::End(Vec::new(), token.to_string())
    }
}

/// Read `tokens` as a command with `options` reads them, or ROBOT's message
/// when they cannot be.
pub(crate) fn parse(options: &Options, tokens: &[String]) -> Result<Parsed, String> {
    let mut read: Vec<(usize, Vec<String>)> = Vec::new();
    // The option the next token may be a value of, as an index into `read`.
    let mut current: Option<usize> = None;
    let mut parsed = Parsed::default();
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if token == "--" {
            i += 1;
            break;
        }
        if let Some(c) = current {
            if options.accepts(read[c].0, &read[c].1) && options.is_argument(token) {
                read[c].1.push(unquote(token));
                i += 1;
                if !options.accepts(read[c].0, &read[c].1) {
                    current = None;
                }
                continue;
            }
        }
        let outcome = if token.starts_with("--") {
            long_option(options, token)?
        } else if token.starts_with('-') && token != "-" {
            short_and_long_option(options, token)?
        } else {
            Read::end(token)
        };
        let (named, open, next) = match outcome {
            Read::Options(named, open) => (named, open, None),
            Read::End(named, next) => (named, false, Some(next)),
        };
        for (index, values) in named {
            if let Some(c) = current {
                if options.requires(read[c].0, &read[c].1) {
                    return Err(missing(options, read[c].0));
                }
            }
            read.push((index, values));
            current = options.takes_values(index).then_some(read.len() - 1);
        }
        if !open {
            current = current.filter(|c| options.accepts(read[*c].0, &read[*c].1) && read[*c].1.is_empty());
        }
        if let Some(next) = next {
            if next != *token {
                parsed.next = Some(next);
                i += 1;
            }
            break;
        }
        i += 1;
        if let Some(c) = current {
            if !options.accepts(read[c].0, &read[c].1) {
                current = None;
            }
        }
    }
    if let Some(c) = current {
        if options.requires(read[c].0, &read[c].1) {
            return Err(missing(options, read[c].0));
        }
    }
    parsed.consumed = i;
    parsed.options = read.into_iter().map(|(index, values)| (options.defs[index].name(), values)).collect();
    Ok(parsed)
}

fn missing(options: &Options, index: usize) -> String {
    format!("Missing argument for option: {}", options.defs[index].key())
}

/// `--name` or `--name=value`.
fn long_option(options: &Options, token: &str) -> Result<Read, String> {
    let (name, value) = match token.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (token, None),
    };
    let found = options.matching(name);
    match found.as_slice() {
        [] => Ok(Read::end(token)),
        [one] => {
            let index = options.get(one).expect("a matched name names an option");
            match value {
                None => Ok(Read::one(index, Vec::new())),
                Some(v) if options.takes_values(index) => Ok(Read::Options(vec![(index, vec![v.to_string()])], false)),
                Some(_) => Ok(Read::end(token)),
            }
        }
        many => Err(format!("Ambiguous option: '{name}'  (could be: '{}')", many.join("', '"))),
    }
}

/// `-x`, `-name`, `-xVALUE`, `-xyz` or `-x=value`.
fn short_and_long_option(options: &Options, token: &str) -> Result<Read, String> {
    let t = strip_hyphens(token);
    match t.split_once('=') {
        None if t.chars().count() == 1 => Ok(match options.get(t).filter(|_| options.has_short(t)) {
            Some(index) => Read::one(index, Vec::new()),
            None => Read::end(token),
        }),
        None => {
            if options.has_short(t) {
                return Ok(Read::one(options.get(t).expect("a short name names an option"), Vec::new()));
            }
            if !options.matching(t).is_empty() {
                return long_option(options, token);
            }
            if let Some(prefix) = options.long_prefix(t) {
                let index = options.get(prefix).expect("a long name names an option");
                if options.takes_values(index) {
                    return Ok(Read::Options(vec![(index, vec![t[prefix.len()..].to_string()])], false));
                }
            }
            let first: String = t.chars().next().map(String::from).unwrap_or_default();
            if let Some(index) = options.get(&first).filter(|i| java_property(options, *i)) {
                return Ok(Read::Options(vec![(index, vec![t[first.len()..].to_string()])], false));
            }
            Ok(concatenated(options, token))
        }
        Some((name, value)) => {
            if name.chars().count() == 1 {
                return Ok(match options.get(name).filter(|i| options.takes_values(*i)) {
                    Some(index) => Read::Options(vec![(index, vec![value.to_string()])], false),
                    None => Read::end(token),
                });
            }
            let first: String = name.chars().next().map(String::from).unwrap_or_default();
            if let Some(index) = options.get(&first).filter(|i| java_property(options, *i)) {
                let values = vec![name[first.len()..].to_string(), value.to_string()];
                return Ok(Read::Options(vec![(index, values)], false));
            }
            long_option(options, token)
        }
    }
}

/// Whether an option reads `-xKEY=VALUE`: it takes two values or more.
fn java_property(options: &Options, index: usize) -> bool {
    matches!(options.defs[index].arity, Arity::Values(n) if n >= 2) || options.defs[index].arity == Arity::Unlimited
}

/// `-xyz`: options `x`, `y` and `z` in turn, until one takes the rest of the
/// token as its value. A character that is no option ends the command's
/// tokens, the rest of the token naming the next command.
fn concatenated(options: &Options, token: &str) -> Read {
    let mut named: Vec<(usize, Vec<String>)> = Vec::new();
    for (n, (at, c)) in token.char_indices().enumerate().skip(1) {
        let name = c.to_string();
        if !(options.has_short(&name) || options.long(&name).is_some()) {
            let next = if n > 1 { token[at..].to_string() } else { token.to_string() };
            return Read::End(named, next);
        }
        let index = options.get(&name).expect("an option's name names it");
        let rest = &token[at + c.len_utf8()..];
        if options.takes_values(index) && !rest.is_empty() {
            named.push((index, vec![rest.to_string()]));
            return Read::Options(named, true);
        }
        named.push((index, Vec::new()));
    }
    Read::Options(named, true)
}

/// The options every command takes (`CommandLineHelper.getCommonOptions`).
const COMMON: &[OptionDef] = &[
    OptionDef { short: Some("h"), long: Some("help"), arity: Arity::Flag },
    OptionDef { short: Some("V"), long: Some("version"), arity: Arity::Flag },
    OptionDef { short: Some("v"), long: Some("verbose"), arity: Arity::Flag },
    OptionDef { short: Some("vv"), long: Some("very-verbose"), arity: Arity::Flag },
    OptionDef { short: Some("vvv"), long: Some("very-very-verbose"), arity: Arity::Flag },
    OptionDef { short: None, long: Some("input-format"), arity: Arity::Values(1) },
    OptionDef { short: None, long: Some("catalog"), arity: Arity::Values(1) },
    OptionDef { short: Some("p"), long: Some("prefix"), arity: Arity::Values(1) },
    OptionDef { short: Some("P"), long: Some("prefixes"), arity: Arity::Values(1) },
    OptionDef { short: None, long: Some("noprefixes"), arity: Arity::Flag },
    OptionDef { short: None, long: Some("add-prefix"), arity: Arity::Values(1) },
    OptionDef { short: None, long: Some("add-prefixes"), arity: Arity::Values(1) },
    OptionDef { short: Some("x"), long: Some("xml-entities"), arity: Arity::Flag },
    OptionDef { short: None, long: Some("strict"), arity: Arity::Flag },
];

/// The options owlmake's commands take besides ROBOT's.
const EXTRA: &[(&str, &[OptionDef])] = &[(
    "reason",
    // The object properties the `PropertyAssertion` generator asserts.
    &[OptionDef { short: None, long: Some("properties"), arity: Arity::Values(1) }],
)];

/// Each command's own options, in the order it adds them after the common ones.
const COMMANDS: &[(&str, &[OptionDef])] = &[
    (
        "annotate",
        &[
            OptionDef { short: Some("R"), long: Some("remove-annotations"), arity: Arity::Flag },
            OptionDef { short: Some("A"), long: Some("annotation-file"), arity: Arity::Values(1) },
            OptionDef { short: Some("O"), long: Some("ontology-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("V"), long: Some("version-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("annotate-derived-from"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("annotate-defined-by"), arity: Arity::Values(1) },
            OptionDef { short: Some("e"), long: Some("interpolate"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("annotation"), arity: Arity::Values(2) },
            OptionDef { short: Some("k"), long: Some("link-annotation"), arity: Arity::Values(2) },
            OptionDef { short: Some("l"), long: Some("language-annotation"), arity: Arity::Values(3) },
            OptionDef { short: Some("t"), long: Some("typed-annotation"), arity: Arity::Values(3) },
            OptionDef { short: Some("x"), long: Some("axiom-annotation"), arity: Arity::Values(3) },
        ],
    ),
    (
        "collapse",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("threshold"), arity: Arity::Values(1) },
            OptionDef { short: Some("r"), long: Some("precious"), arity: Arity::Values(1) },
            OptionDef { short: Some("R"), long: Some("precious-terms"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "convert",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("format"), arity: Arity::Values(1) },
            OptionDef { short: Some("c"), long: Some("check"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("clean-obo"), arity: Arity::Values(1) },
        ],
    ),
    (
        "diff",
        &[
            OptionDef { short: Some("l"), long: Some("left"), arity: Arity::Values(1) },
            OptionDef { short: Some("L"), long: Some("left-iri"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("left-catalog"), arity: Arity::Values(1) },
            OptionDef { short: Some("r"), long: Some("right"), arity: Arity::Values(1) },
            OptionDef { short: Some("R"), long: Some("right-iri"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("right-catalog"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("labels"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("format"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("label-langs-priority"), arity: Arity::Values(1) },
        ],
    ),
    (
        "expand",
        &[
            OptionDef { short: Some("c"), long: Some("create-new-ontology"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("annotate-expansion-axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("expand-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("T"), long: Some("expand-term-file"), arity: Arity::Values(1) },
            OptionDef { short: Some("n"), long: Some("no-expand-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("N"), long: Some("no-expand-term-file"), arity: Arity::Values(1) },
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "explain",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("r"), long: Some("reasoner"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("axiom"), arity: Arity::Values(1) },
            OptionDef { short: Some("m"), long: Some("max"), arity: Arity::Values(1) },
            OptionDef { short: Some("u"), long: Some("unsatisfiable"), arity: Arity::Values(1) },
            OptionDef { short: Some("M"), long: Some("mode"), arity: Arity::Values(1) },
            OptionDef { short: Some("e"), long: Some("explanation"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("format"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "export",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("e"), long: Some("export"), arity: Arity::Values(1) },
            OptionDef { short: Some("c"), long: Some("header"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("sort"), arity: Arity::Values(1) },
            OptionDef { short: Some("n"), long: Some("include"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("format"), arity: Arity::Values(1) },
            OptionDef { short: Some("S"), long: Some("split"), arity: Arity::Values(1) },
            OptionDef { short: Some("E"), long: Some("entity-format"), arity: Arity::Values(1) },
            OptionDef { short: Some("l"), long: Some("entity-select"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("standalone"), arity: Arity::Values(1) },
        ],
    ),
    (
        "export-prefixes",
        &[
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "extract",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("O"), long: Some("output-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("m"), long: Some("method"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("term"), arity: Arity::Values(1) },
            OptionDef { short: Some("T"), long: Some("term-file"), arity: Arity::Values(1) },
            OptionDef { short: Some("u"), long: Some("upper-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("U"), long: Some("upper-terms"), arity: Arity::Values(1) },
            OptionDef { short: Some("l"), long: Some("lower-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("L"), long: Some("lower-terms"), arity: Arity::Values(1) },
            OptionDef { short: Some("b"), long: Some("branch-from-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("B"), long: Some("branch-from-terms"), arity: Arity::Values(1) },
            OptionDef { short: Some("c"), long: Some("copy-ontology-annotations"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("force"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("annotate-with-source"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("sources"), arity: Arity::Values(1) },
            OptionDef { short: Some("n"), long: Some("individuals"), arity: Arity::Values(1) },
            OptionDef { short: Some("M"), long: Some("imports"), arity: Arity::Values(1) },
            OptionDef { short: Some("N"), long: Some("intermediates"), arity: Arity::Values(1) },
        ],
    ),
    (
        "filter",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("O"), long: Some("ontology-iri"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("base-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("term"), arity: Arity::Values(1) },
            OptionDef { short: Some("T"), long: Some("term-file"), arity: Arity::Values(1) },
            OptionDef { short: Some("e"), long: Some("exclude-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("E"), long: Some("exclude-terms"), arity: Arity::Values(1) },
            OptionDef { short: Some("n"), long: Some("include-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("N"), long: Some("include-terms"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("select"), arity: Arity::Values(1) },
            OptionDef { short: Some("p"), long: Some("preserve-structure"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("r"), long: Some("trim"), arity: Arity::Values(1) },
            OptionDef { short: Some("S"), long: Some("signature"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("drop-axiom-annotations"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("allow-punning"), arity: Arity::Values(1) },
        ],
    ),
    (
        "materialize",
        &[
            OptionDef { short: Some("r"), long: Some("reasoner"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("remove-redundant-subclass-axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("n"), long: Some("create-new-ontology"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("annotate-inferred-axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("term"), arity: Arity::Values(1) },
            OptionDef { short: Some("T"), long: Some("term-file"), arity: Arity::Values(1) },
        ],
    ),
    (
        "measure",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("r"), long: Some("reasoner"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("format"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("m"), long: Some("metrics"), arity: Arity::Values(1) },
        ],
    ),
    (
        "merge",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("p"), long: Some("inputs"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("c"), long: Some("collapse-import-closure"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("include-annotations"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("annotate-derived-from"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("annotate-defined-by"), arity: Arity::Values(1) },
        ],
    ),
    (
        "mirror",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("directory"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "python",
        &[
            OptionDef { short: None, long: Some("port"), arity: Arity::Values(1) },
        ],
    ),
    (
        "query",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("format"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("O"), long: Some("output-dir"), arity: Arity::Values(1) },
            OptionDef { short: Some("g"), long: Some("use-graphs"), arity: Arity::Values(1) },
            OptionDef { short: Some("u"), long: Some("update"), arity: Arity::Values(1) },
            OptionDef { short: Some("y"), long: Some("temporary-file"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("tdb"), arity: Arity::Values(1) },
            OptionDef { short: Some("C"), long: Some("create-tdb"), arity: Arity::Values(1) },
            OptionDef { short: Some("k"), long: Some("keep-tdb-mappings"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("tdb-directory"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("select"), arity: Arity::Values(2) },
            OptionDef { short: Some("c"), long: Some("construct"), arity: Arity::Values(2) },
            OptionDef { short: Some("q"), long: Some("query"), arity: Arity::Values(2) },
            OptionDef { short: Some("Q"), long: Some("queries"), arity: Arity::Unlimited },
        ],
    ),
    (
        "reason",
        &[
            OptionDef { short: Some("r"), long: Some("reasoner"), arity: Arity::Values(1) },
            OptionDef { short: Some("D"), long: Some("dump-unsatisfiable"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("remove-redundant-subclass-axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("p"), long: Some("preserve-annotated-axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("n"), long: Some("create-new-ontology"), arity: Arity::Values(1) },
            OptionDef { short: Some("m"), long: Some("create-new-ontology-with-annotations"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("annotate-inferred-axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("x"), long: Some("exclude-duplicate-axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("X"), long: Some("exclude-external-entities"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("exclude-tautologies"), arity: Arity::Values(1) },
            OptionDef { short: Some("T"), long: Some("exclude-owl-thing"), arity: Arity::Values(1) },
            OptionDef { short: Some("e"), long: Some("equivalent-classes-allowed"), arity: Arity::Values(1) },
            OptionDef { short: Some("A"), long: Some("axiom-generators"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("include-indirect"), arity: Arity::Values(1) },
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "reduce",
        &[
            OptionDef { short: Some("r"), long: Some("reasoner"), arity: Arity::Values(1) },
            OptionDef { short: Some("p"), long: Some("preserve-annotated-axioms"), arity: Arity::Values(1) },
            OptionDef { short: Some("c"), long: Some("named-classes-only"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("include-subproperties"), arity: Arity::Values(1) },
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "relax",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("enforce-obo-format"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("exclude-named-classes"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("include-subclass-of"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "remove",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("base-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("term"), arity: Arity::Values(1) },
            OptionDef { short: Some("T"), long: Some("term-file"), arity: Arity::Values(1) },
            OptionDef { short: Some("e"), long: Some("exclude-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("E"), long: Some("exclude-terms"), arity: Arity::Values(1) },
            OptionDef { short: Some("n"), long: Some("include-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("N"), long: Some("include-terms"), arity: Arity::Values(1) },
            OptionDef { short: Some("s"), long: Some("select"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("axioms"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("include-term"), arity: Arity::Values(1) },
            OptionDef { short: Some("r"), long: Some("trim"), arity: Arity::Values(1) },
            OptionDef { short: Some("S"), long: Some("signature"), arity: Arity::Values(1) },
            OptionDef { short: Some("p"), long: Some("preserve-structure"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("drop-axiom-annotations"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("allow-punning"), arity: Arity::Values(1) },
        ],
    ),
    (
        "rename",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("m"), long: Some("mappings"), arity: Arity::Values(1) },
            OptionDef { short: Some("r"), long: Some("prefix-mappings"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("allow-duplicates"), arity: Arity::Values(1) },
            OptionDef { short: Some("M"), long: Some("allow-missing-entities"), arity: Arity::Values(1) },
            OptionDef { short: Some("A"), long: Some("add-prefix"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("mapping"), arity: Arity::Values(2) },
        ],
    ),
    (
        "repair",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("O"), long: Some("output-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("m"), long: Some("merge-axiom-annotations"), arity: Arity::Values(1) },
            OptionDef { short: Some("r"), long: Some("invalid-references"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("annotation-property"), arity: Arity::Values(1) },
            OptionDef { short: Some("A"), long: Some("annotation-properties-file"), arity: Arity::Values(1) },
        ],
    ),
    (
        "report",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("p"), long: Some("profile"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("base-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("format"), arity: Arity::Values(1) },
            OptionDef { short: Some("F"), long: Some("fail-on"), arity: Arity::Values(1) },
            OptionDef { short: Some("l"), long: Some("labels"), arity: Arity::Values(1) },
            OptionDef { short: Some("P"), long: Some("print"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("tdb"), arity: Arity::Values(1) },
            OptionDef { short: Some("k"), long: Some("keep-tdb-mappings"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("tdb-directory"), arity: Arity::Values(1) },
            OptionDef { short: Some("L"), long: Some("limit"), arity: Arity::Values(1) },
            OptionDef { short: None, long: Some("standalone"), arity: Arity::Values(1) },
        ],
    ),
    (
        "template",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("O"), long: Some("ontology-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("V"), long: Some("version-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("template"), arity: Arity::Values(1) },
            OptionDef { short: Some("a"), long: Some("ancestors"), arity: Arity::Flag },
            OptionDef { short: Some("m"), long: Some("merge-before"), arity: Arity::Flag },
            OptionDef { short: Some("M"), long: Some("merge-after"), arity: Arity::Flag },
            OptionDef { short: Some("c"), long: Some("collapse-import-closure"), arity: Arity::Values(1) },
            OptionDef { short: Some("A"), long: Some("include-annotations"), arity: Arity::Values(1) },
            OptionDef { short: Some("f"), long: Some("force"), arity: Arity::Values(1) },
            OptionDef { short: Some("e"), long: Some("errors"), arity: Arity::Values(1) },
            OptionDef { short: Some("E"), long: Some("external-template"), arity: Arity::Values(1) },
        ],
    ),
    (
        "unmerge",
        &[
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
            OptionDef { short: Some("p"), long: Some("inputs"), arity: Arity::Values(1) },
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
        ],
    ),
    (
        "validate-profile",
        &[
            OptionDef { short: Some("o"), long: Some("output"), arity: Arity::Values(1) },
            OptionDef { short: Some("p"), long: Some("profile"), arity: Arity::Values(1) },
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("I"), long: Some("input-iri"), arity: Arity::Values(1) },
        ],
    ),
    (
        "verify",
        &[
            OptionDef { short: Some("F"), long: Some("fail-on-violation"), arity: Arity::Values(1) },
            OptionDef { short: Some("i"), long: Some("input"), arity: Arity::Values(1) },
            OptionDef { short: Some("O"), long: Some("output-dir"), arity: Arity::Values(1) },
            OptionDef { short: Some("t"), long: Some("tdb"), arity: Arity::Values(1) },
            OptionDef { short: Some("k"), long: Some("keep-tdb-mappings"), arity: Arity::Values(1) },
            OptionDef { short: Some("d"), long: Some("tdb-directory"), arity: Arity::Values(1) },
            OptionDef { short: Some("q"), long: Some("queries"), arity: Arity::Unlimited },
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn read(command_name: &str, line: &str) -> Result<Parsed, String> {
        let tokens: Vec<String> = line.split_whitespace().map(String::from).collect();
        parse(&command(command_name).expect("a ROBOT command"), &tokens)
    }

    fn options(parsed: &Parsed) -> Vec<(&str, Vec<&str>)> {
        parsed.options.iter().map(|(n, v)| (n.as_str(), v.iter().map(String::as_str).collect())).collect()
    }

    /// Each option is recorded under its long name, takes as many values as
    /// the command gives it, and the first token that is no option names the
    /// next command.
    #[test]
    fn a_command_reads_its_options_and_stops_at_the_next_command() {
        let p = read("annotate", "-a rdfs:comment c -l rdfs:label chien fr -p ex:x annotate -o y").unwrap();
        assert_eq!(
            options(&p),
            [
                ("--annotation", vec!["rdfs:comment", "c"]),
                ("--language-annotation", vec!["rdfs:label", "chien", "fr"]),
                ("--prefix", vec!["ex:x"]),
            ]
        );
        assert_eq!(p.consumed, 9);
        // `-p` is `--preserve-structure` on `remove`, which takes the short
        // name over from `--prefix`.
        let p = read("remove", "-p false --prefix ex:x").unwrap();
        assert_eq!(options(&p), [("--preserve-structure", vec!["false"]), ("--prefix", vec!["ex:x"])]);
    }

    /// A token that is no option of the command is a value where one is
    /// wanted: ODK's `query --format --csv` gives the format `--csv`, and
    /// `collapse -t -3` the threshold `-3`.
    #[test]
    fn a_token_that_is_no_option_is_a_value() {
        let p = read("query", "--input x --format --csv --query q.sparql out.csv").unwrap();
        assert_eq!(
            options(&p),
            [("--input", vec!["x"]), ("--format", vec!["--csv"]), ("--query", vec!["q.sparql", "out.csv"])]
        );
        let p = read("collapse", "-t -3").unwrap();
        assert_eq!(options(&p), [("--threshold", vec!["-3"])]);
        let p = read("query", "--queries a.rq b.rq --format tsv").unwrap();
        assert_eq!(options(&p), [("--queries", vec!["a.rq", "b.rq"]), ("--format", vec!["tsv"])]);
    }

    /// An option no command has ends the command's tokens, and is the name
    /// of the command that follows.
    #[test]
    fn an_unknown_option_ends_the_command() {
        let p = read("convert", "-i x --no-check -o y").unwrap();
        assert_eq!(options(&p), [("--input", vec!["x"])]);
        assert_eq!(p.consumed, 2);
        let p = read("remove", "-vq").unwrap();
        assert_eq!(options(&p), [("--verbose", vec![])]);
        assert_eq!(p.next.as_deref(), Some("q"));
    }

    /// A command takes owlmake's options with ROBOT's.
    #[test]
    fn a_command_takes_owlmakes_own_options() {
        let p = read("reason", "--axiom-generators SubClass,PropertyAssertion --properties ex:p,ex:q").unwrap();
        assert_eq!(
            options(&p),
            [("--axiom-generators", vec!["SubClass,PropertyAssertion"]), ("--properties", vec!["ex:p,ex:q"])]
        );
    }

    /// A long name may be given by a prefix of it that names no other, and a
    /// value may be attached: `--name=value`, `-xVALUE`.
    #[test]
    fn prefixes_and_attached_values_read_as_robot_reads_them() {
        let p = read("remove", "--sel classes -tUBERON:1 --term=UBERON:2").unwrap();
        assert_eq!(
            options(&p),
            [("--select", vec!["classes"]), ("--term", vec!["UBERON:1"]), ("--term", vec!["UBERON:2"])]
        );
        assert_eq!(
            read("remove", "--in x").unwrap_err(),
            "Ambiguous option: '--in'  (could be: 'input-format', 'input', 'input-iri', 'include-term', 'include-terms')"
        );
        assert_eq!(read("remove", "--term").unwrap_err(), "Missing argument for option: t");
        assert_eq!(read("annotate", "-a rdfs:comment -o y").unwrap_err(), "Missing argument for option: a");
    }
}
