//! `reason` — classify an ontology and assert the inferred axioms.
//!
//! By default this checks coherence (no unsatisfiable classes) and asserts the
//! transitive reduction of inferred SubClassOf axioms. The full option set is
//! available: axiom generators, indirect inference, tautology/owl:Thing/
//! duplicate/external exclusion, equivalent-class policy, new-ontology output,
//! redundant-axiom removal, and unsatisfiable dumping.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    Annotation, AnnotatedComponent, AnnotationValue, ClassAssertion, ClassExpression as CE,
    Component, EquivalentClasses, Individual, Literal, MutableOntology, SubClassOf,
};

use crate::model::Model;
use crate::reason::Reasoner;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// Case-insensitive value parser for the `<bool>`-valued flags.
///
/// Capitalisation of these values varies between ontologies: UBERON's release
/// steps write `--annotate-inferred-axioms False` with a capital F while CL
/// writes it lowercase. `clap`'s built-in bool parser accepts only
/// `true`/`false` verbatim, which would reject the capitalised spelling during
/// argument parsing, before the ontology is ever loaded.
///
/// Anything that is neither spelling is an error rather than a silent `false`:
/// a mistyped `--exclude-owl-thing tru` quietly meaning "off" would turn a
/// requested exclusion into a no-op that nothing reports.
pub fn parse_bool_ci(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("expected 'true' or 'false' (any case), got '{other}'")),
    }
}

#[derive(ClapArgs)]
pub struct Args {
    /// Input ontology path.
    #[arg(short, long)]
    pub input: Option<PathBuf>,

    /// Output ontology path.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format (overrides inference from the output extension).
    #[arg(short, long)]
    pub format: Option<String>,

    /// Reasoner to use. `elk` uses the built-in EL reasoner, and is what CL,
    /// UBERON and MONDO classify their releases with; `structural`/`emr`
    /// likewise. `owlmake` is that same reasoner with sound union-elimination,
    /// which is more complete on disjunctions. `whelk` uses the whelk-rs EL
    /// reasoner. `hermit`/`jfact` use the hermit-rs OWL 2 DL reasoner (full
    /// SROIQ(D), for non-EL inputs).
    #[arg(short = 'r', long, default_value = "elk")]
    pub reasoner: String,

    /// Annotate asserted inferred axioms with `is_inferred true` (`<bool>`).
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub annotate_inferred_axioms: Option<bool>,

    /// Inference types to assert: `SubClass`, `EquivalentClass`,
    /// `ClassAssertion`, … Repeatable / comma-separated. Default: `SubClass`.
    #[arg(short = 'A', long, value_delimiter = ',')]
    pub axiom_generators: Vec<String>,

    /// Assert all (indirect) subsumptions, not just the direct ones (`<bool>`).
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub include_indirect: Option<bool>,

    /// Equivalent-class policy: `all` (allow), `none` (error on any inferred
    /// equivalence) or `asserted-only` (error only on an inferred equivalence
    /// that is not already asserted). `true`/`false` alias `all`/`none`.
    #[arg(short = 'e', long, default_value = "all")]
    pub equivalent_classes_allowed: String,

    /// Output a NEW ontology containing only the inferred axioms (`<bool>`).
    #[arg(short = 'n', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub create_new_ontology: Option<bool>,

    /// Like --create-new-ontology, also copying entity annotations (`<bool>`).
    #[arg(short = 'm', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub create_new_ontology_with_annotations: Option<bool>,

    /// Preserve annotated axioms when removing redundant ones (`<bool>`).
    #[arg(short = 'p', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub preserve_annotated_axioms: Option<bool>,

    /// After asserting, remove redundant SubClassOf axioms (run reduce) (`<bool>`).
    #[arg(short = 's', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub remove_redundant_subclass_axioms: Option<bool>,

    /// Exclude tautologies from output: `structural` or `all`.
    #[arg(short = 't', long)]
    pub exclude_tautologies: Option<String>,

    /// Do not assert subsumptions whose superclass is owl:Thing (`<bool>`).
    #[arg(short = 'T', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub exclude_owl_thing: Option<bool>,

    /// Do not assert an axiom already present in the ontology (`<bool>`, default
    /// false, so an inferred edge can still be annotated).
    #[arg(short = 'x', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub exclude_duplicate_axioms: Option<bool>,

    /// Do not assert axioms whose subject is an external (undeclared) entity (`<bool>`).
    #[arg(short = 'X', long, num_args = 1, default_missing_value = "true", value_parser = parse_bool_ci)]
    pub exclude_external_entities: Option<bool>,

    /// Write the unsatisfiable classes to this file.
    #[arg(short = 'D', long, value_name = "FILE")]
    pub dump_unsatisfiable: Option<PathBuf>,

    /// Do not fail when the ontology is incoherent; report only (owlmake extension).
    #[arg(long)]
    pub allow_incoherent: bool,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// The `--equivalent-classes-allowed` policy.
///
/// The accepted values are `true`, `false`, `all`, `none` and `asserted-only`.
/// `asserted-only` is the value the reasoning QC of OBA, CL and UBERON all
/// choose, so it has to be recognised in its own right: collapsing it into
/// `all` would leave the check passing unconditionally.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EquivMode {
    /// Allow any inferred equivalence (`all`/`true`; the default).
    All,
    /// Fail if the reasoner infers *any* equivalent-class pair (`none`/`false`).
    None,
    /// Fail only on an inferred equivalence that is **not already asserted** in
    /// the input. OBA's three inferred pairs are all asserted in its import
    /// closure, so `none` would break its build while `asserted-only` still
    /// catches a *newly* collapsed pair — the actual logic error.
    AssertedOnly,
}

impl EquivMode {
    /// Parse an `-e/--equivalent-classes-allowed` value, erroring on anything
    /// else. An unrecognised value must not fall through to `all`: that would
    /// silently disable the equivalence check, so a typo would look like a
    /// clean run.
    pub fn parse(s: &str) -> Result<EquivMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "true" => Ok(EquivMode::All),
            "none" | "false" => Ok(EquivMode::None),
            "asserted-only" | "assertedonly" => Ok(EquivMode::AssertedOnly),
            other => bail!(
                "Invalid Equivalent Classes Allowed Error: '{other}' is not a valid \
                 --equivalent-classes-allowed value (must be one of: true, false, all, none, asserted-only)"
            ),
        }
    }
}

/// A `--reasoner` choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReasonerKind {
    /// `elk` — the default, served by the built-in EL engine.
    Elk,
    /// owlmake's own EL engine with sound union-elimination (an extension).
    Owlmake,
    /// `whelk` — the whelk-rs EL engine; CL classifies with it.
    Whelk,
    /// hermit-rs (full OWL 2 DL).
    Hermit,
    /// `jfact` — served by the same OWL 2 DL engine.
    JFact,
    /// `emr` — expression materialization over an EL classification.
    Emr,
    /// `structural` — the told (asserted) class hierarchy, with no reasoning.
    Structural,
}

impl ReasonerKind {
    /// Parse a `--reasoner` name case-insensitively, erroring on anything that
    /// is not one of the accepted names. Falling back to the EL engine with a
    /// `note:` line would let `--reasoner hermit` misspelled as `--reasoner
    /// hermitt` classify in EL and report success on an ontology only a DL
    /// reasoner can refute.
    pub fn parse(name: &str) -> Result<ReasonerKind> {
        match name.trim().to_ascii_lowercase().as_str() {
            "elk" => Ok(ReasonerKind::Elk),
            "owlmake" => Ok(ReasonerKind::Owlmake),
            "whelk" => Ok(ReasonerKind::Whelk),
            "hermit" => Ok(ReasonerKind::Hermit),
            "jfact" => Ok(ReasonerKind::JFact),
            "emr" => Ok(ReasonerKind::Emr),
            "structural" => Ok(ReasonerKind::Structural),
            other => bail!(
                "Invalid Reasoner Error: unknown reasoner '{other}' \
                 (expected one of: ELK, HermiT, JFact, EMR, structural, whelk, owlmake)"
            ),
        }
    }

    /// Whether classification runs on the built-in EL engine, which can take
    /// ownership of the model and free it before saturating.
    fn is_builtin_el(self) -> bool {
        matches!(self, ReasonerKind::Elk | ReasonerKind::Owlmake | ReasonerKind::Emr)
    }
}

/// The `--axiom-generators` names. All fourteen are recognised; the eleven
/// owlmake has no inference for are a hard error, because a name that generated
/// **no** axioms and still exited 0 would turn `reason` into an expensive no-op.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AxiomGenerator {
    SubClass,
    EquivalentClass,
    DisjointClasses,
    ClassAssertion,
    PropertyAssertion,
    EquivalentObjectProperty,
    InverseObjectProperties,
    ObjectPropertyCharacteristic,
    SubObjectProperty,
    ObjectPropertyRange,
    ObjectPropertyDomain,
    EquivalentDataProperties,
    SubDataProperty,
    DataPropertyCharacteristic,
}

impl AxiomGenerator {
    /// Parse one generator name. Matching is case-insensitive and ignores `-`/`_`
    /// so `EquivalentClass`, `equivalent-class` and the plural
    /// `equivalent-classes` all land on the same generator.
    fn parse(name: &str) -> Result<AxiomGenerator> {
        let norm: String = name
            .chars()
            .filter(|c| *c != '-' && *c != '_')
            .flat_map(|c| c.to_lowercase())
            .collect();
        Ok(match norm.as_str() {
            "subclass" | "subclassof" | "subclasses" => AxiomGenerator::SubClass,
            "equivalentclass" | "equivalentclasses" => AxiomGenerator::EquivalentClass,
            "disjointclasses" | "disjointclass" => AxiomGenerator::DisjointClasses,
            "classassertion" | "classassertions" => AxiomGenerator::ClassAssertion,
            "propertyassertion" | "propertyassertions" => AxiomGenerator::PropertyAssertion,
            "equivalentobjectproperty" | "equivalentobjectproperties" => {
                AxiomGenerator::EquivalentObjectProperty
            }
            "inverseobjectproperties" | "inverseobjectproperty" => {
                AxiomGenerator::InverseObjectProperties
            }
            "objectpropertycharacteristic" | "objectpropertycharacteristics" => {
                AxiomGenerator::ObjectPropertyCharacteristic
            }
            "subobjectproperty" | "subobjectproperties" => AxiomGenerator::SubObjectProperty,
            "objectpropertyrange" | "objectpropertyranges" => AxiomGenerator::ObjectPropertyRange,
            "objectpropertydomain" | "objectpropertydomains" => AxiomGenerator::ObjectPropertyDomain,
            "equivalentdataproperties" | "equivalentdataproperty" => {
                AxiomGenerator::EquivalentDataProperties
            }
            "subdataproperty" | "subdataproperties" => AxiomGenerator::SubDataProperty,
            "datapropertycharacteristic" | "datapropertycharacteristics" => {
                AxiomGenerator::DataPropertyCharacteristic
            }
            other => bail!(
                "Invalid Axiom Generator Error: unknown --axiom-generators value '{other}' \
                 (expected one of: SubClass, EquivalentClass, DisjointClasses, ClassAssertion, \
                 PropertyAssertion, EquivalentObjectProperty, InverseObjectProperties, \
                 ObjectPropertyCharacteristic, SubObjectProperty, ObjectPropertyRange, \
                 ObjectPropertyDomain, EquivalentDataProperties, SubDataProperty, \
                 DataPropertyCharacteristic)",
                other = other
            ),
        })
    }

    /// The canonical spelling of a generator, used in the "not implemented" error.
    fn robot_name(self) -> &'static str {
        match self {
            AxiomGenerator::SubClass => "SubClass",
            AxiomGenerator::EquivalentClass => "EquivalentClass",
            AxiomGenerator::DisjointClasses => "DisjointClasses",
            AxiomGenerator::ClassAssertion => "ClassAssertion",
            AxiomGenerator::PropertyAssertion => "PropertyAssertion",
            AxiomGenerator::EquivalentObjectProperty => "EquivalentObjectProperty",
            AxiomGenerator::InverseObjectProperties => "InverseObjectProperties",
            AxiomGenerator::ObjectPropertyCharacteristic => "ObjectPropertyCharacteristic",
            AxiomGenerator::SubObjectProperty => "SubObjectProperty",
            AxiomGenerator::ObjectPropertyRange => "ObjectPropertyRange",
            AxiomGenerator::ObjectPropertyDomain => "ObjectPropertyDomain",
            AxiomGenerator::EquivalentDataProperties => "EquivalentDataProperties",
            AxiomGenerator::SubDataProperty => "SubDataProperty",
            AxiomGenerator::DataPropertyCharacteristic => "DataPropertyCharacteristic",
        }
    }
}

/// Parse and validate `--axiom-generators`; names may be space-separated inside
/// one value as well as comma-separated or repeated. An empty list means the
/// default, `SubClass`.
fn parse_generators(raw: &[String]) -> Result<Vec<AxiomGenerator>> {
    if raw.is_empty() {
        return Ok(vec![AxiomGenerator::SubClass]);
    }
    let mut out = Vec::new();
    for g in raw
        .iter()
        .flat_map(|g| g.split([',', ' ', '\t']))
        .filter(|s| !s.is_empty())
    {
        let gen = AxiomGenerator::parse(g)?;
        if !matches!(
            gen,
            AxiomGenerator::SubClass
                | AxiomGenerator::EquivalentClass
                | AxiomGenerator::ClassAssertion
        ) {
            bail!(
                "--axiom-generators {}: owlmake infers only SubClass, EquivalentClass and \
                 ClassAssertion; refusing to silently emit nothing for '{}'",
                gen.robot_name(),
                gen.robot_name()
            );
        }
        if !out.contains(&gen) {
            out.push(gen);
        }
    }
    Ok(out)
}

/// Validate the `-t/--exclude-tautologies` value. The two real modes are
/// `structural` and `all`; `true` is accepted as a spelling of `structural`
/// because MONDO writes it that way, and the default is `false`.
fn parse_tautologies(raw: Option<&str>) -> Result<TautologyMode> {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("false") => Ok(TautologyMode::Off),
        Some("structural") | Some("true") => Ok(TautologyMode::Structural),
        Some("all") => Ok(TautologyMode::All),
        Some(other) => bail!(
            "Invalid Tautology Error: '{other}' is not a valid --exclude-tautologies value \
             (expected one of: false, true, structural, all)"
        ),
    }
}

/// How `--exclude-tautologies` filters the inferred axioms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TautologyMode {
    /// Keep everything (the default).
    Off,
    /// Structural check only: `X ⊑ X`, `X ⊑ ⊤`, `⊥ ⊑ X`.
    Structural,
    /// Ask whether an EMPTY ontology already entails the candidate axiom, which
    /// also catches the semantic tautologies the structural test cannot see.
    All,
}

/// Options controlling which inferred axioms `reason` asserts. The defaults
/// assert the direct SubClassOf inferences and then drop the asserted subclass
/// edges those inferences make redundant.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReasonOptions {
    pub annotate_inferred_axioms: bool,
    pub allow_incoherent: bool,
    pub axiom_generators: Vec<String>,
    pub include_indirect: bool,
    pub equivalent_classes_allowed: String,
    pub create_new_ontology: bool,
    pub create_new_ontology_with_annotations: bool,
    pub preserve_annotated_axioms: bool,
    pub remove_redundant_subclass_axioms: bool,
    pub exclude_tautologies: Option<String>,
    pub exclude_owl_thing: bool,
    pub exclude_duplicate_axioms: bool,
    pub exclude_external_entities: bool,
    pub dump_unsatisfiable: Option<PathBuf>,
}

impl Default for ReasonOptions {
    fn default() -> Self {
        ReasonOptions {
            annotate_inferred_axioms: false,
            allow_incoherent: false,
            axiom_generators: Vec::new(),
            include_indirect: false,
            equivalent_classes_allowed: "all".to_string(),
            create_new_ontology: false,
            create_new_ontology_with_annotations: false,
            preserve_annotated_axioms: false,
            // Redundant subclass axioms are removed by default.
            remove_redundant_subclass_axioms: true,
            exclude_tautologies: None,
            // False by default: a bare `reason` asserts `X ⊑ owl:Thing` for each
            // root class. The trivial subsumptions are suppressed by any of
            // `--exclude-owl-thing`, `--exclude-duplicate-axioms` or
            // `--exclude-tautologies` (see `exclude_thing` in `reason_with`).
            exclude_owl_thing: false,
            // False by default: an inferred edge is asserted even when already
            // present, so `--annotate-inferred-axioms` can still mark it.
            exclude_duplicate_axioms: false,
            exclude_external_entities: false,
            dump_unsatisfiable: None,
        }
    }
}

impl Args {
    /// Build the validated [`ReasonOptions`]. Every enumerated value
    /// (`--reasoner`, `--equivalent-classes-allowed`, `--axiom-generators`,
    /// `--exclude-tautologies`) is parsed here, so a bad value is an error
    /// *before* the ontology is loaded or classified rather than a silent
    /// fallback discovered (or not) an hour later.
    fn options(&self) -> Result<ReasonOptions> {
        ReasonerKind::parse(&self.reasoner)?;
        EquivMode::parse(&self.equivalent_classes_allowed)?;
        parse_generators(&self.axiom_generators)?;
        parse_tautologies(self.exclude_tautologies.as_deref())?;
        Ok(ReasonOptions {
            annotate_inferred_axioms: self.annotate_inferred_axioms.unwrap_or(false),
            allow_incoherent: self.allow_incoherent,
            axiom_generators: self.axiom_generators.clone(),
            include_indirect: self.include_indirect.unwrap_or(false),
            equivalent_classes_allowed: self.equivalent_classes_allowed.clone(),
            create_new_ontology: self.create_new_ontology.unwrap_or(false),
            create_new_ontology_with_annotations: self
                .create_new_ontology_with_annotations
                .unwrap_or(false),
            preserve_annotated_axioms: self.preserve_annotated_axioms.unwrap_or(false),
            remove_redundant_subclass_axioms: self.remove_redundant_subclass_axioms.unwrap_or(true),
            exclude_tautologies: self.exclude_tautologies.clone(),
            // Default FALSE: a bare `reason` asserts `X ⊑ owl:Thing` for each root
            // class. The trivial subsumptions are suppressed by any of
            // `--exclude-owl-thing`, `--exclude-duplicate-axioms` or
            // `--exclude-tautologies` (see `exclude_thing` in `reason_with`).
            exclude_owl_thing: self.exclude_owl_thing.unwrap_or(false),
            exclude_duplicate_axioms: self.exclude_duplicate_axioms.unwrap_or(false),
            exclude_external_entities: self.exclude_external_entities.unwrap_or(false),
            dump_unsatisfiable: self.dump_unsatisfiable.clone(),
        })
    }
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    // Validate the option values FIRST: loading a multi-gigabyte closure only to
    // reject `--reasoner hermitt` afterwards wastes the whole load.
    let opts = args.options()?;
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut model = reason_with(model, &args.reasoner, &opts)?;
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Convenience entry used by the DOSDP pipeline: classify and assert the
/// inferred direct subsumptions with the default options.
pub fn reason(
    model: Model,
    reasoner: &str,
    annotate_inferred_axioms: bool,
    allow_incoherent: bool,
) -> Result<Model> {
    reason_with(
        model,
        reasoner,
        &ReasonOptions {
            annotate_inferred_axioms,
            allow_incoherent,
            ..Default::default()
        },
    )
}

/// Classify `model` with `reasoner` and assert the inferred axioms selected by
/// `opts`, returning the augmented ontology.
pub fn reason_with(model: Model, reasoner: &str, opts: &ReasonOptions) -> Result<Model> {
    // Every enumerated option is parsed BEFORE any reasoning, so a bad value
    // fails fast for library callers too (the CLI validates in `Args::options`).
    let kind = ReasonerKind::parse(reasoner)?;
    let equiv_mode = EquivMode::parse(&opts.equivalent_classes_allowed)?;
    let generators = parse_generators(&opts.axiom_generators)?;
    let taut_mode = parse_tautologies(opts.exclude_tautologies.as_deref())?;

    let want_subclass = generators.contains(&AxiomGenerator::SubClass);
    let want_equiv = generators.contains(&AxiomGenerator::EquivalentClass);
    let want_class_assertion = generators.contains(&AxiomGenerator::ClassAssertion);
    // The equivalence policy needs the inferred equivalence pairs, NOT the full
    // subsumption closure: each backend computes them directly (O(n·|S(c)|)).
    // `--equivalent-classes-allowed asserted-only` is on essentially every
    // reasoning QC step, so deriving the pairs from `all_subsumptions` would put
    // all of those runs on the O(n·ancestors) full-closure path.
    let need_equiv = want_equiv || equiv_mode != EquivMode::All;
    let need_all = opts.include_indirect;

    // Stash everything the output stage needs from the model UP FRONT, so the
    // (huge) parsed model can be freed before saturation in reasoning-only mode —
    // on phenio that drops ~12 GB held uselessly through the ~60 s saturation.
    let declared = declared_classes(&model);
    let existing = existing_subclass_pairs(&model);
    // `--equivalent-classes-allowed asserted-only` subtracts the equivalences the
    // input ALREADY states, so the asserted set must be captured here, before the
    // model can be handed to `classify_consume`. Normalised in both orders so the
    // lookup is a plain O(1) hit whichever way round the reasoner reports a pair.
    let asserted_equiv = if equiv_mode == EquivMode::AssertedOnly {
        asserted_equivalent_pairs(&model)
    } else {
        HashSet::new()
    };
    let prefixes = crate::model::clone_prefixes(&model.prefixes);
    // Snapshot document metadata (rdf_prefixes/explicit_prefixes/idspaces/…) before
    // the model may be consumed for saturation, so the reasoned result carries it
    // (owlrdf's xmlns + OBO idspaces depend on it downstream in `om make`).
    let meta_src: Model = {
        let mut m = Model::from_parts(
            horned_owl::ontology::set::SetOntology::new(),
            crate::model::clone_prefixes(&model.prefixes),
        );
        m.carry_meta_from(&model);
        m
    };

    // The EL reasoner can release the model before saturating when the model is
    // neither the output (default merge) nor needed for its annotations
    // (`--create-new-ontology-with-annotations`). Whelk/DL/structural keep `&model`
    // — and so does `-D/--dump-unsatisfiable`, which extracts its debug module
    // out of the input ontology after classification.
    let free_model = kind.is_builtin_el()
        && opts.create_new_ontology
        && !opts.create_new_ontology_with_annotations
        && opts.dump_unsatisfiable.is_none();

    let mut model = Some(model);
    let cls = if free_model {
        let union_elim = kind == ReasonerKind::Owlmake;
        if union_elim {
            status!("reason: using the built-in EL reasoner with union-elimination");
        }
        crate::reason::el::set_whelk_mode(union_elim);
        // Hand ownership to the reasoner; it drops the model after normalization.
        let r = Reasoner::classify_consume(model.take().unwrap());
        if r.ignored() > 0 {
            status!("note: {} axiom(s) outside OWL 2 EL were ignored during reasoning", r.ignored());
        }
        Classification {
            consistent: r.is_consistent(),
            unsat: r.unsatisfiable(),
            direct: r.direct_subsumptions(),
            all: if need_all { r.all_subsumptions() } else { Vec::new() },
            equiv: if need_equiv { r.equivalent_class_pairs() } else { Vec::new() },
            class_assertions: if want_class_assertion {
                r.class_assertions()
            } else {
                Vec::new()
            },
        }
    } else {
        classify(
            model.as_ref().unwrap(),
            kind,
            need_all,
            need_equiv,
            want_class_assertion,
        )
    };
    let Classification {
        consistent,
        unsat,
        direct,
        all,
        equiv,
        class_assertions,
    } = cls;

    if !consistent {
        bail!("ontology is inconsistent (owl:Thing is unsatisfiable)");
    }
    if !unsat.is_empty() {
        // A count line followed by one `    unsatisfiable: <IRI>` line each; the
        // wording is fixed because CI jobs grep the log for it. The cap is high
        // because a short silent truncation hides the shape of a
        // mass-unsatisfiability failure — the case where the list is the diagnosis.
        const UNSAT_LIST_CAP: usize = 5000;
        status!("There are {} unsatisfiable classes in the ontology.", unsat.len());
        for u in unsat.iter().take(UNSAT_LIST_CAP) {
            status!("    unsatisfiable: {u}");
        }
        if unsat.len() > UNSAT_LIST_CAP {
            status!("    … and {} more", unsat.len() - UNSAT_LIST_CAP);
        }
        if let Some(path) = &opts.dump_unsatisfiable {
            dump_unsatisfiable_module(model.as_ref(), &unsat, path)?;
        }
        if !opts.allow_incoherent {
            bail!(
                "ontology is incoherent: {} unsatisfiable class(es). Use --allow-incoherent to override.",
                unsat.len()
            );
        }
    }

    // `--equivalent-classes-allowed`: the inferred equivalence pairs come straight
    // from the backend (see `need_equiv` above), never from the full closure.
    let equiv_pairs = equiv;
    let violating: Vec<&(String, String)> = match equiv_mode {
        EquivMode::All => Vec::new(),
        EquivMode::None => equiv_pairs.iter().collect(),
        // `asserted-only` permits an inferred equivalence that the input already
        // asserts, and fails on any other. OBA's three inferred pairs are all
        // asserted in its import closure — treating `asserted-only` as `none`
        // would break its build, which is precisely why its QC picks this value.
        EquivMode::AssertedOnly => equiv_pairs
            .iter()
            .filter(|(a, b)| !asserted_equiv.contains(&(a.clone(), b.clone())))
            .collect(),
    };
    if !violating.is_empty() {
        // Name the offending classes: "N pairs" alone leaves a curator with no
        // way to find the collapse that broke the build.
        for (a, b) in &violating {
            status!("    equivalent: <{a}> == <{b}>");
        }
        bail!(
            "Equivalent Class Axiom Error: {} equivalent class pair(s) were inferred, but \
             --equivalent-classes-allowed is '{}'",
            violating.len(),
            opts.equivalent_classes_allowed
        );
    }

    // Base subsumption set: direct (transitive reduction) or all (indirect).
    // The subclass generator also asserts `X ⊑ owl:Thing` for the top-level
    // classes (every class, under --include-indirect); the exclusion flags below
    // remove those again when requested.
    let mut base: Vec<(String, String)> = if opts.include_indirect {
        all.clone()
    } else {
        direct.clone()
    };
    {
        let mut has_named_super: std::collections::HashSet<String> = direct
            .iter()
            .filter(|(_, sup)| sup.as_str() != OWL_THING)
            .map(|(sub, _)| sub.clone())
            .collect();
        // …and so does a class whose asserted superclass is an ANONYMOUS
        // expression. `BFO_0000002 ⊑ (part_of some BFO_0000001)` puts something
        // between that class and the top, so the root edge is not its to carry:
        // asserting `⊑ owl:Thing` beside it names a parent the class already has
        // by way of the restriction.
        for ac in model.as_ref().map(|m| m.ont.iter()).into_iter().flatten() {
            if let Component::SubClassOf(SubClassOf { sub: CE::Class(sub), sup }) = &ac.component {
                if !matches!(sup, CE::Class(c) if c.0.as_ref() == OWL_THING) {
                    has_named_super.insert(sub.0.as_ref().to_string());
                }
            }
        }
        // Every class in the SIGNATURE, declared or not: a merge can leave an
        // undeclared class referenced by surviving axioms, and its inferred
        // superclass is still asserted — under a bare `reason` that is the
        // trivial `⊑ owl:Thing` root edge.
        let mut sig_classes = declared.clone();
        for ac in model.as_ref().map(|m| m.ont.iter()).into_iter().flatten() {
            for (k, iri) in crate::sig::typed_signature(&ac.component) {
                if k == crate::sig::kind::CLASS {
                    sig_classes.insert(iri);
                }
            }
        }
        for c in &sig_classes {
            if c == OWL_THING || c == OWL_NOTHING {
                continue;
            }
            if opts.include_indirect || !has_named_super.contains(c.as_str()) {
                base.push((c.clone(), OWL_THING.to_string()));
            }
        }
    }
    let base = &base;

    // Tautology / owl:Thing filtering. `structural` and `all` share the cheap
    // structural test (`X ⊑ X`, `X ⊑ ⊤`, `⊥ ⊑ X`); `all` additionally runs the
    // real entailment check below. `--exclude-duplicate-axioms` ALSO suppresses
    // every inferred `X ⊑ owl:Thing`: with any of the three flags set a trivial
    // subsumption is dropped, and only a bare `reason` (UBERON's is
    // `--exclude-duplicate-axioms true`; EFO's passes none of the three and
    // keeps its trivial subsumptions) asserts them.
    let exclude_thing = opts.exclude_owl_thing
        || opts.exclude_duplicate_axioms
        || taut_mode != TautologyMode::Off;
    let exclude_self = taut_mode != TautologyMode::Off;
    // `--exclude-tautologies all` tests every candidate axiom against an EMPTY
    // ontology and drops the ones that empty ontology already entails — catching
    // the semantic tautologies the structural test cannot see (`C ⊑ C ⊔ D`,
    // `C ⊓ D ⊑ C`, `C ⊑ ∃r.⊤ ⊔ ¬∃r.⊤`, …). Built once.
    let taut_checker = (taut_mode == TautologyMode::All).then(|| {
        // One DL consistency check per candidate axiom. No repo's QC asks for
        // this mode (they use `structural`, or `true` in MONDO's case), so
        // announce it rather than let a hand-run command look hung.
        status!("note: --exclude-tautologies all runs a DL entailment check per inferred axiom; this is slow on a large ontology");
        TautologyChecker::new()
    });

    // `declared` (for --exclude-external-entities) and `existing` (asserted
    // SubClassOf pairs, for O(1) dedupe) were stashed up front so the model could
    // be released before saturation.

    // If building a fresh ontology, start from an empty model carrying prefixes.
    let mut target = if opts.create_new_ontology || opts.create_new_ontology_with_annotations {
        let mut fresh = Model::from_parts(horned_owl::ontology::set::SetOntology::new(), prefixes);
        if opts.create_new_ontology_with_annotations {
            // Reached only when `free_model` is false, so the model is still held.
            for ac in model.as_ref().expect("model retained for annotation copy").ont.iter() {
                if matches!(
                    ac.component,
                    Component::AnnotationAssertion(_) | Component::DeclareClass(_)
                ) {
                    fresh.ont.insert(ac.clone());
                }
            }
        }
        fresh
    } else {
        // Default merge: the model itself becomes the output (free_model is false).
        std::mem::replace(model.as_mut().expect("model retained for merge output"), Model::new())
    };

    let infer_prop = target
        .build
        .annotation_property("http://www.geneontology.org/formats/oboInOwl#is_inferred");

    let mut added = 0usize;
    if want_subclass {
        // `owl:Nothing` is part of the taxonomy, so when the ontology MENTIONS it
        // — a general class axiom `… ⊑ owl:Nothing`, say — the bottom node's own
        // reflexive edge is asserted with it. A composite whose input has no such
        // axiom gets no such edge, which is the difference between the metazoan
        // and vertebrate composites.
        let nothing_in_sig = !exclude_self
            && target.ont.iter().any(|ac| match &ac.component {
                Component::SubClassOf(sc) => {
                    matches!(&sc.sup, CE::Class(c) if c.0.as_ref() == OWL_NOTHING)
                        || matches!(&sc.sub, CE::Class(c) if c.0.as_ref() == OWL_NOTHING)
                }
                Component::EquivalentClasses(eq) => eq
                    .0
                    .iter()
                    .any(|m| matches!(m, CE::Class(c) if c.0.as_ref() == OWL_NOTHING)),
                _ => false,
            });
        if nothing_in_sig {
            let ax = Component::SubClassOf(SubClassOf {
                sub: CE::Class(target.build.class(OWL_NOTHING.to_string())),
                sup: CE::Class(target.build.class(OWL_NOTHING.to_string())),
            });
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop) {
                added += 1;
            }
        }
        for (sub, sup) in base {
            if sub == sup && exclude_self {
                continue;
            }
            if sub == sup {
                continue; // X ⊑ X is never asserted
            }
            if sup == OWL_THING && exclude_thing {
                continue;
            }
            if sub == OWL_THING || sub == OWL_NOTHING || sup == OWL_NOTHING {
                continue;
            }
            if opts.exclude_external_entities && !declared.contains(sub) {
                continue;
            }
            if opts.exclude_duplicate_axioms && existing.contains(&(sub.clone(), sup.clone())) {
                continue;
            }
            let ax = Component::SubClassOf(SubClassOf {
                sub: CE::Class(target.build.class(sub.clone())),
                sup: CE::Class(target.build.class(sup.clone())),
            });
            if taut_checker.as_ref().is_some_and(|t| t.is_tautology(&ax)) {
                continue;
            }
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop) {
                added += 1;
            }
        }
    }

    if want_equiv {
        for (a, b) in &equiv_pairs {
            if opts.exclude_external_entities && !declared.contains(a) {
                continue;
            }
            let ax = Component::EquivalentClasses(EquivalentClasses(vec![
                CE::Class(target.build.class(a.clone())),
                CE::Class(target.build.class(b.clone())),
            ]));
            if taut_checker.as_ref().is_some_and(|t| t.is_tautology(&ax)) {
                continue;
            }
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop) {
                added += 1;
            }
        }
    }

    if want_class_assertion {
        let existing_ca = existing_class_assertions(&target);
        for (ind, class) in &class_assertions {
            if class == OWL_THING || class == OWL_NOTHING {
                continue;
            }
            if opts.exclude_external_entities && !declared.contains(class) {
                continue;
            }
            if opts.exclude_duplicate_axioms
                && existing_ca.contains(&(ind.clone(), class.clone()))
            {
                continue;
            }
            let ax = Component::ClassAssertion(ClassAssertion {
                ce: CE::Class(target.build.class(class.clone())),
                i: Individual::Named(target.build.named_individual(ind.clone())),
            });
            if taut_checker.as_ref().is_some_and(|t| t.is_tautology(&ax)) {
                continue;
            }
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop) {
                added += 1;
            }
        }
    }

    status!("reason: asserted {added} inferred axiom(s)");

    // Redundant-subclass removal (on by default): drop an asserted NAMED,
    // unannotated `C ⊑ X` when X is not a *direct* inferred superclass of C.
    // `direct` is the flattened direct-superclass node set — equivalent supers are
    // all emitted, but C's OWN equivalents are excluded — so this removes both
    // transitively-redundant edges (`C ⊑ Y ⊑ X`) and edges to a class *equivalent*
    // to C, which survives as the EquivalentClasses axiom rather than as a
    // subsumption. Anonymous superclasses are left to the explicit `reduce` step;
    // this pass is named-only. Uses the already-computed `direct` set, so it adds
    // no reasoning.
    if opts.remove_redundant_subclass_axioms {
        let direct_set: std::collections::HashSet<(&str, &str)> =
            direct.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let to_remove: Vec<AnnotatedComponent<crate::model::Str>> = target
            .ont
            .iter()
            .filter(|ac| {
                // An ANNOTATED asserted axiom is never removed, whatever
                // `--preserve-annotated-axioms` says: the guard immediately below
                // skips any axiom carrying annotations outright, before the
                // redundancy test is ever reached, so the flag changes nothing
                // here. MONDO leans on this: thousands of its `is_a` edges are
                // asserted, redundant AND annotated —
                // `{source="MONDO:Redundant", …}` records exactly that — and they
                // must all stay.
                if !ac.ann.is_empty() {
                    return false;
                }
                match &ac.component {
                    Component::SubClassOf(sc) => match (&sc.sub, &sc.sup) {
                        (CE::Class(c), CE::Class(x)) => {
                            let (c, x) = (c.0.as_ref(), x.0.as_ref());
                            // A *proper* direct super: `(c, x)` is direct AND the
                            // reverse edge `(x, c)` is absent. The DL backend
                            // reports a class's own equivalence-clique siblings as
                            // direct subsumptions in both directions (deliberately
                            // — the reduction needs them, see tests/dl_reason.rs),
                            // so the reverse-edge test is what separates a genuine
                            // parent from an equivalent class: an asserted
                            // `C ⊑ D` with `C ≡ D` has both directions in the set
                            // and goes. The equivalence then rides on an
                            // EquivalentClasses axiom — one the input asserts, or
                            // one the `EquivalentClass` generator added; under the
                            // default generators nothing replaces the dropped
                            // edge. The EL backend already omits those pairs, so
                            // this changes nothing there.
                            let proper_direct = direct_set.contains(&(c, x))
                                && !direct_set.contains(&(x, c));
                            // A self-subsumption goes too: a class is never among
                            // its own direct superclasses, so an asserted `C ⊑ C`
                            // is never in the inferred set and is removed. EFO
                            // asserts two of them by hand; exempting `c == x` here
                            // would leave both in the released `efo.owl`.
                            x != OWL_THING && x != OWL_NOTHING && !proper_direct
                        }
                        _ => false,
                    },
                    _ => false,
                }
            })
            .cloned()
            .collect();
        for ac in to_remove {
            target.ont.remove(&ac);
        }
    }

    target.carry_meta_from(&meta_src);
    Ok(target)
}

/// Result of running a reasoner, normalized across the EL/whelk/DL/structural
/// backends.
struct Classification {
    consistent: bool,
    unsat: Vec<String>,
    direct: Vec<(String, String)>,
    all: Vec<(String, String)>,
    /// Inferred equivalent-class pairs (`a < b` by IRI). Only populated when the
    /// `EquivalentClass` generator or a non-`all` equivalence policy needs them —
    /// never derived from `all`, which is the full O(n·ancestors) closure.
    equiv: Vec<(String, String)>,
    /// Inferred direct class assertions (individual_iri, class_iri), only the
    /// `class-assertion` generator populates this.
    class_assertions: Vec<(String, String)>,
}

fn classify(
    model: &Model,
    kind: ReasonerKind,
    need_all: bool,
    need_equiv: bool,
    need_class_assertions: bool,
) -> Classification {
    match kind {
        // hermit-rs (DL) and whelk-rs (EL) both build for wasm, so `hermit`/
        // `jfact`/`whelk` all work in the browser too (see src/reason/mod.rs).
        ReasonerKind::Hermit | ReasonerKind::JFact => {
            status!("reason: using hermit-rs, the HermiT OWL 2 DL reasoner ('{kind:?}')");
            let r = crate::reason::DlReasoner::classify(model);
            let direct = r.direct_subsumptions();
            let all = if need_all { r.all_subsumptions() } else { Vec::new() };
            if need_class_assertions {
                status!("note: the 'class-assertion' generator needs the EL reasoner; no inferred class assertions from '{kind:?}'");
            }
            Classification {
                consistent: r.is_consistent(),
                unsat: r.unsatisfiable(),
                direct,
                all,
                equiv: if need_equiv { r.equivalent_class_pairs() } else { Vec::new() },
                class_assertions: Vec::new(),
            }
        }
        ReasonerKind::Whelk => {
            status!("reason: using the whelk-rs EL reasoner");
            let r = crate::reason::WhelkClassification::classify(model);
            let direct = r.direct_subsumptions();
            if need_class_assertions {
                status!("note: the 'class-assertion' generator needs the built-in EL reasoner; no inferred class assertions from whelk");
            }
            Classification {
                consistent: r.is_consistent(),
                unsat: r.unsatisfiable(),
                // Read the saturated closure, NOT `direct`: the direct list drops
                // equivalence-clique siblings, so aliasing `all` to it would make
                // every equivalence invisible to `--equivalent-classes-allowed`
                // under `--reasoner whelk`, the reasoner CL classifies with.
                all: if need_all { r.all_subsumptions() } else { Vec::new() },
                equiv: if need_equiv { r.equivalent_class_pairs() } else { Vec::new() },
                direct,
                class_assertions: Vec::new(),
            }
        }
        // `structural`: the TOLD hierarchy, no reasoning at all.
        ReasonerKind::Structural => classify_structural(model, need_all, need_equiv),
        ReasonerKind::Elk | ReasonerKind::Owlmake | ReasonerKind::Emr => {
            let union_elim = kind == ReasonerKind::Owlmake;
            if union_elim {
                status!("reason: using the built-in EL reasoner with union-elimination");
            } else if kind == ReasonerKind::Emr {
                // `emr` adds materialised `∃r.C` superclasses on top of an EL
                // classification, but classifies named subsumption exactly as the
                // EL engine does, and `reason` only ever reads the named
                // hierarchy. So the EL result is the whole answer here — the extra
                // expressions belong to `om materialize`.
                status!("note: reasoner 'emr' wraps ELK; classifying with the built-in EL reasoner (use `om materialize` for the ∃-expression closure)");
            }
            crate::reason::el::set_whelk_mode(union_elim);
            let r = Reasoner::classify(model);
            if r.ignored() > 0 {
                status!("note: {} axiom(s) outside OWL 2 EL were ignored during reasoning", r.ignored());
            }
            // `all_subsumptions` is the full transitive closure — O(n·avg-supers)
            // entries, which is enormous on phenio-scale inputs; only build it when
            // indirect output actually needs it (the equivalence policy reads
            // `equivalent_class_pairs`, which scans the S-sets without a closure).
            Classification {
                consistent: r.is_consistent(),
                unsat: r.unsatisfiable(),
                direct: r.direct_subsumptions(),
                all: if need_all { r.all_subsumptions() } else { Vec::new() },
                equiv: if need_equiv { r.equivalent_class_pairs() } else { Vec::new() },
                class_assertions: if need_class_assertions {
                    r.class_assertions()
                } else {
                    Vec::new()
                },
            }
        }
    }
}

/// `--reasoner structural` — the told class hierarchy.
///
/// This is a *told* hierarchy, not a reasoner: it is the transitive closure of
/// the asserted named `SubClassOf` and `EquivalentClasses` edges, with no
/// normalisation, no ∃-role reasoning and no satisfiability testing at all: no
/// class is ever reported unsatisfiable — `owl:Nothing` included — and the
/// ontology is always consistent. It is a legal `--reasoner` value, and a repo
/// may well be configured with it, so it has to stay this weak rather than
/// quietly running the full EL engine — which would report inferences a told
/// hierarchy does not make, and unsatisfiable classes it can never find.
fn classify_structural(model: &Model, need_all: bool, need_equiv: bool) -> Classification {
    status!("reason: using the structural reasoner (told class hierarchy)");
    // Told edges: asserted named `C ⊑ D`, plus both directions of every asserted
    // named `C ≡ D`. Nothing else contributes an edge — an anonymous superclass
    // is simply not a told parent.
    let mut told: HashMap<&str, Vec<&str>> = HashMap::new();
    for (sub, sup) in existing_named_subclass_edges(model) {
        told.entry(sub).or_default().push(sup);
    }
    for ac in model.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            let named: Vec<&str> = eq
                .0
                .iter()
                .filter_map(|ce| match ce {
                    CE::Class(c) => Some(c.0.as_ref()),
                    _ => None,
                })
                .collect();
            for &a in &named {
                for &b in &named {
                    if a != b {
                        told.entry(a).or_default().push(b);
                    }
                }
            }
        }
    }

    // Transitive closure, one BFS per class over the told graph. No reduction
    // shortcuts: the told graph is walked exactly as asserted, cycles and all.
    let mut closure: HashMap<&str, HashSet<&str>> = HashMap::new();
    for &start in told.keys() {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = told.get(start).cloned().unwrap_or_default();
        while let Some(c) = stack.pop() {
            if c == start || c == OWL_THING || c == OWL_NOTHING || !seen.insert(c) {
                continue;
            }
            if let Some(next) = told.get(c) {
                stack.extend(next.iter().copied());
            }
        }
        closure.insert(start, seen);
    }
    let sub_of = |a: &str, b: &str| closure.get(a).is_some_and(|s| s.contains(b));

    let mut all: Vec<(String, String)> = Vec::new();
    let mut equiv: Vec<(String, String)> = Vec::new();
    let mut direct: Vec<(String, String)> = Vec::new();
    for (&c, sups) in &closure {
        if c == OWL_THING || c == OWL_NOTHING {
            continue;
        }
        for &d in sups {
            if need_all {
                all.push((c.to_string(), d.to_string()));
            }
            if need_equiv && c < d && sub_of(d, c) {
                equiv.push((c.to_string(), d.to_string()));
            }
        }
        // Transitive reduction, matching the EL backend's `direct_subsumptions`:
        // clique siblings of `c` are related by an equivalence, not an edge, and
        // `d` is dropped when some other super lies strictly between.
        let proper: Vec<&str> = sups.iter().copied().filter(|&d| !sub_of(d, c)).collect();
        for &d in &proper {
            let redundant = proper
                .iter()
                .any(|&mid| mid != d && sub_of(mid, d) && !sub_of(d, mid));
            if !redundant {
                direct.push((c.to_string(), d.to_string()));
            }
        }
    }
    all.sort();
    all.dedup();
    equiv.sort();
    equiv.dedup();
    direct.sort();
    direct.dedup();
    Classification {
        // A told hierarchy has no satisfiability test: it never reports an
        // inconsistency and never reports an unsatisfiable class.
        consistent: true,
        unsat: Vec::new(),
        direct,
        all,
        equiv,
        class_assertions: Vec::new(),
    }
}

/// Insert an axiom, optionally annotated with `is_inferred true`. Returns whether
/// it was newly added.
fn insert_axiom(
    model: &mut Model,
    component: Component<crate::model::Str>,
    annotate: bool,
    infer_prop: &horned_owl::model::AnnotationProperty<crate::model::Str>,
) -> bool {
    if annotate {
        let ann = Annotation { ann: Default::default(),
            ap: infer_prop.clone(),
            av: AnnotationValue::Literal(Literal::Simple {
                literal: "true".to_string(),
            }),
        };
        let mut anns = std::collections::BTreeSet::new();
        anns.insert(ann);
        model.ont.insert(AnnotatedComponent {
            component,
            ann: anns,
        })
    } else {
        model.ont.insert(component)
    }
}

fn existing_subclass_pairs(model: &Model) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(a), CE::Class(b)) = (&sc.sub, &sc.sup) {
                out.insert((a.0.as_ref().to_string(), b.0.as_ref().to_string()));
            }
        }
    }
    out
}

/// The asserted named `SubClassOf` edges, borrowed from the model (no
/// allocation) — the told graph the structural reasoner closes over.
fn existing_named_subclass_edges(model: &Model) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(a), CE::Class(b)) = (&sc.sub, &sc.sup) {
                out.push((a.0.as_ref(), b.0.as_ref()));
            }
        }
    }
    out
}

/// Every equivalence between two NAMED classes the input already asserts,
/// normalised into **both** orders so `--equivalent-classes-allowed
/// asserted-only` can subtract them with one O(1) lookup however the reasoner
/// happened to order the inferred pair.
///
/// `EquivalentClasses` in OWL is an n-ary axiom, so `EquivalentClasses(A B C)`
/// asserts all three pairs; anonymous members (the genus-differentia definitions
/// that make up most of an OBO edit file) contribute nothing here.
fn asserted_equivalent_pairs(model: &Model) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::EquivalentClasses(eq) = &ac.component {
            let named: Vec<&str> = eq
                .0
                .iter()
                .filter_map(|ce| match ce {
                    CE::Class(c) => Some(c.0.as_ref()),
                    _ => None,
                })
                .collect();
            for &a in &named {
                for &b in &named {
                    if a != b {
                        out.insert((a.to_string(), b.to_string()));
                    }
                }
            }
        }
    }
    out
}

/// The `--exclude-tautologies all` checker: an EMPTY ontology as the premise,
/// asked whether it entails each candidate axiom.
///
/// [`crate::reason::entails`] reduces each conclusion axiom to a consistency test
/// against the premise; with an empty premise, "entailed" means "true in every
/// interpretation", i.e. a tautology. Every inferred axiom that passes the test
/// is dropped from the output.
struct TautologyChecker {
    empty: Model,
}

impl TautologyChecker {
    fn new() -> TautologyChecker {
        TautologyChecker { empty: Model::new() }
    }

    fn is_tautology(&self, component: &Component<crate::model::Str>) -> bool {
        let mut ont = horned_owl::ontology::set::SetOntology::new();
        ont.insert(component.clone());
        let conclusion =
            Model::from_parts(ont, crate::model::clone_prefixes(&self.empty.prefixes));
        crate::reason::entails(&self.empty, &conclusion)
    }
}

/// `-D/--dump-unsatisfiable`: write an **extracted debug module** for the
/// unsatisfiable classes, not a list of IRIs.
///
/// The dump seeds a STAR (⊥⊤*) module with the unsatisfiable classes and saves
/// it, so the file can be opened in Protégé and the contradiction traced.
/// A newline-separated IRI list cannot be loaded by anything and answers none of
/// the questions the dump exists to answer. Falls
/// back to the IRI list only if the model has already been released.
fn dump_unsatisfiable_module(
    model: Option<&Model>,
    unsat: &[String],
    path: &std::path::Path,
) -> Result<()> {
    let Some(model) = model else {
        std::fs::write(path, format!("{}\n", unsat.join("\n")))?;
        return Ok(());
    };
    let seed: HashSet<String> = unsat.iter().cloned().collect();
    let mut module = crate::extract::extract(model, &seed, crate::extract::Method::Star);
    // The format comes from the path extension; default to RDF/XML when the
    // extension says nothing, rather than failing the dump.
    let fmt = crate::cmd::resolve_format(None, path).unwrap_or(crate::io::Format::RdfXml);
    crate::io::save_as(&mut module, path, fmt)?;
    status!(
        "reason: wrote the unsatisfiable-class module ({} seed class(es), {} axioms) to {}",
        unsat.len(),
        module.ont.iter().count(),
        path.display()
    );
    Ok(())
}

fn existing_class_assertions(model: &Model) -> std::collections::HashSet<(String, String)> {
    let mut out = std::collections::HashSet::new();
    for ac in model.ont.iter() {
        if let Component::ClassAssertion(ca) = &ac.component {
            if let (CE::Class(c), Individual::Named(i)) = (&ca.ce, &ca.i) {
                out.insert((i.0.as_ref().to_string(), c.0.as_ref().to_string()));
            }
        }
    }
    out
}

fn declared_classes(model: &Model) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for ac in model.ont.iter() {
        if let Component::DeclareClass(dc) = &ac.component {
            out.insert(dc.0 .0.as_ref().to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::{Build, DeclareClass, DisjointClasses};
    use horned_owl::ontology::set::SetOntology;

    const NS: &str = "http://example.org/";

    fn model_of(comps: Vec<Component<crate::model::Str>>) -> Model {
        let mut ont: SetOntology<crate::model::Str> = SetOntology::new();
        for c in comps {
            ont.insert(c);
        }
        Model::from_parts(ont, crate::model::default_prefixes())
    }

    fn cls(b: &Build<crate::model::Str>, n: &str) -> CE<crate::model::Str> {
        CE::Class(b.class(format!("{NS}{n}")))
    }

    fn sub(b: &Build<crate::model::Str>, x: &str, y: &str) -> Component<crate::model::Str> {
        Component::SubClassOf(SubClassOf { sub: cls(b, x), sup: cls(b, y) })
    }

    fn decl(b: &Build<crate::model::Str>, n: &str) -> Component<crate::model::Str> {
        Component::DeclareClass(DeclareClass(b.class(format!("{NS}{n}"))))
    }

    fn equiv(b: &Build<crate::model::Str>, x: &str, y: &str) -> Component<crate::model::Str> {
        Component::EquivalentClasses(EquivalentClasses(vec![cls(b, x), cls(b, y)]))
    }

    fn opts_with(equiv_mode: &str) -> ReasonOptions {
        ReasonOptions {
            equivalent_classes_allowed: equiv_mode.to_string(),
            ..Default::default()
        }
    }

    /// `A ⊑ B` and `B ⊑ A`, so `A ≡ B` is INFERRED but never asserted.
    fn inferred_only_equivalence(b: &Build<crate::model::Str>) -> Vec<Component<crate::model::Str>> {
        vec![decl(b, "A"), decl(b, "B"), sub(b, "A", "B"), sub(b, "B", "A")]
    }

    // --- case-insensitive <bool> -----------------------------------------

    #[test]
    fn bool_values_are_case_insensitive() {
        // UBERON writes `--annotate-inferred-axioms False`, with a capital F.
        assert_eq!(parse_bool_ci("False"), Ok(false));
        assert_eq!(parse_bool_ci("FALSE"), Ok(false));
        assert_eq!(parse_bool_ci("True"), Ok(true));
        assert_eq!(parse_bool_ci(" true "), Ok(true));
        assert!(parse_bool_ci("flase").is_err());
    }

    // --- --equivalent-classes-allowed ------------------------------------

    #[test]
    fn equiv_mode_parses_every_robot_value_and_rejects_the_rest() {
        assert_eq!(EquivMode::parse("all").unwrap(), EquivMode::All);
        assert_eq!(EquivMode::parse("TRUE").unwrap(), EquivMode::All);
        assert_eq!(EquivMode::parse("none").unwrap(), EquivMode::None);
        assert_eq!(EquivMode::parse("false").unwrap(), EquivMode::None);
        assert_eq!(EquivMode::parse("asserted-only").unwrap(), EquivMode::AssertedOnly);
        assert_eq!(EquivMode::parse(" Asserted-Only ").unwrap(), EquivMode::AssertedOnly);
        // The whole point: a typo must NOT degrade to `all`.
        let err = EquivMode::parse("asserted_only").unwrap_err().to_string();
        assert!(err.contains("Invalid Equivalent Classes Allowed Error"), "{err}");
    }

    #[test]
    fn asserted_only_fails_on_a_newly_inferred_equivalence() {
        let b = Build::new_rc();
        let err = reason_with(
            model_of(inferred_only_equivalence(&b)),
            "elk",
            &opts_with("asserted-only"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Equivalent Class Axiom Error"), "{err}");
    }

    #[test]
    fn asserted_only_permits_an_equivalence_the_input_already_states() {
        // OBA's three inferred pairs are all asserted in its import closure —
        // exactly why its reasoning QC asks for `asserted-only` over `none`.
        let b = Build::new_rc();
        let mut comps = inferred_only_equivalence(&b);
        comps.push(equiv(&b, "A", "B"));
        assert!(reason_with(model_of(comps.clone()), "elk", &opts_with("asserted-only")).is_ok());
        // …while `none` still rejects it, and `all` accepts anything.
        assert!(reason_with(model_of(comps.clone()), "elk", &opts_with("none")).is_err());
        assert!(reason_with(model_of(comps), "elk", &opts_with("all")).is_ok());
    }

    #[test]
    fn asserted_pairs_are_normalised_in_both_orders() {
        let b = Build::new_rc();
        // `EquivalentClasses` is n-ary: A≡B≡C asserts all three pairs.
        let m = model_of(vec![Component::EquivalentClasses(EquivalentClasses(vec![
            cls(&b, "A"),
            cls(&b, "B"),
            cls(&b, "C"),
        ]))]);
        let pairs = asserted_equivalent_pairs(&m);
        for (x, y) in [("A", "B"), ("B", "A"), ("A", "C"), ("C", "B")] {
            assert!(
                pairs.contains(&(format!("{NS}{x}"), format!("{NS}{y}"))),
                "missing {x}/{y}"
            );
        }
    }

    // --- the whelk backend must be able to see an equivalence ------------

    #[test]
    fn whelk_reports_inferred_equivalences_to_the_policy() {
        // CL classifies with `--reasoner whelk`. The whelk backend's "all
        // subsumptions" must not be the DIRECT list, which filters equivalence
        // pairs out and leaves the policy unable to fire.
        let b = Build::new_rc();
        let err = reason_with(
            model_of(inferred_only_equivalence(&b)),
            "whelk",
            &opts_with("asserted-only"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Equivalent Class Axiom Error"), "{err}");

        let mut comps = inferred_only_equivalence(&b);
        comps.push(equiv(&b, "A", "B"));
        assert!(reason_with(model_of(comps), "whelk", &opts_with("asserted-only")).is_ok());
    }

    // --- --reasoner validation -------------------------------------------

    #[test]
    fn reasoner_names_are_validated() {
        for name in ["elk", "ELK", "HermiT", "jfact", "whelk", "EMR", "Structural", "owlmake"] {
            assert!(ReasonerKind::parse(name).is_ok(), "{name} should parse");
        }
        let err = ReasonerKind::parse("Hermitt").unwrap_err().to_string();
        assert!(err.contains("Invalid Reasoner Error"), "{err}");
        // …and the error surfaces from `reason_with` before any classification.
        let b = Build::new_rc();
        assert!(reason_with(model_of(vec![decl(&b, "A")]), "elkk", &ReasonOptions::default()).is_err());
    }

    // --- --axiom-generators ----------------------------------------------

    #[test]
    fn axiom_generators_are_mapped_and_unimplemented_ones_refused() {
        assert_eq!(parse_generators(&[]).unwrap(), vec![AxiomGenerator::SubClass]);
        assert_eq!(
            parse_generators(&["SubClass EquivalentClass".to_string()]).unwrap(),
            vec![AxiomGenerator::SubClass, AxiomGenerator::EquivalentClass]
        );
        // The hyphenated and plural spellings resolve to the same generators.
        assert_eq!(
            parse_generators(&["equivalent-classes".to_string(), "class-assertion".to_string()])
                .unwrap(),
            vec![AxiomGenerator::EquivalentClass, AxiomGenerator::ClassAssertion]
        );
        // A recognised name owlmake cannot infer is an error, NOT an empty
        // inference set with exit 0.
        let err = parse_generators(&["DisjointClasses".to_string()]).unwrap_err().to_string();
        assert!(err.contains("DisjointClasses"), "{err}");
        // An unknown name is an error too.
        let err = parse_generators(&["SubClassy".to_string()]).unwrap_err().to_string();
        assert!(err.contains("Invalid Axiom Generator Error"), "{err}");
    }

    // --- --reasoner structural -------------------------------------------

    #[test]
    fn structural_reasoner_is_the_told_hierarchy_only() {
        // `A ⊑ ∃r.X` with `∃r.X ⊑ D` entails `A ⊑ D` in EL, but the structural
        // backend does no reasoning, so it must not appear.
        let b = Build::new_rc();
        let r = b.object_property(format!("{NS}r"));
        let some = CE::ObjectSomeValuesFrom {
            ope: horned_owl::model::ObjectPropertyExpression::ObjectProperty(r),
            bce: Box::new(cls(&b, "X")),
        };
        let m = model_of(vec![
            decl(&b, "A"),
            decl(&b, "B"),
            decl(&b, "C"),
            decl(&b, "D"),
            sub(&b, "A", "B"),
            sub(&b, "B", "C"),
            Component::SubClassOf(SubClassOf { sub: cls(&b, "A"), sup: some.clone() }),
            Component::SubClassOf(SubClassOf { sub: some, sup: cls(&b, "D") }),
        ]);
        let c = classify_structural(&m, true, true);
        let has = |sub: &str, sup: &str| {
            c.all.contains(&(format!("{NS}{sub}"), format!("{NS}{sup}")))
        };
        assert!(has("A", "B"));
        assert!(has("A", "C"), "told edges are closed transitively");
        assert!(!has("A", "D"), "structural must not do ∃-reasoning");
        // Transitive reduction keeps only the immediate told parent.
        assert!(c.direct.contains(&(format!("{NS}A"), format!("{NS}B"))));
        assert!(!c.direct.contains(&(format!("{NS}A"), format!("{NS}C"))));
        // It never reports unsatisfiability or inconsistency.
        assert!(c.consistent && c.unsat.is_empty());
    }

    #[test]
    fn structural_reasoner_sees_asserted_equivalences() {
        let b = Build::new_rc();
        let m = model_of(vec![decl(&b, "A"), decl(&b, "B"), equiv(&b, "A", "B")]);
        let c = classify_structural(&m, false, true);
        assert_eq!(c.equiv, vec![(format!("{NS}A"), format!("{NS}B"))]);
    }

    // --- --exclude-tautologies all -----------------------------------------

    #[test]
    fn tautology_mode_parses() {
        assert_eq!(parse_tautologies(None).unwrap(), TautologyMode::Off);
        assert_eq!(parse_tautologies(Some("false")).unwrap(), TautologyMode::Off);
        // MONDO writes `--exclude-tautologies true`.
        assert_eq!(parse_tautologies(Some("true")).unwrap(), TautologyMode::Structural);
        assert_eq!(parse_tautologies(Some("Structural")).unwrap(), TautologyMode::Structural);
        assert_eq!(parse_tautologies(Some("ALL")).unwrap(), TautologyMode::All);
        assert!(parse_tautologies(Some("structrual")).is_err());
    }

    #[test]
    fn tautology_all_uses_real_entailment() {
        let b = Build::new_rc();
        let t = TautologyChecker::new();
        // Entailed by the EMPTY ontology, so a tautology…
        assert!(t.is_tautology(&Component::SubClassOf(SubClassOf {
            sub: cls(&b, "A"),
            sup: cls(&b, "A"),
        })));
        assert!(t.is_tautology(&Component::SubClassOf(SubClassOf {
            sub: CE::ObjectIntersectionOf(vec![cls(&b, "A"), cls(&b, "B")]),
            sup: cls(&b, "A"),
        })));
        // …and this one is not.
        assert!(!t.is_tautology(&Component::SubClassOf(SubClassOf {
            sub: cls(&b, "A"),
            sup: cls(&b, "B"),
        })));
    }

    // --- -D writes an extracted module -------------------------------------

    #[test]
    fn dump_unsatisfiable_writes_a_loadable_module() {
        // `A ⊑ B`, `A ⊑ C`, `B` and `C` disjoint ⟹ `A` is unsatisfiable.
        let b = Build::new_rc();
        let m = model_of(vec![
            decl(&b, "A"),
            decl(&b, "B"),
            decl(&b, "C"),
            sub(&b, "A", "B"),
            sub(&b, "A", "C"),
            Component::DisjointClasses(DisjointClasses(vec![cls(&b, "B"), cls(&b, "C")])),
        ]);
        let path = std::env::temp_dir().join(format!("om-unsat-{}.ofn", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let opts = ReasonOptions {
            allow_incoherent: true,
            dump_unsatisfiable: Some(path.clone()),
            ..Default::default()
        };
        reason_with(m, "elk", &opts).expect("allow_incoherent");
        // The dump is an `extract`ed debug module, so it must parse as an
        // ontology and mention the unsatisfiable class — not be a bare IRI list.
        let dumped = crate::io::load(&path).expect("the dump is a loadable ontology");
        assert!(dumped.ont.iter().count() > 0);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(&format!("{NS}A")), "{text}");
        let _ = std::fs::remove_file(&path);
    }
}
