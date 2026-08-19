//! Native Python bindings for owlmake.
//!
//! A thin pyo3 wrapper over [`owlmake::api`], exposing the same operations the
//! CLI and the JS (wasm) package expose, as a single [`Ontology`] class. The
//! method set mirrors the JS binding one-for-one; only the surface idiom
//! differs (snake_case methods, `bytes` for serialized output, lists/tuples
//! for the read accessors).
//!
//! ```python
//! from owlmake import Ontology
//! ont = Ontology.parse(data, "ofn")
//! ont.reason("elk")
//! ont.add_axioms("SubClassOf(:A :B)")
//! out = ont.serialize("ofn")
//! ```
//!
//! Everything runs inside the calling process as a native extension: no temp
//! files, no process spawn, and the live ontology stays resident between calls.

use std::collections::BTreeMap;

use owlmake::api::{self, ReasonOptions};
use owlmake::io::Format;
use owlmake::model::Model;
use owlmake::sssom::{io as sssom_io, MappingSet as CoreMappingSet};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use pyo3::wrap_pyfunction;

/// Map an `owlmake::api::Error` to the right Python exception: a bad value for a
/// closed-set parameter (reasoner / DL-query kind / format) becomes a
/// `ValueError`; anything else (parse, reasoning, I/O, SPARQL) a `RuntimeError`.
fn pyerr(e: api::Error) -> PyErr {
    let msg = e.to_string();
    match e {
        api::Error::Unknown { .. } => PyValueError::new_err(msg),
        _ => PyRuntimeError::new_err(msg),
    }
}

/// Coerce a table argument — TSV/CSV `str`, a list of `{column: value}` row
/// dicts, or a pandas/polars DataFrame — into TSV text. The header is the column
/// order of the first row; values are stringified, with None/NaN emitted as
/// empty cells (and any embedded tabs/newlines flattened to spaces). This is what
/// lets the `template` and `dosdp` entry points take a data table in whichever
/// shape the caller already has it in.
fn coerce_tsv(table: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(s) = table.extract::<String>() {
        return Ok(s);
    }
    // DataFrame → list of row dicts (polars `.to_dicts()`, pandas
    // `.to_dict("records")`); otherwise assume it already is such a list.
    let rows_obj = if table.hasattr("to_dicts")? {
        table.call_method0("to_dicts")?
    } else if table.hasattr("to_dict")? {
        table.call_method1("to_dict", ("records",))?
    } else {
        table.clone()
    };
    let rows: Vec<Bound<'_, PyAny>> = rows_obj.try_iter()?.collect::<PyResult<_>>()?;
    if rows.is_empty() {
        return Ok(String::new());
    }
    let first = rows[0]
        .cast::<PyDict>()
        .map_err(|_| pyo3::exceptions::PyTypeError::new_err("table rows must be dicts"))?;
    let cols: Vec<String> = first
        .keys()
        .iter()
        .map(|k| k.str().map(|s| s.to_string()))
        .collect::<PyResult<_>>()?;

    let cell = |v: &Bound<'_, PyAny>| -> PyResult<String> {
        if v.is_none() {
            return Ok(String::new());
        }
        // Treat float NaN (pandas' missing marker) as an empty cell.
        if let Ok(f) = v.extract::<f64>() {
            if f.is_nan() {
                return Ok(String::new());
            }
        }
        Ok(v.str()?.to_string().replace('\t', " ").replace('\n', " "))
    };

    let mut out = String::new();
    out.push_str(&cols.join("\t"));
    out.push('\n');
    for row in &rows {
        let d = row
            .cast::<PyDict>()
            .map_err(|_| pyo3::exceptions::PyTypeError::new_err("table rows must be dicts"))?;
        let mut cells = Vec::with_capacity(cols.len());
        for c in &cols {
            match d.get_item(c.as_str())? {
                Some(v) => cells.push(cell(&v)?),
                None => cells.push(String::new()),
            }
        }
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    Ok(out)
}

/// Resolve a format name (`"ofn"`, `"owl"`, `"obo"`, `"ttl"`, …) the same way
/// the CLI's `--format` does; an unknown name raises `ValueError`.
fn fmt(name: &str) -> PyResult<Format> {
    Format::from_name(name).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// An in-memory OWL ontology: the unit every operation reads and writes. Holds
/// the full horned-owl object model plus its prefix map.
///
/// `unsendable`: the model is reference-counted (horned-owl's `RcStr`), so it is
/// `!Send`/`!Sync`. pyo3 enforces this by pinning the object to the thread that
/// created it and raising if another thread touches it — exactly the right
/// contract for Rc-backed data (and CPython's GIL means single-thread use is the
/// norm anyway).
#[pyclass(name = "Ontology", unsendable)]
pub struct Ontology {
    model: Model,
}

#[pymethods]
impl Ontology {
    /// An empty ontology, ready for `add_axioms`.
    #[new]
    fn new() -> Self {
        Ontology { model: Model::new() }
    }

    /// Parse an ontology from `bytes` in the given serialization format.
    #[staticmethod]
    fn parse(bytes: &[u8], format: &str) -> PyResult<Ontology> {
        let model = api::parse(bytes, fmt(format)?).map_err(pyerr)?;
        Ok(Ontology { model })
    }

    /// Serialize the ontology to `bytes` in the given format.
    fn serialize<'py>(&self, py: Python<'py>, format: &str) -> PyResult<Bound<'py, PyBytes>> {
        let out = api::serialize(&self.model, fmt(format)?).map_err(pyerr)?;
        Ok(PyBytes::new(py, &out))
    }

    /// Classify the ontology and assert the inferred axioms in place.
    /// `reasoner` is `"elk"`/`"owlmake"`/`"whelk"` (EL) or `"hermit"`/`"jfact"`
    /// (full DL).
    fn reason(&mut self, reasoner: &str) -> PyResult<()> {
        let model = std::mem::replace(&mut self.model, Model::new());
        self.model = api::reason(model, reasoner, &ReasonOptions::default()).map_err(pyerr)?;
        Ok(())
    }

    /// Remove logically redundant `SubClassOf` axioms (transitive reduction of
    /// the class hierarchy), in place.
    fn reduce(&mut self) {
        self.model = api::reduce(&self.model);
    }

    /// Relax equivalence/expression axioms into entailed `SubClassOf` axioms
    /// (`om relax`), in place.
    fn relax(&mut self) {
        let model = std::mem::replace(&mut self.model, Model::new());
        self.model = api::relax(model);
    }

    /// Merge another ontology into this one in place (this is the base).
    fn merge(&mut self, other: &Ontology) {
        api::merge_into(&mut self.model, &other.model);
    }

    /// Keep only the axioms mentioning `terms` (`om filter`), in place.
    /// `select` chooses related-entity expansion; `signature` keeps an axiom if
    /// any of its signature is selected (vs the whole signature by default).
    #[pyo3(signature = (terms, select=None, signature=false))]
    fn filter(&mut self, terms: Vec<String>, select: Option<Vec<String>>, signature: bool) -> PyResult<()> {
        let model = std::mem::replace(&mut self.model, Model::new());
        self.model = api::filter(model, &terms, &select.unwrap_or_default(), signature).map_err(pyerr)?;
        Ok(())
    }

    /// Remove the axioms mentioning `terms` (`om remove`), in place.
    #[pyo3(signature = (terms, select=None))]
    fn remove(&mut self, terms: Vec<String>, select: Option<Vec<String>>) -> PyResult<()> {
        let model = std::mem::replace(&mut self.model, Model::new());
        self.model = api::remove(model, &terms, &select.unwrap_or_default()).map_err(pyerr)?;
        Ok(())
    }

    /// Set the ontology/version IRIs and add ontology annotations (`om annotate`),
    /// in place. `annotations` is a flat list of alternating `prop, value` tokens
    /// (e.g. `["rdfs:comment", "hello"]`).
    #[pyo3(signature = (ontology_iri=None, version_iri=None, annotations=None))]
    fn annotate(
        &mut self,
        ontology_iri: Option<String>,
        version_iri: Option<String>,
        annotations: Option<Vec<String>>,
    ) -> PyResult<()> {
        let model = std::mem::replace(&mut self.model, Model::new());
        self.model = api::annotate(
            model,
            ontology_iri.as_deref(),
            version_iri.as_deref(),
            &annotations.unwrap_or_default(),
        )
        .map_err(pyerr)?;
        Ok(())
    }

    /// Bulk-rename entity IRIs from an old→new dict (`om rename`), in place.
    fn rename(&mut self, mapping: std::collections::HashMap<String, String>) -> PyResult<()> {
        let model = std::mem::replace(&mut self.model, Model::new());
        self.model = api::rename(model, &mapping).map_err(pyerr)?;
        Ok(())
    }

    /// Assert inferred existential restrictions (`om materialize`), in place.
    /// `properties` limits which object properties to materialize (all if empty).
    #[pyo3(signature = (properties=None))]
    fn materialize(&mut self, properties: Option<Vec<String>>) {
        let model = std::mem::replace(&mut self.model, Model::new());
        self.model = api::materialize(model, &properties.unwrap_or_default());
    }

    /// Extract a module for a seed term set (`om extract`) as a new ontology,
    /// leaving this one unchanged. `method` is `BOT`/`TOP`/`STAR`/`MIREOT`.
    #[pyo3(signature = (terms, method="STAR"))]
    fn extract(&self, terms: Vec<String>, method: &str) -> PyResult<Ontology> {
        Ok(Ontology { model: api::extract(&self.model, &terms, method).map_err(pyerr)? })
    }

    /// A human-readable diff against another ontology (`om diff`).
    fn diff(&self, other: &Ontology) -> String {
        api::diff(&self.model, &other.model)
    }

    /// Ontology metrics (`om measure`) as tab-separated `metric\tvalue` rows.
    fn measure(&self) -> String {
        api::measure(&self.model)
    }

    /// Run a SPARQL SELECT/ASK query over the ontology (`om query`), returning
    /// the result table as TSV.
    fn query(&self, sparql: &str) -> PyResult<String> {
        Ok(api::query(&self.model, sparql).map_err(pyerr)?)
    }

    /// Run a SPARQL SELECT query, returning the rows as a list of dicts
    /// (`{column: value}`) — ready for `pandas.DataFrame(...)` /
    /// `polars.DataFrame(...)`. Missing values are omitted from a row's dict.
    ///
    /// With `reasoner` set (`"elk"`/`"hermit"`/…), the ontology is classified
    /// first and the query runs over the *entailed* graph (inferred axioms
    /// included); the ontology itself is left unchanged.
    #[pyo3(signature = (sparql, reasoner=None))]
    fn query_records(
        &self,
        sparql: &str,
        reasoner: Option<&str>,
    ) -> PyResult<Vec<BTreeMap<String, String>>> {
        let table = match reasoner {
            Some(r) => api::query_reasoned(&self.model, sparql, r).map_err(pyerr)?,
            None => api::query_table(&self.model, sparql).map_err(pyerr)?,
        };
        Ok(table.records())
    }

    /// Run a SPARQL SELECT query and return the result as a DataFrame directly.
    /// `backend` is `"pandas"` (default) or `"polars"`. With `reasoner` set, the
    /// query runs over the reasoned/entailed graph — the one-call "reason over an
    /// ontology and get back a DataFrame" path.
    ///
    /// >>> df = ont.query_dataframe("SELECT ?s ?o WHERE { ?s rdfs:subClassOf ?o }",
    /// ...                          reasoner="elk")            # doctest: +SKIP
    #[pyo3(signature = (sparql, reasoner=None, backend="pandas"))]
    fn query_dataframe(
        &self,
        py: Python<'_>,
        sparql: &str,
        reasoner: Option<&str>,
        backend: &str,
    ) -> PyResult<Py<PyAny>> {
        let records = self.query_records(sparql, reasoner)?;
        let module = match backend.to_ascii_lowercase().as_str() {
            "polars" | "pl" => "polars",
            "pandas" | "pd" => "pandas",
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown DataFrame backend: {other} (use 'pandas' or 'polars')"
                )))
            }
        };
        let df = py
            .import(module)
            .map_err(|_| {
                pyo3::exceptions::PyImportError::new_err(format!(
                    "{module} is not installed (pip install {module})"
                ))
            })?
            .call_method1("DataFrame", (records,))?;
        Ok(df.unbind())
    }

    /// A DL query: a Manchester-syntax class `expression`, answered by the
    /// reasoner. `kind` is `subclasses` (direct) / `descendants` (all) /
    /// `superclasses` / `ancestors` / `equivalent` / `instances`. `reasoner` is
    /// `"elk"`/`"hermit"`/…. Returns the matching entity IRIs.
    ///
    /// >>> ont.dl_query("part_of some 'brain'", "descendants", reasoner="elk")  # doctest: +SKIP
    #[pyo3(signature = (expression, kind="descendants", reasoner="elk"))]
    fn dl_query(&self, expression: &str, kind: &str, reasoner: &str) -> PyResult<Vec<String>> {
        Ok(api::dl_query(&self.model, expression, kind, reasoner).map_err(pyerr)?)
    }

    /// A DL query (see `dl_query`) returned as a one-column DataFrame (`entity`).
    /// `backend` is `"pandas"` (default) or `"polars"`.
    #[pyo3(signature = (expression, kind="descendants", reasoner="elk", backend="pandas"))]
    fn dl_query_dataframe(
        &self,
        py: Python<'_>,
        expression: &str,
        kind: &str,
        reasoner: &str,
        backend: &str,
    ) -> PyResult<Py<PyAny>> {
        let rows: Vec<BTreeMap<String, String>> = api::dl_query(&self.model, expression, kind, reasoner).map_err(pyerr)?
            .into_iter()
            .map(|iri| BTreeMap::from([("entity".to_string(), iri)]))
            .collect();
        let module = match backend.to_ascii_lowercase().as_str() {
            "polars" | "pl" => "polars",
            "pandas" | "pd" => "pandas",
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown DataFrame backend: {other} (use 'pandas' or 'polars')"
                )))
            }
        };
        let df = py
            .import(module)
            .map_err(|_| {
                pyo3::exceptions::PyImportError::new_err(format!(
                    "{module} is not installed (pip install {module})"
                ))
            })?
            .call_method1("DataFrame", (rows,))?;
        Ok(df.unbind())
    }

    /// Generate OWL axioms from a template table and merge them into this
    /// ontology in place (`om template`). `table` may be TSV/CSV text, a list of
    /// `{column: value}` row dicts, or a pandas/polars DataFrame. The first data
    /// row holds the template strings (the column directives).
    fn template(&mut self, table: &Bound<'_, PyAny>) -> PyResult<()> {
        let tsv = coerce_tsv(table)?;
        let model = std::mem::replace(&mut self.model, Model::new());
        self.model = api::template(model, &tsv).map_err(pyerr)?;
        Ok(())
    }

    /// Add the axioms in an OWL Functional-Syntax fragment, resolved against the
    /// ontology's prefixes. Returns the number newly inserted.
    fn add_axioms(&mut self, ofn: &str) -> PyResult<usize> {
        Ok(api::add_axioms(&mut self.model, ofn).map_err(pyerr)?)
    }

    /// Remove the axioms in an OWL Functional-Syntax fragment. Returns the
    /// number actually removed.
    fn remove_axioms(&mut self, ofn: &str) -> PyResult<usize> {
        Ok(api::remove_axioms(&mut self.model, ofn).map_err(pyerr)?)
    }

    /// Number of components (logical axioms + ontology metadata).
    fn axiom_count(&self) -> usize {
        api::axiom_count(&self.model)
    }

    /// `len(ontology)` is its axiom count.
    fn __len__(&self) -> usize {
        api::axiom_count(&self.model)
    }

    fn __repr__(&self) -> String {
        format!("Ontology(components={})", api::axiom_count(&self.model))
    }

    /// The IRIs of every declared class.
    fn classes(&self) -> Vec<String> {
        api::classes(&self.model)
    }

    /// The IRIs of every declared object property.
    fn object_properties(&self) -> Vec<String> {
        api::object_properties(&self.model)
    }

    /// Every named `SubClassOf` relation as a list of `(sub, super)` IRI
    /// tuples.
    fn subclass_pairs(&self) -> Vec<(String, String)> {
        api::subclass_pairs(&self.model)
    }
}

/// Run an owlmake CLI invocation **in-process** (no subprocess) and return its
/// exit code. `args` is the argv after the program name, e.g.
/// `["convert", "-i", "a.ofn", "-o", "a.obo"]` or a chained
/// `["merge", "-i", "a.owl", "reason", "reduce", "-o", "out.owl"]`.
///
/// This is the engine behind every generated per-command function (and `Chain`)
/// in the Python package: the whole command surface — including the `sssom`,
/// `jq` and `dosdp` sub-CLIs — runs through the same dispatch the `owlmake`
/// binary uses, on a large-stack worker thread, with the GIL released.
/// Output goes to the process's stdout/stderr; the Python layer captures it by
/// redirecting file descriptors when asked.
#[pyfunction]
fn cli(py: Python<'_>, args: Vec<String>) -> i32 {
    py.detach(|| owlmake::cli::run_argv_main(args))
}

/// The clap-introspected CLI spec as a JSON string — the authoritative source
/// the per-command Python wrappers are generated from (see
/// `crates/owlmake-py/scripts/generate.py`). Equivalent to `owlmake __cli-spec`.
#[pyfunction]
fn cli_spec() -> String {
    owlmake::cli::dump_cli_spec()
}

/// An in-memory SSSOM mapping set: a prefix map, set-level metadata, and the
/// mapping records. Each record is a `{slot: value}` dict, so the set converts
/// directly to/from a pandas or polars DataFrame via `records()` /
/// `from_records()` (or the `to_pandas`/`to_polars` helpers in the `owlmake`
/// package).
#[pyclass(name = "MappingSet", unsendable)]
pub struct MappingSet {
    inner: CoreMappingSet,
}

#[pymethods]
impl MappingSet {
    /// An empty mapping set.
    #[new]
    fn new() -> Self {
        MappingSet { inner: CoreMappingSet::new() }
    }

    /// Parse a mapping set from text. `format` is `tsv` (default), `csv`,
    /// `json`, `obographs`, or `alignment` (XML).
    #[staticmethod]
    #[pyo3(signature = (text, format="tsv"))]
    fn parse(text: &str, format: &str) -> PyResult<MappingSet> {
        let inner = match format.to_ascii_lowercase().as_str() {
            "tsv" | "sssom" => sssom_io::read_table(text, '\t', None)?,
            "csv" => sssom_io::read_table(text, ',', None)?,
            "json" => sssom_io::read_json(text)?,
            "obographs" => sssom_io::parse_obographs_json(text, None)?,
            "alignment" | "xml" => sssom_io::parse_alignment_xml(text)?,
            other => return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown sssom input format: {other}"
            ))),
        };
        Ok(MappingSet { inner })
    }

    /// Serialize the mapping set. `format` is `tsv` (default), `csv`, `json`,
    /// `ttl`/`turtle`, or `owl`.
    #[pyo3(signature = (format="tsv"))]
    fn serialize(&self, format: &str) -> PyResult<String> {
        let out = match format.to_ascii_lowercase().as_str() {
            "tsv" | "sssom" => sssom_io::write_table(&self.inner, '\t', false, false)?,
            "csv" => sssom_io::write_table(&self.inner, ',', false, false)?,
            "json" => sssom_io::to_json(&self.inner, false)?,
            "ttl" | "turtle" => sssom_io::to_turtle(&self.inner, false)?,
            "owl" => sssom_io::to_turtle(&self.inner, true)?,
            other => return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown sssom output format: {other}"
            ))),
        };
        Ok(out)
    }

    /// The mapping rows as a list of `{slot: value}` dicts —
    /// `pandas.DataFrame(ms.records())` / `polars.DataFrame(ms.records())`.
    fn records(&self) -> Vec<BTreeMap<String, String>> {
        self.inner.mappings.clone()
    }

    /// Build a mapping set from a list of `{slot: value}` dicts (e.g.
    /// `df.to_dict("records")` / `df.to_dicts()`), with an optional CURIE map.
    #[staticmethod]
    #[pyo3(signature = (records, curie_map=None))]
    fn from_records(
        records: Vec<BTreeMap<String, String>>,
        curie_map: Option<BTreeMap<String, String>>,
    ) -> MappingSet {
        let mut inner = CoreMappingSet::new();
        inner.mappings = records;
        if let Some(cm) = curie_map {
            inner.curie_map = cm;
        }
        inner.recompute_columns();
        MappingSet { inner }
    }

    /// The CURIE prefix map (`prefix -> namespace IRI`).
    #[getter]
    fn curie_map(&self) -> BTreeMap<String, String> {
        self.inner.curie_map.clone()
    }

    #[setter]
    fn set_curie_map(&mut self, value: BTreeMap<String, String>) {
        self.inner.curie_map = value;
    }

    /// Sort columns into canonical order and rows by (subject, predicate,
    /// object), in place.
    fn sort(&mut self) {
        self.inner.sort_columns_canonical();
        self.inner.sort_rows();
    }

    /// Canonicalize the mapping set in place: lift propagatable slots that hold
    /// one identical value on every row up into the set metadata, pin the SSSOM
    /// version, round confidences to three decimals, prune unused prefixes, and
    /// sort columns and rows.
    fn canonicalize(&mut self) {
        self.inner.canonicalize();
    }

    /// Lift each propagatable slot that holds one identical value on every row
    /// up into the set metadata, dropping the column, in place.
    fn condense(&mut self) {
        self.inner.condense();
    }

    /// Propagate set-level slots onto each mapping row, in place.
    fn propagate(&mut self) {
        self.inner.propagate();
    }

    /// Merge another mapping set into this one (rows appended, CURIE maps
    /// unioned), in place.
    fn merge(&mut self, other: &MappingSet) {
        self.inner.mappings.extend(other.inner.mappings.iter().cloned());
        for (k, v) in &other.inner.curie_map {
            self.inner.curie_map.entry(k.clone()).or_insert_with(|| v.clone());
        }
        self.inner.recompute_columns();
    }

    /// Number of mapping rows.
    fn __len__(&self) -> usize {
        self.inner.mappings.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "MappingSet(mappings={}, prefixes={})",
            self.inner.mappings.len(),
            self.inner.curie_map.len()
        )
    }
}

/// Convert a SSSOM mapping set between serializations in memory. `input` is the
/// mapping-set text; `from`/`to` are format names (`tsv`/`csv`/`json`/
/// `obographs`/`alignment` in; `tsv`/`csv`/`json`/`ttl`/`owl` out).
#[pyfunction]
#[pyo3(signature = (input, to, from_format="tsv"))]
fn sssom_convert(input: &str, to: &str, from_format: &str) -> PyResult<String> {
    Ok(api::sssom_convert(input, from_format, to).map_err(pyerr)?)
}

/// Generate an ontology from a DOSDP pattern (YAML text) and a data table
/// (`om dosdp`), in memory. `data` may be TSV/CSV text, a list of
/// `{column: value}` row dicts, or a pandas/polars DataFrame.
#[pyfunction]
fn dosdp(pattern_yaml: &str, data: &Bound<'_, PyAny>) -> PyResult<Ontology> {
    let tsv = coerce_tsv(data)?;
    Ok(Ontology { model: api::dosdp(pattern_yaml, &tsv).map_err(pyerr)? })
}

/// The native extension module, imported by the `owlmake` Python package as
/// `owlmake._owlmake`.
#[pymodule]
fn _owlmake(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Ontology>()?;
    m.add_class::<MappingSet>()?;
    m.add_function(wrap_pyfunction!(cli, m)?)?;
    m.add_function(wrap_pyfunction!(cli_spec, m)?)?;
    m.add_function(wrap_pyfunction!(sssom_convert, m)?)?;
    m.add_function(wrap_pyfunction!(dosdp, m)?)?;
    Ok(())
}
