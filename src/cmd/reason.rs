//! `reason` — classify an ontology and assert the inferred axioms.
//!
//! By default this checks coherence (no unsatisfiable classes) and asserts the
//! transitive reduction of inferred SubClassOf axioms. The full option set is
//! available: axiom generators (subsumptions, equivalences, class assertions
//! and object property assertions), indirect inference, tautology/owl:Thing/
//! duplicate/external exclusion, equivalent-class policy, new-ontology output,
//! redundant-axiom removal, and unsatisfiable dumping.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    Annotation, AnnotatedComponent, AnnotationValue, ClassAssertion, ClassExpression as CE,
    Component, EquivalentClasses, Individual, Literal, MutableOntology, ObjectPropertyAssertion,
    ObjectPropertyExpression as OPE, SubClassOf,
};

use crate::model::Model;
use crate::reason::{Datatypes, Reasoner, Rules};

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const OWL_TOP_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#topObjectProperty";
const OWL_BOTTOM_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#bottomObjectProperty";

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

    /// Annotate asserted inferred axioms with `is_inferred true` (`true` or
    /// `yes` in any case).
    #[arg(short = 'a', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub annotate_inferred_axioms: Option<bool>,

    /// Inference types to assert: `SubClass`, `EquivalentClass`,
    /// `ClassAssertion`, `PropertyAssertion`. Repeatable / comma-separated.
    /// Default: `SubClass`.
    #[arg(short = 'A', long, value_delimiter = ',')]
    pub axiom_generators: Vec<String>,

    /// The object properties whose inferred assertions the `PropertyAssertion`
    /// generator asserts, as IRIs or CURIEs. Repeatable / comma-separated.
    /// Default: every named object property in the signature.
    #[arg(long, value_delimiter = ',')]
    pub properties: Vec<String>,

    /// Assert all (indirect) subsumptions, not just the direct ones (`true` or
    /// `yes` in any case).
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub include_indirect: Option<bool>,

    /// Equivalent-class policy: `all` (allow), `none` (error on any inferred
    /// equivalence) or `asserted-only` (error only on an inferred equivalence
    /// that is not already asserted). `true`/`false` alias `all`/`none`.
    #[arg(short = 'e', long, default_value = "all")]
    pub equivalent_classes_allowed: String,

    /// Output a NEW ontology containing only the inferred axioms (`true` or
    /// `yes` in any case).
    #[arg(short = 'n', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub create_new_ontology: Option<bool>,

    /// Like --create-new-ontology, also copying entity annotations (`true` or
    /// `yes` in any case).
    #[arg(short = 'm', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub create_new_ontology_with_annotations: Option<bool>,

    /// Preserve annotated axioms when removing redundant ones (`true` or `yes`
    /// in any case).
    #[arg(short = 'p', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub preserve_annotated_axioms: Option<bool>,

    /// After asserting, remove redundant SubClassOf axioms (run reduce)
    /// (`true` or `yes` in any case).
    #[arg(short = 's', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub remove_redundant_subclass_axioms: Option<bool>,

    /// Exclude tautologies from output: `structural` or `all`.
    #[arg(short = 't', long)]
    pub exclude_tautologies: Option<String>,

    /// Do not assert subsumptions whose superclass is owl:Thing (`true` or
    /// `yes` in any case).
    #[arg(short = 'T', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub exclude_owl_thing: Option<bool>,

    /// Do not assert an axiom already present in the ontology (`true` or `yes`
    /// in any case; default false, so an inferred edge can still be annotated).
    #[arg(short = 'x', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub exclude_duplicate_axioms: Option<bool>,

    /// Do not assert axioms whose subject is an external (undeclared) entity
    /// (`true` or `yes` in any case).
    #[arg(short = 'X', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
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
    /// Parse a `--reasoner` name, trimmed and case-insensitively, for a
    /// command that classifies with it directly (`reason`, `explain`), erroring
    /// on anything that is not one of the accepted names. Falling back to the
    /// EL engine with a `note:` line would let `--reasoner hermit` misspelled as
    /// `--reasoner hermitt` classify in EL and report success on an ontology
    /// only a DL reasoner can refute.
    pub fn parse(name: &str) -> Result<ReasonerKind> {
        let name = name.trim().to_lowercase();
        match name.as_str() {
            "elk" => Ok(ReasonerKind::Elk),
            "owlmake" => Ok(ReasonerKind::Owlmake),
            "whelk" => Ok(ReasonerKind::Whelk),
            "hermit" => Ok(ReasonerKind::Hermit),
            "jfact" => Ok(ReasonerKind::JFact),
            "emr" => Ok(ReasonerKind::Emr),
            "structural" => Ok(ReasonerKind::Structural),
            _ => bail!("INVALID REASONER ERROR unknown reasoner: {name}"),
        }
    }

    /// Parse a `--reasoner` name for a command that takes a reasoner but is no
    /// classification itself (`materialize`, `reduce`): `emr` is not one of
    /// its names.
    pub fn parse_without_emr(name: &str) -> Result<ReasonerKind> {
        match ReasonerKind::parse(name)? {
            ReasonerKind::Emr => bail!("INVALID REASONER ERROR unknown reasoner: emr"),
            kind => Ok(kind),
        }
    }

    /// How this reasoner finds the unsatisfiable object properties. With
    /// `materializing`, it is wrapped for expression materialization, as
    /// `emr` is the EL reasoner wrapped: the wrapper's property hierarchy is
    /// the wrapped reasoner's, which for the EL reasoner is the told one.
    pub(crate) fn property_check(self, materializing: bool) -> PropertyCheck {
        match self {
            ReasonerKind::Elk | ReasonerKind::Owlmake if !materializing => PropertyCheck::Probe,
            ReasonerKind::Whelk => PropertyCheck::Probe,
            ReasonerKind::Elk | ReasonerKind::Owlmake | ReasonerKind::Emr | ReasonerKind::JFact => {
                PropertyCheck::Told
            }
            ReasonerKind::Hermit => PropertyCheck::BottomNode,
            ReasonerKind::Structural => PropertyCheck::None,
        }
    }

    /// What the reasoner does with a datatype outside the OWL 2 datatype map.
    /// HermiT refuses one when it is wrapped for expression materialization, and
    /// reads past it everywhere else, as its reasoner factory does; JFact always
    /// reads past it. The other reasoners read no datatypes.
    pub(crate) fn datatypes(self, materializing: bool) -> Datatypes {
        if materializing && self == ReasonerKind::Hermit {
            Datatypes::Strict
        } else {
            Datatypes::Lenient
        }
    }

    /// What the reasoner does with a SWRL rule: `jfact` reads past every
    /// rule, and `hermit` takes each as DL-safe.
    pub(crate) fn rules(self) -> Rules {
        if self == ReasonerKind::JFact {
            Rules::Ignored
        } else {
            Rules::DlSafe
        }
    }

    /// The DL reasoner this kind names over `model`, reading datatypes as it
    /// does everywhere but materialization, and rules as it does.
    pub(crate) fn dl_reasoner(self, model: &Model) -> crate::reason::DlReasoner {
        crate::reason::DlReasoner::classify_with(model, self.datatypes(false), self.rules())
    }

    /// Whether classification runs on the built-in EL engine, which can take
    /// ownership of the model and free it before saturating.
    pub(crate) fn is_builtin_el(self) -> bool {
        matches!(self, ReasonerKind::Elk | ReasonerKind::Owlmake | ReasonerKind::Emr)
    }

    /// Whether asking this reasoner for the unsatisfiable classes of an
    /// ontology it finds inconsistent is an error: it is for every reasoner but
    /// whelk and the told hierarchy, which answer with what they derived.
    pub(crate) fn refuses_inconsistent_ontology(self) -> bool {
        !matches!(self, ReasonerKind::Whelk | ReasonerKind::Structural)
    }

    /// Whether the reasoner reads the whole ontology as it is made, before it
    /// is asked anything, and so can refuse one then: hermit checks its
    /// property restrictions and datatypes, and whelk classifies.
    pub(crate) fn reads_ontology_when_made(self) -> bool {
        matches!(self, ReasonerKind::Hermit | ReasonerKind::Whelk)
    }
}

/// Whether `kind` finds `model` consistent, and the named classes it finds
/// unsatisfiable.
pub(crate) fn coherence(model: &Model, kind: ReasonerKind) -> Result<(bool, Vec<String>)> {
    let cls = classify(
        model,
        kind,
        false,
        false,
        Types::None,
        false,
        &HashSet::new(),
        PropertyCheck::None,
        kind.datatypes(false),
    )?;
    Ok((cls.consistent, cls.unsat))
}

/// How a reasoner finds the object properties that are unsatisfiable — those
/// whose every use makes a class unsatisfiable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PropertyCheck {
    /// A probe class `P ⊑ ∃p.⊤` per object property in the signature,
    /// classified with the ontology: `p` is unsatisfiable when its probe is.
    Probe,
    /// The bottom node of the classified object-property hierarchy.
    BottomNode,
    /// The told sub-properties of `owl:bottomObjectProperty`, directly or
    /// through other properties: the bottom node of a told property hierarchy.
    Told,
    /// No property is ever unsatisfiable (`structural`).
    None,
}

/// The `--axiom-generators` names. All fourteen are recognised; the ten
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
                | AxiomGenerator::PropertyAssertion
        ) {
            bail!(
                "--axiom-generators {}: owlmake infers only SubClass, EquivalentClass, \
                 ClassAssertion and PropertyAssertion; refusing to silently emit nothing for '{}'",
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
    /// The object properties the `PropertyAssertion` generator is restricted to
    /// (IRIs or CURIEs); empty means every named object property.
    pub properties: Vec<String>,
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
            properties: Vec::new(),
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
            properties: self.properties.clone(),
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
    let want_property_assertion = generators.contains(&AxiomGenerator::PropertyAssertion);
    let types = match (want_class_assertion, opts.include_indirect) {
        (false, _) => Types::None,
        (true, false) => Types::Direct,
        (true, true) => Types::All,
    };
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
    // Under `--create-new-ontology` the output is the root with its axioms
    // removed ([`emptied_root`]); the filters below judge what it then holds.
    let fresh = opts.create_new_ontology || opts.create_new_ontology_with_annotations;
    // Every class of the signature, declared or not: the subclass generator
    // asserts each one's superclasses, `owl:Thing` for a class with no other.
    let signature_classes = signature_classes(&model);
    // whelk reads no declaration: an `owl:Nothing` only a declaration names is
    // a class it does not hold.
    let nothing_only_declared = kind == ReasonerKind::Whelk
        && signature_classes.contains(OWL_NOTHING)
        && !model.ont.iter().any(|ac| {
            !matches!(ac.component, Component::DeclareClass(_))
                && crate::sig::typed_signature(&ac.component)
                    .iter()
                    .any(|(k, iri)| *k == crate::sig::kind::CLASS && iri == OWL_NOTHING)
        });
    // The structural reasoner's direct superclasses are its told parents,
    // `owl:Thing` among them.
    let told_thing: HashSet<String> = if kind == ReasonerKind::Structural {
        told_parents(&model)
            .into_iter()
            .filter(|(_, parents)| parents.contains(&OWL_THING))
            .map(|(c, _)| c.to_string())
            .collect()
    } else {
        HashSet::new()
    };
    // The classes the output declares, for `--exclude-external-entities`.
    let own_declared = if fresh { HashSet::new() } else { own_declared_classes(&model) };
    let mut individuals = if want_class_assertion { individuals_in_signature(&model) } else { Vec::new() };
    let unmet = if want_class_assertion { Unmet::of(&model, kind) } else { Unmet::default() };
    individuals.retain(|i| unmet.individuals.binary_search(i).is_err());
    // The properties the PropertyAssertion generator is restricted to, expanded
    // against the model's prefixes here, before the model may be released.
    let assertion_properties: HashSet<String> = opts
        .properties
        .iter()
        .flat_map(|p| p.split([',', ' ']))
        .filter(|p| !p.is_empty())
        .map(|p| crate::cmd::select::expand_with_document_prefixes(&model, p))
        .collect();
    let existing = existing_subclass_pairs(&model, fresh);
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
    let emptied = fresh.then(|| emptied_root(&model, opts.create_new_ontology_with_annotations));

    // The EL reasoner can release the model before saturating when the model is
    // neither the output (default merge) nor needed for its annotations
    // (`--create-new-ontology-with-annotations`). Whelk/DL/structural keep `&model`
    // — and so does `-D/--dump-unsatisfiable`, which extracts its debug module
    // out of the input ontology after classification.
    let free_model = kind.is_builtin_el()
        && opts.create_new_ontology
        && !opts.create_new_ontology_with_annotations
        && opts.dump_unsatisfiable.is_none();

    // The bottom node lists its members in the order the EL engine's index first
    // names them, which is read off the model; a model released before
    // saturation has its queue taken while it is still in hand.
    let released_queue = if free_model && kind.is_builtin_el() {
        Some(crate::reason::elk_order::class_queue(&model.ont, model.natural_order()))
    } else {
        None
    };
    let check = kind.property_check(false);
    let mut model = Some(model);
    let cls = if free_model {
        let union_elim = kind == ReasonerKind::Owlmake;
        if union_elim {
            status!("reason: using the built-in EL reasoner with union-elimination");
        }
        crate::reason::el::set_whelk_mode(union_elim);
        // What the property check reads off the model is taken before the model
        // is released.
        let model = model.take().unwrap();
        let probes = if check == PropertyCheck::Probe { object_property_signature(&model) } else { Vec::new() };
        let told = if check == PropertyCheck::Told { told_unsatisfiable_properties(&model) } else { Vec::new() };
        // Hand ownership to the reasoner; it drops the model after normalization.
        let r = Reasoner::classify_consume_probing(model, &probes);
        if r.ignored() > 0 {
            status!("note: {} axiom(s) outside OWL 2 EL were ignored during reasoning", r.ignored());
        }
        let consistent = r.is_consistent();
        let unsat = r.unsatisfiable();
        let unsat_properties = el_unsatisfiable_properties(&r, consistent, &unsat, check, told);
        Classification {
            consistent,
            unsat,
            direct: r.direct_subsumptions(),
            all: if need_all { r.all_subsumptions() } else { Vec::new() },
            equiv: if need_equiv { r.equivalent_class_pairs() } else { Vec::new() },
            class_assertions: if want_class_assertion { r.class_assertions() } else { Vec::new() },
            top: r.top_equivalents(),
            property_assertions: if want_property_assertion {
                r.object_property_assertions()
            } else {
                Vec::new()
            },
            unsat_properties,
            holds_thing: true,
        }
    } else {
        classify(
            model.as_ref().unwrap(),
            kind,
            need_all,
            need_equiv,
            types,
            want_property_assertion,
            &assertion_properties,
            check,
            kind.datatypes(false),
        )?
    };
    let Classification {
        consistent,
        unsat,
        direct,
        all,
        equiv,
        class_assertions,
        top,
        mut property_assertions,
        unsat_properties,
        holds_thing,
    } = cls;
    let mut class_assertions = class_assertion_types(kind, &individuals, class_assertions, &top, &all, types);
    class_assertions.extend(unmet.class_assertions(&unsat, types));
    if !assertion_properties.is_empty() {
        property_assertions.retain(|(_, p, _)| assertion_properties.contains(p));
    }

    let listed = unsatisfiable_in_node_order(kind, &unsat, || match released_queue {
        Some(q) => q,
        None => {
            let model = model.as_ref().expect("model kept");
            crate::reason::elk_order::class_queue(&model.ont, model.natural_order())
        }
    });
    validate(&Validation {
        consistent,
        unsatisfiable: &listed,
        unsatisfiable_properties: &unsat_properties,
        dump: opts.dump_unsatisfiable.as_deref().map(|path| (path, model.as_ref())),
        allow_incoherent: Some(opts.allow_incoherent),
    })?;

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

    // Base subsumption set: direct (transitive reduction) or all (indirect),
    // with the edges into the top node — `owl:Thing` and the classes the
    // reasoner finds equivalent to it. A class in the top node has no
    // superclass to assert. A class with no parent outside it has the whole
    // node as its direct superclasses (under whelk, `owl:Thing` alone), and
    // under --include-indirect every class has. The exclusion flags below
    // remove the edges to `owl:Thing` again when requested.
    let top_node: HashSet<&str> = top.iter().map(String::as_str).filter(|c| *c != OWL_THING).collect();
    // A mutual pair — each member of an equivalence node subsuming the other —
    // is no parent. An asserted ANONYMOUS superclass is no parent either: a
    // class whose only superclass is a restriction is still a child of the top
    // node.
    let pairs: HashSet<(&str, &str)> = direct.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let has_named_super: HashSet<&str> = direct
        .iter()
        .filter(|(sub, sup)| {
            sup != OWL_THING && !top_node.contains(sup.as_str()) && !pairs.contains(&(sup.as_str(), sub.as_str()))
        })
        .map(|(sub, _)| sub.as_str())
        .collect();
    // Whether the top node holds `c`'s direct superclasses.
    let top_is_direct = |c: &str| !top_node.contains(c) && !has_named_super.contains(c);
    let mut base: Vec<(String, String)> = if opts.include_indirect {
        all.clone()
    } else {
        direct.clone()
    };
    base.retain(|(sub, sup)| {
        !top_node.contains(sub.as_str()) && !(kind == ReasonerKind::Whelk && top_node.contains(sup.as_str()))
    });
    // Every class in the SIGNATURE, declared or not: a merge can leave an
    // undeclared class referenced by surviving axioms, and its inferred
    // superclass is still asserted — under a bare `reason` that is the trivial
    // `⊑ owl:Thing` root edge.
    for c in &signature_classes {
        if c == OWL_THING || c == OWL_NOTHING || top_node.contains(c.as_str()) {
            continue;
        }
        if opts.include_indirect || top_is_direct(c) || told_thing.contains(c.as_str()) {
            base.push((c.clone(), OWL_THING.to_string()));
            if kind != ReasonerKind::Whelk {
                base.extend(top_node.iter().map(|t| (c.clone(), t.to_string())));
            }
        }
    }
    base.sort();
    base.dedup();
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

    let mut target = match emptied {
        Some((kept, imported)) => {
            let mut root = Model::from_parts(kept.into_iter().collect(), prefixes);
            root.imported_components = imported;
            root
        }
        // The model itself is the output (free_model is false).
        None => std::mem::take(model.as_mut().expect("model retained for merge output")),
    };

    let infer_prop = target
        .build
        .annotation_property("http://www.geneontology.org/formats/oboInOwl#is_inferred");

    let mut added = 0usize;
    if want_subclass {
        // `owl:Nothing` is a class of the signature when the closure names it
        // anywhere — in an expression, a rule, a declaration — and its node's
        // own edge is asserted with it. whelk holds no class that is only
        // declared, so to it an `owl:Nothing` only declared is one more class
        // below `owl:Thing`.
        let nothing_sup = if nothing_only_declared { OWL_THING } else { OWL_NOTHING };
        let filtered = if nothing_sup == OWL_THING { exclude_thing } else { exclude_self };
        if signature_classes.contains(OWL_NOTHING)
            && !filtered
            && !(opts.exclude_external_entities && !own_declared.contains(OWL_NOTHING))
        {
            let ax = Component::SubClassOf(SubClassOf {
                sub: CE::Class(target.build.class(OWL_NOTHING.to_string())),
                sup: CE::Class(target.build.class(nothing_sup.to_string())),
            });
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop)? {
                added += 1;
            }
        }
        // whelk takes a class it does not hold to be below `owl:Thing` and
        // nothing else; so with every superclass asked for, `owl:Thing`, a
        // class of the signature no axiom the reasoner reads names, is a
        // subclass of itself.
        let thing = (OWL_THING.to_string(), OWL_THING.to_string());
        if opts.include_indirect
            && !holds_thing
            && signature_classes.contains(OWL_THING)
            && !exclude_thing
            && !exclude_self
            && !(opts.exclude_external_entities && !own_declared.contains(OWL_THING))
            && !(opts.exclude_duplicate_axioms && existing.contains(&thing))
        {
            let ax = Component::SubClassOf(SubClassOf {
                sub: CE::Class(target.build.class(OWL_THING.to_string())),
                sup: CE::Class(target.build.class(OWL_THING.to_string())),
            });
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop)? {
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
            if opts.exclude_external_entities && !own_declared.contains(sub) && !own_declared.contains(sup) {
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
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop)? {
                added += 1;
            }
        }
    }

    if want_equiv {
        let existing_eq = if opts.exclude_duplicate_axioms {
            existing_equivalences(&target)
        } else {
            HashSet::new()
        };
        for (a, b) in &equiv_pairs {
            if opts.exclude_external_entities && !own_declared.contains(a) && !own_declared.contains(b) {
                continue;
            }
            if existing_eq.contains(&(a.clone(), b.clone())) {
                continue;
            }
            let ax = Component::EquivalentClasses(EquivalentClasses(vec![
                CE::Class(target.build.class(a.clone())),
                CE::Class(target.build.class(b.clone())),
            ]));
            if taut_checker.as_ref().is_some_and(|t| t.is_tautology(&ax)) {
                continue;
            }
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop)? {
                added += 1;
            }
        }
    }

    if want_class_assertion {
        let existing_ca = existing_class_assertions(&target);
        for (ind, class) in &class_assertions {
            // An assertion of `owl:Thing` names `owl:Thing`, which the three
            // switches drop, and is a tautology to both tests.
            if class == OWL_THING && exclude_thing {
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
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop)? {
                added += 1;
            }
        }
    }

    if want_property_assertion {
        let existing_pa = existing_object_property_assertions(&target);
        for (from, prop, to) in &property_assertions {
            if opts.exclude_duplicate_axioms
                && existing_pa.contains(&(from.clone(), prop.clone(), to.clone()))
            {
                continue;
            }
            let ax = Component::ObjectPropertyAssertion(ObjectPropertyAssertion {
                ope: OPE::ObjectProperty(target.build.object_property(prop.clone())),
                from: Individual::Named(target.build.named_individual(from.clone())),
                to: Individual::Named(target.build.named_individual(to.clone())),
            });
            if taut_checker.as_ref().is_some_and(|t| t.is_tautology(&ax)) {
                continue;
            }
            if insert_axiom(&mut target, ax, opts.annotate_inferred_axioms, &infer_prop)? {
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
    // Under `emr` no subclass axiom is redundant.
    if opts.remove_redundant_subclass_axioms && kind != ReasonerKind::Emr {
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
                            // A superclass of `owl:Thing` or `owl:Nothing` is
                            // never redundant, nor is `owl:Nothing` as a
                            // superclass.
                            if c == OWL_THING || c == OWL_NOTHING || x == OWL_NOTHING {
                                return false;
                            }
                            // The top node holds the direct superclasses of a
                            // class with no parent outside it, but under whelk
                            // only `owl:Thing` of it is one. The structural
                            // reasoner's direct superclasses are its told
                            // parents, so an asserted one is never redundant.
                            if x == OWL_THING || top_node.contains(x) {
                                return match kind {
                                    ReasonerKind::Structural => false,
                                    ReasonerKind::Whelk if x != OWL_THING => true,
                                    _ => !top_is_direct(c),
                                };
                            }
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
                            let proper_direct = pairs.contains(&(c, x)) && !pairs.contains(&(x, c));
                            // A self-subsumption goes too: a class is never among
                            // its own direct superclasses, so an asserted `C ⊑ C`
                            // is never in the inferred set and is removed. EFO
                            // asserts two of them by hand; exempting `c == x` here
                            // would leave both in the released `efo.owl`.
                            !proper_direct
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

    // The axioms the imports lent are still theirs, but for those an inference
    // asserted in the root ([`insert_axiom`]).
    let imported = std::mem::take(&mut target.imported_components);
    target.carry_meta_from(&meta_src);
    target.imported_components = imported;
    if fresh {
        // The emptied root imports nothing: its imports' axioms stay loaded,
        // and declare what they declare, but no import is written.
        target.inlined_imports.clear();
        target.import_order.clear();
        if let Some(closure) = target.imports_closure.as_mut() {
            closure.outlives_imports = true;
        }
    }
    target.mark_root_changed();
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
    /// The class assertions the reasoner makes, (individual, class), only for
    /// the `ClassAssertion` generator: under the EL and DL reasoners each held
    /// individual's direct named types; under whelk and the structural
    /// reasoner the types [`Types`] asks for, `owl:Thing` included.
    class_assertions: Vec<(String, String)>,
    /// The named classes equivalent to `owl:Thing`, which with it make the top
    /// node; for the structural reasoner, those a told cycle puts with it.
    top: Vec<String>,
    /// Inferred object property assertions (subject, property, object) between
    /// named individuals; only the `property-assertion` generator populates this.
    property_assertions: Vec<(String, String, String)>,
    /// The unsatisfiable object properties, sorted; asked only of an ontology
    /// that is consistent and has no unsatisfiable class.
    unsat_properties: Vec<String>,
    /// Whether the reasoner holds `owl:Thing` as a class of its own; only
    /// whelk can hold it not ([`WhelkClassification::holds_thing`](crate::reason::WhelkClassification::holds_thing)).
    holds_thing: bool,
}

#[allow(clippy::too_many_arguments)]
fn classify(
    model: &Model,
    kind: ReasonerKind,
    need_all: bool,
    need_equiv: bool,
    types: Types,
    need_property_assertions: bool,
    assertion_properties: &HashSet<String>,
    check: PropertyCheck,
    datatypes: Datatypes,
) -> Result<Classification> {
    Ok(match kind {
        // hermit-rs (DL) and whelk-rs (EL) both build for wasm, so `hermit`/
        // `jfact`/`whelk` all work in the browser too (see src/reason/mod.rs).
        ReasonerKind::Hermit | ReasonerKind::JFact => {
            status!("reason: using hermit-rs, the HermiT OWL 2 DL reasoner ('{kind:?}')");
            let r = crate::reason::DlReasoner::classify_with(model, datatypes, kind.rules());
            r.try_classify().map_err(anyhow::Error::msg)?;
            let direct = r.direct_subsumptions();
            let all = if need_all { r.all_subsumptions() } else { Vec::new() };
            // The ABox queries need a consistent ontology; `reason_with` fails
            // on inconsistency right after this, so an inconsistent one just
            // gets no assertions.
            let consistent = r.is_consistent();
            let unsat = r.unsatisfiable();
            let unsat_properties = match check {
                _ if !consistent || !unsat.is_empty() => Vec::new(),
                PropertyCheck::BottomNode => r.unsatisfiable_object_properties(),
                PropertyCheck::Told => told_unsatisfiable_properties(model),
                PropertyCheck::Probe | PropertyCheck::None => Vec::new(),
            };
            Classification {
                consistent,
                unsat,
                direct,
                all,
                equiv: if need_equiv { r.equivalent_class_pairs() } else { Vec::new() },
                class_assertions: if types != Types::None && consistent {
                    r.class_assertions()
                } else {
                    Vec::new()
                },
                top: if consistent { r.top_equivalents() } else { Vec::new() },
                property_assertions: if need_property_assertions && consistent {
                    r.object_property_assertions(assertion_properties)
                } else {
                    Vec::new()
                },
                unsat_properties,
                holds_thing: true,
            }
        }
        ReasonerKind::Whelk => {
            status!("reason: using the whelk-rs EL reasoner");
            let r = crate::reason::WhelkClassification::classify(model)?;
            let direct = r.direct_subsumptions();
            let consistent = r.is_consistent();
            let unsat = r.unsatisfiable();
            let unsat_properties = if consistent && unsat.is_empty() && check == PropertyCheck::Probe {
                r.unsatisfiable_properties(&object_property_signature(model))
            } else {
                Vec::new()
            };
            Classification {
                consistent,
                unsat,
                // Read the saturated closure, NOT `direct`: the direct list drops
                // equivalence-clique siblings, so aliasing `all` to it would make
                // every equivalence invisible to `--equivalent-classes-allowed`
                // under `--reasoner whelk`, the reasoner CL classifies with.
                all: if need_all { r.all_subsumptions() } else { Vec::new() },
                equiv: if need_equiv { r.equivalent_class_pairs() } else { Vec::new() },
                direct,
                class_assertions: match types {
                    Types::None => Vec::new(),
                    _ if !consistent => Vec::new(),
                    Types::Direct => r.class_assertions(true),
                    Types::All => r.class_assertions(false),
                },
                top: r.top_node().into_iter().filter(|c| c != OWL_THING).collect(),
                property_assertions: if need_property_assertions && consistent {
                    let mut asked: Vec<String> = if assertion_properties.is_empty() {
                        object_property_signature(model)
                            .into_iter()
                            .filter(|p| p != OWL_TOP_OBJECT_PROPERTY && p != OWL_BOTTOM_OBJECT_PROPERTY)
                            .collect()
                    } else {
                        assertion_properties.iter().cloned().collect()
                    };
                    asked.sort();
                    r.object_property_assertions(&asked)
                } else {
                    Vec::new()
                },
                unsat_properties,
                holds_thing: r.holds_thing(),
            }
        }
        // `structural`: the TOLD hierarchy, no reasoning at all.
        ReasonerKind::Structural => classify_structural(model, need_all, need_equiv, types),
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
            let probes = if check == PropertyCheck::Probe { object_property_signature(model) } else { Vec::new() };
            let r = Reasoner::classify_probing(model, &probes);
            if r.ignored() > 0 {
                status!("note: {} axiom(s) outside OWL 2 EL were ignored during reasoning", r.ignored());
            }
            let told = if check == PropertyCheck::Told { told_unsatisfiable_properties(model) } else { Vec::new() };
            // `all_subsumptions` is the full transitive closure — O(n·avg-supers)
            // entries, which is enormous on phenio-scale inputs; only build it when
            // indirect output actually needs it (the equivalence policy reads
            // `equivalent_class_pairs`, which scans the S-sets without a closure).
            let consistent = r.is_consistent();
            let unsat = r.unsatisfiable();
            let unsat_properties = el_unsatisfiable_properties(&r, consistent, &unsat, check, told);
            Classification {
                consistent,
                unsat,
                direct: r.direct_subsumptions(),
                all: if need_all { r.all_subsumptions() } else { Vec::new() },
                equiv: if need_equiv { r.equivalent_class_pairs() } else { Vec::new() },
                class_assertions: if types != Types::None { r.class_assertions() } else { Vec::new() },
                top: r.top_equivalents(),
                property_assertions: if need_property_assertions {
                    r.object_property_assertions()
                } else {
                    Vec::new()
                },
                unsat_properties,
                holds_thing: true,
            }
        }
    })
}

/// The unsatisfiable object properties of an EL classification under `check`:
/// the probes [`Reasoner::classify_probing`] added, or the `told` ones.
fn el_unsatisfiable_properties(
    r: &Reasoner,
    consistent: bool,
    unsat: &[String],
    check: PropertyCheck,
    told: Vec<String>,
) -> Vec<String> {
    match check {
        _ if !consistent || !unsat.is_empty() => Vec::new(),
        PropertyCheck::Probe => r.unsatisfiable_probes(),
        PropertyCheck::Told => told,
        PropertyCheck::BottomNode | PropertyCheck::None => Vec::new(),
    }
}

/// The object properties in the ontology's signature, sorted: one probe each.
fn object_property_signature(model: &Model) -> Vec<String> {
    use horned_owl::model::{ObjectProperty, RcStr};
    use horned_owl::visitor::immutable::{Visit, Walk};

    #[derive(Default)]
    struct Properties(std::collections::BTreeSet<String>);
    impl Visit<RcStr> for Properties {
        fn visit_object_property(&mut self, p: &ObjectProperty<RcStr>) {
            if !self.0.contains(p.0.as_ref()) {
                self.0.insert(p.0.to_string());
            }
        }
    }
    let mut walk = Walk::new(Properties::default());
    for ac in model.ont.iter() {
        walk.component(&ac.component);
    }
    walk.into_visit().0.into_iter().collect()
}

/// The named object properties that are told sub-properties of
/// `owl:bottomObjectProperty` — directly, through other named properties, or
/// by a told equivalence — sorted.
pub(crate) fn told_unsatisfiable_properties(model: &Model) -> Vec<String> {
    use horned_owl::model::SubObjectPropertyExpression as SOPE;
    const BOTTOM: &str = "http://www.w3.org/2002/07/owl#bottomObjectProperty";
    // Each property's told sub-properties.
    let mut subs: HashMap<&str, Vec<&str>> = HashMap::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubObjectPropertyOf(ax) => {
                if let (SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)), OPE::ObjectProperty(sup)) =
                    (&ax.sub, &ax.sup)
                {
                    subs.entry(sup.0.as_ref()).or_default().push(sub.0.as_ref());
                }
            }
            Component::EquivalentObjectProperties(ax) => {
                let named: Vec<&str> = ax
                    .0
                    .iter()
                    .filter_map(|ope| match ope {
                        OPE::ObjectProperty(p) => Some(p.0.as_ref()),
                        OPE::InverseObjectProperty(_) => None,
                    })
                    .collect();
                for &a in &named {
                    for &b in &named {
                        if a != b {
                            subs.entry(b).or_default().push(a);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut below: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut stack = vec![BOTTOM];
    while let Some(p) = stack.pop() {
        for &sub in subs.get(p).into_iter().flatten() {
            if below.insert(sub) {
                stack.push(sub);
            }
        }
    }
    below.remove(BOTTOM);
    below.into_iter().map(str::to_string).collect()
}

/// Which types of each individual the `ClassAssertion` generator asks the
/// reasoner for: none, its direct types, or under `--include-indirect` all of
/// them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Types {
    None,
    Direct,
    All,
}

/// The named individuals of the signature that the reasoner reads nothing
/// about, and the classes it holds. `jfact` reads past every SWRL rule, so it
/// never meets an individual that only rules name; it gives one the bottom node
/// as its direct types, and as all of them every class it holds, `owl:Thing`
/// and `owl:Nothing` among them. Every other reasoner meets every individual.
#[derive(Default)]
struct Unmet {
    /// Sorted.
    individuals: Vec<String>,
    classes: Vec<String>,
}

impl Unmet {
    fn of(model: &Model, kind: ReasonerKind) -> Unmet {
        if kind != ReasonerKind::JFact {
            return Unmet::default();
        }
        let rules = kind.rules();
        let mut met = HashSet::new();
        let mut named = std::collections::BTreeSet::new();
        let mut classes = std::collections::BTreeSet::from([OWL_THING.to_string(), OWL_NOTHING.to_string()]);
        for ac in model.ont.iter() {
            let reads = rules.reads(&ac.component);
            for (k, iri) in crate::sig::typed_signature(&ac.component) {
                match k {
                    crate::sig::kind::NAMED_INDIVIDUAL if reads => {
                        met.insert(iri);
                    }
                    crate::sig::kind::NAMED_INDIVIDUAL => {
                        named.insert(iri);
                    }
                    crate::sig::kind::CLASS if reads => {
                        classes.insert(iri);
                    }
                    _ => {}
                }
            }
        }
        let individuals: Vec<String> = named.into_iter().filter(|i| !met.contains(i)).collect();
        Unmet { classes: classes.into_iter().collect(), individuals }
    }

    /// The class assertions the `ClassAssertion` generator makes of the
    /// individuals the reasoner never meets, given the classes it found
    /// unsatisfiable.
    fn class_assertions(&self, unsat: &[String], types: Types) -> Vec<(String, String)> {
        let classes: Vec<&str> = match types {
            Types::None => return Vec::new(),
            Types::Direct => std::iter::once(OWL_NOTHING)
                .chain(unsat.iter().map(String::as_str).filter(|c| *c != OWL_NOTHING))
                .collect(),
            Types::All => self.classes.iter().map(String::as_str).collect(),
        };
        self.individuals
            .iter()
            .flat_map(|i| classes.iter().map(move |c| (i.clone(), c.to_string())))
            .collect()
    }
}

/// The named individuals of the signature of `model` and its imports, sorted.
fn individuals_in_signature(model: &Model) -> Vec<String> {
    let mut out: Vec<String> = model
        .ont
        .iter()
        .flat_map(|ac| crate::sig::typed_signature(&ac.component))
        .filter(|(k, _)| *k == crate::sig::kind::NAMED_INDIVIDUAL)
        .map(|(_, iri)| iri)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The class assertions the structural reasoner makes: each named individual
/// with the types [`Told::types`](crate::reason::told::Told::types) gives it
/// from the named classes it is told it is an instance of.
fn told_class_assertions(model: &Model, types: Types) -> Vec<(String, String)> {
    if types == Types::None {
        return Vec::new();
    }
    let mut asserted: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for ac in model.ont.iter() {
        if let Component::ClassAssertion(ClassAssertion { ce: CE::Class(c), i: Individual::Named(i) }) = &ac.component {
            asserted.entry(i.0.to_string()).or_default().push(c.0.to_string());
        }
    }
    if asserted.is_empty() {
        return Vec::new();
    }
    let told = crate::reason::told::Told::of(model);
    let mut out = Vec::new();
    for (i, classes) in asserted {
        for c in told.types(&classes, types == Types::Direct) {
            out.push((i.clone(), c));
        }
    }
    out
}

/// The class assertions the `ClassAssertion` generator makes, from what the
/// reasoner reported ([`Classification::class_assertions`]): each named
/// individual of the signature with its types. The EL and DL reasoners give
/// an individual whose named types are all equivalent to `owl:Thing`, or that
/// has none, the top node, `owl:Thing` and its equivalents; asked for every
/// type, they give each individual the classes above its direct types and the
/// top node too. whelk gives an individual it does not hold `owl:Thing`. The
/// structural reasoner gives an individual the types it is told and no other.
fn class_assertion_types(
    kind: ReasonerKind,
    individuals: &[String],
    reported: Vec<(String, String)>,
    top: &[String],
    all: &[(String, String)],
    types: Types,
) -> Vec<(String, String)> {
    if types == Types::None {
        return Vec::new();
    }
    match kind {
        ReasonerKind::Structural => return reported,
        ReasonerKind::Whelk => {
            let held: HashSet<&str> = reported.iter().map(|(i, _)| i.as_str()).collect();
            let mut out: Vec<(String, String)> = individuals
                .iter()
                .filter(|i| !held.contains(i.as_str()))
                .map(|i| (i.clone(), OWL_THING.to_string()))
                .collect();
            out.extend(reported);
            return out;
        }
        _ => {}
    }
    let mut by_individual: HashMap<&str, std::collections::BTreeSet<&str>> = HashMap::new();
    for (i, c) in &reported {
        if c != OWL_THING && c != OWL_NOTHING {
            by_individual.entry(i.as_str()).or_default().insert(c.as_str());
        }
    }
    let mut out = Vec::new();
    for i in individuals {
        let mut classes = by_individual.remove(i.as_str()).unwrap_or_default();
        if types == Types::All {
            let direct: Vec<&str> = classes.iter().copied().collect();
            for d in direct {
                let from = all.partition_point(|(sub, _)| sub.as_str() < d);
                classes.extend(all[from..].iter().take_while(|(sub, _)| sub == d).map(|(_, sup)| sup.as_str()));
            }
        }
        if types == Types::All || classes.iter().all(|c| top.iter().any(|t| t == c)) {
            classes.insert(OWL_THING);
            classes.extend(top.iter().map(String::as_str));
        }
        out.extend(classes.into_iter().map(|c| (i.clone(), c.to_string())));
    }
    out
}

/// `--reasoner structural` — the told class hierarchy.
///
/// This is a *told* hierarchy, not a reasoner: it is the transitive closure of
/// the told parents [`told_parents`] reads, with no normalisation, no ∃-role
/// reasoning and no satisfiability testing at all: no
/// class is ever reported unsatisfiable — `owl:Nothing` included — and the
/// ontology is always consistent. It is a legal `--reasoner` value, and a repo
/// may well be configured with it, so it has to stay this weak rather than
/// quietly running the full EL engine — which would report inferences a told
/// hierarchy does not make, and unsatisfiable classes it can never find.
fn classify_structural<'a>(model: &'a Model, need_all: bool, need_equiv: bool, types: Types) -> Classification {
    status!("reason: using the structural reasoner (told class hierarchy)");
    let told = told_parents(model);

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
    }
    // A class's direct parents are the told parents of its node, each with every
    // class of its own node: a told parent is direct whatever else lies between.
    // The class's own node (the classes a told cycle makes equivalent to it) is
    // no parent of it.
    let node_of = |c: &'a str| -> HashSet<&'a str> {
        let mut node: HashSet<&str> = HashSet::from([c]);
        if let Some(sups) = closure.get(c) {
            node.extend(sups.iter().copied().filter(|&d| sub_of(d, c)));
        }
        node
    };
    for &c in told.keys() {
        if c == OWL_THING || c == OWL_NOTHING {
            continue;
        }
        let node = node_of(c);
        for &e in &node {
            for &p in told.get(e).into_iter().flatten() {
                if node.contains(p) || p == OWL_THING || p == OWL_NOTHING {
                    continue;
                }
                for m in node_of(p) {
                    if !node.contains(m) {
                        direct.push((c.to_string(), m.to_string()));
                    }
                }
            }
        }
    }
    all.sort();
    all.dedup();
    equiv.sort();
    equiv.dedup();
    direct.sort();
    direct.dedup();
    // The top node: `owl:Thing` and the classes a told cycle makes equivalent
    // to it.
    let told_ancestors = |start: &'a str| -> HashSet<&'a str> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = told.get(start).cloned().unwrap_or_default();
        while let Some(c) = stack.pop() {
            if seen.insert(c) {
                stack.extend(told.get(c).into_iter().flatten().copied());
            }
        }
        seen
    };
    let mut top: Vec<String> = told_ancestors(OWL_THING)
        .into_iter()
        .filter(|&c| c != OWL_THING && c != OWL_NOTHING && told_ancestors(c).contains(OWL_THING))
        .map(str::to_string)
        .collect();
    top.sort();
    Classification {
        // A told hierarchy has no satisfiability test: it never reports an
        // inconsistency, an unsatisfiable class or an unsatisfiable property.
        consistent: true,
        unsat: Vec::new(),
        direct,
        all,
        equiv,
        class_assertions: told_class_assertions(model, types),
        top,
        property_assertions: Vec::new(),
        unsat_properties: Vec::new(),
        holds_thing: true,
    }
}

/// The told parents of each named class: the named superclass of a
/// `SubClassOf` it is the subclass of, or the named conjuncts of an
/// intersection there; and in an `EquivalentClasses` it is a member of, every
/// other named member and the named conjuncts of every other intersection.
/// Nothing else is told: a restriction, a union or a complement names no
/// parent.
pub(crate) fn told_parents(model: &Model) -> HashMap<&str, Vec<&str>> {
    // A named class, or the named conjuncts of an intersection, nested
    // intersections included.
    fn named_conjuncts<'a>(ce: &'a CE<horned_owl::model::RcStr>, out: &mut Vec<&'a str>) {
        match ce {
            CE::Class(c) => out.push(c.0.as_ref()),
            CE::ObjectIntersectionOf(ops) => {
                for op in ops {
                    if let CE::Class(c) = op {
                        out.push(c.0.as_ref());
                    } else if matches!(op, CE::ObjectIntersectionOf(_)) {
                        named_conjuncts(op, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut told: HashMap<&str, Vec<&str>> = HashMap::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    let mut parents = Vec::new();
                    named_conjuncts(&sc.sup, &mut parents);
                    told.entry(sub.0.as_ref()).or_default().extend(parents);
                }
            }
            Component::EquivalentClasses(eq) => {
                for member in &eq.0 {
                    let CE::Class(child) = member else { continue };
                    let mut parents = Vec::new();
                    for other in eq.0.iter().filter(|&o| o != member) {
                        named_conjuncts(other, &mut parents);
                    }
                    told.entry(child.0.as_ref()).or_default().extend(parents);
                }
            }
            _ => {}
        }
    }
    told
}

/// Each class's direct superclasses as `kind` gives them: every class of each
/// node directly above the class's own. Under `structural` those are the told
/// parents; under any other reasoner the superclasses with none of the class's
/// other superclasses strictly below them. `whelk` takes one class of each
/// such node, the first its walk over the class's subsumers reaches.
/// `owl:Thing` and `owl:Nothing` are no superclass. `datatypes` is what the
/// reasoner does with a datatype outside the OWL 2 datatype map.
pub(crate) fn direct_superclass_nodes(
    model: &Model,
    kind: ReasonerKind,
    datatypes: Datatypes,
) -> Result<Vec<(String, String)>> {
    match kind {
        ReasonerKind::Structural => return Ok(classify_structural(model, false, false, Types::None).direct),
        ReasonerKind::Whelk => {
            return Ok(crate::reason::WhelkClassification::classify(model)?.direct_subsumptions())
        }
        _ => {}
    }
    let all = classify(model, kind, true, false, Types::None, false, &HashSet::new(), PropertyCheck::None, datatypes)?.all;
    let mut supers: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (a, b) in &all {
        if a != b && ![a, b].iter().any(|c| *c == OWL_THING || *c == OWL_NOTHING) {
            supers.entry(a.as_str()).or_default().insert(b.as_str());
        }
    }
    let sub_of = |a: &str, b: &str| supers.get(a).is_some_and(|s| s.contains(b));
    let mut out: Vec<(String, String)> = Vec::new();
    for (&c, sups) in &supers {
        // The class's own node is no superclass of it.
        let strict: Vec<&str> = sups.iter().copied().filter(|&d| !sub_of(d, c)).collect();
        for &d in &strict {
            let between = strict.iter().any(|&e| e != d && sub_of(e, d) && !sub_of(d, e));
            if !between {
                out.push((c.to_string(), d.to_string()));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// What a classification says about an ontology's coherence, for [`validate`].
pub(crate) struct Validation<'a> {
    pub consistent: bool,
    /// The unsatisfiable classes, in the order the reasoner's bottom node lists
    /// them (see [`unsatisfiable_in_node_order`]).
    pub unsatisfiable: &'a [String],
    /// The unsatisfiable object properties, sorted.
    pub unsatisfiable_properties: &'a [String],
    /// `-D/--dump-unsatisfiable`, with the model the module is extracted from.
    pub dump: Option<(&'a std::path::Path, Option<&'a Model>)>,
    /// `--allow-incoherent`, for a command that has it.
    pub allow_incoherent: Option<bool>,
}

/// Check a classification before anything is inferred from it. An
/// inconsistent ontology fails; then one with an unsatisfiable class, whose
/// module is first written to the dump file; then one with an unsatisfiable
/// object property. `--allow-incoherent` reports the unsatisfiable classes or
/// properties and goes on.
///
/// Each failure is reported on the console as a logged error — a count line
/// followed by one line per class or property: CI jobs grep the log for the
/// wording, and a recipe that redirects the console (`reason … > report.txt`)
/// keeps the whole list as its report.
pub(crate) fn validate(v: &Validation) -> Result<()> {
    let hint = if v.allow_incoherent.is_some() { " Use --allow-incoherent to override." } else { "" };
    let allowed = v.allow_incoherent == Some(true);
    if !v.consistent {
        let mut lines = vec!["The ontology is inconsistent. TIP: use a tool like Protege to find explanations".to_string()];
        if v.dump.is_some() {
            // No module is written for an inconsistent ontology.
            lines.push(
                "Unfortunately, robot is not able to generate an unsatisfiable minimal model for inconsistent ontologies at this time.\n"
                    .to_string(),
            );
            lines.push("TIP: remove individuals from ontology and try again".to_string());
        }
        log_reasoner_errors(&lines);
        bail!("ontology is inconsistent (owl:Thing is unsatisfiable)");
    }
    if !v.unsatisfiable.is_empty() {
        log_unsatisfiable(v.unsatisfiable);
        if let Some((path, model)) = v.dump {
            dump_unsatisfiable_module(model, v.unsatisfiable, path)?;
        }
        if !allowed {
            bail!("ontology is incoherent: {} unsatisfiable class(es).{hint}", v.unsatisfiable.len());
        }
        return Ok(());
    }
    if !v.unsatisfiable_properties.is_empty() {
        // Listed in the hash order of the set they are gathered in.
        let names = v.unsatisfiable_properties;
        let hashes: Vec<i32> = names.iter().map(|p| crate::owlapi_hash::object_property_hash(p)).collect();
        let mut lines = vec![format!("There are {} unsatisfiable properties in the ontology.", names.len())];
        for i in crate::owlapi_hash::hashset_order(&hashes) {
            lines.push(format!("    unsatisfiable property: {}", names[i]));
        }
        log_reasoner_errors(&lines);
        if !allowed {
            bail!("ontology is incoherent: {} unsatisfiable object propert{}.{hint}", names.len(),
                if names.len() == 1 { "y" } else { "ies" });
        }
    }
    Ok(())
}

/// Classify `model` with `kind` and [`validate`] the result, for a command that
/// reasons only to check the ontology first. `materializing` selects the
/// property check and the datatypes of a reasoner wrapped for expression
/// materialization.
pub(crate) fn validate_model(model: &Model, kind: ReasonerKind, materializing: bool) -> Result<()> {
    let cls = classify(
        model,
        kind,
        false,
        false,
        Types::None,
        false,
        &HashSet::new(),
        kind.property_check(materializing),
        kind.datatypes(materializing),
    )?;
    let listed = unsatisfiable_in_node_order(kind, &cls.unsat, || crate::reason::elk_order::class_queue(&model.ont, model.natural_order()));
    validate(&Validation {
        consistent: cls.consistent,
        unsatisfiable: &listed,
        unsatisfiable_properties: &cls.unsat_properties,
        dump: None,
        allow_incoherent: None,
    })
}

/// The unsatisfiable classes in the order the reasoner's bottom node lists its
/// members. The EL engine's node lists them in the order its index first names
/// them, which is read off the model (`queue`, asked for only when a class is
/// unsatisfiable); every other reasoner's node in its classes' hash order.
pub(crate) fn unsatisfiable_in_node_order(
    kind: ReasonerKind,
    unsat: &[String],
    queue: impl FnOnce() -> Vec<horned_owl::model::IRI<horned_owl::model::RcStr>>,
) -> Vec<String> {
    if unsat.is_empty() {
        Vec::new()
    } else if kind.is_builtin_el() {
        crate::reason::elk_order::bottom_node_order(&queue(), unsat)
    } else {
        crate::owlapi_hash::class_node_order(unsat).into_iter().map(|i| unsat[i].clone()).collect()
    }
}

/// Log a reasoner check's failure on the console as errors, one per line, all
/// under one time stamp.
fn log_reasoner_errors(lines: &[String]) {
    let stamp = log_stamp();
    for msg in lines {
        crate::build::console_line(&format!("{stamp} ERROR org.obolibrary.robot.ReasonerHelper - {msg}"));
    }
}

/// Report unsatisfiable classes on the console as a logged error: a count line,
/// then one `    unsatisfiable: <IRI>` line per class in the order given.
fn log_unsatisfiable(listed: &[String]) {
    let mut lines = vec![format!("There are {} unsatisfiable classes in the ontology.", listed.len())];
    lines.extend(listed.iter().map(|name| format!("    unsatisfiable: {name}")));
    log_reasoner_errors(&lines);
}

/// Insert an inferred axiom. Returns whether it was newly added.
///
/// Annotated with `is_inferred true`, it takes the place of the same axiom
/// asserted without annotations, which is then the inferred one. Only a
/// `SubClassOf` can be annotated: any other inferred axiom is refused.
fn insert_axiom(
    model: &mut Model,
    component: Component<crate::model::Str>,
    annotate: bool,
    infer_prop: &horned_owl::model::AnnotationProperty<crate::model::Str>,
) -> Result<bool> {
    let plain = AnnotatedComponent { component, ann: Default::default() };
    if !annotate {
        // An inference the root's imports assert is asserted in the root as well.
        let promoted = model.imported_components.remove(&plain);
        return Ok(model.ont.insert(plain) || promoted);
    }
    let component = plain.component;
    if !matches!(component, Component::SubClassOf(_)) {
        // The axiom's type, by the class name the message gives it.
        let class = match &component {
            Component::EquivalentClasses(_) => "OWLEquivalentClassesAxiomImpl",
            Component::ClassAssertion(_) => "OWLClassAssertionAxiomImpl",
            Component::ObjectPropertyAssertion(_) => "OWLObjectPropertyAssertionAxiomImpl",
            other => unreachable!("reason asserts no {:?}", horned_owl::model::Kinded::kind(other)),
        };
        bail!("AXIOM TYPE ERROR cannot annotate axioms of type: class uk.ac.manchester.cs.owl.owlapi.{class}");
    }
    // The annotated copy replaces the root's own; an import's stays the import's.
    let plain = AnnotatedComponent { component: component.clone(), ann: Default::default() };
    if !model.imported_components.contains(&plain) {
        model.ont.remove(&plain);
    }
    let ann = Annotation {
        ann: Default::default(),
        ap: infer_prop.clone(),
        av: AnnotationValue::Literal(Literal::Simple { literal: "true".to_string() }),
    };
    Ok(model.ont.insert(AnnotatedComponent { component, ann: std::collections::BTreeSet::from([ann]) }))
}

/// The axioms `--exclude-duplicate-axioms` counts as asserted: those of the
/// root and its imports, or of its imports alone where the root is emptied
/// ([`emptied_root`]), each as stated, so an axiom asserted with annotations
/// is not one inferred without them.
fn asserted(model: &Model, fresh: bool) -> impl Iterator<Item = &Component<crate::model::Str>> {
    model
        .ont
        .iter()
        .filter(move |ac| ac.ann.is_empty() && (!fresh || model.imported_components.contains(*ac)))
        .map(|ac| &ac.component)
}

fn existing_subclass_pairs(model: &Model, fresh: bool) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    for component in asserted(model, fresh) {
        if let Component::SubClassOf(sc) = component {
            if let (CE::Class(a), CE::Class(b)) = (&sc.sub, &sc.sup) {
                out.insert((a.0.as_ref().to_string(), b.0.as_ref().to_string()));
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

/// The two-class equivalences `model` asserts ([`asserted`]), in both orders.
fn existing_equivalences(model: &Model) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    for component in asserted(model, false) {
        if let Component::EquivalentClasses(EquivalentClasses(members)) = component {
            if let [CE::Class(a), CE::Class(b)] = members.as_slice() {
                out.insert((a.0.to_string(), b.0.to_string()));
                out.insert((b.0.to_string(), a.0.to_string()));
            }
        }
    }
    out
}

fn existing_class_assertions(model: &Model) -> std::collections::HashSet<(String, String)> {
    let mut out = std::collections::HashSet::new();
    for component in asserted(model, false) {
        if let Component::ClassAssertion(ca) = component {
            if let (CE::Class(c), Individual::Named(i)) = (&ca.ce, &ca.i) {
                out.insert((i.0.as_ref().to_string(), c.0.as_ref().to_string()));
            }
        }
    }
    out
}

/// The asserted `(subject, property, object)` assertions between named
/// individuals on named properties, for `--exclude-duplicate-axioms`.
fn existing_object_property_assertions(model: &Model) -> HashSet<(String, String, String)> {
    let mut out = HashSet::new();
    for component in asserted(model, false) {
        if let Component::ObjectPropertyAssertion(pa) = component {
            if let (OPE::ObjectProperty(p), Individual::Named(f), Individual::Named(t)) =
                (&pa.ope, &pa.from, &pa.to)
            {
                out.insert((
                    f.0.as_ref().to_string(),
                    p.0.as_ref().to_string(),
                    t.0.as_ref().to_string(),
                ));
            }
        }
    }
    out
}

/// The classes the root declares, not those its imports declare.
fn own_declared_classes(model: &Model) -> HashSet<String> {
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::DeclareClass(dc) = &ac.component {
            if !model.imported_components.contains(ac) {
                out.insert(dc.0 .0.as_ref().to_string());
            }
        }
    }
    out
}

/// The classes of the signature of `model` and its imports.
fn signature_classes(model: &Model) -> HashSet<String> {
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        for (k, iri) in crate::sig::typed_signature(&ac.component) {
            if k == crate::sig::kind::CLASS {
                out.insert(iri);
            }
        }
    }
    out
}

/// The root of `model` with its axioms removed, and the axioms its imports
/// lent, as `--create-new-ontology` leaves it to take the inferences: its
/// ontology ID and annotations stay, and so do the axioms the imports lent,
/// which still count as asserted and still declare what they declare. Under
/// `--create-new-ontology-with-annotations` its annotation assertions stay as
/// well.
#[allow(clippy::type_complexity)]
fn emptied_root(
    model: &Model,
    annotations: bool,
) -> (Vec<AnnotatedComponent<crate::model::Str>>, std::collections::HashSet<AnnotatedComponent<crate::model::Str>>) {
    let mut kept = Vec::new();
    for ac in model.ont.iter() {
        let keep = model.imported_components.contains(ac)
            || match &ac.component {
                Component::OntologyID(_) | Component::DocIRI(_) | Component::OntologyAnnotation(_) => true,
                Component::AnnotationAssertion(_) => annotations,
                _ => false,
            };
        if keep {
            kept.push(ac.clone());
        }
    }
    (kept, model.imported_components.clone())
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
        let err = ReasonerKind::parse(" Hermitt ").unwrap_err().to_string();
        assert_eq!(err, "INVALID REASONER ERROR unknown reasoner: hermitt");
        // `materialize` and `reduce` take every name but `emr`.
        assert!(ReasonerKind::parse_without_emr("WHELK").is_ok());
        let err = ReasonerKind::parse_without_emr("EMR").unwrap_err().to_string();
        assert_eq!(err, "INVALID REASONER ERROR unknown reasoner: emr");
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
        let c = classify_structural(&m, true, true, Types::None);
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
        let c = classify_structural(&m, false, true, Types::None);
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

/// The moment a logged error is stamped with, `YYYY-MM-DD HH:MM:SS,mmm`.
pub(crate) fn log_stamp() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%d %H:%M:%S,%3N")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0000-00-00 00:00:00,000".to_string())
}

/// A warning on the console, in the log's line format, from `logger`.
pub(crate) fn log_warn(logger: &str, msg: &str) {
    let stamp = log_stamp();
    crate::build::console_line(&format!("{stamp} WARN  {logger} - {msg}"));
}

/// An error on the console, in the log's line format, from `logger`.
pub(crate) fn log_error(logger: &str, msg: &str) {
    let stamp = log_stamp();
    crate::build::console_line(&format!("{stamp} ERROR {logger} - {msg}"));
}
