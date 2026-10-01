//! `update_repo` — bring a repository's files into step with its options.
//!
//! A repository states its imports, components, patterns and checks as options.
//! Files follow from them: the edit file declares an import for each module and
//! component, the XML catalog says where each of those IRIs is on disk, and the
//! build reads a module, a term file, a component or a template that has to exist
//! before the first build can replace it. ODK's `update_repo` target writes those,
//! and this is that target for a repository built from its `owlmake.yaml`:
//!
//! - the **XML catalog**: the `odk-managed-catalog` group is rewritten from the
//!   options and everything else in the catalog is kept — though not its
//!   comments or its layout, because the file is written back whole, as ODK
//!   writes it;
//! - the **edit file's import declarations** become exactly the declared imports,
//!   components and pattern products (`odk:import --exclusive true`), and the
//!   file is written back in its edit format;
//! - the **files the standard build reads** are created where they are missing —
//!   import modules and term files, components, templates, mapping and
//!   translation tables, the pattern products, `context.json`,
//!   `keeprelations.txt`, the ID ranges — with the placeholder ODK gives each;
//! - the **SPARQL queries** of the standard build are written over whatever is
//!   there, as ODK does: they belong to the standard build, not the repository.
//!
//! `manage_import_declarations: false` leaves the catalog and the edit file alone.
//!
//! What ODK's target also writes and this does not is what is about ODK itself:
//! the generated Makefile and its `run.sh` wrapper, the CI workflows that run the
//! ODK container, the documentation of ODK's workflows, and the `.gitignore`
//! block ODK rewrites. None of it is read by a build.

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::builtin::Config;
use super::OdkRepo;

const MANAGED_GROUP: &str = "odk-managed-catalog";

/// The standard build's SPARQL queries that are the same for every repository.
const QUERIES: &[(&str, &str)] = &[
    ("basic-report.sparql", include_str!("update/sparql/basic-report.sparql")),
    ("class-count-by-prefix.sparql", include_str!("update/sparql/class-count-by-prefix.sparql")),
    ("edges.sparql", include_str!("update/sparql/edges.sparql")),
    ("obsoletes.sparql", include_str!("update/sparql/obsoletes.sparql")),
    ("postprocess-module.ru", include_str!("update/sparql/postprocess-module.ru")),
    ("preprocess-module.ru", include_str!("update/sparql/preprocess-module.ru")),
    ("simple-seed.sparql", include_str!("update/sparql/simple-seed.sparql")),
    ("subsets-labeled.sparql", include_str!("update/sparql/subsets-labeled.sparql")),
    ("synonyms.sparql", include_str!("update/sparql/synonyms.sparql")),
    ("terms.sparql", include_str!("update/sparql/terms.sparql")),
    ("xrefs.sparql", include_str!("update/sparql/xrefs.sparql")),
];

/// The checks the standard build ships a query for, by the name a repository
/// lists them under. `@FILTER@` is the test for a term of the ontology's own.
const CHECKS: &[(&str, &str)] = &[
    ("owldef-self-reference", include_str!("update/checks/owldef-self-reference-violation.sparql")),
    ("redundant-subClassOf", include_str!("update/checks/redundant-subClassOf-violation.sparql")),
    ("taxon-range", include_str!("update/checks/taxon-range-violation.sparql")),
    ("iri-range", include_str!("update/checks/iri-range-violation.sparql")),
    ("iri-range-advanced", include_str!("update/checks/iri-range-advanced-violation.sparql")),
    ("label-with-iri", include_str!("update/checks/label-with-iri-violation.sparql")),
    ("multiple-replaced_by", include_str!("update/checks/multiple-replaced_by-violation.sparql")),
    ("term-tracker-uri", include_str!("update/checks/term-tracker-uri-violation.sparql")),
    ("illegal-date", include_str!("update/checks/illegal-date-violation.sparql")),
    ("dc-properties", include_str!("update/checks/dc-properties-violation.sparql")),
];

/// `<uribase>/<suffix or id>`: what the ontology's modules hang off.
fn base(c: &Config) -> String {
    format!("{}/{}", c.uribase, c.uribase_suffix.as_deref().unwrap_or(&c.id))
}

/// Every ontology the edit file imports, as `(IRI, path from the catalog)`, in the
/// order ODK declares them.
fn declared_imports(c: &Config) -> Vec<(String, String)> {
    let base = base(c);
    let mut out = Vec::new();
    if let Some(group) = &c.import_group {
        if group.use_base_merging {
            out.push((format!("{base}/imports/merged_import.owl"), "imports/merged_import.owl".to_string()));
        } else {
            for p in &group.products {
                let file = format!("imports/{}_import.owl", p.id);
                out.push((format!("{base}/{file}"), file));
            }
        }
    }
    for p in c.components.iter().flat_map(|g| &g.products) {
        let file = format!("components/{}", p.filename);
        out.push((format!("{base}/{file}"), file));
    }
    if c.use_dosdps {
        out.push((format!("{base}/patterns/definitions.owl"), "../patterns/definitions.owl".to_string()));
        if c.import_pattern_ontology {
            out.push((format!("{base}/patterns/pattern.owl"), "../patterns/pattern.owl".to_string()));
        }
    }
    out
}

pub fn update_repo(repo: &OdkRepo) -> Result<()> {
    let Some(config) = repo.builtin.as_ref().filter(|_| repo.built_from_file()) else {
        bail!(
            "`update_repo` brings a repository's files into step with the options in its \
             owlmake.yaml, and this repository is not built from one: where a generated \
             Makefile is the build, regenerating it is ODK's to do; where the build is \
             the repository's own (`use_builtin_rules: false`), there are no options to \
             bring anything into step with"
        );
    };
    let mut wrote = 0;
    wrote += write_stubs(config, &repo.dir)?;
    wrote += write_queries(config, &repo.dir)?;
    if config.manage_import_declarations {
        let imports = declared_imports(config);
        update_catalog(&repo.dir.join(&config.catalog_file), &imports)?;
        update_import_declarations(config, &repo.dir, &imports)?;
    } else {
        status!(
            "update_repo: `manage_import_declarations` is off, so the edit file and the XML \
             catalog are yours to update for any import or component added or removed"
        );
    }
    status!("update_repo: done ({wrote} file(s) written besides the edit file and the catalog)");
    Ok(())
}

// === Files the build reads ===================================================

/// Write `text` to `path` unless something is already there. Returns whether it wrote.
fn if_missing(path: &Path, text: &str) -> Result<usize> {
    if path.exists() {
        return Ok(0);
    }
    always(path, text)
}

fn always(path: &Path, text: &str) -> Result<usize> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::read_to_string(path).is_ok_and(|was| was == text) {
        return Ok(0);
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    status!("update_repo: wrote {}", path.display());
    Ok(1)
}

/// The files ODK's template lays down for these options, in the template's
/// order: `Some` for one the build reads, `None` where ODK writes something
/// this does not (a README, the generated Makefile, the registry metadata).
///
/// The order matters for one byte. ODK renders all of them as one text and
/// splits it on newlines, and the empty piece after the final newline is
/// written to whichever file is last — so the last file of the pack ends in a
/// blank line, and which file that is depends on the options.
fn stubs(c: &Config, dir: &Path) -> Vec<Option<(std::path::PathBuf, String)>> {
    let src = dir.parent().unwrap_or(dir);
    let base = base(c);
    let upper = c.id.to_uppercase();
    let mut pack = vec![None];
    pack.push(Some((
        dir.join(format!("{}-idranges.owl", c.id)),
        include_str!("update/idranges.omn")
            .replace("@BASE@", &base)
            .replace("@URIBASE@", &c.uribase)
            .replace("@ID_UPPER@", &upper)
            .replace("@ID@", &c.id),
    )));
    pack.push(None);
    if let Some(group) = &c.import_group {
        let module = |name: &str| {
            let text = include_str!("update/import.ofn").replace("@IRI@", &format!("{base}/imports/{name}"));
            Some((dir.join("imports").join(name), text))
        };
        if group.use_base_merging {
            pack.push(module("merged_import.owl"));
        } else {
            pack.extend(group.products.iter().map(|p| module(&format!("{}_import.owl", p.id))));
        }
        for p in &group.products {
            pack.push(Some((dir.join(format!("imports/{}_terms.txt", p.id)), String::new())));
        }
    }
    if c.create_obo_metadata {
        pack.push(None);
    }
    if c.use_templates {
        pack.push(None);
        for p in c.components.iter().flat_map(|g| &g.products).filter(|p| p.use_template) {
            let stem = p.filename.split('.').next().unwrap_or(&p.filename);
            let unlisted = vec![format!("{stem}.tsv")];
            for template in p.templates.as_ref().unwrap_or(&unlisted) {
                pack.push(Some((src.join("templates").join(template), include_str!("update/template.tsv").to_string())));
            }
        }
    }
    if c.use_mappings {
        pack.push(None);
        for set in c.sssom_mappingset_group.iter().flat_map(|g| g.products.iter().flatten()) {
            pack.push(Some((
                src.join(format!("mappings/{}.sssom.tsv", set.id)),
                include_str!("update/mappings.sssom.tsv")
                    .replace("@MAPPING@", &set.id)
                    .replace("@URIBASE@", &c.uribase)
                    .replace("@ID_UPPER@", &upper)
                    .replace("@ID@", &c.id),
            )));
        }
    }
    if c.use_translations {
        pack.push(None);
        for t in c.babelon_translation_group.iter().flat_map(|g| g.products.iter().flatten()) {
            pack.push(Some((
                src.join(format!("translations/{}.babelon.tsv", t.id)),
                include_str!("update/babelon.tsv").to_string(),
            )));
            if t.include_template_synonyms {
                pack.push(Some((
                    src.join(format!("translations/{}.synonyms.tsv", t.id)),
                    include_str!("update/synonyms.tsv").replace("@LANGUAGE@", &t.language),
                )));
            }
        }
    }
    if c.use_dosdps {
        pack.push(Some((src.join("patterns/definitions.owl"), include_str!("update/definitions.ofn").replace("@ID@", &c.id))));
        pack.push(Some((src.join("patterns/pattern.owl"), include_str!("update/pattern.ofn").replace("@ID@", &c.id))));
        pack.push(Some((src.join("patterns/dosdp-patterns/external.txt"), String::new())));
        // The README of the default pipeline's data comes after it.
        pack.push(None);
    }
    if c.use_context {
        pack.push(Some((dir.join("config/context.json"), include_str!("update/context.json").to_string())));
    }
    for p in c.components.iter().flat_map(|g| &g.products) {
        pack.push(Some((
            dir.join("components").join(&p.filename),
            include_str!("update/component.owl")
                .replace("@IRI@", &format!("{base}/components/{}", p.filename))
                .replace("@URIBASE@", &c.uribase),
        )));
    }
    if c.primary_release == "basic" || c.release_artefacts.iter().any(|a| a == "basic") {
        pack.push(Some((dir.join("keeprelations.txt"), include_str!("update/keeprelations.txt").to_string())));
    }
    if c.use_custom_import_module {
        pack.push(Some((src.join("templates/external_import.tsv"), include_str!("update/external_import.tsv").to_string())));
    }
    if let Some(Some((_, text))) = pack.last_mut() {
        text.push('\n');
    }
    pack
}

fn write_stubs(c: &Config, dir: &Path) -> Result<usize> {
    let mut n = 0;
    for (path, text) in stubs(c, dir).into_iter().flatten() {
        n += if_missing(&path, &text)?;
    }
    Ok(n)
}

fn write_queries(c: &Config, dir: &Path) -> Result<usize> {
    let sparql = dir.parent().unwrap_or(dir).join("sparql");
    let own_term = match c.namespaces.as_deref() {
        Some(namespaces) if !namespaces.is_empty() => namespaces
            .iter()
            .map(|ns| format!("STRSTARTS(str(?term), \"{ns}\")"))
            .collect::<Vec<_>>()
            .join(" || "),
        _ => format!("STRSTARTS(str(?term), \"{}/{}_\")", c.uribase, c.id.to_uppercase()),
    };
    let mut n = 0;
    for (name, text) in QUERIES {
        n += always(&sparql.join(name), text)?;
    }
    // One pack again (see `stubs`): the last of these ends in a blank line.
    let report = &c.report;
    let mut pack = vec![(format!("{}_terms.sparql", c.id), include_str!("update/terms.sparql"))];
    for (check, text) in CHECKS {
        if report.checks_unstated || report.custom_sparql_checks.iter().any(|c| c == check) {
            pack.push((format!("{check}-violation.sparql"), *text));
        }
    }
    let last = pack.len() - 1;
    for (i, (name, text)) in pack.iter().enumerate() {
        let mut text = text.replace("@FILTER@", &own_term);
        if i == last {
            text.push('\n');
        }
        n += always(&sparql.join(name), &text)?;
    }
    Ok(n)
}

// === The edit file ===========================================================

fn update_import_declarations(c: &Config, dir: &Path, imports: &[(String, String)]) -> Result<()> {
    use horned_owl::model::{AnnotatedComponent, Component, Import, MutableOntology, IRI};
    let (extension, format) = match c.edit_format.as_str() {
        "obo" => ("obo", crate::io::Format::Obo),
        _ => ("owl", crate::io::Format::Functional),
    };
    let path = dir.join(format!("{}-edit.{extension}", c.id));
    let mut model = crate::io::load(&path).with_context(|| format!("reading {}", path.display()))?;
    let declared = |ac: &AnnotatedComponent<crate::model::Str>| match &ac.component {
        Component::Import(i) => Some(i.0.as_ref().to_string()),
        _ => None,
    };
    let was: Vec<String> = model.ont.iter().filter_map(declared).collect();
    let stale: Vec<_> = model
        .ont
        .iter()
        .filter(|ac| declared(ac).is_some_and(|iri| !imports.iter().any(|(i, _)| *i == iri)))
        .cloned()
        .collect();
    for ac in &stale {
        model.ont.remove(ac);
    }
    for (iri, _) in imports.iter().filter(|(iri, _)| !was.contains(iri)) {
        let iri: IRI<crate::model::Str> = model.build.iri(iri.as_str());
        model.ont.insert(AnnotatedComponent::from(Component::Import(Import(iri))));
    }
    crate::io::save_as(&mut model, &path, format).with_context(|| format!("writing {}", path.display()))
}

// === The XML catalog =========================================================

/// An element of a catalog: its name, its attributes as written, its elements.
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<Element>,
}

impl Element {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn uri(name: &str, uri: &str) -> Element {
        Element {
            name: "uri".to_string(),
            attributes: vec![("name".to_string(), name.to_string()), ("uri".to_string(), uri.to_string())],
            children: Vec::new(),
        }
    }
}

fn parse_catalog(path: &Path) -> Result<Element> {
    use quick_xml::events::Event;
    let text = std::fs::read_to_string(path)?;
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut open: Vec<Element> = Vec::new();
    let element = |e: &quick_xml::events::BytesStart| -> Result<Element> {
        let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
        if name.contains(':') {
            bail!("catalog {}: `{name}` is not an element of an XML catalog", path.display());
        }
        let mut attributes = Vec::new();
        for a in e.attributes() {
            let a = a?;
            let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            // The namespace is the writer's to declare.
            if key != "xmlns" {
                attributes.push((key, a.unescape_value()?.into_owned()));
            }
        }
        Ok(Element { name, attributes, children: Vec::new() })
    };
    loop {
        match reader.read_event()? {
            Event::Start(e) => open.push(element(&e)?),
            Event::Empty(e) => {
                let el = element(&e)?;
                match open.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => return Ok(el),
                }
            }
            Event::End(_) => {
                let el = open.pop().context("an end tag with nothing open")?;
                match open.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => return Ok(el),
                }
            }
            Event::Eof => bail!("catalog {} has no root element", path.display()),
            // Comments, text and declarations are not part of what is kept.
            _ => {}
        }
    }
}

/// What the curators' part of a catalog keeps: not an entry the managed group
/// now holds, not the managed group of last time, and not the empty `xml:base`
/// Protégé writes, which is not valid XML.
fn kept(children: Vec<Element>, managed: &[(String, String)]) -> Vec<Element> {
    children
        .into_iter()
        .filter_map(|mut child| match child.name.as_str() {
            "uri" => {
                let is_managed = matches!(
                    (child.attribute("name"), child.attribute("uri")),
                    (Some(n), Some(u)) if managed.iter().any(|(mn, mu)| mn == n && mu == u)
                );
                (!is_managed).then_some(child)
            }
            "group" if child.attribute("id") == Some(MANAGED_GROUP) => None,
            "group" => {
                child.attributes.retain(|(n, v)| !(n == "xml:base" && v.is_empty()));
                child.children = kept(std::mem::take(&mut child.children), managed);
                Some(child)
            }
            _ => Some(child),
        })
        .collect()
}

/// An attribute value as Python's ElementTree writes one.
fn escaped(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#09;"),
            c => out.push(c),
        }
    }
    out
}

fn write_element(out: &mut String, el: &Element, depth: usize, namespace: Option<&str>) {
    let indent = "  ".repeat(depth);
    out.push_str(&format!("{indent}<{}", el.name));
    if let Some(ns) = namespace {
        out.push_str(&format!(" xmlns=\"{ns}\""));
    }
    for (name, value) in &el.attributes {
        out.push_str(&format!(" {name}=\"{}\"", escaped(value)));
    }
    if el.children.is_empty() {
        out.push_str(" />");
        return;
    }
    out.push('>');
    for child in &el.children {
        out.push('\n');
        write_element(out, child, depth + 1, None);
    }
    out.push_str(&format!("\n{indent}</{}>", el.name));
}

/// Rewrite the catalog's managed group from `imports`, keeping the rest of it.
/// The file comes out as ODK's `update_repo` leaves it, which is as Python's
/// ElementTree serialises it.
fn update_catalog(path: &Path, imports: &[(String, String)]) -> Result<()> {
    let public = ("prefer".to_string(), "public".to_string());
    let mut root = Element { name: "catalog".to_string(), attributes: vec![public.clone()], children: Vec::new() };
    root.children.push(Element {
        name: "group".to_string(),
        attributes: vec![("id".to_string(), MANAGED_GROUP.to_string()), public],
        children: imports.iter().map(|(iri, file)| Element::uri(iri, file)).collect(),
    });
    if path.exists() {
        let existing = parse_catalog(path).with_context(|| format!("reading {}", path.display()))?;
        root.children.extend(kept(existing.children, imports));
    }
    let mut out = String::from("<?xml version='1.0' encoding='UTF-8'?>\n");
    write_element(&mut out, &root, 0, Some("urn:oasis:names:tc:entity:xmlns:xml:catalog"));
    if std::fs::read_to_string(path).is_ok_and(|was| was == out) {
        return Ok(());
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}
