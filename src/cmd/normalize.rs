//! `normalize` — declare the annotation properties an ontology's own metadata
//! uses.
//!
//! Injects the `SubAnnotationPropertyOf` axioms OBO tooling expects for the
//! subset and synonym-type properties an ontology actually uses:
//!
//!  * `--subset-decls`: every IRI used as a value of `oboInOwl:inSubset` gets
//!    `SubAnnotationPropertyOf(<subset>, oboInOwl:SubsetProperty)`.
//!  * `--synonym-decls`: every IRI used as a value of `oboInOwl:hasSynonymType`
//!    gets `SubAnnotationPropertyOf(<type>, oboInOwl:SynonymTypeProperty)`.
//!
//!  * `--add-source`: records where the ontology came from as an ontology
//!    annotation `dc:source <version IRI>` — nothing at all when it has no
//!    version IRI. Every import module carries one: `normalize --add-source true`
//!    runs on the mirror, and `extract --copy-ontology-annotations true` carries
//!    the annotation into the module.
//!
//! Only IRIs in the `--base-iri` namespace(s) are declared (default: OBO, EFO,
//! Biolink). Merging duplicate axiom annotations is not part of this command;
//! `repair --merge-axiom-annotations` does that as its own step.

use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{
    Annotation, AnnotationValue, Component, MutableOntology, SubAnnotationPropertyOf,
};

const IN_SUBSET: &str = "http://www.geneontology.org/formats/oboInOwl#inSubset";
const HAS_SYNONYM_TYPE: &str = "http://www.geneontology.org/formats/oboInOwl#hasSynonymType";
const SUBSET_PROPERTY: &str = "http://www.geneontology.org/formats/oboInOwl#SubsetProperty";
const SYNONYM_TYPE_PROPERTY: &str =
    "http://www.geneontology.org/formats/oboInOwl#SynonymTypeProperty";
const DC_SOURCE: &str = "http://purl.org/dc/elements/1.1/source";

const DEFAULT_BASE_IRIS: &[&str] = &[
    "http://purl.obolibrary.org/obo/",
    "http://www.ebi.ac.uk/efo/",
    "http://w3id.org/biolink/",
];

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,

    /// IRI namespace(s) whose properties get declarations (repeatable). Each one
    /// is ADDED to the built-in OBO / EFO / Biolink namespaces rather than
    /// replacing them, so naming one namespace does not stop the others being
    /// declared.
    #[arg(long = "base-iri")]
    pub base_iri: Vec<String>,

    /// Inject `SubAnnotationPropertyOf(.., oboInOwl:SubsetProperty)` for subset
    /// properties (`<bool>`, default true).
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub subset_decls: Option<bool>,

    /// Inject `SubAnnotationPropertyOf(.., oboInOwl:SynonymTypeProperty)` for
    /// synonym-type properties (`<bool>`, default true).
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub synonym_decls: Option<bool>,

    /// Annotate the ontology with `dc:source <its version IRI>` (`<bool>`,
    /// default false). A no-op when the ontology has no version IRI.
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub add_source: Option<bool>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Options for [`normalize_with`]. The defaults run both declaration passes over
/// the OBO/EFO/Biolink base namespaces and add no `dc:source`.
pub struct NormalizeOptions {
    pub base_iris: Vec<String>,
    pub subset_decls: bool,
    pub synonym_decls: bool,
    pub add_source: bool,
}

impl Default for NormalizeOptions {
    fn default() -> Self {
        NormalizeOptions {
            base_iris: DEFAULT_BASE_IRIS.iter().map(|s| s.to_string()).collect(),
            subset_decls: true,
            synonym_decls: true,
            add_source: false,
        }
    }
}

impl Args {
    fn options(&self) -> NormalizeOptions {
        NormalizeOptions {
            base_iris: DEFAULT_BASE_IRIS
                .iter()
                .map(|s| s.to_string())
                .chain(self.base_iri.iter().cloned())
                .collect(),
            subset_decls: self.subset_decls.unwrap_or(true),
            synonym_decls: self.synonym_decls.unwrap_or(true),
            add_source: self.add_source.unwrap_or(false),
        }
    }
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
    let mut model = normalize_with(model, &args.options());
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Inject subset/synonym-type subproperty declarations (pure core).
pub fn normalize_with(
    mut model: crate::model::Model,
    opts: &NormalizeOptions,
) -> crate::model::Model {
    let in_base = |iri: &str| opts.base_iris.iter().any(|b| iri.starts_with(b.as_str()));

    let mut subsets: BTreeSet<String> = BTreeSet::new();
    let mut synonym_types: BTreeSet<String> = BTreeSet::new();
    let mut consider = |ann: &Annotation<crate::model::Str>| {
        if let AnnotationValue::IRI(iri) = &ann.av {
            let v = iri.as_ref();
            if !in_base(v) {
                return;
            }
            match ann.ap.0.as_ref() {
                IN_SUBSET => {
                    subsets.insert(v.to_string());
                }
                HAS_SYNONYM_TYPE => {
                    synonym_types.insert(v.to_string());
                }
                _ => {}
            }
        }
    };

    for ac in model.ont.iter() {
        // Axiom annotations (e.g. `Annotation(hasSynonymType <type>)` on a synonym
        // assertion, or `Annotation(inSubset <subset>)`).
        for a in &ac.ann {
            consider(a);
        }
        // The assertion itself (`AnnotationAssertion(inSubset <entity> <subset>)`).
        if let Component::AnnotationAssertion(aa) = &ac.component {
            consider(&aa.ann);
        }
    }

    let ap = |iri: &str| horned_owl::model::AnnotationProperty(model.build.iri(iri));
    if opts.subset_decls {
        let parent = ap(SUBSET_PROPERTY);
        for s in &subsets {
            model.ont.insert(Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
                sub: ap(s),
                sup: parent.clone(),
            }));
        }
    }
    if opts.synonym_decls {
        let parent = ap(SYNONYM_TYPE_PROPERTY);
        for t in &synonym_types {
            model.ont.insert(Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
                sub: ap(t),
                sup: parent.clone(),
            }));
        }
    }

    // `--add-source`: `dc:source <version IRI>` as an ontology annotation. Only a
    // version IRI counts, so an unversioned source contributes no annotation at
    // all rather than one naming its ontology IRI.
    if opts.add_source {
        let viri = model.ont.iter().find_map(|ac| match &ac.component {
            Component::OntologyID(id) => id.viri.as_ref().map(|i| i.as_ref().to_string()),
            _ => None,
        });
        if let Some(viri) = viri {
            let prop = ap(DC_SOURCE);
            model.ont.insert(Component::OntologyAnnotation(
                horned_owl::model::OntologyAnnotation(Annotation {
                    ann: Default::default(),
                    ap: prop.clone(),
                    av: AnnotationValue::IRI(model.build.iri(viri.as_str())),
                }),
            ));
            model.ont.insert(Component::DeclareAnnotationProperty(
                horned_owl::model::DeclareAnnotationProperty(prop),
            ));
        }
    }

    status!(
        "normalize: declared {} subset + {} synonym-type propert{}",
        if opts.subset_decls { subsets.len() } else { 0 },
        if opts.synonym_decls { synonym_types.len() } else { 0 },
        "y(ies)"
    );
    model
}
