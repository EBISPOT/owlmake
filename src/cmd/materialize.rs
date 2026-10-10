//! `materialize` — assert the existential restrictions a class is entailed to
//! hold, so `C ⊑ ∃R.D` becomes an asserted axiom and a consumer that does no
//! reasoning still sees the relation.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    ClassExpression as CE, Component, MutableOntology, ObjectPropertyExpression as OPE, SubClassOf,
};

use crate::cmd::reason::ReasonerKind;
use crate::cmd::select;
use crate::reason::Reasoner;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Object properties to materialize over (IRIs/CURIEs, repeatable). If no
    /// properties are given, all properties are materialized.
    #[arg(short = 't', long)]
    pub term: Vec<String>,
    /// Load properties to materialize over from a file, one per line. Blank
    /// lines and `#` comments are ignored.
    #[arg(short = 'T', long = "term-file", value_name = "FILE")]
    pub term_file: Vec<PathBuf>,
    /// Reasoner the ontology is checked and classified with: `elk`, `hermit`,
    /// `jfact`, `whelk`, `structural`, or `owlmake` (the built-in EL reasoner
    /// with union-elimination).
    #[arg(short = 'r', long, default_value = "elk")]
    pub reasoner: String,
    /// Any value. Changes nothing: what materialize asserts is never annotated.
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub annotate_inferred_axioms: Option<bool>,
    /// `true` or `yes` in any case: put the materialized axioms in an ontology
    /// of their own, which is not written: what is written is the input as it
    /// was.
    #[arg(short = 'n', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub create_new_ontology: Option<bool>,
    /// Any value. Changes nothing: materialize removes no axiom.
    #[arg(short = 's', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub remove_redundant_subclass_axioms: Option<bool>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    // A reasoner name that is no reasoner fails before anything is loaded.
    let kind = ReasonerKind::parse_without_emr(&args.reasoner)?;
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // Gather properties from --term and any --term-file inputs.
    let mut raw: Vec<String> = args.term.clone();
    for path in &args.term_file {
        raw.extend(read_terms(path)?);
    }
    let props: HashSet<String> = raw.iter().map(|t| select::expand(&model, t)).collect();

    let mut model = materialize_with(model, &props, kind, args.create_new_ontology.unwrap_or(false))?;
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Check `model` with `kind`, then, unless `create_new_ontology`, assert the
/// direct superclass expressions `kind` infers over `props` (all the
/// ontology's object properties when empty). An inconsistent ontology fails,
/// as does one with an unsatisfiable class or object property. With
/// `create_new_ontology` the materialized axioms belong in an ontology of
/// their own, which is not written: the input is returned as it was, once
/// checked.
pub fn materialize_with(
    model: crate::model::Model,
    props: &HashSet<String>,
    kind: ReasonerKind,
    create_new_ontology: bool,
) -> Result<crate::model::Model> {
    let el = matches!(kind, ReasonerKind::Elk | ReasonerKind::Owlmake);
    crate::reason::el::set_whelk_mode(kind == ReasonerKind::Owlmake);
    if kind == ReasonerKind::Owlmake {
        status!("materialize: using the built-in EL reasoner with union-elimination");
    }
    // The EL reasoner's check rides the classification that materializes.
    if create_new_ontology || !el {
        crate::cmd::reason::validate_model(&model, kind, true)?;
    }
    if create_new_ontology {
        return Ok(model);
    }
    assert_existentials(model, props, kind)
}

/// Read property terms from a file: one IRI/CURIE per line, read the same way
/// every other `--term-file` is (see [`crate::cmd::select::term_line`]).
fn read_terms(path: &std::path::Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading term file {}", path.display()))?;
    Ok(text.lines().filter_map(crate::cmd::select::term_line).map(str::to_string).collect())
}

/// Assert inferred existential restrictions over `props` (all when empty),
/// once the ontology is checked with `elk` (see [`materialize_with`]).
pub fn materialize(model: crate::model::Model, props: &HashSet<String>) -> Result<crate::model::Model> {
    materialize_with(model, props, ReasonerKind::Elk, false)
}

/// Assert the direct superclass expressions `kind` infers over `props` (all
/// the ontology's object properties when empty). Under an EL reasoner the
/// classification that finds them first checks the ontology as `elk` wrapped
/// for materialization does; any other has checked it already.
fn assert_existentials(
    mut model: crate::model::Model,
    props: &HashSet<String>,
    kind: ReasonerKind,
) -> Result<crate::model::Model> {
    // Directness is a question the CLASS HIERARCHY answers, so it is computed
    // in a synthetic space: one named class per (property, filler) pair,
    // equivalent to the restriction it stands for, classified together with the
    // ontology and its imports. A restriction with anything between it and the
    // class — another materialized property's restriction included, or a named
    // class — is not direct, and only direct superclasses are asserted. The
    // named direct subsumptions come from the same augmented hierarchy.
    //
    // The fillers are the classes the ontology's own axioms name, and with no
    // property given the properties are the object properties they name. A
    // class gains superclasses only where the ontology's own axioms define it:
    // a `SubClassOf` it is the subclass of, or an equivalence, disjointness or
    // disjoint union it is a member of.
    const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
    const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
    let mut classes: std::collections::BTreeSet<String> = Default::default();
    let mut all_props: std::collections::BTreeSet<String> = Default::default();
    let mut defined: HashSet<String> = HashSet::new();
    // A named superclass is a class of the ontology or its imports: a nominal
    // the reasoner classifies as a class of its own is none.
    let mut known: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        let own = !model.imported_components.contains(ac);
        for (k, iri) in crate::sig::typed_signature(&ac.component) {
            if k == crate::sig::kind::CLASS {
                if own {
                    classes.insert(iri.clone());
                }
                known.insert(iri);
            } else if k == crate::sig::kind::OBJECT_PROPERTY && own {
                all_props.insert(iri);
            }
        }
        if own {
            defined.extend(defined_classes(&ac.component));
        }
    }
    classes.remove(OWL_THING);
    classes.remove(OWL_NOTHING);
    let prop_list: Vec<String> = if props.is_empty() {
        all_props.into_iter().collect()
    } else {
        let mut v: Vec<String> = props.iter().cloned().collect();
        v.sort();
        v
    };
    let Existentials { mut relations, mut named_subs, unsat, consistent } =
        existential_relations(&model, &prop_list, &classes, kind)?;
    relations.retain(|(c, _, _)| defined.contains(c));
    named_subs.retain(|(c, d)| defined.contains(c) && known.contains(d));
    if kind.is_builtin_el() {
        let listed = crate::cmd::reason::unsatisfiable_in_node_order(ReasonerKind::Elk, &unsat, || {
            crate::reason::elk_order::class_queue(&model.ont)
        });
        let told = crate::cmd::reason::told_unsatisfiable_properties(&model);
        crate::cmd::reason::validate(&crate::cmd::reason::Validation {
            consistent,
            unsatisfiable: &listed,
            unsatisfiable_properties: &told,
            dump: None,
            allow_incoherent: None,
        })?;
    }

    // Restrictions the ontology asserts WITH axiom annotations: the reification
    // points at a labeled node, and a materialized twin at the same owner
    // shares that node instead of joining the cross-owner group.
    let annotated_asserted: std::collections::HashSet<(String, String, String)> = model
        .ont
        .iter()
        .filter(|ac| !ac.ann.is_empty())
        .filter_map(|ac| {
            let Component::SubClassOf(SubClassOf {
                sub: CE::Class(c),
                sup: CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(p), bce },
            }) = &ac.component
            else {
                return None;
            };
            let CE::Class(d) = bce.as_ref() else { return None };
            Some((c.0.to_string(), p.0.to_string(), d.0.to_string()))
        })
        .collect();

    // One restriction OBJECT per (property, filler), however many classes get it:
    // every newly-asserted `C ⊑ ∃R.D` with the same (R, D) is the same object, so
    // it takes ONE blank node across all of them. Each owner references it once,
    // so it renders inline at each — `span_shared` is the carrier for that, and
    // only the numbering moves. An ANNOTATED assertion is different: its
    // reification has to point at a labeled node, so the group renders as
    // `rdf:nodeID` and belongs in `cross_shared`. An axiom that was ALREADY
    // asserted keeps the node it came with and joins no group.
    let mut groups: std::collections::HashMap<(String, String), u64> =
        std::collections::HashMap::new();
    let mut next_group: u64 = model
        .cross_shared
        .values()
        .chain(model.span_shared.values())
        .copied()
        .max()
        .map_or(0, |m| m + 1);

    let mut added = 0usize;
    for (c, r, d) in relations {
        // Reflexive self-edges `C ⊑ ∃R.C` (e.g. a part_of self-loop falling out of
        // a cycle) are not asserted: they restate a class in terms of itself and
        // tell a consumer nothing.
        if c == d {
            continue;
        }
        let (c, r, d) = (c.clone(), r.clone(), d.clone());
        let comp = Component::SubClassOf(SubClassOf {
            sub: CE::Class(model.build.class(c.clone())),
            sup: CE::ObjectSomeValuesFrom {
                ope: OPE::ObjectProperty(model.build.object_property(r.clone())),
                bce: Box::new(CE::Class(model.build.class(d.clone()))),
            },
        });
        if model.ont.insert(comp) {
            added += 1;
            let g = *groups.entry((r.clone(), d.clone())).or_insert_with(|| {
                let v = next_group;
                next_group += 1;
                v
            });
            if annotated_asserted.contains(&(c.clone(), r.clone(), d.clone())) {
                // A reification points at a labeled node, so the minted object
                // renders as ONE `rdf:nodeID` across every class that received
                // it: an annotated sibling of this class's new twin adopts the
                // node. A class whose annotated axiom predates this step (its
                // twin was NOT inserted here) keeps its own node — the sibling
                // share there stays per-class.
                model.cross_shared.insert(format!("{c}\u{1}{r}\u{1}{d}"), g);
            } else {
                let sig = crate::io::genid::ce_sig(&CE::ObjectSomeValuesFrom {
                    ope: OPE::ObjectProperty(model.build.object_property(r.clone())),
                    bce: Box::new(CE::Class(model.build.class(d.clone()))),
                });
                model.span_shared.insert(format!("{c}\u{1}{sig}"), g);
            }
        }
    }

    // Materialization asserts every DIRECT superclass *expression* of a class,
    // which covers named direct superclasses as well as the existentials above.
    // Post-relax, some named direct subsumptions become newly derivable (e.g. a
    // genus edge from a relaxed cardinality definition) that the pre-relax
    // `reason` step could not assert, so add the direct named subsumptions that
    // are not already present.
    let mut named_added = 0usize;
    for (sub, sup) in named_subs {
        let comp = Component::SubClassOf(SubClassOf {
            sub: CE::Class(model.build.class(sub)),
            sup: CE::Class(model.build.class(sup)),
        });
        if model.ont.insert(comp) {
            named_added += 1;
        }
    }
    status!("materialize: asserted {added} existential restriction(s), {named_added} named subsumption(s)");
    Ok(model)
}

/// What the classification behind materialize finds (see
/// [`existential_relations`]).
pub(crate) struct Existentials {
    /// The direct existential superclasses, as `(class, property, filler)`.
    pub relations: Vec<(String, String, String)>,
    /// The direct named subsumptions.
    pub named_subs: Vec<(String, String)>,
    /// The named classes of the ontology that are unsatisfiable.
    pub unsat: Vec<String>,
    pub consistent: bool,
}

/// The DIRECT existential superclasses `kind` infers for every named class
/// over `props`, as `(class, property, filler)`, with the direct named
/// subsumptions, the named classes of the ontology that are unsatisfiable, and
/// whether it is consistent. Only an EL reasoner reports the last two here;
/// any other has checked the ontology before.
///
/// Directness is a question the CLASS HIERARCHY answers, so it is computed in
/// a synthetic space: one named class per (property, filler) pair,
/// equivalent to the restriction it stands for, classified together with the
/// ontology. A restriction with anything between it and the class — another
/// materialized property's restriction included, or a named class — is not
/// direct. Every class of a direct superclass's node counts, so a restriction
/// equivalent to a named direct superclass is direct too — except under
/// `whelk`, which takes one class of each node (see
/// [`crate::cmd::reason::direct_superclass_nodes`]).
///
/// The class standing for `∃r.c` is `<c>__<r>`, with every `:` and `/` of `r`
/// written `_`: under `whelk` its IRI decides where it falls in the walk that
/// picks a node's class.
pub(crate) fn existential_relations(
    model: &crate::model::Model,
    prop_list: &[String],
    classes: &std::collections::BTreeSet<String>,
    kind: ReasonerKind,
) -> Result<Existentials> {
        let mut aux = model.clone();
        let mut aux_map: std::collections::HashMap<String, (String, String)> = Default::default();
        for r in prop_list {
            for c in classes {
                let iri = format!("{c}__{}", r.replace([':', '/'], "_"));
                aux.ont.insert(Component::EquivalentClasses(
                    horned_owl::model::EquivalentClasses(vec![
                        CE::Class(aux.build.class(iri.clone())),
                        CE::ObjectSomeValuesFrom {
                            ope: OPE::ObjectProperty(aux.build.object_property(r.clone())),
                            bce: Box::new(CE::Class(aux.build.class(c.clone()))),
                        },
                    ]),
                ));
                aux_map.insert(iri, (r.clone(), c.clone()));
            }
        }
        if !kind.is_builtin_el() {
            let direct = crate::cmd::reason::direct_superclass_nodes(&aux, kind, kind.datatypes(true))?;
            let mut relations: Vec<(String, String, String)> = Vec::new();
            let mut named_subs: Vec<(String, String)> = Vec::new();
            for (sub, sup) in direct {
                if aux_map.contains_key(&sub) {
                    continue;
                }
                match aux_map.get(&sup) {
                    Some((r, d)) => relations.push((sub, r.clone(), d.clone())),
                    None => named_subs.push((sub, sup)),
                }
            }
            relations.sort();
            relations.dedup();
            named_subs.sort();
            named_subs.dedup();
            return Ok(Existentials { relations, named_subs, unsat: Vec::new(), consistent: true });
        }
        let reasoner = Reasoner::classify(&aux);
        // The classes standing for restrictions say nothing of the ontology's
        // own: a restriction on an unsatisfiable filler is unsatisfiable with it.
        let unsat: Vec<String> = reasoner
            .unsatisfiable()
            .into_iter()
            .map(|u| u.to_string())
            .filter(|u| !aux_map.contains_key(u))
            .collect();
        let direct = reasoner.direct_subsumptions();
        let mutual: std::collections::HashSet<(&str, &str)> =
            direct.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let mut relations: Vec<(String, String, String)> = Vec::new();
        let mut named_subs: Vec<(String, String)> = Vec::new();
        for (sub, sup) in &direct {
            if aux_map.contains_key(sub) {
                continue;
            }
            match aux_map.get(sup) {
                Some((r, d)) => {
                    // An equivalent restriction is the class's own node, not a
                    // superclass.
                    if mutual.contains(&(sup.as_str(), sub.as_str())) {
                        continue;
                    }
                    relations.push((sub.clone(), r.clone(), d.clone()));
                }
                None => named_subs.push((sub.clone(), sup.clone())),
            }
        }
        relations.sort();
        relations.dedup();
        named_subs.sort();
        named_subs.dedup();
        Ok(Existentials { relations, named_subs, unsat, consistent: reasoner.is_consistent() })
}

/// The classes a class axiom defines: the subclass of a `SubClassOf` that is
/// a named class, and the named members of an equivalence or a disjointness,
/// or the class a disjoint union defines.
fn defined_classes(c: &Component<horned_owl::model::RcStr>) -> Vec<String> {
    let named = |ces: &[CE<horned_owl::model::RcStr>]| -> Vec<String> {
        ces.iter()
            .filter_map(|ce| match ce {
                CE::Class(c) => Some(c.0.to_string()),
                _ => None,
            })
            .collect()
    };
    match c {
        Component::SubClassOf(SubClassOf { sub: CE::Class(c), .. }) => vec![c.0.to_string()],
        Component::EquivalentClasses(eq) => named(&eq.0),
        Component::DisjointClasses(dc) => named(&dc.0),
        Component::DisjointUnion(du) => vec![du.0.0.to_string()],
        _ => Vec::new(),
    }
}
