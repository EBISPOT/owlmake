//! owlmake — a self-contained OWL/OBO toolkit: ontology builds, conversion, QC
//! and OWL reasoning in a single Rust binary, with no Docker, Java, or Python
//! runtime dependency.
//!
//! The crate is split into a library (this module tree) holding all the
//! ontology logic, and a thin binary (`src/main.rs`) that wires the CLI.

/// Print a status line to stderr, decorated with a `[HH:MM:SS]` timestamp and a
/// colourised `label:` prefix (see [`progress::status_emit`]). Drop-in for
/// `eprintln!` for owlmake's own diagnostics; the `sssom`, `jq` and `dosdp` CLIs
/// keep plain `eprintln!`, so their diagnostics carry no timestamp or label
/// prefix.
#[macro_export]
macro_rules! status {
    ($($arg:tt)*) => {
        $crate::progress::status_emit(::std::format_args!($($arg)*))
    };
}

/// `Instant`/`SystemTime`/`UNIX_EPOCH` that also work on wasm (where the std
/// clock would trap), via `web-time`; plain `std::time` everywhere else. Use
/// these instead of `std::time::{Instant, SystemTime}` so timing and timestamps
/// work in the wasm build.
pub mod time {
    #[cfg(target_arch = "wasm32")]
    pub use web_time::{Instant, SystemTime, UNIX_EPOCH};
    #[cfg(not(target_arch = "wasm32"))]
    pub use std::time::{Instant, SystemTime, UNIX_EPOCH};
}

/// The stable function API every frontend (CLI, Python, JS) wraps.
pub mod api;
/// The bundled SPARQL CLI (`om arq`): a query evaluated over RDF files.
pub mod arq;
/// Execute a plan. The build itself, independent of where the plan came from.
pub mod build;
/// The bundled `jq` engine.
pub mod jq;
/// The build plan: what owlmake executes.
pub mod plan;
/// `owlmake.yaml`/`owlmake.json` — the on-disk plan, the build's contract.
pub mod spec;
pub mod table;
pub mod cmd;
// The command-chaining CLI dispatch — the `Cli`/`Command` clap tree and
// `run_argv`, the in-process entry point both the binary (`src/main.rs`) and
// the Python extension call. Native-only: it reaches the embedded text
// utilities and network commands that are excluded from the wasm core.
#[cfg(not(target_arch = "wasm32"))]
pub mod cli;
pub mod diff;
pub mod dosdp;
pub mod extract;
pub mod hash_trie;
/// OBO ID-policy files (`<ont>-idranges.owl`) — parsing and checking, behind
/// `om validate-id-ranges`.
pub mod idpolicy;
pub mod io;
pub mod model;
pub mod odk;
// OWLAPI-compatible content hashes and set iteration order. Pure std +
// object model, so it builds everywhere the model does, wasm included.
pub mod owlapi_hash;
pub mod profile;
pub mod progress;
pub mod reason;
pub mod report;
pub mod semsql;
pub mod sig;
pub mod sssom;
pub mod sparql;
// Dictionary recognition of ontology terms in free text (Aho-Corasick + flat DB).
// Pure std + serde, so it builds for wasm too — the automaton, DB read/write, TSV
// build, and tagging all run in the browser. (The `text-tagger` *command* that does
// file/stdin I/O + gzip stays native; this is just the reusable library.)
pub mod tag;
pub mod ubergraph;
// The embedded text-utility CLIs (sed/comm/grep) are a binary-only concern and
// pull unix-only crates; excluded from the wasm core.
#[cfg(not(target_arch = "wasm32"))]
pub mod util;
