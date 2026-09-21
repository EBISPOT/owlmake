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
use serde::de::IgnoredAny;
use serde::Deserialize;

use super::makefile::{MakeModel, Rule};

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

/// A repository's build configuration: the options the built-in rules cover.
///
/// Unknown fields are an error, which is what makes an uncovered option fail by
/// name. The fields held as [`IgnoredAny`] are accepted because they describe the
/// repository (its title, where it is hosted, how its documentation is built)
/// and change nothing about what the build produces.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

    // Describes the repository; no effect on what is built.
    #[serde(default)]
    title: Option<IgnoredAny>,
    #[serde(default)]
    description: Option<IgnoredAny>,
    #[serde(default)]
    license: Option<IgnoredAny>,
    #[serde(default)]
    contact: Option<IgnoredAny>,
    #[serde(default)]
    creators: Option<IgnoredAny>,
    #[serde(default)]
    github_org: Option<IgnoredAny>,
    #[serde(default)]
    repo: Option<IgnoredAny>,
    #[serde(default)]
    git_main_branch: Option<IgnoredAny>,
    #[serde(default)]
    documentation: Option<IgnoredAny>,
    #[serde(default)]
    ci: Option<IgnoredAny>,
    #[serde(default)]
    workflows: Option<IgnoredAny>,
    /// Memory for a JVM owlmake does not start.
    #[serde(default)]
    robot_java_args: Option<IgnoredAny>,
}

/// The import modules, and what holds for all of them unless a product says
/// otherwise.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

    #[serde(default)]
    disabled: Option<IgnoredAny>,
    #[serde(default)]
    rebuild_if_source_changes: Option<IgnoredAny>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

    #[serde(default)]
    maintenance: Option<IgnoredAny>,
    #[serde(default)]
    rebuild_if_source_changes: Option<IgnoredAny>,
    #[serde(default)]
    robot_settings: Option<IgnoredAny>,
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
            maintenance: None,
            rebuild_if_source_changes: None,
            robot_settings: None,
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
            disabled: None,
            rebuild_if_source_changes: None,
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
#[serde(deny_unknown_fields)]
pub struct SubsetGroup {
    #[serde(default)]
    pub products: Vec<SubsetProduct>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubsetProduct {
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentGroup {
    #[serde(default)]
    pub products: Vec<ComponentProduct>,
    #[serde(default)]
    disabled: Option<IgnoredAny>,
    #[serde(default)]
    rebuild_if_source_changes: Option<IgnoredAny>,
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
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotReport {
    #[serde(default)]
    pub fail_on: Option<String>,
    #[serde(default = "yes")]
    pub use_labels: bool,
    #[serde(default = "yes")]
    pub use_base_iris: bool,
    #[serde(default)]
    pub custom_profile: bool,
    #[serde(default)]
    pub release_reports: bool,
    #[serde(default = "default_on_edit")]
    pub report_on: Vec<String>,
    #[serde(default = "default_on_edit")]
    pub sparql_test_on: Vec<String>,
    #[serde(default = "default_sparql_checks")]
    pub custom_sparql_checks: Vec<String>,
    #[serde(default = "default_sparql_exports")]
    pub custom_sparql_exports: Vec<String>,
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
        let mut config: Config = serde_yaml::from_str(text)
            .context("this configuration uses an option owlmake has no built-in rules for")?;
        if let Some(g) = &mut config.import_group {
            g.derive();
        }
        if let Some(g) = &mut config.components {
            g.derive(&config.uribase, &config.id);
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
        if self.robot_report.custom_profile {
            bail!("robot_report.custom_profile has no built-in rules yet");
        }
        if self.robot_report.release_reports {
            bail!("robot_report.release_reports has no built-in rules yet");
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
        "validate_idranges reason_test sparql_test robot_reports \
         $(REPORTDIR)/validate_profile_owl2dl_$(ONT).owl.txt",
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
    b.var("REPORT_PROFILE_OPTS", "");
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
    let other_src: Vec<String> = c
        .component_products()
        .iter()
        .map(|p| format!("$(COMPONENTSDIR)/{}", p.filename))
        .collect();
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

    b.var("MAPPINGS", "");
    b.var("MAPPING_FILES", "");

    let report_of = |x: &String| if x == "edit" { "$(SRC)".to_string() } else { x.clone() };
    let obo_reports: Vec<String> =
        c.robot_report.report_on.iter().map(|x| format!("{}-obo-report", report_of(x))).collect();
    b.var("OBO_REPORT", obo_reports.join(" "));
    b.var("REPORTS", "$(OBO_REPORT)");
    b.var(
        "REPORT_FILES",
        obo_reports.iter().map(|r| format!("$(REPORTDIR)/{r}.tsv")).collect::<Vec<_>>().join(" "),
    );
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
    b.var("ASSETS", "$(IMPORT_FILES) $(MAIN_FILES) $(REPORT_FILES) $(SUBSET_FILES) $(MAPPING_FILES)");
    let released_imports =
        if group.is_some_and(|g| g.release_imports) { "$(IMPORT_FILES) " } else { "" };
    b.var("RELEASE_ASSETS", format!("$(MAIN_FILES) {released_imports}$(SUBSET_FILES)"));
    b.var("CLEANFILES", "$(MAIN_FILES) $(SRCMERGED) $(EDIT_PREPROCESSED)");
    let released: Vec<String> = b
        .words("$(RELEASE_ASSETS)")
        .iter()
        .map(|n| format!("$(RELEASEDIR)/{n}"))
        .collect();
    b.var("RELEASE_ASSETS_AFTER_RELEASE", released.join(" "));
    b.var("CURRENT_RELEASE", "$(ONTBASE).owl");
    b.var("TSV", "");
    b.var("ALL_TSV_FILES", "");
    b.var("GHVERSION", "v$(VERSION)");
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
    b.phony("copy_release_files", "", "", &["rsync -R $(RELEASE_ASSETS) $(RELEASEDIR)"], &[]);
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
        b.rule("$(IMPORTSEED)", "$(PRESEED)", "$(TMPDIR)", &["cat $^ | sort | uniq > $@"], &[]);
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
    b.phony(
        "normalize_src",
        "$(SRC)",
        "",
        &["$(ROBOT) convert -i $< -f ofn -o $(TMPDIR)/normalise && mv $(TMPDIR)/normalise $<"],
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
    b.phony(
        "clean",
        "",
        "",
        &[
            "for dir in $(MIRRORDIR) $(TMPDIR) $(UPDATEREPODIR) ; do \
             reldir=$$(realpath --relative-to=$$(pwd) $$dir) ; \
             case $$reldir in .*|\"\") ;; *) rm -rf $$reldir/* ;; esac \
             done",
            "rm -f $(CLEANFILES)",
        ],
        &[],
    );
}
