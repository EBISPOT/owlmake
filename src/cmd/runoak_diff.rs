//! `runoak diff` — the KGCL change report between two OBO documents.
//!
//! `runoak -i simpleobo:OLD diff -X simpleobo:NEW` compares the two documents
//! term by term and writes the transition from OLD to NEW as KGCL change
//! objects: created and deleted classes, renames, definition changes, synonym
//! and mapping and subset membership changes, and edge (is_a / relationship)
//! changes. Three output shapes:
//!
//! - `--output-type yaml`: one YAML document per change, `---`-separated, each
//!   with a content-derived `uuid:` id — the same two documents always produce
//!   the same bytes.
//! - `--output-type csv` with `--statistics`: a tab-separated count-per-type
//!   table, one row per `--group-by-property` value (`__RESIDUAL__` collects
//!   terms without one), plus the `All_Obsoletion` / `All_Synonym` aggregates.
//! - `--output-type md`: a human summary, one collapsible section per change
//!   kind with a table of the affected terms.
//!
//! Ordering is deterministic everywhere: changes sort by kind, then subject,
//! then the changed values.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

/// Everything the diff reads from one term stanza.
#[derive(Default)]
struct Term {
    typedef: bool,
    name: Option<String>,
    /// The first `def` tag's quoted text — what a definition change reports.
    def: Option<String>,
    /// Every raw `def` line — the equality the change detection runs on, so a
    /// provenance-only edit still registers as a definition change.
    def_raw: BTreeSet<String>,
    obsolete: bool,
    replaced_by: Option<String>,
    /// (predicate CURIE, synonym text)
    synonyms: BTreeSet<(String, String)>,
    /// (predicate CURIE, object) — xrefs carry `oio:hasDbXref`.
    mappings: BTreeSet<(String, String)>,
    subsets: BTreeSet<String>,
    /// (predicate CURIE, object id) — `is_a` is `rdfs:subClassOf`.
    edges: BTreeSet<(String, String)>,
    /// Values of the `--group-by-property` annotation, when asked for.
    group: Option<String>,
}

struct Doc {
    terms: BTreeMap<String, Term>,
    /// Typedef id → CURIE it stands for (its `xref:` when one is given).
    typedef_curie: BTreeMap<String, String>,
    /// Typedef CURIE → label, for rendering predicates in the md tables.
    typedef_label: BTreeMap<String, String>,
}

fn unquote(v: &str) -> String {
    // `"text" rest` → text, with backslash escapes resolved.
    let v = v.trim();
    let Some(rest) = v.strip_prefix('"') else { return v.to_string() };
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(match n {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
            }
            '"' => break,
            other => out.push(other),
        }
    }
    out
}

fn strip_comment(v: &str) -> &str {
    // A trailing ` ! label` comment is display text, and a trailing
    // `{qualifier=...}` block is axiom annotation — neither is the value.
    let v = match v.find(" !") {
        Some(i) => v[..i].trim(),
        None => v.trim(),
    };
    match (v.rfind('{'), v.ends_with('}')) {
        (Some(i), true) => v[..i].trim(),
        _ => v,
    }
}

fn scope_predicate(scope: &str) -> &'static str {
    match scope {
        "EXACT" => "oio:hasExactSynonym",
        "BROAD" => "oio:hasBroadSynonym",
        "NARROW" => "oio:hasNarrowSynonym",
        _ => "oio:hasRelatedSynonym",
    }
}

fn parse(path: &str, group_by: Option<&str>) -> Result<Doc> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {path}: {e}"))?;
    let mut terms: BTreeMap<String, Term> = BTreeMap::new();
    let mut typedef_curie = BTreeMap::new();
    let mut typedef_label = BTreeMap::new();

    #[derive(PartialEq)]
    enum Kind {
        None,
        Term,
        Typedef,
    }
    let mut kind = Kind::None;
    let mut id = String::new();
    let mut cur = Term::default();
    let mut td_xref: Option<String> = None;
    let mut td_name: Option<String> = None;

    let mut flush = |kind: &Kind,
                     id: &mut String,
                     cur: &mut Term,
                     td_xref: &mut Option<String>,
                     td_name: &mut Option<String>,
                     terms: &mut BTreeMap<String, Term>,
                     typedef_curie: &mut BTreeMap<String, String>,
                     typedef_label: &mut BTreeMap<String, String>| {
        if id.is_empty() {
            return;
        }
        match kind {
            Kind::Term => {
                terms.insert(std::mem::take(id), std::mem::take(cur));
            }
            Kind::Typedef => {
                let curie = td_xref.take().unwrap_or_else(|| id.clone());
                if let Some(n) = td_name.clone() {
                    typedef_label.insert(curie.clone(), n);
                }
                cur.typedef = true;
                cur.name = td_name.take();
                typedef_curie.insert(id.clone(), curie);
                terms.insert(std::mem::take(id), std::mem::take(cur));
            }
            Kind::None => {}
        }
    };

    for line in text.lines() {
        let line = line.trim_end();
        if line == "[Term]" || line == "[Typedef]" || line.starts_with('[') {
            flush(&kind, &mut id, &mut cur, &mut td_xref, &mut td_name, &mut terms, &mut typedef_curie, &mut typedef_label);
            kind = match line {
                "[Term]" => Kind::Term,
                "[Typedef]" => Kind::Typedef,
                _ => Kind::None,
            };
            continue;
        }
        if kind == Kind::None {
            continue;
        }
        let Some((tag, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match tag {
            "id" => id = value.to_string(),
            "name" => {
                if kind == Kind::Typedef {
                    td_name = Some(value.to_string());
                } else {
                    cur.name = Some(value.to_string());
                }
            }
            "xref" if kind == Kind::Typedef => td_xref = Some(strip_comment(value).to_string()),
            "def" => {
                if cur.def.is_none() {
                    cur.def = Some(unquote(value));
                }
                cur.def_raw.insert(value.to_string());
            }
            "is_obsolete" => cur.obsolete = value == "true",
            "replaced_by" => cur.replaced_by = Some(strip_comment(value).to_string()),
            "synonym" => {
                let text = unquote(value);
                // The scope is the first token after the quote that closes the
                // synonym text — found by walking the escapes, because later
                // qualifier values carry quotes of their own.
                let mut close = None;
                let bytes = value.as_bytes();
                if bytes.first() == Some(&b'"') {
                    let mut i = 1;
                    while i < bytes.len() {
                        match bytes[i] {
                            b'\\' => i += 1,
                            b'"' => {
                                close = Some(i);
                                break;
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                }
                let after = close.map(|i| value[i + 1..].trim()).unwrap_or("");
                let scope = after.split_whitespace().next().unwrap_or("RELATED");
                cur.synonyms.insert((scope_predicate(scope).to_string(), text));
            }
            "xref" => {
                let mut v = strip_comment(value);
                // `X "label"` keeps only the identifier.
                if let Some(i) = v.find(" \"") {
                    v = v[..i].trim();
                }
                cur.mappings.insert(("oio:hasDbXref".to_string(), v.to_string()));
            }
            "subset" => {
                cur.subsets.insert(strip_comment(value).to_string());
            }
            "is_a" => {
                cur.edges.insert(("rdfs:subClassOf".to_string(), strip_comment(value).to_string()));
            }
            "relationship" => {
                let v = strip_comment(value);
                if let Some((p, o)) = v.split_once(' ') {
                    cur.edges.insert((p.trim().to_string(), o.trim().to_string()));
                }
            }
            "property_value" => {
                if let Some(gp) = group_by {
                    let v = strip_comment(value);
                    if let Some(rest) = v.strip_prefix(gp) {
                        cur.group = Some(unquote(rest.trim()));
                    }
                }
            }
            _ => {}
        }
    }
    flush(&kind, &mut id, &mut cur, &mut td_xref, &mut td_name, &mut terms, &mut typedef_curie, &mut typedef_label);
    Ok(Doc { terms, typedef_curie, typedef_label })
}

/// One change, as (kind, ordered field list). The field list is exactly what
/// the YAML document prints after `id:` and `type:`.
struct Change {
    kind: &'static str,
    fields: Vec<(&'static str, String)>,
    /// The term whose `--group-by-property` value buckets this change.
    group_term: String,
}

impl Change {
    fn new(kind: &'static str, group_term: &str, fields: Vec<(&'static str, String)>) -> Self {
        Change { kind, fields, group_term: group_term.to_string() }
    }
    fn sort_key(&self) -> (String, String) {
        (self.kind.to_string(), self.fields.iter().map(|(k, v)| format!("{k}={v};")).collect())
    }
}

fn resolve_pred<'a>(doc: &'a Doc, p: &'a str) -> &'a str {
    doc.typedef_curie.get(p).map(|s| s.as_str()).unwrap_or(p)
}

fn diff_docs(old: &Doc, new: &Doc) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    let empty = Term::default();
    let mut all_ids: BTreeSet<&String> = old.terms.keys().collect();
    all_ids.extend(new.terms.keys());

    for id in all_ids {
        let a0 = old.terms.get(id.as_str());
        let b0 = new.terms.get(id.as_str());
        let created = a0.is_none();
        let deleted = b0.is_none();
        let a = a0.unwrap_or(&empty);
        let b = b0.unwrap_or(&empty);
        if created {
            let kind = if b.typedef { "NodeCreation" } else { "ClassCreation" };
            let mut fields = vec![("about_node", id.clone())];
            if let Some(n) = &b.name {
                fields.push(("name", n.clone()));
            }
            out.push(Change::new(kind, id, fields));
        }
        if deleted {
            out.push(Change::new("NodeDeletion", id, vec![("about_node", id.clone())]));
        }

        // Name: a rename only when the node exists on both sides.
        if !created && !deleted && a.name != b.name {
            if a.name.is_some() && b.name.is_some() {
                out.push(Change::new(
                    "NodeRename",
                    id,
                    vec![
                        ("old_value", a.name.clone().unwrap_or_default()),
                        ("new_value", b.name.clone().unwrap_or_default()),
                        ("about_node", id.clone()),
                    ],
                ));
            }
        }

        // Definition: a deleted node keeps only its deletion. Change detection
        // runs on the raw lines (provenance included); the reported values are
        // the quoted texts.
        if !deleted && a.def_raw != b.def_raw {
            match (&a.def, &b.def) {
                (Some(o), Some(n)) => out.push(Change::new(
                    "NodeTextDefinitionChange",
                    id,
                    vec![("old_value", o.clone()), ("new_value", n.clone()), ("about_node", id.clone())],
                )),
                (None, Some(n)) => out.push(Change::new(
                    "NewTextDefinition",
                    id,
                    vec![("new_value", n.clone()), ("about_node", id.clone())],
                )),
                (Some(o), None) if !created => out.push(Change::new(
                    "RemoveTextDefinition",
                    id,
                    vec![("old_value", o.clone()), ("about_node", id.clone())],
                )),
                _ => {}
            }
        }

        // Obsoletion state.
        if !deleted && a.obsolete != b.obsolete {
            if a.obsolete {
                out.push(Change::new("NodeUnobsoletion", id, vec![("about_node", id.clone())]));
            } else if let Some(rb) = &b.replaced_by {
                out.push(Change::new(
                    "NodeObsoletionWithDirectReplacement",
                    id,
                    vec![("about_node", id.clone()), ("has_direct_replacement", rb.clone())],
                ));
            } else {
                out.push(Change::new("NodeObsoletion", id, vec![("about_node", id.clone())]));
            }
        }

        if !deleted {
            for sub in b.subsets.difference(&a.subsets) {
                out.push(Change::new(
                    "AddNodeToSubset",
                    id,
                    vec![("about_node", id.clone()), ("in_subset", sub.clone())],
                ));
            }
            for sub in a.subsets.difference(&b.subsets) {
                out.push(Change::new(
                    "RemoveNodeFromSubset",
                    id,
                    vec![("about_node", id.clone()), ("in_subset", sub.clone())],
                ));
            }
            for (pred, text) in a.synonyms.difference(&b.synonyms) {
                out.push(Change::new(
                    "RemoveSynonym",
                    id,
                    vec![("old_value", text.clone()), ("about_node", id.clone())],
                ));
                let _ = pred;
            }
            for (pred, text) in b.synonyms.difference(&a.synonyms) {
                out.push(Change::new(
                    "NewSynonym",
                    id,
                    vec![("new_value", text.clone()), ("about_node", id.clone()), ("predicate", pred.clone())],
                ));
            }
            for (pred, obj) in a.mappings.difference(&b.mappings) {
                out.push(Change::new(
                    "RemoveMapping",
                    id,
                    vec![("about_node", id.clone()), ("object", obj.clone()), ("predicate", pred.clone())],
                ));
            }
            for (pred, obj) in b.mappings.difference(&a.mappings) {
                out.push(Change::new(
                    "MappingCreation",
                    id,
                    vec![("subject", id.clone()), ("predicate", pred.clone()), ("object", obj.clone())],
                ));
            }
        }

        // Edges diff on both sides — a deleted node's edges are deletions too.
        for (pred, obj) in a.edges.difference(&b.edges) {
            out.push(Change::new(
                "EdgeDeletion",
                id,
                vec![
                    ("subject", id.clone()),
                    ("predicate", resolve_pred(old, pred).to_string()),
                    ("object", obj.clone()),
                ],
            ));
        }
        for (pred, obj) in b.edges.difference(&a.edges) {
            out.push(Change::new(
                "EdgeCreation",
                id,
                vec![
                    ("subject", id.clone()),
                    ("predicate", resolve_pred(new, pred).to_string()),
                    ("object", obj.clone()),
                ],
            ));
        }
    }
    out.sort_by_key(|c| c.sort_key());
    out
}

/// A stable `uuid:` id derived from the change's content, shaped like a
/// version-4 UUID so the id namespace is uniform.
fn content_uuid(c: &Change) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(c.kind.as_bytes());
    for (k, v) in &c.fields {
        h.update([1u8]);
        h.update(k.as_bytes());
        h.update([2u8]);
        h.update(v.as_bytes());
    }
    let d = h.finalize();
    let hex: Vec<String> = d.iter().map(|b| format!("{b:02x}")).collect();
    let s: String = hex.concat();
    format!(
        "uuid:{}-{}-4{}-8{}-{}",
        &s[0..8],
        &s[8..12],
        &s[13..16],
        &s[17..20],
        &s[20..32]
    )
}

/// YAML block-scalar quoting for a single-line value: plain when safe, quoted
/// when the text starts with a marker or contains a character YAML would
/// re-parse.
fn yaml_value(v: &str) -> String {
    let needs = v.is_empty()
        || v.contains(": ")
        || v.ends_with(':')
        || v.contains('#')
        || v.contains('\n')
        || v.starts_with(['"', '\'', '-', '?', '[', ']', '{', '}', '&', '*', '!', '|', '>', '%', '@', '`', ' '])
        || v.ends_with(' ');
    if !needs {
        return v.to_string();
    }
    let esc = v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    format!("\"{esc}\"")
}

fn render_yaml(changes: &[Change]) -> String {
    let mut out = String::new();
    for (i, c) in changes.iter().enumerate() {
        if i > 0 {
            out.push_str("---\n");
        }
        out.push_str(&format!("id: {}\n", content_uuid(c)));
        out.push_str(&format!("type: {}\n", c.kind));
        for (k, v) in &c.fields {
            if k == &"name" {
                continue; // ClassCreation prints the node only.
            }
            out.push_str(&format!("{k}: {}\n", yaml_value(v)));
        }
        out.push('\n');
    }
    out
}

fn render_stats(changes: &[Change], old: &Doc, group_by: Option<&str>) -> String {
    // Count per (group value, kind). Terms without a group value fall into
    // `__RESIDUAL__`; when no group property was named everything does.
    let mut kinds: BTreeSet<&'static str> = BTreeSet::new();
    let mut rows: BTreeMap<String, BTreeMap<&'static str, u64>> = BTreeMap::new();
    for c in changes {
        kinds.insert(c.kind);
        let group = group_by
            .and_then(|_| old.terms.get(&c.group_term).and_then(|t| t.group.clone()))
            .unwrap_or_else(|| "__RESIDUAL__".to_string());
        *rows.entry(group).or_default().entry(c.kind).or_insert(0) += 1;
    }
    let kind_cols: Vec<&'static str> = kinds.into_iter().collect();
    let mut out = String::from("group");
    for k in &kind_cols {
        out.push('\t');
        out.push_str(k);
    }
    out.push_str("\tAll_Obsoletion\tAll_Synonym\n");
    for (group, counts) in rows {
        out.push_str(&group);
        let mut obsoletion = 0u64;
        let mut synonym = 0u64;
        for k in &kind_cols {
            let n = counts.get(k).copied().unwrap_or(0);
            if k.contains("Obsoletion") || k.contains("Unobsoletion") {
                obsoletion += n;
            }
            if k.contains("Synonym") {
                synonym += n;
            }
            out.push('\t');
            out.push_str(&n.to_string());
        }
        out.push_str(&format!("\t{obsoletion}\t{synonym}\n"));
    }
    out
}

fn label_for(id: &str, old: &Doc, new: &Doc) -> String {
    let name = new
        .terms
        .get(id)
        .and_then(|t| t.name.clone())
        .or_else(|| old.terms.get(id).and_then(|t| t.name.clone()));
    match name {
        Some(n) => format!("{n} ({id})"),
        None => id.to_string(),
    }
}

fn pred_label(p: &str, old: &Doc, new: &Doc) -> String {
    if p == "rdfs:subClassOf" {
        return p.to_string();
    }
    let name = new.typedef_label.get(p).or_else(|| old.typedef_label.get(p));
    match name {
        Some(n) => format!("{n} ({p})"),
        None => p.to_string(),
    }
}

fn render_md(changes: &[Change], old: &Doc, new: &Doc) -> String {
    // (kind, section title, table headers)
    const SECTIONS: &[(&str, &str, &[&str])] = &[
        ("ClassCreation", "Classes added", &["Term"]),
        ("NodeDeletion", "Classes removed", &["Term"]),
        ("NodeRename", "Nodes renamed", &["ID", "Old Label", "New Label"]),
        ("NewTextDefinition", "Text definitions added", &["Term", "New Text Definition"]),
        ("RemoveTextDefinition", "Text definitions removed", &["Term", "Old Text Definition"]),
        (
            "NodeTextDefinitionChange",
            "Text definitions changed",
            &["Term", "Old Text Definition", "New Text Definition"],
        ),
        ("NewSynonym", "Synonyms added", &["Term", "New Synonym", "Predicate"]),
        ("RemoveSynonym", "Synonyms removed", &["Term", "Removed Synonym"]),
        ("MappingCreation", "Mappings added", &["Subject", "Predicate", "Object"]),
        ("RemoveMapping", "Mappings removed", &["Subject", "Predicate", "Object"]),
        ("AddNodeToSubset", "Nodes added to subset", &["Term", "Subset"]),
        ("RemoveNodeFromSubset", "Nodes removed from subset", &["Term", "Subset"]),
        ("EdgeCreation", "Relationships added", &["Subject", "Predicate", "Object"]),
        ("EdgeDeletion", "Relationships removed", &["Subject", "Predicate", "Object"]),
        ("NodeObsoletion", "Nodes obsoleted", &["Term"]),
        ("NodeUnobsoletion", "Nodes unobsoleted", &["Term"]),
    ];
    let field = |c: &Change, k: &str| -> String {
        c.fields.iter().find(|(f, _)| *f == k).map(|(_, v)| v.clone()).unwrap_or_default()
    };
    let mut out = String::new();
    for (kind, title, headers) in SECTIONS {
        let rows: Vec<&Change> = changes.iter().filter(|c| c.kind == *kind).collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("<details>\n<summary>{title}: {}</summary>\n\n", rows.len()));
        out.push_str(&format!("| {} |\n", headers.join(" | ")));
        out.push_str(&format!("{}|\n", "----|".repeat(headers.len())));
        for c in rows {
            let cells: Vec<String> = match *kind {
                "ClassCreation" | "NodeDeletion" | "NodeObsoletion" | "NodeUnobsoletion" => {
                    vec![label_for(&field(c, "about_node"), old, new)]
                }
                "NodeRename" => vec![
                    field(c, "about_node"),
                    field(c, "old_value"),
                    field(c, "new_value"),
                ],
                "NewTextDefinition" => {
                    vec![label_for(&field(c, "about_node"), old, new), field(c, "new_value")]
                }
                "RemoveTextDefinition" => {
                    vec![label_for(&field(c, "about_node"), old, new), field(c, "old_value")]
                }
                "NodeTextDefinitionChange" => vec![
                    label_for(&field(c, "about_node"), old, new),
                    field(c, "old_value"),
                    field(c, "new_value"),
                ],
                "NewSynonym" => vec![
                    label_for(&field(c, "about_node"), old, new),
                    field(c, "new_value"),
                    field(c, "predicate"),
                ],
                "RemoveSynonym" => {
                    vec![label_for(&field(c, "about_node"), old, new), field(c, "old_value")]
                }
                "MappingCreation" => vec![
                    label_for(&field(c, "subject"), old, new),
                    field(c, "predicate"),
                    field(c, "object"),
                ],
                "RemoveMapping" => vec![
                    label_for(&field(c, "about_node"), old, new),
                    field(c, "predicate"),
                    field(c, "object"),
                ],
                "AddNodeToSubset" | "RemoveNodeFromSubset" => {
                    vec![label_for(&field(c, "about_node"), old, new), field(c, "in_subset")]
                }
                "EdgeCreation" | "EdgeDeletion" => vec![
                    label_for(&field(c, "subject"), old, new),
                    pred_label(&field(c, "predicate"), old, new),
                    label_for(&field(c, "object"), old, new),
                ],
                _ => vec![],
            };
            let cells: Vec<String> =
                cells.into_iter().map(|s| s.replace('|', "\\|").replace('\n', " ")).collect();
            out.push_str(&format!("| {} |\n", cells.join(" | ")));
        }
        out.push_str("\n</details>\n\n");
    }
    out
}

pub struct DiffArgs {
    pub old_path: String,
    pub new_path: String,
    pub output: Option<String>,
    pub output_type: String,
    pub statistics: bool,
    pub group_by: Option<String>,
}

pub fn run(args: &DiffArgs) -> Result<()> {
    let old = parse(&args.old_path, args.group_by.as_deref())?;
    let new = parse(&args.new_path, args.group_by.as_deref())?;
    let changes = diff_docs(&old, &new);
    let rendered = match args.output_type.as_str() {
        "yaml" => render_yaml(&changes),
        "csv" if args.statistics => render_stats(&changes, &old, args.group_by.as_deref()),
        "csv" => render_stats(&changes, &old, None),
        "md" => render_md(&changes, &old, &new),
        other => anyhow::bail!("unsupported --output-type `{other}` (yaml, csv, md)"),
    };
    match &args.output {
        Some(p) => std::fs::write(p, rendered)?,
        None => print!("{rendered}"),
    }
    Ok(())
}
