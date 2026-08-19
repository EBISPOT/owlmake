//! `validate-profile` — check an ontology against an OWL 2 profile
//! (`--profile EL|QL|RL|DL`).

use std::path::PathBuf;

use anyhow::bail;
use clap::Args as ClapArgs;

use crate::profile::{self, Profile};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// OWL profile to validate against: DL, EL, RL, QL, or Full. `Full` accepts
    /// any OWL 2 ontology (no restrictions). Required — there is no default.
    #[arg(short = 'p', long)]
    pub profile: Option<String>,
    /// Optional report output file (defaults to stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    // `--profile` has no default: a recipe that lost its `--profile DL` (or a
    // hand-run command that forgot it) must fail rather than silently validate
    // against some other profile — each profile has its own check set here (DL runs
    // a global non-simple-property pass the others never run, and their per-axiom
    // checks are not a subset of DL's), so a defaulted answer would not even be
    // conservative. Checked before the input is loaded.
    let Some(profile_name) = args.profile.as_deref() else {
        bail!("Missing Profile Error: a profile must be specified with --profile (DL, EL, QL, RL or Full)");
    };
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // OWL 2 Full imposes no restrictions — there is nothing to check, so it never
    // produces violations. Handle it here since the underlying profile checker
    // only models the EL/QL/RL/DL sub-profiles.
    if profile_name.eq_ignore_ascii_case("Full") {
        // Name the profile in full ("OWL 2 Full", not "OWL 2") so the report says
        // which profile was asked for.
        let report = format!("{}\n", header("OWL 2 Full", &[]));
        match &args.output {
            Some(p) => std::fs::write(p, report)?,
            None => print!("{report}"),
        }
        return Ok(Some(model));
    }

    let profile = Profile::parse(profile_name).ok_or_else(|| {
        anyhow::anyhow!("unknown profile: {profile_name} (use EL/QL/RL/DL/Full)")
    })?;

    let violations = profile::validate(&model, profile);

    // The report is the profile name, `" Profile Report: "`, then either the
    // in-profile marker or one `\n`-prefixed line per violation. A failing profile
    // check prints this file, so it IS the error message a curator reads — the
    // wording and layout are part of the command's contract.
    let report = format!("{}\n", header(profile.name(), &violations));

    match &args.output {
        Some(p) => std::fs::write(p, &report)?,
        None => print!("{report}"),
    }

    if !violations.is_empty() {
        bail!("{} {} profile violation(s)", violations.len(), profile.name());
    }
    Ok(Some(model))
}

/// Render the profile report body (no trailing newline).
fn header(name: &str, violations: &[profile::Violation]) -> String {
    let mut s = format!("{name} Profile Report: ");
    if violations.is_empty() {
        s.push_str("[Ontology and imports closure in profile]");
        return s;
    }
    for v in violations {
        // The offending axiom is identified after the message by its kind, which
        // is what `Violation` records.
        s.push('\n');
        s.push_str(&format!("{} [{}]", v.reason, v.axiom_kind));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(reason: &str) -> profile::Violation {
        profile::Violation {
            axiom_kind: "SubClassOf".to_string(),
            reason: reason.to_string(),
        }
    }

    #[test]
    fn clean_report_is_owlapi_shaped() {
        assert_eq!(
            header("OWL 2 DL", &[]),
            "OWL 2 DL Profile Report: [Ontology and imports closure in profile]"
        );
    }

    #[test]
    fn violations_are_newline_prefixed_after_the_header() {
        let report = header("OWL 2 DL", &[v("Use of undeclared class: http://x/A")]);
        assert_eq!(
            report,
            "OWL 2 DL Profile Report: \nUse of undeclared class: http://x/A [SubClassOf]"
        );
        // The header keeps its trailing space even when violations follow.
        assert!(report.starts_with("OWL 2 DL Profile Report: \n"));
    }
}
