//! DOSDP (Dead Simple OWL Design Patterns) generation — the `generate` step
//! CL/UBERON/MONDO use to produce logical definitions and annotations from a
//! pattern + a TSV data table.
//!
//! A pattern YAML declares entity dictionaries (`classes`, `relations`/
//! `objectProperties`, `dataProperties`, `annotationProperties`), variables
//! (`vars`, `list_vars`, `data_vars`, `data_list_vars`), text/annotation
//! templates (`name`, `def`, `comment`, `*_synonym`, `xref`, `annotations`), and
//! logical templates (`equivalentTo`, `subClassOf`, `disjointWith`, `GCI`, and
//! the general `logical_axioms` list). Templates are printf-style (`%s` filled
//! positionally from a `vars` list). Each data row instantiates the pattern: the
//! logical templates become Manchester class expressions (parsed by
//! [`crate::io::manchester`]) and the text templates become annotation axioms.

use std::collections::{BTreeMap, HashMap};

use anyhow::{anyhow, bail, Result};
use horned_owl::model::{
    Annotation, AnnotatedComponent, AnnotationAssertion, AnnotationSubject, AnnotationValue, Build,
    ClassExpression as CE, Component, DeclareClass, DisjointClasses, EquivalentClasses, Literal,
    MutableOntology, RcStr, SubClassOf,
};
use horned_owl::ontology::set::SetOntology;
use regex::Regex;
use serde::Deserialize;

use crate::io::manchester;
use crate::model::{default_prefixes, Model};

/// Deserialize a field written as either a single `T` or a sequence of `T` into a
/// `Vec<T>`. Patterns write `generated_synonyms:` (and friends) as a YAML
/// **list**; a bare object is also accepted, for leniency.
fn de_one_or_many<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany<T> {
        One(T),
        Many(Vec<T>),
    }
    Ok(match OneOrMany::<T>::deserialize(d)? {
        OneOrMany::One(x) => vec![x],
        OneOrMany::Many(v) => v,
    })
}

/// Deserialize a field written as either a single string or a sequence of strings
/// into a `Vec<String>`. Patterns write `xrefs:` as a **string** (a column
/// reference); a list is also accepted.
fn de_string_or_seq<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrOrSeq {
        Str(String),
        Seq(Vec<String>),
    }
    Ok(match StrOrSeq::deserialize(d)? {
        StrOrSeq::Str(s) => vec![s],
        StrOrSeq::Seq(v) => v,
    })
}

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const IAO_DEF: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const OBO_IN_OWL: &str = "http://www.geneontology.org/formats/oboInOwl#";
/// Default property for `--add-axiom-source-annotation`.
const OBO_SOURCE: &str = "http://www.geneontology.org/formats/oboInOwl#source";

/// Which kinds of axioms `generate` emits (`--restrict-axioms-to`).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Restrict {
    #[default]
    All,
    Logical,
    Annotation,
}

impl Restrict {
    pub fn parse(s: &str) -> Restrict {
        match s.to_ascii_lowercase().as_str() {
            "logical" => Restrict::Logical,
            "annotation" | "annotations" => Restrict::Annotation,
            _ => Restrict::All,
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
#[derive(Default)]
pub struct GenerateOptions {
    /// Emit only logical / only annotation / all axioms.
    pub restrict_axioms: Restrict,
    /// A TSV column whose truthy value restricts that row to logical axioms
    /// (`--restrict-axioms-column`).
    pub restrict_axioms_column: Option<String>,
    /// Annotate each generated axiom with its source pattern IRI.
    pub add_axiom_source_annotation: bool,
    /// Property to use for the source annotation (default oboInOwl:source).
    pub axiom_source_annotation_property: Option<String>,
    /// Auto-generate the `defined_class` IRI from `base_IRI` + a hash of the
    /// variable fillers when the TSV has no `defined_class` column.
    pub generate_defined_class: bool,
    /// Index for `permutations`: filler term IRI → (annotation property IRI →
    /// values), built from the supplied ontology. Empty disables permutations.
    pub annotation_index: HashMap<String, HashMap<String, Vec<String>>>,
    /// Extra CURIE prefixes (e.g. a repo's `config/prefixes.yaml`: `SLM`, `LM`),
    /// merged over the standard DOSDP set for entity expansion.
    pub extra_prefixes: Vec<(String, String)>,
    /// Per-VARIABLE display text, overriding the filler IRI's label. `prototype`
    /// needs this: two variables may share one range class (OBA's `entity` and
    /// `stimulus` are both `owl:Thing`), and each must still print as its own
    /// name — a map keyed by filler IRI cannot express that.
    pub var_labels: HashMap<String, String>,
}

#[derive(Deserialize, Default)]
struct Pattern {
    #[serde(default)]
    pattern_name: Option<String>,
    #[serde(default)]
    pattern_iri: Option<String>,
    #[serde(rename = "base_IRI", default)]
    base_iri: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    classes: BTreeMap<String, String>,
    #[serde(default)]
    relations: BTreeMap<String, String>,
    #[serde(rename = "objectProperties", default)]
    object_properties: BTreeMap<String, String>,
    #[serde(rename = "dataProperties", default)]
    data_properties: BTreeMap<String, String>,
    #[serde(rename = "annotationProperties", default)]
    annotation_properties: BTreeMap<String, String>,

    #[serde(default)]
    vars: BTreeMap<String, String>,
    #[serde(rename = "list_vars", default)]
    list_vars: BTreeMap<String, String>,
    #[serde(rename = "data_vars", default)]
    data_vars: BTreeMap<String, String>,
    #[serde(rename = "data_list_vars", default)]
    data_list_vars: BTreeMap<String, String>,

    #[serde(default)]
    name: Option<Template>,
    #[serde(default)]
    def: Option<Template>,
    #[serde(default)]
    comment: Option<Template>,
    #[serde(default)]
    namespace: Option<Template>,
    #[serde(rename = "exact_synonym", default)]
    exact_synonym: Option<Template>,
    #[serde(rename = "narrow_synonym", default)]
    narrow_synonym: Option<Template>,
    #[serde(rename = "related_synonym", default)]
    related_synonym: Option<Template>,
    #[serde(rename = "broad_synonym", default)]
    broad_synonym: Option<Template>,
    #[serde(default)]
    xref: Option<Template>,
    #[serde(rename = "generated_synonyms", default, deserialize_with = "de_one_or_many")]
    generated_synonyms: Vec<Template>,
    #[serde(rename = "generated_narrow_synonyms", default, deserialize_with = "de_one_or_many")]
    generated_narrow_synonyms: Vec<Template>,
    #[serde(rename = "generated_broad_synonyms", default, deserialize_with = "de_one_or_many")]
    generated_broad_synonyms: Vec<Template>,
    #[serde(rename = "generated_related_synonyms", default, deserialize_with = "de_one_or_many")]
    generated_related_synonyms: Vec<Template>,
    #[serde(default)]
    annotations: Vec<AnnotationDef>,

    #[serde(default)]
    substitutions: Vec<Substitution>,
    #[serde(rename = "internal_vars", default)]
    internal_vars: Vec<InternalVar>,
    #[serde(rename = "instance_graph", default)]
    instance_graph: Option<InstanceGraph>,

    #[serde(rename = "equivalentTo", default)]
    equivalent_to: Option<AxiomTemplate>,
    #[serde(rename = "subClassOf", default)]
    subclass_of: Option<AxiomTemplate>,
    #[serde(rename = "disjointWith", default)]
    disjoint_with: Option<AxiomTemplate>,
    #[serde(rename = "GCI", default)]
    gci: Option<AxiomTemplate>,
    #[serde(rename = "logical_axioms", default)]
    logical_axioms: Vec<LogicalAxiom>,
}

#[derive(Deserialize, Default, Clone)]
struct Template {
    #[serde(default)]
    text: String,
    #[serde(default)]
    vars: Vec<String>,
    /// `def`-style cross-references attached as axiom annotations. Patterns
    /// write this as a single string (a column reference); a list is also taken.
    #[serde(default, deserialize_with = "de_string_or_seq")]
    xrefs: Vec<String>,
    #[serde(default)]
    multi_clause: Option<MultiClause>,
    /// `permutations`: generate extra annotation values by substituting, for a
    /// variable, the values of the filler term's own annotation properties (e.g.
    /// its synonyms) drawn from the supplied ontology — combinatorially with the
    /// label.
    #[serde(default)]
    permutations: Vec<Permutation>,
    /// A list variable: one annotation per item (e.g.
    /// `exact_synonym: {value: exact_synonyms}`).
    #[serde(default)]
    value: Option<String>,
    /// A single variable whose filler IRI becomes the annotation object (an
    /// IRI-valued annotation).
    #[serde(default)]
    var: Option<String>,
    /// Axiom annotations attached to the generated assertion — e.g. a `def`'s
    /// nested `annotations:` carrying its `xref` provenance.
    #[serde(default)]
    annotations: Vec<AnnotationDef>,
}

/// A permutation spec (`permutations`): for `var`, also substitute the filler
/// term's values of the listed annotation properties (by their pattern
/// short-names), in addition to its label.
#[derive(Deserialize, Default, Clone)]
struct Permutation {
    var: String,
    #[serde(rename = "annotationProperties", default)]
    annotation_properties: Vec<String>,
}

/// A repeating-clause template (`multi_clause`): each clause is filled (a list
/// variable repeats it), and the results are joined by `sep`.
#[derive(Deserialize, Default, Clone)]
struct MultiClause {
    #[serde(default)]
    sep: Option<String>,
    #[serde(default)]
    clauses: Vec<Clause>,
}

#[derive(Deserialize, Default, Clone)]
struct Clause {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    vars: Vec<String>,
    #[serde(default)]
    sub_clauses: Vec<MultiClause>,
}

/// A regex substitution producing a derived variable (`substitutions`).
#[derive(Deserialize, Default, Clone)]
struct Substitution {
    #[serde(rename = "in")]
    input: String,
    out: String,
    #[serde(rename = "match", default)]
    match_: String,
    #[serde(default)]
    sub: String,
}

/// A derived variable computed from others (`internal_vars`): either a regex
/// substitution over one input, or a join of several inputs.
#[derive(Deserialize, Default, Clone)]
struct InternalVar {
    var: String,
    #[serde(default)]
    input: Option<String>,
    #[serde(rename = "match", default)]
    match_: Option<String>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    join: Option<Join>,
}

#[derive(Deserialize, Default, Clone)]
struct Join {
    #[serde(default)]
    sep: Option<String>,
    #[serde(default)]
    vars: Vec<String>,
}

/// An instance graph (`instance_graph`): nodes become typed individuals and
/// edges become object-property assertions.
#[derive(Deserialize, Default, Clone)]
struct InstanceGraph {
    #[serde(default)]
    nodes: BTreeMap<String, String>,
    #[serde(default)]
    edges: Vec<Vec<String>>,
}

#[derive(Deserialize, Default, Clone)]
struct AxiomTemplate {
    #[serde(default)]
    text: String,
    #[serde(default)]
    vars: Vec<String>,
    #[serde(default)]
    annotations: Vec<AnnotationDef>,
    #[serde(default)]
    multi_clause: Option<MultiClause>,
}

#[derive(Deserialize, Default, Clone)]
struct LogicalAxiom {
    #[serde(default)]
    axiom_type: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    vars: Vec<String>,
    #[serde(default)]
    annotations: Vec<AnnotationDef>,
    #[serde(default)]
    multi_clause: Option<MultiClause>,
}

#[derive(Deserialize, Default, Clone)]
struct AnnotationDef {
    #[serde(rename = "annotationProperty", default)]
    annotation_property: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    vars: Vec<String>,
    /// A single variable whose filler IRI becomes the annotation object.
    #[serde(default)]
    var: Option<String>,
    /// A list variable: one annotation axiom per list item.
    #[serde(default)]
    value: Option<String>,
    #[serde(default, deserialize_with = "de_string_or_seq")]
    xrefs: Vec<String>,
    #[serde(default)]
    multi_clause: Option<MultiClause>,
    #[serde(default)]
    permutations: Vec<Permutation>,
    /// `override`: a data column whose value, when present, supplies this
    /// annotation's value directly, overriding the template.
    #[serde(rename = "override", default)]
    override_column: Option<String>,
    /// Nested axiom annotations on this annotation (e.g. a generated synonym's
    /// `xref` provenance).
    #[serde(default)]
    annotations: Vec<AnnotationDef>,
}

/// Parse a DOSDP pattern YAML robustly: strip a leading UTF-8 BOM (some OBO
/// pattern files carry one) and take the first YAML document (tolerating stray
/// `---` separators), which `serde_yaml::from_str` otherwise rejects.
fn parse_pattern(yaml: &str) -> Result<Pattern> {
    use serde::de::Deserialize;
    let yaml = yaml.strip_prefix('\u{feff}').unwrap_or(yaml);
    let mut last_err = None;
    for doc in serde_yaml::Deserializer::from_str(yaml) {
        match Pattern::deserialize(doc) {
            Ok(p) => return Ok(p),
            Err(e) => last_err = Some(e),
        }
    }
    match last_err {
        Some(e) => Err(anyhow!("parsing DOSDP pattern: {e}")),
        None => bail!("empty DOSDP pattern"),
    }
}

/// Schema-validate a DOSDP pattern YAML — the check every
/// `dosdp-patterns/*.yaml` in a repo has to pass before generation.
///
/// This is a REAL schema check, against the DOSDP JSON Schema (see
/// [`crate::cmd::validate_patterns`]). `parse_pattern(yaml)` validates nothing
/// and cannot stand in for it: [`Pattern`] has no `deny_unknown_fields` and
/// `#[serde(default)]` on every field, so any YAML mapping at all deserializes
/// successfully and a schema-invalid pattern would report PASS.
///
/// [`parse_pattern`] itself stays permissive on purpose — generation runs after
/// validation, so it must load every pattern that got past this gate.
pub fn validate(yaml: &str) -> Result<()> {
    crate::cmd::validate_patterns::validate_text(yaml)
}

/// Prefix map for DOSDP generation: the standard set plus the OBO-pattern
/// aliases (`oio`, `dct`, `skos`), which OBO patterns use ubiquitously and never
/// declare themselves — `oio:hasExactSynonym`, `dct:contributor`, etc.
fn dosdp_prefixes() -> horned_owl::curie::PrefixMapping {
    let mut p = default_prefixes();
    let _ = p.add_prefix("oio", "http://www.geneontology.org/formats/oboInOwl#");
    let _ = p.add_prefix("dct", "http://purl.org/dc/terms/");
    let _ = p.add_prefix("skos", "http://www.w3.org/2004/02/skos/core#");
    p
}

/// Generate OWL from a DOSDP pattern + a TSV data table with default options.
/// `labels` optionally maps filler IRIs to labels for the text templates.
pub fn generate(
    pattern_yaml: &str,
    data_tsv: &str,
    labels: &HashMap<String, String>,
) -> Result<Model> {
    generate_with(pattern_yaml, data_tsv, labels, &GenerateOptions::default())
}

/// Parse a delimited table into records, honoring RFC 4180 quoting — a field
/// wrapped in `"` has its surrounding quotes stripped and doubled `""`
/// unescaped, and a quoted field may contain the delimiter or a newline. A
/// naive `split('\t')` would keep a curator's surrounding quotes (e.g. a
/// comma-bearing `def` column written `"A venule, …"`), emitting a literal
/// `\"…\"` value and, worse, mis-splitting on tabs inside a quote. Blank lines
/// are dropped before parsing.
fn parse_table_records(text: &str, delim: char) -> Vec<Vec<String>> {
    let cleaned: String = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if cleaned.is_empty() {
        return Vec::new();
    }

    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut at_field_start = true;
    let mut chars = cleaned.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && at_field_start {
            in_quotes = true;
            at_field_start = false;
        } else if c == delim {
            record.push(std::mem::take(&mut field));
            at_field_start = true;
        } else if c == '\n' {
            record.push(std::mem::take(&mut field));
            records.push(std::mem::take(&mut record));
            at_field_start = true;
        } else if c != '\r' {
            field.push(c);
            at_field_start = false;
        }
    }
    record.push(field);
    records.push(record);
    records
}

/// Generate OWL from a DOSDP pattern + a TSV data table, honoring `gopts`.
pub fn generate_with(
    pattern_yaml: &str,
    data_tsv: &str,
    labels: &HashMap<String, String>,
    gopts: &GenerateOptions,
) -> Result<Model> {
    let pattern: Pattern =
        parse_pattern(pattern_yaml)?;
    let source_prop = gopts
        .axiom_source_annotation_property
        .clone()
        .unwrap_or_else(|| OBO_SOURCE.to_string());

    // Merge every entity dictionary into one short-name → IRI map for Manchester
    // substitution — object, data and annotation properties included, not just
    // classes and relations.
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for dict in [
        &pattern.classes,
        &pattern.relations,
        &pattern.object_properties,
        &pattern.data_properties,
        &pattern.annotation_properties,
    ] {
        for (k, v) in dict {
            names.insert(k.clone(), v.clone());
        }
    }

    let records = parse_table_records(data_tsv, '\t');
    let mut records = records.into_iter();
    let header: Vec<String> = records
        .next()
        .ok_or_else(|| anyhow!("empty DOSDP data table"))?
        .into_iter()
        .map(|c| c.trim().to_string())
        .collect();
    let dc_idx = header.iter().position(|h| h == "defined_class" || h == "defined class");
    if dc_idx.is_none() && !gopts.generate_defined_class {
        bail!("DOSDP data table needs a `defined_class` column (or --generate-defined-class)");
    }
    let col = |name: &str| header.iter().position(|h| h == name);

    let b = Build::new();
    let mut prefixes = dosdp_prefixes();
    // A repo's custom prefixes (`config/prefixes.yaml`: `SLM`, `LM`, …) win over
    // the standard set, so `SLM:000043005` expands to its SwissLipids IRI.
    for (p, ns) in &gopts.extra_prefixes {
        let _ = prefixes.add_prefix(p, ns);
    }
    let mut ont: SetOntology<RcStr> = SetOntology::new();

    for cells in records {
        let line = cells.join("\t");
        let cells: Vec<&str> = cells.iter().map(String::as_str).collect();
        // The defined class IRI: from the `defined_class` cell, or — under
        // --generate-defined-class — minted from base_IRI + a hash of the row.
        let dc_iri = match dc_idx.and_then(|i| cells.get(i)).map(|v| v.trim()) {
            Some(v) if !v.is_empty() => expand(&prefixes, v),
            _ if gopts.generate_defined_class => mint_defined_class(&pattern, &line),
            _ => continue,
        };
        ont.insert(Component::DeclareClass(DeclareClass(b.class(dc_iri.clone()))));

        // Raw cell value(s), split on `|`.
        let raw_cell = |var: &str| -> Vec<String> {
            let Some(idx) = col(var) else { return Vec::new() };
            let Some(raw) = cells.get(idx).map(|c| c.trim()) else { return Vec::new() };
            if raw.is_empty() {
                return Vec::new();
            }
            raw.split('|').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
        };
        // Whole (un-split) cell value of a column, if present and non-empty. Used
        // for OBO **override columns** (`defined_class_name`, …): when the data
        // table has such a column with a value, it overrides the field's
        // template, so a curator can hand-write one row's label or definition.
        let cell_value = |colname: &str| -> Option<String> {
            let v = col(colname).and_then(|i| cells.get(i)).map(|c| c.trim())?;
            (!v.is_empty()).then(|| v.to_string())
        };
        // Derived variables from `substitutions` and `internal_vars` (regex_sub
        // / join), computed once per row and consulted before the TSV columns.
        let derived = compute_derived(&pattern, &raw_cell);

        let is_list = |var: &str| {
            pattern.list_vars.contains_key(var) || pattern.data_list_vars.contains_key(var)
        };
        // A variable is a *data* variable when it is declared in `data_vars` /
        // `data_list_vars`, is regex-derived, OR is declared under `vars` /
        // `list_vars` with a **datatype range** (e.g. `usage_notes: xsd:string`):
        // DOSDP keys data-ness on the range, not the dictionary it sits in.
        let is_data = |var: &str| {
            derived.contains_key(var)
                || pattern.data_vars.contains_key(var)
                || pattern.data_list_vars.contains_key(var)
                || pattern.vars.get(var).is_some_and(|r| is_datatype_range(r))
                || pattern.list_vars.get(var).is_some_and(|r| is_datatype_range(r))
        };

        // Raw filler value(s) for a variable: a derived value, else the cell
        // (split on `|` for list vars). Data/derived vars stay literal; entity
        // vars are expanded to IRIs.
        let var_values = |var: &str| -> Vec<String> {
            if let Some(v) = derived.get(var) {
                return v.clone();
            }
            let parts = raw_cell(var);
            if parts.is_empty() {
                return Vec::new();
            }
            let parts = if is_list(var) { parts } else { parts.into_iter().take(1).collect() };
            parts.into_iter().map(|p| if is_data(var) { p } else { expand(&prefixes, &p) }).collect()
        };

        // A DOSDP variable must be *declared* to be usable: `var:`/`value:`
        // resolve only against the declared dictionaries, so a reference to a
        // bare TSV column contributes nothing. CL's `cyclingCellStates` pattern
        // has `annotationProperty: contributor` / `var: creator` with no
        // `creator` declaration anywhere, and so must yield no
        // `terms:contributor` axiom at all.
        let is_declared = |var: &str| {
            derived.contains_key(var)
                || pattern.vars.contains_key(var)
                || pattern.list_vars.contains_key(var)
                || pattern.data_vars.contains_key(var)
                || pattern.data_list_vars.contains_key(var)
        };

        let raw_values = |var: &str| -> Vec<String> {
            if let Some(v) = derived.get(var) {
                return v.clone();
            }
            raw_cell(var)
        };

        let ctx = RowCtx {
            b: &b,
            prefixes: &prefixes,
            names: &names,
            labels,
            var_labels: &gopts.var_labels,
            var_values: &var_values,
            is_declared: &is_declared,
            raw_values: &raw_values,
            is_data: &is_data,
            is_list: &is_list,
            annotation_index: &gopts.annotation_index,
            cell_value: &cell_value,
        };

        // ── Logical axioms ───────────────────────────────────────────────
        let mut logical: Vec<(String, AxiomTemplate)> = Vec::new();
        if let Some(t) = &pattern.equivalent_to {
            logical.push(("equivalentTo".into(), t.clone()));
        }
        if let Some(t) = &pattern.subclass_of {
            logical.push(("subClassOf".into(), t.clone()));
        }
        if let Some(t) = &pattern.disjoint_with {
            logical.push(("disjointWith".into(), t.clone()));
        }
        if let Some(t) = &pattern.gci {
            logical.push(("GCI".into(), t.clone()));
        }
        for la in &pattern.logical_axioms {
            logical.push((
                la.axiom_type.clone(),
                AxiomTemplate {
                    text: la.text.clone(),
                    vars: la.vars.clone(),
                    annotations: la.annotations.clone(),
                    multi_clause: la.multi_clause.clone(),
                },
            ));
        }

        // Per-row axiom restriction: a truthy `--restrict-axioms-column` cell
        // forces this row to logical axioms only.
        let row_restrict = match &gopts.restrict_axioms_column {
            Some(c) if col(c).and_then(|i| cells.get(i)).map(|v| is_truthy(v.trim())).unwrap_or(false) => {
                Restrict::Logical
            }
            _ => gopts.restrict_axioms,
        };
        // Optional per-axiom source annotation (= the pattern IRI).
        let source_ann: Option<Annotation<RcStr>> =
            if gopts.add_axiom_source_annotation {
                pattern.pattern_iri.as_ref().map(|piri| Annotation { ann: Default::default(),
                    ap: b.annotation_property(source_prop.clone()),
                    av: AnnotationValue::IRI(b.iri(expand(&prefixes, piri))),
                })
            } else {
                None
            };

        if row_restrict.allows_logical() {
            for (axiom_type, t) in &logical {
                emit_logical(&mut ont, &ctx, &dc_iri, axiom_type, t, source_ann.as_ref());
            }
            if let Some(ig) = &pattern.instance_graph {
                emit_instance_graph(&mut ont, &ctx, &dc_iri, ig);
            }
        }
        if !row_restrict.allows_annotation() {
            continue;
        }

        // Emit a single template field as one annotation per filled value, unless
        // its OBO **override column** is set in the data row (then that value wins,
        // template skipped). `name`/`comment`/`namespace`/`def`/`generated_*` have
        // override columns; the plain `*_synonym`/`xref` fields do not (`None`).
        let emit_field = |ont: &mut SetOntology<RcStr>, t: &Template, prop: &str, ovcol: Option<&str>| {
            // The axiom-annotation set: `xref` provenance plus any nested
            // `annotations:` (e.g. a `def`'s `xref: "AUTO:patterns/…"`).
            let mut ann = ctx.axiom_annotations(&t.annotations);
            for x in ctx.resolve_xrefs(&t.xrefs) {
                ann.insert(Annotation { ann: Default::default(),
                    ap: b.annotation_property(format!("{OBO_IN_OWL}hasDbXref")),
                    av: AnnotationValue::Literal(Literal::Simple { literal: x }),
                });
            }
            // An override column value wins over everything (when present).
            if let Some(v) = ovcol.and_then(cell_value) {
                assert_text_ann(&b, ont, &dc_iri, prop, &v, ann);
                return;
            }
            // `value`: a list variable → one annotation per item (e.g.
            // `exact_synonym: {value: exact_synonyms}`).
            if let Some(var) = &t.value {
                for v in (ctx.var_values)(var) {
                    let val = if (ctx.is_data)(var) {
                        v
                    } else {
                        ctx.display(var, &v)
                    };
                    assert_text_ann(&b, ont, &dc_iri, prop, &val, ann.clone());
                }
                return;
            }
            // `var`: an IRI-valued annotation (the filler IRI is the object).
            if let Some(var) = &t.var {
                if let Some(iri) = (ctx.var_values)(var).into_iter().next() {
                    ont.insert(AnnotatedComponent {
                        component: Component::AnnotationAssertion(AnnotationAssertion {
                            subject: AnnotationSubject::IRI(b.iri(dc_iri.clone())),
                            ann: Annotation { ann: Default::default(),
                                ap: b.annotation_property(prop.to_string()),
                                av: AnnotationValue::IRI(b.iri(iri)),
                            },
                        }),
                        ann,
                    });
                }
                return;
            }
            for text in ctx.fill_text_values(t) {
                assert_text_ann(&b, ont, &dc_iri, prop, &text, ann.clone());
            }
        };

        // ── OBO convenience annotation fields ────────────────────────────
        for (field, prop, ovcol) in [
            (&pattern.name, RDFS_LABEL, Some("defined_class_name")),
            (&pattern.comment, RDFS_COMMENT, Some("defined_class_comment")),
            (&pattern.namespace, &*format!("{OBO_IN_OWL}hasOBONamespace"), Some("defined_class_namespace")),
            (&pattern.exact_synonym, &*format!("{OBO_IN_OWL}hasExactSynonym"), None),
            (&pattern.narrow_synonym, &*format!("{OBO_IN_OWL}hasNarrowSynonym"), None),
            (&pattern.related_synonym, &*format!("{OBO_IN_OWL}hasRelatedSynonym"), None),
            (&pattern.broad_synonym, &*format!("{OBO_IN_OWL}hasBroadSynonym"), None),
            (&pattern.xref, &*format!("{OBO_IN_OWL}hasDbXref"), None),
        ] {
            if let Some(t) = field {
                emit_field(&mut ont, t, prop, ovcol);
            }
        }
        // `generated_*synonyms` (each a YAML list of templates) are processed
        // exactly like the regular `*_synonym` fields — same printf/multi_clause
        // path — because they land on the same annotation property; the only
        // distinction is that they have an override column. So a bare-`%s` list
        // var yields nothing, and per-item synonyms come only from
        // `multi_clause`; a raw IRI is never used as a synonym value.
        for (templates, prop, ovcol) in [
            (&pattern.generated_synonyms, &*format!("{OBO_IN_OWL}hasExactSynonym"), "defined_class_exact_synonym"),
            (&pattern.generated_narrow_synonyms, &*format!("{OBO_IN_OWL}hasNarrowSynonym"), "defined_class_narrow_synonym"),
            (&pattern.generated_broad_synonyms, &*format!("{OBO_IN_OWL}hasBroadSynonym"), "defined_class_broad_synonym"),
            (&pattern.generated_related_synonyms, &*format!("{OBO_IN_OWL}hasRelatedSynonym"), "defined_class_related_synonym"),
        ] {
            for t in templates {
                emit_field(&mut ont, t, prop, Some(ovcol));
            }
        }
        // `def` carries its own xref axiom-annotations.
        if let Some(t) = &pattern.def {
            emit_field(&mut ont, t, IAO_DEF, Some("defined_class_definition"));
        }

        // ── Free-form annotations list ───────────────────────────────────
        for ann in &pattern.annotations {
            emit_annotation(&mut ont, &ctx, &dc_iri, ann);
        }
    }

    // A row the pattern cannot fill produces NOTHING — not even a declaration.
    // The defined class is declared up front here (the loop needs it before it
    // knows whether the row will yield anything), so drop the declarations that
    // ended up standing alone. MP's `obstructedAnatomicalEntity` pattern reads a
    // var named `anatomical_space` from a table whose column is
    // `anatomical_entity`: all fourteen of its rows fill nothing, and without
    // this sweep `definitions.owl` would carry fourteen bare
    // `Declaration(Class(MP_…))` for classes it asserts nothing else about.
    {
        let mut referenced: std::collections::HashSet<String> = std::collections::HashSet::new();
        for ac in ont.iter() {
            if matches!(ac.component, Component::DeclareClass(_)) {
                continue;
            }
            referenced.extend(crate::sig::signature(&ac.component));
            for a in ac.ann.iter() {
                referenced.insert(a.ap.0.as_ref().to_string());
            }
        }
        let orphans: Vec<_> = ont
            .iter()
            .filter(|ac| match &ac.component {
                Component::DeclareClass(d) => !referenced.contains(d.0 .0.as_ref()),
                _ => false,
            })
            .cloned()
            .collect();
        for ac in orphans {
            ont.remove(&ac);
        }
    }

    // Emit a `Declaration(...)` for every entity in the generated ontology's
    // signature — the defined classes, the filler classes (CHEBI, …), the
    // relation object properties (RO, …), and the annotation properties used
    // (IAO_0000115, oboInOwl:hasExactSynonym). Built-in vocabulary (rdfs:label,
    // owl:Thing, xsd:string, …) is never declared. The set is exact, not merely
    // sufficient: released `definitions.owl` files carry precisely these
    // declarations, so a regenerated one has to be axiom-identical and not just
    // logically equivalent, or every rebuild is a whole-file diff.
    declare_signature(&mut ont, &b);

    let mut m = Model::from_parts(ont, prefixes);
    // The generated module declares only the structural prefixes and spells
    // every other IRI in full. That matters downstream, not here:
    // `om merge -i <module>… -o definitions.ofn` keeps the FIRST input's prefix
    // map, so a module carrying om's whole CURIE table would put `oio`, `dct`,
    // `LM`, `SLM` and friends into the released `patterns/definitions.owl`,
    // which should declare only `:`/owl/rdf/xml/xsd/rdfs.
    m.format_prefixes_cleared = true;
    Ok(m)
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

/// Per-row substitution context.
struct RowCtx<'a> {
    b: &'a Build<RcStr>,
    prefixes: &'a horned_owl::curie::PrefixMapping,
    names: &'a BTreeMap<String, String>,
    labels: &'a HashMap<String, String>,
    /// Per-variable display text (see [`GenerateOptions::var_labels`]).
    var_labels: &'a HashMap<String, String>,
    var_values: &'a dyn Fn(&str) -> Vec<String>,
    /// Whether a variable name is declared by the pattern (see `is_declared`).
    is_declared: &'a dyn Fn(&str) -> bool,
    /// Unexpanded cell value(s) (split on `|`) — used for xrefs, which stay as
    /// literal CURIE strings rather than being expanded to IRIs.
    raw_values: &'a dyn Fn(&str) -> Vec<String>,
    is_data: &'a dyn Fn(&str) -> bool,
    is_list: &'a dyn Fn(&str) -> bool,
    /// `permutations` index: filler IRI → (annotation property IRI → values).
    annotation_index: &'a HashMap<String, HashMap<String, Vec<String>>>,
    /// Whole (un-split) value of a data column, if present and non-empty — for
    /// `override` columns on free-form annotations.
    cell_value: &'a dyn Fn(&str) -> Option<String>,
}

impl RowCtx<'_> {
    /// How `var`'s filler prints in text: the variable's own override first, then
    /// the filler IRI's label, else the IRI itself.
    fn display(&self, var: &str, filler: &str) -> String {
        self.var_labels
            .get(var)
            .or_else(|| self.labels.get(filler))
            .cloned()
            .unwrap_or_else(|| filler.to_string())
    }

    /// Substitute entity short-names and `%s` fillers (or a `multi_clause`), then
    /// parse Manchester. A `multi_clause` whose clauses iterate a list variable
    /// yields several expressions; they are combined with `ObjectIntersectionOf`,
    /// because a list in a logical context is a conjunction, e.g.
    /// `(part_of some X) and (part_of some Y)`.
    fn build_ce(&self, t: &AxiomTemplate) -> Option<CE<RcStr>> {
        let texts = match &t.multi_clause {
            Some(mc) => self.fill_multi_clause(mc, true),
            None => self.substitute_logical(&t.text, &t.vars).into_iter().collect(),
        };
        let mut ces: Vec<CE<RcStr>> = Vec::new();
        for txt in &texts {
            ces.push(manchester::parse_class_expression(self.b, self.prefixes, txt)?);
        }
        match ces.len() {
            0 => None,
            1 => ces.pop(),
            _ => Some(CE::ObjectIntersectionOf(ces)),
        }
    }

    /// Replace `'name'`/bareword entity references with `<IRI>`. Longest name
    /// first, and *every* quoted form before *any* bareword form: one dictionary
    /// name is often a word-prefix of another (`cell` / `cell cycle process`),
    /// and substituting the short one as a bareword first would rewrite the
    /// inside of the longer quoted reference — `'cell cycle process'` becomes
    /// `'<…CL_0000000> cycle process'`, which then parses as a single bogus
    /// entity name.
    fn substitute_names(&self, text: &str) -> String {
        let mut names: Vec<(&String, &String)> = self.names.iter().collect();
        names.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
        let mut text = text.to_string();
        for (name, iri) in &names {
            text = text.replace(&format!("'{name}'"), &format!("<{}>", expand(self.prefixes, iri)));
        }
        for (name, iri) in &names {
            text = replace_word(&text, name, &format!("<{}>", expand(self.prefixes, iri)));
        }
        text
    }

    /// Replace `'name'`/bareword entity references with `<IRI>`, then each `%s`
    /// with its filler (a data var becomes a quoted literal). A **list** variable
    /// used via a bare `%s` (rather than a `multi_clause`) yields no axiom: a
    /// list expands only through `multi_clause`, and a bare `%s` has no one
    /// value to stand for the whole list.
    fn substitute_logical(&self, text: &str, vars: &[String]) -> Option<String> {
        if vars.iter().any(|v| (self.is_list)(v)) {
            return None;
        }
        let mut text = self.substitute_names(text);
        for var in vars {
            let vals = (self.var_values)(var);
            if vals.is_empty() {
                return None;
            }
            let sub = if (self.is_data)(var) {
                format!("\"{}\"", vals[0])
            } else {
                format!("<{}>", vals[0])
            };
            text = replace_first(&text, "%s", &sub);
        }
        Some(text)
    }

    /// Fill one clause occurrence: substitute `%s` positionally, using the value
    /// at `idx` for a repeating (list) var and the first value otherwise. In
    /// `logical` mode entities render as `<IRI>` / quoted literals; otherwise as
    /// labels / literals.
    fn fill_clause_once(&self, text: &str, vars: &[String], idx: usize, logical: bool) -> Option<String> {
        let mut text = if logical { self.substitute_names(text) } else { text.to_string() };
        for var in vars {
            let vals = (self.var_values)(var);
            if vals.is_empty() {
                return None;
            }
            let v = vals.get(idx).or_else(|| vals.first()).unwrap();
            let sub = if logical {
                if (self.is_data)(var) {
                    format!("\"{v}\"")
                } else {
                    format!("<{v}>")
                }
            } else if (self.is_data)(var) {
                v.clone()
            } else {
                self.display(var, v)
            };
            text = replace_first(&text, "%s", &sub);
        }
        Some(text)
    }

    /// Fill a `multi_clause`, returning the SET of filled strings. Each clause
    /// yields its own set (a list variable multiplies it — one filled string per
    /// item); the multi_clause result is the **cartesian product** of the
    /// clause-sets, each tuple joined by `sep`. So distinct clauses are joined by
    /// `sep`, while a list-var iteration produces separate results: an annotation
    /// emits one axiom per result, and a logical axiom conjoins the results (see
    /// [`build_ce`]).
    fn fill_multi_clause(&self, mc: &MultiClause, logical: bool) -> Vec<String> {
        let sep = mc.sep.clone().unwrap_or_else(|| " ".to_string());
        let mut acc: Vec<String> = Vec::new();
        let mut produced = false;
        for clause in &mc.clauses {
            // The clause's own text, filled — a list variable yields one variant
            // per item.
            let mut variants: Vec<String> = Vec::new();
            if let Some(text) = &clause.text {
                let repeat = clause
                    .vars
                    .iter()
                    .map(|v| (self.var_values)(v).len())
                    .max()
                    .unwrap_or(1)
                    .max(1);
                for i in 0..repeat {
                    if let Some(filled) = self.fill_clause_once(text, &clause.vars, i, logical) {
                        variants.push(filled);
                    }
                }
            }
            // Sub-clauses are each rendered (recursively) and **appended** to the
            // clause text, joined by `sep` — not treated as alternatives.
            let subs: Vec<String> = clause
                .sub_clauses
                .iter()
                .flat_map(|sub| self.fill_multi_clause(sub, logical))
                .collect();
            let clause_vals: Vec<String> = if variants.is_empty() {
                if subs.is_empty() {
                    continue;
                }
                vec![subs.join(&sep)]
            } else if subs.is_empty() {
                variants
            } else {
                variants
                    .into_iter()
                    .map(|v| {
                        std::iter::once(v).chain(subs.iter().cloned()).collect::<Vec<_>>().join(&sep)
                    })
                    .collect()
            };
            // Distinct clauses combine by cartesian product, each tuple joined by
            // `sep`; a single clause's list-var variants stay separate.
            acc = if !produced {
                clause_vals
            } else {
                let mut next = Vec::with_capacity(acc.len() * clause_vals.len());
                for a in &acc {
                    for c in &clause_vals {
                        next.push(format!("{a}{sep}{c}"));
                    }
                }
                next
            };
            produced = true;
        }
        acc
    }

    /// Fill a text template, returning the SET of filled strings (one per axiom).
    /// A `multi_clause` yields its cartesian set; a plain printf template yields a
    /// single string (data vars: their literal value; class vars: the filler's
    /// rdfs:label, falling back to its IRI). A **list** variable used via a bare
    /// `%s` (no `multi_clause`) yields nothing.
    fn fill_text_values(&self, t: &Template) -> Vec<String> {
        if let Some(mc) = &t.multi_clause {
            return self.fill_multi_clause(mc, false);
        }
        // `permutations`: substitute each var's label AND the filler term's values
        // of the listed annotation properties (from the supplied ontology),
        // combinatorially. With no permutations declared this is skipped; with an
        // empty index it degenerates to the plain label fill.
        if !t.permutations.is_empty() && !t.vars.iter().any(|v| (self.is_list)(v)) {
            return self.fill_permutations(&t.text, &t.vars, &t.permutations);
        }
        if t.vars.iter().any(|v| (self.is_list)(v)) {
            return Vec::new();
        }
        let mut text = t.text.clone();
        for var in &t.vars {
            let vals = (self.var_values)(var);
            if vals.is_empty() {
                return Vec::new();
            }
            let rendered = if (self.is_data)(var) {
                vals[0].clone()
            } else {
                self.display(var, &vals[0])
            };
            text = replace_first(&text, "%s", &rendered);
        }
        vec![text]
    }

    /// Fill a printf template with `permutations`: for each variable, the value
    /// set is its label (or literal) PLUS the filler term's values of the
    /// permutation's annotation properties (looked up in the ontology index by the
    /// filler IRI). The annotation texts are the cartesian product over the
    /// variables' value sets.
    fn fill_permutations(&self, text: &str, vars: &[String], perms: &[Permutation]) -> Vec<String> {
        let perm_by_var: std::collections::HashMap<&str, &Permutation> =
            perms.iter().map(|p| (p.var.as_str(), p)).collect();
        let mut value_lists: Vec<Vec<String>> = Vec::new();
        for var in vars {
            let fillers = (self.var_values)(var);
            let Some(filler) = fillers.first() else { return Vec::new() };
            let base = if (self.is_data)(var) {
                filler.clone()
            } else {
                self.display(var, filler)
            };
            let mut vals = vec![base];
            if let Some(p) = perm_by_var.get(var.as_str()) {
                if let Some(props) = self.annotation_index.get(filler) {
                    for prop_name in &p.annotation_properties {
                        let prop_iri = self
                            .names
                            .get(prop_name)
                            .map(|i| expand(self.prefixes, i))
                            .unwrap_or_else(|| expand(self.prefixes, prop_name));
                        if let Some(pv) = props.get(&prop_iri) {
                            for v in pv {
                                if !vals.contains(v) {
                                    vals.push(v.clone());
                                }
                            }
                        }
                    }
                }
            }
            value_lists.push(vals);
        }
        // Cartesian product of the per-variable value sets.
        let mut combos: Vec<Vec<String>> = vec![Vec::new()];
        for vl in &value_lists {
            let mut next = Vec::with_capacity(combos.len() * vl.len());
            for c in &combos {
                for v in vl {
                    let mut cc = c.clone();
                    cc.push(v.clone());
                    next.push(cc);
                }
            }
            combos = next;
        }
        let mut out: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for combo in combos {
            let mut s = text.to_string();
            for v in &combo {
                s = replace_first(&s, "%s", v);
            }
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
        out
    }

    /// Resolve `def`/annotation xref entries: a var name yields the row's
    /// value(s) (unexpanded, kept as literal CURIEs); anything else is taken as a
    /// literal xref string.
    fn resolve_xrefs(&self, xrefs: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        for x in xrefs {
            let vals = (self.raw_values)(x);
            if vals.is_empty() {
                out.push(x.clone());
            } else {
                out.extend(vals);
            }
        }
        out
    }

    /// Resolve an annotation-property reference (short name in a dictionary, a
    /// known OBO field name, or a CURIE/IRI) to a full IRI.
    fn resolve_ann_prop(&self, name: &str) -> String {
        if let Some(iri) = self.names.get(name) {
            return expand(self.prefixes, iri);
        }
        match name {
            "label" | "name" => RDFS_LABEL.to_string(),
            "comment" => RDFS_COMMENT.to_string(),
            "definition" | "def" => IAO_DEF.to_string(),
            "exact_synonym" => format!("{OBO_IN_OWL}hasExactSynonym"),
            "narrow_synonym" => format!("{OBO_IN_OWL}hasNarrowSynonym"),
            "related_synonym" => format!("{OBO_IN_OWL}hasRelatedSynonym"),
            "broad_synonym" => format!("{OBO_IN_OWL}hasBroadSynonym"),
            "xref" => format!("{OBO_IN_OWL}hasDbXref"),
            _ => expand(self.prefixes, name),
        }
    }

    /// Build the axiom-annotation set for a logical axiom's nested `annotations`.
    fn axiom_annotations(&self, anns: &[AnnotationDef]) -> std::collections::BTreeSet<Annotation<RcStr>> {
        let mut set = std::collections::BTreeSet::new();
        for a in anns {
            let Some(prop_name) = &a.annotation_property else { continue };
            let prop = self.resolve_ann_prop(prop_name);
            if let Some(text) = &a.text {
                let t = Template { text: text.clone(), vars: a.vars.clone(), xrefs: Vec::new(), multi_clause: a.multi_clause.clone(), permutations: a.permutations.clone(), value: None, var: None, annotations: Vec::new() };
                for filled in self.fill_text_values(&t) {
                    set.insert(Annotation { ann: Default::default(),
                        ap: self.b.annotation_property(prop.clone()),
                        av: AnnotationValue::Literal(Literal::Simple { literal: filled }),
                    });
                }
            } else if let Some(var) = a.value.as_ref().filter(|v| (self.is_declared)(v)) {
                // A nested `value:` naming a variable — one axiom annotation per
                // item, the same as `value:` on a top-level annotation. CL's
                // ExtendedDescription pattern needs it: `annotationProperty:
                // xref` / `value: pubs` (a data_list_var) is what carries the
                // `{xref="DOI:…"}` provenance on its several hundred
                // `terms:description` values.
                for v in (self.var_values)(var) {
                    let val = if (self.is_data)(var) {
                        v
                    } else {
                        self.display(var, &v)
                    };
                    set.insert(Annotation { ann: Default::default(),
                        ap: self.b.annotation_property(prop.clone()),
                        av: AnnotationValue::Literal(Literal::Simple { literal: val }),
                    });
                }
            } else if let Some(var) = a.var.as_ref().filter(|v| (self.is_declared)(v)) {
                if let Some(iri) = (self.var_values)(var).into_iter().next() {
                    set.insert(Annotation { ann: Default::default(),
                        ap: self.b.annotation_property(prop),
                        av: AnnotationValue::IRI(self.b.iri(iri)),
                    });
                }
            }
        }
        set
    }
}

/// Emit a logical axiom for a `dc` from a template, attaching any axiom
/// annotations. `disjointWith` becomes `DisjointClasses(dc, CE)`; `GCI` splits the
/// text on `SubClassOf`/`EquivalentTo` into a general inclusion between two
/// expressions; the rest produce equivalent/subclass axioms.
fn emit_logical(
    ont: &mut SetOntology<RcStr>,
    ctx: &RowCtx,
    dc_iri: &str,
    axiom_type: &str,
    t: &AxiomTemplate,
    source_ann: Option<&Annotation<RcStr>>,
) {
    let dc = || CE::Class(ctx.b.class(dc_iri.to_string()));
    let mut ann = ctx.axiom_annotations(&t.annotations);
    if let Some(s) = source_ann {
        ann.insert(s.clone());
    }
    let mut comp = match axiom_type {
        "GCI" => {
            // A general class inclusion between two arbitrary expressions.
            let text = match ctx.substitute_logical(&t.text, &t.vars) {
                Some(s) => s,
                None => return,
            };
            let (lhs, rhs, equiv) = if let Some((l, r)) = split_kw(&text, "SubClassOf") {
                (l, r, false)
            } else if let Some((l, r)) = split_kw(&text, "EquivalentTo") {
                (l, r, true)
            } else {
                return;
            };
            let (Some(lc), Some(rc)) = (
                manchester::parse_class_expression(ctx.b, ctx.prefixes, &lhs),
                manchester::parse_class_expression(ctx.b, ctx.prefixes, &rhs),
            ) else {
                return;
            };
            if equiv {
                Component::EquivalentClasses(EquivalentClasses(vec![lc, rc]))
            } else {
                Component::SubClassOf(SubClassOf { sub: lc, sup: rc })
            }
        }
        other => {
            let Some(ce) = ctx.build_ce(t) else { return };
            match other {
                "subClassOf" => Component::SubClassOf(SubClassOf { sub: dc(), sup: ce }),
                "disjointWith" => Component::DisjointClasses(DisjointClasses(vec![dc(), ce])),
                // default + "equivalentTo"
                _ => Component::EquivalentClasses(EquivalentClasses(vec![dc(), ce])),
            }
        }
    };
    canon_component(&mut comp);
    ont.insert(AnnotatedComponent { component: comp, ann });
}

/// Recursively put the operands of the commutative class-expression operators
/// (`ObjectIntersectionOf`, `ObjectUnionOf`) into a canonical order: sorted by
/// their OWL functional-syntax rendering. The operators are commutative, so the
/// order a pattern happens to write its clauses in carries no meaning; fixing
/// the order means a pattern with two relation clauses (e.g. `… some X and …
/// some Y`) serializes the same way every time, and a regenerated
/// `definitions.owl` diffs only where content really changed.
fn canon_ce(ce: &mut CE<RcStr>) {
    use horned_owl::io::ofn::writer::AsFunctional;
    match ce {
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => {
            for c in v.iter_mut() {
                canon_ce(c);
            }
            v.sort_by_cached_key(|c| c.as_functional().to_string());
        }
        CE::ObjectComplementOf(b) => canon_ce(b),
        CE::ObjectSomeValuesFrom { bce, .. }
        | CE::ObjectAllValuesFrom { bce, .. }
        | CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectMaxCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. } => canon_ce(bce),
        _ => {}
    }
}

/// Canonicalize every class expression carried by a class axiom (see
/// [`canon_ce`]). The axiom's own operand list is left as built.
fn canon_component(c: &mut Component<RcStr>) {
    match c {
        Component::EquivalentClasses(EquivalentClasses(v))
        | Component::DisjointClasses(DisjointClasses(v)) => {
            for ce in v.iter_mut() {
                canon_ce(ce);
            }
        }
        Component::SubClassOf(sc) => {
            canon_ce(&mut sc.sub);
            canon_ce(&mut sc.sup);
        }
        _ => {}
    }
}

/// Emit an annotation axiom (or several, for a list `value`) from an annotations
/// list entry.
fn emit_annotation(ont: &mut SetOntology<RcStr>, ctx: &RowCtx, dc_iri: &str, ann: &AnnotationDef) {
    let Some(prop_name) = &ann.annotation_property else { return };
    let prop = ctx.resolve_ann_prop(prop_name);
    let xrefs = ctx.resolve_xrefs(&ann.xrefs);
    // Axiom annotations: `xref` provenance plus any nested `annotations:`.
    let mut axiom_ann = ctx.axiom_annotations(&ann.annotations);
    for x in &xrefs {
        axiom_ann.insert(Annotation { ann: Default::default(),
            ap: ctx.b.annotation_property(format!("{OBO_IN_OWL}hasDbXref")),
            av: AnnotationValue::Literal(Literal::Simple { literal: x.clone() }),
        });
    }

    if let Some(var) = ann.value.as_ref().filter(|v| (ctx.is_declared)(v)) {
        // One annotation per list item.
        for v in (ctx.var_values)(var) {
            let val = if (ctx.is_data)(var) {
                v
            } else {
                ctx.display(var, &v)
            };
            assert_text_ann(ctx.b, ont, dc_iri, &prop, &val, axiom_ann.clone());
        }
    } else if let Some(var) = ann.var.as_ref().filter(|v| (ctx.is_declared)(v)) {
        // IRI-valued annotation (object is the filler IRI).
        if let Some(iri) = (ctx.var_values)(var).into_iter().next() {
            ont.insert(AnnotatedComponent {
                component: Component::AnnotationAssertion(AnnotationAssertion {
                    subject: AnnotationSubject::IRI(ctx.b.iri(dc_iri.to_string())),
                    ann: Annotation { ann: Default::default(),
                        ap: ctx.b.annotation_property(prop),
                        av: AnnotationValue::IRI(ctx.b.iri(iri)),
                    },
                }),
                ann: axiom_ann,
            });
        }
    } else if let Some(v) = ann.override_column.as_deref().and_then(|c| (ctx.cell_value)(c)) {
        // An explicit `override` column value supersedes the template.
        assert_text_ann(ctx.b, ont, dc_iri, &prop, &v, axiom_ann);
    } else if ann.text.is_some() || ann.multi_clause.is_some() {
        let t = Template {
            text: ann.text.clone().unwrap_or_default(),
            vars: ann.vars.clone(),
            xrefs: Vec::new(),
            multi_clause: ann.multi_clause.clone(),
            permutations: ann.permutations.clone(),
            value: None,
            var: None,
            annotations: Vec::new(),
        };
        for filled in ctx.fill_text_values(&t) {
            assert_text_ann(ctx.b, ont, dc_iri, &prop, &filled, axiom_ann.clone());
        }
    }
}

/// Like [`assert_text`] but with a pre-built axiom-annotation set (xrefs plus any
/// nested `annotations:`).
fn assert_text_ann(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    prop: &str,
    val: &str,
    ann: std::collections::BTreeSet<Annotation<RcStr>>,
) {
    ont.insert(AnnotatedComponent {
        component: Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(subj.to_string())),
            ann: Annotation { ann: Default::default(),
                ap: b.annotation_property(prop.to_string()),
                av: AnnotationValue::Literal(Literal::Simple { literal: val.to_string() }),
            },
        }),
        ann,
    });
}

/// Split a GCI text on a Manchester keyword (` SubClassOf `/` EquivalentTo `),
/// case-insensitively, into (lhs, rhs).
fn split_kw(text: &str, kw: &str) -> Option<(String, String)> {
    let lower = text.to_ascii_lowercase();
    let needle = format!(" {} ", kw.to_ascii_lowercase());
    let pos = lower.find(&needle)?;
    Some((text[..pos].trim().to_string(), text[pos + needle.len()..].trim().to_string()))
}

/// Compute derived variables for a row from `substitutions` (regex over one
/// input var) and `internal_vars` (regex_sub or join). Later entries may build
/// on earlier ones.
fn compute_derived(
    pattern: &Pattern,
    raw_cell: &dyn Fn(&str) -> Vec<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut derived: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let get = |derived: &BTreeMap<String, Vec<String>>, v: &str| -> Vec<String> {
        derived.get(v).cloned().unwrap_or_else(|| raw_cell(v))
    };
    let apply_re = |vals: Vec<String>, pat: &str, sub: &str| -> Vec<String> {
        match Regex::new(pat) {
            Ok(re) => vals.iter().map(|v| re.replace_all(v, sub).to_string()).collect(),
            Err(_) => vals,
        }
    };
    for s in &pattern.substitutions {
        let vals = get(&derived, &s.input);
        derived.insert(s.out.clone(), apply_re(vals, &s.match_, &s.sub));
    }
    for iv in &pattern.internal_vars {
        if let Some(j) = &iv.join {
            let sep = j.sep.clone().unwrap_or_default();
            let joined = j
                .vars
                .iter()
                .map(|v| get(&derived, v).first().cloned().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(&sep);
            derived.insert(iv.var.clone(), vec![joined]);
        } else if let (Some(inp), Some(m), Some(sub)) = (&iv.input, &iv.match_, &iv.sub) {
            let vals = get(&derived, inp);
            derived.insert(iv.var.clone(), apply_re(vals, m, sub));
        }
    }
    derived
}

/// Emit an instance graph: each node becomes a named individual (IRI derived
/// from the defined class) typed by its class/var filler, and each `[s, rel, o]`
/// edge becomes an object-property assertion.
fn emit_instance_graph(ont: &mut SetOntology<RcStr>, ctx: &RowCtx, dc_iri: &str, ig: &InstanceGraph) {
    use horned_owl::model::{
        ClassAssertion, Individual, NamedIndividual, ObjectPropertyAssertion,
        ObjectPropertyExpression as OPE,
    };
    let node_iri = |name: &str| format!("{dc_iri}#{name}");
    // Nodes → typed individuals.
    for (name, ty) in &ig.nodes {
        let ind = Individual::Named(NamedIndividual(ctx.b.iri(node_iri(name))));
        ont.insert(Component::DeclareNamedIndividual(
            horned_owl::model::DeclareNamedIndividual(NamedIndividual(ctx.b.iri(node_iri(name)))),
        ));
        // The type is a class short-name/CURIE or a variable filler.
        let class_iri = ctx
            .names
            .get(ty)
            .map(|i| expand(ctx.prefixes, i))
            .or_else(|| (ctx.var_values)(ty).into_iter().next())
            .unwrap_or_else(|| expand(ctx.prefixes, ty.trim_matches('\'')));
        ont.insert(Component::ClassAssertion(ClassAssertion {
            ce: CE::Class(ctx.b.class(class_iri)),
            i: ind,
        }));
    }
    // Edges → object-property assertions between node individuals.
    for edge in &ig.edges {
        if edge.len() != 3 {
            continue;
        }
        let (s, rel, o) = (&edge[0], &edge[1], &edge[2]);
        let rel_iri = ctx.names.get(rel).map(|i| expand(ctx.prefixes, i)).unwrap_or_else(|| expand(ctx.prefixes, rel));
        ont.insert(Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
            ope: OPE::ObjectProperty(ctx.b.object_property(rel_iri)),
            from: Individual::Named(NamedIndividual(ctx.b.iri(node_iri(s)))),
            to: Individual::Named(NamedIndividual(ctx.b.iri(node_iri(o)))),
        }));
    }
}

/// Whether a DOSDP variable range denotes a datatype (`xsd:*`, the XSD IRI, or
/// `rdfs:Literal`) rather than a class — used to classify a `vars`/`list_vars`
/// entry as a *data* variable (DOSDP keys data-ness on the range).
fn is_datatype_range(range: &str) -> bool {
    let r = range.trim().trim_matches('\'').trim();
    r.starts_with("xsd:")
        || r.starts_with("http://www.w3.org/2001/XMLSchema#")
        || r == "rdfs:Literal"
        || r == "http://www.w3.org/2000/01/rdf-schema#Literal"
}

fn is_truthy(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "t" | "y")
}

/// Mint a `defined_class` IRI from the pattern's `base_IRI` and a stable hash of
/// the data row (`--generate-defined-class`).
fn mint_defined_class(pattern: &Pattern, row: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    row.hash(&mut h);
    pattern.pattern_iri.hash(&mut h);
    let base = pattern
        .base_iri
        .clone()
        .or_else(|| pattern.pattern_iri.clone())
        .unwrap_or_else(|| "urn:dosdp:".to_string());
    format!("{base}{:016x}", h.finish())
}

fn expand(prefixes: &horned_owl::curie::PrefixMapping, s: &str) -> String {
    let s = s.trim();
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("urn:") {
        return s.to_string();
    }
    if let Ok(e) = prefixes.expand_curie_string(s) {
        return e;
    }
    crate::io::obo::expand_id(s)
}

fn replace_first(haystack: &str, needle: &str, with: &str) -> String {
    match haystack.find(needle) {
        Some(i) => format!("{}{}{}", &haystack[..i], with, &haystack[i + needle.len()..]),
        None => haystack.to_string(),
    }
}

/// Replace whole-word occurrences of `word` (not inside another identifier).
/// Whether `word` appears in `haystack` as a whole token — a DOSDP template
/// names a dictionary key either quoted (`'anatomical entity'`) or bare
/// (`part_of`), and a bare name must not match inside a longer identifier.
/// The entity names a class-expression text mentions, tokenized as a Manchester
/// parser reads them: a `'…'` run is one name whatever it contains, and outside
/// quotes each maximal run of name characters is a name. Keywords (`some`,
/// `and`, `that`, …) come out too — harmless, because the caller only asks
/// whether a name is a dictionary key.
fn expression_names(text: &str) -> std::collections::HashSet<String> {
    let mut out: std::collections::HashSet<String> = Default::default();
    let mut rest = text;
    while let Some(open) = rest.find('\'') {
        for w in split_names(&rest[..open]) {
            out.insert(w);
        }
        let after = &rest[open + 1..];
        match after.find('\'') {
            Some(close) => {
                out.insert(after[..close].to_string());
                rest = &after[close + 1..];
            }
            // An unbalanced quote: the remainder is not a quoted name.
            None => {
                for w in split_names(after) {
                    out.insert(w);
                }
                return out;
            }
        }
    }
    for w in split_names(rest) {
        out.insert(w);
    }
    out
}

/// Maximal runs of name characters (letters, digits, `_`, `:`, `-`, `.`, `/`,
/// `#`) — enough to keep a CURIE or an IRI in one piece.
fn split_names(s: &str) -> Vec<String> {
    s.split(|c: char| {
        !(c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.' | '/' | '#'))
    })
    .filter(|w| !w.is_empty())
    .map(str::to_string)
    .collect()
}

fn contains_word(haystack: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(i) = haystack[from..].find(word) {
        let start = from + i;
        let end = start + word.len();
        let before_ok = start == 0 || !is_name_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_name_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn replace_word(haystack: &str, word: &str, with: &str) -> String {
    let mut out = String::with_capacity(haystack.len());
    let bytes = haystack.as_bytes();
    let mut i = 0;
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < haystack.len() {
        if haystack[i..].starts_with(word) {
            let before_ok = i == 0 || !is_word(bytes[i - 1]);
            let after = i + word.len();
            let after_ok = after >= haystack.len() || !is_word(bytes[after]);
            if before_ok && after_ok {
                out.push_str(with);
                i = after;
                continue;
            }
        }
        let ch = haystack[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Dump the term IRIs referenced by a pattern and its data table: the
/// entity-dictionary IRIs, the defined classes, and the entity-reference fillers
/// in the data rows. Returns sorted, unique IRIs.
pub fn terms(pattern_yaml: &str, data_tsv: &str) -> Result<Vec<String>> {
    let pattern: Pattern =
        parse_pattern(pattern_yaml)?;
    let prefixes = dosdp_prefixes();
    let mut out: std::collections::BTreeSet<String> = Default::default();
    // Only the dictionary entries the LOGICAL axioms name — not every entry in
    // every dictionary. HPO's `fracturedAnatomicalEntity` declares
    // `anatomical_entity` with range `UBERON_0001062` and an `exact_synonym`
    // annotation, and neither belongs in its `.txt`: the five terms it emits are
    // exactly those its `equivalent_to` references. Emitting the whole
    // dictionary would seed the imports with `UBERON_0001062` and
    // `oboInOwl:hasExactSynonym`, dragging two terms the ⊥-module has no use for
    // into the extraction.
    let mut logical = String::new();
    for t in [&pattern.equivalent_to, &pattern.subclass_of, &pattern.gci] {
        if let Some(t) = t {
            logical.push(' ');
            logical.push_str(&t.text);
        }
    }
    for la in &pattern.logical_axioms {
        logical.push(' ');
        logical.push_str(&la.text);
    }
    // A dictionary KEY is what a template names (`'fractured'`, `part_of`); map
    // the ones that appear to their IRIs. What "appears" means is decided by the
    // expression's own tokens, not by a substring search: a `'…'` run is ONE name
    // however many words it holds. Searching for a bare word anywhere finds
    // `cell` inside `'cell cycle process'`, so CL's `cyclingCellStates` seeded the
    // imports with `CL:0000000` — a term its equivalence axiom never names.
    let names = expression_names(&logical);
    for dict in [
        &pattern.classes,
        &pattern.relations,
        &pattern.object_properties,
        &pattern.data_properties,
        &pattern.annotation_properties,
    ] {
        for (k, v) in dict.iter() {
            if names.contains(k.as_str()) {
                out.insert(expand(&prefixes, v));
            }
        }
    }
    // Entity-reference variable columns (not data vars) + defined_class are IRIs.
    // Data-ness is keyed on the RANGE, not on which map the variable is declared
    // in: OBA's `entity_attribute_location` declares `usage_notes` under `vars`
    // with range `xsd:string`, and its column holds a sentence. Read as an entity
    // column it becomes `obo:` + that sentence — a term in the pattern seed, and
    // so in the import seed.
    let entity_vars: std::collections::HashSet<&str> = pattern
        .vars
        .iter()
        .chain(pattern.list_vars.iter())
        .filter(|(_, range)| !is_datatype_range(range))
        .map(|(k, _)| k.as_str())
        .collect();
    // The same RFC 4180 reader `generate` uses, not a split on every tab: CL's
    // `ExtendedDescription.tsv` writes its descriptions as quoted fields that run
    // over 2,446 physical lines for 432 records, and splitting those lines gives
    // fragments of prose where a column should be — `obo:` + half a sentence,
    // each one a term in the pattern seed and so in the import seed.
    let records = parse_table_records(data_tsv, '\t');
    if let Some(hl) = records.first() {
        let header: Vec<&str> = hl.iter().map(|c| c.trim()).collect();
        for cells in records.iter().skip(1) {
            for (i, h) in header.iter().enumerate() {
                let is_dc = *h == "defined_class" || *h == "defined class";
                if !is_dc && !entity_vars.contains(*h) {
                    continue;
                }
                let Some(raw) = cells.get(i).map(|c| c.trim()) else { continue };
                for part in raw.split('|').map(str::trim).filter(|p| !p.is_empty()) {
                    out.insert(expand(&prefixes, part));
                }
            }
        }
    }
    // The list is a SET of IRIs, and a set's order is its hash trie's — not the
    // alphabet's. Collected in sorted order so the trie is built from the same
    // elements every run, then walked.
    let items: Vec<(String, i32)> =
        out.into_iter().map(|t| { let h = crate::owlapi_hash::java_string_hash(&t); (t, h) }).collect();
    Ok(crate::hash_trie::order(&items))
}

/// Generate prototypical axioms from a pattern with no data: each variable is
/// filled with its range class, and the defined class is the pattern IRI.
pub fn prototype(pattern_yaml: &str, labels: &HashMap<String, String>) -> Result<Model> {
    let pattern: Pattern =
        parse_pattern(pattern_yaml)?;
    let prefixes = dosdp_prefixes();
    // A pattern whose `pattern_iri` is missing — or is not an absolute IRI, as in
    // OBA's `pattern_iri: entity_homeostasis_trait.yaml` — has no defined class to
    // name, so a fixed placeholder stands in; resolving the relative form against
    // some base would invent an IRI the pattern never claimed.
    let dc = match pattern.pattern_iri.as_deref() {
        Some(iri) if iri.contains(':') => iri.to_string(),
        _ => "urn:dosdp:defined_class".to_string(),
    };
    let mut header = vec!["defined_class".to_string()];
    let mut row = vec![dc.clone()];
    // With no `--ontology` there are no real labels, so a filler in `name:`/`def:`
    // text renders as the var's RANGE EXPRESSION exactly as the pattern writes it
    // — `'behavior'`, quotes included — rather than as a bare IRI, which would
    // make the prototype unreadable. Seed those as labels (a supplied ontology's
    // label still wins).
    let mut var_labels: HashMap<String, String> = HashMap::new();
    for (var, range) in pattern.vars.iter().chain(pattern.list_vars.iter()) {
        header.push(var.clone());
        // …but only for a range that is not itself an identifier. A range written
        // as a CURIE (`cell: CL:0000000`) IS the filler, so the text shows the
        // IRI it expands to; a range written as a label (`'behavior'`) names
        // nothing on its own and stands in the text as written.
        let raw = range.trim();
        if !(raw.contains(':') || raw.starts_with("http")) {
            var_labels.insert(var.clone(), raw.to_string());
        }
        row.push(range_filler(&pattern, &prefixes, range));
    }
    // A data var stands in for itself: it is filled with its declared range
    // (`xsd:anyURI`, `xsd:string`), so the prototype shows what shape the column
    // takes.
    for (var, range) in pattern.data_vars.iter().chain(pattern.data_list_vars.iter()) {
        header.push(var.clone());
        row.push(range.trim().to_string());
    }
    let tsv = format!("{}\n{}\n", header.join("\t"), row.join("\t"));
    let gopts = GenerateOptions { var_labels, ..Default::default() };
    let mut model = generate_with(pattern_yaml, &tsv, labels, &gopts)?;
    // Each prototype is titled with the pattern's name — the one annotation
    // `prototype` adds that `generate` does not.
    if let Some(name) = pattern.pattern_name.as_ref() {
        // The title's subject is the pattern's OWN `pattern_iri`, written verbatim
        // even when it is not a valid absolute IRI — only the axioms fall back to
        // the `urn:dosdp:defined_class` placeholder.
        let iri = pattern.pattern_iri.as_ref().unwrap_or(&dc);
        let b = Build::new();
        model.ont.insert(AnnotatedComponent {
            component: Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(iri.clone())),
                ann: Annotation {
                    ann: Default::default(),
                    ap: b.annotation_property("http://purl.org/dc/terms/title"),
                    av: AnnotationValue::Literal(Literal::Datatype {
                        literal: name.clone(),
                        datatype_iri: b.iri("http://www.w3.org/2001/XMLSchema#string"),
                    }),
                },
            }),
            ann: Default::default(),
        });
        model.ont.insert(AnnotatedComponent {
            component: Component::DeclareAnnotationProperty(
                horned_owl::model::DeclareAnnotationProperty(
                    b.annotation_property("http://purl.org/dc/terms/title"),
                ),
            ),
            ann: Default::default(),
        });
    }
    Ok(type_annotation_literals_as_string(model))
}

/// Give every untyped annotation literal the `xsd:string` datatype.
///
/// `pattern.owl` spells the datatype out on every annotation literal, so it
/// reads `"Part of 'example'"^^xsd:string`. The generator builds bare
/// `Literal::Simple` values, which denote the same thing but render without the
/// datatype; only the `dcterms:title` [`prototype`] adds is typed already.
/// Retyping the rest keeps one document from mixing both spellings.
/// `definitions.owl` is written with the datatype implicit and is unaffected.
fn type_annotation_literals_as_string(model: Model) -> Model {
    use horned_owl::model::MutableOntology;
    let b: Build<crate::model::Str> = Build::new();
    let xsd = b.iri("http://www.w3.org/2001/XMLSchema#string");
    let retype = |av: &mut AnnotationValue<crate::model::Str>| {
        if let AnnotationValue::Literal(Literal::Simple { literal }) = av {
            *av = AnnotationValue::Literal(Literal::Datatype {
                literal: literal.clone(),
                datatype_iri: xsd.clone(),
            });
        }
    };
    let mut out = Model { ont: Default::default(), ..model.clone() };
    out.ont = horned_owl::ontology::set::SetOntology::new();
    for ac in model.ont.iter() {
        let mut ac = ac.clone();
        if let Component::AnnotationAssertion(aa) = &mut ac.component {
            retype(&mut aa.ann.av);
        }
        for a in std::mem::take(&mut ac.ann).into_iter() {
            let mut a = a;
            retype(&mut a.av);
            ac.ann.insert(a);
        }
        out.ont.insert(ac);
    }
    out
}

/// Every pattern YAML in a directory, sorted by filename — `prototype`'s
/// `--template` accepts a directory and renders the whole pattern set into one
/// ontology (a repo's `patterns/pattern.owl`).
fn pattern_files_in(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| anyhow!("reading template directory {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("yaml"))
        .collect();
    out.sort();
    Ok(out)
}

/// Resolve a variable's range expression to a single prototypical filler IRI.
fn range_filler(pattern: &Pattern, prefixes: &horned_owl::curie::PrefixMapping, range: &str) -> String {
    let r = range.trim().trim_matches('\'').trim();
    for dict in [&pattern.classes, &pattern.relations, &pattern.object_properties] {
        if let Some(iri) = dict.get(r) {
            return expand(prefixes, iri);
        }
    }
    if r.contains(':') || r.starts_with("http") {
        return expand(prefixes, r);
    }
    "http://www.w3.org/2002/07/owl#Thing".to_string()
}

/// Namespace for the placeholder classes that stand in for pattern variables
/// while unifying the pattern's logical template against an ontology.
const VAR_NS: &str = "https://www.ebi.ac.uk/spot/owlmake/dosdp/var/";

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
/// pattern's primary logical axiom becomes a template with a placeholder per
/// variable, which is unified against each class's equivalent/subclass axioms.
/// Returns the bound fillers (as IRIs).
pub fn query(pattern_yaml: &str, ontology: &Model) -> Result<QueryResult> {
    let pattern: Pattern =
        parse_pattern(pattern_yaml)?;
    let prefixes = dosdp_prefixes();
    let b = Build::new();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for dict in [
        &pattern.classes,
        &pattern.relations,
        &pattern.object_properties,
        &pattern.data_properties,
        &pattern.annotation_properties,
    ] {
        for (k, v) in dict {
            names.insert(k.clone(), v.clone());
        }
    }

    // Pick the primary logical axiom (equivalentTo preferred, then subClassOf).
    let (atype, text, vars) = pick_primary(&pattern)
        .ok_or_else(|| anyhow!("pattern has no equivalentTo/subClassOf logical axiom to query"))?;

    // Build the template CE: substitute entity short-names, then replace each
    // `%s` with a placeholder var class.
    let mut tt = substitute_names_in(&names, &prefixes, &text);
    for v in &vars {
        tt = replace_first(&tt, "%s", &format!("<{VAR_NS}{v}>"));
    }
    let template = manchester::parse_class_expression(&b, &prefixes, &tt)
        .ok_or_else(|| anyhow!("could not parse the pattern's logical template: {tt}"))?;

    let want_equiv = atype == "equivalentTo";
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
                row.push(binds.get(v).cloned().unwrap_or_default());
            }
            rows.insert(row);
        }
    }

    let mut columns = vec!["defined_class".to_string()];
    columns.extend(vars);
    Ok(QueryResult { columns, rows: rows.into_iter().collect() })
}

/// The pattern's primary logical axiom as (axiom_type, text, vars).
fn pick_primary(pattern: &Pattern) -> Option<(String, String, Vec<String>)> {
    if let Some(t) = &pattern.equivalent_to {
        return Some(("equivalentTo".into(), t.text.clone(), t.vars.clone()));
    }
    if let Some(t) = &pattern.subclass_of {
        return Some(("subClassOf".into(), t.text.clone(), t.vars.clone()));
    }
    pattern
        .logical_axioms
        .iter()
        .find(|la| matches!(la.axiom_type.as_str(), "equivalentTo" | "subClassOf"))
        .map(|la| (la.axiom_type.clone(), la.text.clone(), la.vars.clone()))
}

fn named_class(ce: &CE<RcStr>) -> Option<&str> {
    match ce {
        CE::Class(c) => Some(c.0.as_ref()),
        _ => None,
    }
}

fn substitute_names_in(
    names: &BTreeMap<String, String>,
    prefixes: &horned_owl::curie::PrefixMapping,
    text: &str,
) -> String {
    let mut text = text.to_string();
    for (name, iri) in names {
        let full = format!("<{}>", expand(prefixes, iri));
        text = text.replace(&format!("'{name}'"), &full);
        text = replace_word(&text, name, &full);
    }
    text
}

/// Unify a template class expression (with `VAR_NS` placeholder classes) against
/// a target expression, recording variable → filler IRI bindings. Supports
/// classes, existential restrictions, and intersections (matched as sets).
fn unify(template: &CE<RcStr>, target: &CE<RcStr>, binds: &mut BTreeMap<String, String>) -> bool {
    match template {
        CE::Class(c) if c.0.as_ref().starts_with(VAR_NS) => {
            let var = c.0.as_ref()[VAR_NS.len()..].to_string();
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
    use horned_owl::model::ObjectPropertyExpression as OPE;
    let ope = |o: &OPE<RcStr>| match o {
        OPE::ObjectProperty(p) => p.0.as_ref().to_string(),
        OPE::InverseObjectProperty(p) => format!("inverse {}", p.0.as_ref()),
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
        CE::Class(c) => c.0.as_ref().starts_with(VAR_NS),
        CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => has_var(bce),
        CE::ObjectIntersectionOf(ps) | CE::ObjectUnionOf(ps) => ps.iter().any(has_var),
        CE::ObjectComplementOf(b) => has_var(b),
        _ => false,
    }
}

/// Render a simple Markdown document for a pattern.
pub fn document(pattern_yaml: &str) -> Result<String> {
    let pattern: Pattern =
        parse_pattern(pattern_yaml)?;
    let mut s = String::new();
    let name = pattern.pattern_name.clone().unwrap_or_else(|| "Pattern".to_string());
    s.push_str(&format!("# {name}\n\n"));
    if let Some(iri) = &pattern.pattern_iri {
        s.push_str(&format!("IRI: `{iri}`\n\n"));
    }
    if let Some(d) = &pattern.description {
        s.push_str(&format!("{d}\n\n"));
    }
    let all_vars: Vec<(&String, &String)> = pattern
        .vars
        .iter()
        .chain(pattern.list_vars.iter())
        .chain(pattern.data_vars.iter())
        .collect();
    if !all_vars.is_empty() {
        s.push_str("## Variables\n\n");
        for (v, range) in all_vars {
            s.push_str(&format!("- `{v}`: {range}\n"));
        }
        s.push('\n');
    }
    if let Some((atype, text, _)) = pick_primary(&pattern) {
        s.push_str("## Logical axiom\n\n");
        s.push_str(&format!("- **{atype}**: `{text}`\n\n"));
    }
    Ok(s)
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
            eprintln!("dosdp: {e}");
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
    matches!(s, "generate" | "terms" | "query" | "prototype" | "document" | "validate")
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
    let flag = |names: &[&str]| -> bool { cli_flag(rest, names) };

    let template = val(&["--template", "--pattern", "-t"]);
    let read_template = || -> Result<String> {
        let p = template.clone().ok_or_else(|| anyhow!("--template is required"))?;
        Ok(std::fs::read_to_string(&p).map_err(|e| anyhow!("reading template {p}: {e}"))?)
    };
    let outfile = val(&["--outfile", "--output", "-o"]);

    match sub.as_str() {
        "generate" => {
            let csv = val(&["--table-format"]).as_deref() == Some("csv");
            // One ontology load yields both the label map and the permutation
            // index, and it follows `owl:imports` through `--catalog` — that is
            // where the fillers' labels live.
            let catalog = val(&["--catalog", "-c"]);
            let annotation_index = annotation_index_from_with_catalog(
                val(&["--ontology", "--input", "-i"]).as_deref(),
                catalog.as_deref().map(std::path::Path::new),
            )?;
            let labels: HashMap<String, String> = annotation_index
                .iter()
                .filter_map(|(iri, props)| {
                    // The lexicographic MINIMUM of a term's labels — not the
                    // first one the axiom order happens to yield, which would
                    // make the generated text depend on load order.
                    props.get(RDFS_LABEL).and_then(|v| v.iter().min()).map(|l| (iri.clone(), l.clone()))
                })
                .collect();
            // `--prefixes FILE`: a YAML CURIE map (e.g. `config/prefixes.yaml`).
            let extra_prefixes: Vec<(String, String)> = val(&["--prefixes"])
                .and_then(|p| std::fs::read_to_string(&p).ok())
                .and_then(|t| {
                    serde_yaml::from_str::<std::collections::BTreeMap<String, String>>(&t).ok()
                })
                .map(|m| m.into_iter().collect())
                .unwrap_or_default();
            let gopts = GenerateOptions {
                restrict_axioms: val(&["--restrict-axioms-to"]).map(|s| Restrict::parse(&s)).unwrap_or_default(),
                restrict_axioms_column: val(&["--restrict-axioms-column"]),
                add_axiom_source_annotation: flag(&["--add-axiom-source-annotation"]),
                axiom_source_annotation_property: val(&["--axiom-source-annotation-property"]),
                generate_defined_class: flag(&["--generate-defined-class"]),
                annotation_index,
                extra_prefixes,
                var_labels: HashMap::new(),
            };
            let read_data = |path: &str| -> Result<String> {
                let mut d = std::fs::read_to_string(path).map_err(|e| anyhow!("reading infile {path}: {e}"))?;
                if csv {
                    d = csv_to_tsv(&d);
                }
                Ok(d)
            };
            // Batch mode (`--batch-patterns`): for each pattern NAME,
            // template = <template-dir>/NAME.yaml, data = <infile-dir>/NAME.tsv,
            // output = <outfile-dir>/NAME.ofn.
            if let Some(batch) = val(&["--batch-patterns"]) {
                // In batch mode the template DIRECTORY is usually spelled
                // `--template` (OBA: `--template=../patterns/dosdp-patterns
                // --batch-patterns="…"`), so accept either that or the explicit
                // `--template-dir`.
                let tdir = val(&["--template-dir"])
                    .or_else(|| val(&["--template", "--pattern", "-t"]))
                    .ok_or_else(|| {
                        anyhow!("--template-dir (or --template) is required with --batch-patterns")
                    })?;
                let indir = val(&["--infile", "--data"]).ok_or_else(|| anyhow!("--infile (directory) is required with --batch-patterns"))?;
                let outdir = outfile.clone().ok_or_else(|| anyhow!("--outfile (directory) is required with --batch-patterns"))?;
                std::fs::create_dir_all(&outdir).ok();
                for name in batch.split([' ', ',']).map(str::trim).filter(|n| !n.is_empty()) {
                    let pat = std::fs::read_to_string(format!("{tdir}/{name}.yaml"))
                        .map_err(|e| anyhow!("reading template {tdir}/{name}.yaml: {e}"))?;
                    let data = read_data(&format!("{indir}/{name}.tsv"))?;
                    let mut model = generate_with(&pat, &data, &labels, &gopts)?;
                    crate::io::save(&mut model, std::path::Path::new(&format!("{outdir}/{name}.ofn")))?;
                    eprintln!("dosdp generate: wrote {outdir}/{name}.ofn");
                }
                return Ok(0);
            }
            let pattern = read_template()?;
            let infile = val(&["--infile", "--data"]).ok_or_else(|| anyhow!("--infile is required"))?;
            let data = read_data(&infile)?;
            let mut model = generate_with(&pattern, &data, &labels, &gopts)?;
            write_model(&mut model, outfile.as_deref())?;
            Ok(0)
        }
        "prototype" => {
            let labels = labels_from(
                val(&["--ontology", "--input", "-i"]).as_deref(),
                val(&["--catalog", "-c"]).as_deref().map(std::path::Path::new),
            )?;
            let tpath = template.clone().ok_or_else(|| anyhow!("--template is required"))?;
            let tpath = std::path::Path::new(&tpath);
            let mut model = if tpath.is_dir() {
                // A directory renders the whole pattern set into one ontology,
                // named `urn:unnamed:ontology#ont1` — the IRI committed
                // `patterns/pattern.owl` files already carry, so regenerating
                // one is not a whole-file diff.
                let mut merged = crate::model::Model::default();
                for f in pattern_files_in(tpath)? {
                    let text = std::fs::read_to_string(&f)
                        .map_err(|e| anyhow!("reading template {}: {e}", f.display()))?;
                    let m = prototype(&text, &labels)
                        .map_err(|e| anyhow!("pattern {}: {e}", f.display()))?;
                    merge_into(&mut merged, m);
                }
                type_literals_as_xsd_string(&mut merged);
                crate::cmd::annotate::annotate(
                    merged,
                    Some("urn:unnamed:ontology#ont1"),
                    None,
                    &[],
                    &[],
                    false,
                )?
            } else {
                prototype(&read_template()?, &labels)?
            };
            if tpath.is_dir() {
                model.prefixes = crate::io::robot_ofn_prefixes(&model);
                // Bind `:` to the ontology IRI VERBATIM — no trailing `#`
                // appended. The default `:` derived on the line above is the
                // ontology IRI with `#` appended, so binding it explicitly here
                // is what keeps that `#` out of the emitted prefix line.
                let _ = model.prefixes.add_prefix("", "urn:unnamed:ontology#ont1");
            }
            // `pattern.owl` renders `^^xsd:string` explicitly. The switch is
            // process-wide, so turn it off again immediately — a `definitions.owl`
            // written later in the same `om make` run must not gain the datatype.
            horned_owl::io::ofn::writer::set_write_xsd_string(true);
            let r = write_model(&mut model, outfile.as_deref());
            horned_owl::io::ofn::writer::set_write_xsd_string(false);
            r?;
            Ok(0)
        }
        "terms" => {
            let pattern = read_template()?;
            let mut data = val(&["--infile", "--data"])
                .map(|p| std::fs::read_to_string(&p).map_err(|e| anyhow!("reading infile {p}: {e}")))
                .transpose()?
                .unwrap_or_default();
            if val(&["--table-format"]).as_deref() == Some("csv") {
                data = csv_to_tsv(&data);
            }
            let terms = terms(&pattern, &data)?;
            write_text(format!("{}\n", terms.join("\n")), outfile.as_deref())?;
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
            if flag(&["--print-query"]) {
                if let Some((atype, text, vars)) = pick_primary(
                    &parse_pattern(&pattern)?,
                ) {
                    eprintln!("dosdp query: structural match on {atype} `{text}` binding {vars:?}");
                }
            }
            let result = query(&pattern, &model)?;
            write_text(result.to_tsv(), outfile.as_deref())?;
            Ok(0)
        }
        "document" => {
            let pattern = read_template()?;
            write_text(document(&pattern)?, outfile.as_deref())?;
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

/// Whether a boolean flag is present. A bare flag is presence-only (`true`); the
/// next token is consumed as the value only if it is an explicit boolean literal
/// (so `--flag --other` does not read `--other` as the flag's value).
fn cli_flag(args: &[String], names: &[&str]) -> bool {
    for (i, a) in args.iter().enumerate() {
        for n in names {
            if a == n {
                return match args.get(i + 1) {
                    Some(v) if is_bool_word(v) => is_truthy(v),
                    _ => true,
                };
            }
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return is_truthy(v);
            }
        }
    }
    false
}

fn is_bool_word(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "1" | "0" | "t" | "f" | "y" | "n"
    )
}

fn csv_to_tsv(data: &str) -> String {
    data.lines().map(|l| l.replace(',', "\t")).collect::<Vec<_>>().join("\n")
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
            props.get(RDFS_LABEL).and_then(|v| v.iter().min()).map(|l| (iri.clone(), l.clone()))
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
            props.get(RDFS_LABEL).and_then(|v| v.iter().min()).map(|l| (iri.clone(), l.clone()))
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
    let mut idx: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    if let Some(p) = path {
        let p = std::path::Path::new(p);
        let mut m = crate::io::load(p)?;
        if let Some(cat) = catalog {
            crate::cmd::merge_import_closure(&mut m, cat, Some(p))?;
        }
        let m = m;
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
                    idx.entry(s.as_ref().to_string())
                        .or_default()
                        .entry(aa.ann.ap.0.as_ref().to_string())
                        .or_default()
                        .push(val);
                }
            }
        }
    }
    Ok(idx)
}

fn labels_from(
    path: Option<&str>,
    catalog: Option<&std::path::Path>,
) -> Result<HashMap<String, String>> {
    let mut labels = HashMap::new();
    if let Some(p) = path {
        let p = std::path::Path::new(p);
        let mut m = crate::io::load(p)?;
        if let Some(cat) = catalog {
            crate::cmd::merge_import_closure(&mut m, cat, Some(p))?;
        }
        for ac in m.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                    if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                        (&aa.subject, &aa.ann.av)
                    {
                        let t = match lit {
                            Literal::Simple { literal }
                            | Literal::Language { literal, .. }
                            | Literal::Datatype { literal, .. } => literal.clone(),
                        };
                        // Keep the lexicographic minimum among all of a term's
                        // labels, so the choice does not depend on axiom order.
                        labels
                            .entry(s.as_ref().to_string())
                            .and_modify(|cur: &mut String| {
                                if t < *cur {
                                    *cur = t.clone();
                                }
                            })
                            .or_insert(t);
                    }
                }
            }
        }
    }
    Ok(labels)
}

/// [`type_literals_as_xsd_string`] for a model the caller owns.
pub(crate) fn typed_as_xsd_string(mut model: Model) -> Model {
    type_literals_as_xsd_string(&mut model);
    model
}

/// Retype every plain literal as `xsd:string`. The merged prototype document
/// types its annotation literals explicitly, while the generator produces bare
/// `Literal::Simple`, which denotes the same thing but renders without the
/// datatype.
pub(crate) fn type_literals_as_xsd_string(model: &mut Model) {
    use horned_owl::model::{AnnotatedComponent, Component, MutableOntology};
    let b = Build::new();
    let xsd = b.iri("http://www.w3.org/2001/XMLSchema#string");
    let retype = |av: &mut AnnotationValue<_>| {
        if let AnnotationValue::Literal(Literal::Simple { literal }) = av {
            *av = AnnotationValue::Literal(Literal::Datatype {
                literal: std::mem::take(literal),
                datatype_iri: xsd.clone(),
            });
        }
    };
    let comps: Vec<AnnotatedComponent<_>> = model.ont.iter().cloned().collect();
    for old in comps {
        let mut new = old.clone();
        if let Component::AnnotationAssertion(aa) = &mut new.component {
            retype(&mut aa.ann.av);
        }
        // Axiom annotations too — a DOSDP `annotations:` entry with an `xref`
        // renders as `Annotation(oio:hasDbXref "…"^^xsd:string)` on the assertion.
        let axiom_anns: Vec<_> = new
            .ann
            .iter()
            .cloned()
            .map(|mut a| {
                retype(&mut a.av);
                a
            })
            .collect();
        new.ann = axiom_anns.into_iter().collect();
        if new != old {
            model.ont.remove(&old);
            model.ont.insert(new);
        }
    }
}

/// Merge `other`'s axioms and prefixes into `model`, dropping its ontology header.
fn merge_into(model: &mut Model, other: Model) {
    use horned_owl::model::{Component, MutableOntology};
    for ac in other.ont.iter() {
        if matches!(
            ac.component,
            Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_)
        ) {
            continue;
        }
        model.ont.insert(ac.clone());
    }
    for (prefix, value) in other.prefixes.mappings() {
        let _ = model.prefixes.add_prefix(prefix, value);
    }
}

fn write_model(model: &mut Model, outfile: Option<&str>) -> Result<()> {
    // Always serialize OWL Functional Syntax, whatever the outfile is named — a
    // repo's `patterns/pattern.owl` and `definitions.owl` are both functional
    // under a `.owl` name. Writing RDF/XML for a `.owl` suffix would not round-trip
    // identically (reification absorbs a plain assertion that duplicates an
    // annotated one).
    match outfile {
        Some(p) => {
            crate::io::save_as(model, std::path::Path::new(p), crate::io::Format::Functional)?
        }
        None => {
            let mut buf = Vec::new();
            crate::io::write_to_ref(model, &mut buf, crate::io::Format::Functional)?;
            print!("{}", String::from_utf8_lossy(&buf));
        }
    }
    Ok(())
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
