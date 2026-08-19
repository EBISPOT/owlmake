//! `measure` — report ontology metrics.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{Component, Kinded};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// Output file (defaults to stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output format: tsv (default), csv, yaml, or json.
    #[arg(short, long, default_value = "tsv")]
    pub format: String,
    /// Which metric set to compute: `essential` (default, entity/axiom counts),
    /// `extended`, or `all` (also include the per-axiom-kind breakdown).
    #[arg(short = 'm', long = "metrics", default_value = "essential")]
    pub metrics: String,
    /// Reasoner to use for reasoned metrics: `elk` (default), `structural`,
    /// `emr`, `owlmake`, `whelk`, `hermit`, `jfact`. Only the `extended`/`all`
    /// metric sets use the reasoner; they add an `inferred_subclass_axioms`
    /// count (the total number of inferred SubClassOf edges from
    /// classification).
    #[arg(short = 'r', long = "reasoner", default_value = "elk")]
    pub reasoner: String,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

/// The headline ontology metrics (the `essential` set), as sorted
/// `(name, count)` pairs — entity declaration counts, logical-axiom count, and
/// the total component count. The pure, in-memory core behind the embedding
/// API's `measure()`; [`step`] computes the same counts itself, in the single
/// pass that also builds the per-axiom-kind breakdown.
pub fn essential_metrics(model: &crate::model::Model) -> Vec<(&'static str, usize)> {
    let (mut classes, mut object_props, mut data_props) = (0usize, 0usize, 0usize);
    let (mut annotation_props, mut individuals, mut datatypes) = (0usize, 0usize, 0usize);
    let (mut logical, mut total) = (0usize, 0usize);
    for ac in model.ont.iter() {
        total += 1;
        match &ac.component {
            Component::DeclareClass(_) => classes += 1,
            Component::DeclareObjectProperty(_) => object_props += 1,
            Component::DeclareDataProperty(_) => data_props += 1,
            Component::DeclareAnnotationProperty(_) => annotation_props += 1,
            Component::DeclareNamedIndividual(_) => individuals += 1,
            Component::DeclareDatatype(_) => datatypes += 1,
            // Metadata (not a logical axiom).
            Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::OntologyAnnotation(_)
            | Component::Import(_)
            | Component::AnnotationAssertion(_) => {}
            _ => logical += 1,
        }
    }
    let mut metrics = vec![
        ("classes", classes),
        ("object_properties", object_props),
        ("data_properties", data_props),
        ("annotation_properties", annotation_props),
        ("named_individuals", individuals),
        ("datatypes", datatypes),
        ("logical_axioms", logical),
        ("total_components", total),
    ];
    metrics.sort_by_key(|(k, _)| *k);
    metrics
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    let mut classes = 0usize;
    let mut object_props = 0usize;
    let mut data_props = 0usize;
    let mut annotation_props = 0usize;
    let mut individuals = 0usize;
    let mut datatypes = 0usize;
    let mut logical = 0usize;
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();

    for ac in model.ont.iter() {
        let kind = format!("{:?}", ac.component.kind());
        *by_kind.entry(kind).or_default() += 1;
        match &ac.component {
            Component::DeclareClass(_) => classes += 1,
            Component::DeclareObjectProperty(_) => object_props += 1,
            Component::DeclareDataProperty(_) => data_props += 1,
            Component::DeclareAnnotationProperty(_) => annotation_props += 1,
            Component::DeclareNamedIndividual(_) => individuals += 1,
            Component::DeclareDatatype(_) => datatypes += 1,
            // Logical axioms (everything that is neither a declaration nor metadata).
            Component::OntologyID(_)
            | Component::DocIRI(_)
            | Component::OntologyAnnotation(_)
            | Component::Import(_)
            | Component::AnnotationAssertion(_) => {}
            _ => logical += 1,
        }
    }

    let mut metrics: Vec<(&str, usize)> = vec![
        ("classes", classes),
        ("object_properties", object_props),
        ("data_properties", data_props),
        ("annotation_properties", annotation_props),
        ("named_individuals", individuals),
        ("datatypes", datatypes),
        ("logical_axioms", logical),
        ("total_components", model.ont.iter().count()),
    ];
    metrics.sort_by_key(|(k, _)| *k);

    // `essential` reports only the headline counts; `extended`/`all` add the
    // per-axiom-kind breakdown and a reasoned `inferred_subclass_axioms` metric.
    let include_kinds = match args.metrics.to_ascii_lowercase().as_str() {
        "essential" => false,
        "extended" | "all" => true,
        other => {
            status!("measure: unknown metrics level '{other}'; using 'essential'");
            false
        }
    };

    // Reasoned metrics (extended/all only): classify with the chosen reasoner
    // and report the number of inferred SubClassOf edges.
    if include_kinds {
        let inferred = inferred_subclass_count(&model, &args.reasoner);
        metrics.push(("inferred_subclass_axioms", inferred));
        metrics.sort_by_key(|(k, _)| *k);
    }

    let fmt = args.format.to_ascii_lowercase();
    let mut out = String::new();
    match fmt.as_str() {
        "yaml" => {
            out.push_str("metrics:\n");
            for (k, v) in &metrics {
                out.push_str(&format!("  {k}: {v}\n"));
            }
            if include_kinds {
                out.push_str("axiom_types:\n");
                for (k, v) in &by_kind {
                    out.push_str(&format!("  {k}: {v}\n"));
                }
            }
        }
        "json" => {
            out.push_str("{\n  \"metrics\": {\n");
            for (i, (k, v)) in metrics.iter().enumerate() {
                let comma = if i + 1 < metrics.len() { "," } else { "" };
                out.push_str(&format!("    \"{k}\": {v}{comma}\n"));
            }
            out.push_str("  }");
            if include_kinds {
                out.push_str(",\n  \"axiom_types\": {\n");
                for (i, (k, v)) in by_kind.iter().enumerate() {
                    let comma = if i + 1 < by_kind.len() { "," } else { "" };
                    out.push_str(&format!("    \"{k}\": {v}{comma}\n"));
                }
                out.push_str("  }");
            }
            out.push_str("\n}\n");
        }
        _ => {
            // tsv (default) or csv
            let sep = if fmt == "csv" { "," } else { "\t" };
            out.push_str(&format!("metric{sep}value\n"));
            for (k, v) in &metrics {
                out.push_str(&format!("{k}{sep}{v}\n"));
            }
            if include_kinds {
                for (k, v) in &by_kind {
                    out.push_str(&format!("kind:{k}{sep}{v}\n"));
                }
            }
        }
    }

    match &args.output {
        Some(p) => std::fs::write(p, out)?,
        None => print!("{out}"),
    }
    Ok(Some(model))
}

/// Classify `model` with `reasoner` and return the number of inferred
/// SubClassOf edges (all subsumptions, excluding reflexive X ⊑ X). Dispatches
/// across the EL/whelk/DL backends the same way `reason` does.
fn inferred_subclass_count(model: &crate::model::Model, reasoner: &str) -> usize {
    let lc = reasoner.to_ascii_lowercase();
    let pairs: Vec<(String, String)> = match lc.as_str() {
        // hermit-rs (DL) and whelk-rs (EL) both build for wasm, so every backend
        // is available in the browser too (see src/reason/mod.rs).
        "hermit" | "jfact" => crate::reason::DlReasoner::classify(model).all_subsumptions(),
        "whelk" => crate::reason::WhelkClassification::classify(model).direct_subsumptions(),
        _ => {
            if !matches!(lc.as_str(), "elk" | "structural" | "emr" | "owlmake") {
                status!("measure: unknown reasoner '{reasoner}'; using the EL reasoner");
            }
            crate::reason::el::set_whelk_mode(lc == "owlmake");
            crate::reason::Reasoner::classify(model).all_subsumptions()
        }
    };
    pairs.iter().filter(|(a, b)| a != b).count()
}
