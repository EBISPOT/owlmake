//! DOSDP (Dead Simple OWL Design Patterns): a pattern and a data table
//! generate OWL axioms, one set for each row of the table.
//!
//! A pattern YAML declares entity dictionaries (`classes`, `relations`/
//! `objectProperties`, `dataProperties`, `annotationProperties`), variables
//! (`vars`, `list_vars`, `data_vars`, `data_list_vars`, `internal_vars`,
//! `substitutions`), text templates (`name`, `def`, `comment`, `namespace`,
//! the synonym and `xref` fields, `annotations`) and logical templates
//! (`equivalentTo`, `subClassOf`, `disjointWith`, `GCI`, and the general
//! `logical_axioms` list). A template is printf text filled from its `vars`,
//! or a `multi_clause` of such texts. Each row of a table fills the pattern
//! (see [`render`]): its logical text is read as a Manchester-syntax class
//! expression or axiom (see [`expression`]), its text templates become
//! annotation assertions on the row's defined class.

use std::collections::{BTreeMap, HashMap};

use anyhow::{anyhow, bail, Context, Result};
use horned_owl::model::{
    Annotation, AnnotatedComponent, AnnotationAssertion, AnnotationSubject, AnnotationValue, Build,
    ClassExpression as CE, Component, DeclareClass, Literal, MutableOntology, RcStr,
};
use horned_owl::ontology::set::SetOntology;
use serde::Deserialize;

mod docs;
mod expression;
pub(crate) mod java;
mod render;
mod table;
use crate::model::{default_prefixes, Model};

pub use docs::{docs_batch, docs_page, DocsOptions};
pub use render::Prefixes;
pub use table::TableFormat;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
/// The property a source annotation uses unless a run names another.
const OBO_SOURCE: &str = "http://www.geneontology.org/formats/oboInOwl#source";
/// The property a prototype titles its pattern's IRI with.
const DCT_TITLE: &str = "http://purl.org/dc/terms/title";

/// Which kinds of axioms `generate` emits (`--restrict-axioms-to`).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Restrict {
    #[default]
    All,
    Logical,
    Annotation,
}

impl Restrict {
    /// `all`, `logical` or `annotation`, in any case. Anything else is refused:
    /// a run asked for kinds of axioms it does not name.
    pub fn parse(s: &str) -> Result<Restrict> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Ok(Restrict::All),
            "logical" => Ok(Restrict::Logical),
            "annotation" => Ok(Restrict::Annotation),
            _ => bail!("`{s}` is not a kind of axioms: all, logical or annotation"),
        }
    }
    fn allows_logical(self) -> bool {
        matches!(self, Restrict::All | Restrict::Logical)
    }
    fn allows_annotation(self) -> bool {
        matches!(self, Restrict::All | Restrict::Annotation)
    }
}

/// Options for `generate`.
pub struct GenerateOptions {
    /// Emit only logical / only annotation / all axioms.
    pub restrict_axioms: Restrict,
    /// A table column (`--restrict-axioms-column`) whose cell sets its row's
    /// kinds of axioms: `all`, `logical` or `annotation`, in any case. An empty
    /// cell, or a table without the column, takes [`restrict_axioms`](Self::restrict_axioms).
    pub restrict_axioms_column: Option<String>,
    /// Annotate each generated axiom with the pattern's `pattern_iri`.
    pub add_axiom_source_annotation: bool,
    /// The property the source annotation uses, as an IRI or a CURIE;
    /// `oboInOwl:source` when none is named. It has to name an IRI whether or
    /// not the run annotates.
    pub axiom_source_annotation_property: Option<String>,
    /// Mint each row's defined class from the pattern's `pattern_iri` and the
    /// row's bindings, whatever its `defined_class` cell holds.
    pub generate_defined_class: bool,
    /// The literal annotation values of the supplied ontology: term IRI →
    /// property IRI → values. Readable identifiers other than `rdfs:label`
    /// and `permutations` read it.
    pub annotation_index: HashMap<String, HashMap<String, Vec<String>>>,
    /// How a CURIE becomes an IRI.
    pub prefixes: Prefixes,
    /// How the data table separates and quotes its cells.
    pub table_format: TableFormat,
}

impl Default for GenerateOptions {
    /// Every axiom, OBO prefixes, a TSV table.
    fn default() -> Self {
        GenerateOptions {
            restrict_axioms: Restrict::All,
            restrict_axioms_column: None,
            add_axiom_source_annotation: false,
            axiom_source_annotation_property: None,
            generate_defined_class: false,
            annotation_index: HashMap::new(),
            prefixes: Prefixes::obo(),
            table_format: TableFormat::Tsv,
        }
    }
}

// ── The pattern ─────────────────────────────────────────────────────────────
//
// A pattern file is read into these as it states itself: a key the model does
// not know is ignored, a key whose value is `null` is absent, and a key of the
// wrong shape refuses the whole pattern. A list or text a pattern may leave out
// is an `Option`, so a template that writes `vars: []` is a different template
// from one that writes no `vars` (two OBO fields whose templates are the same
// are one field, see `render`).

/// A value a pattern may write as `null`, read as its default.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
struct Pattern {
    #[serde(default)]
    pattern_name: Option<String>,
    #[serde(default)]
    pattern_iri: Option<String>,
    #[serde(default)]
    contributors: Option<Vec<String>>,
    #[serde(default)]
    description: Option<String>,
    /// The annotation properties whose values stand for an entity in text, in
    /// order of preference; `rdfs:label` when none are named.
    #[serde(default)]
    readable_identifiers: Option<Vec<String>>,
    #[serde(default, deserialize_with = "nullable")]
    classes: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "nullable")]
    relations: BTreeMap<String, String>,
    #[serde(rename = "objectProperties", default, deserialize_with = "nullable")]
    object_properties: BTreeMap<String, String>,
    #[serde(rename = "dataProperties", default, deserialize_with = "nullable")]
    data_properties: BTreeMap<String, String>,
    #[serde(rename = "annotationProperties", default, deserialize_with = "nullable")]
    annotation_properties: BTreeMap<String, String>,

    #[serde(default, deserialize_with = "nullable")]
    vars: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "nullable")]
    list_vars: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "nullable")]
    data_vars: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "nullable")]
    data_list_vars: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "nullable")]
    internal_vars: Vec<InternalVar>,
    #[serde(default, deserialize_with = "nullable")]
    substitutions: Vec<Substitution>,

    #[serde(default, deserialize_with = "nullable")]
    annotations: Vec<AnnotationDef>,
    #[serde(default, deserialize_with = "nullable")]
    logical_axioms: Vec<LogicalAxiom>,
    #[serde(rename = "equivalentTo", default)]
    equivalent_to: Option<AxiomTemplate>,
    #[serde(rename = "subClassOf", default)]
    subclass_of: Option<AxiomTemplate>,
    #[serde(rename = "disjointWith", default)]
    disjoint_with: Option<AxiomTemplate>,
    #[serde(rename = "GCI", default)]
    gci: Option<AxiomTemplate>,

    #[serde(default)]
    name: Option<Template>,
    #[serde(default)]
    comment: Option<Template>,
    #[serde(default)]
    def: Option<Template>,
    #[serde(default)]
    namespace: Option<Template>,
    #[serde(default)]
    exact_synonym: Option<ListAnnotationObo>,
    #[serde(default)]
    narrow_synonym: Option<ListAnnotationObo>,
    #[serde(default)]
    related_synonym: Option<ListAnnotationObo>,
    #[serde(default)]
    broad_synonym: Option<ListAnnotationObo>,
    #[serde(default, deserialize_with = "nullable")]
    generated_synonyms: Vec<Template>,
    #[serde(default, deserialize_with = "nullable")]
    generated_narrow_synonyms: Vec<Template>,
    #[serde(default, deserialize_with = "nullable")]
    generated_broad_synonyms: Vec<Template>,
    #[serde(default, deserialize_with = "nullable")]
    generated_related_synonyms: Vec<Template>,
    #[serde(default)]
    xref: Option<ListAnnotationObo>,

    /// Read for its shape alone: an instance graph generates nothing.
    #[serde(default)]
    #[allow(dead_code)]
    instance_graph: Option<InstanceGraph>,
}

/// A printf OBO annotation (`name`, `def`, `comment`, `namespace` and each
/// `generated_*synonyms` entry): `text` filled from `vars`, else its
/// `multi_clause`. Each item of the list variable `xrefs` names becomes a
/// `hasDbXref` annotation on what it generates, beside its own `annotations`.
#[derive(Deserialize, Default, Clone, PartialEq)]
struct Template {
    #[serde(default)]
    annotations: Option<Vec<AnnotationDef>>,
    #[serde(default)]
    xrefs: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    vars: Option<Vec<String>>,
    #[serde(default)]
    multi_clause: Option<MultiClause>,
    /// Extra values for a variable: its filler's values of the named
    /// annotation properties in the supplied ontology, beside its readable
    /// identifier.
    #[serde(default, deserialize_with = "permutations_field")]
    permutations: Option<Vec<Permutation>>,
}

/// A list OBO annotation (`exact_synonym`, `narrow_synonym`, `related_synonym`,
/// `broad_synonym`, `xref`): one annotation per item of the list variable
/// `value` names, each carrying the `hasDbXref` items of the list variable
/// `xrefs` names. A printf object (`text` and `vars`) names no `value`, so it is
/// refused here: printf synonyms are written under `generated_*`.
#[derive(Deserialize, Clone, PartialEq)]
#[serde(expecting = "a list annotation: a mapping with `value` (a list variable) and optionally `xrefs`")]
struct ListAnnotationObo {
    value: String,
    #[serde(default)]
    xrefs: Option<String>,
}

/// A template's `permutations`. Before dosdp-tools 0.20.0 a template has none,
/// so the key is not read at all.
fn permutations_field<'de, D>(d: D) -> Result<Option<Vec<Permutation>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    if writes_as_0_20() {
        Option::<Vec<Permutation>>::deserialize(d)
    } else {
        serde::de::IgnoredAny::deserialize(d).map(|_| None)
    }
}

/// `permutations`: for `var`, also its filler's values of the annotation
/// properties named.
#[derive(Deserialize, Clone, PartialEq)]
struct Permutation {
    var: String,
    #[serde(rename = "annotationProperties")]
    annotation_properties: Vec<String>,
}

/// A repeating template (`multi_clause`): each clause filled (a list variable
/// repeats it), the results joined by `sep`.
#[derive(Deserialize, Clone, PartialEq)]
struct MultiClause {
    #[serde(default)]
    sep: Option<String>,
    #[serde(default)]
    clauses: Option<Vec<Clause>>,
}

#[derive(Deserialize, Clone, PartialEq)]
struct Clause {
    text: String,
    #[serde(default)]
    vars: Option<Vec<String>>,
    #[serde(default)]
    sub_clauses: Option<Vec<MultiClause>>,
}

/// A regex substitution (`substitutions`): the value of `in` rewritten into
/// the variable `out`.
#[derive(Deserialize, Clone, PartialEq)]
struct Substitution {
    #[serde(rename = "in")]
    input: String,
    out: String,
    #[serde(rename = "match")]
    match_: String,
    sub: String,
}

/// An internal variable (`internal_vars`): the value `apply` gives the list
/// variable `input`.
#[derive(Deserialize, Clone, PartialEq)]
struct InternalVar {
    var_name: String,
    #[serde(default)]
    apply: Option<Function>,
    input: String,
}

/// An internal variable's function: `join` the input's items, or `regex`,
/// which gives the empty string.
#[derive(Deserialize, Clone, PartialEq)]
#[serde(untagged)]
enum Function {
    Join { join: Join },
    Regex {
        #[allow(dead_code)]
        regex: Substitution,
    },
}

/// `join`: the items, separated by `sep`.
#[derive(Deserialize, Clone, PartialEq)]
struct Join {
    sep: String,
}

/// A logical template under `equivalentTo`, `subClassOf`, `disjointWith` or
/// `GCI`. Its `multi_clause` joins its clauses with ` and ` or ` or `.
#[derive(Deserialize, Default, Clone, PartialEq)]
struct AxiomTemplate {
    #[serde(default)]
    annotations: Option<Vec<AnnotationDef>>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    vars: Option<Vec<String>>,
    #[serde(default)]
    multi_clause: Option<MultiClause>,
}

/// An entry of `logical_axioms`.
#[derive(Deserialize, Clone, PartialEq)]
struct LogicalAxiom {
    #[serde(default)]
    annotations: Option<Vec<AnnotationDef>>,
    axiom_type: AxiomType,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    vars: Option<Vec<String>>,
    #[serde(default)]
    multi_clause: Option<MultiClause>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
enum AxiomType {
    #[serde(rename = "equivalentTo")]
    EquivalentTo,
    #[serde(rename = "subClassOf")]
    SubClassOf,
    #[serde(rename = "disjointWith")]
    DisjointWith,
    #[serde(rename = "GCI")]
    Gci,
}

impl AxiomType {
    /// The name a pattern gives the kind.
    fn name(self) -> &'static str {
        match self {
            AxiomType::EquivalentTo => "equivalentTo",
            AxiomType::SubClassOf => "subClassOf",
            AxiomType::DisjointWith => "disjointWith",
            AxiomType::Gci => "GCI",
        }
    }
}

/// An entry of `annotations`: a list annotation when it names a `value`, else
/// an IRI-valued one when it names a `var`, else a printf one. Each names its
/// `annotationProperty`.
#[derive(Deserialize, Clone, PartialEq)]
#[serde(untagged)]
enum AnnotationDef {
    List {
        #[serde(default)]
        annotations: Option<Vec<AnnotationDef>>,
        #[serde(rename = "annotationProperty")]
        annotation_property: String,
        value: String,
    },
    Iri {
        #[serde(default)]
        annotations: Option<Vec<AnnotationDef>>,
        #[serde(rename = "annotationProperty")]
        annotation_property: String,
        var: String,
    },
    Printf {
        #[serde(default)]
        annotations: Option<Vec<AnnotationDef>>,
        #[serde(rename = "annotationProperty")]
        annotation_property: String,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        vars: Option<Vec<String>>,
        /// A column whose cell, when it holds anything, is the annotation's
        /// value in place of the template's.
        #[serde(rename = "override", default)]
        override_column: Option<String>,
        #[serde(default)]
        multi_clause: Option<MultiClause>,
        #[serde(default, deserialize_with = "permutations_field")]
        permutations: Option<Vec<Permutation>>,
    },
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct InstanceGraph {
    nodes: BTreeMap<String, String>,
    edges: Vec<InstanceEdge>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct InstanceEdge {
    edge: Vec<String>,
    #[serde(default)]
    annotations: Option<Vec<AnnotationDef>>,
    #[serde(default)]
    not: Option<bool>,
}

/// The YAML documents of `text`, a leading byte-order mark aside, each read
/// into one value.
///
/// A mapping that names a key more than once keeps the last mention: its
/// value, at its place. A `<<` entry merges the mapping it holds, or each
/// mapping of the sequence it holds in turn, in at its own place. A merged key
/// takes the value of the first mapping that merges it, and a key the mapping
/// names itself takes its own value, at whichever place names the key first.
/// A scalar is the string, number, boolean or null its YAML form resolves to,
/// so a number is not text.
pub(crate) fn yaml_documents(text: &str) -> Result<Vec<serde_yaml::Value>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_yaml::Deserializer::from_str(text)
        .map(|document| Ok(Resolved::deserialize(document)?.0))
        .collect()
}

/// A YAML value read as [`yaml_documents`] reads one.
struct Resolved(serde_yaml::Value);

impl<'de> Deserialize<'de> for Resolved {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde_yaml::Value;
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Resolved;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a YAML value")
            }
            fn visit_bool<E>(self, b: bool) -> Result<Resolved, E> {
                Ok(Resolved(Value::Bool(b)))
            }
            fn visit_i64<E>(self, n: i64) -> Result<Resolved, E> {
                Ok(Resolved(Value::Number(n.into())))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Resolved, E> {
                Ok(Resolved(Value::Number(n.into())))
            }
            fn visit_f64<E>(self, n: f64) -> Result<Resolved, E> {
                Ok(Resolved(Value::Number(n.into())))
            }
            fn visit_str<E>(self, s: &str) -> Result<Resolved, E> {
                Ok(Resolved(Value::String(s.to_string())))
            }
            fn visit_string<E>(self, s: String) -> Result<Resolved, E> {
                Ok(Resolved(Value::String(s)))
            }
            fn visit_unit<E>(self) -> Result<Resolved, E> {
                Ok(Resolved(Value::Null))
            }
            fn visit_none<E>(self) -> Result<Resolved, E> {
                Ok(Resolved(Value::Null))
            }
            fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Resolved, D::Error> {
                Resolved::deserialize(d)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Resolved, A::Error> {
                let mut items = Vec::new();
                while let Some(Resolved(item)) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Resolved(Value::Sequence(items)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Resolved, A::Error> {
                let mut entries: Vec<(Value, Value)> = Vec::new();
                while let Some((Resolved(key), Resolved(value))) = map.next_entry()? {
                    if !is_merge_key(&key) {
                        entries.retain(|(k, _)| *k != key);
                    }
                    entries.push((key, value));
                }
                let mut out = serde_yaml::Mapping::new();
                merge_entries(&mut out, entries, true).map_err(serde::de::Error::custom)?;
                Ok(Resolved(Value::Mapping(out)))
            }
        }
        d.deserialize_any(Visitor)
    }
}

fn is_merge_key(key: &serde_yaml::Value) -> bool {
    key.as_str() == Some("<<")
}

/// Add `entries` to `out` in order, merging in what each `<<` entry holds. An
/// entry whose key `out` already has replaces its value only when `own`: the
/// entries are the mapping's own rather than merged in.
fn merge_entries(
    out: &mut serde_yaml::Mapping,
    entries: impl IntoIterator<Item = (serde_yaml::Value, serde_yaml::Value)>,
    own: bool,
) -> Result<(), String> {
    use serde_yaml::Value;
    for (key, value) in entries {
        if is_merge_key(&key) {
            match value {
                Value::Mapping(m) => merge_entries(out, m, false)?,
                Value::Sequence(items) => {
                    for item in items {
                        let Value::Mapping(m) = item else {
                            return Err("a `<<` sequence merges mappings, and this holds something else".into());
                        };
                        merge_entries(out, m, false)?;
                    }
                }
                _ => return Err("a `<<` entry merges a mapping or a sequence of mappings".into()),
            }
        } else if own || !out.contains_key(&key) {
            out.insert(key, value);
        }
    }
    Ok(())
}

/// The one YAML document of a pattern.
fn pattern_document(yaml: &str) -> Result<serde_yaml::Value> {
    let mut documents = yaml_documents(yaml).map_err(|e| anyhow!("parsing DOSDP pattern: {e}"))?.into_iter();
    let Some(document) = documents.next() else { bail!("empty DOSDP pattern") };
    if documents.next().is_some() {
        bail!("parsing DOSDP pattern: a pattern is one YAML document, and this holds more than one");
    }
    Ok(document)
}

/// `value` decoded as a `T`, an error naming the path to the value it could
/// not decode.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: serde_yaml::Value) -> Result<T> {
    serde_path_to_error::deserialize(value).map_err(|e| {
        let path = e.path().to_string();
        if path == "." {
            anyhow!("{}", e.inner())
        } else {
            anyhow!("{path}: {}", e.inner())
        }
    })
}

/// Read a pattern from its one YAML document (see [`yaml_documents`]).
fn parse_pattern(yaml: &str) -> Result<Pattern> {
    let pattern: Pattern =
        decode(pattern_document(yaml)?).map_err(|e| anyhow!("parsing DOSDP pattern: {e}"))?;
    for (key, template) in [
        ("equivalentTo", &pattern.equivalent_to),
        ("subClassOf", &pattern.subclass_of),
        ("disjointWith", &pattern.disjoint_with),
        ("GCI", &pattern.gci),
    ] {
        if let Some(mc) = template.as_ref().and_then(|t| t.multi_clause.as_ref()) {
            let sep = mc.sep.as_deref().unwrap_or("");
            if sep != " and " && sep != " or " {
                bail!("parsing DOSDP pattern: {key}: a multi_clause joins logical expressions with ' and ' or ' or ', not '{sep}'");
            }
        }
    }
    Ok(pattern)
}

/// Schema-validate a DOSDP pattern YAML — the check every
/// `dosdp-patterns/*.yaml` in a repo has to pass before generation.
///
/// This is a REAL schema check, against the DOSDP JSON Schema (see
/// [`crate::cmd::validate_patterns`]). Reading a pattern for generation is not
/// one: the generator ignores keys it does not know.
pub fn validate(yaml: &str) -> Result<()> {
    crate::cmd::validate_patterns::validate_text(yaml)
}

/// Whether output follows the dosdp-tools 0.20.0 generator: data variables fill
/// logical text unquoted and name IRIs for IRI-valued annotations, and
/// `permutations` add annotations.
fn writes_as_0_20() -> bool {
    dosdp_tools_version() >= (0, 20, 0)
}

/// The keys of a pattern's `key` mapping (`vars:`, `data_vars:`, …) in the
/// order the mapping iterates (see [`scala_map_key_order`]): the order the
/// pattern writes them, beyond four by their hash.
fn ordered_keys(pattern_yaml: &str, key: &str, dict: &BTreeMap<String, String>) -> Vec<String> {
    let written = pattern_key_order(pattern_yaml, key);
    let keys: Vec<String> = if written.len() == dict.len() && written.iter().all(|k| dict.contains_key(k)) {
        written
    } else {
        dict.keys().cloned().collect()
    };
    scala_map_key_order(&keys).into_iter().map(|i| keys[i].clone()).collect()
}

/// The least of `values` as text compares in UTF-16 code units.
fn least<'a>(values: impl IntoIterator<Item = &'a String>) -> Option<&'a String> {
    values.into_iter().min_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()))
}

// ── Generating ──────────────────────────────────────────────────────────────

/// Generate OWL from a pattern and a data table with the default options:
/// every axiom, OBO prefixes, a TSV table. `labels` maps a term IRI to its
/// `rdfs:label`.
pub fn generate(pattern_yaml: &str, data: &str, labels: &HashMap<String, String>) -> Result<Model> {
    generate_with(pattern_yaml, data, labels, &GenerateOptions::default())
}

/// Generate OWL from a pattern and a data table under `gopts`.
///
/// The table's lines that hold nothing above U+0020 are dropped before it is
/// read (see [`table`]); its first record names the columns.
pub fn generate_with(
    pattern_yaml: &str,
    data: &str,
    labels: &HashMap<String, String>,
    gopts: &GenerateOptions,
) -> Result<Model> {
    let source_property = match &gopts.axiom_source_annotation_property {
        Some(p) => gopts
            .prefixes
            .iri(p)
            .ok_or_else(|| anyhow!("the axiom source annotation property `{p}` names no IRI"))?,
        None => OBO_SOURCE.to_string(),
    };
    let pattern = parse_pattern(pattern_yaml)?;
    let (_, rows) = table::read_generator_table(data, gopts.table_format)?;
    let b = Build::new();
    let mut ont = generate_rows(&b, &pattern, pattern_yaml, &rows, labels, gopts)?;
    if gopts.add_axiom_source_annotation {
        // The pattern's `pattern_iri` as written, on every generated axiom,
        // beside the annotations it already carries.
        let Some(pattern_iri) = pattern.pattern_iri.as_deref() else {
            bail!("--add-axiom-source-annotation needs the pattern's `pattern_iri`, and this pattern has none");
        };
        let source = Annotation {
            ap: b.annotation_property(source_property),
            av: AnnotationValue::IRI(b.iri(pattern_iri.to_string())),
            ann: Default::default(),
        };
        let generated: Vec<_> = ont.iter().cloned().collect();
        for mut ac in generated {
            ont.remove(&ac);
            ac.ann.insert(source.clone());
            ont.insert(ac);
        }
    }
    Ok(generated_model(ont, &b))
}

/// The axioms `rows` generate from `pattern`.
fn generate_rows(
    b: &Build<RcStr>,
    pattern: &Pattern,
    pattern_yaml: &str,
    rows: &[table::Row],
    labels: &HashMap<String, String>,
    gopts: &GenerateOptions,
) -> Result<SetOntology<RcStr>> {
    let readable = render::Readable { labels, index: &gopts.annotation_index };
    let renderer = render::Renderer::new(b, pattern, pattern_yaml, &gopts.prefixes, readable, writes_as_0_20())?;
    renderer.check_readable()?;
    let options = render::RowOptions {
        restrict_axioms: gopts.restrict_axioms,
        restrict_axioms_column: gopts.restrict_axioms_column.as_deref(),
        generate_defined_class: gopts.generate_defined_class,
    };
    let mut ont: SetOntology<RcStr> = SetOntology::new();
    for (i, row) in rows.iter().enumerate() {
        let (logical, annotation) = renderer.render(row, &options).with_context(|| format!("row {} of the table", i + 1))?;
        for ac in logical.into_iter().chain(annotation) {
            ont.insert(ac);
        }
    }
    Ok(ont)
}

/// The generated axioms as a model: every entity they name declared, the
/// prefix map left for the writer to choose.
fn generated_model(mut ont: SetOntology<RcStr>, b: &Build<RcStr>) -> Model {
    // Every entity of the signature is declared — the defined classes, the
    // fillers, the relations and annotation properties used — and no built-in
    // vocabulary (rdfs:label, owl:Thing, xsd:string, …). A declaration names
    // nothing else, so a regenerated module is axiom-identical to the last one
    // and not merely equivalent to it.
    declare_signature(&mut ont, b);
    let mut m = Model::from_parts(ont, default_prefixes());
    // The generated module declares only the structural prefixes and spells
    // every other IRI in full. That matters downstream: `om merge -i <module>…`
    // keeps the FIRST input's prefix map, so a module carrying a CURIE table
    // would put its prefixes into the released `definitions.owl`.
    m.format_prefixes_cleared = true;
    m
}

/// The terms a pattern and its data table name, as a set of IRIs with OBO
/// prefixes and a TSV table (see [`terms_with`]).
pub fn terms(pattern_yaml: &str, data: &str) -> Result<Vec<String>> {
    terms_with(pattern_yaml, data, &Prefixes::obo(), TableFormat::Tsv)
}

/// The terms a pattern and its data table name: every entity of the pattern's
/// logical axioms, each variable filled with its placeholder and the defined
/// class unnamed, other than those placeholders; and the IRI each row's
/// `vars` cells, each item of its `list_vars` cells, and its `defined_class`
/// cell name. The table is read as written, blank lines and all, and a row
/// with no `defined_class` cell is an error.
///
/// The terms are a set: up to four in the order they were found, beyond that
/// in the order a hash set of the IRIs iterates (see [`crate::hash_trie`]).
/// They are found axiom by axiom, each axiom's entities in the order a mutable
/// hash set of their IRIs iterates (see `scala_mutable_set_order`), and then
/// row by row.
pub fn terms_with(pattern_yaml: &str, data: &str, prefixes: &Prefixes, format: TableFormat) -> Result<Vec<String>> {
    let pattern = parse_pattern(pattern_yaml)?;
    let b = Build::new();
    let labels = HashMap::new();
    let index = HashMap::new();
    let readable = render::Readable { labels: &labels, index: &index };
    let renderer = render::Renderer::new(&b, &pattern, pattern_yaml, prefixes, readable, writes_as_0_20())?;
    let mut found: Vec<String> = Vec::new();
    for ac in renderer.pattern_logical_axioms()? {
        let named: Vec<String> =
            owl_signature(&ac).into_iter().filter(|iri| !iri.starts_with(render::VARIABLE_NS)).collect();
        for iri in scala_mutable_set_order(named, crate::owlapi_hash::iri_hash) {
            if !found.contains(&iri) {
                found.push(iri);
            }
        }
    }
    let (_, rows) = table::read_table(data, format)?;
    let vars = ordered_keys(pattern_yaml, "vars", &pattern.vars);
    let list_vars = ordered_keys(pattern_yaml, "list_vars", &pattern.list_vars);
    let mut identifiers: Vec<String> = Vec::new();
    for row in &rows {
        let mut of_row: Vec<String> = Vec::new();
        for v in &vars {
            if let Some(cell) = row.get(v) {
                of_row.push(render::java_trim(cell).to_string());
            }
        }
        for v in &list_vars {
            if let Some(cell) = row.get(v) {
                of_row.extend(render::split_list(cell).into_iter().map(|i| render::java_trim(i).to_string()));
            }
        }
        let defined = row.get("defined_class").ok_or_else(|| anyhow!("a row of the table has no `defined_class` cell"))?;
        of_row.push(render::java_trim(defined).to_string());
        identifiers.extend(of_row);
    }
    for id in scala_set_order(identifiers) {
        if let Some(iri) = prefixes.iri(&id) {
            if !found.contains(&iri) {
                found.push(iri);
            }
        }
    }
    if found.len() <= 4 {
        return Ok(found);
    }
    let items: Vec<(String, i32)> = found
        .into_iter()
        .map(|t| {
            let h = crate::owlapi_hash::java_string_hash(&t);
            (t, h)
        })
        .collect();
    Ok(crate::hash_trie::order(&items))
}

/// Every entity an axiom names, its annotations' properties and the datatypes
/// of its literals included.
fn owl_signature(ac: &AnnotatedComponent<RcStr>) -> Vec<String> {
    use horned_owl::visitor::immutable::{Visit, Walk};
    #[derive(Default)]
    struct Entities {
        iris: Vec<String>,
    }
    impl Visit<RcStr> for Entities {
        fn visit_class(&mut self, c: &horned_owl::model::Class<RcStr>) {
            self.iris.push(c.0.to_string());
        }
        fn visit_object_property(&mut self, p: &horned_owl::model::ObjectProperty<RcStr>) {
            self.iris.push(p.0.to_string());
        }
        fn visit_data_property(&mut self, p: &horned_owl::model::DataProperty<RcStr>) {
            self.iris.push(p.0.to_string());
        }
        fn visit_named_individual(&mut self, i: &horned_owl::model::NamedIndividual<RcStr>) {
            self.iris.push(i.0.to_string());
        }
        fn visit_datatype(&mut self, d: &horned_owl::model::Datatype<RcStr>) {
            self.iris.push(d.0.to_string());
        }
        fn visit_annotation_property(&mut self, p: &horned_owl::model::AnnotationProperty<RcStr>) {
            self.iris.push(p.0.to_string());
        }
        fn visit_literal(&mut self, l: &Literal<RcStr>) {
            self.iris.push(match l {
                Literal::Simple { .. } => "http://www.w3.org/2001/XMLSchema#string".to_string(),
                Literal::Language { .. } => "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral".to_string(),
                Literal::Datatype { datatype_iri, .. } => datatype_iri.to_string(),
            });
        }
    }
    let mut walk = Walk::new(Entities::default());
    walk.component(&ac.component);
    for a in ac.ann.iter() {
        walk.annotation(a);
    }
    let mut out: Vec<String> = Vec::new();
    for iri in walk.into_visit().iris {
        if !out.contains(&iri) {
            out.push(iri);
        }
    }
    out
}

/// Generate prototypical axioms from a pattern with OBO prefixes (see
/// [`prototype_with`]).
pub fn prototype(pattern_yaml: &str, labels: &HashMap<String, String>) -> Result<Model> {
    let b = Build::new();
    let ont = prototype_axioms(&b, pattern_yaml, labels, &HashMap::new(), &Prefixes::obo())?;
    Ok(generated_model(ont, &b))
}

/// Generate prototypical axioms from a pattern with no data: what one row
/// generates in which each variable holds its range as the pattern writes it
/// (a name declared under two dictionaries holds the later one's range) and
/// `defined_class` holds the pattern's `pattern_iri`, with the pattern's name
/// as that IRI's title.
pub fn prototype_with(
    pattern_yaml: &str,
    labels: &HashMap<String, String>,
    index: &HashMap<String, HashMap<String, Vec<String>>>,
    prefixes: &Prefixes,
) -> Result<Model> {
    let b = Build::new();
    let ont = prototype_axioms(&b, pattern_yaml, labels, index, prefixes)?;
    Ok(generated_model(ont, &b))
}

fn prototype_axioms(
    b: &Build<RcStr>,
    pattern_yaml: &str,
    labels: &HashMap<String, String>,
    index: &HashMap<String, HashMap<String, Vec<String>>>,
    prefixes: &Prefixes,
) -> Result<SetOntology<RcStr>> {
    let pattern = parse_pattern(pattern_yaml)?;
    let Some(iri) = pattern.pattern_iri.clone() else {
        bail!("a pattern needs a `pattern_iri` to be prototyped");
    };
    let mut header: Vec<String> = Vec::new();
    let mut cells: Vec<String> = Vec::new();
    for (key, dict) in [
        ("vars", &pattern.vars),
        ("list_vars", &pattern.list_vars),
        ("data_vars", &pattern.data_vars),
        ("data_list_vars", &pattern.data_list_vars),
    ] {
        for var in ordered_keys(pattern_yaml, key, dict) {
            header.push(var.clone());
            cells.push(dict[&var].clone());
        }
    }
    header.push("defined_class".to_string());
    cells.push(iri.clone());
    let row = table::Row::from_record(&header, &cells);
    let gopts = GenerateOptions { annotation_index: index.clone(), prefixes: prefixes.clone(), ..Default::default() };
    let mut ont = generate_rows(b, &pattern, pattern_yaml, &[row], labels, &gopts)?;
    if let Some(name) = &pattern.pattern_name {
        // The title's subject is the `pattern_iri` as written, even where it is
        // not an absolute IRI.
        ont.insert(AnnotatedComponent {
            component: Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(iri)),
                ann: Annotation {
                    ap: b.annotation_property(DCT_TITLE),
                    av: AnnotationValue::Literal(Literal::Simple { literal: name.clone() }),
                    ann: Default::default(),
                },
            }),
            ann: Default::default(),
        });
    }
    Ok(ont)
}

/// The patterns of a directory `prototype --template` names: each regular file
/// whose name ends `.yaml` or `.yml` in any case, hidden ones too, sorted by
/// name.
pub fn pattern_files_in(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| anyhow!("reading template directory {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && is_pattern_file_name(p))
        .collect();
    out.sort();
    Ok(out)
}

/// Whether a file's name ends `.yaml` or `.yml`, in any case.
pub fn is_pattern_file_name(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        let ext = ext.to_lowercase();
        ext == "yaml" || ext == "yml"
    })
}
/// Declare every signature entity of `ont`, skipping built-in vocabulary and
/// anything already declared.
fn declare_signature(ont: &mut SetOntology<RcStr>, b: &Build<RcStr>) {
    use horned_owl::model::{
        DeclareAnnotationProperty, DeclareDataProperty, DeclareDatatype, DeclareNamedIndividual,
        DeclareObjectProperty,
    };
    use horned_owl::visitor::immutable::{Visit, Walk};

    #[derive(Default)]
    struct TypedSig {
        classes: Vec<String>,
        object_properties: Vec<String>,
        data_properties: Vec<String>,
        individuals: Vec<String>,
        datatypes: Vec<String>,
        annotation_properties: Vec<String>,
    }
    impl Visit<RcStr> for TypedSig {
        fn visit_class(&mut self, c: &horned_owl::model::Class<RcStr>) {
            self.classes.push(c.0.as_ref().to_string());
        }
        fn visit_object_property(&mut self, p: &horned_owl::model::ObjectProperty<RcStr>) {
            self.object_properties.push(p.0.as_ref().to_string());
        }
        fn visit_data_property(&mut self, p: &horned_owl::model::DataProperty<RcStr>) {
            self.data_properties.push(p.0.as_ref().to_string());
        }
        fn visit_named_individual(&mut self, i: &horned_owl::model::NamedIndividual<RcStr>) {
            self.individuals.push(i.0.as_ref().to_string());
        }
        fn visit_datatype(&mut self, d: &horned_owl::model::Datatype<RcStr>) {
            self.datatypes.push(d.0.as_ref().to_string());
        }
        fn visit_annotation_property(&mut self, ap: &horned_owl::model::AnnotationProperty<RcStr>) {
            self.annotation_properties.push(ap.0.as_ref().to_string());
        }
    }

    let mut walk = Walk::new(TypedSig::default());
    let mut already: std::collections::HashSet<String> = std::collections::HashSet::new();
    for ac in ont.iter() {
        walk.component(&ac.component);
        // Walk does not descend into the annotation set, so visit those too (for
        // annotation properties used only as axiom/assertion annotations).
        for a in ac.ann.iter() {
            walk.annotation(a);
        }
        if let Component::DeclareClass(_)
        | Component::DeclareObjectProperty(_)
        | Component::DeclareDataProperty(_)
        | Component::DeclareNamedIndividual(_)
        | Component::DeclareDatatype(_)
        | Component::DeclareAnnotationProperty(_) = &ac.component
        {
            already.extend(crate::sig::signature(&ac.component));
            if let Component::DeclareAnnotationProperty(d) = &ac.component {
                already.insert(d.0 .0.to_string());
            }
        }
    }
    let sig = walk.into_visit();

    let mut emit = |iris: Vec<String>, mk: &dyn Fn(&str) -> Component<RcStr>| {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for iri in iris {
            if is_builtin_vocabulary(&iri) || already.contains(&iri) || !seen.insert(iri.clone()) {
                continue;
            }
            ont.insert(mk(&iri));
        }
    };
    emit(sig.classes, &|iri| {
        Component::DeclareClass(DeclareClass(b.class(iri)))
    });
    emit(sig.object_properties, &|iri| {
        Component::DeclareObjectProperty(DeclareObjectProperty(b.object_property(iri)))
    });
    emit(sig.data_properties, &|iri| {
        Component::DeclareDataProperty(DeclareDataProperty(b.data_property(iri)))
    });
    emit(sig.annotation_properties, &|iri| {
        Component::DeclareAnnotationProperty(DeclareAnnotationProperty(b.annotation_property(iri)))
    });
    emit(sig.individuals, &|iri| {
        Component::DeclareNamedIndividual(DeclareNamedIndividual(b.named_individual(iri)))
    });
    emit(sig.datatypes, &|iri| {
        Component::DeclareDatatype(DeclareDatatype(b.datatype(iri)))
    });
}

/// True for IRIs in the RDF/RDFS/OWL/XSD namespaces — the built-in vocabulary,
/// which OWL 2 predeclares and which therefore needs no `Declaration`.
fn is_builtin_vocabulary(iri: &str) -> bool {
    const BUILTIN_NS: [&str; 4] = [
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2000/01/rdf-schema#",
        "http://www.w3.org/2002/07/owl#",
        "http://www.w3.org/2001/XMLSchema#",
    ];
    BUILTIN_NS.iter().any(|ns| iri.starts_with(ns))
}

/// The result of a `query`: the variable column names and one row of fillers per
/// match (the first column is always `defined_class`).
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl QueryResult {
    /// Render as a TSV.
    pub fn to_tsv(&self) -> String {
        let mut s = self.columns.join("\t");
        s.push('\n');
        for r in &self.rows {
            s.push_str(&r.join("\t"));
            s.push('\n');
        }
        s
    }
}

/// Query an ontology for terms matching a pattern's logical definition: the
/// pattern's primary logical template (see [`render::Renderer::primary_expression`]),
/// each variable its placeholder, is unified against each class's equivalence
/// or subclass axioms. Returns the bound fillers (as IRIs).
pub fn query(pattern_yaml: &str, ontology: &Model) -> Result<QueryResult> {
    let pattern = parse_pattern(pattern_yaml)?;
    let prefixes = Prefixes::obo();
    let b = Build::new();
    let (labels, index) = (HashMap::new(), HashMap::new());
    let readable = render::Readable { labels: &labels, index: &index };
    let renderer = render::Renderer::new(&b, &pattern, pattern_yaml, &prefixes, readable, writes_as_0_20())?;
    let (kind, template, vars) = renderer
        .primary_expression()?
        .ok_or_else(|| anyhow!("pattern has no equivalentTo/subClassOf logical axiom to query"))?;

    let want_equiv = kind == AxiomType::EquivalentTo;
    let mut rows: std::collections::BTreeSet<Vec<String>> = Default::default();
    for ac in ontology.ont.iter() {
        let (defined, definition) = match (&ac.component, want_equiv) {
            (Component::EquivalentClasses(eq), true) => {
                // Find a named operand C and treat the other(s) as its definition.
                let named: Vec<&str> = eq.0.iter().filter_map(named_class).collect();
                if named.len() != 1 || eq.0.len() != 2 {
                    continue;
                }
                let c = named[0].to_string();
                let def = eq.0.iter().find(|x| named_class(x).is_none());
                match def {
                    Some(d) => (c, d.clone()),
                    None => continue,
                }
            }
            (Component::SubClassOf(sc), false) => match named_class(&sc.sub) {
                Some(c) => (c.to_string(), sc.sup.clone()),
                None => continue,
            },
            _ => continue,
        };
        let mut binds: BTreeMap<String, String> = BTreeMap::new();
        if unify(&template, &definition, &mut binds) {
            let mut row = vec![defined];
            for v in &vars {
                row.push(binds.get(&render::variable_iri(v)).cloned().unwrap_or_default());
            }
            rows.insert(row);
        }
    }

    let mut columns = vec!["defined_class".to_string()];
    columns.extend(vars);
    Ok(QueryResult { columns, rows: rows.into_iter().collect() })
}

fn named_class(ce: &CE<RcStr>) -> Option<&str> {
    match ce {
        CE::Class(c) => Some(c.0.as_ref()),
        _ => None,
    }
}

/// Unify a template class expression (with variable placeholder classes)
/// against a target expression, recording placeholder IRI → filler IRI
/// bindings. Supports classes, existential restrictions, and intersections
/// (matched as sets).
fn unify(template: &CE<RcStr>, target: &CE<RcStr>, binds: &mut BTreeMap<String, String>) -> bool {
    match template {
        CE::Class(c) if c.0.as_ref().starts_with(render::VARIABLE_NS) => {
            let var = c.0.as_ref().to_string();
            // Bind to a named class filler (the common case) or, for a complex
            // filler, its Manchester rendering. A variable that recurs in the
            // template must bind consistently everywhere.
            let filler = match target {
                CE::Class(t) => t.0.as_ref().to_string(),
                other => render_filler(other),
            };
            match binds.get(&var) {
                Some(prev) if *prev != filler => false,
                _ => {
                    binds.insert(var, filler);
                    true
                }
            }
        }
        CE::Class(c) => matches!(target, CE::Class(t) if t.0.as_ref() == c.0.as_ref()),
        CE::ObjectSomeValuesFrom { ope, bce } => match target {
            CE::ObjectSomeValuesFrom { ope: tope, bce: tbce } => {
                ope == tope && unify(bce, tbce, binds)
            }
            _ => false,
        },
        CE::ObjectAllValuesFrom { ope, bce } => match target {
            CE::ObjectAllValuesFrom { ope: tope, bce: tbce } => {
                ope == tope && unify(bce, tbce, binds)
            }
            _ => false,
        },
        CE::ObjectIntersectionOf(tparts) => match target {
            CE::ObjectIntersectionOf(gparts) => set_unify(tparts, gparts, binds),
            _ => false,
        },
        // Anything else must match structurally (no variables inside).
        other => other == target,
    }
}

/// Match each template operand against a distinct target operand (greedy, with
/// per-operand binding rollback). Concrete operands are matched before
/// variable-bearing ones to reduce ambiguity.
fn set_unify(tparts: &[CE<RcStr>], gparts: &[CE<RcStr>], binds: &mut BTreeMap<String, String>) -> bool {
    let mut order: Vec<usize> = (0..tparts.len()).collect();
    order.sort_by_key(|&i| has_var(&tparts[i])); // concrete (false) first
    let mut used = vec![false; gparts.len()];
    for &ti in &order {
        let mut matched = false;
        for (gi, g) in gparts.iter().enumerate() {
            if used[gi] {
                continue;
            }
            let mut trial = binds.clone();
            if unify(&tparts[ti], g, &mut trial) {
                *binds = trial;
                used[gi] = true;
                matched = true;
                break;
            }
        }
        if !matched {
            return false;
        }
    }
    true
}

/// A compact rendering of a non-named filler, used as the bound value when a
/// query variable matches a complex expression (so the match is reported rather
/// than silently dropped).
fn render_filler(ce: &CE<RcStr>) -> String {
    let ope = |o: &horned_owl::model::ObjectPropertyExpression<RcStr>| match o {
        horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) => p.0.as_ref().to_string(),
        horned_owl::model::ObjectPropertyExpression::InverseObjectProperty(p) => format!("inverse {}", p.0.as_ref()),
    };
    match ce {
        CE::Class(c) => c.0.as_ref().to_string(),
        CE::ObjectSomeValuesFrom { ope: o, bce } => format!("{} some {}", ope(o), render_filler(bce)),
        CE::ObjectAllValuesFrom { ope: o, bce } => format!("{} only {}", ope(o), render_filler(bce)),
        CE::ObjectIntersectionOf(ps) => {
            format!("({})", ps.iter().map(render_filler).collect::<Vec<_>>().join(" and "))
        }
        CE::ObjectUnionOf(ps) => {
            format!("({})", ps.iter().map(render_filler).collect::<Vec<_>>().join(" or "))
        }
        other => format!("{other:?}"),
    }
}

fn has_var(ce: &CE<RcStr>) -> bool {
    match ce {
        CE::Class(c) => c.0.as_ref().starts_with(render::VARIABLE_NS),
        CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => has_var(bce),
        CE::ObjectIntersectionOf(ps) | CE::ObjectUnionOf(ps) => ps.iter().any(has_var),
        CE::ObjectComplementOf(b) => has_var(b),
        _ => false,
    }
}

/// The iteration order of a Scala immutable map (or set) of string keys built
/// by inserting `keys` in order. Up to four keys iterate in the order they were
/// inserted. Beyond four, the order is a hash trie's (see [`crate::hash_trie`]),
/// keyed by each key's improved Java string hash: at each level, first the keys
/// alone in their five-bit slot, in slot order, then each slot holding several,
/// ordered the same way one level down; keys with equal hashes keep their
/// insertion order.
fn scala_map_key_order(keys: &[String]) -> Vec<usize> {
    if keys.len() <= 4 {
        return (0..keys.len()).collect();
    }
    fn walk(idx: &[usize], hashes: &[u32], shift: u32, out: &mut Vec<usize>) {
        if idx.len() == 1 || shift >= 32 {
            out.extend_from_slice(idx);
            return;
        }
        let mut slots: Vec<Vec<usize>> = vec![Vec::new(); 32];
        for &i in idx {
            slots[((hashes[i] >> shift) & 31) as usize].push(i);
        }
        for slot in slots.iter().filter(|s| s.len() == 1) {
            out.push(slot[0]);
        }
        for slot in slots.iter().filter(|s| s.len() > 1) {
            walk(slot, hashes, shift + 5, out);
        }
    }
    let hashes: Vec<u32> = keys
        .iter()
        .map(|k| crate::hash_trie::improve(crate::owlapi_hash::java_string_hash(k)))
        .collect();
    let idx: Vec<usize> = (0..keys.len()).collect();
    let mut out = Vec::with_capacity(keys.len());
    walk(&idx, &hashes, 0, &mut out);
    out
}

/// The distinct `items` in the order a Scala mutable hash set built by adding
/// them in order iterates: by bucket, the bucket of an item the low bits of
/// its spread `hash` (`h ^ h >>> 16`), and within a bucket by that spread hash,
/// items with equal hashes in the order added. The table has 16 buckets and
/// doubles whenever an addition would fill three quarters of it.
fn scala_mutable_set_order(items: Vec<String>, hash: impl Fn(&str) -> i32) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::new();
    for item in items {
        if !distinct.contains(&item) {
            distinct.push(item);
        }
    }
    let mut buckets = 16usize;
    while distinct.len() >= buckets * 3 / 4 {
        buckets *= 2;
    }
    let mut keyed: Vec<(usize, i32, String)> = distinct
        .into_iter()
        .map(|item| {
            let h = hash(&item);
            let spread = h ^ ((h as u32) >> 16) as i32;
            ((spread as u32 as usize) & (buckets - 1), spread, item)
        })
        .collect();
    keyed.sort_by_key(|(bucket, spread, _)| (*bucket, *spread));
    keyed.into_iter().map(|(_, _, item)| item).collect()
}

/// The distinct `items` in the order a Scala immutable set built by inserting
/// them in order iterates (see [`scala_map_key_order`]).
fn scala_set_order(items: Vec<String>) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::new();
    for item in items {
        if !distinct.contains(&item) {
            distinct.push(item);
        }
    }
    scala_map_key_order(&distinct).into_iter().map(|i| distinct[i].clone()).collect()
}

/// The keys of a pattern document's `key` mapping (`vars:`, `data_vars:`, …),
/// in document order. The `Pattern` struct's maps are sorted; a docs table
/// shows the variables as the pattern writes them, and the order a generator
/// reads a row's labels in starts from it.
fn pattern_key_order(pattern_yaml: &str, key: &str) -> Vec<String> {
    let Ok(v) = pattern_document(pattern_yaml) else { return Vec::new() };
    let Some(m) = v.get(key).and_then(|m| m.as_mapping()) else { return Vec::new() };
    m.keys().filter_map(|k| k.as_str().map(str::to_string)).collect()
}

// ──────────────────────────────── dosdp CLI ─────────────────────────────────

/// Print `owlmake dosdp` usage — the commands and options of owlmake's
/// DOSDP-pattern toolkit.
pub fn print_cli_help() {
    println!(
        "om dosdp — generate OWL from DOSDP design patterns\n\
         a native Rust reimplementation of dosdp-tools (not the original Scala tool)\n\n\
         Usage: om dosdp <command> [options]\n\n\
         Commands:\n\
         \x20 generate    Expand a pattern over a TSV/CSV data table into OWL axioms\n\
         \x20 terms       List the term IRIs a pattern (+ optional data) references\n\
         \x20 query       Query an ontology for instances of a pattern (TSV out)\n\
         \x20 prototype   Render a pattern's prototypical filled-in axioms\n\
         \x20 document    Render a pattern as Markdown documentation\n\
         \x20 validate    Check patterns against the DOSDP schema (the Python\n\
         \x20             `dosdp validate -i <DIR>`, ODK's PATTERN_TESTER)\n\n\
         Common options:\n\
         \x20 -t, --template/--pattern <FILE>   the DOSDP YAML pattern\n\
         \x20     --infile/--data <FILE>        the TSV (or CSV with --table-format csv) data\n\
         \x20 -i, --ontology/--input <FILE>     ontology for labels / querying\n\
         \x20 -o, --outfile/--output <FILE>     where to write (stdout if omitted)\n\
         \x20     --table-format <tsv|csv>      input table format (default tsv)\n\
         \x20     --batch-patterns <NAMES>      batch mode with --template-dir/--infile dirs\n\
         \x20     --restrict-axioms-to <all|logical|annotation>\n\
         \x20     --generate-defined-class      synthesise the defined class IRI\n\n\
         The legacy flag form `om dosdp --pattern P --data D -o OUT` (no command,\n\
         implies `generate`) is also accepted."
    );
}

/// Entry point for `owlmake dosdp <subcommand> …`
/// (generate / terms / query / prototype / document).
pub fn cli_main(args: &[String]) -> i32 {
    match run_cli(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("dosdp: {e:#}");
            1
        }
    }
}

/// Whether a token names a known dosdp subcommand (used to decide whether to
/// route `owlmake dosdp …` here vs. the legacy clap command).
///
/// `validate` is the odd one out: it schema-checks a pattern DIRECTORY
/// (`dosdp validate -i <DIR>`) rather than generating anything, and shares no
/// flag grammar with the rest. Repos spell it on the same command line, so one
/// shim serves both.
pub fn is_subcommand(s: &str) -> bool {
    matches!(s, "generate" | "terms" | "query" | "prototype" | "docs" | "validate")
}

fn run_cli(args: &[String]) -> Result<i32> {
    let Some((sub, rest)) = args.split_first() else {
        bail!("usage: owlmake dosdp <generate|terms|query|prototype|document|validate> [options]");
    };
    // `validate` has its own flag grammar (`-i` names a pattern DIRECTORY, not an
    // ontology) and shares nothing with the generation commands below.
    if sub == "validate" {
        return Ok(crate::cmd::validate_patterns::validate_main(rest));
    }
    let val = |names: &[&str]| -> Option<String> { cli_opt(rest, names) };
    let flag = |names: &[&str]| -> Result<bool> { cli_bool(rest, names) };

    let template = val(&["--template", "--pattern", "-t"]);
    let read_template = || -> Result<String> {
        let p = template.clone().ok_or_else(|| anyhow!("--template is required"))?;
        std::fs::read_to_string(&p).map_err(|e| anyhow!("reading template {p}: {e}"))
    };
    let outfile = val(&["--outfile", "--output", "-o"]);
    // How CURIEs become IRIs: the `--prefixes` file's, then, with
    // `--obo-prefixes`, the OBO ones.
    let prefixes = || -> Result<Prefixes> {
        let named = match val(&["--prefixes"]) {
            Some(p) => Prefixes::read_file(std::path::Path::new(&p))?,
            None => Vec::new(),
        };
        Ok(Prefixes::new(named, flag(&["--obo-prefixes"])?))
    };
    let table_format = || -> Result<TableFormat> {
        val(&["--table-format"]).map_or(Ok(TableFormat::Tsv), |f| TableFormat::parse(&f).context("--table-format"))
    };
    // The supplied ontology, its imports through `--catalog` included: the
    // fillers' readable identifiers and permutation values.
    let ontology_index = || -> Result<HashMap<String, HashMap<String, Vec<String>>>> {
        annotation_index_from_with_catalog(
            val(&["--ontology", "--input", "-i"]).as_deref(),
            val(&["--catalog", "-c"]).as_deref().map(std::path::Path::new),
        )
    };

    match sub.as_str() {
        "generate" => {
            let restrict_axioms = val(&["--restrict-axioms-to"])
                .map(|s| Restrict::parse(&s).context("--restrict-axioms-to"))
                .transpose()?
                .unwrap_or_default();
            let generate_defined_class = flag(&["--generate-defined-class"])?;
            let add_axiom_source_annotation = flag(&["--add-axiom-source-annotation"])?;
            let annotation_index = ontology_index()?;
            let labels = labels_of(&annotation_index);
            let gopts = GenerateOptions {
                restrict_axioms,
                restrict_axioms_column: val(&["--restrict-axioms-column"]),
                add_axiom_source_annotation,
                axiom_source_annotation_property: val(&["--axiom-source-annotation-property"]),
                generate_defined_class,
                annotation_index,
                prefixes: prefixes()?,
                table_format: table_format()?,
            };
            let read_data =
                |path: &str| std::fs::read_to_string(path).map_err(|e| anyhow!("reading infile {path}: {e}"));
            // Batch mode (`--batch-patterns`): for each pattern NAME, template
            // <template>/NAME.yaml, data <infile>/NAME.<table format>, output
            // <outfile>/NAME.ofn. The names are separated by single spaces.
            if let Some(batch) = val(&["--batch-patterns"]) {
                if batch.trim().is_empty() {
                    bail!("--batch-patterns names no pattern");
                }
                let names: Vec<&str> = batch.split(' ').collect();
                let tdir = val(&["--template-dir"])
                    .or_else(|| template.clone())
                    .ok_or_else(|| anyhow!("--template (a directory) is required with --batch-patterns"))?;
                for name in &names {
                    if !std::path::Path::new(&format!("{tdir}/{name}.yaml")).exists() {
                        bail!("--batch-patterns: there is no pattern `{name}` in {tdir}");
                    }
                }
                let indir = val(&["--infile", "--data"]).unwrap_or_else(|| "fillers.tsv".to_string());
                let outdir = outfile.clone().unwrap_or_else(|| "dosdp.out".to_string());
                for (dir, what) in [(&tdir, "--template"), (&indir, "--infile"), (&outdir, "--outfile")] {
                    if !std::path::Path::new(dir).is_dir() {
                        bail!("{what} must be a directory with --batch-patterns, and {dir} is not one");
                    }
                }
                let ext = gopts.table_format.extension();
                for (i, name) in names.iter().enumerate() {
                    let pat = std::fs::read_to_string(format!("{tdir}/{name}.yaml"))
                        .map_err(|e| anyhow!("reading template {tdir}/{name}.yaml: {e}"))?;
                    let data = read_data(&format!("{indir}/{name}.{ext}"))?;
                    let model = generate_with(&pat, &data, &labels, &gopts).with_context(|| format!("pattern {name}"))?;
                    let mut model = numbered_ontology(model, i + 1)?;
                    write_generated(&mut model, Some(std::path::Path::new(&format!("{outdir}/{name}.ofn"))))?;
                    eprintln!("dosdp generate: wrote {outdir}/{name}.ofn");
                }
                return Ok(0);
            }
            let pattern = read_template()?;
            let infile = val(&["--infile", "--data"]).unwrap_or_else(|| "fillers.tsv".to_string());
            let data = read_data(&infile)?;
            let mut model = numbered_ontology(generate_with(&pattern, &data, &labels, &gopts)?, 1)?;
            write_generated(&mut model, outfile.as_deref().map(std::path::Path::new))?;
            Ok(0)
        }
        "prototype" => {
            let tpath = template.clone().ok_or_else(|| anyhow!("--template is required"))?;
            let tpath = std::path::Path::new(&tpath);
            let files = if tpath.is_dir() { pattern_files_in(tpath)? } else { vec![tpath.to_path_buf()] };
            let prefixes = prefixes()?;
            let index = ontology_index()?;
            let labels = labels_of(&index);
            // Every pattern is read before any is rendered.
            let mut patterns: Vec<(std::path::PathBuf, String)> = Vec::new();
            for f in files {
                let text = std::fs::read_to_string(&f).map_err(|e| anyhow!("reading template {}: {e}", f.display()))?;
                parse_pattern(&text).with_context(|| format!("pattern {}", f.display()))?;
                patterns.push((f, text));
            }
            let b = Build::new();
            let mut ont: SetOntology<RcStr> = SetOntology::new();
            for (f, text) in &patterns {
                let axioms = prototype_axioms(&b, text, &labels, &index, &prefixes)
                    .with_context(|| format!("pattern {}", f.display()))?;
                for ac in axioms.iter() {
                    ont.insert(ac.clone());
                }
            }
            let mut model = numbered_ontology(generated_model(ont, &b), 1)?;
            write_generated(&mut model, outfile.as_deref().map(std::path::Path::new))?;
            Ok(0)
        }
        "terms" => {
            let pattern = read_template()?;
            let prefixes = prefixes()?;
            let format = table_format()?;
            let infile = val(&["--infile", "--data"]).unwrap_or_else(|| "fillers.tsv".to_string());
            // The file as it is: a byte that is not UTF-8 reads as U+FFFD.
            let bytes = std::fs::read(&infile).map_err(|e| anyhow!("reading infile {infile}: {e}"))?;
            let terms = terms_with(&pattern, &String::from_utf8_lossy(&bytes), &prefixes, format)?;
            let body: String = terms.iter().map(|t| format!("{t}\n")).collect();
            write_text(body, outfile.as_deref())?;
            Ok(0)
        }
        "query" => {
            let pattern = read_template()?;
            let onto = val(&["--ontology", "--input", "-i"]).ok_or_else(|| anyhow!("--ontology is required"))?;
            let mut model = crate::io::load(std::path::Path::new(&onto))?;
            // --reasoner: query the *reasoned* ontology — assert the inferred
            // subsumptions first so inferred matches are found too.
            if let Some(r) = val(&["--reasoner"]) {
                model = crate::cmd::reason::reason(model, &r, false, true)?;
            }
            if flag(&["--print-query"])? {
                eprintln!("dosdp query: a structural match on the pattern's primary logical template");
            }
            let result = query(&pattern, &model)?;
            write_text(result.to_tsv(), outfile.as_deref())?;
            Ok(0)
        }
        "docs" => {
            // A batch names the directories of its patterns, tables and pages;
            // without one, `--template`, `--infile` and `--outfile` name one
            // pattern, its table and its page.
            let template = template.clone().ok_or_else(|| anyhow!("--template is required"))?;
            let infile = val(&["--infile", "-i"]).ok_or_else(|| anyhow!("--infile is required"))?;
            let outfile = outfile.clone().ok_or_else(|| anyhow!("--outfile is required"))?;
            let batch: Vec<String> = val(&["--batch-patterns"])
                .map(|s| s.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();
            let ontology = supplied_ontology(
                val(&["--ontology", "--input"]).as_deref(),
                val(&["--catalog", "-c"]).as_deref().map(std::path::Path::new),
            )?;
            let opts = DocsOptions {
                ontology: ontology.as_ref(),
                prefixes: prefixes()?,
                table_format: table_format()?,
                data_location_prefix: val(&["--data-location-prefix"])
                    .unwrap_or_else(|| "http://example.org/".to_string()),
            };
            let (template, infile, outfile) =
                (std::path::Path::new(&template), std::path::Path::new(&infile), std::path::Path::new(&outfile));
            if batch.is_empty() {
                docs_page(template, infile, outfile, &opts)?;
            } else {
                docs_batch(template, infile, &batch, outfile, &opts)?;
            }
            Ok(0)
        }
        other => bail!("unknown dosdp subcommand '{other}'"),
    }
}

/// Find the value of a `--name value` / `--name=value` option.
fn cli_opt(args: &[String], names: &[&str]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        for n in names {
            if a == n {
                return args.get(i + 1).cloned();
            }
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return Some(v.to_string());
            }
        }
        i += 1;
    }
    None
}

/// A boolean option: false when absent, else its value — `true`, `false`, `1`
/// or `0`, in any case — given as `--name=value` or as the next argument.
fn cli_bool(args: &[String], names: &[&str]) -> Result<bool> {
    let present = args.iter().any(|a| names.iter().any(|n| a == n || a.starts_with(&format!("{n}="))));
    if !present {
        return Ok(false);
    }
    let value = cli_opt(args, names).ok_or_else(|| anyhow!("{} needs a value: true or false", names[0]))?;
    match value.to_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => bail!("{}: `{value}` is not a boolean value: true, false, 1 or 0", names[0]),
    }
}

/// Each term's least `rdfs:label` (see [`least`]) in `index`.
fn labels_of(index: &HashMap<String, HashMap<String, Vec<String>>>) -> HashMap<String, String> {
    index
        .iter()
        .filter_map(|(iri, props)| props.get(RDFS_LABEL).and_then(least).map(|l| (iri.clone(), l.clone())))
        .collect()
}

/// Build the `(labels, annotation_index)` from already-loaded models — the edit
/// ontology plus its import closure (where the fillers' labels/synonyms live) —
/// by unioning their literal annotation assertions. The closure is required, not
/// optional: a `%s` naming an imported filler has no label without it.
pub fn ontology_context_from_models(
    models: &[&Model],
) -> (HashMap<String, String>, HashMap<String, HashMap<String, Vec<String>>>) {
    let mut index: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    for m in models {
        for ac in m.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    let val = match lit {
                        Literal::Simple { literal }
                        | Literal::Language { literal, .. }
                        | Literal::Datatype { literal, .. } => literal.clone(),
                    };
                    index
                        .entry(s.as_ref().to_string())
                        .or_default()
                        .entry(aa.ann.ap.0.as_ref().to_string())
                        .or_default()
                        .push(val);
                }
            }
        }
    }
    let labels = index
        .iter()
        .filter_map(|(iri, props)| {
            // Collect every label a term carries and keep the lexicographic
            // minimum, so the choice is deterministic regardless of import/axiom
            // order (e.g. CHEBI_22470 → "alpha-tocopherol" wins over
            // "α-tocopherol").
            props.get(RDFS_LABEL).and_then(least).map(|l| (iri.clone(), l.clone()))
        })
        .collect();
    (labels, index)
}

/// Build the `(labels, annotation_index)` pair from an ontology file, for driving
/// `generate` over a pattern set (`generate --ontology`): the labels map
/// (`rdfs:label`) is derived from the same index that feeds `permutations`. A
/// single load yields both.
pub fn ontology_context(
    path: &std::path::Path,
) -> Result<(HashMap<String, String>, HashMap<String, HashMap<String, Vec<String>>>)> {
    let index = annotation_index_from(path.to_str())?;
    let labels = index
        .iter()
        .filter_map(|(iri, props)| {
            // Collect every label a term carries and keep the lexicographic
            // minimum, so the choice is deterministic regardless of import/axiom
            // order (e.g. CHEBI_22470 → "alpha-tocopherol" wins over
            // "α-tocopherol").
            props.get(RDFS_LABEL).and_then(least).map(|l| (iri.clone(), l.clone()))
        })
        .collect();
    Ok((labels, index))
}

/// Index every literal `AnnotationAssertion` in `path` as filler IRI →
/// (annotation property IRI → values), for `permutations`. The label map is
/// derivable from this (the `rdfs:label` entries).
fn annotation_index_from(
    path: Option<&str>,
) -> Result<HashMap<String, HashMap<String, Vec<String>>>> {
    annotation_index_from_with_catalog(path, None)
}

/// As [`annotation_index_from`], but resolving the ontology's `owl:imports`
/// through `catalog` first.
///
/// `--catalog=catalog-v001.xml` maps the import IRIs onto the repo's local
/// `imports/` files. Without the closure the index holds only the edit
/// ontology's own labels, and every `%s` naming an imported filler substitutes
/// the filler's IRI instead — OBA's generated definitions would read
/// "the ratio of … to http://purl.obolibrary.org/obo/PATO_0000070 of
/// http://purl.obolibrary.org/obo/PR_P18627 in …".
fn annotation_index_from_with_catalog(
    path: Option<&str>,
    catalog: Option<&std::path::Path>,
) -> Result<HashMap<String, HashMap<String, Vec<String>>>> {
    Ok(supplied_ontology(path, catalog)?.map(|m| annotation_index_of(&m)).unwrap_or_default())
}

/// The ontology at `path`, if one is named, its `owl:imports` resolved through
/// `catalog`.
fn supplied_ontology(path: Option<&str>, catalog: Option<&std::path::Path>) -> Result<Option<Model>> {
    let Some(p) = path else { return Ok(None) };
    let p = std::path::Path::new(p);
    let mut m = crate::io::load(p)?;
    if let Some(cat) = catalog {
        crate::cmd::merge_import_closure(&mut m, cat, Some(p))?;
    }
    Ok(Some(m))
}

/// Every literal `AnnotationAssertion` of `model` as term IRI → (annotation
/// property IRI → values).
fn annotation_index_of(model: &Model) -> HashMap<String, HashMap<String, Vec<String>>> {
    let mut idx: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) = (&aa.subject, &aa.ann.av) {
                let val = match lit {
                    Literal::Simple { literal } | Literal::Language { literal, .. } | Literal::Datatype { literal, .. } => {
                        literal.clone()
                    }
                };
                idx.entry(s.as_ref().to_string())
                    .or_default()
                    .entry(aa.ann.ap.0.as_ref().to_string())
                    .or_default()
                    .push(val);
            }
        }
    }
    idx
}

/// Retype every plain annotation literal as `xsd:string`: the value of each
/// annotation assertion and each axiom annotation. The generator builds bare
/// `Literal::Simple` values, which denote the same thing but render without
/// the datatype.
fn type_literals_as_xsd_string(model: &mut Model) {
    use horned_owl::model::{AnnotatedComponent, Component, MutableOntology};
    let b = Build::new();
    let xsd = b.iri("http://www.w3.org/2001/XMLSchema#string");
    // An annotation's value and its own annotations', at any depth.
    fn retype(a: &Annotation<RcStr>, xsd: &horned_owl::model::IRI<RcStr>) -> Annotation<RcStr> {
        let av = match &a.av {
            AnnotationValue::Literal(Literal::Simple { literal }) => {
                AnnotationValue::Literal(Literal::Datatype { literal: literal.clone(), datatype_iri: xsd.clone() })
            }
            other => other.clone(),
        };
        Annotation { ap: a.ap.clone(), av, ann: a.ann.iter().map(|n| retype(n, xsd)).collect() }
    }
    let comps: Vec<AnnotatedComponent<_>> = model.ont.iter().cloned().collect();
    for old in comps {
        let mut new = old.clone();
        if let Component::AnnotationAssertion(aa) = &mut new.component {
            aa.ann = retype(&aa.ann, &xsd);
        }
        // Axiom annotations too — a DOSDP `annotations:` entry with an `xref`
        // renders as `Annotation(oio:hasDbXref "…"^^xsd:string)` on the assertion.
        new.ann = new.ann.iter().map(|a| retype(a, &xsd)).collect();
        if new != old {
            model.ont.remove(&old);
            model.ont.insert(new);
        }
    }
}

/// The dosdp-tools release generation writes as: the one the emulated ODK
/// release ships, else the newest release owlmake models.
fn dosdp_tools_version() -> (u32, u32, u32) {
    crate::build::emulation()
        .and_then(|e| e.odk)
        .map_or((0, 20, 0), crate::odk::workflows::odk_dosdp_tools_version)
}

/// Write `model` as a generator writes an ontology: in functional syntax,
/// whatever `outfile` is named (`pattern.owl` and `definitions.owl` are both
/// functional under a `.owl` name), to stdout without one; and before
/// dosdp-tools 0.20.0, with `^^xsd:string` spelled out on its string literals.
pub(crate) fn write_generated(model: &mut Model, outfile: Option<&std::path::Path>) -> Result<()> {
    let typed = dosdp_tools_version() < (0, 20, 0);
    if typed {
        type_literals_as_xsd_string(model);
    }
    horned_owl::io::ofn::writer::set_write_xsd_string(typed);
    let written = match outfile {
        Some(p) => crate::io::save_as(model, p, crate::io::Format::Functional),
        None => {
            let mut buf = Vec::new();
            crate::io::write_to_ref(model, &mut buf, crate::io::Format::Functional)
                .map(|()| print!("{}", String::from_utf8_lossy(&buf)))
        }
    };
    // The switch is process-wide, and what this process writes next is not a
    // generator's ontology.
    horned_owl::io::ofn::writer::set_write_xsd_string(false);
    written
}

/// `model` as the `n`th ontology (from 1) one generator run writes: named
/// `urn:unnamed:ontology#ont<n>`, with the five standard prefixes and `:` bound
/// to that IRI as written. A run numbers its ontologies in the order it writes
/// them, so a batch's second pattern is `#ont2`.
pub(crate) fn numbered_ontology(model: Model, n: usize) -> Result<Model> {
    let iri = format!("urn:unnamed:ontology#ont{n}");
    let mut model = crate::cmd::annotate::annotate(model, Some(&iri), None, &[], &[], false)?;
    // `:` is the IRI VERBATIM. The default the writer derives from an ontology
    // IRI carries a trailing `#`, so the binding goes in first, the derived set is
    // copied over it minus its own `:`, and the model is marked as carrying its
    // own prefixes, so the writer does not derive them again.
    let mut prefixes = horned_owl::curie::PrefixMapping::default();
    let _ = prefixes.add_prefix("", &iri);
    for (p, ns) in crate::io::robot_ofn_prefixes(&model).mappings() {
        if !p.is_empty() {
            let _ = prefixes.add_prefix(p, ns);
        }
    }
    model.prefixes = prefixes;
    model.format_prefixes_cleared = false;
    Ok(model)
}

fn write_text(text: impl AsRef<str>, outfile: Option<&str>) -> Result<()> {
    match outfile {
        Some(p) => std::fs::write(p, text.as_ref()).map_err(|e| anyhow!("writing {p}: {e}"))?,
        None => print!("{}", text.as_ref()),
    }
    Ok(())
}

/// Parse a DOSDP data TSV, returning nothing useful but validating shape.
pub fn validate_data(data_tsv: &str) -> Result<()> {
    if data_tsv.lines().next().is_none() {
        bail!("empty data table");
    }
    Ok(())
}

#[cfg(test)]
mod multi_clause_terms_tests {
    /// A relation named only inside a repeating clause is one of the pattern's
    /// terms: UBERON's vein pattern says `'tributary of' some %s` under
    /// `multi_clause` alone, and its seed must carry RO:0002376.
    #[test]
    fn a_relation_named_in_a_repeating_clause_is_a_term() {
        let yaml = "pattern_name: vein\n\
                    pattern_iri: http://example.org/vein\n\
                    classes:\n  vessel: \"UBERON:0001981\"\n\
                    relations:\n  part of: \"BFO:0000050\"\n  tributary of: \"RO:0002376\"\n\
                    vars:\n  parent: \"'vessel'\"\n\
                    list_vars:\n  tributary_of: \"'vessel'\"\n\
                    logical_axioms:\n\
                    \x20 - axiom_type: subClassOf\n    text: \"'part of' some %s\"\n    vars:\n      - parent\n\
                    \x20 - axiom_type: subClassOf\n    multi_clause:\n      sep: \" and \"\n      clauses:\n        - text: \"'tributary of' some %s\"\n          vars:\n            - tributary_of\n";
        let terms = super::terms(yaml, "defined_class\tparent\ttributary_of\n").unwrap();
        assert!(terms.contains(&"http://purl.obolibrary.org/obo/RO_0002376".to_string()), "{terms:?}");
        assert!(terms.contains(&"http://purl.obolibrary.org/obo/BFO_0000050".to_string()), "{terms:?}");
    }
}
