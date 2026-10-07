//! A pattern's logical text read as a Manchester-syntax class expression or
//! axiom, every name in it resolved by a [`Names`].
//!
//! The text is split into tokens: a `'…'` or `"…"` run (a backslash escaping
//! the quote or itself), a `<…>` run with no whitespace in it, one of the
//! punctuation marks `()[]{},^<>=?`, an `@` together with what follows it, or a
//! run of anything else; `#` and `*` start a comment to the end of the line.
//! Keywords (`some`, `and`, …) are matched ignoring case. A name stands for an
//! entity of the kind its position asks for, when [`Names`] knows it as one.

use horned_owl::model::{
    AnonymousIndividual, Build, ClassExpression as CE, Component, DataProperty, DataRange,
    FacetRestriction, Individual, Literal, NamedIndividual, ObjectPropertyExpression as OPE, RcStr,
};
use horned_owl::vocab::Facet;

use crate::java_number;

use super::java;

/// What a name in a pattern's logical text stands for, by the kind of entity
/// its position asks for; none when it stands for no entity of that kind.
pub(crate) trait Names {
    fn class(&self, name: &str) -> Option<String>;
    fn object_property(&self, name: &str) -> Option<String>;
    fn data_property(&self, name: &str) -> Option<String>;
    fn individual(&self, name: &str) -> Option<String>;
    fn datatype(&self, name: &str) -> Option<String>;
}

const EOF: &str = "|EOF|";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS_LITERAL: &str = "http://www.w3.org/2000/01/rdf-schema#Literal";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

/// The text as a class expression.
pub(crate) fn class_expression(b: &Build<RcStr>, names: &dyn Names, text: &str) -> Result<CE<RcStr>, String> {
    let mut p = Parser::new(b, names, text)?;
    let ce = p.union()?;
    p.expect_end()?;
    Ok(ce)
}

/// The text as an axiom: a class axiom (`SubClassOf`, `EquivalentTo`,
/// `DisjointWith`), an object or data property axiom, a property
/// characteristic, or an individual's `Type`.
pub(crate) fn axiom(b: &Build<RcStr>, names: &dyn Names, text: &str) -> Result<Component<RcStr>, String> {
    Parser::new(b, names, text)?.axiom()
}

// ── Tokens ──────────────────────────────────────────────────────────────────

fn tokenize(text: &str) -> Result<Vec<String>, String> {
    let buf: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut sb = String::new();
    let mut pos = 0;
    let mut last = ' ';
    let consume = |sb: &mut String, tokens: &mut Vec<String>| {
        if !sb.is_empty() {
            tokens.push(std::mem::take(sb));
        }
    };
    while pos < buf.len() {
        let mut ch = buf[pos];
        pos += 1;
        if ch == '\\' {
            last = ch;
            ch = *buf.get(pos).ok_or_else(|| format!("`{text}` ends with `\\`"))?;
            pos += 1;
        }
        if (ch == '"' || ch == '\'') && last != '\\' {
            let terminator = ch;
            sb.push(terminator);
            while pos < buf.len() {
                let c = buf[pos];
                pos += 1;
                if c == '\\' {
                    if pos + 1 < buf.len() {
                        let escaped = buf[pos];
                        pos += 1;
                        if matches!(escaped, '"' | '\'' | '\\') {
                            sb.push(escaped);
                        } else {
                            sb.push(c);
                            sb.push(escaped);
                        }
                    } else {
                        sb.push('\\');
                    }
                } else if c == terminator {
                    sb.push(c);
                    break;
                } else {
                    sb.push(c);
                }
            }
            consume(&mut sb, &mut tokens);
        } else if ch == '<' {
            // What was being read before the `<` is dropped.
            sb = String::from("<");
            let start = pos;
            while pos < buf.len() {
                let c = buf[pos];
                pos += 1;
                if java::is_whitespace(c) {
                    pos = start;
                    sb = String::from("<");
                    consume(&mut sb, &mut tokens);
                    break;
                } else if c == '>' {
                    sb.push('>');
                    consume(&mut sb, &mut tokens);
                    break;
                } else {
                    sb.push(c);
                }
            }
        } else if matches!(ch, ' ' | '\n' | '\r' | '\t') {
            consume(&mut sb, &mut tokens);
        } else if matches!(ch, '#' | '*') {
            consume(&mut sb, &mut tokens);
            while pos < buf.len() {
                let c = buf[pos];
                pos += 1;
                if c == '\n' {
                    break;
                }
            }
        } else if "()[],{}^@<>=?".contains(ch) {
            consume(&mut sb, &mut tokens);
            sb.push(ch);
            if ch != '@' {
                consume(&mut sb, &mut tokens);
            }
        } else {
            sb.push(ch);
        }
        last = ch;
    }
    consume(&mut sb, &mut tokens);
    tokens.push(EOF.to_string());
    Ok(tokens)
}

/// Whether two strings are equal ignoring case, character by character.
fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count()
        && a.chars().zip(b.chars()).all(|(x, y)| {
            x == y || x.to_uppercase().eq(y.to_uppercase()) || x.to_lowercase().eq(y.to_lowercase())
        })
}

/// Every word the syntax reserves, as a token matches it.
const KEYWORDS: &[&str] = &[
    "ValuePartition:", "-", "(", ")", "{", "}", "[", "]", "Ontology:", "Import:", "Prefix:", "Class:",
    "ObjectProperty:", "->", "o", "DataProperty:", "Individual:", "Datatype:", "AnnotationProperty:", "some",
    "only", "onlysome", "min", "max", "exactly", "value", "and", "or", "not", "inverse", "inv", "Self", "that",
    ",:", "SubClassOf:", "SuperClassOf:", "EquivalentTo:", "EquivalentClasses:", "EquivalentProperties:",
    "DisjointWith:", "Individuals:", "DisjointClasses:", "DisjointProperties:", "DisjointUnionOf:", "Facts:",
    "SameAs:", "SameIndividual:", "DifferentFrom:", "DifferentIndividuals:", ">=:", "<=:", ">:", "<:",
    "Types:", "Type:", "Annotations:", ",", "Domain:", "Range:", "Characteristics:", "Functional",
    "InverseFunctional", "Symmetric", "Transitive", "Reflexive", "Irreflexive", "true", "false", "$integer$",
    "$float$", "$double$", "\"$Literal$\"", "\"$Literal$\"^^<datatype>", "\"$Literal$\"@<lang>",
    "AntiSymmetric", "Asymmetric", "InverseOf:", "Inverses:", "SubPropertyOf:", "SuperPropertyOf:",
    "SubPropertyChain:", "HasKey:", "Rule:",
];

fn is_keyword(tok: &str) -> bool {
    KEYWORDS.iter().any(|k| eq_ignore_case(k, tok))
}

/// Whether `tok` is the keyword `k`, or (`either`) `k` without its colon.
fn kw(tok: &str, k: &str) -> bool {
    eq_ignore_case(tok, k)
}

fn kw_either(tok: &str, k: &str) -> bool {
    eq_ignore_case(tok, k) || eq_ignore_case(tok, k.trim_end_matches(':'))
}

/// The datatype a built-in name names: an `xsd:`, `rdf:`, `rdfs:` or `owl:`
/// datatype's prefixed name or IRI.
fn builtin_datatype(name: &str) -> Option<String> {
    const XSD_NAMES: &[&str] = &[
        "anyType", "anySimpleType", "string", "integer", "long", "int", "short", "byte", "decimal", "float",
        "boolean", "double", "nonPositiveInteger", "negativeInteger", "nonNegativeInteger", "unsignedLong",
        "unsignedInt", "positiveInteger", "base64Binary", "normalizedString", "hexBinary", "anyURI", "QName",
        "NOTATION", "token", "language", "Name", "NCName", "NMTOKEN", "NMTOKENS", "ID", "IDREF", "IDREFS",
        "ENTITY", "ENTITIES", "duration", "dateTime", "dateTimeStamp", "time", "date", "gYearMonth", "gYear",
        "gMonthDay", "gDay", "gMonth", "unsignedShort", "unsignedByte",
    ];
    let iri = if let Some(local) = name.strip_prefix("xsd:") {
        XSD_NAMES.contains(&local).then(|| format!("{XSD}{local}"))
    } else {
        match name {
            "rdf:XMLLiteral" | "XMLLiteral" => Some(format!("{RDF}XMLLiteral")),
            "rdf:PlainLiteral" => Some(format!("{RDF}PlainLiteral")),
            "rdf:langString" => Some(format!("{RDF}langString")),
            "rdfs:Literal" => Some(RDFS_LITERAL.to_string()),
            "owl:real" => Some("http://www.w3.org/2002/07/owl#real".to_string()),
            "owl:rational" => Some("http://www.w3.org/2002/07/owl#rational".to_string()),
            _ => None,
        }
    };
    iri.or_else(|| {
        let full = name.strip_prefix('<').and_then(|n| n.strip_suffix('>')).unwrap_or(name);
        let local = full.strip_prefix(XSD)?;
        XSD_NAMES.contains(&local).then(|| full.to_string())
    })
}

// ── Parser ──────────────────────────────────────────────────────────────────

struct Parser<'a> {
    b: &'a Build<RcStr>,
    names: &'a dyn Names,
    text: &'a str,
    tokens: Vec<String>,
    index: usize,
}

type Res<T> = Result<T, String>;

impl<'a> Parser<'a> {
    fn new(b: &'a Build<RcStr>, names: &'a dyn Names, text: &'a str) -> Res<Parser<'a>> {
        Ok(Parser { b, names, text, tokens: tokenize(text)?, index: 0 })
    }

    fn peek(&self) -> &str {
        self.tokens.get(self.index).or_else(|| self.tokens.last()).map_or(EOF, String::as_str)
    }

    fn peek_ahead(&self, n: usize) -> &str {
        self.tokens.get(self.index + n).map_or(EOF, String::as_str)
    }

    fn consume(&mut self) -> String {
        let t = self.peek().to_string();
        if self.index < self.tokens.len() {
            self.index += 1;
        }
        t
    }

    fn fail<T>(&self, what: &str) -> Res<T> {
        let near = self.tokens.get(self.index.saturating_sub(1)).map_or(EOF, String::as_str);
        Err(format!("`{}`: {what} (near `{near}`)", self.text))
    }

    fn expect_end(&mut self) -> Res<()> {
        let t = self.consume();
        if t != EOF {
            return self.fail(&format!("`{t}` follows the class expression"));
        }
        Ok(())
    }

    fn is_class(&self, t: &str) -> bool {
        self.names.class(t).is_some()
    }

    fn is_object_property(&self, t: &str) -> bool {
        self.names.object_property(t).is_some()
    }

    fn is_data_property(&self, t: &str) -> bool {
        self.names.data_property(t).is_some()
    }

    fn is_individual(&self, t: &str) -> bool {
        self.names.individual(t).is_some()
    }

    fn datatype_iri(&self, t: &str) -> Option<String> {
        self.names.datatype(t).or_else(|| builtin_datatype(t))
    }

    fn class(&self, t: &str) -> Res<CE<RcStr>> {
        match self.names.class(t) {
            Some(iri) => Ok(CE::Class(self.b.class(iri))),
            None => self.fail(&format!("`{t}` names no class")),
        }
    }

    fn individual_named(&self, t: &str) -> Individual<RcStr> {
        if t.starts_with("_:") {
            return Individual::Anonymous(AnonymousIndividual(t.to_string().into()));
        }
        let iri = self.names.individual(t).unwrap_or_default();
        Individual::Named(NamedIndividual(self.b.iri(iri)))
    }

    fn individual(&mut self) -> Res<Individual<RcStr>> {
        let t = self.consume();
        if !self.is_individual(&t) && !t.starts_with("_:") {
            return self.fail(&format!("`{t}` names no individual"));
        }
        Ok(self.individual_named(&t))
    }

    fn data_property(&mut self) -> Res<DataProperty<RcStr>> {
        let t = self.consume();
        match self.names.data_property(&t) {
            Some(iri) => Ok(self.b.data_property(iri)),
            None => self.fail(&format!("`{t}` names no data property")),
        }
    }

    fn object_property_expression(&mut self) -> Res<OPE<RcStr>> {
        let t = self.consume();
        if kw(&t, "inverse") {
            let brackets = kw(self.peek(), "(");
            if brackets {
                self.consume();
            }
            let inner = self.object_property_expression()?;
            if brackets && !kw(&self.consume(), ")") {
                return self.fail("an unclosed `inverse (`");
            }
            return match inner {
                OPE::ObjectProperty(p) => Ok(OPE::InverseObjectProperty(p)),
                OPE::InverseObjectProperty(p) => Ok(OPE::ObjectProperty(p)),
            };
        }
        match self.names.object_property(&t) {
            Some(iri) => Ok(OPE::ObjectProperty(self.b.object_property(iri))),
            None => self.fail(&format!("`{t}` names no object property")),
        }
    }

    fn union(&mut self) -> Res<CE<RcStr>> {
        let mut ops = Vec::new();
        loop {
            push_unique(&mut ops, self.intersection()?);
            if kw(self.peek(), "or") {
                self.consume();
            } else {
                break;
            }
        }
        Ok(if ops.len() == 1 { ops.pop().unwrap() } else { CE::ObjectUnionOf(ops) })
    }

    fn intersection(&mut self) -> Res<CE<RcStr>> {
        let mut ops = Vec::new();
        loop {
            push_unique(&mut ops, self.non_nary()?);
            let t = self.peek();
            if kw(t, "and") || kw(t, "that") {
                self.consume();
            } else {
                break;
            }
        }
        Ok(if ops.len() == 1 { ops.pop().unwrap() } else { CE::ObjectIntersectionOf(ops) })
    }

    fn non_nary(&mut self) -> Res<CE<RcStr>> {
        let t = self.peek().to_string();
        let is_class = self.is_class(&t);
        let is_data = self.is_data_property(&t);
        let is_object = self.is_object_property(&t);
        if is_class && (is_data || is_object) {
            // A name both a class and a property is a property when a
            // restriction keyword follows it.
            let ahead = self.peek_ahead(1);
            if ["some", "only", "value", "min", "max", "exactly", "onlysome", "Self"].iter().any(|k| kw(ahead, k)) {
                return if is_object { self.object_restriction() } else { self.data_restriction() };
            }
            self.consume();
            return self.class(&t);
        }
        if kw(&t, "not") {
            self.consume();
            return Ok(CE::ObjectComplementOf(Box::new(self.nested(false)?)));
        }
        if is_class {
            self.consume();
            return self.class(&t);
        }
        if is_object || kw(&t, "inverse") {
            return self.object_restriction();
        }
        if is_data {
            return self.data_restriction();
        }
        if kw(&t, "{") {
            return self.object_one_of();
        }
        if kw(&t, "(") {
            return self.nested(false);
        }
        self.consume();
        self.fail(&format!("`{t}` is no class, property or class expression"))
    }

    fn cardinality(&mut self) -> Res<u32> {
        let t = self.consume();
        match java_number::parse_int(&t) {
            Some(n) if n >= 0 => Ok(n as u32),
            Some(_) => self.fail("a negative cardinality"),
            None => self.fail(&format!("`{t}` is no cardinality")),
        }
    }

    fn object_restriction(&mut self) -> Res<CE<RcStr>> {
        let ope = self.object_property_expression()?;
        let k = self.consume();
        if kw(&k, "some") {
            if kw(self.peek(), "Self") {
                self.consume();
                return Ok(CE::ObjectHasSelf(ope));
            }
            let bce = Box::new(self.nested(false)?);
            return Ok(CE::ObjectSomeValuesFrom { ope, bce });
        }
        if kw(&k, "only") {
            let bce = Box::new(self.nested(false)?);
            return Ok(CE::ObjectAllValuesFrom { ope, bce });
        }
        if kw(&k, "value") {
            let t = self.consume();
            if !self.is_individual(&t) {
                return self.fail(&format!("`{t}` names no individual"));
            }
            return Ok(CE::ObjectHasValue { ope, i: self.individual_named(&t) });
        }
        if kw(&k, "min") || kw(&k, "max") || kw(&k, "exactly") {
            let n = self.cardinality()?;
            let bce = Box::new(self.nested(true)?);
            return Ok(if kw(&k, "min") {
                CE::ObjectMinCardinality { n, ope, bce }
            } else if kw(&k, "max") {
                CE::ObjectMaxCardinality { n, ope, bce }
            } else {
                CE::ObjectExactCardinality { n, ope, bce }
            });
        }
        if kw(&k, "onlysome") {
            let mut descs = Vec::new();
            if kw(self.peek(), "[") {
                self.consume();
                loop {
                    push_unique(&mut descs, self.union()?);
                    if kw(self.peek(), ",") {
                        self.consume();
                    } else {
                        break;
                    }
                }
                if !kw(&self.consume(), "]") {
                    return self.fail("an unclosed `onlysome [`");
                }
            } else {
                descs.push(self.union()?);
            }
            let mut ops: Vec<CE<RcStr>> = Vec::new();
            for d in &descs {
                push_unique(&mut ops, CE::ObjectSomeValuesFrom { ope: ope.clone(), bce: Box::new(d.clone()) });
            }
            let filler = if descs.len() == 1 { descs.pop().unwrap() } else { CE::ObjectUnionOf(descs) };
            push_unique(&mut ops, CE::ObjectAllValuesFrom { ope, bce: Box::new(filler) });
            return Ok(CE::ObjectIntersectionOf(ops));
        }
        if kw(&k, "Self") {
            return Ok(CE::ObjectHasSelf(ope));
        }
        self.fail(&format!("`{k}` where some, only, value, min, max, exactly or Self belongs"))
    }

    fn data_restriction(&mut self) -> Res<CE<RcStr>> {
        let dp = self.data_property()?;
        let k = self.consume();
        if kw(&k, "some") {
            return Ok(CE::DataSomeValuesFrom { dp, dr: self.data_intersection(false)? });
        }
        if kw(&k, "only") {
            return Ok(CE::DataAllValuesFrom { dp, dr: self.data_intersection(false)? });
        }
        if kw(&k, "value") {
            return Ok(CE::DataHasValue { dp, l: self.literal(None)? });
        }
        if kw(&k, "min") || kw(&k, "max") || kw(&k, "exactly") {
            let n = self.cardinality()?;
            let dr = self.data_intersection(true)?;
            return Ok(if kw(&k, "min") {
                CE::DataMinCardinality { n, dp, dr }
            } else if kw(&k, "max") {
                CE::DataMaxCardinality { n, dp, dr }
            } else {
                CE::DataExactCardinality { n, dp, dr }
            });
        }
        self.fail(&format!("`{k}` where some, only, value, min, max or exactly belongs"))
    }

    fn nested(&mut self, lookahead: bool) -> Res<CE<RcStr>> {
        let t = self.peek().to_string();
        if kw(&t, "(") {
            self.consume();
            let ce = self.union()?;
            if !kw(&self.consume(), ")") {
                return self.fail("an unclosed `(`");
            }
            return Ok(ce);
        }
        if kw(&t, "{") {
            return self.object_one_of();
        }
        if self.is_class(&t) {
            self.consume();
            return self.class(&t);
        }
        // A missing filler is `owl:Thing`.
        if is_keyword(&t) || t == EOF {
            return Ok(CE::Class(self.b.class(OWL_THING)));
        }
        let _ = lookahead;
        self.consume();
        self.fail(&format!("`{t}` is no class or class expression"))
    }

    fn object_one_of(&mut self) -> Res<CE<RcStr>> {
        if !kw(&self.consume(), "{") {
            return self.fail("`{` expected");
        }
        let mut inds = Vec::new();
        loop {
            push_unique(&mut inds, self.individual()?);
            if kw(self.peek(), ",") {
                self.consume();
            } else {
                break;
            }
        }
        if !kw(&self.consume(), "}") {
            return self.fail("an unclosed `{`");
        }
        Ok(CE::ObjectOneOf(inds))
    }

    fn data_intersection(&mut self, lookahead: bool) -> Res<DataRange<RcStr>> {
        let mut ranges = Vec::new();
        loop {
            push_unique(&mut ranges, self.data_union(lookahead)?);
            if kw(self.peek(), "and") {
                self.consume();
            } else {
                break;
            }
        }
        Ok(if ranges.len() == 1 { ranges.pop().unwrap() } else { DataRange::DataIntersectionOf(ranges) })
    }

    fn data_union(&mut self, lookahead: bool) -> Res<DataRange<RcStr>> {
        let mut ranges = Vec::new();
        loop {
            push_unique(&mut ranges, self.data_primary(lookahead)?);
            if kw(self.peek(), "or") {
                self.consume();
            } else {
                break;
            }
        }
        Ok(if ranges.len() == 1 { ranges.pop().unwrap() } else { DataRange::DataUnionOf(ranges) })
    }

    fn data_primary(&mut self, lookahead: bool) -> Res<DataRange<RcStr>> {
        let t = self.peek().to_string();
        if let Some(iri) = self.datatype_iri(&t) {
            self.consume();
            let dt = self.b.datatype(iri.clone());
            if !kw(self.peek(), "[") {
                return Ok(DataRange::Datatype(dt));
            }
            self.consume();
            let mut facets = Vec::new();
            loop {
                let f = self.facet()?;
                let l = self.literal(Some(&iri))?;
                push_unique(&mut facets, FacetRestriction { f, l });
                let sep = self.consume();
                if kw(&sep, ",") {
                    continue;
                }
                if !kw(&sep, "]") {
                    return self.fail("an unclosed `[`");
                }
                break;
            }
            return Ok(DataRange::DatatypeRestriction(dt, facets));
        }
        if kw(&t, "not") {
            self.consume();
            return Ok(DataRange::DataComplementOf(Box::new(self.data_primary(false)?)));
        }
        if kw(&t, "{") {
            self.consume();
            let mut lits = Vec::new();
            loop {
                push_unique(&mut lits, self.literal(None)?);
                let sep = self.consume();
                if kw(&sep, ",") {
                    continue;
                }
                if !kw(&sep, "}") {
                    return self.fail("an unclosed `{`");
                }
                break;
            }
            return Ok(DataRange::DataOneOf(lits));
        }
        if kw(&t, "(") {
            self.consume();
            let dr = self.data_intersection(false)?;
            if !kw(&self.consume(), ")") {
                return self.fail("an unclosed `(`");
            }
            return Ok(dr);
        }
        // A missing data range is `rdfs:Literal`.
        if is_keyword(&t) || (t == EOF && lookahead) {
            return Ok(DataRange::Datatype(self.b.datatype(RDFS_LITERAL)));
        }
        self.consume();
        self.fail(&format!("`{t}` is no datatype or data range"))
    }

    fn facet(&mut self) -> Res<Facet> {
        let f = self.consume();
        let next = self.peek().to_string();
        if (f == ">" || f == "<") && next == "=" {
            self.consume();
            return Ok(if f == ">" { Facet::MinInclusive } else { Facet::MaxInclusive });
        }
        Ok(match f.as_str() {
            ">" => Facet::MinExclusive,
            "<" => Facet::MaxExclusive,
            ">=" => Facet::MinInclusive,
            "<=" => Facet::MaxInclusive,
            "length" => Facet::Length,
            "minLength" => Facet::MinLength,
            "maxLength" => Facet::MaxLength,
            "pattern" => Facet::Pattern,
            "totalDigits" => Facet::TotalDigits,
            "fractionDigits" => Facet::FractionDigits,
            "langRange" => Facet::LangRange,
            _ => return self.fail(&format!("`{f}` is no facet")),
        })
    }

    /// A literal; `datatype` types an unquoted one.
    fn literal(&mut self, datatype: Option<&str>) -> Res<Literal<RcStr>> {
        let t = self.consume();
        if t.starts_with('"') {
            if !t.ends_with('"') {
                self.consume();
                return self.fail("an unclosed `\"`");
            }
            let lex = if t.chars().count() > 2 { t[1..t.len() - 1].to_string() } else { String::new() };
            if self.peek() == "^" {
                self.consume();
                if self.peek() != "^" {
                    return self.fail("`^^` expected");
                }
                self.consume();
                let dt = self.consume();
                let iri = self.datatype_iri(&dt).ok_or_else(|| format!("`{}`: `{dt}` names no datatype", self.text))?;
                return Ok(typed_literal(self.b, &lex, &iri));
            }
            if self.peek().starts_with('@') {
                let lang = self.consume()[1..].to_string();
                return Ok(crate::model::literal_as_made(Literal::Language { literal: lex, lang }));
            }
            return Ok(Literal::Simple { literal: lex });
        }
        if let Some(iri) = datatype {
            return Ok(typed_literal(self.b, &t, iri));
        }
        if let Some(i) = java_number::parse_int(&t) {
            return Ok(datatype_literal(self.b, &i.to_string(), &format!("{XSD}integer")));
        }
        if t.ends_with(['f', 'F']) {
            if let Some(f) = java_number::parse_float(&t.replace("INF", "Infinity").replace("inf", "Infinity")) {
                let lex = java_number::float_to_string(f).replace("Infinity", "INF");
                return Ok(typed_literal(self.b, &lex, &format!("{XSD}float")));
            }
        }
        if java_number::is_double(&t) {
            return Ok(datatype_literal(self.b, &t, &format!("{XSD}decimal")));
        }
        if kw(&t, "true") || kw(&t, "false") {
            return Ok(datatype_literal(self.b, if kw(&t, "true") { "true" } else { "false" }, &format!("{XSD}boolean")));
        }
        self.fail(&format!("`{t}` is no literal"))
    }

    // ── Axioms ──────────────────────────────────────────────────────────────

    fn axiom(&mut self) -> Res<Component<RcStr>> {
        use horned_owl::model::*;
        let t = self.peek().to_string();
        if self.is_class(&t) || kw(&t, "(") || kw(&t, "{") {
            let start = self.union()?;
            return self.class_axiom(start);
        }
        if self.is_object_property(&t) || kw(&t, "inv") {
            return self.object_property_axiom();
        }
        if self.is_data_property(&t) {
            return self.data_property_axiom();
        }
        if self.is_individual(&t) {
            let i = self.individual()?;
            let k = self.consume();
            if !kw(&k, "Type:") {
                return self.fail("`Type` expected");
            }
            let ce = self.union()?;
            self.expect_end()?;
            return Ok(Component::ClassAssertion(ClassAssertion { ce, i }));
        }
        let characteristic = |p: &mut Parser, which: &str| -> Res<Component<RcStr>> {
            p.consume();
            let name = p.peek().to_string();
            if which == "Functional" && !p.is_object_property(&name) && p.is_data_property(&name) {
                let dp = p.data_property()?;
                return Ok(Component::FunctionalDataProperty(FunctionalDataProperty(dp)));
            }
            if !p.is_object_property(&name) && !kw(&name, "inverse") {
                return p.fail(&format!("`{name}` names no object property"));
            }
            let ope = p.object_property_expression()?;
            Ok(match which {
                "Functional" => Component::FunctionalObjectProperty(FunctionalObjectProperty(ope)),
                "InverseFunctional" => Component::InverseFunctionalObjectProperty(InverseFunctionalObjectProperty(ope)),
                "Symmetric" => Component::SymmetricObjectProperty(SymmetricObjectProperty(ope)),
                "Asymmetric" => Component::AsymmetricObjectProperty(AsymmetricObjectProperty(ope)),
                "Transitive" => Component::TransitiveObjectProperty(TransitiveObjectProperty(ope)),
                "Reflexive" => Component::ReflexiveObjectProperty(ReflexiveObjectProperty(ope)),
                _ => Component::IrreflexiveObjectProperty(IrreflexiveObjectProperty(ope)),
            })
        };
        for which in ["Functional", "InverseFunctional", "Symmetric", "Asymmetric", "Transitive", "Reflexive", "Irreflexive"] {
            if kw(&t, which) {
                return characteristic(self, which);
            }
        }
        self.fail(&format!("`{t}` starts no axiom"))
    }

    fn class_axiom(&mut self, start: CE<RcStr>) -> Res<Component<RcStr>> {
        use horned_owl::model::*;
        let k = self.consume();
        let rhs = |p: &mut Parser| -> Res<CE<RcStr>> {
            let ce = p.union()?;
            p.expect_end()?;
            Ok(ce)
        };
        if kw_either(&k, "SubClassOf:") {
            let sup = rhs(self)?;
            return Ok(Component::SubClassOf(SubClassOf { sub: start, sup }));
        }
        if kw_either(&k, "DisjointWith:") {
            let other = rhs(self)?;
            return Ok(Component::DisjointClasses(DisjointClasses(dedup(vec![start, other]))));
        }
        if kw_either(&k, "EquivalentTo:") {
            let other = rhs(self)?;
            return Ok(Component::EquivalentClasses(EquivalentClasses(dedup(vec![start, other]))));
        }
        if kw(&k, "and") {
            let more = self.intersection()?;
            let mut ops = conjuncts(more);
            push_unique(&mut ops, start);
            return self.class_axiom(CE::ObjectIntersectionOf(ops));
        }
        if kw(&k, "or") {
            let more = self.union()?;
            let mut ops = disjuncts(more);
            push_unique(&mut ops, start);
            return self.class_axiom(CE::ObjectUnionOf(ops));
        }
        self.fail(&format!("`{k}` where SubClassOf, EquivalentTo, DisjointWith, and or or belongs"))
    }

    fn object_property_axiom(&mut self) -> Res<Component<RcStr>> {
        use horned_owl::model::*;
        let ope = self.object_property_expression()?;
        let k = self.consume();
        for (word, card) in [("some", 0), ("only", 1), ("min", 2), ("max", 3), ("exactly", 4)] {
            if !kw(&k, word) {
                continue;
            }
            let n = if card >= 2 { self.cardinality()? } else { 0 };
            let bce = Box::new(self.union()?);
            let ce = match card {
                0 => CE::ObjectSomeValuesFrom { ope, bce },
                1 => CE::ObjectAllValuesFrom { ope, bce },
                2 => CE::ObjectMinCardinality { n, ope, bce },
                3 => CE::ObjectMaxCardinality { n, ope, bce },
                _ => CE::ObjectExactCardinality { n, ope, bce },
            };
            return self.class_axiom(ce);
        }
        let other = |p: &mut Parser| p.object_property_expression();
        if kw(&k, "SubPropertyOf:") {
            let sup = other(self)?;
            return Ok(Component::SubObjectPropertyOf(SubObjectPropertyOf { sub: SubObjectPropertyExpression::ObjectPropertyExpression(ope), sup }));
        }
        if kw(&k, "EquivalentTo:") {
            let o = other(self)?;
            return Ok(Component::EquivalentObjectProperties(EquivalentObjectProperties(dedup(vec![ope, o]))));
        }
        if kw(&k, "InverseOf:") {
            let o = other(self)?;
            return Ok(Component::InverseObjectProperties(InverseObjectProperties(ope, o)));
        }
        if kw(&k, "DisjointWith:") {
            let o = other(self)?;
            return Ok(Component::DisjointObjectProperties(DisjointObjectProperties(dedup(vec![ope, o]))));
        }
        if kw(&k, "Domain:") {
            let ce = self.union()?;
            self.expect_end()?;
            return Ok(Component::ObjectPropertyDomain(ObjectPropertyDomain { ope, ce }));
        }
        if kw(&k, "Range:") {
            let ce = self.union()?;
            self.expect_end()?;
            return Ok(Component::ObjectPropertyRange(ObjectPropertyRange { ope, ce }));
        }
        if k == "o" {
            let mut chain = vec![ope];
            let mut sep = k;
            while sep == "o" {
                chain.push(self.object_property_expression()?);
                sep = self.consume();
            }
            if !kw(&sep, "SubPropertyOf:") {
                return self.fail("`SubPropertyOf` expected");
            }
            let sup = self.object_property_expression()?;
            return Ok(Component::SubObjectPropertyOf(SubObjectPropertyOf { sub: SubObjectPropertyExpression::ObjectPropertyChain(chain), sup }));
        }
        self.fail(&format!("`{k}` starts no object property axiom"))
    }

    fn data_property_axiom(&mut self) -> Res<Component<RcStr>> {
        use horned_owl::model::*;
        let dp = self.data_property()?;
        let k = self.consume();
        if kw(&k, "some") || kw(&k, "only") {
            let dr = self.data_intersection(false)?;
            let ce = if kw(&k, "some") { CE::DataSomeValuesFrom { dp, dr } } else { CE::DataAllValuesFrom { dp, dr } };
            return self.class_axiom(ce);
        }
        if kw(&k, "min") || kw(&k, "max") || kw(&k, "exactly") {
            let n = self.cardinality()?;
            let dr = self.data_intersection(true)?;
            let ce = if kw(&k, "min") {
                CE::DataMinCardinality { n, dp, dr }
            } else if kw(&k, "max") {
                CE::DataMaxCardinality { n, dp, dr }
            } else {
                CE::DataExactCardinality { n, dp, dr }
            };
            return self.class_axiom(ce);
        }
        let other = |p: &mut Parser| -> Res<DataProperty<RcStr>> {
            let t = p.consume();
            match p.names.data_property(&t) {
                Some(iri) => Ok(p.b.data_property(iri)),
                None => p.fail(&format!("`{t}` names no data property")),
            }
        };
        if kw(&k, "SubPropertyOf:") {
            let sup = other(self)?;
            return Ok(Component::SubDataPropertyOf(SubDataPropertyOf { sub: dp, sup }));
        }
        if kw(&k, "EquivalentTo:") {
            let o = other(self)?;
            return Ok(Component::EquivalentDataProperties(EquivalentDataProperties(dedup(vec![dp, o]))));
        }
        if kw(&k, "DisjointWith:") {
            let o = other(self)?;
            return Ok(Component::DisjointDataProperties(DisjointDataProperties(dedup(vec![dp, o]))));
        }
        if kw(&k, "Domain:") {
            let ce = self.union()?;
            self.expect_end()?;
            return Ok(Component::DataPropertyDomain(DataPropertyDomain { dp, ce }));
        }
        if kw(&k, "Range:") {
            let dr = self.data_intersection(true)?;
            return Ok(Component::DataPropertyRange(DataPropertyRange { dp, dr }));
        }
        self.fail(&format!("`{k}` starts no data property axiom"))
    }
}

/// The literal `lex` typed `datatype`, as it is made
/// ([`crate::model::literal_as_made`]).
fn typed_literal(b: &Build<RcStr>, lex: &str, datatype: &str) -> Literal<RcStr> {
    crate::model::literal_as_made(datatype_literal(b, lex, datatype))
}

fn datatype_literal(b: &Build<RcStr>, lex: &str, datatype: &str) -> Literal<RcStr> {
    Literal::Datatype { literal: lex.to_string(), datatype_iri: b.iri(datatype.to_string()) }
}

fn push_unique<T: PartialEq>(v: &mut Vec<T>, x: T) {
    if !v.contains(&x) {
        v.push(x);
    }
}

fn dedup<T: PartialEq>(v: Vec<T>) -> Vec<T> {
    let mut out = Vec::new();
    for x in v {
        push_unique(&mut out, x);
    }
    out
}

fn conjuncts(ce: CE<RcStr>) -> Vec<CE<RcStr>> {
    match ce {
        CE::ObjectIntersectionOf(v) => v.into_iter().flat_map(conjuncts).collect(),
        other => vec![other],
    }
}

fn disjuncts(ce: CE<RcStr>) -> Vec<CE<RcStr>> {
    match ce {
        CE::ObjectUnionOf(v) => v.into_iter().flat_map(disjuncts).collect(),
        other => vec![other],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_as_the_syntax_splits_them() {
        let t = tokenize("'part of' some (X and <http://a/b>) # note\nxsd:int[>= 5]").unwrap();
        assert_eq!(
            t,
            ["'part of'", "some", "(", "X", "and", "<http://a/b>", ")", "xsd:int", "[", ">", "=", "5", "]", EOF]
        );
        assert_eq!(tokenize("\"a \\\"b\\\"\"@EN").unwrap(), ["\"a \"b\"\"", "@EN", EOF]);
        assert_eq!(tokenize("a < b").unwrap(), ["a", "<", "b", EOF]);
    }
}
