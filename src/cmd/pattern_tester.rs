//! `om simple-pattern-tester <DIR>` — ODK's `/tools/simple_pattern_tester.py`.
//!
//! MONDO's `pattern_schema_checks` is one line:
//!
//! ```text
//! pattern_schema_checks:
//!         simple_pattern_tester.py ../patterns/dosdp-patterns/
//! ```
//!
//! and it is ODK tooling, not repo content, so on a bare-`om` box it exits 127
//! and the check does not run at all. It is 107 lines of Python and does three
//! things to every `*.yaml`/`*.yml` in the directory:
//!
//! 1. validate it against the DOSDP JSON schema, fetched over HTTP;
//! 2. check that every variable a logical axiom REFERENCES is declared in the
//!    pattern's `vars` / `data_vars` / `substitutions[].out`;
//! 3. check each axiom's `text` for balanced single quotes, and that every
//!    `'quoted name'` in it is a key of `classes` or `relations`.
//!
//! ## The schema is gone, and that is the tool's behaviour, not a shortcut
//!
//! `schema_url` points at
//! `raw.githubusercontent.com/dosumis/dead_simple_owl_design_patterns/master/spec/DOSDP_schema_full.yaml`,
//! which **404s** — the repository no longer serves that path on any branch.
//! The script does not check the status: it hands `requests.get(...).text` to
//! ruamel, so the schema becomes the mapping `{404: 'Not Found'}`, and
//! `Draft7Validator({404: 'Not Found'}).is_valid(anything)` is `True` because a
//! key that is not a JSON Schema keyword imposes no constraint. Measured in the
//! ODK image, not assumed.
//!
//! So today step 1 accepts everything — because the schema is empty of
//! constraints, which is a fact about the world rather than a licence to skip
//! the step. This fetches the same URL, parses the same body, and validates
//! against whatever it gets; a schema that constrains nothing constrains
//! nothing. If the file is ever restored, the validator below does the work, and
//! a Draft-7 keyword it does not implement is a hard ERROR rather than a silent
//! pass.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use serde_yaml::Value;

use crate::model::Model;

/// Where the script fetches the schema from, verbatim.
const SCHEMA_URL: &str = "https://raw.githubusercontent.com/dosumis/dead_simple_owl_design_patterns/master/spec/DOSDP_schema_full.yaml";

#[derive(ClapArgs)]
pub struct Args {
    /// The pattern directory. The script CONCATENATES this with `*.yaml` rather
    /// than joining paths, so a trailing `/` is what makes it a directory and
    /// `patterns/foo` would glob `foo*.yaml` beside it. Reproduced exactly.
    #[arg(value_name = "DIR")]
    pub dir: Option<String>,
}

/// A side-output command: it reads pattern files and never touches the in-flight
/// ontology, so a chained model passes straight through.
pub fn step(model: Option<Model>, a: &Args) -> Result<Option<Model>> {
    run(a)?;
    Ok(model)
}

pub fn run(a: &Args) -> Result<()> {
    let Some(dir) = a.dir.as_deref() else {
        bail!("simple-pattern-tester: no pattern directory given");
    };
    if !check_dir(dir)? {
        std::process::exit(1);
    }
    Ok(())
}

/// `main()` of the script: every pattern is checked, and the exit status is the
/// AND of all of them — one bad pattern does not stop the rest being reported.
fn check_dir(dir: &str) -> Result<bool> {
    let schema = fetch_schema()?;
    let mut ok = true;
    for path in pattern_docs(dir)? {
        eprintln!("UserWarning: Checking {}", path.display());
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let pattern: Value = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        if !test_jschema(&schema, &pattern) {
            ok = false;
        }
        if !test_vars(&pattern) {
            ok = false;
        }
        if !test_text_fields(&pattern)? {
            ok = false;
        }
    }
    Ok(ok)
}

/// `requests.get(schema_url)` then `YAML(typ='safe').load(...)`. The status code
/// is not consulted — see the module docs.
fn fetch_schema() -> Result<Value> {
    let bytes = crate::io::http_get_body_any_status(SCHEMA_URL)
        .with_context(|| format!("fetching the DOSDP schema {SCHEMA_URL}"))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    serde_yaml::from_str(&text).context("parsing the DOSDP schema as YAML")
}

/// `glob.glob(argv[1] + "*.yaml")` then `+ "*.yml"`, in that order.
///
/// Sorted within each extension: `glob` returns readdir order, which is the
/// filesystem's and differs between two clones of the same repo. Nothing
/// downstream reads the order — the check's product is its exit status — and a
/// build owlmake runs twice has to say the same thing twice.
fn pattern_docs(dir: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for ext in ["*.yaml", "*.yml"] {
        let mut matched = glob_concat(&format!("{dir}{ext}"))?;
        matched.sort();
        out.append(&mut matched);
    }
    Ok(out)
}

/// The subset of `glob` the script can reach: one path whose FINAL component may
/// hold `*` and `?`. `*` does not cross a `/` and neither matches a leading `.`,
/// as glob's own rules have it.
fn glob_concat(pattern: &str) -> Result<Vec<PathBuf>> {
    let (dir, name) = match pattern.rfind('/') {
        Some(i) => (&pattern[..=i], &pattern[i + 1..]),
        None => ("", pattern),
    };
    let dir_path = if dir.is_empty() { Path::new(".") } else { Path::new(dir) };
    let Ok(entries) = std::fs::read_dir(dir_path) else {
        return Ok(Vec::new()); // glob on a missing directory yields nothing
    };
    let mut out = Vec::new();
    for e in entries {
        let e = e?;
        let file = e.file_name().to_string_lossy().into_owned();
        if file.starts_with('.') && !name.starts_with('.') {
            continue;
        }
        if glob_match(name, &file) {
            out.push(PathBuf::from(format!("{dir}{file}")));
        }
    }
    Ok(out)
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    // Iterative backtracking match for `*` and `?`.
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ─────────────────────────────── the three checks ───────────────────────────

/// `test_jschema`: `is_valid`, and on failure the FIRST error is warned about.
fn test_jschema(schema: &Value, pattern: &Value) -> bool {
    let mut errors = Vec::new();
    match jsonschema::validate(schema, schema, pattern, String::new(), &mut errors) {
        Ok(()) => {}
        // A Draft-7 keyword owlmake does not implement cannot be reported as a
        // pass: the check would claim to have validated something it did not
        // look at.
        Err(e) => {
            eprintln!("UserWarning: simple-pattern-tester: {e:#}");
            return false;
        }
    }
    match errors.first() {
        None => true,
        Some(first) => {
            eprintln!("UserWarning: {first}");
            false
        }
    }
}

/// `test_vars`: every `vars` LIST anywhere below a top-level field must name only
/// variables the pattern declares.
///
/// The jsonpath is `*..vars` — a wildcard child FIRST, then a descendant search
/// — so the top-level `vars` MAP is not itself a match; what matches is each
/// axiom's `vars` list (`equivalentTo.vars`, `logical_axioms.[0].vars`, …).
fn test_vars(pattern: &Value) -> bool {
    let Some(map) = pattern.as_mapping() else { return true };
    let declared: BTreeSet<String> = match map.get(Value::from("vars")).and_then(|v| v.as_mapping())
    {
        Some(v) => {
            let mut set: BTreeSet<String> = v.keys().filter_map(as_str).collect();
            if let Some(d) = map.get(Value::from("data_vars")).and_then(|v| v.as_mapping()) {
                set.extend(d.keys().filter_map(as_str));
            }
            if let Some(s) = map.get(Value::from("substitutions")).and_then(|v| v.as_sequence()) {
                for sub in s {
                    if let Some(out) = sub.get("out").and_then(as_str_ref) {
                        set.insert(out.to_string());
                    }
                }
            }
            set
        }
        None => {
            eprintln!("UserWarning: Pattern has no vars");
            return true; // the script's own comment: not compulsory here
        }
    };

    let mut fields: Vec<(String, &Value)> = Vec::new();
    for (k, v) in map {
        let Some(k) = as_str(k) else { continue };
        descendant_vars(&k, v, &mut fields);
    }
    if fields.is_empty() {
        eprintln!("UserWarning: Pattern has no var fields");
        return true;
    }
    let mut ok = true;
    for (path, value) in fields {
        let used: BTreeSet<String> = match value.as_sequence() {
            Some(s) => s.iter().filter_map(as_str).collect(),
            None => continue,
        };
        let extra: Vec<&String> = used.difference(&declared).collect();
        if !extra.is_empty() {
            eprintln!(
                "UserWarning: {path} has values ({extra:?}) not found in pattern variable list ({declared:?}): "
            );
            ok = false;
        }
    }
    ok
}

/// `..vars` from `node`: the node itself and every descendant, each yielding its
/// own `vars` field if it has one.
fn descendant_vars<'a>(path: &str, node: &'a Value, out: &mut Vec<(String, &'a Value)>) {
    match node {
        Value::Mapping(m) => {
            if let Some(v) = m.get(Value::from("vars")) {
                out.push((format!("{path}.vars"), v));
            }
            for (k, v) in m {
                if let Some(k) = as_str(k) {
                    descendant_vars(&format!("{path}.{k}"), v, out);
                }
            }
        }
        Value::Sequence(s) => {
            for (i, v) in s.iter().enumerate() {
                descendant_vars(&format!("{path}.[{i}]"), v, out);
            }
        }
        _ => {}
    }
}

/// `test_text_fields`: `logical_axioms.[*].text`, then the `text` of each of
/// `equivalentTo`, `subClassOf`, `GCI` and `disjointWith`.
fn test_text_fields(pattern: &Value) -> Result<bool> {
    let Some(map) = pattern.as_mapping() else { return Ok(true) };
    let mut owl_entities: BTreeSet<String> = BTreeSet::new();
    for key in ["classes", "relations"] {
        if let Some(m) = map.get(Value::from(key)).and_then(|v| v.as_mapping()) {
            owl_entities.extend(m.keys().filter_map(as_str));
        }
    }

    let mut fields: Vec<(String, &Value)> = Vec::new();
    if let Some(axioms) = map.get(Value::from("logical_axioms")).and_then(|v| v.as_sequence()) {
        for (i, ax) in axioms.iter().enumerate() {
            if let Some(t) = ax.get("text") {
                fields.push((format!("logical_axioms.[{i}].text"), t));
            }
        }
    }
    for key in ["equivalentTo", "subClassOf", "GCI", "disjointWith"] {
        if let Some(t) = map.get(Value::from(key)).and_then(|v| v.get("text")) {
            fields.push((format!("{key}.text"), t));
        }
    }
    if fields.is_empty() {
        eprintln!("UserWarning: Pattern has no text fields");
        return Ok(true);
    }

    let mut ok = true;
    for (path, value) in fields {
        // `re.findall` over a non-string raises, and a TypeError is not a failed
        // check — it stops the script. Say so rather than passing the pattern.
        let Some(text) = value.as_str() else {
            bail!("simple-pattern-tester: {path} is not a string");
        };
        if text.matches('\'').count() % 2 == 1 {
            eprintln!("UserWarning: text field '{text}' has an odd number of single quotes.");
            ok = false;
        }
        let quoted = single_quoted(text);
        let extra: Vec<&String> = quoted.difference(&owl_entities).collect();
        if !extra.is_empty() {
            eprintln!(
                "UserWarning: {path} has values ({extra:?}) not found in owl entity dictionaries t ({owl_entities:?}): "
            );
            ok = false;
        }
    }
    Ok(ok)
}

/// `re.findall("'(.+?)'", val)` — non-greedy, non-overlapping, and `.` does not
/// match a newline.
fn single_quoted(text: &str) -> BTreeSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = BTreeSet::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '\'' {
            i += 1;
            continue;
        }
        // `.+?` is at least one non-newline character, then the nearest `'`.
        let mut j = i + 1;
        let mut seen = 0usize;
        while j < chars.len() && chars[j] != '\n' {
            if chars[j] == '\'' && seen > 0 {
                out.insert(chars[i + 1..j].iter().collect());
                break;
            }
            seen += 1;
            j += 1;
        }
        if j < chars.len() && chars[j] == '\'' && seen > 0 {
            i = j + 1; // findall does not re-scan inside a match
        } else {
            i += 1;
        }
    }
    out
}

fn as_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}
fn as_str_ref(v: &Value) -> Option<&str> {
    v.as_str()
}

// ───────────────────────────── JSON Schema (Draft 7) ────────────────────────

/// Enough of Draft 7 to validate a DOSDP pattern, written so that it cannot
/// quietly accept what it has not checked: a keyword in [`DRAFT7_KEYWORDS`] with
/// no arm below is an ERROR, while a key that is not a Draft-7 keyword at all is
/// ignored, which is what the specification says and what makes today's
/// `{404: 'Not Found'}` schema accept everything.
mod jsonschema {
    use anyhow::{bail, Result};
    use serde_yaml::Value;

    /// Every assertion or applicator keyword Draft 7 defines. `format`,
    /// `contentMediaType` and `contentEncoding` are ANNOTATIONS in Draft 7 —
    /// `Draft7Validator` does not assert them without a `FormatChecker`, and the
    /// script passes none — so they are listed as implemented-by-ignoring.
    const DRAFT7_KEYWORDS: &[&str] = &[
        "$schema", "$id", "$ref", "$comment", "title", "description", "default",
        "examples", "definitions", "readOnly", "writeOnly", "format",
        "contentMediaType", "contentEncoding",
        "type", "enum", "const",
        "multipleOf", "maximum", "exclusiveMaximum", "minimum", "exclusiveMinimum",
        "maxLength", "minLength", "pattern",
        "items", "additionalItems", "maxItems", "minItems", "uniqueItems", "contains",
        "maxProperties", "minProperties", "required", "properties",
        "patternProperties", "additionalProperties", "dependencies", "propertyNames",
        "if", "then", "else", "allOf", "anyOf", "oneOf", "not",
    ];

    pub fn validate(
        root: &Value,
        schema: &Value,
        instance: &Value,
        path: String,
        errors: &mut Vec<String>,
    ) -> Result<()> {
        // A boolean schema: `true` accepts everything, `false` nothing.
        if let Some(b) = schema.as_bool() {
            if !b {
                errors.push(format!("{path}: schema is false"));
            }
            return Ok(());
        }
        let Some(map) = schema.as_mapping() else { return Ok(()) };

        for (k, v) in map {
            // A non-string key (YAML allows them, and the 404 body has one) is
            // not a keyword.
            let Some(key) = k.as_str() else { continue };
            if !DRAFT7_KEYWORDS.contains(&key) {
                continue; // an unknown keyword imposes no constraint
            }
            match key {
                // Annotations, and the keywords consumed by their partners below.
                "$schema" | "$id" | "$comment" | "title" | "description" | "default"
                | "examples" | "definitions" | "readOnly" | "writeOnly" | "format"
                | "contentMediaType" | "contentEncoding" | "then" | "else"
                | "additionalItems" | "additionalProperties" => {}
                "$ref" => {
                    let Some(r) = v.as_str() else { bail!("$ref is not a string") };
                    let target = resolve_ref(root, r)?;
                    validate(root, &target, instance, path.clone(), errors)?;
                }
                "type" => {
                    let names: Vec<&str> = match v {
                        Value::String(s) => vec![s.as_str()],
                        Value::Sequence(s) => s.iter().filter_map(|x| x.as_str()).collect(),
                        _ => bail!("type is neither a string nor an array"),
                    };
                    if !names.iter().any(|t| is_type(instance, t)) {
                        errors.push(format!("{path}: {} is not of type {names:?}", brief(instance)));
                    }
                }
                "enum" => {
                    let Some(vals) = v.as_sequence() else { bail!("enum is not an array") };
                    if !vals.iter().any(|x| x == instance) {
                        errors.push(format!("{path}: {} is not one of the enum", brief(instance)));
                    }
                }
                "const" => {
                    if v != instance {
                        errors.push(format!("{path}: {} is not the const", brief(instance)));
                    }
                }
                "properties" => {
                    let Some(props) = v.as_mapping() else { bail!("properties is not an object") };
                    if let Some(obj) = instance.as_mapping() {
                        for (pk, ps) in props {
                            let Some(name) = pk.as_str() else { continue };
                            if let Some(child) = obj.get(Value::from(name)) {
                                validate(root, ps, child, join(&path, name), errors)?;
                            }
                        }
                    }
                }
                "patternProperties" => {
                    let Some(props) = v.as_mapping() else {
                        bail!("patternProperties is not an object")
                    };
                    if let Some(obj) = instance.as_mapping() {
                        for (pk, ps) in props {
                            let Some(re) = pk.as_str().map(compile).transpose()? else { continue };
                            for (ik, iv) in obj {
                                let Some(name) = ik.as_str() else { continue };
                                if re.is_match(name) {
                                    validate(root, ps, iv, join(&path, name), errors)?;
                                }
                            }
                        }
                    }
                }
                "required" => {
                    let Some(names) = v.as_sequence() else { bail!("required is not an array") };
                    if let Some(obj) = instance.as_mapping() {
                        for n in names.iter().filter_map(|x| x.as_str()) {
                            if obj.get(Value::from(n)).is_none() {
                                errors.push(format!("{path}: '{n}' is a required property"));
                            }
                        }
                    }
                }
                "propertyNames" => {
                    if let Some(obj) = instance.as_mapping() {
                        for k in obj.keys() {
                            validate(root, v, k, path.clone(), errors)?;
                        }
                    }
                }
                "minProperties" | "maxProperties" => {
                    if let Some(obj) = instance.as_mapping() {
                        let n = obj.len() as i64;
                        let want = int(v)?;
                        if (key == "minProperties" && n < want) || (key == "maxProperties" && n > want) {
                            errors.push(format!("{path}: {n} properties fails {key} {want}"));
                        }
                    }
                }
                "dependencies" => {
                    let Some(deps) = v.as_mapping() else { bail!("dependencies is not an object") };
                    if let Some(obj) = instance.as_mapping() {
                        for (dk, dv) in deps {
                            let Some(name) = dk.as_str() else { continue };
                            if obj.get(Value::from(name)).is_none() {
                                continue;
                            }
                            match dv {
                                // The array form: these properties must be present too.
                                Value::Sequence(names) => {
                                    for n in names.iter().filter_map(|x| x.as_str()) {
                                        if obj.get(Value::from(n)).is_none() {
                                            errors.push(format!(
                                                "{path}: '{n}' is a dependency of '{name}'"
                                            ));
                                        }
                                    }
                                }
                                other => validate(root, other, instance, path.clone(), errors)?,
                            }
                        }
                    }
                }
                "items" => {
                    if let Some(arr) = instance.as_sequence() {
                        match v {
                            // A single schema applies to every element…
                            Value::Sequence(schemas) => {
                                // …a tuple applies positionally, and
                                // `additionalItems` covers the tail.
                                for (i, item) in arr.iter().enumerate() {
                                    if let Some(s) = schemas.get(i) {
                                        validate(root, s, item, format!("{path}[{i}]"), errors)?;
                                    } else if let Some(extra) =
                                        map.get(Value::from("additionalItems"))
                                    {
                                        validate(root, extra, item, format!("{path}[{i}]"), errors)?;
                                    }
                                }
                            }
                            single => {
                                for (i, item) in arr.iter().enumerate() {
                                    validate(root, single, item, format!("{path}[{i}]"), errors)?;
                                }
                            }
                        }
                    }
                }
                "contains" => {
                    if let Some(arr) = instance.as_sequence() {
                        let hit = arr.iter().any(|item| {
                            let mut sub = Vec::new();
                            validate(root, v, item, path.clone(), &mut sub).is_ok() && sub.is_empty()
                        });
                        if !hit {
                            errors.push(format!("{path}: no item matches 'contains'"));
                        }
                    }
                }
                "minItems" | "maxItems" => {
                    if let Some(arr) = instance.as_sequence() {
                        let n = arr.len() as i64;
                        let want = int(v)?;
                        if (key == "minItems" && n < want) || (key == "maxItems" && n > want) {
                            errors.push(format!("{path}: {n} items fails {key} {want}"));
                        }
                    }
                }
                "uniqueItems" => {
                    if v.as_bool() == Some(true) {
                        if let Some(arr) = instance.as_sequence() {
                            for i in 0..arr.len() {
                                if arr[i + 1..].contains(&arr[i]) {
                                    errors.push(format!("{path}: has non-unique elements"));
                                    break;
                                }
                            }
                        }
                    }
                }
                "minLength" | "maxLength" => {
                    if let Some(s) = instance.as_str() {
                        let n = s.chars().count() as i64;
                        let want = int(v)?;
                        if (key == "minLength" && n < want) || (key == "maxLength" && n > want) {
                            errors.push(format!("{path}: {s:?} fails {key} {want}"));
                        }
                    }
                }
                "pattern" => {
                    if let Some(s) = instance.as_str() {
                        let Some(p) = v.as_str() else { bail!("pattern is not a string") };
                        if !compile(p)?.is_match(s) {
                            errors.push(format!("{path}: {s:?} does not match {p:?}"));
                        }
                    }
                }
                "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                    if let Some(n) = num(instance) {
                        let want = num(v).ok_or_else(|| anyhow::anyhow!("{key} is not a number"))?;
                        let bad = match key {
                            "minimum" => n < want,
                            "maximum" => n > want,
                            "exclusiveMinimum" => n <= want,
                            _ => n >= want,
                        };
                        if bad {
                            errors.push(format!("{path}: {n} fails {key} {want}"));
                        }
                    }
                }
                "multipleOf" => {
                    if let Some(n) = num(instance) {
                        let want = num(v).ok_or_else(|| anyhow::anyhow!("multipleOf is not a number"))?;
                        if want == 0.0 || (n / want).fract().abs() > 1e-9 {
                            errors.push(format!("{path}: {n} is not a multiple of {want}"));
                        }
                    }
                }
                "allOf" => {
                    let Some(schemas) = v.as_sequence() else { bail!("allOf is not an array") };
                    for s in schemas {
                        validate(root, s, instance, path.clone(), errors)?;
                    }
                }
                "anyOf" | "oneOf" => {
                    let Some(schemas) = v.as_sequence() else { bail!("{key} is not an array") };
                    let mut hits = 0usize;
                    for s in schemas {
                        let mut sub = Vec::new();
                        validate(root, s, instance, path.clone(), &mut sub)?;
                        if sub.is_empty() {
                            hits += 1;
                        }
                    }
                    let bad = if key == "anyOf" { hits == 0 } else { hits != 1 };
                    if bad {
                        errors.push(format!("{path}: {} is not valid under any of the given schemas", brief(instance)));
                    }
                }
                "not" => {
                    let mut sub = Vec::new();
                    validate(root, v, instance, path.clone(), &mut sub)?;
                    if sub.is_empty() {
                        errors.push(format!("{path}: {} should not be valid", brief(instance)));
                    }
                }
                "if" => {
                    let mut sub = Vec::new();
                    validate(root, v, instance, path.clone(), &mut sub)?;
                    let branch = if sub.is_empty() { "then" } else { "else" };
                    if let Some(b) = map.get(Value::from(branch)) {
                        validate(root, b, instance, path.clone(), errors)?;
                    }
                }
                other => bail!(
                    "JSON Schema keyword `{other}` is in Draft 7 but owlmake does not implement \
                     it, so this pattern has not been validated. Reporting a pass would claim a \
                     check that did not run."
                ),
            }
        }
        // `additionalProperties` needs the sibling `properties`/`patternProperties`
        // to know which names are already covered, so it runs after the loop.
        if let Some(extra) = map.get(Value::from("additionalProperties")) {
            if let Some(obj) = instance.as_mapping() {
                let named: Vec<&str> = map
                    .get(Value::from("properties"))
                    .and_then(|p| p.as_mapping())
                    .map(|m| m.keys().filter_map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                let patterns: Vec<&str> = map
                    .get(Value::from("patternProperties"))
                    .and_then(|p| p.as_mapping())
                    .map(|m| m.keys().filter_map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                for (ik, iv) in obj {
                    let Some(name) = ik.as_str() else { continue };
                    if named.contains(&name) {
                        continue;
                    }
                    let mut covered = false;
                    for p in &patterns {
                        if compile(p)?.is_match(name) {
                            covered = true;
                            break;
                        }
                    }
                    if covered {
                        continue;
                    }
                    if extra.as_bool() == Some(false) {
                        errors.push(format!(
                            "{path}: additional property '{name}' is not allowed"
                        ));
                    } else {
                        validate(root, extra, iv, join(&path, name), errors)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Only a local pointer — a remote `$ref` would need another fetch, and
    /// pretending it validated would be the silent pass this module exists to
    /// avoid.
    fn resolve_ref(root: &Value, r: &str) -> Result<Value> {
        let Some(frag) = r.strip_prefix("#") else {
            bail!("remote $ref `{r}` is not supported, so the pattern has not been validated")
        };
        let mut cur = root.clone();
        for seg in frag.trim_start_matches('/').split('/').filter(|s| !s.is_empty()) {
            let seg = seg.replace("~1", "/").replace("~0", "~");
            let next = match &cur {
                Value::Mapping(m) => m.get(Value::from(seg.as_str())).cloned(),
                Value::Sequence(s) => seg.parse::<usize>().ok().and_then(|i| s.get(i).cloned()),
                _ => None,
            };
            cur = next.ok_or_else(|| anyhow::anyhow!("$ref `{r}` does not resolve"))?;
        }
        Ok(cur)
    }

    fn is_type(v: &Value, t: &str) -> bool {
        match t {
            "object" => v.is_mapping(),
            "array" => v.is_sequence(),
            "string" => v.is_string(),
            "boolean" => v.is_bool(),
            "null" => v.is_null(),
            // JSON Schema's `integer` accepts a number with a zero fraction.
            "integer" => v.as_i64().is_some() || v.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false),
            "number" => v.is_number(),
            _ => false,
        }
    }

    fn num(v: &Value) -> Option<f64> {
        v.as_f64()
    }

    fn int(v: &Value) -> Result<i64> {
        v.as_i64().ok_or_else(|| anyhow::anyhow!("expected an integer"))
    }

    fn join(path: &str, name: &str) -> String {
        if path.is_empty() {
            name.to_string()
        } else {
            format!("{path}.{name}")
        }
    }

    fn brief(v: &Value) -> String {
        let s = serde_yaml::to_string(v).unwrap_or_default();
        let s = s.trim().replace('\n', " ");
        if s.chars().count() > 60 {
            format!("{}…", s.chars().take(60).collect::<String>())
        } else {
            s
        }
    }

    /// A schema `pattern` is an ECMA-262 regex; the ones a DOSDP schema uses are
    /// ordinary. A pattern the engine cannot compile is an error, not a pass.
    fn compile(p: &str) -> Result<regex::Regex> {
        regex::Regex::new(p)
            .map_err(|e| anyhow::anyhow!("schema pattern {p:?} does not compile: {e}"))
    }
}

/// `simple_pattern_tester.py <DIR>` — the shim's entry point.
pub fn main(args: &[String]) -> i32 {
    let mut dir: Option<String> = None;
    for tok in args {
        match tok.as_str() {
            "--help" | "-h" => {
                println!(
                    "simple_pattern_tester.py (owlmake {}) — validate DOSDP pattern files\n\n\
                     Usage: simple_pattern_tester.py <DIR>",
                    env!("CARGO_PKG_VERSION")
                );
                return 0;
            }
            "--version" | "-V" => {
                println!("owlmake {} (native simple_pattern_tester.py)", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            t => dir = Some(t.to_string()),
        }
    }
    let Some(dir) = dir else {
        eprintln!("simple_pattern_tester.py: no pattern directory given");
        return 1;
    };
    match check_dir(&dir) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("simple_pattern_tester.py: {e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Value {
        serde_yaml::from_str(s).unwrap()
    }

    /// The schema URL 404s and the body parses as `{404: 'Not Found'}`, which
    /// `Draft7Validator` accepts everything against — measured in the ODK image.
    #[test]
    fn a_schema_with_no_keywords_constrains_nothing() {
        let schema = yaml("404: Not Found");
        assert!(test_jschema(&schema, &yaml("{a: 1}")));
        assert!(test_jschema(&schema, &yaml("[1, 2, 3]")));
    }

    #[test]
    fn a_real_schema_is_enforced() {
        let schema = yaml(
            "type: object\nrequired: [pattern_name]\nproperties:\n  pattern_name: {type: string}\n",
        );
        assert!(test_jschema(&schema, &yaml("pattern_name: acute")));
        assert!(!test_jschema(&schema, &yaml("vars: {}")));
        assert!(!test_jschema(&schema, &yaml("pattern_name: 7")));
    }

    /// `*..vars` skips the top-level `vars` map and finds each axiom's list.
    #[test]
    fn an_axiom_may_only_use_declared_vars() {
        let ok = yaml(
            "vars: {disease: \"'disease'\"}\nequivalentTo:\n  text: \"'x' and 'y' some %s\"\n  vars: [disease]\n",
        );
        assert!(test_vars(&ok));
        let bad = yaml(
            "vars: {disease: \"'disease'\"}\nlogical_axioms:\n  - text: \"%s\"\n    vars: [nosuch]\n",
        );
        assert!(!test_vars(&bad));
    }

    #[test]
    fn a_text_field_quotes_only_declared_entities() {
        let doc = yaml(
            "classes:\n  disease: 'MONDO:0000001'\nrelations:\n  part of: 'BFO:0000050'\nequivalentTo:\n  text: \"'disease' and 'part of' some %s\"\n",
        );
        assert!(test_text_fields(&doc).unwrap());
        let odd = yaml("classes:\n  disease: x\nequivalentTo:\n  text: \"'disease' and 'oops\"\n");
        assert!(!test_text_fields(&odd).unwrap());
        let undeclared = yaml("classes:\n  disease: x\nequivalentTo:\n  text: \"'nope'\"\n");
        assert!(!test_text_fields(&undeclared).unwrap());
    }

    #[test]
    fn findall_is_non_greedy_and_non_overlapping() {
        let got = single_quoted("'a' and 'b c' some 'd'");
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            vec!["a".to_string(), "b c".to_string(), "d".to_string()]
        );
    }
}
