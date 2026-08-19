//! A standalone Manchester class-expression *parser* for `owlmake template`.
//!
//! Unlike the lightweight expression parser embedded in [`crate::io::manchester`]
//! (which only resolves CURIEs/IRIs against a prefix map), this parser is built
//! for template cells, which may reference entities by **rdfs:label**
//! (single-quoted `'foo bar'` or, when unambiguous, a bare token), by CURIE
//! (`OBO:1234`), or by full IRI (`<http://...>`). Entity resolution is supplied
//! by the caller as a [`Resolver`] closure so the template engine can feed in a
//! label→IRI index built from the input ontology plus the template's own rows.
//!
//! Supported grammar (precedence `not` > `and` > `or`):
//!
//! ```text
//! expr        := or
//! or          := and ('or' and)*
//! and         := unary ('and' unary)*
//! unary       := 'not' unary | postfix
//! postfix     := primary | PROP 'some' unary | PROP 'only' unary
//!              | PROP 'value' IND | PROP 'Self'
//!              | PROP ('min'|'max'|'exactly') N [unary]
//! primary     := '(' or ')' | ENTITY
//! ```
//!
//! Cardinality restrictions accept an optional filler (`p min 2` ≡
//! `p min 2 owl:Thing`), as Manchester syntax permits. Property positions may
//! use `inverse PROP`. Errors carry a human-readable message with the offending
//! token, which the template engine surfaces with row/column context.

use horned_owl::model::{
    Build, ClassExpression as CE, Individual, ObjectPropertyExpression as OPE, RcStr,
};

/// Resolve an entity reference (label, CURIE, IRI, or bare token) to a full IRI.
/// Returns `None` when the reference is unknown (the parser turns this into an
/// error so unresolved labels do not silently become fresh IRIs).
pub type Resolver<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Parse a Manchester class expression `s` into a [`ClassExpression`].
///
/// `resolve` maps an entity token to an IRI. The parser never invents IRIs: if
/// `resolve` returns `None` for a referenced entity the parse fails with an
/// error naming the token, so a misspelt or missing label is reported rather
/// than silently minted as a new entity.
pub fn parse_class_expression(
    b: &Build<RcStr>,
    s: &str,
    resolve: &Resolver<'_>,
) -> Result<CE<RcStr>, String> {
    let toks = tokenize(s)?;
    if toks.is_empty() {
        return Err("empty class expression".to_string());
    }
    let mut p = Parser { toks, pos: 0, b, resolve };
    let ce = p.parse_or()?;
    if p.pos != p.toks.len() {
        return Err(format!(
            "unexpected trailing token '{}' in class expression",
            p.toks[p.pos].text
        ));
    }
    Ok(ce)
}

/// A token kind, so keywords are not confused with quoted labels that happen to
/// equal a keyword (e.g. a class literally labelled `some`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// `(`
    Open,
    /// `)`
    Close,
    /// A bareword: may be a keyword (`and`/`some`/...) or a bare entity ref.
    Word,
    /// A single-quoted `'label'` — always an entity reference.
    Quoted,
    /// A `<iri>` — always an entity reference.
    Iri,
    /// A bare integer — used as a cardinality count.
    Number,
}

#[derive(Clone, Debug)]
struct Token {
    text: String,
    kind: Kind,
}

struct Parser<'a> {
    toks: Vec<Token>,
    pos: usize,
    b: &'a Build<RcStr>,
    resolve: &'a Resolver<'a>,
}

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

    fn parse_or(&mut self) -> Result<CE<RcStr>, String> {
        let mut parts = vec![self.parse_and()?];
        while self.peek_kw().as_deref() == Some("or") {
            self.advance();
            parts.push(self.parse_and()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            CE::ObjectUnionOf(parts)
        })
    }

    fn parse_and(&mut self) -> Result<CE<RcStr>, String> {
        let mut parts = vec![self.parse_unary()?];
        while self.peek_kw().as_deref() == Some("and") {
            self.advance();
            parts.push(self.parse_unary()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            CE::ObjectIntersectionOf(parts)
        })
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
        // Parenthesised expression — never a property head.
        if self.peek().map(|t| t.kind) == Some(Kind::Open) {
            self.advance();
            let inner = self.parse_or()?;
            match self.advance() {
                Some(t) if t.kind == Kind::Close => {}
                _ => return Err("missing closing ')'".to_string()),
            }
            return Ok(inner);
        }

        // `inverse PROP <restriction>` or a bare property/class atom.
        let inverse = self.peek_kw().as_deref() == Some("inverse");
        if inverse {
            self.advance();
        }
        let head = self
            .advance()
            .ok_or_else(|| "expected a class or property name".to_string())?;
        if matches!(head.kind, Kind::Open | Kind::Close | Kind::Number) {
            return Err(format!("unexpected token '{}'", head.text));
        }

        let kw = self.peek_kw();
        match kw.as_deref() {
            Some("some") => {
                self.advance();
                let bce = self.parse_unary()?;
                Ok(CE::ObjectSomeValuesFrom {
                    ope: self.ope(&head, inverse)?,
                    bce: Box::new(bce),
                })
            }
            Some("only") => {
                self.advance();
                let bce = self.parse_unary()?;
                Ok(CE::ObjectAllValuesFrom {
                    ope: self.ope(&head, inverse)?,
                    bce: Box::new(bce),
                })
            }
            Some("value") => {
                self.advance();
                let ind = self
                    .advance()
                    .ok_or_else(|| "expected an individual after 'value'".to_string())?;
                let iri = self.entity_iri(&ind)?;
                Ok(CE::ObjectHasValue {
                    ope: self.ope(&head, inverse)?,
                    i: Individual::Named(self.b.named_individual(iri)),
                })
            }
            Some("self") => {
                self.advance();
                Ok(CE::ObjectHasSelf(self.ope(&head, inverse)?))
            }
            Some(k @ ("min" | "max" | "exactly")) => {
                self.advance();
                let num = self
                    .advance()
                    .filter(|t| t.kind == Kind::Number)
                    .ok_or_else(|| format!("expected a number after '{k}'"))?;
                let n: u32 = num
                    .text
                    .parse()
                    .map_err(|_| format!("invalid cardinality '{}'", num.text))?;
                // Optional qualifying filler; default owl:Thing.
                let bce = if self.starts_filler() {
                    Box::new(self.parse_unary()?)
                } else {
                    Box::new(CE::Class(self.b.class(OWL_THING)))
                };
                let ope = self.ope(&head, inverse)?;
                Ok(match k {
                    "min" => CE::ObjectMinCardinality { n, ope, bce },
                    "max" => CE::ObjectMaxCardinality { n, ope, bce },
                    _ => CE::ObjectExactCardinality { n, ope, bce },
                })
            }
            _ => {
                if inverse {
                    return Err("'inverse' must be followed by a property restriction".to_string());
                }
                let iri = self.entity_iri(&head)?;
                Ok(CE::Class(self.b.class(iri)))
            }
        }
    }

    /// Whether the upcoming token can begin a class-expression filler (i.e. it
    /// is not a closing paren, a binary operator, or end-of-input). Used to
    /// decide if a cardinality restriction has a qualifying filler.
    fn starts_filler(&self) -> bool {
        match self.peek() {
            None => false,
            Some(t) if t.kind == Kind::Close => false,
            Some(t) if t.kind == Kind::Word => {
                !matches!(t.text.to_ascii_lowercase().as_str(), "and" | "or")
            }
            _ => true,
        }
    }

    /// Resolve a token used in a property position to an ObjectPropertyExpression.
    fn ope(&self, tok: &Token, inverse: bool) -> Result<OPE<RcStr>, String> {
        let iri = self.entity_iri(tok)?;
        let op = self.b.object_property(iri);
        Ok(if inverse {
            OPE::InverseObjectProperty(op)
        } else {
            OPE::ObjectProperty(op)
        })
    }

    /// Resolve any entity token (Word/Quoted/Iri) to a full IRI via the caller's
    /// resolver, erroring if it is unknown.
    fn entity_iri(&self, tok: &Token) -> Result<String, String> {
        // A bareword that is actually a keyword should never reach here.
        if tok.kind == Kind::Word && is_keyword(&tok.text) {
            return Err(format!("unexpected keyword '{}'", tok.text));
        }
        (self.resolve)(&tok.text)
            .ok_or_else(|| format!("could not resolve entity '{}'", tok.text))
    }
}

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

fn is_keyword(w: &str) -> bool {
    matches!(
        w.to_ascii_lowercase().as_str(),
        "and" | "or" | "not" | "some" | "only" | "value" | "self" | "min" | "max" | "exactly"
            | "inverse"
    )
}

/// Tokenize a Manchester expression. Produces `(`/`)`, `<iri>`, `'quoted'`,
/// numbers, and barewords. Barewords run until whitespace, a paren, or a quote.
fn tokenize(s: &str) -> Result<Vec<Token>, String> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' | '{' | '[' => {
                toks.push(Token { text: "(".into(), kind: Kind::Open });
                chars.next();
            }
            ')' | '}' | ']' => {
                toks.push(Token { text: ")".into(), kind: Kind::Close });
                chars.next();
            }
            '<' => {
                chars.next();
                let mut iri = String::new();
                let mut closed = false;
                for d in chars.by_ref() {
                    if d == '>' {
                        closed = true;
                        break;
                    }
                    iri.push(d);
                }
                if !closed {
                    return Err("unterminated '<...>' IRI".to_string());
                }
                toks.push(Token { text: iri, kind: Kind::Iri });
            }
            '\'' => {
                chars.next();
                let mut label = String::new();
                let mut closed = false;
                while let Some(d) = chars.next() {
                    if d == '\\' {
                        // Allow escaped quote inside a label.
                        if let Some(&e) = chars.peek() {
                            label.push(e);
                            chars.next();
                        }
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
                toks.push(Token { text: label, kind: Kind::Quoted });
            }
            _ => {
                let mut word = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_whitespace()
                        || matches!(d, '(' | ')' | '{' | '}' | '[' | ']' | '\'' | '<')
                    {
                        break;
                    }
                    word.push(d);
                    chars.next();
                }
                let kind = if !word.is_empty() && word.chars().all(|c| c.is_ascii_digit()) {
                    Kind::Number
                } else {
                    Kind::Word
                };
                toks.push(Token { text: word, kind });
            }
        }
    }
    Ok(toks)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn bare_class() {
        assert_eq!(parse("cell").unwrap(), cls("cell"));
    }

    #[test]
    fn quoted_label() {
        assert_eq!(parse("'cell'").unwrap(), cls("cell"));
    }

    #[test]
    fn full_iri() {
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
}
