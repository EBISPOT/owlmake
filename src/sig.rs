//! Entity-signature extraction over ontology components.
//!
//! The *signature* of a component is the set of named logical entities
//! (classes, object/data properties, named individuals, datatypes) it
//! mentions. Annotation properties and annotation-value IRIs are deliberately
//! excluded, since they carry no logical content — this is the signature
//! `filter`, `remove` and `extract` select on.

use std::collections::HashSet;

use horned_owl::model::{
    Annotation, AnnotationProperty, Class, Component, DataProperty, Datatype, NamedIndividual,
    ObjectProperty,
    RcStr, IRI,
};
use horned_owl::visitor::immutable::{Visit, Walk};

#[derive(Default)]
struct SigExtract {
    iris: Vec<String>,
}

impl Visit<RcStr> for SigExtract {
    fn visit_class(&mut self, c: &Class<RcStr>) {
        self.iris.push(c.0.as_ref().to_string());
    }
    fn visit_object_property(&mut self, p: &ObjectProperty<RcStr>) {
        self.iris.push(p.0.as_ref().to_string());
    }
    fn visit_data_property(&mut self, p: &DataProperty<RcStr>) {
        self.iris.push(p.0.as_ref().to_string());
    }
    fn visit_named_individual(&mut self, i: &NamedIndividual<RcStr>) {
        self.iris.push(i.0.as_ref().to_string());
    }
    fn visit_datatype(&mut self, d: &Datatype<RcStr>) {
        self.iris.push(d.0.as_ref().to_string());
    }
}

/// The logical-entity signature of a single component.
pub fn signature(comp: &Component<RcStr>) -> HashSet<String> {
    let mut walk = Walk::new(SigExtract::default());
    walk.component(comp);
    walk.into_visit().iris.into_iter().collect()
}

/// The OWL 2 entity kinds, as bit flags.
///
/// A signature is a set of *typed* entities, not of IRIs. An IRI punned as both
/// a class and an individual is TWO entities, so a locality test on one says
/// nothing about the other — GSSO puns hundreds of MESH terms exactly this way,
/// and treating the signature as a set of IRIs would pull every punned term's
/// class hierarchy into its BOT module.
pub mod kind {
    pub const CLASS: u8 = 1;
    pub const OBJECT_PROPERTY: u8 = 2;
    pub const DATA_PROPERTY: u8 = 4;
    pub const NAMED_INDIVIDUAL: u8 = 8;
    pub const DATATYPE: u8 = 16;
    pub const ANNOTATION_PROPERTY: u8 = 32;
}

#[derive(Default)]
struct TypedSig {
    entities: Vec<(u8, String)>,
}

impl Visit<RcStr> for TypedSig {
    fn visit_class(&mut self, c: &Class<RcStr>) {
        self.entities.push((kind::CLASS, c.0.as_ref().to_string()));
    }
    fn visit_object_property(&mut self, p: &ObjectProperty<RcStr>) {
        self.entities.push((kind::OBJECT_PROPERTY, p.0.as_ref().to_string()));
    }
    fn visit_data_property(&mut self, p: &DataProperty<RcStr>) {
        self.entities.push((kind::DATA_PROPERTY, p.0.as_ref().to_string()));
    }
    fn visit_named_individual(&mut self, i: &NamedIndividual<RcStr>) {
        self.entities.push((kind::NAMED_INDIVIDUAL, i.0.as_ref().to_string()));
    }
    fn visit_datatype(&mut self, d: &Datatype<RcStr>) {
        self.entities.push((kind::DATATYPE, d.0.as_ref().to_string()));
    }
    fn visit_annotation_property(&mut self, p: &AnnotationProperty<RcStr>) {
        self.entities.push((kind::ANNOTATION_PROPERTY, p.0.as_ref().to_string()));
    }
}

/// The typed signature of a single component — `(kind, IRI)` pairs.
pub fn typed_signature(comp: &Component<RcStr>) -> Vec<(u8, String)> {
    let mut walk = Walk::new(TypedSig::default());
    walk.component(comp);
    walk.into_visit().entities
}

/// Every entity IRI a class expression mentions. When a SubClassOf's subclass is
/// ANONYMOUS there is no single subject, so its whole signature stands in for
/// one, and the axiom is internal if any of those is in the base namespace — a
/// bare `ObjectSomeValuesFrom(P, C)` is about both P and C.
pub fn class_expression_signature(ce: &horned_owl::model::ClassExpression<RcStr>) -> HashSet<String> {
    let mut walk = Walk::new(SigAll::default());
    walk.class_expression(ce);
    walk.into_visit().iris.into_iter().collect()
}

/// Every IRI an axiom ANNOTATION mentions — its property, plus an IRI value or
/// any nested annotation's. These fold into an axiom's signature, so
/// `remove --term <ap>` takes the annotated axiom with it.
pub fn annotation_iris(a: &Annotation<RcStr>) -> HashSet<String> {
    let mut walk = Walk::new(SigAll::default());
    walk.annotation(a);
    walk.into_visit().iris.into_iter().collect()
}

/// The annotation's own PROPERTY IRI, leaving its VALUE out. An IRI value is not
/// an entity of the axiom — OWL's signature does not carry it — so an axiom
/// annotated `oboInOwl:evidence <…/ncbigene/11668>` is not an axiom about that
/// gene, and a pass removing the gene must leave the axiom alone.
pub fn annotation_property_iri(a: &Annotation<RcStr>) -> String {
    a.ap.0.as_ref().to_string()
}

#[derive(Default)]
struct SigAll {
    iris: Vec<String>,
}

impl Visit<RcStr> for SigAll {
    fn visit_iri(&mut self, i: &horned_owl::model::IRI<RcStr>) {
        self.iris.push(i.to_string());
    }
}

/// Every annotation property IRI mentioned by `comp` (predicate position of an
/// assertion, an axiom annotation, or a nested annotation).
pub fn annotation_properties(comp: &Component<RcStr>) -> HashSet<String> {
    #[derive(Default)]
    struct V {
        iris: Vec<String>,
    }
    impl Visit<RcStr> for V {
        fn visit_annotation_property(&mut self, p: &AnnotationProperty<RcStr>) {
            self.iris.push(p.0.as_ref().to_string());
        }
    }
    let mut walk = Walk::new(V::default());
    walk.component(comp);
    walk.into_visit().iris.into_iter().collect()
}

/// Whether `comp`'s signature intersects `terms`.
pub fn mentions_any(comp: &Component<RcStr>, terms: &HashSet<String>) -> bool {
    signature(comp).iter().any(|t| terms.contains(t))
}

#[derive(Default)]
struct AllIris {
    iris: Vec<String>,
}

impl Visit<RcStr> for AllIris {
    fn visit_iri(&mut self, i: &IRI<RcStr>) {
        self.iris.push(i.as_ref().to_string());
    }
    // Annotation properties are used in *predicate* position (an assertion's
    // property, an axiom annotation's property) and so are not reached by
    // `visit_iri` on their own; collect them explicitly. The srcmerged model
    // declares every used annotation property, so `terms.sparql` seeds it —
    // this keeps e.g. `oboInOwl:source` (used on edit xrefs) out of the import
    // annotation-property strip, while an import-only property (`notes`, `order`)
    // never used in the edit/components is not seeded and is stripped.
    fn visit_annotation_property(&mut self, p: &AnnotationProperty<RcStr>) {
        self.iris.push(p.0.as_ref().to_string());
    }
}

/// Every IRI `comp` references — the logical signature *plus* annotation-value
/// IRIs (xref / mapping targets), annotation properties and annotation
/// subjects. This is the set the import `pre_seed` SPARQL (`terms.sparql`)
/// collects: any IRI appearing as the subject or object of a triple. Used only
/// for seeding the import ⊥-module, where an xref/mapping target like a UBERON
/// or brain-atlas class must be pulled in even though it is referenced purely as
/// an annotation value.
pub fn all_iris(comp: &Component<RcStr>) -> HashSet<String> {
    let mut walk = Walk::new(AllIris::default());
    walk.component(comp);
    walk.into_visit().iris.into_iter().collect()
}
