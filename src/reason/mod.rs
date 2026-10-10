//! OWL reasoning.
//!
//! The default reasoner is an OWL 2 EL reasoner: EL is the profile CL, UBERON
//! and MONDO are written in, and classification in it stays tractable at their
//! size. Full OWL 2 DL (`--reasoner hermit`/`jfact`) is served by
//! [`DlReasoner`], an adapter over the hermit-rs crate; `--reasoner whelk` by
//! the whelk-rs EL reasoner.

// Both external reasoner adapters build for wasm: hermit-rs (`dl`) and whelk-rs
// (`whelk`) each request horned-owl without `remote` (its ureq/rustls import
// resolver), classify without threads, and use a wasm-safe clock, so full OWL 2
// DL (`hermit`/`jfact`) and the whelk-rs EL reasoner are both available in the
// browser, alongside the built-in EL engine.
pub mod dl;
pub mod el;
pub mod elk_order;
pub mod entail;
pub mod whelk;
pub mod whelk_order;

pub use dl::DlReasoner;
pub use el::Reasoner;
pub use entail::{entails, instances, is_instance, types};
pub use whelk::WhelkClassification;

use crate::model::{Model, Onto};
use horned_owl::model::{self as m, AnnotatedComponent, ClassExpression, Component, RcStr};
use std::borrow::Cow;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// `c` as OWL's object model holds it, where a reasoner reads the two
/// differently; `None` where they agree.
///
/// The operands of a disjointness or a difference are a set, so an operand
/// stated twice counts once: `DifferentIndividuals(:j :j)` says nothing, and
/// `DisjointUnion(:U :B :B)` leaves `:B` satisfiable. A disjointness of a
/// single class `C` — `DisjointClasses(:C :C)`, or `:C owl:disjointWith :C` —
/// is `C`'s disjointness from `owl:Thing`, so `C` is empty.
pub(crate) fn as_owl_axiom(c: &Component<RcStr>) -> Option<Component<RcStr>> {
    let is = |ce: &ClassExpression<RcStr>, iri: &str| matches!(ce, ClassExpression::Class(k) if &*k.0 == iri);
    let set = || crate::io::canonical_component(c);
    match c {
        Component::DisjointClasses(ax) => {
            let Some(Component::DisjointClasses(set)) = set() else { return None };
            match set.0.as_slice() {
                [only] if !is(only, OWL_THING) && !is(only, OWL_NOTHING) => {
                    let thing = ClassExpression::Class(m::Build::new_rc().class(OWL_THING));
                    Some(Component::DisjointClasses(m::DisjointClasses(vec![only.clone(), thing])))
                }
                members if members.len() < ax.0.len() => Some(Component::DisjointClasses(set)),
                _ => None,
            }
        }
        Component::DisjointUnion(ax) => set().filter(|s| matches!(s, Component::DisjointUnion(u) if u.1.len() < ax.1.len())),
        Component::DisjointObjectProperties(ax) => {
            set().filter(|s| matches!(s, Component::DisjointObjectProperties(d) if d.0.len() < ax.0.len()))
        }
        Component::DisjointDataProperties(ax) => {
            set().filter(|s| matches!(s, Component::DisjointDataProperties(d) if d.0.len() < ax.0.len()))
        }
        Component::DifferentIndividuals(ax) => {
            set().filter(|s| matches!(s, Component::DifferentIndividuals(d) if d.0.len() < ax.0.len()))
        }
        _ => None,
    }
}

/// `model`'s axioms as every reasoner reads them: each one [`as_owl_axiom`]
/// changes, changed. Borrowed when it changes none.
pub(crate) fn owl_axioms(model: &Model) -> Cow<'_, Onto> {
    if !model.ont.iter().any(|ac| as_owl_axiom(&ac.component).is_some()) {
        return Cow::Borrowed(&model.ont);
    }
    Cow::Owned(
        model
            .ont
            .iter()
            .map(|ac| match as_owl_axiom(&ac.component) {
                Some(component) => AnnotatedComponent { component, ann: ac.ann.clone() },
                None => ac.clone(),
            })
            .collect(),
    )
}
