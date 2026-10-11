//! Manchester syntax with each entity named by its label, as the markdown
//! reports write the objects they list: the axioms of an `explain`
//! explanation, and the changes of a `diff` report and its frame headers.
//!
//! Each entity is written as a `[label](IRI)` link, or as its label alone; an
//! entity without a label goes by its IRI's short form. The members of a set
//! are written in the natural order of the document the objects come from.

use std::collections::HashMap;

use horned_owl::model::{
    Annotation, AnnotationSubject, AnnotationValue, Atom, ClassExpression as CE, Component, DArgument, DataProperty,
    DataRange as DR, IArgument, Individual, Literal, ObjectPropertyExpression as OPE, PropertyExpression, RcStr,
    SubObjectPropertyExpression as SOPE, Variable,
};

use crate::io::natural_order::{iri_cmp, sorted_set, NaturalOrder};

const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
const XSD_FLOAT: &str = "http://www.w3.org/2001/XMLSchema#float";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// How a report spells what the two reports write differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    /// The `explain` report: a quoted literal's `"` and `\` escaped with a
    /// backslash, its datatype written unless it is plain or `xsd:string`; an
    /// IRI written in angle brackets.
    Explain,
    /// The `diff` report: a quoted literal's text HTML-escaped, its datatype
    /// written unless it is plain; an IRI named as the class it identifies.
    Diff,
}

pub(crate) struct Renderer<'a> {
    /// Each labelled entity's label, by IRI.
    pub labels: &'a HashMap<String, String>,
    /// The order of the rendered objects' document, which sorts the members of
    /// a set the renderer writes.
    pub order: NaturalOrder,
    pub dialect: Dialect,
    /// An entity is written as a `[label](IRI)` link; otherwise as its label.
    pub links: bool,
}

impl Renderer<'_> {
    /// An entity by its label, or its IRI's short form when it has none, as a
    /// link to its IRI or alone.
    pub fn link(&self, iri: &str) -> String {
        let label = self.labels.get(iri).cloned().unwrap_or_else(|| crate::owlapi_hash::iri_short_form(iri));
        if self.links {
            format!("[{label}]({iri})")
        } else {
            label
        }
    }

    fn iri(&self, iri: &str, out: &mut String) {
        match self.dialect {
            Dialect::Explain => {
                out.push('<');
                out.push_str(iri);
                out.push('>');
            }
            Dialect::Diff => out.push_str(&self.link(iri)),
        }
    }

    pub fn ope(&self, o: &OPE<RcStr>, out: &mut String) {
        match o {
            OPE::ObjectProperty(p) => out.push_str(&self.link(p.0.as_ref())),
            OPE::InverseObjectProperty(p) => {
                out.push_str(" inverse (");
                out.push_str(&self.link(p.0.as_ref()));
                out.push(')');
            }
        }
    }

    fn dp(&self, p: &DataProperty<RcStr>, out: &mut String) {
        out.push_str(&self.link(p.0.as_ref()));
    }

    /// A literal: an `xsd:decimal`, `xsd:integer` or `xsd:boolean` as written,
    /// an `xsd:float` with an `f`, and anything else quoted, with its language
    /// tag or its datatype, as the [`Dialect`] writes them.
    fn literal(&self, l: &Literal<RcStr>, out: &mut String) {
        let dt = self.order.literal_datatype(l);
        let lex = l.literal();
        if dt == XSD_DECIMAL || dt == XSD_INTEGER || dt == XSD_BOOLEAN {
            out.push_str(lex);
            return;
        }
        if dt == XSD_FLOAT {
            out.push_str(lex);
            out.push('f');
            return;
        }
        out.push('"');
        match self.dialect {
            Dialect::Explain => {
                for c in lex.chars() {
                    if c == '"' || c == '\\' {
                        out.push('\\');
                    }
                    out.push(c);
                }
            }
            Dialect::Diff => out.push_str(&crate::html_escape::escape_html4(lex)),
        }
        out.push('"');
        let typed = match self.dialect {
            Dialect::Explain => dt != RDF_PLAIN_LITERAL && dt != XSD_STRING,
            Dialect::Diff => dt != RDF_PLAIN_LITERAL,
        };
        match l {
            Literal::Language { lang, .. } if !lang.is_empty() => {
                out.push('@');
                out.push_str(lang);
            }
            _ if typed => {
                out.push_str("^^");
                out.push_str(&self.link(dt));
            }
            _ => {}
        }
    }

    pub fn data_range(&self, d: &DR<RcStr>, out: &mut String) {
        match d {
            DR::Datatype(t) => out.push_str(&self.link(t.0.as_ref())),
            DR::DataComplementOf(op) => {
                out.push_str(" not ");
                if matches!(**op, DR::Datatype(_)) {
                    self.data_range(op, out);
                } else {
                    out.push('(');
                    self.data_range(op, out);
                    out.push(')');
                }
            }
            DR::DataOneOf(lits) => {
                out.push('{');
                for (i, l) in sorted_set(lits, |a, b| self.order.literal(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(" , ");
                    }
                    self.literal(l, out);
                }
                out.push('}');
            }
            DR::DataIntersectionOf(ops) | DR::DataUnionOf(ops) => {
                let word = if matches!(d, DR::DataIntersectionOf(_)) { " and " } else { " or " };
                out.push('(');
                for (i, op) in sorted_set(ops, |a, b| self.order.dr(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(word);
                    }
                    self.data_range(op, out);
                }
                out.push(')');
            }
            DR::DatatypeRestriction(t, facets) => {
                out.push_str(&self.link(t.0.as_ref()));
                out.push('[');
                for (i, f) in sorted_set(facets, |a, b| self.order.facet_restriction(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(" , ");
                    }
                    out.push_str(crate::io::manchester_write::facet_symbol(&f.f));
                    out.push(' ');
                    self.literal(&f.l, out);
                }
                out.push(']');
            }
        }
    }

    fn data_restriction(&self, p: &DataProperty<RcStr>, keyword: &str, n: Option<u32>, filler: &DR<RcStr>, out: &mut String) {
        self.dp(p, out);
        out.push(' ');
        out.push_str(keyword);
        out.push(' ');
        if let Some(n) = n {
            out.push_str(&n.to_string());
            out.push(' ');
        }
        self.data_range(filler, out);
    }

    /// Members of a set axiom: `a word b` for two, otherwise a section listing
    /// them, `lead` before its keyword.
    fn members(&self, items: Vec<String>, word: Option<&str>, lead: &str, section: &str) -> String {
        match (word, items.as_slice()) {
            (Some(word), [a, b]) => format!("{a} {word} {b}"),
            _ => format!("{lead}{section}: {}", items.join(", ")),
        }
    }

    fn opes(&self, v: &[OPE<RcStr>]) -> Vec<String> {
        sorted_set(v, |a, b| self.order.ope(a, b))
            .into_iter()
            .map(|o| {
                let mut s = String::new();
                self.ope(o, &mut s);
                s
            })
            .collect()
    }

    fn dps(&self, v: &[DataProperty<RcStr>]) -> Vec<String> {
        sorted_set(v, |a, b| iri_cmp(a.0.as_ref(), b.0.as_ref()))
            .into_iter()
            .map(|p| self.link(p.0.as_ref()))
            .collect()
    }

    fn inds(&self, v: &[Individual<RcStr>]) -> Vec<String> {
        sorted_set(v, |a, b| self.order.individual(a, b))
            .into_iter()
            .map(|i| {
                let mut s = String::new();
                self.ind(i, &mut s);
                s
            })
            .collect()
    }

    pub fn ind(&self, i: &Individual<RcStr>, out: &mut String) {
        match i {
            Individual::Named(n) => out.push_str(&self.link(n.0.as_ref())),
            Individual::Anonymous(a) => out.push_str(&crate::io::entities::node_id(a.0.as_ref())),
        }
    }

    fn operand(&self, c: &CE<RcStr>, out: &mut String) {
        if matches!(c, CE::Class(_)) {
            self.ce(c, out);
        } else {
            out.push('(');
            self.ce(c, out);
            out.push(')');
        }
    }

    /// A quantified restriction: an anonymous filler is bracketed, and an
    /// intersection or union starts a new line.
    fn restriction(&self, o: &OPE<RcStr>, keyword: &str, filler: &CE<RcStr>, out: &mut String) {
        self.ope(o, out);
        out.push(' ');
        out.push_str(keyword);
        out.push(' ');
        match filler {
            CE::Class(_) => self.ce(filler, out),
            CE::ObjectIntersectionOf(_) | CE::ObjectUnionOf(_) => {
                out.push_str("\n(");
                self.ce(filler, out);
                out.push(')');
            }
            _ => {
                out.push('(');
                self.ce(filler, out);
                out.push(')');
            }
        }
    }

    fn cardinality(&self, o: &OPE<RcStr>, keyword: &str, n: u32, filler: &CE<RcStr>, out: &mut String) {
        self.ope(o, out);
        out.push(' ');
        out.push_str(keyword);
        out.push(' ');
        out.push_str(&n.to_string());
        out.push(' ');
        self.operand(filler, out);
    }

    fn ce(&self, c: &CE<RcStr>, out: &mut String) {
        match c {
            CE::Class(x) => out.push_str(&self.link(x.0.as_ref())),
            CE::ObjectIntersectionOf(ops) | CE::ObjectUnionOf(ops) => {
                let word = if matches!(c, CE::ObjectIntersectionOf(_)) { " and " } else { " or " };
                for (i, op) in sorted_set(ops, |a, b| self.order.ce(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(word);
                    }
                    self.operand(op, out);
                }
            }
            CE::ObjectComplementOf(b) => {
                out.push_str("not (");
                self.ce(b, out);
                out.push(')');
            }
            CE::ObjectSomeValuesFrom { ope, bce } => self.restriction(ope, "some", bce, out),
            CE::ObjectAllValuesFrom { ope, bce } => self.restriction(ope, "only", bce, out),
            CE::ObjectHasValue { ope, i } => {
                self.ope(ope, out);
                out.push_str(" value ");
                self.ind(i, out);
            }
            CE::ObjectMinCardinality { n, ope, bce } => self.cardinality(ope, "min", *n, bce, out),
            CE::ObjectMaxCardinality { n, ope, bce } => self.cardinality(ope, "max", *n, bce, out),
            CE::ObjectExactCardinality { n, ope, bce } => self.cardinality(ope, "exactly", *n, bce, out),
            CE::ObjectHasSelf(ope) => {
                self.ope(ope, out);
                out.push_str(" Self ");
            }
            CE::ObjectOneOf(inds) => {
                let mut v: Vec<&Individual<RcStr>> = inds.iter().collect();
                v.sort_by(|a, b| self.order.individual(a, b));
                out.push('{');
                for (i, x) in v.into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(" , ");
                    }
                    self.ind(x, out);
                }
                out.push('}');
            }
            CE::DataSomeValuesFrom { dp, dr } => self.data_restriction(dp, "some", None, dr, out),
            CE::DataAllValuesFrom { dp, dr } => self.data_restriction(dp, "only", None, dr, out),
            CE::DataHasValue { dp, l } => {
                self.dp(dp, out);
                out.push_str(" value ");
                self.literal(l, out);
            }
            CE::DataMinCardinality { n, dp, dr } => self.data_restriction(dp, "min", Some(*n), dr, out),
            CE::DataMaxCardinality { n, dp, dr } => self.data_restriction(dp, "max", Some(*n), dr, out),
            CE::DataExactCardinality { n, dp, dr } => self.data_restriction(dp, "exactly", Some(*n), dr, out),
        }
    }

    fn pair(&self, members: &[CE<RcStr>], binary: &str, nary: &str) -> String {
        let sorted = sorted_set(members, |a, b| self.order.ce(a, b));
        let mut out = String::new();
        if sorted.len() == 2 {
            self.ce(sorted[0], &mut out);
            out.push(' ');
            out.push_str(binary);
            out.push(' ');
            self.ce(sorted[1], &mut out);
        } else {
            out.push(' ');
            out.push_str(nary);
            out.push_str(": ");
            for (i, c) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                self.ce(c, &mut out);
            }
        }
        out
    }

    fn annotation_value(&self, v: &AnnotationValue<RcStr>, out: &mut String) {
        match v {
            AnnotationValue::Literal(l) => self.literal(l, out),
            AnnotationValue::IRI(i) => self.iri(i.as_ref(), out),
            AnnotationValue::AnonymousIndividual(a) => out.push_str(&crate::io::entities::node_id(a.0.as_ref())),
        }
    }

    /// An annotation, without the annotations on it: its property, then its
    /// value.
    pub fn annotation(&self, a: &Annotation<RcStr>) -> String {
        let mut out = self.link(a.ap.0.as_ref());
        out.push(' ');
        self.annotation_value(&a.av, &mut out);
        out
    }

    /// A component, without its annotations: an axiom, an ontology annotation
    /// or an import, which is written as its IRI.
    pub fn axiom(&self, c: &Component<RcStr>) -> String {
        use Component as C;
        let mut out = String::new();
        let section = |out: &mut String, word: &str, o: &OPE<RcStr>| {
            out.push(' ');
            out.push_str(word);
            out.push_str(": ");
            self.ope(o, out);
        };
        let frame = |out: &mut String, keyword: &str, iri: &str| {
            out.push_str(keyword);
            out.push_str(&self.link(iri));
        };
        match c {
            C::OntologyID(_) | C::DocIRI(_) => {}
            C::Import(x) => self.iri(x.0.as_ref(), &mut out),
            C::OntologyAnnotation(x) => out = self.annotation(&x.0),
            C::DeclareClass(x) => frame(&mut out, "Class: ", x.0 .0.as_ref()),
            C::DeclareObjectProperty(x) => frame(&mut out, "ObjectProperty: ", x.0 .0.as_ref()),
            C::DeclareDataProperty(x) => frame(&mut out, "DataProperty: ", x.0 .0.as_ref()),
            C::DeclareAnnotationProperty(x) => frame(&mut out, "AnnotationProperty: ", x.0 .0.as_ref()),
            C::DeclareNamedIndividual(x) => frame(&mut out, "Individual: ", x.0 .0.as_ref()),
            // A datatype has no frame keyword.
            C::DeclareDatatype(x) => frame(&mut out, "", x.0 .0.as_ref()),
            C::SubClassOf(x) => {
                self.ce(&x.sub, &mut out);
                out.push_str(" SubClassOf ");
                self.ce(&x.sup, &mut out);
            }
            C::EquivalentClasses(x) => out = self.pair(&x.0, "EquivalentTo", "EquivalentClasses"),
            C::DisjointClasses(x) => out = self.pair(&x.0, "DisjointWith", "DisjointClasses"),
            C::SubObjectPropertyOf(x) => {
                match &x.sub {
                    SOPE::ObjectPropertyExpression(o) => self.ope(o, &mut out),
                    SOPE::ObjectPropertyChain(chain) => {
                        for (i, o) in chain.iter().enumerate() {
                            if i > 0 {
                                out.push_str(" o ");
                            }
                            self.ope(o, &mut out);
                        }
                    }
                }
                out.push_str(" SubPropertyOf: ");
                self.ope(&x.sup, &mut out);
            }
            C::TransitiveObjectProperty(x) => section(&mut out, "Transitive", &x.0),
            C::FunctionalObjectProperty(x) => section(&mut out, "Functional", &x.0),
            C::InverseFunctionalObjectProperty(x) => section(&mut out, "InverseFunctional", &x.0),
            C::SymmetricObjectProperty(x) => section(&mut out, "Symmetric", &x.0),
            C::AsymmetricObjectProperty(x) => section(&mut out, "Asymmetric", &x.0),
            C::ReflexiveObjectProperty(x) => section(&mut out, "Reflexive", &x.0),
            C::IrreflexiveObjectProperty(x) => section(&mut out, "Irreflexive", &x.0),
            // The pair in its natural order.
            C::InverseObjectProperties(x) => {
                let (first, second) = if self.order.ope(&x.1, &x.0).is_lt() { (&x.1, &x.0) } else { (&x.0, &x.1) };
                self.ope(first, &mut out);
                out.push_str(" InverseOf ");
                self.ope(second, &mut out);
            }
            C::ObjectPropertyDomain(x) => {
                self.ope(&x.ope, &mut out);
                out.push_str(" Domain ");
                self.ce(&x.ce, &mut out);
            }
            C::ObjectPropertyRange(x) => {
                self.ope(&x.ope, &mut out);
                out.push_str(" Range ");
                self.ce(&x.ce, &mut out);
            }
            C::ClassAssertion(x) => {
                self.ind(&x.i, &mut out);
                out.push_str(" Type ");
                self.ce(&x.ce, &mut out);
            }
            C::ObjectPropertyAssertion(x) => {
                self.ind(&x.from, &mut out);
                out.push(' ');
                self.ope(&x.ope, &mut out);
                out.push(' ');
                self.ind(&x.to, &mut out);
            }
            C::NegativeObjectPropertyAssertion(x) => {
                out.push_str(" not (");
                self.ind(&x.from, &mut out);
                out.push(' ');
                self.ope(&x.ope, &mut out);
                out.push(' ');
                self.ind(&x.to, &mut out);
                out.push(')');
            }
            C::DataPropertyAssertion(x) => {
                self.ind(&x.from, &mut out);
                out.push(' ');
                self.dp(&x.dp, &mut out);
                out.push(' ');
                self.literal(&x.to, &mut out);
            }
            C::NegativeDataPropertyAssertion(x) => {
                out.push_str(" not (");
                self.ind(&x.from, &mut out);
                out.push(' ');
                self.dp(&x.dp, &mut out);
                out.push(' ');
                self.literal(&x.to, &mut out);
                out.push(')');
            }
            C::EquivalentObjectProperties(x) => {
                out = self.members(self.opes(&x.0), Some("EquivalentTo"), " ", "EquivalentProperties")
            }
            C::DisjointObjectProperties(x) => {
                out = self.members(self.opes(&x.0), Some("DisjointWith"), " ", "DisjointProperties")
            }
            C::EquivalentDataProperties(x) => out = self.members(self.dps(&x.0), None, "", "EquivalentProperties"),
            C::DisjointDataProperties(x) => {
                out = self.members(self.dps(&x.0), Some("DisjointWith"), " ", "DisjointProperties")
            }
            C::SameIndividual(x) => out = self.members(self.inds(&x.0), Some("SameAs"), " ", "SameIndividual"),
            C::DifferentIndividuals(x) => {
                out = self.members(self.inds(&x.0), Some("DifferentFrom"), " ", "DifferentIndividuals")
            }
            C::SubDataPropertyOf(x) => {
                self.dp(&x.sub, &mut out);
                out.push_str(" SubPropertyOf: ");
                self.dp(&x.sup, &mut out);
            }
            C::FunctionalDataProperty(x) => {
                out.push_str(" Functional: ");
                self.dp(&x.0, &mut out);
            }
            C::DataPropertyDomain(x) => {
                self.dp(&x.dp, &mut out);
                out.push_str(" Domain ");
                self.ce(&x.ce, &mut out);
            }
            C::DataPropertyRange(x) => {
                self.dp(&x.dp, &mut out);
                out.push_str(" Range: ");
                self.data_range(&x.dr, &mut out);
            }
            C::DisjointUnion(x) => {
                out.push_str(&self.link(x.0 .0.as_ref()));
                out.push_str(" DisjointUnionOf ");
                for (i, m) in sorted_set(&x.1, |a, b| self.order.ce(a, b)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.ce(m, &mut out);
                }
            }
            // The key's object properties and then its data properties, each
            // list sorted, with nothing between the two lists.
            C::HasKey(x) => {
                self.ce(&x.ce, &mut out);
                out.push_str(" HasKey ");
                let mut opes = Vec::new();
                let mut dps = Vec::new();
                for p in &x.vpe {
                    match p {
                        PropertyExpression::ObjectPropertyExpression(o) => opes.push(o.clone()),
                        PropertyExpression::DataProperty(d) => dps.push(d.clone()),
                        PropertyExpression::AnnotationProperty(_) => {}
                    }
                }
                out.push_str(&self.opes(&opes).join(" , "));
                out.push_str(&self.dps(&dps).join(" , "));
            }
            // A datatype definition is written as nothing at all.
            C::DatatypeDefinition(_) => {}
            C::AnnotationAssertion(x) => {
                match &x.subject {
                    AnnotationSubject::IRI(i) => self.iri(i.as_ref(), &mut out),
                    AnnotationSubject::AnonymousIndividual(a) => {
                        out.push_str(&crate::io::entities::node_id(a.0.as_ref()))
                    }
                }
                out.push(' ');
                out.push_str(&self.annotation(&x.ann));
            }
            C::SubAnnotationPropertyOf(x) => {
                out.push_str(&self.link(x.sub.0.as_ref()));
                out.push_str(" SubPropertyOf: ");
                out.push_str(&self.link(x.sup.0.as_ref()));
            }
            C::AnnotationPropertyDomain(x) => {
                out.push_str(&self.link(x.ap.0.as_ref()));
                out.push_str(" Domain ");
                self.iri(x.iri.as_ref(), &mut out);
            }
            C::AnnotationPropertyRange(x) => {
                out.push_str(&self.link(x.ap.0.as_ref()));
                out.push_str(" Range ");
                self.iri(x.iri.as_ref(), &mut out);
            }
            C::Rule(r) => {
                self.atoms(&r.body, &mut out);
                out.push_str(" -> ");
                self.atoms(&r.head, &mut out);
            }
        }
        out
    }

    /// A rule's atoms, in the order the rule lists them.
    fn atoms(&self, atoms: &[Atom<RcStr>], out: &mut String) {
        for (i, a) in atoms.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            self.atom(a, out);
        }
    }

    fn atom(&self, a: &Atom<RcStr>, out: &mut String) {
        let pair = |out: &mut String, x: &IArgument<RcStr>, y: &IArgument<RcStr>| {
            out.push('(');
            self.iarg(x, out);
            out.push_str(", ");
            self.iarg(y, out);
            out.push(')');
        };
        match a {
            Atom::ClassAtom { pred, arg } => {
                if let CE::Class(_) = pred {
                    self.ce(pred, out);
                } else {
                    out.push('(');
                    self.ce(pred, out);
                    out.push(')');
                }
                out.push('(');
                self.iarg(arg, out);
                out.push(')');
            }
            Atom::DataRangeAtom { pred, arg } => {
                self.data_range(pred, out);
                out.push('(');
                self.darg(arg, out);
                out.push(')');
            }
            Atom::ObjectPropertyAtom { pred, args } => {
                self.ope(pred, out);
                pair(out, &args.0, &args.1);
            }
            Atom::DataPropertyAtom { pred, args } => {
                self.dp(pred, out);
                out.push('(');
                self.iarg(&args.0, out);
                out.push_str(", ");
                self.darg(&args.1, out);
                out.push(')');
            }
            // A built-in of the SWRL vocabulary goes by its swrlb: name, any
            // other by its IRI; its arguments are sorted.
            Atom::BuiltInAtom { pred, args } => {
                match pred.as_ref().strip_prefix(SWRLB) {
                    Some(name) if SWRL_BUILT_INS.contains(&name) => {
                        out.push_str("swrlb:");
                        out.push_str(name);
                    }
                    _ => {
                        out.push('<');
                        out.push_str(pred.as_ref());
                        out.push('>');
                    }
                }
                out.push('(');
                for (i, d) in sorted_set(args, |x, y| self.order.darg(x, y)).into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.darg(d, out);
                }
                out.push(')');
            }
            Atom::SameIndividualAtom(x, y) => {
                out.push_str(" SameAs ");
                pair(out, x, y);
            }
            Atom::DifferentIndividualsAtom(x, y) => {
                out.push_str(" DifferentFrom ");
                pair(out, x, y);
            }
        }
    }

    fn iarg(&self, a: &IArgument<RcStr>, out: &mut String) {
        match a {
            IArgument::Individual(i) => self.ind(i, out),
            IArgument::Variable(v) => variable(v, out),
        }
    }

    fn darg(&self, a: &DArgument<RcStr>, out: &mut String) {
        match a {
            DArgument::Literal(l) => self.literal(l, out),
            DArgument::Variable(v) => variable(v, out),
        }
    }
}

/// A rule variable: `?` and its local name when it lies in a SWRL variable
/// namespace, otherwise `?` and its IRI in angle brackets.
fn variable(v: &Variable<RcStr>, out: &mut String) {
    out.push('?');
    let (ns, local) = crate::owlapi_hash::iri_split(v.0.as_ref());
    if ns == "urn:swrl:var#" || ns == "urn:swrl#" {
        out.push_str(local);
    } else {
        out.push('<');
        out.push_str(v.0.as_ref());
        out.push('>');
    }
}

/// The namespace of the SWRL built-ins.
const SWRLB: &str = "http://www.w3.org/2003/11/swrlb#";

/// The built-ins of the SWRL vocabulary, by their names in [`SWRLB`].
const SWRL_BUILT_INS: [&str; 69] = [
    "equal", "notEqual", "lessThan", "lessThanOrEqual", "greaterThan", "greaterThanOrEqual", "add",
    "subtract", "multiply", "divide", "integerDivide", "mod", "pow", "unaryMinus", "unaryPlus", "abs",
    "ceiling", "floor", "round", "roundHalfToEven", "sin", "cos", "tan", "booleanNot",
    "stringEqualIgnoreCase", "stringConcat", "substring", "stringLength", "normalizeSpace", "upperCase",
    "lowerCase", "translate", "contains", "containsIgnoreCase", "startsWith", "endsWith", "substringBefore",
    "substringAfter", "matchesLax", "replace", "tokenize", "yearMonthDuration", "dayTimeDuration", "dateTime",
    "date", "time", "subtractDates", "subtractTimes", "resolveURI", "anyURI", "addYearMonthDurations",
    "subtractYearMonthDurations", "multiplyYearMonthDurations", "divideYearMonthDurations",
    "addDayTimeDurations", "subtractDayTimeDurations", "multiplyDayTimeDurations", "divideDayTimeDurations",
    "addDayTimeDurationToDateTime", "subtractYearMonthDurationFromDateTime",
    "subtractDayTimeDurationFromDateTime", "addYearMonthDurationToDate", "addDayTimeDurationToDate",
    "subtractYearMonthDurationFromDate", "subtractDayTimeDurationFromDate", "addDayTimeDurationToTime",
    "subtractDayTimeDurationFromTime", "subtractDateTimesYieldingYearMonthDuration",
    "subtractDateTimesYieldingDayTimeDuration",
];
