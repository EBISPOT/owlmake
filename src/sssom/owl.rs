//! SSSOM commands that act on the OWL ontology flowing through a command chain
//! (rather than on a standalone mapping set):
//!
//! - `sssom:xref-extract` — harvest `oboInOwl:hasDbXref` annotations into a SSSOM
//!   mapping set, written to `--mapping-file`.
//! - `sssom:inject` — generate OWL axioms from a mapping set by interpreting a
//!   SSSOM/T ruleset (the transformation language), optionally dispatching the
//!   generated axioms to per-file bridge ontologies via a dispatch table.
//!
//! - `sssom:rename` — rewrite entity IRIs from a mapping set's
//!   `subject_id → object_id` pairs.
//!
//! These three are the whole `sssom:` surface that acts on an ontology. Each may
//! be invoked mid-chain (`merge … sssom:inject …`), in which case owlmake runs the
//! preceding chain to obtain the in-flight ontology and hands it here, or on its
//! own with `-i`/`-I`, in which case this module loads the ontology itself.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationProperty, AnnotationSubject,
    AnnotationValue, ClassExpression as CE, Component, EquivalentClasses, Literal, MutableOntology,
    SubClassOf,
};

use super::{io as sio, MappingSet};
use crate::model::{Model, Str};

const OIO: &str = "http://www.geneontology.org/formats/oboInOwl#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const HAS_DBXREF: &str = "http://www.geneontology.org/formats/oboInOwl#hasDbXref";
const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";
const MAPPING_JUSTIFICATION: &str = "https://w3id.org/semapv/vocab/UnspecifiedMatching";

/// Dispatch a chained `sssom:<sub>` command over the in-flight ontology.
pub fn chain_step(model: Option<Model>, sub: &str, args: &[String]) -> Result<()> {
    match sub {
        "xref-extract" => xref_extract(model, args),
        "inject" => inject(model, args),
        "rename" => {
            // Terminal `sssom:rename` with its own `-o`. (When followed by more
            // chain commands it is handled as a *producing* step in `run_chain`.)
            let mut m = rename(model, args)?;
            let opts = parse_opts(args, &[("output", &["-o", "--output"]), ("format", &["--format"])]);
            if let Some(o) = opts.one("output") {
                match opts.one("format") {
                    Some(f) => crate::io::save_as(&mut m, Path::new(o), crate::io::Format::from_name(f)?)?,
                    None => crate::io::save(&mut m, Path::new(o))?,
                }
            }
            Ok(())
        }
        other => bail!("sssom:{other} is not supported in a ROBOT chain"),
    }
}

/// `sssom:rename` — rewrite entity IRIs in the ontology using a SSSOM mapping
/// set: each `subject_id → object_id` mapping renames the subject IRI to the
/// object IRI (CURIEs expanded against the set's prefix map). Used by UBERON to
/// correct import IRIs (`import-corrections.sssom.tsv`) before extracting the
/// `local-emapa`/`local-ma`/`local-xao` modules. Returns the renamed model so it
/// can produce the in-flight ontology for the rest of a command chain.
pub fn rename(model: Option<Model>, args: &[String]) -> Result<Model> {
    let opts = parse_opts(
        args,
        &[
            ("input", &["-i", "--input"]),
            ("sssom", &["--sssom"]),
            ("format", &["--format"]),
        ],
    );
    let m = load_model(model, &opts)?;
    let mut map: HashMap<String, String> = HashMap::new();
    for f in opts.many("sssom") {
        let ms = sio::read_path(Path::new(f), None, None)
            .with_context(|| format!("reading mapping set {f}"))?;
        let prefixes = ms.effective_prefixes();
        for mp in &ms.mappings {
            if let (Some(s), Some(o)) = (mp.get("subject_id"), mp.get("object_id")) {
                map.insert(expand(&prefixes, s), expand(&prefixes, o));
            }
        }
    }
    crate::cmd::rename::rename_model(m, &map)
}

// ─────────────────────────────── arg parsing ────────────────────────────────

/// A tiny option parser: `valued` keys consume the next token (repeatable),
/// everything else is a boolean flag. Aliases share a canonical key.
struct Opts {
    vals: HashMap<String, Vec<String>>,
    flags: HashSet<String>,
}

impl Opts {
    fn one(&self, key: &str) -> Option<&str> {
        self.vals.get(key).and_then(|v| v.first()).map(String::as_str)
    }
    fn many(&self, key: &str) -> &[String] {
        self.vals.get(key).map(Vec::as_slice).unwrap_or(&[])
    }
    fn has(&self, key: &str) -> bool {
        self.flags.contains(key)
    }
}

/// Options every `sssom:` step accepts, as every command does: the input the
/// step loads when it is not fed by a preceding chain command, and the catalog
/// that resolves that input's imports.
const SHARED_OPTS: &[(&str, &[&str])] = &[
    ("input", &["-i", "--input"]),
    ("input_iri", &["-I", "--input-iri"]),
    ("catalog", &["--catalog"]),
];

/// Parse `args`. `valued` lists `(canonical, &[aliases])` flags that take a value.
fn parse_opts(args: &[String], valued: &[(&str, &[&str])]) -> Opts {
    let mut vals: HashMap<String, Vec<String>> = HashMap::new();
    let mut flags: HashSet<String> = HashSet::new();
    let lookup = |tok: &str| -> Option<&str> {
        valued
            .iter()
            .chain(SHARED_OPTS.iter())
            .find(|(_, al)| al.contains(&tok))
            .map(|(c, _)| *c)
    };
    let mut i = 0;
    while i < args.len() {
        let tok = &args[i];
        if let Some(canon) = lookup(tok) {
            if i + 1 < args.len() {
                vals.entry(canon.to_string()).or_default().push(args[i + 1].clone());
                i += 2;
                continue;
            }
        }
        if let Some(stripped) = tok.strip_prefix("--") {
            flags.insert(stripped.to_string());
        } else if let Some(stripped) = tok.strip_prefix('-') {
            flags.insert(stripped.to_string());
        }
        i += 1;
    }
    Opts { vals, flags }
}

fn load_model(model: Option<Model>, opts: &Opts) -> Result<Model> {
    if let Some(m) = model {
        return Ok(m);
    }
    // A standalone `sssom:` call takes its ontology from a file or, as CL does
    // when it builds `zfa.sssom.tsv`, straight from an IRI
    // (`sssom:xref-extract -I http://…/zfa.owl`). Either way the import closure is
    // resolved as it is for every other command, honouring `--catalog`.
    let common = crate::cmd::CommonArgs {
        catalog: opts.one("catalog").map(PathBuf::from),
        ..Default::default()
    };
    if let Some(iri) = opts.one("input_iri") {
        let mut m = crate::io::load_iri(iri, None)?;
        common.apply_catalog(&mut m, None)?;
        return Ok(m);
    }
    let input = opts
        .one("input")
        .context("sssom: no piped ontology and no --input given")?;
    let path = Path::new(input);
    let mut m = crate::io::load_with(path, None)?;
    common.apply_catalog(&mut m, Some(path))?;
    Ok(m)
}

// ─────────────────────────────── prefix maps ────────────────────────────────

/// Expand a CURIE against `prefixes`, falling back to the OBO PURL convention.
pub(crate) fn expand(prefixes: &BTreeMap<String, String>, curie: &str) -> String {
    if curie.starts_with("http://") || curie.starts_with("https://") || curie.starts_with("urn:") {
        return curie.to_string();
    }
    if let Some((p, local)) = curie.split_once(':') {
        if let Some(base) = prefixes.get(p) {
            return format!("{base}{local}");
        }
        return crate::io::obo::expand_id(curie);
    }
    curie.to_string()
}

/// Compress an IRI to a CURIE against `prefixes` (longest namespace wins).
fn compress(prefixes: &BTreeMap<String, String>, iri: &str) -> String {
    let mut best: Option<(&str, &str)> = None;
    for (p, base) in prefixes {
        if iri.starts_with(base.as_str())
            && best.map(|(_, b)| base.len() > b.len()).unwrap_or(true)
        {
            best = Some((p, base));
        }
    }
    match best {
        Some((p, base)) => format!("{p}:{}", &iri[base.len()..]),
        None => iri.to_string(),
    }
}

// ───────────────────────────────── xref-extract ─────────────────────────────

fn lit_text(l: &Literal<Str>) -> String {
    match l {
        Literal::Simple { literal } => literal.clone(),
        Literal::Language { literal, .. } => literal.clone(),
        Literal::Datatype { literal, .. } => literal.clone(),
    }
}

/// Build a prefix→predicate map from the ontology's `treat-xrefs-as-*` header
/// annotations: each tag names a prefix and fixes the mapping predicate an xref
/// to that prefix stands for.
fn treat_xrefs_predicates(model: &Model) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut put = |key: &str, val: &str, pred: &str| {
        if key.ends_with("genus-differentia") {
            // value is "PREFIX part_of NCBITaxon:X" → take the prefix.
            if let Some(pfx) = val.split_whitespace().next() {
                map.insert(pfx.to_string(), pred.to_string());
            }
        } else {
            map.insert(val.trim().to_string(), pred.to_string());
        }
    };
    for ac in model.ont.iter() {
        let Component::OntologyAnnotation(oa) = &ac.component else { continue };
        let prop = oa.0.ap.0.as_ref();
        let AnnotationValue::Literal(l) = &oa.0.av else { continue };
        let v = lit_text(l);
        match prop.strip_prefix(OIO) {
            Some("treat-xrefs-as-equivalent") => put(prop, &v, "skos:exactMatch"),
            Some("treat-xrefs-as-is_a") => put(prop, &v, "skos:broadMatch"),
            Some("treat-xrefs-as-has-subclass") => put(prop, &v, "skos:narrowMatch"),
            Some("treat-xrefs-as-genus-differentia")
            | Some("treat-xrefs-as-reverse-genus-differentia") => {
                put("treat-xrefs-as-genus-differentia", &v, "semapv:crossSpeciesExactMatch")
            }
            _ => {}
        }
    }
    map
}

/// Map IRI → first `rdfs:label`, and the set of classes carrying `owl:deprecated true`.
fn labels_and_obsoletes(model: &Model) -> (HashMap<String, String>, HashSet<String>) {
    let mut labels = HashMap::new();
    let mut obsolete = HashSet::new();
    for ac in model.ont.iter() {
        let Component::AnnotationAssertion(aa) = &ac.component else { continue };
        let AnnotationSubject::IRI(s) = &aa.subject else { continue };
        let prop = aa.ann.ap.0.as_ref();
        if prop == RDFS_LABEL {
            if let AnnotationValue::Literal(l) = &aa.ann.av {
                labels.entry(s.as_ref().to_string()).or_insert_with(|| lit_text(l));
            }
        } else if prop == OWL_DEPRECATED && crate::model::asserts_deprecated(&aa.ann.av) {
            obsolete.insert(s.as_ref().to_string());
        }
    }
    (labels, obsolete)
}

fn ontology_iri(model: &Model) -> Option<String> {
    model.ont.iter().find_map(|ac| match &ac.component {
        Component::OntologyID(id) => id.iri.as_ref().map(|i| i.as_ref().to_string()),
        _ => None,
    })
}

/// The prefix map an xref is resolved against: the bundled OBO context. A prefix
/// it does not name is not a prefix the extractor can resolve, and its xrefs are
/// DROPPED — UBERON declares `treat-xrefs-as-*` for `DHBA`, `HBA`, `KUPO`,
/// `OGES`, `PBA` and `SCTID` as well, and none of their 5,563 xrefs reach the
/// mapping set.
///
/// A mirror's own prefix map is deliberately not consulted: it binds `obo:` to
/// the whole `…/obo/` space, which would compress every subject to
/// `obo:UBERON_0000001` instead of the `UBERON:0000001` form a mapping set is
/// expected to carry.
fn xref_prefixes() -> &'static BTreeMap<String, String> {
    static MAP: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| crate::report::obo_context_prefixes().into_iter().collect())
}

/// Split an entity IRI into `(prefix, local)` against `prefixes`, longest
/// namespace first, for the subject column.
fn as_curie(prefixes: &BTreeMap<String, String>, iri: &str) -> Option<(String, String)> {
    let mut best: Option<(&String, &String)> = None;
    for (p, ns) in prefixes {
        if iri.starts_with(ns.as_str()) && best.map(|(_, b)| ns.len() > b.len()).unwrap_or(true) {
            best = Some((p, ns));
        }
    }
    let (p, ns) = best?;
    Some((p.clone(), iri[ns.len()..].to_string()))
}

/// An ontology annotation's value as an IRI, for the set-level metadata.
fn ontology_annotation_iri(model: &Model, prop: &str) -> Option<String> {
    model.ont.iter().find_map(|ac| match &ac.component {
        Component::OntologyAnnotation(oa) if oa.0.ap.0.as_ref() == prop => match &oa.0.av {
            AnnotationValue::IRI(i) => Some(i.as_ref().to_string()),
            AnnotationValue::Literal(l) => Some(lit_text(l)),
            _ => None,
        },
        _ => None,
    })
}

fn xref_extract(model: Option<Model>, args: &[String]) -> Result<()> {
    let opts = parse_opts(
        args,
        &[
            ("mapping_file", &["--mapping-file"]),
            ("output", &["-o", "--output"]),
            ("prefix", &["--prefix", "--add-prefix"]),
            ("map_pred", &["--map-prefix-to-predicate"]),
            ("set_id", &["--set-id"]),
        ],
    );
    let model = load_model(model, &opts)?;

    // The bundled OBO context, with any `--prefix` declaration overriding it.
    let mut prefixes = xref_prefixes().clone();
    for spec in opts.many("prefix") {
        if let Some((p, base)) = spec.split_once(':') {
            prefixes.insert(p.trim().to_string(), base.trim().to_string());
        }
    }

    // prefix → predicate map from treat-xrefs-as-* plus --map-prefix-to-predicate.
    //
    // `--ignore-treat-xrefs` drops the ontology's own `treat-xrefs-as-*` tags, so
    // only the prefixes named on the command line yield mappings. That is how a
    // caller extracts one ontology's cross-references to a single target: ZFA
    // declares `treat-xrefs-as-equivalent` for TAO, CARO and VSAO, and asking for
    // `CL` alone means the other three prefixes contribute nothing.
    let mut pred_map = if opts.has("ignore-treat-xrefs") {
        std::collections::BTreeMap::new()
    } else {
        treat_xrefs_predicates(&model)
    };
    for spec in opts.many("map_pred") {
        let mut it = spec.split_whitespace();
        if let (Some(p), Some(pred)) = (it.next(), it.next()) {
            pred_map.insert(p.to_string(), builtin_curie(pred));
        }
    }
    let all_xrefs = opts.has("all-xrefs");
    let include_obsoletes = opts.has("include-obsoletes");

    let (labels, obsolete) = labels_and_obsoletes(&model);

    // Only CLASSES yield mappings. An `xref:` on a Typedef is a cross-reference of
    // the relation, not a term mapping, and extracting them added 35 rows to
    // UBERON's `uberon-local.sssom.tsv` that the reference does not have — every
    // one of them a self-mapping (`BSPO:0000096 → BSPO:0000096`) from a property
    // whose stanza xrefs itself, and between them the only reason the file
    // declared a `BSPO:` prefix at all.
    // …and only the classes and assertions the INPUT ontology itself carries.
    // An import is loaded (the catalog resolves it, and the labels come from it)
    // but its content is not this ontology's cross-references: extracting over
    // the closure gave CL's local set 14,838 UBERON rows and a curie map naming
    // prefixes CL never cross-references.
    let root = |ac: &AnnotatedComponent<Str>| !model.imported_components.contains(ac);
    let classes: HashSet<String> = model
        .ont
        .iter()
        .filter(|ac| root(ac))
        .filter_map(|ac| match &ac.component {
            Component::DeclareClass(d) => Some(d.0 .0.as_ref().to_string()),
            _ => None,
        })
        .collect();

    let mut ms = MappingSet::new();
    let mut used_prefixes: BTreeSet<String> = BTreeSet::new();
    // (subject IRI, predicate IRI, object IRI) alongside each row: the file is
    // ordered on the EXPANDED forms, which is why every `skos:` predicate
    // (`http://…`) precedes `semapv:crossSpeciesExactMatch` (`https://…`).
    let mut keyed: Vec<((String, String, String), super::Mapping)> = Vec::new();

    for ac in model.ont.iter() {
        if !root(ac) {
            continue;
        }
        let Component::AnnotationAssertion(aa) = &ac.component else { continue };
        if aa.ann.ap.0.as_ref() != HAS_DBXREF {
            continue;
        }
        let AnnotationSubject::IRI(subj) = &aa.subject else { continue };
        let subject_iri = subj.as_ref().to_string();
        if !classes.contains(&subject_iri) {
            continue;
        }
        if !include_obsoletes && obsolete.contains(&subject_iri) {
            continue;
        }
        let Some((subject_prefix, subject_local)) = as_curie(&prefixes, &subject_iri) else {
            continue;
        };
        let AnnotationValue::Literal(lit) = &aa.ann.av else { continue };
        let xref = lit_text(lit);
        let Some((pfx, local)) = xref.split_once(':') else { continue };
        let Some(object_ns) = prefixes.get(pfx) else { continue };
        let Some(predicate) = pred_map.get(pfx).cloned().or_else(|| {
            all_xrefs.then(|| "oboInOwl:hasDbXref".to_string())
        }) else {
            continue;
        };
        used_prefixes.insert(pfx.to_string());
        used_prefixes.insert(subject_prefix.clone());

        let mut m = super::Mapping::new();
        m.insert("subject_id".into(), format!("{subject_prefix}:{subject_local}"));
        if let Some(l) = labels.get(&subject_iri) {
            m.insert("subject_label".into(), l.clone());
        }
        m.insert("predicate_id".into(), predicate.clone());
        m.insert("object_id".into(), xref.clone());
        m.insert("mapping_justification".into(), "semapv:UnspecifiedMatching".into());
        let subject_ns = prefixes.get(&subject_prefix).cloned().unwrap_or_default();
        keyed.push((
            (
                format!("{subject_ns}{subject_local}"),
                expand_predicate(&predicate),
                format!("{object_ns}{local}"),
            ),
            m,
        ));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    ms.mappings = keyed.into_iter().map(|(_, m)| m).collect();

    // `mapping_cardinality` is what `--drop-duplicates` decides by, so it is
    // computed only when that option is given — and then it stays on the rows it
    // kept, giving the set a sixth column. UBERON's `uberon-local.sssom.tsv` asks
    // for the de-duplication and carries it; uPheno's `uberon.sssom.tsv` does not,
    // and came out with six columns against five.
    //
    // Computed BEFORE the de-duplication, so the cardinality describes the
    // mappings the ontology asserts rather than the ones that survive it.
    // UBERON:0006376 xrefs both `EMAPA:29671` and `MA:0002628`, so it is `1:n` —
    // and stays `1:n` on the one row that survives.
    if opts.has("drop-duplicates") {
        ms.set_mapping_cardinality();
        drop_duplicates(&mut ms);
    }

    // Set-level metadata: what the ontology says about itself. `subject_source`
    // is metadata, not a column — every subject comes from the one ontology.
    let source = ontology_iri(&model);
    if let Some(id) = opts.one("set_id") {
        ms.metadata.insert("mapping_set_id".into(), serde_yaml::Value::String(id.to_string()));
    } else if let Some(s) = &source {
        let stem = s.strip_suffix(".owl").unwrap_or(s);
        ms.metadata.insert(
            "mapping_set_id".into(),
            serde_yaml::Value::String(format!("{stem}/mappings.sssom.tsv")),
        );
    }
    if let Some(l) = ontology_annotation_iri(&model, "http://purl.org/dc/terms/license") {
        ms.metadata.insert("license".into(), serde_yaml::Value::String(l));
    }
    if let Some(s) = &source {
        ms.metadata.insert("subject_source".into(), serde_yaml::Value::String(s.clone()));
    }

    // curie_map: the prefixes the mappings actually use.
    for p in &used_prefixes {
        if let Some(ns) = prefixes.get(p) {
            ms.curie_map.insert(p.clone(), ns.clone());
        }
    }
    ms.recompute_columns();

    let dest = opts.one("mapping_file").or_else(|| opts.one("output"));
    let tsv = sio::write_table_styled(&ms, '\t', true, false, sio::MetaStyle::Java)?;
    match dest {
        Some(path) => {
            if let Some(parent) = Path::new(path).parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(path, tsv).with_context(|| format!("writing {path}"))?;
        }
        None => print!("{tsv}"),
    }
    eprintln!("sssom:xref-extract: extracted {} mapping(s)", ms.mappings.len());
    Ok(())
}

/// SSSOM-Java's `standard_map`: the annotation property each mapping-metadata
/// slot is written with when a rule's `/annots="…"` names it, and whether the
/// slot's value is an IRI rather than a literal. UBERON's
/// `mappings-to-xrefs.rules` names five slots; its mapping sets populate two, so
/// ROBOT emits exactly 879 `sssom:mapping_justification` and 190
/// `pav:authoredBy` axiom annotations.
const STANDARD_ANNOT_MAP: &[(&str, &str, bool)] = &[
    ("mapping_justification", "https://w3id.org/sssom/mapping_justification", true),
    ("author_id", "http://purl.org/pav/authoredBy", true),
    ("creator_id", "http://purl.org/dc/terms/creator", true),
    ("reviewer_id", "https://w3id.org/sssom/reviewer_id", true),
    ("mapping_provider", "https://w3id.org/sssom/mapping_provider", false),
    ("mapping_tool", "https://w3id.org/sssom/mapping_tool", false),
    ("comment", "http://www.w3.org/2000/01/rdf-schema#comment", false),
    ("see_also", "http://www.w3.org/2000/01/rdf-schema#seeAlso", false),
];

/// A mapping set's built-in prefixes: the vocabularies every SSSOM file may
/// abbreviate without declaring them in its curie map. An IRI in one is written
/// as a CURIE — `--map-prefix-to-predicate 'AEO http://…/skos/core#exactMatch'`
/// produces `skos:exactMatch`, not the IRI it was spelled with.
const BUILTIN_PREFIXES: [(&str, &str); 8] = [
    ("sssom", "https://w3id.org/sssom/"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("semapv", "https://w3id.org/semapv/vocab/"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("linkml", "https://w3id.org/linkml/"),
];

/// Abbreviate an IRI against [`BUILTIN_PREFIXES`], or return it unchanged.
fn builtin_curie(iri: &str) -> String {
    for (p, ns) in BUILTIN_PREFIXES {
        if let Some(local) = iri.strip_prefix(ns) {
            return format!("{p}:{local}");
        }
    }
    iri.to_string()
}

/// The IRI a predicate CURIE denotes, for ordering. Only the vocabularies the
/// `treat-xrefs-as-*` tags and `--map-prefix-to-predicate` produce are named.
fn expand_predicate(p: &str) -> String {
    match p.split_once(':') {
        Some(("skos", l)) => format!("http://www.w3.org/2004/02/skos/core#{l}"),
        Some(("semapv", l)) => format!("https://w3id.org/semapv/vocab/{l}"),
        Some(("oboInOwl", l)) => format!("{OIO}{l}"),
        _ => p.to_string(),
    }
}

/// `--drop-duplicates`: drop mappings whose cardinality is many-to-one or
/// many-to-many (keep 1:1 and 1:n).
fn drop_duplicates(ms: &mut MappingSet) {
    // subject → #distinct objects, object → #distinct subjects.
    let mut by_subj: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut by_obj: HashMap<&str, HashSet<&str>> = HashMap::new();
    for m in &ms.mappings {
        let (Some(s), Some(o)) = (m.get("subject_id"), m.get("object_id")) else { continue };
        by_subj.entry(s).or_default().insert(o);
        by_obj.entry(o).or_default().insert(s);
    }
    let drop: Vec<bool> = ms
        .mappings
        .iter()
        .map(|m| {
            let (Some(s), Some(o)) = (m.get("subject_id"), m.get("object_id")) else {
                return false;
            };
            // object mapped by many subjects → many-to-one / many-to-many: drop.
            by_obj.get(o.as_str()).map(|set| set.len() > 1).unwrap_or(false)
                && by_subj.get(s.as_str()).map(|_| true).unwrap_or(false)
        })
        .collect();
    let mut i = 0;
    ms.mappings.retain(|_| {
        let keep = !drop[i];
        i += 1;
        keep
    });
}

// ───────────────────────────── SSSOM/T inject ───────────────────────────────

fn inject(model: Option<Model>, args: &[String]) -> Result<()> {
    let opts = parse_opts(
        args,
        &[
            ("input", &["-i", "--input"]),
            ("output", &["-o", "--output"]),
            ("sssom", &["--sssom"]),
            ("ruleset", &["--ruleset"]),
            ("dispatch", &["--dispatch-table"]),
            ("exclude", &["--exclude-rule"]),
            ("format", &["--format"]),
            // `--no-merge --bridge-file F --bridge-iri I`: write the GENERATED
            // axioms to F as an ontology of their own, and (with `--no-merge`) do
            // not inject them into the in-flight ontology. Unlisted, these two
            // fell to `parse_opts`'s catch-all flag branch, which keeps the option
            // NAME and throws its value away — so UBERON's `components/mappings.owl`
            // was written by the chain's fallback save as the whole 54 MB merged
            // ontology where ROBOT writes a 486 KB bridge.
            ("bridge_file", &["--bridge-file"]),
            ("bridge_iri", &["--bridge-iri"]),
            // The release version a dispatch table's `%date` expands to. Unset,
            // it is the date this run happens on, which is what a repo releasing
            // under its build date means by it.
            ("version", &["--version"]),
        ],
    );
    let model = load_model(model, &opts)?;

    // Load and concatenate the mapping sets.
    let mut ms = MappingSet::new();
    for f in opts.many("sssom") {
        let part = sio::read_path(Path::new(f), None, None)
            .with_context(|| format!("reading mapping set {f}"))?;
        for (p, b) in &part.curie_map {
            ms.curie_map.entry(p.clone()).or_insert_with(|| b.clone());
        }
        ms.mappings.extend(part.mappings);
    }

    let ruleset_path = opts.one("ruleset").context("sssom:inject requires --ruleset")?;
    let rules_text = std::fs::read_to_string(ruleset_path)
        .with_context(|| format!("reading ruleset {ruleset_path}"))?;
    let ruleset = parse_ruleset(&rules_text)?;

    // Effective prefix map: ruleset declarations win over the mapping set's.
    let mut prefixes = ms.effective_prefixes();
    for (p, b) in &ruleset.prefixes {
        prefixes.insert(p.clone(), b.clone());
    }

    // `cardinality==…` filters need the per-mapping cardinality precomputed.
    ms.set_mapping_cardinality();

    let exclude: HashSet<&str> = opts.many("exclude").iter().map(String::as_str).collect();

    // Run the ruleset (the engine borrows the model; drop it before reusing `model`).
    let mut out = {
        let engine = Engine::new(&model, &prefixes);
        engine.run(&ms, &ruleset, &exclude)?
    };

    // `--bridge-file` writes the generated axioms as a standalone ontology, the
    // way `--dispatch-table` does per tag but into one file. UBERON's mappings
    // component is exactly this: `--no-merge --bridge-file components/mappings.owl
    // --bridge-iri …/components/mappings.owl`, and ROBOT emits 486 KB of bridge
    // axioms — not the input ontology with the mappings folded in.
    if let Some(bridge) = opts.one("bridge_file") {
        let build = horned_owl::model::Build::new();
        // NOT seeded with the ruleset's prefix map: a bridge declares only what
        // its own axioms use. ROBOT's `components/mappings.owl` carries 8
        // `xmlns:` declarations (owl/rdf/rdfs/xml/xsd plus oboInOwl, pav, sssom);
        // copying the ruleset's map emitted 61, because every `prefix …` line the
        // ruleset declares for CURIE matching came along. The xref targets are
        // written as plain literals, so they need no prefix at all.
        let mut m = Model::new();
        // A bridge is a NEW document, not one read from a file, so it starts with
        // no format prefix map either — the writer declares the structural
        // namespaces plus whatever the bridge's own properties need. Left as the
        // in-memory defaults, CL's bridge announced `dc`, `obo` and `terms` for
        // namespaces not one of its axioms mentions.
        m.format_prefixes_cleared = true;
        for (_, axs) in out.iter_mut() {
            for ax in axs.drain(..) {
                m.ont.insert(ax);
            }
        }
        if let Some(iri) = opts.one("bridge_iri") {
            m.ont.insert(Component::OntologyID(horned_owl::model::OntologyID {
                iri: Some(build.iri(iri)),
                viri: None,
            }));
        }
        crate::io::save(&mut m, Path::new(bridge))
            .with_context(|| format!("writing bridge {bridge}"))?;
    }

    // `--no-merge`: the generated axioms do NOT go into the in-flight ontology.
    // With it, a `-o` writes the input back unchanged; without it, `-o` writes the
    // input plus the axioms (ROBOT's default injecting behaviour).
    if opts.has("no-merge") {
        for (_, axs) in out.iter_mut() {
            axs.clear();
        }
    }

    if let Some(dispatch) = opts.one("dispatch") {
        let table = parse_dispatch(&std::fs::read_to_string(dispatch)
            .with_context(|| format!("reading dispatch table {dispatch}"))?);
        let dir = Path::new(dispatch).parent().unwrap_or(Path::new(".")).to_path_buf();
        // The version this run stamps into `%date`. A date is a run input, so it
        // is read here once and handed to the writer rather than reached for
        // inside it.
        let version = opts
            .one("version")
            .map(str::to_string)
            .unwrap_or_else(crate::plan::today);
        write_dispatched(&prefixes, &mut out, &table, &dir, &version)?;
    } else if let Some(o) = opts.one("output") {
        // `sssom:inject` injects the generated axioms INTO the in-flight
        // ontology; the output is the input plus those axioms.
        let mut m = model;
        for (_, axs) in out.iter_mut() {
            for ax in axs.drain(..) {
                m.ont.insert(ax);
            }
        }
        copy_prefixes(&prefixes, &mut m);
        match opts.one("format") {
            Some(f) => crate::io::save_as(&mut m, Path::new(o), crate::io::Format::from_name(f)?)?,
            None => crate::io::save(&mut m, Path::new(o))?,
        }
    }
    Ok(())
}

fn copy_prefixes(prefixes: &BTreeMap<String, String>, m: &mut Model) {
    for (p, b) in prefixes {
        let _ = m.prefixes.add_prefix(p, b);
    }
}

// ───────────────────────── SSSOM/T ruleset model ────────────────────────────

#[derive(Debug, Clone)]
enum Filter {
    Always,
    Slot { slot: String, glob: String }, // subject==/object==/predicate==/cardinality==
    IsA { var: String, class: String },
    Exists { var: String },
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
}

#[derive(Debug, Clone)]
enum Action {
    Stop,
    Invert,
    SetVar { name: String, value: String },
    /// `annotate(entity, prop, value)`, optionally with SSSOM/T keyword
    /// arguments: `/annots="slot,slot,…"` names mapping-metadata slots to attach
    /// to the GENERATED axiom as axiom annotations, and `/annots_uris="…"` says
    /// their values are IRIs rather than literals.
    Annotate {
        entity: String,
        prop: String,
        value: String,
        annots: Vec<String>,
        annots_uris: bool,
    },
    CreateAxiom { text: String },
    Block(Vec<Rule>),
}

#[derive(Debug, Clone)]
struct Rule {
    tags: Vec<String>,
    filter: Filter,
    action: Action,
}

#[derive(Debug, Default)]
struct Ruleset {
    prefixes: BTreeMap<String, String>,
    rules: Vec<Rule>,
    /// Variables a header-level `set_var(name, value)` gives every mapping before
    /// any rule runs, in the order the ruleset sets them. They are DEFAULTS: a
    /// rule that fires may set the same name to something else.
    defaults: Vec<(String, String)>,
}

// ───────────────────────── SSSOM/T ruleset parser ───────────────────────────

/// Tokenize-free, line/brace-aware parser for the subset of SSSOM/T that bridge
/// rulesets use. Statements end at `;` (or a `{…}` block); `#` starts a
/// comment. A `%INSERT-…` token is a templating placeholder substituted into
/// the ruleset before the file is handed to owlmake, so one still present at
/// parse time carries no rule and is skipped, as are `set_var`/`declare` header
/// statements.
fn parse_ruleset(text: &str) -> Result<Ruleset> {
    parse_ruleset_inner(text, false)
}

/// `in_body`: the statements are the inside of a `FILTER { … }` block, where each
/// one is a bare ACTION guarded by the enclosing filter rather than a rule of its
/// own.
fn parse_ruleset_inner(text: &str, in_body: bool) -> Result<Ruleset> {
    // Strip comments first (line-level `#`) — but a `#` inside an `<IRI>` or a
    // quoted string is DATA, not a comment. Cutting blindly at the first `#`
    // truncated `prefix oboInOwl: <http://www.geneontology.org/formats/oboInOwl#>`
    // to `…/oboInOwl`, so every generated xref asserted
    // `…/formats/oboInOwlhasDbXref` instead of `…/formats/oboInOwl#hasDbXref`.
    let mut cleaned = String::with_capacity(text.len());
    for line in text.lines() {
        let (mut in_iri, mut in_str) = (false, false);
        let mut cut = line.len();
        for (i, c) in line.char_indices() {
            match c {
                '"' if !in_iri => in_str = !in_str,
                '<' if !in_str => in_iri = true,
                '>' if !in_str => in_iri = false,
                '#' if !in_iri && !in_str => {
                    cut = i;
                    break;
                }
                _ => {}
            }
        }
        cleaned.push_str(&line[..cut]);
        cleaned.push('\n');
    }
    let mut rs = Ruleset::default();
    let bytes = cleaned.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // A `prefix` header ends at the END OF LINE, not at a `;`. SSSOM/T writes
        // them unterminated — UBERON's `mappings-to-xrefs.rules` opens with 26 of
        // them — and scanning on to the next `;` glued each declaration to the
        // rule that followed it. The prefix map then bound `UBERON` to
        // `http://purl.obolibrary.org/obo/UBERON_>\nobject==UBERON:* -> …`, so
        // `object==UBERON:*` matched nothing, every rule was filtered out, and the
        // bridge came out with none of its 879 axioms. A trailing `;` is tolerated.
        let start = i;
        if cleaned[start..].starts_with("prefix ") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let stmt = cleaned[start..i].trim().trim_end_matches(';').trim();
            if !stmt.is_empty() {
                parse_statement(stmt, None, &mut rs, in_body)?;
            }
            continue;
        }
        let mut depth = 0i32;
        let mut brace: Option<(usize, usize)> = None;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        i += 1;
                    }
                }
                // `%{slot}` is a SUBSTITUTION PLACEHOLDER, not a `{…}` block.
                // Quoted placeholders were already safe (the `"` arm above skips
                // the whole string), which is why MONDO's rulesets — whose
                // placeholders are all inside quotes — worked. UBERON's
                // `annotate(%{object_id}, …)` puts one UNQUOTED, so the scanner
                // ended the statement at `annotate(%`, parsed `object_id` as a
                // block body, and produced `action = Block([])`: a rule that
                // matches and does nothing.
                b'%' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'}' {
                        i += 1;
                    }
                }
                b'{' if depth == 0 => {
                    let bstart = i;
                    depth = 1;
                    i += 1;
                    while i < bytes.len() && depth > 0 {
                        match bytes[i] {
                            b'{' => depth += 1,
                            b'}' => depth -= 1,
                            _ => {}
                        }
                        i += 1;
                    }
                    brace = Some((bstart + 1, i - 1));
                    break;
                }
                b';' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let stmt = cleaned[start..i.min(cleaned.len())].trim();
        // Consume the terminating `;`.
        if i < bytes.len() && bytes[i] == b';' {
            i += 1;
        }
        if stmt.is_empty() {
            continue;
        }
        parse_statement(stmt, brace.map(|(a, b)| cleaned[a..b].to_string()), &mut rs, in_body)?;
    }
    Ok(rs)
}

fn parse_statement(
    stmt: &str,
    block: Option<String>,
    rs: &mut Ruleset,
    in_body: bool,
) -> Result<()> {
    let stmt = stmt.trim();
    if stmt.is_empty() || stmt.starts_with('%') {
        return Ok(()); // %INSERT-… placeholders: nothing to do.
    }
    // Header: `prefix PFX: <iri>`
    if let Some(rest) = stmt.strip_prefix("prefix ") {
        if let Some((p, iri)) = rest.split_once(':') {
            let iri = iri.trim().trim_start_matches('<').trim_end_matches('>').trim();
            rs.prefixes.insert(p.trim().to_string(), iri.to_string());
        }
        return Ok(());
    }
    // Header-level `declare(...)`: entity declarations are implicit in owlmake's
    // output, so there is nothing to record.
    if stmt.starts_with("declare(") {
        return Ok(());
    }
    // Header-level `set_var(name, value)` — no filter, so it applies to EVERY
    // mapping, as the value the variable holds unless a rule overwrites it.
    // UBERON's bridge ruleset opens with `set_var("TAXREL", BFO:0000050)` and
    // then narrows it to `BFO:0000066` for the two mappings whose object is a
    // life-stage class; without the default the other 102 `%TAXREL` uses expand
    // to nothing and the axiom they build has no property at all.
    if !in_body {
        if let Some(args) = call_args(stmt.trim_end_matches(';').trim(), "set_var") {
            let a = split_top_commas(&args);
            if a.len() == 2 {
                rs.defaults.push((unquote(a[0].trim()), a[1].trim().to_string()));
                return Ok(());
            }
        }
    }

    // A rule: optional `[tag]` prefixes, a filter, then `-> action` or a `{block}`.
    let (tags, rest) = parse_tags(stmt);
    if let Some(body) = block {
        // `[tag] FILTER { subrules }` — and `FILTER -> { action; action; }`, where
        // the arrow introduces a block of BARE actions sharing one filter rather
        // than a single action. The arrow says nothing the filter needs, so drop
        // it; left on, it became part of the last comparison's value, and the rule
        // matched nothing while still looking like a rule.
        //
        // UBERON's bridges are almost entirely this form: every taxon-specific
        // rule pairs an `EquivalentTo:` with a `SubClassOf:` inside one block.
        // Only the taxon-NEUTRAL bridges — AEO, BFO, CARO, GO — use a single
        // action, which is why those four came out right and the other 30 came
        // out holding their annotations and none of their axioms.
        //
        // The statement still carries its own `{ … }` text, so the header is
        // everything before the opening brace — but NOT a `%{slot}` placeholder's
        // brace, which is part of the filter.
        let head = {
            let b = rest.as_bytes();
            let mut cut = rest.len();
            for k in 0..b.len() {
                if b[k] == b'{' && (k == 0 || b[k - 1] != b'%') {
                    cut = k;
                    break;
                }
            }
            rest[..cut].trim()
        };
        let filter = parse_filter(head.strip_suffix("->").unwrap_or(head).trim())?;
        let mut subrules = Vec::new();
        let inner = parse_ruleset_inner(&body, true)?;
        for mut r in inner.rules {
            inherit_tags(&mut r, &tags);
            subrules.push(r);
        }
        rs.rules.push(Rule { tags, filter, action: Action::Block(subrules) });
        return Ok(());
    }
    // `[tag] FILTER -> ACTION`
    let (filt_str, act_str) = match rest.split_once("->") {
        Some((f, a)) => (f.trim(), a.trim()),
        // Inside a block the statement IS the action — `annotate(…)` and
        // `create_axiom(…)` sit under the enclosing rule's filter with no arrow
        // of their own. Read at the top level, such a statement is not a rule and
        // is skipped; read as a block body it is the whole point of the block, and
        // skipping it left `Block([])` — a rule that matches and does nothing.
        None if in_body => ("", rest.trim()),
        None => return Ok(()), // not a rule we understand; skip
    };
    let filter = parse_filter(filt_str)?;
    let action = parse_action(act_str)?;
    rs.rules.push(Rule { tags, filter, action });
    Ok(())
}

/// Prepend `tags` to `r`, and to every rule nested inside it.
///
/// A tag routes the axioms a rule builds to one bridge file, and only the rule
/// that CREATES an axiom hands its tags to `push_tagged`. So the tags have to
/// reach the rule that does the creating, however deeply it sits: a tag that
/// stopped at the outer rule left every axiom built inside a nested block with
/// no tag, and an untagged axiom goes to no file at all.
fn inherit_tags(r: &mut Rule, tags: &[String]) {
    let mut t = tags.to_vec();
    t.extend(r.tags.drain(..));
    r.tags = t;
    if let Action::Block(sub) = &mut r.action {
        for s in sub.iter_mut() {
            inherit_tags(s, tags);
        }
    }
}

/// Strip leading `[tag]` markers, returning the tags and the remainder.
fn parse_tags(s: &str) -> (Vec<String>, &str) {
    let mut tags = Vec::new();
    let mut s = s.trim_start();
    while let Some(rest) = s.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            tags.push(rest[..end].trim().to_string());
            s = rest[end + 1..].trim_start();
        } else {
            break;
        }
    }
    (tags, s)
}

fn parse_action(s: &str) -> Result<Action> {
    let s = s.trim().trim_end_matches(';').trim();
    if s == "stop()" {
        return Ok(Action::Stop);
    }
    if s == "invert()" {
        return Ok(Action::Invert);
    }
    if let Some(args) = call_args(s, "set_var") {
        let a = split_top_commas(&args);
        if a.len() == 2 {
            return Ok(Action::SetVar {
                name: unquote(a[0].trim()),
                value: a[1].trim().to_string(),
            });
        }
    }
    if let Some(args) = call_args(s, "annotate") {
        let a = split_top_commas(&args);
        // Three POSITIONAL arguments; anything further is a `/keyword="value"`.
        // Requiring exactly three sent UBERON's five-argument call to the
        // unknown-action branch below, which silently generated nothing — the
        // rule that produces all 879 annotated xrefs in `components/mappings.owl`.
        if a.len() >= 3 {
            let mut annots = Vec::new();
            let mut annots_uris = false;
            for kw in &a[3..] {
                let kw = kw.trim();
                let Some(rest) = kw.strip_prefix('/') else { continue };
                let Some((k, v)) = rest.split_once('=') else { continue };
                match k.trim() {
                    "annots" => annots.extend(
                        unquote(v.trim())
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()),
                    ),
                    "annots_uris" => annots_uris = !unquote(v.trim()).is_empty(),
                    _ => {}
                }
            }
            return Ok(Action::Annotate {
                entity: a[0].trim().to_string(),
                prop: a[1].trim().to_string(),
                value: unquote(a[2].trim()),
                annots,
                annots_uris,
            });
        }
    }
    if let Some(args) = call_args(s, "create_axiom") {
        return Ok(Action::CreateAxiom { text: unquote(args.trim()) });
    }
    // An action the engine cannot map is an ERROR, not a rule that quietly does
    // nothing. Skipping it "without failing the whole build" is how UBERON's
    // `annotate(…, /annots=…, /annots_uris=…)` came to generate 0 of the 879
    // axioms `components/mappings.owl` needs while the build reported success —
    // the rule matched every mapping and produced nothing. P5: a step in the plan
    // runs, or it fails.
    anyhow::bail!(
        "unsupported SSSOM/T action `{s}`: owlmake implements stop(), invert(), \
         set_var(), annotate() and create_axiom(). A rule it cannot map would \
         silently generate nothing, so it is refused rather than skipped."
    )
}

/// If `s` is `name( … )`, return the inner argument text.
fn call_args(s: &str, name: &str) -> Option<String> {
    let s = s.trim();
    let rest = s.strip_prefix(name)?.trim_start();
    let rest = rest.strip_prefix('(')?;
    let rest = rest.strip_suffix(')')?;
    Some(rest.to_string())
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(s).to_string()
}

fn split_top_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '"' => {
                in_str = !in_str;
                cur.push(c);
            }
            '(' | '{' if !in_str => {
                depth += 1;
                cur.push(c);
            }
            ')' | '}' if !in_str => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 && !in_str => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

// ───────────────────────── SSSOM/T filter parser ────────────────────────────

/// Parse a filter expression with `||` (or), juxtaposition (and), `!` (not),
/// parentheses, and the leaf predicates `slot==glob`, `is_a(var, class)`,
/// `exists(var)`, `cardinality==glob`.
fn parse_filter(s: &str) -> Result<Filter> {
    let toks = filter_tokens(s);
    let mut p = FParser { toks: &toks, pos: 0 };
    let f = p.parse_or()?;
    Ok(f)
}

#[derive(Debug, PartialEq, Clone)]
enum FTok {
    Or,
    Not,
    LParen,
    RParen,
    Atom(String),
}

fn filter_tokens(s: &str) -> Vec<FTok> {
    let mut toks = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'|' && i + 1 < b.len() && b[i + 1] == b'|' {
            toks.push(FTok::Or);
            i += 2;
        } else if c == b'!' {
            toks.push(FTok::Not);
            i += 1;
        } else if c == b'(' {
            // Could be grouping or part of an atom like `is_a(...)`/`exists(...)`.
            // Grouping `(` only follows nothing/Or/Not/LParen; otherwise it's an
            // atom that already consumed its parens below. Here we treat a bare `(`
            // as grouping.
            toks.push(FTok::LParen);
            i += 1;
        } else if c == b')' {
            toks.push(FTok::RParen);
            i += 1;
        } else {
            // Read an atom up to a top-level space, `||`, `!`, or unbalanced `)`.
            let start = i;
            let mut depth = 0i32;
            while i < b.len() {
                let d = b[i];
                if d == b'(' {
                    depth += 1;
                } else if d == b')' {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                } else if depth == 0
                    && (d.is_ascii_whitespace() || (d == b'|' && i + 1 < b.len() && b[i + 1] == b'|'))
                {
                    break;
                }
                i += 1;
            }
            toks.push(FTok::Atom(s[start..i].trim().to_string()));
        }
    }
    toks
}

struct FParser<'a> {
    toks: &'a [FTok],
    pos: usize,
}

impl FParser<'_> {
    fn peek(&self) -> Option<&FTok> {
        self.toks.get(self.pos)
    }
    fn parse_or(&mut self) -> Result<Filter> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(FTok::Or)) {
            self.pos += 1;
            let right = self.parse_and()?;
            left = match left {
                Filter::Or(mut v) => {
                    v.push(right);
                    Filter::Or(v)
                }
                _ => Filter::Or(vec![left, right]),
            };
        }
        Ok(left)
    }
    fn parse_and(&mut self) -> Result<Filter> {
        let mut parts = vec![self.parse_unary()?];
        while matches!(self.peek(), Some(FTok::Not | FTok::LParen | FTok::Atom(_))) {
            parts.push(self.parse_unary()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Filter::And(parts) })
    }
    fn parse_unary(&mut self) -> Result<Filter> {
        if matches!(self.peek(), Some(FTok::Not)) {
            self.pos += 1;
            return Ok(Filter::Not(Box::new(self.parse_unary()?)));
        }
        if matches!(self.peek(), Some(FTok::LParen)) {
            self.pos += 1;
            let f = self.parse_or()?;
            if matches!(self.peek(), Some(FTok::RParen)) {
                self.pos += 1;
            }
            return Ok(f);
        }
        if let Some(FTok::Atom(a)) = self.peek() {
            let a = a.clone();
            self.pos += 1;
            return Ok(parse_leaf(&a));
        }
        Ok(Filter::Always)
    }
}

fn parse_leaf(a: &str) -> Filter {
    let a = a.trim();
    if let Some(args) = call_args(a, "is_a") {
        let parts = split_top_commas(&args);
        if parts.len() == 2 {
            return Filter::IsA { var: parts[0].trim().to_string(), class: parts[1].trim().to_string() };
        }
    }
    if let Some(args) = call_args(a, "exists") {
        return Filter::Exists { var: args.trim().to_string() };
    }
    if let Some((slot, glob)) = a.split_once("==") {
        return Filter::Slot { slot: slot.trim().to_string(), glob: glob.trim().to_string() };
    }
    Filter::Always
}

// ───────────────────────── dispatch table parser ────────────────────────────

#[derive(Debug, Default, Clone)]
struct DispatchEntry {
    file: Option<String>,
    ontology_iri: Option<String>,
    ontology_version: Option<String>,
    add_axioms: Vec<String>,
    annotations: Vec<(String, String)>, // (dc property local, value)
}

#[derive(Debug, Default)]
struct DispatchTable {
    defaults: DispatchEntry,
    entries: BTreeMap<String, DispatchEntry>,
}

fn parse_dispatch(text: &str) -> DispatchTable {
    let mut table = DispatchTable::default();
    let mut cur_key: Option<String> = None;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(name) = l.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            cur_key = Some(name.to_string());
            if name != "__default" {
                table.entries.entry(name.to_string()).or_default();
            }
            continue;
        }
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim().to_string());
        let entry = match cur_key.as_deref() {
            Some("__default") | None => &mut table.defaults,
            Some(key) => table.entries.get_mut(key).unwrap(),
        };
        match k {
            "file" => entry.file = Some(v),
            "ontology-iri" => entry.ontology_iri = Some(v),
            "ontology-version" => entry.ontology_version = Some(v),
            "add-axiom" => entry.add_axioms.push(v),
            other if other.starts_with("dc-") => {
                entry.annotations.push((other[3..].to_string(), v));
            }
            _ => {}
        }
    }
    table
}

// ───────────────────────────── the engine ───────────────────────────────────

struct Engine<'a> {
    prefixes: &'a BTreeMap<String, String>,
    labels: HashMap<String, String>,
    declared: HashSet<String>,
    ancestors: HashMap<String, HashSet<String>>,
    build: horned_owl::model::Build<Str>,
}

impl<'a> Engine<'a> {
    fn new(model: &Model, prefixes: &'a BTreeMap<String, String>) -> Self {
        let (labels, _) = labels_and_obsoletes(model);
        let mut declared = HashSet::new();
        for e in crate::cmd::select::entities(model).classes {
            declared.insert(e);
        }
        // Subsumption closure for is_a(): descendant → {ancestors}.
        let reasoner = crate::reason::el::Reasoner::classify(model);
        let mut ancestors: HashMap<String, HashSet<String>> = HashMap::new();
        for (sub, sup) in reasoner.all_subsumptions() {
            ancestors.entry(sub).or_default().insert(sup);
        }
        Engine { prefixes, labels, declared, ancestors, build: horned_owl::model::Build::new() }
    }

    /// Run the ruleset over every mapping, returning generated axioms grouped by
    /// dispatch tag (empty string = untagged).
    fn run(
        &self,
        ms: &MappingSet,
        ruleset: &Ruleset,
        exclude: &HashSet<&str>,
    ) -> Result<BTreeMap<String, Vec<AnnotatedComponent<Str>>>> {
        let mut out: BTreeMap<String, Vec<AnnotatedComponent<Str>>> = BTreeMap::new();
        for m in &ms.mappings {
            let mut ctx = Ctx::from_mapping(m);
            for (name, value) in &ruleset.defaults {
                ctx.vars.insert(name.clone(), value.clone());
            }
            self.run_rules(&ruleset.rules, &mut ctx, exclude, &mut out)?;
        }
        Ok(out)
    }

    fn run_rules(
        &self,
        rules: &[Rule],
        ctx: &mut Ctx,
        exclude: &HashSet<&str>,
        out: &mut BTreeMap<String, Vec<AnnotatedComponent<Str>>>,
    ) -> Result<()> {
        for rule in rules {
            if rule.tags.iter().any(|t| exclude.contains(t.as_str())) {
                continue;
            }
            // A `set_var` under a filter is RECORDED here and decided when the
            // variable is read, so its filter must not be tested now — testing it
            // here is what the mapping looks like at this point in the ruleset,
            // and what the variable means is what the mapping looks like where it
            // is used. See `Engine::resolve_var`.
            if let Action::SetVar { name, value } = &rule.action {
                if !matches!(rule.filter, Filter::Always) {
                    ctx.conditional_vars.push((
                        name.clone(),
                        rule.filter.clone(),
                        value.clone(),
                    ));
                    continue;
                }
            }
            if !self.eval(&rule.filter, ctx) {
                continue;
            }
            match &rule.action {
                Action::Stop => {
                    ctx.stopped = true;
                    return Ok(());
                }
                Action::Invert => ctx.invert(),
                Action::SetVar { name, value } => {
                    let v = self.subst(value, ctx, false);
                    ctx.vars.insert(name.clone(), v);
                }
                Action::Annotate { entity, prop, value, annots, annots_uris } => {
                    if let Some(ax) =
                        self.make_annotation(entity, prop, value, annots, *annots_uris, ctx)
                    {
                        push_tagged(out, &rule.tags, ax);
                    }
                }
                Action::CreateAxiom { text } => {
                    if let Some(ax) = self.make_axiom(text, ctx)? {
                        push_tagged(out, &rule.tags, ax);
                    }
                }
                Action::Block(sub) => {
                    self.run_rules(sub, ctx, exclude, out)?;
                    if ctx.stopped {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    fn eval(&self, f: &Filter, ctx: &Ctx) -> bool {
        match f {
            Filter::Always => true,
            Filter::And(v) => v.iter().all(|x| self.eval(x, ctx)),
            Filter::Or(v) => v.iter().any(|x| self.eval(x, ctx)),
            Filter::Not(x) => !self.eval(x, ctx),
            Filter::Slot { slot, glob } => {
                // Match against the CURIE computed with the ruleset's prefix map
                // (so `subject==UBERON:*` matches regardless of how the mapping
                // file happened to compress the IRI), and the raw value too.
                let raw = ctx.slot(slot);
                let curie = compress(self.prefixes, &expand(self.prefixes, &raw));
                glob_match(glob, &curie) || glob_match(glob, &raw)
            }
            Filter::Exists { var } => {
                let iri = self.resolve_iri(&self.subst(var, ctx, false));
                self.declared.contains(&iri)
            }
            Filter::IsA { var, class } => {
                let sub = self.resolve_iri(&self.subst(var, ctx, false));
                let sup = self.resolve_iri(class);
                sub == sup
                    || self.ancestors.get(&sub).map(|a| a.contains(&sup)).unwrap_or(false)
            }
        }
    }

    fn resolve_iri(&self, s: &str) -> String {
        let s = s.trim().trim_start_matches('<').trim_end_matches('>');
        expand(self.prefixes, s)
    }

    /// Substitute `%{slot}`, `%{slot|short}`, and `%slot` in `tmpl`. When
    /// `bracket_iri` is set, id slots become `<IRI>` (for Manchester parsing);
    /// otherwise they become the bare IRI. `|short` forces the CURIE form.
    fn subst(&self, tmpl: &str, ctx: &Ctx, bracket_iri: bool) -> String {
        let mut out = String::with_capacity(tmpl.len());
        let b = tmpl.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 1 < b.len() {
                let (name, short, next) = if b[i + 1] == b'{' {
                    let end = tmpl[i + 2..].find('}').map(|e| i + 2 + e).unwrap_or(b.len());
                    let inner = &tmpl[i + 2..end];
                    let (nm, sh) = match inner.split_once('|') {
                        Some((n, m)) => (n.trim(), m.trim() == "short"),
                        None => (inner.trim(), false),
                    };
                    (nm.to_string(), sh, end + 1)
                } else {
                    let start = i + 1;
                    let mut j = start;
                    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                        j += 1;
                    }
                    (tmpl[start..j].to_string(), false, j)
                };
                out.push_str(&self.resolve_var(&name, ctx, short, bracket_iri));
                i = next;
            } else {
                out.push(b[i] as char);
                i += 1;
            }
        }
        out
    }

    fn resolve_var(&self, name: &str, ctx: &Ctx, short: bool, bracket_iri: bool) -> String {
        // A CONDITIONAL `set_var` — one carrying a filter — is decided HERE, against
        // the mapping as it stands when the variable is read, not as it stood where
        // the rule was written. The last condition that holds wins, so a later rule
        // still overrides an earlier one.
        //
        // UBERON's bridges turn on this. `is_a(%{object_id}, UBERON:0000105) ->
        // set_var("TAXREL", BFO:0000066)` sits ABOVE the preamble's `invert()`, so
        // where it is written the object is still the foreign term and the test
        // cannot hold; by the time `%TAXREL` is used, UBERON is on the object side
        // and it does. Deciding it where it is written gives every life-stage
        // bridge `part_of` where it should have `occurs_in`.
        for (n, filter, value) in ctx.conditional_vars.iter().rev() {
            if n == name && self.eval(filter, ctx) {
                return self.subst(value, ctx, false);
            }
        }
        // Variables set via an unconditional set_var take precedence over slots.
        if let Some(v) = ctx.vars.get(name) {
            return v.clone();
        }
        if name.ends_with("_label") {
            // The mapping's own label column is what the placeholder names:
            // `%{subject_label}` on a UBERON→XAO mapping is "death stage", the
            // label the mapping set carries, and the subject need not be in the
            // ontology at all. Only when the set has no such column is the
            // ontology consulted for one.
            let slot = ctx.slot(name);
            if !slot.is_empty() {
                return slot;
            }
            let base = &name[..name.len() - "_label".len()];
            let iri = self.resolve_iri(&ctx.slot(&format!("{base}_id")));
            return self.labels.get(&iri).cloned().unwrap_or_default();
        }
        // An id slot (subject_id/object_id/predicate_id).
        let raw = ctx.slot(name);
        if short {
            return compress(self.prefixes, &expand(self.prefixes, &raw));
        }
        let iri = expand(self.prefixes, &raw);
        if bracket_iri {
            format!("<{iri}>")
        } else {
            iri
        }
    }

    fn make_annotation(
        &self,
        entity: &str,
        prop: &str,
        value: &str,
        annots: &[String],
        annots_uris: bool,
        ctx: &Ctx,
    ) -> Option<AnnotatedComponent<Str>> {
        let subj_iri = self.resolve_iri(&self.subst(entity, ctx, false));
        let prop_iri = self.resolve_iri(prop);
        let val = self.subst(value, ctx, false);
        // `/annots="slot,…"`: each named slot the mapping actually carries becomes
        // an annotation ON the generated axiom. A slot may be multi-valued —
        // SSSOM writes lists `|`-separated — and each value gets its own
        // annotation, which is why 879 axioms carry 190 `pav:authoredBy`.
        let mut anns: Vec<Annotation<Str>> = Vec::new();
        for slot in annots {
            let Some((_, ap_iri, is_uri)) =
                STANDARD_ANNOT_MAP.iter().find(|(s, _, _)| s == slot)
            else {
                continue;
            };
            let Some(raw) = ctx.meta.get(slot.as_str()) else { continue };
            for one in raw.split('|').map(str::trim).filter(|s| !s.is_empty()) {
                let av = if annots_uris && *is_uri {
                    // Resolve against the EFFECTIVE prefix map (the mapping set's
                    // `curie_map` plus the ruleset's declarations), not the small
                    // predicate table: `author_id` is `ORCID:0000-…`, which only
                    // the mapping set's own `ORCID: https://orcid.org/` expands.
                    AnnotationValue::IRI(self.build.iri(self.resolve_iri(one).as_str()))
                } else {
                    AnnotationValue::Literal(Literal::Simple { literal: one.to_string() })
                };
                anns.push(Annotation {
                    ann: Default::default(),
                    ap: AnnotationProperty(self.build.iri(*ap_iri)),
                    av,
                });
            }
        }
        let mut ac: AnnotatedComponent<Str> = Component::AnnotationAssertion(AnnotationAssertion {
            subject: AnnotationSubject::IRI(self.build.iri(subj_iri.as_str())),
            ann: Annotation {
                ann: Default::default(),
                ap: AnnotationProperty(self.build.iri(prop_iri.as_str())),
                av: AnnotationValue::Literal(Literal::Simple { literal: val }),
            },
        })
        .into();
        ac.ann = anns.into_iter().collect();
        Some(ac)
    }

    /// Parse a `LHS (SubClassOf:|EquivalentTo:) RHS` SSSOM/T axiom template.
    fn make_axiom(&self, text: &str, ctx: &Ctx) -> Result<Option<AnnotatedComponent<Str>>> {
        let s = self.subst(text, ctx, true);
        manchester_axiom(&self.build, self.prefixes, &s)
    }
}

/// Parse a `LHS (SubClassOf:|EquivalentTo:) RHS` Manchester axiom, resolving
/// CURIEs/IRIs via `prefixes` (with the OBO fallback). Returns `None` if `s` is
/// not such an axiom.
fn manchester_axiom(
    build: &horned_owl::model::Build<Str>,
    prefixes: &BTreeMap<String, String>,
    s: &str,
) -> Result<Option<AnnotatedComponent<Str>>> {
    let resolver = move |tok: &str| -> Option<String> {
        let t = tok.trim();
        let inner = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')).unwrap_or(t);
        if inner.starts_with("http://") || inner.starts_with("https://") || inner.starts_with("urn:") {
            return Some(inner.to_string());
        }
        let e = expand(prefixes, inner);
        if e != inner {
            Some(e)
        } else {
            None
        }
    };
    let (lhs, rhs, equiv) = if let Some((l, r)) = s.split_once("EquivalentTo:") {
        (l, r, true)
    } else if let Some((l, r)) = s.split_once("SubClassOf:") {
        (l, r, false)
    } else {
        return Ok(None);
    };
    let lc = crate::io::manchester_parse::parse_class_expression(build, lhs.trim(), &resolver)
        .map_err(|e| anyhow::anyhow!("axiom LHS `{}`: {e}", lhs.trim()))?;
    let rc = crate::io::manchester_parse::parse_class_expression(build, rhs.trim(), &resolver)
        .map_err(|e| anyhow::anyhow!("axiom RHS `{}`: {e}", rhs.trim()))?;
    let comp = if equiv {
        // An equivalence has no direction: it is written in the frame of its
        // FIRST operand, and which operand that is comes from the axiom, not from
        // which side of `EquivalentTo:` the rule happened to write. Sorted, the
        // clause lands on the smaller IRI — `GO:0005623 EquivalentTo: CL:0000000`
        // belongs in the `CL:0000000` frame, and a rule that spells the same pair
        // the other way round produces the same file.
        let mut ops = vec![lc, rc];
        ops.sort_by(crate::io::owlfunc::cmp_ce);
        Component::EquivalentClasses(EquivalentClasses(ops))
    } else {
        Component::SubClassOf(SubClassOf { sub: lc, sup: rc })
    };
    Ok(Some(comp.into()))
}

fn push_tagged(
    out: &mut BTreeMap<String, Vec<AnnotatedComponent<Str>>>,
    tags: &[String],
    ax: AnnotatedComponent<Str>,
) {
    if tags.is_empty() {
        out.entry(String::new()).or_default().push(ax);
    } else {
        for t in tags {
            out.entry(t.clone()).or_default().push(ax.clone());
        }
    }
}

/// Per-mapping evaluation context: the mutable slot values + set_var variables.
struct Ctx {
    subject_id: String,
    object_id: String,
    predicate_id: String,
    cardinality: String,
    vars: HashMap<String, String>,
    /// `set_var`s that carry a filter, in rule order. They are not decided when
    /// the rule is reached — see `Engine::resolve_var`.
    conditional_vars: Vec<(String, Filter, String)>,
    stopped: bool,
    /// The whole mapping row. `/annots="mapping_justification,author_id,…"` reads
    /// slots the four fields above do not carry, so the row has to travel with
    /// the context rather than be projected down to subject/object/predicate.
    meta: super::Mapping,
}

impl Ctx {
    fn from_mapping(m: &super::Mapping) -> Self {
        Ctx {
            subject_id: m.get("subject_id").cloned().unwrap_or_default(),
            object_id: m.get("object_id").cloned().unwrap_or_default(),
            predicate_id: m.get("predicate_id").cloned().unwrap_or_default(),
            cardinality: m.get("mapping_cardinality").cloned().unwrap_or_default(),
            vars: HashMap::new(),
            conditional_vars: Vec::new(),
            stopped: false,
            meta: m.clone(),
        }
    }
    fn slot(&self, name: &str) -> String {
        match name {
            "subject_id" => self.subject_id.clone(),
            "object_id" => self.object_id.clone(),
            "predicate_id" | "predicate" => self.predicate_id.clone(),
            "subject" => self.subject_id.clone(),
            "object" => self.object_id.clone(),
            "cardinality" | "mapping_cardinality" => self.cardinality.clone(),
            // Then a `set_var` name, and finally the mapping's own slot — a rule
            // may substitute any of them, `%{subject_label}` as readily as
            // `%{subject_id}`.
            other => self
                .vars
                .get(other)
                .cloned()
                .or_else(|| self.meta.get(other).cloned())
                .unwrap_or_default(),
        }
    }
    fn invert(&mut self) {
        std::mem::swap(&mut self.subject_id, &mut self.object_id);
        // A DIRECTED predicate turns round with the sides it relates: what is a
        // narrow match from A to B is a broad match from B to A. A symmetric one
        // — `exactMatch`, `closeMatch`, `relatedMatch`, `crossSpeciesExactMatch` —
        // reads the same either way and is left alone.
        //
        // UBERON's SCTID and NCIT bridges are the case: their rows are
        // `skos:narrowMatch` with UBERON on the subject side, the preamble
        // inverts them to put UBERON on the object side, and the bridge rules
        // then select `predicate==skos:broadMatch`. Leaving the predicate as it
        // was left both bridges with every label and none of their 8,159
        // subsumptions.
        self.predicate_id = inverse_predicate(&self.predicate_id);
        // `mapping_cardinality` is oriented SUBJECT:OBJECT, so inverting the
        // mapping inverts it too. Leaving it alone made UBERON's very next rule,
        // `!cardinality==*:1 -> stop()`, test the pre-inversion orientation and
        // drop 291 mappings where ROBOT drops 25.
        if let Some((a, b)) = self.cardinality.split_once(':') {
            self.cardinality = format!("{b}:{a}");
        }
        // Inverting exchanges the two sides wholly, so every paired slot goes with
        // them — a rule that substitutes `%{subject_label}` after an invert() must
        // read what was the object's label.
        for (a, b) in [
            ("subject_id", "object_id"),
            ("subject_label", "object_label"),
            ("subject_category", "object_category"),
            ("subject_source", "object_source"),
            ("subject_source_version", "object_source_version"),
            ("subject_type", "object_type"),
            ("subject_match_field", "object_match_field"),
            ("subject_preprocessing", "object_preprocessing"),
        ] {
            let (x, y) = (self.meta.get(a).cloned(), self.meta.get(b).cloned());
            match x {
                Some(v) => drop(self.meta.insert(b.to_string(), v)),
                None => drop(self.meta.remove(b)),
            }
            match y {
                Some(v) => drop(self.meta.insert(a.to_string(), v)),
                None => drop(self.meta.remove(a)),
            }
        }
    }
}

/// The predicate a mapping carries when read from the other side.
///
/// Only the local name decides, so a CURIE and the IRI it expands to invert
/// alike; anything with no inverse comes back unchanged.
fn inverse_predicate(p: &str) -> String {
    let cut = p.rfind(['#', '/', ':']).map_or(0, |i| i + 1);
    let (head, local) = p.split_at(cut);
    let flipped = match local {
        "narrowMatch" => "broadMatch",
        "broadMatch" => "narrowMatch",
        "narrower" => "broader",
        "broader" => "narrower",
        "narrowerTransitive" => "broaderTransitive",
        "broaderTransitive" => "narrowerTransitive",
        _ => return p.to_string(),
    };
    format!("{head}{flipped}")
}

/// Glob match with a single `*` ANYWHERE in the pattern.
///
/// Handling only a trailing `*` was enough for `subject==UBERON:*` and wrong for
/// UBERON's very next rule, `!cardinality==*:1`: `"*:1"` does not end in `*`, so
/// the match fell through to string equality, `cardinality==*:1` was false for
/// every mapping, and the negated filter `stop()`ped all of them. That is why
/// `components/mappings.owl` came out with none of its 879 axioms.
fn glob_match(glob: &str, val: &str) -> bool {
    if let Some((pre, suf)) = glob.split_once('*') {
        return val.len() >= pre.len() + suf.len()
            && val.starts_with(pre)
            && val.ends_with(suf);
    }
    if let Some(prefix) = glob.strip_suffix('*') {
        val.starts_with(prefix)
    } else {
        glob == val
    }
}

/// Write generated axioms to per-tag bridge ontologies named by the dispatch table.
/// `dir` is the directory the dispatch table itself sits in: a `file:` entry names
/// a bridge beside the table that lists it, not beside whatever directory the
/// build happens to be standing in.
fn write_dispatched(
    prefixes: &BTreeMap<String, String>,
    out: &mut BTreeMap<String, Vec<AnnotatedComponent<Str>>>,
    table: &DispatchTable,
    dir: &Path,
    version: &str,
) -> Result<()> {
    let _ = prefixes;
    let build = horned_owl::model::Build::new();
    for (tag, axs) in out.iter_mut() {
        if tag.is_empty() {
            continue;
        }
        let Some(entry) = table.entries.get(tag) else { continue };
        let Some(file) = &entry.file else { continue };
        let mut m = Model::new();
        // A bridge is a NEW ontology, so it inherits no prefix map: it declares
        // the namespaces its own axioms use and nothing else. Left inheriting,
        // it wrote `dc`, `oboInOwl` and every other default binding into a file
        // that mentions two namespaces.
        m.format_prefixes_cleared = true;
        for ax in axs.drain(..) {
            m.ont.insert(ax);
        }
        // `add-axiom: <Manchester>` entries (e.g. the taxon-constraint axiom that
        // makes a species bridge sound).
        for spec in &entry.add_axioms {
            if let Some(ax) = manchester_axiom(&build, prefixes, spec)? {
                m.ont.insert(ax);
            }
        }
        // The dc-* directives are the bridge's own ontology annotations, under
        // `http://purl.org/dc/terms/`. They are the bridge's title, description and
        // credits, so a bridge that drops them is unattributed — and the
        // annotation properties they use have to be declared alongside them.
        for (local, value) in
            table.defaults.annotations.iter().chain(entry.annotations.iter())
        {
            let ap = build.annotation_property(format!("http://purl.org/dc/terms/{local}"));
            m.ont.insert(Component::OntologyAnnotation(horned_owl::model::OntologyAnnotation(
                horned_owl::model::Annotation {
                    ann: Default::default(),
                    ap: ap.clone(),
                    av: AnnotationValue::Literal(Literal::Simple { literal: value.clone() }),
                },
            )));
            m.ont.insert(Component::DeclareAnnotationProperty(
                horned_owl::model::DeclareAnnotationProperty(ap),
            ));
        }
        // Ontology IRI and version IRI. `%filename` is the output's stem and
        // `%date` the version this run stamps, so one pattern in the table's
        // `__default` section names every bridge.
        let stem = Path::new(file)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let expand = |pat: &String| -> String {
            pat.replace("%filename", &stem).replace("%date", version)
        };
        let iri_pat = entry.ontology_iri.as_ref().or(table.defaults.ontology_iri.as_ref());
        let viri_pat =
            entry.ontology_version.as_ref().or(table.defaults.ontology_version.as_ref());
        if let Some(pat) = iri_pat {
            let iri = expand(pat);
            if !iri.contains('%') {
                m.ont.insert(Component::OntologyID(horned_owl::model::OntologyID {
                    iri: Some(build.iri(iri.as_str())),
                    viri: viri_pat
                        .map(expand)
                        .filter(|v| !v.contains('%'))
                        .map(|v| build.iri(v.as_str())),
                }));
            }
        }
        // A bridge names only the prefixes its own axioms use. Carrying the whole
        // input's prefix map over writes ninety `xmlns:` declarations into a file
        // that mentions four namespaces.
        crate::io::save(&mut m, &dir.join(file))
            .with_context(|| format!("writing bridge {file}"))?;
    }
    Ok(())
}
