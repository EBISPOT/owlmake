//! `annotate` — add ontology-level annotations and set the ontology/version IRI,
//! the provenance every released ontology file is expected to carry.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationProperty, AnnotationSubject,
    AnnotationValue, Component, Kinded, Literal, MutableOntology, OntologyID,
};

const RDFS_IS_DEFINED_BY: &str = "http://www.w3.org/2000/01/rdf-schema#isDefinedBy";
const PROV_WAS_DERIVED_FROM: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    // NOTE: `-f` in this command is `--annotate-derived-from`, so `--format` is
    // long-only here rather than colliding with it.
    #[arg(long)]
    pub format: Option<String>,

    /// Set the ontology IRI.
    #[arg(short = 'O', long)]
    pub ontology_iri: Option<String>,
    /// Set the version IRI.
    #[arg(short = 'V', long)]
    pub version_iri: Option<String>,
    /// Add an ontology annotation as `PROP VALUE` (literal value).
    /// May be repeated. (Also spelled `--annotate`.)
    #[arg(short = 'a', long, visible_alias = "annotate", num_args = 2, value_names = ["PROP", "VALUE"])]
    pub annotation: Vec<String>,
    /// Add an ontology annotation as `PROP IRI` (IRI value). May be repeated.
    #[arg(short = 'k', long, num_args = 2, value_names = ["PROP", "IRI"])]
    pub link_annotation: Vec<String>,
    /// Annotate the axioms with `PROP VALUE` (literal value). Each occurrence
    /// takes three values, and the values of every occurrence are read in
    /// `PROP VALUE` pairs. Each pair replaces the annotations of every
    /// `SubClassOf`; an ontology with an axiom of any other type is refused.
    #[arg(short = 'x', long, num_args = 3, value_names = ["PROP", "VALUE", "PROP"])]
    pub axiom_annotation: Vec<String>,
    /// Add an ontology annotation with a language-tagged literal as
    /// `PROP VALUE LANG`. May be repeated.
    #[arg(short = 'l', long, num_args = 3, value_names = ["PROP", "VALUE", "LANG"])]
    pub language_annotation: Vec<String>,
    /// Add an ontology annotation with a typed literal as `PROP VALUE TYPE`
    /// (TYPE is a datatype CURIE/IRI). May be repeated.
    #[arg(short = 't', long, num_args = 3, value_names = ["PROP", "VALUE", "TYPE"])]
    pub typed_annotation: Vec<String>,
    /// Merge the axioms and ontology annotations of an ontology file, but not
    /// its imports. May be repeated.
    #[arg(short = 'A', long, value_name = "FILE")]
    pub annotation_file: Vec<PathBuf>,
    /// Assert `rdfs:isDefinedBy` the ontology IRI of every entity of the
    /// signature outside the OWL, RDF, RDFS and XSD vocabularies that has no
    /// `rdfs:isDefinedBy` (`<bool>`, default false).
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub annotate_defined_by: Option<bool>,
    /// Annotate every axiom with no `prov:wasDerivedFrom` with one naming the
    /// version IRI, or the ontology IRI where there is no version IRI
    /// (`<bool>`, default false).
    #[arg(short = 'f', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub annotate_derived_from: Option<bool>,
    /// Remove all existing ontology annotations first.
    #[arg(short = 'R', long)]
    pub remove_annotations: bool,
    /// If true, replace `%{ontology_iri}` and `%{version_iri}` in each annotation
    /// value with the ontology's IRI and version IRI. `<bool>`.
    #[arg(short = 'e', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub interpolate: Option<bool>,

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
    // The annotated document is written among the ontologies it imports, which
    // the load reads.
    let mut model = crate::cmd::take_or_load_no_imports(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut model = annotate_with(
        model,
        &AnnotateOptions {
            ontology_iri: args.ontology_iri.clone(),
            version_iri: args.version_iri.clone(),
            annotation: args.annotation.clone(),
            link_annotation: args.link_annotation.clone(),
            language_annotation: args.language_annotation.clone(),
            typed_annotation: args.typed_annotation.clone(),
            axiom_annotation: args.axiom_annotation.clone(),
            annotation_file: args.annotation_file.clone(),
            annotate_defined_by: args.annotate_defined_by.unwrap_or(false),
            annotate_derived_from: args.annotate_derived_from.unwrap_or(false),
            remove_annotations: args.remove_annotations,
            interpolate: args.interpolate.unwrap_or(false),
        },
    )?;
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// The literal `--annotation` and `--axiom-annotation` make of a value: typed
/// `xsd:string`, as a data factory makes one from text alone.
fn string_literal(model: &crate::model::Model, value: &str) -> Literal<crate::model::Str> {
    Literal::Datatype {
        literal: value.to_string(),
        datatype_iri: model.build.iri("http://www.w3.org/2001/XMLSchema#string"),
    }
}

/// The ontology's IRI and version IRI, where it has them.
pub(crate) fn ontology_iris(model: &crate::model::Model) -> (Option<String>, Option<String>) {
    model
        .ont
        .iter()
        .find_map(|ac| match &ac.component {
            Component::OntologyID(id) => Some((
                id.iri.as_ref().map(|i| i.as_ref().to_string()),
                id.viri.as_ref().map(|i| i.as_ref().to_string()),
            )),
            _ => None,
        })
        .unwrap_or((None, None))
}

/// `value` with `%{ontology_iri}` and `%{version_iri}` replaced by the IRIs
/// given; a placeholder whose IRI the ontology lacks, and any other `%{…}`, stays
/// as written.
fn interpolated(value: &str, ontology: Option<&str>, version: Option<&str>) -> String {
    let mut value = value.to_string();
    if let Some(iri) = ontology {
        value = value.replace("%{ontology_iri}", iri);
    }
    if let Some(iri) = version {
        value = value.replace("%{version_iri}", iri);
    }
    value
}
/// The options of `annotate`. Pairs and triples are flattened in the order
/// they are given. Defaults are empty / false so callers can set only what they
/// need.
#[derive(Default)]
pub struct AnnotateOptions {
    pub ontology_iri: Option<String>,
    pub version_iri: Option<String>,
    /// `PROP VALUE` pairs.
    pub annotation: Vec<String>,
    /// `PROP IRI` pairs.
    pub link_annotation: Vec<String>,
    /// `PROP VALUE LANG` triples.
    pub language_annotation: Vec<String>,
    /// `PROP VALUE TYPE` triples.
    pub typed_annotation: Vec<String>,
    /// `PROP VALUE` pairs.
    pub axiom_annotation: Vec<String>,
    pub annotation_file: Vec<PathBuf>,
    pub annotate_defined_by: bool,
    pub annotate_derived_from: bool,
    pub remove_annotations: bool,
    /// Replace `%{ontology_iri}` and `%{version_iri}` in each value with the
    /// IRIs the ontology has before these options set them.
    pub interpolate: bool,
}

impl AnnotateOptions {
    /// Whether these options ask for any change. `interpolate` alone does not.
    fn ask_for_anything(&self) -> bool {
        self.remove_annotations
            || !self.annotation.is_empty()
            || !self.link_annotation.is_empty()
            || !self.language_annotation.is_empty()
            || !self.typed_annotation.is_empty()
            || !self.axiom_annotation.is_empty()
            || !self.annotation_file.is_empty()
            || self.ontology_iri.is_some()
            || self.version_iri.is_some()
            || self.annotate_derived_from
            || self.annotate_defined_by
    }
}

/// Narrow entry point for the common case: set the ontology/version IRI and add
/// the literal and link ontology annotations, leaving every other option at its
/// default.
pub fn annotate(
    model: crate::model::Model,
    ontology_iri: Option<&str>,
    version_iri: Option<&str>,
    annotation: &[String],
    link_annotation: &[String],
    remove_annotations: bool,
) -> Result<crate::model::Model> {
    annotate_with(
        model,
        &AnnotateOptions {
            ontology_iri: ontology_iri.map(str::to_string),
            version_iri: version_iri.map(str::to_string),
            annotation: annotation.to_vec(),
            link_annotation: link_annotation.to_vec(),
            remove_annotations,
            ..Default::default()
        },
    )
}

/// Apply `opts` to `model`, in this order: remove the ontology annotations; add
/// the literal, link, language-tagged and typed ontology annotations; annotate
/// the axioms; merge the annotation files; set the ontology and version IRIs;
/// annotate every axiom with what it is derived from; and assert what defines
/// every entity. Options that ask for no change are refused.
pub fn annotate_with(
    mut model: crate::model::Model,
    opts: &AnnotateOptions,
) -> Result<crate::model::Model> {
    if !opts.ask_for_anything() {
        bail!("MISSING ANNOTATION ERROR at least one annotation option or annotation file is required");
    }
    if opts.remove_annotations {
        let header: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| matches!(ac.component, Component::OntologyAnnotation(_)))
            .cloned()
            .collect();
        for ac in header {
            model.ont.remove(&ac);
        }
    }

    // `interpolate` names the IRIs the ontology has as it is read, before the
    // ontology and version IRIs below change them.
    let (ontology, version) = ontology_iris(&model);
    let value = |v: &str| -> String {
        if opts.interpolate {
            interpolated(v, ontology.as_deref(), version.as_deref())
        } else {
            v.to_string()
        }
    };

    for pair in opts.annotation.chunks(2) {
        let [prop, v] = pair else {
            bail!("ANNOTATION FORMAT ERROR each annotation must include PROP VALUE")
        };
        let ap = annotation_property(&model, prop)?;
        let literal = string_literal(&model, &value(v));
        add_ontology_annotation(&mut model, ap, AnnotationValue::Literal(literal));
    }
    for pair in opts.link_annotation.chunks(2) {
        let [prop, v] = pair else {
            bail!("ANNOTATION FORMAT ERROR each link annotation must include PROP LINK")
        };
        let ap = annotation_property(&model, prop)?;
        let link = model.build.iri(iri(&model, &value(v), "value")?.as_str());
        add_ontology_annotation(&mut model, ap, AnnotationValue::IRI(link));
    }
    for triple in opts.language_annotation.chunks(3) {
        let [prop, v, lang] = triple else {
            bail!("ANNOTATION FORMAT ERROR each language annotation must include PROP VALUE LANG")
        };
        let ap = annotation_property(&model, prop)?;
        let literal = crate::model::literal_as_made(Literal::Language { literal: value(v), lang: lang.clone() });
        add_ontology_annotation(&mut model, ap, AnnotationValue::Literal(literal));
    }
    for triple in opts.typed_annotation.chunks(3) {
        let [prop, v, ty] = triple else {
            bail!("ANNOTATION FORMAT ERROR each typed annotation must include PROP VALUE TYPE")
        };
        let ap = annotation_property(&model, prop)?;
        let literal = crate::model::literal_as_made(Literal::Datatype {
            literal: value(v),
            datatype_iri: model.build.iri(iri(&model, ty, "datatype")?.as_str()),
        });
        add_ontology_annotation(&mut model, ap, AnnotationValue::Literal(literal));
    }

    // Each axiom annotation replaces the annotations of every `SubClassOf`, and
    // annotates nothing else: an ontology with an axiom of another type is
    // refused.
    for pair in opts.axiom_annotation.chunks(2) {
        let [prop, v] = pair else {
            bail!("ANNOTATION FORMAT ERROR each axiom annotation must include PROP VALUE")
        };
        let annotation = Annotation {
            ann: BTreeSet::new(),
            ap: annotation_property(&model, prop)?,
            av: AnnotationValue::Literal(string_literal(&model, &value(v))),
        };
        let axioms: Vec<_> = model.ont.iter().filter(|ac| own_axiom(&model, ac)).cloned().collect();
        if let Some(other) = axioms.iter().find(|ac| !matches!(ac.component, Component::SubClassOf(_))) {
            bail!("AXIOM TYPE ERROR cannot annotate axioms of type: {:?}", other.component.kind());
        }
        for ac in axioms {
            model.ont.remove(&ac);
            model.ont.insert(AnnotatedComponent { component: ac.component, ann: BTreeSet::from([annotation.clone()]) });
        }
    }

    // An annotation file lends its own axioms and ontology annotations, not its
    // imports or its name.
    for file in &opts.annotation_file {
        let other = crate::io::load(file)?;
        let lent: Vec<_> = other
            .ont
            .iter()
            .filter(|ac| {
                !matches!(ac.component, Component::OntologyID(_) | Component::DocIRI(_) | Component::Import(_))
                    && !other.imported_components.contains(*ac)
            })
            .cloned()
            .collect();
        for ac in lent {
            model.ont.insert(ac);
        }
    }

    // Set ontology / version IRI by replacing the OntologyID component; an IRI
    // not given keeps its value.
    if opts.ontology_iri.is_some() || opts.version_iri.is_some() {
        let existing = model.ont.iter().find_map(|ac| match &ac.component {
            Component::OntologyID(id) => Some(id.clone()),
            _ => None,
        });
        let existing = existing.unwrap_or(OntologyID { iri: None, viri: None });
        let id = crate::model::ontology_id(
            &model.build,
            opts.ontology_iri.as_deref().or(existing.iri.as_deref()),
            opts.version_iri.as_deref().or(existing.viri.as_deref()),
        )?;
        let old: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| matches!(ac.component, Component::OntologyID(_)))
            .cloned()
            .collect();
        for ac in old {
            model.ont.remove(&ac);
        }
        model.ont.insert(Component::OntologyID(id));
    }

    // Every axiom with no `prov:wasDerivedFrom` gets one naming the version IRI,
    // or the ontology IRI where there is none.
    if opts.annotate_derived_from {
        let (ontology, version) = ontology_iris(&model);
        match version.or(ontology) {
            Some(source) => {
                let ap = model.build.annotation_property(PROV_WAS_DERIVED_FROM);
                let annotation = Annotation {
                    ann: BTreeSet::new(),
                    ap: ap.clone(),
                    av: AnnotationValue::IRI(model.build.iri(source.as_str())),
                };
                let axioms: Vec<_> = model
                    .ont
                    .iter()
                    .filter(|ac| own_axiom(&model, ac) && !ac.ann.iter().any(|a| a.ap == ap))
                    .cloned()
                    .collect();
                for mut ac in axioms {
                    model.ont.remove(&ac);
                    ac.ann.insert(annotation.clone());
                    model.ont.insert(ac);
                }
            }
            None => status!("annotate: --annotate-derived-from: the ontology has no IRI"),
        }
    }

    // Every entity of the signature outside the reserved vocabularies that has
    // no `rdfs:isDefinedBy` is defined by the ontology.
    if opts.annotate_defined_by {
        match ontology_iris(&model).0 {
            Some(ontology) => {
                let ap = model.build.annotation_property(RDFS_IS_DEFINED_BY);
                let defined: HashSet<String> = model
                    .ont
                    .iter()
                    .filter(|ac| !model.imported_components.contains(*ac))
                    .filter_map(|ac| match &ac.component {
                        Component::AnnotationAssertion(AnnotationAssertion {
                            subject: AnnotationSubject::IRI(subject),
                            ann,
                        }) if ann.ap == ap => Some(subject.as_ref().to_string()),
                        _ => None,
                    })
                    .collect();
                let entities: BTreeSet<String> = crate::io::entities::root_signature(&model)
                    .into_iter()
                    .map(|(_, iri)| iri)
                    .filter(|iri| !reserved(iri) && !defined.contains(iri))
                    .collect();
                let target = model.build.iri(ontology.as_str());
                for entity in entities {
                    model.ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
                        subject: AnnotationSubject::IRI(model.build.iri(entity.as_str())),
                        ann: Annotation {
                            ann: BTreeSet::new(),
                            ap: ap.clone(),
                            av: AnnotationValue::IRI(target.clone()),
                        },
                    }));
                }
            }
            None => status!("annotate: --annotate-defined-by: the ontology has no IRI"),
        }
    }

    Ok(model)
}

/// The annotation property `prop` names.
fn annotation_property(
    model: &crate::model::Model,
    prop: &str,
) -> Result<AnnotationProperty<crate::model::Str>> {
    Ok(model.build.annotation_property(iri(model, prop, "property")?.as_str()))
}

/// Add an ontology annotation, as the ontology adds one to those it holds. Its
/// property is not declared: a written document declares the properties the
/// ontology uses and does not declare.
fn add_ontology_annotation(
    model: &mut crate::model::Model,
    ap: AnnotationProperty<crate::model::Str>,
    av: AnnotationValue<crate::model::Str>,
) {
    crate::owlapi_annotations::add(&mut model.ont, [Annotation { ann: BTreeSet::new(), ap, av }]);
}

/// Whether `ac` is an axiom of the ontology's own: not its name, a header
/// annotation or an import, and not lent by an import.
fn own_axiom(model: &crate::model::Model, ac: &AnnotatedComponent<crate::model::Str>) -> bool {
    !matches!(
        ac.component,
        Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_) | Component::Import(_)
    ) && !model.imported_components.contains(ac)
}

/// Whether `iri` belongs to the OWL, RDF, RDFS or XSD vocabulary: its namespace,
/// what precedes its longest NCName suffix, is one of theirs.
pub(crate) fn reserved(iri: &str) -> bool {
    matches!(crate::owlapi_hash::iri_split(iri).0, OWL | RDF | RDFS | XSD)
}

/// The IRI `term` names, read with the command line's context
/// ([`crate::context`]), where `dc` is dc/TERMS/ and a document's own prefixes
/// play no part. A term that names none is refused, naming the `field` it was
/// given for.
fn iri(model: &crate::model::Model, term: &str, field: &str) -> anyhow::Result<String> {
    crate::cmd::select::iri(model, term)
        .ok_or_else(|| anyhow::anyhow!("INVALID IRI ERROR {field} \"{term}\" is not a valid CURIE or IRI"))
}
