//! `merge` — combine multiple ontologies (and optionally their import
//! closures) into a single ontology.

use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{
    Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, AnnotatedComponent,
    Component, MutableOntology,
};

use crate::io;
use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    /// Input ontology paths (repeatable).
    #[arg(short = 'i', long = "input", num_args = 1..)]
    pub inputs: Vec<PathBuf>,

    /// Merge ontologies matching a filesystem wildcard pattern. Bound without a
    /// short: `-p` collides with the global `-P,--prefixes`/`--prefix`, so only
    /// the long form is exposed here. Repeatable.
    #[arg(long = "inputs", value_name = "PATTERN")]
    pub input_globs: Vec<String>,

    /// Output ontology path.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format (overrides inference from the output extension). `-f` is
    /// taken by `--annotate-derived-from` on this command, so the format has no
    /// short; use the long `--format`.
    #[arg(long)]
    pub format: Option<String>,

    /// Keep secondary inputs' ontology-level annotations (`<bool>`, default false:
    /// by default only the primary ontology's annotations survive).
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true")]
    pub include_annotations: Option<bool>,

    /// Merge the imports closure (`<bool>`, default true). When true, each
    /// input's `owl:imports` transitive closure is resolved (via `--catalog` or
    /// as sibling files) and merged in, then the import declarations are dropped.
    /// When false, imports are kept and their content is not merged.
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true")]
    pub collapse_import_closure: Option<bool>,

    /// Annotate each entity with rdfs:isDefinedBy = its source ontology IRI
    /// (`<bool>`, default false).
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true")]
    pub annotate_defined_by: Option<bool>,

    /// Annotate merged axioms with prov:wasDerivedFrom = their source ontology
    /// IRI (`<bool>`, default false).
    #[arg(short = 'f', long = "annotate-derived-from", num_args = 1, default_missing_value = "true")]
    pub annotate_derived_from: Option<bool>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Post-merge options, one per `merge` flag; the defaults here are the flag
/// defaults.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MergeOptions {
    pub include_annotations: bool,
    pub collapse_import_closure: bool,
    pub annotate_defined_by: bool,
    pub annotate_derived_from: bool,
}

impl Default for MergeOptions {
    fn default() -> Self {
        MergeOptions {
            include_annotations: false,
            collapse_import_closure: true,
            annotate_defined_by: false,
            annotate_derived_from: false,
        }
    }
}

impl Args {
    fn options(&self) -> MergeOptions {
        MergeOptions {
            include_annotations: self.include_annotations.unwrap_or(false),
            collapse_import_closure: self.collapse_import_closure.unwrap_or(true),
            annotate_defined_by: self.annotate_defined_by.unwrap_or(false),
            annotate_derived_from: self.annotate_derived_from.unwrap_or(false),
        }
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> anyhow::Result<Option<Model>> {
    // merge loads its inputs directly (not via take_or_load), so push the shared
    // `--strict`/`--xml-entities`/`-v` options into the I/O layer here.
    args.common.activate();
    // Expand any `--inputs <glob>` patterns into concrete files, appended after
    // the explicit `--input` files.
    let mut all_inputs: Vec<PathBuf> = args.inputs.clone();
    for pattern in &args.input_globs {
        let matched = expand_glob(pattern)?;
        if matched.is_empty() {
            status!("merge: WARNING — pattern `{pattern}` matched no files");
        }
        all_inputs.extend(matched);
    }
    // Drop empty *stamp* inputs (e.g. UBERON's `tmp/bridges`, a `touch`ed marker
    // listed among a `merge`'s prerequisites): they carry no axioms, so merging
    // them is a no-op — but `io::load` would fail to determine a format.
    all_inputs.retain(|p| !io::is_empty_ontology_file(p));

    let opts = args.options();
    let (mut merged, rest): (Model, Vec<PathBuf>) = match piped {
        Some(m) => (m, all_inputs.clone()),
        // The global `-I,--input-iri` works on `merge` too, and a component
        // download needs it: CL builds `component-download-%.owl` from a remote
        // subset IRI with no file input at all. Treat the IRI as the primary
        // ontology, exactly as a first `--input` would be.
        None if all_inputs.is_empty() && args.common.input_iri.is_some() => {
            let iri = args.common.input_iri.clone().unwrap();
            (io::load_iri(&iri, args.common.input_format.as_deref())?, Vec::new())
        }
        None => {
            if all_inputs.is_empty() {
                anyhow::bail!(
                    "merge requires at least one --input/--inputs, an --input-iri, or a piped ontology"
                );
            }
            (io::load(&all_inputs[0])?, all_inputs[1..].to_vec())
        }
    };
    args.common.apply(&mut merged)?;

    // Provenance for the primary ontology, when annotating defined-by/derived-from.
    if opts.annotate_defined_by || opts.annotate_derived_from {
        if let Some(src) = ontology_iri(&merged) {
            let ents = declared_entities(&merged);
            annotate_provenance(&mut merged, &src, &ents, &opts);
        }
    }

    // Collect every input's `owl:imports` so the whole closure (not just the
    // primary's) is followed/preserved — `merge_into` strips imports per input.
    let mut all_import_iris = Vec::new();
    for ac in merged.ont.iter() {
        if let Component::Import(imp) = &ac.component {
            all_import_iris.push(imp.0.clone());
        }
    }
    for path in &rest {
        let other = io::load(path)?;
        for ac in other.ont.iter() {
            if let Component::Import(imp) = &ac.component {
                all_import_iris.push(imp.0.clone());
            }
        }
        merge_into(&mut merged, &other, &opts);
    }
    // Re-introduce the imports (deduped by the axiom set) so the closure logic
    // below sees secondary inputs' imports too.
    {
        use horned_owl::model::{Import, MutableOntology};
        for iri in all_import_iris {
            merged.ont.insert(Component::Import(Import(iri)));
        }
    }

    // `--collapse-import-closure` (default true): follow each input's
    // `owl:imports` transitive closure (resolved through `--catalog` if given,
    // else as sibling files of the first input), merge that content in, then drop
    // the import declarations for a single self-contained ontology. With
    // `--collapse-import-closure false` the import declarations are kept untouched
    // and their content is not merged.
    if opts.collapse_import_closure {
        crate::cmd::resolve_imports_auto(
            &mut merged,
            args.common.catalog.as_deref(),
            all_inputs.first().map(|p| p.as_path()),
        )?;
        // Drop any imports that could not be resolved, so nothing dangles.
        use horned_owl::model::MutableOntology;
        let imports: Vec<_> = merged
            .ont
            .iter()
            .filter(|ac| matches!(ac.component, Component::Import(_)))
            .cloned()
            .collect();
        for ac in imports {
            merged.ont.remove(&ac);
        }
        // …and they stay dropped. `resolve_import_closure` records what it inlined
        // so that a save can write the ROOT ontology: `om reason -i x.owl -o
        // y.owl` hands back the ontology it was given, still importing. `merge`
        // means the opposite — a collapsed merge inlines the closure and writes
        // ONE self-contained document — so the record is discarded here, and the
        // merged-in axioms become the result's own.
        merged.detach_import_closure();
    }

    collapse_inverse_pairs(&mut merged);

    crate::cmd::maybe_save(&mut merged, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(merged))
}

/// Drop an `InverseObjectProperties(B, A)` when `(A, B)` is already present.
///
/// Being inverses is symmetric, so `InverseObjectProperties(A B)` and
/// `InverseObjectProperties(B A)` assert the same thing: an import closure that
/// states the inverse from both ends, or two merged documents that each state one
/// direction, contribute ONE axiom and not two. horned-owl models the axiom as an
/// ordered pair, so without this the merge of BFO's `(BFO_0000117 BFO_0000132)`
/// and its mirror image leaves both in the output.
///
/// The first orientation encountered wins.
fn collapse_inverse_pairs(model: &mut Model) {
    use horned_owl::model::{Component as C, MutableOntology, ObjectPropertyExpression as OPE};
    let named = |o: &OPE<horned_owl::model::RcStr>| match o {
        OPE::ObjectProperty(p) => Some(p.0.to_string()),
        OPE::InverseObjectProperty(_) => None,
    };
    let mut seen: std::collections::HashSet<(String, String)> = Default::default();
    let mut drop: Vec<_> = Vec::new();
    for ac in model.ont.iter() {
        let C::InverseObjectProperties(ax) = &ac.component else { continue };
        let (Some(a), Some(b)) = (named(&ax.0), named(&ax.1)) else { continue };
        if seen.contains(&(b.clone(), a.clone())) {
            drop.push(ac.clone());
        } else {
            seen.insert((a, b));
        }
    }
    for ac in drop {
        model.ont.remove(&ac);
    }
}

/// Merge `other`'s contents into `merged` honoring `opts`: keep only the primary
/// ontology's identity (the OFN writer rejects multiple ontology IRIs);
/// ontology-level annotations from secondaries are dropped unless
/// `include_annotations` is set.
pub fn merge_into(merged: &mut Model, other: &Model, opts: &MergeOptions) {
    let source = ontology_iri(other);

    // A merge keeps the PRIMARY's identity — but where there is no primary
    // identity to keep, the first input merged in supplies it. `merge -i a -i b`
    // is still a's ontology; a merge that starts from nothing is the ontology it
    // merged.
    //
    // A rule whose recipe names its own inputs has no `$<` to open from, so the
    // pipeline starts empty: ECTO's `tmp/ecto-base-release.owl` is
    // `merge -I <the published base> remove … -o $@`, and dropping the secondary's
    // identity there left the artefact with no `owl:Ontology` at all and an
    // `xml:base` of the OWL namespace.
    // Its header comes with it: the identity, and the ontology annotations that
    // belong to that identity — title, licence, `versionInfo`. Keeping the IRI and
    // dropping the annotations would describe the ontology it merged under a
    // header stripped of everything the ontology says about itself.
    let primary_has_identity =
        merged.ont.iter().any(|c| matches!(c.component, Component::OntologyID(_)));
    if !primary_has_identity {
        for component in other.ont.iter() {
            if matches!(
                component.component,
                Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_)
            ) {
                merged.ont.insert(component.clone());
            }
        }
    }

    for component in other.ont.iter() {
        match &component.component {
            // Never carry secondary identity/imports (single-identity result).
            Component::OntologyID(_) | Component::DocIRI(_) => continue,
            // owl:imports are collapsed (dropped); owlmake merges explicit inputs
            // only, so when collapse_import_closure is true (the default) this is
            // satisfied. When false we still drop them — owlmake does not follow
            // imports, so re-emitting bare import declarations would dangle.
            Component::Import(_) => continue,
            // Secondary ontology annotations: keep iff --include-annotations.
            Component::OntologyAnnotation(_) => {
                if opts.include_annotations {
                    merged.ont.insert(component.clone());
                }
                continue;
            }
            _ => {
                merged.ont.insert(component.clone());
            }
        }
    }

    // Per-entity provenance from this secondary's own declared entities, so each
    // entity is attributed to the ontology it actually came from.
    if (opts.annotate_defined_by || opts.annotate_derived_from) && source.is_some() {
        let ents = declared_entities(other);
        annotate_provenance(merged, source.as_deref().unwrap(), &ents, opts);
    }

    // Carry over any prefixes declared by the additional inputs — both the
    // formal prefix map and the `xmlns:` bindings an RDF/XML input surfaces in
    // `idspaces` (a component's non-OBO prefix, e.g. a brain-atlas taxonomy).
    for (prefix, value) in other.prefixes.mappings() {
        let _ = merged.prefixes.add_prefix(prefix, value);
    }
    for (prefix, ns) in &other.idspaces {
        let _ = merged.prefixes.add_prefix(prefix, ns);
    }

    carry_shared_anon(merged, other);
}

/// Union a secondary input's blank-node sharing evidence into the merge result.
///
/// Two axioms are written with the SAME blank node only when they refer to one
/// node in the source RDF, which is what `owl_shared_owners` records — and that
/// evidence has to survive the merge: OBA merges an RDF/XML `merged_import`, where
/// UBERON and CL genuinely share nodes, into an OBO-derived base that has none,
/// and without the secondary's evidence every blank node downstream of the first
/// shared one is renumbered.
///
/// Public because the BUILD merges by a different route (`build::merge_file_into`,
/// which streams a file's axioms into the threaded model rather than building a
/// merge result). That route must carry this alongside prefixes and idspaces, or a
/// product like EFO's `build/efo.owl` loses MONDO's shared relax nodes and shifts
/// every blank-node id after the first of them.
pub(crate) fn carry_shared_anon(merged: &mut Model, other: &Model) {
    for (owner, keys) in &other.owl_shared_owners {
        merged.owl_shared_owners.entry(owner.clone()).or_default().extend(keys.iter().cloned());
    }
    for (owner, keys) in &other.shared_anon {
        merged.shared_anon.entry(owner.clone()).or_default().extend(keys.iter().cloned());
    }
    // Cross-owner groups (one blank node serving several classes' axioms) are
    // evidence of the same kind and die the same way without this. Group ids
    // are per-DOCUMENT numbers, so a secondary's groups are remapped past the
    // primary's — two files both using group 42 must not merge into one node.
    if !other.cross_shared.is_empty() {
        let base = merged.cross_shared.values().copied().max().map_or(0, |m| m + 1);
        for (key, grp) in &other.cross_shared {
            merged.cross_shared.entry(key.clone()).or_insert(base + grp);
        }
    }
}

/// Charge an IMPORT's blank-node allocations to the importing document's base.
///
/// Blank-node ids come from one counter shared by the whole load, and an
/// `owl:imports` is loaded as its triple streams — from the ontology header, so
/// before the importing document's body. Everything the closure consumes therefore
/// shifts the parse-time ids of the target's own anonymous individuals, which is
/// what orders them.
///
/// Deliberately separate from `carry_shared_anon`: a secondary `--input` is not
/// an import. The primary ontology is loaded first and the others are merged into
/// it, so the primary's own nodes are numbered from its own closure alone.
/// Charging a secondary here as if it were an import would number the primary's
/// blocks from the secondary's total too, and two blocks a few allocations apart
/// flip order on a single extra allocation.
pub(crate) fn charge_import_allocations(merged: &mut Model, other: &Model) {
    merged.anon_alloc_base += other.anon_alloc_total;
}

/// The named entities `model` declares (deduped, sorted).
pub(crate) fn declared_entities(model: &Model) -> Vec<String> {
    let mut entities: Vec<String> = Vec::new();
    for ac in model.ont.iter() {
        if is_declaration(&ac.component) {
            entities.extend(crate::sig::signature(&ac.component));
        }
    }
    entities.sort();
    entities.dedup();
    entities
}

/// Annotate each entity in `entities` with rdfs:isDefinedBy (`-d`) and/or
/// prov:wasDerivedFrom (`-f`) = the given `source` ontology IRI, skipping any
/// entity that already carries that property (so primary entities are not
/// re-attributed to a later source).
pub(crate) fn annotate_provenance(model: &mut Model, source: &str, entities: &[String], opts: &MergeOptions) {
    let is_defined_by = "http://www.w3.org/2000/01/rdf-schema#isDefinedBy";
    let derived_from = "http://www.w3.org/ns/prov#wasDerivedFrom";
    // A fresh IRI builder; interned IRIs compare by value across `Build`s.
    let build: horned_owl::model::Build<crate::model::Str> = horned_owl::model::Build::new();
    let src_iri = build.iri(source);

    let mut existing: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let AnnotationSubject::IRI(iri) = &aa.subject {
                existing.insert((iri.as_ref().to_string(), aa.ann.ap.0.as_ref().to_string()));
            }
        }
    }

    let mut adds: Vec<AnnotatedComponent<crate::model::Str>> = Vec::new();
    let mut props: Vec<&str> = Vec::new();
    if opts.annotate_defined_by {
        props.push(is_defined_by);
    }
    if opts.annotate_derived_from {
        props.push(derived_from);
    }
    for ent in entities {
        for prop in &props {
            if existing.contains(&(ent.clone(), (*prop).to_string())) {
                continue;
            }
            adds.push(AnnotatedComponent {
                component: Component::AnnotationAssertion(AnnotationAssertion {
                    subject: AnnotationSubject::IRI(build.iri(ent.as_str())),
                    ann: Annotation { ann: Default::default(),
                        ap: build.annotation_property(*prop),
                        av: AnnotationValue::IRI(src_iri.clone()),
                    },
                }),
                ann: Default::default(),
            });
        }
    }
    for a in adds {
        model.ont.insert(a);
    }
}

fn is_declaration(comp: &Component<crate::model::Str>) -> bool {
    matches!(
        comp,
        Component::DeclareClass(_)
            | Component::DeclareObjectProperty(_)
            | Component::DeclareDataProperty(_)
            | Component::DeclareAnnotationProperty(_)
            | Component::DeclareNamedIndividual(_)
            | Component::DeclareDatatype(_)
    )
}

pub(crate) fn ontology_iri(model: &Model) -> Option<String> {
    for ac in model.ont.iter() {
        if let Component::OntologyID(id) = &ac.component {
            if let Some(iri) = &id.iri {
                return Some(iri.as_ref().to_string());
            }
        }
    }
    None
}

/// Expand a filesystem wildcard pattern into matching files. Supports a single
/// `*` (and `?`) in the final path component, matched against directory entries by
/// the wildcard matcher below rather than through a glob dependency, so the `om`
/// binary stays self-contained. A pattern with no wildcard is returned verbatim if
/// it names an existing file.
pub(crate) fn expand_glob(pattern: &str) -> anyhow::Result<Vec<PathBuf>> {
    let path = PathBuf::from(pattern);
    if !pattern.contains('*') && !pattern.contains('?') {
        return Ok(if path.exists() { vec![path] } else { Vec::new() });
    }
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let glob = path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut out = Vec::new();
    let rd = std::fs::read_dir(&dir)
        .map_err(|e| anyhow::anyhow!("reading dir for pattern `{pattern}` ({}): {e}", dir.display()))?;
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if wildcard_match(&glob, &name) {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

/// Minimal `*`/`?` wildcard match over a single filename (no `/`), `*` matching
/// any run of characters and `?` exactly one.
fn wildcard_match(pat: &str, name: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = name.chars().collect();
    // Classic DP / two-pointer with backtracking on `*`.
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}
