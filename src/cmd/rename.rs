//! `rename` — bulk-replace entity IRIs.
//!
//! Mappings come from `--mapping OLD NEW` pairs and/or a `--mappings` TSV file
//! (`old<TAB>new` per line). Replacement is performed over the RDF/XML
//! serialization (which round-trips real ontologies faithfully) by replacing
//! quoted full IRIs, then reparsing.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;

use crate::cmd::select;
use crate::io::{self, Format};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// A single OLD NEW IRI/CURIE mapping. Repeatable.
    #[arg(long, num_args = 2, value_names = ["OLD", "NEW"])]
    pub mapping: Vec<String>,
    /// A TSV file of `old<TAB>new` mappings.
    #[arg(short = 'm', long)]
    pub mappings: Option<PathBuf>,
    /// Allow mappings for entities that do not appear in the ontology
    /// (default false). `<bool>`.
    #[arg(short = 'M', long, num_args = 1, default_missing_value = "true")]
    pub allow_missing_entities: Option<bool>,
    /// Allow two or more terms to be renamed to the same full IRI
    /// (default false). `<bool>`.
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true")]
    pub allow_duplicates: Option<bool>,
    /// A TSV file of `oldNamespace<TAB>newNamespace` mappings; every IRI starting
    /// with an old namespace has that prefix rewritten to the new one.
    #[arg(short = 'r', long = "prefix-mappings", value_name = "FILE")]
    pub prefix_mappings: Option<PathBuf>,

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

    // Prefix-mappings mode: rewrite IRIs by namespace prefix rather than by full
    // IRI. Mutually-exclusive in spirit with --mapping/--mappings; if a prefix
    // mappings file is given we run that path and return.
    if let Some(path) = &args.prefix_mappings {
        let text = std::fs::read_to_string(path)?;
        // Resolve a token to a namespace: a declared prefix name resolves to its
        // namespace IRI, otherwise the token is used literally.
        let resolve_ns = |tok: &str| -> String {
            let tok = tok.trim().trim_end_matches(':');
            for (name, ns) in model.prefixes.mappings() {
                if name.as_str() == tok {
                    return ns.clone();
                }
            }
            tok.to_string()
        };
        let mut prefix_map: HashMap<String, String> = HashMap::new();
        // The first row is a header, not a mapping — same as for `--mappings`.
        for line in text.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((old, new)) = line.split_once('\t') {
                prefix_map.insert(resolve_ns(old), resolve_ns(new));
            }
        }
        if prefix_map.is_empty() {
            bail!("rename: --prefix-mappings file contained no `old<TAB>new` rows");
        }
        let mut renamed = rename_by_prefix(model, &prefix_map)?;
        crate::cmd::maybe_save(&mut renamed, args.output.as_deref(), args.format.as_deref())?;
        return Ok(Some(renamed));
    }

    let mut map: HashMap<String, String> = HashMap::new();
    for pair in args.mapping.chunks(2) {
        if let [old, new] = pair {
            map.insert(select::expand(&model, old), select::expand(&model, new));
        }
    }
    if let Some(path) = &args.mappings {
        let text = std::fs::read_to_string(path)?;
        // The first row is a HEADER, not a mapping: only its column count matters.
        // EFO's `pr_efo_map.tsv` leads with `Old IRI<TAB>New IRI`, which read as a
        // mapping row makes the whole rename fail on a missing entity.
        for line in text.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((old, new)) = line.split_once('\t') {
                map.insert(
                    select::expand(&model, old.trim()),
                    select::expand(&model, new.trim()),
                );
            }
        }
    }
    if map.is_empty() {
        bail!("rename requires at least one --mapping or a --mappings file");
    }

    let allow_duplicates = args.allow_duplicates.unwrap_or(false);
    if !allow_duplicates {
        // Detect two distinct source IRIs mapped to the same target IRI.
        let mut seen: HashMap<&String, &String> = HashMap::new();
        for (old, new) in &map {
            if let Some(prev) = seen.insert(new, old) {
                if prev != old {
                    bail!(
                        "rename: '{prev}' and '{old}' both map to '{new}'; \
                         use --allow-duplicates true to allow",
                    );
                }
            }
        }
    }

    let allow_missing = args.allow_missing_entities.unwrap_or(false);
    if !allow_missing {
        // Every source IRI must appear somewhere in the ontology. Use the logical
        // signature plus annotation subjects/IRI values (rename rewrites any IRI).
        use horned_owl::model::{AnnotationSubject, AnnotationValue, Component};
        let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
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
        let missing: Vec<&String> = map
            .keys()
            .filter(|old| !present.contains(*old))
            .collect();
        if !missing.is_empty() {
            bail!(
                "rename: {} mapping source(s) do not appear in the ontology \
                 (e.g. '{}'); use --allow-missing-entities true to allow",
                missing.len(),
                missing[0]
            );
        }
    }

    let mut renamed = rename_model(model, &map)?;

    crate::cmd::maybe_save(&mut renamed, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(renamed))
}

/// Rewrite every occurrence of each `old` IRI to `new` across the ontology.
/// Pure core shared by the CLI and by class-merging operations.
pub fn rename_model(model: crate::model::Model, map: &HashMap<String, String>) -> Result<crate::model::Model> {
    if map.is_empty() {
        return Ok(model);
    }
    // Round-trip through RDF/XML, replacing the quoted IRIs (rdf:about /
    // rdf:resource / rdf:datatype). Robust across every axiom type.
    let mut rdf = Vec::new();
    io::write_to_ref(&model, &mut rdf, Format::RdfXml)?;
    let mut text = String::from_utf8(rdf)?;
    for (old, new) in map {
        if old != new {
            text = text.replace(&format!("\"{old}\""), &format!("\"{new}\""));
        }
    }
    // The round-trip through RDF/XML is an implementation detail — the caller's
    // document state (prefix map, anonymous-node allocation, import order, …) has
    // to survive it. A rename in the middle of a chain that dropped it would hand
    // the next step the re-read document's own metadata, so the chain's output
    // would be serialised against the wrong prefix map.
    let mut out = io::load_from(std::io::Cursor::new(text.into_bytes()), Format::RdfXml)?;
    out.carry_meta_from(&model);
    // The blank-node evidence is keyed by IRIs — owners, and the property/filler
    // parts of shared-node keys — so it renames WITH the ontology. Left under
    // the old names, a renamed class's axioms lose their shared-node identity
    // and the writer splits every node it owned back into per-owner copies
    // (the composite chain renames thousands of classes in merge-equivalent-sets).
    let ren = |s: &str| -> String { map.get(s).cloned().unwrap_or_else(|| s.to_string()) };
    out.owl_shared_owners = out
        .owl_shared_owners
        .drain()
        .map(|(owner, keys)| {
            let keys = keys
                .into_iter()
                .map(|k| {
                    let parts: Vec<String> = k.split('\u{1}').map(&ren).collect();
                    parts.join("\u{1}")
                })
                .collect();
            (ren(&owner), keys)
        })
        .collect();
    out.shared_anon = out.shared_anon.drain().map(|(owner, v)| (ren(&owner), v)).collect();
    out.cross_shared = out
        .cross_shared
        .drain()
        .map(|(k, g)| {
            let parts: Vec<String> = k.split('\u{1}').map(&ren).collect();
            (parts.join("\u{1}"), g)
        })
        .collect();
    drop_invented_declarations(&mut out, &model, map);
    Ok(out)
}

/// Remove the `Declaration`s the RDF/XML round-trip invented.
///
/// The renderer gives every SIGNATURE entity a section, so an entity that the
/// input only *references* (its declaration living in an import) comes back from
/// the re-read as a real `Declaration` axiom. Renaming must rewrite IRIs and
/// nothing else: minting two IDs in EFO's edit file would otherwise add thousands
/// of declarations for externally-referenced PR/CHEBI/MONDO classes, and an ID
/// allocation job would commit every one of them.
fn drop_invented_declarations(
    out: &mut crate::model::Model,
    input: &crate::model::Model,
    map: &HashMap<String, String>,
) {
    use horned_owl::model::MutableOntology;
    // The input's declarations, under their post-rename IRIs.
    let before: std::collections::HashSet<(u8, String)> = declarations(input)
        .map(|(kind, iri)| {
            (kind, map.get(&iri).cloned().unwrap_or(iri))
        })
        .collect();
    let extra: Vec<_> = out
        .ont
        .iter()
        .filter(|ac| {
            declaration_of(&ac.component).is_some_and(|(kind, iri)| !before.contains(&(kind, iri)))
        })
        .cloned()
        .collect();
    for ac in extra {
        out.ont.remove(&ac);
    }
}

/// The `(kind, IRI)` of every `Declaration` in `model`.
fn declarations(model: &crate::model::Model) -> impl Iterator<Item = (u8, String)> + '_ {
    model.ont.iter().filter_map(|ac| declaration_of(&ac.component))
}

/// `(kind, IRI)` when the component is a `Declaration`, else `None`. The kind
/// discriminates the six entity types, so a class and an annotation property of
/// the same IRI are not conflated.
fn declaration_of(c: &horned_owl::model::Component<horned_owl::model::RcStr>) -> Option<(u8, String)> {
    use horned_owl::model::Component;
    let (kind, iri) = match c {
        Component::DeclareClass(d) => (0u8, d.0 .0.as_ref()),
        Component::DeclareObjectProperty(d) => (1, d.0 .0.as_ref()),
        Component::DeclareAnnotationProperty(d) => (2, d.0 .0.as_ref()),
        Component::DeclareDataProperty(d) => (3, d.0 .0.as_ref()),
        Component::DeclareNamedIndividual(d) => (4, d.0 .0.as_ref()),
        Component::DeclareDatatype(d) => (5, d.0 .0.as_ref()),
        _ => return None,
    };
    Some((kind, iri.to_string()))
}

/// Rewrite IRIs by namespace prefix: every IRI beginning with an `old` namespace
/// has that leading portion replaced with the corresponding `new` namespace.
/// Like [`rename_model`], the rewrite is done over the RDF/XML serialization by
/// substituting the quoted-open + namespace, so it matches any axiom position.
pub fn rename_by_prefix(
    model: crate::model::Model,
    prefix_map: &HashMap<String, String>,
) -> Result<crate::model::Model> {
    if prefix_map.is_empty() {
        return Ok(model);
    }
    let mut rdf = Vec::new();
    io::write_to_ref(&model, &mut rdf, Format::RdfXml)?;
    let mut text = String::from_utf8(rdf)?;
    for (old, new) in prefix_map {
        if old != new {
            // IRIs in RDF/XML appear as `"<full-iri>"`; replace the opening
            // quote + old namespace with quote + new namespace.
            text = text.replace(&format!("\"{old}"), &format!("\"{new}"));
        }
    }
    let mut out = io::load_from(std::io::Cursor::new(text.into_bytes()), Format::RdfXml)?;
    out.carry_meta_from(&model);
    Ok(out)
}
