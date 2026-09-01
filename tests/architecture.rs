//! Architectural invariants that no type can enforce.
//!
//! These are grep tests, and that is deliberate: each guards something the
//! compiler cannot see. Adding a name to one of these lists is a decision, and
//! the comment beside it must say which category it falls into.

use std::path::Path;

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn src_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// **The plan-flow boundary.** `src/odk/` is the only code that may read a
/// `Makefile` or an `<id>-odk.yaml`; it resolves what it finds and writes the
/// consequence into the plan. Everything downstream reads the plan and nothing
/// else — because the whole purpose of owlmake is that a repo can delete both
/// files and still build.
///
/// The type system carries most of this (`crate::build::Repo` holds directories
/// and a `&Plan` and cannot reach an `OdkRepo`), but a string literal cannot be
/// typed, so a new `"Makefile"` or `"-odk.yaml"` outside ingest is caught here.
#[test]
fn only_ingest_names_the_makefile_or_the_odk_yaml() {
    let src = src_root();
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut offenders: Vec<String> = Vec::new();
    for f in files {
        let rel = f.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        // Ingest owns these names, and this test is allowed to spell them.
        if rel.starts_with("odk/") || rel == "odk.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for (n, line) in text.lines().enumerate() {
            // Comments explain WHY a rule exists and legitimately mention both
            // filenames; only code is a violation.
            let code = line.split("//").next().unwrap_or("");
            if code.contains("\"Makefile\"") || code.contains("-odk.yaml") {
                offenders.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the Makefile / ODK yaml may only be named inside src/odk/ (ingest). \
         If execution needs to know something, add a plan field — do not reach for the repo.\n{}",
        offenders.join("\n")
    );
}

/// **Determinism.** Same plan plus same run inputs must give the same bytes.
/// An environment variable that changes what a build PRODUCES is therefore a
/// plan field, not an ambient input. Diagnostics — progress, colour, timing,
/// tracing — are fine, and are listed here.
///
/// `std::env::var` is reachable from anywhere and always will be, which is why
/// this is a test rather than a type.
#[test]
fn no_new_environment_variables_decide_output() {
    // Diagnostic only: they change what is PRINTED, never what is written.
    const ALLOWED: &[&str] = &[
        // Banner-label resolution tracing: the input document's identity at
        // write time and the consultation order of the closure's documents.
        // Diagnostic only — stderr, no output bytes depend on it.
        "OM_BANNER_DEBUG",
        // genid/reification tracing
        "OM_ANON_DEBUG",
        "OM_GENID_DEBUG",
        "OM_GENID_DUPLOG",
        "OM_GENID_ENTITYSTART",
        // Writes the (owner, signature) of each blank-node reuse request that
        // missed while the entity had already allocated a node for that
        // structure. It is what turns a counter drift into a named construct —
        // OBA's nested conjunctions were 5,677 of these.
        "OM_GENID_MISSLOG",
        "OM_GENID_REIF",
        "OM_GENID_REUSED",
        // Counter value at the start of each owning entity — per-entity spend,
        // which localises a drift to the entity that over- or under-allocates.
        "OM_GENID_STARTS",
        // shared_anon population after relax records derived-super sharing.
        "OM_RELAX_DEBUG",
        // shared_anon arity entering each pipeline op — names the op that drops
        // carried document state.
        "OM_PIPE_DEBUG",
        "OM_GENID_REUSELOG",
        "OM_GENID_STARTS",
        "OM_GENID_TRACE",
        "OM_IMPORT_DEBUG",
        "OM_LABEL_DEBUG",
        "OM_MODEL_DEBUG",
        // Prints the hash, bucket and table size behind each `property_value:`
        // clause's place in its frame. Two clauses that differ only in a
        // qualifier tie in the clause comparison and fall to the axiom set's own
        // order, and this is the only way to see which one the model puts first.
        "OM_PV_DEBUG",
        "OM_REIF_DUMP",
        // Traces why `apply_jena_scan_order` declined a query — no `?v a <T>` to
        // drive, an unbound column, no document order for the type. Every use is
        // an `eprintln!`; the ordering decision is the same either way.
        "OM_SCAN_DEBUG",
        "OM_PLAN_DEBUG",
        // progress / resource shaping
        "OWLMAKE_PROGRESS",
        "OWLMAKE_TIMING",
        "OWLMAKE_ANALYZE",
        "OWLMAKE_MEM_FLOOR_GIB",
        "OWLMAKE_WORKERS",
        "OWLMAKE_TIMESTAMPS",
        "OWLMAKE_STACK_GIB",
        "COLUMNS",
        // span/axiom tracing in remove/filter
        "OM_SPAN_LOG",
        // Traces which classes each subset closure round pulls in, and the
        // axiom that pulled them, to stderr. The closure itself is unchanged.
        "OM_DEBUG_SUBSET",
        // Prints the evaluation plan a SPARQL query compiles to, then drops the
        // solutions unread. It reports how a query WILL be run, and changes
        // neither the query nor its results.
        "OWLMAKE_EXPLAIN_SPARQL",
        // Writes the computed extraction seed to a path the caller names, for
        // inspection. It dumps the seed and returns it unchanged, so it cannot
        // alter what the build produces.
        "OM_DUMP_SEED",
        // Saves a COPY of the piped model after each step of a matching target,
        // under the directory OM_DUMP_DIR names, so a pipeline can be inspected
        // step by step. The model that continues down the pipeline is the
        // original, and the dumps land outside the build, so neither switch can
        // reach what the build produces.
        "OM_DUMP_STEPS",
        "OM_DUMP_DIR",
        "NO_COLOR",
        "OWLMAKE_COLOR",
        "TERM",
        // process environment owlmake passes THROUGH rather than reads
        "PATH",
        "HOME",
        "TMPDIR",
        "ROBOT_PLUGINS_DIRECTORY",
    ];

    let src = src_root();
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut offenders: Vec<String> = Vec::new();
    for f in files {
        let rel = f.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        // `om jq` implements the jq language, which exposes `$ENV`; a user-typed
        // `om jq` sees the user's environment by design. The build path seals it
        // instead — see `crate::build::recipe`.
        if rel == "jq.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for marker in ["env::var(\"", "env::var_os(\""] {
                let Some(at) = code.find(marker) else { continue };
                let rest = &code[at + marker.len()..];
                let Some(end) = rest.find('"') else { continue };
                let name = &rest[..end];
                if !ALLOWED.contains(&name) {
                    offenders.push(format!("{rel}:{}: {name}", n + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "an environment variable that can change what a build produces must be a plan field. \
         If this one is purely diagnostic, add it to ALLOWED \
         with a comment saying so.\n{}",
        offenders.join("\n")
    );
}

/// **Honesty.** A QC check that cannot run must fail, never pass quietly. QC
/// targets are ordinary plan targets: their recipes are recorded like any other
/// and run through the same path, so a check that cannot run is a step that
/// fails. A private QC module understands only ODK's variable spellings, so a
/// repo that deviates falls off its edge — and an unconfigurable check there
/// becomes a skip, letting a repo print "all checks passed" having run none of
/// them. This test keeps that module out of the tree.
#[test]
fn there_is_no_second_qc_implementation() {
    let qc = src_root().join("odk/qc.rs");
    assert!(
        !qc.exists(),
        "src/odk/qc.rs is back. QC targets are plan targets: their recipes are recorded like any \
         other and run through the same path. A private reimplementation understands only ODK's \
         variable spellings, so any repo that deviates falls off its edge silently."
    );
}

/// **A plan variable is resolved in ONE place.** A variable the plan records is
/// read by execution to decide where a file goes; resolve it twice and the two
/// readings drift, so half the build writes to the plan's directory and half to
/// the default.
///
/// `TMPDIR` is the case this guards. It names the scratch directory the DOSDP
/// engine writes `all_pattern_terms.txt` and the per-pattern seed files into, and
/// the import-seed rules read them back from wherever the plan says. A second
/// reading that ignored the recorded value put the regenerated files somewhere
/// the cached path never looked, which is a stale seed rather than an error.
/// `Repo::tmp_dir` is the one reading, and this is what keeps it the one.
#[test]
fn the_scratch_directory_is_resolved_once() {
    let build = src_root().join("build/mod.rs");
    let text = std::fs::read_to_string(&build).unwrap();
    let readings: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("var(\"TMPDIR\")"))
        .map(|(n, l)| (n + 1, l.trim()))
        .collect();
    assert_eq!(
        readings.len(),
        1,
        "`TMPDIR` must be resolved only by `Repo::tmp_dir`; every other site calls \
         that. Found {} readings in build/mod.rs: {:#?}",
        readings.len(),
        readings
    );
    assert!(
        readings[0].1.contains("match") || text.contains("fn tmp_dir"),
        "the one reading must be `Repo::tmp_dir`, not an inline expansion: {:?}",
        readings[0]
    );
}

/// **Every `Step` variant is handled by every step-dispatch loop.** The executor
/// walks a step list in two places — `run_steps`, for prerequisites, and a second
/// loop inside `run_artefact` for the artefact pipeline — and both end in a
/// catch-all that bails. So a variant handled by one and missed by the other
/// compiles cleanly and fails at run time, on whichever repo happens to have a
/// recipe of that shape.
///
/// `Step::Boundary` is the case this guards. It was added to `run_steps` and
/// missed in `run_artefact`, and the result was not a wrong artefact but a hard
/// failure forty minutes into a MONDO build:
/// `internal: uncovered step reached executor: ── new invocation`.
///
/// That is one of three instances of the same shape in a single night — the
/// boundary rule implemented on `Op::Merge` alone, `owl_anon_blocks`
/// invalidation implemented in `extract` and `remove` but not `filter`, and this
/// — so the guard is worth more than the one variant it names. A rule that holds
/// of a family has to be checked against the family, because the compiler cannot:
/// a catch-all arm satisfies exhaustiveness while handling nothing.
#[test]
fn every_step_variant_reaches_both_dispatch_loops() {
    let step_src = std::fs::read_to_string(src_root().join("plan/step.rs")).unwrap();
    // The `Step` enum's variants, read off its declaration.
    let enum_body = step_src
        .split_once("pub enum Step {")
        .expect("Step enum")
        .1
        .split_once("\n}")
        .expect("end of Step enum")
        .0;
    let variants: Vec<String> = enum_body
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            let name = t.split(['{', '(', ',']).next()?.trim();
            (!name.is_empty()
                && name.chars().next()?.is_ascii_uppercase()
                && name.chars().all(|c| c.is_ascii_alphanumeric()))
            .then(|| name.to_string())
        })
        .collect();
    assert!(variants.len() >= 8, "expected the full Step enum, found {variants:?}");

    let build = std::fs::read_to_string(src_root().join("build/mod.rs")).unwrap();
    // The two loops, split at the second one's opening.
    let (prereq_loop, artefact_loop) = build
        .split_once("fn run_artefact")
        .expect("run_artefact exists in build/mod.rs");

    // `Inert` never reaches a plan (the planner drops it) and `Partial` is always
    // matched alongside `Op`; both are handled, just not by bare name.
    let exempt = ["Inert", "Partial"];
    let mut missing: Vec<String> = Vec::new();
    for v in &variants {
        if exempt.contains(&v.as_str()) {
            continue;
        }
        let pat = format!("Step::{v}");
        // `Op` is matched as `Step::Op(op)`; the shell-shaped variants are routed
        // through the `is_shell_step` guard rather than by name.
        let shell_routed =
            ["Shell", "Fallback", "Jq", "Sssom", "Oort", "OwlmakeCli", "UnsupportedSubcommand"];
        let in_prereq = prereq_loop.contains(&pat)
            || (shell_routed.contains(&v.as_str()) && prereq_loop.contains("is_shell_step"));
        let in_artefact = artefact_loop.contains(&pat)
            || (shell_routed.contains(&v.as_str()) && artefact_loop.contains("is_shell_step"));
        if !in_prereq || !in_artefact {
            missing.push(format!(
                "Step::{v} (run_steps: {}, run_artefact: {})",
                if in_prereq { "handled" } else { "MISSING" },
                if in_artefact { "handled" } else { "MISSING" },
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "a Step variant is handled by one dispatch loop and not the other. Both end in a \
         catch-all `bail!`, so this compiles and fails at run time on whichever repo has a \
         recipe of that shape:\n  {}",
        missing.join("\n  ")
    );
}
