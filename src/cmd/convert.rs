//! `convert` — translate an ontology between serialization formats.
//!
//! The input is read, optionally re-formatted, and written to `--output` (format
//! inferred from the output extension unless `--format` is given).

use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{AnnotationValue, ClassExpression as CE, Component, MutableOntology};

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

    /// Check OBO document structure on write (`<bool>`, default true). When
    /// false, OBO-structure strictness is suppressed (the OBO writer is already
    /// lenient and never errors, so this controls only the OBO strictness gate).
    #[arg(short = 'c', long, num_args = 1, default_missing_value = "true")]
    pub check: Option<bool>,

    /// Options for clean OBO output, comma-separated. Recognized tokens:
    /// `true`, `drop-untranslatable-axioms`, `drop-gci-axioms`. When set and the
    /// output format is OBO, drop axioms that do not translate to OBO before
    /// writing.
    #[arg(long)]
    pub clean_obo: Option<String>,

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
    let mut model = crate::cmd::take_or_load_no_imports(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    // `--check` (default true): run OBO document-structure checks when the
    // output is OBO, reporting issues to stderr. `--check false` skips them. The
    // OBO writer itself is lenient (it never errors), so this is the gate for
    // structural diagnostics.
    if args.check.unwrap_or(true) {
        let writing_obo = match (args.format.as_deref(), args.output.as_deref()) {
            (Some(name), _) => {
                crate::io::Format::from_name(name).map(|f| f == crate::io::Format::Obo).unwrap_or(false)
            }
            (None, Some(path)) => {
                crate::io::Format::from_path(path).map(|f| f == crate::io::Format::Obo).unwrap_or(false)
            }
            (None, None) => false,
        };
        if writing_obo {
            obo_structure_check(&model);
        }
    }

    // `--clean-obo`: drop axioms that don't translate to OBO before writing OBO
    // output. Cleaning only applies when the output format is OBO.
    if let Some(spec) = &args.clean_obo {
        let writing_obo = match (args.format.as_deref(), args.output.as_deref()) {
            (Some(name), _) => crate::io::Format::from_name(name)
                .map(|f| f == crate::io::Format::Obo)
                .unwrap_or(false),
            (None, Some(path)) => crate::io::Format::from_path(path)
                .map(|f| f == crate::io::Format::Obo)
                .unwrap_or(false),
            (None, None) => false,
        };
        if writing_obo {
            apply_clean_obo(&mut model, spec);
        }
    }

    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// `convert --check` (OBO output): report OBO document-structure problems
/// to stderr — a missing ontology id, and `is_a`/`relationship` axioms pointing
/// at classes the document never declares (dangling references). Diagnostic only
/// (the writer stays lenient); `--check false` skips this.
fn obo_structure_check(model: &crate::model::Model) {
    use std::collections::HashSet;
    let mut declared: HashSet<String> = HashSet::new();
    let mut has_id = false;
    for ac in model.ont.iter() {
        match &ac.component {
            Component::DeclareClass(d) => {
                declared.insert(d.0 .0.to_string());
            }
            Component::OntologyID(_) => has_id = true,
            _ => {}
        }
    }
    let mut dangling = 0usize;
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            for ce in [&sc.sub, &sc.sup] {
                if let CE::Class(c) = ce {
                    if !declared.contains(c.0.as_ref()) {
                        dangling += 1;
                    }
                }
            }
        }
    }
    if !has_id {
        status!("convert: check: ontology has no ontology IRI (OBO `ontology:` header)");
    }
    if dangling > 0 {
        status!("convert: check: {dangling} is_a/relationship reference(s) to undeclared classes");
    }
}

/// The `--clean-obo` option set, resolved from its keyword list.
///
/// The keywords are read left to right, each one turning options on (and, for the
/// two comment options, turning the other off), so the LAST word about comments
/// wins: `'simple merge-comments'` merges them where `'merge-comments simple'`
/// drops the extras instead.
#[derive(Default, Clone, Copy)]
struct CleanOptions {
    /// Keep one `rdfs:label` per subject.
    drop_extra_labels: bool,
    /// Keep one `IAO:0000115` definition per subject.
    drop_extra_definitions: bool,
    /// Keep one `rdfs:comment` per subject.
    drop_extra_comments: bool,
    /// Join a subject's comments into one.
    merge_comments: bool,
    /// Drop the axioms that would otherwise go to the `owl-axioms:` header.
    drop_untranslatable: bool,
    /// Drop general concept inclusion axioms.
    drop_gci: bool,
}

/// Resolve `--clean-obo <spec>`. The spec is a list of keywords, written either
/// space-separated (`'simple merge-comments'`) or comma-separated, so split on
/// both. Recognized:
///
/// - `drop-extra-labels`, `drop-extra-definitions`, `drop-extra-comments`,
///   `merge-comments`, `drop-untranslatable-axioms`, `drop-gci-axioms` — the
///   individual options.
/// - `strict` / `true` — the three `drop-extra-*` options.
/// - `simple` — those three plus `drop-untranslatable-axioms` and
///   `drop-gci-axioms`.
///
/// Anything else is ignored.
fn clean_options(spec: &str) -> CleanOptions {
    let mut o = CleanOptions::default();
    let mut drop_extras = |o: &mut CleanOptions| {
        o.drop_extra_labels = true;
        o.drop_extra_definitions = true;
        o.drop_extra_comments = true;
    };
    for token in spec.split([' ', ',']).filter(|t| !t.is_empty()) {
        match token {
            "drop-extra-labels" => o.drop_extra_labels = true,
            "drop-extra-definitions" => o.drop_extra_definitions = true,
            "drop-extra-comments" => {
                o.merge_comments = false;
                o.drop_extra_comments = true;
            }
            "merge-comments" => {
                o.drop_extra_comments = false;
                o.merge_comments = true;
            }
            "drop-untranslatable-axioms" => o.drop_untranslatable = true,
            "drop-gci-axioms" => o.drop_gci = true,
            "simple" => {
                o.drop_untranslatable = true;
                o.drop_gci = true;
                drop_extras(&mut o);
            }
            "strict" | "true" => drop_extras(&mut o),
            _ => {}
        }
    }
    o
}

/// Apply `convert --clean-obo <spec>` to `model` (already known to be written as
/// OBO).
pub fn apply_clean_obo(model: &mut crate::model::Model, spec: &str) {
    let o = clean_options(spec);
    clean_obo(model, o.drop_untranslatable, o.drop_gci);
    model.obo_drop_untranslatable |= o.drop_untranslatable;
    // Supernumerary single-valued annotations go BEFORE the comment merge: a
    // subject left with one comment has nothing to merge.
    let mut single: Vec<&str> = Vec::new();
    if o.drop_extra_labels {
        single.push(RDFS_LABEL);
    }
    if o.drop_extra_definitions {
        single.push(IAO_DEFINITION);
    }
    if o.drop_extra_comments {
        single.push(RDFS_COMMENT);
    }
    if !single.is_empty() {
        drop_single_valued_extras(model, &single);
    }
    if o.merge_comments {
        merge_comments(model);
    }
}

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const IAO_DEFINITION: &str = "http://purl.obolibrary.org/obo/IAO_0000115";

/// Keep ONE annotation of each named property per subject, dropping the rest.
///
/// An OBO frame's `name:`, `def:` and `comment:` are each single-valued, so a
/// subject carrying two `rdfs:label`s, two `IAO:0000115` definitions or two
/// `rdfs:comment`s must lose all but one before the frame is written.
///
/// The survivor is chosen by language first: if exactly ONE of the assertions is
/// language-agnostic — a non-literal value, or a literal with no language tag —
/// that one is kept whatever the values say, which is what makes a
/// multi-lingual ontology keep its untagged English label. Otherwise the
/// assertions are put in axiom order and the first is kept, so two untagged
/// definitions resolve to the lexically smaller one.
///
/// Dropping the axiom rather than filtering at write time also fixes the `!
/// label` comments, which must name the surviving label: `is_a: BFO:0000017 !
/// realizable`, not `! realizable entity`.
fn drop_single_valued_extras(model: &mut crate::model::Model, properties: &[&str]) {
    use horned_owl::model::MutableOntology;
    let mut groups: std::collections::BTreeMap<
        String,
        Vec<horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>>,
    > = Default::default();
    for ac in model.ont.iter() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            let prop = aa.ann.ap.0.as_ref();
            if !properties.contains(&prop) {
                continue;
            }
            let key = format!("{}\u{1}{prop}", subject_key(&aa.subject));
            groups.entry(key).or_default().push(ac.clone());
        }
    }
    for acs in groups.into_values() {
        if acs.len() < 2 {
            continue;
        }
        let agnostic: Vec<usize> =
            (0..acs.len()).filter(|&i| !is_language_tagged(value_of(&acs[i]))).collect();
        let keep = if agnostic.len() == 1 {
            agnostic[0]
        } else {
            (0..acs.len())
                .min_by_key(|&i| (axiom_order(value_of(&acs[i])), annotation_order(&acs[i])))
                .unwrap_or(0)
        };
        for (i, ac) in acs.iter().enumerate() {
            if i != keep {
                model.ont.remove(ac);
            }
        }
    }
}

/// The value of an annotation assertion.
fn value_of(
    ac: &horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>,
) -> &AnnotationValue<horned_owl::model::RcStr> {
    match &ac.component {
        Component::AnnotationAssertion(aa) => &aa.ann.av,
        _ => unreachable!("only annotation assertions are grouped"),
    }
}

/// Whether an annotation value carries a language tag. Anything else — a plain or
/// typed literal, an IRI, an anonymous individual — is language-agnostic.
fn is_language_tagged(v: &AnnotationValue<horned_owl::model::RcStr>) -> bool {
    matches!(
        v,
        AnnotationValue::Literal(horned_owl::model::Literal::Language { .. })
    )
}

/// The sort key that orders two annotation assertions sharing a subject and a
/// property: an IRI value before a literal before an anonymous individual, and
/// literals by (datatype, lexical form, language).
fn axiom_order(v: &AnnotationValue<horned_owl::model::RcStr>) -> (u8, String, String, String) {
    use horned_owl::model::Literal;
    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
    const PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
    match v {
        AnnotationValue::IRI(i) => (0, String::new(), i.as_ref().to_string(), String::new()),
        AnnotationValue::Literal(l) => {
            let (dt, lang) = match l {
                Literal::Simple { .. } => (XSD_STRING.to_string(), String::new()),
                Literal::Language { lang, .. } => (PLAIN_LITERAL.to_string(), lang.clone()),
                Literal::Datatype { datatype_iri, .. } => {
                    (datatype_iri.as_ref().to_string(), String::new())
                }
            };
            (1, dt, l.literal().clone(), lang)
        }
        AnnotationValue::AnonymousIndividual(a) => {
            (2, String::new(), a.0.as_ref().to_string(), String::new())
        }
    }
}

/// The tie-break when two assertions carry the SAME value: their own axiom
/// annotations, in order. A definition reified twice — once `hasDbXref
/// "GOC:curators"` and once `"GOC:go_curators"` — resolves to the first of those
/// annotation sets, so the `def:` clause stays put across runs.
fn annotation_order(
    ac: &horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>,
) -> Vec<(String, (u8, String, String, String))> {
    let mut v: Vec<(String, (u8, String, String, String))> = ac
        .ann
        .iter()
        .map(|a| (a.ap.0.as_ref().to_string(), axiom_order(&a.av)))
        .collect();
    v.sort();
    v
}

/// `--clean-obo merge-comments`: every `rdfs:comment` on a subject becomes one
/// assertion whose value is the comments joined by a single space in axiom order,
/// unioning their axiom annotations. A subject with a non-literal comment is left
/// alone — there is no text to join.
fn merge_comments(model: &mut crate::model::Model) {
    use horned_owl::model::{Annotation, AnnotationAssertion, Literal};

    let acs: Vec<_> = model.ont.iter().cloned().collect();
    let mut by_subject: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (i, ac) in acs.iter().enumerate() {
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() == RDFS_COMMENT {
                by_subject.entry(subject_key(&aa.subject)).or_default().push(i);
            }
        }
    }
    let mut drop: std::collections::HashSet<usize> = Default::default();
    let mut add = Vec::new();
    for idxs in by_subject.values() {
        if idxs.len() < 2 {
            continue;
        }
        if idxs.iter().any(|&i| !matches!(value_of(&acs[i]), AnnotationValue::Literal(_))) {
            continue;
        }
        let mut parts: Vec<usize> = idxs.clone();
        parts.sort_by_key(|&i| (axiom_order(value_of(&acs[i])), annotation_order(&acs[i])));
        let joined = parts
            .iter()
            .map(|&i| lexical(value_of(&acs[i])))
            .collect::<Vec<_>>()
            .join(" ");
        let mut ann = std::collections::BTreeSet::new();
        for &i in idxs {
            ann.extend(acs[i].ann.iter().cloned());
        }
        let (subject, ap) = match &acs[idxs[0]].component {
            Component::AnnotationAssertion(aa) => (aa.subject.clone(), aa.ann.ap.clone()),
            _ => unreachable!(),
        };
        idxs.iter().for_each(|&i| {
            drop.insert(i);
        });
        add.push(horned_owl::model::AnnotatedComponent {
            component: Component::AnnotationAssertion(AnnotationAssertion {
                subject,
                ann: Annotation { ann: Default::default(), ap, av: AnnotationValue::Literal(Literal::Simple { literal: joined }) },
            }),
            ann,
        });
    }
    if drop.is_empty() {
        return;
    }
    let mut ont = horned_owl::ontology::set::SetOntology::new();
    for (i, ac) in acs.into_iter().enumerate() {
        if !drop.contains(&i) {
            ont.insert(ac);
        }
    }
    for ac in add {
        ont.insert(ac);
    }
    model.ont = ont;
}

fn subject_key(s: &horned_owl::model::AnnotationSubject<crate::model::Str>) -> String {
    use horned_owl::model::AnnotationSubject;
    match s {
        AnnotationSubject::IRI(i) => i.as_ref().to_string(),
        AnnotationSubject::AnonymousIndividual(a) => format!("_:{}", a.0.as_ref()),
    }
}

fn lexical(v: &AnnotationValue<crate::model::Str>) -> String {
    match v {
        AnnotationValue::Literal(l) => match l {
            horned_owl::model::Literal::Simple { literal }
            | horned_owl::model::Literal::Language { literal, .. }
            | horned_owl::model::Literal::Datatype { literal, .. } => literal.clone(),
        },
        _ => String::new(),
    }
}

/// Drop axioms that the OBO writer cannot represent (and, when requested, GCI
/// axioms whose SubClassOf subject is a class expression rather than a named
/// class) — the work behind `--clean-obo drop-untranslatable-axioms` /
/// `drop-gci-axioms`. The set of OBO-translatable axioms tracks what
/// [`crate::io::obo::save`] actually renders.
fn clean_obo(model: &mut crate::model::Model, drop_untranslatable: bool, drop_gci: bool) {
    let before = model.ont.iter().count();
    // `drop-untranslatable-axioms` removes exactly what the OBO writer would
    // otherwise divert into the `owl-axioms:` header — NOT everything with no
    // dedicated OBO tag. Use the writer's own collector as the authority.
    let untranslatable = if drop_untranslatable {
        crate::io::obo::untranslatable_axioms(model)
    } else {
        Default::default()
    };
    let kept: Vec<_> = model
        .ont
        .iter()
        .filter(|ac| {
            // A GCI is a SubClassOf whose subject is a non-named class expression.
            let is_gci = matches!(
                &ac.component,
                Component::SubClassOf(sc) if !matches!(sc.sub, CE::Class(_))
            );
            if drop_gci && is_gci {
                return false;
            }
            if drop_untranslatable && untranslatable.contains(*ac) {
                return false;
            }
            true
        })
        .cloned()
        .collect();
    let mut ont = horned_owl::ontology::set::SetOntology::new();
    for ac in kept {
        ont.insert(ac);
    }
    let after = ont.iter().count();
    model.ont = ont;
    status!("convert: clean-obo dropped {} axiom(s)", before.saturating_sub(after));
}

