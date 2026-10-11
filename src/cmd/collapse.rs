//! `collapse` — reduce a class hierarchy to a set of "precious" terms,
//! reconnecting each kept term to its nearest kept ancestors through removed
//! intermediates.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::bail;
use clap::Args as ClapArgs;
use horned_owl::model::{ClassExpression as CE, Component, MutableOntology, RcStr};

use crate::cmd::objects::{self, Judge, Obj, Selection};
use crate::cmd::select;
use crate::io::entities::Kind;
use crate::model::Model;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Terms to keep ("precious"). Repeatable. (owlmake alias of `--precious`.)
    #[arg(long)]
    pub term: Vec<String>,
    /// File(s) listing precious terms. (owlmake alias of `--precious-terms`.)
    #[arg(long)]
    pub term_file: Vec<PathBuf>,
    /// CURIE or IRI of a class to keep. Repeatable.
    #[arg(short = 'r', long = "precious", value_name = "TERM")]
    pub precious: Vec<String>,
    /// File(s) listing CURIEs/IRIs of classes to keep.
    #[arg(short = 'R', long = "precious-terms", value_name = "FILE")]
    pub precious_terms: Vec<PathBuf>,
    /// Number of named subclasses an intermediate class needs to be kept
    /// (default 2, at least 2). One with fewer, but at least one, is collapsed
    /// and the hierarchy is bridged across it.
    #[arg(short = 't', long, allow_hyphen_values = true)]
    pub threshold: Option<String>,

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
    let threshold = read_threshold(args.threshold.as_deref())?;
    args.common.apply(&mut model)?;
    // --term/--term-file and --precious/--precious-terms name the same set.
    let mut terms = args.term.clone();
    terms.extend(args.precious.iter().cloned());
    let mut term_files = args.term_file.clone();
    term_files.extend(args.precious_terms.iter().cloned());
    let precious = select::collect_terms(&model, &terms, &term_files)?;

    let (mut result, removed) = collapse(model, threshold, &precious)?;
    status!("collapse: removed {removed} intermediate class(es) (threshold {threshold})");
    crate::cmd::maybe_save(&mut result, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(result))
}

/// The `--threshold` text as an `int` of at least 2; 2 when it is not given.
fn read_threshold(text: Option<&str>) -> anyhow::Result<usize> {
    let text = text.unwrap_or("2");
    let Some(n) = crate::java_number::parse_int(text) else {
        bail!("THRESHOLD ERROR threshold ('{text}') must be a valid integer.");
    };
    if n < 2 {
        bail!("THRESHOLD VALUE ERROR threshold ('{n}') must be 2 or greater.");
    }
    Ok(n as usize)
}

/// Collapse the class hierarchy of `model`, until no class qualifies:
///
/// - a class qualifies when it is not `owl:Thing` nor `precious`, has a named
///   superclass other than `owl:Thing`, and is the superclass of at least one
///   and fewer than `threshold` `SubClassOf` axioms with a named subclass;
/// - every axiom any of whose objects, or any of whose annotations' properties
///   and values, is a qualifying class goes;
/// - the hierarchy among the objects still named is re-asserted from the
///   ontology as it was before any class went, across the classes that went —
///   so an entity only the removed axioms named is not reached, and each
///   remaining edge is asserted once more without annotations.
///
/// Returns the collapsed model and how many classes went.
pub fn collapse(model: Model, threshold: usize, precious: &HashSet<String>) -> anyhow::Result<(Model, usize)> {
    let original = model.clone();
    let mut model = model;
    let mut removed = 0usize;
    loop {
        let classes = classes_to_remove(&model, threshold, precious);
        if classes.is_empty() {
            break;
        }
        removed += classes.len();
        let surviving = remove_classes(&mut model, &classes)?;
        let mut shared = HashMap::new();
        let mut cross = HashMap::new();
        for bridge in crate::cmd::remove::span_gaps(&original, &surviving, &mut shared, &mut cross) {
            model.ont.insert(bridge);
        }
        merge_groups(&mut model, shared, cross);
    }
    Ok((model, removed))
}

/// Remove from `model` every axiom any of whose objects, or any of whose
/// annotations' properties and values, is one of `classes`, and return the
/// objects the axioms left name.
pub(crate) fn remove_classes(model: &mut Model, classes: &HashSet<String>) -> anyhow::Result<HashSet<Obj>> {
    let plain = objects::plain_datatype(model);
    let all = ["all".to_string()];
    let judge = Judge { selectors: &all, base: &[], partial: true, named_only: false, annotation_values: true, plain };
    let selected: HashSet<Obj> = classes.iter().map(|c| Obj::Entity(Kind::Class, RcStr::from(c.as_str()))).collect();
    let (doomed, surviving) = {
        let sel = Selection::new(model);
        let doomed: HashSet<_> = objects::judge_axioms(sel.axioms(), &selected, &judge)?.into_iter().collect();
        let mut surviving: HashSet<Obj> = HashSet::new();
        for ac in sel.axioms().iter().filter(|ac| !doomed.contains(**ac)) {
            surviving.extend(objects::axiom_objects(&ac.component, Some(&ac.ann), plain));
        }
        (doomed, surviving)
    };
    for ac in &doomed {
        model.ont.remove(ac);
    }
    Ok(surviving)
}

/// The classes one pass of [`collapse`] removes.
fn classes_to_remove(model: &Model, threshold: usize, precious: &HashSet<String>) -> HashSet<String> {
    let mut named_subs: HashMap<&str, usize> = HashMap::new();
    let mut has_named_super: HashSet<&str> = HashSet::new();
    for ac in model.ont.iter() {
        let Component::SubClassOf(sc) = &ac.component else { continue };
        if let CE::Class(sup) = &sc.sup {
            if matches!(sc.sub, CE::Class(_)) {
                *named_subs.entry(sup.0.as_ref()).or_default() += 1;
            }
            if let CE::Class(sub) = &sc.sub {
                if sup.0.as_ref() != OWL_THING {
                    has_named_super.insert(sub.0.as_ref());
                }
            }
        }
    }
    named_subs
        .into_iter()
        .filter(|&(class, n)| {
            class != OWL_THING && !precious.contains(class) && has_named_super.contains(class) && n < threshold
        })
        .map(|(class, _)| class.to_string())
        .collect()
}

/// Add one pass's blank-node groups to the model's, numbered above every group
/// the model already holds, so groups from different passes stay apart.
fn merge_groups(model: &mut Model, shared: HashMap<String, u64>, cross: HashMap<String, u64>) {
    let base = model.span_shared.values().chain(model.cross_shared.values()).copied().max().map_or(0, |m| m + 1);
    for (key, group) in shared {
        model.span_shared.entry(key).or_insert(base + group);
    }
    for (key, group) in cross {
        model.cross_shared.entry(key).or_insert(base + group);
    }
}
