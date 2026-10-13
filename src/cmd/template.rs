//! `template` — build an ontology from a template table (TSV/CSV).
//!
//! Row 1 holds the human-readable headers, row 2 the template strings, rows 3+
//! the data. A template string says what its column's cells mean; each data
//! row is one entity, named by its `ID` (a CURIE or IRI the command line's
//! context reads) and typed by its `TYPE`. A template string the language does
//! not have refuses the table.
//!
//! The template strings:
//!   `ID`, `LABEL`                the subject's IRI and its rdfs:label
//!   `TYPE [SPLIT=c]`             the subject's kind: `class`/`owl:Class` (the
//!                                default), `object property`, `data property`,
//!                                `annotation property`, `datatype` or
//!                                `individual`; any other value makes the
//!                                subject a named individual of that class
//!                                (expression), one per split value
//!   `CLASS_TYPE`                 what a `C` column states: `subclass` (the
//!                                default), `equivalent` or `disjoint`
//!   `PROPERTY_TYPE`              what a `P` column states: `subproperty` (the
//!                                default), `equivalent`, `disjoint` or `inverse`
//!   `INDIVIDUAL_TYPE`            what an `I` column states: `named` (the
//!                                default), `same` or `different`
//!   `C %`, `SC %`, `EC %`, `DC %`  class rows: a superclass, an equivalent class
//!                                (several `EC` columns intersect) or a disjoint
//!                                class, each a Manchester expression the cell
//!                                fills in at `%`; `CI` names a class
//!   `P %`, `SP %`, `EP %`, `DP %`, `IP %`  property rows: a super-, equivalent,
//!                                disjoint or inverse property; `PI` names one
//!   `DOMAIN`, `RANGE`            a property's domain and range: class
//!                                expressions for an object property, a class
//!                                expression and a datatype for a data property,
//!                                IRIs for an annotation property
//!   `CHARACTERISTIC [SPLIT=c]`   an object property's characteristics; a data
//!                                property's may only be `functional`
//!   `I %`, `TI %`, `SI`, `DI`    individual rows: a type, same and different
//!                                individuals; `I <prop>` asserts a property —
//!                                to individuals, or to literals (`v^^dt` typed)
//!                                when `<prop>` is a data property
//!   `A <p>`, `AT <p>^^<dt>`, `AL <p>@<lang>`, `AI <p>`, `AP`
//!                                annotations: a string, a typed literal, a
//!                                language-tagged literal, an IRI, or `property
//!                                value` pairs
//!   `>…`, `>>…`                  an annotation on the axiom the column before
//!                                produced, and on that annotation
//!   trailing `SPLIT=<c>`         split the cell on `<c>` and apply once per value
//!
//! A cell names an entity by rdfs:label, by the short form of its IRI, by CURIE
//! or by IRI. The names are the input ontology's entities' and every table's
//! own rows', each under the kind its `TYPE` gives; a name no row and no input
//! entity has is made from a CURIE or IRI. A class-expression cell that holds
//! white space is one quoted name unless it starts with a quote or a
//! parenthesis, so an expression of several terms quotes its names.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::bail;
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationPropertyDomain,
    AnnotationPropertyRange, AnnotationSubject, AnnotationValue, AsymmetricObjectProperty, Build,
    ClassAssertion, ClassExpression as CE, Component, DataPropertyAssertion, DataPropertyDomain,
    DataPropertyRange, DataRange, DeclareAnnotationProperty, DeclareClass, DeclareDataProperty,
    DeclareDatatype, DeclareNamedIndividual, DeclareObjectProperty, DifferentIndividuals,
    DisjointClasses, DisjointDataProperties, DisjointObjectProperties, EquivalentClasses,
    EquivalentDataProperties, EquivalentObjectProperties, FunctionalDataProperty,
    FunctionalObjectProperty, Individual, InverseFunctionalObjectProperty, InverseObjectProperties,
    IrreflexiveObjectProperty, Literal, MutableOntology, ObjectPropertyAssertion,
    ObjectPropertyDomain, ObjectPropertyExpression as OPE, ObjectPropertyRange, RcStr,
    ReflexiveObjectProperty, SameIndividual, SubAnnotationPropertyOf, SubClassOf,
    SubDataPropertyOf, SubObjectPropertyOf, SymmetricObjectProperty, TransitiveObjectProperty,
};
use horned_owl::ontology::set::SetOntology;
use regex::Regex;

use crate::io;
use crate::io::manchester_parse::{self, Lookup};
use crate::model::{default_prefixes, literal_as_made, Model};
use crate::owlapi_hash::iri_short_form;
use crate::sig::{kind, typed_signature};

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

#[derive(ClapArgs)]
pub struct Args {
    /// Template table(s) (.tsv or.csv). Repeatable; all tables are applied
    /// into the same output ontology.
    #[arg(short = 't', long = "template", value_name = "FILE")]
    pub template: Vec<PathBuf>,
    /// Optional input ontology to merge the generated axioms into.
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output format. Long-only: `-f` is taken by `--force` on `template`.
    #[arg(long)]
    pub format: Option<String>,

    /// Log template errors and continue instead of aborting (`true` or `yes`
    /// in any case).
    #[arg(short = 'f', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub force: Option<bool>,

    /// Write template errors to this file (TSV or CSV).
    #[arg(short = 'e', long, value_name = "FILE")]
    pub errors: Option<PathBuf>,

    /// Additional template file(s), treated exactly like `--template`.
    #[arg(short = 'E', long = "external-template", value_name = "FILE")]
    pub external_template: Vec<PathBuf>,

    /// Set the output ontology IRI.
    #[arg(short = 'O', long, value_name = "IRI")]
    pub ontology_iri: Option<String>,
    /// Set the output version IRI.
    #[arg(short = 'V', long, value_name = "IRI")]
    pub version_iri: Option<String>,

    /// Add the generated axioms to the input ontology, and write and go on
    /// with the input so merged.
    #[arg(short = 'm', long)]
    pub merge_before: bool,
    /// Write the generated axioms alone, then add them to the input ontology
    /// and go on with the input so merged.
    #[arg(short = 'M', long)]
    pub merge_after: bool,

    /// Add the ancestors the input gives the generated axioms' terms, each
    /// with its labels: every term the axioms name that the input names too,
    /// climbed as `extract --method MIREOT` climbs a lower term.
    #[arg(short = 'a', long)]
    pub ancestors: bool,
    /// If true, a merge adds the ontology annotations of the generated
    /// axioms' ontology, which has none. `<bool>`.
    #[arg(short = 'A', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub include_annotations: Option<bool>,
    /// If true, a merge takes the input ontology's imports out of it: the
    /// merged ontology imports nothing, and the imports' axioms stay out.
    /// `<bool>`.
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub collapse_import_closure: Option<bool>,

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
    // --template and --external-template are processed identically.
    let templates: Vec<PathBuf> = args
        .template
        .iter()
        .chain(args.external_template.iter())
        .cloned()
        .collect();
    if templates.is_empty() {
        bail!("template requires at least one --template file");
    }
    let force = args.force.unwrap_or(false);
    if args.merge_before && args.merge_after {
        bail!("MERGE ERROR merge-before and merge-after cannot be combined");
    }

    // `template` emits ONLY the generated axioms: without
    // `--merge-before`/`--merge-after` the result is the fresh ontology built from
    // the rows. The input is there to resolve labels, not to be carried — a
    // component such as EFO's `components/gwas_import.owl` is the template's own
    // axioms, not the whole of `efo-edit.owl` plus the template.
    let merge = args.merge_before || args.merge_after;
    let needs_input = merge || args.ancestors;

    // An input is read with its imports closure. One that cannot be read —
    // missing, unparseable, or importing an ontology that resolves nowhere — is
    // no input at all, unless the result is merged with it or draws its
    // ancestors from it.
    let read_input = |p: &std::path::Path| -> anyhow::Result<Model> {
        let mut m = io::load(p)?;
        crate::cmd::read_imports_closure(&mut m, Some(p), &args.common)?;
        Ok(m)
    };
    let input = match piped {
        Some(m) => Some(m),
        None => match args.input.as_deref() {
            Some(p) => match read_input(p) {
                Ok(m) => Some(m),
                Err(e) if !needs_input => {
                    status!("template: {} not read ({e:#}); the template is built without it", p.display());
                    None
                }
                Err(e) => return Err(e),
            },
            None => None,
        },
    };
    let has_input = input.is_some();
    let mut model = input.unwrap_or_else(|| Model::from_parts(SetOntology::new(), default_prefixes()));
    args.common.apply(&mut model)?;

    let b = Build::new();
    // Errors collected across all template tables: (table, row, column, message).
    let mut errors: Vec<TemplateError> = Vec::new();

    // The names cells may use: the input's entities, then every table's own
    // rows, so a cell in any table can name an entity another table defines.
    let names = Names::new(&model);
    let mut tables: Vec<Table> = Vec::new();
    for path in &templates {
        let text = std::fs::read_to_string(path)?;
        let delim = if path.extension().map(|e| e == "csv").unwrap_or(false) {
            ','
        } else {
            '\t'
        };
        // Quoted fields are part of the grammar, not an escape a splitter can
        // ignore: a cell reading `"""Ghost teeth"""` holds `"Ghost teeth"`.
        let rows: Vec<Vec<String>> = crate::table::read(&text, delim);
        tables.push(Table::parse(&path.display().to_string(), rows, &names)?);
    }
    for table in &tables {
        for (_, cells) in &table.rows {
            names.add_row(table, cells);
        }
    }

    // The generated axioms go into their own ontology, which carries the input's
    // prefixes and reads its cells with the command line's context.
    let mut out = Model::from_parts(SetOntology::new(), crate::model::clone_prefixes(&model.prefixes));
    out.context = model.context.clone();
    for table in &tables {
        for (row, cells) in &table.rows {
            process_row(&b, &mut out, &names, table, *row, cells, &mut errors);
        }
    }
    declare_annotation_properties(&b, &mut out);

    if !errors.is_empty() {
        if let Some(path) = &args.errors {
            write_errors(path, &errors)?;
            status!("template: wrote {} error(s) to {}", errors.len(), path.display());
        }
        for e in errors.iter().take(20) {
            status!("template error [{}:{}] {}: {}", e.table, e.row, e.column, e.message);
        }
        if !force {
            bail!(
                "template: {} error(s); rerun with --force true to continue",
                errors.len()
            );
        }
    }

    // `template` builds a NEW ontology from the rows, and a new ontology has a
    // fresh document format — so the prefix block it writes is the bare default
    // set, not the input's. Same rule as `filter` and `query --update`. OBA's
    // `components/synonyms.owl` and `obsoletes.owl` come out of `template`, and
    // carrying the input's prefixes through would change every downstream artefact.
    out.format_prefixes_cleared = true;
    // The prefixes the command line ADDED are declared by every document the
    // command writes: the generated axioms' own, and the input they join.
    out.added_prefixes = model.added_prefixes.clone();

    // `--ancestors`: the terms of the generated axioms the input names too,
    // each with the ancestors the input gives it and their labels. With no
    // input there are none.
    if args.ancestors && has_input {
        let closure = crate::mireot::closure_of(&model)?;
        let source = crate::mireot::Source::new(closure.as_ref().unwrap_or(&model), &model);
        let mut terms: HashSet<crate::mireot::Term> = HashSet::new();
        for ac in out.ont.iter() {
            for (kind, iri) in typed_signature(&ac.component) {
                terms.insert(crate::mireot::Term { kind, iri });
            }
            for a in ac.ann.iter() {
                terms.insert(crate::mireot::Term { kind: kind::ANNOTATION_PROPERTY, iri: a.ap.0.to_string() });
            }
        }
        let lower: Vec<crate::mireot::Term> = terms.into_iter().filter(|t| source.has(t)).collect();
        let spec = crate::mireot::Spec {
            lower: &lower,
            upper: &[],
            branch: &[],
            only_annotations: Some(crate::mireot::RDFS_LABEL),
            intermediates: crate::extract::Intermediates::All,
        };
        for ac in crate::mireot::module(&source, &spec)?.iter() {
            out.ont.insert(ac.clone());
        }
    }

    if !merge {
        // The generated axioms' ontology takes the IRIs `--ontology-iri` and
        // `--version-iri` give it, through annotate's core.
        if args.ontology_iri.is_some() || args.version_iri.is_some() {
            out = crate::cmd::annotate::annotate_with(
                out,
                &crate::cmd::annotate::AnnotateOptions {
                    ontology_iri: args.ontology_iri.clone(),
                    version_iri: args.version_iri.clone(),
                    ..Default::default()
                },
            )?;
        }
        crate::cmd::maybe_save(&mut out, args.output.as_deref(), args.format.as_deref())?;
        return Ok(Some(out));
    }

    // A merge adds the generated axioms to the input, which keeps its own IRIs,
    // annotations and prefixes: `--ontology-iri` and `--version-iri` name nothing
    // under it. `--merge-after` writes the generated axioms alone first, and
    // only then finds whether there is an input to add them to.
    if args.merge_after {
        crate::cmd::maybe_save(&mut out, args.output.as_deref(), args.format.as_deref())?;
    }
    if !has_input {
        let switch = if args.merge_before { "--merge-before" } else { "--merge-after" };
        bail!("template: {switch} has no input ontology to merge into");
    }
    for ac in out.ont.iter() {
        model.ont.insert(ac.clone());
    }
    // The input counts as changed whatever the merge added to it.
    model.mark_root_changed();
    // `--collapse-import-closure true` takes the input's imports out of it. The
    // imports' axioms stay out, and the ontology is written among no closure.
    if args.collapse_import_closure.unwrap_or(false) {
        let imports: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| matches!(ac.component, Component::Import(_)))
            .cloned()
            .collect();
        for ac in imports {
            model.ont.remove(&ac);
        }
    }
    if args.merge_before {
        crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    }
    Ok(Some(model))
}

/// Generate axioms from a single template table given as text (TSV, or CSV when
/// `csv` is true), inserting them into `model`. The in-memory core behind the
/// `template` command — no files. Template-cell errors are joined into the
/// returned `Err`.
///
/// Layout: row 0 is the column-name/label row, row 1 the template-string row,
/// rows 2.. the data.
pub fn template_from_str(mut model: Model, table: &str, csv: bool) -> anyhow::Result<Model> {
    let b = Build::new();
    let names = Names::new(&model);
    let delim = if csv { ',' } else { '\t' };
    // Quoted fields are part of the grammar, not an escape a splitter can ignore:
    // a cell reading `"""Ghost teeth"""` holds `"Ghost teeth"`.
    let rows: Vec<Vec<String>> = crate::table::read(table, delim);
    let table = Table::parse("template", rows, &names)?;
    for (_, cells) in &table.rows {
        names.add_row(&table, cells);
    }
    let mut errors: Vec<TemplateError> = Vec::new();
    for (row, cells) in &table.rows {
        process_row(&b, &mut model, &names, &table, *row, cells, &mut errors);
    }
    declare_annotation_properties(&b, &mut model);
    if !errors.is_empty() {
        let joined = errors
            .iter()
            .take(20)
            .map(|e| format!("[{}:{}] {}: {}", e.table, e.row, e.column, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        bail!("template: {} error(s): {joined}", errors.len());
    }
    Ok(model)
}

/// A single template processing error — one row of the `--errors` table.
struct TemplateError {
    table: String,
    row: usize,
    column: String,
    message: String,
}

fn write_errors(path: &std::path::Path, errors: &[TemplateError]) -> anyhow::Result<()> {
    let csv = path.extension().map(|e| e == "csv").unwrap_or(false);
    let sep = if csv { ',' } else { '\t' };
    let mut out = String::new();
    out.push_str(&["table", "row", "column", "message"].join(&sep.to_string()));
    out.push('\n');
    for e in errors {
        let cells = [e.table.clone(), e.row.to_string(), e.column.clone(), e.message.clone()];
        let line = cells
            .iter()
            .map(|c| {
                if csv && (c.contains(',') || c.contains('"')) {
                    format!("\"{}\"", c.replace('"', "\"\""))
                } else {
                    c.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(&sep.to_string());
        out.push_str(&line);
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

// === Names =================================================================

const CLASS: usize = 0;
const OBJECT_PROPERTY: usize = 1;
const DATA_PROPERTY: usize = 2;
const ANNOTATION_PROPERTY: usize = 3;
const DATATYPE: usize = 4;
const INDIVIDUAL: usize = 5;

/// The names a template's cells may use, per entity kind: for each entity of
/// the input, the short form of its IRI and each of its `rdfs:label`s; for each
/// template row, its `ID` and its `LABEL`, under the kind its `TYPE` gives. A
/// name no entity has is made from a CURIE or IRI the command line's context
/// reads, and a property so made is remembered under its kind, so that one
/// name is not an object property in one cell and a data property in another.
struct Names {
    context: crate::context::Context,
    by_kind: [RefCell<HashMap<String, String>>; 6],
    /// Every name of every kind, to the IRI it names.
    iris: RefCell<HashMap<String, String>>,
}

impl Names {
    fn new(model: &Model) -> Names {
        let names = Names {
            context: model.context.clone(),
            by_kind: Default::default(),
            iris: Default::default(),
        };
        let mut labels: HashMap<String, Vec<String>> = HashMap::new();
        for ac in model.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                    if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                        (&aa.subject, &aa.ann.av)
                    {
                        labels.entry(s.as_ref().to_string()).or_default().push(literal_text(lit));
                    }
                }
            }
        }
        let mut seen: HashSet<(u8, String)> = HashSet::new();
        for ac in model.ont.iter() {
            let mut entities = typed_signature(&ac.component);
            for a in ac.ann.iter() {
                annotation_properties_in(a, &mut entities);
            }
            for (k, iri) in entities {
                if !seen.insert((k, iri.clone())) {
                    continue;
                }
                let k = match k {
                    kind::CLASS => CLASS,
                    kind::OBJECT_PROPERTY => OBJECT_PROPERTY,
                    kind::DATA_PROPERTY => DATA_PROPERTY,
                    kind::ANNOTATION_PROPERTY => ANNOTATION_PROPERTY,
                    kind::DATATYPE => DATATYPE,
                    kind::NAMED_INDIVIDUAL => INDIVIDUAL,
                    _ => continue,
                };
                names.add(k, &iri_short_form(&iri), &iri);
                for label in labels.get(&iri).into_iter().flatten() {
                    names.add(k, label, &iri);
                }
            }
        }
        names
    }

    /// Add a template row's subject under its `ID` and its `LABEL`, with the
    /// kind its `TYPE` gives (a class, when it has none).
    fn add_row(&self, table: &Table, cells: &[String]) {
        let Some(id) = table.id.and_then(|i| cells.get(i)) else { return };
        let label = table.label.and_then(|i| cells.get(i)).filter(|l| !l.is_empty());
        let type_text = table
            .kind
            .and_then(|i| cells.get(i))
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("class");
        let iri = self.create(id).unwrap_or_else(|| id.to_string());
        let k = kind_index(row_kind(self, type_text));
        self.add(k, id, &iri);
        if let Some(label) = label {
            self.add(k, label, &iri);
        }
    }

    fn add(&self, k: usize, name: &str, iri: &str) {
        self.iris.borrow_mut().insert(name.to_string(), iri.to_string());
        self.by_kind[k].borrow_mut().insert(name.to_string(), iri.to_string());
    }

    /// The IRI `name` has under kind `k`: as written, else without the single
    /// quotes around it, else without the double quotes around it.
    fn find(&self, k: usize, name: &str) -> Option<String> {
        let map = self.by_kind[k].borrow();
        if let Some(iri) = map.get(name) {
            return Some(iri.clone());
        }
        let n = unquote(name.trim(), '\'');
        if let Some(iri) = map.get(n) {
            return Some(iri.clone());
        }
        let n = unquote(n.trim(), '"');
        map.get(n).cloned()
    }

    /// The IRI a CURIE or IRI names, read with the command line's context.
    fn create(&self, name: &str) -> Option<String> {
        self.context.iri(name)
    }

    fn class(&self, name: &str, create: bool) -> Option<String> {
        self.find(CLASS, name).or_else(|| if create { self.create(name) } else { None })
    }

    fn object_property(&self, name: &str) -> Option<String> {
        if let Some(iri) = self.find(OBJECT_PROPERTY, name) {
            return Some(iri);
        }
        let iri = self.create(name);
        let known_data = {
            let dps = self.by_kind[DATA_PROPERTY].borrow();
            dps.contains_key(name) || iri.as_ref().is_some_and(|i| dps.values().any(|v| v == i))
        };
        if known_data {
            return None;
        }
        let iri = iri?;
        self.by_kind[OBJECT_PROPERTY].borrow_mut().insert(name.to_string(), iri.clone());
        Some(iri)
    }

    fn data_property(&self, name: &str) -> Option<String> {
        if let Some(iri) = self.find(DATA_PROPERTY, name) {
            return Some(iri);
        }
        let iri = self.create(name);
        let known_object = {
            let ops = self.by_kind[OBJECT_PROPERTY].borrow();
            ops.contains_key(name) || iri.as_ref().is_some_and(|i| ops.values().any(|v| v == i))
        };
        if known_object {
            return None;
        }
        let iri = iri?;
        self.by_kind[DATA_PROPERTY].borrow_mut().insert(name.to_string(), iri.clone());
        Some(iri)
    }

    fn annotation_property(&self, name: &str, create: bool) -> Option<String> {
        self.find(ANNOTATION_PROPERTY, name).or_else(|| if create { self.create(name) } else { None })
    }

    fn datatype(&self, name: &str, create: bool) -> Option<String> {
        self.find(DATATYPE, name).or_else(|| if create { self.create(name) } else { None })
    }

    fn individual(&self, name: &str) -> Option<String> {
        self.find(INDIVIDUAL, name).or_else(|| self.create(name))
    }

    /// The IRI `name` names, of any kind, as written; else one made of it.
    fn iri(&self, name: &str, create: bool) -> Option<String> {
        self.iris.borrow().get(name).cloned().or_else(|| if create { self.create(name) } else { None })
    }

    /// The kind of the entity `name` names as written, where one does.
    fn kind_of(&self, name: &str) -> Option<usize> {
        [ANNOTATION_PROPERTY, OBJECT_PROPERTY, DATA_PROPERTY, DATATYPE, INDIVIDUAL, CLASS]
            .into_iter()
            .find(|&k| self.by_kind[k].borrow().contains_key(name))
    }
}

impl Lookup for Names {
    fn class(&self, name: &str, create: bool) -> Option<String> {
        Names::class(self, name, create)
    }
    fn object_property(&self, name: &str) -> Option<String> {
        Names::object_property(self, name)
    }
    fn data_property(&self, name: &str) -> Option<String> {
        Names::data_property(self, name)
    }
    fn datatype(&self, name: &str) -> Option<String> {
        Names::datatype(self, name, true)
    }
    fn individual(&self, name: &str) -> Option<String> {
        Names::individual(self, name)
    }
}

/// `s` without a leading and a trailing `q`.
fn unquote(s: &str, q: char) -> &str {
    let s = s.strip_prefix(q).unwrap_or(s);
    s.strip_suffix(q).unwrap_or(s)
}

/// The annotation properties `a` and its nested annotations use.
fn annotation_properties_in(a: &Annotation<RcStr>, out: &mut Vec<(u8, String)>) {
    out.push((kind::ANNOTATION_PROPERTY, a.ap.0.as_ref().to_string()));
    for nested in a.ann.iter() {
        annotation_properties_in(nested, out);
    }
}

fn literal_text(lit: &Literal<RcStr>) -> String {
    match lit {
        Literal::Simple { literal }
        | Literal::Language { literal, .. }
        | Literal::Datatype { literal, .. } => literal.clone(),
    }
}

/// A cell as one name: unchanged when it starts with a quote or a parenthesis;
/// quoted, with its own single quotes escaped, when it holds white space; else
/// with its single quotes escaped.
fn wrap(content: &str) -> String {
    let t = content.trim();
    if t.starts_with('\'') || t.starts_with('"') || t.starts_with('(') {
        return content.to_string();
    }
    let escaped = content.replace('\'', "\\'");
    if t.contains(char::is_whitespace) {
        format!("'{escaped}'")
    } else {
        escaped
    }
}

// === Tables ================================================================

/// A template table: its template strings, the columns the language gives a
/// role, and its data rows.
struct Table {
    name: String,
    templates: Vec<String>,
    /// Each data row with its row number in the file; a row with no `ID` (or
    /// no `LABEL`, in a table with no `ID` column) is left out.
    rows: Vec<(usize, Vec<String>)>,
    id: Option<usize>,
    label: Option<usize>,
    kind: Option<usize>,
    kind_split: Option<String>,
    class_type: Option<usize>,
    property_type: Option<usize>,
    individual_type: Option<usize>,
    characteristic: Option<usize>,
    characteristic_split: Option<String>,
}

impl Table {
    fn parse(name: &str, rows: Vec<Vec<String>>, names: &Names) -> anyhow::Result<Table> {
        if rows.len() < 2 {
            bail!("template {name} needs at least a header row and a template-string row");
        }
        let headers = &rows[0];
        let templates: Vec<String> = rows[1].iter().map(|t| t.trim().to_string()).collect();
        let mut table = Table {
            name: name.to_string(),
            templates: templates.clone(),
            rows: Vec::new(),
            id: None,
            label: None,
            kind: None,
            kind_split: None,
            class_type: None,
            property_type: None,
            individual_type: None,
            characteristic: None,
            characteristic_split: None,
        };
        let split_of = |t: &str| t.find("SPLIT=").map(|i| t[i + 6..].trim().to_string());
        let labels = |property: &str| {
            names.annotation_property(property.trim(), true).as_deref() == Some(RDFS_LABEL)
        };
        for (column, template) in templates.iter().enumerate() {
            if template.is_empty() {
                continue;
            }
            let header = headers.get(column).map(|h| h.trim()).unwrap_or("");
            if header.is_empty() {
                bail!("COLUMN MISMATCH ERROR the template string in column {column} must have a corresponding header in table \"{name}\"");
            }
            if !valid_template(template) {
                bail!("UNKNOWN TEMPLATE ERROR could not interpret template string \"{template}\" for column {column} (\"{header}\") in table \"{name}\".");
            }
            if let Some(rest) = template.strip_prefix("A ") {
                if labels(rest) {
                    table.label = Some(column);
                }
            } else if let Some(rest) = template.strip_prefix("AT ") {
                if labels(rest.split("^^").next().unwrap_or(rest)) {
                    table.label = Some(column);
                }
            } else if let Some(rest) = template.strip_prefix("AL ") {
                if labels(rest.split('@').next().unwrap_or(rest)) {
                    table.label = Some(column);
                }
            } else if let Some(rest) = template.strip_prefix("AI ") {
                if labels(rest) {
                    table.label = Some(column);
                }
            } else if template == "ID" {
                table.id = Some(column);
            } else if template == "LABEL" {
                table.label = Some(column);
            } else if template.starts_with("TYPE") {
                table.kind = Some(column);
                table.kind_split = split_of(template);
            } else if template.starts_with("CLASS_TYPE") {
                if template.contains("SPLIT=") {
                    bail!("CLASS TYPE SPLIT ERROR the SPLIT functionality should not be used for CLASS_TYPE in column {column} in table \"{name}\".");
                }
                table.class_type = Some(column);
            } else if template.starts_with("PROPERTY_TYPE") {
                if template.contains("SPLIT=") {
                    bail!("PROPERTY TYPE SPLIT ERROR the SPLIT functionality should not be used for PROPERTY_TYPE in column {column} in table \"{name}\".");
                }
                table.property_type = Some(column);
            } else if template.starts_with("INDIVIDUAL_TYPE") {
                if template.contains("SPLIT=") {
                    bail!("INDIVIDUAL TYPE SPLIT ERROR the SPLIT functionality should not be used for INDIVIDUAL_TYPE in column {column} in table \"{name}\".");
                }
                table.individual_type = Some(column);
            } else if template.starts_with("CHARACTERISTIC") {
                table.characteristic = Some(column);
                table.characteristic_split = split_of(template);
            }
        }
        if table.id.is_none() && table.label.is_none() {
            bail!("Template row must include an \"ID\" or \"LABEL\" column in table: {name}");
        }
        for (ri, cells) in rows.iter().enumerate().skip(2) {
            let named = match (table.id, table.label) {
                (Some(i), _) => cells.get(i).is_some_and(|c| !c.trim().is_empty()),
                (None, Some(l)) => cells.get(l).is_some_and(|c| !c.is_empty()),
                (None, None) => false,
            };
            if named {
                table.rows.push((ri + 1, cells.clone()));
            }
        }
        Ok(table)
    }
}

/// Whether `t` is a template string of the language.
fn valid_template(t: &str) -> bool {
    if matches!(t, "ID" | "LABEL" | "CLASS_TYPE" | "PROPERTY_TYPE" | "DOMAIN" | "RANGE" | "INDIVIDUAL_TYPE" | "AP") {
        return true;
    }
    let split_only = |rest: &str| rest.is_empty() || rest.strip_prefix(" SPLIT=").is_some_and(|s| !s.is_empty());
    if t.strip_prefix("TYPE").is_some_and(split_only)
        || t.strip_prefix("CHARACTERISTIC").is_some_and(split_only)
        || t.strip_prefix("AP SPLIT=").is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    // Annotations: up to two `>`, then A, AL, AT or AI and a space.
    let s = t.strip_prefix(">>").or_else(|| t.strip_prefix('>')).unwrap_or(t);
    if let Some(r) = s.strip_prefix('A') {
        let r = r.strip_prefix(['L', 'T', 'I']).unwrap_or(r);
        if r.starts_with(' ') {
            return true;
        }
    }
    let one = |s: &str, first: &[char], then: &str| -> bool {
        let mut cs = s.chars();
        cs.next().is_some_and(|c| first.contains(&c)) && cs.as_str().starts_with(then)
    };
    let at_most_one_char = |r: &str| r.chars().count() <= 1;
    // Classes: one `>` allowed; C, CI, SC, EC or DC.
    let s = t.strip_prefix('>').unwrap_or(t);
    if s.starts_with("C ")
        || s.strip_prefix("CI").is_some_and(at_most_one_char)
        || one(s, &['S', 'E', 'D'], "C ")
    {
        return true;
    }
    // Properties: P, PI, SP, EP, DP or IP; individuals: I, II, TI, SI or DI.
    t.starts_with("P ")
        || t.strip_prefix("PI").is_some_and(at_most_one_char)
        || one(t, &['S', 'E', 'D', 'I'], "P ")
        || t.starts_with("I ")
        || t.strip_prefix("II").is_some_and(at_most_one_char)
        || one(t, &['T', 'S', 'D'], "I ")
}

/// A template string without its `SPLIT=` clause, and the split text.
fn split_template(t: &str) -> (String, Option<String>) {
    match t.find("SPLIT=") {
        Some(i) => (t[..i].trim().to_string(), Some(t[i + 6..].trim().to_string())),
        None => (t.trim().to_string(), None),
    }
}

/// `value` split on `delimiter`, written `\n` for a line break; the pieces
/// after the last non-empty one are dropped.
fn split_values(value: &str, delimiter: &str) -> Vec<String> {
    if delimiter.trim() == "\\n" {
        java_split(value, "\n")
    } else {
        java_split(value, delimiter)
    }
}

/// `value` split on `delimiter`: the whole value where the delimiter does not
/// occur, else the pieces up to the last non-empty one.
fn java_split(value: &str, delimiter: &str) -> Vec<String> {
    if delimiter.is_empty() || !value.contains(delimiter) {
        return vec![value.to_string()];
    }
    let mut parts: Vec<String> = value.split(delimiter).map(str::to_string).collect();
    while parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    parts
}

/// The values of a cell, each trimmed: the cell split where `split` is given,
/// else the cell alone.
fn all_values(value: &str, split: Option<&str>) -> Vec<String> {
    match split {
        Some(s) => split_values(value, s).iter().map(|v| v.trim().to_string()).collect(),
        None => vec![value.trim().to_string()],
    }
}

// === Rows ==================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowKind {
    Class,
    ObjectProperty,
    DataProperty,
    AnnotationProperty,
    Datatype,
    Individual,
}

/// The kind a `TYPE` cell gives a row: the cell as an IRI where the context
/// reads one, else as written, matched ignoring case. Anything else types an
/// individual, and the cell is its class.
fn row_kind(names: &Names, type_text: &str) -> RowKind {
    let t = names.create(type_text).unwrap_or_else(|| type_text.to_string()).to_lowercase();
    match t.as_str() {
        "http://www.w3.org/2002/07/owl#class" | "class" => RowKind::Class,
        "http://www.w3.org/2002/07/owl#objectproperty" | "object property" => RowKind::ObjectProperty,
        "http://www.w3.org/2002/07/owl#dataproperty" | "data property" => RowKind::DataProperty,
        "http://www.w3.org/2002/07/owl#annotationproperty" | "annotation property" => {
            RowKind::AnnotationProperty
        }
        "http://www.w3.org/2002/07/owl#datatype" | "datatype" => RowKind::Datatype,
        _ => RowKind::Individual,
    }
}

fn kind_index(k: RowKind) -> usize {
    match k {
        RowKind::Class => CLASS,
        RowKind::ObjectProperty => OBJECT_PROPERTY,
        RowKind::DataProperty => DATA_PROPERTY,
        RowKind::AnnotationProperty => ANNOTATION_PROPERTY,
        RowKind::Datatype => DATATYPE,
        RowKind::Individual => INDIVIDUAL,
    }
}

/// The IRI an entity kind has as a type, which is what a row with no `TYPE`
/// is typed by when an entity of that kind already has its label or ID.
fn kind_type_iri(k: usize) -> &'static str {
    match k {
        CLASS => "http://www.w3.org/2002/07/owl#Class",
        OBJECT_PROPERTY => "http://www.w3.org/2002/07/owl#ObjectProperty",
        DATA_PROPERTY => "http://www.w3.org/2002/07/owl#DatatypeProperty",
        ANNOTATION_PROPERTY => "http://www.w3.org/2002/07/owl#AnnotationProperty",
        DATATYPE => "http://www.w3.org/2000/01/rdf-schema#Datatype",
        _ => "http://www.w3.org/2002/07/owl#NamedIndividual",
    }
}

/// A row's failure: the template string of the column it failed in, and why.
type Fail = (String, String);

fn process_row(
    b: &Build<RcStr>,
    model: &mut Model,
    names: &Names,
    table: &Table,
    row: usize,
    cells: &[String],
    errors: &mut Vec<TemplateError>,
) {
    let cell = |column: Option<usize>| -> Option<&str> {
        column.and_then(|i| cells.get(i)).map(String::as_str).filter(|c| !c.trim().is_empty())
    };
    let id = cell(table.id);
    let label = cell(table.label);
    let (Some(named), column) = (id.or(label), if id.is_some() { "ID" } else { "LABEL" }) else {
        return;
    };
    // A row with no `TYPE` has the kind of the entity its label, else its ID,
    // already names; a class when none does.
    let type_text = match cell(table.kind) {
        Some(t) => t.to_string(),
        None => match names.kind_of(label.unwrap_or(named)) {
            Some(k) => kind_type_iri(k).to_string(),
            None => "class".to_string(),
        },
    };
    let iri = match id {
        Some(id) => names.create(id),
        None => names.iri(named, true),
    };
    let Some(iri) = iri else {
        errors.push(TemplateError {
            table: table.name.clone(),
            row,
            column: column.to_string(),
            message: format!("UNKNOWN ENTITY ERROR could not interpret '{named}'"),
        });
        return;
    };
    let r = Row { b, names, table, cells, iri };
    let result = match row_kind(names, &type_text) {
        RowKind::Class => r.class_row(model),
        RowKind::ObjectProperty => r.object_property_row(model),
        RowKind::DataProperty => r.data_property_row(model),
        RowKind::AnnotationProperty => r.annotation_property_row(model),
        RowKind::Datatype => r.datatype_row(model),
        RowKind::Individual => r.individual_row(model),
    };
    if let Err((column, message)) = result {
        errors.push(TemplateError { table: table.name.clone(), row, column, message });
    }
}

/// One data row being built: its subject and what it reads its cells with.
struct Row<'a> {
    b: &'a Build<RcStr>,
    names: &'a Names,
    table: &'a Table,
    cells: &'a [String],
    iri: String,
}

impl Row<'_> {
    /// The columns with a template string, with each one's non-blank cell.
    fn columns(&self) -> Vec<(usize, &str, &str)> {
        self.table
            .templates
            .iter()
            .enumerate()
            .filter_map(|(column, template)| {
                let value = self.cells.get(column)?;
                (!value.trim().is_empty()).then_some((column, template.as_str(), value.as_str()))
            })
            .collect()
    }

    fn short_form(&self) -> String {
        iri_short_form(&self.iri)
    }

    fn insert(&self, model: &mut Model, component: Component<RcStr>, ann: BTreeSet<Annotation<RcStr>>) {
        model.ont.insert(AnnotatedComponent { component, ann });
    }

    /// The annotations of the `>` columns after `column`, each carrying those
    /// of the `>>` columns after it.
    fn axiom_annotations(&self, column: usize) -> Result<BTreeSet<Annotation<RcStr>>, Fail> {
        let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
        let mut c = column;
        while c + 1 < self.cells.len() {
            c += 1;
            let Some(template) = self.table.templates.get(c) else { break };
            if template.is_empty() {
                continue;
            }
            if let Some(rest) = template.strip_prefix('>') {
                if rest.starts_with('>') {
                    match groups.last_mut() {
                        Some(last) => last.1.push(c),
                        None => {
                            return Err((
                                template.clone(),
                                "an annotation on an annotation needs an annotation column before it".to_string(),
                            ))
                        }
                    }
                } else {
                    groups.push((c, Vec::new()));
                }
            } else {
                break;
            }
        }
        let mut out = BTreeSet::new();
        for (first, more) in groups {
            let template = &self.table.templates[first];
            let value = self.cells[first].trim();
            if value.is_empty() {
                continue;
            }
            let anns = self.annotations(template, value).map_err(|m| (template.clone(), m))?;
            let mut nested = BTreeSet::new();
            for m in more {
                let template = self.table.templates[m].trim_start_matches('>');
                let value = self.cells[m].trim();
                if value.is_empty() {
                    continue;
                }
                nested.extend(self.annotations(template, value).map_err(|e| (template.to_string(), e))?);
            }
            for mut a in anns {
                a.ann = nested.clone();
                out.insert(a);
            }
        }
        Ok(out)
    }

    /// The annotations an annotation template string and its cell make.
    fn annotations(&self, template: &str, value: &str) -> Result<Vec<Annotation<RcStr>>, String> {
        annotations_for(self.b, self.names, template, value)
    }

    /// An `A…` or `LABEL` column: an annotation assertion on the subject per
    /// annotation, annotated by the `>` columns after it.
    fn annotation_assertions(&self, model: &mut Model, column: usize, template: &str, value: &str) -> Result<(), Fail> {
        let anns = self.annotations(template, value).map_err(|m| (template.to_string(), m))?;
        let axiom_anns = self.axiom_annotations(column)?;
        for ann in anns {
            self.insert(
                model,
                Component::AnnotationAssertion(AnnotationAssertion {
                    subject: AnnotationSubject::IRI(self.b.iri(self.iri.as_str())),
                    ann,
                }),
                axiom_anns.clone(),
            );
        }
        Ok(())
    }

    fn property_type(&self) -> String {
        self.table
            .property_type
            .and_then(|i| self.cells.get(i))
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "subproperty".to_string())
    }

    /// The `CHARACTERISTIC` cell's values.
    fn characteristics(&self) -> Vec<String> {
        let Some(text) = self.table.characteristic.and_then(|i| self.cells.get(i)) else {
            return Vec::new();
        };
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        match &self.table.characteristic_split {
            Some(split) if !split.is_empty() && text.contains(split.as_str()) => java_split(text, split),
            _ => vec![text.to_string()],
        }
    }

    fn class_row(&self, model: &mut Model) -> Result<(), Fail> {
        let b = self.b;
        let cls = CE::Class(b.class(self.iri.as_str()));
        self.insert(model, Component::DeclareClass(DeclareClass(b.class(self.iri.as_str()))), BTreeSet::new());
        let class_type = self
            .table
            .class_type
            .and_then(|i| self.cells.get(i))
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "subclass".to_string());
        if !matches!(class_type.as_str(), "subclass" | "equivalent" | "disjoint") {
            return Err((
                "CLASS_TYPE".to_string(),
                format!("CLASS TYPE ERROR class {} has unknown type '{class_type}'", self.short_form()),
            ));
        }
        let mut sub: Vec<(usize, Vec<CE<RcStr>>)> = Vec::new();
        let mut equivalent: Vec<(usize, Vec<CE<RcStr>>)> = Vec::new();
        let mut disjoint: Vec<(usize, Vec<CE<RcStr>>)> = Vec::new();
        for (column, template, value) in self.columns() {
            let expressions = || class_expressions(b, self.names, template, value).map_err(|m| (template.to_string(), m));
            if template.starts_with('A') || template.starts_with("LABEL") {
                self.annotation_assertions(model, column, template, value)?;
            } else if template.starts_with("SC") {
                sub.push((column, expressions()?));
            } else if template.starts_with("EC") {
                equivalent.push((column, expressions()?));
            } else if template.starts_with("DC") {
                disjoint.push((column, expressions()?));
            } else if template.starts_with('C') && !template.starts_with("CLASS_TYPE") {
                match class_type.as_str() {
                    "subclass" => sub.push((column, expressions()?)),
                    "equivalent" => equivalent.push((column, expressions()?)),
                    _ => disjoint.push((column, expressions()?)),
                }
            }
        }
        for (column, exprs) in sub {
            let anns = self.axiom_annotations(column)?;
            for sup in exprs {
                self.insert(model, Component::SubClassOf(SubClassOf { sub: cls.clone(), sup }), anns.clone());
            }
        }
        // Every equivalent expression of the row makes one axiom: the class
        // equivalent to the expression, or to the intersection of them all.
        let mut anns = BTreeSet::new();
        let mut exprs: Vec<CE<RcStr>> = Vec::new();
        for (column, column_exprs) in equivalent {
            anns.extend(self.axiom_annotations(column)?);
            for ce in column_exprs {
                if !exprs.contains(&ce) {
                    exprs.push(ce);
                }
            }
        }
        if let Some(only) = (exprs.len() == 1).then(|| exprs[0].clone()) {
            self.insert(model, Component::EquivalentClasses(EquivalentClasses(vec![cls.clone(), only])), anns);
        } else if exprs.len() > 1 {
            self.insert(
                model,
                Component::EquivalentClasses(EquivalentClasses(vec![cls.clone(), CE::ObjectIntersectionOf(exprs)])),
                anns,
            );
        }
        for (column, exprs) in disjoint {
            let anns = self.axiom_annotations(column)?;
            for ce in exprs {
                self.insert(model, Component::DisjointClasses(DisjointClasses(vec![cls.clone(), ce])), anns.clone());
            }
        }
        Ok(())
    }

    fn object_property_row(&self, model: &mut Model) -> Result<(), Fail> {
        let b = self.b;
        let property = OPE::ObjectProperty(b.object_property(self.iri.as_str()));
        self.insert(
            model,
            Component::DeclareObjectProperty(DeclareObjectProperty(b.object_property(self.iri.as_str()))),
            BTreeSet::new(),
        );
        let property_type = self.property_type();
        for c in self.characteristics() {
            let component = match c.trim().to_lowercase().as_str() {
                "asymmetric" => Component::AsymmetricObjectProperty(AsymmetricObjectProperty(property.clone())),
                "functional" => Component::FunctionalObjectProperty(FunctionalObjectProperty(property.clone())),
                "inversefunctional" | "inverse functional" => {
                    Component::InverseFunctionalObjectProperty(InverseFunctionalObjectProperty(property.clone()))
                }
                "irreflexive" => Component::IrreflexiveObjectProperty(IrreflexiveObjectProperty(property.clone())),
                "reflexive" => Component::ReflexiveObjectProperty(ReflexiveObjectProperty(property.clone())),
                "symmetric" => Component::SymmetricObjectProperty(SymmetricObjectProperty(property.clone())),
                "transitive" => Component::TransitiveObjectProperty(TransitiveObjectProperty(property.clone())),
                _ => {
                    return Err((
                        "CHARACTERISTIC".to_string(),
                        format!(
                            "UNKNOWN CHARACTERISTIC ERROR property '{}' has unknown characteristic '{c}'",
                            self.short_form()
                        ),
                    ))
                }
            };
            self.insert(model, component, BTreeSet::new());
        }
        for (column, template, value) in self.columns() {
            let fail = |m: String| (template.to_string(), m);
            let expressions = || object_property_expressions(b, self.names, template, value).map_err(fail);
            let class_exprs = || class_expressions(b, self.names, template, value).map_err(fail);
            if template.starts_with('A') || template.starts_with("LABEL") {
                self.annotation_assertions(model, column, template, value)?;
                continue;
            }
            let relation = if template.starts_with("SP") {
                "subproperty"
            } else if template.starts_with("EP") {
                "equivalent"
            } else if template.starts_with("DP") {
                "disjoint"
            } else if template.starts_with("IP") {
                "inverse"
            } else if template.starts_with('P') && !template.starts_with("PROPERTY_TYPE") {
                property_type.as_str()
            } else if template.starts_with("DOMAIN") {
                let anns = self.axiom_annotations(column)?;
                for ce in class_exprs()? {
                    self.insert(
                        model,
                        Component::ObjectPropertyDomain(ObjectPropertyDomain { ope: property.clone(), ce }),
                        anns.clone(),
                    );
                }
                continue;
            } else if template.starts_with("RANGE") {
                let anns = self.axiom_annotations(column)?;
                for ce in class_exprs()? {
                    self.insert(
                        model,
                        Component::ObjectPropertyRange(ObjectPropertyRange { ope: property.clone(), ce }),
                        anns.clone(),
                    );
                }
                continue;
            } else {
                continue;
            };
            let exprs = expressions()?;
            let anns = self.axiom_annotations(column)?;
            match relation {
                "subproperty" => {
                    for sup in exprs {
                        self.insert(
                            model,
                            Component::SubObjectPropertyOf(SubObjectPropertyOf { sub: property.clone().into(), sup }),
                            anns.clone(),
                        );
                    }
                }
                "equivalent" => {
                    for other in exprs {
                        self.insert(
                            model,
                            Component::EquivalentObjectProperties(EquivalentObjectProperties(vec![
                                property.clone(),
                                other,
                            ])),
                            anns.clone(),
                        );
                    }
                }
                "disjoint" => {
                    let mut all = exprs;
                    if !all.contains(&property) {
                        all.push(property.clone());
                    }
                    self.insert(model, Component::DisjointObjectProperties(DisjointObjectProperties(all)), anns);
                }
                "inverse" => {
                    for other in exprs {
                        self.insert(
                            model,
                            Component::InverseObjectProperties(InverseObjectProperties(property.clone(), other)),
                            anns.clone(),
                        );
                    }
                }
                _ => {
                    return Err(fail(format!(
                        "PROPERTY TYPE ERROR property {} has unknown type '{property_type}'",
                        self.short_form()
                    )))
                }
            }
        }
        Ok(())
    }

    fn data_property_row(&self, model: &mut Model) -> Result<(), Fail> {
        let b = self.b;
        let property = b.data_property(self.iri.as_str());
        self.insert(model, Component::DeclareDataProperty(DeclareDataProperty(property.clone())), BTreeSet::new());
        let property_type = self.property_type();
        for c in self.characteristics() {
            if !c.eq_ignore_ascii_case("functional") {
                return Err((
                    "CHARACTERISTIC".to_string(),
                    format!(
                        "DATA PROPERTY CHARACTERISTIC ERROR data property '{}' can only have characteristic 'functional'",
                        self.short_form()
                    ),
                ));
            }
            self.insert(model, Component::FunctionalDataProperty(FunctionalDataProperty(property.clone())), BTreeSet::new());
        }
        for (column, template, value) in self.columns() {
            let fail = |m: String| (template.to_string(), m);
            let (_, split) = split_template(template);
            let expressions = || data_property_expressions(b, self.names, template, value).map_err(fail);
            if template.starts_with('A') || template.starts_with("LABEL") {
                self.annotation_assertions(model, column, template, value)?;
                continue;
            }
            let relation = if template.starts_with("SP") {
                "subproperty"
            } else if template.starts_with("EP") {
                "equivalent"
            } else if template.starts_with("DP") {
                "disjoint"
            } else if template.starts_with("IP") {
                return Err(fail(format!(
                    "PROPERTY TYPE ERROR property {} has unknown type '{property_type}'",
                    self.short_form()
                )));
            } else if template.starts_with("P ") {
                property_type.as_str()
            } else if template.starts_with("DOMAIN") {
                let anns = self.axiom_annotations(column)?;
                for ce in class_expressions(b, self.names, template, value).map_err(fail)? {
                    self.insert(
                        model,
                        Component::DataPropertyDomain(DataPropertyDomain { dp: property.clone(), ce }),
                        anns.clone(),
                    );
                }
                continue;
            } else if template.starts_with("RANGE") {
                let anns = self.axiom_annotations(column)?;
                for dt in datatypes(self.names, value, split.as_deref()).map_err(fail)? {
                    self.insert(
                        model,
                        Component::DataPropertyRange(DataPropertyRange {
                            dp: property.clone(),
                            dr: DataRange::Datatype(b.datatype(dt)),
                        }),
                        anns.clone(),
                    );
                }
                continue;
            } else {
                continue;
            };
            let exprs = expressions()?;
            let anns = self.axiom_annotations(column)?;
            match relation {
                "subproperty" => {
                    for sup in exprs {
                        self.insert(
                            model,
                            Component::SubDataPropertyOf(SubDataPropertyOf { sup, sub: property.clone() }),
                            anns.clone(),
                        );
                    }
                }
                "equivalent" => {
                    for other in exprs {
                        self.insert(
                            model,
                            Component::EquivalentDataProperties(EquivalentDataProperties(vec![property.clone(), other])),
                            anns.clone(),
                        );
                    }
                }
                "disjoint" => {
                    let mut all = exprs;
                    if !all.contains(&property) {
                        all.push(property.clone());
                    }
                    self.insert(model, Component::DisjointDataProperties(DisjointDataProperties(all)), anns);
                }
                _ => {
                    return Err(fail(format!(
                        "PROPERTY TYPE ERROR property {} has unknown type '{property_type}'",
                        self.short_form()
                    )))
                }
            }
        }
        Ok(())
    }

    fn annotation_property_row(&self, model: &mut Model) -> Result<(), Fail> {
        let b = self.b;
        let property = b.annotation_property(self.iri.as_str());
        self.insert(
            model,
            Component::DeclareAnnotationProperty(DeclareAnnotationProperty(property.clone())),
            BTreeSet::new(),
        );
        let property_type = self.property_type();
        if property_type != "subproperty" {
            return Err((
                "PROPERTY_TYPE".to_string(),
                format!(
                    "ANNOTATION PROPERTY TYPE ERROR annotation property {} type '{property_type}' must be 'subproperty'",
                    self.iri
                ),
            ));
        }
        if self.table.characteristic.and_then(|i| self.cells.get(i)).is_some_and(|c| !c.trim().is_empty()) {
            return Err((
                "CHARACTERISTIC".to_string(),
                format!(
                    "ANNOTATION PROPERTY CHARACTERISTIC ERROR annotation property '{}' should not have any characteristics",
                    self.short_form()
                ),
            ));
        }
        for (column, template, value) in self.columns() {
            let fail = |m: String| (template.to_string(), m);
            let (_, split) = split_template(template);
            if template.starts_with('A') || template.starts_with("LABEL") {
                self.annotation_assertions(model, column, template, value)?;
            } else if template.starts_with("SP") || (template.starts_with('P') && !template.starts_with("PROPERTY_TYPE")) {
                let anns = self.axiom_annotations(column)?;
                for v in all_values(value, split.as_deref()) {
                    let content = wrap(&v);
                    let sup = annotation_property(self.names, &content).map_err(fail)?;
                    self.insert(
                        model,
                        Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
                            sup: b.annotation_property(sup),
                            sub: property.clone(),
                        }),
                        anns.clone(),
                    );
                }
            } else if template.starts_with("DOMAIN") || template.starts_with("RANGE") {
                let anns = self.axiom_annotations(column)?;
                for v in all_values(value, split.as_deref()) {
                    let Some(iri) = self.names.iri(v.trim(), true) else { continue };
                    let component = if template.starts_with("DOMAIN") {
                        Component::AnnotationPropertyDomain(AnnotationPropertyDomain {
                            ap: property.clone(),
                            iri: b.iri(iri),
                        })
                    } else {
                        Component::AnnotationPropertyRange(AnnotationPropertyRange {
                            ap: property.clone(),
                            iri: b.iri(iri),
                        })
                    };
                    self.insert(model, component, anns.clone());
                }
            }
        }
        Ok(())
    }

    fn datatype_row(&self, model: &mut Model) -> Result<(), Fail> {
        let b = self.b;
        self.insert(model, Component::DeclareDatatype(DeclareDatatype(b.datatype(self.iri.as_str()))), BTreeSet::new());
        for (column, template, value) in self.columns() {
            if template.starts_with('A') {
                self.annotation_assertions(model, column, template, value)?;
            }
        }
        Ok(())
    }

    fn individual_row(&self, model: &mut Model) -> Result<(), Fail> {
        let b = self.b;
        let individual = Individual::Named(b.named_individual(self.iri.as_str()));
        let Some(type_cell) = self.table.kind.and_then(|i| self.cells.get(i)) else {
            return Err((
                "TYPE".to_string(),
                format!("INDIVIDUAL TYPE ERROR individual {} has no TYPE cell", self.short_form()),
            ));
        };
        let type_cell = type_cell.trim();
        let types: Vec<&str> = match &self.table.kind_split {
            Some(split) if !split.is_empty() => {
                type_cell.split(split.as_str()).map(str::trim).filter(|t| !t.is_empty()).collect()
            }
            _ => vec![type_cell],
        };
        let individual_type = self
            .table
            .individual_type
            .and_then(|i| self.cells.get(i))
            .map(String::as_str)
            .unwrap_or("named");
        self.insert(
            model,
            Component::DeclareNamedIndividual(DeclareNamedIndividual(b.named_individual(self.iri.as_str()))),
            BTreeSet::new(),
        );
        for t in types {
            let type_or_iri = self.names.create(t).unwrap_or_else(|| t.to_string());
            if [
                "individual",
                "named individual",
                "http://www.w3.org/2002/07/owl#NamedIndividual",
                "http://www.w3.org/2002/07/owl#Individual",
            ]
            .iter()
            .any(|known| type_or_iri.eq_ignore_ascii_case(known))
            {
                continue;
            }
            let ce = match self.names.class(t, true) {
                Some(iri) => CE::Class(b.class(iri)),
                None => try_parse(b, self.names, t).map_err(|m| ("TYPE".to_string(), m))?,
            };
            self.insert(model, Component::ClassAssertion(ClassAssertion { ce, i: individual.clone() }), BTreeSet::new());
        }
        for (column, raw_template, value) in self.columns() {
            let (template, split) = split_template(raw_template);
            let fail = |m: String| (raw_template.to_string(), m);
            let others = || individuals(b, self.names, value, split.as_deref()).map_err(fail);
            if template.starts_with('A') || template.starts_with("LABEL") {
                self.annotation_assertions(model, column, raw_template, value)?;
                continue;
            }
            if template.starts_with("TI ") {
                for ce in class_expressions(b, self.names, raw_template, value).map_err(fail)? {
                    self.insert(model, Component::ClassAssertion(ClassAssertion { ce, i: individual.clone() }), BTreeSet::new());
                }
                continue;
            }
            let relation = if template.starts_with("SI") {
                "same"
            } else if template.starts_with("DI") {
                "different"
            } else if template.starts_with('I') && !template.starts_with("INDIVIDUAL_TYPE") {
                individual_type
            } else {
                continue;
            };
            match relation {
                "named" => {
                    let Some(prop) = template.strip_prefix("I ") else { continue };
                    let prop = prop.replace('\'', "");
                    if let Some(op) = self.names.object_property(&prop) {
                        let anns = self.axiom_annotations(column)?;
                        for to in others()? {
                            self.insert(
                                model,
                                Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
                                    ope: OPE::ObjectProperty(b.object_property(op.as_str())),
                                    from: individual.clone(),
                                    to,
                                }),
                                anns.clone(),
                            );
                        }
                    } else if let Some(dp) = self.names.data_property(&prop) {
                        let anns = self.axiom_annotations(column)?;
                        for to in literals(b, self.names, value, split.as_deref()).map_err(fail)? {
                            self.insert(
                                model,
                                Component::DataPropertyAssertion(DataPropertyAssertion {
                                    dp: b.data_property(dp.as_str()),
                                    from: individual.clone(),
                                    to,
                                }),
                                anns.clone(),
                            );
                        }
                    }
                }
                "same" | "different" => {
                    let mut all = others()?;
                    if all.is_empty() {
                        continue;
                    }
                    if !all.contains(&individual) {
                        all.push(individual.clone());
                    }
                    let anns = self.axiom_annotations(column)?;
                    let component = if relation == "same" {
                        Component::SameIndividual(SameIndividual(all))
                    } else {
                        Component::DifferentIndividuals(DifferentIndividuals(all))
                    };
                    self.insert(model, component, anns);
                }
                _ => {
                    return Err(fail(format!(
                        "INDIVIDUAL TYPE ERROR individual {} has unknown type '{individual_type}'",
                        self.short_form()
                    )))
                }
            }
        }
        Ok(())
    }
}

// === Cells =================================================================

/// The class expressions a class-expression template string and its cell
/// make: one per split value; `CI` names a class.
fn class_expressions(b: &Build<RcStr>, names: &Names, template: &str, value: &str) -> Result<Vec<CE<RcStr>>, String> {
    let (template, split) = split_template(template);
    let values = match &split {
        Some(s) => split_values(value, s),
        None => vec![value.to_string()],
    };
    let mut out = Vec::new();
    for v in values {
        let ce = if template.starts_with("CI") {
            try_parse(b, names, &wrap(&v))?
        } else {
            class_expression(b, names, &template, &v)?
        };
        if !out.contains(&ce) {
            out.push(ce);
        }
    }
    Ok(out)
}

/// The class expression a cell makes: the cell as one name, filled in at the
/// template string's `%`; the cell alone where the template string has none.
fn class_expression(b: &Build<RcStr>, names: &Names, template: &str, value: &str) -> Result<CE<RcStr>, String> {
    let content = wrap(value);
    let sub = if template.contains('%') {
        after_first_space(template).replace('%', &content)
    } else {
        content
    };
    try_parse(b, names, &sub)
}

/// The class `content` names outright, as a name or as an IRI, else the class
/// expression it parses to.
fn try_parse(b: &Build<RcStr>, names: &Names, content: &str) -> Result<CE<RcStr>, String> {
    let content = content.trim();
    if let Some(iri) = names.class(content, false) {
        return Ok(CE::Class(b.class(iri)));
    }
    if content.starts_with("http") {
        if let Some(iri) = names.class(content, true) {
            return Ok(CE::Class(b.class(iri)));
        }
    }
    manchester_parse::parse_class_expression(b, content, names)
        .map_err(|e| format!("MANCHESTER PARSE ERROR the expression ({content}) cannot be parsed: {e}"))
}

/// A template string from its first space on: the expression a cell fills in.
fn after_first_space(template: &str) -> &str {
    template.find(' ').map(|i| &template[i..]).unwrap_or(template)
}

/// The object property expressions a property template string and its cell
/// make: one list per split value; `PI` names a property.
fn object_property_expressions(
    b: &Build<RcStr>,
    names: &Names,
    template: &str,
    value: &str,
) -> Result<Vec<OPE<RcStr>>, String> {
    let (template, split) = split_template(template);
    let mut out: Vec<OPE<RcStr>> = Vec::new();
    for v in all_values(value, split.as_deref()) {
        let content = wrap(&v);
        let list = if template.starts_with("PI") {
            let iri = names
                .object_property(&content)
                .ok_or_else(|| format!("could not resolve object property '{content}'"))?;
            vec![OPE::ObjectProperty(b.object_property(iri))]
        } else {
            let sub = after_first_space(&template).trim().replace('%', &content);
            manchester_parse::parse_object_property_list(b, &sub, names)
                .map_err(|e| format!("MANCHESTER PARSE ERROR the expression ({sub}) cannot be parsed: {e}"))?
        };
        for e in list {
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    Ok(out)
}

/// The data properties a property template string and its cell make.
fn data_property_expressions(
    b: &Build<RcStr>,
    names: &Names,
    template: &str,
    value: &str,
) -> Result<Vec<horned_owl::model::DataProperty<RcStr>>, String> {
    let (template, split) = split_template(template);
    let mut out = Vec::new();
    for v in all_values(value, split.as_deref()) {
        let content = wrap(&v);
        let list = if template.starts_with("PI") {
            let iri = names
                .data_property(&content)
                .ok_or_else(|| format!("could not resolve data property '{content}'"))?;
            vec![b.data_property(iri)]
        } else {
            let sub = after_first_space(&template).trim().replace('%', &content);
            manchester_parse::parse_data_property_list(b, &sub, names)
                .map_err(|e| format!("MANCHESTER PARSE ERROR the expression ({sub}) cannot be parsed: {e}"))?
        };
        for e in list {
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    Ok(out)
}

/// The datatype IRIs a cell names, one per value.
fn datatypes(names: &Names, value: &str, split: Option<&str>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for v in all_values(value, split) {
        let iri = datatype(names, &wrap(&v))?;
        if !out.contains(&iri) {
            out.push(iri);
        }
    }
    Ok(out)
}

fn datatype(names: &Names, name: &str) -> Result<String, String> {
    names
        .datatype(name, true)
        .ok_or_else(|| format!("DATATYPE ERROR could not find datatype for '{name}'"))
}

fn annotation_property(names: &Names, name: &str) -> Result<String, String> {
    names
        .annotation_property(name, true)
        .ok_or_else(|| format!("ANNOTATION PROPERTY ERROR could not handle annotation property: {name}"))
}

/// The individuals a cell names, one per value; none for a blank cell.
fn individuals(b: &Build<RcStr>, names: &Names, value: &str, split: Option<&str>) -> Result<Vec<Individual<RcStr>>, String> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for v in all_values(value, split) {
        let iri = names.individual(&v).ok_or_else(|| format!("could not resolve individual '{v}'"))?;
        let i = Individual::Named(b.named_individual(iri));
        if !out.contains(&i) {
            out.push(i);
        }
    }
    Ok(out)
}

/// The literals a cell holds, one per value: `text^^datatype` typed, anything
/// else a string.
fn literals(b: &Build<RcStr>, names: &Names, value: &str, split: Option<&str>) -> Result<Vec<Literal<RcStr>>, String> {
    let mut out = Vec::new();
    for v in all_values(value, split) {
        let lit = match v.split_once("^^") {
            Some((text, dt)) => {
                let datatype_iri = datatype(names, dt)?;
                literal_as_made(Literal::Datatype { literal: text.trim().to_string(), datatype_iri: b.iri(datatype_iri) })
            }
            None => string_literal(b, v.trim()),
        };
        if !out.contains(&lit) {
            out.push(lit);
        }
    }
    Ok(out)
}

/// A string literal: its text typed `xsd:string`, as a data factory makes a
/// literal from text alone.
fn string_literal(b: &Build<RcStr>, text: &str) -> Literal<RcStr> {
    Literal::Datatype { literal: text.to_string(), datatype_iri: b.iri(XSD_STRING) }
}

fn annotation(b: &Build<RcStr>, ap: &str, av: AnnotationValue<RcStr>) -> Annotation<RcStr> {
    Annotation { ann: Default::default(), ap: b.annotation_property(ap), av }
}

/// The annotations an annotation template string and its cell make:
///   `A <p>` / `LABEL`       a string per value
///   `AT <p>^^<dt>`          a typed literal per value
///   `AL <p>@<lang>`         a language-tagged literal per value
///   `AI <p>`                an IRI per value: the name's, else the value itself
///   `AP`                    a `property value` pair per value
/// A `C`, `CT`, `CL` or `CI` template string reads as its `A` counterpart. A
/// leading `>` is dropped. Any other template string makes none.
fn annotations_for(b: &Build<RcStr>, names: &Names, template: &str, value: &str) -> Result<Vec<Annotation<RcStr>>, String> {
    let (template, split) = split_template(template);
    let template = template.strip_prefix('>').unwrap_or(&template);
    let each = |value: &str| -> Vec<String> {
        match &split {
            Some(s) => split_values(value, s),
            None => vec![value.to_string()],
        }
    };
    if template == "AP" {
        return annotation_pairs(b, names, split.as_deref(), value);
    }
    if template == "LABEL" {
        return Ok(each(value)
            .into_iter()
            .map(|v| annotation(b, RDFS_LABEL, AnnotationValue::Literal(string_literal(b, &v))))
            .collect());
    }
    if template.starts_with("A ") || template.starts_with("C ") {
        let ap = annotation_property(names, template[1..].trim())?;
        return Ok(each(value)
            .into_iter()
            .map(|v| annotation(b, &ap, AnnotationValue::Literal(string_literal(b, &v))))
            .collect());
    }
    if template.starts_with("AT ") || template.starts_with("CT ") {
        let rest = template[2..].trim();
        let Some((property, dt)) = rest.split_once("^^") else {
            return Err(format!("TYPED FORMAT ERROR invalid typed annotation string: {template}"));
        };
        let ap = annotation_property(names, property.trim())?;
        let datatype_iri = datatype(names, dt.trim())?;
        return Ok(each(value)
            .into_iter()
            .map(|v| {
                let lit = literal_as_made(Literal::Datatype { literal: v, datatype_iri: b.iri(datatype_iri.as_str()) });
                annotation(b, &ap, AnnotationValue::Literal(lit))
            })
            .collect());
    }
    if template.starts_with("AL ") || template.starts_with("CL ") {
        let rest = template[2..].trim();
        let Some((property, lang)) = rest.split_once('@') else {
            return Err(format!("LANGUAGE FORMAT ERROR invalid language annotation template string: {template}"));
        };
        let ap = annotation_property(names, property.trim())?;
        let lang = lang.trim();
        return Ok(all_values(value, split.as_deref())
            .into_iter()
            .map(|v| {
                let lit = literal_as_made(Literal::Language { literal: v, lang: lang.to_string() });
                annotation(b, &ap, AnnotationValue::Literal(lit))
            })
            .collect());
    }
    if template.starts_with("AI ") || template.starts_with("CI ") {
        let ap = annotation_property(names, template[2..].trim())?;
        return Ok(each(value)
            .into_iter()
            .map(|v| {
                let iri = names.iri(&v, true).unwrap_or(v);
                annotation(b, &ap, AnnotationValue::IRI(b.iri(iri)))
            })
            .collect());
    }
    Ok(Vec::new())
}

/// The annotations of an `AP` cell: `property value` pairs, the value a
/// single- or double-quoted string, optionally tagged `@lang` or typed
/// `^^datatype`, a `<IRI>`, or a name or CURIE of an IRI.
fn annotation_pairs(b: &Build<RcStr>, names: &Names, split: Option<&str>, value: &str) -> Result<Vec<Annotation<RcStr>>, String> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r"^(.+)\s+'([^']+)'$",
            r#"^(.+)\s+"([^"]+)"$"#,
            r"^(.+)\s+'([^']+)'@([0-9A-Za-z_]+)$",
            r#"^(.+)\s+"([^"]+)"@([0-9A-Za-z_]+)$"#,
            r"^(.+)\s+'([^']+)'\^\^(\S+)$",
            r#"^(.+)\s+"([^"]+)"\^\^(\S+)$"#,
            r"^(.+)\s+<(\S+)>$",
            r"^(.+)\s+(.+)$",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("annotation pair pattern"))
        .collect()
    });
    let pairs: Vec<String> = match split {
        Some(s) if !s.is_empty() => java_split(value, s),
        _ => vec![value.to_string()],
    };
    let mut out = Vec::new();
    for pair in pairs {
        let pair = pair.trim();
        let found = patterns.iter().enumerate().find_map(|(i, p)| p.captures(pair).map(|c| (i, c)));
        let Some((which, c)) = found else {
            return Err(format!("ANNOTATION PARSE ERROR invalid annotation string: {pair}"));
        };
        let ap = annotation_property(names, &c[1])?;
        let av = match which {
            0 | 1 => AnnotationValue::Literal(string_literal(b, &c[2])),
            2 | 3 => AnnotationValue::Literal(literal_as_made(Literal::Language {
                literal: c[2].to_string(),
                lang: c[3].to_string(),
            })),
            4 | 5 => {
                let datatype_iri = datatype(names, &c[3])?;
                AnnotationValue::Literal(literal_as_made(Literal::Datatype {
                    literal: c[2].to_string(),
                    datatype_iri: b.iri(datatype_iri),
                }))
            }
            _ => {
                let iri = names
                    .iri(&c[2], true)
                    .ok_or_else(|| format!("ANNOTATION PARSE ERROR invalid annotation string: {pair}"))?;
                AnnotationValue::IRI(b.iri(iri))
            }
        };
        out.push(annotation(b, &ap, av));
    }
    Ok(out)
}

/// Declare every annotation property the generated axioms use, but for the
/// OWL, RDF, RDFS and XSD vocabulary, which is known already.
fn declare_annotation_properties(b: &Build<RcStr>, model: &mut Model) {
    let mut properties: BTreeSet<String> = BTreeSet::new();
    for ac in model.ont.iter() {
        let mut entities = typed_signature(&ac.component);
        for a in ac.ann.iter() {
            annotation_properties_in(a, &mut entities);
        }
        properties.extend(entities.into_iter().filter(|(k, _)| *k == kind::ANNOTATION_PROPERTY).map(|(_, iri)| iri));
    }
    for iri in properties {
        if !is_builtin_vocabulary(&iri) {
            model.ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(b.annotation_property(iri))));
        }
    }
}

/// True for IRIs in the RDF/RDFS/OWL/XSD namespaces — the built-in vocabulary a
/// template never re-declares.
fn is_builtin_vocabulary(iri: &str) -> bool {
    const BUILTIN_NS: [&str; 4] = [
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2000/01/rdf-schema#",
        "http://www.w3.org/2002/07/owl#",
        "http://www.w3.org/2001/XMLSchema#",
    ];
    BUILTIN_NS.iter().any(|ns| iri.starts_with(ns))
}
