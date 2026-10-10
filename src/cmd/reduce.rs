//! `reduce` — remove the `SubClassOf` axioms of the root ontology that the
//! rest of its class hierarchy makes redundant, as the named reasoner
//! classifies it.
//!
//! Over every class expression (the default), each anonymous expression of a
//! root `SubClassOf` is stood for by a class of its own, equivalent to it. The
//! reasoner classifies the import closure's `SubClassOf` axioms and object
//! property characteristics, its sub-property axioms and property chains with
//! `--include-subproperties`, and those equivalences — nothing else. An axiom
//! `C ⊑ X` is redundant when another asserted superclass of `C` lies strictly
//! below `X`; for an anonymous `C`, also when a superclass of `C` that is the
//! subclass of a root `SubClassOf` itself does.
//!
//! Between named classes only (`--named-classes-only`), the reasoner
//! classifies the whole import closure. An axiom `A ⊑ B` is kept when, walking
//! down from the top node, `A` is among the classes of a node directly below a
//! node `B` belongs to.
//!
//! An inconsistent ontology loses nothing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Build, ClassExpression as CE, Component, MutableOntology,
    ObjectPropertyExpression as OPE, RcStr, SubClassOf,
};
use horned_owl::ontology::set::SetOntology;

use crate::cmd::reason::ReasonerKind;
use crate::model::Model;
use crate::reason::told::Told;
use crate::reason::{Reasoner, WhelkClassification};

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Reasoner the hierarchy is classified with: `elk`, `hermit`, `jfact`,
    /// `whelk`, `structural`, or `owlmake` (the built-in EL reasoner with
    /// union-elimination).
    #[arg(short = 'r', long, default_value = "elk")]
    pub reasoner: String,
    /// Preserve redundant axioms that carry annotations (`true` or `yes` in
    /// any case; default false).
    #[arg(short = 'p', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub preserve_annotated_axioms: Option<bool>,
    /// Classify the sub-property axioms and property chains too (`true` or
    /// `yes` in any case; default false), so that an existential restriction
    /// entailed through them is redundant.
    #[arg(short = 's', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub include_subproperties: Option<bool>,
    /// Reduce only the axioms between named classes, over the classification
    /// of the whole ontology (`true` or `yes` in any case; default false).
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub named_classes_only: Option<bool>,
    /// Reduce by entailment instead: drop an axiom iff the ontology minus it
    /// still entails it, as the built-in EL reasoner decides over ⊥-modules.
    /// Slower on huge ontologies.
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub exact: Option<bool>,
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
    // A reasoner name that is no reasoner fails before anything is loaded.
    let kind = ReasonerKind::parse_without_emr(&args.reasoner)?;
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let opts = ReduceOptions {
        reasoner: kind,
        preserve_annotated: args.preserve_annotated_axioms.unwrap_or(false),
        named_classes_only: args.named_classes_only.unwrap_or(false),
        include_subproperties: args.include_subproperties.unwrap_or(false),
    };
    let mut reduced = if args.exact.unwrap_or(false) {
        crate::reason::el::set_whelk_mode(kind == ReasonerKind::Owlmake);
        reduce_exact(&model, opts.preserve_annotated, opts.named_classes_only, opts.include_subproperties)
    } else {
        reduce_with_options(&model, &opts)?
    };
    crate::cmd::maybe_save(&mut reduced, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(reduced))
}

/// How [`reduce_with_options`] reduces: the reasoner that classifies, and
/// which axioms take part.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReduceOptions {
    /// The reasoner the hierarchy is classified with (`--reasoner`).
    pub reasoner: ReasonerKind,
    /// Keep a redundant axiom that carries annotations
    /// (`--preserve-annotated-axioms`).
    pub preserve_annotated: bool,
    /// Reduce only the axioms between named classes
    /// (`--named-classes-only`).
    pub named_classes_only: bool,
    /// Classify the sub-property axioms and property chains too
    /// (`--include-subproperties`).
    pub include_subproperties: bool,
}

impl Default for ReduceOptions {
    fn default() -> Self {
        ReduceOptions {
            reasoner: ReasonerKind::Elk,
            preserve_annotated: false,
            named_classes_only: false,
            include_subproperties: false,
        }
    }
}

/// Remove the redundant `SubClassOf` axioms of `model`'s root ontology, as
/// `elk` finds them over every class expression.
pub fn reduce(model: &Model) -> Result<Model> {
    reduce_with_options(model, &ReduceOptions::default())
}

/// Remove the redundant `SubClassOf` axioms of `model`'s root ontology, as
/// `opts` asks.
pub fn reduce_with_options(model: &Model, opts: &ReduceOptions) -> Result<Model> {
    crate::reason::el::set_whelk_mode(opts.reasoner == ReasonerKind::Owlmake);
    let redundant = if opts.named_classes_only {
        redundant_between_named_classes(model, opts)?
    } else {
        redundant_over_expressions(model, opts)?
    };
    let mut out = SetClone::new(model);
    out.retain(|ac| !redundant.contains(ac));
    let mut result = out.into_model();
    result.carry_meta_from(model);
    Ok(result)
}

/// The root ontology's `SubClassOf` axioms: the model's own, not those an
/// import lent it.
fn root_subclass_axioms(model: &Model) -> Vec<(&AnnotatedComponent<RcStr>, &SubClassOf<RcStr>)> {
    model
        .ont
        .iter()
        .filter(|ac| !model.imported_components.contains(*ac))
        .filter_map(|ac| match &ac.component {
            Component::SubClassOf(sc) => Some((ac, sc)),
            _ => None,
        })
        .collect()
}

/// The class standing for `ce`: itself when it is a named class, otherwise a
/// class of its own, equivalent to it, one for each distinct expression.
fn stand_in<'m>(
    ce: &'m CE<RcStr>,
    names: &mut HashMap<&'m CE<RcStr>, String>,
    equivalences: &mut Vec<Component<RcStr>>,
    build: &Build<RcStr>,
) -> String {
    if let CE::Class(c) = ce {
        return c.0.to_string();
    }
    if let Some(name) = names.get(ce) {
        return name.clone();
    }
    let name = format!("urn:owlmake:reduce#{}", names.len());
    equivalences.push(Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
        CE::Class(build.class(name.clone())),
        ce.clone(),
    ])));
    names.insert(ce, name.clone());
    name
}

/// The redundant `SubClassOf` axioms of the root over every class expression.
fn redundant_over_expressions(model: &Model, opts: &ReduceOptions) -> Result<HashSet<AnnotatedComponent<RcStr>>> {
    let build = Build::new_rc();
    let mut names: HashMap<&CE<RcStr>, String> = HashMap::new();
    let mut equivalences: Vec<Component<RcStr>> = Vec::new();
    // Each root axiom as (axiom, subclass, superclass, whether the subclass is
    // anonymous), and the superclasses the root asserts of each subclass.
    let mut tested: Vec<(&AnnotatedComponent<RcStr>, String, String, bool)> = Vec::new();
    let mut asserted: HashMap<String, HashSet<String>> = HashMap::new();
    for (ac, sc) in root_subclass_axioms(model) {
        let sub = stand_in(&sc.sub, &mut names, &mut equivalences, &build);
        let sup = stand_in(&sc.sup, &mut names, &mut equivalences, &build);
        asserted.entry(sub.clone()).or_default().insert(sup.clone());
        tested.push((ac, sub, sup, !matches!(sc.sub, CE::Class(_))));
    }
    let mut ont: SetOntology<RcStr> = SetOntology::new();
    for ac in model.ont.iter() {
        let classified = match &ac.component {
            Component::SubClassOf(_)
            | Component::TransitiveObjectProperty(_)
            | Component::ReflexiveObjectProperty(_)
            | Component::IrreflexiveObjectProperty(_)
            | Component::SymmetricObjectProperty(_)
            | Component::AsymmetricObjectProperty(_)
            | Component::FunctionalObjectProperty(_)
            | Component::InverseFunctionalObjectProperty(_) => true,
            Component::SubObjectPropertyOf(_) => opts.include_subproperties,
            _ => false,
        };
        if classified {
            ont.insert(ac.clone());
        }
    }
    for eq in equivalences {
        ont.insert(eq);
    }
    let classified =
        Classification::of(&Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes)), opts.reasoner)?;
    if !classified.consistent() {
        status!("reduce: the ontology is inconsistent; no axiom is removed");
        return Ok(HashSet::new());
    }
    let mut redundant = HashSet::new();
    for (ac, sub, sup, anonymous) in tested {
        if opts.preserve_annotated && !ac.ann.is_empty() {
            continue;
        }
        let mut is_redundant = asserted[&sub].iter().any(|y| classified.above(y, &sup));
        if !is_redundant && anonymous {
            is_redundant =
                asserted.keys().any(|between| classified.above(&sub, between) && classified.above(between, &sup));
        }
        if is_redundant {
            redundant.insert(ac.clone());
        }
    }
    Ok(redundant)
}

/// The redundant `SubClassOf` axioms of the root between named classes: every
/// one but those the walk down the taxonomy finds directly below their
/// superclass.
fn redundant_between_named_classes(model: &Model, opts: &ReduceOptions) -> Result<HashSet<AnnotatedComponent<RcStr>>> {
    // superclass → subclass → the root axioms stating it.
    let mut assertions: HashMap<&str, HashMap<&str, Vec<&AnnotatedComponent<RcStr>>>> = HashMap::new();
    for (ac, sc) in root_subclass_axioms(model) {
        if let (CE::Class(a), CE::Class(b)) = (&sc.sub, &sc.sup) {
            assertions.entry(b.0.as_ref()).or_default().entry(a.0.as_ref()).or_default().push(ac);
        }
    }
    let taxonomy = Taxonomy::of(model, opts.reasoner)?;
    if !taxonomy.consistent() {
        status!("reduce: the ontology is inconsistent; no axiom is removed");
        return Ok(HashSet::new());
    }
    let mut kept: HashSet<&AnnotatedComponent<RcStr>> = HashSet::new();
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    let mut pending = vec![taxonomy.top_node()];
    while let Some(node) = pending.pop() {
        let mut key = node.clone();
        key.sort();
        if !seen.insert(key) {
            continue;
        }
        let below = taxonomy.sub_nodes(&node);
        for sup in &node {
            let Some(subs) = assertions.get(sup.as_str()) else { continue };
            for sub in below.iter().flatten() {
                if let Some(axioms) = subs.get(sub.as_str()) {
                    kept.extend(axioms.iter().copied());
                }
            }
        }
        pending.extend(below);
    }
    Ok(assertions
        .values()
        .flat_map(|subs| subs.values())
        .flatten()
        .filter(|ac| !kept.contains(*ac) && !(opts.preserve_annotated && !ac.ann.is_empty()))
        .map(|ac| (*ac).clone())
        .collect())
}

/// `x` lies strictly above `y` in a taxonomy of nodes: a satisfiable class
/// lies above an unsatisfiable one, a class equivalent to `owl:Thing` above
/// every class that is not, and otherwise `x` subsumes `y` and not the
/// reverse.
fn node_above(
    y: &str,
    x: &str,
    unsatisfiable: impl Fn(&str) -> bool,
    top: impl Fn(&str) -> bool,
    subsumes: impl Fn(&str, &str) -> bool,
) -> bool {
    if y == x {
        return false;
    }
    let bottom = |c: &str| c == OWL_NOTHING || unsatisfiable(c);
    if bottom(y) {
        return !bottom(x);
    }
    if bottom(x) {
        return false;
    }
    let top = |c: &str| c == OWL_THING || top(c);
    if top(x) {
        return !top(y);
    }
    if top(y) {
        return false;
    }
    subsumes(y, x) && !subsumes(x, y)
}

/// A classification as the reduction over every class expression reads it:
/// whether the ontology is consistent, and which classes lie strictly above a
/// class.
enum Classification {
    /// The built-in EL engine, asked directly.
    El { reasoner: Reasoner, unsatisfiable: HashSet<String> },
    /// A DL taxonomy.
    Dl {
        consistent: bool,
        unsatisfiable: HashSet<String>,
        top: HashSet<String>,
        supers: HashMap<String, HashSet<String>>,
    },
    /// The whelk closure: a class lies above another where the closure records
    /// it and not the reverse, `owl:Thing` above every class not equivalent to
    /// it, and nothing above an unsatisfiable class but what the closure
    /// records.
    Whelk(Box<WhelkClassification>),
    /// The told hierarchy.
    Told(Told),
}

impl Classification {
    fn of(model: &Model, kind: ReasonerKind) -> Result<Classification> {
        Ok(match kind {
            ReasonerKind::Hermit | ReasonerKind::JFact => {
                let reasoner = kind.dl_reasoner(model);
                let consistent = reasoner.is_consistent();
                if !consistent {
                    return Ok(Classification::Dl {
                        consistent,
                        unsatisfiable: HashSet::new(),
                        top: HashSet::new(),
                        supers: HashMap::new(),
                    });
                }
                let mut supers: HashMap<String, HashSet<String>> = HashMap::new();
                for (a, b) in reasoner.all_subsumptions() {
                    supers.entry(a).or_default().insert(b);
                }
                Classification::Dl {
                    consistent,
                    unsatisfiable: reasoner.unsatisfiable().into_iter().collect(),
                    top: reasoner.top_equivalents().into_iter().collect(),
                    supers,
                }
            }
            ReasonerKind::Whelk => Classification::Whelk(Box::new(WhelkClassification::classify(model)?)),
            ReasonerKind::Structural => Classification::Told(Told::of(model)),
            ReasonerKind::Elk | ReasonerKind::Owlmake | ReasonerKind::Emr => {
                let reasoner = Reasoner::classify(model);
                let unsatisfiable = reasoner.unsatisfiable().into_iter().collect();
                Classification::El { reasoner, unsatisfiable }
            }
        })
    }

    fn consistent(&self) -> bool {
        match self {
            Classification::El { reasoner, .. } => reasoner.is_consistent(),
            Classification::Dl { consistent, .. } => *consistent,
            Classification::Whelk(w) => w.is_consistent(),
            Classification::Told(_) => true,
        }
    }

    /// Whether `x` lies strictly above `y`.
    fn above(&self, y: &str, x: &str) -> bool {
        match self {
            Classification::El { reasoner, unsatisfiable } => node_above(
                y,
                x,
                |c| unsatisfiable.contains(c),
                |c| reasoner.is_subsumed(OWL_THING, c),
                |a, b| reasoner.is_subsumed(a, b),
            ),
            Classification::Dl { unsatisfiable, top, supers, .. } => node_above(
                y,
                x,
                |c| unsatisfiable.contains(c),
                |c| top.contains(c),
                |a, b| supers.get(a).is_some_and(|s| s.contains(b)),
            ),
            Classification::Whelk(w) => {
                y != x && (x == OWL_THING || w.subsumes(y, x)) && !w.subsumes(x, y)
            }
            Classification::Told(t) => t.above(y, x),
        }
    }
}

/// A taxonomy of nodes of equivalent classes. The top node's classes are
/// `owl:Thing` and those equivalent to it; the bottom node's, `owl:Nothing`
/// and the unsatisfiable classes. The bottom node lies directly below every
/// node nothing else lies below.
struct Nodes {
    consistent: bool,
    top: Vec<String>,
    bottom: Vec<String>,
    /// The classes of each satisfiable class's node.
    node: HashMap<String, Vec<String>>,
    /// The satisfiable classes directly below each class.
    children: HashMap<String, Vec<String>>,
    /// The satisfiable classes directly below the top node.
    roots: Vec<String>,
}

impl Nodes {
    /// A taxonomy from what a reasoner gives: the satisfiable classes other
    /// than those of the top node, the top node's, the unsatisfiable ones,
    /// the pairs of equivalent classes, and each class's direct superclasses.
    fn new(
        classes: Vec<String>,
        top: Vec<String>,
        unsatisfiable: Vec<String>,
        equivalent: Vec<(String, String)>,
        direct: Vec<(String, String)>,
    ) -> Nodes {
        let top_set: HashSet<&str> = top.iter().map(String::as_str).collect();
        let mut node: HashMap<String, Vec<String>> =
            classes.iter().map(|c| (c.clone(), vec![c.clone()])).collect();
        for (a, b) in &equivalent {
            if top_set.contains(a.as_str()) || top_set.contains(b.as_str()) {
                continue;
            }
            for (x, y) in [(a, b), (b, a)] {
                if let Some(members) = node.get_mut(x) {
                    if !members.contains(y) {
                        members.push(y.clone());
                    }
                }
            }
        }
        for members in node.values_mut() {
            members.sort();
        }
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        let mut has_parent: HashSet<&str> = HashSet::new();
        for (sub, sup) in &direct {
            if top_set.contains(sup.as_str()) || node.get(sub).is_some_and(|m| m.contains(sup)) {
                continue;
            }
            children.entry(sup.clone()).or_default().push(sub.clone());
            has_parent.insert(sub.as_str());
        }
        let roots: Vec<String> =
            classes.iter().filter(|c| !has_parent.contains(c.as_str())).cloned().collect();
        let mut top_node = vec![OWL_THING.to_string()];
        top_node.extend(top.into_iter().filter(|c| c != OWL_THING));
        let mut bottom = vec![OWL_NOTHING.to_string()];
        bottom.extend(unsatisfiable.into_iter().filter(|c| c != OWL_NOTHING));
        Nodes { consistent: true, top: top_node, bottom, node, children, roots }
    }

    fn inconsistent() -> Nodes {
        Nodes {
            consistent: false,
            top: vec![OWL_THING.to_string()],
            bottom: vec![OWL_NOTHING.to_string()],
            node: HashMap::new(),
            children: HashMap::new(),
            roots: Vec::new(),
        }
    }

    fn sub_nodes(&self, of: &[String]) -> Vec<Vec<String>> {
        if of.iter().any(|c| c == OWL_NOTHING) {
            return Vec::new();
        }
        let below: Vec<&String> = if of.iter().any(|c| c == OWL_THING) {
            self.roots.iter().collect()
        } else {
            of.iter().flat_map(|c| self.children.get(c).into_iter().flatten()).collect()
        };
        let mut out: Vec<Vec<String>> = Vec::new();
        for c in below {
            let members = self.node.get(c).cloned().unwrap_or_else(|| vec![c.clone()]);
            if !out.contains(&members) {
                out.push(members);
            }
        }
        if out.is_empty() {
            out.push(self.bottom.clone());
        }
        out
    }
}

/// A classification as the reduction between named classes walks it: the top
/// node, and the nodes directly below a node.
enum Taxonomy {
    Nodes(Nodes),
    /// The walk from the top reaches the bottom node and nothing else: below
    /// every node lies the bottom node alone.
    BottomOnly(Nodes),
    /// The whelk closure, whose nodes are single classes.
    Whelk(Box<WhelkClassification>),
    Told(Told),
}

impl Taxonomy {
    fn of(model: &Model, kind: ReasonerKind) -> Result<Taxonomy> {
        let classes = || -> Vec<String> {
            let mut out: Vec<String> = model
                .ont
                .iter()
                .flat_map(|ac| crate::sig::typed_signature(&ac.component))
                .filter(|(k, iri)| *k == crate::sig::kind::CLASS && iri != OWL_THING && iri != OWL_NOTHING)
                .map(|(_, iri)| iri)
                .collect();
            out.sort();
            out.dedup();
            out
        };
        Ok(match kind {
            ReasonerKind::Hermit | ReasonerKind::JFact => {
                let reasoner = kind.dl_reasoner(model);
                let nodes = if reasoner.is_consistent() {
                    let unsatisfiable = reasoner.unsatisfiable();
                    let top = reasoner.top_equivalents();
                    let classes = classes()
                        .into_iter()
                        .filter(|c| !unsatisfiable.contains(c) && !top.contains(c))
                        .collect();
                    Nodes::new(
                        classes,
                        top,
                        unsatisfiable,
                        reasoner.equivalent_class_pairs(),
                        reasoner.direct_subsumptions(),
                    )
                } else {
                    Nodes::inconsistent()
                };
                if kind == ReasonerKind::JFact {
                    Taxonomy::BottomOnly(nodes)
                } else {
                    Taxonomy::Nodes(nodes)
                }
            }
            ReasonerKind::Whelk => Taxonomy::Whelk(Box::new(WhelkClassification::classify(model)?)),
            ReasonerKind::Structural => Taxonomy::Told(Told::of(model)),
            ReasonerKind::Elk | ReasonerKind::Owlmake | ReasonerKind::Emr => {
                let reasoner = Reasoner::classify(model);
                if !reasoner.is_consistent() {
                    return Ok(Taxonomy::Nodes(Nodes::inconsistent()));
                }
                let (top, classes): (Vec<String>, Vec<String>) = reasoner
                    .satisfiable_named_classes()
                    .into_iter()
                    .partition(|c| reasoner.is_subsumed(OWL_THING, c));
                Taxonomy::Nodes(Nodes::new(
                    classes,
                    top,
                    reasoner.unsatisfiable(),
                    reasoner.equivalent_class_pairs(),
                    reasoner.direct_subsumptions(),
                ))
            }
        })
    }

    fn consistent(&self) -> bool {
        match self {
            Taxonomy::Nodes(n) | Taxonomy::BottomOnly(n) => n.consistent,
            Taxonomy::Whelk(w) => w.is_consistent(),
            Taxonomy::Told(_) => true,
        }
    }

    fn top_node(&self) -> Vec<String> {
        match self {
            Taxonomy::Nodes(n) | Taxonomy::BottomOnly(n) => n.top.clone(),
            Taxonomy::Whelk(w) => w.top_node(),
            Taxonomy::Told(t) => t.top.to_vec(),
        }
    }

    /// The nodes directly below `node`, each as its classes.
    fn sub_nodes(&self, node: &[String]) -> Vec<Vec<String>> {
        match self {
            Taxonomy::Nodes(n) => n.sub_nodes(node),
            Taxonomy::BottomOnly(n) => vec![n.bottom.clone()],
            Taxonomy::Whelk(w) => {
                w.direct_subclasses(representative(node)).into_iter().map(|c| vec![c]).collect()
            }
            Taxonomy::Told(t) => t.sub_nodes(node),
        }
    }
}

/// The class a node of the whelk closure is asked for its subclasses by: the
/// first of its classes in a hash set that starts with four buckets and
/// doubles past three quarters full, holding them in the order the node lists
/// them. A node of the closure below the top is a single class.
fn representative(node: &[String]) -> &str {
    let mut buckets = 4usize;
    while node.len() > buckets * 3 / 4 {
        buckets *= 2;
    }
    let bucket = |c: &str| {
        let h = crate::owlapi_hash::class_hash(c) as u32;
        ((h ^ (h >> 16)) as usize) & (buckets - 1)
    };
    node.iter()
        .enumerate()
        .min_by_key(|(i, c)| (bucket(c), *i))
        .map(|(_, c)| c.as_str())
        .unwrap_or(OWL_THING)
}

/// Exact reduction: an axiom is removed iff the ontology *minus that axiom*
/// still entails it. Named `A ⊑ B` uses transitive reduction (which is
/// exactly the entailment test for named subsumption). Each existential
/// `C ⊑ ∃R.F` (named F) is checked against the ⊥-locality module of `O − α` over
/// its signature — a small ontology that preserves exactly the Σ-entailments of
/// `O − α`, so the entailment check is exact without re-classifying all of `O`.
///
/// This is `O(candidates × module-extraction)`; on very large ontologies it is
/// slower than [`reduce`], which is why it is opt-in (`--exact`).
pub fn reduce_exact(
    model: &Model,
    preserve_annotated: bool,
    named_classes_only: bool,
    include_subproperties: bool,
) -> Model {
    let reasoner = Reasoner::classify(model);
    let direct: HashSet<(String, String)> = reasoner.direct_subsumptions().into_iter().collect();
    let no_filter: HashSet<String> = HashSet::new();

    // Is `C ⊑ ∃R.F` entailed by the ontology with `target` removed?
    let entailed_without =
        |c: &str, r: &str, f: &str, target: &horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>| -> bool {
            let mut seed: HashSet<String> = HashSet::new();
            seed.insert(c.to_string());
            seed.insert(r.to_string());
            seed.insert(f.to_string());
            let module = crate::extract::extract(model, &seed, crate::extract::Method::Bot);
            let mut m2 = Model::from_parts(
                horned_owl::ontology::set::SetOntology::new(),
                crate::model::clone_prefixes(&module.prefixes),
            );
            for ac in module.ont.iter() {
                // With `--include-subproperties false` a sub-role R'⊑R must not
                // dominate, so drop SubObjectPropertyOf from the entailment
                // module (property chains are not subproperties and are kept).
                if !include_subproperties
                    && matches!(ac.component, Component::SubObjectPropertyOf(_))
                {
                    continue;
                }
                if ac != target {
                    m2.ont.insert(ac.clone());
                }
            }
            // Use the *redundant* closure (`materialize_all`), not the
            // most-specific-only `materialize`: `C ⊑ ∃R.F` may be entailed only
            // via `C ⊑ ∃R.F'` with `F' ⊏ F`, in which case the most-specific set
            // omits `(C,R,F)` and the redundant axiom would wrongly be kept.
            Reasoner::classify(&m2)
                .materialize_all(&no_filter)
                .into_iter()
                .any(|(cc, rr, ff)| cc == c && rr == r && ff == f)
        };

    let mut out = SetClone::new(model);
    out.retain(|ac| {
        if preserve_annotated && !ac.ann.is_empty() {
            return true;
        }
        match &ac.component {
            Component::SubClassOf(ax) => match (&ax.sub, &ax.sup) {
                (CE::Class(a), CE::Class(b)) => {
                    direct.contains(&(a.0.to_string(), b.0.to_string())) || a.0 == b.0
                }
                _ if named_classes_only => true,
                (CE::Class(c), CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(r), bce }) => {
                    match bce.as_ref() {
                        CE::Class(f) => {
                            !entailed_without(c.0.as_ref(), r.0.as_ref(), f.0.as_ref(), ac)
                        }
                        _ => true,
                    }
                }
                _ => true,
            },
            _ => true,
        }
    });
    let mut result = out.into_model();
    result.carry_meta_from(model);
    result
}

/// Small helper to clone a model and filter its components.
struct SetClone {
    components: Vec<horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>>,
    prefixes: horned_owl::curie::PrefixMapping,
}

impl SetClone {
    fn new(model: &Model) -> Self {
        SetClone {
            components: model.ont.iter().cloned().collect(),
            prefixes: crate::model::clone_prefixes(&model.prefixes),
        }
    }
    fn retain<F: Fn(&horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>) -> bool>(
        &mut self,
        f: F,
    ) {
        self.components.retain(|ac| f(ac));
    }
    fn into_model(self) -> Model {
        use horned_owl::ontology::set::SetOntology;
        let mut ont: SetOntology<_> = SetOntology::new();
        use horned_owl::model::MutableOntology;
        for ac in self.components {
            ont.insert(ac);
        }
        Model::from_parts(ont, self.prefixes)
    }
}
