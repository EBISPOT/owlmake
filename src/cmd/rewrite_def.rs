//! `rewrite-def` — regenerate the textual definitions of an ontology's classes.
//!
//! The FlyBase-family ontologies (dpo, fbbt, fbcv, cl) run this as a *preprocess*
//! step over the edit ontology. It rewrites `IAO:0000115` (definition) literals
//! of two kinds:
//!
//!  * **SUB** (`--sub-definitions`): a definition containing `$sub_PFX:1234` has
//!    the placeholder replaced by the definition text of `obo/PFX_1234` (looked
//!    up across the import closure), merging that term's definition-axiom
//!    annotations. A whole-definition placeholder gets `" (from PFX)."` appended;
//!    a missing target becomes `"No definition for PFX:1234."`.
//!  * **DOT** (`--dot-definitions`): a definition that is a single `"."` is
//!    replaced by prose synthesised from the class's logical (genus–differentia)
//!    `EquivalentClasses` definition — `"<genus> that <prop phrase> <filler> …."`.
//!    With `--null-definitions`, classes lacking any definition get one generated
//!    de novo the same way.
//!
//! `--filter-prefix PFX` restricts rewriting to classes under `obo/PFX`;
//! `--no-ids` drops the `(ID)` suffix in generated labels; `--include-obsolete`
//! processes deprecated classes too; `--add-annotation "PROP VALUE"` /
//! `--add-annotation-iri "PROP IRI"` force-annotate generated axioms.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{
    Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, Build, ClassExpression as CE,
    Component, Literal, MutableOntology, ObjectPropertyExpression as OPE,
};

use crate::model::{Model, Str};

const DEFINITION: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const OBO_ID: &str = "http://www.geneontology.org/formats/oboInOwl#id";
const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";
const OBO_PREFIX: &str = "http://purl.obolibrary.org/obo/";

/// Object properties with a fixed English phrasing in generated definitions, used
/// in place of the property's own label. `true` = emit a trailing "some" after
/// the phrase (the default); `false` = phrase only.
fn property_phrase(iri: &str) -> Option<(&'static str, bool)> {
    let local = iri.strip_prefix(OBO_PREFIX)?;
    Some(match local {
        "BFO_0000050" => ("is part of", true),
        "BFO_0000051" => ("has part", true),
        "BSPO_0000120" => ("is in left side of", true),
        "BSPO_0000121" => ("is in right side of", true),
        "BSPO_0000122" => ("is in posterior side of", true),
        "BSPO_0000123" => ("is in anterior side of", true),
        "BSPO_0000124" => ("is in proximal side of", true),
        "BSPO_0000125" => ("is in distal side of", true),
        "BSPO_0000126" => ("is in lateral side of", true),
        "BSPO_0001100" => ("is superficial part of", true),
        "BSPO_0001101" => ("is in deep part of", true),
        "BSPO_0001106" => ("is proximalmost part of", true),
        "BSPO_0001107" => ("is immediately deep to", true),
        "BSPO_0001108" => ("is distalmost part of", true),
        "BSPO_0015101" => ("is in dorsal side of", true),
        "BSPO_0015102" => ("is in ventral side of", true),
        "BSPO_0020001" => ("is in central side of", true),
        "RO_0001025" => ("is located in", true),
        "RO_0002007" => ("is a bounding layer of", true),
        "RO_0002100" => ("has its soma located in", true),
        "RO_0002103" => ("electrically synapses to", true),
        "RO_0002105" => ("is synapsed via type Ib bouton to", true),
        "RO_0002106" => ("is synapsed via type Is bouton to", true),
        "RO_0002107" => ("is synapsed via type II bouton to", true),
        "RO_0002114" => ("is synapsed via type III bouton to", true),
        "RO_0002150" => ("is continuous with", true),
        "RO_0002160" => ("only exists in", false),
        "RO_0002170" => ("is connected to", true),
        "RO_0002177" => ("is attached to part of", true),
        "RO_0002215" => ("is capable of", true),
        "RO_0002216" => ("is capable of part of", true),
        "RO_0002252" => ("is a connecting branch of", true),
        "RO_0002292" => ("expresses", false),
        "RO_0002371" => ("is attached to", true),
        "RO_0002376" => ("is tributary of", true),
        "RO_0002380" => ("is a branching part of", true),
        "RO_0002473" => ("is composed primarily of", true),
        "RO_0002494" => ("is a transformation of", true),
        "RO_0002571" => ("is lumen of", true),
        "RO_0002572" => ("is luminal space of", true),
        "RO_0002576" => ("is skeleton of", true),
        "RO_0003001" => ("is produced by", true),
        "RO_0013009" => ("sends synaptic output to", true),
        _ => return None,
    })
}

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub format: Option<String>,
    /// Only rewrite definitions for terms under `obo/<PFX>`.
    #[arg(short = 'f', long = "filter-prefix")]
    pub filter_prefix: Option<String>,
    #[arg(long = "include-obsolete")]
    pub include_obsolete: bool,
    #[arg(short = 'd', long = "dot-definitions")]
    pub dot_definitions: bool,
    #[arg(short = 'D', long = "null-definitions")]
    pub null_definitions: bool,
    #[arg(long = "no-ids")]
    pub no_ids: bool,
    #[arg(short = 's', long = "sub-definitions")]
    pub sub_definitions: bool,
    #[arg(long = "add-annotation")]
    pub add_annotation: Vec<String>,
    #[arg(long = "add-annotation-iri")]
    pub add_annotation_iri: Vec<String>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Options for [`rewrite_with_maps`].
#[derive(Default, Clone)]
pub struct RewriteOptions {
    pub sub: bool,
    pub dot: bool,
    pub null_definitions: bool,
    pub include_ids: bool,
    pub include_obsolete: bool,
    pub filter_prefix: Option<String>,
    /// `(property_iri, literal_value)` annotations forced onto generated axioms.
    pub add_annotation: Vec<(String, String)>,
    /// `(property_iri, iri_value)` annotations forced onto generated axioms.
    pub add_annotation_iri: Vec<(String, String)>,
}

impl Args {
    fn options(&self) -> RewriteOptions {
        let split = |v: &[String]| -> Vec<(String, String)> {
            v.iter().filter_map(|s| s.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string()))).collect()
        };
        RewriteOptions {
            sub: self.sub_definitions,
            dot: self.dot_definitions,
            null_definitions: self.null_definitions,
            include_ids: !self.no_ids,
            include_obsolete: self.include_obsolete,
            filter_prefix: self.filter_prefix.clone(),
            add_annotation: split(&self.add_annotation),
            add_annotation_iri: split(&self.add_annotation_iri),
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    // Standalone: look definitions/labels up in the model itself (callers needing
    // cross-import lookups should merge the closure first; the build path walks
    // the closure explicitly).
    let mut maps = Maps::default();
    maps.collect(&model);
    let mut model = rewrite_with_maps(model, &maps, &args.options());
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Label / id / definition lookup tables, populated from a model (and, in the
/// build path, each file of its import closure).
#[derive(Default)]
pub struct Maps {
    labels: HashMap<String, String>,
    ids: HashMap<String, String>,
    /// definition literal + the def axiom's own annotations (for SUB merging).
    defs: HashMap<String, (String, BTreeSet<Annotation<Str>>)>,
}

impl Maps {
    pub fn collect(&mut self, model: &Model) {
        for ac in model.ont.iter() {
            let Component::AnnotationAssertion(aa) = &ac.component else { continue };
            let AnnotationSubject::IRI(subj) = &aa.subject else { continue };
            let subj = subj.as_ref().to_string();
            let prop = aa.ann.ap.0.as_ref();
            let AnnotationValue::Literal(lit) = &aa.ann.av else { continue };
            let text = lit.literal().to_string();
            match prop {
                RDFS_LABEL => { self.labels.entry(subj).or_insert(text); }
                OBO_ID => { self.ids.entry(subj).or_insert(text); }
                DEFINITION => { self.defs.entry(subj).or_insert((text, ac.ann.clone())); }
                _ => {}
            }
        }
    }

    /// A term's label, optionally with a `(ID)` suffix; falls back to id, then IRI.
    fn label(&self, iri: &str, with_id: bool) -> String {
        match (self.labels.get(iri), self.ids.get(iri)) {
            (Some(l), Some(id)) if with_id => format!("{l} ({id})"),
            (Some(l), _) => l.clone(),
            (None, Some(id)) => id.clone(),
            (None, None) => iri.to_string(),
        }
    }
}

/// Rewrite definitions in `model` using the supplied lookup `maps`.
pub fn rewrite_with_maps(mut model: Model, maps: &Maps, opts: &RewriteOptions) -> Model {
    let b = Build::new();
    let filter = opts.filter_prefix.as_ref().map(|p| format!("{OBO_PREFIX}{p}"));
    let in_scope = |iri: &str| filter.as_ref().map(|f| iri.starts_with(f.as_str())).unwrap_or(true);

    // Index the root model: which classes are obsolete, and each class's existing
    // definition axiom(s) and defining (genus–differentia) class expression.
    let mut obsolete: HashSet<String> = HashSet::new();
    let mut def_axioms: HashMap<String, Vec<usize>> = HashMap::new();
    let mut defining_ce: HashMap<String, CE<Str>> = HashMap::new();
    let mut all_classes: BTreeSet<String> = BTreeSet::new();
    let components: Vec<_> = model.ont.iter().cloned().collect();
    for (i, ac) in components.iter().enumerate() {
        match &ac.component {
            Component::AnnotationAssertion(aa) => {
                if let AnnotationSubject::IRI(s) = &aa.subject {
                    let s = s.as_ref().to_string();
                    match aa.ann.ap.0.as_ref() {
                        DEFINITION => def_axioms.entry(s.clone()).or_default().push(i),
                        OWL_DEPRECATED => {
                            if crate::model::asserts_deprecated(&aa.ann.av) {
                                obsolete.insert(s.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Component::DeclareClass(dc) => { all_classes.insert(dc.0.0.as_ref().to_string()); }
            Component::EquivalentClasses(eqc) => {
                // The defining expression is the operand carrying object properties;
                // its sibling is the named class being defined.
                let named: Option<String> = eqc.0.iter().find_map(|ce| match ce {
                    CE::Class(c) => Some(c.0.as_ref().to_string()),
                    _ => None,
                });
                let genus = eqc.0.iter().find(|ce| has_object_property(ce));
                if let (Some(n), Some(g)) = (named, genus) {
                    defining_ce.entry(n).or_insert_with(|| g.clone());
                }
            }
            _ => {}
        }
    }
    for c in def_axioms.keys() {
        all_classes.insert(c.clone());
    }

    let mut remove_idx: HashSet<usize> = HashSet::new();
    let mut additions: Vec<(String, String)> = Vec::new(); // (class iri, new definition)

    for c in &all_classes {
        if !in_scope(c) || (obsolete.contains(c) && !opts.include_obsolete) {
            continue;
        }
        let existing = def_axioms.get(c);
        match existing {
            Some(idxs) => {
                for &i in idxs {
                    let Component::AnnotationAssertion(aa) = &components[i].component else { continue };
                    let AnnotationValue::Literal(lit) = &aa.ann.av else { continue };
                    let old = lit.literal();
                    if let Some(new) = rewrite_one(c, old, opts, maps, &defining_ce) {
                        remove_idx.insert(i);
                        additions.push((c.clone(), new));
                    }
                }
            }
            None if opts.null_definitions && opts.dot => {
                if let Some(new) = generate_dot(c, maps, opts, &defining_ce) {
                    additions.push((c.clone(), new));
                }
            }
            None => {}
        }
    }

    // Apply: drop rewritten axioms, add the regenerated ones (force-annotated).
    let kept: Vec<_> = components
        .iter()
        .enumerate()
        .filter(|(i, _)| !remove_idx.contains(i))
        .map(|(_, ac)| ac.clone())
        .collect();
    let mut ont = crate::model::Onto::new();
    for ac in kept {
        ont.insert(ac);
    }
    let forced = forced_annotations(&b, opts);
    for (c, def) in additions {
        let comp = Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(c.as_str())),
            ann: Annotation { ann: Default::default(),
                ap: b.annotation_property(DEFINITION),
                av: AnnotationValue::Literal(Literal::Simple { literal: def }),
            },
        });
        ont.insert(horned_owl::model::AnnotatedComponent { component: comp, ann: forced.clone() });
    }
    model.ont = ont;
    model
}

fn forced_annotations(b: &Build<Str>, opts: &RewriteOptions) -> BTreeSet<Annotation<Str>> {
    use crate::cmd::babelon::expand_curie;
    let mut set = BTreeSet::new();
    for (p, v) in &opts.add_annotation {
        // Literal value kept verbatim (e.g. a `FBC:Autogenerated` dbxref string).
        set.insert(Annotation { ann: Default::default(),
            ap: b.annotation_property(expand_curie(p).as_str()),
            av: AnnotationValue::Literal(Literal::Simple { literal: v.clone() }),
        });
    }
    for (p, v) in &opts.add_annotation_iri {
        set.insert(Annotation { ann: Default::default(),
            ap: b.annotation_property(expand_curie(p).as_str()),
            av: AnnotationValue::IRI(b.iri(expand_curie(v).as_str())),
        });
    }
    set
}

/// Apply DOT then SUB to one definition literal; `None` = unchanged.
fn rewrite_one(
    c: &str,
    old: &str,
    opts: &RewriteOptions,
    maps: &Maps,
    defining_ce: &HashMap<String, CE<Str>>,
) -> Option<String> {
    if opts.dot && old == "." {
        return generate_dot(c, maps, opts, defining_ce);
    }
    if opts.sub {
        return rewrite_sub(old, maps);
    }
    None
}

/// SUB: replace each `$sub_PFX:1234` with the definition of `obo/PFX_1234`.
fn rewrite_sub(old: &str, maps: &Maps) -> Option<String> {
    let m = find_sub(old)?;
    let (start, end, pfx, num) = m;
    let target = format!("{OBO_PREFIX}{pfx}_{num}");
    let (mut foreign, found) = match maps.defs.get(&target) {
        Some((d, _)) => (d.clone(), true),
        None => (format!("No definition for {pfx}:{num}."), false),
    };
    // Whole-definition placeholder: disambiguate to avoid duplicate definitions.
    if start == 0 && end == old.len() && found {
        if foreign.ends_with('.') {
            foreign.pop();
        }
        foreign = format!("{foreign} (from {pfx}).");
    }
    // Replace every occurrence of the motif, not just the one located above.
    Some(replace_all_subs(old, &foreign))
}

/// Find the first `$sub_PFX:1234` motif: returns (start, end, PFX, NUM).
fn find_sub(s: &str) -> Option<(usize, usize, String, String)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while let Some(rel) = s[i..].find("$sub_") {
        let start = i + rel;
        let mut j = start + 5;
        let p0 = j;
        while j < bytes.len() && bytes[j].is_ascii_alphabetic() { j += 1; }
        if j == p0 || j >= bytes.len() || bytes[j] != b':' { i = start + 5; continue; }
        let pfx = s[p0..j].to_string();
        j += 1;
        let n0 = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() { j += 1; }
        if j == n0 { i = start + 5; continue; }
        let num = s[n0..j].to_string();
        return Some((start, j, pfx, num));
    }
    None
}

/// Replace every `$sub_PFX:1234` motif with `repl`.
fn replace_all_subs(s: &str, repl: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some((start, end, _, _)) = find_sub(rest) {
        out.push_str(&rest[..start]);
        out.push_str(repl);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// DOT: synthesise prose from the class's genus–differentia definition.
fn generate_dot(
    c: &str,
    maps: &Maps,
    opts: &RewriteOptions,
    defining_ce: &HashMap<String, CE<Str>>,
) -> Option<String> {
    let ce = defining_ce.get(c)?;
    let mut items: Vec<String> = Vec::new();
    write_ce(ce, maps, opts.include_ids, &mut items, None);
    Some(format!("{}.", items.join(" ")))
}

/// Whether a class expression mentions an object property (i.e. is a genus–
/// differentia definition rather than the named class itself).
fn has_object_property(ce: &CE<Str>) -> bool {
    match ce {
        CE::Class(_) => false,
        CE::ObjectSomeValuesFrom { .. }
        | CE::ObjectAllValuesFrom { .. }
        | CE::ObjectHasValue { .. }
        | CE::ObjectMinCardinality { .. }
        | CE::ObjectMaxCardinality { .. }
        | CE::ObjectExactCardinality { .. }
        | CE::ObjectHasSelf(_) => true,
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => v.iter().any(has_object_property),
        CE::ObjectComplementOf(b) => has_object_property(b),
        _ => false,
    }
}

/// Walk the class expression, appending phrase fragments in reading order.
/// `parent` carries the enclosing expression kind (so a class operand of an
/// intersection prepends "Any"/"is a(n)").
fn write_ce(ce: &CE<Str>, maps: &Maps, with_id: bool, items: &mut Vec<String>, parent: Option<&'static str>) {
    match ce {
        CE::ObjectIntersectionOf(operands) => {
            for (i, op) in operands.iter().enumerate() {
                write_ce(op, maps, with_id, items, Some("intersection"));
                if i == 0 {
                    items.push("that".to_string());
                } else if i < operands.len() - 1 {
                    items.push("and".to_string());
                }
            }
        }
        CE::ObjectSomeValuesFrom { ope, bce } => {
            let piri = ope_iri(ope);
            match property_phrase(&piri) {
                Some((phrase, emit_some)) => {
                    items.push(if emit_some { format!("{phrase} some") } else { phrase.to_string() });
                }
                None => {
                    items.push(maps.label(&piri, false).replace('_', " "));
                    items.push("some".to_string());
                }
            }
            write_ce(bce, maps, with_id, items, Some("some"));
        }
        CE::Class(c) => {
            if parent == Some("intersection") {
                items.push(if items.is_empty() { "Any".to_string() } else { "is a(n)".to_string() });
            }
            items.push(maps.label(c.0.as_ref(), with_id));
        }
        _ => {}
    }
}

fn ope_iri(ope: &OPE<Str>) -> String {
    match ope {
        OPE::ObjectProperty(p) => p.0.as_ref().to_string(),
        OPE::InverseObjectProperty(p) => p.0.as_ref().to_string(),
    }
}
