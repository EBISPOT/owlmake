//! `repair` — migrate every reference to a deprecated entity to its
//! replacement, and merge the annotations of axioms that are otherwise the same.
//!
//! An entity is deprecated where an annotation assertion gives it
//! `owl:deprecated "true"^^xsd:boolean`, and its replacement is the value of its
//! `term replaced by` (IAO:0100001). Both are read from the whole imports
//! closure; only the ontology's own axioms are rewritten.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, AnnotationSubject, AnnotationValue, ClassExpression as CE, Component, Literal, MutableOntology,
    ObjectPropertyExpression as OPE, RcStr, SubObjectPropertyExpression as SOPE,
};

use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Migrate every reference to a deprecated entity to its replacement.
    /// Without `--merge-axiom-annotations true` this repair is made whatever
    /// this says. `<bool>`.
    #[arg(short = 'r', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub invalid_references: Option<bool>,

    /// Merge the annotations of axioms that are otherwise the same, before
    /// any reference is migrated. `<bool>`.
    #[arg(short = 'm', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub merge_axiom_annotations: Option<bool>,

    /// An annotation property whose assertions on a deprecated entity move to
    /// its replacement; the deprecated entity keeps its other annotations.
    /// Repeatable.
    #[arg(short = 'a', long, value_name = "PROP")]
    pub annotation_property: Vec<String>,

    /// A file listing more such annotation properties, one per line; a blank
    /// line and a line starting `#` list none.
    #[arg(short = 'A', long = "annotation-properties-file", value_name = "FILE")]
    pub annotation_properties_file: Option<PathBuf>,

    /// Read and not used: the ontology keeps its IRI.
    #[arg(short = 'O', long = "output-iri", value_name = "IRI")]
    pub output_iri: Option<String>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

/// What `repair` does.
#[derive(Default, Clone)]
pub struct RepairOptions {
    /// Migrate every reference to a deprecated entity to its replacement.
    pub invalid_references: bool,
    /// Merge the annotations of axioms that are otherwise the same.
    pub merge_axiom_annotations: bool,
    /// The annotation properties, as given, whose assertions on a deprecated
    /// entity move to its replacement.
    pub annotation_properties: Vec<String>,
}

impl RepairOptions {
    /// The repairs the two switches ask for: a merge of axiom annotations when
    /// `merge_axiom_annotations` is true, and the migration of references when
    /// `invalid_references` is true or the merge is not asked for.
    pub fn from_switches(invalid_references: Option<bool>, merge_axiom_annotations: Option<bool>) -> RepairOptions {
        let merge = merge_axiom_annotations.unwrap_or(false);
        RepairOptions {
            invalid_references: invalid_references.unwrap_or(false) || !merge,
            merge_axiom_annotations: merge,
            annotation_properties: Vec::new(),
        }
    }
}

pub fn step(piped: Option<Model>, args: &Args) -> anyhow::Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut opts = RepairOptions::from_switches(args.invalid_references, args.merge_axiom_annotations);
    opts.annotation_properties = args.annotation_property.clone();
    if let Some(path) = &args.annotation_properties_file {
        opts.annotation_properties.extend(read_annotation_properties(path)?);
    }
    let mut model = repair_with(model, &opts);
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// The annotation properties a file lists: one per line, trimmed, with blank
/// lines and lines starting `#` skipped.
pub fn read_annotation_properties(path: &Path) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read annotation properties file {}: {e}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Make the repairs `opts` asks for: the merge of axiom annotations first,
/// then the migration of references. The core the CLI and the build share.
pub fn repair_with(mut model: Model, opts: &RepairOptions) -> Model {
    if opts.merge_axiom_annotations {
        merge_axiom_annotations(&mut model);
        model.mark_root_changed();
    }
    if opts.invalid_references {
        migrate_deprecated(&mut model, &opts.annotation_properties);
    }
    model
}

/// Merge the ontology's own axioms that differ only in their annotations into
/// one carrying all of them.
fn merge_axiom_annotations(model: &mut Model) {
    let mut groups: BTreeMap<Component<RcStr>, Vec<AnnotatedComponent<RcStr>>> = BTreeMap::new();
    for ac in model.ont.iter() {
        if !model.imported_components.contains(ac) {
            groups.entry(ac.component.clone()).or_default().push(ac.clone());
        }
    }
    for (component, twins) in groups {
        if twins.len() < 2 {
            continue;
        }
        let ann: BTreeSet<_> = twins.iter().flat_map(|ac| ac.ann.iter().cloned()).collect();
        for ac in &twins {
            model.ont.remove(ac);
        }
        model.ont.insert(AnnotatedComponent { component, ann });
    }
}

const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";
/// `term replaced by`.
const IAO_REPLACED_BY: &str = "http://purl.obolibrary.org/obo/IAO_0100001";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";

/// Rename every deprecated entity that has a replacement to it, all at once,
/// in the ontology's own axioms but for those that belong to the entity: its
/// declarations, its annotation assertions other than those of
/// `annotation_properties`, and, of a class or an object property, the axioms
/// about it.
fn migrate_deprecated(model: &mut Model, annotation_properties: &[String]) {
    let mut deprecated: Vec<String> = Vec::new();
    let mut replacements: HashMap<String, Vec<AnnotationValue<RcStr>>> = HashMap::new();
    for ac in model.ont.iter() {
        let Component::AnnotationAssertion(aa) = &ac.component else { continue };
        let AnnotationSubject::IRI(subject) = &aa.subject else { continue };
        match aa.ann.ap.0.as_ref() {
            OWL_DEPRECATED if is_true(&aa.ann.av) => deprecated.push(subject.as_ref().to_string()),
            IAO_REPLACED_BY => replacements.entry(subject.as_ref().to_string()).or_default().push(aa.ann.av.clone()),
            _ => {}
        }
    }
    // Of an entity's replacements, the first in the natural order that names an
    // IRI.
    let order = model.natural_order();
    let mut map: HashMap<String, String> = HashMap::new();
    for iri in deprecated {
        let Some(values) = replacements.get(&iri) else { continue };
        let mut values: Vec<&AnnotationValue<RcStr>> = values.iter().collect();
        values.sort_by(|a, b| order.annotation_value(a, b));
        if let Some(target) = values.into_iter().find_map(|v| replacement(model, v)) {
            map.insert(iri, target);
        }
    }
    if map.is_empty() {
        return;
    }
    let moved: HashSet<String> =
        annotation_properties.iter().map(|p| crate::cmd::select::expand(model, p)).collect();
    let keep: HashSet<AnnotatedComponent<RcStr>> = model
        .ont
        .iter()
        .filter(|ac| {
            model.imported_components.contains(*ac)
                || owners(&ac.component, &moved).iter().any(|owner| map.contains_key(*owner))
        })
        .cloned()
        .collect();
    crate::cmd::rename::rename_entities_at_once(model, &map, |ac| keep.contains(ac));
}

/// Whether a value is the `xsd:boolean` true.
fn is_true(value: &AnnotationValue<RcStr>) -> bool {
    matches!(value, AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri })
        if literal == "true" && datatype_iri.as_ref() == XSD_BOOLEAN)
}

/// The IRI a replacement value names: an IRI itself, or a literal's text read
/// as the command line reads a CURIE or an IRI, as written.
fn replacement(model: &Model, value: &AnnotationValue<RcStr>) -> Option<String> {
    match value {
        AnnotationValue::IRI(iri) => Some(iri.as_ref().to_string()),
        AnnotationValue::Literal(literal) => model.context.iri(literal.literal()),
        AnnotationValue::AnonymousIndividual(_) => None,
    }
}

/// The entities an axiom belongs to, which keep it when they are migrated: a
/// declaration's entity; an annotation assertion's subject, unless its
/// property is one of `moved`; the subclass of a subclass axiom, the classes of
/// an equivalence or a disjointness and the class a disjoint union defines; and
/// the object property an axiom of sub-properties, equivalence, disjointness,
/// inverses, a characteristic, a domain or a range is about, each as itself and
/// not as an inverse.
fn owners<'a>(component: &'a Component<RcStr>, moved: &HashSet<String>) -> Vec<&'a str> {
    let class = |ce: &'a CE<RcStr>| match ce {
        CE::Class(c) => Some(c.0.as_ref()),
        _ => None,
    };
    let property = |ope: &'a OPE<RcStr>| match ope {
        OPE::ObjectProperty(p) => Some(p.0.as_ref()),
        OPE::InverseObjectProperty(_) => None,
    };
    match component {
        Component::DeclareClass(d) => vec![d.0 .0.as_ref()],
        Component::DeclareObjectProperty(d) => vec![d.0 .0.as_ref()],
        Component::DeclareDataProperty(d) => vec![d.0 .0.as_ref()],
        Component::DeclareAnnotationProperty(d) => vec![d.0 .0.as_ref()],
        Component::DeclareNamedIndividual(d) => vec![d.0 .0.as_ref()],
        Component::DeclareDatatype(d) => vec![d.0 .0.as_ref()],
        Component::AnnotationAssertion(aa) => match &aa.subject {
            AnnotationSubject::IRI(s) if !moved.contains(aa.ann.ap.0.as_ref()) => vec![s.as_ref()],
            _ => vec![],
        },
        Component::SubClassOf(sc) => class(&sc.sub).into_iter().collect(),
        Component::EquivalentClasses(eq) => eq.0.iter().filter_map(class).collect(),
        Component::DisjointClasses(dc) => dc.0.iter().filter_map(class).collect(),
        Component::DisjointUnion(du) => vec![du.0 .0.as_ref()],
        Component::SubObjectPropertyOf(sp) => match &sp.sub {
            SOPE::ObjectPropertyExpression(ope) => property(ope).into_iter().collect(),
            SOPE::ObjectPropertyChain(_) => vec![],
        },
        Component::EquivalentObjectProperties(e) => e.0.iter().filter_map(property).collect(),
        Component::DisjointObjectProperties(d) => d.0.iter().filter_map(property).collect(),
        Component::InverseObjectProperties(i) => [&i.0, &i.1].into_iter().filter_map(property).collect(),
        Component::FunctionalObjectProperty(p) => property(&p.0).into_iter().collect(),
        Component::InverseFunctionalObjectProperty(p) => property(&p.0).into_iter().collect(),
        Component::ReflexiveObjectProperty(p) => property(&p.0).into_iter().collect(),
        Component::IrreflexiveObjectProperty(p) => property(&p.0).into_iter().collect(),
        Component::SymmetricObjectProperty(p) => property(&p.0).into_iter().collect(),
        Component::AsymmetricObjectProperty(p) => property(&p.0).into_iter().collect(),
        Component::TransitiveObjectProperty(p) => property(&p.0).into_iter().collect(),
        Component::ObjectPropertyDomain(d) => property(&d.ope).into_iter().collect(),
        Component::ObjectPropertyRange(r) => property(&r.ope).into_iter().collect(),
        _ => vec![],
    }
}
