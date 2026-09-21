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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportGroup {
    #[serde(default = "default_annotation_properties")]
    pub annotation_properties: Vec<String>,
    #[serde(default)]
    pub products: Vec<ImportProduct>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportProduct {
    pub id: String,
    /// `slme` (a locality module over the seed) or `mirror` (the whole ontology).
    #[serde(default = "default_module_type")]
    pub module_type: String,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentProduct {
    pub filename: String,
    #[serde(default)]
    pub use_template: bool,
    #[serde(default)]
    pub templates: Vec<String>,
    #[serde(default)]
    pub template_options: Option<String>,
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
fn default_primary_release() -> String {
    "full".into()
}
fn default_module_type() -> String {
    "slme".into()
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
        let config: Config = serde_yaml::from_str(text)
            .context("this configuration uses an option owlmake has no built-in rules for")?;
        config.check()?;
        Ok(config)
    }

    /// Options that parse but are covered for only some of their values.
    fn check(&self) -> Result<()> {
        for a in &self.release_artefacts {
            if !matches!(a.as_str(), "base" | "full" | "simple") {
                bail!("release artefact `{a}` has no built-in rules yet (covered: base, full, simple)");
            }
        }
        if !self.release_artefacts.contains(&self.primary_release) {
            bail!(
                "primary_release `{}` is not one of the release_artefacts",
                self.primary_release
            );
        }
        for f in &self.export_formats {
            if !matches!(f.as_str(), "owl" | "obo" | "json") {
                bail!("export format `{f}` has no built-in rules yet (covered: owl, obo, json)");
            }
        }
        for p in self.import_group.iter().flat_map(|g| &g.products) {
            if !matches!(p.module_type.as_str(), "slme" | "mirror") {
                bail!(
                    "import `{}`: module_type `{}` has no built-in rules yet (covered: slme, mirror)",
                    p.id,
                    p.module_type
                );
            }
        }
        for c in self.components.iter().flat_map(|g| &g.products) {
            if c.use_template && c.templates.is_empty() {
                bail!("component `{}` sets use_template but names no templates", c.filename);
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
    b.phony("all_odk", "test custom_reports all_assets", "", &[], &[]);
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
    b.phony("all_assets", "$(ASSETS) check_rdfxml_assets", "", &[], &[]);
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
    b.var("CATALOG", "catalog-v001.xml");
    b.var("ROBOT", "om --catalog $(CATALOG)");
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
    b.var("OBO_FORMAT_OPTIONS", "--clean-obo \"strict drop-untranslatable-axioms\"");
    b.var("SPARQL_VALIDATION_CHECKS", c.robot_report.custom_sparql_checks.join(" "));
    b.var("SPARQL_EXPORTS", c.robot_report.custom_sparql_exports.join(" "));
    b.var("ODK_VERSION_MAKEFILE", format!("v{BEHAVIOUR_SET}"));
    b.var("RELAX_OPTIONS", "--include-subclass-of true");
    b.var("REDUCE_OPTIONS", "--include-subproperties true");
    b.var("TODAY", "$(shell date +%Y-%m-%d)");
    b.var("VERSION", "$(TODAY)");
    b.var(
        "ANNOTATE_ONTOLOGY_VERSION",
        "annotate -V $(ONTBASE)/releases/$(VERSION)/$@ --annotation owl:versionInfo $(VERSION)",
    );
    // Staged through a temporary file, as a repository's own rules that end in
    // this variable expect: they may go on to edit the target in place.
    b.var(
        "ANNOTATE_CONVERT_FILE",
        "annotate --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) \
         convert -f ofn --output $@.tmp.owl && mv $@.tmp.owl $@",
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
    b.var("PRESEED", "$(TMPDIR)/pre_seed.txt");
    b.var("SIMPLESEED", "$(TMPDIR)/simple_seed.txt");
    b.var("IMPORTSEED", "$(TMPDIR)/seed.txt");
    b.var("T_IMPORTSEED", "--term-file $(IMPORTSEED)");

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
    let mut release_artefacts: Vec<String> =
        c.release_artefacts.iter().map(|a| format!("{id}-{a}")).collect();
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
    b.var("MAIN_GZIPPED", "");
    b.var("MAIN_FILES", cross(&main_products, &formats));

    let import_ids: Vec<String> = c.imports().iter().map(|p| p.id.clone()).collect();
    let import_roots: Vec<String> =
        import_ids.iter().map(|i| format!("$(IMPORTDIR)/{i}_import")).collect();
    b.var("IMPORTS", import_ids.join(" "));
    b.var("IMPORT_ROOTS", import_roots.join(" "));
    b.var("IMPORT_OWL_FILES", cross(&import_roots, &strings(&["owl"])));
    b.var("IMPORT_FILES", "$(IMPORT_OWL_FILES)");
    b.var(
        "ANNOTATION_PROPERTIES",
        c.import_group
            .as_ref()
            .map(|g| g.annotation_properties.join(" "))
            .unwrap_or_default(),
    );

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
    b.var("RELEASE_ASSETS", "$(MAIN_FILES) $(SUBSET_FILES)");
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
        &["$(ROBOT) reason --input $< --reasoner $(REASONER) --equivalent-classes-allowed asserted-only \
           --exclude-tautologies structural --output test.owl && rm test.owl"],
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

    let base_iris = if c.robot_report.use_base_iris {
        format!("--base-iri $(URIBASE)/{upper}_ --base-iri $(URIBASE)/{id} ")
    } else {
        String::new()
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

    b.rule("check_rdfxml_%", "%", "", &["@check-rdfxml $<"], &[]);
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
    if c.import_group.is_some() {
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
    if !c.release_artefacts.iter().any(|a| a == "simple") {
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

/// One module per import: a locality module over the seed, or the whole mirror.
fn imports(b: &mut Build, c: &Config) {
    let uribase = c.uribase.as_str();
    if b.switch("IMP") {
        let slme = format!(
            "$(ROBOT) annotate --input $< --remove-annotations \
             normalize --add-source true \
             extract --term-file $(IMPORTDIR)/$*_terms.txt $(T_IMPORTSEED) \
             --force true --copy-ontology-annotations true --individuals include --method BOT \
             remove $(foreach p, $(ANNOTATION_PROPERTIES), --term $(p)) \
             --term-file $(IMPORTDIR)/$*_terms.txt $(T_IMPORTSEED) \
             --select complement --select annotation-properties \
             normalize --base-iri {uribase} --subset-decls true --synonym-decls true \
             repair --merge-axiom-annotations true \
             $(ANNOTATE_CONVERT_FILE)"
        );
        b.rule(
            "$(IMPORTDIR)/%_import.owl",
            "$(MIRRORDIR)/%.owl $(IMPORTDIR)/%_terms.txt $(IMPORTSEED)",
            "",
            &[&slme],
            &["IMP"],
        );
        let whole = format!(
            "$(ROBOT) annotate --input $< --remove-annotations \
             normalize --base-iri {uribase} --subset-decls true --synonym-decls true --add-source true \
             repair --merge-axiom-annotations true \
             $(ANNOTATE_CONVERT_FILE)"
        );
        for p in c.imports().iter().filter(|p| p.module_type == "mirror") {
            b.rule(
                &format!("$(IMPORTDIR)/{}_import.owl", p.id),
                &format!("$(MIRRORDIR)/{}.owl", p.id),
                "",
                &[&whole],
                &["IMP"],
            );
        }
    }

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
    b.rule("$(TMPDIR)/stamp-component-%.owl", "", "$(TMPDIR)", &["touch $@"], g);

    for p in c.component_products().iter().filter(|p| p.use_template) {
        let templates: Vec<String> =
            p.templates.iter().map(|t| format!("$(TEMPLATEDIR)/{t}")).collect();
        let args: Vec<String> = templates.iter().map(|t| format!("--template {t}")).collect();
        let recipe = format!(
            "$(ROBOT) template {} {} $(ANNOTATE_CONVERT_FILE)",
            p.template_options.as_deref().unwrap_or(""),
            args.join(" ")
        );
        b.rule(
            &format!("$(COMPONENTSDIR)/{}", p.filename),
            &format!("{} $(TMPDIR)/stamp-component-{}", templates.join(" "), p.filename),
            "",
            &[&recipe],
            g,
        );
    }
}

/// Each import's upstream ontology, fetched and kept unless it has changed.
fn mirrors(b: &mut Build, c: &Config) {
    if !b.switch("MIR") {
        return;
    }
    let g = &["MIR"];
    for p in c.imports() {
        let id = p.id.as_str();
        b.phony(
            &format!("mirror-{id}"),
            "",
            "$(TMPDIR)",
            &[&format!(
                "curl -L $(OBOBASE)/{id}.owl --create-dirs -o $(TMPDIR)/{id}-download.owl --retry 4 --max-time 200 && \
                 $(ROBOT) convert -i $(TMPDIR)/{id}-download.owl -o $(TMPDIR)/$@.owl"
            )],
            g,
        );
    }
    b.rule(
        "$(MIRRORDIR)/%.owl",
        "mirror-%",
        "$(MIRRORDIR)",
        &["if [ -f $(TMPDIR)/mirror-$*.owl ]; then if cmp -s $(TMPDIR)/mirror-$*.owl $@ ; then \
           echo \"Mirror identical, ignoring.\"; else echo \"Mirrors different, updating.\" && \
           cp $(TMPDIR)/mirror-$*.owl $@; fi; fi"],
        g,
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
    let upper = c.id.to_uppercase();
    let uribase = c.uribase.as_str();
    let obo = "$(ROBOT) convert --input $< --check false -f obo $(OBO_FORMAT_OPTIONS) -o $@";
    let has = |f: &str| c.export_formats.iter().any(|x| x == f);

    for a in &c.release_artefacts {
        if has("obo") {
            b.rule(&format!("$(ONT)-{a}.obo"), &format!("$(ONT)-{a}.owl"), "", &[obo], &[]);
        }
        if has("json") {
            b.rule(
                &format!("$(ONT)-{a}.json"),
                &format!("$(ONT)-{a}.owl"),
                "",
                &["$(ROBOT) annotate --input $< --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) \
                   convert --check false -f json -o $@"],
                &[],
            );
        }
    }

    // The primary product is one of the artefacts under the ontology's own IRI.
    b.rule(
        "$(ONT).owl",
        &format!("$(ONT)-{}.owl", c.primary_release),
        "",
        &["$(ROBOT) annotate --input $< --ontology-iri $(URIBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) convert -o $@"],
        &[],
    );
    if has("obo") {
        b.rule("$(ONT).obo", "$(ONT).owl", "", &[obo], &[]);
    }
    if has("json") {
        b.rule(
            "$(ONT).json",
            "$(ONT).owl",
            "",
            &["$(ROBOT) annotate --input $< --ontology-iri $(URIBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) \
               convert --check false -f json -o $@"],
            &[],
        );
    }

    let reason = "reason --reasoner $(REASONER) --equivalent-classes-allowed asserted-only --exclude-tautologies structural";
    let annotate = "annotate --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) --output $@";
    let sources = "$(EDIT_PREPROCESSED) $(OTHER_SRC)";
    for a in &c.release_artefacts {
        match a.as_str() {
            // Nothing that belongs to another ontology.
            "base" => b.rule(
                "$(ONT)-base.owl",
                &format!("{sources} $(IMPORT_FILES)"),
                "",
                &[&format!(
                    "$(ROBOT) merge --input $< \
                     {reason} --annotate-inferred-axioms false \
                     relax $(RELAX_OPTIONS) \
                     reduce -r $(REASONER) $(REDUCE_OPTIONS) \
                     remove --base-iri $(URIBASE)/{upper} --axioms external --preserve-structure false --trim false \
                     annotate --link-annotation http://purl.org/dc/elements/1.1/type http://purl.obolibrary.org/obo/IAO_8000001 \
                     --ontology-iri $(ONTBASE)/$@ $(ANNOTATE_ONTOLOGY_VERSION) --output $@"
                )],
                &[],
            ),
            // Imports merged in, classified.
            "full" => b.rule(
                "$(ONT)-full.owl",
                &format!("{sources} $(IMPORT_FILES)"),
                "",
                &[&format!(
                    "$(ROBOT) merge --input $< {reason} relax $(RELAX_OPTIONS) \
                     reduce -r $(REASONER) $(REDUCE_OPTIONS) {annotate}"
                )],
                &[],
            ),
            // Classified, then cut down to the ontology's own terms and the
            // plain hierarchy between them.
            "simple" => b.rule(
                "$(ONT)-simple.owl",
                &format!("{sources} $(SIMPLESEED) $(IMPORT_FILES)"),
                "",
                &[&format!(
                    "$(ROBOT) merge --input $< {reason} --annotate-inferred-axioms false \
                     relax $(RELAX_OPTIONS) \
                     remove --axioms equivalent \
                     filter --term-file $(SIMPLESEED) --select \"annotations ontology anonymous self\" --trim true --signature true \
                     reduce -r $(REASONER) $(REDUCE_OPTIONS) \
                     normalize --base-iri {uribase} --subset-decls true --synonym-decls true \
                     repair --merge-axiom-annotations true {annotate}"
                )],
                &[],
            ),
            other => unreachable!("release artefact `{other}` passed Config::check"),
        }
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
