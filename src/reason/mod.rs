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
