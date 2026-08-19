//! `ubergraph` — build the **ubergraph** data product natively.
//!
//! Given the source ontologies (a `--mirror-dir` of OBO base files, an explicit
//! `-i` list, and/or an `--ontologies` list file naming remote IRIs to download
//! and local paths to read in place), this runs the whole pipeline in one
//! binary:
//!
//! 1. **merge** the sources → **remove** disjointness axioms and `owl:Nothing`
//!    → **unmerge** the patch axioms → **reason** with the EL reasoner — the
//!    `ontologies-merged` product;
//! 2. materialize the **redundant** existential-relation graph and prune it to
//!    the **non-redundant** graph;
//! 3. compute **information content** and `rdfs:isDefinedBy`;
//! 4. assemble everything into named-graph **N-Quads** (`ubergraph.nq`) and KGX
//!    **edge tables**.
//!
//! `ubergraph.nq` is the whole dataset in one text file — every named graph, in
//! sorted order — so consumers need no binary triple-store journal to load it,
//! and a release diff is readable.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, ClassExpression as CE, Component, MutableOntology,
    ObjectPropertyExpression as OPE, SubClassOf, SubObjectPropertyExpression as SubOPE,
};

use crate::io::{self, Format};
use crate::model::Model;
use crate::reason::Reasoner;
use crate::sig;
use crate::ubergraph::{self, assemble::Dataset, edges, graph, ic, prune};

#[derive(ClapArgs)]
pub struct Args {
    /// Source ontology files to merge (repeatable). Combine with --mirror-dir
    /// and/or --ontologies.
    #[arg(short, long)]
    pub input: Vec<PathBuf>,
    /// Directory of local source ontologies; every `*.owl`/`*.ofn`/`*.ttl`/
    /// `*.obo` in it is merged (the equivalent of ubergraph's `mirror/`).
    #[arg(long)]
    pub mirror_dir: Option<PathBuf>,
    /// A list file of ontology sources to fetch and merge — one IRI/URL or local
    /// path per line (`#` comments and blank lines ignored). This is the
    /// arbitrary input set; owlmake ships no built-in list. Remote entries are
    /// downloaded into `<output-dir>/mirror/` (or `--mirror-dir` if given).
    #[arg(long, value_name = "FILE")]
    pub ontologies: Option<PathBuf>,
    /// Patch axioms to remove after merging (the role of ubergraph's
    /// `unmerge.ofn` — entirely user-supplied).
    #[arg(long)]
    pub unmerge: Option<PathBuf>,
    /// Object properties to materialize over (IRIs/CURIEs, repeatable). Empty =
    /// all.
    #[arg(short = 't', long)]
    pub term: Vec<String>,
    /// Output directory for the products.
    #[arg(short, long, default_value = "ubergraph-out")]
    pub output_dir: PathBuf,
    /// Skip products that need network access (biolink, opposites, prefixes).
    #[arg(long)]
    pub offline: bool,
    /// Base IRI for the ubergraph named graphs: `<prefix>/ontology`,
    /// `<prefix>/redundant`, `<prefix>/nonredundant`. Defaults to owlmake's own
    /// namespace; set e.g. `--graph-prefix http://reasoner.renci.org` to match
    /// the upstream ubergraph graphs.
    #[arg(long, default_value = graph::DEFAULT_PREFIX)]
    pub graph_prefix: String,
    /// Annotate every term with `rdfs:isDefinedBy = <its source ontology IRI>`
    /// during the merge, so each term is attributed to the ontology it actually
    /// came from (works for any IRI scheme, including non-OBO like EFO). Default
    /// true. Pass `--annotate-defined-by false` to fall back to the legacy
    /// post-hoc OBO-PURL heuristic (which silently drops non-OBO terms).
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub annotate_defined_by: Option<bool>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> Result<Option<Model>> {
    std::fs::create_dir_all(&args.output_dir)?;
    let out = &args.output_dir;
    let graphs = graph::Names::from_prefix(&args.graph_prefix);

    // 1. Gather + merge sources -------------------------------------------------
    let mut model = gather_sources(piped, args)?;
    status!("ubergraph: merged sources → {} components", model.ont.iter().count());

    // 2. Preprocess: remove disjointness + owl:Nothing, unmerge patch axioms ----
    let removed_disjoint = remove_disjoint(&mut model);
    let removed_nothing = remove_owl_nothing(&mut model);
    status!("ubergraph: removed {removed_disjoint} disjoint + {removed_nothing} owl:Nothing axiom(s)");
    if let Some(path) = &args.unmerge {
        let patch = io::load(path).with_context(|| format!("loading unmerge {}", path.display()))?;
        let n = unmerge(&mut model, &patch);
        status!("ubergraph: unmerged {n} patch axiom(s)");
    }

    // 3. Reason with the EL reasoner --------------------------------------------
    let props: HashSet<String> = args
        .term
        .iter()
        .map(|t| crate::cmd::select::expand(&model, t))
        .collect();
    let transitive = transitive_properties(&model);
    let subprop = subproperty_closure(&model);

    status!("ubergraph: classifying with the EL reasoner …");
    let reasoner = Reasoner::classify(&model);
    if !reasoner.is_consistent() {
        bail!("ubergraph: merged ontology is inconsistent");
    }
    let unsat = reasoner.unsatisfiable();
    if !unsat.is_empty() {
        status!("ubergraph: WARNING {} unsatisfiable class(es)", unsat.len());
    }

    // ontologies-merged: assert the inferred direct subsumptions.
    let mut merged = model;
    let mut asserted = 0usize;
    for (sub, sup) in reasoner.direct_subsumptions() {
        let comp = Component::SubClassOf(SubClassOf {
            sub: CE::Class(merged.build.class(sub)),
            sup: CE::Class(merged.build.class(sup)),
        });
        if merged.ont.insert(comp) {
            asserted += 1;
        }
    }
    status!("ubergraph: reason asserted {asserted} inferred SubClassOf axiom(s)");
    io::save_as(&mut merged, &out.join("ontologies-merged.ofn"), Format::Functional)?;
    io::save_as(&mut merged, &out.join("ontologies-merged.ttl"), Format::Turtle)?;

    // 4. Redundant + non-redundant property graphs ------------------------------
    status!("ubergraph: materializing redundant relation graph …");
    let redundant = edges::redundant_graph(&reasoner, &props);
    status!("ubergraph: redundant graph has {} edge(s)", redundant.len());
    let nonredundant = prune::nonredundant(&redundant, &transitive, &subprop);
    status!("ubergraph: non-redundant graph has {} edge(s)", nonredundant.len());
    std::fs::write(
        out.join("properties-redundant.nt"),
        ubergraph::iri_triples_to_ntriples(&redundant),
    )?;
    std::fs::write(
        out.join("properties-nonredundant.nt"),
        ubergraph::iri_triples_to_ntriples(&nonredundant),
    )?;

    // 5. Information content + isDefinedBy + build metadata ----------------------
    let ic_lines = ic::information_content(&redundant);
    std::fs::write(out.join("information-content.nt"), join_lines(&ic_lines))?;

    // rdfs:isDefinedBy: when --annotate-defined-by is on (default), per-source
    // attribution is already stamped into the merged model during the merge and
    // emitted via the ontology graph (load_model below), so skip the post-hoc
    // OBO-PURL heuristic — it only handles obolibrary IRIs and silently drops
    // non-OBO terms (e.g. EFO). Fall back to it only when annotation is disabled.
    let is_defined_by = if args.annotate_defined_by.unwrap_or(true) {
        Vec::new()
    } else {
        ubergraph::is_defined_by(subject_iris(&merged))
    };
    std::fs::write(
        out.join("is_defined_by.nt"),
        ubergraph::iri_triples_to_ntriples(&is_defined_by),
    )?;

    let build_metadata = build_metadata_nt(&graphs.ontology);

    // 6. Assemble named-graph N-Quads -------------------------------------------
    status!("ubergraph: assembling named-graph dataset …");
    let ds = Dataset::new()?;
    ds.load_model(&merged, &graphs.ontology)?;
    ds.load_iri_triples(&redundant, &graphs.redundant)?;
    ds.load_iri_triples(&nonredundant, &graphs.nonredundant)?;
    ds.load_iri_triples(&is_defined_by, &graphs.ontology)?;
    ds.load_ntriples(&join_lines(&ic_lines), &graphs.ontology)?;
    ds.load_ntriples(&build_metadata, &graphs.ontology)?;

    let mut have_biolink = false;
    if !args.offline {
        match load_network_extras(&ds, &graphs) {
            Ok(b) => have_biolink = b,
            Err(e) => status!("ubergraph: WARNING skipping network extras: {e}"),
        }
    }

    let nq = ds.to_nquads_sorted()?;
    std::fs::write(out.join("ubergraph.nq"), &nq)?;
    status!("ubergraph: wrote ubergraph.nq ({} quad(s))", ds.len());

    // 7. Graph edge tables + KGX ------------------------------------------------
    std::fs::write(
        out.join("redundant-graph-table.tsv"),
        ubergraph::assemble::edge_table(&redundant),
    )?;
    std::fs::write(
        out.join("nonredundant-graph-table.tsv"),
        ubergraph::assemble::edge_table(&nonredundant),
    )?;
    if have_biolink {
        std::fs::create_dir_all(out.join("kgx"))?;
        write_kgx(&ds, &out.join("kgx"), &graphs)?;
    } else {
        status!("ubergraph: KGX needs the biolink model (skipped — offline or download failed)");
    }

    status!("ubergraph: done → {}", out.display());
    Ok(None)
}

/// SPARQL prefixes shared by the biolink/KGX queries.
const BL_PREFIXES: &str = r#"PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX linkml: <https://w3id.org/linkml/>
PREFIX bl: <https://w3id.org/biolink/vocab/>
"#;

/// Selects the (term, category) pairs to assert as biolink categories: a
/// biolink class or slot definition that maps to an ontology term gives that
/// term — and every subclass of it — the category itself plus everything above
/// it in the `is_a`/`mixins` closure.
const BIOLINK_CATEGORIES_SELECT: &str = r#"SELECT DISTINCT ?term ?blcategory WHERE {
  VALUES ?linkml_type { linkml:ClassDefinition linkml:SlotDefinition }
  ?category rdf:type ?linkml_type .
  ?category skos:mappingRelation|skos:exactMatch|skos:narrowMatch ?mapped .
  ?category (linkml:is_a|linkml:mixins)* ?blcategory .
  ?term rdfs:subClassOf* ?mapped .
}"#;

const KGX_NODES_SELECT: &str = r#"SELECT DISTINCT ?term (MIN(?label) AS ?name) (GROUP_CONCAT(?category ; separator="|") AS ?categories)
WHERE {
  ?term rdf:type owl:Class .
  ?term rdfs:label ?label .
  ?term rdfs:subClassOf/^(skos:mappingRelation|skos:exactMatch|skos:narrowMatch)/(linkml:is_a|linkml:mixins)* ?category .
  ?category (linkml:is_a|linkml:mixins)* bl:NamedThing .
  FILTER(isIRI(?term))
}
GROUP BY ?term"#;

// KGX edges come from two queries joined in Rust rather than one. The
// most-specific-predicate filter constrains only `(relation, predicate)`,
// independently of subject and object, so expressing it inside a single edge
// query would re-evaluate it once per *result edge* — tens of thousands of them.
// The cost is the shape of that query, not the property-path closures in it:
// spareval caches path evaluation for the life of a query, so the paths are
// already cheap, and folding the two queries back into one cannot be rescued by
// making paths faster. The two pieces are the biolink
// relation→candidate-predicate map (over the biolink graph alone) and the
// class-to-class edges (over the ontology + redundant graphs); joining on the
// relation applies the filter once per (relation, predicate) pair, for the same
// result set.
const KGX_EDGE_CANDIDATES: &str = r#"SELECT DISTINCT ?relation ?predicate
WHERE {
  ?relation ^(skos:mappingRelation|skos:exactMatch|skos:narrowMatch)/(linkml:is_a|linkml:mixins)* ?predicate .
  ?predicate (linkml:is_a|linkml:mixins)* bl:related_to .
}"#;

// The biolink slot ancestor closure (`?p is_a+/mixins+ ?ancestor`), used to keep
// only the most-specific predicate per relation: a candidate that is a proper
// ancestor of another candidate for the same relation is dropped.
const KGX_PREDICATE_ANCESTORS: &str = r#"SELECT DISTINCT ?p ?ancestor
WHERE { ?p (linkml:is_a|linkml:mixins)+ ?ancestor }"#;

const KGX_CLASS_EDGES: &str = r#"SELECT DISTINCT ?subject ?relation ?object
WHERE {
  ?subject ?relation ?object .
  ?subject rdf:type owl:Class .
  ?object rdf:type owl:Class .
  FILTER(isIRI(?subject))
  FILTER(isIRI(?object))
}"#;

const BIOLINK_CATEGORY: &str = "https://w3id.org/biolink/vocab/category";
const BIOLINK_VERSION: &str = "3.0.0";

/// Write `kgx/nodes.tsv` and `kgx/edges.tsv` from the assembled dataset.
fn write_kgx(ds: &Dataset, dir: &std::path::Path, graphs: &graph::Names) -> Result<()> {
    // Nodes: class decls/labels/subClassOf (ontology) + biolink mappings.
    let (_, node_rows) = ds.select_over(
        &[graphs.ontology.as_str(), graph::BIOLINK],
        &format!("{BL_PREFIXES}{KGX_NODES_SELECT}"),
    )?;
    std::fs::write(
        dir.join("nodes.tsv"),
        ubergraph::assemble::rows_to_tsv(&["id", "name", "category"], &node_rows),
    )?;
    // Edges: biolink relation→predicate map (biolink graph) ⋈ class-to-class
    // edges (ontology + redundant graphs), joined on the relation in Rust — the
    // most-specific filter applied once per (relation, predicate), not per edge.
    use std::collections::{HashMap, HashSet};
    let (_, cand_rows) =
        ds.select_over(&[graph::BIOLINK], &format!("{BL_PREFIXES}{KGX_EDGE_CANDIDATES}"))?;
    let (_, anc_rows) =
        ds.select_over(&[graph::BIOLINK], &format!("{BL_PREFIXES}{KGX_PREDICATE_ANCESTORS}"))?;
    // predicate → its proper ancestors.
    let mut ancestors: HashMap<String, HashSet<String>> = HashMap::new();
    for r in &anc_rows {
        if r.len() == 2 {
            ancestors.entry(r[0].clone()).or_default().insert(r[1].clone());
        }
    }
    // relation → candidate predicates, then keep only the most-specific (drop a
    // predicate that is a proper ancestor of another candidate for the relation).
    let mut candidates: HashMap<String, Vec<String>> = HashMap::new();
    for r in &cand_rows {
        if r.len() == 2 {
            candidates.entry(r[0].clone()).or_default().push(r[1].clone());
        }
    }
    let mut rel_to_preds: HashMap<String, Vec<String>> = HashMap::new();
    for (rel, preds) in &candidates {
        let set: HashSet<&String> = preds.iter().collect();
        let kept: Vec<String> = preds
            .iter()
            .filter(|p| {
                !set.iter().any(|p2| {
                    *p2 != *p && ancestors.get(*p2).is_some_and(|a| a.contains(*p))
                })
            })
            .cloned()
            .collect();
        rel_to_preds.insert(rel.clone(), kept);
    }
    let (_, raw_edges) = ds.select_over(
        &[graphs.ontology.as_str(), graphs.redundant.as_str()],
        &format!("{BL_PREFIXES}{KGX_CLASS_EDGES}"),
    )?;
    let mut edge_rows: Vec<Vec<String>> = Vec::new();
    for e in &raw_edges {
        if e.len() != 3 {
            continue;
        }
        if let Some(preds) = rel_to_preds.get(&e[1]) {
            for p in preds {
                // columns: subject, predicate, object, relation
                edge_rows.push(vec![e[0].clone(), p.clone(), e[2].clone(), e[1].clone()]);
            }
        }
    }
    std::fs::write(
        dir.join("edges.tsv"),
        ubergraph::assemble::rows_to_tsv(&["subject", "predicate", "object", "relation"], &edge_rows),
    )?;
    status!("ubergraph: wrote kgx/nodes.tsv ({} nodes), kgx/edges.tsv ({} edges)", node_rows.len(), edge_rows.len());
    Ok(())
}

/// Merge the configured sources into a single model. Sources come from any of
/// `-i <file>`, `--mirror-dir <dir>`, and a user-supplied `--ontologies <list>`
/// (IRIs/URLs fetched into the mirror dir) — owlmake provides no built-in set.
fn gather_sources(piped: Option<Model>, args: &Args) -> Result<Model> {
    let mut model = piped.unwrap_or_else(Model::new);

    let mut paths: Vec<PathBuf> = args.input.clone();

    // Fetch any sources listed in the user's `--ontologies` file into the mirror
    // directory, then treat the mirror directory as a source of local files.
    let mirror_dir = args
        .mirror_dir
        .clone()
        .unwrap_or_else(|| args.output_dir.join("mirror"));
    if let Some(list) = &args.ontologies {
        fetch_ontology_list(list, &mirror_dir, &mut paths)?;
    }

    if let Some(dir) = &args.mirror_dir {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let p = entry?.path();
            if matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("owl" | "ofn" | "ttl" | "obo" | "rdf" | "omn")
            ) {
                paths.push(p);
            }
        }
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        bail!("ubergraph: no sources — pass -i <file>…, --mirror-dir <dir>, and/or --ontologies <list>");
    }

    // Merge with single-identity semantics: drop secondary ontology IDs/imports
    // so the result has one identity. When annotating defined-by, merge_into
    // stamps each secondary's entities with that secondary's ontology IRI as it
    // is merged in.
    let annotate_defined_by = args.annotate_defined_by.unwrap_or(true);
    let opts = crate::cmd::merge::MergeOptions {
        annotate_defined_by,
        ..Default::default()
    };
    let had_piped = !model.is_empty();
    for p in &paths {
        match io::load(p) {
            Ok(src) => {
                if model.is_empty() && !had_piped {
                    // First source becomes the base (keeps its prefixes + identity).
                    model = src;
                } else {
                    for (k, v) in src.prefixes.mappings() {
                        let _ = model.prefixes.add_prefix(k, v);
                    }
                    crate::cmd::merge::merge_into(&mut model, &src, &opts);
                }
                status!("  + {} ({} components total)", p.display(), model.ont.iter().count());
            }
            Err(e) => status!("  ! skipping {}: {e}", p.display()),
        }
    }

    // The base (first/piped) ontology's own entities are not stamped by the
    // merge_into loop above (which only annotates each secondary). Attribute
    // them now to the merged ontology's identity IRI. annotate_provenance skips
    // any entity that already carries rdfs:isDefinedBy, so the secondaries keep
    // their own source attribution and only the base's entities get the base IRI.
    if annotate_defined_by {
        if let Some(src) = crate::cmd::merge::ontology_iri(&model) {
            let ents = crate::cmd::merge::declared_entities(&model);
            crate::cmd::merge::annotate_provenance(&mut model, &src, &ents, &opts);
        }
    }

    Ok(model)
}

/// Drop disjointness axioms (as `remove --axioms disjoint` does).
fn remove_disjoint(model: &mut Model) -> usize {
    let before = model.ont.iter().count();
    let keep: Vec<_> = model
        .ont
        .iter()
        .filter(|ac| {
            !matches!(
                ac.component,
                Component::DisjointClasses(_)
                    | Component::DisjointUnion(..)
                    | Component::DisjointObjectProperties(_)
                    | Component::DisjointDataProperties(_)
            )
        })
        .cloned()
        .collect();
    rebuild(model, keep);
    before - model.ont.iter().count()
}

/// Drop axioms mentioning `owl:Nothing` (as `remove --term owl:Nothing` does).
fn remove_owl_nothing(model: &mut Model) -> usize {
    const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
    let before = model.ont.iter().count();
    let keep: Vec<_> = model
        .ont
        .iter()
        .filter(|ac| !sig::signature(&ac.component).iter().any(|s| s == NOTHING))
        .cloned()
        .collect();
    rebuild(model, keep);
    before - model.ont.iter().count()
}

/// Remove every component of `patch` from `model` (as `unmerge` does).
fn unmerge(model: &mut Model, patch: &Model) -> usize {
    let drop: HashSet<_> = patch.ont.iter().cloned().collect();
    let before = model.ont.iter().count();
    let keep: Vec<_> = model
        .ont
        .iter()
        .filter(|ac| !drop.contains(*ac))
        .cloned()
        .collect();
    rebuild(model, keep);
    before - model.ont.iter().count()
}

fn rebuild(model: &mut Model, comps: Vec<AnnotatedComponent<crate::model::Str>>) {
    let mut ont = horned_owl::ontology::set::SetOntology::new();
    for c in comps {
        ont.insert(c);
    }
    model.ont = ont;
}

/// Transitive object properties declared in the ontology.
fn transitive_properties(model: &Model) -> HashSet<String> {
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::TransitiveObjectProperty(tp) = &ac.component {
            if let OPE::ObjectProperty(p) = &tp.0 {
                out.insert(p.0.as_ref().to_string());
            }
        }
    }
    out
}

/// Transitively-closed, reflexive-free `rdfs:subPropertyOf` pairs (simple,
/// non-chain, non-inverse). Pruning uses them to drop an edge whose
/// sub-property already states it.
fn subproperty_closure(model: &Model) -> Vec<(String, String)> {
    use std::collections::BTreeSet;
    let mut direct: Vec<(String, String)> = Vec::new();
    for ac in model.ont.iter() {
        if let Component::SubObjectPropertyOf(spo) = &ac.component {
            if let (SubOPE::ObjectPropertyExpression(OPE::ObjectProperty(s)), OPE::ObjectProperty(p)) =
                (&spo.sub, &spo.sup)
            {
                if s.0 != p.0 {
                    direct.push((s.0.as_ref().to_string(), p.0.as_ref().to_string()));
                }
            }
        }
    }
    // Transitive closure.
    let mut closure: BTreeSet<(String, String)> = direct.iter().cloned().collect();
    loop {
        let mut added = Vec::new();
        for (a, b) in &closure {
            for (c, d) in &direct {
                if b == c && a != d && !closure.contains(&(a.clone(), d.clone())) {
                    added.push((a.clone(), d.clone()));
                }
            }
        }
        if added.is_empty() {
            break;
        }
        closure.extend(added);
    }
    closure.into_iter().collect()
}

/// IRIs that occur as subjects of the merged ontology (for `isDefinedBy`).
fn subject_iris(model: &Model) -> HashSet<String> {
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        // Every signature IRI is a candidate; the OBO-prefix filter in
        // is_defined_by narrows it. (A superset of true subjects; referenced-
        // only terms add at most a defining-source triple.)
        for s in sig::signature(&ac.component) {
            out.insert(s);
        }
    }
    out
}

/// `build-metadata`: `<ontology> dcterms:created "<now>"^^xsd:dateTime`.
fn build_metadata_nt(ontology_graph: &str) -> String {
    let now = crate::time::SystemTime::now()
        .duration_since(crate::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal ISO-8601-ish stamp; the value is inherently build-time and not
    // part of the reproducible graph content.
    format!(
        "<{}> <http://purl.org/dc/terms/created> \"{}\"^^<http://www.w3.org/2001/XMLSchema#dateTime> .\n",
        ontology_graph,
        epoch_to_iso8601(now)
    )
}

fn epoch_to_iso8601(secs: u64) -> String {
    // Compute a UTC timestamp without pulling in a date crate.
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Howard Hinnant's days-from-civil inverse (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn join_lines(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Download the network-only products (biolink model + categories, lexical
/// opposites, OBO prefixes) and fold them into the dataset. Returns whether the
/// biolink model loaded (a precondition for biolink categories + KGX).
fn load_network_extras(ds: &Dataset, graphs: &graph::Names) -> Result<bool> {
    // --- opposites from phenopposites antonyms_HP.txt → RO_0002604 (both ways).
    if let Ok(bytes) = fetch("https://raw.githubusercontent.com/Phenomics/phenopposites/master/opposites/antonyms_HP.txt")
    {
        let text = String::from_utf8_lossy(&bytes);
        let hp = |c: &str| format!("http://purl.obolibrary.org/obo/HP_{}", c.trim_start_matches("HP:"));
        let ro = "http://purl.obolibrary.org/obo/RO_0002604";
        let mut triples: Vec<crate::ubergraph::IriTriple> = Vec::new();
        for line in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() >= 2 {
                let (a, b) = (hp(cols[0]), hp(cols[1]));
                triples.push((a.clone(), ro.to_string(), b.clone()));
                triples.push((b, ro.to_string(), a));
            }
        }
        ds.load_iri_triples(&triples, &graphs.ontology)?;
        status!("ubergraph: loaded {} opposite assertion(s)", triples.len());
    }

    // --- lexically-derived opposites (+ inverse) as N-Triples.
    if let Ok(bytes) = fetch("https://raw.githubusercontent.com/NCATSTranslator/opposites/main/assertions/results/lexically-derived-opposites/lexically-derived-opposites.nt")
    {
        let nt = String::from_utf8_lossy(&bytes).to_string();
        ds.load_ntriples(&nt, &graphs.ontology)?;
        ds.load_ntriples(&invert_ntriples(&nt), &graphs.ontology)?;
    }

    // --- OBO prefixes (loaded verbatim into the ontology graph).
    if let Ok(bytes) = fetch("http://purl.obolibrary.org/meta/obo_prefixes.ttl") {
        ds.load_bytes(&bytes, oxigraph::io::RdfFormat::Turtle, &graphs.ontology)?;
    }

    // --- biolink model: download, apply the vocab→OBO namespace rewrite, load
    //     into the biolink graph.
    let url = format!(
        "https://raw.githubusercontent.com/biolink/biolink-model/v{BIOLINK_VERSION}/biolink-model.ttl"
    );
    let biolink_ttl = match fetch(&url) {
        Ok(b) => b,
        Err(e) => {
            status!("ubergraph: biolink model unavailable ({e}); skipping categories + KGX");
            return Ok(false);
        }
    };
    let rewritten = rewrite_biolink_namespaces(&biolink_ttl)?;
    ds.load_ntriples(&rewritten, graph::BIOLINK)?;
    status!("ubergraph: loaded biolink model v{BIOLINK_VERSION}");

    // --- biolink categories: select over the union, assert into the biolink
    //     graph. Skip non-IRI bindings (blank-node mixins) and never let a
    //     single bad row abort the rest of the assembly.
    // Restricted to ontology+biolink: `subClassOf*` reachability over the
    // asserted hierarchy equals reachability over the closure, so the huge
    // redundant graph is unnecessary here (and pathological for path eval).
    let (_, rows) = ds.select_over(
        &[graphs.ontology.as_str(), graph::BIOLINK],
        &format!("{BL_PREFIXES}{BIOLINK_CATEGORIES_SELECT}"),
    )?;
    let mut inserted = 0usize;
    for row in &rows {
        if row.len() == 2 && is_iri(&row[0]) && is_iri(&row[1]) {
            if ds.insert_triple(&row[0], BIOLINK_CATEGORY, &row[1], graph::BIOLINK).is_ok() {
                inserted += 1;
            }
        }
    }
    status!("ubergraph: asserted {inserted} biolink category triple(s)");
    Ok(true)
}

/// A bare term string is an IRI iff it isn't a blank node and parses as one.
fn is_iri(s: &str) -> bool {
    !s.is_empty() && !s.starts_with("_:") && oxigraph::model::NamedNodeRef::new(s).is_ok()
}

/// Fetch a URL into bytes (best-effort; gzip handled by the transport).
fn fetch(url: &str) -> Result<Vec<u8>> {
    crate::io::http_get(url)
}

/// Swap subject and object of every N-Triples line (lexical opposites inverse).
fn invert_ntriples(nt: &str) -> String {
    let mut out = String::new();
    for line in nt.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        // `<s> <p> <o> .` → `<o> <p> <s> .`
        let body = l.trim_end_matches('.').trim();
        let parts: Vec<&str> = body.splitn(3, ' ').collect();
        if parts.len() == 3 {
            out.push_str(&format!("{} {} {} .\n", parts[2].trim(), parts[1], parts[0]));
        }
    }
    out
}

/// Rewrite the biolink model's namespaces, converting CURIE-style vocab IRIs
/// `<…/biolink/vocab/PREFIX:LOCAL>` (mapping targets) into OBO IRIs
/// `<…/obo/PREFIX_LOCAL>`, then return the model as N-Triples. This is what
/// makes a biolink mapping target match the term IRI the merged ontology uses.
/// Plain biolink class/slot IRIs (no embedded colon) are left untouched.
fn rewrite_biolink_namespaces(ttl: &[u8]) -> Result<String> {
    // Parse the Turtle and re-serialize as N-Triples via a temp store.
    let store = oxigraph::store::Store::new().map_err(|e| anyhow::anyhow!("store: {e}"))?;
    store
        .load_from_slice(oxigraph::io::RdfFormat::Turtle, ttl)
        .map_err(|e| anyhow::anyhow!("parsing biolink Turtle: {e}"))?;
    let buf = store
        .dump_graph_to_writer(
            oxigraph::model::GraphNameRef::DefaultGraph,
            oxigraph::io::RdfFormat::NTriples,
            Vec::new(),
        )
        .map_err(|e| anyhow::anyhow!("serialize biolink NT: {e}"))?;
    let nt = String::from_utf8(buf).map_err(|e| anyhow::anyhow!("utf8: {e}"))?;
    let re = regex::Regex::new(r"<https://w3id\.org/biolink/vocab/([^>\s]+?):")
        .map_err(|e| anyhow::anyhow!("regex: {e}"))?;
    Ok(re
        .replace_all(&nt, "<http://purl.obolibrary.org/obo/${1}_")
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn biolink_vocab_curies_rewrite_to_obo() {
        // A mapping target `vocab/CL:0000000` → OBO; a plain class `vocab/Cell`
        // (no embedded colon) is left as-is.
        let ttl = br#"@prefix bl: <https://w3id.org/biolink/vocab/> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
bl:Cell skos:exactMatch <https://w3id.org/biolink/vocab/CL:0000000> .
"#;
        let nt = rewrite_biolink_namespaces(ttl).unwrap();
        assert!(nt.contains("<http://purl.obolibrary.org/obo/CL_0000000>"), "got: {nt}");
        assert!(nt.contains("<https://w3id.org/biolink/vocab/Cell>"), "got: {nt}");
    }

    #[test]
    fn ntriples_inversion_swaps_subject_object() {
        let nt = "<http://ex/a> <http://ex/p> <http://ex/b> .\n";
        assert_eq!(invert_ntriples(nt), "<http://ex/b> <http://ex/p> <http://ex/a> .\n");
    }
}

/// Resolve the user's `--ontologies` list: one IRI/URL or local path per line
/// (`#` comments and blank lines ignored). Local paths are added directly;
/// remote entries are downloaded into `dir` and the local copy added. The list
/// is entirely user-supplied — owlmake ships no default set.
fn fetch_ontology_list(
    list: &std::path::Path,
    dir: &std::path::Path,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let text = std::fs::read_to_string(list)
        .with_context(|| format!("reading ontologies list {}", list.display()))?;
    let entries: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let remote = entries.iter().any(|e| is_url(e));
    if remote {
        std::fs::create_dir_all(dir)?;
    }
    for entry in entries {
        if is_url(entry) {
            let name = entry.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("import.owl");
            let dest = dir.join(name);
            if !dest.exists() {
                status!("  downloading {entry}");
                match fetch(entry) {
                    Ok(bytes) => std::fs::write(&dest, &bytes)
                        .with_context(|| format!("writing {}", dest.display()))?,
                    Err(e) => {
                        status!("  ! failed {entry}: {e}");
                        continue;
                    }
                }
            }
            paths.push(dest);
        } else {
            // A local path, resolved relative to the list file's directory.
            let p = std::path::Path::new(entry);
            paths.push(if p.is_absolute() {
                p.to_path_buf()
            } else {
                list.parent().unwrap_or_else(|| std::path::Path::new(".")).join(p)
            });
        }
    }
    Ok(())
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}
