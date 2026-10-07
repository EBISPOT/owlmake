//! A pattern rendered over a row of its data table.
//!
//! A row binds each variable whose cell holds anything once trimmed: a `vars`
//! or `data_vars` variable to that text, a `list_vars` or `data_list_vars`
//! variable to the set of its `|`-separated items, each trimmed. An internal
//! variable binds the items of its list joined, a substitution binds what it
//! rewrites another binding into, `defined_class` binds the row's defined
//! class, and every other column binds its cell, trimmed, whatever it holds.
//!
//! Logical text reads the bindings as the row writes them, each name in it
//! resolved through the pattern's dictionaries (see [`PatternNames`]).
//! Annotation text reads an entity variable's readable identifier in place of
//! the IRI its cell names (see [`Renderer::label_of`]), and an internal
//! variable with each CURIE in it replaced the same way.
//!
//! Bindings, and the set of values a list variable binds, iterate as Scala's
//! immutable maps and sets do (see [`super::scala_map_key_order`]): that order
//! decides which list variable a template naming several of them repeats over,
//! and the order a clause's repetitions are joined in.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{anyhow, bail, Context, Result};
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, Build,
    ClassExpression as CE, Component, DisjointClasses, EquivalentClasses, Literal, RcStr, SubClassOf,
};

use crate::java_number;

use super::table::Row;
use super::{
    expression, java, least, ordered_keys, scala_map_key_order, scala_set_order, AnnotationDef, AxiomType, Clause,
    Function, ListAnnotationObo, MultiClause, Pattern, Permutation, Restrict, Substitution, Template,
};

const OBO_IN_OWL: &str = "http://www.geneontology.org/formats/oboInOwl#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const IAO_DEF: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
/// The variable a row's defined class is bound to.
pub(super) const DEFINED_CLASS: &str = "defined_class";
/// The namespace of a variable's placeholder IRI.
pub(super) const VARIABLE_NS: &str = "urn:dosdp:";

/// A variable's placeholder IRI: [`VARIABLE_NS`] and its name, each space an
/// underscore. The defined class of a row that names none is
/// `urn:dosdp:defined_class`.
pub(super) fn variable_iri(name: &str) -> String {
    format!("{VARIABLE_NS}{}", name.replace(' ', "_"))
}

// ── Text ────────────────────────────────────────────────────────────────────

/// `s` without the characters up to U+0020 at either end.
pub(super) fn java_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

fn has_line_terminator(s: &str) -> bool {
    s.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}'])
}

/// The inside of `'…'`, when no line break is in it.
fn quoted_inner(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('\'')?.strip_suffix('\'')?;
    (!has_line_terminator(inner)).then_some(inner)
}

/// The inside of `<…>`, when it holds something and no line break.
fn bracketed_iri(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('<')?.strip_suffix('>')?;
    (!inner.is_empty() && !has_line_terminator(inner)).then_some(inner)
}

/// Whether `s` is `http` and at least one character more, none a line break.
fn is_http(s: &str) -> bool {
    s.strip_prefix("http").is_some_and(|rest| !rest.is_empty() && !has_line_terminator(rest))
}

/// `s` as `prefix:local`: the prefix runs to the first colon, and the local
/// part holds no line break.
fn curie(s: &str) -> Option<(&str, &str)> {
    let (prefix, local) = s.split_once(':')?;
    (!has_line_terminator(local)).then_some((prefix, local))
}

/// A list cell's items: the text split at each `|`, without the empty items it
/// ends with. A text with no `|` is one item, empty or not.
pub(super) fn split_list(cell: &str) -> Vec<&str> {
    if !cell.contains('|') {
        return vec![cell];
    }
    let mut items: Vec<&str> = cell.split('|').collect();
    while items.last().is_some_and(|i| i.is_empty()) {
        items.pop();
    }
    items
}

/// A cell's text trimmed, when anything is left.
fn strip(s: &str) -> Option<String> {
    let t = java_trim(s);
    (!t.is_empty()).then(|| t.to_string())
}

// ── Prefixes ────────────────────────────────────────────────────────────────

/// How a CURIE becomes an IRI: through the prefixes a prefix file names, then,
/// with OBO prefixes, through the standard ones (`rdf`, `rdfs`, `owl`, `xsd`,
/// `dc`, `dct`, `skos`, `obo`, `oio`, `oboInOwl`) and any other prefix as an
/// OBO ontology's namespace (`GO` → `http://purl.obolibrary.org/obo/GO_`).
#[derive(Clone, Debug, Default)]
pub struct Prefixes {
    named: HashMap<String, String>,
    obo: bool,
}

impl Prefixes {
    pub fn new(named: impl IntoIterator<Item = (String, String)>, obo: bool) -> Prefixes {
        Prefixes { named: named.into_iter().collect(), obo }
    }

    /// No named prefixes, and OBO prefixes.
    pub fn obo() -> Prefixes {
        Prefixes::new([], true)
    }

    /// The prefixes a prefix file names: one YAML document (see
    /// [`super::yaml_documents`]) mapping prefix to namespace, each a string.
    pub fn read_file(path: &std::path::Path) -> Result<Vec<(String, String)>> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading the prefixes {}", path.display()))?;
        let not_a_mapping = || format!("the prefixes {} are not a mapping of prefix to namespace", path.display());
        let mut documents = super::yaml_documents(&text).with_context(not_a_mapping)?.into_iter();
        let (Some(document), None) = (documents.next(), documents.next()) else {
            bail!(not_a_mapping());
        };
        let map: BTreeMap<String, String> = super::decode(document).with_context(not_a_mapping)?;
        Ok(map.into_iter().collect())
    }

    /// The namespace `prefix` stands for.
    pub(super) fn namespace(&self, prefix: &str) -> Option<String> {
        if let Some(ns) = self.named.get(prefix) {
            return Some(ns.clone());
        }
        if !self.obo {
            return None;
        }
        Some(match prefix {
            "rdf" => "http://www.w3.org/1999/02/22-rdf-syntax-ns#".to_string(),
            "rdfs" => "http://www.w3.org/2000/01/rdf-schema#".to_string(),
            "owl" => "http://www.w3.org/2002/07/owl#".to_string(),
            "xsd" => "http://www.w3.org/2001/XMLSchema#".to_string(),
            "dc" => "http://purl.org/dc/elements/1.1/".to_string(),
            "dct" => "http://purl.org/dc/terms/".to_string(),
            "skos" => "http://www.w3.org/2004/02/skos/core#".to_string(),
            "obo" => "http://purl.obolibrary.org/obo/".to_string(),
            "oio" | "oboInOwl" => OBO_IN_OWL.to_string(),
            other => format!("http://purl.obolibrary.org/obo/{other}_"),
        })
    }

    /// The IRI `id` names: text that is `http` and at least one character more
    /// is an IRI as written, `prefix:local` a CURIE (see
    /// [`namespace`](Self::namespace)); any other text names none.
    pub fn iri(&self, id: &str) -> Option<String> {
        if is_http(id) {
            return Some(id.to_string());
        }
        let (prefix, local) = curie(id)?;
        self.namespace(prefix).map(|ns| format!("{ns}{local}"))
    }
}

// ── Names ───────────────────────────────────────────────────────────────────

/// What a name in a pattern's logical text stands for. A class, individual or
/// datatype name is `'$var'` for the variable's placeholder, a name in quotes
/// for the name inside them, `<…>` for the IRI inside, an IRI or a CURIE for
/// the IRI it names (see [`Prefixes::iri`]), and any other name for the IRI its
/// entry in the `classes` dictionary names (datatypes have no dictionary). A
/// property name, quoted or not, is an entry of its dictionary alone.
pub(super) struct PatternNames<'a> {
    classes: &'a BTreeMap<String, String>,
    object_properties: BTreeMap<String, String>,
    data_properties: &'a BTreeMap<String, String>,
    annotation_properties: BTreeMap<String, String>,
    prefixes: &'a Prefixes,
}

static NO_NAMES: BTreeMap<String, String> = BTreeMap::new();

impl<'a> PatternNames<'a> {
    fn new(pattern: &'a Pattern, prefixes: &'a Prefixes) -> PatternNames<'a> {
        let mut object_properties = pattern.relations.clone();
        object_properties.extend(pattern.object_properties.clone());
        // A readable identifier names itself as an annotation property.
        let mut annotation_properties = pattern.annotation_properties.clone();
        for id in pattern.readable_identifiers.iter().flatten() {
            annotation_properties.insert(id.clone(), id.clone());
        }
        PatternNames {
            classes: &pattern.classes,
            object_properties,
            data_properties: &pattern.data_properties,
            annotation_properties,
            prefixes,
        }
    }

    fn name_or_variable(&self, name: &str, dict: &BTreeMap<String, String>) -> Option<String> {
        if let Some(inner) = quoted_inner(name) {
            if let Some(var) = inner.strip_prefix('$').filter(|v| !v.is_empty()) {
                return Some(variable_iri(var));
            }
            return self.name_or_variable(inner, dict);
        }
        if let Some(iri) = bracketed_iri(name) {
            return Some(iri.to_string());
        }
        if is_http(name) || curie(name).is_some() {
            return self.prefixes.iri(name);
        }
        dict.get(name).and_then(|v| self.prefixes.iri(v))
    }

    fn dictionary(&self, name: &str, dict: &BTreeMap<String, String>) -> Option<String> {
        let key = quoted_inner(name).unwrap_or(name);
        dict.get(key).and_then(|v| self.prefixes.iri(v))
    }

    fn annotation_property(&self, name: &str) -> Option<String> {
        self.dictionary(name, &self.annotation_properties)
    }
}

impl expression::Names for PatternNames<'_> {
    fn class(&self, name: &str) -> Option<String> {
        self.name_or_variable(name, self.classes)
    }

    fn object_property(&self, name: &str) -> Option<String> {
        self.dictionary(name, &self.object_properties)
    }

    fn data_property(&self, name: &str) -> Option<String> {
        self.dictionary(name, self.data_properties)
    }

    fn individual(&self, name: &str) -> Option<String> {
        self.name_or_variable(name, self.classes)
    }

    fn datatype(&self, name: &str) -> Option<String> {
        self.name_or_variable(name, &NO_NAMES)
    }
}

// ── Bindings ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Binding {
    Single(String),
    /// Distinct values, in the order a Scala set of them iterates.
    Multi(Vec<String>),
}

/// Variable bindings, iterated as a Scala immutable map built by inserting
/// them in order iterates.
#[derive(Clone, Debug, Default)]
struct Bindings {
    order: Vec<String>,
    values: HashMap<String, Binding>,
}

impl Bindings {
    /// Bind `key`; a key already bound keeps its place.
    fn insert(&mut self, key: &str, value: Binding) {
        if !self.values.contains_key(key) {
            self.order.push(key.to_string());
        }
        self.values.insert(key.to_string(), value);
    }

    /// Bind each of `other`'s keys, in its order.
    fn extend(&mut self, other: &Bindings) {
        for k in other.keys() {
            self.insert(k, other.values[k].clone());
        }
    }

    fn get(&self, key: &str) -> Option<&Binding> {
        self.values.get(key)
    }

    fn single(&self, key: &str) -> Option<&str> {
        match self.values.get(key) {
            Some(Binding::Single(v)) => Some(v),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        scala_map_key_order(&self.order).into_iter().map(|i| self.order[i].as_str()).collect()
    }

    /// The single-valued bindings, in iteration order.
    fn singles(&self) -> Bindings {
        let mut out = Bindings::default();
        for k in self.keys() {
            if let b @ Binding::Single(_) = &self.values[k] {
                out.insert(k, b.clone());
            }
        }
        out
    }

    /// The first list binding, in iteration order, of a variable in `vars`.
    fn first_multi(&self, vars: &[String]) -> Option<(String, Vec<String>)> {
        self.keys().into_iter().filter(|k| vars.iter().any(|v| v == k)).find_map(|k| match &self.values[k] {
            Binding::Multi(v) => Some((k.to_string(), v.clone())),
            Binding::Single(_) => None,
        })
    }
}

type AC = AnnotatedComponent<RcStr>;

/// `axioms` without repeats, each where it first comes.
fn distinct(axioms: Vec<AC>) -> Vec<AC> {
    let mut out: Vec<AC> = Vec::with_capacity(axioms.len());
    for ac in axioms {
        if !out.contains(&ac) {
            out.push(ac);
        }
    }
    out
}

/// The distinct `values` in Scala set order.
fn set_of(values: impl IntoIterator<Item = String>) -> Vec<String> {
    scala_set_order(values.into_iter().collect())
}

// ── Rendering ───────────────────────────────────────────────────────────────

/// An annotation a pattern asks for, its property resolved to an IRI.
#[derive(Clone, PartialEq)]
enum Normalized {
    Printf {
        prop: String,
        text: Option<String>,
        vars: Option<Vec<String>>,
        multi_clause: Option<MultiClause>,
        override_column: Option<String>,
        sub: Vec<Normalized>,
        /// Each permuted variable and its properties; a later entry for a
        /// variable replaces an earlier one.
        permutations: Vec<(String, Vec<String>)>,
    },
    List {
        prop: String,
        value: String,
        sub: Vec<Normalized>,
    },
    IriValue {
        prop: String,
        var: String,
        sub: Vec<Normalized>,
    },
}

/// The values the readable-identifier properties give a term in the supplied
/// ontology: `rdfs:label` from `labels`, any other from `index`.
pub(super) struct Readable<'a> {
    pub(super) labels: &'a HashMap<String, String>,
    pub(super) index: &'a HashMap<String, HashMap<String, Vec<String>>>,
}

/// How a run treats each row.
pub(super) struct RowOptions<'a> {
    pub(super) restrict_axioms: Restrict,
    pub(super) restrict_axioms_column: Option<&'a str>,
    pub(super) generate_defined_class: bool,
}

/// A logical template, in the order a row's logical axioms are made: `GCI`,
/// each of `logical_axioms`, `equivalentTo`, `subClassOf`, `disjointWith`.
struct Logical<'a> {
    kind: AxiomType,
    /// Whether it is an entry of `logical_axioms`, whose annotations are
    /// resolved whether or not the row fills it.
    listed: bool,
    text: Option<&'a str>,
    vars: Option<&'a [String]>,
    multi_clause: Option<&'a MultiClause>,
    annotations: Result<Vec<Normalized>, String>,
}

/// A logical template read with each variable its placeholder: its kind, its
/// class expression and its variables.
pub(super) type Primary = (AxiomType, CE<RcStr>, Vec<String>);

pub(super) struct Renderer<'a> {
    b: &'a Build<RcStr>,
    pattern: &'a Pattern,
    names: PatternNames<'a>,
    prefixes: &'a Prefixes,
    readable: Readable<'a>,
    /// Whether data variables fill logical text, unquoted, and `permutations`
    /// add annotations.
    data_in_logic: bool,
    /// The readable-identifier properties, in order.
    readable_props: Result<Vec<String>, String>,
    substitutions: Vec<(java::Regex, &'a Substitution)>,
    group_finder: java::Regex,
    curie_list: java::Regex,
    /// Each variable dictionary's keys in the order its map iterates.
    var_order: Vec<String>,
    list_order: Vec<String>,
    data_order: Vec<String>,
    data_list_order: Vec<String>,
    /// Every variable the pattern declares.
    declared: HashSet<&'a str>,
    /// The variables whose values fill logical text unquoted.
    unquoted: HashSet<&'a str>,
    logical: Vec<Logical<'a>>,
    /// The OBO fields and `annotations`, resolved.
    annotation_fields: Result<Vec<Normalized>, String>,
}

impl<'a> Renderer<'a> {
    pub(super) fn new(
        b: &'a Build<RcStr>,
        pattern: &'a Pattern,
        pattern_yaml: &str,
        prefixes: &'a Prefixes,
        readable: Readable<'a>,
        data_in_logic: bool,
    ) -> Result<Renderer<'a>> {
        let names = PatternNames::new(pattern, prefixes);
        let readable_props = match &pattern.readable_identifiers {
            Some(ids) => ids
                .iter()
                .map(|id| names.annotation_property(id).ok_or_else(|| format!("no annotation property is named `{id}`")))
                .collect(),
            None => Ok(vec![RDFS_LABEL.to_string()]),
        };
        let substitutions = pattern
            .substitutions
            .iter()
            .map(|s| {
                java::Regex::new(&s.match_)
                    .map(|re| (re, s))
                    .map_err(|e| anyhow!("the substitution of `{}` into `{}`: {e}", s.input, s.out))
            })
            .collect::<Result<_>>()?;
        let declared: HashSet<&str> = [&pattern.vars, &pattern.list_vars, &pattern.data_vars, &pattern.data_list_vars]
            .into_iter()
            .flat_map(|d| d.keys().map(String::as_str))
            .collect();
        let unquoted: HashSet<&str> = if data_in_logic {
            pattern.data_vars.keys().chain(pattern.data_list_vars.keys()).map(String::as_str).collect()
        } else {
            HashSet::new()
        };
        let mut r = Renderer {
            b,
            pattern,
            names,
            prefixes,
            readable,
            data_in_logic,
            readable_props,
            substitutions,
            group_finder: java::Regex::new(r"\\(\d+)").map_err(|e| anyhow!(e))?,
            curie_list: java::Regex::new("([^ ,:]*):([^ ,]*)").map_err(|e| anyhow!(e))?,
            var_order: ordered_keys(pattern_yaml, "vars", &pattern.vars),
            list_order: ordered_keys(pattern_yaml, "list_vars", &pattern.list_vars),
            data_order: ordered_keys(pattern_yaml, "data_vars", &pattern.data_vars),
            data_list_order: ordered_keys(pattern_yaml, "data_list_vars", &pattern.data_list_vars),
            declared,
            unquoted,
            logical: Vec::new(),
            annotation_fields: Ok(Vec::new()),
        };
        r.logical = r.logical_templates();
        r.annotation_fields = r.normalized_fields();
        Ok(r)
    }

    /// Whether the readable-identifier properties are all annotation
    /// properties the pattern names.
    pub(super) fn check_readable(&self) -> Result<()> {
        self.readable_props.as_ref().map(|_| ()).map_err(|e| anyhow!("readable_identifiers: {e}"))
    }

    fn logical_templates(&self) -> Vec<Logical<'a>> {
        let p: &'a Pattern = self.pattern;
        let mut out = Vec::new();
        let convenience = |kind: AxiomType, t: &'a Option<super::AxiomTemplate>| {
            t.as_ref().map(|t| Logical {
                kind,
                listed: false,
                text: t.text.as_deref(),
                vars: t.vars.as_deref(),
                multi_clause: t.multi_clause.as_ref(),
                annotations: self.normalize_all(t.annotations.as_deref().unwrap_or(&[])),
            })
        };
        out.extend(convenience(AxiomType::Gci, &p.gci));
        for la in &p.logical_axioms {
            out.push(Logical {
                kind: la.axiom_type,
                listed: true,
                text: la.text.as_deref(),
                vars: la.vars.as_deref(),
                multi_clause: la.multi_clause.as_ref(),
                annotations: self.normalize_all(la.annotations.as_deref().unwrap_or(&[])),
            });
        }
        out.extend(convenience(AxiomType::EquivalentTo, &p.equivalent_to));
        out.extend(convenience(AxiomType::SubClassOf, &p.subclass_of));
        out.extend(convenience(AxiomType::DisjointWith, &p.disjoint_with));
        out
    }

    /// The OBO fields, then `annotations`. Two OBO fields whose templates are
    /// the same are one field, the later in this order: `name`, `comment`,
    /// `def`, `namespace`, the four list synonyms, `xref`, the four generated
    /// synonyms.
    fn normalized_fields(&self) -> Result<Vec<Normalized>, String> {
        let p: &'a Pattern = self.pattern;
        #[derive(PartialEq)]
        enum Obo<'t> {
            Printf(&'t Template),
            List(&'t ListAnnotationObo),
        }
        let oio = |local: &str| format!("{OBO_IN_OWL}{local}");
        let printf = |t: &'a Option<Template>| -> Vec<Obo<'a>> { t.iter().map(Obo::Printf).collect() };
        let list = |t: &'a Option<ListAnnotationObo>| -> Vec<Obo<'a>> { t.iter().map(Obo::List).collect() };
        let generated = |ts: &'a [Template]| -> Vec<Obo<'a>> {
            let mut out: Vec<Obo<'a>> = Vec::new();
            for t in ts {
                push_unique(&mut out, Obo::Printf(t));
            }
            out
        };
        let fields: Vec<(Vec<Obo<'a>>, String, Option<&str>)> = vec![
            (printf(&p.name), RDFS_LABEL.to_string(), Some("defined_class_name")),
            (printf(&p.comment), RDFS_COMMENT.to_string(), Some("defined_class_comment")),
            (printf(&p.def), IAO_DEF.to_string(), Some("defined_class_definition")),
            (printf(&p.namespace), oio("hasOBONamespace"), Some("defined_class_namespace")),
            (list(&p.exact_synonym), oio("hasExactSynonym"), None),
            (list(&p.narrow_synonym), oio("hasNarrowSynonym"), None),
            (list(&p.related_synonym), oio("hasRelatedSynonym"), None),
            (list(&p.broad_synonym), oio("hasBroadSynonym"), None),
            (list(&p.xref), oio("hasDbXref"), None),
            (generated(&p.generated_synonyms), oio("hasExactSynonym"), Some("defined_class_exact_synonym")),
            (generated(&p.generated_narrow_synonyms), oio("hasNarrowSynonym"), Some("defined_class_narrow_synonym")),
            (generated(&p.generated_broad_synonyms), oio("hasBroadSynonym"), Some("defined_class_broad_synonym")),
            (generated(&p.generated_related_synonyms), oio("hasRelatedSynonym"), Some("defined_class_related_synonym")),
        ];
        let same = |a: &[Obo], b: &[Obo]| a.len() == b.len() && a.iter().all(|x| b.contains(x));
        let xref = |var: &Option<String>| -> Vec<Normalized> {
            var.iter().map(|v| Normalized::List { prop: oio("hasDbXref"), value: v.clone(), sub: Vec::new() }).collect()
        };
        let mut out: Vec<Normalized> = Vec::new();
        for (i, (templates, prop, column)) in fields.iter().enumerate() {
            if fields[i + 1..].iter().any(|(later, _, _)| same(templates, later)) {
                continue;
            }
            for t in templates {
                let n = match t {
                    Obo::Printf(t) => {
                        let mut sub = self.normalize_all(t.annotations.as_deref().unwrap_or(&[]))?;
                        for x in xref(&t.xrefs) {
                            push_unique(&mut sub, x);
                        }
                        Normalized::Printf {
                            prop: prop.clone(),
                            text: t.text.clone(),
                            vars: t.vars.clone(),
                            multi_clause: t.multi_clause.clone(),
                            override_column: column.map(str::to_string),
                            sub,
                            permutations: self.permutations(t.permutations.as_deref(), t.vars.as_deref())?,
                        }
                    }
                    Obo::List(l) => Normalized::List { prop: prop.clone(), value: l.value.clone(), sub: xref(&l.xrefs) },
                };
                push_unique(&mut out, n);
            }
        }
        for a in &p.annotations {
            push_unique(&mut out, self.normalize(a)?);
        }
        Ok(out)
    }

    fn normalize_all(&self, anns: &[AnnotationDef]) -> Result<Vec<Normalized>, String> {
        let mut out = Vec::new();
        for a in anns {
            push_unique(&mut out, self.normalize(a)?);
        }
        Ok(out)
    }

    fn property(&self, name: &str) -> Result<String, String> {
        self.names.annotation_property(name).ok_or_else(|| format!("no annotation property is named `{name}`"))
    }

    fn normalize(&self, a: &AnnotationDef) -> Result<Normalized, String> {
        Ok(match a {
            AnnotationDef::List { annotations, annotation_property, value } => Normalized::List {
                prop: self.property(annotation_property)?,
                value: value.clone(),
                sub: self.normalize_all(annotations.as_deref().unwrap_or(&[]))?,
            },
            AnnotationDef::Iri { annotations, annotation_property, var } => Normalized::IriValue {
                prop: self.property(annotation_property)?,
                var: var.clone(),
                sub: self.normalize_all(annotations.as_deref().unwrap_or(&[]))?,
            },
            AnnotationDef::Printf {
                annotations,
                annotation_property,
                text,
                vars,
                override_column,
                multi_clause,
                permutations,
            } => {
                let prop = self.property(annotation_property)?;
                let sub = self.normalize_all(annotations.as_deref().unwrap_or(&[]))?;
                Normalized::Printf {
                    prop,
                    text: text.clone(),
                    vars: vars.clone(),
                    multi_clause: multi_clause.clone(),
                    override_column: override_column.clone(),
                    sub,
                    permutations: self.permutations(permutations.as_deref(), vars.as_deref())?,
                }
            }
        })
    }

    /// A template's permutations, each property resolved. Each permuted
    /// variable has to be one of the template's `vars`.
    fn permutations(
        &self,
        perms: Option<&[Permutation]>,
        vars: Option<&[String]>,
    ) -> Result<Vec<(String, Vec<String>)>, String> {
        let perms = perms.unwrap_or(&[]);
        let vars = vars.unwrap_or(&[]);
        let missing: Vec<&str> =
            perms.iter().map(|p| p.var.as_str()).filter(|v| !vars.iter().any(|x| x == v)).collect();
        if !missing.is_empty() {
            return Err(format!(
                "a permutation names {}, which the template's vars ({}) do not",
                missing.join(", "),
                vars.join(", ")
            ));
        }
        perms
            .iter()
            .map(|p| {
                let props = p
                    .annotation_properties
                    .iter()
                    .map(|n| {
                        self.names
                            .annotation_property(n)
                            .ok_or_else(|| format!("a permutation names `{n}`, and no annotation property is named so"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((p.var.clone(), props))
            })
            .collect()
    }

    // ── A row ────────────────────────────────────────────────────────────

    /// The readable identifier of `iri`: the value the first readable-identifier
    /// property gives it in the supplied ontology (the least, where it gives
    /// several), else the row's own label for it, else the IRI itself.
    fn label_of(&self, iri: &str, row_labels: &HashMap<String, String>) -> String {
        for p in self.readable_props.as_deref().unwrap_or(&[]) {
            let value = if p == RDFS_LABEL {
                self.readable.labels.get(iri).cloned()
            } else {
                self.readable.index.get(iri).and_then(|m| m.get(p)).and_then(least).cloned()
            };
            if let Some(v) = value {
                return v;
            }
        }
        row_labels.get(iri).cloned().unwrap_or_else(|| iri.to_string())
    }

    /// An entity binding as annotation text reads it: each value that names an
    /// IRI as that IRI's readable identifier.
    fn labelled(&self, b: &Binding, row_labels: &HashMap<String, String>) -> Binding {
        let one = |v: &String| match self.prefixes.iri(v) {
            Some(iri) => self.label_of(&iri, row_labels),
            None => v.clone(),
        };
        match b {
            Binding::Single(v) => Binding::Single(one(v)),
            Binding::Multi(vs) => Binding::Multi(set_of(vs.iter().map(one))),
        }
    }

    /// An internal variable's value as annotation text reads it: each
    /// `prefix:local` in it whose prefix names a namespace replaced, read as a
    /// regular expression, by the readable identifier of the IRI it names.
    fn labelled_curies(&self, value: &str, row_labels: &HashMap<String, String>) -> Result<String> {
        let mut resolved = value.to_string();
        for m in self.curie_list.find_all(value).map_err(|e| anyhow!(e))? {
            let (prefix, local) = (m.group(1).unwrap_or(""), m.group(2).unwrap_or(""));
            let Some(ns) = self.prefixes.namespace(prefix) else { continue };
            let label = self.label_of(&format!("{ns}{local}"), row_labels);
            let re = java::Regex::new(&format!("{prefix}:{local}"))
                .map_err(|e| anyhow!("the internal variable value `{value}`: {e}"))?;
            resolved = re.replace_first(&resolved, &label).map_err(|e| anyhow!(e))?;
        }
        Ok(resolved)
    }

    /// `value` substituted: when `re` matches in it, `sub` with each `\N` in it
    /// replaced by what group N of the first match matched (that text read as
    /// a replacement, `$` and `\` included); `value` itself when `re` does not
    /// match.
    fn substitute(&self, re: &java::Regex, sub: &str, value: &str) -> Result<String> {
        let Some(m) = re.find(value).map_err(|e| anyhow!(e))? else { return Ok(value.to_string()) };
        let mut out = String::new();
        let mut last = 0;
        for gm in self.group_finder.find_all(sub).map_err(|e| anyhow!(e))? {
            out.push_str(&sub[last..gm.start()]);
            let digits = gm.group(1).unwrap_or("");
            let n = java_number::parse_int(digits)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| anyhow!("`{sub}`: `\\{digits}` is no group number"))?;
            if n > re.group_count() {
                bail!("`{sub}` refers to group {n}, and its pattern has {}", re.group_count());
            }
            let text = m.group(n).ok_or_else(|| anyhow!("`{sub}`: group {n} took no part in matching `{value}`"))?;
            out.push_str(&self.group_finder.expand(&gm, text).map_err(|e| anyhow!(e))?);
            last = gm.end();
        }
        out.push_str(&sub[last..]);
        Ok(out)
    }

    /// The axioms `row` generates: its logical axioms and its annotation
    /// assertions, each without repeats (see [`Renderer::logical_axioms`] for
    /// the order of the first; the second come in the order of the fields that
    /// make them).
    pub(super) fn render(&self, row: &Row, opts: &RowOptions) -> Result<(Vec<AC>, Vec<AC>)> {
        let p = self.pattern;
        let cell = |var: &str| row.get(var).and_then(strip);
        let mut var_bindings = Bindings::default();
        let mut row_labels: HashMap<String, String> = HashMap::new();
        for v in &self.var_order {
            let Some(filler) = cell(v) else { continue };
            if let (Some(iri), Some(label)) = (self.prefixes.iri(&filler), cell(&format!("{v}_label"))) {
                row_labels.insert(iri, label);
            }
            var_bindings.insert(v, Binding::Single(filler));
        }
        let lists = |order: &[String]| {
            let mut out = Bindings::default();
            for v in order {
                if let Some(filler) = cell(v) {
                    let items = split_list(&filler).into_iter().map(|i| java_trim(i).to_string());
                    out.insert(v, Binding::Multi(set_of(items)));
                }
            }
            out
        };
        let singles = |order: &[String]| {
            let mut out = Bindings::default();
            for v in order {
                if let Some(filler) = cell(v) {
                    out.insert(v, Binding::Single(filler));
                }
            }
            out
        };
        let list_bindings = lists(&self.list_order);
        let data_bindings = singles(&self.data_order);
        let data_list_bindings = lists(&self.data_list_order);
        let mut internal = Bindings::default();
        for iv in &p.internal_vars {
            let Some(f) = &iv.apply else { continue };
            let items: &[String] = match data_list_bindings.get(&iv.input).or_else(|| list_bindings.get(&iv.input)) {
                Some(Binding::Multi(v)) => v,
                _ => &[],
            };
            let value = match f {
                Function::Join { join } => {
                    let joined = items.join(&join.sep);
                    if joined.is_empty() {
                        continue;
                    }
                    joined
                }
                Function::Regex { .. } => String::new(),
            };
            internal.insert(&iv.var_name, Binding::Single(value));
        }
        // Every column no variable is declared for, in the row's own order.
        let columns: Vec<String> = row.cells.iter().map(|(c, _)| c.clone()).collect();
        let mut additional = Bindings::default();
        for i in scala_map_key_order(&columns) {
            let (column, value) = &row.cells[i];
            if !self.declared.contains(column.as_str()) {
                additional.insert(column, Binding::Single(java_trim(value).to_string()));
            }
        }

        let defined_class = if opts.generate_defined_class {
            let pattern_iri = p
                .pattern_iri
                .as_deref()
                .and_then(|i| self.prefixes.iri(i))
                .ok_or_else(|| anyhow!("generating defined classes needs the pattern's `pattern_iri`, naming an IRI"))?;
            let mut minted = var_bindings.clone();
            minted.extend(&list_bindings);
            minted.extend(&data_bindings);
            minted.extend(&data_list_bindings);
            minted.extend(&internal);
            defined_iri(&pattern_iri, &minted)
        } else {
            let value = row.get(DEFINED_CLASS).ok_or_else(|| anyhow!("a row has no `{DEFINED_CLASS}` cell"))?;
            java_trim(value).to_string()
        };
        let iri_binding = Binding::Single(defined_class);

        let mut logical = var_bindings.clone();
        logical.insert(DEFINED_CLASS, iri_binding.clone());
        logical.extend(&list_bindings);
        if self.data_in_logic {
            logical.extend(&data_bindings);
            logical.extend(&data_list_bindings);
        }

        let mut annotation = Bindings::default();
        for k in var_bindings.keys() {
            annotation.insert(k, self.labelled(&var_bindings.values[k], &row_labels));
        }
        for k in list_bindings.keys() {
            annotation.insert(k, self.labelled(&list_bindings.values[k], &row_labels));
        }
        for k in internal.keys() {
            if let Binding::Single(v) = &internal.values[k] {
                annotation.insert(k, Binding::Single(self.labelled_curies(v, &row_labels)?));
            }
        }
        annotation.extend(&data_bindings);
        annotation.extend(&data_list_bindings);
        annotation.insert(DEFINED_CLASS, iri_binding);
        for (re, s) in &self.substitutions {
            let out = match annotation.get(&s.input) {
                Some(Binding::Single(v)) => Binding::Single(self.substitute(re, &s.sub, v)?),
                Some(Binding::Multi(vs)) => Binding::Multi(set_of(
                    vs.iter().map(|v| self.substitute(re, &s.sub, v)).collect::<Result<Vec<_>>>()?,
                )),
                None => continue,
            };
            annotation.insert(&s.out, out);
        }
        annotation.extend(&additional);

        let kinds = match opts.restrict_axioms_column.and_then(|c| row.get(c)).and_then(strip) {
            Some(v) => Restrict::parse(&v).context("the restrict-axioms column")?,
            None => opts.restrict_axioms,
        };
        let logical_axioms =
            if kinds.allows_logical() { self.logical_axioms(Some(&logical), Some(&annotation))? } else { Vec::new() };
        let annotation_axioms =
            if kinds.allows_annotation() { distinct(self.annotation_axioms(&annotation, &logical)?) } else { Vec::new() };
        Ok((logical_axioms, annotation_axioms))
    }

    /// The pattern's logical axioms with no row: each variable its placeholder,
    /// the defined class unnamed (see [`Renderer::logical_axioms`]).
    pub(super) fn pattern_logical_axioms(&self) -> Result<Vec<AC>> {
        self.logical_axioms(None, None)
    }

    /// A variable's range, read as a logical template's class expression is.
    pub(super) fn range_expression(&self, range: &str) -> Result<CE<RcStr>> {
        expression::class_expression(self.b, &self.names, range).map_err(|e| anyhow!("the range `{range}`: {e}"))
    }

    /// The pattern's primary logical template — its `equivalentTo`, else its
    /// `subClassOf`, else the first entry of either kind in `logical_axioms` —
    /// read with each variable its placeholder: its kind, its class expression
    /// and its variables.
    pub(super) fn primary_expression(&self) -> Result<Option<Primary>> {
        let convenience = |kind: AxiomType| self.logical.iter().find(|t| !t.listed && t.kind == kind);
        let primary = convenience(AxiomType::EquivalentTo).or_else(|| convenience(AxiomType::SubClassOf)).or_else(|| {
            self.logical
                .iter()
                .find(|t| t.listed && matches!(t.kind, AxiomType::EquivalentTo | AxiomType::SubClassOf))
        });
        let Some(t) = primary else { return Ok(None) };
        let Some(text) = self.replaced(t.text, t.vars, t.multi_clause, None, true)? else { return Ok(None) };
        let ce = expression::class_expression(self.b, &self.names, &text).map_err(|e| anyhow!("a class expression: {e}"))?;
        Ok(Some((t.kind, ce, t.vars.unwrap_or(&[]).to_vec())))
    }

    /// The class `bindings` bind `defined_class` to, else the unnamed one.
    fn defined_term(&self, bindings: Option<&Bindings>) -> String {
        bindings
            .and_then(|b| b.single(DEFINED_CLASS))
            .and_then(|v| self.prefixes.iri(v))
            .unwrap_or_else(|| variable_iri(DEFINED_CLASS))
    }

    // ── Printf text ──────────────────────────────────────────────────────

    /// What a template fills from `bindings`: its `text`, formatted with its
    /// `vars`' values when each holds one, else its `multi_clause`. In logical
    /// text (`quote`) a value is quoted unless it is already in single quotes
    /// or fills unquoted. With no bindings each variable fills in as its
    /// placeholder, `'$name'`.
    fn replaced(
        &self,
        text: Option<&str>,
        vars: Option<&[String]>,
        multi_clause: Option<&MultiClause>,
        bindings: Option<&Bindings>,
        quote: bool,
    ) -> Result<Option<String>> {
        let text = match text {
            Some(t) => self.replace_text(t, vars, bindings, quote)?,
            None => None,
        };
        let clauses = match multi_clause {
            Some(mc) => self.replace_multi_clause(mc, bindings, quote)?,
            None => None,
        };
        Ok(text.or(clauses))
    }

    fn replace_text(
        &self,
        text: &str,
        vars: Option<&[String]>,
        bindings: Option<&Bindings>,
        quote: bool,
    ) -> Result<Option<String>> {
        let fillers: Option<Vec<String>> = match (vars, bindings) {
            (None, _) => Some(Vec::new()),
            (Some(vars), None) => Some(vars.iter().map(|v| format!("'${v}'")).collect()),
            (Some(vars), Some(b)) => vars
                .iter()
                .map(|v| {
                    b.single(v).map(|value| {
                        let quoted = value.starts_with('\'') && value.ends_with('\'');
                        if quote && !self.unquoted.contains(v.as_str()) && !quoted {
                            format!("'{value}'")
                        } else {
                            value.to_string()
                        }
                    })
                })
                .collect(),
        };
        let Some(fillers) = fillers else { return Ok(None) };
        let args: Vec<&str> = fillers.iter().map(String::as_str).collect();
        java::format(java_trim(text), &args).map(Some).map_err(|e| anyhow!("the template `{text}`: {e}"))
    }

    /// Each clause filled (see [`replace_clause`](Self::replace_clause)), each
    /// filling followed by its sub-clauses', joined by `sep` (a space when
    /// none); the empty ones left out and the whole trimmed; none when nothing
    /// is left.
    fn replace_multi_clause(
        &self,
        mc: &MultiClause,
        bindings: Option<&Bindings>,
        quote: bool,
    ) -> Result<Option<String>> {
        let sep = mc.sep.as_deref().unwrap_or(" ");
        let mut texts: Vec<String> = Vec::new();
        for clause in mc.clauses.iter().flatten() {
            for filled in self.replace_clause(clause, bindings, quote)? {
                let mut parts = vec![filled];
                for sub in clause.sub_clauses.iter().flatten() {
                    parts.extend(self.replace_multi_clause(sub, bindings, quote)?);
                }
                texts.push(parts.join(sep));
            }
        }
        let joined = texts.into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(sep);
        let trimmed = java_trim(&joined);
        Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
    }

    /// A clause filled: once, or, when one of its variables holds a list (the
    /// first in iteration order), once for each value of that list with the
    /// other variables' single values. Its fillings are a set.
    fn replace_clause(&self, clause: &Clause, bindings: Option<&Bindings>, quote: bool) -> Result<Vec<String>> {
        let vars = clause.vars.as_deref();
        match bindings.and_then(|b| b.first_multi(vars.unwrap_or(&[]))) {
            None => Ok(self.replaced(Some(&clause.text), vars, None, bindings, quote)?.into_iter().collect()),
            Some((var, values)) => {
                let singles = bindings.map(Bindings::singles).unwrap_or_default();
                let mut out = Vec::new();
                for value in values {
                    let mut b = singles.clone();
                    b.insert(&var, Binding::Single(value));
                    out.extend(self.replaced(Some(&clause.text), vars, None, Some(&b), quote)?);
                }
                Ok(set_of(out))
            }
        }
    }

    // ── Logical axioms ───────────────────────────────────────────────────

    /// The logical axioms of the templates the bindings fill, without repeats:
    /// those of the `equivalentTo`, `subClassOf`, `disjointWith` and `GCI`
    /// templates, then those of the entries of `logical_axioms`. The templates
    /// are filled in the order [`Logical`] gives, so of two that cannot be, the
    /// first in that order is the one reported.
    fn logical_axioms(&self, logical: Option<&Bindings>, annotation: Option<&Bindings>) -> Result<Vec<AC>> {
        let rank = |t: &Logical| match (t.listed, t.kind) {
            (false, AxiomType::EquivalentTo) => 0,
            (false, AxiomType::SubClassOf) => 1,
            (false, AxiomType::DisjointWith) => 2,
            (false, AxiomType::Gci) => 3,
            (true, _) => 4,
        };
        let term = CE::Class(self.b.class(self.defined_term(logical)));
        let mut out: Vec<(u8, AC)> = Vec::new();
        for t in &self.logical {
            let early = if t.listed { Some(self.axiom_annotations(t, annotation, logical)?) } else { None };
            let Some(text) = self.replaced(t.text, t.vars, t.multi_clause, logical, true)? else { continue };
            let component = match t.kind {
                AxiomType::Gci => {
                    expression::axiom(self.b, &self.names, &text).map_err(|e| anyhow!("the GCI axiom: {e}"))?
                }
                kind => {
                    let ce = expression::class_expression(self.b, &self.names, &text)
                        .map_err(|e| anyhow!("the {} axiom: {e}", kind.name()))?;
                    match kind {
                        AxiomType::EquivalentTo => {
                            Component::EquivalentClasses(EquivalentClasses(vec![term.clone(), ce]))
                        }
                        AxiomType::SubClassOf => Component::SubClassOf(SubClassOf { sub: term.clone(), sup: ce }),
                        _ => Component::DisjointClasses(DisjointClasses(vec![term.clone(), ce])),
                    }
                }
            };
            let ann = match early {
                Some(a) => a,
                None => self.axiom_annotations(t, annotation, logical)?,
            };
            let component = crate::io::canonical_component(&component).unwrap_or(component);
            out.push((rank(t), AnnotatedComponent { component, ann }));
        }
        out.sort_by_key(|(rank, _)| *rank);
        Ok(distinct(out.into_iter().map(|(_, ac)| ac).collect()))
    }

    fn axiom_annotations(
        &self,
        t: &Logical,
        annotation: Option<&Bindings>,
        logical: Option<&Bindings>,
    ) -> Result<BTreeSet<Annotation<RcStr>>> {
        let normalized = t.annotations.as_ref().map_err(|e| anyhow!("an annotation of a logical axiom: {e}"))?;
        let none = HashMap::new();
        let mut out = BTreeSet::new();
        for n in normalized {
            out.extend(self.translate(n, annotation, logical, &none)?);
        }
        Ok(out)
    }

    // ── Annotations ──────────────────────────────────────────────────────

    fn annotation_axioms(&self, annotation: &Bindings, logical: &Bindings) -> Result<Vec<AnnotatedComponent<RcStr>>> {
        let fields = self.annotation_fields.as_ref().map_err(|e| anyhow!("{e}"))?;
        let subject = AnnotationSubject::IRI(self.b.iri(self.defined_term(Some(annotation))));
        let mut out = Vec::new();
        for n in fields {
            for a in self.translate(n, Some(annotation), Some(logical), self.readable.index)? {
                out.push(AnnotatedComponent {
                    component: Component::AnnotationAssertion(AnnotationAssertion {
                        subject: subject.clone(),
                        ann: Annotation { ap: a.ap, av: a.av, ann: BTreeSet::new() },
                    }),
                    ann: a.ann,
                });
            }
        }
        Ok(out)
    }

    /// The annotations an annotation of the pattern makes, each carrying its
    /// own annotations.
    fn translate(
        &self,
        n: &Normalized,
        annotation: Option<&Bindings>,
        logical: Option<&Bindings>,
        index: &HashMap<String, HashMap<String, Vec<String>>>,
    ) -> Result<Vec<Annotation<RcStr>>> {
        let make = |prop: &str, values: Vec<AnnotationValue<RcStr>>, sub: &[Normalized]| -> Result<Vec<Annotation<RcStr>>> {
            if values.is_empty() {
                return Ok(Vec::new());
            }
            let mut ann = BTreeSet::new();
            for s in sub {
                ann.extend(self.translate(s, annotation, logical, index)?);
            }
            let mut out: Vec<Annotation<RcStr>> = Vec::new();
            for av in values {
                push_unique(&mut out, Annotation { ap: self.b.annotation_property(prop.to_string()), av, ann: ann.clone() });
            }
            Ok(out)
        };
        let literal = |v: String| AnnotationValue::Literal(Literal::Simple { literal: v });
        match n {
            Normalized::Printf { prop, text, vars, multi_clause, override_column, sub, permutations } => {
                let overridden = override_column
                    .as_deref()
                    .and_then(|c| annotation.and_then(|b| b.single(c)))
                    .map(java_trim)
                    .filter(|v| !v.is_empty());
                let values = match overridden {
                    Some(v) => vec![v.to_string()],
                    None => self.print_with_permutations(
                        text.as_deref(),
                        vars.as_deref(),
                        multi_clause.as_ref(),
                        annotation,
                        logical,
                        permutations,
                        index,
                    )?,
                };
                make(prop, values.into_iter().map(literal).collect(), sub)
            }
            Normalized::List { prop, value, sub } => {
                let values: Vec<String> = match annotation {
                    None => vec![format!("'${value}'")],
                    Some(b) => match b.get(value) {
                        Some(Binding::Multi(items)) => items.clone(),
                        _ => Vec::new(),
                    },
                };
                make(prop, values.into_iter().map(literal).collect(), sub)
            }
            Normalized::IriValue { prop, var, sub } => {
                let iri = match logical {
                    None => Some(variable_iri(var)),
                    Some(b) => b.single(var).and_then(|v| self.prefixes.iri(v)),
                };
                make(prop, iri.into_iter().map(|i| AnnotationValue::IRI(self.b.iri(i))).collect(), sub)
            }
        }
    }

    /// An annotation's texts, with each permuted variable also taking its
    /// filler's values of the permutation's properties in `index`: one text
    /// for each combination of the variables' values.
    #[allow(clippy::too_many_arguments)]
    fn print_with_permutations(
        &self,
        text: Option<&str>,
        vars: Option<&[String]>,
        multi_clause: Option<&MultiClause>,
        annotation: Option<&Bindings>,
        logical: Option<&Bindings>,
        permutations: &[(String, Vec<String>)],
        index: &HashMap<String, HashMap<String, Vec<String>>>,
    ) -> Result<Vec<String>> {
        let variables = vars.unwrap_or(&[]);
        if permutations.is_empty() || variables.is_empty() {
            return self.print(text, vars, multi_clause, annotation);
        }
        let by_var: HashMap<&str, &Vec<String>> = permutations.iter().map(|(v, ps)| (v.as_str(), ps)).collect();
        let mut value_lists: Vec<Vec<String>> = Vec::new();
        for v in variables {
            let mut values: Vec<String> = annotation.and_then(|b| b.single(v)).map(str::to_string).into_iter().collect();
            let filler = logical.and_then(|b| b.single(v)).and_then(|f| self.prefixes.iri(f));
            if let (Some(props), Some(iri)) = (by_var.get(v.as_str()), filler) {
                if let Some(term) = index.get(&iri) {
                    let mut extra: Vec<String> = Vec::new();
                    for prop in props.iter() {
                        for value in term.get(prop).into_iter().flatten() {
                            push_unique(&mut extra, value.clone());
                        }
                    }
                    values.extend(extra);
                }
            }
            value_lists.push(values);
        }
        let mut combinations: Vec<Vec<String>> = vec![Vec::new()];
        for list in &value_lists {
            combinations = combinations
                .iter()
                .flat_map(|c| {
                    list.iter().map(move |v| {
                        let mut next = c.clone();
                        next.push(v.clone());
                        next
                    })
                })
                .collect();
        }
        let mut out: Vec<String> = Vec::new();
        for combination in combinations {
            let mut b = Bindings::default();
            for (v, value) in variables.iter().zip(combination) {
                b.insert(v, Binding::Single(value));
            }
            if let Some(t) = self.replaced(text, vars, multi_clause, Some(&b), false)? {
                push_unique(&mut out, t);
            }
        }
        Ok(out)
    }

    /// An annotation's texts: one, or, when one of its variables or its top
    /// clauses' variables holds a list (the first in iteration order), one
    /// for each value of that list, each its `multi_clause` filled with that
    /// value and the single values.
    fn print(
        &self,
        text: Option<&str>,
        vars: Option<&[String]>,
        multi_clause: Option<&MultiClause>,
        annotation: Option<&Bindings>,
    ) -> Result<Vec<String>> {
        let mut variables: Vec<String> = vars.unwrap_or(&[]).to_vec();
        for clause in multi_clause.and_then(|m| m.clauses.as_deref()).unwrap_or(&[]) {
            variables.extend(clause.vars.iter().flatten().cloned());
        }
        match annotation.and_then(|b| b.first_multi(&variables)) {
            None => {
                let singles = annotation.map(Bindings::singles);
                Ok(self.replaced(text, vars, multi_clause, singles.as_ref(), false)?.into_iter().collect())
            }
            Some((var, values)) => {
                let singles = annotation.map(Bindings::singles).unwrap_or_default();
                let mut out = Vec::new();
                for value in values {
                    let mut b = singles.clone();
                    b.insert(&var, Binding::Single(value));
                    out.extend(self.replaced(None, None, multi_clause, Some(&b), false)?);
                }
                Ok(set_of(out))
            }
        }
    }
}

/// The defined class minted for a row: the pattern IRI, `#`, and the SHA-1 of
/// the bindings' values sorted by variable name, a list's values sorted and
/// joined by `|`, the whole joined by `&` (names and values compared in UTF-16
/// code units, the text hashed as UTF-8).
fn defined_iri(pattern_iri: &str, bindings: &Bindings) -> String {
    use sha1::{Digest, Sha1};
    let utf16 = |a: &String, b: &String| a.encode_utf16().cmp(b.encode_utf16());
    let mut keys: Vec<&String> = bindings.values.keys().collect();
    keys.sort_by(|a, b| utf16(a, b));
    let text = keys
        .into_iter()
        .map(|k| match &bindings.values[k] {
            Binding::Single(v) => v.clone(),
            Binding::Multi(vs) => {
                let mut vs = vs.clone();
                vs.sort_by(utf16);
                vs.join("|")
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    let hex: String = Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    format!("{pattern_iri}#{hex}")
}

fn push_unique<T: PartialEq>(v: &mut Vec<T>, x: T) {
    if !v.contains(&x) {
        v.push(x);
    }
}
