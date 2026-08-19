//! SSSOM 1.1 conformance: enum vocabularies, version inference, and the
//! validator that enforces the spec's structural, enum, datatype, cross-field,
//! prefix, version, extension and record-id rules.
//!
//! The vocabularies are those the published SSSOM 1.1 LinkML schema defines, and
//! the rules those of the prose spec at <https://mapping-commons.github.io/sssom/>.

use super::{MappingSet, SLOT_ORDER};

pub const SSSOM_VERSION_1_0: &str = "1.0";
pub const SSSOM_VERSION_1_1: &str = "1.1";

/// `sssom:NoTermFound` — the sentinel for "this side has no matching term".
pub const NO_TERM_FOUND: &str = "sssom:NoTermFound";

/// `entity_type_enum` permissible values (subject/object/predicate `_type`).
pub const ENTITY_TYPES: &[&str] = &[
    "owl class",
    "owl object property",
    "owl data property",
    "owl annotation property",
    "owl named individual",
    "skos concept",
    "rdfs resource",
    "rdfs class",
    "rdfs literal",
    "rdfs datatype",
    "rdf property",
    "composed entity expression",
];
/// The entity type marking a *literal* mapping side (id may be empty; the label
/// carries the literal).
pub const RDFS_LITERAL: &str = "rdfs literal";
/// `composed entity expression` — like `rdfs literal`, forbidden in `predicate_type`.
pub const COMPOSED_ENTITY: &str = "composed entity expression";

/// `predicate_modifier_enum`.
pub const PREDICATE_MODIFIERS: &[&str] = &["Not"];

/// `mapping_cardinality_enum` (incl. the NoTermFound `1:0`/`0:1`/`0:0` cases).
pub const MAPPING_CARDINALITIES: &[&str] = &["1:1", "1:n", "n:1", "n:n", "1:0", "0:1", "0:0"];

/// Permissible `mapping_justification` values (semapv terms).
pub const MAPPING_JUSTIFICATIONS: &[&str] = &[
    "semapv:LexicalMatching",
    "semapv:LogicalReasoning",
    "semapv:CompositeMatching",
    "semapv:UnspecifiedMatching",
    "semapv:SemanticSimilarityThresholdMatching",
    "semapv:LexicalSimilarityThresholdMatching",
    "semapv:MappingChaining",
    "semapv:MappingReview",
    "semapv:ManualMappingCuration",
    "semapv:MappingInversion",
    "semapv:StructuralMatching",
    "semapv:InstanceBasedMatching",
    "semapv:BackgroundKnowledgeBasedMatching",
];

/// Slots whose presence (on a mapping or the set) was added in 1.1 and therefore
/// forces `sssom_version = 1.1`.
pub const SLOTS_ADDED_1_1: &[&str] = &[
    "sssom_version",
    "mapping_set_confidence",
    "predicate_type",
    "cardinality_scope",
    "mapping_tool_id",
    "review_date",
    "reviewer_agreement",
    "record_id",
    "derived_from",
];

/// Mapping-set-level slots added in 1.1 (they pre-existed on `Mapping`); their
/// presence in *set metadata* also forces 1.1.
const SET_SLOTS_ADDED_1_1: &[&str] = &["similarity_measure", "curation_rule", "curation_rule_text"];

/// Infer the lowest SSSOM version that defines every feature the set uses.
pub fn infer_version(ms: &MappingSet) -> &'static str {
    // A 1.1 slot used on any record.
    let record_1_1 = ms
        .mappings
        .iter()
        .any(|m| m.keys().any(|k| SLOTS_ADDED_1_1.contains(&k.as_str())));
    // A 1.1 slot, or a 1.1 MappingSet-level slot, in the metadata.
    let meta_1_1 = ms
        .metadata
        .keys()
        .any(|k| SLOTS_ADDED_1_1.contains(&k.as_str()) || SET_SLOTS_ADDED_1_1.contains(&k.as_str()));
    // The `composed entity expression` entity type, or a `0:0` cardinality.
    let value_1_1 = ms.mappings.iter().any(|m| {
        ["subject_type", "object_type", "predicate_type"]
            .iter()
            .any(|t| m.get(*t).map(|v| v == COMPOSED_ENTITY).unwrap_or(false))
            || m.get("mapping_cardinality").map(|v| v == "0:0").unwrap_or(false)
    });
    if record_1_1 || meta_1_1 || value_1_1 {
        SSSOM_VERSION_1_1
    } else {
        SSSOM_VERSION_1_0
    }
}

/// Validation categories. The default `validate` runs them all. `structure` is
/// raw-text-only (see [`structure`]); the rest operate on the parsed model.
pub const ALL_CATEGORIES: &[&str] = &[
    "required", "enum", "datatype", "crossfield", "prefix", "curie", "version", "extension",
    "record_id",
];

/// Structural SSSOM/TSV checks that need the raw file text rather than the parsed
/// model: a leading UTF-8 BOM is forbidden, the metadata block must be a
/// contiguous run of `#` lines at the very top (no interleaved blank lines), and
/// no `#` comment may appear after the table begins.
pub fn structure(text: &str) -> Vec<String> {
    let mut e = Vec::new();
    if text.starts_with('\u{feff}') {
        e.push("file: starts with a UTF-8 BOM (forbidden)".into());
    }
    let mut in_header = true;
    let mut saw_hash = false;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.strip_prefix('\u{feff}').unwrap_or(raw);
        let n = i + 1;
        if in_header {
            if line.starts_with('#') {
                saw_hash = true;
            } else if line.trim().is_empty() {
                if saw_hash {
                    e.push(format!("line {n}: blank line inside the metadata block"));
                }
            } else {
                in_header = false; // the column-header row
            }
        } else if line.starts_with('#') {
            e.push(format!("line {n}: stray '#' comment after the metadata block"));
        }
    }
    e
}

/// Run the selected validation categories, returning a list of human-readable
/// violations (empty = conformant).
pub fn run(ms: &MappingSet, categories: &[&str]) -> Vec<String> {
    let mut e = Vec::new();
    for cat in categories {
        match *cat {
            "required" => required(ms, &mut e),
            "enum" => enums(ms, &mut e),
            "datatype" => datatypes(ms, &mut e),
            "crossfield" => crossfield(ms, &mut e),
            "prefix" => prefixes(ms, &mut e),
            "curie" => curies(ms, &mut e),
            "version" => version(ms, &mut e),
            "extension" => extensions(ms, &mut e),
            "record_id" => record_id(ms, &mut e),
            _ => {}
        }
    }
    e
}

/// Run every conformance check.
pub fn validate(ms: &MappingSet) -> Vec<String> {
    run(ms, ALL_CATEGORIES)
}

/// The effective value of a (possibly propagated) slot for a record: the record's
/// own value, else the set-level metadata value.
fn effective<'a>(ms: &'a MappingSet, m: &'a super::Mapping, slot: &str) -> Option<String> {
    if let Some(v) = m.get(slot) {
        return Some(v.clone());
    }
    ms.metadata.get(slot).map(super::value_to_cell)
}

fn is_blank(v: Option<&String>) -> bool {
    v.map(|s| s.is_empty()).unwrap_or(true)
}

fn required(ms: &MappingSet, e: &mut Vec<String>) {
    // MappingSet: mapping_set_id and license are required.
    for slot in ["mapping_set_id", "license"] {
        if !ms.metadata.contains_key(slot) {
            e.push(format!("metadata: missing required slot '{slot}'"));
        }
    }
    for (n, m) in ms.mappings.iter().enumerate() {
        if is_blank(m.get("predicate_id")) {
            e.push(format!("row {n}: missing required slot 'predicate_id'"));
        }
        // mapping_justification may equivalently be carried at the set level.
        if is_blank(m.get("mapping_justification"))
            && !ms.metadata.contains_key("mapping_justification")
        {
            e.push(format!("row {n}: missing required slot 'mapping_justification'"));
        }
        // Literal mappings carry the value in the label and may omit the id.
        for (ty, id, label) in [
            ("subject_type", "subject_id", "subject_label"),
            ("object_type", "object_id", "object_label"),
        ] {
            let is_literal = effective(ms, m, ty).as_deref() == Some(RDFS_LITERAL);
            if is_literal {
                if is_blank(m.get(label)) {
                    e.push(format!("row {n}: literal mapping requires '{label}'"));
                }
            } else if is_blank(m.get(id)) {
                e.push(format!("row {n}: missing required slot '{id}'"));
            }
        }
    }
}

fn enums(ms: &MappingSet, e: &mut Vec<String>) {
    // sssom_version: must be a known value; if present it must be 1.1 (the slot
    // itself is 1.1, so declaring 1.0 is self-contradictory).
    if let Some(v) = ms.metadata.get("sssom_version").map(super::value_to_cell) {
        if v != SSSOM_VERSION_1_0 && v != SSSOM_VERSION_1_1 {
            e.push(format!("metadata: sssom_version '{v}' is not a valid version"));
        } else if v == SSSOM_VERSION_1_0 {
            e.push("metadata: sssom_version '1.0' is self-contradictory (the slot is 1.1-only)".into());
        }
    }
    let check = |e: &mut Vec<String>, loc: &str, slot: &str, val: &str, allowed: &[&str]| {
        if !val.is_empty() && !allowed.contains(&val) {
            e.push(format!("{loc}: '{val}' is not a valid {slot} value"));
        }
    };
    // Metadata-level (propagated) type slots.
    for slot in ["subject_type", "object_type", "predicate_type"] {
        if let Some(v) = ms.metadata.get(slot).map(super::value_to_cell) {
            check(e, "metadata", slot, &v, ENTITY_TYPES);
            if slot == "predicate_type" && (v == RDFS_LITERAL || v == COMPOSED_ENTITY) {
                e.push(format!("metadata: predicate_type must not be '{v}'"));
            }
        }
    }
    for (n, m) in ms.mappings.iter().enumerate() {
        let loc = format!("row {n}");
        for slot in ["subject_type", "object_type", "predicate_type"] {
            if let Some(v) = m.get(slot) {
                check(e, &loc, slot, v, ENTITY_TYPES);
                if slot == "predicate_type" && (v == RDFS_LITERAL || v == COMPOSED_ENTITY) {
                    e.push(format!("{loc}: predicate_type must not be '{v}'"));
                }
            }
        }
        if let Some(v) = m.get("predicate_modifier") {
            check(e, &loc, "predicate_modifier", v, PREDICATE_MODIFIERS);
        }
        if let Some(v) = m.get("mapping_cardinality") {
            check(e, &loc, "mapping_cardinality", v, MAPPING_CARDINALITIES);
        }
        if let Some(v) = m.get("mapping_justification") {
            check(e, &loc, "mapping_justification", v, MAPPING_JUSTIFICATIONS);
        }
    }
    if let Some(v) = ms.metadata.get("mapping_justification").map(super::value_to_cell) {
        check(e, "metadata", "mapping_justification", &v, MAPPING_JUSTIFICATIONS);
    }
}

fn datatypes(ms: &MappingSet, e: &mut Vec<String>) {
    let unit = |e: &mut Vec<String>, loc: &str, slot: &str, v: &str, lo: f64, hi: f64| {
        match v.parse::<f64>() {
            Ok(x) if (lo..=hi).contains(&x) => {}
            Ok(x) => e.push(format!("{loc}: {slot} {x} is outside [{lo}, {hi}]")),
            Err(_) => e.push(format!("{loc}: {slot} '{v}' is not a number")),
        }
    };
    for slot in ["mapping_set_confidence", "registry_confidence"] {
        if let Some(v) = ms.metadata.get(slot).map(super::value_to_cell) {
            unit(e, "metadata", slot, &v, 0.0, 1.0);
        }
    }
    // The required URI slots are `NonRelativeURI` — they must be absolute.
    for slot in ["mapping_set_id", "license"] {
        if let Some(v) = ms.metadata.get(slot).map(super::value_to_cell) {
            if !v.is_empty() && !is_absolute_uri(&v) {
                e.push(format!("metadata: {slot} '{v}' must be an absolute URI"));
            }
        }
    }
    for (n, m) in ms.mappings.iter().enumerate() {
        let loc = format!("row {n}");
        for slot in ["confidence", "similarity_score"] {
            if let Some(v) = m.get(slot) {
                unit(e, &loc, slot, v, 0.0, 1.0);
            }
        }
        if let Some(v) = m.get("reviewer_agreement") {
            unit(e, &loc, "reviewer_agreement", v, -1.0, 1.0);
        }
        for slot in ["mapping_date", "publication_date", "review_date"] {
            if let Some(v) = m.get(slot) {
                if !is_iso_date(v) {
                    e.push(format!("{loc}: {slot} '{v}' is not an ISO-8601 date (YYYY-MM-DD)"));
                }
            }
        }
    }
}

fn crossfield(ms: &MappingSet, e: &mut Vec<String>) {
    for (n, m) in ms.mappings.iter().enumerate() {
        let has_reviewer = !is_blank(m.get("reviewer_id")) || !is_blank(m.get("reviewer_label"));
        for slot in ["review_date", "reviewer_agreement"] {
            if m.contains_key(slot) && !has_reviewer {
                e.push(format!("row {n}: '{slot}' requires 'reviewer_id' or 'reviewer_label'"));
            }
        }
    }
}

fn prefixes(ms: &MappingSet, e: &mut Vec<String>) {
    for p in ms.prefixes_used() {
        if !ms.curie_map.contains_key(&p) && super::io::builtin_namespace(&p).is_none() {
            e.push(format!("prefix '{p}' is used but not declared in curie_map"));
        }
    }
}

fn curies(ms: &MappingSet, e: &mut Vec<String>) {
    for (n, m) in ms.mappings.iter().enumerate() {
        for slot in ["subject_id", "predicate_id", "object_id"] {
            if let Some(v) = m.get(slot) {
                if !v.is_empty() && !is_curie(v) {
                    e.push(format!("row {n}: '{v}' in {slot} is not a valid CURIE"));
                }
            }
        }
    }
}

fn version(ms: &MappingSet, e: &mut Vec<String>) {
    let inferred = infer_version(ms);
    let declared = ms.metadata.get("sssom_version").map(super::value_to_cell);
    if inferred == SSSOM_VERSION_1_1 && declared.as_deref() != Some(SSSOM_VERSION_1_1) {
        e.push(
            "metadata: set uses SSSOM 1.1 features but 'sssom_version' is not declared as '1.1'"
                .into(),
        );
    }
}

fn extensions(ms: &MappingSet, e: &mut Vec<String>) {
    let Some(defs) = ms.metadata.get("extension_definitions") else { return };
    let serde_yaml::Value::Sequence(seq) = defs else {
        e.push("metadata: extension_definitions must be a list".into());
        return;
    };
    let mut slot_names = std::collections::BTreeSet::new();
    let mut properties = std::collections::BTreeSet::new();
    // slot_name → resolved type hint (default xsd:string), for value checking.
    let mut hints: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (i, def) in seq.iter().enumerate() {
        let serde_yaml::Value::Mapping(map) = def else {
            e.push(format!("extension_definitions[{i}]: must be a mapping"));
            continue;
        };
        let get = |k: &str| {
            map.get(serde_yaml::Value::String(k.to_string()))
                .map(super::value_to_cell)
                .filter(|s| !s.is_empty())
        };
        let this_name = get("slot_name");
        match &this_name {
            None => e.push(format!("extension_definitions[{i}]: missing required 'slot_name'")),
            Some(name) => {
                if !is_ncname(name) {
                    e.push(format!("extension_definitions[{i}]: slot_name '{name}' is not an NCName"));
                }
                if SLOT_ORDER.contains(&name.as_str()) {
                    e.push(format!("extension_definitions[{i}]: slot_name '{name}' collides with a standard slot"));
                }
                if !slot_names.insert(name.clone()) {
                    e.push(format!("extension_definitions: duplicate slot_name '{name}'"));
                }
            }
        }
        if let Some(prop) = get("property") {
            if !is_curie(&prop) && !is_absolute_uri(&prop) {
                e.push(format!("extension_definitions[{i}]: property '{prop}' is not a CURIE or URI"));
            }
            if !properties.insert(prop.clone()) {
                e.push(format!("extension_definitions: duplicate property '{prop}'"));
            }
        }
        let hint = match get("type_hint") {
            Some(hint) => {
                if !is_curie(&hint) && !is_absolute_uri(&hint) {
                    e.push(format!("extension_definitions[{i}]: type_hint '{hint}' is not a CURIE or URI"));
                }
                hint
            }
            // Default type hint when omitted.
            None => "xsd:string".to_string(),
        };
        if let Some(name) = this_name {
            hints.insert(name, hint);
        }
    }
    // Validate the actual extension-slot values against their type hint (the spec
    // permits this; we report a clearly-wrong value). Checks records and the set.
    let check = |e: &mut Vec<String>, loc: &str, slot: &str, hint: &str, val: &str| {
        if !val.is_empty() && !type_matches(hint, val) {
            e.push(format!("{loc}: extension '{slot}' value '{val}' is not a valid {hint}"));
        }
    };
    for (slot, hint) in &hints {
        if let Some(v) = ms.metadata.get(slot).map(super::value_to_cell) {
            check(e, "metadata", slot, hint, &v);
        }
        for (n, m) in ms.mappings.iter().enumerate() {
            if let Some(v) = m.get(slot) {
                check(e, &format!("row {n}"), slot, hint, v);
            }
        }
    }
}

/// Whether `val` conforms to the extension `type_hint`. Unknown hints (and
/// `xsd:string`) accept anything.
fn type_matches(hint: &str, val: &str) -> bool {
    match hint {
        "xsd:integer" | "xsd:int" | "xsd:long" => val.parse::<i64>().is_ok(),
        "xsd:double" | "xsd:float" | "xsd:decimal" => val.parse::<f64>().is_ok(),
        "xsd:boolean" => matches!(val, "true" | "false"),
        "xsd:date" => is_iso_date(val) && !val.contains('T'),
        "xsd:dateTime" => is_iso_date(val),
        "linkml:Uriorcurie" | "xsd:anyURI" | "linkml:Uri" => is_curie(val) || is_absolute_uri(val),
        _ => true,
    }
}

fn record_id(ms: &MappingSet, e: &mut Vec<String>) {
    let with = ms.mappings.iter().filter(|m| !is_blank(m.get("record_id"))).count();
    if with == 0 {
        return;
    }
    if with != ms.mappings.len() {
        e.push(format!(
            "record_id: present on {with}/{} records (must be all-or-none)",
            ms.mappings.len()
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (n, m) in ms.mappings.iter().enumerate() {
        if let Some(id) = m.get("record_id").filter(|s| !s.is_empty()) {
            if !seen.insert(id.clone()) {
                e.push(format!("row {n}: duplicate record_id '{id}'"));
            }
        }
    }
}

// ── small validators ─────────────────────────────────────────────────────────

fn is_curie(s: &str) -> bool {
    match s.split_once(':') {
        Some((p, l)) => !p.is_empty() && !l.is_empty() && !p.contains(' ') && !s.starts_with("http"),
        None => false,
    }
}

fn is_absolute_uri(s: &str) -> bool {
    // A scheme component followed by `:` — the SSSOM `NonRelativeURI` requirement.
    match s.split_once(':') {
        Some((scheme, _)) => {
            !scheme.is_empty()
                && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

fn is_ncname(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn is_iso_date(s: &str) -> bool {
    // YYYY-MM-DD, optionally with a time component (datetime).
    let date = s.split(['T', ' ']).next().unwrap_or(s);
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    let ok_len = [4, 2, 2];
    parts.iter().zip(ok_len).all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> MappingSet {
        let mut ms = MappingSet::new();
        ms.metadata.insert("mapping_set_id".into(), serde_yaml::Value::String("https://ex.org/s".into()));
        ms.metadata.insert("license".into(), serde_yaml::Value::String("https://ex.org/l".into()));
        ms.mappings.push(
            [
                ("subject_id", "HP:1"),
                ("predicate_id", "skos:exactMatch"),
                ("object_id", "MP:1"),
                ("mapping_justification", "semapv:LexicalMatching"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        ms.curie_map.insert("HP".into(), "http://x/HP_".into());
        ms.curie_map.insert("MP".into(), "http://x/MP_".into());
        ms
    }

    #[test]
    fn clean_set_is_conformant() {
        assert!(validate(&clean()).is_empty());
    }

    #[test]
    fn flags_bad_enum_and_range() {
        let mut ms = clean();
        ms.mappings[0].insert("mapping_justification".into(), "semapv:Nope".into());
        ms.mappings[0].insert("confidence".into(), "2.0".into());
        ms.mappings[0].insert("predicate_modifier".into(), "Maybe".into());
        let errs = validate(&ms);
        assert!(errs.iter().any(|e| e.contains("mapping_justification")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("confidence")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("predicate_modifier")), "{errs:?}");
    }

    #[test]
    fn flags_missing_required_and_relative_uri() {
        let mut ms = clean();
        ms.metadata.insert("mapping_set_id".into(), serde_yaml::Value::String("not-a-uri".into()));
        ms.metadata.remove("license");
        let errs = validate(&ms);
        assert!(errs.iter().any(|e| e.contains("license")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("absolute URI")), "{errs:?}");
    }

    #[test]
    fn requires_1_1_declaration_for_1_1_features() {
        let mut ms = clean();
        ms.mappings[0].insert("predicate_type".into(), "owl object property".into());
        // Uses predicate_type (1.1) without declaring the version.
        assert!(validate(&ms).iter().any(|e| e.contains("1.1")));
        ms.enforce_version();
        assert!(!validate(&ms).iter().any(|e| e.contains("1.1 features")));
    }
}
