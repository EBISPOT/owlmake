//! `unmerge` — remove the axioms of a second ontology from the first, leaving
//! only what the base ontology asserts on its own.

use std::collections::HashSet;
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{AnnotatedComponent, MutableOntology, RcStr};
use horned_owl::ontology::set::SetOntology;

use crate::io;
use crate::model::Model;

#[derive(ClapArgs)]
pub struct Args {
    /// Input ontology files (repeatable). The FIRST is the base; every
    /// subsequent input has its axioms subtracted from it. When a model is
    /// piped in, the piped model is the base and all `-i` inputs are
    /// subtracted.
    #[arg(short, long)]
    pub input: Vec<PathBuf>,
    /// The ontology whose axioms are subtracted from the input (owlmake alias;
    /// equivalent to giving a second `-i`).
    #[arg(long)]
    pub second_input: Option<PathBuf>,
    /// Subtract every ontology matching this glob pattern.
    #[arg(short = 'p', long = "inputs", value_name = "PATTERN")]
    pub inputs: Option<String>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

/// Subtract `to_remove` from `base`, returning the result and how many axioms went.
///
/// The ONE implementation of unmerge — `om unmerge` and a plan's unmerge step
/// both come through here, so the two cannot say different things.
///
/// Ontology identity is never subtracted. A merge skips a secondary input's
/// `OntologyID`/`DocIRI`, so those cannot have arrived from a subtrahend, and
/// removing them would take away something the base asserts about itself. It
/// matters whenever the two share an IRI, which is the shape of any component
/// EXTRACTED from the ontology it is later subtracted from: the result keeps its
/// own name rather than becoming anonymous.
///
/// Matching is on the WHOLE annotated component, not the bare component: in OWL
/// an axiom and the same axiom carrying different annotations are different
/// axioms, and only the one actually present in the subtrahend goes.
pub(crate) fn subtract(
    base: Model,
    to_remove: &HashSet<AnnotatedComponent<RcStr>>,
) -> (Model, usize) {
    use horned_owl::model::Component;
    let mut ont = SetOntology::new();
    let mut removed = 0usize;
    for ac in base.ont.iter() {
        let is_meta = matches!(ac.component, Component::OntologyID(_) | Component::DocIRI(_));
        if !is_meta && to_remove.contains(ac) {
            removed += 1;
            continue;
        }
        ont.insert(ac.clone());
    }
    // `from_parts` REBUILDS the model, so document metadata is carried over
    // explicitly — `rdf_prefixes` above all, the verbatim xmlns block the RDF/XML
    // writer reproduces. Carrying it keeps the document's own prefix names: two
    // namespaces can generate the same name (`…/efo/` and `…/efo/#` both want
    // `efo`), and only the input's declaration says which holds it.
    let mut out = Model::from_parts(ont, crate::model::clone_prefixes(&base.prefixes));
    out.carry_meta_from(&base);
    (out, removed)
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    // Determine the base and the ontologies to subtract. The first `-i` is the
    // base and the rest are subtrahends; if a model is piped in, that is the base
    // and all `-i` inputs are subtracted.
    let mut subtrahends: Vec<PathBuf> = Vec::new();
    let mut base = if let Some(piped) = piped {
        subtrahends.extend(args.input.iter().cloned());
        piped
    } else {
        let mut iter = args.input.iter();
        let first = iter.next().cloned();
        subtrahends.extend(iter.cloned());
        crate::cmd::take_or_load(None, first.as_deref(), &args.common)?
    };
    args.common.apply(&mut base)?;

    if let Some(p) = &args.second_input {
        subtrahends.push(p.clone());
    }
    if let Some(pattern) = &args.inputs {
        subtrahends.extend(crate::cmd::merge::expand_glob(pattern)?);
    }
    if subtrahends.is_empty() {
        anyhow::bail!("unmerge: no ontology to subtract (give a second -i, --second-input, or --inputs)");
    }

    let mut to_remove: HashSet<AnnotatedComponent<RcStr>> = HashSet::new();
    for path in &subtrahends {
        let second = io::load(path)?;
        to_remove.extend(second.ont.iter().cloned());
    }

    let (mut result, removed) = subtract(base, &to_remove);
    status!("unmerge: removed {removed} shared axiom(s)");

    crate::cmd::maybe_save(&mut result, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::{Build, Component, OntologyID};

    /// A subtrahend that shares the base's ontology IRI does not take it away.
    ///
    /// A component extracted from an ontology keeps that ontology's IRI, so
    /// subtracting one back out meets its own `OntologyID`. The result keeps its
    /// name: an anonymous ontology gives the RDF/XML writer no namespace to use
    /// as the default `xmlns`, which renames every element in the document.
    #[test]
    fn unmerge_keeps_the_base_ontology_iri_a_subtrahend_shares() {
        let b = Build::new_rc();
        let id = OntologyID {
            iri: Some(b.iri("http://www.ebi.ac.uk/efo")),
            viri: None,
        };
        let mut base_ont = SetOntology::new();
        base_ont.insert(AnnotatedComponent::from(Component::OntologyID(id.clone())));
        base_ont.insert(AnnotatedComponent::from(Component::DeclareClass(
            horned_owl::model::DeclareClass(b.class("http://example.org/Keep")),
        )));
        base_ont.insert(AnnotatedComponent::from(Component::DeclareClass(
            horned_owl::model::DeclareClass(b.class("http://example.org/Drop")),
        )));
        let base = Model::from_parts(base_ont, Default::default());

        // The subtrahend carries the SAME OntologyID, plus one shared axiom.
        let mut to_remove: HashSet<AnnotatedComponent<RcStr>> = HashSet::new();
        to_remove.insert(AnnotatedComponent::from(Component::OntologyID(id)));
        to_remove.insert(AnnotatedComponent::from(Component::DeclareClass(
            horned_owl::model::DeclareClass(b.class("http://example.org/Drop")),
        )));

        let (out, removed) = subtract(base, &to_remove);
        assert_eq!(removed, 1, "only the shared axiom is subtracted");
        let iri = out.ont.iter().find_map(|ac| match &ac.component {
            Component::OntologyID(i) => i.iri.clone(),
            _ => None,
        });
        assert_eq!(
            iri.map(|i| i.to_string()).as_deref(),
            Some("http://www.ebi.ac.uk/efo"),
            "the base keeps its own ontology IRI"
        );
        assert!(out.ont.iter().any(|ac| matches!(&ac.component,
            Component::DeclareClass(d) if d.0.0.to_string().ends_with("Keep"))));
        assert!(!out.ont.iter().any(|ac| matches!(&ac.component,
            Component::DeclareClass(d) if d.0.0.to_string().ends_with("Drop"))));
    }

    /// The document's verbatim xmlns block survives the subtraction.
    ///
    /// `from_parts` rebuilds the model and blanks `rdf_prefixes`, so the metadata
    /// is carried over. It decides the prefix NAMES: two namespaces can generate
    /// the same one — `http://www.ebi.ac.uk/efo/` and `.../efo/#` both want
    /// `efo` — and only the document's own declaration settles which holds it.
    #[test]
    fn unmerge_keeps_the_documents_own_prefix_block() {
        let b = Build::new_rc();
        let mut base_ont = SetOntology::new();
        base_ont.insert(AnnotatedComponent::from(Component::DeclareClass(
            horned_owl::model::DeclareClass(b.class("http://example.org/Keep")),
        )));
        let mut base = Model::from_parts(base_ont, Default::default());
        base.rdf_prefixes = vec![
            ("efo".to_string(), "http://www.ebi.ac.uk/efo/".to_string()),
            ("efo1".to_string(), "http://www.ebi.ac.uk/efo/#".to_string()),
        ];

        let (out, _) = subtract(base, &HashSet::new());
        assert_eq!(
            out.rdf_prefixes,
            vec![
                ("efo".to_string(), "http://www.ebi.ac.uk/efo/".to_string()),
                ("efo1".to_string(), "http://www.ebi.ac.uk/efo/#".to_string()),
            ],
            "the input's xmlns block is reproduced, not regenerated"
        );
    }
}
