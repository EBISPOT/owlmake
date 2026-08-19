//! Cross-format parity harness over the multi-syntax test corpus.
//!
//! The corpus under `tests/corpus/{owl-functional,owl-xml,owl-rdf,owl-ttl}`
//! contains the same ontologies serialized in each syntax. Every syntax must
//! parse into the same logical ontology, and must round-trip through itself
//! without loss, or a build that converts between formats silently drops axioms.
//! This harness measures both and prints a parity report.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use owlmake::diff;
use owlmake::io::{self, Format};
use owlmake::model::Model;

const CORPUS: &str = "tests/corpus";

/// Every base ontology name in the corpus (driven by the functional-syntax dir).
fn base_names() -> Vec<String> {
    let dir = Path::new(CORPUS).join("owl-functional");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let p = e.path();
            if p.extension().map(|x| x == "ofn").unwrap_or(false) {
                p.file_stem().map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

fn path_for(name: &str, dir: &str, ext: &str) -> PathBuf {
    Path::new(CORPUS).join(dir).join(format!("{name}.{ext}"))
}

fn try_load(path: &Path) -> Result<Model, String> {
    // Each load here stands for a separate run: blank-node ids are minted per
    // process, so a harness that reads a dozen documents in one would number the
    // twelfth from where the eleventh stopped and report two identical
    // serialisations as different ontologies.
    io::reset_anon_counter();
    io::load(path).map_err(|e| format!("{e:#}"))
}

/// Compare two models, returning an error string describing the divergence.
fn semantic_eq(a: &Model, b: &Model) -> Result<(), String> {
    let d = diff::diff(a, b);
    if d.is_empty() {
        Ok(())
    } else {
        let mut msg = String::new();
        for c in d.only_left.iter().take(4) {
            msg.push_str(&format!("\n      - only A: {}", diff::describe(c)));
        }
        for c in d.only_right.iter().take(4) {
            msg.push_str(&format!("\n      + only B: {}", diff::describe(c)));
        }
        Err(format!(
            "{} only-A, {} only-B{}",
            d.only_left.len(),
            d.only_right.len(),
            msg
        ))
    }
}

struct Report {
    total: usize,
    pass: usize,
    fails: Vec<String>,
}

impl Report {
    fn new() -> Self {
        Report {
            total: 0,
            pass: 0,
            fails: Vec::new(),
        }
    }
    fn record(&mut self, name: &str, result: Result<(), String>) {
        self.total += 1;
        match result {
            Ok(()) => self.pass += 1,
            Err(e) => self.fails.push(format!("  {name}: {e}")),
        }
    }
    fn print(&self, title: &str) {
        eprintln!(
            "\n=== {title}: {}/{} pass ({:.1}%) ===",
            self.pass,
            self.total,
            100.0 * self.pass as f64 / self.total.max(1) as f64
        );
        for f in self.fails.iter().take(40) {
            eprintln!("{f}");
        }
        if self.fails.len() > 40 {
            eprintln!("  ... and {} more", self.fails.len() - 40);
        }
    }
}

/// Round-trip stability: load a format, write it back, reload, compare.
fn roundtrip_stability(fmt: Format, dir: &str, ext: &str) -> Report {
    let mut rep = Report::new();
    for name in base_names() {
        let p = path_for(&name, dir, ext);
        let result = (|| {
            let m1 = try_load(&p)?;
            let mut buf = Vec::new();
            io::write_to_ref(&m1, &mut buf, fmt).map_err(|e| format!("write: {e:#}"))?;
            io::reset_anon_counter();
            let m2 = io::load_from(std::io::Cursor::new(buf), fmt)
                .map_err(|e| format!("reload: {e:#}"))?;
            semantic_eq(&m1, &m2)
        })();
        rep.record(&name, result);
    }
    rep
}

#[test]
fn functional_roundtrip_is_stable() {
    let rep = roundtrip_stability(Format::Functional, "owl-functional", "ofn");
    rep.print("Functional round-trip");
    // Functional syntax is the most complete path through `horned-owl`'s parse
    // and write stack, so this floor is the highest here; the slack covers the
    // constructs that path does not yet carry.
    assert!(
        rep.pass * 100 >= rep.total * 95,
        "functional round-trip parity regressed below 95%"
    );
}

#[test]
fn owlxml_roundtrip_is_stable() {
    let rep = roundtrip_stability(Format::OwlXml, "owl-xml", "owx");
    rep.print("OWL/XML round-trip");
    assert!(
        rep.pass * 100 >= rep.total * 90,
        "OWL/XML round-trip parity regressed below 90%"
    );
}

#[test]
fn rdfxml_roundtrip_is_stable() {
    let rep = roundtrip_stability(Format::RdfXml, "owl-rdf", "owl");
    rep.print("RDF/XML round-trip");
    assert!(
        rep.pass * 100 >= rep.total * 85,
        "RDF/XML round-trip parity regressed below 85%"
    );
}

/// Cross-format agreement: functional vs OWL/XML must parse to the same logic.
#[test]
fn functional_matches_owlxml() {
    let mut rep = Report::new();
    for name in base_names() {
        let result = (|| {
            let a = try_load(&path_for(&name, "owl-functional", "ofn"))?;
            let b = try_load(&path_for(&name, "owl-xml", "owx"))?;
            semantic_eq(&a, &b)
        })();
        rep.record(&name, result);
    }
    rep.print("Functional vs OWL/XML");
    assert!(
        rep.pass * 100 >= rep.total * 90,
        "Functional/OWL-XML cross-format parity regressed below 90%"
    );
}

/// Cross-format agreement: functional vs RDF/XML.
#[test]
fn functional_matches_rdfxml() {
    let mut rep = Report::new();
    let mut kinds_diverging: BTreeSet<String> = BTreeSet::new();
    for name in base_names() {
        let result = (|| {
            let a = try_load(&path_for(&name, "owl-functional", "ofn"))?;
            let b = try_load(&path_for(&name, "owl-rdf", "owl"))?;
            semantic_eq(&a, &b)
        })();
        if let Err(ref e) = result {
            for line in e.lines() {
                if let Some(idx) = line.find(": ") {
                    if line.contains("only") {
                        kinds_diverging.insert(line[..idx].trim().to_string());
                    }
                }
            }
        }
        rep.record(&name, result);
    }
    rep.print("Functional vs RDF/XML");
    // RDF/XML is the hardest path (triples -> axioms), so the floor is lower here
    // than for the other formats.
    assert!(
        rep.pass * 100 >= rep.total * 75,
        "Functional/RDF-XML cross-format parity regressed below 75%"
    );
}

// ---------------------------------------------------------------------------
// Undeclared entities in RDF/XML
// ---------------------------------------------------------------------------
//
// An OWL entity carries its type in RDF only as an `rdf:type` triple, and the
// only axiom that renders one is `Declaration`. But OWL does not *require* a
// declaration: an entity's kind is recoverable from the axiom positions it
// occupies, so CL's `cl-edit.owl` line
//
//   EquivalentClasses(obo:GO_0051932 ObjectIntersectionOf(obo:GO_0007268 …))
//
// with no `Declaration(Class(obo:GO_0051932))` anywhere in the file is legal and
// has to be read. Serializing such a subject as a bare
// `<rdf:Description rdf:about="…GO_0051932">` throws the type away, and the
// document is then unreadable ("Unknown entity in equivalent class statement") —
// which in a CL release means `tmp/simple_seed.txt` cannot be extracted from
// `tmp/cl-preprocess.owl`, so `cl-simple.owl`/`cl-basic.owl` are filtered
// against an empty seed while the build still reports success.

/// The IRIs CL's `cl-edit.owl` uses here, so the assertions read like the file.
const GO_NEUROTRANSMITTER_SECRETION: &str = "http://purl.obolibrary.org/obo/GO_0051932";
const GO_SYNAPTIC_SIGNALING: &str = "http://purl.obolibrary.org/obo/GO_0007268";
const RO_HAS_PARTICIPANT: &str = "http://purl.obolibrary.org/obo/RO_0000057";
const CHEBI_NEUROTRANSMITTER: &str = "http://purl.obolibrary.org/obo/CHEBI_59888";

/// `EquivalentClasses(GO_0051932 (GO_0007268 and RO_0000057 some CHEBI_59888))`
/// with not one `Declaration` in sight — the shape CL's `cl-edit.owl` has.
fn undeclared_equivalence() -> Model {
    use horned_owl::model::{
        Build, ClassExpression as CE, Component, EquivalentClasses, MutableOntology,
        ObjectPropertyExpression as OPE,
    };
    use horned_owl::ontology::set::SetOntology;

    let b = Build::new();
    let mut ont = SetOntology::new();
    ont.insert(Component::EquivalentClasses(EquivalentClasses(vec![
        CE::Class(b.class(GO_NEUROTRANSMITTER_SECRETION)),
        CE::ObjectIntersectionOf(vec![
            CE::Class(b.class(GO_SYNAPTIC_SIGNALING)),
            CE::ObjectSomeValuesFrom {
                ope: OPE::ObjectProperty(b.object_property(RO_HAS_PARTICIPANT)),
                bce: Box::new(CE::Class(b.class(CHEBI_NEUROTRANSMITTER))),
            },
        ]),
    ])));
    Model::from_parts(ont, owlmake::model::default_prefixes())
}

/// Writer: every entity in the signature must be given a type, declared or not —
/// otherwise the type is unrecoverable from the document and the file that comes
/// out cannot be parsed again.
#[test]
fn rdfxml_types_undeclared_entities() {
    let m = undeclared_equivalence();
    let mut buf = Vec::new();
    io::write_to_ref(&m, &mut buf, Format::RdfXml).unwrap();
    let text = String::from_utf8(buf.clone()).unwrap();

    assert!(
        !text.contains(&format!("<rdf:Description rdf:about=\"{GO_NEUROTRANSMITTER_SECRETION}\"")),
        "undeclared equivalence subject written as an untyped rdf:Description:\n{text}"
    );
    for iri in [GO_NEUROTRANSMITTER_SECRETION, GO_SYNAPTIC_SIGNALING, CHEBI_NEUROTRANSMITTER] {
        assert!(
            text.contains(&format!("<owl:Class rdf:about=\"{iri}\"")),
            "{iri} not typed as an owl:Class:\n{text}"
        );
    }
    assert!(
        text.contains(&format!("<owl:ObjectProperty rdf:about=\"{RO_HAS_PARTICIPANT}\"")),
        "{RO_HAS_PARTICIPANT} not typed as an owl:ObjectProperty:\n{text}"
    );

    // ... and the whole point: the output parses back. The re-read ontology gains
    // the four `Declaration`s the type triples encode — declarations carry no
    // logical content, so that is not a loss — but nothing may go missing, so
    // `only_left` has to be empty and every addition a declaration.
    let m2 = io::load_from(std::io::Cursor::new(buf), Format::RdfXml)
        .unwrap_or_else(|e| panic!("re-reading our own RDF/XML: {e:#}"));
    let d = diff::diff(&m, &m2);
    assert!(
        d.only_left.is_empty(),
        "axioms lost through RDF/XML: {:?}",
        d.only_left.iter().map(diff::describe).collect::<Vec<_>>()
    );
    assert!(
        d.only_right.iter().all(|c| matches!(
            c.component,
            horned_owl::model::Component::DeclareClass(_)
                | horned_owl::model::Component::DeclareObjectProperty(_)
        )),
        "unexpected non-declaration additions: {:?}",
        d.only_right.iter().map(diff::describe).collect::<Vec<_>>()
    );
}

/// Reader: a file that omits the type triple must still parse. Type the subject
/// of an `owl:equivalentClass` from its axiom position rather than rejecting the
/// file.
#[test]
fn rdfxml_reads_untyped_equivalent_class_subject() {
    // An untyped `owl:equivalentClass` block, as CL's GO_0051932 is written.
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:owl="http://www.w3.org/2002/07/owl#">
    <rdf:Description rdf:about="{GO_NEUROTRANSMITTER_SECRETION}">
        <owl:equivalentClass>
            <owl:Class>
                <owl:intersectionOf rdf:parseType="Collection">
                    <rdf:Description rdf:about="{GO_SYNAPTIC_SIGNALING}"/>
                    <owl:Restriction>
                        <owl:onProperty rdf:resource="{RO_HAS_PARTICIPANT}"/>
                        <owl:someValuesFrom rdf:resource="{CHEBI_NEUROTRANSMITTER}"/>
                    </owl:Restriction>
                </owl:intersectionOf>
            </owl:Class>
        </owl:equivalentClass>
    </rdf:Description>
</rdf:RDF>
"#
    );

    let m = io::load_from(std::io::Cursor::new(doc.into_bytes()), Format::RdfXml)
        .unwrap_or_else(|e| panic!("reading an untyped equivalent-class subject: {e:#}"));
    let equivalences = m
        .ont
        .iter()
        .filter(|ac| {
            matches!(
                ac.component,
                horned_owl::model::Component::EquivalentClasses(_)
            )
        })
        .count();
    assert_eq!(equivalences, 1, "the EquivalentClasses axiom was not recovered");
}

/// An ontology with NO ontology IRI puts the OWL namespace in the default
/// position, and its OWL elements are then written unprefixed — `<Class>`, not
/// `<owl:Class>`. UBERON's fourteen `*-minimal.owl` subsets are all of them:
/// `$(SUBSETCMD)` has no `annotate` step, so none of them gets an IRI.
#[test]
fn an_ontology_without_an_iri_writes_owl_elements_unprefixed() {
    let dir = std::env::temp_dir().join(format!("om_noiri_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let obo = "http://purl.obolibrary.org/obo";
    let src = dir.join("noiri.ofn");
    std::fs::write(
        &src,
        format!(
            "Prefix(:=<{obo}/>)\n\
             Ontology(\n\
             Declaration(Class(<{obo}/UBERON_0000001>))\n\
             Declaration(ObjectProperty(<{obo}/BFO_0000050>))\n\
             SubClassOf(<{obo}/UBERON_0000001> ObjectSomeValuesFrom(<{obo}/BFO_0000050> \
             <{obo}/UBERON_0000001>))\n\
             AnnotationAssertion(Annotation(rdfs:comment \"x\") rdfs:label \
             <{obo}/UBERON_0000001> \"a\")\n\
             )\n"
        ),
    )
    .unwrap();
    let owl = dir.join("noiri.owl");
    assert!(std::process::Command::new(env!("CARGO_BIN_EXE_om"))
        .args(["convert", "-i"]).arg(&src).args(["-f", "owl", "-o"]).arg(&owl)
        .status().unwrap().success());
    let doc = std::fs::read_to_string(&owl).unwrap();

    assert!(doc.contains("<rdf:RDF xmlns=\"http://www.w3.org/2002/07/owl#\""),
        "OWL takes the default namespace when there is no ontology IRI:\n{}", &doc[..400.min(doc.len())]);
    assert!(doc.contains("xml:base=\"http://www.w3.org/2002/07/owl\""), "…and the xml:base with it");
    for tag in ["<Class ", "<Restriction>", "<Axiom>", "<onProperty ", "<annotatedSource "] {
        assert!(doc.contains(tag), "`{tag}` should be unprefixed:\n{doc}");
    }
    assert!(!doc.contains("<owl:"), "no element should carry the `owl:` prefix:\n{doc}");

    // …and it reads back, which is the half that would fail silently: the
    // unprefixed names mean nothing unless the default namespace is honoured.
    let back = dir.join("back.ofn");
    assert!(std::process::Command::new(env!("CARGO_BIN_EXE_om"))
        .args(["convert", "-i"]).arg(&owl).args(["-f", "ofn", "-o"]).arg(&back)
        .status().unwrap().success());
    let rt = std::fs::read_to_string(&back).unwrap();
    assert!(rt.contains("SubClassOf("), "the restriction survived the round trip:\n{rt}");
    assert!(rt.contains("ObjectSomeValuesFrom("), "…as an existential:\n{rt}");
    assert!(rt.contains("Annotation(rdfs:comment \"x\")"), "…and so did the axiom annotation:\n{rt}");

    let _ = std::fs::remove_dir_all(&dir);
}
