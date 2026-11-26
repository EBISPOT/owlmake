//! Nothing owlmake is built with is chosen at build time.
//!
//! Every dependency is named by one exact version: a crate by `=x.y.z` (and a
//! git dependency by a full commit id), the compiler by `rust-toolchain.toml`,
//! a workflow's action by commit id, its runner by release, a tool it installs
//! by version, an image by digest. These are grep tests over the files that
//! name dependencies; a range, a moving tag or a floating install fails here,
//! so what a build uses changes only when a line in the repository changes.
//!
//! Two things are ranges on purpose and are not read here: what the published
//! Python package asks of its users' environment (`requires-python` and the
//! optional `pandas`/`polars` extras in `crates/owlmake-py/pyproject.toml`),
//! and the dev-dependencies of the crates under `deps/`, which no build of
//! owlmake resolves.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The manifests whose requirements a build of owlmake resolves.
fn manifests() -> Vec<PathBuf> {
    let root = root();
    let mut out = vec![root.join("Cargo.toml")];
    for dir in ["crates", "deps"] {
        for e in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
            let manifest = e.path().join("Cargo.toml");
            if manifest.is_file() {
                out.push(manifest);
            }
        }
    }
    out
}

fn workflows() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root().join(".github/workflows"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml"))
        .collect();
    out.sort();
    assert!(out.len() >= 6, "the workflows were not found");
    out
}

/// A line with its comment removed; `None` for a line that is only a comment.
fn code(line: &str) -> Option<&str> {
    let line = line.split(" #").next().unwrap_or(line).trim_end();
    let trimmed = line.trim_start();
    (!trimmed.is_empty() && !trimmed.starts_with('#')).then_some(line)
}

fn is_exact(requirement: &str) -> bool {
    let Some(version) = requirement.strip_prefix('=') else { return false };
    let core = version.split(['+', '-']).next().unwrap_or(version);
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn has_digest(image: &str) -> bool {
    image.split_once("@sha256:").is_some_and(|(_, d)| {
        let d = d.split_whitespace().next().unwrap_or("");
        d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// The value of `key = "…"` in a line, wherever in the line it is.
fn quoted<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!("{key} = \""))?;
    let rest = &line[at + key.len() + 4..];
    rest.split('"').next()
}

#[test]
fn every_crate_requirement_is_one_version() {
    let mut seen = 0;
    for manifest in manifests() {
        let vendored = manifest.strip_prefix(root()).unwrap().starts_with("deps");
        let mut section = String::new();
        for line in read(&manifest).lines() {
            let Some(line) = code(line) else { continue };
            let line = line.trim();
            if line.starts_with('[') {
                section = line.trim_matches(['[', ']']).to_string();
                continue;
            }
            // `[dependencies]`, `[target.….dependencies]`, `[dependencies.name]` …
            let Some(at) = section.find("dependencies") else { continue };
            if section == "patch.crates-io" || (vendored && section[..at].ends_with("dev-")) {
                continue;
            }
            let named_table = section[at..].contains('.');
            let requirement = if named_table {
                if !line.starts_with("version") {
                    continue;
                }
                quoted(line, "version")
            } else if line.contains('{') {
                if line.contains("path = ") || line.contains("git = ") {
                    quoted(line, "version").or(Some("=0.0.0"))
                } else {
                    quoted(line, "version")
                }
            } else {
                line.split_once(" = \"").map(|(_, v)| v.trim_end_matches('"'))
            };
            let Some(requirement) = requirement else {
                panic!("{}: no version in `{line}`", manifest.display())
            };
            assert!(
                is_exact(requirement),
                "{} [{section}]: `{line}` is not one exact version (=x.y.z)",
                manifest.display()
            );
            seen += 1;
        }
    }
    assert!(seen > 60, "the requirements were not found ({seen})");
}

#[test]
fn every_git_dependency_is_a_full_commit() {
    let mut seen = 0;
    for manifest in manifests() {
        for line in read(&manifest).lines() {
            let Some(line) = code(line) else { continue };
            if !line.contains("git = \"") {
                continue;
            }
            let rev = quoted(line, "rev").unwrap_or("");
            assert!(is_commit(rev), "{}: `{}` names no full commit", manifest.display(), line.trim());
            seen += 1;
        }
    }
    assert!(seen >= 3);
}

/// The compiler `rust-toolchain.toml` names, which every workflow asks for by
/// the same number.
fn compiler() -> String {
    let file = read(&root().join("rust-toolchain.toml"));
    let channel = file.lines().find_map(|l| quoted(l, "channel")).expect("a channel").to_string();
    let parts: Vec<&str> = channel.split('.').collect();
    assert!(
        parts.len() == 3 && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())),
        "rust-toolchain.toml: `{channel}` is not one compiler version"
    );
    channel
}

#[test]
fn workflows_name_what_they_use_exactly() {
    let compiler = compiler();
    for workflow in workflows() {
        let name = workflow.file_name().unwrap().to_string_lossy().to_string();
        for line in read(&workflow).lines() {
            let Some(line) = code(line) else { continue };
            let t = line.trim().trim_start_matches("- ");
            if let Some(action) = t.strip_prefix("uses: ") {
                let commit = action.rsplit('@').next().unwrap_or("");
                assert!(is_commit(commit), "{name}: `{action}` is not at a commit id");
            }
            for key in ["runs-on: ", "runner: "] {
                if let Some(at) = t.find(key) {
                    let runner = t[at + key.len()..].split([',', ' ', '}']).next().unwrap_or("");
                    assert!(
                        runner.starts_with("${{") || runner.bytes().any(|b| b.is_ascii_digit()),
                        "{name}: runner `{runner}` names no release"
                    );
                    assert!(!runner.ends_with("-latest"), "{name}: runner `{runner}` moves");
                }
            }
            for key in ["toolchain: ", "rust-toolchain: "] {
                if let Some(version) = t.strip_prefix(key) {
                    assert_eq!(version, compiler, "{name}: another compiler than rust-toolchain.toml's");
                }
            }
            for key in ["node-version: ", "python-version: "] {
                if let Some(version) = t.strip_prefix(key) {
                    let version = version.trim_matches('\'');
                    assert_eq!(version.split('.').count(), 3, "{name}: `{t}` is not one version");
                }
            }
            if let Some(tool) = t.strip_prefix("tool: ") {
                assert!(tool.contains('@'), "{name}: tool `{tool}` has no version");
            }
            if t.contains("pip install") && !t.contains(" -r ") && !t.contains("--no-index") {
                let packages: Vec<&str> =
                    t.split("pip install").nth(1).unwrap().split_whitespace().collect();
                assert!(
                    !packages.is_empty() && packages.iter().all(|p| p.contains("==")),
                    "{name}: `{t}` installs a package without a version"
                );
            }
            assert!(!t.contains("npm install"), "{name}: `{t}` resolves packages; use `npm ci`");
            for key in ["image: ", "container: ", "driver-opts: image="] {
                if let Some(image) = t.strip_prefix(key) {
                    assert!(
                        image.starts_with("${{") || has_digest(image),
                        "{name}: image `{image}` is not named by digest"
                    );
                }
            }
        }
    }
}

/// A build never rewrites `Cargo.lock`: every command that resolves crates is
/// given `--locked`, so a lock that no longer fits the manifests stops the build.
#[test]
fn builds_take_the_lock_as_it_is() {
    const BUILDS: [&str; 7] = [
        "cargo test",
        "cargo build",
        "cargo clippy",
        "cargo doc",
        "cargo zigbuild",
        "wasm-pack build",
        "maturin build",
    ];
    let mut files = workflows();
    files.push(root().join("Dockerfile"));
    let mut seen = 0;
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        for line in read(&file).lines() {
            let Some(line) = code(line) else { continue };
            let t = line.trim().trim_start_matches("- ");
            if t.starts_with("name: ") {
                continue;
            }
            let builds = BUILDS.iter().any(|b| t.contains(b)) || t.starts_with("args: --release");
            if builds {
                assert!(t.contains("--locked"), "{name}: `{t}` may rewrite Cargo.lock");
                seen += 1;
            }
        }
    }
    assert!(seen >= 12, "the build commands were not found ({seen})");
}

#[test]
fn installs_from_requirement_files_are_exact() {
    let dir = root().join(".github/requirements");
    let mut seen = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        for line in read(&e.path()).lines() {
            let Some(line) = code(line) else { continue };
            assert!(line.contains("=="), "{}: `{line}` has no version", e.path().display());
            seen += 1;
        }
    }
    assert!(seen > 0);
    let python = read(&root().join("crates/owlmake-py/pyproject.toml"));
    let requires = python.lines().find(|l| l.starts_with("requires = ")).expect("build requirements");
    assert!(requires.contains("==") && !requires.contains(">="), "pyproject.toml: `{requires}`");
    assert!(root().join(".github/typedoc/package-lock.json").is_file());
}

#[test]
fn images_are_named_by_digest() {
    let dockerfile = read(&root().join("Dockerfile"));
    let mut stages: Vec<String> = Vec::new();
    let mut seen = 0;
    for line in dockerfile.lines() {
        if let Some(syntax) = line.strip_prefix("# syntax=") {
            assert!(has_digest(syntax), "Dockerfile: syntax `{syntax}` is not named by digest");
            seen += 1;
        }
        let Some(rest) = line.strip_prefix("FROM ") else { continue };
        let words: Vec<&str> = rest.split_whitespace().filter(|w| !w.starts_with("--")).collect();
        let image = words[0];
        if !stages.iter().any(|s| s == image) {
            assert!(has_digest(image), "Dockerfile: `{image}` is not named by digest");
            seen += 1;
        }
        if let Some(at) = words.iter().position(|w| w.eq_ignore_ascii_case("as")) {
            stages.push(words[at + 1].to_string());
        }
    }
    assert!(seen >= 3, "the images were not found");
}
