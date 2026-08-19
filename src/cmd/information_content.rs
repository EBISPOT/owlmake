//! `information-content` — annotate every class with a structural
//! information-content (IC) score derived from the subclass hierarchy.
//!
//! The IC of a class is the standard Resnik-style structural measure: a class
//! that subsumes many others (a general class like *biological process*) carries
//! little information, while a leaf carries the most. Concretely, with `total`
//! the number of named classes and `desc(t)` the size of `t`'s reflexive,
//! transitive subclass set (i.e. `t` plus everything below it):
//!
//! ```text
//!   IC(t) = -ln(desc(t) / total) * 100 / ln(total)
//! ```
//!
//! so a leaf (`desc = 1`) scores 100 and the top of the hierarchy scores 0. The
//! same normalization produces the `normalizedSubClassInformationContent` values
//! `crate::ubergraph::ic` writes, and the score is rendered with that module's
//! `format_g6` — six significant digits, trailing zeros stripped — so both emit
//! their `xsd:decimal` literals in one decimal syntax. The numbers are not
//! interchangeable, though: `total` here counts named classes (owl:Thing and
//! owl:Nothing excluded), while there it counts every term appearing as a subject
//! in the redundant graph, properties included.
//!
//! By default IC is computed over the subclass axioms already present in the
//! ontology (their transitive closure). Pass `--reasoner` to classify first so
//! the score reflects the *inferred* hierarchy — equivalently, chain
//! `owlmake reason … information-content …`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{
    Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, ClassExpression, Component,
    DeclareAnnotationProperty, Literal, MutableOntology,
};

use crate::ubergraph::ic::format_g6;
use crate::ubergraph::iri;

const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Output file (defaults to stdout / the chain).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Annotation property IRI/CURIE to carry the IC score
    /// (default: `reasoner:normalizedInformationContent`).
    #[arg(short = 'p', long, value_name = "IRI")]
    pub property: Option<String>,
    /// Classify with this reasoner before computing IC, so the score reflects the
    /// *inferred* subclass hierarchy: e.g. `elk`, `structural`, `emr`, `owlmake`,
    /// `whelk`, `hermit`, or `jfact`. When omitted, IC is computed over the
    /// subclass axioms already in the ontology (chain `owlmake reason …
    /// information-content …` for the same effect with finer control).
    #[arg(short = 'r', long = "reasoner", value_name = "NAME")]
    pub reasoner: Option<String>,
    /// Compute IC over the full **relation graph** rather than only the subclass
    /// hierarchy: materialize ubergraph's redundant existential-relation graph
    /// (every `SubClassOf C (R some D)` becomes an `R` edge, transitively closed)
    /// and count references across *all* edges, matching ubergraph's
    /// `normalizedInformationContent`. Implies EL reasoning, so `--reasoner` is
    /// ignored. Without this flag IC is the subclass-only structural measure.
    #[arg(long)]
    pub relations: bool,
    /// Also emit an integer `reasoner:referenceCount` per class — the size of its
    /// reflexive-transitive subclass set (or, with `--relations`, its total
    /// reference count across all relation edges).
    #[arg(long)]
    pub reference_count: bool,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // Per-class `(referenceCount, IC)`: either the full existential-relation
    // measure or the subclass-only structural one. owl:Thing/owl:Nothing are
    // dropped so they
    // neither skew totals nor get a score.
    let scored: Vec<(String, usize, f64)> = if args.relations {
        relation_scores(&model)
    } else {
        subclass_scores(&model, args)
    };
    let scored: Vec<(String, usize, f64)> =
        scored.into_iter().filter(|(t, _, _)| named(t)).collect();

    if scored.is_empty() {
        status!("information-content: no named classes to score");
        crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
        return Ok(Some(model));
    }

    // Resolve the annotation property and declare it (plus referenceCount's, if
    // requested) so the result carries no dangling property reference.
    let prop_iri = match &args.property {
        Some(p) => crate::cmd::select::expand(&model, p),
        None => iri::NORMALIZED_IC.to_string(),
    };
    let ic_ap = model.build.annotation_property(prop_iri.as_str());
    model.ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
        ic_ap.clone(),
    )));
    let rc_ap = args.reference_count.then(|| {
        let ap = model.build.annotation_property(iri::REFERENCE_COUNT);
        model.ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
            ap.clone(),
        )));
        ap
    });

    let decimal = model.build.iri(XSD_DECIMAL);
    let integer = model.build.iri(XSD_INTEGER);

    let n = scored.len();
    for (class, count, ic) in scored {
        let subject = AnnotationSubject::IRI(model.build.iri(class.as_str()));
        model.ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
            subject: subject.clone(),
            ann: Annotation { ann: Default::default(),
                ap: ic_ap.clone(),
                av: AnnotationValue::Literal(Literal::Datatype {
                    literal: format_g6(ic),
                    datatype_iri: decimal.clone(),
                }),
            },
        }));
        if let Some(rc_ap) = &rc_ap {
            model.ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
                subject,
                ann: Annotation { ann: Default::default(),
                    ap: rc_ap.clone(),
                    av: AnnotationValue::Literal(Literal::Datatype {
                        literal: count.to_string(),
                        datatype_iri: integer.clone(),
                    }),
                },
            }));
        }
    }

    status!(
        "information-content: scored {n} class(es){}",
        if args.relations {
            " over the relation graph".to_string()
        } else {
            match &args.reasoner {
                Some(r) => format!(" over the {r}-inferred hierarchy"),
                None => String::new(),
            }
        }
    );

    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// True for class IRIs that should be scored (everything but owl:Thing/Nothing).
fn named(iri: &str) -> bool {
    iri != OWL_THING && iri != OWL_NOTHING
}

/// Subclass-only structural IC: `(class, desc(t), IC(t))` over the subclass
/// hierarchy (asserted, or `--reasoner`-inferred), sorted by class.
fn subclass_scores(model: &crate::model::Model, args: &Args) -> Vec<(String, usize, f64)> {
    // Direct subclass edges (child → parents) over named classes, plus the full
    // class set (so isolated classes still score).
    let mut classes: HashSet<String> = HashSet::new();
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::DeclareClass(d) = &ac.component {
            let c = d.0 .0.as_ref();
            if named(c) {
                classes.insert(c.to_string());
            }
        }
    }
    let edges = match &args.reasoner {
        Some(r) => reasoned_subsumptions(model, r),
        None => asserted_subclass_edges(model),
    };
    for (sub, sup) in edges {
        // Self-edges carry no hierarchy information; the reflexive count is added
        // for every class by `scores`.
        if sub == sup || !named(&sub) || !named(&sup) {
            continue;
        }
        classes.insert(sub.clone());
        classes.insert(sup.clone());
        parents.entry(sub).or_default().push(sup);
    }
    let score = scores(&classes, &parents);
    let mut out: Vec<(String, usize, f64)> =
        score.into_iter().map(|(t, (d, ic))| (t, d, ic)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Full existential-relation IC: classify with the EL reasoner, materialize the
/// redundant existential-relation graph over every object property, and compute
/// the reference-based IC (`crate::ubergraph::ic::reference_ic`).
fn relation_scores(model: &crate::model::Model) -> Vec<(String, usize, f64)> {
    status!("information-content: classifying with the EL reasoner …");
    let reasoner = crate::reason::Reasoner::classify(model);
    if !reasoner.is_consistent() {
        status!("information-content: WARNING ontology is inconsistent; IC may be degenerate");
    }
    // An empty property set means "materialize over every object property", which
    // is what a full relation graph requires.
    let all_props: HashSet<String> = HashSet::new();
    let redundant = crate::ubergraph::edges::redundant_graph(&reasoner, &all_props);
    status!("information-content: relation graph has {} edge(s)", redundant.len());
    crate::ubergraph::ic::reference_ic(&redundant)
}

/// For every class, compute `(desc(t), IC(t))` where `desc(t)` is the size of
/// `t`'s reflexive, transitive subclass set and `IC(t) = -ln(desc/total) *
/// 100/ln(total)`.
///
/// `desc(t)` is found by walking up from every class and tallying each ancestor
/// (and the class itself). Treating `parents` as *direct* edges keeps this
/// correct whether they came from the asserted hierarchy or a reasoner's
/// transitive closure — the upward reachable set is the same either way.
fn scores(
    classes: &HashSet<String>,
    parents: &HashMap<String, Vec<String>>,
) -> HashMap<String, (usize, f64)> {
    let mut desc: HashMap<&str, usize> = classes.iter().map(|c| (c.as_str(), 0usize)).collect();
    for start in classes {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack = vec![start.as_str()];
        seen.insert(start.as_str());
        while let Some(node) = stack.pop() {
            if let Some(ps) = parents.get(node) {
                for p in ps {
                    if seen.insert(p.as_str()) {
                        stack.push(p.as_str());
                    }
                }
            }
        }
        for ancestor in seen {
            *desc.get_mut(ancestor).unwrap() += 1;
        }
    }

    let total = classes.len() as f64;
    let max_ic = total.ln(); // = -ln(1/total)
    let scale = if max_ic > 0.0 { 100.0 / max_ic } else { 0.0 };
    classes
        .iter()
        .map(|c| {
            let d = desc[c.as_str()];
            let ic = -((d as f64) / total).ln() * scale;
            (c.clone(), (d, ic))
        })
        .collect()
}

/// Direct `SubClassOf(Class(sub), Class(sup))` pairs (named classes only).
fn asserted_subclass_edges(model: &crate::model::Model) -> Vec<(String, String)> {
    use ClassExpression as CE;
    let mut out = Vec::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(sub), CE::Class(sup)) = (&sc.sub, &sc.sup) {
                out.push((sub.0.as_ref().to_string(), sup.0.as_ref().to_string()));
            }
        }
    }
    out
}

/// Classify `model` with `reasoner` and return its subsumption `(sub, sup)`
/// pairs. Dispatches across the EL/whelk/DL backends exactly as `measure` does.
fn reasoned_subsumptions(model: &crate::model::Model, reasoner: &str) -> Vec<(String, String)> {
    let lc = reasoner.to_ascii_lowercase();
    match lc.as_str() {
        // hermit-rs (DL) and whelk-rs (EL) both build for wasm, so every backend
        // is available in the browser too (see src/reason/mod.rs).
        "hermit" | "jfact" => crate::reason::DlReasoner::classify(model).all_subsumptions(),
        "whelk" => crate::reason::WhelkClassification::classify(model).direct_subsumptions(),
        _ => {
            if !matches!(lc.as_str(), "elk" | "structural" | "emr" | "owlmake") {
                status!("information-content: unknown reasoner '{reasoner}'; using the EL reasoner");
            }
            crate::reason::el::set_whelk_mode(lc == "owlmake");
            crate::reason::Reasoner::classify(model).all_subsumptions()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diamond() -> (HashSet<String>, HashMap<String, Vec<String>>) {
        // root ⊒ {mid} ⊒ {leafA, leafB}
        let classes: HashSet<String> = ["root", "mid", "leafA", "leafB"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut parents: HashMap<String, Vec<String>> = HashMap::new();
        parents.insert("mid".into(), vec!["root".into()]);
        parents.insert("leafA".into(), vec!["mid".into()]);
        parents.insert("leafB".into(), vec!["mid".into()]);
        (classes, parents)
    }

    #[test]
    fn ic_leaves_are_max_root_is_zero() {
        let (classes, parents) = diamond();
        let s = scores(&classes, &parents);
        // desc: root=4, mid=3, leaves=1.
        assert_eq!(s["root"].0, 4);
        assert_eq!(s["mid"].0, 3);
        assert_eq!(s["leafA"].0, 1);
        // root subsumes everything → IC 0; leaves → 100 (the maximum).
        assert!((s["root"].1 - 0.0).abs() < 1e-9);
        assert!((s["leafA"].1 - 100.0).abs() < 1e-9);
        assert!((s["leafB"].1 - 100.0).abs() < 1e-9);
        // mid sits in between.
        assert!(s["mid"].1 > 0.0 && s["mid"].1 < 100.0);
        assert_eq!(format_g6(s["mid"].1), "20.7519");
    }

    #[test]
    fn ic_counts_a_shared_descendant_once() {
        // A multi-parent leaf must be counted once per ancestor, not duplicated.
        let classes: HashSet<String> =
            ["top", "p1", "p2", "leaf"].iter().map(|s| s.to_string()).collect();
        let mut parents: HashMap<String, Vec<String>> = HashMap::new();
        parents.insert("p1".into(), vec!["top".into()]);
        parents.insert("p2".into(), vec!["top".into()]);
        parents.insert("leaf".into(), vec!["p1".into(), "p2".into()]);
        let s = scores(&classes, &parents);
        // top reaches all 4; p1/p2 reach themselves + leaf = 2; leaf = 1.
        assert_eq!(s["top"].0, 4);
        assert_eq!(s["p1"].0, 2);
        assert_eq!(s["p2"].0, 2);
        assert_eq!(s["leaf"].0, 1);
    }
}
