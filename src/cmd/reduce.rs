//! `reduce` — remove redundant SubClassOf axioms (those entailed transitively
//! by the remaining axioms).
//!
//! An asserted `A SubClassOf B` (both named) is redundant when the reasoner
//! still entails it after the axiom is removed — i.e. there is an inferred path
//! `A ⊑ C ⊑ B` through some other class C. We compute this from the inferred
//! direct-subsumption hierarchy: any asserted named subsumption that is not a
//! direct edge is redundant.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{
    ClassExpression as CE, Component, MutableOntology, ObjectPropertyExpression as OPE,
};

use crate::model::Model;
use crate::reason::Reasoner;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Reasoner to use. Reduction runs on the built-in EL reasoner.
    #[arg(short = 'r', long, default_value = "elk")]
    pub reasoner: String,
    /// Preserve redundant axioms that carry annotations (`<bool>`).
    #[arg(short = 'p', long, num_args = 1, default_missing_value = "true")]
    pub preserve_annotated_axioms: Option<bool>,
    /// Take subproperties into account over existential restrictions (`<bool>`,
    /// default false). A bare `reduce`, as OBA's build runs it, therefore does
    /// NOT eliminate existentials entailed only via sub-property or
    /// property-chain reasoning. Pass `--include-subproperties true` for the more
    /// aggressive reduction.
    #[arg(short = 's', long, num_args = 1, default_missing_value = "true")]
    pub include_subproperties: Option<bool>,
    /// Only reduce named `A ⊑ B` subclass axioms (`<bool>`).
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true")]
    pub named_classes_only: Option<bool>,
    /// Use exact entailment-based reduction (drop an axiom iff the ontology minus
    /// it still entails it), via ⊥-module localization. Slower on huge ontologies
    /// than the default structural reduction, and exact rather than heuristic.
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
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    // `--reasoner owlmake` enables union-elimination in the reduce reasoner; every
    // other value runs on the plain EL engine. Set before classifying.
    crate::reason::configure(&args.reasoner);
    let preserve = args.preserve_annotated_axioms.unwrap_or(false);
    let named_only = args.named_classes_only.unwrap_or(false);
    // `--include-subproperties` defaults to false, so existentials entailed only
    // via sub-property or property-chain reasoning are KEPT unless the flag is
    // explicitly set.
    let subprops = args.include_subproperties.unwrap_or(false);
    let mut reduced = if args.exact.unwrap_or(false) {
        reduce_exact(&model, preserve, named_only, subprops)
    } else {
        reduce_with_opts(&model, preserve, named_only, subprops)
    };
    crate::cmd::maybe_save(&mut reduced, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(reduced))
}

/// Remove redundant SubClassOf axioms from `model`:
///  * named ⊑ named that are not direct edges (transitive reduction), and
///  * `C ⊑ ∃R.F` (named filler) that are entailed by another existential
///    superclass of `C` or one of its ancestors — i.e. some `C' ⊒ C` asserts
///    `C' ⊑ ∃R'.F'` with `R' ⊑ R` and `F' ⊑ F` (what
///    `--include-subproperties true` licenses over existential restrictions).
pub fn reduce(model: &Model) -> Model {
    reduce_with(model, false, false)
}

/// Options for [`reduce_with_options`] — the named-struct form of the boolean
/// flags accepted by [`reduce_with`]/[`reduce_with_opts`]. Every option defaults
/// to false, the conservative reduction.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ReduceOptions {
    /// Keep redundant axioms that carry axiom annotations
    /// (`--preserve-annotated-axioms`).
    pub preserve_annotated: bool,
    /// Reduce only named `A ⊑ B` subclass axioms (`--named-classes-only`).
    pub named_classes_only: bool,
    /// Let a sub-role `R' ⊑ R` dominate an existential
    /// (`--include-subproperties`).
    pub include_subproperties: bool,
}

/// Transitive reduction with [`ReduceOptions`] — the recommended form (the
/// boolean [`reduce_with`]/[`reduce_with_opts`] remain for convenience).
pub fn reduce_with_options(model: &Model, opts: &ReduceOptions) -> Model {
    reduce_with_opts(
        model,
        opts.preserve_annotated,
        opts.named_classes_only,
        opts.include_subproperties,
    )
}

/// Like [`reduce`], but `preserve_annotated` keeps redundant axioms that carry
/// axiom annotations (`--preserve-annotated-axioms true`), and
/// `named_classes_only` reduces only named `A ⊑ B` subclass axioms, leaving
/// existential/complex superclass axioms untouched (`--named-classes-only`).
pub fn reduce_with(model: &Model, preserve_annotated: bool, named_classes_only: bool) -> Model {
    reduce_with_opts(model, preserve_annotated, named_classes_only, false)
}

/// Like [`reduce_with`] but with explicit `include_subproperties`
/// (`--include-subproperties`, default false): when false, a `C ⊑ ∃R.F` is only
/// dominated by another existential with the *same* role R (plus filler
/// subsumption); when true, a sub-role `R' ⊑ R` also dominates. Property *chains*
/// (transitivity etc.) are applied regardless — they are not subproperties.
pub fn reduce_with_opts(
    model: &Model,
    preserve_annotated: bool,
    named_classes_only: bool,
    include_subproperties: bool,
) -> Model {
    use horned_owl::model::{Build, RcStr};
    use horned_owl::ontology::set::SetOntology;
    use horned_owl::model::MutableOntology;

    // === How redundancy is decided ============================================
    //
    // Redundancy of an asserted `C ⊑ X` is decided purely within the SubClassOf
    // graph: `C ⊑ X` is redundant iff `C` has *another asserted* superclass `Y`
    // whose strict superclasses include `X`. The reasoner runs over a
    // **sub-ontology** that contains only `SubClassOf` axioms + object-property
    // *characteristic* axioms (transitivity, …) and — *only* with
    // `--include-subproperties` — `SubObjectPropertyOf`/`SubPropertyChainOf`. So
    // reduction proceeds via transitivity but NOT via property chains or the role
    // hierarchy unless asked. Anonymous class expressions on either side of a
    // `SubClassOf` are mapped to fresh named temp classes via
    // `EquivalentClasses(temp, expr)` so the reasoner can place them.
    //
    // One pass over that sub-ontology, not a battery of bespoke structural
    // chain/transitivity/GCI rules: those would apply the property chains a
    // reasoner over the *full* model entails, over-removing existentials on
    // chain-heavy ontologies (OBA `develops_from`/`part_of`). owlmake's reasoner
    // is chain- and range-aware, so the chain-free sub-ontology built here is what
    // keeps those existentials asserted.

    let build = Build::new_rc();
    // Distinct class-expression (Debug-keyed) → the named/temp class IRI standing
    // for it in the reduce reasoner. Named classes map to their own IRI; anonymous
    // expressions get a fresh `urn:owlmake-reduce-temp-N` class with a temp
    // `EquivalentClasses`.
    let mut expr_to_iri: HashMap<String, String> = HashMap::new();
    let mut temps: Vec<(String, CE<RcStr>)> = Vec::new();
    let mut tmpn: usize = 0;
    fn map_ce(
        ce: &CE<RcStr>,
        expr_to_iri: &mut HashMap<String, String>,
        temps: &mut Vec<(String, CE<RcStr>)>,
        tmpn: &mut usize,
    ) -> String {
        if let CE::Class(c) = ce {
            return c.0.to_string();
        }
        let k = format!("{ce:?}");
        if let Some(iri) = expr_to_iri.get(&k) {
            return iri.clone();
        }
        let iri = format!("urn:owlmake-reduce-temp-{tmpn}");
        *tmpn += 1;
        expr_to_iri.insert(k, iri.clone());
        temps.push((iri.clone(), ce.clone()));
        iri
    }
    // Read-only key lookup for the removal pass (every SubClassOf expr was mapped).
    let key_of = |ce: &CE<RcStr>, expr_to_iri: &HashMap<String, String>| -> Option<String> {
        match ce {
            CE::Class(c) => Some(c.0.to_string()),
            _ => expr_to_iri.get(&format!("{ce:?}")).cloned(),
        }
    };

    // Asserted named equivalence pairs. A `SubClassOf(C, X)` between two classes
    // that are asserted equivalent is entailed by that `EquivalentClasses` axiom,
    // so it is dropped and only the equivalence kept (the mutual-subclass form is
    // never emitted). Crucially the equivalence is preserved independently, so
    // removing the subclass axiom loses nothing. These mutual-subclass edges are
    // also kept OUT of the reduce reasoner and the asserted-superclass map below,
    // so they cannot collapse a class onto its equivalent partner and spuriously
    // dominate that partner's other (e.g. existential) superclasses.
    let mut equiv_pairs: HashSet<(String, String)> = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            let named: Vec<String> = eq
                .0
                .iter()
                .filter_map(|c| match c {
                    CE::Class(k) => Some(k.0.to_string()),
                    _ => None,
                })
                .collect();
            for i in 0..named.len() {
                for j in 0..named.len() {
                    if i != j {
                        equiv_pairs.insert((named[i].clone(), named[j].clone()));
                    }
                }
            }
        }
    }
    let is_equiv_pair = |sc: &horned_owl::model::SubClassOf<RcStr>| -> bool {
        matches!(
            (&sc.sub, &sc.sup),
            (CE::Class(a), CE::Class(b))
                if equiv_pairs.contains(&(a.0.to_string(), b.0.to_string()))
        )
    };

    // Asserted superclasses per (mapped) subject, over all `SubClassOf` axioms
    // (excluding mutual-subclass edges between asserted-equivalent classes).
    let mut asserted_supers: HashMap<String, HashSet<String>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if named_classes_only
                && !matches!((&sc.sub, &sc.sup), (CE::Class(_), CE::Class(_)))
            {
                continue;
            }
            if is_equiv_pair(sc) {
                continue;
            }
            let sk = map_ce(&sc.sub, &mut expr_to_iri, &mut temps, &mut tmpn);
            let pk = map_ce(&sc.sup, &mut expr_to_iri, &mut temps, &mut tmpn);
            asserted_supers.entry(sk).or_default().insert(pk);
        }
    }

    // Build the reduce sub-ontology: SubClassOf + property characteristics always;
    // SubObjectPropertyOf / chains only with --include-subproperties; plus the temp
    // equivalences. Deliberately excludes the ontology's own EquivalentClasses,
    // DisjointClasses, domains and ranges, so redundancy is decided from the
    // subsumption graph alone.
    let mut ont: SetOntology<RcStr> = SetOntology::new();
    for ac in model.ont.iter() {
        let keep = match &ac.component {
            Component::SubClassOf(sc) => !is_equiv_pair(sc),
            Component::TransitiveObjectProperty(_)
            | Component::ReflexiveObjectProperty(_)
            | Component::IrreflexiveObjectProperty(_)
            | Component::SymmetricObjectProperty(_)
            | Component::AsymmetricObjectProperty(_)
            | Component::FunctionalObjectProperty(_)
            | Component::InverseFunctionalObjectProperty(_) => true,
            Component::SubObjectPropertyOf(_) => include_subproperties,
            _ => false,
        };
        if keep {
            ont.insert(ac.clone());
        }
    }
    for (iri, ce) in &temps {
        let temp = CE::Class(build.class(iri.as_str()));
        ont.insert(Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
            temp,
            ce.clone(),
        ])));
    }
    let rr = Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes));
    let reasoner = Reasoner::classify(&rr);

    // `X` is a *strict* superclass of `Y`: `Y ⊑ X` and not `X ⊑ Y` (so equivalents
    // are excluded).
    let strict = |y: &str, x: &str| reasoner.is_subsumed(y, x) && !reasoner.is_subsumed(x, y);

    // Degenerate self-referential definitions `C ≡ (C ⊓ D ⊓ …)` — where C itself
    // is a conjunct of its own equivalent intersection — are logically just
    // `C ⊑ D` (and `C ⊑ …`). The asserted/relaxed `C ⊑ D` is therefore redundant
    // with the retained equivalence and is dropped. (This does NOT apply to a
    // normal genus `X ≡ G ⊓ ∃r.D` where X is not among the conjuncts — there
    // `X ⊑ G` is a real kept superclass.) Map C → the keys of its co-conjuncts.
    let mut self_genus_supers: HashMap<String, HashSet<String>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            for (i, m) in eq.0.iter().enumerate() {
                let c = match m {
                    CE::Class(k) => k.0.to_string(),
                    _ => continue,
                };
                for (j, other) in eq.0.iter().enumerate() {
                    if i == j {
                        continue;
                    }
                    if let CE::ObjectIntersectionOf(parts) = other {
                        let c_is_conjunct = parts
                            .iter()
                            .any(|p| matches!(p, CE::Class(k) if k.0.as_ref() == c));
                        if !c_is_conjunct {
                            continue;
                        }
                        for p in parts {
                            if let Some(pk) = key_of(p, &expr_to_iri) {
                                if pk != c {
                                    self_genus_supers.entry(c.clone()).or_default().insert(pk);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Self-referential genus collapse (the default). When `relax` has emitted the
    // degenerate self-loop `C ⊑ C` (from `C ≡ C ⊓ X`), that loop serves as a
    // transitive-reduction "via" for *every* other superclass of C — so C's entire
    // named/existential parent set is dropped and only the `C ⊑ C` kept. Any class
    // carrying an asserted self loop is collapsed this way.
    // (`--clean-self-genus` on `relax` suppresses the loop, so this set is empty
    // there and C's real parents are kept by the `self_genus_supers` rule.)
    let mut self_loop_classes: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(a), CE::Class(b)) = (&sc.sub, &sc.sup) {
                if a.0 == b.0 {
                    self_loop_classes.insert(a.0.to_string());
                }
            }
        }
    }

    // Decide redundancy. Keyed by (subject-iri, super-iri) using the same mapping,
    // so the removal pass can recompute the key without the temp table.
    let mut redundant: HashSet<(String, String)> = HashSet::new();
    for ac in model.ont.iter() {
        let sc = match &ac.component {
            Component::SubClassOf(sc) => sc,
            _ => continue,
        };
        if named_classes_only && !matches!((&sc.sub, &sc.sup), (CE::Class(_), CE::Class(_))) {
            continue;
        }
        let (sk, pk) = match (key_of(&sc.sub, &expr_to_iri), key_of(&sc.sup, &expr_to_iri)) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        if redundant.contains(&(sk.clone(), pk.clone())) {
            continue;
        }
        // Never drop the self-loop itself; collapse every *other* superclass of a
        // self-looped class (the self-referential-genus rule above).
        if sk == pk {
            continue;
        }
        if self_loop_classes.contains(&sk) {
            redundant.insert((sk, pk));
            continue;
        }
        let sub_is_anon = !matches!(sc.sub, CE::Class(_));
        // A subclass axiom between two asserted-equivalent named classes is
        // entailed by the (retained) EquivalentClasses axiom — redundant.
        if equiv_pairs.contains(&(sk.clone(), pk.clone())) {
            redundant.insert((sk, pk));
            continue;
        }
        // Entailed by a degenerate self-referential equivalence `C ≡ C ⊓ X`.
        if self_genus_supers.get(&sk).is_some_and(|s| s.contains(&pk)) {
            redundant.insert((sk, pk));
            continue;
        }
        // Main rule: another asserted *strict* superclass Y of C (C ⊏ Y, so Y is
        // not equivalent to C) with X a strict super of Y. Requiring `strict(C,Y)`
        // — not merely that Y is asserted — excludes equivalent classes as
        // transitive-reduction intermediates: when the only "via" is a class
        // equivalent to C (a mutual-subclass cycle), C ⊑ X is a real direct
        // superclass and must be kept, not dropped.
        let mut is_red = asserted_supers
            .get(&sk)
            .is_some_and(|sups| sups.iter().any(|y| *y != pk && strict(&sk, y) && strict(y, &pk)));
        // GCI special case: anonymous subject. Any strict super `ip` of the subject
        // that is itself an asserted-sub class and has `X` as a strict super.
        if !is_red && sub_is_anon {
            is_red = asserted_supers
                .keys()
                .any(|ip| *ip != sk && strict(&sk, ip) && strict(ip, &pk));
        }
        if is_red {
            redundant.insert((sk, pk));
        }
    }

    // Removal pass: drop redundant SubClassOf axioms (respecting
    // --preserve-annotated-axioms and --named-classes-only).
    let mut out = SetClone::new(model);
    out.retain(|ac| {
        if preserve_annotated && !ac.ann.is_empty() {
            return true;
        }
        match &ac.component {
            Component::SubClassOf(ax) => {
                if named_classes_only
                    && !matches!((&ax.sub, &ax.sup), (CE::Class(_), CE::Class(_)))
                {
                    return true;
                }
                match (key_of(&ax.sub, &expr_to_iri), key_of(&ax.sup, &expr_to_iri)) {
                    (Some(sk), Some(pk)) => !redundant.contains(&(sk, pk)),
                    _ => true,
                }
            }
            _ => true,
        }
    });
    let mut result = out.into_model();
    result.carry_meta_from(model);
    result
}

/// Exact reduction: an axiom is removed iff the ontology *minus that axiom*
/// still entails it. Named `A ⊑ B` uses transitive reduction (which is
/// exactly the entailment test for named subsumption). Each existential
/// `C ⊑ ∃R.F` (named F) is checked against the ⊥-locality module of `O − α` over
/// its signature — a small ontology that preserves exactly the Σ-entailments of
/// `O − α`, so the entailment check is exact without re-classifying all of `O`.
///
/// This is `O(candidates × module-extraction)`; on very large ontologies it is
/// slower than the structural [`reduce`], which is why it is opt-in (`--exact`).
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
