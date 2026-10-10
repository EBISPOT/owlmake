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
/// only matching classes; `filter --select "self parents object-properties"`
/// keeps the seed and its parents, and the object properties among them, which
/// are none. As ROBOT 1.9.11 does.
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
        !f.contains("<http://x.org/r>"),
        "object-properties selects among the seed and its parents, not the ontology:\n{f}"
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
        .args(["template", "--prefix", "EX: http://purl.obolibrary.org/obo/EX_", "--template"])
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
        .args(["--header", "ID|LABEL|SubClass Of", "--export"])
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
        .args(["template", "--prefix", "EX: http://purl.obolibrary.org/obo/EX_", "--template"])
        .arg(&tmpl)
        .arg("-o")
        .arg(&owl)
        .args(["--format", "ofn"])
        .status()
        .unwrap();
    assert!(status.success(), "template command failed");
    let text = std::fs::read_to_string(&owl).unwrap();

    // TYPE handling: part-of is an object property, not a class.
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

/// Individuals: a `TYPE` cell naming a class makes the row a named individual of
/// that class (several with `TYPE SPLIT=`), and an `I <prop>` column asserts a
/// property — to another individual, or to a literal when a row types `<prop>` as
/// a data property.
#[test]
fn template_individual_types_and_property_assertions() {
    let tmpl = tmp("ind.tsv");
    std::fs::write(
        &tmpl,
        "ID\tLabel\tType\tLocated in\tPopulation\n\
         ID\tLABEL\tTYPE SPLIT=|\tI EX:located_in\tI 'population' SPLIT=|\n\
         EX:population\tpopulation\tdata property\t\t\n\
         EX:europe\tEurope\tEX:Region\t\t\n\
         EX:austria\tAustria\tEX:Country|EX:Place\tEurope\t9000000^^xsd:integer|about nine million\n",
    )
    .unwrap();

    let owl = tmp("ind.ofn");
    let status = bin()
        .args(["template", "--prefix", "EX: http://purl.obolibrary.org/obo/EX_", "--template"])
        .arg(&tmpl)
        .arg("-o")
        .arg(&owl)
        .args(["--format", "ofn"])
        .status()
        .unwrap();
    assert!(status.success(), "template command failed");
    let text = std::fs::read_to_string(&owl).unwrap();
    let ex = |local: &str| format!("<http://purl.obolibrary.org/obo/EX_{local}>");

    // The subject is an individual, never a class, and carries each TYPE value.
    assert!(
        text.contains(&format!("Declaration(NamedIndividual({}))", ex("austria"))),
        "expected an individual declaration:\n{text}"
    );
    assert!(
        !text.contains(&format!("Declaration(Class({}))", ex("austria"))),
        "a row typed by a class is not itself a class:\n{text}"
    );
    for class in ["Country", "Place"] {
        assert!(
            text.contains(&format!("ClassAssertion({} {})", ex(class), ex("austria"))),
            "expected a {class} class assertion:\n{text}"
        );
    }
    assert!(
        text.contains(&format!("ClassAssertion({} {})", ex("Region"), ex("europe"))),
        "expected a Region class assertion:\n{text}"
    );

    // `I <prop>` to an individual named by label; the property is never a class.
    assert!(
        text.contains(&format!(
            "ObjectPropertyAssertion({} {} {})",
            ex("located_in"),
            ex("austria"),
            ex("europe")
        )),
        "expected an object property assertion:\n{text}"
    );
    assert!(
        !text.contains(&format!("ClassAssertion({}", ex("located_in"))),
        "the property of an `I <prop>` column is not a type:\n{text}"
    );

    // `I <prop>` on a data property named by label: typed and plain literals.
    assert!(
        text.contains(&format!(
            "DataPropertyAssertion({} {} \"9000000\"^^xsd:integer)",
            ex("population"),
            ex("austria")
        )),
        "expected a typed data property assertion:\n{text}"
    );
    assert!(
        text.contains(&format!(
            "DataPropertyAssertion({} {} \"about nine million\")",
            ex("population"),
            ex("austria")
        )),
        "expected a plain data property assertion:\n{text}"
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

#[test]
fn one_disjointness_axiom_however_its_members_are_ordered() {
    // OWL makes DisjointClasses a SET: `DisjointClasses(A B)` in one source and
    // `DisjointClasses(B A)` in another are ONE axiom, and any serialization
    // must render it once. Keeping both rendered UBERON's
    // `DisjointClasses(UBERON_0000001 GO_0110165)` twice — once under each
    // member's RDF/XML frame.
    let ont = tmp("disjoint-orders.ofn");
    std::fs::write(
        &ont,
        "Prefix(:=<http://example.org/>)\n\
         Ontology(\n\
         Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         DisjointClasses(:A :B)\n\
         DisjointClasses(:B :A)\n\
         )\n",
    )
    .unwrap();
    let out_owl = tmp("disjoint-orders.owl");
    let st = bin().args(["convert", "-i"]).arg(&ont).arg("-o").arg(&out_owl).status().unwrap();
    assert!(st.success());
    let text = std::fs::read_to_string(&out_owl).unwrap();
    assert_eq!(
        text.matches("disjointWith").count(),
        1,
        "one disjointness axiom must render exactly once:\n{text}"
    );
    let _ = std::fs::remove_file(&ont);
    let _ = std::fs::remove_file(&out_owl);
}

#[test]
fn explain_unsatisfiability_justification_is_minimal() {
    // :A is unsatisfiable through exactly three axioms. The rest of the ontology
    // is a decoy: a redundant second route to :C and axioms about unrelated
    // classes. A justification that carries any of them is not minimal, and a
    // search that cannot cut them away is the search that never terminates on a
    // real merge.
    let ont = tmp("unsat-minimal.ofn");
    std::fs::write(
        &ont,
        "Prefix(:=<http://example.org/>)\n\
         Ontology(\n\
         SubClassOf(:A :B)\n\
         SubClassOf(:A :D)\n\
         DisjointClasses(:B :D)\n\
         SubClassOf(:B :C)\n\
         SubClassOf(:A :C)\n\
         SubClassOf(:E :F)\n\
         SubClassOf(:F :G)\n\
         )\n",
    )
    .unwrap();

    let out = bin()
        .args(["explain", "-i"])
        .arg(&ont)
        .args(["-M", "unsatisfiability", "--unsatisfiable", "http://example.org/A"])
        .output()
        .unwrap();
    assert!(out.status.success(), "explain failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Justification 1 (3 axioms)"), "expected a minimal 3-axiom justification:\n{text}");
    assert!(!text.contains("example.org/E"), "unrelated axioms must not appear:\n{text}");
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
pattern_iri: http://purl.obolibrary.org/obo/cl/patterns/part_of_x.yaml
classes:
  cell: CL:0000000
relations:
  part_of: BFO:0000050
vars:
  part: "'cell'"
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
        schema_text.contains("\"targets\"") && schema_text.contains("use_builtin_rules"),
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
    // The seed asks for the standard build and states nothing it derives.
    assert!(
        before.contains("id: foo") && before.contains("edit_format: obo") && !before.contains("targets:"),
        "the seed should be the repository's options, not a plan spelled out:\n{before}"
    );

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
/// on that file produces.
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

/// `remove --select complement --select "classes individuals annotation-properties"`
/// is the cut the `minimal` module type makes: keep the seed, drop every other
/// class and individual, and bridge the hierarchy across what goes. The singular
/// `individual`, which the module type writes for a product of the repository's
/// own, is no selector: it selects nothing, so the individuals stay. As ROBOT
/// 1.9.11 does.
#[test]
fn class_complement_cuts_a_minimal_module_to_its_seed() {
    let inp = tmp("minimalmod.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/m>\n\
         Declaration(Class(<http://x.org/Seed>))\n\
         Declaration(Class(<http://x.org/Mid>))\n\
         Declaration(Class(<http://x.org/Top>))\n\
         Declaration(Class(<http://x.org/Stray>))\n\
         Declaration(NamedIndividual(<http://x.org/i1>))\n\
         Declaration(NamedIndividual(<http://x.org/i2>))\n\
         SubClassOf(<http://x.org/Seed> <http://x.org/Mid>)\n\
         SubClassOf(<http://x.org/Mid> <http://x.org/Top>)\n\
         SubClassOf(<http://x.org/Stray> <http://x.org/Top>)\n\
         ClassAssertion(<http://x.org/Stray> <http://x.org/i1>)\n\
         ClassAssertion(<http://x.org/Seed> <http://x.org/i2>)\n\
         AnnotationAssertion(rdfs:label <http://x.org/Seed> \"seed\")\n\
         AnnotationAssertion(rdfs:label <http://x.org/Stray> \"stray\")\n\
         )\n",
    )
    .unwrap();
    let out = tmp("minimalmod-o.ofn");
    let cut = |kinds: &str| -> String {
        assert!(bin().args(["remove", "-i"]).arg(&inp)
            .args(["--term", "rdfs:label",
                   "--term", "http://x.org/Seed", "--term", "http://x.org/Top",
                   "--term", "http://x.org/i2",
                   "--select", "complement",
                   "--select", kinds, "-o"])
            .arg(&out).status().unwrap().success());
        std::fs::read_to_string(&out).unwrap()
    };
    let r = cut("classes individuals annotation-properties");
    assert!(r.contains("\"seed\""), "the seed keeps its annotations:\n{r}");
    assert!(!r.contains("Stray"), "a class outside the seed goes:\n{r}");
    assert!(!r.contains("/Mid"), "so does an intermediate above the seed:\n{r}");
    assert!(!r.contains("/i1"), "an individual outside the seed goes:\n{r}");
    assert!(r.contains("<http://x.org/i2>"), "a seeded individual stays:\n{r}");
    assert!(
        r.contains("SubClassOf(<http://x.org/Seed> <http://x.org/Top>)"),
        "the hierarchy bridges across the removed intermediate:\n{r}"
    );
    let r = cut("classes individual annotation-properties");
    assert!(!r.contains("Stray"), "a class outside the seed goes:\n{r}");
    assert!(r.contains("Declaration(NamedIndividual(<http://x.org/i1>))"), "`individual` selects nothing:\n{r}");
    let _ = std::fs::remove_file(&out);
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

/// A `SELECT` with an `ORDER BY` comes out in the order it asks for, in memory as
/// on disk, as ROBOT 1.9.11 returns it.
#[test]
fn an_order_by_orders_the_rows() {
    let out = tmp("ordered.csv");
    for input in ["selection-options.ofn", "selection-options.owl"] {
        let run = bin()
            .args(["query", "--input"])
            .arg(robot_fixture(input))
            .arg("--query")
            .arg(robot_fixture("select-classes.rq"))
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{input}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("selection-options.classes.robot.csv"), "{input}");
    }
    let _ = std::fs::remove_file(&out);
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
                // The justification ROBOT finds: Tepal is a perianth, a perianth is
                // an organ part of a flower, and a flower is a structure part of a
                // reproductive system — so a flower is one, and Tepal is part of
                // one without leaning on part_of's transitivity.
                assert!(text.contains("Justification 1 (6 axioms)"), "{reasoner}: {text}");
                assert!(
                    text.contains("sup: Class(Class(IRI(\"http://x.org/po#Structure\"))), sub: Class(Class(IRI(\"http://x.org/po#Flower\")))"),
                    "the imported Flower ⊑ Structure is part of the justification:\n{text}"
                );
                assert!(!text.contains("TransitiveObjectProperty"), "{reasoner}: {text}");
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
    assert!(!ok && err.contains("no prefix `ZZQ` is bound") && !err.contains("not entailed"), "{err}");

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
    assert!(!out.status.success() && err.contains("INVALID REASONER ERROR unknown reasoner: hermitt"), "{err}");
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

/// The reasoner that decides an entailment is the reasoner that justifies it.
///
/// `:A` here is unsatisfiable only through a cardinality clash — an axiom shape
/// the EL engine does not see — so under `-r hermit` the class must both be
/// FOUND unsatisfiable and be explained. Deciding with hermit-rs and minimizing
/// with EL reports the ontology coherent and returns nothing, which is what the
/// `elk` half of this test shows.
#[test]
fn explain_justifies_a_non_el_unsatisfiability_with_the_reasoner_that_decided_it() {
    let ont = tmp("cardinality-clash.ofn");
    std::fs::write(
        &ont,
        "Prefix(:=<http://example.org/>)\n\
         Ontology(\n\
         Declaration(Class(:A))\n\
         Declaration(ObjectProperty(:r))\n\
         SubClassOf(:A ObjectMinCardinality(2 :r))\n\
         SubClassOf(:A ObjectMaxCardinality(1 :r))\n\
         )\n",
    )
    .unwrap();

    let run = |reasoner: &str| {
        let out = bin()
            .args(["explain", "-i"])
            .arg(&ont)
            .args(["-M", "unsatisfiability", "-u", "all", "-r", reasoner])
            .output()
            .unwrap();
        assert!(out.status.success(), "{reasoner}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    let hermit = run("hermit");
    assert!(
        hermit.contains("1 justification(s)") && hermit.contains("http://example.org/A"),
        "hermit must explain the class it found unsatisfiable:\n{hermit}"
    );
    assert!(
        hermit.contains("ObjectMinCardinality") && hermit.contains("ObjectMaxCardinality"),
        "the justification is the clashing pair:\n{hermit}"
    );

    // The EL engine cannot see the clash at all, so there is nothing to explain.
    assert_eq!(run("elk"), "No explanations found.");

    let _ = std::fs::remove_file(&ont);
}

/// The justification search is bounded by the size of the justification's
/// neighbourhood, not by the size of the module around it: it grows a set
/// outward from the terms of the entailment until it entails, and minimizes
/// that. Both halves are measured here — how many entailment tests the search
/// asks, and how big the largest ontology it classified was. Contracting the
/// module itself instead costs one classification of the whole module per axiom
/// in it, which is hours on a module the size of a real import closure.
#[test]
fn explain_does_not_test_every_axiom_of_the_module() {
    // `:X ⊑ :A`, `:X ⊑ :B` and `DisjointClasses(:A :B)` make `:X` unsatisfiable
    // in three axioms; the 8,000 others are pulled into the ⊥-module by hanging
    // off `:A`, and none of them is part of any justification.
    const N: usize = 4000;
    let mut text = String::from("Prefix(:=<http://example.org/>)\nOntology(\n");
    text.push_str("SubClassOf(:X :A)\nSubClassOf(:X :B)\nDisjointClasses(:A :B)\n");
    for i in 0..N {
        text.push_str(&format!("SubClassOf(:A ObjectSomeValuesFrom(:p :F{i}))\n"));
        text.push_str(&format!("SubClassOf(:F{i} :G{i})\n"));
    }
    text.push_str(")\n");
    let ont = tmp("wide-module.ofn");
    std::fs::write(&ont, text).unwrap();

    let out = bin()
        .args(["explain", "-i"])
        .arg(&ont)
        .args(["-M", "unsatisfiability", "-u", "all"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(
        report.contains("1 justification(s)") && report.contains("DisjointClasses"),
        "the three-axiom justification must be found:\n{report}"
    );

    // The status line carries all three numbers.
    let line = err
        .lines()
        .find(|l| l.contains("entailment tests"))
        .unwrap_or_else(|| panic!("no search-cost status line:\n{err}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    let num = |marker: &str, offset: isize| -> usize {
        let i = words.iter().position(|w| *w == marker).unwrap_or_else(|| panic!("{marker}: {line}"));
        words[(i as isize + offset) as usize]
            .trim_matches(|c: char| !c.is_ascii_digit())
            .parse()
            .unwrap_or_else(|_| panic!("{marker}: {line}"))
    };
    let (tests, widest, candidates) = (num("entailment", -1), num("widest", 1), num("over", 1));
    assert!(candidates > 2 * N, "the module must be wide: {line}");
    assert!(tests * 10 < candidates, "the search must not walk the module: {line}");
    assert!(
        widest * 10 < candidates,
        "the search must not classify the module: {line}"
    );

    let _ = std::fs::remove_file(&ont);
}

/// `-o /dev/null` is a discard, not a document: a command run for its verdict
/// writes nothing and does not need a format to infer.
#[cfg(unix)]
#[test]
fn output_to_dev_null_is_a_discard() {
    let ont = tmp("discard.ofn");
    std::fs::write(
        &ont,
        "Prefix(:=<http://example.org/>)\n\
         Ontology(\n\
         SubClassOf(:A :B)\n\
         SubClassOf(:B :C)\n\
         )\n",
    )
    .unwrap();
    let out = bin()
        .args(["reason", "-i"])
        .arg(&ont)
        .args(["-r", "elk", "-o", "/dev/null"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(!err.contains("cannot infer ontology format"), "{err}");
    let _ = std::fs::remove_file(&ont);
}

/// `check-align`: a class is aligned when the reasoner puts it under a class of
/// the upper ontology. An unaligned class takes its ancestors with it; an obsolete
/// class is never checked; `--detail` picks which of them the report lists; and
/// any unaligned class fails the command unless `--fail false`. The expected
/// reports are the ones the `odk:check-align` plugin (0.3.1) writes for the same
/// input.
#[test]
fn check_align_reports_unaligned_classes() {
    let upper = tmp("align-upper.ofn");
    std::fs::write(
        &upper,
        "Prefix(:=<http://example.org/upper/>)\n\
         Ontology(<http://example.org/upper.owl>\n\
         Declaration(Class(:U1))\nDeclaration(Class(:U2))\nSubClassOf(:U2 :U1)\n)\n",
    )
    .unwrap();
    let ont = tmp("align.ofn");
    std::fs::write(
        &ont,
        "Prefix(:=<http://example.org/t/T_>)\n\
         Prefix(o:=<http://example.org/other/O_>)\n\
         Prefix(u:=<http://example.org/upper/>)\n\
         Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
         Ontology(<http://example.org/t.owl>\n\
         Declaration(Class(:A))\nDeclaration(Class(:B))\nDeclaration(Class(:C))\nDeclaration(Class(:D))\n\
         Declaration(Class(:E))\nDeclaration(Class(:F))\nDeclaration(Class(:G))\nDeclaration(Class(o:H))\n\
         Declaration(Class(:I))\nDeclaration(ObjectProperty(:r))\n\
         SubClassOf(:A u:U1)\nSubClassOf(:B :A)\n\
         AnnotationAssertion(rdfs:label :C \"unaligned root\")\n\
         SubClassOf(:D :C)\nSubClassOf(:D ObjectSomeValuesFrom(:r :G))\n\
         EquivalentClasses(:E u:U2)\n\
         AnnotationAssertion(owl:deprecated :F \"true\"^^xsd:boolean)\n\
         SubClassOf(o:H :C)\nSubClassOf(:I o:H)\n)\n",
    )
    .unwrap();
    let report = tmp("align-report.txt");
    let run = |extra: &[&str]| {
        let _ = std::fs::remove_file(&report);
        let out = bin()
            .args(["check-align", "-i"])
            .arg(&ont)
            .arg("-u")
            .arg(&upper)
            .arg("--report-output")
            .arg(&report)
            .args(extra)
            .output()
            .unwrap();
        (out.status.success(), std::fs::read_to_string(&report).unwrap_or_default())
    };
    let t = |n: &str| format!("http://example.org/t/T_{n}\n");

    // By default only the classes with nothing above them: C heads the unaligned
    // branch, G is only ever mentioned. F is obsolete, E sits under U1 through U2.
    let (ok, said) = run(&[]);
    assert!(!ok, "an unaligned class fails the command");
    assert_eq!(said, format!("{}{}", t("C"), t("G")));

    // Every unaligned class, the out-of-base one included.
    let (_, said) = run(&["--detail", "all"]);
    assert_eq!(
        said,
        format!("http://example.org/other/O_H\n{}{}{}{}", t("C"), t("D"), t("G"), t("I"))
    );

    // A class nothing is said about is skipped on request.
    let (_, said) = run(&["--detail", "all", "--ignore-dangling", "true"]);
    assert!(!said.contains("T_G") && said.contains("T_C"), "{said}");

    // Reported, not failed.
    let (ok, said) = run(&["--fail", "false"]);
    assert!(ok && said == format!("{}{}", t("C"), t("G")), "{said}");

    for f in [&upper, &ont, &report] {
        let _ = std::fs::remove_file(f);
    }
}

/// `sssom:inject --create --direct` — a mapping set exported as an ontology, in
/// the recipe ODK builds a mappings component with. The expected files are what
/// ROBOT 1.9.7 with the sssom plugin (1.10.0) writes for the same command: one set
/// read as SSSOM 1.0, where `predicate_type` is not a slot and decides nothing,
/// and one declaring 1.1, with every enumerated value and propagated set slots.
#[test]
fn sssom_inject_exports_a_mapping_set() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sssom-export");
    for version in ["v1.0", "v1.1"] {
        let written = tmp(&format!("sssom-export-{version}.ofn"));
        let out = bin()
            .args(["--add-prefix", "sssom: https://w3id.org/sssom/"])
            .args(["--add-prefix", "semapv: http://w3id.org/semapv/vocab/"])
            .args(["sssom:inject", "--sssom"])
            .arg(fixtures.join(format!("{version}.sssom.tsv")))
            .args(["--create", "--direct", "annotate"])
            .args(["--ontology-iri", "http://purl.obolibrary.org/obo/x/components/m.owl"])
            .args(["--version-iri", "http://purl.obolibrary.org/obo/x/releases/2026-09-21/components/m.owl"])
            .args(["convert", "-f", "ofn", "--output"])
            .arg(&written)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            std::fs::read_to_string(fixtures.join(format!("{version}.ofn"))).unwrap(),
            "{version}"
        );
    }
}

/// A prefix ADDED on the command line is declared by an ontology built from
/// nothing, used or not; one given with `--prefix` only reads CURIEs. As ROBOT
/// 1.9.7 writes them.
#[test]
fn an_added_prefix_is_declared_by_a_new_ontology() {
    let table = tmp("added-prefix.tsv");
    std::fs::write(&table, "ID\tLabel\nID\tA rdfs:label\nzz:A\ta\n").unwrap();
    let run = |option: &str, format: &str| {
        let written = tmp(&format!("added-prefix{option}.{format}"));
        let out = bin()
            .args([option, "zz: http://example.org/", "template", "--template"])
            .arg(&table)
            .arg("-o")
            .arg(&written)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        std::fs::read_to_string(&written).unwrap()
    };
    let added = run("--add-prefix", "ofn");
    assert!(added.contains("Prefix(zz:=<http://example.org/>)"), "{added}");
    assert!(added.contains("Declaration(Class(zz:A))"), "{added}");
    assert!(run("--add-prefix", "owl").contains("xmlns:zz=\"http://example.org/\""));
    let read_only = run("--prefix", "ofn");
    assert!(!read_only.contains("Prefix(zz:"), "{read_only}");
    assert!(read_only.contains("Declaration(Class(<http://example.org/A>))"), "{read_only}");
}

/// `tsvalid` — the table lint the standard build runs. The expected findings
/// are what the Python tool (0.0.5) prints for the same file: on standard error,
/// in a logger's format, failing nothing unless `--fail` is given.
#[test]
fn tsvalid_lints_a_table_as_the_tool_does() {
    let table = tmp("tsvalid-bad.tsv");
    std::fs::write(&table, "# comment\na\tb\tb\n 1\t2 \t3\n\n4\t5\r\n6\t7\t\u{e9}").unwrap();
    let name = table.display().to_string();
    let out = bin().arg("tsvalid").arg(&table).args(["--comment", "#", "--summary"]).output().unwrap();
    assert!(out.status.success());
    let expected: String = [
        (2, 0, "ERROR", "E10", "Header row has duplicate values, line 2."),
        (3, 1, "ERROR", "E2", "Redundant leading whitespace in column 1 at line number 3."),
        (3, 2, "ERROR", "E3", "Redundant trailing whitespace in column 2 at line number 3."),
        (4, 0, "ERROR", "E4", "Number of tabs in line 4 does not match tabs in header."),
        (5, 0, "ERROR", "E4", "Number of tabs in line 5 does not match tabs in header."),
        (5, 0, "ERROR", "E1", "Invalid line break in line 5."),
        (4, 0, "ERROR", "E5", "Empty line 4."),
        (6, 3, "WARNING", "W1", "Non ASCII character in column 3 at line number 6."),
        (5, 0, "ERROR", "E9", "Last row in file should be empty."),
    ]
    .iter()
    .map(|(line, column, level, code, text)| format!("{level}:root:{name}:{line}:{column}: {code}: {text}\n"))
    .collect();
    assert_eq!(String::from_utf8_lossy(&out.stderr), expected);
    let summary = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(summary.starts_with("\n##### TSValid Summary #####\n\nError: duplicate Value In Header Row\n * count: 1\n * error_code: E10\n"), "{summary}");
    assert!(summary.contains("\nError: number Of Tabs Check\n * count: 2\n * error_code: E4\n"), "{summary}");

    // Skipped by code or by pattern; and `--fail` stops at the first finding.
    let out = bin().arg("tsvalid").arg(&table).args(["--comment", "#", "--skip", "E.*"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stderr).lines().count(), 1);
    let out = bin().arg("tsvalid").arg(&table).args(["--comment", "#", "--fail"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(err.lines().count(), 2, "{err}");
    assert!(err.contains("tsvalid: Validation failed: {'line_number': 2, 'column': 0, "), "{err}");
}

/// `context2csv` — a JSON-LD context as the prefix table the SQL export reads.
#[test]
fn context2csv_writes_the_prefix_table() {
    use std::io::Write as _;
    let mut child = bin()
        .arg("context2csv")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"@context": {"obo": "http://purl.obolibrary.org/obo/", "EX": "http://example.org/EX_"}}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "prefix,base\nobo,http://purl.obolibrary.org/obo/\nEX,http://example.org/EX_\n"
    );
}

/// `make-release-assets.py` — against a stand-in for GitHub's API, which records
/// what it is asked: an existing release and an existing asset are both replaced
/// under `--create --force`, and the file goes to the release's upload address.
#[test]
fn release_assets_are_uploaded_to_a_release() {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut asked: Vec<String> = Vec::new();
        for stream in listener.incoming().take(7) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let (mut length, mut token) = (0usize, String::new());
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap(),
                    "authorization" => token = value.trim().to_string(),
                    _ => {}
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let request = request.trim().trim_end_matches(" HTTP/1.1").to_string();
            assert_eq!(token, "token SECRET", "{request}");
            let answer = match request.as_str() {
                "GET /repos/org/repo/releases?per_page=100&page=1" => r#"[{"id": 7, "tag_name": "v1"}]"#.to_string(),
                "POST /repos/org/repo/releases" => r#"{"id": 8}"#.to_string(),
                "GET /repos/org/repo/releases/tags/v1" => format!(
                    r#"{{"id": 8, "upload_url": "http://127.0.0.1:{port}/upload/8/assets{{?name,label}}"}}"#
                ),
                "GET /repos/org/repo/releases/8/assets?per_page=100&page=1" => {
                    r#"[{"id": 3, "name": "x.owl", "size": 10, "download_count": 2}]"#.to_string()
                }
                "POST /upload/8/assets?name=x.owl&label=" => format!(r#"{{"name": "x.owl", "size": {length}}}"#),
                _ => String::new(),
            };
            asked.push(format!("{request} [{length}]"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            )
            .unwrap();
        }
        asked
    });

    let file = tmp("release-assets").join("x.owl");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "<rdf/>").unwrap();
    let out = bin()
        .args(["make-release-assets.py", "--api-url", &format!("http://127.0.0.1:{port}"), "-t", "SECRET", "-r", "org/repo", "--release", "v1", "-c", "-f"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let path = file.display();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("Existing assets:\nAsset: x.owl Size: 10 Downloads: 2\nUploading: {path}\nUploaded: x.owl 6 from {path}\n")
    );
    assert_eq!(
        server.join().unwrap(),
        [
            "GET /repos/org/repo/releases?per_page=100&page=1 [0]",
            "DELETE /repos/org/repo/releases/7 [0]",
            "POST /repos/org/repo/releases [72]",
            "GET /repos/org/repo/releases/tags/v1 [0]",
            "GET /repos/org/repo/releases/8/assets?per_page=100&page=1 [0]",
            "DELETE /repos/org/repo/releases/assets/3 [0]",
            "POST /upload/8/assets?name=x.owl&label= [6]",
        ]
    );
}

/// `reason --axiom-generators PropertyAssertion` asserts the entailed object
/// property assertions between individuals; with `--reasoner hermit` that
/// includes the inverse of an asserted assertion. Only a `SubClassOf` can be
/// annotated as inferred, so `--annotate-inferred-axioms true` refuses them. As
/// ROBOT 1.9.11 reasons over the same ontology.
#[test]
fn reason_property_assertion_generator() {
    let inp = tmp("pa.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://ex/>)\nOntology(\n\
         Declaration(ObjectProperty(:hasSubCohort))\nDeclaration(ObjectProperty(:isSubCohortOf))\n\
         Declaration(NamedIndividual(:twingene))\nDeclaration(NamedIndividual(:registry))\n\
         InverseObjectProperties(:hasSubCohort :isSubCohortOf)\n\
         ObjectPropertyAssertion(:isSubCohortOf :twingene :registry)\n)\n",
    )
    .unwrap();
    let out = tmp("pa-out.ofn");
    let status = bin()
        .args(["reason", "--reasoner", "hermit", "--axiom-generators", "PropertyAssertion"])
        .args(["--exclude-duplicate-axioms", "true"])
        .arg("-i").arg(&inp).arg("-o").arg(&out).args(["--format", "ofn"])
        .status()
        .unwrap();
    assert!(status.success(), "reason failed");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.contains("ObjectPropertyAssertion(<http://ex/hasSubCohort> <http://ex/registry> <http://ex/twingene>)"),
        "the inverse assertion is asserted:\n{text}"
    );
    assert_eq!(
        text.matches("ObjectPropertyAssertion(").count(),
        2,
        "the asserted assertion is not duplicated:\n{text}"
    );

    let _ = std::fs::remove_file(&out);
    let run = bin()
        .args(["reason", "--reasoner", "hermit", "--axiom-generators", "PropertyAssertion"])
        .args(["--annotate-inferred-axioms", "true"])
        .arg("-i").arg(&inp).arg("-o").arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success() && stderr.contains("AXIOM TYPE ERROR"), "{stderr}");
    assert!(!out.exists());
}

/// `reason --reasoner hermit` on an inconsistent ontology fails with the
/// inconsistency error, as it does under the EL reasoner: never with a panic.
#[test]
fn reason_hermit_reports_inconsistency_as_an_error() {
    let inp = tmp("inconsistent.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://ex/>)\nOntology(\nDeclaration(Class(:A))\nDeclaration(NamedIndividual(:a))\n\
         SubClassOf(:A owl:Nothing)\nClassAssertion(:A :a)\n)\n",
    )
    .unwrap();
    let out = tmp("inconsistent-out.ofn");
    let output = bin()
        .args(["reason", "--reasoner", "hermit", "-i"]).arg(&inp).arg("-o").arg(&out)
        .output()
        .unwrap();
    assert!(!output.status.success(), "an inconsistent ontology fails reason");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ontology is inconsistent"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// `reason --properties` restricts the `PropertyAssertion` generator to the
/// named object properties, given as CURIEs of the ontology's own prefixes.
#[test]
fn reason_property_assertions_restricted_to_named_properties() {
    let inp = tmp("pa-props.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://ex/>)\nOntology(\n\
         Declaration(ObjectProperty(:hasSubCohort))\nDeclaration(ObjectProperty(:isSubCohortOf))\n\
         Declaration(ObjectProperty(:partOf))\n\
         Declaration(NamedIndividual(:a))\nDeclaration(NamedIndividual(:b))\nDeclaration(NamedIndividual(:c))\n\
         InverseObjectProperties(:hasSubCohort :isSubCohortOf)\n\
         TransitiveObjectProperty(:partOf)\n\
         ObjectPropertyAssertion(:isSubCohortOf :a :b)\n\
         ObjectPropertyAssertion(:partOf :a :b)\nObjectPropertyAssertion(:partOf :b :c)\n)\n",
    )
    .unwrap();
    let run = |properties: &str| {
        let out = tmp(&format!("pa-props-out-{}.ofn", properties.len()));
        let status = bin()
            .args(["reason", "--reasoner", "hermit", "--axiom-generators", "PropertyAssertion"])
            .args(["--exclude-duplicate-axioms", "true", "--properties", properties])
            .arg("-i").arg(&inp).arg("-o").arg(&out).args(["--format", "ofn"])
            .status()
            .unwrap();
        assert!(status.success(), "reason failed");
        std::fs::read_to_string(&out).unwrap()
    };
    let text = run(":hasSubCohort");
    assert!(
        text.contains("ObjectPropertyAssertion(<http://ex/hasSubCohort> <http://ex/b> <http://ex/a>)"),
        "the listed property's inverse assertion is asserted:\n{text}"
    );
    assert!(
        !text.contains("ObjectPropertyAssertion(<http://ex/partOf> <http://ex/a> <http://ex/c>)"),
        "the transitive closure of an unlisted property is not:\n{text}"
    );
    let text = run(":partOf,:hasSubCohort");
    assert!(
        text.contains("ObjectPropertyAssertion(<http://ex/partOf> <http://ex/a> <http://ex/c>)")
            && text.contains("ObjectPropertyAssertion(<http://ex/hasSubCohort> <http://ex/b> <http://ex/a>)"),
        "both listed properties' inferences are asserted:\n{text}"
    );
}

/// `owltools … --list-cycles -f`: the asserted graph's cycles are listed and
/// counted, and a count above zero fails the command. `A ⊑ B ⊑ part_of some A`
/// puts A, B and the restriction in one cycle — three for each of A and B.
#[test]
fn owltools_list_cycles_counts_and_fails_on_a_cycle() {
    let cyclic = tmp("cycles.ofn");
    std::fs::write(
        &cyclic,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         Declaration(ObjectProperty(:part_of))\n\
         SubClassOf(:A :B)\n\
         SubClassOf(:B ObjectSomeValuesFrom(:part_of :A))\n\
         )\n",
    )
    .unwrap();
    let out = bin().args(["owltools", cyclic.to_str().unwrap(), "--list-cycles", "-f"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "a cycle must fail the check:\n{text}");
    assert!(text.ends_with("Number of cycles: 6\n"), "{text}");
    assert!(
        text.contains("http://x.org/B in-cycle-with http://x.org/A // via [http://x.org/part_of some]"),
        "{text}"
    );

    // Without `-f` the same cycles are listed, and the command succeeds.
    let out = bin().args(["owltools", cyclic.to_str().unwrap(), "--list-cycles"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));

    // An acyclic graph has none.
    let acyclic = tmp("nocycles.ofn");
    std::fs::write(
        &acyclic,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/o>\n\
         SubClassOf(:A :B)\n\
         SubClassOf(:B ObjectSomeValuesFrom(:part_of :C))\n\
         EquivalentClasses(:C ObjectIntersectionOf(:A ObjectSomeValuesFrom(:part_of :B)))\n\
         )\n",
    )
    .unwrap();
    let out = bin().args(["owltools", acyclic.to_str().unwrap(), "--list-cycles", "-f"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Number of cycles: 0\n");
    assert_eq!(out.status.code(), Some(0));
}

/// `report` warns on the console about a rule whose query does not PROJECT
/// `?entity`, `?property` or `?value`, a line per variable, whether or not it
/// matched anything, ahead of the summary. A projected variable left unbound is
/// not a defect: the bundled "missing X" rules bind `?value` only inside
/// `FILTER NOT EXISTS` or an `OPTIONAL … !bound`, and their rows are reported
/// with an empty Value and no warning.
#[test]
fn report_warns_only_for_variables_a_query_does_not_project() {
    let dir = tmp("report-vars");
    std::fs::create_dir_all(&dir).unwrap();
    let ont = dir.join("test.obo");
    std::fs::write(
        &ont,
        "format-version: 1.4\nontology: ex\n\n\
         [Term]\nid: EX:0000001\nname: root thing\n\n\
         [Term]\nid: EX:0000002\nname: thing A\nis_a: EX:0000001 ! root thing\n",
    )
    .unwrap();
    let no_value = dir.join("no_value.sparql");
    std::fs::write(
        &no_value,
        "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n\
         SELECT DISTINCT ?entity ?property WHERE { VALUES ?property { rdfs:label } ?entity ?property ?x }\n",
    )
    .unwrap();
    let never = dir.join("never.sparql");
    std::fs::write(&never, "SELECT ?entity WHERE { ?entity <http://example.org/nothing> ?o }\n").unwrap();

    // The default profile: four of its rules match rows with an unbound `?value`.
    let tsv = dir.join("default.tsv");
    let out = bin().args(["report", "-i"]).arg(&ont).arg("-o").arg(&tsv).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("query is missing"), "{stdout}");
    let rows = std::fs::read_to_string(&tsv).unwrap();
    assert!(rows.contains("WARN\tmissing_definition\tobo:EX_0000001\tIAO:0000115\t\n"), "{rows}");

    let profile = dir.join("profile.txt");
    std::fs::write(
        &profile,
        format!("WARN\tfile:{}\nINFO\tfile:{}\n", no_value.display(), never.display()),
    )
    .unwrap();
    let out = bin()
        .args(["report", "-i"])
        .arg(&ont)
        .arg("--profile")
        .arg(&profile)
        .arg("-o")
        .arg(dir.join("custom.tsv"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    // …and a rule that matched nothing is told all the same.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "WARN: 'no_value' query is missing ?value variable\n\
         WARN: 'never' query is missing ?property variable\n\
         WARN: 'never' query is missing ?value variable\n\
         Violations: 4\n\
         -----------------\n\
         ERROR:      0\n\
         WARN:       4\n\
         INFO:       0\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Whether `line` is a console log line: `YYYY-MM-DD HH:MM:SS,mmm ` and then `rest`.
fn is_log_line(line: &str, rest: &str) -> bool {
    let Some((stamp, tail)) = line.split_at_checked(24) else { return false };
    let shape = stamp.bytes().zip("0000-00-00 00:00:00,000 ".bytes()).all(|(b, s)| match s {
        b'0' => b.is_ascii_digit(),
        _ => b == s,
    });
    shape && tail == rest
}

/// A report that fails logs it on the console, after the rows it prints, and
/// says nothing on stderr; the run exits 1. The report file is written all the
/// same.
#[test]
fn report_logs_its_failure_on_the_console() {
    let dir = tmp("report-fail");
    std::fs::create_dir_all(&dir).unwrap();
    let ont = dir.join("test.obo");
    // No description, license or title: three ERROR violations under the default
    // profile.
    std::fs::write(&ont, "format-version: 1.4\nontology: ex\n\n[Term]\nid: EX:0000001\nname: root thing\n")
        .unwrap();
    let failed = "ERROR org.obolibrary.robot.ReportCommand - Report failed!";

    let out = bin().args(["report", "-i"]).arg(&ont).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.contains(&"ERROR:      3"), "{stdout}");
    assert!(lines[lines.len() - 2].starts_with("INFO\tmissing_superclass\t"), "{stdout}");
    assert!(is_log_line(lines[lines.len() - 1], failed), "{stdout}");

    let tsv = dir.join("report.tsv");
    let out = bin().args(["report", "-i"]).arg(&ont).arg("-o").arg(&tsv).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(is_log_line(stdout.lines().last().unwrap(), failed), "{stdout}");
    assert!(std::fs::read_to_string(&tsv).unwrap().contains("ERROR\tmissing_ontology_title\t"));

    // Below the threshold nothing is logged and the run succeeds.
    let out = bin().args(["report", "-i"]).arg(&ont).args(["--fail-on", "none"]).output().unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("Report failed!"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A row that binds no `?property` is a violation with no statement: it counts,
/// and has no row in the table, printed or written. YAML and JSON give its
/// subject alone.
#[test]
fn report_counts_a_violation_without_a_property_and_lists_no_statement() {
    let dir = tmp("report-noprop");
    std::fs::create_dir_all(&dir).unwrap();
    let ont = dir.join("test.obo");
    std::fs::write(&ont, "format-version: 1.4\nontology: ex\n\n[Term]\nid: EX:0000001\nname: root thing\n")
        .unwrap();
    let query = dir.join("noprop.rq");
    std::fs::write(
        &query,
        "PREFIX owl: <http://www.w3.org/2002/07/owl#>\n\
         SELECT ?entity ?value WHERE { ?entity a owl:Class . BIND(\"x\" AS ?value) }\n",
    )
    .unwrap();
    let profile = dir.join("profile.txt");
    std::fs::write(&profile, format!("WARN\tfile:{}\n", query.display())).unwrap();
    let report = |out: Option<&str>| {
        let mut cmd = bin();
        cmd.args(["report", "-i"]).arg(&ont).arg("--profile").arg(&profile);
        if let Some(name) = out {
            cmd.arg("-o").arg(dir.join(name));
        }
        let run = cmd.output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        String::from_utf8_lossy(&run.stdout).into_owned()
    };

    assert_eq!(
        report(None),
        "WARN: 'noprop' query is missing ?property variable\n\
         Violations: 1\n\
         -----------------\n\
         ERROR:      0\n\
         WARN:       1\n\
         INFO:       0\n\
         \n\
         First 0 violations:\n"
    );
    report(Some("r.tsv"));
    assert_eq!(std::fs::read_to_string(dir.join("r.tsv")).unwrap(), "Level\tRule Name\tSubject\tProperty\tValue\n");
    report(Some("r.yaml"));
    assert_eq!(
        std::fs::read_to_string(dir.join("r.yaml")).unwrap(),
        "- level: 'WARN'\n  violations:\n  - noprop:\n    - subject: \"obo:EX_0000001\"\n"
    );
    report(Some("r.json"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("r.json")).unwrap()).unwrap();
    assert_eq!(json[0]["violations"][0]["noprop"], serde_json::json!([{ "subject": "obo:EX_0000001" }]));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A query that does not project `?entity` is warned about like any other
/// missing variable. It stops the report only when one of its rows comes back,
/// as such a row names nothing to report.
#[test]
fn report_fails_on_a_row_without_an_entity() {
    let dir = tmp("report-noentity");
    std::fs::create_dir_all(&dir).unwrap();
    let ont = dir.join("test.obo");
    std::fs::write(&ont, "format-version: 1.4\nontology: ex\n\n[Term]\nid: EX:0000001\nname: root thing\n")
        .unwrap();
    let report = |name: &str, pattern: &str| {
        let query = dir.join(format!("{name}.rq"));
        std::fs::write(
            &query,
            format!("PREFIX owl: <http://www.w3.org/2002/07/owl#>\nSELECT ?thing WHERE {{ ?thing a {pattern} }}\n"),
        )
        .unwrap();
        let profile = dir.join(format!("{name}.txt"));
        std::fs::write(&profile, format!("WARN\tfile:{}\n", query.display())).unwrap();
        bin().args(["report", "-i"]).arg(&ont).arg("--profile").arg(&profile).output().unwrap()
    };

    let out = report("nothing", "owl:Nothing");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "WARN: 'nothing' query is missing ?entity variable\n\
         WARN: 'nothing' query is missing ?property variable\n\
         WARN: 'nothing' query is missing ?value variable\n\
         No violations found.\n\
         \n\
         First 0 violations:\n"
    );

    let out = report("classes", "owl:Class");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("MISSING ENTITY BINDING query 'classes' must include an '?entity'"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A source in `tests/fixtures/robot-1.9.11/`, where each sits beside what ROBOT
/// 1.9.11 writes from it.
fn robot_fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11").join(name)
}

/// `om convert` a fixture with `args`, to a file named `out` (whose extension
/// picks the format); the text written.
fn convert_fixture(src: &str, out: &str, args: &[&str]) -> String {
    let path = tmp(out);
    let run = bin().args(["convert", "-i"]).arg(robot_fixture(src)).args(args).arg("-o").arg(&path).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    text
}

fn fixture_text(name: &str) -> String {
    std::fs::read_to_string(robot_fixture(name)).unwrap()
}

/// Runs `cmd` to completion, failing the test once it has run for a minute.
/// Its output goes to files, so a full pipe cannot stall it.
fn output_within_a_minute(mut cmd: Command, tag: &str) -> std::process::Output {
    let (out, err) = (tmp(&format!("{tag}.stdout")), tmp(&format!("{tag}.stderr")));
    let mut child = cmd
        .stdout(std::fs::File::create(&out).unwrap())
        .stderr(std::fs::File::create(&err).unwrap())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > std::time::Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{tag}: still running after a minute");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let output = std::process::Output { status, stdout: std::fs::read(&out).unwrap(), stderr: std::fs::read(&err).unwrap() };
    let _ = (std::fs::remove_file(&out), std::fs::remove_file(&err));
    output
}

/// `om export` a fixture with `args` to a file named `out`, whose extension picks
/// the format when `args` names none; the bytes written.
fn export_fixture(src: &str, out: &str, args: &[&str]) -> Vec<u8> {
    let path = tmp(out);
    let run = bin().args(["export", "-i"]).arg(robot_fixture(src)).args(args).arg("--export").arg(&path).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    bytes
}

/// [`export_fixture`] as text.
fn export_text(src: &str, out: &str, args: &[&str]) -> String {
    String::from_utf8(export_fixture(src, out, args)).unwrap()
}

/// The prefixes the `export-terms` fixtures are written with.
const EXPORT_PREFIXES: [&str; 4] =
    ["--prefix", "EX: http://purl.obolibrary.org/obo/EX_", "--prefix", "ex: http://example.org/"];

/// `args` after the `export-terms` prefixes.
fn with_export_prefixes<'a>(args: &[&'a str]) -> Vec<&'a str> {
    EXPORT_PREFIXES.iter().copied().chain(args.iter().copied()).collect()
}

/// One part of a ZIP archive, inflated: the archive's entries are read in turn
/// from their local headers.
fn zip_part(zip: &[u8], name: &str) -> Option<String> {
    use std::io::Read;
    let u16_at = |i: usize| u16::from_le_bytes([zip[i], zip[i + 1]]) as usize;
    let u32_at = |i: usize| u32::from_le_bytes([zip[i], zip[i + 1], zip[i + 2], zip[i + 3]]) as usize;
    let mut i = 0;
    while i + 30 <= zip.len() && zip[i..i + 4] == [0x50, 0x4b, 0x03, 0x04] {
        let (method, size, name_len, extra_len) = (u16_at(i + 8), u32_at(i + 18), u16_at(i + 26), u16_at(i + 28));
        let entry = &zip[i + 30..i + 30 + name_len];
        let data = &zip[i + 30 + name_len + extra_len..i + 30 + name_len + extra_len + size];
        if entry == name.as_bytes() {
            let mut text = String::new();
            match method {
                0 => text = String::from_utf8(data.to_vec()).unwrap(),
                _ => {
                    flate2::read::DeflateDecoder::new(data).read_to_string(&mut text).unwrap();
                }
            }
            return Some(text);
        }
        i += 30 + name_len + extra_len + size;
    }
    None
}

/// Entities render as ROBOT 1.9.11 renders them under each entity format: by
/// name (the label, quoted inside an expression when it holds a space, else the
/// CURIE), by label (empty without one) and by CURIE. Expressions are one line
/// with runs of spaces collapsed, an inverse property keeps the space its
/// keyword opens with, `owl:Thing` is no superclass, a superclass stated twice
/// is listed twice, and every keyword column — subclasses, equivalents,
/// disjoints, superproperties, domains, ranges, types, synonyms — holds what
/// ROBOT's does, for classes, properties and individuals.
#[test]
fn export_renders_entities_as_robot_does_by_name_label_and_id() {
    let all = "ID|LABEL|SubClass Of|Equivalent Class|Disjoint With|SubProperty Of|Domain|Range|Type|SYNONYMS|SubClasses|Equivalent Property";
    for (format, expected) in [
        (None, "export-terms.name.tsv"),
        (Some("LABEL"), "export-terms.label.tsv"),
        (Some("ID"), "export-terms.id.tsv"),
    ] {
        let mut args = with_export_prefixes(&["-n", "classes properties individuals", "-c", all]);
        if let Some(f) = format {
            args.extend(["-E", f]);
        }
        assert_eq!(export_text("export-terms.ofn", expected, &args), fixture_text(expected), "{expected}");
    }
}

/// A header names a property by its label — spaces and all — or by CURIE: an
/// object or data property named by label lists the fillers of the restrictions
/// on it and an individual's values of it, while a header that expands to an IRI
/// is an annotation property column, even for an IRI that names an object or
/// data property or none at all. `rdf:type` is the type column. As ROBOT 1.9.11
/// writes `export-terms.columns.tsv`.
#[test]
fn export_resolves_property_columns_as_robot_does() {
    let args = with_export_prefixes(&[
        "-n",
        "classes individuals",
        "-c",
        "ID|part of|has  two   spaces|weight|see also|EX:ap|BFO:0000051|EX:dp|rdfs:subClassOf|rdf:type|obo",
    ]);
    assert_eq!(
        export_text("export-terms.ofn", "export-terms.columns.tsv", &args),
        fixture_text("export-terms.columns.tsv")
    );
}

/// A column's tag sets how it renders entities and which values it holds, in
/// either case; `ID`, `CURIE`, `IRI` and `LABEL` columns keep their own
/// rendering whatever the tag says of an expression. With no `--include`,
/// individuals have rows as classes do. As ROBOT 1.9.11 writes
/// `export-terms.tags.tsv`.
#[test]
fn export_column_tags_set_rendering_and_selection() {
    let args = with_export_prefixes(&[
        "-c",
        "ID [NAME]|ID [LABEL]|ID [IRI]|CURIE|IRI [ID]|LABEL [ID]|label|SubClass Of [NAMED]|SubClass Of [ANON]|SubClass Of [id anon]",
    ]);
    assert_eq!(export_text("export-terms.ofn", "export-terms.tags.tsv", &args), fixture_text("export-terms.tags.tsv"));
}

/// Rows the sort leaves tied stay in the order the entity set holds them — by
/// hash bucket, then by bucket of the larger set they were gathered from, then
/// kind by kind in IRI order — which a column empty for every row exposes
/// whole: here three buckets hold two entities or more, and two pairs share
/// both. As ROBOT 1.9.11 writes `export-terms.ties.tsv`.
#[test]
fn export_keeps_tied_rows_in_the_entity_sets_order() {
    let args = ["-n", "classes properties individuals", "-c", "BFO:0000051|ID [IRI]|Type [IRI]"];
    assert_eq!(export_text("export-terms.ofn", "export-terms.ties.tsv", &args), fixture_text("export-terms.ties.tsv"));
}

/// A Turtle document types its untyped literals `xsd:string`, and so sorts an
/// axiom's annotations by that datatype: `"q"` after `"7"^^xsd:integer`, where
/// a document that keeps them untyped puts it first. The hash of `:A`'s label
/// `v`, annotated with both, decides which of `:A`'s two labels is its label.
/// As ROBOT 1.9.11 writes `export-typed-label-annotations.robot.tsv`.
#[test]
fn export_picks_a_label_by_the_documents_literal_order() {
    let args = ["--header", "ID|LABEL"];
    assert_eq!(
        export_text("export-typed-label-annotations.ttl", "export-typed-label-annotations.tsv", &args),
        fixture_text("export-typed-label-annotations.robot.tsv")
    );
}

/// `--sort` names several columns, `^` reverses one, and the last named orders
/// the rows while the earlier ones break its ties; an empty value sorts last,
/// or first in reverse. As ROBOT 1.9.11 writes `export-terms.sort.tsv`.
#[test]
fn export_sorts_on_several_columns_and_in_reverse() {
    let args = with_export_prefixes(&["-n", "classes", "-c", "ID|LABEL|SubClass Of", "-s", "^LABEL|SubClass Of"]);
    assert_eq!(export_text("export-terms.ofn", "export-terms.sort.tsv", &args), fixture_text("export-terms.sort.tsv"));
}

/// HTML links every entity and escapes its name, JSON writes `ID`, `CURIE` and
/// `IRI` as single values and every other column as an array, and CSV quotes as
/// TSV does; the format is read from the file's extension. As ROBOT 1.9.11
/// writes each.
#[test]
fn export_writes_html_json_and_csv_as_robot_does() {
    let html = with_export_prefixes(&[
        "-n",
        "classes properties individuals",
        "-c",
        "ID|LABEL|SubClass Of|Disjoint With|Type|see also|weight|part of|IRI",
    ]);
    assert_eq!(export_text("export-terms.ofn", "export-terms.html", &html), fixture_text("export-terms.html"));
    let json = with_export_prefixes(&[
        "-n",
        "classes properties individuals",
        "-c",
        "ID|LABEL|SubClass Of|Disjoint With|Type|see also|weight|part of|IRI|CURIE",
    ]);
    assert_eq!(export_text("export-terms.ofn", "export-terms.json", &json), fixture_text("export-terms.json"));
    let csv = with_export_prefixes(&["-n", "classes properties individuals", "-c", "ID|LABEL|SubClass Of|SYNONYMS", "-S", " | "]);
    assert_eq!(export_text("export-terms.ofn", "export-terms.csv", &csv), fixture_text("export-terms.csv"));
}

/// An `.xlsx` export is a workbook whose sheet and shared strings are those
/// ROBOT 1.9.11 writes, and the same rows make the same bytes.
#[test]
fn export_writes_a_workbook_with_robots_sheet() {
    let args = with_export_prefixes(&[
        "-n",
        "classes properties individuals",
        "-c",
        "ID|LABEL|SubClass Of|SYNONYMS|see also|weight",
    ]);
    let book = export_fixture("export-terms.ofn", "export-terms.xlsx", &args);
    assert_eq!(zip_part(&book, "xl/worksheets/sheet1.xml").unwrap(), fixture_text("export-terms.xlsx.sheet1.xml"));
    assert_eq!(zip_part(&book, "xl/sharedStrings.xml").unwrap(), fixture_text("export-terms.xlsx.sharedStrings.xml"));
    assert_eq!(book, export_fixture("export-terms.ofn", "export-terms.xlsx", &args));
}

/// IRIs are shortened against the built-in prefix map and the prefixes the
/// command line binds — never the document's own — the longest namespace
/// winning; `--noprefixes` leaves only the ones bound on the command line, and
/// an IRI no namespace starts stays whole. `owl:Thing` has no row. As ROBOT
/// 1.9.11 writes each.
#[test]
fn export_shortens_iris_with_the_built_in_and_given_prefixes() {
    let base = ["-c", "ID|SubClass Of", "-E", "ID"];
    assert_eq!(
        export_text("export-prefixes.ofn", "export-prefixes.default.tsv", &base),
        fixture_text("export-prefixes.default.tsv")
    );
    let none: Vec<&str> = base.iter().copied().chain(["--noprefixes"]).collect();
    assert_eq!(
        export_text("export-prefixes.ofn", "export-prefixes.noprefixes.tsv", &none),
        fixture_text("export-prefixes.noprefixes.tsv")
    );
    let ex: Vec<&str> = base.iter().copied().chain(["--prefix", "ex: http://example.org/ns/"]).collect();
    assert_eq!(export_text("export-prefixes.ofn", "export-prefixes.ex.tsv", &ex), fixture_text("export-prefixes.ex.tsv"));
}

/// A TSV or CSV cell holding a quote, its delimiter or a line break is quoted,
/// its quotes doubled, so a tab in a label never moves the cells after it; JSON
/// escapes control characters. As ROBOT 1.9.11 writes each.
#[test]
fn export_quotes_cells_holding_quotes_delimiters_and_line_breaks() {
    let args = ["-c", "ID|LABEL", "--prefix", "EX: http://purl.obolibrary.org/obo/EX_"];
    for out in ["export-quoting.tsv", "export-quoting.csv", "export-quoting.json"] {
        assert_eq!(export_fixture("export-quoting.ofn", out, &args), std::fs::read(robot_fixture(out)).unwrap(), "{out}");
    }
}

/// What ROBOT 1.9.11 refuses, `export` refuses, writing nothing: no header, a
/// column that names nothing, an unknown entity format, selection, output format
/// or tag, two format tags, an `--include` that names no kind, a sort column
/// some rows have no value in, and a sort position that names no column.
#[test]
fn export_refuses_what_robot_refuses() {
    let src = robot_fixture("export-terms.ofn");
    for (args, says) in [
        (vec![], "--header is a required option"),
        (vec!["-c", "ID|no such column"], "unable to find property for column header 'no such column'"),
        (vec!["-c", "ID|Definition"], "unable to find property for column header 'Definition'"),
        (vec!["-c", "ID", "-E", "CURIE"], "'CURIE' is not a valid entity rendering format"),
        (vec!["-c", "ID", "-l", "ALL"], "'ALL' is not a valid entity selection"),
        (vec!["-c", "ID", "-f", "yaml"], "--format yaml must be one of"),
        (vec!["-c", "ID", "-f", "TSV"], "--format TSV must be one of"),
        (vec!["-c", "ID|SubClass Of [CURIE]"], "unknown rendering tag: CURIE"),
        (vec!["-c", "ID|SubClass Of [ID IRI]"], "more than one entity format tag"),
        (vec!["-c", "ID|SubClass Of [NAMED ANON]"], "more than one entity selection tag"),
        (vec!["-c", "ID", "-n", "things"], "names no kind of entity"),
        (vec!["-c", "ID|SubClass Of", "-s", "SubClass Of"], "cannot sort on column 'SubClass Of'"),
        (vec!["-c", "ID|LABEL", "-s", "label"], "sort position 1 names no column"),
        (vec!["-c", "ID|LABEL", "-s", "nosuch|LABEL"], "sort position 1 names no column"),
    ] {
        let out = tmp("export-refused.tsv");
        let _ = std::fs::remove_file(&out);
        let run = bin().args(["export", "-i"]).arg(&src).args(&args).arg("--export").arg(&out).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success(), "{args:?} succeeded");
        assert!(stderr.contains(says), "{args:?}: {stderr}");
        assert!(!out.exists(), "{args:?} wrote {}", out.display());
    }
}

/// Manchester syntax keeps what an ontology says about its terms — every
/// annotation assertion, the annotations on them, annotation property frames,
/// IRI and typed values, the ontology IRI — and reads all of it back. As ROBOT
/// 1.9.11 writes and reads `annotated-terms`, a Turtle source whose untyped
/// literals are `xsd:string`.
#[test]
fn manchester_keeps_annotations_and_reads_them_back() {
    assert_eq!(
        convert_fixture("annotated-terms.ttl", "annotated-terms.omn", &[]),
        fixture_text("annotated-terms.omn")
    );
    assert_eq!(
        convert_fixture("annotated-terms.omn", "annotated-terms.ofn", &[]),
        fixture_text("annotated-terms.omn.ofn")
    );
}

/// A prefix the command line adds is declared by every prefix format, used or
/// not, and abbreviates the IRIs it covers; a context given with `-P` only reads
/// CURIEs. As ROBOT 1.9.11 writes `prefixed-terms` and `anonymous-terms`.
#[test]
fn added_prefixes_are_declared_and_used_by_every_prefix_format() {
    let obo = ["--add-prefix", "obo: http://purl.obolibrary.org/obo/"];
    for ext in ["ofn", "owx", "omn", "ttl"] {
        assert_eq!(
            convert_fixture("prefixed-terms.ttl", &format!("added.{ext}"), &obo),
            fixture_text(&format!("prefixed-terms.add-obo.{ext}")),
            "{ext}"
        );
    }
    assert_eq!(
        convert_fixture("prefixed-terms.ttl", "unused.ofn", &["--add-prefix", "foo: http://example.org/foo/"]),
        fixture_text("prefixed-terms.add-unused.ofn")
    );
    let context = robot_fixture("obo-context.json");
    let read_only = ["-P", context.to_str().unwrap()];
    for ext in ["ofn", "ttl"] {
        assert_eq!(
            convert_fixture("prefixed-terms.ttl", &format!("read-only.{ext}"), &read_only),
            fixture_text(&format!("prefixed-terms.read-only.{ext}")),
            "{ext}"
        );
    }
    assert_eq!(
        convert_fixture(
            "anonymous-terms.ttl",
            "anonymous.ttl",
            &["--add-prefix", "obo: http://purl.obolibrary.org/obo/", "--add-prefix", "foo: http://example.org/foo/"]
        ),
        fixture_text("anonymous-terms.add-prefixes.ttl")
    );
}

/// OWL/XML names an IRI by prefix only in `abbreviatedIRI`, and only with a
/// prefix whose namespace is the IRI's own. An `IRI` value is an IRI — absolute,
/// or relative to `xml:base` — and one that looks like a CURIE names the IRI it
/// spells. As ROBOT 1.9.11 writes `prefixed-terms` and reads
/// `curie-iri-attributes`.
#[test]
fn owlxml_names_an_iri_by_prefix_only_in_abbreviated_iri() {
    assert_eq!(convert_fixture("prefixed-terms.ttl", "plain.owx", &[]), fixture_text("prefixed-terms.owx"));
    assert_eq!(
        convert_fixture("curie-iri-attributes.owx", "curie-iri-attributes.ofn", &[]),
        fixture_text("curie-iri-attributes.ofn")
    );
}

/// The `idspace:` lines an OBO document gets from a Turtle source are the
/// prefixes it binds, less the built-in vocabularies and the OBO PURL space — as
/// for an RDF/XML source's `xmlns:` bindings. As ROBOT 1.9.11 writes
/// `prefixed-terms` and `custom-prefixes`.
#[test]
fn a_turtle_sources_prefixes_become_idspaces_as_xmlns_bindings_do() {
    assert_eq!(convert_fixture("prefixed-terms.ttl", "prefixed.obo", &[]), fixture_text("prefixed-terms.obo"));
    assert_eq!(convert_fixture("custom-prefixes.ttl", "custom.obo", &[]), fixture_text("custom-prefixes.obo"));
}

/// An OBO document read as ROBOT 1.9.11 reads it: every header tag kept (the
/// ones without a rule of their own as `oboInOwl:` ontology annotations), a
/// synonym type's scope, every boolean tag whether `true` or `false`, and
/// declarations for the properties the OBO vocabulary names but not for the ones
/// the document only uses — and written back out with the same tags.
#[test]
fn obo_tags_are_read_and_written_as_robot_does() {
    for ext in ["ofn", "owx", "owl"] {
        assert_eq!(
            convert_fixture("obo-terms.obo", &format!("obo-terms.{ext}"), &[]),
            fixture_text(&format!("obo-terms.{ext}")),
            "{ext}"
        );
    }
    for ext in ["ofn", "owx", "obo"] {
        assert_eq!(
            convert_fixture("obo-tags.obo", &format!("obo-tags.out.{ext}"), &[]),
            fixture_text(&format!("obo-tags.out.{ext}")),
            "{ext}"
        );
    }
}

/// An OBO document's `idspace:` lines, and the prefixes its ids are shortened
/// with, are the ones its source declared: a Turtle or RDF/XML source's
/// prefixes, an OBO source's own `idspace:` lines, less the OBO PURL and
/// built-in namespaces either way. A prefix the command line adds joins them
/// only when the document is cleaned. With `--clean-obo`, every added prefix
/// gets an `idspace:`, used or not, and shortens ids. Without it,
/// `--add-prefix` and `--add-prefixes` leave the OBO document as it was. As
/// ROBOT 1.9.11 writes all three sources.
#[test]
fn added_prefixes_become_obo_idspaces_only_when_cleaned() {
    let context = robot_fixture("obo-prefixes.json");
    let context = context.to_str().unwrap();
    let baz = "baz: http://example.org/baz/";
    for src in ["ttl", "owl", "obo"] {
        let input = format!("obo-prefixes.{src}");
        let out = format!("obo-prefixes-{src}.obo");
        let plain = fixture_text(&format!("obo-prefixes.{src}.out.obo"));
        for args in [&[][..], &["--add-prefixes", context], &["--add-prefix", baz], &["--clean-obo", "strict"]] {
            assert_eq!(convert_fixture(&input, &out, args), plain, "{src} {args:?}");
        }
        assert_eq!(
            convert_fixture(&input, &out, &["--add-prefixes", context, "--clean-obo", "strict"]),
            fixture_text(&format!("obo-prefixes.{src}.add-prefixes-clean.obo")),
            "{src}"
        );
        assert_eq!(
            convert_fixture(&input, &out, &["--add-prefix", baz, "--clean-obo", "strict"]),
            fixture_text(&format!("obo-prefixes.{src}.add-prefix-clean.obo")),
            "{src}"
        );
    }
}

/// General class axioms are written one frame per subclass expression, in the
/// order a hash map keyed by those expressions holds them: by bucket, and
/// within a bucket last-first. An individual in the expression is hashed as a
/// named individual, as a set member in a one-of, and by node id when it is
/// anonymous. As ROBOT 1.9.11 writes them.
#[test]
fn general_class_axioms_are_framed_in_hash_map_order() {
    for name in ["gci-individuals", "gci-anonymous"] {
        assert_eq!(
            convert_fixture(&format!("{name}.ofn"), &format!("{name}.omn"), &[]),
            fixture_text(&format!("{name}.omn")),
            "{name}"
        );
    }
}

/// An individual is typed first with its declared type, then with the classes
/// asserted for it in IRI order — `owl:Thing` among them, though RDF/XML names
/// the element after it — then with the asserted expressions. As ROBOT 1.9.11
/// writes `typed-individual` in RDF/XML and in Turtle.
#[test]
fn an_individual_lists_its_declared_type_first() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("typed-individual.ofn", &format!("typed-individual.{ext}"), &[]),
            fixture_text(&format!("typed-individual.{ext}")),
            "{ext}"
        );
    }
}

/// Every property axiom is stated where ROBOT 1.9.11 states it, in RDF/XML and
/// in Turtle: an annotated one reifies after its subject's block (an annotated
/// anonymous domain or range as a node of its own), a binary one is stated of
/// its first member in order, each axiom about an inverse property is a node of
/// its own after the property, a datatype's definition follows its annotations,
/// and the members of a data range, a one-of and a facet list come in order. The
/// closing general axiom's node id pins the numbering of every node before it.
#[test]
fn property_axioms_are_written_as_robot_writes_them() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-property-axioms.ofn", &format!("rdf-property-axioms.{ext}"), &[]),
            fixture_text(&format!("rdf-property-axioms.{ext}")),
            "{ext}"
        );
    }
}

/// Every individual axiom is stated where ROBOT 1.9.11 states it, in RDF/XML
/// and in Turtle: annotated class and property assertions reify after the
/// individual, an assertion on an inverse is stated the other way round, a pair
/// of individuals is stated of the first, and an anonymous individual's own
/// statements form its block. The closing general axiom's node id pins the
/// numbering of every node before it.
#[test]
fn individual_axioms_are_written_as_robot_writes_them() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-individual-axioms.ofn", &format!("rdf-individual-axioms.{ext}"), &[]),
            fixture_text(&format!("rdf-individual-axioms.{ext}")),
            "{ext}"
        );
    }
}

/// Axioms about inverse properties, keys, disjoint unions and annotated negative
/// assertions are stated where ROBOT 1.9.11 states them, in RDF/XML and in
/// Turtle. An annotated axiom about an inverse reifies with the inverse's node
/// nested as its source; a named property's annotated edge to an inverse names
/// the inverse by id; a key's inverse member is nested in its list; an
/// annotated key or disjoint union reifies with its list as the target, a list
/// Turtle names by id. The closing general axiom's node id pins the numbering
/// of every node before it.
#[test]
fn inverse_property_axioms_keys_and_unions_are_written_as_robot_writes_them() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-inverse-axioms.ofn", &format!("rdf-inverse-axioms.{ext}"), &[]),
            fixture_text(&format!("rdf-inverse-axioms.{ext}")),
            "{ext}"
        );
    }
}

/// Equivalences and samenesses of three or more members are stated as ROBOT
/// 1.9.11 states them, in RDF/XML and in Turtle: one triple per consecutive pair
/// of the ordered members, every named member after the first a block of its
/// own, and, annotated, every pair reified with its anonymous members defined by
/// id. Equivalences of classes and of object properties take their pairs in
/// order, those of data properties and samenesses in the order of a hash set of
/// the pairs, which this document's members do not share with their order. An
/// equivalence with no named class is a general axiom, and one whose inverse
/// member's property comes first is stated after that property.
#[test]
fn equivalences_and_samenesses_of_three_or_more_are_written_as_robot_writes_them() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-nary-axioms.ofn", &format!("rdf-nary-axioms.{ext}"), &[]),
            fixture_text(&format!("rdf-nary-axioms.{ext}")),
            "{ext}"
        );
    }
}

/// Annotations of annotations are stated as ROBOT 1.9.11 states them, in
/// RDF/XML and in Turtle: each annotated annotation is an `owl:Annotation` node
/// whose source is what it annotates, the roots of the graph are those whose own
/// annotations are not annotated, and the axiom's node is named by id after the
/// first of them. The pairs of a sameness share one set of such nodes, and the
/// ontology's come after its header in the order of a hash set of its triples.
/// The annotated axioms are on classes, individuals and an undeclared IRI, and
/// two are general axioms. Assertions of one statement that differ only in
/// their annotations are reified in the order of their annotations.
#[test]
fn annotations_of_annotations_are_written_as_robot_writes_them() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-nested-annotations.ofn", &format!("rdf-nested-annotations.{ext}"), &[]),
            fixture_text(&format!("rdf-nested-annotations.{ext}")),
            "{ext}"
        );
    }
}

/// Anonymous individuals are written as ROBOT 1.9.11 writes them, in RDF/XML
/// and in Turtle: each individual's statements are made in the first graph to
/// reach it — an entity's, the ontology's, a general axiom's, or its own in the
/// anonymous section — nested where it is an object, or named by id and
/// defined after the first block naming it when the document names it twice or
/// the graph names it as the object of two statements. Its reifications and
/// negative assertions are roots of that graph; an `owl:AllDifferent` reached
/// first from one of its members is too.
#[test]
fn anonymous_individuals_are_written_as_robot_writes_them() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-anonymous-individuals.ofn", &format!("rdf-anonymous-individuals.{ext}"), &[]),
            fixture_text(&format!("rdf-anonymous-individuals.{ext}")),
            "{ext}"
        );
    }
}

/// An anonymous individual in an annotation of an annotation is written as
/// ROBOT 1.9.11 writes it, in RDF/XML and in Turtle: as the value of an
/// annotation on the node of an annotated annotation, nested with what it is
/// stated to be, and as the value of an annotated annotation, named by id in
/// the axiom's node and as the `owl:annotatedTarget` of the annotation's. The
/// annotations are on the ontology, on assertions, class and property axioms,
/// a general axiom, n-ary axioms, a negative assertion and a rule.
#[test]
fn anonymous_individuals_in_annotations_of_annotations_are_written_as_robot_writes_them() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-nested-anonymous.ofn", &format!("rdf-nested-anonymous.{ext}"), &[]),
            fixture_text(&format!("rdf-nested-anonymous.{ext}")),
            "{ext}"
        );
    }
}

/// A Turtle document's anonymous individuals are numbered in the order the
/// reader translates them, which follows the names its blank nodes take and
/// the order the parse states the statements in: the statement naming a
/// `[ … ]` object before the statements inside it. As ROBOT 1.9.11 converts
/// `rdf-anonymous-individuals`, `rdf-nested-anonymous` and
/// `rdf-nested-annotations` from Turtle to functional syntax with the blank
/// nodes named as owlmake names them (`scripts/gen_robot_turtle_fixture.sh`).
#[test]
fn a_turtle_documents_anonymous_individuals_are_numbered_as_the_reader_translates_them() {
    for stem in ["rdf-anonymous-individuals", "rdf-nested-anonymous", "rdf-nested-annotations"] {
        assert_eq!(
            convert_fixture(&format!("{stem}.ttl"), &format!("{stem}.ttl.ofn"), &[]),
            fixture_text(&format!("{stem}.ttl.robot.ofn")),
            "{stem}"
        );
    }
}

/// A statement a document makes twice is one statement: the reification of a
/// property assertion between anonymous individuals annotates the one
/// assertion, which is not kept again unannotated. As ROBOT 1.9.11 converts
/// `rdf-repeated-statement` from RDF/XML and from Turtle (the latter with the
/// blank nodes named as owlmake names them) to functional syntax.
#[test]
fn a_statement_made_twice_is_one_statement() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture(&format!("rdf-repeated-statement.{ext}"), &format!("rdf-repeated-statement.{ext}.ofn"), &[]),
            fixture_text(&format!("rdf-repeated-statement.{ext}.robot.ofn")),
            "{ext}"
        );
    }
}

/// A relative reference in a Turtle document has each character an IRI may
/// not hold percent-encoded before it resolves: `{fixtures}` against the
/// document's own IRI is `…/%7Bfixtures%7D`, and against a base that already
/// holds it, twice over. As ROBOT 1.9.11 converts ROBOT's own Turtle of
/// `rdf-document-base` and `ttl-document-base`, whose IRIs carry the
/// `{fixtures}` placeholder, to functional syntax; the recorded outputs name
/// the fixture directory `{fixtures}`.
#[test]
fn a_relative_reference_has_its_refused_characters_encoded() {
    let fixtures = format!("file:{}/", robot_fixture("").display().to_string().trim_end_matches('/'));
    for stem in ["rdf-document-base", "ttl-document-base"] {
        assert_eq!(
            convert_fixture(&format!("{stem}.robot.ttl"), &format!("{stem}.robot.ttl.ofn"), &[])
                .replace(&fixtures, "{fixtures}/"),
            fixture_text(&format!("{stem}.robot.ttl.robot.ofn")),
            "{stem}"
        );
    }
}

/// A document read from RDF/XML or Turtle writes its anonymous individuals
/// from the model, as ROBOT 1.9.11 writes the document it reads: one typed
/// `owl:Thing` and a class, as the value of an entity's annotation, of an
/// axiom's, of an annotated annotation and of the ontology's, as the object of
/// assertions, of a value restriction in a general axiom and of an assertion on
/// an inverse property, and in an annotated difference; and the shapes of
/// `read-anonymous` and `rdf-individual-axioms`.
#[test]
fn anonymous_individuals_read_from_rdf_are_written_as_robot_writes_them() {
    for name in ["rdf-anonymous-rewrite", "read-anonymous", "rdf-individual-axioms"] {
        for src in ["owl", "ttl"] {
            assert_eq!(
                convert_fixture(&format!("{name}.{src}"), &format!("{name}-{src}.owl"), &[]),
                fixture_text(&format!("{name}.owl")),
                "{name}.{src} to RDF/XML"
            );
        }
    }
    for (src, expected) in [("owl", "rdf-anonymous-rewrite.owl.ttl"), ("ttl", "rdf-anonymous-rewrite.ttl")] {
        assert_eq!(
            convert_fixture(&format!("rdf-anonymous-rewrite.{src}"), &format!("rdf-anonymous-rewrite-{src}.ttl"), &[]),
            fixture_text(expected),
            "rdf-anonymous-rewrite.{src} to Turtle"
        );
    }
}

/// An RDF/XML document's anonymous individuals are named in the order the
/// reader translates them, as ROBOT 1.9.11 names them: one typed with a named
/// class, or the same as or different from another, as the parse reaches that
/// statement; the rest once the document is complete, pass by pass, in the hash
/// order of the reader's tables. The names are the `_:genid` numbers in
/// functional syntax and the order of the anonymous section in RDF/XML and
/// Turtle.
#[test]
fn an_rdfxml_documents_anonymous_individuals_are_named_as_robot_reads_them() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-anonymous-order.rdf", &format!("rdf-anonymous-order.{ext}"), &[]),
            fixture_text(&format!("rdf-anonymous-order.robot.{ext}")),
            "{ext}"
        );
    }
}

/// An annotation of the ontology takes the annotations of the first node that
/// names it, in the order the reader reads them: an `owl:Annotation` node, or an
/// `owl:Axiom` block naming the ontology as its source, with a literal typed
/// `xsd:string` naming the untyped one. The other nodes are not read. An
/// annotation whose value is an IRI is read bare, and so is one only a block
/// states. The ontology is then given a bare copy of each later statement of
/// the first property the reader reaches, as stated, and of each block with a
/// literal target, as the block states it, and it holds of them what it holds
/// of what it is given: a copy typed `xsd:string` stays beside an untyped
/// annotation it follows, and the RDF writers state the two once. An
/// annotation of an axiom's annotation is matched the same way. As ROBOT
/// 1.9.11 reads `rdf-header-reifications`, `rdf-header-typed-reifications`
/// and `turtle-header-reifications`, in every format it writes.
#[test]
fn reifications_of_ontology_annotations_are_read_as_robot_reads_them() {
    for name in ["rdf-header-reifications", "rdf-header-typed-reifications"] {
        for ext in ["ofn", "owl", "ttl", "owx", "omn", "obo", "json"] {
            assert_eq!(
                convert_fixture(&format!("{name}.rdf"), &format!("{name}.{ext}"), &[]),
                fixture_text(&format!("{name}.robot.{ext}")),
                "{name}.{ext}"
            );
        }
    }
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("turtle-header-reifications.ttl", &format!("turtle-header-reifications.{ext}"), &[]),
            fixture_text(&format!("turtle-header-reifications.robot.{ext}")),
            "{ext}"
        );
    }
}

/// Of two annotations of the ontology with one property and one value, it
/// holds the first it is given, whatever annotations each carries; but an
/// untyped literal and the literal of its text typed `xsd:string` are two
/// values when the typed one is given after the untyped one. A reader gives
/// the ontology its header's annotations in document order, an OBO header's
/// tag by tag as a hash set of its tags iterates them, and `annotate` and
/// `merge --include-annotations` give it theirs after those it holds. As ROBOT
/// 1.9.11 reads `ontology-annotation-duplicates` in functional syntax, OWL/XML
/// and Manchester syntax and `obo-header-duplicates`, annotates the first and
/// merges `merge-annotations-1` and `-2`.
#[test]
fn an_ontology_holds_the_annotations_robot_holds_of_those_it_is_given() {
    for ext in ["ofn", "owx", "omn"] {
        assert_eq!(
            convert_fixture(
                &format!("ontology-annotation-duplicates.{ext}"),
                &format!("ontology-annotation-duplicates-{ext}.ofn"),
                &[]
            ),
            fixture_text(&format!("ontology-annotation-duplicates.{ext}.robot.ofn")),
            "{ext}"
        );
    }
    assert_eq!(
        convert_fixture("obo-header-duplicates.obo", "obo-header-duplicates.ofn", &[]),
        fixture_text("obo-header-duplicates.robot.ofn")
    );

    let annotated = tmp("ontology-annotation-duplicates-annotated.ofn");
    let run = bin()
        .args(["annotate", "-i"])
        .arg(robot_fixture("ontology-annotation-duplicates.ofn"))
        .args(["--annotation", "rdfs:comment", "h", "--annotation", "rdfs:comment", "e", "-o"])
        .arg(&annotated)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        std::fs::read_to_string(&annotated).unwrap(),
        fixture_text("ontology-annotation-duplicates.annotated.robot.ofn")
    );

    let merged = tmp("merge-annotations.ofn");
    let run = bin()
        .args(["merge", "-i"])
        .arg(robot_fixture("merge-annotations-1.ofn"))
        .arg("-i")
        .arg(robot_fixture("merge-annotations-2.ofn"))
        .args(["--include-annotations", "true", "-o"])
        .arg(&merged)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&merged).unwrap(), fixture_text("merge-annotations.robot.ofn"));
}

/// A document in the vocabulary of OWL 1.1 is read in OWL 2's (`owl11:onClass`
/// as `owl:onClass`), and an unqualified cardinality given a filler all the
/// same — `owl:onClass` for an object property, `owl:onDataRange` for a data
/// property — is the qualified one; the OWL 1.1 namespace is not declared in
/// RDF/XML. As ROBOT 1.9.11 reads `rdf-legacy-cardinality`, in functional
/// syntax, RDF/XML and Turtle.
#[test]
fn owl_1_1_vocabulary_and_cardinalities_with_a_filler_are_read_as_robot_reads_them() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-legacy-cardinality.rdf", &format!("rdf-legacy-cardinality.{ext}"), &[]),
            fixture_text(&format!("rdf-legacy-cardinality.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A restriction on a property declared for annotations is read as its filler
/// makes it, an object restriction for a class; and `owl:equivalentProperty`
/// between two properties of no known kind relates nothing, so the statement
/// is left unread rather than failing the document. As ROBOT 1.9.11 reads
/// `rdf-lax-property-kinds`, in functional syntax, RDF/XML and Turtle.
#[test]
fn properties_of_an_unexpected_kind_are_read_as_robot_reads_them() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-lax-property-kinds.rdf", &format!("rdf-lax-property-kinds.{ext}"), &[]),
            fixture_text(&format!("rdf-lax-property-kinds.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A reification that spells the axiom's class expression out again as its
/// `owl:annotatedTarget`, in place of naming the axiom's own node, annotates
/// the axiom, which is read once, annotated. As ROBOT 1.9.11 reads
/// `rdf-reified-copies`, in functional syntax, RDF/XML and Turtle.
#[test]
fn a_reification_that_copies_the_axioms_class_expression_annotates_it() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-reified-copies.rdf", &format!("rdf-reified-copies.{ext}"), &[]),
            fixture_text(&format!("rdf-reified-copies.robot.{ext}")),
            "{ext}"
        );
    }
}

/// Reading a document again names its anonymous individuals afresh, as ROBOT
/// 1.9.11 does: each read labels the blank nodes its own parse makes and names
/// the individuals in the hash order of those labels, so the document written
/// from one read can come back in another order. ROBOT's own `merge` output,
/// `rdf-anonymous-reread.owl`, is written in the reverse order when read again,
/// and that in turn in the first order.
#[test]
fn anonymous_individuals_read_again_are_ordered_as_robot_orders_them() {
    assert_eq!(
        convert_fixture("rdf-anonymous-reread.owl", "rdf-anonymous-reread-1.owl", &[]),
        fixture_text("rdf-anonymous-reread.robot.owl")
    );
    assert_eq!(
        convert_fixture("rdf-anonymous-reread.robot.owl", "rdf-anonymous-reread-2.owl", &[]),
        fixture_text("rdf-anonymous-reread.owl")
    );
}

/// An annotated general axiom that reaches an anonymous individual is the
/// block of the node that reifies it, among the roots of its graph by that
/// node's id: after the axioms about the individual its graph takes in. When
/// its annotation carries an annotation of its own, the `owl:Annotation` node
/// that states it comes first; when an axiom about the individual carries one,
/// that axiom is the node the `owl:Annotation` names, and the general axiom
/// keeps its own block. As ROBOT 1.9.11 converts
/// `general-axioms-reaching-individuals`, its `-nested` twin and
/// `general-axioms-reaching-annotated-assertions`, in RDF/XML and Turtle.
#[test]
fn an_annotated_general_axiom_reaching_an_individual_is_its_reification_block() {
    for name in [
        "general-axioms-reaching-individuals",
        "general-axioms-reaching-individuals-nested",
        "general-axioms-reaching-annotated-assertions",
    ] {
        for ext in ["owl", "ttl"] {
            assert_eq!(
                convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]),
                fixture_text(&format!("{name}.robot.{ext}")),
                "{name}.{ext}"
            );
        }
    }
}

/// Axioms that differ only in their annotations make one statement: a domain,
/// a range, a datatype definition, a class assertion, a sameness, a
/// difference of two or a sub-property of an inverse is one edge, of one node
/// when its value is anonymous, and each annotated axiom reifies that edge, in
/// the order the axioms take. An inverse subject is a node of its own in each
/// axiom. As ROBOT 1.9.11 converts `axiom-twins`, in RDF/XML and Turtle.
#[test]
fn axioms_differing_only_in_annotations_state_one_edge() {
    let name = "axiom-twins";
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]),
            fixture_text(&format!("{name}.robot.{ext}")),
            "{name}.{ext}"
        );
    }
}

/// An equivalence, a superclass and a disjointness over one class expression
/// are statements of three predicates, each of a node of its own, however
/// their axioms are annotated: a plain one is stated beside an annotated one
/// of another predicate, and twins of one predicate share their node. Two
/// predicates share one node only where the source names one node from both.
/// As ROBOT 1.9.11 converts `predicate-twins` and `predicate-twins-shared`, in
/// RDF/XML and Turtle.
#[test]
fn statements_of_two_predicates_share_a_node_only_where_the_source_does() {
    for (name, source) in [
        ("predicate-twins", "predicate-twins.ofn"),
        ("predicate-twins-shared", "predicate-twins-shared.owl"),
    ] {
        for ext in ["owl", "ttl"] {
            assert_eq!(
                convert_fixture(source, &format!("{name}.{ext}"), &[]),
                fixture_text(&format!("{name}.robot.{ext}")),
                "{name}.{ext}"
            );
        }
    }
}

/// Equivalences and samenesses of three or more members that differ only in
/// their annotations make one statement of each pair: the first states the
/// pairs, and each annotated one reifies them in turn, in the order the axioms
/// take. A pair's anonymous object is the first's node, but an anonymous
/// member after the first is, as the subject of its pair in a later axiom, a
/// node of its own, nested in that pair's reification. A general axiom is a
/// graph of its own and shares nothing. As ROBOT 1.9.11 converts
/// `chain-twins`, in RDF/XML and Turtle.
#[test]
fn chains_differing_only_in_annotations_state_each_pair_once() {
    let name = "chain-twins";
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]),
            fixture_text(&format!("{name}.robot.{ext}")),
            "{name}.{ext}"
        );
    }
}

/// The nodes a class's block names by id are defined after it in turn, and
/// the nodes those definitions name after all of them: a chain's member that
/// the class's equivalence names is defined after the node of the class's
/// superclass. As ROBOT 1.9.11 converts `definition-order`, in RDF/XML and
/// Turtle.
#[test]
fn nodes_are_defined_level_by_level_after_the_block_naming_them() {
    let name = "definition-order";
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]),
            fixture_text(&format!("{name}.robot.{ext}")),
            "{name}.{ext}"
        );
    }
}

/// A difference of one individual, an individual named twice, is an
/// `owl:AllDifferent` node with a list of one member, as a difference of three
/// or more is: stated in the graph that reaches its member, or among the
/// general axioms when none does, and numbered where it is stated. As ROBOT
/// 1.9.11 converts `one-member-difference`, in RDF/XML and Turtle.
#[test]
fn a_difference_of_one_individual_is_an_all_different_node() {
    let name = "one-member-difference";
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]),
            fixture_text(&format!("{name}.robot.{ext}")),
            "{name}.{ext}"
        );
    }
}

/// A set axiom whose members come to one, or to none, is written as ROBOT
/// writes it: a disjoint union of one class, or of none, in its class's frame,
/// its empty list `rdf:nil`; a disjointness of one property as an
/// `owl:AllDisjointProperties` node in that property's frame, numbered among
/// the frame's other nodes; an equivalence of one property not at all. As
/// ROBOT 1.9.11 converts `set-axioms-of-one` and `disjoint-union-of-none`, in
/// functional syntax, RDF/XML, Turtle and OBO.
#[test]
fn set_axioms_of_one_member_are_written_as_robot_writes_them() {
    for source in ["set-axioms-of-one.ofn", "disjoint-union-of-none.owl"] {
        let name = source.rsplit_once('.').unwrap().0;
        for ext in ["ofn", "owl", "ttl", "obo"] {
            assert_eq!(
                convert_fixture(source, &format!("{name}.{ext}"), &[]),
                fixture_text(&format!("{name}.robot.{ext}")),
                "{name}.{ext}"
            );
        }
    }
}

/// Anonymous individuals that name only each other, with nothing else naming
/// any of them, are in no graph: one is an instance of a class enumerating the
/// other, the two are the same, and one is annotated. Those axioms are written
/// nowhere, and the command says so. A query reads the same document. As ROBOT
/// 1.9.11 converts `select-objects`, in RDF/XML and Turtle, and answers a
/// query over it.
#[test]
fn anonymous_individuals_naming_only_each_other_are_written_nowhere() {
    for ext in ["owl", "ttl"] {
        let path = tmp(&format!("select-objects.{ext}"));
        let mut cmd = bin();
        cmd.args(["convert", "-i"]).arg(robot_fixture("select-objects.ofn")).arg("-o").arg(&path);
        let run = output_within_a_minute(cmd, &format!("select-objects-{ext}"));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(run.status.success(), "{stderr}");
        assert!(
            stderr.contains("3 axiom(s) about anonymous individuals no other statement reaches are written nowhere"),
            "{stderr}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), fixture_text(&format!("select-objects.robot.{ext}")), "{ext}");
    }
    let query = tmp("select-objects-labels.rq");
    std::fs::write(&query, "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\nSELECT ?s ?l WHERE { ?s rdfs:label ?l } ORDER BY ?l ?s\n")
        .unwrap();
    let out = tmp("select-objects-labels.csv");
    let mut cmd = bin();
    cmd.args(["query", "-i"]).arg(robot_fixture("select-objects.ofn")).arg("--query").arg(&query).arg(&out);
    let run = output_within_a_minute(cmd, "select-objects-query");
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("select-objects.labels.robot.csv"));
}

/// A sameness, or a difference of more than two, that an entity's graph
/// reaches through an anonymous member before any named member's own is stated
/// in that graph: each named member but the entity is a block of its own after
/// the entity's, holding the statements of the graph it is the subject of, a
/// reached difference of two and an assertion on an inverse property among
/// them, and followed again by the graph's anonymous roots, the blocks ordered
/// by kind and then by IRI. An annotated difference of two so reached is
/// stated by its reification there. Reached through an annotation property's
/// annotation value, an object property's range and two classes'
/// superclasses, as ROBOT 1.9.11 converts `individual-pairs-reached-elsewhere`,
/// in RDF/XML and Turtle.
#[test]
fn a_pair_reached_from_another_graph_is_stated_there() {
    let name = "individual-pairs-reached-elsewhere";
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]),
            fixture_text(&format!("{name}.robot.{ext}")),
            "{name}.{ext}"
        );
    }
}

/// An `rdf:XMLLiteral` is written as the markup it is, under
/// `rdf:parseType="Literal"`, in an annotation, a reified annotation's target
/// and a data property assertion, its own prefixes kept where the ontology has
/// no IRI; Turtle writes it as a typed string. As ROBOT 1.9.11 converts
/// `xml-literal` and `xml-literal-anonymous`, in RDF/XML and Turtle.
#[test]
fn an_xml_literal_is_written_as_its_markup() {
    for name in ["xml-literal", "xml-literal-anonymous"] {
        for ext in ["owl", "ttl"] {
            assert_eq!(
                convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]),
                fixture_text(&format!("{name}.robot.{ext}")),
                "{name}.{ext}"
            );
        }
    }
}

/// An `rdf:XMLLiteral` that is not a document of its own cannot be markup, and
/// the RDF/XML holding it is not written, as ROBOT 1.9.11 does not write it.
#[test]
fn an_xml_literal_that_is_not_a_document_is_not_written() {
    let path = tmp("xml-literal-unwritable.owl");
    let run = bin()
        .args(["convert", "-i"])
        .arg(robot_fixture("xml-literal-unwritable.ofn"))
        .arg("-o")
        .arg(&path)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success(), "{stderr}");
    assert!(stderr.contains("XML literal is not self contained: \"plain text\""), "{stderr}");
}

/// What ROBOT 1.9.11 writes in RDF/XML reads back as ROBOT reads it: of the
/// `owl:Annotation` nodes naming one annotation, the one read first annotates
/// it; an ontology annotation whose value is an anonymous individual carries no
/// annotations; and a rule is annotated by its literal annotations alone,
/// each bare, while one whose value is an anonymous individual is asserted of
/// the rule's node. As ROBOT reads `rdf-nested-annotations` and
/// `rdf-nested-anonymous`, its own RDF/XML of the functional documents.
#[test]
fn robots_rdf_xml_reads_back_as_robot_reads_it() {
    for name in ["rdf-nested-annotations", "rdf-nested-anonymous"] {
        assert_eq!(
            convert_fixture(&format!("{name}.owl"), &format!("{name}-read-back.ofn"), &[]),
            fixture_text(&format!("{name}.owl.robot.ofn")),
            "{name}"
        );
    }
}

/// An annotation node shared by several reifications — the pairs an annotated
/// equivalence of three classes is written as — keeps its own annotations on
/// the reification the reader translates first, and the others carry the
/// annotation alone. As ROBOT 1.9.11 reads `rdf-nested-anonymous`: C ≡ D first.
#[test]
fn a_shared_annotation_node_keeps_its_annotations_on_the_axiom_read_first() {
    let text = convert_fixture("rdf-nested-anonymous.owl", "rdf-nested-anonymous-read.ofn", &[]);
    for line in [
        "EquivalentClasses(Annotation(nest:see _:genid2147483693) nest:B nest:C)\n",
        "EquivalentClasses(Annotation(Annotation(rdfs:comment \"equivalent value\") nest:see _:genid2147483693) nest:C nest:D)\n",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
}

/// `name`'s `.ofn` fixture written as RDF/XML and as Turtle holds every line of
/// ROBOT 1.9.11's, in order, around what it writes beyond them, but for the
/// lines `edits` names: for an extension, ROBOT's line and the one written in
/// its place, as a block ROBOT writes empty opens to hold a statement. `query`,
/// a SPARQL `ASK`, holds of it, and it reads back whole. `prefix` is the name
/// the document read back gives the fixture's namespace.
fn assert_written_around_the_layout(name: &str, prefix: &str, query: &str, edits: &[(&str, &str, &str)]) {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};
    use oxigraph::store::Store;
    // The first line ROBOT writes that `written` does not, in order.
    fn missing<'a>(written: &str, robot: &'a str) -> Option<&'a str> {
        let mut lines = written.lines();
        robot.lines().find(|want| !lines.any(|l| l == *want))
    }
    // The axioms of a functional-syntax document, an assertion on an inverse
    // as the assertion of the named property it is, without anonymous labels.
    let axioms = |text: &str| -> Vec<String> {
        let mut out: Vec<String> = text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with(['#', ')']) && !l.starts_with("Prefix(") && !l.starts_with("Ontology("))
            .map(|l| {
                let l = l.replace(prefix, ":");
                let l = match l.split_once("ObjectInverseOf(") {
                    Some((head, rest)) => {
                        let (p, terms) = rest.split_once(") ").unwrap();
                        let (a, b) = terms.trim_end_matches(')').split_once(' ').unwrap();
                        format!("{head}{p} {b} {a})")
                    }
                    None => l,
                };
                let mut bare = String::new();
                let mut rest = l.as_str();
                while let Some(at) = rest.find("_:") {
                    bare.push_str(&rest[..at + 2]);
                    rest = rest[at + 2..].trim_start_matches(|c: char| c.is_ascii_alphanumeric());
                }
                bare + rest
            })
            .collect();
        out.sort();
        out
    };
    let source = axioms(&convert_fixture(&format!("{name}.ofn"), &format!("{name}-source.ofn"), &[]));
    for (ext, format) in [("owl", RdfFormat::RdfXml), ("ttl", RdfFormat::Turtle)] {
        let written = convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]);
        let mut robot = fixture_text(&format!("{name}.{ext}"));
        for (_, from, to) in edits.iter().filter(|(e, _, _)| *e == ext) {
            assert!(robot.contains(from), "{ext}: {from}");
            robot = robot.replacen(from, to, 1);
        }
        assert_eq!(missing(&written, &robot), None, "{ext}\n{written}");
        let store = Store::new().unwrap();
        store.load_from_slice(RdfParser::from_format(format), written.as_bytes()).unwrap();
        let answer = SparqlEvaluator::new().parse_query(query).unwrap().on_store(&store).execute().unwrap();
        assert!(matches!(answer, QueryResults::Boolean(true)), "{ext}\n{written}");
        let path = tmp(&format!("{name}-written.{ext}"));
        std::fs::write(&path, &written).unwrap();
        let back = tmp(&format!("{name}-back-{ext}.ofn"));
        let run = bin().args(["convert", "-i"]).arg(&path).arg("-o").arg(&back).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(axioms(&std::fs::read_to_string(&back).unwrap()), source, "{ext} read back");
    }
}

/// A statement about an anonymous individual with no place in the layout is
/// written around it, and the rest of the document keeps the layout ROBOT
/// 1.9.11 writes, line for line, in RDF/XML and in Turtle: what a later graph
/// states about a node defined by id, the annotations of an assertion on an
/// inverse property, an assertion on an inverse whose subject is anonymous, and
/// an annotated assertion about an ontology annotation's value are blocks of
/// their own after the first block of the graph that makes them; and the
/// document reads back whole.
#[test]
fn statements_with_no_place_in_the_layout_are_written_around_it() {
    assert_written_around_the_layout(
        "rdf-dropped-statements",
        "drop:",
        "PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
         PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX : <http://example.org/drop#> \
         ASK { :a1 owl:differentFrom ?x1 . :a2 :p ?x1 . :a3 :q ?x1 . ?x1 a :A . \
         <http://example.org/drop> :ap ?h . ?h a :A . ?r2 owl:annotatedSource ?h ; \
         owl:annotatedProperty rdf:type ; owl:annotatedTarget :A ; rdfs:comment \"ann\" . \
         ?x3 :p :c1 . FILTER(isBlank(?x3)) \
         ?r5 owl:annotatedSource :e2 ; owl:annotatedProperty :p ; owl:annotatedTarget :e1 ; rdfs:seeAlso ?x5 . :e3 :q ?x5 . \
         ?r6 owl:annotatedSource :f2 ; owl:annotatedProperty :p ; owl:annotatedTarget :f1 ; rdfs:seeAlso ?x6 . :f3 :q ?x6 . ?x6 a :B . \
         ?r7 owl:annotatedSource :g2 ; owl:annotatedProperty :p ; owl:annotatedTarget :g1 ; rdfs:comment \"c\" . \
         ?n7 owl:annotatedSource ?r7 ; owl:annotatedProperty rdfs:comment ; owl:annotatedTarget \"c\" ; rdfs:seeAlso ?x7 . :g3 :q ?x7 }",
        &[],
    );
}

/// A sameness or a difference of two with a named member, which a graph
/// reaches through its anonymous member before the named member's own, and
/// that graph's layout does not state, is written around the layout: an
/// unannotated pair is an edge of the named member's block, and the
/// reification of an annotated one, with the annotations of its annotations,
/// a root of the graph that reaches it. Here that graph is the ontology
/// header's, through the value of an ontology annotation, and, for a
/// difference, an object property's, through its range; and an individual
/// that graph names only in such a pair is a root of it, with what the graph
/// states about it. An assertion on an inverse property, of the anonymous
/// individual about a named one, is an edge of the named one's block, with
/// the reification of its annotations. The rest of the document is as ROBOT
/// 1.9.11 writes it, which states none of these pairs and assertions, nothing
/// about that individual, and not the annotations of a sameness of the
/// header's anonymous individuals.
#[test]
fn pairs_no_graph_states_are_written_around_the_layout() {
    assert_written_around_the_layout(
        "rdf-dropped-pairs",
        "pairs:",
        "PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
         PREFIX : <http://example.org/pairs#> \
         ASK { <http://example.org/pairs> rdfs:seeAlso ?h . ?h a ?one . ?one owl:oneOf ?list . \
         :n1 owl:sameAs ?a . :n3 owl:differentFrom ?c . \
         ?r2 owl:annotatedSource :n2 ; owl:annotatedProperty owl:sameAs ; owl:annotatedTarget ?b ; rdfs:label \"same\" . \
         ?s2 owl:annotatedSource ?r2 ; owl:annotatedProperty rdfs:label ; owl:annotatedTarget \"same\" ; rdfs:comment \"nested\" . \
         ?r5 owl:annotatedProperty owl:sameAs ; rdfs:label \"anonymous\" . \
         ?s5 owl:annotatedSource ?r5 ; owl:annotatedProperty rdfs:label ; owl:annotatedTarget \"anonymous\" ; rdfs:comment \"deep\" . \
         :p rdfs:range ?range . ?range owl:hasValue ?d . :n4 owl:differentFrom ?d . \
         :q rdfs:range ?qrange . ?qrange owl:hasValue ?e . ?f :q ?e ; :dp \"1\" . :n5 owl:differentFrom ?f . \
         :n6 :q ?d . :n7 :q ?d . \
         ?r7 owl:annotatedSource :n7 ; owl:annotatedProperty :q ; owl:annotatedTarget ?d ; rdfs:comment \"inverse\" }",
        &[
            ("owl", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n1\"/>", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n1\">"),
            ("owl", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n3\"/>", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n3\">"),
            ("owl", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n4\"/>", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n4\">"),
            ("owl", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n5\"/>", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n5\">"),
            ("owl", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n6\"/>", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n6\">"),
            ("owl", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n7\"/>", "<owl:NamedIndividual rdf:about=\"http://example.org/pairs#n7\">"),
            ("ttl", ":n1 rdf:type owl:NamedIndividual .", ":n1 rdf:type owl:NamedIndividual ;"),
            ("ttl", ":n3 rdf:type owl:NamedIndividual .", ":n3 rdf:type owl:NamedIndividual ;"),
            ("ttl", ":n4 rdf:type owl:NamedIndividual .", ":n4 rdf:type owl:NamedIndividual ;"),
            ("ttl", ":n5 rdf:type owl:NamedIndividual .", ":n5 rdf:type owl:NamedIndividual ;"),
            ("ttl", ":n6 rdf:type owl:NamedIndividual .", ":n6 rdf:type owl:NamedIndividual ;"),
            ("ttl", ":n7 rdf:type owl:NamedIndividual .", ":n7 rdf:type owl:NamedIndividual ;"),
        ],
    );
}

/// A negative assertion about an anonymous individual a rule names, which no
/// other graph reaches, is a root of the rules' graph, written around the
/// layout with the `owl:Annotation` node its annotation's annotation makes;
/// every other line is as ROBOT 1.9.11 writes the RDF/XML owlmake writes of
/// `rdf-rule-nested`, which ROBOT's functional-syntax parser rejects, and the
/// document reads back whole.
#[test]
fn a_rule_graph_states_the_annotations_of_annotations() {
    assert_written_around_the_layout(
        "rdf-rule-nested",
        ":",
        "PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
         PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX swrl: <http://www.w3.org/2003/11/swrl#> \
         PREFIX : <http://example.org/rule-nested#> \
         ASK { ?rule a swrl:Imp ; swrl:body ?body . ?body rdf:first ?atom . ?atom swrl:argument1 ?x . \
         ?neg a owl:NegativePropertyAssertion ; owl:sourceIndividual ?x ; owl:assertionProperty :p ; \
         owl:targetIndividual :i ; rdfs:label \"neg\" . \
         ?n owl:annotatedSource ?neg ; owl:annotatedProperty rdfs:label ; owl:annotatedTarget \"neg\" ; rdfs:comment \"nested\" }",
        &[],
    );
}

/// An assertion about an anonymous individual a rule names, which no other
/// graph reaches, is a statement of the individual's node in the rule: the one
/// line ROBOT 1.9.11 writes for that node, empty, becomes the node with its
/// statement, and every other line is as ROBOT writes it.
#[test]
fn an_assertion_about_an_individual_a_rule_names_is_stated_in_the_rule() {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};
    use oxigraph::store::Store;
    let robot = fixture_text("rdf-dropped-rule.owl");
    let empty = "<swrl:argument2>\n                            <rdf:Description/>\n";
    assert!(robot.contains(empty));
    let stated = "<swrl:argument2>\n                            <rdf:Description>\n                                \
                  <rdf:type rdf:resource=\"http://example.org/drop#B\"/>\n                            </rdf:Description>\n";
    assert_eq!(convert_fixture("rdf-dropped-rule.ofn", "rdf-dropped-rule.owl", &[]), robot.replacen(empty, stated, 1));
    for (ext, format) in [("owl", RdfFormat::RdfXml), ("ttl", RdfFormat::Turtle)] {
        let out = tmp(&format!("rdf-dropped-rule-checked.{ext}"));
        let run = bin().args(["convert", "-i"]).arg(robot_fixture("rdf-dropped-rule.ofn")).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!stderr.contains("layout cannot state"), "{ext}: {stderr}");
        let store = Store::new().unwrap();
        store.load_from_slice(RdfParser::from_format(format), &std::fs::read(&out).unwrap()).unwrap();
        let query = "PREFIX swrl: <http://www.w3.org/2003/11/swrl#> PREFIX : <http://example.org/drop#> \
                     ASK { ?atom a swrl:IndividualPropertyAtom ; swrl:argument2 ?x . ?x a :B }";
        let answer = SparqlEvaluator::new().parse_query(query).unwrap().on_store(&store).execute().unwrap();
        assert!(matches!(answer, QueryResults::Boolean(true)), "{ext}");
    }
}

/// A data-property atom's subject is an individual argument: a variable or a
/// named individual. Rules naming an individual there are read and written in
/// every format, ROBOT 1.9.11's RDF/XML, Turtle, OWL/XML and Manchester
/// renderings of them read back to its functional syntax, and HermiT binds the
/// individual when it applies the rule, as ROBOT 1.9.11 does.
#[test]
fn a_data_property_atom_takes_an_individual_as_its_subject() {
    let name = "swrl-data-property-subject";
    for ext in ["ofn", "owl", "ttl", "owx", "omn", "obo", "json"] {
        assert_eq!(
            convert_fixture(&format!("{name}.ofn"), &format!("{name}.{ext}"), &[]),
            fixture_text(&format!("{name}.robot.{ext}")),
            "{ext}"
        );
    }
    for ext in ["owl", "ttl", "owx", "omn"] {
        let src = format!("{name}.robot.{ext}");
        assert_eq!(convert_fixture(&src, &format!("{src}.ofn"), &[]), fixture_text(&format!("{src}.robot.ofn")), "{src}");
    }
    let out = tmp(&format!("{name}-reason.ofn"));
    let run = bin()
        .args(["reason", "--reasoner", "hermit", "--axiom-generators", "SubClass ClassAssertion", "-i"])
        .arg(robot_fixture(&format!("{name}-reason.ofn")))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(&format!("{name}-reason.hermit.robot.ofn")));
    let _ = std::fs::remove_file(&out);
}

/// An OBO document carries every axiom it has no tag for in its `owl-axioms:`
/// clause, as ROBOT 1.9.11 writes it: keys, data property axioms, datatype
/// definitions, and a disjointness of object properties with an inverse member.
#[test]
fn obo_carries_every_untranslatable_axiom_in_owl_axioms() {
    assert_eq!(convert_fixture("obo-owl-axioms.ofn", "obo-owl-axioms.obo", &[]), fixture_text("obo-owl-axioms.obo"));
}

/// Classes defined twice, over has-values, one-ofs, data restrictions, unions,
/// complements and inverse properties, are written as ROBOT 1.9.11 writes them
/// in OBO. The definitions reach a frame in the order of their hash; the first
/// one is its `intersection_of:` clauses, and a later one with an operand OBO
/// cannot spell adds the operands it can. A restriction on an inverse property
/// is its filler alone, and a class whose definitions all go to `owl-axioms:`
/// keeps an empty frame.
#[test]
fn classes_defined_twice_are_written_as_robot_writes_them_in_obo() {
    assert_eq!(
        convert_fixture("obo-two-definitions.ofn", "obo-two-definitions.obo", &[]),
        fixture_text("obo-two-definitions.obo")
    );
}

/// A class has a `[Term]` frame wherever an axiom's translation starts on it —
/// a subclass axiom with the class as its subclass, an equivalence of two
/// members with the class as its named member — even when that axiom goes to
/// `owl-axioms:` and the frame is empty. An equivalence of three members, a
/// disjointness or key with no clause, and a general axiom give it none. As
/// ROBOT 1.9.11 writes `obo-empty-frames`.
#[test]
fn a_class_whose_axioms_have_no_clause_keeps_its_frame() {
    assert_eq!(
        convert_fixture("obo-empty-frames.ofn", "obo-empty-frames.obo", &[]),
        fixture_text("obo-empty-frames.obo")
    );
}

/// A subclass axiom that relates its class through an inverse property has no
/// OBO spelling — its `relationship:` would name no relation — so the document
/// is not written as OBO, as ROBOT 1.9.11 does not write it, and the axiom is
/// named.
#[test]
fn a_relationship_through_an_inverse_property_is_not_written_as_obo() {
    let src = tmp("obo-inverse-relationship.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://purl.obolibrary.org/obo/>)\n\
         Ontology(<http://purl.obolibrary.org/obo/z.owl>\n\
         Declaration(Class(:Z_A))\n\
         Declaration(Class(:Z_C))\n\
         Declaration(ObjectProperty(:Z_p))\n\
         SubClassOf(:Z_C ObjectSomeValuesFrom(ObjectInverseOf(:Z_p) :Z_A))\n\
         )\n",
    )
    .unwrap();
    let out = tmp("obo-inverse-relationship.obo");
    let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
    let _ = std::fs::remove_file(&src);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success(), "{stderr}");
    assert!(stderr.contains("cannot be saved in OBO format") && stderr.contains("ObjectInverseOf"), "{stderr}");
}

/// An equivalence or disjointness of named object properties is one clause of
/// its first member's [Typedef], naming its last member, with the axiom's
/// annotations as qualifiers; one with an inverse member goes to `owl-axioms:`;
/// and an annotated equivalence of classes keeps its qualifiers. As ROBOT 1.9.11
/// writes `obo-property-axioms`.
#[test]
fn obo_translates_property_equivalence_and_disjointness_as_robot_does() {
    assert_eq!(
        convert_fixture("obo-property-axioms.ofn", "obo-property-axioms.obo", &[]),
        fixture_text("obo-property-axioms.obo")
    );
}

/// A data restriction is written in the `owl-axioms:` clause wherever it sits
/// in a class expression, as ROBOT 1.9.11 writes it.
#[test]
fn obo_owl_axioms_write_data_restrictions() {
    let clause = |text: &str| text.lines().find(|l| l.starts_with("owl-axioms: ")).map(str::to_string);
    let written = convert_fixture("obo-data-restrictions.ofn", "obo-data-restrictions.obo", &[]);
    assert_eq!(clause(&written), clause(&fixture_text("obo-data-restrictions.obo")));
}

/// `om convert` a fixture to OBO as ROBOT 1.9.11, under ODK 1.6.1, writes it —
/// with no [Instance] frames; the text written.
fn convert_fixture_to_obo_as_robot(src: &str) -> String {
    convert_fixture_to_obo_as_robot_with(src, &[])
}

fn convert_fixture_to_obo_as_robot_with(src: &str, args: &[&str]) -> String {
    let path = tmp(&format!("{src}.as-robot.obo"));
    let run = bin()
        .args(["__emulate-robot-version=1.9.11", "__emulate-odk-version=1.6.1", "convert", "-i"])
        .arg(robot_fixture(src))
        .args(args)
        .arg("-o")
        .arg(&path)
        .output()
        .unwrap();
    assert!(run.status.success(), "{src}: {}", String::from_utf8_lossy(&run.stderr));
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    text
}

/// An ontology's annotations are header clauses: a property with an OBO tag is
/// that tag's clause — rdfs:comment a `remark:` — and any other a
/// `property_value:`, each qualified by the annotation's own annotations, with
/// a definition's xrefs its bracket list and an xref's label its description.
/// Tags are written by rank, and those of one rank in the order of a hash set
/// of every tag the header holds, so `name:` follows `owl-axioms:`. A literal
/// outside OWL 2's datatype map, an anonymous individual and a blank value of a
/// tag have no clause: in the header they are dropped, on a frame they go to
/// `owl-axioms:`. As ROBOT 1.9.11 writes `obo-header-annotations`.
#[test]
fn ontology_annotations_are_written_as_robot_writes_them_in_obo() {
    assert_eq!(
        convert_fixture_to_obo_as_robot("obo-header-annotations.ofn"),
        fixture_text("obo-header-annotations.robot.obo")
    );
}

/// A [Typedef] clause carries its axiom's annotations as qualifiers — a
/// characteristic, `is_a:`, `inverse_of:` and `domain:` alike — and starts the
/// frame of a property that is not declared. A characteristic, domain or
/// inverse pair on an inverse property, and a domain of owl:Thing, go to
/// `owl-axioms:`; a sub-property of a top, bottom or OWL property, and a class
/// expression as domain, are written nowhere. An annotation property has a
/// frame through its `is_metadata_tag` assertion, whatever its value, and its
/// sub-property axiom goes to `owl-axioms:`. As ROBOT 1.9.11 writes
/// `obo-typedef-clauses`.
#[test]
fn typedef_clauses_are_written_as_robot_writes_them_in_obo() {
    assert_eq!(
        convert_fixture_to_obo_as_robot("obo-typedef-clauses.ofn"),
        fixture_text("obo-typedef-clauses.robot.obo")
    );
}

/// OWL input written as OBO, as ROBOT 1.9.11 writes it: an anonymous individual
/// is its node id where a tag or a qualifier holds it, and no
/// `property_value:`; only a declared class or property has its annotations
/// translated, while an axiom's translation starts the frame of the class or
/// property it is written on, declared or not; axioms that differ only in
/// annotations each go to `owl-axioms:`; a datatype other than XSD's is
/// written in full; and a label stated plain, as an `xsd:string` and with a
/// language tag is one `name:`.
#[test]
fn owl_input_is_written_as_robot_writes_it_in_obo() {
    for stem in [
        "rdf-anonymous-individuals",
        "rdf-nested-anonymous",
        "general-axioms-reaching-annotated-assertions",
        "imports-closure-leaf",
        "reason-annotate",
        "undeclared-properties",
        "rdf-root-block-types",
        "validate-prop-bottom",
        "chain-twins",
        "axioms-namespace",
        "xml-literal",
        "typed-string-duplicates",
    ] {
        assert_eq!(
            convert_fixture_to_obo_as_robot(&format!("{stem}.ofn")),
            fixture_text(&format!("{stem}.robot.obo")),
            "{stem}"
        );
    }
}

/// A frame holding a lone `intersection_of:`, and an ontology stating the range
/// of an inverse property, have no OBO document: neither is written, as ROBOT
/// 1.9.11 writes neither.
#[test]
fn obo_refuses_what_robot_refuses() {
    for (src, says) in [
        ("rdf-connective-members.owl", "single intersection_of tags are not allowed"),
        ("rdf-inverse-axioms.ofn", "states the range of an inverse property"),
    ] {
        let out = tmp(&format!("{src}.refused.obo"));
        let run = bin()
            .args(["__emulate-robot-version=1.9.11", "__emulate-odk-version=1.6.1", "convert", "-i"])
            .arg(robot_fixture(src))
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success() && stderr.contains(says), "{src}: {stderr}");
        let _ = std::fs::remove_file(&out);
    }
}

/// Each OBO clause read as ROBOT 1.9.11 reads it. An unquoted value — a name, a
/// comment, a created_by, a [Term]'s subset, a custom tag, a header tag — runs
/// to the first `!` or `{` no backslash escapes, with its escapes resolved and
/// its trailing white space dropped, and the qualifier blocks after it annotate
/// the assertion. A qualifier key or a custom tag names its property through the
/// OBO vocabulary or the oboInOwl namespace, `owl:`, `rdf:`, `rdfs:` and `xsd:`
/// names their vocabulary, a deprecated `<scope>_synonym:` is a synonym, and an
/// alt_id of a [Typedef] is a deprecated property. Written back as OBO, every
/// such clause keeps its qualifiers and escapes as ROBOT writes them, owl-axioms
/// included.
#[test]
fn obo_clauses_are_read_and_written_as_robot_does() {
    assert_eq!(
        convert_fixture("obo-unquoted-clauses.obo", "obo-unquoted-clauses.ofn", &[]),
        fixture_text("obo-unquoted-clauses.robot.ofn")
    );
    assert_eq!(
        convert_fixture_to_obo_as_robot("obo-unquoted-clauses.obo"),
        fixture_text("obo-unquoted-clauses.robot.obo")
    );
    assert_eq!(
        convert_fixture_to_obo_as_robot("obo-unquoted-values.ofn"),
        fixture_text("obo-unquoted-values.robot.obo")
    );
    assert_eq!(
        convert_fixture("obo-unquoted-values.robot.obo", "obo-unquoted-values.back.ofn", &[]),
        fixture_text("obo-unquoted-values.robot.obo.robot.ofn")
    );
}

/// An OBO clause ROBOT 1.9.11 cannot read fails the read: text after a value's
/// qualifier blocks, a block with no `key=`, a third block on a [Term] line or
/// a second on a header line, a line ending in a backslash, a tag with no value,
/// a qualifier key, custom tag or subset holding a space, and an owl-axioms
/// value that is not an ontology in functional syntax.
#[test]
fn obo_clauses_robot_cannot_read_are_refused() {
    for (clauses, says) in [
        ("[Term]\nid: Q:1\nname: e {comment=\"x\"} extra", "expected the end of the line"),
        ("[Term]\nid: Q:1\nname: g {}", "missing '='"),
        ("[Term]\nid: Q:1\nname: a {comment=\"x\"} {source=\"y\"} {seeAlso=\"z\"}", "expected the end of the line"),
        ("[Term]\nid: Q:1\nname: abc\\", "a backslash ends the line"),
        ("[Term]\nid: Q:1\ncomment:", "expected a value"),
        ("[Term]\nid: Q:1\ndef: \"d\" [] {comment = \"x\"}", "spaces not allowed: 'comment '"),
        ("[Term]\nid: Q:1\nsubset: s1 s2", "spaces not allowed: 's1 s2'"),
        ("[Term]\nid: Q:1\nfoo bar: x", "spaces not allowed: 'foo bar'"),
        ("[Term]\nid: Q:1\nnamespace: ns1 extra", "expected the end of the line"),
        ("remark: r1 {comment=\"a\"} {comment=\"b\"}", "expected the end of the line"),
        (
            "owl-axioms: Ontology(\\nAnnotationAssertion(<http://www.w3.org/2000/01/rdf-schema#comment> \
             <http://x/a> \\\"hi! there\\\")\\n)",
            "owl-axioms",
        ),
    ] {
        let src = tmp("unreadable-clause.obo");
        std::fs::write(&src, format!("format-version: 1.2\nontology: q\n\n{clauses}\n")).unwrap();
        let out = tmp("unreadable-clause.ofn");
        let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success() && stderr.contains(says), "{clauses}: {stderr}");
        assert!(!out.exists(), "{clauses}");
        let _ = std::fs::remove_file(&src);
    }
}

/// A relation clause's qualifiers make the class expression ROBOT 1.9.11 makes
/// of it — an exact, minimum or maximum cardinality, `only not` for none, the
/// intersection of a minimum and a maximum or of `some` and `only`, a value
/// restriction for a class-level relation — and `gci_relation`/`gci_filler`
/// make the subject of an is_a, relationship, disjoint_from or equivalent_to. A
/// metadata-tag [Typedef] is an annotation property every clause of which but
/// is_a is an annotation. A header date is read leniently, in the Julian
/// calendar before 15 October 1582, and written back normalised; the first
/// date of a header is the ontology's, one that does not read is not written,
/// several are written in the order of their sort text and two of one instant
/// once. A property whose tag a frame clause spells is written as that tag.
#[test]
fn obo_relations_metadata_tags_and_dates_are_read_and_written_as_robot_does() {
    for name in ["obo-relation-qualifiers", "obo-metadata-tags", "obo-header-date", "obo-header-dates"] {
        assert_eq!(
            convert_fixture(&format!("{name}.obo"), &format!("{name}.ofn"), &[]),
            fixture_text(&format!("{name}.robot.ofn")),
            "{name} to functional syntax"
        );
        assert_eq!(
            convert_fixture_to_obo_as_robot(&format!("{name}.obo")),
            fixture_text(&format!("{name}.robot.obo")),
            "{name} to OBO"
        );
    }
    for name in ["obo-tag-properties", "obo-date-annotation-1", "obo-date-annotation-2"] {
        assert_eq!(
            convert_fixture_to_obo_as_robot(&format!("{name}.ofn")),
            fixture_text(&format!("{name}.robot.obo")),
            "{name} to OBO"
        );
    }
    assert_eq!(
        convert_fixture_to_obo_as_robot_with("obo-date-annotation-3.ofn", &["--check", "false"]),
        fixture_text("obo-date-annotation-3.robot.obo"),
        "obo-date-annotation-3 to OBO"
    );
}

/// A cardinality that is not an integer, a gci_relation with no gci_filler and
/// a header date not of the form dd:MM:yyyy HH:mm, first or not, fail the read,
/// as they fail ROBOT 1.9.11's; a metadata tag with a single intersection_of
/// and a header with two dates or two saved-by values are not written as OBO,
/// while two dates of one instant are one.
#[test]
fn obo_relations_and_dates_robot_cannot_read_are_refused() {
    for (clauses, says) in [
        ("[Term]\nid: Q:1\nrelationship: R:1 Q:2 {cardinality=\"two\"}", "not an integer"),
        ("[Term]\nid: Q:1\nrelationship: R:1 Q:2 {gci_relation=\"R:1\"}", "no gci_filler"),
        ("date: 2021-03-05", "header date"),
        ("date: 05:03:2021", "header date"),
        ("date: 01:02:2020 10:00\ndate: 2021-03-05", "header date"),
    ] {
        let src = tmp("unreadable-relation.obo");
        std::fs::write(&src, format!("format-version: 1.2\nontology: q\n\n{clauses}\n")).unwrap();
        let out = tmp("unreadable-relation.ofn");
        let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success() && stderr.contains(says), "{clauses}: {stderr}");
        assert!(!out.exists(), "{clauses}");
        let _ = std::fs::remove_file(&src);
    }
    let src = tmp("single-intersection.obo");
    std::fs::write(
        &src,
        "format-version: 1.2\nontology: q\n\n[Typedef]\nid: R:1\nis_metadata_tag: true\nintersection_of: R:2\n",
    )
    .unwrap();
    let out = tmp("single-intersection.obo.out.obo");
    let run = bin()
        .args(["__emulate-robot-version=1.9.11", "__emulate-odk-version=1.6.1", "convert", "-i"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success() && stderr.contains("single intersection_of"), "{stderr}");
    let _ = std::fs::remove_file(&src);
    for (annotations, refused) in [
        (r#"Annotation(oboInOwl:date "01:01:2020 10:00") Annotation(oboInOwl:date "00:01:2020 10:00")"#, Some("date")),
        (r#"Annotation(oboInOwl:saved-by "a") Annotation(oboInOwl:saved-by "b")"#, Some("saved-by")),
        (r#"Annotation(oboInOwl:date "01:01:2020 10:00") Annotation(oboInOwl:date "31:12:2019 34:00")"#, None),
    ] {
        let src = tmp("two-header-values.ofn");
        std::fs::write(
            &src,
            format!(
                "Prefix(oboInOwl:=<http://www.geneontology.org/formats/oboInOwl#>)\n\
                 Ontology(<http://purl.obolibrary.org/obo/q.owl>\n{annotations}\n)\n"
            ),
        )
        .unwrap();
        let out = tmp("two-header-values.obo");
        let run = bin()
            .args(["__emulate-robot-version=1.9.11", "__emulate-odk-version=1.6.1", "convert", "-i"])
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        match refused {
            Some(tag) => assert!(
                !run.status.success() && stderr.contains(&format!("multiple {tag} tags not allowed")),
                "{annotations}: {stderr}"
            ),
            None => {
                assert!(run.status.success(), "{annotations}: {stderr}");
                let text = std::fs::read_to_string(&out).unwrap();
                assert_eq!(text.matches("\ndate: ").count(), 1, "{annotations}: {text}");
            }
        }
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
    }
}

/// An OBO document declares its frames and what its relations reach, and nothing
/// it only names: each [Term] and [Typedef], each alt_id, the filler of a
/// `relationship:`, of a GCI and of an `intersection_of:` relation, and each
/// property a tag is read as, with its OBO name as its label. An is_a,
/// disjoint_from, union_of or equivalent_to operand, a property_value predicate,
/// a subset, a synonym type and a relation with no [Typedef] stay undeclared, so
/// they leave with the last axiom that names them, and Turtle types them where
/// it types any undeclared entity. An id with no prefix is in the ontology's own
/// id space, `http://purl.obolibrary.org/obo/<ontology>#<id>`, whatever the
/// ontology id is. A [Typedef]'s equivalent_to and disjoint_from are property
/// equivalence and disjointness, and those clauses keep their qualifiers as a
/// [Term]'s equivalent_to keeps them. As ROBOT 1.9.11 reads them.
#[test]
fn obo_declares_and_resolves_ids_as_robot_reads_them() {
    for (command, input, args, fixture) in [
        ("convert", "obo-data-restrictions.obo", &[][..], "obo-data-restrictions.obo.robot.ofn"),
        ("convert", "obo-property-axioms.obo", &[], "obo-property-axioms.obo.robot.ofn"),
        ("convert", "obo-terms.obo", &[], "obo-terms.obo.robot.ttl"),
        ("remove", "obo-terms.obo", &["--select", "classes"], "obo-terms.obo.remove-classes.robot.ofn"),
        ("collapse", "obo-prefixes.obo", &[], "obo-prefixes.obo.collapse.robot.ofn"),
        ("convert", "obo-bare-ids.obo", &[], "obo-bare-ids.obo.robot.ofn"),
        ("convert", "obo-bare-ids-iri.obo", &[], "obo-bare-ids-iri.obo.robot.ofn"),
    ] {
        let out = tmp(&format!("obo-read.{}", fixture.rsplit('.').next().unwrap()));
        let run = bin().arg(command).arg("-i").arg(robot_fixture(input)).args(args).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{command} {input} {args:?}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(fixture), "{command} {input} {args:?}");
        let _ = std::fs::remove_file(&out);
    }
}

/// Turtle types an undeclared annotation property after its annotations when it
/// is the subject of a sub-property, domain or range axiom, and before them
/// otherwise; an undeclared object or data property is typed first. As ROBOT
/// 1.9.11 writes `undeclared-properties`.
#[test]
fn turtle_types_an_undeclared_annotation_property_after_its_annotations() {
    assert_eq!(
        convert_fixture("undeclared-properties.ofn", "undeclared-properties.ttl", &[]),
        fixture_text("undeclared-properties.robot.ttl")
    );
}

/// An RDF document stating a property equivalence and a property disjointness
/// from both ends holds each axiom once, and every syntax writes it once, as
/// ROBOT 1.9.11 writes `symmetric-pairs`.
#[test]
fn axioms_stated_from_both_ends_are_written_once() {
    for ext in ["ofn", "owl", "obo"] {
        assert_eq!(
            convert_fixture("symmetric-pairs.owl", &format!("symmetric-pairs-written.{ext}"), &[]),
            fixture_text(&format!("symmetric-pairs.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A disjointness of more than two properties is an `owl:AllDisjointProperties`
/// node. With an inverse member it is a root of the frame of the property that
/// member names, in node order among the frame's others, and of the first such
/// frame when two members are inverses; with none it is a general axiom. Its
/// annotations, nested ones included, are statements of the node. As ROBOT
/// 1.9.11 writes `all-disjoint-inverse.ofn` in RDF/XML and Turtle.
#[test]
fn a_disjointness_with_an_inverse_member_is_stated_in_the_frame_the_inverse_names() {
    for ext in ["owl", "ttl"] {
        assert_eq!(
            convert_fixture("all-disjoint-inverse.ofn", &format!("all-disjoint-inverse.{ext}"), &[]),
            fixture_text(&format!("all-disjoint-inverse.robot.{ext}")),
            "{ext}"
        );
    }
}

/// An annotated disjointness of more than two properties, one an inverse, is
/// written by the RDF layout without a warning, and comes back with its
/// annotation.
#[test]
fn an_annotated_disjointness_of_properties_comes_back_with_its_annotation() {
    let src = tmp("annotated-disjoint-properties.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://example.org/t#>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/t>\nDeclaration(ObjectProperty(:p))\nDeclaration(ObjectProperty(:q))\n\
         Declaration(ObjectProperty(:r))\n\
         DisjointObjectProperties(Annotation(rdfs:comment \"c\") :p ObjectInverseOf(:q) :r)\n)\n",
    )
    .unwrap();
    for ext in ["owl", "ttl"] {
        let out = tmp(&format!("annotated-disjoint-properties.{ext}"));
        let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!stderr.contains("cannot state"), "{ext}: {stderr}");
        let back = tmp(&format!("annotated-disjoint-properties-{ext}.ofn"));
        let run = bin().args(["convert", "-i"]).arg(&out).arg("-o").arg(&back).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&back).unwrap();
        assert!(
            text.contains("DisjointObjectProperties(Annotation(rdfs:comment \"c\") :p :r ObjectInverseOf(:q))"),
            "{ext}:\n{text}"
        );
    }
}

/// `remove --axioms external` removes every assertion about an anonymous
/// individual, which is in no base namespace whatever the class or property
/// the assertion names, and keeps an assertion about an internal individual
/// whose object is anonymous: as ROBOT 1.9.11 removes them from the document
/// in functional syntax and in RDF/XML. Read from RDF/XML, the individual it
/// keeps is numbered as owlmake's reader numbers it.
#[test]
fn assertions_about_anonymous_individuals_are_external() {
    fn unlabelled(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find("_:genid") {
            out.push_str(&rest[..at + "_:".len()]);
            rest = rest[at + "_:genid".len()..].trim_start_matches(|c: char| c.is_ascii_digit());
        }
        out + rest
    }
    for (src, expected) in [
        ("ofn", "remove-external-anonymous.removed.ofn"),
        ("owl", "remove-external-anonymous.owl.removed.ofn"),
    ] {
        let out = tmp(&format!("remove-external-anonymous-{src}.ofn"));
        let run = bin()
            .args(["remove", "-i"])
            .arg(robot_fixture(&format!("remove-external-anonymous.{src}")))
            .args(["--base-iri", "http://example.org/x/E_", "--axioms", "external", "-o"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        if src == "ofn" {
            assert_eq!(text, fixture_text(expected), "{src}");
        } else {
            assert_eq!(unlabelled(&text), unlabelled(&fixture_text(expected)), "{src}");
        }
    }
}

/// `filter` keeps the root ontology's own axioms: a term the import closure
/// declares still selects, but what the import states stays with the import,
/// and the result imports nothing unless `--select imports` keeps the root's
/// `owl:imports`. As ROBOT 1.9.11 filters.
#[test]
fn filter_keeps_the_root_axioms_and_its_imports_only_when_selected() {
    for (expected, args) in [
        ("term-closure", &["--term", "http://example.org/int/K"][..]),
        ("terms", &["--term", "http://example.org/int/A", "--term", "http://example.org/int/K"]),
        ("select-imports", &["--term", "http://example.org/int/A", "--select", "self imports"]),
        ("all", &["--axioms", "all"]),
    ] {
        let out = tmp(&format!("filter-imports-{expected}.ofn"));
        let run = bin()
            .args(["filter", "--catalog"])
            .arg(robot_fixture("filter-imports-catalog.xml"))
            .arg("-i")
            .arg(robot_fixture("filter-imports.ofn"))
            .args(args)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(text, fixture_text(&format!("filter-imports.{expected}.ofn")), "{expected}");
    }
}

/// A functional-syntax banner names an entity by the label the document gives
/// it as written or, where it gives none, by the label a document opened with
/// it gives, an import's even after `--select imports`; a label that spans
/// lines stays a comment. An OBO comment, and the `id:` line of a stanza with
/// no `name:`, take an import's label only while the document imports it. As
/// ROBOT 1.9.11 writes `remove` over `banner-labels.ofn`.
#[test]
fn banners_and_obo_comments_take_the_labels_robot_takes() {
    for (tag, args) in [
        (
            "remove-labels",
            &[
                "--term",
                "http://purl.obolibrary.org/obo/EX_1",
                "--term",
                "http://purl.obolibrary.org/obo/EX_2",
                "--axioms",
                "annotation",
            ][..],
        ),
        (
            "remove-imports",
            &["--select", "imports", "--term", "http://purl.obolibrary.org/obo/EX_1", "--axioms", "annotation"],
        ),
    ] {
        for format in ["ofn", "obo"] {
            let out = tmp(&format!("banner-labels.{tag}.{format}"));
            let run = bin()
                .args(["remove", "--catalog"])
                .arg(robot_fixture("banner-labels-catalog.xml"))
                .arg("-i")
                .arg(robot_fixture("banner-labels.ofn"))
                .args(args)
                .arg("-o")
                .arg(&out)
                .output()
                .unwrap();
            assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
            let text = std::fs::read_to_string(&out).unwrap();
            let _ = std::fs::remove_file(&out);
            assert_eq!(text, fixture_text(&format!("banner-labels.{tag}.robot.{format}")), "{tag} {format}");
        }
    }
}

/// An entity whose `rdfs:label` is an IRI is named by that IRI's short form:
/// what follows its namespace, else what follows its last `/`, else the IRI in
/// angle brackets. A literal label names it before any IRI does. As ROBOT
/// 1.9.11 names the entities of `iri-labels` in a functional-syntax banner, in
/// `explain`'s report and in a markdown `diff`; only the diff's `Loaded from`
/// lines, which name where each side was read, are not compared.
#[test]
fn an_iri_label_names_an_entity_by_its_short_form() {
    assert_eq!(convert_fixture("iri-labels.ofn", "iri-labels.ofn", &[]), fixture_text("iri-labels.robot.ofn"));

    let md = tmp("iri-labels.explain.md");
    let run = bin()
        .args(["explain", "-i"])
        .arg(robot_fixture("iri-labels.ofn"))
        .args(["-M", "unsatisfiability", "-u", "all", "--explanation"])
        .arg(&md)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&md).unwrap(), fixture_text("iri-labels.robot.md"));
    let _ = std::fs::remove_file(&md);

    let out = tmp("iri-labels.diff.md");
    let run = bin()
        .args(["diff", "--left"])
        .arg(robot_fixture("iri-labels.ofn"))
        .arg("--right")
        .arg(robot_fixture("iri-labels-right.ofn"))
        .args(["-f", "markdown", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let comparable = |text: String| -> String {
        text.lines().filter(|l| !l.starts_with("- Loaded from: ")).map(|l| format!("{l}\n")).collect()
    };
    assert_eq!(
        comparable(std::fs::read_to_string(&out).unwrap()),
        comparable(fixture_text("iri-labels.diff.robot.md"))
    );
    let _ = std::fs::remove_file(&out);
}

/// A plain diff writes each axiom as OWL API's `toString()` does, and a pretty
/// one as its functional renderer does, naming every entity by its short form,
/// an OBO id as a CURIE, in angle brackets, then its label: `--labels` asks for
/// the pretty report, and `--label-langs-priority` picks each label by its
/// language. A side compares its own axioms and imports, and draws labels from
/// the ontologies it imports. An unnamed ontology's ID is numbered as the
/// documents read before it, and its own syntax, number it. A literal compares
/// and is written as its side's syntax reads it. As ROBOT 1.9.11 diffs
/// `diff-render`, `diff-imports`, `diff-unnamed` and Turtle fixtures against
/// `diff-render-left`, `diff-unnamed` against `diff-imports`, and
/// `custom-prefixes` in Turtle against OBO.
#[test]
fn diff_writes_each_report_as_robot_does() {
    let between = |left: &str, right: &str, args: &[&str], expected: &str| {
        let out = tmp(&format!("{expected}.txt"));
        let run = bin()
            .args(["diff", "--left"])
            .arg(robot_fixture(left))
            .arg("--right")
            .arg(robot_fixture(right))
            .args(args)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{expected}: {}", String::from_utf8_lossy(&run.stderr));
        let comparable = |text: String| -> String {
            text.lines().filter(|l| !l.starts_with("- Loaded from: ")).map(|l| format!("{l}\n")).collect()
        };
        assert_eq!(
            comparable(std::fs::read_to_string(&out).unwrap()),
            comparable(fixture_text(expected)),
            "{expected}"
        );
        let _ = std::fs::remove_file(&out);
    };
    let run = |right: &str, args: &[&str], expected: &str| between("diff-render-left.ofn", right, args, expected);
    run("diff-render.ofn", &[], "diff-render.plain.robot.txt");
    run("diff-render.ofn", &["-f", "pretty"], "diff-render.pretty.robot.txt");
    run("diff-render.ofn", &["--labels", "true"], "diff-render.pretty.robot.txt");
    run("diff-render.ofn", &["-f", "pretty", "--label-langs-priority", "en,none"], "diff-render.langs.robot.txt");
    let catalog = robot_fixture("diff-imports-catalog.xml");
    let catalog = catalog.to_str().unwrap();
    run("diff-imports.ofn", &["--right-catalog", catalog], "diff-imports.plain.robot.txt");
    run("diff-imports.ofn", &["--right-catalog", catalog, "-f", "pretty"], "diff-imports.pretty.robot.txt");
    run("diff-imports.ofn", &["--right-catalog", catalog, "-f", "markdown"], "diff-imports.robot.md");
    // The plain report writes a cardinality's `owl:Thing` or `rdfs:Literal`
    // filler, an annotation property's range in full, a one-member set axiom
    // and a one-operand union as they stand; the pretty one names the entities
    // a declaration's annotation names by their entity types, and writes no
    // one-member set axiom.
    run("diff-styles.ofn", &[], "diff-styles.plain.robot.txt");
    run("diff-styles.ofn", &["-f", "pretty"], "diff-styles.pretty.robot.txt");
    run("rdf-connective-members.owl", &[], "rdf-connective-members.diff.plain.robot.txt");
    // An unnamed ontology is numbered by the ontology IDs reading both sides
    // mints, the left side first.
    between("diff-unnamed.ofn", "diff-render-left.ofn", &[], "diff-unnamed-left.robot.txt");
    between("diff-unnamed.omn", "diff-render-left.ofn", &[], "diff-unnamed-left-omn.robot.txt");
    between(
        "diff-imports.ofn",
        "diff-unnamed.owl",
        &["--left-catalog", catalog],
        "diff-unnamed-right.robot.txt",
    );
    between("diff-render-left.ofn", "diff-unnamed.omn", &["-f", "pretty"], "diff-unnamed-right-omn.robot.txt");
    // A literal typed `xsd:string` equals the untyped literal; each side
    // states its literals as its syntax reads them, Turtle and OBO typing an
    // untyped one, and orders an axiom's annotations by those types.
    between("custom-prefixes.ttl", "custom-prefixes.obo", &[], "custom-prefixes.diff.robot.txt");
    between("diff-render-left.ofn", "custom-prefixes.ttl", &[], "custom-prefixes.diff.plain.robot.txt");
    between("diff-render-left.ofn", "custom-prefixes.ttl", &["-f", "pretty"], "custom-prefixes.diff.pretty.robot.txt");
    between(
        "diff-render-left.ofn",
        "export-typed-label-annotations.ttl",
        &["-f", "pretty"],
        "export-typed-label-annotations.diff.pretty.robot.txt",
    );
    // The markdown report lists each change in its subject's frame, in
    // Manchester syntax with each entity linked by its label: an inverse
    // property, an anonymous individual and a datatype definition's data range
    // are subjects of their own, an anonymous class's axioms are GCIs. Frames
    // sort by their headers, tied ones in the order the report's frame map
    // iterates in; an object's annotations nest under it in the order a hash
    // set of them iterates in.
    run("diff-render.ofn", &["-f", "markdown"], "diff-render.robot.md");
    run("diff-markdown.ofn", &["-f", "markdown"], "diff-markdown.robot.md");
    run("diff-markdown-axioms.ofn", &["-f", "markdown"], "diff-markdown-axioms.robot.md");
    between("diff-markdown.ofn", "diff-render-left.ofn", &["-f", "markdown"], "diff-markdown-removed.robot.md");
    run("annotated-terms.omn.ofn", &["-f", "markdown"], "annotated-terms.diff.robot.md");
    run("diff-headless-rule.ofn", &[], "diff-headless-rule.plain.robot.txt");
}

/// A rule with an empty head has no subject, so the markdown report has no
/// frame to list it in: the command fails and writes no report.
#[test]
fn diff_markdown_refuses_a_rule_with_an_empty_head() {
    let out = tmp("diff-headless-rule.md");
    let run = bin()
        .args(["diff", "--left"])
        .arg(robot_fixture("diff-render-left.ofn"))
        .arg("--right")
        .arg(robot_fixture("diff-headless-rule.ofn"))
        .args(["-f", "markdown", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("a rule with an empty head"));
    assert!(!out.exists());
}

/// `filter`'s bridges join the result whatever axiom types `--axioms` names:
/// a selected class keeps its path to its nearest selected superclass, through
/// classes dropped or directly, and a property likewise. Under `internal` or
/// `external` only the bridges whose subject is in, or outside, the base
/// namespaces join. The hierarchy bridged is the root ontology's own, so an
/// imported superclass link carries no bridge. As ROBOT 1.9.11 filters.
#[test]
fn filter_bridges_whatever_axiom_types_are_named() {
    let run = |input: &str, args: &[&str], expected: &str| {
        let out = tmp(&format!("{expected}.ofn"));
        let run = bin()
            .args(["filter", "--catalog"])
            .arg(robot_fixture("filter-imports-catalog.xml"))
            .arg("-i")
            .arg(robot_fixture(input))
            .args(args)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(text, fixture_text(&format!("{expected}.ofn")), "{expected}");
    };
    const C: &str = "http://example.org/int/C";
    const A: &str = "http://example.org/int/A";
    let label = "rdfs:label";
    run(
        "filter-bridges.ofn",
        &["--term", C, "--term", A, "--term", label, "--axioms", "annotation"],
        "filter-bridges.annotation",
    );
    run(
        "filter-bridges.ofn",
        &["--term", C, "--term", "http://example.org/int/B", "--term", label, "--axioms", "annotation"],
        "filter-bridges.direct",
    );
    run(
        "filter-bridges.ofn",
        &["--term", "http://example.org/int/r", "--term", "http://example.org/int/p", "--axioms", "annotation"],
        "filter-bridges.properties",
    );
    run(
        "filter-bridges.ofn",
        &[
            "--base-iri",
            "http://example.org/int/",
            "--term",
            C,
            "--term",
            A,
            "--term",
            "http://example.org/ext/D",
            "--axioms",
            "external annotation",
        ],
        "filter-bridges.external",
    );
    run("filter-imports-bridge.ofn", &["--term", C, "--term", A], "filter-imports-bridge.terms");
}

/// A bridge reaches a superclass expression only when everything it names is
/// still an object: its classes, properties and individuals, the datatypes its
/// data ranges name, and the datatypes of the literals a data one-of or facet
/// holds — though not the value of a data has-value. For `remove` the objects
/// are what the surviving axioms mention, a literal's datatype included where it
/// is logical content; for `filter`, what it selected. As ROBOT 1.9.11 bridges.
#[test]
fn bridges_reach_only_expressions_whose_entities_remain() {
    const B: &str = "http://example.org/B";
    for (input, expected, args) in [
        ("bridges-data.ofn", "bridges-data.remove-B", &["remove", "--term", B][..]),
        ("bridges-data.ofn", "bridges-data.remove-B-d", &["remove", "--term", B, "--term", "http://example.org/d"]),
        ("bridges-data.ofn", "bridges-data.remove-B-y", &["remove", "--term", B, "--term", "http://example.org/y"]),
        ("bridges-datatypes.ofn", "bridges-datatypes.remove-B", &["remove", "--term", B]),
        (
            "bridges-datatypes.ofn",
            "bridges-datatypes.filter",
            &[
                "filter",
                "--term",
                "http://example.org/C",
                "--term",
                "http://example.org/A",
                "--term",
                "http://example.org/d",
                "--term",
                "http://example.org/p",
            ],
        ),
    ] {
        let out = tmp(&format!("{expected}.ofn"));
        let run = bin()
            .args(&args[..1])
            .arg("-i")
            .arg(robot_fixture(input))
            .args(&args[1..])
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(text, fixture_text(&format!("{expected}.ofn")), "{expected}");
    }
}

/// A Turtle document is read as it is written: its anonymous individuals are
/// numbered in the order the document first mentions them, and each typed
/// literal keeps its lexical form and its datatype (`"007"^^xsd:integer`,
/// `"1.50"^^xsd:decimal`, `"+5"^^xsd:int`). As ROBOT 1.9.11 reads it, in
/// functional syntax, RDF/XML and Turtle.
#[test]
fn a_turtle_document_is_read_in_its_own_order_and_literals() {
    for name in ["turtle-anonymous-order", "turtle-literals"] {
        for ext in ["ofn", "owl", "ttl"] {
            let out = tmp(&format!("{name}.{ext}"));
            let run = bin()
                .args(["convert", "-i"])
                .arg(robot_fixture(&format!("{name}.ttl")))
                .arg("-o")
                .arg(&out)
                .output()
                .unwrap();
            assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
            let text = std::fs::read_to_string(&out).unwrap();
            let _ = std::fs::remove_file(&out);
            assert_eq!(text, fixture_text(&format!("{name}.robot.{ext}")), "{name}.{ext}");
        }
    }
}

/// A Turtle document's prefixes are the directives it makes, `@prefix` and
/// SPARQL-style `PREFIX`, wherever they stand: the same text inside a string
/// literal, short or long, an IRI or a comment declares nothing. As ROBOT
/// 1.9.11 reads it, in functional syntax, RDF/XML and Turtle.
#[test]
fn prefixes_quoted_in_a_turtle_document_declare_nothing() {
    for ext in ["ofn", "owl", "ttl"] {
        let out = tmp(&format!("turtle-quoted-prefixes.{ext}"));
        let run = bin()
            .args(["convert", "-i"])
            .arg(robot_fixture("turtle-quoted-prefixes.ttl"))
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(text, fixture_text(&format!("turtle-quoted-prefixes.robot.{ext}")), "{ext}");
    }
}

/// An RDF/XML document's prefixes are the entities its internal subset
/// declares and the namespaces every element declares, a declaration reading
/// its entity references as any attribute does; and an element's name in a
/// default namespace given by entity is read through it. As ROBOT 1.9.11 reads
/// `rdf-declared-prefixes`, in functional syntax, Turtle, RDF/XML and as OBO
/// `idspace:` lines.
#[test]
fn an_rdfxml_documents_prefixes_are_its_entities_and_every_namespace_declaration() {
    for ext in ["ofn", "ttl", "owl"] {
        assert_eq!(
            convert_fixture("rdf-declared-prefixes.rdf", &format!("rdf-declared-prefixes.{ext}"), &[]),
            fixture_text(&format!("rdf-declared-prefixes.robot.{ext}")),
            "{ext}"
        );
    }
    // The individual is an `[Instance]` frame, which ROBOT does not write.
    let obo = convert_fixture("rdf-declared-prefixes.rdf", "rdf-declared-prefixes.obo", &[]);
    assert_eq!(obo.split("[Instance]").next().unwrap(), fixture_text("rdf-declared-prefixes.robot.obo"));
}

/// An inverse-properties axiom relates a pair, not an ordered couple:
/// `p owl:inverseOf q` and `q owl:inverseOf p` are one axiom, written with its
/// properties in the order the object model keeps them. As ROBOT 1.9.11 reads
/// `rdf-inverse-pairs`, in functional syntax, RDF/XML and Turtle.
#[test]
fn an_inverse_pair_stated_either_way_is_one_axiom() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-inverse-pairs.rdf", &format!("rdf-inverse-pairs.{ext}"), &[]),
            fixture_text(&format!("rdf-inverse-pairs.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A `file:` IRI given as an input names a file, which is read where any
/// other IRI would be fetched: `file:/abs`, `file:///abs` and a relative
/// `file:rel` alike. As ROBOT 1.9.11 converts `rdf-inverse-pairs` from
/// `--input-iri` and diffs `diff-unnamed` from `--left-iri`.
#[test]
fn a_file_iri_input_is_read_from_its_file() {
    let source = robot_fixture("rdf-inverse-pairs.rdf");
    for iri in [format!("file:{}", source.display()), format!("file://{}", source.display())] {
        let path = tmp("file-iri-input.ofn");
        let run = bin().args(["convert", "--input-iri", &iri, "-o"]).arg(&path).output().unwrap();
        assert!(run.status.success(), "{iri}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), fixture_text("rdf-inverse-pairs.robot.ofn"), "{iri}");
        let _ = std::fs::remove_file(&path);
    }
    let run = bin()
        .current_dir(robot_fixture(""))
        .args(["diff", "--left-iri", "file:diff-unnamed.ofn", "--right", "diff-render-left.ofn"])
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), fixture_text("diff-unnamed-left.robot.txt"));
}

/// A same-individuals pair is written with the member whose frame it stands in
/// first; a pair of anonymous individuals stands in none and is written turned
/// round. A different-individuals pair is written in order. As ROBOT 1.9.11
/// converts `same-individual-pairs` to functional syntax.
#[test]
fn a_same_individual_pair_of_anonymous_individuals_is_written_turned_round() {
    assert_eq!(
        convert_fixture("same-individual-pairs.ofn", "same-individual-pairs.ofn", &[]),
        fixture_text("same-individual-pairs.robot.ofn")
    );
}

/// A document that states no base of its own resolves its relative IRIs
/// against its own IRI: `file:` and its path. As ROBOT 1.9.11 converts
/// `rdf-document-base` and `ttl-document-base`, in functional syntax, RDF/XML
/// and Turtle, read where the suite reads them; the recorded outputs name the
/// fixture directory `{fixtures}`.
#[test]
fn a_documents_relative_iris_resolve_against_its_own_iri() {
    let fixtures = format!("file:{}/", robot_fixture("").display().to_string().trim_end_matches('/'));
    for src in ["rdf-document-base.owl", "ttl-document-base.ttl"] {
        let stem = src.rsplit_once('.').unwrap().0;
        for ext in ["ofn", "owl", "ttl"] {
            assert_eq!(
                convert_fixture(src, &format!("{stem}.{ext}"), &[]).replace(&fixtures, "{fixtures}/"),
                fixture_text(&format!("{stem}.robot.{ext}")),
                "{src} as {ext}"
            );
        }
    }
}

/// A full IRI is every character up to the next `>`. Functional syntax reads a
/// relative one, a prefix declaration's among them, after the default prefix
/// bound before it, as `rdf-document-base` and `ttl-document-base` show once
/// written with the `{fixtures}` placeholder. Manchester syntax reads one with
/// no colon as a local name of the default prefix, angle brackets and all,
/// takes the header's IRIs, a prefix's and an annotation value as written, and
/// names a rule variable `?n` `urn:swrl:var#n`. An ontology or version IRI that
/// is still relative is made absolute by prefixing `urn:absolute:`, with an
/// error logged, in every reader and in `annotate`, `filter`, `extract` and
/// `template`; one that labels a blank node names no IRI. As ROBOT 1.9.11 reads
/// and writes them.
#[test]
fn relative_iris_are_read_as_robot_reads_them() {
    let run = |args: &[&str], out: &str| {
        let path = tmp(out);
        let mut cmd = bin();
        for arg in args {
            match arg.strip_prefix('@') {
                Some(fixture) => cmd.arg(robot_fixture(fixture)),
                None => cmd.arg(arg),
            };
        }
        let output = cmd.arg("-o").arg(&path).output().unwrap();
        let written = std::fs::read_to_string(&path).ok();
        let _ = std::fs::remove_file(&path);
        (output, written)
    };
    let cases: &[(&[&str], &str, &[&str])] = &[
        (&["convert", "-i", "@relative-iris.ofn"], "relative-iris.robot.ofn", &[]),
        (&["convert", "-i", "@relative-ontology-iri.ofn"], "relative-ontology-iri.robot.ofn", &["rel/o.owl", "rel/v.owl"]),
        (&["convert", "-i", "@relative-ontology-iri.owx"], "relative-ontology-iri.owx.robot.ofn", &["rel/o.owl", "rel/v.owl"]),
        (&["convert", "-i", "@relative-iris.omn"], "relative-iris.omn.robot.ofn", &["rel/o.owl", "rel/v.owl"]),
        (
            &["convert", "-i", "@rdf-document-base.robot.ofn"],
            "rdf-document-base.robot.ofn.robot.ofn",
            &["{fixtures}/rdf-document-base.owl#{fixtures}/rdf-document-base.owl"],
        ),
        (
            &["convert", "-i", "@ttl-document-base.robot.ofn"],
            "ttl-document-base.robot.ofn.robot.ofn",
            &["{fixtures}/ttl-document-base.ttl#{fixtures}/ttl-document-base.ttl"],
        ),
        (
            &["annotate", "-i", "@ontology-iri-options.ofn", "--ontology-iri", "rel/o.owl", "--version-iri", "rel/v.owl"],
            "ontology-iri-options.annotate.robot.ofn",
            &["rel/o.owl", "rel/v.owl"],
        ),
        (
            &["annotate", "-i", "@ontology-iri-options.ofn", "--version-iri", "rel/v.owl"],
            "ontology-iri-options.annotate-version.robot.ofn",
            &["rel/v.owl"],
        ),
        (
            &["filter", "-i", "@ontology-iri-options.ofn", "--term", "http://example.org/ontology-iri-options#A", "--ontology-iri", "rel/o.owl"],
            "ontology-iri-options.filter.robot.ofn",
            &["rel/o.owl"],
        ),
        (
            &["extract", "-i", "@ontology-iri-options.ofn", "--method", "STAR", "--term", "http://example.org/ontology-iri-options#A", "--output-iri", "rel/o.owl"],
            "ontology-iri-options.extract.robot.ofn",
            &["rel/o.owl"],
        ),
        (
            &["template", "--template", "@ontology-iri-options.tsv", "--ontology-iri", "rel/o.owl", "--version-iri", "rel/v.owl"],
            "ontology-iri-options.template.robot.ofn",
            &["rel/o.owl", "rel/v.owl"],
        ),
        (
            &["annotate", "-i", "@ontology-iri-options.ofn", "--ontology-iri", "_:genid1"],
            "ontology-iri-options.annotate-genid.robot.ofn",
            &[],
        ),
        (&["convert", "-i", "@ontology-iri-genid.ofn"], "ontology-iri-genid.robot.ofn", &[]),
    ];
    for (args, expected, relative) in cases {
        let (output, written) = run(args, &format!("relative-iris-{expected}"));
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(written.as_deref(), Some(fixture_text(expected).as_str()), "{args:?}");
        let logged: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|l| l.split_once(" ERROR org.semanticweb.owlapi.model.OWLOntologyID - ").map(|(_, m)| m.to_string()))
            .collect();
        let expected_log: Vec<String> = relative
            .iter()
            .map(|iri| {
                format!("Ontology IRIs must be absolute; IRI {iri} is relative and will be made absolute by prefixing urn:absolute: to it")
            })
            .collect();
        assert_eq!(logged, expected_log, "{args:?}");
    }
    // A version IRI with no ontology IRI is refused, and nothing is written.
    for args in [
        &["annotate", "-i", "@ontology-iri-options.ofn", "--ontology-iri", "_:genid1", "--version-iri", "http://example.org/v.owl"][..],
        &["convert", "-i", "@ontology-iri-genid-version.ofn"],
        &["annotate", "-i", "@ontology-iri-anonymous.ofn", "--version-iri", "http://example.org/v.owl"],
    ] {
        let (output, written) = run(args, "relative-iris-refused.ofn");
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("If the ontology IRI is null then it is not possible to specify a version IRI"),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(written, None, "{args:?}");
    }
}

/// Every node typed `owl:Ontology` states the ontology's annotations and
/// imports, a blank one as well as a named one, and the first the document
/// states names the ontology: no IRI when it is blank, and only its own version
/// IRI. As ROBOT 1.9.11 converts `rdf-ontology-headers`, whose blank header
/// comes first, and `rdf-ontology-headers-named`, whose first header is named,
/// in functional syntax, RDF/XML and Turtle.
#[test]
fn every_ontology_header_states_the_ontologys_annotations_and_imports() {
    let catalog = robot_fixture("rdf-ontology-headers-catalog.xml").display().to_string();
    for stem in ["rdf-ontology-headers", "rdf-ontology-headers-named"] {
        for ext in ["ofn", "owl", "ttl"] {
            assert_eq!(
                convert_fixture(&format!("{stem}.owl"), &format!("{stem}.{ext}"), &["--catalog", &catalog]),
                fixture_text(&format!("{stem}.robot.{ext}")),
                "{stem} as {ext}"
            );
        }
    }
}

/// A document that imports is written among the ontologies it imports,
/// directly or not. Functional syntax and OWL/XML declare every entity of the
/// document's signature and its imports' that nothing declares: an imported
/// ontology's undeclared classes and annotation property, and the datatype of a
/// literal, OWL/XML in hash order after the document's own declarations.
/// RDF/XML and Turtle state no type for an entity an import has in its
/// signature, a literal's datatype included, and RDF/XML binds a prefix for the
/// namespace of an imported annotation property. As ROBOT 1.9.11 converts
/// `imports-closure`, which imports a chain of two ontologies, in all four.
#[test]
fn a_document_is_written_among_the_ontologies_it_imports() {
    let catalog = robot_fixture("imports-closure-catalog.xml").display().to_string();
    for ext in ["ofn", "owx", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("imports-closure.ofn", &format!("imports-closure.{ext}"), &["--catalog", &catalog]),
            fixture_text(&format!("imports-closure.robot.{ext}")),
            "{ext}"
        );
    }
}

/// An intersection, union or enumeration holds the members its list names,
/// however many: none (`rdf:nil`), one or more, on a named class or an
/// anonymous one, and in a data range. Functional syntax writes a one-member
/// union or intersection as its member, RDF an empty list as `rdf:nil`. As
/// ROBOT 1.9.11 converts `rdf-connective-members`, in functional syntax,
/// RDF/XML, Turtle, OWL/XML and Manchester.
#[test]
fn a_connective_holds_the_members_its_list_names() {
    for ext in ["ofn", "owl", "ttl", "owx", "omn"] {
        assert_eq!(
            convert_fixture("rdf-connective-members.owl", &format!("rdf-connective-members.{ext}"), &[]),
            fixture_text(&format!("rdf-connective-members.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A literal typed `xsd:string` sorts by that datatype, after `xsd:anyURI`,
/// where the untyped literal of its text sorts by `rdf:PlainLiteral`; both are
/// written untyped. The two are one literal, so of the two statements the first
/// is kept, and an `owl:Axiom` block typing a statement's literal the other way
/// names that statement and gives the axiom its literal. As ROBOT 1.9.11
/// converts `rdf-typed-strings`, in functional syntax, RDF/XML and Turtle.
#[test]
fn a_string_typed_xsd_string_sorts_as_typed_and_is_its_untyped_text() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-typed-strings.owl", &format!("rdf-typed-strings.{ext}"), &[]),
            fixture_text(&format!("rdf-typed-strings.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A property chain stated twice, each statement a list of its own, and
/// reified once is one axiom, annotated. As ROBOT 1.9.11 converts
/// `rdf-restated-chain`, in functional syntax, RDF/XML and Turtle.
#[test]
fn a_chain_stated_twice_and_reified_once_is_one_annotated_axiom() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-restated-chain.owl", &format!("rdf-restated-chain.{ext}"), &[]),
            fixture_text(&format!("rdf-restated-chain.robot.{ext}")),
            "{ext}"
        );
    }
}

/// Of two axioms that differ only in typing a string `xsd:string`, the
/// ontology holds the one its document states first, in functional syntax,
/// Manchester syntax and OWL/XML alike. As ROBOT 1.9.11 converts
/// `typed-string-duplicates` from each to functional syntax.
#[test]
fn of_a_typed_and_an_untyped_string_the_first_stated_is_kept() {
    for ext in ["ofn", "omn", "owx"] {
        assert_eq!(
            convert_fixture(
                &format!("typed-string-duplicates.{ext}"),
                &format!("typed-string-duplicates-{ext}.ofn"),
                &[]
            ),
            fixture_text(&format!("typed-string-duplicates.{ext}.robot.ofn")),
            "{ext}"
        );
    }
}

/// An import names its ontology by the full IRI in functional syntax, whatever
/// prefix would abbreviate it; Turtle abbreviates it as it does any resource.
/// As ROBOT 1.9.11 converts `import-full-iri`, in all four.
#[test]
fn an_import_names_its_ontology_by_the_full_iri() {
    let catalog = robot_fixture("imports-closure-catalog.xml").display().to_string();
    for ext in ["ofn", "owl", "owx", "ttl"] {
        assert_eq!(
            convert_fixture("import-full-iri.ofn", &format!("import-full-iri.{ext}"), &["--catalog", &catalog]),
            fixture_text(&format!("import-full-iri.robot.{ext}")),
            "{ext}"
        );
    }
}

/// An `owl:Axiom` block whose source is an anonymous class stating the edge it
/// names, and whose target is a copy of the class at the other end, means one
/// annotated axiom: an equivalence, a disjointness and a general subclass axiom.
/// As ROBOT 1.9.11 converts `rdf-reified-anonymous-classes`, in functional
/// syntax, RDF/XML and Turtle.
#[test]
fn a_reification_of_an_anonymous_class_with_a_copied_target_keeps_its_annotation() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture(
                "rdf-reified-anonymous-classes.owl",
                &format!("rdf-reified-anonymous-classes.{ext}"),
                &[]
            ),
            fixture_text(&format!("rdf-reified-anonymous-classes.robot.{ext}")),
            "{ext}"
        );
    }
}

/// An `owl:Axiom` block means its axiom whether or not the document states the
/// triple it names, an anonymous individual's included: a difference, a
/// sameness and a property assertion whose object is a node stating nothing,
/// and a type of a node with a label and of one with nothing else. As ROBOT
/// 1.9.11 converts `rdf-reified-anonymous`, in functional syntax, RDF/XML and
/// Turtle.
#[test]
fn a_reified_statement_of_an_anonymous_individual_needs_no_stated_triple() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-reified-anonymous.owl", &format!("rdf-reified-anonymous.{ext}"), &[]),
            fixture_text(&format!("rdf-reified-anonymous.robot.{ext}")),
            "{ext}"
        );
    }
}

/// Every annotated general axiom is its own shared node: two of them with one
/// target, an equivalence of two anonymous classes beside them and a
/// disjointness stated twice with different comments each name a node of their
/// own, numbered in the order the axioms are written. As ROBOT 1.9.11 converts
/// `general-axiom-nodes`, in functional syntax, RDF/XML and Turtle.
#[test]
fn every_annotated_general_axiom_names_a_node_of_its_own() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("general-axiom-nodes.ofn", &format!("general-axiom-nodes.{ext}"), &[]),
            fixture_text(&format!("general-axiom-nodes.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A member of a same-individuals axiom or an equivalence is a root of its
/// host's graph, a block of its own after the host's, which states its type
/// when the document declares it nowhere. As ROBOT 1.9.11 converts
/// `rdf-root-block-types`, pairs and chains of individuals, classes, object
/// and data properties, each with and without a declared member, in
/// functional syntax, RDF/XML and Turtle.
#[test]
fn a_root_block_states_the_type_of_an_undeclared_member() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("rdf-root-block-types.ofn", &format!("rdf-root-block-types.{ext}"), &[]),
            fixture_text(&format!("rdf-root-block-types.robot.{ext}")),
            "{ext}"
        );
    }
}

/// Two axioms that state the same edge, one annotated and one not, are two
/// axioms and one statement: RDF writes the edge once, beside the annotated
/// one's reification. As ROBOT 1.9.11 converts `annotated-edge-twice`, in
/// functional syntax, RDF/XML and Turtle.
#[test]
fn an_edge_stated_by_two_axioms_is_written_once() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("annotated-edge-twice.ofn", &format!("annotated-edge-twice.{ext}"), &[]),
            fixture_text(&format!("annotated-edge-twice.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A functional document's prefix declarations count with white space around
/// their parts, as the syntax allows. As ROBOT 1.9.11 converts
/// `ofn-spaced-prefixes`, an anonymous ontology declaring `Prefix(: = <…>)`,
/// in functional syntax, RDF/XML and Turtle.
#[test]
fn a_functional_documents_prefix_declarations_may_be_spaced() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("ofn-spaced-prefixes.ofn", &format!("ofn-spaced-prefixes.{ext}"), &[]),
            fixture_text(&format!("ofn-spaced-prefixes.robot.{ext}")),
            "{ext}"
        );
    }
}

/// A document with no default prefix of its own is written with one for the
/// ontology IRI: the IRI itself when it ends in `/` or `#` or holds a `#`
/// anywhere, and the IRI with `#` appended otherwise. As ROBOT 1.9.11 writes
/// it, in functional syntax and Turtle.
#[test]
fn the_default_prefix_gains_a_hash_only_when_the_ontology_iri_has_none() {
    for (n, iri, ns) in [
        (0, "http://example.org/o/", "http://example.org/o/"),
        (1, "http://example.org/o#", "http://example.org/o#"),
        (2, "http://example.org/a#b/c", "http://example.org/a#b/c"),
        (3, "http://example.org/o", "http://example.org/o#"),
    ] {
        let src = tmp(&format!("default-prefix-{n}.owl"));
        std::fs::write(
            &src,
            format!(
                "<?xml version=\"1.0\"?>\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:owl=\"http://www.w3.org/2002/07/owl#\">\n<owl:Ontology rdf:about=\"{iri}\"/>\n<owl:Class rdf:about=\"http://example.org/A\"/>\n</rdf:RDF>\n"
            ),
        )
        .unwrap();
        for (ext, line) in [("ofn", format!("Prefix(:=<{ns}>)")), ("ttl", format!("@prefix : <{ns}> ."))] {
            let out = tmp(&format!("default-prefix-{n}.{ext}"));
            let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
            assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
            let text = std::fs::read_to_string(&out).unwrap();
            let _ = std::fs::remove_file(&out);
            assert!(text.lines().any(|l| l == line), "{iri} as {ext}:\n{text}");
        }
        let _ = std::fs::remove_file(&src);
    }
}

/// A Turtle document may leave an anonymous class expression untyped: a node
/// with `owl:onProperty` is a restriction, and one with `owl:unionOf`,
/// `owl:intersectionOf`, `owl:complementOf` or `owl:oneOf` a class, nested or
/// not. As ROBOT 1.9.11 reads it, in functional syntax, RDF/XML and Turtle.
#[test]
fn untyped_class_expressions_in_turtle_are_read_by_their_predicates() {
    for ext in ["ofn", "owl", "ttl"] {
        let out = tmp(&format!("turtle-untyped-expressions.{ext}"));
        let run = bin()
            .args(["convert", "-i"])
            .arg(robot_fixture("turtle-untyped-expressions.ttl"))
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(text, fixture_text(&format!("turtle-untyped-expressions.robot.{ext}")), "{ext}");
    }
}

/// A blank node that does not say what it is reads as the expression its
/// predicates describe, in RDF/XML as in Turtle. A `someValuesFrom`,
/// `allValuesFrom` or `hasValue` restriction and a range take their kind from
/// what fills them, so a data property restricted to an enumeration of literals
/// is an object restriction to the empty enumeration; and the property is then
/// declared once, as what it was declared. As ROBOT 1.9.11 reads and writes them,
/// in functional syntax, RDF/XML and Turtle.
#[test]
fn untyped_expressions_and_fillers_are_read_as_their_shape_says() {
    for src in ["untyped-data-ranges.ttl", "untyped-data-ranges-rdfxml.rdf"] {
        let name = src.rsplit_once('.').unwrap().0;
        for ext in ["ofn", "owl", "ttl"] {
            let out = tmp(&format!("{name}.{ext}"));
            let run = bin().args(["convert", "-i"]).arg(robot_fixture(src)).arg("-o").arg(&out).output().unwrap();
            assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
            let text = std::fs::read_to_string(&out).unwrap();
            let _ = std::fs::remove_file(&out);
            assert_eq!(text, fixture_text(&format!("{name}.robot.{ext}")), "{src} as {ext}");
        }
    }
}

/// Axioms and their operands are written in the order OWL's object model
/// compares them, as ROBOT 1.9.11 writes them: an assertion by its subject
/// first, a sub-property axiom by its sub-property, a property characteristic,
/// domain or range of an inverse by the property, an n-ary axiom by its operand
/// set; IRIs by namespace and then local name, so `…/p_b` precedes `…/p/c`; a
/// data union before every other data range, and a one-of's, union's and
/// restriction's operands sorted; a class's restrictions and an individual's
/// property values in a frame in that same order, with its `owl:sameAs`
/// statements ahead of its `owl:differentFrom` ones; and in OBO, a pair's
/// clause on the first member naming the other. In functional syntax, RDF/XML
/// and Turtle for all four documents, and in OBO for the one ROBOT can write as
/// OBO.
#[test]
fn axioms_and_operands_are_ordered_as_robot_orders_them() {
    for (name, exts) in [
        ("axiom-order-general", &["ofn", "owl", "ttl"][..]),
        ("axiom-order-operands", &["ofn", "owl", "ttl", "obo"]),
        ("axiom-order-frames", &["ofn", "owl", "ttl"]),
        ("frame-identity-edges", &["ofn", "owl", "ttl"]),
    ] {
        for ext in exts {
            let out = tmp(&format!("{name}.{ext}"));
            let run = bin()
                .args(["convert", "-i"])
                .arg(robot_fixture(&format!("{name}.ofn")))
                .arg("-o")
                .arg(&out)
                .output()
                .unwrap();
            assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
            let text = std::fs::read_to_string(&out).unwrap();
            let _ = std::fs::remove_file(&out);
            assert_eq!(text, fixture_text(&format!("{name}.robot.{ext}")), "{name}.{ext}");
        }
    }
}

/// Two expressions that differ only in a literal's datatype are two
/// expressions: a property's domains `DataHasValue(d "1")` and
/// `DataHasValue(d "1"^^xsd:integer)` are both written. As ROBOT 1.9.11 writes
/// them, in functional syntax, RDF/XML and Turtle.
#[test]
fn expressions_that_differ_only_in_a_datatype_are_both_written() {
    for ext in ["ofn", "owl", "ttl"] {
        let out = tmp(&format!("frame-distinct-values.{ext}"));
        let run = bin()
            .args(["convert", "-i"])
            .arg(robot_fixture("frame-distinct-values.ofn"))
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(text, fixture_text(&format!("frame-distinct-values.robot.{ext}")), "{ext}");
    }
}

/// `--axioms internal` and `--axioms external` judge each axiom by its
/// subjects — the declared entity, the sub-class or sub-property, the named
/// individual asserted about, every member of a disjointness — and select from
/// every axiom, whatever the terms select; `--base-iri` names the namespaces and
/// nothing else. An axiom the namespace selector takes is kept whole, beside the
/// copy without annotations a type selector keeps of it. As ROBOT 1.9.11 does,
/// for `remove` and for `filter`.
#[test]
fn internal_and_external_axioms_are_judged_by_their_subjects() {
    const BASE: &str = "http://example.org/int/";
    for (expected, args) in [
        ("remove-external", &["remove", "--axioms", "external"][..]),
        ("remove-internal", &["remove", "--term", "http://example.org/ext/B", "--axioms", "internal"]),
        ("filter-internal", &["filter", "--term", "http://example.org/int/A", "--axioms", "internal"]),
        (
            "filter-internal-subclass",
            &[
                "filter",
                "--term",
                "http://example.org/int/C",
                "--term",
                "http://example.org/ext/B",
                "--axioms",
                "internal subclass",
            ],
        ),
        ("filter-external", &["filter", "--axioms", "external"]),
    ] {
        let out = tmp(&format!("axioms-namespace-{expected}.ofn"));
        let run = bin()
            .args(&args[..1])
            .arg("-i")
            .arg(robot_fixture("axioms-namespace.ofn"))
            .args(["--base-iri", BASE])
            .args(&args[1..])
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let text = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(text, fixture_text(&format!("axioms-namespace.{expected}.ofn")), "{expected}");
    }
}

/// The annotations of an assertion on an inverse property reify the
/// statement of the named property it is, with no warning, annotations of
/// annotations and anonymous values included, and the document reads back
/// whole.
#[test]
fn the_annotations_of_an_assertion_on_an_inverse_are_stated() {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};
    use oxigraph::store::Store;
    let src = tmp("inverse-assertion.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://example.org/w#>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/w>\nDeclaration(Class(:A))\nDeclaration(ObjectProperty(:p))\n\
         Declaration(ObjectProperty(:q))\nDeclaration(NamedIndividual(:i))\nDeclaration(NamedIndividual(:j))\n\
         ObjectPropertyAssertion(Annotation(Annotation(rdfs:comment \"nn\") rdfs:comment \"ann\") ObjectInverseOf(:p) :i :j)\n\
         ObjectPropertyAssertion(Annotation(rdfs:seeAlso _:x) ObjectInverseOf(:q) :i :j)\nClassAssertion(:A _:x)\n)\n",
    )
    .unwrap();
    for (ext, format) in [("owl", RdfFormat::RdfXml), ("ttl", RdfFormat::Turtle)] {
        let out = tmp(&format!("inverse-assertion.{ext}"));
        let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!stderr.contains("layout cannot state"), "{ext}: {stderr}");
        let text = std::fs::read(&out).unwrap();
        let store = Store::new().unwrap();
        store.load_from_slice(RdfParser::from_format(format), &text).unwrap();
        let query = "PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                     PREFIX : <http://example.org/w#> \
                     ASK { :j :p :i ; :q :i . ?a owl:annotatedSource :j ; owl:annotatedProperty :p ; owl:annotatedTarget :i ; \
                     rdfs:comment \"ann\" . ?n owl:annotatedSource ?a ; owl:annotatedProperty rdfs:comment ; \
                     owl:annotatedTarget \"ann\" ; rdfs:comment \"nn\" . ?b owl:annotatedSource :j ; owl:annotatedProperty :q ; \
                     owl:annotatedTarget :i ; rdfs:seeAlso ?x . ?x a :A }";
        let answer = SparqlEvaluator::new().parse_query(query).unwrap().on_store(&store).execute().unwrap();
        assert!(matches!(answer, QueryResults::Boolean(true)), "{ext}\n{}", String::from_utf8_lossy(&text));
        // Read back, the document holds the assertions, stated of the named
        // properties, and the class assertion. RDF/XML declares the document's
        // namespace under a prefix of its own, `w`, which the read-back takes.
        let back = tmp(&format!("inverse-assertion-{ext}.ofn"));
        let run = bin().args(["convert", "-i"]).arg(&out).arg("-o").arg(&back).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let read = std::fs::read_to_string(&back).unwrap();
        let w = if ext == "owl" { "w:" } else { ":" };
        for axiom in [
            format!("ObjectPropertyAssertion(Annotation(Annotation(rdfs:comment \"nn\") rdfs:comment \"ann\") {w}p {w}j {w}i)"),
            "ObjectPropertyAssertion(Annotation(rdfs:seeAlso _:genid".to_string(),
            format!("ClassAssertion({w}A _:genid"),
        ] {
            assert!(read.contains(axiom.as_str()), "{ext}: {axiom}\n{read}");
        }
        let _ = std::fs::remove_file(&back);
        let _ = std::fs::remove_file(&out);
    }
    let _ = std::fs::remove_file(&src);
}

/// An annotated chain whose super-property is an inverse is stated of the
/// inverse's node, the nested source of its reification, with no warning, and
/// reads back whole.
#[test]
fn an_annotated_chain_under_an_inverse_is_stated() {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};
    use oxigraph::store::Store;
    let src = tmp("inverse-chain.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://example.org/w#>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/w>\nDeclaration(ObjectProperty(:p))\nDeclaration(ObjectProperty(:q))\n\
         Declaration(ObjectProperty(:r))\n\
         SubObjectPropertyOf(Annotation(rdfs:comment \"chain\") ObjectPropertyChain(:p ObjectInverseOf(:q)) ObjectInverseOf(:r))\n)\n",
    )
    .unwrap();
    for (ext, format) in [("owl", RdfFormat::RdfXml), ("ttl", RdfFormat::Turtle)] {
        let out = tmp(&format!("inverse-chain.{ext}"));
        let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!stderr.contains("layout cannot state"), "{ext}: {stderr}");
        let text = std::fs::read(&out).unwrap();
        let store = Store::new().unwrap();
        store.load_from_slice(RdfParser::from_format(format), &text).unwrap();
        let query = "PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX : <http://example.org/w#> \
                     ASK { ?x owl:inverseOf :r ; owl:propertyChainAxiom ?l . ?l rdf:first :p ; rdf:rest/rdf:first/owl:inverseOf :q . \
                     ?a owl:annotatedSource ?x ; owl:annotatedProperty owl:propertyChainAxiom ; owl:annotatedTarget ?t ; \
                     rdfs:comment \"chain\" . ?t rdf:first :p ; rdf:rest/rdf:first/owl:inverseOf :q }";
        let answer = SparqlEvaluator::new().parse_query(query).unwrap().on_store(&store).execute().unwrap();
        assert!(matches!(answer, QueryResults::Boolean(true)), "{ext}\n{}", String::from_utf8_lossy(&text));
        let back = tmp(&format!("inverse-chain-{ext}.ofn"));
        let run = bin().args(["convert", "-i"]).arg(&out).arg("-o").arg(&back).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let read = std::fs::read_to_string(&back).unwrap();
        let axiom = "SubObjectPropertyOf(Annotation(rdfs:comment \"chain\") \
                     ObjectPropertyChain(:p ObjectInverseOf(:q)) ObjectInverseOf(:r))";
        assert!(read.contains(axiom), "{ext}: {axiom}\n{read}");
        let _ = std::fs::remove_file(&back);
        let _ = std::fs::remove_file(&out);
    }
    let _ = std::fs::remove_file(&src);
}

/// What ROBOT 1.9.11 writes of `read-annotations` as RDF/XML, Turtle and
/// OWL/XML reads back as ROBOT reads each of them: annotations of annotations
/// three deep, on an assertion and on the ontology's own annotation, and the
/// annotated axioms stated on a node of their own — negative assertions, n-ary
/// disjointness and difference, disjoint properties with an inverse member —
/// with theirs.
#[test]
fn annotations_are_read_as_robot_reads_them() {
    for ext in ["owl", "ttl", "owx"] {
        assert_eq!(
            convert_fixture(&format!("read-annotations.{ext}"), &format!("read-annotations-{ext}.ofn"), &[]),
            fixture_text("read-annotations.read.ofn"),
            "{ext}"
        );
    }
}

/// An anonymous individual is read wherever an axiom names one: a has-value's
/// value, the one its class assertion types; an enumeration's member; the other
/// member of a sameness and of a difference; the source and the target of a
/// negative assertion. As ROBOT 1.9.11 writes `read-anonymous` in RDF/XML and
/// Turtle, and as owlmake writes it.
#[test]
fn anonymous_individuals_are_read_wherever_an_axiom_names_one() {
    // The node id the first `_:genid…` after `prefix` names.
    fn id_after<'a>(read: &'a str, prefix: &str) -> Option<&'a str> {
        let at = read.find(prefix)? + prefix.len();
        let rest = &read[at..];
        Some(&rest[..rest.find([')', ' '])?])
    }
    let check = |what: &str, read: &str| {
        let value = id_after(read, "SubClassOf(:A ObjectHasValue(:p _:")
            .unwrap_or_else(|| panic!("{what}: the has-value\n{read}"));
        assert!(read.contains(&format!("ClassAssertion(:C _:{value})")), "{what}: the typed value\n{read}");
        let mut ids = vec![value];
        for (prefix, suffix) in [
            ("SubClassOf(:B ObjectOneOf(:i _:", "))"),
            ("SameIndividual(:i _:", ")"),
            ("DifferentIndividuals(:j _:", ")"),
            ("NegativeObjectPropertyAssertion(:p :i _:", ")"),
            ("NegativeObjectPropertyAssertion(:p _:", " :j)"),
        ] {
            let id = id_after(read, prefix).unwrap_or_else(|| panic!("{what}: {prefix}…\n{read}"));
            assert!(read.contains(&format!("{prefix}{id}{suffix}")), "{what}: {prefix}{id}{suffix}\n{read}");
            ids.push(id);
        }
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 6, "{what}: six individuals\n{read}");
    };
    for ext in ["owl", "ttl"] {
        let read = convert_fixture(&format!("read-anonymous.{ext}"), &format!("read-anonymous-{ext}.ofn"), &[]);
        check(&format!("ROBOT's {ext}"), &read);
        let written = tmp(&format!("read-anonymous-written.{ext}"));
        let run = bin().args(["convert", "-i"]).arg(robot_fixture("read-anonymous.ofn")).arg("-o").arg(&written).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let back = tmp(&format!("read-anonymous-written-{ext}.ofn"));
        let run = bin().args(["convert", "-i"]).arg(&written).arg("-o").arg(&back).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        check(&format!("owlmake's {ext}"), &std::fs::read_to_string(&back).unwrap());
        let _ = std::fs::remove_file(&written);
        let _ = std::fs::remove_file(&back);
    }
}

/// Each entry of an annotated list in Manchester syntax — `Facts:`,
/// `Characteristics:`, a data property's `Range:` — carries annotations of its
/// own after a comma, as ROBOT 1.9.11 writes `manchester-annotated-lists`, and
/// the document reads back as ROBOT reads it.
#[test]
fn annotated_list_entries_are_read_from_manchester_syntax() {
    assert_eq!(
        convert_fixture("manchester-annotated-lists.ofn", "annotated-lists.omn", &[]),
        fixture_text("manchester-annotated-lists.omn")
    );
    assert_eq!(
        convert_fixture("manchester-annotated-lists.omn", "annotated-lists.ofn", &[]),
        fixture_text("manchester-annotated-lists.omn.ofn")
    );
}

/// A namespace the document binds only to the empty prefix is declared under a
/// prefix of its own too, when a property in it is written as an element — an
/// object, a data or an annotation property — as ROBOT 1.9.11 writes
/// `rdf-generated-prefix`.
#[test]
fn a_property_namespace_bound_to_the_empty_prefix_gets_a_prefix_of_its_own() {
    assert_eq!(
        convert_fixture("rdf-generated-prefix.ofn", "generated-prefix.owl", &[]),
        fixture_text("rdf-generated-prefix.owl")
    );
}

/// An equivalence, inverse or disjointness between a named property and an
/// inverse whose property comes first in IRI order is stated of the named
/// property, with no warning, annotations and all, and reads back whole.
#[test]
fn an_axiom_between_a_property_and_an_earlier_inverse_is_stated() {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};
    use oxigraph::store::Store;
    let src = tmp("inverse-first.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://example.org/w#>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/w>\nDeclaration(ObjectProperty(:p))\nDeclaration(ObjectProperty(:q))\n\
         Declaration(ObjectProperty(:r))\nEquivalentObjectProperties(:q ObjectInverseOf(:p))\n\
         InverseObjectProperties(Annotation(rdfs:comment \"inverse\") :q ObjectInverseOf(:p))\n\
         DisjointObjectProperties(:r ObjectInverseOf(:p))\n)\n",
    )
    .unwrap();
    for (ext, format) in [("owl", RdfFormat::RdfXml), ("ttl", RdfFormat::Turtle)] {
        let out = tmp(&format!("inverse-first.{ext}"));
        let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!stderr.contains("layout cannot state"), "{ext}: {stderr}");
        let text = std::fs::read(&out).unwrap();
        let store = Store::new().unwrap();
        store.load_from_slice(RdfParser::from_format(format), &text).unwrap();
        let holds = |pattern: &str| {
            let query = format!(
                "PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                 PREFIX : <http://example.org/w#> ASK {{ {pattern} }}"
            );
            let answer = SparqlEvaluator::new().parse_query(&query).unwrap().on_store(&store).execute().unwrap();
            matches!(answer, QueryResults::Boolean(true))
        };
        for pattern in [
            ":q owl:equivalentProperty ?x . ?x owl:inverseOf :p",
            ":r owl:propertyDisjointWith ?x . ?x owl:inverseOf :p",
            ":q owl:inverseOf ?x . ?x owl:inverseOf :p . ?a owl:annotatedSource :q ; owl:annotatedProperty owl:inverseOf ; \
             owl:annotatedTarget ?x ; rdfs:comment \"inverse\"",
        ] {
            assert!(holds(pattern), "{ext}: {pattern}\n{}", String::from_utf8_lossy(&text));
        }
        // Read back, the document holds the three axioms.
        let back = tmp(&format!("inverse-first-{ext}.ofn"));
        let run = bin().args(["convert", "-i"]).arg(&out).arg("-o").arg(&back).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let read = std::fs::read_to_string(&back).unwrap();
        for axiom in [
            "EquivalentObjectProperties(:q ObjectInverseOf(:p))",
            "InverseObjectProperties(Annotation(rdfs:comment \"inverse\") :q ObjectInverseOf(:p))",
            "DisjointObjectProperties(:r ObjectInverseOf(:p))",
        ] {
            assert!(read.contains(axiom), "{ext}: {axiom}\n{read}");
        }
        let _ = std::fs::remove_file(&back);
        let _ = std::fs::remove_file(&out);
    }
    let _ = std::fs::remove_file(&src);
}

/// A document holding an axiom the RDF layout cannot state is written in full
/// through the plain RDF mapping, with a warning naming the axiom. The layout
/// has no place for an annotation with an annotation of its own on an ontology
/// with no IRI; in RDF/XML and in Turtle the ontology is a blank node carrying
/// the annotation, a reification of it carries the inner one, and the rest of
/// the document is there beside them.
#[test]
fn a_document_the_layout_cannot_state_is_written_whole_with_a_warning() {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};
    use oxigraph::store::Store;
    let src = tmp("unstated.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://example.org/a#>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(\n\
         Annotation(Annotation(rdfs:comment \"inner\") rdfs:comment \"c\")\n\
         SubClassOf(:A :B)\n)\n",
    )
    .unwrap();
    for (ext, format) in [("owl", RdfFormat::RdfXml), ("ttl", RdfFormat::Turtle)] {
        let out = tmp(&format!("unstated.{ext}"));
        let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            stderr.contains("layout cannot state 1 axiom(s)")
                && stderr.contains("Annotation(Annotation(rdfs:comment \"inner\") rdfs:comment \"c\")"),
            "{ext}: {stderr}"
        );
        let text = std::fs::read(&out).unwrap();
        let store = Store::new().unwrap();
        store.load_from_slice(RdfParser::from_format(format), &text).unwrap();
        let whole = SparqlEvaluator::new()
            .parse_query(
                "PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                 PREFIX : <http://example.org/a#> \
                 ASK { ?o a owl:Ontology ; rdfs:comment \"c\" . FILTER(isBlank(?o)) \
                 ?r a owl:Annotation ; owl:annotatedSource ?o ; owl:annotatedProperty rdfs:comment ; \
                 owl:annotatedTarget \"c\" ; rdfs:comment \"inner\" . :A rdfs:subClassOf :B }",
            )
            .unwrap()
            .on_store(&store)
            .execute()
            .unwrap();
        assert!(matches!(whole, QueryResults::Boolean(true)), "{ext}: {}", String::from_utf8_lossy(&text));
        let _ = std::fs::remove_file(&out);
    }
    let _ = std::fs::remove_file(&src);
}

/// An annotated assertion on an inverse property that names an anonymous
/// individual is written where ROBOT 1.9.11 writes the statement it makes of
/// the named property, the other way round, which keeps none of its
/// annotations; owlmake writes that statement's reification around the
/// layout, each anonymous individual it names named by id where ROBOT nests
/// it. For an anonymous subject, an anonymous object, which ROBOT writes
/// nowhere and owlmake writes as a block of its own, and both, in RDF/XML and
/// in Turtle; the document reads back whole.
#[test]
fn annotated_inverse_assertions_keep_their_annotations_around_the_layout() {
    let k = "<rdf:type rdf:resource=\"http://example.org/inverse-annotated.owl#K\"/>";
    let nested_a = format!(
        "        <p>\n            <rdf:Description>\n                {k}\n            </rdf:Description>\n        </p>\n"
    );
    let root_z = format!("    <rdf:Description>\n{nested_a}    </rdf:Description>\n");
    assert_written_around_the_layout(
        "inverse-annotated",
        "inverse-annotated:",
        "PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
         PREFIX : <http://example.org/inverse-annotated.owl#> \
         ASK { :b :p ?a . ?a a :K . \
         ?r1 owl:annotatedSource :b ; owl:annotatedProperty :p ; owl:annotatedTarget ?a ; rdfs:comment \"one\" . \
         ?x :p :s ; a :K . \
         ?r2 owl:annotatedSource ?x ; owl:annotatedProperty :p ; owl:annotatedTarget :s ; rdfs:comment \"two\" . \
         ?z :p ?y . ?y a :K . \
         ?r3 owl:annotatedSource ?z ; owl:annotatedProperty :p ; owl:annotatedTarget ?y ; rdfs:comment \"three\" }",
        &[
            ("owl", nested_a.as_str(), "        <p rdf:nodeID=\"genid1\"/>\n"),
            (
                "owl",
                root_z.as_str(),
                "    <rdf:Description rdf:nodeID=\"genid4\">\n        <p rdf:nodeID=\"genid3\"/>\n    </rdf:Description>\n",
            ),
            ("ttl", "   :p [ rdf:type :K\n      ] .\n", "   :p _:genid1 .\n"),
            ("ttl", "[ :p [ rdf:type :K\n     ]\n] .\n", "_:genid4 :p _:genid3 .\n"),
        ],
    );
}

/// A document is read in the syntax its content is in, whatever the file is
/// called, and comment lines before its first statement do not hide it. ROBOT's
/// `asserted-equiv.owl` example is Manchester syntax, and is written as ROBOT
/// 1.9.11 writes it.
#[test]
fn a_manchester_document_named_owl_is_read_as_manchester() {
    assert_eq!(
        convert_fixture("manchester-document.owl", "manchester-document.rdf.owl", &[]),
        fixture_text("manchester-document.robot.owl")
    );
    assert_eq!(
        convert_fixture("manchester-document.owl", "manchester-document.ttl", &[]),
        fixture_text("manchester-document.robot.ttl")
    );
    let src = tmp("commented-manchester.owl");
    let out = tmp("commented-manchester.rdf.owl");
    std::fs::write(&src, format!("# A comment\n\n{}", fixture_text("manchester-document.owl"))).unwrap();
    let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("manchester-document.robot.owl"));
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

/// A literal is held as it is made, whatever syntax states it:
/// `"x@en"^^rdf:PlainLiteral` is `"x"@en`, a language tag is lower-cased, and
/// an `xsd:boolean`, `xsd:float`, `xsd:double` or `xsd:integer` takes the form
/// its value prints as. OWL/XML keeps `rdf:PlainLiteral` text whole, and
/// Manchester syntax reads a bare number by its form. As ROBOT 1.9.11 reads
/// `literal-forms` in functional syntax, RDF/XML, OWL/XML, Manchester syntax,
/// OBO and Turtle.
#[test]
fn literals_are_held_as_robot_makes_them_from_every_syntax() {
    for ext in ["ofn", "rdf", "owx", "omn", "obo", "ttl"] {
        assert_eq!(
            convert_fixture(&format!("literal-forms.{ext}"), &format!("literal-forms-{ext}.ofn"), &[]),
            fixture_text(&format!("literal-forms.{ext}.robot.ofn")),
            "{ext}"
        );
    }
}

/// `annotate --language-annotation`/`--typed-annotation` and a template's `AT`
/// and `AL` columns make their literals as a document's are made. As ROBOT
/// 1.9.11 runs them over `literal-annotate` and `literal-template`.
#[test]
fn annotate_and_template_make_literals_as_robot_makes_them() {
    let out = tmp("literal-annotate.ofn");
    let run = bin()
        .args(["annotate", "-i"])
        .arg(robot_fixture("literal-annotate.ofn"))
        .args(["--language-annotation", "rdfs:comment", "X", "EN-GB"])
        .args(["--typed-annotation", "rdfs:comment", "1", "xsd:boolean"])
        .args(["--typed-annotation", "rdfs:comment", "2e3", "xsd:double"])
        .args(["--typed-annotation", "rdfs:comment", "+5", "xsd:integer"])
        .args(["--typed-annotation", "rdfs:comment", "y@FR", "rdf:PlainLiteral"])
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("literal-annotate.robot.ofn"));
    let _ = std::fs::remove_file(&out);

    let out = tmp("literal-template.ofn");
    let run = bin()
        .args(["template", "-i"])
        .arg(robot_fixture("literal-template.ofn"))
        .args(["--prefix", "ex: http://example.org/t#", "-t"])
        .arg(robot_fixture("literal-template.tsv"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("literal-template.robot.ofn"));
    let _ = std::fs::remove_file(&out);
}

/// `annotate --interpolate` replaces `%{ontology_iri}` and `%{version_iri}` in
/// each value with the IRIs the ontology has as the command reads it, before its
/// own `--ontology-iri` and `--version-iri`, and leaves any other `%{…}`, and a
/// placeholder whose IRI the ontology lacks, as written. As ROBOT 1.9.11 runs it
/// over `interpolate` and `interpolate-unversioned`.
#[test]
fn annotate_interpolates_the_ontology_iris_as_robot_does() {
    let out = tmp("interpolate.ofn");
    let run = bin()
        .args(["annotate", "-i"])
        .arg(robot_fixture("interpolate.ofn"))
        .args(["--interpolate", "true"])
        .args(["--annotation", "rdfs:comment", "v %{ontology_iri} %{version_iri} %{rdfs:label}"])
        .args(["--link-annotation", "rdfs:seeAlso", "%{version_iri}"])
        .args(["--language-annotation", "rdfs:comment", "%{ontology_iri}", "en"])
        .args(["--typed-annotation", "rdfs:comment", "%{version_iri}", "xsd:anyURI"])
        .args(["--ontology-iri", "http://example.org/new.owl"])
        .args(["--version-iri", "http://example.org/new/v2.owl"])
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("interpolate.robot.ofn"));

    let run = bin()
        .args(["annotate", "-i"])
        .arg(robot_fixture("interpolate-unversioned.ofn"))
        .args(["--interpolate", "true"])
        .args(["--annotation", "rdfs:comment", "v %{ontology_iri} %{version_iri}"])
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("interpolate-unversioned.robot.ofn"));
    let _ = std::fs::remove_file(&out);
}

/// `reason --annotate-inferred-axioms true` asserts each inferred `SubClassOf`
/// annotated `is_inferred "true"`, in place of the same axiom asserted without
/// annotations, and refuses to annotate an inferred axiom of any other type.
/// `materialize` annotates nothing and removes nothing, and with
/// `--create-new-ontology true` writes its input as it was. As ROBOT 1.9.11 runs
/// both over `reason-annotate.ofn`, `reason-annotate-equivalent.ofn`,
/// `materialize-options.ofn` and `materialize-redundant.ofn`.
#[test]
fn reason_and_materialize_take_the_reason_options_as_robot_does() {
    let p = ["--term", "http://example.org/p"];
    let cases: &[(&str, &str, &[&str], &str)] = &[
        ("reason", "reason-annotate.ofn", &["--annotate-inferred-axioms", "true"], "reason-annotate.robot.ofn"),
        ("materialize", "materialize-options.ofn", &["--annotate-inferred-axioms", "true"], "materialize-options.robot.ofn"),
        ("materialize", "materialize-options.ofn", &["--create-new-ontology", "true"], "materialize-options.new.robot.ofn"),
        (
            "materialize",
            "materialize-redundant.ofn",
            &["--remove-redundant-subclass-axioms", "true"],
            "materialize-redundant.robot.ofn",
        ),
    ];
    let out = tmp("reason-options.ofn");
    for (command, src, args, robot) in cases {
        let mut run = bin();
        run.arg(command).arg("--input").arg(robot_fixture(src)).args(["--reasoner", "ELK"]).args(*args);
        if *command == "materialize" {
            run.args(p);
        }
        let run = run.arg("--output").arg(&out).output().unwrap();
        assert!(run.status.success(), "{robot}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(robot), "{robot}");
    }
    let _ = std::fs::remove_file(&out);
    let run = bin()
        .args(["reason", "--input"])
        .arg(robot_fixture("reason-annotate-equivalent.ofn"))
        .args(["--reasoner", "ELK", "--axiom-generators", "SubClass EquivalentClass"])
        .args(["--annotate-inferred-axioms", "true", "--output"])
        .arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success() && stderr.contains("AXIOM TYPE ERROR"), "{stderr}");
    assert!(!out.exists());
}

/// `materialize` asserts the direct superclasses its reasoner infers, the
/// restrictions over the ontology's object properties among them. The EL
/// reasoners infer nothing from a universal restriction or a functional
/// property, whelk does from a union, the DL reasoners from all three, and the
/// structural reasoner only what an equivalence or a superclass names. Every
/// class of a direct superclass's node is asserted, except under whelk, which
/// asserts the one its walk over the subsumers reaches first — a restriction
/// equivalent to a named class among them. A class gains superclasses only
/// where the ontology's own axioms define it. As ROBOT 1.9.11 materializes
/// `materialize-reasoners.ofn` with each reasoner.
#[test]
fn materialize_asserts_what_its_reasoner_infers() {
    let out = tmp("materialize-reasoners.ofn");
    for reasoner in ["elk", "hermit", "jfact", "whelk", "structural"] {
        let run = bin()
            .args(["materialize", "--input"])
            .arg(robot_fixture("materialize-reasoners.ofn"))
            .args(["--reasoner", reasoner, "--output"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{reasoner}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            fixture_text(&format!("materialize-reasoners.{reasoner}.robot.ofn")),
            "{reasoner}"
        );
    }
    let _ = std::fs::remove_file(&out);
}

/// The structural reasoner's parents of a class are the named classes and the
/// named conjuncts of an intersection that a `SubClassOf` it is the subclass
/// of, or an `EquivalentClasses` it is a member of, names. As ROBOT 1.9.11
/// reasons over `materialize-reasoners.ofn` with it.
#[test]
fn the_structural_reasoner_takes_named_conjuncts_as_parents() {
    let out = tmp("materialize-reasoners.reason-structural.ofn");
    let run = bin()
        .args(["reason", "--input"])
        .arg(robot_fixture("materialize-reasoners.ofn"))
        .args(["--reasoner", "structural", "--output"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        fixture_text("materialize-reasoners.reason-structural.robot.ofn")
    );
    let _ = std::fs::remove_file(&out);
}

/// Before anything is inferred, `reason` and `materialize` check the ontology:
/// an inconsistent ontology fails, then one with an unsatisfiable class, then
/// one with an unsatisfiable object property, each logged as errors first. A
/// property is unsatisfiable where its probe `P ⊑ ∃p.⊤` is (`reason` with `elk`
/// or `whelk`), where it is a told sub-property of `owl:bottomObjectProperty`
/// (`materialize` and `emr`, and `jfact`), or where the classified property
/// hierarchy puts it at the bottom (`hermit`); `structural` finds nothing.
/// `materialize` checks with `--create-new-ontology true` too, and has no `emr`.
/// As ROBOT 1.9.11 runs both, with every reasoner, over the `validate-*.ofn`
/// fixtures: `validate.robot.txt` holds each run's exit code and logged lines.
/// Where ROBOT answered differently on the same input, a `varies:` line says
/// how often each answer came, and the expected one is the most complete:
/// its ELK property hierarchy at times leaves out a property that reaches
/// `owl:bottomObjectProperty` through another one.
#[test]
fn reason_and_materialize_check_the_ontology_as_robot_does() {
    // A logged reasoner error's message, marked `\n` where the message itself
    // ends in a newline (an empty line follows it).
    fn logged(stdout: &str) -> Vec<String> {
        const MARK: &str = " ERROR org.obolibrary.robot.ReasonerHelper - ";
        let lines: Vec<&str> = stdout.split('\n').collect();
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(at) = line.find(MARK) {
                let mut msg = line[at + MARK.len()..].to_string();
                if lines.get(i + 1) == Some(&"") && i + 2 < lines.len() {
                    msg.push_str("\\n");
                }
                out.push(msg);
            }
        }
        out
    }
    let expected = fixture_text("validate.robot.txt");
    let out = tmp("validate-out.ofn");
    let dump = tmp("validate-dump.ofn");
    let mut cases: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in expected.lines() {
        match line.strip_prefix("# ") {
            Some(header) => cases.push((header, Vec::new())),
            None => cases.last_mut().expect("a case header first").1.push(line),
        }
    }
    assert_eq!(cases.len(), 105);
    let mut differ = Vec::new();
    for (header, lines) in &cases {
        let (src, command) = header.split_once(" | ").unwrap();
        let exit: i32 = lines[0].strip_prefix("exit ").unwrap().parse().unwrap();
        let errors: Vec<&str> = lines[1..].iter().filter_map(|l| l.strip_prefix("error: ")).collect();
        let want: Vec<&str> = lines[1..]
            .iter()
            .filter(|l| !l.starts_with("error: ") && !l.starts_with("varies: "))
            .copied()
            .collect();
        let args: Vec<std::ffi::OsString> = command
            .split_whitespace()
            .map(|a| if a == "DUMP" { dump.clone().into_os_string() } else { a.into() })
            .collect();
        let run = bin().args(&args).arg("--input").arg(robot_fixture(src)).arg("--output").arg(&out).output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        let got = logged(&stdout);
        if run.status.code() != Some(exit)
            || got != want
            || errors.iter().any(|e| !stderr.contains(e))
        {
            differ.push(format!("{header}: exit {:?} (ROBOT {exit})\n  om:    {got:?}\n  ROBOT: {want:?}\n  {stderr}", run.status.code()));
        }
    }
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&dump);
    assert!(differ.is_empty(), "{} of {} differ:\n{}", differ.len(), cases.len(), differ.join("\n"));
}

/// Every reasoner counts each member of a disjointness or a difference once:
/// `DifferentIndividuals(:j :j)` says nothing, `DisjointClasses(:A :A :B)`
/// leaves an instance of `:A` consistent, and `DisjointUnion(:U :C :C)` makes
/// `:U` and `:C` equivalent without emptying `:C` (whelk reads no disjoint
/// union, and the structural reasoner no equivalence). As ROBOT 1.9.11
/// reasons over `repeated-operands` and `repeated-union`.
#[test]
fn every_reasoner_counts_a_repeated_member_once() {
    for r in ["elk", "hermit", "jfact", "whelk", "structural"] {
        let union = match r {
            "whelk" | "structural" => "repeated-union.ignored.robot.ofn",
            _ => "repeated-union.equivalent.robot.ofn",
        };
        for (src, expected) in [("repeated-operands", "repeated-operands.robot.ofn"), ("repeated-union", union)] {
            let out = tmp(&format!("{src}.{r}.ofn"));
            let run =
                bin().args(["reason", "-r", r, "-i"]).arg(robot_fixture(&format!("{src}.ofn"))).arg("-o").arg(&out).output().unwrap();
            assert!(run.status.success(), "{src} {r}: {}", String::from_utf8_lossy(&run.stderr));
            assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(expected), "{src} {r}");
            let _ = std::fs::remove_file(&out);
        }
    }
}

/// A class and an individual that share an IRI are two things to every
/// reasoner: the individual `:P` is a `:B`, which makes `:D ≡ {:P}` a `:B` and
/// leaves the class `:P` under `:E` alone; and the class `:P` being empty
/// leaves the ontology consistent, with `:P` its one unsatisfiable class. As
/// ROBOT 1.9.11 reasons over `el-punning` and `el-punning-unsat`.
#[test]
fn a_class_and_an_individual_sharing_an_iri_are_reasoned_apart() {
    for r in ["elk", "hermit", "jfact", "whelk"] {
        let out = tmp(&format!("el-punning.{r}.ofn"));
        let run = bin().args(["reason", "-r", r, "-i"]).arg(robot_fixture("el-punning.ofn")).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{r}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("el-punning.robot.ofn"), "{r}");
        let _ = std::fs::remove_file(&out);
    }
    for r in ["elk", "hermit", "whelk"] {
        let out = tmp(&format!("el-punning-unsat.{r}.ofn"));
        let run =
            bin().args(["reason", "-r", r, "-i"]).arg(robot_fixture("el-punning-unsat.ofn")).arg("-o").arg(&out).output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let logged: Vec<&str> = stdout
            .lines()
            .filter_map(|l| l.split_once(" ERROR org.obolibrary.robot.ReasonerHelper - ").map(|(_, m)| m))
            .collect();
        assert_eq!(run.status.code(), Some(1), "{r}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            logged,
            [
                "There are 1 unsatisfiable classes in the ontology.",
                "    unsatisfiable: http://example.org/el-punning-unsat#P",
            ],
            "{r}"
        );
    }
}

/// whelk reads `p value i` as `p some {i}`, a one-of as the union of its named
/// members, each a one-of alone, and `p max 0 C` as `not (p some C)`; it reads
/// no disjoint union. So `:A` falls under `:B`, `:D` and `:H` under `:C`, and
/// `:X` stays out of `:U`; and `:K` and `:C`, each with a `p` its maximum
/// forbids, are empty, where `:L`'s `p` has another filler. As ROBOT 1.9.11
/// reasons with whelk over `whelk-expressions` and `whelk-max-zero`.
#[test]
fn whelk_reads_class_expressions_as_it_does() {
    let out = tmp("whelk-expressions.ofn");
    let run = bin()
        .args(["reason", "-r", "whelk", "-i"])
        .arg(robot_fixture("whelk-expressions.ofn"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("whelk-expressions.whelk.robot.ofn"));
    let _ = std::fs::remove_file(&out);

    let out = tmp("whelk-max-zero.ofn");
    let run = bin()
        .args(["reason", "-r", "whelk", "-i"])
        .arg(robot_fixture("whelk-max-zero.ofn"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let logged: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.split_once(" ERROR org.obolibrary.robot.ReasonerHelper - ").map(|(_, m)| m))
        .collect();
    assert_eq!(run.status.code(), Some(1), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        logged,
        [
            "There are 2 unsatisfiable classes in the ontology.",
            "    unsatisfiable: http://example.org/whelk-max-zero#K",
            "    unsatisfiable: http://example.org/whelk-max-zero#C",
        ]
    );
}

/// A disjointness of one class is that class's disjointness from
/// `owl:Thing`, so the class is empty, and so is every class below it. Every
/// reasoner reports both, as ROBOT 1.9.11 logs them for
/// `one-class-disjointness`.
#[test]
fn a_disjointness_of_one_class_empties_it() {
    let out = tmp("one-class-disjointness.ofn");
    for r in ["elk", "hermit", "jfact", "whelk"] {
        let run = bin()
            .args(["reason", "-r", r, "-i"])
            .arg(robot_fixture("one-class-disjointness.ofn"))
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let logged: Vec<&str> = stdout
            .lines()
            .filter_map(|l| l.split_once(" ERROR org.obolibrary.robot.ReasonerHelper - ").map(|(_, m)| m))
            .collect();
        assert_eq!(run.status.code(), Some(1), "{r}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            logged,
            [
                "There are 2 unsatisfiable classes in the ontology.",
                "    unsatisfiable: http://example.org/one-class-disjointness#F",
                "    unsatisfiable: http://example.org/one-class-disjointness#E",
            ],
            "{r}"
        );
    }
}

/// HermiT's reasoner factory reads past a datatype outside the OWL 2 datatype
/// map, so `reason` and `reduce` do under `hermit` and `jfact`. Wrapped for
/// expression materialization HermiT refuses it, with its own message, where
/// JFact still reads past it. As ROBOT 1.9.11 runs them over
/// `unsupported-datatype`, whose `xsd:date` is both a literal and a data range.
#[test]
fn only_materialize_under_hermit_refuses_a_datatype_outside_the_owl_2_map() {
    let src = robot_fixture("unsupported-datatype.ofn");
    let refusal = "HermiT supports all and only the datatypes of the OWL 2 datatype map, see \n\
                   http://www.w3.org/TR/owl2-syntax/#Datatype_Maps. \n\
                   The datatype 'http://www.w3.org/2001/XMLSchema#date' is not part of the OWL 2 datatype map and \n\
                   no custom datatype definition is given; \n\
                   therefore, HermiT cannot handle this datatype.";
    for (command, r, expected) in [
        ("reason", "hermit", Some("unsupported-datatype.reason.robot.ofn")),
        ("reason", "jfact", Some("unsupported-datatype.reason.robot.ofn")),
        ("reduce", "hermit", Some("unsupported-datatype.robot.ofn")),
        ("reduce", "jfact", Some("unsupported-datatype.robot.ofn")),
        ("materialize", "jfact", Some("unsupported-datatype.robot.ofn")),
        ("materialize", "hermit", None),
    ] {
        let out = tmp(&format!("unsupported-datatype.{command}.{r}.ofn"));
        let run = bin().args([command, "-r", r, "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        match expected {
            Some(expected) => {
                assert!(run.status.success(), "{command} {r}: {stderr}");
                assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(expected), "{command} {r}");
            }
            None => {
                assert_eq!(run.status.code(), Some(1), "{command} {r}: {stderr}");
                assert!(stderr.contains(refusal), "{command} {r}: {stderr}");
            }
        }
        let _ = std::fs::remove_file(&out);
    }
}

/// A command writes its ontology with the prefixes its own `--add-prefix` adds,
/// also where it builds the ontology afresh. As ROBOT 1.9.11 runs `filter` over
/// `filter-own-add-prefix.ofn`.
#[test]
fn filter_writes_the_prefixes_its_own_options_add() {
    let out = tmp("filter-own-add-prefix.ofn");
    let run = bin()
        .args(["filter", "--input"])
        .arg(robot_fixture("filter-own-add-prefix.ofn"))
        .args(["--term", "http://example.org/A", "--add-prefix", "zz: http://zz.org/", "--output"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("filter-own-add-prefix.robot.ofn"));
    let _ = std::fs::remove_file(&out);
}

/// A graph has a node for what the ontology declares, and none for a property it
/// only uses: an object, data or annotation property used without a declaration,
/// or a relation and a `property_value:` predicate an OBO document names without
/// a frame of their own. Read back from the RDF/XML it writes, which types every
/// property it uses, the same ontology has a node for each. As ROBOT 1.9.11
/// converts `obographs-undeclared.ofn`, directly and through RDF/XML, and
/// `obographs-relation.obo`.
#[test]
fn a_graph_has_a_node_for_a_property_only_where_the_ontology_declares_it() {
    assert_eq!(
        convert_fixture("obographs-undeclared.ofn", "obographs-undeclared.json", &[]),
        fixture_text("obographs-undeclared.robot.json")
    );
    assert_eq!(
        convert_fixture("obographs-relation.obo", "obographs-relation.json", &[]),
        fixture_text("obographs-relation.robot.json")
    );
    let owl = tmp("obographs-undeclared.owl");
    let json = tmp("obographs-undeclared-roundtrip.json");
    for (input, output) in [(robot_fixture("obographs-undeclared.ofn"), &owl), (owl.clone(), &json)] {
        let run = bin().args(["convert", "-i"]).arg(&input).arg("-o").arg(output).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    }
    assert_eq!(
        std::fs::read_to_string(&json).unwrap(),
        fixture_text("obographs-undeclared.roundtrip.robot.json")
    );
    let _ = std::fs::remove_file(&owl);
    let _ = std::fs::remove_file(&json);
}

/// `convert` of `src` to OBO Graphs JSON, as ROBOT 1.9.11 writes it under ODK
/// 1.6.1.
fn convert_to_json_as_robot(src: &std::path::Path, tag: &str) -> String {
    let path = tmp(&format!("{tag}.as-robot.json"));
    let run = bin()
        .args(["__emulate-robot-version=1.9.11", "__emulate-odk-version=1.6.1", "convert", "-i"])
        .arg(src)
        .arg("-o")
        .arg(&path)
        .output()
        .unwrap();
    assert!(run.status.success(), "{tag}: {}", String::from_utf8_lossy(&run.stderr));
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    text
}

/// A graph is built from the ontology's axioms in their sorted order. A node
/// takes its place where an axiom first names it — a declaration, the named
/// subclass of `SubClassOf`, either side of a class assertion, the subject of a
/// property assertion or of an annotation assertion — and keeps the last type
/// and label any axiom gives it; edges stand in the order of their axioms, so
/// property assertions follow class assertions and order by subject. An
/// anonymous individual is a node of its own (`_:genid…`). An axiom's
/// annotations are the `meta` of its edge or equivalence — `owl:deprecated
/// true`, xrefs, subsets and synonym types, property values — and the property
/// values among an annotation assertion's annotations the `meta` of the value it
/// writes. Axioms that differ only in their annotations are each written. A
/// control character and a character outside the Basic Multilingual Plane are
/// escaped in upper-case hex. As ROBOT 1.9.11 writes `obographs-axioms.ofn`.
#[test]
fn a_graph_is_built_from_the_axioms_in_their_sorted_order() {
    assert_eq!(
        convert_to_json_as_robot(&robot_fixture("obographs-axioms.ofn"), "obographs-axioms"),
        fixture_text("obographs-axioms.robot.json")
    );
}

/// A document that imports is written as its own graph and one graph for each
/// ontology of its imports closure, in the order of their ontology ids' text:
/// the root `uberon/core.owl` follows the `uberon/components/mappings.owl` it
/// imports, and `mrg.owl` follows `merged-one/components/part.owl` and
/// `merged-one/imports/merged_import.owl`. As ROBOT 1.9.11 converts
/// `xref-twins/twin.obo` and the ODK 1.6.1 repository's `mrg-edit.obo`.
#[test]
fn a_graph_is_written_for_each_ontology_of_the_imports_closure() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (src, expected) in [
        ("xref-twins/twin.obo", "obographs-closure-twin.robot.json"),
        ("odk-1.6.1-update/merged/after/src/ontology/mrg-edit.obo", "obographs-closure-mrg.robot.json"),
    ] {
        assert_eq!(convert_to_json_as_robot(&fixtures.join(src), expected), fixture_text(expected), "{src}");
    }
}

/// What no graph can hold is refused, as ROBOT 1.9.11 fails to convert it: an
/// axiom annotated with an anonymous individual, an ontology annotation whose
/// value is one, and a domain or range of an inverse property.
#[test]
fn json_refuses_what_robot_refuses() {
    for (src, says) in [
        ("rdf-nested-anonymous.ofn", "is annotated with an anonymous individual"),
        ("obo-header-annotations.ofn", "annotates the ontology with an anonymous individual"),
        ("rdf-inverse-axioms.ofn", "states the domain of an inverse property"),
    ] {
        let out = tmp(&format!("{src}.refused.json"));
        let run = bin().args(["convert", "-i"]).arg(robot_fixture(src)).arg("-o").arg(&out).output().unwrap();
        assert!(!run.status.success(), "{src} was written");
        let err = String::from_utf8_lossy(&run.stderr);
        assert!(err.contains(says), "{src}: {err}");
        let _ = std::fs::remove_file(&out);
    }
}

/// `annotate`'s options do what ROBOT 1.9.11 does with them over
/// `annotate-options.ofn` and its siblings: `--annotation-file` merges a file's
/// axioms and ontology annotations; `--annotate-derived-from` annotates every
/// axiom without a `prov:wasDerivedFrom` with the version IRI, or the ontology
/// IRI where there is none; `--annotate-defined-by` asserts `rdfs:isDefinedBy`
/// of every entity of the signature outside the reserved vocabularies that has
/// none; and `--axiom-annotation`, three values at a time read in pairs,
/// replaces the annotations of every `SubClassOf`.
#[test]
fn annotate_options_do_what_robot_does() {
    let extra = robot_fixture("annotate-options-extra.ofn");
    let extra = extra.to_str().unwrap();
    let cases: &[(&str, &[&str], &str)] = &[
        ("annotate-options.ofn", &["--annotation-file", extra], "annotate-options.file.robot.ofn"),
        ("annotate-options.ofn", &["--annotate-derived-from", "true"], "annotate-options.derived.robot.ofn"),
        (
            "annotate-options-unversioned.ofn",
            &["--annotate-derived-from", "true"],
            "annotate-options-unversioned.derived.robot.ofn",
        ),
        ("annotate-options.ofn", &["--annotate-defined-by", "true"], "annotate-options.defined.robot.ofn"),
        (
            "annotate-options.ofn",
            &[
                "--annotation", "dc:creator", "me", "--annotation-file", extra,
                "--annotate-derived-from", "true", "--annotate-defined-by", "true",
            ],
            "annotate-options.all.robot.ofn",
        ),
        (
            "annotate-options-subclasses.ofn",
            &["--axiom-annotation", "rdfs:comment", "x", "rdfs:comment", "--axiom-annotation", "y", "dc:source", "s"],
            "annotate-options-subclasses.axiom.robot.ofn",
        ),
    ];
    let out = tmp("annotate-options.ofn");
    for (src, args, robot) in cases {
        let run = bin().args(["annotate", "-i"]).arg(robot_fixture(src)).args(*args).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{robot}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(robot), "{robot}");
    }
    let _ = std::fs::remove_file(&out);
}

/// `annotate` refuses what ROBOT 1.9.11 refuses: an `--axiom-annotation` of two
/// values, options that ask for nothing, an axiom annotation over an ontology
/// with an axiom other than `SubClassOf`, and axiom annotation values that do
/// not pair up.
#[test]
fn annotate_refuses_what_robot_refuses() {
    let cases: &[(&str, &[&str], &str)] = &[
        ("annotate-options.ofn", &["--axiom-annotation", "rdfs:comment", "x"], "--axiom-annotation"),
        ("annotate-options.ofn", &["--interpolate", "true"], "MISSING ANNOTATION ERROR"),
        (
            "annotate-options.ofn",
            &["--axiom-annotation", "rdfs:comment", "x", "rdfs:comment", "--axiom-annotation", "y", "dc:source", "s"],
            "AXIOM TYPE ERROR",
        ),
        (
            "annotate-options-subclasses.ofn",
            &["--axiom-annotation", "rdfs:comment", "x", "dc:source"],
            "ANNOTATION FORMAT ERROR",
        ),
    ];
    let out = tmp("annotate-refused.ofn");
    for (src, args, message) in cases {
        let _ = std::fs::remove_file(&out);
        let run = bin().args(["annotate", "-i"]).arg(robot_fixture(src)).args(*args).arg("-o").arg(&out).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success() && stderr.contains(message), "{args:?}: {stderr}");
        assert!(!out.exists(), "{args:?} wrote an ontology");
    }
}

/// A template's `LABEL` and `A` cells make their text an `xsd:string` literal,
/// which sorts after a language-tagged literal of the same property whatever the
/// two say. As ROBOT 1.9.11 builds `text-literals.tsv`.
#[test]
fn a_template_makes_a_cells_text_an_xsd_string() {
    let out = tmp("text-literals.ofn");
    let run = bin()
        .args(["template", "--template"])
        .arg(robot_fixture("text-literals.tsv"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("text-literals.robot.ofn"));
    let _ = std::fs::remove_file(&out);
}

/// A CURIE a command is given is read with the command line's prefixes — the
/// built-in map and what `--prefix` and its kin bind — never with the ones the
/// document declares. As ROBOT 1.9.11 runs `annotate`, `filter`, `template` and
/// `export-prefixes` over `cli-curies.ofn`, whose `ex:` no command line binds.
#[test]
fn a_command_line_curie_is_read_with_the_command_lines_prefixes() {
    let input = robot_fixture("cli-curies.ofn");
    let out = tmp("cli-curies.ofn");
    let stderr = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).to_string();

    // `oboInOwl:`, `dc:` (dc/terms/) and `GO:` are the built-in map's.
    let run = bin()
        .args(["annotate", "-i"])
        .arg(&input)
        .args(["--annotation", "oboInOwl:date", "07:10:2026 03:41"])
        .args(["--annotation", "dc:title", "curies"])
        .args(["--link-annotation", "rdfs:seeAlso", "GO:0000001"])
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("cli-curies.annotate.robot.ofn"));

    // A prefix only the document binds, and one nothing binds, name nothing.
    for term in ["ex:foo", "foo:bar"] {
        let run = bin()
            .args(["annotate", "-i"])
            .arg(&input)
            .args(["--annotation", term, "x"])
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        let said = stderr(&run);
        let refusal = format!("INVALID IRI ERROR property \"{term}\" is not a valid CURIE or IRI");
        assert!(!run.status.success() && said.contains(&refusal), "{said}");
    }

    // A term that names nothing is no term at all, so `filter` keeps everything.
    let run = bin()
        .args(["filter", "-i"])
        .arg(&input)
        .args(["--term", "ex:A"])
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("cli-curies.filter.robot.ofn"));

    // A template cell that names nothing is refused.
    let table = tmp("cli-curies.tsv");
    std::fs::write(&table, "ID\tLabel\nID\tLABEL\nex:X\tx\n").unwrap();
    let run = bin()
        .args(["template", "-i"])
        .arg(&input)
        .arg("--template")
        .arg(&table)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    let said = stderr(&run);
    assert!(!run.status.success() && said.contains("UNKNOWN ENTITY ERROR could not interpret 'ex:X'"), "{said}");

    // `export-prefixes` writes the context itself.
    let json = tmp("cli-curies.json");
    let run = bin()
        .args(["export-prefixes", "--prefix", "ex2: http://example.org/ex2#", "-o"])
        .arg(&json)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(std::fs::read_to_string(&json).unwrap(), fixture_text("export-prefixes.robot.json"));
    for f in [&out, &table, &json] {
        let _ = std::fs::remove_file(f);
    }
}

/// `remove` with no term that names an IRI selects the whole ontology: a bare
/// `remove`, a term nothing binds, an empty term file and a term file of such
/// terms each take every axiom, and with `--axioms annotation` every annotation
/// axiom; a term that names an IRI the ontology never mentions removes nothing.
/// As ROBOT 1.9.11 removes over `remove-terms.ofn` and `remove-complement.ofn`.
#[test]
fn remove_with_no_term_naming_an_iri_selects_the_whole_ontology() {
    let input = robot_fixture("remove-terms.ofn");
    let out = tmp("remove-terms.ofn");
    let empty = tmp("remove-terms-empty.txt");
    std::fs::write(&empty, "").unwrap();
    let unread = tmp("remove-terms-unread.txt");
    std::fs::write(&unread, "ex:A\n").unwrap();
    let (empty, unread) = (empty.to_str().unwrap(), unread.to_str().unwrap());
    for (args, fixture) in [
        (vec![], "remove-terms.all.robot.ofn"),
        (vec!["--term", "ex:A"], "remove-terms.all.robot.ofn"),
        (vec!["--term-file", empty], "remove-terms.all.robot.ofn"),
        (vec!["--term-file", unread], "remove-terms.all.robot.ofn"),
        (vec!["--term", "ex:A", "--axioms", "annotation"], "remove-terms.annotations.robot.ofn"),
        (vec!["--term", "http://example.org/none"], "remove-terms.none.robot.ofn"),
    ] {
        let run = bin().arg("remove").args(&args).arg("-i").arg(&input).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{args:?}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(fixture), "{args:?}");
    }
    // A complement of an entity type is taken over the entities the terms select,
    // so with none it removes nothing — whether no term names an IRI or the
    // ontology has none of them.
    let complement = robot_fixture("remove-complement.ofn");
    let absent = tmp("remove-complement-absent.txt");
    std::fs::write(&absent, "http://example.org/none\n").unwrap();
    let keep_p = tmp("remove-complement-p.txt");
    std::fs::write(&keep_p, "http://example.org/ex#p\n").unwrap();
    let (absent, keep_p) = (absent.to_str().unwrap(), keep_p.to_str().unwrap());
    for (file, kind, fixture) in [
        (empty, "object-properties", "remove-complement.none.robot.ofn"),
        (absent, "object-properties", "remove-complement.none.robot.ofn"),
        (empty, "annotation-properties", "remove-complement.none.robot.ofn"),
        (absent, "annotation-properties", "remove-complement.none.robot.ofn"),
        (keep_p, "object-properties", "remove-complement.keep-p.robot.ofn"),
    ] {
        let run = bin()
            .args(["remove", "--term-file", file, "--select", "complement", "--select", kind, "-i"])
            .arg(&complement)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{file} {kind}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(fixture), "{file} {kind}");
    }
    for f in [&out, std::path::Path::new(empty), std::path::Path::new(unread)] {
        let _ = std::fs::remove_file(f);
    }
    for f in [absent, keep_p] {
        let _ = std::fs::remove_file(f);
    }
}

/// `collapse` removes, pass after pass until none is left, every class that is
/// not `--precious`, has a named superclass other than `owl:Thing`, and is the
/// named superclass of at least one and fewer than `--threshold` (2 when not
/// given) subclasses — with every axiom naming it, its annotation assertions
/// included — and re-asserts the hierarchy among what is left from the input as
/// it was. The document keeps its header and binds `:` to its ontology IRI. A
/// threshold that is not an integer of at least 2 fails. As ROBOT 1.9.11
/// collapses `collapse-chain.ofn`, `collapse-precious.ofn`,
/// `collapse-anonymous.ofn` and `reason-annotate-equivalent.ofn`.
#[test]
fn collapse_removes_intermediate_classes_until_none_qualifies() {
    let ofn = tmp("collapse.ofn");
    let owl = tmp("collapse.owl");
    for (input, args, out, fixture) in [
        ("collapse-chain.ofn", vec![], &ofn, "collapse-chain.robot.ofn"),
        ("collapse-chain.ofn", vec![], &owl, "collapse-chain.robot.owl"),
        ("collapse-chain.ofn", vec!["--precious", "http://example.org/B"], &ofn, "collapse-chain.precious.robot.ofn"),
        ("collapse-precious.ofn", vec![], &ofn, "collapse-precious.robot.ofn"),
        ("collapse-precious.ofn", vec!["--threshold", "3"], &ofn, "collapse-precious.threshold-3.robot.ofn"),
        ("collapse-anonymous.ofn", vec![], &ofn, "collapse-anonymous.robot.ofn"),
        ("reason-annotate-equivalent.ofn", vec![], &ofn, "reason-annotate-equivalent.collapse.robot.ofn"),
    ] {
        let run = bin().arg("collapse").arg("-i").arg(robot_fixture(input)).args(&args).arg("-o").arg(out).output().unwrap();
        assert!(run.status.success(), "{input} {args:?}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(out).unwrap(), fixture_text(fixture), "{input} {args:?}");
    }
    for (threshold, refusal) in [
        ("1", "THRESHOLD VALUE ERROR threshold ('1') must be 2 or greater."),
        ("-3", "THRESHOLD VALUE ERROR threshold ('-3') must be 2 or greater."),
        ("x", "THRESHOLD ERROR threshold ('x') must be a valid integer."),
        ("2.5", "THRESHOLD ERROR threshold ('2.5') must be a valid integer."),
    ] {
        let run = bin()
            .args(["collapse", "-i"])
            .arg(robot_fixture("collapse-chain.ofn"))
            .args(["--threshold", threshold, "-o"])
            .arg(&ofn)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success() && said.contains(refusal), "{threshold}: {said}");
    }
    for f in [&ofn, &owl] {
        let _ = std::fs::remove_file(f);
    }
}

/// `remove`, `filter` and `collapse` re-assert the hierarchy among what they
/// keep: every kept class and property is walked up its asserted superclasses
/// and super-properties, stepping over removed ones. A re-link is asserted
/// without annotations, so an annotated edge gains a plain twin, and a walk that
/// comes back to where it started asserts `A ⊑ A`. An object property keeps an
/// inverse super-property whose property is kept, and past a removed object
/// property the walk leaves out the properties asserted equivalent to it. As
/// ROBOT 1.9.11 does over `span-gaps-*.ofn` and `reason-annotate-equivalent.ofn`.
#[test]
fn gap_spanning_reasserts_the_hierarchy_it_keeps() {
    let out = tmp("span-gaps.ofn");
    let terms = |names: &[&str]| -> Vec<String> {
        names.iter().flat_map(|n| ["--term".to_string(), format!("http://example.org/{n}")]).collect()
    };
    for (command, input, args, fixture) in [
        ("remove", "span-gaps-properties.ofn", terms(&["r", "v"]), "span-gaps-properties.remove-r-v.robot.ofn"),
        ("remove", "span-gaps-properties.ofn", terms(&["e", "y"]), "span-gaps-properties.remove-e-y.robot.ofn"),
        ("remove", "span-gaps-properties.ofn", terms(&["q"]), "span-gaps-properties.remove-q.robot.ofn"),
        ("filter", "span-gaps-properties.ofn", terms(&["q", "u", "s"]), "span-gaps-properties.filter-q-u-s.robot.ofn"),
        ("collapse", "span-gaps-twins.ofn", vec![], "span-gaps-twins.collapse.robot.ofn"),
        ("remove", "span-gaps-twins.ofn", terms(&["B"]), "span-gaps-twins.remove-B.robot.ofn"),
        ("remove", "span-gaps-loops.ofn", terms(&["q", "e", "y", "B"]), "span-gaps-loops.remove.robot.ofn"),
        ("filter", "reason-annotate-equivalent.ofn", terms(&["A", "C"]), "reason-annotate-equivalent.filter-A-C.robot.ofn"),
    ] {
        let run = bin().arg(command).arg("-i").arg(robot_fixture(input)).args(&args).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{command} {input} {args:?}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(fixture), "{command} {input} {args:?}");
    }
    let _ = std::fs::remove_file(&out);
}

/// `filter` and `remove` select as ROBOT does: from the entities the terms name
/// — none for an IRI that names entities of two kinds, unless punning is
/// allowed — or, with no term, from every object of the ontology, anonymous
/// class expressions and individuals among them. Each `--select` selector maps
/// the set it is given, so `classes` keeps the classes of it; an axiom is kept
/// or removed by its objects; `logical` leaves out every annotation axiom; an
/// untyped literal equals the `xsd:string` literal of its text; and a removed
/// individual is no gap in the hierarchy of the class its IRI also names. As
/// ROBOT 1.9.11 does over `select-objects.ofn`.
#[test]
fn filter_and_remove_select_objects_as_robot_does() {
    let out = tmp("select-objects.ofn");
    for (command, tag, args) in [
        ("filter", "classes", &["--select", "classes"][..]),
        ("filter", "C-classes", &["--term", "http://example.org/C", "--select", "classes"]),
        ("filter", "C-self-parents", &["--term", "http://example.org/C", "--select", "self parents"]),
        ("filter", "anonymous", &["--select", "anonymous"]),
        ("filter", "logical", &["--axioms", "logical"]),
        ("filter", "subclass", &["--axioms", "subclass"]),
        ("filter", "label-A", &["--select", "rdfs:label='A'"]),
        ("filter", "iri-pattern", &["--select", "<http://example.org/?>"]),
        ("filter", "A-punning", &["--term", "http://example.org/A", "--allow-punning", "true"]),
        ("filter", "individuals-untrimmed", &["--select", "individuals", "--trim", "false"]),
        ("remove", "individuals", &["--select", "individuals"]),
        ("remove", "classes", &["--select", "classes"]),
        ("remove", "C-equivalents-anonymous", &["--term", "http://example.org/C", "--select", "self equivalents anonymous"]),
        ("remove", "B-signature", &["--term", "http://example.org/B", "--signature", "true"]),
    ] {
        let run = bin()
            .arg(command)
            .arg("-i")
            .arg(robot_fixture("select-objects.ofn"))
            .args(args)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{command} {args:?}: {}", String::from_utf8_lossy(&run.stderr));
        let expected = fixture_text(&format!("select-objects.{command}-{tag}.robot.ofn"));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), expected, "{command} {args:?}");
    }
    let _ = std::fs::remove_file(&out);
}

/// `--drop-axiom-annotations` takes the annotations off the ontology's own
/// axioms as ROBOT 1.9.11 reads it: as often as it is given, `all` for every
/// one, a property for its own, `PROP=VALUE` and `PROP=~REGEX` for those with
/// that value or a value the regex finds, the last value for a property
/// winning, and after a `remove` that removes nothing. An import's axioms are
/// left alone, and a property that names no IRI is refused.
#[test]
fn drop_axiom_annotations_as_robot_reads_it() {
    let out = tmp("drop-annotations.ofn");
    let nothing = "http://example.org/d#Z";
    for (command, tag, args) in [
        ("remove", "r-nothing-all", &["--term", nothing, "-d", "all"][..]),
        ("remove", "r-two-props", &["--term", "http://example.org/d#C", "-d", "rdfs:comment", "-d", "oboInOwl:source"]),
        ("remove", "r-exact", &["--term", nothing, "-d", "rdfs:comment=foo"]),
        ("remove", "r-regex", &["--term", nothing, "-d", "rdfs:comment=~'^f'"]),
        ("remove", "r-iri-value", &["--term", nothing, "-d", "oboInOwl:source=http://example.org/src"]),
        ("remove", "r-last-wins", &["--term", nothing, "-d", "rdfs:comment=foo", "-d", "rdfs:comment"]),
        ("remove", "r-last-wins2", &["--term", nothing, "-d", "rdfs:comment", "-d", "rdfs:comment=foo"]),
        (
            "filter",
            "f-source",
            &["--term", "http://example.org/d#A", "--term", "http://example.org/d#B", "--select", "annotations", "-d", "oboInOwl:source"],
        ),
    ] {
        let run = bin()
            .arg(command)
            .arg("-i")
            .arg(robot_fixture("drop-annotations.ofn"))
            .args(args)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{command} {args:?}: {}", String::from_utf8_lossy(&run.stderr));
        let expected = fixture_text(&format!("drop-annotations.{tag}.robot.ofn"));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), expected, "{command} {args:?}");
    }
    // What the imports lend keeps its annotations and stays theirs.
    let run = bin()
        .args(["remove", "--catalog"])
        .arg(robot_fixture("drop-import-catalog.xml"))
        .arg("-i")
        .arg(robot_fixture("drop-import.ofn"))
        .args(["--term", "http://example.org/p#Z", "-d", "all", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("drop-import.all.robot.ofn"), "with an import");
    for value in ["true", "nope:x"] {
        let run = bin()
            .args(["remove", "-i"])
            .arg(robot_fixture("drop-annotations.ofn"))
            .args(["--term", nothing, "-d", value, "-o"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(!run.status.success(), "-d {value} was not refused");
        assert!(
            String::from_utf8_lossy(&run.stderr)
                .contains(&format!("INVALID IRI ERROR drop-axiom-annotations \"{value}\" is not a valid CURIE or IRI")),
            "-d {value}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    let _ = std::fs::remove_file(&out);
}

/// reduce's switches are on for `true` or `yes` in any case and off for any
/// other value: one keeps a redundant subclass axiom that carries an
/// annotation, the other reduces only the axioms between named classes. As
/// ROBOT 1.9.11 reduces reduce-annotated.ofn with each of these.
#[test]
fn reduce_reads_its_switches_as_robot_does() {
    let out = tmp("reduce-annotated.ofn");
    for (tag, args) in [
        ("plain", &[][..]),
        ("plain", &["--preserve-annotated-axioms", "nope"]),
        ("preserve", &["--preserve-annotated-axioms", "yes"]),
        ("named", &["--named-classes-only", "Yes"]),
    ] {
        let run = bin()
            .args(["reduce", "-i"])
            .arg(robot_fixture("reduce-annotated.ofn"))
            .args(args)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{args:?}: {}", String::from_utf8_lossy(&run.stderr));
        let expected = fixture_text(&format!("reduce-annotated.{tag}.robot.ofn"));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), expected, "{args:?}");
    }
    let _ = std::fs::remove_file(&out);
}

/// reduce removes what its reasoner classifies as redundant. Over every class
/// expression, the reasoner sees the `SubClassOf` axioms and property
/// characteristics alone, so an asserted equivalence makes no subclass axiom
/// redundant, and one asserted superclass hides another above it even through
/// a class equivalent to the subclass. A union, a universal restriction or an
/// unsatisfiable superclass hides more under the reasoners that read them.
/// Between named classes only, an axiom is kept where its subclass's node
/// lies directly below its superclass's: under `whelk` one class stands for
/// each node, under `jfact` the walk from the top reaches only the bottom
/// node, and under `structural` every told parent is direct. As ROBOT 1.9.11
/// reduces `reduce-reasoners.ofn` with each reasoner.
#[test]
fn reduce_removes_what_its_reasoner_finds_redundant() {
    let out = tmp("reduce-reasoners.ofn");
    for reasoner in ["elk", "hermit", "jfact", "whelk", "structural"] {
        for (tag, args) in [("plain", &[][..]), ("named", &["--named-classes-only", "true"][..])] {
            let run = bin()
                .args(["reduce", "-i"])
                .arg(robot_fixture("reduce-reasoners.ofn"))
                .args(["--reasoner", reasoner])
                .args(args)
                .arg("-o")
                .arg(&out)
                .output()
                .unwrap();
            assert!(run.status.success(), "{reasoner} {tag}: {}", String::from_utf8_lossy(&run.stderr));
            assert_eq!(
                std::fs::read_to_string(&out).unwrap(),
                fixture_text(&format!("reduce-reasoners.{reasoner}.{tag}.robot.ofn")),
                "{reasoner} {tag}"
            );
        }
    }
    let _ = std::fs::remove_file(&out);
}

/// reduce removes only the root's own axioms, judged by the superclasses the
/// root asserts, over a classification of the whole import closure: an
/// imported superclass hides nothing, an imported subsumption hides an
/// asserted axiom above it. As ROBOT 1.9.11 reduces `reduce-imports.ofn`.
#[test]
fn reduce_judges_the_root_axioms_over_the_closure() {
    let out = tmp("reduce-imports.ofn");
    for (tag, args) in [("plain", &[][..]), ("named", &["--named-classes-only", "true"][..])] {
        let run = bin()
            .args(["reduce", "--catalog"])
            .arg(robot_fixture("reduce-imports-catalog.xml"))
            .arg("-i")
            .arg(robot_fixture("reduce-imports.ofn"))
            .args(args)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{tag}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            fixture_text(&format!("reduce-imports.{tag}.robot.ofn")),
            "{tag}"
        );
    }
    let _ = std::fs::remove_file(&out);
}

/// An inconsistent ontology loses nothing to reduce. Under `whelk` an
/// ontology is inconsistent only where an individual is unsatisfiable, so
/// `owl:Thing ⊑ owl:Nothing` alone is reduced: over every class expression as
/// any other, and between named classes from the top node's representative,
/// `owl:Nothing`, below which nothing lies. As ROBOT 1.9.11 reduces
/// `reduce-inconsistent.ofn`.
#[test]
fn an_inconsistent_ontology_loses_nothing_to_reduce() {
    let out = tmp("reduce-inconsistent.ofn");
    for reasoner in ["elk", "whelk"] {
        for (tag, args) in [("plain", &[][..]), ("named", &["--named-classes-only", "true"][..])] {
            let run = bin()
                .args(["reduce", "-i"])
                .arg(robot_fixture("reduce-inconsistent.ofn"))
                .args(["--reasoner", reasoner])
                .args(args)
                .arg("-o")
                .arg(&out)
                .output()
                .unwrap();
            assert!(run.status.success(), "{reasoner} {tag}: {}", String::from_utf8_lossy(&run.stderr));
            assert_eq!(
                std::fs::read_to_string(&out).unwrap(),
                fixture_text(&format!("reduce-inconsistent.{reasoner}.{tag}.robot.ofn")),
                "{reasoner} {tag}"
            );
        }
    }
    let _ = std::fs::remove_file(&out);
}

/// extract refuses what ROBOT 1.9.11 refuses: a MIREOT option with another
/// method, no term at all, a MIREOT with neither lower nor branch terms or
/// with upper terms and no lower ones, and terms the ontology does not name.
/// `--force`, `true` or `yes` in any case, extracts the module of terms the
/// ontology does not name, as ROBOT does.
#[test]
fn extract_refuses_and_forces_as_robot_does() {
    let out = tmp("extract-missing.ofn");
    let [a, b, c, z] = ["A", "B", "C", "Z"].map(|local| format!("http://example.org/s#{local}"));
    let refusals: [(Vec<&str>, &str); 6] = [
        (vec!["--method", "BOT"], "MISSING TERMS ERROR term(s) are required with --term or --term-file"),
        (vec!["--method", "BOT", "--force", "true"], "MISSING TERMS ERROR term(s) are required with --term or --term-file"),
        (
            vec!["--method", "BOT", "--term", &a, "--lower-term", &b],
            "INVALID OPTION ERROR only --term or --term-file can be used to specify extract term(s) for methods: star, top, bot, subset",
        ),
        (vec!["--method", "BOT", "--term", &z], "EMPTY TERMS ERROR ontology does not contain input terms"),
        (
            vec!["--method", "MIREOT", "--upper-term", &a],
            "MISSING MIREOT TERMS ERROR either lower term(s) or branch term(s) must be specified for MIREOT",
        ),
        (
            vec!["--method", "MIREOT", "--upper-term", &b, "--branch-from-term", &c],
            "MISSING LOWER TERMS ERROR lower term(s) must be specified with upper term(s) for MIREOT",
        ),
    ];
    for (args, message) in refusals {
        let run = bin()
            .args(["extract", "-i"])
            .arg(robot_fixture("selection-options.ofn"))
            .args(&args)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(!run.status.success(), "{args:?} was not refused");
        assert!(String::from_utf8_lossy(&run.stderr).contains(message), "{args:?}: {}", String::from_utf8_lossy(&run.stderr));
    }
    for force in ["true", "Yes"] {
        let run = bin()
            .args(["extract", "-i"])
            .arg(robot_fixture("selection-options.ofn"))
            .args(["--method", "BOT", "--term", &z, "--force", force, "-o"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "--force {force}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("extract-missing.force.robot.ofn"), "--force {force}");
    }
    let _ = std::fs::remove_file(&out);
}

/// A command reads its switches as ROBOT 1.9.11 reads them. One read as `true`
/// or `false` exactly is refused with ROBOT's message where ROBOT reads it:
/// `remove` and `filter` read theirs only once something is selected, and
/// `query` reads `--use-graphs` only when it queries in memory. One read
/// leniently is on for `true` or `yes` in any case and off for anything else.
#[test]
fn switches_are_read_as_robot_reads_them() {
    let sel = robot_fixture("selection-options.ofn");
    let red = robot_fixture("reduce-annotated.ofn");
    let out = tmp("switch.ofn");
    let (a, c, z) = ("http://example.org/s#A", "http://example.org/s#C", "http://example.org/s#Z");
    let query = robot_fixture("select-classes.rq");
    let csv = tmp("switch.csv");
    let reports = tmp("switch-reports");
    let refusals: [(Vec<std::ffi::OsString>, &str); 11] = [
        (args(&["remove", "--term", c, "--trim", "TRUE"], &sel, &out), "trim"),
        (args(&["remove", "--term", c, "--allow-punning", "yes"], &sel, &out), "allow-punning"),
        (args(&["filter", "--term", c, "--signature", "yes"], &sel, &out), "signature"),
        (args(&["annotate", "--annotation", "rdfs:comment", "x", "--interpolate", "yes"], &sel, &out), "interpolate"),
        (args(&["merge", "--collapse-import-closure", "TRUE"], &sel, &out), "collapse-import-closure"),
        (args(&["convert", "--check", "FALSE"], &sel, &tmp("switch.obo")), "check"),
        (
            args(&["extract", "--method", "BOT", "--term", a, "--copy-ontology-annotations", "True"], &sel, &out),
            "copy-ontology-annotations",
        ),
        (args(&["relax", "--include-subclass-of", "1"], &red, &out), "include-subclass-of"),
        (args(&["repair", "--invalid-references", "yes"], &sel, &out), "invalid-references"),
        (
            ["query", "--input"].iter().map(Into::into).chain([sel.clone().into_os_string()])
                .chain(["--use-graphs", "TRUE", "--query"].iter().map(Into::into))
                .chain([query.clone().into_os_string(), csv.clone().into_os_string()])
                .collect(),
            "use-graphs",
        ),
        (
            ["verify", "--input"].iter().map(Into::into).chain([sel.clone().into_os_string()])
                .chain(["--fail-on-violation", "none", "--queries"].iter().map(Into::into))
                .chain([robot_fixture("select-nothing.rq").into_os_string(), "--output-dir".into(), reports.clone().into_os_string()])
                .collect(),
            "fail-on-violation",
        ),
    ];
    for (argv, switch) in &refusals {
        let run = bin().args(argv).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success(), "{argv:?} was not refused");
        assert!(
            stderr.contains(&format!("BOOLEAN VALUE ERROR arg for {switch} must be true or false")),
            "{argv:?}: {stderr}"
        );
    }
    // Read where something is selected, and so not read where nothing is.
    for (argv, robot) in [
        (args(&["remove", "--term", z, "--trim", "TRUE"], &sel, &out), "switch-remove-empty.robot.ofn"),
        (args(&["filter", "--term", z, "--preserve-structure", "nope"], &sel, &out), "switch-filter-empty.robot.ofn"),
        // Read leniently.
        (args(&["reason", "--exclude-owl-thing", "Yes"], &red, &out), "reduce-annotated.reason-thing.robot.ofn"),
        (args(&["reason", "--annotate-inferred-axioms", "nope"], &red, &out), "reduce-annotated.reason.robot.ofn"),
        (
            args(&["materialize", "--term", "http://example.org/r#p", "--create-new-ontology", "yes"], &red, &out),
            "reduce-annotated.materialize-new.robot.ofn",
        ),
    ] {
        let run = bin().args(&argv).output().unwrap();
        assert!(run.status.success(), "{argv:?}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(robot), "{argv:?}");
    }
    // On disk, `query` does not read `--use-graphs`.
    let run = bin()
        .args(["query", "--input"])
        .arg(robot_fixture("selection-options.owl"))
        .args(["--tdb", "true", "--use-graphs", "nope", "--query"])
        .arg(&query)
        .arg(&csv)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&csv).unwrap(), fixture_text("selection-options.classes.robot.csv"));
    for path in [&out, &csv] {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_dir_all(&reports);

    /// `COMMAND --input INPUT REST… --output OUTPUT`.
    fn args(command: &[&str], input: &std::path::Path, output: &std::path::Path) -> Vec<std::ffi::OsString> {
        let mut argv: Vec<std::ffi::OsString> = vec![command[0].into(), "--input".into(), input.into()];
        argv.extend(command[1..].iter().map(Into::into));
        argv.extend(["--output".into(), output.into()]);
        argv
    }
}

/// A command's own prefix options are its alone: the command after it reads its
/// CURIEs with the options stated before the chain's first command, and declares
/// what those add, but not what its predecessor's own added. As ROBOT 1.9.11
/// refuses `template --prefix "zz: …" … annotate --link-annotation rdfs:seeAlso
/// zz:x` and accepts it with the `--prefix` stated first, and writes
/// `template --add-prefix "zz: …" … annotate …` without `zz:`.
#[test]
fn a_commands_own_prefix_options_are_its_alone() {
    let table = robot_fixture("own-prefix.tsv");
    let out = tmp("own-prefix.ofn");
    let stderr = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).to_string();
    let zz = "zz: http://example.org/zz#";

    let run = bin()
        .args(["template", "--prefix", zz, "--template"])
        .arg(&table)
        .args(["annotate", "--link-annotation", "rdfs:seeAlso", "zz:x", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    let said = stderr(&run);
    let refusal = "INVALID IRI ERROR value \"zz:x\" is not a valid CURIE or IRI";
    assert!(!run.status.success() && said.contains(refusal), "{said}");

    let run = bin()
        .args(["--prefix", zz, "template", "--template"])
        .arg(&table)
        .args(["annotate", "--link-annotation", "rdfs:seeAlso", "zz:x", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", stderr(&run));
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("Annotation(rdfs:seeAlso <http://example.org/zz#x>)"), "{text}");

    for (leading, own, fixture) in [
        (vec![], vec!["--add-prefix", zz], "own-add-prefix.robot.ofn"),
        (vec!["--add-prefix", zz], vec![], "chain-add-prefix.robot.ofn"),
    ] {
        let run = bin()
            .args(&leading)
            .arg("template")
            .args(&own)
            .arg("--template")
            .arg(&table)
            .args(["annotate", "--annotation", "rdfs:comment", "x", "-o"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", stderr(&run));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(fixture), "{fixture}");
    }
    let _ = std::fs::remove_file(&out);
}

/// In Manchester syntax a prefix name standing alone, `idsfor:`, names the
/// prefix's own IRI — as an ID-ranges file names its annotation properties —
/// wherever an entity may stand. As ROBOT 1.9.11 reads
/// `manchester-prefix-names.omn`, in functional syntax, RDF/XML and Turtle.
#[test]
fn a_manchester_prefix_name_standing_alone_names_the_prefix() {
    for ext in ["ofn", "owl", "ttl"] {
        assert_eq!(
            convert_fixture("manchester-prefix-names.omn", &format!("manchester-prefix-names.{ext}"), &[]),
            fixture_text(&format!("manchester-prefix-names.robot.{ext}")),
            "{ext}"
        );
    }
}

/// Line-based RDF is the same bytes on every run: N-Triples, and Turtle for a
/// document the layout cannot state. Each blank node takes its label from the
/// order the mapping first names it, and the triples are sorted.
#[test]
fn line_based_rdf_is_the_same_on_every_run() {
    let src = tmp("blank-nodes.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://example.org/a#>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(\n\
         Annotation(Annotation(rdfs:comment \"inner\") rdfs:comment \"c\")\n\
         SubClassOf(:A ObjectSomeValuesFrom(:p ObjectIntersectionOf(:B ObjectSomeValuesFrom(:q :C))))\n)\n",
    )
    .unwrap();
    for ext in ["nt", "ttl"] {
        let runs: Vec<Vec<u8>> = (0..3)
            .map(|i| {
                let out = tmp(&format!("blank-nodes-{i}.{ext}"));
                let run = bin().args(["convert", "-i"]).arg(&src).arg("-o").arg(&out).output().unwrap();
                assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
                let text = std::fs::read(&out).unwrap();
                let _ = std::fs::remove_file(&out);
                text
            })
            .collect();
        assert!(runs.iter().all(|r| *r == runs[0]), "{ext} differs between runs");
        assert!(String::from_utf8_lossy(&runs[0]).contains("_:b0"), "{ext}: {}", String::from_utf8_lossy(&runs[0]));
    }
    let _ = std::fs::remove_file(&src);
}

/// An empty list is `rdf:nil` in line-based RDF: the intersection, union and
/// one-of with no members in `rdf-connective-members.owl` are written as
/// N-Triples, and the document read back holds the same axioms.
#[test]
fn line_based_rdf_writes_an_empty_list_as_rdf_nil() {
    let nt = tmp("rdf-connective-members.nt");
    let run = bin()
        .args(["convert", "-i"])
        .arg(robot_fixture("rdf-connective-members.owl"))
        .arg("-o")
        .arg(&nt)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let diff = tmp("rdf-connective-members.nt.diff");
    let run = bin()
        .args(["diff", "--left"])
        .arg(robot_fixture("rdf-connective-members.owl"))
        .arg("--right")
        .arg(&nt)
        .arg("-o")
        .arg(&diff)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&diff).unwrap().trim_end(), "Ontologies are identical");
    let _ = std::fs::remove_file(&nt);
    let _ = std::fs::remove_file(&diff);
}

/// A rename keeps every annotation of the ontology on the ontology, one with
/// an annotation of its own and the ones after it alike. As ROBOT 1.9.11
/// renames `rename-header.ofn`.
#[test]
fn a_rename_keeps_each_ontology_annotation_on_the_ontology() {
    let out = tmp("rename-header.ofn");
    let run = bin()
        .args(["rename", "-i"])
        .arg(robot_fixture("rename-header.ofn"))
        .arg("--mappings")
        .arg(robot_fixture("rename-header.mappings.tsv"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text("rename-header.robot.ofn"));
    let _ = std::fs::remove_file(&out);
}

/// A rename gives an entity its new IRI in every axiom that names it, every
/// annotation assertion about it and every ontology annotation, wherever the IRI
/// stands there; an axiom that holds the IRI only as an annotation value keeps
/// it. A row's label replaces the renamed entity's labels, the ontology
/// annotations keep their own annotations, and the anonymous individuals keep
/// the names the read gave them. As ROBOT 1.9.11 renames `rename-entities.ofn`
/// from a table and a `--mapping` pair, and `obo-header-annotations.ofn`.
#[test]
fn a_rename_rewrites_what_names_the_entity_and_keeps_the_rest() {
    for (src, mappings, pairs, expected) in [
        (
            "rename-entities.ofn",
            "rename-entities.mappings.tsv",
            &["http://example.org/rename-entities#B", "http://example.org/rename-entities#Y"][..],
            "rename-entities.robot.ofn",
        ),
        ("obo-header-annotations.ofn", "rename-obo-header.mappings.tsv", &[][..], "rename-obo-header.robot.ofn"),
    ] {
        let out = tmp(&format!("renamed-{expected}"));
        let mut cmd = bin();
        cmd.args(["rename", "-i"]).arg(robot_fixture(src)).arg("--mappings").arg(robot_fixture(mappings));
        if !pairs.is_empty() {
            cmd.arg("--mapping").args(pairs);
        }
        let run = cmd.arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{src}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), fixture_text(expected), "{src}");
        let _ = std::fs::remove_file(&out);
    }
}

/// `owltools … --run-reasoner -u` lists, after the unsatisfiable count, every
/// direct superclass the reasoner infers that no `SubClassOf` asserts, and every
/// named equivalence — each class as its id and quoted label, or its id twice
/// when it has none. The classes come in the order owltools 2020-04-06
/// (OWLAPI 4.5.6, Trove 3.0.3) lists them, the same three times over: not
/// IRI order.
#[test]
fn owltools_run_reasoner_lists_inferences_in_hash_set_order() {
    let inp = tmp("run-reasoner.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://purl.obolibrary.org/obo/>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://purl.obolibrary.org/obo/x.owl>\n\
         Declaration(Class(:X_1))\n\
         Declaration(Class(:X_2))\n\
         Declaration(Class(:X_3))\n\
         Declaration(Class(:X_4))\n\
         Declaration(Class(:X_5))\n\
         Declaration(Class(:X_6))\n\
         Declaration(Class(:X_8))\n\
         Declaration(Class(:X_9))\n\
         Declaration(ObjectProperty(:BFO_0000050))\n\
         AnnotationAssertion(rdfs:label :X_1 \"one\")\n\
         AnnotationAssertion(rdfs:label :X_2 \"two\")\n\
         AnnotationAssertion(rdfs:label :X_3 \"three\")\n\
         AnnotationAssertion(rdfs:label :X_4 \"four\")\n\
         AnnotationAssertion(rdfs:label :X_5 \"five\")\n\
         AnnotationAssertion(rdfs:label :X_6 \"six\")\n\
         AnnotationAssertion(rdfs:label :X_8 \"eight\")\n\
         EquivalentClasses(:X_1 ObjectIntersectionOf(:X_2 ObjectSomeValuesFrom(:BFO_0000050 :X_3)))\n\
         SubClassOf(:X_4 :X_2)\n\
         EquivalentClasses(:X_5 :X_6)\n\
         SubClassOf(:X_8 :X_2)\n\
         SubClassOf(:X_8 ObjectSomeValuesFrom(:BFO_0000050 :X_3))\n\
         SubClassOf(:X_9 :X_2)\n\
         SubClassOf(:X_9 ObjectSomeValuesFrom(:BFO_0000050 :X_3))\n\
         )\n",
    )
    .unwrap();
    let out = bin()
        .args(["owltools", "--no-debug", inp.to_str().unwrap(), "--silence-elk", "--run-reasoner", "-r", "elk", "-u"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "NUMBER_OF_UNSATISFIABLE_CLASSES: 0\n\
         all inferences\n\
         Consistent? true\n\
         INFERENCE: X:9 X:9 SubClassOf X:1 'one'\n\
         INFERENCE: X:8 'eight' SubClassOf X:1 'one'\n\
         INFERENCE: X:6 'six' EquivalentTo X:5 'five'\n\
         INFERENCE: X:5 'five' EquivalentTo X:6 'six'\n\
         INFERENCE: X:1 'one' SubClassOf X:2 'two'\n"
    );
}

/// A `SELECT DISTINCT` over one pattern with only its predicate bound, and one
/// over a UNION of single-pattern groups, come out in the order the graph
/// answers them: each branch in turn from the index its bound terms select,
/// each value at its first appearance. A literal and an IRI with the same text
/// are two rows. The expected tables are ROBOT 1.9.10's (ODK v1.6.1), the same
/// three times over.
#[test]
fn query_answers_predicate_scans_and_unions_in_index_order() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/query-order");
    for (query, expected) in [("contributors.sparql", "contributors.robot.csv"), ("seed.sparql", "seed.robot.csv")] {
        let out = tmp(expected);
        let run = bin()
            .args(["query", "-f", "csv", "-i"])
            .arg(fixtures.join("contributors.ofn"))
            .arg("--query")
            .arg(fixtures.join(query))
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            std::fs::read_to_string(fixtures.join(expected)).unwrap(),
            "{query}"
        );
    }
}

/// A query reads an inverse property expression that several statements name
/// as one node: `:q`'s super-property `ObjectInverseOf(:u)`, which an
/// annotated axiom's reification also names, is the node stating its inverse.
/// As ROBOT 1.9.11 answers over `span-gaps-properties`.
#[test]
fn a_query_joins_through_a_node_several_statements_name() {
    for (name, query) in [
        (
            "inverse-supers",
            "SELECT ?sub ?inverse WHERE { ?sub rdfs:subPropertyOf ?super . ?super owl:inverseOf ?inverse } \
             ORDER BY ?sub ?inverse",
        ),
        (
            "inverse-targets",
            "SELECT ?source ?inverse WHERE { ?axiom owl:annotatedSource ?source ; owl:annotatedTarget ?target . \
             ?target owl:inverseOf ?inverse } ORDER BY ?source ?inverse",
        ),
    ] {
        let rq = tmp(&format!("{name}.rq"));
        std::fs::write(
            &rq,
            format!(
                "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\nPREFIX owl: <http://www.w3.org/2002/07/owl#>\n{query}\n"
            ),
        )
        .unwrap();
        let out = tmp(&format!("{name}.csv"));
        let run = bin()
            .args(["query", "-i"])
            .arg(robot_fixture("span-gaps-properties.ofn"))
            .arg("--query")
            .arg(&rq)
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            fixture_text(&format!("span-gaps-properties.{name}.robot.csv")),
            "{name}"
        );
    }
}

/// A query reads the ontology's RDF rendering, the document a file of it holds.
/// The rendering types an entity the ontology names without declaring unless
/// an ontology it imports has it: `:q`, declared nowhere, is an object
/// property, and the chain's members, which the import declares, are not. Each
/// cell of a list is an `rdf:List`, and a document with an empty list is read.
/// As ROBOT 1.9.11 answers.
#[test]
fn a_query_reads_the_ontology_as_a_file_of_it_states_it() {
    let dir = tmp("query-rendering");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("imp.ofn"),
        "Prefix(:=<http://example.org/c#>)\nOntology(<http://example.org/imp.owl>\n\
         Declaration(ObjectProperty(:bfo50))\nDeclaration(ObjectProperty(:bfo51))\n)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("root.ofn"),
        "Prefix(:=<http://example.org/c#>)\nOntology(<http://example.org/root.owl>\n\
         Import(<http://example.org/imp.owl>)\nDeclaration(Class(:A))\nDeclaration(ObjectProperty(:p))\n\
         SubObjectPropertyOf(ObjectPropertyChain(:bfo50 :bfo51) :p)\nSubClassOf(:A ObjectSomeValuesFrom(:q :A))\n)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         \x20   <uri name=\"http://example.org/imp.owl\" uri=\"imp.ofn\"/>\n</catalog>\n",
    )
    .unwrap();
    let query = |input: &std::path::Path, sparql: &str, name: &str| -> String {
        let rq = dir.join(format!("{name}.rq"));
        std::fs::write(&rq, sparql).unwrap();
        let out = dir.join(format!("{name}.csv"));
        let run = bin().args(["query", "-i"]).arg(input).arg("--query").arg(&rq).arg(&out).output().unwrap();
        assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
        std::fs::read_to_string(&out).unwrap()
    };
    assert_eq!(
        query(
            &dir.join("root.ofn"),
            "PREFIX owl: <http://www.w3.org/2002/07/owl#>\nSELECT ?p WHERE { ?p a owl:ObjectProperty } ORDER BY ?p\n",
            "properties"
        ),
        "p\r\nhttp://example.org/c#p\r\nhttp://example.org/c#q\r\n"
    );
    assert_eq!(
        query(
            &robot_fixture("select-objects.ofn"),
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\n\
             SELECT ?first WHERE { ?cell a rdf:List ; rdf:first ?first . FILTER(isIRI(?first)) } ORDER BY ?first\n",
            "lists"
        ),
        "first\r\nhttp://example.org/B\r\nhttp://example.org/B\r\nhttp://example.org/C\r\n"
    );
    assert_eq!(
        query(
            &robot_fixture("rdf-connective-members.owl"),
            "SELECT ?s WHERE { ?s a <http://www.w3.org/2002/07/owl#Class> } ORDER BY ?s\n",
            "classes"
        ),
        fixture_text("rdf-connective-members.classes.robot.csv")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A functional write banners an entity with two labels by the one its
/// annotation-assertion set yields first, and that set is sized by what the
/// entity holds as written. `label4` precedes `label10` in a 16-slot table and
/// follows it in a 32-slot one: merging twelve comments in takes E from 4
/// assertions to 16, and extracting a module and stripping its comments takes
/// it from 16 to 2. The expected documents are ROBOT 1.9.10's (ODK v1.6.1),
/// the same three times over.
#[test]
fn banner_labels_follow_the_document_as_written() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/banner-labels");
    let f = |name: &str| fixtures.join(name).to_str().unwrap().to_string();
    let merged = tmp("banner-merged.ofn");
    let run = bin()
        .args(["merge", "-i", &f("two-labels.ofn"), "-i", &f("more-comments.ofn"), "convert", "-f", "ofn", "-o"])
        .arg(&merged)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        std::fs::read_to_string(&merged).unwrap(),
        std::fs::read_to_string(fixtures.join("merged.robot.ofn")).unwrap()
    );

    let extracted = tmp("banner-extracted.ofn");
    let run = bin()
        .args(["merge", "-i", &f("many-comments.ofn")])
        .args(["extract", "--method", "BOT", "--term", "http://example.org/E", "--force", "true"])
        .args(["--copy-ontology-annotations", "false"])
        .args(["remove", "--term", "rdfs:label", "--term", "http://example.org/E"])
        .args(["--select", "complement", "--select", "annotation-properties"])
        .args(["convert", "-f", "ofn", "-o"])
        .arg(&extracted)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        std::fs::read_to_string(&extracted).unwrap(),
        std::fs::read_to_string(fixtures.join("extracted.robot.ofn")).unwrap()
    );
}

/// `explain --output` saves the ontology the command was given; what it hands
/// the next command is the ontology of its justifications. The expected
/// document is ROBOT 1.9.10's (ODK v1.6.1) for the same command line: `D ⊑ E`
/// explains nothing and is saved all the same.
#[test]
fn explain_output_saves_the_ontology_it_was_given() {
    let inp = tmp("explain-output-in.ofn");
    std::fs::write(
        &inp,
        "Prefix(:=<http://example.org/>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/inc.owl>\n\
         Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         Declaration(Class(:C))\n\
         Declaration(Class(:D))\n\
         Declaration(Class(:E))\n\
         AnnotationAssertion(rdfs:label :A \"a\")\n\
         SubClassOf(:A :B)\n\
         SubClassOf(:A :C)\n\
         DisjointClasses(:B :C)\n\
         SubClassOf(:D :E)\n\
         )\n",
    )
    .unwrap();
    let out = tmp("explain-output-out.ofn");
    let run = bin()
        .args(["explain", "-i"])
        .arg(&inp)
        .args(["-M", "unsatisfiability", "-u", "all", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "Prefix(:=<http://example.org/inc.owl#>)\n\
         Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
         Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)\n\
         Prefix(xml:=<http://www.w3.org/XML/1998/namespace>)\n\
         Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         \n\
         \n\
         Ontology(<http://example.org/inc.owl>\n\
         \n\
         Declaration(Class(<http://example.org/A>))\n\
         Declaration(Class(<http://example.org/B>))\n\
         Declaration(Class(<http://example.org/C>))\n\
         Declaration(Class(<http://example.org/D>))\n\
         Declaration(Class(<http://example.org/E>))\n\
         \n\
         \n\
         ############################\n\
         #   Classes\n\
         ############################\n\
         \n\
         # Class: <http://example.org/A> (a)\n\
         \n\
         AnnotationAssertion(rdfs:label <http://example.org/A> \"a\")\n\
         SubClassOf(<http://example.org/A> <http://example.org/B>)\n\
         SubClassOf(<http://example.org/A> <http://example.org/C>)\n\
         \n\
         # Class: <http://example.org/B> (<http://example.org/B>)\n\
         \n\
         DisjointClasses(<http://example.org/B> <http://example.org/C>)\n\
         \n\
         # Class: <http://example.org/D> (<http://example.org/D>)\n\
         \n\
         SubClassOf(<http://example.org/D> <http://example.org/E>)\n\
         \n\
         \n\
         )"
    );
}

/// A markdown diff writes a literal as ROBOT 1.9.10's owl-diff renderer does
/// (ODK v1.6.1): `xsd:decimal`, `xsd:integer` and `xsd:boolean` values bare,
/// `xsd:float` with an `f`, every other value quoted with its text
/// HTML-escaped — newlines, tabs and backslashes as they stand. Only the
/// `Loaded from` lines, which name where each side was read, are not compared.
#[test]
fn markdown_diff_writes_literals_as_robot_does() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/markdown-literals");
    let out = tmp("markdown-literals.md");
    let run = bin()
        .args(["diff", "--labels", "true", "--left"])
        .arg(fixtures.join("left.ofn"))
        .arg("--right")
        .arg(fixtures.join("right.ofn"))
        .args(["-f", "markdown", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let comparable = |text: String| -> String {
        text.lines().filter(|l| !l.starts_with("- Loaded from: ")).map(|l| format!("{l}\n")).collect()
    };
    assert_eq!(
        comparable(std::fs::read_to_string(&out).unwrap()),
        comparable(std::fs::read_to_string(fixtures.join("diff.robot.md")).unwrap())
    );
}

/// `merge` reads every `--input` file, then every `-I/--input-iri`, and an IRI
/// the catalog maps is read from the file it maps it to — `robot --catalog
/// catalog-v001.xml merge -i uberon.owl -I <cl PURL>` merges the repo's own
/// CL module, never a download. A command that reads one input refuses two.
#[test]
fn merge_reads_input_iris_after_its_files_through_the_catalog() {
    let dir = tmp("mergeiri");
    std::fs::create_dir_all(dir.join("imports")).unwrap();
    std::fs::write(
        dir.join("a.ofn"),
        "Prefix(:=<http://x.org/>)\nOntology(<http://x.org/a>\nDeclaration(Class(<http://x.org/A>))\n)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("imports/b.ofn"),
        "Prefix(:=<http://x.org/>)\nOntology(<http://x.org/b>\nDeclaration(Class(<http://x.org/B>))\n)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         <uri name=\"http://example.invalid/b.owl\" uri=\"imports/b.ofn\"/>\n\
         </catalog>\n",
    )
    .unwrap();
    let out = dir.join("merged.ofn");
    let status = bin()
        .current_dir(&dir)
        .args(["merge", "--catalog", "catalog-v001.xml", "-i", "a.ofn", "-I", "http://example.invalid/b.owl", "-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "merge with a catalog-mapped -I failed");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("Ontology(<http://x.org/a>"), "the first --input is the primary:\n{text}");
    assert!(text.contains("Declaration(Class(<http://x.org/A>))"), "the --input file was not merged:\n{text}");
    assert!(text.contains("Declaration(Class(<http://x.org/B>))"), "the -I input was not merged:\n{text}");

    // A mapped file that is missing is an error, never a download.
    std::fs::remove_file(dir.join("imports/b.ofn")).unwrap();
    let status = bin()
        .current_dir(&dir)
        .args(["merge", "--catalog", "catalog-v001.xml", "-i", "a.ofn", "-I", "http://example.invalid/b.owl", "-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(!status.success(), "a catalog entry naming a missing file must fail");

    // One input only, for a command that reads one.
    let status = bin()
        .current_dir(&dir)
        .args(["convert", "-i", "a.ofn", "-I", "http://example.invalid/b.owl", "-o"])
        .arg(dir.join("c.ofn"))
        .status()
        .unwrap();
    assert!(!status.success(), "convert accepted both --input and --input-iri");
}

/// Two axioms the same but for their annotations are written in the order of
/// those annotations, an IRI value by namespace and then by the rest, as ROBOT
/// 1.9.11 writes them: declarations, an entity's annotation assertions and its
/// other axioms alike.
#[test]
fn axioms_that_differ_only_in_annotations_are_ordered_by_them() {
    assert_eq!(
        convert_fixture("annotation-twins.ofn", "annotation-twins.ofn", &[]),
        fixture_text("annotation-twins.robot.ofn")
    );
}

/// `merge` attributes entities and axioms to the ontologies they come from as
/// ROBOT 1.9.11 does: through each input's imports closure in turn, an entity
/// in an import defined by the import rather than by its importer, and without
/// collapsing the closure, only the first input keeping its imports. Every
/// expected file is ROBOT's output for the same command.
#[test]
fn merge_attributes_provenance_as_robot_does() {
    let catalog = robot_fixture("merge-prov-catalog.xml");
    let (p, s) = ("merge-prov-p.ofn", "merge-prov-s.ofn");
    let cases: &[(&str, &[&str], &[&str])] = &[
        ("defined-by", &[p, s], &["--annotate-defined-by", "true"]),
        ("derived-from", &[p, s], &["--annotate-derived-from", "true"]),
        ("both", &[p, s], &["--annotate-defined-by", "true", "--annotate-derived-from", "true"]),
        ("keep-imports.defined-by", &[p, s], &["--collapse-import-closure", "false", "--annotate-defined-by", "true"]),
        ("keep-imports.derived-from", &[p, s], &["--collapse-import-closure", "false", "--annotate-derived-from", "true"]),
        ("keep-imports.annotations", &[p, s], &["--collapse-import-closure", "false", "--include-annotations", "true"]),
        ("one.defined-by", &[p], &["--annotate-defined-by", "true"]),
        ("one.derived-from", &[p], &["--annotate-derived-from", "true"]),
        ("secondary-first.defined-by", &[s, p], &["--annotate-defined-by", "true"]),
        ("anonymous-first.defined-by", &["merge-prov-anon.ofn", p], &["--annotate-defined-by", "true"]),
    ];
    for (name, inputs, options) in cases {
        let out = tmp(&format!("merge-prov.{name}.ofn"));
        let mut cmd = bin();
        cmd.arg("merge").arg("--catalog").arg(&catalog);
        for input in *inputs {
            cmd.arg("-i").arg(robot_fixture(input));
        }
        let run = cmd.args(*options).arg("-o").arg(&out).output().unwrap();
        assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            fixture_text(&format!("merge-prov.{name}.robot.ofn")),
            "{name}"
        );
        let _ = std::fs::remove_file(&out);
    }
    // An ontology with no IRI has nothing its axioms could be derived from.
    let run = bin()
        .arg("merge")
        .arg("--catalog")
        .arg(&catalog)
        .arg("-i")
        .arg(robot_fixture(p))
        .arg("-i")
        .arg(robot_fixture("merge-prov-anon.ofn"))
        .args(["--annotate-derived-from", "true", "-o"])
        .arg(tmp("merge-prov.anonymous.ofn"))
        .output()
        .unwrap();
    assert!(!run.status.success(), "derived-from over an ontology with no IRI succeeded");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("use Optional.orNull() instead of Optional.or(null)"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    // `--inputs` takes a wildcard pattern, and a merge whose pattern matches
    // nothing has nothing to merge.
    for (pattern, message) in [
        (robot_fixture(p), "WILDCARD ERROR --inputs argument must be a quoted wildcard pattern"),
        (robot_fixture("merge-prov-*.none"), "MISSING INPUT ERROR at least one --input is required"),
    ] {
        let run = bin()
            .arg("merge")
            .arg("--inputs")
            .arg(&pattern)
            .arg("-o")
            .arg(tmp("merge-prov.pattern.ofn"))
            .output()
            .unwrap();
        assert!(!run.status.success(), "{}", pattern.display());
        assert!(String::from_utf8_lossy(&run.stderr).contains(message), "{}", String::from_utf8_lossy(&run.stderr));
    }
}

/// Every command reads its input with the input's imports closure, so an import
/// that resolves nowhere fails it, naming the import, as ROBOT 1.9.11 fails
/// every load with `UnloadableImportException`. `template` reads its input as
/// optional: one it cannot read is no input, so the table alone is written, and
/// only a merge with it fails.
#[test]
fn an_import_that_resolves_nowhere_fails_the_load() {
    let import = "file:///nonexistent-owlmake-fixture/unresolvable-import.owl";
    let input = robot_fixture("import-unresolvable.ofn");
    let other = robot_fixture("annotation-twins.ofn");
    let dir = tmp("unresolvable-import");
    std::fs::create_dir_all(&dir).unwrap();
    let query = dir.join("q.rq");
    std::fs::write(&query, "SELECT ?s WHERE { ?s ?p ?o }\n").unwrap();
    let (i, o, q) = (input.to_str().unwrap(), other.to_str().unwrap(), query.to_str().unwrap());
    let out = |name: &str| dir.join(name).to_str().unwrap().to_string();
    let table = robot_fixture("literal-template.tsv");
    let t = table.to_str().unwrap();
    let mint_ranges = ["--temp-id-prefix", "http://example.org/TEMP_", "--id-range-name", "x"];
    let runs: Vec<Vec<String>> = [
        vec!["convert", "-i", i, "-o", &out("c.ofn")],
        vec!["annotate", "-i", i, "--annotation", "rdfs:comment", "x", "-o", &out("a.ofn")],
        vec!["report", "-i", i, "-o", &out("r.tsv")],
        vec!["query", "-i", i, "--query", q, &out("q.csv")],
        vec!["verify", "-i", i, "--queries", q, "-O", &out("verify")],
        vec!["diff", "--left", i, "--right", o],
        vec!["diff", "--left", o, "--right", i],
        vec!["unmerge", "-i", o, "-i", i, "-o", &out("u.ofn")],
        vec!["merge", "-i", i, "-o", &out("m.ofn")],
        vec!["remove", "-i", i, "--term", "http://example.org/x", "-o", &out("rm.ofn")],
        [&["mint", "-i", i][..], &mint_ranges[..], &["-o", &out("mint.ofn")][..]].concat(),
        vec!["template", "-i", i, "--merge-before", "true", "--prefix", "ex: http://example.org/t#", "-t", t, "-o", &out("tm.ofn")],
    ]
    .into_iter()
    .map(|args| args.into_iter().map(String::from).collect())
    .collect();
    for args in &runs {
        let run = bin().args(args).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success() && stderr.contains(import), "{args:?}: {stderr}");
    }
    let run = bin()
        .args(["template", "-i", i, "--prefix", "ex: http://example.org/t#", "-t", t, "-o", &out("t.ofn")])
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        std::fs::read_to_string(out("t.ofn")).unwrap(),
        fixture_text("import-unresolvable.template.robot.ofn")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An import named by a `file:` IRI is read from the file it names, as ROBOT
/// 1.9.11 reads it, whatever command loads the importer.
#[test]
fn a_file_iri_import_is_read_from_its_file() {
    let dir = tmp("file-iri-import");
    std::fs::create_dir_all(dir.join("in sub")).unwrap();
    std::fs::write(
        dir.join("in sub/imported.ofn"),
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/imported>\n\
         Declaration(Class(<http://example.org/imported#S>))\n\
         AnnotationAssertion(rdfs:label <http://example.org/imported#S> \"from a file IRI\")\n)\n",
    )
    .unwrap();
    let importer = dir.join("importer.ofn");
    std::fs::write(
        &importer,
        format!(
            "Ontology(<http://example.org/importer>\n\
             Import(<file://{}/in%20sub/imported.ofn>)\n\
             SubClassOf(<http://example.org/imported#S> <http://example.org/importer#A>)\n)\n",
            dir.display()
        ),
    )
    .unwrap();
    // The label the import gives `S` heads its section in the importer…
    let converted = dir.join("converted.ofn");
    let run = bin().arg("convert").arg("-i").arg(&importer).arg("-o").arg(&converted).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let text = std::fs::read_to_string(&converted).unwrap();
    assert!(text.contains("# Class: <http://example.org/imported#S> (from a file IRI)"), "{text}");
    // …and a merge takes the import's axioms in.
    let merged = dir.join("merged.ofn");
    let run = bin().arg("merge").arg("-i").arg(&importer).arg("-o").arg(&merged).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let text = std::fs::read_to_string(&merged).unwrap();
    assert!(
        text.contains("AnnotationAssertion(rdfs:label <http://example.org/imported#S> \"from a file IRI\")"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}


/// `--explanation` writes the markdown report: the justification as a tree grown
/// from the entailment's subject, each axiom in Manchester syntax with every
/// entity a `[label](IRI)` link, then the axiom impact summary tagging each
/// axiom with the ontology it comes from. The expected text is ROBOT 1.9.10's
/// report for the same command.
#[test]
fn explain_writes_the_markdown_report() {
    let root = plant_import_fixture("plant-md");
    let catalog = root.with_file_name("catalog-v001.xml");
    let md = root.with_file_name("tepal.md");
    let status = bin()
        .args(["explain", "-i"])
        .arg(&root)
        .arg("--catalog")
        .arg(&catalog)
        .args(["--prefix", "po: http://x.org/po#", "--prefix", "rs: http://x.org/root#"])
        .args(["--axiom", "po:Tepal SubClassOf rs:ReproSystem", "--explanation"])
        .arg(&md)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&md).unwrap(), "## [Tepal](http://x.org/po#Tepal) SubClassOf [ReproSystem](http://x.org/root#ReproSystem) ##

  - [Tepal](http://x.org/po#Tepal) SubClassOf [Perianth](http://x.org/po#Perianth)
    - [Perianth](http://x.org/po#Perianth) EquivalentTo [Organ](http://x.org/po#Organ) and ([part_of](http://x.org/po#part_of) some [Flower](http://x.org/po#Flower))
      - [Flower](http://x.org/po#Flower) SubClassOf [part_of](http://x.org/po#part_of) some [ReproSystem](http://x.org/root#ReproSystem)
        - [ReproSystem](http://x.org/root#ReproSystem) EquivalentTo [Structure](http://x.org/po#Structure) and ([part_of](http://x.org/po#part_of) some [ReproSystem](http://x.org/root#ReproSystem))
      - [Organ](http://x.org/po#Organ) SubClassOf [Structure](http://x.org/po#Structure)
      - [Flower](http://x.org/po#Flower) SubClassOf [Structure](http://x.org/po#Structure)

# Axiom Impact 
## Axioms used 1 times
- [Perianth](http://x.org/po#Perianth) EquivalentTo [Organ](http://x.org/po#Organ) and ([part_of](http://x.org/po#part_of) some [Flower](http://x.org/po#Flower)) [po]
- [ReproSystem](http://x.org/root#ReproSystem) EquivalentTo [Structure](http://x.org/po#Structure) and ([part_of](http://x.org/po#part_of) some [ReproSystem](http://x.org/root#ReproSystem)) [root]
- [Flower](http://x.org/po#Flower) SubClassOf [Structure](http://x.org/po#Structure) [po]
- [Flower](http://x.org/po#Flower) SubClassOf [part_of](http://x.org/po#part_of) some [ReproSystem](http://x.org/root#ReproSystem) [root]
- [Organ](http://x.org/po#Organ) SubClassOf [Structure](http://x.org/po#Structure) [po]
- [Tepal](http://x.org/po#Tepal) SubClassOf [Perianth](http://x.org/po#Perianth) [po]



# Ontologies used: 
- root (http://x.org/root)
- po (http://x.org/po)
");
}

/// A class unsatisfiable through a nominal and a same- or different-individual
/// axiom gets its justification: the module the search runs over holds every
/// same- and different-individual axiom naming an individual of its signature.
/// ROBOT 1.9.11 explains all nine classes of `explain-individuals.ofn`.
#[test]
fn explain_justifies_an_unsatisfiability_that_needs_same_or_different_individuals() {
    let run = bin()
        .args(["explain", "-r", "hermit", "-i"])
        .arg(robot_fixture("explain-individuals.ofn"))
        .args(["-M", "unsatisfiability", "-u", "all"])
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let report = String::from_utf8_lossy(&run.stdout);
    for n in 1..=9 {
        let line = format!(
            "1 justification(s) for http://example.org/explain-individuals#A{n} ⊑ http://www.w3.org/2002/07/owl#Nothing:"
        );
        assert!(report.contains(&line), "A{n}:\n{report}");
    }
}

/// Asking elk, emr, hermit or jfact for the unsatisfiable classes of an
/// ontology it finds inconsistent is an error, "Inconsistent ontology"; whelk
/// answers with what it derived, and the told hierarchy finds no unsatisfiable
/// class. As ROBOT 1.9.11 runs `explain -M unsatisfiability -u all` over
/// `explain-inconsistent.ofn`.
#[test]
fn explain_refuses_unsatisfiability_where_the_reasoner_finds_the_ontology_inconsistent() {
    for (r, refuses) in
        [("elk", true), ("emr", true), ("hermit", true), ("jfact", true), ("whelk", false), ("structural", false)]
    {
        let md = tmp(&format!("explain-inconsistent.{r}.md"));
        let _ = std::fs::remove_file(&md);
        let run = bin()
            .args(["explain", "-r", r, "-i"])
            .arg(robot_fixture("explain-inconsistent.ofn"))
            .args(["-M", "unsatisfiability", "-u", "all", "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        if refuses {
            assert_eq!(run.status.code(), Some(1), "{r}: {stderr}");
            assert!(stderr.contains("Inconsistent ontology"), "{r}: {stderr}");
            assert!(!md.exists(), "{r}");
        } else {
            assert!(run.status.success(), "{r}: {stderr}");
            assert!(md.exists(), "{r}");
        }
        if r == "structural" {
            assert_eq!(std::fs::read_to_string(&md).unwrap(), "No explanations found.");
        }
        let _ = std::fs::remove_file(&md);
    }
}


/// The markdown report writes data restrictions, data ranges, literals, and the
/// data property, property-set and individual axioms in Manchester syntax, a
/// self restriction with a space after `Self`, and orders explanations, their
/// axioms and the impact list by the hashes and the order of those
/// expressions. The expected reports are ROBOT 1.9.11's for the same commands.
#[test]
fn explain_writes_data_expressions_and_individual_axioms_in_the_markdown_report() {
    for name in ["explain-data", "explain-data-facets", "explain-literals", "explain-individuals", "explain-self"] {
        let md = tmp(&format!("{name}.md"));
        let run = bin()
            .args(["explain", "-r", "hermit", "-i"])
            .arg(robot_fixture(&format!("{name}.ofn")))
            .args(["-M", "unsatisfiability", "-u", "all", "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&md).unwrap(), fixture_text(&format!("{name}.robot.md")), "{name}");
        let _ = std::fs::remove_file(&md);
    }
}

/// A justification axiom that is the entailment itself stands at the root of
/// its explanation, so the tree under the root leaves it out; the axiom impact
/// summary still counts it. As ROBOT 1.9.11 writes the report for an asserted
/// subsumption and, under whelk, for a class asserted unsatisfiable.
#[test]
fn explain_leaves_the_entailment_itself_out_of_its_explanation_tree() {
    for r in ["elk", "hermit"] {
        let md = tmp(&format!("explain-asserted.{r}.md"));
        let run = bin()
            .args(["explain", "-r", r, "-i"])
            .arg(robot_fixture("explain-asserted.ofn"))
            .args(["--prefix", "ex: http://example.org/explain-asserted#", "--axiom", "ex:A SubClassOf ex:B"])
            .args(["-m", "2", "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(run.status.success(), "{r}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&md).unwrap(), fixture_text("explain-asserted.robot.md"), "{r}");
        let _ = std::fs::remove_file(&md);
    }
    let md = tmp("explain-inconsistent.whelk-tree.md");
    let run = bin()
        .args(["explain", "-r", "whelk", "-i"])
        .arg(robot_fixture("explain-inconsistent.ofn"))
        .args(["-M", "unsatisfiability", "-u", "all", "--explanation"])
        .arg(&md)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(std::fs::read_to_string(&md).unwrap(), fixture_text("explain-inconsistent.whelk.robot.md"));
    let _ = std::fs::remove_file(&md);
}

/// `--axiom` reads its axiom as ROBOT reads it. A class goes by the short form
/// of its IRI and by each label it has, and `X_1`, one class's short form and
/// another's label, is the name of the class later in the order a hash set of
/// the ontology's entities iterates in; a name may be quoted with `'` or `"`,
/// and a CURIE of the built-in context or a bare IRI names its class too. The
/// keyword takes any case and its colon, a class may stand in parentheses, and
/// `#` starts a comment. An axiom the ontology states without annotations is
/// its own justification, a tautology and an axiom of an inconsistent
/// ontology among them; one the ontology does not entail, or about a class it
/// does not have, has no explanation. A class with no NCName at the end of its
/// IRI is written by what follows its last `/`, or by the whole IRI in angle
/// brackets. The expected reports are ROBOT 1.9.11's.
#[test]
fn explain_reads_the_axiom_as_robot_reads_it() {
    let none = "No explanations found.";
    for (i, (fixture, axiom, expected)) in [
        ("explain-names.ofn", "X_1 SubClassOf X_4", "explain-names.X_3-X_4.robot.md"),
        ("explain-names.ofn", "X_3 SubClassOf X_4", "explain-names.X_3-X_4.robot.md"),
        ("explain-names.ofn", "'X_1' SubClassOf X_4", "explain-names.X_3-X_4.robot.md"),
        ("explain-names.ofn", "X_1 subclassof X_4", "explain-names.X_3-X_4.robot.md"),
        ("explain-names.ofn", "X_1 SUBCLASSOF: X_4", "explain-names.X_3-X_4.robot.md"),
        ("explain-names.ofn", "(X_1) SubClassOf X_4", "explain-names.X_3-X_4.robot.md"),
        ("explain-names.ofn", "X_1 SubClassOf ((X_4))", "explain-names.X_3-X_4.robot.md"),
        ("explain-names.ofn", "X_1 SubClassOf X_4 # comment", "explain-names.X_3-X_4.robot.md"),
        ("explain-names.ofn", "'alpha one' SubClassOf beta", "explain-names.X_1-X_2.robot.md"),
        ("explain-names.ofn", "\"alpha one\" SubClassOf beta", "explain-names.X_1-X_2.robot.md"),
        ("explain-names.ofn", "' alpha one ' SubClassOf beta", "explain-names.X_1-X_2.robot.md"),
        ("explain-names.ofn", "obo:X_1 SubClassOf obo:X_4", "explain-names.X_1-X_4.robot.md"),
        ("explain-names.ofn", "'alpha one' SubClassOf 'X_4'", "explain-names.X_1-X_4.robot.md"),
        ("explain-names.ofn", "http://purl.obolibrary.org/obo/X_1 SubClassOf X_4", "explain-names.X_1-X_4.robot.md"),
        ("materialize-reasoners.reason-structural.robot.ofn", "'A' SubClassOf 'Thing'", "explain-stated.A-Thing.robot.md"),
        ("reduce-inconsistent.ofn", "'A' SubClassOf 'B'", "explain-stated.inconsistent-A-B.robot.md"),
        ("explain-names.ofn", "X_2 SubClassOf X_1", none),
        ("explain-names.ofn", "obo:X_99 SubClassOf X_4", none),
        ("explain-names.ofn", "GO SubClassOf X_4", none),
        ("explain-short-forms.ofn", "'a#1' SubClassOf 'd#'", "explain-short-forms.a1-d.robot.md"),
        ("explain-short-forms.ofn", "<http://example.org/b/> SubClassOf eff", "explain-short-forms.b-f.robot.md"),
    ]
    .into_iter()
    .enumerate()
    {
        let md = tmp(&format!("explain-axiom-{i}.md"));
        let out = bin()
            .args(["explain", "-i"])
            .arg(robot_fixture(fixture))
            .args(["--axiom", axiom, "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(out.status.success(), "{axiom}: {}", String::from_utf8_lossy(&out.stderr));
        let want = if expected == none { none.to_string() } else { fixture_text(expected) };
        assert_eq!(std::fs::read_to_string(&md).unwrap(), want, "{axiom}");
        let _ = std::fs::remove_file(&md);
    }
    // Text that names no class where one is wanted, or goes on after the
    // superclass, is no axiom.
    for (i, axiom) in [
        "<http://purl.obolibrary.org/obo/X_1> SubClassOf X_4",
        "alpha SubClassOf beta",
        "X_1SubClassOf X_4",
        "X_1 SubClassOf X_99",
        "X_1 SubClassOf <abc",
        "X_1 SubClassOf X_4 garbage",
        "'alpha one' SubClassOf beta)",
    ]
    .into_iter()
    .enumerate()
    {
        let md = tmp(&format!("explain-axiom-refused-{i}.md"));
        let out = bin()
            .args(["explain", "-i"])
            .arg(robot_fixture("explain-names.ofn"))
            .args(["--axiom", axiom, "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{axiom} was read");
        assert!(!md.exists(), "{axiom} wrote a report");
    }
}

/// `-M inconsistency` explains `owl:Thing ⊑ owl:Nothing` when the reasoner
/// finds the ontology inconsistent, with a justification drawn from all of its
/// logical axioms: here through property sets, negative assertions, sameness
/// and difference of individuals, a data range and an anonymous individual,
/// which the report writes by its node ID. The expected reports are ROBOT
/// 1.9.11's for the same commands.
#[test]
fn explain_justifies_the_inconsistency_of_an_ontology() {
    for (name, ext) in [
        ("nary", "ofn"),
        ("negdata", "ofn"),
        ("negobj", "ofn"),
        ("objnary", "ofn"),
        ("range", "ofn"),
        ("same", "ofn"),
        ("same2", "ofn"),
        ("anon", "owl"),
    ] {
        let md = tmp(&format!("explain-inc-{name}.md"));
        let run = bin()
            .args(["explain", "-r", "hermit", "-i"])
            .arg(robot_fixture(&format!("explain-inc-{name}.{ext}")))
            .args(["-M", "inconsistency", "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&md).unwrap(),
            fixture_text(&format!("explain-inc-{name}.robot.md")),
            "{name}"
        );
        let _ = std::fs::remove_file(&md);
    }
}

/// Under whelk every unsatisfiable class is justified, the classes made
/// unsatisfiable through an existential restriction among them, and the same
/// report comes out of every run. The expected report is ROBOT 1.9.11's.
#[test]
fn explain_justifies_every_unsatisfiable_class_under_whelk() {
    for run in 0..5 {
        let md = tmp(&format!("explain-unsat-graph-whelk-{run}.md"));
        let out = bin()
            .args(["explain", "-r", "whelk", "-i"])
            .arg(robot_fixture("explain-unsat-graph.ofn"))
            .args(["-M", "unsatisfiability", "-u", "all", "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            std::fs::read_to_string(&md).unwrap(),
            fixture_text("explain-unsat-graph.whelk.robot.md"),
            "run {run}"
        );
        let _ = std::fs::remove_file(&md);
    }
}

/// `owl:Thing` is justified unsatisfiable through axioms none of which is
/// about it: the search takes in the axioms that refer to the entailment's
/// own classes, here an unqualified cardinality whose filler is `owl:Thing`,
/// and grows from them to a reflexive property's range. The expected report
/// is ROBOT 1.9.11's.
#[test]
fn explain_justifies_an_unsatisfiable_owl_thing() {
    let md = tmp("explain-unsat-top.whelk.md");
    let out = bin()
        .args(["explain", "-r", "whelk", "-i"])
        .arg(robot_fixture("explain-unsat-top.ofn"))
        .args(["-M", "unsatisfiability", "-u", "all", "--explanation"])
        .arg(&md)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&md).unwrap(), fixture_text("explain-unsat-top.whelk.robot.md"));
    let _ = std::fs::remove_file(&md);
}

/// `explain -M unsatisfiability` explains the classes `--unsatisfiable`
/// selects as ROBOT selects them: `root`, the classes no other unsatisfiable
/// class's told definition explains, those of a cycle of dependencies among
/// them; `most_general`, those with no unsatisfiable told superclass;
/// `random:2`, the first two by IRI; and, without the option, none. The
/// expected reports are ROBOT 1.9.11's.
#[test]
fn explain_selects_the_unsatisfiable_classes_robot_selects() {
    for (fixture, reasoner, selector, expected) in [
        ("explain-unsat-graph2.ofn", "hermit", Some("root"), "explain-unsat-graph2.hermit-root.robot.md"),
        ("explain-unsat-graph2.ofn", "hermit", Some("random:2"), "explain-unsat-graph2.hermit-random2.robot.md"),
        ("explain-unsat-graph2.ofn", "elk", Some("most_general"), "explain-unsat-graph2.elk-most_general.robot.md"),
        ("explain-unsat-cycle.ofn", "hermit", Some("root"), "explain-unsat-cycle.hermit-root.robot.md"),
        ("explain-unsat-graph2.ofn", "hermit", None, ""),
        ("explain-unsat-incons.ofn", "elk", None, ""),
    ] {
        let md = tmp(&format!("{fixture}.{reasoner}.{}.md", selector.unwrap_or("none").replace(':', "_")));
        let mut cmd = bin();
        cmd.args(["explain", "-r", reasoner, "-i"]).arg(robot_fixture(fixture)).args(["-M", "unsatisfiability"]);
        if let Some(s) = selector {
            cmd.args(["-u", s]);
        }
        let out = cmd.arg("--explanation").arg(&md).output().unwrap();
        assert!(out.status.success(), "{fixture} {reasoner} {selector:?}: {}", String::from_utf8_lossy(&out.stderr));
        let want = if expected.is_empty() { "No explanations found.".to_string() } else { fixture_text(expected) };
        assert_eq!(std::fs::read_to_string(&md).unwrap(), want, "{fixture} {reasoner} {selector:?}");
        let _ = std::fs::remove_file(&md);
    }
}

/// `--unsatisfiable list` explains nothing and writes each unsatisfiable
/// class's CURIE in the built-in OBO context, or its IRI where no prefix
/// fits, sorted, a line each. The expected list is ROBOT 1.9.11's.
#[test]
fn explain_lists_the_unsatisfiable_classes_as_curies() {
    let txt = tmp("explain-unsat-graph2.list.txt");
    let out = bin()
        .args(["explain", "-r", "hermit", "-i"])
        .arg(robot_fixture("explain-unsat-graph2.ofn"))
        .args(["-M", "unsatisfiability", "-u", "list", "--explanation"])
        .arg(&txt)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&txt).unwrap(), fixture_text("explain-unsat-graph2.hermit-list.robot.txt"));
    let _ = std::fs::remove_file(&txt);
}

/// Where `--unsatisfiable` cannot be followed the command fails, as ROBOT's
/// does: `most_general` over a class told to be below owl:Nothing, whose
/// climb through its superclasses never ends; `all` over an ontology the
/// reasoner finds inconsistent; and a value that is no keyword, no
/// `random:` with an integer, and no class.
#[test]
fn explain_fails_where_the_unsatisfiable_selection_cannot_be_made() {
    for (fixture, reasoner, selector, message) in [
        ("explain-unsat-graph.ofn", "hermit", "most_general", "never ends"),
        ("explain-unsat-incons.ofn", "elk", "all", "Inconsistent ontology"),
        ("explain-unsat-graph2.ofn", "hermit", "ALL", "ILLEGAL UNSATISFIABLE ARGUMENT ERROR: ALL."),
        ("explain-unsat-graph2.ofn", "hermit", "random:x", "ILLEGAL UNSATISFIABLE ARGUMENT ERROR: random:x."),
    ] {
        let md = tmp(&format!("explain-unsat-fails.{reasoner}.md"));
        let out = bin()
            .args(["explain", "-r", reasoner, "-i"])
            .arg(robot_fixture(fixture))
            .args(["-M", "unsatisfiability", "-u", selector, "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{fixture} {selector} succeeded");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(message), "{fixture} {selector}: {err}");
        assert!(!md.exists(), "{fixture} {selector} wrote a report");
    }
}

/// An axiom annotated with an anonymous individual hashes that individual by
/// its node ID, and the hash places the axiom in the search for a
/// justification. The expected reports are ROBOT 1.9.11's, for the functional
/// and the RDF/XML document, whose node IDs differ.
#[test]
fn explain_places_axioms_annotated_with_anonymous_individuals_by_their_node_ids() {
    for ext in ["ofn", "owl"] {
        for reasoner in ["elk", "hermit"] {
            let md = tmp(&format!("rdf-nested-anonymous.{ext}.{reasoner}.md"));
            let run = bin()
                .args(["explain", "-r", reasoner, "-i"])
                .arg(robot_fixture(&format!("rdf-nested-anonymous.{ext}")))
                .args(["-M", "inconsistency", "--explanation"])
                .arg(&md)
                .output()
                .unwrap();
            assert!(run.status.success(), "{ext} {reasoner}: {}", String::from_utf8_lossy(&run.stderr));
            assert_eq!(
                std::fs::read_to_string(&md).unwrap(),
                fixture_text(&format!("rdf-nested-anonymous.{ext}.{reasoner}.robot.md")),
                "{ext} {reasoner}"
            );
            let _ = std::fs::remove_file(&md);
        }
    }
}

/// The markdown report writes a disjoint union, a key and a SWRL rule as
/// ROBOT's renderer does: the union's members and the key's property lists
/// sorted, the key's object and data properties run together, and the rule's
/// atoms in the order it lists them, a complex class atom in parentheses. The
/// expected reports are ROBOT 1.9.11's; the key's place in its explanation
/// follows the key's hash, which leaves its annotations out.
#[test]
fn explain_writes_disjoint_unions_keys_and_rules_in_the_markdown_report() {
    for name in ["union", "key", "rule"] {
        let md = tmp(&format!("explain-inc-{name}.md"));
        let run = bin()
            .args(["explain", "-r", "hermit", "-i"])
            .arg(robot_fixture(&format!("explain-inc-{name}.ofn")))
            .args(["-M", "inconsistency", "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&md).unwrap(),
            fixture_text(&format!("explain-inc-{name}.robot.md")),
            "{name}"
        );
        let _ = std::fs::remove_file(&md);
    }
}

/// Further justifications of an inconsistency come from the hitting-set tree,
/// whose order of branches, reuse of justifications and order of the search's
/// axioms decide which ones `--max` admits and how each is written. The
/// expected reports are ROBOT 1.9.11's, identical over at least six runs each;
/// in `explain-inc-reuse` (a generated probe) which justification a branch
/// reuses decides the report.
#[test]
fn explain_finds_further_justifications_of_an_inconsistency() {
    for (name, m) in [
        ("explain-inc-multi", "1"),
        ("explain-inc-multi", "2"),
        ("explain-inc-multi", "3"),
        ("explain-inc-multi", "10"),
        ("explain-inc-reuse", "10"),
    ] {
        let md = tmp(&format!("{name}.m{m}.md"));
        let run = bin()
            .args(["explain", "-r", "hermit", "-i"])
            .arg(robot_fixture(&format!("{name}.ofn")))
            .args(["-M", "inconsistency", "-m", m, "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(run.status.success(), "{name} -m {m}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&md).unwrap(),
            fixture_text(&format!("{name}.m{m}.robot.md")),
            "{name} -m {m}"
        );
        let _ = std::fs::remove_file(&md);
    }
}

/// An ontology the reasoner finds consistent has no inconsistency to explain,
/// and the structural reasoner finds every ontology consistent.
#[test]
fn explain_finds_nothing_to_explain_in_a_consistent_ontology() {
    for (r, input) in [
        ("hermit", "explain-inc-consistent.ofn"),
        ("elk", "explain-inc-consistent.ofn"),
        ("whelk", "explain-inc-consistent.ofn"),
        ("structural", "explain-inc-multi.ofn"),
    ] {
        let md = tmp(&format!("explain-inc-none.{r}.md"));
        let run = bin()
            .args(["explain", "-r", r, "-i"])
            .arg(robot_fixture(input))
            .args(["-M", "inconsistency", "--explanation"])
            .arg(&md)
            .output()
            .unwrap();
        assert!(run.status.success(), "{r}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(std::fs::read_to_string(&md).unwrap(), "No explanations found.", "{r}");
        let _ = std::fs::remove_file(&md);
    }
}


/// Under `--use-graphs` the root and the ontology it imports are named graphs,
/// and the query's default graph is their union: a pattern is answered graph by
/// graph, in the order the union holds the graphs, and a triple both graphs
/// assert is counted where it is first found. The expected rows are ROBOT
/// 1.9.10's (three runs, identical).
#[test]
fn query_use_graphs_answers_graph_by_graph() {
    let dir = tmp("usegraphs");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("root.ofn"), "Prefix(:=<http://x.org/root#>)
Prefix(o:=<http://www.geneontology.org/formats/oboInOwl#>)
Ontology(<http://x.org/root>
Import(<http://x.org/imp>)
Declaration(Class(:L))
Declaration(Class(:M))
Declaration(Class(:C))
Declaration(Class(:D))
Declaration(AnnotationProperty(o:hasDbXref))
SubClassOf(:C :L)
SubClassOf(:D :M)
AnnotationAssertion(o:hasDbXref :L \"R:L1\")
AnnotationAssertion(o:hasDbXref :L \"R:L2\")
AnnotationAssertion(o:hasDbXref :C \"R:C1\")
AnnotationAssertion(o:hasDbXref :C \"R:C2\")
AnnotationAssertion(o:hasDbXref :D \"R:D1\")
)
").unwrap();
    std::fs::write(dir.join("imp.ofn"), "Prefix(:=<http://x.org/root#>)
Prefix(o:=<http://www.geneontology.org/formats/oboInOwl#>)
Ontology(<http://x.org/imp>
Declaration(AnnotationProperty(o:hasDbXref))
AnnotationAssertion(o:hasDbXref :L \"I:L1\")
AnnotationAssertion(o:hasDbXref :C \"I:C1\")
AnnotationAssertion(o:hasDbXref :C \"R:C2\")
AnnotationAssertion(o:hasDbXref :D \"I:D1\")
)
").unwrap();
    std::fs::write(
        dir.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         <uri name=\"http://x.org/imp\" uri=\"imp.ofn\"/>\n\
         </catalog>\n",
    )
    .unwrap();
    std::fs::write(dir.join("q.sparql"), "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX oboInOwl: <http://www.geneontology.org/formats/oboInOwl#>
SELECT DISTINCT ?xref WHERE {
  { ?sub rdfs:subClassOf* <http://x.org/root#L> . }
  UNION
  { ?sub rdfs:subClassOf* <http://x.org/root#M> . }
  ?sub oboInOwl:hasDbXref ?xref .
}
").unwrap();
    let status = bin()
        .current_dir(&dir)
        .args(["--catalog", "catalog-v001.xml", "query", "-i", "root.ofn", "--use-graphs", "true", "--query", "q.sparql", "out.tsv"])
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(dir.join("out.tsv")).unwrap(), "?xref
\"I:L1\"
\"R:L2\"
\"R:L1\"
\"R:C2\"
\"I:C1\"
\"R:C1\"
\"I:D1\"
\"R:D1\"
");
}

/// `reason` lists unsatisfiable classes in the order the reasoner's bottom node
/// iterates them. The node is a concurrent hash table keyed on each IRI's string
/// hash and filled in the order the classes were queued, and the listing copies
/// it through three hash sets of classes. `bottom-order.ofn` has 263
/// unsatisfiable classes, 159 of them in groups that collide in both tables, so
/// the listing turns on the queue order and on how the table's doublings
/// reorder its chains. The expected listing is ROBOT 1.9.10's (ODK v1.6.1) on
/// one CPU, the same three times over.
#[test]
fn reason_lists_unsatisfiable_classes_in_bottom_node_order() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/elk-order");
    let run = bin()
        .args(["reason", "-r", "ELK", "-i"])
        .arg(fixtures.join("bottom-order.ofn"))
        .output()
        .unwrap();
    assert!(!run.status.success());
    let listed = |text: &str| -> Vec<String> {
        text.lines()
            .filter_map(|l| l.split_once("unsatisfiable: ").map(|(_, iri)| iri.trim().to_string()))
            .collect()
    };
    let ours = listed(&String::from_utf8_lossy(&run.stdout));
    let robot = listed(&std::fs::read_to_string(fixtures.join("bottom-order.robot.txt")).unwrap());
    assert_eq!(robot.len(), 263);
    assert_eq!(ours, robot);
}

/// `owltools --export-parents` lists a cell's parents in the order of the sets
/// they pass through, the first being every superclass of the row's class with
/// owl:Thing among them. X:0000900 has twelve superclasses besides owl:Thing,
/// so owl:Thing takes that set from 16 buckets to 32; the two parents' classes
/// share a bucket at 16 and at every later step, and at 32 the second whole
/// comes first. The expected table is owltools 2020-04-06's (ODK v1.6.1), the
/// same three times over.
#[test]
fn export_parents_sizes_the_superclass_set_with_owl_thing() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/export-parents");
    let out = tmp("thing-sizes.tsv");
    let run = bin()
        .arg("owltools")
        .arg(fixtures.join("thing-sizes.ofn"))
        .args(["--reasoner", "mexr", "--export-parents", "-p", "BFO:0000050", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        std::fs::read_to_string(fixtures.join("thing-sizes.owltools.tsv")).unwrap()
    );
}

/// A class frame that states one xref twice — as a plain literal from an
/// imported mapping, with provenance, and as `xsd:string` from the OBO edit file
/// — writes it once, where the first of the two sorts. The frame's order is a
/// red-black tree's listing sorted by a run-merging sort. Under the frame's
/// comparison each of the two copies follows the other, so which comes first
/// turns on the frame's other axioms. With the full stanza
/// (`twin.obo`) the MESH xref stands in its alphabetical place after FMA; cut
/// down to its xrefs (`twin-cut.obo`) it stands first. The expected documents
/// are ROBOT 1.9.10's (ODK v1.6.1), the same three times over each.
#[test]
fn a_twice_stated_xref_stands_where_the_frame_sorts_its_first_copy() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xref-twins");
    for variant in ["twin", "twin-cut"] {
        let out = tmp(&format!("{variant}.owl"));
        let run = bin()
            .arg("merge")
            .arg("--catalog")
            .arg(fixtures.join("catalog-v001.xml"))
            .arg("-i")
            .arg(fixtures.join(format!("{variant}.obo")))
            .args(["expand", "--no-expand-term", "http://purl.obolibrary.org/obo/RO_0002175", "-o"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            std::fs::read_to_string(fixtures.join(format!("{variant}.robot.owl"))).unwrap(),
            "{variant}"
        );
    }
}
