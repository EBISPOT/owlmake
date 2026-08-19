//! `extract-upheno-relations` — the phenotype-relation augmentation uPheno runs
//! over its merged mirror.
//!
//! uPheno's `mirror/merged.owl` recipe reads every phenotype ontology's EQ
//! definitions and materialises two shortcut relations from them, so that a
//! downstream consumer can ask "what does this phenotype affect?" without
//! unfolding the definition:
//!
//! * `UPHENO:0000001` (has phenotype affecting) — `C ⊑ has_phenotype_affecting
//!   some <bearer filler>`;
//! * `UPHENO:0000003` (has associated entity) — `C ⊑ has_associated_entity some
//!   <named class in the bearer>`, one axiom per named class the bearer mentions.
//!
//! Both are read off an equivalence axiom of the shape OBO phenotype patterns
//! use — `C ≡ has_part some (Q and (inheres_in some B))` — where the bearer's
//! property is `inheres_in` (RO:0000052) or `inheres_in_part_of` (RO:0002314).
//!
//! `UPHENO:0000002` (has phenotypic analogue) is an ANNOTATION assertion between
//! classes an EL classification finds equivalent. uPheno's own build never asks
//! for it — its `--relation` flags are `0000003` and `0000001` — but it is the
//! third relation this command accepts, so asking for it materialises those
//! assertions rather than landing on the unknown-relation report below.
//!
//! ## Which classes are considered
//!
//! `--term`/`--term-file` name classes directly. `--root-phenotype` /
//! `--root-phenotype-file` name a class AND everything under it, where "under"
//! is a TOLD-subclass walk rather than a classification: a class reaches the set
//! through asserted `SubClassOf`/`EquivalentClasses` axioms alone and never
//! through anything inferred. uPheno passes fourteen roots (`UPHENO:0001001`,
//! `MP:0000001`, `HP:0000118`, …), one per species ontology.
//!
//! Nothing is selected when no term and no root is given: the scope set is empty
//! and no axiom is added. "Select everything" is not the default.
//!
//! An unrecognised `--relation` is reported on the status stream and skipped
//! rather than failing the run: every relation this command does know still
//! contributes its axioms, and the unknown name is surfaced rather than passing
//! unnoticed.
//!
//! ## Edge cases, and which way they are settled
//!
//! Each is inert for the inputs uPheno feeds this command; the trigger says what
//! would make it observable.
//!
//! * **Expression order within one axiom.** Where an `EquivalentClasses` axiom
//!   holds two top-level `has_part some (<intersection>)` expressions, the first
//!   written is the one mined and the walk over that axiom then stops. Trigger:
//!   an `EquivalentClasses` axiom with two of them — a definition carries one.
//! * **A root equivalent to its own descendant.** Told equivalence edges are
//!   symmetric, so a root inside a told equivalence cycle keeps every class of
//!   that cycle — itself included — in the scope set. Trigger: one of the roots
//!   inside such a cycle.
//! * **`owl:Nothing` in the descendant set.** It reaches the set only through a
//!   told edge, and even then [`named_classes`] excludes it, so it can never be
//!   the one named class of a definition.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::MutableOntology;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationProperty, AnnotationSubject,
    AnnotationValue, Build, ClassExpression as CE, Component, ObjectPropertyExpression as OPE,
    SubClassOf,
};

use crate::cmd::select;
use crate::model::{Model, Str};

const HAS_PART: &str = "http://purl.obolibrary.org/obo/BFO_0000051";
const INHERES_IN: &str = "http://purl.obolibrary.org/obo/RO_0000052";
const INHERES_IN_PART_OF: &str = "http://purl.obolibrary.org/obo/RO_0002314";
const HAS_PHENOTYPE_AFFECTING: &str = "http://purl.obolibrary.org/obo/UPHENO_0000001";
const HAS_ASSOCIATED_ENTITY: &str = "http://purl.obolibrary.org/obo/UPHENO_0000003";
const HAS_PHENOTYPIC_ANALOGUE: &str = "http://purl.obolibrary.org/obo/UPHENO_0000002";

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

    /// Which relation to augment (repeatable): `UPHENO:0000001`,
    /// `UPHENO:0000003` or `UPHENO:0000002`.
    #[arg(short = 'r', long = "relation")]
    pub relation: Vec<String>,

    /// A class to consider, by IRI or CURIE (repeatable).
    #[arg(short = 't', long = "term")]
    pub term: Vec<String>,

    /// A file of classes to consider, one per line (repeatable).
    #[arg(short = 'T', long = "term-file")]
    pub term_file: Vec<PathBuf>,

    /// A class whose told descendants are all considered (repeatable).
    #[arg(short = 'p', long = "root-phenotype")]
    pub root_phenotype: Vec<String>,

    /// A file of root classes, one per line (repeatable).
    ///
    /// Long-form only on owlmake's own command line: `-P` is `--prefixes` on
    /// every command, and two flags cannot share a short. Ingest still reads `-P`
    /// as this flag when it finds one in a RECIPE (`odk::robot`) — that is
    /// someone else's command line being parsed, and what it means there is
    /// settled by the recipe that wrote it.
    #[arg(long = "root-phenotype-file")]
    pub root_phenotype_file: Vec<PathBuf>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let phenotypes = phenotype_set(
        &model,
        &args.term,
        &args.term_file,
        &args.root_phenotype,
        &args.root_phenotype_file,
    )?;
    augment(&mut model, &args.relation, &phenotypes);
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Augment `model` in place, resolving term/root files relative to `dir`.
///
/// The pipeline entry point: uPheno chains this between `merge` and `remove`, so
/// the op runs against the threaded model rather than loading its own input.
pub fn apply(
    model: &mut Model,
    dir: &std::path::Path,
    relations: &[String],
    terms: &[String],
    term_files: &[String],
    roots: &[String],
    root_files: &[String],
) -> Result<()> {
    let at = |f: &String| dir.join(f);
    let term_files: Vec<PathBuf> = term_files.iter().map(at).collect();
    let root_files: Vec<PathBuf> = root_files.iter().map(at).collect();
    let phenotypes = phenotype_set(model, terms, &term_files, roots, &root_files)?;
    augment(model, relations, &phenotypes);
    Ok(())
}

/// The classes the augmentation considers: the named terms, plus every root and
/// its told descendants.
fn phenotype_set(
    model: &Model,
    term: &[String],
    term_file: &[PathBuf],
    root: &[String],
    root_file: &[PathBuf],
) -> Result<HashSet<String>> {
    let mut out: HashSet<String> = HashSet::new();
    out.extend(select::collect_terms(model, term, term_file)?);

    let roots = select::collect_terms(model, root, root_file)?;
    if roots.is_empty() {
        return Ok(out);
    }
    let children = told_children(model);
    let mut stack: Vec<String> = Vec::new();
    for r in &roots {
        out.insert(r.clone());
        stack.push(r.clone());
    }
    // The walk recurses through every told child; a `seen` set is what keeps a
    // cyclic told hierarchy — one where a class is its own told descendant —
    // from spinning here.
    let mut seen: HashSet<String> = roots.iter().cloned().collect();
    while let Some(c) = stack.pop() {
        let Some(kids) = children.get(&c) else { continue };
        for k in kids {
            if seen.insert(k.clone()) {
                out.insert(k.clone());
                stack.push(k.clone());
            }
        }
    }
    Ok(out)
}

/// The told parent → children map the `--root-phenotype` walk descends.
///
/// A named `C` is a child of `P` when some `SubClassOf(C, S)` has `P` among the
/// CONJUNCTS of `S` — so `C ⊑ (P and ∃r.X)` counts, exactly as a bare `C ⊑ P`
/// does — or when an `EquivalentClasses` axiom holds both a named `C` and a
/// different expression having `P` as a conjunct.
fn told_children(model: &Model) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut add = |parent: &str, child: &str| {
        out.entry(parent.to_string()).or_default().push(child.to_string());
    };
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    let mut ps = Vec::new();
                    conjuncts(&sc.sup, &mut ps);
                    for p in ps {
                        add(&p, sub.0.as_ref());
                    }
                }
            }
            Component::EquivalentClasses(eq) => {
                for ce in eq.0.iter() {
                    let mut ps = Vec::new();
                    conjuncts(ce, &mut ps);
                    for p in ps {
                        for other in eq.0.iter() {
                            if std::ptr::eq(other, ce) {
                                continue;
                            }
                            if let CE::Class(c) = other {
                                add(&p, c.0.as_ref());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The NAMED classes among a class expression's conjuncts. A bare class is its
/// own single conjunct; an intersection contributes each named conjunct.
///
/// RECURSIVE: an intersection's conjunct set is the union of its operands'
/// conjunct sets, so `C ⊑ (P and (Q and ∃r.X))` makes C a told child of Q as well
/// as of P. Reading only the top level loses every class whose superclass nests
/// its intersections that way.
fn conjuncts(ce: &CE<Str>, out: &mut Vec<String>) {
    match ce {
        CE::Class(c) => out.push(c.0.as_ref().to_string()),
        CE::ObjectIntersectionOf(ops) => {
            for o in ops {
                conjuncts(o, out);
            }
        }
        _ => {}
    }
}

/// Add the axioms each requested relation contributes.
pub fn augment(model: &mut Model, relations: &[String], phenotypes: &HashSet<String>) {
    let mut new: Vec<AnnotatedComponent<Str>> = Vec::new();
    for relation in relations {
        match crate::io::obo::expand_id(relation).as_str() {
            HAS_PHENOTYPE_AFFECTING => {
                new.extend(from_eq_definitions(model, phenotypes, Shortcut::Affecting))
            }
            HAS_ASSOCIATED_ENTITY => {
                new.extend(from_eq_definitions(model, phenotypes, Shortcut::Associated))
            }
            HAS_PHENOTYPIC_ANALOGUE => new.extend(phenotypic_analogues(model, phenotypes)),
            other => {
                crate::status!("extract-upheno-relations: unknown relation: {other}");
            }
        }
    }
    for ax in new {
        model.ont.insert(ax);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Shortcut {
    /// `UPHENO:0000001`, over the bearer's FILLER.
    Affecting,
    /// `UPHENO:0000003`, over each named class the bearer mentions.
    Associated,
}

/// Read the shortcut axioms off the EQ definitions.
fn from_eq_definitions(
    model: &Model,
    phenotypes: &HashSet<String>,
    which: Shortcut,
) -> Vec<AnnotatedComponent<Str>> {
    let b = Build::new();
    let mut out: Vec<AnnotatedComponent<Str>> = Vec::new();
    for ac in model.ont.iter() {
        let Component::EquivalentClasses(eq) = &ac.component else { continue };
        // Exactly ONE named class in the axiom, and it has to be in scope: an
        // `A ≡ B` between two named phenotypes defines neither of them.
        let named = named_classes(&eq.0);
        if named.len() != 1 || !phenotypes.contains(named[0]) {
            continue;
        }
        let subject = named[0];
        for ce in eq.0.iter() {
            let CE::ObjectSomeValuesFrom { ope, bce } = ce else { continue };
            if !is_property(ope, HAS_PART) {
                continue;
            }
            // Only an INTERSECTION filler is a phenotype definition. Anything
            // else contributes nothing AND does not end the search: the `break`
            // below is reached only once an intersection filler has been mined,
            // so a `has_part` whose filler is something else leaves a later
            // expression free to match.
            let CE::ObjectIntersectionOf(ops) = bce.as_ref() else { continue };
            for operand in ops {
                let CE::ObjectSomeValuesFrom { ope: bearer_ope, bce: bearer } = operand else {
                    continue;
                };
                if !is_property(bearer_ope, INHERES_IN)
                    && !is_property(bearer_ope, INHERES_IN_PART_OF)
                {
                    continue;
                }
                match which {
                    Shortcut::Affecting => {
                        // `owl:Thing`/`owl:Nothing` bearers say nothing about
                        // what the phenotype affects.
                        if is_named(bearer, OWL_THING) || is_named(bearer, OWL_NOTHING) {
                            continue;
                        }
                        out.push(
                            Component::SubClassOf(SubClassOf {
                                sub: CE::Class(b.class(subject)),
                                sup: CE::ObjectSomeValuesFrom {
                                    ope: OPE::ObjectProperty(
                                        b.object_property(HAS_PHENOTYPE_AFFECTING),
                                    ),
                                    bce: bearer.clone(),
                                },
                            })
                            .into(),
                        );
                    }
                    Shortcut::Associated => {
                        // Every named class the bearer EXPRESSION mentions, so a
                        // nested bearer contributes one axiom per class in it.
                        let mut names: Vec<String> = Vec::new();
                        classes_in_signature(operand, &mut names);
                        for name in names {
                            out.push(
                                Component::SubClassOf(SubClassOf {
                                    sub: CE::Class(b.class(subject)),
                                    sup: CE::ObjectSomeValuesFrom {
                                        ope: OPE::ObjectProperty(
                                            b.object_property(HAS_ASSOCIATED_ENTITY),
                                        ),
                                        bce: Box::new(CE::Class(b.class(name.as_str()))),
                                    },
                                })
                                .into(),
                            );
                        }
                    }
                }
            }
            // One `has_part some (…)` per definition: the first intersection
            // filler ends the walk over this axiom's expressions.
            break;
        }
    }
    out
}

/// `UPHENO:0000002`: an annotation assertion between each in-scope phenotype and
/// every OTHER class a classification finds equivalent to it. An unsatisfiable
/// class is equivalent to everything unsatisfiable, so those are skipped.
fn phenotypic_analogues(
    model: &Model,
    phenotypes: &HashSet<String>,
) -> Vec<AnnotatedComponent<Str>> {
    let b = Build::new();
    let reasoner = crate::reason::el::Reasoner::classify(model);
    let unsat: HashSet<String> = reasoner.unsatisfiable().into_iter().collect();
    let mut equivalents: HashMap<String, Vec<String>> = HashMap::new();
    for (a, c) in reasoner.equivalent_class_pairs() {
        equivalents.entry(a.clone()).or_default().push(c.clone());
        equivalents.entry(c).or_default().push(a);
    }
    let mut out: Vec<AnnotatedComponent<Str>> = Vec::new();
    for phenotype in phenotypes {
        if unsat.contains(phenotype) {
            continue;
        }
        for equiv in equivalents.get(phenotype).into_iter().flatten() {
            if equiv == phenotype || unsat.contains(equiv) {
                continue;
            }
            let equiv = equiv.clone();
            out.push(
                Component::AnnotationAssertion(AnnotationAssertion {
                    subject: AnnotationSubject::IRI(b.iri(phenotype.as_str())),
                    ann: Annotation {
                        ann: Default::default(),
                        ap: AnnotationProperty(b.iri(HAS_PHENOTYPIC_ANALOGUE)),
                        av: AnnotationValue::IRI(b.iri(equiv.as_str())),
                    },
                })
                .into(),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::EquivalentClasses;

    const ROOT: &str = "http://purl.obolibrary.org/obo/UPHENO_0001001";
    const PHENO: &str = "http://purl.obolibrary.org/obo/TEST_0000001";
    const ORPHAN: &str = "http://purl.obolibrary.org/obo/TEST_0000002";
    const QUALITY: &str = "http://purl.obolibrary.org/obo/PATO_0000001";
    const BRAIN: &str = "http://purl.obolibrary.org/obo/UBERON_0000955";

    /// `subject ≡ has_part some (PATO:0000001 and (<bearer_prop> some <bearer>))`,
    /// with `subject ⊑ ROOT` so a root walk reaches it.
    fn eq_model(subject: &str, bearer_prop: &str, bearer: CE<Str>) -> Model {
        let b = Build::new();
        let mut model = Model::new();
        model.ont.insert::<AnnotatedComponent<Str>>(
            Component::SubClassOf(SubClassOf {
                sub: CE::Class(b.class(subject)),
                sup: CE::Class(b.class(ROOT)),
            })
            .into(),
        );
        let definition = CE::ObjectSomeValuesFrom {
            ope: OPE::ObjectProperty(b.object_property(HAS_PART)),
            bce: Box::new(CE::ObjectIntersectionOf(vec![
                CE::Class(b.class(QUALITY)),
                CE::ObjectSomeValuesFrom {
                    ope: OPE::ObjectProperty(b.object_property(bearer_prop)),
                    bce: Box::new(bearer),
                },
            ])),
        };
        model.ont.insert::<AnnotatedComponent<Str>>(
            Component::EquivalentClasses(EquivalentClasses(vec![
                CE::Class(b.class(subject)),
                definition,
            ]))
            .into(),
        );
        model
    }

    /// The `(subject, property, filler)` of every `SubClassOf(C, p some F)` whose
    /// property is one of the two shortcuts.
    fn shortcuts(model: &Model) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for ac in model.ont.iter() {
            let Component::SubClassOf(sc) = &ac.component else { continue };
            let (CE::Class(sub), CE::ObjectSomeValuesFrom { ope, bce }) = (&sc.sub, &sc.sup) else {
                continue;
            };
            let OPE::ObjectProperty(p) = ope else { continue };
            let p = p.0.as_ref();
            if p != HAS_PHENOTYPE_AFFECTING && p != HAS_ASSOCIATED_ENTITY {
                continue;
            }
            if let CE::Class(f) = bce.as_ref() {
                out.push((sub.0.as_ref().to_string(), p.to_string(), f.0.as_ref().to_string()));
            }
        }
        out.sort();
        out
    }

    fn roots() -> HashSet<String> {
        [ROOT.to_string(), PHENO.to_string()].into_iter().collect()
    }

    #[test]
    fn an_eq_definition_yields_both_shortcut_relations() {
        let b = Build::new();
        let mut model = eq_model(PHENO, INHERES_IN, CE::Class(b.class(BRAIN)));
        augment(
            &mut model,
            &["UPHENO:0000003".into(), "UPHENO:0000001".into()],
            &roots(),
        );
        assert_eq!(
            shortcuts(&model),
            vec![
                (PHENO.to_string(), HAS_PHENOTYPE_AFFECTING.to_string(), BRAIN.to_string()),
                (PHENO.to_string(), HAS_ASSOCIATED_ENTITY.to_string(), BRAIN.to_string()),
            ]
        );
    }

    #[test]
    fn inheres_in_part_of_is_a_bearer_too() {
        let b = Build::new();
        let mut model = eq_model(PHENO, INHERES_IN_PART_OF, CE::Class(b.class(BRAIN)));
        augment(&mut model, &["UPHENO:0000001".into()], &roots());
        assert_eq!(
            shortcuts(&model),
            vec![(PHENO.to_string(), HAS_PHENOTYPE_AFFECTING.to_string(), BRAIN.to_string())]
        );
    }

    /// An `owl:Thing` bearer says nothing about what is affected, so
    /// `UPHENO:0000001` skips it — but that guard covers only that one relation,
    /// and `UPHENO:0000003` still reports every class the bearer mentions.
    #[test]
    fn an_owl_thing_bearer_is_affecting_nothing_but_still_an_associated_entity() {
        let b = Build::new();
        let mut model = eq_model(PHENO, INHERES_IN, CE::Class(b.class(OWL_THING)));
        augment(
            &mut model,
            &["UPHENO:0000001".into(), "UPHENO:0000003".into()],
            &roots(),
        );
        assert_eq!(
            shortcuts(&model),
            vec![(PHENO.to_string(), HAS_ASSOCIATED_ENTITY.to_string(), OWL_THING.to_string())]
        );
    }

    /// A definition on a class the root walk never reached contributes nothing.
    #[test]
    fn a_class_outside_the_phenotype_set_is_left_alone() {
        let b = Build::new();
        let mut model = eq_model(ORPHAN, INHERES_IN, CE::Class(b.class(BRAIN)));
        augment(
            &mut model,
            &["UPHENO:0000001".into(), "UPHENO:0000003".into()],
            &roots(),
        );
        assert!(shortcuts(&model).is_empty());
    }

    /// The conjunct walk recurses, so a NESTED intersection's named operands are
    /// conjuncts too and the class reaches the root through them.
    #[test]
    fn told_descendants_reach_through_a_nested_intersection() {
        let b = Build::new();
        let mut model = Model::new();
        model.ont.insert::<AnnotatedComponent<Str>>(
            Component::SubClassOf(SubClassOf {
                sub: CE::Class(b.class(PHENO)),
                sup: CE::ObjectIntersectionOf(vec![
                    CE::Class(b.class(QUALITY)),
                    CE::ObjectIntersectionOf(vec![
                        CE::Class(b.class(ROOT)),
                        CE::ObjectSomeValuesFrom {
                            ope: OPE::ObjectProperty(b.object_property(INHERES_IN)),
                            bce: Box::new(CE::Class(b.class(BRAIN))),
                        },
                    ]),
                ]),
            })
            .into(),
        );
        let found = phenotype_set(&model, &[], &[], &[ROOT.to_string()], &[]).unwrap();
        assert!(found.contains(PHENO), "{found:?}");
    }

    /// `named_classes` skips `owl:Thing`, so an axiom naming it alongside one
    /// real class still counts as that class's definition.
    #[test]
    fn an_owl_thing_operand_does_not_disqualify_a_definition() {
        let b = Build::new();
        let mut model = eq_model(PHENO, INHERES_IN, CE::Class(b.class(BRAIN)));
        // Re-state the definition with `owl:Thing` as an extra operand.
        let definition = CE::ObjectSomeValuesFrom {
            ope: OPE::ObjectProperty(b.object_property(HAS_PART)),
            bce: Box::new(CE::ObjectIntersectionOf(vec![
                CE::Class(b.class(QUALITY)),
                CE::ObjectSomeValuesFrom {
                    ope: OPE::ObjectProperty(b.object_property(INHERES_IN)),
                    bce: Box::new(CE::Class(b.class(BRAIN))),
                },
            ])),
        };
        let mut m2 = Model::new();
        m2.ont.insert::<AnnotatedComponent<Str>>(
            Component::SubClassOf(SubClassOf {
                sub: CE::Class(b.class(PHENO)),
                sup: CE::Class(b.class(ROOT)),
            })
            .into(),
        );
        m2.ont.insert::<AnnotatedComponent<Str>>(
            Component::EquivalentClasses(EquivalentClasses(vec![
                CE::Class(b.class(PHENO)),
                CE::Class(b.class(OWL_THING)),
                definition,
            ]))
            .into(),
        );
        model = m2;
        augment(&mut model, &["UPHENO:0000001".into()], &roots());
        assert_eq!(
            shortcuts(&model),
            vec![(PHENO.to_string(), HAS_PHENOTYPE_AFFECTING.to_string(), BRAIN.to_string())]
        );
    }

    /// `--root-phenotype` reaches a class through told `SubClassOf` axioms, and
    /// through a conjunct of an intersection superclass just the same.
    #[test]
    fn told_descendants_include_intersection_conjuncts() {
        let b = Build::new();
        let mut model = Model::new();
        model.ont.insert::<AnnotatedComponent<Str>>(
            Component::SubClassOf(SubClassOf {
                sub: CE::Class(b.class(PHENO)),
                sup: CE::ObjectIntersectionOf(vec![
                    CE::Class(b.class(ROOT)),
                    CE::ObjectSomeValuesFrom {
                        ope: OPE::ObjectProperty(b.object_property(INHERES_IN)),
                        bce: Box::new(CE::Class(b.class(BRAIN))),
                    },
                ]),
            })
            .into(),
        );
        let found = phenotype_set(&model, &[], &[], &[ROOT.to_string()], &[]).unwrap();
        assert!(found.contains(PHENO), "{found:?}");
    }
}

/// The named classes an `EquivalentClasses` axiom holds, deduped.
///
/// `owl:Thing` and `owl:Nothing` are NOT among them: the two builtins are
/// skipped alongside the anonymous expressions. That is what decides whether an
/// axiom counts as a definition — `C ≡ owl:Thing ⊓ …`
/// spelled as `EquivalentClasses(C, owl:Thing, has_part some (…))` holds ONE
/// named class and is mined, where counting the builtin would make it two and
/// skip it.
fn named_classes(exprs: &[CE<Str>]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for ce in exprs {
        let CE::Class(c) = ce else { continue };
        let iri = c.0.as_ref();
        if iri == OWL_THING || iri == OWL_NOTHING || out.contains(&iri) {
            continue;
        }
        out.push(iri);
    }
    out
}

fn is_property(ope: &OPE<Str>, iri: &str) -> bool {
    matches!(ope, OPE::ObjectProperty(p) if p.0.as_ref() == iri)
}

fn is_named(ce: &CE<Str>, iri: &str) -> bool {
    matches!(ce, CE::Class(c) if c.0.as_ref() == iri)
}

/// Every named class inside a class expression, in the order encountered.
fn classes_in_signature(ce: &CE<Str>, out: &mut Vec<String>) {
    match ce {
        CE::Class(c) => {
            let iri = c.0.as_ref().to_string();
            if !out.contains(&iri) {
                out.push(iri);
            }
        }
        CE::ObjectIntersectionOf(ops) | CE::ObjectUnionOf(ops) => {
            for o in ops {
                classes_in_signature(o, out);
            }
        }
        CE::ObjectComplementOf(inner) => classes_in_signature(inner, out),
        CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => {
            classes_in_signature(bce, out)
        }
        CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectMaxCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. } => classes_in_signature(bce, out),
        _ => {}
    }
}
