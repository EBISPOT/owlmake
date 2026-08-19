//! `oort` — the one release-variant primitive owlmake's other ops do not already
//! cover: the `--simple` artefact's "core class subset".
//!
//! The variants themselves decompose into owlmake's existing ops (the plan-level
//! `rewrite_oort` pass emits `merge → relax → reason → reduce [→ remove --axioms
//! equivalent] [→ simple-subset] → annotate`):
//!
//!  * **main** (`<id>.owl`): relax equivalence definitions into existential
//!    `SubClassOf`, assert inferred direct named superclasses (`owl:Thing`
//!    skipped), drop redundant named subclass axioms. Equivalence axioms kept.
//!    → `relax → reason → reduce`.
//!  * **relaxed** (`<id>-relaxed.owl`): main, then `remove --axioms equivalent`.
//!  * **simple** (`<id>-simple.owl`, the artefact older releases name "basic"):
//!    relaxed, then this module's [`simple_subset`].
//!
//! [`simple_subset`] keeps only classes in the ontology's own OBO ID-space,
//! dropping every logical axiom that references an external/imported class and
//! every annotation assertion *about* one. Object/annotation properties (e.g.
//! `BFO_0000050` part-of), individuals, datatypes, ontology annotations and
//! annotation cross-references *to* external terms are retained: the subset
//! restricts which classes the artefact carries axioms about, and a term standing
//! in annotation-value position is not one of them.

use std::collections::HashSet;

use horned_owl::model::{AnnotationSubject, Class, Component, RcStr};
use horned_owl::visitor::immutable::{Visit, Walk};

use crate::model::Model;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Main,
    Relaxed,
    Simple,
}

impl Variant {
    /// Classify a release-variant output target by its filename suffix
    /// (`<id>-simple.owl` → Simple, `<id>-relaxed.*` → Relaxed, otherwise the
    /// main ontology).
    pub fn classify(target: &str) -> Variant {
        let stem = std::path::Path::new(target)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(target);
        if stem.ends_with("-simple") {
            Variant::Simple
        } else if stem.ends_with("-relaxed") {
            Variant::Relaxed
        } else {
            Variant::Main
        }
    }
}

/// Keep only classes in the ontology's own OBO ID-space (`ont_id`), dropping every
/// axiom whose logical class signature includes an external (imported/upper-
/// ontology) class, plus the annotation assertions *about* such classes.
/// Object/annotation properties, individuals, datatypes, ontology annotations and
/// annotation cross-references whose *value* is an external term are retained: the
/// removal is class-level, so a term standing in annotation-value position never
/// makes its axiom external.
pub fn simple_subset(mut model: Model, ont_id: &str) -> Model {
    use horned_owl::ontology::set::SetOntology;

    // Non-native classes appearing anywhere in a logical class position.
    let external: HashSet<String> = model
        .ont
        .iter()
        .flat_map(|ac| class_iris(&ac.component))
        .filter(|iri| !is_native_class(iri, ont_id))
        .collect();

    let kept: SetOntology<RcStr> = model
        .ont
        .iter()
        .filter(|ac| {
            // Drop any axiom referencing an external class in a logical position.
            if !class_iris(&ac.component).is_disjoint(&external) {
                return false;
            }
            // Drop annotation assertions *about* an external class.
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if let AnnotationSubject::IRI(iri) = &aa.subject {
                    if external.contains(iri.as_ref()) {
                        return false;
                    }
                }
            }
            true
        })
        .cloned()
        .collect();
    model.ont = kept;
    model
}

/// The named classes a component references (object/annotation properties,
/// individuals and datatypes are ignored — only class membership decides native).
fn class_iris(comp: &Component<RcStr>) -> HashSet<String> {
    #[derive(Default)]
    struct ClassExtract {
        iris: HashSet<String>,
    }
    impl Visit<RcStr> for ClassExtract {
        fn visit_class(&mut self, c: &Class<RcStr>) {
            self.iris.insert(c.0.as_ref().to_string());
        }
    }
    let mut walk = Walk::new(ClassExtract::default());
    walk.component(comp);
    walk.into_visit().iris
}

/// Whether a class IRI is native to the `ont_id` ontology: an OBO PURL whose
/// ID-space (the part before `_`), lower-cased, equals `ont_id`. `owl:Thing`/
/// `owl:Nothing` are always native (structural).
fn is_native_class(iri: &str, ont_id: &str) -> bool {
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    if iri == format!("{OWL}Thing") || iri == format!("{OWL}Nothing") {
        return true;
    }
    if let Some(local) = iri.strip_prefix("http://purl.obolibrary.org/obo/") {
        if let Some((prefix, _)) = local.split_once('_') {
            return prefix.eq_ignore_ascii_case(ont_id);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_classification() {
        assert!(is_native_class("http://purl.obolibrary.org/obo/WBbt_0000001", "wbbt"));
        assert!(!is_native_class("http://purl.obolibrary.org/obo/IAO_0000115", "wbbt"));
        assert!(!is_native_class("http://purl.obolibrary.org/obo/BFO_0000050", "wbbt"));
        assert!(is_native_class("http://www.w3.org/2002/07/owl#Thing", "wbbt"));
    }

    #[test]
    fn variant_classification() {
        assert_eq!(Variant::classify("wbbt-simple.owl"), Variant::Simple);
        assert_eq!(Variant::classify("wbbt-relaxed.owl"), Variant::Relaxed);
        assert_eq!(Variant::classify("wbbt.owl"), Variant::Main);
        assert_eq!(Variant::classify("wbbt-simple.obo"), Variant::Simple);
    }
}
