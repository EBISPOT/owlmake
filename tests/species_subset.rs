//! `create-species-subset` — for a target taxon and a set of root classes, compute
//! the classes valid in that taxon, optionally tag them with `oboInOwl:inSubset`,
//! and optionally remove the rest. These tests take the tag-only path:
//! `--no-remove` with `--write-tags-to`, so the tags are the observable result.

use std::io::Write;

use owlmake::cmd::create_species_subset::{self, Args};

/// A tiny taxon-constrained ontology in the shape CL's release uses:
///
/// - `cell` is the root.
/// - `human cell` is `in_taxon` human and `mouse cell` `in_taxon` mouse. The
///   disjointness is stated over the *restrictions* (`in_taxon some human` vs
///   `in_taxon some mouse`), which is how NCBITaxon's
///   `taxslim-disjoint-over-in-taxon` module expresses it — disjointness of the
///   taxa alone would not make a cell in both unsatisfiable, since nothing says
///   `in_taxon` is functional.
/// - `neuron` carries no constraint at all, so it is valid in both.
/// - `human neuron` sits below both `neuron` and `human cell`; it must follow
///   `human cell` out of the mouse subset.
/// - `GO:0005575` is outside the `cell` root and must never be tagged.
const ONT: &str = r#"Prefix(:=<http://purl.obolibrary.org/obo/>)
Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Prefix(obo:=<http://purl.obolibrary.org/obo/>)
Ontology(<http://example.org/tax.owl>
Declaration(Class(obo:CL_0000000))
Declaration(Class(obo:CL_0000001))
Declaration(Class(obo:CL_0000002))
Declaration(Class(obo:CL_0000003))
Declaration(Class(obo:CL_0000004))
Declaration(Class(obo:GO_0005575))
Declaration(Class(obo:NCBITaxon_9606))
Declaration(Class(obo:NCBITaxon_10090))
Declaration(ObjectProperty(obo:RO_0002162))
DisjointClasses(ObjectSomeValuesFrom(obo:RO_0002162 obo:NCBITaxon_9606) ObjectSomeValuesFrom(obo:RO_0002162 obo:NCBITaxon_10090))
SubClassOf(obo:CL_0000001 obo:CL_0000000)
SubClassOf(obo:CL_0000002 obo:CL_0000000)
SubClassOf(obo:CL_0000003 obo:CL_0000000)
SubClassOf(obo:CL_0000004 obo:CL_0000003)
SubClassOf(obo:CL_0000004 obo:CL_0000001)
SubClassOf(obo:CL_0000001 ObjectSomeValuesFrom(obo:RO_0002162 obo:NCBITaxon_9606))
SubClassOf(obo:CL_0000002 ObjectSomeValuesFrom(obo:RO_0002162 obo:NCBITaxon_10090))
)
"#;

fn tags_for(strategy: &str, taxon: &str) -> Vec<String> {
    // A unique, per-call directory. Both tests probe ("precise", 9606/10090)
    // concurrently in the same process, so keying on pid+strategy+taxon alone
    // would hand two threads the same path, and one's `remove_dir_all` would blow
    // away the other's `tags.ofn` mid-run. The atomic nonce keeps them disjoint.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "owlmake_sss_{}_{}_{}_{}",
        std::process::id(),
        seq,
        strategy,
        taxon.replace(':', "_")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.ofn");
    std::fs::File::create(&input).unwrap().write_all(ONT.as_bytes()).unwrap();
    let tags = dir.join("tags.ofn");

    create_species_subset::run(Args {
        input: Some(input),
        output: None,
        format: None,
        taxon: taxon.to_string(),
        reasoner: None,
        strategy: strategy.to_string(),
        root: vec!["CL:0000000".into()],
        subset_name: Some("http://purl.obolibrary.org/obo/cl#tagged".into()),
        only_tag_in: vec!["CL:".into()],
        write_tags_to: Some(tags.clone()),
        no_remove: true,
        common: Default::default(),
    })
    .unwrap();

    let text = std::fs::read_to_string(&tags).unwrap();
    let mut ids: Vec<String> = text
        .lines()
        .filter(|l| l.contains("inSubset"))
        .filter_map(|l| {
            // The subject may render as a full IRI (`…/obo/CL_0000001`) or a CURIE
            // (`obo:CL_0000001`); match on the shared `CL_` + 7 digits.
            let i = l.find("CL_")?;
            Some(format!("CL:{}", &l[i + 3..i + 10]))
        })
        .collect();
    ids.sort();
    ids.dedup();
    let _ = std::fs::remove_dir_all(&dir);
    ids
}

/// The `precise` strategy tests `C and in_taxon some TAXON` for satisfiability.
/// Every candidate is probed in ONE classification rather than one classification
/// per class, which on CL would be ~3,600 passes over a 19,000-class model; the
/// batched answers must equal the per-class ones.
///
/// The root is included by fiat rather than satisfiability-tested, and
/// `--only-tag-in CL:` keeps the out-of-namespace `GO:0005575` untagged.
#[test]
fn precise_strategy_respects_taxon_constraints() {
    assert_eq!(
        tags_for("precise", "NCBITaxon:9606"),
        vec!["CL:0000000", "CL:0000001", "CL:0000003", "CL:0000004"],
        "human: the mouse-only class and nothing else is excluded"
    );
    assert_eq!(
        tags_for("precise", "NCBITaxon:10090"),
        vec!["CL:0000000", "CL:0000002", "CL:0000003"],
        "mouse: the human-only class AND its subclass are excluded"
    );
}

/// The `default` strategy asserts `root ⊑ in_taxon some TAXON` and treats
/// whatever becomes unsatisfiable as excluded. On an ontology with no
/// cross-taxon homology relations it must agree with `precise`.
#[test]
fn default_strategy_agrees_with_precise_here() {
    for taxon in ["NCBITaxon:9606", "NCBITaxon:10090"] {
        assert_eq!(
            tags_for("default", taxon),
            tags_for("precise", taxon),
            "strategies must agree for {taxon} when no cross-taxon relations are present"
        );
    }
}

/// `odk:subset` builds a NEW ontology, so the result carries none of the input's
/// ontology header — no IRI, no version IRI, no ontology annotations. UBERON's
/// fourteen `*-minimal.owl` otherwise claimed to BE `…/obo/uberon.owl`.
///
/// It is not cosmetic: the ontology IRI decides the document's default namespace,
/// so with one present every OWL element is written `<owl:Class>` where the
/// reference — having no IRI — writes `<Class>`.
#[test]
fn odk_subset_starts_a_new_ontology_header() {
    let dir = std::env::temp_dir().join(format!("om_subhdr_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let obo = "http://purl.obolibrary.org/obo";
    let src = dir.join("in.ofn");
    std::fs::write(
        &src,
        format!(
            "Prefix(:=<{obo}/>)\n\
             Ontology(<{obo}/uberon.owl> <{obo}/uberon/releases/1/uberon.owl>\n\
             Annotation(rdfs:comment \"set-level\")\n\
             Declaration(Class(<{obo}/UBERON_0000001>))\n\
             Declaration(Class(<{obo}/UBERON_0000002>))\n\
             SubClassOf(<{obo}/UBERON_0000002> <{obo}/UBERON_0000001>)\n\
             )\n"
        ),
    )
    .unwrap();
    let out = dir.join("out.owl");
    assert!(std::process::Command::new(env!("CARGO_BIN_EXE_om"))
        .args(["odk:subset", "-i"]).arg(&src)
        .args(["--query", &format!("{obo}/UBERON_0000001"), "-o"]).arg(&out)
        .status().unwrap().success());
    let doc = std::fs::read_to_string(&out).unwrap();

    assert!(!doc.contains("uberon.owl\""), "no ontology IRI is carried over:\n{doc}");
    assert!(!doc.contains("versionIRI"), "nor the version IRI:\n{doc}");
    assert!(!doc.contains("set-level"), "nor the ontology annotations:\n{doc}");
    // …and with no IRI, OWL takes the default namespace and elements go unprefixed.
    assert!(doc.contains("<rdf:RDF xmlns=\"http://www.w3.org/2002/07/owl#\""),
        "OWL takes the default namespace:\n{}", &doc[..300.min(doc.len())]);
    assert!(!doc.contains("<owl:"), "so no element carries the `owl:` prefix:\n{doc}");
    // The selection itself still happened.
    assert!(doc.contains("UBERON_0000002"), "the subset kept its classes:\n{doc}");

    let _ = std::fs::remove_dir_all(&dir);
}
