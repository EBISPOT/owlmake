//! OWL/XML (`.owx`): the writer, and the IRI normalisation the reader applies.
//!
//! A written document is an `Ontology` element whose `xml:base` is the ontology
//! IRI — the OWL namespace for an anonymous ontology — holding, in order: one
//! `Prefix` element per declared prefix (shortest name first, then
//! alphabetically), the imports, the ontology annotations, the ontology's own
//! declarations, a declaration for each entity it mentions without declaring
//! (see [`crate::io::entities::undeclared`]), and every other axiom; each group
//! in the natural order of OWL objects ([`crate::io::natural_order`]).
//!
//! An IRI is written one of three ways. Inside the base it is relative to it
//! (`IRI="#A"`); one whose namespace — the IRI up to its NCName suffix — is a
//! declared prefix's namespace is abbreviated (`abbreviatedIRI="rdfs:label"`,
//! `<AbbreviatedIRI>obo:EX_0000001</AbbreviatedIRI>`); any other is written in
//! full. A prefixed name is never written where an IRI belongs.
//!
//! Reading takes the same view: an `IRI` value is an IRI — absolute when it has
//! a scheme, so `obo:EX_0000001` there names the IRI `obo:EX_0000001`, and
//! otherwise relative to `xml:base` — and only an `abbreviatedIRI` value is a
//! prefixed name, which must use a declared prefix.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{bail, Context, Result};
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, Atom, ClassExpression as CE,
    Component, DArgument, DataRange as DR, IArgument, Individual, Literal, ObjectPropertyExpression as OPE,
    PropertyExpression, RcStr, SubObjectPropertyExpression as SOPE,
};

use crate::io::entities::{self, Kind};
use crate::io::natural_order::{iri_split, sorted_set, NaturalOrder};
use crate::io::owlrdf::esc;
use crate::model::Model;

const OWL_NS: &str = "http://www.w3.org/2002/07/owl#";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const RDFS_LITERAL: &str = "http://www.w3.org/2000/01/rdf-schema#Literal";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

// === Element writer ======================================================

/// One open element: its start tag is written as late as possible, so a
/// childless element can close itself (`<Class IRI="#A"/>`) and one with text
/// content keeps it on one line.
struct Elem {
    name: &'static str,
    attrs: Vec<(&'static str, String)>,
    text: Option<String>,
    started: bool,
    depth: usize,
    /// Attributes on lines of their own (the root element).
    wrap: bool,
}

struct Xml<'w, W: Write> {
    w: &'w mut W,
    stack: Vec<Elem>,
}

impl<W: Write> Xml<'_, W> {
    fn indent(w: &mut W, depth: usize) -> std::io::Result<()> {
        for _ in 0..depth * 4 {
            w.write_all(b" ")?;
        }
        Ok(())
    }

    fn write_start(w: &mut W, e: &mut Elem, close: bool) -> std::io::Result<()> {
        if e.started {
            return Ok(());
        }
        e.started = true;
        Self::indent(w, e.depth)?;
        write!(w, "<{}", e.name)?;
        let n = e.attrs.len();
        for (i, (k, v)) in e.attrs.iter().enumerate() {
            write!(w, " {k}=\"{}\"", esc(v))?;
            if e.wrap && i + 1 < n {
                w.write_all(b"\n")?;
                Self::indent(w, e.depth + 1)?;
            }
        }
        if let Some(t) = &e.text {
            write!(w, ">{}", esc(t))?;
        }
        if close {
            if e.text.is_some() {
                writeln!(w, "</{}>", e.name)?;
            } else {
                w.write_all(b"/>\n")?;
            }
        } else if e.text.is_none() {
            w.write_all(b">\n")?;
        }
        Ok(())
    }

    fn start(&mut self, name: &'static str) -> std::io::Result<()> {
        if let Some(top) = self.stack.last_mut() {
            Self::write_start(self.w, top, false)?;
        }
        let depth = self.stack.len();
        self.stack.push(Elem { name, attrs: Vec::new(), text: None, started: false, depth, wrap: false });
        Ok(())
    }

    fn attr(&mut self, name: &'static str, value: impl Into<String>) {
        if let Some(top) = self.stack.last_mut() {
            top.attrs.push((name, value.into()));
        }
    }

    fn text(&mut self, text: impl Into<String>) {
        if let Some(top) = self.stack.last_mut() {
            top.text = Some(text.into());
        }
    }

    fn end(&mut self) -> std::io::Result<()> {
        let Some(mut e) = self.stack.pop() else { return Ok(()) };
        if !e.started {
            return Self::write_start(self.w, &mut e, true);
        }
        if e.text.is_none() {
            Self::indent(self.w, e.depth)?;
        }
        writeln!(self.w, "</{}>", e.name)
    }

    /// An element holding nothing but text.
    fn text_element(&mut self, name: &'static str, text: &str) -> std::io::Result<()> {
        self.start(name)?;
        self.text(text);
        self.end()
    }
}

// === Writer ==============================================================

struct Renderer<'w, W: Write> {
    xml: Xml<'w, W>,
    /// `xml:base`: IRIs under it are written relative to it.
    base: String,
    /// Namespace → `prefix:`, for abbreviating.
    abbrev: HashMap<String, String>,
    order: NaturalOrder,
}

impl<W: Write> Renderer<'_, W> {
    /// The abbreviated form of `iri`, when its namespace is a declared prefix's.
    fn abbreviated(&self, iri: &str) -> Option<String> {
        let (ns, local) = iri_split(iri);
        self.abbrev.get(ns).map(|p| format!("{p}{local}"))
    }

    fn iri_attr(&mut self, iri: &str) {
        if let Some(rel) = iri.strip_prefix(self.base.as_str()) {
            self.xml.attr("IRI", rel);
        } else if let Some(short) = self.abbreviated(iri) {
            self.xml.attr("abbreviatedIRI", short);
        } else {
            self.xml.attr("IRI", iri);
        }
    }

    fn iri_element(&mut self, iri: &str) -> std::io::Result<()> {
        if let Some(rel) = iri.strip_prefix(self.base.as_str()) {
            let rel = rel.to_string();
            self.xml.text_element("IRI", &rel)
        } else if let Some(short) = self.abbreviated(iri) {
            self.xml.text_element("AbbreviatedIRI", &short)
        } else {
            self.xml.text_element("IRI", iri)
        }
    }

    fn entity(&mut self, element: &'static str, iri: &str) -> std::io::Result<()> {
        self.xml.start(element)?;
        self.iri_attr(iri);
        self.xml.end()
    }

    fn class(&mut self, iri: &str) -> std::io::Result<()> {
        self.entity("Class", iri)
    }

    fn literal(&mut self, l: &Literal<RcStr>) -> std::io::Result<()> {
        self.xml.start("Literal")?;
        match l {
            Literal::Language { lang, .. } if !lang.is_empty() => self.xml.attr("xml:lang", lang.clone()),
            Literal::Datatype { datatype_iri, .. } => {
                let dt: &str = datatype_iri.as_ref();
                if dt != XSD_STRING && dt != RDF_PLAIN_LITERAL {
                    self.xml.attr("datatypeIRI", dt);
                }
            }
            _ => {}
        }
        self.xml.text(l.literal().clone());
        self.xml.end()
    }

    fn anonymous(&mut self, id: &str) -> std::io::Result<()> {
        self.xml.start("AnonymousIndividual")?;
        self.xml.attr("nodeID", crate::io::entities::node_id(id).into_owned());
        self.xml.end()
    }

    fn individual(&mut self, i: &Individual<RcStr>) -> std::io::Result<()> {
        match i {
            Individual::Named(n) => self.entity("NamedIndividual", n.0.as_ref()),
            Individual::Anonymous(a) => self.anonymous(a.0.as_ref()),
        }
    }

    fn individuals(&mut self, inds: &[Individual<RcStr>]) -> std::io::Result<()> {
        let order = self.order;
        for i in sorted_set(inds, |a, b| order.individual(a, b)) {
            self.individual(i)?;
        }
        Ok(())
    }

    fn ope(&mut self, ope: &OPE<RcStr>) -> std::io::Result<()> {
        match ope {
            OPE::ObjectProperty(p) => self.entity("ObjectProperty", p.0.as_ref()),
            OPE::InverseObjectProperty(p) => {
                self.xml.start("ObjectInverseOf")?;
                self.entity("ObjectProperty", p.0.as_ref())?;
                self.xml.end()
            }
        }
    }

    fn opes(&mut self, opes: &[OPE<RcStr>]) -> std::io::Result<()> {
        let order = self.order;
        for p in sorted_set(opes, |a, b| order.ope(a, b)) {
            self.ope(p)?;
        }
        Ok(())
    }

    fn data_properties<'a>(&mut self, dps: impl IntoIterator<Item = &'a str>) -> std::io::Result<()> {
        let mut v: Vec<&str> = dps.into_iter().collect();
        v.sort_by(|a, b| crate::io::natural_order::iri_cmp(a, b));
        v.dedup();
        for dp in v {
            self.entity("DataProperty", dp)?;
        }
        Ok(())
    }

    fn ces(&mut self, ces: &[CE<RcStr>]) -> std::io::Result<()> {
        let order = self.order;
        for ce in sorted_set(ces, |a, b| order.ce(a, b)) {
            self.ce(ce)?;
        }
        Ok(())
    }

    fn ce(&mut self, ce: &CE<RcStr>) -> std::io::Result<()> {
        let qualified_object = |f: &CE<RcStr>| !matches!(f, CE::Class(c) if c.0.as_ref() == OWL_THING);
        let qualified_data = |f: &DR<RcStr>| !matches!(f, DR::Datatype(d) if d.0.as_ref() == RDFS_LITERAL);
        match ce {
            CE::Class(c) => self.class(c.0.as_ref()),
            CE::ObjectIntersectionOf(ops) => {
                self.xml.start("ObjectIntersectionOf")?;
                self.ces(ops)?;
                self.xml.end()
            }
            CE::ObjectUnionOf(ops) => {
                self.xml.start("ObjectUnionOf")?;
                self.ces(ops)?;
                self.xml.end()
            }
            CE::ObjectComplementOf(op) => {
                self.xml.start("ObjectComplementOf")?;
                self.ce(op)?;
                self.xml.end()
            }
            CE::ObjectOneOf(inds) => {
                self.xml.start("ObjectOneOf")?;
                self.individuals(inds)?;
                self.xml.end()
            }
            CE::ObjectSomeValuesFrom { ope, bce } => {
                self.xml.start("ObjectSomeValuesFrom")?;
                self.ope(ope)?;
                self.ce(bce)?;
                self.xml.end()
            }
            CE::ObjectAllValuesFrom { ope, bce } => {
                self.xml.start("ObjectAllValuesFrom")?;
                self.ope(ope)?;
                self.ce(bce)?;
                self.xml.end()
            }
            CE::ObjectHasValue { ope, i } => {
                self.xml.start("ObjectHasValue")?;
                self.ope(ope)?;
                self.individual(i)?;
                self.xml.end()
            }
            CE::ObjectHasSelf(ope) => {
                self.xml.start("ObjectHasSelf")?;
                self.ope(ope)?;
                self.xml.end()
            }
            CE::ObjectMinCardinality { n, ope, bce }
            | CE::ObjectMaxCardinality { n, ope, bce }
            | CE::ObjectExactCardinality { n, ope, bce } => {
                self.xml.start(match ce {
                    CE::ObjectMinCardinality { .. } => "ObjectMinCardinality",
                    CE::ObjectMaxCardinality { .. } => "ObjectMaxCardinality",
                    _ => "ObjectExactCardinality",
                })?;
                self.xml.attr("cardinality", n.to_string());
                self.ope(ope)?;
                if qualified_object(bce) {
                    self.ce(bce)?;
                }
                self.xml.end()
            }
            CE::DataSomeValuesFrom { dp, dr } => {
                self.xml.start("DataSomeValuesFrom")?;
                self.entity("DataProperty", dp.0.as_ref())?;
                self.dr(dr)?;
                self.xml.end()
            }
            CE::DataAllValuesFrom { dp, dr } => {
                self.xml.start("DataAllValuesFrom")?;
                self.entity("DataProperty", dp.0.as_ref())?;
                self.dr(dr)?;
                self.xml.end()
            }
            CE::DataHasValue { dp, l } => {
                self.xml.start("DataHasValue")?;
                self.entity("DataProperty", dp.0.as_ref())?;
                self.literal(l)?;
                self.xml.end()
            }
            CE::DataMinCardinality { n, dp, dr }
            | CE::DataMaxCardinality { n, dp, dr }
            | CE::DataExactCardinality { n, dp, dr } => {
                self.xml.start(match ce {
                    CE::DataMinCardinality { .. } => "DataMinCardinality",
                    CE::DataMaxCardinality { .. } => "DataMaxCardinality",
                    _ => "DataExactCardinality",
                })?;
                self.xml.attr("cardinality", n.to_string());
                self.entity("DataProperty", dp.0.as_ref())?;
                if qualified_data(dr) {
                    self.dr(dr)?;
                }
                self.xml.end()
            }
        }
    }

    fn dr(&mut self, dr: &DR<RcStr>) -> std::io::Result<()> {
        let order = self.order;
        match dr {
            DR::Datatype(d) => self.entity("Datatype", d.0.as_ref()),
            DR::DataIntersectionOf(ops) | DR::DataUnionOf(ops) => {
                self.xml.start(if matches!(dr, DR::DataIntersectionOf(_)) {
                    "DataIntersectionOf"
                } else {
                    "DataUnionOf"
                })?;
                for op in sorted_set(ops, |a, b| order.dr(a, b)) {
                    self.dr(op)?;
                }
                self.xml.end()
            }
            DR::DataComplementOf(op) => {
                self.xml.start("DataComplementOf")?;
                self.dr(op)?;
                self.xml.end()
            }
            DR::DataOneOf(lits) => {
                self.xml.start("DataOneOf")?;
                for l in sorted_set(lits, |a, b| order.literal(a, b)) {
                    self.literal(l)?;
                }
                self.xml.end()
            }
            DR::DatatypeRestriction(dt, facets) => {
                self.xml.start("DatatypeRestriction")?;
                self.entity("Datatype", dt.0.as_ref())?;
                for f in sorted_set(facets, |a, b| order.facet_restriction(a, b)) {
                    self.xml.start("FacetRestriction")?;
                    self.xml.attr("facet", f.f.as_ref().to_string());
                    self.literal(&f.l)?;
                    self.xml.end()?;
                }
                self.xml.end()
            }
        }
    }

    fn annotation_value(&mut self, v: &AnnotationValue<RcStr>) -> std::io::Result<()> {
        match v {
            AnnotationValue::IRI(iri) => self.iri_element(iri.as_ref()),
            AnnotationValue::Literal(l) => self.literal(l),
            AnnotationValue::AnonymousIndividual(a) => self.anonymous(a.0.as_ref()),
        }
    }

    fn annotation(&mut self, a: &Annotation<RcStr>) -> std::io::Result<()> {
        self.xml.start("Annotation")?;
        self.annotations(a.ann.iter())?;
        self.entity("AnnotationProperty", a.ap.0.as_ref())?;
        self.annotation_value(&a.av)?;
        self.xml.end()
    }

    fn annotations<'a>(&mut self, anns: impl IntoIterator<Item = &'a Annotation<RcStr>>) -> std::io::Result<()> {
        let order = self.order;
        for a in order.sorted_annotations(anns) {
            self.annotation(a)?;
        }
        Ok(())
    }

    fn iarg(&mut self, a: &IArgument<RcStr>) -> std::io::Result<()> {
        match a {
            IArgument::Individual(i) => self.individual(i),
            IArgument::Variable(v) => self.variable(v.0.as_ref()),
        }
    }

    fn darg(&mut self, a: &DArgument<RcStr>) -> std::io::Result<()> {
        match a {
            DArgument::Literal(l) => self.literal(l),
            DArgument::Variable(v) => self.variable(v.0.as_ref()),
        }
    }

    /// A rule variable; one in the conventional `urn:swrl#` namespace is written
    /// in `urn:swrl:var#`.
    fn variable(&mut self, iri: &str) -> std::io::Result<()> {
        let (ns, local) = iri_split(iri);
        let iri = if ns == "urn:swrl:var#" || ns == "urn:swrl#" {
            format!("urn:swrl:var#{local}")
        } else {
            iri.to_string()
        };
        self.xml.start("Variable")?;
        self.iri_attr(&iri);
        self.xml.end()
    }

    fn atom(&mut self, atom: &Atom<RcStr>) -> std::io::Result<()> {
        match atom {
            Atom::ClassAtom { pred, arg } => {
                self.xml.start("ClassAtom")?;
                self.ce(pred)?;
                self.iarg(arg)?;
            }
            Atom::DataRangeAtom { pred, arg } => {
                self.xml.start("DataRangeAtom")?;
                self.dr(pred)?;
                self.darg(arg)?;
            }
            Atom::ObjectPropertyAtom { pred, args } => {
                self.xml.start("ObjectPropertyAtom")?;
                self.ope(pred)?;
                self.iarg(&args.0)?;
                self.iarg(&args.1)?;
            }
            Atom::DataPropertyAtom { pred, args } => {
                self.xml.start("DataPropertyAtom")?;
                self.entity("DataProperty", pred.0.as_ref())?;
                self.darg(&args.0)?;
                self.darg(&args.1)?;
            }
            Atom::BuiltInAtom { pred, args } => {
                self.xml.start("BuiltInAtom")?;
                self.iri_attr(pred.as_ref());
                for a in args {
                    self.darg(a)?;
                }
            }
            Atom::SameIndividualAtom(a, b) => {
                self.xml.start("SameIndividualAtom")?;
                self.iarg(a)?;
                self.iarg(b)?;
            }
            Atom::DifferentIndividualsAtom(a, b) => {
                self.xml.start("DifferentIndividualsAtom")?;
                self.iarg(a)?;
                self.iarg(b)?;
            }
        }
        self.xml.end()
    }

    /// An axiom: its element, its annotations, then its operands.
    fn axiom(&mut self, ac: &AnnotatedComponent<RcStr>) -> std::io::Result<()> {
        use Component as C;
        let name = match &ac.component {
            C::DeclareClass(_)
            | C::DeclareObjectProperty(_)
            | C::DeclareDataProperty(_)
            | C::DeclareNamedIndividual(_)
            | C::DeclareAnnotationProperty(_)
            | C::DeclareDatatype(_) => "Declaration",
            C::SubClassOf(_) => "SubClassOf",
            C::EquivalentClasses(_) => "EquivalentClasses",
            C::DisjointClasses(_) => "DisjointClasses",
            C::DisjointUnion(_) => "DisjointUnion",
            C::SubObjectPropertyOf(_) => "SubObjectPropertyOf",
            C::EquivalentObjectProperties(_) => "EquivalentObjectProperties",
            C::DisjointObjectProperties(_) => "DisjointObjectProperties",
            C::InverseObjectProperties(_) => "InverseObjectProperties",
            C::ObjectPropertyDomain(_) => "ObjectPropertyDomain",
            C::ObjectPropertyRange(_) => "ObjectPropertyRange",
            C::FunctionalObjectProperty(_) => "FunctionalObjectProperty",
            C::InverseFunctionalObjectProperty(_) => "InverseFunctionalObjectProperty",
            C::ReflexiveObjectProperty(_) => "ReflexiveObjectProperty",
            C::IrreflexiveObjectProperty(_) => "IrreflexiveObjectProperty",
            C::SymmetricObjectProperty(_) => "SymmetricObjectProperty",
            C::AsymmetricObjectProperty(_) => "AsymmetricObjectProperty",
            C::TransitiveObjectProperty(_) => "TransitiveObjectProperty",
            C::SubDataPropertyOf(_) => "SubDataPropertyOf",
            C::EquivalentDataProperties(_) => "EquivalentDataProperties",
            C::DisjointDataProperties(_) => "DisjointDataProperties",
            C::DataPropertyDomain(_) => "DataPropertyDomain",
            C::DataPropertyRange(_) => "DataPropertyRange",
            C::FunctionalDataProperty(_) => "FunctionalDataProperty",
            C::DatatypeDefinition(_) => "DatatypeDefinition",
            C::HasKey(_) => "HasKey",
            C::SameIndividual(_) => "SameIndividual",
            C::DifferentIndividuals(_) => "DifferentIndividuals",
            C::ClassAssertion(_) => "ClassAssertion",
            C::ObjectPropertyAssertion(_) => "ObjectPropertyAssertion",
            C::NegativeObjectPropertyAssertion(_) => "NegativeObjectPropertyAssertion",
            C::DataPropertyAssertion(_) => "DataPropertyAssertion",
            C::NegativeDataPropertyAssertion(_) => "NegativeDataPropertyAssertion",
            C::AnnotationAssertion(_) => "AnnotationAssertion",
            C::SubAnnotationPropertyOf(_) => "SubAnnotationPropertyOf",
            C::AnnotationPropertyDomain(_) => "AnnotationPropertyDomain",
            C::AnnotationPropertyRange(_) => "AnnotationPropertyRange",
            C::Rule(_) => "DLSafeRule",
            C::OntologyID(_) | C::DocIRI(_) | C::Import(_) | C::OntologyAnnotation(_) => return Ok(()),
        };
        self.xml.start(name)?;
        self.annotations(ac.ann.iter())?;
        match &ac.component {
            C::DeclareClass(e) => self.entity("Class", e.0.as_ref())?,
            C::DeclareObjectProperty(e) => self.entity("ObjectProperty", e.0.as_ref())?,
            C::DeclareDataProperty(e) => self.entity("DataProperty", e.0.as_ref())?,
            C::DeclareNamedIndividual(e) => self.entity("NamedIndividual", e.0.as_ref())?,
            C::DeclareAnnotationProperty(e) => self.entity("AnnotationProperty", e.0.as_ref())?,
            C::DeclareDatatype(e) => self.entity("Datatype", e.0.as_ref())?,
            C::SubClassOf(ax) => {
                self.ce(&ax.sub)?;
                self.ce(&ax.sup)?;
            }
            C::EquivalentClasses(ax) => self.ces(&ax.0)?,
            C::DisjointClasses(ax) => self.ces(&ax.0)?,
            C::DisjointUnion(ax) => {
                self.class(ax.0 .0.as_ref())?;
                self.ces(&ax.1)?;
            }
            C::SubObjectPropertyOf(ax) => {
                match &ax.sub {
                    SOPE::ObjectPropertyExpression(sub) => self.ope(sub)?,
                    SOPE::ObjectPropertyChain(chain) => {
                        self.xml.start("ObjectPropertyChain")?;
                        for p in chain {
                            self.ope(p)?;
                        }
                        self.xml.end()?;
                    }
                }
                self.ope(&ax.sup)?;
            }
            C::EquivalentObjectProperties(ax) => self.opes(&ax.0)?,
            C::DisjointObjectProperties(ax) => self.opes(&ax.0)?,
            C::InverseObjectProperties(ax) => {
                self.ope(&ax.0)?;
                self.ope(&ax.1)?;
            }
            C::ObjectPropertyDomain(ax) => {
                self.ope(&ax.ope)?;
                self.ce(&ax.ce)?;
            }
            C::ObjectPropertyRange(ax) => {
                self.ope(&ax.ope)?;
                self.ce(&ax.ce)?;
            }
            C::FunctionalObjectProperty(ax) => self.ope(&ax.0)?,
            C::InverseFunctionalObjectProperty(ax) => self.ope(&ax.0)?,
            C::ReflexiveObjectProperty(ax) => self.ope(&ax.0)?,
            C::IrreflexiveObjectProperty(ax) => self.ope(&ax.0)?,
            C::SymmetricObjectProperty(ax) => self.ope(&ax.0)?,
            C::AsymmetricObjectProperty(ax) => self.ope(&ax.0)?,
            C::TransitiveObjectProperty(ax) => self.ope(&ax.0)?,
            C::SubDataPropertyOf(ax) => {
                self.entity("DataProperty", ax.sub.0.as_ref())?;
                self.entity("DataProperty", ax.sup.0.as_ref())?;
            }
            C::EquivalentDataProperties(ax) => self.data_properties(ax.0.iter().map(|d| d.0.as_ref()))?,
            C::DisjointDataProperties(ax) => self.data_properties(ax.0.iter().map(|d| d.0.as_ref()))?,
            C::DataPropertyDomain(ax) => {
                self.entity("DataProperty", ax.dp.0.as_ref())?;
                self.ce(&ax.ce)?;
            }
            C::DataPropertyRange(ax) => {
                self.entity("DataProperty", ax.dp.0.as_ref())?;
                self.dr(&ax.dr)?;
            }
            C::FunctionalDataProperty(ax) => self.entity("DataProperty", ax.0 .0.as_ref())?,
            C::DatatypeDefinition(ax) => {
                self.entity("Datatype", ax.kind.0.as_ref())?;
                self.dr(&ax.range)?;
            }
            C::HasKey(ax) => {
                self.ce(&ax.ce)?;
                let opes: Vec<OPE<RcStr>> = ax
                    .vpe
                    .iter()
                    .filter_map(|pe| match pe {
                        PropertyExpression::ObjectPropertyExpression(o) => Some(o.clone()),
                        _ => None,
                    })
                    .collect();
                self.opes(&opes)?;
                self.data_properties(ax.vpe.iter().filter_map(|pe| match pe {
                    PropertyExpression::DataProperty(d) => Some(d.0.as_ref()),
                    _ => None,
                }))?;
            }
            C::SameIndividual(ax) => self.individuals(&ax.0)?,
            C::DifferentIndividuals(ax) => self.individuals(&ax.0)?,
            C::ClassAssertion(ax) => {
                self.ce(&ax.ce)?;
                self.individual(&ax.i)?;
            }
            C::ObjectPropertyAssertion(ax) => {
                self.ope(&ax.ope)?;
                self.individual(&ax.from)?;
                self.individual(&ax.to)?;
            }
            C::NegativeObjectPropertyAssertion(ax) => {
                self.ope(&ax.ope)?;
                self.individual(&ax.from)?;
                self.individual(&ax.to)?;
            }
            C::DataPropertyAssertion(ax) => {
                self.entity("DataProperty", ax.dp.0.as_ref())?;
                self.individual(&ax.from)?;
                self.literal(&ax.to)?;
            }
            C::NegativeDataPropertyAssertion(ax) => {
                self.entity("DataProperty", ax.dp.0.as_ref())?;
                self.individual(&ax.from)?;
                self.literal(&ax.to)?;
            }
            C::AnnotationAssertion(ax) => {
                self.entity("AnnotationProperty", ax.ann.ap.0.as_ref())?;
                match &ax.subject {
                    AnnotationSubject::IRI(iri) => self.iri_element(iri.as_ref())?,
                    AnnotationSubject::AnonymousIndividual(a) => self.anonymous(a.0.as_ref())?,
                }
                self.annotation_value(&ax.ann.av)?;
            }
            C::SubAnnotationPropertyOf(ax) => {
                self.entity("AnnotationProperty", ax.sub.0.as_ref())?;
                self.entity("AnnotationProperty", ax.sup.0.as_ref())?;
            }
            C::AnnotationPropertyDomain(ax) => {
                self.entity("AnnotationProperty", ax.ap.0.as_ref())?;
                self.iri_element(ax.iri.as_ref())?;
            }
            C::AnnotationPropertyRange(ax) => {
                self.entity("AnnotationProperty", ax.ap.0.as_ref())?;
                self.iri_element(ax.iri.as_ref())?;
            }
            C::Rule(rule) => {
                self.xml.start("Body")?;
                for a in &rule.body {
                    self.atom(a)?;
                }
                self.xml.end()?;
                self.xml.start("Head")?;
                for a in &rule.head {
                    self.atom(a)?;
                }
                self.xml.end()?;
            }
            C::OntologyID(_) | C::DocIRI(_) | C::Import(_) | C::OntologyAnnotation(_) => {}
        }
        self.xml.end()
    }
}

/// The element a declaration of an entity of this kind holds.
fn entity_element(kind: Kind) -> &'static str {
    match kind {
        Kind::Class => "Class",
        Kind::ObjectProperty => "ObjectProperty",
        Kind::DataProperty => "DataProperty",
        Kind::NamedIndividual => "NamedIndividual",
        Kind::AnnotationProperty => "AnnotationProperty",
        Kind::Datatype => "Datatype",
    }
}

/// Write `model` as OWL/XML, declaring `prefixes` (name → namespace, the default
/// prefix as the empty name) in the order given.
pub fn save<W: Write>(model: &Model, prefixes: &[(String, String)], w: &mut W) -> Result<()> {
    let order = NaturalOrder::new(model.plain_literals_typed);
    let (iri, viri) = model
        .ont
        .iter()
        .find_map(|ac| match &ac.component {
            Component::OntologyID(id) => Some((
                id.iri.as_ref().map(|i| i.as_ref().to_string()),
                id.viri.as_ref().map(|i| i.as_ref().to_string()),
            )),
            _ => None,
        })
        .unwrap_or((None, None));
    let base = iri.clone().unwrap_or_else(|| OWL_NS.to_string());

    w.write_all(b"<?xml version=\"1.0\"?>\n")?;
    let mut r = Renderer {
        xml: Xml { w, stack: Vec::new() },
        base: base.clone(),
        abbrev: HashMap::new(),
        order,
    };
    r.xml.start("Ontology")?;
    if let Some(root) = r.xml.stack.last_mut() {
        root.wrap = true;
    }
    r.xml.attr("xmlns", OWL_NS);
    r.xml.attr("xml:base", base);
    r.xml.attr("xmlns:rdf", RDF_NS);
    r.xml.attr("xmlns:xml", XML_NS);
    r.xml.attr("xmlns:xsd", XSD_NS);
    r.xml.attr("xmlns:rdfs", RDFS_NS);
    if let Some(i) = &iri {
        r.xml.attr("ontologyIRI", i.clone());
        if let Some(v) = &viri {
            r.xml.attr("versionIRI", v.clone());
        }
    }

    let mut declared_names: Vec<&str> = Vec::new();
    let write_prefix = |r: &mut Renderer<W>, name: &str, ns: &str| -> std::io::Result<()> {
        r.xml.start("Prefix")?;
        r.xml.attr("name", name);
        r.xml.attr("IRI", ns);
        r.xml.end()?;
        r.abbrev.insert(ns.to_string(), format!("{name}:"));
        Ok(())
    };
    for (name, ns) in prefixes {
        if ns.is_empty() {
            continue;
        }
        write_prefix(&mut r, name, ns)?;
        declared_names.push(name.as_str());
    }
    for (name, ns) in [("rdf", RDF_NS), ("rdfs", RDFS_NS), ("xsd", XSD_NS), ("owl", OWL_NS)] {
        if !declared_names.contains(&name) {
            write_prefix(&mut r, name, ns)?;
        }
    }

    let mut imports: Vec<&str> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::Import(i) => Some(i.0.as_ref()),
            _ => None,
        })
        .collect();
    imports.sort_by(|a, b| crate::io::natural_order::iri_cmp(a, b));
    imports.dedup();
    for i in imports {
        r.xml.text_element("Import", i)?;
    }

    let ont_anns: Vec<&Annotation<RcStr>> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::OntologyAnnotation(a) => Some(&a.0),
            _ => None,
        })
        .collect();
    r.annotations(ont_anns)?;

    let mut decls: Vec<&AnnotatedComponent<RcStr>> = Vec::new();
    let mut axioms: Vec<&AnnotatedComponent<RcStr>> = Vec::new();
    for ac in model.ont.iter() {
        match NaturalOrder::axiom_index(&ac.component) {
            0 => {
                let supplied = entities::declaration(&ac.component)
                    .is_some_and(|(kind, iri)| entities::is_materialised(model, kind, iri));
                if !supplied {
                    decls.push(ac);
                }
            }
            99 => {}
            _ => axioms.push(ac),
        }
    }
    decls.sort_by(|a, b| order.axiom(a, b));
    for d in decls {
        r.axiom(d)?;
    }
    for (kind, iri) in entities::undeclared(model) {
        r.xml.start("Declaration")?;
        r.entity(entity_element(kind), &iri)?;
        r.xml.end()?;
    }
    axioms.sort_by(|a, b| order.axiom(a, b));
    for ax in axioms {
        r.axiom(ax)?;
    }

    while !r.xml.stack.is_empty() {
        r.xml.end()?;
    }
    let version = if model.owlapi_456 { "4.5.6" } else { "4.5.29" };
    write!(r.xml.w, "\n\n\n<!-- Generated by the OWL API (version {version}) https://github.com/owlcs/owlapi -->\n\n")?;
    Ok(())
}

// === Reader ==============================================================

/// Whether `s` is an absolute IRI: it opens with a scheme (a letter, then
/// letters, digits, `+`, `-` or `.`, then `:`).
fn has_scheme(s: &str) -> bool {
    let Some(colon) = s.find(':') else { return false };
    let scheme = &s[..colon];
    let mut chars = scheme.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Rewrite an OWL/XML document so that every IRI in it is absolute and written
/// as an `IRI`, with no `Prefix` elements left to expand anything with, and
/// return it together with the prefixes it declared (name → namespace, in
/// document order).
///
/// An `IRI` value (attribute or element) that is not absolute is appended to
/// `xml:base`; an `abbreviatedIRI` value is expanded with its declared prefix —
/// a name without a colon uses the default prefix — and an undeclared prefix is
/// an error.
pub fn normalise_iris(text: &str) -> Result<(String, Vec<(String, String)>)> {
    use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};

    // First pass: the base and the prefixes, which may follow the elements that
    // use them only in a malformed document, but are read up front all the same.
    let mut base: Option<String> = None;
    let mut prefixes: Vec<(String, String)> = Vec::new();
    {
        let mut reader = quick_xml::Reader::from_str(text);
        loop {
            match reader.read_event().context("reading OWL/XML")? {
                Event::Start(e) | Event::Empty(e) => {
                    let local = e.local_name();
                    if base.is_none() && local.as_ref() == b"Ontology" {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"xml:base" {
                                base = Some(a.unescape_value().context("reading xml:base")?.into_owned());
                            }
                        }
                    }
                    if local.as_ref() == b"Prefix" {
                        let mut name = None;
                        let mut iri = None;
                        for a in e.attributes().flatten() {
                            match a.key.local_name().as_ref() {
                                b"name" => name = Some(a.unescape_value()?.into_owned()),
                                b"IRI" => iri = Some(a.unescape_value()?.into_owned()),
                                _ => {}
                            }
                        }
                        if let (Some(n), Some(i)) = (name, iri) {
                            prefixes.push((n, i));
                        }
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }
    let resolve = |v: &str| -> String {
        let v = v.trim();
        if has_scheme(v) {
            v.to_string()
        } else {
            match &base {
                Some(b) => format!("{b}{v}"),
                None => v.to_string(),
            }
        }
    };
    let expand = |v: &str| -> Result<String> {
        let v = v.trim();
        let (name, local) = v.split_once(':').unwrap_or(("", v));
        match prefixes.iter().rev().find(|(n, _)| n == name) {
            Some((_, ns)) => Ok(resolve(&format!("{ns}{local}"))),
            None => bail!("OWL/XML: prefix name not declared: {name}:"),
        }
    };

    let mut reader = quick_xml::Reader::from_str(text);
    let mut out = quick_xml::Writer::new(Vec::with_capacity(text.len()));
    // The IRI element being read, if any: its text is an IRI to resolve or a
    // prefixed name to expand.
    let mut in_iri: Option<bool> = None;
    let mut skip_depth = 0usize;
    loop {
        let event = reader.read_event().context("reading OWL/XML")?;
        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                Event::Eof => break,
                _ => {}
            }
            continue;
        }
        match event {
            Event::Empty(e) if e.local_name().as_ref() == b"Prefix" => continue,
            Event::Start(e) if e.local_name().as_ref() == b"Prefix" => {
                skip_depth = 1;
                continue;
            }
            Event::Start(e) => {
                let local = e.local_name().as_ref().to_vec();
                if local == b"AbbreviatedIRI" || local == b"IRI" {
                    in_iri = Some(local == b"AbbreviatedIRI");
                    out.write_event(Event::Start(BytesStart::new("IRI")))?;
                } else {
                    out.write_event(Event::Start(rewrite_attrs(&e, &resolve, &expand)?))?;
                }
            }
            Event::Empty(e) => {
                let local = e.local_name().as_ref().to_vec();
                if local == b"AbbreviatedIRI" || local == b"IRI" {
                    let value = if local == b"AbbreviatedIRI" { expand("")? } else { resolve("") };
                    out.write_event(Event::Start(BytesStart::new("IRI")))?;
                    out.write_event(Event::Text(BytesText::new(&value)))?;
                    out.write_event(Event::End(BytesEnd::new("IRI")))?;
                } else {
                    out.write_event(Event::Empty(rewrite_attrs(&e, &resolve, &expand)?))?;
                }
            }
            Event::Text(t) if in_iri.is_some() => {
                let raw = t.unescape().context("reading an OWL/XML IRI")?;
                let value = if in_iri == Some(true) { expand(&raw)? } else { resolve(&raw) };
                out.write_event(Event::Text(BytesText::new(&value)))?;
            }
            Event::End(e) => {
                let local = e.local_name().as_ref().to_vec();
                if local == b"AbbreviatedIRI" || local == b"IRI" {
                    in_iri = None;
                    out.write_event(Event::End(BytesEnd::new("IRI")))?;
                } else {
                    out.write_event(Event::End(e))?;
                }
            }
            Event::Eof => break,
            other => out.write_event(other)?,
        }
    }
    let rewritten = String::from_utf8(out.into_inner()).context("OWL/XML is not UTF-8")?;
    Ok((rewritten, prefixes))
}

/// An element's start tag with its IRI-valued attributes made absolute:
/// `abbreviatedIRI` becomes `IRI`, holding the expansion.
fn rewrite_attrs<'a>(
    e: &quick_xml::events::BytesStart<'a>,
    resolve: &dyn Fn(&str) -> String,
    expand: &dyn Fn(&str) -> Result<String>,
) -> Result<quick_xml::events::BytesStart<'static>> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut out = quick_xml::events::BytesStart::new(name);
    for a in e.attributes() {
        let a = a.context("reading an OWL/XML attribute")?;
        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let value = a.unescape_value().context("reading an OWL/XML attribute")?;
        match key.as_str() {
            "IRI" | "URI" => out.push_attribute(("IRI", resolve(&value).as_str())),
            "abbreviatedIRI" => out.push_attribute(("IRI", expand(&value)?.as_str())),
            _ => out.push_attribute((key.as_str(), value.as_ref())),
        }
    }
    Ok(out)
}
