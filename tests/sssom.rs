//! End-to-end tests for the `owlmake sssom` command line.
//!
//! These exercise the real binary the way a repo's mapping scripts invoke it:
//! parse/convert round-trips, the table operations (sort, filter, annotate,
//! merge, invert, dedupe, remove, diff, split), validation, and the JSON/OWL
//! serialisations.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_om");

/// A minimal but representative SSSOM/TSV file with an embedded metadata header.
const SAMPLE: &str = "\
# curie_map:
#   HP: http://purl.obolibrary.org/obo/HP_
#   MP: http://purl.obolibrary.org/obo/MP_
#   skos: http://www.w3.org/2004/02/skos/core#
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/set1
subject_id\tpredicate_id\tobject_id\tmapping_justification\tconfidence
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\t0.8
HP:0000020\tskos:exactMatch\tMP:0000020\tsemapv:LexicalMatching\t0.9
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:ManualMappingCuration\t0.5
";

fn run(args: &[&str], input: Option<&str>) -> (String, String, i32) {
    let mut cmd = Command::new(BIN);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().expect("spawn");
    if let Some(inp) = input {
        child.stdin.take().unwrap().write_all(inp.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Write a temp file, return its path.
fn tmp(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("owlmake-sssom-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn convert_roundtrip_tsv() {
    let f = tmp("a.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(&["sssom", "convert", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    // Header preserved (as `# ` YAML), and the records survive.
    assert!(out.contains("# mapping_set_id: https://example.org/set1"), "{out}");
    assert!(out.contains("# curie_map:"), "{out}");
    assert!(out.contains("subject_id\tpredicate_id\tobject_id"), "{out}");
    assert!(out.contains("HP:0000010\tskos:exactMatch\tMP:0000010"), "{out}");
}

#[test]
fn convert_to_json() {
    let f = tmp("b.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(&["sssom", "convert", "-O", "json", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(v["mapping_set_id"], "https://example.org/set1");
    assert!(v["mappings"].as_array().unwrap().len() == 3, "{out}");
    assert!(v["curie_map"]["HP"].is_string());
}

#[test]
fn convert_to_owl_turtle() {
    let f = tmp("c.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(&["sssom", "convert", "-O", "owl", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.contains("owl:Axiom"), "{out}");
    assert!(out.contains("owl:annotatedSource"), "{out}");
    // Hydrated direct triple present in OWL mode.
    assert!(out.contains("HP:0000010 skos:exactMatch MP:0000010"), "{out}");
}

#[test]
fn sort_orders_rows_and_columns() {
    let f = tmp("d.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(&["sssom", "sort", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    let body: Vec<&str> = out.lines().filter(|l| !l.starts_with('#')).collect();
    // First data row after header should be the lowest subject/object/justification.
    assert!(body[0].starts_with("subject_id"), "header first: {out}");
    assert!(body[1].starts_with("HP:0000010"), "{out}");
}

#[test]
fn filter_by_predicate_and_confidence() {
    let f = tmp("e.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(
        &["sssom", "filter", "--object-id", "MP:0000020", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    let rows = out.lines().filter(|l| l.starts_with("HP:")).count();
    assert_eq!(rows, 1, "{out}");
    assert!(out.contains("HP:0000020"), "{out}");
}

#[test]
fn annotate_sets_metadata() {
    let f = tmp("f.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(
        &["sssom", "annotate", "--mapping-set-title", "My Mappings", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.contains("# mapping_set_title: My Mappings"), "{out}");
}

#[test]
fn dedupe_keeps_highest_confidence() {
    let f = tmp("g.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(&["sssom", "dedupe", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    // The duplicate HP:0000010→MP:0000010 (0.8 vs 0.5) collapses to one row.
    let rows = out.matches("HP:0000010\tskos:exactMatch\tMP:0000010").count();
    assert_eq!(rows, 1, "{out}");
    assert!(out.contains("\t0.8"), "kept higher confidence: {out}");
}

#[test]
fn invert_swaps_subject_object() {
    let f = tmp("h.sssom.tsv", SAMPLE);
    let (out, err, rc) =
        run(&["sssom", "invert", "--no-merge-inverted", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    // exactMatch is symmetric, so inverted rows have MP as subject.
    assert!(out.contains("MP:0000010\tskos:exactMatch\tHP:0000010"), "{out}");
    assert!(out.contains("semapv:MappingInversion"), "{out}");
}

#[test]
fn validate_clean_file_passes() {
    let f = tmp("i.sssom.tsv", SAMPLE);
    let (_out, _err, rc) = run(&["sssom", "validate", f.to_str().unwrap()], None);
    assert_eq!(rc, 0);
}

#[test]
fn validate_detects_missing_required() {
    let bad = "subject_id\tobject_id\nHP:1\tMP:1\n";
    let f = tmp("bad.sssom.tsv", bad);
    let (_out, err, rc) = run(&["sssom", "validate", f.to_str().unwrap()], None);
    assert_eq!(rc, 1, "should fail validation");
    assert!(err.contains("predicate_id") || err.contains("mapping_justification"), "{err}");
}

#[test]
fn merge_unions_sets() {
    let a = tmp("m1.sssom.tsv", SAMPLE);
    let b = "\
# mapping_set_id: https://example.org/set2
subject_id\tpredicate_id\tobject_id\tmapping_justification
HP:0000030\tskos:exactMatch\tMP:0000030\tsemapv:LexicalMatching
";
    let bf = tmp("m2.sssom.tsv", b);
    let (out, err, rc) =
        run(&["sssom", "merge", a.to_str().unwrap(), bf.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.contains("HP:0000030"), "{out}");
    assert!(out.contains("HP:0000020"), "{out}");
}

#[test]
fn remove_drops_listed_mappings() {
    let f = tmp("r1.sssom.tsv", SAMPLE);
    let rm = "\
subject_id\tpredicate_id\tobject_id\tmapping_justification
HP:0000020\tskos:exactMatch\tMP:0000020\tsemapv:LexicalMatching
";
    let rmf = tmp("rm.sssom.tsv", rm);
    let (out, err, rc) = run(
        &["sssom", "remove", "--remove-map", rmf.to_str().unwrap(), f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(!out.contains("HP:0000020"), "removed: {out}");
    assert!(out.contains("HP:0000010"), "kept: {out}");
}

#[test]
fn split_writes_per_prefix_files() {
    let f = tmp("s1.sssom.tsv", SAMPLE);
    let dir = std::env::temp_dir()
        .join(format!("owlmake-sssom-split-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (_out, err, rc) = run(
        &["sssom", "split", "-d", dir.to_str().unwrap(), f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    let produced = std::fs::read_dir(&dir).unwrap().count();
    assert!(produced >= 1, "expected at least one split file");
}

#[test]
fn rewire_collapses_equivalent_iris() {
    // A tiny Turtle ontology where MONDO:1 subClassOf MONDO:2, plus an equivalence
    // mapping DOID:1 == MONDO:1. Rewiring should replace DOID:1 with MONDO:1.
    let onto = "\
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix MONDO: <http://purl.obolibrary.org/obo/MONDO_> .
@prefix DOID: <http://purl.obolibrary.org/obo/DOID_> .
DOID:1 a owl:Class ; rdfs:subClassOf MONDO:2 .
";
    let of = tmp("onto.ttl", onto);
    let maps = "\
# curie_map:
#   MONDO: http://purl.obolibrary.org/obo/MONDO_
#   DOID: http://purl.obolibrary.org/obo/DOID_
# mapping_set_id: https://example.org/eq
subject_id\tpredicate_id\tobject_id\tmapping_justification
DOID:1\towl:equivalentClass\tMONDO:1\tsemapv:ManualMappingCuration
";
    let mf = tmp("eq.sssom.tsv", maps);
    let (out, err, rc) = run(
        &[
            "sssom",
            "rewire",
            "-O",
            "owl",
            "-m",
            mf.to_str().unwrap(),
            of.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    // DOID_1 has been rewired to MONDO_1 in the serialized ontology.
    assert!(out.contains("MONDO_1"), "expected MONDO_1 in output: {out}");
    assert!(!out.contains("DOID_1"), "DOID_1 should be gone: {out}");
}

#[test]
fn xref_extract_from_ontology() {
    // An OBO term carrying an xref becomes an oboInOwl:hasDbXref annotation, which
    // xref-extract turns into a SSSOM mapping with the mapped predicate.
    let obo = "\
format-version: 1.2

[Term]
id: ZFA:0000001
name: thing
xref: UBERON:0000001

[Term]
id: ZFA:0000002
name: other
xref: GO:0000002
";
    let of = tmp("x.obo", obo);
    let out = tmp("xrefs.sssom.tsv", "");
    let (_o, err, rc) = run(
        &[
            // The colon form is the only entry point: these three commands take
            // the global options, so they live on the chain path and not in the
            // standalone SSSOM CLI.
            "sssom:xref-extract",
            "-i",
            of.to_str().unwrap(),
            "--mapping-file",
            out.to_str().unwrap(),
            "--map-prefix-to-predicate",
            "UBERON http://w3id.org/semapv/vocab/crossSpeciesExactMatch",
        ],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    let written = std::fs::read_to_string(&out).unwrap();
    // Only the UBERON xref is mapped (GO is not in the prefix→predicate map).
    assert!(written.contains("UBERON:0000001"), "{written}");
    assert!(written.contains("crossSpeciesExactMatch"), "{written}");
    assert!(!written.contains("GO:0000002"), "GO xref should be skipped: {written}");
}

#[test]
fn xref_extract_without_a_prefix_map_extracts_nothing() {
    // `XrefExtractor.extract` skips an xref whose prefix has no predicate mapped
    // to it unless `--all-xrefs` was given:
    //   if (!prefixToPredicateMap.containsKey(parts[0]) && !includeGeneric) continue;
    // …with `includeGeneric = line.hasOption("all-xrefs")`. So a bare
    // `sssom:xref-extract -i x.obo` writes an EMPTY mapping set, and `--all-xrefs`
    // is what falls back to `oboInOwl:hasDbXref`.
    let obo = "format-version: 1.2\n\n[Term]\nid: ZFA:0000003\nxref: UBERON:0000003\n";
    let of = tmp("y.obo", obo);
    let out = tmp("xrefs2.sssom.tsv", "");
    let (_o, err, rc) = run(
        &["sssom:xref-extract", "-i", of.to_str().unwrap(), "--mapping-file", out.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(!std::fs::read_to_string(&out).unwrap().contains("UBERON:0000003"));

    let out2 = tmp("xrefs3.sssom.tsv", "");
    let (_o, err, rc) = run(
        &[
            "sssom:xref-extract",
            "-i",
            of.to_str().unwrap(),
            "--all-xrefs",
            "--mapping-file",
            out2.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    let written = std::fs::read_to_string(&out2).unwrap();
    assert!(written.contains("UBERON:0000003"), "{written}");
    assert!(written.contains("hasDbXref"), "{written}");
}

#[test]
fn parse_predicate_filter() {
    let f = tmp("p1.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(
        &[
            "sssom",
            "parse",
            "-F",
            "skos:exactMatch",
            f.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.contains("HP:0000010"), "{out}");
}

// ── the remaining subcommands ────────────────────────────────────────────────

#[test]
fn diff_tags_unique_and_common() {
    // SAMPLE has the (HP:10,MP:10) and (HP:20,MP:20) pairs; set2 shares (HP:10,MP:10)
    // and adds (HP:30,MP:30). diff keys on the unordered (subject,object) pair.
    let a = tmp("diff_a.sssom.tsv", SAMPLE);
    let b = "\
# mapping_set_id: https://example.org/set2
subject_id\tpredicate_id\tobject_id\tmapping_justification\tconfidence
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\t0.7
HP:0000030\tskos:exactMatch\tMP:0000030\tsemapv:LexicalMatching\t0.6
";
    let bf = tmp("diff_b.sssom.tsv", b);
    let (out, err, rc) =
        run(&["sssom", "diff", a.to_str().unwrap(), bf.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    // A `comment` column tags each row's provenance.
    assert!(out.contains("comment"), "comment column present: {out}");
    assert!(out.contains("COMMON_TO_BOTH"), "shared pair tagged: {out}");
    assert!(out.contains("UNIQUE_1"), "HP:0000020 only in A: {out}");
    assert!(out.contains("UNIQUE_2") && out.contains("HP:0000030"), "HP:0000030 only in B: {out}");
}

#[test]
fn partition_splits_into_components() {
    // SAMPLE's two disjoint pairs (HP:10↔MP:10, HP:20↔MP:20) are separate cliques.
    let f = tmp("part.sssom.tsv", SAMPLE);
    let dir = std::env::temp_dir().join(format!("owlmake-sssom-part-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (_o, err, rc) = run(
        &["sssom", "partition", "-d", dir.to_str().unwrap(), f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(files.len(), 2, "two connected components → two partition files");
}

#[test]
fn cliquesummary_lists_cliques() {
    let f = tmp("clq.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(&["sssom", "cliquesummary", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.starts_with("clique_id\tsize\tmembers"), "header: {out}");
    // Two cliques, each of size 2 (a subject + its object).
    let rows: Vec<&str> = out.lines().skip(1).filter(|l| !l.is_empty()).collect();
    assert_eq!(rows.len(), 2, "{out}");
    assert!(rows.iter().all(|r| r.contains("\t2\t")), "each clique has 2 members: {out}");
    assert!(out.contains("HP:0000010") && out.contains("MP:0000010"), "members listed: {out}");
}

#[test]
fn crosstab_tabulates_counts() {
    let f = tmp("xt.sssom.tsv", SAMPLE);
    // No categories in SAMPLE, so tabulate over subject/object ids directly.
    let (out, err, rc) = run(
        &["sssom", "crosstab", "-f", "subject_id", "object_id", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.starts_with("subject_id\t"), "row-field header: {out}");
    assert!(out.contains("MP:0000010"), "object_id as a column: {out}");
    // (HP:0000010, MP:0000010) occurs twice in SAMPLE.
    let hp10 = out.lines().find(|l| l.starts_with("HP:0000010")).expect("HP:0000010 row");
    assert!(hp10.contains('2'), "duplicate pair counted twice: {hp10}");
}

#[test]
fn correlations_runs_like_crosstab() {
    let f = tmp("corr.sssom.tsv", SAMPLE);
    let (out, err, rc) = run(
        &["sssom", "correlations", "-f", "subject_id", "object_id", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.starts_with("subject_id\t"), "{out}");
}

#[test]
fn reconcile_prefixes_rewrites_synonyms() {
    let f = tmp("rec.sssom.tsv", SAMPLE);
    // Map the alias prefix `MP` onto the canonical `MOUSE`.
    let cfg = "\
prefix_synonyms:
  MP: MOUSE
prefixes:
  MOUSE: http://purl.obolibrary.org/obo/MP_
";
    let cfgf = tmp("reconcile.yaml", cfg);
    let (out, err, rc) = run(
        &["sssom", "reconcile-prefixes", "-p", cfgf.to_str().unwrap(), f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.contains("MOUSE:0000010"), "object CURIE reconciled: {out}");
    assert!(!out.contains("MP:0000010"), "old prefix gone from data: {out}");
}

#[test]
fn convert_to_fhir_json() {
    let f = tmp("fhir.sssom.tsv", SAMPLE);
    let (out, err, rc) =
        run(&["sssom", "convert", "-O", "fhir_json", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(v["resourceType"], "ConceptMap");
    let elems = v["group"][0]["element"].as_array().expect("elements");
    assert!(elems.iter().any(|e| e["code"] == "HP:0000010"), "{out}");
    // skos:exactMatch maps to FHIR equivalence "equivalent".
    assert!(
        elems.iter().any(|e| e["target"][0]["equivalence"] == "equivalent"),
        "{out}"
    );
}

#[test]
fn convert_to_ontoportal_json() {
    let f = tmp("op.sssom.tsv", SAMPLE);
    let (out, err, rc) =
        run(&["sssom", "convert", "-O", "ontoportal_json", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid json array");
    let arr = v.as_array().expect("array");
    assert!(!arr.is_empty(), "{out}");
    // CURIEs are expanded to full IRIs in the `classes` pair.
    assert!(
        arr.iter().any(|m| m["classes"]
            .as_array()
            .map(|c| c.iter().any(|x| x == "http://purl.obolibrary.org/obo/HP_0000010"))
            .unwrap_or(false)),
        "{out}"
    );
}

#[test]
fn parse_obographs_json() {
    // `from_obographs` keeps an edge only when its predicate is one of the seven
    // `DEFAULT_MAPPING_PROPERTIES`, compared as a full IRI — obographs writes
    // `is_a` or an IRI, never a CURIE — and every id is compressed through the
    // converter, so the rows carry CURIEs and not the IRIs that went in.
    let json = r#"{"graphs":[{"nodes":[{"id":"http://purl.obolibrary.org/obo/HP_0000001","lbl":"All"}],"edges":[{"sub":"http://purl.obolibrary.org/obo/HP_0000001","pred":"http://www.w3.org/2004/02/skos/core#exactMatch","obj":"http://purl.obolibrary.org/obo/MP_0000001"},{"sub":"http://purl.obolibrary.org/obo/HP_0000001","pred":"is_a","obj":"http://purl.obolibrary.org/obo/HP_0000000"}]}]}"#;
    let f = tmp("og.json", json);
    let (out, err, rc) = run(
        &["sssom", "parse", "-I", "obographs-json", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.contains("HP:0000001"), "subject: {out}");
    assert!(out.contains("MP:0000001"), "object: {out}");
    assert!(out.contains("skos:exactMatch"), "mapping predicate: {out}");
    // A node's own `lbl` becomes `subject_label`.
    assert!(out.contains("All"), "subject label: {out}");
    // `is_a` reads as rdfs:subClassOf, which is not a mapping predicate.
    assert!(!out.contains("HP:0000000"), "is_a edge skipped: {out}");
}

#[test]
fn parse_alignment_xml() {
    let xml = "\
<?xml version=\"1.0\"?>
<rdf:RDF><Alignment><map><Cell>
<entity1 rdf:resource=\"http://example.org/A\"/>
<entity2 rdf:resource=\"http://example.org/B\"/>
<relation>=</relation><measure>0.95</measure>
</Cell></map></Alignment></rdf:RDF>
";
    let f = tmp("align.xml", xml);
    let (out, err, rc) = run(
        &["sssom", "parse", "-I", "alignment-api-xml", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 0, "stderr: {err}");
    assert!(out.contains("http://example.org/A"), "subject: {out}");
    assert!(out.contains("http://example.org/B"), "object: {out}");
    assert!(out.contains("skos:exactMatch"), "= → exactMatch: {out}");
    assert!(out.contains("0.95"), "measure → confidence: {out}");
}

// ── SSSOM 1.1 conformance ─────────────────────────────────────────────────────

/// A fully conformant (1.0-feature) set: required set slots + a clean mapping.
const CLEAN: &str = "\
# curie_map:
#   HP: http://purl.obolibrary.org/obo/HP_
#   MP: http://purl.obolibrary.org/obo/MP_
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/set1
subject_id\tpredicate_id\tobject_id\tmapping_justification
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching
";

fn validate(content: &str, name: &str) -> (String, i32) {
    let f = tmp(name, content);
    let (_o, err, rc) = run(&["sssom", "validate", f.to_str().unwrap()], None);
    (err, rc)
}

#[test]
fn conformance_clean_set_passes() {
    let (_e, rc) = validate(CLEAN, "conf_clean.sssom.tsv");
    assert_eq!(rc, 0);
}

#[test]
fn conformance_rejects_missing_license() {
    let no_license = CLEAN.replace("# license: https://creativecommons.org/publicdomain/zero/1.0/\n", "");
    let (err, rc) = validate(&no_license, "conf_nolicense.sssom.tsv");
    assert_eq!(rc, 1);
    assert!(err.contains("license"), "{err}");
}

#[test]
fn conformance_rejects_bad_justification() {
    let bad = CLEAN.replace("semapv:LexicalMatching", "semapv:Bogus");
    let (err, rc) = validate(&bad, "conf_just.sssom.tsv");
    assert_eq!(rc, 1);
    assert!(err.contains("mapping_justification"), "{err}");
}

#[test]
fn conformance_rejects_bad_entity_type() {
    let bad = CLEAN
        .replace(
            "subject_id\tpredicate_id\tobject_id\tmapping_justification",
            "subject_id\tpredicate_id\tobject_id\tmapping_justification\tsubject_type",
        )
        .replace(
            "HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching",
            "HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\towl klass",
        );
    let (err, rc) = validate(&bad, "conf_type.sssom.tsv");
    assert_eq!(rc, 1);
    assert!(err.contains("subject_type"), "{err}");
}

#[test]
fn conformance_rejects_confidence_out_of_range() {
    let bad = CLEAN
        .replace(
            "subject_id\tpredicate_id\tobject_id\tmapping_justification",
            "subject_id\tpredicate_id\tobject_id\tmapping_justification\tconfidence",
        )
        .replace(
            "HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching",
            "HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\t1.5",
        );
    let (err, rc) = validate(&bad, "conf_conf.sssom.tsv");
    assert_eq!(rc, 1);
    assert!(err.contains("confidence"), "{err}");
}

#[test]
fn conformance_rejects_self_contradictory_version() {
    let bad = CLEAN.replace("# license:", "# sssom_version: '1.0'\n# license:");
    let (err, rc) = validate(&bad, "conf_v10.sssom.tsv");
    assert_eq!(rc, 1);
    assert!(err.contains("self-contradictory"), "{err}");
}

#[test]
fn conformance_rejects_undeclared_1_1_version() {
    // record_id is a 1.1 feature, so the set MUST declare sssom_version: 1.1.
    let bad = CLEAN
        .replace(
            "subject_id\tpredicate_id\tobject_id\tmapping_justification",
            "subject_id\tpredicate_id\tobject_id\tmapping_justification\trecord_id",
        )
        .replace(
            "HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching",
            "HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\tex:rec1",
        );
    let (err, rc) = validate(&bad, "conf_undeclared.sssom.tsv");
    assert_eq!(rc, 1);
    assert!(err.contains("1.1"), "{err}");
}

#[test]
fn conformance_accepts_literal_mapping() {
    // object is a bare literal: object_type=rdfs literal, object_label carries it,
    // object_id may be omitted entirely.
    let lit = "\
# curie_map:
#   HP: http://purl.obolibrary.org/obo/HP_
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/lit
subject_id\tpredicate_id\tobject_type\tobject_label\tmapping_justification
HP:0000010\tskos:exactMatch\trdfs literal\tsome free text\tsemapv:LexicalMatching
";
    let (err, rc) = validate(lit, "conf_literal.sssom.tsv");
    assert_eq!(rc, 0, "{err}");
}

#[test]
fn conformance_rejects_review_without_reviewer() {
    let bad = "\
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/rev
# sssom_version: '1.1'
subject_id\tpredicate_id\tobject_id\tmapping_justification\treview_date
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\t2020-01-01
";
    let f = tmp("conf_review.sssom.tsv", bad);
    // HP/MP not declared here; isolate the cross-field check.
    let (_o, err, rc) = run(
        &["sssom", "validate", "--validation-types", "crossfield", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 1);
    assert!(err.contains("reviewer"), "{err}");
}

#[test]
fn conformance_rejects_extension_slot_collision() {
    let bad = "\
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/ext
# extension_definitions:
#   - slot_name: subject_id
#     property: http://example.org/p
subject_id\tpredicate_id\tobject_id\tmapping_justification
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching
";
    let f = tmp("conf_ext.sssom.tsv", bad);
    let (_o, err, rc) = run(
        &["sssom", "validate", "--validation-types", "extension", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 1);
    assert!(err.contains("collides"), "{err}");
}

#[test]
fn conformance_rejects_partial_record_id() {
    let bad = "\
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/rid
# sssom_version: '1.1'
subject_id\tpredicate_id\tobject_id\tmapping_justification\trecord_id
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\tex:r1
HP:0000020\tskos:exactMatch\tMP:0000020\tsemapv:LexicalMatching\t
";
    let f = tmp("conf_rid.sssom.tsv", bad);
    let (_o, err, rc) = run(
        &["sssom", "validate", "--validation-types", "record_id", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 1);
    assert!(err.contains("all-or-none"), "{err}");
}

#[test]
fn convert_never_declares_a_version_the_set_did_not() {
    // A set is written at the version it DECLARES. Carrying a 1.1 slot —
    // `record_id` on a record, `mapping_set_confidence` on the set — is a fact
    // about the data and not an instruction to the writer, so an undeclared set
    // stays undeclared through a convert.
    let f = tmp(
        "conf_writever.sssom.tsv",
        "\
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_confidence: 0.9
# mapping_set_id: https://example.org/w
subject_id\tpredicate_id\tobject_id\tmapping_justification\trecord_id
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\tex:r1
",
    );
    let (out, err, rc) = run(&["sssom", "convert", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "{err}");
    assert!(!out.contains("sssom_version"), "no version invented: {out}");
    // A declaration that IS there survives, and so do the slots it licenses.
    let g = tmp(
        "conf_writever_declared.sssom.tsv",
        "\
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_confidence: 0.9
# mapping_set_id: https://example.org/w3
# sssom_version: \"1.1\"
subject_id\tpredicate_id\tobject_id\tmapping_justification\trecord_id
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\tex:r1
",
    );
    let (out, err, rc) = run(&["sssom", "convert", g.to_str().unwrap()], None);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("sssom_version: '1.1'"), "declaration survives: {out}");
    assert!(out.contains("record_id"), "and the slots it licenses: {out}");
}

#[test]
fn multivalued_pipe_escaping_round_trips_through_json() {
    // author_label is multivalued; one element contains a literal pipe (\|).
    let f = tmp(
        "conf_escape.sssom.tsv",
        "\
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/esc
subject_id\tpredicate_id\tobject_id\tmapping_justification\tauthor_label
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\tSmith\\|Jones|Doe
",
    );
    let (out, err, rc) = run(&["sssom", "convert", "-O", "json", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    let labels = v["mappings"][0]["author_label"].as_array().expect("array");
    // The escaped pipe is one element "Smith|Jones", not two.
    assert_eq!(labels.len(), 2, "{out}");
    assert_eq!(labels[0], "Smith|Jones", "{out}");
    assert_eq!(labels[1], "Doe", "{out}");
}

#[test]
fn conformance_typed_extension_value() {
    let header = "\
# curie_map:
#   HP: http://purl.obolibrary.org/obo/HP_
#   MP: http://purl.obolibrary.org/obo/MP_
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/ext
# extension_definitions:
#   - slot_name: ext_score
#     property: http://example.org/score
#     type_hint: xsd:integer
subject_id\tpredicate_id\tobject_id\tmapping_justification\text_score
";
    // Non-integer extension value fails the type_hint check.
    let bad = format!("{header}HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\tnotnum\n");
    let f = tmp("ext_bad.sssom.tsv", &bad);
    let (_o, err, rc) = run(
        &["sssom", "validate", "--validation-types", "extension", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 1);
    assert!(err.contains("ext_score") && err.contains("xsd:integer"), "{err}");
    // A valid integer passes the full check.
    let good = format!("{header}HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\t42\n");
    let (_e, rc2) = validate(&good, "ext_good.sssom.tsv");
    assert_eq!(rc2, 0);
}

#[test]
fn conformance_structure_rejects_stray_comment() {
    let bad = format!("{CLEAN}# a stray comment after the table\n");
    let f = tmp("struct_stray.sssom.tsv", &bad);
    let (_o, err, rc) = run(
        &["sssom", "validate", "--validation-types", "structure", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 1);
    assert!(err.contains("stray"), "{err}");
}

#[test]
fn conformance_structure_rejects_blank_line_in_header() {
    let bad = "\
# license: https://creativecommons.org/publicdomain/zero/1.0/

# mapping_set_id: https://example.org/s
subject_id\tpredicate_id\tobject_id\tmapping_justification
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching
";
    let f = tmp("struct_blank.sssom.tsv", bad);
    let (_o, err, rc) = run(
        &["sssom", "validate", "--validation-types", "structure", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 1);
    assert!(err.contains("blank line"), "{err}");
}

#[test]
fn conformance_structure_rejects_bom() {
    let bad = format!("\u{feff}{CLEAN}");
    let f = tmp("struct_bom.sssom.tsv", &bad);
    let (_o, err, rc) = run(
        &["sssom", "validate", "--validation-types", "structure", f.to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 1);
    assert!(err.contains("BOM"), "{err}");
}

#[test]
fn convert_canonical_prunes_prefixes_rounds_and_sorts() {
    let f = tmp(
        "canon.sssom.tsv",
        "\
# curie_map:
#   HP: http://purl.obolibrary.org/obo/HP_
#   MP: http://purl.obolibrary.org/obo/MP_
#   UNUSED: http://example.org/u_
#   skos: http://www.w3.org/2004/02/skos/core#
# license: https://creativecommons.org/publicdomain/zero/1.0/
# mapping_set_id: https://example.org/c
subject_id\tpredicate_id\tobject_id\tmapping_justification\tconfidence
HP:0000020\tskos:exactMatch\tMP:0000020\tsemapv:LexicalMatching\t0.123456
HP:0000010\tskos:exactMatch\tMP:0000010\tsemapv:LexicalMatching\t0.9
",
    );
    let (out, err, rc) = run(&["sssom", "convert", "--canonical", f.to_str().unwrap()], None);
    assert_eq!(rc, 0, "{err}");
    // Unused and built-in prefixes are dropped from the curie_map.
    assert!(!out.contains("UNUSED"), "unused prefix dropped: {out}");
    assert!(!out.contains("#   skos:"), "built-in prefix dropped: {out}");
    assert!(out.contains("HP:"), "used prefix kept: {out}");
    // Floats rounded to <=3 decimals.
    assert!(out.contains("0.123") && !out.contains("0.123456"), "rounded: {out}");
    // Records sorted lexicographically (HP:0000010 before HP:0000020).
    let i10 = out.find("HP:0000010").unwrap();
    let i20 = out.find("HP:0000020").unwrap();
    assert!(i10 < i20, "rows sorted: {out}");
}

/// A rule written as `FILTER { action; action; }` applies its body.
///
/// The block form is how UBERON's bridge rulesets are written, and its body is a
/// list of bare ACTIONS — no `->` of their own. Reading the body as a ruleset
/// dropped every statement in it, leaving a rule that matched and did nothing:
/// the Xenopus life-stage bridge came out with none of its annotations or
/// equivalences. `%{subject_label}` names the mapping set's own column, and the
/// subject need not be in the ontology at all.
#[test]
fn inject_applies_a_rule_block() {
    let ont = tmp(
        "block.ofn",
        "Prefix(obo:=<http://purl.obolibrary.org/obo/>)\n\
         Ontology(<http://x.org/block>\n\
         Declaration(Class(obo:XAO_1000000))\n\
         Declaration(Class(obo:XAO_0000437))\n\
         SubClassOf(obo:XAO_0000437 obo:XAO_1000000)\n\
         )\n",
    );
    let map = tmp(
        "block.sssom.tsv",
        "#curie_map:\n\
         #  UBERON: http://purl.obolibrary.org/obo/UBERON_\n\
         #  XAO: http://purl.obolibrary.org/obo/XAO_\n\
         #  semapv: https://w3id.org/semapv/vocab/\n\
         subject_id\tsubject_label\tpredicate_id\tobject_id\tmapping_justification\n\
         UBERON:0000071\tdeath stage\tsemapv:crossSpeciesExactMatch\tXAO:0000437\tsemapv:UnspecifiedMatching\n",
    );
    let rules = tmp(
        "block.rules",
        "prefix BFO: <http://purl.obolibrary.org/obo/BFO_>\n\
         prefix NCBITaxon: <http://purl.obolibrary.org/obo/NCBITaxon_>\n\
         prefix UBERON: <http://purl.obolibrary.org/obo/UBERON_>\n\
         prefix XAO: <http://purl.obolibrary.org/obo/XAO_>\n\
         prefix IAO: <http://purl.obolibrary.org/obo/IAO_>\n\
         subject==UBERON:* predicate==semapv:crossSpeciesExactMatch is_a(%{object_id}, XAO:1000000) -> {\n\
         \x20   annotate(%{object_id}, IAO:0000589, \"%{subject_label} (xenopus)\");\n\
         \x20   create_axiom(\"%object_id EquivalentTo: %subject_id and (BFO:0000066 some NCBITaxon:8353)\");\n\
         }\n",
    );
    let out = tmp("block-out.ofn", "");
    let (_, err, code) = run(
        &[
            "sssom:inject",
            "-i",
            ont.to_str().unwrap(),
            "--sssom",
            map.to_str().unwrap(),
            "--ruleset",
            rules.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0, "inject failed: {err}");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.contains("\"death stage (xenopus)\""),
        "the block's annotate() did not run, or %{{subject_label}} did not resolve:\n{text}"
    );
    assert!(
        text.contains("EquivalentClasses"),
        "the block's create_axiom() did not run:\n{text}"
    );
}
