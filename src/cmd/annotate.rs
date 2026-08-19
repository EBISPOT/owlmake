//! `annotate` — add ontology-level annotations and set the ontology/version IRI,
//! the provenance every released ontology file is expected to carry.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue,
    Component, DeclareAnnotationProperty, Literal, MutableOntology, OntologyID,
};

const RDFS_IS_DEFINED_BY: &str = "http://www.w3.org/2000/01/rdf-schema#isDefinedBy";
const PROV_WAS_DERIVED_FROM: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";

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
    /// Annotate every axiom in the ontology with `PROP VALUE` (literal value).
    /// May be repeated.
    #[arg(short = 'x', long, num_args = 2, value_names = ["PROP", "VALUE"])]
    pub axiom_annotation: Vec<String>,
    /// Add an ontology annotation with a language-tagged literal as
    /// `PROP VALUE LANG`. May be repeated.
    #[arg(short = 'l', long, num_args = 3, value_names = ["PROP", "VALUE", "LANG"])]
    pub language_annotation: Vec<String>,
    /// Add an ontology annotation with a typed literal as `PROP VALUE TYPE`
    /// (TYPE is a datatype CURIE/IRI). May be repeated.
    #[arg(short = 't', long, num_args = 3, value_names = ["PROP", "VALUE", "TYPE"])]
    pub typed_annotation: Vec<String>,
    /// Load ontology annotations from a Turtle/OWL file and merge them.
    /// May be repeated.
    #[arg(short = 'A', long, value_name = "FILE")]
    pub annotation_file: Vec<PathBuf>,
    /// Add an `rdfs:isDefinedBy` annotation to each entity, pointing at the
    /// ontology IRI (`<bool>`, default false).
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true")]
    pub annotate_defined_by: Option<bool>,
    /// Add a `prov:wasDerivedFrom` ontology annotation pointing at the version
    /// IRI (`<bool>`, default false).
    #[arg(short = 'f', long, num_args = 1, default_missing_value = "true")]
    pub annotate_derived_from: Option<bool>,
    /// Remove all existing ontology annotations first.
    #[arg(short = 'R', long)]
    pub remove_annotations: bool,
    /// If true, interpolate `%{...}` placeholders within annotation
    /// values. Accepted for compatibility; placeholder interpolation is
    /// not performed. `<bool>`.
    #[arg(short = 'e', long, num_args = 1, default_missing_value = "true")]
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
    let mut model = crate::cmd::take_or_load_no_imports(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    // `--interpolate`: replace `%{CURIE-or-IRI}` placeholders in annotation
    // values with that entity's rdfs:label (falling back to the IRI).
    let interp = args.interpolate.unwrap_or(false);
    let (annotation, axiom_annotation, language_annotation, typed_annotation) = {
        let labels = if interp { label_map(&model) } else { std::collections::HashMap::new() };
        let interpolate_all = |vs: &[String]| -> Vec<String> {
            if interp {
                vs.iter().map(|s| interpolate_str(s, &model, &labels)).collect()
            } else {
                vs.to_vec()
            }
        };
        (
            interpolate_all(&args.annotation),
            interpolate_all(&args.axiom_annotation),
            interpolate_all(&args.language_annotation),
            interpolate_all(&args.typed_annotation),
        )
    };
    let mut model = annotate_with(
        model,
        &AnnotateOptions {
            ontology_iri: args.ontology_iri.clone(),
            version_iri: args.version_iri.clone(),
            annotation,
            link_annotation: args.link_annotation.clone(),
            axiom_annotation,
            language_annotation,
            typed_annotation,
            annotation_file: args.annotation_file.clone(),
            annotate_defined_by: args.annotate_defined_by.unwrap_or(false),
            annotate_derived_from: args.annotate_derived_from.unwrap_or(false),
            remove_annotations: args.remove_annotations,
        },
    )?;
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// Map entity IRI → its rdfs:label literal, for `--interpolate`.
fn label_map(model: &crate::model::Model) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                if let (AnnotationSubject::IRI(iri), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    map.insert(iri.as_ref().to_string(), lit.literal().clone());
                }
            }
        }
    }
    map
}

/// Replace each `%{token}` in `s` with the rdfs:label of the entity named by
/// `token` (a CURIE or IRI), falling back to the expanded IRI when unlabelled.
fn interpolate_str(
    s: &str,
    model: &crate::model::Model,
    labels: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("%{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let token = &after[..end];
                let iri = crate::cmd::select::expand(model, token.trim());
                match labels.get(&iri) {
                    Some(label) => out.push_str(label),
                    None => out.push_str(&iri),
                }
                rest = &after[end + 1..];
            }
            // Unterminated `%{` — emit verbatim and stop.
            None => {
                out.push_str("%{");
                rest = after;
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Full set of `annotate` options. Defaults are empty / false so callers can set
/// only what they need.
#[derive(Default)]
pub struct AnnotateOptions {
    pub ontology_iri: Option<String>,
    pub version_iri: Option<String>,
    pub annotation: Vec<String>,
    pub link_annotation: Vec<String>,
    pub axiom_annotation: Vec<String>,
    pub language_annotation: Vec<String>,
    pub typed_annotation: Vec<String>,
    pub annotation_file: Vec<PathBuf>,
    pub annotate_defined_by: bool,
    pub annotate_derived_from: bool,
    pub remove_annotations: bool,
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

/// Apply the ontology-level annotations, the ontology/version IRIs and the rest
/// of the `annotate` options to `model` (pure core).
pub fn annotate_with(
    mut model: crate::model::Model,
    opts: &AnnotateOptions,
) -> Result<crate::model::Model> {
    let ontology_iri = opts.ontology_iri.as_deref();
    let version_iri = opts.version_iri.as_deref();
    let annotation = &opts.annotation;
    let link_annotation = &opts.link_annotation;
    let remove_annotations = opts.remove_annotations;
    if remove_annotations {
        let kept: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| !matches!(ac.component, Component::OntologyAnnotation(_)))
            .cloned()
            .collect();
        let mut ont = horned_owl::ontology::set::SetOntology::new();
        for ac in kept {
            ont.insert(ac);
        }
        model.ont = ont;
    }

    // Set ontology / version IRI by replacing the OntologyID component.
    if ontology_iri.is_some() || version_iri.is_some() {
        let mut existing: Option<OntologyID<_>> = None;
        for ac in model.ont.iter() {
            if let Component::OntologyID(id) = &ac.component {
                existing = Some(id.clone());
                break;
            }
        }
        let mut id = existing.clone().unwrap_or(OntologyID {
            iri: None,
            viri: None,
        });
        if let Some(iri) = ontology_iri {
            id.iri = Some(model.build.iri(iri));
        }
        if let Some(viri) = version_iri {
            id.viri = Some(model.build.iri(viri));
        }
        // Remove old OntologyID, insert the new one.
        let kept: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| !matches!(ac.component, Component::OntologyID(_)))
            .cloned()
            .collect();
        let mut ont = horned_owl::ontology::set::SetOntology::new();
        for ac in kept {
            ont.insert(ac);
        }
        ont.insert(Component::OntologyID(id));
        model.ont = ont;
    }

    for pair in annotation.chunks(2) {
        let [prop, value] = pair else { bail!("--annotation needs PROP VALUE") };
        let full = expand(&model, prop);
        let ap = model.build.annotation_property(full.as_str());
        model.ont.insert(Component::OntologyAnnotation(
            horned_owl::model::OntologyAnnotation(Annotation { ann: Default::default(),
                ap,
                av: AnnotationValue::Literal(Literal::Simple {
                    literal: value.clone(),
                }),
            }),
        ));
        declare_ap_if_custom(&mut model, &full);
    }
    for pair in link_annotation.chunks(2) {
        let [prop, iri] = pair else { bail!("--link-annotation needs PROP IRI") };
        let full = expand(&model, prop);
        let ap = model.build.annotation_property(full.as_str());
        model.ont.insert(Component::OntologyAnnotation(
            horned_owl::model::OntologyAnnotation(Annotation { ann: Default::default(),
                ap,
                av: AnnotationValue::IRI(model.build.iri(expand(&model, iri).as_str())),
            }),
        ));
        declare_ap_if_custom(&mut model, &full);
    }

    // Language-tagged ontology annotations: PROP VALUE LANG.
    for triple in opts.language_annotation.chunks(3) {
        let [prop, value, lang] = triple else {
            bail!("--language-annotation needs PROP VALUE LANG")
        };
        let full = expand(&model, prop);
        let ap = model.build.annotation_property(full.as_str());
        model.ont.insert(Component::OntologyAnnotation(
            horned_owl::model::OntologyAnnotation(Annotation { ann: Default::default(),
                ap,
                av: AnnotationValue::Literal(Literal::Language {
                    literal: value.clone(),
                    lang: lang.clone(),
                }),
            }),
        ));
        declare_ap_if_custom(&mut model, &full);
    }

    // Typed ontology annotations: PROP VALUE TYPE (TYPE is a datatype CURIE/IRI).
    for triple in opts.typed_annotation.chunks(3) {
        let [prop, value, ty] = triple else {
            bail!("--typed-annotation needs PROP VALUE TYPE")
        };
        let full = expand(&model, prop);
        let ap = model.build.annotation_property(full.as_str());
        model.ont.insert(Component::OntologyAnnotation(
            horned_owl::model::OntologyAnnotation(Annotation { ann: Default::default(),
                ap,
                av: AnnotationValue::Literal(Literal::Datatype {
                    literal: value.clone(),
                    datatype_iri: model.build.iri(expand(&model, ty).as_str()),
                }),
            }),
        ));
        declare_ap_if_custom(&mut model, &full);
    }

    // Merge ontology annotations from external files.
    for file in &opts.annotation_file {
        let loaded = crate::io::load(file)?;
        for ac in loaded.ont.iter() {
            if let Component::OntologyAnnotation(oa) = &ac.component {
                model
                    .ont
                    .insert(Component::OntologyAnnotation(oa.clone()));
            }
        }
    }

    // Annotate every axiom with PROP VALUE (literal). horned-owl groups an axiom
    // with its axiom-level annotations in `AnnotatedComponent.ann`, so we rebuild
    // the ontology adding the annotation to each component's annotation set.
    for pair in opts.axiom_annotation.chunks(2) {
        let [prop, value] = pair else { bail!("--axiom-annotation needs PROP VALUE") };
        let full = expand(&model, prop);
        let ann = Annotation { ann: Default::default(),
            ap: model.build.annotation_property(full.as_str()),
            av: AnnotationValue::Literal(Literal::Simple {
                literal: value.clone(),
            }),
        };
        declare_ap_if_custom(&mut model, &full);
        let rebuilt: Vec<AnnotatedComponent<_>> = model
            .ont
            .iter()
            .map(|ac| {
                let mut ac = ac.clone();
                ac.ann.insert(ann.clone());
                ac
            })
            .collect();
        let mut ont = horned_owl::ontology::set::SetOntology::new();
        for ac in rebuilt {
            ont.insert(ac);
        }
        model.ont = ont;
    }

    // Effective ontology / version IRI, used by the defined-by / derived-from
    // options (after any --ontology-iri/--version-iri have been applied above).
    let (eff_ont_iri, eff_viri) = {
        let mut o = None;
        let mut v = None;
        for ac in model.ont.iter() {
            if let Component::OntologyID(id) = &ac.component {
                o = id.iri.as_ref().map(|i| i.as_ref().to_string());
                v = id.viri.as_ref().map(|i| i.as_ref().to_string());
                break;
            }
        }
        (o, v)
    };

    // --annotate-defined-by: add rdfs:isDefinedBy <ontology IRI> to each entity.
    if opts.annotate_defined_by {
        if let Some(ont_iri) = &eff_ont_iri {
            let subjects = entity_iris(&model);
            let ap = model.build.annotation_property(RDFS_IS_DEFINED_BY);
            let target = model.build.iri(ont_iri.as_str());
            for subj in subjects {
                model.ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
                    subject: AnnotationSubject::IRI(model.build.iri(subj.as_str())),
                    ann: Annotation { ann: Default::default(),
                        ap: ap.clone(),
                        av: AnnotationValue::IRI(target.clone()),
                    },
                }));
            }
        } else {
            status!("annotate: --annotate-defined-by ignored (no ontology IRI set)");
        }
    }

    // --annotate-derived-from: add prov:wasDerivedFrom <version IRI> as an
    // ontology annotation.
    if opts.annotate_derived_from {
        if let Some(viri) = &eff_viri {
            let ap = model.build.annotation_property(PROV_WAS_DERIVED_FROM);
            model.ont.insert(Component::OntologyAnnotation(
                horned_owl::model::OntologyAnnotation(Annotation { ann: Default::default(),
                    ap,
                    av: AnnotationValue::IRI(model.build.iri(viri.as_str())),
                }),
            ));
        } else {
            status!("annotate: --annotate-derived-from ignored (no version IRI set)");
        }
    }

    Ok(model)
}

/// Collect the IRIs of declared entities (classes, object/data/annotation
/// properties, named individuals, datatypes) for `--annotate-defined-by`.
fn entity_iris(model: &crate::model::Model) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::DeclareClass(d) => {
                out.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareObjectProperty(d) => {
                out.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareDataProperty(d) => {
                out.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareAnnotationProperty(d) => {
                out.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareNamedIndividual(d) => {
                out.insert(d.0 .0.as_ref().to_string());
            }
            Component::DeclareDatatype(d) => {
                out.insert(d.0 .0.as_ref().to_string());
            }
            _ => {}
        }
    }
    out.into_iter().collect()
}

/// Expand a CURIE (`prefix:local`) against the model's prefix map; pass full
/// IRIs through unchanged.
/// Declare every *non-built-in* annotation property `annotate` adds, so the
/// property is never dangling (a base module built with
/// `annotate --link-annotation dc:type …` carries `Declaration(AnnotationProperty(dc:type))`).
/// Built-in OWL/RDFS annotation properties are never declared: OWL 2 predeclares
/// them, so an added declaration would be pure noise in the output.
/// `model.ont` is a set, so re-declaring an already-declared property is a no-op.
fn declare_ap_if_custom(model: &mut crate::model::Model, iri: &str) {
    const BUILTIN: &[&str] = &[
        "http://www.w3.org/2000/01/rdf-schema#label",
        "http://www.w3.org/2000/01/rdf-schema#comment",
        "http://www.w3.org/2000/01/rdf-schema#seeAlso",
        "http://www.w3.org/2000/01/rdf-schema#isDefinedBy",
        "http://www.w3.org/2002/07/owl#versionInfo",
        "http://www.w3.org/2002/07/owl#backwardCompatibleWith",
        "http://www.w3.org/2002/07/owl#priorVersion",
        "http://www.w3.org/2002/07/owl#incompatibleWith",
        "http://www.w3.org/2002/07/owl#deprecated",
    ];
    if BUILTIN.contains(&iri) {
        return;
    }
    let ap = model.build.annotation_property(iri);
    model
        .ont
        .insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(ap)));
}

fn expand(model: &crate::model::Model, s: &str) -> String {
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("urn:") {
        return s.to_string();
    }
    // A CURIE on the COMMAND LINE expands against the context map, where `dc` is
    // dc/TERMS/ — not against the document's own map, where `dc` is the
    // elements/1.1/ namespace that documents declare. `--annotation dc:description`
    // therefore annotates with `…/dc/terms/description`; the same split already
    // applies to template CURIEs (`template::robot_context_prefixes`).
    if let Some((pfx, local)) = s.split_once(':') {
        if pfx == "dc" && !local.starts_with('/') {
            return format!("http://purl.org/dc/terms/{local}");
        }
    }
    model
        .prefixes
        .expand_curie_string(s)
        .unwrap_or_else(|_| s.to_string())
}
