//! End-to-end CLI tests driving the built `owlmake` binary.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_om"))
}

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("owlmake_cli_{}_{name}", std::process::id()));
    p
}

/// `--select` entity selectors: `remove --select "<pat>" --select classes` drops
/// only matching classes; `filter --select "parents object-properties"` keeps the
/// seed's parents and all object properties.
#[test]
fn select_entity_selectors() {
    let inp = tmp("sel.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(<http://x.org/A>))\n\
         Declaration(Class(<http://x.org/B>))\n\
         Declaration(Class(<http://x.org/BFO_1>))\n\
         Declaration(ObjectProperty(<http://x.org/r>))\n\
         SubClassOf(<http://x.org/A> <http://x.org/B>)\n\
         SubClassOf(<http://x.org/A> ObjectSomeValuesFrom(<http://x.org/r> <http://x.org/B>))\n\
         )\n",
    )
    .unwrap();

    // remove the BFO_* classes only.
    let ro = tmp("sel-r.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--select", "<http://x.org/BFO_*>", "--select", "classes", "-o"]).arg(&ro)
        .status().unwrap().success());
    let r = std::fs::read_to_string(&ro).unwrap();
    assert!(!r.contains("BFO_1"), "BFO class should be removed:\n{r}");
    // `saveOntology` overwrites the input's `:` with the OUTPUT format's, and the
    // functional renderer then binds it to ontologyIRI + `#` — so `http://x.org/A`
    // no longer abbreviates. Measured on `robot convert -i named.ofn --format ofn`.
    assert!(r.contains("Prefix(:=<http://x.org/o#>)"), "default prefix is the ontology IRI:\n{r}");
    assert!(r.contains("Declaration(Class(<http://x.org/A>))"), "A should survive:\n{r}");

    // filter A keeping its parents and all object properties.
    let fo = tmp("sel-f.ofn");
    assert!(bin().args(["filter", "-i"]).arg(&inp)
        .args(["--term", "http://x.org/A", "--select", "self parents object-properties", "--signature", "true", "-o"]).arg(&fo)
        .status().unwrap().success());
    let f = std::fs::read_to_string(&fo).unwrap();
    // `filter` builds a new ontology, whose document format carries no prefixes —
    // so the OFN it writes declares only the standard owl/rdfs/rdf/xsd/xml bindings
    // plus the default `:`, and every other IRI is written out in full.
    assert!(
        f.contains("SubClassOf(<http://x.org/A> <http://x.org/B>)"),
        "parent B not kept:\n{f}"
    );
    assert!(
        f.contains("Declaration(ObjectProperty(<http://x.org/r>))"),
        "object property not kept:\n{f}"
    );

    for p in [&inp, &ro, &fo] {
        let _ = std::fs::remove_file(p);
    }
}

/// `normalize` injects subset / synonym-type subproperty declarations for the
/// in-namespace properties an ontology uses.
#[test]
fn normalize_injects_subproperty_declarations() {
    let inp = tmp("norm.ofn");
    let out = tmp("norm-out.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://purl.obolibrary.org/obo/X_>)\n\
         Prefix(oio:=<http://www.geneontology.org/formats/oboInOwl#>)\n\
         Ontology(<http://purl.obolibrary.org/obo/x.owl>\n\
         Declaration(Class(<http://purl.obolibrary.org/obo/X_1>))\n\
         AnnotationAssertion(oio:inSubset <http://purl.obolibrary.org/obo/X_1> <http://purl.obolibrary.org/obo/x#myslim>)\n\
         AnnotationAssertion(Annotation(oio:hasSynonymType <http://purl.obolibrary.org/obo/x#ABBREV>) oio:hasExactSynonym <http://purl.obolibrary.org/obo/X_1> \"X1\")\n\
         )\n",
    )
    .unwrap();
    let st = bin()
        .args(["normalize", "-i"])
        .arg(&inp)
        .args(["--base-iri", "http://purl.obolibrary.org/obo", "--subset-decls", "true", "--synonym-decls", "true", "-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(st.success());
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.contains("SubAnnotationPropertyOf(<http://purl.obolibrary.org/obo/x#myslim>")
            && text.contains("SubsetProperty"),
        "missing subset declaration:\n{text}"
    );
    assert!(
        text.contains("SubAnnotationPropertyOf(<http://purl.obolibrary.org/obo/x#ABBREV>")
            && text.contains("SynonymTypeProperty"),
        "missing synonym-type declaration:\n{text}"
    );
    let _ = std::fs::remove_file(&inp);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn template_then_query_roundtrip() {
    let tmpl = tmp("t.tsv");
    std::fs::write(
        &tmpl,
        "Class\tLabel\tParent\nID\tLABEL\tSC %\nEX:1\talpha\tEX:2\nEX:2\tbeta\t\n",
    )
    .unwrap();

    let owl = tmp("t.ofn");
    let status = bin()
        .args(["template", "--template"])
        .arg(&tmpl)
        .arg("-o")
        .arg(&owl)
        .args(["--format", "ofn"])
        .status()
        .unwrap();
    assert!(status.success(), "template command failed");

    let text = std::fs::read_to_string(&owl).unwrap();
    assert!(text.contains("SubClassOf"), "expected a SubClassOf axiom:\n{text}");
    assert!(text.contains("alpha"), "expected the alpha label");

    // export it back to TSV and check the rows.
    let tsv = tmp("t_export.tsv");
    let status = bin()
        .args(["export", "-i"])
        .arg(&owl)
        .arg("-o")
        .arg(&tsv)
        .status()
        .unwrap();
    assert!(status.success());
    let exported = std::fs::read_to_string(&tsv).unwrap();
    assert!(exported.contains("alpha"));
    assert!(exported.lines().count() >= 3, "header + 2 classes");

    let _ = std::fs::remove_file(&tmpl);
    let _ = std::fs::remove_file(&owl);
    let _ = std::fs::remove_file(&tsv);
}

/// The `query --query <FILE> <OUTPUT>` form (two positional-style values, the form
/// existing invocations use when a build writes a term list) must write the result
/// table to OUTPUT. A lone `--query <FILE> -o <OUT>` (single value) must still
/// write to --output and NOT swallow the following `-o` flag — this pins both the
/// clap arity and the chain-splitter's ranged-flag handling.
#[test]
fn query_two_arg_form_and_single_form() {
    let ont = tmp("q.ofn");
    std::fs::write(
        &ont,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(:A))\n\
         AnnotationAssertion(<http://www.w3.org/2000/01/rdf-schema#label> :A \"alpha\")\n\
         )\n",
    )
    .unwrap();
    let rq = tmp("q.rq");
    std::fs::write(&rq, "SELECT ?s WHERE { ?s <http://www.w3.org/2000/01/rdf-schema#label> ?l }\n")
        .unwrap();

    // Two-arg form: `--query <FILE> <OUTPUT>`.
    let out = tmp("q_pair.csv");
    let status = bin()
        .args(["query", "-i"])
        .arg(&ont)
        .args(["-f", "csv", "--query"])
        .arg(&rq)
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "two-arg --query failed");
    let pair = std::fs::read_to_string(&out).unwrap();
    assert!(pair.contains("http://x.org/A"), "pair OUTPUT missing result:\n{pair}");

    // Single form: `--query <FILE> -o <OUT>` — `-o` must not be eaten as OUTPUT.
    let single = tmp("q_single.tsv");
    let status = bin()
        .args(["query", "-i"])
        .arg(&ont)
        .args(["-f", "tsv", "--query"])
        .arg(&rq)
        .arg("-o")
        .arg(&single)
        .status()
        .unwrap();
    assert!(status.success(), "single --query -o failed");
    let one = std::fs::read_to_string(&single).unwrap();
    assert!(one.contains("http://x.org/A"), "single --output missing result:\n{one}");

    let _ = std::fs::remove_file(&ont);
    let _ = std::fs::remove_file(&rq);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&single);
}

/// Exercises the richer template DSL: TYPE handling, Manchester cells that
/// reference entities by rdfs:label across rows, an axiom annotation (`>A`), a
/// property characteristic, and a SPLIT column.
#[test]
fn template_manchester_and_dsl() {
    let tmpl = tmp("dsl.tsv");
    // row1 = human headers, row2 = template strings, row3+ = data.
    std::fs::write(
        &tmpl,
        "Class\tLabel\tType\tParent\tDef\tSource\tChar\tSyn\n\
         ID\tLABEL\tTYPE\tSC\tA obo:IAO_0000115\t>A dc:source\tCHARACTERISTIC\tA oboInOwl:hasExactSynonym SPLIT=|\n\
         EX:partof\tpart of\tobject property\t\t\t\ttransitive\t\n\
         EX:ns\tnervous system\tclass\t\t\t\t\t\n\
         EX:neuron\tneuron\tclass\t'part of' some 'nervous system'\ta neuron\tPMID:1\t\tnerve cell|neurocyte\n",
    )
    .unwrap();

    let owl = tmp("dsl.ofn");
    let status = bin()
        .args(["template", "--template"])
        .arg(&tmpl)
        .arg("-o")
        .arg(&owl)
        .args(["--format", "ofn"])
        .status()
        .unwrap();
    assert!(status.success(), "template command failed");
    let text = std::fs::read_to_string(&owl).unwrap();

    // TYPE handling: part-of is an object property, not a class.
    // A template ID cell is one of the DOCUMENT's own ids, so an unbound prefix
    // still follows the OBO convention — unlike a command-line `--term`, which is
    // expanded only by a prefix something actually binds.
    assert!(
        text.contains("Declaration(ObjectProperty(<http://purl.obolibrary.org/obo/EX_partof>))"),
        "expected object-property declaration:\n{text}"
    );
    // Manchester cell with cross-row label references resolved to IRIs.
    assert!(
        text.contains("SubClassOf(<http://purl.obolibrary.org/obo/EX_neuron> ObjectSomeValuesFrom(<http://purl.obolibrary.org/obo/EX_partof> <http://purl.obolibrary.org/obo/EX_ns>))"),
        "expected resolved some-restriction:\n{text}"
    );
    // Property characteristic.
    assert!(
        text.contains("TransitiveObjectProperty(<http://purl.obolibrary.org/obo/EX_partof>)"),
        "expected transitive characteristic:\n{text}"
    );
    // Axiom annotation attached to the definition assertion.
    assert!(
        text.contains("Annotation(<http://purl.org/dc/terms/source> \"PMID:1\")"),
        "expected axiom annotation on definition:\n{text}"
    );
    // SPLIT produced two synonym assertions.
    assert!(text.contains("\"nerve cell\"") && text.contains("\"neurocyte\""), "{text}");

    // Every annotation property referenced in an annotation column (A/AT/AL/AI
    // and `>` axiom annotations) is declared unless it is a built-in. Here
    // IAO_0000115, oboInOwl:hasExactSynonym and dc:source are custom.
    assert!(
        text.contains("Declaration(AnnotationProperty(<http://purl.obolibrary.org/obo/IAO_0000115>))"),
        "expected definition property declared:\n{text}"
    );
    assert!(
        text.contains("Declaration(AnnotationProperty(<http://www.geneontology.org/formats/oboInOwl#hasExactSynonym>))"),
        "expected synonym property declared:\n{text}"
    );
    assert!(
        text.contains("Declaration(AnnotationProperty(<http://purl.org/dc/terms/source>))"),
        "expected axiom-annotation property declared:\n{text}"
    );
    // Built-in vocabulary used via LABEL (rdfs:label) is never re-declared.
    assert!(
        !text.contains("Declaration(AnnotationProperty(<http://www.w3.org/2000/01/rdf-schema#label>))"),
        "rdfs:label must not be declared (built-in):\n{text}"
    );

    let _ = std::fs::remove_file(&tmpl);
    let _ = std::fs::remove_file(&owl);
}

#[test]
fn explain_finds_justification() {
    // The canonical EL inference; explain must return a non-empty justification.
    let ont = tmp("endo.ofn");
    std::fs::write(
        &ont,
        "Prefix(:=<http://example.org/>)\n\
         Ontology(\n\
         SubClassOf(:Endocardium :Tissue)\n\
         SubClassOf(:Endocardium ObjectSomeValuesFrom(:part_of :HeartWall))\n\
         SubClassOf(:HeartWall ObjectSomeValuesFrom(:part_of :Heart))\n\
         SubObjectPropertyOf(ObjectPropertyChain(:part_of :part_of) :part_of)\n\
         SubClassOf(ObjectIntersectionOf(:Tissue ObjectSomeValuesFrom(:part_of :Heart)) :HeartTissue)\n\
         )\n",
    )
    .unwrap();

    let out = bin()
        .args(["explain", "-i"])
        .arg(&ont)
        .args([
            "--sub",
            "http://example.org/Endocardium",
            "--sup",
            "http://example.org/HeartTissue",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "explain failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Justification"));
    assert!(text.contains("HeartTissue"));
    let _ = std::fs::remove_file(&ont);
}

/// A small ontology exercised by the command-integration test below.
const PIPELINE_ONT: &str = "Prefix(:=<http://example.org/>)\n\
    Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
    Ontology(\n\
    Declaration(Class(:Animal))\n\
    Declaration(Class(:Mammal))\n\
    Declaration(Class(:Dog))\n\
    Declaration(Class(:Cat))\n\
    AnnotationAssertion(rdfs:label :Animal \"animal\")\n\
    AnnotationAssertion(rdfs:label :Mammal \"mammal\")\n\
    AnnotationAssertion(rdfs:label :Dog \"dog\")\n\
    SubClassOf(:Mammal :Animal)\n\
    SubClassOf(:Dog :Mammal)\n\
    SubClassOf(:Cat :Mammal)\n\
    SubClassOf(:Dog :Animal)\n\
    )\n";

#[test]
fn command_pipeline_smoke() {
    let ont = tmp("pipe.ofn");
    std::fs::write(&ont, PIPELINE_ONT).unwrap();
    let ont_s = ont.to_str().unwrap().to_string();
    let run = |args: &[&str]| {
        let out = bin().args(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };

    // reduce: Dog ⊑ Animal is redundant (via Mammal) and should be dropped.
    let reduced = tmp("reduced.ofn");
    let reduced_s = reduced.to_str().unwrap().to_string();
    let (ok, _) = run(&["reduce", "-i", &ont_s, "-o", &reduced_s, "--format", "ofn"]);
    assert!(ok);
    let red = std::fs::read_to_string(&reduced).unwrap();
    // A save replaces the input's default prefix with the OUTPUT format's, and an
    // anonymous ontology gives the output format none, so `:Dog` is written in
    // full.
    assert!(red.contains("SubClassOf(<http://example.org/Dog> <http://example.org/Mammal>)"), "{red}");
    assert!(
        !red.contains("<http://example.org/Dog> <http://example.org/Animal>"),
        "redundant Dog⊑Animal must be removed:\n{red}"
    );

    // measure: 4 classes.
    let (ok, out) = run(&["measure", "-i", &ont_s]);
    assert!(ok);
    assert!(out.contains("classes\t4"), "expected 4 classes:\n{out}");

    // validate-profile EL: clean.
    let (ok, _) = run(&["validate-profile", "-i", &ont_s, "--profile", "EL"]);
    assert!(ok, "EL profile should be clean");

    // query: count subclass edges.
    let (ok, out) = run(&[
        "query",
        "-i",
        &ont_s,
        "--query-string",
        "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> SELECT ?a ?b WHERE { ?a rdfs:subClassOf ?b }",
    ]);
    assert!(ok);
    assert!(out.lines().count() >= 4, "header + >=3 edges:\n{out}");

    // filter to one term keeps only its axioms.
    let filtered = tmp("filtered.ofn");
    let filtered_s = filtered.to_str().unwrap().to_string();
    let (ok, _) = run(&[
        "filter", "-i", &ont_s, "-o", &filtered_s, "--term", "http://example.org/Dog", "--format", "ofn",
    ]);
    assert!(ok);
    let filt = std::fs::read_to_string(&filtered).unwrap();
    // Full IRIs — `filter`'s output format has no prefixes (see above).
    assert!(filt.contains("http://example.org/Dog"));
    assert!(
        !filt.contains("http://example.org/Cat"),
        "Cat axioms should be filtered out:\n{filt}"
    );

    for p in [&ont, &reduced, &filtered] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn make_merges_components_patterns_and_imports() {
    // A self-contained repository: an edit ontology plus a config wiring in a
    // local component, a DOSDP pattern, and a dynamic import — all offline.
    //
    // Driven through `om make`, so it exercises the PLAN-driven path: the config is
    // resolved once at plan time and every build step reads the plan. What this
    // covers: a component class, a DOSDP-generated class and an imported label all
    // reach the release.
    let root = tmp("rel");
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("src/ontology");
    std::fs::create_dir_all(&dir).unwrap();
    // The pattern layout `plan_dosdp` enumerates at plan time.
    std::fs::create_dir_all(root.join("src/patterns/dosdp-patterns")).unwrap();
    std::fs::create_dir_all(root.join("src/patterns/data/default")).unwrap();
    std::fs::create_dir_all(root.join("src/sparql")).unwrap();
    let p = |n: &str| dir.join(n);

    std::fs::write(
        p("cl-edit.ofn"),
        r#"Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Ontology(<http://purl.obolibrary.org/obo/cl.owl>
  Import(<http://purl.obolibrary.org/obo/cl/imports/uberon_import.owl>)
  Declaration(Class(<http://purl.obolibrary.org/obo/CL_0000100>))
  AnnotationAssertion(rdfs:label <http://purl.obolibrary.org/obo/CL_0000100> "motor neuron")
  Declaration(Class(<http://purl.obolibrary.org/obo/UBERON_0000955>))
  SubClassOf(<http://purl.obolibrary.org/obo/CL_0000100> ObjectSomeValuesFrom(<http://purl.obolibrary.org/obo/BFO_0000050> <http://purl.obolibrary.org/obo/UBERON_0000955>))
)
"#,
    )
    .unwrap();

    std::fs::write(
        p("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n         <uri id=\"u1\" name=\"http://purl.obolibrary.org/obo/cl/imports/uberon_import.owl\" uri=\"imports/uberon_import.owl\"/>\n         </catalog>\n",
    )
    .unwrap();

    // A local component merged verbatim.
    std::fs::write(
        p("component.ofn"),
        r#"Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Ontology(
  Declaration(Class(<http://purl.obolibrary.org/obo/CL_0000200>))
  AnnotationAssertion(rdfs:label <http://purl.obolibrary.org/obo/CL_0000200> "interneuron")
  SubClassOf(<http://purl.obolibrary.org/obo/CL_0000200> <http://purl.obolibrary.org/obo/CL_0000100>)
)
"#,
    )
    .unwrap();

    // An import source — only the referenced term should be pulled in.
    std::fs::create_dir_all(dir.join("imports")).unwrap();
    std::fs::write(
        p("imports/uberon_import.owl"),
        r#"Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Ontology(
  Declaration(Class(<http://purl.obolibrary.org/obo/UBERON_0000955>))
  Declaration(Class(<http://purl.obolibrary.org/obo/UBERON_0001062>))
  SubClassOf(<http://purl.obolibrary.org/obo/UBERON_0000955> <http://purl.obolibrary.org/obo/UBERON_0001062>)
  AnnotationAssertion(rdfs:label <http://purl.obolibrary.org/obo/UBERON_0000955> "brain")
)
"#,
    )
    .unwrap();

    std::fs::write(
        root.join("src/patterns/dosdp-patterns/part_of_x.yaml"),
        r#"pattern_name: part_of_x
classes:
  cell: CL:0000000
relations:
  part_of: BFO:0000050
vars:
  part: "'thing'"
name:
  text: "%s cell"
  vars: [part]
equivalentTo:
  text: "'cell' and ('part_of' some %s)"
  vars: [part]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/patterns/data/default/part_of_x.tsv"),
        "defined_class\tpart\nCL:1000001\tUBERON:0000955\n",
    )
    .unwrap();

    // `src/sparql/terms.sparql`, which the DOSDP pipeline runs over the pattern
    // prototype to collect `tmp/pattern_owl_seed.txt`. A repo that uses patterns
    // has it, and a missing declared input is an error rather than a silent skip —
    // so a fixture that asks for `use_dosdps: true` has to ship it.
    std::fs::write(
        root.join("src/sparql/terms.sparql"),
        "SELECT DISTINCT ?term\n\
         WHERE {\n\
         \x20 { ?s1 ?p1 ?term . }\n\
         \x20 UNION\n\
         \x20 { ?term ?p2 ?o2 . }\n\
         \x20 FILTER(isIRI(?term))\n\
         }\n",
    )
    .unwrap();

    // The config in the shape real repositories ship it in (`import_group.products`,
    // `components.products`), so ingest is exercised on the keys it has to resolve
    // in the field.
    std::fs::write(
        p("cl-odk.yaml"),
        &format!(r#"id: cl
reasoner: ELK
export_formats:
  - owl
  - obo
  - json
use_dosdps: true
import_group:
  products:
    - id: uberon
      mirror_from: MIRROR_URL
components:
  products:
    - filename: component.ofn
"#)
        .replace("MIRROR_URL", &format!("file://{}", p("imports/uberon_import.owl").display())),
    )
    .unwrap();

    let out = root.join("out");
    let status = bin()
        .args(["make", "-C"])
        .arg(&dir)
        // The import module is not committed in this fixture, so ask for it to be
        // built — `om make` defaults to reusing committed modules.
        .args(["--imports", "fresh", "-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "plan-driven release failed");

    // The release this config yields: the conventional primary `<id>.owl` and
    // `<id>-base.owl`, plus one export per `export_formats` entry the repo
    // declared — the format set is the repo's, not a fixed product list.
    for f in ["cl.owl", "cl-base.owl", "cl.obo", "cl.json"] {
        assert!(out.join(f).exists(), "missing release artefact {f}");
    }

    // The three pipelines that must all reach the release product: a component
    // file, a DOSDP-generated class, and an imported label.
    let full = std::fs::read_to_string(out.join("cl.owl")).unwrap();
    assert!(full.contains("CL_0000200"), "component class not merged");
    assert!(full.contains("CL_1000001"), "DOSDP-generated class not merged");
    assert!(full.contains("brain"), "imported UBERON label not merged");

    // The OBO export carries the component term through the format conversion.
    let obo = std::fs::read_to_string(out.join("cl.obo")).unwrap();
    assert!(obo.contains("CL:0000200"), "cl.obo missing component term");

    let _ = std::fs::remove_dir_all(&root);
}

/// Command chaining: `merge … reason … reduce -o` threads one ontology in
/// memory and must produce exactly what running the three steps sequentially
/// (via temp files) produces.
#[test]
fn chain_equals_sequential() {
    let a = tmp("chain_a.ofn");
    let b = tmp("chain_b.ofn");
    std::fs::write(
        &a,
        "Prefix(:=<http://ex/>)\nOntology(\nDeclaration(Class(:Animal))\nDeclaration(Class(:Mammal))\nSubClassOf(:Mammal :Animal)\n)\n",
    )
    .unwrap();
    std::fs::write(
        &b,
        "Prefix(:=<http://ex/>)\nOntology(\nDeclaration(Class(:Dog))\nSubClassOf(:Dog :Mammal)\nSubClassOf(:Dog :Animal)\n)\n",
    )
    .unwrap();
    let (a_s, b_s) = (a.to_str().unwrap(), b.to_str().unwrap());

    // Outputs go in a directory of their own. A `.ofn` written INTO a directory
    // named `tmp` is an owlmake build cache and carries `#…` marker lines; the
    // system temp dir has exactly that name, so comparands written straight into
    // it are not comparable — the sequential run's intermediates would be caches
    // and the chained run's single output would not.
    let dir = tmp("chain_seq");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Chained, in one invocation.
    let chained = dir.join("chain_out.ofn");
    let ok = bin()
        .args(["merge", "-i", a_s, "-i", b_s, "reason", "--reasoner", "elk", "reduce", "-o"])
        .arg(&chained)
        .args(["--format", "ofn"])
        .status()
        .unwrap()
        .success();
    assert!(ok, "chained pipeline failed");

    // Sequential, three invocations through temp files.
    let m = dir.join("seq_merged.ofn");
    let r = dir.join("seq_reasoned.ofn");
    let seq = dir.join("seq_out.ofn");
    assert!(bin()
        .args(["merge", "-i", a_s, "-i", b_s, "-o"]).arg(&m).args(["--format", "ofn"])
        .status().unwrap().success());
    assert!(bin()
        .args(["reason", "--reasoner", "elk", "-i"]).arg(&m).arg("-o").arg(&r).args(["--format", "ofn"])
        .status().unwrap().success());
    assert!(bin()
        .args(["reduce", "-i"]).arg(&r).arg("-o").arg(&seq).args(["--format", "ofn"])
        .status().unwrap().success());

    let chained_txt = std::fs::read_to_string(&chained).unwrap();
    let seq_txt = std::fs::read_to_string(&seq).unwrap();
    assert_eq!(chained_txt, seq_txt, "chained output must equal sequential output");
    // And reduce really ran: the redundant Dog⊑Animal is gone.
    assert!(!chained_txt.contains("SubClassOf(:Dog :Animal)"), "redundant edge must be reduced:\n{chained_txt}");
}

/// A side-output command in the middle of a chain (here `measure`) must emit its
/// report yet pass the ontology through unchanged to the next step.
#[test]
fn side_output_command_midchain_passes_through() {
    let a = tmp("so_a.ofn");
    std::fs::write(
        &a,
        "Prefix(:=<http://ex/>)\nOntology(\nDeclaration(Class(:Animal))\nDeclaration(Class(:Mammal))\nSubClassOf(:Mammal :Animal)\n)\n",
    )
    .unwrap();
    let out = tmp("so_out.ofn");
    let res = bin()
        .args(["reason", "--reasoner", "elk", "-i"]).arg(&a)
        .arg("measure")
        .arg("reduce").arg("-o").arg(&out).args(["--format", "ofn"])
        .output()
        .unwrap();
    assert!(res.status.success(), "midchain side-output pipeline failed");
    // measure printed its metrics on stdout …
    let stdout = String::from_utf8_lossy(&res.stdout);
    assert!(stdout.contains("classes\t2"), "measure should report 2 classes:\n{stdout}");
    // … and the pipe continued: reduce wrote the final ontology.
    let txt = std::fs::read_to_string(&out).unwrap();
    // Anonymous ontology, so the output format is left with no default prefix at
    // all and the IRIs are written in full.
    assert!(
        txt.contains("SubClassOf(<http://ex/Mammal> <http://ex/Animal>)"),
        "ontology must pass through:\n{txt}"
    );
}

#[test]
fn convert_format_inference_and_help() {
    // --version works (binary is wired). clap prints "<bin> <version>", and the
    // binary is named `om`.
    let out = bin().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("om "));

    // Unknown format errors cleanly rather than panicking.
    let bad = bin()
        .args(["convert", "-i", "/nonexistent.owl", "-o", "/tmp/x.zzz"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}

/// A repo that ships only an edit ontology, with no build config at all, falls back
/// to the canonical stock release: `<id>.owl` = merge→reason→relax→reduce→annotate,
/// plus `<id>-base.owl` and the obo/json exports — built end to end.
#[test]
fn odk_edit_only_default_release() {
    let root = tmp("odk_edit_only");
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();

    // Only an edit file — no build config of any kind, no imports.
    std::fs::write(
        ont.join("foo-edit.ofn"),
        "Prefix(:=<http://purl.obolibrary.org/obo/foo#>)\n\
         Ontology(<http://purl.obolibrary.org/obo/foo.owl>\n\
         Declaration(Class(<http://purl.obolibrary.org/obo/FOO_1>))\n\
         Declaration(Class(<http://purl.obolibrary.org/obo/FOO_2>))\n\
         Declaration(Class(<http://purl.obolibrary.org/obo/FOO_3>))\n\
         SubClassOf(<http://purl.obolibrary.org/obo/FOO_1> <http://purl.obolibrary.org/obo/FOO_2>)\n\
         SubClassOf(<http://purl.obolibrary.org/obo/FOO_2> <http://purl.obolibrary.org/obo/FOO_3>)\n\
         )\n",
    )
    .unwrap();

    let outdir = root.join("out");
    let out = bin()
        .arg("make")
        .arg("-C")
        .arg(&root)
        .arg("-o")
        .arg(&outdir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "edit-only odk build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The stock release artefacts are produced.
    for f in ["foo.owl", "foo-base.owl", "foo.obo", "foo.json"] {
        assert!(outdir.join(f).exists(), "missing artefact {f}");
    }
    // The primary release is non-empty. `FOO_1 ⊑ FOO_3` is not asserted in the edit
    // file: the reason step infers it and reduce drops it again, so the hierarchy the
    // release carries is the asserted one.
    let owl = std::fs::read_to_string(outdir.join("foo.owl")).unwrap();
    assert!(owl.contains("FOO_1"), "primary release looks empty:\n{owl}");

    let _ = std::fs::remove_dir_all(&root);
}

/// Bare `owlmake` (no subcommand) run from inside a repo defaults to the `make`
/// builder on the current directory, auto-detecting the repo root up the tree
/// (so `owlmake --plan-only` from `src/ontology` writes `owlmake.yaml` at the
/// root), and accepts positional target names.
#[test]
fn bare_owlmake_defaults_to_make() {
    let root = tmp("bare_make_default");
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();

    std::fs::write(
        ont.join("foo-odk.yaml"),
        "id: foo\nreasoner: ELK\nrelease_artefacts:\n  - full\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("Makefile"),
        "VERSION = 2026-01-01\nONTBASE = http://example.org/foo\nROBOT = robot\n\n\
         foo-full.owl: foo-edit.ofn\n\trobot merge --input $< reason --reasoner ELK reduce -o $@\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("foo-edit.ofn"),
        "Prefix(:=<http://example.org/foo#>)\nOntology(<http://example.org/foo.owl>\n\
         Declaration(Class(:A))\nDeclaration(Class(:B))\nSubClassOf(:A :B)\n)\n",
    )
    .unwrap();

    // No subcommand at all, run from *inside* `src/ontology`: it must route to
    // `make`, walk up to the repo root, and write the plan THERE — not in
    // the ontology dir it was launched from.
    let out = bin()
        .current_dir(&ont)
        .arg("--plan-only")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bare `owlmake --plan-only` should default to make: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        root.join("owlmake.yaml").exists(),
        "bare invocation should have written owlmake.yaml at the repo root"
    );
    assert!(
        !ont.join("owlmake.yaml").exists(),
        "the plan should NOT be written in src/ontology"
    );

    // A positional target routes through too: `owlmake foo-full.owl` builds just
    // that target.
    let out = bin()
        .current_dir(&ont)
        .arg("foo-full.owl")
        .arg("--plan-only")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "positional target `owlmake foo-full.owl` should route to make: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("foo-full.owl"),
        "the plan should target foo-full.owl:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // An unknown target fails with a "no rule to make target" error, not an empty plan.
    let out = bin()
        .current_dir(&ont)
        .arg("nope.owl")
        .arg("--plan-only")
        .output()
        .unwrap();
    assert!(!out.status.success(), "unknown target should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no rule to make target") && err.contains("nope.owl"),
        "expected a make-style unknown-target error, got:\n{err}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `owlmake make` generates the plan from the repo's build config on first run —
/// YAML by default, JSON under `--plan-format json` — and thereafter CHECKS the
/// committed plan against that config instead of rewriting it.
///
/// A plan and the config it was generated from can only disagree because the
/// plan is stale, and a stale plan is invisible while the config is still there
/// to regenerate from, then becomes the entire build the moment the Makefile is
/// deleted. Silently overwriting it hides the same thing the other way round:
/// the plan in review would never be the plan that ran. So a disagreement fails
/// the build and `--regenerate` is the one way to write over it.
#[test]
fn odk_checks_the_committed_plan_against_the_build_config() {
    let root = tmp("odk_json_repo");
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap(); // marks the repo root

    std::fs::write(
        ont.join("foo-odk.yaml"),
        "id: foo\nreasoner: ELK\nrelease_artefacts:\n  - full\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("Makefile"),
        "VERSION = 2026-01-01\nONTBASE = http://example.org/foo\nROBOT = robot\n\n\
         foo-full.owl: foo-edit.ofn\n\trobot merge --input $< reason --reasoner ELK reduce -o $@\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("foo-edit.ofn"),
        "Prefix(:=<http://example.org/foo#>)\nOntology(<http://example.org/foo.owl>\n\
         Declaration(Class(:A))\nDeclaration(Class(:B))\nDeclaration(Class(:C))\n\
         SubClassOf(:A :B)\nSubClassOf(:B :C)\n)\n",
    )
    .unwrap();

    let plan = root.join("owlmake.yaml");
    let json = root.join("owlmake.json");

    // First run: generates owlmake.yaml at the repo root — and nothing else. The
    // schema is the same for every plan, so it is not littered per repo.
    let out = bin().arg("make").arg("-C").arg(&root).arg("--plan-only").output().unwrap();
    assert!(out.status.success(), "odk plan-only failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(plan.exists(), "owlmake.yaml was not generated");
    assert!(!json.exists(), "JSON should not be written unless asked for");
    assert!(!root.join("owlmake.schema.json").exists(), "schema should not be written per repo");
    let text = std::fs::read_to_string(&plan).unwrap();
    assert!(text.contains("op: reduce"), "plan should contain the mapped reduce step:\n{text}");

    // `--plan-format json` writes the JSON spelling instead.
    let out = bin()
        .arg("make").arg("-C").arg(&root).arg("--plan-only").arg("--plan-format").arg("json")
        .output().unwrap();
    assert!(out.status.success(), "json plan failed: {}", String::from_utf8_lossy(&out.stderr));
    let jtext = std::fs::read_to_string(&json).unwrap();
    assert!(jtext.contains("\"op\": \"reduce\""), "json plan should map reduce:\n{jtext}");
    let out = bin().arg("make").arg("-C").arg(&root).arg("--plan-only").output().unwrap();
    assert!(out.status.success(), "run with both present failed: {}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_file(&json).unwrap();

    // The canonical schema is available on demand and validates this plan.
    let out = bin().arg("schema").output().unwrap();
    assert!(out.status.success(), "schema command failed");
    let schema_text = String::from_utf8_lossy(&out.stdout);
    assert!(
        schema_text.contains("artefacts"),
        "schema output looks wrong:\n{schema_text}"
    );

    // Second run: the committed plan already says what the build config says, so
    // it is CHECKED and left exactly as it is. Rewriting it on every run would
    // mean the plan a reviewer reads is never the plan that ran.
    let before = std::fs::read(&plan).unwrap();
    let out = bin().arg("make").arg("-C").arg(&root).arg("--plan-only").output().unwrap();
    assert!(out.status.success(), "in-step plan should build: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("regenerated"),
        "an in-step plan must not be rewritten: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(before, std::fs::read(&plan).unwrap(), "the committed plan was rewritten");

    // A committed plan that describes a DIFFERENT build than the build config is
    // a hard error naming what moved. It is stale, and it is the whole build the
    // moment the Makefile is deleted — so it cannot be quietly overwritten or
    // quietly ignored.
    let good = String::from_utf8(before.clone()).unwrap();
    let doctored: String = good
        .lines()
        .map(|l| if l.starts_with("reasoner:") { "reasoner: whelk" } else { l })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(doctored, good, "fixture plan has no `reasoner:` line to doctor");
    std::fs::write(&plan, &doctored).unwrap();
    let out = bin().arg("make").arg("-C").arg(&root).arg("--plan-only").output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a plan describing another build must fail:\n{err}");
    assert!(
        err.contains("reasoner") && err.contains("--regenerate"),
        "the error must name the field that moved and how to fix it:\n{err}"
    );
    assert_eq!(
        doctored,
        std::fs::read_to_string(&plan).unwrap(),
        "a failing check must not rewrite the plan"
    );

    // A corrupt committed plan is fatal for the same reason: it cannot be read,
    // so it cannot be shown to agree with the build config.
    std::fs::write(&plan, "{ totally not a valid plan }").unwrap();
    let out = bin().arg("make").arg("-C").arg(&root).arg("--plan-only").output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a corrupt committed plan must be fatal:\n{err}");
    assert!(err.contains("--regenerate"), "the error must say how to repair it:\n{err}");

    // `--regenerate` is that repair, and the only thing that writes over a
    // committed plan.
    let out = bin()
        .arg("make").arg("-C").arg(&root).arg("--plan-only").arg("--regenerate")
        .output().unwrap();
    assert!(
        out.status.success(),
        "--regenerate must repair a corrupt plan: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&plan).unwrap();
    assert!(
        text.contains("op: reduce"),
        "--regenerate should have rewritten the plan with the mapped reduce step:\n{text}"
    );
    // And having repaired it, the ordinary run is clean again.
    let out = bin().arg("make").arg("-C").arg(&root).arg("--plan-only").output().unwrap();
    assert!(out.status.success(), "repaired plan should build: {}", String::from_utf8_lossy(&out.stderr));

    let _ = std::fs::remove_dir_all(&root);
}

/// owlmake runs a project's `python3` recipe and builds a *generated*
/// prerequisite on demand (uPheno-style): a repo's own script is repo content and
/// runs through its interpreter, an ordinary environment dependency. Skipped where
/// no `python3` interpreter is installed.
#[test]
fn odk_runs_python_generated_prerequisite() {
    let have_python = std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !have_python {
        eprintln!("python3 not found — skipping odk_runs_python_generated_prerequisite");
        return;
    }

    let root = tmp("odk_python_prereq");
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();

    // A Python script that emits a tiny OFN ontology with one class.
    std::fs::write(
        ont.join("gen.py"),
        "open('gen.ofn','w').write('Prefix(:=<http://example.org/foo#>)\\n\
Ontology(<http://example.org/foo.owl>\\n\
Declaration(Class(<http://purl.obolibrary.org/obo/GEN_1>))\\n)\\n')\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("foo-odk.yaml"),
        "id: foo\nreasoner: ELK\nrelease_artefacts:\n  - full\n",
    )
    .unwrap();
    // `foo-full.owl` depends on `gen.ofn`, which is built by the Python rule.
    std::fs::write(
        ont.join("Makefile"),
        "VERSION = 2026-01-01\nONTBASE = http://example.org/foo\nROBOT = robot\nSRC = foo-edit.ofn\n\n\
         gen.ofn:\n\tpython3 gen.py\n\n\
         foo-full.owl: gen.ofn\n\t$(ROBOT) convert --input $< -o $@\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("foo-edit.ofn"),
        "Prefix(:=<http://example.org/foo#>)\nOntology(<http://example.org/foo.owl>\n\
         Declaration(Class(:A))\n)\n",
    )
    .unwrap();

    let outdir = root.join("out");
    let out = bin().arg("make").arg("-C").arg(&root).arg("-o").arg(&outdir).output().unwrap();
    assert!(
        out.status.success(),
        "odk python build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let full = outdir.join("foo-full.owl");
    assert!(full.exists(), "missing foo-full.owl");
    let owl = std::fs::read_to_string(&full).unwrap();
    assert!(owl.contains("GEN_1"), "python-generated class missing from output:\n{owl}");

    let _ = std::fs::remove_dir_all(&root);
}

/// `owlmake seed` scaffolds a buildable `owlmake.yaml`, and a repo defined solely
/// by that committed plan — no other build file in the tree — builds straight from
/// it, without regenerating the plan (it is the source of truth, not an output).
#[test]
fn seed_then_spec_driven_build() {
    let root = tmp("seed_spec");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // Scaffold the plan.
    let out = bin().current_dir(&root).args(["seed", "--id", "foo"]).output().unwrap();
    assert!(out.status.success(), "seed failed: {}", String::from_utf8_lossy(&out.stderr));
    let plan = root.join("owlmake.yaml");
    assert!(plan.exists(), "seed did not write owlmake.yaml");
    let before = std::fs::read_to_string(&plan).unwrap();

    // Provide the edit ontology the seeded plan references.
    std::fs::write(
        root.join("foo-edit.obo"),
        "[Term]\nid: FOO:0000001\nname: a\n\n[Term]\nid: FOO:0000002\nname: b\nis_a: FOO:0000001\n",
    )
    .unwrap();

    // A spec-driven build reads the plan as input and does NOT regenerate it.
    let out = bin().current_dir(&root).output().unwrap();
    assert!(out.status.success(), "spec-driven build failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("building from committed"),
        "expected a spec-driven build: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(before, std::fs::read_to_string(&plan).unwrap(), "the plan must not be regenerated");

    // Committing BOTH spellings is fine while they agree, and a hard error the
    // moment they describe different builds — owlmake will not silently pick one.
    let json = root.join("owlmake.json");
    let out = bin()
        .current_dir(&root)
        .args(["seed", "--id", "foo", "-o", "owlmake.json", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success(), "json seed failed: {}", String::from_utf8_lossy(&out.stderr));
    let out = bin().current_dir(&root).output().unwrap();
    assert!(
        out.status.success(),
        "two identical plans should build: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let differing = std::fs::read_to_string(&json).unwrap().replace("\"foo\"", "\"bar\"");
    std::fs::write(&json, differing).unwrap();
    let out = bin().current_dir(&root).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("describe different builds"),
        "conflicting plans should be a hard error: {err}");
    std::fs::remove_file(&json).unwrap();

    // The stock artefacts are produced.
    for f in ["foo.owl", "foo-base.owl", "foo.obo", "foo.json"] {
        assert!(root.join(f).exists(), "missing artefact {f}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// The standard build targets are usable as top-level owlmake commands: the
/// curated ones are discoverable in `--help`; a target that only manages build
/// infrastructure gives a clear error; an unknown target reports "no rule to make
/// target"; and any other target the repo defines dispatches through its recipe.
#[test]
fn odk_targets_as_commands() {
    // Curated commands are listed at the top level.
    let help = bin().arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    for c in ["prepare-release", "refresh-imports", "all-imports"] {
        assert!(help.contains(c), "`{c}` missing from --help:\n{help}");
    }

    let root = tmp("odk_targets");
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(ont.join("foo-odk.yaml"), "id: foo\nreasoner: ELK\nrelease_artefacts:\n  - full\n").unwrap();
    std::fs::write(
        ont.join("Makefile"),
        "VERSION = 2026-01-01\nONTBASE = http://example.org/foo\nROBOT = robot\n\n\
         foo-full.owl: foo-edit.ofn\n\trobot merge --input $< reason --reasoner ELK reduce -o $@\n\n\
         greet:\n\t@echo hello-from-custom-target\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("foo-edit.ofn"),
        "Prefix(:=<http://example.org/foo#>)\nOntology(<http://example.org/foo.owl>\n\
         Declaration(Class(:A))\n)\n",
    )
    .unwrap();

    // `clean` runs from the repo's own recorded recipe; a repo that defines no
    // such rule gets the ordinary no-rule error, not a special case.
    let out = bin().current_dir(&ont).arg("clean").output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no rule to make target `clean`"),
        "a repo with no clean rule should say so: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Unknown target.
    let out = bin().current_dir(&ont).arg("bogus.owl").output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no rule to make target"),
        "unknown target error missing: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Any other target the repo defines dispatches by interpreting its recipe.
    let out = bin().current_dir(&ont).arg("greet").output().unwrap();
    assert!(out.status.success(), "custom target failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hello-from-custom-target"),
        "custom target recipe did not run:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `om ubergraph` attributes every term to its source ontology via merge-time
/// `rdfs:isDefinedBy`, taking the target from the ontology the term came from — so
/// a non-OBO IRI (EFO's, say) is attributed as well, not just terms whose ID shape
/// happens to yield an OBO PURL.
#[test]
fn ubergraph_isdefinedby_covers_non_obo() {
    let root = tmp("ug_isdefinedby");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let inp = root.join("efo.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://www.ebi.ac.uk/efo/>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://www.ebi.ac.uk/efo/efo.owl>\n\
           Declaration(Class(:EFO_0000001))\n\
           Declaration(Class(:EFO_0000002))\n\
           AnnotationAssertion(rdfs:label :EFO_0000001 \"experimental factor\")\n\
           SubClassOf(:EFO_0000002 :EFO_0000001)\n\
         )\n",
    )
    .unwrap();
    let out = root.join("out");
    let status = bin()
        .args(["ubergraph", "-i"])
        .arg(&inp)
        .args(["--offline", "-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "om ubergraph failed");

    let nq = std::fs::read_to_string(out.join("ubergraph.nq")).unwrap();
    // Both EFO terms attributed to the source ontology IRI (not an OBO PURL).
    for t in ["EFO_0000001", "EFO_0000002"] {
        assert!(
            nq.contains(&format!(
                "<http://www.ebi.ac.uk/efo/{t}> <http://www.w3.org/2000/01/rdf-schema#isDefinedBy> <http://www.ebi.ac.uk/efo/efo.owl>"
            )),
            "{t} not attributed to its source ontology via isDefinedBy:\n{nq}"
        );
    }
    // And no obolibrary PURL is minted from the ID's shape: the attribution target
    // is the source ontology's own IRI.
    assert!(
        !nq.contains("purl.obolibrary.org/obo/efo.owl"),
        "unexpected OBO-PURL isDefinedBy target leaked from the heuristic:\n{nq}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `ogrep` searches an ontology for a pattern: it finds the entity, and the
/// output carries every axiom that mentions it, including those belonging to
/// OTHER terms that refer to it. An OBO-style CURIE finds the underscore form of
/// the same ID, which is the papercut the command exists to remove.
#[test]
fn ogrep_finds_terms_and_their_referrers() {
    let inp = tmp("ogrep.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://purl.obolibrary.org/obo/>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(<http://purl.obolibrary.org/obo/EFO_0000001>))\n\
         Declaration(Class(<http://purl.obolibrary.org/obo/EFO_0000002>))\n\
         Declaration(Class(<http://purl.obolibrary.org/obo/EFO_0000003>))\n\
         AnnotationAssertion(rdfs:label <http://purl.obolibrary.org/obo/EFO_0000001> \"chromatin assay\")\n\
         SubClassOf(<http://purl.obolibrary.org/obo/EFO_0000002> <http://purl.obolibrary.org/obo/EFO_0000001>)\n\
         )\n",
    )
    .unwrap();

    // By OBO-style CURIE: the term, plus the child that refers to it. EFO_0000003
    // mentions neither and must not appear.
    let out = bin().args(["ogrep", "EFO:0000001", "-i"]).arg(&inp).args(["-f", "ofn"]).output().unwrap();
    assert!(out.status.success(), "ogrep failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("EFO_0000001"), "the matched term is missing:\n{text}");
    assert!(text.contains("EFO_0000002"), "the referring term is missing:\n{text}");
    assert!(!text.contains("EFO_0000003"), "an unrelated term leaked in:\n{text}");

    // By label.
    let out = bin().args(["ogrep", "chromatin", "-i"]).arg(&inp).args(["-f", "ofn"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("EFO_0000001"), "label search found nothing");

    // `--self-only` drops the referrers.
    let out = bin().args(["ogrep", "EFO:0000001", "--self-only", "-i"]).arg(&inp).args(["-f", "ofn"]).output().unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("EFO_0000002"), "--self-only kept a referrer");

    // No match is an error, not an empty ontology written as if it were an answer.
    let out = bin().args(["ogrep", "no-such-term-anywhere", "-i"]).arg(&inp).output().unwrap();
    assert!(!out.status.success(), "a pattern matching nothing should fail");
}

/// A rule whose recipe merges its own `$<` must produce exactly what `om merge -i`
/// on that file produces, and the resulting RDF/XML must be a fixed point.
#[test]
fn a_rule_merging_its_own_input_reads_it_once() {
    let root = tmp("merge_self");
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();

    // An import with anonymous restrictions exercises the blank-node accounting
    // around the edit file's own bare anonymous individuals.
    std::fs::write(
        ont.join("imp.owl"),
        r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#"
         xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Ontology rdf:about="http://example.org/imp.owl"/>
  <owl:ObjectProperty rdf:about="http://example.org/p"/>
  <owl:Class rdf:about="http://example.org/I1"/>
  <owl:Class rdf:about="http://example.org/I2">
    <rdfs:subClassOf>
      <owl:Restriction>
        <owl:onProperty rdf:resource="http://example.org/p"/>
        <owl:someValuesFrom rdf:resource="http://example.org/I1"/>
      </owl:Restriction>
    </rdfs:subClassOf>
  </owl:Class>
  <owl:Class rdf:about="http://example.org/I3">
    <rdfs:subClassOf>
      <owl:Restriction>
        <owl:onProperty rdf:resource="http://example.org/p"/>
        <owl:someValuesFrom rdf:resource="http://example.org/I2"/>
      </owl:Restriction>
    </rdfs:subClassOf>
  </owl:Class>
  <owl:Class rdf:about="http://example.org/I4">
    <rdfs:subClassOf>
      <owl:Restriction>
        <owl:onProperty rdf:resource="http://example.org/p"/>
        <owl:someValuesFrom rdf:resource="http://example.org/I3"/>
      </owl:Restriction>
    </rdfs:subClassOf>
  </owl:Class>
</rdf:RDF>
"#,
    )
    .unwrap();

    // Bare `<rdf:Description>` blocks: anonymous individuals with nothing to claim
    // them, which is the shape whose ORDER the counter decides.
    std::fs::write(
        ont.join("foo-edit.owl"),
        r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#"
         xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Ontology rdf:about="http://example.org/foo.owl">
    <owl:imports rdf:resource="http://example.org/imp.owl"/>
  </owl:Ontology>
  <owl:AnnotationProperty rdf:about="http://example.org/note"/>
  <owl:Class rdf:about="http://example.org/FOO_1"/>
  <owl:Class rdf:about="http://example.org/FOO_2"/>
    <rdf:Description>
        <ex:note xmlns:ex="http://example.org/">alpha</ex:note>
    </rdf:Description>
    <rdf:Description>
        <ex:note xmlns:ex="http://example.org/">beta</ex:note>
    </rdf:Description>
    <rdf:Description>
        <ex:note xmlns:ex="http://example.org/">gamma</ex:note>
    </rdf:Description>
    <rdf:Description>
        <ex:note xmlns:ex="http://example.org/">delta</ex:note>
    </rdf:Description>
</rdf:RDF>
"#,
    )
    .unwrap();

    std::fs::write(
        ont.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         <uri name=\"http://example.org/imp.owl\" uri=\"imp.owl\"/>\n\
         </catalog>\n",
    )
    .unwrap();

    std::fs::write(
        ont.join("Makefile"),
        "ONT = foo\n\
         SRC = foo-edit.owl\n\
         ROBOT = robot\n\
         \n\
         all: release\n\
         .PHONY: all release\n\
         \n\
         release: foo.owl\n\
         \n\
         foo.owl: $(SRC)\n\
         \t$(ROBOT) merge -i $< -o $@\n",
    )
    .unwrap();

    let outdir = root.join("out");
    let out = bin().arg("make").arg("-C").arg(&root).arg("-o").arg(&outdir).output().unwrap();
    assert!(out.status.success(), "build failed: {}", String::from_utf8_lossy(&out.stderr));

    let direct = root.join("direct.owl");
    let out = bin()
        .arg("merge")
        .arg("-i")
        .arg(ont.join("foo-edit.owl"))
        .arg("--catalog")
        .arg(ont.join("catalog-v001.xml"))
        .arg("-o")
        .arg(&direct)
        .output()
        .unwrap();
    assert!(out.status.success(), "om merge failed: {}", String::from_utf8_lossy(&out.stderr));

    let order = |text: &str| -> Vec<String> {
        text.split("<rdf:Description>")
            .skip(1)
            .map(|b| b[..b.find("</rdf:Description>").unwrap()].trim().to_string())
            .collect()
    };
    let built = order(&std::fs::read_to_string(outdir.join("foo.owl")).unwrap());
    let expected = order(&std::fs::read_to_string(&direct).unwrap());
    assert_eq!(built.len(), 4, "the anonymous individuals did not survive the build");
    assert_eq!(
        built, expected,
        "`merge -i $<` in a rule ordered the anonymous individuals differently from \
         `om merge -i` on the same file"
    );
    // The emitted order must be a fixed point. ROBOT hashes the transient blank-node
    // ids assigned during each parse, so converting its own output applies the same
    // permutation again and can cycle forever. owlmake promises deterministic bytes;
    // a document it has canonicalised once must therefore stay byte-identical when
    // it is read and written again.
    let direct_second = root.join("direct-second.owl");
    let out = bin()
        .arg("convert")
        .arg("-i")
        .arg(&direct)
        .arg("-o")
        .arg(&direct_second)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "second conversion failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&direct).unwrap(),
        std::fs::read(&direct_second).unwrap(),
        "RDF/XML output changed when owlmake converted its own output"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--exclude-term` subtracts from the REMOVAL SET; it does not make every axiom
/// that mentions the term immune. UBERON's `merged-partonomy.owl` is
/// `remove --exclude-term BFO:0000050 --select object-properties` — keep part_of,
/// drop the rest — and under an immunity reading the RBox axioms tying each
/// dropped property to the kept one survived, so the writer re-declared 82 object
/// properties where the reference has 1.
#[test]
fn exclude_term_shrinks_the_removal_set_it_does_not_shield_axioms() {
    let inp = tmp("excl.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(<http://x.org/A>))\n\
         Declaration(Class(<http://x.org/B>))\n\
         Declaration(ObjectProperty(<http://x.org/keep>))\n\
         Declaration(ObjectProperty(<http://x.org/dSub>))\n\
         Declaration(ObjectProperty(<http://x.org/dInv>))\n\
         Declaration(ObjectProperty(<http://x.org/dChain>))\n\
         SubObjectPropertyOf(<http://x.org/dSub> <http://x.org/keep>)\n\
         InverseObjectProperties(<http://x.org/dInv> <http://x.org/keep>)\n\
         SubObjectPropertyOf(ObjectPropertyChain(<http://x.org/dChain> <http://x.org/keep>) \
         <http://x.org/keep>)\n\
         SubClassOf(<http://x.org/A> ObjectSomeValuesFrom(<http://x.org/keep> <http://x.org/B>))\n\
         )\n",
    )
    .unwrap();
    let out = tmp("excl-o.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--exclude-term", "http://x.org/keep", "--select", "object-properties", "-o"])
        .arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();

    // The excluded property, and the axiom that uses only it, survive.
    assert!(r.contains("Declaration(ObjectProperty(<http://x.org/keep>"), "kept property:\n{r}");
    assert!(r.contains("ObjectSomeValuesFrom(<http://x.org/keep>"), "its restriction:\n{r}");
    // Every other property goes, and so does each axiom naming one — even though
    // all three also name the excluded property.
    for p in ["dSub", "dInv", "dChain"] {
        assert!(!r.contains(&format!("http://x.org/{p}")), "`{p}` survived exclusion:\n{r}");
    }
}

/// A `--header` cell may carry a per-column entity format in brackets, which
/// overrides `--entity-format` for that column and stays in the emitted header.
/// UBERON's seven subset `.tsv` exports are `--header "ID [IRI]|LABEL"`; unparsed,
/// the cell matched no column and every row's first field came out empty.
#[test]
fn export_header_cell_carries_its_own_entity_format() {
    let inp = tmp("exp.ofn");
    // An OBO IRI, so the default (compressed) rendering and the bracketed IRI
    // rendering actually differ — `UBERON:0000001` against the full IRI.
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(<http://purl.obolibrary.org/obo/UBERON_0000001>))\n\
         AnnotationAssertion(rdfs:label <http://purl.obolibrary.org/obo/UBERON_0000001> \
         \"a label\")\n\
         )\n",
    )
    .unwrap();
    let out = tmp("exp.tsv");
    assert!(bin().args(["export", "-i"]).arg(&inp)
        .args(["--header", "ID [IRI]|LABEL", "--format", "tsv", "--export"])
        .arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(r.starts_with("ID [IRI]\tLABEL\n"), "header is emitted verbatim:\n{r}");
    assert!(r.contains("http://purl.obolibrary.org/obo/UBERON_0000001\ta label"),
        "ID renders as a full IRI:\n{r}");

    // Without the suffix the same column is a CURIE, so the bracket is doing the work.
    let out2 = tmp("exp2.tsv");
    assert!(bin().args(["export", "-i"]).arg(&inp)
        .args(["--header", "ID|LABEL", "--format", "tsv", "--export"])
        .arg(&out2).status().unwrap().success());
    assert!(
        std::fs::read_to_string(&out2).unwrap().contains("UBERON:0000001\t"),
        "fixture is inert: the default format does not differ from the bracketed one"
    );
}

/// `--select 'PROP=VALUE'` selects the entities carrying that annotation. UBERON
/// builds its `cumbo` subset by exporting exactly this selection; unhandled, the
/// token left an empty seed, which then selected the WHOLE ontology — 16,417 term
/// IDs instead of 14.
#[test]
fn select_by_annotation_value() {
    let inp = tmp("annsel.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(<http://x.org/A>))\n\
         Declaration(Class(<http://x.org/B>))\n\
         Declaration(Class(<http://x.org/C>))\n\
         AnnotationAssertion(<http://www.geneontology.org/formats/oboInOwl#inSubset> \
         <http://x.org/A> <http://x.org/core#mine>)\n\
         AnnotationAssertion(<http://www.geneontology.org/formats/oboInOwl#inSubset> \
         <http://x.org/B> <http://x.org/core#other>)\n\
         )\n",
    )
    .unwrap();
    let out = tmp("annsel.txt");
    assert!(bin().args(["filter", "-i"]).arg(&inp)
        .args(["--prefix", "u: http://x.org/core#",
               "--select", "oboInOwl:inSubset=u:mine",
               "export", "--header", "ID", "--export"])
        .arg(&out).status().unwrap().success());
    let ids: Vec<&str> = std::fs::read_to_string(&out).unwrap()
        .lines().skip(1).filter(|l| !l.trim().is_empty()).map(|l| l.trim()).collect::<Vec<_>>()
        .iter().map(|s| Box::leak(s.to_string().into_boxed_str()) as &str).collect();
    assert!(ids.iter().any(|i| i.contains('A')), "the tagged class is selected: {ids:?}");
    assert!(!ids.iter().any(|i| i.contains('B') || i.contains('C')),
        "an untagged class must not be selected — an unhandled selector selects everything: {ids:?}");
}

/// The same selector with a LITERAL value, spelled the way a recipe spells it —
/// quoted and datatyped. UBERON's `composite-vertebrate-basic.owl` is
/// `remove --select owl:deprecated='true'^^xsd:boolean`, so the quotes and the
/// `^^` suffix have to come off before the lexical form is compared.
#[test]
fn select_by_annotation_value_matches_a_quoted_typed_literal() {
    let inp = tmp("annlit.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(<http://x.org/Dead>))\n\
         Declaration(Class(<http://x.org/Live>))\n\
         AnnotationAssertion(owl:deprecated <http://x.org/Dead> \"true\"^^xsd:boolean)\n\
         )\n",
    )
    .unwrap();
    let out = tmp("annlit-o.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--select", "owl:deprecated='true'^^xsd:boolean", "-o"])
        .arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    // A term removal takes the entity AND what is said about it. (This assertion
    // was briefly weakened to "the declaration goes, the annotations stay", on a
    // misreading of the `-basic` composites; see `term_match_with`.)
    assert!(!r.contains("http://x.org/Dead"), "the deprecated class is removed:\n{r}");
    assert!(r.contains("http://x.org/Live"), "the live class is kept:\n{r}");
}

/// The composite pipeline rewrites assertions onto merged classes, and each
/// rewritten assertion must keep the annotations ON it — a definition's or
/// synonym's `oboInOwl:hasDbXref` provenance. Both rewriters dropped them,
/// keeping the text: `composite-metazoan.owl` came out with 94,660 reified
/// axioms against a reference 178,986, while the assertion counts looked right.
#[test]
fn species_and_equivalent_set_merges_keep_axiom_annotations() {
    let obo = "http://purl.obolibrary.org/obo";
    let oio = "http://www.geneontology.org/formats/oboInOwl#";

    // merge-equivalent-sets: the winning definition is re-stated, not stripped.
    let eq = tmp("axann-eq.ofn");
    std::fs::write(
        &eq,
        format!(
            "Prefix(:=<{obo}/>)\n\
             Ontology(<http://x.org/e>\n\
             Declaration(Class(<{obo}/UBERON_0000001>))\n\
             Declaration(Class(<{obo}/CL_0000001>))\n\
             EquivalentClasses(<{obo}/UBERON_0000001> <{obo}/CL_0000001>)\n\
             AnnotationAssertion(Annotation(<{oio}hasDbXref> \"SRC:2\") \
             <{obo}/IAO_0000115> <{obo}/UBERON_0000001> \"u definition\")\n\
             AnnotationAssertion(Annotation(<{oio}hasDbXref> \"SYN:1\") \
             <{oio}hasExactSynonym> <{obo}/CL_0000001> \"a synonym\")\n\
             )\n"
        ),
    )
    .unwrap();
    let eqo = tmp("axann-eq-o.ofn");
    assert!(bin().args(["uberon:merge-equivalent-sets", "-i"]).arg(&eq)
        .args(["-s", "UBERON=10", "-s", "CL=9", "-l", "UBERON=10", "-l", "CL=9",
               "-d", "UBERON=10", "-d", "CL=9", "-o"])
        .arg(&eqo).status().unwrap().success());
    let r = std::fs::read_to_string(&eqo).unwrap();
    assert!(r.contains("\"u definition\""), "the winning definition survives:\n{r}");
    assert!(r.contains("\"SRC:2\""), "the winning definition kept its provenance:\n{r}");
    // The renamed synonym keeps its own annotation too.
    assert!(r.contains("\"SYN:1\""), "a rewritten synonym kept its provenance:\n{r}");

    // merge-species: a translated assertion keeps its annotations.
    let ms = tmp("axann-ms.ofn");
    std::fs::write(
        &ms,
        format!(
            "Prefix(:=<{obo}/>)\n\
             Ontology(<http://x.org/m>\n\
             Declaration(Class(<{obo}/UBERON_0000001>))\n\
             Declaration(Class(<{obo}/FBbt_00000001>))\n\
             Declaration(Class(<{obo}/NCBITaxon_7227>))\n\
             Declaration(ObjectProperty(<{obo}/RO_0002162>))\n\
             SubClassOf(<{obo}/FBbt_00000001> ObjectSomeValuesFrom(<{obo}/RO_0002162> \
             <{obo}/NCBITaxon_7227>))\n\
             SubClassOf(<{obo}/FBbt_00000001> <{obo}/UBERON_0000001>)\n\
             AnnotationAssertion(Annotation(<{oio}hasDbXref> \"SYN:9\") \
             <{oio}hasExactSynonym> <{obo}/FBbt_00000001> \"fly synonym\")\n\
             )\n"
        ),
    )
    .unwrap();
    let batch = tmp("axann-tax.tsv");
    std::fs::write(&batch, "NCBITaxon:7227\tD melanogaster\tRO:0002162\t\n").unwrap();
    let mso = tmp("axann-ms-o.ofn");
    assert!(bin().args(["uberon:merge-species", "-i"]).arg(&ms)
        .arg("--batch-file").arg(&batch)
        .args(["--remove-declarations", "--extended-translation", "--translate-gcas", "-o"])
        .arg(&mso).status().unwrap().success());
    let r = std::fs::read_to_string(&mso).unwrap();
    assert!(r.contains("\"fly synonym\""), "the synonym survives the merge:\n{r}");
    // On the value, not its rendering: the fixture binds no `oboInOwl:` prefix, so
    // the property may print as a full IRI. If the annotation were stripped,
    // "SYN:9" would not appear at all.
    assert!(r.contains("\"SYN:9\""), "the translated synonym kept its provenance:\n{r}");
}

/// `--trim` and `--signature` are orthogonal: trim decides ANY-vs-ALL, signature
/// decides what counts as the axiom's objects. `--trim false` must therefore
/// survive `--signature true`, and `--axioms annotation` has to honour it too.
///
/// UBERON's `-basic` composites keep their labels with exactly one step —
/// `remove --term rdfs:label --select complement --axioms annotation --trim false
/// --signature true`, i.e. "drop every annotation axiom EXCEPT the labels". Under
/// any-entity semantics the labels went with everything else:
/// `composite-metazoan-basic.owl` had 0 `rdfs:label` against 81,374, and its
/// `.obo` no `name:` line at all.
#[test]
fn trim_false_keeps_an_annotation_whose_property_is_excluded() {
    let inp = tmp("trimlbl.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/l>\n\
         Declaration(Class(<http://x.org/A>))\n\
         AnnotationAssertion(rdfs:label <http://x.org/A> \"a label\")\n\
         AnnotationAssertion(rdfs:comment <http://x.org/A> \"a comment\")\n\
         SubClassOf(<http://x.org/A> owl:Thing)\n\
         )\n",
    )
    .unwrap();
    let out = tmp("trimlbl-o.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--term", "rdfs:label", "--select", "complement", "--axioms", "annotation",
               "--trim", "false", "--signature", "true", "-o"])
        .arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(r.contains("\"a label\""), "the label is what the step exists to keep:\n{r}");
    assert!(!r.contains("\"a comment\""), "every other annotation goes:\n{r}");

    // Inertness: with the default `--trim true` the same command takes both, so
    // the flag is doing the work rather than the fixture being trivially safe.
    let out2 = tmp("trimlbl-o2.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--term", "rdfs:label", "--select", "complement", "--axioms", "annotation", "-o"])
        .arg(&out2).status().unwrap().success());
    let r2 = std::fs::read_to_string(&out2).unwrap();
    assert!(!r2.contains("\"a label\""), "under --trim true the label goes too:\n{r2}");
}

/// `filter --axioms <types>` selects axioms BY TYPE, and a declaration is a type
/// like any other — it survives only when the request names it. Retaining
/// declarations unconditionally does not dangle (the writer re-declares whatever
/// the signature holds) but it keeps every entity IN the signature, which changes
/// what later steps can reach.
///
/// UBERON's `-basic` composites turn on exactly that: after
/// `filter --axioms "subclass equivalent annotation"`, a class with annotations
/// and no logical axioms must appear only as an annotation SUBJECT — an IRI, not
/// an entity of any axiom's signature — so the later `remove --term rdfs:label
/// --select complement --axioms annotation --trim false` cannot reach its
/// definition. The reference keeps 1,512 such definitions, every one on a class
/// with no edge; holding the declarations took all 1,512.
#[test]
fn filter_by_axiom_type_does_not_hold_declarations() {
    let obo = "http://purl.obolibrary.org/obo";
    let inp = tmp("axdecl.ofn");
    std::fs::write(
        &inp,
        format!(
            "Prefix(:=<{obo}/>)\n\
             Ontology(<http://x.org/c>\n\
             Declaration(Class(<{obo}/UBERON_1>))\n\
             Declaration(Class(<{obo}/UBERON_9>))\n\
             SubClassOf(<{obo}/UBERON_1> <{obo}/UBERON_2>)\n\
             AnnotationAssertion(<{obo}/IAO_0000115> <{obo}/UBERON_9> \"annotation-only\")\n\
             AnnotationAssertion(rdfs:label <{obo}/UBERON_9> \"nine\")\n\
             )\n"
        ),
    )
    .unwrap();
    let filtered = tmp("axdecl-f.ofn");
    assert!(bin().args(["filter", "-i"]).arg(&inp)
        .args(["--axioms", "subclass equivalent annotation", "-o"]).arg(&filtered)
        .status().unwrap().success());

    // The end of the chain: keep only the labels among annotation axioms. The
    // definition on the annotation-only class survives because that class is not
    // in the signature — nothing but an annotation subject mentions it.
    let out = tmp("axdecl-o.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&filtered)
        .args(["--term", "rdfs:label", "--select", "complement", "--axioms", "annotation",
               "--trim", "false", "--signature", "true", "-o"]).arg(&out)
        .status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(r.contains("annotation-only"),
        "a definition on a class with no logical axioms survives:\n{r}");
    assert!(r.contains("\"nine\""), "and so does its label:\n{r}");
}

/// An annotation assertion's objects are its property, its subject and — when the
/// value is an IRI — that value. So removing an entity takes the assertions that
/// point AT it, not only the ones about it.
#[test]
fn removing_an_entity_takes_the_assertions_that_point_at_it() {
    let inp = tmp("annval.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/v>\n\
         Declaration(Class(<http://x.org/A>))\n\
         Declaration(NamedIndividual(<http://x.org/Who>))\n\
         Declaration(AnnotationProperty(<http://x.org/contributor>))\n\
         AnnotationAssertion(<http://x.org/contributor> <http://x.org/A> <http://x.org/Who>)\n\
         AnnotationAssertion(<http://x.org/contributor> <http://x.org/A> \"a literal\")\n\
         )\n",
    )
    .unwrap();
    let out = tmp("annval-o.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--term", "http://x.org/Who", "-o"]).arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(!r.contains("x.org/Who"), "the assertion pointing at Who goes with Who:\n{r}");
    // Inertness: the literal-valued assertion on the SAME subject and property
    // survives, so the value is what decided it — not the subject or the property.
    assert!(r.contains("\"a literal\""), "only the assertion naming Who is taken:\n{r}");
}

/// The all-branch (`--trim false`) removes an assertion only when EVERY object is
/// selected. An IRI that names no entity of the ontology is in no entity-derived
/// object set, so an assertion pointing at one is never wholly selected and stays.
/// This is what leaves a `-basic` composite holding the assertions whose value is
/// a dangling IRI, and dropping every literal-valued one.
#[test]
fn trim_false_spares_an_assertion_whose_value_names_no_entity() {
    let inp = tmp("dangle.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/d>\n\
         Declaration(Class(<http://x.org/A>))\n\
         Declaration(NamedIndividual(<http://x.org/Known>))\n\
         Declaration(AnnotationProperty(<http://x.org/contributor>))\n\
         AnnotationAssertion(rdfs:label <http://x.org/A> \"a label\")\n\
         AnnotationAssertion(<http://x.org/contributor> <http://x.org/A> <http://x.org/Known>)\n\
         AnnotationAssertion(<http://x.org/contributor> <http://x.org/A> <http://x.org/Dangling>)\n\
         AnnotationAssertion(<http://x.org/contributor> <http://x.org/A> \"a literal\")\n\
         )\n",
    )
    .unwrap();
    let out = tmp("dangle-o.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--term", "rdfs:label", "--select", "complement", "--axioms", "annotation",
               "--trim", "false", "--signature", "true", "-o"])
        .arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(r.contains("x.org/Dangling"), "a dangling value is not selected, so its assertion stays:\n{r}");
    // The DECLARATION of `Known` survives (only annotation axioms were selected),
    // so test the assertion itself rather than the bare IRI.
    assert!(
        !r.contains("<http://x.org/A> <http://x.org/Known>"),
        "a declared value IS selected, so its assertion goes:\n{r}"
    );
    assert!(!r.contains("\"a literal\""), "a literal contributes no object, so the rest are all selected:\n{r}");
}

/// A COMPLETE `filter` match (the default, `--trim true`) keeps an axiom's
/// annotations only when they lie in the seed themselves — and without
/// `--signature true` a LITERAL annotation value never does, so the annotation
/// here is stripped even though its property is selected. `--trim false` keeps
/// them. This is why a `-basic` composite carries no `owl:Axiom` block at all.
#[test]
fn a_complete_filter_match_strips_axiom_annotations() {
    let inp = tmp("axann.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/a>\n\
         Declaration(Class(<http://x.org/A>))\n\
         Declaration(Class(<http://x.org/B>))\n\
         Declaration(AnnotationProperty(<http://x.org/src>))\n\
         SubClassOf(Annotation(<http://x.org/src> \"PMID:1\") <http://x.org/A> <http://x.org/B>)\n\
         )\n",
    )
    .unwrap();
    let out = tmp("axann-o.ofn");
    assert!(bin().args(["filter", "-i"]).arg(&inp)
        .args(["--axioms", "subclass", "-o"]).arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(r.contains("SubClassOf"), "the logical content is kept:\n{r}");
    assert!(!r.contains("PMID:1"), "the complete match strips the axiom annotation:\n{r}");

    // Inertness: `--trim false` keeps the very same annotation, so the mode is
    // doing the work rather than the annotation being unreachable.
    let out2 = tmp("axann-o2.ofn");
    assert!(bin().args(["filter", "-i"]).arg(&inp)
        .args(["--axioms", "subclass", "--trim", "false", "-o"]).arg(&out2).status().unwrap().success());
    let r2 = std::fs::read_to_string(&out2).unwrap();
    assert!(r2.contains("PMID:1"), "a partial match keeps it:\n{r2}");
}

/// `--term` naming something the ontology does not contain leaves an EMPTY object
/// set, and an empty set selects nothing — so the command removes nothing. Only
/// the absence of `--term`/`--term-file` altogether means "the whole ontology".
/// Conflating the two turns a no-op step into one that strips every annotation.
#[test]
fn a_named_but_absent_term_selects_nothing_rather_than_everything() {
    let inp = tmp("absent.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/n>\n\
         Declaration(Class(<http://x.org/A>))\n\
         Declaration(AnnotationProperty(<http://x.org/contributor>))\n\
         AnnotationAssertion(<http://x.org/contributor> <http://x.org/A> \"kept\")\n\
         )\n",
    )
    .unwrap();
    // `rdfs:label` appears nowhere in this ontology, so the complement of it is empty.
    let out = tmp("absent-o.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--term", "rdfs:label", "--select", "complement", "--axioms", "annotation",
               "--trim", "false", "--signature", "true", "-o"])
        .arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(r.contains("\"kept\""), "a named term that is absent selects nothing:\n{r}");

    // Inertness: with NO --term at all the object set IS the whole ontology, and
    // the same annotation goes — so the two cases genuinely differ.
    let out2 = tmp("absent-o2.ofn");
    assert!(bin().args(["remove", "-i"]).arg(&inp)
        .args(["--axioms", "annotation", "-o"]).arg(&out2).status().unwrap().success());
    let r2 = std::fs::read_to_string(&out2).unwrap();
    assert!(!r2.contains("\"kept\""), "with no term named, the whole ontology is the object set:\n{r2}");
}

/// A complete `filter` match keeps a literal-valued axiom annotation only under
/// `--signature true` (tested separately below) — but an annotation assertion the
/// `annotations` selector re-adds comes back WHOLE regardless. An assertion that
/// merely passes the signature test, without `--signature true`, is kept stripped.
#[test]
fn the_annotations_selector_re_adds_assertions_with_their_annotations() {
    let x = "http://x.org";
    let xref = "http://www.geneontology.org/formats/oboInOwl#hasDbXref";
    let def = "http://purl.obolibrary.org/obo/IAO_0000115";
    let inp = tmp("backfill.ofn");
    std::fs::write(
        &inp,
        format!(
            "Prefix(:=<{x}/>)\n\
             Ontology(<{x}/bf>\n\
             Declaration(Class(<{x}/A>))\n\
             Declaration(Class(<{x}/B>))\n\
             Declaration(AnnotationProperty(<{xref}>))\n\
             Declaration(AnnotationProperty(<{def}>))\n\
             AnnotationAssertion(Annotation(<{xref}> \"PMID:9\") <{def}> <{x}/A> \"a definition\")\n\
             SubClassOf(Annotation(<{xref}> \"PMID:1\") <{x}/A> <{x}/B>)\n\
             )\n"
        ),
    )
    .unwrap();

    // Subject selected => the assertion is re-added whole, annotation and all.
    let out = tmp("backfill-o.ofn");
    assert!(bin().args(["filter", "-i"]).arg(&inp)
        .args(["--term", &format!("{x}/A"), "--term", &format!("{x}/B"),
               "--select", "annotations", "-o"])
        .arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(r.contains("a definition"), "the assertion is re-added:\n{r}");
    assert!(r.contains("PMID:9"), "…keeping its axiom annotation:\n{r}");

    // Inertness: with the SAME assertion reachable through the signature test
    // instead (its properties in the seed, no `annotations` selector), it is kept
    // but STRIPPED — so the selector is what preserves the annotation, not the
    // assertion merely surviving.
    let out2 = tmp("backfill-o2.ofn");
    assert!(bin().args(["filter", "-i"]).arg(&inp)
        .args(["--term", &format!("{x}/A"), "--term", &format!("{x}/B"),
               "--term", xref, "--term", def, "--axioms", "subclass annotation", "-o"])
        .arg(&out2).status().unwrap().success());
    let r2 = std::fs::read_to_string(&out2).unwrap();
    assert!(r2.contains("a definition"), "the assertion still passes the seed test:\n{r2}");
    assert!(!r2.contains("PMID:9"), "…but a complete match strips its annotation:\n{r2}");
}

/// Under `--signature true`, a complete `filter` match keeps its axiom annotations
/// WHOLE when every annotation property is in the seed — a literal value is
/// ignored. This is how `uberon-simple.owl` keeps 6,637 `oboInOwl:source`
/// reifications on `subClassOf`: the seed lists the property, the values are
/// literals, and the axioms' signatures lie in the seed. An annotation whose
/// property is NOT in the seed still strips — and one failing annotation strips
/// them all.
#[test]
fn signature_true_keeps_seed_annotations_on_logical_axioms() {
    let x = "http://x.org";
    let src = "http://www.geneontology.org/formats/oboInOwl#source";
    let inf = "http://www.geneontology.org/formats/oboInOwl#is_inferred";
    let inp = tmp("sigann.ofn");
    std::fs::write(
        &inp,
        format!(
            "Prefix(:=<{x}/>)\n\
             Ontology(<{x}/sg>\n\
             Declaration(Class(<{x}/A>))\n\
             Declaration(Class(<{x}/B>))\n\
             Declaration(Class(<{x}/C>))\n\
             Declaration(AnnotationProperty(<{src}>))\n\
             Declaration(AnnotationProperty(<{inf}>))\n\
             SubClassOf(Annotation(<{src}> \"ZFA\") <{x}/A> <{x}/B>)\n\
             SubClassOf(Annotation(<{inf}> \"true\") <{x}/B> <{x}/C>)\n\
             )\n"
        ),
    )
    .unwrap();
    // Seed: the classes plus `source`, but NOT `is_inferred`.
    let out = tmp("sigann-o.ofn");
    assert!(bin().args(["filter", "-i"]).arg(&inp)
        .args(["--term", &format!("{x}/A"), "--term", &format!("{x}/B"),
               "--term", &format!("{x}/C"), "--term", src,
               "--select", "annotations ontology anonymous self",
               "--trim", "true", "--signature", "true", "-o"])
        .arg(&out).status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();
    assert!(r.contains("ZFA"), "property in seed, literal value ⇒ kept whole:\n{r}");
    assert!(!r.contains("\"true\""), "property NOT in seed ⇒ kept stripped:\n{r}");
    assert!(
        r.contains(&format!("SubClassOf(<{x}/B> <{x}/C>)")),
        "…the stripped axiom's logical content survives:\n{r}"
    );
}

/// `--tdb-directory` pointed at a directory that already holds a `dataset.rdf`
/// leaves that file alone.
///
/// owlmake deletes the dataset it materializes when the run ends, so writing over
/// one it did not create would destroy the caller's file and then remove the
/// replacement — `--keep-tdb-mappings` would preserve only the replacement, and
/// the original would be unrecoverable.
#[test]
fn tdb_directory_does_not_consume_a_dataset_it_did_not_create() {
    let dir = tmp("tdb-existing");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dataset = dir.join("dataset.rdf");
    std::fs::write(&dataset, "SENTINEL").unwrap();

    let inp = tmp("tdb.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(<http://x.org/A>))\n\
         )\n",
    )
    .unwrap();
    let q = tmp("tdb.sparql");
    std::fs::write(&q, "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1\n").unwrap();

    let out = bin()
        .args(["query", "--tdb", "true", "-i"])
        .arg(&inp)
        .arg("--tdb-directory")
        .arg(&dir)
        .arg("--query")
        .arg(&q)
        .arg(tmp("tdb-res.csv"))
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "materializing over an existing dataset must be refused, not silently done"
    );
    assert_eq!(
        std::fs::read_to_string(&dataset).unwrap(),
        "SENTINEL",
        "the caller's dataset.rdf was overwritten"
    );

    // …and an empty directory is still usable, with the dataset cleaned up after.
    let fresh = tmp("tdb-fresh");
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).unwrap();
    let ok = bin()
        .args(["query", "--tdb", "true", "-i"])
        .arg(&inp)
        .arg("--tdb-directory")
        .arg(&fresh)
        .arg("--query")
        .arg(&q)
        .arg(tmp("tdb-res2.csv"))
        .output()
        .unwrap();
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    assert!(!fresh.join("dataset.rdf").exists(), "the dataset it created was left behind");
    assert!(fresh.is_dir(), "a directory it did not create was removed");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&fresh);
}

/// Write a two-document import fixture: a root importing `other`, and a catalog
/// mapping the import IRI to the sibling file. Returns the root's path.
fn import_fixture(name: &str, with_catalog: bool) -> std::path::PathBuf {
    let dir = tmp(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("root.ofn"),
        "Prefix(:=<http://x.org/root#>)\n\
         Prefix(other:=<http://x.org/other#>)\n\
         Ontology(<http://x.org/root>\n\
         Import(<http://x.org/other>)\n\
         Declaration(Class(<http://x.org/root#A>))\n\
         Declaration(Class(<http://x.org/root#B>))\n\
         SubClassOf(<http://x.org/root#B> <http://x.org/root#A>)\n\
         )\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("other.ofn"),
        "Prefix(other:=<http://x.org/other#>)\n\
         Ontology(<http://x.org/other>\n\
         Declaration(Class(<http://x.org/other#IMPORTED>))\n\
         )\n",
    )
    .unwrap();
    if with_catalog {
        std::fs::write(
            dir.join("catalog-v001.xml"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
             <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
             <uri name=\"http://x.org/other\" uri=\"other.ofn\"/>\n\
             </catalog>\n",
        )
        .unwrap();
    }
    dir.join("root.ofn")
}

/// A command works over the whole import closure and writes the ROOT ontology:
/// its own axioms plus whatever it added, still importing.
///
/// The two halves go together. Keeping the imported axioms AND restoring the
/// import declaration would freeze one version of the import into a file that
/// also tells its consumer to load whatever the IRI resolves to later; dropping
/// the declaration would hand back a silently self-contained document in place of
/// the one that was asked for.
#[test]
fn a_processed_root_keeps_its_imports_and_not_their_axioms() {
    let root = import_fixture("imports-root", true);
    let out = tmp("imports-root-out.ofn");
    let st = bin()
        .args(["reason", "--reasoner", "structural", "-i"])
        .arg(&root)
        .args(["-o"])
        .arg(&out)
        .args(["-f", "ofn"])
        .output()
        .unwrap();
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    let text = std::fs::read_to_string(&out).unwrap();

    assert!(
        text.contains("Import(<http://x.org/other>)"),
        "the root's import declaration must survive:\n{text}"
    );
    assert!(
        !text.contains("Declaration(Class(other:IMPORTED))"),
        "an axiom that exists only in the import was written into the root:\n{text}"
    );
    assert!(
        text.contains("Declaration(Class(:A))") && text.contains("SubClassOf(:B :A)"),
        "the root's own axioms must survive:\n{text}"
    );
}

/// `merge` means the opposite, and says so: it collapses the closure into ONE
/// document, so the imported axioms are its own content and it imports nothing.
#[test]
fn merge_collapses_the_closure_it_was_given() {
    let root = import_fixture("imports-merge", true);
    let out = tmp("imports-merge-out.ofn");
    let st = bin()
        .args(["merge", "-i"])
        .arg(&root)
        .args(["-o"])
        .arg(&out)
        .args(["--format", "ofn"])
        .output()
        .unwrap();
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.contains("IMPORTED"),
        "a collapsed merge must carry the imported axioms:\n{text}"
    );
    assert!(
        !text.contains("Import("),
        "a collapsed merge must not still import what it inlined:\n{text}"
    );
}

/// A declared `owl:imports` that resolves to nothing is an error.
///
/// A command is handed the whole closure so that it works over the complete axiom
/// set. Carrying on without an import does all of that over part of it, and the
/// answer — an unsatisfiability never checked, a report with no violations —
/// cannot be told apart from the right one.
#[test]
fn an_unresolvable_import_is_an_error() {
    let root = import_fixture("imports-broken", false);
    std::fs::remove_file(root.parent().unwrap().join("other.ofn")).unwrap();
    let out = bin()
        .args(["reason", "--reasoner", "structural", "-i"])
        .arg(&root)
        .args(["-o", "/dev/null", "-f", "ofn"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "an unresolved import was accepted silently");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("http://x.org/other"),
        "the error must NAME the import it could not resolve:\n{err}"
    );
}

/// A file that is not a tagger DB is a reported error, not a panic.
///
/// The format carries no magic number, so anything at all parses as plausible
/// offsets and state ids. For a long-lived tagging service, an unchecked one
/// means a single bad DB ends the process.
#[test]
fn a_malformed_tagger_db_is_rejected_not_followed() {
    let db = tmp("bogus-tagger.bin");
    std::fs::write(&db, vec![0xABu8; 4096]).unwrap();
    let out = bin()
        .args(["text-tagger", "stream"])
        .arg(&db)
        .output()
        .unwrap();
    assert!(!out.status.success(), "a malformed DB was accepted");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked"), "the loader panicked instead of failing:\n{err}");
    assert!(
        err.contains("tagger DB"),
        "the error must say what it could not load:\n{err}"
    );

    // A truncated but otherwise real DB is caught the same way.
    let tsv = tmp("tagger-terms.tsv");
    std::fs::write(&tsv, "ontology_id\tlabel\tiri\nefo\tcarcinoma\thttp://x/EFO_1\n").unwrap();
    let good = tmp("tagger-good.bin");
    assert!(bin()
        .args(["text-tagger", "build", "-i"])
        .arg(&tsv)
        .args(["-o"])
        .arg(&good)
        .status()
        .unwrap()
        .success());
    let bytes = std::fs::read(&good).unwrap();
    let cut = tmp("tagger-cut.bin");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
    let out = bin().args(["text-tagger", "stream"]).arg(&cut).output().unwrap();
    assert!(!out.status.success(), "a truncated DB was accepted");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("panicked"),
        "a truncated DB panicked the loader"
    );

    // …and the whole DB still loads and tags.
    let mut tag = bin();
    tag.args(["text-tagger", "stream"]).arg(&good);
    tag.stdin(std::process::Stdio::piped());
    tag.stdout(std::process::Stdio::piped());
    let mut child = tag.spawn().unwrap();
    {
        use std::io::Write as _;
        child.stdin.as_mut().unwrap().write_all(b"a carcinoma here\n").unwrap();
    }
    let done = child.wait_with_output().unwrap();
    assert!(done.status.success());
    assert!(
        String::from_utf8_lossy(&done.stdout).contains("EFO_1"),
        "a valid DB must still tag: {}",
        String::from_utf8_lossy(&done.stdout)
    );
}

/// A rule's first prerequisite is a dependency edge; the pipeline opens with
/// whatever the recipe's first invocation names.
///
/// uPheno's mappings component is `components/upheno-mappings.owl: $(SRC)
/// …sssom.owl` and its recipe is `merge -i …sssom.owl -i …sssom.owl`. Opening
/// from `$(SRC)` there merges the edit ontology — and, through its
/// `owl:imports`, the component's own previous build — into its replacement.
#[test]
fn a_recipe_that_names_its_own_input_does_not_also_read_the_first_prerequisite() {
    let root = tmp("recipe_opens_pipeline");
    let _ = std::fs::remove_dir_all(&root);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(ont.join("components")).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();

    std::fs::write(
        ont.join("foo-odk.yaml"),
        "id: foo\nreasoner: ELK\nrelease_artefacts:\n  - full\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("Makefile"),
        "VERSION = 2026-01-01\nONTBASE = http://example.org/foo\nROBOT = robot\nSRC = foo-edit.ofn\n\n\
         components/merged.ofn: $(SRC) components/a.ofn components/b.ofn\n\
         \t$(ROBOT) merge -i components/a.ofn -i components/b.ofn -o $@\n",
    )
    .unwrap();
    let ont_of = |iri: &str, decl: &str| {
        format!(
            "Prefix(:=<http://example.org/foo#>)\nOntology(<{iri}>\nDeclaration(Class(:{decl}))\n)\n"
        )
    };
    std::fs::write(ont.join("foo-edit.ofn"), ont_of("http://example.org/foo.owl", "EDIT_ONLY"))
        .unwrap();
    std::fs::write(ont.join("components/a.ofn"), ont_of("http://example.org/a.owl", "A")).unwrap();
    std::fs::write(ont.join("components/b.ofn"), ont_of("http://example.org/b.owl", "B")).unwrap();

    let out = bin()
        .current_dir(&ont)
        .args(["make", "components/merged.ofn"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the component should build: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let merged = std::fs::read_to_string(ont.join("components/merged.ofn")).unwrap();
    assert!(merged.contains(":A)") || merged.contains("/foo#A>"), "merged should carry A:\n{merged}");
    assert!(merged.contains(":B)") || merged.contains("/foo#B>"), "merged should carry B:\n{merged}");
    assert!(
        !merged.contains("EDIT_ONLY"),
        "the recipe never reads $<, so the edit ontology must not be in the product:\n{merged}"
    );

    // …and the plan says so: the recorded input is the recipe's own first
    // `--input`, not the rule's first prerequisite.
    let plan = std::fs::read_to_string(root.join("owlmake.yaml")).unwrap();
    let entry = plan
        .split("- target: ")
        .find(|s| s.starts_with("src/ontology/components/merged.ofn\n"))
        .expect("the plan should carry the component");
    let input = entry
        .lines()
        .find_map(|l| l.trim().strip_prefix("input: "))
        .expect("the component should record an input");
    assert_eq!(input, "src/ontology/components/a.ofn", "plan entry:\n{entry}");
}

/// An `.ofn` document is byte-clean wherever it is written: the state a cache
/// carries about the document it stands in for lives in a companion beside it,
/// never in the document's own bytes.
///
/// The companion is written only for owlmake's own cache, which is named for the
/// target it stands in for — the cache for `x.owl` is `x.owl.ofn`. A `.ofn` a
/// REPO names as a target has no such second extension and gets no companion,
/// wherever it lives: uPheno's `$(SRCMERGED)` is `tmp/merged-upheno-edit.ofn`.
#[test]
fn an_ofn_cache_keeps_its_state_in_a_companion() {
    let dir = tmp("ofn_markers");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tmp")).unwrap();

    // RDF/XML in, so there is an xmlns block for a marker to carry.
    let src = dir.join("src.owl");
    std::fs::write(
        &src,
        "<?xml version=\"1.0\"?>\n\
         <rdf:RDF xmlns=\"http://www.w3.org/2002/07/owl#\"\n\
              xmlns:owl=\"http://www.w3.org/2002/07/owl#\"\n\
              xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"\n\
              xmlns:rdfs=\"http://www.w3.org/2000/01/rdf-schema#\"\n\
              xmlns:xsd=\"http://www.w3.org/2001/XMLSchema#\"\n\
              xmlns:ex=\"http://example.org/\">\n\
             <Ontology rdf:about=\"http://example.org/o\"/>\n\
             <Class rdf:about=\"http://example.org/A\"/>\n\
         </rdf:RDF>\n",
    )
    .unwrap();

    let convert = |out: &std::path::Path| {
        assert!(
            bin().args(["convert", "-i"]).arg(&src).args(["-f", "ofn", "-o"]).arg(out)
                .status().unwrap().success(),
            "convert to {} should succeed",
            out.display()
        );
        std::fs::read_to_string(out).unwrap()
    };

    // The repo's own target: its own content, and no companion at all.
    let target = dir.join("tmp/merged-src.ofn");
    let text = convert(&target);
    assert!(
        !text.starts_with('#'),
        "a repo-named .ofn target must start with its own content:\n{}",
        text.lines().next().unwrap_or("")
    );
    assert!(
        !dir.join("tmp/.omcache/merged-src.ofn.omcache").exists(),
        "a repo-named .ofn target is not a cache and gets no companion"
    );

    // owlmake's cache for `src.owl`: the document is just as clean, and the
    // source's xmlns is carried beside it, keyed to the bytes it describes.
    let cache = dir.join("tmp/src.owl.ofn");
    let text = convert(&cache);
    assert!(
        !text.starts_with('#'),
        "a cache document carries no markers of its own:\n{}",
        text.lines().next().unwrap_or("")
    );
    let companion = std::fs::read_to_string(dir.join("tmp/.omcache/src.owl.ofn.omcache"))
        .expect("the cache should have a companion");
    assert!(
        companion.contains("\n#rdfxmlns "),
        "the companion should carry the source's xmlns:\n{companion}"
    );
    assert!(
        companion.contains(&format!("#doc {}:", text.len())),
        "the companion should name the bytes it describes:\n{companion}"
    );
}

/// A `SELECT` with no `ORDER BY` still has an order: the one the graph answers the
/// pattern in. An arbitrary-length path drives it — the rows walk out from the
/// path's object — and a `FILTER (?p IN (…))` is answered one alternative at a
/// time, so the rows come out grouped by alternative in the order the list names
/// them. `NOT IN` enumerates nothing and leaves the order alone.
#[test]
fn a_path_and_an_in_list_fix_the_row_order() {
    let inp = tmp("alp.ofn");
    let mut o = String::from(
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(<http://x.org/ROOT>))\n\
         AnnotationAssertion(rdfs:label <http://x.org/ROOT> \"root\")\n",
    );
    // A chain, so a walk from the root has only one order it can produce.
    for i in 0..6 {
        let me = format!("http://x.org/C{i}");
        let parent = if i == 0 { "http://x.org/ROOT".to_string() } else { format!("http://x.org/C{}", i - 1) };
        o.push_str(&format!("Declaration(Class(<{me}>))\n"));
        o.push_str(&format!("SubClassOf(<{me}> <{parent}>)\n"));
        o.push_str(&format!("AnnotationAssertion(rdfs:label <{me}> \"l{i}\")\n"));
        o.push_str(&format!(
            "AnnotationAssertion(<http://www.geneontology.org/formats/oboInOwl#hasExactSynonym> <{me}> \"s{i}\")\n"
        ));
    }
    o.push_str(")\n");
    std::fs::write(&inp, o).unwrap();

    let q = tmp("alp.sparql");
    std::fs::write(
        &q,
        "prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n\
         prefix oio: <http://www.geneontology.org/formats/oboInOwl#>\n\
         SELECT ?s ?p ?l WHERE { ?s rdfs:subClassOf* <http://x.org/ROOT> . ?s ?p ?l .\n\
         FILTER ( ?p IN (rdfs:label, oio:hasExactSynonym)) }\n",
    )
    .unwrap();
    let out = tmp("alp.csv");
    assert!(bin().args(["query", "-f", "csv", "-i"]).arg(&inp).args(["--query"]).arg(&q).arg(&out)
        .status().unwrap().success());
    let csv = std::fs::read_to_string(&out).unwrap();
    let preds: Vec<&str> = csv.lines().skip(1).map(|l| l.split(',').nth(1).unwrap()).collect();
    let labels = preds.iter().filter(|p| p.ends_with("#label")).count();
    // Grouped by alternative, in the order the list names them: every label first.
    assert_eq!(labels, 7, "one label per class:\n{csv}");
    assert!(
        preds[..labels].iter().all(|p| p.ends_with("#label"))
            && preds[labels..].iter().all(|p| p.ends_with("hasExactSynonym")),
        "rows group by the IN list's order:\n{csv}"
    );
    // The walk starts at the path's object and descends the chain.
    let subs: Vec<&str> = csv.lines().skip(1).take(labels).map(|l| l.split(',').next().unwrap()).collect();
    assert_eq!(subs[0], "http://x.org/ROOT", "the walk starts at the object:\n{csv}");
    assert_eq!(subs[1], "http://x.org/C0", "then its own reachers:\n{csv}");

    // `NOT IN` names no alternatives to answer one after another.
    let qn = tmp("alp-not.sparql");
    std::fs::write(
        &qn,
        "prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n\
         prefix oio: <http://www.geneontology.org/formats/oboInOwl#>\n\
         SELECT ?s ?p ?l WHERE { ?s rdfs:subClassOf* <http://x.org/ROOT> . ?s ?p ?l .\n\
         FILTER ( ?p NOT IN (oio:hasExactSynonym)) }\n",
    )
    .unwrap();
    let outn = tmp("alp-not.csv");
    assert!(bin().args(["query", "-f", "csv", "-i"]).arg(&inp).args(["--query"]).arg(&qn).arg(&outn)
        .status().unwrap().success());
    let csvn = std::fs::read_to_string(&outn).unwrap();
    assert!(!csvn.contains("hasExactSynonym"), "NOT IN excludes:\n{csvn}");
}

/// The import-closure shape behind EBISPOT/owlmake#2, in two documents: a
/// Plant-Ontology-like module carrying the hierarchy, a genus-differentia
/// definition of `Perianth` and the transitive `part_of`, and a root that
/// imports it and adds EFO's cyclic `part_of` definition, the bridging
/// existentials, and one OBO-style id (`…/efo/EFO_0000998`) under a namespace
/// the root binds as `efo:` and no context binds as `EFO`. Returns the root's
/// path; the catalog sits beside it.
fn plant_import_fixture(name: &str) -> std::path::PathBuf {
    let dir = tmp(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("root.ofn"),
        "Prefix(:=<http://x.org/root#>)\n\
         Prefix(po:=<http://x.org/po#>)\n\
         Prefix(efo:=<http://x.org/efo/>)\n\
         Ontology(<http://x.org/root>\n\
         Import(<http://x.org/po>)\n\
         Declaration(Class(:ReproSystem))\n\
         Declaration(Class(:LeafComponent))\n\
         Declaration(Class(efo:EFO_0000998))\n\
         Declaration(ObjectProperty(po:part_of))\n\
         EquivalentClasses(:ReproSystem ObjectIntersectionOf(po:Structure ObjectSomeValuesFrom(po:part_of :ReproSystem)))\n\
         EquivalentClasses(:LeafComponent ObjectIntersectionOf(po:Structure ObjectSomeValuesFrom(po:part_of po:Leaf)))\n\
         SubClassOf(po:Flower ObjectSomeValuesFrom(po:part_of :ReproSystem))\n\
         SubClassOf(po:Stoma ObjectSomeValuesFrom(po:part_of po:Leaf))\n\
         SubClassOf(efo:EFO_0000998 po:Structure)\n\
         )\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("po.ofn"),
        "Prefix(po:=<http://x.org/po#>)\n\
         Ontology(<http://x.org/po>\n\
         Declaration(Class(po:Structure))\n\
         Declaration(Class(po:Organ))\n\
         Declaration(Class(po:Tissue))\n\
         Declaration(Class(po:Flower))\n\
         Declaration(Class(po:Perianth))\n\
         Declaration(Class(po:Tepal))\n\
         Declaration(Class(po:Leaf))\n\
         Declaration(Class(po:Stoma))\n\
         Declaration(ObjectProperty(po:part_of))\n\
         TransitiveObjectProperty(po:part_of)\n\
         SubClassOf(po:Tepal po:Perianth)\n\
         EquivalentClasses(po:Perianth ObjectIntersectionOf(po:Organ ObjectSomeValuesFrom(po:part_of po:Flower)))\n\
         SubClassOf(po:Perianth po:Organ)\n\
         SubClassOf(po:Organ po:Structure)\n\
         SubClassOf(po:Flower po:Structure)\n\
         SubClassOf(po:Stoma po:Tissue)\n\
         SubClassOf(po:Tissue po:Structure)\n\
         SubClassOf(po:Leaf po:Structure)\n\
         )\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         <uri name=\"http://x.org/po\" uri=\"po.ofn\"/>\n\
         </catalog>\n",
    )
    .unwrap();
    dir.join("root.ofn")
}

/// Does `text` state `SubClassOf(sub sup)`, whichever way the writer spelt the
/// two fixture namespaces (`po:X` / `:X` or the full IRI)?
fn plant_edge(text: &str, sub: &str, sup: &str) -> bool {
    let forms = |t: &str| -> Vec<String> {
        match t.split_once(':') {
            Some(("po", l)) => vec![format!("po:{l}"), format!("<http://x.org/po#{l}>")],
            Some(("", l)) => vec![format!(":{l}"), format!("<http://x.org/root#{l}>")],
            _ => vec![t.to_string()],
        }
    };
    forms(sub)
        .iter()
        .any(|s| forms(sup).iter().any(|p| text.contains(&format!("SubClassOf({s} {p})"))))
}

/// Both classifications the issue reported missing hold, and `explain` derives
/// them through the catalog under the EL engine and under hermit-rs — saying on
/// stderr which reasoner decided when it is not the EL engine.
#[test]
fn explain_derives_the_imported_plant_shape_under_elk_and_hermit() {
    let root = plant_import_fixture("plant-explain");
    let catalog = root.with_file_name("catalog-v001.xml");
    for reasoner in ["elk", "hermit"] {
        for (sub, sup) in [
            ("po:Tepal", "http://x.org/root#ReproSystem"),
            ("po:Stoma", "http://x.org/root#LeafComponent"),
        ] {
            let out = bin()
                .args(["explain", "-i"])
                .arg(&root)
                .arg("--catalog")
                .arg(&catalog)
                .args(["--sub", sub, "--sup", sup, "-r", reasoner])
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{reasoner} {sub} ⊑ {sup}: {err}");
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.contains("1 justification(s)"), "{reasoner} {sub}: {text}");
            if sub == "po:Tepal" {
                assert!(
                    text.contains("TransitiveObjectProperty"),
                    "the chain through the flower is part of the justification:\n{text}"
                );
            }
            assert_eq!(
                reasoner == "hermit",
                err.contains("decided by hermit-rs"),
                "{reasoner}: the deciding backend must be named exactly when it is not the EL engine:\n{err}"
            );
        }
    }
}

/// A query term that names no class is an error about the query, never a
/// verdict about the ontology. `EFO:0000998` against a document that binds
/// `efo:` — and an OBO context that binds no `EFO` — used to reach the reasoner
/// as an unknown IRI and come back "not entailed" (EBISPOT/owlmake#2).
#[test]
fn explain_rejects_a_term_that_names_no_class_instead_of_calling_it_unentailed() {
    let root = plant_import_fixture("plant-unbound");
    let catalog = root.with_file_name("catalog-v001.xml");
    let run = |sub: &str, sup: &str| {
        let out = bin()
            .args(["explain", "-i"])
            .arg(&root)
            .arg("--catalog")
            .arg(&catalog)
            .args(["--sub", sub, "--sup", sup])
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stderr).to_string())
    };

    // An unbound prefix, with the class it almost certainly meant named.
    let (ok, err) = run("po:Tepal", "EFO:0000998");
    assert!(!ok, "an unexpanded CURIE must fail:\n{err}");
    assert!(!err.contains("not entailed"), "not a verdict on the ontology:\n{err}");
    assert!(
        err.contains("`EFO:0000998` did not expand")
            && err.contains("<http://x.org/efo/EFO_0000998>")
            && err.contains("--prefix \"EFO: http://x.org/efo/EFO_\""),
        "{err}"
    );

    // An unbound prefix with nothing to suggest.
    let (ok, err) = run("ZZQ:Tepal", "http://x.org/root#ReproSystem");
    assert!(!ok && err.contains("prefix `ZZQ` is bound neither") && !err.contains("not entailed"), "{err}");

    // A bound prefix whose expansion the ontology never uses as a class.
    let (ok, err) = run("po:Petal", "http://x.org/root#ReproSystem");
    assert!(!ok && err.contains("<http://x.org/po#Petal> is not a class in the ontology"), "{err}");

    // The document's own spelling of the same term works.
    let (ok, err) = run("po:Tepal", "efo:EFO_0000998");
    assert!(!ok && err.contains("is not entailed"), "a real class that is not a superclass:\n{err}");
}

/// `explain` validates `--reasoner` as `reason` does: a misspelt backend is an
/// error, not a quiet EL run reporting a verdict the requested reasoner never gave.
#[test]
fn explain_rejects_an_unknown_reasoner() {
    let root = plant_import_fixture("plant-reasoner");
    let catalog = root.with_file_name("catalog-v001.xml");
    let out = bin()
        .args(["explain", "-i"])
        .arg(&root)
        .arg("--catalog")
        .arg(&catalog)
        .args(["--sub", "po:Tepal", "--sup", "http://x.org/root#ReproSystem", "-r", "hermitt"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("unknown reasoner 'hermitt'"), "{err}");
}

/// `--create-new-ontology` writes a NEW ontology of inferences. An inferred
/// direct parent that the import also asserts is an inference all the same and
/// stays in it, as in ROBOT (`--exclude-duplicate-axioms` is the switch that
/// drops it); the import declaration is kept, as ROBOT keeps it. The processed
/// root, by contrast, hands back the root: what the import lent is not written
/// into it. And `--include-indirect` needs redundancy removal switched off to
/// keep its indirect edges, exactly as it does in ROBOT.
#[test]
fn a_fresh_reasoned_ontology_keeps_an_inference_the_import_also_asserts() {
    let root = plant_import_fixture("plant-fresh");
    let catalog = root.with_file_name("catalog-v001.xml");
    let reason = |name: &str, extra: &[&str]| -> String {
        let out = tmp(name);
        let st = bin()
            .args(["reason", "-i"])
            .arg(&root)
            .arg("--catalog")
            .arg(&catalog)
            .args(extra)
            .args(["-f", "ofn", "-o"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(st.status.success(), "{name}: {}", String::from_utf8_lossy(&st.stderr));
        std::fs::read_to_string(&out).unwrap()
    };

    let fresh = reason("plant-fresh-out.ofn", &["--create-new-ontology", "true"]);
    assert!(
        plant_edge(&fresh, "po:Tepal", "po:Perianth"),
        "Tepal ⊑ Perianth is an inferred direct parent even though the import asserts it:\n{fresh}"
    );
    // Perianth is itself a ReproSystem, so that is the direct derived edge;
    // Tepal reaches ReproSystem through it (see the indirect run below).
    assert!(plant_edge(&fresh, "po:Perianth", ":ReproSystem"), "{fresh}");
    assert!(plant_edge(&fresh, "po:Stoma", ":LeafComponent"), "{fresh}");
    assert!(
        fresh.contains("Import(<http://x.org/po>)"),
        "a fresh reasoned ontology still declares the root's imports:\n{fresh}"
    );
    assert!(
        !fresh.contains("Declaration(Class(po:Tepal))") && !fresh.contains("TransitiveObjectProperty"),
        "only inferences, none of the import's own axioms:\n{fresh}"
    );

    let indirect = reason(
        "plant-fresh-indirect.ofn",
        &[
            "--create-new-ontology",
            "true",
            "--include-indirect",
            "true",
            "--remove-redundant-subclass-axioms",
            "false",
        ],
    );
    assert!(plant_edge(&indirect, "po:Tepal", ":ReproSystem"), "{indirect}");
    assert!(plant_edge(&indirect, "po:Tepal", "po:Structure"), "{indirect}");
    assert!(plant_edge(&indirect, "po:Tepal", "po:Organ"), "{indirect}");

    let processed = reason("plant-root-out.ofn", &[]);
    assert!(plant_edge(&processed, "po:Perianth", ":ReproSystem"), "{processed}");
    assert!(
        !plant_edge(&processed, "po:Tepal", "po:Perianth"),
        "the processed root does not carry what its import lent:\n{processed}"
    );
    assert!(processed.contains("Import(<http://x.org/po>)"), "{processed}");
}

/// A `.gz` path is a gzipped file of the format named inside the suffix:
/// `x.owl.gz` is gzipped RDF/XML, `x.ofn.gz` gzipped functional syntax. Both
/// directions, so a repository can commit a module GitHub would refuse as plain
/// text (EFO's untrimmed OBA module: 106 MB, or 2 MB gzipped).
#[test]
fn gzipped_ontologies_round_trip() {
    let a = tmp("gz_a.ofn");
    std::fs::write(
        &a,
        "Prefix(:=<http://ex/>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\nOntology(<http://ex/o.owl>\nDeclaration(Class(:Gz))\nAnnotationAssertion(rdfs:label :Gz \"gzipped class\")\n)\n",
    )
    .unwrap();
    for (mid, back) in [("gz_b.owl.gz", "gz_c.ofn"), ("gz_d.ofn.gz", "gz_e.ofn")] {
        let m = tmp(mid);
        let out = bin().args(["convert", "-i"]).arg(&a).arg("-o").arg(&m).output().unwrap();
        assert!(out.status.success(), "convert to {mid} failed:\n{}", String::from_utf8_lossy(&out.stderr));
        let bytes = std::fs::read(&m).unwrap();
        assert!(bytes.starts_with(&[0x1f, 0x8b]), "{mid} must start with the gzip magic");
        let b = tmp(back);
        let out = bin().args(["convert", "-i"]).arg(&m).arg("-o").arg(&b).output().unwrap();
        assert!(out.status.success(), "convert from {mid} failed:\n{}", String::from_utf8_lossy(&out.stderr));
        let text = std::fs::read_to_string(&b).unwrap();
        assert!(text.contains("http://ex/Gz") && text.contains("gzipped class"), "round trip through {mid} lost content:\n{text}");
    }
}

/// An `owl:imports` the catalog maps to a `.gz` module loads like any other —
/// the OWL API does the same, so a gzipped module works in Protégé and ROBOT too.
#[test]
fn a_catalog_import_may_be_gzipped() {
    let dir = tmp("gzcat");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let module_ofn = dir.join("mod.ofn");
    std::fs::write(
        &module_ofn,
        "Prefix(:=<http://ex/mod/>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\nOntology(<http://ex/imports/mod_import.owl>\nDeclaration(Class(:M1))\nAnnotationAssertion(rdfs:label :M1 \"module class\")\n)\n",
    )
    .unwrap();
    let module_gz = dir.join("mod_import.owl.gz");
    assert!(bin().args(["convert", "-i"]).arg(&module_ofn).arg("-o").arg(&module_gz).status().unwrap().success());
    std::fs::write(
        dir.join("edit.ofn"),
        "Prefix(:=<http://ex/edit/>)\nOntology(<http://ex/edit.owl>\nImport(<http://ex/imports/mod_import.owl>)\nDeclaration(Class(:E1))\nSubClassOf(:E1 <http://ex/mod/M1>)\n)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n  <uri name=\"http://ex/imports/mod_import.owl\" uri=\"mod_import.owl.gz\"/>\n</catalog>\n",
    )
    .unwrap();
    let out_path = dir.join("merged.ofn");
    let out = bin()
        .args(["merge", "--catalog"]).arg(dir.join("catalog-v001.xml")).arg("-i").arg(dir.join("edit.ofn")).arg("-o").arg(&out_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "merge through a gzipped import failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let merged = std::fs::read_to_string(&out_path).unwrap();
    assert!(merged.contains("module class"), "the gzipped module's content did not reach the merge:\n{merged}");
    let _ = std::fs::remove_dir_all(&dir);
}
