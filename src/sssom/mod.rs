//! `owlmake sssom` — the SSSOM ("Simple Standard for Sharing Ontological
//! Mappings") mapping-set toolkit. Curator and build scripts invoke it as
//! `sssom <subcommand> …`, so the subcommand and flag surface is part of the
//! contract owlmake owes them.
//!
//! This module holds the data model (a [`MappingSet`]: a CURIE map, mapping-set
//! metadata, and a table of mapping records) plus the spec operations the CLI is
//! built on (propagation/condensation of propagatable slots, canonical sorting,
//! prefix/CURIE handling, multivalued `\|` escaping, `mapping_cardinality`
//! inference and `sssom_version` inference). SSSOM 1.1 conformance validation
//! lives in [`conformance`], I/O in [`io`], and the command line in [`cli`].
//!
//! Slot names, slot ordering, the propagatable/multivalued/entity-reference sets,
//! the built-in prefixes and the inverse-predicate map all come from the published
//! SSSOM LinkML schema, so a set written here is one any SSSOM consumer can read
//! and a set read here keeps every slot the schema defines.

use std::collections::BTreeMap;

pub mod cli;
pub mod conformance;
pub mod converter;
pub mod dosql;
pub mod io;
pub mod owl;
pub mod sssom_cli;
pub mod transform;

pub use cli::main;

/// The `sssom:` namespace and assorted spec-level constants.
pub const SSSOM_URI_PREFIX: &str = "https://w3id.org/sssom/";
pub const DEFAULT_LICENSE: &str = "https://w3id.org/sssom/license/unspecified";
pub const MAPPING_JUSTIFICATION_UNSPECIFIED: &str = "semapv:UnspecifiedMatching";

/// SSSOM "built-in" prefixes — usable in CURIEs without appearing in `curie_map`
/// (SSSOM spec §"IRI prefixes": owl, rdf, rdfs, semapv, skos, sssom, xsd, linkml).
pub const BUILTIN_PREFIXES: &[(&str, &str)] = &[
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("sssom", SSSOM_URI_PREFIX),
    ("semapv", "https://w3id.org/semapv/vocab/"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("linkml", "https://w3id.org/linkml/"),
];

/// Canonical declaration order of every slot in the SSSOM schema. Column ordering
/// for `sort` and for newly added columns follows this sequence (extras append).
pub const SLOT_ORDER: &[&str] = &[
    "prefix_name", "prefix_url", "sssom_version", "curie_map", "mirror_from",
    "registry_confidence", "last_updated", "local_name", "mapping_set_references",
    "mapping_registry_id", "mapping_registry_title", "mapping_registry_description",
    "imports", "documentation", "homepage", "mappings", "subject_id", "subject_label",
    "subject_category", "subject_type", "predicate_id", "predicate_modifier",
    "predicate_label", "predicate_type", "object_id", "object_label", "object_category",
    "mapping_justification", "object_type", "mapping_set_id", "mapping_set_version",
    "mapping_set_group", "mapping_set_title", "mapping_set_description",
    "mapping_set_confidence", "creator_id", "creator_label", "author_id", "author_label",
    "reviewer_id", "reviewer_label", "license", "subject_source", "subject_source_version",
    "object_source", "object_source_version", "mapping_provider", "mapping_set_source",
    "mapping_source", "mapping_cardinality", "cardinality_scope", "mapping_tool",
    "mapping_tool_id", "mapping_tool_version", "mapping_date", "publication_date",
    "review_date", "confidence", "reviewer_agreement", "subject_match_field",
    "object_match_field", "match_string", "subject_preprocessing", "object_preprocessing",
    "curation_rule", "curation_rule_text", "similarity_score", "similarity_measure",
    "issue_tracker_item", "issue_tracker", "derived_from", "see_also", "other", "comment",
    "extension_definitions", "record_id",
];

/// The mapping-level schema: every slot a mapping record may carry as a TSV
/// column, in the order the columns are written.
///
/// This is deliberately NOT [`SLOT_ORDER`]. That sequence declares the schema as
/// a whole, header slots included, and its order governs the YAML header; the
/// columns of the table are their own sequence, and the two disagree in four
/// places (`predicate_label` precedes `predicate_modifier` here; author, then
/// reviewer, then creator; the curation-rule pair precedes `match_string`;
/// `see_also` precedes `issue_tracker_item`).
///
/// A slot absent from this list is not written as a column even when a record
/// holds a value for it. Seven are known to the schema yet have no mapping-level
/// column — `cardinality_scope`, `derived_from`, `mapping_tool_id`,
/// `predicate_type`, `record_id`, `review_date` and `reviewer_agreement`. Some
/// are set-level only: `mapping_tool_id` still reaches the header when
/// `condense` lifts it, which is why the value is dropped from the COLUMN set
/// and never from the record.
///
/// `mapping_cardinality` is NOT among them, though `sssom-cli` never writes one.
/// That is the COMMAND's behaviour and not the slot's: `sssom:xref-extract
/// --drop-duplicates` writes the column, and feeding its output straight back
/// through `sssom-cli` takes the column away again. So the slot has a column
/// here, and `sssom-cli` clears it from the records it is about to write,
/// because a cardinality it did not derive describes a set that no longer
/// exists.
///
/// Columns the schema does not describe at all are extensions, and those are
/// kept, appended after these in the order the extension rules give.
pub const MAPPING_COLUMN_ORDER: &[&str] = &[
    "subject_id", "subject_label", "subject_category", "predicate_id", "predicate_label",
    "predicate_modifier", "object_id", "object_label", "object_category",
    "mapping_justification", "author_id", "author_label", "reviewer_id", "reviewer_label",
    "creator_id", "creator_label", "license", "subject_type", "subject_source",
    "subject_source_version", "object_type", "object_source", "object_source_version",
    "mapping_provider", "mapping_source", "mapping_cardinality", "mapping_tool",
    "mapping_tool_version",
    "mapping_date", "publication_date", "confidence", "curation_rule", "curation_rule_text",
    "subject_match_field", "object_match_field", "match_string", "subject_preprocessing",
    "object_preprocessing", "similarity_score", "similarity_measure", "see_also",
    "issue_tracker_item", "other", "comment",
];

/// Slots whose set-level value propagates to individual records (and back, on
/// condensation). Verbatim from the schema's `propagated: true` annotations.
pub const PROPAGATABLE_SLOTS: &[&str] = &[
    "subject_type", "predicate_type", "object_type", "subject_source",
    "subject_source_version", "object_source", "object_source_version", "mapping_provider",
    "cardinality_scope", "mapping_tool", "mapping_tool_id", "mapping_tool_version",
    "mapping_date", "subject_match_field", "object_match_field", "subject_preprocessing",
    "object_preprocessing", "curation_rule", "curation_rule_text", "similarity_measure",
];

/// Multivalued slots — cell values are `|`-separated lists.
pub const MULTIVALUED_SLOTS: &[&str] = &[
    "curie_map", "mapping_set_references", "imports", "mappings", "creator_id",
    "creator_label", "author_id", "author_label", "reviewer_id", "reviewer_label",
    "mapping_set_source", "cardinality_scope", "subject_match_field", "object_match_field",
    "match_string", "subject_preprocessing", "object_preprocessing", "curation_rule",
    "curation_rule_text", "derived_from", "see_also", "extension_definitions",
];

/// Slots whose values are entity references (CURIEs) rather than literals.
pub const ENTITY_REFERENCE_SLOTS: &[&str] = &[
    "mapping_registry_id", "subject_id", "predicate_id", "object_id",
    "mapping_justification", "creator_id", "author_id", "reviewer_id", "subject_source",
    "object_source", "mapping_source", "mapping_tool_id", "subject_match_field",
    "object_match_field", "subject_preprocessing", "object_preprocessing", "curation_rule",
    "issue_tracker_item", "derived_from", "record_id",
];

/// Slots whose values are URIs — the schema's `uri`/`NonRelativeURI` ranges — so
/// RDF writes them as resources rather than as string literals, exactly as it
/// does an entity reference.
pub const URI_SLOTS: &[&str] = &[
    "prefix_url", "mirror_from", "imports", "documentation", "homepage", "mapping_set_id",
    "license", "mapping_provider", "mapping_set_source", "issue_tracker", "see_also",
];

/// The slots RDF names by something other than `sssom:<slot>`, as the SSSOM
/// schema's `slot_uri` gives them. Every other slot takes the default.
pub const SLOT_URI: &[(&str, &str)] = &[
    ("subject_id", "owl:annotatedSource"),
    ("predicate_id", "owl:annotatedProperty"),
    ("object_id", "owl:annotatedTarget"),
    ("mapping_set_version", "owl:versionInfo"),
    ("mapping_set_title", "dcterms:title"),
    ("mapping_set_description", "dcterms:description"),
    ("creator_id", "dcterms:creator"),
    ("author_id", "pav:authoredBy"),
    ("license", "dcterms:license"),
    ("mapping_set_source", "prov:wasDerivedFrom"),
    ("mapping_date", "dcterms:created"),
    ("publication_date", "dcterms:issued"),
    ("see_also", "rdfs:seeAlso"),
    ("comment", "rdfs:comment"),
];

/// The RDF predicate for a slot: its schema `slot_uri` where it has one, else
/// `sssom:<slot>`.
pub fn slot_predicate(slot: &str) -> String {
    match SLOT_URI.iter().find(|(s, _)| *s == slot) {
        Some((_, uri)) => (*uri).to_string(),
        None => format!("sssom:{slot}"),
    }
}

/// Whether a slot's RDF object is a resource rather than a literal.
pub fn is_uri_valued(slot: &str) -> bool {
    is_entity_reference(slot) || URI_SLOTS.contains(&slot)
}

/// The slots whose range is a number. A missing cell of one of these is no value
/// rather than the empty string, which is what puts it last in a row sort.
pub const NUMERIC_SLOTS: &[&str] =
    &["confidence", "similarity_score", "mapping_set_confidence", "registry_confidence"];

/// Identity/key columns used by `remove`, `dedupe`, merge de-duplication.
pub const KEY_FEATURES: &[&str] = &["subject_id", "predicate_id", "object_id", "predicate_modifier"];

/// Inverse-predicate map: how `invert` rewrites the predicate when subject and
/// object are swapped.
pub const INVERSE_PREDICATE_MAP: &[(&str, &str)] = &[
    ("skos:closeMatch", "skos:closeMatch"),
    ("skos:relatedMatch", "skos:relatedMatch"),
    ("skos:exactMatch", "skos:exactMatch"),
    ("skos:narrowMatch", "skos:broadMatch"),
    ("skos:broadMatch", "skos:narrowMatch"),
    ("semapv:crossSpeciesExactMatch", "semapv:crossSpeciesExactMatch"),
    // NOT `semapv:crossSpeciesCloseMatch`: it has no declared inverse, and a
    // mapping that cannot be inverted is DROPPED rather than passed through
    // unchanged. Treating it as self-inverse kept two rows in UBERON's
    // `uberon.sssom.tsv` that the reference does not have — the only two
    // close-matches FBbt asserts, against 354 exact-matches that all survive.
    ("semapv:crossSpeciesNarrowMatch", "semapv:crossSpeciesBroadMatch"),
    ("semapv:crossSpeciesBroadMatch", "semapv:crossSpeciesNarrowMatch"),
    ("owl:equivalentClass", "owl:equivalentClass"),
    ("owl:sameAs", "owl:sameAs"),
    ("rdfs:subClassOf", "sssom:superClassOf"),
    ("sssom:superClassOf", "rdfs:subClassOf"),
];

/// subject↔object column swap used by `invert`.
pub fn invert_column_name(col: &str) -> Option<&'static str> {
    Some(match col {
        "subject_id" => "object_id",
        "subject_label" => "object_label",
        "subject_category" => "object_category",
        "subject_match_field" => "object_match_field",
        "subject_source" => "object_source",
        "subject_preprocessing" => "object_preprocessing",
        "subject_source_version" => "object_source_version",
        "subject_type" => "object_type",
        "object_id" => "subject_id",
        "object_label" => "subject_label",
        "object_category" => "subject_category",
        "object_match_field" => "subject_match_field",
        "object_source" => "subject_source",
        "object_preprocessing" => "subject_preprocessing",
        "object_source_version" => "subject_source_version",
        "object_type" => "subject_type",
        _ => return None,
    })
}

pub fn is_propagatable(slot: &str) -> bool {
    PROPAGATABLE_SLOTS.contains(&slot)
}
pub fn is_multivalued(slot: &str) -> bool {
    MULTIVALUED_SLOTS.contains(&slot)
}
pub fn is_entity_reference(slot: &str) -> bool {
    ENTITY_REFERENCE_SLOTS.contains(&slot)
}

/// A single mapping record: slot name → serialized cell value (multivalued cells
/// are stored as their raw `|`-joined string, matching the on-disk form).
pub type Mapping = BTreeMap<String, String>;

/// An in-memory SSSOM mapping set: prefix map, set-level metadata, and records.
#[derive(Clone, Default)]
pub struct MappingSet {
    /// `curie_map`: prefix → namespace IRI.
    pub curie_map: BTreeMap<String, String>,
    /// Mapping-set-level metadata slots (everything except `curie_map`/`mappings`).
    pub metadata: BTreeMap<String, serde_yaml::Value>,
    /// Column order as it should be serialized.
    pub columns: Vec<String>,
    /// The mapping records.
    pub mappings: Vec<Mapping>,
}

impl MappingSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Recompute [`Self::columns`] as the union of keys present across records,
    /// preserving any existing order and appending new columns in schema order.
    pub fn recompute_columns(&mut self) {
        // A column counts as present only when some mapping gives it a NON-EMPTY
        // value. Keying on mere presence kept every column the input declared,
        // even once filtering had emptied it: UBERON's biomappings artefact came
        // out with a `reviewer_agreement` column that no surviving mapping fills,
        // so every row carried a trailing tab the reference output does not have.
        let mut present: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for m in &self.mappings {
            for (k, v) in m {
                if !v.is_empty() {
                    present.insert(k.as_str());
                }
            }
        }
        // The column sequence is the schema's, not the order the columns happened
        // to arrive in: a set read with `object_label` last, or gaining one from a
        // later step, still writes it in its schema position. Slots the
        // mapping-level schema has no column for are not written; extensions,
        // which the schema does not describe at all, follow alphabetically.
        let mut cols: Vec<String> = MAPPING_COLUMN_ORDER
            .iter()
            .filter(|s| present.contains(**s))
            .map(|s| (*s).to_string())
            .collect();
        let mut extras: Vec<String> =
            present.iter().filter(|c| !SLOT_ORDER.contains(c)).map(|c| (*c).to_string()).collect();
        extras.sort();
        cols.extend(extras);
        self.columns = cols;
    }

    /// Order columns canonically: mapping-level schema order first, then any
    /// extension columns alphabetically.
    pub fn sort_columns(&mut self) {
        let present: std::collections::BTreeSet<String> = self.columns.iter().cloned().collect();
        let mut cols: Vec<String> = MAPPING_COLUMN_ORDER
            .iter()
            .filter(|s| present.contains(**s))
            .map(|s| s.to_string())
            .collect();
        let mut extras: Vec<String> =
            present.iter().filter(|c| !SLOT_ORDER.contains(&c.as_str())).cloned().collect();
        extras.sort();
        cols.extend(extras);
        self.columns = cols;
    }

    /// Sort records ascending by every column in current column order, so a
    /// re-serialized set is byte-stable regardless of the order it was read in.
    pub fn sort_rows(&mut self) {
        let cols = self.columns.clone();
        self.mappings.sort_by(|a, b| {
            for c in &cols {
                // An empty cell of a text column is the empty string, which sorts
                // before every value — a record with no subject label comes first
                // among the records that share its subject. Only a NUMERIC column
                // keeps a missing cell as no value at all, and that sorts last.
                let ord = if NUMERIC_SLOTS.contains(&c.as_str()) {
                    match (a.get(c), b.get(c)) {
                        (Some(x), Some(y)) => x.cmp(y),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    }
                } else {
                    let av = a.get(c).map(String::as_str).unwrap_or("");
                    let bv = b.get(c).map(String::as_str).unwrap_or("");
                    av.cmp(bv)
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
    }

    /// Push propagatable set-metadata slots down into every record, only when no
    /// record already carries that slot (spec-compliant `propagate()`).
    pub fn propagate(&mut self) {
        let mut propagated = Vec::new();
        for &slot in PROPAGATABLE_SLOTS {
            let Some(val) = self.metadata.get(slot) else { continue };
            // Only propagate if no record already has the slot.
            if self.mappings.iter().any(|m| m.contains_key(slot)) {
                continue;
            }
            let cell = value_to_cell(val);
            for m in &mut self.mappings {
                m.insert(slot.to_string(), cell.clone());
            }
            propagated.push(slot.to_string());
        }
        if !propagated.is_empty() {
            self.recompute_columns();
        }
    }

    /// Lift propagatable columns whose value is identical across all records up to
    /// the set metadata, dropping the column (spec-compliant `condense()`).
    pub fn condense(&mut self) {
        if self.mappings.is_empty() {
            return;
        }
        let mut condensed = Vec::new();
        for &slot in PROPAGATABLE_SLOTS {
            if !self.columns.iter().any(|c| c == slot) {
                continue;
            }
            // All records must share one identical value.
            let mut iter = self.mappings.iter().map(|m| m.get(slot).cloned().unwrap_or_default());
            let first = iter.next().unwrap_or_default();
            if !iter.all(|v| v == first) {
                continue;
            }
            let new_val = cell_to_value(slot, &first);
            if let Some(existing) = self.metadata.get(slot) {
                if *existing != new_val {
                    continue; // conflict — leave as a column
                }
            } else {
                self.metadata.insert(slot.to_string(), new_val);
            }
            condensed.push(slot.to_string());
        }
        for slot in &condensed {
            for m in &mut self.mappings {
                m.remove(slot);
            }
        }
        if !condensed.is_empty() {
            self.recompute_columns();
        }
    }

    /// Infer and set the `mapping_cardinality` slot on every record (SSSOM 1.1).
    ///
    /// For each mapping `(s, p, o)`, the subject side is `1` when `s` maps to a
    /// single distinct object across the set and `n` otherwise; the object side is
    /// `1` when `o` is mapped from a single distinct subject and `n` otherwise,
    /// giving `1:1`/`1:n`/`n:1`/`n:n`. A `sssom:NoTermFound` object yields `1:0`, a
    /// `NoTermFound` subject `0:1`, both `0:0`; such records are excluded from the
    /// distinct counts of the others. The subject/object of a literal mapping is
    /// keyed by its label. Matches the SSSOM spec's cardinality algorithm.
    pub fn set_mapping_cardinality(&mut self) {
        use std::collections::BTreeSet;
        // Key a side by its label for literal mappings, else by its id.
        let side_key = |m: &Mapping, id: &str, label: &str, ty: &str| -> Option<String> {
            match m.get(ty).map(String::as_str) {
                Some(conformance::RDFS_LITERAL) => m.get(label).cloned(),
                _ => m.get(id).cloned(),
            }
        };
        let no_term = |v: Option<&String>| -> bool {
            v.map(|s| s == conformance::NO_TERM_FOUND || s.ends_with("#NoTermFound")).unwrap_or(false)
        };
        // `cardinality_scope` (1.1, propagatable, multivalued) lists slots whose
        // equal values define a scope; counts are computed within each scope.
        // Absent ⇒ a single scope spanning the whole set.
        let scope_slots: Vec<String> = self
            .metadata
            .get("cardinality_scope")
            .map(|v| flatten_value(v))
            .unwrap_or_default();
        let scope_key = |m: &Mapping| -> String {
            scope_slots
                .iter()
                .map(|s| m.get(s).cloned().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\u{1f}")
        };
        // Side counts are keyed by (scope, side value).
        let mut subj_objs: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
        let mut obj_subjs: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
        for m in &self.mappings {
            // NoTermFound records don't participate in others' counts.
            if no_term(m.get("subject_id")) || no_term(m.get("object_id")) {
                continue;
            }
            let (Some(s), Some(o)) = (
                side_key(m, "subject_id", "subject_label", "subject_type"),
                side_key(m, "object_id", "object_label", "object_type"),
            ) else {
                continue;
            };
            let scope = scope_key(m);
            subj_objs.entry((scope.clone(), s.clone())).or_default().insert(o.clone());
            obj_subjs.entry((scope, o)).or_default().insert(s);
        }
        let card: Vec<Option<String>> = self
            .mappings
            .iter()
            .map(|m| {
                let subj_missing = no_term(m.get("subject_id"));
                let obj_missing = no_term(m.get("object_id"));
                if subj_missing || obj_missing {
                    let ss = if subj_missing { "0" } else { "1" };
                    let os = if obj_missing { "0" } else { "1" };
                    return Some(format!("{ss}:{os}"));
                }
                let s = side_key(m, "subject_id", "subject_label", "subject_type")?;
                let o = side_key(m, "object_id", "object_label", "object_type")?;
                let scope = scope_key(m);
                // `mapping_cardinality` reads SUBJECT-SIDE:OBJECT-SIDE, where the
                // subject side counts how many SUBJECTS share this object and the
                // object side how many OBJECTS this subject has. So a set where 25
                // species-specific stages all map to one UBERON stage is `n:1`.
                //
                // Emitting the two the other way round inverted every non-1:1
                // mapping. UBERON's `!cardinality==*:1 -> stop()` — whose comment
                // is "ignore any mapping where the same foreign term is mapped to
                // more than one UBERON class", i.e. exclude a subject with several
                // objects — then dropped 266 of `sslso.sssom.tsv`'s 269 mappings
                // that ROBOT keeps.
                let objs_per_subj =
                    if subj_objs.get(&(scope.clone(), s)).map(|x| x.len()).unwrap_or(0) > 1 { "n" } else { "1" };
                let subjs_per_obj =
                    if obj_subjs.get(&(scope, o)).map(|x| x.len()).unwrap_or(0) > 1 { "n" } else { "1" };
                Some(format!("{subjs_per_obj}:{objs_per_subj}"))
            })
            .collect();
        for (m, c) in self.mappings.iter_mut().zip(card) {
            if let Some(c) = c {
                m.insert("mapping_cardinality".to_string(), c);
            }
        }
        self.recompute_columns();
    }

    /// Rewrite the set into **canonical SSSOM/TSV** form (spec §"Canonical
    /// SSSOM/TSV"): lift constant propagatable slots, round float slots to ≤3
    /// decimals, drop unused and built-in prefixes
    /// from the `curie_map`, order columns by schema (extensions last, by their
    /// declared `property`), and sort the records lexicographically.
    pub fn canonicalize(&mut self) {
        self.condense();
        self.round_floats(3);
        self.prune_curie_map();
        self.sort_columns_canonical();
        self.sort_rows();
    }

    /// Round the float-valued slots (`confidence`/`similarity_score`, and the
    /// set-level `*_confidence`) to `decimals` places, trimming trailing zeros.
    pub fn round_floats(&mut self, decimals: usize) {
        let round = |v: &str| -> Option<String> {
            let x: f64 = v.parse().ok()?;
            let s = format!("{x:.decimals$}");
            Some(if s.contains('.') {
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                s
            })
        };
        for m in &mut self.mappings {
            for slot in ["confidence", "similarity_score"] {
                if let Some(v) = m.get(slot).and_then(|v| round(v)) {
                    m.insert(slot.to_string(), v);
                }
            }
        }
        for slot in ["mapping_set_confidence", "registry_confidence"] {
            if let Some(v) = self.metadata.get(slot).map(value_to_cell).and_then(|v| round(&v)) {
                self.metadata.insert(slot.to_string(), serde_yaml::Value::String(v));
            }
        }
    }

    /// Drop prefixes from the `curie_map` that are unused or built-in (built-ins
    /// are implicit and MUST NOT be redeclared in canonical form).
    ///
    /// "Used" means used by the table that gets written. A CURIE sitting in a
    /// record slot with no column — a `mapping_tool_id`, say — is not in the
    /// output, so declaring its prefix leaves the reader a binding nothing
    /// resolves against.
    pub fn prune_curie_map(&mut self) {
        let used = self.prefixes_used_written();
        self.curie_map
            .retain(|p, _| used.contains(p) && BUILTIN_PREFIXES.iter().all(|(b, _)| b != p));
    }

    /// Order columns canonically: mapping-level schema order first, then
    /// extension columns sorted by their declared `property` (falling back to the
    /// column name when undeclared).
    pub fn sort_columns_canonical(&mut self) {
        let prop = self.extension_properties();
        let present: std::collections::BTreeSet<String> = self.columns.iter().cloned().collect();
        let mut cols: Vec<String> = MAPPING_COLUMN_ORDER
            .iter()
            .filter(|s| present.contains(**s))
            .map(|s| s.to_string())
            .collect();
        let mut extras: Vec<String> =
            present.iter().filter(|c| !SLOT_ORDER.contains(&c.as_str())).cloned().collect();
        extras.sort_by(|a, b| {
            let ka = prop.get(a).unwrap_or(a);
            let kb = prop.get(b).unwrap_or(b);
            ka.cmp(kb).then_with(|| a.cmp(b))
        });
        cols.extend(extras);
        self.columns = cols;
    }

    /// Map extension `slot_name` → `property` from `extension_definitions`.
    fn extension_properties(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        if let Some(serde_yaml::Value::Sequence(seq)) = self.metadata.get("extension_definitions") {
            for def in seq {
                if let serde_yaml::Value::Mapping(map) = def {
                    let get = |k: &str| {
                        map.get(serde_yaml::Value::String(k.to_string())).map(value_to_cell)
                    };
                    if let Some(name) = get("slot_name") {
                        if let Some(p) = get("property") {
                            out.insert(name, p);
                        }
                    }
                }
            }
        }
        out
    }

    /// Infer the SSSOM version this set requires from its content: `"1.1"` if it
    /// uses any post-1.0 feature (a 1.1-only slot, the `composed entity
    /// expression` entity type, a `0:0` cardinality, or a MappingSet-level
    /// `similarity_measure`/`curation_rule`/`curation_rule_text`), else `"1.0"`.
    pub fn infer_version(&self) -> &'static str {
        conformance::infer_version(self)
    }

    /// Drop everything the set's DECLARED version does not define.
    ///
    /// A set is read at the version it says it is, and one that declares no
    /// `sssom_version` is a 1.0 set. So a 1.0 set carrying a
    /// `mapping_set_confidence` — a slot 1.1 introduced — does not have one: the
    /// header line is not a value the set holds, it is a line a 1.0 reader has no
    /// slot for. Reading it anyway would let a set claim a feature its own
    /// version never defined, and would then declare 1.1 on the way out, turning
    /// an undeclared input into a 1.1 output.
    pub fn restrict_to_declared_version(&mut self) {
        let declared = self
            .metadata
            .get("sssom_version")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| conformance::SSSOM_VERSION_1_0.to_string());
        if declared.as_str() >= conformance::SSSOM_VERSION_1_1 {
            return;
        }
        self.metadata.retain(|k, _| !conformance::is_slot_added_1_1(k));
        for m in &mut self.mappings {
            m.retain(|k, _| !conformance::is_record_slot_added_1_1(k));
        }
        self.recompute_columns();
    }

    /// Declare `sssom_version` when the set AS WRITTEN uses 1.1 features.
    ///
    /// The question the writer asks is not the one [`Self::infer_version`]
    /// answers. That reports what the data uses, which is what validation needs;
    /// this reports what the serialized table uses. The two part company because
    /// no 1.1 slot has a mapping-level column: a record may carry a
    /// `reviewer_agreement`, but nothing in the written table does, so declaring
    /// the table 1.1 on its account describes bytes that were never emitted.
    pub fn enforce_version(&mut self) {
        if conformance::written_version(self) == conformance::SSSOM_VERSION_1_1 {
            self.metadata.insert(
                "sssom_version".to_string(),
                serde_yaml::Value::String(conformance::SSSOM_VERSION_1_1.to_string()),
            );
        }
    }

    /// The effective prefix map: built-ins overlaid by the declared `curie_map`.
    pub fn effective_prefixes(&self) -> BTreeMap<String, String> {
        let mut m: BTreeMap<String, String> =
            BUILTIN_PREFIXES.iter().map(|(p, n)| (p.to_string(), n.to_string())).collect();
        for (p, n) in &self.curie_map {
            m.insert(p.clone(), n.clone());
        }
        m
    }

    /// Expand a CURIE to a full IRI using the effective prefix map (passes through
    /// values that are already IRIs or have no known prefix).
    pub fn expand(&self, curie: &str) -> String {
        if curie.starts_with("http://") || curie.starts_with("https://") {
            return curie.to_string();
        }
        let Some((p, l)) = curie.split_once(':') else { return curie.to_string() };
        match self.effective_prefixes().get(p) {
            Some(ns) => format!("{ns}{l}"),
            None => curie.to_string(),
        }
    }

    /// Compress a full IRI to a CURIE using the effective prefix map (longest
    /// namespace wins). Returns the input unchanged if it is not an IRI or no
    /// declared prefix matches.
    pub fn compress(&self, iri: &str) -> String {
        if !(iri.starts_with("http://") || iri.starts_with("https://")) {
            return iri.to_string();
        }
        let pm = self.effective_prefixes();
        let mut best: Option<(String, String)> = None;
        for (p, ns) in &pm {
            if iri.starts_with(ns.as_str())
                && best.as_ref().map(|(_, bns)| ns.len() > bns.len()).unwrap_or(true)
            {
                best = Some((p.clone(), ns.clone()));
            }
        }
        match best {
            Some((p, ns)) => format!("{p}:{}", &iri[ns.len()..]),
            None => iri.to_string(),
        }
    }

    /// Set of prefixes referenced by entity-reference cells and metadata.
    pub fn prefixes_used(&self) -> std::collections::BTreeSet<String> {
        let mut used = std::collections::BTreeSet::new();
        for m in &self.mappings {
            for (k, v) in m {
                if is_entity_reference(k) {
                    for part in split_multivalued(v) {
                        if let Some((p, _)) = part.split_once(':') {
                            if !part.starts_with("http") {
                                used.insert(p.to_string());
                            }
                        }
                    }
                }
            }
        }
        for (k, v) in &self.metadata {
            if is_entity_reference(k) {
                for s in flatten_value(v) {
                    if let Some((p, _)) = s.split_once(':') {
                        if !s.starts_with("http") {
                            used.insert(p.to_string());
                        }
                    }
                }
            }
        }
        used
    }

    /// The prefixes the WRITTEN table uses: as [`Self::prefixes_used`], but a
    /// record's value counts only when its slot has a column to appear in.
    pub fn prefixes_used_written(&self) -> std::collections::BTreeSet<String> {
        let writable = |k: &str| MAPPING_COLUMN_ORDER.contains(&k) || !SLOT_ORDER.contains(&k);
        let mut used = std::collections::BTreeSet::new();
        for m in &self.mappings {
            for (k, v) in m {
                if is_entity_reference(k) && writable(k.as_str()) {
                    for part in split_multivalued(v) {
                        if let Some((p, _)) = part.split_once(':') {
                            if !part.starts_with("http") {
                                used.insert(p.to_string());
                            }
                        }
                    }
                }
            }
        }
        for (k, v) in &self.metadata {
            if is_entity_reference(k) {
                for s in flatten_value(v) {
                    if let Some((p, _)) = s.split_once(':') {
                        if !s.starts_with("http") {
                            used.insert(p.to_string());
                        }
                    }
                }
            }
        }
        used
    }
}

/// Split a multivalued SSSOM/TSV cell into its element values, honouring the
/// SSSOM 1.1 escaping rules: `|` separates elements, `\|` is a literal pipe and
/// `\\` a literal backslash. An empty cell yields no elements.
pub fn split_multivalued(cell: &str) -> Vec<String> {
    if cell.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cell.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('|') => cur.push('|'),
                Some('\\') => cur.push('\\'),
                // An unrecognised escape keeps the backslash verbatim.
                Some(other) => {
                    cur.push('\\');
                    cur.push(other);
                }
                None => cur.push('\\'),
            },
            '|' => {
                out.push(std::mem::take(&mut cur));
            }
            other => cur.push(other),
        }
    }
    out.push(cur);
    out
}

/// Join element values into a multivalued SSSOM/TSV cell, escaping `\` as `\\`
/// and `|` as `\|` so the result round-trips through [`split_multivalued`].
pub fn join_multivalued<S: AsRef<str>>(values: &[S]) -> String {
    values
        .iter()
        .map(|v| v.as_ref().replace('\\', "\\\\").replace('|', "\\|"))
        .collect::<Vec<_>>()
        .join("|")
}

/// Convert a metadata YAML value into the on-disk cell string (lists escaped and
/// `|`-joined per the SSSOM 1.1 multivalued rules).
pub fn value_to_cell(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::Sequence(seq) => {
            let parts: Vec<String> = seq.iter().map(scalar_to_string).collect();
            join_multivalued(&parts)
        }
        other => scalar_to_string(other),
    }
}

/// Convert a cell string into a metadata YAML value (multivalued → sequence).
pub fn cell_to_value(slot: &str, cell: &str) -> serde_yaml::Value {
    if is_multivalued(slot) {
        serde_yaml::Value::Sequence(
            split_multivalued(cell).into_iter().map(serde_yaml::Value::String).collect(),
        )
    } else {
        serde_yaml::Value::String(cell.to_string())
    }
}

/// Flatten a YAML value to a list of strings (a scalar → one element).
pub fn flatten_value(v: &serde_yaml::Value) -> Vec<String> {
    match v {
        serde_yaml::Value::Sequence(seq) => seq.iter().map(scalar_to_string).collect(),
        other => vec![scalar_to_string(other)],
    }
}

fn scalar_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => String::new(),
        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pairs: &[(&str, &str)]) -> Mapping {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn multivalued_escaping_round_trips() {
        // A literal pipe and backslash inside element values survive a join/split.
        let vals = vec!["a|b".to_string(), "c\\d".to_string(), "plain".to_string()];
        let cell = join_multivalued(&vals);
        assert_eq!(cell, r"a\|b|c\\d|plain");
        assert_eq!(split_multivalued(&cell), vals);
        // Empty cell → no elements.
        assert!(split_multivalued("").is_empty());
    }

    #[test]
    fn cardinality_classifies_each_side() {
        let mut ms = MappingSet::new();
        ms.mappings = vec![
            rec(&[("subject_id", "A"), ("object_id", "X")]),
            rec(&[("subject_id", "A"), ("object_id", "Y")]),
            rec(&[("subject_id", "B"), ("object_id", "X")]),
        ];
        ms.set_mapping_cardinality();
        // Read SUBJECTS-sharing-this-object : OBJECTS-of-this-subject.
        // A→{X,Y}; X←{A,B}; Y←{A}; B→{X}.
        assert_eq!(ms.mappings[0]["mapping_cardinality"], "n:n"); // A→X: X has 2 subjects, A has 2 objects
        assert_eq!(ms.mappings[1]["mapping_cardinality"], "1:n"); // A→Y: Y has 1 subject,  A has 2 objects
        assert_eq!(ms.mappings[2]["mapping_cardinality"], "n:1"); // B→X: X has 2 subjects, B has 1 object
    }

    #[test]
    fn cardinality_respects_scope() {
        // Same subject A maps to X and Y, but under different predicates. With a
        // cardinality_scope of predicate_id, each scope sees A→one object (1:1).
        let mut ms = MappingSet::new();
        ms.metadata.insert(
            "cardinality_scope".into(),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("predicate_id".into())]),
        );
        ms.mappings = vec![
            rec(&[("subject_id", "A"), ("predicate_id", "skos:exactMatch"), ("object_id", "X")]),
            rec(&[("subject_id", "A"), ("predicate_id", "skos:broadMatch"), ("object_id", "Y")]),
        ];
        ms.set_mapping_cardinality();
        assert_eq!(ms.mappings[0]["mapping_cardinality"], "1:1");
        assert_eq!(ms.mappings[1]["mapping_cardinality"], "1:1");
        // Without the scope, A maps to two objects, and X has one subject → 1:n.
        ms.metadata.remove("cardinality_scope");
        ms.set_mapping_cardinality();
        assert_eq!(ms.mappings[0]["mapping_cardinality"], "1:n");
    }

    #[test]
    fn cardinality_handles_no_term_found() {
        let mut ms = MappingSet::new();
        ms.mappings = vec![
            rec(&[("subject_id", "C"), ("object_id", "sssom:NoTermFound")]),
            rec(&[("subject_id", "sssom:NoTermFound"), ("object_id", "D")]),
        ];
        ms.set_mapping_cardinality();
        assert_eq!(ms.mappings[0]["mapping_cardinality"], "1:0");
        assert_eq!(ms.mappings[1]["mapping_cardinality"], "0:1");
    }

    #[test]
    fn version_inference_detects_1_1_features() {
        let mut ms = MappingSet::new();
        ms.mappings = vec![rec(&[("subject_id", "A"), ("object_id", "X")])];
        assert_eq!(ms.infer_version(), "1.0");
        // record_id was added in 1.1, and the DATA now uses it.
        ms.mappings[0].insert("record_id".into(), "urn:x:1".into());
        assert_eq!(ms.infer_version(), "1.1");
        // The written table does not. No 1.1 slot has a mapping-level column, so
        // serializing this set emits nothing that needs 1.1, and it must not
        // claim a version its bytes do not use.
        ms.enforce_version();
        assert!(!ms.metadata.contains_key("sssom_version"));
        // A set-level 1.1 slot IS written, and that one does claim it.
        ms.metadata
            .insert("mapping_set_confidence".into(), serde_yaml::Value::String("0.9".into()));
        ms.enforce_version();
        assert_eq!(value_to_cell(&ms.metadata["sssom_version"]), "1.1");
    }

    #[test]
    fn a_set_is_read_at_the_version_it_declares() {
        let mut ms = MappingSet::new();
        ms.mappings = vec![rec(&[("subject_id", "A"), ("object_id", "X")])];
        ms.mappings[0].insert("record_id".into(), "urn:x:1".into());
        ms.metadata
            .insert("mapping_set_confidence".into(), serde_yaml::Value::String("0.9".into()));
        // Declaring no version makes it a 1.0 set, and a 1.0 set has neither slot.
        ms.restrict_to_declared_version();
        assert!(!ms.mappings[0].contains_key("record_id"));
        assert!(!ms.metadata.contains_key("mapping_set_confidence"));

        // Declaring 1.1 keeps both.
        let mut declared = MappingSet::new();
        declared.mappings = vec![rec(&[("subject_id", "A"), ("object_id", "X")])];
        declared.mappings[0].insert("record_id".into(), "urn:x:1".into());
        declared
            .metadata
            .insert("sssom_version".into(), serde_yaml::Value::String("1.1".into()));
        declared
            .metadata
            .insert("mapping_set_confidence".into(), serde_yaml::Value::String("0.9".into()));
        declared.restrict_to_declared_version();
        assert!(declared.mappings[0].contains_key("record_id"));
        assert!(declared.metadata.contains_key("mapping_set_confidence"));
    }
}
