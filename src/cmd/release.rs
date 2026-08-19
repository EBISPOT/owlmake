//! `release` — run the release pipeline and emit the standard artefacts an
//! ontology release ships.
//!
//! Pipeline: merge inputs → relax (equivalences → SubClassOf) → reason (assert
//! inferred direct subsumptions, check coherence) → reduce (drop redundant
//! SubClassOf). Then write the `-full`, `-base` and `-simple` products in
//! RDF/XML, plus `.obo` and OBO-Graphs `.json` for the primary product, and run
//! the QC report.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{ClassExpression as CE, Component, MutableOntology, SubClassOf};
use horned_owl::ontology::set::SetOntology;

use crate::cmd::reduce;
use crate::extract::{self, Method};
use crate::io::{self, Format};
use crate::model::{clone_prefixes, Model};
use crate::reason::Reasoner;
use crate::{dosdp, report, sig};

/// The subset of a project config file that drives a release. Paths in it are
/// resolved relative to the config file's own directory.
#[derive(serde::Deserialize, Default)]
struct OdkConfig {
    id: Option<String>,
    #[serde(default)]
    reasoner: Option<String>,
    /// Local ontology files merged into the edit ontology before reasoning
    /// (the `components` step).
    #[serde(default)]
    components: Vec<ComponentCfg>,
    /// Import modules to build from local source ontologies and merge (the
    /// dynamic-import step).
    #[serde(default)]
    imports: Vec<ImportCfg>,
    /// DOSDP patterns to generate and merge before reasoning.
    #[serde(default)]
    patterns: Vec<PatternCfg>,
}

#[derive(serde::Deserialize)]
struct ComponentCfg {
    path: PathBuf,
}

#[derive(serde::Deserialize)]
struct ImportCfg {
    id: String,
    source: PathBuf,
    #[serde(default = "default_method")]
    method: String,
}

fn default_method() -> String {
    "BOT".to_string()
}

#[derive(serde::Deserialize)]
struct PatternCfg {
    pattern: PathBuf,
    data: PathBuf,
}

#[derive(ClapArgs)]
pub struct Args {
    /// Edit/source ontology (the `-edit` file or main ontology).
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Output directory for release artefacts.
    #[arg(short, long, default_value = ".")]
    pub output_dir: PathBuf,
    /// Ontology short id used to name artefacts (e.g. `cl`). Required unless
    /// supplied via `--config`.
    #[arg(long)]
    pub ontology_id: Option<String>,
    /// The reasoner to classify with.
    #[arg(long, default_value = "ELK", value_name = "NAME",
          value_parser = clap::builder::PossibleValuesParser::new(["ELK", "WHELK"]),
          ignore_case = true)]
    pub reasoner: String,

    /// REMOVED. A project config (`*-odk.yaml`) to read the ontology id (and other
    /// settings) from — compatibility with existing repositories.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// IRI prefix considered "internal"; axioms over other namespaces are
    /// treated as imported and dropped from the `-base` product. Defaults to
    /// `http://purl.obolibrary.org/obo/<ID>_`.
    #[arg(long)]
    pub base_prefix: Option<String>,
    /// Continue even if the ontology is incoherent (unsatisfiable classes).
    #[arg(long)]
    pub allow_incoherent: bool,
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
    std::fs::create_dir_all(&args.output_dir)?;

    // `om release` is a CHAIN STEP, not a repo build: it takes a piped or
    // `--input` model and runs a fixed relax→reason→reduce pipeline. Its inputs
    // are CLI arguments, full stop. Parsing a repo's build config here would be
    // a SECOND, independent ingest, and ingest belongs in `src/odk/`, which
    // resolves what it reads into the plan. A repo that wants its components,
    // imports and patterns wired in runs `om make`, which is the plan-driven
    // path.
    if args.config.is_some() {
        bail!(
            "`--config` has been removed: `om release` is a chain step and takes its inputs as \
             arguments. Use `om make` for a plan-driven build of a repository."
        );
    }
    let cfg: OdkConfig = match &None::<PathBuf> {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
            serde_yaml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("parsing ODK config {}: {e}", path.display()))?
        }
        None => OdkConfig::default(),
    };
    let id = match args.ontology_id.clone().or_else(|| cfg.id.clone()) {
        Some(id) => id,
        None => bail!("release requires --ontology-id"),
    };
    if let Some(r) = &cfg.reasoner.clone().or_else(|| Some(args.reasoner.clone())) {
        if !r.eq_ignore_ascii_case("ELK") && !r.eq_ignore_ascii_case("WHELK") {
            status!("release: config requests reasoner '{r}'; using the built-in EL reasoner");
        }
    }
    let id = &id;
    let base_prefix = args
        .base_prefix
        .clone()
        .unwrap_or_else(|| format!("http://purl.obolibrary.org/obo/{}_", id.to_uppercase()));

    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    status!("  {} components", model.ont.iter().count());

    // Config-driven pre-processing (the seeding / imports / patterns steps),
    // resolved relative to the config file's directory.
    if let Some(cfg_path) = &args.config {
        let cfg_dir = cfg_path.parent().unwrap_or_else(|| Path::new("."));
        run_config_steps(&mut model, &cfg, cfg_dir, &args.output_dir)?;
    }

    // -baselite: the import-free product *without* reasoning (built from the
    // unreasoned edit ontology).
    let mut baselite = make_base(&model, &base_prefix);
    let baselite_path = artefact(&args.output_dir, id, "-baselite", "owl");
    io::save_as(&mut baselite, &baselite_path, Format::RdfXml)?;
    status!("  wrote {} ({} components)", baselite_path.display(), baselite.ont.iter().count());

    // 1. Relax: equivalences with conjunctions → weaker SubClassOf.
    let relaxed = relax(&mut model);
    status!("  relax: +{relaxed} SubClassOf axioms");

    // 2. Reason: coherence check + assert inferred direct subsumptions.
    let reasoner = Reasoner::classify(&model);
    if !reasoner.is_consistent() {
        bail!("release: ontology is inconsistent");
    }
    let unsat = reasoner.unsatisfiable();
    if !unsat.is_empty() {
        status!("  WARNING: {} unsatisfiable class(es)", unsat.len());
        if !args.allow_incoherent {
            bail!(
                "release: {} unsatisfiable class(es); pass --allow-incoherent to continue",
                unsat.len()
            );
        }
    }
    let inferred = reasoner.direct_subsumptions();
    let mut asserted = 0;
    for (sub, sup) in inferred {
        let comp = Component::SubClassOf(SubClassOf {
            sub: CE::Class(model.build.class(sub)),
            sup: CE::Class(model.build.class(sup)),
        });
        if model.ont.insert(comp) {
            asserted += 1;
        }
    }
    status!("  reason: +{asserted} inferred SubClassOf axioms");

    // 3. Reduce: remove redundant SubClassOf.
    let mut full = reduce::reduce(&model);
    status!("  reduce: {} components in -full", full.ont.iter().count());

    // -full
    let full_path = artefact(&args.output_dir, id, "-full", "owl");
    io::save_as(&mut full, &full_path, Format::RdfXml)?;
    status!("  wrote {}", full_path.display());

    // -base: drop axioms that mention only-external terms (imports).
    let mut base = make_base(&full, &base_prefix);
    let base_path = artefact(&args.output_dir, id, "-base", "owl");
    io::save_as(&mut base, &base_path, Format::RdfXml)?;
    status!("  wrote {} ({} components)", base_path.display(), base.ont.iter().count());

    // -simple: SubClassOf-only view over internal terms.
    let mut simple = make_simple(&full, &base_prefix);
    let simple_path = artefact(&args.output_dir, id, "-simple", "owl");
    io::save_as(&mut simple, &simple_path, Format::RdfXml)?;
    status!("  wrote {}", simple_path.display());

    // -basic: the OBO-safe subset — `-simple` plus existential relationships
    // (`A ⊑ ∃r.B`) over internal terms, which legacy OBO tooling can consume.
    let mut basic = make_basic(&full, &base_prefix);
    let basic_path = artefact(&args.output_dir, id, "-basic", "owl");
    io::save_as(&mut basic, &basic_path, Format::RdfXml)?;
    let basic_obo = artefact(&args.output_dir, id, "-basic", "obo");
    io::save_as(&mut basic, &basic_obo, Format::Obo)?;
    status!(
        "  wrote {}, {} ({} components)",
        basic_path.display(),
        basic_obo.display(),
        basic.ont.iter().count()
    );

    // Primary product + OBO + OBO-Graphs JSON.
    let main_owl = artefact(&args.output_dir, id, "", "owl");
    io::save_as(&mut full, &main_owl, Format::RdfXml)?;
    let obo_path = artefact(&args.output_dir, id, "", "obo");
    io::save_as(&mut full, &obo_path, Format::Obo)?;
    let json_path = artefact(&args.output_dir, id, "", "json");
    io::save_as(&mut full, &json_path, Format::OboGraph)?;
    status!("  wrote {}, {}, {}", main_owl.display(), obo_path.display(), json_path.display());

    // QC report.
    let qc = report::run_report(&full)?;
    let report_path = artefact(&args.output_dir, &format!("{id}-report"), "", "tsv");
    std::fs::write(&report_path, qc.to_tsv())?;
    let errors = qc.count_at_least(report::Severity::Error);
    status!(
        "  report: {} violation(s), {} ERROR → {}",
        qc.rows.len(),
        errors,
        report_path.display()
    );

    status!("release: done.");
    Ok(None)
}

fn artefact(dir: &std::path::Path, id: &str, suffix: &str, ext: &str) -> PathBuf {
    dir.join(format!("{id}{suffix}.{ext}"))
}

/// Add `A SubClassOf Bi` for each conjunct of `A EquivalentTo (B1 ⊓ ... ⊓ Bn)`.
fn relax(model: &mut Model) -> usize {
    let mut to_add = Vec::new();
    for ac in model.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            let named: Vec<&CE<_>> = eq.0.iter().filter(|c| matches!(c, CE::Class(_))).collect();
            for n in &named {
                for member in &eq.0 {
                    if let CE::ObjectIntersectionOf(parts) = member {
                        for p in parts {
                            to_add.push(Component::SubClassOf(SubClassOf {
                                sub: (*n).clone(),
                                sup: p.clone(),
                            }));
                        }
                    }
                }
            }
        }
    }
    let mut n = 0;
    for c in to_add {
        if model.ont.insert(c) {
            n += 1;
        }
    }
    n
}

/// Run the config-declared pre-reasoning steps: generate DOSDP patterns, merge
/// local components, and build/merge dynamic import modules. All paths are
/// resolved relative to `cfg_dir`; generated import modules are also written to
/// `out_dir/imports/`.
fn run_config_steps(
    model: &mut Model,
    cfg: &OdkConfig,
    cfg_dir: &Path,
    out_dir: &Path,
) -> Result<()> {
    let resolve = |p: &Path| {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cfg_dir.join(p)
        }
    };

    // DOSDP patterns → logical axioms, merged in.
    for pat in &cfg.patterns {
        let pattern = std::fs::read_to_string(resolve(&pat.pattern))?;
        let data = std::fs::read_to_string(resolve(&pat.data))?;
        let labels = label_index(model);
        let generated = dosdp::generate(&pattern, &data, &labels)?;
        let n = merge_into(model, &generated);
        status!("  dosdp {}: +{n} components", pat.pattern.display());
    }

    // Local components merged verbatim.
    for c in &cfg.components {
        let comp = io::load(&resolve(&c.path))?;
        let n = merge_into(model, &comp);
        status!("  component {}: +{n} components", c.path.display());
    }

    // Dynamic imports: a locality module of each source over the signature the
    // (current) edit ontology uses.
    if !cfg.imports.is_empty() {
        let seed: HashSet<String> = model
            .ont
            .iter()
            .flat_map(|ac| sig::signature(&ac.component))
            .collect();
        let imports_dir = out_dir.join("imports");
        std::fs::create_dir_all(&imports_dir)?;
        for imp in &cfg.imports {
            let source = io::load(&resolve(&imp.source))?;
            let method = Method::parse(&imp.method)
                .ok_or_else(|| anyhow::anyhow!("unknown extract method: {}", imp.method))?;
            let mut module = extract::extract(&source, &seed, method);
            let mod_path = imports_dir.join(format!("{}_import.owl", imp.id));
            io::save_as(&mut module, &mod_path, Format::RdfXml)?;
            let n = merge_into(model, &module);
            status!(
                "  import {}: {} → {} ({n} merged) → {}",
                imp.id,
                seed.len(),
                module.ont.iter().count(),
                mod_path.display()
            );
        }
    }
    Ok(())
}

/// Insert every component of `other` into `model`; returns the number added.
fn merge_into(model: &mut Model, other: &Model) -> usize {
    let mut n = 0;
    for ac in other.ont.iter() {
        if model.ont.insert(ac.clone()) {
            n += 1;
        }
    }
    n
}

/// `rdfs:label` lookup over a model, for DOSDP variable filling.
fn label_index(model: &Model) -> std::collections::HashMap<String, String> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Literal};
    let mut labels = std::collections::HashMap::new();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == "http://www.w3.org/2000/01/rdf-schema#label" {
                if let (AnnotationSubject::IRI(s), AnnotationValue::Literal(lit)) =
                    (&aa.subject, &aa.ann.av)
                {
                    let text = match lit {
                        Literal::Simple { literal }
                        | Literal::Language { literal, .. }
                        | Literal::Datatype { literal, .. } => literal.clone(),
                    };
                    labels.insert(s.as_ref().to_string(), text);
                }
            }
        }
    }
    labels
}

/// The `-basic` product: the `-simple` graph plus existential relationships
/// (`A ⊑ ∃r.B`) between internal named classes — an OBO-exportable view.
fn make_basic(model: &Model, base_prefix: &str) -> Model {
    let internal = |iri: &str| iri.starts_with(base_prefix);
    let mut ont = SetOntology::new();
    for ac in model.ont.iter() {
        let keep = match &ac.component {
            Component::DeclareClass(dc) => internal(dc.0 .0.as_ref()),
            Component::SubClassOf(sc) => match (&sc.sub, &sc.sup) {
                (CE::Class(a), CE::Class(b)) => internal(a.0.as_ref()) && internal(b.0.as_ref()),
                // A ⊑ ∃r.B between internal named terms (an OBO relationship).
                (CE::Class(a), CE::ObjectSomeValuesFrom { bce, .. }) => {
                    internal(a.0.as_ref())
                        && matches!(bce.as_ref(), CE::Class(b) if internal(b.0.as_ref()))
                }
                _ => false,
            },
            Component::AnnotationAssertion(_) => {
                sig::signature(&ac.component).iter().any(|s| internal(s))
            }
            Component::OntologyID(_) => true,
            _ => false,
        };
        if keep {
            ont.insert(ac.clone());
        }
    }
    Model::from_parts(ont, clone_prefixes(&model.prefixes))
}

/// The `-base` product: keep axioms that mention at least one internal term.
fn make_base(model: &Model, base_prefix: &str) -> Model {
    let mut ont = SetOntology::new();
    for ac in model.ont.iter() {
        let keep = match &ac.component {
            Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_) => {
                true
            }
            _ => sig::signature(&ac.component)
                .iter()
                .any(|s| s.starts_with(base_prefix)),
        };
        if keep {
            ont.insert(ac.clone());
        }
    }
    Model::from_parts(ont, clone_prefixes(&model.prefixes))
}

/// The `-simple` product: SubClassOf (named ⊑ named) over internal terms, plus
/// labels/definitions.
fn make_simple(model: &Model, base_prefix: &str) -> Model {
    let internal = |iri: &str| iri.starts_with(base_prefix);
    let mut ont = SetOntology::new();
    for ac in model.ont.iter() {
        let keep = match &ac.component {
            Component::DeclareClass(dc) => internal(dc.0 .0.as_ref()),
            Component::SubClassOf(sc) => matches!((&sc.sub, &sc.sup),
                (CE::Class(a), CE::Class(b)) if internal(a.0.as_ref()) && internal(b.0.as_ref())),
            Component::AnnotationAssertion(_) => sig::signature(&ac.component)
                .iter()
                .any(|s| internal(s)),
            Component::OntologyID(_) => true,
            _ => false,
        };
        if keep {
            ont.insert(ac.clone());
        }
    }
    Model::from_parts(ont, clone_prefixes(&model.prefixes))
}
