//! `extract` — extract a module for a seed term set by any of the locality-based
//! methods (BOT, TOP, STAR), MIREOT, or the materialized `subset` method.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::bail;
use clap::Args as ClapArgs;

use crate::cmd::select;
use crate::extract::{self, ExtractOptions, Individuals, Intermediates, Method};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output format. `-f` is taken by `--force` on this command, so the format
    /// has no short; use the long `--format`.
    #[arg(long)]
    pub format: Option<String>,

    /// Extraction method: star, top, bot, mireot.
    #[arg(short = 'm', long, default_value = "star")]
    pub method: String,

    /// Seed term to extract (repeatable). For MIREOT these are the lower
    /// terms.
    #[arg(short = 't', long)]
    pub term: Vec<String>,
    /// File(s) listing seed terms (repeatable).
    #[arg(short = 'T', long)]
    pub term_file: Vec<PathBuf>,

    /// MIREOT upper (boundary) term (repeatable).
    #[arg(short = 'u', long)]
    pub upper_term: Vec<String>,
    /// File(s) of MIREOT upper terms (repeatable).
    #[arg(short = 'U', long)]
    pub upper_terms: Vec<PathBuf>,

    /// MIREOT lower term (repeatable; alias for --term under MIREOT).
    #[arg(short = 'l', long)]
    pub lower_term: Vec<String>,
    /// File(s) of MIREOT lower terms (repeatable).
    #[arg(short = 'L', long)]
    pub lower_terms: Vec<PathBuf>,

    /// Branch root term (repeatable): extract the branch rooted here.
    #[arg(short = 'b', long)]
    pub branch_from_term: Vec<String>,
    /// File(s) of branch root terms (repeatable).
    #[arg(short = 'B', long)]
    pub branch_from_terms: Vec<PathBuf>,

    /// Copy the source ontology's ontology-level annotations into the module
    /// (`<bool>`, default false).
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true")]
    pub copy_ontology_annotations: Option<bool>,

    /// Annotate extracted terms with rdfs:isDefinedBy / oboInOwl:source = their
    /// source ontology IRI (`<bool>`, default false).
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true")]
    pub annotate_with_source: Option<bool>,

    /// Handle individuals: include|minimal|definitions|exclude.
    #[arg(short = 'n', long, default_value = "include")]
    pub individuals: String,

    /// Handle imports: include|exclude. owlmake operates on the already-merged
    /// input (it does not follow owl:imports during extract), so `include` is a
    /// no-op and `exclude` drops any owl:imports declarations.
    #[arg(short = 'M', long, default_value = "include")]
    pub imports: String,

    /// Handle intermediate terms: all|minimal|none.
    #[arg(short = 'N', long, default_value = "all")]
    pub intermediates: String,

    /// Mapping file of term→source ontology IRI, one
    /// `TERM<tab/space>SOURCE_IRI` per line, used by --annotate-with-source.
    #[arg(short = 's', long, value_name = "FILE")]
    pub sources: Option<PathBuf>,

    /// Warn (instead of error) when no input terms are given (`<bool>`,
    /// default false).
    #[arg(short = 'f', long, num_args = 1, default_missing_value = "true")]
    pub force: Option<bool>,

    /// Set the module's ontology IRI.
    #[arg(short = 'O', long, value_name = "IRI")]
    pub output_iri: Option<String>,

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

    // -M,--imports exclude: drop owl:imports declarations up front (owlmake never
    // follows them, so `include` needs no special handling).
    if args.imports.eq_ignore_ascii_case("exclude") {
        model = select::retain(model, |c| {
            !matches!(c, horned_owl::model::Component::Import(_))
        });
    }

    // Seed terms: --term/--term-file, plus --lower-term(s) (the MIREOT lower
    // boundary) and --branch-from-term(s) (branch roots are extra seeds).
    let mut seed = select::collect_terms(&model, &args.term, &args.term_file)?;
    let lower = select::collect_terms(&model, &args.lower_term, &args.lower_terms)?;
    seed.extend(lower.iter().cloned());
    // --branch-from-term(s): a branch root pulls in its whole descendant subtree,
    // not just the root term itself.
    let branch = select::collect_terms(&model, &args.branch_from_term, &args.branch_from_terms)?;
    if !branch.is_empty() {
        seed.extend(branch.iter().cloned());
        let desc = descendants(&model, &branch);
        if crate::progress::verbosity() >= 1 {
            status!("extract: --branch-from-term added {} descendant(s)", desc.len());
        }
        seed.extend(desc);
    }

    if seed.is_empty() {
        if args.force.unwrap_or(false) {
            status!("extract: WARNING — no input terms; producing an empty module");
        } else {
            bail!("extract requires at least one --term / --lower-term / --branch-from-term (use --force to warn instead)");
        }
    }

    let opts = build_options(&model, args)?;

    let mut result = if args.method.eq_ignore_ascii_case("MIREOT") {
        let upper = select::collect_terms(&model, &args.upper_term, &args.upper_terms)?;
        // MIREOT lower seeds: explicit lower terms, else the generic --term set —
        // minus the branch roots and their subtrees, which do not climb.
        let branch_set: std::collections::HashSet<String> = if branch.is_empty() {
            Default::default()
        } else {
            let mut b: std::collections::HashSet<String> = branch.iter().cloned().collect();
            b.extend(descendants(&model, &branch));
            b
        };
        let lower_seed: std::collections::HashSet<String> = if lower.is_empty() {
            seed.difference(&branch_set).cloned().collect()
        } else {
            lower.clone()
        };
        extract::mireot_with(&model, &lower_seed, &upper, &branch_set, &opts)
    } else if args.method.eq_ignore_ascii_case("subset") {
        subset_module(model, &seed)?
    } else {
        let method = Method::parse(&args.method)
            .ok_or_else(|| anyhow::anyhow!("unknown extract method: {}", args.method))?;
        extract::extract_with(&model, &seed, method, &opts)
    };

    // `extract` builds a NEW `OWLOntology`, so its document format declares no
    // prefixes — the same rule as `filter`, `query --update` and `template` (see
    // `Model::format_prefixes_cleared`). All the writer then declares is `:` plus
    // owl/rdf/xml/xsd/rdfs and the namespaces the module's own annotation and
    // assertion properties pull in; every other IRI is spelled in full, which is
    // the shape released import modules such as OBA's `imports/merged_import.owl`
    // carry. Carrying the input mirror's whole prefix map over instead would
    // abbreviate every namespace it names — 54 MB down to 31 MB for that file —
    // and rewrite every line of every release diff.
    result.format_prefixes_cleared = true;

    // A module is a document in its own right: it holds the axioms the extraction
    // selected, wherever in the closure they came from, and it declares no
    // `owl:imports` of its own. The whole point of extracting from an import is
    // that the module can be consumed WITHOUT that import.
    result.detach_import_closure();

    crate::cmd::maybe_save(&mut result, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(result))
}

/// `--method subset`: a MATERIALIZED sub-ontology holding the seed terms and the
/// relations between them. Not a locality-based module at all — the four steps
/// are:
///
/// 1. `relations` = the seed terms that are object properties in the input;
/// 2. `materialize` the relation graph over just those relations, on top of every
///    axiom the input already has;
/// 3. `filter` that to the seed entities with a COMPLETE signature match — an axiom
///    survives only when every named entity it mentions is a seed term — then add
///    the annotation axioms on those entities and copy the import declarations;
/// 4. copy the annotation axioms of every object, annotation and data property left
///    in the result's signature, then `reduce` to drop subclass axioms the survivors
///    already entail.
fn subset_module(
    model: crate::model::Model,
    seed: &HashSet<String>,
) -> anyhow::Result<crate::model::Model> {
    let sig = select::signature_entities(&model);
    let relations: HashSet<String> =
        seed.iter().filter(|t| sig.object_properties.contains(*t)).cloned().collect();
    if crate::progress::verbosity() >= 1 {
        status!("extract: subset — materializing {} relation(s)", relations.len());
    }
    let source_anns = property_annotation_axioms(&model);
    // No object sharing created while BUILDING the module survives into it. Two
    // steps here make one expression stand for several classes: `materialize`
    // builds one `∃R.D` per (property, filler) and asserts it for every subclass
    // that gets it, and `--preserve-structure` re-links several classes through
    // one source expression. Both are real sharing in the model they were made
    // in — a `materialize` STEP writes exactly that — and neither is sharing
    // here, because a subset module is rebuilt from the axioms the filter keeps
    // and each retained restriction is an object of its own again.
    //
    // Carrying either in leaves the module short of blank nodes: MONDO's
    // `subsets/mondo-rare.owl` carries 363 more than the shared count, and
    // `uberon`'s `subsets/cumbo.owl` three classes bridged through one removed
    // ancestor spend one node instead of three. The offset renders as swapped
    // `owl:Axiom` blocks wherever it crosses a digit-length boundary.
    let span_before = model.span_shared.clone();
    let cross_before = model.cross_shared.clone();
    let materialized = crate::cmd::materialize::materialize(model, &relations);
    let terms: Vec<String> = seed.iter().cloned().collect();
    let mut filtered =
        crate::cmd::filter::filter(materialized, &terms, &[], &["annotations".to_string()], Some(true))?;
    // Property annotations: every annotation axiom the SOURCE ontology has on an
    // object, annotation or data property still in the result's signature. The
    // filter drops them — its seed is the term list, and a property like
    // `IAO_0000115` or `mondo#curated_content_resource` is not in it — so without
    // this step each renders as a bare stub instead of carrying its label, `id`,
    // `hasDbXref`, `is_metadata_tag` …; MONDO's `subsets/mondo-rare.owl` carries
    // 3,326 lines of them.
    {
        use horned_owl::model::MutableOntology;
        let props = property_signature(&filtered);
        for ac in &source_anns {
            if let horned_owl::model::Component::AnnotationAssertion(aa) = &ac.component {
                if let horned_owl::model::AnnotationSubject::IRI(s) = &aa.subject {
                    if props.contains(s.as_ref()) {
                        filtered.ont.insert(ac.clone());
                    }
                }
            }
        }
    }
    let mut out = crate::cmd::reduce::reduce(&filtered);
    out.span_shared = span_before;
    out.cross_shared = cross_before;
    // A materialized subset is a NEW ontology, so it takes no version from its
    // source. Its `<owl:Axiom>` blocks are ordered by the blank-node counter, not
    // by the source document: `reduce` builds its result from parts, which starts
    // `owl_reif_order` empty, and the writer orders reifications by genid anyway.
    {
        use horned_owl::model::{Component, MutableOntology, OntologyID};
        let ids: Vec<_> = out
            .ont
            .iter()
            .filter(|ac| matches!(&ac.component, Component::OntologyID(_)))
            .cloned()
            .collect();
        for ac in ids {
            if let Component::OntologyID(id) = &ac.component {
                let stripped = OntologyID { iri: id.iri.clone(), viri: None };
                out.ont.remove(&ac);
                out.ont.insert(Component::OntologyID(stripped));
            }
        }
    }
    Ok(out)
}

/// Every `AnnotationAssertion` in `model` whose subject is an object, annotation or
/// data property — the candidate set for the property-annotation copy in
/// `subset_module`.
fn property_annotation_axioms(
    model: &crate::model::Model,
) -> Vec<horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>> {
    use horned_owl::model::{AnnotationSubject, Component};
    let props = property_signature(model);
    model
        .ont
        .iter()
        .filter(|ac| match &ac.component {
            Component::AnnotationAssertion(aa) => match &aa.subject {
                AnnotationSubject::IRI(s) => props.contains(s.as_ref()),
                _ => false,
            },
            _ => false,
        })
        .cloned()
        .collect()
}

/// The object, data and annotation properties in `model`'s signature, including the
/// ones that occur only inside axiom annotations.
///
/// `select::signature_entities` walks each component but NOT its axiom
/// annotations, so a property used only there — `OMO_0002001` inside
/// `Annotation(OMO_0002001 …)` on a retained synonym, `oboInOwl:hasSynonymType`,
/// `rdfs:seeAlso` — would be missed and the property-annotation copy would leave it
/// as a bare stub. An entity occurring in an axiom annotation is in the ontology's
/// signature, so this walk adds them back.
pub(crate) fn property_signature(model: &crate::model::Model) -> HashSet<String> {
    let sig = select::signature_entities(model);
    let mut out: HashSet<String> = HashSet::new();
    out.extend(sig.object_properties.iter().cloned());
    out.extend(sig.data_properties.iter().cloned());
    out.extend(sig.annotation_properties.iter().cloned());
    for ac in model.ont.iter() {
        out.extend(crate::sig::annotation_properties(&ac.component));
        for a in ac.ann.iter() {
            out.insert(a.ap.0.as_ref().to_string());
        }
    }
    out
}

/// All asserted-named descendants of `roots` (transitive subclasses), via the
/// `SubClassOf(named, named)` edges. Roots themselves are not included.
fn descendants(model: &crate::model::Model, roots: &HashSet<String>) -> HashSet<String> {
    use horned_owl::model::{ClassExpression as CE, Component};
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(sub), CE::Class(sup)) = (&sc.sub, &sc.sup) {
                children
                    .entry(sup.0.to_string())
                    .or_default()
                    .push(sub.0.to_string());
            }
        }
    }
    let mut out = HashSet::new();
    let mut stack: Vec<String> = roots.iter().cloned().collect();
    while let Some(c) = stack.pop() {
        if let Some(kids) = children.get(&c) {
            for k in kids {
                if out.insert(k.clone()) {
                    stack.push(k.clone());
                }
            }
        }
    }
    out
}

/// Assemble [`ExtractOptions`] from the parsed CLI args, expanding/validating the
/// enum-valued flags and loading the optional `--sources` mapping.
fn build_options(model: &crate::model::Model, args: &Args) -> anyhow::Result<ExtractOptions> {
    let individuals = Individuals::parse(&args.individuals)
        .ok_or_else(|| anyhow::anyhow!("unknown --individuals value: {}", args.individuals))?;
    let intermediates = Intermediates::parse(&args.intermediates)
        .ok_or_else(|| anyhow::anyhow!("unknown --intermediates value: {}", args.intermediates))?;

    // -s,--sources: a `TERM SOURCE_IRI` per line (CURIEs expanded against the
    // model's prefix map). Used only by --annotate-with-source.
    let mut sources: HashMap<String, String> = HashMap::new();
    if let Some(path) = &args.sources {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading --sources {}: {e}", path.display()))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            if let (Some(term), Some(src)) = (it.next(), it.next()) {
                sources.insert(select::expand(model, term), select::expand(model, src));
            }
        }
    }

    Ok(ExtractOptions {
        copy_ontology_annotations: args.copy_ontology_annotations.unwrap_or(false),
        annotate_with_source: args.annotate_with_source.unwrap_or(false),
        individuals,
        intermediates,
        output_iri: args.output_iri.clone(),
        sources,
    })
}
