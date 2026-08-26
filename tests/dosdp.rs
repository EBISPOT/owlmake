//! DOSDP pattern generation tests.

use std::collections::HashMap;

use horned_owl::model::{ClassExpression as CE, Component};

use owlmake::dosdp;

const PATTERN: &str = r#"
pattern_name: part_of_x
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
"#;

const DATA: &str = "defined_class\tpart\nCL:1000001\tUBERON:0000955\n";

#[test]
fn dosdp_generates_logical_definition() {
    let labels = HashMap::new();
    let model = dosdp::generate(PATTERN, DATA, &labels).unwrap();

    // The generated EquivalentClasses must be `dc ≡ cell ⊓ ∃part_of.filler`.
    let dc = "http://purl.obolibrary.org/obo/CL_1000001";
    let cell = "http://purl.obolibrary.org/obo/CL_0000000";
    let part_of = "http://purl.obolibrary.org/obo/BFO_0000050";
    let filler = "http://purl.obolibrary.org/obo/UBERON_0000955";

    let mut found = false;
    for ac in model.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            let names: Vec<String> = eq
                .0
                .iter()
                .filter_map(|c| match c {
                    CE::Class(cl) => Some(cl.0.as_ref().to_string()),
                    _ => None,
                })
                .collect();
            if !names.contains(&dc.to_string()) {
                continue;
            }
            // The other member is the intersection.
            for member in &eq.0 {
                if let CE::ObjectIntersectionOf(parts) = member {
                    let has_cell = parts.iter().any(|p| matches!(p, CE::Class(c) if c.0.as_ref() == cell));
                    let has_existential = parts.iter().any(|p| matches!(p,
                        CE::ObjectSomeValuesFrom { ope, bce }
                        if matches!(ope, horned_owl::model::ObjectPropertyExpression::ObjectProperty(r) if r.0.as_ref() == part_of)
                        && matches!(bce.as_ref(), CE::Class(c) if c.0.as_ref() == filler)));
                    if has_cell && has_existential {
                        found = true;
                    }
                }
            }
        }
    }
    assert!(found, "expected dc ≡ cell ⊓ ∃part_of.filler");

    // And a label annotation was generated.
    let has_label = model.ont.iter().any(|ac| matches!(&ac.component,
        Component::AnnotationAssertion(aa)
            if aa.ann.ap.0.as_ref() == "http://www.w3.org/2000/01/rdf-schema#label"));
    assert!(has_label, "expected a generated rdfs:label");
}

/// Exercises the richer DOSDP surface: `objectProperties` substitution, the
/// `logical_axioms` list with `disjointWith`, `list_vars` → union, and the
/// `multi_clause` over a list var in a LOGICAL axiom conjoins the per-item
/// clauses into an `ObjectIntersectionOf` — a list in a logical context is a
/// conjunction, `(has_part some U1) and (has_part some U2)` — plus
/// `disjointWith`. `multi_clause` is the idiom DOSDP defines for list expansion;
/// a list var used via a bare `%s` is checked separately below.
#[test]
fn dosdp_rich_pattern() {
    const PAT: &str = r#"
pattern_name: rich
classes:
  cell: CL:0000000
objectProperties:
  has_part: BFO:0000051
list_vars:
  parts: "'cell'"
logical_axioms:
  - axiom_type: equivalentTo
    multi_clause:
      sep: " and "
      clauses:
        - text: "'has_part' some %s"
          vars: [parts]
  - axiom_type: disjointWith
    text: "'cell'"
"#;
    const D: &str = "defined_class\tparts\nCL:9\tUBERON:1|UBERON:2\n";
    let model = dosdp::generate(PAT, D, &HashMap::new()).unwrap();

    let has_part = "http://purl.obolibrary.org/obo/BFO_0000051";
    // equivalentTo with a multi_clause list var → intersection of two existentials.
    let intersection_ok = model.ont.iter().any(|ac| match &ac.component {
        Component::EquivalentClasses(eq) => eq.0.iter().any(|m| match m {
            CE::ObjectIntersectionOf(parts) => {
                let svfs = parts.iter().filter(|p| matches!(p,
                    CE::ObjectSomeValuesFrom { ope, .. }
                    if matches!(ope, horned_owl::model::ObjectPropertyExpression::ObjectProperty(r) if r.0.as_ref() == has_part))).count();
                svfs == 2
            }
            _ => false,
        }),
        _ => false,
    });
    assert!(intersection_ok, "expected equivalentTo = (has_part some U1) and (has_part some U2)");

    assert!(
        model.ont.iter().any(|ac| matches!(&ac.component, Component::DisjointClasses(_))),
        "expected a DisjointClasses axiom from logical_axioms disjointWith"
    );
}

/// A list variable used via a bare `%s` (NOT a `multi_clause`) yields no axiom:
/// the template is dropped, because list expansion has to go through
/// `multi_clause`. In particular the items are neither unioned nor joined.
#[test]
fn dosdp_bare_pct_list_var_yields_nothing() {
    const PAT: &str = r#"
pattern_name: bare
classes: {cell: CL:0000000}
relations: {has_part: BFO:0000051}
annotationProperties: {syn: oboInOwl:hasExactSynonym}
list_vars: {parts: "'cell'"}
annotations:
  - {annotationProperty: syn, text: "%s-cell", vars: [parts]}
equivalentTo:
  text: "'cell' and ('has_part' some %s)"
  vars: [parts]
"#;
    let m = dosdp::generate(PAT, "defined_class\tparts\nCL:9\tUBERON:1|UBERON:2\n", &HashMap::new()).unwrap();
    assert!(!m.ont.iter().any(|ac| matches!(&ac.component, Component::EquivalentClasses(_))),
        "bare-%s list var must not produce a logical axiom");
    assert!(!m.ont.iter().any(|ac| matches!(&ac.component, Component::AnnotationAssertion(_))),
        "bare-%s list var must not produce an annotation");
}

/// `terms`, `prototype`, and `query` (round-trip: generate then query) on the
/// top-of-file PATTERN.
#[test]
fn dosdp_terms_prototype_query() {
    // terms: dictionary IRIs + defined class + filler.
    let terms = dosdp::terms(PATTERN, DATA).unwrap();
    assert!(terms.iter().any(|t| t == "http://purl.obolibrary.org/obo/BFO_0000050"));
    assert!(terms.iter().any(|t| t == "http://purl.obolibrary.org/obo/CL_1000001"));
    assert!(terms.iter().any(|t| t == "http://purl.obolibrary.org/obo/UBERON_0000955"));

    // prototype: a defined class with the equivalentTo filled by the var's range.
    let proto = dosdp::prototype(PATTERN, &HashMap::new()).unwrap();
    assert!(proto.ont.iter().any(|ac| matches!(&ac.component, Component::EquivalentClasses(_))));

    // query: generate the def into a model, then recover the bindings.
    let model = dosdp::generate(PATTERN, DATA, &HashMap::new()).unwrap();
    let res = dosdp::query(PATTERN, &model).unwrap();
    assert_eq!(res.columns, vec!["defined_class".to_string(), "part".to_string()]);
    assert!(
        res.rows.iter().any(|r| r[0] == "http://purl.obolibrary.org/obo/CL_1000001"
            && r[1] == "http://purl.obolibrary.org/obo/UBERON_0000955"),
        "expected query to bind dc=CL_1000001, part=UBERON_0000955; got {:?}",
        res.rows
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Feature coverage for the rest of `dosdp`: subClassOf / GCI / data_vars /
// substitutions / internal_vars / instance_graph / generated_synonyms /
// multi_clause / annotations(var,value) / def / document. Each test pins the
// axioms a pattern must generate for a given TSV row.
// ───────────────────────────────────────────────────────────────────────────

use horned_owl::model::AnnotationValue;

/// Helper: does any AnnotationAssertion on `subj` use property `prop` with the
/// given literal value?
fn has_literal_ann(model: &owlmake::model::Model, prop: &str, lit: &str) -> bool {
    model.ont.iter().any(|ac| matches!(&ac.component,
        Component::AnnotationAssertion(aa)
            if aa.ann.ap.0.as_ref() == prop
            && matches!(&aa.ann.av, AnnotationValue::Literal(l) if l.literal() == lit)))
}

/// `subClassOf` → `SubClassOf(defined_class, CE)`.
#[test]
fn dosdp_subclass_of_axiom() {
    const P: &str = r#"
pattern_name: sc
classes: {cell: CL:0000000}
relations: {capable_of: RO:0002215}
vars: {proc: "'cell'"}
subClassOf:
  text: "'capable_of' some %s"
  vars: [proc]
"#;
    let m = dosdp::generate(P, "defined_class\tproc\nT:1\tGO:0008150\n", &HashMap::new()).unwrap();
    let ok = m.ont.iter().any(|ac| match &ac.component {
        Component::SubClassOf(sc) => matches!(&sc.sub, CE::Class(c) if c.0.as_ref().ends_with("T_1"))
            && matches!(&sc.sup, CE::ObjectSomeValuesFrom { .. }),
        _ => false,
    });
    assert!(ok, "expected SubClassOf(T_1, capable_of some GO:0008150)");
}

/// `GCI` splits its text on the Manchester keyword into a general inclusion
/// between two arbitrary expressions (not anchored on the defined class).
#[test]
fn dosdp_gci_axiom() {
    const P: &str = r#"
pattern_name: gci
classes: {cell: CL:0000000, tissue: UBERON:0000479}
relations: {part_of: BFO:0000050}
vars: {part: "'tissue'"}
GCI:
  text: "'cell' and ('part_of' some %s) SubClassOf 'tissue'"
  vars: [part]
"#;
    let m = dosdp::generate(P, "defined_class\tpart\nT:1\tUBERON:0000479\n", &HashMap::new()).unwrap();
    // A SubClassOf whose subject is an intersection (the GCI lhs), not a named class.
    let ok = m.ont.iter().any(|ac| matches!(&ac.component,
        Component::SubClassOf(sc) if matches!(&sc.sub, CE::ObjectIntersectionOf(_))));
    assert!(ok, "expected a GCI SubClassOf with an intersection subject");
}

/// `data_vars` values are substituted into text templates as literal text.
#[test]
fn dosdp_data_var_literal() {
    const P: &str = r#"
pattern_name: dv
classes: {cell: CL:0000000}
annotationProperties: {label: rdfs:label}
data_vars: {n: "xsd:string"}
name:
  text: "cell number %s"
  vars: [n]
subClassOf: {text: "'cell'"}
"#;
    let m = dosdp::generate(P, "defined_class\tn\nT:1\t42\n", &HashMap::new()).unwrap();
    assert!(has_literal_ann(&m, "http://www.w3.org/2000/01/rdf-schema#label", "cell number 42"),
        "expected label literal using the data var verbatim");
}

/// `substitutions` (regex over one input) and `internal_vars` `join` derive new
/// variables usable in templates.
#[test]
fn dosdp_substitutions_and_internal_join() {
    const P: &str = r#"
pattern_name: sub
classes: {cell: CL:0000000}
annotationProperties: {label: rdfs:label}
data_vars: {raw: "xsd:string"}
substitutions:
  - {in: raw, out: upper, match: "(.*)", sub: "X-$1"}
internal_vars:
  - var: joined
    join: {sep: "/", vars: [raw, upper]}
name:
  text: "%s | %s"
  vars: [upper, joined]
subClassOf: {text: "'cell'"}
"#;
    let m = dosdp::generate(P, "defined_class\traw\nT:1\tfoo\n", &HashMap::new()).unwrap();
    assert!(has_literal_ann(&m, "http://www.w3.org/2000/01/rdf-schema#label", "X-foo | foo/X-foo"),
        "expected substitution (X-foo) and join (foo/X-foo)");
}

/// `instance_graph`: nodes → typed named individuals, edges → object-property
/// assertions between them.
#[test]
fn dosdp_instance_graph() {
    const P: &str = r#"
pattern_name: ig
classes: {cell: CL:0000000, neuron: CL:0000540}
relations: {part_of: BFO:0000050}
vars: {part: "'cell'"}
instance_graph:
  nodes: {n1: cell, n2: part}
  edges:
    - [n1, part_of, n2]
subClassOf: {text: "'cell'"}
"#;
    let m = dosdp::generate(P, "defined_class\tpart\nT:1\tCL:0000540\n", &HashMap::new()).unwrap();
    let inds = m.ont.iter().filter(|ac| matches!(&ac.component, Component::DeclareNamedIndividual(_))).count();
    let casserts = m.ont.iter().filter(|ac| matches!(&ac.component, Component::ClassAssertion(_))).count();
    let opa = m.ont.iter().filter(|ac| matches!(&ac.component, Component::ObjectPropertyAssertion(_))).count();
    assert_eq!(inds, 2, "two named individuals");
    assert_eq!(casserts, 2, "two class assertions (cell, neuron-filler)");
    assert_eq!(opa, 1, "one part_of edge assertion");
}

/// `generated_synonyms`: one synonym annotation PER list item (not one joined).
#[test]
fn dosdp_generated_synonyms_one_per_item() {
    // `generated_synonyms` is processed like a regular synonym: per-item synonyms
    // come from a `multi_clause` over a list var (a bare `%s` list yields nothing).
    const P: &str = r#"
pattern_name: gs
classes: {cell: CL:0000000}
annotationProperties: {exact_synonym: oboInOwl:hasExactSynonym}
list_vars: {syns: "'cell'"}
generated_synonyms:
  - multi_clause:
      sep: " "
      clauses:
        - {text: "%s cell", vars: [syns]}
subClassOf: {text: "'cell'"}
"#;
    // labels so the synonym text uses friendly names, not IRIs.
    let mut labels = HashMap::new();
    labels.insert("http://purl.obolibrary.org/obo/CL_0000100".to_string(), "alpha".to_string());
    labels.insert("http://purl.obolibrary.org/obo/CL_0000200".to_string(), "beta".to_string());
    let m = dosdp::generate(P, "defined_class\tsyns\nT:1\tCL:0000100|CL:0000200\n", &labels).unwrap();
    let syn = "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym";
    assert!(has_literal_ann(&m, syn, "alpha cell"), "expected per-item synonym 'alpha cell'");
    assert!(has_literal_ann(&m, syn, "beta cell"), "expected per-item synonym 'beta cell'");
}

/// `generated_synonyms` over a list var via a bare `%s` (no multi_clause) yields
/// nothing — and in particular never a synonym containing a raw IRI. Generated
/// synonyms follow the same rule as any other synonym template: a bare-`%s` list
/// does not expand.
#[test]
fn dosdp_generated_synonyms_bare_list_is_empty() {
    const P: &str = r#"
pattern_name: gs2
classes: {cell: CL:0000000}
annotationProperties: {exact_synonym: oboInOwl:hasExactSynonym}
list_vars: {syns: "'cell'"}
generated_synonyms:
  - {text: "%s cell", vars: [syns]}
subClassOf: {text: "'cell'"}
"#;
    // No labels, so a template that did expand the list would emit
    // "http://…CL_0000100 cell" — a synonym with a raw IRI in it.
    let m = dosdp::generate(P, "defined_class\tsyns\nT:1\tCL:0000100|CL:0000200\n", &HashMap::new()).unwrap();
    assert!(!m.ont.iter().any(|ac| matches!(&ac.component, Component::AnnotationAssertion(_))),
        "bare-%s list generated_synonyms must emit no synonym (and never a raw-IRI one)");
}

/// `annotations` with `var` → an IRI-valued annotation (object is the filler IRI).
#[test]
fn dosdp_annotation_var_iri_valued() {
    const P: &str = r#"
pattern_name: av
classes: {cell: CL:0000000}
annotationProperties: {seeAlso: rdfs:seeAlso}
vars: {ref: "'cell'"}
annotations:
  - {annotationProperty: seeAlso, var: ref}
subClassOf: {text: "'cell'"}
"#;
    let m = dosdp::generate(P, "defined_class\tref\nT:1\tCL:0000540\n", &HashMap::new()).unwrap();
    let ok = m.ont.iter().any(|ac| matches!(&ac.component,
        Component::AnnotationAssertion(aa)
            if aa.ann.ap.0.as_ref() == "http://www.w3.org/2000/01/rdf-schema#seeAlso"
            && matches!(&aa.ann.av, AnnotationValue::IRI(i) if i.as_ref().ends_with("CL_0000540"))));
    assert!(ok, "expected IRI-valued seeAlso annotation to the filler");
}

/// `def` → an IAO:0000115 definition annotation.
#[test]
fn dosdp_def_field() {
    const P: &str = r#"
pattern_name: d
classes: {cell: CL:0000000}
vars: {part: "'cell'"}
def:
  text: "A cell related to %s."
  vars: [part]
subClassOf: {text: "'cell'"}
"#;
    let mut labels = HashMap::new();
    labels.insert("http://purl.obolibrary.org/obo/CL_0000540".to_string(), "neuron".to_string());
    let m = dosdp::generate(P, "defined_class\tpart\nT:1\tCL:0000540\n", &labels).unwrap();
    assert!(has_literal_ann(&m, "http://purl.obolibrary.org/obo/IAO_0000115", "A cell related to neuron."),
        "expected IAO:0000115 definition");
}

/// `docs` renders a batch of patterns as Markdown pages plus an index;
/// `validate_data` accepts a good table.
#[test]
fn dosdp_docs_and_validate() {
    let dir = std::env::temp_dir().join(format!("om-dosdp-docs-{}", std::process::id()));
    let tpl = dir.join("patterns");
    let data = dir.join("data");
    let out = dir.join("docs");
    for d in [&tpl, &data, &out] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(tpl.join("part_of_x.yaml"), PATTERN).unwrap();
    std::fs::write(data.join("part_of_x.tsv"), "defined_class\tpart\nCL:1\tUBERON:1\n").unwrap();
    dosdp::docs_batch(
        &tpl,
        &data,
        &["part_of_x".to_string()],
        &out,
        "http://example.org/",
        &HashMap::new(),
        "tsv",
    )
    .unwrap();
    let md = std::fs::read_to_string(out.join("part_of_x.md")).unwrap();
    assert!(md.contains("# part_of_x"), "docs page should be titled with the pattern name");
    assert!(md.contains("## Data preview"), "docs page should carry the data preview");
    let index = std::fs::read_to_string(out.join("index.md")).unwrap();
    assert!(index.contains("part_of_x.md"), "index should link the pattern page");
    std::fs::remove_dir_all(&dir).ok();
    dosdp::validate_data("defined_class\tpart\nCL:1\tUBERON:1\n").unwrap();
}

/// `multi_clause` semantics: `sep` joins distinct CLAUSES, while a list var
/// ITERATES — one annotation axiom per item. So a single clause over a 2-item list
/// yields two def axioms, NOT one joined string.
#[test]
fn dosdp_multi_clause_list_is_per_item() {
    const P: &str = r#"
pattern_name: mc
classes: {cell: CL:0000000}
list_vars: {parts: "'cell'"}
def:
  multi_clause:
    sep: " and "
    clauses:
      - {text: "part of %s", vars: [parts]}
subClassOf: {text: "'cell'"}
"#;
    let mut labels = HashMap::new();
    labels.insert("http://purl.obolibrary.org/obo/CL_0000100".to_string(), "a".to_string());
    labels.insert("http://purl.obolibrary.org/obo/CL_0000200".to_string(), "b".to_string());
    let m = dosdp::generate(P, "defined_class\tparts\nT:1\tCL:0000100|CL:0000200\n", &labels).unwrap();
    let iao = "http://purl.obolibrary.org/obo/IAO_0000115";
    assert!(has_literal_ann(&m, iao, "part of a"), "one def per list item");
    assert!(has_literal_ann(&m, iao, "part of b"), "one def per list item");
    assert!(!has_literal_ann(&m, iao, "part of a and part of b"), "must NOT join list items");
}

/// `multi_clause` with several SCALAR clauses joins them with `sep` into one
/// string (the cartesian product of single-element clause-sets).
#[test]
fn dosdp_multi_clause_scalar_clauses_join() {
    const P: &str = r#"
pattern_name: mc2
classes: {cell: CL:0000000}
vars: {x: "'cell'", y: "'cell'"}
def:
  multi_clause:
    sep: " and "
    clauses:
      - {text: "foo %s", vars: [x]}
      - {text: "bar %s", vars: [y]}
subClassOf: {text: "'cell'"}
"#;
    let mut labels = HashMap::new();
    labels.insert("http://purl.obolibrary.org/obo/CL_0000100".to_string(), "a".to_string());
    labels.insert("http://purl.obolibrary.org/obo/CL_0000200".to_string(), "b".to_string());
    let m = dosdp::generate(P, "defined_class\tx\ty\nT:1\tCL:0000100\tCL:0000200\n", &labels).unwrap();
    assert!(has_literal_ann(&m, "http://purl.obolibrary.org/obo/IAO_0000115", "foo a and bar b"),
        "scalar clauses join with sep");
}

/// `annotations` with a list `value` → one annotation axiom per list item.
#[test]
fn dosdp_annotation_value_list() {
    const P: &str = r#"
pattern_name: avl
classes: {cell: CL:0000000}
annotationProperties: {xref: oboInOwl:hasDbXref}
data_list_vars: {refs: "xsd:string"}
annotations:
  - {annotationProperty: xref, value: refs}
subClassOf: {text: "'cell'"}
"#;
    let m = dosdp::generate(P, "defined_class\trefs\nT:1\tPMID:1|PMID:2\n", &HashMap::new()).unwrap();
    let xref = "http://www.geneontology.org/formats/oboInOwl#hasDbXref";
    assert!(has_literal_ann(&m, xref, "PMID:1") && has_literal_ann(&m, xref, "PMID:2"),
        "expected one xref annotation per list item");
}

/// Schema leniency for the shorthand forms that patterns in the wild use:
/// `generated_synonyms` as a YAML list of templates, and `xrefs` as a single
/// string (a column reference). Both must parse, not be rejected as schema
/// violations.
#[test]
fn dosdp_native_schema_forms_parse() {
    const P: &str = r#"
pattern_name: native
classes: {cell: CL:0000000}
annotationProperties: {syn: oboInOwl:hasExactSynonym}
vars: {syn1: "'cell'"}
data_vars: {x: "xsd:string"}
def:
  text: "def %s"
  vars: [x]
  xrefs: refcol
generated_synonyms:
  - text: "%s cell"
    vars: [syn1]
subClassOf: {text: "'cell'"}
"#;
    let mut labels = HashMap::new();
    labels.insert("http://purl.obolibrary.org/obo/CL_0000100".to_string(), "a".to_string());
    let m = dosdp::generate(P, "defined_class\tsyn1\tx\trefcol\nT:1\tCL:0000100\thi\tPMID:9\n", &labels).unwrap();
    // array generated_synonyms parsed and produced the synonym
    assert!(has_literal_ann(&m, "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym", "a cell"));
    // string xref `refcol` resolved to the column value as an axiom annotation on the def
    let def_has_xref = m.ont.iter().any(|ac| matches!(&ac.component,
        Component::AnnotationAssertion(aa)
            if aa.ann.ap.0.as_ref() == "http://purl.obolibrary.org/obo/IAO_0000115")
        && ac.ann.iter().any(|a| matches!(&a.av, AnnotationValue::Literal(l) if l.literal() == "PMID:9")));
    assert!(def_has_xref, "string xref column ref should resolve to the cell value as a def xref");
}

/// OBO **override columns**: a data column named `defined_class_<field>` supplies
/// the value directly, overriding the field's template. Applies to
/// name/comment/def/namespace and `generated_*synonyms`; plain `*_synonym` fields
/// have no override.
#[test]
fn dosdp_override_columns() {
    const P: &str = r#"
pattern_name: ov
classes: {cell: CL:0000000}
annotationProperties: {exact_synonym: oboInOwl:hasExactSynonym}
vars: {part: "'cell'"}
name: {text: "templated %s", vars: [part]}
generated_synonyms:
  - {text: "templated syn %s", vars: [part]}
subClassOf: {text: "'cell'"}
"#;
    let data = "defined_class\tpart\tdefined_class_name\tdefined_class_exact_synonym\n\
                T:1\tCL:0000540\tOVERRIDE NAME\tOVERRIDE SYN\n";
    let m = dosdp::generate(P, data, &HashMap::new()).unwrap();
    assert!(has_literal_ann(&m, "http://www.w3.org/2000/01/rdf-schema#label", "OVERRIDE NAME"),
        "name should be overridden by defined_class_name column");
    assert!(has_literal_ann(&m, "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym", "OVERRIDE SYN"),
        "generated synonym should be overridden by defined_class_exact_synonym column");
    // the templates must NOT also be emitted.
    assert!(!has_literal_ann(&m, "http://www.w3.org/2000/01/rdf-schema#label", "templated neuron"),
        "template output should be suppressed when overridden");
}

/// **Permutation synonyms**: a `permutations` spec generates extra synonyms by
/// substituting the filler term's own annotation values (from the supplied
/// ontology index), combinatorially with its label.
#[test]
fn dosdp_permutation_synonyms() {
    const P: &str = r#"
pattern_name: perm
classes: {cell: CL:0000000, neuron: CL:0000540}
annotationProperties: {exact_synonym: oboInOwl:hasExactSynonym}
vars: {part: "'neuron'"}
generated_synonyms:
  - text: "%s found in tissue"
    vars: [part]
    permutations:
      - var: part
        annotationProperties: [exact_synonym]
subClassOf: {text: "'cell'"}
"#;
    use std::collections::HashMap as Map;
    let mut labels = HashMap::new();
    labels.insert("http://purl.obolibrary.org/obo/CL_0000540".to_string(), "neuron".to_string());
    let mut props: Map<String, Vec<String>> = Map::new();
    props.insert(
        "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym".to_string(),
        vec!["nerve cell".to_string(), "neurocyte".to_string()],
    );
    let mut index = Map::new();
    index.insert("http://purl.obolibrary.org/obo/CL_0000540".to_string(), props);
    let gopts = dosdp::GenerateOptions { annotation_index: index, ..Default::default() };
    let m = dosdp::generate_with(P, "defined_class\tpart\nT:1\tCL:0000540\n", &labels, &gopts).unwrap();
    let syn = "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym";
    for expected in ["neuron found in tissue", "nerve cell found in tissue", "neurocyte found in tissue"] {
        assert!(has_literal_ann(&m, syn, expected), "expected permutation synonym {expected:?}");
    }
}

/// Free-form `annotations:` entries may name an explicit `override` column whose
/// value supersedes the template for that row.
#[test]
fn dosdp_explicit_override_column() {
    const P: &str = r#"
pattern_name: eov
classes: {cell: CL:0000000}
annotationProperties: {comment: rdfs:comment}
vars: {part: "'cell'"}
annotations:
  - annotationProperty: comment
    text: "templated %s"
    vars: [part]
    override: my_comment
subClassOf: {text: "'cell'"}
"#;
    // row 1 supplies the override column; row 2 leaves it empty → template used.
    let data = "defined_class\tpart\tmy_comment\nT:1\tCL:0000540\tEXPLICIT\nT:2\tCL:0000540\t\n";
    let mut labels = HashMap::new();
    labels.insert("http://purl.obolibrary.org/obo/CL_0000540".to_string(), "neuron".to_string());
    let m = dosdp::generate(P, data, &labels).unwrap();
    let comment = "http://www.w3.org/2000/01/rdf-schema#comment";
    assert!(has_literal_ann(&m, comment, "EXPLICIT"), "override column value should be used for row 1");
    assert!(has_literal_ann(&m, comment, "templated neuron"), "template should be used for row 2 (no override)");
}

/// A `Declaration(...)` is emitted for every entity in the generated ontology's
/// signature — the defined class, the filler classes, the relation object
/// properties, and the annotation properties used — but never for the OWL/RDF
/// built-ins (rdfs:label), whose types the specification already fixes. Declaring
/// the whole signature means a regenerated `definitions.owl` types every term it
/// mentions when it is read on its own.
#[test]
fn dosdp_declares_full_signature() {
    let model = dosdp::generate(PATTERN, DATA, &HashMap::new()).unwrap();

    let declared_classes: Vec<String> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::DeclareClass(d) => Some(d.0 .0.as_ref().to_string()),
            _ => None,
        })
        .collect();
    // The defined class AND the filler class are declared.
    assert!(declared_classes.iter().any(|c| c == "http://purl.obolibrary.org/obo/CL_1000001"));
    assert!(
        declared_classes.iter().any(|c| c == "http://purl.obolibrary.org/obo/UBERON_0000955"),
        "filler class must be declared (dosdp-tools parity): {declared_classes:?}"
    );
    // The relation object property is declared.
    assert!(
        model.ont.iter().any(|ac| matches!(&ac.component,
            Component::DeclareObjectProperty(d) if d.0 .0.as_ref() == "http://purl.obolibrary.org/obo/BFO_0000050")),
        "relation object property must be declared"
    );
    // rdfs:label (built-in) is used but never declared.
    assert!(
        !model.ont.iter().any(|ac| matches!(&ac.component,
            Component::DeclareAnnotationProperty(d) if d.0 .0.as_ref() == "http://www.w3.org/2000/01/rdf-schema#label")),
        "rdfs:label is built-in and must not be declared"
    );
}

/// When a filler term carries several `rdfs:label` values across the import
/// closure, the label map keeps the lexicographic minimum of those values, so the
/// choice is deterministic and independent of axiom order. Here the filler has both
/// "alpha-tocopherol" and "α-tocopherol"; the ASCII form wins ('a' < 'α').
#[test]
fn dosdp_label_picks_lexicographic_min() {
    use owlmake::model::Model;
    use horned_owl::model::{
        AnnotatedComponent, AnnotationAssertion, AnnotationSubject, AnnotationValue, Build,
        Component, Literal, MutableOntology,
    };
    use horned_owl::ontology::set::SetOntology;

    let b = Build::new();
    let filler = "http://purl.obolibrary.org/obo/CHEBI_22470";
    let label = b.annotation_property("http://www.w3.org/2000/01/rdf-schema#label");
    let mut ont: SetOntology<_> = SetOntology::new();
    // Insert the Greek form first so a naive first-wins index would pick it.
    for text in ["α-tocopherol", "alpha-tocopherol"] {
        ont.insert(AnnotatedComponent::from(Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(filler)),
            ann: horned_owl::model::Annotation {
                ap: label.clone(),
                av: AnnotationValue::Literal(Literal::Simple { literal: text.to_string() }),
                ann: Default::default(),
            },
        })));
    }
    let src = Model::from_parts(ont, owlmake::model::default_prefixes());
    let (labels, _index) = dosdp::ontology_context_from_models(&[&src]);
    assert_eq!(
        labels.get(filler).map(String::as_str),
        Some("alpha-tocopherol"),
        "lexicographic-min label must win regardless of axiom order"
    );
}

/// A `def` (or any) column wrapped in double quotes in the TSV follows the RFC 4180
/// convention: its surrounding quotes are stripped (and `""` unescaped), not emitted
/// verbatim as `\"…\"`. A quoted field may also contain the tab delimiter without
/// mis-splitting the row.
#[test]
fn dosdp_tsv_strips_rfc4180_quoting() {
    const P: &str = r#"
pattern_name: defp
classes: {thing: owl:Thing}
data_vars: {definition: xsd:string}
vars: {part: "'thing'"}
def:
  text: "%s"
  vars: [definition]
subClassOf: {text: "'thing'"}
"#;
    // The definition cell is CSV-quoted and contains a comma and a tab.
    let data = "defined_class\tpart\tdefinition\nT:1\tCL:0000540\t\"A cell, with\ttab and \"\"quotes\"\".\"\n";
    let m = dosdp::generate(P, data, &HashMap::new()).unwrap();
    let iao_def = "http://purl.obolibrary.org/obo/IAO_0000115";
    let got: Vec<String> = m
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            horned_owl::model::Component::AnnotationAssertion(aa) if aa.ann.ap.0.as_ref() == iao_def => {
                match &aa.ann.av {
                    horned_owl::model::AnnotationValue::Literal(l) => Some(match l {
                        horned_owl::model::Literal::Simple { literal }
                        | horned_owl::model::Literal::Language { literal, .. }
                        | horned_owl::model::Literal::Datatype { literal, .. } => literal.clone(),
                    }),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        got,
        vec!["A cell, with\ttab and \"quotes\".".to_string()],
        "outer quotes stripped, \"\" unescaped, embedded tab preserved"
    );
}

/// One dictionary name being a word-prefix of another (`cell` / `cell cycle
/// process`, as in CL's `cyclingCellStates`) must not let the shorter name's
/// bareword rule rewrite the inside of the longer quoted reference. Substituting
/// `cell` first would turn `'cell cycle process'` into `'<…CL_0000000> cycle
/// process'`, which parses as one bogus entity name and loses the trailing
/// conjunct, so the longest matching dictionary name is substituted first.
#[test]
fn dosdp_longer_entity_name_wins_over_prefix() {
    const P: &str = r#"
pattern_name: cycling
classes:
  cell: CL:0000000
  cell cycle process: GO:0022402
  active: PATO:0002354
relations:
  participates in: RO:0000056
  has quality: RO:0000086
vars:
  cell: "'cell'"
name:
  text: "cycling %s"
  vars: [cell]
equivalentTo:
  text: "%s and ('participates in' some 'cell cycle process') and ('has quality' some active)"
  vars: [cell]
"#;
    let data = "defined_class\tcell\nCL:4033068\tCL:0000236\n";
    let m = dosdp::generate(P, data, &HashMap::new()).unwrap();

    let obo = |frag: &str| format!("http://purl.obolibrary.org/obo/{frag}");
    let mut fillers: Vec<(String, String)> = Vec::new();
    for ac in m.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            for member in &eq.0 {
                if let CE::ObjectIntersectionOf(parts) = member {
                    for p in parts {
                        if let CE::ObjectSomeValuesFrom { ope, bce } = p {
                            let horned_owl::model::ObjectPropertyExpression::ObjectProperty(r) = ope else {
                                continue;
                            };
                            if let CE::Class(c) = bce.as_ref() {
                                fillers.push((r.0.as_ref().to_string(), c.0.as_ref().to_string()));
                            }
                        }
                    }
                }
            }
        }
    }
    fillers.sort();
    assert_eq!(
        fillers,
        vec![
            (obo("RO_0000056"), obo("GO_0022402")),
            (obo("RO_0000086"), obo("PATO_0002354")),
        ],
        "both conjuncts survive and 'cell cycle process' resolves to GO:0022402"
    );
}
