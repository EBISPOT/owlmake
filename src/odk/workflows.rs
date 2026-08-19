//! Read the toolchain a repo builds with, out of the repo's own CI.
//!
//! Ingest only. A hand-written Makefile names its own ROBOT — EFO's is
//! `ROBOT = ../../bin/robot` — and the version it names decides artefact BYTES:
//! from 1.9.9 on, a nested axiom annotation carries its own `meta`, so
//! `<ont>.json` has two different shapes depending on the version. A repo's
//! committed releases carry the shape of the version it pins, so ingest resolves
//! that version once and the plan records it.
//!
//! The repo states that version where every OBO repo states its toolchain: in
//! `.github/workflows/`. EFO's `copilot-setup-steps.yml` fetches
//! `ontodev/robot/releases/download/v1.9.7/robot.jar` — the jar its
//! `ROBOT = ../../bin/robot` launcher runs.
//!
//! An ODK repo pins ROBOT the other way — through the ODK image. Usually the
//! Makefile says so in `ODK_VERSION_MAKEFILE`, but MONDO's does not: it names
//! the image in `src/ontology/run.sh` (`IMAGE=${IMAGE:-odkfull:v1.6}`, the
//! script the generated Makefile itself tells you to run) and as the `container:`
//! of every workflow. Those are the repo's statement of its toolchain just as
//! much as the Makefile variable is, so `odk_image_version` reads them.
//!
//! Only an `odkfull`/`odklite` image maps to a ROBOT. EFO's workflows also carry
//! `obolibrary/odkfull:` containers, and neither provides the `robot` that
//! repo's `$(ROBOT)` resolves to — which is why the image is consulted only for
//! a repo whose Makefile the ODK generated.

use std::path::Path;

/// A dotted version, as a comparable tuple.
pub type Version = (u32, u32, u32);

/// The ROBOT release this repo's CI installs, if it names one.
pub fn ci_robot_version(root: &Path) -> Option<Version> {
    let mut found: Option<Version> = None;
    for entry in std::fs::read_dir(root.join(".github/workflows")).ok()?.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "yml" || e == "yaml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for v in robot_releases(&text) {
            // The highest wins: a repo that pins one version for its build and an
            // older one somewhere incidental is built by the newer.
            if found.is_none_or(|f| v > f) {
                found = Some(v);
            }
        }
    }
    found
}

/// The ODK image this repo builds with, as the repo itself names it.
///
/// `src/ontology/run.sh` wins: it is the entry point the generated Makefile
/// documents (`sh run.sh make …`), so its default image is what a person running
/// this repo's build actually gets. Failing that, the `container:` of the
/// workflows — the highest named, since a repo pinning one version for its
/// release and an older one for some incidental job is released by the newer.
pub fn odk_image_version(root: &Path) -> Option<Version> {
    if let Ok(text) = std::fs::read_to_string(root.join("src/ontology/run.sh")) {
        if let Some(v) = odk_images(&text).into_iter().max() {
            return Some(v);
        }
    }
    let mut found: Option<Version> = None;
    for entry in std::fs::read_dir(root.join(".github/workflows")).ok()?.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "yml" || e == "yaml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for v in odk_images(&text) {
            if found.is_none_or(|f| v > f) {
                found = Some(v);
            }
        }
    }
    found
}

/// Every `odkfull:v<version>` / `odklite:v<version>` in one file.
fn odk_images(text: &str) -> Vec<Version> {
    let mut out = Vec::new();
    for marker in ["odkfull:v", "odklite:v"] {
        let mut rest = text;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(rest.len());
            if let Some(v) = parse_version(&rest[..end]) {
                out.push(v);
            }
        }
    }
    out
}

/// The ROBOT an ODK image ships.
///
/// Only the boundary that changes artefact BYTES is modelled: `odkfull:v1.6`
/// ships ROBOT 1.9.8 and `odkfull:v1.6.1` ships 1.9.10. Everything
/// older than v1.6.1 is older than 1.9.9 too, and anything newer is read as the
/// newest ROBOT owlmake models — the point of this map is which side of 1.9.9 a
/// repo sits on, not a full release table.
pub fn odk_robot_version(odk: Version) -> Version {
    if odk >= (1, 6, 1) { (1, 9, 10) } else { (1, 9, 8) }
}

/// Every `ontodev/robot/releases/download/v<version>/…` in one workflow file.
fn robot_releases(text: &str) -> Vec<Version> {
    const MARKER: &str = "ontodev/robot/releases/download/v";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(MARKER) {
        rest = &rest[at + MARKER.len()..];
        let end = rest.find('/').unwrap_or(rest.len());
        if let Some(v) = parse_version(&rest[..end]) {
            out.push(v);
        }
    }
    out
}

/// `1.9.10` → `(1, 9, 10)`; a missing component reads as zero.
fn parse_version(s: &str) -> Option<Version> {
    let core = s.split(['-', '_']).next()?;
    if core.is_empty() || !core.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let mut it = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    Some((it.next()?, it.next().unwrap_or(0), it.next().unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_robot_release_a_workflow_installs() {
        let text = "        run: |\n          curl -L \
             https://github.com/ontodev/robot/releases/download/v1.9.7/robot.jar \
             -o ~/.jar-cache/robot.jar\n";
        assert_eq!(robot_releases(text), vec![(1, 9, 7)]);
    }

    #[test]
    fn an_odk_container_is_not_a_robot_release() {
        assert!(robot_releases("    container: obolibrary/odkfull:v1.5.1\n").is_empty());
    }

    #[test]
    fn reads_the_odk_image_a_workflow_runs_in() {
        assert_eq!(odk_images("    container: obolibrary/odkfull:v1.6\n"), vec![(1, 6, 0)]);
        assert_eq!(odk_images("    container: obolibrary/odklite:v1.5.4\n"), vec![(1, 5, 4)]);
    }

    #[test]
    fn reads_the_odk_image_run_sh_defaults_to() {
        // MONDO's `src/ontology/run.sh`, which its Makefile documents as the way
        // to run the build — and the only place MONDO names a version.
        assert_eq!(odk_images("IMAGE=${IMAGE:-odkfull:v1.6}\n"), vec![(1, 6, 0)]);
    }

    #[test]
    fn the_odk_image_decides_which_side_of_robot_1_9_9_a_repo_is_on() {
        assert_eq!(odk_robot_version((1, 6, 0)), (1, 9, 8));
        assert_eq!(odk_robot_version((1, 6, 1)), (1, 9, 10));
        assert!(odk_robot_version((1, 5, 4)) < (1, 9, 9));
    }
}
