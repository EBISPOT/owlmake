//! The framed Turtle layout for a saved model: a fixed prefix block, an
//! ontology header, entity sections under banner comments, one aligned block
//! per entity, and a closing banner.
//!
//! The layout covers a model made of entity declarations and unannotated
//! annotation assertions on named subjects — the shape a merge of construct
//! outputs produces. `render` returns `None` for anything richer (a named
//! ontology, imports, axioms, punning, assertions on undeclared subjects), and
//! the caller keeps the line-per-triple serializer.

use std::collections::{BTreeMap, BTreeSet};

use horned_owl::model::{AnnotationSubject, AnnotationValue, Component};

use crate::model::Model;

/// The declared prefixes, in the order the block prints them.
const PREFIXES: [(&str, &str); 5] = [
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("xml", "http://www.w3.org/XML/1998/namespace"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
];

const BANNER: &str = "#################################################################";

/// Section index per entity kind, in printed order.
const SECTIONS: [&str; 6] = [
    "Annotation properties",
    "Datatypes",
    "Object Properties",
    "Data properties",
    "Classes",
    "Individuals",
];

/// The `owl:` type an entity of each section declares.
const KINDS: [&str; 6] = [
    "owl:AnnotationProperty",
    "rdfs:Datatype",
    "owl:ObjectProperty",
    "owl:DatatypeProperty",
    "owl:Class",
    "owl:NamedIndividual",
];

#[derive(Default)]
struct Entity {
    /// Section the entity's declaration puts it in.
    section: Option<usize>,
    /// predicate IRI -> sorted rendered objects
    anns: BTreeMap<String, BTreeSet<(u8, String, String)>>,
}

/// An IRI as the layout prints it: prefixed when a declared prefix covers it
/// and the remainder is a plain local name, framed otherwise.
fn iri(u: &str) -> String {
    for (p, ns) in PREFIXES {
        if let Some(local) = u.strip_prefix(ns) {
            if !local.is_empty()
                && local.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return format!("{p}:{local}");
            }
        }
    }
    format!("<{u}>")
}

fn literal(lit: &horned_owl::model::Literal<crate::model::Str>) -> String {
    use horned_owl::model::Literal as L;
    let escape = |s: &str| -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(c),
            }
        }
        out
    };
    match lit {
        L::Simple { literal } => format!("\"{}\"", escape(literal)),
        L::Language { literal, lang } => format!("\"{}\"@{lang}", escape(literal)),
        L::Datatype { literal, datatype_iri } => {
            format!("\"{}\"^^{}", escape(literal), iri(datatype_iri.as_ref()))
        }
    }
}

/// Render the model in the framed layout, or `None` when it holds anything the
/// layout does not state.
pub fn render(model: &Model) -> Option<Vec<u8>> {
    use Component as C;
    let mut entities: BTreeMap<String, Entity> = BTreeMap::new();
    // A second declaration puns the entity across sections, which the layout
    // does not state.
    fn declare(
        entities: &mut BTreeMap<String, Entity>,
        iri: &str,
        section: usize,
    ) -> Option<()> {
        let e = entities.entry(iri.to_string()).or_default();
        if e.section.replace(section).is_some() {
            return None;
        }
        Some(())
    }
    for ac in model.ont.iter() {
        if !ac.ann.is_empty() {
            return None;
        }
        match &ac.component {
            // Only an anonymous ontology header is stated by this layout.
            C::OntologyID(id) => {
                if id.iri.is_some() || id.viri.is_some() {
                    return None;
                }
            }
            C::DeclareAnnotationProperty(d) => declare(&mut entities, d.0 .0.as_ref(), 0)?,
            C::DeclareDatatype(d) => declare(&mut entities, d.0 .0.as_ref(), 1)?,
            C::DeclareObjectProperty(d) => declare(&mut entities, d.0 .0.as_ref(), 2)?,
            C::DeclareDataProperty(d) => declare(&mut entities, d.0 .0.as_ref(), 3)?,
            C::DeclareClass(d) => declare(&mut entities, d.0 .0.as_ref(), 4)?,
            C::DeclareNamedIndividual(d) => declare(&mut entities, d.0 .0.as_ref(), 5)?,
            C::AnnotationAssertion(aa) => {
                let AnnotationSubject::IRI(subject) = &aa.subject else { return None };
                let (rank, key, rendered) = match &aa.ann.av {
                    AnnotationValue::IRI(v) => {
                        (0u8, v.as_ref().to_string(), iri(v.as_ref()))
                    }
                    AnnotationValue::Literal(l) => (1u8, l.literal().clone(), literal(l)),
                    AnnotationValue::AnonymousIndividual(_) => return None,
                };
                entities
                    .entry(subject.as_ref().to_string())
                    .or_default()
                    .anns
                    .entry(aa.ann.ap.0.as_ref().to_string())
                    .or_default()
                    .insert((rank, key, rendered));
            }
            _ => return None,
        }
    }
    // An assertion on an undeclared subject has no section to sit in.
    if entities.values().any(|e| e.section.is_none()) {
        return None;
    }
    if entities.is_empty() {
        return None;
    }

    let mut out = String::new();
    for (p, ns) in PREFIXES {
        out.push_str(&format!("@prefix {p}: <{ns}> .\n"));
    }
    out.push_str("@base <http://www.w3.org/2002/07/owl#> .\n\n");
    out.push_str("[ rdf:type owl:Ontology\n ] .\n\n");

    for (section, title) in SECTIONS.iter().enumerate() {
        let members: Vec<(&String, &Entity)> =
            entities.iter().filter(|(_, e)| e.section == Some(section)).collect();
        if members.is_empty() {
            continue;
        }
        out.push_str(&format!("{BANNER}\n#    {title}\n{BANNER}\n\n"));
        for (subject, e) in members {
            out.push_str(&format!("###  {subject}\n"));
            let subj = iri(subject);
            let pred_col = subj.chars().count() + 1;
            out.push_str(&format!("{subj} rdf:type {}", KINDS[section]));
            let mut anns = e.anns.iter().peekable();
            if anns.peek().is_some() {
                out.push_str(" ;");
            }
            while let Some((pred, values)) = anns.next() {
                let pred = iri(pred);
                let obj_col = pred_col + pred.chars().count() + 1;
                out.push('\n');
                out.push_str(&" ".repeat(pred_col));
                out.push_str(&pred);
                let mut vals = values.iter().peekable();
                let mut first = true;
                while let Some((_, _, rendered)) = vals.next() {
                    if first {
                        out.push(' ');
                        first = false;
                    } else {
                        out.push('\n');
                        out.push_str(&" ".repeat(obj_col));
                    }
                    out.push_str(rendered);
                    if vals.peek().is_some() {
                        out.push_str(" ,");
                    }
                }
                if anns.peek().is_some() {
                    out.push_str(" ;");
                }
            }
            out.push_str(" .\n\n\n");
        }
    }
    out.push_str("###  Generated by the OWL API (version 4.5.29) https://github.com/owlcs/owlapi\n");
    Some(out.into_bytes())
}
