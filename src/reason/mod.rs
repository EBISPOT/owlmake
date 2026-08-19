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
pub mod entail;
pub mod whelk;
pub mod whelk_order;

pub use dl::DlReasoner;
pub use el::Reasoner;
pub use entail::{entails, instances, is_instance, types};
pub use whelk::WhelkClassification;

/// Configure the shared EL engine for a `--reasoner` choice, for the commands
/// that run on the EL [`Reasoner`] (`reduce`, `materialize`, `explain`).
/// `owlmake` turns on union-elimination; the other EL names (`elk`/`structural`/
/// `emr`) use plain EL; the non-EL names (`hermit`/`jfact`/`whelk`) fall back to
/// the EL engine for these operations with a one-line note (rather than being
/// silently ignored). Must be called before `Reasoner::classify`, since the mode
/// is process-global. Returns whether union-elimination was enabled.
pub fn configure(reasoner: &str) -> bool {
    let lc = reasoner.to_ascii_lowercase();
    let union_elim = lc == "owlmake";
    el::set_whelk_mode(union_elim);
    match lc.as_str() {
        "elk" | "structural" | "emr" => {}
        "owlmake" => status!("reason: using the built-in EL reasoner with union-elimination"),
        "whelk" | "hermit" | "jfact" => status!(
            "note: reasoner '{reasoner}' is not available for this operation; using the built-in EL reasoner"
        ),
        _ => status!("note: unknown reasoner '{reasoner}'; using the built-in EL reasoner"),
    }
    union_elim
}
