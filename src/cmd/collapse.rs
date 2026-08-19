//! `collapse` — reduce a class hierarchy to a set of "precious" terms,
//! reconnecting each kept term to its nearest kept ancestors through removed
//! intermediates.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{ClassExpression as CE, Component, MutableOntology, RcStr, SubClassOf};
use horned_owl::ontology::set::SetOntology;

use crate::cmd::select;
use crate::model::{clone_prefixes, Model};
use crate::sig;

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
    /// Minimum number of named subclasses an intermediate class must have to be
    /// kept (default 2). Non-precious intermediates with fewer named subclasses
    /// are collapsed and their hierarchy is bridged.
    #[arg(short = 't', long)]
    pub threshold: Option<usize>,

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
    // --term/--term-file and --precious/--precious-terms name the same set.
    let mut terms = args.term.clone();
    terms.extend(args.precious.iter().cloned());
    let mut term_files = args.term_file.clone();
    term_files.extend(args.precious_terms.iter().cloned());
    let keep = select::collect_terms(&model, &terms, &term_files)?;

    // `collapse` removes *sparse intermediate* classes — those with at least one
    // but fewer than `--threshold` named subclasses — and bridges the hierarchy
    // across them. Leaves (no subclasses), top-level classes (directly under
    // owl:Thing only), owl:Thing, and precious terms are kept. The default
    // threshold is 2.
    let precious = keep; // the protected ("precious") set
    let threshold = args.threshold.unwrap_or(2);

    // Named direct-subclass counts and named-superclass edges.
    let mut sub_count: HashMap<String, usize> = HashMap::new();
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(sub), CE::Class(sup)) = (&sc.sub, &sc.sup) {
                let sub_s = sub.0.as_ref().to_string();
                let sup_s = sup.0.as_ref().to_string();
                *sub_count.entry(sup_s.clone()).or_default() += 1;
                parents.entry(sub_s).or_default().push(sup_s);
            }
        }
    }
    let has_named_super = |c: &str| {
        parents
            .get(c)
            .is_some_and(|ps| ps.iter().any(|p| p != OWL_THING))
    };

    // Removed = non-precious intermediates with 1..threshold named subclasses and
    // a named superclass. (Leaves never appear in `sub_count`, so are kept.)
    let mut removed: HashSet<String> = HashSet::new();
    for (cls, &cnt) in &sub_count {
        if cls == OWL_THING || precious.contains(cls) {
            continue;
        }
        if has_named_super(cls) && cnt < threshold {
            removed.insert(cls.clone());
        }
    }

    // Rewire each kept class to its nearest kept ancestors, walking up the named
    // class hierarchy to bridge the removed intermediates.
    let mut edges: HashSet<(String, String)> = HashSet::new();
    for (sub, sups) in &parents {
        if removed.contains(sub) {
            continue; // a removed class contributes no edges of its own
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = sups.clone();
        while let Some(p) = stack.pop() {
            if !seen.insert(p.clone()) {
                continue;
            }
            if removed.contains(&p) {
                if let Some(gp) = parents.get(&p) {
                    stack.extend(gp.iter().cloned());
                }
            } else if &p != sub {
                edges.insert((sub.clone(), p.clone()));
            }
        }
    }

    // Keep every axiom except: named SubClassOf edges (rebuilt below) and any
    // axiom that references a removed class — a removed class must not survive in
    // the remains of an axiom that mentioned it.
    let mut ont: SetOntology<RcStr> = SetOntology::new();
    for ac in model.ont.iter() {
        let drop = match &ac.component {
            Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::Import(_)
            | Component::OntologyAnnotation(_) => false,
            Component::SubClassOf(sc)
                if matches!((&sc.sub, &sc.sup), (CE::Class(_), CE::Class(_))) =>
            {
                true
            }
            other => sig::signature(other).iter().any(|s| removed.contains(s.as_str())),
        };
        if !drop {
            ont.insert(ac.clone());
        }
    }
    for (sub, sup) in &edges {
        ont.insert(Component::SubClassOf(SubClassOf {
            sub: CE::Class(model.build.class(sub.clone())),
            sup: CE::Class(model.build.class(sup.clone())),
        }));
    }

    status!(
        "collapse: removed {} intermediate class(es) (threshold {}), {} reconnected subclass edge(s)",
        removed.len(),
        threshold,
        edges.len()
    );
    let mut result = Model::from_parts(ont, clone_prefixes(&model.prefixes));
    crate::cmd::maybe_save(&mut result, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(result))
}
