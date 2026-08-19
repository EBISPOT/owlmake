//! OWL 2 Manchester Syntax support (the `.omn` format).
//!
//! Covers the frame-based structure used in practice: `Prefix:`, `Ontology:`,
//! `Class:` frames (Annotations / SubClassOf / EquivalentTo / DisjointWith) and
//! `ObjectProperty:` frames, with class expressions built from `and`, `or`,
//! `some`, `only`, `value`, `not`, and parentheses.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use anyhow::Result;
use horned_owl::model::{
    AnnotationAssertion, AnnotationSubject, AnnotationValue, Build, ClassExpression as CE, Component,
    DeclareClass, DeclareObjectProperty, DisjointClasses, EquivalentClasses, Individual, Literal,
    MutableOntology, ObjectPropertyExpression as OPE, RcStr, SubClassOf,
};
use horned_owl::ontology::set::SetOntology;

use crate::model::{clone_prefixes, default_prefixes, Model};

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

// === Writer ==============================================================

/// Write an ontology in Manchester Syntax.
pub fn save<W: Write>(model: &Model, w: &mut W) -> Result<()> {
    // Group axioms by subject class.
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut supers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut equivs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut classes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut props: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for ac in model.ont.iter() {
        match &ac.component {
            Component::DeclareClass(dc) => {
                classes.insert(dc.0 .0.as_ref().to_string());
            }
            Component::DeclareObjectProperty(dp) => {
                props.insert(dp.0 .0.as_ref().to_string());
            }
            Component::AnnotationAssertion(aa) => {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                        labels.insert(s.as_ref().to_string(), literal_text(lit));
                    }
                }
            }
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    classes.insert(sub.0.as_ref().to_string());
                    supers
                        .entry(sub.0.as_ref().to_string())
                        .or_default()
                        .push(render_ce(&sc.sup));
                }
            }
            Component::EquivalentClasses(eq) => {
                if let Some(CE::Class(c)) = eq.0.iter().find(|c| matches!(c, CE::Class(_))) {
                    let key = c.0.as_ref().to_string();
                    for m in &eq.0 {
                        if !matches!(m, CE::Class(cc) if cc.0.as_ref() == key) {
                            equivs.entry(key.clone()).or_default().push(render_ce(m));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    writeln!(w, "Prefix: owl: <http://www.w3.org/2002/07/owl#>")?;
    writeln!(w, "Prefix: rdfs: <http://www.w3.org/2000/01/rdf-schema#>")?;
    writeln!(w, "Ontology:")?;
    writeln!(w)?;
    for p in &props {
        writeln!(w, "ObjectProperty: <{p}>")?;
        if let Some(l) = labels.get(p) {
            writeln!(w, "    Annotations: rdfs:label \"{}\"", escape(l))?;
        }
        writeln!(w)?;
    }
    for c in &classes {
        writeln!(w, "Class: <{c}>")?;
        if let Some(l) = labels.get(c) {
            writeln!(w, "    Annotations: rdfs:label \"{}\"", escape(l))?;
        }
        if let Some(es) = equivs.get(c) {
            writeln!(w, "    EquivalentTo: {}", es.join(",\n        "))?;
        }
        if let Some(ss) = supers.get(c) {
            writeln!(w, "    SubClassOf: {}", ss.join(",\n        "))?;
        }
        writeln!(w)?;
    }
    Ok(())
}

fn render_ce(ce: &CE<RcStr>) -> String {
    match ce {
        CE::Class(c) => format!("<{}>", c.0.as_ref()),
        CE::ObjectIntersectionOf(parts) => parts
            .iter()
            .map(render_ce_paren)
            .collect::<Vec<_>>()
            .join(" and "),
        CE::ObjectUnionOf(parts) => parts
            .iter()
            .map(render_ce_paren)
            .collect::<Vec<_>>()
            .join(" or "),
        CE::ObjectComplementOf(inner) => format!("not {}", render_ce_paren(inner)),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            format!("{} some {}", render_ope(ope), render_ce_paren(bce))
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            format!("{} only {}", render_ope(ope), render_ce_paren(bce))
        }
        CE::ObjectHasValue { ope, i } => format!("{} value {}", render_ope(ope), render_ind(i)),
        _ => "owl:Thing".to_string(), // unsupported expression rendered as Thing
    }
}

fn render_ce_paren(ce: &CE<RcStr>) -> String {
    match ce {
        CE::Class(_) => render_ce(ce),
        _ => format!("({})", render_ce(ce)),
    }
}

fn render_ope(ope: &OPE<RcStr>) -> String {
    match ope {
        OPE::ObjectProperty(p) => format!("<{}>", p.0.as_ref()),
        OPE::InverseObjectProperty(p) => format!("inverse <{}>", p.0.as_ref()),
    }
}

fn render_ind(i: &Individual<RcStr>) -> String {
    match i {
        Individual::Named(n) => format!("<{}>", n.0.as_ref()),
        Individual::Anonymous(a) => format!("_:{}", a.0.as_ref()),
    }
}

fn literal_text(lit: &Literal<RcStr>) -> String {
    match lit {
        Literal::Simple { literal }
        | Literal::Language { literal, .. }
        | Literal::Datatype { literal, .. } => literal.clone(),
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// === Reader ==============================================================

/// Load an ontology from Manchester Syntax.
pub fn load<R: BufRead>(mut reader: R) -> Result<Model> {
    let mut text = String::new();
    reader.read_to_string(&mut text)?;
    let b = Build::new();
    let mut ont: SetOntology<RcStr> = SetOntology::new();
    let mut prefixes = default_prefixes();

    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    // Header: Prefix: declarations.
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("Prefix:") {
            if let Some((p, iri)) = rest.trim().split_once(char::is_whitespace) {
                let p = p.trim().trim_end_matches(':');
                let iri = iri.trim().trim_start_matches('<').trim_end_matches('>');
                let _ = prefixes.add_prefix(p, iri);
            }
            i += 1;
        } else if line.starts_with("Ontology:") || line.is_empty() {
            i += 1;
        } else {
            break;
        }
    }

    // Frames.
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() {
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("Class:") {
            let subj = resolve(&prefixes, rest.trim());
            ont.insert(Component::DeclareClass(DeclareClass(b.class(subj.clone()))));
            i += 1;
            i = parse_class_frame(&b, &mut ont, &prefixes, &lines, i, &subj)?;
        } else if let Some(rest) = line.strip_prefix("ObjectProperty:") {
            let subj = resolve(&prefixes, rest.trim());
            ont.insert(Component::DeclareObjectProperty(DeclareObjectProperty(
                b.object_property(subj),
            )));
            i += 1;
            i = skip_frame(&lines, i);
        } else {
            i += 1;
        }
    }

    Ok(Model::from_parts(ont, clone_prefixes(&prefixes)))
}

/// Parse the sub-clauses of a Class frame until the next frame keyword.
fn parse_class_frame(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    prefixes: &horned_owl::curie::PrefixMapping,
    lines: &[&str],
    mut i: usize,
    subj: &str,
) -> Result<usize> {
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();
        if is_frame_start(line) {
            break;
        }
        if line.is_empty() {
            i += 1;
            continue;
        }
        // A clause may be "Keyword: expr[, expr...]" possibly spanning lines.
        let (keyword, mut body) = match line.split_once(':') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => {
                i += 1;
                continue;
            }
        };
        i += 1;
        // Gather continuation lines (indented, not a new clause/frame).
        while i < lines.len() {
            let next = lines[i];
            let nt = next.trim();
            if nt.is_empty() || is_frame_start(nt) || looks_like_clause(nt) {
                break;
            }
            body.push(' ');
            body.push_str(nt);
            i += 1;
        }

        match keyword.as_str() {
            "SubClassOf" => {
                for expr in split_top_commas(&body) {
                    if let Some(ce) = parse_ce(b, prefixes, &expr) {
                        ont.insert(Component::SubClassOf(SubClassOf {
                            sub: CE::Class(b.class(subj)),
                            sup: ce,
                        }));
                    }
                }
            }
            "EquivalentTo" => {
                for expr in split_top_commas(&body) {
                    if let Some(ce) = parse_ce(b, prefixes, &expr) {
                        ont.insert(Component::EquivalentClasses(EquivalentClasses(vec![
                            CE::Class(b.class(subj)),
                            ce,
                        ])));
                    }
                }
            }
            "DisjointWith" => {
                for expr in split_top_commas(&body) {
                    if let Some(ce) = parse_ce(b, prefixes, &expr) {
                        ont.insert(Component::DisjointClasses(DisjointClasses(vec![
                            CE::Class(b.class(subj)),
                            ce,
                        ])));
                    }
                }
            }
            "Annotations" => {
                for expr in split_top_commas(&body) {
                    if let Some((prop, val)) = parse_annotation(prefixes, &expr) {
                        ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
                            subject: AnnotationSubject::IRI(b.iri(subj)),
                            ann: horned_owl::model::Annotation { ann: Default::default(),
                                ap: b.annotation_property(prop.as_str()),
                                av: AnnotationValue::Literal(Literal::Simple { literal: val }),
                            },
                        }));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(i)
}

fn skip_frame(lines: &[&str], mut i: usize) -> usize {
    while i < lines.len() {
        let line = lines[i].trim();
        if is_frame_start(line) {
            break;
        }
        i += 1;
    }
    i
}

fn is_frame_start(line: &str) -> bool {
    ["Class:", "ObjectProperty:", "DataProperty:", "Individual:", "Datatype:", "AnnotationProperty:", "Ontology:", "Prefix:"]
        .iter()
        .any(|k| line.starts_with(k))
}

fn looks_like_clause(line: &str) -> bool {
    ["SubClassOf:", "EquivalentTo:", "DisjointWith:", "Annotations:", "Types:", "Facts:", "SubPropertyOf:", "Domain:", "Range:", "Characteristics:"]
        .iter()
        .any(|k| line.starts_with(k))
}

/// Split on commas that are not inside parentheses.
fn split_top_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn parse_annotation(
    prefixes: &horned_owl::curie::PrefixMapping,
    expr: &str,
) -> Option<(String, String)> {
    let (prop, rest) = expr.split_once(char::is_whitespace)?;
    let prop = resolve(prefixes, prop.trim());
    let val = rest.trim();
    let val = val.strip_prefix('"').and_then(|v| {
        let end = v.find('"')?;
        Some(v[..end].to_string())
    })?;
    Some((prop, val))
}

// --- Class-expression parser (recursive descent) ------------------------

fn parse_ce(b: &Build<RcStr>, prefixes: &horned_owl::curie::PrefixMapping, s: &str) -> Option<CE<RcStr>> {
    let toks = tokenize(s);
    let mut p = Parser { toks, pos: 0, b, prefixes };
    let ce = p.parse_or()?;
    Some(ce)
}

/// Parse a Manchester class expression from a string (public entry point,
/// reused by the DOSDP pattern engine).
pub fn parse_class_expression(
    b: &Build<RcStr>,
    prefixes: &horned_owl::curie::PrefixMapping,
    s: &str,
) -> Option<CE<RcStr>> {
    parse_ce(b, prefixes, s)
}

struct Parser<'a> {
    toks: Vec<String>,
    pos: usize,
    b: &'a Build<RcStr>,
    prefixes: &'a horned_owl::curie::PrefixMapping,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&str> {
        self.toks.get(self.pos).map(|s| s.as_str())
    }
    fn next(&mut self) -> Option<String> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn parse_or(&mut self) -> Option<CE<RcStr>> {
        let mut parts = vec![self.parse_and()?];
        while self.peek() == Some("or") {
            self.next();
            parts.push(self.parse_and()?);
        }
        if parts.len() == 1 {
            Some(parts.pop().unwrap())
        } else {
            Some(CE::ObjectUnionOf(parts))
        }
    }
    fn parse_and(&mut self) -> Option<CE<RcStr>> {
        let mut parts = vec![self.parse_primary()?];
        while self.peek() == Some("and") {
            self.next();
            parts.push(self.parse_primary()?);
        }
        if parts.len() == 1 {
            Some(parts.pop().unwrap())
        } else {
            Some(CE::ObjectIntersectionOf(parts))
        }
    }
    fn parse_primary(&mut self) -> Option<CE<RcStr>> {
        let t = self.next()?;
        match t.as_str() {
            "(" => {
                let inner = self.parse_or()?;
                if self.peek() == Some(")") {
                    self.next();
                }
                Some(inner)
            }
            "not" => Some(CE::ObjectComplementOf(Box::new(self.parse_primary()?))),
            _ => {
                // `t` is a property or class name. Look ahead for some/only/value.
                match self.peek() {
                    Some("some") => {
                        self.next();
                        let filler = self.parse_primary()?;
                        Some(CE::ObjectSomeValuesFrom {
                            ope: OPE::ObjectProperty(self.b.object_property(resolve(self.prefixes, &t))),
                            bce: Box::new(filler),
                        })
                    }
                    Some("only") => {
                        self.next();
                        let filler = self.parse_primary()?;
                        Some(CE::ObjectAllValuesFrom {
                            ope: OPE::ObjectProperty(self.b.object_property(resolve(self.prefixes, &t))),
                            bce: Box::new(filler),
                        })
                    }
                    Some("value") => {
                        self.next();
                        let ind = self.next()?;
                        Some(CE::ObjectHasValue {
                            ope: OPE::ObjectProperty(self.b.object_property(resolve(self.prefixes, &t))),
                            i: Individual::Named(
                                self.b.named_individual(resolve(self.prefixes, &ind)),
                            ),
                        })
                    }
                    _ => Some(CE::Class(self.b.class(resolve(self.prefixes, &t)))),
                }
            }
        }
    }
}

/// Tokenize a Manchester class expression: parentheses, `<iri>`, and words.
fn tokenize(s: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' => {
                chars.next();
            }
            '(' | ')' => {
                toks.push(c.to_string());
                chars.next();
            }
            '<' => {
                let mut iri = String::from("<");
                chars.next();
                for d in chars.by_ref() {
                    iri.push(d);
                    if d == '>' {
                        break;
                    }
                }
                toks.push(iri);
            }
            _ => {
                let mut word = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_whitespace() || d == '(' || d == ')' {
                        break;
                    }
                    word.push(d);
                    chars.next();
                }
                toks.push(word);
            }
        }
    }
    toks
}

/// Resolve a Manchester token (`<iri>` or `prefix:local`) to a full IRI.
fn resolve(prefixes: &horned_owl::curie::PrefixMapping, tok: &str) -> String {
    let tok = tok.trim();
    if let Some(inner) = tok.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
        return inner.to_string();
    }
    if let Ok(expanded) = prefixes.expand_curie_string(tok) {
        return expanded;
    }
    crate::io::obo::expand_id(tok)
}
