//! WebAssembly bindings for owlmake.
//!
//! A thin wrapper over [`owlmake::api`] exposing the same operations the CLI
//! and the Python extension expose, as a single [`Ontology`] handle plus a few
//! free functions. The API mirrors the Python binding method-for-method so the
//! two read the same; only the surface syntax differs (camelCase method names,
//! `Uint8Array` for bytes, native JS arrays for lists).
//!
//! ```js
//! import { Ontology } from "owlmake";
//! const ont = Ontology.parse(bytes, "ofn");
//! ont.reason("elk");
//! ont.addAxioms("SubClassOf(:A :B)");
//! const out = ont.serialize("ofn");
//! ```

use std::collections::BTreeMap;

use owlmake::api::{self, ReasonOptions};
use owlmake::io::Format;
use owlmake::model::Model;
use owlmake::sssom::{io as sssom_io, MappingSet as CoreMappingSet};
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[wasm_bindgen]
extern "C" {
    /// An array of `{ column: value }` row objects (a table of records).
    #[wasm_bindgen(typescript_type = "Record<string, string>[]")]
    pub type RecordArray;
    /// A `{ key: value }` string map (e.g. a CURIE/prefix map).
    #[wasm_bindgen(typescript_type = "Record<string, string>")]
    pub type StringMap;
    /// An array of `[sub, super]` IRI pairs.
    #[wasm_bindgen(typescript_type = "[string, string][]")]
    pub type PairArray;
}

/// Serialize to a typed JS value (`T` is one of the `typescript_type` extern
/// types above) with maps rendered as plain objects, so record rows / CURIE
/// maps read naturally and the generated `.d.ts` types them precisely.
fn to_js<T: JsCast>(value: &impl Serialize) -> Result<T, JsError> {
    let s = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    Ok(value
        .serialize(&s)
        .map_err(|e| JsError::new(&e.to_string()))?
        .unchecked_into())
}

/// Install the panic hook once, on first use, so a Rust panic becomes a JS
/// exception with a readable message rather than an opaque wasm trap.
fn init() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(console_error_panic_hook::set_once);
}

/// Map any displayable error (an `owlmake::api::Error`, `anyhow::Error`, …) into
/// a JS exception.
fn js_err<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}

/// Resolve a format name (`"ofn"`, `"owl"`, `"obo"`, `"ttl"`, …) the same way
/// the CLI's `--format` does.
fn fmt(name: &str) -> Result<Format, JsError> {
    Format::from_name(name).map_err(js_err)
}

/// An in-memory OWL ontology: the unit every operation reads and writes. Holds
/// the full horned-owl object model plus its prefix map.
#[wasm_bindgen]
pub struct Ontology {
    model: Model,
}

#[wasm_bindgen]
impl Ontology {
    /// Parse an ontology from bytes in the given serialization format.
    #[wasm_bindgen(js_name = parse)]
    pub fn parse(bytes: &[u8], format: &str) -> Result<Ontology, JsError> {
        init();
        let model = api::parse(bytes, fmt(format)?).map_err(js_err)?;
        Ok(Ontology { model })
    }

    /// An empty ontology, ready for `addAxioms`.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Ontology {
        init();
        Ontology { model: Model::new() }
    }

    /// Serialize the ontology to bytes in the given format.
    #[wasm_bindgen(js_name = serialize)]
    pub fn serialize(&self, format: &str) -> Result<Vec<u8>, JsError> {
        api::serialize(&self.model, fmt(format)?).map_err(js_err)
    }

    /// Classify the ontology and assert the inferred axioms in place.
    /// `reasoner` selects the engine: `"elk"`/`"owlmake"` use owlmake's built-in
    /// OWL 2 EL reasoner; `"hermit"`/`"jfact"` use the hermit-rs OWL 2 DL
    /// reasoner; `"whelk"` uses the whelk-rs OWL 2 EL reasoner. All build for
    /// wasm and run in the browser.
    #[wasm_bindgen(js_name = reason)]
    pub fn reason(&mut self, reasoner: &str) -> Result<(), JsError> {
        let model = self.take();
        self.model = api::reason(model, reasoner, &ReasonOptions::default()).map_err(js_err)?;
        Ok(())
    }

    /// Remove logically redundant `SubClassOf` axioms (transitive reduction of
    /// the class hierarchy), in place.
    #[wasm_bindgen(js_name = reduce)]
    pub fn reduce(&mut self) {
        self.model = api::reduce(&self.model);
    }

    /// Relax equivalence/expression axioms into entailed `SubClassOf` axioms,
    /// in place.
    #[wasm_bindgen(js_name = relax)]
    pub fn relax(&mut self) {
        let model = self.take();
        self.model = api::relax(model);
    }

    /// Add the axioms in an OWL Functional-Syntax fragment, resolved against the
    /// ontology's prefixes. Returns the number newly inserted.
    #[wasm_bindgen(js_name = addAxioms)]
    pub fn add_axioms(&mut self, ofn: &str) -> Result<usize, JsError> {
        api::add_axioms(&mut self.model, ofn).map_err(js_err)
    }

    /// Remove the axioms in an OWL Functional-Syntax fragment. Returns the
    /// number actually removed.
    #[wasm_bindgen(js_name = removeAxioms)]
    pub fn remove_axioms(&mut self, ofn: &str) -> Result<usize, JsError> {
        api::remove_axioms(&mut self.model, ofn).map_err(js_err)
    }

    /// Number of components (logical axioms + ontology metadata).
    #[wasm_bindgen(js_name = axiomCount)]
    pub fn axiom_count(&self) -> usize {
        api::axiom_count(&self.model)
    }

    /// The IRIs of every declared class.
    #[wasm_bindgen(js_name = classes)]
    pub fn classes(&self) -> Vec<String> {
        api::classes(&self.model)
    }

    /// The IRIs of every declared object property.
    #[wasm_bindgen(js_name = objectProperties)]
    pub fn object_properties(&self) -> Vec<String> {
        api::object_properties(&self.model)
    }

    /// Every named `SubClassOf` relation as an array of `[sub, super]` IRI
    /// pairs.
    #[wasm_bindgen(js_name = subclassPairs)]
    pub fn subclass_pairs(&self) -> Result<PairArray, JsError> {
        to_js(&api::subclass_pairs(&self.model))
    }

    /// Keep only the axioms mentioning `terms`, in place. `select` chooses
    /// related-entity expansion; `signature` keeps an axiom if any of its
    /// signature is selected (vs the whole signature by default).
    #[wasm_bindgen(js_name = filter)]
    pub fn filter(&mut self, terms: Vec<String>, select: Vec<String>, signature: bool) -> Result<(), JsError> {
        let model = self.take();
        self.model = api::filter(model, &terms, &select, signature).map_err(js_err)?;
        Ok(())
    }

    /// Remove the axioms mentioning `terms`, in place. `select` chooses
    /// related-entity expansion, as for `filter`.
    #[wasm_bindgen(js_name = remove)]
    pub fn remove(&mut self, terms: Vec<String>, select: Vec<String>) -> Result<(), JsError> {
        let model = self.take();
        self.model = api::remove(model, &terms, &select).map_err(js_err)?;
        Ok(())
    }

    /// Set the ontology/version IRIs and add ontology annotations, in place.
    /// `annotations` is a flat list of alternating `prop, value` tokens
    /// (e.g. `["rdfs:comment", "hello"]`).
    #[wasm_bindgen(js_name = annotate)]
    pub fn annotate(
        &mut self,
        ontology_iri: Option<String>,
        version_iri: Option<String>,
        annotations: Vec<String>,
    ) -> Result<(), JsError> {
        let model = self.take();
        self.model = api::annotate(model, ontology_iri.as_deref(), version_iri.as_deref(), &annotations)
            .map_err(js_err)?;
        Ok(())
    }

    /// Bulk-rename entity IRIs from an old→new map, in place. `mapping` is a JS
    /// object / `Map` of `{ "old_iri": "new_iri" }`.
    #[wasm_bindgen(js_name = rename)]
    pub fn rename(&mut self, mapping: StringMap) -> Result<(), JsError> {
        let mapping: std::collections::HashMap<String, String> =
            serde_wasm_bindgen::from_value(mapping.into()).map_err(js_err)?;
        let model = self.take();
        self.model = api::rename(model, &mapping).map_err(js_err)?;
        Ok(())
    }

    /// Assert inferred existential restrictions, in place. `properties` limits
    /// which object properties to materialize (all if empty).
    #[wasm_bindgen(js_name = materialize)]
    pub fn materialize(&mut self, properties: Vec<String>) {
        let model = self.take();
        self.model = api::materialize(model, &properties);
    }

    /// Extract a module for a seed term set as a new ontology, leaving this one
    /// unchanged. `method` is `BOT`/`TOP`/`STAR` (locality-based) or `MIREOT`.
    #[wasm_bindgen(js_name = extract)]
    pub fn extract(&self, terms: Vec<String>, method: &str) -> Result<Ontology, JsError> {
        let model = api::extract(&self.model, &terms, method).map_err(js_err)?;
        Ok(Ontology { model })
    }

    /// A human-readable diff against another ontology: any ontology-ID change,
    /// then a `N components removed, M added.` count line and one `- removed` /
    /// `+ added` line per differing component (logical axioms plus ontology
    /// metadata such as annotations and imports). When the two sides carry the
    /// same components, a single sentence instead of any `-`/`+` lines.
    #[wasm_bindgen(js_name = diff)]
    pub fn diff(&self, other: &Ontology) -> String {
        api::diff(&self.model, &other.model)
    }

    /// Ontology metrics as tab-separated `metric\tvalue` rows with a header.
    #[wasm_bindgen(js_name = measure)]
    pub fn measure(&self) -> String {
        api::measure(&self.model)
    }

    /// Run a SPARQL SELECT/ASK query over the ontology, returning the result
    /// table as TSV with a header row.
    #[wasm_bindgen(js_name = query)]
    pub fn query(&self, sparql: &str) -> Result<String, JsError> {
        api::query(&self.model, sparql).map_err(js_err)
    }

    /// Run a SPARQL SELECT query, returning the rows as a JS array of
    /// `{column: value}` objects (empty cells omitted). With `reasoner` set
    /// (`"elk"`/…), the ontology is classified first and the query runs over the
    /// entailed graph (inferred axioms included); the ontology is left unchanged.
    #[wasm_bindgen(js_name = queryRecords)]
    pub fn query_records(&self, sparql: &str, reasoner: Option<String>) -> Result<RecordArray, JsError> {
        let table = match reasoner.as_deref() {
            Some(r) => api::query_reasoned(&self.model, sparql, r).map_err(js_err)?,
            None => api::query_table(&self.model, sparql).map_err(js_err)?,
        };
        to_js(&table.records())
    }

    /// A DL query: a Manchester-syntax class `expression`, answered by the
    /// reasoner. `kind` is `subclasses` (direct) / `descendants` (all) /
    /// `superclasses` / `ancestors` / `equivalent` / `instances`, defaulting to
    /// `descendants`; `reasoner` takes the same engine names as `reason()` and
    /// defaults to `"elk"`. Returns the matching entity IRIs.
    #[wasm_bindgen(js_name = dlQuery)]
    pub fn dl_query(
        &self,
        expression: &str,
        kind: Option<String>,
        reasoner: Option<String>,
    ) -> Result<Vec<String>, JsError> {
        api::dl_query(
            &self.model,
            expression,
            kind.as_deref().unwrap_or("descendants"),
            reasoner.as_deref().unwrap_or("elk"),
        )
        .map_err(js_err)
    }

    /// Generate OWL axioms from a template table (TSV text) and merge them into
    /// this ontology in place. The header row names the columns and the first
    /// data row holds each column's template string (`ID`, `LABEL`, `SC %`, …);
    /// every row after that describes one term.
    #[wasm_bindgen(js_name = template)]
    pub fn template(&mut self, tsv: &str) -> Result<(), JsError> {
        let model = self.take();
        self.model = api::template(model, tsv).map_err(js_err)?;
        Ok(())
    }

    /// Merge another ontology into this one in place (this is the base).
    #[wasm_bindgen(js_name = merge)]
    pub fn merge(&mut self, other: &Ontology) {
        api::merge_into(&mut self.model, &other.model);
    }

    /// Take the model out of the handle, leaving an empty one in its place, so
    /// the by-value `api` operations (`reason`, `relax`) can consume it while
    /// the binding keeps `&mut self`.
    fn take(&mut self) -> Model {
        std::mem::replace(&mut self.model, Model::new())
    }
}

impl Default for Ontology {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate an ontology from a DOSDP pattern (YAML text) and a TSV data table,
/// in memory — no files.
#[wasm_bindgen(js_name = dosdp)]
pub fn dosdp(pattern_yaml: &str, data_tsv: &str) -> Result<Ontology, JsError> {
    let model = api::dosdp(pattern_yaml, data_tsv).map_err(js_err)?;
    Ok(Ontology { model })
}

/// An in-memory SSSOM mapping set: a prefix map, set-level metadata, and the
/// mapping records. Each record is a `{slot: value}` object, so the set converts
/// directly to/from tabular data via `records()` / `fromRecords()`.
#[wasm_bindgen]
pub struct MappingSet {
    inner: CoreMappingSet,
}

#[wasm_bindgen]
impl MappingSet {
    /// An empty mapping set.
    #[wasm_bindgen(constructor)]
    pub fn new() -> MappingSet {
        MappingSet { inner: CoreMappingSet::new() }
    }

    /// Parse a mapping set from text. `format` is required, and is one of
    /// `tsv`/`sssom`, `csv`, `json`, `obographs`, or `alignment`/`xml`.
    #[wasm_bindgen(js_name = parse)]
    pub fn parse(text: &str, format: &str) -> Result<MappingSet, JsError> {
        let inner = match format.to_ascii_lowercase().as_str() {
            "tsv" | "sssom" => sssom_io::read_table(text, '\t', None),
            "csv" => sssom_io::read_table(text, ',', None),
            "json" => sssom_io::read_json(text),
            "obographs" => sssom_io::parse_obographs_json(text, None),
            "alignment" | "xml" => sssom_io::parse_alignment_xml(text),
            other => return Err(JsError::new(&format!("unknown sssom input format: {other}"))),
        }
        .map_err(js_err)?;
        Ok(MappingSet { inner })
    }

    /// Serialize the mapping set. `format` is required, and is one of
    /// `tsv`/`sssom`, `csv`, `json`, `ttl`/`turtle`, or `owl`.
    #[wasm_bindgen(js_name = serialize)]
    pub fn serialize(&self, format: &str) -> Result<String, JsError> {
        match format.to_ascii_lowercase().as_str() {
            "tsv" | "sssom" => sssom_io::write_table(&self.inner, '\t', false, false),
            "csv" => sssom_io::write_table(&self.inner, ',', false, false),
            "json" => sssom_io::to_json(&self.inner, false),
            "ttl" | "turtle" => sssom_io::to_turtle(&self.inner, false),
            "owl" => sssom_io::to_turtle(&self.inner, true),
            other => return Err(JsError::new(&format!("unknown sssom output format: {other}"))),
        }
        .map_err(js_err)
    }

    /// The mapping rows as an array of `{slot: value}` objects.
    #[wasm_bindgen(js_name = records)]
    pub fn records(&self) -> Result<RecordArray, JsError> {
        to_js(&self.inner.mappings)
    }

    /// Build a mapping set from an array of `{slot: value}` row objects, with an
    /// optional CURIE-map object.
    #[wasm_bindgen(js_name = fromRecords)]
    pub fn from_records(records: RecordArray, curie_map: Option<StringMap>) -> Result<MappingSet, JsError> {
        let mappings: Vec<BTreeMap<String, String>> =
            serde_wasm_bindgen::from_value(records.into()).map_err(js_err)?;
        let mut inner = CoreMappingSet::new();
        inner.mappings = mappings;
        if let Some(cm) = curie_map {
            inner.curie_map = serde_wasm_bindgen::from_value(cm.into()).map_err(js_err)?;
        }
        inner.recompute_columns();
        Ok(MappingSet { inner })
    }

    /// The CURIE prefix map as a `{ prefix: namespace }` object.
    #[wasm_bindgen(getter, js_name = curieMap)]
    pub fn curie_map(&self) -> Result<StringMap, JsError> {
        to_js(&self.inner.curie_map)
    }

    /// Replace the CURIE prefix map.
    #[wasm_bindgen(setter, js_name = curieMap)]
    pub fn set_curie_map(&mut self, value: StringMap) -> Result<(), JsError> {
        self.inner.curie_map = serde_wasm_bindgen::from_value(value.into()).map_err(js_err)?;
        Ok(())
    }

    /// Sort columns into canonical order and rows by (subject, predicate,
    /// object), in place.
    #[wasm_bindgen(js_name = sort)]
    pub fn sort(&mut self) {
        self.inner.sort_columns_canonical();
        self.inner.sort_rows();
    }

    /// Canonicalize the mapping set, in place.
    #[wasm_bindgen(js_name = canonicalize)]
    pub fn canonicalize(&mut self) {
        self.inner.canonicalize();
    }

    /// Condense multi-valued slots, in place.
    #[wasm_bindgen(js_name = condense)]
    pub fn condense(&mut self) {
        self.inner.condense();
    }

    /// Propagate set-level slots onto each row, in place.
    #[wasm_bindgen(js_name = propagate)]
    pub fn propagate(&mut self) {
        self.inner.propagate();
    }

    /// Merge another mapping set into this one (rows appended, CURIE maps
    /// unioned), in place.
    #[wasm_bindgen(js_name = merge)]
    pub fn merge(&mut self, other: &MappingSet) {
        self.inner.mappings.extend(other.inner.mappings.iter().cloned());
        for (k, v) in &other.inner.curie_map {
            self.inner.curie_map.entry(k.clone()).or_insert_with(|| v.clone());
        }
        self.inner.recompute_columns();
    }

    /// Number of mapping rows (the `size` accessor, mirroring `Map`/`Set`).
    #[wasm_bindgen(getter, js_name = size)]
    pub fn size(&self) -> usize {
        self.inner.mappings.len()
    }
}

impl Default for MappingSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a SSSOM mapping set between serializations in memory. `input` is the
/// mapping-set text, `to` the output format (`tsv`/`sssom`, `csv`, `json`,
/// `ttl`/`turtle`, or `owl`), and `from` the input format (`tsv`/`sssom`, `csv`,
/// `json`, `obographs`, or `alignment`/`xml`), which defaults to `tsv` when
/// omitted.
#[wasm_bindgen(js_name = sssomConvert)]
pub fn sssom_convert(input: &str, to: &str, from: Option<String>) -> Result<String, JsError> {
    api::sssom_convert(input, from.as_deref().unwrap_or("tsv"), to).map_err(js_err)
}
