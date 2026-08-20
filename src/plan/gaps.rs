//! Gap derivation: the one place that decides what a plan cannot build.
//!
//! A gap is a REFUSAL — [`crate::plan::Plan::blocking_gaps`] stops the build with
//! it — so it is derived rather than serialized: a refusal recorded in a file is
//! one a hand edit can delete. Deriving it, though, has to be possible from the
//! plan ALONE: "is there a rule for this path" is answered by the plan's own
//! target set, so loading a committed plan never has to look outside it.
//!
//! These functions take a directory and plan data, and nothing else. Both the
//! planner and the loader call them, so the two paths cannot drift.

use std::collections::HashSet;
use std::path::Path;

use crate::plan::step::Op;
use crate::plan::{ImportPlan, Step};

/// Term files a step needs that are neither on disk nor produced by a planned
/// target.
///
/// `planned` is the plan's own answer to "is there a rule for this" — the only
/// answer there is, since the executor consults nothing outside the plan.
pub fn term_file_gaps(dir: &Path, steps: &[Step], planned: &HashSet<String>) -> Vec<String> {
    let mut gaps = Vec::new();
    // A rule can PRODUCE its own term file: HPO's `test.owl` opens with a
    // `query … --query hp_terms.sparql tmp/ontologyterms-test.txt` and then
    // filters over it in the same chain. Treating that as a missing dependency
    // refuses the whole target, and with it `test_obo` — the side-effect write
    // that produces the published `hp.obo`.
    let produced = step_outputs(steps);
    for tf in step_term_files(steps) {
        if !dir.join(&tf).exists() && !planned.contains(&tf) && !produced.contains(&tf) {
            gaps.push(format!(
                "requires term file `{tf}`, which is absent and has no rule to build"
            ));
        }
    }
    gaps
}

/// Paths a rule's own steps WRITE — a query's per-query output, a convert's
/// `output`, a file op's destination — so a later step in the same rule may
/// depend on one without it being an external prerequisite.
pub fn step_outputs(steps: &[Step]) -> HashSet<String> {
    let mut out = HashSet::new();
    fn walk(steps: &[Step], out: &mut HashSet<String>) {
        for s in steps {
            if let Step::Branch { then_steps, else_steps, .. } = s {
                walk(then_steps, out);
                walk(else_steps, out);
                continue;
            }
            let op = match s {
                Step::Op(op) | Step::Partial { op, .. } => op,
                _ => continue,
            };
            match op {
                Op::Query { selects, constructs, .. } => {
                    for (_, o) in selects.iter().chain(constructs.iter()) {
                        if !o.is_empty() {
                            out.insert(o.clone());
                        }
                    }
                }
                Op::Convert { output: Some(o), .. } => {
                    out.insert(o.clone());
                }
                _ => {}
            }
        }
    }
    walk(steps, &mut out);
    out
}

/// `--term-file` paths referenced by remove / filter / materialize steps,
/// including inside a conditional branch (which `Step::gaps` also descends).
pub fn step_term_files(steps: &[Step]) -> Vec<String> {
    let mut out = Vec::new();
    collect_term_files(steps, &mut out);
    out
}

fn collect_term_files(steps: &[Step], out: &mut Vec<String>) {
    for s in steps {
        match s {
            Step::Branch { then_steps, else_steps, .. } => {
                collect_term_files(then_steps, out);
                collect_term_files(else_steps, out);
                continue;
            }
            _ => {}
        }
        let op = match s {
            Step::Op(op) | Step::Partial { op, .. } => op,
            _ => continue,
        };
        match op {
            Op::Remove(spec) => out.extend(spec.term_files.clone()),
            Op::Filter(spec) => out.extend(spec.term_files.clone()),
            Op::Materialize { term_files, .. } => out.extend(term_files.clone()),
            _ => {}
        }
    }
}

/// Prerequisites a target needs that are neither on disk nor buildable by any
/// planned target.
///
/// make refuses these with "No rule to make target", and so does owlmake. Left
/// for the steps to consume, the failure surfaces from inside whatever the recipe
/// shells out to: EFO's `check_mondo_obsoletes` needs `mirror/mondo.owl` and its
/// Makefile has no rule for it, so the build dies in a Python traceback rather
/// than naming the missing file.
/// Every file some planned recipe WRITES, besides its own target.
///
/// A prerequisite with no rule of its own is a gap only if nothing builds it —
/// and a phony's recipe builds files. MONDO's ten QC reports are written by
/// `fix-report-%`, whose target is the phony `fix-report-basic-report` and whose
/// recipe is `../utils/tidy-sparql-output.pl reports/$*.tmp.tsv > reports/$*.tsv`,
/// so `reports/basic-report.tsv` is named by no rule and produced by one. Read as
/// a gap, `all_artefacts` refuses to run at all — and only sometimes, depending on
/// whether the plan happened to be generated before or after the reports were
/// written, which is worse than either answer.
pub fn recipe_outputs(steps: &[crate::plan::step::Step]) -> Vec<String> {
    use crate::build::recipe::FileOp;
    use crate::plan::step::Step;
    let mut out = Vec::new();
    for step in steps {
        match step {
            Step::Shell { command, .. } | Step::Fallback { command, .. } => {
                out.extend(redirect_targets(command));
            }
            Step::File(FileOp::Move { dst, .. } | FileOp::Copy { dst, .. }) => out.push(dst.clone()),
            Step::File(FileOp::Print { dst: Some(dst), .. }) => out.push(dst.clone()),
            _ => {}
        }
    }
    out
}

/// The `> FILE` / `>> FILE` destinations in a command line, outside quotes.
fn redirect_targets(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = command.as_bytes();
    let (mut i, mut quote) = (0usize, 0u8);
    while i < bytes.len() {
        let c = bytes[i];
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' | b'"' => quote = c,
            b'>' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] == b'>' {
                    j += 1;
                }
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let start = j;
                while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j > start {
                    out.push(command[start..j].trim_matches(['"', '\'']).to_string());
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

pub fn prerequisite_gaps(
    dir: &Path,
    needs: &[String],
    planned: &HashSet<String>,
    phony: &HashSet<String>,
) -> Vec<String> {
    let mut gaps = Vec::new();
    for n in needs {
        // Plugin-JAR targets are deliberately not planned: owlmake implements
        // those plugin commands natively, so there are no JARs to fetch.
        if n == "all_robot_plugins" || n.ends_with(".jar") {
            continue;
        }
        // A `.PHONY` prerequisite names no file: it declares "this rule is always
        // out of date". UBERON forces a re-fetch of its three mapping sets by
        // giving each one a phony `.FORCE` prerequisite, so reading `.FORCE` as a
        // missing input would report all three release assets as uncovered.
        if phony.contains(n) {
            continue;
        }
        // A data asset owlmake serves from its own bytes is not a missing input.
        // The path is the reference image's, so it never exists here, and the
        // bytes arrive when the recipe runs — which is after this gate.
        if crate::build::recipe::is_served_image_asset(n) {
            continue;
        }
        if !planned.contains(n) && !dir.join(n).exists() {
            gaps.push(format!(
                "needs `{n}`, which is absent and has no rule to build"
            ));
        }
    }
    gaps
}

/// Gaps that block building one import module, and whether its output is already
/// on disk.
///
/// An import whose `output` is EMPTY is the recorded statement "ingest could not
/// resolve this declared `owl:imports`". That must stay a gap, and it must not
/// count as cached: `Path::join("")` is the directory itself, which always
/// exists, so an unguarded existence test marks every unresolvable import as
/// present and lets the release build without it.
pub fn import_state(dir: &Path, imp: &ImportPlan, merged_cached: bool) -> (bool, Vec<String>) {
    let cached = (!imp.output.is_empty() && dir.join(&imp.output).exists()) || merged_cached;
    let mut gaps = Vec::new();
    if imp.output.is_empty() {
        gaps.push(format!(
            "declared import `{}` is not resolvable (no catalog entry or local module); \
             the release would be missing it",
            imp.source
        ));
    } else if imp.source == "<custom mirror script>" && !cached {
        gaps.push(format!(
            "custom mirror for `{}` requires a project script owlmake can't run \
             (provide a cached mirror/import)",
            imp.id
        ));
    }
    (cached, gaps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unresolvable_import_is_not_cached_and_carries_one_gap() {
        let imp = ImportPlan {
            id: "mondo".into(),
            source: "http://example.org/mondo.owl".into(),
            output: String::new(),
            steps: vec![],
            cached: false,
            gaps: vec![],
            product: None,
            mirror_steps: vec![],
                    mirror_inputs: Vec::new(),
        };
        let (cached, gaps) = import_state(Path::new("."), &imp, false);
        assert!(!cached, "an empty output must not resolve to the directory");
        assert_eq!(gaps.len(), 1);
    }
}
