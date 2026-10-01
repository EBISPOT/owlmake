//! `check-align` — check that an ontology's classes sit under an upper ontology.
//!
//! A class is ALIGNED when the reasoner places it under one of the root classes:
//! every class of the upper ontology (`--upper-ontology`, `--upper-ontology-iri`,
//! `--use-cob`), each `--term`/`--term-file` class, and, with `--use-self`, the
//! roots the ontology declares for itself (`IAO:0000700`). The upper ontology is
//! merged with the input for classification, so an alignment the ontology only
//! reaches through the upper ontology's own hierarchy counts.
//!
//! Only classes in a `--base-iri` namespace are checked (all of them when none is
//! given); obsolete classes never are, and `--ignore-dangling` skips a class
//! nothing is said about. An unaligned class makes its ancestors unaligned too,
//! since none of them can be under a root either.
//!
//! `--report-output` lists unaligned classes, one IRI per line, sorted — always
//! written when asked for, empty if there are none. `--detail` chooses which:
//! `root` (default) only those directly under `owl:Thing`, `base-root` those with
//! no unaligned in-base ancestor, `all` every one. Any unaligned class is a
//! failure unless `--fail false`. The ontology passes through unchanged.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Context;
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotationSubject, AnnotationValue, ClassExpression as CE, Component, Literal, MutableOntology,
};

use crate::model::Model;
use crate::sig::kind;

const COB_IRI: &str = "http://purl.obolibrary.org/obo/cob.owl";
const PREFERRED_ROOT: &str = "http://purl.obolibrary.org/obo/IAO_0000700";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub format: Option<String>,

    /// Load the upper ontology from a file.
    #[arg(short = 'u', long)]
    pub upper_ontology: Option<PathBuf>,
    /// Load the upper ontology from an IRI.
    #[arg(short = 'U', long)]
    pub upper_ontology_iri: Option<String>,
    /// Use COB as the upper ontology (`<bool>`).
    #[arg(short = 'C', long, num_args = 1, default_missing_value = "true")]
    pub use_cob: Option<bool>,
    /// Check against the roots the ontology declares for itself (`<bool>`).
    #[arg(short = 'S', long, num_args = 1, default_missing_value = "true")]
    pub use_self: Option<bool>,
    /// A class to check alignment against (repeatable).
    #[arg(short = 't', long)]
    pub term: Vec<String>,
    /// A file listing classes to check alignment against.
    #[arg(short = 'T', long)]
    pub term_file: Vec<PathBuf>,
    /// Only check classes in this namespace (repeatable).
    #[arg(short = 'b', long = "base-iri")]
    pub base_iri: Vec<String>,
    /// Skip a class nothing is said about (`<bool>`).
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true")]
    pub ignore_dangling: Option<bool>,
    /// Which unaligned classes to report: `root`, `base-root` or `all`.
    #[arg(long, default_value = "root")]
    pub detail: String,
    /// Write the unaligned classes to this file.
    #[arg(short = 'O', long)]
    pub report_output: Option<PathBuf>,
    /// The reasoner to classify with.
    #[arg(short, long)]
    pub reasoner: Option<String>,
    /// Whether an unaligned class fails the command (`<bool>`, default true).
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub fail: Option<bool>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

#[derive(Clone, Copy, PartialEq)]
enum Detail {
    Root,
    BaseRoot,
    All,
}

pub fn step(piped: Option<Model>, args: &Args) -> anyhow::Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    let mut detail = match args.detail.to_ascii_lowercase().as_str() {
        "all" => Detail::All,
        "base-root" => Detail::BaseRoot,
        _ => Detail::Root,
    };
    // Without a namespace to be the base there are no base roots to speak of.
    if args.base_iri.is_empty() && detail == Detail::BaseRoot {
        detail = Detail::Root;
    }
    let in_base = |iri: &str| {
        args.base_iri.is_empty() || args.base_iri.iter().any(|b| iri.starts_with(b.as_str()))
    };

    let mut roots: HashSet<String> = HashSet::new();
    for t in &args.term {
        roots.insert(crate::cmd::select::expand(&model, t));
    }
    for file in &args.term_file {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("reading term file {}", file.display()))?;
        roots.extend(
            text.lines()
                .filter_map(crate::cmd::select::term_line)
                .map(|t| crate::cmd::select::expand(&model, t.trim())),
        );
    }
    if args.use_self.unwrap_or(false) {
        roots.extend(model.ont.iter().filter_map(|ac| match &ac.component {
            Component::OntologyAnnotation(a) if a.0.ap.0.as_ref() == PREFERRED_ROOT => {
                match &a.0.av {
                    AnnotationValue::IRI(i) => Some(i.as_ref().to_string()),
                    _ => None,
                }
            }
            _ => None,
        }));
    }

    let upper = if let Some(path) = &args.upper_ontology {
        Some(crate::io::load(path)?)
    } else if let Some(iri) = &args.upper_ontology_iri {
        Some(crate::io::load_iri(iri, None)?)
    } else if args.use_cob.unwrap_or(false) {
        Some(crate::io::load_iri(COB_IRI, None)?)
    } else {
        None
    };
    // The ontology is merged INTO the upper one, leaving it as it was for
    // whatever the command line does next.
    let classified = match upper {
        Some(mut upper) => {
            crate::cmd::resolve_imports_auto(&mut upper, None, None)?;
            roots.extend(classes(&upper));
            for ac in model.ont.iter() {
                upper.ont.insert(ac.clone());
            }
            upper
        }
        None => model.clone(),
    };
    roots.remove(OWL_THING);
    if roots.is_empty() {
        status!("check-align: no roots to validate against");
        crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
        return Ok(Some(model));
    }

    // Strict ancestors: a class the reasoner finds EQUIVALENT to a root is not
    // under it.
    use crate::cmd::reason::ReasonerKind;
    let kind = ReasonerKind::parse(args.reasoner.as_deref().unwrap_or("elk"))?;
    let (subsumptions, equivalences) = match kind {
        ReasonerKind::Hermit | ReasonerKind::JFact => {
            let r = crate::reason::DlReasoner::classify(&classified);
            (r.all_subsumptions(), r.equivalent_class_pairs())
        }
        _ => {
            let r = crate::reason::Reasoner::classify(&classified);
            (r.all_subsumptions(), r.equivalent_class_pairs())
        }
    };
    let equivalent: HashSet<(String, String)> = equivalences
        .into_iter()
        .flat_map(|(a, b)| [(a.clone(), b.clone()), (b, a)])
        .collect();
    let mut ancestors: HashMap<String, HashSet<String>> = HashMap::new();
    let mut descendants: HashMap<String, HashSet<String>> = HashMap::new();
    for (sub, sup) in subsumptions {
        if equivalent.contains(&(sub.clone(), sup.clone())) {
            continue;
        }
        descendants.entry(sup.clone()).or_default().insert(sub.clone());
        ancestors.entry(sub).or_default().insert(sup);
    }
    let none = HashSet::new();

    let said_about = facts_about(&classified);
    let ignore_dangling = args.ignore_dangling.unwrap_or(false);
    let mut unaligned: HashSet<String> = HashSet::new();
    for class in classes(&classified) {
        if class == OWL_THING || roots.contains(&class) || !in_base(&class) {
            continue;
        }
        let facts = said_about.get(&class);
        if ignore_dangling && facts.is_none_or(|f| f.count == 0) {
            continue;
        }
        if facts.is_some_and(|f| f.obsolete) || unaligned.contains(&class) {
            continue;
        }
        let above = ancestors.get(&class).unwrap_or(&none);
        if !above.iter().any(|a| roots.contains(a)) {
            unaligned.extend(above.iter().cloned());
            unaligned.insert(class);
        }
    }
    unaligned.remove(OWL_THING);

    if let Some(path) = &args.report_output {
        let trimmed: HashSet<&String> = match detail {
            Detail::All => HashSet::new(),
            // Only those with nothing above them but owl:Thing.
            Detail::Root => unaligned
                .iter()
                .filter(|c| !ancestors.get(*c).unwrap_or(&none).is_empty())
                .collect(),
            // Not those under another unaligned class of the base.
            Detail::BaseRoot => unaligned
                .iter()
                .filter(|c| in_base(c))
                .flat_map(|c| descendants.get(c).into_iter().flatten())
                .collect(),
        };
        let mut report: Vec<&String> = unaligned.iter().filter(|c| !trimmed.contains(c)).collect();
        report.sort();
        let text: String = report.iter().map(|iri| format!("{iri}\n")).collect();
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    }

    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    if !unaligned.is_empty() {
        let message = format!("ontology contains {} unaligned class(es)", unaligned.len());
        if args.fail.unwrap_or(true) {
            anyhow::bail!("check-align: {message}");
        }
        status!("check-align: {message}");
    }
    Ok(Some(model))
}

/// Every class in the signature, declared or only mentioned.
fn classes(model: &Model) -> HashSet<String> {
    model
        .ont
        .iter()
        .flat_map(|ac| crate::sig::typed_signature(&ac.component))
        .filter(|(k, _)| *k == kind::CLASS)
        .map(|(_, iri)| iri)
        .collect()
}

#[derive(Default)]
struct Facts {
    /// Axioms and annotations about the class, not counting a disjointness or a
    /// bare `SubClassOf owl:Thing`: a class with none is dangling.
    count: usize,
    obsolete: bool,
}

fn facts_about(model: &Model) -> HashMap<String, Facts> {
    let mut out: HashMap<String, Facts> = HashMap::new();
    let named = |ce: &CE<crate::model::Str>| match ce {
        CE::Class(c) => Some(c.0.as_ref().to_string()),
        _ => None,
    };
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(ax) => {
                let to_thing = matches!(&ax.sup, CE::Class(c) if c.0.as_ref() == OWL_THING);
                if let (Some(sub), false) = (named(&ax.sub), to_thing) {
                    out.entry(sub).or_default().count += 1;
                }
            }
            Component::EquivalentClasses(ax) => {
                for c in ax.0.iter().filter_map(named) {
                    out.entry(c).or_default().count += 1;
                }
            }
            Component::DisjointUnion(ax) => {
                out.entry(ax.0 .0.as_ref().to_string()).or_default().count += 1;
            }
            Component::AnnotationAssertion(ax) => {
                let AnnotationSubject::IRI(subject) = &ax.subject else { continue };
                let facts = out.entry(subject.as_ref().to_string()).or_default();
                facts.count += 1;
                let truthy = matches!(
                    &ax.ann.av,
                    AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri })
                        if literal == "true" && datatype_iri.as_ref().ends_with("#boolean")
                );
                if ax.ann.ap.0.as_ref() == crate::model::OWL_DEPRECATED && truthy {
                    facts.obsolete = true;
                }
            }
            _ => {}
        }
    }
    out
}
