//! The graph-surgery operations UBERON's release path needs, so `owlmake make`
//! can build the composite products end to end:
//!
//! - [`extract_ontology_subset`] — `--extract-ontology-subset [--fill-gaps] --subset NAME`
//!   (UBERON's `common-anatomy.owl`).
//! - [`extract_mingraph`] — `--extract-mingraph` (composite `-basic`).
//! - [`remove_axiom_annotations`] — `--remove-axiom-annotations`.
//! - [`make_subset_by_properties`] — `--make-subset-by-properties -f PROPS`
//!   (composite `-basic`).
//!
//! Subset extraction here is a different operation from [`crate::cmd::subset`]:
//! it keeps the tagged slice ∪ its full graph-ancestor closure and then prunes
//! whatever the slice left dangling, where [`crate::cmd::subset`] keeps the
//! tagged classes and bridges the hierarchy across the ones it drops. The two
//! products differ, so the two stay separate code paths.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, AnnotationSubject, AnnotationValue, ClassExpression as CE, Component,
    ObjectPropertyExpression as OPE,
};

use horned_owl::model::MutableOntology as _;

use crate::model::{Model, Str};

/// `owltools --extract-ontology-subset [--fill-gaps] --subset NAME`.
#[derive(ClapArgs)]
pub struct SubsetArgs {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub format: Option<String>,
    /// The named `oboInOwl:inSubset` slice (e.g. `common_anatomy`).
    #[arg(short, long)]
    pub subset: String,
    /// Extend the subset to its full graph-ancestor closure before slicing.
    #[arg(long = "fill-gaps", num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    pub fill_gaps: bool,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run_subset(args: SubsetArgs) -> Result<()> {
    step_subset(None, &args)?;
    Ok(())
}

/// Entry point for the `owltools` argv token — the name UBERON's composite
/// `-basic` and `common-anatomy` recipes invoke these operations under. A recipe
/// line such as
///   `owltools --use-catalog composite-metazoan.owl --extract-mingraph
///    --remove-axiom-annotations --make-subset-by-properties -f PROPS
///    -o -f obo --no-check out.tmp`
/// arrives here whole through the `owltools` shim the build writes onto the
/// recipe's PATH, which re-executes this binary. Such a line runs through the
/// shell path because it also greps and annotates. Input is the first
/// positional; the ops run in order; the output section begins at `-o`
/// (`-f FMT` then the file).
pub fn owltools_main(args: &[String]) -> i32 {
    match owltools_run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("owltools: {e:#}");
            1
        }
    }
}

enum OwltoolsAct {
    Mingraph,
    RemoveAxiomAnnotations,
    MakeSubsetByProps(Vec<String>),
    OntologySubset { subset: String, fill_gaps: bool },
    /// `--merge-support-ontologies`: fold every ontology after the first into it.
    MergeSupport,
    /// `--run-reasoner [-r NAME] [-u] [-m FILE] [-x]`.
    RunReasoner { list_unsat: bool, remove_unsat: bool, module: Option<String> },
    /// `--merge-equivalence-sets [-s PREFIX SCORE]… [-P PREFIX]…`.
    MergeEquivalenceSets { scores: Vec<(String, f64)>, no_merge: Vec<String> },
    /// `--remove-dangling`.
    RemoveDangling,
}

fn owltools_run(args: &[String]) -> Result<i32> {
    use anyhow::Context;
    // Any number of ontologies may precede the first operation: the FIRST is the
    // source ontology and the rest are SUPPORT ontologies, which
    // `--merge-support-ontologies` then folds in. MONDO's `debug.owl` passes
    // three (`mondo-edit.obo disjoint_sibs.owl imports/equivalencies.owl`).
    let mut inputs: Vec<String> = Vec::new();
    let mut output: Option<String> = None;
    let mut out_format: Option<String> = None;
    let mut acts: Vec<OwltoolsAct> = Vec::new();
    let mut in_output = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            // Logging level only — `CommandRunner` sets a log4j level and nothing
            // about the output changes. Failing on them (as an unknown `--` op)
            // stopped MONDO's `debug.owl` and `test_nomerge` before their first
            // real operation.
            "--use-catalog" | "--no-check" | "--silence-elk" | "--no-logging" | "--log-error"
            | "--log-warning" | "--log-info" | "--log-debug" | "--no-debug"
            | "--monitor-memory" => {}
            "--merge-support-ontologies" => {
                acts.push(OwltoolsAct::MergeSupport);
                // `-l|--labels` selects the label policy; owlmake keeps every
                // label either way, which is `ALLOW_DUPLICATES`, the default.
                if args.get(i + 1).map(|a| a == "-l" || a == "--labels").unwrap_or(false) {
                    i += 1;
                }
            }
            "--remove-dangling" => acts.push(OwltoolsAct::RemoveDangling),
            // `--reasoner NAME` only selects which reasoner the later operations
            // use (`CommandRunner` line 277: `reasonerName = opts.nextOpt()`).
            // owlmake has one, so the name is consumed and the choice recorded
            // nowhere — `--run-reasoner -r` is treated the same way.
            "--reasoner" | "--init-reasoner" => {
                if args.get(i + 1).map(|a| !a.starts_with("--")).unwrap_or(false) {
                    i += 1;
                }
            }
            "--run-reasoner" => {
                let mut list_unsat = false;
                let mut remove_unsat = false;
                let mut module = None;
                let mut j = i + 1;
                while j < args.len() {
                    match args[j].as_str() {
                        "-r" | "--reasoner" => j += 1, // owlmake always uses its EL engine
                        "-u" | "--list-unsatisfiable" => list_unsat = true,
                        "-x" | "--remove-unsatisfiable" => {
                            remove_unsat = true;
                            list_unsat = true;
                        }
                        "-m" | "--unsatisfiable-module" => {
                            module = args.get(j + 1).cloned();
                            j += 1;
                        }
                        "--assert-implied" | "--indirect" | "-e" | "--show-explanation"
                        | "--trace-module-axioms" => {}
                        _ => break,
                    }
                    j += 1;
                }
                acts.push(OwltoolsAct::RunReasoner { list_unsat, remove_unsat, module });
                i = j;
                continue;
            }
            "--merge-equivalence-sets" => {
                let mut scores: Vec<(String, f64)> = Vec::new();
                let mut no_merge: Vec<String> = Vec::new();
                let mut j = i + 1;
                while j < args.len() {
                    match args[j].as_str() {
                        // `-s PREFIX SCORE` picks the clique REPRESENTATIVE;
                        // `-l`/`-c`/`-d` pick which label/comment/definition
                        // survives, which owlmake does not need to model because
                        // the representative keeps its own.
                        "-s" | "-l" | "-c" | "-d" => {
                            if args[j] == "-s" {
                                if let (Some(p), Some(v)) = (args.get(j + 1), args.get(j + 2)) {
                                    if let Ok(v) = v.parse::<f64>() {
                                        scores.push((p.clone(), v));
                                    }
                                }
                            }
                            j += 2;
                        }
                        "-P" | "--preserve" => {
                            if let Some(p) = args.get(j + 1) {
                                no_merge.push(p.clone());
                            }
                            j += 1;
                        }
                        "-x" => {}
                        _ => break,
                    }
                    j += 1;
                }
                acts.push(OwltoolsAct::MergeEquivalenceSets { scores, no_merge });
                i = j;
                continue;
            }
            "--extract-mingraph" => acts.push(OwltoolsAct::Mingraph),
            "--remove-axiom-annotations" => acts.push(OwltoolsAct::RemoveAxiomAnnotations),
            "--make-subset-by-properties" => {
                let mut props = Vec::new();
                let mut j = i + 1;
                while j < args.len() {
                    let t = &args[j];
                    if t == "//" {
                        j += 1;
                        break;
                    }
                    if t == "-f" || t == "--force" || t == "-n" || t == "--no-remove-dangling" {
                        j += 1;
                        continue;
                    }
                    if t == "-o" || t == "--out" || t.starts_with("--") {
                        break;
                    }
                    props.push(t.clone());
                    j += 1;
                }
                acts.push(OwltoolsAct::MakeSubsetByProps(props));
                i = j;
                continue;
            }
            "--extract-ontology-subset" => {
                let mut subset = String::new();
                let mut fill_gaps = false;
                let mut j = i + 1;
                while j < args.len() {
                    match args[j].as_str() {
                        "--fill-gaps" => fill_gaps = true,
                        "--minimal" => fill_gaps = false,
                        "-s" | "--subset" => {
                            if j + 1 < args.len() {
                                subset = args[j + 1].clone();
                                j += 1;
                            }
                        }
                        "-u" | "--iri" | "--uri" | "-i" | "--input-file" => {
                            j += 1;
                        }
                        t if t == "-o" || t.starts_with("--extract") || t.starts_with("--make") => {
                            break
                        }
                        _ => {}
                    }
                    j += 1;
                }
                acts.push(OwltoolsAct::OntologySubset { subset, fill_gaps });
                i = j;
                continue;
            }
            "-o" | "--out" => in_output = true,
            "-f" | "--format" if in_output => {
                if i + 1 < args.len() {
                    out_format = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            // An operation owlmake does not implement must FAIL. Silently ignoring
            // it and exiting 0 turns "owlmake cannot do this" into "the check
            // passed": MONDO's test target reaches this parser for `debug.owl`,
            // `debug_inference_check.owl` and `test_nomerge`, and a no-op would
            // report each of those checks as having succeeded.
            s if s.starts_with("--") => {
                anyhow::bail!(
                    "owltools operation `{s}` is not implemented by owlmake\n                     (owlmake reimplements the owltools operations ODK builds use;                      an unimplemented one must fail rather than silently do nothing)"
                );
            }
            s if s.starts_with('-') => {} // other single-dash options of this grammar
            s => {
                if in_output {
                    output = Some(s.to_string());
                } else {
                    inputs.push(s.to_string());
                }
            }
        }
        i += 1;
    }

    let (first, rest) = inputs.split_first().context("owltools: no input ontology given")?;
    let mut model = crate::io::load(std::path::Path::new(first))
        .with_context(|| format!("owltools: loading {first}"))?;
    let mut support: Vec<Model> = Vec::new();
    for p in rest {
        support.push(
            crate::io::load(std::path::Path::new(p))
                .with_context(|| format!("owltools: loading {p}"))?,
        );
    }
    for act in acts {
        model = match act {
            OwltoolsAct::Mingraph => extract_mingraph(model),
            OwltoolsAct::RemoveAxiomAnnotations => remove_axiom_annotations(model),
            OwltoolsAct::MakeSubsetByProps(props) => make_subset_by_properties(model, &props),
            OwltoolsAct::OntologySubset { subset, fill_gaps } => {
                extract_ontology_subset(model, &subset, fill_gaps)
            }
            OwltoolsAct::MergeSupport => {
                let opts = crate::cmd::merge::MergeOptions::default();
                for s in &support {
                    crate::cmd::merge::merge_into(&mut model, s, &opts);
                }
                support.clear();
                model
            }
            OwltoolsAct::RemoveDangling => remove_dangling(model),
            OwltoolsAct::RunReasoner { list_unsat, remove_unsat, module } => {
                match run_reasoner(model, list_unsat, remove_unsat, module.as_deref())? {
                    Some(m) => m,
                    None => return Ok(1),
                }
            }
            OwltoolsAct::MergeEquivalenceSets { scores, no_merge } => {
                match merge_equivalence_sets(model, &scores, &no_merge)? {
                    Some(m) => m,
                    None => return Ok(1),
                }
            }
        };
    }
    if let Some(out) = output {
        // `-o [-f FORMAT] FILE`: the format is `-f`, and RDF/XML when there is no
        // `-f`. The file NAME does not decide it — MONDO's `test_nomerge` writes
        // to a path with no extension at all, and reading the name would leave
        // that check unable to run.
        let fmt = match out_format.as_deref() {
            Some(f) => crate::io::Format::from_name(f)?,
            None => crate::io::Format::RdfXml,
        };
        crate::io::save_as(&mut model, std::path::Path::new(&out), fmt)?;
    }
    Ok(0)
}

/// `owltools --extract-mingraph` / `--remove-axiom-annotations` (no operands).
#[derive(ClapArgs)]
pub struct SimpleArgs {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub format: Option<String>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// `owltools --make-subset-by-properties [-f] PROPS…`.
#[derive(ClapArgs)]
pub struct MakeSubsetByPropsArgs {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub format: Option<String>,
    /// Force-remove dangling axioms; always on in owlmake.
    #[arg(short = 'f', long = "force", num_args = 0, default_value_t = false)]
    pub force: bool,
    /// The object-property keep-list (CURIE / IRI / label / shorthand).
    #[arg(value_name = "PROPS", trailing_var_arg = true)]
    pub props: Vec<String>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn step_mingraph(piped: Option<Model>, args: &SimpleArgs) -> Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut model = extract_mingraph(model);
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

pub fn run_mingraph(args: SimpleArgs) -> Result<()> {
    step_mingraph(None, &args)?;
    Ok(())
}

pub fn step_remove_axiom_annotations(piped: Option<Model>, args: &SimpleArgs) -> Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut model = remove_axiom_annotations(model);
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

pub fn run_remove_axiom_annotations(args: SimpleArgs) -> Result<()> {
    step_remove_axiom_annotations(None, &args)?;
    Ok(())
}

pub fn step_make_subset_by_properties(
    piped: Option<Model>,
    args: &MakeSubsetByPropsArgs,
) -> Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut model = make_subset_by_properties(model, &args.props);
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

pub fn run_make_subset_by_properties(args: MakeSubsetByPropsArgs) -> Result<()> {
    step_make_subset_by_properties(None, &args)?;
    Ok(())
}

pub fn step_subset(piped: Option<Model>, args: &SubsetArgs) -> Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut model = extract_ontology_subset(model, &args.subset, args.fill_gaps);
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

const IN_SUBSET: &str = "http://www.geneontology.org/formats/oboInOwl#inSubset";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// The local name of an IRI (after the last `#` or `/`).
fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

/// Classes carrying `oboInOwl:inSubset` whose value names `subset_name`
/// (by full IRI or by local name).
fn inset_seed(model: &Model, subset_name: &str) -> HashSet<String> {
    let mut seed = HashSet::new();
    for ac in model.ont.iter() {
        let Component::AnnotationAssertion(aa) = &ac.component else { continue };
        if aa.ann.ap.0.as_ref() != IN_SUBSET {
            continue;
        }
        let AnnotationSubject::IRI(subj) = &aa.subject else { continue };
        let matches = match &aa.ann.av {
            AnnotationValue::IRI(v) => {
                let v = v.as_ref();
                v == subset_name || local_name(v) == subset_name
            }
            AnnotationValue::Literal(l) => {
                let v = l.literal();
                v == subset_name || local_name(v) == subset_name
            }
            _ => false,
        };
        if matches {
            seed.insert(subj.as_ref().to_string());
        }
    }
    seed
}

/// The upward graph edges `--extract-ontology-subset --fill-gaps` walks: each
/// named class maps to the named classes its superclass expressions and its
/// equivalence partners point at. Extending the seed over these edges is what
/// fills the gaps — a kept class keeps its ancestors instead of leaving a
/// superclass reference behind for the dangling prune to delete.
fn outgoing_named(model: &Model) -> HashMap<String, HashSet<String>> {
    let mut edges: HashMap<String, HashSet<String>> = HashMap::new();
    // Named outgoing targets of a superclass expression: a named superclass, an
    // intersection's operands, and a restriction's named filler (`p some D` → D).
    // Only the *forward* (subclass → superclass/filler) direction is walked —
    // never the reverse — so the closure stays within the seed's genuine
    // ancestors and does not descend into specific classes (e.g. reaching
    // `human`/`nucleus` from an abstract seed).
    fn targets_of(ce: &CE<Str>, out: &mut HashSet<String>) {
        match ce {
            CE::Class(c) => {
                out.insert(c.0.to_string());
            }
            CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => {
                targets_of(bce, out)
            }
            CE::ObjectIntersectionOf(parts) => {
                for p in parts {
                    targets_of(p, out);
                }
            }
            _ => {}
        }
    }
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    let e = edges.entry(sub.0.to_string()).or_default();
                    targets_of(&sc.sup, e);
                }
            }
            Component::EquivalentClasses(eq) => {
                // Each named member's is_a parents are the named classes / genus
                // of the other members (forward direction only).
                for (i, m) in eq.0.iter().enumerate() {
                    let CE::Class(x) = m else { continue };
                    let e = edges.entry(x.0.to_string()).or_default();
                    for (j, other) in eq.0.iter().enumerate() {
                        if i != j {
                            targets_of(other, e);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    edges
}

/// Reflexive-transitive ancestor closure of `seed` over [`outgoing_named`].
fn ancestor_closure(seed: &HashSet<String>, edges: &HashMap<String, HashSet<String>>) -> HashSet<String> {
    let mut acc: HashSet<String> = seed.clone();
    let mut frontier: Vec<String> = seed.iter().cloned().collect();
    while let Some(c) = frontier.pop() {
        if let Some(ts) = edges.get(&c) {
            for t in ts {
                if acc.insert(t.clone()) {
                    frontier.push(t.clone());
                }
            }
        }
    }
    acc
}

/// Collect the class / named-individual / object-property IRIs referenced
/// anywhere in `comps` (the entity kinds the dangling check considers).
fn danglable_entities(comps: &[AnnotatedComponent<Str>]) -> HashSet<String> {
    use horned_owl::model::{Class, NamedIndividual, ObjectProperty};
    use horned_owl::visitor::immutable::{Visit, Walk};

    #[derive(Default)]
    struct Collect {
        iris: Vec<String>,
    }
    impl Visit<Str> for Collect {
        fn visit_class(&mut self, c: &Class<Str>) {
            self.iris.push(c.0.as_ref().to_string());
        }
        fn visit_object_property(&mut self, p: &ObjectProperty<Str>) {
            self.iris.push(p.0.as_ref().to_string());
        }
        fn visit_named_individual(&mut self, i: &NamedIndividual<Str>) {
            self.iris.push(i.0.as_ref().to_string());
        }
    }
    let mut walk = Walk::new(Collect::default());
    for ac in comps {
        walk.component(&ac.component);
    }
    walk.into_visit().iris.into_iter().collect()
}

/// The set of entity IRIs that have at least one `AnnotationAssertion`. An
/// entity referenced by an axiom but with no annotation assertion is *dangling*:
/// nothing among the kept axioms says what it is.
fn annotated_subjects(comps: &[AnnotatedComponent<Str>]) -> HashSet<String> {
    let mut out = HashSet::new();
    for ac in comps {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let AnnotationSubject::IRI(i) = &aa.subject {
                out.insert(i.as_ref().to_string());
            }
        }
    }
    out
}

/// Collect the named classes and object properties mentioned by an axiom's class
/// expressions. Only `SubClassOf`, `EquivalentClasses` and `DisjointClasses`
/// contribute; every other component yields nothing.
fn signature_entities(comp: &Component<Str>, out: &mut HashSet<String>) {
    use horned_owl::model::Component as C;
    // One walk over the expression tree picks up both kinds: a class name, and the
    // property of an existential or universal restriction. Expression forms that
    // carry neither — nominals, `has_value`, cardinality — add nothing.
    fn ce_entities(ce: &CE<Str>, out: &mut HashSet<String>) {
        match ce {
            CE::Class(c) => {
                out.insert(c.0.to_string());
            }
            CE::ObjectSomeValuesFrom { ope, bce } => {
                if let OPE::ObjectProperty(p) = ope {
                    out.insert(p.0.to_string());
                }
                ce_entities(bce, out);
            }
            CE::ObjectAllValuesFrom { ope, bce } => {
                if let OPE::ObjectProperty(p) = ope {
                    out.insert(p.0.to_string());
                }
                ce_entities(bce, out);
            }
            CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => {
                for p in v {
                    ce_entities(p, out);
                }
            }
            CE::ObjectComplementOf(b) => ce_entities(b, out),
            _ => {}
        }
    }
    match comp {
        C::SubClassOf(sc) => {
            ce_entities(&sc.sub, out);
            ce_entities(&sc.sup, out);
        }
        C::EquivalentClasses(eq) => {
            for m in &eq.0 {
                ce_entities(m, out);
            }
        }
        C::DisjointClasses(d) => {
            for m in &d.0 {
                ce_entities(m, out);
            }
        }
        _ => {}
    }
}

/// `--extract-ontology-subset [--fill-gaps] --subset NAME`: the ontology reduced
/// to the classes tagged with the named subset, with everything the reduction
/// leaves dangling pruned. With `fill_gaps` the tagged seed is first extended to
/// its graph-ancestor closure, so the slice keeps a connected hierarchy — the
/// path UBERON's `common-anatomy.owl` takes, and the only one a release recipe
/// exercises.
///
/// Without it (`--minimal`) the slice is the tagged classes alone, and gaps are
/// NOT spanned: where the graph relates two tagged classes only through untagged
/// intermediates, that relation is dropped rather than bridged by a direct edge
/// between the two. That mode is a plain slice, untested territory, and a
/// divergence to settle before a recipe relies on it.
pub fn extract_ontology_subset(model: Model, subset_name: &str, fill_gaps: bool) -> Model {
    let mut subset = inset_seed(&model, subset_name);
    if fill_gaps {
        let edges = outgoing_named(&model);
        subset = ancestor_closure(&subset, &edges);
    }

    // All classes NOT in the (extended) subset — used to drop their annotation
    // assertions. Non-class subjects (properties) are never excluded.
    let all_classes = crate::cmd::select::entities(&model).classes;
    let exclude: HashSet<String> = all_classes.difference(&subset).cloned().collect();

    // First pass: select the axioms the slice keeps. The result is a *fresh*
    // ontology built from the retained axioms, so the source ontology's own
    // header annotations are NOT carried over — the recipe's later `annotate`
    // step re-adds only the ontology/version IRI.
    let mut kept: Vec<AnnotatedComponent<Str>> = Vec::new();
    for ac in model.ont.iter() {
        if matches!(&ac.component, Component::OntologyAnnotation(_)) {
            continue;
        }
        let include = match &ac.component {
            Component::SubClassOf(sc) => matches!(&sc.sub, CE::Class(c) if subset.contains(c.0.as_ref())),
            Component::EquivalentClasses(eq) => eq.0.iter().any(|m| match m {
                CE::Class(c) => subset.contains(c.0.as_ref()),
                _ => {
                    // any named class in the expression's signature
                    let mut sig = HashSet::new();
                    signature_entities(&ac.component, &mut sig);
                    sig.iter().any(|i| subset.contains(i))
                }
            }),
            Component::AnnotationAssertion(aa) => match &aa.subject {
                AnnotationSubject::IRI(i) => !exclude.contains(i.as_ref()),
                _ => true,
            },
            // Declarations, property axioms, disjoints, ontology annotations …
            _ => true,
        };
        if include {
            kept.push(ac.clone());
        }
    }

    let kept = drop_dangling(kept);

    rebuild(model, kept)
}

/// Remove dangling axioms. An entity that is a class / named individual / object
/// property is *dangling* when it has no annotation assertion in the kept set;
/// every axiom referencing a dangling entity (in its logical signature) is
/// removed — including its declaration. Datatypes, data properties and
/// annotation properties are not subject to the check, and annotation *values*
/// are not part of the logical signature, so a surviving axiom may still carry a
/// dangling annotation-value IRI.
fn drop_dangling(comps: Vec<AnnotatedComponent<Str>>) -> Vec<AnnotatedComponent<Str>> {
    // Danglable entities: classes / individuals / object properties referenced by
    // any kept axiom (declared OR merely mentioned) — the entity kinds the check
    // covers (datatypes / data / annotation properties excluded).
    let danglable = danglable_entities(&comps);

    // Entities that carry at least one annotation assertion are not dangling.
    let annotated = annotated_subjects(&comps);
    let dangling: HashSet<String> = danglable.difference(&annotated).cloned().collect();
    if dangling.is_empty() {
        return comps;
    }

    comps
        .into_iter()
        .filter(|ac| {
            let sig = crate::sig::signature(&ac.component);
            !sig.iter().any(|e| dangling.contains(e))
        })
        .collect()
}

const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";
const SHORTHAND: &str = "http://www.geneontology.org/formats/oboInOwl#shorthand";
const HAS_DBXREF: &str = "http://www.geneontology.org/formats/oboInOwl#hasDbXref";

/// Classes annotated `owl:deprecated "true"`, i.e. the obsolete ones.
fn obsolete_classes(model: &Model) -> HashSet<String> {
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        let Component::AnnotationAssertion(aa) = &ac.component else { continue };
        if aa.ann.ap.0.as_ref() != OWL_DEPRECATED {
            continue;
        }
        let AnnotationSubject::IRI(subj) = &aa.subject else { continue };
        if crate::model::asserts_deprecated(&aa.ann.av) {
            out.insert(subj.as_ref().to_string());
        }
    }
    out
}

/// `--extract-mingraph`: reduce the ontology to a minimal graph — class
/// hierarchy (`SubClassOf` / `EquivalentClasses`), class labels, and the
/// property ontology (declarations, label/shorthand/xref assertions, property
/// characteristics, sub-property and chain axioms) — dropping every axiom that
/// references an obsolete class. Axiom annotations are left alone here: the
/// composite `-basic` recipe strips all of them in the very next step, so
/// reducing them now would be work no output depends on.
pub fn extract_mingraph(model: Model) -> Model {
    let obsolete = obsolete_classes(&model);
    let refs_obsolete = |comp: &Component<Str>| {
        crate::sig::signature(comp).iter().any(|e| obsolete.contains(e))
    };

    // 1. Seed graph axioms: SubClassOf, EquivalentClasses, class rdfs:label AAAs,
    //    skipping any that reference an obsolete class.
    let mut graph_axioms: Vec<AnnotatedComponent<Str>> = Vec::new();
    let obj_props = crate::cmd::select::entities(&model).object_properties;
    for ac in model.ont.iter() {
        let keep = match &ac.component {
            Component::SubClassOf(_) | Component::EquivalentClasses(_) => !refs_obsolete(&ac.component),
            Component::AnnotationAssertion(aa) => {
                aa.ann.ap.0.as_ref() == RDFS_LABEL
                    && matches!(&aa.subject, AnnotationSubject::IRI(i) if !obsolete.contains(i.as_ref()))
                    // class labels only (property labels come from the property pass)
                    && matches!(&aa.subject, AnnotationSubject::IRI(i) if !obj_props.contains(i.as_ref()))
            }
            _ => false,
        };
        if keep {
            graph_axioms.push(ac.clone());
        }
    }

    // 2. Prune classes unreachable from the non-obsolete seed (upward over
    //    SubClassOf/EquivalentClasses signatures). Seed = all non-obsolete classes.
    let edges = mingraph_up_edges(&graph_axioms);
    let all_classes = crate::cmd::select::entities(&model).classes;
    let seed: HashSet<String> = all_classes.difference(&obsolete).cloned().collect();
    let reachable = ancestor_closure(&seed, &edges);
    graph_axioms.retain(|ac| {
        crate::sig::signature(&ac.component).iter().all(|e| {
            // keep unless it mentions a class that is neither reachable nor a property
            !all_classes.contains(e) || reachable.contains(e)
        })
    });

    // 3. Declarations for the classes surviving in the graph axioms.
    let mut out = graph_axioms;
    let present_classes = {
        let mut s = HashSet::new();
        for ac in &out {
            for e in crate::sig::signature(&ac.component) {
                if all_classes.contains(&e) {
                    s.insert(e);
                }
            }
        }
        s
    };
    for c in &present_classes {
        out.push(AnnotatedComponent {
            component: Component::DeclareClass(horned_owl::model::DeclareClass(
                model.build.class(c.as_str()),
            )),
            ann: Default::default(),
        });
    }

    // 4. Property ontology: for every declared object property, emit its
    //    declaration, label/shorthand/xref assertions, characteristics,
    //    sub-property and chain axioms. Keeping them all is safe because the
    //    recipe's later make-subset-by-properties step prunes the set down to
    //    the property list it names.
    for ac in model.ont.iter() {
        let keep = match &ac.component {
            Component::DeclareObjectProperty(_) => true,
            Component::SubObjectPropertyOf(_)
            | Component::TransitiveObjectProperty(_)
            | Component::ReflexiveObjectProperty(_)
            | Component::SymmetricObjectProperty(_)
            | Component::AsymmetricObjectProperty(_)
            | Component::IrreflexiveObjectProperty(_)
            | Component::FunctionalObjectProperty(_)
            | Component::InverseFunctionalObjectProperty(_) => true,
            Component::AnnotationAssertion(aa) => {
                let ap = aa.ann.ap.0.as_ref();
                (ap == RDFS_LABEL || ap == SHORTHAND || ap == HAS_DBXREF)
                    && matches!(&aa.subject, AnnotationSubject::IRI(i) if obj_props.contains(i.as_ref()))
            }
            _ => false,
        };
        if keep {
            out.push(ac.clone());
        }
    }

    rebuild(model, out)
}

/// Upward edges (subclass → named superclass / filler-class, member → member) for
/// the mingraph reachability prune — over the graph axioms only.
fn mingraph_up_edges(comps: &[AnnotatedComponent<Str>]) -> HashMap<String, HashSet<String>> {
    let mut edges: HashMap<String, HashSet<String>> = HashMap::new();
    for ac in comps {
        match &ac.component {
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    let e = edges.entry(sub.0.to_string()).or_default();
                    for x in class_sig(&sc.sup) {
                        e.insert(x);
                    }
                }
            }
            Component::EquivalentClasses(eq) => {
                let members: Vec<String> = eq.0.iter().flat_map(class_sig).collect();
                for (i, m) in eq.0.iter().enumerate() {
                    if let CE::Class(x) = m {
                        let e = edges.entry(x.0.to_string()).or_default();
                        for (j, _) in eq.0.iter().enumerate() {
                            if i != j {
                                for y in class_sig(&eq.0[j]) {
                                    e.insert(y);
                                }
                            }
                        }
                        let _ = &members;
                    }
                }
            }
            _ => {}
        }
    }
    edges
}

/// The named classes in a class expression's signature.
fn class_sig(ce: &CE<Str>) -> HashSet<String> {
    let mut out = HashSet::new();
    fn walk(ce: &CE<Str>, out: &mut HashSet<String>) {
        match ce {
            CE::Class(c) => {
                out.insert(c.0.to_string());
            }
            CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => {
                walk(bce, out)
            }
            CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => {
                for p in v {
                    walk(p, out);
                }
            }
            CE::ObjectComplementOf(b) => walk(b, out),
            _ => {}
        }
    }
    walk(ce, &mut out);
    out
}

/// `--remove-axiom-annotations`: replace every annotated axiom with its
/// annotation-free version. Ontology annotations and annotation-assertion
/// *values* are untouched; only the annotations attached to an axiom are cleared.
pub fn remove_axiom_annotations(model: Model) -> Model {
    let comps: Vec<AnnotatedComponent<Str>> = model
        .ont
        .iter()
        .map(|ac| {
            let mut ac = ac.clone();
            if !ac.ann.is_empty() {
                ac.ann = Default::default();
            }
            ac
        })
        .collect();
    rebuild(model, comps)
}

/// Object-property IRIs used in a component's signature (not annotation
/// properties or classes).
fn object_properties_in(comp: &Component<Str>) -> HashSet<String> {
    use horned_owl::model::ObjectProperty;
    use horned_owl::visitor::immutable::{Visit, Walk};
    #[derive(Default)]
    struct Collect {
        iris: Vec<String>,
    }
    impl Visit<Str> for Collect {
        fn visit_object_property(&mut self, p: &ObjectProperty<Str>) {
            self.iris.push(p.0.as_ref().to_string());
        }
    }
    let mut walk = Walk::new(Collect::default());
    walk.component(comp);
    walk.into_visit().iris.into_iter().collect()
}

/// Resolve one token of the object-property keep-list: a CURIE (`BFO:0000050`),
/// full IRI, `rdfs:label`, or `oboInOwl:shorthand`.
fn resolve_property(model: &Model, token: &str) -> Option<String> {
    // CURIE or IRI first.
    if token.starts_with("http://") || token.starts_with("https://") {
        return Some(token.to_string());
    }
    if token.contains(':') && !token.contains(' ') {
        let iri = crate::cmd::select::expand(model, token);
        if iri != token {
            return Some(iri);
        }
    }
    // Otherwise a label or shorthand of a declared object property.
    const SHORTHAND: &str = "http://www.geneontology.org/formats/oboInOwl#shorthand";
    let obj_props = crate::cmd::select::entities(model).object_properties;
    for ac in model.ont.iter() {
        let Component::AnnotationAssertion(aa) = &ac.component else { continue };
        let AnnotationSubject::IRI(subj) = &aa.subject else { continue };
        if !obj_props.contains(subj.as_ref()) {
            continue;
        }
        let ap = aa.ann.ap.0.as_ref();
        if ap == RDFS_LABEL || ap == SHORTHAND {
            if let AnnotationValue::Literal(l) = &aa.ann.av {
                if l.literal() == token {
                    return Some(subj.as_ref().to_string());
                }
            }
        }
    }
    None
}

/// Map each object property to its asserted direct super-properties
/// (`SubObjectPropertyOf(P, Q)`, named only).
fn super_property_map(model: &Model) -> HashMap<String, HashSet<String>> {
    use horned_owl::model::SubObjectPropertyExpression as SOPE;
    let mut m: HashMap<String, HashSet<String>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::SubObjectPropertyOf(sp) = &ac.component {
            if let (SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)), OPE::ObjectProperty(sup)) =
                (&sp.sub, &sp.sup)
            {
                m.entry(sub.0.to_string()).or_default().insert(sup.0.to_string());
            }
        }
    }
    m
}

/// The transitive super-properties of `p`, from the asserted
/// `SubObjectPropertyOf` edges alone — no reasoning, and `p` itself is not
/// included unless a cycle leads back to it.
fn super_properties(p: &str, supers: &HashMap<String, HashSet<String>>) -> HashSet<String> {
    let mut acc = HashSet::new();
    let mut stack = vec![p.to_string()];
    while let Some(x) = stack.pop() {
        if let Some(ss) = supers.get(&x) {
            for s in ss {
                if acc.insert(s.clone()) {
                    stack.push(s.clone());
                }
            }
        }
    }
    acc
}

/// `--make-subset-by-properties -f PROPS`, where `-f` forces the dangling removal
/// owlmake always performs. Remove every axiom that uses an object property
/// outside `props`; when such an axiom is `SubClassOf(X, p some Y)` with a named
/// `X`, first re-add the weakened `SubClassOf(X, p' some Y)` for each
/// super-property `p'` of `p` that is in `props`. Finally drop dangling axioms.
pub fn make_subset_by_properties(model: Model, prop_tokens: &[String]) -> Model {
    let filter: HashSet<String> =
        prop_tokens.iter().filter_map(|t| resolve_property(&model, t)).collect();
    let supers = super_property_map(&model);

    let mut kept: Vec<AnnotatedComponent<Str>> = Vec::new();
    let mut added: Vec<AnnotatedComponent<Str>> = Vec::new();
    for ac in model.ont.iter() {
        let mut used = object_properties_in(&ac.component);
        used.retain(|p| !filter.contains(p));
        if used.is_empty() {
            // No out-of-subset property → keep as-is.
            kept.push(ac.clone());
            continue;
        }
        // Axiom uses an excluded property → drop it, but try the super-property
        // rewrite for a named-subject existential SubClassOf.
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(sub), CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(p), bce }) =
                (&sc.sub, &sc.sup)
            {
                let sps = super_properties(p.0.as_ref(), &supers);
                for sp in sps.iter().filter(|sp| filter.contains(*sp)) {
                    let new_sup = CE::ObjectSomeValuesFrom {
                        ope: OPE::ObjectProperty(model.build.object_property(sp.as_str())),
                        bce: bce.clone(),
                    };
                    added.push(AnnotatedComponent {
                        component: Component::SubClassOf(horned_owl::model::SubClassOf {
                            sub: CE::Class(sub.clone()),
                            sup: new_sup,
                        }),
                        ann: Default::default(),
                    });
                }
            }
        }
        // else: dropped.
    }
    kept.extend(added);
    let kept = drop_dangling(kept);
    rebuild(model, kept)
}

/// Rebuild a [`Model`] from a component vector, preserving the prefix map and
/// serialization metadata of `base`.
fn rebuild(base: Model, comps: Vec<AnnotatedComponent<Str>>) -> Model {
    use horned_owl::model::MutableOntology;
    use horned_owl::ontology::set::SetOntology;
    let mut ont = SetOntology::new();
    for ac in comps {
        ont.insert(ac);
    }
    let mut m = Model::from_parts(ont, base.prefixes);
    m.banner_labels = base.banner_labels;
    m.import_order = base.import_order;
    m.idspaces = base.idspaces;
    m
}

// ───────────────────────── reasoner-driven operations ───────────────────────

/// `owltools --run-reasoner [-u] [-m FILE] [-x]`.
///
/// With `-u` it prints one `UNSAT: <class>` line per unsatisfiable class and then
/// `NUMBER_OF_UNSATISFIABLE_CLASSES: n`; with `-m FILE` it writes the ⊥-module
/// seeded by those classes to `FILE` — unconditionally, so a coherent ontology
/// leaves an essentially empty module behind, which is exactly what MONDO's
/// `debug.owl` is. An unsatisfiable class without `-x` fails the step; returning
/// `Ok(None)` here is that failure.
///
/// The property domain/range check above it is skipped under an EL reasoner,
/// which does not answer property-domain queries.
fn run_reasoner(
    mut model: Model,
    list_unsat: bool,
    remove_unsat: bool,
    module: Option<&str>,
) -> Result<Option<Model>> {
    if !list_unsat && module.is_none() {
        return Ok(Some(model));
    }
    let mut unsats = crate::reason::Reasoner::classify(&model).unsatisfiable();
    unsats.sort();
    unsats.retain(|c| !is_builtin_class(c));
    if list_unsat {
        for c in &unsats {
            println!("UNSAT: {c}");
        }
        println!("NUMBER_OF_UNSATISFIABLE_CLASSES: {}", unsats.len());
    }
    if let Some(path) = module {
        let seeds: HashSet<String> = unsats.iter().cloned().collect();
        let mut m = crate::extract::extract(&model, &seeds, crate::extract::Method::Bot);
        crate::io::save(&mut m, std::path::Path::new(path))?;
    }
    if !unsats.is_empty() {
        if !remove_unsat {
            eprintln!("Ontology has unsat classes - will not proceed");
            return Ok(None);
        }
        let drop: Vec<AnnotatedComponent<Str>> = model
            .ont
            .iter()
            .filter(|ac| crate::sig::signature(&ac.component).iter().any(|i| seeds_contains(&unsats, i)))
            .cloned()
            .collect();
        for ac in drop {
            model.ont.remove(&ac);
        }
    }
    Ok(Some(model))
}

fn seeds_contains(unsats: &[String], iri: &str) -> bool {
    unsats.iter().any(|u| u == iri)
}

/// `owl:Thing`/`owl:Nothing` — `OWLClass.isBuiltIn()`, which the `-u` loop skips.
fn is_builtin_class(iri: &str) -> bool {
    iri == "http://www.w3.org/2002/07/owl#Thing" || iri == "http://www.w3.org/2002/07/owl#Nothing"
}

/// `owltools --merge-equivalence-sets [-s PREFIX SCORE]… [-P PREFIX]…`.
///
/// Each equivalence clique of more than one class gets a LEADER — the member with
/// the highest prefix score — and every other member is rewritten to it. `-P
/// PREFIX` forbids that: when a non-leader AND the leader both carry the
/// preserved prefix, the class is bad, and any bad class fails the step as
/// incoherent. An unsatisfiable class fails it before any of this.
///
/// That is the whole of MONDO's `test_nomerge`: `-P MONDO -s MONDO 100` asks
/// whether inference ever makes two MONDO classes equivalent.
fn merge_equivalence_sets(
    mut model: Model,
    scores: &[(String, f64)],
    no_merge: &[String],
) -> Result<Option<Model>> {
    let classified = crate::reason::Reasoner::classify(&model);
    let unsats = classified.unsatisfiable();
    if !unsats.is_empty() {
        eprintln!("Ontology is incoherent: {} unsatisfiable class(es)", unsats.len());
        return Ok(None);
    }
    // Cliques, by union-find over the reasoner's equivalent-class pairs.
    let mut parent: HashMap<String, String> = HashMap::new();
    fn find(parent: &mut HashMap<String, String>, x: &str) -> String {
        let p = parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if p == x {
            return p;
        }
        let r = find(parent, &p);
        parent.insert(x.to_string(), r.clone());
        r
    }
    for (a, b) in classified.equivalent_class_pairs() {
        let (ra, rb) = (find(&mut parent, &a), find(&mut parent, &b));
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let members: Vec<String> = parent.keys().cloned().collect();
    let mut cliques: HashMap<String, Vec<String>> = HashMap::new();
    for m in members {
        let r = find(&mut parent, &m);
        cliques.entry(r).or_default().push(m);
    }

    let score = |iri: &str| -> Option<f64> {
        scores
            .iter()
            .find(|(p, _)| has_obo_prefix(iri, p))
            .map(|(_, s)| *s)
    };
    let mut bad: Vec<String> = Vec::new();
    let mut rename: HashMap<String, String> = HashMap::new();
    for (_, mut clique) in cliques {
        if clique.len() < 2 {
            continue;
        }
        clique.sort();
        // `getScore` order: the first member seen wins ties, and a later member
        // only displaces it with a STRICTLY higher score.
        let mut leader = clique[0].clone();
        let mut best = score(&leader);
        for c in &clique[1..] {
            let s = score(c);
            if best.is_none() || s.map(|s| Some(s) > best).unwrap_or(false) {
                leader = c.clone();
                best = s;
            }
        }
        for c in &clique {
            if *c == leader {
                continue;
            }
            rename.insert(c.clone(), leader.clone());
            for p in no_merge {
                if has_obo_prefix(c, p) && has_obo_prefix(&leader, p) {
                    eprintln!("Illegal merge into {p} :: {c} --> {leader}");
                    bad.push(c.clone());
                }
            }
        }
    }
    if !bad.is_empty() {
        eprintln!("The following classes would be merged: {bad:?}");
        return Ok(None);
    }
    if !rename.is_empty() {
        model = crate::cmd::rename::rename_model(model, &rename)?;
    }
    Ok(Some(model))
}

/// Does `iri` carry the OBO-style id prefix `prefix` (`…/obo/MONDO_…`, or a
/// `PREFIX:` CURIE-shaped tail)? `OWLGraphWrapper.getIdentifier` renders an OBO
/// IRI back to `PREFIX:LOCAL`, and `hasPrefix` tests that identifier.
fn has_obo_prefix(iri: &str, prefix: &str) -> bool {
    // `…/obo/MONDO_0000001` → `MONDO`; anything else keeps whatever precedes the
    // last `:` or `#`, which is what a non-OBO id renders as.
    let tail = iri.rsplit('/').next().unwrap_or(iri);
    match tail.split_once('_') {
        Some((p, _)) => p == prefix,
        None => tail.split_once(':').map(|(p, _)| p == prefix).unwrap_or(false),
    }
}

/// `owltools --remove-dangling`: drop every axiom that references an entity the
/// ontology does not declare.
fn remove_dangling(mut model: Model) -> Model {
    let declared: HashSet<String> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::DeclareClass(d) => Some(d.0 .0.to_string()),
            Component::DeclareObjectProperty(d) => Some(d.0 .0.to_string()),
            Component::DeclareAnnotationProperty(d) => Some(d.0 .0.to_string()),
            Component::DeclareDataProperty(d) => Some(d.0 .0.to_string()),
            Component::DeclareNamedIndividual(d) => Some(d.0 .0.to_string()),
            Component::DeclareDatatype(d) => Some(d.0 .0.to_string()),
            _ => None,
        })
        .collect();
    let drop: Vec<AnnotatedComponent<Str>> = model
        .ont
        .iter()
        .filter(|ac| {
            !matches!(
                &ac.component,
                Component::OntologyID(_)
                    | Component::DocIRI(_)
                    | Component::Import(_)
                    | Component::OntologyAnnotation(_)
            ) && crate::sig::signature(&ac.component)
                .iter()
                .any(|i| !declared.contains(i.as_str()) && !i.starts_with("http://www.w3.org/"))
        })
        .cloned()
        .collect();
    for ac in drop {
        model.ont.remove(&ac);
    }
    model
}
