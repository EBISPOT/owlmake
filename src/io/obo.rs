//! OBO 1.4 flat-file format support, implementing the OBO ↔ OWL 2 mapping for
//! the constructs used by CL/UBERON/MONDO and the rest of the OBO library.
//!
//! Reader: parses header + `[Term]`/`[Typedef]` stanzas into horned-owl axioms.
//! Writer: renders an ontology back to OBO for the OBO-expressible fragment.
//!
//! ID expansion follows the standard OBO PURL convention:
//! `PREFIX:LOCAL` ⇄ `http://purl.obolibrary.org/obo/PREFIX_LOCAL`.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::{BufRead, Write};

use anyhow::Result;
use horned_owl::model::{
    Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, AsymmetricObjectProperty,
    Build, ClassExpression as CE, Component, DeclareAnnotationProperty, DeclareClass,
    DeclareObjectProperty, DisjointClasses, EquivalentClasses, FunctionalObjectProperty,
    InverseFunctionalObjectProperty, InverseObjectProperties, Literal, MutableOntology,
    ObjectPropertyDomain, ObjectPropertyExpression as OPE, ObjectPropertyRange,
    ReflexiveObjectProperty, RcStr, SubAnnotationPropertyOf, SubClassOf, SubObjectPropertyOf,
    SymmetricObjectProperty, TransitiveObjectProperty,
};
use horned_owl::ontology::set::SetOntology;
use std::collections::BTreeSet;

use crate::model::{default_prefixes, Model};

const OBO_BASE: &str = "http://purl.obolibrary.org/obo/";
const OIO: &str = "http://www.geneontology.org/formats/oboInOwl#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const IAO_DEF: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
const IAO_TERM_REPLACED_BY: &str = "http://purl.obolibrary.org/obo/IAO_0100001";
const IAO_OBSOLESCENCE_REASON: &str = "http://purl.obolibrary.org/obo/IAO_0000231";
const IAO_TERMS_MERGED: &str = "http://purl.obolibrary.org/obo/IAO_0000227";

/// Expand an OBO id to a full IRI string.
pub fn expand_id(id: &str) -> String {
    let id = id.trim();
    if id.starts_with("http://") || id.starts_with("https://") {
        return id.to_string();
    }
    match id.split_once(':') {
        Some((pre, local)) => format!("{OBO_BASE}{pre}_{local}"),
        None => format!("{OBO_BASE}{id}"),
    }
}

thread_local! {
    /// The `idspace:` prefix map declared by the OBO document currently being
    /// parsed. It is a thread-local rather than a parameter because the ~25 tag
    /// handlers that need it (every `qualifier_anns` call site) sit several
    /// layers below `load`, and only the reader ever touches it.
    static IDSPACES: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Expand an OBO id, honouring the document's `idspace:` declarations before
/// falling back to the OBO PURL convention. Needed because the writer renders an
/// IRI in a declared non-OBO namespace as a CURIE — CL's
/// `{sssom:mapping_justification="…"}` must come back as
/// `https://w3id.org/sssom/mapping_justification`, not
/// `…/obo/sssom_mapping_justification`.
fn expand_curie(id: &str) -> String {
    let id = id.trim();
    if id.starts_with("http://") || id.starts_with("https://") {
        return id.to_string();
    }
    if let Some((pre, local)) = id.split_once(':') {
        if let Some(ns) = IDSPACES.with(|m| m.borrow().get(pre).cloned()) {
            return format!("{ns}{local}");
        }
    }
    expand_id(id)
}

/// Compress a full IRI to an OBO id where possible (inverse of [`expand_id`]).
pub fn compress_iri(iri: &str) -> String {
    if let Some(rest) = iri.strip_prefix(OBO_BASE) {
        // An ontology-local `#` namespace (e.g. `obo/mondo#disease_has_basis_in`)
        // is written as the bare local name, and the reader re-expands a bare
        // relation/typedef id back into the ontology's `#` namespace. (Emitting
        // `mondo#…` instead would make a reload double-prefix it to
        // `mondo#mondo#…`.)
        if let Some((_, local)) = rest.rsplit_once('#') {
            return local.to_string();
        }
        if let Some(idx) = rest.find('_') {
            // Only treat as PREFIX_LOCAL when the suffix looks like a local id.
            let (pre, local) = rest.split_at(idx);
            let local = &local[1..];
            if !pre.is_empty() && !local.is_empty() && !local.contains('_') {
                return format!("{pre}:{local}");
            }
        }
        return rest.to_string();
    }
    iri.to_string()
}

// === Reader ==============================================================

#[derive(Default)]
struct Stanza {
    tags: Vec<(String, String)>,
}

impl Stanza {
    fn get(&self, key: &str) -> Option<&str> {
        self.tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.tags
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Load an ontology from OBO format.
pub fn load<R: BufRead>(reader: R) -> Result<Model> {
    let b = Build::new();
    let mut ont: SetOntology<RcStr> = SetOntology::new();

    let mut header = Stanza::default();
    let mut stanzas: Vec<(String, Stanza)> = Vec::new();
    let mut current: Option<(String, Stanza)> = None;

    for line in reader.lines() {
        let line = line?;
        let line = strip_comment(&line);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if let Some(s) = current.take() {
                stanzas.push(s);
            }
            let kind = trimmed[1..trimmed.len() - 1].to_string();
            current = Some((kind, Stanza::default()));
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            match &mut current {
                Some((_, s)) => s.tags.push((key, value)),
                None => header.tags.push((key, value)),
            }
        }
    }
    if let Some(s) = current.take() {
        stanzas.push(s);
    }

    // `idspace: PREFIX NAMESPACE [description]` header lines: the document's own
    // CURIE bindings, consulted by `expand_curie` for the rest of the parse.
    IDSPACES.with(|m| {
        let mut m = m.borrow_mut();
        m.clear();
        for line in header.all("idspace") {
            let mut it = line.split_whitespace();
            if let (Some(prefix), Some(ns)) = (it.next(), it.next()) {
                m.insert(prefix.to_string(), ns.to_string());
            }
        }
    });

    // Header → ontology id + annotations.
    if let Some(ont_id) = header.get("ontology") {
        let iri = if ont_id.starts_with("http") {
            ont_id.to_string()
        } else {
            format!("{OBO_BASE}{ont_id}.owl")
        };
        // `data-version:` is the version IRI relative to the ontology id (the
        // inverse of the writer's `data_version`), so a release `.obo` keeps its
        // `owl:versionIRI` through an obo→owl conversion.
        let viri = header.get("data-version").map(|dv| {
            let dv = dv.trim();
            if dv.starts_with("http") {
                b.iri(dv.to_string())
            } else if let Some(short) = iri.strip_prefix(OBO_BASE).and_then(|s| s.strip_suffix(".owl")) {
                b.iri(format!("{OBO_BASE}{short}/{dv}/{short}.owl"))
            } else {
                b.iri(format!("{iri}/{dv}"))
            }
        });
        ont.insert(Component::OntologyID(horned_owl::model::OntologyID {
            iri: Some(b.iri(iri)),
            viri,
        }));
    }

    // `import:` header lines → owl:imports, so downstream merge/import-removal
    // (e.g. the release pipeline) sees the document's full import closure.
    for imp in header.all("import") {
        ont.insert(Component::Import(horned_owl::model::Import(b.iri(imp.trim()))));
    }

    // The header `default-namespace` is applied as `hasOBONamespace` to every
    // term/typedef that does not declare its own `namespace`.
    let default_ns = header.get("default-namespace").map(|s| s.to_string());
    // It is also recorded as an ontology-level `oboInOwl:default-namespace`
    // annotation, so an obo→owl→obo trip can re-derive the header tag (the
    // property is declared by declare_referenced_entities).
    if let Some(ns) = &default_ns {
        ont.insert(Component::OntologyAnnotation(horned_owl::model::OntologyAnnotation(
            ann(&b, &format!("{OIO}default-namespace"), ns),
        )));
    }

    let onto_ns_for_defs = header.get("ontology").and_then(|o| {
        if o.starts_with("http") { None } else { Some(format!("{OBO_BASE}{o}#")) }
    });
    // `synonymtypedef:`/`subsetdef:` header lines declare an annotation property
    // that is a sub-property of oboInOwl:SynonymTypeProperty / :SubsetProperty.
    // The quoted description is carried as `rdfs:label` for a synonymtypedef but
    // as `rdfs:comment` for a subsetdef.
    for (tag, parent, descr_prop) in [
        ("synonymtypedef", "SynonymTypeProperty", RDFS_LABEL),
        ("subsetdef", "SubsetProperty", RDFS_COMMENT),
    ] {
        // Declare the oboInOwl parent property itself. Without its declaration
        // the RDF reader can't classify a `X rdfs:subPropertyOf
        // SubsetProperty` triple as a SubAnnotationPropertyOf when the model is
        // round-tripped through RDF (e.g. owlmake's `query --update`), silently
        // dropping every subsetdef/synonymtypedef.
        if header.all(tag).next().is_some() {
            ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
                b.annotation_property(format!("{OIO}{parent}").as_str()),
            )));
        }
        for s in header.all(tag) {
            let id = s.split_whitespace().next().unwrap_or(s);
            let iri = if id.contains(':') {
                expand_id(id)
            } else {
                resolve_local(id, onto_ns_for_defs.as_deref())
            };
            ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
                b.annotation_property(iri.as_str()),
            )));
            ont.insert(Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
                sub: b.annotation_property(iri.as_str()),
                sup: b.annotation_property(format!("{OIO}{parent}").as_str()),
            }));
            if let Some(rest) = s.strip_prefix(id) {
                if let Some((name, _)) = parse_quoted(rest.trim()) {
                    assert_ann(&b, &mut ont, &iri, descr_prop, &name);
                }
            }
        }
    }

    // Subset and (local) synonym-type names map to IRIs in the ontology's own
    // namespace, `http://purl.obolibrary.org/obo/<ontology>#<name>` — e.g.
    // `ontology: uberon/core` ⇒ `obo/uberon/core#efo_slim`. That is the OBO→OWL
    // mapping for a bare local name.
    let onto_ns = header.get("ontology").and_then(|o| {
        if o.starts_with("http") {
            None
        } else {
            Some(format!("{OBO_BASE}{o}#"))
        }
    });

    // Relation shorthands: a `[Typedef]` whose `id` is a bare name and which has
    // a single `xref` to an ontology term (e.g. `id: disease_has_basis_in_…` +
    // `xref: RO:0004020`) is the OBO shorthand for that property. All uses in
    // `relationship:`/`intersection_of:` resolve to the xref IRI under the OBO
    // relation-shorthand rule, not `obo/<shorthand>`.
    let mut rel_map: HashMap<String, String> = HashMap::new();
    // Metadata-tag properties (`is_metadata_tag: true`) are annotation
    // properties: a `relationship:` using one is an annotation assertion, not a
    // logical existential, and the typedef is declared as an AnnotationProperty.
    let mut metadata_tags: BTreeSet<String> = BTreeSet::new();
    for (kind, st) in &stanzas {
        if kind == "Typedef" {
            if let Some(id) = st.get("id") {
                let iri = if id.contains(':') {
                    expand_id(id)
                } else {
                    let i = typedef_iri(id, st.get("xref"), onto_ns.as_deref());
                    rel_map.insert(id.to_string(), i.clone());
                    i
                };
                if st.get("is_metadata_tag") == Some("true") {
                    metadata_tags.insert(iri);
                }
            }
        }
    }
    // A bare relation name USED in a `relationship:`/`intersection_of:` clause but
    // never DECLARED by a `[Typedef]` is ontology-local: it maps to
    // `obo/<ontology>#<rel>`, not the generic `obo/<rel>` — an undeclared
    // `relationship: undeclared_rel X` in `ontology: mondo` yields
    // `obo/mondo#undeclared_rel`, while a Typedef-with-xref relation resolves to its
    // xref IRI. Seed those into `rel_map` so every resolve_rel call agrees.
    if let Some(ns) = onto_ns.as_deref() {
        for (kind, st) in &stanzas {
            if kind != "Term" {
                continue;
            }
            for tag in ["relationship", "intersection_of"] {
                for v in st.all(tag) {
                    let first = v.split_whitespace().next().unwrap_or("");
                    // Only bare names (a CURIE/IRI resolves on its own), and only
                    // when no Typedef already defined them.
                    if first.is_empty()
                        || first.contains(':')
                        || first.starts_with("http")
                        || rel_map.contains_key(first)
                    {
                        continue;
                    }
                    // `intersection_of: <genus>` (one token) is a class, not a relation.
                    if tag == "intersection_of" && v.split_whitespace().count() < 2 {
                        continue;
                    }
                    rel_map.insert(first.to_string(), format!("{ns}{first}"));
                }
            }
        }
    }

    // Header `property_value:`/`remark:` lines become ontology-level annotations,
    // so the primary ontology's header survives a merge.
    for pv in header.all("property_value") {
        if let Some((ann, _)) = property_value_annotation(&b, pv, &rel_map, onto_ns.as_deref(), true) {
            ont.insert(Component::OntologyAnnotation(horned_owl::model::OntologyAnnotation(ann)));
        }
    }
    for r in header.all("remark") {
        ont.insert(Component::OntologyAnnotation(horned_owl::model::OntologyAnnotation(ann(&b, RDFS_COMMENT, r))));
    }
    // Other OBO header tags become ontology-level annotations in the oboInOwl
    // namespace: `format-version` → `hasOBOFormatVersion`, and the
    // `treat-xrefs-as-*` macro directives (their tag name is the property local).
    for fv in header.all("format-version") {
        ont.insert(Component::OntologyAnnotation(horned_owl::model::OntologyAnnotation(
            ann(&b, &format!("{OIO}hasOBOFormatVersion"), fv),
        )));
    }
    for key in [
        "treat-xrefs-as-equivalent",
        "treat-xrefs-as-genus-differentia",
        "treat-xrefs-as-reverse-genus-differentia",
        "treat-xrefs-as-relationship",
        "treat-xrefs-as-is_a",
        "treat-xrefs-as-has-subclass",
    ] {
        let mut any = false;
        for v in header.all(key) {
            any = true;
            ont.insert(Component::OntologyAnnotation(horned_owl::model::OntologyAnnotation(
                ann(&b, &format!("{OIO}{key}"), v),
            )));
        }
        // A used macro-directive property is declared, carrying the tag name
        // itself as its `rdfs:label` (the format's built-in label for it).
        if any {
            ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
                b.annotation_property(format!("{OIO}{key}").as_str()),
            )));
            assert_ann(&b, &mut ont, &format!("{OIO}{key}"), RDFS_LABEL, key);
        }
    }
    // The built-in oboInOwl annotation properties carry an `rdfs:label` whenever
    // they are used. The synonym/xref/etc. ones come labelled from imports; these
    // edit-file metadata properties do not, so add them.
    for (local, label, present) in [
        ("created_by", "created by", stanzas.iter().any(|(_, s)| s.get("created_by").is_some())),
        ("creation_date", "creation date", stanzas.iter().any(|(_, s)| s.get("creation_date").is_some())),
        ("id", "id", true),
    ] {
        if present {
            assert_ann(&b, &mut ont, &format!("{OIO}{local}"), RDFS_LABEL, label);
        }
    }

    // The `owl-axioms:` header clause carries, in OWL functional syntax, the axioms
    // OBO has no tag for (ClassAssertion, DifferentIndividuals,
    // IrreflexiveObjectProperty, extra DisjointClasses/SubClassOf, re-declarations,
    // …). Its value is one OBO-escaped functional-syntax `Ontology(…)` document.
    // Unescape it, parse it, and fold every axiom back into the model — otherwise
    // an obo→owl conversion silently loses them.
    for oa in header.all("owl-axioms") {
        let text = obo_unescape(oa);
        let mut cfg = horned_owl::io::ParserConfiguration::default();
        cfg.lax = true;
        let parsed: std::result::Result<(SetOntology<RcStr>, _), _> =
            horned_owl::io::ofn::reader::read(&mut text.as_bytes(), cfg);
        match parsed {
            Ok((parsed, _)) => {
                for ac in parsed {
                    if matches!(ac.component, Component::OntologyID(_)) {
                        continue;
                    }
                    ont.insert(ac);
                }
            }
            Err(e) => {
                eprintln!("warning: could not parse owl-axioms header block: {e}");
            }
        }
    }

    for (kind, st) in &stanzas {
        match kind.as_str() {
            "Term" => term_to_owl(&b, &mut ont, st, default_ns.as_deref(), onto_ns.as_deref(), &rel_map, &metadata_tags),
            "Typedef" => typedef_to_owl(&b, &mut ont, st, default_ns.as_deref(), onto_ns.as_deref(), &rel_map, &metadata_tags),
            _ => {} // [Instance] and unknown stanzas: skipped for now
        }
    }

    add_oboinowl_builtin_labels(&b, &mut ont);
    let materialised = declare_referenced_entities(&b, &mut ont);

    let mut m = Model::from_parts(ont, default_prefixes());
    m.materialised_declarations = materialised;
    // OBO carries no document prefix map, so a model read from OBO must not claim
    // one: every prefix such a document ends up declaring is either a builtin or
    // generated from an entity's namespace. Converting a two-term obo yields an
    // xmlns block of owl/rdf/xml/xsd/rdfs plus a generated `oboInOwl`, with no
    // `obo` (the only IRI in that namespace is a CLASS, and classes do not require
    // a namespace declaration) and no `dc`; its functional syntax declares only
    // `:`/owl/rdf/xml/xsd/rdfs, spelling every other IRI in full.
    // `default_prefixes()` above is still the map used to EXPAND CURIEs
    // while parsing — it is just not the document's own declaration set.
    m.format_prefixes_cleared = true;
    // …but the document's own `idspace:` lines ARE its prefix declarations, and an
    // obo→obo trip has to give them back: consulting them only to expand CURIEs
    // during the parse (the thread-local above) and then dropping them would lose
    // every declaration the writer had just made. MONDO's `mondo.obo` is the case —
    // a build step re-reads the written target and re-serialises that model, so a
    // declaration lost on read never comes back.
    let declared: Vec<(String, String)> =
        IDSPACES.with(|m| m.borrow().iter().map(|(p, n)| (p.clone(), n.clone())).collect());
    for (prefix, ns) in declared {
        let _ = m.prefixes.add_prefix(&prefix, &ns);
        if !m.explicit_prefixes.iter().any(|(p, _)| *p == prefix) {
            m.explicit_prefixes.push((prefix, ns));
        }
    }
    Ok(m)
}

/// The OBO built-in annotation properties, each with the canonical `rdfs:label`
/// it carries (`hasExactSynonym` → "has_exact_synonym").
///
/// A property in this table is INTRODUCED by the OBO tag that used it — `def:`
/// gives `IAO_0000115`, `synonym:` gives `oboInOwl:hasExactSynonym`, `xref:`
/// gives `oboInOwl:hasDbXref` — so its declaration is the document's own and
/// stands whatever the import closure declares. A property merely named as a
/// `property_value:` predicate is referenced rather than introduced, and an
/// imported ontology that declares it takes that job over.
///
/// One table, two readers: [`add_oboinowl_builtin_labels`] labels them, and
/// [`declare_referenced_entities`] keeps them out of the withdrawable set.
fn obo_builtin_annotation_properties() -> [(String, &'static str); 31] {
    // Full IRIs so the IAO_* and oboInOwl meta-properties (SubsetProperty …) sit
    // alongside the oboInOwl synonym/xref properties.
    const OBO: &str = "http://purl.obolibrary.org/obo/";
    [
        (format!("{OIO}hasExactSynonym"), "has_exact_synonym"),
        (format!("{OIO}hasNarrowSynonym"), "has_narrow_synonym"),
        (format!("{OIO}hasBroadSynonym"), "has_broad_synonym"),
        (format!("{OIO}hasRelatedSynonym"), "has_related_synonym"),
        (format!("{OIO}hasSynonymType"), "has_synonym_type"),
        (format!("{OIO}hasScope"), "has_scope"),
        (format!("{OIO}hasDbXref"), "database_cross_reference"),
        (format!("{OIO}hasOBONamespace"), "has_obo_namespace"),
        (format!("{OIO}hasOBOFormatVersion"), "has_obo_format_version"),
        (format!("{OIO}hasAlternativeId"), "has_alternative_id"),
        (format!("{OIO}inSubset"), "in_subset"),
        (format!("{OIO}SubsetProperty"), "subset_property"),
        (format!("{OIO}SynonymTypeProperty"), "synonym_type_property"),
        (format!("{OIO}NamespaceIdRule"), "namespace-id-rule"),
        (format!("{OIO}logical-definition-view-relation"), "logical-definition-view-relation"),
        (format!("{OIO}consider"), "consider"),
        (format!("{OIO}shorthand"), "shorthand"),
        (format!("{OIO}id"), "id"),
        (format!("{OIO}created_by"), "created by"),
        (format!("{OIO}creation_date"), "creation date"),
        (format!("{OIO}treat-xrefs-as-is_a"), "treat-xrefs-as-is_a"),
        (format!("{OIO}treat-xrefs-as-has-subclass"), "treat-xrefs-as-has-subclass"),
        (format!("{OIO}treat-xrefs-as-relationship"), "treat-xrefs-as-relationship"),
        (format!("{OIO}treat-xrefs-as-genus-differentia"), "treat-xrefs-as-genus-differentia"),
        (
            format!("{OIO}treat-xrefs-as-reverse-genus-differentia"),
            "treat-xrefs-as-reverse-genus-differentia",
        ),
        (format!("{OIO}treat-xrefs-as-equivalent"), "treat-xrefs-as-equivalent"),
        (format!("{OBO}IAO_0000115"), "definition"),
        (format!("{OBO}IAO_0000424"), "expand expression to"),
        (format!("{OBO}IAO_0000425"), "expand assertion to"),
        (format!("{OBO}IAO_0000427"), "antisymmetric property"),
        (format!("{OBO}IAO_0100001"), "term replaced by"),
    ]
}

/// Each standard oboInOwl annotation property *that is actually used* carries a
/// canonical `rdfs:label` (e.g. `hasExactSynonym` → "has_exact_synonym"). Add
/// those for any used+unlabelled built-in property.
fn add_oboinowl_builtin_labels(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>) {
    let labels = obo_builtin_annotation_properties();
    // Annotation-property IRIs referenced anywhere (assertions, axiom/ontology
    // annotations, declarations, sub-property axioms) and subjects already labelled.
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut labelled: BTreeSet<String> = BTreeSet::new();
    for ac in ont.iter() {
        for a in ac.ann.iter() {
            used.insert(a.ap.0.to_string());
        }
        match &ac.component {
            Component::AnnotationAssertion(ax) => {
                used.insert(ax.ann.ap.0.to_string());
                if ax.ann.ap.0.as_ref() == RDFS_LABEL {
                    if let horned_owl::model::AnnotationSubject::IRI(i) = &ax.subject {
                        labelled.insert(i.to_string());
                    }
                }
            }
            Component::OntologyAnnotation(oa) => {
                used.insert(oa.0.ap.0.to_string());
            }
            Component::DeclareAnnotationProperty(d) => {
                used.insert(d.0 .0.to_string());
            }
            Component::SubAnnotationPropertyOf(s) => {
                used.insert(s.sub.0.to_string());
                used.insert(s.sup.0.to_string());
            }
            _ => {}
        }
    }
    for (iri, label) in &labels {
        if used.contains(iri) && !labelled.contains(iri) {
            assert_ann(b, ont, iri, RDFS_LABEL, label);
        }
    }
}

/// Declare every entity referenced by an axiom that is not already declared —
/// classes/object-properties used in logical axioms and annotation properties
/// used in assertions or axiom annotations.
///
/// These declarations are a WRITER-side materialisation, not anything the OBO
/// document states: a property that only ever appears as a `property_value:`
/// predicate is declared nowhere, yet a serialised RDF/XML document has to give it
/// a type. Returns the set it synthesised, keyed `kind\0IRI`, so a caller that
/// knows the import closure can withdraw the ones whose entity is already typed
/// there — see `Model::materialised_declarations`.
fn declare_referenced_entities(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
) -> std::collections::HashSet<String> {
    let mut classes: BTreeSet<String> = BTreeSet::new();
    // Classes met as the FILLER of a relation restriction. An OBO `relationship:`
    // (and an `intersection_of:` that names a relation) declares its filler
    // outright, because obo format allows a dangling reference there and the
    // translation makes the class explicit to be sure. Such a declaration is the
    // document's own, so it stands whatever the import closure holds — unlike a
    // class named as a PLAIN operand of `is_a:`, `disjoint_from:`, a bare
    // `intersection_of:` or a `union_of:`, which the translation leaves to the
    // signature and which the closure therefore suppresses.
    let mut filler_classes: BTreeSet<String> = BTreeSet::new();
    let mut obj_props: BTreeSet<String> = BTreeSet::new();
    let mut ann_props: BTreeSet<String> = BTreeSet::new();
    let mut declared_c: BTreeSet<String> = BTreeSet::new();
    let mut declared_o: BTreeSet<String> = BTreeSet::new();
    let mut declared_a: BTreeSet<String> = BTreeSet::new();

    fn walk_ce(
        ce: &CE<RcStr>,
        classes: &mut BTreeSet<String>,
        filler_classes: &mut BTreeSet<String>,
        ops: &mut BTreeSet<String>,
        in_filler: bool,
    ) {
        match ce {
            CE::Class(c) => {
                classes.insert(c.0.to_string());
                if in_filler {
                    filler_classes.insert(c.0.to_string());
                }
            }
            CE::ObjectSomeValuesFrom { ope, bce } | CE::ObjectAllValuesFrom { ope, bce } => {
                if let OPE::ObjectProperty(p) = ope {
                    ops.insert(p.0.to_string());
                }
                walk_ce(bce, classes, filler_classes, ops, true);
            }
            CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => {
                for x in v {
                    walk_ce(x, classes, filler_classes, ops, in_filler);
                }
            }
            CE::ObjectComplementOf(x) => walk_ce(x, classes, filler_classes, ops, in_filler),
            _ => {}
        }
    }

    for ac in ont.iter() {
        for a in ac.ann.iter() {
            ann_props.insert(a.ap.0.to_string());
        }
        match &ac.component {
            Component::DeclareClass(d) => {
                declared_c.insert(d.0 .0.to_string());
            }
            Component::DeclareObjectProperty(d) => {
                declared_o.insert(d.0 .0.to_string());
            }
            Component::DeclareAnnotationProperty(d) => {
                declared_a.insert(d.0 .0.to_string());
            }
            Component::SubClassOf(ax) => {
                walk_ce(&ax.sub, &mut classes, &mut filler_classes, &mut obj_props, false);
                walk_ce(&ax.sup, &mut classes, &mut filler_classes, &mut obj_props, false);
            }
            Component::EquivalentClasses(ax) => {
                for ce in &ax.0 {
                    walk_ce(ce, &mut classes, &mut filler_classes, &mut obj_props, false);
                }
            }
            Component::DisjointClasses(ax) => {
                for ce in &ax.0 {
                    walk_ce(ce, &mut classes, &mut filler_classes, &mut obj_props, false);
                }
            }
            Component::AnnotationAssertion(ax) => {
                ann_props.insert(ax.ann.ap.0.to_string());
            }
            // Annotation properties used only in the ontology header (e.g.
            // `dc:title`, `dc:description`, `dcterms:license`,
            // `oboInOwl:hasOBOFormatVersion`) must still be declared: every
            // referenced annotation property gets a declaration.
            Component::OntologyAnnotation(oa) => {
                ann_props.insert(oa.0.ap.0.to_string());
            }
            _ => {}
        }
    }

    let mut materialised: std::collections::HashSet<String> = Default::default();
    for c in classes.difference(&declared_c) {
        ont.insert(Component::DeclareClass(DeclareClass(b.class(c.as_str()))));
        // A relation's filler is declared by the document itself, so it is not
        // withdrawable; a plain operand is ours to withdraw once the closure is
        // known to type it.
        if !filler_classes.contains(c) {
            materialised.insert(format!("class\u{0}{c}"));
        }
    }
    for p in obj_props.difference(&declared_o) {
        ont.insert(Component::DeclareObjectProperty(DeclareObjectProperty(
            b.object_property(p.as_str()),
        )));
        materialised.insert(format!("op\u{0}{p}"));
    }
    // A built-in property is introduced by the tag that used it, so its
    // declaration is the document's own and no import can stand in for it; one
    // named as a `property_value:` predicate is ours to withdraw once the closure
    // is known to type it.
    let builtin: BTreeSet<String> =
        obo_builtin_annotation_properties().into_iter().map(|(iri, _)| iri).collect();
    for p in ann_props.difference(&declared_a) {
        ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
            b.annotation_property(p.as_str()),
        )));
        if !builtin.contains(p) {
            materialised.insert(format!("ap\u{0}{p}"));
        }
    }
    materialised
}

fn strip_comment(line: &str) -> String {
    // OBO `!` comments apply only *outside* quoted strings and when not escaped
    // (`\!`). A naive cut at " ! " would truncate literals like
    // `"… UBERON:0000091 ! bilaminar disc"`, so track quoting and escaping.
    let bytes = line.as_bytes();
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2; // skip escaped char
                continue;
            }
            b'"' => in_quote = !in_quote,
            b'!' if !in_quote => {
                // A comment: trim trailing whitespace before it.
                return line[..i].trim_end().to_string();
            }
            _ => {}
        }
        i += 1;
    }
    line.to_string()
}

/// Reverse the OBO escaping of an `owl-axioms:` header value
/// (`\` → `\\`, `"` → `\"`, newline → `\n`, tab → `\t`). The forward transform
/// escapes backslashes first, so in the escaped text a `\` only ever introduces
/// one of those four sequences.
fn obo_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// An OBO-native string annotation.
///
/// `xsd:string`, explicitly, NOT `Literal::Simple`. An untyped, language-free
/// literal has two spellings and they are NOT equal: an OBO read produces
/// `xsd:string`, a functional-syntax or RDF/XML read produces `rdf:PlainLiteral`
/// — see `Model::plain_literals_typed`. Both serialize bare, so the difference is
/// invisible until one subject carries literals from BOTH kinds of source, and
/// then it decides their order: literals compare on the datatype IRI first, and
/// `…/1999/02/22-rdf-syntax-ns#PlainLiteral` sorts before
/// `…/2001/XMLSchema#string`.
///
/// OBA is exactly that case. `oba-full.owl` merges `oba-edit.obo` with its import
/// closure collapsed, so a class can hold one `hasExactSynonym` from the OBO edit
/// file and another from the functional `patterns/definitions.owl`; the
/// pattern-derived one comes first whatever the alphabet says — in a two-file
/// merge of `"aaa from obo"` and `"zzz from ofn"`, the OFN literal leads.
///
/// `Literal::Simple` stays what an OFN/RDF-XML read produces, and every writer
/// already renders an `xsd:string` datatype bare, so nothing else changes.
fn ann(b: &Build<RcStr>, prop: &str, value: &str) -> Annotation<RcStr> {
    Annotation { ann: Default::default(),
        ap: b.annotation_property(prop),
        av: AnnotationValue::Literal(Literal::Datatype {
            literal: value.to_string(),
            datatype_iri: b.iri(XSD_STRING_IRI),
        }),
    }
}

/// An annotation whose value is an IRI (e.g. a synonym-type id).
fn ann_iri(b: &Build<RcStr>, prop: &str, iri: &str) -> Annotation<RcStr> {
    Annotation { ann: Default::default(),
        ap: b.annotation_property(prop),
        av: AnnotationValue::IRI(b.iri(iri)),
    }
}

/// Split an unquoted OBO tag value (e.g. `comment:`) into its text and trailing
/// `{qualifier}` block. The qualifier block is not part of the text: it maps to
/// axiom annotations (`{xref=…}` → `oboInOwl:hasDbXref`).
fn split_qualifier_block(v: &str) -> (&str, &str) {
    if v.trim_end().ends_with('}') {
        if let Some(i) = v.rfind('{') {
            return (v[..i].trim_end(), &v[i..]);
        }
    }
    (v, "")
}

fn assert_ann(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, prop: &str, value: &str) {
    ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
        subject: AnnotationSubject::IRI(b.iri(subj)),
        ann: ann(b, prop, value),
    }));
}

/// Assert an annotation assertion carrying its own axiom-level annotations
/// (e.g. an OBO `def:`/`synonym:` line's `[dbxref]` list, which the OBO→OWL
/// mapping turns into `oboInOwl:hasDbXref` annotations on the axiom).
fn assert_ann_with(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    prop: &str,
    value: &str,
    axiom_anns: Vec<Annotation<RcStr>>,
) {
    if axiom_anns.is_empty() {
        assert_ann(b, ont, subj, prop, value);
        return;
    }
    ont.insert(horned_owl::model::AnnotatedComponent {
        component: Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(subj)),
            ann: ann(b, prop, value),
        }),
        ann: axiom_anns.into_iter().collect(),
    });
}

/// Like [`assert_ann_with`] but with an IRI value (e.g. `consider`/`replaced_by`
/// point at another term, and that pointer is an IRI, not a literal id string).
fn assert_ann_iri_with(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    prop: &str,
    iri: &str,
    axiom_anns: Vec<Annotation<RcStr>>,
) {
    if axiom_anns.is_empty() {
        assert_ann_iri(b, ont, subj, prop, iri);
        return;
    }
    ont.insert(horned_owl::model::AnnotatedComponent {
        component: Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(subj)),
            ann: ann_iri(b, prop, iri),
        }),
        ann: axiom_anns.into_iter().collect(),
    });
}

/// Parse the OBO trailing `[xref, xref, ...]` list from the remainder of a
/// `def:`/`synonym:` value (the part after the quoted string). Each entry's id
/// is the leading token (an optional quoted description is ignored). Returns the
/// xref ids in document order.
fn parse_bracket_xrefs(rest: &str) -> Vec<String> {
    let start = match rest.find('[') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let end = match rest[start..].find(']') {
        Some(i) => start + i,
        None => return Vec::new(),
    };
    let inner = &rest[start + 1..end];
    // Split on *unescaped* commas (an `\,` is part of the id, e.g.
    // `ISBN:9004086161\,9789004086166` is a single xref), then take each id up to
    // the first *unescaped* whitespace or `"` (a trailing description) and
    // unescape OBO escapes (`\:`→`:`, `\,`→`,`, …).
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut esc = false;
    for c in inner.chars() {
        if esc {
            cur.push('\\');
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == ',' {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
        .iter()
        .filter_map(|part| {
            // The xref id is everything up to an unescaped `"` (a trailing quoted
            // description), trimmed. Internal whitespace stays part of the id
            // (e.g. the erroneous `PMID: 5466382`), so do NOT split on it.
            let mut id = String::new();
            let mut esc = false;
            for c in part.chars() {
                if esc {
                    id.push(c);
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    break;
                } else {
                    id.push(c);
                }
            }
            let id = id.trim().to_string();
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        })
        .collect()
}

/// A single `xref:` line's trailing quoted description (`xref: CARO:0000030
/// "asexual organism"`) becomes an `rdfs:label` axiom annotation on the
/// `hasDbXref` axiom.
fn xref_label_ann(b: &Build<RcStr>, x: &str) -> Vec<Annotation<RcStr>> {
    let after = x.splitn(2, char::is_whitespace).nth(1).unwrap_or("").trim_start();
    match parse_quoted(after) {
        Some((label, _)) => vec![ann(b, RDFS_LABEL, &label)],
        None => Vec::new(),
    }
}

/// Build `oboInOwl:hasDbXref` axiom annotations for each xref in an OBO
/// `[dbxref]` list found in the remainder `rest` (the text after the quoted
/// string).
fn dbxref_anns(b: &Build<RcStr>, rest: &str) -> Vec<Annotation<RcStr>> {
    parse_bracket_xrefs(rest)
        .iter()
        .map(|x| ann(b, &format!("{OIO}hasDbXref"), x))
        .collect()
}

/// Parse an OBO trailing-qualifier block `{key="value", key2="value2", …}` from
/// the end of a tag value. Returns the `(key, value)` pairs in document order
/// (a key may repeat, e.g. several `source=`). Values are OBO-quoted.
fn parse_qualifiers(rest: &str) -> Vec<(String, String)> {
    let start = match rest.rfind('{') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let end = match rest[start..].rfind('}') {
        Some(i) => start + i,
        None => return Vec::new(),
    };
    let mut inner = &rest[start + 1..end];
    let mut out = Vec::new();
    while let Some(eq) = inner.find('=') {
        let key = inner[..eq].trim_matches(|c: char| c.is_whitespace() || c == ',').trim();
        let after = inner[eq + 1..].trim_start();
        if let Some((val, tail)) = parse_quoted(after) {
            if !key.is_empty() {
                out.push((key.to_string(), val));
            }
            inner = tail;
        } else {
            break;
        }
    }
    out
}

/// Map an OBO qualifier key to its annotation property IRI. A CURIE key
/// (`OMO:0002001`, an evidence property) expands as an IRI; a bare key
/// (`source`) is a qualifier shorthand and lives in the oboInOwl namespace.
fn qualifier_prop(key: &str) -> String {
    if key.contains(':') {
        expand_curie(key)
    } else if key == "xref" {
        // A trailing `{xref=…}` qualifier is oboInOwl:hasDbXref.
        format!("{OIO}hasDbXref")
    } else if key == "comment" {
        RDFS_COMMENT.to_string()
    } else if key == "seeAlso" {
        "http://www.w3.org/2000/01/rdf-schema#seeAlso".to_string()
    } else if key == "scope" {
        // A `{scope=…}` qualifier is oboInOwl:hasScope.
        format!("{OIO}hasScope")
    } else if key == "def" {
        // a `{def=…}` qualifier (a definition-source URL) maps to IAO_0000115.
        IAO_DEF.to_string()
    } else {
        format!("{OIO}{key}")
    }
}

/// Axiom-level annotations for a tag's trailing `{…}` qualifier block.
fn qualifier_anns(b: &Build<RcStr>, rest: &str) -> Vec<Annotation<RcStr>> {
    parse_qualifiers(rest)
        .iter()
        // Cardinality qualifiers are consumed into the class expression
        // (`relation_ce` builds the qualified cardinality restriction for an
        // `intersection_of` genus-differentia, and the relationship handler emits
        // the existential + the separate exact axiom / min-max annotation), and
        // gci_relation/gci_filler into the GCI subject — none are re-emitted as a
        // generic annotation here.
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "cardinality"
                    | "minCardinality"
                    | "maxCardinality"
                    | "min_cardinality"
                    | "max_cardinality"
                    | "gci_relation"
                    | "gci_filler"
            )
        })
        .map(|(k, v)| ann(b, &qualifier_prop(k), v))
        .collect()
}

/// Build the class expression for an OBO `relationship`/`intersection_of`
/// operand `R filler`. A `{cardinality|minCardinality|maxCardinality=N}`
/// qualifier becomes the corresponding qualified cardinality restriction rather
/// than a plain existential: cardinality is outside OWL 2 EL, so an EL reasoner
/// ignores the restriction entirely. Emitting `∃R.filler` instead adds a
/// constraint the reasoner DOES see, which spuriously makes classes unsatisfiable
/// against the spatial/BFO disjointness axioms.
fn relation_ce(b: &Build<RcStr>, rel_iri: String, filler: String, rest: &str) -> CE<RcStr> {
    let ope = OPE::ObjectProperty(b.object_property(rel_iri));
    let bce = Box::new(CE::Class(b.class(filler)));
    for (k, v) in parse_qualifiers(rest) {
        if let Ok(n) = v.parse::<u32>() {
            match k.as_str() {
                "cardinality" => return CE::ObjectExactCardinality { n, ope, bce },
                "minCardinality" | "min_cardinality" => {
                    return CE::ObjectMinCardinality { n, ope, bce }
                }
                "maxCardinality" | "max_cardinality" => {
                    return CE::ObjectMaxCardinality { n, ope, bce }
                }
                _ => {}
            }
        }
    }
    CE::ObjectSomeValuesFrom { ope, bce }
}

/// Insert a logical axiom carrying axiom-level annotations (e.g. `is_a`/
/// `relationship` lines whose `{source=…}` qualifiers map to annotations).
fn insert_annotated(
    ont: &mut SetOntology<RcStr>,
    component: Component<RcStr>,
    anns: Vec<Annotation<RcStr>>,
) {
    if anns.is_empty() {
        ont.insert(component);
    } else {
        ont.insert(horned_owl::model::AnnotatedComponent {
            component,
            ann: anns.into_iter().collect(),
        });
    }
}

/// Assert an annotation whose value is an IRI (rather than a literal).
fn assert_ann_iri(b: &Build<RcStr>, ont: &mut SetOntology<RcStr>, subj: &str, prop: &str, iri: &str) {
    ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
        subject: AnnotationSubject::IRI(b.iri(subj)),
        ann: Annotation { ann: Default::default(),
            ap: b.annotation_property(prop),
            av: AnnotationValue::IRI(b.iri(iri)),
        },
    }));
}

/// Assert an annotation whose value is a datatyped literal.
fn assert_ann_typed(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    prop: &str,
    value: &str,
    datatype: &str,
) {
    ont.insert(Component::AnnotationAssertion(AnnotationAssertion {
        subject: AnnotationSubject::IRI(b.iri(subj)),
        ann: Annotation { ann: Default::default(),
            ap: b.annotation_property(prop),
            av: AnnotationValue::Literal(Literal::Datatype {
                literal: value.to_string(),
                datatype_iri: b.iri(datatype),
            }),
        },
    }));
}

/// Emit an OBO `property_value:` tag as an annotation assertion. Forms:
///   `property_value: REL "literal" DATATYPE`  → datatyped literal
///   `property_value: REL "literal"`           → plain literal
///   `property_value: REL TARGET_ID`           → IRI value
/// Parse an OBO `property_value` body into its `Annotation` plus any trailing
/// `{…}` qualifier annotations. Used both for term/typedef assertions and for
/// the ontology header (where it becomes an `OntologyAnnotation`).
fn property_value_annotation(
    b: &Build<RcStr>,
    value: &str,
    rel_map: &HashMap<String, String>,
    onto_ns: Option<&str>,
    nl_to_space: bool,
) -> Option<(Annotation<RcStr>, Vec<Annotation<RcStr>>)> {
    let v = value.trim();
    let (rel, rest) = v.split_once(char::is_whitespace).map(|(r, rest)| (r, rest.trim()))?;
    // An undeclared, unprefixed relation (e.g. `seeAlso`) is an ontology-local
    // annotation property `obo/<ontology>#<rel>`, not the generic `obo/<rel>`.
    let prop = rel_map.get(rel).cloned().unwrap_or_else(|| match onto_ns {
        Some(ns) if !rel.contains(':') && !rel.starts_with("http") => format!("{ns}{rel}"),
        _ => expand_curie(rel),
    });
    let anns = qualifier_anns(b, value);
    // `nl_to_space` is the stanza-level flag the callers thread in (a Term and the
    // header pass `true`; a Typedef passes whether the value has a space-adjacent
    // `\n`). An OBO `\n` unescapes to a literal newline in every case — see
    // `parse_quoted_nl` — so it does not change the value built here.
    let av = if let Some((lit, after)) = parse_quoted_nl(rest, nl_to_space) {
        let dt = after.trim().split_whitespace().next().unwrap_or("");
        if dt.is_empty() || dt == "xsd:string" || dt.starts_with('{') {
            // Explicitly `xsd:string`, for the reason spelled out on `ann`.
            AnnotationValue::Literal(Literal::Datatype {
                literal: lit,
                datatype_iri: b.iri(XSD_STRING_IRI),
            })
        } else {
            AnnotationValue::Literal(Literal::Datatype {
                literal: lit,
                datatype_iri: b.iri(expand_datatype(dt)),
            })
        }
    } else {
        let target = rest.split_whitespace().next().unwrap_or(rest);
        AnnotationValue::IRI(b.iri(expand_curie(target)))
    };
    Some((Annotation { ann: Default::default(), ap: b.annotation_property(prop.as_str()), av }, anns))
}

fn assert_property_value(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    subj: &str,
    value: &str,
    rel_map: &HashMap<String, String>,
    onto_ns: Option<&str>,
    nl_to_space: bool,
) {
    if let Some((ann, anns)) = property_value_annotation(b, value, rel_map, onto_ns, nl_to_space) {
        insert_annotated(
            ont,
            Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(subj)),
                ann,
            }),
            anns,
        );
    }
}

/// Expand a datatype id. Built-in prefixes (`xsd`/`rdf`/`rdfs`/`owl`) map to
/// their standard namespaces; everything else uses the OBO PURL convention.
fn expand_datatype(dt: &str) -> String {
    match dt.split_once(':') {
        Some(("xsd", local)) => format!("http://www.w3.org/2001/XMLSchema#{local}"),
        Some(("rdf", local)) => format!("http://www.w3.org/1999/02/22-rdf-syntax-ns#{local}"),
        Some(("rdfs", local)) => format!("http://www.w3.org/2000/01/rdf-schema#{local}"),
        Some(("owl", local)) => format!("http://www.w3.org/2002/07/owl#{local}"),
        _ => expand_id(dt),
    }
}

/// Parse an OBO-quoted string at the start of `s` (which must begin with `"`),
/// honoring `\"`/`\\`/`\n`/`\t` escapes and multi-byte characters. Returns the
/// unescaped content and the remainder after the closing quote.
/// Unescape an OBO string value (`\n`→newline, `\t`→tab, `\W`→space, and a
/// backslash before any other char drops the backslash). Unquoted values such as
/// `comment:` are unescaped too.
fn unescape_obo(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            out.push(match c {
                // An OBO `\n` escape inside a quoted value (def, comment, synonym,
                // property_value) is a LITERAL NEWLINE in the OWL literal, in a
                // Term and a Typedef alike — the text genuinely spans lines, and
                // collapsing it to a space would rewrite the curator's wording.
                'n' => '\n',
                't' => '\t',
                'W' => ' ',
                other => other,
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c);
        }
    }
    if escaped {
        out.push('\\');
    }
    out
}

fn parse_quoted(s: &str) -> Option<(String, &str)> {
    parse_quoted_nl(s, true)
}

/// Like [`parse_quoted`], but taking the `nl_to_space` flag callers thread through
/// for the Term/Typedef distinction. An OBO `\n` escape becomes a literal newline
/// in either stanza kind, so the flag does not change what this parses.
fn parse_quoted_nl(s: &str, nl_to_space: bool) -> Option<(String, &str)> {
    let mut chars = s.char_indices();
    if chars.next().map(|(_, c)| c) != Some('"') {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for (idx, c) in chars {
        if escaped {
            out.push(match c {
                // Always a literal newline — see the note in `unescape_obo`; `\n`
                // is kept in Term AND Typedef quoted values alike, so `nl_to_space`
                // does not select between them.
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some((out, &s[idx + c.len_utf8()..]));
        } else {
            out.push(c);
        }
    }
    None
}

fn synonym_property(value: &str) -> &'static str {
    // synonym: "text" SCOPE [xrefs]
    let after = parse_quoted(value.trim()).map(|(_, a)| a).unwrap_or("");
    let scope = after.trim().split_whitespace().next().unwrap_or("");
    match scope {
        "EXACT" => "hasExactSynonym",
        "NARROW" => "hasNarrowSynonym",
        "BROAD" => "hasBroadSynonym",
        _ => "hasRelatedSynonym",
    }
}

/// Resolve an OBO subset/synonym-type name to an IRI: prefixed ids
/// (`OMO:0003011`) expand normally; bare local names (`efo_slim`, `SENSU`) live
/// in the ontology's own namespace (`onto_ns`).
fn resolve_local(name: &str, onto_ns: Option<&str>) -> String {
    if name.contains(':') {
        expand_curie(name)
    } else if let Some(ns) = onto_ns {
        format!("{ns}{name}")
    } else {
        expand_id(name)
    }
}

/// Resolve an OBO relation reference to its property IRI, honoring relation
/// shorthands (`disease_has_basis_in_dysfunction_of` ⇒ `RO_0004020`).
fn resolve_rel(r: &str, rel_map: &HashMap<String, String>) -> String {
    rel_map.get(r).cloned().unwrap_or_else(|| expand_id(r))
}

/// The IRI of a `[Typedef]` with a bare (non-CURIE) `id`: its single `xref`
/// (a full IRI as-is, a CURIE expanded) if present, else the OBO default
/// namespace `obo/<id>` — an unprefixed relation id maps to the default OBO PURL
/// prefix plus the id (e.g. `part_of` → `obo/part_of`), the same mapping
/// [`expand_id`]/`resolve_rel` apply to a `relationship:` reference, so the
/// typedef and its uses resolve to one IRI.
fn typedef_iri(id: &str, xref: Option<&str>, onto_ns: Option<&str>) -> String {
    if let Some(x) = xref {
        let x = x.split_whitespace().next().unwrap_or(x);
        if x.starts_with("http") {
            return x.to_string();
        }
        if x.contains(':') {
            return expand_id(x);
        }
    }
    // A bare-name property with no usable xref is ontology-native: it lives in the
    // ontology's own namespace (`obo/uberon/core#extends_fibers_into`), not the
    // generic `obo/` namespace.
    resolve_local(id, onto_ns)
}

fn term_to_owl(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    st: &Stanza,
    default_ns: Option<&str>,
    onto_ns: Option<&str>,
    rel_map: &HashMap<String, String>,
    metadata_tags: &BTreeSet<String>,
) {
    let id = match st.get("id") {
        Some(i) => i,
        None => return,
    };
    let iri = expand_id(id);
    ont.insert(Component::DeclareClass(DeclareClass(b.class(iri.clone()))));

    // Every term carries its OBO id as an `oboInOwl:id` annotation.
    assert_ann(b, ont, &iri, &format!("{OIO}id"), id);

    // EVERY `name:` clause, not just the first. One line is written per
    // `rdfs:label` (see the `name` emission), so a stanza can legitimately carry
    // several: GO:0051705 in MONDO has both "multi-organism behavior" and
    // "obsolete multi-organism behavior". Reading only the first would halve them
    // on an obo→obo trip, which MONDO's build performs — a step re-reads the
    // written target and the artefact's write re-serialises it.
    for name in st.all("name") {
        assert_ann(b, ont, &iri, RDFS_LABEL, name);
    }
    // `hasOBONamespace`: explicit `namespace`, else the header default.
    if let Some(ns) = st.get("namespace").or(default_ns) {
        assert_ann(b, ont, &iri, &format!("{OIO}hasOBONamespace"), ns);
    }
    for cb in st.all("created_by") {
        assert_ann(b, ont, &iri, &format!("{OIO}created_by"), cb);
    }
    for cd in st.all("creation_date") {
        assert_ann(b, ont, &iri, &format!("{OIO}creation_date"), cd);
    }
    for pv in st.all("property_value") {
        // Term (class) property_value. The stanza flag is `true` here, but a `\n` in
        // the value unescapes to a literal newline either way.
        assert_property_value(b, ont, &iri, pv, rel_map, onto_ns, true);
    }
    if let Some(raw) = st.get("def") {
        if let Some((def, rest)) = parse_quoted(raw.trim()) {
            let mut anns = dbxref_anns(b, rest);
            anns.extend(qualifier_anns(b, rest));
            assert_ann_with(b, ont, &iri, IAO_DEF, &def, anns);
        }
    }
    // Every `comment:` line, not just the first — CL has 28 terms carrying two
    // (e.g. alveolar macrophage's marker-set note alongside its morphology note),
    // and taking only the first would drop them on an obo→owl→obo trip.
    for c in st.all("comment") {
        // A trailing `{xref=…}` qualifier on a comment becomes hasDbXref axiom
        // annotations, stripped from the comment text.
        let (text, quals) = split_qualifier_block(c);
        let anns = qualifier_anns(b, quals);
        assert_ann_with(b, ont, &iri, RDFS_COMMENT, &unescape_obo(text), anns);
    }
    for syn in st.all("synonym") {
        let prop = synonym_property(syn);
        if let Some((text, rest)) = parse_quoted(syn.trim()) {
            // `synonym: "text" SCOPE [TYPEID] [xrefs]` — a synonym-type id may sit
            // between the scope and the `[xref]` list; map it to a
            // `hasSynonymType` annotation (matching the OBO→OWL mapping).
            let mut anns = dbxref_anns(b, rest);
            let before_brackets = rest.split('[').next().unwrap_or("");
            let mut toks = before_brackets.split_whitespace();
            toks.next(); // skip the scope token (EXACT/NARROW/…)
            if let Some(type_id) = toks.next() {
                anns.push(ann_iri(
                    b,
                    &format!("{OIO}hasSynonymType"),
                    &resolve_local(type_id, onto_ns),
                ));
            }
            anns.extend(qualifier_anns(b, rest));
            assert_ann_with(b, ont, &iri, &format!("{OIO}{prop}"), &text, anns);
        }
    }
    for x in st.all("xref") {
        let id = x.split_whitespace().next().unwrap_or(x);
        // OBO escapes in the xref id are unescaped (e.g. an escaped colon
        // `Category\:Embryonic` → `Category:Embryonic`).
        let id = unescape_obo(id);
        let mut anns = xref_label_ann(b, x);
        anns.extend(qualifier_anns(b, x));
        assert_ann_with(b, ont, &iri, &format!("{OIO}hasDbXref"), &id, anns);
    }
    for s in st.all("subset") {
        let sid = s.split_whitespace().next().unwrap_or(s);
        insert_annotated(
            ont,
            Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(iri.as_str())),
                ann: ann_iri(b, &format!("{OIO}inSubset"), &resolve_local(sid, onto_ns)),
            }),
            qualifier_anns(b, s),
        );
    }
    // `is_obsolete: true` (possibly with a trailing `{source=…}` qualifier, which
    // becomes an axiom annotation on the owl:deprecated assertion).
    if let Some(v) = st.get("is_obsolete") {
        if v.split_whitespace().next() == Some("true") {
            insert_annotated(
                ont,
                Component::AnnotationAssertion(AnnotationAssertion {
                    subject: AnnotationSubject::IRI(b.iri(iri.as_str())),
                    ann: Annotation { ann: Default::default(),
                        ap: b.annotation_property(OWL_DEPRECATED),
                        av: AnnotationValue::Literal(Literal::Datatype {
                            literal: "true".to_string(),
                            datatype_iri: b.iri(XSD_BOOLEAN),
                        }),
                    },
                }),
                qualifier_anns(b, v),
            );
        }
    }
    // Obsolescence pointers: a term's own `replaced_by:`/`consider:` tags point
    // at other terms, and that pointer is an **IRI** (e.g.
    // `<obo/UBERON_0000965>`), not a literal id string.
    for rb in st.all("replaced_by") {
        let t = rb.split_whitespace().next().unwrap_or(rb);
        assert_ann_iri_with(b, ont, &iri, IAO_TERM_REPLACED_BY, &expand_id(t), qualifier_anns(b, rb));
    }
    for c in st.all("consider") {
        let t = c.split_whitespace().next().unwrap_or(c);
        assert_ann_iri_with(b, ont, &iri, &format!("{OIO}consider"), &expand_id(t), qualifier_anns(b, c));
    }
    for a in st.all("alt_id") {
        let t = a.split_whitespace().next().unwrap_or(a);
        assert_ann(b, ont, &iri, &format!("{OIO}hasAlternativeId"), t);
        // Each alt_id is also materialised as its own *deprecated* class, merged
        // into (replaced_by) the primary term with obsolescence reason "terms
        // merged": owl:deprecated + IAO_0100001 + IAO_0000231.
        let alt = expand_id(t);
        if alt != *iri {
            ont.insert(Component::DeclareClass(DeclareClass(b.class(alt.as_str()))));
            assert_ann_typed(b, ont, &alt, OWL_DEPRECATED, "true", XSD_BOOLEAN);
            assert_ann_iri(b, ont, &alt, IAO_TERM_REPLACED_BY, &iri);
            assert_ann_iri(b, ont, &alt, IAO_OBSOLESCENCE_REASON, IAO_TERMS_MERGED);
        }
    }

    // Logical axioms.
    for parent in st.all("is_a") {
        let pid = parent.split_whitespace().next().unwrap_or(parent);
        // A `gci_relation`/`gci_filler` qualifier turns the `is_a` into a General
        // Class Inclusion: `(C ⊓ gci_rel some gci_filler) ⊑ parent` — the
        // subsumption holds only within the given taxon/context, not
        // unconditionally. Without this the class is wrongly placed under
        // `parent` for every individual.
        let quals = parse_qualifiers(parent);
        let gci_rel = quals.iter().find(|(k, _)| k == "gci_relation").map(|(_, v)| v);
        let gci_fill = quals.iter().find(|(k, _)| k == "gci_filler").map(|(_, v)| v);
        let sub = match (gci_rel, gci_fill) {
            (Some(gr), Some(gf)) => CE::ObjectIntersectionOf(vec![
                CE::Class(b.class(iri.clone())),
                CE::ObjectSomeValuesFrom {
                    ope: OPE::ObjectProperty(b.object_property(resolve_rel(gr, rel_map))),
                    bce: Box::new(CE::Class(b.class(expand_id(gf)))),
                },
            ]),
            _ => CE::Class(b.class(iri.clone())),
        };
        insert_annotated(
            ont,
            Component::SubClassOf(SubClassOf {
                sub,
                sup: CE::Class(b.class(expand_id(pid))),
            }),
            qualifier_anns(b, parent),
        );
    }
    // Each `relationship:` line yields exactly ONE axiom, and `{all_only="true"}`
    // selects the universal form:
    //   `relationship: R X`                     → SubClassOf(C ObjectSomeValuesFrom(R X))
    //   `relationship: R X {all_only="true"}`   → SubClassOf(C ObjectAllValuesFrom(R X))
    // both UNANNOTATED. A frame carrying BOTH lines for one (R, X) therefore emits
    // BOTH axioms (the OBO "all-some" translation), and a frame carrying only the
    // qualified line emits ONLY the universal axiom. Deduping the pair into one
    // `some` carrying an `oboInOwl:all_only` annotation would produce a spurious
    // existential and drop the universal.
    let deduped_rels: Vec<&str> = st.all("relationship").collect();
    for rel in deduped_rels {
        let mut parts = rel.split_whitespace();
        if let (Some(r), Some(target)) = (parts.next(), parts.next()) {
            let rel_iri = resolve_rel(r, rel_map);
            if metadata_tags.contains(&rel_iri) {
                // A metadata-tag relationship is an annotation assertion (IRI value).
                insert_annotated(
                    ont,
                    Component::AnnotationAssertion(AnnotationAssertion {
                        subject: AnnotationSubject::IRI(b.iri(iri.as_str())),
                        ann: ann_iri(b, &rel_iri, &expand_id(target)),
                    }),
                    qualifier_anns(b, rel),
                );
            } else {
                // A `gci_relation`/`gci_filler` qualifier makes the line a General
                // Class Inclusion: `(X ⊓ gci_rel some gci_filler) ⊑ R some target`
                // — the relationship holds only within the given taxon/context, not
                // unconditionally. Applying it unconditionally (as `X ⊑ R some
                // target`) over-constrains X and spuriously makes it (and
                // everything that conflicts) unsatisfiable.
                let quals = parse_qualifiers(rel);
                let gci_rel = quals.iter().find(|(k, _)| k == "gci_relation").map(|(_, v)| v);
                let gci_fill = quals.iter().find(|(k, _)| k == "gci_filler").map(|(_, v)| v);
                let sub = match (gci_rel, gci_fill) {
                    (Some(gr), Some(gf)) => CE::ObjectIntersectionOf(vec![
                        CE::Class(b.class(iri.clone())),
                        CE::ObjectSomeValuesFrom {
                            ope: OPE::ObjectProperty(b.object_property(resolve_rel(gr, rel_map))),
                            bce: Box::new(CE::Class(b.class(expand_id(gf)))),
                        },
                    ]),
                    _ => CE::Class(b.class(iri.clone())),
                };
                let target_iri = expand_id(target);
                // The relationship's primary axiom is the existential `R some F`. The
                // *snake_case* `min_cardinality`/`max_cardinality` qualifiers are
                // non-standard and ride along as `oboInOwl:*` axiom annotations on
                // that existential, which is still emitted; the standard *camelCase*
                // `cardinality`/`minCardinality`/`maxCardinality` qualifiers replace
                // it with a separate qualified-cardinality axiom (below).
                // `all_only` selects the universal form and is NOT carried as an
                // axiom annotation — it is a translation marker rather than curated
                // content, and the obo writer re-derives the `{all_only="true"}`
                // qualifier from the ObjectAllValuesFrom twin.
                let all_only = parse_qualifiers(rel)
                    .iter()
                    .any(|(k, v)| k == "all_only" && v == "true");
                let mut rel_anns: Vec<_> = qualifier_anns(b, rel)
                    .into_iter()
                    .filter(|a| a.ap.0.as_ref() != format!("{OIO}all_only"))
                    .collect();
                for (k, v) in parse_qualifiers(rel) {
                    match k.as_str() {
                        "min_cardinality" => rel_anns.push(ann(b, &format!("{OIO}min_cardinality"), &v)),
                        "max_cardinality" => rel_anns.push(ann(b, &format!("{OIO}max_cardinality"), &v)),
                        _ => {}
                    }
                }
                // A STANDARD camelCase `{cardinality|minCardinality|maxCardinality}`
                // qualifier REPLACES the existential: only the
                // qualified-cardinality axiom is emitted. The
                // snake_case `min_cardinality`/`max_cardinality` spellings are
                // non-standard and instead ride along as annotations on the
                // existential, which is still emitted.
                let has_std_cardinality = parse_qualifiers(rel).iter().any(|(k, v)| {
                    matches!(k.as_str(), "cardinality" | "minCardinality" | "maxCardinality")
                        && v.parse::<u32>().is_ok()
                });
                if !has_std_cardinality {
                    let ope = OPE::ObjectProperty(b.object_property(rel_iri.clone()));
                    let bce = Box::new(CE::Class(b.class(target_iri.clone())));
                    let sup = if all_only {
                        CE::ObjectAllValuesFrom { ope, bce }
                    } else {
                        CE::ObjectSomeValuesFrom { ope, bce }
                    };
                    insert_annotated(
                        ont,
                        Component::SubClassOf(SubClassOf { sub: sub.clone(), sup }),
                        rel_anns,
                    );
                }
                // A standard `{cardinality|minCardinality|maxCardinality="N"}`
                // qualifier → an unannotated qualified-cardinality axiom, standing
                // in for the existential skipped above. It is outside OWL 2 EL, so
                // an EL reasoner ignores it.
                for (k, v) in parse_qualifiers(rel) {
                    let n: u32 = match v.parse() { Ok(n) => n, Err(_) => continue };
                    let ope = OPE::ObjectProperty(b.object_property(rel_iri.clone()));
                    let bce = Box::new(CE::Class(b.class(target_iri.clone())));
                    let sup = match k.as_str() {
                        "cardinality" => CE::ObjectExactCardinality { n, ope, bce },
                        "minCardinality" => CE::ObjectMinCardinality { n, ope, bce },
                        "maxCardinality" => CE::ObjectMaxCardinality { n, ope, bce },
                        _ => continue,
                    };
                    ont.insert(Component::SubClassOf(SubClassOf { sub: sub.clone(), sup }));
                }
            }
        }
    }
    for d in st.all("disjoint_from") {
        let did = d.split_whitespace().next().unwrap_or(d);
        insert_annotated(
            ont,
            Component::DisjointClasses(DisjointClasses(vec![
                CE::Class(b.class(iri.clone())),
                CE::Class(b.class(expand_id(did))),
            ])),
            qualifier_anns(b, d),
        );
    }
    // intersection_of lines combine into one EquivalentClasses(X, And(...)).
    let inter: Vec<&str> = st.all("intersection_of").collect();
    if !inter.is_empty() {
        let mut conj: Vec<CE<RcStr>> = Vec::new();
        let mut anns: Vec<Annotation<RcStr>> = Vec::new();
        for line in inter {
            anns.extend(qualifier_anns(b, line));
            // Strip the trailing `{…}` qualifier block before tokenizing operands.
            let body = line.split('{').next().unwrap_or(line);
            let toks: Vec<&str> = body.split_whitespace().collect();
            match toks.as_slice() {
                [genus] => conj.push(CE::Class(b.class(expand_id(genus)))),
                [rel, filler] => conj.push(relation_ce(
                    b,
                    resolve_rel(rel, rel_map),
                    expand_id(filler),
                    line,
                )),
                _ => {}
            }
        }
        if conj.len() >= 2 {
            insert_annotated(
                ont,
                Component::EquivalentClasses(EquivalentClasses(vec![
                    CE::Class(b.class(iri.clone())),
                    CE::ObjectIntersectionOf(conj),
                ])),
                anns,
            );
        }
    }
    // union_of lines combine into one EquivalentClasses(X, Or(...)).
    let union: Vec<&str> = st.all("union_of").collect();
    if union.len() >= 2 {
        let mut disj: Vec<CE<RcStr>> = Vec::new();
        let mut anns: Vec<Annotation<RcStr>> = Vec::new();
        for line in union {
            anns.extend(qualifier_anns(b, line));
            let body = line.split('{').next().unwrap_or(line);
            if let Some(member) = body.split_whitespace().next() {
                disj.push(CE::Class(b.class(expand_id(member))));
            }
        }
        if disj.len() >= 2 {
            insert_annotated(
                ont,
                Component::EquivalentClasses(EquivalentClasses(vec![
                    CE::Class(b.class(iri.clone())),
                    CE::ObjectUnionOf(disj),
                ])),
                anns,
            );
        }
    }
    for eq in st.all("equivalent_to") {
        let eq = eq.split_whitespace().next().unwrap_or(eq);
        ont.insert(Component::EquivalentClasses(EquivalentClasses(vec![
            CE::Class(b.class(iri.clone())),
            CE::Class(b.class(expand_id(eq))),
        ])));
    }
}

fn typedef_to_owl(
    b: &Build<RcStr>,
    ont: &mut SetOntology<RcStr>,
    st: &Stanza,
    default_ns: Option<&str>,
    onto_ns: Option<&str>,
    rel_map: &HashMap<String, String>,
    metadata_tags: &BTreeSet<String>,
) {
    let id = match st.get("id") {
        Some(i) => i,
        None => return,
    };
    // Use the same resolved IRI relation usages do (xref / ontology namespace).
    let iri = resolve_rel(id, rel_map);
    // Metadata tags are annotation properties, everything else an object property.
    if metadata_tags.contains(&iri) {
        ont.insert(Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
            b.annotation_property(iri.as_str()),
        )));
    } else {
        ont.insert(Component::DeclareObjectProperty(DeclareObjectProperty(
            b.object_property(iri.clone()),
        )));
    }
    assert_ann(b, ont, &iri, &format!("{OIO}id"), id);
    // Every `name:` clause — see the term reader above.
    for name in st.all("name") {
        assert_ann(b, ont, &iri, RDFS_LABEL, name);
    }
    if let Some(ns) = st.get("namespace").or(default_ns) {
        assert_ann(b, ont, &iri, &format!("{OIO}hasOBONamespace"), ns);
    }
    if let Some(raw) = st.get("def") {
        if let Some((def, rest)) = parse_quoted(raw.trim()) {
            // Carry the def's trailing `[dbxref]` list (and any `{qualifier}`) as
            // axiom annotations, exactly as terms do: a typedef's IAO_0000115
            // carries them too.
            let mut anns = dbxref_anns(b, rest);
            anns.extend(qualifier_anns(b, rest));
            assert_ann_with(b, ont, &iri, IAO_DEF, &def, anns);
        }
    }
    // Every `comment:` line, not just the first — CL has 28 terms carrying two
    // (e.g. alveolar macrophage's marker-set note alongside its morphology note),
    // and taking only the first would drop them on an obo→owl→obo trip.
    for c in st.all("comment") {
        // A trailing `{xref=…}` qualifier on a comment becomes hasDbXref axiom
        // annotations, stripped from the comment text.
        let (text, quals) = split_qualifier_block(c);
        let anns = qualifier_anns(b, quals);
        assert_ann_with(b, ont, &iri, RDFS_COMMENT, &unescape_obo(text), anns);
    }
    // A bare-name id is an OBO `shorthand` *only* when an `xref` remaps it to an
    // external IRI (e.g. `aboral_to` → `BSPO_0015202`): the bare name then aliases
    // that IRI. A bare id with no such xref simply lives in the ontology's own
    // namespace (`<onto>#<id>`) and is the IRI's local name, not a shorthand — so
    // no `oboInOwl:shorthand` is emitted for it.
    let remapped_by_xref = st.get("xref").is_some_and(|x| {
        let x = x.split_whitespace().next().unwrap_or(x);
        x.starts_with("http") || x.contains(':')
    });
    if !id.contains(':') && remapped_by_xref {
        assert_ann(b, ont, &iri, &format!("{OIO}shorthand"), id);
    }
    for x in st.all("xref") {
        let id = unescape_obo(x.split_whitespace().next().unwrap_or(x));
        if !id.is_empty() {
            let mut anns = xref_label_ann(b, x);
            anns.extend(qualifier_anns(b, x));
            assert_ann_with(b, ont, &iri, &format!("{OIO}hasDbXref"), &id, anns);
        }
    }
    if st.get("is_metadata_tag") == Some("true") {
        assert_ann_typed(b, ont, &iri, &format!("{OIO}is_metadata_tag"), "true", XSD_BOOLEAN);
    }
    if st.get("is_class_level") == Some("true") {
        assert_ann_typed(b, ont, &iri, &format!("{OIO}is_class_level"), "true", XSD_BOOLEAN);
    }
    for cb in st.all("created_by") {
        assert_ann(b, ont, &iri, &format!("{OIO}created_by"), cb);
    }
    for cd in st.all("creation_date") {
        assert_ann(b, ont, &iri, &format!("{OIO}creation_date"), cd);
    }
    for pv in st.all("property_value") {
        // Typedef (object-property) property_value: an OBO `\n` stays a literal
        // newline (e.g. BFO_0000050/51's parthood comment). `space_adjacent_nl`
        // marks a value whose newline sits next to a space (` \n` / `\n `, as in
        // RO_0002410's bulleted causal-relations comment); it reaches
        // `parse_quoted_nl` as the stanza flag, which keeps the newline regardless.
        let space_adjacent_nl = pv.contains(" \\n") || pv.contains("\\n ");
        assert_property_value(b, ont, &iri, pv, rel_map, onto_ns, space_adjacent_nl);
    }
    // Obsolescence on a typedef behaves exactly as on a term: `owl:deprecated true`
    // plus the obsolescence pointers (`replaced_by` → IAO_0100001, `consider` →
    // oboInOwl:consider) on the property. Without this the deprecation metadata the
    // release carries is lost.
    if let Some(v) = st.get("is_obsolete") {
        if v.split_whitespace().next() == Some("true") {
            insert_annotated(
                ont,
                Component::AnnotationAssertion(AnnotationAssertion {
                    subject: AnnotationSubject::IRI(b.iri(iri.as_str())),
                    ann: Annotation { ann: Default::default(),
                        ap: b.annotation_property(OWL_DEPRECATED),
                        av: AnnotationValue::Literal(Literal::Datatype {
                            literal: "true".to_string(),
                            datatype_iri: b.iri(XSD_BOOLEAN),
                        }),
                    },
                }),
                qualifier_anns(b, v),
            );
        }
    }
    for rb in st.all("replaced_by") {
        let t = rb.split_whitespace().next().unwrap_or(rb);
        assert_ann_iri_with(b, ont, &iri, IAO_TERM_REPLACED_BY, &expand_id(t), qualifier_anns(b, rb));
    }
    for c in st.all("consider") {
        let t = c.split_whitespace().next().unwrap_or(c);
        assert_ann_iri_with(b, ont, &iri, &format!("{OIO}consider"), &expand_id(t), qualifier_anns(b, c));
    }
    for parent in st.all("is_a") {
        let parent = parent.split_whitespace().next().unwrap_or(parent);
        let parent_iri = resolve_rel(parent, rel_map);
        // An annotation property's `is_a` is a SubAnnotationPropertyOf, not a
        // SubObjectPropertyOf.
        if metadata_tags.contains(&iri) {
            ont.insert(Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
                sub: b.annotation_property(iri.clone()),
                sup: b.annotation_property(parent_iri),
            }));
        } else {
            ont.insert(Component::SubObjectPropertyOf(SubObjectPropertyOf {
                sub: horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(
                    OPE::ObjectProperty(b.object_property(iri.clone())),
                ),
                sup: OPE::ObjectProperty(b.object_property(parent_iri)),
            }));
        }
    }
    if st.get("is_transitive") == Some("true") || st.get("transitive") == Some("true") {
        ont.insert(Component::TransitiveObjectProperty(TransitiveObjectProperty(
            OPE::ObjectProperty(b.object_property(iri.clone())),
        )));
    }
    if st.get("is_symmetric") == Some("true") {
        ont.insert(Component::SymmetricObjectProperty(SymmetricObjectProperty(
            OPE::ObjectProperty(b.object_property(iri.clone())),
        )));
    }
    if st.get("is_reflexive") == Some("true") {
        ont.insert(Component::ReflexiveObjectProperty(ReflexiveObjectProperty(
            OPE::ObjectProperty(b.object_property(iri.clone())),
        )));
    }
    if st.get("is_asymmetric") == Some("true") {
        ont.insert(Component::AsymmetricObjectProperty(AsymmetricObjectProperty(
            OPE::ObjectProperty(b.object_property(iri.clone())),
        )));
    }
    if st.get("is_functional") == Some("true") {
        ont.insert(Component::FunctionalObjectProperty(FunctionalObjectProperty(
            OPE::ObjectProperty(b.object_property(iri.clone())),
        )));
    }
    if st.get("is_inverse_functional") == Some("true") {
        ont.insert(Component::InverseFunctionalObjectProperty(InverseFunctionalObjectProperty(
            OPE::ObjectProperty(b.object_property(iri.clone())),
        )));
    }
    // A trailing `{qualifier=…}` block on `domain:`/`range:` is axiom annotations,
    // exactly as on `is_a:`/`relationship:` — splitting on whitespace and keeping
    // the first token would throw them away, losing RO's domain/range comments
    // (`{IAO:0000116="This is redundant with the more specific …"}`) on an obo→obo
    // trip. The writer emits them, so the reader has to keep them.
    for d in st.all("domain") {
        let (head, quals) = split_qualifier_block(d);
        let d = head.split_whitespace().next().unwrap_or(head);
        insert_annotated(
            ont,
            Component::ObjectPropertyDomain(ObjectPropertyDomain {
                ope: OPE::ObjectProperty(b.object_property(iri.clone())),
                ce: CE::Class(b.class(expand_id(d))),
            }),
            qualifier_anns(b, quals),
        );
    }
    for r in st.all("range") {
        let (head, quals) = split_qualifier_block(r);
        let r = head.split_whitespace().next().unwrap_or(head);
        insert_annotated(
            ont,
            Component::ObjectPropertyRange(ObjectPropertyRange {
                ope: OPE::ObjectProperty(b.object_property(iri.clone())),
                ce: CE::Class(b.class(expand_id(r))),
            }),
            qualifier_anns(b, quals),
        );
    }
    for inv in st.all("inverse_of") {
        let inv = inv.split_whitespace().next().unwrap_or(inv);
        ont.insert(Component::InverseObjectProperties(InverseObjectProperties(
            OPE::ObjectProperty(b.object_property(iri.clone())),
            OPE::ObjectProperty(b.object_property(resolve_rel(inv, rel_map))),
        )));
    }
    // `holds_over_chain: R1 R2` → `R1 ∘ R2 ⊑ this`, a SubObjectPropertyOf over a
    // property chain. `equivalent_to_chain` adds the same axiom (the reverse
    // direction is not OWL-expressible).
    for chain in st.all("holds_over_chain").chain(st.all("equivalent_to_chain")) {
        let links: Vec<OPE<RcStr>> = chain
            .split_whitespace()
            .take_while(|t| !t.starts_with('{') && !t.starts_with('!'))
            .map(|t| OPE::ObjectProperty(b.object_property(resolve_rel(t, rel_map))))
            .collect();
        if links.len() >= 2 {
            ont.insert(Component::SubObjectPropertyOf(SubObjectPropertyOf {
                sub: horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(links),
                sup: OPE::ObjectProperty(b.object_property(iri.clone())),
            }));
        }
    }
    // The OBO macro tags map back to their IAO annotation properties (the value is
    // a quoted Manchester template, so the leading `"` must be consumed).
    for (tag, prop) in [
        ("expand_expression_to", IAO_EXPAND_EXPRESSION_TO),
        ("expand_assertion_to", IAO_EXPAND_ASSERTION_TO),
    ] {
        for v in st.all(tag) {
            if let Some((text, rest)) = parse_quoted(v.trim()) {
                assert_ann_with(b, ont, &iri, prop, &text, dbxref_anns(b, rest));
            }
        }
    }
    // `transitive_over: R` → `this ∘ R ⊑ this`.
    for to in st.all("transitive_over") {
        let to = to.split_whitespace().next().unwrap_or(to);
        ont.insert(Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(vec![
                OPE::ObjectProperty(b.object_property(iri.clone())),
                OPE::ObjectProperty(b.object_property(resolve_rel(to, rel_map))),
            ]),
            sup: OPE::ObjectProperty(b.object_property(iri.clone())),
        }));
    }
    // Property synonyms map exactly as term synonyms do.
    for syn in st.all("synonym") {
        let prop = synonym_property(syn);
        if let Some((text, rest)) = parse_quoted(syn.trim()) {
            let mut anns = dbxref_anns(b, rest);
            let before_brackets = rest.split('[').next().unwrap_or("");
            let mut toks = before_brackets.split_whitespace();
            toks.next();
            if let Some(type_id) = toks.next() {
                anns.push(ann_iri(b, &format!("{OIO}hasSynonymType"), &resolve_local(type_id, onto_ns)));
            }
            anns.extend(qualifier_anns(b, rest));
            assert_ann_with(b, ont, &iri, &format!("{OIO}{prop}"), &text, anns);
        }
    }
}

// === Writer ==============================================================

/// Write an ontology to OBO format. Renders the OBO-expressible fragment;
/// axioms outside it are skipped.
const OWL_DEPRECATED_W: &str = "http://www.w3.org/2002/07/owl#deprecated";
const IAO_REPLACED_BY_W: &str = "http://purl.obolibrary.org/obo/IAO_0100001";
const RDFS_SEEALSO: &str = "http://www.w3.org/2000/01/rdf-schema#seeAlso";
const OWL_NS: &str = "http://www.w3.org/2002/07/owl#";
const IAO_EXPAND_EXPRESSION_TO: &str = "http://purl.obolibrary.org/obo/IAO_0000424";
const IAO_EXPAND_ASSERTION_TO: &str = "http://purl.obolibrary.org/obo/IAO_0000425";

/// Writer-side rendering context: the CURIE prefixes usable in the output plus a
/// record of which ones the body actually referenced, so the header can list the
/// matching `idspace:` lines.
/// OBO header directives that live as ontology-level annotations in the
/// `oboInOwl` namespace, as `(property local name, header tag)`. The property
/// local name equals the tag name except `namespace-id-rule`, whose property is
/// `NamespaceIdRule`. `format-version` (→ `hasOBOFormatVersion`), `data-version`
/// (→ the version IRI) and `remark` (→ `rdfs:comment`) are handled separately.
const HEADER_DIRECTIVES: &[(&str, &str)] = &[
    ("date", "date"),
    ("saved-by", "saved-by"),
    ("auto-generated-by", "auto-generated-by"),
    ("default-namespace", "default-namespace"),
    ("NamespaceIdRule", "namespace-id-rule"),
    ("treat-xrefs-as-equivalent", "treat-xrefs-as-equivalent"),
    ("treat-xrefs-as-genus-differentia", "treat-xrefs-as-genus-differentia"),
    ("treat-xrefs-as-relationship", "treat-xrefs-as-relationship"),
    ("treat-xrefs-as-is_a", "treat-xrefs-as-is_a"),
    ("treat-xrefs-as-has-subclass", "treat-xrefs-as-has-subclass"),
    ("treat-xrefs-as-reverse-genus-differentia", "treat-xrefs-as-reverse-genus-differentia"),
    // Emitted AFTER the `property_value:` block, not with the directives above —
    // see the call site. HPO's edit file carries it; without this entry the
    // annotation falls through to `ont_anns` and is written as a
    // `property_value: logical-definition-view-relation …` line instead of as the
    // header directive itself.
    ("logical-definition-view-relation", "logical-definition-view-relation"),
];

/// The header tag an oboInOwl-namespaced ontology-annotation property maps to, or
/// `None` if it is an ordinary `property_value:`.
fn header_directive_tag(prop_iri: &str) -> Option<&'static str> {
    let local = prop_iri.strip_prefix(OIO)?;
    HEADER_DIRECTIVES.iter().find(|(l, _)| *l == local).map(|(_, tag)| *tag)
}

#[derive(Default)]
struct Ctx {
    /// (prefix, namespace), longest namespace first so the most specific wins.
    idspaces: Vec<(String, String)>,
    /// The ontology's `default-namespace` (oboInOwl:default-namespace), if any. A
    /// term/typedef whose own `namespace:` equals it is written WITHOUT the tag —
    /// `namespace:` is emitted only when it differs from the default.
    default_namespace: Option<String>,
    /// OBO relation shorthands (`oboInOwl:shorthand`) keyed by property IRI. OBO
    /// §5.9.3 "Special Rules for Relations": a property carrying one is written
    /// under that name everywhere, so RO_0000057 appears as `has_participant`
    /// throughout CL's released `cl.obo`, never as `RO:0000057`.
    shorthands: HashMap<String, String>,
    /// Property IRIs carrying `oboInOwl:is_metadata_tag`. An annotation assertion
    /// with an IRI value on a [Term] is a `relationship:` iff its property is one of
    /// these, else a `property_value:` — so a property with a shorthand but no
    /// `is_metadata_tag` (CL's `rdfs:seeAlso`, which carries a `seeAlso` shorthand)
    /// stays a `property_value:`.
    metadata_tags: std::collections::HashSet<String>,
    /// Per subject IRI, the subject's `rdfs:label` values in the order the source
    /// document carried them. Two labels can land in the same slot of the
    /// subject's assertion set, and then the one read first is the one the
    /// `! …` comments name.
    label_order: HashMap<String, Vec<String>>,
    used: std::cell::RefCell<BTreeSet<String>>,
}

impl Ctx {
    fn new(model: &Model) -> Ctx {
        // The prefixes usable for CURIE shortening are the document's own declared
        // `xmlns:PREFIX` bindings (captured into `model.idspaces` at read time for
        // RDF/XML, which carries no formal prefix map). Anything NOT declared falls
        // to `id_impl`'s mechanical id rule — never a hard-coded well-known prefix.
        // A namespace that appears only via a default `xmlns="…"` (cl-full.owl's
        // dc/terms/skos) is therefore rendered by that rule, bare local or full IRI.
        // A config-loaded model (`explicit_prefixes` set, e.g. mondo via
        // `--add-prefixes`) always uses the reconstructed prefix set, even if
        // `idspaces` is populated (it may carry the OWL xmlns fallback set,
        // which is NOT the OBO idspace set). A plain RDF/XML model (cl/uberon) with
        // no explicit prefixes keeps using its scanned `idspaces`.
        let mut idspaces: Vec<(String, String)> =
            if !model.idspaces.is_empty() && model.explicit_prefixes.is_empty() {
                model.idspaces.clone()
        } else {
            // No scanned prefix set (OBO→OBO, or an OWL/functional model whose prefix
            // map horned-owl surfaces directly): fall back to the declared prefixes,
            // skipping the `obo/` PURL space (table 5.9.2 handles it) and builtins.
            //
            // The document's own prefixes come FIRST, in declaration order (the curie
            // map is an `IndexMap`, so `mappings()` preserves it). A namespace is
            // shortened with its *first-declared* prefix: CL declares `terms:` before
            // `dcterms:` (both `http://purl.org/dc/terms/`) so it renders `terms:`,
            // while EFO declares `dcterms:` first so it renders `dcterms:`. Every
            // declared prefix is kept (dedup only on the prefix *name*), not collapsed
            // by namespace. Nothing is appended: a namespace the document never bound
            // has no prefix to shorten with, and renders under `id_impl`'s mechanical
            // rule (a bare local name, or the full IRI).
            let mut v: Vec<(String, String)> = Vec::new();
            for (prefix, ns) in model.prefixes.mappings() {
                if prefix.is_empty()
                    || ns.starts_with(OBO_BASE)
                    || ns.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                    || ns.starts_with("http://www.w3.org/2000/01/rdf-schema#")
                    || ns.starts_with("http://www.w3.org/2001/XMLSchema#")
                    || ns.starts_with(OWL_NS)
                    || ns.starts_with("http://www.w3.org/XML/1998/namespace")
                {
                    continue;
                }
                if v.iter().any(|(p, _)| p == prefix) {
                    continue;
                }
                v.push((prefix.clone(), ns.clone()));
            }
            // The document's declared RDF/XML xmlns (carried through the pipeline
            // via the OFN `#rdfxmlns` comment) contribute their prefixes too — e.g.
            // `its`/`swrl`, which mondo declares but which the CURIE prefix map does
            // not carry. Same builtin/PURL skips as above.
            for (prefix, ns) in &model.rdf_prefixes {
                if prefix.is_empty()
                    || ns.starts_with(OBO_BASE)
                    || ns.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                    || ns.starts_with("http://www.w3.org/2000/01/rdf-schema#")
                    || ns.starts_with("http://www.w3.org/2001/XMLSchema#")
                    || ns.starts_with(OWL_NS)
                    || ns.starts_with("http://www.w3.org/XML/1998/namespace")
                {
                    continue;
                }
                if v.iter().any(|(p, _)| p == prefix) {
                    continue;
                }
                v.push((prefix.clone(), ns.clone()));
            }
            v
        };
        // Sort by namespace length, longest first (so an IRI matches the most
        // specific namespace — `gwas_trait:` before `efo:`). For two prefixes that
        // share ONE namespace (aliases, e.g. `ICD11` and `icd11.foundation` for
        // `http://id.who.int/icd/entity/`), the LONGEST prefix wins, ties broken by
        // the alphabetically-GREATEST (ASCII): `icd11.foundation` (16) beats `ICD11`
        // (5); `icd10cm` beats `ICD10CM` (both 7, lowercase 'i' > 'I'); `dcterms`
        // beats `terms`. So the tie-break is (prefix length desc, prefix string
        // desc). This only reorders same-namespace aliases; different namespaces of
        // equal length can't both prefix one IRI, so their relative order is
        // immaterial to `id_impl`.
        idspaces.sort_by(|a, b| {
            b.1.len()
                .cmp(&a.1.len())
                .then_with(|| b.0.len().cmp(&a.0.len()))
                .then_with(|| b.0.cmp(&a.0))
        });
        let mut shorthands = HashMap::new();
        let mut metadata_tags: std::collections::HashSet<String> = std::collections::HashSet::new();
        for ac in model.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if aa.ann.ap.0.as_ref() == format!("{OIO}shorthand") {
                    if let (AnnotationSubject::IRI(subj), AnnotationValue::Literal(lit)) =
                        (&aa.subject, &aa.ann.av)
                    {
                        let text = match lit {
                            Literal::Simple { literal } => literal,
                            Literal::Language { literal, .. } => literal,
                            Literal::Datatype { literal, .. } => literal,
                        };
                        shorthands.insert(subj.as_ref().to_string(), text.clone());
                    }
                } else if aa.ann.ap.0.as_ref() == format!("{OIO}is_metadata_tag") {
                    if let AnnotationSubject::IRI(subj) = &aa.subject {
                        metadata_tags.insert(subj.as_ref().to_string());
                    }
                }
            }
        }
        // The ontology-level `oboInOwl:default-namespace` annotation, if present.
        let default_namespace = model.ont.iter().find_map(|ac| {
            if let Component::OntologyAnnotation(oa) = &ac.component {
                if oa.0.ap.0.as_ref() == format!("{OIO}default-namespace") {
                    if let AnnotationValue::Literal(lit) = &oa.0.av {
                        return Some(match lit {
                            Literal::Simple { literal }
                            | Literal::Language { literal, .. }
                            | Literal::Datatype { literal, .. } => literal.clone(),
                        });
                    }
                }
            }
            None
        });
        Ctx {
            idspaces,
            default_namespace,
            shorthands,
            metadata_tags,
            label_order: model.owl_label_order.clone(),
            used: std::cell::RefCell::new(BTreeSet::new()),
        }
    }

    /// The OBO id for an IRI: a declared idspace prefix first, then OBO 1.4 table
    /// 5.9.2 (§"Translation of OWL IRIs to OBO IDs"). This is *not* the inverse of
    /// [`expand_id`] — it deliberately drops namespaces it cannot reconstruct
    /// (`obo/cl#cellxgene_subset` → `cellxgene_subset`), because that is what the
    /// committed CL `cl.obo` contains.
    fn id(&self, iri: &str) -> String {
        self.id_impl(iri, true)
    }
    /// Like `id` but never abbreviates to an `oboInOwl:shorthand`. The *value* of a
    /// `property_value:` is a plain CURIE (`BSPO:0000096`,
    /// `UBPROP:0000113`) even when the referenced entity has a shorthand
    /// (`anterior_to`, `dental_formula`); the predicate position still uses `id`.
    fn curie(&self, iri: &str) -> String {
        self.id_impl(iri, false)
    }
    /// The IRI shortened against a namespace the DOCUMENT declares, or `None` when
    /// none covers it. A declared binding is the only thing that may shorten a
    /// value: a prefix invented for the occasion would announce an `idspace:` for a
    /// namespace the document never named.
    fn declared_curie(&self, iri: &str) -> Option<String> {
        for (prefix, ns) in &self.idspaces {
            if let Some(local) = iri.strip_prefix(ns.as_str()) {
                if local.is_empty() {
                    continue;
                }
                self.used.borrow_mut().insert(prefix.clone());
                return Some(format!("{prefix}:{local}"));
            }
        }
        None
    }
    fn id_impl(&self, iri: &str, use_shorthand: bool) -> String {
        if use_shorthand {
            if let Some(sh) = self.shorthands.get(iri) {
                return sh.clone();
            }
        }
        for (prefix, ns) in &self.idspaces {
            if let Some(local) = iri.strip_prefix(ns.as_str()) {
                // An empty local part is allowed: an IRI that *is* a declared
                // namespace shortens to `prefix:` — EFO's annotation property
                // `http://www.ebi.ac.uk/efo/gwas_trait` is written `gwas_trait:`
                // (its own prefix), not `efo:gwas_trait`. idspaces are sorted
                // longest-namespace-first, so this exact match wins over `efo:`.
                self.used.borrow_mut().insert(prefix.clone());
                return format!("{prefix}:{local}");
            }
        }
        // The OWL namespace keeps its prefix even when undeclared (`owl:versionInfo`
        // in every released `.obo`), unlike rdf/rdfs which fall through to the
        // `#`-stripping rule below and render bare (`seeAlso`).
        if let Some(local) = iri.strip_prefix(OWL_NS) {
            if !local.is_empty() && !local.contains('/') {
                return format!("owl:{local}");
            }
        }
        let id = match iri.rfind('/') {
            Some(i) => &iri[i + 1..],
            None => iri,
        };
        // Row 2, NonCanonical-Prefixed-ID: `…/ubprop#_upper_level` → `ubprop:upper_level`.
        if let Some((pre, local)) = id.split_once("#_") {
            if !pre.is_empty() && !local.is_empty() && !local.contains('#') {
                return format!("{pre}:{local}");
            }
            return iri.to_string();
        }
        // Row 3, Unprefixed-ID: `…/obo/cl#cellxgene_subset` → `cellxgene_subset`.
        if let Some((pre, local)) = id.split_once('#') {
            if local.is_empty() || local.contains('#') {
                return iri.to_string();
            }
            // Only the *local* part is percent-decoded (the prefix keeps its
            // `%HH`): `…/Chinois_(R%C3%A9union)` → `Chinois:(Réunion)` but
            // `…/Gourmanch%C3%A9_language` → `Gourmanch%C3%A9:language`.
            return if pre == "_" {
                format!("_:{}", percent_decode(local))
            } else {
                percent_decode(local)
            };
        }
        // Row 4, Canonical-Prefixed-ID: the LAST `_` of the id (the part after the
        // final `/`) splits idspace from local id, and does so for *any* IRI,
        // not just obo-PURLs — `CL_0000540` → `CL:0000540`,
        // `EFO_0008992` → `EFO:0008992` (`http://www.ebi.ac.uk/efo/…`),
        // `DHBA_10333` → `DHBA:10333` (`https://purl.brain-bican.org/…`),
        // `Ontology_extensions` → `Ontology:extensions` (a GO-wiki link). No
        // `idspace:` line is emitted for these; the reader re-splits at the last `_`.
        if let Some(p) = id.rfind('_') {
            let (pre, local) = (&id[..p], &id[p + 1..]);
            // A single-underscore id always splits (`CL_0000540`, `FOO_baz`,
            // `Ontology_extensions`). A multi-underscore idspace splits only when the
            // local id is numeric: `NCBITaxon_Union_0000030` →
            // `NCBITaxon_Union:0000030`, but `valid_for_gocam` stays a full IRI. An id
            // with no `_` (an ORCID, a DOI, a GitHub issue URL) stays full too.
            let single = !pre.contains('_');
            let numeric_local = local.bytes().all(|b| b.is_ascii_digit());
            if !pre.is_empty() && !local.is_empty() && (single || numeric_local) {
                return format!("{pre}:{}", percent_decode(local));
            }
        }
        iri.to_string()
    }
}

/// Case-insensitive sort key for the repeated clauses of a tag (`xref:`,
/// `synonym:`, `is_a:` …) — CL's `cl.obo` has `xref: ncithesaurus:…` between
/// `MA:…` and `VHOG:…`, which only a case-folding comparison produces.
///
/// The comparison upper-cases to decide but returns the lower-case difference, so
/// the effective key is `lowercase(uppercase(c))` per single UTF-16 unit.
///
/// Plain `str::to_lowercase` is nearly right, but it is the full Unicode mapping
/// and can EXPAND: `'İ'` (U+0130) becomes `i` + U+0307 COMBINING DOT ABOVE where a
/// char-to-char fold yields just `'i'`. That trailing U+0307 sorts after every
/// ASCII letter, which would put HPO's Turkish `İdrar yolu…` after `Infekce…`
/// instead of before `Infections…` — 3241 `synonym:` lines of
/// `hp-international.obo`.
fn fold(s: &str) -> String {
    /// A length-preserving single-character case map: a multi-char Unicode
    /// expansion is declined (`'ß'.to_uppercase()` is `"SS"`, so `'ß'` is left
    /// as it is).
    fn single(c: char, mut it: impl Iterator<Item = char>) -> char {
        let first = it.next().unwrap_or(c);
        if it.next().is_some() {
            c
        } else {
            first
        }
    }
    s.chars()
        .map(|c| {
            let upper = single(c, c.to_uppercase());
            // The one mapping where declining the expansion is wrong: the
            // single-character lower case of U+0130 is defined as U+0069.
            if upper == '\u{130}' {
                return 'i';
            }
            single(upper, upper.to_lowercase())
        })
        .collect()
}

/// All the OBO-renderable facts about one term/typedef subject, grouped so the
/// stanza can be emitted faithfully (axiom-annotation `[xref]`/`TYPE`/`{qual}`
/// blocks included) and re-read to the same axioms.
#[derive(Default)]
struct SubjData {
    id: Option<String>,
    name: Option<(String, BTreeSet<Annotation<RcStr>>)>,
    // Additional `rdfs:label` values beyond the primary `name`, each with its own
    // axiom annotations. An entity may carry several labels (OBI:0000295 is both
    // "is_input_of" and "is specified input of"); one `name:` line is written per
    // label, sorted, carrying any `{key="…"}` qualifiers (GSSO's translated
    // labels have a `{terms:isReferencedBy="…"}` source).
    extra_names: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// `rdfs:label` values asserted WITHOUT a language tag (a plain/`xsd:string`
    /// literal). An entity's display name — used in the `! <label>` end-of-line
    /// comments — comes from the language-neutral label when one exists, in
    /// preference to any `@en` (etc.) label. RO:0002211 carries both
    /// `"regulates (processual)"` (no tag) and `"regulates"@en`, and the
    /// `! regulates (processual) …` comment comes from the untagged one.
    label_no_lang: BTreeSet<String>,
    /// Every `rdfs:label` axiom on the subject: (value, language tag, axiom
    /// annotations). The `!`-comment name is the label whose axiom lands in the
    /// minimum hash bucket (see [`pick_comment_name`]).
    label_axioms: Vec<(String, Option<String>, BTreeSet<Annotation<RcStr>>)>,
    /// Count of ALL annotation-assertion axioms on the subject — it sizes the hash
    /// table whose bucket order picks the `!`-comment label.
    ann_count: usize,
    namespace: Vec<String>,
    def: Option<(String, BTreeSet<Annotation<RcStr>>)>,
    // Additional `IAO:0000115` definitions beyond the first. A term may carry more
    // than one (RO's `overlaps` has two); one `def:` line is written for each,
    // sorted, identical ones collapsed.
    extra_defs: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    comments: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// (scope, text, language tag, axiom annotations).
    ///
    /// The LANGUAGE TAG is part of the axiom hash, and that hash is what buckets
    /// synonym clauses whose values tie. Dropping it hashes every translated
    /// synonym as if untagged, which reorders 5,956 lines of
    /// `hp-international.obo`.
    synonyms: Vec<(String, String, Option<String>, BTreeSet<Annotation<RcStr>>)>,
    xrefs: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    /// (rendered subset name, RAW annotation value, value-is-IRI, axiom annotations).
    /// The raw value and flag rebuild the assertion's hash, which is what breaks
    /// ties between two `subset:` clauses naming the same subset.
    subsets: Vec<(String, String, bool, BTreeSet<Annotation<RcStr>>)>,
    alt_ids: Vec<String>,
    replaced_by: Vec<String>,
    consider: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    created_by: Vec<String>,
    creation_date: Vec<String>,
    deprecated: bool,
    // Axiom annotations on the `owl:deprecated true` assertion — a `{source=…}`
    // qualifier on the `is_obsolete: true` line.
    deprecated_anns: BTreeSet<Annotation<RcStr>>,
    /// `IAO:0000231` obsolescence reason. `IAO:0000227` ("terms merged") marks the
    /// stub that a primary term's `alt_id:` expands to; see [`fold_alt_ids`].
    obsolescence_reason: Option<String>,
    /// OBO macro expansions (`IAO:0000424`/`IAO:0000425`) — Typedef-only tags,
    /// written as `expand_expression_to: "…" []`, not `property_value:`.
    expand_expression_to: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    expand_assertion_to: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    shorthand: Option<String>,
    is_metadata_tag: bool,
    is_class_level: bool,
    // (predicate-obo, printed value, is_iri, datatype-curie, anns, predicate IRI,
    // value IRI). Both IRIs are kept because the axiom hash that orders clauses
    // tying on predicate AND value is over FULL IRIs, not the CURIEs the clause
    // prints — two values that share a CURIE prefix hash nothing alike.
    property_values: Vec<(
        String,
        String,
        bool,
        Option<String>,
        BTreeSet<Annotation<RcStr>>,
        String,
        String,
    )>,
    // (parent, anns, extra `{gci_*}` qualifiers from a General Class Inclusion,
    // source SubClassOf axiom's hash, which breaks ties between equal clauses)
    is_a: Vec<(String, BTreeSet<Annotation<RcStr>>, Vec<(String, String)>, i32)>,
    // (rel, target, anns, extra `{gci_*}` qualifiers, source axiom hash)
    relationships: Vec<(String, String, BTreeSet<Annotation<RcStr>>, Vec<(String, String)>, i32)>,
    // Shorthand-property annotations with an IRI value whose routing
    // (`relationship:` in a [Term] vs `property_value:` in a [Typedef]) depends on
    // the subject's stanza type, which is only known at write time. (predicate-obo,
    // value, anns)
    rel_or_pv: Vec<(String, String, BTreeSet<Annotation<RcStr>>, String, String)>,
    // each line's tokens, plus any clause qualifiers (a cardinality bound)
    intersection_of: Vec<(Vec<String>, Vec<(String, String)>, BTreeSet<Annotation<RcStr>>)>,
    union_of: Vec<String>,
    equivalent_to: Vec<String>,
    disjoint_from: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    // Typedef-only property axioms.
    domain: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    range: Vec<(String, BTreeSet<Annotation<RcStr>>)>,
    inverse_of: Vec<String>,
    transitive: bool,
    // `oboInOwl:is_transitive` on a Typedef: its value is the
    // `is_transitive:` tag (true *or* false), not a `property_value:`. The axiom
    // (`TransitiveObjectProperty`) covers the true case; this carries an explicit
    // `false` (EFO marks several relations non-transitive) that would otherwise leak.
    transitive_anno: Option<bool>,
    symmetric: bool,
    reflexive: bool,
    asymmetric: bool,
    functional: bool,
    inverse_functional: bool,
    chains: Vec<(Vec<String>, BTreeSet<Annotation<RcStr>>)>, // holds_over_chain (links, axiom anns)
    sub_property_of: Vec<String>,
    // An annotation property declared via a header `subsetdef:`/`synonymtypedef:`
    // (sub-property of oboInOwl:SubsetProperty / SynonymTypeProperty).
    subset_property: bool,
    synonymtype_property: bool,
}

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

fn ce_is_top_or_bottom(ce: &CE<RcStr>) -> bool {
    matches!(ce, CE::Class(c) if c.0.as_ref() == OWL_THING || c.0.as_ref() == OWL_NOTHING)
}

fn ce_named_class(ce: &CE<RcStr>) -> bool {
    matches!(ce, CE::Class(_))
}

/// A filler has no OBO spelling unless it is a named class.
fn filler_bad(bce: &CE<RcStr>) -> bool {
    ce_is_top_or_bottom(bce) || !ce_named_class(bce)
}

/// Which `SubClassOf` axioms have no OBO clause and go to the `owl-axioms:` bag.
fn subclassof_untranslatable(sub: &CE<RcStr>, sup: &CE<RcStr>) -> bool {
    if ce_is_top_or_bottom(sub) || ce_is_top_or_bottom(sup) {
        return true;
    }
    // GCI reduction: subject must be a named class or a 2-part `C ⊓ ∃R.F`.
    let sub_is_class = match sub {
        CE::Class(_) => true,
        CE::ObjectIntersectionOf(xs) if xs.len() == 2 => {
            let has_c = xs.iter().any(ce_named_class);
            let has_restr = xs.iter().any(|x| matches!(x,
                CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(_), bce }
                    if ce_named_class(bce)));
            has_c && has_restr
        }
        _ => false,
    };
    if !sub_is_class {
        return true;
    }
    match sup {
        CE::Class(_) => false,
        CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => filler_bad(bce),
        CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. }
        | CE::ObjectMaxCardinality { bce, .. } => filler_bad(bce),
        CE::ObjectIntersectionOf(ops) => {
            if ops.is_empty() {
                return true;
            }
            ops.iter().any(|op| match op {
                CE::ObjectSomeValuesFrom { bce, .. }
                | CE::ObjectAllValuesFrom { bce, .. }
                | CE::ObjectMinCardinality { bce, .. }
                | CE::ObjectExactCardinality { bce, .. }
                | CE::ObjectMaxCardinality { bce, .. } => filler_bad(bce),
                _ => true,
            })
        }
        _ => true,
    }
}

fn ec_operand_bad(op: &CE<RcStr>) -> bool {
    match op {
        CE::Class(_) => false,
        CE::ObjectSomeValuesFrom { bce, .. }
        | CE::ObjectMinCardinality { bce, .. }
        | CE::ObjectExactCardinality { bce, .. }
        | CE::ObjectMaxCardinality { bce, .. } => !ce_named_class(bce),
        CE::ObjectAllValuesFrom { bce, .. } => match bce.as_ref() {
            CE::Class(_) => false,
            CE::ObjectComplementOf(inner) => !ce_named_class(inner),
            _ => true,
        },
        // A nested `ObjectIntersectionOf` (all-some / min-max combination) resolves
        // as long as one operand is a restriction with a named filler — that
        // operand is kept and the rest dropped.
        CE::ObjectIntersectionOf(inner) if inner.len() == 2 => !inner.iter().any(|o| match o {
            CE::ObjectMinCardinality { bce, .. }
            | CE::ObjectMaxCardinality { bce, .. }
            | CE::ObjectAllValuesFrom { bce, .. }
            | CE::ObjectSomeValuesFrom { bce, .. } => ce_named_class(bce),
            _ => false,
        }),
        _ => true,
    }
}

fn ec_untranslatable(ops: &[CE<RcStr>]) -> bool {
    if ops.len() != 2 {
        return true;
    }
    if ops.iter().any(ce_is_top_or_bottom) {
        return true;
    }
    let (ce1_named, ce2) = if ce_named_class(&ops[0]) {
        (true, &ops[1])
    } else if ce_named_class(&ops[1]) {
        (true, &ops[0])
    } else {
        (false, &ops[1])
    };
    if !ce1_named {
        return true;
    }
    match ce2 {
        CE::Class(_) => false,
        CE::ObjectUnionOf(list) => list.iter().any(|o| !ce_named_class(o)),
        CE::ObjectIntersectionOf(list) => list.iter().any(ec_operand_bad),
        // `NamedClass ≡ ObjectOneOf(individuals)` (an enumeration — IAO_0000078 ≡
        // {IAO_0000002 … IAO_0000428}) has no OBO spelling, so it is parked in
        // `owl-axioms:` together with the class's Declaration and one for every
        // NamedIndividual in the enumeration. It is what MONDO's
        // `mondo-international.obo` bag holds for IAO_0000078 / IAO_0000225 /
        // IAO_0000409.
        CE::ObjectOneOf(_) => true,
        _ => true,
    }
}

fn dc_untranslatable(ops: &[CE<RcStr>]) -> bool {
    ops.len() != 2 || ops.iter().any(ce_is_top_or_bottom) || !ops.iter().all(ce_named_class)
}

/// Whether `--clean-obo drop-untranslatable-axioms` should DROP this
/// `DisjointClasses`, which is not the same question as whether it belongs in the
/// `owl-axioms:` bag.
///
/// An n-ary one is PARTIALLY translatable: one `disjoint_from:` is written for its
/// first two members in IRI order and the other pairs are lost, so it lands
/// in the bag (MONDO's `mondo-international.obo` bags a four-member FOODON
/// disjointness) AND still contributes its clause (MONDO's `mondo.obo` and OBA's
/// `oba.obo` both carry it). Dropping it would take the clause with it.
fn dc_droppable(ops: &[CE<RcStr>]) -> bool {
    ops.len() < 2 || ops.iter().any(ce_is_top_or_bottom) || !ops.iter().all(ce_named_class)
}

fn opr_untranslatable(ope: &OPE<RcStr>, ce: &CE<RcStr>) -> bool {
    matches!(ope, OPE::InverseObjectProperty(_)) || ce_is_top_or_bottom(ce) || !ce_named_class(ce)
}

fn sop_untranslatable(
    ax: &horned_owl::model::SubObjectPropertyOf<RcStr>,
) -> bool {
    use horned_owl::model::SubObjectPropertyExpression as SOPE;
    match &ax.sub {
        // A chain becomes `holds_over_chain`/`transitive_over` only when it is
        // exactly two *named* properties with a named super-property; anything else
        // (an inverse element, 3+ links) is untranslatable.
        SOPE::ObjectPropertyChain(v) => {
            v.len() != 2
                || v.iter().any(|o| matches!(o, OPE::InverseObjectProperty(_)))
                || matches!(ax.sup, OPE::InverseObjectProperty(_))
        }
        SOPE::ObjectPropertyExpression(OPE::ObjectProperty(_)) => {
            matches!(ax.sup, OPE::InverseObjectProperty(_))
        }
        SOPE::ObjectPropertyExpression(OPE::InverseObjectProperty(_)) => true,
    }
}

/// Axioms that OBO has no tag for. Collected for the `owl-axioms:` header value.
/// The IRIs of a subject's annotation assertions that are "unrelated" to its
/// OBO alt-id role — those become untranslatable. Returns a set of
/// `(subject, property, value-string)` keys identifying them.
fn alt_id_unrelated(model: &Model) -> HashSet<(String, String, String)> {
    use horned_owl::model::AnnotationSubject as AS;
    // Group annotation assertions by subject IRI.
    let mut by_subject: HashMap<String, Vec<&Annotation<RcStr>>> = HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let AS::IRI(s) = &aa.subject {
                by_subject.entry(s.as_ref().to_string()).or_default().push(&aa.ann);
            }
        }
    }
    let av_is_iri = |av: &AnnotationValue<RcStr>| matches!(av, AnnotationValue::IRI(_));
    let av_is_literal = |av: &AnnotationValue<RcStr>| matches!(av, AnnotationValue::Literal(_));
    let av_key = |av: &AnnotationValue<RcStr>| match av {
        AnnotationValue::Literal(l) => l.literal().clone(),
        AnnotationValue::IRI(i) => i.as_ref().to_string(),
        AnnotationValue::AnonymousIndividual(a) => a.0.as_ref().to_string(),
    };
    let mut out = HashSet::new();
    for (subj, anns) in &by_subject {
        let mut is_deprecated = false;
        let mut is_merged = false;
        let mut replaced_by = false;
        for a in anns {
            let p = a.ap.0.as_ref();
            if p == OWL_DEPRECATED {
                is_deprecated = true;
            } else if p == IAO_OBSOLESCENCE_REASON {
                if let AnnotationValue::IRI(i) = &a.av {
                    if i.as_ref() == IAO_TERMS_MERGED {
                        is_merged = true;
                    }
                }
            } else if p == IAO_TERM_REPLACED_BY {
                if av_is_literal(&a.av) || av_is_iri(&a.av) {
                    replaced_by = true;
                }
            }
        }
        if !(replaced_by && is_merged && is_deprecated) {
            continue;
        }
        for a in anns {
            let p = a.ap.0.as_ref();
            let unrelated = if p == OWL_DEPRECATED {
                false
            } else if p == IAO_OBSOLESCENCE_REASON {
                !av_is_iri(&a.av)
            } else if p == IAO_TERM_REPLACED_BY {
                !(av_is_literal(&a.av) || av_is_iri(&a.av))
            } else {
                true
            };
            if unrelated {
                out.insert((subj.clone(), p.to_string(), av_key(&a.av)));
            }
        }
    }
    out
}

/// A `def:`/`synonym:`/`comment:` clause with an EMPTY string value cannot be
/// written as OBO (an empty scalar clause is invalid), so the assertion is parked
/// in the `owl-axioms:` bag instead. EFO's IAO_0000115 EFO_0010180 "",
/// hasExactSynonym OBI_0000512 "", and rdfs:comment EFO_0007034 "" are the three.
/// An empty `IAO_0000117`/other-property value stays a normal `property_value:`.
fn empty_scalar_clause(aa: &horned_owl::model::AnnotationAssertion<RcStr>) -> bool {
    let empty = matches!(&aa.ann.av, AnnotationValue::Literal(l) if l.literal().is_empty());
    if !empty {
        return false;
    }
    let p = aa.ann.ap.0.as_ref();
    p == IAO_DEF
        || p == RDFS_COMMENT
        || p == format!("{OIO}hasExactSynonym")
        || p == format!("{OIO}hasNarrowSynonym")
        || p == format!("{OIO}hasBroadSynonym")
        || p == format!("{OIO}hasRelatedSynonym")
}

fn collect_untranslatable(
    model: &Model,
) -> Vec<&horned_owl::model::AnnotatedComponent<RcStr>> {
    collect_untranslatable_opt(model, false)
}

/// As [`collect_untranslatable`]; `for_drop` asks the narrower question of what
/// `--clean-obo drop-untranslatable-axioms` may REMOVE, which excludes the
/// partially-translatable n-ary `DisjointClasses` (see [`dc_droppable`]).
fn collect_untranslatable_opt(
    model: &Model,
    for_drop: bool,
) -> Vec<&horned_owl::model::AnnotatedComponent<RcStr>> {
    use horned_owl::model::AnnotationSubject as AS;
    let alt_unrelated = alt_id_unrelated(model);
    // Under a declared `logical-definition-view-relation`, every EquivalentClasses
    // axiom is rewritten before anything is decided, so translatability must be
    // judged on the REWRITTEN axiom. Judging the original calls
    // `HP:0000002 ≡ has_part some (…)` untranslatable and lets
    // `--clean-obo drop-untranslatable-axioms` delete it before the writer can
    // unwrap it — every one of `hp-base.obo`'s 12,806 `intersection_of:` lines.
    let view_rel = declares_view_relation(model);
    let mut out = Vec::new();
    for ac in model.ont.iter() {
        let unt = match &ac.component {
            Component::AnnotationAssertion(aa) => match &aa.subject {
                AS::IRI(s) => {
                    let vk = match &aa.ann.av {
                        AnnotationValue::Literal(l) => l.literal().clone(),
                        AnnotationValue::IRI(i) => i.as_ref().to_string(),
                        AnnotationValue::AnonymousIndividual(a) => a.0.as_ref().to_string(),
                    };
                    alt_unrelated.contains(&(
                        s.as_ref().to_string(),
                        aa.ann.ap.0.as_ref().to_string(),
                        vk,
                    )) || empty_scalar_clause(aa)
                }
                _ => false,
            },
            Component::ObjectPropertyAssertion(_)
            | Component::DisjointUnion(_)
            | Component::IrreflexiveObjectProperty(_)
            | Component::Rule(_)
            | Component::DataPropertyAssertion(_)
            | Component::HasKey(_)
            | Component::SameIndividual(_)
            | Component::DifferentIndividuals(_)
            | Component::NegativeObjectPropertyAssertion(_)
            | Component::NegativeDataPropertyAssertion(_)
            | Component::SubDataPropertyOf(_)
            | Component::DataPropertyDomain(_)
            | Component::DataPropertyRange(_)
            | Component::FunctionalDataProperty(_)
            | Component::EquivalentDataProperties(_)
            | Component::DisjointDataProperties(_)
            | Component::DatatypeDefinition(_)
            | Component::AnnotationPropertyDomain(_)
            | Component::AnnotationPropertyRange(_) => true,
            Component::SubClassOf(ax) => subclassof_untranslatable(&ax.sub, &ax.sup),
            Component::EquivalentClasses(ax) => {
                if view_rel {
                    match &rewrite_logical_definition_view(ac).component {
                        Component::EquivalentClasses(r) => ec_untranslatable(&r.0),
                        _ => ec_untranslatable(&ax.0),
                    }
                } else {
                    ec_untranslatable(&ax.0)
                }
            }
            Component::DisjointClasses(ax) => {
                if for_drop {
                    dc_droppable(&ax.0)
                } else {
                    dc_untranslatable(&ax.0)
                }
            }
            Component::ObjectPropertyRange(ax) => opr_untranslatable(&ax.ope, &ax.ce),
            Component::SubObjectPropertyOf(ax) => sop_untranslatable(ax),
            // A SubAnnotationPropertyOf whose super-property is oboInOwl:SubsetProperty
            // or SynonymTypeProperty becomes a `subsetdef:`/`synonymtypedef:` header
            // line (translatable). Any other super-property (EFO's created_by ⊑
            // dc:creator and skos:prefLabel ⊑ rdfs:label) has no OBO spelling — the
            // sub-property is not a Typedef frame — so it goes in the bag.
            Component::SubAnnotationPropertyOf(ax) => {
                let sup = ax.sup.0.as_ref();
                sup != format!("{OIO}SubsetProperty") && sup != format!("{OIO}SynonymTypeProperty")
            }
            _ => false,
        };
        if unt {
            out.push(ac);
        }
    }
    // horned-owl's RDF/XML reader emits a spurious un-annotated axiom alongside the
    // reified annotated one (the base triple + its `owl:Axiom` reification), which
    // are one axiom and not two. Drop an un-annotated instance when the same
    // component also appears annotated.
    let annotated: HashSet<&Component<RcStr>> =
        out.iter().filter(|ac| !ac.ann.is_empty()).map(|ac| &ac.component).collect();
    out.retain(|ac| !(ac.ann.is_empty() && annotated.contains(&ac.component)));
    out
}

/// The set of axioms the OBO writer would divert into the `owl-axioms:` header
/// block — i.e. those `--clean-obo drop-untranslatable-axioms` removes. Owned
/// clones so callers can filter the model against them.
pub fn untranslatable_axioms(
    model: &Model,
) -> HashSet<horned_owl::model::AnnotatedComponent<RcStr>> {
    collect_untranslatable_opt(model, true).into_iter().cloned().collect()
}

/// Whether the ontology declares `oboInOwl:logical-definition-view-relation`.
fn declares_view_relation(model: &Model) -> bool {
    let prop = format!("{OIO}logical-definition-view-relation");
    model.ont.iter().any(|ac| match &ac.component {
        Component::OntologyAnnotation(oa) => oa.0.ap.0.as_ref() == prop,
        _ => false,
    })
}

/// One `EquivalentClasses` axiom under the `logical-definition-view-relation`
/// rewrite — see the call site in `save`. Returns the axiom unchanged when it does
/// not have exactly one named-class operand.
fn rewrite_logical_definition_view(
    ac: &horned_owl::model::AnnotatedComponent<RcStr>,
) -> horned_owl::model::AnnotatedComponent<RcStr> {
    use horned_owl::model::EquivalentClasses;
    let Component::EquivalentClasses(eq) = &ac.component else { return ac.clone() };
    let mut named = 0usize;
    let mut xs: Vec<CE<RcStr>> = Vec::new();
    for x in &eq.0 {
        match x {
            CE::Class(_) => {
                named += 1;
                xs.push(x.clone());
            }
            // The property is only CHECKED against the declared view relation (a
            // mismatch is logged, not acted on), so unwrap whatever it is.
            CE::ObjectSomeValuesFrom { bce, .. } => xs.push((**bce).clone()),
            // Anything else is logged as unexpected and DROPPED, not carried over.
            _ => {}
        }
    }
    if named != 1 {
        return ac.clone();
    }
    // The operands are collected into a set, so equal ones collapse.
    let mut deduped: Vec<CE<RcStr>> = Vec::new();
    for x in xs {
        if !deduped.contains(&x) {
            deduped.push(x);
        }
    }
    // The rewritten axiom is rebuilt with its operands sorted, and the FIRST
    // operand becomes the frame subject, so for two named classes the clause
    // lands on the smaller IRI — `equivalent_to: NBO:0001786` belongs in the
    // NBO:0000313 frame, not the other way round.
    deduped.sort_by(crate::io::owlfunc::cmp_ce);
    horned_owl::model::AnnotatedComponent {
        component: Component::EquivalentClasses(EquivalentClasses(deduped)),
        ann: ac.ann.clone(),
    }
}

pub fn save<W: Write>(model: &Model, writer: &mut W) -> Result<()> {
    let ctx = Ctx::new(model);
    let mut classes: BTreeSet<String> = BTreeSet::new();
    let mut obj_props: BTreeSet<String> = BTreeSet::new();
    let mut ann_props: BTreeSet<String> = BTreeSet::new();
    let mut data: BTreeMap<String, SubjData> = BTreeMap::new();
    let mut ont_iri: Option<String> = None;
    let mut ont_version_iri: Option<String> = None;
    let mut ont_anns: Vec<(String, String, bool, Option<String>)> = Vec::new();
    let mut remarks: Vec<String> = Vec::new();
    let mut imports: Vec<String> = Vec::new();
    let mut format_version: Option<String> = None;
    let mut directives: HashMap<&'static str, Vec<String>> = HashMap::new();

    // Per-subject count of annotation-assertion axioms. A subject's assertions sit
    // in a hash table whose size — hence its bucket order — is fixed by this count,
    // and that order is what breaks ties between clauses of equal value (see
    // `owlapi_aa_bucket`).
    let mut aa_counts: HashMap<String, usize> = HashMap::new();
    // Every `SubClassOf` axiom sits in one ontology-wide hash table; its size (from
    // this total) fixes the bucket order that breaks `is_a:`/`relationship:` clause
    // ties (see `owlapi_subclassof_hash`).
    let mut subclass_count: usize = 0;
    // Axioms are consumed one type at a time out of a per-type hash table, so
    // EquivalentClasses axioms arrive in hash-bucket order — which decides which
    // definition reaches a frame FIRST, and therefore whether a later one with an
    // unspellable operand is dropped whole or merely trimmed (see the
    // `ObjectIntersectionOf` arm of `record_ac`). Hold them back and replay them in
    // that order.
    let mut equivs: Vec<&horned_owl::model::AnnotatedComponent<RcStr>> = Vec::new();
    for ac in model.ont.iter() {
        if matches!(ac.component, Component::EquivalentClasses(_)) {
            equivs.push(ac);
        } else {
            record_ac(ac, &ctx, &mut classes, &mut obj_props, &mut ann_props, &mut data, &mut ont_iri, &mut ont_version_iri, &mut ont_anns, &mut remarks, &mut imports, &mut format_version, &mut directives);
        }
        match &ac.component {
            Component::AnnotationAssertion(aa) => {
                if let AnnotationSubject::IRI(subj) = &aa.subject {
                    *aa_counts.entry(subj.as_ref().to_string()).or_insert(0) += 1;
                }
            }
            Component::SubClassOf(_) => subclass_count += 1,
            _ => {}
        }
    }
    // When the ontology declares `logical-definition-view-relation`, EVERY
    // EquivalentClasses axiom is rewritten before translation — each
    // `ObjectSomeValuesFrom(p, filler)` operand is replaced by its FILLER, any other
    // anonymous operand is dropped, and the rewrite applies only when exactly one
    // operand is a named class. HPO declares the view relation `has_part`, so
    // `HP:0000002 ≡ has_part some (PATO:0000119 and inheres_in some
    // UBERON:0000468 and …)` becomes `HP:0000002 ≡ (PATO:0000119 and …)` and yields
    // three `intersection_of:` lines; without the rewrite the equivalence is a bare
    // someValuesFrom and no clause comes out of it at all — 12,806 missing lines in
    // `hp-base.obo`.
    //
    // The rewrite precedes the read of the EquivalentClasses axiom set, so the
    // hash-bucket order below is over the REWRITTEN axioms.
    let rewritten: Vec<horned_owl::model::AnnotatedComponent<RcStr>> =
        if directives.contains_key("logical-definition-view-relation") {
            equivs.iter().map(|ac| rewrite_logical_definition_view(ac)).collect()
        } else {
            Vec::new()
        };
    let equivs: Vec<&horned_owl::model::AnnotatedComponent<RcStr>> =
        if rewritten.is_empty() { equivs } else { rewritten.iter().collect() };
    {
        let eq_cap = owlapi_set_cap(equivs.len());
        let mut keyed: Vec<(usize, usize, &horned_owl::model::AnnotatedComponent<RcStr>)> = equivs
            .iter()
            .enumerate()
            .map(|(i, ac)| {
                let Component::EquivalentClasses(eq) = &ac.component else { unreachable!() };
                let h = owlapi_equivalent_classes_hash(&eq.0, &ac.ann) as u32;
                let spread = h ^ (h >> 16);
                ((spread as usize) & (eq_cap - 1), i, *ac)
            })
            .collect();
        keyed.sort_by_key(|(b, i, _)| (*b, *i));
        for (_, _, ac) in keyed {
            record_ac(ac, &ctx, &mut classes, &mut obj_props, &mut ann_props, &mut data, &mut ont_iri, &mut ont_version_iri, &mut ont_anns, &mut remarks, &mut imports, &mut format_version, &mut directives);
        }
    }
    let subclass_cap = owlapi_set_cap(subclass_count);
    // Header-directive values are written case-insensitively sorted, like the
    // other multi-valued header tags (`subsetdef:`, the treat-xrefs lists).
    for vals in directives.values_mut() {
        vals.sort_by_key(|v| fold(v));
    }
    imports.sort();
    remarks.sort_by_key(|r| fold(r));
    // Ontology header `property_value:` lines are sorted the same way stanza
    // clauses are (CL's `cl.obo` lists `dc:description … terms:license`
    // case-insensitively, not in axiom order).
    ont_anns.sort_by_key(|(pred, val, _, _)| (fold(pred), fold(val)));

    // Alternate-id classes (the deprecated stubs `alt_id:` expands to) are NOT
    // written as their own stanzas — the reader regenerates them (declaration +
    // owl:deprecated + replaced_by + obsolescence reason) from the primary term's
    // `alt_id:`. Emitting a stanza would add a spurious `oboInOwl:id`.
    let (alt_classes, alt_targets) = fold_alt_ids(&ctx, &mut data);
    // A merge target that is otherwise undeclared still gets a `[Term]` stanza for
    // its inherited `alt_id:`.
    classes.extend(alt_targets);

    // Two object properties can render to the SAME obo typedef id: RO:0002202 via
    // its `oboInOwl:shorthand` "develops_from" and `bto#develops_from` via its local
    // name. ONE merged `[Typedef]` is emitted. When the extra property carries
    // nothing but a `name:` (bto#develops_from is only labelled "derives from/develops
    // from"), fold that lone name into the content-bearing property and drop it, so a
    // single stanza is written whose two `name:` lines sort together — instead of a
    // second, near-empty duplicate-id stanza.
    {
        let mut by_id: HashMap<String, Vec<String>> = HashMap::new();
        for prop in &obj_props {
            if data.get(prop).map(has_content).unwrap_or(false) {
                by_id.entry(ctx.id(prop)).or_default().push(prop.clone());
            }
        }
        for group in by_id.into_values() {
            if group.len() != 2 {
                continue;
            }
            let a_only = data.get(&group[0]).map(is_name_only).unwrap_or(false);
            let b_only = data.get(&group[1]).map(is_name_only).unwrap_or(false);
            let (primary, secondary) = match (a_only, b_only) {
                (false, true) => (group[0].clone(), group[1].clone()),
                (true, false) => (group[1].clone(), group[0].clone()),
                _ => continue,
            };
            if let Some(sec) = data.remove(&secondary) {
                if let Some(pd) = data.get_mut(&primary) {
                    if let Some(nm) = sec.name {
                        pd.extra_names.push(nm);
                    }
                    pd.extra_names.extend(sec.extra_names);
                    pd.label_axioms.extend(sec.label_axioms);
                    pd.ann_count += sec.ann_count;
                }
            }
        }
    }

    // The label of every subject that has one, keyed by the *rendered* OBO id, so
    // referring clauses can carry the trailing `! label` comment. A property
    // written under its relation shorthand is included: it IS labelled in *value*
    // position (`inverse_of: has_participant ! has participant`, a [Typedef]'s
    // `is_a: transitively_anteriorly_connected_to ! transitively anteriorly
    // connected to`) and unlabelled only as the head of a `RELATION FILLER` clause,
    // which `label_comment_pred` takes care of.
    // A `! label` comment resolves from class and property labels only — never a
    // NamedIndividual's. So a relationship whose value is an individual (uberon's
    // `dc-contributor <ORCID>`, a labelled `owl:NamedIndividual`) gets no comment,
    // while one whose value is a labelled class (CL's `RO:0002292 <ncbigene IRI>`,
    // "expresses LHX6") does. Keep only class/property labels here so
    // `label_comment` naturally omits the individual ones.
    let mut labels: HashMap<String, String> = HashMap::new();
    for (iri, sd) in &data {
        if sd.name.is_some()
            && (classes.contains(iri) || obj_props.contains(iri) || ann_props.contains(iri))
        {
            // The `! label` comment is the `rdfs:label` whose annotation-assertion
            // axiom lands in the minimum hash bucket — the table sized to the
            // subject's total annotation-assertion count. That picks OBI:0000295
            // ("is specified input of"), PR:000003918 ("serum albumin"), part_of
            // and the GSSO multilingual labels.
            if let Some(name) = pick_comment_name(&ctx, iri, sd) {
                labels.insert(ctx.id(iri), name);
            }
        }
    }

    // The body is buffered because the header's `idspace:` lines can only be
    // known once every clause has been rendered (see `Ctx`), and the header is
    // written first.
    let mut body: Vec<u8> = Vec::new();

    // A class declared but with no stanza content (a bare reference, e.g. an
    // imported filler used in a relationship) gets no `[Term]` stanza — it is
    // re-declared from its references on reload, which is why a released `.obo` has
    // far fewer `[Term]` stanzas than declared classes. Emitting a stub stanza
    // would add a spurious `oboInOwl:id`.
    // Stanzas are ordered by their rendered id with a plain case-SENSITIVE
    // comparison — so every CURIE typedef (`BFO:0000066`, `RO:0000052`) precedes
    // every shorthand one (`aboral_to`, `part_of`), because an uppercase letter
    // sorts before a lowercase one. This is not the IRI order a `BTreeSet` gives
    // (`BFO_0000050` = part_of before `BFO_0000066`).
    let by_rendered_id = |set: &BTreeSet<String>| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = set.iter().map(|i| (ctx.id(i), i.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    for (_, class) in by_rendered_id(&classes) {
        if alt_classes.contains(&class) {
            continue;
        }
        match data.get(&class) {
            Some(sd) if has_content(sd) => {
                writeln!(body, "[Term]")?;
                let cap = owlapi_set_cap(aa_counts.get(&class).copied().unwrap_or(0));
                write_stanza(&mut body, &ctx, &labels, &class, Some(sd), Stanza2::Term, cap, subclass_cap)?;
                writeln!(body)?;
            }
            _ => {}
        }
    }
    // Every `[Typedef]` — an object property with content, and an annotation
    // property explicitly tagged `is_metadata_tag` — belongs to ONE id-sorted run,
    // interleaved (`RO:0002131` between `CL:…` and `IAO:…`), not
    // object properties first and metadata tags appended. A bare-referenced object
    // property (a relationship predicate with no typedef content) is re-declared
    // from its use on reload, so it gets no stanza; likewise subset/synonymtype
    // properties, which are header `subsetdef:`/`synonymtypedef:` lines.
    let mut typedefs: Vec<(String, String, Stanza2)> = Vec::new();
    for prop in &obj_props {
        if data.get(prop).map(has_content).unwrap_or(false) {
            typedefs.push((ctx.id(prop), prop.clone(), Stanza2::ObjectProperty));
        }
    }
    for prop in ann_props.difference(&obj_props) {
        let sd = data.get(prop);
        let header_def = sd.map(|s| s.subset_property || s.synonymtype_property).unwrap_or(false);
        let metadata = sd.map(|s| s.is_metadata_tag).unwrap_or(false);
        if metadata && !header_def {
            typedefs.push((ctx.id(prop), prop.clone(), Stanza2::AnnotationProperty));
        }
    }
    typedefs.sort_by(|a, b| a.0.cmp(&b.0));
    // One stanza per OBO ID, not per IRI. Properties from different namespaces can
    // share a local name — the life-stage ontologies each define `end_dpb`,
    // `has_end_time` and eighteen more — and an OBO id IS the identity, so their
    // stanzas MERGE: the clauses are pooled and sorted within each tag (the
    // reference's single `end_dpb` carries four `namespace:` lines). Every tag
    // pools as a SET — two members with the same `namespace:`, `synonym:`, `xref:`
    // or `alt_id:` yield one line — except `property_value:`, which keeps one line
    // per member, duplicates and all.
    // Rendering is per member and merged as text, so a stanza with no duplicate id
    // is written exactly as before.
    let mut i = 0usize;
    while i < typedefs.len() {
        let mut j = i + 1;
        while j < typedefs.len() && typedefs[j].0 == typedefs[i].0 {
            j += 1;
        }
        writeln!(body, "[Typedef]")?;
        if j - i == 1 {
            let (_, prop, kind) = &typedefs[i];
            let cap = owlapi_set_cap(aa_counts.get(prop).copied().unwrap_or(0));
            write_stanza(&mut body, &ctx, &labels, prop, data.get(prop), *kind, cap, subclass_cap)?;
        } else {
            let mut tags: Vec<String> = Vec::new();
            let mut by_tag: HashMap<String, Vec<String>> = HashMap::new();
            for (_, prop, kind) in &typedefs[i..j] {
                let mut buf: Vec<u8> = Vec::new();
                let cap = owlapi_set_cap(aa_counts.get(prop).copied().unwrap_or(0));
                write_stanza(&mut buf, &ctx, &labels, prop, data.get(prop), *kind, cap, subclass_cap)?;
                for line in String::from_utf8_lossy(&buf).lines() {
                    let tag = line.split_once(':').map(|(t, _)| t.to_string()).unwrap_or_default();
                    if !by_tag.contains_key(&tag) {
                        tags.push(tag.clone());
                    }
                    let repeats = tag == "property_value";
                    let e = by_tag.entry(tag).or_default();
                    if repeats || !e.iter().any(|x| x == line) {
                        e.push(line.to_string());
                    }
                }
            }
            for tag in &tags {
                let mut lines = by_tag.remove(tag).unwrap_or_default();
                // Case-INSENSITIVELY, with a case-sensitive tie-break — the same
                // law every other tag sorts by, so `dog_stages_ontology` precedes
                // `Dpseudobscura_stages_ontology` and both precede `gorilla_…`.
                lines.sort_by(|a, b| fold(a).cmp(&fold(b)).then_with(|| a.cmp(b)));
                for line in lines {
                    writeln!(body, "{line}")?;
                }
            }
        }
        writeln!(body)?;
        i = j;
    }

    // `subsetdef:` / `synonymtypedef:` header lines (reconstructed from the
    // sub-property-of oboInOwl:SubsetProperty / SynonymTypeProperty axioms plus
    // the property's comment / label).
    let mut subsetdefs: Vec<(String, String)> = Vec::new();
    let mut syntypedefs: Vec<(String, String)> = Vec::new();
    for (iri, sd) in &data {
        if sd.subset_property {
            let descr = sd.comments.first().map(|(t, _)| t.clone()).unwrap_or_default();
            subsetdefs.push((ctx.id(iri), descr));
        } else if sd.synonymtype_property {
            let descr = sd.name.as_ref().map(|(t, _)| t.clone()).unwrap_or_default();
            syntypedefs.push((ctx.id(iri), descr));
        }
    }
    subsetdefs.sort_by_key(|(id, d)| (fold(id), d.clone()));
    subsetdefs.dedup();
    syntypedefs.sort_by_key(|(id, d)| (fold(id), d.clone()));
    syntypedefs.dedup();

    // Header, in OBO's header-tag order (format-version, data-version,
    // import, subsetdef, synonymtypedef, idspace, remark, ontology,
    // property_value) — the order released files such as CL's `cl.obo` carry.
    // The default is 1.2, not 1.4: `format-version: 1.2` is what released OBO files
    // carry, so a model that reaches the writer with no
    // `oboInOwl:hasOBOFormatVersion` (one read from OWL, say) is stamped with that.
    // A header directive's tag lines (`tag: value`), in the collected+sorted
    // order. Written at the fixed header position for that tag.
    macro_rules! emit_directive {
        ($tag:expr) => {
            if let Some(vals) = directives.get($tag) {
                for v in vals {
                    writeln!(writer, "{}: {}", $tag, escape_unquoted(v))?;
                }
            }
        };
    }

    writeln!(writer, "format-version: {}", format_version.as_deref().unwrap_or("1.2"))?;
    if let Some(dv) = data_version(ont_iri.as_deref(), ont_version_iri.as_deref()) {
        writeln!(writer, "data-version: {dv}")?;
    }
    emit_directive!("date");
    emit_directive!("saved-by");
    emit_directive!("auto-generated-by");
    for imp in &imports {
        writeln!(writer, "import: {imp}")?;
    }
    for (id, descr) in &subsetdefs {
        writeln!(writer, "subsetdef: {id} \"{}\"", escape(descr))?;
    }
    for (id, descr) in &syntypedefs {
        writeln!(writer, "synonymtypedef: {id} \"{}\"", escape(descr))?;
    }
    emit_directive!("default-namespace");
    emit_directive!("namespace-id-rule");
    // The trailing space is deliberate: `idspace:` has an optional third
    // (quoted description) field, and its separator is always emitted.
    let used = ctx.used.borrow();
    let mut idspaces: Vec<(String, String)> = if model.idspaces.is_empty()
        || !model.explicit_prefixes.is_empty()
    {
        // No scanned prefix map (an obo→obo trip, or a pipeline-built model). An
        // `idspace:` line belongs only to a prefix that actually *shortened an id*
        // in the body — a declared-but-unused alias is dropped. CL declares both
        // `terms:` and `dcterms:` for `http://purl.org/dc/terms/` but only ever
        // renders `terms:`, so its header lists `idspace: terms` and not `dcterms`;
        // EFO uses both (`dcterms:` 20k times, `terms:` once) and lists both — hence
        // the filter on `used`.
        // Emit an `idspace:` for every prefix actually used to shorten an id, plus —
        // for a *declared* namespace that no used prefix covers — its first-declared
        // prefix, so the namespace is still represented. This is why CL lists
        // `idspace: swrl` (declared, but never shortening an obo id, and the only
        // prefix for `swrl#`) yet omits `dcterms`/`dce` (declared-but-unused aliases
        // of `dc/terms/` and `dc/elements/`, which `terms`/`dc` already cover).
        // `declared` names get an `idspace:` when their namespace isn't already
        // covered by a *used* prefix; it is the CURIE map plus the document xmlns
        // (`rdf_prefixes`) — so e.g. CL's `sssom`, declared in `cl.owl`'s header but
        // abbreviating nothing (its xref stays a full IRI), still gets a line, while
        // `terms` stays suppressed because `dcterms` covers its namespace.
        //
        // A document declaring only owl/rdf/xsd/rdfs/obo therefore converts to OBO
        // with NO `idspace:` lines at all, and every IRI outside those namespaces
        // stays full.
        let declared: std::collections::HashSet<&str> = model
            .prefixes
            .mappings()
            .map(|(p, _)| p.as_str())
            .chain(model.rdf_prefixes.iter().map(|(p, _)| p.as_str()))
            .collect();
        // Prefixes from an explicit `--prefixes`/`--add-prefixes` context: EVERY one
        // gets an `idspace:`, whether or not it shortens an id (so mondo's `ICD11`
        // appears with zero references).
        let explicit: std::collections::HashSet<&str> =
            model.explicit_prefixes.iter().map(|(p, _)| p.as_str()).collect();
        let used_ns: std::collections::HashSet<&str> = ctx
            .idspaces
            .iter()
            .filter(|(p, _)| used.contains(p))
            .map(|(_, n)| n.as_str())
            .collect();
        let mut covered_ns: std::collections::HashSet<&str> = std::collections::HashSet::new();
        ctx.idspaces
            .iter()
            .filter(|(p, n)| {
                if used.contains(p) || explicit.contains(p.as_str()) {
                    return true;
                }
                if declared.contains(p.as_str())
                    && !used_ns.contains(n.as_str())
                    && covered_ns.insert(n.as_str())
                {
                    return true;
                }
                false
            })
            .map(|(p, n)| (p.clone(), n.clone()))
            .collect()
    } else {
        // An OWL source: emit the exact prefix set the document declares or uses.
        model.idspaces.clone()
    };
    // Header order: case-insensitive by prefix, ties broken by the prefix ASCENDING
    // (so a same-fold pair like `ICD10CM`/`icd10cm` lists uppercase first — the
    // OPPOSITE of the abbreviation tie-break, which prefers the alpha-greatest).
    idspaces.sort_by(|a, b| fold(&a.0).cmp(&fold(&b.0)).then_with(|| a.0.cmp(&b.0)));
    for (prefix, ns) in &idspaces {
        writeln!(writer, "idspace: {prefix} {ns} ")?;
    }
    emit_directive!("treat-xrefs-as-equivalent");
    emit_directive!("treat-xrefs-as-genus-differentia");
    emit_directive!("treat-xrefs-as-relationship");
    emit_directive!("treat-xrefs-as-is_a");
    for r in &remarks {
        writeln!(writer, "remark: {}", escape_unquoted(r))?;
    }
    if let Some(iri) = &ont_iri {
        // The ontology id strips the OBO PURL base UNCONDITIONALLY and strips a
        // trailing `.owl` only when there is one. Requiring both would leave any
        // non-`.owl` OBO IRI unshortened: HPO's `test_obo` target annotates with
        // `…/obo/test_obo`, and `hp.obo`'s own rule with `…/obo/hp.obo`, which must
        // come out as `ontology: test_obo` / `ontology: hp.obo`.
        let short = match iri.strip_prefix(OBO_BASE) {
            Some(rest) => rest.strip_suffix(".owl").unwrap_or(rest),
            None => iri,
        };
        writeln!(writer, "ontology: {short}")?;
    } else {
        // An ANONYMOUS ontology still declares the directive, with an empty value:
        // the fourteen `*-minimal.obo` subsets are written from ontologies that
        // carry no IRI, and each has a bare `ontology: ` line.
        writeln!(writer, "ontology: ")?;
    }
    // Ontology-level annotations → header `property_value:` lines (the reader
    // re-creates them as OntologyAnnotation and declares their predicates).
    for (pred, val, is_iri, dt) in &ont_anns {
        if *is_iri {
            writeln!(writer, "property_value: {pred} {val}")?;
        } else {
            // A literal `property_value` must carry a datatype — a bare quoted
            // value is misread as an IRI. Default to xsd:string.
            let dt_tok = dt.clone().unwrap_or_else(|| "xsd:string".into());
            writeln!(writer, "property_value: {pred} \"{}\" {dt_tok}", escape(val))?;
        }
    }
    // `logical-definition-view-relation:` is written after the `property_value:`
    // block — it sorts last among the recognised header tags.
    emit_directive!("logical-definition-view-relation");
    // The `owl-axioms:` clause — OWL functional syntax carrying the axioms OBO has
    // no tag for — sits after `property_value:` and before the trailing treat-xrefs
    // directives. Its value is OBO-escaped (newline → `\n`, `"` → `\"`, `\` → `\\`,
    // tab → `\t`).
    {
        // `--clean-obo drop-untranslatable-axioms` throws the untranslatable
        // remainder away instead of parking it here, so the header is omitted
        // entirely (see `Model::obo_drop_untranslatable`).
        let unt =
            if model.obo_drop_untranslatable { Vec::new() } else { collect_untranslatable(model) };
        let mut oa_labels: HashMap<String, String> = HashMap::new();
        for ac in &unt {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if aa.ann.ap.0.as_ref() == RDFS_LABEL {
                    if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(l)) =
                        (&aa.subject, &aa.ann.av)
                    {
                        oa_labels.insert(s.as_ref().to_string(), l.literal().clone());
                    }
                }
            }
        }
        if let Some(block) = crate::io::owlfunc::render_owl_axioms(&unt, &oa_labels) {
            let escaped = block
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\t', "\\t");
            writeln!(writer, "owl-axioms: {escaped}")?;
        }
    }
    // These two treat-xrefs directives go at the very end of the header (after
    // `ontology:`/`property_value:`), unlike the four above.
    emit_directive!("treat-xrefs-as-has-subclass");
    emit_directive!("treat-xrefs-as-reverse-genus-differentia");
    writeln!(writer)?;
    writer.write_all(&body)?;
    Ok(())
}

/// The `data-version:` header value: the version IRI relative to the ontology id
/// — strip the OBO PURL base, then the leading
/// `<ontology-id>/` and the trailing `/<ontology-id>.owl`. CL's
/// `…/obo/cl/releases/2026-06-08/cl.owl` becomes `releases/2026-06-08`.
fn data_version(ont_iri: Option<&str>, version_iri: Option<&str>) -> Option<String> {
    let v = version_iri?;
    let mut vs = v.strip_prefix(OBO_BASE).unwrap_or(v).to_string();
    // Only OBO-library ontologies carry the `{id}/releases/…/{id}.owl` shape that
    // is shortened against the ontology id; a non-OBO version IRI (EFO's
    // `http://www.ebi.ac.uk/efo/releases/v3.91.0/efo.owl`) is kept verbatim rather
    // than dropping the whole `data-version:` line.
    if let Some(oid) = ont_iri
        .and_then(|iri| iri.strip_prefix(OBO_BASE))
        .and_then(|s| s.strip_suffix(".owl"))
    {
        if let Some(rest) = vs.strip_prefix(&format!("{oid}/")) {
            vs = rest.to_string();
        }
        vs = vs.replace(&format!("/{oid}.owl"), "");
    }
    Some(vs)
}

/// Fold the deprecated "terms merged" stubs into their target's `alt_id:`.
///
/// In OWL, `alt_id: X` on term T is a *separate* deprecated class X carrying
/// `IAO:0100001 replaced_by T` plus obsolescence reason `IAO:0000227` ("terms
/// merged"). Writing OBO again collapses that stub back into T's `alt_id:` and
/// gives it no stanza of its own. Handling only the explicit
/// `oboInOwl:hasAlternativeId` spelling turns CL's 76 merged ids into 76 bogus
/// obsolete `[Term]` stanzas with no `alt_id:` lines at all.
///
/// Returns `(stubs, targets)`: stub IRIs whose stanza must be suppressed, and the
/// merge-target IRIs that gain an `alt_id:` — the latter must be rendered as a
/// `[Term]` even when the target class is otherwise undeclared/contentless (e.g.
/// CHEBI:16422, only referenced as CHEBI:2709's replacement).
fn fold_alt_ids(
    ctx: &Ctx,
    data: &mut BTreeMap<String, SubjData>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut merged: Vec<(String, String)> = Vec::new(); // (target iri, alt id)
    let mut stubs: BTreeSet<String> = BTreeSet::new();
    let mut targets: BTreeSet<String> = BTreeSet::new();
    // A stub that also carries LOGICAL axioms keeps a stanza — of those axioms
    // alone. EMAPA:16045 is a merged stub whose whole content is an
    // `intersection_of` and a `relationship`; folding it away lost both, and the
    // fold takes only the obsolescence bookkeeping with it (its name, synonyms
    // and `is_obsolete:` go too, not just its id).
    let mut logical_only: Vec<String> = Vec::new();
    for (iri, sd) in data.iter() {
        if sd.deprecated
            && sd.obsolescence_reason.as_deref() == Some("IAO:0000227")
            && sd.replaced_by.len() == 1
        {
            merged.push((expand_id(&sd.replaced_by[0]), ctx.id(iri)));
            if sd.is_a.is_empty()
                && sd.relationships.is_empty()
                && sd.intersection_of.is_empty()
                && sd.union_of.is_empty()
                && sd.equivalent_to.is_empty()
                && sd.disjoint_from.is_empty()
            {
                stubs.insert(iri.clone());
            } else {
                logical_only.push(iri.clone());
            }
        }
    }
    for iri in logical_only {
        if let Some(sd) = data.get_mut(&iri) {
            let keep = SubjData {
                id: sd.id.take(),
                is_a: std::mem::take(&mut sd.is_a),
                relationships: std::mem::take(&mut sd.relationships),
                intersection_of: std::mem::take(&mut sd.intersection_of),
                union_of: std::mem::take(&mut sd.union_of),
                equivalent_to: std::mem::take(&mut sd.equivalent_to),
                disjoint_from: std::mem::take(&mut sd.disjoint_from),
                ..Default::default()
            };
            *sd = keep;
        }
    }
    for (target, alt) in merged {
        targets.insert(target.clone());
        let e = data.entry(target).or_default();
        // The same id can arrive twice — once as an explicit
        // `oboInOwl:hasAlternativeId` on the primary term and once as the merged
        // stub the OBO reader materialises from it — and it is listed once.
        if !e.alt_ids.contains(&alt) {
            e.alt_ids.push(alt);
        }
    }
    // Stubs materialised from an explicit `oboInOwl:hasAlternativeId` are skipped
    // too — but only when they are bare. A class with content of its own keeps
    // its stanza even when another term lists it as an `alt_id:`, and content is
    // not only a name or a definition: EMAPA:16045 is nothing but an
    // `intersection_of` and a `relationship`, and it is still a term.
    let alt_targets: Vec<String> = data
        .values()
        .flat_map(|sd| sd.alt_ids.iter().map(|a| expand_id(a)))
        .collect();
    for a in alt_targets {
        let is_real = data.get(&a).is_some_and(has_content);
        if !is_real {
            stubs.insert(a);
        }
    }
    (stubs, targets)
}

/// What kind of stanza is being written (controls is_metadata_tag and which
/// property axioms apply).
#[derive(Clone, Copy, PartialEq)]
enum Stanza2 {
    Term,
    ObjectProperty,
    AnnotationProperty,
}

/// Whether a subject carries any OBO stanza content (so it warrants a stanza).
/// A bare reference (only declared, used as an axiom filler) has none.
fn has_content(sd: &SubjData) -> bool {
    sd.id.is_some()
        || sd.name.is_some()
        || !sd.namespace.is_empty()
        || sd.def.is_some()
        || sd.deprecated
        || !sd.comments.is_empty()
        || !sd.synonyms.is_empty()
        || !sd.xrefs.is_empty()
        || !sd.subsets.is_empty()
        || !sd.alt_ids.is_empty()
        || !sd.replaced_by.is_empty()
        || !sd.consider.is_empty()
        || !sd.created_by.is_empty()
        || !sd.creation_date.is_empty()
        || !sd.property_values.is_empty()
        || !sd.is_a.is_empty()
        || !sd.relationships.is_empty()
        || !sd.intersection_of.is_empty()
        || !sd.union_of.is_empty()
        || !sd.equivalent_to.is_empty()
        || !sd.disjoint_from.is_empty()
        || !sd.sub_property_of.is_empty()
        || !sd.domain.is_empty()
        || !sd.range.is_empty()
        || !sd.inverse_of.is_empty()
        || !sd.chains.is_empty()
        || sd.transitive
        || sd.symmetric
        || sd.reflexive
        || sd.asymmetric
        || sd.functional
        || sd.inverse_functional
        || sd.is_metadata_tag
        || sd.is_class_level
        || !sd.expand_expression_to.is_empty()
        || !sd.expand_assertion_to.is_empty()
}

/// A subject whose ONLY obo content is one or more `name:` clauses — no id, def,
/// xref, relationship, characteristic, or any other clause. Used to detect the
/// `develops_from` case: two object properties (`RO:0002202` via its shorthand and
/// `bto#develops_from` via its local name) render to the same typedef id, but the
/// bto one carries nothing but a name, so that lone name folds into the single
/// merged `[Typedef]` rather than becoming a second, near-empty stanza.
fn is_name_only(sd: &SubjData) -> bool {
    sd.name.is_some()
        && sd.id.is_none()
        && !sd.deprecated
        && sd.def.is_none()
        && sd.extra_defs.is_empty()
        && sd.namespace.is_empty()
        && sd.comments.is_empty()
        && sd.synonyms.is_empty()
        && sd.xrefs.is_empty()
        && sd.subsets.is_empty()
        && sd.alt_ids.is_empty()
        && sd.replaced_by.is_empty()
        && sd.consider.is_empty()
        && sd.created_by.is_empty()
        && sd.creation_date.is_empty()
        && sd.property_values.is_empty()
        && sd.rel_or_pv.is_empty()
        && sd.is_a.is_empty()
        && sd.relationships.is_empty()
        && sd.intersection_of.is_empty()
        && sd.union_of.is_empty()
        && sd.equivalent_to.is_empty()
        && sd.disjoint_from.is_empty()
        && sd.sub_property_of.is_empty()
        && sd.domain.is_empty()
        && sd.range.is_empty()
        && sd.inverse_of.is_empty()
        && sd.chains.is_empty()
        && !sd.transitive
        && !sd.symmetric
        && !sd.reflexive
        && !sd.asymmetric
        && !sd.functional
        && !sd.inverse_functional
        && !sd.is_metadata_tag
        && !sd.is_class_level
        && sd.expand_expression_to.is_empty()
        && sd.expand_assertion_to.is_empty()
}

#[allow(clippy::too_many_arguments)]
fn record_ac(
    ac: &horned_owl::model::AnnotatedComponent<RcStr>,
    ctx: &Ctx,
    classes: &mut BTreeSet<String>,
    obj_props: &mut BTreeSet<String>,
    ann_props: &mut BTreeSet<String>,
    data: &mut BTreeMap<String, SubjData>,
    ont_iri: &mut Option<String>,
    ont_version_iri: &mut Option<String>,
    ont_anns: &mut Vec<(String, String, bool, Option<String>)>,
    remarks: &mut Vec<String>,
    imports: &mut Vec<String>,
    format_version: &mut Option<String>,
    directives: &mut HashMap<&'static str, Vec<String>>,
) {
    let comp = &ac.component;
    let axanns = &ac.ann;
    match comp {
        Component::OntologyID(id) => {
            if let Some(iri) = &id.iri {
                *ont_iri = Some(iri.as_ref().to_string());
            }
            if let Some(viri) = &id.viri {
                *ont_version_iri = Some(viri.as_ref().to_string());
            }
        }
        Component::OntologyAnnotation(oa) => {
            let (val, is_iri, dt) = ann_value_ctx(ctx, &oa.0.av);
            if let Some(tag) = header_directive_tag(oa.0.ap.0.as_ref()) {
                // A dedicated OBO header directive (default-namespace,
                // treat-xrefs-as-*, date, …), not a header `property_value:`.
                directives.entry(tag).or_default().push(val);
            } else if oa.0.ap.0.as_ref() == format!("{OIO}hasOBOFormatVersion") {
                *format_version = Some(val);
            } else if oa.0.ap.0.as_ref() == RDFS_COMMENT {
                // An ontology-level rdfs:comment is the OBO header `remark:` tag
                // (CL's "See PMID:15693950 …; Contact Alexander Diehl …" line),
                // not a header `property_value:`.
                remarks.push(val);
            } else {
                ont_anns.push((ctx.id(oa.0.ap.0.as_ref()), val, is_iri, dt));
            }
        }
        Component::Import(i) => {
            imports.push(i.0.as_ref().to_string());
        }
        Component::DeclareClass(dc) => {
            classes.insert(dc.0 .0.as_ref().to_string());
        }
        Component::DeclareObjectProperty(dp) => {
            obj_props.insert(dp.0 .0.as_ref().to_string());
        }
        Component::DeclareAnnotationProperty(ap) => {
            ann_props.insert(ap.0 .0.as_ref().to_string());
        }
        Component::SubClassOf(sc) => {
            // Only a named subject produces a clause. A named subclass is a plain
            // `is_a:`/`relationship:`; a General Class Inclusion whose subclass is
            // `NamedClass ⊓ ∃gci_rel.gci_filler` is written on `NamedClass` with
            // `{gci_relation="…", gci_filler="…"}` qualifiers, in both the `is_a:`
            // and the `relationship:` form. That is the OBO spelling of a
            // subsumption holding only in a given taxon/context — CL:0000163 is an
            // enteroendocrine cell only as part of the stomach, and CL has 136 such
            // axioms; dropping them on write deletes them from an obo→owl→obo round
            // trip.
            let (subject, gci): (Option<String>, Vec<(String, String)>) = match &sc.sub {
                CE::Class(sub) => (Some(sub.0.as_ref().to_string()), Vec::new()),
                CE::ObjectIntersectionOf(parts) => {
                    let named = parts.iter().find_map(|p| match p {
                        CE::Class(c) => Some(c.0.as_ref().to_string()),
                        _ => None,
                    });
                    let restr = parts.iter().find_map(|p| match p {
                        CE::ObjectSomeValuesFrom { ope: OPE::ObjectProperty(r), bce } => {
                            match bce.as_ref() {
                                CE::Class(f) => {
                                    Some((ctx.id(r.0.as_ref()), ctx.id(f.0.as_ref())))
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    });
                    match (named, restr) {
                        (Some(n), Some((rel, fill))) => (
                            Some(n),
                            vec![
                                ("gci_relation".to_string(), rel),
                                ("gci_filler".to_string(), fill),
                            ],
                        ),
                        _ => (None, Vec::new()),
                    }
                }
                _ => (None, Vec::new()),
            };
            if let Some(s) = subject {
                classes.insert(s.clone());
                // The whole SubClassOf axiom's hash fixes its position in the
                // per-type axiom table — hence the tie-order of same-value
                // is_a/relationship clauses. Computed once here (full IRIs in hand); a
                // superclass conjunction that splits into several clauses shares it.
                let sc_hash = owlapi_subclassof_hash(ctx, &sc.sub, &sc.sup, &axanns);
                let e = data.entry(s).or_default();
                // The GCI context rides along as extra qualifiers on the line.
                let gci_quals = gci;
                // A conjunction in superclass position is not one clause but
                // several: `C ⊑ (∃has_part.X ⊓ ∃has_part.Y)` is two
                // `relationship:` lines, exactly as if it had been written as two
                // SubClassOf axioms. OBO has no way to say "and" on the right-hand
                // side, so the axiom splits. Dropping it whole would cost CL's
                // `cl.obo` 821 `relationship:` lines, all of CLM's multi-gene
                // NS-forest marker sets.
                // Each queued expression carries whether it was reached by
                // descending into a superclass CONJUNCTION, which decides the
                // `{all_only="true"}` qualifier below.
                let mut queue: std::collections::VecDeque<(&CE<RcStr>, bool)> =
                    std::collections::VecDeque::new();
                // An all-restriction superclass intersection splits
                // (`C ⊑ (∃R.X ⊓ ∃R.Y)` → two `relationship:` lines), but only when
                // EVERY conjunct is translatable. If any operand is untranslatable —
                // a mixed intersection with a *named-class* conjunct (EFO's BTO cell
                // lines, `BTO ⊑ (CL:0000010 ⊓ ∃RO_0000053.MONDO)`), or a restriction
                // with a non-named filler (`HP:0001891 ⊑ (∃RO_0000056.(…complex…) ⊓
                // ∃RO_0000057.CHEBI_18248)`) — the WHOLE axiom goes to the
                // `owl-axioms:` bag and NEITHER conjunct is emitted. Partially
                // splitting it (emitting only the simple operand) would put a
                // spurious `relationship:` line in the frame. `subclassof_
                // untranslatable` is the reachability test.
                if !subclassof_untranslatable(&sc.sub, &sc.sup) {
                    queue.push_back((&sc.sup, false));
                }
                // Clauses this ONE axiom contributes; deduped before they join the
                // frame (see the note on the conjunction branch below).
                let rel_start = e.relationships.len();
                while let Some((sup, in_inter)) = queue.pop_front() {
                    match sup {
                        CE::Class(sup) => {
                            // `C ⊑ owl:Thing` is vacuous, so `is_a: owl:Thing` is
                            // never written.
                            if sup.0.as_ref() != format!("{OWL_NS}Thing") {
                                e.is_a.push((
                                    ctx.id(sup.0.as_ref()),
                                    axanns.clone(),
                                    gci_quals.clone(),
                                    sc_hash,
                                ));
                            }
                        }
                        CE::ObjectSomeValuesFrom { ope, bce } => {
                            if let (OPE::ObjectProperty(r), CE::Class(t)) = (ope, bce.as_ref()) {
                                e.relationships.push((
                                    ctx.id(r.0.as_ref()),
                                    ctx.id(t.0.as_ref()),
                                    axanns.clone(),
                                    gci_quals.clone(),
                                    sc_hash,
                                ));
                            }
                        }
                        // A universal restriction is the same `REL FILLER` clause
                        // flagged `all_only` — BFO's `part_of only continuant`
                        // reads `relationship: BFO:0000050 BFO:0000002
                        // {all_only="true"}`. Cardinality bounds ride along the
                        // same way as in an `intersection_of:` clause.
                        CE::ObjectAllValuesFrom { ope, bce } => {
                            if let (OPE::ObjectProperty(r), CE::Class(t)) = (ope, bce.as_ref()) {
                                let mut q = gci_quals.clone();
                                // …but ONLY when the universal is the whole
                                // superclass. Inside a conjunction every conjunct
                                // is a PLAIN `relationship:` clause:
                                //   C ⊑ (∃R.Y ⊓ ∀R.Z)  →  `R Y` and `R Z`, unqualified.
                                if !in_inter {
                                    q.push(("all_only".to_string(), "true".to_string()));
                                }
                                e.relationships.push((
                                    ctx.id(r.0.as_ref()),
                                    ctx.id(t.0.as_ref()),
                                    axanns.clone(),
                                    q,
                                    sc_hash,
                                ));
                            }
                        }
                        CE::ObjectExactCardinality { .. }
                        | CE::ObjectMinCardinality { .. }
                        | CE::ObjectMaxCardinality { .. } => {
                            if let Some((toks, q)) = ce_to_inter_tokens(ctx, sup) {
                                if toks.len() == 2 {
                                    let mut quals = gci_quals.clone();
                                    quals.extend(q);
                                    e.relationships.push((
                                        toks[0].clone(),
                                        toks[1].clone(),
                                        axanns.clone(),
                                        quals,
                                        sc_hash,
                                    ));
                                }
                            }
                        }
                        CE::ObjectIntersectionOf(parts) => {
                            queue.extend(parts.iter().map(|p| (p, true)))
                        }
                        _ => {}
                    }
                }
                // The conjuncts are a SET, so two of them that render to the same
                // clause collapse to one line — MONDO's FOODON_03400229 is
                // `⊑ (∃has_member.X ⊓ ∀has_member.X)`, which is a single
                // `relationship: RO:0002351 FOODON:03301977`. The dedup is strictly
                // within this axiom: a separate `SubClassOf(C, ∃R.Y)` axiom still
                // adds its own identical clause.
                if e.relationships.len() > rel_start + 1 {
                    let mut seen: std::collections::HashSet<(String, String, Vec<(String, String)>)> =
                        std::collections::HashSet::new();
                    let mut i = rel_start;
                    while i < e.relationships.len() {
                        let (r, t, _, q, _) = &e.relationships[i];
                        if seen.insert((r.clone(), t.clone(), q.clone())) {
                            i += 1;
                        } else {
                            e.relationships.remove(i);
                        }
                    }
                }
            }
        }
        Component::EquivalentClasses(eq) => {
            // Named class C ≡ expr → intersection_of / union_of / equivalent_to.
            let named: Vec<&str> = eq.0.iter().filter_map(|m| match m {
                CE::Class(c) => Some(c.0.as_ref()),
                _ => None,
            }).collect();
            if let Some(subj) = named.first().map(|s| s.to_string()) {
                let e = data.entry(subj).or_default();
                for m in &eq.0 {
                    match m {
                        CE::Class(_) => {} // handled as subject / equivalent_to below
                        CE::ObjectIntersectionOf(parts) => {
                            // All-or-nothing, but only for the FIRST definition on a
                            // frame. An operand OBO cannot spell (a nested anonymous
                            // filler such as CL:0000041's `has_part some (nucleus and
                            // bearer_of some polymorphic)`) is fatal to the *whole*
                            // equivalence — none of its clauses are added — EXCEPT
                            // when the frame already carries `intersection_of:`
                            // clauses from an earlier axiom. Then the unspellable
                            // operand is merely skipped, so the rest of this axiom
                            // still lands. Eight EFO classes have a second definition
                            // whose filler is a union, and its genus line comes out
                            // as a second `intersection_of: EFO:0000408`.
                            let had_inter = !e.intersection_of.is_empty();
                            let mut untranslatable = false;
                            let mut lines: Vec<(Vec<String>, Vec<(String, String)>)> = Vec::new();
                            for p in parts {
                                match ce_to_inter_tokens(ctx, p) {
                                    Some(toks) => lines.push(toks),
                                    None => {
                                        if !had_inter {
                                            untranslatable = true;
                                        }
                                    }
                                }
                            }
                            if !untranslatable {
                                // The axiom annotations belong to every line —
                                // `{xref="PMID:27565351"}` repeats on each
                                // `intersection_of:` of CL:0000754.
                                e.intersection_of.extend(
                                    lines.into_iter().map(|(toks, q)| (toks, q, axanns.clone())),
                                );
                            }
                        }
                        CE::ObjectUnionOf(parts) => {
                            for p in parts {
                                if let CE::Class(c) = p {
                                    e.union_of.push(ctx.id(c.0.as_ref()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Pairwise named equivalences (C ≡ D) → equivalent_to.
                for other in &named[1..] {
                    let e = data.entry(named[0].to_string()).or_default();
                    e.equivalent_to.push(ctx.id(other));
                }
            }
        }
        Component::DisjointClasses(dj) => {
            let named: Vec<&str> = dj.0.iter().filter_map(|m| match m {
                CE::Class(c) => Some(c.0.as_ref()),
                _ => None,
            }).collect();
            if named.len() == 2 {
                classes.insert(named[0].to_string());
                let e = data.entry(named[0].to_string()).or_default();
                e.disjoint_from.push((ctx.id(named[1]), axanns.clone()));
            } else if named.len() >= 3 {
                // A nary DisjointClasses maps to a SINGLE `disjoint_from:` clause,
                // on the first two members in IRI order — `DisjointClasses(A B C)`
                // in any input order yields `A disjoint_from B`. The other pairs are
                // dropped, as OBO has no nary disjoint form.
                let mut sorted = named.clone();
                sorted.sort_unstable();
                classes.insert(sorted[0].to_string());
                let e = data.entry(sorted[0].to_string()).or_default();
                e.disjoint_from.push((ctx.id(sorted[1]), axanns.clone()));
            }
        }
        Component::AnnotationAssertion(aa) => {
            if let AnnotationSubject::IRI(subj) = &aa.subject {
                record_annotation(ctx, subj.as_ref(), &aa.ann, axanns, data);
            }
        }
        // --- Object-property axioms → Typedef facts. ---
        Component::SubObjectPropertyOf(sp) => {
            use horned_owl::model::SubObjectPropertyExpression as SOPE;
            if let OPE::ObjectProperty(sup) = &sp.sup {
                match &sp.sub {
                    SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)) => {
                        // `R ⊑ owl:topObjectProperty` is vacuous, so
                        // `is_a: owl:topObjectProperty` is never written (RO_0015001
                        // in CL's import closure carries one).
                        if sup.0.as_ref() != format!("{OWL_NS}topObjectProperty") {
                            let e = data.entry(sub.0.as_ref().to_string()).or_default();
                            e.sub_property_of.push(ctx.id(sup.0.as_ref()));
                        }
                    }
                    SOPE::ObjectPropertyChain(chain) => {
                        // R1∘…∘Rn ⊑ sup → holds_over_chain on `sup`.
                        let toks: Vec<String> = chain.iter().filter_map(|o| match o {
                            OPE::ObjectProperty(p) => Some(ctx.id(p.0.as_ref())),
                            _ => None,
                        }).collect();
                        let e = data.entry(sup.0.as_ref().to_string()).or_default();
                        e.chains.push((toks, axanns.clone()));
                    }
                    _ => {}
                }
            }
        }
        Component::SubAnnotationPropertyOf(sp) => {
            let sup = sp.sup.0.as_ref();
            let e = data.entry(sp.sub.0.as_ref().to_string()).or_default();
            if sup == format!("{OIO}SubsetProperty") {
                e.subset_property = true;
            } else if sup == format!("{OIO}SynonymTypeProperty") {
                e.synonymtype_property = true;
            } else {
                e.sub_property_of.push(ctx.id(sup));
            }
        }
        Component::TransitiveObjectProperty(p) => set_op_flag(data, &p.0, |e| e.transitive = true),
        Component::SymmetricObjectProperty(p) => set_op_flag(data, &p.0, |e| e.symmetric = true),
        Component::ReflexiveObjectProperty(p) => set_op_flag(data, &p.0, |e| e.reflexive = true),
        Component::AsymmetricObjectProperty(p) => set_op_flag(data, &p.0, |e| e.asymmetric = true),
        Component::FunctionalObjectProperty(p) => set_op_flag(data, &p.0, |e| e.functional = true),
        Component::InverseFunctionalObjectProperty(p) => {
            set_op_flag(data, &p.0, |e| e.inverse_functional = true)
        }
        Component::ObjectPropertyDomain(pd) => {
            if let OPE::ObjectProperty(p) = &pd.ope {
                if let CE::Class(c) = &pd.ce {
                    data.entry(p.0.as_ref().to_string()).or_default().domain.push((ctx.id(c.0.as_ref()), axanns.clone()));
                }
            }
        }
        Component::ObjectPropertyRange(pr) => {
            if let OPE::ObjectProperty(p) = &pr.ope {
                if let CE::Class(c) = &pr.ce {
                    data.entry(p.0.as_ref().to_string()).or_default().range.push((ctx.id(c.0.as_ref()), axanns.clone()));
                }
            }
        }
        Component::InverseObjectProperties(inv) => {
            if let (OPE::ObjectProperty(a), OPE::ObjectProperty(b)) = (&inv.0, &inv.1) {
                data.entry(a.0.as_ref().to_string()).or_default().inverse_of.push(ctx.id(b.0.as_ref()));
            }
        }
        // Disjoint object properties become `disjoint_from:` in the first property's
        // [Typedef] (in_lateral_side_of is disjoint_from in_central_side_of and
        // in_right_side_of), mirroring the [Term] `DisjointClasses` handling.
        Component::DisjointObjectProperties(dj) => {
            let named: Vec<&str> = dj.0.iter().filter_map(|ope| match ope {
                OPE::ObjectProperty(p) => Some(p.0.as_ref()),
                _ => None,
            }).collect();
            if let Some((first, rest)) = named.split_first() {
                let e = data.entry(first.to_string()).or_default();
                for other in rest {
                    e.disjoint_from.push((ctx.id(other), axanns.clone()));
                }
            }
        }
        _ => {}
    }
}

fn set_op_flag<F: FnOnce(&mut SubjData)>(
    data: &mut BTreeMap<String, SubjData>,
    ope: &OPE<RcStr>,
    f: F,
) {
    if let OPE::ObjectProperty(p) = ope {
        f(data.entry(p.0.as_ref().to_string()).or_default());
    }
}

/// An `intersection_of` operand → its OBO tokens (`[genus]` or `[rel filler]`).
/// The canonical class-expression sort key: the expression's type index first,
/// then the *raw* IRIs of the property and filler. Used to pick which operand of a
/// nested `ObjectIntersectionOf` survives (the last one), which must not depend on
/// shorthand rendering.
pub(crate) fn owlapi_ce_sort_key(ce: &CE<RcStr>) -> (u8, String, String) {
    let ope_iri = |ope: &OPE<RcStr>| match ope {
        OPE::ObjectProperty(r) => r.0.as_ref().to_string(),
        OPE::InverseObjectProperty(r) => r.0.as_ref().to_string(),
    };
    let bce_iri = |bce: &CE<RcStr>| match bce {
        CE::Class(t) => t.0.as_ref().to_string(),
        _ => String::new(),
    };
    match ce {
        CE::Class(c) => (10, c.0.as_ref().to_string(), String::new()),
        CE::ObjectIntersectionOf(_) => (31, String::new(), String::new()),
        CE::ObjectSomeValuesFrom { ope, bce } => (34, ope_iri(ope), bce_iri(bce)),
        CE::ObjectAllValuesFrom { ope, bce } => (35, ope_iri(ope), bce_iri(bce)),
        CE::ObjectMinCardinality { ope, bce, .. } => (37, ope_iri(ope), bce_iri(bce)),
        CE::ObjectExactCardinality { ope, bce, .. } => (38, ope_iri(ope), bce_iri(bce)),
        CE::ObjectMaxCardinality { ope, bce, .. } => (39, ope_iri(ope), bce_iri(bce)),
        _ => (99, String::new(), String::new()),
    }
}

fn ce_to_inter_tokens(ctx: &Ctx, ce: &CE<RcStr>) -> Option<(Vec<String>, Vec<(String, String)>)> {
    match ce {
        CE::Class(c) => Some((vec![ctx.id(c.0.as_ref())], Vec::new())),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            if let (OPE::ObjectProperty(r), CE::Class(t)) = (ope, bce.as_ref()) {
                Some((vec![ctx.id(r.0.as_ref()), ctx.id(t.0.as_ref())], Vec::new()))
            } else {
                None
            }
        }
        // A cardinality restriction is the same `REL FILLER` clause carrying the
        // bound as a qualifier — UBERON's `zygapophysis ≡ skeletal joint and
        // connects exactly 2 vertebral centrum` is
        // `intersection_of: RO:0002176 UBERON:0001075 {cardinality="2"}`.
        // A universal restriction is the same `REL FILLER` clause flagged
        // `all_only` — OBI:0002076 `≡ material entity and has member only
        // specimen` is `intersection_of: RO:0002351 OBI:0100051
        // {all_only="true"}`. (A universal inside a SUPERCLASS conjunction is
        // rendered unqualified instead; that path is handled separately.)
        CE::ObjectAllValuesFrom { ope, bce } => {
            if let (OPE::ObjectProperty(r), CE::Class(t)) = (ope, bce.as_ref()) {
                Some((
                    vec![ctx.id(r.0.as_ref()), ctx.id(t.0.as_ref())],
                    vec![("all_only".to_string(), "true".to_string())],
                ))
            } else {
                None
            }
        }
        CE::ObjectExactCardinality { n, ope, bce } => {
            card_tokens(ctx, ope, bce, "cardinality", *n)
        }
        CE::ObjectMinCardinality { n, ope, bce } => {
            card_tokens(ctx, ope, bce, "minCardinality", *n)
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            card_tokens(ctx, ope, bce, "maxCardinality", *n)
        }
        // A conjunction nested inside a conjunction has no OBO spelling. Exactly one
        // of its operands survives — the last in canonical order, which puts a named
        // class before a restriction and then orders by the rendered tokens — and it
        // is flagged `all_some="true"`; the rest are dropped. MBA:10688's
        // `(paraflocculus and part_of some Mus musculus) and part_of some
        // paraflocculus` comes out as the `part_of Mus musculus` line alone, without
        // the named operand. CL has 15 such terms; treating the nested conjunction
        // as untranslatable instead loses their whole definition, genus included.
        CE::ObjectIntersectionOf(inner) => {
            let mut lines: Vec<(Vec<String>, Vec<(String, String)>, (u8, String, String))> = inner
                .iter()
                .filter_map(|p| ce_to_inter_tokens(ctx, p).map(|(t, q)| (t, q, owlapi_ce_sort_key(p))))
                .collect();
            if lines.len() != inner.len() {
                return None;
            }
            // Order canonically — class-expression type, then the
            // *raw* IRIs of property and filler — and keep the last. This is not the
            // *rendered* token order: `has_part` (BFO:0000051's shorthand) sorts
            // after `RO:0000053`, but BFO_0000051 sorts before RO_0000053, so
            // CL:0008008's `has characteristic striated` (not `has_part sarcomere`)
            // is the surviving operand.
            lines.sort_by(|a, b| a.2.cmp(&b.2));
            let (toks, mut q, _) = lines.pop()?;
            q.push(("all_some".to_string(), "true".to_string()));
            Some((toks, q))
        }
        _ => None,
    }
}

/// Shared shape of the three cardinality restrictions.
fn card_tokens(
    ctx: &Ctx,
    ope: &OPE<RcStr>,
    bce: &CE<RcStr>,
    qual: &str,
    n: u32,
) -> Option<(Vec<String>, Vec<(String, String)>)> {
    let OPE::ObjectProperty(r) = ope else { return None };
    let CE::Class(t) = bce else { return None };
    Some((
        vec![ctx.id(r.0.as_ref()), ctx.id(t.0.as_ref())],
        vec![(qual.to_string(), n.to_string())],
    ))
}

/// Sort one `AnnotationAssertion` into the right OBO tag of its subject.
fn record_annotation(
    ctx: &Ctx,
    subj: &str,
    ann: &Annotation<RcStr>,
    axanns: &BTreeSet<Annotation<RcStr>>,
    data: &mut BTreeMap<String, SubjData>,
) {
    let prop = ann.ap.0.as_ref();
    let e = data.entry(subj.to_string()).or_default();
    e.ann_count += 1;
    let oio = prop.strip_prefix(OIO);
    let (val, is_iri, dt) = ann_value_ctx(ctx, &ann.av);
    // The value's language tag, kept for the synonym clauses: it is part of the
    // axiom hash that buckets tied clauses (see `SubjData::synonyms`).
    let val_lang: Option<String> = match &ann.av {
        AnnotationValue::Literal(Literal::Language { lang, .. }) => Some(lang.clone()),
        _ => None,
    };
    match prop {
        RDFS_LABEL => {
            // Track language-neutral labels for the `! <label>` comment resolution.
            let lang = match &ann.av {
                horned_owl::model::AnnotationValue::Literal(
                    horned_owl::model::Literal::Language { lang, .. },
                ) => Some(lang.clone()),
                _ => None,
            };
            if lang.is_none() {
                e.label_no_lang.insert(val.clone());
            }
            e.label_axioms.push((val.clone(), lang, axanns.clone()));
            if let Some(old) = e.name.replace((val, axanns.clone())) {
                e.extra_names.push(old);
            }
        }
        IAO_DEF => {
            if let Some(old) = e.def.replace((val, axanns.clone())) {
                e.extra_defs.push(old);
            }
        }
        RDFS_COMMENT => e.comments.push((val, axanns.clone())),
        OWL_DEPRECATED_W => {
            if val == "true" {
                e.deprecated = true;
                e.deprecated_anns = axanns.clone();
            }
        }
        IAO_REPLACED_BY_W => e.replaced_by.push(val),
        _ => match oio {
            Some("id") => e.id = Some(val),
            Some("hasExactSynonym") => e.synonyms.push(("EXACT".into(), val, val_lang.clone(), axanns.clone())),
            Some("hasNarrowSynonym") => e.synonyms.push(("NARROW".into(), val, val_lang.clone(), axanns.clone())),
            Some("hasBroadSynonym") => e.synonyms.push(("BROAD".into(), val, val_lang.clone(), axanns.clone())),
            Some("hasRelatedSynonym") => e.synonyms.push(("RELATED".into(), val, val_lang.clone(), axanns.clone())),
            Some("hasDbXref") => e.xrefs.push((val, axanns.clone())),
            Some("hasOBONamespace") => {
                // A term/typedef may carry several `hasOBONamespace` values — e.g.
                // BSPO's `ventral_to` keeps both "spatial" and the base-merge
                // default "uberon", or CHEBI's carbon monoxide "chebi_ontology" and
                // "protein". Every value that differs from the header default is
                // emitted (see the emit site), so keep them all.
                if !e.namespace.contains(&val) {
                    e.namespace.push(val);
                }
            }
            Some("inSubset") => {
                let (raw, _, _, riri) = av_lit_parts(&ann.av);
                e.subsets.push((val, raw, riri, axanns.clone()))
            }
            Some("hasAlternativeId") => e.alt_ids.push(val),
            Some("consider") => e.consider.push((val, axanns.clone())),
            Some("created_by") => e.created_by.push(val),
            Some("creation_date") => e.creation_date.push(val),
            Some("shorthand") => e.shorthand = Some(val),
            Some("is_metadata_tag") => e.is_metadata_tag = val == "true",
            Some("is_class_level") => e.is_class_level = val == "true",
            Some("is_transitive") => e.transitive_anno = Some(val == "true"),
            _ => match prop {
                // `IAO:0000231` on a deprecated class records the obsolescence
                // reason so `alt_id:` folding can recognise the "terms merged"
                // stubs — but the tag is NOT otherwise special-cased: it is still
                // emitted as a plain `property_value:` (627 of them in EFO, on
                // deprecated MONDO/EFO terms). A term whose reason makes it a
                // merged stub has its whole stanza dropped later, so emitting the
                // clause here is harmless there and correct everywhere else.
                IAO_OBSOLESCENCE_REASON => {
                    e.obsolescence_reason = Some(val.clone());
                    let (pv_val, val_iri) = match &ann.av {
                        AnnotationValue::IRI(i) => (ctx.curie(i.as_ref()), i.as_ref().to_string()),
                        _ => (val, String::new()),
                    };
                    e.property_values.push((ctx.id(prop), pv_val, is_iri, dt, axanns.clone(), prop.to_string(), val_iri));
                }
                // The OBO macro tags are Typedef-only; on anything else they stay
                // ordinary property_values.
                IAO_EXPAND_EXPRESSION_TO => e.expand_expression_to.push((val, axanns.clone())),
                IAO_EXPAND_ASSERTION_TO => e.expand_assertion_to.push((val, axanns.clone())),
                _ if is_iri && ctx.metadata_tags.contains(prop) => {
                    let (val, val_iri) = match &ann.av {
                        AnnotationValue::IRI(i) => (ctx.curie(i.as_ref()), i.as_ref().to_string()),
                        _ => (val, String::new()),
                    };
                    // An annotation assertion with an IRI value on a [Term] is a
                    // `relationship:` iff the property is a metadata tag (it
                    // carries `oboInOwl:is_metadata_tag`), else a
                    // `property_value:`. In a [Typedef] it is always a
                    // `property_value:`. The subject's stanza type is only known at
                    // write time, so defer the routing. Note: a shorthand alone does
                    // NOT qualify — `rdfs:seeAlso` has a `seeAlso` shorthand but no
                    // `is_metadata_tag`, so it stays a `property_value:`.
                    e.rel_or_pv.push((ctx.id(prop), val, axanns.clone(), prop.to_string(), val_iri));
                }
                _ => {
                    // Everything else is an OBO `property_value:`. An IRI value is a
                    // plain CURIE, never a shorthand (`seeAlso UBPROP:0000113`, not
                    // `seeAlso dental_formula`).
                    let (val, val_iri) = match &ann.av {
                        AnnotationValue::IRI(i) => (ctx.curie(i.as_ref()), i.as_ref().to_string()),
                        _ => (val, String::new()),
                    };
                    e.property_values.push((ctx.id(prop), val, is_iri, dt, axanns.clone(), prop.to_string(), val_iri));
                }
            },
        },
    }
}

/// The OBO id of an IRI-valued `hasDbXref` inside a
/// `{xref="…"}` QUALIFIER. (A `def:`/`synonym:` bracket keeps the IRI whole:
/// ECTO's `def: "…" [https://en.wikipedia.org/wiki/Drop_%28liquid%29]`.)
///
/// Take the part after the last `/`; if that carries a `#`, the fragment is the
/// id; else if it splits on exactly one `_`, the two halves become `PREFIX:LOCAL`;
/// otherwise there is no id and the FULL IRI stands. That is what makes ECTO's
/// `ecto.obo` read
/// `{xref="Properties"}` for `…/wiki/Clay#Properties`,
/// `{xref="Shortwave:radiation"}` for `…/wiki/Shortwave_radiation`, and
/// `{xref="https://orcid.org/0000-0003-4808-4736"}` — unshortened — for an ORCID,
/// whose last segment has neither.
fn xref_identifier(iri: &str) -> String {
    let last = iri.rsplit('/').next().unwrap_or(iri);
    if let Some((_, frag)) = last.split_once('#') {
        return frag.to_string();
    }
    let parts: Vec<&str> = last.split('_').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        return format!("{}:{}", parts[0], parts[1]);
    }
    iri.to_string()
}

/// The part of an `xref:` clause's axiom annotations that distinguishes it from
/// another clause on the same value.
///
/// A literal `rdfs:label` on the axiom is the xref's own description: it is
/// consumed into the xref rather than becoming one of the clause's qualifiers, and
/// two xrefs are equal when their ids are, descriptions notwithstanding. So the
/// description is invisible to the redundancy check while every other qualifier is
/// not.
fn xref_dedup_anns(anns: &BTreeSet<Annotation<RcStr>>) -> BTreeSet<Annotation<RcStr>> {
    anns.iter()
        .filter(|a| {
            !(a.ap.0.as_ref() == "http://www.w3.org/2000/01/rdf-schema#label"
                && matches!(a.av, AnnotationValue::Literal(_)))
        })
        .cloned()
        .collect()
}

/// Render an annotation value: (obo-text, is_iri, datatype-curie).
/// An IRI annotation VALUE outside the obo PURL space, as an OBO clause value.
///
/// Three cases, in order: a non-empty FRAGMENT wins
/// (`…/issues/225#issuecomment-218584934` → `issuecomment-218584934`); else a last
/// path segment shaped like an OBO id becomes one
/// (`http://www.ebi.ac.uk/efo/EFO_0008992` → `EFO:0008992`); else the IRI is
/// written verbatim (`https://w3id.org/semapv/vocab/ManualMappingCuration`,
/// `https://ror.org/03cpe7c52`).
///
/// Deliberately pure string work: asking the render context to shorten an IRI
/// REGISTERS a generated prefix, which then earns an `idspace:` line the reference
/// does not write.
fn obo_iri_value(iri: &str) -> String {
    if let Some((_, frag)) = iri.rsplit_once('#') {
        if !frag.is_empty() {
            return frag.to_string();
        }
    }
    let last = iri.rsplit('/').next().unwrap_or(iri);
    if let Some((pfx, local)) = last.split_once('_') {
        let prefix_ok = !pfx.is_empty()
            && pfx.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && pfx.chars().all(|c| c.is_ascii_alphanumeric());
        if prefix_ok && !local.is_empty() && !local.contains('_') {
            return format!("{pfx}:{local}");
        }
    }
    iri.to_string()
}

fn ann_value_ctx(ctx: &Ctx, av: &AnnotationValue<RcStr>) -> (String, bool, Option<String>) {
    match av {
        AnnotationValue::Literal(lit) => match lit {
            Literal::Simple { literal } => (literal.clone(), false, None),
            Literal::Language { literal, .. } => (literal.clone(), false, None),
            Literal::Datatype { literal, datatype_iri } => {
                let dt = datatype_iri.as_ref();
                // xsd:string is the OBO default; omit it. Other XSD datatypes
                // render as `xsd:NAME`, not a full IRI.
                let dt = if dt == "http://www.w3.org/2001/XMLSchema#string" {
                    None
                } else if let Some(local) = dt.strip_prefix("http://www.w3.org/2001/XMLSchema#") {
                    Some(format!("xsd:{local}"))
                } else {
                    Some(ctx.id(dt))
                };
                (literal.clone(), false, dt)
            }
        },
        AnnotationValue::IRI(i) => (ctx.id(i.as_ref()), true, None),
        _ => (String::new(), false, None),
    }
}

/// The canonical OBO tag for an annotation property used as a qualifier key.
/// `None` means the property has no canonical tag and falls back to its bare
/// `oboInOwl#` local name or a CURIE. Entries whose tag equals the `oboInOwl#`
/// local name (`consider`, `created_by`, `id`, `shorthand`, `treat-xrefs-*` …) are
/// omitted: the local-name fallback already yields them.
fn qualifier_key_tag(p: &str) -> Option<&'static str> {
    match p {
        "http://purl.obolibrary.org/obo/IAO_0000115" => Some("def"),
        "http://purl.obolibrary.org/obo/IAO_0000424" => Some("expand_expression_to"),
        "http://purl.obolibrary.org/obo/IAO_0000425" => Some("expand_assertion_to"),
        "http://purl.obolibrary.org/obo/IAO_0000427" => Some("is_anti_symmetric"),
        "http://purl.obolibrary.org/obo/IAO_0100001" => Some("replaced_by"),
        "http://www.w3.org/2000/01/rdf-schema#comment" => Some("comment"),
        "http://www.w3.org/2000/01/rdf-schema#label" => Some("name"),
        "http://www.w3.org/2002/07/owl#deprecated" => Some("is_obsolete"),
        _ => p.strip_prefix(OIO).and_then(|l| match l {
            "NamespaceIdRule" => Some("namespace-id-rule"),
            "SubsetProperty" => Some("subsetdef"),
            "SynonymTypeProperty" => Some("synonymtypedef"),
            "hasAlternativeId" => Some("alt_id"),
            "hasBroadSynonym" => Some("BROAD"),
            "hasDbXref" => Some("xref"),
            "hasExactSynonym" => Some("EXACT"),
            "hasNarrowSynonym" => Some("NARROW"),
            "hasOBOFormatVersion" => Some("format-version"),
            "hasOBONamespace" => Some("namespace"),
            "hasRelatedSynonym" => Some("RELATED"),
            "hasScope" => Some("scope"),
            "hasSynonymType" => Some("has_synonym_type"),
            "inSubset" => Some("subset"),
            _ => None,
        }),
    }
}

/// Decompose an axiom-annotation set into the OBO pieces a stanza line carries:
/// the `[dbxref…]` list, an optional synonym-type id, and the residual
/// `{key=value}` qualifiers.
fn ax_ann_pieces(
    ctx: &Ctx,
    anns: &BTreeSet<Annotation<RcStr>>,
) -> (Vec<(String, bool)>, Option<String>, Vec<Qual>) {
    // Each dbxref keeps its value-is-IRI flag: a `hasDbXref` whose value is an IRI
    // (`<https://doi.org/…>`) hashes as an IRI in the `{xref=…}` block order, a
    // literal (`"PMID:…"`) as a literal — CL:4033035's mixed pair depends on it.
    let mut dbxrefs: Vec<(String, bool)> = Vec::new();
    let mut syn_type = None;
    // (annotation property IRI, rendered qualifier name, value, value-is-IRI).
    // The IRI and is-IRI flag feed the hash used to reorder the block.
    let mut quals: Vec<Qual> = Vec::new();
    for a in anns {
        let p = a.ap.0.as_ref();
        // An IRI value that is NOT an OBO PURL is written verbatim, and asking the
        // context to shorten it would REGISTER a generated prefix — which then earns
        // an `idspace:` line the reference does not have (`vocab` for
        // `https://w3id.org/semapv/vocab/`). So the shortening is only attempted for
        // the obo PURL space, where the result is a real OBO id.
        let (val, is_iri, _dt) = match &a.av {
            AnnotationValue::IRI(i) if !i.as_ref().starts_with(OBO_BASE) => {
                // Outside the obo PURL space the value is a CURIE against whichever
                // namespace the document DECLARES — `vocab:ManualMappingCuration`
                // where the header binds `vocab:` to `https://w3id.org/semapv/vocab/`.
                // With no declared namespace covering it the IRI reduces to its
                // FRAGMENT when it has one (`…/issues/225#issuecomment-218584934` →
                // `issuecomment-218584934`) and is written verbatim when it has none.
                let v = ctx
                    .declared_curie(i.as_ref())
                    .unwrap_or_else(|| obo_iri_value(i.as_ref()));
                (v, true, None)
            }
            _ => ann_value_ctx(ctx, &a.av),
        };
        match p {
            _ if p == format!("{OIO}hasDbXref") => {
                // The raw IRI. A `def:`/`synonym:` bracket keeps it verbatim; only
                // the `{xref="…"}` QUALIFIER form is reduced to an OBO id — see
                // `xref_identifier`.
                let xv = match &a.av {
                    AnnotationValue::IRI(i) => i.as_ref().to_string(),
                    _ => val,
                };
                dbxrefs.push((xv, is_iri));
            }
            _ if p == format!("{OIO}hasSynonymType") => syn_type = Some(val),
            // `oboInOwl:all_only` is an internal marker for an all-some
            // (`∀R.F ⊓ ∃R.F`) translation, not an OBO qualifier — it is never
            // written out as `{all_only="true"}`, so skip it.
            _ if p == format!("{OIO}all_only") => {}
            _ => {
                // The remaining annotation properties become `{key="value"}`
                // qualifiers under the canonical OBO tag for them. `rdfs:label`
                // is `name` (CL's CELLxGENE `seeAlso` links all carry one) and
                // `oboInOwl:SynonymTypeProperty` — the *other* spelling of a
                // synonym type, used by CL's `hasRelatedSynonym` axioms — is
                // `synonymtypedef`, not its bare local name.
                // The qualifier key is a canonical
                // OBO tag when the property has one (`def`, `xref`, `scope`,
                // the synonym scopes …), else the bare `oboInOwl#` local name
                // (`source`, `notes`, `created_by` …), else a plain CURIE — never an
                // `oboInOwl:shorthand` (so `editor_note` → `IAO:0000116`,
                // `dc-contributor` → `terms:contributor`).
                let key = if let Some(tag) = qualifier_key_tag(p) {
                    tag.to_string()
                } else if p == RDFS_SEEALSO {
                    "seeAlso".to_string()
                } else if let Some(local) = p.strip_prefix(OIO) {
                    local.to_string()
                } else {
                    ctx.curie(p)
                };
                let (raw, dtf, langf, _) = av_lit_parts(&a.av);
                quals.push((p.to_string(), key, val, is_iri, dtf, langf, raw));
            }
        }
    }
    dbxrefs.sort_by(|a, b| fold(&a.0).cmp(&fold(&b.0)));
    // Default (`property_value`, `relationship`, `is_a`, `intersection_of`) order:
    // these qualifiers come straight from the axiom's *sorted* annotation stream,
    // so ascending by property IRI then value. The xref/comment/def/synonym/subset
    // tags override this with `owlapi_hashset_order`, because their qualifiers pass
    // through a hash set on the way out.
    quals.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    (dbxrefs, syn_type, quals)
}

/// The dbxrefs of an annotation set rendered as `{xref="…"}` qualifiers. Only
/// `def:` and `synonym:` have the bracket-list syntax; every other tag
/// (comment, relationship, property_value, …) carries its provenance as an
/// `xref` qualifier, as in CL's `comment: … {xref="PMID:26106328"}`.
/// One `{name="value"}` qualifier: (annotation property IRI, rendered qualifier
/// name, value, value-is-IRI). The IRI and is-IRI flag exist only to reconstruct
/// the annotation hash; [`plain`] drops them just before rendering.
/// One `{key="value"}` qualifier. The trailing fields exist only to rebuild the
/// annotation hash that decides the block's order; [`plain`] drops
/// them just before rendering. The RAW value is kept alongside the rendered one
/// because the hash is over the full IRI, not the CURIE the qualifier prints
/// (`https://w3id.org/semapv/vocab/LexicalMatching`, not `vocab:LexicalMatching`).
type Qual = (String, String, String, bool, Option<String>, Option<String>, String);

/// Drop the hash-only fields, leaving what `render_quals` prints.
fn plain(quals: &[Qual]) -> Vec<(String, String)> {
    quals.iter().map(|(_, k, v, _, _, _, _)| (k.clone(), v.clone())).collect()
}

// --- Qualifier block ordering --------------------------------------------------
//
// A `{…}` qualifier block comes out of a hash set with no sort applied, so its
// order is hash-bucket order — a pure function of each annotation's hash. The OBO
// spec does not define that order, but released files such as CL's `cl.obo` carry
// it, and a `.obo` whose blocks come out in any other order rewrites lines whose
// content never changed. Reproducing it means reproducing the hash and the bucket
// walk:
//
//   IRI hash               = jhash(namespace) + jhash(remainder)     (NCName split)
//   annotation property    = IRI hash + 188077
//   literal(xsd:string)    = 3231644899 + jhash(value) * 65536
//   annotation             = 31*property hash + value hash + 6064871
//
// where jhash is the 31-multiplier string hash over UTF-16 units. Every seed here
// is part of the on-disk order, so changing one changes every release diff.

/// The 31-multiplier string hash: `s[0]*31^(n-1) + … + s[n-1]`, over UTF-16 code
/// units, wrapping in `i32`.
pub(crate) fn java_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

fn is_xml_name_start(c: u32) -> bool {
    c == b':' as u32
        || (b'A' as u32..=b'Z' as u32).contains(&c)
        || c == b'_' as u32
        || (b'a' as u32..=b'z' as u32).contains(&c)
        || (0xC0..=0xD6).contains(&c)
        || (0xD8..=0xF6).contains(&c)
        || (0xF8..=0x2FF).contains(&c)
        || (0x370..=0x37D).contains(&c)
        || (0x37F..=0x1FFF).contains(&c)
        || (0x200C..=0x200D).contains(&c)
        || (0x2070..=0x218F).contains(&c)
        || (0x2C00..=0x2FEF).contains(&c)
        || (0x3001..=0xD7FF).contains(&c)
        || (0xF900..=0xFDCF).contains(&c)
        || (0xFDF0..=0xFFFD).contains(&c)
        || (0x10000..=0xEFFFF).contains(&c)
}

fn is_xml_name_char(c: u32) -> bool {
    is_xml_name_start(c)
        || c == b'-' as u32
        || c == b'.' as u32
        || (b'0' as u32..=b'9' as u32).contains(&c)
        || c == 0xB7
        || (0x0300..=0x036F).contains(&c)
        || (0x203F..=0x2040).contains(&c)
}

/// The byte index where the local part
/// (NCName suffix) begins, or `None` when the string has no such suffix (the
/// whole string is then the namespace). `:` is not an NCName char.
pub(crate) fn ncname_suffix_index(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() > 1 && b[0] == b'_' && b[1] == b':' {
        return None;
    }
    let mut index = None;
    for (i, ch) in s.char_indices().rev() {
        let cp = ch as u32;
        if cp != ':' as u32 && is_xml_name_start(cp) {
            index = Some(i);
        }
        if !(cp != ':' as u32 && is_xml_name_char(cp)) {
            break;
        }
    }
    index
}

/// The IRI hash: `hash(namespace) + hash(remainder)`, split at the NCName suffix.
pub(crate) fn owlapi_iri_hash(iri: &str) -> i32 {
    match ncname_suffix_index(iri) {
        Some(i) => java_hash(&iri[..i]).wrapping_add(java_hash(&iri[i..])),
        None => java_hash(iri),
    }
}

/// The literal hash for a plain `xsd:string` literal (no language tag) — the only
/// literal kind CL puts in a qualifier.
fn owlapi_literal_hash(value: &str) -> i32 {
    (3231644899u32 as i32).wrapping_add(java_hash(value).wrapping_mul(65536))
}

const XSD_STRING_IRI: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_PLAIN_LITERAL_IRI: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// The literal hash for any literal. `xsd:string` and `rdf:langString` normalize to
/// `rdf:PlainLiteral` before hashing, so a plain or language literal keeps the
/// `owlapi_literal_hash` base; a typed literal shifts the base by its datatype's
/// IRI-hash difference from `rdf:PlainLiteral`; a language tag then folds in as
/// `base*37 + hash(lang)`.
fn owlapi_lit_hash(value: &str, datatype: Option<&str>, lang: Option<&str>) -> i32 {
    // The datatype the literal hashes UNDER, which also decides how the value
    // itself contributes: a typed number contributes the number, so
    // `"20"^^xsd:integer` and `"20"^^xsd:string` do not hash alike.
    let dt = match datatype {
        Some(d) if d != XSD_STRING_IRI => d,
        _ => RDF_PLAIN_LITERAL_IRI,
    };
    let jv = crate::owlapi_hash::literal_payload_hash(value, dt);
    let d = owlapi_iri_hash(dt)
        .wrapping_sub(owlapi_iri_hash(RDF_PLAIN_LITERAL_IRI))
        .wrapping_mul(37);
    let base = (3231644899u32 as i32).wrapping_add(d).wrapping_add(jv);
    match lang {
        Some(l) => base.wrapping_mul(37).wrapping_add(java_hash(l)),
        None => base,
    }
}

/// The `(value, datatype, language)` the literal hash needs from an annotation
/// value; an IRI value returns its IRI in `value` with `is_iri` true.
fn av_lit_parts(av: &AnnotationValue<RcStr>) -> (String, Option<String>, Option<String>, bool) {
    match av {
        AnnotationValue::IRI(i) => (i.as_ref().to_string(), None, None, true),
        AnnotationValue::Literal(Literal::Simple { literal }) => (literal.clone(), None, None, false),
        AnnotationValue::Literal(Literal::Language { literal, lang }) => {
            (literal.clone(), None, Some(lang.clone()), false)
        }
        AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri }) => {
            (literal.clone(), Some(datatype_iri.as_ref().to_string()), None, false)
        }
        _ => (String::new(), None, None, false),
    }
}

/// The annotation hash for an un-nested annotation (plain value).
fn owlapi_annotation_hash(prop_iri: &str, value: &str, is_iri: bool) -> i32 {
    owlapi_annotation_hash_full(prop_iri, value, None, None, is_iri)
}

/// [`owlapi_annotation_hash`] with the value's datatype/language, so typed and
/// language-tagged qualifier values hash exactly.
fn owlapi_annotation_hash_full(
    prop_iri: &str,
    value: &str,
    datatype: Option<&str>,
    lang: Option<&str>,
    is_iri: bool,
) -> i32 {
    let prop = owlapi_iri_hash(prop_iri).wrapping_add(188077);
    let val = if is_iri {
        owlapi_iri_hash(value)
    } else {
        owlapi_lit_hash(value, datatype, lang)
    };
    31i32.wrapping_mul(prop).wrapping_add(val).wrapping_add(6064871)
}

/// The hash of an axiom's annotation collection, as it feeds the axiom hash (see
/// [`owlapi_aa_axiom_hash`]). Empty → 0; otherwise a list hash
/// (`acc = 1; acc = 31*acc + element`) over the annotations *sorted* canonically
/// (property IRI, then value) — the same order [`owlapi_hashset_order`]
/// uses. Each element is an [`owlapi_annotation_hash`].
fn owlapi_aa_collection_hash(_ctx: &Ctx, anns: &BTreeSet<Annotation<RcStr>>) -> i32 {
    if anns.is_empty() {
        return 0;
    }
    // The hash is over the *full* IRI / raw literal — not the shortened
    // CURIE `ann_value_ctx` renders — so read the value straight off the annotation.
    let mut elems: Vec<(String, String, Option<String>, Option<String>, bool)> = anns
        .iter()
        .map(|a| {
            let (val, dt, lang, is_iri) = av_lit_parts(&a.av);
            (a.ap.0.as_ref().to_string(), val, dt, lang, is_iri)
        })
        .collect();
    // Annotations compare on the property, then the VALUE — and a value compares on
    // its TYPE index BEFORE its content. An IRI's index is below a literal's, so an
    // IRI-valued qualifier always precedes a literal-valued one on the same
    // property, whatever the two strings are.
    //
    // Ranking on the string alone inverts every synonym xref block in HPO, which
    // are uniformly one plain literal (wikipedia/mayoclinic/radiopaedia) plus one
    // ORCID IRI whose string sorts ABOVE it. That shifts the annotation-collection
    // hash, hence the axiom hash, hence the bucket the frame is built from —
    // putting the annotated synonym on the wrong side of its bare twin in all 18 of
    // the stanzas whose order is determinate.
    elems.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| (!a.4).cmp(&(!b.4)))
            .then_with(|| a.1.cmp(&b.1))
    });
    let mut acc: i32 = 1;
    for (p, v, dt, lang, is_iri) in &elems {
        acc = acc.wrapping_mul(31).wrapping_add(owlapi_annotation_hash_full(
            p,
            v,
            dt.as_deref(),
            lang.as_deref(),
            *is_iri,
        ));
    }
    acc
}

/// The hash of an annotation-assertion axiom: seed 739, then
/// `hash = 31*hash + component` over subject IRI, annotation property, value, and
/// the annotation-collection hash. A subject's assertion axioms sit in a hash
/// table, and their OBO clauses are written in this hash's bucket order (see
/// [`owlapi_aa_bucket`]).
fn owlapi_aa_axiom_hash(
    subj_iri: &str,
    prop_iri: &str,
    value: &str,
    val_is_iri: bool,
    coll_hash: i32,
) -> i32 {
    owlapi_aa_axiom_hash_full(subj_iri, prop_iri, value, None, None, val_is_iri, coll_hash)
}

/// [`owlapi_aa_axiom_hash`] with the value's datatype/language, so a typed
/// (xsd:decimal) or language-tagged main value hashes exactly.
#[allow(clippy::too_many_arguments)]
fn owlapi_aa_axiom_hash_full(
    subj_iri: &str,
    prop_iri: &str,
    value: &str,
    datatype: Option<&str>,
    lang: Option<&str>,
    val_is_iri: bool,
    coll_hash: i32,
) -> i32 {
    let mut h: i32 = 739;
    h = h.wrapping_mul(31).wrapping_add(owlapi_iri_hash(subj_iri));
    h = h
        .wrapping_mul(31)
        .wrapping_add(owlapi_iri_hash(prop_iri).wrapping_add(188077));
    let vh = if val_is_iri {
        owlapi_iri_hash(value)
    } else {
        owlapi_lit_hash(value, datatype, lang)
    };
    h = h.wrapping_mul(31).wrapping_add(vh);
    h = h.wrapping_mul(31).wrapping_add(coll_hash);
    h
}

/// The hash bucket of one `rdfs:label` annotation-assertion axiom, for a
/// table of `cap` — the axiom hash (subject IRI, `rdfs:label`, the literal
/// value hash for its plain/language kind, then its annotation-collection hash)
/// spread into `cap` buckets.
fn owlapi_label_bucket(subj: &str, value: &str, lang: Option<&str>, coll: i32, cap: usize) -> usize {
    owlapi_aa_bucket(owlapi_label_axiom_hash(subj, value, lang, coll), cap)
}

/// The raw hash of an `rdfs:label` annotation-assertion axiom, before
/// it is spread into a bucket. Split out of [`owlapi_label_bucket`] so a tie inside
/// one bucket can be broken on the finer value.
fn owlapi_label_axiom_hash(subj: &str, value: &str, lang: Option<&str>, coll: i32) -> i32 {
    let mut h: i32 = 739;
    h = h.wrapping_mul(31).wrapping_add(owlapi_iri_hash(subj));
    h = h
        .wrapping_mul(31)
        .wrapping_add(owlapi_iri_hash(RDFS_LABEL).wrapping_add(188077));
    let vh = match lang {
        None => owlapi_literal_hash(value),
        Some(l) => owlapi_literal_hash(value).wrapping_mul(37).wrapping_add(java_hash(l)),
    };
    h = h.wrapping_mul(31).wrapping_add(vh);
    h = h.wrapping_mul(31).wrapping_add(coll);
    h
}

/// The display name for a subject's `! label` comments: the `rdfs:label` whose
/// axiom lands in the minimum bucket of the hash table holding all of the
/// subject's annotation-assertion axioms.
///
/// The bucket pick is applied only when it is unambiguous — every label axiom has
/// no annotations (so the collection hash, which is exact only for plain literals,
/// is 0) AND the labels fall in distinct buckets (a within-bucket tie is decided by
/// insertion order, which an unordered model cannot recover). That settles the
/// clean multi-label cases (OBI:0000295, PR:000003918, part_of). Otherwise — the
/// multilingual terms with `{source}`-annotated labels (GSSO) whose buckets collide
/// — the tie falls to the un-annotated label, and failing that to the
/// fold-maximum.
fn pick_comment_name(ctx: &Ctx, subj_iri: &str, sd: &SubjData) -> Option<String> {
    if sd.label_axioms.is_empty() {
        return None;
    }
    {
        let cap = owlapi_set_cap(sd.ann_count.max(1));
        let buckets: Vec<usize> = sd
            .label_axioms
            .iter()
            .map(|(v, lang, anns)| {
                owlapi_label_bucket(subj_iri, v, lang.as_deref(), owlapi_aa_collection_hash(ctx, anns), cap)
            })
            .collect();
        if std::env::var("OM_LABEL_DEBUG").is_ok() {
            let rows: Vec<String> = sd
                .label_axioms
                .iter()
                .zip(buckets.iter())
                .map(|((v, lang, anns), b)| {
                    let h = owlapi_label_axiom_hash(
                        subj_iri,
                        v,
                        lang.as_deref(),
                        owlapi_aa_collection_hash(ctx, anns),
                    );
                    format!(
                        "{b}\u{1}{h}\u{1}{}\u{1}{}\u{1}{}",
                        lang.clone().unwrap_or_default(),
                        anns.len(),
                        v
                    )
                })
                .collect();
            eprintln!(
                "[label]\t{subj_iri}\t{}\t{cap}\t{}",
                sd.ann_count,
                rows.join("\u{2}")
            );
        }
        let mut sorted = buckets.clone();
        sorted.sort_unstable();
        sorted.dedup();
        // Unambiguous only: distinct buckets (a same-bucket tie would need the
        // unrecoverable parse-insertion order).
        if sorted.len() == buckets.len() {
            let (i, _) = buckets.iter().enumerate().min_by_key(|(_, b)| **b).unwrap();
            return Some(sd.label_axioms[i].0.clone());
        }
        // Two labels in the SAME bucket. Colliding entries chain in insertion order,
        // and in RDF/XML the plain assertion is parsed inside the class element while an
        // ANNOTATED one is only created once its `owl:Axiom` block is read — so the
        // un-annotated label heads the chain and wins.
        //
        // That is the mechanically justified choice, and it is NOT reliable: over
        // 300 real translated MONDO classes (782 `! …` comments) every non-tied case
        // is correct and only the 13 ties err — 6 of them under this rule, and the
        // OPPOSITE rule ("prefer annotated") gets those 6 right and the other 6
        // wrong, with zero overlap. So neither is the rule; a tie is decided by the
        // true insertion order of ALL the subject's assertions into the chain, which
        // an unordered model cannot reconstruct. Closing the last ~1,286 lines of
        // `mondo-international.obo` means recording per-subject label order at read
        // time, the way `owl_reif_order` already records reification order.
        // `OM_LABEL_DEBUG=1` prints ann_count / cap / buckets per subject.
        let lo = *sorted.first().unwrap();
        let tied: Vec<usize> = (0..buckets.len()).filter(|&i| buckets[i] == lo).collect();
        if tied.len() > 1 {
            // Colliding assertions chain in the order they were read, and the head
            // of the chain is the name. The source document's order is the record
            // of that; `RO_0002314`'s two labels collide in bucket 14 of a 16-slot
            // table and the comment takes "characteristic of part of", written
            // first, over "inheres in part of".
            if let Some(order) = ctx.label_order.get(subj_iri) {
                let first = tied
                    .iter()
                    .copied()
                    .min_by_key(|&i| {
                        order
                            .iter()
                            .position(|v| *v == sd.label_axioms[i].0)
                            .unwrap_or(usize::MAX)
                    })
                    .unwrap();
                if order.iter().any(|v| *v == sd.label_axioms[first].0) {
                    return Some(sd.label_axioms[first].0.clone());
                }
            }
            let bare: Vec<usize> = tied
                .iter()
                .copied()
                .filter(|&i| sd.label_axioms[i].2.is_empty())
                .collect();
            let cands = if bare.is_empty() { tied } else { bare };
            if cands.len() == 1 {
                return Some(sd.label_axioms[cands[0]].0.clone());
            }
            // A model built in memory carries no document to have read an order
            // from. See the fold-maximum note below.
            return cands
                .iter()
                .map(|&i| &sd.label_axioms[i].0)
                .max_by(|a, b| fold(a).cmp(&fold(b)))
                .cloned();
        }
    }
    // Within-bucket tie: the LAST one wins. `RO_0004024` carries "disease causes
    // disruption of" and "disease disrupts", both landing in bucket 3 of a 16-slot
    // table, and the comment takes whichever is written SECOND — swapping the two
    // `AnnotationAssertion` lines in the input flips the answer. Colliding keys
    // chain in insertion order and the display name is overwritten as the chain is
    // walked, so the last one through wins.
    //
    // owlmake's model is an unordered set, so document order is not recoverable
    // here; the fold-MAXIMUM stands in for it, which is what both observed cases
    // resolve to — "disease disrupts" over "disease causes disruption of" for
    // `mondo.obo`, and the Japanese label over the English one for the ~1,288
    // `! …` comments in `mondo-international.obo` (CJK sorts above ASCII).
    // Preferring a language-NEUTRAL label instead is exactly backwards for the
    // translated MONDO products.
    sd.label_axioms
        .iter()
        .max_by(|a, b| fold(&a.0).cmp(&fold(&b.0)))
        .map(|(v, _, _)| v.clone())
}

/// The bucket an axiom hash falls in, for a table of `cap`
/// (a power of two): `spread(h) & (cap - 1)` with the spreader
/// `h ^ (h >>> 16)`. This is the position in the per-subject assertion-set
/// walk, so clauses left tied by the value comparison (same value,
/// differing only in qualifiers) come out in ascending bucket order.
fn owlapi_aa_bucket(hash: i32, cap: usize) -> usize {
    let h = hash as u32;
    let spread = h ^ (h >> 16);
    (spread as usize) & (cap - 1)
}

/// Table size of the hash set after `n` incremental adds: start
/// at 16, double whenever the size exceeds 0.75·capacity.
pub(crate) fn owlapi_set_cap(n: usize) -> usize {
    let mut cap = 16usize;
    while n * 4 > cap * 3 {
        cap <<= 1;
    }
    cap
}

/// The hash of a class expression: each expression type is seeded with its own
/// prime and folds `hash = 31*hash + component`.
/// A named class is `2293*31 + IRI`; a some/all restriction adds property
/// then filler; a conjunction adds its operands' list hash, taken over the operands
/// in canonical order.
/// Only the expression shapes OBO produces (is_a/relationship/GCI heads) are
/// covered; anything else returns 0, which is harmless — it only feeds a
/// tie-break between clauses of otherwise identical value.
pub(crate) fn owlapi_ce_hash(ce: &CE<RcStr>) -> i32 {
    let ope_iri = |ope: &OPE<RcStr>| match ope {
        OPE::ObjectProperty(r) => r.0.as_ref().to_string(),
        OPE::InverseObjectProperty(r) => r.0.as_ref().to_string(),
    };
    match ce {
        CE::Class(c) => (2293i32.wrapping_mul(31)).wrapping_add(owlapi_iri_hash(c.0.as_ref())),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            let mut h: i32 = 3517;
            h = h.wrapping_mul(31).wrapping_add(
                4153i32.wrapping_mul(31).wrapping_add(owlapi_iri_hash(&ope_iri(ope))),
            );
            h.wrapping_mul(31).wrapping_add(owlapi_ce_hash(bce))
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            let mut h: i32 = 2833;
            h = h.wrapping_mul(31).wrapping_add(
                4153i32.wrapping_mul(31).wrapping_add(owlapi_iri_hash(&ope_iri(ope))),
            );
            h.wrapping_mul(31).wrapping_add(owlapi_ce_hash(bce))
        }
        CE::ObjectIntersectionOf(parts) => {
            let mut ops: Vec<&CE<RcStr>> = parts.iter().collect();
            ops.sort_by(|a, b| owlapi_ce_sort_key(a).cmp(&owlapi_ce_sort_key(b)));
            let mut acc: i32 = 1;
            for o in ops {
                acc = acc.wrapping_mul(31).wrapping_add(owlapi_ce_hash(o));
            }
            3083i32.wrapping_mul(31).wrapping_add(acc)
        }
        // Cardinality restrictions fold property, then the bound, then filler
        // (seeds: exact 3001, max 3187, min 3259).
        CE::ObjectExactCardinality { n, ope, bce } => owlapi_card_hash(3001, *n, ope, bce),
        CE::ObjectMaxCardinality { n, ope, bce } => owlapi_card_hash(3187, *n, ope, bce),
        CE::ObjectMinCardinality { n, ope, bce } => owlapi_card_hash(3259, *n, ope, bce),
        // A union folds like an intersection, under its own seed. Returning 0 for it
        // instead puts every axiom whose definition mentions a union into the same
        // hash bucket, which duplicates EFO's `intersection_of:` genus lines onto
        // the wrong five classes.
        CE::ObjectUnionOf(parts) => {
            let mut ops: Vec<&CE<RcStr>> = parts.iter().collect();
            ops.sort_by(|a, b| owlapi_ce_sort_key(a).cmp(&owlapi_ce_sort_key(b)));
            let mut acc: i32 = 1;
            for o in ops {
                acc = acc.wrapping_mul(31).wrapping_add(owlapi_ce_hash(o));
            }
            3581i32.wrapping_mul(31).wrapping_add(acc)
        }
        CE::ObjectComplementOf(b) => {
            2909i32.wrapping_mul(31).wrapping_add(owlapi_ce_hash(b))
        }
        CE::ObjectHasSelf(ope) => 3433i32
            .wrapping_mul(31)
            .wrapping_add(4153i32.wrapping_mul(31).wrapping_add(owlapi_iri_hash(&ope_iri(ope)))),
        _ => 0,
    }
}

/// The hash of an object cardinality restriction: `seed`, then
/// `31*hash + {property, cardinality, filler}` in that order.
fn owlapi_card_hash(seed: i32, n: u32, ope: &OPE<RcStr>, bce: &CE<RcStr>) -> i32 {
    let prop_iri = match ope {
        OPE::ObjectProperty(r) => r.0.as_ref().to_string(),
        OPE::InverseObjectProperty(r) => r.0.as_ref().to_string(),
    };
    let mut h: i32 = seed;
    h = h
        .wrapping_mul(31)
        .wrapping_add(4153i32.wrapping_mul(31).wrapping_add(owlapi_iri_hash(&prop_iri)));
    h = h.wrapping_mul(31).wrapping_add(n as i32);
    h.wrapping_mul(31).wrapping_add(owlapi_ce_hash(bce))
}

/// The hash of a SubClassOf axiom: seed 2063, then
/// `31*hash + component` over subclass, superclass, and the annotation-collection
/// hash. Each axiom type is processed as one table, so a subject's
/// `is_a:`/`relationship:` clauses left tied by the value comparison come out in
/// this hash's bucket order within that global set (see [`owlapi_aa_bucket`]).
/// The hash of an EquivalentClasses axiom: seed 811, then the class expressions,
/// then the axiom annotations. The members hash as a LIST — `acc = 31*acc + member`
/// over them in canonical order — not as a set sum.
pub(crate) fn owlapi_equivalent_classes_hash(
    members: &[CE<RcStr>],
    anns: &BTreeSet<Annotation<RcStr>>,
) -> i32 {
    let mut sorted: Vec<&CE<RcStr>> = members.iter().collect();
    sorted.sort_by(|a, b| owlapi_ce_sort_key(a).cmp(&owlapi_ce_sort_key(b)));
    let mut acc: i32 = 1;
    for m in sorted {
        acc = acc.wrapping_mul(31).wrapping_add(owlapi_ce_hash(m));
    }
    let mut h: i32 = 811;
    h = h.wrapping_mul(31).wrapping_add(acc);
    h.wrapping_mul(31).wrapping_add(owlapi_aa_collection_hash(&Ctx::default(), anns))
}

fn owlapi_subclassof_hash(
    ctx: &Ctx,
    sub: &CE<RcStr>,
    sup: &CE<RcStr>,
    anns: &BTreeSet<Annotation<RcStr>>,
) -> i32 {
    let mut h: i32 = 2063;
    h = h.wrapping_mul(31).wrapping_add(owlapi_ce_hash(sub));
    h = h.wrapping_mul(31).wrapping_add(owlapi_ce_hash(sup));
    h.wrapping_mul(31)
        .wrapping_add(owlapi_aa_collection_hash(ctx, anns))
}

/// Reorder a qualifier list into hash-set iteration order for the annotations'
/// hashes. A synthetic qualifier (a writer-invented
/// `gci_*`/`all_some`/`cardinality`, marked by a `\u{FFFF}` sentinel in its IRI
/// slot) is not a real annotation; those are appended after, in name
/// order, since a clause never carries more than one and their order is moot.
fn owlapi_hashset_order(quals: Vec<Qual>) -> Vec<Qual> {
    let (mut synthetic, real): (Vec<Qual>, Vec<Qual>) =
        quals.into_iter().partition(|q| q.0.starts_with('\u{FFFF}'));
    synthetic.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));

    // Insertion order into the set is the list's own order, and that list is the
    // axiom's annotations sorted canonically — for a qualifier, property IRI then
    // value.
    let mut items: Vec<Qual> = real;
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));

    // A set built from a collection pre-sizes to hold it at load factor
    // 0.75, minimum table 16, rounded up to a power of two.
    let need = ((items.len() as f64 / 0.75) as usize + 1).max(16);
    let mut cap = 16usize;
    while cap < need {
        cap <<= 1;
    }
    let mut buckets: Vec<Vec<Qual>> = vec![Vec::new(); cap];
    for q in items {
        let h = owlapi_annotation_hash_full(&q.0, &q.6, q.4.as_deref(), q.5.as_deref(), q.3) as u32;
        let spread = h ^ (h >> 16);
        buckets[(spread as usize) & (cap - 1)].push(q);
    }
    let mut out: Vec<Qual> = buckets.into_iter().flatten().collect();
    out.append(&mut synthetic);
    out
}

/// Merge the leftover `hasDbXref` annotations in as `xref="…"` qualifiers. They
/// take their place by that property's IRI like any other, rather than being
/// appended and the whole block re-sorted by rendered name.
fn quals_with_xrefs(dbxrefs: &[(String, bool)], quals: &[Qual]) -> Vec<(String, String)> {
    let mut out: Vec<Qual> = quals.to_vec();
    for (x, is_iri) in dbxrefs {
        // A `{xref="…"}` qualifier carries the OBO IDENTIFIER of an IRI value, not
        // the IRI — unlike the `def:`/`synonym:` bracket, which keeps it whole.
        let x = if *is_iri { xref_identifier(x) } else { x.clone() };
        out.push((format!("{OIO}hasDbXref"), "xref".to_string(), x.clone(), *is_iri, None, None, x));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    plain(&out)
}

/// Like [`quals_with_xrefs`], but ordering the `{…}` block by hash bucket rather
/// than by name. Used by the tags whose qualifiers pass through a hash set on the
/// way out: `xref`, `comment`, `def`, `synonym`, `subset`.
fn quals_with_xrefs_hashset(dbxrefs: &[(String, bool)], quals: &[Qual]) -> Vec<(String, String)> {
    let mut out: Vec<Qual> = quals.to_vec();
    for (x, is_iri) in dbxrefs {
        // A `{xref="…"}` qualifier carries the OBO IDENTIFIER of an IRI value, not
        // the IRI — unlike the `def:`/`synonym:` bracket, which keeps it whole.
        let x = if *is_iri { xref_identifier(x) } else { x.clone() };
        out.push((format!("{OIO}hasDbXref"), "xref".to_string(), x.clone(), *is_iri, None, None, x));
    }
    plain(&owlapi_hashset_order(out))
}

/// Order a `relationship:`/`is_a:`/`intersection_of:` clause's `{…}` qualifiers
/// by hash bucket, keyed on the qualifier itself:
/// `hash = 31*(31 + hash(key)) + hash(value)`.
/// This is distinct from the annotation ordering used for def/synonym/xref
/// blocks: on a relationship the axiom annotations (e.g. `source`) and the
/// synthesised `gci_relation`/`gci_filler`/`cardinality` qualifiers share one
/// set keyed by that qualifier hash, which is what puts UBERON's
/// `{gci_relation=…, gci_filler=…, source=…}` in the order it has.
fn qualifier_value_hashset_order(pairs: &[(String, String)]) -> Vec<(String, String)> {
    // Within a bucket, *insertion* order is kept (new nodes append to the tail), and
    // `gci_relation` is inserted before `gci_filler`, so a
    // same-bucket collision resolves relation-first — UBERON's
    // `{gci_relation="part_of", gci_filler="NCBITaxon:9443"}` (both bucket 6). Keep
    // `pairs` in caller order; do NOT sort.
    let items: Vec<(String, String)> = pairs.to_vec();
    let need = ((items.len() as f64 / 0.75) as usize + 1).max(16);
    let mut cap = 16usize;
    while cap < need {
        cap <<= 1;
    }
    let mut buckets: Vec<Vec<(String, String)>> = vec![Vec::new(); cap];
    for (k, v) in items {
        let hc = 31i32
            .wrapping_mul(31i32.wrapping_add(java_hash(&k)))
            .wrapping_add(java_hash(&v)) as u32;
        let spread = hc ^ (hc >> 16);
        buckets[(spread as usize) & (cap - 1)].push((k, v));
    }
    buckets.into_iter().flatten().collect()
}

/// Assemble a relationship-style clause's qualifiers — axiom-annotation quals
/// (`quals`), leftover `hasDbXref` (`xref="…"`), and synthesised clause quals
/// (`extra`: `gci_relation`/`gci_filler`/`cardinality`/`all_only`) — and order
/// them with [`qualifier_value_hashset_order`].
fn quals_relationship(dbxrefs: &[(String, bool)], quals: &[Qual], extra: &[(String, String)]) -> Vec<(String, String)> {
    if extra.is_empty() {
        // No synthesised qualifier: the plain relationship/is_a path sorts
        // the axiom-annotation qualifiers (by property IRI, then value).
        return quals_with_xrefs(dbxrefs, quals);
    }
    // A synthesised qualifier (gci_relation/gci_filler, cardinality, all_only)
    // routes the clause through the General-Class-Inclusion path: the
    // synthetic qualifiers come first, ordered among themselves by the qualifier
    // hash, and the axiom-annotation qualifiers (source, xref,
    // …) follow in the plain sorted order.
    let mut out = qualifier_value_hashset_order(extra);
    out.extend(quals_with_xrefs(dbxrefs, quals));
    out
}

fn render_quals(quals: &[(String, String)]) -> String {
    if quals.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = quals.iter().map(|(k, v)| format!("{k}=\"{}\"", escape(v))).collect();
    format!(" {{{}}}", inner.join(", "))
}

fn render_bracket(dbxrefs: &[(String, bool)]) -> String {
    // Within a `[id, id, …]` list `,` separates ids, `]` ends the list, `"` begins
    // a per-id description and `:` splits idspace from local id, so a literal one
    // inside an id must be escaped — e.g. a URL
    // `…/cerebral_aneurysm_85\,P08772/` and the DOI `doi:10.1023/a\:1018564904170`.
    // Otherwise the round-trip re-parse would split the id on the comma.
    let escaped: Vec<String> = dbxrefs.iter().map(|(x, _)| escape_xref(x)).collect();
    format!("[{}]", escaped.join(", "))
}

/// A reference is annotated with the referent's label as a trailing
/// `! comment` — `is_a: CL:0000393 ! electrically responsive cell`. Every token of
/// the clause that has a label contributes (so a `relationship:` carries both the
/// relation's and the filler's, space-joined); tokens whose referent is unlabelled
/// or external contribute nothing, and if none is labelled there is no comment at
/// all. Only the *reference* tags get one: `xref:`, `subset:`, `alt_id:`,
/// `replaced_by:`, `consider:` and `holds_over_chain:` never do, as CL's `cl.obo`
/// shows.
/// [`label_comment`] for a clause written as `RELATION FILLER`. A relation given
/// by its OBO shorthand is already spelled as its own name, so it is not
/// repeated — `relationship: filtered_through UBERON:0000042 ! serous membrane`,
/// not `… ! filtered through serous membrane`. A relation given as a CURIE
/// (`BFO:0000050`) is labelled like anything else, and a shorthand in *value*
/// position still is (a [Typedef]'s `is_a: transitively_anteriorly_connected_to
/// ! transitively anteriorly connected to`), so this only applies to the head.
fn label_comment_pred(
    labels: &HashMap<String, String>,
    declared: &std::collections::HashSet<&str>,
    toks: &[&str],
) -> String {
    // The relation head contributes its own label to the `! comment` only when the
    // head's name resolves. It never does for a bare
    // shorthand (`part_of`, already spelled as its name), and for a CURIE it does
    // ONLY when the id is a mechanical OBO PURL (`RO:0000087` ⇐ obo/RO_0000087) —
    // NOT one shortened with a *declared* idspace prefix (`obo1:has_role` ⇐
    // obo#has_role, `efo:EFO_0000784` ⇐ .../efo/EFO_0000784), whose label is
    // omitted. `id_impl` only emits a declared-idspace prefix for a non-OBO-PURL
    // namespace, so "prefix is a declared idspace" is exactly "not an OBO PURL id".
    match toks.split_first() {
        // A single token is a value, not a relation head (an `intersection_of:`
        // genus, `efo:EFO_0000324 ! cell type`); label it like any reference.
        Some((_, [])) => label_comment(labels, toks),
        Some((rel, rest)) => {
            // ...and ALSO only when the relation's OBO id is CANONICAL: a mechanical
            // OBO PURL with an all-numeric local part (`BFO:0000050`, `RO:0000087`).
            // A letter-bearing local (`NCIT:R81`, `NCIT:C99999`) is not resolved as a
            // relation head even though it carries an rdfs:label:
            // `relationship: NCIT:R81 T:0000002 ! continuant` labels the target only.
            // (The filler is always name-resolved; this restriction is head-only.)
            let head_labelled = match rel.split_once(':') {
                None => false,
                Some((_p, local)) => {
                    // The all-numeric local part is the whole test. Every case the
                    // note above cites already fails on it — `obo1:has_role`,
                    // `efo:EFO_0000784`, `NCIT:R81` — so the extra "prefix is not a
                    // declared idspace" condition never decided any of them, and it
                    // is wrong where a declared idspace IS the entity's canonical OBO
                    // id: EFO declares `idspace: EFO http://www.ebi.ac.uk/efo/EFO_`,
                    // and `relationship: EFO:0000784 UBERON:0001004` must carry
                    // `! has_disease_location respiratory system`.
                    !local.is_empty() && local.bytes().all(|b| b.is_ascii_digit())
                }
            };
            if head_labelled {
                label_comment(labels, toks)
            } else {
                label_comment(labels, rest)
            }
        }
        None => String::new(),
    }
}

/// Shorten every `<IRI>` inside an OBO macro template to its local name, as
/// `expand_expression_to`/`expand_assertion_to` are written: the stored
/// literal holds full IRIs (`<…/obo/BFO_0000051> some (…)`) but the OBO clause
/// reads `BFO_0000051 some (…)`. Tokens already written bare (`part_of`) and all
/// whitespace, including embedded newlines, are left exactly as they are.
fn shorten_macro_iris(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('>') {
            Some(close) => {
                let iri = &after[..close];
                // Only OBO-PURL IRIs shorten to their bare local id;
                // a non-OBO IRI (owl:Nothing) is kept in full `<…>` form.
                if let Some(local) = iri.strip_prefix("http://purl.obolibrary.org/obo/") {
                    out.push_str(local);
                } else {
                    out.push('<');
                    out.push_str(iri);
                    out.push('>');
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn label_comment(labels: &HashMap<String, String>, toks: &[&str]) -> String {
    // A `! label` is appended for any referenced entity that has
    // one, keyed by its rendered id — including a full-IRI target such as
    // `http://identifiers.org/ncbigene/26468` (an `owl:Class` labelled "LHX6"). A
    // target with no label (an ORCID individual) simply isn't in `labels` and so
    // contributes nothing.
    let names: Vec<&str> = toks
        .iter()
        .filter_map(|t| labels.get(*t).map(|s| s.as_str()))
        .collect();
    if names.is_empty() {
        return String::new();
    }
    // The joined names are trimmed before the `" ! "` is prepended, so
    // leading/trailing whitespace in a LABEL never reaches the file. HPO's French
    // translations carry several — `"Mouvements involontaires "`,
    // `" Retard développemental…"` — and emitting them raw puts a stray space on 72
    // `is_a:` lines of `hp-fr.obo`. The trim strips code points <= U+0020, not
    // Unicode whitespace.
    let joined = names.join(" ");
    let trimmed = joined.trim_matches(|c: char| c <= '\u{20}');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(" ! {trimmed}")
    }
}

/// Emit the lines of one repeated tag, sorted the way OBO orders clauses:
/// case-insensitively on the clause *value*, before the `{qualifier}` block and
/// the `! label` comment are appended. The sort is stable, so equal keys keep the
/// (deterministic) axiom order they were collected in.
/// Like [`write_sorted`], but collapses clauses that render to the SAME line — two
/// synonym axioms differing only by a language tag (`"X"` and `"X"@en`) are one
/// OBO clause. Safe only where distinct clauses always render
/// distinctly (synonyms); a general dedup would swallow other tags' rendering.
fn write_sorted_dedup<W: Write>(writer: &mut W, tag: &str, mut lines: Vec<(String, String)>) -> Result<()> {
    lines.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut seen = std::collections::HashSet::new();
    for (_, line) in lines {
        if seen.insert(line.clone()) {
            writeln!(writer, "{tag}: {line}")?;
        }
    }
    Ok(())
}

fn write_sorted<W: Write>(writer: &mut W, tag: &str, mut lines: Vec<(String, String)>) -> Result<()> {
    // A tag's clauses sort case-INSENSITIVELY (the `a.0` key is case-folded), then
    // break ties case-SENSITIVELY via the rendered line, which leads with the clause
    // value — `"Acetylcholine"` before `"acetylcholine"`. A residue of
    // same-value/different-qualifier clauses (e.g. three
    // `xref: CAS:64-19-7 {source=…}`) is left in the order it arrived in; the tags
    // where that order is load-bearing fold the axiom-set bucket into `a.0`
    // themselves.
    lines.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (_, line) in lines {
        writeln!(writer, "{tag}: {line}")?;
    }
    Ok(())
}

fn write_stanza<W: Write>(
    writer: &mut W,
    ctx: &Ctx,
    labels: &HashMap<String, String>,
    iri: &str,
    sd: Option<&SubjData>,
    kind: Stanza2,
    aa_cap: usize,
    subclass_cap: usize,
) -> Result<()> {
    let typedef = kind != Stanza2::Term;
    // The document's declared idspace prefixes — a relation head shortened with
    // one of these is a non-OBO-PURL CURIE whose label is omitted from a
    // `relationship:`/`intersection_of:` `! comment` (see `label_comment_pred`).
    let declared_prefixes: std::collections::HashSet<&str> =
        ctx.idspaces.iter().map(|(p, _)| p.as_str()).collect();
    let empty = SubjData::default();
    let sd = sd.unwrap_or(&empty);
    // The stanza id is derived from the IRI (plus any relation shorthand) — an
    // `oboInOwl:id` annotation is *not* authoritative and is ignored. CL's
    // cl-base.owl carries five stale `oboInOwl:id "CL:99xxxxx"` from temporary-id
    // terms whose IRIs were long since renumbered; honouring them renames those
    // five stanzas.
    let id = ctx.id(iri);
    let self_ref = id.clone();
    let _ = &sd.id;

    // --- The tags shared by [Term] and [Typedef], in OBO's tag order. ---
    writeln!(writer, "id: {id}")?;
    // One `name:` clause per distinct `rdfs:label`, ordered by case-folded value,
    // then case-sensitive value, then the axiom-set bucket for value-ties. A
    // single-label entity yields one line; a multi-label one (OBI:0000295's
    // `is_input_of` / `is specified input of`) yields one each, with any qualifiers.
    if sd.name.is_some() || !sd.extra_names.is_empty() {
        // Collect the distinct labels to emit as `name:` clauses — one per distinct
        // `rdfs:label`. A relation carrying both a readable label and its underscore
        // form (`part of` + `part_of`, `results in fission of` +
        // `results_in_fission_of`) yields both `name:` lines: a label equal to the
        // entity's shorthand is NOT suppressed. Labels that render to the same
        // clause (BFO:0000050's `"part of"` and `"part of"@en`) collapse to one via
        // the value+annotation dedup, while OBI:0000295's two genuinely distinct
        // labels stay two.
        let mut items: Vec<(&String, &BTreeSet<Annotation<RcStr>>)> = Vec::new();
        for (t, a) in sd.name.iter().chain(sd.extra_names.iter()) {
            if t.is_empty() {
                continue;
            }
            if items.iter().any(|(t2, a2)| *t2 == t && *a2 == a) {
                continue;
            }
            items.push((t, a));
        }
        // Every distinct label is a `name:` clause. `name:` is single-valued in the
        // OBO *structure*, but that is a cardinality rule the structure check
        // enforces by REFUSING the document, not by silently keeping one clause —
        // so an entity with two genuinely different labels writes two lines, and
        // GSSO:000413's three write three.
        write_sorted(writer, "name", items.into_iter().map(|(text, anns)| {
            let (dbxrefs, syn_type, mut quals) = ax_ann_pieces(ctx, anns);
            // On a `name:` clause, a `hasSynonymType` annotation is a qualifier
            // `{has_synonym_type="…"}`, not the type token a synonym clause uses.
            if let Some(st) = syn_type {
                quals.push((format!("{OIO}hasSynonymType"), "has_synonym_type".to_string(), st.clone(), false, None, None, st));
            }
            let coll = owlapi_aa_collection_hash(ctx, anns);
            // The literal's LANGUAGE TAG is part of the axiom hash, so it has to be
            // recovered here — `sd.name`/`sd.extra_names` carry only the text.
            // Without it a `"X"@ja` label hashes as if it were plain, which lands it
            // in the wrong bucket and reverses the two `name:` lines whenever the
            // English and Japanese labels are the SAME STRING and the value
            // comparison therefore ties (MONDO:0019020 "PANDAS", MONDO:0018276
            // "CADDS", …).
            let lang = sd
                .label_axioms
                .iter()
                .find(|(v, _, a)| v == text && a == anns)
                .and_then(|(_, l, _)| l.clone());
            let bucket = owlapi_aa_bucket(
                owlapi_label_axiom_hash(iri, text, lang.as_deref(), coll),
                aa_cap,
            );
            let quals = quals_with_xrefs_hashset(&dbxrefs, &quals);
            (
                format!("{}\u{0}{}\u{1}{bucket:010}", fold(text), text),
                format!("{}{}", escape_name(text), render_quals(&quals)),
            )
        }).collect())?;
    }
    // A `namespace:` equal to the header `default-namespace` is suppressed (it is
    // implied); every other value is emitted, sorted by the clause comparison
    // (fold-min).
    let mut nss: Vec<&String> = sd
        .namespace
        .iter()
        .filter(|ns| ctx.default_namespace.as_deref() != Some(ns.as_str()))
        .collect();
    nss.sort_by(|a, b| fold(a).cmp(&fold(b)).then_with(|| a.cmp(b)));
    for ns in nss {
        writeln!(writer, "namespace: {}", escape_name(ns))?;
    }
    write_sorted(writer, "alt_id", sd.alt_ids.iter().map(|a| (fold(a), a.clone())).collect())?;
    // One `def:` clause per distinct IAO:0000115 definition. A term
    // with several (e.g. EFO_0004253's two MeSH-sourced defs, or AfPO defs that share
    // text but differ in `def=` source) yields one line each, ordered by
    // case-folded value, then case-sensitive value, then the
    // axiom-set bucket order for value-tied clauses. An OBO structure
    // check rejects >1 def, but the EFO release converts with checking off, so
    // every distinct definition survives.
    if sd.def.is_some() || !sd.extra_defs.is_empty() {
        // Distinct clauses only: a `"…"` and `"…"@en` pair for the same definition
        // (OBI:0400103's "DNA sequencer" def) renders identically, so collapse to one;
        // AfPO's same-text/different-`def=`-source defs stay two.
        let mut ditems: Vec<(&String, &BTreeSet<Annotation<RcStr>>)> = Vec::new();
        for (t, a) in sd.def.iter().chain(sd.extra_defs.iter()) {
            if t.is_empty() || ditems.iter().any(|(t2, a2)| *t2 == t && *a2 == a) {
                continue;
            }
            ditems.push((t, a));
        }
        write_sorted(writer, "def", ditems.into_iter().map(|(text, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let coll = owlapi_aa_collection_hash(ctx, anns);
            let bucket = owlapi_aa_bucket(owlapi_aa_axiom_hash(iri, IAO_DEF, text, false, coll), aa_cap);
            (
                format!("{}\u{0}{}\u{1}{bucket:010}", fold(text), text),
                format!("\"{}\" {}{}", escape(text), render_bracket(&dbxrefs), render_quals(&plain(&owlapi_hashset_order(quals)))),
            )
        }).collect())?;
    }
    // One `comment:` line per `rdfs:comment` axiom, each carrying its
    // own `{xref=…}`, sorted case-insensitively — CL:4033035's three NS-forest notes
    // and CL:0000055's "define using PATO…" / "Redundant grouping term" come out as
    // separate lines, not one space-joined clause. Identical comments collapse.
    if !sd.comments.is_empty() {
        let mut items: Vec<&(String, BTreeSet<Annotation<RcStr>>)> =
            sd.comments.iter().filter(|(t, _)| !t.is_empty()).collect();
        items.sort_by(|a, b| fold(&a.0).cmp(&fold(&b.0)));
        items.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        for (text, anns) in items {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs_hashset(&dbxrefs, &quals);
            writeln!(writer, "comment: {}{}", escape_unquoted(text), render_quals(&quals))?;
        }
    }
    write_sorted(writer, "subset", sd.subsets.iter().map(|(s, raw, is_iri, anns)| {
        let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
        // Two `subset:` clauses naming the same subset tie on (tag, value), so
        // their order is the order the assertions were consumed — the
        // annotation-assertion bucket order `name:` and `xref:`
        // already use. EFO:0000218 carries `gard_rare` twice, with different
        // `{source=…}` qualifiers, and sorting on the name alone reverses them.
        let coll = owlapi_aa_collection_hash(ctx, anns);
        let bucket = owlapi_aa_bucket(
            owlapi_aa_axiom_hash(iri, &format!("{OIO}inSubset"), raw, *is_iri, coll),
            aa_cap,
        );
        (
            format!("{}\u{1}{bucket:010}", fold(s)),
            format!("{s}{}", render_quals(&quals_with_xrefs_hashset(&dbxrefs, &quals))),
        )
    }).collect())?;
    write_sorted_dedup(writer, "synonym", sd.synonyms.iter().filter(|(_, t, _, _)| !t.is_empty()).map(|(scope, text, lang, anns)| {
        let (dbxrefs, syn_type, quals) = ax_ann_pieces(ctx, anns);
        let type_tok = syn_type.map(|t| format!("{t} ")).unwrap_or_default();
        // The sort key is the *unquoted* text: `"interstitial cell"` comes
        // before `"interstitial cell of Leydig"`, which comparing the rendered
        // (quoted) lines would reverse. Same-text/same-scope synonyms that differ
        // only in qualifiers tie; break the tie in the axiom-set bucket order
        // (see `owlapi_aa_bucket`).
        //
        // The bucket is the finest position a subject's assertion set records, so
        // two synonyms that share text, scope AND bucket have no order of their
        // own. `write_sorted_dedup` settles those on the rendered line, which
        // costs nothing in fidelity and keeps the file reproducible.
        let syn_prop = match scope.as_str() {
            "EXACT" => format!("{OIO}hasExactSynonym"),
            "NARROW" => format!("{OIO}hasNarrowSynonym"),
            "BROAD" => format!("{OIO}hasBroadSynonym"),
            _ => format!("{OIO}hasRelatedSynonym"),
        };
        let coll = owlapi_aa_collection_hash(ctx, anns);
        let bucket = owlapi_aa_bucket(
            owlapi_aa_axiom_hash_full(iri, &syn_prop, text, None, lang.as_deref(), false, coll),
            aa_cap,
        );
        (
            format!("{}\u{0}{}\u{0}{}\u{1}{bucket:010}", fold(text), text, scope),
            format!("\"{}\" {} {}{}{}", escape(text), scope, type_tok, render_bracket(&dbxrefs), render_quals(&plain(&owlapi_hashset_order(quals)))),
        )
    }).collect())?;
    // `xref:` clauses that share a value merge, unioning their axiom annotations: a
    // plain `hasDbXref` (from the edit) and an annotated one (a mapping's
    // `{sssom:mapping_justification=…}`) for the same target collapse to one
    // qualified line rather than a bare plus a qualified line.
    //
    // Which of two duplicates survives is decided by the order the subject's
    // annotation assertions are walked in — bucket order, the same order the clause
    // sort keys below already use as a tie-break.
    let mut in_owlapi_order: Vec<(&String, &BTreeSet<Annotation<RcStr>>)> =
        sd.xrefs.iter().map(|(x, a)| (x, a)).collect();
    in_owlapi_order.sort_by_key(|(x, anns)| {
        let coll = owlapi_aa_collection_hash(ctx, anns);
        let h = owlapi_aa_axiom_hash(iri, &format!("{OIO}hasDbXref"), x.trim(), false, coll);
        owlapi_aa_bucket(h, aa_cap)
    });
    let mut merged_xrefs: Vec<(String, BTreeSet<Annotation<RcStr>>)> = Vec::new();
    for (x, anns) in in_owlapi_order {
        // Surrounding whitespace is trimmed off an xref value, so EFO's
        // ` CLO:0001200` (a stray leading space in the source hasDbXref) collapses
        // onto the clean `CLO:0001200` rather than emitting a second `xref:  …` line.
        let x = x.trim();
        // Two `hasDbXref` axioms with the SAME value but DIFFERENT axiom annotations
        // are two distinct `xref:` clauses — EFO's MedDRA:10002449 carries
        // {source="DOID:0111147"} on one and {source="ORDO:86886/e",
        // source="Orphanet:86886"} on another, and both are kept.
        //
        // The one qualifier that does NOT distinguish them is the xref's own
        // description. An `rdfs:label` on the xref axiom folds into the xref itself
        // rather than staying one of the clause's qualifiers, and two xrefs are
        // equal when their idrefs are — the description does not enter it. So the
        // redundancy check sees two equal clauses and drops the later one:
        // GO:0055085's two
        // `Reactome:R-HSA-382556` xrefs, described "ABC-family protein mediated
        // transport" and "ABC-family proteins mediated transport", write one line.
        // The survivor is the first in axiom order, keeping its own description.
        let key = xref_dedup_anns(anns);
        if !merged_xrefs.iter().any(|(v, a)| v == x && xref_dedup_anns(a) == key) {
            merged_xrefs.push((x.to_string(), anns.clone()));
        }
    }
    write_sorted(writer, "xref", merged_xrefs.iter().map(|(x, anns)| {
        let coll = owlapi_aa_collection_hash(ctx, anns);
        let bucket = owlapi_aa_bucket(owlapi_aa_axiom_hash(iri, &format!("{OIO}hasDbXref"), x, false, coll), aa_cap);
        let (dbxrefs, _, mut quals) = ax_ann_pieces(ctx, anns);
        // An xref value may carry a trailing quoted description in the OBO
        // `IDSPACE:LOCAL "description"` form — CHEBI stores the whole thing in one
        // `hasDbXref` literal (`Beilstein:147610 "Beilstein Registry Number"`). Split
        // it so the id is escaped as an id and the description spelled as the
        // trailing quoted string, not `\"…\"` inside the id.
        let (xid, embedded_desc) = xref_id_desc(x);
        // An `rdfs:label` on the xref axiom is likewise the xref's *description*,
        // spelled the same way (`xref: BAMS:1028 "MB"`), not a `{name="…"}` qualifier.
        let label_raw = quals
            .iter()
            .position(|(_, k, _, _, _, _, _)| k == "name")
            .map(|i| quals.remove(i).2)
            // An EMPTY description is no description: `xref: WB:rynl`, not
            // `xref: WB:rynl ""`. The qualifier is still consumed above, so it
            // does not fall through to the trailing `{…}` block either.
            .filter(|v| !v.is_empty());
        let label_tok = match &label_raw {
            Some(v) => format!(" \"{}\"", escape(v)),
            None => String::new(),
        };
        let desc_tok = match embedded_desc {
            Some(d) => format!(" \"{}\"", escape(d)),
            None => label_tok,
        };
        let quals = quals_with_xrefs_hashset(&dbxrefs, &quals);
        // An xref sorts on `idref + ' ' + description`, compared case-INsensitively
        // with a case-sensitive tie-break. The two ways an xref carries a
        // description end up on opposite sides of that space:
        //   * embedded in the `hasDbXref` literal (CHEBI's
        //     `Patent:DE2250327 "Patent"`) it is part of the IDREF, quotes and all,
        //     and the description slot is empty → `Patent:DE2250327 "Patent" null`;
        //   * from an `rdfs:label` on the xref axiom (HP's MedDRA pair) it IS the
        //     description → `MEDDRA:10038077 Rectal prolapse`.
        // So the described CHEBI clause sorts first ('"' < 'n') while the described
        // HP clause sorts second ('n' < 'r') — one rule, opposite outcomes. Keying on
        // the id alone with described-first gets CHEBI right and HP backwards.
        let id_part = match embedded_desc {
            Some(d) => format!("{xid} \"{d}\""),
            None => xid.to_string(),
        };
        let cmp = format!("{id_part} {}", label_raw.as_deref().unwrap_or("null"));
        (
            format!("{}\u{0}{}\u{1}{bucket:010}", fold(&cmp), cmp),
            format!("{}{desc_tok}{}", escape_xref(xid), render_quals(&quals)),
        )
    }).collect())?;

    // `property_value:` sits after `xref:` in a [Typedef] but after
    // `relationship:` in a [Term] — the two frame types have different tag orders,
    // and CL's `cl.obo` shows both.
    // One `property_value:` clause, keyed and rendered. Both routes into the tag go
    // through this: the assertions that are always a `property_value:`, and — in a
    // [Typedef] — the metadata-tag IRI assertions that a [Term] would have made a
    // `relationship:`. A clause sorts against its neighbours by predicate then value,
    // so the two routes must produce the SAME key shape; a shorter one compares its
    // value against the other's predicate and orders the tag by neither.
    let pv_entry = |pred: &String,
                    val: &String,
                    is_iri: bool,
                    dt: &Option<String>,
                    anns: &BTreeSet<Annotation<RcStr>>,
                    prop_iri: &str,
                    val_iri: &str| {
        let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
        let quals = quals_with_xrefs(&dbxrefs, &quals);
        let line = if is_iri {
            format!("{pred} {val}{}", render_quals(&quals))
        } else {
            let dt_tok = dt.clone().unwrap_or_else(|| "xsd:string".into());
            format!("{pred} \"{}\" {dt_tok}{}", escape(val), render_quals(&quals))
        };
        // Key on predicate + raw (unquoted) value, so a literal and an IRI value
        // of the same property interleave in value order. Two clauses with the
        // same predicate AND value tie there — UBERON carries `oboInOwl:status
        // "Verified"` twice, once sourced to a ROR id and once to an ORCID — and
        // the tie goes to the axiom-set bucket order, not to whichever qualifier
        // sorts first.
        let coll = owlapi_aa_collection_hash(ctx, anns);
        // The value's DATATYPE is part of the axiom hash. A `property_value:` is the
        // one clause whose main value is routinely typed — GSSO's Dewey numbers are
        // `xsd:decimal` — and hashing them as if they were plain puts three clauses
        // that differ only in a qualifier in the wrong order.
        let dt_iri = dt.as_deref().map(expand_datatype);
        // An IRI value hashes as its FULL IRI; `val` is only the CURIE the clause
        // prints, and hashing that puts every IRI-valued clause in the wrong bucket.
        let hash_val = if is_iri { val_iri } else { val.as_str() };
        let bucket = owlapi_aa_bucket(
            owlapi_aa_axiom_hash_full(
                iri,
                prop_iri,
                hash_val,
                dt_iri.as_deref(),
                None,
                is_iri,
                coll,
            ),
            aa_cap,
        );
        if std::env::var_os("OM_PV_DEBUG").is_some() {
            eprintln!(
                "[pv]\t{iri}\tcap={aa_cap}\tbucket={bucket}\thash={}\tcoll={coll}\tdt={:?}\t{line}",
                owlapi_aa_axiom_hash_full(iri, prop_iri, hash_val, dt_iri.as_deref(), None, is_iri, coll),
                dt_iri
            );
        }
        // Each of predicate and value compares case-INSENSITIVELY first and, when
        // that ties, case-SENSITIVELY — so `"Olof"` precedes `"olof"`. Only clauses
        // that tie on BOTH fall through to the axiom-set bucket.
        (
            format!(
                "{}\u{0}{pred}\u{0}{}\u{0}{val}\u{1}{bucket:010}",
                fold(pred),
                fold(val)
            ),
            line,
        )
    };
    let mut property_values: Vec<(String, String)> = sd
        .property_values
        .iter()
        .map(|(pred, val, is_iri, dt, anns, prop_iri, val_iri)| {
            pv_entry(pred, val, *is_iri, dt, anns, prop_iri, val_iri)
        })
        .collect();

    if !typedef {
        // One `is_a` clause per SubClassOf AXIOM — there is no
        // folding on (parent, GCI). Only the annotations of a *single* axiom combine,
        // into that one clause's qualifier list. The four same-parent shapes:
        //
        //   plain + `{source="A"}`          -> two clauses (`{source="A"}`, then bare)
        //   `{source="A"}` + `{source="B"}` -> two clauses
        //   plain + `{is_inferred="true"}`  -> two clauses (is_inferred DOES print)
        //   one axiom, two `source` anns    -> ONE clause `{source="A", source="B"}`
        //
        // Folding these costs MONDO dearly: `remove` re-asserts the hierarchy as
        // plain axioms (see `span_gaps`), so every annotated `is_a:` legitimately
        // has an unannotated twin, and collapsing the pair drops 44,774 lines from
        // `filtered.obo`. If a *reduced* release shows a duplicate parent it should
        // not, the fault is `reduce` leaving a redundant axiom behind — fix it
        // there, not by rewriting the writer's rule.
        let is_a = &sd.is_a;
        write_sorted(writer, "is_a", is_a.iter().map(|(p, anns, gci, hash)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_relationship(&dbxrefs, &quals, gci);
            // The clause comparison keys only on the value (the parent id); the
            // `{gci_*}` qualifiers do not enter it, so same-parent clauses — plain and
            // GCI alike — tie and fall to the SubClassOf axiom-set bucket order.
            let bucket = owlapi_aa_bucket(*hash, subclass_cap);
            let key = format!("{}\u{0}{}\u{1}{bucket:010}", fold(p), p);
            (key, format!("{p}{}{}", render_quals(&quals), label_comment(labels, &[p])))
        }).collect())?;
        // A genus line (one token) always precedes the differentiae, which are then
        // sorted among themselves — the clause key leads with its argument count.
        // No dedup: clauses append unconditionally, so two DIFFERENT equivalence
        // axioms that share a genus really do write `intersection_of: EFO:0000408`
        // twice (eight EFO classes do). What prevents a spurious repeat is that the
        // axioms themselves live in a set, so one axiom can only ever contribute its
        // operands once.
        let ii: Vec<&(Vec<String>, Vec<(String, String)>, BTreeSet<Annotation<RcStr>>)> =
            sd.intersection_of.iter().collect();
        write_sorted(writer, "intersection_of", ii.into_iter().map(|(toks, extra, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_relationship(&dbxrefs, &quals, extra);
            let refs: Vec<&str> = toks.iter().map(|s| s.as_str()).collect();
            let value = toks.join(" ");
            (
                format!("{}\u{0}{}\u{0}{value}", toks.len(), fold(&value)),
                format!("{value}{}{}", render_quals(&quals), label_comment_pred(labels, &declared_prefixes, &refs)),
            )
        }).collect())?;
        if sd.union_of.len() >= 2 {
            write_sorted(writer, "union_of", sd.union_of.iter().map(|u| {
                (fold(u), format!("{u}{}", label_comment(labels, &[u])))
            }).collect())?;
        }
        write_sorted(writer, "equivalent_to", sd.equivalent_to.iter().map(|e| {
            (fold(e), format!("{e}{}", label_comment(labels, &[e])))
        }).collect())?;
        write_sorted(writer, "disjoint_from", sd.disjoint_from.iter().map(|(dj, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(dj), format!("{dj}{}{}", render_quals(&quals), label_comment(labels, &[dj])))
        }).collect())?;
        // In a [Term], the deferred shorthand-IRI annotations are `relationship:`s.
        let mut all_rels: Vec<(String, String, BTreeSet<Annotation<RcStr>>, Vec<(String, String)>, i32)> =
            sd.relationships.clone();
        // These come from annotation assertions, not `SubClassOf`, so they carry no
        // subclass-axiom bucket; 0 is a stable placeholder (they do not tie on value).
        for (pred, val, anns, _, _) in &sd.rel_or_pv {
            all_rels.push((pred.clone(), val.clone(), anns.clone(), Vec::new(), 0));
        }
        // As with `is_a`, there is one `relationship:` clause per axiom — an
        // asserted `SubClassOf(X, R some Y)` and an annotated one over the same
        // (rel, target) are two clauses, not one folded clause: a plain and a
        // `{source="A"}` existential on the same rel+target render as two lines
        // (plain first here, the order falling out of the SubClassOf axiom-set
        // bucket). The pair only merges when the ontology goes through RDF/XML — see
        // the note on `is_a` above; the OBO writer is the wrong place for it. Folding
        // here costs MONDO's `filtered.obo` 13,763 `relationship:` lines, the
        // anonymous-superclass twins `span_gaps` adds.
        //
        // A cardinality/all_only restriction still keeps its own synthetic qualifier
        // and is its own clause anyway (`has_member Y` vs `has_member Y
        // {cardinality="1"}`, UBERON:0000170).
        let rels = &all_rels;
        write_sorted(writer, "relationship", rels.iter().map(|(r, t, anns, gci, hash)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_relationship(&dbxrefs, &quals, gci);
            // The clause comparison keys only on the value (`rel target`); the
            // `{gci_*}`/`{all_only}` qualifiers do not enter it, so same rel+target
            // clauses tie and break in the SubClassOf axiom-set bucket order.
            let bucket = owlapi_aa_bucket(*hash, subclass_cap);
            let key = format!("{}\u{0}{r} {t}\u{1}{bucket:010}", fold(&format!("{r} {t}")));
            (key, format!("{r} {t}{}{}", render_quals(&quals), label_comment_pred(labels, &declared_prefixes, &[r, t])))
        }).collect())?;
        write_sorted(writer, "property_value", property_values)?;
    } else {
        // In a [Typedef], the deferred shorthand-IRI annotations are `property_value:`s
        // — the same clause as any other, so they take the same key.
        for (pred, val, anns, prop_iri, val_iri) in &sd.rel_or_pv {
            property_values.push(pv_entry(pred, val, true, &None, anns, prop_iri, val_iri));
        }
        write_sorted(writer, "property_value", property_values)?;
        write_sorted(writer, "domain", sd.domain.iter().map(|(d, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(d), format!("{d}{}{}", render_quals(&quals), label_comment(labels, &[d])))
        }).collect())?;
        write_sorted(writer, "range", sd.range.iter().map(|(r, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(r), format!("{r}{}{}", render_quals(&quals), label_comment(labels, &[r])))
        }).collect())?;
        // A two-link chain headed by the property itself is `transitive_over:`;
        // any other two-link chain is `holds_over_chain:`. Chains of three or more
        // links have no OBO tag at all and belong in `owl-axioms:`; emitting one as
        // `holds_over_chain:` would invent a tag the format does not have and change
        // the axiom's meaning on re-read.
        // One clause per distinct chain, merging the annotations of duplicate chain
        // axioms (a bare and a `{RO:0002582=…}`-annotated twin) into a single
        // qualifier block.
        let mut merged_chains: Vec<(Vec<String>, BTreeSet<Annotation<RcStr>>)> = Vec::new();
        for (links, anns) in &sd.chains {
            if let Some((_, existing)) = merged_chains.iter_mut().find(|(l, _)| l == links) {
                existing.extend(anns.iter().cloned());
            } else {
                merged_chains.push((links.clone(), anns.clone()));
            }
        }
        let mut chains: Vec<(String, String)> = Vec::new();
        let mut transitive_over: Vec<(String, String)> = Vec::new();
        for (chain, anns) in &merged_chains {
            if chain.len() != 2 {
                continue;
            }
            if chain[0] == self_ref || chain[0] == id {
                let t = &chain[1];
                transitive_over.push((fold(t), format!("{t}{}", label_comment(labels, &[t]))));
            } else {
                let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
                let quals = quals_with_xrefs(&dbxrefs, &quals);
                chains.push((fold(&chain.join(" ")), format!("{}{}", chain.join(" "), render_quals(&quals))));
            }
        }
        write_sorted(writer, "holds_over_chain", chains)?;
        if sd.reflexive {
            writeln!(writer, "is_reflexive: true")?;
        }
        if sd.symmetric {
            writeln!(writer, "is_symmetric: true")?;
        }
        if sd.transitive || sd.transitive_anno == Some(true) {
            writeln!(writer, "is_transitive: true")?;
        } else if sd.transitive_anno == Some(false) {
            writeln!(writer, "is_transitive: false")?;
        }
        if sd.functional {
            writeln!(writer, "is_functional: true")?;
        }
        if sd.inverse_functional {
            writeln!(writer, "is_inverse_functional: true")?;
        }
        write_sorted(writer, "is_a", sd.sub_property_of.iter().map(|sp| {
            (fold(sp), format!("{sp}{}", label_comment(labels, &[sp])))
        }).collect())?;
        // `disjoint_from:` follows `is_a:` in a [Typedef], from DisjointObjectProperties.
        write_sorted(writer, "disjoint_from", sd.disjoint_from.iter().map(|(dj, anns)| {
            let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
            let quals = quals_with_xrefs(&dbxrefs, &quals);
            (fold(dj), format!("{dj}{}{}", render_quals(&quals), label_comment(labels, &[dj])))
        }).collect())?;
        write_sorted(writer, "inverse_of", sd.inverse_of.iter().map(|inv| {
            (fold(inv), format!("{inv}{}", label_comment(labels, &[inv])))
        }).collect())?;
        write_sorted(writer, "transitive_over", transitive_over)?;
    }

    if sd.deprecated {
        let (dbxrefs, _, quals) = ax_ann_pieces(ctx, &sd.deprecated_anns);
        let quals = quals_with_xrefs(&dbxrefs, &quals);
        writeln!(writer, "is_obsolete: true{}", render_quals(&quals))?;
    }
    write_sorted(writer, "replaced_by", sd.replaced_by.iter().map(|r| (fold(r), r.clone())).collect())?;
    write_sorted(writer, "consider", sd.consider.iter().map(|(c, anns)| {
        let (dbxrefs, _, quals) = ax_ann_pieces(ctx, anns);
        // `consider`'s `{source=…}` qualifiers route through the annotation hash set
        // like xref/def/synonym (not the ascending property/value sort), so their
        // block order is hash-bucket order, not alphabetical.
        let quals = quals_with_xrefs_hashset(&dbxrefs, &quals);
        (fold(c), format!("{c}{}", render_quals(&quals)))
    }).collect())?;
    // The clauses of a repeated tag sort by value, so a term with several
    // `created_by:` annotations (EFO_0000001 has three editors; EFO_0004017 two)
    // emits them in string order — not the source/parse order.
    let mut created_by: Vec<&String> = sd.created_by.iter().collect();
    created_by.sort();
    for cb in created_by {
        writeln!(writer, "created_by: {cb}")?;
    }
    for cd in &sd.creation_date {
        writeln!(writer, "creation_date: {cd}")?;
    }
    if let Some(sh) = &sd.shorthand {
        let _ = sh; // shorthand is reconstructed from the id+xref; not re-emitted
    }
    if typedef {
        for (v, anns) in &sd.expand_assertion_to {
            let (dbxrefs, _, _) = ax_ann_pieces(ctx, anns);
            writeln!(writer, "expand_assertion_to: \"{}\" {}", escape(&shorten_macro_iris(v)), render_bracket(&dbxrefs))?;
        }
        for (v, anns) in &sd.expand_expression_to {
            let (dbxrefs, _, _) = ax_ann_pieces(ctx, anns);
            writeln!(writer, "expand_expression_to: \"{}\" {}", escape(&shorten_macro_iris(v)), render_bracket(&dbxrefs))?;
        }
    }
    // Annotation-property typedefs must carry `is_metadata_tag: true` so the
    // reader re-classifies them as annotation properties (not object properties).
    if kind == Stanza2::AnnotationProperty || (typedef && sd.is_metadata_tag) {
        writeln!(writer, "is_metadata_tag: true")?;
    }
    if typedef && sd.is_class_level {
        writeln!(writer, "is_class_level: true")?;
    }
    // `is_asymmetric` has no assigned place in the Typedef tag order, so it is
    // written last, after even `expand_expression_to`.
    if typedef && sd.asymmetric {
        writeln!(writer, "is_asymmetric: true")?;
    }
    Ok(())
}

/// Percent-decode a URI fragment to the string the OBO id is built from —
/// `R%C3%A9union` → `Réunion`. Only complete `%HH` pairs decode; a
/// stray `%` or invalid UTF-8 result is left verbatim. A fragment with no `%` is
/// returned as-is (the common case), so ordinary obo ids pay nothing.
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let hex = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn escape(s: &str) -> String {
    // OBO escaping (reverse of `unescape_obo`) for a *quoted* value — a `def:`
    // text, a `synonym:` text, a literal `property_value:` or a `{key="…"}`
    // qualifier: backslash, quote, newline. An unescaped newline in a value breaks
    // the OBO parser (the continuation is read as a new tag), so it must escape.
    // A literal TAB, though, stays as-is even inside a quoted value — AfPO's
    // coordinate `property_value`s and three EFO `def`s carry real tabs, written
    // verbatim. Braces are *not* escaped here; they only need escaping where they
    // are not already inside quotes.
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Escaping for an unquoted `comment:` value. The qualifier braces escape (they
/// would otherwise start a `{…}` block) but a double quote deliberately does not —
/// CL's `comment: The term "neuroepithelial cell" is used …` is written verbatim,
/// and escaping it makes the round-trip text differ.
fn escape_unquoted(s: &str) -> String {
    // A literal TAB is left as-is: it is only a field delimiter in the `[Term]`
    // header line, never inside a clause value — RO:0002120's macro-expansion
    // comment carries an indented multi-line body with real tabs. Newlines still
    // escape, since a bare one would end the clause.
    s.replace('\\', "\\\\")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('\n', "\\n")
}

/// Escaping for an unquoted `name:`/`namespace:` value: as `escape_unquoted`,
/// plus the double quote.
fn escape_name(s: &str) -> String {
    escape_unquoted(s).replace('"', "\\\"")
}

/// Escaping for an xref id, in an `xref:` tag or inside a `[…]` list. `:`
/// separates idspace from local id, `,` separates list entries and `]` ends
/// the list, so every one of those after the leading idspace colon must be
/// escaped — as in CL's `doi:10.1023/a\:1018564904170`.
/// Split an OBO xref value into its id and optional trailing quoted description:
/// `Beilstein:147610 "Beilstein Registry Number"` → (`Beilstein:147610`, `Beilstein
/// Registry Number`). CHEBI stores the description inside the `hasDbXref` literal, so
/// the writer must recover the two parts (the description is spelled as an
/// unescaped trailing quoted string). A value with no ` "…"` suffix is all id.
fn xref_id_desc(x: &str) -> (&str, Option<&str>) {
    if let Some(body) = x.strip_suffix('"') {
        if let Some(pos) = body.find(" \"") {
            return (&x[..pos], Some(&body[pos + 2..]));
        }
    }
    (x, None)
}

/// An xref is split at its FIRST colon and the two halves are escaped separately,
/// so that separator colon comes out bare:
///
/// ```text
/// colonPos = index of first ':'
/// colonPos > 0  =>  escape(prefix) + ':' + escape(local)
/// otherwise     =>  escape(idref)          // whole string, colon included
/// ```
///
/// The guard is `> 0`, not `>= 0`: an xref that BEGINS with a colon has no prefix
/// to split off, so the whole value is escaped and the colon becomes `\:`. HPO's
/// `hp-full.obo` has one such xref — a bare `:` — which must come out as `[\:]`,
/// not `[:]`.
fn escape_xref(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // The separator is the first colon at a position > 0. A value that BEGINS with
    // a colon has no prefix, so no colon in it is a separator and all are escaped.
    let mut separator_taken = s.starts_with(':');
    for c in s.chars() {
        match c {
            ':' if !separator_taken => {
                separator_taken = true;
                out.push(':');
            }
            '\\' | ':' | ',' | ']' | '"' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}
