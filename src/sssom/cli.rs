//! The `owlmake sssom` command line. It is intercepted in `main` before the
//! command-chaining harness (a SSSOM command line has positional inputs and
//! `--<slot>` options that the chainer must not see), then parsed here
//! command-by-command: each subcommand carries its own long/short options,
//! `--x/--no-x` boolean pairs, and — for `filter`/`annotate` — one dynamically
//! generated `--<slot>` option per SSSOM schema slot.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::io;
use super::{
    invert_column_name, is_multivalued, MappingSet, INVERSE_PREDICATE_MAP, KEY_FEATURES, SLOT_ORDER,
};

/// Top-level entry point. Returns a process exit code.
pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sssom: error: {e:#}");
            1
        }
    }
}

const SUBCOMMANDS: &[&str] = &[
    "convert", "parse", "validate", "split", "ptable", "dedupe", "dosql", "sparql", "diff",
    "partition", "cliquesummary", "crosstab", "correlations", "merge", "rewire",
    "reconcile-prefixes", "sort", "filter", "annotate", "remove", "invert", "serve-rdf",
    "transform", "help",
];

fn run(args: &[String]) -> Result<i32> {
    // Global options precede the subcommand: -v/--verbose, -q/--quiet, --version.
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-v" | "--verbose" | "-q" | "--quiet" => i += 1,
            "--version" => {
                println!("sssom, version {} (owlmake)", env!("CARGO_PKG_VERSION"));
                return Ok(0);
            }
            "--help" | "-h" => {
                print_main_help();
                return Ok(0);
            }
            s if s.starts_with('-') => bail!("no such option: {s}"),
            _ => break,
        }
    }
    if i >= args.len() {
        print_main_help();
        return Ok(0);
    }
    let cmd = args[i].clone();
    let rest = &args[i + 1..];
    if !SUBCOMMANDS.contains(&cmd.as_str()) {
        bail!("no such command '{cmd}'.");
    }
    match cmd.as_str() {
        "convert" => cmd_convert(rest),
        "parse" => cmd_parse(rest),
        "validate" => cmd_validate(rest),
        "split" => cmd_split(rest),
        "dedupe" => cmd_dedupe(rest),
        "diff" => cmd_diff(rest),
        "merge" => cmd_merge(rest),
        "sort" => cmd_sort(rest),
        "filter" => cmd_filter(rest),
        "annotate" => cmd_annotate(rest),
        "remove" => cmd_remove(rest),
        "invert" => cmd_invert(rest),
        "reconcile-prefixes" => cmd_reconcile_prefixes(rest),
        "crosstab" => cmd_crosstab(rest),
        "correlations" => cmd_crosstab(rest), // same tabulation engine
        "partition" => cmd_partition(rest),
        "cliquesummary" => cmd_cliquesummary(rest),
        "rewire" => cmd_rewire(rest),
        // The SSSOM/Transform rule engine, reachable both as
        // `owlmake sssom transform` and through the `sssom-cli` entry point;
        // the two share one mapping model.
        "transform" => Ok(crate::sssom::sssom_cli::main(rest)),
        "help" => {
            print_main_help();
            Ok(0)
        }
        // Commands requiring machinery owlmake does not (yet) embed. They parse
        // their args (so `--help`/scripts don't choke) and report clearly.
        "ptable" => unsupported("ptable", rest),
        "dosql" => cmd_dosql(rest),
        "sparql" => unsupported("sparql", rest),
        "serve-rdf" => unsupported("serve-rdf", rest),
        _ => unreachable!(),
    }
}

fn unsupported(name: &str, _rest: &[String]) -> Result<i32> {
    bail!("`sssom {name}` is recognised but not yet implemented in owlmake")
}

// ────────────────────────────── tiny option parser ──────────────────────────

/// A value option: every accepted spelling (`-o`, `--output`) maps to a canonical
/// name and a value arity (1 or 2).
struct Val(&'static str, usize, &'static [&'static str]);
/// A boolean option: every accepted spelling maps to canonical name + the value
/// it sets (so `--no-foo` → ("foo", false)).
struct Flag(&'static str, bool, &'static [&'static str]);

#[derive(Default)]
struct Parsed {
    pos: Vec<String>,
    vals: BTreeMap<String, Vec<String>>,
    flags: BTreeMap<String, bool>,
}
impl Parsed {
    fn val(&self, k: &str) -> Option<&str> {
        self.vals.get(k).and_then(|v| v.first()).map(String::as_str)
    }
    fn multi(&self, k: &str) -> Vec<String> {
        self.vals.get(k).cloned().unwrap_or_default()
    }
    fn flag(&self, k: &str, default: bool) -> bool {
        self.flags.get(k).copied().unwrap_or(default)
    }
}

/// Parse `tokens` against the value/flag specs. When `dynamic` is set, an unknown
/// `--name`/`--name=val` is accepted as a single-value option named `name` (this
/// is how `filter`/`annotate` accept one `--<slot>` per schema slot).
fn parse(tokens: &[String], vals: &[Val], flags: &[Flag], dynamic: bool) -> Result<Parsed> {
    let mut out = Parsed::default();
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok == "--help" || tok == "-h" {
            // Defer to a generic note; full per-command help text is large.
            println!("(owlmake sssom) options follow the upstream sssom CLI.");
            std::process::exit(0);
        }
        if tok.starts_with('-') && tok != "-" {
            let (name, inline) = match tok.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (tok.clone(), None),
            };
            if let Some(Flag(canon, set, _)) = flags.iter().find(|f| f.2.contains(&name.as_str())) {
                out.flags.insert(canon.to_string(), *set);
                i += 1;
                continue;
            }
            if let Some(Val(canon, n, _)) = vals.iter().find(|v| v.2.contains(&name.as_str())) {
                let mut collected = Vec::new();
                if let Some(v) = inline {
                    collected.push(v);
                }
                i += 1;
                while collected.len() < *n {
                    let v = tokens.get(i).with_context(|| format!("option {name} requires a value"))?;
                    collected.push(v.clone());
                    i += 1;
                }
                out.vals.entry(canon.to_string()).or_default().extend(collected);
                continue;
            }
            // Short combined form like `-oFILE`.
            if !name.starts_with("--") && name.len() > 2 {
                let short = &name[..2];
                if let Some(Val(canon, _, _)) = vals.iter().find(|v| v.2.contains(&short)) {
                    out.vals.entry(canon.to_string()).or_default().push(name[2..].to_string());
                    i += 1;
                    continue;
                }
            }
            if dynamic && name.starts_with("--") {
                let canon = name[2..].replace('-', "_");
                let v = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        tokens.get(i).with_context(|| format!("option {name} requires a value"))?.clone()
                    }
                };
                out.vals.entry(canon).or_default().push(v);
                i += 1;
                continue;
            }
            bail!("no such option: {name}");
        } else {
            out.pos.push(tok.clone());
            i += 1;
        }
    }
    Ok(out)
}

// shared option specs
fn output_opt() -> Val {
    Val("output", 1, &["-o", "--output"])
}

/// Open the configured output sink (file or stdout).
fn writer(output: Option<&str>) -> Result<Box<dyn Write>> {
    match output {
        Some(p) if p != "-" => Ok(Box::new(
            std::fs::File::create(p).with_context(|| format!("creating {p}"))?,
        )),
        _ => Ok(Box::new(std::io::stdout())),
    }
}

/// Serialize a mapping set to `output` in `format` (defaults: explicit format,
/// else output extension, else tsv).
fn write_output(
    ms: &MappingSet,
    output: Option<&str>,
    format: Option<&str>,
    condense: bool,
) -> Result<()> {
    let fmt = format
        .map(str::to_string)
        .or_else(|| {
            output
                .filter(|o| *o != "-")
                .and_then(|o| Path::new(o).extension().and_then(|e| e.to_str()).map(str::to_string))
        })
        .unwrap_or_else(|| "tsv".to_string());
    let mut w = writer(output)?;
    let text = match fmt.as_str() {
        "tsv" => io::write_table(ms, '\t', condense, false)?,
        "csv" => io::write_table(ms, ',', condense, false)?,
        "json" => io::to_json(ms, condense)?,
        "owl" => io::to_turtle(ms, true)?,
        "rdf" | "ttl" | "turtle" => io::to_turtle(ms, false)?,
        "nt" | "xml" => {
            eprintln!("sssom: serialisation '{fmt}' not supported, using turtle instead.");
            io::to_turtle(ms, false)?
        }
        "ontoportal_json" => to_ontoportal_json(ms)?,
        "fhir_json" => to_fhir_json(ms)?,
        other => bail!("Unknown output format: {other}"),
    };
    w.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        w.write_all(b"\n")?;
    }
    Ok(())
}

fn require_input(p: &Parsed, cmd: &str) -> Result<PathBuf> {
    p.pos
        .first()
        .map(PathBuf::from)
        .with_context(|| format!("sssom {cmd}: missing INPUT argument"))
}

// ───────────────────────────────── commands ─────────────────────────────────

fn cmd_convert(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[output_opt(), Val("output_format", 1, &["-O", "--output-format"])],
        &[
            Flag("propagate", true, &["--propagate"]),
            Flag("propagate", false, &["--no-propagate"]),
            Flag("condense", true, &["--condense"]),
            Flag("condense", false, &["--no-condense"]),
            Flag("canonical", true, &["--canonical"]),
        ],
        false,
    )?;
    let input = require_input(&p, "convert")?;
    let mut ms = io::read_path(&input, None, None)?;
    if p.flag("propagate", true) {
        ms.propagate();
    }
    // `--canonical` rewrites the set into canonical SSSOM/TSV form before writing.
    if p.flag("canonical", false) {
        ms.canonicalize();
    }
    write_output(&ms, p.val("output"), p.val("output_format"), p.flag("condense", true))?;
    Ok(0)
}

fn cmd_parse(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[
            Val("input_format", 1, &["-I", "--input-format"]),
            Val("metadata", 1, &["-m", "--metadata"]),
            output_opt(),
            Val("mapping_predicate_filter", 1, &["-F", "--mapping-predicate-filter"]),
            Val("prefix_map_mode", 1, &["-C", "--prefix-map-mode"]),
        ],
        &[
            Flag("propagate", true, &["--propagate"]),
            Flag("propagate", false, &["--no-propagate"]),
            Flag("condense", true, &["--condense"]),
            Flag("condense", false, &["--no-condense"]),
            Flag("clean_prefixes", true, &["-p", "--clean-prefixes"]),
            Flag("clean_prefixes", false, &["--no-clean-prefixes"]),
            Flag("strict_clean_prefixes", true, &["--strict-clean-prefixes"]),
            Flag("strict_clean_prefixes", false, &["--no-strict-clean-prefixes"]),
            Flag("embedded_mode", true, &["-E", "--embedded-mode"]),
            Flag("embedded_mode", false, &["--non-embedded-mode"]),
        ],
        false,
    )?;
    let input = require_input(&p, "parse")?;
    // owlmake builds the effective prefix map by merging the file's curie_map with
    // the built-in prefixes — the `merged` mode. Other modes aren't supported —
    // warn rather than silently ignore the flag.
    if let Some(mode) = p.val("prefix_map_mode") {
        if mode != "merged" {
            eprintln!("sssom parse: --prefix-map-mode '{mode}' is not supported; using 'merged'");
        }
    }
    if !p.flag("embedded_mode", true) {
        eprintln!("sssom parse: --non-embedded-mode is not supported; writing an embedded TSV");
    }
    let metadata = p.val("metadata").map(PathBuf::from);
    let mut ms = io::read_path(&input, p.val("input_format"), metadata.as_deref())?;

    // Predicate filter (repeatable) keeps only listed predicates.
    let preds = p.multi("mapping_predicate_filter");
    if !preds.is_empty() {
        ms.mappings.retain(|m| m.get("predicate_id").map(|v| preds.contains(v)).unwrap_or(false));
    }

    // `mapping_set_id` and `license` are required of a conformant set: supply a
    // default for each when the input carries none.
    ms.metadata
        .entry("mapping_set_id".into())
        .or_insert_with(|| serde_yaml::Value::String(format!("{}mappings/owlmake", super::SSSOM_URI_PREFIX)));
    ms.metadata
        .entry("license".into())
        .or_insert_with(|| serde_yaml::Value::String(super::DEFAULT_LICENSE.into()));

    if p.flag("propagate", true) {
        ms.propagate();
    }
    if p.flag("clean_prefixes", true) {
        clean_prefixes(&mut ms, p.flag("strict_clean_prefixes", true))?;
    }
    ms.recompute_columns();
    // A parsed table is written in the schema's slot order, whatever order the
    // input file happened to use, so the header of a parsed table depends on the
    // schema alone. A generator that emits `subject_source` beside `subject_label`
    // still yields a table with it after `license`, where the schema places it.
    ms.sort_columns();
    // And its ROWS are in that same order: parsing is a canonicalisation, so a
    // parsed table is sorted on every column it has, left to right. uPheno's
    // `../mappings/upheno-species-independent.sssom.tsv` is built from one —
    // `sssom parse … --metadata config/upheno-species-independent.sssom.yml` —
    // and keeping the input's order left all but 24 of its 6,000-odd rows in a
    // different place.
    ms.sort_rows();
    // -E/--non-embedded-mode is honoured only for tsv; we always embed (the
    // common case); a follow-up could split the .yaml sidecar.
    write_output(&ms, p.val("output"), None, p.flag("condense", true))?;
    Ok(0)
}

/// Drop `curie_map` entries whose prefix is unused; in strict mode, error when a
/// CURIE uses a prefix that is neither declared nor built-in.
fn clean_prefixes(ms: &mut MappingSet, strict: bool) -> Result<()> {
    let mut used = ms.prefixes_used();
    if strict {
        for p in &used {
            if !ms.curie_map.contains_key(p) && io::builtin_namespace(p).is_none() {
                bail!("undeclared prefix in CURIE: {p}");
            }
        }
    }
    // `get_prefixes_used_in_metadata` starts from `set(SSSOM_BUILT_IN_PREFIXES)`,
    // and `clean_prefix_map` unions that in — so a set whose metadata is non-empty
    // (every set: `license` and `mapping_set_id` are defaulted) keeps all six
    // whether or not a CURIE uses them. MONDO's `mappings/mondo.sssom.tsv`
    // declares `rdf` and `rdfs` for exactly this reason and no other.
    for (p, ns) in SSSOM_BUILT_IN_PREFIXES {
        used.insert((*p).to_string());
        ms.curie_map.entry((*p).to_string()).or_insert_with(|| (*ns).to_string());
    }
    ms.curie_map.retain(|p, _| used.contains(p));
    Ok(())
}

/// `SSSOM_BUILT_IN_PREFIXES` with the namespaces the schema context binds them to.
const SSSOM_BUILT_IN_PREFIXES: &[(&str, &str)] = &[
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("semapv", "https://w3id.org/semapv/vocab/"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("sssom", "https://w3id.org/sssom/"),
];

fn cmd_validate(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[Val("validation_types", 1, &["-V", "--validation-types"])],
        &[
            Flag("propagate", true, &["--propagate"]),
            Flag("propagate", false, &["--no-propagate"]),
        ],
        false,
    )?;
    let input = require_input(&p, "validate")?;
    let mut ms = io::read_path(&input, None, None)?;
    if p.flag("propagate", true) {
        ms.propagate();
    }
    // Map the requested validation types to conformance categories. With no
    // `--validation-types`, run the full SSSOM 1.1 conformance check.
    let requested = p.multi("validation_types");
    let mut cats: Vec<&str> = Vec::new();
    let mut want_structure = false;
    if requested.is_empty() {
        cats.extend_from_slice(super::conformance::ALL_CATEGORIES);
        want_structure = true;
    } else {
        for t in &requested {
            match t.as_str() {
                // The LinkML/JSON-schema-style structural check covers required
                // slots, enums, datatypes, cross-field rules, version, extensions
                // and record-id consistency.
                "JsonSchema" => cats.extend_from_slice(&[
                    "required", "enum", "datatype", "crossfield", "version", "extension",
                    "record_id",
                ]),
                "PrefixMapCompleteness" => cats.push("prefix"),
                "StrictCurieFormat" => cats.push("curie"),
                "structure" => want_structure = true,
                "Shacl" | "Sparql" => {
                    eprintln!("sssom validate: '{t}' validation is not implemented; skipping.");
                }
                // Allow naming a conformance category directly.
                c if super::conformance::ALL_CATEGORIES.contains(&c) => cats.push(c),
                other => bail!("unknown validation type: {other}"),
            }
        }
    }
    let mut errors = super::conformance::run(&ms, &cats);
    // Structural checks need the raw text and only apply to the SSSOM/TSV form.
    if want_structure {
        let is_tsv = input
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e, "tsv" | "csv" | "sssom"))
            .unwrap_or(true);
        if is_tsv {
            if let Ok(text) = std::fs::read_to_string(&input) {
                errors.extend(super::conformance::structure(&text));
            }
        }
    }
    if errors.is_empty() {
        Ok(0)
    } else {
        for e in &errors {
            eprintln!("{e}");
        }
        Ok(1)
    }
}

fn cmd_sort(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[output_opt()],
        &[
            Flag("by_columns", true, &["-k", "--by-columns"]),
            Flag("by_rows", true, &["-r", "--by-rows"]),
        ],
        false,
    )?;
    let input = require_input(&p, "sort")?;
    let mut ms = io::read_path(&input, None, None)?;
    if p.flag("by_columns", true) {
        ms.sort_columns();
    }
    if p.flag("by_rows", true) {
        ms.sort_rows();
    }
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&ms, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

/// `sssom dosql -Q "<SQL>" <FILE>…` — see [`super::dosql`].
fn cmd_dosql(rest: &[String]) -> Result<i32> {
    let p = parse(rest, &[Val("query", 1, &["-Q", "--query"]), output_opt()], &[], false)?;
    let Some(query) = p.val("query") else {
        bail!("`sssom dosql` needs a query (-Q/--query)");
    };
    // `run_sql_query` binds each input to `df{n}` AND to its stemmed filename,
    // and the loop leaves the LAST one visible as the bare name `df` — which is
    // the binding MONDO's query uses.
    let inputs = p.pos.clone();
    if inputs.is_empty() {
        bail!("`sssom dosql` needs at least one input file");
    }
    let mut tables: Vec<(String, MappingSet)> = Vec::new();
    for (i, path) in inputs.iter().enumerate() {
        let path = PathBuf::from(path);
        let ms = io::read_path(&path, None, None)?;
        tables.push((format!("df{}", i + 1), ms.clone()));
        tables.push((super::dosql::stem_binding(&path), ms.clone()));
        if i + 1 == inputs.len() {
            tables.push(("df".to_string(), ms));
        }
    }
    let out = super::dosql::run(&query, &tables)?;
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&out, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

fn cmd_filter(rest: &[String]) -> Result<i32> {
    // `dynamic` accepts one `--<slot>` per mapping slot (repeatable).
    let p = parse(rest, &[output_opt()], &[], true)?;
    let input = require_input(&p, "filter")?;
    let ms = io::read_path(&input, None, None)?;

    // Every dynamic value option that names a real slot is a filter constraint.
    let mut constraints: Vec<(String, Vec<String>)> = Vec::new();
    for (k, v) in &p.vals {
        if k == "output" {
            continue;
        }
        if SLOT_ORDER.contains(&k.as_str()) {
            constraints.push((k.clone(), v.clone()));
        } else {
            bail!("no such option: --{}", k.replace('_', "-"));
        }
    }
    let mut out = ms.clone();
    out.mappings.retain(|m| {
        constraints.iter().all(|(slot, allowed)| match m.get(slot) {
            Some(val) => allowed.iter().any(|a| glob_match(a, val)),
            None => false,
        })
    });
    out.recompute_columns();
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&out, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

/// `*` wildcard match (the only glob sssom filters use).
fn glob_match(pattern: &str, value: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == value;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0;
    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if idx == 0 {
            if !value[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if let Some(found) = value[pos..].find(part) {
            pos += found + part.len();
        } else {
            return false;
        }
    }
    if let Some(last) = parts.last() {
        if !last.is_empty() && !pattern.ends_with('*') {
            return value.ends_with(last);
        }
    }
    true
}

fn cmd_annotate(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[output_opt()],
        &[Flag("replace_multivalued", true, &["--replace-multivalued"])],
        true,
    )?;
    let input = require_input(&p, "annotate")?;
    let mut ms = io::read_path(&input, None, None)?;
    let replace = p.flag("replace_multivalued", false);
    for (k, v) in &p.vals {
        if k == "output" || k == "replace_multivalued" {
            continue;
        }
        if is_multivalued(k) && !replace {
            // Append to any existing list.
            let mut existing = ms
                .metadata
                .get(k)
                .map(super::flatten_value)
                .unwrap_or_default();
            existing.extend(v.iter().cloned());
            ms.metadata.insert(
                k.clone(),
                serde_yaml::Value::Sequence(
                    existing.into_iter().map(serde_yaml::Value::String).collect(),
                ),
            );
        } else if is_multivalued(k) {
            ms.metadata.insert(
                k.clone(),
                serde_yaml::Value::Sequence(
                    v.iter().cloned().map(serde_yaml::Value::String).collect(),
                ),
            );
        } else {
            ms.metadata
                .insert(k.clone(), serde_yaml::Value::String(v.last().cloned().unwrap_or_default()));
        }
    }
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&ms, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

fn cmd_remove(rest: &[String]) -> Result<i32> {
    let p = parse(rest, &[output_opt(), Val("remove_map", 1, &["--remove-map"])], &[], false)?;
    let input = require_input(&p, "remove")?;
    let mut ms = io::read_path(&input, None, None)?;
    let rm_path = p.val("remove_map").context("sssom remove: --remove-map is required")?;
    let rm = io::read_path(Path::new(rm_path), None, None)?;
    let keys: std::collections::BTreeSet<Vec<String>> =
        rm.mappings.iter().map(|m| key_tuple(m)).collect();
    ms.mappings.retain(|m| !keys.contains(&key_tuple(m)));
    ms.recompute_columns();
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&ms, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

/// The identity tuple (subject/predicate/object/predicate_modifier) used to match
/// records across sets for `remove`, `dedupe`, and merge de-duplication.
fn key_tuple(m: &super::Mapping) -> Vec<String> {
    KEY_FEATURES.iter().map(|k| m.get(*k).cloned().unwrap_or_default()).collect()
}

fn cmd_dedupe(rest: &[String]) -> Result<i32> {
    let p = parse(rest, &[output_opt()], &[], false)?;
    let input = require_input(&p, "dedupe")?;
    let mut ms = io::read_path(&input, None, None)?;
    filter_redundant_rows(&mut ms);
    ms.recompute_columns();
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&ms, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

/// Collapse redundant rows: among rows that share a (subject, predicate, object)
/// key AND carry a parseable confidence, keep only those at the maximum
/// confidence. Rows with NO confidence are always preserved — an absent
/// confidence is not a low one, and dropping such a row would silently lose a
/// curated mapping. Exact-duplicate rows are then collapsed.
fn filter_redundant_rows(ms: &mut MappingSet) {
    let mut best: BTreeMap<Vec<String>, f64> = BTreeMap::new();
    for m in &ms.mappings {
        if let Some(c) = m.get("confidence").and_then(|x| x.parse::<f64>().ok()) {
            best.entry(spo_tuple(m))
                .and_modify(|b| {
                    if c > *b {
                        *b = c;
                    }
                })
                .or_insert(c);
        }
    }
    ms.mappings.retain(|m| match m.get("confidence").and_then(|x| x.parse::<f64>().ok()) {
        Some(c) => best.get(&spo_tuple(m)).map(|b| c >= *b).unwrap_or(true),
        None => true, // a row with no parseable confidence is preserved
    });
    dedup_exact_rows(ms);
}

/// Drop a row whose every column value repeats an earlier row.
fn dedup_exact_rows(ms: &mut MappingSet) {
    dedup_rows_excluding(ms, &[]);
}

/// Drop duplicate rows, ignoring the named columns when comparing them.
fn dedup_rows_excluding(ms: &mut MappingSet, exclude: &[&str]) {
    let mut seen = std::collections::BTreeSet::new();
    ms.mappings.retain(|m| {
        let key: Vec<(String, String)> = m
            .iter()
            .filter(|(k, _)| !exclude.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        seen.insert(key)
    });
}

fn cmd_diff(rest: &[String]) -> Result<i32> {
    let p = parse(rest, &[output_opt()], &[], false)?;
    if p.pos.len() < 2 {
        bail!("sssom diff: requires two INPUT arguments");
    }
    let a = io::read_path(Path::new(&p.pos[0]), None, None)?;
    let b = io::read_path(Path::new(&p.pos[1]), None, None)?;
    // The diff keys on the UNORDERED (subject, object) pair — two sets that relate
    // the same two terms under different predicates are still the same mapping for
    // this purpose — and emits the union as a SSSOM mapping set with a `comment`
    // column tagging UNIQUE_1 / UNIQUE_2 / COMMON_TO_BOTH. Rows common to both are
    // emitted from BOTH inputs (then identical ones collapse).
    let key = |m: &super::Mapping| {
        let s = m.get("subject_id").cloned().unwrap_or_default();
        let o = m.get("object_id").cloned().unwrap_or_default();
        if s <= o {
            (s, o)
        } else {
            (o, s)
        }
    };
    let ka: std::collections::BTreeSet<(String, String)> = a.mappings.iter().map(key).collect();
    let kb: std::collections::BTreeSet<(String, String)> = b.mappings.iter().map(key).collect();

    // Output set: a's metadata as base, merged curie maps from both.
    let mut out = MappingSet::new();
    out.metadata = a.metadata.clone();
    for src in [&a, &b] {
        for (pre, ns) in &src.curie_map {
            out.curie_map.entry(pre.clone()).or_insert_with(|| ns.clone());
        }
    }
    eprintln!(
        "sssom diff: {} vs {} — see the `comment` column (UNIQUE_1/UNIQUE_2/COMMON_TO_BOTH)",
        p.pos[0], p.pos[1]
    );
    // All of a's mappings, tagged COMMON_TO_BOTH or UNIQUE_1.
    for m in &a.mappings {
        let mut row = m.clone();
        let tag = if kb.contains(&key(m)) { "COMMON_TO_BOTH" } else { "UNIQUE_1" };
        row.insert("comment".into(), tag.into());
        out.mappings.push(row);
    }
    // b's mappings: COMMON_TO_BOTH if the pair is also in a (emit b's row too),
    // else UNIQUE_2.
    for m in &b.mappings {
        let mut row = m.clone();
        let tag = if ka.contains(&key(m)) { "COMMON_TO_BOTH" } else { "UNIQUE_2" };
        row.insert("comment".into(), tag.into());
        out.mappings.push(row);
    }
    // Collapse rows that are identical across both inputs.
    dedup_exact_rows(&mut out);
    out.recompute_columns();
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&out, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

fn spo_tuple(m: &super::Mapping) -> Vec<String> {
    ["subject_id", "predicate_id", "object_id"]
        .iter()
        .map(|k| m.get(*k).cloned().unwrap_or_default())
        .collect()
}

fn cmd_merge(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[output_opt()],
        &[
            Flag("propagate", true, &["--propagate"]),
            Flag("propagate", false, &["--no-propagate"]),
            Flag("condense", true, &["--condense"]),
            Flag("condense", false, &["--no-condense"]),
            Flag("reconcile", true, &["-R", "--reconcile"]),
        ],
        false,
    )?;
    if p.pos.is_empty() {
        bail!("sssom merge: requires at least one INPUT argument");
    }
    let mut sets: Vec<MappingSet> = Vec::new();
    let mut source_injected = 0usize;
    for input in &p.pos {
        let mut ms = io::read_path(Path::new(input), None, None)?;
        ms.propagate();
        // Stamp each record with its set id, so a merged set still says which
        // input a mapping came from.
        if let Some(id) = ms.metadata.get("mapping_set_id").map(super::value_to_cell) {
            if !ms.mappings.iter().any(|m| m.contains_key("mapping_source")) {
                for m in &mut ms.mappings {
                    m.insert("mapping_source".into(), id.clone());
                }
                // The injected column joins that input's own, at the end, which
                // is where the merged table's column order picks it up.
                if !ms.columns.iter().any(|c| c == "mapping_source") {
                    ms.columns.push("mapping_source".into());
                }
                source_injected += 1;
            }
        }
        sets.push(ms);
    }
    let mut merged = MappingSet::new();
    for s in &sets {
        for (p, n) in &s.curie_map {
            merged.curie_map.entry(p.clone()).or_insert_with(|| n.clone());
        }
        merged.mappings.extend(s.mappings.iter().cloned());
    }
    // Carry first set's metadata as the base.
    if let Some(first) = sets.first() {
        merged.metadata = first.metadata.clone();
    }
    // A plain merge concatenates and drops duplicate rows; redundant mappings are
    // only filtered when `--reconcile` asks for it. When `mapping_source` was
    // injected into more than one set, exclude it from the dedup key so
    // otherwise-identical mappings from different sources collapse.
    if source_injected > 1 {
        dedup_rows_excluding(&mut merged, &["mapping_source"]);
    } else {
        dedup_exact_rows(&mut merged);
    }
    // -R/--reconcile: collapse SPO-redundant rows by max confidence, preserving
    // rows that carry no confidence.
    if p.flag("reconcile", false) {
        filter_redundant_rows(&mut merged);
    }
    // A merged table's columns are the inputs' own, in the order the inputs bring
    // them: the first set's columns, then whatever each later set adds. It is a
    // concatenation, not a re-canonicalisation, so the schema's slot order does
    // not apply — uPheno's merge of `upheno-species-independent-eq` with the
    // parsed manual set puts `mapping_source` fifth and `subject_label` sixth,
    // where the schema would have them last and second.
    let mut order: Vec<String> = Vec::new();
    for s in &sets {
        for c in &s.columns {
            if !order.iter().any(|o| o == c) {
                order.push(c.clone());
            }
        }
    }
    merged.columns = order;
    merged.recompute_columns();
    write_output(&merged, p.val("output"), None, p.flag("condense", true))?;
    Ok(0)
}

fn cmd_invert(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[
            output_opt(),
            Val("subject_prefix", 1, &["-P", "--subject-prefix"]),
            Val("inverse_map", 1, &["--inverse-map"]),
        ],
        &[
            Flag("merge_inverted", true, &["--merge-inverted"]),
            Flag("merge_inverted", false, &["--no-merge-inverted"]),
            Flag("update_justification", true, &["--update-justification"]),
            Flag("update_justification", false, &["--no-update-justification"]),
        ],
        false,
    )?;
    let input = require_input(&p, "invert")?;
    // Inverting reads its input the way `parse` does — canonical column order,
    // rows sorted on every column — and the inverted rows are appended to that,
    // so the table it writes starts with the input in canonical order. Reading it
    // as it stands left uPheno's inverted species-independent set in the column
    // order its `merge` had produced, and its rows in the merge's order.
    let mut ms = io::read_path(&input, None, None)?;
    ms.recompute_columns();
    ms.sort_columns();
    ms.sort_rows();
    let ms = ms;
    let mut invmap: BTreeMap<String, String> =
        INVERSE_PREDICATE_MAP.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    // --inverse-map: a custom YAML predicate→inverse map augments/overrides the
    // built-in one — a flat `predicate: inverse` mapping, optionally nested under
    // an `inverse_predicate_map` key.
    if let Some(path) = p.val("inverse_map") {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading --inverse-map {path}: {e}"))?;
        let doc: serde_yaml::Value = serde_yaml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing --inverse-map {path}: {e}"))?;
        let node = doc.get("inverse_predicate_map").unwrap_or(&doc);
        if let Some(map) = node.as_mapping() {
            for (k, v) in map {
                if let (Some(k), Some(v)) = (k.as_str(), v.as_str()) {
                    invmap.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    // --subject-prefix: only invert mappings whose subject_id has this prefix.
    let subject_prefix = p.val("subject_prefix").map(|s| format!("{s}:"));
    let update_just = p.flag("update_justification", true);

    let mut inverted: Vec<super::Mapping> = Vec::new();
    for m in &ms.mappings {
        if let Some(pfx) = &subject_prefix {
            if !m.get("subject_id").map(|s| s.starts_with(pfx.as_str())).unwrap_or(false) {
                continue;
            }
        }
        let Some(pred) = m.get("predicate_id") else { continue };
        let Some(new_pred) = invmap.get(pred) else { continue };
        let mut row = super::Mapping::new();
        for (k, v) in m {
            let nk = invert_column_name(k).map(str::to_string).unwrap_or_else(|| k.clone());
            row.insert(nk, v.clone());
        }
        row.insert("predicate_id".into(), new_pred.clone());
        if update_just {
            row.insert("mapping_justification".into(), "semapv:MappingInversion".into());
        }
        inverted.push(row);
    }

    let mut out = ms.clone();
    if p.flag("merge_inverted", true) {
        out.mappings.extend(inverted);
    } else {
        out.mappings = inverted;
    }
    // De-duplicate.
    let mut seen = std::collections::BTreeSet::new();
    out.mappings.retain(|m| {
        let key: Vec<(String, String)> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        seen.insert(key)
    });
    out.recompute_columns();
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&out, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

fn cmd_reconcile_prefixes(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[output_opt(), Val("reconcile_prefix_file", 1, &["-p", "--reconcile-prefix-file"])],
        &[],
        false,
    )?;
    let input = require_input(&p, "reconcile-prefixes")?;
    let mut ms = io::read_path(&input, None, None)?;
    let file = p
        .val("reconcile_prefix_file")
        .context("sssom reconcile-prefixes: --reconcile-prefix-file is required")?;
    let text = std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?;
    let cfg: serde_yaml::Value = serde_yaml::from_str(&text).context("parsing reconcile file")?;
    // The reconciliation file maps a canonical prefix -> namespace, plus a
    // `prefix_synonyms` map of alias -> canonical. We rewrite CURIEs accordingly.
    let mut synonyms: BTreeMap<String, String> = BTreeMap::new();
    if let Some(syn) = cfg.get("prefix_synonyms").and_then(|v| v.as_mapping()) {
        for (k, v) in syn {
            if let (Some(k), Some(v)) = (k.as_str(), v.as_str()) {
                synonyms.insert(k.to_string(), v.to_string());
            }
        }
    }
    let mut canonical: BTreeMap<String, String> = BTreeMap::new();
    if let Some(pm) = cfg.get("prefixes").and_then(|v| v.as_mapping()) {
        for (k, v) in pm {
            if let (Some(k), Some(v)) = (k.as_str(), v.as_str()) {
                canonical.insert(k.to_string(), v.to_string());
            }
        }
    }
    let rewrite = |val: &str| -> String {
        match val.split_once(':') {
            Some((pre, local)) => {
                let canon = synonyms.get(pre).cloned().unwrap_or_else(|| pre.to_string());
                format!("{canon}:{local}")
            }
            None => val.to_string(),
        }
    };
    for m in &mut ms.mappings {
        for (k, v) in m.iter_mut() {
            if super::is_entity_reference(k) {
                *v = v.split('|').map(rewrite).collect::<Vec<_>>().join("|");
            }
        }
    }
    // Update curie_map: replace synonym prefixes with canonical ones.
    let mut new_map: BTreeMap<String, String> = BTreeMap::new();
    for (pre, ns) in &ms.curie_map {
        let canon = synonyms.get(pre).cloned().unwrap_or_else(|| pre.to_string());
        let ns = canonical.get(&canon).cloned().unwrap_or_else(|| ns.clone());
        new_map.insert(canon, ns);
    }
    ms.curie_map = new_map;
    let mut w = writer(p.val("output"))?;
    w.write_all(io::write_table(&ms, '\t', true, false)?.as_bytes())?;
    Ok(0)
}

fn cmd_crosstab(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[output_opt(), Val("fields", 2, &["-f", "--fields"])],
        &[Flag("transpose", true, &["-t", "--transpose"])],
        false,
    )?;
    let input = require_input(&p, "crosstab")?;
    let ms = io::read_path(&input, None, None)?;
    let fields = p.multi("fields");
    let (rowf, colf) = if fields.len() == 2 {
        (fields[0].clone(), fields[1].clone())
    } else {
        ("subject_category".to_string(), "object_category".to_string())
    };
    let (rowf, colf) = if p.flag("transpose", false) { (colf, rowf) } else { (rowf, colf) };

    // Build the contingency table.
    let mut rows: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut cols: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for m in &ms.mappings {
        let r = m.get(&rowf).cloned().unwrap_or_default();
        let c = m.get(&colf).cloned().unwrap_or_default();
        rows.insert(r.clone());
        cols.insert(c.clone());
        *counts.entry((r, c)).or_default() += 1;
    }
    let mut w = writer(p.val("output"))?;
    let mut header = vec![rowf.clone()];
    header.extend(cols.iter().cloned());
    writeln!(w, "{}", header.join("\t"))?;
    for r in &rows {
        let mut line = vec![r.clone()];
        for c in &cols {
            line.push(counts.get(&(r.clone(), c.clone())).copied().unwrap_or(0).to_string());
        }
        writeln!(w, "{}", line.join("\t"))?;
    }
    Ok(0)
}

fn cmd_split(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[Val("output_directory", 1, &["-d", "--output-directory"]), Val("method", 1, &["--method"])],
        &[],
        false,
    )?;
    let input = require_input(&p, "split")?;
    let ms = io::read_path(&input, None, None)?;
    let dir = p.val("output_directory").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    // Group by subject_prefix + predicate local-id + object_prefix, all
    // lowercased and joined with `_`, written out as `<key>.sssom.tsv`.
    let mut groups: BTreeMap<String, MappingSet> = BTreeMap::new();
    for m in &ms.mappings {
        let (Some(s), Some(pr), Some(o)) =
            (m.get("subject_id"), m.get("predicate_id"), m.get("object_id"))
        else {
            continue;
        };
        let sp = s.split_once(':').map(|(p, _)| p).unwrap_or("");
        let op = o.split_once(':').map(|(p, _)| p).unwrap_or("");
        let pl = pr.split_once(':').map(|(_, l)| l).unwrap_or(pr);
        let key = format!("{}_{}_{}", sp.to_lowercase(), pl.to_lowercase(), op.to_lowercase());
        let entry = groups.entry(key).or_insert_with(|| {
            let mut sub = MappingSet::new();
            sub.metadata = ms.metadata.clone();
            sub.curie_map = ms.curie_map.clone();
            sub
        });
        entry.mappings.push(m.clone());
    }
    for (name, mut sub) in groups {
        sub.recompute_columns();
        let path = dir.join(format!("{name}.sssom.tsv"));
        std::fs::write(&path, io::write_table(&sub, '\t', true, false)?)
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("sssom split: wrote {}", path.display());
    }
    Ok(0)
}

fn cmd_partition(rest: &[String]) -> Result<i32> {
    let p = parse(rest, &[Val("output_directory", 1, &["-d", "--output-directory"])], &[], false)?;
    if p.pos.is_empty() {
        bail!("sssom partition: requires at least one INPUT argument");
    }
    let dir = p.val("output_directory").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    // Merge inputs, then partition mappings into connected components (cliques)
    // over the subject/object identifier graph.
    let mut all = MappingSet::new();
    for input in &p.pos {
        let ms = io::read_path(Path::new(input), None, None)?;
        for (pre, ns) in &ms.curie_map {
            all.curie_map.entry(pre.clone()).or_insert_with(|| ns.clone());
        }
        all.mappings.extend(ms.mappings.iter().cloned());
    }
    let comps = connected_components(&all.mappings);
    for (i, idxs) in comps.iter().enumerate() {
        let mut sub = MappingSet::new();
        sub.curie_map = all.curie_map.clone();
        for &j in idxs {
            sub.mappings.push(all.mappings[j].clone());
        }
        sub.recompute_columns();
        let path = dir.join(format!("partition_{i}.sssom.tsv"));
        std::fs::write(&path, io::write_table(&sub, '\t', true, false)?)?;
        eprintln!("sssom partition: wrote {}", path.display());
    }
    Ok(0)
}

/// Union-find over subject/object identifiers to find mapping cliques.
fn connected_components(maps: &[super::Mapping]) -> Vec<Vec<usize>> {
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    fn find(parent: &mut BTreeMap<String, String>, x: &str) -> String {
        let mut cur = x.to_string();
        loop {
            let p = parent.get(&cur).cloned().unwrap_or_else(|| cur.clone());
            if p == cur {
                return cur;
            }
            cur = p;
        }
    }
    fn union(parent: &mut BTreeMap<String, String>, a: &str, b: &str) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    for m in maps {
        let s = m.get("subject_id").cloned().unwrap_or_default();
        let o = m.get("object_id").cloned().unwrap_or_default();
        parent.entry(s.clone()).or_insert_with(|| s.clone());
        parent.entry(o.clone()).or_insert_with(|| o.clone());
        union(&mut parent, &s, &o);
    }
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, m) in maps.iter().enumerate() {
        let s = m.get("subject_id").cloned().unwrap_or_default();
        let root = find(&mut parent, &s);
        groups.entry(root).or_default().push(i);
    }
    groups.into_values().collect()
}

fn cmd_cliquesummary(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[output_opt(), Val("metadata", 1, &["-m", "--metadata"]), Val("statsfile", 1, &["-s", "--statsfile"])],
        &[],
        false,
    )?;
    let input = require_input(&p, "cliquesummary")?;
    let ms = io::read_path(&input, None, None)?;
    let comps = connected_components(&ms.mappings);
    let mut w = writer(p.val("output"))?;
    writeln!(w, "clique_id\tsize\tmembers")?;
    for (i, idxs) in comps.iter().enumerate() {
        let mut members: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for &j in idxs {
            if let Some(s) = ms.mappings[j].get("subject_id") {
                members.insert(s.clone());
            }
            if let Some(o) = ms.mappings[j].get("object_id") {
                members.insert(o.clone());
            }
        }
        writeln!(w, "{i}\t{}\t{}", members.len(), members.into_iter().collect::<Vec<_>>().join("|"))?;
    }
    Ok(0)
}

/// `rewire` — rewrite an ontology, collapsing entities linked by
/// `owl:equivalentClass`/`owl:equivalentProperty` mappings onto a single IRI:
/// subject→object by default, with `--precedence <prefix>` (repeatable) breaking
/// ambiguous targets. Reuses owlmake's ontology I/O and bulk IRI-rename core.
fn cmd_rewire(rest: &[String]) -> Result<i32> {
    let p = parse(
        rest,
        &[
            output_opt(),
            Val("mapping_file", 1, &["-m", "--mapping-file"]),
            Val("input_format", 1, &["-I", "--input-format"]),
            Val("output_format", 1, &["-O", "--output-format"]),
            Val("precedence", 1, &["--precedence"]),
        ],
        &[],
        false,
    )?;
    let input = require_input(&p, "rewire")?;
    let map_file = p.val("mapping_file").context("sssom rewire: --mapping-file is required")?;
    let ms = io::read_path(Path::new(map_file), None, None)?;
    let precedence = p.multi("precedence");
    let in_fmt = p.val("input_format").unwrap_or("turtle");
    let out_fmt = p.val("output_format").unwrap_or("turtle");

    // Build the subject→object rewire map over equivalence mappings, resolving a
    // source with two candidate targets via `--precedence`.
    let mut rewire: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for m in &ms.mappings {
        let Some(pred) = m.get("predicate_id") else { continue };
        if pred != "owl:equivalentClass" && pred != "owl:equivalentProperty" {
            continue;
        }
        let (Some(src), Some(tgt)) = (m.get("subject_id"), m.get("object_id")) else { continue };
        let (src, tgt) = (src.clone(), tgt.clone());
        match rewire.get(&src) {
            Some(curr) => {
                if precedence.is_empty() {
                    bail!("Ambiguous rewire: {src} -> {tgt} vs {curr}");
                }
                let pfx = |c: &str| c.split_once(':').map(|(p, _)| p.to_string()).unwrap_or_default();
                let tgt_idx = precedence.iter().position(|p| *p == pfx(&tgt));
                let curr_idx = precedence.iter().position(|p| *p == pfx(curr));
                if let Some(ti) = tgt_idx {
                    if curr_idx.map(|ci| ti < ci).unwrap_or(true) {
                        rewire.insert(src, tgt);
                    }
                }
            }
            None => {
                rewire.insert(src, tgt);
            }
        }
    }

    // Expand CURIEs to full IRIs and rewrite the ontology.
    let expanded: std::collections::HashMap<String, String> =
        rewire.iter().map(|(k, v)| (ms.expand(k), ms.expand(v))).collect();
    let model = crate::io::load_with(&input, Some(in_fmt))?;
    let renamed = crate::cmd::rename::rename_model(model, &expanded)?;
    let fmt = crate::io::Format::from_name(out_fmt)?;
    let mut buf = Vec::new();
    crate::io::write_to_ref(&renamed, &mut buf, fmt)?;
    let mut w = writer(p.val("output"))?;
    w.write_all(&buf)?;
    eprintln!("sssom rewire: rewired {} entit(ies)", expanded.len());
    Ok(0)
}

// ─────────────────────────── extra JSON serializers ─────────────────────────

fn to_ontoportal_json(ms: &MappingSet) -> Result<String> {
    let mut list: Vec<serde_json::Value> = Vec::new();
    for m in &ms.mappings {
        let mut obj = serde_json::Map::new();
        let subj = m.get("subject_id").map(|s| ms.expand(s)).unwrap_or_default();
        let obj_id = m.get("object_id").map(|s| ms.expand(s)).unwrap_or_default();
        obj.insert(
            "classes".into(),
            serde_json::Value::Array(vec![
                serde_json::Value::String(subj),
                serde_json::Value::String(obj_id),
            ]),
        );
        let mut put = |k: &str, v: String| {
            if !v.is_empty() {
                obj.insert(k.into(), serde_json::Value::String(v));
            }
        };
        put("subject_source_id", m.get("subject_source").cloned().unwrap_or_default());
        put("object_source_id", m.get("object_source").cloned().unwrap_or_default());
        put(
            "source_name",
            ms.metadata.get("mapping_set_id").map(super::value_to_cell).unwrap_or_default(),
        );
        if let Some(just) = m.get("mapping_justification") {
            put("source", ms.expand(just));
        }
        put("comment", m.get("comment").cloned().unwrap_or_default());
        if let Some(pred) = m.get("predicate_id") {
            obj.insert(
                "relation".into(),
                serde_json::Value::Array(vec![serde_json::Value::String(ms.expand(pred))]),
            );
        }
        list.push(serde_json::Value::Object(obj));
    }
    Ok(serde_json::to_string_pretty(&serde_json::Value::Array(list))?)
}

fn to_fhir_json(ms: &MappingSet) -> Result<String> {
    // Minimal FHIR ConceptMap rendering of the mapping set.
    let mut elements: Vec<serde_json::Value> = Vec::new();
    for m in &ms.mappings {
        let subj = m.get("subject_id").cloned().unwrap_or_default();
        let obj = m.get("object_id").cloned().unwrap_or_default();
        let equivalence = match m.get("predicate_id").map(String::as_str) {
            Some("skos:exactMatch") | Some("owl:equivalentClass") => "equivalent",
            Some("skos:broadMatch") => "wider",
            Some("skos:narrowMatch") => "narrower",
            Some("rdfs:subClassOf") => "subsumes",
            _ => "relatedto",
        };
        elements.push(serde_json::json!({
            "code": subj,
            "target": [{"code": obj, "equivalence": equivalence}],
        }));
    }
    let cm = serde_json::json!({
        "resourceType": "ConceptMap",
        "url": ms.metadata.get("mapping_set_id").map(super::value_to_cell).unwrap_or_default(),
        "group": [{"element": elements}],
    });
    Ok(serde_json::to_string_pretty(&cm)?)
}

// ──────────────────────────────── help text ─────────────────────────────────

fn print_main_help() {
    println!(
        "owlmake sssom — SSSOM mapping-set toolkit\n\
         a native Rust reimplementation of the `sssom` CLI (not the original Python tool)\n\n\
         Usage: owlmake sssom <command> [options]\n\n\
         Options:\n  -v, --verbose\n  -q, --quiet\n  --version\n  --help\n\n\
         Commands:\n  {}",
        SUBCOMMANDS.join("\n  ")
    );
}

