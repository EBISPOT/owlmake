//! `materialize` — assert the existential restrictions a class is entailed to
//! hold, so `C ⊑ ∃R.D` becomes an asserted axiom and a consumer that does no
//! reasoning still sees the relation.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    Annotation, AnnotatedComponent, AnnotationValue, ClassExpression as CE, Component, Literal,
    MutableOntology, ObjectPropertyExpression as OPE, SubClassOf,
};

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
    /// Reasoner to use. Materializing existential restrictions is an EL
    /// operation; `elk`/`structural`/`emr`/`owlmake` all use the built-in EL
    /// reasoner.
    #[arg(short = 'r', long, default_value = "elk")]
    pub reasoner: String,
    /// Annotate asserted inferred axioms with `is_inferred true`.
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true")]
    pub annotate_inferred_axioms: Option<bool>,
    /// Output a new ontology containing only the materialized axioms.
    /// `<bool>`.
    #[arg(short = 'n', long, num_args = 1, default_missing_value = "true")]
    pub create_new_ontology: Option<bool>,
    /// After materializing, remove redundant SubClassOf axioms by running
    /// reduce. `<bool>`.
    #[arg(short = 's', long, num_args = 1, default_missing_value = "true")]
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
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // Apply `--reasoner`: `owlmake` enables union-elimination, every other name
    // runs on the EL engine (with a note for non-EL choices). Set before
    // classification.
    crate::reason::configure(&args.reasoner);

    // Gather properties from --term and any --term-file inputs.
    let mut raw: Vec<String> = args.term.clone();
    for path in &args.term_file {
        raw.extend(read_terms(path)?);
    }
    let props: HashSet<String> = raw.iter().map(|t| select::expand(&model, t)).collect();

    let annotate = args.annotate_inferred_axioms.unwrap_or(false);
    let mut model = materialize_with_opts(
        model,
        &props,
        annotate,
        args.create_new_ontology.unwrap_or(false),
    );
    // Reduce runs after materialization, so redundancy is judged against the
    // newly asserted axioms as well.
    if args.remove_redundant_subclass_axioms.unwrap_or(false) {
        model = crate::cmd::reduce::reduce(&model);
    }
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Read property terms from a file: one IRI/CURIE per line, read the same way
/// every other `--term-file` is (see [`crate::cmd::select::term_line`]).
fn read_terms(path: &std::path::Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading term file {}", path.display()))?;
    Ok(text.lines().filter_map(crate::cmd::select::term_line).map(str::to_string).collect())
}

/// Assert inferred existential restrictions over `props` (all when empty).
/// Entry point for callers that want no inferred-axiom annotation.
pub fn materialize(model: crate::model::Model, props: &HashSet<String>) -> crate::model::Model {
    materialize_with(model, props, false)
}

/// Assert inferred existential restrictions over `props` (all when empty),
/// optionally annotating each asserted axiom with `is_inferred true`.
pub fn materialize_with(
    model: crate::model::Model,
    props: &HashSet<String>,
    annotate: bool,
) -> crate::model::Model {
    materialize_with_opts(model, props, annotate, false)
}

/// Like [`materialize_with`], but when `create_new_ontology` is true the result
/// contains only the materialized axioms (carrying the input's prefixes), so the
/// output is the inference delta rather than the whole ontology.
pub fn materialize_with_opts(
    mut model: crate::model::Model,
    props: &HashSet<String>,
    annotate: bool,
    create_new_ontology: bool,
) -> crate::model::Model {
    let reasoner = Reasoner::classify(&model);
    let relations = reasoner.materialize(props);

    let infer_prop = model
        .build
        .annotation_property("http://www.geneontology.org/formats/oboInOwl#is_inferred");

    // When building a fresh ontology, insert into an empty model carrying prefixes.
    if create_new_ontology {
        model = crate::model::Model::from_parts(
            horned_owl::ontology::set::SetOntology::new(),
            crate::model::clone_prefixes(&model.prefixes),
        );
    }

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
        let inserted = if annotate {
            let ann = Annotation { ann: Default::default(),
                ap: infer_prop.clone(),
                av: AnnotationValue::Literal(Literal::Simple {
                    literal: "true".to_string(),
                }),
            };
            let mut anns = std::collections::BTreeSet::new();
            anns.insert(ann);
            model.ont.insert(AnnotatedComponent {
                component: comp,
                ann: anns,
            })
        } else {
            model.ont.insert(comp)
        };
        if inserted {
            added += 1;
            let g = *groups.entry((r.clone(), d.clone())).or_insert_with(|| {
                let v = next_group;
                next_group += 1;
                v
            });
            if annotate {
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
    for (sub, sup) in reasoner.direct_subsumptions() {
        let comp = Component::SubClassOf(SubClassOf {
            sub: CE::Class(model.build.class(sub)),
            sup: CE::Class(model.build.class(sup)),
        });
        let inserted = if annotate {
            let ann = Annotation { ann: Default::default(),
                ap: infer_prop.clone(),
                av: AnnotationValue::Literal(Literal::Simple { literal: "true".to_string() }),
            };
            let mut anns = std::collections::BTreeSet::new();
            anns.insert(ann);
            model.ont.insert(AnnotatedComponent { component: comp, ann: anns })
        } else {
            model.ont.insert(comp)
        };
        if inserted {
            named_added += 1;
        }
    }
    status!("materialize: asserted {added} existential restriction(s), {named_added} named subsumption(s)");
    model
}
