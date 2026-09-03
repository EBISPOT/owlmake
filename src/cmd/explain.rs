//! `explain` — find a justification: a minimal set of axioms that entails a
//! subsumption.
//!
//! Strategy: extract the ⊥-module for the query signature — it contains every
//! justification for the entailment — then grow a set outward from the terms of
//! the entailment until it entails, and black-box minimize that: drop axioms
//! whose removal preserves the entailment until none can be removed. When
//! `--max > 1`, multiple distinct justifications are enumerated with Reiter's
//! hitting-set tree.
//!
//! Every step — which classes are unsatisfiable, whether the entailment holds,
//! and each of the entailment tests the minimization asks — is put to the
//! reasoner `--reasoner` names, so an entailment that needs a non-EL axiom is
//! justified rather than reported as having no explanation.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::Args as ClapArgs;
use horned_owl::model::{AnnotatedComponent, Component, Kinded, MutableOntology, RcStr};
use horned_owl::ontology::set::SetOntology;

use crate::cmd::reason::ReasonerKind;
use crate::cmd::select;
use crate::extract::{self, Method};
use crate::model::{clone_prefixes, Model};
use crate::reason::{DlReasoner, Reasoner, WhelkClassification};

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
    /// Reasoner that decides the entailment: `elk`/`emr`/`structural`/`owlmake`
    /// use the built-in EL reasoner (`owlmake` with union-elimination),
    /// `hermit`/`jfact` the hermit-rs OWL 2 DL reasoner, `whelk` the whelk-rs EL
    /// reasoner. The same reasoner decides the entailment, finds the
    /// unsatisfiable classes and minimizes the justifications. An unknown name
    /// is an error, as it is for `reason`.
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

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // `--reasoner` is validated up front, exactly as `reason` validates it: a
    // misspelt backend is an error, never a quiet fall-back to the EL engine
    // that then reports a verdict the requested reasoner never gave. Only
    // `owlmake` changes how the EL engine itself runs (union-elimination), and
    // that is set before any classification below.
    let kind = ReasonerKind::parse(&args.reasoner)?;
    crate::reason::el::set_whelk_mode(kind == ReasonerKind::Owlmake);
    let backend = Backend::of(kind);

    let max = args.max.max(1);

    // Determine the set of (sub, sup) entailments to explain, depending on mode.
    let mode = args.mode.to_ascii_lowercase();
    let targets: Vec<(String, String)> = match mode.as_str() {
        "unsatisfiability" | "inconsistency" => {
            // Explain why class(es) are unsatisfiable, i.e. C ⊑ owl:Nothing.
            let unsat = backend.unsatisfiable(&model, &args.reasoner);
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
            // Both ends must name a class the ontology actually uses. Without
            // this check a term that expanded to nothing — `EFO:0000998` against
            // a document that binds `efo:` and an OBO context that binds no
            // `EFO` — went to the reasoner as an unknown IRI and came back as
            // "not entailed": a verdict on the ontology, when the fault was in
            // the spelling of the query (EBISPOT/owlmake#2).
            let classes = class_signature(&model);
            require_class(&classes, &sub)?;
            require_class(&classes, &sup)?;
            let (sub, sup) = (sub.iri, sup.iri);
            if !backend.decide(&model, &args.reasoner, &sub, &sup) {
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

    // One ⊥-module for the signature of EVERY target, extracted from the input
    // once. Locality-based modules nest — the ⊥-module of a signature Σ carved
    // out of the ⊥-module of a signature Σ' ⊇ Σ is the one the whole input would
    // have given — so each target's own module is carved out of this one, and the
    // input is walked once rather than once per unsatisfiable class. The module
    // holds every justification for its signature, so the input itself is dropped
    // the moment it is built: the search below never looks at it again, and on a
    // multi-hundred-megabyte input that is most of the resident memory.
    let seed: HashSet<String> = targets.iter().flat_map(|(a, b)| [a.clone(), b.clone()]).collect();
    let module = if targets.is_empty() {
        model
    } else {
        let t0 = std::time::Instant::now();
        let m = extract::extract(&model, &seed, Method::Bot);
        drop(model);
        status!(
            "explain: ⊥-module for {} target(s): {} axioms in {:.1}s",
            targets.len(),
            m.ont.iter().count(),
            t0.elapsed().as_secs_f64()
        );
        m
    };

    for (n, (sub, sup)) in targets.iter().enumerate() {
        status!("explain: [{}/{}] {sub} ⊑ {sup}", n + 1, targets.len());
        let (text, axioms) = explain_one(&module, backend, sub, sup, max);
        report.push_str(&text);
        justification_axioms.extend(axioms);
    }

    // An ontology with nothing to explain still gets a report that says so, rather
    // than an empty file that reads as a check which did not run.
    if report.is_empty() {
        report.push_str("No explanations found.");
    }

    // `--output`: when a format is given or the path extension names an ontology
    // serialization, write an ontology of the justification axioms; otherwise fall
    // back to writing the human-readable report.
    if let Some(p) = &args.output {
        match resolve_ontology_format(args.format.as_deref(), p) {
            Some(fmt) => write_justification_ontology(&justification_axioms, p, fmt)?,
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
    // The model handed to the next command in a chain is the ontology OF the
    // justifications — the union of their axioms, empty when nothing needed
    // explaining — not the ontology that was examined. A chain ending
    // `explain … annotate --output x.ofn` therefore writes the explanation
    // ontology, carrying only the default prefix set.
    let mut just = SetOntology::new();
    for ac in justification_axioms {
        just.insert(ac);
    }
    Ok(Some(Model::from_parts(just, horned_owl::curie::PrefixMapping::default())))
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

/// Write the union of justification axioms to `path` in `fmt`. The
/// justification ontology is a NEW ontology: it carries the default prefix set,
/// not the examined document's.
fn write_justification_ontology(
    axioms: &[AnnotatedComponent<RcStr>],
    path: &std::path::Path,
    fmt: crate::io::Format,
) -> anyhow::Result<()> {
    let mut ont = SetOntology::new();
    for ac in axioms {
        ont.insert(ac.clone());
    }
    let mut out = Model::from_parts(ont, horned_owl::curie::PrefixMapping::default());
    crate::io::save_as(&mut out, path, fmt)
}

/// The reasoner that answers every question this command asks: which classes are
/// unsatisfiable, whether the entailment holds, and — for each of the thousands
/// of candidate axiom subsets the justification search tries — whether that
/// subset still entails it.
///
/// One reasoner throughout, because a justification is only a justification with
/// respect to the reasoner that saw the entailment. An entailment that needs a
/// non-EL axiom — a union, a cardinality restriction, an inverse-driven clash —
/// is invisible to the EL engine, so minimizing with EL an entailment a DL
/// reasoner decided returns "0 justifications" for something that plainly has
/// one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// The built-in EL reasoner (`elk`/`emr`/`structural`/`owlmake`).
    El,
    /// The hermit-rs OWL 2 DL reasoner (`hermit`/`jfact`).
    Dl,
    /// The whelk-rs EL reasoner (`whelk`).
    Whelk,
}

impl Backend {
    fn of(kind: ReasonerKind) -> Backend {
        match kind {
            ReasonerKind::Hermit | ReasonerKind::JFact => Backend::Dl,
            ReasonerKind::Whelk => Backend::Whelk,
            ReasonerKind::Elk | ReasonerKind::Owlmake | ReasonerKind::Structural | ReasonerKind::Emr => {
                Backend::El
            }
        }
    }

    /// The name of the engine, for the status lines that say which reasoner
    /// answered.
    fn engine(self) -> &'static str {
        match self {
            Backend::El => "the built-in EL reasoner",
            Backend::Dl => "hermit-rs",
            Backend::Whelk => "whelk-rs",
        }
    }

    /// Is `sub ⊑ sup` entailed?
    fn is_subsumed(self, model: &Model, sub: &str, sup: &str) -> bool {
        match self {
            Backend::El => Reasoner::classify(model).is_subsumed(sub, sup),
            Backend::Dl => DlReasoner::classify(model).is_subsumed(sub, sup),
            Backend::Whelk => WhelkClassification::classify(model)
                .all_subsumptions()
                .iter()
                .any(|(a, b)| a == sub && b == sup),
        }
    }

    /// The unsatisfiable named classes, as the requested reasoner sees them. A
    /// class only a DL reasoner finds unsatisfiable must be found here too, or
    /// `-M unsatisfiability -r hermit` explains a different set of classes from
    /// the one that reasoner reports.
    fn unsatisfiable(self, model: &Model, name: &str) -> Vec<String> {
        if self != Backend::El {
            status!("explain: unsatisfiable classes decided by {} (--reasoner {name})", self.engine());
        }
        match self {
            Backend::El => Reasoner::classify(model).unsatisfiable(),
            Backend::Dl => DlReasoner::classify(model).unsatisfiable(),
            Backend::Whelk => WhelkClassification::classify(model).unsatisfiable(),
        }
    }

    /// Decide the `--sub`/`--sup` entailment, naming the deciding engine on
    /// stderr whenever it is not the EL engine.
    fn decide(self, model: &Model, name: &str, sub: &str, sup: &str) -> bool {
        if self == Backend::El {
            if !matches!(ReasonerKind::parse(name), Ok(ReasonerKind::Elk) | Ok(ReasonerKind::Owlmake)) {
                status!("note: --reasoner {name}: explain decides the entailment with the built-in EL reasoner");
            }
            return self.is_subsumed(model, sub, sup);
        }
        let entailed = self.is_subsumed(model, sub, sup);
        status!(
            "explain: entailment decided by {} (--reasoner {name}): {sub} ⊑ {sup} = {entailed}; \
             justifications are minimized with the same reasoner",
            self.engine()
        );
        entailed
    }
}

/// The IRIs the ontology uses as classes: declared as one, or standing in a
/// class position of some axiom.
fn class_signature(model: &Model) -> HashSet<String> {
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::DeclareClass(dc) = &ac.component {
            out.insert(dc.0 .0.as_ref().to_string());
        }
        for (k, iri) in crate::sig::typed_signature(&ac.component) {
            if k == crate::sig::kind::CLASS {
                out.insert(iri);
            }
        }
    }
    out
}

/// A query term as the caller typed it, with the IRI it expanded to.
struct Term {
    raw: String,
    iri: String,
}

fn term(model: &Model, raw: &str) -> Term {
    Term {
        raw: raw.to_string(),
        iri: select::expand(model, raw),
    }
}

/// A query term must name a class of the ontology (`owl:Thing`/`owl:Nothing`
/// always count). The error says which step failed — a CURIE whose prefix is
/// bound nowhere, so it never became an IRI, or an IRI the ontology never uses
/// as a class — and, for an unexpanded CURIE, names any class whose IRI ends in
/// the OBO-style `PREFIX_LOCAL`, since that is almost always the term meant:
/// `EFO:0000998` against EFO, whose document binds `efo:` and whose ids are
/// `…/efo/EFO_0000998`.
fn require_class(classes: &HashSet<String>, t: &Term) -> anyhow::Result<()> {
    let (raw, iri) = (t.raw.as_str(), t.iri.as_str());
    if iri == OWL_THING || iri == OWL_NOTHING || classes.contains(iri) {
        return Ok(());
    }
    let has_scheme = iri.starts_with("http://") || iri.starts_with("https://") || iri.starts_with("urn:");
    if has_scheme {
        bail!(
            "<{iri}> is not a class in the ontology: it is neither declared as one nor used in a \
             class position (from `{raw}`)"
        );
    }
    let Some((pre, local)) = iri.split_once(':') else {
        bail!("`{raw}` is neither an IRI nor a CURIE with a bound prefix, and names no class in the ontology");
    };
    let suffix = format!("{pre}_{local}");
    let mut candidates: Vec<&String> = classes
        .iter()
        .filter(|c| c.ends_with(&suffix) && c[..c.len() - suffix.len()].ends_with(['/', '#']))
        .collect();
    candidates.sort();
    let hint = match candidates.as_slice() {
        [] => String::new(),
        [one] => format!(
            " The ontology has a class <{one}>; if that is the term, bind the prefix with \
             --prefix \"{pre}: {ns}\" or give the IRI.",
            ns = &one[..one.len() - local.len()]
        ),
        many => format!(
            " Classes whose IRI ends in `{suffix}`: {}.",
            many.iter().map(|c| format!("<{c}>")).collect::<Vec<_>>().join(", ")
        ),
    };
    bail!(
        "`{raw}` did not expand to an IRI: the prefix `{pre}` is bound neither in the ontology's \
         prefix map nor in the bundled OBO context, so it names no class.{hint}"
    )
}

/// Resolve the entailment to explain from `--axiom` (Manchester
/// `SUB SubClassOf SUP`) or the `--sub`/`--sup` pair.
fn resolve_entailment(model: &Model, args: &Args) -> anyhow::Result<(Term, Term)> {
    if let Some(axiom) = &args.axiom {
        return parse_subclassof_axiom(model, axiom);
    }
    match (&args.sub, &args.sup) {
        (Some(sub), Some(sup)) => Ok((term(model, sub), term(model, sup))),
        _ => bail!("explain requires --axiom or both --sub and --sup (in entailment mode)"),
    }
}

/// Parse a Manchester `<SUBCLASS> SubClassOf <SUPERCLASS>` axiom into the
/// expanded subclass/superclass terms. Only named classes on either side are
/// supported (matching what the justification machinery can explain).
fn parse_subclassof_axiom(model: &Model, axiom: &str) -> anyhow::Result<(Term, Term)> {
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
    let parse_side = |side: &str| -> anyhow::Result<Term> {
        let iri = match crate::io::manchester::parse_class_expression(&model.build, &model.prefixes, side) {
            Some(horned_owl::model::ClassExpression::Class(c)) => c.0.as_ref().to_string(),
            Some(_) => bail!("--axiom: only named classes are supported (got a complex expression in '{side}')"),
            None => select::expand(model, side), // fall back to CURIE/IRI expansion
        };
        Ok(Term { raw: side.to_string(), iri })
    };
    Ok((parse_side(sub_str)?, parse_side(sup_str)?))
}


/// Compute and format the justification(s) for a single `sub ⊑ sup` entailment.
/// Returns the human-readable report and the deduplicated union of all axioms
/// appearing in any justification (for ontology output).
fn explain_one(
    model: &crate::model::Model,
    backend: Backend,
    sub: &str,
    sup: &str,
    max: usize,
) -> (String, Vec<AnnotatedComponent<RcStr>>) {
    let t0 = std::time::Instant::now();
    // Shrink to the ⊥-module for the two terms: it contains every justification
    // for the entailment, so the search never has to look outside it.
    let seed: HashSet<String> = [sub.to_string(), sup.to_string()].into_iter().collect();
    let module = extract::extract(model, &seed, Method::Bot);

    let search = Search::new(&module, backend, sub, sup);
    let justifications = {
        let _hb = crate::progress::Heartbeat::start(format!("explain: justifying {sub} ⊑ {sup}"));
        search.enumerate(max)
    };
    // The two numbers that say what the search cost: how many entailment tests it
    // asked, and how big the largest ontology it classified was. The module's
    // candidate count is the ceiling both are measured against.
    status!(
        "explain: {} justification(s) for {sub} ⊑ {sup} in {:.1}s ({} entailment tests, widest {} axioms, \
         over {} candidate axioms)",
        justifications.len(),
        t0.elapsed().as_secs_f64(),
        search.tests.get(),
        search.widest.get(),
        search.candidates.len()
    );

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

/// The justification search for one entailment, over the candidate axioms of its
/// ⊥-module.
///
/// Every question is a *test*: does this subset of the candidates still entail
/// `sub ⊑ sup`? Each test builds a small ontology and classifies it, so the
/// number of tests, and the size of the subsets tested, is the whole cost of the
/// command. Two things keep both small:
///
/// - **Expansion before contraction.** A justification is a handful of axioms;
///   the module around it is tens of thousands. Growing a set outward from the
///   terms of the entailment until it entails costs a logarithmic number of
///   tests and lands on a set the size of the justification's neighbourhood.
///   Contracting the whole module instead costs one test per axiom in it — on a
///   24,000-axiom module that is 24,000 classifications of a 24,000-axiom
///   ontology, which is hours per class.
/// - **Trials carry only what reasoning uses.** A trial ontology is the chosen
///   axioms plus the declarations of the entities they mention. The module's
///   annotation assertions — usually most of its components — never enter a
///   trial, and never appear in a justification.
struct Search<'a> {
    backend: Backend,
    sub: &'a str,
    sup: &'a str,
    /// The logical axioms a justification may be built from.
    candidates: Vec<AnnotatedComponent<RcStr>>,
    /// `candidates[i]`'s signature, precomputed: the expansion walks it, and
    /// every trial ontology collects declarations from it.
    sigs: Vec<Vec<String>>,
    /// Declarations, by the entity declared.
    declarations: std::collections::HashMap<String, AnnotatedComponent<RcStr>>,
    /// Candidate axioms by every entity in their signature — the graph the
    /// expansion walks outward from the terms of the entailment.
    by_entity: std::collections::HashMap<String, Vec<usize>>,
    prefixes: horned_owl::curie::PrefixMapping,
    /// Entailment tests performed, for the status line.
    tests: std::cell::Cell<usize>,
    /// Axioms in the largest set tested — what the expansion phase exists to
    /// keep well below the size of the module.
    widest: std::cell::Cell<usize>,
}

impl<'a> Search<'a> {
    fn new(module: &Model, backend: Backend, sub: &'a str, sup: &'a str) -> Search<'a> {
        let mut candidates = Vec::new();
        let mut declarations = std::collections::HashMap::new();
        for ac in module.ont.iter() {
            if is_logical(&ac.component) {
                candidates.push(ac.clone());
            } else if is_declaration(&ac.component) {
                if let Some((_, iri)) = crate::sig::typed_signature(&ac.component).into_iter().next() {
                    declarations.insert(iri, ac.clone());
                }
            }
        }
        let sigs: Vec<Vec<String>> = candidates
            .iter()
            .map(|ac| {
                let mut s: Vec<String> = crate::sig::typed_signature(&ac.component)
                    .into_iter()
                    .map(|(_, iri)| iri)
                    .collect();
                s.sort();
                s.dedup();
                s
            })
            .collect();
        let mut by_entity: std::collections::HashMap<String, Vec<usize>> = Default::default();
        for (i, sig) in sigs.iter().enumerate() {
            for iri in sig {
                by_entity.entry(iri.clone()).or_default().push(i);
            }
        }
        Search {
            backend,
            sub,
            sup,
            candidates,
            sigs,
            declarations,
            by_entity,
            prefixes: clone_prefixes(&module.prefixes),
            tests: std::cell::Cell::new(0),
            widest: std::cell::Cell::new(0),
        }
    }

    /// Does the subset given by `idx` entail `sub ⊑ sup`?
    fn entails(&self, idx: &[usize]) -> bool {
        self.tests.set(self.tests.get() + 1);
        self.widest.set(self.widest.get().max(idx.len()));
        let mut ont = SetOntology::new();
        let mut entities: HashSet<&str> = HashSet::new();
        for &i in idx {
            ont.insert(self.candidates[i].clone());
            entities.extend(self.sigs[i].iter().map(String::as_str));
        }
        for e in entities {
            if let Some(decl) = self.declarations.get(e) {
                ont.insert(decl.clone());
            }
        }
        let m = Model::from_parts(ont, clone_prefixes(&self.prefixes));
        self.backend.is_subsumed(&m, self.sub, self.sup)
    }

    /// Grow a subset of the axioms allowed by `mask` outward from the terms of
    /// the entailment until it entails, and return it — a superset of some
    /// justification, for [`Search::contract`] to minimize.
    ///
    /// The walk is breadth-first over the signature graph: the axioms mentioning
    /// `sub`, then the axioms mentioning what those mention, and so on. It stops
    /// to test after each wave, and the wave size doubles, so a justification a
    /// few hops away is found in a logarithmic number of tests on sets that stay
    /// close to its own size. When nothing more is reachable and the reachable
    /// part does not entail — a GCI keyed on nothing in the signature can do that
    /// — the whole allowed set is the answer, which is where the search would
    /// have started without expansion.
    ///
    /// `None` means the allowed axioms do not entail at all: on the root of the
    /// hitting-set tree that the entailment is gone, on a branch that this branch
    /// is dead.
    fn expand(&self, mask: &[bool]) -> Option<Vec<usize>> {
        let mut chosen: Vec<usize> = Vec::new();
        let mut taken = vec![false; self.candidates.len()];
        let mut queue: std::collections::VecDeque<String> = Default::default();
        let mut seen: HashSet<String> = HashSet::new();
        // owl:Thing seeds the walk alongside the two terms: a domain axiom or a
        // GCI written against ⊤ belongs to every class's neighbourhood.
        for e in [self.sub, self.sup, OWL_THING] {
            if seen.insert(e.to_string()) {
                queue.push_back(e.to_string());
            }
        }
        let mut wave = 64usize;
        loop {
            let mut added = 0usize;
            'wave: while added < wave {
                let Some(entity) = queue.pop_front() else { break };
                let Some(axioms) = self.by_entity.get(&entity) else { continue };
                for &i in axioms {
                    if taken[i] || !mask[i] {
                        continue;
                    }
                    if added == wave {
                        // A hub entity — a class thousands of axioms mention —
                        // must not swallow the wave whole: put it back and take
                        // the rest of it next round. Resuming re-walks its
                        // axioms, and the ones already taken are skipped.
                        queue.push_front(entity);
                        break 'wave;
                    }
                    taken[i] = true;
                    chosen.push(i);
                    added += 1;
                    for iri in &self.sigs[i] {
                        if seen.insert(iri.clone()) {
                            queue.push_back(iri.clone());
                        }
                    }
                }
            }
            if added == 0 {
                // Reachability is exhausted.
                if !chosen.is_empty() && self.entails(&chosen) {
                    return Some(chosen);
                }
                let all: Vec<usize> = (0..self.candidates.len()).filter(|&i| mask[i]).collect();
                return self.entails(&all).then_some(all);
            }
            if self.entails(&chosen) {
                return Some(chosen);
            }
            wave = wave.saturating_mul(2);
        }
    }

    /// Shrink an entailing set to a justification: a subset that entails and
    /// whose every proper subset does not.
    ///
    /// Two phases. The first drops whole windows at a time — a tenth of the set
    /// per test — and repeats while that keeps paying, so a set of tens of
    /// thousands falls to tens in a few dozen tests rather than as many tests as
    /// it has axioms. The second goes axiom by axiom over what survives, which
    /// is what makes the result minimal. Minimality is intrinsic: a minimal
    /// entailing subset of the module is a justification of the ontology,
    /// whatever set the search happened to start from.
    fn contract(&self, mut set: Vec<usize>) -> Vec<usize> {
        while set.len() > 32 {
            let before = set.len();
            let window = (set.len() / 10).max(1);
            let mut start = 0;
            while start < set.len() {
                let end = (start + window).min(set.len());
                let trial: Vec<usize> =
                    set[..start].iter().chain(&set[end..]).copied().collect();
                if self.entails(&trial) {
                    set = trial; // the whole window is redundant
                } else {
                    start = end;
                }
            }
            // Windows the justification is spread across cannot be dropped; when
            // a whole pass buys little, the axiom-by-axiom phase is the cheaper
            // way through what is left.
            if set.len() * 5 > before * 4 {
                break;
            }
        }
        let mut i = 0;
        while i < set.len() {
            let mut trial = set.clone();
            trial.remove(i);
            if self.entails(&trial) {
                set = trial; // redundant — drop permanently
            } else {
                i += 1; // needed — keep
            }
        }
        set
    }

    /// One justification within the axioms `mask` allows, or `None` if they do
    /// not entail.
    fn find(&self, mask: &[bool]) -> Option<Vec<usize>> {
        self.expand(mask).map(|s| self.contract(s))
    }

    /// Enumerate up to `max` distinct justifications with Reiter's hitting-set
    /// tree: find one, then look for others in the ontology with one of its
    /// axioms removed, repeating over the growing set of removals.
    fn enumerate(&self, max: usize) -> Vec<Vec<AnnotatedComponent<RcStr>>> {
        let n = self.candidates.len();
        let mut found: Vec<IndexSet> = Vec::new();
        let mut queue: std::collections::VecDeque<IndexSet> = Default::default();
        let mut seen_paths: HashSet<IndexSet> = HashSet::new();
        queue.push_back(IndexSet::new());
        seen_paths.insert(IndexSet::new());

        while let Some(removed) = queue.pop_front() {
            if found.len() >= max {
                break;
            }
            let mut mask = vec![true; n];
            for &i in &removed {
                mask[i] = false;
            }
            let Some(just) = self.find(&mask) else {
                continue; // entailment already broken on this branch
            };
            let just_set: IndexSet = just.into_iter().collect();
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
            .map(|set| set.into_iter().map(|i| self.candidates[i].clone()).collect())
            .collect()
    }
}

fn is_declaration(c: &Component<RcStr>) -> bool {
    matches!(
        c,
        Component::DeclareClass(_)
            | Component::DeclareObjectProperty(_)
            | Component::DeclareDataProperty(_)
            | Component::DeclareAnnotationProperty(_)
            | Component::DeclareNamedIndividual(_)
            | Component::DeclareDatatype(_)
    )
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
