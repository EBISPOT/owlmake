//! `sssom-cli` — the entry point OBO builds (UBERON) invoke to run a SSSOM/T rule
//! pipeline over mapping sets, as against the table-manipulation subcommands of
//! [`crate::sssom::cli`]. Supports the flags UBERON uses:
//! positional input sets, `-i/--input`, `-o/--output`, `-R/--rule`,
//! `-I/--include`, `-E/--exclude`, `-a/--include-all`, `-p/--prefix-map-from-input`,
//! `--prefix NAME=IRI`, `--mangle-iris` (accepted; entity references are stored as
//! CURIEs so it is a no-op on canonical sets), reading from stdin when no set is
//! given.

use anyhow::{Context, Result};

use crate::sssom::transform::{self, Action, Filter, Rule};
use crate::sssom::MappingSet;

/// Process-style entry point. Returns an exit code.
pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sssom-cli: {e:#}");
            1
        }
    }
}

#[derive(Default)]
struct Opts {
    inputs: Vec<String>,
    output: Option<String>,
    rules: Vec<Rule>,
    include_all: bool,
    prefix_map_from_input: bool,
    prefixes: Vec<(String, String)>,
    read_stdin: bool,
    no_stdout: bool,
    updates: Vec<OntologyUpdate>,
}

/// `--update-from-ontology <FILE>[:<opts>]` — refresh one side of every mapping
/// from an ontology. UBERON's biomappings pipeline is
/// `--update-from-ontology=$(SRC):subject,label,existence`: after its `-> invert()`
/// rule has put UBERON on the subject side, keep only the mappings whose subject
/// still EXISTS in `uberon-edit.obo` and refresh their labels from it.
struct OntologyUpdate {
    path: String,
    /// `subject` (default) or `object`.
    side: String,
    label: bool,
    existence: bool,
}

fn parse_update_spec(spec: &str) -> OntologyUpdate {
    // `FILE:opt,opt`. Split at the LAST colon so a path may contain one.
    let (path, opts) = match spec.rsplit_once(':') {
        Some((p, o)) if !p.is_empty() && !o.contains('/') => (p, o),
        _ => (spec, ""),
    };
    let mut u = OntologyUpdate {
        path: path.to_string(),
        side: "subject".into(),
        label: false,
        existence: false,
    };
    for opt in opts.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match opt {
            "subject" | "object" => u.side = opt.to_string(),
            "label" => u.label = true,
            "existence" => u.existence = true,
            _ => {}
        }
    }
    u
}

/// Apply one `--update-from-ontology` to the set.
fn apply_ontology_update(set: &mut MappingSet, u: &OntologyUpdate) -> Result<()> {
    let model = crate::io::load(std::path::Path::new(&u.path))
        .with_context(|| format!("reading ontology {}", u.path))?;
    let declared: std::collections::HashSet<String> =
        crate::cmd::select::entities(&model).classes.into_iter().collect();
    let mut labels: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for ac in model.ont.iter() {
        if let horned_owl::model::Component::AnnotationAssertion(aa) = &ac.component {
            if aa.ann.ap.0.as_ref() != "http://www.w3.org/2000/01/rdf-schema#label" {
                continue;
            }
            if let (
                horned_owl::model::AnnotationSubject::IRI(s),
                horned_owl::model::AnnotationValue::Literal(l),
            ) = (&aa.subject, &aa.ann.av)
            {
                labels.insert(s.as_ref().to_string(), l.literal().to_string());
            }
        }
    }

    let prefixes = set.effective_prefixes();
    let id_slot = format!("{}_id", u.side);
    let label_slot = format!("{}_label", u.side);
    let mut kept = Vec::with_capacity(set.mappings.len());
    for mut m in std::mem::take(&mut set.mappings) {
        let Some(id) = m.get(&id_slot).cloned() else { continue };
        let iri = crate::sssom::owl::expand(&prefixes, &id);
        if u.existence && !declared.contains(&iri) {
            continue;
        }
        if u.label {
            if let Some(lab) = labels.get(&iri) {
                m.insert(label_slot.clone(), lab.clone());
            }
        }
        kept.push(m);
    }
    set.mappings = kept;
    set.recompute_columns();
    Ok(())
}

fn next_val(args: &[String], i: &mut usize) -> Result<String> {
    let a = args[*i].clone();
    *i += 1;
    args.get(*i).cloned().ok_or_else(|| anyhow::anyhow!("missing value after {a}"))
}

fn run(args: &[String]) -> Result<i32> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        match a.as_str() {
            "-o" | "--output" => o.output = Some(next_val(args, &mut i)?),
            "-i" | "--input" => {
                let v = next_val(args, &mut i)?;
                if v == "-" {
                    o.read_stdin = true;
                } else {
                    o.inputs.push(v);
                }
            }
            "-R" | "--rule" => {
                let v = next_val(args, &mut i)?;
                o.rules.push(transform::parse_rule(&v)?);
            }
            "-I" | "--include" => {
                let v = next_val(args, &mut i)?;
                o.rules.push(Rule { filter: transform::parse_filter(&v)?, action: Action::Include });
            }
            "-E" | "--exclude" => {
                let v = next_val(args, &mut i)?;
                o.rules.push(Rule {
                    filter: Filter::Not(Box::new(transform::parse_filter(&v)?)),
                    action: Action::Include,
                });
            }
            "-a" | "--include-all" => o.include_all = true,
            "-p" | "--prefix-map-from-input" => o.prefix_map_from_input = true,
            "--prefix" => {
                let v = next_val(args, &mut i)?;
                let (n, p) = v
                    .split_once('=')
                    .or_else(|| v.split_once(':').map(|(a, b)| (a, b.trim_start())))
                    .ok_or_else(|| anyhow::anyhow!("--prefix expects NAME=IRI: {v}"))?;
                o.prefixes.push((n.trim().to_string(), p.trim().to_string()));
            }
            // Accepted no-ops / format directives owlmake handles implicitly.
            "--mangle-iris" => {
                let _ = next_val(args, &mut i)?; // its argument (e.g. `obo`)
            }
            "--no-stdout" => o.no_stdout = true,
            "-f" | "--output-format" | "--input-format" | "-m" | "--output-metadata"
            | "--prefix-map" | "--output-prefix-map" | "--assume-version" | "--force-version"
            | "-C" | "--cardinality" | "--accept-extra-metadata" | "--write-extra-metadata" => {
                let _ = next_val(args, &mut i)?;
            }
            "--no-condensation" | "--condensation" | "--no-sorting" | "--sorting"
            | "--no-propagation" | "--propagation" | "--lax" | "--no-metadata-merge" => {}
            "-" => o.read_stdin = true,
            "--update-from-ontology" => {
                let v = next_val(args, &mut i)?;
                o.updates.push(parse_update_spec(&v));
            }
            s if s.starts_with("--update-from-ontology=") => {
                o.updates.push(parse_update_spec(&s["--update-from-ontology=".len()..]));
            }
            s if s.starts_with("--") => anyhow::bail!("unsupported sssom-cli option: {s}"),
            s if s.starts_with('-') && s.len() > 1 => {
                anyhow::bail!("unsupported sssom-cli option: {s}")
            }
            // A positional input set (optionally `SET:META`).
            s => o.inputs.push(s.to_string()),
        }
        i += 1;
    }
    let _ = o.prefix_map_from_input;

    // Load & merge the input sets (their curie maps union).
    let mut set = MappingSet::new();
    let mut loaded_any = false;
    for spec in &o.inputs {
        let (path, meta) = match spec.split_once(':') {
            Some((p, m)) if !p.contains("://") => (p, Some(m)),
            _ => (spec.as_str(), None),
        };
        let ms = crate::sssom::io::read_path(
            std::path::Path::new(path),
            None,
            meta.map(std::path::Path::new),
        )
        .with_context(|| format!("reading {path}"))?;
        merge_into(&mut set, ms);
        loaded_any = true;
    }
    if o.read_stdin || !loaded_any {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).context("reading stdin")?;
        if !buf.trim().is_empty() {
            let ms = crate::sssom::io::read_table(&buf, '\t', None).context("parsing stdin set")?;
            merge_into(&mut set, ms);
        }
    }

    // A propagated entity reference becomes a CURIE on the record —
    // `obo:uberon/core.owl`, not the full IRI the set-level slot spells out. The
    // set header and the column disagree on purpose. Contracted against the MERGED
    // prefix map, not each input's own: the local xref set declares no `obo:`
    // prefix at all, and gets one only because FBbt's set brings it.
    let entity_propagatable: Vec<&str> = crate::sssom::PROPAGATABLE_SLOTS
        .iter()
        .copied()
        .filter(|s| crate::sssom::ENTITY_REFERENCE_SLOTS.contains(s))
        .collect();
    let contracted: Vec<Vec<(String, String)>> = set
        .mappings
        .iter()
        .map(|m| {
            entity_propagatable
                .iter()
                .filter_map(|slot| {
                    m.get(*slot).map(|v| ((*slot).to_string(), set.compress(v)))
                })
                .collect()
        })
        .collect();
    for (m, subs) in set.mappings.iter_mut().zip(contracted) {
        for (k, v) in subs {
            m.insert(k, v);
        }
    }

    // SSSOM/T prefix declarations (`--prefix`, `--prefix-map-from-input`).
    // owlmake already keeps the input curie map on `set`; extra `--prefix`
    // declarations extend it.
    for (n, p) in &o.prefixes {
        set.curie_map.entry(n.clone()).or_insert_with(|| p.clone());
    }

    // Apply the rule pipeline.
    transform::apply(&mut set, &o.rules, o.include_all);

    // A rule pipeline changes which mappings exist and which way round they face,
    // so any `mapping_cardinality` carried in from an input now describes a set
    // that no longer exists. It is a DERIVED slot: stale is worse than absent, and
    // the reference drops it. UBERON's `uberon.sssom.tsv` merges the local xref
    // set — which does carry the column — under `object==UBERON:* -> invert()`,
    // and comes out with no cardinality column at all. With no rules the command
    // is a pure converter and the column passes through untouched.
    if !o.rules.is_empty() || o.include_all {
        for m in &mut set.mappings {
            m.remove("mapping_cardinality");
        }
        set.recompute_columns();
    }

    // …then the ontology updates. AFTER the rules, because UBERON's
    // `object==UBERON:* -> invert()` has to have moved UBERON onto the subject
    // side before `:subject,label,existence` can find it there.
    for u in &o.updates {
        apply_ontology_update(&mut set, u)?;
    }

    // Keep only prefixes still used by surviving mappings, so the output curie
    // map declares exactly what the result references and no more.
    set.prune_curie_map();
    set.recompute_columns();

    // The SSSOM-Java header style — a bare `#`, schema slot order — because this
    // command IS `sssom-cli`. Writing the Python style put a space after every
    // `#` and sorted the slots alphabetically.
    // Ordered on the EXPANDED subject/predicate/object, not on the CURIEs — the
    // same rule `sssom:xref-extract` applies to the set it builds. The two orders
    // genuinely differ: `FMA:` expands to `purl.org/sig/ont/fma/fma`, which sorts
    // AFTER `obo/MA_`, while the CURIE `FMA:` sorts before `MA:`. Sorting on the
    // written form put `UBERON:0000002`'s matches in EMAPA, FMA, MA order against
    // the reference's EMAPA, MA, FMA, and misplaced every merged-in FBbt row —
    // 22,440 lines of diff over the same set of records.
    //
    // Over EVERY column in order, not just the three key ones, and a record that
    // LACKS a column compares as the four letters `null` — records are ordered by
    // their rendered form, in which an absent slot is written out. So a present
    // value sorts before an absent one only when it sorts before "null":
    // `UBERON:0000066`'s local matches carry `subject_label` "fully formed stage"
    // and precede the SSLSO rows that have none, while `UBERON:0000106`'s "zygote
    // stage" follows them. Sorting on the three key columns alone left 94 lines
    // misplaced; treating absent as unconditionally last left 20.
    {
        let cols = set.columns.clone();
        let entity: std::collections::HashSet<&str> =
            crate::sssom::ENTITY_REFERENCE_SLOTS.iter().copied().collect();
        let key = |m: &crate::sssom::Mapping| -> Vec<Option<String>> {
            cols.iter()
                .map(|c| {
                    m.get(c).filter(|v| !v.is_empty()).map(|v| {
                        if entity.contains(c.as_str()) { set.expand(v) } else { v.clone() }
                    })
                })
                .collect()
        };
        let mut keyed: Vec<(Vec<Option<String>>, crate::sssom::Mapping)> =
            set.mappings.iter().map(|m| (key(m), m.clone())).collect();
        keyed.sort_by(|a, b| {
            for (x, y) in a.0.iter().zip(b.0.iter()) {
                let ord = x.as_deref().unwrap_or("null").cmp(y.as_deref().unwrap_or("null"));
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
        set.mappings = keyed.into_iter().map(|(_, m)| m).collect();
    }
    let text = crate::sssom::io::write_table_styled(
        &set,
        '\t',
        true,
        false,
        crate::sssom::io::MetaStyle::Java,
    )?;
    if let Some(out) = &o.output {
        std::fs::write(out, &text).with_context(|| format!("writing {out}"))?;
        if !o.no_stdout && o.output.is_none() {
            print!("{text}");
        }
    } else if !o.no_stdout {
        print!("{text}");
    }
    Ok(0)
}

/// Merge `src` into `dst`: union curie maps (first declaration wins) and append
/// mappings.
/// Merge one input set into the accumulating one.
///
/// Each set's PROPAGATABLE metadata lands on its OWN mappings first, because a
/// merged set cannot carry one `subject_source` for rows drawn from several
/// ontologies. That is the SSSOM propagation rule, and skipping it lost the
/// distinction entirely: UBERON's `uberon.sssom.tsv` merges the local xref set
/// with FBbt's and SSLSO's, and the reference tags each row with the source it
/// came from — 354 rows `obo:uberon.owl`/`obo:fbbt.owl`/`2024-07-12`, 269 rows
/// `obo:life-stages.owl`, 30,585 rows `obo:uberon/core.owl` — where om wrote no
/// such columns at all. (The FBbt rows read inverted because the set's own
/// `object==UBERON:* -> invert()` rule swaps the two source slots with the
/// subject and object they describe.)
///
/// Once propagated, the slot is REMOVED from that set's metadata: it is now a
/// property of the records, and leaving it at set level would re-assert one
/// input's source over every other input's rows.
///
/// The remaining (non-propagatable) metadata is unioned, first set winning a
/// contested slot — so the merged set keeps the first `mapping_set_id` and
/// `license` while still picking up a `creator_id` that only a later set has.
fn merge_into(dst: &mut MappingSet, mut src: MappingSet) {
    src.propagate();
    for slot in crate::sssom::PROPAGATABLE_SLOTS {
        src.metadata.remove(*slot);
    }
    for (k, v) in src.curie_map {
        dst.curie_map.entry(k).or_insert(v);
    }
    // Only a MULTIVALUED slot merges across sets; a single-valued one belongs to
    // the first set and a later set's is dropped rather than filling the gap. So
    // the merged header picks up FBbt's `creator_id` list but not its
    // `mapping_set_description`, which describes FBbt's set and not this one.
    // `first` is read BEFORE the loop: testing `is_empty()` inside it let only the
    // first slot of the first set through, which lost `mapping_set_id`.
    let first = dst.metadata.is_empty();
    for (k, v) in src.metadata {
        if first || crate::sssom::MULTIVALUED_SLOTS.contains(&k.as_str()) {
            dst.metadata.entry(k).or_insert(v);
        }
    }
    dst.mappings.extend(src.mappings);
}
