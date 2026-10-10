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
    /// Load the input ontology from an IRI instead of a file. Repeatable: a
    /// command that reads one input reads the first, and `merge` reads every
    /// one, after its `--input` files.
    #[arg(short = 'I', long = "input-iri", value_name = "IRI", action = clap::ArgAction::Append)]
    pub input_iri: Vec<String>,

    /// Override the input parser format.
    #[arg(long = "input-format", value_name = "FORMAT")]
    pub input_format: Option<String>,

    /// Bind the prefixes of a JSON-LD context file for reading CURIEs.
    #[arg(short = 'P', long = "prefixes", value_name = "FILE")]
    pub prefixes: Option<std::path::PathBuf>,

    /// Bind a prefix `"foo: http://bar"` for reading CURIEs (repeatable). The `-p`
    /// short is bound per-command, where it is free.
    #[arg(long = "prefix", value_name = "PREFIX")]
    pub prefix: Vec<String>,

    /// Bind a prefix as `--prefix` does, and declare it in the output as well
    /// (repeatable).
    #[arg(long = "add-prefix", value_name = "PREFIX")]
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
    /// sibling file, else fetching the import IRI over the network. An import none
    /// of these resolves fails the command (see [`for_each_import`]). `input` is the
    /// main document's path, used to resolve catalog-relative and default-local
    /// paths.
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

    /// The prefixes this command line supplies, in the order given: those of each
    /// `--prefixes`/`--add-prefixes` context file (both JSON-LD forms, a bare
    /// namespace string and `{"@id": …, "@prefix": true}`), then each
    /// `--prefix`/`--add-prefix "name: iri"`.
    pub fn given_prefixes(&self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for file in self.prefixes.iter().chain(self.add_prefixes.iter()) {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("reading prefixes file {}", file.display()))?;
            let json: serde_json::Value = serde_json::from_str(&text)
                .with_context(|| format!("parsing prefixes JSON {}", file.display()))?;
            let ctx = json.get("@context").unwrap_or(&json);
            for (k, v) in ctx.as_object().into_iter().flatten() {
                let ns = v.as_str().or_else(|| v.get("@id").and_then(|x| x.as_str()));
                if let Some(ns) = ns {
                    out.push((k.clone(), ns.to_string()));
                }
            }
        }
        for spec in self.prefix.iter().chain(&self.add_prefix) {
            out.push(Self::binding(spec)?);
        }
        Ok(out)
    }

    /// Bind this command line's prefixes into `context`, the one a CURIE it is
    /// given is read with: a `--prefixes` file in place of the built-in map, or
    /// none under `--noprefixes`, then each `--add-prefixes` file, `--prefix` and
    /// `--add-prefix`, in that order.
    pub fn bind(&self, context: &mut crate::context::Context) -> Result<()> {
        if let Some(file) = &self.prefixes {
            *context = crate::context::Context::without_builtin();
            let only = CommonArgs { prefixes: Some(file.clone()), ..Default::default() };
            for (name, ns) in only.given_prefixes()? {
                context.bind(&name, &ns);
            }
        } else if self.noprefixes {
            *context = crate::context::Context::without_builtin();
        }
        for file in &self.add_prefixes {
            let only = CommonArgs { add_prefixes: vec![file.clone()], ..Default::default() };
            for (name, ns) in only.given_prefixes()? {
                context.bind(&name, &ns);
            }
        }
        for spec in self.prefix.iter().chain(&self.add_prefix) {
            let (name, ns) = Self::binding(spec)?;
            context.bind(&name, &ns);
        }
        Ok(())
    }

    fn binding(spec: &str) -> Result<(String, String)> {
        let (name, ns) = spec
            .split_once(':')
            .with_context(|| format!("bad --prefix (want \"name: iri\"): {spec}"))?;
        Ok((name.trim().to_string(), ns.trim().to_string()))
    }

    /// Apply prefix-affecting options to a freshly loaded model: they land after
    /// loading, on top of the document's own prefixes (`--noprefixes` clears the
    /// built-in defaults first).
    ///
    /// Every prefix given binds a name for reading the CURIEs this command is
    /// given ([`CommonArgs::bind`]), and joins the document's own prefix map.
    /// `--prefix` and `--prefixes` do nothing more: nothing written declares
    /// them. An ADDED prefix (`--add-prefix`, `--add-prefixes`) is also declared
    /// by what this command writes, used or not, even by an ontology built from
    /// nothing, which declares no other. An OBO document takes added prefixes as
    /// idspaces only when it is cleaned (see `convert::apply_clean_obo`).
    pub fn apply(&self, model: &mut Model) -> Result<()> {
        self.bind(&mut model.context)?;
        if self.noprefixes {
            model.prefixes = PrefixMapping::default();
        }
        for (name, ns) in self.given_prefixes()? {
            let _ = model.prefixes.add_prefix(&name, &ns);
        }
        let mut added = Vec::new();
        for file in &self.add_prefixes {
            let only = CommonArgs { add_prefixes: vec![file.clone()], ..Default::default() };
            added.extend(only.given_prefixes()?);
        }
        for spec in &self.add_prefix {
            added.push(Self::binding(spec)?);
        }
        for (name, ns) in added {
            match model.added_prefixes.iter_mut().find(|(p, _)| *p == name) {
                Some(slot) => slot.1 = ns,
                None => model.added_prefixes.push((name, ns)),
            }
        }
        Ok(())
    }
}

/// Load an `-I/--input-iri` ontology. A catalog decides where an IRI is read
/// from, for an input exactly as for an import: an IRI the catalog maps is the
/// file it maps it to, and a mapped file that is missing is an error, never a
/// fetch. Only an IRI the catalog does not map is fetched.
pub(crate) fn load_iri_via_catalog(
    iri: &str,
    format: Option<&str>,
    catalog: &std::collections::BTreeMap<String, std::path::PathBuf>,
) -> Result<Model> {
    match catalog_resolve(catalog, iri) {
        Some(path) => io::load_with(&path, format)
            .with_context(|| format!("loading {iri} from {}, where the catalog maps it", path.display())),
        None => io::load_iri(iri, format),
    }
}

/// The one ontology a single-input command reads: its `--input` file or its
/// `-I/--input-iri`, the IRI read through `--catalog` when one is given. Naming
/// more than one is an error.
fn single_input(input: Option<&Path>, common: &CommonArgs) -> Result<(Model, String)> {
    let fmt = common.input_format.as_deref();
    let given = usize::from(input.is_some()) + common.input_iri.len();
    if given > 1 {
        bail!("only one --input or --input-iri may be given; `merge` combines several");
    }
    if let Some(iri) = common.input_iri.first() {
        let catalog = match common.catalog.as_deref() {
            Some(c) => parse_catalog(c).with_context(|| format!("reading catalog {}", c.display()))?,
            None => Default::default(),
        };
        return Ok((load_iri_via_catalog(iri, fmt, &catalog)?, iri.clone()));
    }
    let path = input
        .context("missing input: provide --input/--input-iri or pipe from a previous command")?;
    Ok((io::load_with(path, fmt)?, path.display().to_string()))
}

/// Resolve the working model for a command: use the model piped from the previous
/// chain step if present, otherwise load `--input` (or `--input-iri`). Honors the
/// global `--input-format` override. Errors when no source exists.
pub fn take_or_load(piped: Option<Model>, input: Option<&Path>, common: &CommonArgs) -> Result<Model> {
    common.activate();
    if let Some(model) = piped {
        return Ok(model);
    }
    let (mut model, src) = single_input(input, common)?;
    common.apply_catalog(&mut model, input)?;
    if crate::progress::verbosity() >= 1 {
        status!("loaded {}: {} axioms", src, model.ont.iter().count());
    }
    Ok(model)
}

/// Read the entities of the imports closure of `model`, a document loaded
/// without its imports, unless they are known already: a document that
/// imports is written among the ontologies it imports (see
/// [`crate::model::ImportsClosure`]). The closure is resolved from a scratch
/// document carrying only the root's `Import(...)`s, so the root's own
/// signature never counts as the closure's. An import that resolves nowhere
/// fails, as it fails every load of the document. The documents the closure
/// reads are ones a functional write's banners draw their labels from, as
/// they are for a document read with its imports merged.
pub(crate) fn read_imports_closure(
    model: &mut Model,
    input: Option<&Path>,
    common: &CommonArgs,
) -> Result<()> {
    if model.imports_closure.is_some() {
        return Ok(());
    }
    if let Some(imports) = read_imports(model, input, None, common)? {
        if model.banner_docs.is_empty() {
            model.banner_docs.push(banner_doc_of(model, true));
            model.banner_docs.extend(imports.banner_docs.into_iter().filter(|d| !d.root));
        }
    }
    Ok(())
}

/// Read the imports closure of `model`, a document loaded without its imports,
/// where `catalog` resolves each import, else the command's own catalog, else
/// the catalog beside `input`, and record its entities on `model` (see
/// [`read_imports_closure`]). Returns the closure's ontologies merged, with a
/// banner document for each, or `None` when `model` imports nothing. An import
/// that resolves nowhere fails.
pub(crate) fn read_imports(
    model: &mut Model,
    input: Option<&Path>,
    catalog: Option<&Path>,
    common: &CommonArgs,
) -> Result<Option<Model>> {
    use horned_owl::model::MutableOntology;

    let mut imports = Model::new();
    for ac in model.ont.iter() {
        if matches!(ac.component, horned_owl::model::Component::Import(_)) {
            imports.ont.insert(ac.clone());
        }
    }
    if imports.ont.iter().next().is_none() {
        return Ok(None);
    }
    match catalog {
        Some(catalog) => merge_import_closure(&mut imports, catalog, input)?,
        None => common.apply_catalog(&mut imports, input)?,
    }
    model.imports_closure = imports.imports_closure.clone();
    Ok(Some(imports))
}

/// Like [`take_or_load`] but WITHOUT merging the `owl:imports` closure.
///
/// For commands that operate only on a document's own axioms and must keep its
/// `Import(...)` declarations rather than inline the imported ontologies — e.g.
/// `kgcl:mint … convert`, where only the root is serialised, so the edit file
/// keeps its import declarations instead of being flattened into its whole
/// closure. The document is still read with its imports closure
/// ([`read_imports_closure`]): an import that resolves nowhere fails the load,
/// and the closure decides what the root writes of an entity it only names. A
/// piped model is used as the command before it left it.
pub fn take_or_load_no_imports(
    piped: Option<Model>,
    input: Option<&Path>,
    common: &CommonArgs,
) -> Result<Model> {
    common.activate();
    if let Some(model) = piped {
        return Ok(model);
    }
    let (mut model, _) = single_input(input, common)?;
    read_imports_closure(&mut model, input, common)?;
    Ok(model)
}

/// Return every process-wide option to its default, so one invocation cannot
/// inherit another's.
///
/// These options are process-wide because they are read deep inside the loaders
/// and writers, where threading them through every call would reach into
/// horned-owl's serializers. That is workable only because each of them is
/// established at the START of a run — latched from the command line by
/// [`CommonArgs::activate`], or set from the plan by
/// `build::set_robot_behaviours` (in a process a build starts, from the
/// arguments it is started with) — and this is the point at which "the start of
/// a run" is defined.
pub fn reset_invocation_options() {
    crate::io::set_run_options(crate::io::RunOptions::default());
    crate::build::set_emulation(None);
}

/// A failure a command has already reported on the console in its own words.
/// The run exits 1 and prints nothing more for it; anything that reports errors
/// itself, such as a build step, still reads the message.
#[derive(Debug)]
pub struct Reported(pub String);

impl std::fmt::Display for Reported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Reported {}

/// Whether an option's value switches it on: `true` or `yes`, in any case and
/// with white space around it. Any other value switches it off.
pub fn option_is_true(value: &str) -> bool {
    matches!(value.trim().to_lowercase().as_str(), "true" | "yes")
}

/// [`option_is_true`] as an argument parser: such an option takes any value.
pub fn parse_option_true(value: &str) -> Result<bool, std::convert::Infallible> {
    Ok(option_is_true(value))
}

/// The message refusing a value of switch `name` that is neither `true` nor
/// `false`.
pub fn boolean_value_error(name: &str) -> String {
    format!("BOOLEAN VALUE ERROR arg for {name} must be true or false")
}

/// `value` read as switch `name` (named without hyphens): `true` or `false`,
/// exactly.
pub fn read_bool(name: &str, value: &str) -> std::result::Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(boolean_value_error(name)),
    }
}

/// A switch as a command line gives it. Text that is neither `true` nor
/// `false` is kept as written, and fails the command where the command reads
/// the switch.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Switch {
    Bool(bool),
    Text(String),
}

impl Switch {
    pub fn parse(text: &str) -> Switch {
        match read_bool("", text) {
            Ok(on) => Switch::Bool(on),
            Err(_) => Switch::Text(text.to_string()),
        }
    }

    /// The value of switch `name` (named without hyphens) given as `switch`,
    /// or `default` when it is not given.
    pub fn read(switch: Option<&Switch>, name: &str, default: bool) -> Result<bool> {
        match switch {
            None => Ok(default),
            Some(Switch::Bool(on)) => Ok(*on),
            Some(Switch::Text(_)) => bail!(boolean_value_error(name)),
        }
    }
}

impl From<bool> for Switch {
    fn from(on: bool) -> Switch {
        Switch::Bool(on)
    }
}

impl std::fmt::Display for Switch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Switch::Bool(on) => write!(f, "{on}"),
            Switch::Text(text) => write!(f, "{text:?}"),
        }
    }
}

/// The argument parser for a switch the command reads later: it takes any
/// text ([`Switch::parse`]).
#[derive(Clone, Copy, Debug)]
pub struct SwitchParser;

impl clap::builder::TypedValueParser for SwitchParser {
    type Value = Switch;

    fn parse_ref(
        &self,
        _cmd: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> std::result::Result<Switch, clap::Error> {
        Ok(Switch::parse(&value.to_string_lossy()))
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(["true", "false"].into_iter().map(clap::builder::PossibleValue::new)))
    }
}

/// The argument parser for a switch: `true` or `false`, exactly, and any other
/// value refused with the message naming the option ([`read_bool`]).
#[derive(Clone, Copy, Debug)]
pub struct BoolParser;

impl clap::builder::TypedValueParser for BoolParser {
    type Value = bool;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> std::result::Result<bool, clap::Error> {
        let name = arg.and_then(|a| a.get_long()).unwrap_or_default();
        read_bool(name, &value.to_string_lossy())
            .map_err(|message| clap::Error::raw(clap::error::ErrorKind::InvalidValue, format!("{message}\n")).with_cmd(cmd))
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(["true", "false"].into_iter().map(clap::builder::PossibleValue::new)))
    }
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
        // A discard path writes nothing: the caller ran the command for its
        // verdict, not its document. The `--format` name is still validated —
        // throwing the output away is not a reason to accept a format that does
        // not exist.
        if io::is_discard_path(out) {
            if let Some(name) = format {
                Format::from_name(name)?;
            }
            if crate::progress::verbosity() >= 1 {
                status!("discarding output ({}): {} axioms", out.display(), model.ont.iter().count());
            }
            return Ok(());
        }
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

    let rule = command_import_rule(&map, &base);
    resolve_import_closure(model, &rule)
}

/// Resolve and inline the `owl:imports` transitive closure, each import where
/// `resolve` finds it (see [`for_each_import`]). Inlined imports are dropped so
/// the result is self-contained. Used by both the `--catalog` path and `merge`'s
/// implicit closure following.
pub(crate) fn resolve_import_closure(model: &mut Model, resolve: &ImportRule) -> Result<()> {
    let queue: Vec<String> = imports_of(model);
    // Every document opened with this one is one more a functional write's
    // banners draw their labels from; the document itself gives the labels it
    // has when it is written.
    if !queue.is_empty() && model.banner_docs.is_empty() {
        model.banner_docs.push(crate::cmd::banner_doc_of(model, true));
    }
    let mut merged_any = false;
    for_each_import(queue, resolve, |source, imported, from| {
        let iri = source.iri.clone();
        lend_import(model, source, &imported);
        merged_any = true;
        if crate::progress::verbosity() >= 1 {
            status!("imports: merged import <{iri}> from {from}");
        }
        Ok(())
    })?;
    if merged_any {
        finish_lending(model);
    }
    Ok(())
}

/// Where an import IRI is read from: the file `Ok(Some(..))` names, or with
/// `Ok(None)` the IRI itself, fetched over the network.
pub(crate) type ImportRule<'a> = dyn Fn(&str) -> Result<Option<std::path::PathBuf>> + 'a;

/// A command's rule for an import: through `map`, else the file a `file:` IRI
/// names, else a document of its name beside the importer under `base`, else the
/// IRI over the network.
pub(crate) fn command_import_rule<'a>(
    map: &'a std::collections::BTreeMap<String, std::path::PathBuf>,
    base: &'a Path,
) -> impl Fn(&str) -> Result<Option<std::path::PathBuf>> + 'a {
    move |iri| {
        Ok(catalog_resolve(map, iri)
            .or_else(|| io::file_iri_path(iri))
            .or_else(|| default_local(iri, base)))
    }
}

/// Read each ontology of an imports closure, starting from the imports `iris`
/// names, in the order they are followed, and hand each to `each` with where it
/// was read from (a path, or the IRI fetched). Each import is read where
/// `resolve` finds it.
pub(crate) fn for_each_import(
    iris: Vec<String>,
    resolve: &ImportRule,
    mut each: impl FnMut(crate::model::ImportSource, Model, String) -> Result<()>,
) -> Result<()> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let direct: std::collections::HashSet<String> = iris.iter().cloned().collect();
    // Say that this ran, and with how many imports, BEFORE resolving any. The
    // per-import lines below are printed only when there is something to print,
    // so their absence would otherwise be ambiguous between "this path resolves
    // no closure" and "this path was never asked to" — and a reader cannot tell
    // which. Silence must not be the answer to a question the flag was asked.
    if std::env::var("OM_IMPORT_DEBUG").is_ok() {
        eprintln!("[import] resolving closure: {} direct import(s)", iris.len());
    }
    let mut queue = iris;
    while let Some(iri) = queue.pop() {
        if !seen.insert(iri.clone()) {
            continue;
        }
        let path = resolve(&iri)?;
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
        let source = crate::model::ImportSource {
            iri: iri.clone(),
            path: path.clone(),
            direct: direct.contains(&iri),
        };
        let (imported, from) = match path {
            Some(path) => {
                let m = crate::io::load(&path)
                    .with_context(|| format!("loading import <{iri}> from {}", path.display()))?;
                (m, path.display().to_string())
            }
            None => {
                // No file resolved the import, so fall back to fetching the import
                // IRI over the network.
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
        // Follow nested imports too.
        for nested in imports_of(&imported) {
            if !seen.contains(&nested) {
                queue.push(nested);
            }
        }
        if std::env::var("OM_IMPORT_DEBUG").is_ok() {
            eprintln!(
                "[import] <{iri}> -> {from} ({} components)",
                imported.ont.iter().count()
            );
        }
        each(source, imported, from)?;
    }
    Ok(())
}

/// Lend `model` the content of an ontology it imports, read from `source`,
/// recording what was lent so that a save of the root can take it back out.
pub(crate) fn lend_import(model: &mut Model, source: crate::model::ImportSource, imported: &Model) {
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
    // The closure's entities decide what the root, written among its
    // imports, states of an entity it only names. The save drops the
    // borrowed axioms again, which is exactly when this record is the only
    // thing left that knows.
    model.import_sources.push(source.clone());
    model.imports_closure.get_or_insert_with(Default::default).add(source, imported);
    model.banner_docs.push(crate::cmd::banner_doc_of(imported, false));
    crate::cmd::merge::merge_into(model, imported, &crate::cmd::merge::MergeOptions::default());
    for c in borrowed {
        if model.ont.i().contains(&c) {
            model.imported_components.insert(c);
        }
    }
}

/// Close the lending of a model's imports: its import declarations stand for
/// content it now holds, and the labels its imports give are gathered.
pub(crate) fn finish_lending(model: &mut Model) {
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
    fold_import_labels(model);
}

/// Gather the labels a model's imports give, for a writer naming an entity the
/// document does not label itself: an edit file that only DECLARES a class is
/// still commented with the label its imported pattern module asserts.
pub(crate) fn fold_import_labels(model: &mut Model) {
    if model.banner_labels.is_empty() {
        let (iri, version) = crate::build::model_ontology_id(model);
        let none = std::collections::HashMap::new();
        model.banner_labels = fold_banner_docs(&model.banner_docs, iri.as_deref(), version.as_deref(), &none);
    }
}

/// Every `entity IRI → label` the model asserts, as a functional write's banner
/// and a report name the entity by: the label
/// [`held_labels`](crate::io::entities::held_labels) picks, a literal's text or
/// an IRI value's short form.
///
/// Where two labels land in the same slot of the entity's set, no order can
/// settle which one names it, because the reference has no stable answer to
/// match. Two runs of the reference over one unchanged tree, minutes apart,
/// write `imports/merged_import.owl` with `database_cross_reference` and then
/// with `has cross-reference`, the two 41 MB documents otherwise
/// byte-identical, and both values have been seen twice: a Java bucket holds
/// its members in insertion order, and the pipeline does not add its axioms in
/// a fixed one. owlmake always writes the same value.
pub(crate) fn rdfs_labels(model: &Model) -> std::collections::HashMap<String, String> {
    crate::io::entities::held_labels(model).into_iter().map(|(s, l)| (s.to_string(), l.short_form())).collect()
}

/// Every `entity IRI → label` the model asserts, with the label's kind, as a
/// document among several gives it (see [`fold_labels`]).
pub(crate) fn doc_labels(model: &Model) -> std::collections::HashMap<String, crate::model::DocLabel> {
    use crate::io::entities::HeldLabel;
    use crate::model::DocLabel;
    crate::io::entities::held_labels(model)
        .into_iter()
        .map(|(s, l)| {
            let label = match l {
                HeldLabel::Literal(text) => DocLabel::Literal(text.to_string()),
                HeldLabel::Iri(iri) => DocLabel::Iri(iri.to_string()),
            };
            (s.to_string(), label)
        })
        .collect()
}

/// The labels several documents give, in the order they are consulted: an
/// entity takes the first literal label any of them gives it, and failing one,
/// the last IRI.
pub(crate) fn fold_labels<'d>(
    docs: impl IntoIterator<Item = &'d std::collections::HashMap<String, crate::model::DocLabel>>,
) -> std::collections::HashMap<String, crate::model::DocLabel> {
    use crate::model::DocLabel;
    let mut out: std::collections::HashMap<String, DocLabel> = std::collections::HashMap::new();
    for labels in docs {
        for (subj, label) in labels {
            let taken = matches!(out.get(subj), Some(DocLabel::Literal(_)));
            if !taken {
                out.insert(subj.clone(), label.clone());
            }
        }
    }
    out
}

/// The banner-label document for `model` as it stands.
pub(crate) fn banner_doc_of(model: &Model, root: bool) -> crate::model::BannerDoc {
    let (iri, version) = crate::build::model_ontology_id(model);
    crate::model::BannerDoc { iri, version, labels: std::sync::Arc::new(doc_labels(model)), root }
}

/// The label a functional write banners each entity with, over every loaded
/// document: the documents stand in the order a set of them is iterated in,
/// keyed on each one's identity, and [`fold_labels`] settles each entity's
/// label over them. The document being written is the root, under the
/// identity it is written with (`root_iri`/`root_version`), and its labels are
/// the ones it carries as written (`root_labels`): an entity's set of
/// annotation assertions is sized by what the entity holds when the write
/// asks for it, whatever it held when the document was loaded. Every other
/// document keeps the labels it was loaded with.
pub(crate) fn fold_banner_docs(
    docs: &[crate::model::BannerDoc],
    root_iri: Option<&str>,
    root_version: Option<&str>,
    root_labels: &std::collections::HashMap<String, crate::model::DocLabel>,
) -> std::collections::HashMap<String, String> {
    let root_id = (root_iri.map(str::to_string), root_version.map(str::to_string));
    let mut seen: std::collections::HashSet<(Option<String>, Option<String>)> = Default::default();
    seen.insert(root_id.clone());
    let others: Vec<&crate::model::BannerDoc> = docs
        .iter()
        .filter(|d| !d.root)
        .filter(|d| d.iri.is_none() || seen.insert((d.iri.clone(), d.version.clone())))
        .collect();
    let mut hashes: Vec<i32> = vec![crate::owlapi_hash::ontology_id_hash(root_iri, root_version)];
    hashes.extend(others.iter().map(|d| crate::owlapi_hash::ontology_id_hash(d.iri.as_deref(), d.version.as_deref())));
    let ordered = crate::owlapi_hash::ontology_set_order(&hashes)
        .into_iter()
        .map(|i| if i == 0 { root_labels } else { &*others[i - 1].labels });
    fold_labels(ordered).into_iter().map(|(subj, label)| (subj, label.short_form())).collect()
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
    let (map, base) = import_resolution(catalog, input)?;
    let rule = command_import_rule(&map, &base);
    resolve_import_closure(model, &rule)
}

/// The catalog mapping and the base directory a document's imports resolve
/// against: the `catalog` named, else the `catalog-v001.xml` beside the
/// document at `input`, and the directory of whichever catalog that is, else
/// of the document.
pub(crate) fn import_resolution(
    catalog: Option<&Path>,
    input: Option<&Path>,
) -> Result<(std::collections::BTreeMap<String, std::path::PathBuf>, std::path::PathBuf)> {
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
    Ok((map, base))
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
pub(crate) fn imports_of(model: &Model) -> Vec<String> {
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
pub(crate) fn parse_catalog(path: &Path) -> Result<std::collections::BTreeMap<String, std::path::PathBuf>> {
    let text = std::fs::read_to_string(path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut map = std::collections::BTreeMap::new();
    // `<rewriteURI uriStartString="IRI-PREFIX" rewritePrefix="PATH-PREFIX"/>`:
    // every IRI under the prefix maps to the path under the other, which is how
    // one catalog line covers a directory of import shards.
    for frag in text.split("<rewriteURI").skip(1) {
        let tag = frag.split('>').next().unwrap_or(frag);
        if let (Some(start), Some(prefix)) = (attr(tag, "uriStartString"), attr(tag, "rewritePrefix")) {
            let prefix = crate::build::percent_decode(&prefix);
            let p = std::path::Path::new(&prefix);
            let resolved = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
            map.insert(format!("{CATALOG_REWRITE_KEY}{start}"), resolved);
        }
    }
    for frag in text.split("<uri").skip(1) {
        let tag = frag.split('>').next().unwrap_or(frag);
        let (name, uri) = match (attr(tag, "name"), attr(tag, "uri")) {
            (Some(n), Some(u)) => (n, u),
            _ => continue,
        };
        let uri = crate::build::percent_decode(&uri);
        let p = std::path::Path::new(&uri);
        let resolved = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
        map.insert(name, resolved);
    }
    Ok(map)
}

/// Marker prefix under which a catalog map carries its `rewriteURI` rules: the
/// key is the marker plus the IRI prefix, the value the path prefix. Exact
/// `<uri>` entries are plain keys, and [`catalog_resolve`] consults both.
pub(crate) const CATALOG_REWRITE_KEY: &str = "\u{2}rewriteURI\u{2}";

/// Resolve an import IRI against a catalog map: an exact `<uri>` entry first,
/// else the longest `rewriteURI` prefix that matches, with the rest of the IRI
/// appended to its path prefix.
pub(crate) fn catalog_resolve(
    map: &std::collections::BTreeMap<String, std::path::PathBuf>,
    iri: &str,
) -> Option<std::path::PathBuf> {
    if let Some(p) = map.get(iri) {
        return Some(p.clone());
    }
    map.iter()
        .filter_map(|(k, v)| {
            let start = k.strip_prefix(CATALOG_REWRITE_KEY)?;
            let rest = iri.strip_prefix(start)?;
            Some((start.len(), format!("{}{}", v.display(), rest)))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| std::path::PathBuf::from(p))
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

pub mod annotate;
pub mod explain_axiom;
pub mod explain_blackbox;
pub mod explain_markdown;
pub(crate) mod manchester_markdown;
pub mod explain_unsat;
pub mod babelon;
pub mod babelon_tsv;
pub mod collapse;
// The repo-hygiene commands — not ontology transformations, but gates a release
// has to pass, and native because the `om` binary is all a repo has: ID-policy
// (`<ont>-idranges.owl`) and DOSDP-pattern validation, RDF/XML parseability of
// each product, the build's tool inventory, and checksums. See each module's
// header for what it checks.
pub mod check_align;
pub mod check_rdfxml;
pub mod context2csv;
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
pub mod obo_grep;
pub mod merge;
pub mod merge_equivalent_sets;
pub mod merge_species;
pub mod create_species_subset;
pub mod mint;
pub mod objects;
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
// `make-release-assets.py` talks to GitHub, and there is no HTTP client on wasm.
#[cfg(not(target_arch = "wasm32"))]
pub mod release_assets;
pub mod subset;
pub mod template;
pub mod tsvalid;
pub mod ubergraph;
pub mod unmerge;
pub mod validate_id_ranges;
pub mod validate_patterns;
pub mod validate_profile;
pub mod verify;

#[cfg(test)]
mod catalog_tests {
    /// A catalog's escapes are bytes of UTF-8: `caf%C3%A9_import.owl` names
    /// `café_import.owl`.
    #[test]
    fn a_catalog_uri_decodes_to_the_file_it_names() {
        let dir = std::env::temp_dir().join(format!("om-catalog-uri-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let catalog = dir.join("catalog-v001.xml");
        std::fs::write(
            &catalog,
            "<catalog xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
             <uri name=\"http://example.org/cafe.owl\" uri=\"imports/caf%C3%A9_import.owl\"/>\n\
             </catalog>\n",
        )
        .unwrap();
        let map = super::parse_catalog(&catalog).unwrap();
        assert_eq!(map["http://example.org/cafe.owl"], dir.join("imports/café_import.owl"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;

    /// A switch read leniently is on for `true` or `yes`, in any case and with
    /// white space around it, and off for anything else.
    #[test]
    fn a_lenient_switch_is_on_for_true_or_yes() {
        for value in ["true", "TRUE", " True ", "yes", "Yes"] {
            assert!(option_is_true(value), "{value:?}");
        }
        for value in ["false", "False", "no", "1", "on", "", "nope"] {
            assert!(!option_is_true(value), "{value:?}");
        }
    }

    /// A switch read strictly is `true` or `false` exactly; anything else is
    /// refused with the message naming the switch.
    #[test]
    fn a_strict_switch_is_true_or_false_exactly() {
        assert_eq!(read_bool("trim", "true"), Ok(true));
        assert_eq!(read_bool("trim", "false"), Ok(false));
        for value in ["TRUE", "False", " true", "yes", ""] {
            assert_eq!(
                read_bool("trim", value),
                Err("BOOLEAN VALUE ERROR arg for trim must be true or false".to_string()),
                "{value:?}"
            );
        }
        assert_eq!(Switch::parse("true"), Switch::Bool(true));
        assert_eq!(Switch::parse("TRUE"), Switch::Text("TRUE".into()));
        assert!(!Switch::read(None, "trim", false).unwrap());
        assert!(Switch::read(None, "trim", true).unwrap());
        assert!(!Switch::read(Some(&Switch::Bool(false)), "trim", true).unwrap());
        assert_eq!(
            Switch::read(Some(&Switch::parse("TRUE")), "preserve-structure", true).unwrap_err().to_string(),
            "BOOLEAN VALUE ERROR arg for preserve-structure must be true or false"
        );
    }
}
