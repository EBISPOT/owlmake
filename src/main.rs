//! owlmake — a single self-contained Rust binary that builds ontologies end to
//! end, OWL reasoning included, with no Docker, Java, or Python runtime
//! dependency.
//!
//! This binary is a thin shell: the command-line surface (the `Cli`/`Command`
//! clap tree) and all dispatch live in the library's `owlmake::cli` module, so
//! the Python extension can run the exact same commands in-process. `main` only
//! wires the global allocator and hands argv to `cli::run_argv_main`.

/// Thread-caching global allocator — see the `mimalloc` note in Cargo.toml. The
/// parallel DL satisfiability pass is allocation-bound across all cores, where
/// the system allocator's shared arena serializes; mimalloc's per-thread heaps
/// remove that bottleneck. (Non-wasm only — mimalloc bundles C with no wasm
/// target; wasm uses the default allocator.)
#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    // The large-stack worker, command interception, and command chaining all
    // live in `owlmake::cli::run_argv_main`; the binary just supplies argv and
    // propagates the exit code.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(owlmake::cli::run_argv_main(argv));
}
