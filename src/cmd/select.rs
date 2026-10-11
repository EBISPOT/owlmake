//! Shared term-selection logic for `filter` and `remove`.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::model::Model;

/// Trim only code points `<= U+0020` — NOT Unicode whitespace.
///
/// A NO-BREAK SPACE (U+00A0) is whitespace to Unicode and is not trimmed here, so
/// a term carrying one keeps it and resolves to nothing.
pub(crate) fn ascii_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= '\u{20}')
}

/// One line of a `--term-file`, or `None` if it contributes no term.
///
/// The parse is three rules and no more: carriage returns are stripped; a line
/// whose first non-blank character is `#` contributes nothing; and a `#` at the
/// start of the line, or preceded by ASCII whitespace, ends the term, with what
/// remains trimmed.
///
/// Two details decide real seeds. It does NOT take the first whitespace token —
/// only a `#` comment is stripped, so `GO:1 ! label` stays whole and then fails
/// to resolve. And the trim strips only chars `<= U+0020`, so a NO-BREAK SPACE
/// survives: HPO's `chebi_terms.txt` ends seven lines with `U+00A0`, which read
/// as `CHEBI:15843\u{a0}` — a CURIE that does not expand, so (under
/// `--force true`) those lines drop out. Rust's `split_whitespace`/`trim` treat
/// U+00A0 as whitespace, so a naive read keeps those seven and pulls 34 extra
/// CHEBI classes into `merged_import.owl`.
pub(crate) fn term_line(line: &str) -> Option<&str> {
    let line = line.trim_end_matches('\r');
    let jtrim = ascii_trim;
    if jtrim(line).starts_with('#') {
        return None;
    }
    // A `#` at the start, or preceded by ASCII whitespace — `[ \t\n\x0B\f\r]`,
    // which excludes U+00A0.
    let is_ascii_ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r');
    let mut cut = None;
    for (i, c) in line.char_indices() {
        if c != '#' {
            continue;
        }
        if i == 0 {
            cut = Some(0);
            break;
        }
        if line[..i].chars().next_back().is_some_and(is_ascii_ws) {
            cut = Some(i);
            break;
        }
    }
    let body = jtrim(&line[..cut.unwrap_or(line.len())]);
    (!body.is_empty()).then_some(body)
}

/// Gather the seed term set from `--term` values and `--term-file` files, each
/// read as [`iri`] reads it. A term that names no IRI is no term at all.
pub fn collect_terms(
    model: &Model,
    terms: &[String],
    term_files: &[PathBuf],
) -> Result<HashSet<String>> {
    gather(terms, term_files, |t| iri(model, t))
}

/// [`collect_terms`] for a command that reads its terms as the document reads
/// its own CURIEs ([`expand_with_document_prefixes`]).
pub fn collect_terms_with_document_prefixes(
    model: &Model,
    terms: &[String],
    term_files: &[PathBuf],
) -> Result<HashSet<String>> {
    gather(terms, term_files, |t| Some(expand_with_document_prefixes(model, t)))
}

/// Each `--term` value and each line of each `--term-file`, as `read` reads it.
fn gather(
    terms: &[String],
    term_files: &[PathBuf],
    read: impl Fn(&str) -> Option<String>,
) -> Result<HashSet<String>> {
    let mut set = HashSet::new();
    for t in terms {
        set.extend(read(t));
    }
    for f in term_files {
        let content =
            std::fs::read_to_string(f).with_context(|| format!("reading term file {}", f.display()))?;
        for line in content.lines() {
            let Some(line) = term_line(line) else { continue };
            set.extend(read(line));
        }
    }
    Ok(set)
}

/// The IRI a term given on the command line names, read with the command line's
/// context ([`crate::context`]) — never with the document's own prefixes — or
/// `None` where it names none. Angle brackets around it are dropped.
///
/// It is trimmed as a term-file line is (see [`term_line`]): code points
/// `<= U+0020` only. A term carrying a NO-BREAK SPACE keeps it, so the CURIE
/// expands to an IRI that names no entity and the term selects nothing —
/// `imports/chebi_terms.txt` ends seven of its lines with one.
pub fn iri(model: &Model, s: &str) -> Option<String> {
    model.context.iri(bare(s))
}

/// [`iri`], or the term as written where it names no IRI, so that it matches
/// nothing.
pub fn expand(model: &Model, s: &str) -> String {
    iri(model, s).unwrap_or_else(|| bare(s).to_string())
}

/// The IRI a term names where a command reads it as the document reads its own
/// CURIEs: an IRI as itself, a CURIE of the document's prefixes, else one the
/// command line's context reads ([`iri`]), else the term as written, so that it
/// matches nothing. A prefix bound in neither expands to nothing: uPheno's
/// merged mirror passes fourteen `--root-phenotype` roots and MGPO is the one
/// prefix of them the built-in map does not bind, so `MGPO:0001001` names no
/// root unless the caller binds `MGPO`.
pub fn expand_with_document_prefixes(model: &Model, s: &str) -> String {
    let t = bare(s);
    if t.starts_with("http://") || t.starts_with("https://") || t.starts_with("urn:") {
        return t.to_string();
    }
    if let Ok(expanded) = model.prefixes.expand_curie_string(t) {
        return expanded;
    }
    if t.contains(':') {
        expand(model, t)
    } else {
        t.to_string()
    }
}

/// A term trimmed as a term-file line is, without the angle brackets around it.
fn bare(s: &str) -> &str {
    let s = ascii_trim(s);
    s.strip_prefix('<').and_then(|x| x.strip_suffix('>')).unwrap_or(s)
}

/// Declared entities grouped by kind (from `Declaration` axioms).
#[derive(Default)]
pub struct Entities {
    pub classes: HashSet<String>,
    pub object_properties: HashSet<String>,
    pub data_properties: HashSet<String>,
    pub annotation_properties: HashSet<String>,
    pub individuals: HashSet<String>,
    pub datatypes: HashSet<String>,
    /// Datatypes named as a LITERAL's type rather than in an entity position —
    /// `"2010-01-01"^^xsd:date` puts `xsd:date` here. They belong to the signature
    /// (a document that types a literal mentions that datatype) but not to the set
    /// a selector means by "datatypes", so they are kept apart.
    pub literal_datatypes: HashSet<String>,
}

impl Entities {
    /// Every declared entity IRI, across all kinds.
    pub fn all(&self) -> impl Iterator<Item = &String> {
        self.classes
            .iter()
            .chain(&self.object_properties)
            .chain(&self.data_properties)
            .chain(&self.annotation_properties)
            .chain(&self.individuals)
            .chain(&self.datatypes)
    }
}

/// Entities by kind as the OWL *signature* defines them: everything the ontology
/// mentions in an entity position, declared or not.
///
/// `entities` reads `Declaration` axioms only, whereas a signature is collected
/// from axiom structure, so an entity that is merely *referenced* still belongs to
/// it. The difference is not academic: MONDO's `mondo-base.owl` runs
/// `remove --input reasoned.owl --select imports` first, which strips the imports
/// that declared `BFO_0000004`/`BFO_0000050`; the later
/// `remove --select "<BFO_*>" --select classes` must still see `BFO_0000004` as a
/// class and drop the `rdfs:subClassOf` referencing it. Where the only mention of
/// those two is inside a `SubClassOf`, the class reference goes and
/// `Declaration(ObjectProperty(BFO_0000050))` is materialised for the retained
/// property.
pub fn signature_entities(model: &Model) -> Entities {
    use horned_owl::model::{
        AnnotationProperty, Class, DataProperty, Datatype, Literal, NamedIndividual, ObjectProperty,
        RcStr,
    };
    use horned_owl::visitor::immutable::{Visit, Walk};

    #[derive(Default)]
    struct TypedSig {
        e: Entities,
    }
    impl Visit<RcStr> for TypedSig {
        fn visit_class(&mut self, c: &Class<RcStr>) {
            self.e.classes.insert(c.0.as_ref().to_string());
        }
        fn visit_object_property(&mut self, p: &ObjectProperty<RcStr>) {
            self.e.object_properties.insert(p.0.as_ref().to_string());
        }
        fn visit_data_property(&mut self, p: &DataProperty<RcStr>) {
            self.e.data_properties.insert(p.0.as_ref().to_string());
        }
        fn visit_annotation_property(&mut self, p: &AnnotationProperty<RcStr>) {
            self.e.annotation_properties.insert(p.0.as_ref().to_string());
        }
        fn visit_named_individual(&mut self, i: &NamedIndividual<RcStr>) {
            self.e.individuals.insert(i.0.as_ref().to_string());
        }
        fn visit_datatype(&mut self, d: &Datatype<RcStr>) {
            self.e.datatypes.insert(d.0.as_ref().to_string());
        }
        // A typed literal's datatype is reached as a bare IRI, not as a datatype
        // entity, so it is collected here.
        fn visit_literal(&mut self, l: &Literal<RcStr>) {
            if let Literal::Datatype { datatype_iri, .. } = l {
                self.e.literal_datatypes.insert(datatype_iri.as_ref().to_string());
            }
        }
    }

    let mut walk = Walk::new(TypedSig::default());
    for ac in model.ont.iter() {
        // The whole ANNOTATED component: an axiom's signature includes the entities
        // named in its own annotations. In MONDO's extracted import module
        // `oboInOwl:hasDbXref` appears nowhere else — only as an annotation on the
        // definitions — so walking `ac.component` alone would leave it out of the
        // annotation-property signature and `--select complement` could not reach it.
        walk.annotated_component(ac);
    }
    walk.into_visit().e
}

/// Collect declared entities by kind.
pub fn entities(model: &Model) -> Entities {
    use horned_owl::model::Component as C;
    let mut e = Entities::default();
    for ac in model.ont.iter() {
        match &ac.component {
            C::DeclareClass(d) => {
                e.classes.insert(d.0 .0.to_string());
            }
            C::DeclareObjectProperty(d) => {
                e.object_properties.insert(d.0 .0.to_string());
            }
            C::DeclareDataProperty(d) => {
                e.data_properties.insert(d.0 .0.to_string());
            }
            C::DeclareAnnotationProperty(d) => {
                e.annotation_properties.insert(d.0 .0.to_string());
            }
            C::DeclareNamedIndividual(d) => {
                e.individuals.insert(d.0 .0.to_string());
            }
            C::DeclareDatatype(d) => {
                e.datatypes.insert(d.0 .0.to_string());
            }
            _ => {}
        }
    }
    e
}

type Rc = horned_owl::model::RcStr;

/// Any of the OWL entity declaration components.
pub fn is_declaration(comp: &horned_owl::model::Component<Rc>) -> bool {
    use horned_owl::model::Component as C;
    matches!(
        comp,
        C::DeclareClass(_)
            | C::DeclareObjectProperty(_)
            | C::DeclareDataProperty(_)
            | C::DeclareAnnotationProperty(_)
            | C::DeclareNamedIndividual(_)
            | C::DeclareDatatype(_)
    )
}

/// Whether an axiom is logical: neither a declaration, nor an annotation
/// axiom — an annotation assertion, a sub-annotation-property axiom, an
/// annotation property's domain or range — nor part of the ontology's header.
pub fn is_logical(comp: &horned_owl::model::Component<Rc>) -> bool {
    use horned_owl::model::Component as C;
    !matches!(
        comp,
        C::AnnotationAssertion(_)
            | C::SubAnnotationPropertyOf(_)
            | C::AnnotationPropertyDomain(_)
            | C::AnnotationPropertyRange(_)
            | C::OntologyAnnotation(_)
            | C::OntologyID(_)
            | C::DocIRI(_)
            | C::Import(_)
    ) && !is_declaration(comp)
}

/// The IRIs `comp` is about. `--axioms internal` selects an axiom when one of
/// them lies in a base namespace, and `--axioms external` every other axiom.
///
/// A declaration is about its entity; an assertion about its named individual or
/// annotation subject; a sub-class or sub-property axiom about its sub-class or
/// sub-property; a property characteristic, domain or range about the property;
/// and an inverse pair about its first property. An n-ary class or property
/// axiom is about each of its members, save that an equivalence of classes counts
/// its named members alone and an equivalence of properties needs two members to
/// be about any. Where that subject is an anonymous expression, each entity in it
/// is a subject: a property chain is about its links, and
/// `ObjectSomeValuesFrom(p C) ⊑ D` about `p` and `C`.
///
/// An assertion about an anonymous individual is about no IRI, and nor is an
/// axiom with no subject to give: a key, a disjoint union, a datatype definition,
/// sameness or difference of individuals, a negative assertion, a functional data
/// property, an annotation property's domain or range, and a rule. Each is
/// therefore external to any namespace.
pub fn axiom_subjects(comp: &horned_owl::model::Component<Rc>) -> Vec<String> {
    use horned_owl::model::{
        AnnotationSubject, ClassExpression as CE, Component as C, Individual,
        ObjectPropertyExpression as OPE, SubObjectPropertyExpression as SOPE,
    };
    let ope = |o: &OPE<Rc>| match o {
        OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => p.0.to_string(),
    };
    let ce = |c: &CE<Rc>| match c {
        CE::Class(cl) => vec![cl.0.to_string()],
        _ => crate::sig::class_expression_signature(c).into_iter().collect(),
    };
    let named = |i: &Individual<Rc>| match i {
        Individual::Named(n) => vec![n.0.to_string()],
        Individual::Anonymous(_) => Vec::new(),
    };
    // An equivalence of properties is about the sub-property of each pairwise
    // inclusion it implies, which one member alone does not give.
    let pairwise = |members: Vec<String>| if members.len() < 2 { Vec::new() } else { members };
    match comp {
        C::DeclareClass(d) => vec![d.0.0.to_string()],
        C::DeclareObjectProperty(d) => vec![d.0.0.to_string()],
        C::DeclareAnnotationProperty(d) => vec![d.0.0.to_string()],
        C::DeclareDataProperty(d) => vec![d.0.0.to_string()],
        C::DeclareNamedIndividual(d) => vec![d.0.0.to_string()],
        C::DeclareDatatype(d) => vec![d.0.0.to_string()],
        C::SubClassOf(ax) => ce(&ax.sub),
        C::EquivalentClasses(ax) => ax
            .0
            .iter()
            .filter_map(|c| match c {
                CE::Class(cl) => Some(cl.0.to_string()),
                _ => None,
            })
            .collect(),
        C::DisjointClasses(ax) => ax.0.iter().flat_map(ce).collect(),
        C::SubObjectPropertyOf(ax) => match &ax.sub {
            SOPE::ObjectPropertyExpression(o) => vec![ope(o)],
            SOPE::ObjectPropertyChain(chain) => chain.iter().map(ope).collect(),
        },
        C::SubDataPropertyOf(ax) => vec![ax.sub.0.to_string()],
        C::SubAnnotationPropertyOf(ax) => vec![ax.sub.0.to_string()],
        C::EquivalentObjectProperties(ax) => pairwise(ax.0.iter().map(ope).collect()),
        C::EquivalentDataProperties(ax) => pairwise(ax.0.iter().map(|p| p.0.to_string()).collect()),
        C::DisjointObjectProperties(ax) => ax.0.iter().map(ope).collect(),
        C::DisjointDataProperties(ax) => ax.0.iter().map(|p| p.0.to_string()).collect(),
        C::AnnotationAssertion(ax) => match &ax.subject {
            AnnotationSubject::IRI(i) => vec![i.to_string()],
            AnnotationSubject::AnonymousIndividual(_) => Vec::new(),
        },
        C::ClassAssertion(ax) => named(&ax.i),
        C::ObjectPropertyAssertion(ax) => named(&ax.from),
        C::DataPropertyAssertion(ax) => named(&ax.from),
        C::FunctionalObjectProperty(ax) => vec![ope(&ax.0)],
        C::InverseFunctionalObjectProperty(ax) => vec![ope(&ax.0)],
        C::ReflexiveObjectProperty(ax) => vec![ope(&ax.0)],
        C::IrreflexiveObjectProperty(ax) => vec![ope(&ax.0)],
        C::SymmetricObjectProperty(ax) => vec![ope(&ax.0)],
        C::AsymmetricObjectProperty(ax) => vec![ope(&ax.0)],
        C::TransitiveObjectProperty(ax) => vec![ope(&ax.0)],
        C::ObjectPropertyDomain(ax) => vec![ope(&ax.ope)],
        C::ObjectPropertyRange(ax) => vec![ope(&ax.ope)],
        C::DataPropertyDomain(ax) => vec![ax.dp.0.to_string()],
        C::DataPropertyRange(ax) => vec![ax.dp.0.to_string()],
        C::InverseObjectProperties(ax) => vec![ope(&ax.0)],
        _ => Vec::new(),
    }
}

/// Whether `comp` is an axiom internal to `base_iris`: one of its subjects
/// ([`axiom_subjects`]) lies in one of those namespaces.
fn is_internal(comp: &horned_owl::model::Component<Rc>, base_iris: &[String]) -> bool {
    axiom_subjects(comp)
        .iter()
        .any(|iri| base_iris.iter().any(|b| iri.starts_with(b.as_str())))
}

/// Whether `comp` is an axiom at all. The ontology's IRIs, its annotations and
/// its imports belong to the ontology rather than stating anything in it, so no
/// axiom selector reaches them. MONDO's `remove --base-iri …/MFOMD --axioms
/// external` keeps mfomd's four imports that way, and the merge over the mirrors
/// follows them to the terms its import module needs.
fn is_axiom(comp: &horned_owl::model::Component<Rc>) -> bool {
    use horned_owl::model::Component as C;
    !matches!(comp, C::OntologyID(_) | C::DocIRI(_) | C::OntologyAnnotation(_) | C::Import(_))
}

/// Axiom-type classification: does `comp` belong to the named category?
/// Covers `all`, `logical`, `annotation`, `subclass`, `subproperty`,
/// `equivalent`, `disjoint`, `type`, `tbox`, `abox`, `rbox`, `declaration`,
/// `internal`, `external`. `base_iris` defines the internal namespace(s).
pub fn axiom_in_category(
    comp: &horned_owl::model::Component<Rc>,
    cat: &str,
    base_iris: &[String],
) -> bool {
    use horned_owl::model::Component as C;
    match cat {
        "all" => true,
        "logical" => is_logical(comp),
        // The annotation-axiom family, not assertions alone: sub-annotation-
        // property axioms and annotation-property domains/ranges belong to it,
        // and a filter keeping "annotation" keeps a subsetdef's
        // `⊑ oboInOwl:SubsetProperty` — which is what keeps the subset VALUES
        // declared, so a later label-keeper can remove the `inSubset`
        // assertions pointing at them (12,928 `subset:` lines of the `-basic`
        // composites turned on exactly this).
        "annotation" => matches!(
            comp,
            C::AnnotationAssertion(_)
                | C::SubAnnotationPropertyOf(_)
                | C::AnnotationPropertyDomain(_)
                | C::AnnotationPropertyRange(_)
        ),
        "declaration" => is_declaration(comp),
        "subclass" => matches!(comp, C::SubClassOf(_)),
        // A property chain says a composition implies a property, not that one
        // property is under another, so it is no sub-property axiom.
        "subproperty" => match comp {
            C::SubDataPropertyOf(_) | C::SubAnnotationPropertyOf(_) => true,
            C::SubObjectPropertyOf(ax) => !matches!(
                ax.sub,
                horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(_)
            ),
            _ => false,
        },
        "equivalent" => matches!(
            comp,
            C::EquivalentClasses(_)
                | C::EquivalentObjectProperties(_)
                | C::EquivalentDataProperties(_)
        ),
        "disjoint" => matches!(
            comp,
            C::DisjointClasses(_)
                | C::DisjointObjectProperties(_)
                | C::DisjointDataProperties(_)
                | C::DisjointUnion(_)
        ),
        "type" => matches!(comp, C::ClassAssertion(_)),
        // `tbox`/`abox`/`rbox` group axioms by axiom TYPE, and that grouping is not
        // the conceptual partition the names suggest: the property *characteristic*
        // axioms (`FunctionalObjectProperty`, the domain/range axioms) sit in TBox,
        // `SubObjectPropertyOf` — chain form included — sits in RBox, and
        // DECLARATIONS are in none of the three. uPheno's merged mirror is where the
        // difference shows: `remove --term RO:0000052 --term RO:0002314 --axioms
        // tbox` must take `FunctionalObjectProperty(RO_0000052)`, which a class-level
        // reading of "tbox" leaves behind.
        "tbox" => matches!(
            comp,
            C::SubClassOf(_)
                | C::EquivalentClasses(_)
                | C::DisjointClasses(_)
                | C::ObjectPropertyDomain(_)
                | C::ObjectPropertyRange(_)
                | C::FunctionalObjectProperty(_)
                | C::InverseFunctionalObjectProperty(_)
                | C::DataPropertyDomain(_)
                | C::DataPropertyRange(_)
                | C::FunctionalDataProperty(_)
                | C::DatatypeDefinition(_)
                | C::DisjointUnion(_)
                | C::HasKey(_)
        ),
        "abox" => matches!(
            comp,
            C::ClassAssertion(_)
                | C::SameIndividual(_)
                | C::DifferentIndividuals(_)
                | C::ObjectPropertyAssertion(_)
                | C::NegativeObjectPropertyAssertion(_)
                | C::DataPropertyAssertion(_)
                | C::NegativeDataPropertyAssertion(_)
        ),
        "rbox" => matches!(
            comp,
            C::TransitiveObjectProperty(_)
                | C::DisjointDataProperties(_)
                | C::SubDataPropertyOf(_)
                | C::EquivalentDataProperties(_)
                | C::DisjointObjectProperties(_)
                | C::SubObjectPropertyOf(_)
                | C::EquivalentObjectProperties(_)
                | C::InverseObjectProperties(_)
                | C::SymmetricObjectProperty(_)
                | C::AsymmetricObjectProperty(_)
                | C::ReflexiveObjectProperty(_)
                | C::IrreflexiveObjectProperty(_)
        ),
        // Each axiom is internal or external to the base namespaces by its
        // subjects (see `axiom_subjects`). With no base IRIs, every axiom is
        // external.
        "internal" => is_axiom(comp) && is_internal(comp, base_iris),
        "external" => is_axiom(comp) && !is_internal(comp, base_iris),
        // A single axiom type, named the way the OWL object model names it.
        // uPheno's `upheno-old-model.owl` asks for ten of these one per step
        // (`--axioms FunctionalObjectProperty`, `--axioms DisjointDataProperties`,
        // …) where the grouping categories above would take too much.
        other => axiom_type_matches(comp, other),
    }
}

/// Does `comp` have the single axiom type spelled `name`?
///
/// The names are the object model's own, which is why `IrrefexiveObjectProperty`
/// is spelled without its second `l` and an annotation-property range is
/// `AnnotationPropertyRangeOf`. They are matched exactly: a near miss is not an
/// axiom type, and [`is_axiom_category`] reports it as one owlmake cannot run.
///
/// `Declaration` is one type covering all six entity kinds. `SubObjectPropertyOf`
/// and `SubPropertyChainOf` are two types over one component: a property chain is
/// the latter and never the former.
fn axiom_type_matches(comp: &horned_owl::model::Component<Rc>, name: &str) -> bool {
    use horned_owl::model::Component as C;
    use horned_owl::model::SubObjectPropertyExpression as SubOPE;
    match name {
        "Declaration" => is_declaration(comp),
        "EquivalentClasses" => matches!(comp, C::EquivalentClasses(_)),
        "SubClassOf" => matches!(comp, C::SubClassOf(_)),
        "DisjointClasses" => matches!(comp, C::DisjointClasses(_)),
        "DisjointUnion" => matches!(comp, C::DisjointUnion(_)),
        "ClassAssertion" => matches!(comp, C::ClassAssertion(_)),
        "SameIndividual" => matches!(comp, C::SameIndividual(_)),
        "DifferentIndividuals" => matches!(comp, C::DifferentIndividuals(_)),
        "ObjectPropertyAssertion" => matches!(comp, C::ObjectPropertyAssertion(_)),
        "NegativeObjectPropertyAssertion" => {
            matches!(comp, C::NegativeObjectPropertyAssertion(_))
        }
        "DataPropertyAssertion" => matches!(comp, C::DataPropertyAssertion(_)),
        "NegativeDataPropertyAssertion" => matches!(comp, C::NegativeDataPropertyAssertion(_)),
        "EquivalentObjectProperties" => matches!(comp, C::EquivalentObjectProperties(_)),
        "SubObjectPropertyOf" => match comp {
            C::SubObjectPropertyOf(ax) => !matches!(ax.sub, SubOPE::ObjectPropertyChain(_)),
            _ => false,
        },
        "SubPropertyChainOf" => match comp {
            C::SubObjectPropertyOf(ax) => matches!(ax.sub, SubOPE::ObjectPropertyChain(_)),
            _ => false,
        },
        "InverseObjectProperties" => matches!(comp, C::InverseObjectProperties(..)),
        "FunctionalObjectProperty" => matches!(comp, C::FunctionalObjectProperty(_)),
        "InverseFunctionalObjectProperty" => {
            matches!(comp, C::InverseFunctionalObjectProperty(_))
        }
        "SymmetricObjectProperty" => matches!(comp, C::SymmetricObjectProperty(_)),
        "AsymmetricObjectProperty" => matches!(comp, C::AsymmetricObjectProperty(_)),
        "TransitiveObjectProperty" => matches!(comp, C::TransitiveObjectProperty(_)),
        "ReflexiveObjectProperty" => matches!(comp, C::ReflexiveObjectProperty(_)),
        "IrrefexiveObjectProperty" => matches!(comp, C::IrreflexiveObjectProperty(_)),
        "ObjectPropertyDomain" => matches!(comp, C::ObjectPropertyDomain { .. }),
        "ObjectPropertyRange" => matches!(comp, C::ObjectPropertyRange { .. }),
        "DisjointObjectProperties" => matches!(comp, C::DisjointObjectProperties(_)),
        "EquivalentDataProperties" => matches!(comp, C::EquivalentDataProperties(_)),
        "SubDataPropertyOf" => matches!(comp, C::SubDataPropertyOf { .. }),
        "FunctionalDataProperty" => matches!(comp, C::FunctionalDataProperty(_)),
        "DataPropertyDomain" => matches!(comp, C::DataPropertyDomain { .. }),
        "DataPropertyRange" => matches!(comp, C::DataPropertyRange { .. }),
        "DisjointDataProperties" => matches!(comp, C::DisjointDataProperties(_)),
        "DatatypeDefinition" => matches!(comp, C::DatatypeDefinition { .. }),
        "HasKey" => matches!(comp, C::HasKey { .. }),
        "Rule" => matches!(comp, C::Rule(_)),
        "AnnotationAssertion" => matches!(comp, C::AnnotationAssertion(_)),
        "SubAnnotationPropertyOf" => matches!(comp, C::SubAnnotationPropertyOf { .. }),
        "AnnotationPropertyRangeOf" => matches!(comp, C::AnnotationPropertyRange { .. }),
        "AnnotationPropertyDomain" => matches!(comp, C::AnnotationPropertyDomain { .. }),
        _ => false,
    }
}

/// Every value `--axioms` accepts: the grouping categories, the two selectors
/// that are namespace tests rather than type tests and `structural-tautologies`,
/// in any case, and each single axiom type by its object-model name.
///
/// One list serves the classifier and the plan's coverage check, so a category
/// cannot be executable but reported as a gap, or the reverse.
pub fn is_axiom_category(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "all" | "logical" | "annotation" | "declaration" | "subclass" | "subproperty"
            | "equivalent" | "disjoint" | "type" | "tbox" | "abox" | "rbox"
            | "internal" | "external" | "structural-tautologies"
    ) || AXIOM_TYPE_NAMES.contains(&name)
}

/// The axiom-type names, exactly as the OWL object model spells them.
const AXIOM_TYPE_NAMES: &[&str] = &[
    "Declaration",
    "EquivalentClasses",
    "SubClassOf",
    "DisjointClasses",
    "DisjointUnion",
    "ClassAssertion",
    "SameIndividual",
    "DifferentIndividuals",
    "ObjectPropertyAssertion",
    "NegativeObjectPropertyAssertion",
    "DataPropertyAssertion",
    "NegativeDataPropertyAssertion",
    "EquivalentObjectProperties",
    "SubObjectPropertyOf",
    "InverseObjectProperties",
    "FunctionalObjectProperty",
    "InverseFunctionalObjectProperty",
    "SymmetricObjectProperty",
    "AsymmetricObjectProperty",
    "TransitiveObjectProperty",
    "ReflexiveObjectProperty",
    "IrrefexiveObjectProperty",
    "ObjectPropertyDomain",
    "ObjectPropertyRange",
    "DisjointObjectProperties",
    "SubPropertyChainOf",
    "EquivalentDataProperties",
    "SubDataPropertyOf",
    "FunctionalDataProperty",
    "DataPropertyDomain",
    "DataPropertyRange",
    "DisjointDataProperties",
    "DatatypeDefinition",
    "HasKey",
    "Rule",
    "AnnotationAssertion",
    "SubAnnotationPropertyOf",
    "AnnotationPropertyRangeOf",
    "AnnotationPropertyDomain",
];

/// Rebuild a model keeping only components for which `keep` returns true,
/// preserving the prefix map.
pub fn retain<F>(model: Model, keep: F) -> Model
where
    F: Fn(&horned_owl::model::Component<horned_owl::model::RcStr>) -> bool,
{
    retain_ac(model, |ac| keep(&ac.component))
}

/// [`retain`] with the whole annotated component in hand — for predicates that
/// must see an axiom's own annotations, which belong to its signature.
pub fn retain_ac<F>(model: Model, keep: F) -> Model
where
    F: Fn(&horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>) -> bool,
{
    use horned_owl::model::MutableOntology;
    use horned_owl::ontology::set::SetOntology;
    let mut ont = SetOntology::new();
    for ac in model.ont.iter() {
        if keep(ac) {
            ont.insert(ac.clone());
        }
    }
    // Preserve document-level metadata that isn't derivable from the axioms — as
    // Model::clone does — so a filtered model still carries e.g. `rdf_prefixes`
    // (the verbatim RDF/XML xmlns the owlrdf writer reproduces). Without this, a
    // `remove`/`filter` hop silently drops the input's prefix map.
    //
    // `carry_meta_from` is the one canonical copier, and copying a hand-kept field
    // list here instead is not equivalent: any field the list omits is silently
    // reverted by the hop. Losing `plain_literals_typed` and the shared-blank-node
    // state a preceding `query --update` established, for instance, un-types the
    // literals again and reorders every subject that mixes a plain and a typed
    // literal.
    let mut out = Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes));
    out.carry_meta_from(&model);
    out
}

#[cfg(test)]
mod namespace_tests {
    use super::*;
    use horned_owl::model::{
        Build, Component, ObjectProperty, ObjectPropertyExpression as OPE, RcStr,
        SubObjectPropertyExpression as SOPE, SubObjectPropertyOf,
    };

    const UPHENO: &str = "http://purl.obolibrary.org/obo/UPHENO_";

    fn chain(members: &[&str], sup: &str) -> Component<RcStr> {
        let b: Build<RcStr> = Build::new();
        let op = |i: &str| OPE::ObjectProperty(ObjectProperty(b.iri(i)));
        Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: SOPE::ObjectPropertyChain(members.iter().map(|m| op(m)).collect()),
            sup: match op(sup) {
                OPE::ObjectProperty(p) => OPE::ObjectProperty(p),
                other => other,
            },
        })
    }

    /// A chain axiom's subjects are its chain members alone; the super-property is
    /// not among them. So a chain built out of foreign properties is external to
    /// `--base-iri …/UPHENO_` however internal its super-property is, and a base
    /// module states none of them.
    #[test]
    fn a_chain_of_foreign_properties_is_external_whatever_its_super_property() {
        let base = vec![UPHENO.to_string()];
        let c = chain(
            &[
                "http://purl.obolibrary.org/obo/BFO_0000051",
                "http://purl.obolibrary.org/obo/RO_0000052",
            ],
            "http://purl.obolibrary.org/obo/UPHENO_0000001",
        );
        assert!(axiom_in_category(&c, "external", &base), "chain members are all foreign, so the axiom is external");
    }

    /// The converse: one internal chain member keeps it, because an axiom is
    /// internal when ANY of its subjects lies in the base namespace.
    #[test]
    fn a_chain_with_one_internal_member_is_kept() {
        let base = vec![UPHENO.to_string()];
        let c = chain(
            &[
                "http://purl.obolibrary.org/obo/UPHENO_0000001",
                "http://purl.obolibrary.org/obo/BFO_0000050",
            ],
            "http://purl.obolibrary.org/obo/UPHENO_0000001",
        );
        assert!(!axiom_in_category(&c, "external", &base), "an internal chain member keeps the axiom");
    }
}
