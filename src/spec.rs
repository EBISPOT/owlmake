//! `owlmake.yaml` — the declarative build plan.
//!
//! This is the on-disk, hand-editable description of a build: a repository's
//! options over owlmake's standard build, and what it states beyond them.
//! owlmake reads [`OwlmakeSpec`] from the file, resolves the standard build for
//! its options, lays the file's statements over it, and consults nothing else
//! about how the repository is built.
//!
//! The format is decoupled from owlmake's internal runtime types ([`Plan`],
//! [`Op`], [`Step`]) on purpose, so the file stays a stable, documented contract
//! even as the internals evolve. A JSON Schema is *derived* from these types
//! (via `schemars`), so it can never drift from what the loader accepts, and
//! loaded files are validated against it (via `jsonschema`) before use.

use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::plan::{ArtefactPlan, ImportPlan, Plan};
use crate::build::recipe::FileOp;
use crate::plan::step::{self as step, AnnotateSpec, FilterSpec, Op, RemoveSpec, Step};

/// Conventional filename of the build plan at the repository root. YAML is the
/// default — the plan is meant to be read and edited by hand, and YAML keeps the
/// recipe lines and long IRI lists legible.
pub const PLAN_FILE: &str = "owlmake.yaml";
/// The JSON spelling of the same plan (`--plan-format json`). Both are accepted
/// on load; committing both is fine as long as they describe the same build.
pub const PLAN_FILE_JSON: &str = "owlmake.json";

/// Serialization format of the committed plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFormat {
    Yaml,
    Json,
}

impl PlanFormat {
    pub fn parse(name: &str) -> Result<PlanFormat> {
        match name.trim().to_ascii_lowercase().as_str() {
            "yaml" | "yml" => Ok(PlanFormat::Yaml),
            "json" => Ok(PlanFormat::Json),
            other => bail!("unknown plan format `{other}` (expected `yaml` or `json`)"),
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            PlanFormat::Yaml => PLAN_FILE,
            PlanFormat::Json => PLAN_FILE_JSON,
        }
    }

    /// The format a plan file's name implies (JSON only for a `.json` suffix).
    pub fn of_path(path: &Path) -> PlanFormat {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => PlanFormat::Json,
            _ => PlanFormat::Yaml,
        }
    }
}

/// The committed plan in `dir`, whichever spelling it uses.
///
/// A repo may commit both; that is only meaningful if they describe the SAME
/// build, so when both are present they are compared and a mismatch is an error
/// rather than a silent choice between two different plans.
pub fn find_in(dir: &Path) -> Result<Option<PathBuf>> {
    let yaml = dir.join(PLAN_FILE);
    let json = dir.join(PLAN_FILE_JSON);
    match (yaml.is_file(), json.is_file()) {
        (false, false) => Ok(None),
        (true, false) => Ok(Some(yaml)),
        (false, true) => Ok(Some(json)),
        (true, true) => {
            let a = serde_json::to_value(load(&yaml)?)?;
            let b = serde_json::to_value(load(&json)?)?;
            if a != b {
                bail!(
                    "{} and {} describe different builds — delete or regenerate one of them",
                    yaml.display(),
                    json.display()
                );
            }
            Ok(Some(yaml))
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The newest artefact-format generation owlmake models — what a plan that names
/// no version is read as. Kept equal to the default ingest records
/// (`odk::planner::CURRENT_ROBOT`); `Plan → Spec → Plan` is the identity only if
/// both ends agree on the default.
const CURRENT_ROBOT: (u32, u32, u32) = (1, 9, 10);

/// `(1, 9, 10)` → `"1.9.10"`.
fn format_version(v: (u32, u32, u32)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// `"1.9.8"` → `(1, 9, 8)`; a missing component reads as zero.
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    Some((it.next()?, it.next().unwrap_or(0), it.next().unwrap_or(0)))
}

/// What a repository commits: `owlmake.yaml`.
///
/// One shape. The file is a set of options over owlmake's standard build, and
/// statements about the plan those options resolve to. Its top-level keys are
/// the repository's options — `release_artefacts`, `import_group`, `components`,
/// `report`, … (see [`crate::odk::builtin::Config`]) — and beside them what the
/// repository states outright: a target it builds in a way of its own, an
/// import or the pattern products it obtains its own way, the release version,
/// the ontology IRI, the output conventions it emulates. A statement overrides
/// or extends what the standard build derives; what the file leaves out is
/// derived. A file with no statements is the standard build for its options; a
/// file that states every target is a build of the repository's own; and every
/// file in between means what it says.
///
/// A repository whose build is all its own — one with no standard build to lean
/// on — says so with `use_builtin_rules: false`. Nothing is then derived: the
/// file names every target, every path and every switch, and an option of the
/// standard build is refused because nothing would read it.
///
/// An unknown key is an error ([`OwlmakeSpec::check`]).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OwlmakeSpec {
    /// Optional pointer to a (shared) JSON Schema for editor/CI validation. Not
    /// written by default — `owlmake schema` emits the canonical schema; the
    /// loader always validates against the built-in one regardless of this field.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// The lowest owlmake version that can build this plan.
    ///
    /// Set automatically to the version that GENERATED the file: a plan can only
    /// use the vocabulary its generator knew, so that version is by construction
    /// sufficient to execute it. An older binary reading a newer plan would
    /// otherwise fail somewhere in the middle of a build, on a step it cannot
    /// name — this turns that into one clear message before anything runs.
    ///
    /// Optional: a hand-written plan may omit it, and one without the field is
    /// accepted by every version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_owlmake_version: Option<String>,
    /// Whether owlmake's built-in rules — the standard build — underlie this
    /// file. They do unless the file says otherwise: the rules for the options
    /// resolve to every target, path and switch the file does not state itself.
    ///
    /// `false` for a repository whose build is all its own. The file is then the
    /// whole build — every target under `targets`, every import under `imports`,
    /// the release `version`, the `ontology_iri`, the `reasoner` — and nothing is
    /// derived from a convention. An option of the standard build is refused in
    /// such a file, because nothing would read it.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub use_builtin_rules: bool,
    /// The ODK release whose output this build emulates, e.g. `"1.6.1"`: the
    /// bytes its artefacts carry, not the rules that build them. The standard
    /// build is owlmake's own whatever this says.
    ///
    /// A release settles more than a tool version does — the OBO extended prefix
    /// map is baked into the image, and the two releases' maps differ by 388
    /// prefixes — and a release neither writes nor reads OBO `[Instance]` frames.
    /// Absent, the artefacts are written under owlmake's own conventions: the
    /// current tool generation, and `[Instance]` frames written and read. A
    /// repository migrating from the release records it; one that wants its
    /// individuals in its OBO export deletes the line and keeps everything else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulate_odk_version: Option<String>,
    /// The artefact-format generation this build emulates, e.g. `"1.9.8"`. Two
    /// byte-level behaviours flip at 1.9.9 — see `Plan::emulate_robot_version`
    /// for both. Absent, it follows from `emulate_odk_version`, or is the current
    /// generation when that is absent too.
    ///
    /// Stated by a repository that ships its own tool — EFO launches
    /// `../../bin/robot` at 1.9.7 — and so emulates a tool version and no
    /// release. Recording BOTH is an error unless they agree, because a plan that
    /// says two things about one behaviour cannot be obeyed: see
    /// [`OwlmakeSpec::check_emulation_versions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulate_robot_version: Option<String>,
    /// Ontology short id (e.g. `oba`). The one key every file states.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The release version to stamp, as a default (typically `{today}`, the date
    /// of the build). Stated, it replaces the one the standard build derives; a
    /// build of the repository's own must state it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// Optional: the repo file the release version is read from, relative to the
    /// repo root. When present a run that names no version reads it, and
    /// [`version`](Self::version) is only the fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_file: Option<String>,
    /// The ontology IRI of the primary product. Stated, it replaces the one the
    /// standard build derives from `uribase` and `id`; a build of the
    /// repository's own must state it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ontology_iri: String,
    /// Reasoner backend (`ELK`, `whelk`, …). An option of the standard build as
    /// well as a plan field; a build of the repository's own must state it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoner: String,
    /// The repository's options for the standard build, held as written and in the
    /// order they were written.
    #[serde(flatten)]
    pub options: serde_json::Map<String, serde_json::Value>,
    /// Whether imports are squashed into a single base-merged module. The standard
    /// build takes this from `import_group`; a build of the repository's own
    /// states it here.
    #[serde(default, skip_serializing_if = "is_false")]
    pub use_base_merging: bool,
    /// IRI patterns dropped from the merged import. From `import_group` in the
    /// standard build.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_iri_patterns: Vec<String>,
    /// How individuals are handled in the merged import — the `--individuals`
    /// setting of its module extraction:
    /// `include`/`minimal`/`definitions`/`exclude`. From `import_group` in the
    /// standard build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slme_individuals: Option<String>,
    /// Import products feeding the release. Each entry is the whole of how that
    /// import is obtained and cut: with the standard build it replaces the
    /// standard import of its id, and must be a product of `import_group`; a
    /// build of the repository's own lists every import here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<ImportSpec>,
    /// The single merged-import file, when base-merging is on. The standard build
    /// derives it from `import_group`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_import: Option<String>,
    /// The ontology IRI to stamp on the merged import module. Without it the IRI is
    /// derived from `ontology_iri` and the module path (compression suffix and the
    /// ontology directory stripped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_import_iri: Option<String>,
    /// Shard the merged import: one functional-syntax document per source
    /// ontology in this directory (`imports/merged`), and `merged_import` becomes
    /// the index that `owl:imports` them. Opt-in; without it the merged import is
    /// the single file `merged_import` names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_import_shards: Option<String>,
    /// Cap on one shard file in bytes; a shard above it is split on its local ids
    /// (`mondo-000.owl`, …). Default 10 MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_import_shard_bytes: Option<usize>,
    /// What a bare `owlmake` builds, RESOLVED. A goal NAME (`all`) would need
    /// something outside the plan to say what it covered; the resolved list needs
    /// nothing. Stated, it is the whole list and replaces the one the standard
    /// build derives from its default goal. EFO's `all` covers `all_imports
    /// all_gwas all_components release qc`, so a bare build that stopped at the
    /// release artefacts would leave the QC unrun.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_targets: Option<Vec<String>>,
    /// Targets that name no file — always out of date, never satisfied by a
    /// same-named file. Joined to the standard build's phony names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phony: Vec<String>,
    /// Paths the build writes on its way to something else and does not keep — a
    /// file it reaches only through a pattern, never named outright. Removed once
    /// the chain that needed it is done. Joined to the standard build's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transient_targets: Vec<String>,
    /// Paths owlmake's own engines produce without a recorded rule — the DOSDP
    /// pattern products and the per-import mirrors. A prerequisite naming one of
    /// these is satisfied by the build even though no target produces it. Joined
    /// to the standard build's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_targets: Vec<String>,
    /// The edit ontology, relative to the ontology directory. The standard build
    /// reads `<id>-edit.<edit_format>`; stated, this is the file every rule reads
    /// instead. A build of the repository's own names it here or has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_file: Option<String>,
    /// The `owl:imports` catalog file, relative to the ontology directory. The
    /// plan names it; execution reads it. Not inlined — Protégé writes that file.
    /// An option of the standard build as well as a plan field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_file: Option<String>,
    /// The DOSDP pattern set, enumerated at plan time. Stated, it replaces the set
    /// the standard build enumerates from the pattern directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dosdp: Option<DosdpSpec>,
    /// `--strict` parsing: structurally-broken RDF is rejected rather than
    /// repaired, so it decides which axioms survive a parse.
    #[serde(default, skip_serializing_if = "is_false")]
    pub strict: bool,
    /// `-x`/`--xml-entities` output: `&prefix;` entity references in RDF/XML, a
    /// byte-level change to every RDF/XML artefact.
    #[serde(default, skip_serializing_if = "is_false")]
    pub xml_entities: bool,
    /// The rebuild switches this plan exposes — the target groups a run may ask
    /// to rebuild rather than reuse. The VALUES are run inputs and are not
    /// recorded; the parameter space is. Stated, it is the whole list and
    /// replaces the groups the build derives from its switches; a target that
    /// exists only under a switch says so with `when`, and joins that switch's
    /// group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_groups: Option<Vec<crate::plan::RefreshGroup>>,
    /// The build-configuration variables whose value decides which rules exist,
    /// and the value this repository gives each. A run may assign one, but only
    /// to the value recorded here: the plan holds the rules of one branch, so a
    /// run asking for the other is refused rather than quietly given this one.
    /// Joined to the standard build's, the stated value winning.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub gating_flags: std::collections::BTreeMap<String, String>,
    /// Build variables the executor reads at build time rather than at plan time
    /// — `SRC`, `OTHER_SRC`, `ROBOT`, `OBOBASE`, `ODK_VERSION_MAKEFILE`. They are
    /// recorded so a plan-only repo builds exactly what the plan was generated
    /// from: without `ODK_VERSION_MAKEFILE` the OBO Graphs writer silently stops
    /// nesting axiom-annotation `meta`, and without `OTHER_SRC` the merged-import
    /// seed loses whole component branches. A stated variable is bound before the
    /// standard build's rules are built, so they read it too.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub variables: std::collections::BTreeMap<String, String>,
    /// Steps in the rules that BUILD declared components which owlmake cannot
    /// reproduce (e.g. uPheno's `python3 upheno_build.py`). Release-blocking, so
    /// recorded rather than silently re-derived as empty on load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_gaps: Vec<String>,
    /// The targets the repository builds in a way of its own — or, in a build of
    /// the repository's own, every target. Each replaces the standard target of
    /// that name, or is one the standard build does not have — unless it says
    /// `extends`, and then its `needs` join the standard target's and the standard
    /// steps stand. `artefact` makes it a release artefact; `when` names the
    /// switches it exists under.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<ArtefactSpec>,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// One import product.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportSpec {
    pub id: String,
    /// Upstream source URL, or `<custom mirror script>` for project scripts. The
    /// pipeline input is this ontology (mirrored under `mirror/<id>.owl`).
    /// An entry that `extends` leaves it out: the standard build derives it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Output module path, relative to `src/ontology` (`imports/<id>_import.owl`).
    /// An entry that `extends` leaves it out: the standard build derives it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    /// The mirror→module pipeline: the ordered operations that turn the source
    /// ontology into the committed import module (`extract`/`filter`/`remove`/
    /// `rename`/…). Seed and exclude `term_files` are recorded as their committed
    /// source paths (e.g. `iri_dependencies/<id>_terms.txt`), so the build never
    /// re-guesses where the term list lives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepEntry>,
    /// The import product declaration this module came from — mirror URL/type,
    /// base-IRI flags, `is_large`. A `--imports fresh` run reads these, so
    /// recording them keeps that path working from the plan alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<crate::odk::ImportProduct>,
    /// For a custom mirror (`mirror_type: custom`), the project's own
    /// `mirror-<id>` steps. owlmake cannot synthesize them — without these a
    /// plan-only fresh-import run has no way to produce `mirror/<id>.owl`. A
    /// mirror rule threads no model, so these are whole-command shell steps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirror_steps: Vec<StepEntry>,
    /// Appends this entry's `steps` to the end of the standard mirror→module
    /// pipeline for this import, which otherwise stands: the source, the output
    /// and the standard steps are derived as usual and need not be stated. This
    /// is how a repository states one extra operation over a module — COHO
    /// removes the subset properties its NCIT slice drags along with
    /// `remove --term oboInOwl:SubsetProperty --select children` — without
    /// restating the pipeline. Without it, the entry is the whole pipeline,
    /// taken as written.
    #[serde(default, skip_serializing_if = "is_false")]
    pub extends: bool,
    /// Files the mirror steps read that another rule makes (MONDO's
    /// `mirror/hgnc_gene.nt`, `mirror/ncbi_gene.nt`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirror_inputs: Vec<String>,
}

/// One SPARQL `--query`/`--select`/`--construct` invocation: the query file and
/// where its result is written. Named (rather than a positional pair) so the
/// `owlmake.json` is self-describing.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryResult {
    pub query: String,
    pub output: String,
}

impl QueryResult {
    fn from_pair((query, output): &(String, String)) -> Self {
        QueryResult { query: query.clone(), output: output.clone() }
    }
    fn into_pair(self) -> (String, String) {
        (self.query, self.output)
    }
}

/// A structured branch condition, e.g. `{ "condition": "file_exists", "filename":
/// "imports/foo.owl" }`. `condition` is one of `file_exists`, `file_non_empty`,
/// `file_missing`, `dir_exists` (each with a `filename`), or `shell` (with a
/// `command` evaluated by the shell as a fallback).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionSpec {
    pub condition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl ConditionSpec {
    fn from_condition(c: &crate::plan::step::Condition) -> Self {
        use crate::plan::step::Condition;
        let (condition, filename, command) = match c {
            Condition::FileExists(f) => ("file_exists", Some(f.clone()), None),
            Condition::FileNonEmpty(f) => ("file_non_empty", Some(f.clone()), None),
            Condition::FileMissing(f) => ("file_missing", Some(f.clone()), None),
            Condition::DirExists(f) => ("dir_exists", Some(f.clone()), None),
            Condition::Shell(c) => ("shell", None, Some(c.clone())),
        };
        ConditionSpec { condition: condition.into(), filename, command }
    }

    fn into_condition(self) -> crate::plan::step::Condition {
        use crate::plan::step::Condition;
        let f = self.filename.unwrap_or_default();
        match self.condition.as_str() {
            "file_exists" => Condition::FileExists(f),
            "file_non_empty" => Condition::FileNonEmpty(f),
            "file_missing" => Condition::FileMissing(f),
            "dir_exists" => Condition::DirExists(f),
            // `shell` (or any unrecognised kind) — fall back to a raw shell test.
            _ => Condition::Shell(self.command.unwrap_or(f)),
        }
    }
}

/// The DOSDP pattern set, resolved at plan time.
///
/// Ingest resolves the pattern directory once and enumerates it HERE, so
/// execution never `read_dir`s to discover which patterns to build and a missing
/// directory cannot skip the whole step in silence.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DosdpSpec {
    /// Where the generated definitions go (`../patterns/definitions.owl`).
    pub output: String,
    /// Prefix files pattern expansion reads (typically `config/prefixes.yaml`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefixes: Vec<String>,
    /// The query that derives the import seed's pattern half from the COMMITTED
    /// `output` when the DOSDP products are not regenerated (ODK `PAT=false`).
    ///
    /// That case is a DIFFERENT derivation, not a skipped one: with patterns
    /// regenerated the seed is the union of the per-pattern term files and
    /// `pattern_owl_seed.txt`, and without, the ODK runs this query over
    /// `definitions.owl` instead. Recorded because only the repo knows which
    /// query, and a plan-only build has to run it: with neither derivation, `cat
    /// $(PRESEED) $(TMPDIR)/all_pattern_terms.txt | sort | uniq` yields a seed
    /// short of every pattern term — and its exit status is `uniq`'s, so nothing
    /// reports the loss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_seed_query: Option<String>,
    /// The patterns that are BUILT: a template paired with its data table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<DosdpPattern>,
    /// What happens to the per-pattern products once they are merged: the rest of
    /// the repo's `definitions.owl` pipeline, in order.
    ///
    /// The two `annotate`s that stamp the ontology and version IRI are always
    /// here; a repo may add more. OBA post-processes the merge with `query
    /// --update ../sparql/postprocess-definitions.ru`, which strips the placeholder
    /// location out of ~1,900 definitions and deletes the synonyms built from it.
    /// Deriving the tail from a convention instead would silently drop that
    /// update and publish a `definitions.owl` — and, because it is a source of the
    /// edit merge, an `oba.owl`, `oba-base.owl`, `oba-full.owl` and `oba-basic.owl`
    /// — that the repo never asked for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepEntry>,
    /// Every template in the pattern directory — a SUPERSET of `patterns`,
    /// because `dosdp validate` covers templates that have no data table yet.
    /// Recorded separately so validation cannot silently check a subset while
    /// reporting success.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<String>,
}

/// One DOSDP pattern module: the template, the data table it is generated from,
/// and the options of the invocation that generates it.
///
/// A repository may run the generator over MORE THAN ONE data directory — HPO has
/// a `default` pipeline (`--restrict-axioms-to=logical`) and a `full` one
/// (unrestricted), and `patterns/definitions.owl` merges the products of both — so
/// the options belong to the module, not to the pattern set. The directory the
/// table sits in is also the BATCH: each `generate` invocation numbers its own
/// modules `urn:unnamed:ontology#ont1`, `#ont2`, … from one.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DosdpPattern {
    pub name: String,
    pub template: String,
    pub data: String,
    /// `--restrict-axioms-to`: `all` (the default), `logical`, or `annotation`.
    ///
    /// It decides WHICH AXIOMS this module contains, so it is as much a part of
    /// what this plan builds as the pattern itself. MP asks for `logical`;
    /// generating everything instead adds 8,875 annotation assertions to its
    /// `definitions.owl` and carries them into every artefact that merges it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restrict_axioms: Option<String>,
    /// `--restrict-axioms-column`: a TSV column whose truthy value restricts that
    /// ROW to logical axioms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restrict_axioms_column: Option<String>,
    /// `--add-axiom-source-annotation`: annotate each generated axiom with the
    /// pattern IRI it came from.
    #[serde(default)]
    pub add_axiom_source_annotation: bool,
    /// `--axiom-source-annotation-property`: the property that annotation uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axiom_source_annotation_property: Option<String>,
    /// `--generate-defined-class`: mint the defined class IRI from `base_IRI`
    /// plus a hash of the fillers when the table has no `defined_class` column.
    #[serde(default)]
    pub generate_defined_class: bool,
}

/// One release artefact and its build pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtefactSpec {
    /// Output filename (e.g. `oba-full.owl`).
    pub target: String,
    /// The pipeline input: the file the first step reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Everything this target depends on. Execution builds a prerequisite only
    /// when a target that needs it is missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// Which of `needs` are order-only: built first if missing, never compared
    /// with the target's age. Without them a rule whose only prerequisite is a
    /// DIRECTORY re-runs on every build, because a directory is touched by every
    /// file written into it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order_only: Vec<String>,
    /// Ordered operations applied to the input. None for a target that only groups
    /// others, or that `extends` a standard one.
    #[serde(default)]
    pub steps: Vec<StepEntry>,
    /// Set when ingest found no rule for this target. It blocks the release, so
    /// it is recorded: without it the artefact loads as one with no steps and
    /// silently builds nothing instead of reporting "no rule found".
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing_rule: bool,
    /// The recipe writes only the side files its steps name, never the target
    /// itself: no target file is created, and the executor must not
    /// materialise the pipeline model at the target path — see
    /// `ArtefactPlan::side_effect_only`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub side_effect_only: bool,
    /// Where the recipe sends its console output (`… reason > $@`). The steps
    /// name only the intermediates they write with `-o`, so for a check built out
    /// of what its tool prints this is the only field that names the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_file: Option<String>,
    /// The target is an intermediate of a pattern-rule chain: nothing in the
    /// build configuration spells its concrete name. When it is missing and the
    /// target that needs it is otherwise up to date, the chain does not run and
    /// the file is not created (ECTO's `tmp/stamp-component-<x>.owl`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub intermediate: bool,
    /// What this target is built by under the OTHER value of a switch. Absent is
    /// the ordinary case: the other branch defines no rule, so under that value
    /// the target is not built and the committed file stands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<BranchSpec>,
    /// This entry ADDS its `needs` to the standard target of the same name, whose
    /// steps stand. Without it an entry is the whole of how the target is built,
    /// steps or none.
    #[serde(default, skip_serializing_if = "is_false")]
    pub extends: bool,
    /// The rebuild switches this target exists under (`MIR`, `IMP`, …): it joins
    /// each switch's group. Switched off, the target is not rebuilt and the file
    /// on disk stands, as for a standard target of the same group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<String>,
    /// The target is a release artefact: built by the release pipeline together
    /// with the other artefacts, in dependency order, and published with them.
    /// Said only of a target the standard build does not already build as one;
    /// a build of the repository's own says it of each of its artefacts.
    #[serde(default, skip_serializing_if = "is_false")]
    pub artefact: bool,
}

impl ImportSpec {
    pub fn from_plan(i: &ImportPlan) -> Self {
        ImportSpec {
            id: i.id.clone(),
            source: i.source.clone(),
            output: i.output.clone(),
            steps: i.steps.iter().map(StepEntry::from_step).collect(),
            extends: false,
            product: i.product.clone(),
            mirror_steps: i.mirror_steps.iter().map(StepEntry::from_step).collect(),
            mirror_inputs: i.mirror_inputs.clone(),
        }
    }

    /// The import as the planner and executor hold it. `source` is taken as
    /// recorded — that is what recording it means. A `-base` product is resolved
    /// to its URL before it is written down, so re-resolving here would make a
    /// hand edit to the recorded source silently ineffective; and mirroring
    /// `mondo.owl` where `mondo/mondo-base.owl` was recorded pulls MONDO's whole
    /// closure, and BFO's disjointness with it, into EFO's module.
    ///
    /// Whether the module is already on disk, and what it lacks, are facts about
    /// this checkout and are derived here rather than read.
    pub fn into_plan(self, dir: &Path, merged_cached: bool) -> ImportPlan {
        let mut plan = ImportPlan {
            id: self.id,
            source: self.source,
            output: self.output,
            steps: self.steps.into_iter().map(StepEntry::into_step).collect(),
            cached: false,
            gaps: Vec::new(),
            product: self.product,
            mirror_steps: self.mirror_steps.into_iter().map(StepEntry::into_step).collect(),
            mirror_inputs: self.mirror_inputs,
        };
        let (cached, gaps) = crate::plan::gaps::import_state(dir, &plan, merged_cached);
        plan.cached = cached;
        plan.gaps = gaps;
        plan
    }
}

impl ArtefactSpec {
    /// A target with nothing said about it yet.
    pub fn named(target: &str) -> ArtefactSpec {
        ArtefactSpec::from_plan(&ArtefactPlan {
            target: target.to_string(),
            input: None,
            needs: Vec::new(),
            order_only: Vec::new(),
            steps: Vec::new(),
            gaps: Vec::new(),
            missing_rule: false,
            intermediate: false,
            side_effect_only: false,
            stdout_file: None,
            branches: Vec::new(),
        })
    }

    /// The target as the planner and executor hold it.
    pub fn into_plan(self) -> ArtefactPlan {
        let steps: Vec<Step> = self.steps.into_iter().map(StepEntry::into_step).collect();
        let gaps = steps.iter().flat_map(|s| s.gaps()).collect();
        ArtefactPlan {
            target: self.target,
            input: self.input,
            needs: self.needs,
            order_only: self.order_only,
            steps,
            gaps,
            missing_rule: self.missing_rule,
            side_effect_only: self.side_effect_only,
            stdout_file: self.stdout_file,
            intermediate: self.intermediate,
            branches: self.branches.into_iter().map(BranchSpec::into_branch).collect(),
        }
    }

    pub fn from_plan(a: &ArtefactPlan) -> Self {
        ArtefactSpec {
            target: a.target.clone(),
            input: a.input.clone(),
            needs: a.needs.clone(),
            order_only: a.order_only.clone(),
            steps: a.steps.iter().map(StepEntry::from_step).collect(),
            missing_rule: a.missing_rule,
            side_effect_only: a.side_effect_only,
            stdout_file: a.stdout_file.clone(),
            intermediate: a.intermediate,
            branches: a
                .branches
                .iter()
                .map(|b| BranchSpec {
                    flag: b.flag.clone(),
                    value: b.value.clone(),
                    input: b.input.clone(),
                    needs: b.needs.clone(),
                    steps: b.steps.iter().map(StepEntry::from_step).collect(),
                })
                .collect(),
            extends: false,
            when: Vec::new(),
            artefact: false,
        }
    }
}

/// Whether two step lists are the same pipeline.
///
/// Compared through the serialized form, which is the definition of what a plan
/// says a step IS — two steps that write the same plan are the same step, and no
/// structural equality has to be maintained alongside the serializer to say so.
pub(crate) fn steps_differ(a: &[Step], b: &[Step]) -> bool {
    let text = |v: &[Step]| {
        serde_json::to_string(&v.iter().map(StepEntry::from_step).collect::<Vec<_>>()).ok()
    };
    text(a) != text(b)
}

impl BranchSpec {
    fn into_branch(self) -> crate::plan::Branch {
        crate::plan::Branch {
            flag: self.flag,
            value: self.value,
            input: self.input,
            needs: self.needs,
            steps: self.steps.into_iter().map(StepEntry::into_step).collect(),
        }
    }
}

/// What a target is built by under the other value of a switch. See
/// [`crate::plan::Branch`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BranchSpec {
    /// The switch this pipeline belongs to.
    pub flag: String,
    /// The value of that switch which selects it.
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepEntry>,
}

/// A step as a plan writes it: the operation, plus the per-step settings that
/// apply whatever the operation is.
///
/// Flattened, so a step stays one mapping — `op: query` with `may_fail: true`
/// beside it, not an operation nested inside a wrapper.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StepEntry {
    #[serde(flatten)]
    pub spec: StepSpec,
    /// The step's failure is not the build's. A recipe writes this `cmd || true`;
    /// see [`crate::plan::step::Step::MayFail`] for why it is a property of the
    /// one step rather than of the steps around it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub may_fail: bool,
}

impl StepEntry {
    pub fn from_step(s: &Step) -> Self {
        match s {
            Step::MayFail(inner) => {
                StepEntry { spec: StepSpec::from_step(inner), may_fail: true }
            }
            other => StepEntry { spec: StepSpec::from_step(other), may_fail: false },
        }
    }

    pub fn into_step(self) -> Step {
        let step = self.spec.into_step();
        if self.may_fail { Step::MayFail(Box::new(step)) } else { step }
    }
}

/// One step of an artefact's pipeline. Most variants are owlmake's own ops,
/// applied to the model in memory; `shell`/`fallback` carry a command line run
/// outside the pipeline, and `unsupported-subcommand` records an ontology subcommand
/// owlmake has no implementation for, so a coverage gap shows up in the plan
/// rather than vanishing from it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum StepSpec {
    /// The start of a new tool invocation: the model is re-established from this
    /// invocation's own `input` (or from nothing when it names none), never
    /// carried over from the previous command line.
    Boundary {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
    },
    /// Merge `--input` files (and their import closures) into the ontology.
    Merge {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        inputs: Vec<String>,
        /// `--collapse-import-closure` (default true). False keeps imports as
        /// declarations and a read-only reasoning closure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collapse_import_closure: Option<bool>,
    },
    /// Remove a second ontology's axioms from the current one.
    Unmerge {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        second_input: Option<String>,
    },
    /// Classify and assert inferred subsumptions.
    Reason {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoner: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        equivalent_classes_allowed: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude_tautologies: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotate_inferred_axioms: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allow_incoherent: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude_external_entities: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude_owl_thing: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remove_redundant_subclass_axioms: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create_new_ontology: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create_new_ontology_with_annotations: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude_duplicate_axioms: Option<bool>,
        /// The inference types to assert; empty is the default, `SubClass`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        axiom_generators: Vec<String>,
        /// The object properties the `PropertyAssertion` generator is
        /// restricted to; empty is every named object property.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        properties: Vec<String>,
    },
    /// Relax equivalence axioms into weaker existentials.
    Relax {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        include_subclass_of: bool,
    },
    /// Transitive reduction of the asserted hierarchy.
    Reduce {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoner: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_subproperties: Option<bool>,
    },
    /// Materialize inferred existential restrictions over the given properties.
    Materialize {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        properties: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        term_files: Vec<String>,
    },
    /// Remove axioms mentioning the selected terms/IRIs. Named `remove-terms` in
    /// the plan to distinguish it from the `remove-file` (`rm`) file operation.
    #[serde(rename = "remove-terms")]
    Remove {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        term_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        axioms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        selects: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        base_iri: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trim: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preserve_structure: Option<bool>,
        /// ROBOT `--exclude-term`/`--exclude-terms`: terms that survive whatever
        /// the selectors match.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude_terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude_term_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<bool>,
        /// ROBOT `--drop-axiom-annotations <selector>`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drop_axiom_annotations: Option<String>,
    },
    /// Keep only axioms mentioning the selected terms/IRIs.
    Filter {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        term_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        selects: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trim: Option<bool>,
        /// ROBOT `--axioms`: keep only these axiom types.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        axioms: Vec<String>,
        /// `--prefix "name: namespace"`, which is how a `--select` CURIE resolves.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prefixes: Vec<String>,
    },
    /// Add ontology annotations / set the ontology and version IRIs.
    Annotate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ontology_iri: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_iri: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        annotations: Vec<AnnotationSpec>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        link_annotations: Vec<AnnotationSpec>,
        #[serde(default, skip_serializing_if = "is_false")]
        remove_annotations: bool,
    },
    /// Re-serialize into another format (applied by the final write).
    Convert {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clean_obo: Option<String>,
        /// The step's own `-o/--output`, when it names one — see `Op::Convert`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        add_prefixes: Vec<String>,
    },
    /// SPARQL `query`: `--update` files (model transforms) and/or
    /// `--query`/`--select`/`--construct FILE OUTPUT` result files.
    Query {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        updates: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        selects: Vec<QueryResult>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        constructs: Vec<QueryResult>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        /// `-g,--use-graphs`: query the import closure, not just the root.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        use_graphs: bool,
        /// `-t,--tdb`: for a SELECT with no `ORDER BY`, order rows by each term's
        /// first appearance in the input document — see [`crate::plan::step::Op`].
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        tdb: bool,
    },
    /// Remove intermediate classes with fewer than `threshold` named subclasses
    /// (default 2), bridging the hierarchy across them. Leaves, top-level classes
    /// and the `precious` terms are kept whatever their subclass count.
    Collapse {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        precious: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        precious_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<usize>,
    },
    /// Materialise uPheno's phenotype shortcut relations from EQ definitions
    /// (`upheno:extract-upheno-relations`).
    ExtractUphenoRelations {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        relations: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        term_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        roots: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        root_files: Vec<String>,
    },
    /// Replace temporary IDs with definitive ones from a named ID range
    /// (KGCL `mint`).
    Mint {
        temp_id_prefix: String,
        id_range_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id_ranges: Option<String>,
    },
    /// Inject subset / synonym-type subproperty declarations.
    Normalize {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        base_iris: Vec<String>,
        #[serde(default)]
        subset_decls: bool,
        #[serde(default)]
        synonym_decls: bool,
        #[serde(default)]
        add_source: bool,
    },
    /// A prefix binding stated by the launcher, before any subcommand; it binds
    /// for the whole chain and the written document declares it.
    AddPrefix {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prefixes: Vec<String>,
    },
    /// Generate axioms from template tables — TSV/CSV carrying a row of template
    /// strings over a table of terms — and merge them in.
    Template {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        templates: Vec<String>,
        #[serde(default)]
        merge: bool,
        /// ROBOT `--prefix "foo: http://bar"` bindings for the template's header
        /// CURIEs. They decide which IRI a column asserts, so they are plan
        /// content, not a run input.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prefixes: Vec<String>,
    },
    /// Rewrite entity IRIs from a mapping file. Named `rename-terms` in the plan
    /// to make clear it renames ontology entities, not files.
    #[serde(rename = "rename-terms")]
    Rename {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mappings: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix_mappings: Option<String>,
        #[serde(default)]
        allow_missing: bool,
    },
    /// Extract a module for a seed term set.
    Extract {
        method: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        term_files: Vec<String>,
        #[serde(default)]
        copy_ontology_annotations: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        individuals: Option<String>,
        /// MIREOT `--branch-from-term`/`--branch-from-terms` roots.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        branch_from_terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        branch_from_term_files: Vec<String>,
    },
    /// Write the model to `output` and read it back — the round trip a recipe
    /// performs when one step writes a file and a later step reads it in again.
    /// Recorded because it is not identity: an RDF/XML write derives an `xmlns`
    /// block the in-memory model never had.
    RoundTrip { output: String },
    /// Fix mechanical problems (dangling references / duplicate axioms).
    Repair {
        #[serde(default, skip_serializing_if = "is_false")]
        invalid_references: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        merge_axiom_annotations: bool,
    },
    /// Collapse equivalent-class cliques by IRI-prefix priority.
    MergeEquivalentSets {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        set_prefix: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        label_prefix: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        definition_prefix: Vec<String>,
    },
    /// Convert a Babelon translation TSV into OWL annotation axioms.
    Babelon {
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// `--output-format`: `owl` (default) or `json` (the babelon profile).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    /// Expand OBO/OWL macros (`IAO:0000424`).
    Expand {
        /// The ALLOW-list: with one, only these properties' macros are expanded.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        expand_terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        expand_term_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        no_expand_terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        no_expand_term_files: Vec<String>,
    },
    /// Extract an `oboInOwl:inSubset` slice, or a query-mode slice (`odk:subset`).
    Subset {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        subset: String,
        /// Manchester DL queries seeding a QUERY-mode subset.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        queries: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        term_files: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoner: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ancestors: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill_gaps: Option<bool>,
    },
    /// Fold species-specific classes into a composite ontology (`uberon:merge-species`).
    MergeSpecies {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch_file: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        extended: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        gca_translate: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        gca_delete: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        remove_declarations: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        taxon: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        properties: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        included: Vec<String>,
    },
    /// Regenerate textual definitions (FlyBase `rewrite-def`).
    RewriteDef {
        #[serde(default, skip_serializing_if = "is_false")]
        sub: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        dot: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        null_definitions: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        no_ids: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        include_obsolete: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter_prefix: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        add_annotation: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        add_annotation_iri: Vec<String>,
    },
    /// The simple/"basic" core-class subset: keep only classes in the ontology's
    /// own OBO ID-space, dropping axioms that reference an external class.
    SimpleSubset { ont_id: String },
    /// `--extract-ontology-subset [--fill-gaps] --subset NAME`: the named
    /// `oboInOwl:inSubset` slice, extended to its full graph-ancestor closure when
    /// `fill_gaps` is set, with whatever the slice leaves dangling pruned.
    ExtractOntologySubset { subset: String, fill_gaps: bool },
    /// `--extract-mingraph`: reduce the ontology to a minimal graph — class
    /// hierarchy, class labels and the property ontology — dropping every axiom
    /// that references an obsolete class.
    ExtractMingraph,
    /// `--remove-axiom-annotations`: strip the annotations carried on each axiom,
    /// keeping the axiom itself.
    RemoveAxiomAnnotations,
    /// `--make-subset-by-properties [-f] PROPS…`: drop every axiom using an object
    /// property outside `properties`, first weakening a named-subject existential
    /// `SubClassOf` onto each super-property that is inside the set.
    MakeSubsetByProperties {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        properties: Vec<String>,
    },
    /// A recipe line run as a command line: text processors, control flow, and
    /// anything else with an effect owlmake does not model as an op.
    Shell {
        command: String,
        /// Command words owlmake cannot vouch for: not bundled, not a POSIX text
        /// tool it knows. They must be present in the build environment.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires: Vec<String>,
    },
    /// A shell command run ONLY IF the preceding step failed — the right-hand
    /// side of `||`. See [`crate::plan::step::Step::Fallback`].
    Fallback {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requires: Vec<String>,
    },
    /// The bundled jq engine (`owlmake jq`) with its argument tokens.
    Jq { args: Vec<String> },
    /// The bundled SSSOM CLI (`owlmake sssom`) with its argument tokens
    /// (including the `sssom`/`sssom:<cmd>` launcher).
    Sssom { args: Vec<String> },
    /// `cp [-r] SRC… DST` — a native file copy. `relative` is `rsync -R`'s
    /// relative mode: each source's own relative path is recreated under the
    /// destination.
    CopyFile {
        src: Vec<String>,
        dst: String,
        #[serde(default, skip_serializing_if = "is_false")]
        recursive: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        relative: bool,
    },
    /// `mv SRC… DST` — a native file move.
    MoveFile { src: Vec<String>, dst: String },
    /// `rm [-r] [-f] PATH…` — a native file removal.
    RemoveFile {
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        recursive: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
    },
    /// `mkdir [-p] PATH…` — a native directory creation.
    Mkdir {
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        parents: bool,
    },
    /// `touch PATH…` — a native file touch.
    Touch { paths: Vec<String> },
    /// `wget URL -O DST` / `curl URL -o DST` — fetch a URL to a file over
    /// owlmake's own HTTP client.
    Fetch { url: String, dst: String },
    /// `babelon merge` — concatenate translation tables into one TSV.
    #[serde(rename = "babelon-merge")]
    BabelonMerge {
        inputs: Vec<String>,
        output: String,
        #[serde(default)]
        sort_tables: bool,
        #[serde(default)]
        drop_unknown_columns: bool,
        #[serde(default)]
        update_translations: bool,
    },
    /// `babelon prepare-translation` — reconcile a translation table against the
    /// ontology it translates.
    #[serde(rename = "babelon-prepare-translation")]
    BabelonPrepare {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        oak_adapter: String,
        language_code: String,
        fields: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        term_list: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_source_changed: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_not_translated: Option<String>,
        #[serde(default)]
        include_not_translated: bool,
        #[serde(default)]
        update_translation_status: bool,
        #[serde(default)]
        sort_tables: bool,
        #[serde(default)]
        drop_unknown_columns: bool,
    },
    /// `cat SRC… >> DST` — concatenate files into `dst`, appending by default
    /// (set `overwrite` for `cat … > DST`).
    Append {
        src: Vec<String>,
        dst: String,
        #[serde(default, skip_serializing_if = "is_false")]
        overwrite: bool,
    },
    /// `sort [-u] -o OUT IN` — a deterministic, platform-independent line sort.
    Sort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        output: String,
        #[serde(default, skip_serializing_if = "is_false")]
        unique: bool,
    },
    /// `echo MSG [>|>> DST]` — print a message (to stdout, or to a file).
    Print {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        append: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        no_newline: bool,
    },
    /// A shell `if … then … [else …] fi` decomposed into a condition and the
    /// nested step lists run on success / failure (sub-steps may be branches).
    /// A branch whose condition and bodies are all native runs without a shell.
    Branch {
        #[serde(rename = "if")]
        r#if: ConditionSpec,
        #[serde(default, rename = "then", skip_serializing_if = "Vec::is_empty")]
        then_steps: Vec<StepEntry>,
        #[serde(default, rename = "else", skip_serializing_if = "Vec::is_empty")]
        else_steps: Vec<StepEntry>,
    },
    /// An ontology subcommand a recipe names that owlmake does not implement (a
    /// coverage gap).
    UnsupportedSubcommand { command: String },
    /// An ontology subcommand owlmake implements on its CLI but not as a pipeline
    /// op; executed by invoking the owlmake binary's matching subcommand with
    /// `args` (the invocation's own option tokens, in argv order).
    OwlmakeCli {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
    /// A release-runner invocation: one command that emits a whole set of release
    /// variants from one input. Normally rewritten into the artefacts it produces
    /// before the plan is written; recorded in full when one survives, so it does
    /// not degrade into an uncovered shell command.
    Oort {
        input: String,
        outdir: String,
        reasoner: String,
        #[serde(default, skip_serializing_if = "is_false")]
        simple: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        relaxed: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        asserted: bool,
    },
}

/// An annotation property/value pair.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnnotationSpec {
    pub property: String,
    pub value: String,
}

// --- Conversions: runtime Plan → spec ------------------------------------------

impl OwlmakeSpec {
    /// The whole of a [`Plan`] as a file states it: every target, import and
    /// setting, with no standard build underneath (`use_builtin_rules: false`), so
    /// that [`into_plan`](Self::into_plan) reads it back without deriving
    /// anything. Diagnostic fields (coverage gaps, cache state) are dropped —
    /// they are re-derived on load.
    pub fn from_plan(plan: &Plan) -> Self {
        let mut targets: Vec<ArtefactSpec> =
            plan.prerequisites.iter().map(ArtefactSpec::from_plan).collect();
        targets.extend(plan.artefacts.iter().map(|a| ArtefactSpec {
            artefact: true,
            ..ArtefactSpec::from_plan(a)
        }));
        OwlmakeSpec {
            // No per-repo `$schema` pointer: the schema is the same for every
            // `owlmake.yaml` and is emitted on demand by `owlmake schema`. Users
            // who want editor validation can add a `$schema` pointing at a shared
            // copy; the loader validates against the built-in schema regardless.
            schema: None,
            use_builtin_rules: false,
            default_targets: Some(plan.default_targets.clone()),
            phony: plan.phony.clone(),
            transient_targets: plan.transient_targets.clone(),
            native_targets: plan.native_targets.clone(),
            edit_file: plan.edit_file.clone(),
            catalog_file: plan.catalog_file.clone(),
            dosdp: plan.dosdp.clone(),
            emulate_odk_version: plan.emulate_odk_version.map(format_version),
            // The tool generation is written only where the release does not
            // already imply it, so a file never says one thing twice.
            emulate_robot_version: (plan.emulate_robot_version
                != implied_robot_version(plan.emulate_odk_version))
            .then(|| format_version(plan.emulate_robot_version)),
            strict: plan.strict,
            xml_entities: plan.xml_entities,
            refresh_groups: Some(plan.refresh_groups.clone()),
            gating_flags: plan.gating_flags.clone(),
            // The FORMAT floor, not the generator's version. Stamping
            // `CARGO_PKG_VERSION` would make every plan claim a minimum it does
            // not have, so a plan generated by 0.9.3 would refuse to load on 0.9.2
            // even when the format had not moved since 0.8. See
            // `PLAN_FORMAT_MIN_VERSION`.
            min_owlmake_version: Some(PLAN_FORMAT_MIN_VERSION.to_string()),
            id: plan.id.clone(),
            version: plan.version.clone(),
            version_file: plan.version_file.clone(),
            ontology_iri: plan.ontology_iri.clone(),
            reasoner: plan.reasoner.clone(),
            use_base_merging: plan.use_base_merging,
            exclude_iri_patterns: plan.exclude_iri_patterns.clone(),
            slme_individuals: plan.slme_individuals.clone(),
            imports: plan.imports.iter().map(ImportSpec::from_plan).collect(),
            merged_import: plan.merged_import.clone(),
            merged_import_iri: plan.merged_import_iri.clone(),
            merged_import_shards: plan.merged_import_shards.clone(),
            merged_import_shard_bytes: plan.merged_import_shard_bytes,
            targets,
            options: Default::default(),
            variables: plan.variables.clone(),
            component_gaps: plan.component_gaps.clone(),
        }
    }

    /// The [`Plan`] this spec states, read as a complete statement: nothing is
    /// derived, so this is the reverse of [`from_plan`](Self::from_plan) and the
    /// in-memory round trip a run binds its version through. A file is not read
    /// this way — the standard build for its options is resolved first and the
    /// file laid over it (`OdkRepo::from_file`).
    ///
    /// `dir` is the ONTOLOGY DIRECTORY, and it is used only to observe which
    /// files are already on disk — a fact about this filesystem right now, not a
    /// build instruction. Nothing else about the repository is consulted: taking
    /// an `&OdkRepo` and re-deriving gaps through `MakeModel::rule_for` would make
    /// loading a committed plan read build information from outside the plan, and
    /// would report gaps for term files a repo's own rules build once that
    /// information is no longer there. The parameter is a `&Path` so that cannot
    /// happen.
    pub fn into_plan(self, dir: &Path) -> Plan {
        let merged_cached =
            self.use_base_merging && dir.join("imports/merged_import.owl").exists();
        let imports: Vec<ImportPlan> = self
            .imports
            .into_iter()
            .map(|i| i.into_plan(dir, merged_cached))
            .collect();

        // Gaps are diagnostics about THIS filesystem, so they are re-derived here
        // rather than serialized: a plan says what the build needs, and whether a
        // needed file is present is a question only the checkout can answer, and
        // answers differently tomorrow.
        //
        // Both kinds are derived, over the same target set the planner uses, so a
        // committed plan and the repo it came from reach the same verdict. A
        // missing input is otherwise reported only while the repo still has the
        // files ingest read — that is, everywhere except the plan-only build the
        // check exists for, where a declared source deleted after planning would
        // leave a stale output standing and the build calling it up to date.
        let (mut prerequisites, mut artefacts): (Vec<ArtefactPlan>, Vec<ArtefactPlan>) =
            (Vec::new(), Vec::new());
        for t in self.targets {
            let is_artefact = t.artefact;
            let planned = t.into_plan();
            if is_artefact {
                artefacts.push(planned);
            } else {
                prerequisites.push(planned);
            }
        }
        {
            // Everything the build produces, by any route: a recorded rule, one of
            // owlmake's own engines, or another rule's recipe writing a file it
            // does not name as its target.
            let mut planned: std::collections::HashSet<String> = artefacts
                .iter()
                .chain(prerequisites.iter())
                .filter(|a| !a.missing_rule)
                .map(|a| a.target.clone())
                .collect();
            planned.extend(self.native_targets.iter().cloned());
            for a in artefacts.iter().chain(prerequisites.iter()) {
                planned.extend(crate::plan::gaps::recipe_outputs(&a.steps));
            }
            let phony: std::collections::HashSet<String> = self.phony.iter().cloned().collect();
            let products: Vec<String> =
                imports.iter().filter(|i| !i.steps.is_empty()).map(|i| i.output.clone()).collect();
            for a in artefacts.iter_mut().chain(prerequisites.iter_mut()) {
                a.gaps.extend(crate::plan::gaps::term_file_gaps(dir, &a.steps, &planned));
                a.gaps.extend(crate::plan::gaps::prerequisite_gaps(
                    dir, &a.needs, &planned, &phony, &products,
                ));
            }
        }

        let emulate_odk_version = self.emulate_odk_version.as_deref().and_then(parse_version);
        Plan {
            default_targets: self.default_targets.unwrap_or_default(),
            phony: self.phony,
            transient_targets: self.transient_targets,
            native_targets: self.native_targets,
            edit_file: self.edit_file,
            catalog_file: self.catalog_file,
            dosdp: self.dosdp,
            emulate_odk_version,
            emulate_robot_version: self
                .emulate_robot_version
                .as_deref()
                .and_then(parse_version)
                .unwrap_or_else(|| implied_robot_version(emulate_odk_version)),
            strict: self.strict,
            xml_entities: self.xml_entities,
            refresh_groups: self.refresh_groups.unwrap_or_default(),
            gating_flags: self.gating_flags,
            id: self.id,
            version: self.version,
            version_file: self.version_file,
            ontology_iri: self.ontology_iri,
            reasoner: self.reasoner,
            use_base_merging: self.use_base_merging,
            exclude_iri_patterns: self.exclude_iri_patterns,
            slme_individuals: self.slme_individuals,
            imports,
            merged_import: self.merged_import,
            merged_import_iri: self.merged_import_iri,
            merged_import_shards: self.merged_import_shards,
            merged_import_shard_bytes: self.merged_import_shard_bytes,
            variables: self.variables,
            component_gaps: self.component_gaps,
            prerequisites,
            artefacts,
        }
    }

    /// Lay this file's statements over `plan`, the standard build resolved for
    /// its options with its targets, imports and switches already threaded in
    /// (`OdkRepo::from_file`). What remains is the settings the rules never
    /// decide: each replaces what was derived, or joins it where the field is a
    /// list the file may only add to.
    ///
    /// The output conventions are the file's whatever the rules derived: absent,
    /// they are owlmake's own, which is what a repository that deletes its
    /// `emulate_odk_version` line is asking for.
    pub fn overlay(&self, plan: &mut Plan) {
        if !self.version.is_empty() {
            plan.version = self.version.clone();
        }
        if self.version_file.is_some() {
            plan.version_file = self.version_file.clone();
        }
        if !self.ontology_iri.is_empty() {
            plan.ontology_iri = self.ontology_iri.clone();
        }
        if !self.reasoner.is_empty() {
            plan.reasoner = self.reasoner.clone();
        }
        if self.edit_file.is_some() {
            plan.edit_file = self.edit_file.clone();
        }
        if self.catalog_file.is_some() {
            plan.catalog_file = self.catalog_file.clone();
        }
        if let Some(targets) = &self.default_targets {
            plan.default_targets = targets.clone();
        }
        if let Some(groups) = &self.refresh_groups {
            plan.refresh_groups = groups.clone();
        }
        for (name, value) in &self.gating_flags {
            plan.gating_flags.insert(name.clone(), value.clone());
        }
        for (name, value) in &self.variables {
            plan.variables.insert(name.clone(), value.clone());
        }
        let join = |into: &mut Vec<String>, from: &[String]| {
            for t in from {
                if !into.contains(t) {
                    into.push(t.clone());
                }
            }
            into.sort();
        };
        join(&mut plan.phony, &self.phony);
        join(&mut plan.transient_targets, &self.transient_targets);
        join(&mut plan.native_targets, &self.native_targets);
        for gap in &self.component_gaps {
            if !plan.component_gaps.contains(gap) {
                plan.component_gaps.push(gap.clone());
            }
        }
        plan.strict |= self.strict;
        plan.xml_entities |= self.xml_entities;
        plan.emulate_odk_version = self.emulate_odk_version.as_deref().and_then(parse_version);
        plan.emulate_robot_version = self
            .emulate_robot_version
            .as_deref()
            .and_then(parse_version)
            .unwrap_or_else(|| implied_robot_version(plan.emulate_odk_version));
        if self.use_base_merging {
            plan.use_base_merging = true;
        }
        if !self.exclude_iri_patterns.is_empty() {
            plan.exclude_iri_patterns = self.exclude_iri_patterns.clone();
        }
        if self.slme_individuals.is_some() {
            plan.slme_individuals = self.slme_individuals.clone();
        }
        if self.merged_import.is_some() {
            plan.merged_import = self.merged_import.clone();
        }
        if self.merged_import_iri.is_some() {
            plan.merged_import_iri = self.merged_import_iri.clone();
        }
        if self.merged_import_shards.is_some() {
            plan.merged_import_shards = self.merged_import_shards.clone();
        }
        if self.merged_import_shard_bytes.is_some() {
            plan.merged_import_shard_bytes = self.merged_import_shard_bytes;
        }
    }
}

/// The tool generation an emulated release implies, or the current one where no
/// release is emulated.
fn implied_robot_version(odk: Option<(u32, u32, u32)>) -> (u32, u32, u32) {
    odk.map_or(CURRENT_ROBOT, crate::odk::workflows::odk_robot_version)
}

/// Bind a run's release version into a plan.
///
/// The plan states the version once, in its `version` field, and refers to it as
/// [`crate::plan::VERSION_REF`] everywhere else — in ontology and version IRIs,
/// in the `owl:versionInfo` literal an annotate step stamps, in the recorded
/// `ANNOTATE_ONTOLOGY_VERSION` a recipe expands as it runs. This replaces every
/// reference with the version this run stamps and records it as the plan's own,
/// so everything downstream reads an ordinary string.
///
/// The substitution runs over the SERIALIZED plan rather than over a list of
/// fields, so a step or an option added later is covered without anyone
/// remembering to add it here.
pub fn bind_version(
    plan: &Plan,
    version: &str,
    today: Option<&str>,
    clock: Option<&str>,
    dir: &Path,
) -> Result<Plan> {
    let spec = OwlmakeSpec::from_plan(plan);
    let mut value = serde_json::to_value(&spec)
        .context("internal: a plan did not serialize while binding its release version")?;
    substitute(&mut value, crate::plan::VERSION_REF, version);
    // A recipe that reads the calendar date directly refers to it as
    // [`crate::plan::VERSION_TODAY`], which is the day the build runs whatever
    // version the run stamps — uPheno's pattern ontology names both, one in its
    // version IRI and the other in the artefacts around it.
    //
    // It is a RUN INPUT, so it comes from the run when the run named one and from
    // the clock only when it did not. Reading the clock unconditionally ignored
    // `TODAY=` for every string built from `{today}` while honouring it for every
    // string built from `{version}`: MONDO's mondo.owl took the wall-clock date in
    // its versionIRI, one line of a 254 MB file, on a build that passed
    // TODAY=2026-08-19 across midnight.
    let today = today.map(str::to_string).unwrap_or_else(crate::plan::today);
    substitute(&mut value, crate::plan::VERSION_TODAY, &today);
    // …and the clock is the clock, unless the run names it: a recipe that
    // shells out to `date` gets the day the build runs whatever version it
    // stamps, and `CLOCK=` reproduces such a build on any later day.
    let clock = clock.map(str::to_string).unwrap_or_else(crate::plan::today);
    substitute(&mut value, crate::plan::VERSION_CLOCK, &clock);
    let mut bound: OwlmakeSpec = serde_json::from_value(value)
        .context("internal: a plan did not read back while binding its release version")?;
    bound.version = version.to_string();
    Ok(bound.into_plan(dir))
}

/// Select, for each target, the recipe this run's switches call for.
///
/// A target whose recipe differs by branch carries both; this swaps in the one
/// the run asked for and drops the target from that switch's refresh group,
/// because a target with a recipe under this value is BUILT, not pinned. Targets
/// the other branch leaves ruleless keep their group membership and are pinned as
/// before — one switch does both, and the plan says which per target.
///
/// `switches` is every switch this run resolved, as `(flag, value)`.
pub fn bind_switches(mut plan: Plan, switches: &[(String, String)]) -> Plan {
    let chosen = |branches: &[crate::plan::Branch]| -> Option<crate::plan::Branch> {
        branches
            .iter()
            .find(|b| {
                switches.iter().any(|(f, v)| {
                    *f == b.flag && crate::plan::is_on(v) == crate::plan::is_on(&b.value)
                })
            })
            .cloned()
    };
    let mut taken: Vec<(String, String)> = Vec::new();
    for a in plan.artefacts.iter_mut().chain(plan.prerequisites.iter_mut()) {
        let Some(b) = chosen(&a.branches) else {
            a.branches.clear();
            continue;
        };
        taken.push((b.flag.clone(), a.target.clone()));
        a.input = b.input;
        a.needs = b.needs;
        a.gaps = b.steps.iter().flat_map(|s| s.gaps()).collect();
        a.steps = b.steps;
        a.missing_rule = false;
        a.branches.clear();
    }
    for (flag, target) in taken {
        for g in plan.refresh_groups.iter_mut().filter(|g| g.flag == flag) {
            g.targets.retain(|t| t != &target);
        }
    }
    plan
}

/// Replace every occurrence of `from` with `to` in every string of a value tree.
fn substitute(value: &mut serde_json::Value, from: &str, to: &str) {
    match value {
        serde_json::Value::String(s) => {
            if s.contains(from) {
                *s = s.replace(from, to);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|v| substitute(v, from, to)),
        serde_json::Value::Object(o) => o.iter_mut().for_each(|(_, v)| substitute(v, from, to)),
        _ => {}
    }
}

// --- Conversions: Step ↔ StepSpec ---------------------------------------------

impl StepSpec {
    pub(crate) fn from_step(step: &Step) -> Self {
        match step {
            // An Op or a Partial both serialize by their operation; partial-ness
            // (the coverage gaps) is re-derived on load from the op's options.
            Step::Op(op) | Step::Partial { op, .. } => Self::from_op(op),
            // `may_fail` is a field of the step's own mapping, so it is written by
            // [`StepEntry`] — the only thing that builds one of these — and what
            // remains here is the operation it applies to.
            Step::MayFail(inner) => Self::from_step(inner),
            Step::Boundary { input } => StepSpec::Boundary { input: input.clone() },
            // `Inert` never reaches a plan (the planner drops it); mapped for
            // exhaustiveness only.
            Step::Inert(c) => StepSpec::Shell { command: c.clone(), requires: vec![] },
            Step::Shell { command, requires } => {
                StepSpec::Shell { command: command.clone(), requires: requires.clone() }
            }
            Step::Fallback { command, requires } => {
                StepSpec::Fallback { command: command.clone(), requires: requires.clone() }
            }
            Step::Jq(args) => StepSpec::Jq { args: args.clone() },
            Step::Sssom(args) => StepSpec::Sssom { args: args.clone() },
            Step::File(op) => StepSpec::from_file_op(op),

            Step::Branch { condition, then_steps, else_steps } => StepSpec::Branch {
                r#if: ConditionSpec::from_condition(condition),
                then_steps: then_steps.iter().map(StepEntry::from_step).collect(),
                else_steps: else_steps.iter().map(StepEntry::from_step).collect(),
            },
            Step::UnsupportedSubcommand(c) => StepSpec::UnsupportedSubcommand { command: c.clone() },
            Step::OwlmakeCli { name, args } => {
                StepSpec::OwlmakeCli { command: name.clone(), args: args.clone() }
            }

            // The release-runner marker is resolved away by `rewrite_oort` before
            // a plan is persisted; if one survives (no consumer), record it
            // faithfully.
            Step::Oort(s) => StepSpec::Oort {
                input: s.input.clone(),
                outdir: s.outdir.clone(),
                reasoner: s.reasoner.clone(),
                simple: s.simple,
                relaxed: s.relaxed,
                asserted: s.asserted,
            },
        }
    }

    fn from_file_op(op: &FileOp) -> Self {
        match op {
            FileOp::Copy { src, dst, recursive, relative } => StepSpec::CopyFile {
                src: src.clone(),
                dst: dst.clone(),
                recursive: *recursive,
                relative: *relative,
            },
            FileOp::Move { src, dst } => StepSpec::MoveFile { src: src.clone(), dst: dst.clone() },
            FileOp::Remove { paths, recursive, force } => StepSpec::RemoveFile {
                paths: paths.clone(),
                recursive: *recursive,
                force: *force,
            },
            FileOp::Mkdir { paths, parents } => StepSpec::Mkdir { paths: paths.clone(), parents: *parents },
            FileOp::Touch { paths } => StepSpec::Touch { paths: paths.clone() },
            FileOp::Fetch { url, dst } => StepSpec::Fetch { url: url.clone(), dst: dst.clone() },
            // Both babelon table ops round-trip through the plan verbatim: the
            // spec mirrors the op field for field.
            FileOp::BabelonMerge {
                inputs,
                output,
                sort_tables,
                drop_unknown_columns,
                update_translations,
            } => StepSpec::BabelonMerge {
                inputs: inputs.clone(),
                output: output.clone(),
                sort_tables: *sort_tables,
                drop_unknown_columns: *drop_unknown_columns,
                update_translations: *update_translations,
            },
            FileOp::BabelonPrepare {
                input,
                oak_adapter,
                language_code,
                fields,
                term_list,
                output,
                output_source_changed,
                output_not_translated,
                include_not_translated,
                update_translation_status,
                sort_tables,
                drop_unknown_columns,
            } => StepSpec::BabelonPrepare {
                input: input.clone(),
                oak_adapter: oak_adapter.clone(),
                language_code: language_code.clone(),
                fields: fields.clone(),
                term_list: term_list.clone(),
                output: output.clone(),
                output_source_changed: output_source_changed.clone(),
                output_not_translated: output_not_translated.clone(),
                include_not_translated: *include_not_translated,
                update_translation_status: *update_translation_status,
                sort_tables: *sort_tables,
                drop_unknown_columns: *drop_unknown_columns,
            },
            FileOp::Concat { src, dst, append } => StepSpec::Append {
                src: src.clone(),
                dst: dst.clone(),
                overwrite: !*append,
            },
            FileOp::Sort { input, output, unique } => StepSpec::Sort {
                input: input.clone(),
                output: output.clone(),
                unique: *unique,
            },
            FileOp::Print { message, dst, append, newline } => StepSpec::Print {
                message: message.clone(),
                dst: dst.clone(),
                append: *append,
                no_newline: !*newline,
            },
        }
    }

    fn from_op(op: &Op) -> Self {
        match op {
            Op::Merge { inputs, collapse_import_closure } => StepSpec::Merge {
                inputs: inputs.clone(),
                collapse_import_closure: *collapse_import_closure,
            },
            Op::Unmerge { second_input } => StepSpec::Unmerge { second_input: second_input.clone() },
            Op::Reason {
                reasoner,
                equivalent_classes_allowed,
                exclude_tautologies,
                annotate_inferred_axioms,
                allow_incoherent,
                exclude_external_entities,
                exclude_owl_thing,
                remove_redundant_subclass_axioms,
                create_new_ontology,
                create_new_ontology_with_annotations,
                exclude_duplicate_axioms,
                axiom_generators,
                properties,
            } => StepSpec::Reason {
                reasoner: reasoner.clone(),
                equivalent_classes_allowed: equivalent_classes_allowed.clone(),
                exclude_tautologies: exclude_tautologies.clone(),
                annotate_inferred_axioms: *annotate_inferred_axioms,
                allow_incoherent: *allow_incoherent,
                exclude_external_entities: *exclude_external_entities,
                exclude_owl_thing: *exclude_owl_thing,
                remove_redundant_subclass_axioms: *remove_redundant_subclass_axioms,
                create_new_ontology: *create_new_ontology,
                create_new_ontology_with_annotations: *create_new_ontology_with_annotations,
                exclude_duplicate_axioms: *exclude_duplicate_axioms,
                axiom_generators: axiom_generators.clone(),
                properties: properties.clone(),
            },
            Op::Relax { include_subclass_of } => {
                StepSpec::Relax { include_subclass_of: *include_subclass_of }
            }
            Op::Reduce { reasoner, include_subproperties } => StepSpec::Reduce {
                reasoner: reasoner.clone(),
                include_subproperties: *include_subproperties,
            },
            Op::Materialize { properties, term_files } => StepSpec::Materialize {
                properties: properties.clone(),
                term_files: term_files.clone(),
            },
            Op::Remove(s) => StepSpec::Remove {
                terms: s.terms.clone(),
                term_files: s.term_files.clone(),
                axioms: s.axioms.clone(),
                selects: s.selects.clone(),
                base_iri: s.base_iri.clone(),
                trim: s.trim,
                preserve_structure: s.preserve_structure,
                exclude_terms: s.exclude_terms.clone(),
                exclude_term_files: s.exclude_term_files.clone(),
                signature: s.signature,
                drop_axiom_annotations: s.drop_axiom_annotations.clone(),
            },
            Op::Filter(s) => StepSpec::Filter {
                terms: s.terms.clone(),
                term_files: s.term_files.clone(),
                selects: s.selects.clone(),
                signature: s.signature,
                trim: s.trim,
                axioms: s.axioms.clone(),
                prefixes: s.prefixes.clone(),
            },
            Op::Annotate(s) => StepSpec::Annotate {
                ontology_iri: s.ontology_iri.clone(),
                version_iri: s.version_iri.clone(),
                annotations: s.annotations.iter().map(AnnotationSpec::from_pair).collect(),
                link_annotations: s.link_annotations.iter().map(AnnotationSpec::from_pair).collect(),
                remove_annotations: s.remove_annotations,
            },
            Op::Convert { format, clean_obo, output, add_prefixes } => StepSpec::Convert {
                format: format.clone(),
                clean_obo: clean_obo.clone(),
                output: output.clone(),
                add_prefixes: add_prefixes.clone(),
            },
            Op::Template { templates, merge, prefixes } => StepSpec::Template {
                templates: templates.clone(),
                merge: *merge,
                prefixes: prefixes.clone(),
            },
            Op::Rename { mappings, prefix_mappings, allow_missing } => StepSpec::Rename {
                mappings: mappings.clone(),
                prefix_mappings: prefix_mappings.clone(),
                allow_missing: *allow_missing,
            },
            Op::RoundTrip { path } => StepSpec::RoundTrip { output: path.clone() },
            Op::Extract {
                method, terms, term_files, copy_ontology_annotations, individuals,
                branch_from_terms, branch_from_term_files,
            } => {
                StepSpec::Extract {
                    method: method.clone(),
                    terms: terms.clone(),
                    term_files: term_files.clone(),
                    copy_ontology_annotations: *copy_ontology_annotations,
                    individuals: individuals.clone(),
                    branch_from_terms: branch_from_terms.clone(),
                    branch_from_term_files: branch_from_term_files.clone(),
                }
            }
            Op::Collapse { precious, precious_files, threshold } => StepSpec::Collapse {
                precious: precious.clone(),
                precious_files: precious_files.clone(),
                threshold: *threshold,
            },
            Op::ExtractUphenoRelations { relations, terms, term_files, roots, root_files } => {
                StepSpec::ExtractUphenoRelations {
                    relations: relations.clone(),
                    terms: terms.clone(),
                    term_files: term_files.clone(),
                    roots: roots.clone(),
                    root_files: root_files.clone(),
                }
            }
            Op::Mint { temp_id_prefix, id_range_name, id_ranges } => StepSpec::Mint {
                temp_id_prefix: temp_id_prefix.clone(),
                id_range_name: id_range_name.clone(),
                id_ranges: id_ranges.clone(),
            },
            Op::AddPrefix { prefixes } => StepSpec::AddPrefix { prefixes: prefixes.clone() },
            Op::Normalize { base_iris, subset_decls, synonym_decls, add_source } => StepSpec::Normalize {
                base_iris: base_iris.clone(),
                subset_decls: *subset_decls,
                synonym_decls: *synonym_decls,
                add_source: *add_source,
            },
            Op::Query { updates, selects, constructs, format, use_graphs, tdb } => StepSpec::Query {
                updates: updates.clone(),
                selects: selects.iter().map(QueryResult::from_pair).collect(),
                constructs: constructs.iter().map(QueryResult::from_pair).collect(),
                format: format.clone(),
                use_graphs: *use_graphs,
                tdb: *tdb,
            },
            Op::Repair { invalid_references, merge_axiom_annotations } => {
                StepSpec::Repair {
                    invalid_references: *invalid_references,
                    merge_axiom_annotations: *merge_axiom_annotations,
                }
            }
            Op::MergeEquivalentSets { set_prefix, label_prefix, definition_prefix } => {
                StepSpec::MergeEquivalentSets {
                    set_prefix: set_prefix.clone(),
                    label_prefix: label_prefix.clone(),
                    definition_prefix: definition_prefix.clone(),
                }
            }
            Op::Babelon { input, output, format } => {
                StepSpec::Babelon {
                    input: input.clone(),
                    output: output.clone(),
                    format: format.clone(),
                }
            }
            Op::Expand { expand_terms, expand_term_files, no_expand_terms, no_expand_term_files } => {
                StepSpec::Expand {
                    expand_terms: expand_terms.clone(),
                    expand_term_files: expand_term_files.clone(),
                    no_expand_terms: no_expand_terms.clone(),
                    no_expand_term_files: no_expand_term_files.clone(),
                }
            }
            Op::Subset { subset, queries, terms, term_files, reasoner, ancestors, fill_gaps } => {
                StepSpec::Subset {
                    subset: subset.clone(),
                    queries: queries.clone(),
                    terms: terms.clone(),
                    term_files: term_files.clone(),
                    reasoner: reasoner.clone(),
                    ancestors: *ancestors,
                    fill_gaps: *fill_gaps,
                }
            }
            Op::MergeSpecies {
                batch_file, extended, gca_translate, gca_delete, remove_declarations,
                taxon, suffix, properties, included,
            } => StepSpec::MergeSpecies {
                batch_file: batch_file.clone(),
                extended: *extended,
                gca_translate: *gca_translate,
                gca_delete: *gca_delete,
                remove_declarations: *remove_declarations,
                taxon: taxon.clone(),
                suffix: suffix.clone(),
                properties: properties.clone(),
                included: included.clone(),
            },
            Op::RewriteDef(s) => StepSpec::RewriteDef {
                sub: s.sub,
                dot: s.dot,
                null_definitions: s.null_definitions,
                no_ids: s.no_ids,
                include_obsolete: s.include_obsolete,
                filter_prefix: s.filter_prefix.clone(),
                add_annotation: s.add_annotation.clone(),
                add_annotation_iri: s.add_annotation_iri.clone(),
            },
            Op::SimpleSubset { ont_id } => StepSpec::SimpleSubset { ont_id: ont_id.clone() },
            Op::ExtractOntologySubset { subset, fill_gaps } => {
                StepSpec::ExtractOntologySubset { subset: subset.clone(), fill_gaps: *fill_gaps }
            }
            Op::ExtractMingraph => StepSpec::ExtractMingraph,
            Op::RemoveAxiomAnnotations => StepSpec::RemoveAxiomAnnotations,
            Op::MakeSubsetByProperties { properties } => {
                StepSpec::MakeSubsetByProperties { properties: properties.clone() }
            }
        }
    }

    pub(crate) fn into_step(self) -> Step {
        match self {
            StepSpec::Boundary { input } => Step::Boundary { input },
            StepSpec::Merge { inputs, collapse_import_closure } => {
                Step::Op(Op::Merge { inputs, collapse_import_closure })
            }
            StepSpec::Unmerge { second_input } => Step::Op(Op::Unmerge { second_input }),
            StepSpec::Reason {
                reasoner,
                equivalent_classes_allowed,
                exclude_tautologies,
                annotate_inferred_axioms,
                allow_incoherent,
                exclude_external_entities,
                exclude_owl_thing,
                remove_redundant_subclass_axioms,
                create_new_ontology,
                create_new_ontology_with_annotations,
                exclude_duplicate_axioms,
                axiom_generators,
                properties,
            } => Step::Op(Op::Reason {
                reasoner,
                equivalent_classes_allowed,
                exclude_tautologies,
                annotate_inferred_axioms,
                allow_incoherent,
                exclude_external_entities,
                exclude_owl_thing,
                remove_redundant_subclass_axioms,
                create_new_ontology,
                create_new_ontology_with_annotations,
                exclude_duplicate_axioms,
                axiom_generators,
                properties,
            }),
            StepSpec::Relax { include_subclass_of } => {
                Step::Op(Op::Relax { include_subclass_of })
            }
            StepSpec::Reduce { reasoner, include_subproperties } => {
                Step::Op(Op::Reduce { reasoner, include_subproperties })
            }
            StepSpec::Materialize { properties, term_files } => {
                Step::Op(Op::Materialize { properties, term_files })
            }
            // remove/filter recompute their partial-ness from the option set.
            StepSpec::Remove {
                terms, term_files, axioms, selects, base_iri, trim, preserve_structure,
                exclude_terms, exclude_term_files, signature, drop_axiom_annotations,
            } => step::remove_step(RemoveSpec {
                terms,
                term_files,
                axioms,
                selects,
                base_iri,
                trim,
                preserve_structure,
                exclude_terms,
                exclude_term_files,
                signature,
                drop_axiom_annotations,
            }),
            StepSpec::Filter { terms, term_files, selects, signature, trim, axioms, prefixes } => {
                step::filter_step(FilterSpec {
                    terms,
                    term_files,
                    selects,
                    signature,
                    trim,
                    axioms,
                    prefixes,
                })
            }
            StepSpec::Annotate {
                ontology_iri,
                version_iri,
                annotations,
                link_annotations,
                remove_annotations,
            } => Step::Op(Op::Annotate(AnnotateSpec {
                ontology_iri,
                version_iri,
                annotations: annotations.into_iter().map(AnnotationSpec::into_pair).collect(),
                link_annotations: link_annotations.into_iter().map(AnnotationSpec::into_pair).collect(),
                remove_annotations,
            })),
            StepSpec::Convert { format, clean_obo, output, add_prefixes } => {
                Step::Op(Op::Convert { format, clean_obo, output, add_prefixes })
            }
            StepSpec::Query { updates, selects, constructs, format, use_graphs, tdb } => Step::Op(Op::Query {
                updates,
                selects: selects.into_iter().map(QueryResult::into_pair).collect(),
                constructs: constructs.into_iter().map(QueryResult::into_pair).collect(),
                format,
                use_graphs,
                tdb,
            }),
            StepSpec::Collapse { precious, precious_files, threshold } => {
                Step::Op(Op::Collapse { precious, precious_files, threshold })
            }
            StepSpec::ExtractUphenoRelations { relations, terms, term_files, roots, root_files } => {
                Step::Op(Op::ExtractUphenoRelations {
                    relations,
                    terms,
                    term_files,
                    roots,
                    root_files,
                })
            }
            StepSpec::Mint { temp_id_prefix, id_range_name, id_ranges } => {
                Step::Op(Op::Mint { temp_id_prefix, id_range_name, id_ranges })
            }
            StepSpec::AddPrefix { prefixes } => Step::Op(Op::AddPrefix { prefixes }),
            StepSpec::Normalize { base_iris, subset_decls, synonym_decls, add_source } => {
                Step::Op(Op::Normalize { base_iris, subset_decls, synonym_decls, add_source })
            }
            StepSpec::Template { templates, merge, prefixes } => {
                Step::Op(Op::Template { templates, merge, prefixes })
            }
            StepSpec::Rename { mappings, prefix_mappings, allow_missing } => {
                Step::Op(Op::Rename { mappings, prefix_mappings, allow_missing })
            }
            StepSpec::RoundTrip { output } => Step::Op(Op::RoundTrip { path: output }),
            StepSpec::Extract {
                method, terms, term_files, copy_ontology_annotations, individuals,
                branch_from_terms, branch_from_term_files,
            } => {
                Step::Op(Op::Extract {
                    method, terms, term_files, copy_ontology_annotations, individuals,
                    branch_from_terms, branch_from_term_files,
                })
            }
            StepSpec::Repair { invalid_references, merge_axiom_annotations } => {
                Step::Op(Op::Repair { invalid_references, merge_axiom_annotations })
            }
            StepSpec::Babelon { input, output, format } => Step::Op(Op::Babelon { input, output, format }),
            StepSpec::Expand { expand_terms, expand_term_files, no_expand_terms, no_expand_term_files } => {
                Step::Op(Op::Expand { expand_terms, expand_term_files, no_expand_terms, no_expand_term_files })
            }
            StepSpec::Subset { subset, queries, terms, term_files, reasoner, ancestors, fill_gaps } => {
                Step::Op(Op::Subset {
                    subset,
                    queries,
                    terms,
                    term_files,
                    reasoner,
                    ancestors,
                    fill_gaps,
                })
            }
            StepSpec::MergeSpecies {
                batch_file, extended, gca_translate, gca_delete, remove_declarations,
                taxon, suffix, properties, included,
            } => Step::Op(Op::MergeSpecies {
                batch_file, extended, gca_translate, gca_delete, remove_declarations,
                taxon, suffix, properties, included,
            }),
            StepSpec::RewriteDef {
                sub, dot, null_definitions, no_ids, include_obsolete, filter_prefix,
                add_annotation, add_annotation_iri,
            } => Step::Op(Op::RewriteDef(step::RewriteDefSpec {
                sub, dot, null_definitions, no_ids, include_obsolete, filter_prefix,
                add_annotation, add_annotation_iri,
            })),
            StepSpec::SimpleSubset { ont_id } => Step::Op(Op::SimpleSubset { ont_id }),
            StepSpec::ExtractOntologySubset { subset, fill_gaps } => {
                Step::Op(Op::ExtractOntologySubset { subset, fill_gaps })
            }
            StepSpec::ExtractMingraph => Step::Op(Op::ExtractMingraph),
            StepSpec::RemoveAxiomAnnotations => Step::Op(Op::RemoveAxiomAnnotations),
            StepSpec::MakeSubsetByProperties { properties } => {
                Step::Op(Op::MakeSubsetByProperties { properties })
            }
            StepSpec::MergeEquivalentSets { set_prefix, label_prefix, definition_prefix } => {
                Step::Op(Op::MergeEquivalentSets { set_prefix, label_prefix, definition_prefix })
            }
            StepSpec::Shell { command, requires } => Step::Shell { command, requires },
            StepSpec::Fallback { command, requires } => Step::Fallback { command, requires },
            StepSpec::Jq { args } => Step::Jq(args),
            StepSpec::Sssom { args } => Step::Sssom(args),
            StepSpec::CopyFile { src, dst, recursive, relative } => {
                Step::File(FileOp::Copy { src, dst, recursive, relative })
            }
            StepSpec::MoveFile { src, dst } => Step::File(FileOp::Move { src, dst }),
            StepSpec::RemoveFile { paths, recursive, force } => {
                Step::File(FileOp::Remove { paths, recursive, force })
            }
            StepSpec::Mkdir { paths, parents } => Step::File(FileOp::Mkdir { paths, parents }),
            StepSpec::Touch { paths } => Step::File(FileOp::Touch { paths }),
            StepSpec::Fetch { url, dst } => Step::File(FileOp::Fetch { url, dst }),
            StepSpec::BabelonMerge {
                inputs,
                output,
                sort_tables,
                drop_unknown_columns,
                update_translations,
            } => Step::File(FileOp::BabelonMerge {
                inputs,
                output,
                sort_tables,
                drop_unknown_columns,
                update_translations,
            }),
            StepSpec::BabelonPrepare {
                input,
                oak_adapter,
                language_code,
                fields,
                term_list,
                output,
                output_source_changed,
                output_not_translated,
                include_not_translated,
                update_translation_status,
                sort_tables,
                drop_unknown_columns,
            } => Step::File(FileOp::BabelonPrepare {
                input,
                oak_adapter,
                language_code,
                fields,
                term_list,
                output,
                output_source_changed,
                output_not_translated,
                include_not_translated,
                update_translation_status,
                sort_tables,
                drop_unknown_columns,
            }),
            StepSpec::Append { src, dst, overwrite } => {
                Step::File(FileOp::Concat { src, dst, append: !overwrite })
            }
            StepSpec::Sort { input, output, unique } => {
                Step::File(FileOp::Sort { input, output, unique })
            }
            StepSpec::Print { message, dst, append, no_newline } => {
                Step::File(FileOp::Print { message, dst, append, newline: !no_newline })
            }

            StepSpec::Branch { r#if, then_steps, else_steps } => Step::Branch {
                condition: r#if.into_condition(),
                then_steps: then_steps.into_iter().map(StepEntry::into_step).collect(),
                else_steps: else_steps.into_iter().map(StepEntry::into_step).collect(),
            },
            StepSpec::UnsupportedSubcommand { command } => Step::UnsupportedSubcommand(command),
            StepSpec::OwlmakeCli { command, args } => Step::OwlmakeCli { name: command, args },

            StepSpec::Oort { input, outdir, reasoner, simple, relaxed, asserted } => {
                Step::Oort(crate::plan::step::OortSpec {
                    input,
                    outdir,
                    reasoner,
                    simple,
                    relaxed,
                    asserted,
                })
            }
        }
    }
}

impl AnnotationSpec {
    fn from_pair((property, value): &(String, String)) -> Self {
        AnnotationSpec { property: property.clone(), value: value.clone() }
    }
    fn into_pair(self) -> (String, String) {
        (self.property, self.value)
    }
}

// --- Schema, validation, and file I/O -----------------------------------------

/// The JSON Schema for `owlmake.json`, derived from [`OwlmakeSpec`].
pub fn schema_value() -> serde_json::Value {
    let root = schemars::schema_for!(OwlmakeSpec);
    serde_json::to_value(root).expect("owlmake schema serializes")
}

/// A stable digest of the emitted plan schema, so a change to `OwlmakeSpec` or
/// `StepSpec` cannot slip past the format-floor decision (see
/// `PLAN_FORMAT_MIN_VERSION`). FNV-1a — not cryptographic, and it does not need
/// to be: it only has to change when the schema does.
pub fn schema_digest() -> String {
    let text = schema_pretty();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// The derived JSON Schema, pretty-printed for writing to disk.
pub fn schema_pretty() -> String {
    serde_json::to_string_pretty(&schema_value()).expect("owlmake schema serializes")
}

// ---------------------------------------------------------------------------
// Path relocation: the file's base vs the build's base
// ---------------------------------------------------------------------------
//
// The plan file lives at the repository ROOT, but the build runs in the ONTOLOGY
// directory (`src/ontology`, where the repo has one) — so every path a rule names
// is written relative to that, not to the file. Read straight off the page,
// `../sparql/hgnc_terms.sparql` next to `owlmake.yaml` points outside the repo,
// and `imports/mondo_import.owl` names a directory that isn't there.
//
// So paths are stored relative to the FILE and translated to the execution base
// on load. The translation is symmetric and total: `save` rebases every path from
// the ontology dir to the plan's directory, `load` rebases them back.

/// The build's working directory relative to a plan at `plan_path` — `src/ontology`
/// where the repo has one, the plan's own directory otherwise (nothing to
/// translate).
fn exec_dir(plan_path: &Path) -> (PathBuf, PathBuf) {
    let file_dir = plan_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let nested = file_dir.join("src/ontology");
    let exec = if nested.is_dir() { nested } else { file_dir.clone() };
    (file_dir, exec)
}

/// Resolve `.`/`..` lexically (no filesystem, no symlink semantics), so a path
/// can be rebased without existing yet.
fn normalize(p: &Path) -> PathBuf {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for c in p.components() {
        use std::path::Component::*;
        match c {
            CurDir => {}
            ParentDir => match out.last().map(|s| s.as_os_str()) {
                Some(last) if last != ".." => {
                    out.pop();
                }
                _ => out.push("..".into()),
            },
            other => out.push(other.as_os_str().to_os_string()),
        }
    }
    out.iter().collect()
}

/// `path` expressed relative to `base`, using `..` to climb where needed.
fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let (p, b) = (normalize(path), normalize(base));
    let mut pc: Vec<_> = p.components().collect();
    let mut bc: Vec<_> = b.components().collect();
    let shared = pc.iter().zip(bc.iter()).take_while(|(a, b)| a == b).count();
    pc.drain(..shared);
    bc.drain(..shared);
    let mut out = PathBuf::new();
    for _ in &bc {
        out.push("..");
    }
    for c in &pc {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// Known file suffixes, so a bare filename (`efo-edit.owl`, `catalog-v001.xml`)
/// is recognised as a path even without a `/`.
const PATH_SUFFIXES: [&str; 21] = [
    ".owl", ".obo", ".ofn", ".omn", ".ttl", ".json", ".jsonld", ".tsv", ".csv", ".txt", ".sparql",
    ".yaml", ".yml", ".xml", ".jar", ".py", ".pl", ".sh",
    // The SQLite database uPheno builds from `upheno.owl` and reads back to
    // match labels, and the two compressed forms a repo publishes it in.
    ".db", ".gz", ".ru",
];

/// Whether a token could name a file. Deliberately conservative: an IRI, a CURIE
/// (`rdfs:comment`, `NCBITaxon:9606`), a flag and a bare word are all rejected, so
/// only a token with a directory separator or a known suffix is even considered.
fn could_be_path(tok: &str) -> bool {
    !tok.is_empty()
        && !tok.starts_with('-')
        && !tok.contains(':')
        && !tok.contains('*')
        && (tok.contains('/') || PATH_SUFFIXES.iter().any(|s| tok.ends_with(s)))
}

/// A token made only of `.` and `..` components — `.`, `..`, `../..`. It cannot
/// be anything but a directory, and it is the one path shape `could_be_path`
/// rejects: no separator in `.`, no suffix in either. EFO's `RELEASEDIR = ../..`
/// rebases to `.` on save, and without this it would not come back, so a
/// plan-only build would publish the release into the ontology directory instead
/// of the repository root.
fn is_dot_path(tok: &str) -> bool {
    !tok.is_empty() && tok.split('/').all(|c| c == "." || c == "..")
}

/// Reinterpret `tok` — a path relative to `from` — as a path relative to `to`.
///
/// This is total and purely lexical. Probing the filesystem here would make the
/// mapping neither total nor symmetric: a token whose parent directory exists at
/// save time and not at load time is rewritten once and never rewritten back.
/// EFO's `.gitignore` lists `build`, `mirror` and `tmp`, so a plan generated
/// after a build records `src/ontology/build/efo.owl`, and on a fresh clone — the
/// case owlmake exists for — the executor would resolve that against the
/// ontology directory and write `src/ontology/src/ontology/build/efo.owl`, exit
/// code 0. It would also make the plan's own bytes depend on which gitignored
/// directories happened to be present.
fn rebase(tok: &str, from: &Path, to: &Path) -> Option<String> {
    if !could_be_path(tok) && !is_dot_path(tok) {
        return None;
    }
    // An ABSOLUTE path names a machine location, not a repo file — the reference
    // image's `/tools/obo.epm.json` is the case. Expressed relative to the plan
    // it would encode where the repo happens to sit, so the same tree at two
    // paths would carry two different plans.
    if tok.starts_with('/') {
        return None;
    }
    let abs = normalize(&from.join(tok));
    // What is written has to be something the reverse trip turns back into `tok`.
    // A path keeps the trailing `/` that says it is a directory; and one that no
    // longer LOOKS like a path once rebased — `../../mappings` becoming `mappings`
    // — is written `./mappings`, or it would be read back as a bare word and left
    // where the other base put it.
    let mut out = relative_to(&abs, to).to_string_lossy().into_owned();
    if tok.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    if !could_be_path(&out) && !is_dot_path(&out) {
        out.insert_str(0, "./");
    }
    Some(out)
}

/// Split a command line into shell words: whitespace separates, but a `'…'` or
/// `"…"` region is one word however much whitespace it contains. Quotes are kept
/// on the word (the caller trims them, and needs them to reconstruct the match),
/// and an unterminated quote runs to the end of the line rather than being
/// dropped.
fn shell_words(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let c = bytes[i];
            match quote {
                Some(q) => {
                    if c == q {
                        quote = None;
                    }
                }
                None if c == b'\'' || c == b'"' => quote = Some(c),
                None if c.is_ascii_whitespace() => break,
                None => {}
            }
            i += 1;
        }
        out.push(&s[start..i]);
    }
    out
}

/// Rebase every path-like token in `s`, leaving the rest of the string byte for
/// byte intact: a declared value may name several paths, as a recorded
/// variable's list does. Tokens are located by splitting on whitespace — a quoted
/// region is one word however much whitespace is inside it — and stripping the
/// punctuation a shell puts around a filename.
///
/// Substitution is ONE left-to-right pass, taking the longest token that starts
/// at each position and skipping past what it wrote, and a match must sit on a
/// token boundary. A sequence of `String::replace` calls cannot do this even
/// ordered longest-first: that stops a token being rewritten before a longer one
/// containing it, but not inside the REPLACEMENT the longer one just produced, so
/// `mondo.obo.tmp.obo mondo.obo` would come out with the first prefixed twice.
fn rebase_in_string(s: &str, from: &Path, to: &Path) -> String {
    const EDGE: [char; 10] = ['\'', '"', '(', ')', ';', ',', '<', '>', '|', '`'];
    let mut subs: Vec<(String, String)> = Vec::new();
    for raw in shell_words(s) {
        let tok = raw.trim_matches(|c| EDGE.contains(&c));
        if tok.is_empty() || subs.iter().any(|(t, _)| t == tok) {
            continue;
        }
        if let Some(new) = rebase(tok, from, to) {
            if new != tok {
                subs.push((tok.to_string(), new));
            }
        }
    }
    if subs.is_empty() {
        return s.to_string();
    }
    subs.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
    let boundary = |c: char| c.is_whitespace() || EDGE.contains(&c);
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let rest = &s[i..];
        let before_ok = i == 0 || s[..i].chars().next_back().is_some_and(boundary);
        let hit = before_ok
            .then(|| {
                subs.iter().find(|(old, _)| {
                    rest.starts_with(old.as_str())
                        && rest[old.len()..].chars().next().is_none_or(boundary)
                })
            })
            .flatten();
        match hit {
            Some((old, new)) => {
                out.push_str(new);
                i += old.len();
            }
            None => {
                let c = rest.chars().next().expect("rest is non-empty");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// Keys whose value is arbitrary text a human wrote rather than a path the schema
/// declares: a shell line, a `Print` message, a tool's argument vector. A path
/// inside such text can only be guessed at, and rebasing a guess is not undone
/// by rebasing it back, so the text is written as it runs: in the build's working
/// directory, exactly as the step executes it. Everything else in the plan is a
/// declared path field and is rebased on shape alone.
///
/// `variables` is deliberately NOT here. A recorded variable's value is a path or
/// a list of them (`SRC`, `ROBOT`, `MIRRORDIR`, `VQUERIES`) — that is why it is
/// recorded at all — so it is rebased as every other declared path is.
fn is_free_text_key(key: &str) -> bool {
    matches!(key, "command" | "message" | "args")
}

/// Keys whose value is a LITERAL — an annotation's text, which may contain a `/`
/// or end in something that looks like a file suffix and is neither.
fn is_literal_key(key: &str) -> bool {
    matches!(key, "value" | "annotations" | "add_annotation" | "add_annotation_iri")
}

/// Walk a serialized plan and rebase every path it declares.
///
/// The walk is key-aware because the plan holds three different kinds of string
/// — a declared path, free text, a literal — and they cannot be told apart by
/// looking at one; only a declared path is rebased. Defaulting an unrecognised
/// key to the path treatment is deliberate: a path field added later is rebased
/// without anyone remembering to list it, which is the direction the mistake
/// should fall.
fn relocate(value: &mut serde_json::Value, from: &Path, to: &Path) {
    match value {
        serde_json::Value::String(s) => *s = rebase_in_string(s, from, to),
        serde_json::Value::Array(a) => {
            for v in a {
                relocate(v, from, to);
            }
        }
        serde_json::Value::Object(o) => {
            for (k, v) in o.iter_mut() {
                if !is_free_text_key(k) && !is_literal_key(k) {
                    relocate(v, from, to);
                }
            }
        }
        _ => {}
    }
}

/// Rebase the paths of a whole document, leaving the standard-build options as
/// written. An option is a setting, not a path: a template is named `GWAS.csv`
/// whichever directory the file is read from.
fn relocate_document(value: &mut serde_json::Value, from: &Path, to: &Path) {
    let Some(map) = value.as_object_mut() else { return };
    for (key, v) in map.iter_mut() {
        if crate::odk::builtin::is_option(key) && !OwlmakeSpec::is_plan_key(key) {
            continue;
        }
        relocate(v, from, to);
    }
}

/// Validate a parsed `owlmake.json` document against the derived schema,
/// reporting every violation with its JSON path.
fn validate(value: &serde_json::Value) -> Result<()> {
    let schema = schema_value();
    let compiled = jsonschema::JSONSchema::compile(&schema)
        .map_err(|e| anyhow::anyhow!("internal: owlmake schema is invalid: {e}"))?;
    if let Err(errors) = compiled.validate(value) {
        let mut msgs: Vec<String> = errors
            .map(|e| {
                let path = e.instance_path.to_string();
                let at = if path.is_empty() { "(root)".to_string() } else { path };
                format!("  at {at}: {e}")
            })
            .collect();
        msgs.sort();
        msgs.dedup();
        bail!("the plan failed schema validation:\n{}", msgs.join("\n"));
    }
    Ok(())
}

/// The oldest owlmake that can execute a plan in the CURRENT format.
///
/// Hand-maintained, and deliberately not `CARGO_PKG_VERSION`: it moves only when
/// a change to `OwlmakeSpec` or `StepSpec` makes an older binary unable to run a
/// new plan. Because a hand-maintained constant rots, `plan_schema_is_pinned`
/// below fails whenever the emitted schema changes without this being
/// reconsidered.
pub const PLAN_FORMAT_MIN_VERSION: &str = "0.3.2";

/// Load and validate a committed plan (`owlmake.yaml` or `owlmake.json`).
pub fn load(path: &Path) -> Result<OwlmakeSpec> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    // Both spellings validate against the one JSON Schema: YAML is parsed to the
    // same value tree first. (YAML is a superset of JSON, so a `.yaml` file that
    // happens to contain JSON also loads.)
    let mut value: serde_json::Value = match PlanFormat::of_path(path) {
        PlanFormat::Json => serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        PlanFormat::Yaml => serde_yaml::from_str(&text)
            .with_context(|| format!("{} is not valid YAML", path.display()))?,
    };
    // The version gate runs BEFORE the strict parse. `OwlmakeSpec` carries
    // `#[serde(deny_unknown_fields)]`, so an older binary reading a newer plan
    // would otherwise die inside `from_value` with `unknown field 'x'` and never
    // reach the version message — which would make the "min_owlmake_version will
    // tell you to upgrade" promise vacuous for every field ever added.
    if let Some(req) = value.get("min_owlmake_version").and_then(serde_json::Value::as_str) {
        check_min_version(req, path)?;
    }
    validate(&value).with_context(|| format!("validating {}", path.display()))?;
    // On disk every declared path is relative to this file; the build runs in the
    // ontology directory, so translate them to that base before anything reads
    // them.
    let (file_dir, exec) = exec_dir(path);
    if file_dir != exec {
        relocate_document(&mut value, &file_dir, &exec);
    }
    let spec: OwlmakeSpec = serde_json::from_value(value)
        .with_context(|| format!("interpreting {}", path.display()))?;
    spec.check().with_context(|| format!("in {}", path.display()))?;
    check_version(&spec, path)?;
    Ok(spec)
}

impl OwlmakeSpec {
    /// A file stating `options` — the repository's options for the standard
    /// build, `id` among them — over the built-in rules or, with
    /// `use_builtin_rules` off, over nothing, and nothing else yet.
    pub fn for_options(
        options: serde_json::Map<String, serde_json::Value>,
        use_builtin_rules: bool,
    ) -> Result<OwlmakeSpec> {
        let mut document = serde_json::Map::new();
        document.insert("min_owlmake_version".into(), PLAN_FORMAT_MIN_VERSION.into());
        document.insert("use_builtin_rules".into(), use_builtin_rules.into());
        document.extend(options);
        serde_json::from_value(serde_json::Value::Object(document))
            .context("reading the repository's options")
    }

    /// The options as one document: the keys this struct holds as fields of its
    /// own, and the rest as written.
    pub fn options(&self) -> serde_json::Value {
        let mut all = serde_json::Map::new();
        all.insert("id".into(), self.id.clone().into());
        if !self.reasoner.is_empty() {
            all.insert("reasoner".into(), self.reasoner.clone().into());
        }
        if let Some(catalog) = &self.catalog_file {
            all.insert("catalog_file".into(), catalog.clone().into());
        }
        all.extend(self.options.clone());
        serde_json::Value::Object(all)
    }

    /// A key that is a field of the plan as well as an option, and is read as the
    /// field: both mean the same thing.
    fn is_plan_key(key: &str) -> bool {
        matches!(key, "id" | "reasoner" | "catalog_file")
    }

    /// Refuse a file that holds a key nothing reads, or that says something its
    /// base cannot honour.
    ///
    /// With the standard build, the merged import is decided by `import_group`, so
    /// the fields that would state it a second way are refused. Without it, an
    /// option of the standard build sets nothing and is refused by name, and the
    /// identity of the ontology has to be stated because nothing else can derive
    /// it.
    pub fn check(&self) -> Result<()> {
        for key in self.options.keys() {
            if let Some(reason) = crate::odk::builtin::tool_only_reason(key) {
                bail!("`{key}` sets {reason}; leave it out");
            }
            if !crate::odk::builtin::is_option(key) {
                bail!("unknown key `{key}`: it is neither part of a plan nor an option of the standard build");
            }
        }
        if self.id.is_empty() {
            bail!("the file names no `id`");
        }
        if self.use_builtin_rules {
            let stated = [
                ("use_base_merging", self.use_base_merging),
                ("exclude_iri_patterns", !self.exclude_iri_patterns.is_empty()),
                ("slme_individuals", self.slme_individuals.is_some()),
                ("merged_import", self.merged_import.is_some()),
                ("merged_import_iri", self.merged_import_iri.is_some()),
                ("merged_import_shards", self.merged_import_shards.is_some()),
                ("merged_import_shard_bytes", self.merged_import_shard_bytes.is_some()),
            ];
            if let Some((key, _)) = stated.iter().find(|(_, set)| *set) {
                bail!(
                    "`{key}` states the merged import, which the standard build decides from \
                     `import_group`: state it there, or state the whole build with \
                     `use_builtin_rules: false`"
                );
            }
        } else {
            if let Some(key) = self.options.keys().next() {
                bail!(
                    "`{key}` is an option of the standard build, and this file does not use \
                     its rules (`use_builtin_rules: false`): nothing would read it"
                );
            }
            for (name, value) in [
                ("version", &self.version),
                ("ontology_iri", &self.ontology_iri),
                ("reasoner", &self.reasoner),
            ] {
                if value.is_empty() {
                    bail!(
                        "a build of the repository's own (`use_builtin_rules: false`) must state \
                         `{name}`: there is no standard build to derive it from"
                    );
                }
            }
        }
        for i in &self.imports {
            if i.extends {
                if !self.use_builtin_rules {
                    bail!(
                        "import `{}` says `extends`, and this file does not use the standard \
                         build's rules (`use_builtin_rules: false`): there is no standard \
                         pipeline to extend",
                        i.id
                    );
                }
                if i.steps.is_empty() {
                    bail!("import `{}` says `extends` but states no `steps` to append", i.id);
                }
            } else {
                for (name, value) in [("source", &i.source), ("output", &i.output)] {
                    if value.is_empty() {
                        bail!(
                            "import `{}` states no `{name}`: an entry that is the whole \
                             pipeline states both, and one that only appends steps says \
                             `extends`",
                            i.id
                        );
                    }
                }
            }
        }
        self.check_emulation_versions()
    }

    /// A plan states which ODK release it emulates, or which tool version, or
    /// both AGREEING. Both disagreeing is refused.
    ///
    /// The two are separate facts — a repo that ships its own tool runs a version
    /// its image never carried — so neither can be derived from the other in
    /// general. But when a plan names both, execution would have to pick one, and
    /// picking silently is how a build produces the older JSON nesting with the
    /// newer prefix map: a combination no release carries. Refusing says which two
    /// statements conflict, which is something a repo with no build configuration
    /// left can still act on.
    pub fn check_emulation_versions(&self) -> Result<()> {
        let (Some(odk), Some(robot)) =
            (self.emulate_odk_version.as_deref(), self.emulate_robot_version.as_deref())
        else {
            return Ok(());
        };
        let (Some(o), Some(r)) = (parse_version(odk), parse_version(robot)) else {
            return Ok(());
        };
        let implied = crate::odk::workflows::odk_robot_version(o);
        if implied != r {
            bail!(
                "emulate_odk_version {odk} and emulate_robot_version {robot} disagree: \
                 ODK {odk} carries {}.{}.{}. Record the one the repo actually states — \
                 the ODK release when it runs the image's own tool, the tool version \
                 when it ships its own — or make them agree.",
                implied.0,
                implied.1,
                implied.2
            );
        }
        Ok(())
    }

    /// State in this file what `mine` builds that `base` does not — `base` being
    /// the plan this file's options and `use_builtin_rules` resolve to with nothing
    /// else stated, and `mine` the build the repository actually has.
    ///
    /// This is the half of the file the graph is built from: its targets, with
    /// `artefact` where `mine` releases what `base` does not, `extends` where only
    /// prerequisites were added, and `when` for the switches each exists under;
    /// its imports and pattern products; its phony names, intermediates, switch
    /// values and variables; and, for a build of the repository's own, its
    /// identity. What the graph derives from these is stated afterwards, by
    /// [`state_rest`](Self::state_rest).
    pub fn state_build(&mut self, mine: &Plan, base: &Plan) {
        use std::collections::HashMap;
        let json = |v: &dyn erased::Json| v.json();
        let same = |a: &ArtefactSpec, b: &ArtefactSpec| json(a) == json(b);
        let base_targets: HashMap<&str, ArtefactSpec> = base
            .prerequisites
            .iter()
            .chain(base.artefacts.iter())
            .map(|t| (t.target.as_str(), ArtefactSpec::from_plan(t)))
            .collect();
        let base_artefacts: std::collections::HashSet<&str> =
            base.artefacts.iter().map(|a| a.target.as_str()).collect();
        let switches = |target: &str| -> Vec<String> {
            mine.refresh_groups
                .iter()
                .filter(|g| !g.flag.is_empty() && g.targets.iter().any(|t| t == target))
                .map(|g| g.flag.clone())
                .collect()
        };
        let targets = mine
            .prerequisites
            .iter()
            .map(|t| (t, false))
            .chain(mine.artefacts.iter().map(|t| (t, true)));
        for (t, released) in targets {
            let mut own = ArtefactSpec::from_plan(t);
            // Said only where the base does not already release it.
            let artefact = released && !base_artefacts.contains(t.target.as_str());
            match base_targets.get(t.target.as_str()) {
                Some(theirs) if same(&own, theirs) => {
                    if !artefact {
                        continue;
                    }
                    // The standard target as it stands, released.
                    own = ArtefactSpec { extends: true, ..ArtefactSpec::named(&t.target) };
                }
                // The standard target with prerequisites added: record the additions.
                Some(theirs) => {
                    let mut grown = theirs.clone();
                    grown.needs = own.needs.clone();
                    grown.order_only = own.order_only.clone();
                    grown.input = own.input.clone();
                    let added = |all: &[String], had: &[String]| -> Vec<String> {
                        all.iter().filter(|n| !had.contains(n)).cloned().collect()
                    };
                    if same(&own, &grown) && theirs.needs.iter().all(|n| own.needs.contains(n)) {
                        own = ArtefactSpec {
                            extends: true,
                            needs: added(&own.needs, &theirs.needs),
                            order_only: added(&own.order_only, &theirs.order_only),
                            ..ArtefactSpec::named(&t.target)
                        };
                    }
                }
                None => {}
            }
            own.artefact = artefact;
            own.when = switches(&t.target);
            self.targets.push(own);
        }
        self.phony = mine.phony.iter().filter(|p| !base.phony.contains(p)).cloned().collect();
        // What its own rules decide beyond their targets: the switches they test,
        // with the values the repository gives them, and the targets they reach
        // only as intermediates.
        self.gating_flags = mine
            .gating_flags
            .iter()
            .filter(|(k, v)| base.gating_flags.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.transient_targets = mine
            .transient_targets
            .iter()
            .filter(|t| !base.transient_targets.contains(t))
            .cloned()
            .collect();
        self.variables = mine
            .variables
            .iter()
            .filter(|(k, v)| base.variables.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // The natively built products are not targets, so what the repository does
        // its own way with one shows up in the import, or in the pattern spec.
        for import in &mine.imports {
            let own = ImportSpec::from_plan(import);
            let standard =
                base.imports.iter().find(|i| i.id == import.id).map(ImportSpec::from_plan);
            if standard.map(|s| json(&s)) != Some(json(&own)) {
                self.imports.push(own);
            }
        }
        if mine.dosdp.as_ref().map(|d| json(d)) != base.dosdp.as_ref().map(|d| json(d)) {
            self.dosdp = mine.dosdp.clone();
        }
        if mine.reasoner != base.reasoner {
            self.reasoner = mine.reasoner.clone();
        }
        if mine.edit_file != base.edit_file {
            self.edit_file = mine.edit_file.clone();
        }
        if mine.catalog_file != base.catalog_file {
            self.catalog_file = mine.catalog_file.clone();
        }
        if !self.use_builtin_rules {
            // Nothing derives these; the file is the whole build.
            self.version = mine.version.clone();
            self.ontology_iri = mine.ontology_iri.clone();
            self.use_base_merging = mine.use_base_merging;
            self.exclude_iri_patterns = mine.exclude_iri_patterns.clone();
            self.slme_individuals = mine.slme_individuals.clone();
            self.merged_import = mine.merged_import.clone();
            self.merged_import_iri = mine.merged_import_iri.clone();
            self.merged_import_shards = mine.merged_import_shards.clone();
            self.merged_import_shard_bytes = mine.merged_import_shard_bytes;
        }
    }

    /// State in this file what `mine` says that `derived` — the plan this file
    /// resolves to as it stands — does not: what a bare build makes, the switches
    /// the plan exposes, the natively built paths, the component gaps, and the
    /// settings no rule decides. Each is written only where the derivation falls
    /// short, so the file carries what the repository decided and nothing that
    /// follows from it.
    pub fn state_rest(&mut self, mine: &Plan, derived: &Plan) {
        if mine.default_targets != derived.default_targets {
            self.default_targets = Some(mine.default_targets.clone());
        }
        let groups = |plan: &Plan| -> Vec<(String, String, Vec<String>, crate::plan::Freshness)> {
            plan.refresh_groups
                .iter()
                .map(|g| {
                    let mut targets = g.targets.clone();
                    targets.sort();
                    (g.name.clone(), g.flag.clone(), targets, g.default)
                })
                .collect()
        };
        if groups(mine) != groups(derived) {
            self.refresh_groups = Some(mine.refresh_groups.clone());
        }
        self.native_targets = mine
            .native_targets
            .iter()
            .filter(|t| !derived.native_targets.contains(t))
            .cloned()
            .collect();
        self.component_gaps = mine
            .component_gaps
            .iter()
            .filter(|g| !derived.component_gaps.contains(g))
            .cloned()
            .collect();
        if mine.version != derived.version {
            self.version = mine.version.clone();
        }
        if mine.version_file != derived.version_file {
            self.version_file = mine.version_file.clone();
        }
        if mine.ontology_iri != derived.ontology_iri {
            self.ontology_iri = mine.ontology_iri.clone();
        }
        self.strict = mine.strict && !derived.strict;
        self.xml_entities = mine.xml_entities && !derived.xml_entities;
        self.emulate_odk_version = mine.emulate_odk_version.map(format_version);
        self.emulate_robot_version = (mine.emulate_robot_version
            != implied_robot_version(mine.emulate_odk_version))
        .then(|| format_version(mine.emulate_robot_version));
    }
}

/// Comparing two recorded things by what they would write.
mod erased {
    pub trait Json {
        fn json(&self) -> Option<serde_json::Value>;
    }
    impl<T: serde::Serialize> Json for T {
        fn json(&self) -> Option<serde_json::Value> {
            serde_json::to_value(self).ok()
        }
    }
}

/// Refuse a plan that declares a minimum owlmake version this binary is below.
///
/// The check is a plain numeric comparison of dot-separated components, so
/// `0.10.0` beats `0.9.0`. An unparseable or absent declaration is accepted: the
/// field is optional, and a version string owlmake cannot read is not grounds to
/// refuse to build.
fn check_version(spec: &OwlmakeSpec, path: &Path) -> Result<()> {
    let Some(required) = spec.min_owlmake_version.as_deref() else { return Ok(()) };
    check_min_version(required, path)
}

/// The comparison itself, callable before the typed parse (see `load`).
fn check_min_version(required: &str, path: &Path) -> Result<()> {
    let running = env!("CARGO_PKG_VERSION");
    let (Some(req), Some(run)) = (version_key(required), version_key(running)) else {
        return Ok(());
    };
    if req > run {
        anyhow::bail!(
            "{} needs owlmake {required} or newer; this is {running}. \
             The plan was generated by a later version and may name steps this one \
             cannot execute — upgrade owlmake, or regenerate the plan with this version.",
            path.display()
        );
    }
    Ok(())
}

/// A dot-separated version as a comparable tuple. `None` when any component is
/// not a number (a pre-release tag, a git description, …).
fn version_key(v: &str) -> Option<Vec<u64>> {
    v.split('.').map(|c| c.parse::<u64>().ok()).collect()
}

/// The exact value [`save`] would write for `spec` at `path` — paths rebased to
/// the plan file's own directory.
///
/// Separate from `save` so a plan can be COMPARED with one on disk without
/// writing it. Comparing the two files' text would answer a different and
/// wrong question, since formatting and key order are the serializer's, not the
/// build's; going through this puts both sides in the one canonical form.
pub fn to_value(spec: &OwlmakeSpec, path: &Path) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(spec)?;
    let (file_dir, exec) = exec_dir(path);
    if file_dir != exec {
        relocate_document(&mut value, &exec, &file_dir);
    }
    Ok(value)
}

/// Serialize a spec to `path`, in the format its extension names.
pub fn save(spec: &OwlmakeSpec, path: &Path) -> Result<()> {
    // Declared paths are held relative to the build's working directory; write
    // them relative to the file, so a reader sitting next to it can resolve them.
    // `load` translates them straight back. Free text is written as it runs.
    let value = to_value(spec, path)?;
    let mut text = match PlanFormat::of_path(path) {
        PlanFormat::Json => serde_json::to_string_pretty(&value)?,
        PlanFormat::Yaml => serde_yaml::to_string(&value)?,
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `may_fail` is a field of the step, beside its `op` — not a wrapper around
    /// it — and it survives a round trip through the plan file.
    #[test]
    fn may_fail_is_written_beside_the_op_it_applies_to() {
        let step = Step::MayFail(Box::new(Step::Op(crate::plan::step::Op::Query {
            updates: vec![],
            selects: vec![("q.sparql".into(), "out.tsv".into())],
            constructs: vec![],
            format: None,
            use_graphs: false,
            tdb: false,
        })));

        let yaml = serde_yaml::to_string(&StepEntry::from_step(&step)).unwrap();
        assert!(yaml.contains("op: query"), "the operation is still named: {yaml}");
        assert!(yaml.contains("may_fail: true"), "the tolerance is a field: {yaml}");

        let back: StepEntry = serde_yaml::from_str(&yaml).unwrap();
        assert!(
            matches!(back.into_step(), Step::MayFail(inner)
                if matches!(*inner, Step::Op(crate::plan::step::Op::Query { .. }))),
            "a plan that says may_fail reads back as a step that may fail"
        );
    }

    /// An import entry either states its whole pipeline (and with it its source
    /// and output) or says `extends` and appends steps to the standard one —
    /// `check` refuses the shapes in between, where the plan would silently
    /// build less than the file says.
    #[test]
    fn an_import_entry_is_whole_or_extends() {
        let yaml = |imports: &str| -> OwlmakeSpec {
            serde_yaml::from_str(&format!("id: tiny
imports:
{imports}")).unwrap()
        };
        let extends_no_steps = yaml("- id: ncit
  extends: true
");
        let err = extends_no_steps.check().unwrap_err().to_string();
        assert!(err.contains("no `steps` to append"), "{err}");

        let whole_without_source = yaml("- id: ncit
  output: imports/ncit_import.owl
");
        let err = whole_without_source.check().unwrap_err().to_string();
        assert!(err.contains("states no `source`"), "{err}");

        let extends: OwlmakeSpec = yaml(
            "- id: ncit
  extends: true
  steps:
  - op: remove-terms
    terms:
    - oboInOwl:SubsetProperty
    selects:
    - children
",
        );
        extends.check().expect("an extending entry with steps is well-formed");

        let mut own_build = yaml("- id: ncit
  extends: true
  steps:
  - op: relax
");
        own_build.use_builtin_rules = false;
        own_build.version = "2026-01-01".into();
        own_build.ontology_iri = "http://example.com/tiny.owl".into();
        own_build.reasoner = "elk".into();
        let err = own_build.check().unwrap_err().to_string();
        assert!(err.contains("no standard"), "{err}");
    }

    /// An ordinary step writes no `may_fail` at all — the flag is the exception,
    /// so every other step in a plan is unchanged by its existence.
    #[test]
    fn an_ordinary_step_writes_no_may_fail() {
        let step = Step::Op(crate::plan::step::Op::Relax { include_subclass_of: false });
        let yaml = serde_yaml::to_string(&StepEntry::from_step(&step)).unwrap();
        assert!(!yaml.contains("may_fail"), "unexpected flag: {yaml}");
    }

    /// A value naming both `X` and `X.tmp.obo` must rebase each exactly once: a
    /// sequence of `String::replace` calls would rewrite `mondo.obo` inside the
    /// replacement it had just produced for `mondo.obo.tmp.obo`, doubling the
    /// prefix.
    #[test]
    fn rebases_a_token_that_is_a_prefix_of_another_once() {
        let base = Path::new("/repo");
        let onto = base.join("src/ontology");

        // save: paths held relative to src/ontology, written relative to the root.
        let value = "mondo.obo.tmp.obo mondo.obo";
        let saved = rebase_in_string(value, &onto, base);
        assert_eq!(saved, "src/ontology/mondo.obo.tmp.obo src/ontology/mondo.obo");

        // load: and straight back, so the round trip is the identity.
        assert_eq!(rebase_in_string(&saved, base, &onto), value);

        // A quoted region is one word, not three path-shaped fragments.
        let sed = "sed -i 's/  */ /g' reports/mondo_release_diff.md";
        assert_eq!(
            rebase_in_string(sed, &onto, base),
            "sed -i 's/  */ /g' src/ontology/reports/mondo_release_diff.md"
        );
    }

    /// **The plan file is the contract, so every path in it is relative to the
    /// plan file — and `load` must give back exactly what `save` was handed.**
    ///
    /// This is the only test in this module that goes through `save`/`load`, and so
    /// the only one here that exercises `relocate`: `plan_survives_a_spec_round_trip`
    /// goes `from_plan(…).into_plan(…)` in memory and never touches either. Two
    /// failures it guards against. A target under a directory that does not exist
    /// yet, left un-rebased, makes a plan generated after a build and used on a
    /// fresh clone — where `build/` is gitignored — build into
    /// `src/ontology/src/ontology/build/…` and exit 0; and `../..`, which is EFO's
    /// `RELEASEDIR`, saved as `.` and not coming back, makes a plan-only build
    /// publish the release into the ontology directory instead of the repository
    /// root.
    ///
    /// The fixture names, deliberately: a target under a directory that does NOT
    /// exist, `../..` as a copy destination, a bare filename, a path outside the
    /// repo in a recorded variable, a phony target that must not be touched, and
    /// an IRI.
    #[test]
    fn every_path_survives_a_save_and_load_unchanged() {
        use crate::plan::step::{Op, Step};

        let base =
            std::env::temp_dir().join(format!("owlmake-planpaths-{}", std::process::id()));
        let onto = base.join("src/ontology");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&onto).unwrap();
        // NOTE: `build/` is deliberately NOT created — that is the whole point.

        let artefact = ArtefactPlan {
            target: "build/tiny.owl".into(),
            input: Some("tiny-edit.owl".into()),
            needs: vec!["tiny-edit.owl".into(), "components/c.owl".into()],
            order_only: vec![],
            steps: vec![Step::Op(Op::Merge {
                inputs: vec!["tiny-edit.owl".into()],
                collapse_import_closure: None,
            })],
            gaps: vec![],
            missing_rule: false,
            side_effect_only: false,
            stdout_file: None,
            intermediate: false,
            branches: vec![],
        };
        let release = ArtefactPlan {
            target: "release".into(),
            input: None,
            needs: vec!["build/tiny.owl".into()],
            order_only: vec![],
            steps: vec![Step::File(crate::build::recipe::FileOp::Copy {
                src: vec!["build/tiny.owl".into()],
                dst: "../..".into(),
                recursive: false,
                relative: false,
            })],
            gaps: vec![],
            missing_rule: false,
            side_effect_only: false,
            stdout_file: None,
            intermediate: false,
            branches: vec![],
        };
        let plan = Plan {
            id: "tiny".into(),
            version: "1".into(),
            version_file: None,
            ontology_iri: "http://example.org/tiny.owl".into(),
            reasoner: "ELK".into(),
            use_base_merging: false,
            exclude_iri_patterns: vec![],
            slme_individuals: None,
            imports: vec![],
            merged_import: None,
            merged_import_iri: None,
            merged_import_shards: None,
            merged_import_shard_bytes: None,
            variables: [("ROBOT".to_string(), "../../bin/robot".to_string())]
                .into_iter()
                .collect(),
            component_gaps: vec![],
            prerequisites: vec![release],
            artefacts: vec![artefact],
            default_targets: vec!["build/tiny.owl".into(), "release".into()],
            phony: vec!["release".into()],
            transient_targets: vec![],
            native_targets: vec![],
            edit_file: Some("tiny-edit.owl".into()),
            catalog_file: Some("catalog-v001.xml".into()),
            dosdp: None,
            emulate_odk_version: Some((1, 6, 0)),
            emulate_robot_version: (1, 9, 8),
            strict: false,
            xml_entities: false,
            refresh_groups: vec![],
            gating_flags: Default::default(),
        };

        let path = base.join("owlmake.yaml");
        save(&OwlmakeSpec::from_plan(&plan), &path).unwrap();

        // On disk, every path is spelled relative to the plan file.
        let text = std::fs::read_to_string(&path).unwrap();
        for want in [
            "src/ontology/build/tiny.owl",
            "src/ontology/tiny-edit.owl",
            "src/ontology/components/c.owl",
            "src/ontology/catalog-v001.xml",
        ] {
            assert!(text.contains(want), "`{want}` is not spelled relative to the plan:\n{text}");
        }
        // …and a phony target and an IRI are left exactly alone.
        assert!(text.contains("- release"), "a phony target was mangled:\n{text}");
        assert!(text.contains("http://example.org/tiny.owl"), "an IRI was mangled:\n{text}");

        let back = load(&path).unwrap().into_plan(&onto);
        assert_eq!(back.artefacts[0].target, plan.artefacts[0].target);
        assert_eq!(back.artefacts[0].input, plan.artefacts[0].input);
        assert_eq!(back.artefacts[0].needs, plan.artefacts[0].needs);
        match &back.artefacts[0].steps[..] {
            [Step::Op(Op::Merge { inputs, .. })] => {
                assert_eq!(inputs, &vec!["tiny-edit.owl".to_string()], "a merge input did not survive")
            }
            other => panic!("the merge step did not survive: {} step(s)", other.len()),
        }
        assert_eq!(back.edit_file, plan.edit_file);
        assert_eq!(back.catalog_file, plan.catalog_file);
        assert_eq!(back.default_targets, plan.default_targets);
        assert_eq!(back.phony, plan.phony);
        assert_eq!(back.variables, plan.variables, "a Makefile variable's path did not survive");
        match &back.prerequisites[0].steps[..] {
            [Step::File(crate::build::recipe::FileOp::Copy { src, dst, .. })] => {
                assert_eq!(src, &vec!["build/tiny.owl".to_string()]);
                assert_eq!(
                    dst, "../..",
                    "`cp … ../..` did not come back — the release would publish into the ontology dir"
                );
            }
            other => panic!("the copy step did not survive: {} step(s)", other.len()),
        }

        // And the spelling does not depend on what happens to be on disk: create
        // the directory that was missing and save again — byte-identical.
        std::fs::create_dir_all(onto.join("build")).unwrap();
        let second = base.join("owlmake2.yaml");
        save(&OwlmakeSpec::from_plan(&plan), &second).unwrap();
        assert_eq!(
            text,
            std::fs::read_to_string(&second).unwrap(),
            "the plan's bytes changed because a directory appeared"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// A command, a message and a tool's argument vector are written as they run,
    /// in the build's working directory, and come back byte for byte.
    ///
    /// Only a declared path can be rebased exactly. A path inside free text can
    /// only be guessed at, and the reverse guess does not undo it: UBERON's
    /// normalisation recipe names `../../src/ontology/imports/caro_import.owl`,
    /// which climbs out of the ontology directory and back in, and
    /// `src/ontology/imports/fbbt_import.owl`, which from the ontology directory
    /// names a file that is not there. Rebased to the plan file and back, both
    /// read `imports/…`: the first spelled differently, the second naming another
    /// file. Nor may what is written depend on what is on disk: EFO's qc steps name
    /// `build/efo.owl`, and `build/` exists only once the ontology has built.
    #[test]
    fn free_text_is_written_as_it_runs() {
        let base =
            std::env::temp_dir().join(format!("owlmake-freetext-{}", std::process::id()));
        let onto = base.join("src/ontology");
        let _ = std::fs::remove_dir_all(&base);
        // The directories the commands climb into exist, as they do in the
        // repository; `build/` does not.
        std::fs::create_dir_all(onto.join("imports")).unwrap();
        std::fs::create_dir_all(base.join("src/scripts")).unwrap();

        let norm = "sh ../scripts/norm_rdfxml.sh ../../src/ontology/imports/caro_import.owl \
                    src/ontology/imports/fbbt_import.owl";
        let release = "sh ../scripts/norm_rdfxml.sh ../../tiny-base.owl";
        let qc = "python3 ../scripts/check.py build/efo.owl";
        let message = "normalised imports/caro_import.owl";
        let spec: OwlmakeSpec = serde_json::from_value(serde_json::json!({
            "min_owlmake_version": PLAN_FORMAT_MIN_VERSION,
            "emulate_odk_version": crate::odk::builtin::BEHAVIOUR_SET,
            "id": "tiny",
            "phony": ["normalise"],
            "targets": [{
                "target": "normalise",
                "steps": [
                    { "op": "shell", "command": norm, "requires": ["sh"] },
                    { "op": "shell", "command": release, "requires": ["sh"] },
                    { "op": "shell", "command": qc, "requires": ["python3"] },
                    { "op": "owlmake-cli", "command": "diff",
                      "args": ["--left", "build/efo.owl", "--right", "../../efo.owl"] },
                    { "op": "print", "message": message },
                ],
            }],
        }))
        .unwrap();

        let path = base.join("owlmake.yaml");
        save(&spec, &path).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&back.targets).unwrap(),
            serde_json::to_value(&spec.targets).unwrap(),
            "free text did not come back byte for byte"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        for want in [norm, release, qc, "- build/efo.owl", "- ../../efo.owl", message] {
            assert!(text.contains(want), "`{want}` is not written as it runs:\n{text}");
        }

        std::fs::remove_dir_all(&base).ok();
    }
}


#[cfg(test)]
mod format_floor_tests {
    /// The plan format's minimum-version floor is hand-maintained (a generator
    /// version is the wrong thing to stamp — see `PLAN_FORMAT_MIN_VERSION`), so
    /// this test makes forgetting it impossible: any change to `OwlmakeSpec` or
    /// `StepSpec` changes the emitted JSON Schema and reddens this assertion.
    ///
    /// To fix a failure: decide whether the change is one an older owlmake could
    /// still execute. If it is not, raise `PLAN_FORMAT_MIN_VERSION`. Then update
    /// the digest below in the SAME commit.
    #[test]
    fn plan_schema_is_pinned() {
        // Updated deliberately, in the same commit as any schema change.
        //
        // `emulate_odk_version` arrives beside `emulate_robot_version`, both
        // `#[serde(default)]`, so a plan written before it still loads: neither
        // is present, and the current tool generation is read, which is what the
        // build did before the field existed. PLAN_FORMAT_MIN_VERSION stays put.
        //
        // The two are separate facts rather than one renamed. A repo built by the
        // ODK image states its RELEASE, and that settles the extended prefix map
        // as well as the tool — the two images' maps differ by 388 prefixes. A
        // repo shipping its own tool (EFO launches `../../bin/robot` at 1.9.7
        // inside a 1.6.1 image) states the TOOL and no release. Recording both is
        // refused unless they agree, because a plan saying two things about one
        // behaviour cannot be obeyed.
        //
        // `add-prefix` joins them: the launcher's own `--prefix`/`--add-prefix`,
        // which binds for a whole chain and which nothing else in the plan carried.
        // A plan written before the step existed still loads and still describes
        // the build it described — the step is simply absent — so
        // PLAN_FORMAT_MIN_VERSION stays put here too.
        //
        // `boundary` replaces `merge`'s `restart` flag. Where the pipeline starts
        // over is a fact about the invocation, not about one of the operations
        // inside it, so it is its own step and carries the invocation's `input`.
        // A plan carrying the old flag would lose the boundary rather than
        // mis-execute it, and there is nothing to migrate, so the floor stays put.
        // `version_file` arrived as an OPTIONAL field with a default, so a plan
        // written before it loads unchanged and one written with it is ignored by
        // a build that does not know it — only the frozen version comes back.
        // That is a format an older owlmake can still execute, so the floor stays.
        //
        // `merged_import_shards` and `merged_import_shard_bytes` are optional with
        // no default behaviour change: a plan without them is the single-file
        // merged import every owlmake wrote. A plan WITH them is executable only
        // by a build that shards and reads `rewriteURI` catalogs, and an older one
        // stops with an unresolved import rather than mis-executing — a repo that
        // opts in pins `min_owlmake_version` itself (EFO: 0.2.11). The floor stays.
        //
        // `may_fail` arrives as an optional field on every step, flattened beside
        // the `op` it applies to — the first setting that belongs to a step rather
        // than to an operation, which is why it is a field of `StepEntry` and not
        // of each op in turn. `owlmake-cli` and `unsupported-subcommand` are what
        // the two subcommand steps are called: a step naming the tool it runs —
        // `jq`, `sssom` — says which tool, and these two run owlmake's own CLI.
        //
        // The floor moves to 0.2.0 for both. A 0.1.0 build reading a 0.2.0 plan
        // does not fail on `may_fail`, it IGNORES it, and runs a step the plan
        // says may fail as one that may not — a silent change of what the build
        // does, which is the case the floor exists to refuse.
        //
        // `intermediate` (an artefact only pattern-rule chains name), the copy
        // step's `relative` (rsync -R) and `side_effect_only` (a recipe that
        // never writes its own target) all default off, and an older build
        // ignoring them over-builds rather than mis-builds, so the floor stays.
        //
        // A file that states no `version` or `ontology_iri` now asks for the standard build: its
        // top-level keys are the repository's options, `targets` holds what it
        // builds its own way (with `extends` and `when`), and the rest is
        // owlmake's built-in rules. A whole plan no longer carries `components`,
        // which nothing but the display read — the build merges `OTHER_SRC`.
        //
        // The floor moves to 0.3.0. An older build reads a standard file as a plan
        // with no artefacts and builds nothing, reporting success; that is the
        // silent case the floor exists to refuse.
        //
        // The options a repository states by the tool ODK runs them with are
        // now named for what they set (`report`, `relax_options`,
        // `reduce_options`, `dosdp_options`), and the ones that configure a tool
        // owlmake does not run are refused. Only the schema's description
        // changed with the option names; an older build refuses a file that
        // names `report` as an unknown key, loudly, so the floor stays.
        //
        // A reason step carries `axiom_generators` and `properties`: which
        // inference types it asserts, and which object properties its
        // property assertions are restricted to. A step's fields are not
        // refused when unknown, so a 0.3.1 build reading a plan that asks for
        // property assertions would reason without them and release an
        // ontology missing what the plan says it holds. That is the silent
        // case, so the floor moves to 0.3.2.
        //
        // One shape of file. `use_builtin_rules` says whether the standard build
        // underlies it; `artefacts` and `prerequisites` are one `targets` list
        // whose entries may say `artefact`; `default_targets` and
        // `refresh_groups` are stated or derived, so they are optional; and the
        // two emulation versions are plain options with defaults. A 0.3.2
        // build reading a file of this shape refuses it — `use_builtin_rules` and
        // `artefact` are unknown to it, loudly — and this build refuses the
        // old `artefacts` key the same way, so nothing is silently built
        // differently and the floor stays.
        // An import entry gains `extends` — append the entry's steps to the
        // standard pipeline — and with it `source` and `output` become optional,
        // since an extending entry derives both. An older build reading a plan
        // that uses either refuses it loudly (`ImportSpec` denies unknown fields,
        // and an old full entry still states source and output), so nothing is
        // silently built differently and the floor stays.
        const PLAN_SCHEMA_DIGEST: &str = "81c800a22a269d61";
        let actual = super::schema_digest();
        assert_eq!(
            actual, PLAN_SCHEMA_DIGEST,
            "the plan schema changed.\n\
             Decide whether an older owlmake could still execute the new format:\n\
             * it could  -> leave PLAN_FORMAT_MIN_VERSION alone;\n\
             * it could not -> raise PLAN_FORMAT_MIN_VERSION.\n\
             Then set PLAN_SCHEMA_DIGEST to {actual} in the same commit."
        );
    }
}

#[cfg(test)]
mod round_trip_tests {
    use super::*;

    /// `Plan -> Spec -> Plan` must be the identity for everything the executor
    /// reads.
    ///
    /// This catches what the module boundary cannot: a field that ingest computes
    /// and execution reads but `spec.rs` forgets to carry. Such a field works
    /// perfectly while the repo can still be re-ingested, and silently reverts to
    /// its default once the plan is all there is. `ImportSpec::source` (serialized,
    /// then overwritten on load) and `on_release_path` (re-derived by position
    /// rather than read back) are both that shape.
    #[test]
    fn plan_survives_a_spec_round_trip() {
        let plan = Plan {
            id: "tiny".into(),
            version: "2026-08-07".into(),
            version_file: None,
            ontology_iri: "http://example.org/tiny.owl".into(),
            reasoner: "ELK".into(),
            use_base_merging: true,
            exclude_iri_patterns: vec!["http://example.org/x*".into()],
            slme_individuals: Some("minimal".into()),
            imports: vec![],
            merged_import: Some("imports/merged_import.owl".into()),
            merged_import_iri: None,
            merged_import_shards: None,
            merged_import_shard_bytes: None,
            variables: [("OTHER_SRC".to_string(), "components/c.owl".to_string())]
                .into_iter()
                .collect(),
            component_gaps: vec![],
            prerequisites: vec![ArtefactPlan {
                target: "tmp/seed.txt".into(),
                input: None,
                needs: vec![],
                order_only: vec![],
                steps: vec![Step::File(crate::build::recipe::FileOp::Touch {
                    paths: vec!["tmp/seed.txt".into()],
                })],
                gaps: vec![],
                missing_rule: false,
                side_effect_only: false,
                stdout_file: None,
                intermediate: false,
                branches: vec![],
            }],
            artefacts: vec![ArtefactPlan {
                target: "tiny.owl".into(),
                input: Some("tiny-edit.ofn".into()),
                needs: vec!["tiny-edit.ofn".into()],
                order_only: vec![],
                steps: vec![Step::Op(Op::Merge {
                    inputs: vec!["tiny-edit.ofn".into()],
                    collapse_import_closure: None,
                })],
                gaps: vec![],
                missing_rule: false,
                side_effect_only: false,
                stdout_file: None,
                intermediate: false,
                branches: vec![crate::plan::Branch {
                    flag: "BRI".into(),
                    value: "false".into(),
                    input: None,
                    needs: vec![],
                    steps: vec![Step::File(crate::build::recipe::FileOp::Touch {
                        paths: vec!["tiny.owl".into()],
                    })],
                }],
            }],
            default_targets: vec!["tiny.owl".into(), "qc".into()],
            phony: vec!["qc".into()],
            transient_targets: vec![],
            native_targets: vec![],
            edit_file: Some("tiny-edit.ofn".into()),
            catalog_file: Some("catalog-v001.xml".into()),
            dosdp: None,
            emulate_odk_version: Some((1, 6, 1)),
            emulate_robot_version: (1, 9, 10),
            strict: true,
            xml_entities: true,
            refresh_groups: vec![crate::plan::RefreshGroup {
                name: "imports".into(),
                flag: "IMP".into(),
                targets: vec!["imports/merged_import.owl".into()],
                default: crate::plan::Freshness::Keep,
            }],
            gating_flags: [("BRI".to_string(), "true".to_string())].into_iter().collect(),
        };

        let back = OwlmakeSpec::from_plan(&plan).into_plan(std::path::Path::new("."));

        // Every field the executor reads must survive. Listed one by one rather
        // than compared wholesale so a NEW field forces a decision here.
        assert_eq!(back.id, plan.id);
        // A target's place in the release survives: the one list of targets says
        // which are artefacts, and the split comes back as it went.
        assert_eq!(back.artefacts.len(), 1, "the artefact was not read back as one");
        assert_eq!(back.artefacts[0].target, "tiny.owl");
        assert_eq!(back.prerequisites.len(), 1, "the prerequisite was not read back as one");
        assert_eq!(back.prerequisites[0].target, "tmp/seed.txt");
        assert_eq!(back.version, plan.version);
        assert_eq!(back.ontology_iri, plan.ontology_iri);
        assert_eq!(back.reasoner, plan.reasoner);
        assert_eq!(back.use_base_merging, plan.use_base_merging);
        assert_eq!(back.exclude_iri_patterns, plan.exclude_iri_patterns);
        assert_eq!(back.slme_individuals, plan.slme_individuals);
        assert_eq!(back.merged_import, plan.merged_import);
        assert_eq!(back.variables, plan.variables);
        assert_eq!(back.default_targets, plan.default_targets, "default_targets was dropped");
        assert_eq!(back.phony, plan.phony, "phony was dropped");
        assert_eq!(back.edit_file, plan.edit_file, "edit_file was dropped");
        assert_eq!(back.catalog_file, plan.catalog_file, "catalog_file was dropped");
        assert_eq!(
            back.emulate_robot_version, plan.emulate_robot_version,
            "emulate_robot_version was dropped — every .json artefact changes shape, and \
             so does every artefact downstream of a `query --update`"
        );
        assert_eq!(back.strict, plan.strict, "strict was dropped");
        assert_eq!(back.xml_entities, plan.xml_entities, "xml_entities was dropped");
        assert_eq!(
            back.artefacts[0].branches.len(),
            1,
            "branches was dropped — flipping a switch would then leave the target with no \
             recipe at all, which is the fault the field exists to prevent"
        );
        assert_eq!(back.artefacts[0].branches[0].flag, "BRI");
        assert_eq!(back.artefacts[0].branches[0].value, "false");
        assert_eq!(
            back.gating_flags, plan.gating_flags,
            "gating_flags was dropped — a switch the plan cannot vary would then \
             be silently accepted"
        );
    }

    /// `into_plan` takes a `&Path`, not a repo. If this stops compiling because
    /// the parameter has widened to `&OdkRepo`, that is the failure it guards:
    /// loading a committed plan must not be able to consult anything but the plan.
    #[test]
    fn loading_a_plan_needs_only_a_directory() {
        let spec = OwlmakeSpec::from_plan(&Plan {
            id: "x".into(),
            version: "1".into(),
            version_file: None,
            ontology_iri: "http://example.org/x.owl".into(),
            reasoner: "ELK".into(),
            use_base_merging: false,
            exclude_iri_patterns: vec![],
            slme_individuals: None,
            imports: vec![],
            merged_import: None,
            merged_import_iri: None,
            merged_import_shards: None,
            merged_import_shard_bytes: None,
            variables: Default::default(),
            component_gaps: vec![],
            prerequisites: vec![],
            artefacts: vec![],
            default_targets: vec![],
            phony: vec![],
            transient_targets: vec![],
            native_targets: vec![],
            edit_file: None,
            catalog_file: None,
            dosdp: None,
            emulate_odk_version: Some((1, 6, 0)),
            emulate_robot_version: (1, 9, 8),
            strict: false,
            xml_entities: false,
            refresh_groups: vec![],
            gating_flags: Default::default(),
        });
        let _: Plan = spec.into_plan(std::path::Path::new("/nonexistent"));
    }
}
