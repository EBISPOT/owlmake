//! The state an `*.ofn` cache carries about the document it stands in for, held
//! in a companion file rather than in the document's own bytes.
//!
//! A functional-syntax cache has to carry things the syntax cannot say: the
//! xmlns block of the RDF/XML it stands for, which anonymous expressions were
//! ONE blank node, whether the prefix map was cleared. Writing them into the
//! document as `#…` lines makes every such file differ from what the reference
//! writes, and a repo's own declared intermediates are exactly the files that
//! must not — `tmp/merged-cl-edit.ofn` and `tmp/validate.ofn` are targets a
//! build compares, not owlmake's scratch.
//!
//! # What makes a companion honest
//!
//! State kept beside a file is a lie the moment something else rewrites the
//! file. The directory a path sits in cannot tell owlmake that: a repo's `tmp/`
//! holds both its own declared intermediates and owlmake's caches, and the same
//! path can be rewritten by a later step in the same build.
//!
//! So the companion names the exact bytes it describes. A rewrite by any other
//! step — ROBOT, a shell redirect, a later owlmake write — changes those bytes
//! and the fingerprint no longer matches, and a companion that does not match is
//! not consulted. The failure mode is a cache MISS, which is the behaviour the
//! build had before any of this existed; it is never a stale answer.
//!
//! A rewrite that produces byte-identical output leaves the companion valid, and
//! that is correct rather than lucky: it describes the same document.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The `#…` state a cache used to carry inline.
#[derive(Default, Debug, Clone)]
pub struct Markers {
    pub prefixes_cleared: bool,
    pub rdf_prefixes: Vec<(String, String)>,
    pub explicit_prefixes: Vec<(String, String)>,
    pub idspaces: Vec<(String, String)>,
    /// owner IRI → structure hashes of the anonymous expressions that shared a node.
    pub anonshare: HashMap<String, HashSet<u64>>,
    /// owner IRI → the `(class, property, filler)` keys the RDF scan derived.
    pub sharedowner: HashMap<String, HashSet<String>>,
}

impl Markers {
    pub fn is_empty(&self) -> bool {
        !self.prefixes_cleared
            && self.rdf_prefixes.is_empty()
            && self.explicit_prefixes.is_empty()
            && self.idspaces.is_empty()
            && self.anonshare.is_empty()
            && self.sharedowner.is_empty()
    }
}

/// Where a document's companion lives: a hidden directory beside it, so the
/// repository tree gains no file next to its own intermediates and a comparison
/// that walks the tree can skip one name.
pub fn companion_path(doc: &Path) -> Option<PathBuf> {
    let dir = doc.parent()?;
    let name = doc.file_name()?.to_str()?;
    Some(dir.join(".omcache").join(format!("{name}.omcache")))
}

/// The fingerprint of the bytes a companion describes: length and a content
/// hash. Length alone is far too weak — a rewrite that preserves size is exactly
/// the case a cache must not survive — so the hash is over every byte.
pub fn fingerprint(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{}:{:016x}", bytes.len(), h)
}

fn enc(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .filter(|(p, ns)| !p.is_empty() && !p.contains(' ') && !ns.contains(' '))
        .map(|(p, ns)| format!("{p}={ns}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn dec(rest: &str) -> Vec<(String, String)> {
    rest.split_whitespace()
        .filter_map(|kv| kv.split_once('=').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect()
}

/// Write the companion for a document whose bytes are `doc`. Best-effort: a
/// companion that cannot be written is a cache that will miss, not a failed
/// build, so the caller's write still stands.
pub fn write(doc_path: &Path, doc: &[u8], m: &Markers) {
    let Some(p) = companion_path(doc_path) else { return };
    if m.is_empty() {
        // Nothing to carry — and any companion left from a previous write
        // describes a document that no longer exists here.
        let _ = std::fs::remove_file(&p);
        return;
    }
    let mut out = String::new();
    out.push_str("#omcache 1\n");
    out.push_str(&format!("#doc {}\n", fingerprint(doc)));
    if m.prefixes_cleared {
        out.push_str("#prefixes-cleared\n");
    }
    for (tag, pairs) in [
        ("#rdfxmlns ", &m.rdf_prefixes),
        ("#explicit-prefixes ", &m.explicit_prefixes),
        ("#idspaces ", &m.idspaces),
    ] {
        let j = enc(pairs);
        if !j.is_empty() {
            out.push_str(tag);
            out.push_str(&j);
            out.push('\n');
        }
    }
    // Owner-keyed lines are the bulk — MONDO carries 2,189 `#sharedowner` lines
    // on a 102 MB document — so they are written straight out in sorted order
    // rather than through any intermediate structure.
    let mut owners: Vec<&String> = m.anonshare.keys().collect();
    owners.sort();
    for owner in owners {
        let sigs = &m.anonshare[owner];
        if owner.contains(' ') || sigs.is_empty() {
            continue;
        }
        let mut v: Vec<String> = sigs.iter().map(|h| format!("{h:x}")).collect();
        v.sort();
        out.push_str(&format!("#anonshare {owner} {}\n", v.join(" ")));
    }
    let mut owners: Vec<&String> = m.sharedowner.keys().collect();
    owners.sort();
    for owner in owners {
        let keys = &m.sharedowner[owner];
        if owner.contains(' ') || keys.is_empty() {
            continue;
        }
        let mut v: Vec<String> = keys.iter().map(|k| k.replace('\u{1}', "\u{2}")).collect();
        v.sort();
        if v.iter().any(|k| k.contains(' ')) {
            continue;
        }
        out.push_str(&format!("#sharedowner {owner} {}\n", v.join(" ")));
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, out);
}

/// Read the companion for `doc_path`, if it describes exactly the bytes `doc`.
///
/// A companion whose fingerprint does not match the document is IGNORED, not
/// repaired and not trusted: something rewrote the file since it was written,
/// and what it says about the old bytes says nothing about the new ones.
pub fn read(doc_path: &Path, doc: &[u8]) -> Option<Markers> {
    let p = companion_path(doc_path)?;
    let text = std::fs::read_to_string(&p).ok()?;
    let want = fingerprint(doc);
    let stamped = text.lines().find_map(|l| l.strip_prefix("#doc "))?;
    if stamped.trim() != want {
        return None;
    }
    let mut m = Markers::default();
    for l in text.lines() {
        if l.trim_end() == "#prefixes-cleared" {
            m.prefixes_cleared = true;
        } else if let Some(r) = l.strip_prefix("#rdfxmlns ") {
            m.rdf_prefixes = dec(r);
        } else if let Some(r) = l.strip_prefix("#explicit-prefixes ") {
            m.explicit_prefixes = dec(r);
        } else if let Some(r) = l.strip_prefix("#idspaces ") {
            m.idspaces = dec(r);
        } else if let Some(r) = l.strip_prefix("#anonshare ") {
            let mut it = r.split_whitespace();
            if let Some(owner) = it.next() {
                m.anonshare.insert(
                    owner.to_string(),
                    it.filter_map(|h| u64::from_str_radix(h, 16).ok()).collect(),
                );
            }
        } else if let Some(r) = l.strip_prefix("#sharedowner ") {
            let mut it = r.split_whitespace();
            if let Some(owner) = it.next() {
                m.sharedowner.insert(
                    owner.to_string(),
                    it.map(|k| k.replace('\u{2}', "\u{1}")).collect(),
                );
            }
        }
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory on the build volume — never the system temp dir,
    /// which on this machine is a RAM filesystem shared with everything else.
    fn scratch(tag: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("omcache-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> Markers {
        let mut m = Markers { prefixes_cleared: true, ..Default::default() };
        m.rdf_prefixes = vec![("obo".into(), "http://purl.obolibrary.org/obo/".into())];
        m.anonshare.insert("http://x/A".into(), [1u64, 2].into_iter().collect());
        m.sharedowner.insert("http://x/B".into(), ["k1\u{1}k2".to_string()].into_iter().collect());
        m
    }

    #[test]
    fn round_trips_through_the_companion() {
        let dir = scratch("roundtrip");
        let doc = dir.join("x.ofn");
        let bytes = b"Ontology(<http://x/>)\n";
        std::fs::write(&doc, bytes).unwrap();
        write(&doc, bytes, &sample());
        let back = read(&doc, bytes).expect("companion describes these bytes");
        assert!(back.prefixes_cleared);
        assert_eq!(back.rdf_prefixes.len(), 1);
        assert_eq!(back.anonshare["http://x/A"].len(), 2);
        assert_eq!(back.sharedowner["http://x/B"].iter().next().unwrap(), "k1\u{1}k2");
    }

    /// The guard that makes a companion honest: another step rewrites the
    /// document, and what the companion says about the old bytes is withdrawn.
    #[test]
    fn a_rewritten_document_withdraws_its_companion() {
        let dir = scratch("rewrite");
        let doc = dir.join("x.ofn");
        let before = b"Ontology(<http://x/>)\n";
        std::fs::write(&doc, before).unwrap();
        write(&doc, before, &sample());
        assert!(read(&doc, before).is_some());
        // Same length, different bytes — the case a length check would miss.
        let after = b"Ontology(<http://y/>)\n";
        assert_eq!(before.len(), after.len());
        std::fs::write(&doc, after).unwrap();
        assert!(read(&doc, after).is_none(), "a companion must not survive a rewrite");
    }

    #[test]
    fn nothing_to_carry_leaves_no_companion() {
        let dir = scratch("empty");
        let doc = dir.join("x.ofn");
        let bytes = b"Ontology(<http://x/>)\n";
        std::fs::write(&doc, bytes).unwrap();
        write(&doc, bytes, &sample());
        assert!(companion_path(&doc).unwrap().exists());
        write(&doc, bytes, &Markers::default());
        assert!(!companion_path(&doc).unwrap().exists(), "a stale companion is removed");
    }
}
