//! OBO format reader/writer tests.

use horned_owl::model::{ClassExpression as CE, Component};

use owlmake::io::{self, Format};

const SAMPLE: &str = r#"format-version: 1.2
ontology: test

[Term]
id: X:1
name: alpha
def: "the first thing" [PMID:123]
synonym: "first" EXACT []
is_a: X:2

[Term]
id: X:2
name: beta
relationship: part_of X:3

[Term]
id: X:3
name: gamma

[Typedef]
id: part_of
name: part of
is_transitive: true
"#;

fn load_obo(s: &str) -> owlmake::model::Model {
    io::load_from(std::io::Cursor::new(s.as_bytes().to_vec()), Format::Obo).unwrap()
}

#[test]
fn obo_reader_maps_core_constructs() {
    let m = load_obo(SAMPLE);

    let x1 = "http://purl.obolibrary.org/obo/X_1";
    let x2 = "http://purl.obolibrary.org/obo/X_2";
    let x3 = "http://purl.obolibrary.org/obo/X_3";
    // A bare un-xref'd Typedef id has no globally agreed identity, so it resolves to
    // the ontology-local IRI `obo/<ontology>#<id>` rather than the global
    // `obo/<id>`.
    let part_of = "http://purl.obolibrary.org/obo/test#part_of";

    // is_a → SubClassOf
    let has_isa = m.ont.iter().any(|ac| match &ac.component {
        Component::SubClassOf(sc) => matches!((&sc.sub, &sc.sup),
            (CE::Class(a), CE::Class(b)) if a.0.as_ref() == x1 && b.0.as_ref() == x2),
        _ => false,
    });
    assert!(has_isa, "is_a X:1 → X:2 must map to SubClassOf");

    // relationship → SubClassOf(X, ObjectSomeValuesFrom(R, Y))
    let has_rel = m.ont.iter().any(|ac| match &ac.component {
        Component::SubClassOf(sc) => match (&sc.sub, &sc.sup) {
            (CE::Class(a), CE::ObjectSomeValuesFrom { .. }) => a.0.as_ref() == x2,
            _ => false,
        },
        _ => false,
    });
    assert!(has_rel, "relationship must map to existential SubClassOf");

    // Typedef transitive → TransitiveObjectProperty
    let has_trans = m.ont.iter().any(|ac| matches!(&ac.component,
        Component::TransitiveObjectProperty(t)
            if matches!(&t.0, horned_owl::model::ObjectPropertyExpression::ObjectProperty(p) if p.0.as_ref() == part_of)));
    assert!(has_trans, "is_transitive must map to TransitiveObjectProperty");

    // Declarations present.
    let n_classes = m.ont.iter().filter(|ac| matches!(ac.component, Component::DeclareClass(_))).count();
    assert_eq!(n_classes, 3, "three Term stanzas → three class declarations");

    let _ = x3;
}

#[test]
fn obo_roundtrips_through_owl() {
    // obo -> owl(functional) -> obo preserves the class set and is_a edges.
    let m = load_obo(SAMPLE);
    let mut owx = Vec::new();
    io::write_to_ref(&m, &mut owx, Format::Functional).unwrap();
    let m2 = io::load_from(std::io::Cursor::new(owx), Format::Functional).unwrap();

    let count_classes = |m: &owlmake::model::Model| {
        m.ont
            .iter()
            .filter(|ac| matches!(ac.component, Component::DeclareClass(_)))
            .count()
    };
    assert_eq!(count_classes(&m), count_classes(&m2));

    // And obo output is non-empty and re-readable.
    let mut obo_out = Vec::new();
    io::write_to_ref(&m, &mut obo_out, Format::Obo).unwrap();
    let m3 = io::load_from(std::io::Cursor::new(obo_out), Format::Obo).unwrap();
    assert_eq!(count_classes(&m3), 3);
}

// --- Writer fidelity: the exact bytes of an OBO release file --------------
//
// Most tests below render an OWL functional-syntax fixture to OBO and pin the
// resulting stanza verbatim; the round-trip test starts from OBO source instead, and
// a couple pin a header prefix or a single line rather than a whole stanza. Tag
// choice, tag order, label comments and qualifier blocks are all observable in a
// release file, so each is nailed down rather than left to whatever the axiom order
// happens to give.

/// An OWL functional-syntax fixture rendered to OBO by owlmake.
fn to_obo(ofn: &str) -> String {
    let m = io::load_from(std::io::Cursor::new(ofn.as_bytes().to_vec()), Format::Functional).unwrap();
    let mut out = Vec::new();
    io::write_to_ref(&m, &mut out, Format::Obo).unwrap();
    String::from_utf8(out).unwrap()
}

/// The `[Term]`/`[Typedef]` stanza for `id`, without the leading stanza marker.
fn stanza(obo: &str, id: &str) -> String {
    obo.split("\n\n")
        .find(|s| s.lines().any(|l| l == format!("id: {id}")))
        .unwrap_or_else(|| panic!("no stanza for {id} in:\n{obo}"))
        .lines()
        .filter(|l| !l.starts_with('['))
        .collect::<Vec<_>>()
        .join("\n")
}

const PREAMBLE: &str = r#"Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)
Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Prefix(obo:=<http://purl.obolibrary.org/obo/>)
"#;

#[test]
fn obo_writer_appends_label_comments_to_reference_tags() {
    // A reference tag carries its referent's label as a trailing `! …` comment:
    // `is_a: TST:0000002 ! two`. A `relationship:`/`intersection_of:` line carries
    // the labels of *both* tokens, and a token whose referent has no label
    // contributes nothing (TST:0000003 below is unlabelled).
    let obo = to_obo(&format!(
        "{PREAMBLE}Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:TST_0000001))
Declaration(Class(obo:TST_0000002))
Declaration(Class(obo:TST_0000003))
Declaration(ObjectProperty(obo:RO_0002131))
Declaration(ObjectProperty(obo:BFO_0000050))
AnnotationAssertion(rdfs:label obo:TST_0000002 \"two\")
AnnotationAssertion(rdfs:label obo:RO_0002131 \"overlaps\")
AnnotationAssertion(rdfs:label obo:BFO_0000050 \"part of\")
SubClassOf(obo:TST_0000001 obo:TST_0000002)
SubClassOf(obo:TST_0000001 ObjectSomeValuesFrom(obo:BFO_0000050 obo:TST_0000002))
SubClassOf(obo:TST_0000001 ObjectSomeValuesFrom(obo:RO_0002131 obo:TST_0000003))
EquivalentClasses(obo:TST_0000003 ObjectIntersectionOf(obo:TST_0000002 ObjectSomeValuesFrom(obo:RO_0002131 obo:TST_0000002)))
)"
    ));
    assert_eq!(
        stanza(&obo, "TST:0000001"),
        "id: TST:0000001\n\
         is_a: TST:0000002 ! two\n\
         relationship: BFO:0000050 TST:0000002 ! part of two\n\
         relationship: RO:0002131 TST:0000003 ! overlaps"
    );
    assert_eq!(
        stanza(&obo, "TST:0000003"),
        "id: TST:0000003\n\
         intersection_of: TST:0000002 ! two\n\
         intersection_of: RO:0002131 TST:0000002 ! overlaps two"
    );
}

#[test]
fn obo_writer_uses_oboformat_tag_order_and_sorts_repeated_tags() {
    // The `[Term]` tag order is id, name, namespace, alt_id, def, comment,
    // subset, synonym, xref, is_a, …, relationship, property_value, is_obsolete,
    // … — an order that follows neither the axiom order nor the alphabet, so
    // `def` precedes `comment` and `is_a` precedes `property_value`. Repeated tags
    // (and the `[xref, …]` list inside `def:`) are sorted case-insensitively, not
    // left in axiom order.
    let obo = to_obo(&format!(
        "{PREAMBLE}Prefix(oio:=<http://www.geneontology.org/formats/oboInOwl#>)
Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:TST_0000001))
Declaration(Class(obo:TST_0000002))
AnnotationAssertion(rdfs:label obo:TST_0000001 \"one\")
AnnotationAssertion(Annotation(oio:hasDbXref \"ZZZ:9\") Annotation(oio:hasDbXref \"AAA:1\") obo:IAO_0000115 obo:TST_0000001 \"a def\")
AnnotationAssertion(rdfs:comment obo:TST_0000001 \"a comment\")
AnnotationAssertion(oio:hasDbXref obo:TST_0000001 \"ZZZ:1\")
AnnotationAssertion(oio:hasDbXref obo:TST_0000001 \"AAA:1\")
AnnotationAssertion(oio:hasExactSynonym obo:TST_0000001 \"zsyn\")
AnnotationAssertion(oio:hasExactSynonym obo:TST_0000001 \"asyn\")
AnnotationAssertion(obo:IAO_0000116 obo:TST_0000001 \"a note\")
SubClassOf(obo:TST_0000001 obo:TST_0000002)
)"
    ));
    assert_eq!(
        stanza(&obo, "TST:0000001"),
        "id: TST:0000001\n\
         name: one\n\
         def: \"a def\" [AAA:1, ZZZ:9]\n\
         comment: a comment\n\
         synonym: \"asyn\" EXACT []\n\
         synonym: \"zsyn\" EXACT []\n\
         xref: AAA:1\n\
         xref: ZZZ:1\n\
         is_a: TST:0000002\n\
         property_value: IAO:0000116 \"a note\" xsd:string"
    );
}

#[test]
fn obo_writer_shortens_iris_the_way_oboformat_does() {
    // `subset:` values are OBO short forms, not full IRIs (`obo/cl#cellxgene_subset`
    // → `cellxgene_subset`, `obo/ubprop#_upper_level` → `ubprop:upper_level`).
    //
    // Only a prefix the DOCUMENT declares may shorten anything, and it shortens
    // under the name the document gave it — the `oboInOwl` namespace is bound to
    // `oio` here, so that is the `idspace:` line. A namespace nothing declared gets
    // no CURIE and no `idspace:`: `foaf:depiction` and `terms:date` stay full IRIs,
    // and the sssom qualifier key falls to the mechanical id rule, which takes the
    // last path segment and splits it on `_` — `mapping_justification` becomes
    // `mapping:justification`.
    let obo = to_obo(&format!(
        "{PREAMBLE}Prefix(oio:=<http://www.geneontology.org/formats/oboInOwl#>)
Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:TST_0000001))
AnnotationAssertion(oio:inSubset obo:TST_0000001 <http://purl.obolibrary.org/obo/cl#cellxgene_subset>)
AnnotationAssertion(oio:inSubset obo:TST_0000001 <http://purl.obolibrary.org/obo/ubprop#_upper_level>)
AnnotationAssertion(<http://xmlns.com/foaf/0.1/depiction> obo:TST_0000001 \"http://img\"^^xsd:anyURI)
AnnotationAssertion(<http://purl.org/dc/terms/date> obo:TST_0000001 \"2020-01-01\")
AnnotationAssertion(Annotation(<https://w3id.org/sssom/mapping_justification> \"semapv:ManualMappingCuration\") oio:hasDbXref obo:TST_0000001 \"FBbt:00005106\")
)"
    ));
    assert_eq!(
        stanza(&obo, "TST:0000001"),
        "id: TST:0000001\n\
         subset: cellxgene_subset\n\
         subset: ubprop:upper_level\n\
         xref: FBbt:00005106 {mapping:justification=\"semapv:ManualMappingCuration\"}\n\
         property_value: http://purl.org/dc/terms/date \"2020-01-01\" xsd:string\n\
         property_value: http://xmlns.com/foaf/0.1/depiction \"http://img\" xsd:anyURI"
    );
    assert!(obo.contains("idspace: oio http://www.geneontology.org/formats/oboInOwl# \n"), "{obo}");
    for undeclared in ["idspace: foaf", "idspace: sssom", "idspace: terms"] {
        assert!(!obo.contains(undeclared), "undeclared namespace announced:\n{obo}");
    }
}

#[test]
fn obo_writer_folds_merged_stubs_into_alt_id() {
    // A deprecated class with obsolescence reason IAO:0000227 ("terms merged")
    // and a single `IAO:0100001 replaced_by` is the OWL spelling of `alt_id:` on
    // the replacement — the stub gets no stanza of its own. Read literally instead,
    // CL's 76 merged ids become bogus obsolete `[Term]`s with no `alt_id:` at all.
    let obo = to_obo(&format!(
        "{PREAMBLE}Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:TST_0000001))
Declaration(Class(obo:TST_0000009))
AnnotationAssertion(rdfs:label obo:TST_0000001 \"one\")
AnnotationAssertion(owl:deprecated obo:TST_0000009 \"true\"^^xsd:boolean)
AnnotationAssertion(obo:IAO_0000231 obo:TST_0000009 obo:IAO_0000227)
AnnotationAssertion(obo:IAO_0100001 obo:TST_0000009 obo:TST_0000001)
)"
    ));
    assert_eq!(stanza(&obo, "TST:0000001"), "id: TST:0000001\nname: one\nalt_id: TST:0000009");
    assert!(!obo.lines().any(|l| l == "id: TST:0000009"), "merged stub must not get a stanza:\n{obo}");
}

#[test]
fn obo_writer_splits_property_chains_by_head() {
    // `P ∘ R ⊑ P` is `transitive_over: R` (with a label comment); only a chain
    // headed by some *other* property is `holds_over_chain:`. A chain of three or
    // more links has no OBO tag at all — it is carried in the `owl-axioms:` header
    // instead — so it must never appear as a `holds_over_chain:` clause.
    let obo = to_obo(&format!(
        "{PREAMBLE}Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(ObjectProperty(obo:BFO_0000050))
Declaration(ObjectProperty(obo:RO_0002131))
AnnotationAssertion(rdfs:label obo:BFO_0000050 \"part of\")
AnnotationAssertion(rdfs:label obo:RO_0002131 \"overlaps\")
SubObjectPropertyOf(ObjectPropertyChain(obo:BFO_0000050 obo:RO_0002131) obo:BFO_0000050)
SubObjectPropertyOf(ObjectPropertyChain(obo:RO_0002131 obo:BFO_0000050) obo:BFO_0000050)
SubObjectPropertyOf(ObjectPropertyChain(obo:BFO_0000050 obo:BFO_0000050 obo:RO_0002131) obo:BFO_0000050)
)"
    ));
    assert_eq!(
        stanza(&obo, "BFO:0000050"),
        "id: BFO:0000050\n\
         name: part of\n\
         holds_over_chain: RO:0002131 BFO:0000050\n\
         transitive_over: RO:0002131 ! overlaps"
    );
}

#[test]
fn obo_writer_emits_header_and_macro_tags() {
    // `data-version:` comes from the version IRI, `remark:` from the ontology's
    // rdfs:comment, and the Typedef-only `expand_expression_to:` from IAO:0000424.
    // Header tags are emitted in OBO format's fixed header order.
    let obo = to_obo(&format!(
        "{PREAMBLE}Ontology(<http://purl.obolibrary.org/obo/test.owl><http://purl.obolibrary.org/obo/test/releases/2020-01-01/test.owl>
Annotation(rdfs:comment \"a remark here\")
Declaration(ObjectProperty(obo:RO_0002131))
AnnotationAssertion(rdfs:label obo:RO_0002131 \"overlaps\")
AnnotationAssertion(obo:IAO_0000424 obo:RO_0002131 \"BFO_0000051 some ?Y\")
)"
    ));
    assert!(
        obo.starts_with(
            "format-version: 1.2\n\
             data-version: releases/2020-01-01\n\
             remark: a remark here\n\
             ontology: test\n"
        ),
        "{obo}"
    );
    assert_eq!(
        stanza(&obo, "RO:0002131"),
        "id: RO:0002131\n\
         name: overlaps\n\
         expand_expression_to: \"BFO_0000051 some ?Y\" []"
    );
}

#[test]
fn obo_writer_keeps_dbxref_provenance_as_qualifiers() {
    // Only `def:`/`synonym:` have the `[xref, …]` bracket list; on every other tag
    // an `oboInOwl:hasDbXref` axiom annotation is an `xref="…"` qualifier. CL alone
    // carries ~2400 of these, mostly on `comment:` and `relationship:`, so dropping
    // them would strip that provenance out of the release.
    let obo = to_obo(&format!(
        "{PREAMBLE}Prefix(oio:=<http://www.geneontology.org/formats/oboInOwl#>)
Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:TST_0000001))
Declaration(Class(obo:TST_0000002))
Declaration(ObjectProperty(obo:RO_0002162))
AnnotationAssertion(Annotation(oio:hasDbXref \"PMID:9\") rdfs:comment obo:TST_0000001 \"a comment\")
SubClassOf(Annotation(oio:hasDbXref \"PMID:8\") obo:TST_0000001 ObjectSomeValuesFrom(obo:RO_0002162 obo:TST_0000002))
)"
    ));
    assert_eq!(
        stanza(&obo, "TST:0000001"),
        "id: TST:0000001\n\
         comment: a comment {xref=\"PMID:9\"}\n\
         relationship: RO:0002162 TST:0000002 {xref=\"PMID:8\"}"
    );
}

#[test]
fn obo_roundtrip_preserves_idspace_curies_and_repeated_comments() {
    // The writer renders a declared non-OBO namespace as a CURIE, so the reader
    // has to honour the `idspace:` header when expanding one back — otherwise
    // `sssom:mapping_justification` reloads as the bogus
    // `…/obo/sssom_mapping_justification`. A stanza may also repeat `comment:`
    // (CL has 28 such terms), and every clause has to survive the read, or an
    // obo→obo trip silently loses one.
    //
    // Both comments must survive the READ and be written back as two separate
    // `comment:` lines, one line per clause, rather than joined into one.
    const SRC: &str = r#"format-version: 1.2
idspace: sssom https://w3id.org/sssom/ 
ontology: test

[Term]
id: X:1
name: alpha
comment: first note
comment: second note
xref: Y:2 {sssom:mapping_justification="semapv:ManualMappingCuration"}
"#;
    let m = load_obo(SRC);
    let mut out = Vec::new();
    io::write_to_ref(&m, &mut out, Format::Obo).unwrap();
    let obo = String::from_utf8(out).unwrap();
    assert_eq!(
        stanza(&obo, "X:1"),
        "id: X:1\n\
         name: alpha\n\
         comment: first note\n\
         comment: second note\n\
         xref: Y:2 {sssom:mapping_justification=\"semapv:ManualMappingCuration\"}"
    );
    assert!(obo.contains("idspace: sssom https://w3id.org/sssom/ \n"), "{obo}");
}

/// One `is_a` clause is emitted per SubClassOf AXIOM. An `is_a` that is both
/// asserted and inferred exists in OWL as two distinct axioms — a bare
/// `SubClassOf(A B)` and an annotated `SubClassOf(Annotation(is_inferred "true")
/// A B)` — so the direct OWL→OBO path writes BOTH.
///
/// The pair only collapses when the ontology passes through RDF/XML, because
/// there both axioms share the one `A rdfs:subClassOf B` triple and the reified
/// `owl:Axiom` re-absorbs it. That is a property of the RDF round-trip, not of the
/// OBO writer, so the folding belongs in the RDF reader:
///
///   functional syntax -> obo              -> 2 clauses (this test)
///   functional syntax -> RDF/XML -> obo   -> 1 clause
///
/// MONDO's `filtered.obo` takes the direct path, so folding the pair here would
/// silently drop 44,774 `is_a:` lines from it.
///
/// A General Class Inclusion whose subject is an intersection over a named class
/// still lands in that class's stanza, as an `is_a:` clause qualified by
/// `gci_filler`/`gci_relation` — the OBO spelling of `(A ⊓ ∃part_of.X) ⊑ B`. The
/// two such axioms below therefore contribute one clause each. The OWL products
/// keep these axioms as written, so emitting the clauses is what keeps the `.obo`
/// product carrying the same content: drop them on write and an obo→owl→obo trip
/// loses them.
#[test]
fn obo_writer_emits_one_is_a_clause_per_subclassof_axiom() {
    let obo = to_obo(&format!(
        "{PREAMBLE}Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:TST_0000001))
Declaration(Class(obo:TST_0000002))
Declaration(Class(obo:TST_0000003))
Declaration(Class(obo:TST_0000004))
Declaration(ObjectProperty(obo:BFO_0000050))
SubClassOf(obo:TST_0000001 obo:TST_0000002)
SubClassOf(Annotation(<http://www.geneontology.org/formats/oboInOwl#is_inferred> \"true\") obo:TST_0000001 obo:TST_0000002)
SubClassOf(ObjectIntersectionOf(obo:TST_0000001 ObjectSomeValuesFrom(obo:BFO_0000050 obo:TST_0000003)) obo:TST_0000002)
SubClassOf(ObjectIntersectionOf(obo:TST_0000001 ObjectSomeValuesFrom(obo:BFO_0000050 obo:TST_0000004)) obo:TST_0000002)
)"
    ));
    // The asserted and inferred `SubClassOf(A B)` axioms each get their own `is_a`
    // line; the two General Class Inclusions on the same subject
    // (`A ⊓ part_of some C ⊑ B`) render as their own `is_a` lines qualified by
    // `gci_filler` then `gci_relation` — the qualifier set's hash-bucket order, not
    // a sort and not the insertion order: `gci_relation` is inserted first, so it
    // leads whenever the two land in the same bucket.
    //
    // All four clauses name the same parent, so clause ordering — which keys only
    // on the parent id — leaves them tied, and the tie is broken by the hashed
    // bucket order of the SubClassOf axiom set, whose size fixes the bucket walk.
    // For this fixture that yields `gci_filler=4`, the inferred clause,
    // `gci_filler=3`, then the bare clause last. The walk is deterministic, so
    // pinning the sequence here keeps `is_a:` lines from reshuffling through every
    // release diff.
    assert_eq!(
        stanza(&obo, "TST:0000001"),
        "id: TST:0000001\n\
         is_a: TST:0000002 {gci_filler=\"TST:0000004\", gci_relation=\"BFO:0000050\"}\n\
         is_a: TST:0000002 {is_inferred=\"true\"}\n\
         is_a: TST:0000002 {gci_filler=\"TST:0000003\", gci_relation=\"BFO:0000050\"}\n\
         is_a: TST:0000002"
    );
}

/// The `{…}` qualifier block on an xref/comment/def/synonym tag is not sorted: its
/// order comes from hashing each annotation and walking the resulting buckets, which
/// owlmake computes directly. CL's `UBERON:0002019` xref is the case that pins it —
/// `{source="ncithesaurus:Accessory_Nerve", source="BIRNLEX:812"}` is
/// value-descending, an order no sort key produces. Released OBO files carry this
/// order, so reproducing it keeps qualifier reshuffles out of every release diff.
#[test]
fn obo_writer_orders_xref_qualifiers_by_owlapi_hashset() {
    let obo = to_obo(&format!(
        "{PREAMBLE}Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:UBERON_0002019))
AnnotationAssertion(Annotation(<http://www.geneontology.org/formats/oboInOwl#source> \"BIRNLEX:812\") Annotation(<http://www.geneontology.org/formats/oboInOwl#source> \"ncithesaurus:Accessory_Nerve\") <http://www.geneontology.org/formats/oboInOwl#hasDbXref> obo:UBERON_0002019 \"UMLS:C0000905\")
)"
    ));
    assert!(
        stanza(&obo, "UBERON:0002019").contains(
            "xref: UMLS:C0000905 {source=\"ncithesaurus:Accessory_Nerve\", source=\"BIRNLEX:812\"}"
        ),
        "{}",
        stanza(&obo, "UBERON:0002019")
    );
}

/// A `property_value:` block, by contrast, is added straight from the axiom's
/// *sorted* annotation stream — it never goes through the hashed bucket order — so
/// it stays ascending by property IRI, as in CL's CLM location assignments:
/// `{CLM:0010002="…", evidence="…", comment="…"}`.
#[test]
fn obo_writer_orders_property_value_qualifiers_by_sorted_iri() {
    let obo = to_obo(&format!(
        "{PREAMBLE}Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:CLM_0000001))
Declaration(ObjectProperty(obo:RO_0002131))
AnnotationAssertion(Annotation(obo:CLM_0010002 \"0.2\") Annotation(<http://www.geneontology.org/formats/oboInOwl#evidence> obo:EFO_0008992) Annotation(rdfs:comment \"loc\") obo:RO_0002131 obo:CLM_0000001 obo:DHBA_10333)
)"
    ));
    let s = stanza(&obo, "CLM:0000001");
    assert!(
        s.contains("{CLM:0010002=\"0.2\", evidence=\"EFO:0008992\", comment=\"loc\"}"),
        "{s}"
    );
}

/// `--clean-obo strict` allows a frame ONE `name:`, `def:` and `comment:`, and the
/// survivor is the lexically smallest value — including when that value is the
/// empty string, which writes no line at all.
///
/// The empty value is also untranslatable, so `drop-untranslatable-axioms` removes
/// it. The two must happen in that order: the slot is claimed first and emptied
/// second. Dropping the empty value first hands the slot to the other value and
/// writes a line the frame should not have — `CEPH:0000138` holds `rdfs:comment ""`
/// beside `rdfs:comment "The siphon of Cephalopoda"`, and ends up with neither.
#[test]
fn clean_obo_strict_gives_the_single_slot_to_the_empty_value() {
    let ofn = format!(
        "{PREAMBLE}Ontology(<http://purl.obolibrary.org/obo/test.owl>
Declaration(Class(obo:X_0000001))
Declaration(Class(obo:X_0000002))
AnnotationAssertion(rdfs:label obo:X_0000001 \"one\")
AnnotationAssertion(rdfs:comment obo:X_0000001 \"\")
AnnotationAssertion(rdfs:comment obo:X_0000001 \"zzz\")
AnnotationAssertion(rdfs:label obo:X_0000002 \"two\")
AnnotationAssertion(rdfs:comment obo:X_0000002 \"aaa\")
AnnotationAssertion(rdfs:comment obo:X_0000002 \"zzz\")
)"
    );
    let mut m =
        io::load_from(std::io::Cursor::new(ofn.as_bytes().to_vec()), Format::Functional).unwrap();
    owlmake::cmd::convert::apply_clean_obo(&mut m, "strict drop-untranslatable-axioms");
    let mut out = Vec::new();
    io::write_to_ref(&m, &mut out, Format::Obo).unwrap();
    let obo = String::from_utf8(out).unwrap();

    let one = stanza(&obo, "X:0000001");
    assert!(!one.contains("comment:"), "the empty value holds the slot:\n{one}");
    let two = stanza(&obo, "X:0000002");
    assert!(two.contains("comment: aaa"), "the smallest value holds the slot:\n{two}");
    assert!(!two.contains("comment: zzz"), "only one comment survives:\n{two}");
}
