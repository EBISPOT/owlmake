//! `expand` — expand OBO/OWL macros.
//!
//! Handles the `oboInOwl`/IAO macro mechanism: an object property `P` annotated
//! with `IAO:0000424` (expandExpressionTo) carries a Manchester-template value
//! using `?Y` for the filler. Each `X SubClassOf (P some Z)` then expands by
//! substituting `?Y := Z` into the template. The supported template forms are
//! `?Y` and `REL some ?Y` (optionally conjoined with `and`), which cover the
//! common GCI macros.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationSubject, AnnotationValue, ClassExpression as CE,
    Component, Literal, MutableOntology, ObjectPropertyExpression as OPE, RcStr, SubClassOf,
};

use crate::cmd::select;
use crate::model::Model;

const IAO_EXPAND_EXPR: &str = "http://purl.obolibrary.org/obo/IAO_0000424";
/// The second macro mechanism: an entity annotated with `OMO_0002000` ("is expanded
/// by"/defined-by-construct) carries a SPARQL CONSTRUCT query whose results are added
/// as axioms.
const OMO_EXPAND_CONSTRUCT: &str = "http://purl.obolibrary.org/obo/OMO_0002000";
const DCT_SOURCE: &str = "http://purl.org/dc/terms/source";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,

    /// Macro property to expand. Repeatable. When any are given, only these
    /// properties' macros are expanded.
    #[arg(short = 't', long = "expand-term", value_name = "TERM")]
    pub expand_term: Vec<String>,
    /// File(s) listing macro properties to expand.
    #[arg(short = 'T', long = "expand-term-file", value_name = "FILE")]
    pub expand_term_file: Vec<PathBuf>,
    /// Macro property to NOT expand. Repeatable.
    #[arg(short = 'n', long = "no-expand-term", value_name = "TERM")]
    pub no_expand_term: Vec<String>,
    /// File(s) listing macro properties to NOT expand.
    #[arg(short = 'N', long = "no-expand-term-file", value_name = "FILE")]
    pub no_expand_term_file: Vec<PathBuf>,
    /// If true, output ontology will only contain the expansions.
    /// `<bool>`.
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true")]
    pub create_new_ontology: Option<bool>,
    /// If true, annotate each expansion axiom with `dct:source <expansion
    /// property>`. `<bool>`.
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true")]
    pub annotate_expansion_axioms: Option<bool>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // Which macro properties to expand: an optional allow-list (--expand-term) and
    // a deny-list (--no-expand-term), both CURIE-expanded.
    let include = select::collect_terms(&model, &args.expand_term, &args.expand_term_file)?;
    let exclude = select::collect_terms(&model, &args.no_expand_term, &args.no_expand_term_file)?;

    // Collect macros: property IRI -> template string.
    let mut macros: HashMap<String, String> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == IAO_EXPAND_EXPR {
                if let (AnnotationSubject::IRI(p), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    let prop = p.as_ref().to_string();
                    if !include.is_empty() && !include.contains(&prop) {
                        continue;
                    }
                    if exclude.contains(&prop) {
                        continue;
                    }
                    macros.insert(prop, literal_text(lit));
                }
            }
        }
    }

    // Collect OMO_0002000 SPARQL-CONSTRUCT macros: subject IRI -> query string.
    let mut construct_macros: Vec<(String, String)> = Vec::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == OMO_EXPAND_CONSTRUCT {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    let subj = s.as_ref().to_string();
                    if !include.is_empty() && !include.contains(&subj) {
                        continue;
                    }
                    if exclude.contains(&subj) {
                        continue;
                    }
                    construct_macros.push((subj, literal_text(lit)));
                }
            }
        }
    }

    if macros.is_empty() && construct_macros.is_empty() {
        status!(
            "expand: no IAO:0000424 or OMO:0002000 macros to expand (after term filtering)"
        );
    }

    // Find X SubClassOf (P some Z) where P is a macro property, and expand. Each
    // generated axiom remembers its source macro property (for --annotate-…).
    let mut to_add: Vec<(Component<RcStr>, String)> = Vec::new();

    // Run each OMO_0002000 SPARQL CONSTRUCT against the ontology and fold the
    // resulting triples back in as OWL axioms.
    if !construct_macros.is_empty() {
        let q = crate::sparql::Queryable::from_model(&model)?;
        for (subj, query) in &construct_macros {
            let rdf = match q.construct(query, oxigraph::io::RdfFormat::RdfXml) {
                Ok(bytes) => bytes,
                Err(e) => {
                    status!("expand: skipping OMO:0002000 construct on <{subj}>: {e}");
                    continue;
                }
            };
            match parse_constructed(&rdf) {
                Ok(constructed) => {
                    for ac in constructed.ont.iter() {
                        if is_skippable(&ac.component) {
                            continue;
                        }
                        to_add.push((ac.component.clone(), subj.clone()));
                    }
                }
                Err(e) => {
                    status!("expand: could not parse construct output for <{subj}>: {e}");
                }
            }
        }
    }
    // rdfs:label → IRI, so a macro template referencing a relation by quoted
    // label (`'part of' some ?Y`) resolves to the property IRI.
    let label_to_iri = label_to_iri_map(&model);
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (sub_ce, CE::ObjectSomeValuesFrom { ope, bce }) = (&sc.sub, &sc.sup) {
                if let (OPE::ObjectProperty(p), CE::Class(z)) = (ope, bce.as_ref()) {
                    if let Some(tmpl) = macros.get(p.0.as_ref()) {
                        if let Some(expanded) =
                            instantiate(&model, &label_to_iri, tmpl, z.0.as_ref())
                        {
                            to_add.push((
                                Component::SubClassOf(SubClassOf {
                                    sub: sub_ce.clone(),
                                    sup: expanded,
                                }),
                                p.0.as_ref().to_string(),
                            ));
                        }
                    }
                }
            }
        }
    }

    let create_new = args.create_new_ontology.unwrap_or(false);
    let annotate = args.annotate_expansion_axioms.unwrap_or(false);
    // Build a `dct:source <macro property>` annotation for an expansion axiom.
    // Use a dedicated Build so it does not conflict with mutable inserts into the
    // model below (separate Build instances combine without consequence).
    let ann_build = horned_owl::model::Build::new();
    let make_annotated = |c: Component<RcStr>, src: &str| -> AnnotatedComponent<RcStr> {
        let ann = Annotation { ann: Default::default(),
            ap: ann_build.annotation_property(DCT_SOURCE),
            av: AnnotationValue::IRI(ann_build.iri(src)),
        };
        let mut anns = std::collections::BTreeSet::new();
        anns.insert(ann);
        AnnotatedComponent { component: c, ann: anns }
    };

    if create_new {
        // Output only the expansion axioms, in a fresh ontology carrying prefixes.
        use horned_owl::ontology::set::SetOntology;
        let mut ont = SetOntology::new();
        let mut added = 0;
        for (c, src) in to_add {
            let inserted = if annotate {
                ont.insert(make_annotated(c, &src))
            } else {
                ont.insert(c)
            };
            if inserted {
                added += 1;
            }
        }
        status!(
            "expand: created new ontology with {added} expanded axiom(s) from {} macro(s)",
            macros.len() + construct_macros.len()
        );
        let mut result = Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes));
        crate::cmd::maybe_save(&mut result, args.output.as_deref(), args.format.as_deref())?;
        return Ok(Some(result));
    }

    let mut added = 0;
    for (c, src) in to_add {
        let inserted = if annotate {
            model.ont.insert(make_annotated(c, &src))
        } else {
            model.ont.insert(c)
        };
        if inserted {
            added += 1;
        }
    }
    status!(
        "expand: added {added} expanded axiom(s) from {} macro(s)",
        macros.len() + construct_macros.len()
    );

    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Instantiate a macro template, substituting `?Y` with the filler `z`. Returns
/// the resulting class expression, or None if the template is unsupported.
/// Build an rdfs:label → entity-IRI map from the model.
fn label_to_iri_map(model: &Model) -> HashMap<String, String> {
    const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
    let mut map = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    map.insert(literal_text(lit), s.as_ref().to_string());
                }
            }
        }
    }
    map
}

fn instantiate(
    model: &Model,
    label_to_iri: &HashMap<String, String>,
    template: &str,
    z: &str,
) -> Option<CE<RcStr>> {
    let b = &model.build;
    // Conjunction of clauses joined by " and ".
    let clauses: Vec<&str> = template.split(" and ").map(|s| s.trim()).collect();
    let mut parts: Vec<CE<RcStr>> = Vec::new();
    for clause in clauses {
        let part = if clause == "?Y" {
            CE::Class(b.class(z))
        } else if let Some(rel) = clause.strip_suffix("some ?Y").map(|s| s.trim().to_string()) {
            let rel = rel.trim().trim_matches('\'');
            // Resolve the relation by rdfs:label first (Manchester quoted-label
            // form), then fall back to CURIE/IRI expansion.
            let rel_iri = label_to_iri.get(rel).cloned().unwrap_or_else(|| select::expand(model, rel));
            CE::ObjectSomeValuesFrom {
                ope: OPE::ObjectProperty(b.object_property(rel_iri.as_str())),
                bce: Box::new(CE::Class(b.class(z))),
            }
        } else {
            return None; // unsupported template form
        };
        parts.push(part);
    }
    match parts.len() {
        0 => None,
        1 => Some(parts.into_iter().next().unwrap()),
        _ => Some(CE::ObjectIntersectionOf(parts)),
    }
}

/// Parse RDF/XML bytes (the output of a CONSTRUCT) into a Model by round-tripping
/// through a temp file, so the triples are mapped to OWL axioms by the RDF reader.
fn parse_constructed(rdf: &[u8]) -> anyhow::Result<Model> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = crate::time::SystemTime::now()
        .duration_since(crate::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!("owlmake-expand-{}-{nanos}-{n}.owl", std::process::id()));
    {
        let mut f = std::fs::File::create(&path)?;
        f.write_all(rdf)?;
    }
    let model = crate::io::load(&path);
    let _ = std::fs::remove_file(&path);
    model
}

/// Components from a constructed graph that should not be folded back into the
/// ontology (document/ontology metadata and imports).
fn is_skippable(c: &Component<RcStr>) -> bool {
    matches!(
        c,
        Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::Import(_)
            | Component::OntologyAnnotation(_)
    )
}

fn literal_text(lit: &Literal<RcStr>) -> String {
    match lit {
        Literal::Simple { literal }
        | Literal::Language { literal, .. }
        | Literal::Datatype { literal, .. } => literal.clone(),
    }
}
