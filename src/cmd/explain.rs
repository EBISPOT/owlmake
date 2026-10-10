//! `explain` — find a justification: a minimal set of axioms that entails a
//! subsumption, or that is inconsistent.
//!
//! Strategy: extract the ⊥-module for the query signature — it contains every
//! justification for the entailment — then grow a set outward from the terms of
//! the entailment until it entails, and black-box minimize that: drop axioms
//! whose removal preserves the entailment until none can be removed. When
//! `--max > 1`, multiple distinct justifications are enumerated with Reiter's
//! hitting-set tree. An inconsistency has no terms to grow from: its search
//! holds every logical axiom of the ontology.
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
    /// The axiom to explain, in Manchester syntax: `<SUBCLASS> SubClassOf
    /// <SUPERCLASS>` between two named classes, each named by a label, the
    /// short form of its IRI, a CURIE or an IRI. An axiom the ontology does
    /// not entail has no explanation.
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
    /// What to explain: `entailment` (default), `unsatisfiability` (why
    /// class(es) are unsatisfiable, i.e. C ⊑ owl:Nothing), or `inconsistency`
    /// (why the ontology is inconsistent, i.e. owl:Thing ⊑ owl:Nothing).
    #[arg(short = 'M', long, default_value = "entailment")]
    pub mode: String,
    /// For unsatisfiability mode, which unsatisfiable classes to explain: `all`;
    /// `root`, those no other unsatisfiable class's told definition explains;
    /// `most_general`, those with no unsatisfiable told superclass;
    /// `random:N`, the first N by IRI (all of them when N is not positive); a
    /// class's IRI or CURIE; or `list`, which explains none and writes their
    /// CURIEs to --explanation, one per line. Without it nothing is explained.
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
    /// Write the markdown report of the explanations to this file.
    #[arg(short = 'e', long)]
    pub explanation: Option<PathBuf>,
    /// Save the ontology this command was given, as it was given. The ontology
    /// of the justifications is what the next command in a chain receives.
    /// With neither this nor `--explanation`, the human-readable report goes
    /// to stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Serialization format for `--output`; inferred from its extension when
    /// omitted.
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
    if args.output.is_some() {
        crate::cmd::maybe_save(&mut model.clone(), args.output.as_deref(), args.format.as_deref())?;
    }

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
    // `--unsatisfiable list` explains none and lists the unsatisfiable classes.
    let mut listed: Option<Vec<String>> = None;
    let mode = args.mode.to_ascii_lowercase();
    let inconsistency = mode == "inconsistency";
    let targets: Vec<(String, String)> = match mode.as_str() {
        // Explain why the ontology is inconsistent, i.e. owl:Thing ⊑ owl:Nothing,
        // when the reasoner finds it so.
        "inconsistency" => {
            let (consistent, _) = crate::cmd::reason::coherence(&model, kind)?;
            if consistent {
                status!("explain: Ontology consistent, nothing to be done.");
                Vec::new()
            } else {
                vec![(OWL_THING.to_string(), OWL_NOTHING.to_string())]
            }
        }
        "unsatisfiability" => {
            // Explain why class(es) are unsatisfiable, i.e. C ⊑ owl:Nothing.
            let selector = Unsatisfiable::parse(args.unsatisfiable.as_deref())?;
            if selector == Unsatisfiable::None {
                // Nothing is asked of the reasoner, so only one that reads the
                // ontology as it is made can refuse it.
                if kind.reads_ontology_when_made() {
                    crate::cmd::reason::coherence(&model, kind)?;
                }
            }
            let (consistent, mut unsat) = if selector == Unsatisfiable::None {
                (true, Vec::new())
            } else {
                crate::cmd::reason::coherence(&model, kind)?
            };
            if !consistent && kind.refuses_inconsistent_ontology() {
                bail!("Inconsistent ontology");
            }
            // An EMPTY set is not an error: the report says there is nothing to
            // explain, which is what a QC step running this on a coherent
            // ontology wants.
            unsat.sort_by(|a, b| crate::io::natural_order::iri_cmp(a, b));
            let chosen: Vec<String> = match selector {
                Unsatisfiable::None => Vec::new(),
                Unsatisfiable::All => unsat,
                Unsatisfiable::Root => {
                    let mut failed = None;
                    let roots = crate::cmd::explain_unsat::roots(&model, &unsat, &mut |ce| {
                        satisfiable(&model, kind, ce).unwrap_or_else(|e| {
                            failed.get_or_insert(e);
                            true
                        })
                    });
                    if let Some(e) = failed {
                        return Err(e);
                    }
                    roots
                }
                Unsatisfiable::MostGeneral => crate::cmd::explain_unsat::most_general(&model, &unsat)?,
                Unsatisfiable::List => {
                    listed = Some(crate::cmd::explain_unsat::curie_list(&unsat));
                    Vec::new()
                }
                Unsatisfiable::Random(n) => {
                    if n > 0 {
                        unsat.truncate(n as usize);
                    }
                    unsat
                }
                Unsatisfiable::Class(term) => {
                    let c = select::expand_with_document_prefixes(&model, &term);
                    if !class_signature(&model).contains(&c) {
                        bail!(illegal_unsatisfiable(&term));
                    }
                    if !unsat.contains(&c) {
                        bail!("{c} is satisfiable (not entailed to be ⊑ owl:Nothing)");
                    }
                    vec![c]
                }
            };
            chosen.into_iter().map(|c| (c, OWL_NOTHING.to_string())).collect()
        }
        _ => {
            if mode != "entailment" {
                status!("explain: unknown mode '{}'; using 'entailment'", args.mode);
            }
            if let Some(axiom) = &args.axiom {
                // An axiom the ontology does not entail has no explanation.
                let (sub, sup) = crate::cmd::explain_axiom::subsumption(&model, axiom)?;
                if stated_subsumption(&model, &sub, &sup).is_some() || backend.decide(&model, &args.reasoner, &sub, &sup)
                {
                    vec![(sub, sup)]
                } else {
                    Vec::new()
                }
            } else {
                let (sub, sup) = match (&args.sub, &args.sup) {
                    (Some(sub), Some(sup)) => (term(&model, sub), term(&model, sup)),
                    _ => bail!("explain requires --axiom or both --sub and --sup (in entailment mode)"),
                };
                // Both ends must name a class the ontology actually uses, so
                // that a term that expanded to nothing — `EFO:0000998` against
                // a document that binds `efo:` and an OBO context that binds no
                // `EFO` — is an error about the query rather than a verdict on
                // the ontology.
                let classes = class_signature(&model);
                require_class(&classes, &sub)?;
                require_class(&classes, &sup)?;
                let (sub, sup) = (sub.iri, sup.iri);
                if stated_subsumption(&model, &sub, &sup).is_none() && !backend.decide(&model, &args.reasoner, &sub, &sup) {
                    bail!("{sub} ⊑ {sup} is not entailed by the ontology");
                }
                vec![(sub, sup)]
            }
        }
    };

    let mut report = String::new();
    let mut explained: Vec<crate::cmd::explain_markdown::Explained> = Vec::new();
    // The union of all justification axioms across targets: the ontology the
    // next command in a chain receives.
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
    // The examined ontology's label assertions, by subject: the ontology of the
    // justifications carries the labels of every term they name.
    let mut labels: std::collections::HashMap<String, Vec<AnnotatedComponent<RcStr>>> = Default::default();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == "http://www.w3.org/2000/01/rdf-schema#label" {
                if let horned_owl::model::AnnotationSubject::IRI(iri) = &aa.subject {
                    labels.entry(iri.as_ref().to_string()).or_default().push(ac.clone());
                }
            }
        }
    }
    // What the markdown report needs of the examined ontology: the label each
    // entity is shown with, and which ontology each axiom comes from.
    let md_labels = if args.explanation.is_some() { crate::cmd::rdfs_labels(&model) } else { Default::default() };
    let order = crate::io::natural_order::NaturalOrder::new(model.plain_literals_typed);
    // The root ontology's own logical axioms, which an inconsistency search's
    // set of axioms is made with room for.
    let root_logical = model
        .ont
        .iter()
        .filter(|ac| select::is_logical(&ac.component) && !model.imported_components.contains(*ac))
        .count();
    let provenance = crate::cmd::explain_markdown::Provenance {
        root: crate::build::model_ontology_id(&model).0,
        import: if model.inlined_imports.len() == 1 { model.inlined_imports.first().cloned() } else { None },
        imported: std::mem::take(&mut model.imported_components),
    };
    // The subsumption each target states as the ontology does, unannotated:
    // its own first justification, whether or not its module holds it.
    let stated: Vec<Option<AnnotatedComponent<RcStr>>> = targets
        .iter()
        .map(|(sub, sup)| if inconsistency { None } else { stated_subsumption(&model, sub, sup) })
        .collect();
    let module = if targets.is_empty() || inconsistency {
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
        let goal =
            if inconsistency { Goal::Inconsistency(root_logical, &provenance.imported) } else { Goal::Subsumption(sub, sup) };
        let (text, axioms, justifications) = explain_one(&module, backend, goal, stated[n].as_ref(), max);
        report.push_str(&text);
        justification_axioms.extend(axioms);
        for j in justifications {
            explained.push(crate::cmd::explain_markdown::Explained { sub: sub.clone(), sup: sup.clone(), axioms: j });
        }
    }

    // An ontology with nothing to explain still gets a report that says so, rather
    // than an empty file that reads as a check which did not run.
    if report.is_empty() {
        report.push_str("No explanations found.");
    }

    if let Some(lines) = &listed {
        // `--explanation` carries the list, a line per class.
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        if let Some(p) = &args.explanation {
            std::fs::write(p, text)?;
        } else if args.output.is_none() {
            print!("{text}");
        }
    } else {
        // `--explanation` carries the markdown report.
        if let Some(p) = &args.explanation {
            std::fs::write(p, crate::cmd::explain_markdown::report(&explained, &md_labels, &provenance, order))?;
        }
        if args.output.is_none() && args.explanation.is_none() {
            print!("{report}");
        }
    }
    // The model handed to the next command in a chain is the ontology OF the
    // justifications — the union of their axioms, empty when nothing needed
    // explaining — not the ontology that was examined. A chain ending
    // `explain … annotate --output x.ofn` therefore writes the explanation
    // ontology, carrying only the default prefix set.
    let mut terms: HashSet<String> = HashSet::new();
    for ac in &justification_axioms {
        terms.extend(crate::sig::typed_signature(&ac.component).into_iter().map(|(_, iri)| iri));
        terms.extend(ac.ann.iter().map(|a| a.ap.0.to_string()));
    }
    let mut just = SetOntology::new();
    for ac in justification_axioms {
        just.insert(ac);
    }
    for t in &terms {
        for ac in labels.get(t).into_iter().flatten() {
            just.insert(ac.clone());
        }
    }
    let mut out = Model::from_parts(just, horned_owl::curie::PrefixMapping::default());
    out.banner_labels = crate::cmd::rdfs_labels(&out);
    Ok(Some(out))
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
            Backend::Whelk => WhelkClassification::classify(model).subsumes(sub, sup),
        }
    }

    /// Is the ontology consistent?
    fn is_consistent(self, model: &Model) -> bool {
        match self {
            Backend::El => Reasoner::classify(model).is_consistent(),
            Backend::Dl => DlReasoner::classify(model).is_consistent(),
            Backend::Whelk => WhelkClassification::classify(model).is_consistent(),
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

/// What `--unsatisfiable` asks for. See [`Args::unsatisfiable`].
#[derive(Debug, PartialEq, Eq)]
enum Unsatisfiable {
    None,
    All,
    Root,
    MostGeneral,
    List,
    Random(i32),
    Class(String),
}

impl Unsatisfiable {
    /// The keywords are matched exactly. `random:` takes the integer after it,
    /// up to any further `:`.
    fn parse(value: Option<&str>) -> anyhow::Result<Unsatisfiable> {
        Ok(match value {
            None => Unsatisfiable::None,
            Some("all") => Unsatisfiable::All,
            Some("root") => Unsatisfiable::Root,
            Some("most_general") => Unsatisfiable::MostGeneral,
            Some("list") => Unsatisfiable::List,
            Some(s) if s.starts_with("random:") => {
                let n = s.split(':').nth(1).and_then(crate::java_number::parse_int);
                Unsatisfiable::Random(n.with_context(|| illegal_unsatisfiable(s))?)
            }
            Some(s) => Unsatisfiable::Class(s.to_string()),
        })
    }
}

fn illegal_unsatisfiable(value: &str) -> String {
    format!(
        "ILLEGAL UNSATISFIABLE ARGUMENT ERROR: {value}. Must have either a valid --unsatisfiable option (all, \
         root, most_general, random:n), where n is an integer."
    )
}

/// Whether `kind` finds the class expression `ce` satisfiable in `model`: a
/// fresh class made equivalent to it is not unsatisfiable.
fn satisfiable(model: &Model, kind: ReasonerKind, ce: &horned_owl::model::ClassExpression<RcStr>) -> anyhow::Result<bool> {
    const PROBE: &str = "urn:owlmake:explain:probe";
    let b = horned_owl::model::Build::new_rc();
    let mut probed = model.clone();
    probed.ont.insert(Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
        horned_owl::model::ClassExpression::Class(b.class(PROBE)),
        ce.clone(),
    ])));
    let (_, unsat) = crate::cmd::reason::coherence(&probed, kind)?;
    Ok(!unsat.iter().any(|c| c == PROBE))
}

/// The axiom `SubClassOf(sub, sup)` as the ontology or its imports state it,
/// without annotations.
fn stated_subsumption(model: &Model, sub: &str, sup: &str) -> Option<AnnotatedComponent<RcStr>> {
    use horned_owl::model::{ClassExpression, SubClassOf};
    let stated = AnnotatedComponent {
        component: Component::SubClassOf(SubClassOf {
            sub: ClassExpression::Class(model.build.class(sub)),
            sup: ClassExpression::Class(model.build.class(sup)),
        }),
        ann: Default::default(),
    };
    model.ont.i().contains(&stated).then_some(stated)
}

/// A query term as the caller typed it, with the IRI it expanded to.
struct Term {
    raw: String,
    iri: String,
}

fn term(model: &Model, raw: &str) -> Term {
    Term {
        raw: raw.to_string(),
        iri: select::expand_with_document_prefixes(model, raw),
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
        "`{raw}` did not expand to an IRI: no prefix `{pre}` is bound where it is read, so it \
         names no class.{hint}"
    )
}

/// What a justification is a justification of.
#[derive(Clone, Copy)]
enum Goal<'a> {
    /// The subsumption `sub ⊑ sup`.
    Subsumption(&'a str, &'a str),
    /// The inconsistency of the whole ontology, owl:Thing ⊑ owl:Nothing: the
    /// number of logical axioms the root ontology states itself, and the
    /// axioms its imports bring.
    Inconsistency(usize, &'a HashSet<AnnotatedComponent<RcStr>>),
}

impl<'a> Goal<'a> {
    /// The entailment's subclass and superclass.
    fn terms(self) -> (&'a str, &'a str) {
        match self {
            Goal::Subsumption(sub, sup) => (sub, sup),
            Goal::Inconsistency(..) => (OWL_THING, OWL_NOTHING),
        }
    }

    /// Does `model` entail it, as `backend` decides?
    fn holds(self, backend: Backend, model: &Model) -> bool {
        match self {
            Goal::Subsumption(sub, sup) => backend.is_subsumed(model, sub, sup),
            Goal::Inconsistency(..) => !backend.is_consistent(model),
        }
    }
}

/// Compute and format the justification(s) for a single entailment. Returns
/// the human-readable report and the deduplicated union of all axioms
/// appearing in any justification (for ontology output).
fn explain_one(
    model: &crate::model::Model,
    backend: Backend,
    goal: Goal,
    stated: Option<&AnnotatedComponent<RcStr>>,
    max: usize,
) -> (String, Vec<AnnotatedComponent<RcStr>>, Vec<Vec<AnnotatedComponent<RcStr>>>) {
    let t0 = std::time::Instant::now();
    let (sub, sup) = goal.terms();
    // A subsumption's justifications all lie in the ⊥-module for its two terms,
    // so the search never has to look outside it. An inconsistency's search
    // holds the whole ontology.
    let extracted;
    let module = match goal {
        Goal::Subsumption(..) => {
            let seed: HashSet<String> = [sub.to_string(), sup.to_string()].into_iter().collect();
            extracted = extract::extract(model, &seed, Method::Bot);
            &extracted
        }
        Goal::Inconsistency(..) => model,
    };

    let search = Search::new(module, backend, sub, sup);
    let justifications = {
        let _hb = crate::progress::Heartbeat::start(format!("explain: justifying {sub} ⊑ {sup}"));
        // One justification is the one black-box search finds, and an
        // inconsistency's justifications are the ones its hitting-set tree finds.
        if max == 1 || matches!(goal, Goal::Inconsistency(..)) {
            let axioms: Vec<AnnotatedComponent<RcStr>> = module.ont.iter().cloned().collect();
            let entails = |axs: &[&AnnotatedComponent<RcStr>]| -> bool {
                search.tests.set(search.tests.get() + 1);
                search.widest.set(search.widest.get().max(axs.len()));
                let mut ont = SetOntology::new();
                let mut entities: HashSet<String> = HashSet::new();
                for ac in axs {
                    ont.insert((*ac).clone());
                    entities.extend(crate::sig::typed_signature(&ac.component).into_iter().map(|(_, iri)| iri));
                }
                for e in &entities {
                    if let Some(decl) = search.declarations.get(e) {
                        ont.insert(decl.clone());
                    }
                }
                let m = Model::from_parts(ont, clone_prefixes(&module.prefixes));
                goal.holds(backend, &m)
            };
            match goal {
                // A subsumption the ontology states is its own justification.
                Goal::Subsumption(..) if stated.is_some() => stated.into_iter().map(|ac| vec![ac.clone()]).collect(),
                Goal::Subsumption(sub, sup) => crate::cmd::explain_blackbox::justification(&axioms, sub, sup, &entails)
                    .map(|j| vec![j])
                    .unwrap_or_default(),
                Goal::Inconsistency(root_logical, imported) => crate::cmd::explain_blackbox::inconsistency_justifications(
                    &axioms,
                    root_logical,
                    max,
                    &|ac| imported.contains(ac),
                    &entails,
                ),
            }
        } else {
            search.enumerate(max)
        }
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
    (report, union, justifications)
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
            if select::is_logical(&ac.component) {
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

