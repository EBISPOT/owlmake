//! `filter` — keep only selected axioms (the complement of `remove`). Supports a
//! term whitelist (`--term`/`--term-file`), include/exclude term forcing, and the
//! signature-subset filter that builds the `-basic`/`-simple` products
//! (`--signature true --select "annotations ontology anonymous self"`): keep an
//! axiom when its whole signature lies in the seed, plus annotation/ontology
//! axioms over the seed.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{Component, MutableOntology, OntologyID, RcStr};

use crate::cmd::remove::TermOptions;
use crate::cmd::select;
use crate::model::Model;
use crate::sig;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Terms (IRIs or CURIEs) to keep. Repeatable.
    #[arg(short = 't', long)]
    pub term: Vec<String>,
    /// Files listing terms to keep, one per line.
    #[arg(short = 'T', long)]
    pub term_file: Vec<PathBuf>,
    /// Selectors (space- or repeat-separated): `annotations`, `ontology`,
    /// `anonymous`, `self`, …
    #[arg(short = 's', long)]
    pub select: Vec<String>,
    /// Axiom categories to filter for (accepted for compatibility; advisory).
    #[arg(short = 'a', long)]
    pub axioms: Vec<String>,
    /// If false, do not preserve hierarchical relationships (`<bool>`).
    #[arg(short = 'p', long, num_args = 1, default_missing_value = "true")]
    pub preserve_structure: Option<bool>,
    /// If true, keep axioms containing only selected objects (`<bool>`).
    #[arg(short = 'r', long, num_args = 1, default_missing_value = "true")]
    pub trim: Option<bool>,
    /// Terms to force-exclude (never kept on their own account). Repeatable.
    #[arg(short = 'e', long = "exclude-term", value_name = "TERM")]
    pub exclude_term: Vec<String>,
    /// Files of terms to force-exclude. Repeatable.
    #[arg(short = 'E', long = "exclude-terms", value_name = "FILE")]
    pub exclude_terms: Vec<PathBuf>,
    /// Terms to force-include in the seed. Repeatable.
    #[arg(short = 'n', long = "include-term", value_name = "TERM")]
    pub include_term: Vec<String>,
    /// Files of terms to force-include in the seed. Repeatable.
    #[arg(short = 'N', long = "include-terms", value_name = "FILE")]
    pub include_terms: Vec<PathBuf>,
    /// Drop axiom annotations involving a particular annotation property, or
    /// `all`/`true` to strip every annotation from kept axioms.
    #[arg(short = 'd', long = "drop-axiom-annotations", value_name = "ARG")]
    pub drop_axiom_annotations: Option<String>,
    /// Signature mode: when true keep an axiom if ANY of its entities is in the
    /// seed; when false (the OBO sub-ontology filter) keep only when *all* of its
    /// entities are in the seed (`<bool>`).
    #[arg(short = 'S', long, num_args = 1, default_missing_value = "true")]
    pub signature: Option<bool>,
    /// If true, allow selecting punned entities (`<bool>`).
    #[arg(long = "allow-punning", num_args = 1, default_missing_value = "true")]
    pub allow_punning: Option<bool>,
    /// Base IRI(s). Accepted for compatibility.
    #[arg(long = "base-iri", value_name = "IRI")]
    pub base_iri: Vec<String>,
    /// Set the output ontology IRI.
    #[arg(short = 'O', long = "ontology-iri", value_name = "IRI")]
    pub ontology_iri: Option<String>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
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
            signature: self.signature,
            trim: self.trim,
            allow_punning: self.allow_punning,
            // `--preserve-structure` defaults to true: a filtered product keeps its
            // hierarchy connected across the classes it drops. Resolved here so the
            // value this CLI path passes is explicit, not so it differs — the core
            // applies the same default (`unwrap_or(true)`), as `remove` does. The
            // in-process `filter()` entry point builds its own
            // `TermOptions::default()`, leaving this `None`, and bridges for that
            // reason rather than this one.
            preserve_structure: Some(self.preserve_structure.unwrap_or(true)),
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
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut kept = filter_with(
        model,
        &args.term,
        &args.term_file,
        &args.select,
        &args.axioms,
        &args.base_iri,
        &args.term_options(),
    )?;
    if let Some(iri) = &args.ontology_iri {
        set_ontology_iri(&mut kept, iri);
    }
    crate::cmd::maybe_save(&mut kept, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(kept))
}

/// How an axiom's signature is matched against the seed.
#[derive(Clone, Copy)]
enum SigMode {
    /// Keep when EVERY signature entity is in the seed (the OBO sub-ontology
    /// filter, which is what [`filter`]'s `signature_opt == Some(true)` selects).
    All,
    /// Keep when ANY signature entity is in the seed.
    Any,
}

/// In-process entry point (used by `extract`'s subset path and the in-memory
/// API). Here `signature_opt == Some(true)` means "whole signature within the
/// seed" (the `-simple`/`-basic` filter); anything else means "mentions any".
///
/// Like the CLI path, it does NOT exempt the built-in vocabulary from the seed
/// test: exempting it would keep 27,279 annotation assertions on IRIs outside
/// MONDO's simple seed.
pub fn filter(
    model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    signature_opt: Option<bool>,
) -> Result<crate::model::Model> {
    let mode = if signature_opt.unwrap_or(false) { SigMode::All } else { SigMode::Any };
    // `signature` must reach the options too: it decides whether a literal-valued
    // axiom annotation passes the keep-whole test in the kept loop below.
    let opts = TermOptions { signature: signature_opt, ..TermOptions::default() };
    filter_core(model, term, term_file, select, &[], &[], &opts, mode, false)
}

/// CLI entry point. The default keeps axioms whose whole signature is selected;
/// `--trim false` widens it to axioms whose signature contains ANY selected
/// entity.
pub fn filter_with(
    model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    axioms: &[String],
    base_iri: &[String],
    opts: &TermOptions,
) -> Result<crate::model::Model> {
    // `--trim` decides the match and defaults to TRUE, so the default is the
    // COMPLETE match — every entity of the axiom's signature must be selected — and
    // `--trim false` is what widens it to "uses at least one". Keying the mode off
    // `--signature` instead (true → any-entity) would leave MONDO's
    // `mondo-simple.owl` carrying 27,279 annotation assertions on IRIs outside the
    // seed.
    let mode = if opts.trim.unwrap_or(true) { SigMode::All } else { SigMode::Any };
    filter_core(model, term, term_file, select, axioms, base_iri, opts, mode, false)
}

/// Apply the `filter` selectors to `model` (pure core).
#[allow(clippy::too_many_arguments)]
fn filter_core(
    mut model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    axioms: &[String],
    base_iri: &[String],
    opts: &TermOptions,
    mode: SigMode,
    exempt_builtins: bool,
) -> Result<crate::model::Model> {
    // Effective seed = (--term ∪ --include-term) \ --exclude-term.
    // Each of the three lists is resolved to ENTITIES, so a punned or unknown IRI
    // silently drops out of all of them — see `select::resolve_entity_terms`.
    let ent = |t| select::resolve_entity_terms(&model, t);
    let mut terms = ent(select::collect_terms(&model, term, term_file)?);
    let included = ent(select::collect_terms(&model, &opts.include_term, &opts.include_terms)?);
    terms.extend(included);
    let excluded = ent(select::collect_terms(&model, &opts.exclude_term, &opts.exclude_terms)?);

    // --base-iri: every entity under one of the base namespaces joins the seed, so
    // the base "module" (and every axiom over it) is kept.
    let base_iris: Vec<String> = base_iri.iter().map(|b| select::expand(&model, b)).collect();
    if !base_iris.is_empty() {
        for e in select::entities(&model).all() {
            if base_iris.iter().any(|b| e.starts_with(b.as_str())) {
                terms.insert(e.clone());
            }
        }
    }

    let selects: HashSet<String> = select
        .iter()
        .flat_map(|s| s.split_whitespace())
        .map(str::to_string)
        .collect();
    let keep_ontology = selects.contains("ontology");

    // Entity/relation selectors EXPAND the seed before filtering: a type selector
    // (`object-properties`, `classes`, …) adds every entity of that type; a
    // relation selector (`parents`, `ancestors`, `children`, `descendants`,
    // `equivalents`, `types`, `instances`, `domains`, `ranges`) adds related
    // entities. This is how a `-simple` product keeps the seed classes plus their
    // parents and all object properties (e.g. WBls: `--select "… parents
    // object-properties self"`). `complement` inverts the resulting selection.
    let mut complement_select = false;
    {
        // Signature rather than declarations, so an entity that is only referenced
        // still selects — see the note on the same call in `cmd::remove`.
        let ent = select::signature_entities(&model);
        let mut extra: HashSet<String> = HashSet::new();
        for kw in &selects {
            if let Some(set) = select::category_members(&ent, kw) {
                extra.extend(set);
            } else {
                match kw.as_str() {
                    "parents" => extra.extend(select::direct_parents(&model, &terms)),
                    "ancestors" => extra.extend(select::ancestors(&model, &terms)),
                    "children" => extra.extend(select::direct_children(&model, &terms)),
                    "descendants" => extra.extend(select::descendants(&model, &terms)),
                    "equivalents" => extra.extend(select::equivalents_of(&model, &terms)),
                    "types" => extra.extend(select::types_of(&model, &terms)),
                    "instances" => extra.extend(select::instances_of(&model, &terms)),
                    "domains" => extra.extend(select::domains_of(&model, &terms)),
                    "ranges" => extra.extend(select::ranges_of(&model, &terms)),
                    "complement" => complement_select = true,
                    // `PROP=VALUE`: every entity carrying that annotation. UBERON's
                    // `cumbo` subset is built by exporting the IDs this selects
                    // (`--select 'oboInOwl:inSubset=uberon:cumbo'`) and extracting
                    // them. Unhandled, the token fell through to the no-op arm
                    // below, leaving an empty seed that then selected the WHOLE
                    // ontology: the term list came out with 16,417 IDs instead of
                    // 14, and `subsets/cumbo.owl` at 48 MB instead of 123 KB.
                    _ if select::parse_annotation_value(kw).is_some() => {
                        let (p, v) = select::parse_annotation_value(kw).unwrap();
                        extra.extend(select::annotation_value_members(&model, p, v));
                    }
                    // `named`/`anonymous`: owlmake selects entities by IRI, so
                    // every selectable entity is named; `named` is a no-op pass
                    // and `anonymous` selects nothing extra.
                    _ => {}
                }
            }
        }
        terms.extend(extra);
    }

    // `--select complement`: invert the selection so the seed becomes every
    // declared entity NOT currently selected.
    if complement_select {
        let inverted: HashSet<String> = select::entities(&model)
            .all()
            .filter(|e| !terms.contains(*e))
            .cloned()
            .collect();
        terms = inverted;
    }

    // With no `--term`/`--term-file` and no selector to narrow it, the object set is
    // the whole ontology, so `filter --axioms <types>` on its own keeps every axiom
    // of those types rather than nothing.
    //
    // "The whole ontology" is its ENTITIES. An IRI that appears only as an
    // annotation subject or value is not one — nothing declares it and no logical
    // axiom mentions it — so an assertion pointing at such an IRI is not wholly
    // within the object set and falls out of the signature test below. That is what
    // leaves a `-basic` composite holding the assertions whose value names a real
    // entity. Materialising the seed here also means nothing counts as removed, so
    // the gap-spanning pass below has no gaps to bridge.
    if terms.is_empty() && term.is_empty() && term_file.is_empty() {
        terms = select::signature_entities(&model).all().cloned().collect();
    }

    // `--exclude-term`/`--exclude-terms` names what must NOT be selected, and it
    // is the last word: it applies after the selectors have expanded the seed and
    // after the whole-ontology default above, so an excluded entity cannot be
    // reintroduced by either. Applying it earlier left `filter --exclude-terms F`
    // — no `--term` at all — selecting everything, F included.
    for e in &excluded {
        terms.remove(e);
    }

    // --preserve-structure (default true): bridge the hierarchy across the classes
    // being dropped (everything not in the kept seed) so a kept subclass still
    // connects to its nearest kept superclass expression. Reuses `remove`'s
    // `spanGaps` with the *removed* set = declared entities − seed, then keeps the
    // bridge axioms (their signature lies in the seed). The default is on, matching
    // `remove`, so an unset option bridges rather than silently flattening.
    if opts.preserve_structure.unwrap_or(true) {
        use horned_owl::model::MutableOntology;
        let removed: HashSet<String> = select::entities(&model)
            .all()
            .filter(|e| !terms.contains(*e))
            .cloned()
            .collect();
        let no_exclude = HashSet::new();
        // Same object identity as `remove`'s path: re-links made from one source
        // expression are one blank node, however many classes now carry them.
        let mut span_shared = std::collections::HashMap::new();
        let mut cross_add = std::collections::HashMap::new();
        let bridges = crate::cmd::remove::span_gaps_shared(
            &model,
            &removed,
            &no_exclude,
            None,
            &mut span_shared,
            &mut cross_add,
        );
        model.cross_shared.extend(cross_add);
        for b in bridges {
            model.ont.insert(b);
        }
        if std::env::var("OM_SPAN_LOG").is_ok() {
            let mut v: Vec<_> = span_shared.iter().collect();
            v.sort();
            let mut out = String::new();
            for (k, g) in v {
                out.push_str(&format!("{g}\t{}\n", k.replace('\u{1}', " | ")));
            }
            std::fs::write("/tmp/om_span_groups.txt", out).ok();
        }
        model.span_shared.extend(span_shared);
    }

    // --axioms: restrict to the requested axiom categories
    // (`logical|annotation|subclass|…`). Empty ⇒ no restriction.
    let axiom_toks: Vec<String> = axioms
        .iter()
        .flat_map(|a| a.split_whitespace())
        .map(str::to_string)
        .collect();

    // The signature matched against the seed counts an annotation property as an
    // entity — so `AnnotationAssertion(rdfs:label X "…")` has `rdfs:label` in its
    // signature and is dropped by a seed that does not list it. The `annotations`
    // selector then adds back every annotation assertion whose SUBJECT is selected,
    // which is how each seed class keeps its labels while the assertions on classes
    // outside the seed are dropped.
    let axiom_sig = |comp: &Component<RcStr>| -> HashSet<String> {
        let mut s = sig::signature(comp);
        s.extend(sig::annotation_properties(comp));
        // …and, for an annotation assertion, its SUBJECT and any IRI value. The
        // logical signature has neither (an annotation subject is not an entity),
        // so counting the property alone would decide the axiom's fate — and
        // MONDO's simple seed DOES list `rdfs:label`, which would let all 27,279
        // labels on IRIs outside the seed (`http://identifiers.org/hgnc/…`) through.
        // The seed test covers all three: subject, then property, then value.
        if let Component::AnnotationAssertion(aa) = comp {
            if let horned_owl::model::AnnotationSubject::IRI(i) = &aa.subject {
                s.insert(i.as_ref().to_string());
            }
            if let horned_owl::model::AnnotationValue::IRI(i) = &aa.ann.av {
                s.insert(i.as_ref().to_string());
            }
        }
        s
    };
    let keep_annotations = selects.contains("annotations");
    let annotation_backfill: Vec<horned_owl::model::AnnotatedComponent<RcStr>> = if keep_annotations
    {
        model
            .ont
            .iter()
            .filter(|ac| match &ac.component {
                Component::AnnotationAssertion(aa) => match &aa.subject {
                    horned_owl::model::AnnotationSubject::IRI(i) => {
                        terms.contains(i.as_ref())
                    }
                    _ => false,
                },
                _ => false,
            })
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    let matches_seed = |comp: &Component<RcStr>| -> bool {
        if matches!(comp, Component::OntologyID(_) | Component::DocIRI(_)) {
            return true;
        }
        if matches!(comp, Component::OntologyAnnotation(_)) {
            return keep_ontology;
        }
        if !axiom_category_match(comp, &axiom_toks, &base_iris) {
            return false;
        }
        let sig = axiom_sig(comp);
        match mode {
            SigMode::Any => sig.iter().any(|s| terms.contains(s)),
            // Keep when the whole signature is within the seed, exempting nothing:
            // an `AnnotationAssertion(rdfs:label X "…")` has `rdfs:label` in its
            // signature, and MONDO's simple seed lists only class IRIs plus two
            // oboInOwl properties — so every annotation assertion falls out here and
            // comes back through the `annotations` selector, for selected SUBJECTS
            // only. `exempt_builtins` would waive the OWL/RDF vocabulary, but both
            // callers of this private core pass `false`, so no `filter` run reaches
            // that arm of the test — waiving it would keep 27,279 assertions on IRIs
            // outside the seed.
            SigMode::All => sig
                .iter()
                .all(|s| terms.contains(s) || (exempt_builtins && is_builtin(s))),
        }
    };

    let mut kept = {
        use horned_owl::model::MutableOntology;
        use horned_owl::ontology::set::SetOntology;
        let mut ont = SetOntology::new();
        for ac in model.ont.iter() {
            if !matches_seed(&ac.component) {
                continue;
            }
            // A matched axiom keeps its logical content either way; what differs is
            // its axiom ANNOTATIONS. A COMPLETE match (`SigMode::All`, the default)
            // keeps them only when the annotations themselves also lie in the seed:
            // every annotation property, and every IRI value, must be selected. A
            // LITERAL value is ignored under `--signature true` — which is how the
            // `-simple` products keep their 6,637 `oboInOwl:source`-annotated
            // `subClassOf` reifications, `oboInOwl:source` being in the seed — but
            // fails the test otherwise, so without `--signature` any literal-valued
            // annotation is stripped (a `-basic` composite carries no `owl:Axiom`
            // block at all). The test is all-or-nothing per axiom: one failing
            // annotation strips them all. `--trim false` (`SigMode::Any`) keeps the
            // axiom whole — HPO's `hp-simple-non-classified.owl` filters that way and
            // would lose 15,777 of its 25,529 `owl:Axiom` blocks to stripping.
            // And an assertion the `annotations` selector re-adds keeps its axiom
            // annotations regardless: subject selected ⇒ whole.
            let backfilled = keep_annotations
                && matches!(&ac.component, Component::AnnotationAssertion(aa)
                    if matches!(&aa.subject,
                        horned_owl::model::AnnotationSubject::IRI(i) if terms.contains(i.as_ref())));
            let literal_ok = opts.signature.unwrap_or(false);
            let ann_in_seed = ac.ann.iter().all(|a| {
                terms.contains(a.ap.0.as_ref())
                    && match &a.av {
                        horned_owl::model::AnnotationValue::IRI(i) => terms.contains(i.as_ref()),
                        horned_owl::model::AnnotationValue::Literal(_) => literal_ok,
                        horned_owl::model::AnnotationValue::AnonymousIndividual(_) => false,
                    }
            });
            if ac.ann.is_empty() || matches!(mode, SigMode::Any) || backfilled || ann_in_seed {
                ont.insert(ac.clone());
            } else {
                ont.insert(horned_owl::model::AnnotatedComponent {
                    component: ac.component.clone(),
                    ann: Default::default(),
                });
            }
        }
        for ac in annotation_backfill {
            ont.insert(ac);
        }
        let mut out = crate::model::Model::from_parts(ont, model.prefixes);
        out.banner_labels = model.banner_labels;
        out.import_order = model.import_order;
        // NOT the document format's prefix map. `filter` builds a NEW ontology from
        // the retained axioms, and a new ontology has a fresh format — so its
        // `rdf:RDF` xmlns block is rebuilt from the entities alone.
        // On MONDO's mondo-simple chain every step up to and including `remove
        // --select object-properties relax` still declares `xmlns:doap` and
        // `xmlns:protege` (inherited from `reasoned.owl`, where the import closure
        // contributed them — see `Model::closure_ann_ns`), and the output of
        // `filter` declares neither, while keeping every other prefix, all of which
        // some retained entity uses. Carrying them through would add exactly those
        // two `xmlns:`/`idspace:` lines to `mondo-simple.owl` and
        // `mondo-simple.obo`.
        out.explicit_prefixes = model.explicit_prefixes;
        out.format_prefixes_cleared = true;
        out.owl_genid_refs = model.owl_genid_refs;
        out.owl_label_order = model.owl_label_order;
        out.owl_reif_order = model.owl_reif_order;
        out.owl_anon_blocks = model.owl_anon_blocks;
        out.closure_ann_ns = model.closure_ann_ns;
        out.closure_declared = model.closure_declared;
        out.shared_anon = model.shared_anon;
        // The `spanGaps` groups computed above: without them the rebuilt model
        // loses the fact that a set of re-links is one object, and the numbering
        // spends a blank node on each.
        out.span_shared = model.span_shared;
        out.cross_shared = model.cross_shared;
        out
    };

    if let Some(drop) = &opts.drop_axiom_annotations {
        crate::cmd::remove::drop_axiom_annotations(&mut kept, drop);
    }
    Ok(kept)
}

/// Replace the ontology's OntologyID IRI with `iri`.
fn set_ontology_iri(model: &mut Model, iri: &str) {
    let expanded = select::expand(model, iri);
    let mut existing: Option<OntologyID<_>> = None;
    for ac in model.ont.iter() {
        if let Component::OntologyID(id) = &ac.component {
            existing = Some(id.clone());
            break;
        }
    }
    let mut id = existing.unwrap_or(OntologyID { iri: None, viri: None });
    id.iri = Some(model.build.iri(expanded.as_str()));
    let kept: Vec<_> = model
        .ont
        .iter()
        .filter(|ac| !matches!(ac.component, Component::OntologyID(_)))
        .cloned()
        .collect();
    let mut ont = horned_owl::ontology::set::SetOntology::new();
    for ac in kept {
        ont.insert(ac);
    }
    ont.insert(Component::OntologyID(id));
    model.ont = ont;
}

/// `filter --axioms <category>`: does `comp` belong to one of the requested
/// axiom categories? Declarations and ontology structure are always kept (so the
/// result stays well-formed); an empty request keeps everything. Supports the
/// full category vocabulary (`all`, `logical`, `annotation`, `subclass`,
/// `subproperty`, `equivalent`, `disjoint`, `type`, `tbox`, `abox`, `rbox`,
/// `declaration`, `internal`, `external`).
fn axiom_category_match(
    comp: &Component<horned_owl::model::RcStr>,
    toks: &[String],
    base_iris: &[String],
) -> bool {
    if toks.is_empty() {
        return true;
    }
    // A declaration is an axiom TYPE like any other, so it survives only when the
    // request names it. Retaining declarations unconditionally does not dangle —
    // the writer re-declares whatever the signature holds — but it does keep every
    // entity IN the signature, and that changes what later steps can see.
    //
    // UBERON's `-basic` composites turn on exactly that. After `filter --axioms
    // "subclass equivalent annotation"` a class with annotations and no logical
    // axioms should appear only as an annotation SUBJECT, which is an IRI and not
    // an entity of any axiom's signature — so the later
    // `remove --term rdfs:label --select complement --axioms annotation --trim false`
    // cannot reach its definition and keeps it. The reference keeps 1,512 such
    // definitions and every one of them is on a class with no edge at all. Holding
    // the declarations put those classes back in the signature and took all 1,512.
    let only_namespace = toks.iter().all(|t| t == "internal" || t == "external");
    if select::is_declaration(comp) && only_namespace {
        return toks.iter().any(|t| select::axiom_in_category(comp, t, base_iris));
    }
    toks.iter().any(|t| select::axiom_in_category(comp, t, base_iris))
}

/// Built-in OWL/RDF/RDFS/XSD vocabulary IRIs are not part of a term seed.
fn is_builtin(iri: &str) -> bool {
    iri.starts_with("http://www.w3.org/2002/07/owl#")
        || iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
        || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
        || iri.starts_with("http://www.w3.org/2001/XMLSchema#")
}
