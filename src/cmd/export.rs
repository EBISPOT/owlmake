//! `export` — an ontology's entities as a table: one row per entity, one column
//! per header field (`ID|LABEL|SubClass Of`), written as TSV, CSV, HTML, JSON or
//! an Excel workbook.
//!
//! The rows are the root document's classes and named individuals (or the kinds
//! `--include` names: `classes`, `properties`, `individuals`), less the top and
//! bottom entities and every datatype. A property row is an object, data or
//! annotation property the document uses, declared or not.
//!
//! A column is resolved from its header. The keywords are `ID`/`CURIE` (the
//! entity's CURIE), `IRI`, `LABEL`, `SYNONYMS`, `SubClasses`, and — in their
//! spaced, unspaced, camel-case and lower-case spellings — `SubClass Of`,
//! `SubProperty Of`, `Equivalent Class(es)`, `Equivalent Propert(y|ies)`,
//! `Disjoint With`, `Type`, `Domain` and `Range`. Any other header names a
//! property by its label or by CURIE or IRI. An annotation property column lists
//! the property's values on the entity; an object or data property column lists
//! the fillers of the restrictions on it among a class's anonymous superclasses
//! and equivalents, and an individual's assertions of it. A header that names
//! none of these is an error.
//!
//! A trailing bracketed tag sets how the column renders entities — `NAME`,
//! `LABEL`, `ID` or `IRI` — and which values it holds: `ANY`, `NAMED`, or `ANON`
//! (`ANONYMOUS`). `--entity-format` and `--entity-select` set them where a column
//! has no tag; `ID`, `CURIE`, `IRI` and `LABEL` columns always render their own
//! way and hold named values.
//!
//! An entity renders by CURIE, against the built-in prefix map and the prefixes
//! given on the command line (the longest namespace wins; the document's own
//! prefixes take no part), by IRI, by label (empty without one), or by name: its
//! label, or its CURIE without one, quoted when it holds a space and stands inside
//! an expression. Expressions are Manchester syntax on one line, with runs of
//! spaces collapsed. A cell's values are sorted by their plain rendering, empty
//! values last; rows are sorted on the `--sort` columns, empty values last.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{bail, Context as _, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotationSubject, AnnotationValue, ClassExpression as CE, Component, DataRange as DR,
    Individual, Literal, ObjectPropertyExpression as OPE, RcStr,
};

use crate::io::entities::{held_labels, HeldLabel, Kind};
use crate::io::manchester_write::{object_text, Layout, Names, Object, ShortForms};
use crate::io::natural_order::{iri_cmp, str_cmp, NaturalOrder};
use crate::model::Model;
use crate::owlapi_hash::{annotation_assertion_hash, java_hashset_capacity, java_string_hash};

const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDFS_SUBPROPERTY_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const RDFS_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const OWL_EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const OWL_EQUIVALENT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#equivalentProperty";
const OWL_DISJOINT_WITH: &str = "http://www.w3.org/2002/07/owl#disjointWith";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// The annotation properties a `SYNONYMS` column collects.
const SYNONYM_PROPERTIES: [&str; 5] = [
    "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym",
    "http://www.geneontology.org/formats/oboInOwl#hasBroadSynonym",
    "http://www.geneontology.org/formats/oboInOwl#hasNarrowSynonym",
    "http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym",
    "http://purl.obolibrary.org/obo/IAO_0000118",
];

/// The keyword headers that name an axiom type rather than a property.
const KEYWORDS: &[(&str, &str)] = &[
    ("SubClass Of", RDFS_SUBCLASS_OF),
    ("SubProperty Of", RDFS_SUBPROPERTY_OF),
    ("Equivalent Class", OWL_EQUIVALENT_CLASS),
    ("Equivalent Classes", OWL_EQUIVALENT_CLASS),
    ("Equivalent Property", OWL_EQUIVALENT_PROPERTY),
    ("Equivalent Properties", OWL_EQUIVALENT_PROPERTY),
    ("Disjoint With", OWL_DISJOINT_WITH),
    ("Type", RDF_TYPE),
    ("SubClassOf", RDFS_SUBCLASS_OF),
    ("SubPropertyOf", RDFS_SUBPROPERTY_OF),
    ("EquivalentClass", OWL_EQUIVALENT_CLASS),
    ("EquivalentClasses", OWL_EQUIVALENT_CLASS),
    ("EquivalentProperty", OWL_EQUIVALENT_PROPERTY),
    ("EquivalentProperties", OWL_EQUIVALENT_PROPERTY),
    ("DisjointWith", OWL_DISJOINT_WITH),
    ("Domain", RDFS_DOMAIN),
    ("Range", RDFS_RANGE),
    ("subClassOf", RDFS_SUBCLASS_OF),
    ("subPropertyOf", RDFS_SUBPROPERTY_OF),
    ("equivalentClass", OWL_EQUIVALENT_CLASS),
    ("equivalentClasses", OWL_EQUIVALENT_CLASS),
    ("equivalentProperty", OWL_EQUIVALENT_PROPERTY),
    ("equivalentProperties", OWL_EQUIVALENT_PROPERTY),
    ("disjointWith", OWL_DISJOINT_WITH),
    ("subclass of", RDFS_SUBCLASS_OF),
    ("subproperty of", RDFS_SUBPROPERTY_OF),
    ("equivalent class", OWL_EQUIVALENT_CLASS),
    ("equivalent classes", OWL_EQUIVALENT_CLASS),
    ("equivalent property", OWL_EQUIVALENT_PROPERTY),
    ("equivalent properties", OWL_EQUIVALENT_PROPERTY),
    ("disjoint with", OWL_DISJOINT_WITH),
    ("type", RDF_TYPE),
    ("subclassof", RDFS_SUBCLASS_OF),
    ("subpropertyof", RDFS_SUBPROPERTY_OF),
    ("equivalentclass", OWL_EQUIVALENT_CLASS),
    ("equivalentclasses", OWL_EQUIVALENT_CLASS),
    ("equivalentproperty", OWL_EQUIVALENT_PROPERTY),
    ("equivalentproperties", OWL_EQUIVALENT_PROPERTY),
    ("disjointwith", OWL_DISJOINT_WITH),
    ("domain", RDFS_DOMAIN),
    ("range", RDFS_RANGE),
];

/// The formats `--format` names, and the extensions the format is read from.
const FORMATS: [&str; 6] = ["csv", "html", "html-list", "json", "tsv", "xlsx"];

/// The stylesheet a standalone HTML table links.
const BOOTSTRAP_CSS: &str = "https://stackpath.bootstrapcdn.com/bootstrap/4.5.2/css/bootstrap.min.css";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// The file to write the table to. Required.
    #[arg(short = 'e', long = "export", value_name = "FILE")]
    pub export: Option<PathBuf>,
    /// The columns, in order, separated by `|`: e.g. `ID|LABEL|SubClass Of`.
    /// Required.
    #[arg(short = 'c', long = "header", value_name = "COLS")]
    pub header: Option<String>,
    /// The columns to sort the rows on, separated by `|`, each written as in
    /// `--header`; `^` before a column sorts it in reverse. The last one listed
    /// orders the rows, and each before it orders the rows the later ones leave
    /// tied. Default: the first column.
    #[arg(short = 's', long, value_name = "COLS")]
    pub sort: Option<String>,
    /// The kinds of entity to export, separated by spaces, or else by commas, or
    /// else by tabs: `classes`, `properties`, `individuals`. Default: `classes
    /// individuals`.
    #[arg(short = 'n', long, value_name = "KINDS")]
    pub include: Option<String>,
    /// Output format: `tsv`, `csv`, `html`, `html-list`, `json` or `xlsx`.
    /// Default: the export file's extension when it is one of these, else `tsv`.
    #[arg(short, long)]
    pub format: Option<String>,
    /// The separator between the values of one cell. Default: `|`.
    #[arg(short = 'S', long = "split", value_name = "SEP")]
    pub split: Option<String>,
    /// How a column with no tag renders entities: `NAME` (label, quoted inside
    /// expressions, else CURIE), `LABEL`, `ID` (CURIE) or `IRI`. Default: `NAME`.
    #[arg(short = 'E', long = "entity-format", value_name = "FORMAT")]
    pub entity_format: Option<String>,
    /// Which values a column with no tag holds: `ANY`, `NAMED` or
    /// `ANON`/`ANONYMOUS`. Default: `ANY`.
    #[arg(short = 'l', long = "entity-select", value_name = "SELECT")]
    pub entity_select: Option<String>,
    /// For HTML: `true` or `yes` writes a whole page with its stylesheet link;
    /// anything else writes the table alone. Default: `true`.
    #[arg(long, value_name = "BOOL")]
    pub standalone: Option<String>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    let header = args.header.as_deref().context("export: --header is a required option")?;
    let path = args.export.as_ref().context("export: an export file must be specified with --export")?;
    let format = match &args.format {
        Some(f) if FORMATS.contains(&f.as_str()) => f.clone(),
        Some(f) => bail!("export: --format {f} must be one of: csv, html, html-list, json, tsv or xlsx"),
        None => {
            let ext = extension(path).to_lowercase();
            if FORMATS.contains(&ext.as_str()) { ext } else { "tsv".to_string() }
        }
    };
    let html = format.starts_with("html");

    let ctx = Ctx::new(&model)?;
    let names = java_split(header, '|');
    if names.is_empty() {
        bail!("export: --header '{header}' names no column");
    }
    let entity_format = args.entity_format.clone().unwrap_or_else(|| "NAME".to_string());
    if !["id", "iri", "label", "name"].contains(&entity_format.to_lowercase().as_str()) {
        bail!("export: '{entity_format}' is not a valid entity rendering format");
    }
    let entity_select = args.entity_select.clone().unwrap_or_else(|| "ANY".to_string());
    if !["any", "named", "anon", "anonymous"].contains(&entity_select.to_lowercase().as_str()) {
        bail!("export: '{entity_select}' is not a valid entity selection");
    }
    let sort = args.sort.clone().unwrap_or_else(|| names[0].clone());
    let columns = ctx.columns(&names, &entity_format, &entity_select, &java_split(java_trim(&sort), '|'))?;

    let entities = ctx.entities(args.include.as_deref().unwrap_or("classes individuals"))?;
    let mut rows = Vec::with_capacity(entities.len());
    for (kind, iri) in &entities {
        rows.push(ctx.row(&columns, *kind, iri, html)?);
    }
    sort_rows(&columns, &mut rows)?;

    let split = args.split.as_deref().unwrap_or("|");
    let standalone = args
        .standalone
        .as_deref()
        .map(|s| matches!(s.trim().to_lowercase().as_str(), "true" | "yes"))
        .unwrap_or(true);
    let bytes = match format.as_str() {
        "tsv" => delimited(&columns, &rows, split, '\t').into_bytes(),
        "csv" => delimited(&columns, &rows, split, ',').into_bytes(),
        "html" => html_table(&columns, &rows, split, standalone).into_bytes(),
        // An HTML list is the JSON array with every entity written as a link.
        "html-list" | "json" => json(&columns, &rows).into_bytes(),
        "xlsx" => crate::xlsx::workbook(&grid(&columns, &rows, split))
            .with_context(|| format!("export: writing {}", path.display()))?,
        _ => unreachable!("the format is one of FORMATS"),
    };
    std::fs::write(path, bytes).with_context(|| format!("export: writing {}", path.display()))?;
    Ok(Some(model))
}

/// The extension of a path's file name: what follows its last `.`, or nothing.
fn extension(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let name = s.rsplit(['/', '\\']).next().unwrap_or("");
    name.rfind('.').map(|i| name[i + 1..].to_string()).unwrap_or_default()
}

/// `s` split at each `sep`, empty fields kept except at the end.
fn java_split(s: &str, sep: char) -> Vec<String> {
    let mut parts: Vec<String> = s.split(sep).map(str::to_string).collect();
    while parts.len() > 1 && parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    if parts.len() == 1 && parts[0].is_empty() && !s.is_empty() {
        parts.clear();
    }
    parts
}

/// `s` without the characters up to U+0020 at either end.
fn java_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

// === Columns ==============================================================

/// How a column renders an entity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Provider {
    /// Its CURIE.
    Curie,
    /// Its IRI.
    Iri,
    /// Its label, else its CURIE; quoted inside an expression when it holds a
    /// space.
    Name,
    /// Its label, else nothing.
    Label,
}

impl Provider {
    fn of(format: &str) -> Option<Provider> {
        match format.to_uppercase().as_str() {
            "ID" | "CURIE" => Some(Provider::Curie),
            "IRI" => Some(Provider::Iri),
            "NAME" => Some(Provider::Name),
            "LABEL" => Some(Provider::Label),
            _ => None,
        }
    }
}

/// What a column's values come from.
enum Source {
    Annotation(String),
    Data(String),
    Object(String),
    /// A keyword column, or none at all — the IRI its header resolves to.
    Iri(Option<String>),
}

struct Column {
    /// The header without its tag.
    name: String,
    /// The header as written, which is also how a row finds its cell.
    display: String,
    source: Source,
    provider: Provider,
    named: bool,
    anonymous: bool,
    /// Where `--sort` lists the column, and whether in reverse.
    sort: Option<(i64, bool)>,
}

/// A cell: the values it shows, and the text rows are sorted on.
struct Cell {
    display: Vec<String>,
    sort: String,
}

impl Cell {
    fn single(display: String, sort: String) -> Cell {
        Cell { display: vec![display], sort }
    }

    /// Values sorted by their sort text, empty ones last, ties in the order
    /// given.
    fn values(mut pairs: Vec<(String, String)>) -> Cell {
        pairs.sort_by(|a, b| match (a.1.is_empty(), b.1.is_empty()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => str_cmp(&a.1, &b.1),
        });
        let sort = pairs.iter().map(|p| p.1.as_str()).collect::<Vec<_>>().join("|");
        Cell { display: pairs.into_iter().map(|p| p.0).collect(), sort }
    }

    fn of_strings(values: Vec<String>) -> Cell {
        Cell::values(values.into_iter().map(|v| (v.clone(), v)).collect())
    }

    /// The cell as one text: its values joined by `split`, a `split` inside a
    /// value escaped with `\` when there is more than one value.
    fn joined(&self, split: &str) -> String {
        if self.display.len() > 1 {
            let escaped = format!("\\{split}");
            self.display.iter().map(|v| v.replace(split, &escaped)).collect::<Vec<_>>().join(split)
        } else {
            self.display.join(split)
        }
    }
}

/// A row: its cells, by the header that names their column.
struct Row {
    kind: Kind,
    iri: String,
    cells: HashMap<String, Cell>,
}

// === The ontology, indexed ================================================

/// Everything a table is built from: the prefix map, the labels, and the root
/// document's axioms indexed by the entity they are about.
struct Ctx<'m> {
    order: NaturalOrder,
    /// The prefixes CURIEs are written with, longest namespace first.
    curies: Vec<(String, String)>,
    /// The same prefixes as a prefix manager names IRIs with them.
    prefix_manager: ShortForms,
    /// The built-in prefixes alone.
    builtin_names: ShortForms,
    /// The prefixes a header's CURIE is expanded with, in the order given.
    terms: Vec<(String, String)>,
    labels: HashMap<&'m str, HeldLabel<'m>>,
    /// The root document's labels, by label: the IRI a header names.
    label_iris: HashMap<&'m str, &'m str>,
    /// Property names — short form and labels — by kind, over the whole
    /// closure: the IRI a header names.
    property_names: HashMap<Kind, HashMap<String, String>>,
    /// The root document's entities.
    signature: HashSet<(Kind, String)>,
    /// How many entities the root document has, of every kind.
    signature_size: usize,
    /// The root document's entities, kind by kind.
    root_entities: Vec<(Kind, String)>,
    ix: Index<'m>,
}

/// The root document's axioms, by the entity they are about.
#[derive(Default)]
struct Index<'m> {
    supers: HashMap<&'m str, Vec<&'m CE<RcStr>>>,
    subs: HashMap<&'m str, Vec<&'m CE<RcStr>>>,
    equivalents: HashMap<&'m str, Vec<&'m CE<RcStr>>>,
    disjoints: HashMap<&'m str, Vec<&'m CE<RcStr>>>,
    types: HashMap<&'m str, Vec<&'m CE<RcStr>>>,
    super_object_properties: HashMap<&'m str, Vec<&'m OPE<RcStr>>>,
    super_data_properties: HashMap<&'m str, Vec<&'m str>>,
    super_annotation_properties: HashMap<&'m str, Vec<&'m str>>,
    equivalent_object_properties: HashMap<&'m str, Vec<&'m OPE<RcStr>>>,
    equivalent_data_properties: HashMap<&'m str, Vec<&'m str>>,
    disjoint_object_properties: HashMap<&'m str, Vec<&'m OPE<RcStr>>>,
    disjoint_data_properties: HashMap<&'m str, Vec<&'m str>>,
    object_domains: HashMap<&'m str, Vec<&'m CE<RcStr>>>,
    data_domains: HashMap<&'m str, Vec<&'m CE<RcStr>>>,
    annotation_domains: HashMap<&'m str, Vec<&'m str>>,
    object_ranges: HashMap<&'m str, Vec<&'m CE<RcStr>>>,
    data_ranges: HashMap<&'m str, Vec<&'m DR<RcStr>>>,
    annotation_ranges: HashMap<&'m str, Vec<&'m str>>,
    object_values: HashMap<(&'m str, &'m str), Vec<&'m Individual<RcStr>>>,
    data_values: HashMap<(&'m str, &'m str), Vec<&'m Literal<RcStr>>>,
    /// Annotation assertions by subject: property and value.
    annotations: HashMap<&'m str, Vec<(&'m str, &'m AnnotationValue<RcStr>)>>,
}

/// Push `x` onto the list at `key` unless an equal one is there.
fn push_distinct<'m, K: std::hash::Hash + Eq, T: PartialEq + ?Sized>(
    map: &mut HashMap<K, Vec<&'m T>>,
    key: K,
    x: &'m T,
) {
    let v = map.entry(key).or_default();
    if !v.contains(&x) {
        v.push(x);
    }
}

impl<'m> Index<'m> {
    fn build(model: &'m Model) -> Index<'m> {
        let mut ix = Index::default();
        for ac in model.ont.iter().filter(|ac| !model.imported_components.contains(*ac)) {
            match &ac.component {
                Component::SubClassOf(sc) => {
                    if let CE::Class(c) = &sc.sub {
                        ix.supers.entry(c.0.as_ref()).or_default().push(&sc.sup);
                    }
                    if let CE::Class(c) = &sc.sup {
                        ix.subs.entry(c.0.as_ref()).or_default().push(&sc.sub);
                    }
                }
                Component::EquivalentClasses(eq) => {
                    for member in &eq.0 {
                        if let CE::Class(c) = member {
                            for other in &eq.0 {
                                push_distinct(&mut ix.equivalents, c.0.as_ref(), other);
                            }
                        }
                    }
                }
                Component::DisjointClasses(dj) => {
                    for member in &dj.0 {
                        if let CE::Class(c) = member {
                            for other in &dj.0 {
                                push_distinct(&mut ix.disjoints, c.0.as_ref(), other);
                            }
                        }
                    }
                }
                Component::ClassAssertion(ca) => {
                    if let Individual::Named(i) = &ca.i {
                        push_distinct(&mut ix.types, i.0.as_ref(), &ca.ce);
                    }
                }
                Component::SubObjectPropertyOf(sp) => {
                    if let horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(
                        OPE::ObjectProperty(p),
                    ) = &sp.sub
                    {
                        ix.super_object_properties.entry(p.0.as_ref()).or_default().push(&sp.sup);
                    }
                }
                Component::SubDataPropertyOf(sp) => {
                    ix.super_data_properties.entry(sp.sub.0.as_ref()).or_default().push(sp.sup.0.as_ref());
                }
                Component::SubAnnotationPropertyOf(sp) => {
                    ix.super_annotation_properties
                        .entry(sp.sub.0.as_ref())
                        .or_default()
                        .push(sp.sup.0.as_ref());
                }
                Component::EquivalentObjectProperties(eq) => {
                    for member in &eq.0 {
                        if let OPE::ObjectProperty(p) = member {
                            for other in &eq.0 {
                                push_distinct(&mut ix.equivalent_object_properties, p.0.as_ref(), other);
                            }
                        }
                    }
                }
                Component::EquivalentDataProperties(eq) => {
                    for member in &eq.0 {
                        for other in &eq.0 {
                            push_distinct(&mut ix.equivalent_data_properties, member.0.as_ref(), other.0.as_ref());
                        }
                    }
                }
                Component::DisjointObjectProperties(dj) => {
                    for member in &dj.0 {
                        if let OPE::ObjectProperty(p) = member {
                            for other in &dj.0 {
                                push_distinct(&mut ix.disjoint_object_properties, p.0.as_ref(), other);
                            }
                        }
                    }
                }
                Component::DisjointDataProperties(dj) => {
                    for member in &dj.0 {
                        for other in &dj.0 {
                            push_distinct(&mut ix.disjoint_data_properties, member.0.as_ref(), other.0.as_ref());
                        }
                    }
                }
                Component::ObjectPropertyDomain(d) => {
                    if let OPE::ObjectProperty(p) = &d.ope {
                        ix.object_domains.entry(p.0.as_ref()).or_default().push(&d.ce);
                    }
                }
                Component::DataPropertyDomain(d) => {
                    ix.data_domains.entry(d.dp.0.as_ref()).or_default().push(&d.ce);
                }
                Component::AnnotationPropertyDomain(d) => {
                    ix.annotation_domains.entry(d.ap.0.as_ref()).or_default().push(d.iri.as_ref());
                }
                Component::ObjectPropertyRange(r) => {
                    if let OPE::ObjectProperty(p) = &r.ope {
                        ix.object_ranges.entry(p.0.as_ref()).or_default().push(&r.ce);
                    }
                }
                Component::DataPropertyRange(r) => {
                    ix.data_ranges.entry(r.dp.0.as_ref()).or_default().push(&r.dr);
                }
                Component::AnnotationPropertyRange(r) => {
                    ix.annotation_ranges.entry(r.ap.0.as_ref()).or_default().push(r.iri.as_ref());
                }
                Component::ObjectPropertyAssertion(a) => {
                    if let (OPE::ObjectProperty(p), Individual::Named(from)) = (&a.ope, &a.from) {
                        push_distinct(&mut ix.object_values, (from.0.as_ref(), p.0.as_ref()), &a.to);
                    }
                }
                Component::DataPropertyAssertion(a) => {
                    if let Individual::Named(from) = &a.from {
                        push_distinct(&mut ix.data_values, (from.0.as_ref(), a.dp.0.as_ref()), &a.to);
                    }
                }
                Component::AnnotationAssertion(aa) => {
                    if let AnnotationSubject::IRI(s) = &aa.subject {
                        ix.annotations.entry(s.as_ref()).or_default().push((aa.ann.ap.0.as_ref(), &aa.ann.av));
                    }
                }
                _ => {}
            }
        }
        ix
    }
}

/// The order a set of entities is held in: by bucket of the table they end in,
/// then by bucket of the larger table they were gathered from, then as they were
/// gathered — kind by kind, each kind in its natural order.
fn hash_set_order(entities: &mut [(Kind, String)], held: usize, gathered: usize) {
    let bucket = |h: i32, cap: usize| {
        let h = h as u32;
        (h ^ (h >> 16)) & (cap as u32 - 1)
    };
    let (held, gathered) = (java_hashset_capacity(held), java_hashset_capacity(gathered));
    entities.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| iri_cmp(&a.1, &b.1)));
    entities.sort_by_cached_key(|(kind, iri)| {
        let h = kind.hash(iri);
        (bucket(h, held), bucket(h, gathered))
    });
}

/// Distinct strings in the order a hash set of them is held in.
fn string_set_order(values: Vec<String>) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::with_capacity(values.len());
    for v in values {
        if !distinct.contains(&v) {
            distinct.push(v);
        }
    }
    let cap = java_hashset_capacity(distinct.len()) as u32;
    distinct.sort_by_key(|s| {
        let h = java_string_hash(s) as u32;
        (h ^ (h >> 16)) & (cap - 1)
    });
    distinct
}

impl<'m> Ctx<'m> {
    fn new(model: &'m Model) -> Result<Ctx<'m>> {
        let order = model.natural_order();
        // CURIEs are written and read with the command line's context.
        let terms = model.context.entries();
        let mut curies = terms.clone();
        // Longest namespace first; namespaces of one length in the order bound.
        curies.sort_by_key(|(_, ns)| std::cmp::Reverse(ns.encode_utf16().count()));
        let prefix_manager = ShortForms::new(&terms);
        let builtin_names = ShortForms::new(&[]);

        let labels = held_labels(model);

        // The root document's labels by label; of two entities with one label,
        // the one held later wins.
        let mut root_labels: Vec<(i32, &str, &str)> = Vec::new();
        let mut root_label_count = 0usize;
        for ac in model.ont.iter().filter(|ac| !model.imported_components.contains(*ac)) {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                    root_label_count += 1;
                    if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(l)) = (&aa.subject, &aa.ann.av) {
                        let subject: &str = s.as_ref();
                        let h = annotation_assertion_hash(subject, RDFS_LABEL, &aa.ann.av, &ac.ann, order);
                        root_labels.push((h, l.literal().as_str(), subject));
                    }
                }
            }
        }
        let hashes: Vec<i32> = root_labels.iter().map(|l| l.0).collect();
        let mut label_iris: HashMap<&str, &str> = HashMap::new();
        for i in crate::owlapi_hash::hashset_order_of(&hashes, root_label_count) {
            label_iris.insert(root_labels[i].1, root_labels[i].2);
        }

        // Property names over the whole closure: each property's short form,
        // then each of its labels.
        let closure = crate::io::entities::signature(model);
        let mut ordered: Vec<(Kind, String)> = closure.iter().cloned().collect();
        let n = ordered.len();
        hash_set_order(&mut ordered, n, n);
        let mut property_names: HashMap<Kind, HashMap<String, String>> = HashMap::new();
        let mut closure_labels: HashMap<&str, Vec<&str>> = HashMap::new();
        for ac in model.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(l), true) =
                    (&aa.subject, &aa.ann.av, aa.ann.ap.0.as_ref() == RDFS_LABEL)
                {
                    closure_labels.entry(s.as_ref()).or_default().push(l.literal().as_str());
                }
            }
        }
        for (kind, iri) in &ordered {
            if !matches!(kind, Kind::AnnotationProperty | Kind::DataProperty | Kind::ObjectProperty) {
                continue;
            }
            let names = property_names.entry(*kind).or_default();
            names.insert(simple_short_form(iri), iri.clone());
            for l in closure_labels.get(iri.as_str()).into_iter().flatten() {
                names.insert(l.to_string(), iri.clone());
            }
        }

        let root = crate::io::entities::root_signature(model);
        let signature_size = root.len();
        let root_entities: Vec<(Kind, String)> = root.iter().cloned().collect();
        Ok(Ctx {
            order,
            curies,
            prefix_manager,
            builtin_names,
            terms,
            labels,
            label_iris,
            property_names,
            signature: root.into_iter().collect(),
            signature_size,
            root_entities,
            ix: Index::build(model),
        })
    }

    // --- naming --------------------------------------------------------------

    /// An IRI as a CURIE: the longest bound namespace that starts it, replaced
    /// wherever it occurs by its prefix; the IRI itself when none does.
    fn curie(&self, iri: &str) -> String {
        for (prefix, ns) in &self.curies {
            if iri.starts_with(ns.as_str()) {
                return iri.replace(ns.as_str(), &format!("{prefix}:"));
            }
        }
        iri.to_string()
    }

    /// How `provider` names the entity at `iri`; `quoting` quotes a name that
    /// holds a space.
    fn short_form(&self, provider: Provider, iri: &str, quoting: bool) -> String {
        match provider {
            Provider::Curie => self.curie(iri),
            Provider::Iri => iri.to_string(),
            Provider::Name => {
                let name = match self.labels.get(iri) {
                    Some(HeldLabel::Literal(l)) => l.to_string(),
                    Some(HeldLabel::Iri(i)) => self.prefix_manager.prefixed_or_quoted(i),
                    None => self.curie(iri),
                };
                if quoting && name.contains(' ') {
                    format!("'{name}'")
                } else {
                    name
                }
            }
            Provider::Label => match self.labels.get(iri) {
                Some(HeldLabel::Literal(l)) => l.to_string(),
                _ => String::new(),
            },
        }
    }

    /// One object rendered by `provider`: an anonymous one names its entities
    /// quoted, and the whole is on one line with runs of spaces collapsed.
    fn render(&self, object: Object<'_>, anonymous: bool, provider: Provider, html: bool) -> String {
        let names = EntityNames { ctx: self, provider, quoting: anonymous && provider == Provider::Name, html };
        let text = object_text(object, self.order, &names, Layout::OneLine);
        collapse(&text)
    }

    /// An object's display and sort renderings.
    fn rendered(&self, object: Object<'_>, anonymous: bool, provider: Provider, html: bool) -> (String, String) {
        let sort = self.render(object, anonymous, provider, false);
        let display = if html { self.render(object, anonymous, provider, true) } else { sort.clone() };
        (display, sort)
    }

    // --- columns -------------------------------------------------------------

    /// The IRI a header names: a keyword's, else the entity of the root
    /// document with that label, else the header as a CURIE or IRI.
    fn header_iri(&self, name: &str) -> Option<String> {
        if let Some((_, iri)) = KEYWORDS.iter().find(|(k, _)| *k == name) {
            return Some(iri.to_string());
        }
        if let Some(iri) = self.label_iris.get(name) {
            return Some(iri.to_string());
        }
        self.expand(name)
    }

    /// A name as an IRI: a bound prefix with its local part, a prefix's own name,
    /// or an absolute IRI with a known scheme. Anything else names no IRI.
    fn expand(&self, name: &str) -> Option<String> {
        let term = |t: &str| self.terms.iter().rev().find(|(p, _)| p == t).map(|(_, ns)| ns.clone());
        let expanded = if let Some(ns) = term(name) {
            ns
        } else if let Some((prefix, local)) = name.split_once(':') {
            match term(prefix) {
                Some(ns) if prefix != "_" && !local.starts_with("//") => format!("{ns}{local}"),
                _ => name.to_string(),
            }
        } else {
            return None;
        };
        valid_iri(&expanded).then_some(expanded)
    }

    /// A property of `kind` by the name a header gives it: its short form or
    /// label, as written or with one pair of quotes taken off.
    fn property_by_name(&self, kind: Kind, name: &str) -> Option<String> {
        let names = self.property_names.get(&kind)?;
        if let Some(iri) = names.get(name) {
            return Some(iri.clone());
        }
        let unquoted = strip_one(java_trim(name), '\'');
        if let Some(iri) = names.get(&unquoted) {
            return Some(iri.clone());
        }
        names.get(&strip_one(java_trim(&unquoted), '"')).cloned()
    }

    fn columns(&self, headers: &[String], entity_format: &str, entity_select: &str, sorts: &[String]) -> Result<Vec<Column>> {
        let mut columns = Vec::with_capacity(headers.len());
        for c in headers {
            let mut format: Option<String> = None;
            let mut select: Option<String> = None;
            let mut name = c.clone();
            if let Some((base, tags)) = split_tag(c) {
                name = base.to_string();
                for tag in java_split(tags, ' ') {
                    match tag.to_uppercase().as_str() {
                        "NAMED" | "ANON" | "ANONYMOUS" | "ANY" => {
                            if select.is_some() {
                                bail!("export: column header '{c}' contains more than one entity selection tag");
                            }
                            select = Some(tag.clone());
                        }
                        "NAME" | "LABEL" | "ID" | "IRI" => {
                            if format.is_some() {
                                bail!("export: column header '{c}' contains more than one entity format tag");
                            }
                            format = Some(tag.clone());
                        }
                        _ => bail!("export: column '{c}' contains an unknown rendering tag: {tag}"),
                    }
                }
            }
            let mut format = format.unwrap_or_else(|| entity_format.to_string());
            let mut select = select.unwrap_or_else(|| entity_select.to_string());
            let iri = self.header_iri(&name);
            for (keyword, f) in [("ID", "ID"), ("CURIE", "ID"), ("IRI", "IRI"), ("LABEL", "LABEL")] {
                if c.eq_ignore_ascii_case(keyword) {
                    format = f.to_string();
                    select = "NAMED".to_string();
                    break;
                }
            }
            let mut annotation: Option<String> = None;
            if iri.as_deref() == Some(RDFS_LABEL) {
                format = "LABEL".to_string();
                select = "NAMED".to_string();
                annotation = Some(RDFS_LABEL.to_string());
            }
            match &iri {
                Some(i) if annotation.is_none() && self.signature.contains(&(Kind::AnnotationProperty, i.clone())) => {
                    annotation = Some(i.clone());
                }
                _ => {
                    annotation = self
                        .property_by_name(Kind::AnnotationProperty, &name)
                        .or_else(|| self.expand(&name));
                }
            }
            let mut data = None;
            let mut object = None;
            if annotation.is_none() {
                data = match &iri {
                    Some(i) if self.signature.contains(&(Kind::DataProperty, i.clone())) => Some(i.clone()),
                    _ => self.property_by_name(Kind::DataProperty, &name),
                };
                if data.is_none() {
                    object = match &iri {
                        Some(i) if self.signature.contains(&(Kind::ObjectProperty, i.clone())) => Some(i.clone()),
                        _ => self.property_by_name(Kind::ObjectProperty, &name),
                    };
                }
            }
            let provider = Provider::of(&format).with_context(|| {
                format!("export: column '{c}' contains an unknown rendering tag: {format}")
            })?;
            let source = if iri.as_deref() == Some(RDF_TYPE) {
                Source::Iri(iri)
            } else if let Some(a) = annotation {
                Source::Annotation(a)
            } else if let Some(d) = data {
                Source::Data(d)
            } else if let Some(o) = object {
                Source::Object(o)
            } else {
                Source::Iri(iri)
            };
            let (named, anonymous) = match select.to_lowercase().as_str() {
                "named" => (true, false),
                "anon" | "anonymous" => (false, true),
                _ => (true, true),
            };
            let mut sort = None;
            for s in sorts {
                if c.eq_ignore_ascii_case(s) {
                    sort = Some((sorts.iter().position(|x| x == c).map_or(-1, |i| i as i64), false));
                    break;
                }
                let reverse = format!("^{c}");
                if s.eq_ignore_ascii_case(&reverse) {
                    sort = Some((sorts.iter().position(|x| *x == reverse).map_or(-1, |i| i as i64), true));
                    break;
                }
            }
            columns.push(Column { name, display: c.clone(), source, provider, named, anonymous, sort });
        }
        Ok(columns)
    }

    // --- rows ----------------------------------------------------------------

    /// The entities that get a row, in the order they are held before sorting.
    fn entities(&self, include: &str) -> Result<Vec<(Kind, String)>> {
        let kinds: Vec<&str> = if include.contains(' ') {
            include.split(' ').collect()
        } else if include.contains(',') {
            include.split(',').collect()
        } else if include.contains('\t') {
            include.split('\t').collect()
        } else {
            vec![include]
        };
        let (mut classes, mut properties, mut individuals) = (false, false, false);
        for k in kinds {
            match java_trim(&k.to_lowercase()) {
                "classes" => classes = true,
                "properties" => properties = true,
                "individuals" => individuals = true,
                _ => {}
            }
        }
        if !classes && !properties && !individuals {
            bail!("export: --include '{include}' names no kind of entity: give classes, properties or individuals");
        }
        let mut entities: Vec<(Kind, String)> = self
            .root_entities
            .iter()
            .filter(|(kind, iri)| {
                let top_or_bottom = match kind {
                    Kind::Class | Kind::ObjectProperty | Kind::DataProperty => {
                        crate::io::entities::is_builtin(*kind, iri)
                    }
                    _ => false,
                };
                !top_or_bottom
                    && match kind {
                        Kind::Class => classes,
                        Kind::ObjectProperty | Kind::DataProperty | Kind::AnnotationProperty => properties,
                        Kind::NamedIndividual => individuals,
                        Kind::Datatype => false,
                    }
            })
            .cloned()
            .collect();
        let held = entities.len();
        hash_set_order(&mut entities, held, self.signature_size);
        Ok(entities)
    }

    fn row(&self, columns: &[Column], kind: Kind, iri: &str, html: bool) -> Result<Row> {
        let mut cells: HashMap<String, Cell> = HashMap::new();
        for col in columns {
            let name = match &col.source {
                Source::Annotation(ap) if ap == RDFS_LABEL => "LABEL",
                _ => col.name.as_str(),
            };
            let cell = match name.to_uppercase().as_str() {
                "IRI" => Some(if html {
                    Cell::single(format!("<a href=\"{iri}\">{}</a>", iri.replace('&', "&amp;")), iri.to_string())
                } else {
                    Cell::single(iri.to_string(), iri.to_string())
                }),
                "ID" | "CURIE" => {
                    let (display, sort) = self.rendered(Object::Entity(iri), false, col.provider, html);
                    Some(Cell::single(display, sort))
                }
                "LABEL" => {
                    let label = self.short_form(col.provider, iri, false);
                    Some(Cell::single(label.clone(), label))
                }
                "SYNONYMS" => Some(Cell::of_strings(self.synonyms(iri))),
                "SUBCLASSES" => (kind == Kind::Class).then(|| {
                    let subs = self.ix.subs.get(iri).cloned().unwrap_or_default();
                    self.object_cell(subs.into_iter().map(ObjectRef::Ce), col, html)
                }),
                _ => match &col.source {
                    Source::Annotation(ap) => Some(self.annotation_cell(iri, ap, col, html)),
                    Source::Data(dp) => Some(self.data_cell(kind, iri, dp, col, html)?),
                    Source::Object(op) => Some(self.object_property_cell(kind, iri, op, col, html)?),
                    Source::Iri(target) => self.keyword_cell(kind, iri, target.as_deref(), col, html, name)?,
                },
            };
            if let Some(cell) = cell {
                cells.insert(col.display.clone(), cell);
            }
        }
        Ok(Row { kind, iri: iri.to_string(), cells })
    }

    fn synonyms(&self, iri: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (p, v) in self.ix.annotations.get(iri).into_iter().flatten() {
            if SYNONYM_PROPERTIES.contains(p) {
                if let AnnotationValue::Literal(l) = v {
                    out.push(l.literal().to_string());
                }
            }
        }
        out
    }

    /// A cell of objects: the named ones and the anonymous ones the column
    /// holds, each rendered.
    fn object_cell<'a>(&self, objects: impl Iterator<Item = ObjectRef<'a>>, col: &Column, html: bool) -> Cell {
        let mut pairs = Vec::new();
        for o in objects {
            let anonymous = o.is_anonymous();
            if (anonymous && col.anonymous) || (!anonymous && col.named) {
                pairs.push(self.rendered(o.object(), anonymous, col.provider, html));
            }
        }
        Cell::values(pairs)
    }

    /// An annotation property column: each value of the property on the entity —
    /// a literal's text, an IRI's entities rendered, or the IRI when it names
    /// none.
    fn annotation_cell(&self, iri: &str, ap: &str, col: &Column, html: bool) -> Cell {
        let mut pairs = Vec::new();
        for (p, v) in self.ix.annotations.get(iri).into_iter().flatten() {
            if *p != ap {
                continue;
            }
            match v {
                AnnotationValue::Literal(l) => {
                    pairs.push((l.literal().to_string(), l.literal().to_string()));
                }
                AnnotationValue::IRI(value) => {
                    let value: &str = value.as_ref();
                    let kinds: Vec<Kind> = [
                        Kind::Class,
                        Kind::ObjectProperty,
                        Kind::DataProperty,
                        Kind::NamedIndividual,
                        Kind::AnnotationProperty,
                        Kind::Datatype,
                    ]
                    .into_iter()
                    .filter(|k| self.signature.contains(&(*k, value.to_string())))
                    .collect();
                    if kinds.is_empty() {
                        pairs.push((value.to_string(), value.to_string()));
                    }
                    for _ in kinds {
                        pairs.push(self.rendered(Object::Entity(value), false, col.provider, html));
                    }
                }
                AnnotationValue::AnonymousIndividual(_) => {}
            }
        }
        Cell::values(pairs)
    }

    /// A data property column: an individual's values of it, or the fillers of
    /// the restrictions on it among a class's anonymous superclasses and
    /// equivalents.
    fn data_cell(&self, kind: Kind, iri: &str, dp: &str, col: &Column, html: bool) -> Result<Cell> {
        match kind {
            Kind::NamedIndividual => {
                let values = self.ix.data_values.get(&(iri, dp)).cloned().unwrap_or_default();
                Ok(Cell::of_strings(values.into_iter().map(|l| self.literal_text(l)).collect()))
            }
            Kind::Class => self.filler_cell(iri, col, html, |ce| match ce {
                CE::DataSomeValuesFrom { dp: p, dr } | CE::DataAllValuesFrom { dp: p, dr } if p.0.as_ref() == dp => {
                    Some((Filler::Dr(dr), None))
                }
                CE::DataMinCardinality { n, dp: p, dr }
                | CE::DataMaxCardinality { n, dp: p, dr }
                | CE::DataExactCardinality { n, dp: p, dr }
                    if p.0.as_ref() == dp =>
                {
                    Some((Filler::Dr(dr), Some(*n)))
                }
                _ => None,
            }),
            _ => Ok(Cell::values(Vec::new())),
        }
    }

    /// An object property column: an individual's named values of it, or the
    /// fillers of the restrictions on it among a class's anonymous superclasses
    /// and equivalents.
    fn object_property_cell(&self, kind: Kind, iri: &str, op: &str, col: &Column, html: bool) -> Result<Cell> {
        match kind {
            Kind::NamedIndividual => {
                let values = self.ix.object_values.get(&(iri, op)).cloned().unwrap_or_default();
                let mut pairs = Vec::new();
                for v in values {
                    if let Individual::Named(n) = v {
                        pairs.push(self.rendered(Object::Entity(n.0.as_ref()), false, col.provider, html));
                    }
                }
                Ok(Cell::values(pairs))
            }
            Kind::Class => self.filler_cell(iri, col, html, |ce| match ce {
                CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(p), bce }
                | CE::ObjectAllValuesFrom { ope: OPE::ObjectProperty(p), bce }
                    if p.0.as_ref() == op =>
                {
                    Some((Filler::Ce(bce), None))
                }
                CE::ObjectMinCardinality { n, ope: OPE::ObjectProperty(p), bce }
                | CE::ObjectMaxCardinality { n, ope: OPE::ObjectProperty(p), bce }
                | CE::ObjectExactCardinality { n, ope: OPE::ObjectProperty(p), bce }
                    if p.0.as_ref() == op =>
                {
                    Some((Filler::Ce(bce), Some(*n)))
                }
                _ => None,
            }),
            _ => Ok(Cell::values(Vec::new())),
        }
    }

    /// The fillers `filler` finds among the conjuncts of each anonymous
    /// superclass and equivalent of the class at `iri`: within one expression
    /// each rendering once, in the order a set of them is held.
    fn filler_cell<'a>(
        &self,
        iri: &str,
        col: &Column,
        html: bool,
        filler: impl Fn(&'a CE<RcStr>) -> Option<(Filler<'a>, Option<u32>)>,
    ) -> Result<Cell>
    where
        'm: 'a,
    {
        let equivalents = self.equivalents(iri);
        let expressions = self
            .ix
            .supers
            .get(iri)
            .into_iter()
            .flatten()
            .chain(equivalents.iter())
            .filter(|ce| !matches!(ce, CE::Class(_)));
        let (mut displays, mut sorts) = (Vec::new(), Vec::new());
        for ce in expressions {
            let ce: &'a CE<RcStr> = ce;
            let (mut display, mut sort) = (Vec::new(), Vec::new());
            for conjunct in conjuncts(ce, self.order) {
                let Some((f, n)) = filler(conjunct) else { continue };
                let anonymous = f.is_anonymous();
                if !((anonymous && col.anonymous) || (!anonymous && col.named)) {
                    continue;
                }
                let text = |html: bool| {
                    let r = self.render(f.object(), anonymous, col.provider, html);
                    match (anonymous, n) {
                        (false, Some(n)) => format!("{n} {r}"),
                        (false, None) => r,
                        (true, Some(n)) => format!("{n} ({r})"),
                        (true, None) => format!("({r})"),
                    }
                };
                sort.push(text(false));
                display.push(if html { text(true) } else { text(false) });
            }
            displays.extend(string_set_order(display));
            sorts.extend(string_set_order(sort));
        }
        if displays.len() != sorts.len() {
            bail!("export: the restrictions on {iri} render to different numbers of distinct values as links and as text");
        }
        Ok(Cell::values(displays.into_iter().zip(sorts).collect()))
    }

    /// The class's equivalents: every member of an equivalence it is in, itself
    /// aside.
    fn equivalents(&self, iri: &str) -> Vec<&'m CE<RcStr>> {
        self.ix
            .equivalents
            .get(iri)
            .into_iter()
            .flatten()
            .copied()
            .filter(|ce| !matches!(ce, CE::Class(c) if c.0.as_ref() == iri))
            .collect()
    }

    /// A keyword column's cell, or none where the keyword does not apply to the
    /// entity's kind.
    fn keyword_cell(
        &self,
        kind: Kind,
        iri: &str,
        target: Option<&str>,
        col: &Column,
        html: bool,
        name: &str,
    ) -> Result<Option<Cell>> {
        let cell = |objects: Vec<ObjectRef<'m>>| Some(self.object_cell(objects.into_iter(), col, html));
        Ok(match target.unwrap_or("") {
            RDFS_SUBCLASS_OF => match kind {
                Kind::Class => {
                    let mut supers = self.ix.supers.get(iri).cloned().unwrap_or_default();
                    if let Some(i) = supers.iter().position(|ce| matches!(ce, CE::Class(c) if c.0.as_ref() == OWL_THING)) {
                        supers.remove(i);
                    }
                    cell(supers.into_iter().map(ObjectRef::Ce).collect())
                }
                _ => None,
            },
            RDFS_SUBPROPERTY_OF => match kind {
                Kind::AnnotationProperty => cell(
                    self.ix.super_annotation_properties.get(iri).into_iter().flatten().map(|p| ObjectRef::Entity(p)).collect(),
                ),
                Kind::DataProperty => cell(
                    self.ix.super_data_properties.get(iri).into_iter().flatten().map(|p| ObjectRef::Entity(p)).collect(),
                ),
                Kind::ObjectProperty => cell(
                    self.ix.super_object_properties.get(iri).into_iter().flatten().map(|p| ObjectRef::Ope(p)).collect(),
                ),
                _ => None,
            },
            OWL_EQUIVALENT_CLASS => match kind {
                Kind::Class => cell(self.equivalents(iri).into_iter().map(ObjectRef::Ce).collect()),
                _ => None,
            },
            OWL_EQUIVALENT_PROPERTY => match kind {
                Kind::DataProperty => cell(
                    self.ix
                        .equivalent_data_properties
                        .get(iri)
                        .into_iter()
                        .flatten()
                        .filter(|p| **p != iri)
                        .map(|p| ObjectRef::Entity(p))
                        .collect(),
                ),
                Kind::ObjectProperty => cell(
                    self.ix
                        .equivalent_object_properties
                        .get(iri)
                        .into_iter()
                        .flatten()
                        .filter(|p| !matches!(p, OPE::ObjectProperty(x) if x.0.as_ref() == iri))
                        .map(|p| ObjectRef::Ope(p))
                        .collect(),
                ),
                _ => None,
            },
            OWL_DISJOINT_WITH => match kind {
                Kind::Class => cell(
                    self.ix
                        .disjoints
                        .get(iri)
                        .into_iter()
                        .flatten()
                        .filter(|ce| !matches!(ce, CE::Class(c) if c.0.as_ref() == iri))
                        .map(|ce| ObjectRef::Ce(ce))
                        .collect(),
                ),
                Kind::DataProperty => cell(
                    self.ix
                        .disjoint_data_properties
                        .get(iri)
                        .into_iter()
                        .flatten()
                        .filter(|p| **p != iri)
                        .map(|p| ObjectRef::Entity(p))
                        .collect(),
                ),
                Kind::ObjectProperty => cell(
                    self.ix
                        .disjoint_object_properties
                        .get(iri)
                        .into_iter()
                        .flatten()
                        .filter(|p| !matches!(p, OPE::ObjectProperty(x) if x.0.as_ref() == iri))
                        .map(|p| ObjectRef::Ope(p))
                        .collect(),
                ),
                _ => None,
            },
            RDF_TYPE => {
                let types = match kind {
                    Kind::NamedIndividual => self.ix.types.get(iri).cloned().unwrap_or_default(),
                    _ => Vec::new(),
                };
                if types.is_empty() {
                    let value = match col.provider {
                        Provider::Curie => self.curie(&kind_iri(kind)),
                        Provider::Name | Provider::Label => kind_name(kind).to_string(),
                        Provider::Iri => kind_iri(kind),
                    };
                    Some(Cell::single(value.clone(), value))
                } else {
                    cell(types.into_iter().map(ObjectRef::Ce).collect())
                }
            }
            RDFS_DOMAIN => match kind {
                Kind::ObjectProperty => {
                    cell(self.ix.object_domains.get(iri).into_iter().flatten().map(|c| ObjectRef::Ce(c)).collect())
                }
                Kind::DataProperty => {
                    cell(self.ix.data_domains.get(iri).into_iter().flatten().map(|c| ObjectRef::Ce(c)).collect())
                }
                Kind::AnnotationProperty => {
                    cell(self.ix.annotation_domains.get(iri).into_iter().flatten().map(|i| ObjectRef::Iri(i)).collect())
                }
                _ => None,
            },
            RDFS_RANGE => match kind {
                Kind::ObjectProperty => {
                    cell(self.ix.object_ranges.get(iri).into_iter().flatten().map(|c| ObjectRef::Ce(c)).collect())
                }
                Kind::DataProperty => {
                    cell(self.ix.data_ranges.get(iri).into_iter().flatten().map(|d| ObjectRef::Dr(d)).collect())
                }
                Kind::AnnotationProperty => {
                    cell(self.ix.annotation_ranges.get(iri).into_iter().flatten().map(|i| ObjectRef::Iri(i)).collect())
                }
                _ => None,
            },
            _ => bail!("export: unable to find property for column header '{name}'"),
        })
    }

    /// A literal as an individual's data value: quoted, with its language tag,
    /// or with its datatype when it is not a plain literal.
    fn literal_text(&self, l: &Literal<RcStr>) -> String {
        let text = l.literal().replace('\\', "\\\\").replace('"', "\\\"");
        let datatype = self.order.literal_datatype(l);
        match l {
            Literal::Language { lang, .. } if !lang.is_empty() => format!("\"{text}\"@{lang}"),
            _ if datatype == RDF_PLAIN_LITERAL => format!("\"{text}\""),
            _ => format!("\"{text}\"^^{}", self.builtin_names.prefixed_or_quoted(datatype)),
        }
    }
}

/// A restriction's filler.
#[derive(Clone, Copy)]
enum Filler<'a> {
    Ce(&'a CE<RcStr>),
    Dr(&'a DR<RcStr>),
}

impl<'a> Filler<'a> {
    fn is_anonymous(&self) -> bool {
        !matches!(self, Filler::Ce(CE::Class(_)) | Filler::Dr(DR::Datatype(_)))
    }

    fn object(&self) -> Object<'a> {
        match *self {
            Filler::Ce(ce) => Object::Ce(ce),
            Filler::Dr(dr) => Object::Dr(dr),
        }
    }
}

/// An object a cell holds.
#[derive(Clone, Copy)]
enum ObjectRef<'a> {
    Ce(&'a CE<RcStr>),
    Ope(&'a OPE<RcStr>),
    Dr(&'a DR<RcStr>),
    /// A named property, by IRI.
    Entity(&'a str),
    /// An IRI that names no entity.
    Iri(&'a str),
}

impl<'a> ObjectRef<'a> {
    fn is_anonymous(&self) -> bool {
        match self {
            ObjectRef::Ce(ce) => !matches!(ce, CE::Class(_)),
            ObjectRef::Ope(ope) => matches!(ope, OPE::InverseObjectProperty(_)),
            ObjectRef::Dr(dr) => !matches!(dr, DR::Datatype(_)),
            ObjectRef::Entity(_) | ObjectRef::Iri(_) => false,
        }
    }

    fn object(&self) -> Object<'a> {
        match *self {
            ObjectRef::Ce(ce) => Object::Ce(ce),
            ObjectRef::Ope(ope) => Object::Ope(ope),
            ObjectRef::Dr(dr) => Object::Dr(dr),
            ObjectRef::Entity(iri) => Object::Entity(iri),
            ObjectRef::Iri(iri) => Object::Iri(iri),
        }
    }
}

/// The conjuncts of an expression: an intersection's operands, nested
/// intersections flattened, in the order a set of them is held, each hashed in
/// the document's natural order; anything else alone.
fn conjuncts(ce: &CE<RcStr>, order: NaturalOrder) -> Vec<&CE<RcStr>> {
    fn gather<'a>(ce: &'a CE<RcStr>, out: &mut Vec<&'a CE<RcStr>>) {
        match ce {
            CE::ObjectIntersectionOf(ops) => {
                for op in ops {
                    gather(op, out);
                }
            }
            _ => {
                if !out.contains(&ce) {
                    out.push(ce);
                }
            }
        }
    }
    let mut out = Vec::new();
    gather(ce, &mut out);
    let cap = java_hashset_capacity(out.len()) as u32;
    out.sort_by_key(|c| {
        let h = crate::owlapi_hash::ce_hash(c, order) as u32;
        (h ^ (h >> 16)) & (cap - 1)
    });
    out
}

/// How an entity is named inside an object rendered for a column.
struct EntityNames<'c, 'm> {
    ctx: &'c Ctx<'m>,
    provider: Provider,
    quoting: bool,
    html: bool,
}

impl Names for EntityNames<'_, '_> {
    fn entity(&self, iri: &str) -> String {
        let name = self.ctx.short_form(self.provider, iri, self.quoting);
        if self.html {
            format!("<a href=\"{iri}\">{}</a>", name.replace('&', "&amp;").replace('<', "&lt;"))
        } else {
            name
        }
    }
}

/// Line breaks removed and runs of spaces made one.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        match c {
            '\n' => {}
            ' ' => {
                if !space {
                    out.push(' ');
                }
                space = true;
            }
            _ => {
                out.push(c);
                space = false;
            }
        }
    }
    out
}

/// A header's trailing tag: `SubClass Of [ID ANON]` → (`SubClass Of`, `ID
/// ANON`).
fn split_tag(header: &str) -> Option<(&str, &str)> {
    let open = header.rfind(" [")?;
    let base = &header[..open];
    let tags = header[open + 2..].strip_suffix(']')?;
    if base.is_empty() || base.contains('\n') || tags.is_empty() || tags.contains(['[', ']']) {
        return None;
    }
    Some((base, tags))
}

/// `s` with one `q` taken off each end it stands at.
fn strip_one(s: &str, q: char) -> String {
    let s = s.strip_prefix(q).unwrap_or(s);
    s.strip_suffix(q).unwrap_or(s).to_string()
}

/// An IRI's simple short form: the part after its namespace, else the part
/// after its last `/`, else the IRI in angle brackets.
fn simple_short_form(iri: &str) -> String {
    let (ns, local) = crate::owlapi_hash::iri_split(iri);
    if !local.is_empty() {
        return local.to_string();
    }
    match ns.rfind('/') {
        Some(i) if i + 1 != ns.len() => ns[i + 1..].to_string(),
        _ => format!("<{iri}>"),
    }
}

/// Whether an expanded header names an IRI: a URN of the form RFC 2141 gives,
/// or a URL with a scheme the platform reads.
fn valid_iri(iri: &str) -> bool {
    if let Some(rest) = iri.strip_prefix("urn:") {
        let Some((nid, nss)) = rest.split_once(':') else { return false };
        let nid_ok = nid.len() <= 32
            && nid.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && nid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        let mut chars = nss.chars().peekable();
        let mut any = false;
        while let Some(c) = chars.next() {
            any = true;
            if c == '%' {
                let hex = |c: Option<char>| c.is_some_and(|c| c.is_ascii_hexdigit());
                if !(hex(chars.next()) && hex(chars.next())) {
                    return false;
                }
            } else if !(c.is_ascii_alphanumeric() || "()+,-.:=@;$_!*'".contains(c)) {
                return false;
            }
        }
        return nid_ok && any;
    }
    let Some((scheme, _)) = iri.split_once(':') else { return false };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ftp" | "file" | "jar" | "mailto" | "jrt" | "jmod"
    )
}

/// The IRI of an entity kind's class.
fn kind_iri(kind: Kind) -> String {
    match kind {
        Kind::Class => format!("{OWL}Class"),
        Kind::ObjectProperty => format!("{OWL}ObjectProperty"),
        Kind::DataProperty => format!("{OWL}DatatypeProperty"),
        Kind::AnnotationProperty => format!("{OWL}AnnotationProperty"),
        Kind::NamedIndividual => format!("{OWL}NamedIndividual"),
        Kind::Datatype => format!("{RDFS}Datatype"),
    }
}

/// An entity kind's name.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Class => "Class",
        Kind::ObjectProperty => "Object property",
        Kind::DataProperty => "Data property",
        Kind::AnnotationProperty => "Annotation property",
        Kind::NamedIndividual => "Named individual",
        Kind::Datatype => "Datatype",
    }
}

// === Sorting and writing ==================================================

/// Sort the rows on each sort column in turn, so the last one listed orders
/// them and the earlier ones break its ties. Empty values sort last (first in
/// reverse).
fn sort_rows(columns: &[Column], rows: &mut [Row]) -> Result<()> {
    let mut by_position: BTreeMap<i64, usize> = BTreeMap::new();
    let mut last = 0i64;
    for (i, c) in columns.iter().enumerate() {
        let position = c.sort.map_or(-1, |s| s.0);
        by_position.insert(position, i);
        last = last.max(position);
    }
    for position in 0..=last {
        let Some(&i) = by_position.get(&position) else {
            bail!(
                "export: sort position {} names no column of the header; a sort column is written as its header \
                 is, case and tag included, and every position up to the last must name one",
                position + 1
            );
        };
        let col = &columns[i];
        if rows.len() > 1 {
            if let Some(row) = rows.iter().find(|r| !r.cells.contains_key(&col.display)) {
                bail!(
                    "export: cannot sort on column '{}': the {} {} has no value in it",
                    col.display,
                    kind_name(row.kind).to_lowercase(),
                    row.iri
                );
            }
        }
        let reverse = col.sort.is_some_and(|s| s.1);
        rows.sort_by(|a, b| {
            let (x, y) = (&a.cells[&col.display].sort, &b.cells[&col.display].sort);
            let o = match (java_trim(x).is_empty(), java_trim(y).is_empty()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => str_cmp(x, y),
            };
            if reverse {
                o.reverse()
            } else {
                o
            }
        });
    }
    Ok(())
}

/// The table as rows of text: the header, then each row's cells joined.
fn grid(columns: &[Column], rows: &[Row], split: &str) -> Vec<Vec<String>> {
    let mut out = Vec::with_capacity(rows.len() + 1);
    out.push(columns.iter().map(|c| c.display.clone()).collect());
    for row in rows {
        out.push(
            columns
                .iter()
                .map(|c| row.cells.get(&c.display).map(|cell| cell.joined(split)).unwrap_or_default())
                .collect(),
        );
    }
    out
}

fn delimited(columns: &[Column], rows: &[Row], split: &str, delim: char) -> String {
    grid(columns, rows, split).iter().map(|r| crate::table::record(r, delim)).collect()
}

fn html_table(columns: &[Column], rows: &[Row], split: &str, standalone: bool) -> String {
    let mut out = String::new();
    if standalone {
        out.push_str(&format!("<head>\n  <link rel=\"stylesheet\" href=\"{BOOTSTRAP_CSS}\">\n</head>\n<body>\n"));
    }
    out.push_str("<table class=\"table table-bordered table-striped\">\n<thead class=\"bg-dark text-white header-row\">\n<tr>\n");
    for c in columns {
        out.push_str(&format!("  <th>{}</th>\n", c.display));
    }
    out.push_str("</tr>\n</thead>\n");
    for row in grid(columns, rows, split).iter().skip(1) {
        out.push_str("\t<tr>\n");
        for value in row {
            out.push_str(&format!("\t\t<td>{value}</td>\n"));
        }
        out.push_str("\t</tr>\n");
    }
    out.push_str("</table>\n");
    if standalone {
        out.push_str("</body>\n");
    }
    out
}

/// The table as a JSON array of row objects, keyed by header: an `ID`, `CURIE`
/// or `IRI` column as its one value, any other as the array of its values, and
/// a cell without values left out.
fn json(columns: &[Column], rows: &[Row]) -> String {
    if rows.is_empty() {
        return "[]".to_string();
    }
    let mut out = String::from("[");
    for (r, row) in rows.iter().enumerate() {
        out.push_str(if r == 0 { "\n  {" } else { ",\n  {" });
        let mut fields: Vec<(&str, Cow<str>)> = Vec::new();
        for c in columns {
            let Some(cell) = row.cells.get(&c.display) else { continue };
            let value = if matches!(c.display.to_uppercase().as_str(), "CURIE" | "ID" | "IRI") {
                if cell.display.len() != 1 {
                    continue;
                }
                json_string(&cell.display[0]).into()
            } else {
                if cell.display.is_empty() {
                    continue;
                }
                let items: Vec<String> = cell.display.iter().map(|v| format!("      {}", json_string(v))).collect();
                format!("[\n{}\n    ]", items.join(",\n")).into()
            };
            match fields.iter_mut().find(|(k, _)| *k == c.display) {
                Some(slot) => slot.1 = value,
                None => fields.push((&c.display, value)),
            }
        }
        for (i, (k, v)) in fields.iter().enumerate() {
            out.push_str(if i == 0 { "\n    " } else { ",\n    " });
            out.push_str(&format!("{}: {v}", json_string(k)));
        }
        out.push_str(if fields.is_empty() { "}" } else { "\n  }" });
    }
    out.push_str("\n]");
    out
}

/// A JSON string: quotes and backslashes escaped, the short escapes for
/// `\b \f \n \r \t`, `\uXXXX` for the other control characters and for the line
/// and paragraph separators.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{c}' => out.push_str("\\f"),
            '\u{0}'..='\u{1f}' | '\u{2028}' | '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_splits_as_written_and_drops_trailing_empty_columns() {
        assert_eq!(java_split("ID|LABEL|", '|'), vec!["ID", "LABEL"]);
        assert_eq!(java_split("ID||LABEL", '|'), vec!["ID", "", "LABEL"]);
        assert_eq!(java_split("ID", '|'), vec!["ID"]);
        assert_eq!(java_split("", '|'), vec![""]);
        assert!(java_split("|", '|').is_empty());
    }

    #[test]
    fn a_tag_is_the_last_bracket_of_a_header() {
        assert_eq!(split_tag("SubClass Of [ID ANON]"), Some(("SubClass Of", "ID ANON")));
        assert_eq!(split_tag("A [B] [IRI]"), Some(("A [B]", "IRI")));
        assert_eq!(split_tag("[ID]"), None);
        assert_eq!(split_tag("ID [IRI] x"), None);
    }

    #[test]
    fn spaces_collapse_and_line_breaks_go() {
        assert_eq!(collapse("has  part some\n    (a\n  and b)"), "has part some (a and b)");
        assert_eq!(collapse(" inverse (p)"), " inverse (p)");
    }

    #[test]
    fn json_escapes_as_a_pretty_printer_does() {
        assert_eq!(json_string("a \"q\" \\ \t\n\r\u{1}\u{1f}\u{2028}é"), "\"a \\\"q\\\" \\\\ \\t\\n\\r\\u0001\\u001f\\u2028é\"");
    }

    #[test]
    fn a_header_names_an_iri_only_through_a_known_scheme() {
        assert!(valid_iri("http://purl.obolibrary.org/obo/BFO_0000051"));
        assert!(valid_iri("urn:isbn:0451450523"));
        assert!(!valid_iri("foo:bar"));
        assert!(!valid_iri("urn:x"));
        assert!(!valid_iri("no scheme"));
    }
}
