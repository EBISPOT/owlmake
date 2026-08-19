//! `relax` — rewrite EquivalentClasses axioms involving conjunctions into the
//! weaker SubClassOf axioms a plain class hierarchy can carry: a named
//! superclass, or `R some F` with any filler. `--enforce-obo-format` narrows
//! that further to the `is_a`/`relationship:` shapes OBO format itself renders.
//!
//! For `A EquivalentTo B1 ⊓ B2 ⊓ ... ⊓ Bn`, assert `A SubClassOf Bi` for each
//! conjunct `Bi`. The equivalence axiom is retained (relax only *adds* the
//! weaker entailments); use `reduce` afterwards to drop redundancy.

use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{ClassExpression as CE, Component, MutableOntology, SubClassOf};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,

    /// Also relax asserted `SubClassOf(C, R exactly|min n F)` axioms to
    /// existentials (`<bool>`, default false). When false, only EquivalentClasses
    /// axioms are relaxed.
    #[arg(short = 's', long, num_args = 1, default_missing_value = "true")]
    pub include_subclass_of: Option<bool>,

    /// Only emit OBO-expressible relaxed superclasses (`<bool>`, default false):
    /// a named class, or `R some named`. Relaxed leaves that are not
    /// OBO-expressible are skipped.
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub enforce_obo_format: Option<bool>,

    /// Do not relax an EquivalentClasses axiom whose members are ALL named
    /// classes (`<bool>`, default false).
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub exclude_named_classes: Option<bool>,

    /// Suppress the degenerate self-subclass `C ⊑ C` that a self-referential
    /// genus `C ≡ C ⊓ X` otherwise relaxes to (`<bool>`, default false). The
    /// default emits the `C ⊑ C`, which `reduce` then collapses C's hierarchy to,
    /// because that is the shape released ontologies carry. Set true for the
    /// cleaner (and arguably more correct) output that keeps C's real parents.
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub clean_self_genus: Option<bool>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Options controlling which relaxed SubClassOf axioms are emitted. The defaults
/// relax only EquivalentClasses axioms, enforce no OBO-format restriction on the
/// result, skip named-to-named pairs, and emit the self-referential `C ⊑ C`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RelaxOptions {
    pub include_subclass_of: bool,
    pub enforce_obo_format: bool,
    pub exclude_named_classes: bool,
    pub clean_self_genus: bool,
}

impl Default for RelaxOptions {
    fn default() -> Self {
        RelaxOptions {
            // A plain `relax` relaxes ONLY EquivalentClasses, NOT standalone
            // SubClassOf.
            include_subclass_of: false,
            enforce_obo_format: false,
            // A named-to-named equivalence is NOT relaxed unless asked for:
            // asserting `A ⊑ B` for such a pair turns an identity between two
            // names into a taxonomic parent, a hierarchy edge no release should
            // carry (see the per-pair check in `relax_with`).
            exclude_named_classes: true,
            // Emit `C ⊑ C` for a self-referential genus by default.
            clean_self_genus: false,
        }
    }
}

impl Args {
    fn options(&self) -> RelaxOptions {
        RelaxOptions {
            include_subclass_of: self.include_subclass_of.unwrap_or(false),
            enforce_obo_format: self.enforce_obo_format.unwrap_or(false),
            exclude_named_classes: self.exclude_named_classes.unwrap_or(true),
            clean_self_genus: self.clean_self_genus.unwrap_or(false),
        }
    }
}

/// True for a relaxed superclass expressible in OBO: a named class (`is_a`) or
/// `R some named` (a `relationship:`). Mirrors what the OBO writer can render.
fn obo_expressible(ce: &CE<horned_owl::model::RcStr>) -> bool {
    match ce {
        CE::Class(_) => true,
        CE::ObjectSomeValuesFrom { ope, bce } => {
            matches!(ope, horned_owl::model::ObjectPropertyExpression::ObjectProperty(_))
                && matches!(bce.as_ref(), CE::Class(_))
        }
        _ => false,
    }
}

/// Collect the leaf conjuncts of a class expression, flattening any nested
/// `ObjectIntersectionOf` (but not descending into restriction fillers).
fn collect_conjuncts<'a>(ce: &'a CE<horned_owl::model::RcStr>, out: &mut Vec<&'a CE<horned_owl::model::RcStr>>) {
    match ce {
        CE::ObjectIntersectionOf(parts) => {
            for p in parts {
                collect_conjuncts(p, out);
            }
        }
        other => out.push(other),
    }
}

/// Weaken a single relaxed conjunct: *any* `ObjectCardinalityRestriction` with
/// cardinality `≥ 1` — min, exact, **and max** — becomes the existential
/// `R some F`. (Max cardinality `≤ k` does not logically entail `R some F`, so
/// that case is not sound; it is kept because released ontologies carry those
/// existentials and dropping them would rewrite every downstream diff.)
fn relax_leaf(leaf: &CE<horned_owl::model::RcStr>) -> CE<horned_owl::model::RcStr> {
    match leaf {
        CE::ObjectMinCardinality { n, ope, bce } if *n >= 1 => CE::ObjectSomeValuesFrom {
            ope: ope.clone(),
            bce: bce.clone(),
        },
        CE::ObjectExactCardinality { n, ope, bce } if *n >= 1 => CE::ObjectSomeValuesFrom {
            ope: ope.clone(),
            bce: bce.clone(),
        },
        CE::ObjectMaxCardinality { n, ope, bce } if *n >= 1 => CE::ObjectSomeValuesFrom {
            ope: ope.clone(),
            bce: bce.clone(),
        },
        other => other.clone(),
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
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut model = relax_with(model, &args.options());
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Relax with the default options — the entry point re-exported as `api::relax`
/// for the embedding API and its language bindings.
pub fn relax(model: crate::model::Model) -> crate::model::Model {
    relax_with(model, &RelaxOptions::default())
}

/// Relax equivalence axioms into weaker SubClassOf existentials (pure core).
pub fn relax_with(mut model: crate::model::Model, opts: &RelaxOptions) -> crate::model::Model {
    // NOTE: `include_subclass_of` does NOT gate relaxation as a whole —
    // EquivalentClasses is ALWAYS relaxed; the flag only additionally relaxes
    // standalone `SubClassOf` axioms (handled below, gated). The equivalence
    // relaxation here therefore always runs.
    let mut to_add: Vec<Component<_>> = Vec::new();
    // (owner IRI, anon-signature hash) whose blank node relax shares between the
    // kept equivalence operand and the derived superclass.
    let mut derived_shared: Vec<(String, u64)> = Vec::new();

    // A relaxed `C ⊑ X` that duplicates an already-asserted `C ⊑ X` (e.g. an
    // ontology asserts both `C ≡ G ⊓ ∃r.D` and, with provenance annotations,
    // `C ⊑ ∃r.D`) must not be added as a second, bare axiom: RDF cannot carry two
    // identical `C subClassOf X` triples, so only the pre-existing (annotated)
    // axiom survives serialisation. Relaxed superclasses already present are
    // therefore skipped (ignoring axiom annotations).
    let existing: std::collections::HashSet<(CE<_>, CE<_>)> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::SubClassOf(sc) => Some((sc.sub.clone(), sc.sup.clone())),
            _ => None,
        })
        .collect();

    // Adding an axiom EQUAL to one already asserted is a no-op, and equality
    // includes annotations. So the derived superclass is recorded as sharing the
    // equivalence operand's blank node only when nothing identical is already
    // asserted bare: an UNANNOTATED pre-existing twin blocks the record; MONDO's
    // annotated `relationship: R F {source=…}` does not, so that case still shares.
    let existing_plain: std::collections::HashSet<(CE<_>, CE<_>)> = model
        .ont
        .iter()
        .filter(|ac| ac.ann.is_empty())
        .filter_map(|ac| match &ac.component {
            Component::SubClassOf(sc) => Some((sc.sub.clone(), sc.sup.clone())),
            _ => None,
        })
        .collect();

    for ac in model.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            // Find a named class member and a conjunction member.
            let named: Vec<&CE<_>> = eq.0.iter().filter(|c| matches!(c, CE::Class(_))).collect();
            for n in &named {
                for member in &eq.0 {
                    if std::ptr::eq(*n, member) {
                        continue;
                    }
                    // `--exclude-named-classes` is decided PER PAIR, not per
                    // axiom: each named member is skipped on its own, so a
                    // three-way `EquivalentClasses(A, B, ∃r.C)` still relaxes
                    // A ⊑ ∃r.C while dropping A ⊑ B. Skipping only when EVERY
                    // member is named would relax GO's `X ≡ PR_000000001` pairs
                    // inside such mixed axioms, adding `rdfs:subClassOf` lines to
                    // `mirror/go.owl` that belong in no release.
                    if opts.exclude_named_classes && matches!(member, CE::Class(_)) {
                        continue;
                    }
                    // Flatten nested intersections: `X ≡ ((A ⊓ ∃r.B) ⊓ ∃s.C)`
                    // relaxes to `X ⊑ A`, `X ⊑ ∃r.B`, `X ⊑ ∃s.C` — one weaker
                    // SubClassOf per *leaf* conjunct.
                    let mut leaves: Vec<&CE<_>> = Vec::new();
                    collect_conjuncts(member, &mut leaves);
                    for leaf in leaves {
                        // A leaf identical to the defined class itself (a
                        // self-referential genus `C ≡ C ⊓ X`) relaxes to the
                        // degenerate `C ⊑ C`, which is emitted by default (a later
                        // `reduce` then collapses C's hierarchy to that self-loop);
                        // `--clean-self-genus` skips it to retain C's real parents.
                        if leaf == *n {
                            if opts.clean_self_genus {
                                continue;
                            }
                            // Emit `C ⊑ C` verbatim (no leaf-weakening needed).
                            to_add.push(Component::SubClassOf(SubClassOf {
                                sub: (*n).clone(),
                                sup: (*n).clone(),
                            }));
                            continue;
                        }
                        // A qualified-cardinality conjunct (`R exactly|min|max k F`,
                        // k≥1) is weakened to the existential `R some F` rather than
                        // asserted verbatim — the cardinality stays only inside the
                        // kept equivalence axiom. See `relax_leaf` for the max case.
                        let sup = relax_leaf(leaf);
                        // A weaker superclass is emitted only for a named class or an
                        // existential (`R some …`, incl. a cardinality weakened to
                        // one). Conjuncts of any other shape — `ObjectUnionOf`,
                        // `ObjectComplementOf`, universals, `hasValue`/`hasSelf`/
                        // `oneOf` — have no weaker OBO-shaped consequence and are
                        // skipped, so e.g. an OBO `X ≡ G ⊓ (∃r.A ⊔ ∃r.B)` relaxes to
                        // `X ⊑ G` only.
                        if !matches!(sup, CE::Class(_) | CE::ObjectSomeValuesFrom { .. }) {
                            continue;
                        }
                        // With --enforce-obo-format, additionally require the
                        // existential's filler to be a named class (`R some named`).
                        if opts.enforce_obo_format && !obo_expressible(&sup) {
                            continue;
                        }
                        // The derived superclass is a structural CLONE of the
                        // equivalence operand, so nothing in the two axioms ties them
                        // together; the RDF writer takes blank-node sharing from
                        // `Model::shared_anon` instead. Recording the (owner,
                        // signature) pair here is what makes it emit ONE blank node
                        // shared between the intersection operand and the
                        // `rdfs:subClassOf` edge.
                        // Record it BEFORE the already-asserted skip below,
                        // because the sharing happens even when relax adds nothing:
                        // MONDO's `relationship: R F {source=…}` already asserts the
                        // axiom, and its released `mondo.owl` still shares the node.
                        // Only an unweakened leaf qualifies; a cardinality weakened
                        // to an existential is a freshly built expression.
                        if sup == *leaf
                            && !matches!(sup, CE::Class(_))
                            && !existing_plain.contains(&((*n).clone(), sup.clone()))
                        {
                            if let CE::Class(owner) = *n {
                                derived_shared.push((
                                    owner.0.as_ref().to_string(),
                                    crate::io::anon_sig_hash(&crate::io::genid::ce_sig(&sup)),
                                ));
                            }
                        }
                        // A pre-existing ANNOTATED twin does not block the derived
                        // axiom: the two are different axioms, and functional syntax
                        // carries both (the `*-view` subsets end in `convert -f ofn`
                        // and the reference writes the annotated axiom AND its bare
                        // derivation). RDF/XML cannot repeat the triple, so the
                        // writer collapses them there — that is the serialiser's job,
                        // not this step's.
                        if existing_plain.contains(&((*n).clone(), sup.clone())) {
                            continue;
                        }
                        to_add.push(Component::SubClassOf(SubClassOf {
                            sub: (*n).clone(),
                            sup,
                        }));
                    }
                }
            }
        }
    }

    // Asserted `SubClassOf` axioms are also processed, under
    // `--include-subclass-of` (default FALSE — a plain `relax` relaxes ONLY
    // EquivalentClasses). Gating this is what keeps a plain `relax` from
    // weakening standalone `C ⊑ R exactly|min n F` cardinality axioms to
    // existentials the curator never asserted.
    //
    // A superclass INTERSECTION is flattened into its leaf conjuncts, exactly as
    // an equivalence member is, and every leaf `relax` can emit — a named class, or
    // an existential (including a cardinality weakened to one) — contributes a
    // SubClassOf axiom. ECTO's `FOODON_03400229 ⊑ (∃RO_0002351.X ⊓ ∀RO_0002351.X)`
    // gains `⊑ ∃RO_0002351.X` in `ecto-full.owl` — and only that one, since the
    // universal is not a shape `relax` emits.
    if opts.include_subclass_of {
        for ac in model.ont.iter() {
            if let Component::SubClassOf(sc) = &ac.component {
                if !matches!(sc.sub, CE::Class(_)) {
                    continue;
                }
                let mut leaves: Vec<&CE<_>> = Vec::new();
                collect_conjuncts(&sc.sup, &mut leaves);
                for leaf in leaves {
                    let sup = relax_leaf(leaf);
                    if !matches!(sup, CE::Class(_) | CE::ObjectSomeValuesFrom { .. }) {
                        continue;
                    }
                    if opts.enforce_obo_format && !obo_expressible(&sup) {
                        continue;
                    }
                    if existing.contains(&(sc.sub.clone(), sup.clone())) {
                        continue;
                    }
                    to_add.push(Component::SubClassOf(SubClassOf {
                        sub: sc.sub.clone(),
                        sup,
                    }));
                }
            }
        }
    }

    let mut added = 0;
    for c in to_add {
        if model.ont.insert(c) {
            added += 1;
        }
    }
    for (owner, sig) in derived_shared {
        model.shared_anon.entry(owner).or_default().insert(sig);
    }
    status!("relax: added {added} SubClassOf axiom(s)");
    model
}
