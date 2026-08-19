//! `create-species-subset` — carve out the part of an ontology that is valid in a
//! given taxon. UBERON's release builds its species subsets with the `default`
//! strategy.
//!
//! For a target taxon and a set of root classes, it computes the classes valid in
//! that taxon and (optionally) tags them with an `oboInOwl:inSubset` annotation
//! and/or removes every non-member class from the ontology.
//!
//! Default strategy: assert `root ⊑ in_taxon some TAXON`, classify, and treat any
//! class that becomes unsatisfiable as excluded — which is exactly how UBERON's
//! `only_in_taxon`/`never_in_taxon` constraints plus the `in_taxon` property
//! chains force exclusion. The "cross-taxon" homology relations are stripped
//! before reasoning (and restored after) so a class is not dragged out of the
//! subset merely by being homologous to one that is invalid in the taxon.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationProperty, AnnotationSubject,
    AnnotationValue, Build, ClassExpression as CE, Component, ObjectPropertyExpression as OPE,
    SubAnnotationPropertyOf, SubClassOf,
};
use horned_owl::model::MutableOntology;

use crate::cmd::remove::{self, TermOptions};
use crate::cmd::select;
use crate::model::{Model, Str};
use crate::reason::el;

const IN_TAXON: &str = "http://purl.obolibrary.org/obo/RO_0002162";
const IN_SUBSET: &str = "http://www.geneontology.org/formats/oboInOwl#inSubset";
const SUBSET_PROPERTY: &str = "http://www.geneontology.org/formats/oboInOwl#SubsetProperty";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The "cross-taxon" relation properties (homology and descent). Axioms over
/// these are removed before reasoning so homology links do not propagate taxon
/// invalidity, then restored afterwards.
const CROSS_TAXON_RELATIONS: &[&str] = &[
    "http://purl.obolibrary.org/obo/RO_0002320",
    "http://purl.obolibrary.org/obo/RO_0002156",
    "http://purl.obolibrary.org/obo/RO_0002374",
    "http://purl.obolibrary.org/obo/RO_0002312",
    "http://purl.obolibrary.org/obo/RO_0002157",
    "http://purl.obolibrary.org/obo/RO_0002159",
    "http://purl.obolibrary.org/obo/RO_0002158",
];

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub format: Option<String>,
    /// The taxon to build the subset for (CURIE/IRI, e.g. `NCBITaxon:9606`).
    #[arg(short = 't', long)]
    pub taxon: String,
    /// Accepted and ignored; this subsetter always uses owlmake's EL reasoner.
    #[arg(short = 'r', long)]
    pub reasoner: Option<String>,
    /// `default` (assert the taxon on each root) or `precise` (per-class satisfiability).
    #[arg(long, default_value = "default")]
    pub strategy: String,
    /// Root class(es) to seed the subset (CURIE/IRI, repeatable). Defaults to
    /// `owl:Thing` (all classes) when none given.
    #[arg(long)]
    pub root: Vec<String>,
    /// The `oboInOwl:inSubset` value to tag in-subset classes with (optional).
    #[arg(long = "subset-name")]
    pub subset_name: Option<String>,
    /// Only tag classes whose IRI starts with one of these (CURIE) prefixes.
    #[arg(long = "only-tag-in")]
    pub only_tag_in: Vec<String>,
    /// Write the inSubset tag axioms to this file instead of the main ontology.
    #[arg(long = "write-tags-to")]
    pub write_tags_to: Option<PathBuf>,
    /// Keep non-subset classes in the output (default: remove them).
    #[arg(long = "no-remove")]
    pub no_remove: bool,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    let taxon = select::expand(&model, &args.taxon);
    let roots: Vec<String> = args.root.iter().map(|r| select::expand(&model, r)).collect();

    let subset = if args.strategy.eq_ignore_ascii_case("precise") {
        precise_subset(&mut model, &roots, &taxon)
    } else {
        default_subset(&mut model, &roots, &taxon)
    };

    // Tagging pass (only when --subset-name is given).
    if let Some(name) = &args.subset_name {
        let subset_iri = select::expand(&model, name);
        let prefixes: Vec<String> =
            args.only_tag_in.iter().map(|p| select::expand(&model, p)).collect();
        let annotations = in_subset_annotations(&subset, &subset_iri, &prefixes);
        if let Some(path) = &args.write_tags_to {
            let mut tags = Model::new();
            // The tags file is a NEW ontology with no prefix map of its own, so it
            // declares only the builtins and writes every IRI in full — the
            // reference's carries five `Prefix(` lines, not the input's nine.
            tags.format_prefixes_cleared = true;
            for ax in annotations {
                tags.ont.insert(ax);
            }
            crate::io::save(&mut tags, path)?;
        } else {
            for ax in annotations {
                model.ont.insert(ax);
            }
        }
    }

    // Removal pass (unless --no-remove): drop every class not in the subset.
    if !args.no_remove {
        let excluded: Vec<String> = select::entities(&model)
            .classes
            .into_iter()
            .filter(|c| c != OWL_THING && c != OWL_NOTHING && !subset.contains(c))
            .collect();
        let opts = TermOptions {
            preserve_structure: Some(false),
            trim: Some(true),
            // A class this pass drops must not take the assertions that merely POINT
            // at it: the subset keeps `RO_0002175 … NCBITaxon_9606` on the classes it
            // keeps, even though the taxon itself is not a member.
            annotation_values: Some(false),
            ..Default::default()
        };
        // A PUNNED excluded IRI is ambiguous to `remove`, which leaves it alone —
        // but only its CLASS sense is excluded here, and that sense still has to
        // go. CL's `STATO:0000416` is a class of `cl-full.owl` and the annotation
        // property that carries an F-beta score on a CLM assertion: the subset
        // keeps the property and its assertions, and drops the class, its
        // declaration and the definition and label written on its IRI.
        let sig = select::entities(&model);
        let punned: std::collections::HashSet<String> = excluded
            .iter()
            .filter(|c| {
                [
                    &sig.object_properties,
                    &sig.data_properties,
                    &sig.annotation_properties,
                    &sig.individuals,
                    &sig.datatypes,
                ]
                .iter()
                .any(|k| k.contains(*c))
            })
            .cloned()
            .collect();
        model = remove::remove_with(model, &excluded, &[], &[], &[], &[], &opts)?;
        if !punned.is_empty() {
            let doomed: Vec<AnnotatedComponent<Str>> = model
                .ont
                .iter()
                .filter(|ac| {
                    crate::sig::typed_signature(&ac.component)
                        .iter()
                        .any(|(k, iri)| *k == crate::sig::kind::CLASS && punned.contains(iri))
                        || matches!(&ac.component, Component::AnnotationAssertion(aa)
                            if matches!(&aa.subject, horned_owl::model::AnnotationSubject::IRI(i)
                                if punned.contains(i.as_ref())))
                })
                .cloned()
                .collect();
            for ac in doomed {
                model.ont.remove(&ac);
            }
        }
    }

    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// The default species subsetter: assert the taxon on each root, classify once,
/// and drop whatever the assertion makes unsatisfiable.
fn default_subset(model: &mut Model, roots: &[String], taxon: &str) -> HashSet<String> {
    let b = Build::new();
    let roots: Vec<String> =
        if roots.is_empty() { vec![OWL_THING.to_string()] } else { roots.to_vec() };

    // Strip cross-taxon homology axioms so they don't propagate invalidity.
    let cross: Vec<AnnotatedComponent<Str>> = model
        .ont
        .iter()
        .filter(|ac| mentions_object_property(&ac.component, CROSS_TAXON_RELATIONS))
        .cloned()
        .collect();
    for ac in &cross {
        model.ont.remove(ac);
    }

    // Base hierarchy (cross-taxon stripped, no in_taxon assertion yet): superclass
    // sets for every class, used to enumerate each root's descendants.
    let all_classes = select::entities(model).classes;
    let base = el::Reasoner::classify(model);
    let mut sup_of: std::collections::HashMap<String, HashSet<String>> =
        std::collections::HashMap::new();
    for (sub, sup) in base.all_subsumptions() {
        sup_of.entry(sub).or_default().insert(sup);
    }
    drop(base);

    let mut subset: HashSet<String> = HashSet::new();
    for root in &roots {
        // Candidate set = root's descendants (+ the root itself unless owl:Thing).
        let mut candidates: HashSet<String> = if root == OWL_THING {
            all_classes.iter().filter(|c| c.as_str() != OWL_THING).cloned().collect()
        } else {
            let mut c: HashSet<String> = sup_of
                .iter()
                .filter(|(_, sups)| sups.contains(root))
                .map(|(sub, _)| sub.clone())
                .collect();
            c.insert(root.clone());
            c
        };

        // Assert `root ⊑ in_taxon some TAXON`, classify, drop the now-unsatisfiable.
        let ax: AnnotatedComponent<Str> = Component::SubClassOf(SubClassOf {
            sub: CE::Class(b.class(root.as_str())),
            sup: CE::ObjectSomeValuesFrom {
                ope: OPE::ObjectProperty(b.object_property(IN_TAXON)),
                bce: Box::new(CE::Class(b.class(taxon))),
            },
        })
        .into();
        model.ont.insert(ax.clone());
        let reasoner = el::Reasoner::classify(model);
        for u in reasoner.unsatisfiable() {
            candidates.remove(&u);
        }
        drop(reasoner);
        model.ont.remove(&ax);

        subset.extend(candidates);
    }

    // Restore the cross-taxon axioms.
    for ac in cross {
        model.ont.insert(ac);
    }
    subset
}

/// The precise species subsetter: a class is in-subset iff `C ⊓ (in_taxon some
/// TAXON)` is satisfiable.
///
/// Asking the reasoner that question one class at a time would mean a full
/// classification per candidate — ~3,600 of them for CL, each over a
/// 19,000-class model — so every candidate is probed in a single pass instead:
/// define one fresh class `probe_C ≡ C ⊓ (in_taxon some TAXON)` per candidate,
/// classify once, and read the answers off the unsatisfiable set.
/// A probe is a brand-new name occurring only on the left of its own definition,
/// so it cannot affect any other class's satisfiability — the batched answers are
/// exactly the individual ones.
///
/// Unlike the default strategy this leaves the ontology's own axioms untouched,
/// which is the point of `precise`: it needs no cross-taxon stripping, because a
/// homology link to a taxon-invalid class does not by itself make `C ⊓ in_taxon
/// some TAXON` unsatisfiable.
fn precise_subset(model: &mut Model, roots: &[String], taxon: &str) -> HashSet<String> {
    let b = Build::new();
    let roots: Vec<String> =
        if roots.is_empty() { vec![OWL_THING.to_string()] } else { roots.to_vec() };

    let all_classes = select::entities(model).classes;
    let base = el::Reasoner::classify(model);
    let mut sup_of: std::collections::HashMap<String, HashSet<String>> =
        std::collections::HashMap::new();
    for (sub, sup) in base.all_subsumptions() {
        sup_of.entry(sub).or_default().insert(sup);
    }
    drop(base);

    let mut subset: HashSet<String> = HashSet::new();
    let mut candidates: Vec<String> = Vec::new();
    for root in &roots {
        if root == OWL_THING {
            candidates.extend(all_classes.iter().filter(|c| c.as_str() != OWL_THING).cloned());
        } else {
            // The root is in the subset by fiat: it seeds the candidate set and is
            // never satisfiability-tested itself.
            subset.insert(root.clone());
            candidates.extend(
                sup_of
                    .iter()
                    .filter(|(_, sups)| sups.contains(root))
                    .map(|(sub, _)| sub.clone()),
            );
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates.retain(|c| !subset.contains(c));

    let probe_of = |c: &str| format!("urn:owlmake:species-subset-probe:{c}");
    let probes: Vec<AnnotatedComponent<Str>> = candidates
        .iter()
        .map(|c| {
            Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
                CE::Class(b.class(probe_of(c).as_str())),
                CE::ObjectIntersectionOf(vec![
                    CE::Class(b.class(c.as_str())),
                    CE::ObjectSomeValuesFrom {
                        ope: OPE::ObjectProperty(b.object_property(IN_TAXON)),
                        bce: Box::new(CE::Class(b.class(taxon))),
                    },
                ]),
            ]))
            .into()
        })
        .collect();
    for ax in &probes {
        model.ont.insert(ax.clone());
    }
    let reasoner = el::Reasoner::classify(model);
    let unsat: HashSet<String> = reasoner.unsatisfiable().into_iter().collect();
    drop(reasoner);
    for ax in &probes {
        model.ont.remove(ax);
    }

    for c in candidates {
        if !unsat.contains(&probe_of(&c)) {
            subset.insert(c);
        }
    }
    subset
}

/// Build the `inSubset` tag axioms: one `SubAnnotationPropertyOf(subset,
/// oboInOwl:SubsetProperty)` declaration plus one `AnnotationAssertion(inSubset,
/// class, subset)` per in-subset class whose IRI passes the prefix filter.
fn in_subset_annotations(
    subset: &HashSet<String>,
    subset_iri: &str,
    prefixes: &[String],
) -> Vec<AnnotatedComponent<Str>> {
    let b = Build::new();
    let mut out: Vec<AnnotatedComponent<Str>> = Vec::new();
    out.push(
        Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
            sub: AnnotationProperty(b.iri(subset_iri)),
            sup: AnnotationProperty(b.iri(SUBSET_PROPERTY)),
        })
        .into(),
    );
    let mut classes: Vec<&String> = subset.iter().collect();
    classes.sort();
    for c in classes {
        if !prefixes.is_empty() && !prefixes.iter().any(|p| c.starts_with(p)) {
            continue;
        }
        out.push(
            Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(c.as_str())),
                ann: Annotation { ann: Default::default(),
                    ap: AnnotationProperty(b.iri(IN_SUBSET)),
                    av: AnnotationValue::IRI(b.iri(subset_iri)),
                },
            })
            .into(),
        );
    }
    out
}

/// Whether a component's signature mentions any of `props` (the cross-taxon
/// relation IRIs, which only ever appear as object properties).
fn mentions_object_property(comp: &Component<Str>, props: &[&str]) -> bool {
    crate::sig::signature(comp).iter().any(|iri| props.contains(&iri.as_str()))
}
