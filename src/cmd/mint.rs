//! `mint` — allocate definitive IDs for temporary ones (KGCL `kgcl:mint`, the
//! `allocate-definitive-ids` target).
//!
//! CL's release process lets editors add terms with *temporary* IDs under a
//! reserved prefix (`…/CL_99xxxxx`). A CI job then rewrites each temporary IRI to
//! the next free *definitive* ID drawn from a named block in the repo's
//! `*-idranges.owl` file (for CL, the `Automation` range `[20000, 120000)`), and
//! reserializes: `mint` rewrites the IRIs, then passes the model on to a chained
//! `convert` that writes the OFN back over the edit file.
//!
//! Allocation is deterministic: temporary IRIs are taken in ascending numeric
//! order and assigned the lowest free IDs in the range (skipping any already used
//! anywhere in the ontology). Which definitive ID a given temporary IRI receives
//! is owlmake's own choice — the target runs only in CI and commits its own
//! output, so determinism and non-collision are what the repo depends on, not one
//! particular pairing.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;

use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// IRI prefix that marks a temporary ID (KGCL `--temp-id-prefix`), e.g.
    /// `http://purl.obolibrary.org/obo/CL_99`. Every entity whose IRI starts with
    /// this prefix is reassigned a definitive ID.
    #[arg(long = "temp-id-prefix", value_name = "IRI")]
    pub temp_id_prefix: String,
    /// Name (the `allocatedto` owner) of the ID range to draw definitive IDs from,
    /// e.g. `Automation` (KGCL `--id-range-name`).
    #[arg(long = "id-range-name", value_name = "NAME")]
    pub id_range_name: String,
    /// The `*-idranges.owl` file. Defaults to the single `*-idranges.owl` sitting
    /// next to the input file (or in the current directory when piped).
    #[arg(long = "id-ranges", value_name = "FILE")]
    pub id_ranges: Option<PathBuf>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    // Load the edit file's OWN axioms only — mint reallocates IDs that live in the
    // edit file and must preserve its `Import(...)` declarations rather than
    // flatten the whole import closure into the reserialised output.
    let mut model = crate::cmd::take_or_load_no_imports(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    let used_iris = all_iris(&model);

    // The temporary entities to reallocate, ascending by their numeric suffix so
    // the assignment is deterministic.
    let mut temp: Vec<String> =
        used_iris.iter().filter(|i| i.starts_with(&args.temp_id_prefix)).cloned().collect();
    temp.sort_by(|a, b| suffix_num(a).cmp(&suffix_num(b)).then_with(|| a.cmp(b)));

    // Banner labels for the reserialised edit file are resolved from the import
    // closure: the closure is loaded for its labels, but only the root is
    // serialised. Best-effort: if the closure can't be loaded, banners fall back to
    // the entity CURIE.
    let banner_labels = closure_labels(args.input.as_deref(), &args.common);
    // …and so is the set of entities the closure declares. The RDF/XML renderer
    // gives every signature entity a section, materialising a bare
    // `<owl:Class rdf:about="…"/>` stub for one nothing declares — unless an
    // imported ontology has it in signature. Knowing what the closure declares is
    // what keeps the reserialised edit file from carrying thousands of stubs for
    // terms it merely references (EFO's PR/CHEBI/MONDO classes), while an entity
    // that neither the imports nor the edit file declares still gets its stub.
    // Only when this command owns the load: piped into a build pipeline there is
    // no `-i` to resolve a closure from, and the executor has already supplied it.
    let declared = closure_declared(&model, args.input.as_deref(), &args.common);
    if !declared.is_empty() {
        model.closure_declared = declared;
    }
    // The in-memory ontology is an unordered set, so recover the source file's
    // `Import(...)` order from its text and preserve it on output — reserialising
    // the edit file must not reshuffle its imports.
    let import_order = source_import_order(args.input.as_deref());

    if temp.is_empty() {
        status!("mint: no IRIs under `{}` — nothing to allocate", args.temp_id_prefix);
        model.banner_labels = banner_labels;
        model.import_order = import_order;
        crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
        return Ok(Some(model));
    }

    // The definitive-ID namespace is the temp prefix with its trailing digits
    // dropped (`…/CL_99` → `…/CL_`); the zero-padding width is the temp IRIs' own
    // numeric-suffix length (CL: 7, i.e. `CL_0020000`).
    let id_prefix = args.temp_id_prefix.trim_end_matches(|c: char| c.is_ascii_digit()).to_string();
    let width = digits_after(&temp[0], &id_prefix).len().max(1);

    // Numbers already taken in that namespace, so a definitive ID never collides.
    let mut used_nums: HashSet<i64> = used_iris
        .iter()
        .filter_map(|i| digits_after_opt(i, &id_prefix))
        .filter_map(|d| d.parse::<i64>().ok())
        .collect();

    let (low, high) = range_bounds(args)?;

    // Assign the lowest free ID in the range to each temporary entity in turn.
    let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut next = low;
    for iri in &temp {
        while used_nums.contains(&next) {
            next += 1;
        }
        if next > high {
            bail!(
                "mint: ID range `{}` [{}..={}] is exhausted — {} temporary ID(s) still unallocated",
                args.id_range_name,
                low,
                high,
                temp.len() - map.len()
            );
        }
        let definitive = format!("{id_prefix}{next:0width$}");
        used_nums.insert(next);
        map.insert(iri.clone(), definitive);
        next += 1;
    }

    status!(
        "mint: allocated {} definitive ID(s) from `{}` [{}..={}]",
        map.len(),
        args.id_range_name,
        low,
        high
    );

    let mut renamed = crate::cmd::rename::rename_model(model, &map)?;
    renamed.banner_labels = banner_labels;
    renamed.import_order = import_order;
    crate::cmd::maybe_save(&mut renamed, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(renamed))
}

/// Recover the `Import(<IRI>)` order from the source file's text. The parsed
/// ontology is an unordered set, so this is the only place the document order
/// survives. Returns an empty vec when there is no file input or it can't be read.
fn source_import_order(input: Option<&std::path::Path>) -> Vec<String> {
    let Some(path) = input else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut order = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("Import(<") {
            if let Some(end) = rest.find('>') {
                order.push(rest[..end].to_string());
            }
        }
    }
    order
}

/// Collect `entity IRI → rdfs:label` across the input's whole import closure, for
/// the functional-syntax banner comments. Best-effort: a load failure (offline,
/// missing catalog, no input when piped) yields an empty map and banners fall
/// back to the CURIE, so this never fails the command.
fn closure_labels(input: Option<&std::path::Path>, common: &crate::cmd::CommonArgs) -> std::collections::HashMap<String, String> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Component, Literal};

    const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
    let mut labels = std::collections::HashMap::new();
    // `take_or_load` merges the import closure (via the catalog), which is exactly
    // the label set the banners need; only the labels are read, then it is
    // discarded.
    let merged = match crate::cmd::take_or_load(None, input, common) {
        Ok(m) => m,
        Err(_) => return labels,
    };
    for ac in merged.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                if let (AnnotationSubject::IRI(subj), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    let text = match lit {
                        Literal::Simple { literal }
                        | Literal::Language { literal, .. }
                        | Literal::Datatype { literal, .. } => literal.clone(),
                    };
                    labels.entry(subj.as_ref().to_string()).or_insert(text);
                }
            }
        }
    }
    labels
}

/// The signature of the import closure ALONE — the entities an imported
/// ontology declares on the root's behalf, keyed as [`crate::build::closure_declared_entities`]
/// keys them. Resolved from a scratch document carrying only the root's
/// `Import(...)`s, so the root's own signature never leaks in: an entity the edit
/// file references but nothing imports must still get its stub. Best-effort — an
/// unresolvable closure yields an empty set, and every undeclared entity is then
/// stubbed.
fn closure_declared(
    root: &Model,
    input: Option<&std::path::Path>,
    common: &crate::cmd::CommonArgs,
) -> std::collections::HashSet<String> {
    use horned_owl::model::{Component, MutableOntology};

    let mut imports_only = Model::new();
    for ac in root.ont.iter() {
        if matches!(ac.component, Component::Import(_)) {
            imports_only.ont.insert(ac.clone());
        }
    }
    if !imports_only.ont.iter().any(|ac| matches!(ac.component, Component::Import(_))) {
        return std::collections::HashSet::new();
    }
    match common.apply_catalog(&mut imports_only, input) {
        Ok(()) => crate::build::closure_declared_entities(&imports_only),
        Err(_) => std::collections::HashSet::new(),
    }
}

/// Resolve `(low, high)` inclusive for the requested named range.
fn range_bounds(args: &Args) -> Result<(i64, i64)> {
    let path = match &args.id_ranges {
        Some(p) => p.clone(),
        None => find_idranges(args.input.as_deref())?,
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading ID-ranges file {}", path.display()))?;
    let ranges = crate::idpolicy::parse_idranges(&text);
    let r = ranges
        .iter()
        .find(|r| r.owner == args.id_range_name)
        .ok_or_else(|| {
            let names: Vec<&str> = ranges.iter().map(|r| r.owner.as_str()).collect();
            anyhow::anyhow!(
                "mint: no ID range named `{}` in {} (found: {})",
                args.id_range_name,
                path.display(),
                names.join(", ")
            )
        })?;
    Ok((r.low, r.high))
}

/// Locate the `*-idranges.owl` file next to the input (or in the CWD when piped).
fn find_idranges(input: Option<&std::path::Path>) -> Result<PathBuf> {
    let dir = input
        .and_then(|p| p.parent())
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut hits: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("scanning {} for an *-idranges.owl file", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with("-idranges.owl")))
        .collect();
    hits.sort();
    match hits.len() {
        0 => bail!(
            "mint: no *-idranges.owl file found in {}; pass one with --id-ranges",
            dir.display()
        ),
        1 => Ok(hits.pop().unwrap()),
        _ => bail!(
            "mint: multiple *-idranges.owl files in {}; pass one with --id-ranges",
            dir.display()
        ),
    }
}

/// Every IRI referenced anywhere in the ontology (logical signature plus
/// annotation subjects, properties, and IRI values — mint rewrites any IRI).
fn all_iris(model: &Model) -> HashSet<String> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Component};
    let mut present: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        present.extend(crate::sig::signature(&ac.component));
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let AnnotationSubject::IRI(iri) = &aa.subject {
                present.insert(iri.as_ref().to_string());
            }
            present.insert(aa.ann.ap.0.as_ref().to_string());
            if let AnnotationValue::IRI(iri) = &aa.ann.av {
                present.insert(iri.as_ref().to_string());
            }
        }
    }
    present
}

/// The digit run of `iri` immediately after `prefix`, or `""` if it does not
/// start with `prefix` / has no digits there.
fn digits_after(iri: &str, prefix: &str) -> String {
    digits_after_opt(iri, prefix).unwrap_or_default()
}

/// `Some(digits)` when `iri` starts with `prefix` and the remainder is all
/// ASCII digits; `None` otherwise (so mixed suffixes are ignored).
fn digits_after_opt(iri: &str, prefix: &str) -> Option<String> {
    let rest = iri.strip_prefix(prefix)?;
    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
        Some(rest.to_string())
    } else {
        None
    }
}

/// Numeric value of an IRI's trailing digit run (for ordering temp IRIs);
/// non-numeric suffixes sort first as 0.
fn suffix_num(iri: &str) -> i64 {
    let digits: String = iri.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    digits.chars().rev().collect::<String>().parse::<i64>().unwrap_or(0)
}
