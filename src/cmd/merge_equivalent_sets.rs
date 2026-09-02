//! `merge-equivalent-sets` — collapse cliques of equivalent named classes into a
//! single representative, so an ontology that asserts equivalences across sources
//! releases one class per concept instead of a mutually-equivalent set.
//!
//! Cliques are *inferred*: the ontology is classified with the EL reasoner and
//! two classes belong to one clique when each is entailed to subsume the other.
//! This is broader than the asserted `EquivalentClasses` links (mutual
//! subsumption can arise through complex axioms) and narrower where an asserted
//! equivalence has an anonymous side that entails only one direction. For each
//! clique a *leader* is chosen by IRI-prefix priority (`-s PREFIX=SCORE`); all
//! other members are rewritten to the leader, and the leader gains an
//! `oboInOwl:hasDbXref` naming each merged member's OBO id. The leader's label
//! and definition are taken from the member whose prefix has the highest
//! `-l`/`-d` priority. Nothing here is ontology-specific — the priorities are
//! supplied as arguments.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotationSubject, AnnotationValue, ClassExpression as CE, Component, Literal, MutableOntology,
};
use horned_owl::ontology::set::SetOntology;

use crate::model::Model;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const IAO_DEF: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Leader-selection priority `PREFIX=SCORE` (repeatable). Highest wins.
    #[arg(short = 's', long = "set-prefix")]
    pub set_prefix: Vec<String>,
    /// Label-source priority `PREFIX=SCORE` (repeatable).
    #[arg(short = 'l', long = "label-prefix")]
    pub label_prefix: Vec<String>,
    /// Definition-source priority `PREFIX=SCORE` (repeatable).
    #[arg(short = 'd', long = "definition-prefix")]
    pub definition_prefix: Vec<String>,

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
    let mut merged = merge_equivalent_sets(
        model,
        &parse_prio(&args.set_prefix),
        &parse_prio(&args.label_prefix),
        &parse_prio(&args.definition_prefix),
    )?;
    crate::cmd::maybe_save(&mut merged, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(merged))
}

/// Parse `PREFIX=SCORE` priority arguments into a map.
pub fn parse_prio(args: &[String]) -> HashMap<String, i64> {
    args.iter()
        .filter_map(|s| s.split_once('='))
        .filter_map(|(p, v)| v.trim().parse::<i64>().ok().map(|n| (p.trim().to_string(), n)))
        .collect()
}

/// Merge cliques of equivalent named classes. Pure core.
pub fn merge_equivalent_sets(
    model: Model,
    set_prio: &HashMap<String, i64>,
    label_prio: &HashMap<String, i64>,
    def_prio: &HashMap<String, i64>,
) -> Result<Model> {
    // 1. Classify and take the inferred equivalence cliques. An unsatisfiable
    //    class means the merge would conflate everything below it — refuse.
    crate::reason::el::set_whelk_mode(false);
    let (equiv_pairs, unsat) = {
        let r = crate::reason::el::Reasoner::classify(&model);
        (r.equivalent_class_pairs(), r.unsatisfiable())
    };
    if !unsat.is_empty() {
        anyhow::bail!(
            "merge-equivalent-sets: ontology contains {} unsatisfiable class(es), e.g. {}",
            unsat.len(),
            unsat.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        );
    }
    let mut uf = UnionFind::default();
    for (a, b) in &equiv_pairs {
        uf.union(a, b);
    }
    // 2. Group members by component, each clique in NODE order — the member
    //    iteration order of the reasoner node the election walks (see
    //    `owlapi_hash::class_node_order`). The leader election is
    //    order-sensitive on ties, so this order is part of the contract.
    let mut cliques: HashMap<String, Vec<String>> = HashMap::new();
    for m in uf.members() {
        cliques.entry(uf.find(&m)).or_default().push(m);
    }
    for members in cliques.values_mut() {
        members.sort();
        let order = crate::owlapi_hash::class_node_order(members);
        let reordered: Vec<String> = order.into_iter().map(|i| members[i].clone()).collect();
        *members = reordered;
    }

    // Per-class label/definition from the original model.
    let (labels, defs) = annotation_index(&model);

    // 3. For each non-trivial clique, choose a leader and label/def source.
    let mut rename: HashMap<String, String> = HashMap::new();
    let mut leader_label: HashMap<String, Annotated> = HashMap::new();
    let mut leader_def: HashMap<String, Annotated> = HashMap::new();
    let mut leaders: Vec<String> = Vec::new();
    for members in cliques.values() {
        if members.len() < 2 {
            continue;
        }
        let leader = pick(members, set_prio);
        leaders.push(leader.clone());
        for m in members {
            if *m != leader {
                rename.insert(m.clone(), leader.clone());
            }
        }
        // Label/def from the highest-priority member that actually has one.
        if let Some(l) = pick_annotation(members, label_prio, &labels) {
            leader_label.insert(leader.clone(), l);
        }
        if let Some(d) = pick_annotation(members, def_prio, &defs) {
            leader_def.insert(leader.clone(), d);
        }
    }

    status!(
        "merge-equivalent-sets: merged {} class(es) into {} leader(s)",
        rename.len(),
        leaders.len()
    );

    // 4. Rewrite all members → their leader. The rename rewrites ENTITY
    //    references, not annotation IRI VALUES: an assertion pointing AT a
    //    merged class keeps pointing at the old IRI (its subject still moves).
    let mut model = model;

    // A document-shared node dies where the merge folds a PARALLEL edge onto
    // it: when a merged member of the node's owner asserts `member ⊑ ∃p.F'`
    // and F' merges into the node's filler, the fold rebuilds that owner's
    // edge and the node stops being one object across its owners — each keeps
    // a per-owner copy. Drop the share evidence for exactly those (owner, key)
    // pairs, so the writer renders them per owner.
    {
        let mut partners: HashMap<&str, Vec<&str>> = HashMap::new();
        for (member, leader) in &rename {
            partners.entry(leader.as_str()).or_default().push(member.as_str());
        }
        let mut edges: std::collections::HashSet<(String, String, String)> =
            std::collections::HashSet::new();
        for ac in model.ont.iter() {
            if let Component::SubClassOf(sc) = &ac.component {
                if let (CE::Class(sub), CE::ObjectSomeValuesFrom { ope, bce }) =
                    (&sc.sub, &sc.sup)
                {
                    if let (
                        horned_owl::model::ObjectPropertyExpression::ObjectProperty(p),
                        CE::Class(f),
                    ) = (ope, &**bce)
                    {
                        edges.insert((
                            sub.0.as_ref().to_string(),
                            p.0.as_ref().to_string(),
                            f.0.as_ref().to_string(),
                        ));
                    }
                }
            }
        }
        let mut drop: Vec<(String, String)> = Vec::new();
        for (owner, keys) in &model.owl_shared_owners {
            let Some(ops) = partners.get(owner.as_str()) else { continue };
            for k in keys {
                let Some((prop, filler)) = k.split_once('\u{1}') else { continue };
                let Some(fps) = partners.get(filler) else { continue };
                let collides = ops.iter().any(|op| {
                    fps.iter().any(|fp| {
                        edges.contains(&(op.to_string(), prop.to_string(), fp.to_string()))
                    })
                });
                if collides {
                    drop.push((owner.clone(), k.clone()));
                }
            }
        }
        if !drop.is_empty() {
            status!("merge-equivalent-sets: {} shared node(s) fold per-owner", drop.len());
        }
        for (owner, k) in drop {
            if let Some(keys) = model.owl_shared_owners.get_mut(&owner) {
                keys.remove(&k);
            }
            model.cross_shared.remove(&format!("{owner}\u{1}{k}"));
        }
    }
    let mut value_holders: Vec<horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>> =
        Vec::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let AnnotationValue::IRI(v) = &aa.ann.av {
                if rename.contains_key(v.as_ref()) {
                    value_holders.push(ac.clone());
                }
            }
        }
    }
    for ac in &value_holders {
        model.ont.remove(ac);
    }
    let mut renamed = crate::cmd::rename::rename_model(model, &rename)?;
    {
        let b: horned_owl::model::Build<horned_owl::model::RcStr> =
            horned_owl::model::Build::new();
        for ac in value_holders {
            let Component::AnnotationAssertion(aa) = &ac.component else { unreachable!() };
            let subject = match &aa.subject {
                AnnotationSubject::IRI(s) => AnnotationSubject::IRI(
                    b.iri(rename.get(s.as_ref()).map(|x| x.as_str()).unwrap_or(s.as_ref())),
                ),
                other => other.clone(),
            };
            renamed.ont.insert(horned_owl::model::AnnotatedComponent {
                component: Component::AnnotationAssertion(
                    horned_owl::model::AnnotationAssertion {
                        subject,
                        ann: aa.ann.clone(),
                    },
                ),
                ann: ac.ann.clone(),
            });
        }
    }

    // 5. Drop reflexive axioms produced by the merge, and fix up leader
    //    labels/definitions to the chosen source.
    let mut ont = SetOntology::new();
    for ac in renamed.ont.iter() {
        match &ac.component {
            // An equivalence whose members collapsed to a single distinct
            // expression (named or anonymous) says nothing — drop it.
            Component::EquivalentClasses(eq) if distinct_count(&eq.0) < 2 => continue,
            // Reflexive subsumptions, over any expression shape.
            Component::SubClassOf(sc) if sc.sub == sc.sup => continue,
            // Remove leader label/def assertions; the chosen ones are re-added
            // below CARRYING THEIR OWN ANNOTATIONS. Re-stating them bare instead
            // stripped the provenance off every definition and label the merge
            // touched — `composite-metazoan.owl` lost 37,046 annotated
            // `IAO_0000115` assertions and 29,004 annotated `hasExactSynonym`,
            // keeping the text and dropping the `oboInOwl:hasDbXref` that says
            // where it came from. The assertion count was right, so only the
            // reified-axiom count showed it.
            Component::AnnotationAssertion(aa) => {
                // Every `oboInOwl:id` assertion goes — the merge invalidates the
                // OBO ids of collapsed cliques, and the cleanup removes the lot
                // rather than tracking which survived (69,039 axioms of the
                // composite chain's diff were exactly these).
                if aa.ann.ap.0.as_ref() == "http://www.geneontology.org/formats/oboInOwl#id" {
                    continue;
                }
                if let AnnotationSubject::IRI(iri) = &aa.subject {
                    let s = iri.to_string();
                    let p = aa.ann.ap.0.as_ref();
                    if (p == RDFS_LABEL && leader_label.contains_key(&s))
                        || (p == IAO_DEF && leader_def.contains_key(&s))
                    {
                        continue;
                    }
                }
            }
            _ => {}
        }
        ont.insert(ac.clone());
    }
    // Each leader carries a cross-reference naming every class merged into it.
    let b = &renamed.build;
    const XREF: &str = "http://www.geneontology.org/formats/oboInOwl#hasDbXref";
    // A cross-reference the leader ALREADY carries is not added again: the
    // existing assertion — parsed, so a plain literal — stands, and only a
    // genuinely new id enters as a constructed `xsd:string` one (which is why
    // the two sort into different places in a frame).
    let mut existing_xrefs: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for ac in renamed.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == XREF {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(l)) =
                    (&aa.subject, &aa.ann.av)
                {
                    let text = match l {
                        Literal::Simple { literal }
                        | Literal::Language { literal, .. }
                        | Literal::Datatype { literal, .. } => literal.clone(),
                    };
                    existing_xrefs.insert((s.to_string(), text));
                }
            }
        }
    }
    for (member, leader) in &rename {
        if existing_xrefs.contains(&(leader.clone(), obo_id(member))) {
            continue;
        }
        ont.insert(horned_owl::model::AnnotatedComponent {
            component: Component::AnnotationAssertion(ann_assert(
                b,
                leader,
                XREF,
                // A CONSTRUCTED literal is `xsd:string`-typed. It renders
                // identically to a plain one, but sorts after every plain
                // literal of the same property — which is where the merge
                // cross-references land in a frame.
                &Literal::Datatype {
                    literal: obo_id(member),
                    datatype_iri: b.iri(XSD_STRING),
                },
            )),
            ann: Default::default(),
        });
    }
    // Re-add the chosen label/definition for each leader.
    for (iri, (label, anns)) in &leader_label {
        ont.insert(horned_owl::model::AnnotatedComponent {
            component: Component::AnnotationAssertion(ann_assert(b, iri, RDFS_LABEL, label)),
            ann: anns.clone(),
        });
    }
    for (iri, (def, anns)) in &leader_def {
        ont.insert(horned_owl::model::AnnotatedComponent {
            component: Component::AnnotationAssertion(ann_assert(b, iri, IAO_DEF, def)),
            ann: anns.clone(),
        });
    }

    let mut out = Model::from_parts(ont, renamed.prefixes);
    // The merge rewrites axioms but the untouched majority keeps its blank-node
    // identity: the source-scan and provenance evidence travels with the model,
    // exactly as `filter` carries it. Rebuilding without these renders every
    // shared anonymous node split back into per-owner copies.
    out.banner_labels = renamed.banner_labels;
    out.import_order = renamed.import_order;
    out.explicit_prefixes = renamed.explicit_prefixes;
    out.owl_genid_refs = renamed.owl_genid_refs;
    out.owl_label_order = renamed.owl_label_order;
    out.owl_anon_blocks = renamed.owl_anon_blocks;
    out.closure_ann_ns = renamed.closure_ann_ns;
    out.closure_declared = renamed.closure_declared;
    out.shared_anon = renamed.shared_anon;
    out.span_shared = renamed.span_shared;
    out.cross_shared = renamed.cross_shared;
    out.owl_shared_owners = renamed.owl_shared_owners;
    Ok(out)
}

fn ann_assert(
    b: &horned_owl::model::Build<horned_owl::model::RcStr>,
    subj: &str,
    prop: &str,
    value: &Literal<horned_owl::model::RcStr>,
) -> horned_owl::model::AnnotationAssertion<horned_owl::model::RcStr> {
    horned_owl::model::AnnotationAssertion {
        subject: AnnotationSubject::IRI(b.iri(subj)),
        ann: horned_owl::model::Annotation { ann: Default::default(),
            ap: b.annotation_property(prop),
            av: AnnotationValue::Literal(value.clone()),
        },
    }
}

/// Choose the clique leader. Walking members in IRI order: while no member has
/// scored yet, each member in turn takes the lead (so an all-unscored clique
/// ends with its last member); once a member scores, only a strictly higher
/// score displaces it. Net effect for a scored clique: the first member, in IRI
/// order, holding the maximal score.
fn pick(members: &[String], prio: &HashMap<String, i64>) -> String {
    let mut leader = &members[0];
    let mut best: Option<i64> = None;
    for m in members {
        let s = score(m, prio);
        let wins = match (s, best) {
            (_, None) => true,
            (Some(s), Some(b)) => s > b,
            (None, Some(_)) => false,
        };
        if wins {
            leader = m;
            best = s;
        }
    }
    leader.clone()
}

/// The label/definition of the leading member that has one, elected by the same
/// walk as `pick` but only over members carrying the annotation.
fn pick_annotation(
    members: &[String],
    prio: &HashMap<String, i64>,
    index: &HashMap<String, Annotated>,
) -> Option<Annotated> {
    let annotated: Vec<String> =
        members.iter().filter(|m| index.contains_key(*m)).cloned().collect();
    if annotated.is_empty() {
        return None;
    }
    index.get(&pick(&annotated, prio)).cloned()
}

/// The priority score of the first configured prefix (in sorted order — at most
/// one can match an OBO IRI) found in the IRI. A `http…` prefix matches from the
/// start; a bare prefix matches after a `/` or `#`, so `CL` covers
/// `…/CL_0000814` wherever the class lives.
fn score(iri: &str, prio: &HashMap<String, i64>) -> Option<i64> {
    let mut keys: Vec<&String> = prio.keys().collect();
    keys.sort();
    for p in keys {
        let hit = if p.starts_with("http") {
            iri.starts_with(p.as_str())
        } else {
            iri.contains(&format!("/{p}")) || iri.contains(&format!("#{p}"))
        };
        if hit {
            return Some(prio[p]);
        }
    }
    None
}

/// `http://…/CL_0002438` → `CL:0002438`, the OBO id recorded as the merge xref.
fn obo_id(iri: &str) -> String {
    let frag = iri.rsplit(['/', '#']).next().unwrap_or(iri);
    match frag.split_once('_') {
        Some((p, n)) => format!("{p}:{n}"),
        None => iri.to_string(),
    }
}

/// How many distinct expressions an axiom's member list holds.
fn distinct_count(v: &[CE<horned_owl::model::RcStr>]) -> usize {
    let mut seen: Vec<&CE<horned_owl::model::RcStr>> = Vec::new();
    for c in v {
        if !seen.contains(&c) {
            seen.push(c);
        }
    }
    seen.len()
}

/// The text of an annotation together with the annotations ON that assertion —
/// a definition's `oboInOwl:hasDbXref` provenance, a synonym's type. Carrying the
/// pair is what lets the chosen assertion be re-stated without being stripped.
type Annotated = (
    Literal<horned_owl::model::RcStr>,
    std::collections::BTreeSet<horned_owl::model::Annotation<horned_owl::model::RcStr>>,
);

/// Build `iri → label` and `iri → definition` maps from the model.
fn annotation_index(model: &Model) -> (HashMap<String, Annotated>, HashMap<String, Annotated>) {
    let mut labels = HashMap::new();
    let mut defs = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let (AnnotationSubject::IRI(iri), AnnotationValue::Literal(lit)) =
                (&aa.subject, &aa.ann.av)
            {
                match aa.ann.ap.0.as_ref() {
                    RDFS_LABEL => {
                        labels.insert(iri.to_string(), (lit.clone(), ac.ann.clone()));
                    }
                    IAO_DEF => {
                        defs.insert(iri.to_string(), (lit.clone(), ac.ann.clone()));
                    }
                    _ => {}
                }
            }
        }
    }
    (labels, defs)
}

/// Minimal union-find over string keys.
#[derive(Default)]
struct UnionFind {
    parent: HashMap<String, String>,
}

impl UnionFind {
    fn find(&mut self, x: &str) -> String {
        let p = self.parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if p == x {
            return p;
        }
        let root = self.find(&p);
        self.parent.insert(x.to_string(), root.clone());
        root
    }
    fn union(&mut self, a: &str, b: &str) {
        self.parent.entry(a.to_string()).or_insert_with(|| a.to_string());
        self.parent.entry(b.to_string()).or_insert_with(|| b.to_string());
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent.insert(ra, rb);
        }
    }
    fn members(&self) -> Vec<String> {
        self.parent.keys().cloned().collect()
    }
}
