//! The stable function API behind every owlmake frontend.
//!
//! Every operation owlmake performs is a plain function over an in-memory
//! [`Model`] — decoupled from the CLI's argument parsing and from the
//! filesystem. The command-line interface, the native Python (pyo3) extension,
//! and the wasm-bindgen JS package are all thin wrappers over this module: each
//! loads bytes into a `Model`, calls one of these functions, and writes the
//! result back out. New language bindings should target `owlmake::api` directly
//! rather than shelling out to the CLI.
//!
//! The underlying operations live in `crate::cmd::*` (and `crate::io`); this
//! module re-exports and lightly normalizes them into one namespace with
//! consistent `Model`-in/`Model`-out shapes. It is the intended public surface
//! and will grow to cover the full command set; today it covers the core
//! pipeline (parse → relax → reason → reduce → merge → serialize).

use std::str::FromStr;

use horned_owl::model::{
    AnnotatedComponent, AnnotationSubject, AnnotationValue, ClassExpression, Component, DeclareClass,
    EquivalentClasses, Literal, MutableOntology,
};

use crate::io::Format;
use crate::model::{Model, Str};

/// Errors returned by the owlmake API.
///
/// The variant of interest to callers is [`Error::Unknown`] — a closed-set
/// parameter (a reasoner name, a DL-query `kind`, a format) was given a value
/// owlmake does not recognize. Everything else — parse failures, reasoning
/// errors, I/O, SPARQL — is carried opaquely in [`Error::Other`] (the crate
/// uses `anyhow` internally); match on it via `err.source()` if needed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A closed-set parameter received a value owlmake does not recognize.
    #[error("unknown {param}: {value:?}")]
    Unknown {
        /// The parameter that received the bad value (e.g. `"reasoner"`, `"kind"`).
        param: &'static str,
        /// The offending value.
        value: String,
    },
    /// Any other failure (parse, reasoning, I/O, SPARQL, serialization, …).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The owlmake API result type ([`Error`]).
pub type Result<T> = std::result::Result<T, Error>;

/// The reasoner backends owlmake accepts. Used to validate the `reasoner`
/// string arguments so an unknown name errors rather than silently falling back.
const REASONERS: &[&str] = &["elk", "owlmake", "structural", "emr", "whelk", "hermit", "jfact"];

fn check_reasoner(reasoner: &str) -> Result<()> {
    if REASONERS.contains(&reasoner.to_ascii_lowercase().as_str()) {
        Ok(())
    } else {
        Err(Error::Unknown { param: "reasoner", value: reasoner.to_string() })
    }
}

/// Which set of related entities a [`dl_query`] returns, relative to the class
/// expression that was queried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DlQueryKind {
    /// Direct subclasses.
    Subclasses,
    /// All subclasses (transitive).
    Descendants,
    /// Direct superclasses.
    Superclasses,
    /// All superclasses (transitive).
    Ancestors,
    /// Equivalent named classes.
    Equivalent,
    /// Individuals that are instances of the expression.
    Instances,
}

impl FromStr for DlQueryKind {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "subclasses" => Self::Subclasses,
            "descendants" => Self::Descendants,
            "superclasses" => Self::Superclasses,
            "ancestors" => Self::Ancestors,
            "equivalent" | "equivalents" => Self::Equivalent,
            "instances" => Self::Instances,
            _ => return Err(Error::Unknown { param: "kind", value: s.to_string() }),
        })
    }
}

/// A SPARQL SELECT result table: the column (variable) names and the rows of
/// string values. Returned by [`query_table`]/[`query_reasoned`].
pub use crate::sparql::QueryTable;

// Operation option structs + the operations whose signature is already the
// canonical `Model`-shape, re-exported under one namespace.
pub use crate::cmd::merge::MergeOptions;
pub use crate::cmd::reason::ReasonOptions;
pub use crate::cmd::reduce::{
    reduce, reduce_exact, reduce_with_opts, reduce_with_options, ReduceOptions,
};
pub use crate::cmd::relax::{relax, relax_with, RelaxOptions};

/// Parse an ontology from in-memory bytes in the given serialization format.
///
/// This is the filesystem-free entry the bindings use: the CLI's `load(path)`
/// is just `std::fs::read` + `parse`.
pub fn parse(bytes: &[u8], fmt: Format) -> Result<Model> {
    Ok(crate::io::load_from(std::io::Cursor::new(bytes), fmt)?)
}

/// Serialize an ontology to bytes in the given serialization format.
///
/// The CLI's `save(path)` is just `serialize` + `std::fs::write`.
pub fn serialize(model: &Model, fmt: Format) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    crate::io::write_to_ref(model, &mut buf, fmt)?;
    Ok(buf)
}

/// Classify `model` and assert the inferred axioms.
///
/// `reasoner` selects the engine (`elk`/`owlmake`/`whelk` for EL, `hermit`/
/// `jfact` for full DL); `opts` mirrors the `reason` command's flags.
///
/// # Errors
/// Returns [`Error::Unknown`] if `reasoner` is not a recognized backend, or
/// [`Error::Other`] on a reasoning failure.
pub fn reason(model: Model, reasoner: &str, opts: &ReasonOptions) -> Result<Model> {
    check_reasoner(reasoner)?;
    Ok(crate::cmd::reason::reason_with(model, reasoner, opts)?)
}

/// Merge several ontologies into one. The first is the base; each subsequent
/// ontology is merged in using the default merge options.
pub fn merge(models: Vec<Model>) -> Model {
    let opts = MergeOptions::default();
    let mut iter = models.into_iter();
    let mut merged = iter.next().unwrap_or_else(Model::new);
    for other in iter {
        crate::cmd::merge::merge_into(&mut merged, &other, &opts);
    }
    merged
}

/// Merge `other` into `base` in place, using the default merge options. The
/// in-place counterpart of [`merge`]; the bindings expose it as a method so a
/// caller can fold ontologies together without surrendering ownership of each.
pub fn merge_into(base: &mut Model, other: &Model) {
    crate::cmd::merge::merge_into(base, other, &MergeOptions::default());
}

// ---------------------------------------------------------------------------
// Editable model surface
//
// The bindings expose the full horned-owl object model through two layers:
// structured *read* accessors (classes / object_properties / subclass_pairs)
// for cheap inspection, and string-based *edit* operations that take OWL 2
// Functional-Syntax axiom fragments. Editing via fragments — rather than a
// generated constructor for every one of horned-owl's ~80 axiom types — keeps
// one small, version-stable surface that already covers every axiom the parser
// accepts, and reads natively in every host language (a string is a string in
// Python and JS). Fragments are parsed against the model's own prefix map, so
// CURIEs like `:A` / `obo:BFO_0000050` resolve exactly as in the document.
// ---------------------------------------------------------------------------

/// Number of annotated components (logical axioms + ontology metadata) in the
/// model — the same count the CLI prints and `Model::len` returns.
pub fn axiom_count(model: &Model) -> usize {
    model.len()
}

/// Render the model's prefix map as OFN `Prefix(..)` declarations, so a parsed
/// fragment resolves CURIEs against the same namespaces as the document.
fn prefix_prelude(model: &Model) -> String {
    let mut s = String::new();
    for (prefix, iri) in model.prefixes.mappings() {
        s.push_str(&format!("Prefix({prefix}:=<{iri}>)\n"));
    }
    s
}

/// Parse an OFN axiom fragment (one or more axioms, no `Ontology(..)` wrapper)
/// against the model's prefixes, yielding the contained components. Ontology
/// header components (id / imports) are dropped — only axioms are returned.
fn parse_fragment(model: &Model, ofn: &str) -> Result<Vec<AnnotatedComponent<Str>>> {
    let doc = format!("{}Ontology(\n{ofn}\n)\n", prefix_prelude(model));
    let parsed = parse(doc.as_bytes(), Format::Functional)?;
    Ok(parsed
        .ont
        .iter()
        .filter(|ac| !matches!(ac.component, Component::OntologyID(_) | Component::Import(_)))
        .cloned()
        .collect())
}

/// Add the axioms in an OWL Functional-Syntax fragment to the model. Returns
/// the number newly inserted (axioms already present are no-ops, as the model
/// is a set). Example fragment: `SubClassOf(:A :B) Declaration(Class(:C))`.
pub fn add_axioms(model: &mut Model, ofn: &str) -> Result<usize> {
    let comps = parse_fragment(model, ofn)?;
    let mut n = 0;
    for c in comps {
        if model.ont.insert(c) {
            n += 1;
        }
    }
    Ok(n)
}

/// Remove the axioms in an OWL Functional-Syntax fragment from the model.
/// Returns the number actually removed (axioms not present are skipped).
pub fn remove_axioms(model: &mut Model, ofn: &str) -> Result<usize> {
    let comps = parse_fragment(model, ofn)?;
    let mut n = 0;
    for c in comps {
        if model.ont.remove(&c) {
            n += 1;
        }
    }
    Ok(n)
}

/// The IRIs of every declared class in the model (declaration order is not
/// preserved — the underlying ontology is a set).
pub fn classes(model: &Model) -> Vec<String> {
    let mut out: Vec<String> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::DeclareClass(d) => Some(d.0 .0.as_ref().to_string()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// The IRIs of every declared object property in the model.
pub fn object_properties(model: &Model) -> Vec<String> {
    let mut out: Vec<String> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::DeclareObjectProperty(d) => Some(d.0 .0.as_ref().to_string()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// Every named (atomic-class) `SubClassOf` relation as `(sub_iri, super_iri)`
/// pairs. Axioms with complex class expressions on either side are skipped —
/// this is the asserted/inferred class hierarchy, the typical thing a caller
/// wants after `reason`.
pub fn subclass_pairs(model: &Model) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::SubClassOf(sc) => match (&sc.sub, &sc.sup) {
                (ClassExpression::Class(sub), ClassExpression::Class(sup)) => {
                    Some((sub.0.as_ref().to_string(), sup.0.as_ref().to_string()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// In-memory command operations
//
// The pure model→model (and model→report) cores behind owlmake's commands,
// exposed so the bindings can run them on an in-memory ontology with
// no filesystem — the same surface the CLI dispatches to.
// ---------------------------------------------------------------------------

/// Keep only the axioms mentioning `terms` (`om filter`). `select` chooses
/// related-entity expansion (e.g. `["self", "descendants"]`); when `signature`
/// is true an axiom is kept only if its WHOLE signature is selected, otherwise if
/// ANY entity of its signature is.
pub fn filter(model: Model, terms: &[String], select: &[String], signature: bool) -> Result<Model> {
    Ok(crate::cmd::filter::filter(model, terms, &[], select, Some(signature))?)
}

/// Remove the axioms mentioning `terms` (`om remove`). `select` chooses
/// related-entity expansion, as for [`filter`].
pub fn remove(model: Model, terms: &[String], select: &[String]) -> Result<Model> {
    Ok(crate::cmd::remove::remove(model, terms, &[], select, &[], &[])?)
}

/// Set the ontology/version IRIs and add ontology-level annotations
/// (`om annotate`). `annotations` is a flat list of alternating `prop, value`
/// tokens (e.g. `["rdfs:comment", "hello", "dc:creator", "me"]`), matching the
/// CLI's repeatable `--annotation PROP VALUE`.
pub fn annotate(
    model: Model,
    ontology_iri: Option<&str>,
    version_iri: Option<&str>,
    annotations: &[String],
) -> Result<Model> {
    Ok(crate::cmd::annotate::annotate(model, ontology_iri, version_iri, annotations, &[], false)?)
}

/// Bulk-rename entity IRIs by an old→new map (`om rename`).
pub fn rename(model: Model, mapping: &std::collections::HashMap<String, String>) -> Result<Model> {
    Ok(crate::cmd::rename::rename_model(model, mapping)?)
}

/// Assert inferred existential restrictions (`om materialize`). `properties`
/// limits which object properties to materialize (all when empty).
pub fn materialize(model: Model, properties: &[String]) -> Model {
    let props: std::collections::HashSet<String> = properties.iter().cloned().collect();
    crate::cmd::materialize::materialize(model, &props)
}

/// Extract a module for a seed term set (`om extract`). `method` is one of
/// `BOT`, `TOP`, `STAR` (syntactic locality) or `MIREOT`.
pub fn extract(model: &Model, terms: &[String], method: &str) -> Result<Model> {
    let seed: std::collections::HashSet<String> = terms.iter().cloned().collect();
    let opts = crate::extract::ExtractOptions::default();
    if method.eq_ignore_ascii_case("MIREOT") {
        Ok(crate::extract::mireot_with(
            model,
            &seed,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &opts,
        ))
    } else {
        let m = crate::extract::Method::parse(method)
            .ok_or_else(|| Error::Unknown { param: "method", value: method.to_string() })?;
        Ok(crate::extract::extract_with(model, &seed, m, &opts))
    }
}

/// A human-readable diff of two ontologies (`om diff`), in the default
/// `- removed` / `+ added` line format, prefixed with any ontology-ID change.
pub fn diff(left: &Model, right: &Model) -> String {
    let d = crate::diff::diff(left, right);
    let id_change = crate::diff::ontology_id_change(left, right);
    let mut report = String::new();
    if let Some(ch) = &id_change {
        report.push_str(ch);
        report.push('\n');
    }
    if d.is_empty() {
        report.push_str(if id_change.is_none() {
            "Ontologies are identical (no logical differences).\n"
        } else {
            "No logical differences (ontology ID/version differs, see above).\n"
        });
        return report;
    }
    report.push_str(&format!(
        "{} components removed, {} added.\n\n",
        d.only_left.len(),
        d.only_right.len()
    ));
    for c in &d.only_left {
        report.push_str(&format!("- {}\n", crate::diff::describe(c)));
    }
    for c in &d.only_right {
        report.push_str(&format!("+ {}\n", crate::diff::describe(c)));
    }
    report
}

/// Ontology metrics (`om measure`, the `essential` set), as tab-separated
/// `metric\tvalue` rows with a header line.
pub fn measure(model: &Model) -> String {
    let mut s = String::from("metric\tvalue\n");
    for (k, v) in crate::cmd::measure::essential_metrics(model) {
        s.push_str(&format!("{k}\t{v}\n"));
    }
    s
}

/// Run a SPARQL SELECT/ASK query over the ontology (`om query`), returning
/// the result table as TSV (tab-separated, with a header row).
pub fn query(model: &Model, sparql: &str) -> Result<String> {
    Ok(crate::sparql::Queryable::from_model(model)?.query_table(sparql)?.render(false))
}

/// Run a SPARQL SELECT query, returning the structured [`QueryTable`]
/// (`columns` + `rows`) — the form the bindings turn into records / DataFrames
/// (pandas, polars). See [`query`] for the TSV rendering.
pub fn query_table(model: &Model, sparql: &str) -> Result<QueryTable> {
    Ok(crate::sparql::Queryable::from_model(model)?.query_table(sparql)?)
}

/// Classify `model` with `reasoner`, then run a SPARQL SELECT over the entailed
/// graph — so the query sees inferred axioms (e.g. inferred `rdfs:subClassOf`
/// edges), not just the asserted ones. The input is left unchanged (reasoning
/// runs on a clone); for many queries against one classification, call
/// [`reason`] once in place and then [`query_table`].
///
/// # Errors
/// [`Error::Unknown`] for an unrecognized `reasoner`, else [`Error::Other`].
pub fn query_reasoned(model: &Model, sparql: &str, reasoner: &str) -> Result<QueryTable> {
    let reasoned = reason(model.clone(), reasoner, &ReasonOptions::default())?;
    query_table(&reasoned, sparql)
}

/// Generate OWL axioms from a template table (TSV text) and merge them into
/// `model` (`om template`). In memory — no files. Template-cell errors are
/// returned as an `Err`.
pub fn template(model: Model, tsv: &str) -> Result<Model> {
    Ok(crate::cmd::template::template_from_str(model, tsv, false)?)
}

/// Generate OWL from a DOSDP pattern (YAML text) and a data table (TSV text),
/// returning the generated ontology (`om dosdp`). In memory — no files.
pub fn dosdp(pattern_yaml: &str, data_tsv: &str) -> Result<Model> {
    Ok(crate::dosdp::generate(pattern_yaml, data_tsv, &std::collections::HashMap::new())?)
}

/// The IRI of the fresh class a DL query is run as.
const DL_QUERY_IRI: &str = "urn:owlmake:dl-query";

/// Build the `label -> IRI` and `local-name -> IRI` indices a DL query uses to
/// resolve bare Manchester names. Local name = the IRI segment after the last
/// `/` or `#`.
fn dl_name_indices(
    model: &Model,
) -> (
    std::collections::HashMap<String, String>,
    std::collections::HashMap<String, String>,
) {
    const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
    let mut labels = std::collections::HashMap::new();
    let mut locals = std::collections::HashMap::new();
    let local_of = |iri: &str| -> Option<String> {
        iri.rsplit(['/', '#']).next().filter(|s| !s.is_empty()).map(str::to_string)
    };
    let lit_text = |lit: &Literal<crate::model::Str>| -> String {
        match lit {
            Literal::Simple { literal }
            | Literal::Language { literal, .. }
            | Literal::Datatype { literal, .. } => literal.clone(),
        }
    };
    for ac in model.ont.iter() {
        match &ac.component {
            Component::AnnotationAssertion(aa) if aa.ann.ap.0.as_ref() == RDFS_LABEL => {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    labels.entry(lit_text(lit)).or_insert_with(|| s.as_ref().to_string());
                }
            }
            Component::DeclareClass(d) => insert_local(&mut locals, d.0 .0.as_ref(), &local_of),
            Component::DeclareObjectProperty(d) => insert_local(&mut locals, d.0 .0.as_ref(), &local_of),
            Component::DeclareDataProperty(d) => insert_local(&mut locals, d.0 .0.as_ref(), &local_of),
            Component::DeclareAnnotationProperty(d) => insert_local(&mut locals, d.0 .0.as_ref(), &local_of),
            Component::DeclareNamedIndividual(d) => insert_local(&mut locals, d.0 .0.as_ref(), &local_of),
            _ => {}
        }
    }
    (labels, locals)
}

fn insert_local(
    locals: &mut std::collections::HashMap<String, String>,
    iri: &str,
    local_of: &impl Fn(&str) -> Option<String>,
) {
    if let Some(local) = local_of(iri) {
        locals.entry(local).or_insert_with(|| iri.to_string());
    }
}

/// A **DL query**: parse a Manchester-syntax class `expression`, then use the
/// reasoner to return the entities related to it. The expression is asserted
/// equivalent to a fresh class, the ontology is classified, and that class's
/// position in the hierarchy gives the answer.
///
/// `kind` selects the result set:
///   * `subclasses`   — direct subclasses
///   * `descendants`  — all subclasses (transitive)
///   * `superclasses` — direct superclasses
///   * `ancestors`    — all superclasses (transitive)
///   * `equivalent`   — equivalent named classes
///   * `instances`    — individuals that are instances of the expression
///
/// `reasoner` is `elk`/`owlmake` (EL) or `hermit`/`jfact` (full DL; native
/// only — on wasm it falls back to the EL reasoner). The input model is left
/// unchanged. Returns the matching entity IRIs, sorted.
pub fn dl_query(model: &Model, expression: &str, kind: &str, reasoner: &str) -> Result<Vec<String>> {
    // Validate the closed-set args up front so a typo errors instead of
    // silently returning an empty result / the wrong reasoner.
    let kind: DlQueryKind = kind.parse()?;
    check_reasoner(reasoner)?;
    let mut clone = model.clone();
    // Resolve Manchester names in order of preference: a full IRI (bare or in
    // angle brackets), then an exact rdfs:label, then a CURIE, then a declared
    // entity's local name, then the default namespace. So `part_of some Brain`,
    // `Cell`, `:Brain` and full IRIs all work.
    let (labels, locals) = dl_name_indices(&clone);
    let prefixes = crate::model::clone_prefixes(&clone.prefixes);
    let resolver = move |tok: &str| -> Option<String> {
        let t = tok.trim();
        if t.is_empty() {
            return None;
        }
        let inner = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')).unwrap_or(t);
        if inner.starts_with("http://") || inner.starts_with("https://") || inner.starts_with("urn:") {
            return Some(inner.to_string());
        }
        if let Some(iri) = labels.get(t) {
            return Some(iri.clone());
        }
        if t.contains(':') {
            if let Ok(e) = prefixes.expand_curie_string(t) {
                return Some(e);
            }
        }
        if let Some(iri) = locals.get(t) {
            return Some(iri.clone());
        }
        if let Ok(e) = prefixes.expand_curie_string(&format!(":{t}")) {
            return Some(e);
        }
        // OBO PURL convention for a `PREFIX:LOCAL` CURIE whose prefix the ontology
        // does not declare (e.g. a `UBERON:…` query against a subset whose header
        // dropped the prefix): `PREFIX:LOCAL` → `…/obo/PREFIX_LOCAL`.
        if t.contains(':') && !t.contains(' ') {
            let iri = crate::io::obo::expand_id(t);
            if iri != t {
                return Some(iri);
            }
        }
        None
    };
    let ce = crate::io::manchester_parse::parse_class_expression(&clone.build, expression, &resolver)
        .map_err(|e| {
            Error::Other(anyhow::anyhow!(
                "could not parse Manchester class expression '{expression}': {e}"
            ))
        })?;
    let q = clone.build.class(DL_QUERY_IRI);
    clone.ont.insert(Component::DeclareClass(DeclareClass(q.clone())));
    clone.ont.insert(Component::EquivalentClasses(EquivalentClasses(vec![
        ClassExpression::Class(q),
        ce,
    ])));

    let lc = reasoner.to_ascii_lowercase();
    if matches!(lc.as_str(), "hermit" | "jfact") {
        let r = crate::reason::DlReasoner::classify(&clone);
        // The DL reasoner answers instance membership by full entailment.
        let instances = crate::reason::instances(&clone, DL_QUERY_IRI);
        return Ok(dl_select(&r.all_subsumptions(), &r.direct_subsumptions(), kind, &instances));
    }
    let r = crate::reason::Reasoner::classify(&clone);
    let all = r.all_subsumptions();
    // EL instances of Q: individuals whose (direct) type is Q or any subclass of
    // Q — `class_assertions` gives direct types, so widen through the subsumption
    // closure.
    let mut q_and_subs: std::collections::HashSet<&str> = std::collections::HashSet::new();
    q_and_subs.insert(DL_QUERY_IRI);
    for (s, o) in &all {
        if o == DL_QUERY_IRI {
            q_and_subs.insert(s.as_str());
        }
    }
    let mut instances: Vec<String> = r
        .class_assertions()
        .into_iter()
        .filter(|(_, c)| q_and_subs.contains(c.as_str()))
        .map(|(i, _)| i)
        .collect();
    instances.sort();
    instances.dedup();
    Ok(dl_select(&all, &r.direct_subsumptions(), kind, &instances))
}

/// Turn a reasoner's `(sub, super)` subsumption sets (and the precomputed
/// instance set of the query class) into one DL-query result set for
/// [`dl_query`].
fn dl_select(
    all: &[(String, String)],
    direct: &[(String, String)],
    kind: DlQueryKind,
    instances: &[String],
) -> Vec<String> {
    use std::collections::HashSet;
    let q = DL_QUERY_IRI;
    // Equivalent classes: named classes both above and below Q.
    let up: HashSet<&str> = all.iter().filter(|(s, _)| s == q).map(|(_, o)| o.as_str()).collect();
    let down: HashSet<&str> = all.iter().filter(|(_, o)| o == q).map(|(s, _)| s.as_str()).collect();
    let equiv: HashSet<&str> = up.intersection(&down).copied().filter(|c| *c != q).collect();

    // Strip Q itself and its equivalents from the sub/super-class result sets;
    // the `equivalent` kind is where an equivalent class is reported.
    let strip = |v: Vec<String>| -> Vec<String> {
        v.into_iter().filter(|c| c != q && !equiv.contains(c.as_str())).collect()
    };

    let mut out = match kind {
        DlQueryKind::Subclasses => {
            strip(direct.iter().filter(|(_, o)| o == q).map(|(s, _)| s.clone()).collect())
        }
        DlQueryKind::Descendants => {
            strip(all.iter().filter(|(_, o)| o == q).map(|(s, _)| s.clone()).collect())
        }
        DlQueryKind::Superclasses => {
            strip(direct.iter().filter(|(s, _)| s == q).map(|(_, o)| o.clone()).collect())
        }
        DlQueryKind::Ancestors => {
            strip(all.iter().filter(|(s, _)| s == q).map(|(_, o)| o.clone()).collect())
        }
        DlQueryKind::Equivalent => equiv.iter().map(|s| s.to_string()).collect(),
        DlQueryKind::Instances => instances.to_vec(),
    };
    out.sort();
    out.dedup();
    out
}

/// Run a jq `filter` over a JSON `input` string, returning the JSON output
/// (the bundled pure-Rust jq engine — the same one behind `om jq`).
/// String→string and entirely in memory: no files, no stdin/stdout. This is a
/// data transform unrelated to the ontology model, so it's a free function (not
/// a `Model` method).
pub fn jq(filter: &str, input: &str) -> Result<String> {
    crate::jq::run_string(filter, input)
        .map_err(|e| Error::Other(anyhow::anyhow!("{e}")))
}

/// Convert a SSSOM mapping set between serializations, in memory (the `sssom
/// convert` operation). `input` is the mapping set text; `from`/`to` are format
/// names. Supported `from`: `tsv`/`sssom`, `csv`, `json`, `obographs`,
/// `alignment` (XML). Supported `to`: `tsv`/`sssom`, `csv`, `json`,
/// `ttl`/`turtle`, `owl`. String→string — the `MappingSet` object model
/// (`crate::sssom`) is the in-memory unit underneath, mirroring how `Model`
/// backs the ontology operations.
pub fn sssom_convert(input: &str, from: &str, to: &str) -> Result<String> {
    use crate::sssom::io;
    let ms = match from.to_ascii_lowercase().as_str() {
        "tsv" | "sssom" => io::read_table(input, '\t', None)?,
        "csv" => io::read_table(input, ',', None)?,
        "json" => io::read_json(input)?,
        "obographs" => io::parse_obographs_json(input, None)?,
        "alignment" | "xml" => io::parse_alignment_xml(input)?,
        other => return Err(Error::Unknown { param: "from", value: other.to_string() }),
    };
    Ok(match to.to_ascii_lowercase().as_str() {
        "tsv" | "sssom" => io::write_table(&ms, '\t', false, false)?,
        "csv" => io::write_table(&ms, ',', false, false)?,
        "json" => io::to_json(&ms, false)?,
        "ttl" | "turtle" => io::to_turtle(&ms, false)?,
        "owl" => io::to_turtle(&ms, true)?,
        other => return Err(Error::Unknown { param: "to", value: other.to_string() }),
    })
}
