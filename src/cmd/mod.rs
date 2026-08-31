//! The `om` subcommands and the input/output plumbing they share.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use horned_owl::curie::PrefixMapping;

use crate::io::{self, Format};
use crate::model::Model;

/// Global options accepted on (nearly) every command. Flattened into each
/// command's `Args` with `#[command(flatten)]` so every subcommand takes the same
/// input, prefix and catalog switches, and applied uniformly via
/// [`CommonArgs::apply`].
#[derive(ClapArgs, Clone, Default)]
pub struct CommonArgs {
    /// Load the input ontology from an IRI instead of a file.
    #[arg(short = 'I', long = "input-iri", value_name = "IRI")]
    pub input_iri: Option<String>,

    /// Override the input parser format.
    #[arg(long = "input-format", value_name = "FORMAT")]
    pub input_format: Option<String>,

    /// Use prefixes from a JSON-LD context file.
    #[arg(short = 'P', long = "prefixes", value_name = "FILE")]
    pub prefixes: Option<std::path::PathBuf>,

    /// Add a single prefix `"foo: http://bar"` (also spelled `--add-prefix`;
    /// repeatable). The `-p` short is bound per-command, where it is free.
    #[arg(long = "prefix", visible_alias = "add-prefix", value_name = "PREFIX")]
    pub add_prefix: Vec<String>,

    /// Add prefixes from a JSON-LD context file (repeatable).
    #[arg(long = "add-prefixes", value_name = "FILE")]
    pub add_prefixes: Vec<std::path::PathBuf>,

    /// Drop the standard built-in prefixes, keeping only those given explicitly.
    #[arg(long = "noprefixes")]
    pub noprefixes: bool,

    /// Emit `&prefix;` XML entities in RDF/XML output.
    #[arg(long = "xml-entities")]
    pub xml_entities: bool,

    /// XML catalog used to resolve imports.
    #[arg(long = "catalog", value_name = "FILE")]
    pub catalog: Option<std::path::PathBuf>,

    /// Use strict parsing when loading.
    #[arg(long = "strict")]
    pub strict: bool,

    /// Increase logging verbosity (`-v`/`-vv`/`-vvv`); repeatable.
    #[arg(short = 'v', long = "verbose", visible_aliases = ["very-verbose", "very-very-verbose"], action = clap::ArgAction::Count)]
    pub verbose: u8,
}

impl CommonArgs {
    /// Push the process-global options (`--strict`, `--xml-entities`,
    /// `-v/--verbose`) into the I/O and progress layers so they take effect for
    /// loads and saves. Called at the start of [`take_or_load`] (and by the
    /// multi-input commands that load directly) so it runs *before* parsing.
    pub fn activate(&self) {
        // LATCHING, not assignment. `activate` runs per subcommand, so in a
        // chained invocation (`om merge --strict -i x.owl reason -o y.owl`) plain
        // assignment would let the second subcommand's call reset `STRICT` to
        // false mid-chain, silently turning the flag off partway through. A flag
        // given anywhere in a chain applies to the whole chain.
        crate::io::latch_run_options(crate::io::RunOptions {
            strict: self.strict,
            xml_entities: self.xml_entities,
        });
        crate::progress::set_verbosity(self.verbose);
    }

    /// Resolve `owl:imports` and merge the whole import closure into `model`, then
    /// drop the (now-inlined) import declarations — so a single self-contained
    /// ontology is handed to the command, which therefore works over the whole
    /// loaded closure. This is the *default* behaviour, not opt-in: a command run
    /// on an import-bearing file (e.g. `reason -i x.owl`) must see the axioms its
    /// imports contribute, or it silently reasons/serialises over an incomplete set.
    ///
    /// Resolution order: an explicit `--catalog`, else an auto-detected sibling
    /// `catalog-v001.xml` (the layout curators and Protégé maintain), else a local
    /// sibling file, else fetching the import IRI over the network. A network
    /// failure is non-fatal (the import is left unresolved). `input` is the main
    /// document's path, used to resolve catalog-relative and default-local paths.
    pub fn apply_catalog(&self, model: &mut Model, input: Option<&Path>) -> Result<()> {
        if let Some(catalog) = self.catalog.as_deref() {
            return merge_import_closure(model, catalog, input);
        }
        // No explicit `--catalog`: still follow `owl:imports`. Skip the work
        // entirely when the document declares none.
        if imports_of(model).is_empty() {
            return Ok(());
        }
        resolve_imports_auto(model, None, input)
    }

    /// Apply prefix-affecting options to a freshly loaded model: they land after
    /// loading, on top of the document's own prefixes (`--noprefixes` clears the
    /// built-in defaults first).
    pub fn apply(&self, model: &mut Model) -> Result<()> {
        if self.noprefixes {
            model.prefixes = PrefixMapping::default();
        }
        for file in self.prefixes.iter().chain(self.add_prefixes.iter()) {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("reading prefixes file {}", file.display()))?;
            let json: serde_json::Value = serde_json::from_str(&text)
                .with_context(|| format!("parsing prefixes JSON {}", file.display()))?;
            let ctx = json.get("@context").unwrap_or(&json);
            if let Some(map) = ctx.as_object() {
                for (k, v) in map {
                    // JSON-LD context values are either a bare namespace string or a
                    // `{"@id": "...", "@prefix": true}` object (mondo's config uses
                    // both forms).
                    let ns = v.as_str().or_else(|| v.get("@id").and_then(|x| x.as_str()));
                    if let Some(ns) = ns {
                        let _ = model.prefixes.add_prefix(k, ns);
                        // Track explicitly-provided prefixes so the OBO writer emits
                        // an `idspace:` for each — regardless of use.
                        if !model.explicit_prefixes.iter().any(|(p, _)| p == k) {
                            model.explicit_prefixes.push((k.clone(), ns.to_string()));
                        }
                    }
                }
            }
        }
        for spec in &self.add_prefix {
            let (name, ns) = spec
                .split_once(':')
                .with_context(|| format!("bad --add-prefix (want \"name: iri\"): {spec}"))?;
            let _ = model.prefixes.add_prefix(name.trim(), ns.trim());
        }
        Ok(())
    }
}

/// Resolve the working model for a command: use the model piped from the previous
/// chain step if present, otherwise load `--input` (or `--input-iri`). Honors the
/// global `--input-format` override. Errors when no source exists.
pub fn take_or_load(piped: Option<Model>, input: Option<&Path>, common: &CommonArgs) -> Result<Model> {
    common.activate();
    if let Some(model) = piped {
        return Ok(model);
    }
    let fmt = common.input_format.as_deref();
    let model = if let Some(iri) = &common.input_iri {
        io::load_iri(iri, fmt).map(|m| (m, iri.clone()))?
    } else {
        let path = input
            .context("missing input: provide --input/--input-iri or pipe from a previous command")?;
        io::load_with(path, fmt).map(|m| (m, path.display().to_string()))?
    };
    let (mut model, src) = model;
    common.apply_catalog(&mut model, input)?;
    if crate::progress::verbosity() >= 1 {
        status!("loaded {}: {} axioms", src, model.ont.iter().count());
    }
    Ok(model)
}

/// Like [`take_or_load`] but WITHOUT resolving/merging the `owl:imports` closure.
///
/// For commands that operate only on a document's own axioms and must keep its
/// `Import(...)` declarations rather than inline the imported ontologies — e.g.
/// `kgcl:mint … convert`, where no import document is read at all and only the
/// root is serialised, so the edit file keeps its import declarations instead of
/// being flattened into its whole closure.
/// The signature of the import closure ALONE — the entities an imported
/// ontology declares on the root's behalf, keyed as
/// [`crate::build::closure_declared_entities`] keys them. Resolved from a scratch
/// document carrying only the root's `Import(...)`s, so the root's own signature
/// never leaks in: an entity the document references but nothing imports must
/// still get its stub. Best-effort — an unresolvable closure yields an empty set,
/// and every undeclared entity is then stubbed.
pub(crate) fn closure_declared_signature(
    root: &Model,
    input: Option<&Path>,
    common: &CommonArgs,
) -> std::collections::HashSet<String> {
    use horned_owl::model::MutableOntology;

    let mut imports_only = Model::new();
    for ac in root.ont.iter() {
        if matches!(ac.component, horned_owl::model::Component::Import(_)) {
            imports_only.ont.insert(ac.clone());
        }
    }
    if !imports_only
        .ont
        .iter()
        .any(|ac| matches!(ac.component, horned_owl::model::Component::Import(_)))
    {
        return std::collections::HashSet::new();
    }
    match common.apply_catalog(&mut imports_only, input) {
        Ok(()) => crate::build::closure_declared_entities(&imports_only),
        Err(_) => std::collections::HashSet::new(),
    }
}

pub fn take_or_load_no_imports(
    piped: Option<Model>,
    input: Option<&Path>,
    common: &CommonArgs,
) -> Result<Model> {
    common.activate();
    if let Some(model) = piped {
        return Ok(model);
    }
    let fmt = common.input_format.as_deref();
    if let Some(iri) = &common.input_iri {
        io::load_iri(iri, fmt)
    } else {
        let path = input
            .context("missing input: provide --input/--input-iri or pipe from a previous command")?;
        io::load_with(path, fmt)
    }
}

/// Collect `entity IRI → rdfs:label` across the input's whole import closure, for
/// the functional-syntax banner comments. Best-effort: a load failure (offline,
/// missing catalog, no input when piped) yields an empty map and banners fall
/// back to the CURIE, so this never fails the command.
pub(crate) fn closure_labels(input: Option<&std::path::Path>, common: &crate::cmd::CommonArgs) -> std::collections::HashMap<String, String> {
    // `take_or_load` merges the import closure (via the catalog), which is exactly
    // the label set the banners need; only the labels are read, then it is
    // discarded.
    match take_or_load(None, input, common) {
        Ok(m) => rdfs_labels(&m),
        Err(_) => std::collections::HashMap::new(),
    }
}

/// Return every process-wide option to its default, so one invocation cannot
/// inherit another's.
///
/// These options are process-wide because they are read deep inside the loaders
/// and writers, where threading them through every call would reach into
/// horned-owl's serializers. That is workable only because each of them is
/// established at the START of a run — latched from the command line by
/// [`CommonArgs::activate`], or set from the plan by
/// `build::set_robot_behaviours` — and this is the point at which "the start of a
/// run" is defined.
pub fn reset_invocation_options() {
    crate::io::set_run_options(crate::io::RunOptions::default());
    crate::io::obograph::set_nest_axiom_anns(false);
    crate::cmd::query::set_update_keeps_prefixes(true);
}

/// Resolve an output format from an explicit `--format` name, else the output
/// path's extension.
pub fn resolve_format(format: Option<&str>, output: &Path) -> Result<Format> {
    match format {
        Some(name) => Format::from_name(name),
        None => Format::from_path(output),
    }
}

/// Save the model when `--output` is given (no-op otherwise, so non-terminal
/// chain steps simply pass the model along).
pub fn maybe_save(model: &mut Model, output: Option<&Path>, format: Option<&str>) -> Result<()> {
    if let Some(out) = output {
        // Write the ROOT ontology. A command works over the whole inlined
        // closure, so the two halves of that inlining are undone together here:
        // the axioms the imports contributed come back out, and the
        // `owl:imports` declarations that stand for them go back in. `om reason
        // -i x.owl -o y.owl` therefore yields a y.owl that still imports, with
        // its own axioms plus whatever the reasoner added — not one that both
        // carries a frozen copy of an import and instructs its consumer to load
        // that import again.
        //
        // A command whose product IS the closure collapsed into one document
        // (`merge`, `extract`, `subset`) has called
        // `Model::detach_import_closure`, which empties both records, so nothing
        // below applies to it.
        restore_root_for_save(model);
        let fmt = resolve_format(format, out)?;
        if crate::progress::verbosity() >= 1 {
            status!("saving {}: {} axioms", out.display(), model.ont.iter().count());
        }
        io::save_as(model, out, fmt)?;
    }
    Ok(())
}

/// Undo closure inlining for a save that writes the ROOT ontology: the axioms
/// the imports contributed come back out of the component set, and the
/// `owl:imports` declarations that stand for them go back in. Every save of a
/// closure-inlined model that is NOT itself a collapse (`merge`, `extract`,
/// `subset` call [`Model::detach_import_closure`] instead) goes through this —
/// [`maybe_save`], and the owltools emulation's own `-o` save.
pub(crate) fn restore_root_for_save(model: &mut Model) {
    if !model.imported_components.is_empty() {
        use horned_owl::model::MutableOntology;
        let doomed: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| model.imported_components.contains(*ac))
            .cloned()
            .collect();
        for ac in doomed {
            model.ont.remove(&ac);
        }
    }
    if !model.inlined_imports.is_empty() {
        use horned_owl::model::{Component, MutableOntology};
        let existing: std::collections::HashSet<String> = model
            .ont
            .iter()
            .filter_map(|ac| match &ac.component {
                Component::Import(i) => Some(i.0.to_string()),
                _ => None,
            })
            .collect();
        let iris: Vec<String> = model.inlined_imports.clone();
        let new: Vec<horned_owl::model::Import<_>> = iris
            .into_iter()
            .filter(|iri| !existing.contains(iri))
            .map(|iri| horned_owl::model::Import(model.build.iri(iri)))
            .collect();
        for imp in new {
            model.ont.insert(imp);
        }
    }
}

/// Resolve `model`'s `owl:imports` through the XML `catalog`, merge the whole
/// closure into `model`, and drop the now-inlined import declarations. `input` is
/// the document's own path (for default-local resolution). Shared by the global
/// `--catalog` ([`CommonArgs::apply_catalog`]) and `diff --left/right-catalog`.
pub(crate) fn merge_import_closure(
    model: &mut Model,
    catalog: &Path,
    input: Option<&Path>,
) -> Result<()> {
    let map =
        parse_catalog(catalog).with_context(|| format!("reading catalog {}", catalog.display()))?;
    let base = catalog
        .parent()
        .map(Path::to_path_buf)
        .or_else(|| input.and_then(|p| p.parent().map(Path::to_path_buf)))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    resolve_import_closure(model, &map, &base)
}

/// Resolve and inline the `owl:imports` transitive closure using `map` (a
/// catalog mapping, possibly empty) with a `default_local` fallback under `base`.
/// Inlined imports are dropped so the result is self-contained. Used by both the
/// `--catalog` path and `merge`'s implicit closure following.
pub(crate) fn resolve_import_closure(
    model: &mut Model,
    map: &std::collections::BTreeMap<String, std::path::PathBuf>,
    base: &Path,
) -> Result<()> {
    let opts = crate::cmd::merge::MergeOptions::default();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: Vec<String> = imports_of(model);
    // Say that this ran, and with how many imports, BEFORE resolving any. The
    // per-import lines below are printed only when there is something to print,
    // so their absence would otherwise be ambiguous between "this path resolves
    // no closure" and "this path was never asked to" — and a reader cannot tell
    // which. Silence must not be the answer to a question the flag was asked.
    if std::env::var("OM_IMPORT_DEBUG").is_ok() {
        eprintln!("[import] resolving closure: {} direct import(s)", queue.len());
    }
    let mut merged_any = false;
    while let Some(iri) = queue.pop() {
        if !seen.insert(iri.clone()) {
            continue;
        }
        let path = map.get(&iri).cloned().or_else(|| default_local(&iri, base));
        // …and dedupe on the DOCUMENT too, not only on the name that reached it.
        // Two import IRIs a catalog maps to one file must be parsed once and
        // advance the blank-node counter once. Keyed on the IRI alone, the same
        // document would be merged twice and charge its allocation total to the
        // base twice, numbering every anonymous individual downstream from too far
        // along.
        if let Some(p) = path.as_deref() {
            let key = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
            if !seen.insert(format!("\u{1}path\u{1}{}", key.display())) {
                continue;
            }
        }
        let (imported, source) = match path {
            Some(path) => {
                let m = crate::io::load(&path)
                    .with_context(|| format!("loading import <{iri}> from {}", path.display()))?;
                (m, path.display().to_string())
            }
            None => {
                // Neither a catalog mapping nor a local sibling resolved the import,
                // so fall back to fetching the import IRI over the network.
                //
                // A failure here is FATAL. A command is handed the whole closure
                // precisely so that it reasons, filters and serialises over the
                // complete axiom set; carrying on without an import means doing
                // all of that over part of it, and the answer that comes back —
                // an unsatisfiability that was never checked, a report with no
                // violations, a module missing half its terms — is
                // indistinguishable from the right one. A declared import that
                // cannot be resolved is a broken input, and it is named as such.
                crate::io::load_iri(&iri, None)
                    .map(|m| (m, format!("<{iri}> (network)")))
                    .with_context(|| {
                        format!(
                            "unresolved import <{iri}>: no catalog entry, no local \
                             document beside the importing file, and it could not be \
                             fetched — map it in a catalog (--catalog) or place the \
                             document next to its importer"
                        )
                    })?
            }
        };
        // Follow nested imports too: `merge_into` drops them, so collect first.
        for nested in imports_of(&imported) {
            if !seen.contains(&nested) {
                queue.push(nested);
            }
        }
        if std::env::var("OM_IMPORT_DEBUG").is_ok() {
            eprintln!(
                "[import] <{iri}> -> {source} ({} components)",
                imported.ont.iter().count()
            );
        }
        // What this import LENDS the root: the components the merge is about to
        // add and the root does not already assert. Taken as the difference the
        // merge actually makes — candidates checked for membership before and
        // after — so which components a merge carries stays `merge_into`'s
        // business alone (it drops the secondary's identity and imports, and
        // gates its ontology annotations), and this cannot fall out of step with
        // it. An axiom the root asserts itself is not borrowed and stays.
        let borrowed: Vec<_> = imported
            .ont
            .iter()
            .filter(|c| !model.ont.i().contains(c))
            .cloned()
            .collect();
        // What the closure declares on the root's behalf: an entity anywhere in an
        // imported ontology's signature is that ontology's to declare, so the root
        // materialises no stub for it. The save drops the borrowed axioms again,
        // which is exactly when this record is the only thing left that knows.
        let declared = crate::build::closure_declared_entities(&imported);
        crate::cmd::merge::merge_into(model, &imported, &opts);
        model.closure_declared.extend(declared);
        for c in borrowed {
            if model.ont.i().contains(&c) {
                model.imported_components.insert(c);
            }
        }
        // This one IS an import, so its allocations move the importer's base.
        crate::cmd::merge::charge_import_allocations(model, &imported);
        merged_any = true;
        if crate::progress::verbosity() >= 1 {
            status!("imports: merged import <{iri}> from {source}");
        }
    }
    if merged_any {
        // The closure is now inlined, so the import declarations are dropped from
        // the working ontology and every command reasons over the whole closure.
        //
        // A save, however, WRITES the root ontology with its `Import(…)`
        // declarations intact: `om reason -i x.owl -o y.owl` gives a y.owl that
        // still imports rather than a self-contained document. The IRIs are
        // recorded on the model so a save can put them back — see
        // `Model::inlined_imports`.
        use horned_owl::model::{Component, MutableOntology};
        let decls: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| matches!(ac.component, Component::Import(_)))
            .cloned()
            .collect();
        for ac in &decls {
            if let Component::Import(i) = &ac.component {
                let iri = i.0.to_string();
                if !model.inlined_imports.contains(&iri) {
                    model.inlined_imports.push(iri);
                }
            }
        }
        for ac in decls {
            model.ont.remove(&ac);
        }
        // A functional-syntax banner names its entity `# Class: <IRI> (label)`,
        // and the label is the one anywhere in the closure — an edit file that
        // only DECLARES a class still banners it with the label its imported
        // pattern module asserts. The closure is inlined right now and is dropped
        // again on save, so this is the one moment the whole label set is in hand.
        if model.banner_labels.is_empty() {
            model.banner_labels = rdfs_labels(model);
        }
    }
    Ok(())
}

/// Every `entity IRI → rdfs:label` the model asserts, for the functional-syntax
/// banner comments.
///
/// An entity may carry several labels, and which one it is named by is decided
/// by the iteration order of its own annotation-assertion set — by the axioms'
/// hashes, that is, not by document order and not by the values. `oboInOwl:hasDbXref`
/// carries both "database_cross_reference" and "has cross-reference", and picking
/// the wrong one is a one-line difference in every artefact that banners it.
///
/// Deciding it by anything else is not merely wrong but UNSTABLE: an arbitrary
/// tie-break follows the model's insertion history, so a change with nothing to
/// do with labels can silently flip a previously-identical artefact either way.
///
/// The set is the SUBJECT's own, and its table is sized by how many annotation
/// assertions that subject carries — measured: giving `hasDbXref` twenty further
/// annotations, touching neither label, moves the table from 16 slots to 32 and
/// changes which label wins.
///
/// Where two labels land in the SAME slot this cannot settle it, and neither can
/// anything else, because there is no stable answer to match. Two runs of the
/// reference over one unchanged tree, minutes apart, write
/// `imports/merged_import.owl` with `database_cross_reference` and then with
/// `has cross-reference`, the two 41 MB documents otherwise byte-identical. Both
/// values have been seen twice. A Java bucket holds its members in insertion
/// order, and the pipeline does not add its axioms in a fixed one.
///
/// So this is reference non-determinism, measured, and belongs beside a SELECT
/// whose row order differs between runs. owlmake is deterministic here and
/// always writes the same value; whether that value matches is a coin toss per
/// run, and no amount of reproducing insertion order would change that.
pub(crate) fn rdfs_labels(model: &Model) -> std::collections::HashMap<String, String> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Component, Literal};

    const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
    // Candidate labels per subject, each with the hash of the axiom carrying it,
    // and the size of the set that axiom lives in.
    let mut cands: std::collections::HashMap<String, Vec<(i32, String)>> = Default::default();
    let mut subject_ann_count: std::collections::HashMap<String, usize> = Default::default();
    for ac in model.ont.iter() {
        let Component::AnnotationAssertion(aa) = &ac.component else { continue };
        let AnnotationSubject::IRI(subj) = &aa.subject else { continue };
        let subj = subj.as_ref().to_string();
        *subject_ann_count.entry(subj.clone()).or_insert(0) += 1;
        if aa.ann.ap.0.as_ref() != RDFS_LABEL {
            continue;
        }
        let AnnotationValue::Literal(lit) = &aa.ann.av else { continue };
        let text = match lit {
            Literal::Simple { literal }
            | Literal::Language { literal, .. }
            | Literal::Datatype { literal, .. } => literal.clone(),
        };
        let h = crate::owlapi_hash::annotation_assertion_hash(
            &subj,
            aa.ann.ap.0.as_ref(),
            &aa.ann.av,
            &ac.ann,
        );
        cands.entry(subj).or_default().push((h, text));
    }
    cands
        .into_iter()
        .map(|(subj, c)| {
            let text = if c.len() == 1 {
                c[0].1.clone()
            } else {
                let hashes: Vec<i32> = c.iter().map(|(h, _)| *h).collect();
                let total = subject_ann_count.get(&subj).copied().unwrap_or(c.len());
                c[crate::owlapi_hash::hashset_order_of(&hashes, total)[0]].1.clone()
            };
            (subj, text)
        })
        .collect()
}

/// Resolve the `owl:imports` closure with no catalog named on the command line.
///
/// Resolution order is the same wherever imports are followed: an explicit
/// `--catalog` (handled by the caller), else the sibling `catalog-v001.xml` that
/// curators and Protégé maintain, else a local sibling document, else the import
/// IRI over the network. The auto-detected catalog lives HERE rather than in one
/// caller, because a repo's catalog is how its imports resolve at all — a command
/// that skipped it would report every import in a normally-laid-out repo as
/// unresolvable.
pub(crate) fn resolve_imports_auto(
    model: &mut Model,
    catalog: Option<&Path>,
    input: Option<&Path>,
) -> Result<()> {
    let auto = input
        .and_then(Path::parent)
        .map(|dir| dir.join("catalog-v001.xml"))
        .filter(|c| c.exists());
    let catalog = catalog.or(auto.as_deref());
    let map = match catalog {
        Some(c) => parse_catalog(c)
            .with_context(|| format!("reading catalog {}", c.display()))?,
        None => std::collections::BTreeMap::new(),
    };
    let base = catalog
        .and_then(|c| c.parent().map(Path::to_path_buf))
        .or_else(|| input.and_then(|p| p.parent().map(Path::to_path_buf)))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    resolve_import_closure(model, &map, &base)
}

/// An on-disk dataset [`materialize_tdb`] wrote, and exactly which of its paths
/// belong to owlmake. Cleanup removes those and nothing else, so a directory the
/// caller pointed at keeps whatever else it held.
pub(crate) struct Tdb {
    dir: std::path::PathBuf,
    /// The directory did not exist and was created here, so cleanup removes the
    /// whole of it. False for a directory the caller already had.
    owns_dir: bool,
}

/// The single file a materialized dataset consists of.
const TDB_DATASET: &str = "dataset.rdf";

/// Materialize an in-memory model to an on-disk dataset for the `--tdb` family of
/// flags. owlmake evaluates SPARQL/QC in memory, but these flags still create a
/// real on-disk store. `temp` picks a temp-dir default (`--temporary-file`).
///
/// A `dataset.rdf` that is already there is NOT overwritten. The dataset is
/// owlmake's to delete when the run ends, so writing over one it did not create
/// would destroy a file on a path the caller chose and then remove the
/// replacement too — `--keep-tdb-mappings` would preserve only the replacement.
/// Refusing names the collision instead.
pub(crate) fn materialize_tdb(
    model: &Model,
    dir: Option<&Path>,
    temp: bool,
) -> Result<Option<Tdb>> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        // Unique per run, not merely per process: a PID is reused, and a stale
        // directory left by a killed run would then be adopted (and deleted) by an
        // unrelated one.
        None if temp => std::env::temp_dir().join(format!(
            "owlmake-tdb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )),
        None => std::path::PathBuf::from(".tdb"),
    };
    let owns_dir = !dir.exists();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating TDB directory {}", dir.display()))?;
    let dataset = dir.join(TDB_DATASET);
    if !owns_dir && dataset.exists() {
        bail!(
            "{} already exists: owlmake deletes the dataset it materializes, so it \
             will not write over one it did not create — point --tdb-directory at an \
             empty or new directory",
            dataset.display()
        );
    }
    let mut rdf = Vec::new();
    crate::io::write_to_ref(model, &mut rdf, Format::RdfXml)?;
    std::fs::write(&dataset, &rdf)
        .with_context(|| format!("writing TDB dataset in {}", dir.display()))?;
    if crate::progress::verbosity() >= 1 {
        status!("materialized on-disk TDB dataset at {}", dir.display());
    }
    Ok(Some(Tdb { dir, owns_dir }))
}

/// Remove a dataset created by [`materialize_tdb`], unless `keep` is set.
///
/// Only owlmake's own paths go: the dataset file always, and the directory only
/// when this run created it.
pub(crate) fn cleanup_tdb(tdb: Option<Tdb>, keep: bool) {
    let Some(Tdb { dir, owns_dir }) = tdb else { return };
    if keep {
        return;
    }
    let _ = std::fs::remove_file(dir.join(TDB_DATASET));
    if owns_dir {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Collect the `owl:imports` IRIs declared by `model`.
fn imports_of(model: &Model) -> Vec<String> {
    model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            horned_owl::model::Component::Import(i) => Some(i.0.to_string()),
            _ => None,
        })
        .collect()
}

/// Parse an XML catalog file into an import-IRI → local-path map. Recognizes the
/// `<uri name="IRI" uri="PATH"/>` entries curators and Protégé write; relative
/// `uri` paths resolve against the catalog file's directory.
fn parse_catalog(path: &Path) -> Result<std::collections::BTreeMap<String, std::path::PathBuf>> {
    let text = std::fs::read_to_string(path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut map = std::collections::BTreeMap::new();
    for frag in text.split("<uri").skip(1) {
        let tag = frag.split('>').next().unwrap_or(frag);
        let (name, uri) = match (attr(tag, "name"), attr(tag, "uri")) {
            (Some(n), Some(u)) => (n, u),
            _ => continue,
        };
        let uri = percent_decode(&uri);
        let p = std::path::Path::new(&uri);
        let resolved = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
        map.insert(name, resolved);
    }
    Ok(map)
}

/// Fallback for an import with no catalog entry: a sibling file named after the
/// IRI's last path/fragment segment, if it exists next to the catalog/input.
fn default_local(iri: &str, dir: &Path) -> Option<std::path::PathBuf> {
    let name = iri.rsplit(['/', '#']).next()?;
    if name.is_empty() {
        return None;
    }
    let cand = dir.join(name);
    cand.exists().then_some(cand)
}

/// Read the value of an XML attribute `key="..."` from a tag fragment.
fn attr(frag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let start = frag.find(&pat)? + pat.len();
    let rest = &frag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Decode `%XX` percent-escapes in a catalog `uri` (other bytes untouched).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v as char);
                i += 3;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

pub mod annotate;
pub mod babelon;
pub mod babelon_tsv;
pub mod collapse;
// The repo-hygiene commands — not ontology transformations, but gates a release
// has to pass, and native because the `om` binary is all a repo has: ID-policy
// (`<ont>-idranges.owl`) and DOSDP-pattern validation, RDF/XML parseability of
// each product, the build's tool inventory, and checksums. See each module's
// header for what it checks.
pub mod check_rdfxml;
pub mod pattern_tester;
pub mod config_check;
pub mod convert;
pub mod diff;
pub mod dosdp;
pub mod expand;
pub mod explain;
pub mod export;
pub mod export_prefixes;
pub mod extract;
// `embeddings` (arrow/parquet/zstd C + rayon threads + network) and `map` (which
// uses it) are the only genuinely native pieces of the OLS-compatible text and
// embedding commands — excluded from the wasm core. `extract_strings`, `text_tagger`
// and `lexmatch` are pure Rust and build for wasm (tiktoken-rs/sha1/flate2 are
// wasm-safe general deps; see also `tag`).
#[cfg(not(target_arch = "wasm32"))]
pub mod embeddings;
#[cfg(not(target_arch = "wasm32"))]
pub mod map;
pub mod fastobo_validator;
pub mod runoak;
pub mod runoak_diff;
pub mod extract_strings;
pub mod extract_upheno_relations;
pub mod text_tagger;
pub mod kgx;
pub mod lexmatch;
pub mod filter;
pub mod import_module;
pub mod information_content;
pub mod materialize;
pub mod measure;
pub mod normalize;
pub mod ogrep;
pub mod merge;
pub mod merge_equivalent_sets;
pub mod merge_species;
pub mod create_species_subset;
pub mod mint;
pub mod mirror;
pub mod make;
pub mod oort;
pub mod owltools_ops;
pub mod query;
pub mod reason;
pub mod reduce;
pub mod relax;
pub mod release;
pub mod rename;
pub mod repair;
pub mod report;
pub mod remove;
pub mod rewrite_def;
pub mod schema;
pub mod seed;
pub mod select;
// `semsql make` builds the ontology SQL database, so it is native-only for the
// same reason `crate::semsql` is: SQLite's C has no wasm target.
#[cfg(not(target_arch = "wasm32"))]
pub mod semsql;
pub mod subset;
pub mod template;
pub mod ubergraph;
pub mod unmerge;
pub mod validate_id_ranges;
pub mod validate_patterns;
pub mod validate_profile;
pub mod verify;
