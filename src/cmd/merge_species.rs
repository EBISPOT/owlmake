//! `merge-species` — fold species-specific classes into taxon-neutral ones to
//! build UBERON-style "composite" cross-species ontologies. The fold is
//! reasoner-driven: which classes are specific to a taxon is an inference, not
//! something the edit file states.
//!
//! For each `(taxon, linking-property, suffix)` operation:
//!
//! 1. Assert `taxon-part ≡ <link> some <taxon>`, classify, and collect every
//!    inferred subclass — the taxon-specific classes.
//! 2. From `C ≡ N ⊓ (P some T)` equivalence definitions, map each species class
//!    `C` to its taxon-neutral counterpart `N` (and the full defining expression).
//! 3. For each `C`: take its axioms plus inferred direct superclasses, *translate*
//!    them by substituting `C→N` recursively through class expressions, suffix its
//!    label, drop axioms the neutral class already entails, and delete `C`.
//!
//! Operations are read from a batch TSV (`taxon ⟨tab⟩ label ⟨tab⟩ link-props
//! ⟨tab⟩ included-props`) or generated from UBERON's `config/taxa.yaml`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, Build,
    Class, ClassExpression as CE, Component, DeclareClass, EquivalentClasses, Literal,
    ObjectPropertyExpression as OPE, SubClassOf,
};
use horned_owl::model::MutableOntology;

use crate::cmd::babelon::expand_curie;
use crate::model::{Model, Str};
use crate::reason::el;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const IN_SUBSET: &str = "http://www.geneontology.org/formats/oboInOwl#inSubset";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcaMode {
    /// Keep general class axioms referencing merged classes unchanged.
    Original,
    /// Translate them.
    Translate,
    /// Remove them.
    Delete,
}

pub struct Options {
    pub extended_translation: bool,
    pub gca_mode: GcaMode,
    pub remove_declarations: bool,
}

#[derive(Debug, Clone)]
pub struct MergeOp {
    pub taxon: String,
    pub label: String,
    pub link_properties: Vec<String>,
    pub included_properties: Vec<String>,
}

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub format: Option<String>,
    /// Batch file describing the merges (`taxon\tlabel\tlink-props\tincluded-props`).
    #[arg(short = 'b', long = "batch-file")]
    pub batch_file: Option<PathBuf>,
    /// Single-merge taxon (CURIE or IRI), if no batch file.
    #[arg(short = 't', long)]
    pub taxon: Option<String>,
    #[arg(short = 'p', long)]
    pub property: Vec<String>,
    #[arg(short = 's', long)]
    pub suffix: Option<String>,
    #[arg(short = 'q', long = "include-property")]
    pub include_property: Vec<String>,
    #[arg(short = 'x', long = "extended-translation")]
    pub extended_translation: bool,
    #[arg(short = 'g', long = "translate-gcas")]
    pub translate_gcas: bool,
    #[arg(short = 'G', long = "remove-gcas")]
    pub remove_gcas: bool,
    #[arg(short = 'd', long = "remove-declarations")]
    pub remove_declarations: bool,
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

    let ops = if let Some(bf) = &args.batch_file {
        parse_batch(&std::fs::read_to_string(bf).with_context(|| format!("reading {}", bf.display()))?)
    } else if let Some(t) = &args.taxon {
        let props = if args.property.is_empty() {
            vec![expand_curie("BFO:0000050")]
        } else {
            args.property.iter().map(|p| expand_curie(p)).collect()
        };
        vec![MergeOp {
            taxon: expand_curie(t),
            label: args.suffix.clone().unwrap_or_else(|| "species specific".into()),
            link_properties: props,
            included_properties: args.include_property.iter().map(|p| expand_curie(p)).collect(),
        }]
    } else {
        vec![]
    };
    let gca_mode = if args.translate_gcas {
        GcaMode::Translate
    } else if args.remove_gcas {
        GcaMode::Delete
    } else {
        GcaMode::Original
    };
    let mut model = merge_species(
        model,
        &ops,
        &Options {
            extended_translation: args.extended_translation,
            gca_mode,
            remove_declarations: args.remove_declarations,
        },
    )?;
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Parse a `merge-species` batch TSV (CURIEs expanded to full IRIs).
pub fn parse_batch(text: &str) -> Vec<MergeOp> {
    let mut ops = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let split = |s: &str| s.split(',').filter(|x| !x.is_empty()).map(|x| expand_curie(x.trim())).collect();
        ops.push(MergeOp {
            taxon: expand_curie(f[0].trim()),
            label: f.get(1).map(|s| s.trim().to_string()).unwrap_or_else(|| "species specific".into()),
            link_properties: if f.len() > 2 && !f[2].trim().is_empty() {
                split(f[2])
            } else {
                vec![expand_curie("BFO:0000050")]
            },
            included_properties: if f.len() > 3 { split(f[3]) } else { vec![] },
        });
    }
    ops
}

/// Build the merge table UBERON's `config/taxa.yaml` describes: one [`MergeOp`]
/// per species listed there, with link/included properties defaulting to the
/// file's `defaults.compositing.unfold_over` / `preserve`.
pub fn ops_from_taxa_yaml(text: &str) -> Result<Vec<MergeOp>> {
    let y: serde_yaml::Value = serde_yaml::from_str(text).context("parsing taxa.yaml")?;
    let comp = |v: &serde_yaml::Value, k: &str| -> Vec<String> {
        v.get("compositing")
            .and_then(|c| c.get(k))
            .and_then(|s| s.as_sequence())
            .map(|seq| seq.iter().filter_map(|x| x.as_str()).map(expand_curie).collect())
            .unwrap_or_default()
    };
    let defaults = y.get("defaults").cloned().unwrap_or(serde_yaml::Value::Null);
    let mut def_links = comp(&defaults, "unfold_over");
    if def_links.is_empty() {
        def_links = vec![expand_curie("BFO:0000050"), expand_curie("BFO:0000066")];
    }
    let def_preserve = comp(&defaults, "preserve");

    let mut ops = Vec::new();
    if let Some(species) = y.get("species").and_then(|s| s.as_sequence()) {
        for sp in species {
            let Some(taxon) = sp.get("taxon_id").and_then(|t| t.as_str()) else { continue };
            let links = {
                let l = comp(sp, "unfold_over");
                if l.is_empty() { def_links.clone() } else { l }
            };
            let preserve = {
                let p = comp(sp, "preserve");
                if p.is_empty() { def_preserve.clone() } else { p }
            };
            ops.push(MergeOp {
                taxon: expand_curie(taxon),
                label: sp.get("label").and_then(|l| l.as_str()).unwrap_or("species specific").to_string(),
                link_properties: links,
                included_properties: preserve,
            });
        }
    }
    Ok(ops)
}

/// Run every merge operation in sequence, each over each of its link properties;
/// every unfold sees the model the previous one produced.
pub fn merge_species(mut model: Model, ops: &[MergeOp], opts: &Options) -> Result<Model> {
    for op in ops {
        for link in &op.link_properties {
            model = merge_one(model, &op.taxon, link, &op.label, &op.included_properties, opts)?;
        }
    }
    Ok(model)
}

/// One unfold: fold the classes specific to `taxon` (via `link`) into their
/// taxon-neutral counterparts.
fn merge_one(
    mut model: Model,
    taxon: &str,
    link: &str,
    suffix: &str,
    included: &[String],
    opts: &Options,
) -> Result<Model> {
    let b = Build::new();
    let tx_root = format!("{taxon}-part");

    // 1. Classify with a temporary `tx_root ≡ link some taxon` to enumerate the
    //    taxon-specific classes (all inferred subclasses of tx_root).
    let temp: AnnotatedComponent<Str> = Component::EquivalentClasses(EquivalentClasses(vec![
        CE::Class(b.class(tx_root.as_str())),
        CE::ObjectSomeValuesFrom {
            ope: OPE::ObjectProperty(b.object_property(link)),
            bce: Box::new(CE::Class(b.class(taxon))),
        },
    ]))
    .into();
    model.ont.insert(temp.clone());

    let reasoner = el::Reasoner::classify(&model);
    let unsat: Vec<String> = reasoner.unsatisfiable();
    let all_subs = reasoner.all_subsumptions();
    let dir_subs = reasoner.direct_subsumptions();
    drop(reasoner);
    model.ont.remove(&temp);
    if !unsat.is_empty() {
        anyhow::bail!("merge-species: ontology has {} unsatisfiable class(es)", unsat.len());
    }

    // Superclass tables from the classification.
    let mut sup_all: HashMap<String, HashSet<String>> = HashMap::new();
    for (sub, sup) in all_subs {
        sup_all.entry(sub).or_default().insert(sup);
    }
    let mut sup_dir: HashMap<String, HashSet<String>> = HashMap::new();
    for (sub, sup) in dir_subs {
        sup_dir.entry(sub).or_default().insert(sup);
    }
    let tx_classes: Vec<String> = sup_all
        .iter()
        .filter(|(_, sups)| sups.contains(&tx_root))
        .map(|(c, _)| c.clone())
        .collect();

    // 2. ecMap (C → N) and exMap (C → "N ⊓ (P some T)") from equivalence axioms
    //    mentioning both the taxon and the link property.
    //
    //    A class can appear in several qualifying equivalences — its own
    //    definition and as a conjunct in other classes' definitions — and the
    //    LAST write wins, per axiom. The iteration order that decides the
    //    winner is the hash-set order the axioms are read in: OWLAPI content
    //    hash buckets (see `owlapi_hash`), document order within a
    //    bucket. Within one axiom, members and operands iterate in OWLAPI
    //    sorted order, so the last class operand of the last intersection
    //    wins. Nothing here prefers an own definition; a foreign context CAN
    //    map a class to a self-referencing expression, and the translation
    //    then carries the class name through — the leftover mentions are what
    //    keep such a class in the signature.
    let tx_set: HashSet<&String> = tx_classes.iter().collect();
    let mut ec_map: HashMap<String, String> = HashMap::new();
    let mut ex_map: HashMap<String, CE<Str>> = HashMap::new();
    let qualifying: Vec<&AnnotatedComponent<Str>> = model
        .ont
        .iter()
        .filter(|ac| {
            let Component::EquivalentClasses(eqc) = &ac.component else { return false };
            let sig = class_iris_ce_list(&eqc.0);
            sig.contains(taxon) && mentions_property(&eqc.0, link)
        })
        .collect();
    let hashes: Vec<i32> = qualifying
        .iter()
        .map(|ac| {
            let Component::EquivalentClasses(eqc) = &ac.component else { unreachable!() };
            crate::owlapi_hash::equivalent_classes_hash(&eqc.0, &ac.ann)
        })
        .collect();
    // The set the axioms are read from holds EVERY EquivalentClasses axiom;
    // qualification filters during iteration, so the bucket mask comes from
    // the full count.
    let total_eq = model
        .ont
        .iter()
        .filter(|ac| matches!(&ac.component, Component::EquivalentClasses(_)))
        .count();
    for i in crate::owlapi_hash::hashset_order_of(&hashes, total_eq) {
        let Component::EquivalentClasses(eqc) = &qualifying[i].component else { unreachable!() };
        let mut members: Vec<&CE<Str>> = eqc.0.iter().collect();
        members.sort_by(|a, b| crate::owlapi_hash::owl_cmp(a, b));
        let sig = class_iris_ce_list(&eqc.0);
        for c in &sig {
            if !tx_set.contains(c) {
                continue;
            }
            for x in &members {
                if matches!(x, CE::Class(xc) if xc.0.as_ref() == c.as_str()) {
                    continue;
                }
                if let CE::ObjectIntersectionOf(operands) = x {
                    let mut ops: Vec<&CE<Str>> = operands.iter().collect();
                    ops.sort_by(|a, b| crate::owlapi_hash::owl_cmp(a, b));
                    for n in ops {
                        if let CE::Class(nc) = n {
                            ec_map.insert(c.clone(), nc.0.as_ref().to_string());
                            ex_map.insert(c.clone(), (*x).clone());
                        }
                    }
                }
            }
        }
    }

    // Asserted SubClassOf axioms indexed by named subclass (for the redundancy
    // check), and the "skippable" upper-level/non-informative classes.
    let mut sub_axioms: HashMap<String, Vec<CE<Str>>> = HashMap::new();
    let mut skippable: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(sc) => {
                if let CE::Class(c) = &sc.sub {
                    sub_axioms.entry(c.0.as_ref().to_string()).or_default().push(sc.sup.clone());
                }
            }
            Component::AnnotationAssertion(aa) => {
                if aa.ann.ap.0.as_ref() == IN_SUBSET {
                    if let (AnnotationSubject::IRI(s), AnnotationValue::IRI(v)) = (&aa.subject, &aa.ann.av) {
                        let v = v.as_ref();
                        if v.contains("upper_level") || v.contains("non_informative") || v.contains("early_development") {
                            skippable.insert(s.as_ref().to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let tr = Translator {
        b: &b,
        ec_map: &ec_map,
        ex_map: &ex_map,
        extended: opts.extended_translation,
    };

    // 3. Process each taxon-specific class.
    let mut to_remove: Vec<AnnotatedComponent<Str>> = Vec::new();
    let mut to_add: Vec<AnnotatedComponent<Str>> = Vec::new();
    let empty = HashSet::new();

    for c in &tx_classes {
        // Gather the axioms "about" C: its asserted SubClassOf/EquivalentClasses/
        // annotations, plus its inferred direct named superclasses.
        // Each axiom about C travels WITH its own annotations: a synonym's
        // `oboInOwl:hasDbXref`, a definition's provenance. Translating the bare
        // component instead silently unreified every assertion the species merge
        // rewrote — 29,004 annotated `hasExactSynonym` in `composite-metazoan.owl`
        // kept their text and lost their source.
        let mut about: Vec<(Component<Str>, std::collections::BTreeSet<Annotation<Str>>)> =
            Vec::new();
        for ac in model.ont.iter() {
            match &ac.component {
                Component::SubClassOf(sc) if matches!(&sc.sub, CE::Class(x) if x.0.as_ref() == c) => {
                    about.push((ac.component.clone(), ac.ann.clone()));
                    to_remove.push(ac.clone());
                }
                Component::EquivalentClasses(eqc)
                    if eqc.0.iter().any(|e| matches!(e, CE::Class(x) if x.0.as_ref() == c)) =>
                {
                    about.push((ac.component.clone(), ac.ann.clone()));
                    to_remove.push(ac.clone());
                }
                Component::AnnotationAssertion(aa)
                    if matches!(&aa.subject, AnnotationSubject::IRI(s) if s.as_ref() == c) =>
                {
                    about.push((ac.component.clone(), ac.ann.clone()));
                    to_remove.push(ac.clone());
                }
                _ => {}
            }
        }
        for p in sup_dir.get(c).unwrap_or(&empty) {
            if p != &tx_root {
                // Synthesised from the inferred hierarchy, so it carries no
                // annotations of its own.
                about.push((
                    Component::SubClassOf(SubClassOf {
                        sub: CE::Class(b.class(c.as_str())),
                        sup: CE::Class(b.class(p.as_str())),
                    }),
                    Default::default(),
                ));
            }
        }
        if opts.remove_declarations && ec_map.contains_key(c) {
            to_remove.push(Component::DeclareClass(DeclareClass(b.class(c.as_str()))).into());
        }

        let merged = ec_map.contains_key(c);
        for (axiom, anns) in about {
            let Some(t) = tr.translate_axiom(&axiom, c, suffix, &sup_all, &sub_axioms, included) else {
                continue;
            };
            // Drop translations that pull in an upper-level/non-informative class.
            if merged && class_iris(&t).iter().any(|s| skippable.contains(s)) {
                continue;
            }
            // Never re-introduce the temporary tx_root.
            if class_iris(&t).contains(&tx_root) {
                continue;
            }
            // A translated LOGICAL axiom is re-asserted BARE: the translation
            // builds a new axiom and its reification does not follow it. An
            // annotation assertion keeps its axiom annotations only when it
            // passes through UNCHANGED — a relabelled one (the `(suffix)` form)
            // is a new assertion with none. This is what keeps a synonym's
            // `hasDbXref` reification on an untouched assertion while the
            // brain-atlas equivalences come back without their `rdfs:label`
            // reification blocks.
            let keep_anns = t == axiom && matches!(t, Component::AnnotationAssertion(_));
            to_add.push(AnnotatedComponent {
                component: t,
                ann: if keep_anns { anns } else { Default::default() },
            });
        }
    }

    // Apply the per-class removals and translated re-adds BEFORE the general-
    // class-axiom phase: the phase runs over the live ontology, so a translated
    // equivalence the merge just added is itself re-examined — and one whose
    // sides now translate to the SAME expression collapses to unary and is
    // dropped, which is how the anonymous taxon-bridge equivalences disappear.
    for r in to_remove.drain(..) {
        model.ont.remove(&r);
    }
    for a in to_add.drain(..) {
        model.ont.insert(a);
    }

    // 4. General class axioms referencing a merged class: a SubClassOf with an
    //    anonymous subclass, and an EquivalentClasses or DisjointClasses whose
    //    members are ALL anonymous. The anonymous equivalences are how the
    //    taxon-bridge files relate species expressions, and left untouched they
    //    keep every merged class alive through their nested references.
    if opts.gca_mode != GcaMode::Original {
        for ac in model.ont.iter() {
            let is_gca = match &ac.component {
                Component::SubClassOf(sc) => !matches!(&sc.sub, CE::Class(_)),
                Component::EquivalentClasses(eq) => {
                    !eq.0.iter().any(|e| matches!(e, CE::Class(_)))
                }
                Component::DisjointClasses(dj) => {
                    !dj.0.iter().any(|e| matches!(e, CE::Class(_)))
                }
                _ => false,
            };
            if !is_gca {
                continue;
            }
            if !class_iris(&ac.component).iter().any(|s| ec_map.contains_key(s)) {
                continue; // unaffected
            }
            to_remove.push(ac.clone());
            if opts.gca_mode == GcaMode::Translate {
                if let Some(t) = tr.translate_axiom(&ac.component, "", suffix, &sup_all, &sub_axioms, included) {
                    to_add.push(AnnotatedComponent { component: t, ann: ac.ann.clone() });
                }
            }
        }
    }

    for r in &to_remove {
        model.ont.remove(r);
    }
    for a in to_add {
        model.ont.insert(a);
    }
    Ok(model)
}

struct Translator<'a> {
    b: &'a Build<Str>,
    ec_map: &'a HashMap<String, String>,
    ex_map: &'a HashMap<String, CE<Str>>,
    extended: bool,
}

impl Translator<'_> {
    /// Translate one axiom about class `subject`. Returns `None` if it can't be
    /// fully translated (then it is simply dropped).
    fn translate_axiom(
        &self,
        comp: &Component<Str>,
        subject: &str,
        suffix: &str,
        sup_all: &HashMap<String, HashSet<String>>,
        sub_axioms: &HashMap<String, Vec<CE<Str>>>,
        included: &[String],
    ) -> Option<Component<Str>> {
        match comp {
            Component::EquivalentClasses(eqc) => {
                // The translated expressions form a SET. When translating C to
                // its defining expression makes both sides equal, the axiom
                // collapses to a single expression — equivalent to nothing —
                // and nothing is re-asserted.
                let mut xs: Vec<CE<Str>> = Vec::with_capacity(eqc.0.len());
                for x in &eqc.0 {
                    let t = self.translate(x, true)?;
                    if !xs.contains(&t) {
                        xs.push(t);
                    }
                }
                if xs.len() < 2 {
                    return None;
                }
                Some(Component::EquivalentClasses(EquivalentClasses(xs)))
            }
            Component::AnnotationAssertion(aa) => {
                // Annotations on a merged class are dropped (the neutral class
                // keeps its own); for a non-merged tx class, suffix its label.
                if self.ec_map.contains_key(subject) {
                    return None;
                }
                if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                    if let AnnotationValue::Literal(lit) = &aa.ann.av {
                        let new = format!("{} ({suffix})", lit.literal());
                        return Some(Component::AnnotationAssertion(AnnotationAssertion {
                            subject: aa.subject.clone(),
                            ann: Annotation { ann: Default::default(),
                                ap: aa.ann.ap.clone(),
                                av: AnnotationValue::Literal(Literal::Simple { literal: new }),
                            },
                        }));
                    }
                }
                Some(comp.clone())
            }
            Component::SubClassOf(sc) => {
                let tr_sub = self.translate(&sc.sub, true)?;
                let tr_sup = self.translate(&sc.sup, false)?;
                // Avoid circular references.
                if let CE::Class(s) = &tr_sup {
                    if class_iris_ce(&tr_sub).contains(s.0.as_ref()) {
                        return None;
                    }
                }
                let sub_anon = !matches!(&sc.sub, CE::Class(_));
                if sub_anon {
                    return Some(Component::SubClassOf(SubClassOf { sub: tr_sub, sup: tr_sup }));
                }
                // Named subclass that was folded (C → N): elide redundancy.
                let was_translated = !ce_eq(&tr_sub, &sc.sub);
                if was_translated {
                    if let CE::Class(orig) = &sc.sub {
                        if let Some(n) = self.ec_map.get(orig.0.as_ref()) {
                            // N already has a NAMED tr_sup as an inferred STRICT
                            // superclass. An equivalent is not a superclass here:
                            // `S ≡ N` still gets its translated `(N ⊓ tax) ⊑ S`.
                            if let CE::Class(trs) = &tr_sup {
                                let trs = trs.0.as_ref();
                                // owl:Thing is a strict superclass of every
                                // satisfiable class.
                                if trs == "http://www.w3.org/2002/07/owl#Thing" {
                                    return None;
                                }
                                let above = sup_all.get(n).map(|s| s.contains(trs)).unwrap_or(false);
                                let below = sup_all.get(trs).map(|s| s.contains(n.as_str())).unwrap_or(false);
                                if above && !below {
                                    return None;
                                }
                            }
                            // An ancestor or equivalent of N — N ITSELF included —
                            // already asserts ⊑ tr_sup, whatever tr_sup's shape.
                            // Including N is what drops a self-mapped class's own
                            // translations: its original axioms are still asserted
                            // while it is being processed, and `(N ⊓ tax) ⊑ S`
                            // says nothing `N ⊑ S` doesn't.
                            let mut ancs: HashSet<&String> = HashSet::new();
                            ancs.insert(n);
                            if let Some(s) = sup_all.get(n) {
                                ancs.extend(s.iter());
                                // Inferred equivalents: mutual subsumption.
                                for e in s.iter() {
                                    if sup_all.get(e).is_some_and(|se| se.contains(n)) {
                                        ancs.insert(e);
                                    }
                                }
                            }
                            for p in ancs {
                                if let Some(sups) = sub_axioms.get(p) {
                                    if sups.iter().any(|s| ce_eq(s, &tr_sup)) {
                                        return None;
                                    }
                                }
                            }
                        }
                    }
                    // --include-property filter: keep only axioms over a preserved
                    // property (or with no object property at all).
                    if !included.is_empty() {
                        let props = object_properties_ce(&sc.sup);
                        let ok = props.is_empty() || props.iter().any(|p| included.contains(p));
                        if !ok {
                            return None;
                        }
                    }
                }
                Some(Component::SubClassOf(SubClassOf { sub: tr_sub, sup: tr_sup }))
            }
            // Other axiom types about C (e.g. DisjointClasses) have no translation
            // rule, so they are dropped along with C.
            _ => None,
        }
    }

    /// Translate a class expression. With `must_be_equiv`, a merged named class
    /// maps to its full defining expression `N ⊓ (P some T)`; otherwise to the
    /// neutral class `N`. Non-merged classes map to themselves.
    fn translate(&self, x: &CE<Str>, must_be_equiv: bool) -> Option<CE<Str>> {
        match x {
            CE::Class(c) => {
                let iri = c.0.as_ref();
                if must_be_equiv {
                    Some(self.ex_map.get(iri).cloned().unwrap_or_else(|| x.clone()))
                } else {
                    Some(
                        self.ec_map
                            .get(iri)
                            .map(|n| CE::Class(self.b.class(n.as_str())))
                            .unwrap_or_else(|| x.clone()),
                    )
                }
            }
            CE::ObjectSomeValuesFrom { ope, bce } => {
                let f = self.translate(bce, must_be_equiv)?;
                Some(CE::ObjectSomeValuesFrom { ope: ope.clone(), bce: Box::new(f) })
            }
            CE::ObjectIntersectionOf(v) if self.extended => self.translate_nary(v, must_be_equiv).map(CE::ObjectIntersectionOf),
            CE::ObjectUnionOf(v) if self.extended => self.translate_nary(v, must_be_equiv).map(CE::ObjectUnionOf),
            CE::ObjectComplementOf(b) if self.extended => {
                self.translate(b, must_be_equiv).map(|t| CE::ObjectComplementOf(Box::new(t)))
            }
            CE::ObjectExactCardinality { n, ope, bce } if self.extended => {
                let f = self.translate(bce, must_be_equiv)?;
                Some(CE::ObjectExactCardinality { n: *n, ope: ope.clone(), bce: Box::new(f) })
            }
            CE::ObjectMinCardinality { n, ope, bce } if self.extended => {
                let f = self.translate(bce, must_be_equiv)?;
                Some(CE::ObjectMinCardinality { n: *n, ope: ope.clone(), bce: Box::new(f) })
            }
            CE::ObjectMaxCardinality { n, ope, bce } if self.extended => {
                let f = self.translate(bce, must_be_equiv)?;
                Some(CE::ObjectMaxCardinality { n: *n, ope: ope.clone(), bce: Box::new(f) })
            }
            _ => None,
        }
    }

    fn translate_nary(&self, v: &[CE<Str>], must_be_equiv: bool) -> Option<Vec<CE<Str>>> {
        // Operands live in a SET: a translation that makes two operands equal
        // COLLAPSES the expression, and a collapsed expression is no longer the
        // one asserted — so the translation fails and the whole axiom is
        // dropped, exactly as when an operand cannot be translated at all.
        let mut out: Vec<CE<Str>> = Vec::with_capacity(v.len());
        for o in v {
            let t = self.translate(o, must_be_equiv)?;
            if !out.contains(&t) {
                out.push(t);
            }
        }
        let mut distinct_in = 0usize;
        for (i, o) in v.iter().enumerate() {
            if !v[..i].contains(o) {
                distinct_in += 1;
            }
        }
        (out.len() == distinct_in).then_some(out)
    }
}

// --- small helpers ------------------------------------------------------------

fn ce_eq(a: &CE<Str>, b: &CE<Str>) -> bool {
    a == b
}

fn class_iris(comp: &Component<Str>) -> HashSet<String> {
    use horned_owl::visitor::immutable::{Visit, Walk};
    #[derive(Default)]
    struct E(HashSet<String>);
    impl Visit<Str> for E {
        fn visit_class(&mut self, c: &Class<Str>) {
            self.0.insert(c.0.as_ref().to_string());
        }
    }
    let mut w = Walk::new(E::default());
    w.component(comp);
    w.into_visit().0
}

fn class_iris_ce(ce: &CE<Str>) -> HashSet<String> {
    class_iris(&Component::SubClassOf(SubClassOf { sub: ce.clone(), sup: ce.clone() }))
}

fn class_iris_ce_list(list: &[CE<Str>]) -> HashSet<String> {
    let mut out = HashSet::new();
    for ce in list {
        out.extend(class_iris_ce(ce));
    }
    out
}

fn object_properties_ce(ce: &CE<Str>) -> HashSet<String> {
    use horned_owl::visitor::immutable::{Visit, Walk};
    #[derive(Default)]
    struct E(HashSet<String>);
    impl Visit<Str> for E {
        fn visit_object_property(&mut self, p: &horned_owl::model::ObjectProperty<Str>) {
            self.0.insert(p.0.as_ref().to_string());
        }
    }
    let mut w = Walk::new(E::default());
    w.class_expression(ce);
    w.into_visit().0
}

fn mentions_property(list: &[CE<Str>], prop: &str) -> bool {
    list.iter().any(|ce| object_properties_ce(ce).contains(prop))
}
