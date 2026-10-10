//! `merge` — combine multiple ontologies (and optionally their import
//! closures) into a single ontology.

use std::path::PathBuf;

use clap::Args as ClapArgs;
use std::collections::{BTreeSet, HashSet};

use horned_owl::model::{
    Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, AnnotatedComponent,
    Component, Literal, MutableOntology, RcStr,
};

use crate::io;
use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    /// Input ontology paths (repeatable).
    #[arg(short = 'i', long = "input", num_args = 1..)]
    pub inputs: Vec<PathBuf>,

    /// Merge the ontologies whose file names match a wildcard pattern (`*` and
    /// `?` in the file name). The first pattern given is the one read. Bound
    /// without a short: `-p` collides with the global `-P,--prefixes`/`--prefix`,
    /// so only the long form is exposed here.
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

    /// Keep the other inputs' ontology annotations (`<bool>`, default false: by
    /// default only the first ontology's annotations survive).
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub include_annotations: Option<bool>,

    /// Merge the imports closure (`<bool>`, default true). When true, each
    /// input's `owl:imports` transitive closure is resolved (via `--catalog`, or
    /// the catalog beside the input) and merged in, then the import declarations
    /// are dropped. When false, the first ontology keeps its imports, every
    /// input contributes its own axioms only, and the other inputs' imports go.
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub collapse_import_closure: Option<bool>,

    /// Assert `rdfs:isDefinedBy` the ontology IRI of every entity an input or
    /// one of its imports names that has none (`<bool>`, default false).
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub annotate_defined_by: Option<bool>,

    /// Annotate every axiom with `prov:wasDerivedFrom` the version IRI (else the
    /// ontology IRI) of the ontology it comes from (`<bool>`, default false).
    #[arg(short = 'f', long = "annotate-derived-from", num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
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

impl MergeOptions {
    /// The options a plan's merge step states.
    pub(crate) fn of(
        include_annotations: bool,
        collapse_import_closure: bool,
        annotate_defined_by: bool,
        annotate_derived_from: bool,
    ) -> Self {
        MergeOptions { include_annotations, collapse_import_closure, annotate_defined_by, annotate_derived_from }
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
    // The inputs in the order they are read: every `--input` file, then every
    // `-I/--input-iri`, then each file the first `--inputs` pattern matches. The
    // first is the primary ontology and the rest are merged into it.
    let mut sources: Vec<Source> = args.inputs.iter().cloned().map(Source::File).collect();
    sources.extend(args.common.input_iri.iter().cloned().map(Source::Iri));
    if let Some(pattern) = args.input_globs.first() {
        sources.extend(expand_glob(pattern)?.into_iter().map(Source::File));
    }
    // Drop empty *stamp* inputs (e.g. UBERON's `tmp/bridges`, a `touch`ed marker
    // listed among a `merge`'s prerequisites): they carry no axioms, so merging
    // them is a no-op — but `io::load` would fail to determine a format.
    sources.retain(|s| !matches!(s, Source::File(p) if io::is_empty_ontology_file(p)));
    let fmt = args.common.input_format.as_deref();
    let catalog = args.common.catalog.as_deref();
    let iri_catalog = match catalog {
        Some(c) if !args.common.input_iri.is_empty() => crate::cmd::parse_catalog(c)
            .map_err(|e| e.context(format!("reading catalog {}", c.display())))?,
        _ => Default::default(),
    };
    let opts = args.options();
    let provenance = opts.annotate_defined_by || opts.annotate_derived_from;

    // Every input is read, with its imports closure, before anything is merged.
    let mut sources = sources.into_iter();
    let (mut target, banners_lent) = match piped {
        Some(m) => Input::piped(m, provenance, catalog)?,
        None => {
            let Some(primary) = sources.next() else {
                anyhow::bail!("MISSING INPUT ERROR at least one --input is required");
            };
            (primary.read(fmt, &iri_catalog, catalog)?, false)
        }
    };
    let rest = sources.map(|s| s.read(fmt, &iri_catalog, catalog)).collect::<anyhow::Result<Vec<_>>>()?;
    args.common.apply(&mut target.doc)?;

    let has_imports = !target.imports.is_empty();
    if opts.collapse_import_closure && has_imports && !banners_lent {
        // The ontologies the first input's closure holds are documents a
        // functional write's banners draw their labels from.
        if target.doc.banner_docs.is_empty() {
            target.doc.banner_docs.push(crate::cmd::banner_doc_of(&target.doc, true));
        }
        for imp in &target.imports {
            target.doc.banner_docs.push(crate::cmd::banner_doc_of(&imp.doc, false));
        }
    }
    let (mut merged, imports) = merge_inputs(target, rest, &opts)?;
    if opts.collapse_import_closure {
        if has_imports {
            crate::cmd::fold_import_labels(&mut merged);
        }
        // A collapsed merge writes ONE self-contained document: the closure's
        // axioms are the result's own.
        merged.detach_import_closure();
    } else if !imports.is_empty() {
        // The first input still imports what it imported, and its closure is
        // lent to it as to any ontology a command reads.
        if merged.banner_docs.is_empty() {
            merged.banner_docs.push(crate::cmd::banner_doc_of(&merged, true));
        }
        for imp in imports {
            crate::cmd::lend_import(&mut merged, imp.source, &imp.doc);
        }
        crate::cmd::finish_lending(&mut merged);
    }

    collapse_inverse_pairs(&mut merged);

    crate::cmd::maybe_save(&mut merged, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(merged))
}

/// One input of a merge: a file, or an ontology fetched from an IRI.
enum Source {
    File(PathBuf),
    Iri(String),
}

impl Source {
    fn load(
        &self,
        format: Option<&str>,
        catalog: &std::collections::BTreeMap<String, PathBuf>,
    ) -> anyhow::Result<Model> {
        match self {
            Source::File(path) => io::load(path),
            Source::Iri(iri) => crate::cmd::load_iri_via_catalog(iri, format, catalog),
        }
    }

    /// Read the input with its imports closure. A file's imports resolve
    /// through `catalog`, else the catalog beside it.
    fn read(
        &self,
        format: Option<&str>,
        iri_catalog: &std::collections::BTreeMap<String, PathBuf>,
        catalog: Option<&std::path::Path>,
    ) -> anyhow::Result<Input> {
        let doc = self.load(format, iri_catalog)?;
        let path = match self {
            Source::File(p) => Some(p.as_path()),
            Source::Iri(_) => None,
        };
        let (map, base) = crate::cmd::import_resolution(catalog, path)?;
        let mut read = Vec::new();
        crate::cmd::for_each_import(
            crate::cmd::imports_of(&doc),
            &crate::cmd::command_import_rule(&map, &base),
            |source, m, _| {
                read.push((source, m));
                Ok(())
            },
        )?;
        Ok(Input::new(doc, read, true))
    }
}

/// An ontology a merge reads, with each ontology of its imports closure read
/// on its own, as ROBOT's merge holds them.
pub(crate) struct Input {
    pub doc: Model,
    /// Every ontology `doc` imports, directly or not, in the order they were
    /// read.
    pub imports: Vec<Imported>,
    /// Whether `doc`'s signature still counts the entities of its imports.
    ///
    /// Reading an ontology together with its imports takes the signature of
    /// the whole closure, and until the ontology changes, that is the
    /// signature it reports as its own. So an input this merge read, and that
    /// nothing has changed since, attributes its imports' entities to itself.
    pub fresh: bool,
}

/// An ontology of an input's imports closure.
pub(crate) struct Imported {
    pub source: crate::model::ImportSource,
    pub doc: Model,
    /// The ontologies of the same closure this one imports, directly or not,
    /// whose entities its signature counts as its own (see [`Input::fresh`]).
    nested: Vec<usize>,
}

/// The entities the `i`th ontology of a closure reports as its signature.
fn imported_signature(imports: &[Imported], i: usize) -> BTreeSet<String> {
    let mut sig = entity_iris(&imports[i].doc);
    for &j in &imports[i].nested {
        sig.extend(entity_iris(&imports[j].doc));
    }
    sig
}

impl Input {
    /// `doc` with the ontologies of its imports closure, in the order they were
    /// read. `read_here` says whether the merge itself read `doc`.
    pub(crate) fn new(
        doc: Model,
        imports: Vec<(crate::model::ImportSource, Model)>,
        read_here: bool,
    ) -> Input {
        // The ontologies each import imports in turn: an import IRI names the
        // ontology read for it, and an ontology is also named by its own IRIs.
        let mut by_name: std::collections::HashMap<String, usize> = Default::default();
        for (i, (source, m)) in imports.iter().enumerate() {
            by_name.entry(source.iri.clone()).or_insert(i);
            let (iri, version) = crate::cmd::annotate::ontology_iris(m);
            for name in iri.into_iter().chain(version) {
                by_name.entry(name).or_insert(i);
            }
        }
        let direct: Vec<Vec<usize>> = imports
            .iter()
            .map(|(_, m)| crate::cmd::imports_of(m).iter().filter_map(|iri| by_name.get(iri).copied()).collect())
            .collect();
        let mut nested_of = Vec::with_capacity(imports.len());
        for i in 0..imports.len() {
            let mut nested = Vec::new();
            let mut seen: HashSet<usize> = HashSet::from([i]);
            let mut stack: Vec<usize> = direct[i].clone();
            while let Some(j) = stack.pop() {
                if seen.insert(j) {
                    nested.push(j);
                    stack.extend(direct[j].iter().copied());
                }
            }
            nested_of.push(nested);
        }
        let imports: Vec<Imported> = imports
            .into_iter()
            .zip(nested_of)
            .map(|((source, doc), nested)| Imported { source, doc, nested })
            .collect();
        let fresh = read_here && !imports.is_empty();
        Input { doc, imports, fresh }
    }

    /// The ontology a pipeline hands the merge, and whether the documents of
    /// its closure are already among those its banners draw labels from.
    ///
    /// A closure an earlier command lent it stays lent, unless the merge
    /// attributes provenance: then each ontology of it is read again, on its
    /// own. Imports nothing lent yet are read here.
    fn piped(
        mut doc: Model,
        provenance: bool,
        catalog: Option<&std::path::Path>,
    ) -> anyhow::Result<(Input, bool)> {
        if !doc.import_sources.is_empty() {
            if !provenance {
                return Ok((Input { doc, imports: Vec::new(), fresh: false }, true));
            }
            let sources = std::mem::take(&mut doc.import_sources);
            crate::cmd::restore_root_for_save(&mut doc);
            doc.detach_import_closure();
            doc.banner_docs.truncate(1);
            let mut read = Vec::new();
            for source in sources {
                let m = match &source.path {
                    Some(p) => io::load(p)?,
                    None => io::load_iri(&source.iri, None)?,
                };
                read.push((source, m));
            }
            return Ok((Input::new(doc, read, false), false));
        }
        let (map, base) = crate::cmd::import_resolution(catalog, None)?;
        let mut read = Vec::new();
        crate::cmd::for_each_import(
            crate::cmd::imports_of(&doc),
            &crate::cmd::command_import_rule(&map, &base),
            |source, m, _| {
                read.push((source, m));
                Ok(())
            },
        )?;
        Ok((Input::new(doc, read, false), false))
    }
}

/// The IRIs of the entities in `model`'s signature.
fn entity_iris(model: &Model) -> BTreeSet<String> {
    crate::io::entities::signature(model).into_iter().map(|(_, iri)| iri).collect()
}

/// The signature `doc` reports: its own, and while it is `fresh`, its imports'.
fn reported_signature(doc: &Model, fresh: bool, imports: &[Imported]) -> BTreeSet<String> {
    let mut sig = entity_iris(doc);
    if fresh {
        for imp in imports {
            sig.extend(entity_iris(&imp.doc));
        }
    }
    sig
}

/// The order an ontology's imports closure is walked in: by ontology ID, as
/// the ID is spelled `OntologyID(OntologyIRI(<…>) VersionIRI(<…>))`, compared
/// character by character, with an ontology that has no IRI first.
fn closure_order(imports: &[Imported]) -> Vec<usize> {
    let key = |m: &Model| -> Option<Vec<u16>> {
        let (iri, version) = crate::cmd::annotate::ontology_iris(m);
        iri.map(|iri| {
            format!("OntologyID(OntologyIRI(<{iri}>) VersionIRI(<{}>))", version.as_deref().unwrap_or("null"))
                .encode_utf16()
                .collect()
        })
    };
    let keys: Vec<Option<Vec<u16>>> = imports.iter().map(|i| key(&i.doc)).collect();
    let mut order: Vec<usize> = (0..imports.len()).collect();
    order.sort_by(|&a, &b| keys[a].cmp(&keys[b]).then(a.cmp(&b)));
    order
}

/// The axioms of `model` itself: every component but its name, its header
/// annotations and its imports, and nothing a lent closure contributes.
fn own_axioms(model: &Model) -> Vec<AnnotatedComponent<RcStr>> {
    model
        .ont
        .iter()
        .filter(|ac| {
            !matches!(
                ac.component,
                Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_) | Component::Import(_)
            ) && !model.imported_components.contains(*ac)
        })
        .cloned()
        .collect()
}

/// Take every axiom out of `model`, keeping its header.
fn clear_axioms(model: &mut Model) {
    for ac in own_axioms(model) {
        model.ont.remove(&ac);
    }
}

const RDFS_IS_DEFINED_BY: &str = "http://www.w3.org/2000/01/rdf-schema#isDefinedBy";
const PROV_WAS_DERIVED_FROM: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";

/// Assert `rdfs:isDefinedBy <ontology>` in `dst` of each entity of `signature`
/// outside the OWL, RDF, RDFS and XSD vocabularies that `dst` gives no
/// `rdfs:isDefinedBy`. Whether `dst` changed.
fn define(dst: &mut Model, signature: &BTreeSet<String>, ontology: &str) -> bool {
    let ap = dst.build.annotation_property(RDFS_IS_DEFINED_BY);
    let defined: HashSet<String> = dst
        .ont
        .iter()
        .filter(|ac| !dst.imported_components.contains(*ac))
        .filter_map(|ac| match &ac.component {
            Component::AnnotationAssertion(AnnotationAssertion { subject: AnnotationSubject::IRI(subject), ann })
                if ann.ap == ap =>
            {
                Some(subject.as_ref().to_string())
            }
            _ => None,
        })
        .collect();
    let value = dst.build.iri(ontology);
    let mut changed = false;
    for entity in signature {
        if crate::cmd::annotate::reserved(entity) || defined.contains(entity) {
            continue;
        }
        dst.ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(dst.build.iri(entity.as_str())),
            ann: Annotation { ann: BTreeSet::new(), ap: ap.clone(), av: AnnotationValue::IRI(value.clone()) },
        }));
        changed = true;
    }
    changed
}

/// The IRI an ontology's axioms are derived from: its version IRI, else its
/// ontology IRI. An ontology with neither is refused.
fn provenance(model: &Model) -> anyhow::Result<String> {
    let (iri, version) = crate::cmd::annotate::ontology_iris(model);
    version.or(iri).ok_or_else(|| anyhow::anyhow!("use Optional.orNull() instead of Optional.or(null)"))
}

/// Put into `dst` each of `axioms` annotated `prov:wasDerivedFrom <source>`,
/// in place of the axiom as it was. An axiom that already says what it was
/// derived from is left out, and so is the property's own declaration, which
/// `dst` gains as it is.
fn derive(dst: &mut Model, axioms: Vec<AnnotatedComponent<RcStr>>, source: &str) {
    let ap = dst.build.annotation_property(PROV_WAS_DERIVED_FROM);
    let declaration = AnnotatedComponent {
        component: Component::DeclareAnnotationProperty(horned_owl::model::DeclareAnnotationProperty(ap.clone())),
        ann: BTreeSet::new(),
    };
    let annotation = Annotation { ann: BTreeSet::new(), ap: ap.clone(), av: AnnotationValue::IRI(dst.build.iri(source)) };
    for ax in axioms {
        if ax == declaration || ax.ann.iter().any(|a| a.ap == ap) {
            continue;
        }
        let mut derived = ax.clone();
        derived.ann.insert(annotation.clone());
        dst.ont.insert(derived);
        dst.ont.remove(&ax);
    }
    dst.ont.insert(declaration);
}

/// Merge `rest` into `target`, as ROBOT's merge does, and hand back the result
/// with the ontologies of the target's closure.
///
/// Each input in turn, the target first:
/// - collapsing the imports closure, `--annotate-defined-by` attributes the
///   entities of each ontology the input imports to that ontology, inside the
///   input, and then the input's own entities to the input, in the result;
///   `--annotate-derived-from` does the same for axioms, after which the
///   ontologies it annotated are empty. The input and its closure are then
///   merged in, and at the end the result imports nothing;
/// - otherwise each input's own entities and axioms are attributed to it in the
///   result, and only its own axioms are merged in. The result keeps the
///   target's imports.
///
/// An entity that already has an `rdfs:isDefinedBy` keeps it, and an axiom that
/// already says what it was derived from is not taken from an ontology other
/// than the target.
pub(crate) fn merge_inputs(
    target: Input,
    rest: Vec<Input>,
    opts: &MergeOptions,
) -> anyhow::Result<(Model, Vec<Imported>)> {
    let collapse = opts.collapse_import_closure;
    let Input { doc: mut t, imports: mut t_imports, fresh: mut t_fresh } = target;
    if collapse {
        if opts.annotate_defined_by {
            for i in closure_order(&t_imports) {
                if let Some(iri) = ontology_iri(&t_imports[i].doc) {
                    t_fresh &= !define(&mut t, &imported_signature(&t_imports, i), &iri);
                }
            }
            if let Some(iri) = ontology_iri(&t) {
                let signature = reported_signature(&t, t_fresh, &t_imports);
                define(&mut t, &signature, &iri);
            }
        }
        if opts.annotate_derived_from {
            for i in closure_order(&t_imports) {
                let source = provenance(&t_imports[i].doc)?;
                derive(&mut t, own_axioms(&t_imports[i].doc), &source);
                clear_axioms(&mut t_imports[i].doc);
            }
            let source = provenance(&t)?;
            let axioms = own_axioms(&t);
            derive(&mut t, axioms, &source);
        }
        for imp in &t_imports {
            merge_into(&mut t, &imp.doc, &MergeOptions::default());
        }
    } else {
        if opts.annotate_defined_by {
            if let Some(iri) = ontology_iri(&t) {
                let signature = entity_iris(&t);
                define(&mut t, &signature, &iri);
            }
        }
        if opts.annotate_derived_from {
            let source = provenance(&t)?;
            let axioms = own_axioms(&t);
            derive(&mut t, axioms, &source);
        }
    }
    for input in rest {
        let Input { mut doc, mut imports, mut fresh } = input;
        if collapse {
            if opts.annotate_defined_by {
                for i in closure_order(&imports) {
                    if let Some(iri) = ontology_iri(&imports[i].doc) {
                        fresh &= !define(&mut doc, &imported_signature(&imports, i), &iri);
                    }
                }
                if let Some(iri) = ontology_iri(&doc) {
                    define(&mut t, &reported_signature(&doc, fresh, &imports), &iri);
                }
            }
            if opts.annotate_derived_from {
                for i in closure_order(&imports) {
                    let source = provenance(&imports[i].doc)?;
                    derive(&mut doc, own_axioms(&imports[i].doc), &source);
                    clear_axioms(&mut imports[i].doc);
                }
                let source = provenance(&doc)?;
                derive(&mut t, own_axioms(&doc), &source);
                clear_axioms(&mut doc);
            }
        } else {
            if opts.annotate_defined_by {
                if let Some(iri) = ontology_iri(&doc) {
                    define(&mut t, &reported_signature(&doc, fresh, &imports), &iri);
                }
            }
            if opts.annotate_derived_from {
                let source = provenance(&doc)?;
                derive(&mut t, own_axioms(&doc), &source);
                clear_axioms(&mut doc);
            }
        }
        // What an input states is the result's own, even where the target's
        // lent closure states it too.
        let own = own_axioms(&doc);
        merge_into(&mut t, &doc, opts);
        for ax in &own {
            t.imported_components.remove(ax);
        }
        if collapse {
            for imp in &imports {
                merge_into(&mut t, &imp.doc, &MergeOptions::default());
            }
        }
    }
    if collapse {
        let imports: Vec<_> =
            t.ont.iter().filter(|ac| matches!(ac.component, Component::Import(_))).cloned().collect();
        for ac in imports {
            t.ont.remove(&ac);
        }
    }
    Ok((t, t_imports))
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

/// A literal typed `xsd:string` and the same text untyped are one literal, so
/// two axioms that differ only there are one axiom. This is the form the
/// comparison is made in: every `xsd:string` literal of an axiom untyped.
fn untyped_strings(ac: &AnnotatedComponent<RcStr>) -> Option<AnnotatedComponent<RcStr>> {
    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
    let mut changed = false;
    let mut av = |v: &AnnotationValue<RcStr>| -> AnnotationValue<RcStr> {
        match v {
            AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri })
                if datatype_iri.as_ref() == XSD_STRING =>
            {
                changed = true;
                AnnotationValue::Literal(Literal::Simple { literal: literal.clone() })
            }
            other => other.clone(),
        }
    };
    let ann: BTreeSet<Annotation<RcStr>> =
        ac.ann.iter().map(|a| Annotation { ap: a.ap.clone(), av: av(&a.av), ann: a.ann.clone() }).collect();
    let component = match &ac.component {
        Component::AnnotationAssertion(aa) => Component::AnnotationAssertion(AnnotationAssertion {
            subject: aa.subject.clone(),
            ann: Annotation { ap: aa.ann.ap.clone(), av: av(&aa.ann.av), ann: aa.ann.ann.clone() },
        }),
        Component::OntologyAnnotation(oa) => Component::OntologyAnnotation(
            horned_owl::model::OntologyAnnotation(Annotation { ap: oa.0.ap.clone(), av: av(&oa.0.av), ann: oa.0.ann.clone() }),
        ),
        c => c.clone(),
    };
    changed.then_some(AnnotatedComponent { component, ann })
}

/// The axioms already in a merge result, in the form two axioms are compared
/// in: an incoming axiom that is already there under either typing of its
/// strings is not added again.
pub(crate) struct MergedAxioms {
    untyped: HashSet<AnnotatedComponent<RcStr>>,
}

impl MergedAxioms {
    pub(crate) fn of(merged: &Model) -> Self {
        MergedAxioms { untyped: merged.ont.iter().filter_map(untyped_strings).collect() }
    }

    /// Whether `ac` is already in `merged`, under any typing of its strings.
    pub(crate) fn holds(&self, merged: &Model, ac: &AnnotatedComponent<RcStr>) -> bool {
        match untyped_strings(ac) {
            Some(u) => merged.ont.i().contains(&u) || self.untyped.contains(&u),
            None => self.untyped.contains(ac),
        }
    }

    /// `ac` has been added to `merged`.
    pub(crate) fn added(&mut self, ac: &AnnotatedComponent<RcStr>) {
        if let Some(u) = untyped_strings(ac) {
            self.untyped.insert(u);
        }
    }
}

/// Merge `other`'s contents into `merged` honoring `opts`: keep only the primary
/// ontology's identity (the OFN writer rejects multiple ontology IRIs);
/// ontology-level annotations from secondaries are dropped unless
/// `include_annotations` is set.
pub fn merge_into(merged: &mut Model, other: &Model, opts: &MergeOptions) {
    let mut present = MergedAxioms::of(merged);

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
            // Another ontology's imports are its own: a merge keeps the
            // primary's alone, or none once the closure is collapsed.
            Component::Import(_) => continue,
            // Secondary ontology annotations: keep iff --include-annotations.
            Component::OntologyAnnotation(_) => {
                if opts.include_annotations {
                    merged.ont.insert(component.clone());
                }
                continue;
            }
            _ => {
                if present.holds(merged, component) {
                    continue;
                }
                merged.ont.insert(component.clone());
                present.added(component);
            }
        }
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

/// The files an `--inputs` pattern names: those in the pattern's directory
/// whose names match its last component, where `*` matches any run of
/// characters and `?` exactly one. A pattern with neither is refused. A
/// directory that does not exist, or a pattern nothing matches, names no file
/// and says so. The files come in name order.
pub(crate) fn expand_glob(pattern: &str) -> anyhow::Result<Vec<PathBuf>> {
    if !pattern.contains('*') && !pattern.contains('?') {
        anyhow::bail!("WILDCARD ERROR --inputs argument must be a quoted wildcard pattern");
    }
    let path = PathBuf::from(pattern);
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if !dir.is_dir() {
        status!("ERROR '{}' is not a valid directory for --inputs pattern", dir.display());
        return Ok(Vec::new());
    }
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
    if out.is_empty() {
        status!("ERROR No files match pattern: {pattern}");
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
