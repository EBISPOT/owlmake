//! Reading and writing SSSOM mapping sets.
//!
//! Primary format is SSSOM/TSV: a YAML metadata header (each line prefixed with
//! `# `) carrying `curie_map` plus mapping-set metadata, followed by a TSV table
//! of mapping records. We also read/write SSSOM JSON, emit RDF/Turtle and an OWL
//! (reified-axiom) RDF rendering, and parse OBO Graphs JSON and Alignment-API XML
//! inputs for `sssom parse`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::{
    flatten_value, Mapping, MappingSet, BUILTIN_PREFIXES,
    MAPPING_JUSTIFICATION_UNSPECIFIED,
};

/// Separator for a serialisation name (`csv` → comma, everything else → tab).
pub fn separator(serialisation: &str) -> char {
    if serialisation == "csv" {
        ','
    } else {
        '\t'
    }
}

// ───────────────────────────── TSV/CSV reading ──────────────────────────────

/// Parse a SSSOM table (TSV or CSV) from `text`. An external metadata YAML may be
/// supplied (used when the table has no embedded `# ` header, or to override it).
pub fn read_table(text: &str, sep: char, external_meta: Option<&str>) -> Result<MappingSet> {
    let mut header_yaml = String::new();
    let mut body_start = 0usize;
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.strip_prefix('#') {
            // Drop exactly one leading space: a header line is `#` or `# ` followed
            // by the YAML line, and further indentation is part of the YAML.
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            header_yaml.push_str(rest);
            header_yaml.push('\n');
            body_start = i + 1;
        } else {
            body_start = i;
            break;
        }
    }

    let mut ms = MappingSet::new();

    // Metadata: embedded header overlaid by an external metadata file.
    let mut meta_map: serde_yaml::Mapping = serde_yaml::Mapping::new();
    if !header_yaml.trim().is_empty() {
        let v: serde_yaml::Value = serde_yaml::from_str(&header_yaml)
            .context("parsing embedded SSSOM metadata header")?;
        if let serde_yaml::Value::Mapping(m) = v {
            meta_map = m;
        }
    }
    if let Some(ext) = external_meta {
        let v: serde_yaml::Value =
            serde_yaml::from_str(ext).context("parsing external SSSOM metadata file")?;
        if let serde_yaml::Value::Mapping(m) = v {
            for (k, val) in m {
                meta_map.insert(k, val);
            }
        }
    }
    apply_metadata(&mut ms, meta_map);

    // Body: first non-comment line is the column header.
    if body_start < lines.len() {
        let columns: Vec<String> =
            split_row(lines[body_start], sep).into_iter().map(|s| s.to_string()).collect();
        ms.columns = columns.clone();
        for line in &lines[body_start + 1..] {
            if line.is_empty() {
                continue;
            }
            let cells = split_row(line, sep);
            let mut rec = BTreeMap::new();
            for (j, col) in columns.iter().enumerate() {
                if let Some(val) = cells.get(j) {
                    if !val.is_empty() {
                        rec.insert(col.clone(), val.clone());
                    }
                }
            }
            if !rec.is_empty() {
                ms.mappings.push(rec);
            }
        }
    }
    Ok(ms)
}

/// Split a delimited row, supporting minimal CSV quoting for the `csv` flavour.
fn split_row(line: &str, sep: char) -> Vec<String> {
    if sep == '\t' {
        return line.split('\t').map(|s| s.to_string()).collect();
    }
    // Minimal RFC-4180-ish CSV parsing.
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == sep {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// Distribute a parsed metadata YAML mapping into `curie_map` and `metadata`.
fn apply_metadata(ms: &mut MappingSet, map: serde_yaml::Mapping) {
    for (k, v) in map {
        let key = match k {
            serde_yaml::Value::String(s) => s,
            other => super::value_to_cell(&other),
        };
        if key == "curie_map" {
            if let serde_yaml::Value::Mapping(cm) = v {
                for (pk, pv) in cm {
                    if let (serde_yaml::Value::String(p), serde_yaml::Value::String(n)) = (pk, pv) {
                        ms.curie_map.insert(p, n);
                    }
                }
            }
        } else if key == "mappings" {
            // Inline mappings (JSON-style) are handled by the JSON reader.
        } else {
            ms.metadata.insert(key, v);
        }
    }
}

// ───────────────────────────── TSV/CSV writing ──────────────────────────────

/// Which SSSOM/TSV header convention to write. Mapping sets in circulation use
/// two, and a build both writes new sets and reads back sets of either shape.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MetaStyle {
    /// `# ` before every header line, with the keys sorted alphabetically.
    Python,
    /// A bare `#`, with the set-level slots in the order the SSSOM schema declares
    /// them rather than alphabetically — `mapping_set_id` before `license` before
    /// `subject_source`. `curie_map` leads either way.
    Java,
}

/// The SSSOM schema's declaration order for the set-level slots
/// [`MetaStyle::Java`] leads with. Anything not named here follows,
/// alphabetically.
/// `curie_map` leads; everything after it follows the schema's own declaration
/// order, which is already written down once as [`SLOT_ORDER`]. Keeping a second,
/// shorter copy here meant a slot the copy omitted fell to the alphabetical
/// fallback: `creator_id` sorted AFTER `license`, where the schema — and the
/// reference output for a merged set — puts it before.
fn java_meta_rank(k: &str) -> usize {
    if k == "curie_map" {
        return 0;
    }
    crate::sssom::SLOT_ORDER
        .iter()
        .position(|s| *s == k)
        .map_or(usize::MAX - 1, |i| i + 1)
}

/// Serialize a mapping set as a SSSOM table (embedded YAML header + TSV/CSV
/// body). `condense` lifts constant propagatable columns; `sort` canonicalizes.
pub fn write_table(ms: &MappingSet, sep: char, condense: bool, sort: bool) -> Result<String> {
    write_table_styled(ms, sep, condense, sort, MetaStyle::Python)
}

pub fn write_table_styled(
    ms: &MappingSet,
    sep: char,
    condense: bool,
    sort: bool,
    style: MetaStyle,
) -> Result<String> {
    let mut ms = ms.clone();
    if condense {
        ms.condense();
    }
    if sort {
        ms.sort_columns();
        ms.sort_rows();
    }

    // Header: metadata + curie_map, dumped as YAML.
    let mut pairs: Vec<(String, serde_yaml::Value)> =
        ms.metadata.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    if !ms.curie_map.is_empty() {
        let mut cm = serde_yaml::Mapping::new();
        for (p, n) in &ms.curie_map {
            cm.insert(serde_yaml::Value::String(p.clone()), serde_yaml::Value::String(n.clone()));
        }
        pairs.push(("curie_map".to_string(), serde_yaml::Value::Mapping(cm)));
    }
    let rank = |k: &str| -> usize {
        match style {
            MetaStyle::Python => 0,
            MetaStyle::Java => {
                java_meta_rank(k)
            }
        }
    };
    pairs.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0)));
    let mut meta = serde_yaml::Mapping::new();
    for (k, v) in pairs {
        meta.insert(serde_yaml::Value::String(k), v);
    }

    let mut out = String::new();
    if !meta.is_empty() {
        let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(meta))
            .context("serializing SSSOM metadata header")?;
        for line in yaml.lines() {
            if line == "---" || line.is_empty() {
                continue;
            }
            out.push_str(match style {
                MetaStyle::Python => "# ",
                MetaStyle::Java => "#",
            });
            // `serde_yaml` writes a block sequence flush with its key; the Java
            // writer indents it under the key. So a `creator_id` list reads
            // `#  - ORCID:…`, not `#- ORCID:…`.
            if style == MetaStyle::Java && line.starts_with("- ") {
                out.push_str("  ");
            }
            out.push_str(line);
            out.push('\n');
        }
    }

    // Body.
    let cols = &ms.columns;
    out.push_str(&cols.join(&sep.to_string()));
    out.push('\n');
    for m in &ms.mappings {
        let row: Vec<String> = cols
            .iter()
            .map(|c| {
                let raw = m.get(c).map(String::as_str).unwrap_or("");
                let cell = if style == MetaStyle::Java
                    && !raw.is_empty()
                    && crate::sssom::NUMERIC_SLOTS.contains(&c.as_str())
                {
                    render_numeric(raw)
                } else {
                    raw.to_string()
                };
                escape_cell(&cell, sep)
            })
            .collect();
        out.push_str(&row.join(&sep.to_string()));
        out.push('\n');
    }
    Ok(out)
}

/// Render a numeric cell as the table carries numbers: the value rounded to
/// three decimal places, with trailing zeros and any trailing point removed.
///
/// So `0.10` is written `0.1`, `1.0` is written `1`, and a confidence carrying
/// more precision than the column keeps — `0.3333333333333333` — is written
/// `0.333`. Rounding is of the value itself rather than of the digits it was
/// spelled with, which is why `0.1235` gives `0.123`: the nearest double to
/// `0.1235` is below it, so three places round down.
///
/// A cell that is not a number is left exactly as it stands.
fn render_numeric(v: &str) -> String {
    match v.trim().parse::<f64>() {
        Ok(x) if x.is_finite() => {
            let s = format!("{x:.3}");
            let s = s.trim_end_matches('0').trim_end_matches('.');
            if s.is_empty() || s == "-" {
                "0".to_string()
            } else {
                s.to_string()
            }
        }
        _ => v.to_string(),
    }
}

fn escape_cell(v: &str, sep: char) -> String {
    if sep == ',' && (v.contains(',') || v.contains('"') || v.contains('\n')) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

// ───────────────────────────────── JSON ─────────────────────────────────────

/// Serialize a mapping set as SSSOM JSON (a JSON object with mapping-set metadata,
/// `curie_map`, and a `mappings` array).
pub fn to_json(ms: &MappingSet, condense: bool) -> Result<String> {
    let mut ms = ms.clone();
    if condense {
        ms.condense();
    }
    let mut obj = serde_json::Map::new();
    for (k, v) in &ms.metadata {
        obj.insert(k.clone(), yaml_to_json(v));
    }
    if !ms.curie_map.is_empty() {
        let cm: serde_json::Map<String, serde_json::Value> = ms
            .curie_map
            .iter()
            .map(|(p, n)| (p.clone(), serde_json::Value::String(n.clone())))
            .collect();
        obj.insert("curie_map".into(), serde_json::Value::Object(cm));
    }
    let maps: Vec<serde_json::Value> = ms
        .mappings
        .iter()
        .map(|m| {
            let mut row = serde_json::Map::new();
            for (k, v) in m {
                if super::is_multivalued(k) {
                    row.insert(
                        k.clone(),
                        serde_json::Value::Array(
                            super::split_multivalued(v)
                                .into_iter()
                                .map(serde_json::Value::String)
                                .collect(),
                        ),
                    );
                } else {
                    row.insert(k.clone(), serde_json::Value::String(v.clone()));
                }
            }
            serde_json::Value::Object(row)
        })
        .collect();
    obj.insert("mappings".into(), serde_json::Value::Array(maps));
    Ok(serde_json::to_string_pretty(&serde_json::Value::Object(obj))?)
}

/// Parse SSSOM JSON into a mapping set.
pub fn read_json(text: &str) -> Result<MappingSet> {
    let v: serde_json::Value = serde_json::from_str(text).context("parsing SSSOM JSON")?;
    let obj = v.as_object().context("SSSOM JSON: expected a top-level object")?;
    let mut ms = MappingSet::new();
    for (k, val) in obj {
        match k.as_str() {
            "mappings" => {
                if let Some(arr) = val.as_array() {
                    for m in arr {
                        if let Some(row) = m.as_object() {
                            let mut rec = BTreeMap::new();
                            for (rk, rv) in row {
                                rec.insert(rk.clone(), json_cell(rv));
                            }
                            ms.mappings.push(rec);
                        }
                    }
                }
            }
            "curie_map" => {
                if let Some(cm) = val.as_object() {
                    for (p, n) in cm {
                        if let Some(ns) = n.as_str() {
                            ms.curie_map.insert(p.clone(), ns.to_string());
                        }
                    }
                }
            }
            _ => {
                ms.metadata.insert(k.clone(), json_to_yaml(val));
            }
        }
    }
    ms.recompute_columns();
    Ok(ms)
}

fn json_cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(json_scalar).collect();
            super::join_multivalued(&parts)
        }
        other => json_scalar(other),
    }
}
fn json_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn yaml_to_json(v: &serde_yaml::Value) -> serde_json::Value {
    match v {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::from(i)
            } else if let Some(f) = n.as_f64() {
                serde_json::Value::from(f)
            } else {
                serde_json::Value::String(n.to_string())
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            serde_json::Value::Array(seq.iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, v)| (super::value_to_cell(k), yaml_to_json(v)))
                .collect(),
        ),
        serde_yaml::Value::Tagged(t) => yaml_to_json(&t.value),
    }
}
fn json_to_yaml(v: &serde_json::Value) -> serde_yaml::Value {
    match v {
        serde_json::Value::Null => serde_yaml::Value::Null,
        serde_json::Value::Bool(b) => serde_yaml::Value::Bool(*b),
        serde_json::Value::Number(n) => serde_yaml::from_str(&n.to_string()).unwrap_or(serde_yaml::Value::String(n.to_string())),
        serde_json::Value::String(s) => serde_yaml::Value::String(s.clone()),
        serde_json::Value::Array(a) => serde_yaml::Value::Sequence(a.iter().map(json_to_yaml).collect()),
        serde_json::Value::Object(o) => serde_yaml::Value::Mapping(
            o.iter()
                .map(|(k, v)| (serde_yaml::Value::String(k.clone()), json_to_yaml(v)))
                .collect(),
        ),
    }
}

// ───────────────────────────── RDF / OWL output ─────────────────────────────

/// Render the mapping set as RDF/Turtle. Each mapping is an `owl:Axiom`
/// (reified) node hanging off the set via `sssom:mappings`. The `owl` form
/// additionally emits the direct `subject predicate object` triple and types the
/// set as an `owl:Ontology`, so the mappings arrive as asserted axioms an OWL
/// consumer can reason over rather than as annotations about a set.
pub fn to_turtle(ms: &MappingSet, owl: bool) -> Result<String> {
    let pm = ms.effective_prefixes();
    // The prefixes the document DECLARES are the ones its terms use — a declared
    // prefix no term needs is not written. `owl:` is always among them: the type
    // of the set and of every reified record.
    let mut used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    used.insert("owl".to_string());
    let mut out = String::new();

    let set_iri = ms
        .metadata
        .get("mapping_set_id")
        .map(super::value_to_cell)
        .unwrap_or_else(|| format!("{}mappings/unknown", super::SSSOM_URI_PREFIX));
    let set_term = curie_term(&set_iri, &pm, &mut used);

    if !owl {
        used.insert("sssom".to_string());
    }
    out.push_str(&format!("{set_term} a {} ", if owl { "owl:Ontology" } else { "sssom:MappingSet" }));
    // Set-level metadata.
    for (k, v) in &ms.metadata {
        if k == "mapping_set_id" {
            continue;
        }
        for s in flatten_value(v) {
            let pred = super::slot_predicate(k);
            note_prefix(&pred, &mut used);
            out.push_str(&format!(";\n    {pred} {} ", lit_or_iri(k, &s, &pm, &mut used)));
        }
    }
    out.push_str(".\n\n");

    for m in &ms.mappings {
        let subj = m.get("subject_id").cloned().unwrap_or_default();
        let pred = m.get("predicate_id").cloned().unwrap_or_default();
        let obj = m.get("object_id").cloned().unwrap_or_default();

        if owl && !subj.is_empty() && !pred.is_empty() && !obj.is_empty() {
            // Direct, hydrated triple.
            out.push_str(&format!(
                "{} {} {} .\n",
                curie_term(&subj, &pm, &mut used),
                curie_term(&pred, &pm, &mut used),
                curie_term(&obj, &pm, &mut used)
            ));
        }

        // Reified record: a standalone blank node in OWL mode, otherwise hung off
        // the mapping set via `sssom:mappings`.
        if owl {
            out.push_str("[ a owl:Axiom");
        } else {
            used.insert("sssom".to_string());
            out.push_str(&format!("{set_term} sssom:mappings [ a owl:Axiom"));
        }
        if !subj.is_empty() {
            out.push_str(&format!(
                " ;\n    owl:annotatedSource {}",
                curie_term(&subj, &pm, &mut used)
            ));
        }
        if !pred.is_empty() {
            out.push_str(&format!(
                " ;\n    owl:annotatedProperty {}",
                curie_term(&pred, &pm, &mut used)
            ));
        }
        if !obj.is_empty() {
            out.push_str(&format!(
                " ;\n    owl:annotatedTarget {}",
                curie_term(&obj, &pm, &mut used)
            ));
        }
        for (k, v) in m {
            if matches!(k.as_str(), "subject_id" | "predicate_id" | "object_id") {
                continue;
            }
            for s in v.split('|') {
                let slot_pred = super::slot_predicate(k);
                note_prefix(&slot_pred, &mut used);
                out.push_str(&format!(" ;\n    {slot_pred} {}", lit_or_iri(k, s, &pm, &mut used)));
            }
        }
        if owl {
            out.push_str(" ] .\n");
        } else {
            out.push_str(" ] .\n");
        }
    }
    // The declarations, in one alphabetical run. A prefix a slot's own name needs
    // but the set's `curie_map` does not declare — `dcterms:`, `pav:`, `prov:` —
    // is declared here too, or the document does not parse.
    let mut decls: BTreeMap<&str, &str> = BTreeMap::new();
    for (p, n) in &pm {
        if used.contains(p) {
            decls.insert(p.as_str(), n.as_str());
        }
    }
    for (p, n) in SLOT_PREFIXES {
        if used.contains(*p) {
            decls.entry(p).or_insert(n);
        }
    }
    let mut doc = String::new();
    for (p, n) in &decls {
        doc.push_str(&format!("@prefix {p}: <{n}> .\n"));
    }
    doc.push('\n');
    doc.push_str(&out);
    Ok(doc)
}

/// The namespaces of the prefixes a slot's own RDF name can use, for a set whose
/// `curie_map` does not declare them.
const SLOT_PREFIXES: &[(&str, &str)] = &[
    ("dcterms", "http://purl.org/dc/terms/"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("pav", "http://purl.org/pav/"),
    ("prov", "http://www.w3.org/ns/prov#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("sssom", "https://w3id.org/sssom/"),
];

/// Record the prefix a written CURIE uses, so the document declares it.
fn note_prefix(term: &str, used: &mut std::collections::BTreeSet<String>) {
    if let Some((p, _)) = term.split_once(':') {
        if !p.is_empty() && !term.starts_with('<') {
            used.insert(p.to_string());
        }
    }
}

/// A slot's RDF object: an IRI term for entity references, a typed literal for
/// the numeric/date slots (per the SSSOM schema's slot ranges), else a plain
/// string literal.
fn lit_or_iri(
    slot: &str,
    val: &str,
    pm: &BTreeMap<String, String>,
    used: &mut std::collections::BTreeSet<String>,
) -> String {
    if super::is_uri_valued(slot) {
        return curie_term(val, pm, used);
    }
    let esc = val.replace('\\', "\\\\").replace('"', "\\\"");
    match slot_datatype(slot) {
        // Only type a numeric slot when the value actually parses, so a malformed
        // cell degrades to a plain string rather than an invalid typed literal.
        Some(dt @ "http://www.w3.org/2001/XMLSchema#double") if val.parse::<f64>().is_ok() => {
            format!("\"{esc}\"^^<{dt}>")
        }
        Some(dt @ "http://www.w3.org/2001/XMLSchema#date") if is_xsd_date(val) => {
            format!("\"{esc}\"^^<{dt}>")
        }
        _ => format!("\"{esc}\""),
    }
}

/// Whether `s` is a well-formed `xsd:date` (`YYYY-MM-DD`), so a malformed cell
/// degrades to a plain literal instead of an invalid typed literal.
fn is_xsd_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// The XSD datatype IRI for a typed SSSOM slot (double / date), per the schema's
/// slot ranges; `None` for plain-string slots.
fn slot_datatype(slot: &str) -> Option<&'static str> {
    match slot {
        "confidence" | "similarity_score" | "mapping_set_confidence" => {
            Some("http://www.w3.org/2001/XMLSchema#double")
        }
        "mapping_date" | "publication_date" | "review_date" | "last_updated" => {
            Some("http://www.w3.org/2001/XMLSchema#date")
        }
        _ => None,
    }
}

/// Render a CURIE/IRI as a Turtle term: keep prefixed form if the prefix is known,
/// else as a full `<IRI>`.
fn curie_term(
    curie: &str,
    pm: &BTreeMap<String, String>,
    used: &mut std::collections::BTreeSet<String>,
) -> String {
    if curie.starts_with("http://") || curie.starts_with("https://") {
        return format!("<{curie}>");
    }
    match curie.split_once(':') {
        Some((p, _)) if pm.contains_key(p) => {
            used.insert(p.to_string());
            curie.to_string()
        }
        _ => format!("<{curie}>"),
    }
}

// ────────────────────────── alternative input parsers ───────────────────────

/// Read mappings out of an OBO Graphs document.
///
/// Three sources of mappings, in this order per graph: each node's
/// `meta.xrefs` (as `oboInOwl:hasDbXref`) and the `meta.basicPropertyValues`
/// whose predicate is a mapping predicate; then each `edge` whose predicate is
/// one (`is_a` reading as `rdfs:subClassOf`, which is not one, so plain
/// subclass edges never become mappings); then every ORDERED pair drawn from
/// each `equivalentNodesSets` entry.
///
/// The predicate set is the four SKOS `*Match` predicates, `relatedMatch`,
/// `oboInOwl:hasDbXref` and `owl:equivalentClass`, and nothing else. Taking
/// every non-`is_a` edge instead fills MONDO's mapping set with `RO:0002162`
/// "in taxon" assertions.
///
/// Every id is compressed to a CURIE through the chained converter (see
/// [`crate::sssom::converter`]); a field whose IRI no prefix covers is left
/// OUT of the record, and any record then missing a required slot is dropped.
/// Labels come from the nodes' own `lbl`.
pub fn parse_obographs_json(text: &str, external_meta: Option<&str>) -> Result<MappingSet> {
    use crate::sssom::converter::Converter;

    /// `DEFAULT_MAPPING_PROPERTIES` (`sssom/constants.py`).
    const MAPPING_PREDICATES: &[&str] = &[
        "http://www.w3.org/2004/02/skos/core#exactMatch",
        "http://www.w3.org/2004/02/skos/core#closeMatch",
        "http://www.w3.org/2004/02/skos/core#broadMatch",
        "http://www.w3.org/2004/02/skos/core#narrowMatch",
        "http://www.geneontology.org/formats/oboInOwl#hasDbXref",
        "http://www.w3.org/2004/02/skos/core#relatedMatch",
        "http://www.w3.org/2002/07/owl#equivalentClass",
    ];
    const HAS_DB_XREF: &str = "http://www.geneontology.org/formats/oboInOwl#hasDbXref";
    const EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
    const SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

    let v: serde_json::Value = serde_json::from_str(text).context("parsing OBO Graphs JSON")?;
    let mut ms = MappingSet::new();
    if let Some(ext) = external_meta {
        let y: serde_yaml::Value =
            serde_yaml::from_str(ext).context("parsing external SSSOM metadata file")?;
        if let serde_yaml::Value::Mapping(m) = y {
            apply_metadata(&mut ms, m);
        }
    }
    let converter = Converter::merged_with_metadata(&ms.curie_map);

    let Some(graphs) = v.get("graphs").and_then(|g| g.as_array()) else {
        bail!("obographs-json: no `graphs` array found");
    };

    // One label table over ALL graphs, built before any mapping is made.
    let empty = Vec::new();
    let mut labels: BTreeMap<&str, &str> = BTreeMap::new();
    for g in graphs {
        for n in g.get("nodes").and_then(|n| n.as_array()).unwrap_or(&empty) {
            if let (Some(id), Some(lbl)) =
                (n.get("id").and_then(|x| x.as_str()), n.get("lbl").and_then(|x| x.as_str()))
            {
                labels.insert(id, lbl);
            }
        }
    }

    // `_make_mdict` + `_add_valid_mapping_to_list`: compress each of the three
    // ids, omitting any that will not compress, then keep the record only if
    // every required slot survived.
    let mut push = |ms: &mut MappingSet, subject: &str, predicate: &str, object: &str| {
        let mut row: Mapping = BTreeMap::new();
        row.insert("mapping_justification".into(), MAPPING_JUSTIFICATION_UNSPECIFIED.to_string());
        let s = converter.safe_compress(subject);
        let p = converter.safe_compress(predicate);
        let o = converter.safe_compress(object);
        if let Some(s) = &s {
            row.insert("subject_id".into(), s.clone());
        }
        if let Some(p) = &p {
            row.insert("predicate_id".into(), p.clone());
        }
        if let Some(o) = &o {
            row.insert("object_id".into(), o.clone());
        }
        if let Some(l) = labels.get(subject) {
            row.insert("subject_label".into(), (*l).to_string());
        }
        if let Some(l) = labels.get(object) {
            row.insert("object_label".into(), (*l).to_string());
        }
        if s.is_none() || p.is_none() || o.is_none() {
            return;
        }
        ms.mappings.push(row);
    };

    for g in graphs {
        for n in g.get("nodes").and_then(|n| n.as_array()).unwrap_or(&empty) {
            let Some(id) = n.get("id").and_then(|x| x.as_str()) else { continue };
            let Some(meta) = n.get("meta") else { continue };
            for x in meta.get("xrefs").and_then(|x| x.as_array()).unwrap_or(&empty) {
                if let Some(val) = x.get("val").and_then(|v| v.as_str()) {
                    push(&mut ms, id, HAS_DB_XREF, val);
                }
            }
            for bpv in meta.get("basicPropertyValues").and_then(|x| x.as_array()).unwrap_or(&empty)
            {
                let (Some(pred), Some(val)) = (
                    bpv.get("pred").and_then(|v| v.as_str()),
                    bpv.get("val").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                if MAPPING_PREDICATES.contains(&pred) {
                    push(&mut ms, id, pred, val);
                }
            }
        }
        for e in g.get("edges").and_then(|e| e.as_array()).unwrap_or(&empty) {
            let (Some(sub), Some(pred), Some(obj)) = (
                e.get("sub").and_then(|x| x.as_str()),
                e.get("pred").and_then(|x| x.as_str()),
                e.get("obj").and_then(|x| x.as_str()),
            ) else {
                continue;
            };
            let pred = if pred == "is_a" { SUBCLASS_OF } else { pred };
            if MAPPING_PREDICATES.contains(&pred) {
                push(&mut ms, sub, pred, obj);
            }
        }
        for set in g.get("equivalentNodesSets").and_then(|x| x.as_array()).unwrap_or(&empty) {
            let Some(ids) = set.get("nodeIds").and_then(|x| x.as_array()) else { continue };
            let ids: Vec<&str> = ids.iter().filter_map(|x| x.as_str()).collect();
            for s in &ids {
                for o in &ids {
                    if s != o {
                        push(&mut ms, s, EQUIVALENT_CLASS, o);
                    }
                }
            }
        }
    }

    // The written `curie_map` is the converter's binding for every prefix the
    // records actually use — not the metadata file's map, which declares
    // prefixes the set never mentions and omits the ones the EPM supplied.
    ms.curie_map.clear();
    for m in &ms.mappings {
        for k in ["subject_id", "predicate_id", "object_id"] {
            if let Some((p, _)) = m.get(k).and_then(|v| v.split_once(':')) {
                if !ms.curie_map.contains_key(p) {
                    if let Some(ns) = converter.uri_prefix(p) {
                        ms.curie_map.insert(p.to_string(), ns.to_string());
                    }
                }
            }
        }
    }
    ms.recompute_columns();
    Ok(ms)
}

/// Parse Alignment-API XML (the `<map><Cell>` form) into SSSOM mappings.
pub fn parse_alignment_xml(text: &str) -> Result<MappingSet> {
    // Lightweight, dependency-free scan of `<Cell>` blocks. The Alignment format
    // is regular enough that we can extract entity1/entity2/relation/measure
    // without a full XML parser.
    let mut ms = MappingSet::new();
    for cell in split_between(text, "<Cell", "</Cell>") {
        let e1 = attr_value(&cell, "entity1");
        let e2 = attr_value(&cell, "entity2");
        let rel = tag_text(&cell, "relation").unwrap_or_else(|| "=".to_string());
        let measure = tag_text(&cell, "measure");
        if let (Some(s), Some(o)) = (e1, e2) {
            let predicate = match rel.trim() {
                "=" => "skos:exactMatch",
                "<" => "skos:broadMatch",
                ">" => "skos:narrowMatch",
                _ => "skos:relatedMatch",
            };
            let mut row = mapping_row(&s, predicate, &o);
            if let Some(c) = measure {
                row.insert("confidence".into(), c.trim().to_string());
            }
            ms.mappings.push(row);
        }
    }
    ms.recompute_columns();
    Ok(ms)
}

fn mapping_row(s: &str, p: &str, o: &str) -> BTreeMap<String, String> {
    let mut row = BTreeMap::new();
    row.insert("subject_id".into(), s.to_string());
    row.insert("predicate_id".into(), p.to_string());
    row.insert("object_id".into(), o.to_string());
    row.insert("mapping_justification".into(), MAPPING_JUSTIFICATION_UNSPECIFIED.to_string());
    row
}

fn split_between(text: &str, open: &str, close: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let after = &rest[start..];
        if let Some(end) = after.find(close) {
            out.push(after[..end].to_string());
            rest = &after[end + close.len()..];
        } else {
            break;
        }
    }
    out
}
fn attr_value(s: &str, attr: &str) -> Option<String> {
    // Matches `attr="..."` or `attr='...'` (rdf:resource for entity1/entity2).
    for marker in [format!("{attr} rdf:resource=\""), format!("{attr}=\"")] {
        if let Some(i) = s.find(&marker) {
            let after = &s[i + marker.len()..];
            if let Some(end) = after.find('"') {
                return Some(after[..end].to_string());
            }
        }
    }
    None
}
fn tag_text(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let i = s.find(&open)?;
    let after = &s[i + open.len()..];
    let end = after.find(&close)?;
    Some(after[..end].to_string())
}

// ─────────────────────────── file convenience ──────────────────────────────

/// Read a mapping set from a path, dispatching on the (optional) input format,
/// else the file extension.
pub fn read_path(path: &Path, format: Option<&str>, metadata: Option<&Path>) -> Result<MappingSet> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let fmt = format
        .map(str::to_string)
        .or_else(|| path.extension().and_then(|e| e.to_str()).map(str::to_string))
        .unwrap_or_else(|| "tsv".to_string());
    let ext_meta = match metadata {
        Some(p) => Some(
            std::fs::read_to_string(p).with_context(|| format!("reading metadata {}", p.display()))?,
        ),
        None => None,
    };
    match fmt.as_str() {
        "tsv" => read_table(&text, '\t', ext_meta.as_deref()),
        "csv" => read_table(&text, ',', ext_meta.as_deref()),
        "json" => read_json(&text),
        "obographs-json" => parse_obographs_json(&text, ext_meta.as_deref()),
        "alignment-api-xml" => parse_alignment_xml(&text),
        "rdf" => bail!("sssom: rdf input parsing is not yet supported"),
        other => bail!("sssom: unknown input format '{other}'"),
    }
}

/// Built-in prefix lookup (used by validation).
pub fn builtin_namespace(prefix: &str) -> Option<&'static str> {
    BUILTIN_PREFIXES.iter().find(|(p, _)| *p == prefix).map(|(_, n)| *n)
}
