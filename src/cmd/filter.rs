//! `filter` — keep the axioms a selection selects, in a new ontology. How the
//! terms and `--select` groups choose objects, and how an axiom is judged
//! against them, is [`crate::cmd::objects`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{AnnotatedComponent, Component, MutableOntology, OntologyID, RcStr};

use crate::cmd::objects::{self, Judge, Request, Selection};
use crate::cmd::remove::TermOptions;
use crate::cmd::{select, Switch};
use crate::model::Model;

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
    /// Selector groups, applied in turn: each value's space-separated
    /// selectors (`self`, `classes`, `parents`, `anonymous`, `complement`,
    /// `PROP=VALUE`, `<IRI-pattern>`, …) select the union of what each maps the
    /// set to. `annotations`, `imports` and `ontology` ask for the kept
    /// entities' annotation assertions, the imports and the ontology
    /// annotations.
    #[arg(short = 's', long)]
    pub select: Vec<String>,
    /// Axiom types to keep: `all` (the default), `logical`, `annotation`,
    /// `subclass`, `subproperty`, `equivalent`, `disjoint`, `type`, `abox`,
    /// `tbox`, `rbox`, `declaration`, `structural-tautologies`, one axiom type by
    /// name, or `internal`/`external` to the `--base-iri` namespaces.
    #[arg(short = 'a', long)]
    pub axioms: Vec<String>,
    /// If false, do not preserve hierarchical relationships (`<bool>`).
    #[arg(short = 'p', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::SwitchParser)]
    pub preserve_structure: Option<crate::cmd::Switch>,
    /// If true (the default), keep the axioms all of whose objects are
    /// selected; if false, those with any selected object (`<bool>`).
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
    /// Drop the annotations of the kept axioms: `PROP` drops a property's,
    /// `PROP=VALUE` those with that value and `PROP=~REGEX` those whose value
    /// the regex finds (`'` taken out of either); `all` drops every one.
    /// Repeatable.
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
    /// The IRI of the output ontology, taken as written; the input's when absent.
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
            signature: self.signature.clone(),
            trim: self.trim.clone(),
            allow_punning: self.allow_punning.clone(),
            preserve_structure: self.preserve_structure.clone(),
            whole_ontology_without_terms: false,
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
        set_ontology_iri(&mut kept, iri)?;
    }
    crate::cmd::maybe_save(&mut kept, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(kept))
}

/// In-process entry point (`extract`'s subset path and the in-memory API). With
/// `signature_opt == Some(true)` an axiom is kept when every IRI it names is
/// selected; otherwise when any of its objects is.
pub fn filter(
    model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    signature_opt: Option<bool>,
) -> Result<crate::model::Model> {
    let opts = TermOptions {
        signature: signature_opt.map(Switch::Bool),
        trim: Some(Switch::Bool(signature_opt == Some(true))),
        ..TermOptions::default()
    };
    filter_with(model, term, term_file, select, &[], &[], &opts)
}

/// Keep what a selection selects (pure core). The objects are what the terms
/// name — or, with no term naming an IRI, every object of the ontology —
/// mapped through each `--select` group in turn ([`objects::objects`]). An
/// axiom is kept when every one of its objects is selected (`--trim true`, the
/// default) or any is (`--trim false`), judged by the IRIs it names alone
/// under `--signature true`; one whose annotations are not all selected is kept
/// without them. The hierarchy among the kept objects is re-asserted across
/// what is left out, and `--select annotations` adds every annotation
/// assertion on a kept entity. The result is a new ontology with the input's
/// IRI and no version IRI, its imports under `--select imports` and its
/// annotations under `--select ontology`; only the input's own axioms are read.
pub fn filter_with(
    model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    axioms: &[String],
    base_iri: &[String],
    opts: &TermOptions,
) -> Result<crate::model::Model> {
    let terms = select::collect_terms(&model, term, term_file)?;
    let include = select::collect_terms(&model, &opts.include_term, &opts.include_terms)?;
    let exclude = select::collect_terms(&model, &opts.exclude_term, &opts.exclude_terms)?;

    // `annotations`, `imports` and `ontology` are the command's own words, taken
    // out of the group that names them; a group left empty selects nothing more.
    let values: Vec<String> = if select.is_empty() { vec!["self".to_string()] } else { select.to_vec() };
    let (mut annotations, mut keep_imports, mut keep_ontology) = (false, false, false);
    let mut groups: Vec<Vec<String>> = Vec::new();
    for value in &values {
        let mut group = objects::split_selects(value);
        for (word, flag) in
            [("annotations", &mut annotations), ("imports", &mut keep_imports), ("ontology", &mut keep_ontology)]
        {
            if let Some(i) = group.iter().position(|s| s == word) {
                *flag = true;
                group.remove(i);
            }
        }
        if !group.is_empty() {
            groups.push(group);
        }
    }
    let drops = crate::cmd::remove::AnnotationDrops::read(&model, &opts.drop_axiom_annotations)?;
    let axiom_selectors = objects::axiom_selectors(axioms);
    let base = objects::base_namespaces(&model, base_iri);

    let mut kept: Vec<AnnotatedComponent<RcStr>> = Vec::new();
    let mut bridges: Vec<Component<RcStr>> = Vec::new();
    let mut span_shared = HashMap::new();
    let mut cross_add = HashMap::new();
    {
        let sel = Selection::new(&model);
        let request = Request {
            terms: &terms,
            include: &include,
            exclude: &exclude,
            groups: &groups,
            selected: !select.is_empty(),
            axiom_selectors: &axiom_selectors,
            allow_punning: Switch::read(opts.allow_punning.as_ref(), "allow-punning", false)?,
        };
        let chosen = objects::objects(&sel, &request)?;
        if !chosen.is_empty() {
            let judge = Judge {
                selectors: &axiom_selectors,
                base: &base,
                partial: !Switch::read(opts.trim.as_ref(), "trim", true)?,
                named_only: Switch::read(opts.signature.as_ref(), "signature", false)?,
                annotation_values: opts.annotation_values.unwrap_or(true),
                plain: objects::plain_datatype(&model),
            };
            kept = objects::judge_axioms(sel.axioms(), &chosen, &judge)?;
            if Switch::read(opts.preserve_structure.as_ref(), "preserve-structure", true)? {
                bridges = span_kept(&model, &chosen, &axiom_selectors, &base, &mut span_shared, &mut cross_add);
            }
            if annotations {
                kept.extend(sel.annotation_axioms(&chosen));
            }
        }
    }

    let mut model = model;
    model.cross_shared.extend(cross_add);
    model.span_shared.extend(span_shared);
    let mut kept = {
        use horned_owl::model::MutableOntology;
        use horned_owl::ontology::set::SetOntology;
        let mut ont = SetOntology::new();
        for ac in model.ont.iter().filter(|ac| !model.imported_components.contains(*ac)) {
            match &ac.component {
                // `filter` builds a NEW ontology from the retained axioms, and a
                // new ontology is not the release the input was: it keeps the
                // ontology IRI and carries no version IRI. EFO's
                // `tmp/mirror-efo.owl` is `merge … filter --term EFO:0001444`, and
                // the version IRI it inherits from `efo-dl.owl`
                // (`…/releases/v3.93.0/efo.owl`) is not in the ODK's output.
                Component::OntologyID(id) => {
                    ont.insert(AnnotatedComponent {
                        component: Component::OntologyID(OntologyID { iri: id.iri.clone(), viri: None }),
                        ann: ac.ann.clone(),
                    });
                }
                Component::DocIRI(_) => {
                    ont.insert(ac.clone());
                }
                Component::OntologyAnnotation(_) if keep_ontology => {
                    ont.insert(ac.clone());
                }
                Component::Import(_) if keep_imports => {
                    ont.insert(ac.clone());
                }
                _ => {}
            }
        }
        for ac in kept {
            ont.insert(ac);
        }
        for b in bridges {
            ont.insert(b);
        }
        if keep_imports {
            for iri in &model.inlined_imports {
                ont.insert(Component::Import(horned_owl::model::Import(model.build.iri(iri.as_str()))));
            }
        }
        let mut out = crate::model::Model::from_parts(ont, model.prefixes);
        // The banner labels are NOT carried: a filter keeps a subset, and a
        // label it dropped is not one the written document has. The writer
        // names each section from what it writes, and from the closure where
        // imports remain.
        out.import_order = model.import_order;
        // NOT the document format's prefix map. `filter` builds a NEW ontology from
        // the retained axioms, and a new ontology has a fresh format — so its
        // `rdf:RDF` xmlns block is rebuilt from the entities alone.
        // On MONDO's mondo-simple chain every step up to and including `remove
        // --select object-properties relax` still declares `xmlns:doap` and
        // `xmlns:protege` (inherited from `reasoned.owl`, where the import closure
        // contributed them — see `Model::imports_closure`), and the output of
        // `filter` declares neither, while keeping every other prefix, all of which
        // some retained entity uses. Carrying them through would add exactly those
        // two `xmlns:`/`idspace:` lines to `mondo-simple.owl` and
        // `mondo-simple.obo`.
        out.explicit_prefixes = model.explicit_prefixes;
        out.format_prefixes_cleared = true;
        // What the command line binds and adds is the command's, not the
        // document's: the new ontology is written with the prefixes it adds.
        out.context = model.context;
        out.added_prefixes = model.added_prefixes;
        out.owl_genid_refs = model.owl_genid_refs;
        out.owl_label_order = model.owl_label_order;
        out.imports_closure = model.imports_closure;
        out.shared_anon = model.shared_anon;
        // The `spanGaps` groups computed above: without them the rebuilt model
        // loses the fact that a set of re-links is one object, and the numbering
        // spends a blank node on each.
        out.span_shared = model.span_shared;
        out.cross_shared = model.cross_shared;
        out.shared_occurrences = model.shared_occurrences;
        out
    };

    drops.apply(&mut kept)?;
    Ok(kept)
}

/// The hierarchy among the kept objects, re-asserted across what the filter
/// leaves out: each kept class's nearest kept superclass expressions and each
/// kept property's nearest kept super-properties, from the ontology's own
/// axioms. Under `--axioms internal` or `external`, the first named, only those
/// whose subject is inside, or outside, the base namespaces.
fn span_kept(
    model: &Model,
    chosen: &HashSet<objects::Obj>,
    axiom_selectors: &[String],
    base: &[String],
    shared: &mut HashMap<String, u64>,
    cross: &mut HashMap<String, u64>,
) -> Vec<Component<RcStr>> {
    let root = crate::cmd::remove::own_ontology(model);
    let bridges = crate::cmd::remove::span_gaps(root.as_ref().unwrap_or(model), chosen, shared, cross);
    let (internal, external) = objects::namespace_flags(axiom_selectors);
    bridges.into_iter().filter(|b| objects::in_namespace(b, internal, external, base)).collect()
}

/// Name the ontology `iri` (`--ontology-iri`), keeping its version IRI.
fn set_ontology_iri(model: &mut Model, iri: &str) -> Result<()> {
    let mut existing: Option<OntologyID<_>> = None;
    for ac in model.ont.iter() {
        if let Component::OntologyID(id) = &ac.component {
            existing = Some(id.clone());
            break;
        }
    }
    let viri = existing.and_then(|id| id.viri);
    let id = crate::model::ontology_id(&model.build, Some(iri), viri.as_deref())?;
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
    Ok(())
}
