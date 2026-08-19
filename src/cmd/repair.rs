//! `repair` — fix common mechanical problems in an ontology.
//!
//! Implemented repairs: drop duplicate components (the `SetOntology` already
//! dedupes structurally, but annotations on otherwise-equal axioms are merged),
//! and optionally remove annotation assertions whose subject is not declared in
//! the ontology (dangling `--invalid-references`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{AnnotationSubject, AnnotationValue, Component, MutableOntology, RcStr};
use horned_owl::ontology::set::SetOntology;

use crate::model::Model;
use crate::sig;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Remove annotation assertions whose subject IRI is never declared or used in
    /// a logical axiom (dangling references).
    #[arg(short = 'r', long)]
    pub invalid_references: bool,

    /// If true, merge the annotation sets of axioms that are otherwise
    /// identical. `<bool>`.
    #[arg(short = 'm', long, num_args = 1, default_missing_value = "true")]
    pub merge_axiom_annotations: Option<bool>,

    /// An annotation property whose assertions should be migrated/retained.
    /// Repeatable. Accepted for compatibility; currently only recorded, not
    /// used to drive a migration.
    #[arg(short = 'a', long, value_name = "PROP")]
    pub annotation_property: Vec<String>,

    /// File listing annotation properties to migrate, one IRI/CURIE per line; unioned
    /// with `--annotation-property`.
    #[arg(short = 'A', long = "annotation-properties-file", value_name = "FILE")]
    pub annotation_properties_file: Option<PathBuf>,

    /// Set the OntologyIRI for the output.
    #[arg(short = 'O', long = "output-iri", value_name = "IRI")]
    pub output_iri: Option<String>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

/// Options controlling `repair`. Both repairs are off by default: no
/// invalid-reference removal, no annotation merging.
#[derive(Default, Clone)]
pub struct RepairOptions {
    pub invalid_references: bool,
    pub merge_axiom_annotations: bool,
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // Union the inline --annotation-property values with any from the file.
    let mut annotation_properties = args.annotation_property.clone();
    if let Some(path) = &args.annotation_properties_file {
        let text = std::fs::read_to_string(path)?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            annotation_properties.push(line.to_string());
        }
    }
    // Migrate references to obsolete entities to their replacement. The
    // replacement target is read from `term replaced_by` (IAO:0100001) by default,
    // plus any property given via --annotation-property/-A, then every reference to
    // the obsolete IRI is rewritten.
    let mut props: Vec<String> = vec![IAO_REPLACED_BY.to_string()];
    props.extend(annotation_properties.iter().map(|p| crate::cmd::select::expand(&model, p)));
    let replace_map = build_replacement_map(&model, &props);
    if !replace_map.is_empty() {
        model = crate::cmd::rename::rename_model(model, &replace_map)?;
        status!(
            "repair: migrated {} reference(s) to replacement entities",
            replace_map.len()
        );
    }

    let mut model = repair_with(
        model,
        &RepairOptions {
            invalid_references: args.invalid_references,
            merge_axiom_annotations: args.merge_axiom_annotations.unwrap_or(false),
        },
    );

    // Set the output ontology IRI, reusing annotate's core.
    if let Some(iri) = &args.output_iri {
        model = crate::cmd::annotate::annotate_with(
            model,
            &crate::cmd::annotate::AnnotateOptions {
                ontology_iri: Some(iri.clone()),
                ..Default::default()
            },
        )?;
    }

    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Convenience entry point for the default repairs: dedupe, and optionally drop
/// dangling annotation assertions.
pub fn repair(model: Model, invalid_references: bool) -> Model {
    repair_with(
        model,
        &RepairOptions {
            invalid_references,
            ..Default::default()
        },
    )
}

/// Repair mechanical problems (dedupe; optionally drop dangling annotation
/// assertions; optionally merge annotation sets of otherwise-identical axioms).
/// Pure core shared by the CLI and the build executor.
pub fn repair_with(model: Model, opts: &RepairOptions) -> Model {
    // Collect the set of IRIs that are declared or appear in a logical axiom.
    let mut known: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        if is_logical_or_decl(&ac.component) {
            known.extend(sig::signature(&ac.component));
            // `sig::signature` omits annotation properties, so a synonym-type /
            // subset property declared only via `SubAnnotationPropertyOf` (or a
            // bare `DeclareAnnotationProperty`) would count as unknown and its own
            // `rdfs:label` be dropped as a dangling reference. Count declared
            // annotation properties as known too.
            match &ac.component {
                Component::DeclareAnnotationProperty(d) => {
                    known.insert(d.0 .0.as_ref().to_string());
                }
                Component::SubAnnotationPropertyOf(s) => {
                    known.insert(s.sub.0.as_ref().to_string());
                    known.insert(s.sup.0.as_ref().to_string());
                }
                _ => {}
            }
        }
    }

    // When merging axiom annotations, group annotated components by their bare
    // component, unioning every annotation set onto a single representative.
    let mut merged: std::collections::BTreeMap<
        Component<RcStr>,
        std::collections::BTreeSet<horned_owl::model::Annotation<RcStr>>,
    > = std::collections::BTreeMap::new();
    let mut merge_hits = 0usize;

    let mut ont: SetOntology<RcStr> = SetOntology::new();
    let mut dropped = 0usize;
    for ac in model.ont.iter() {
        if opts.invalid_references {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if let AnnotationSubject::IRI(iri) = &aa.subject {
                    if !known.contains(iri.as_ref()) {
                        dropped += 1;
                        continue;
                    }
                }
            }
        }
        if opts.merge_axiom_annotations {
            let entry = merged.entry(ac.component.clone()).or_default();
            if !entry.is_empty() && !ac.ann.is_empty() {
                merge_hits += 1;
            }
            entry.extend(ac.ann.iter().cloned());
            continue;
        }
        // SetOntology insertion dedupes structurally-identical components.
        ont.insert(ac.clone());
    }

    if opts.merge_axiom_annotations {
        for (component, ann) in merged {
            ont.insert(horned_owl::model::AnnotatedComponent { component, ann });
        }
        status!("repair: merged annotation sets across {merge_hits} duplicate axiom(s)");
    }

    status!("repair: dropped {dropped} dangling annotation assertion(s)");
    // `repair` rewrites axioms into a fresh `SetOntology`, but the document state a
    // `Model` carries alongside them is unchanged, so all of it has to come across:
    // rebuilding from parts alone loses `format_prefixes_cleared` and puts the input's
    // prefix map back on the output of an extract chain.
    let mut out = Model::from_parts(ont, model.prefixes.clone());
    out.carry_meta_from(&model);
    out
}

/// `term replaced_by` — the default property carrying an obsolete entity's
/// replacement IRI.
const IAO_REPLACED_BY: &str = "http://purl.obolibrary.org/obo/IAO_0100001";

/// Build the obsolete-IRI → replacement-IRI map from annotation assertions whose
/// property is one of `props` (IRI or CURIE/IRI-literal value).
fn build_replacement_map(model: &Model, props: &[String]) -> HashMap<String, String> {
    let propset: HashSet<&str> = props.iter().map(String::as_str).collect();
    let mut map = HashMap::new();
    for ac in model.ont.iter() {
        let Component::AnnotationAssertion(aa) = &ac.component else {
            continue;
        };
        if !propset.contains(aa.ann.ap.0.as_ref()) {
            continue;
        }
        let AnnotationSubject::IRI(subj) = &aa.subject else {
            continue;
        };
        let target = match &aa.ann.av {
            AnnotationValue::IRI(iri) => iri.as_ref().to_string(),
            AnnotationValue::Literal(lit) => crate::cmd::select::expand(model, lit.literal()),
            _ => continue,
        };
        if !target.is_empty() && target != subj.as_ref() {
            map.insert(subj.as_ref().to_string(), target);
        }
    }
    map
}

fn is_logical_or_decl(c: &Component<RcStr>) -> bool {
    !matches!(
        c,
        Component::AnnotationAssertion(_)
            | Component::OntologyAnnotation(_)
            | Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::Import(_)
    )
}
