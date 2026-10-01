//! `subset` — extract a named slice of an ontology and drop the rest. Two modes:
//!
//! - **inSubset mode** (`--subset NAME`): keep the classes tagged with the named
//!   `oboInOwl:inSubset` subset (used e.g. by UBERON's `common-anatomy.owl`).
//! - **query mode** (`--query EXPR`, repeatable): keep the classes matched by one
//!   or more Manchester DL queries — the (transitive) subclasses and equivalents
//!   of each expression, plus its ancestors with `--ancestors true`, plus the
//!   named query class itself. Used by UBERON's `life-stages-minimal.owl` etc.
//!
//! Either way the complement is removed; with `--fill-gaps` the hierarchy is
//! bridged across dropped classes (owlmake's `remove --preserve-structure true`),
//! otherwise it is a plain signature filter (`--preserve-structure false`).

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{AnnotationSubject, AnnotationValue, Component};

use crate::cmd::select;
use crate::model::Model;

const IN_SUBSET: &str = "http://www.geneontology.org/formats/oboInOwl#inSubset";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub format: Option<String>,
    /// The inSubset slice to extract (by name, e.g. `common_anatomy`). Required
    /// unless `--query`/`--term` seed the subset instead.
    #[arg(short, long)]
    pub subset: Option<String>,
    /// A Manchester DL query whose matches seed the subset (repeatable).
    #[arg(short = 'q', long = "query")]
    pub query: Vec<String>,
    /// Also include the inferred ancestors of each query's matches (`<bool>`).
    #[arg(short = 'a', long = "ancestors", num_args = 1, default_missing_value = "true")]
    pub ancestors: Option<bool>,
    /// Reasoner for `--query`; owlmake answers it with its EL engine.
    #[arg(short = 'r', long = "reasoner")]
    pub reasoner: Option<String>,
    /// Seed term(s) added to the subset directly (CURIE/IRI, repeatable).
    #[arg(short = 't', long = "term")]
    pub term: Vec<String>,
    /// File(s) of seed terms (repeatable).
    #[arg(short = 'T', long = "term-file")]
    pub term_file: Vec<PathBuf>,
    /// Bridge the hierarchy across dropped classes (`<bool>`). Defaults to true in
    /// inSubset mode, false in query mode.
    #[arg(long = "fill-gaps", num_args = 1, default_missing_value = "true")]
    pub fill_gaps: Option<bool>,
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
    let query_mode = !args.query.is_empty() || !args.term.is_empty() || !args.term_file.is_empty();
    let mut filled_gaps = false;
    let mut model = if query_mode {
        query_subset(model, args)?
    } else {
        let name = args
            .subset
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("subset requires --subset NAME (or --query/--term)"))?;
        let fg = args.fill_gaps.unwrap_or(true);
        filled_gaps = fg;
        subset(model, name, fg)?
    };
    // `odk:subset` builds a NEW ontology, so it carries none of the input's
    // ontology header — no ontology IRI, no version IRI, no ontology annotations.
    // Threading the input's through left UBERON's fourteen `*-minimal.owl` claiming
    // to BE `…/obo/uberon.owl`, complete with its versionIRI and its 39
    // `IAO_0000700` term-editor annotations; the reference writes a bare
    // `<Ontology/>`. It also decides the whole document's namespaces: with no
    // ontology IRI the OWL namespace takes the default position and every OWL
    // element is written unprefixed, so this one header difference re-prefixed
    // every tag in all fourteen files.
    //
    // Safe for the other caller: `common-anatomy.owl` is `odk:subset … annotate
    // --ontology-iri …`, and the reference's copy carries exactly what that
    // `annotate` sets and nothing from `uberon.owl`.
    let header: Vec<_> = model
        .ont
        .iter()
        .filter(|ac| {
            matches!(
                &ac.component,
                Component::OntologyID(_) | Component::OntologyAnnotation(_)
            )
        })
        .cloned()
        .collect();
    for ac in header {
        use horned_owl::model::MutableOntology;
        model.ont.remove(&ac);
    }

    // The gap-filling closure already decides exactly which declarations belong
    // (one per included class and property, used or not), so the prune below
    // must not second-guess it.
    if !filled_gaps {
        // The subset is built from CLASSES and the properties its axioms use — no
        // individuals. `merged-partonomy` declares 49 ORCID contributors as named
        // individuals, and carrying them (plus the assertions about them) also kept
        // `terms:description`/`terms:source` alive as orphaned declarations, which
        // is how it surfaced.
        {
            use horned_owl::model::MutableOntology;
            let inds: HashSet<String> = model
                .ont
                .iter()
                .filter_map(|ac| match &ac.component {
                    Component::DeclareNamedIndividual(d) => Some(d.0 .0.as_ref().to_string()),
                    _ => None,
                })
                .collect();
            if !inds.is_empty() {
                let doomed: Vec<_> = model
                    .ont
                    .iter()
                    .filter(|ac| match &ac.component {
                        Component::DeclareNamedIndividual(d) => {
                            inds.contains(d.0 .0.as_ref())
                        }
                        Component::AnnotationAssertion(aa) => match &aa.subject {
                            AnnotationSubject::IRI(sub) => inds.contains(sub.as_ref()),
                            _ => false,
                        },
                        _ => false,
                    })
                    .cloned()
                    .collect();
                for ac in doomed {
                    model.ont.remove(&ac);
                }
            }
        }
        // A property is kept only when the retained CONTENT uses it. Everything
        // about an unused property goes: its declaration, its hierarchy and
        // characteristics, its domains and ranges, its annotations. `uberon.owl`
        // carries 268 object properties; the life-stages subset's class axioms use
        // SIX, and the reference keeps exactly those six — with their whole frames,
        // labels and examples included.
        {
            use horned_owl::model::MutableOntology;
            let props = crate::cmd::extract::property_signature(&model);
            // Properties named by axioms that are not themselves ABOUT a property.
            let mut used: HashSet<String> = HashSet::new();
            for ac in model.ont.iter() {
                let about_prop = match &ac.component {
                    Component::AnnotationAssertion(aa) => match &aa.subject {
                        AnnotationSubject::IRI(sub) => props.contains(sub.as_ref()),
                        _ => false,
                    },
                    Component::DeclareObjectProperty(_)
                    | Component::DeclareDataProperty(_)
                    | Component::DeclareAnnotationProperty(_)
                    | Component::SubObjectPropertyOf(_)
                    | Component::SubDataPropertyOf(_)
                    | Component::SubAnnotationPropertyOf(_)
                    | Component::InverseObjectProperties(_)
                    | Component::TransitiveObjectProperty(_)
                    | Component::ReflexiveObjectProperty(_)
                    | Component::IrreflexiveObjectProperty(_)
                    | Component::SymmetricObjectProperty(_)
                    | Component::AsymmetricObjectProperty(_)
                    | Component::FunctionalObjectProperty(_)
                    | Component::InverseFunctionalObjectProperty(_)
                    | Component::ObjectPropertyDomain(_)
                    | Component::ObjectPropertyRange(_)
                    | Component::DataPropertyDomain(_)
                    | Component::DataPropertyRange(_)
                    | Component::AnnotationPropertyDomain(_)
                    | Component::AnnotationPropertyRange(_) => true,
                    _ => false,
                };
                if about_prop {
                    continue;
                }
                if let Component::AnnotationAssertion(aa) = &ac.component {
                    used.insert(aa.ann.ap.0.as_ref().to_string());
                } else {
                    used.extend(crate::sig::signature(&ac.component));
                }
                for a in &ac.ann {
                    used.insert(a.ap.0.as_ref().to_string());
                }
            }
            // The SUBJECT property of a property axiom decides that axiom's fate.
            let subject_prop = |c: &Component<horned_owl::model::RcStr>| -> Option<String> {
                use horned_owl::model::ObjectPropertyExpression as OPE;
                let ope = |o: &OPE<horned_owl::model::RcStr>| match o {
                    OPE::ObjectProperty(p) => Some(p.0.as_ref().to_string()),
                    OPE::InverseObjectProperty(p) => Some(p.0.as_ref().to_string()),
                };
                match c {
                    Component::DeclareObjectProperty(d) => Some(d.0 .0.as_ref().to_string()),
                    Component::DeclareDataProperty(d) => Some(d.0 .0.as_ref().to_string()),
                    Component::DeclareAnnotationProperty(d) => Some(d.0 .0.as_ref().to_string()),
                    Component::TransitiveObjectProperty(a) => ope(&a.0),
                    Component::ReflexiveObjectProperty(a) => ope(&a.0),
                    Component::IrreflexiveObjectProperty(a) => ope(&a.0),
                    Component::SymmetricObjectProperty(a) => ope(&a.0),
                    Component::AsymmetricObjectProperty(a) => ope(&a.0),
                    Component::FunctionalObjectProperty(a) => ope(&a.0),
                    Component::InverseFunctionalObjectProperty(a) => ope(&a.0),
                    Component::ObjectPropertyDomain(a) => ope(&a.ope),
                    Component::ObjectPropertyRange(a) => ope(&a.ope),
                    Component::InverseObjectProperties(a) => ope(&a.0),
                    Component::SubObjectPropertyOf(a) => match &a.sub {
                        horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(o) => ope(o),
                        horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(v) => {
                            v.first().and_then(ope)
                        }
                    },
                    Component::SubAnnotationPropertyOf(a) => Some(a.sub.0.as_ref().to_string()),
                    Component::AnnotationPropertyDomain(a) => Some(a.ap.0.as_ref().to_string()),
                    Component::AnnotationPropertyRange(a) => Some(a.ap.0.as_ref().to_string()),
                    Component::AnnotationAssertion(aa) => match &aa.subject {
                        AnnotationSubject::IRI(sub) if props.contains(sub.as_ref()) => {
                            Some(sub.as_ref().to_string())
                        }
                        _ => None,
                    },
                    _ => None,
                }
            };
            let doomed: Vec<_> = model
                .ont
                .iter()
                .filter(|ac| {
                    subject_prop(&ac.component)
                        .is_some_and(|p| props.contains(&p) && !used.contains(&p))
                })
                .cloned()
                .collect();
            for ac in doomed {
                model.ont.remove(&ac);
            }
        }
        // A property keeps its own frame only when the retained CONTENT uses it.
        // One level, not a fixpoint: `IAO_0000112` is used on retained classes so
        // its whole frame comes along; `oboInOwl:shorthand` is used only INSIDE
        // that frame, so it is declared bare; `UBPROP_0000100` is used nowhere and
        // disappears entirely (the prune below takes it once its frame is gone).
        {
            use horned_owl::model::MutableOntology;
            let props = crate::cmd::extract::property_signature(&model);
            let is_about_prop = |ac: &horned_owl::model::AnnotatedComponent<
                horned_owl::model::RcStr,
            >| match &ac.component {
                Component::AnnotationAssertion(aa) => match &aa.subject {
                    AnnotationSubject::IRI(s) => props.contains(s.as_ref()),
                    _ => false,
                },
                _ => false,
            };
            let mut used_by_content: HashSet<String> = HashSet::new();
            for ac in model.ont.iter() {
                if is_about_prop(ac) || matches!(&ac.component, Component::DeclareAnnotationProperty(_) | Component::DeclareObjectProperty(_) | Component::DeclareDataProperty(_)) {
                    continue;
                }
                if let Component::AnnotationAssertion(aa) = &ac.component {
                    used_by_content.insert(aa.ann.ap.0.to_string());
                } else {
                    used_by_content.extend(crate::sig::signature(&ac.component));
                }
                for a in &ac.ann {
                    used_by_content.insert(a.ap.0.to_string());
                }
            }
            let doomed: Vec<_> = model
                .ont
                .iter()
                .filter(|ac| {
                    is_about_prop(ac)
                        && match &ac.component {
                            Component::AnnotationAssertion(aa) => match &aa.subject {
                                AnnotationSubject::IRI(s) => {
                                    !used_by_content.contains(s.as_ref())
                                }
                                _ => false,
                            },
                            _ => false,
                        }
                })
                .cloned()
                .collect();
            for ac in doomed {
                model.ont.remove(&ac);
            }
        }
        prune_dangling_declarations(&mut model);
        // The other paths reach the result by removing the complement, which
        // leaves the INPUT document's state attached. The op builds a new
        // ontology, so it takes neither the input's namespace declarations nor
        // the order its document happened to write reification blocks in.
        model.format_prefixes_cleared = true;
        // Nor does it inherit the source's import-closure evidence: that is what
        // decides which signature entities get a bare declaration stub, and the
        // subset's signature is its own.
        model.closure_declared.clear();
        model.closure_ann_ns.clear();
    }

    // The slice is a new ontology built out of whatever the closure offered, so
    // the terms it kept are its own content and it imports nothing.
    model.detach_import_closure();

    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Query-mode subset: seed from `--query` (Manchester DL queries) plus any
/// `--term`/`--term-file`/`--subset`, then remove the complement.
fn query_subset(model: Model, args: &Args) -> Result<Model> {
    let reasoner = args.reasoner.as_deref().unwrap_or("elk");
    let ancestors = args.ancestors.unwrap_or(false);
    let mut seed: HashSet<String> = HashSet::new();

    for q in &args.query {
        // A bare named class (no Manchester operators) is itself a member — the
        // reasoner's Descendants/Equivalent sets never include the query class.
        if !q.trim().contains(char::is_whitespace) {
            seed.insert(select::expand(&model, q));
        }
        for kind in ["descendants", "equivalent"] {
            seed.extend(
                crate::api::dl_query(&model, q, kind, reasoner)
                    .map_err(|e| anyhow::anyhow!("subset --query `{q}`: {e}"))?,
            );
        }
        if ancestors {
            seed.extend(
                crate::api::dl_query(&model, q, "ancestors", reasoner)
                    .map_err(|e| anyhow::anyhow!("subset --query `{q}`: {e}"))?,
            );
        }
    }
    // Additive seeds: explicit terms and (if given) an inSubset slice.
    seed.extend(select::collect_terms(&model, &args.term, &args.term_file)?);
    if let Some(name) = &args.subset {
        seed.extend(inset_seed(&model, name));
    }
    seed.remove(OWL_THING);
    seed.remove(OWL_NOTHING);
    if seed.is_empty() {
        bail!("subset --query matched no classes");
    }

    Ok(fill_gaps_subset(&model, &seed, args.fill_gaps.unwrap_or(false)))
}

/// The classes tagged `oboInOwl:inSubset = subset_name` (by IRI or local name).
fn inset_seed(model: &Model, subset_name: &str) -> HashSet<String> {
    let mut seed: HashSet<String> = HashSet::new();
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

/// The local name of an IRI (after the last `#`, `/`, or `_`).
fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

/// Extract the `subset_name` subset. Classes carrying `oboInOwl:inSubset` with a
/// value naming the subset are kept; every other class is removed (bridging the
/// hierarchy when `fill_gaps`).
pub fn subset(model: Model, subset_name: &str, fill_gaps: bool) -> Result<Model> {
    let seed = inset_seed(&model, subset_name);

    // Gap filling builds a NEW ontology from the closure of the seed — every
    // class the seed refers to, directly or not, plus the axioms and properties
    // that hold the result together (UBERON's `common-anatomy.owl`: 42 tagged
    // classes close over 325). Without it the extraction is a plain removal of
    // the complement, bridging the hierarchy so retained members keep their
    // (now-direct) subsumptions.
    Ok(fill_gaps_subset(&model, &seed, fill_gaps))
}

/// Drop the declaration of any entity no OTHER retained component mentions.
///
/// A subset removes the classes outside its seed and every axiom that named them,
/// which leaves behind the declarations of entities those axioms were the only
/// users of. The reference prunes them; owlmake kept them, so
/// `subsets/immune-minimal.owl` carried 67 `NamedIndividual` declarations against
/// the reference's 0 and 264 `AnnotationProperty` against its 52 — and those 52
/// are EXACTLY the properties its surviving annotations use, with none left over.
///
/// "Mentions" excludes the entity's own declaration, and deliberately also its own
/// annotation assertions: an unused property with a label would otherwise keep
/// itself alive through that label, and the reference drops both together.
fn prune_dangling_declarations(model: &mut crate::model::Model) {
    use horned_owl::model::MutableOntology;

    let declared_iri = |c: &Component<horned_owl::model::RcStr>| -> Option<String> {
        Some(match c {
            Component::DeclareClass(d) => d.0 .0.to_string(),
            Component::DeclareObjectProperty(d) => d.0 .0.to_string(),
            Component::DeclareAnnotationProperty(d) => d.0 .0.to_string(),
            Component::DeclareDataProperty(d) => d.0 .0.to_string(),
            Component::DeclareNamedIndividual(d) => d.0 .0.to_string(),
            Component::DeclareDatatype(d) => d.0 .0.to_string(),
            _ => return None,
        })
    };

    let mut used: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        if declared_iri(&ac.component).is_some() {
            continue;
        }
        // An annotation assertion is ABOUT its subject, so the subject does not
        // count as a use — but the PROPERTY does, which is what keeps the 52.
        if let Component::AnnotationAssertion(aa) = &ac.component {
            used.insert(aa.ann.ap.0.to_string());
            // The VALUE does not count either. An annotation pointing at an
            // entity is not a use of it: `IAO_0000700` naming a term editor, or a
            // `seeAlso` naming an individual, must not keep that entity's
            // declaration alive once the axioms that used it logically are gone.
            for a in &ac.ann {
                used.insert(a.ap.0.to_string());
            }
            continue;
        }
        used.extend(crate::sig::signature(&ac.component));
        for a in &ac.ann {
            used.insert(a.ap.0.to_string());
        }
    }

    // Removing the declaration alone is not enough: an axiom that is merely ABOUT
    // the dead entity keeps it in the signature, and the RDF writer re-declares
    // anything the signature holds. `SubAnnotationPropertyOf(chebi/1_STAR,
    // oboInOwl:SubsetProperty)` — subsetdef scaffolding, one of 112 such — put the
    // declaration straight back. So the entity's own axioms go with it, which is
    // what leaves the reference's file with no mention of it at all.
    let dead: HashSet<String> = model
        .ont
        .iter()
        .filter_map(|ac| declared_iri(&ac.component))
        .filter(|i| !used.contains(i))
        .collect();
    if dead.is_empty() {
        return;
    }
    let about_dead = |c: &Component<horned_owl::model::RcStr>| -> bool {
        match c {
            Component::AnnotationAssertion(aa) => match &aa.subject {
                AnnotationSubject::IRI(i) => dead.contains(i.as_ref()),
                AnnotationSubject::AnonymousIndividual(_) => false,
            },
            Component::SubAnnotationPropertyOf(sp) => dead.contains(sp.sub.0.as_ref()),
            _ => declared_iri(c).is_some_and(|i| dead.contains(&i)),
        }
    };
    let doomed: Vec<_> =
        model.ont.iter().filter(|ac| about_dead(&ac.component)).cloned().collect();
    for ac in doomed {
        model.ont.remove(&ac);
    }
}

/// The gap-filling subset closure: expand the seed to every class it refers to,
/// directly or not, then build a NEW ontology holding those classes' axioms,
/// the object/annotation properties those axioms use, and the axioms defining
/// the properties — iterated to a fixpoint, since property axioms (domains,
/// ranges) can pull in further classes.
///
/// The closure over one round's classes adds (a) every named ancestor the
/// reasoner derives and (b) every class referenced from a class's defining
/// axioms, disjointness excluded. A "dangling" class — no defining axiom other
/// than disjointness, and no annotation assertion — is never PULLED IN by the
/// closure, though a seed member is kept however bare it is. A class axiom is
/// included only when every class it references is in the accumulated subset;
/// a property axiom only when every property it references is included and
/// every class it references is non-dangling. Property chains come along only
/// when wholly within the included properties.
/// Build a subset ontology from `seed`: the seed classes' own axioms, the
/// properties those axioms use, and those properties' axioms. Nothing else in
/// the source survives — rules and unused properties included.
///
/// With `fill_gaps` the seed additionally closes over the class hierarchy and
/// the property hierarchies, and the classes that the property axioms name feed
/// a further round; without it the seed is exactly what was asked for.
fn fill_gaps_subset(model: &Model, seed: &HashSet<String>, fill_gaps: bool) -> Model {
    use horned_owl::model::{AnnotatedComponent, MutableOntology, RcStr};
    use std::collections::HashMap;

    type AC = AnnotatedComponent<RcStr>;

    // ---- Indexes over the source, one pass ---------------------------------
    let mut class_axioms: HashMap<String, Vec<&AC>> = HashMap::new(); // defining axioms by named subject
    let mut gcis: Vec<&AC> = Vec::new();
    let mut anns_by_subject: HashMap<String, Vec<&AC>> = HashMap::new();
    let mut obj_axioms: HashMap<String, Vec<&AC>> = HashMap::new();
    let mut ann_prop_axioms: HashMap<String, Vec<&AC>> = HashMap::new();
    let mut chains: Vec<&AC> = Vec::new();

    let named = |ce: &horned_owl::model::ClassExpression<RcStr>| -> Option<String> {
        match ce {
            horned_owl::model::ClassExpression::Class(c) => Some(c.0.as_ref().to_string()),
            _ => None,
        }
    };
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(sc) => match named(&sc.sub) {
                Some(c) => class_axioms.entry(c).or_default().push(ac),
                None => gcis.push(ac),
            },
            Component::EquivalentClasses(eq) => {
                let members: Vec<String> = eq.0.iter().filter_map(named).collect();
                if members.is_empty() {
                    gcis.push(ac);
                }
                for c in members {
                    class_axioms.entry(c).or_default().push(ac);
                }
            }
            Component::DisjointClasses(dj) => {
                let members: Vec<String> = dj.0.iter().filter_map(named).collect();
                if members.is_empty() {
                    gcis.push(ac);
                }
                for c in members {
                    class_axioms.entry(c).or_default().push(ac);
                }
            }
            Component::DisjointUnion(du) => {
                class_axioms.entry(du.0 .0.as_ref().to_string()).or_default().push(ac);
            }
            Component::AnnotationAssertion(aa) => {
                if let AnnotationSubject::IRI(i) = &aa.subject {
                    anns_by_subject.entry(i.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::SubObjectPropertyOf(sp) => {
                use horned_owl::model::{ObjectPropertyExpression as OPE, SubObjectPropertyExpression as SOPE};
                match &sp.sub {
                    SOPE::ObjectPropertyExpression(OPE::ObjectProperty(p)) => {
                        obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac)
                    }
                    SOPE::ObjectPropertyChain(_) => chains.push(ac),
                    _ => {}
                }
            }
            // An axiom belongs to the properties it is ABOUT — its subject for
            // domain/range/characteristics, its members for equivalence/
            // disjointness/inverses — never to a property merely mentioned
            // inside a domain or range expression. Keying those in would let
            // the closure walk from one property into another's domain and pull
            // that domain's whole subtree into the subset.
            Component::ObjectPropertyDomain(d) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &d.ope {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::ObjectPropertyRange(r) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &r.ope {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::FunctionalObjectProperty(x) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &x.0 {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::InverseFunctionalObjectProperty(x) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &x.0 {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::SymmetricObjectProperty(x) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &x.0 {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::AsymmetricObjectProperty(x) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &x.0 {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::ReflexiveObjectProperty(x) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &x.0 {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::IrreflexiveObjectProperty(x) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &x.0 {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::TransitiveObjectProperty(x) => {
                if let horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) = &x.0 {
                    obj_axioms.entry(p.0.as_ref().to_string()).or_default().push(ac);
                }
            }
            Component::EquivalentObjectProperties(_)
            | Component::DisjointObjectProperties(_)
            | Component::InverseObjectProperties(_) => {
                for (k, iri) in crate::sig::typed_signature(&ac.component) {
                    if k == crate::sig::kind::OBJECT_PROPERTY {
                        obj_axioms.entry(iri).or_default().push(ac);
                    }
                }
            }
            Component::SubAnnotationPropertyOf(sp) => {
                ann_prop_axioms.entry(sp.sub.0.as_ref().to_string()).or_default().push(ac);
            }
            Component::AnnotationPropertyDomain(d) => {
                ann_prop_axioms.entry(d.ap.0.as_ref().to_string()).or_default().push(ac);
            }
            Component::AnnotationPropertyRange(r) => {
                ann_prop_axioms.entry(r.ap.0.as_ref().to_string()).or_default().push(ac);
            }
            _ => {}
        }
    }

    let dangling = |c: &str| -> bool {
        let defining = class_axioms.get(c).is_some_and(|v| {
            v.iter().any(|ac| !matches!(ac.component, Component::DisjointClasses(_)))
        });
        !defining && !anns_by_subject.contains_key(c)
    };
    let include_class = |c: &str| -> bool { !dangling(c) };

    let sig_of = |ac: &AC, kind: u8| -> Vec<String> {
        let mut out: Vec<String> = crate::sig::typed_signature(&ac.component)
            .into_iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, i)| i)
            .collect();
        out.sort();
        out.dedup();
        out
    };

    // All named STRICT ancestors, from the classified hierarchy. An entailed
    // equivalence is mutual subsumption, and an equivalent class is not an
    // ancestor — UBERON's cross-ontology equivalences would otherwise pull the
    // whole equivalent CL/GO subtree into every subset.
    let reasoner = crate::reason::Reasoner::classify(model);
    let subs: HashSet<(String, String)> = reasoner.all_subsumptions().into_iter().collect();
    let mut ancestors: HashMap<String, Vec<String>> = HashMap::new();
    for (sub, sup) in &subs {
        if sup != OWL_THING && !subs.contains(&(sup.clone(), sub.clone())) {
            ancestors.entry(sub.clone()).or_default().push(sup.clone());
        }
    }

    // ---- The closure -------------------------------------------------------
    let mut axioms: std::collections::BTreeSet<AC> = std::collections::BTreeSet::new();
    let mut work: HashSet<String> = seed.clone();
    let mut round: HashSet<String> = seed.clone();
    let mut first = true;
    loop {
        let before = axioms.len();

        // Classes closure over this round's classes: reasoner ancestors, plus
        // classes referenced from defining axioms (disjointness excluded).
        // Runs to a fixpoint when gap-filling is on, and not at all when it is off:
        // the guard is the mode, the exit is the empty round below.
        #[allow(clippy::while_immutable_condition)]
        while fill_gaps {
            let mut fresh: HashSet<String> = HashSet::new();
            for c in &round {
                for sup in ancestors.get(c).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if !round.contains(sup) && include_class(sup) {
                        fresh.insert(sup.clone());
                    }
                }
                for ac in class_axioms.get(c).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if matches!(ac.component, Component::DisjointClasses(_)) {
                        continue;
                    }
                    for r in sig_of(ac, crate::sig::kind::CLASS) {
                        if !round.contains(&r) && include_class(&r) {
                            fresh.insert(r);
                        }
                    }
                }
            }
            if fresh.is_empty() {
                break;
            }
            round.extend(fresh);
        }
        let closure_added = round.len();
        work.extend(round.iter().cloned());
        // On the first pass the round IS the whole subset, so every seed class
        // gets its axioms; later rounds only visit the classes the property
        // axioms just pulled in.
        if first {
            round = work.clone();
            first = false;
        }

        // Class axioms whose class signature lies wholly in the subset, plus
        // annotations and a declaration for each class of the round.
        let mut class_ax: Vec<&AC> = Vec::new();
        for c in &round {
            let mut pool: Vec<&AC> =
                class_axioms.get(c).map(|v| v.clone()).unwrap_or_default();
            pool.extend(gcis.iter().filter(|ac| {
                sig_of(ac, crate::sig::kind::CLASS).iter().any(|i| i == c)
            }));
            for ac in pool {
                if sig_of(ac, crate::sig::kind::CLASS).iter().all(|i| work.contains(i)) {
                    class_ax.push(ac);
                }
            }
            for ac in anns_by_subject.get(c).map(|v| v.as_slice()).unwrap_or(&[]) {
                class_ax.push(ac);
            }
        }
        for ac in &class_ax {
            axioms.insert((*ac).clone());
        }
        for c in &round {
            axioms.insert(AC::from(Component::DeclareClass(horned_owl::model::DeclareClass(
                model.build.class(c.clone()),
            ))));
        }

        // Properties those axioms use, closed over the property hierarchy
        // (super- and inverse properties, read from the axioms — the reasoner
        // is not consulted for property hierarchies).
        let mut oprops: HashSet<String> = HashSet::new();
        let mut aprops: HashSet<String> = HashSet::new();
        for ac in &class_ax {
            oprops.extend(sig_of(ac, crate::sig::kind::OBJECT_PROPERTY));
            aprops.extend(crate::sig::annotation_properties(&ac.component));
            // An axiom's signature includes its AXIOM ANNOTATIONS: a synonym
            // reification's `hasSynonymType` is a used annotation property and
            // must be carried with its own axioms, not left to the writer's
            // bare-declaration stub. Properties only — an annotation's IRI
            // VALUE names no property.
            fn ann_props(a: &horned_owl::model::Annotation<horned_owl::model::RcStr>, out: &mut HashSet<String>) {
                out.insert(a.ap.0.as_ref().to_string());
                for n in &a.ann {
                    ann_props(n, out);
                }
            }
            for a in &ac.ann {
                ann_props(a, &mut aprops);
            }
        }
        // Runs to a fixpoint when gap-filling is on, and not at all when it is off:
        // the guard is the mode, the exit is the empty round below.
        #[allow(clippy::while_immutable_condition)]
        while fill_gaps {
            let mut fresh: HashSet<String> = HashSet::new();
            for p in &oprops {
                for ac in obj_axioms.get(p).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if matches!(ac.component, Component::DisjointObjectProperties(_)) {
                        continue;
                    }
                    for q in sig_of(ac, crate::sig::kind::OBJECT_PROPERTY) {
                        if !oprops.contains(&q) {
                            fresh.insert(q);
                        }
                    }
                }
            }
            if fresh.is_empty() {
                break;
            }
            oprops.extend(fresh);
        }
        // The property hierarchy is also asked of the reasoner, whose top node
        // caps every answer — so a subset with any object property at all
        // includes (and declares) owl:topObjectProperty.
        if fill_gaps && !oprops.is_empty() {
            oprops.insert("http://www.w3.org/2002/07/owl#topObjectProperty".to_string());
        }
        // Runs to a fixpoint when gap-filling is on, and not at all when it is off:
        // the guard is the mode, the exit is the empty round below.
        #[allow(clippy::while_immutable_condition)]
        while fill_gaps {
            let mut fresh: HashSet<String> = HashSet::new();
            for p in &aprops {
                for ac in ann_prop_axioms.get(p).map(|v| v.as_slice()).unwrap_or(&[]) {
                    for q in crate::sig::annotation_properties(&ac.component) {
                        if !aprops.contains(&q) {
                            fresh.insert(q);
                        }
                    }
                }
                for ac in anns_by_subject.get(p).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if let Component::AnnotationAssertion(aa) = &ac.component {
                        let q = aa.ann.ap.0.as_ref().to_string();
                        if !aprops.contains(&q) {
                            fresh.insert(q);
                        }
                    }
                }
            }
            if fresh.is_empty() {
                break;
            }
            aprops.extend(fresh);
        }

        // Property axioms: wholly within the included properties, touching no
        // dangling class. Their class signatures seed the next round.
        let mut prop_ax: Vec<&AC> = Vec::new();
        for p in &oprops {
            for ac in obj_axioms.get(p).map(|v| v.as_slice()).unwrap_or(&[]) {
                if sig_of(ac, crate::sig::kind::OBJECT_PROPERTY).iter().all(|q| oprops.contains(q))
                    && sig_of(ac, crate::sig::kind::CLASS).iter().all(|c| include_class(c))
                {
                    prop_ax.push(ac);
                }
            }
            for ac in anns_by_subject.get(p).map(|v| v.as_slice()).unwrap_or(&[]) {
                prop_ax.push(ac);
            }
            axioms.insert(AC::from(Component::DeclareObjectProperty(
                horned_owl::model::DeclareObjectProperty(model.build.object_property(p.clone())),
            )));
        }
        for ac in &chains {
            if sig_of(ac, crate::sig::kind::OBJECT_PROPERTY).iter().all(|q| oprops.contains(q)) {
                prop_ax.push(ac);
            }
        }
        for p in &aprops {
            for ac in ann_prop_axioms.get(p).map(|v| v.as_slice()).unwrap_or(&[]) {
                if sig_of(ac, crate::sig::kind::CLASS).iter().all(|c| include_class(c)) {
                    prop_ax.push(ac);
                }
            }
            for ac in anns_by_subject.get(p).map(|v| v.as_slice()).unwrap_or(&[]) {
                prop_ax.push(ac);
            }
            axioms.insert(AC::from(Component::DeclareAnnotationProperty(
                horned_owl::model::DeclareAnnotationProperty(
                    model.build.annotation_property(p.clone()),
                ),
            )));
        }

        let mut next_round: HashSet<String> = HashSet::new();
        let dbg = std::env::var("OM_DEBUG_SUBSET").is_ok();
        for ac in &prop_ax {
            axioms.insert((*ac).clone());
            if !fill_gaps {
                continue;
            }
            for c in sig_of(ac, crate::sig::kind::CLASS) {
                if !work.contains(&c) {
                    if dbg {
                        eprintln!("SUBSET next_round {} via {:?}", c, ac.component);
                    }
                    next_round.insert(c);
                }
            }
        }

        crate::status!(
            "subset: closure round added {} class(es); {} axiom(s) new",
            closure_added,
            axioms.len() - before
        );
        if !fill_gaps || axioms.len() == before {
            break;
        }
        round = next_round;
    }

    let mut ont = horned_owl::ontology::set::SetOntology::new();
    for ac in axioms {
        ont.insert(ac);
    }
    let mut out = Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes));
    // Two axioms that name the SAME anonymous expression name the same node, and
    // which expressions are shared that way is a property of the content, not of
    // how it got here — so the subset keeps the source's sharing.
    out.shared_anon = model.shared_anon.clone();
    out.rdf_shared_anon = model.rdf_shared_anon.clone();
    out.owl_shared_owners = model.owl_shared_owners.clone();
    out.span_shared = model.span_shared.clone();
    out.cross_shared = model.cross_shared.clone();
    // A NEW ontology has a fresh document format: its `rdf:RDF` xmlns block is
    // rebuilt from the entities the subset actually carries, not inherited from
    // the input's declarations.
    out.explicit_prefixes = model.explicit_prefixes.clone();
    out.format_prefixes_cleared = true;
    out
}
