//! `explain` — find a justification: a minimal set of axioms that entails a
//! subsumption.
//!
//! Strategy: extract the ⊥-module for the query signature (small, and
//! guaranteed to contain every justification), then black-box minimize — drop
//! axioms whose removal preserves the entailment until none can be removed. When
//! `--max > 1`, multiple distinct justifications are enumerated with Reiter's
//! hitting-set tree.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::Args as ClapArgs;
use horned_owl::model::{AnnotatedComponent, Component, Kinded, MutableOntology, RcStr};
use horned_owl::ontology::set::SetOntology;

use crate::cmd::select;
use crate::extract::{self, Method};
use crate::model::{clone_prefixes, Model};
use crate::reason::Reasoner;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// The axiom to explain, in Manchester syntax. Only `<SUBCLASS>
    /// SubClassOf <SUPERCLASS>` axioms are supported; this is the ROBOT-style
    /// alternative to owlmake's --sub/--sup pair.
    #[arg(short = 'a', long)]
    pub axiom: Option<String>,
    /// The subclass of the entailment to explain (IRI/CURIE). owlmake extension;
    /// alternative to --axiom. Required unless --axiom/--mode is given.
    #[arg(long)]
    pub sub: Option<String>,
    /// The superclass of the entailment to explain (IRI/CURIE). owlmake
    /// extension; alternative to --axiom.
    #[arg(long)]
    pub sup: Option<String>,
    /// What to explain: `entailment` (default), or
    /// `inconsistency`/`unsatisfiability` (explain why class(es) are
    /// unsatisfiable, i.e. C ⊑ owl:Nothing).
    #[arg(short = 'M', long, default_value = "entailment")]
    pub mode: String,
    /// For unsatisfiability/inconsistency mode, which class(es) to
    /// explain: `all`, `root`, or a specific CLASS IRI/CURIE. Default
    /// `all`.
    #[arg(short = 'u', long)]
    pub unsatisfiable: Option<String>,
    /// Reasoner to use: `elk`/`structural`/`emr`/ `owlmake` use the
    /// built-in EL reasoner.
    #[arg(short = 'r', long, default_value = "elk")]
    pub reasoner: String,
    /// Maximum number of justifications (distinct minimal explanations) to
    /// retrieve. Default 1.
    #[arg(short = 'm', long, default_value_t = 1)]
    pub max: usize,
    /// Write the justification(s) to this file. Same content as --output;
    /// provided for compatibility with existing invocations.
    #[arg(short = 'e', long)]
    pub explanation: Option<PathBuf>,
    /// Output file for the justification. With `--format` (or an ontology file
    /// extension) this is an ontology of the union of justification axioms, as in
    /// `robot explain`; otherwise the human-readable report is written. Defaults
    /// to stdout (the report).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Serialization format for the `--output` ontology of justification
    /// axioms: owl/owx/ofn/obo/omn/ttl/json. When omitted the format is
    /// inferred from the `--output` extension.
    #[arg(short = 'f', long)]
    pub format: Option<String>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // `--reasoner`: `owlmake` enables union-elimination, others run on the EL
    // engine (with a note for non-EL choices). Set before classification.
    crate::reason::configure(&args.reasoner);

    let lc = args.reasoner.to_ascii_lowercase();
    let dl = matches!(lc.as_str(), "hermit" | "jfact");
    if !dl && !matches!(lc.as_str(), "elk" | "structural" | "emr" | "owlmake") {
        status!(
            "note: reasoner '{}' not supported for explain; using the built-in EL reasoner",
            args.reasoner
        );
    }

    let max = args.max.max(1);

    // Determine the set of (sub, sup) entailments to explain, depending on mode.
    let mode = args.mode.to_ascii_lowercase();
    let targets: Vec<(String, String)> = match mode.as_str() {
        "unsatisfiability" | "inconsistency" => {
            // Explain why class(es) are unsatisfiable, i.e. C ⊑ owl:Nothing.
            let unsat = Reasoner::classify(&model).unsatisfiable();
            let selector = args.unsatisfiable.as_deref().unwrap_or("all");
            let chosen: Vec<String> = match selector.to_ascii_lowercase().as_str() {
                "all" | "root" | "most_general" => {
                    // owlmake does not distinguish root vs derived unsatisfiable
                    // classes; treat `root` like `all`.
                    //
                    // An EMPTY set is not an error. `ExplainOperation
                    // .explainUnsatisfiableClasses` just returns no explanations,
                    // and `ExplainCommand` writes the (empty) markdown and exits 0
                    // — which is the whole point of MONDO's `explain_unsat.owl`
                    // QC step: it passes when the ontology is coherent.
                    let mut u = unsat;
                    u.sort();
                    u
                }
                // `random:n` is not random: `explainUnsatisfiableClasses` SORTS the
                // unsatisfiable classes and takes the first `n` (`ExplainOperation`
                // line 107 onward). MONDO's `test` runs `--unsatisfiable random:10`,
                // and reading it as a term selected nothing, so the check failed
                // with "random:10 is satisfiable" on a perfectly coherent ontology.
                s if s.starts_with("random:") => {
                    let n: usize = s["random:".len()..].parse().with_context(|| {
                        format!(
                            "ILLEGAL UNSATISFIABLE ARGUMENT ERROR: {selector}. Must have either a \
                             valid --unsatisfiable option (all, root, most_general, random:n), \
                             where n is an integer."
                        )
                    })?;
                    let mut u = unsat;
                    u.sort();
                    u.truncate(n);
                    u
                }
                _ => {
                    let c = select::expand(&model, selector);
                    if !unsat.contains(&c) {
                        bail!("{c} is satisfiable (not entailed to be ⊑ owl:Nothing)");
                    }
                    vec![c]
                }
            };
            chosen
                .into_iter()
                .map(|c| (c, OWL_NOTHING.to_string()))
                .collect()
        }
        _ => {
            if mode != "entailment" {
                status!("explain: unknown mode '{}'; using 'entailment'", args.mode);
            }
            // entailment mode: take the pair from --axiom or --sub/--sup.
            let (sub, sup) = resolve_entailment(&model, args)?;
            // The DL reasoners answer the entailment check directly (via the DL
            // tableau's `is_subsumed`, bypassing the classification candidate-set);
            // the justification below is still computed with the EL machinery.
            // The DL reasoner (hermit-rs) is not compiled into the wasm build;
            // there the entailment check uses the built-in EL reasoner.
            #[cfg(not(target_arch = "wasm32"))]
            let entailed = if dl {
                crate::reason::DlReasoner::classify(&model).is_subsumed(&sub, &sup)
            } else {
                Reasoner::classify(&model).is_subsumed(&sub, &sup)
            };
            #[cfg(target_arch = "wasm32")]
            let entailed = Reasoner::classify(&model).is_subsumed(&sub, &sup);
            if dl {
                status!("explain: DL ({lc}) entailment check: {sub} ⊑ {sup} = {entailed}");
            }
            if !entailed {
                bail!("{sub} ⊑ {sup} is not entailed by the ontology");
            }
            vec![(sub, sup)]
        }
    };

    let mut report = String::new();
    // The union of all justification axioms across targets — used when `--output`
    // (with `--format` or an ontology extension) asks for an ontology rather than
    // the human-readable report.
    let mut justification_axioms: Vec<AnnotatedComponent<RcStr>> = Vec::new();
    for (sub, sup) in &targets {
        let (text, axioms) = explain_one(&model, sub, sup, max);
        report.push_str(&text);
        justification_axioms.extend(axioms);
    }

    // `--output`: when a format is given or the path extension names an ontology
    // serialization, write an ontology of the justification axioms; otherwise fall
    // back to writing the human-readable report.
    if let Some(p) = &args.output {
        match resolve_ontology_format(args.format.as_deref(), p) {
            Some(fmt) => write_justification_ontology(&model, &justification_axioms, p, fmt)?,
            None => std::fs::write(p, &report)?,
        }
    }
    // `--explanation` always carries the human-readable report, whatever form
    // `--output` was asked for.
    if let Some(p) = &args.explanation {
        std::fs::write(p, &report)?;
    }
    if args.output.is_none() && args.explanation.is_none() {
        print!("{report}");
    }
    Ok(Some(model))
}

/// Resolve the ontology serialization for `--output`: an explicit `--format`
/// wins (erroring on an unknown name), otherwise infer from the path extension.
/// Returns `None` when neither names a known ontology format, signalling that the
/// human-readable report should be written instead.
fn resolve_ontology_format(format: Option<&str>, output: &std::path::Path) -> Option<crate::io::Format> {
    match format {
        Some(name) => crate::io::Format::from_name(name).ok(),
        None => crate::io::Format::from_path(output).ok(),
    }
}

/// Write the union of justification axioms (plus their declarations, for a
/// well-formed result) to `path` in `fmt`, reusing the source prefixes.
fn write_justification_ontology(
    source: &crate::model::Model,
    axioms: &[AnnotatedComponent<RcStr>],
    path: &std::path::Path,
    fmt: crate::io::Format,
) -> anyhow::Result<()> {
    let mut ont = SetOntology::new();
    for ac in axioms {
        ont.insert(ac.clone());
    }
    let mut out = Model::from_parts(ont, clone_prefixes(&source.prefixes));
    crate::io::save_as(&mut out, path, fmt)
}

/// Resolve the entailment to explain from `--axiom` (Manchester
/// `SUB SubClassOf SUP`) or the `--sub`/`--sup` pair, returning expanded IRIs.
fn resolve_entailment(model: &crate::model::Model, args: &Args) -> anyhow::Result<(String, String)> {
    if let Some(axiom) = &args.axiom {
        return parse_subclassof_axiom(model, axiom);
    }
    match (&args.sub, &args.sup) {
        (Some(sub), Some(sup)) => Ok((select::expand(model, sub), select::expand(model, sup))),
        _ => bail!("explain requires --axiom or both --sub and --sup (in entailment mode)"),
    }
}

/// Parse a Manchester `<SUBCLASS> SubClassOf <SUPERCLASS>` axiom into the
/// expanded subclass/superclass IRIs. Only named classes on either side are
/// supported (matching what the justification machinery can explain).
fn parse_subclassof_axiom(
    model: &crate::model::Model,
    axiom: &str,
) -> anyhow::Result<(String, String)> {
    // Split on the SubClassOf keyword (case-insensitive), tolerating extra
    // whitespace.
    let lower = axiom.to_ascii_lowercase();
    let Some(pos) = lower.find("subclassof") else {
        bail!("--axiom must be a Manchester 'A SubClassOf B' axiom");
    };
    let sub_str = axiom[..pos].trim();
    let sup_str = axiom[pos + "subclassof".len()..].trim();
    if sub_str.is_empty() || sup_str.is_empty() {
        bail!("--axiom must name both a subclass and a superclass");
    }
    let parse_side = |side: &str| -> anyhow::Result<String> {
        match crate::io::manchester::parse_class_expression(&model.build, &model.prefixes, side) {
            Some(horned_owl::model::ClassExpression::Class(c)) => Ok(c.0.as_ref().to_string()),
            Some(_) => bail!("--axiom: only named classes are supported (got a complex expression in '{side}')"),
            None => Ok(select::expand(model, side)), // fall back to CURIE/IRI expansion
        }
    };
    Ok((parse_side(sub_str)?, parse_side(sup_str)?))
}

/// Compute and format the justification(s) for a single `sub ⊑ sup` entailment.
/// Returns the human-readable report and the deduplicated union of all axioms
/// appearing in any justification (for ontology output).
fn explain_one(
    model: &crate::model::Model,
    sub: &str,
    sup: &str,
    max: usize,
) -> (String, Vec<AnnotatedComponent<RcStr>>) {
    // Shrink to the ⊥-module for the two terms (small, and guaranteed to contain
    // every justification).
    let seed: HashSet<String> = [sub.to_string(), sup.to_string()].into_iter().collect();
    let module = extract::extract(model, &seed, Method::Bot);

    // Candidate logical axioms (exclude declarations/annotations/metadata) and
    // the fixed non-logical support.
    let candidates: Vec<AnnotatedComponent<RcStr>> = module
        .ont
        .iter()
        .filter(|ac| is_logical(&ac.component))
        .cloned()
        .collect();
    let support: Vec<AnnotatedComponent<RcStr>> = module
        .ont
        .iter()
        .filter(|ac| !is_logical(&ac.component))
        .cloned()
        .collect();

    let justifications = compute_justifications(&candidates, &support, &module, sub, sup, max);

    let mut report = format!(
        "{} justification(s) for {sub} ⊑ {sup}:\n",
        justifications.len()
    );
    let mut union: Vec<AnnotatedComponent<RcStr>> = Vec::new();
    for (n, just) in justifications.iter().enumerate() {
        report.push_str(&format!("Justification {} ({} axioms):\n", n + 1, just.len()));
        for ac in just {
            report.push_str(&format!("  {:?}: {:?}\n", ac.component.kind(), ac.component));
            if !union.contains(ac) {
                union.push(ac.clone());
            }
        }
    }
    (report, union)
}

/// A single justification, identified by the indices of the axioms (into the
/// candidate list) it contains.
type IndexSet = std::collections::BTreeSet<usize>;

/// Enumerate up to `max` distinct justifications for `sub ⊑ sup` using Reiter's
/// hitting-set tree: find one justification, then search for others in the
/// ontology with one of its axioms removed, repeating over the growing set of
/// "hitting sets". Each justification is found by black-box contraction.
fn compute_justifications(
    candidates: &[AnnotatedComponent<RcStr>],
    support: &[AnnotatedComponent<RcStr>],
    module: &Model,
    sub: &str,
    sup: &str,
    max: usize,
) -> Vec<Vec<AnnotatedComponent<RcStr>>> {
    let all: IndexSet = (0..candidates.len()).collect();
    let mut found: Vec<IndexSet> = Vec::new();

    // Worklist of "removed axiom" sets to explore (hitting-set tree nodes).
    let mut queue: std::collections::VecDeque<IndexSet> = std::collections::VecDeque::new();
    let mut seen_paths: HashSet<IndexSet> = HashSet::new();
    queue.push_back(IndexSet::new());
    seen_paths.insert(IndexSet::new());

    while let Some(removed) = queue.pop_front() {
        if found.len() >= max {
            break;
        }
        // Working axiom set = all candidates minus the removed indices.
        let working: Vec<usize> = all.difference(&removed).copied().collect();
        if !entails_idx(&working, candidates, support, module, sub, sup) {
            continue; // entailment already broken on this branch
        }
        // Minimize to a justification within the working set.
        let just = minimize(&working, candidates, support, module, sub, sup);
        let just_set: IndexSet = just.iter().copied().collect();
        if !found.contains(&just_set) {
            found.push(just_set.clone());
            if found.len() >= max {
                break;
            }
        }
        // Branch: remove each axiom of this justification in turn.
        for &ax in &just_set {
            let mut next = removed.clone();
            next.insert(ax);
            if seen_paths.insert(next.clone()) {
                queue.push_back(next);
            }
        }
    }

    found
        .into_iter()
        .map(|set| set.into_iter().map(|i| candidates[i].clone()).collect())
        .collect()
}

/// Black-box contraction: shrink `working` (indices) to a minimal subset that
/// still entails `sub ⊑ sup`.
fn minimize(
    working: &[usize],
    candidates: &[AnnotatedComponent<RcStr>],
    support: &[AnnotatedComponent<RcStr>],
    module: &Model,
    sub: &str,
    sup: &str,
) -> Vec<usize> {
    let mut just: Vec<usize> = working.to_vec();
    let mut i = 0;
    while i < just.len() {
        let mut trial = just.clone();
        trial.remove(i);
        if entails_idx(&trial, candidates, support, module, sub, sup) {
            just.remove(i); // redundant — drop permanently
        } else {
            i += 1; // needed — keep
        }
    }
    just
}

/// Does the candidate subset given by `idx` (plus the fixed support) entail it?
fn entails_idx(
    idx: &[usize],
    candidates: &[AnnotatedComponent<RcStr>],
    support: &[AnnotatedComponent<RcStr>],
    module: &Model,
    sub: &str,
    sup: &str,
) -> bool {
    let axioms: Vec<AnnotatedComponent<RcStr>> = idx.iter().map(|&i| candidates[i].clone()).collect();
    entails(&axioms, support, module, sub, sup)
}

fn is_logical(c: &Component<RcStr>) -> bool {
    !matches!(
        c,
        Component::DeclareClass(_)
            | Component::DeclareObjectProperty(_)
            | Component::DeclareDataProperty(_)
            | Component::DeclareAnnotationProperty(_)
            | Component::DeclareNamedIndividual(_)
            | Component::DeclareDatatype(_)
            | Component::AnnotationAssertion(_)
            | Component::OntologyAnnotation(_)
            | Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::Import(_)
    )
}

/// Does `axioms` (plus the fixed `support`) entail `sub ⊑ sup`?
fn entails(
    axioms: &[AnnotatedComponent<RcStr>],
    support: &[AnnotatedComponent<RcStr>],
    template: &Model,
    sub: &str,
    sup: &str,
) -> bool {
    let mut ont = SetOntology::new();
    for ac in support.iter().chain(axioms.iter()) {
        ont.insert(ac.clone());
    }
    let m = Model::from_parts(ont, clone_prefixes(&template.prefixes));
    Reasoner::classify(&m).is_subsumed(sub, sup)
}
