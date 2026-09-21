//! The standard build, as rules.
//!
//! What a repository's configuration implies — its release artefacts, import
//! modules, components, subsets and checks — expressed as the rule model the
//! planner consumes. A repository says WHAT it builds in its configuration; HOW
//! each of those things is built is owlmake's, and lives here.
//!
//! The rules are data: [`model`] is one function from a [`Config`] to a
//! [`MakeModel`], adding each rule through [`MakeModel::add_rule`]. Where the
//! build has one rule per import, component or subset, this loops over them.
//! Recipes are written in owlmake's command language, and the variables they
//! name (`$(TMPDIR)`, `$(OTHER_SRC)`, `$(ANNOTATE_CONVERT_FILE)`, …) are the
//! vocabulary a repository's own rules are written against, so those rules read
//! the same whichever way the model was produced.
//!
//! A configuration option these rules do not cover is refused by name
//! ([`Config::parse`]): a build that quietly ignored `use_dosdps: true` would
//! release an ontology without its pattern-derived classes.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::makefile::{MakeModel, Rule};

/// Options that change the standard build and have no built-in rules yet, as
/// paths into the configuration. A configuration that sets one is refused: built
/// without it, the result would differ from what the repository asked for, and
/// nothing would say so.
const UNPORTED: &[&str] = &[
    "use_translations",
    "babelon_translation_group",
    "use_custom_import_module",
    "custom_makefile_header",
    "robot_plugins",
];

/// The release artefacts the standard build knows how to make.
const VARIANTS: &[&str] = &[
    "base",
    "baselite",
    "full",
    "non-classified",
    "simple",
    "simple-non-classified",
    "international",
    "basic",
];

/// The behaviour set these rules implement: the build an ODK release of this
/// version generates for the same configuration.
pub const BEHAVIOUR_SET: &str = "1.6.1";

// === Configuration ==========================================================

/// A repository's build configuration, as far as it decides what is built.
///
/// Read as ODK reads it: a key that is not an option is ignored, and so is an
/// option that shapes something other than the build (the repository's title, its
/// CI workflows, how its container is run). An option that DOES shape the build
/// and that these rules do not cover yet is refused by name — see [`UNPORTED`].
#[derive(Debug, Deserialize)]
pub struct Config {
    pub id: String,
    #[serde(default = "default_uribase")]
    pub uribase: String,
    #[serde(default)]
    pub uribase_suffix: Option<String>,
    #[serde(default = "default_edit_format")]
    pub edit_format: String,
    #[serde(default = "default_reasoner")]
    pub reasoner: String,
    #[serde(default = "default_release_artefacts")]
    pub release_artefacts: Vec<String>,
    #[serde(default = "default_primary_release")]
    pub primary_release: String,
    #[serde(default = "default_export_formats")]
    pub export_formats: Vec<String>,
    #[serde(default)]
    pub import_group: Option<ImportGroup>,
    #[serde(default)]
    pub subset_group: Option<SubsetGroup>,
    #[serde(default)]
    pub components: Option<ComponentGroup>,
    #[serde(default)]
    pub robot_report: RobotReport,
    #[serde(default = "default_catalog_file")]
    pub catalog_file: String,
    /// Read prefixes from `config/context.json` in every command.
    #[serde(default)]
    pub use_context: bool,
    /// The namespaces that are this ontology's own. Defaults to the one its id
    /// implies under `uribase`.
    #[serde(default)]
    pub namespaces: Option<Vec<String>>,
    #[serde(default = "default_allow_equivalents")]
    pub allow_equivalents: String,
    #[serde(default = "default_exclude_tautologies")]
    pub exclude_tautologies: String,
    /// Classify the release artefacts. Off, they are relaxed and cut, not reasoned.
    #[serde(default = "yes")]
    pub release_use_reasoner: bool,
    #[serde(default)]
    pub release_annotate_inferred_axioms: bool,
    #[serde(default)]
    pub release_materialize_object_properties: Option<Vec<String>>,
    #[serde(default)]
    pub remove_owl_nothing: bool,
    /// Stamp `oboInOwl:date` on the artefacts.
    #[serde(default)]
    pub release_date: bool,
    /// Publish a gzipped copy of each export of the primary product.
    #[serde(default)]
    pub gzip_main: bool,
    /// Merge the imports the edit file declares; off, merge the import modules
    /// the configuration lists.
    #[serde(default = "yes")]
    pub use_edit_file_imports: bool,
    #[serde(default)]
    pub obo_format_options: String,
    #[serde(default = "default_relax_options")]
    pub robot_relax_options: String,
    #[serde(default = "default_reduce_options")]
    pub robot_reduce_options: String,
    /// The serialization components and import modules are written in.
    #[serde(default = "default_import_component_format")]
    pub import_component_format: String,
    #[serde(default = "yes")]
    pub ensure_valid_rdfxml: bool,
    #[serde(default)]
    pub extra_rdfxml_checks: bool,
    /// Compare each release with the one published, as part of the build.
    #[serde(default)]
    pub release_diff: bool,
    /// Kept for repositories whose own rules still call the older tool.
    #[serde(default)]
    pub owltools_memory: String,

    /// Maintain mapping sets alongside the ontology.
    #[serde(default)]
    pub use_mappings: bool,
    #[serde(default)]
    pub sssom_mappingset_group: Option<MappingSetGroup>,
    /// Generate classes from design patterns and their data tables.
    #[serde(default)]
    pub use_dosdps: bool,
    #[serde(default = "default_dosdp_tools_options")]
    pub dosdp_tools_options: String,
    #[serde(default)]
    pub pattern_pipelines_group: Option<PatternPipelineGroup>,
    /// Where the repository is hosted: the pattern documentation links its data
    /// tables there.
    #[serde(default)]
    pub repo_url: String,
    #[serde(default)]
    pub github_org: String,
    #[serde(default = "default_repo")]
    pub repo: String,
    #[serde(default = "default_git_main_branch")]
    pub git_main_branch: String,
    /// How a release is published to the repository's host: `none`,
    /// `github_curl` or `github_python`.
    #[serde(default = "default_public_release")]
    pub public_release: String,
    /// The files published with it. Defaults to every asset.
    #[serde(default)]
    pub public_release_assets: Option<Vec<String>>,
    /// How the documentation is built. Only whether it is configured matters here.
    #[serde(default)]
    pub documentation: Option<serde_yaml::Value>,

}

/// The mapping sets, each kept up to date in the way its `maintenance` says.
#[derive(Debug, Deserialize)]
pub struct MappingSetGroup {
    /// Publish every set with the release, not only those that ask for it.
    #[serde(default)]
    pub release_mappings: bool,
    /// What extracts a set from the ontology's cross-references: `sssom-py` or
    /// `sssom-java`.
    #[serde(default = "default_mapping_extractor")]
    pub mapping_extractor: String,
    #[serde(default)]
    pub products: Option<Vec<MappingSet>>,
}

#[derive(Debug, Deserialize)]
pub struct MappingSet {
    pub id: String,
    /// `manual`, `extract`, `merged`, `mirror` or `custom`.
    #[serde(default = "default_maintenance")]
    pub maintenance: String,
    #[serde(default)]
    pub mirror_from: Option<String>,
    /// What an extracted set is read from. Defaults to the preprocessed edit file.
    #[serde(default)]
    pub source_file: Option<String>,
    #[serde(default)]
    pub sssom_tool_options: Option<String>,
    #[serde(default)]
    pub release_mappings: bool,
    /// The sets a merged set is made of. Defaults to every set that is not merged.
    #[serde(default)]
    pub source_mappings: Option<Vec<String>>,
}

impl MappingSetGroup {
    fn derive(&mut self) -> Result<()> {
        let Some(products) = &mut self.products else { return Ok(()) };
        let ids: Vec<String> = products.iter().map(|p| p.id.clone()).collect();
        let unmerged: Vec<String> =
            products.iter().filter(|p| p.maintenance != "merged").map(|p| p.id.clone()).collect();
        for p in products.iter_mut() {
            match p.maintenance.as_str() {
                "merged" => match &p.source_mappings {
                    None => p.source_mappings = Some(unmerged.clone()),
                    Some(sources) => {
                        if let Some(unknown) = sources.iter().find(|s| !ids.contains(s)) {
                            bail!("mapping set `{}` merges `{unknown}`, which is not a mapping set", p.id);
                        }
                    }
                },
                "extract" => {
                    p.source_file.get_or_insert_with(|| "$(EDIT_PREPROCESSED)".to_string());
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn sets(&self) -> &[MappingSet] {
        self.products.as_deref().unwrap_or(&[])
    }

    /// The sets published with a release.
    fn released(&self) -> Vec<&MappingSet> {
        self.sets().iter().filter(|p| self.release_mappings || p.release_mappings).collect()
    }
}

/// The pattern data directories beyond `default`, each generated on its own.
#[derive(Debug, Deserialize)]
pub struct PatternPipelineGroup {
    #[serde(default)]
    pub products: Vec<PatternPipeline>,
    /// Pipelines that MATCH the patterns against an ontology rather than
    /// generating from them.
    #[serde(default)]
    pub matches: Option<Vec<PatternPipeline>>,
    #[serde(default)]
    pub ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PatternPipeline {
    pub id: String,
    #[serde(default = "default_dosdp_tools_options")]
    pub dosdp_tools_options: String,
    #[serde(default = "default_pipeline_ontology")]
    pub ontology: String,
}

impl PatternPipelineGroup {
    fn derive(&mut self) {
        for id in std::mem::take(&mut self.ids) {
            if !self.products.iter().any(|p| p.id == id) {
                self.products.push(PatternPipeline {
                    id,
                    dosdp_tools_options: default_dosdp_tools_options(),
                    ontology: default_pipeline_ontology(),
                });
            }
        }
    }
}

/// The import modules, and what holds for all of them unless a product says
/// otherwise.
#[derive(Debug, Deserialize)]
pub struct ImportGroup {
    #[serde(default)]
    pub products: Vec<ImportProduct>,
    /// Product ids given bare; each is a product with every default.
    #[serde(default)]
    pub ids: Vec<String>,
    /// `slme`, `minimal`, `mirror`, `filter` or `custom`.
    #[serde(default = "default_module_type")]
    pub module_type: String,
    /// The locality module kind: `BOT`, `TOP` or `STAR`.
    #[serde(default = "default_module_type_slme")]
    pub module_type_slme: String,
    #[serde(default = "default_slme_individuals")]
    pub slme_individuals: String,
    #[serde(default = "default_mirror_retry")]
    pub mirror_retry_download: u32,
    #[serde(default = "default_mirror_max_time")]
    pub mirror_max_time_download: u32,
    /// Copy the modules to the release directory with the artefacts.
    #[serde(default)]
    pub release_imports: bool,
    /// Merge every mirror, then cut ONE module from the merged whole.
    #[serde(default)]
    pub use_base_merging: bool,
    #[serde(default)]
    pub base_merge_drop_equivalent_class_axioms: bool,
    #[serde(default)]
    pub exclude_iri_patterns: Option<Vec<String>>,
    /// Write each module in OBO format as well.
    #[serde(default)]
    pub export_obo: bool,
    #[serde(default = "default_annotation_properties")]
    pub annotation_properties: Vec<String>,
    #[serde(default = "yes")]
    pub strip_annotation_properties: bool,
    #[serde(default)]
    pub annotate_defined_by: bool,
    /// Seed the modules from what the edit file refers to, not only from the
    /// committed term lists.
    #[serde(default = "yes")]
    pub scan_signature: bool,

}

#[derive(Debug, Deserialize)]
pub struct ImportProduct {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Fetch the ontology from here instead of its OBO PURL.
    #[serde(default)]
    pub mirror_from: Option<String>,
    /// The namespaces that are this ontology's own. Defaults to its OBO one.
    #[serde(default)]
    pub base_iris: Option<Vec<String>>,
    /// Rebuilt only while `IMP_LARGE` is on.
    #[serde(default)]
    pub is_large: bool,
    /// Defaults to the group's.
    #[serde(default)]
    pub module_type: Option<String>,
    #[serde(default)]
    pub module_type_slme: Option<String>,
    #[serde(default = "default_annotation_properties")]
    pub annotation_properties: Vec<String>,
    #[serde(default)]
    pub slme_individuals: Option<String>,
    /// Mirror the ontology's published base product.
    #[serde(default)]
    pub use_base: bool,
    /// Cut the mirror down to the ontology's own axioms.
    #[serde(default)]
    pub make_base: bool,
    #[serde(default)]
    pub use_gzipped: bool,
    /// `base`, `custom` or `no_mirror`.
    #[serde(default)]
    pub mirror_type: Option<String>,

}

impl ImportProduct {
    fn stub(id: &str) -> ImportProduct {
        ImportProduct {
            id: id.to_string(),
            description: None,
            mirror_from: None,
            base_iris: None,
            is_large: false,
            module_type: None,
            module_type_slme: None,
            annotation_properties: default_annotation_properties(),
            slme_individuals: None,
            use_base: false,
            make_base: false,
            use_gzipped: false,
            mirror_type: None,
        }
    }

    fn kind(&self) -> &str {
        self.module_type.as_deref().unwrap_or("slme")
    }

    /// Whether the mirror is cut down to the ontology's own axioms.
    fn mirrors_base(&self) -> bool {
        self.make_base || self.mirror_type.as_deref() == Some("base")
    }
}

impl ImportGroup {
    /// The group of a repository that configures none.
    fn empty() -> ImportGroup {
        ImportGroup {
            products: Vec::new(),
            ids: Vec::new(),
            module_type: default_module_type(),
            module_type_slme: default_module_type_slme(),
            slme_individuals: default_slme_individuals(),
            mirror_retry_download: default_mirror_retry(),
            mirror_max_time_download: default_mirror_max_time(),
            release_imports: false,
            use_base_merging: false,
            base_merge_drop_equivalent_class_axioms: false,
            exclude_iri_patterns: None,
            export_obo: false,
            annotation_properties: default_annotation_properties(),
            strip_annotation_properties: true,
            annotate_defined_by: false,
            scan_signature: true,
        }
    }

    /// Settle what each product inherits from the group.
    fn derive(&mut self) {
        for id in std::mem::take(&mut self.ids) {
            if !self.products.iter().any(|p| p.id == id) {
                self.products.push(ImportProduct::stub(&id));
            }
        }
        for p in &mut self.products {
            match p.module_type.as_deref() {
                None => p.module_type = Some(self.module_type.clone()),
                Some("fast_slme") => p.module_type = Some("slme".into()),
                Some(_) => {}
            }
            if p.kind() == "slme" {
                p.module_type_slme.get_or_insert_with(|| self.module_type_slme.clone());
                p.slme_individuals.get_or_insert_with(|| self.slme_individuals.clone());
            }
            p.base_iris.get_or_insert_with(|| {
                vec![format!("http://purl.obolibrary.org/obo/{}", p.id.to_uppercase())]
            });
        }
    }

    /// A product the group's one rule does not build: a large one, or one of
    /// another kind than the group's.
    fn is_special(&self, p: &ImportProduct) -> bool {
        p.is_large
            || p.kind() != self.module_type
            || (p.kind() == "slme"
                && p.module_type_slme.as_deref() != Some(self.module_type_slme.as_str()))
    }
}

#[derive(Debug, Deserialize)]
pub struct SubsetGroup {
    #[serde(default)]
    pub products: Vec<SubsetProduct>,
}

#[derive(Debug, Deserialize)]
pub struct SubsetProduct {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct ComponentGroup {
    #[serde(default)]
    pub products: Vec<ComponentProduct>,
}

impl ComponentGroup {
    /// Settle what a component's options imply when it does not spell them out.
    fn derive(&mut self, uribase: &str, id: &str) {
        for p in &mut self.products {
            let stem = p.filename.split('.').next().unwrap_or_default().to_string();
            p.base_iris.get_or_insert_with(|| vec![format!("{uribase}/{}", id.to_uppercase())]);
            if p.use_template {
                p.templates.get_or_insert_with(|| vec![format!("{stem}.tsv")]);
            } else if p.use_mappings {
                p.mappings.get_or_insert_with(|| vec![format!("{stem}.sssom.tsv")]);
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ComponentProduct {
    pub filename: String,
    /// Fetch the component from here.
    #[serde(default)]
    pub source: Option<String>,
    /// Build it from template tables. Defaults to `<stem>.tsv`.
    #[serde(default)]
    pub use_template: bool,
    /// Build it from mapping sets. Defaults to `<stem>.sssom.tsv`.
    #[serde(default)]
    pub use_mappings: bool,
    #[serde(default)]
    pub template_options: Option<String>,
    #[serde(default)]
    pub sssom_tool_options: Option<String>,
    #[serde(default)]
    pub templates: Option<Vec<String>>,
    #[serde(default)]
    pub mappings: Option<Vec<String>>,
    /// The namespaces that are a fetched component's own.
    #[serde(default)]
    pub base_iris: Option<Vec<String>>,
    /// Cut a fetched component down to its own axioms.
    #[serde(default)]
    pub make_base: bool,
}

/// How the QC report and the SPARQL checks are run.
///
/// A configuration that says nothing about the report gets every default below.
/// One that says ANYTHING gets defaults only for the options the build falls back
/// on by itself: the rest read as unset, so `use_base_iris` is off unless stated
/// and the default checks are the four that do not include `dc-properties`.
#[derive(Debug, Deserialize)]
#[serde(from = "Option<StatedReport>")]
pub struct RobotReport {
    pub fail_on: Option<String>,
    pub use_labels: bool,
    pub use_base_iris: bool,
    /// A repository's own report profile, `profile.txt`.
    pub custom_profile: bool,
    /// Publish the reports with the release.
    pub release_reports: bool,
    pub report_on: Vec<String>,
    pub sparql_test_on: Vec<String>,
    pub custom_sparql_checks: Vec<String>,
    pub custom_sparql_exports: Vec<String>,
    /// Check the released ontology against the OWL 2 DL profile as part of `test`.
    pub ensure_owl2dl_profile: bool,
    /// Report how the ontology's classes align with this upper ontology.
    pub upper_ontology: Option<String>,
}

/// The report options as a configuration states them.
#[derive(Debug, Deserialize)]
struct StatedReport {
    fail_on: Option<String>,
    use_labels: Option<bool>,
    use_base_iris: Option<bool>,
    custom_profile: Option<bool>,
    release_reports: Option<bool>,
    report_on: Option<Vec<String>>,
    sparql_test_on: Option<Vec<String>>,
    custom_sparql_checks: Option<Vec<String>>,
    custom_sparql_exports: Option<Vec<String>>,
    ensure_owl2dl_profile: Option<bool>,
    upper_ontology: Option<String>,
}

impl From<Option<StatedReport>> for RobotReport {
    fn from(stated: Option<StatedReport>) -> Self {
        let Some(s) = stated else { return RobotReport::default() };
        RobotReport {
            fail_on: s.fail_on,
            use_labels: s.use_labels.unwrap_or(true),
            use_base_iris: s.use_base_iris.unwrap_or(false),
            custom_profile: s.custom_profile.unwrap_or(false),
            release_reports: s.release_reports.unwrap_or(false),
            report_on: s.report_on.unwrap_or_else(default_on_edit),
            sparql_test_on: s.sparql_test_on.unwrap_or_else(default_on_edit),
            custom_sparql_checks: s.custom_sparql_checks.unwrap_or_else(|| {
                strings(&["owldef-self-reference", "iri-range", "label-with-iri", "multiple-replaced_by"])
            }),
            custom_sparql_exports: s.custom_sparql_exports.unwrap_or_else(default_sparql_exports),
            ensure_owl2dl_profile: s.ensure_owl2dl_profile.unwrap_or(true),
            upper_ontology: s.upper_ontology,
        }
    }
}

impl Default for RobotReport {
    fn default() -> Self {
        RobotReport {
            fail_on: None,
            use_labels: true,
            use_base_iris: true,
            custom_profile: false,
            release_reports: false,
            report_on: default_on_edit(),
            sparql_test_on: default_on_edit(),
            custom_sparql_checks: default_sparql_checks(),
            custom_sparql_exports: default_sparql_exports(),
            ensure_owl2dl_profile: true,
            upper_ontology: None,
        }
    }
}

fn yes() -> bool {
    true
}
fn default_uribase() -> String {
    "http://purl.obolibrary.org/obo".into()
}
fn default_edit_format() -> String {
    "owl".into()
}
fn default_reasoner() -> String {
    "ELK".into()
}
fn default_dosdp_tools_options() -> String {
    "--obo-prefixes=true".into()
}
fn default_pipeline_ontology() -> String {
    "$(SRC)".into()
}
fn default_mapping_extractor() -> String {
    "sssom-py".into()
}
fn default_maintenance() -> String {
    "manual".into()
}
fn default_public_release() -> String {
    "none".into()
}
fn default_repo() -> String {
    "noname".into()
}
fn default_git_main_branch() -> String {
    "main".into()
}
fn default_catalog_file() -> String {
    "catalog-v001.xml".into()
}
fn default_allow_equivalents() -> String {
    "asserted-only".into()
}
fn default_exclude_tautologies() -> String {
    "structural".into()
}
fn default_relax_options() -> String {
    "--include-subclass-of true".into()
}
fn default_reduce_options() -> String {
    "--include-subproperties true".into()
}
fn default_import_component_format() -> String {
    "ofn".into()
}
fn default_primary_release() -> String {
    "full".into()
}
fn default_module_type() -> String {
    "slme".into()
}
fn default_module_type_slme() -> String {
    "BOT".into()
}
fn default_slme_individuals() -> String {
    "include".into()
}
fn default_mirror_retry() -> u32 {
    4
}
fn default_mirror_max_time() -> u32 {
    200
}
fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}
fn default_release_artefacts() -> Vec<String> {
    strings(&["full", "base"])
}
fn default_export_formats() -> Vec<String> {
    strings(&["owl", "obo"])
}
fn default_annotation_properties() -> Vec<String> {
    strings(&["rdfs:label", "IAO:0000115", "OMO:0002000"])
}
fn default_on_edit() -> Vec<String> {
    strings(&["edit"])
}
fn default_sparql_checks() -> Vec<String> {
    strings(&[
        "owldef-self-reference",
        "iri-range",
        "label-with-iri",
        "multiple-replaced_by",
        "dc-properties",
    ])
}
fn default_sparql_exports() -> Vec<String> {
    strings(&["basic-report", "class-count-by-prefix", "edges", "xrefs", "obsoletes", "synonyms"])
}

impl Config {
    /// Read a configuration, refusing any option the built-in rules do not cover.
    pub fn parse(text: &str) -> Result<Config> {
        let raw: serde_yaml::Value = serde_yaml::from_str(text).context("not a YAML document")?;
        for path in UNPORTED {
            let set = path.split('.').try_fold(&raw, |v, key| v.get(key));
            // An option spelled out at the value that leaves the build alone is
            // not a request for anything.
            let inert = |v: &serde_yaml::Value| {
                v.is_null() || v.as_bool() == Some(false) || v.as_str() == Some("")
            };
            if set.is_some_and(|v| !inert(v)) {
                bail!("the configuration sets `{path}`, which owlmake has no built-in rules for yet");
            }
        }
        let mut config: Config =
            serde_yaml::from_value(raw).context("reading the build configuration")?;
        if let Some(g) = &mut config.import_group {
            g.derive();
        }
        if let Some(g) = &mut config.components {
            g.derive(&config.uribase, &config.id);
        }
        if let Some(g) = &mut config.pattern_pipelines_group {
            g.derive();
        }
        if let Some(g) = &mut config.sssom_mappingset_group {
            g.derive()?;
        }
        // An OBO export is always cleaned; a configuration may only add to that.
        if !config.obo_format_options.contains("--clean-obo") {
            if !config.obo_format_options.is_empty() {
                config.obo_format_options.push(' ');
            }
            config.obo_format_options.push_str("--clean-obo \"strict drop-untranslatable-axioms\"");
        }
        config.check()?;
        Ok(config)
    }

    /// Options that parse but are covered for only some of their values.
    fn check(&self) -> Result<()> {
        for a in self.release_artefacts.iter().chain([&self.primary_release]) {
            if !VARIANTS.contains(&a.as_str()) && !a.starts_with("custom-") {
                bail!("release artefact `{a}` is not one of {VARIANTS:?} or `custom-<name>`");
            }
        }
        for f in &self.export_formats {
            if !matches!(f.as_str(), "owl" | "obo" | "json" | "ttl" | "db") {
                bail!("export format `{f}` is not one of owl, obo, json, ttl, db");
            }
        }
        let kinds = ["slme", "minimal", "mirror", "filter", "custom"];
        for g in &self.import_group {
            if !kinds.contains(&g.module_type.as_str()) {
                bail!("import_group.module_type `{}` is not one of {kinds:?}", g.module_type);
            }
            for p in &g.products {
                if !kinds.contains(&p.kind()) {
                    bail!("import `{}`: module_type `{}` is not one of {kinds:?}", p.id, p.kind());
                }
            }
        }
        Ok(())
    }

    /// Whether the build makes this artefact, as a release or as the source of
    /// the primary product.
    fn makes(&self, variant: &str) -> bool {
        self.primary_release == variant || self.release_artefacts.iter().any(|a| a == variant)
    }

    /// The namespaces that are this ontology's own, as `remove` wants them.
    fn own_namespaces(&self) -> String {
        match &self.namespaces {
            Some(ns) => ns.iter().map(|i| format!("--base-iri {i} ")).collect(),
            None => format!("--base-iri $(URIBASE)/{} ", self.id.to_uppercase()),
        }
    }

    fn pipelines(&self) -> &[PatternPipeline] {
        self.pattern_pipelines_group.as_ref().map(|g| g.products.as_slice()).unwrap_or(&[])
    }

    /// Where a pattern pipeline's data tables can be browsed, as the option the
    /// documentation generator takes — nothing when the hosting is not configured.
    fn data_location(&self, pipeline: &str) -> String {
        if !self.repo_url.is_empty() {
            format!(" --data-location-prefix={}/src/patterns/data/{pipeline}", self.repo_url)
        } else if !self.github_org.is_empty() && !self.repo.is_empty() {
            format!(
                " --data-location-prefix=https://github.com/{}/{}/tree/{}/src/patterns/data/{pipeline}",
                self.github_org, self.repo, self.git_main_branch
            )
        } else {
            String::new()
        }
    }

    fn imports(&self) -> &[ImportProduct] {
        self.import_group.as_ref().map(|g| g.products.as_slice()).unwrap_or(&[])
    }
    fn subsets(&self) -> &[SubsetProduct] {
        self.subset_group.as_ref().map(|g| g.products.as_slice()).unwrap_or(&[])
    }
    fn component_products(&self) -> &[ComponentProduct] {
        self.components.as_ref().map(|g| g.products.as_slice()).unwrap_or(&[])
    }
}

/// The file stem of a release artefact: `<id>-<variant>`, or the name a
/// `custom-<name>` artefact gives itself.
fn artefact_root(id: &str, artefact: &str) -> String {
    match artefact.strip_prefix("custom-") {
        Some(name) => name.to_string(),
        None => format!("{id}-{artefact}"),
    }
}

// === The rules ==============================================================

/// A rule under construction. Targets and prerequisites are expanded against the
/// model as they are added, recipes when they run — as for any rule.
struct Build<'a> {
    m: &'a mut MakeModel,
}

impl Build<'_> {
    fn var(&mut self, name: &str, value: impl Into<String>) {
        // What the run bound outranks the default.
        if !self.m.command_line_vars.contains(name) {
            self.m.vars.insert(name.to_string(), value.into());
        }
    }

    fn words(&self, s: &str) -> Vec<String> {
        self.m.expand(s).split_whitespace().map(str::to_string).collect()
    }

    /// `targets: needs | order_only`, built by `recipe`, existing only while every
    /// switch in `guards` is on.
    fn rule(&mut self, targets: &str, needs: &str, order_only: &str, recipe: &[&str], guards: &[&str]) {
        let rule = Rule {
            targets: self.words(targets),
            prereqs: self.words(needs),
            order_only: self.words(order_only),
            recipe: recipe.iter().map(|l| l.to_string()).collect(),
            guards: guards.iter().map(|g| g.to_string()).collect(),
        };
        self.m.add_rule(rule);
    }

    /// Files kept once built even where only a pattern reaches them, and so named
    /// outright: a build does not sweep what it lists here.
    fn precious(&mut self, targets: &str) {
        self.rule(".PRECIOUS", targets, "", &[], &[]);
    }

    /// A target that names no file: a group, a check, a command.
    fn phony(&mut self, target: &str, needs: &str, order_only: &str, recipe: &[&str], guards: &[&str]) {
        self.m.phony.extend(self.words(target));
        self.rule(target, needs, order_only, recipe, guards);
    }

    /// Whether a switch is on for this run, recording that the rules consult it.
    fn switch(&mut self, name: &str) -> bool {
        let value = self.m.expand(&format!("$({name})")).trim().to_string();
        self.m.switch_vars.insert(name.to_string());
        self.m.cond_vars.entry(name.to_string()).or_insert_with(|| value.clone());
        crate::plan::is_on(&value)
    }
}

/// The standard build for `config`, as a rule model rooted at `dir`.
///
/// `overrides` and `flags` are the run's assignments and switches, bound before
/// any rule is added: a switched-off group (`IMP=false`) has no rules at all, so
/// its files on disk stand. The release version is left unbound, for the caller
/// to bind once the repository's own rules have been added.
pub fn model(
    config: &Config,
    dir: &Path,
    overrides: &[(String, String)],
    flags: &[(&str, &str)],
) -> MakeModel {
    let mut m = MakeModel::with_flags(dir, overrides, flags);
    let mut b = Build { m: &mut m };
    let id = config.id.as_str();

    variables(&mut b, config);

    // --- Top level -----------------------------------------------------------
    b.phony("all", "all_odk", "", &[], &[]);
    b.phony(
        "all_odk",
        if config.release_diff {
            "test custom_reports all_assets release_diff"
        } else {
            "test custom_reports all_assets"
        },
        "",
        &[],
        &[],
    );
    b.phony(
        "test",
        &format!(
            "validate_idranges {}reason_test sparql_test robot_reports {}",
            if config.use_dosdps { "dosdp_validation " } else { "" },
            if config.robot_report.ensure_owl2dl_profile {
                "$(REPORTDIR)/validate_profile_owl2dl_$(ONT).owl.txt"
            } else {
                ""
            }
        ),
        "",
        &["echo \"Finished running all tests successfully.\""],
        &[],
    );
    b.rule("test_fast", "", "", &["$(MAKE_FAST) test"], &[]);
    b.rule(
        "$(TMPDIR) $(REPORTDIR) $(MIRRORDIR) $(IMPORTDIR) $(COMPONENTSDIR) $(SUBSETDIR)",
        "",
        "",
        &["mkdir -p $@"],
        &[],
    );
    b.phony("all_main", "$(MAIN_FILES)", "", &[], &[]);
    b.phony("all_imports", "$(IMPORT_FILES)", "", &[], &[]);
    b.phony("all_subsets", "$(SUBSET_FILES)", "", &[], &[]);
    b.phony("all_mappings", "$(MAPPING_FILES)", "", &[], &[]);
    b.phony(
        "all_assets",
        if config.ensure_valid_rdfxml { "$(ASSETS) check_rdfxml_assets" } else { "$(ASSETS)" },
        "",
        &[],
        &[],
    );
    b.phony("show_assets", "", "", &["echo $(ASSETS)", "du -sh $(ASSETS)"], &[]);

    checks(&mut b, config);
    release(&mut b, config);
    seeds(&mut b, config);
    imports(&mut b, config);
    components(&mut b, config);
    mirrors(&mut b, config);
    subsets(&mut b);
    patterns(&mut b, config);
    mappings(&mut b, config);
    artefacts(&mut b, config);
    utilities(&mut b, config);
    m
}

fn variables(b: &mut Build, c: &Config) {
    let id = c.id.as_str();
    b.var("OBOBASE", "http://purl.obolibrary.org/obo");
    b.var("URIBASE", c.uribase.as_str());
    b.var("ONT", id);
    b.var(
        "ONTBASE",
        format!("{}/{}", c.uribase, c.uribase_suffix.as_deref().unwrap_or(id)),
    );
    b.var("EDIT_FORMAT", c.edit_format.as_str());
    b.var("SRC", "$(ONT)-edit.$(EDIT_FORMAT)");
    b.var("MAKE_FAST", "$(MAKE) IMP=false PAT=false COMP=false MIR=false");
    b.var("CATALOG", c.catalog_file.as_str());
    if c.use_context {
        b.var("CONTEXT_FILE", "config/context.json");
        b.var("ROBOT", "om --catalog $(CATALOG) --add-prefixes $(CONTEXT_FILE)");
        if c.export_formats.iter().any(|f| f == "db") {
            b.var("CONTEXT_FILE_CSV", "$(TMPDIR)/context.csv");
        }
    } else {
        b.var("ROBOT", "om --catalog $(CATALOG)");
    }
    if c.owltools_memory.is_empty() {
        b.var("OWLTOOLS", "owltools --use-catalog");
    } else {
        b.var("OWLTOOLS_MEMORY", c.owltools_memory.as_str());
        b.var("OWLTOOLS", "OWLTOOLS_MEMORY=$(OWLTOOLS_MEMORY) owltools --use-catalog");
    }
    b.var("REASONER", c.reasoner.as_str());
    b.var("RELEASEDIR", "../..");
    b.var("DOCSDIR", "../../docs");
    b.var("REPORTDIR", "reports");
    b.var("TEMPLATEDIR", "../templates");
    b.var("TMPDIR", "tmp");
    b.var("MIRRORDIR", "mirror");
    b.var("IMPORTDIR", "imports");
    b.var("SUBSETDIR", "subsets");
    b.var("SCRIPTSDIR", "../scripts");
    b.var("UPDATEREPODIR", "target");
    b.var("SPARQLDIR", "../sparql");
    b.var("COMPONENTSDIR", "components");
    b.var("EXTENDED_PREFIX_MAP", "$(TMPDIR)/obo.epm.json");
    b.var("REPORT_FAIL_ON", c.robot_report.fail_on.as_deref().unwrap_or("None"));
    b.var("REPORT_LABEL", if c.robot_report.use_labels { "-l true" } else { "" });
    // A repository's own report profile, kept beside the edit file.
    if c.robot_report.custom_profile {
        b.var("ROBOT_PROFILE", "profile.txt");
        b.var("REPORT_PROFILE_OPTS", "--profile $(ROBOT_PROFILE)");
    } else {
        b.var("REPORT_PROFILE_OPTS", "");
    }
    b.var("OBO_FORMAT_OPTIONS", c.obo_format_options.as_str());
    b.var("SPARQL_VALIDATION_CHECKS", c.robot_report.custom_sparql_checks.join(" "));
    b.var("SPARQL_EXPORTS", c.robot_report.custom_sparql_exports.join(" "));
    b.var("ODK_VERSION_MAKEFILE", format!("v{BEHAVIOUR_SET}"));
    b.var("RELAX_OPTIONS", c.robot_relax_options.as_str());
    b.var("REDUCE_OPTIONS", c.robot_reduce_options.as_str());
    b.var("TODAY", "$(shell date +%Y-%m-%d)");
    b.var("OBODATE", "$(shell date +'%d:%m:%Y %H:%M')");
    b.var("VERSION", "$(TODAY)");
    b.var(
        "ANNOTATE_ONTOLOGY_VERSION",
        "annotate -V $(ONTBASE)/releases/$(VERSION)/$@ --annotation owl:versionInfo $(VERSION)",
    );
    // Staged through a temporary file, as a repository's own rules that end in
    // this variable expect: they may go on to edit the target in place.
    b.var(
        "ANNOTATE_CONVERT_FILE",
        format!(
            "annotate --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) \
             convert -f {} --output $@.tmp.owl && mv $@.tmp.owl $@",
            c.import_component_format
        ),
    );
    let mut other_src: Vec<String> = c
        .component_products()
        .iter()
        .map(|p| format!("$(COMPONENTSDIR)/{}", p.filename))
        .collect();
    if c.use_dosdps {
        b.var("PATTERNDIR", "../patterns");
        b.var("PATTERN_TESTER", "dosdp validate -i");
        b.var("DOSDPT", "dosdp-tools");
        b.var("PATTERN_RELEASE_FILES", "$(PATTERNDIR)/definitions.owl $(PATTERNDIR)/pattern.owl");
        other_src.insert(0, "$(PATTERNDIR)/definitions.owl".to_string());
    }
    b.var("OTHER_SRC", other_src.join(" "));
    b.var("ONTOLOGYTERMS", "$(TMPDIR)/ontologyterms.txt");
    b.var("EDIT_PREPROCESSED", "$(TMPDIR)/$(ONT)-preprocess.owl");
    b.var("SRCMERGED", "$(TMPDIR)/merged-$(ONT)-edit.ofn");
    // A seed the configuration turns off is not an empty file but no file: the
    // recipes that name it then name nothing.
    if c.import_group.as_ref().is_some_and(|g| g.scan_signature) {
        b.var("PRESEED", "$(TMPDIR)/pre_seed.txt");
        b.var("IMPORTSEED", "$(TMPDIR)/seed.txt");
        b.var("T_IMPORTSEED", "--term-file $(IMPORTSEED)");
    }
    b.var("SIMPLESEED", "$(TMPDIR)/simple_seed.txt");

    // The lists every group is written against, in the order the build makes
    // them: sorted, as a release's file listing is.
    let mut formats = c.export_formats.clone();
    formats.push("owl".into());
    formats.sort();
    formats.dedup();
    let mut formats_tsv = formats.clone();
    formats_tsv.push("tsv".into());
    formats_tsv.sort();
    formats_tsv.dedup();
    let mut release_artefacts: Vec<String> = c.release_artefacts.iter().map(|a| artefact_root(id, a)).collect();
    release_artefacts.sort();
    release_artefacts.dedup();
    let mut main_products = release_artefacts.clone();
    main_products.push(id.to_string());
    main_products.sort();
    main_products.dedup();
    let cross = |roots: &[String], exts: &[String]| -> String {
        roots
            .iter()
            .flat_map(|r| exts.iter().map(move |e| format!("{r}.{e}")))
            .collect::<Vec<_>>()
            .join(" ")
    };
    b.var("FORMATS", formats.join(" "));
    b.var("FORMATS_INCL_TSV", formats_tsv.join(" "));
    b.var("RELEASE_ARTEFACTS", release_artefacts.join(" "));
    b.var("MAIN_PRODUCTS", main_products.join(" "));
    if c.gzip_main {
        b.var("MAIN_GZIPPED", "$(foreach f,$(FORMATS), $(ONT).$(f).gz)");
    } else {
        b.var("MAIN_GZIPPED", "");
    }
    b.var("MAIN_FILES", format!("{} $(MAIN_GZIPPED)", cross(&main_products, &formats)));
    if c.makes("basic") {
        b.var("KEEPRELATIONS", "keeprelations.txt");
    }
    b.var("SHARED_ROBOT_COMMANDS", if c.remove_owl_nothing { "remove --term owl:Nothing" } else { "" });

    let import_ids: Vec<String> = c.imports().iter().map(|p| p.id.clone()).collect();
    let import_roots: Vec<String> =
        import_ids.iter().map(|i| format!("$(IMPORTDIR)/{i}_import")).collect();
    let group = c.import_group.as_ref();
    let import_roots = if group.is_some_and(|g| g.use_base_merging) {
        strings(&["$(IMPORTDIR)/merged_import"])
    } else {
        import_roots
    };
    b.var("IMPORTS", import_ids.join(" "));
    b.var("IMPORT_ROOTS", import_roots.join(" "));
    b.var("IMPORT_OWL_FILES", cross(&import_roots, &strings(&["owl"])));
    if group.is_some_and(|g| g.export_obo) {
        b.var("IMPORT_OBO_FILES", cross(&import_roots, &strings(&["obo"])));
        b.var("IMPORT_FILES", "$(IMPORT_OWL_FILES) $(IMPORT_OBO_FILES)");
    } else {
        b.var("IMPORT_FILES", "$(IMPORT_OWL_FILES)");
    }
    if let Some(g) = c.import_group.as_ref().filter(|g| g.strip_annotation_properties) {
        b.var("ANNOTATION_PROPERTIES", g.annotation_properties.join(" "));
    }

    let subset_ids: Vec<String> = c.subsets().iter().map(|p| p.id.clone()).collect();
    let subset_roots: Vec<String> =
        subset_ids.iter().map(|s| format!("$(SUBSETDIR)/{s}")).collect();
    b.var("SUBSETS", subset_ids.join(" "));
    b.var("SUBSET_ROOTS", subset_roots.join(" "));
    b.var("SUBSET_FILES", cross(&subset_roots, &formats_tsv));

    let mapping_group = c.sssom_mappingset_group.as_ref();
    if c.use_mappings {
        b.var("MAPPINGDIR", "../mappings");
        b.var("MAPPING_TESTER", "sssom validate");
        b.var("SSSOMPY", "sssom");
        b.var("MAPPING_RELEASE_FILES", "$(foreach n,$(MAPPINGS), $(MAPPINGDIR)/$(n).sssom.tsv)");
    }
    let set_ids = |sets: Vec<&MappingSet>| sets.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(" ");
    b.var("MAPPINGS", set_ids(mapping_group.map(|g| g.sets().iter().collect()).unwrap_or_default()));
    let released = mapping_group.map(|g| g.released()).unwrap_or_default();
    if !released.is_empty() {
        b.var("RELEASED_MAPPINGS", set_ids(released));
    }
    b.var("MAPPING_FILES", "$(foreach p, $(MAPPINGS), $(MAPPINGDIR)/$(p).sssom.tsv)");
    b.var("RELEASED_MAPPING_FILES", "$(foreach p, $(RELEASED_MAPPINGS), $(MAPPINGDIR)/$(p).sssom.tsv)");

    let report_of = |x: &String| if x == "edit" { "$(SRC)".to_string() } else { x.clone() };
    let obo_reports: Vec<String> =
        c.robot_report.report_on.iter().map(|x| format!("{}-obo-report", report_of(x))).collect();
    let align_reports: Vec<String> =
        c.robot_report.report_on.iter().map(|x| format!("{}-align-report", report_of(x))).collect();
    let aligned = c.robot_report.upper_ontology.as_deref().is_some_and(|u| !u.is_empty());
    b.var("OBO_REPORT", obo_reports.join(" "));
    b.var("ALIGNMENT_REPORT", align_reports.join(" "));
    b.var("REPORTS", if aligned { "$(OBO_REPORT) $(ALIGNMENT_REPORT)" } else { "$(OBO_REPORT)" });
    b.var("REPORT_FILES", "$(patsubst %, $(REPORTDIR)/%.tsv, $(REPORTS))");
    b.var(
        "SPARQL_VALIDATION_QUERIES",
        c.robot_report
            .custom_sparql_checks
            .iter()
            .map(|v| format!("$(SPARQLDIR)/{v}-violation.sparql"))
            .collect::<Vec<_>>()
            .join(" "),
    );
    b.var(
        "SPARQL_EXPORTS_ARGS",
        c.robot_report
            .custom_sparql_exports
            .iter()
            .map(|v| format!("-s $(SPARQLDIR)/{v}.sparql $(REPORTDIR)/{v}.tsv"))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let pattern_files = if c.use_dosdps { "$(PATTERN_RELEASE_FILES) " } else { "" };
    b.var(
        "ASSETS",
        format!("$(IMPORT_FILES) $(MAIN_FILES) {pattern_files}$(REPORT_FILES) $(SUBSET_FILES) $(MAPPING_FILES)"),
    );
    let released_imports =
        if group.is_some_and(|g| g.release_imports) { "$(IMPORT_FILES) " } else { "" };
    let released_reports = if c.robot_report.release_reports { " $(REPORT_FILES)" } else { "" };
    b.var(
        "RELEASE_ASSETS",
        format!("$(MAIN_FILES) {released_imports}$(SUBSET_FILES){released_reports}"),
    );
    b.var("CLEANFILES", "$(MAIN_FILES) $(SRCMERGED) $(EDIT_PREPROCESSED)");
    let mut released: Vec<String> = b
        .words("$(RELEASE_ASSETS)")
        .iter()
        .map(|n| format!("$(RELEASEDIR)/{n}"))
        .collect();
    if b.m.vars.contains_key("RELEASED_MAPPINGS") {
        released.push("$(foreach n,$(RELEASED_MAPPINGS), $(RELEASEDIR)/mappings/$(n).sssom.tsv)".into());
    }
    b.var("RELEASE_ASSETS_AFTER_RELEASE", released.join(" "));
    b.var("CURRENT_RELEASE", "$(ONTBASE).owl");
    b.var("TSV", "");
    let mut tables: Vec<String> = Vec::new();
    if c.use_dosdps {
        tables.extend(c.pipelines().iter().map(|p| format!("$(DOSDP_TSV_FILES_{})", p.id.to_uppercase())));
        tables.push("$(DOSDP_TSV_FILES_DEFAULT)".to_string());
    }
    b.var("ALL_TSV_FILES", tables.join(" "));
}

/// The QC a release must pass: reasoning, the SPARQL checks, the report, the
/// OWL 2 DL profile, the ID policy, and that each product is valid RDF/XML.
fn checks(b: &mut Build, c: &Config) {
    let upper = c.id.to_uppercase();
    let id = c.id.as_str();
    b.phony(
        "reason_test",
        "$(EDIT_PREPROCESSED)",
        "",
        &[&format!(
            "$(ROBOT) reason --input $< --reasoner $(REASONER) --equivalent-classes-allowed {} \
             --exclude-tautologies {} --output test.owl && rm test.owl",
            c.allow_equivalents, c.exclude_tautologies
        )],
        &[],
    );
    b.phony(
        "validate_idranges",
        "",
        "",
        &[&format!(
            "if [ -f {id}-idranges.owl ]; then \
             dicer-cli policy --assume-manchester --show-owlapi-error {id}-idranges.owl ; fi"
        )],
        &[],
    );

    let on: Vec<String> = c
        .robot_report
        .sparql_test_on
        .iter()
        .map(|x| if x == "edit" { "$(SRCMERGED)".to_string() } else { x.clone() })
        .collect();
    let verify: Vec<String> = if c.robot_report.custom_sparql_checks.is_empty() {
        Vec::new()
    } else {
        on.iter()
            .map(|x| {
                format!("$(ROBOT) verify -i {x} --queries $(SPARQL_VALIDATION_QUERIES) -O $(REPORTDIR)")
            })
            .collect()
    };
    let verify: Vec<&str> = verify.iter().map(String::as_str).collect();
    b.rule("sparql_test", &on.join(" "), "$(REPORTDIR)", &verify, &[]);

    // What the report treats as this ontology's own terms.
    let base_iris = match (&c.namespaces, c.robot_report.use_base_iris) {
        (_, false) => String::new(),
        (Some(ns), true) => ns.iter().map(|i| format!("--base-iri {i} ")).collect(),
        (None, true) => format!("--base-iri $(URIBASE)/{upper}_ --base-iri $(URIBASE)/{id} "),
    };
    let report = format!(
        "$(ROBOT) report -i $< $(REPORT_LABEL) $(REPORT_PROFILE_OPTS) --fail-on $(REPORT_FAIL_ON) \
         {base_iris}--print 5 -o $@"
    );
    b.rule("$(REPORTDIR)/$(SRC)-obo-report.tsv", "$(SRCMERGED)", "$(REPORTDIR)", &[&report], &[]);
    b.rule("$(REPORTDIR)/%-obo-report.tsv", "%", "$(REPORTDIR)", &[&report], &[]);
    if let Some(upper) = c.robot_report.upper_ontology.as_deref().filter(|u| !u.is_empty()) {
        let align = format!(
            "$(ROBOT) odk:check-align -i $< --reasoner $(REASONER) --upper-ontology-iri {upper} \
             {base_iris}--report-output $@"
        );
        b.rule("$(REPORTDIR)/$(SRC)-align-report.tsv", "$(SRCMERGED)", "$(REPORTDIR)", &[&align], &[]);
        b.rule("$(REPORTDIR)/%-align-report.tsv", "%", "$(REPORTDIR)", &[&align], &[]);
    }
    b.phony("robot_reports", "$(REPORT_FILES)", "", &[], &[]);
    b.phony("all_reports", "custom_reports robot_reports", "", &[], &[]);

    // Merged first: a profile check over a document with unresolved imports
    // reports every imported entity as undeclared.
    b.rule(
        "$(REPORTDIR)/validate_profile_owl2dl_%.txt",
        "%",
        "$(REPORTDIR) $(TMPDIR)",
        &[
            "$(ROBOT) merge -i $< convert -f ofn -o $(TMPDIR)/validate.ofn",
            "$(ROBOT) validate-profile --profile DL -i $(TMPDIR)/validate.ofn -o $@ || { cat $@ && exit 1; }",
        ],
        &[],
    );
    b.precious("$(REPORTDIR)/validate_profile_owl2dl_%.txt");
    b.rule(
        "validate_profile_%",
        "$(REPORTDIR)/validate_profile_owl2dl_%.txt",
        "",
        &["echo \"$* profile validation completed.\""],
        &[],
    );

    b.rule(
        "check_rdfxml_%",
        "%",
        "",
        &[if c.extra_rdfxml_checks { "@check-rdfxml --jena --rdflib $<" } else { "@check-rdfxml $<" }],
        &[],
    );
    let products: Vec<String> = b
        .words("$(MAIN_PRODUCTS)")
        .iter()
        .map(|p| format!("check_rdfxml_{p}.owl"))
        .collect();
    b.phony("check_rdfxml_assets", &products.join(" "), "", &[], &[]);

    let exports: &[&str] = if c.robot_report.custom_sparql_exports.is_empty() {
        &[]
    } else {
        &["$(ROBOT) query -f tsv --use-graphs true -i $< $(SPARQL_EXPORTS_ARGS)"]
    };
    b.phony("custom_reports", "$(EDIT_PREPROCESSED)", "$(REPORTDIR)", exports, &[]);
}

/// Publishing a release: copy the release files to the repository root, and the
/// comparison against the release currently published.
fn release(b: &mut Build, _c: &Config) {
    let c = _c;
    let _ = c;
    b.phony(
        "prepare_release",
        "all_odk",
        "",
        &[
            "$(MAKE) copy_release_files",
            "rm -f $(CLEANFILES)",
            "@echo \"Release files are now in $(RELEASEDIR) - now you should commit, push and make a release \
             on your git hosting site such as GitHub or GitLab\"",
        ],
        &[],
    );
    b.phony(
        "prepare_release_fast",
        "",
        "",
        &["$(MAKE) prepare_release IMP=false PAT=false MIR=false COMP=false"],
        &[],
    );
    let mut copy = vec!["rsync -R $(RELEASE_ASSETS) $(RELEASEDIR)"];
    if b.m.vars.contains_key("RELEASED_MAPPINGS") {
        copy.push("mkdir -p $(RELEASEDIR)/mappings");
        copy.push("cp -rf $(RELEASED_MAPPING_FILES) $(RELEASEDIR)/mappings");
    }
    b.phony("copy_release_files", "", "", &copy, &[]);
    b.phony("show_release_assets", "", "", &["@echo $(RELEASE_ASSETS_AFTER_RELEASE)"], &[]);
    b.phony("release_diff", "$(REPORTDIR)/release-diff.md", "", &[], &[]);
    b.rule("$(TMPDIR)/current-release.owl", "", "", &["wget $(CURRENT_RELEASE) -O $@"], &[]);
    b.rule(
        "$(REPORTDIR)/release-diff.md",
        "$(ONT).owl $(TMPDIR)/current-release.owl",
        "",
        &["$(ROBOT) diff --labels true --left $(TMPDIR)/current-release.owl --right $(ONT).owl -f markdown -o $@"],
        &[],
    );
    publishing(b, _c);
}

/// Publishing a release on the repository's host.
fn publishing(b: &mut Build, c: &Config) {
    if c.public_release != "none" {
        b.var(
            "RELEASEFILES",
            match &c.public_release_assets {
                Some(files) => files.join(" "),
                None => "$(ASSETS)".to_string(),
            },
        );
        b.var("TAGNAME", "v$(TODAY)");
    }
    if c.public_release == "github_curl" {
        // Through the host's API: find or create the release, then upload each file.
        b.var("USER", "unknown");
        b.var("GH_ASSETS", "$(patsubst %, $(TMPDIR)/gh_release_asset_%.txt, $(RELEASEFILES))");
        b.var("GITHUB_REPO", format!("{}/{}", c.github_org, c.repo));
        b.rule(
            "$(TMPDIR)/release_get.txt",
            "",
            "$(TMPDIR)",
            &["curl -s https://api.github.com/repos/${GITHUB_REPO}/releases/tags/${TAGNAME} > $@"],
            &[],
        );
        b.rule(
            "$(TMPDIR)/release_op.txt",
            "$(TMPDIR)/release_get.txt",
            "$(TMPDIR)",
            &[
                "$(eval RELEASEID=$(shell cat $(TMPDIR)/release_get.txt | jq '.id'))",
                "if ! [ \"$(RELEASEID)\" -eq \"$(RELEASEID)\" ] ; then \
                 curl -s -X POST \
                 https://api.github.com/repos/${GITHUB_REPO}/releases \
                 -H 'Accept: */*' \
                 -H 'Content-Type: application/json' \
                 -u ${USER} \
                 -d '{ \"tag_name\": \"${TAGNAME}\", \"target_commitish\": \"master\", \"name\": \"${TAGNAME}\", \"body\": \"Ontology release ${TODAY}\", \"draft\": false, \"prerelease\": false }' > $@; \
                 else \
                 cp $< $@; \
                 fi;",
            ],
            &[],
        );
        b.rule(
            "$(TMPDIR)/gh_release_id.txt",
            "$(TMPDIR)/release_op.txt",
            "$(TMPDIR)",
            &["echo $(shell cat $(TMPDIR)/release_op.txt | jq '.id') > $@;"],
            &[],
        );
        b.rule(
            "$(TMPDIR)/gh_release_asset_%.txt",
            "$(TMPDIR)/gh_release_id.txt %",
            "$(TMPDIR)",
            &["curl -X POST \
               \"https://uploads.github.com/repos/${GITHUB_REPO}/releases/$(shell cat $(TMPDIR)/gh_release_id.txt)/assets?name=$*&label=$*\" \
               --data-binary @$* \
               -u ${USER} \
               -H 'Accept: */*' \
               -H 'Cache-Control: no-cache' \
               -H 'Connection: keep-alive' \
               -H 'Content-Type: application/octet-stream' > $@"],
            &[],
        );
        b.rule("public_release", "$(TMPDIR)/gh_release_id.txt $(GH_ASSETS)", "$(TMPDIR)", &[], &[]);
    }
    if c.public_release == "github_python" {
        b.var("GITHUB_RELEASE_PYTHON", "make-release-assets.py");
        b.phony(
            "public_release",
            "",
            "",
            &["ls -alt $(ASSETS)", "$(GITHUB_RELEASE_PYTHON) --release $(TAGNAME) $(RELEASEFILES)"],
            &[],
        );
    } else {
        // With the host's own command-line client.
        b.var("GHVERSION", "v$(VERSION)");
        b.phony(
            "public_release",
            "",
            "",
            &[
                "@test $(GHVERSION)",
                "ls -alt $(RELEASE_ASSETS_AFTER_RELEASE)",
                "gh release create $(GHVERSION) --title \"$(VERSION) Release\" --draft $(RELEASE_ASSETS_AFTER_RELEASE) --generate-notes",
            ],
            &[],
        );
    }
}

/// The term lists the import modules and the simple artefact are cut against,
/// each derived from the edit file merged with its components.
fn seeds(b: &mut Build, c: &Config) {
    b.rule(
        "$(EDIT_PREPROCESSED)",
        "$(SRC)",
        "",
        &["$(ROBOT) convert --input $< --format ofn --output $@"],
        &[],
    );
    b.rule(
        "$(SRCMERGED)",
        "$(EDIT_PREPROCESSED) $(OTHER_SRC)",
        "",
        &["$(ROBOT) remove --input $< --select imports --trim false \
           merge $(foreach src, $(OTHER_SRC), --input $(src)) --output $@"],
        &[],
    );
    // Import modules are cut against everything the edit file and its components
    // refer to; a repository with no imports has no use for that list.
    if c.import_group.as_ref().is_some_and(|g| g.scan_signature) {
        b.rule(
            "$(PRESEED)",
            "$(SRCMERGED)",
            "",
            &["$(ROBOT) query --input $< --format --csv --query $(SPARQLDIR)/terms.sparql $@"],
            &[],
        );
        // The terms the patterns refer to are imported along with the edit file's.
        b.rule(
            "$(IMPORTSEED)",
            if c.use_dosdps { "$(PRESEED) $(TMPDIR)/all_pattern_terms.txt" } else { "$(PRESEED)" },
            "$(TMPDIR)",
            &["cat $^ | sort | uniq > $@"],
            &[],
        );
    }
    b.rule(
        "$(ONTOLOGYTERMS)",
        "$(SRCMERGED)",
        "",
        &["$(ROBOT) query -f csv -i $< --query ../sparql/$(ONT)_terms.sparql $@"],
        &[],
    );
    // The ontology's own terms and what they need, which only the artefacts cut
    // down to them read.
    if !["basic", "simple", "simple-non-classified"].iter().any(|v| c.makes(v)) {
        return;
    }
    b.rule(
        "$(SIMPLESEED)",
        "$(SRCMERGED) $(ONTOLOGYTERMS)",
        "",
        &["$(ROBOT) query -f csv -i $< --query ../sparql/simple-seed.sparql $@.tmp &&\
           cat $@.tmp $(ONTOLOGYTERMS) | sort | uniq >  $@ &&\
           echo \"http://www.geneontology.org/formats/oboInOwl#SubsetProperty\" >> $@ &&\
           echo \"http://www.geneontology.org/formats/oboInOwl#SynonymTypeProperty\" >> $@"],
        &[],
    );
}

/// The recipe that turns a mirror into a module of `kind`. `terms` names the
/// module's committed term list and `own` the extra properties and namespaces a
/// product of its own brings; the group's one rule has neither.
fn module_recipe(g: &ImportGroup, uribase: &str, kind: &str, terms: &str, own: Option<&ImportProduct>) -> Vec<String> {
    let normalize = format!("normalize --base-iri {uribase} --subset-decls true --synonym-decls true");
    let own_properties: String = own
        .map(|p| p.annotation_properties.iter().map(|a| format!("--term {a} ")).collect())
        .unwrap_or_default();
    let kept = format!(
        "remove $(foreach p, $(ANNOTATION_PROPERTIES), --term $(p)) {own_properties}\
         --term-file {terms} $(T_IMPORTSEED) --select complement"
    );
    // A product's own namespaces, or the one its id implies.
    let external = match own {
        Some(p) => format!(
            "remove --axioms external --preserve-structure false --trim false {}",
            p.base_iris.iter().flatten().map(|i| format!("--base-iri {i} ")).collect::<String>()
        ),
        None => "remove --base-iri $(OBOBASE)\"/$(shell echo $* | tr a-z A-Z)_\" \
                 --axioms external --preserve-structure false --trim false"
            .to_string(),
    };
    let extract = |individuals: Option<&str>, method: &str| {
        let individuals = individuals.map(|i| format!("--individuals {i} ")).unwrap_or_default();
        format!(
            "extract --term-file {terms} $(T_IMPORTSEED) --force true \
             --copy-ontology-annotations true {individuals}--method {method}"
        )
    };
    let open = "$(ROBOT) annotate --input $< --remove-annotations normalize --add-source true";
    match kind {
        "slme" => {
            let (individuals, method) = match own {
                Some(p) => (p.slme_individuals.clone(), p.module_type_slme.clone()),
                None => (Some(g.slme_individuals.clone()), Some(g.module_type_slme.clone())),
            };
            let strip = if g.strip_annotation_properties {
                format!("{kept} --select annotation-properties ")
            } else {
                String::new()
            };
            vec![format!(
                "{open} {} {strip}{normalize} repair --merge-axiom-annotations true $(ANNOTATE_CONVERT_FILE)",
                extract(individuals.as_deref(), method.as_deref().unwrap_or("BOT"))
            )]
        }
        "minimal" => {
            let select = if own.is_some() {
                "classes individual annotation-properties"
            } else {
                "classes individuals annotation-properties"
            };
            vec![format!(
                "{open} {} {external} {normalize} repair --merge-axiom-annotations true \
                 {kept} --select \"{select}\" $(ANNOTATE_CONVERT_FILE)",
                extract(None, "BOT")
            )]
        }
        "mirror" => vec![format!(
            "$(ROBOT) annotate --input $< --remove-annotations {normalize} --add-source true \
             repair --merge-axiom-annotations true $(ANNOTATE_CONVERT_FILE)"
        )],
        "filter" => match own {
            Some(_) => vec![format!(
                "{open} {} {external} {kept} {normalize} \
                 repair --merge-axiom-annotations true $(ANNOTATE_CONVERT_FILE)",
                extract(None, "BOT")
            )],
            None => vec![format!(
                "$(ROBOT) merge --input $< annotate --remove-annotations normalize --add-source true \
                 {external} {kept} {normalize} \
                 repair --merge-axiom-annotations true $(ANNOTATE_CONVERT_FILE)"
            )],
        },
        _ => unreachable!("module kind `{kind}` passed Config::check"),
    }
}

/// A rule that can only say it has to be written: the configuration declares
/// this the repository's own to build.
fn must_be_overridden(what: &str, id: &str) -> Vec<String> {
    vec![
        format!("@echo \"ERROR: You have configured {what};\""),
        format!("@echo \"       This rule needs to be overwritten in {id}.Makefile!\""),
        "@false".to_string(),
    ]
}

/// The import modules: one per import, each cut from its mirror according to its
/// kind — or, with base merging, a single module cut from every mirror merged.
fn imports(b: &mut Build, c: &Config) {
    // With no import group there are no modules, but the switch and the refresh
    // commands are part of every build.
    let Some(g) = c.import_group.as_ref() else {
        b.switch("IMP");
        refresh_targets(b);
        return;
    };
    let uribase = c.uribase.as_str();
    let id = c.id.as_str();
    fn lines(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }
    if b.switch("IMP") {
        if g.use_base_merging {
            b.var("ALL_TERMS", "$(foreach imp, $(IMPORTS), $(IMPORTDIR)/$(imp)_terms.txt)");
            let recipe = if g.module_type == "slme" {
                let excluded: String = g
                    .exclude_iri_patterns
                    .iter()
                    .flatten()
                    .map(|p| format!("remove --select \"{p}\" "))
                    .collect();
                let strip = if g.strip_annotation_properties {
                    "remove $(foreach p, $(ANNOTATION_PROPERTIES), --term $(p)) \
                     $(foreach f, $(ALL_TERMS), --term-file $(f)) $(T_IMPORTSEED) \
                     --select complement --select annotation-properties "
                } else {
                    ""
                };
                vec![format!(
                    "$(ROBOT) merge --input $< {excluded}\
                     extract $(foreach f, $(ALL_TERMS), --term-file $(f)) $(T_IMPORTSEED) \
                     --force true --copy-ontology-annotations false \
                     --individuals {} --method {} {strip}\
                     normalize --base-iri {uribase} --subset-decls true --synonym-decls true \
                     repair --merge-axiom-annotations true $(ANNOTATE_CONVERT_FILE)",
                    g.slme_individuals, g.module_type_slme
                )]
            } else {
                must_be_overridden("the merged import as a custom module", id)
            };
            b.rule(
                "$(IMPORTDIR)/merged_import.owl",
                "$(MIRRORDIR)/merged.owl $(ALL_TERMS) $(IMPORTSEED)",
                "",
                &lines(&recipe),
                &["IMP"],
            );
        } else {
            // The group's one rule, for every product of the group's kind…
            let (needs, recipe) = match g.module_type.as_str() {
                "mirror" => (
                    "$(MIRRORDIR)/%.owl",
                    module_recipe(g, uribase, "mirror", "", None),
                ),
                "custom" => (
                    "$(MIRRORDIR)/%.owl",
                    must_be_overridden("the default module type to be custom", id),
                ),
                kind => (
                    "$(MIRRORDIR)/%.owl $(IMPORTDIR)/%_terms.txt $(IMPORTSEED)",
                    module_recipe(g, uribase, kind, "$(IMPORTDIR)/$*_terms.txt", None),
                ),
            };
            b.rule("$(IMPORTDIR)/%_import.owl", needs, "", &lines(&recipe), &["IMP"]);
            b.precious("$(IMPORTDIR)/%_import.owl");

            // …and a rule of its own for each product that is not.
            for p in g.products.iter().filter(|p| g.is_special(p)) {
                let large = p.is_large;
                if large && !b.switch("IMP_LARGE") {
                    continue;
                }
                let guards: &[&str] = if large { &["IMP", "IMP_LARGE"] } else { &["IMP"] };
                let target = format!("$(IMPORTDIR)/{}_import.owl", p.id);
                let mirror = format!("$(MIRRORDIR)/{}.owl", p.id);
                let terms = format!("$(IMPORTDIR)/{}_terms.txt", p.id);
                let (needs, recipe) = match p.kind() {
                    "mirror" => (mirror, module_recipe(g, uribase, "mirror", "", Some(p))),
                    "custom" => (
                        if p.mirror_type.as_deref() == Some("no_mirror") { String::new() } else { mirror },
                        must_be_overridden(&format!("{} as a custom module", p.id), id),
                    ),
                    kind => (
                        format!("{mirror} {terms} $(IMPORTSEED)"),
                        module_recipe(g, uribase, kind, &terms, Some(p)),
                    ),
                };
                b.rule(&target, &needs, "", &lines(&recipe), guards);
            }
        }
        if g.export_obo {
            b.rule(
                "$(IMPORTDIR)/%_import.obo",
                "$(IMPORTDIR)/%_import.owl",
                "",
                &["$(ROBOT) convert --input $< --check false --format obo --output $@"],
                &["IMP"],
            );
        }
    }

    refresh_targets(b);
}

/// Rebuilding the modules on request, with or without fetching their mirrors.
fn refresh_targets(b: &mut Build) {
    b.phony(
        "refresh-imports",
        "",
        "",
        &["$(MAKE) IMP=true MIR=true PAT=false IMP_LARGE=true clean all_imports"],
        &[],
    );
    b.phony(
        "no-mirror-refresh-imports",
        "",
        "",
        &["$(MAKE) --assume-new=$(SRC) \
           $(foreach imp,$(IMPORTS),--assume-new=$(IMPORTDIR)/$(imp)_terms.txt) \
           IMP=true MIR=false PAT=false IMP_LARGE=true all_imports"],
        &[],
    );
    b.phony(
        "refresh-imports-excluding-large",
        "",
        "",
        &["$(MAKE) IMP=true MIR=true PAT=false IMP_LARGE=false clean all_imports"],
        &[],
    );
    b.phony(
        "refresh-%",
        "",
        "",
        &["$(MAKE) --assume-new=$(SRC) --assume-new=$(IMPORTDIR)/$*_terms.txt \
           IMP=true IMP_LARGE=true MIR=true PAT=false $(IMPORTDIR)/$*_import.owl"],
        &[],
    );
    b.phony(
        "no-mirror-refresh-%",
        "",
        "",
        &["$(MAKE) --assume-new=$(SRC) --assume-new=$(IMPORTDIR)/$*_terms.txt \
           IMP=true IMP_LARGE=true MIR=false PAT=false $(IMPORTDIR)/$*_import.owl"],
        &[],
    );
}

/// Files merged into the release whole. One built from templates has its own
/// rule; any other is kept as committed, and created empty if it is missing.
fn components(b: &mut Build, c: &Config) {
    if c.components.is_none() || !b.switch("COMP") {
        return;
    }
    let g = &["COMP"];
    let stamps: Vec<String> = c
        .component_products()
        .iter()
        .map(|p| format!("--assume-new=$(TMPDIR)/stamp-component-{}", p.filename))
        .collect();
    let stamps = stamps.join(" ");
    b.phony("all_components", "$(OTHER_SRC)", "", &[], g);
    b.phony(
        "recreate-components",
        "",
        "",
        &[&format!("$(MAKE) {stamps} COMP=true IMP=false MIR=true PAT=true all_components")],
        g,
    );
    b.phony(
        "no-mirror-recreate-components",
        "",
        "",
        &[&format!("$(MAKE) {stamps} COMP=true IMP=false MIR=false PAT=true all_components")],
        g,
    );
    b.phony(
        "recreate-%",
        "",
        "",
        &["$(MAKE) --assume-new=$(TMPDIR)/stamp-component-$*.owl \
           COMP=true IMP=false MIR=true PAT=true $(COMPONENTSDIR)/$*.owl"],
        g,
    );
    b.phony(
        "no-mirror-recreate-%",
        "",
        "",
        &["$(MAKE) --assume-new=$(TMPDIR)/stamp-component-$*.owl \
           COMP=true IMP=false MIR=false PAT=true $(COMPONENTSDIR)/$*.owl"],
        g,
    );
    b.rule(
        "$(COMPONENTSDIR)/%.owl",
        "$(TMPDIR)/stamp-component-%.owl",
        "$(COMPONENTSDIR)",
        &["test -f $@ || touch $@"],
        g,
    );
    b.precious("$(COMPONENTSDIR)/%.owl");
    b.rule("$(TMPDIR)/stamp-component-%.owl", "", "$(TMPDIR)", &["touch $@"], g);
    b.precious("$(TMPDIR)/stamp-component-%.owl");

    for p in c.component_products() {
        let file = p.filename.as_str();
        let target = format!("$(COMPONENTSDIR)/{file}");
        let stamp = format!("$(TMPDIR)/stamp-component-{file}");
        if let Some(source) = &p.source {
            // Fetched, and replaced only when what was fetched has changed.
            if !b.switch("MIR") {
                continue;
            }
            let guards = &["COMP", "MIR"];
            let download = format!("component-download-{file}");
            let own_axioms = if p.make_base {
                format!(
                    "remove {}--axioms external --preserve-structure false --trim false ",
                    p.base_iris.iter().flatten().map(|i| format!("--base-iri {i} ")).collect::<String>()
                )
            } else {
                String::new()
            };
            b.phony(
                &download,
                "",
                "$(TMPDIR)",
                &[&format!(
                    "$(ROBOT) merge -I {source} {own_axioms}\
                     annotate --annotation owl:versionInfo $(VERSION) --output $(TMPDIR)/$@.owl"
                )],
                guards,
            );
            b.rule(
                &target,
                &format!("{download} {stamp}"),
                "",
                &[&format!(
                    "@if cmp -s $(TMPDIR)/{download}.owl $(TMPDIR)/{download}.tmp.owl ; then \
                     echo \"Component identical.\" ; \
                     else \
                     echo \"Component different, updating.\" && \
                     cp $(TMPDIR)/{download}.owl $(TMPDIR)/{download}.tmp.owl && \
                     $(ROBOT) annotate --input $(TMPDIR)/{download}.owl \
                     --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) \
                     --output $@ ; \
                     fi"
                )],
                guards,
            );
            b.precious(&target);
        } else if p.use_template {
            let templates: Vec<String> =
                p.templates.iter().flatten().map(|t| format!("$(TEMPLATEDIR)/{t}")).collect();
            let args: Vec<String> = templates.iter().map(|t| format!("--template {t}")).collect();
            b.rule(
                &target,
                &format!("{} {stamp}", templates.join(" ")),
                "",
                &[&format!(
                    "$(ROBOT) template {} {} $(ANNOTATE_CONVERT_FILE)",
                    p.template_options.as_deref().unwrap_or(""),
                    args.join(" ")
                )],
                g,
            );
            b.precious(&target);
        } else if p.use_mappings {
            let sets: Vec<String> =
                p.mappings.iter().flatten().map(|m| format!("$(MAPPINGDIR)/{m}")).collect();
            let args: Vec<String> = sets.iter().map(|m| format!("--sssom {m}")).collect();
            b.rule(
                &target,
                &format!("{} {stamp}", sets.join(" ")),
                "",
                &[&format!(
                    "$(ROBOT) --add-prefix 'sssom: https://w3id.org/sssom/' \
                     --add-prefix 'semapv: http://w3id.org/semapv/vocab/' \
                     sssom:inject {} --create --direct $(ANNOTATE_CONVERT_FILE)",
                    args.join(" ")
                )],
                g,
            );
            b.precious(&target);
        }
    }
}

/// Each import's upstream ontology, fetched and kept unless it has changed.
fn mirrors(b: &mut Build, c: &Config) {
    if !b.switch("MIR") {
        return;
    }
    let no_group = ImportGroup::empty();
    let g = c.import_group.as_ref().unwrap_or(&no_group);
    let curl = format!(
        "--retry {} --max-time {}",
        g.mirror_retry_download, g.mirror_max_time_download
    );
    for p in &g.products {
        let id = p.id.as_str();
        let large = p.is_large;
        if large && !b.switch("IMP_LARGE") {
            continue;
        }
        let guards: &[&str] = if large { &["MIR", "IMP_LARGE"] } else { &["MIR"] };
        // The mirror is the ontology as published, or only what is its own.
        let namespaces: String =
            p.base_iris.iter().flatten().map(|i| format!("--base-iri {i} ")).collect();
        let keep = |input: &str| {
            if p.mirrors_base() {
                format!(
                    "$(ROBOT) remove {input} {namespaces}--axioms external --preserve-structure false --trim false -o $(TMPDIR)/$@.owl"
                )
            } else {
                format!("$(ROBOT) convert {input} -o $(TMPDIR)/$@.owl")
            }
        };
        let fetch = |url: &str, to: &str| format!("curl -L {url} --create-dirs -o {to} {curl}");
        let mirror = format!("mirror-{id}");
        let recipe = if let Some(from) = &p.mirror_from {
            Some(keep(&format!("-I {from}")))
        } else if p.use_base && p.use_gzipped {
            let gz = format!("$(MIRRORDIR)/{id}-base.owl.gz");
            Some(format!(
                "{} && {}",
                fetch(&format!("$(OBOBASE)/{id}/{id}-base.owl.gz"), &gz),
                keep(&format!("-i {gz}"))
            ))
        } else if p.use_base {
            let download = format!("$(TMPDIR)/{id}-download.owl");
            Some(format!(
                "{} && $(ROBOT) convert -i {download} -o $(TMPDIR)/$@.owl",
                fetch(&format!("$(OBOBASE)/{id}/{id}-base.owl"), &download)
            ))
        } else if p.use_gzipped {
            let gz = format!("$(MIRRORDIR)/{id}.owl.gz");
            Some(format!(
                "{} && {}",
                fetch(&format!("$(OBOBASE)/{id}.owl.gz"), &gz),
                keep(&format!("-i {gz}"))
            ))
        } else {
            match p.mirror_type.as_deref() {
                Some("custom") => {
                    b.rule(
                        &format!("$(MIRRORDIR)/{id}.owl"),
                        "",
                        "",
                        &[&format!(
                            "echo \"ERROR: You have configured your default mirror type to be custom; \
                             this behavior needs to be overwritten in {}.Makefile!\" && false",
                            c.id
                        )],
                        guards,
                    );
                    None
                }
                Some("no_mirror") => None,
                _ => {
                    let download = format!("$(TMPDIR)/{id}-download.owl");
                    Some(format!(
                        "{} && {}",
                        fetch(&format!("$(OBOBASE)/{id}.owl"), &download),
                        keep(&format!("-i {download}"))
                    ))
                }
            }
        };
        if p.mirror_type.as_deref() != Some("no_mirror") {
            b.m.phony.insert(mirror.clone());
            b.precious(&format!("$(MIRRORDIR)/{id}.owl"));
        }
        if let Some(recipe) = recipe {
            b.rule(&mirror, "", "$(TMPDIR)", &[&recipe], guards);
        }
    }
    if g.use_base_merging {
        b.var("ALL_MIRRORS", "$(patsubst %, $(MIRRORDIR)/%.owl, $(IMPORTS))");
        b.var("MERGE_MIRRORS", "true");
        let defined_by = if g.annotate_defined_by { "--annotate-defined-by true " } else { "" };
        let drop_equivalents = if g.base_merge_drop_equivalent_class_axioms {
            "remove --axioms equivalent --preserve-structure false "
        } else {
            ""
        };
        // Switched off, the merged mirror is whatever its own `mirror-merged`
        // rule makes it: a repository that assembles it some other way.
        if b.switch("MERGE_MIRRORS") {
            b.rule(
                "$(MIRRORDIR)/merged.owl",
                "$(ALL_MIRRORS)",
                "",
                &[&format!("$(ROBOT) merge $(patsubst %, -i %, $^) {defined_by}{drop_equivalents}-o $@")],
                &["MIR", "MERGE_MIRRORS"],
            );
            b.precious("$(MIRRORDIR)/merged.owl");
        }
    }
    b.rule(
        "$(MIRRORDIR)/%.owl",
        "mirror-%",
        "$(MIRRORDIR)",
        &["if [ -f $(TMPDIR)/mirror-$*.owl ]; then if cmp -s $(TMPDIR)/mirror-$*.owl $@ ; then \
           echo \"Mirror identical, ignoring.\"; else echo \"Mirrors different, updating.\" && \
           cp $(TMPDIR)/mirror-$*.owl $@; fi; fi"],
        &["MIR"],
    );
}

/// Classes generated from design patterns: each pipeline's data tables become
/// one module per pattern, and the modules together become `definitions.owl`, a
/// source of the release.
fn patterns(b: &mut Build, c: &Config) {
    if !c.use_dosdps {
        return;
    }
    b.var("ALL_PATTERN_FILES", "$(wildcard $(PATTERNDIR)/dosdp-patterns/*.yaml)");
    b.var(
        "ALL_PATTERN_NAMES",
        "$(strip $(patsubst %.yaml,%, $(notdir $(wildcard $(PATTERNDIR)/dosdp-patterns/*.yaml))))",
    );
    // `default` first, then the configured pipelines: (directory, variable suffix, options).
    let mut pipelines: Vec<(String, String, String)> =
        vec![("default".into(), "DEFAULT".into(), c.dosdp_tools_options.clone())];
    pipelines.extend(
        c.pipelines().iter().map(|p| (p.id.clone(), p.id.to_uppercase(), p.dosdp_tools_options.clone())),
    );
    let each = |what: &str| -> String {
        pipelines.iter().map(|(_, v, _)| format!("$(DOSDP_{what}_FILES_{v}) ")).collect()
    };
    b.var(
        "PATTERN_CLEAN_FILES",
        format!(
            "../patterns/all_pattern_terms.txt {}",
            pipelines
                .iter()
                .map(|(_, v, _)| format!("$(DOSDP_OWL_FILES_{v}) $(DOSDP_TERM_FILES_{v}) "))
                .collect::<String>()
        ),
    );
    b.phony("pattern_clean", "", "", &["rm -f $(PATTERN_CLEAN_FILES)"], &[]);

    // The per-pipeline file lists are read off the data directory.
    for (dir, v, _) in &pipelines {
        b.var(&format!("DOSDP_TSV_FILES_{v}"), format!("$(wildcard $(PATTERNDIR)/data/{dir}/*.tsv)"));
        b.var(
            &format!("DOSDP_PATTERN_NAMES_{v}"),
            format!("$(strip $(patsubst %.tsv, %, $(notdir $(DOSDP_TSV_FILES_{v}))))"),
        );
        for (what, ext, under) in [
            ("OWL", "ofn", format!("data/{dir}")),
            ("TERM", "txt", format!("data/{dir}")),
            ("YAML", "yaml", "dosdp-patterns".to_string()),
        ] {
            b.var(
                &format!("DOSDP_{what}_FILES_{v}"),
                format!("$(foreach name, $(DOSDP_PATTERN_NAMES_{v}), $(PATTERNDIR)/{under}/$(name).{ext})"),
            );
        }
    }

    if !b.switch("PAT") {
        // Generation is off, but the committed definitions still name terms the
        // imports have to bring in.
        b.rule(
            "$(TMPDIR)/all_pattern_terms.txt",
            "$(PATTERNDIR)/definitions.owl",
            "",
            &["$(ROBOT) query --use-graphs true -f csv -i $< --query $(SPARQLDIR)/terms.sparql $@"],
            &["PAT"],
        );
        b.rule("dosdp_validation", "", "", &[], &["PAT"]);
        return;
    }
    let g = &["PAT"];
    b.m.phony.insert("patterns".to_string());
    b.rule(
        "patterns dosdp",
        "",
        "",
        &[
            "echo \"Validating all DOSDP templates\"",
            "$(MAKE) dosdp_validation",
            "echo \"Building $(PATTERNDIR)/definitions.owl\"",
            "$(MAKE) $(PATTERNDIR)/pattern.owl $(PATTERNDIR)/definitions.owl",
        ],
        g,
    );
    b.rule(
        "$(TMPDIR)/pattern_schema_checks",
        "$(ALL_PATTERN_FILES)",
        "$(TMPDIR)",
        &["$(PATTERN_TESTER) $(PATTERNDIR)/dosdp-patterns/ && touch $@"],
        g,
    );
    b.m.phony.insert("pattern_schema_checks".to_string());
    b.rule("pattern_schema_checks dosdp_validation", "$(TMPDIR)/pattern_schema_checks", "", &[], g);
    b.phony(
        "update_patterns",
        "download_patterns",
        "",
        &["if [ -n \"$$(find $(TMPDIR) -type f -path '$(TMPDIR)/dosdp/*.yaml')\" ]; then \
           cp -r $(TMPDIR)/dosdp/*.yaml $(PATTERNDIR)/dosdp-patterns; fi"],
        g,
    );
    // The patterns a repository takes from elsewhere, listed in `external.txt`.
    b.phony(
        "download_patterns",
        "",
        "",
        &[
            "rm -f $(TMPDIR)/dosdp/*.yaml.1 || true",
            "if [ -s $(PATTERNDIR)/dosdp-patterns/external.txt ]; then \
             wget -i $(PATTERNDIR)/dosdp-patterns/external.txt --backups=1 -P $(TMPDIR)/dosdp; fi",
            "rm -f $(TMPDIR)/dosdp/*.yaml.1 || true",
        ],
        g,
    );
    b.rule(
        "$(PATTERNDIR)/dospd-patterns/%.yml",
        "download_patterns",
        "",
        &["if cmp -s $(TMPDIR)/dosdp-$*.yml $@ ; then echo \"DOSDP templates identical.\"; \
           else echo \"DOSDP templates different, updating.\" && cp $(TMPDIR)/dosdp-$*.yml $@; fi"],
        g,
    );

    for (dir, v, options) in &pipelines {
        // One `generate` writes every module of the pipeline.
        let slash = if dir == "default" { "/" } else { "" };
        let template_slash = if dir == "default" { "" } else { "/" };
        b.rule(
            &format!("$(DOSDP_OWL_FILES_{v})"),
            &format!("$(EDIT_PREPROCESSED) $(DOSDP_TSV_FILES_{v}) $(ALL_PATTERN_FILES)"),
            "",
            &[&format!(
                "if [ \"${{DOSDP_PATTERN_NAMES_{v}}}\" ]; then $(DOSDPT) generate --catalog=$(CATALOG) \
                 --infile=$(PATTERNDIR)/data/{dir}{slash} --template=$(PATTERNDIR)/dosdp-patterns{template_slash} \
                 --batch-patterns=\"$(DOSDP_PATTERN_NAMES_{v})\" \
                 --ontology=$< {options} --outfile=$(PATTERNDIR)/data/{dir}; fi"
            )],
            g,
        );
        b.phony(
            &format!("dosdp-docs-{dir}"),
            &format!("$(EDIT_PREPROCESSED) $(DOSDP_TSV_FILES_{v}) $(DOSDP_YAML_FILES_{v})"),
            "",
            &[
                &format!("mkdir -p $(DOCSDIR)/patterns/{dir}"),
                &format!(
                    "$(DOSDPT) docs {options} --catalog=$(CATALOG) --ontology=$< \
                     --infile=$(PATTERNDIR)/data/{dir} --template=$(PATTERNDIR)/dosdp-patterns \
                     --batch-patterns=\"$(DOSDP_PATTERN_NAMES_{v})\" \
                     --outfile=$(DOCSDIR)/patterns/{dir}{}",
                    c.data_location(dir)
                ),
            ],
            g,
        );
    }
    // The terms each data table refers to.
    for (dir, _, _) in &pipelines {
        b.rule(
            &format!("$(PATTERNDIR)/data/{dir}/%.txt"),
            &format!("$(PATTERNDIR)/dosdp-patterns/%.yaml $(PATTERNDIR)/data/{dir}/%.tsv"),
            "",
            &["$(DOSDPT) terms --infile=$(word 2, $^) --template=$< --obo-prefixes=true --outfile=$@"],
            g,
        );
    }
    for m in c.pattern_pipelines_group.iter().flat_map(|g| g.matches.iter().flatten()) {
        b.rule(
            &format!("dosdp-matches-{}", m.id),
            &format!("{} $(ALL_PATTERN_FILES)", m.ontology),
            "",
            &[&format!(
                "$(DOSDPT) query --ontology=$< --catalog=$(CATALOG) --reasoner=elk {} \
                 --batch-patterns=\"$(ALL_PATTERN_NAMES)\" --template=\"$(PATTERNDIR)/dosdp-patterns\" \
                 --outfile=\"$(PATTERNDIR)/data/{}/\"",
                m.dosdp_tools_options, m.id
            )],
            g,
        );
    }
    b.rule(
        "$(TMPDIR)/all_pattern_terms.txt",
        &format!("{}$(TMPDIR)/pattern_owl_seed.txt", each("TERM")),
        "",
        &["cat $^ | sort | uniq > $@"],
        g,
    );
    b.rule(
        "$(TMPDIR)/pattern_owl_seed.txt",
        "$(PATTERNDIR)/pattern.owl",
        "",
        &["$(ROBOT) query --use-graphs true -f csv -i $< --query ../sparql/terms.sparql $@"],
        g,
    );
    // Every pattern as an ontology of its own…
    b.rule(
        "$(PATTERNDIR)/pattern.owl",
        "$(ALL_PATTERN_FILES)",
        "",
        &["$(DOSDPT) prototype --obo-prefixes true --template=$(PATTERNDIR)/dosdp-patterns --outfile=$@"],
        g,
    );
    // …and every generated module merged into the one file the release reads.
    let any_names: String = pipelines
        .iter()
        .map(|(_, v, _)| format!("[ \"${{DOSDP_PATTERN_NAMES_{v}}}\" ]"))
        .collect::<Vec<_>>()
        .join(" || ");
    b.rule(
        "$(PATTERNDIR)/definitions.owl",
        each("OWL").trim_end(),
        "",
        &[&format!(
            "if {any_names} && [ $(PAT) = true ]; then $(ROBOT) merge $(addprefix -i , $^) \
             annotate --ontology-iri $(ONTBASE)/patterns/definitions.owl \
             --version-iri $(ONTBASE)/releases/$(TODAY)/patterns/definitions.owl \
             --annotation owl:versionInfo $(VERSION) -o definitions.ofn && mv definitions.ofn $@; fi"
        )],
        g,
    );
}

/// The mapping sets: checked, normalised, and each kept up to date in its own way.
fn mappings(b: &mut Build, c: &Config) {
    if !c.use_mappings {
        return;
    }
    b.rule(
        "validate-sssom-%",
        "",
        "",
        &[
            "tsvalid $(MAPPINGDIR)/$*.sssom.tsv --comment \"#\"",
            "sssom validate $(MAPPINGDIR)/$*.sssom.tsv",
        ],
        &[],
    );
    b.rule(
        "validate_mappings",
        "",
        "",
        &["$(MAKE_FAST) $(foreach n,$(MAPPINGS),validate-sssom-$(n))"],
        &[],
    );
    b.rule(
        "normalize-sssom-%",
        "",
        "",
        &["sssom-cli --output $(MAPPINGDIR)/$*.sssom.tsv $(MAPPINGDIR)/$*.sssom.tsv"],
        &[],
    );
    b.rule(
        "normalize_mappings",
        "",
        "",
        &["$(MAKE_FAST) $(foreach n,$(MAPPINGS),normalize-sssom-$(n))"],
        &[],
    );
    let Some(g) = c.sssom_mappingset_group.as_ref() else { return };
    for p in g.sets() {
        let id = p.id.as_str();
        let set = format!("$(MAPPINGDIR)/{id}.sssom.tsv");
        match p.maintenance.as_str() {
            // Read off the ontology's own cross-references.
            "extract" if g.mapping_extractor == "sssom-py" => {
                let graph = format!("$(TMPDIR)/{id}.obographs.json");
                b.rule(
                    &graph,
                    p.source_file.as_deref().unwrap_or_default(),
                    "",
                    &["$(ROBOT) annotate --input $< --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) \
                       convert --check false --format json --output $@"],
                    &[],
                );
                b.rule(
                    &set,
                    &graph,
                    "",
                    &[&format!(
                        "sssom parse $< -I obographs-json {} -o $@",
                        p.sssom_tool_options.as_deref().unwrap_or_default()
                    )],
                    &[],
                );
            }
            "extract" if g.mapping_extractor == "sssom-java" => b.rule(
                &set,
                p.source_file.as_deref().unwrap_or_default(),
                "",
                &["$(ROBOT) sssom:xref-extract --input $< --replace --ignore-treat-xrefs --all-xrefs --mapping-file $@"],
                &[],
            ),
            // Curated by hand: all the build can do is insist that it is there.
            "manual" => b.rule(&set, "", "", &["test -f $@"], &[]),
            "merged" => {
                let sources: Vec<String> = p
                    .source_mappings
                    .iter()
                    .flatten()
                    .map(|s| format!("$(MAPPINGDIR)/{s}.sssom.tsv"))
                    .collect();
                b.rule(&set, &sources.join(" "), "", &["sssom-cli --output $@ $^"], &[]);
            }
            "mirror" => {
                if b.switch("MIR") {
                    b.rule(
                        &set,
                        "",
                        "",
                        &[&format!("wget \"{}\" -O $@", p.mirror_from.as_deref().unwrap_or("None"))],
                        &["MIR"],
                    );
                }
            }
            "custom" => {
                let recipe = must_be_overridden(&format!("{id} as a custom mapping set"), &c.id);
                let lines: Vec<&str> = recipe.iter().map(String::as_str).collect();
                b.rule(&set, "", "", &lines, &[]);
            }
            _ => {}
        }
    }
}

/// A subset is the slice of the release tagged with it, in every export format
/// and as a table of its classes.
fn subsets(b: &mut Build) {
    b.rule(
        "$(SUBSETDIR)/%.tsv",
        "$(SUBSETDIR)/%.owl",
        "",
        &["$(ROBOT) export -i $< --include classes --header \"ID [IRI]|LABEL\" --format tsv --export $@"],
        &[],
    );
    b.rule(
        "$(SUBSETDIR)/%.owl",
        "$(ONT).owl",
        "$(SUBSETDIR)",
        &["$(ROBOT) subset -i $< --subset $* --fill-gaps true \
           annotate --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) -o $@"],
        &[],
    );
    b.rule(
        "$(SUBSETDIR)/%.obo",
        "$(SUBSETDIR)/%.owl",
        "",
        &["$(ROBOT) convert --input $< --check false -f obo $(OBO_FORMAT_OPTIONS) -o $@"],
        &[],
    );
    b.rule(
        "$(SUBSETDIR)/%.json",
        "$(SUBSETDIR)/%.owl",
        "",
        &["$(ROBOT) convert --input $< --check false -f json -o $@"],
        &[],
    );
}

/// The release artefacts and their export formats.
fn artefacts(b: &mut Build, c: &Config) {
    let id = c.id.as_str();
    let uribase = c.uribase.as_str();
    let has = |f: &str| c.export_formats.iter().any(|x| x == f);
    let obo = "$(ROBOT) convert --input $< --check false -f obo $(OBO_FORMAT_OPTIONS) -o $@";
    let export = |iri_base: &str, format: &str| {
        format!(
            "$(ROBOT) annotate --input $< --ontology-iri {iri_base}/$@ $(ANNOTATE_ONTOLOGY_VERSION) \
             convert --check false -f {format} -o $@"
        )
    };

    // Each artefact in each export format.
    for a in &c.release_artefacts {
        let root = match a.strip_prefix("custom-") {
            Some(name) => name.to_string(),
            None => format!("$(ONT)-{a}"),
        };
        if has("obo") {
            b.rule(&format!("{root}.obo"), &format!("{root}.owl"), "", &[obo], &[]);
        }
        for format in ["ttl", "json"] {
            if has(format) {
                b.rule(
                    &format!("{root}.{format}"),
                    &format!("{root}.owl"),
                    "",
                    &[&export("$(ONTBASE)", format)],
                    &[],
                );
            }
        }
    }
    if has("db") {
        let context = if c.use_context { " $(CONTEXT_FILE_CSV)" } else { "" };
        let prefixes = if c.use_context { " -P $(CONTEXT_FILE_CSV)" } else { "" };
        if c.use_context {
            b.rule("$(CONTEXT_FILE_CSV)", "$(CONTEXT_FILE)", "$(TMPDIR)", &["context2csv < $< > $@"], &[]);
        }
        b.rule(
            "%.db",
            &format!("%.owl{context}"),
            "",
            &[
                "@rm -f $*.db $*-relation-graph.tsv.gz .template.db .template.db.tmp",
                &format!("semsql make $*.db{prefixes}"),
                "@rm -f $*-relation-graph.tsv.gz .template.db .template.db.tmp",
                "@test -f $*.db || (echo \"SQLite/SemSQL generation failed\" && exit 1)",
            ],
            &[],
        );
    }

    // The primary product is one of the artefacts under the ontology's own IRI.
    let primary = format!("$(ONT)-{}.owl", c.primary_release);
    b.rule(
        "$(ONT).owl",
        &primary,
        "",
        &["$(ROBOT) annotate --input $< --ontology-iri $(URIBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) convert -o $@"],
        &[],
    );
    if has("obo") {
        b.rule("$(ONT).obo", "$(ONT).owl", "", &[obo], &[]);
    }
    for format in ["ttl", "json"] {
        if has(format) {
            b.rule(&format!("$(ONT).{format}"), "$(ONT).owl", "", &[&export("$(URIBASE)", format)], &[]);
        }
    }
    if c.gzip_main {
        let mut zipped: Vec<&str> = c.export_formats.iter().map(String::as_str).collect();
        if !has("owl") {
            zipped.push("owl");
        }
        for format in zipped {
            b.rule(
                &format!("$(ONT).{format}.gz"),
                &format!("$(ONT).{format}"),
                "",
                &["gzip -c $< > $@.tmp && mv $@.tmp $@"],
                &[],
            );
        }
    }
    // A build that does not export OWL still makes the primary product: a copy.
    if !has("owl") {
        b.rule("$(ONT).owl", &primary, "", &["cp $< $@"], &[]);
    }

    // How an artefact starts: the edit file with what it imports, either as it
    // declares its imports or as the configuration lists them…
    // The components are merged whether or not there are any: with none the list
    // is empty and the merge adds nothing.
    let merge_components = "merge $(patsubst %, -i %, $(OTHER_SRC)) ";
    let open = if c.use_edit_file_imports {
        let defined_by = c
            .import_group
            .as_ref()
            .is_some_and(|g| !g.use_base_merging && g.annotate_defined_by);
        format!(
            "$(ROBOT) merge --input $< {}",
            if defined_by { "--annotate-defined-by true" } else { "" }
        )
    } else {
        format!(
            "$(ROBOT) remove --input $< --select imports --trim false {merge_components}\
             merge $(patsubst %, -i %, $(IMPORT_FILES))"
        )
    };
    // …or without its imports at all.
    let open_base =
        format!("$(ROBOT) remove --input $< --select imports --trim false {merge_components}");
    b.var("ROBOT_RELEASE_IMPORT_MODE", open.as_str());
    b.var("ROBOT_RELEASE_IMPORT_MODE_BASE", open_base.as_str());

    let reason = |annotated: bool| {
        let annotate = if annotated {
            format!(" --annotate-inferred-axioms {}", c.release_annotate_inferred_axioms)
        } else {
            String::new()
        };
        format!(
            "reason --reasoner $(REASONER) --equivalent-classes-allowed {} --exclude-tautologies {}{annotate}",
            c.allow_equivalents, c.exclude_tautologies
        )
    };
    let materialize: String = match &c.release_materialize_object_properties {
        Some(props) if !props.is_empty() => format!(
            "materialize {} ",
            props.iter().map(|p| format!("--term {p} ")).collect::<String>()
        ),
        _ => String::new(),
    };
    let reduce = "reduce -r $(REASONER) $(REDUCE_OPTIONS)";
    let date = if c.release_date { "--annotation oboInOwl:date \"$(OBODATE)\" " } else { "" };
    let close = format!(
        "$(SHARED_ROBOT_COMMANDS) annotate --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) {date}--output $@"
    );
    let sources = "$(EDIT_PREPROCESSED) $(OTHER_SRC)";
    let reasoned = c.release_use_reasoner;

    // Nothing that belongs to another ontology.
    if c.makes("base") {
        let classify = if reasoned { format!("{} {materialize}", reason(true)) } else { String::new() };
        let reduced = if reasoned { reduce } else { "" };
        b.rule(
            "$(ONT)-base.owl",
            &format!("{sources} $(IMPORT_FILES)"),
            "",
            &[&format!(
                "$(ROBOT_RELEASE_IMPORT_MODE) {classify}relax $(RELAX_OPTIONS) {reduced} \
                 remove {}--axioms external --preserve-structure false --trim false \
                 $(SHARED_ROBOT_COMMANDS) \
                 annotate --link-annotation http://purl.org/dc/elements/1.1/type http://purl.obolibrary.org/obo/IAO_8000001 \
                 --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) {date}--output $@",
                c.own_namespaces()
            )],
            &[],
        );
    }
    // The edit file and its components as written: no imports, no reasoning.
    if c.makes("baselite") {
        b.rule(
            "$(ONT)-baselite.owl",
            sources,
            "",
            &[&format!("$(ROBOT_RELEASE_IMPORT_MODE_BASE) {close}")],
            &[],
        );
    }
    // Imports merged in, classified.
    if c.makes("full") {
        b.rule(
            "$(ONT)-full.owl",
            &format!("{sources} $(IMPORT_FILES)"),
            "",
            &[&format!(
                "$(ROBOT_RELEASE_IMPORT_MODE) {} {materialize}relax $(RELAX_OPTIONS) {reduce} {close}",
                reason(false)
            )],
            &[],
        );
    }
    // Imports merged in, nothing inferred.
    if c.makes("non-classified") {
        b.rule(
            "$(ONT)-non-classified.owl",
            &format!("{sources} $(IMPORT_FILES)"),
            "",
            &[&format!("$(ROBOT_RELEASE_IMPORT_MODE) {close}")],
            &[],
        );
    }
    // Classified, then cut down to the ontology's own terms and the plain
    // hierarchy between them.
    if c.makes("simple") {
        let classify = if reasoned { reason(true) } else { String::new() };
        let reduced = if reasoned { reduce } else { "" };
        b.rule(
            "$(ONT)-simple.owl",
            &format!("{sources} $(SIMPLESEED) $(IMPORT_FILES)"),
            "",
            &[&format!(
                "$(ROBOT_RELEASE_IMPORT_MODE) {classify} relax $(RELAX_OPTIONS) \
                 remove --axioms equivalent \
                 filter --term-file $(SIMPLESEED) --select \"annotations ontology anonymous self\" --trim true --signature true \
                 {reduced} \
                 normalize --base-iri {uribase} --subset-decls true --synonym-decls true \
                 repair --merge-axiom-annotations true {close}"
            )],
            &[],
        );
    }
    // The same cut, without classifying first.
    if c.makes("simple-non-classified") {
        let reduced = if reasoned { reduce } else { "" };
        b.rule(
            "$(ONT)-simple-non-classified.owl",
            &format!("{sources} $(SIMPLESEED) $(IMPORT_FILES)"),
            "",
            &[&format!(
                "$(ROBOT_RELEASE_IMPORT_MODE_BASE) remove --axioms equivalent {reduced} \
                 filter --select ontology --term-file $(SIMPLESEED) --trim false {close}"
            )],
            &[],
        );
    }
    // The primary product with its translations merged in.
    if c.makes("international") {
        b.rule(
            "$(ONT)-international.owl",
            "$(ONT).owl $(TRANSLATIONS_OWL)",
            "",
            &[&format!("$(ROBOT) merge $(patsubst %, -i %, $^) {close}")],
            &[],
        );
    }
    // The simple cut, keeping only relationships over the listed relations.
    if c.makes("basic") {
        let classify = if reasoned { reason(true) } else { String::new() };
        b.rule(
            "$(ONT)-basic.owl",
            &format!("{sources} $(SIMPLESEED) $(KEEPRELATIONS) $(IMPORT_FILES)"),
            "",
            &[&format!(
                "$(ROBOT_RELEASE_IMPORT_MODE) {classify} relax $(RELAX_OPTIONS) \
                 remove --axioms equivalent \
                 remove --axioms disjoint \
                 remove --term-file $(KEEPRELATIONS) --select complement --select object-properties --trim true \
                 filter --term-file $(SIMPLESEED) --select \"annotations ontology anonymous self\" --trim true --signature true \
                 {reduce} {close}"
            )],
            &[],
        );
    }
    // An artefact the repository builds itself.
    for a in c.release_artefacts.iter().filter_map(|a| a.strip_prefix("custom-")) {
        b.rule(
            &format!("{a}.owl"),
            "",
            "",
            &[&format!(
                "echo \"ERROR: You have configured a custom release artefact ($@); \
                 this release artefact needs to be define in {id}.Makefile!\" && false"
            )],
            &[],
        );
    }
}

/// Commands for the people editing the ontology rather than for the release.
fn utilities(b: &mut Build, c: &Config) {
    b.rule(
        "explain_unsat",
        "$(EDIT_PREPROCESSED)",
        "",
        &["$(ROBOT) explain -i $< -M unsatisfiability --unsatisfiable random:10 --explanation $(TMPDIR)/$@.md"],
        &[],
    );
    // Rewriting the edit file in its own serialization, so a diff shows only what
    // an editor changed.
    if c.edit_format.contains("obo") {
        b.phony(
            "normalize_obo_src",
            "$(SRC)",
            "",
            &["$(ROBOT) repair -i $< --merge-axiom-annotations true \
               convert -o $(TMPDIR)/NORM.tmp.obo && mv $(TMPDIR)/NORM.tmp.obo $(SRC)"],
            &[],
        );
    }
    let format = if c.edit_format == "obo" { "obo --check false" } else { "ofn" };
    b.phony(
        "normalize_src",
        "$(SRC)",
        "",
        &[&format!("$(ROBOT) convert -i $< -f {format} -o $(TMPDIR)/normalise && mv $(TMPDIR)/normalise $<")],
        &[],
    );
    if c.documentation.is_some() {
        b.rule("update_docs", "", "", &["mkdocs gh-deploy --config-file ../../mkdocs.yaml"], &[]);
    }
    // The extended prefix map, for a repository's own rules to name as
    // `$(EXTENDED_PREFIX_MAP)`.
    b.rule("$(EXTENDED_PREFIX_MAP)", "/tools/obo.epm.json", "", &["cp $< $@"], &[]);
    b.rule(
        "validate-tsv",
        "$(TSV)",
        "$(TMPDIR)",
        &["for FILE in $< ; do \
           tsvalid $$FILE > $(TMPDIR)/validate.txt; \
           if [ -s $(TMPDIR)/validate.txt ]; then cat $(TMPDIR)/validate.txt && exit 1; fi ; \
           done"],
        &[],
    );
    b.rule("validate-all-tsv", "$(ALL_TSV_FILES)", "", &["$(MAKE) validate-tsv TSV=\"$^\""], &[]);
    let mut clean: Vec<&str> = Vec::new();
    if c.use_dosdps {
        clean.push("$(MAKE) pattern_clean");
    }
    clean.push(
        "for dir in $(MIRRORDIR) $(TMPDIR) $(UPDATEREPODIR) ; do \
         reldir=$$(realpath --relative-to=$$(pwd) $$dir) ; \
         case $$reldir in .*|\"\") ;; *) rm -rf $$reldir/* ;; esac \
         done",
    );
    clean.push("rm -f $(CLEANFILES)");
    b.phony("clean", "", "", &clean, &[]);
}
