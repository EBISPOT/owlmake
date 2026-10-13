//! The Manchester class-expression parser `owlmake template` reads cells with.
//!
//! A cell names an entity by rdfs:label (single-quoted `'part of'`, or a bare
//! token for a label without white space), by the short form of its IRI (`W`),
//! by CURIE (`OBO:1234`) or by full IRI (`http://…`). The caller supplies the
//! names through [`Lookup`], one lookup per entity kind, so what a name is — a
//! class, a property, a datatype, an individual — is decided by what the caller
//! knows of it, and the parser never invents an entity: a name no lookup
//! resolves is an error naming the token.
//!
//! Supported grammar (precedence `not` > `and` > `or`):
//!
//! ```text
//! expr        := or
//! or          := and ('or' and)*
//! and         := unary ('and' unary)*
//! unary       := 'not' unary | postfix
//! postfix     := '(' or ')' | '{' IND (',' IND)* '}' | restriction | ENTITY
//! restriction := ['inverse'] PROP 'some' unary | PROP 'only' unary
//!              | PROP 'value' IND | PROP 'Self'
//!              | PROP ('min'|'max'|'exactly') N [unary]
//!              | DPROP 'some' range | DPROP 'only' range
//!              | DPROP 'value' literal
//!              | DPROP ('min'|'max'|'exactly') N [range]
//! range       := conj ('or' conj)*
//! conj        := primary ('and' primary)*
//! primary     := 'not' primary | '(' range ')' | '{' literal (',' literal)* '}'
//!              | DATATYPE ['[' facet literal (',' facet literal)* ']']
//! ```
//!
//! A restriction's property is a data property when the lookup knows the name
//! as one — asked first, so a name no lookup knows but a CURIE or IRI can be
//! made of becomes a data property — and an object property otherwise. A
//! literal is a quoted string, optionally typed with `^^` or tagged with `@`, an
//! integer, a decimal, a float written with an `f` suffix, or `true`/`false`. A
//! cardinality with no filler is over `owl:Thing` or `rdfs:Literal`.
//!
//! The tokenizer reads a `#` outside quotes as the start of a comment that runs
//! to the end of the line, so a bare `http://ex.org/x#p` names `http://ex.org/x`;
//! a bracketed `<iri>` is a token the lookups are asked about as written, and
//! none of the template's resolve it.

use horned_owl::model::{
    Build, ClassExpression as CE, DataProperty, DataRange, FacetRestriction, Individual, Literal,
    ObjectPropertyExpression as OPE, RcStr,
};
use horned_owl::vocab::Facet;

/// The entity names a cell may use, one lookup per kind. Each takes the token as
/// written — a quoted label keeps its quotes, a bracketed IRI its brackets — and
/// answers the IRI the name has, or `None`. `create` lets a class or datatype be
/// made from a CURIE or IRI the lookup does not know.
pub trait Lookup {
    fn class(&self, name: &str, create: bool) -> Option<String>;
    fn object_property(&self, name: &str) -> Option<String>;
    fn data_property(&self, name: &str) -> Option<String>;
    fn datatype(&self, name: &str) -> Option<String>;
    fn individual(&self, name: &str) -> Option<String>;
}

/// A plain resolver closure answers every kind but data properties, with a
/// quoted label's quotes and a bracketed IRI's brackets taken off: the names
/// of a caller that has no entity kinds to offer.
impl<F: Fn(&str) -> Option<String>> Lookup for F {
    fn class(&self, name: &str, _create: bool) -> Option<String> {
        self(bare(name))
    }
    fn object_property(&self, name: &str) -> Option<String> {
        self(bare(name))
    }
    fn data_property(&self, _name: &str) -> Option<String> {
        None
    }
    fn datatype(&self, name: &str) -> Option<String> {
        self(bare(name))
    }
    fn individual(&self, name: &str) -> Option<String> {
        self(bare(name))
    }
}

/// `name` without the quotes or angle brackets the tokenizer kept on it.
fn bare(name: &str) -> &str {
    let n = name.trim();
    if n.len() >= 2 && ((n.starts_with('\'') && n.ends_with('\'')) || (n.starts_with('<') && n.ends_with('>'))) {
        &n[1..n.len() - 1]
    } else {
        n
    }
}

/// Parse a Manchester class expression `s` into a [`ClassExpression`].
pub fn parse_class_expression(
    b: &Build<RcStr>,
    s: &str,
    names: &dyn Lookup,
) -> Result<CE<RcStr>, String> {
    let toks = tokenize(s)?;
    if toks.is_empty() {
        return Err("empty class expression".to_string());
    }
    let mut p = Parser { toks, pos: 0, b, names };
    let ce = p.parse_or()?;
    p.finish()?;
    Ok(ce)
}

/// The object property expressions a property cell names: a comma-separated
/// list, each `[inverse] NAME`.
pub fn parse_object_property_list(
    b: &Build<RcStr>,
    s: &str,
    names: &dyn Lookup,
) -> Result<Vec<OPE<RcStr>>, String> {
    let toks = tokenize(s)?;
    let mut p = Parser { toks, pos: 0, b, names };
    let mut out = Vec::new();
    loop {
        let inverse = p.peek_kw().as_deref() == Some("inverse");
        if inverse {
            p.advance();
        }
        let tok = p.name_token("an object property name")?;
        let iri = names
            .object_property(&tok.text)
            .ok_or_else(|| format!("could not resolve property '{}'", tok.text))?;
        let op = b.object_property(iri);
        out.push(if inverse { OPE::InverseObjectProperty(op) } else { OPE::ObjectProperty(op) });
        if p.peek().map(|t| t.kind) == Some(Kind::Comma) {
            p.advance();
            continue;
        }
        break;
    }
    p.finish()?;
    Ok(out)
}

/// The data properties a property cell names: a comma-separated list of names.
pub fn parse_data_property_list(
    b: &Build<RcStr>,
    s: &str,
    names: &dyn Lookup,
) -> Result<Vec<DataProperty<RcStr>>, String> {
    let toks = tokenize(s)?;
    let mut p = Parser { toks, pos: 0, b, names };
    let mut out = Vec::new();
    loop {
        let tok = p.name_token("a data property name")?;
        let iri = names
            .data_property(&tok.text)
            .ok_or_else(|| format!("could not resolve data property '{}'", tok.text))?;
        out.push(b.data_property(iri));
        if p.peek().map(|t| t.kind) == Some(Kind::Comma) {
            p.advance();
            continue;
        }
        break;
    }
    p.finish()?;
    Ok(out)
}

/// A token kind, so keywords are not confused with quoted labels that happen to
/// equal a keyword (e.g. a class literally labelled `some`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// `(` / `)`
    Open,
    Close,
    /// `{` / `}`
    BraceOpen,
    BraceClose,
    /// `[` / `]`
    BracketOpen,
    BracketClose,
    Comma,
    /// A bareword: may be a keyword (`and`/`some`/...) or a bare entity ref.
    Word,
    /// A single-quoted `'label'`, quotes kept — always an entity reference.
    Quoted,
    /// A `<iri>`, brackets kept.
    Iri,
    /// A number: an integer, a decimal, or a float with its `f` suffix taken off.
    Number,
    /// A double-quoted string literal, optionally typed or language-tagged.
    Str,
    /// A facet operator: `<`, `<=`, `>`, `>=`.
    Op,
}

#[derive(Clone, Debug)]
struct Token {
    text: String,
    kind: Kind,
    /// A string literal's `^^datatype`.
    dt: Option<String>,
    /// A string literal's `@lang`.
    lang: Option<String>,
    /// A number written with an `f` suffix.
    float: bool,
}

impl Token {
    fn new(text: impl Into<String>, kind: Kind) -> Token {
        Token { text: text.into(), kind, dt: None, lang: None, float: false }
    }
}

struct Parser<'a> {
    toks: Vec<Token>,
    pos: usize,
    b: &'a Build<RcStr>,
    names: &'a dyn Lookup,
}

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const RDFS_LITERAL: &str = "http://www.w3.org/2000/01/rdf-schema#Literal";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }
    /// The lowercased text of the next token *only if it is a bareword* — used
    /// for keyword matching so a quoted `'and'` label is never read as the
    /// operator.
    fn peek_kw(&self) -> Option<String> {
        self.peek()
            .filter(|t| t.kind == Kind::Word)
            .map(|t| t.text.to_ascii_lowercase())
    }
    fn advance(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn finish(&self) -> Result<(), String> {
        match self.peek() {
            None => Ok(()),
            Some(t) => Err(format!("unexpected trailing token '{}' in class expression", t.text)),
        }
    }
    /// The next token, which must be able to name an entity.
    fn name_token(&mut self, what: &str) -> Result<Token, String> {
        match self.advance() {
            Some(t) if matches!(t.kind, Kind::Word | Kind::Quoted | Kind::Iri) => {
                if t.kind == Kind::Word && is_keyword(&t.text) {
                    return Err(format!("unexpected keyword '{}'", t.text));
                }
                Ok(t)
            }
            Some(t) => Err(format!("expected {what}, found '{}'", t.text)),
            None => Err(format!("expected {what}")),
        }
    }
    fn expect(&mut self, kind: Kind, what: &str) -> Result<(), String> {
        match self.advance() {
            Some(t) if t.kind == kind => Ok(()),
            Some(t) => Err(format!("expected {what}, found '{}'", t.text)),
            None => Err(format!("expected {what}")),
        }
    }

    fn parse_or(&mut self) -> Result<CE<RcStr>, String> {
        let mut parts = vec![self.parse_and()?];
        while self.peek_kw().as_deref() == Some("or") {
            self.advance();
            parts.push(self.parse_and()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { CE::ObjectUnionOf(parts) })
    }

    fn parse_and(&mut self) -> Result<CE<RcStr>, String> {
        let mut parts = vec![self.parse_unary()?];
        while self.peek_kw().as_deref() == Some("and") {
            self.advance();
            parts.push(self.parse_unary()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { CE::ObjectIntersectionOf(parts) })
    }

    fn parse_unary(&mut self) -> Result<CE<RcStr>, String> {
        if self.peek_kw().as_deref() == Some("not") {
            self.advance();
            return Ok(CE::ObjectComplementOf(Box::new(self.parse_unary()?)));
        }
        self.parse_postfix()
    }

    /// A primary, optionally followed by a restriction keyword that makes the
    /// primary's leading token a property. Manchester syntax only allows a
    /// restriction directly after an atomic property name, so one atom is read
    /// and the following keyword inspected.
    fn parse_postfix(&mut self) -> Result<CE<RcStr>, String> {
        match self.peek().map(|t| t.kind) {
            // Parenthesised expression — never a property head.
            Some(Kind::Open) => {
                self.advance();
                let inner = self.parse_or()?;
                self.expect(Kind::Close, "')'")?;
                return Ok(inner);
            }
            Some(Kind::BraceOpen) => return self.parse_one_of(),
            _ => {}
        }

        // `inverse PROP <restriction>` or a bare property/class atom.
        let inverse = self.peek_kw().as_deref() == Some("inverse");
        if inverse {
            self.advance();
        }
        let head = self.name_token("a class or property name")?;

        let restriction = matches!(
            self.peek_kw().as_deref(),
            Some("some" | "only" | "value" | "self" | "min" | "max" | "exactly")
        );
        if !restriction {
            if inverse {
                return Err("'inverse' must be followed by a property restriction".to_string());
            }
            let iri = self
                .names
                .class(&head.text, true)
                .ok_or_else(|| format!("could not resolve entity '{}'", head.text))?;
            return Ok(CE::Class(self.b.class(iri)));
        }
        if !inverse {
            if let Some(dp) = self.names.data_property(&head.text) {
                let dp = self.b.data_property(dp);
                return self.parse_data_restriction(dp);
            }
        }
        let iri = self
            .names
            .object_property(&head.text)
            .ok_or_else(|| format!("could not resolve property '{}'", head.text))?;
        let op = self.b.object_property(iri);
        let ope = if inverse { OPE::InverseObjectProperty(op) } else { OPE::ObjectProperty(op) };
        self.parse_object_restriction(ope)
    }

    /// `{ a, b }`: the class of those individuals.
    fn parse_one_of(&mut self) -> Result<CE<RcStr>, String> {
        self.expect(Kind::BraceOpen, "'{'")?;
        let mut members = Vec::new();
        loop {
            members.push(self.parse_individual()?);
            match self.advance() {
                Some(t) if t.kind == Kind::Comma => continue,
                Some(t) if t.kind == Kind::BraceClose => break,
                Some(t) => return Err(format!("expected ',' or '}}', found '{}'", t.text)),
                None => return Err("missing closing '}'".to_string()),
            }
        }
        Ok(CE::ObjectOneOf(members))
    }

    fn parse_individual(&mut self) -> Result<Individual<RcStr>, String> {
        let tok = self.name_token("an individual name")?;
        let iri = self
            .names
            .individual(&tok.text)
            .ok_or_else(|| format!("could not resolve individual '{}'", tok.text))?;
        Ok(Individual::Named(self.b.named_individual(iri)))
    }

    fn parse_object_restriction(&mut self, ope: OPE<RcStr>) -> Result<CE<RcStr>, String> {
        let kw = self.advance().map(|t| t.text.to_ascii_lowercase()).unwrap_or_default();
        Ok(match kw.as_str() {
            "some" => CE::ObjectSomeValuesFrom { ope, bce: Box::new(self.parse_unary()?) },
            "only" => CE::ObjectAllValuesFrom { ope, bce: Box::new(self.parse_unary()?) },
            "value" => CE::ObjectHasValue { ope, i: self.parse_individual()? },
            "self" => CE::ObjectHasSelf(ope),
            k @ ("min" | "max" | "exactly") => {
                let n = self.cardinality(k)?;
                // Optional qualifying filler; default owl:Thing.
                let bce = if self.starts_filler() {
                    Box::new(self.parse_unary()?)
                } else {
                    Box::new(CE::Class(self.b.class(OWL_THING)))
                };
                match k {
                    "min" => CE::ObjectMinCardinality { n, ope, bce },
                    "max" => CE::ObjectMaxCardinality { n, ope, bce },
                    _ => CE::ObjectExactCardinality { n, ope, bce },
                }
            }
            other => return Err(format!("unexpected '{other}' after a property")),
        })
    }

    fn parse_data_restriction(&mut self, dp: DataProperty<RcStr>) -> Result<CE<RcStr>, String> {
        let kw = self.advance().map(|t| t.text.to_ascii_lowercase()).unwrap_or_default();
        Ok(match kw.as_str() {
            "some" => CE::DataSomeValuesFrom { dp, dr: self.parse_data_range()? },
            "only" => CE::DataAllValuesFrom { dp, dr: self.parse_data_range()? },
            "value" => CE::DataHasValue { dp, l: self.parse_literal()? },
            "self" => return Err("'Self' needs an object property".to_string()),
            k @ ("min" | "max" | "exactly") => {
                let n = self.cardinality(k)?;
                // Optional qualifying filler; default rdfs:Literal.
                let dr = if self.starts_filler() {
                    self.parse_data_range()?
                } else {
                    DataRange::Datatype(self.b.datatype(RDFS_LITERAL))
                };
                match k {
                    "min" => CE::DataMinCardinality { n, dp, dr },
                    "max" => CE::DataMaxCardinality { n, dp, dr },
                    _ => CE::DataExactCardinality { n, dp, dr },
                }
            }
            other => return Err(format!("unexpected '{other}' after a property")),
        })
    }

    fn cardinality(&mut self, k: &str) -> Result<u32, String> {
        let num = self
            .advance()
            .filter(|t| t.kind == Kind::Number)
            .ok_or_else(|| format!("expected a number after '{k}'"))?;
        num.text
            .parse()
            .map_err(|_| format!("invalid cardinality '{}'", num.text))
    }

    /// Whether the upcoming token can begin a filler (i.e. it is not a closing
    /// bracket, a binary operator, a comma, or end-of-input). Used to decide if
    /// a cardinality restriction has a qualifying filler.
    fn starts_filler(&self) -> bool {
        match self.peek() {
            None => false,
            Some(t) if matches!(t.kind, Kind::Close | Kind::BraceClose | Kind::BracketClose | Kind::Comma) => false,
            Some(t) if t.kind == Kind::Word => {
                !matches!(t.text.to_ascii_lowercase().as_str(), "and" | "or")
            }
            _ => true,
        }
    }

    fn parse_data_range(&mut self) -> Result<DataRange<RcStr>, String> {
        let mut parts = vec![self.parse_data_conjunction()?];
        while self.peek_kw().as_deref() == Some("or") {
            self.advance();
            parts.push(self.parse_data_conjunction()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { DataRange::DataUnionOf(parts) })
    }

    fn parse_data_conjunction(&mut self) -> Result<DataRange<RcStr>, String> {
        let mut parts = vec![self.parse_data_primary()?];
        while self.peek_kw().as_deref() == Some("and") {
            self.advance();
            parts.push(self.parse_data_primary()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            DataRange::DataIntersectionOf(parts)
        })
    }

    fn parse_data_primary(&mut self) -> Result<DataRange<RcStr>, String> {
        if self.peek_kw().as_deref() == Some("not") {
            self.advance();
            return Ok(DataRange::DataComplementOf(Box::new(self.parse_data_primary()?)));
        }
        match self.peek().map(|t| t.kind) {
            Some(Kind::Open) => {
                self.advance();
                let inner = self.parse_data_range()?;
                self.expect(Kind::Close, "')'")?;
                Ok(inner)
            }
            Some(Kind::BraceOpen) => {
                self.advance();
                let mut literals = Vec::new();
                loop {
                    literals.push(self.parse_literal()?);
                    match self.advance() {
                        Some(t) if t.kind == Kind::Comma => continue,
                        Some(t) if t.kind == Kind::BraceClose => break,
                        Some(t) => return Err(format!("expected ',' or '}}', found '{}'", t.text)),
                        None => return Err("missing closing '}'".to_string()),
                    }
                }
                Ok(DataRange::DataOneOf(literals))
            }
            _ => {
                let tok = self.name_token("a data range")?;
                let iri = self
                    .names
                    .datatype(&tok.text)
                    .ok_or_else(|| format!("could not resolve datatype '{}'", tok.text))?;
                let dt = self.b.datatype(iri);
                if self.peek().map(|t| t.kind) == Some(Kind::BracketOpen) {
                    self.advance();
                    let facets = self.parse_facets()?;
                    Ok(DataRange::DatatypeRestriction(dt, facets))
                } else {
                    Ok(DataRange::Datatype(dt))
                }
            }
        }
    }

    /// The facet restrictions between `[` and `]`, each a facet and a literal.
    fn parse_facets(&mut self) -> Result<Vec<FacetRestriction<RcStr>>, String> {
        let mut out = Vec::new();
        loop {
            let tok = self.advance().ok_or_else(|| "expected a facet".to_string())?;
            let f = match (tok.kind, tok.text.as_str()) {
                (Kind::Op, "<") => Facet::MaxExclusive,
                (Kind::Op, "<=") => Facet::MaxInclusive,
                (Kind::Op, ">") => Facet::MinExclusive,
                (Kind::Op, ">=") => Facet::MinInclusive,
                (Kind::Word, "length") => Facet::Length,
                (Kind::Word, "minLength") => Facet::MinLength,
                (Kind::Word, "maxLength") => Facet::MaxLength,
                (Kind::Word, "pattern") => Facet::Pattern,
                (Kind::Word, "langRange") => Facet::LangRange,
                (Kind::Word, "totalDigits") => Facet::TotalDigits,
                (Kind::Word, "fractionDigits") => Facet::FractionDigits,
                _ => return Err(format!("expected a facet, found '{}'", tok.text)),
            };
            let l = self.parse_literal()?;
            out.push(FacetRestriction { f, l });
            match self.advance() {
                Some(t) if t.kind == Kind::Comma => continue,
                Some(t) if t.kind == Kind::BracketClose => break,
                Some(t) => return Err(format!("expected ',' or ']', found '{}'", t.text)),
                None => return Err("missing closing ']'".to_string()),
            }
        }
        Ok(out)
    }

    /// A literal, made as a data factory makes one from its text.
    fn parse_literal(&mut self) -> Result<Literal<RcStr>, String> {
        let tok = self.advance().ok_or_else(|| "expected a literal".to_string())?;
        let lit = match tok.kind {
            Kind::Str => {
                if let Some(dt) = &tok.dt {
                    let iri = self
                        .names
                        .datatype(dt)
                        .ok_or_else(|| format!("could not resolve datatype '{dt}'"))?;
                    Literal::Datatype { literal: tok.text, datatype_iri: self.b.iri(iri) }
                } else if let Some(lang) = tok.lang {
                    Literal::Language { literal: tok.text, lang }
                } else {
                    Literal::Simple { literal: tok.text }
                }
            }
            Kind::Number => {
                let (literal, datatype) = if tok.float {
                    (tok.text, "float")
                } else if tok.text.contains('.') {
                    (tok.text, "decimal")
                } else {
                    let n: i64 = tok
                        .text
                        .parse()
                        .map_err(|_| format!("'{}' is not an integer", tok.text))?;
                    (n.to_string(), "integer")
                };
                Literal::Datatype { literal, datatype_iri: self.b.iri(format!("{XSD}{datatype}")) }
            }
            Kind::Word if tok.text.eq_ignore_ascii_case("true") || tok.text.eq_ignore_ascii_case("false") => {
                Literal::Datatype {
                    literal: tok.text.to_ascii_lowercase(),
                    datatype_iri: self.b.iri(format!("{XSD}boolean")),
                }
            }
            _ => return Err(format!("expected a literal, found '{}'", tok.text)),
        };
        Ok(crate::model::literal_as_made(lit))
    }
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w.to_ascii_lowercase().as_str(),
        "and" | "or" | "not" | "some" | "only" | "value" | "self" | "min" | "max" | "exactly"
            | "inverse"
    )
}

/// Whether a bareword is a number: an optional sign, digits, an optional
/// fraction, and an optional `f` suffix.
fn is_number(w: &str) -> bool {
    let w = w.strip_suffix(['f', 'F']).unwrap_or(w);
    let w = w.strip_prefix(['+', '-']).unwrap_or(w);
    let (int, frac) = match w.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (w, None),
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    digits(int) && frac.is_none_or(digits)
}

/// Tokenize a Manchester expression. Produces brackets, commas, `<iri>` and
/// `'quoted'` tokens as written, string literals, numbers, facet operators and
/// barewords. A bareword runs until white space, a bracket, a comma, a quote,
/// an angle bracket or a `#`; a `#` starts a comment that runs to the end of
/// the line.
fn tokenize(s: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let is_delim = |c: char| c.is_whitespace() || matches!(c, '(' | ')' | '{' | '}' | '[' | ']' | ',' | '\'' | '"' | '<' | '>' | '#');
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '#' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '(' | ')' | '{' | '}' | '[' | ']' | ',' => {
                let kind = match c {
                    '(' => Kind::Open,
                    ')' => Kind::Close,
                    '{' => Kind::BraceOpen,
                    '}' => Kind::BraceClose,
                    '[' => Kind::BracketOpen,
                    ']' => Kind::BracketClose,
                    _ => Kind::Comma,
                };
                toks.push(Token::new(c.to_string(), kind));
                i += 1;
            }
            '"' => {
                i += 1;
                let mut text = String::new();
                let mut closed = false;
                while i < chars.len() {
                    let d = chars[i];
                    i += 1;
                    if d == '\\' && i < chars.len() {
                        text.push(chars[i]);
                        i += 1;
                    } else if d == '"' {
                        closed = true;
                        break;
                    } else {
                        text.push(d);
                    }
                }
                if !closed {
                    return Err("unterminated string literal".to_string());
                }
                let mut tok = Token::new(text, Kind::Str);
                if chars[i..].starts_with(&['^', '^']) {
                    i += 2;
                    let start = i;
                    while i < chars.len() && !is_delim(chars[i]) {
                        i += 1;
                    }
                    tok.dt = Some(chars[start..i].iter().collect());
                } else if i < chars.len() && chars[i] == '@' {
                    i += 1;
                    let start = i;
                    while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '-') {
                        i += 1;
                    }
                    tok.lang = Some(chars[start..i].iter().collect());
                }
                toks.push(tok);
            }
            '\'' => {
                i += 1;
                let mut label = String::new();
                let mut closed = false;
                while i < chars.len() {
                    let d = chars[i];
                    i += 1;
                    if d == '\\' && i < chars.len() {
                        label.push(chars[i]);
                        i += 1;
                    } else if d == '\'' {
                        closed = true;
                        break;
                    } else {
                        label.push(d);
                    }
                }
                if !closed {
                    return Err("unterminated single-quoted label".to_string());
                }
                toks.push(Token::new(format!("'{label}'"), Kind::Quoted));
            }
            '<' => {
                let next = chars.get(i + 1).copied();
                if next == Some('=') {
                    toks.push(Token::new("<=", Kind::Op));
                    i += 2;
                } else if next.is_none_or(|d| d.is_whitespace() || d.is_ascii_digit()) {
                    toks.push(Token::new("<", Kind::Op));
                    i += 1;
                } else {
                    let start = i;
                    i += 1;
                    while i < chars.len() && chars[i] != '>' {
                        i += 1;
                    }
                    if i >= chars.len() {
                        return Err("unterminated '<...>' IRI".to_string());
                    }
                    i += 1;
                    toks.push(Token::new(chars[start..i].iter().collect::<String>(), Kind::Iri));
                }
            }
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    toks.push(Token::new(">=", Kind::Op));
                    i += 2;
                } else {
                    toks.push(Token::new(">", Kind::Op));
                    i += 1;
                }
            }
            _ => {
                let start = i;
                while i < chars.len() && !is_delim(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if is_number(&word) {
                    let float = word.ends_with(['f', 'F']);
                    let text = if float { word[..word.len() - 1].to_string() } else { word };
                    toks.push(Token { text, kind: Kind::Number, dt: None, lang: None, float });
                } else {
                    toks.push(Token::new(word, Kind::Word));
                }
            }
        }
    }
    Ok(toks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A resolver that maps a few labels/CURIEs to IRIs and passes IRIs through.
    fn res(tok: &str) -> Option<String> {
        let base = "http://example.org/";
        match tok {
            // labels
            "cell" => Some(format!("{base}cell")),
            "neuron" => Some(format!("{base}neuron")),
            "tissue" => Some(format!("{base}tissue")),
            "heart" => Some(format!("{base}heart")),
            "part of" | "part_of" => Some(format!("{base}part_of")),
            "has part" => Some(format!("{base}has_part")),
            "bob" => Some(format!("{base}bob")),
            // curies
            "EX:1" => Some(format!("{base}1")),
            "EX:2" => Some(format!("{base}2")),
            _ => {
                if tok.starts_with("http://") || tok.starts_with("https://") {
                    Some(tok.to_string())
                } else {
                    None
                }
            }
        }
    }

    fn parse(s: &str) -> Result<CE<RcStr>, String> {
        let b = Build::new();
        parse_class_expression(&b, s, &res)
    }

    fn cls(name: &str) -> CE<RcStr> {
        let b = Build::new();
        CE::Class(b.class(res(name).unwrap()))
    }

    /// Names with kinds, as a template's checker holds them: labels and short
    /// forms per kind, CURIEs made of two prefixes, and a property no name
    /// knows made a data property first.
    struct Kinds {
        maps: [HashMap<&'static str, &'static str>; 5],
    }

    impl Kinds {
        fn new() -> Kinds {
            let classes = HashMap::from([("cell", "http://example.org/cell"), ("W", "http://example.org/x#W")]);
            let ops = HashMap::from([("part of", "http://example.org/part_of")]);
            let dps = HashMap::from([("dee", "http://example.org/x#d")]);
            let dts = HashMap::new();
            let inds = HashMap::from([("bob", "http://example.org/bob")]);
            Kinds { maps: [classes, ops, dps, dts, inds] }
        }
        fn find(&self, i: usize, name: &str) -> Option<String> {
            let m = &self.maps[i];
            m.get(name)
                .or_else(|| m.get(name.trim_matches('\'')))
                .map(|s| s.to_string())
        }
        fn create(&self, name: &str) -> Option<String> {
            if let Some(rest) = name.strip_prefix("xsd:") {
                Some(format!("{XSD}{rest}"))
            } else if let Some(rest) = name.strip_prefix("obo:") {
                Some(format!("http://purl.obolibrary.org/obo/{rest}"))
            } else if name.starts_with("http://") {
                Some(name.to_string())
            } else {
                None
            }
        }
    }

    impl Lookup for Kinds {
        fn class(&self, name: &str, create: bool) -> Option<String> {
            self.find(0, name).or_else(|| if create { self.create(name) } else { None })
        }
        fn object_property(&self, name: &str) -> Option<String> {
            self.find(1, name)
                .or_else(|| self.create(name).filter(|iri| !self.maps[2].values().any(|v| v == iri)))
        }
        fn data_property(&self, name: &str) -> Option<String> {
            self.find(2, name)
                .or_else(|| self.create(name).filter(|iri| !self.maps[1].values().any(|v| v == iri)))
        }
        fn datatype(&self, name: &str) -> Option<String> {
            self.find(3, name).or_else(|| self.create(name))
        }
        fn individual(&self, name: &str) -> Option<String> {
            self.find(4, name).or_else(|| self.create(name))
        }
    }

    fn parse_kinds(s: &str) -> Result<CE<RcStr>, String> {
        let b = Build::new();
        parse_class_expression(&b, s, &Kinds::new())
    }

    #[test]
    fn bare_class() {
        assert_eq!(parse("cell").unwrap(), cls("cell"));
    }

    #[test]
    fn quoted_label() {
        assert_eq!(parse("'cell'").unwrap(), cls("cell"));
    }

    #[test]
    fn full_iri_for_a_plain_resolver() {
        let b = Build::new();
        assert_eq!(
            parse("<http://example.org/cell>").unwrap(),
            CE::Class(b.class("http://example.org/cell"))
        );
    }

    #[test]
    fn curie() {
        let b = Build::new();
        assert_eq!(parse("EX:1").unwrap(), CE::Class(b.class("http://example.org/1")));
    }

    #[test]
    fn some_restriction() {
        let ce = parse("'part of' some heart").unwrap();
        match ce {
            CE::ObjectSomeValuesFrom { ope, bce } => {
                assert!(matches!(ope, OPE::ObjectProperty(_)));
                assert_eq!(*bce, cls("heart"));
            }
            other => panic!("expected some restriction, got {other:?}"),
        }
    }

    #[test]
    fn only_restriction() {
        assert!(matches!(
            parse("'part of' only heart").unwrap(),
            CE::ObjectAllValuesFrom { .. }
        ));
    }

    #[test]
    fn value_restriction() {
        match parse("'part of' value bob").unwrap() {
            CE::ObjectHasValue { i, .. } => {
                assert!(matches!(i, Individual::Named(_)));
            }
            other => panic!("expected value restriction, got {other:?}"),
        }
    }

    #[test]
    fn self_restriction() {
        assert!(matches!(parse("'part of' Self").unwrap(), CE::ObjectHasSelf(_)));
    }

    #[test]
    fn cardinality_qualified_and_unqualified() {
        match parse("'has part' min 2 cell").unwrap() {
            CE::ObjectMinCardinality { n, bce, .. } => {
                assert_eq!(n, 2);
                assert_eq!(*bce, cls("cell"));
            }
            other => panic!("got {other:?}"),
        }
        match parse("'has part' max 3").unwrap() {
            CE::ObjectMaxCardinality { n, bce, .. } => {
                assert_eq!(n, 3);
                assert!(matches!(*bce, CE::Class(_))); // owl:Thing default
            }
            other => panic!("got {other:?}"),
        }
        assert!(matches!(
            parse("'has part' exactly 1 cell").unwrap(),
            CE::ObjectExactCardinality { n: 1, .. }
        ));
    }

    #[test]
    fn not_binds_tighter_than_and() {
        // not cell and tissue  ==  (not cell) and tissue
        match parse("not cell and tissue").unwrap() {
            CE::ObjectIntersectionOf(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[0], CE::ObjectComplementOf(_)));
                assert_eq!(parts[1], cls("tissue"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // cell and tissue or neuron == (cell and tissue) or neuron
        match parse("cell and tissue or neuron").unwrap() {
            CE::ObjectUnionOf(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[0], CE::ObjectIntersectionOf(_)));
                assert_eq!(parts[1], cls("neuron"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parentheses_override_precedence() {
        // cell and (tissue or neuron)
        match parse("cell and (tissue or neuron)").unwrap() {
            CE::ObjectIntersectionOf(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0], cls("cell"));
                assert!(matches!(parts[1], CE::ObjectUnionOf(_)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn nested_restriction_filler() {
        // 'part of' some ('part of' some heart)
        match parse("'part of' some ('part of' some heart)").unwrap() {
            CE::ObjectSomeValuesFrom { bce, .. } => {
                assert!(matches!(*bce, CE::ObjectSomeValuesFrom { .. }));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn inverse_property() {
        match parse("inverse 'part of' some heart").unwrap() {
            CE::ObjectSomeValuesFrom { ope, .. } => {
                assert!(matches!(ope, OPE::InverseObjectProperty(_)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn one_of_individuals() {
        match parse("{ bob, bob }").unwrap() {
            CE::ObjectOneOf(members) => assert_eq!(members.len(), 2),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unknown_entity_errors() {
        let e = parse("nonexistent").unwrap_err();
        assert!(e.contains("could not resolve"), "{e}");
    }

    #[test]
    fn unbalanced_paren_errors() {
        assert!(parse("(cell and tissue").is_err());
    }

    #[test]
    fn trailing_garbage_errors() {
        assert!(parse("cell tissue").is_err());
    }

    /// A `#` outside quotes starts a comment, so a bare IRI with a fragment
    /// names the IRI before the `#`.
    #[test]
    fn a_hash_starts_a_comment() {
        let b = Build::new();
        assert_eq!(
            parse_kinds("'part of' some http://example.org/x#W").unwrap(),
            CE::ObjectSomeValuesFrom {
                ope: OPE::ObjectProperty(b.object_property("http://example.org/part_of")),
                bce: Box::new(CE::Class(b.class("http://example.org/x"))),
            }
        );
    }

    /// A bracketed IRI is asked about as written, and a template's names know
    /// none; a quoted CURIE is a label and no CURIE.
    #[test]
    fn bracketed_iris_and_quoted_curies_name_nothing_for_kinds() {
        assert!(parse_kinds("<http://example.org/x#W>").is_err());
        assert!(parse_kinds("'obo:q'").is_err());
        assert!(parse_kinds("obo:q").is_ok());
    }

    /// A data property's restrictions take data ranges and literals.
    #[test]
    fn data_restrictions() {
        let b = Build::new();
        let dp = b.data_property("http://example.org/x#d");
        let string = DataRange::Datatype(b.datatype(format!("{XSD}string")));
        assert_eq!(
            parse_kinds("'dee' some xsd:string").unwrap(),
            CE::DataSomeValuesFrom { dp: dp.clone(), dr: string.clone() }
        );
        assert_eq!(
            parse_kinds("dee only xsd:string").unwrap(),
            CE::DataAllValuesFrom { dp: dp.clone(), dr: string.clone() }
        );
        assert_eq!(
            parse_kinds("'dee' min 2").unwrap(),
            CE::DataMinCardinality { n: 2, dp: dp.clone(), dr: DataRange::Datatype(b.datatype(RDFS_LITERAL)) }
        );
        assert_eq!(
            parse_kinds("'dee' exactly 1 xsd:string").unwrap(),
            CE::DataExactCardinality { n: 1, dp: dp.clone(), dr: string }
        );
        assert!(parse_kinds("'dee' some cell").is_err(), "a class is no data range");
        assert!(parse_kinds("'dee' Self").is_err());
    }

    /// A name no lookup knows that a CURIE can be made of is a data property
    /// before it is an object property.
    #[test]
    fn an_unknown_property_is_a_data_property_first() {
        assert!(matches!(parse_kinds("obo:q some xsd:string").unwrap(), CE::DataSomeValuesFrom { .. }));
        assert!(parse_kinds("obo:q some cell").is_err());
        assert!(matches!(parse_kinds("obo:q min 2").unwrap(), CE::DataMinCardinality { .. }));
    }

    #[test]
    fn literals() {
        let b = Build::new();
        let typed = |text: &str, dt: &str| Literal::Datatype { literal: text.to_string(), datatype_iri: b.iri(format!("{XSD}{dt}")) };
        let value = |s: &str| match parse_kinds(s).unwrap() {
            CE::DataHasValue { l, .. } => l,
            other => panic!("got {other:?}"),
        };
        assert_eq!(value("'dee' value 5"), typed("5", "integer"));
        assert_eq!(value("'dee' value -3"), typed("-3", "integer"));
        assert_eq!(value("'dee' value 5.5"), typed("5.5", "decimal"));
        assert_eq!(value("'dee' value 1.5f"), typed("1.5", "float"));
        assert_eq!(value("'dee' value true"), typed("true", "boolean"));
        assert_eq!(value("'dee' value \"x\""), Literal::Simple { literal: "x".to_string() });
        assert_eq!(
            value("'dee' value \"x\"@en"),
            Literal::Language { literal: "x".to_string(), lang: "en".to_string() }
        );
        assert_eq!(value("'dee' value \"5\"^^xsd:string"), typed("5", "string"));
        assert!(parse_kinds("'dee' value 5^^xsd:string").is_err());
        assert!(parse_kinds("'dee' value cell").is_err());
    }

    #[test]
    fn data_ranges() {
        let b = Build::new();
        let int = b.datatype(format!("{XSD}integer"));
        match parse_kinds("'dee' some {\"a\", \"b\"}").unwrap() {
            CE::DataSomeValuesFrom { dr: DataRange::DataOneOf(ls), .. } => assert_eq!(ls.len(), 2),
            other => panic!("got {other:?}"),
        }
        match parse_kinds("'dee' some xsd:integer[>= 5, < 10]").unwrap() {
            CE::DataSomeValuesFrom { dr: DataRange::DatatypeRestriction(dt, facets), .. } => {
                assert_eq!(dt, int);
                assert_eq!(facets.len(), 2);
                assert_eq!(facets[0].f, Facet::MinInclusive);
                assert_eq!(facets[1].f, Facet::MaxExclusive);
                assert_eq!(facets[0].l, Literal::Datatype { literal: "5".to_string(), datatype_iri: b.iri(format!("{XSD}integer")) });
            }
            other => panic!("got {other:?}"),
        }
        assert!(matches!(
            parse_kinds("'dee' only not xsd:string").unwrap(),
            CE::DataAllValuesFrom { dr: DataRange::DataComplementOf(_), .. }
        ));
        assert!(matches!(
            parse_kinds("'dee' some (xsd:string or xsd:integer)").unwrap(),
            CE::DataSomeValuesFrom { dr: DataRange::DataUnionOf(_), .. }
        ));
        assert!(parse_kinds("'dee' some xsd:integer[length cell]").is_err());
    }

    #[test]
    fn property_lists() {
        let b = Build::new();
        let ops = parse_object_property_list(&b, "'part of', inverse 'part of'", &Kinds::new()).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[1], OPE::InverseObjectProperty(_)));
        // The data property `dee` is no object property, and a bracketed IRI names nothing.
        assert!(parse_object_property_list(&b, "dee", &Kinds::new()).is_err());
        assert!(parse_object_property_list(&b, "<http://example.org/x#p>", &Kinds::new()).is_err());
        let dps = parse_data_property_list(&b, "http://example.org/x#d", &Kinds::new()).unwrap();
        assert_eq!(dps[0], b.data_property("http://example.org/x"), "a bare # starts a comment");
        assert!(parse_data_property_list(&b, "inverse dee", &Kinds::new()).is_err());
    }
}
