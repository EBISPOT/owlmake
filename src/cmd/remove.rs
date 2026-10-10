//! `remove` — drop the axioms a selection selects, and re-link the hierarchy
//! across what went. How the terms and `--select` groups choose objects, and
//! how an axiom is judged against them, is [`crate::cmd::objects`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{ClassExpression as CE, Component, ObjectPropertyExpression as OPE};

use crate::cmd::objects::{self, Judge, Request, Selection};
use crate::cmd::{select, Switch};
use crate::io::entities::Kind;
use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Terms (IRIs or CURIEs) to remove. Repeatable.
    #[arg(short = 't', long)]
    pub term: Vec<String>,
    /// Files listing terms to remove, one per line.
    #[arg(short = 'T', long)]
    pub term_file: Vec<PathBuf>,
    /// Selector groups, applied in turn: each value's space-separated
    /// selectors (`self`, `classes`, `parents`, `anonymous`, `complement`,
    /// `PROP=VALUE`, `<IRI-pattern>`, …) select the union of what each maps the
    /// set to. `imports` removes the imports and `ontology` the ontology
    /// annotations; with `anonymous`, no anonymous superclass is re-linked.
    #[arg(short = 's', long)]
    pub select: Vec<String>,
    /// Axiom types to remove: `all` (the default), `logical`, `annotation`,
    /// `subclass`, `subproperty`, `equivalent`, `disjoint`, `type`, `abox`,
    /// `tbox`, `rbox`, `declaration`, `structural-tautologies`, one axiom type by
    /// name, or `internal`/`external` to the `--base-iri` namespaces.
    #[arg(short = 'a', long)]
    pub axioms: Vec<String>,
    /// If false, do not preserve hierarchical relationships (`<bool>`).
    #[arg(short = 'p', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::SwitchParser)]
    pub preserve_structure: Option<crate::cmd::Switch>,
    /// If true (the default), remove the axioms any of whose objects is
    /// selected; if false, those all of whose objects are (`<bool>`).
    #[arg(short = 'r', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::SwitchParser)]
    pub trim: Option<crate::cmd::Switch>,
    /// Terms to take out of the selection. Repeatable.
    #[arg(short = 'e', long = "exclude-term", value_name = "TERM")]
    pub exclude_term: Vec<String>,
    /// Files of terms to take out of the selection. Repeatable.
    #[arg(short = 'E', long = "exclude-terms", value_name = "FILE")]
    pub exclude_terms: Vec<PathBuf>,
    /// Terms to add to the selection. Repeatable.
    #[arg(short = 'n', long = "include-term", value_name = "TERM")]
    pub include_term: Vec<String>,
    /// Files of terms to add to the selection. Repeatable.
    #[arg(short = 'N', long = "include-terms", value_name = "FILE")]
    pub include_terms: Vec<PathBuf>,
    /// Drop the annotations of the ontology's own axioms: `PROP` drops a
    /// property's, `PROP=VALUE` those with that value and `PROP=~REGEX` those
    /// whose value the regex finds (`'` taken out of either); `all` drops
    /// every one. Repeatable.
    #[arg(short = 'd', long = "drop-axiom-annotations", value_name = "ARG")]
    pub drop_axiom_annotations: Vec<String>,
    /// If true, judge an axiom by the IRIs it names rather than by its
    /// objects, which include its anonymous class expressions and individuals
    /// (`<bool>`).
    #[arg(short = 'S', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::SwitchParser)]
    pub signature: Option<crate::cmd::Switch>,
    /// If true, a term naming entities of several kinds selects all of them;
    /// otherwise it selects none (`<bool>`).
    #[arg(long = "allow-punning", num_args = 1, default_missing_value = "true", value_parser = crate::cmd::SwitchParser)]
    pub allow_punning: Option<crate::cmd::Switch>,
    /// Base IRI(s): the namespaces `--axioms internal` and `--axioms external`
    /// judge an axiom's subjects by. Repeatable.
    #[arg(long = "base-iri", value_name = "IRI")]
    pub base_iri: Vec<String>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Options for the `remove`/`filter` term-set machinery, shared so callers can
/// extend behaviour without growing positional argument lists.
#[derive(Default, Clone)]
pub struct TermOptions {
    /// Whether an annotation's value counts among the objects of the assertion
    /// or axiom it is on, where any selected object takes an axiom.
    ///
    /// True (the default) is the command-line meaning: removing an entity takes the
    /// assertions that point AT it. The in-process subsetters set it false — they
    /// remove a set of classes structurally, and an assertion on a class they KEEP
    /// must survive even when it names a class they drop, which is how a species
    /// subset keeps its `RO_0002175 … NCBITaxon_9606` assertions.
    pub annotation_values: Option<bool>,
    /// Force-exclude terms (read from `--exclude-term`/`--exclude-terms`).
    pub exclude_term: Vec<String>,
    pub exclude_terms: Vec<PathBuf>,
    /// Force-include terms (read from `--include-term`/`--include-terms`).
    pub include_term: Vec<String>,
    pub include_terms: Vec<PathBuf>,
    /// `--drop-axiom-annotations` values ([`AnnotationDrops`]).
    pub drop_axiom_annotations: Vec<String>,
    /// `--signature`: match on any-signature-entity intersection.
    pub signature: Option<crate::cmd::Switch>,
    /// `--trim`: keep/remove axioms containing only/any selected objects.
    pub trim: Option<crate::cmd::Switch>,
    /// `--allow-punning`.
    pub allow_punning: Option<crate::cmd::Switch>,
    /// `--preserve-structure` (default true): bridge the hierarchy across
    /// removed classes so retained subclasses inherit retained superclass
    /// expressions of the removed ones.
    pub preserve_structure: Option<crate::cmd::Switch>,
    /// Whether no term naming an IRI, with no `--select` or `--axioms`, selects
    /// every entity of the ontology: the `remove` command's meaning, so a bare
    /// `remove` takes every axiom. Off, a caller removing a list of its own
    /// removes nothing when the list is empty.
    pub whole_ontology_without_terms: bool,
}

impl Args {
    fn term_options(&self) -> TermOptions {
        TermOptions {
            annotation_values: None,
            exclude_term: self.exclude_term.clone(),
            exclude_terms: self.exclude_terms.clone(),
            include_term: self.include_term.clone(),
            include_terms: self.include_terms.clone(),
            drop_axiom_annotations: self.drop_axiom_annotations.clone(),
            signature: self.signature.clone(),
            trim: self.trim.clone(),
            allow_punning: self.allow_punning.clone(),
            preserve_structure: self.preserve_structure.clone(),
            whole_ontology_without_terms: true,
        }
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    // The whole closure is loaded, as it is for every command: what an import
    // declares is part of what the document says, and the render of the result
    // turns on it — an entity the closure declares gets no stub, so a module built
    // from an import-bearing source carries a different entity list depending on
    // whether the imports were followed. The result is still the ROOT ontology,
    // with its `owl:imports` intact (`maybe_save`).
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut kept = remove_with(
        model,
        &args.term,
        &args.term_file,
        &args.select,
        &args.axioms,
        &args.base_iri,
        &args.term_options(),
    )?;
    crate::cmd::maybe_save(&mut kept, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(kept))
}

/// Entry point that takes the default term options (no exclude/include/drop).
pub fn remove(
    model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    axioms: &[String],
    base_iri: &[String],
) -> Result<crate::model::Model> {
    remove_with(model, term, term_file, select, axioms, base_iri, &TermOptions::default())
}

/// Remove what a selection selects (pure core). The objects are what the terms
/// name — or, with no term naming an IRI, every object of the ontology —
/// mapped through each `--select` group in turn ([`objects::objects`]). An
/// axiom goes when any of its objects is selected (`--trim true`, the default)
/// or every one is (`--trim false`), judged by the IRIs it names alone under
/// `--signature true`. The hierarchy among what the remaining axioms still
/// name is then re-asserted across what went, from the ontology as it was —
/// without anonymous superclasses when a group selects `anonymous`.
/// `--select imports` and `--select ontology` drop the imports and the
/// ontology's annotations. Only the ontology's own axioms are judged.
pub fn remove_with(
    mut model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    axioms: &[String],
    base_iri: &[String],
    opts: &TermOptions,
) -> Result<crate::model::Model> {
    use horned_owl::model::MutableOntology;
    let terms = select::collect_terms(&model, term, term_file)?;
    let include = select::collect_terms(&model, &opts.include_term, &opts.include_terms)?;
    let exclude = select::collect_terms(&model, &opts.exclude_term, &opts.exclude_terms)?;

    let values: Vec<String> = if select.is_empty() { vec!["self".to_string()] } else { select.to_vec() };
    let mut anonymous = false;
    let mut groups: Vec<Vec<String>> = Vec::new();
    for value in &values {
        let mut group = objects::split_selects(value);
        // `--select imports` hands on the ontology without its closure: the
        // axioms the imports lent go, and the `owl:imports` declarations with
        // them.
        if let Some(i) = group.iter().position(|s| s == "imports") {
            let lent: Vec<_> = model
                .ont
                .iter()
                .filter(|ac| model.imported_components.contains(*ac) || matches!(ac.component, Component::Import(_)))
                .cloned()
                .collect();
            for ac in lent {
                model.ont.remove(&ac);
            }
            model.detach_import_closure();
            group.remove(i);
        }
        if let Some(i) = group.iter().position(|s| s == "ontology") {
            let header: Vec<_> =
                model.ont.iter().filter(|ac| matches!(ac.component, Component::OntologyAnnotation(_))).cloned().collect();
            for ac in header {
                model.ont.remove(&ac);
            }
            group.remove(i);
        }
        if group.iter().any(|s| s == "anonymous") {
            anonymous = true;
        }
        if !group.is_empty() {
            groups.push(group);
        }
    }

    let drops = AnnotationDrops::read(&model, &opts.drop_axiom_annotations)?;

    // A caller removing a list of its own removes nothing when the list names
    // no IRI and nothing else selects.
    let nothing_asked = terms.is_empty() && select.is_empty() && axioms.is_empty();
    if !(nothing_asked && !opts.whole_ontology_without_terms) {
        let axiom_selectors = objects::axiom_selectors(axioms);
        let base = objects::base_namespaces(&model, base_iri);
        let plain = objects::plain_datatype(&model);
        let mut span_shared: HashMap<String, u64> = HashMap::new();
        let mut cross_add: HashMap<String, u64> = HashMap::new();
        let (doomed, bridges) = {
            let sel = Selection::new(&model);
            let allow_punning = Switch::read(opts.allow_punning.as_ref(), "allow-punning", false)?;
            let request = Request {
                terms: &terms,
                include: &include,
                exclude: &exclude,
                groups: &groups,
                selected: !select.is_empty(),
                axiom_selectors: &axiom_selectors,
                allow_punning,
            };
            let related = objects::objects(&sel, &request)?;
            if related.is_empty() {
                (HashSet::new(), Vec::new())
            } else {
                let judge = Judge {
                    selectors: &axiom_selectors,
                    base: &base,
                    partial: Switch::read(opts.trim.as_ref(), "trim", true)?,
                    named_only: Switch::read(opts.signature.as_ref(), "signature", false)?,
                    annotation_values: opts.annotation_values.unwrap_or(true),
                    plain,
                };
                let doomed: HashSet<horned_owl::model::AnnotatedComponent<Rc>> =
                    objects::judge_axioms(sel.axioms(), &related, &judge)?.into_iter().collect();
                let bridges = if Switch::read(opts.preserve_structure.as_ref(), "preserve-structure", true)? {
                    // What the remaining axioms still name, less the selection:
                    // the objects a re-link may reach.
                    let mut surviving: HashSet<objects::Obj> = HashSet::new();
                    for ac in sel.axioms().iter().filter(|ac| !doomed.contains(**ac)) {
                        surviving.extend(objects::axiom_objects(&ac.component, Some(&ac.ann), plain));
                    }
                    for o in &related {
                        surviving.remove(o);
                    }
                    let root = own_ontology(&model);
                    let (internal, external) = objects::namespace_flags(&axiom_selectors);
                    span_gaps(root.as_ref().unwrap_or(&model), &surviving, &mut span_shared, &mut cross_add)
                    .into_iter()
                    .filter(|b| !(anonymous && matches!(b, Component::SubClassOf(sc) if !matches!(sc.sup, CE::Class(_)))))
                    .filter(|b| objects::in_namespace(b, internal, external, &base))
                    .collect()
                } else {
                    Vec::new()
                };
                (doomed, bridges)
            }
        };
        for ac in &doomed {
            model.ont.remove(ac);
        }
        for b in bridges {
            model.ont.insert(b);
        }
        model.span_shared.extend(span_shared);
        model.cross_shared.extend(cross_add);
    }
    drops.apply(&mut model)?;
    Ok(model)
}

/// The ontology without what its imports lend, where they lend anything.
pub(crate) fn own_ontology(model: &Model) -> Option<Model> {
    use horned_owl::model::MutableOntology;
    if model.imported_components.is_empty() {
        return None;
    }
    let mut root = model.clone();
    for ac in &model.imported_components {
        root.ont.remove(ac);
    }
    Some(root)
}

type Rc = horned_owl::model::RcStr;

/// The axioms that re-assert the hierarchy among `surviving` across what is not
/// in it. Each class of `surviving` is linked to each superclass expression
/// every entity of which is in `surviving` (an expression's datatypes
/// included), walking up through the named superclasses that are not; and
/// each property of `surviving` likewise to its super-properties. An entity is
/// in `surviving` as the kind it is, so a class whose IRI also names a removed
/// individual is a kept class.
///
/// `remove` passes the objects the remaining axioms name, less what it
/// removed, so an entity that is kept but no longer mentioned anywhere cannot
/// appear in a bridge; `filter` passes its selection, and `collapse` the
/// objects of what it keeps.
pub(crate) fn span_gaps(
    model: &Model,
    surviving: &HashSet<objects::Obj>,
    shared_out: &mut HashMap<String, u64>,
    cross_out: &mut HashMap<String, u64>,
) -> Vec<Component<Rc>> {
    use horned_owl::model::SubClassOf;
    let plain = objects::plain_datatype(model);
    let mut by_kind: HashMap<Kind, HashSet<&str>> = HashMap::new();
    for o in surviving {
        if let objects::Obj::Entity(kind, iri) = o {
            by_kind.entry(*kind).or_default().insert(iri.as_ref());
        }
    }
    let removed = |kind: Kind, iri: &str| !by_kind.get(&kind).is_some_and(|s| s.contains(iri));
    let is_removed = |iri: &str| removed(Kind::Class, iri);

    // Superclass expressions per named class IRI (from SubClassOf + the named
    // members of EquivalentClasses, flattening an intersection definition).
    let mut sup: HashMap<String, Vec<CE<Rc>>> = HashMap::new();
    // Equivalence map: named class -> the named classes asserted equivalent to it.
    // A superclass that is also asserted equivalent to the class is not a gap, so a
    // class equivalent to a removed one (e.g. `CHEBI_36080 ≡ PR:000000001`) is NOT
    // bridged — only its *subclasses* inherit the removed class's superclass
    // expressions.
    let mut equiv: HashMap<String, HashSet<String>> = HashMap::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(sc) => {
                if let CE::Class(c) = &sc.sub {
                    sup.entry(c.0.to_string()).or_default().push(sc.sup.clone());
                }
            }
            Component::EquivalentClasses(eq) => {
                let named: Vec<&str> = eq
                    .0
                    .iter()
                    .filter_map(|m| match m {
                        CE::Class(c) => Some(c.0.as_ref()),
                        _ => None,
                    })
                    .collect();
                for a in &named {
                    for b in &named {
                        if a != b {
                            equiv.entry(a.to_string()).or_default().insert(b.to_string());
                        }
                    }
                }
                // NOTE: an EquivalentClasses definition is deliberately NOT a source
                // of superclasses. The superclass expressions are the SubClassOf
                // axioms alone; the equivalence only feeds the exclusion test above.
                // Flattening a genus-differentia definition into `sup` here would
                // invent bridges the asserted hierarchy does not license (and is why
                // MONDO's `intersection_of:` lines stay untouched by this pass).
            }
            _ => {}
        }
    }

    let ce_retained = |ce: &CE<Rc>| objects::expression_entities(ce, plain).iter().all(|o| surviving.contains(o));

    let empty_eq: HashSet<String> = HashSet::new();
    // The asserted superclass expressions of `x`, minus any *named* one that is also
    // asserted equivalent to `x`, minus a self-loop. Anonymous expressions are
    // always kept.
    // Each superclass expression is carried with the identity of the source axiom
    // it came from — `(owning class, index)`. A re-link reuses that source
    // expression, so two re-links from one source are one blank node.
    // The expressions form a SET: two structurally-equal superclass expressions on
    // one class collapse to the first of them. The set is then walked in hash-bucket
    // order, which decides which source expression a re-link is attributed to when a
    // class can reach one structure through more than one removed ancestor — and so
    // which of the re-links this pass emits share a blank node. The order is a fixed
    // function of the expressions themselves, so one input always spends the same
    // blank nodes on the same bridges.
    let super_classes_of = |x: &str| -> Vec<(CE<Rc>, (String, String))> {
        let x_equiv = equiv.get(x).unwrap_or(&empty_eq);
        let Some(v) = sup.get(x) else { return Vec::new() };
        let mut seen: HashSet<String> = HashSet::new();
        let mut items: Vec<(CE<Rc>, (String, String))> = Vec::new();
        for (i, e) in v.iter().enumerate() {
            if let CE::Class(y) = e {
                if y.0.as_ref() == x || x_equiv.contains(y.0.as_ref()) {
                    continue;
                }
            }
            // A duplicate structure keeps the instance already present.
            if !seen.insert(crate::io::genid::ce_sig(e)) {
                continue;
            }
            items.push((e.clone(), (x.to_string(), i.to_string())));
        }
        let cap = crate::io::obo::owlapi_set_cap(items.len());
        let mut with_bucket: Vec<(usize, usize, (CE<Rc>, (String, String)))> = items
            .into_iter()
            .enumerate()
            .map(|(ins, it)| {
                let h = crate::owlapi_hash::ce_hash(&it.0, model.natural_order());
                let spread = (h ^ ((h as u32) >> 16) as i32) as usize;
                (spread & (cap - 1), ins, it)
            })
            .collect();
        with_bucket.sort_by_key(|(b, ins, _)| (*b, *ins));
        with_bucket.into_iter().map(|(_, _, it)| it).collect()
    };

    // The (sub, super) pairs are deduped as they are built, keeping whichever
    // superclass EXPRESSION was reached first. So which re-links end up sharing a
    // blank node is decided by the order the classes are walked in, not by anything
    // structural. Walk them in hash-bucket order: bucket index
    // `(h ^ h>>>16) & (cap-1)`, buckets ascending, ties broken by insertion order.
    //
    // A Rust `HashMap`'s iteration order is randomised per process, so it cannot
    // stand in here: the same build would emit different blank-node numbering run
    // to run.
    let ordered_classes: Vec<String> = {
        let mut keys: Vec<&String> = sup.keys().collect();
        keys.sort();
        // Capacity comes from the count of RETAINED entities — the object set the
        // pass re-asserts the hierarchy over — not from `keys.len()`. The classes
        // that have superclasses are only a subset of that set, so sizing the table
        // to the subset would land them in different buckets and change which
        // re-links share a blank node.
        let iris = objects::named_iris(surviving);
        let retained = select::entities(model).all().filter(|e| iris.contains(e.as_str())).count();
        let cap = crate::io::obo::owlapi_set_cap(retained);
        let mut with_bucket: Vec<(usize, usize, &String)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                // A named class hashes from its own kind seed mixed with the IRI
                // hash, not from the bare IRI hash.
                let h = 2293i32
                    .wrapping_mul(31)
                    .wrapping_add(crate::io::obo::owlapi_iri_hash(k));
                let spread = (h ^ ((h as u32) >> 16) as i32) as usize;
                (spread & (cap - 1), i, *k)
            })
            .collect();
        with_bucket.sort_by_key(|(b, i, _)| (*b, *i));
        with_bucket.into_iter().map(|(_, _, k)| k.clone()).collect()
    };
    let mut out: Vec<Component<Rc>> = Vec::new();
    // signature -> the source expressions every re-link with that signature came from
    // source expression -> the `owner\u{1}signature` keys of the re-links made from it
    let mut by_sig: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut cross_add: HashMap<String, u64> = HashMap::new();
    // The `SubClassOf` axioms the model ALREADY holds, by `subject + super`. A
    // bridge equal to one of them is absorbed — axiom equality is structural and
    // the ontology is a set — so it costs no blank node, and neither should the
    // grouping. Six of UBERON's bridges are absorbed this way, which is why gap
    // spanning costs it nothing at all there.
    let mut already: HashSet<String> = HashSet::new();
    let mut bridge_dbg = String::new();
    let span_log = std::env::var("OM_SPAN_LOG").is_ok();
    // The run-wide set of (subject, superclass) pairs, compared structurally, which
    // both dedupes re-links and gates the recursion.
    let mut class_pairs: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let CE::Class(c) = &sc.sub {
                // Only an axiom that SURVIVES absorbs a bridge: the bridges are
                // computed against the input but added to the filtered output, so a
                // duplicate of an axiom that is itself being dropped is still new.
                if is_removed(c.0.as_ref()) || !ce_retained(&sc.sup) {
                    continue;
                }
                already.insert(format!(
                    "{}\u{1}{}",
                    c.0.as_ref(),
                    crate::io::genid::ce_sig(&sc.sup)
                ));
            }
        }
    }
    for c in &ordered_classes {
        if is_removed(c) {
            continue;
        }
        // The hierarchy is re-asserted for EVERY retained class, not only those
        // sitting next to a gap: the walk visits each retained class's superclasses
        // and emits a PLAIN `SubClassOf`. Where the model already holds that same
        // axiom *annotated*, the plain copy is a distinct axiom, so every annotated
        // `is_a:`/`relationship:` gains an unannotated twin. That is not incidental —
        // it is why MONDO's `filtered.obo` goes from 46,491 to 91,265 `is_a:` lines
        // (the gain, 44,774, equals the number of annotated `is_a:` lines in the
        // input exactly).
        let csub = CE::Class(model.build.class(c.as_str()));
        // Walk the class's superclasses in bucket order, recursing IN PLACE at each
        // element. A superclass whose signature survives becomes a re-link — deduped
        // against the run-wide `class_pairs`, so the FIRST source reached wins and
        // any later path to the same pair is dropped outright. A removed named one is
        // stepped over carrying the SAME subject upwards, so the subject inherits
        // that ancestor's own superclass EXPRESSION. That is what decides which
        // re-links share a blank node: not the structure, but which originating axiom
        // the walk reached first.
        let mut stack: Vec<(CE<Rc>, (String, String), usize)> =
            super_classes_of(c).into_iter().rev().map(|(e, s)| (e, s, 0usize)).collect();
        // Path-local guard on the removed-ancestor step: without it a cycle among
        // removed classes would recurse forever.
        let mut path: Vec<HashSet<String>> = vec![HashSet::new()];
        while let Some((sc, src, depth)) = stack.pop() {
            path.truncate(depth + 1);
            if ce_retained(&sc) {
                // A walk that comes back to its own class through removed ones
                // re-links the class to itself.
                let sig = crate::io::genid::ce_sig(&sc);
                if !class_pairs.insert(format!("{c}\u{1}{sig}")) {
                    continue;
                }
                if !matches!(sc, CE::Class(_)) {
                    let bkey = format!("{}\u{1}{}", c, sig);
                    if span_log {
                        bridge_dbg.push_str(&format!("BR\t{}\t{}.{}\t{}\n", c, src.0, src.1, sig));
                    }
                    if !already.contains(&bkey) {
                        by_sig.entry(src.clone()).or_insert_with(Vec::new).push(bkey);
                    }
                }
                out.push(Component::SubClassOf(SubClassOf { sub: csub.clone(), sup: sc }));
            } else if let CE::Class(y) = &sc {
                // A removed named superclass: step over it and keep walking upwards.
                let mut seen = path[depth].clone();
                if !seen.insert(y.0.to_string()) {
                    continue;
                }
                path.push(seen);
                let d = path.len() - 1;
                stack.extend(
                    super_classes_of(y.0.as_ref()).into_iter().rev().map(|(e, s)| (e, s, d)),
                );
            }
            // An anonymous expression mentioning a removed entity is dropped
            // outright — it is not rebuilt.
        }
    }
    // The same bridging over the property hierarchies: span removed
    // object/data/annotation properties so retained sub-properties reconnect to
    // their nearest retained super-property.
    span_property_gaps(model, &removed, &mut out);
    // A signature is one shared object only when EVERY re-link carrying it traces
    // to the same source expression, and there is more than one of them.
    // Every re-link made from one source expression is that one object: give them a
    // group so the numbering pass can spend a single blank node on the set, without
    // touching any other occurrence of the same structure.
    // Deterministic order, and FIRST writer wins: the pair inserted first keeps the
    // expression. Iterating the map directly would leave the winner up to Rust's
    // randomised hash order whenever one (owner, structure) pair is reachable from
    // two sources.
    let mut group = 0u64;
    let mut span_dbg = String::new();
    let mut srcs: Vec<_> = by_sig.into_iter().collect();
    srcs.sort_by(|a, b| a.0.cmp(&b.0));
    for (_src, keys) in srcs {
        if keys.len() > 1 {
            group += 1;
            if std::env::var("OM_SPAN_LOG").is_ok() {
                span_dbg.push_str(&format!("group {group} source {:?} keys {}\n", _src, keys.len()));
            }
            for k in keys {
                shared_out.entry(k).or_insert(group);
            }
        }
    }
    for (k, g) in cross_add {
        cross_out.entry(k).or_insert(g);
    }
    if !span_dbg.is_empty() {
        std::fs::write("/tmp/om_span_sources.txt", &span_dbg).ok();
    }
    if !bridge_dbg.is_empty() {
        std::fs::write("/tmp/om_bridges.txt", &bridge_dbg).ok();
    }
    out
}

/// Re-assert the object, data and annotation property hierarchies across
/// removed properties, as [`span_gaps`] does for classes. Every retained
/// property is walked up its asserted super-properties: a retained one is a
/// re-link `sub ⊑ super`, asserted without annotations — so an annotated
/// sub-property axiom gains a plain twin, and a walk that comes back to its
/// start through removed properties asserts `p ⊑ p` — and a removed named one
/// is stepped over, keeping the subject. An object property's super-properties
/// include inverse expressions, kept when their property is kept; past a
/// removed object property the walk leaves out the property itself and the
/// properties asserted equivalent to it. Data and annotation properties are
/// walked over every asserted super-property.
fn span_property_gaps(
    model: &Model,
    removed: &dyn Fn(Kind, &str) -> bool,
    out: &mut Vec<Component<Rc>>,
) {
    use horned_owl::model::{
        SubAnnotationPropertyOf, SubDataPropertyOf, SubObjectPropertyOf,
        SubObjectPropertyExpression as SOPE,
    };
    use std::collections::BTreeSet;

    // The asserted super-properties of each named property, per kind.
    let mut obj: HashMap<String, BTreeSet<OPE<Rc>>> = HashMap::new();
    let mut obj_equiv: HashMap<String, HashSet<String>> = HashMap::new();
    let mut data: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut ann: HashMap<String, BTreeSet<String>> = HashMap::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubObjectPropertyOf(sp) => {
                if let SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)) = &sp.sub {
                    obj.entry(sub.0.to_string()).or_default().insert(sp.sup.clone());
                }
            }
            Component::EquivalentObjectProperties(eq) => {
                let named: Vec<&str> = eq
                    .0
                    .iter()
                    .filter_map(|m| match m {
                        OPE::ObjectProperty(p) => Some(p.0.as_ref()),
                        _ => None,
                    })
                    .collect();
                for a in &named {
                    for b in &named {
                        if a != b {
                            obj_equiv.entry(a.to_string()).or_default().insert(b.to_string());
                        }
                    }
                }
            }
            Component::SubDataPropertyOf(sp) => {
                data.entry(sp.sub.0.to_string()).or_default().insert(sp.sup.0.to_string());
            }
            Component::SubAnnotationPropertyOf(sp) => {
                ann.entry(sp.sub.0.to_string()).or_default().insert(sp.sup.0.to_string());
            }
            _ => {}
        }
    }

    // The re-links are sets, so the order the properties are walked in decides
    // nothing; a removed property is stepped over at most once on a path, so a
    // cycle among removed properties ends.
    let named_pairs = |sup: &HashMap<String, BTreeSet<String>>, kind: Kind| -> BTreeSet<(String, String)> {
        let mut pairs = BTreeSet::new();
        for (p, sups) in sup {
            if removed(kind, p) {
                continue;
            }
            let mut stack: Vec<(String, Vec<String>)> = sups.iter().map(|s| (s.clone(), Vec::new())).collect();
            while let Some((s, path)) = stack.pop() {
                if !removed(kind, &s) {
                    pairs.insert((p.clone(), s));
                } else if !path.contains(&s) {
                    let mut path = path;
                    path.push(s.clone());
                    if let Some(next) = sup.get(&s) {
                        stack.extend(next.iter().map(|t| (t.clone(), path.clone())));
                    }
                }
            }
        }
        pairs
    };

    // Past a removed object property: its super-properties other than itself
    // and the named ones asserted equivalent to it.
    let past = |r: &str| -> Vec<OPE<Rc>> {
        let equiv = obj_equiv.get(r);
        obj.get(r)
            .into_iter()
            .flatten()
            .filter(|s| match s {
                OPE::ObjectProperty(q) => q.0.as_ref() != r && !equiv.is_some_and(|e| e.contains(q.0.as_ref())),
                OPE::InverseObjectProperty(_) => true,
            })
            .cloned()
            .collect()
    };
    let mut obj_pairs: BTreeSet<(String, OPE<Rc>)> = BTreeSet::new();
    for (p, sups) in &obj {
        if removed(Kind::ObjectProperty, p) {
            continue;
        }
        let mut stack: Vec<(OPE<Rc>, Vec<String>)> = sups.iter().map(|s| (s.clone(), Vec::new())).collect();
        while let Some((s, path)) = stack.pop() {
            let (q, named) = match &s {
                OPE::ObjectProperty(q) => (q.0.to_string(), true),
                OPE::InverseObjectProperty(q) => (q.0.to_string(), false),
            };
            if !removed(Kind::ObjectProperty, &q) {
                obj_pairs.insert((p.clone(), s));
            } else if named && !path.contains(&q) {
                let mut path = path;
                path.push(q.clone());
                stack.extend(past(&q).into_iter().map(|t| (t, path.clone())));
            }
        }
    }

    for (p, s) in obj_pairs {
        out.push(Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: SOPE::ObjectPropertyExpression(OPE::ObjectProperty(model.build.object_property(p.as_str()))),
            sup: s,
        }));
    }
    for (p, s) in named_pairs(&data, Kind::DataProperty) {
        out.push(Component::SubDataPropertyOf(SubDataPropertyOf {
            sub: model.build.data_property(p.as_str()),
            sup: model.build.data_property(s.as_str()),
        }));
    }
    for (p, s) in named_pairs(&ann, Kind::AnnotationProperty) {
        out.push(Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
            sub: model.build.annotation_property(p.as_str()),
            sup: model.build.annotation_property(s.as_str()),
        }));
    }
}

/// What `--drop-axiom-annotations` asks to drop from the ontology's own
/// axioms: every annotation when any value is `all`, in any case; otherwise
/// the annotations of each property a value names, with any value for `PROP`,
/// the value given for `PROP=VALUE`, or a value `REGEX` finds for
/// `PROP=~REGEX`. A later value for a property replaces an earlier one.
pub(crate) struct AnnotationDrops {
    all: bool,
    /// Each property's IRI and the value it asks for, in the order first named.
    properties: Vec<(String, Option<String>)>,
}

impl AnnotationDrops {
    /// Read `values`, refusing a property that names no IRI. `PROP=VALUE` is
    /// split at every `=`, and its value is the text between the first and the
    /// second.
    pub(crate) fn read(model: &Model, values: &[String]) -> Result<AnnotationDrops> {
        let mut drops = AnnotationDrops { all: false, properties: Vec::new() };
        for value in values {
            if value.eq_ignore_ascii_case("all") {
                drops.all = true;
                continue;
            }
            let (property, wanted) = if value.contains('=') {
                let mut parts: Vec<&str> = value.split('=').collect();
                while parts.last() == Some(&"") {
                    parts.pop();
                }
                match parts.as_slice() {
                    [property, wanted, ..] => (*property, Some(wanted.to_string())),
                    _ => anyhow::bail!("drop-axiom-annotations \"{value}\" gives no value after `=`"),
                }
            } else {
                (value.as_str(), None)
            };
            let iri = select::iri(model, property).ok_or_else(|| {
                anyhow::anyhow!("INVALID IRI ERROR drop-axiom-annotations \"{property}\" is not a valid CURIE or IRI")
            })?;
            match drops.properties.iter_mut().find(|(p, _)| *p == iri) {
                Some(entry) => entry.1 = wanted,
                None => drops.properties.push((iri, wanted)),
            }
        }
        Ok(drops)
    }

    /// Take the annotations asked for off the ontology's own axioms; what the
    /// imports lend, the ontology's own annotations and annotations on
    /// annotations stay. An annotation's value is compared as an IRI's text or
    /// a literal's lexical form.
    pub(crate) fn apply(&self, model: &mut Model) -> Result<()> {
        use horned_owl::model::{AnnotatedComponent, AnnotationValue, HigherKinded, MutableOntology};
        if !self.all && self.properties.is_empty() {
            return Ok(());
        }
        let mut patterns: HashMap<usize, crate::dosdp::java::Regex> = HashMap::new();
        let mut cleaned = Vec::new();
        for ac in model.ont.iter() {
            if ac.ann.is_empty() || ac.is_meta() || model.imported_components.contains(ac) {
                continue;
            }
            let mut kept = std::collections::BTreeSet::new();
            if !self.all {
                for a in &ac.ann {
                    let Some(i) = self.properties.iter().position(|(p, _)| p.as_str() == a.ap.0.as_ref()) else {
                        kept.insert(a.clone());
                        continue;
                    };
                    let Some(wanted) = &self.properties[i].1 else { continue };
                    let text = match &a.av {
                        AnnotationValue::IRI(iri) => iri.as_ref().to_string(),
                        AnnotationValue::Literal(l) => l.literal().clone(),
                        AnnotationValue::AnonymousIndividual(_) => anyhow::bail!(
                            "drop-axiom-annotations: an annotation of <{}> has an anonymous individual for its value, which has no text to compare",
                            self.properties[i].0
                        ),
                    };
                    let dropped = match wanted.strip_prefix('~') {
                        Some(pattern) => {
                            let regex = match patterns.entry(i) {
                                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                                std::collections::hash_map::Entry::Vacant(e) => e.insert(
                                    crate::dosdp::java::Regex::new(&pattern.replace('\'', ""))
                                        .map_err(|e| anyhow::anyhow!("{e}"))?,
                                ),
                            };
                            regex.find(&text).map_err(|e| anyhow::anyhow!("{e}"))?.is_some()
                        }
                        None => wanted.replace('\'', "") == text,
                    };
                    if !dropped {
                        kept.insert(a.clone());
                    }
                }
            }
            if kept.len() != ac.ann.len() {
                cleaned.push((ac.clone(), AnnotatedComponent { component: ac.component.clone(), ann: kept }));
            }
        }
        for (old, new) in cleaned {
            model.ont.remove(&old);
            model.ont.insert(new);
        }
        Ok(())
    }
}
