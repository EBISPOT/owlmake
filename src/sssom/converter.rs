//! The prefix converter `sssom parse` compresses IRIs with.
//!
//! Every IRI read is compressed through a CHAIN of prefix maps, not through the
//! SSSOM built-in map alone: `sssom parse -C merged` chains the `-m` file's
//! `curie_map`, then the six SSSOM built-ins, then the extended prefix map
//! bundled beside this file.
//!
//! That EPM is where
//! `RO`, `BFO`, `SCTID: http://snomed.info/id/` and the rest of MONDO's mapping
//! namespaces come from; the repo's own `metadata/mondo.sssom.config.yml`
//! declares barely a third of them. Compressing with owlmake's own small
//! built-in table instead left every such IRI uncompressed, which is not a
//! near-miss: `subject_id` then holds an IRI where SSSOM requires a CURIE, and
//! MONDO's `add_object_label.py` fails outright on the result.
//!
//! A record's `uri_prefix_synonyms` all compress to its CANONICAL prefix, so
//! `http://purl.obolibrary.org/obo/OMIM_607208` and `https://omim.org/entry/607208`
//! both become `OMIM:…` — and which of the two the CURIE map then declares
//! depends on which converter in the chain won the prefix.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// One `curies` record: a canonical prefix and URI prefix, plus the synonyms
/// that resolve to them.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Record {
    pub prefix: String,
    pub uri_prefix: String,
    #[serde(default)]
    pub prefix_synonyms: Vec<String>,
    #[serde(default)]
    pub uri_prefix_synonyms: Vec<String>,
}

/// The OBO extended prefix map, vendored verbatim — one copy per emulated
/// version, because the map is part of what a version IS.
///
/// It is a data asset rather than a tool: a recipe that wants it copies it out of
/// the reference image, so there is nothing to reimplement and no way to derive
/// it — owlmake carries the bytes or the map does not exist. Each entry is a
/// prefix with its URI prefix and the synonyms that resolve to it.
///
/// The two differ by 388 prefixes and 1,418 shared entries, so which one is in
/// hand decides how every CURIE in every SSSOM artefact resolves. Emulating one
/// version with the other's map is not an approximation, it is a different
/// answer. Re-vendor by copying the file straight out of the image for that
/// version; there is no generation step to run.
const OBO_EPM_PRE_1_9_9: &str = include_str!("obo.epm.1.9.8.json");
const OBO_EPM: &str = include_str!("obo.epm.json");

/// Which of the two maps is in hand. Resolved at ingest, recorded as
/// `Plan::emulate_robot_version`, and set ONCE by `build::set_robot_behaviours` —
/// the plan is its only writer, as it is for the other version-dependent
/// switches. There is deliberately no environment override: it decides bytes.
static EPM_POST_1_9_9: AtomicBool = AtomicBool::new(true);

/// Select the map for the version being emulated — see the static above.
pub fn set_obo_epm(emulate_robot_version: (u32, u32, u32)) {
    EPM_POST_1_9_9.store(emulate_robot_version >= (1, 9, 9), Ordering::Relaxed);
}

pub fn obo_epm() -> &'static str {
    if EPM_POST_1_9_9.load(Ordering::Relaxed) { OBO_EPM } else { OBO_EPM_PRE_1_9_9 }
}

/// The six prefixes `_get_built_in_prefix_map` keeps from the SSSOM schema
/// context — the ones a converter chain always puts FIRST, so a metadata file
/// cannot rebind them.
const SSSOM_BUILT_IN: &[(&str, &str)] = &[
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("semapv", "https://w3id.org/semapv/vocab/"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("sssom", "https://w3id.org/sssom/"),
];

/// A chained prefix map: compresses an IRI to a CURIE, and reports the URI
/// prefix each canonical prefix is declared with.
#[derive(Debug, Clone, Default)]
pub struct Converter {
    records: Vec<Record>,
}

impl Converter {
    /// `Converter.from_prefix_map`: one record per binding, no synonyms.
    pub fn from_prefix_map<I, K, V>(map: I) -> Converter
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Converter {
            records: map
                .into_iter()
                .map(|(p, u)| Record {
                    prefix: p.into(),
                    uri_prefix: u.into(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    /// `curies.chain`: walk the converters in order and keep the FIRST record
    /// that claims a prefix or a URI prefix, folding every later claimant's
    /// prefix and URI prefix into that record as synonyms. So the earliest
    /// converter decides the canonical spelling and the later ones only widen
    /// what the map recognises.
    pub fn chain(converters: Vec<Converter>) -> Converter {
        let mut out: Vec<Record> = Vec::new();
        for c in converters {
            for r in c.records {
                let hit = out.iter().position(|e| {
                    e.prefix == r.prefix
                        || e.prefix_synonyms.contains(&r.prefix)
                        || r.prefix_synonyms.contains(&e.prefix)
                        || e.uri_prefix == r.uri_prefix
                        || e.uri_prefix_synonyms.contains(&r.uri_prefix)
                        || r.uri_prefix_synonyms.contains(&e.uri_prefix)
                });
                match hit {
                    Some(i) => {
                        let e = &mut out[i];
                        for p in std::iter::once(r.prefix.clone()).chain(r.prefix_synonyms) {
                            if e.prefix != p && !e.prefix_synonyms.contains(&p) {
                                e.prefix_synonyms.push(p);
                            }
                        }
                        for u in std::iter::once(r.uri_prefix.clone()).chain(r.uri_prefix_synonyms)
                        {
                            if e.uri_prefix != u && !e.uri_prefix_synonyms.contains(&u) {
                                e.uri_prefix_synonyms.push(u);
                            }
                        }
                    }
                    None => out.push(r),
                }
            }
        }
        Converter { records: out }
    }

    /// The standing chain: the SSSOM built-ins, then the bundled EPM.
    pub fn sssom_default() -> Converter {
        let epm: Vec<Record> = serde_json::from_str(obo_epm()).unwrap_or_default();
        // `_get_default_converter` drops any record whose prefix is not an
        // NCName, and any prefix synonym that is not one.
        let epm: Vec<Record> = epm
            .into_iter()
            .filter(|r| is_ncname(&r.prefix))
            .map(|mut r| {
                r.prefix_synonyms.retain(|s| is_ncname(s));
                r
            })
            .collect();
        Converter::chain(vec![
            Converter::from_prefix_map(SSSOM_BUILT_IN.iter().copied()),
            Converter { records: epm },
        ])
    }

    /// `ensure_converter(prefix_map)` for a NON-empty map, then `_merge_converter`
    /// under `-C merged`: the metadata's own map is chained behind the built-ins
    /// and ahead of the default map.
    pub fn merged_with_metadata(curie_map: &BTreeMap<String, String>) -> Converter {
        if curie_map.is_empty() {
            return Converter::sssom_default();
        }
        let meta = Converter::chain(vec![
            Converter::from_prefix_map(SSSOM_BUILT_IN.iter().copied()),
            Converter::from_prefix_map(curie_map.iter().map(|(p, u)| (p.clone(), u.clone()))),
        ]);
        Converter::chain(vec![meta, Converter::sssom_default()])
    }

    /// `safe_compress`: the CURIE for `uri`, or `None` when no URI prefix in the
    /// map is a prefix of it. The LONGEST match wins, as curies' trie does.
    pub fn compress(&self, uri: &str) -> Option<String> {
        let mut best: Option<(usize, &str, &str)> = None; // (len, prefix, uri_prefix)
        for r in &self.records {
            for u in std::iter::once(&r.uri_prefix).chain(r.uri_prefix_synonyms.iter()) {
                if !u.is_empty() && uri.starts_with(u.as_str()) {
                    let len = u.len();
                    if best.map(|(b, _, _)| len > b).unwrap_or(true) {
                        best = Some((len, &r.prefix, u));
                    }
                }
            }
        }
        let (len, prefix, _) = best?;
        Some(format!("{prefix}:{}", &uri[len..]))
    }

    /// The CURIE for `id`, which may be given as a URI **or** as a CURIE.
    ///
    /// A URI compresses. A string that is already a CURIE is STANDARDISED: its
    /// prefix is looked up among the canonical prefixes and their synonyms and
    /// replaced by the canonical one, so `OMIM:610799` becomes `omim:610799` and
    /// `Wikipedia:Foo` becomes `wikipedia.en:Foo`. A prefix the map does not know
    /// yields `None`, and the caller drops the record.
    ///
    /// Both forms occur in one document. OBO Graphs JSON gives a node's
    /// `meta.xrefs` as CURIEs and its edges and `basicPropertyValues` as full
    /// URIs, so a compressor that took URIs alone silently discarded every
    /// xref-derived mapping — on MONDO, all 142,731 of them.
    pub fn safe_compress(&self, id: &str) -> Option<String> {
        if let Some(curie) = self.compress(id) {
            return Some(curie);
        }
        // Not a URI under this map. Treat it as a CURIE and standardise the
        // prefix. A URI that no prefix covers is not rescued by this: `://`
        // never appears in a CURIE's prefix.
        let (prefix, local) = id.split_once(':')?;
        if prefix.is_empty() || local.starts_with("//") {
            return None;
        }
        let canonical = self.records.iter().find(|r| {
            r.prefix == prefix || r.prefix_synonyms.iter().any(|s| s == prefix)
        })?;
        Some(format!("{}:{local}", canonical.prefix))
    }

    /// The URI prefix `prefix` is declared with — what the written `curie_map`
    /// carries for it.
    pub fn uri_prefix(&self, prefix: &str) -> Option<&str> {
        self.records
            .iter()
            .find(|r| r.prefix == prefix)
            .map(|r| r.uri_prefix.as_str())
    }
}

/// XML NCName: a letter or `_` followed by letters, digits, `.`, `-` or `_`.
/// (`curies.is_ncname`, which `_get_default_converter` filters on.)
fn is_ncname(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    cs.all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epm_supplies_what_the_metadata_does_not() {
        let c = Converter::sssom_default();
        // MONDO's config declares none of these; the bundled EPM does, and the
        // ODK's own `tmp/mondo-extracted.sssom.tsv` compresses all three.
        assert_eq!(c.compress("http://purl.obolibrary.org/obo/RO_0002162").as_deref(), Some("RO:0002162"));
        assert_eq!(c.compress("http://purl.obolibrary.org/obo/BFO_0000054").as_deref(), Some("BFO:0000054"));
        assert_eq!(c.compress("http://snomed.info/id/62251004").as_deref(), Some("SCTID:62251004"));
    }

    /// Probed against the ODK's own `sssom.io._get_converter_and_metadata(
    /// metadata_path=metadata/mondo.sssom.config.yml, prefix_map_mode='merged')`,
    /// not reasoned about: every line below is what that converter returns.
    #[test]
    fn merged_matches_the_odk_converter() {
        let mut m = BTreeMap::new();
        m.insert("OMIM".to_string(), "https://omim.org/entry/".to_string());
        m.insert("MedDRA".to_string(), "http://identifiers.org/meddra/".to_string());
        let c = Converter::merged_with_metadata(&m);
        assert_eq!(c.uri_prefix("OMIM"), Some("https://omim.org/entry/"));
        for (uri, curie) in [
            ("https://omim.org/entry/607208", "OMIM:607208"),
            ("http://identifiers.org/meddra/10001843", "MedDRA:10001843"),
            ("http://purl.obolibrary.org/obo/RO_0002162", "RO:0002162"),
            ("http://snomed.info/id/62251004", "SCTID:62251004"),
            ("http://purl.obolibrary.org/obo/MONDO_0000001", "MONDO:0000001"),
            // The EPM binds OMIM to omim.org, so the obo-style spelling is NOT an
            // OMIM synonym and falls to the `obo` record.
            ("http://purl.obolibrary.org/obo/OMIM_607208", "obo:OMIM_607208"),
        ] {
            assert_eq!(c.compress(uri).as_deref(), Some(curie), "compressing {uri}");
        }
    }
}
