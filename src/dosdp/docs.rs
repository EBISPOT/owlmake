//! A pattern's documentation page.
//!
//! A page shows what the pattern generates for one row: each entity variable
//! filled with the class `http://dosdp.org/filler/<var>`, read `` `{var}` ``,
//! each data variable with the text `` `{var}` ``, and the pattern's own
//! `pattern_iri` as the defined class. Those axioms are sorted into the page's
//! sections by what they say about the class `pattern_iri` names as written: its
//! labels (Name), its definitions, its other annotations, the classes it is
//! equivalent to, its superclasses, and everything else (Other axioms).
//!
//! The generated axioms are a set, and so are a section's axioms and its lines:
//! a line is written once. A row that generates four axioms or fewer holds them
//! in the order they were made, the logical axioms first, and a section keeps
//! that order. A row that generates more holds them in a hash set, and every
//! set taken from that one is a hash set too: a section's lines come in the
//! order of a hash trie over their hashes as text (see [`crate::hash_trie`]),
//! however few there are.
//!
//! An entity is written as a Markdown link to its IRI, by its label in the
//! supplied ontology (see [`crate::io::entities::held_labels`]) or else by its
//! IRI's short form; a variable's placeholder class by `` `{var}` `` alone, and
//! an IRI annotation value as an entity. A literal is quoted, its text
//! HTML-escaped (see [`crate::html_escape`]), and followed by its language tag
//! or by its datatype as a link; an `xsd:decimal`, `xsd:integer` or
//! `xsd:boolean` value stands bare, and an `xsd:float` bare with an `f`. The
//! value an annotation template makes is an `xsd:string`; a literal a logical
//! template names without a datatype is a plain literal, which has none to
//! write. A class
//! expression is laid out as a frame lays it out and then flattened to one
//! line, each line break becoming a space; an axiom is written as a one-line
//! frame (see [`crate::io::manchester_write`]).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use horned_owl::model::{
    AnnotatedComponent, AnnotationAssertion, AnnotationSubject, AnnotationValue, Build, ClassExpression as CE,
    Component, Literal, RcStr,
};

use super::render::{self, Prefixes};
use super::table::{self, Row, TableFormat};
use super::{ordered_keys, parse_pattern, scala_map_key_order, scala_mutable_set_order, writes_as_0_20, Restrict};
use crate::io::manchester_write::{axiom_text, object_text, Layout, Names, Object};
use crate::io::natural_order::NaturalOrder;
use crate::model::Model;
use crate::owlapi_hash::java_string_hash;

type AC = AnnotatedComponent<RcStr>;

/// The namespace of the classes a page fills entity variables with.
const FILLER_NS: &str = "http://dosdp.org/filler/";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const IAO_DEFINITION: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// What documentation pages are written with besides their patterns and
/// tables.
pub struct DocsOptions<'a> {
    /// The supplied ontology, its imports included: the labels entities are
    /// written by, and the values of the readable-identifier properties.
    pub ontology: Option<&'a Model>,
    /// How a CURIE becomes an IRI.
    pub prefixes: Prefixes,
    /// How a table separates and quotes its cells.
    pub table_format: TableFormat,
    /// What a table's file name is appended to, to link the page to it.
    pub data_location_prefix: String,
}

/// The supplied ontology, read once for every page.
struct Supplied<'a> {
    opts: &'a DocsOptions<'a>,
    /// The label each entity is written by.
    names: HashMap<String, String>,
    /// The ontology's literal annotation values: term → property → values.
    index: HashMap<String, HashMap<String, Vec<String>>>,
}

impl<'a> Supplied<'a> {
    fn new(opts: &'a DocsOptions<'a>) -> Supplied<'a> {
        let (names, index) = match opts.ontology {
            Some(m) => {
                let names = crate::io::entities::held_labels(m)
                    .into_iter()
                    .map(|(iri, label)| {
                        let text = match label {
                            crate::io::entities::HeldLabel::Literal(l) => l.to_string(),
                            crate::io::entities::HeldLabel::Iri(i) => crate::owlapi_hash::iri_split(i).1.to_string(),
                        };
                        (iri.to_string(), text)
                    })
                    .collect();
                (names, super::annotation_index_of(m))
            }
            None => (HashMap::new(), HashMap::new()),
        };
        Supplied { opts, names, index }
    }
}

/// Write each pattern of `patterns` — `<template_dir>/<pattern>.yaml` over
/// `<infile_dir>/<pattern>.<format>` — as `<outdir>/<pattern>.md`, then an
/// `index.md` listing them by name. Every pattern is checked to exist before
/// any page is written; a page that cannot be written stops the run, and no
/// index is written.
pub fn docs_batch(
    template_dir: &Path,
    infile_dir: &Path,
    patterns: &[String],
    outdir: &Path,
    opts: &DocsOptions,
) -> Result<()> {
    for p in patterns {
        let yaml = template_dir.join(format!("{p}.yaml"));
        if !yaml.exists() {
            bail!("docs: the pattern {p} does not exist ({} is missing)", yaml.display());
        }
    }
    for (dir, what) in [(template_dir, "--template"), (infile_dir, "--infile"), (outdir, "--outfile")] {
        if !dir.is_dir() {
            bail!("docs: {what} names {}, and over a batch of patterns it names a directory", dir.display());
        }
    }
    let cx = Supplied::new(opts);
    let ext = opts.table_format.extension();
    let mut index: Vec<(Option<String>, Option<String>, String)> = Vec::new();
    for p in patterns {
        let yaml_path = template_dir.join(format!("{p}.yaml"));
        let data_path = infile_dir.join(format!("{p}.{ext}"));
        let out = outdir.join(format!("{p}.md"));
        write_page(&yaml_path, &data_path, &out, &cx).with_context(|| format!("docs: the pattern {p}"))?;
    }
    for p in patterns {
        let yaml_path = template_dir.join(format!("{p}.yaml"));
        let yaml = std::fs::read_to_string(&yaml_path).with_context(|| format!("reading {}", yaml_path.display()))?;
        let pattern = parse_pattern(&yaml)?;
        index.push((pattern.pattern_name, pattern.description, format!("{p}.md")));
    }
    // By name, a pattern with none first; names compare as UTF-16 text.
    index.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => x.encode_utf16().cmp(y.encode_utf16()),
        (x, y) => x.is_some().cmp(&y.is_some()),
    });
    let rows: Vec<String> = index
        .iter()
        .map(|(name, desc, file)| {
            format!(
                "| [{}]({file}) | {} |",
                name.as_deref().unwrap_or("*unnamed*"),
                desc.as_deref().unwrap_or("*no description*")
            )
        })
        .collect();
    let md = format!("# Design Patterns\n\n| Pattern | Description |\n|:--------|:------------|\n{}\n", rows.join("\n"));
    std::fs::write(outdir.join("index.md"), md).with_context(|| format!("writing {}/index.md", outdir.display()))?;
    Ok(())
}

/// Write the pattern `template` over the table `infile` as the page `outfile`.
pub fn docs_page(template: &Path, infile: &Path, outfile: &Path, opts: &DocsOptions) -> Result<()> {
    write_page(template, infile, outfile, &Supplied::new(opts))
}

fn write_page(template: &Path, infile: &Path, outfile: &Path, cx: &Supplied) -> Result<()> {
    let yaml = std::fs::read_to_string(template).with_context(|| format!("reading the pattern {}", template.display()))?;
    let bytes = std::fs::read(infile).with_context(|| format!("reading the table {}", infile.display()))?;
    let data = String::from_utf8_lossy(&bytes);
    let file_name = infile.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let location = format!("{}{file_name}", cx.opts.data_location_prefix);
    let md = page(&yaml, &data, &location, cx)?;
    std::fs::write(outfile, md).with_context(|| format!("writing {}", outfile.display()))
}

/// How a page names entities and writes literals.
struct PageNames<'a> {
    labels: &'a HashMap<String, String>,
}

impl Names for PageNames<'_> {
    fn entity(&self, iri: &str) -> String {
        if let Some(var) = iri.strip_prefix(FILLER_NS) {
            return format!("`{{{var}}}`");
        }
        match self.labels.get(iri) {
            Some(label) => format!("[{label}]({iri})"),
            None => format!("[{}]({iri})", crate::owlapi_hash::iri_split(iri).1),
        }
    }

    fn literal(&self, l: &Literal<RcStr>, datatype: &str) -> Option<String> {
        let lex = l.literal();
        match datatype.strip_prefix(XSD) {
            Some("decimal" | "integer" | "boolean") => return Some(lex.to_string()),
            Some("float") => return Some(format!("{lex}f")),
            _ => {}
        }
        let mut text = format!("\"{}\"", crate::html_escape::escape_html4(lex));
        match l {
            Literal::Language { lang, .. } if !lang.is_empty() => {
                text.push('@');
                text.push_str(lang);
            }
            _ if datatype != RDF_PLAIN_LITERAL => {
                text.push_str("^^");
                text.push_str(&self.entity(datatype));
            }
            _ => {}
        }
        Some(text)
    }

    fn iri(&self, iri: &str) -> String {
        self.entity(iri)
    }
}

/// The page of the pattern `pattern_yaml` over the table `data`, linking to the
/// table at `location`.
fn page(pattern_yaml: &str, data: &str, location: &str, cx: &Supplied) -> Result<String> {
    let pattern = parse_pattern(pattern_yaml)?;
    let Some(pattern_iri) = pattern.pattern_iri.as_deref() else {
        bail!("a pattern's documentation needs its `pattern_iri`, and this pattern has none");
    };
    let vars = ordered_keys(pattern_yaml, "vars", &pattern.vars);
    let list_vars = ordered_keys(pattern_yaml, "list_vars", &pattern.list_vars);
    let data_vars = ordered_keys(pattern_yaml, "data_vars", &pattern.data_vars);
    let data_list_vars = ordered_keys(pattern_yaml, "data_list_vars", &pattern.data_list_vars);

    // The row: a placeholder class, labelled `{var}`, for each entity variable;
    // `{var}` itself for each data variable; the pattern as the defined class.
    let mut placeholders: HashMap<String, String> = HashMap::new();
    let mut cells: Vec<(String, String)> = Vec::new();
    for v in vars.iter().chain(&list_vars) {
        let iri = format!("{FILLER_NS}{v}");
        placeholders.insert(iri.clone(), format!("`{{{v}}}`"));
        cells.push((v.clone(), iri));
    }
    for v in data_vars.iter().chain(&data_list_vars) {
        cells.push((v.clone(), format!("`{{{v}}}`")));
    }
    cells.push((render::DEFINED_CLASS.to_string(), pattern_iri.to_string()));
    let row = Row { cells };

    let b: Build<RcStr> = Build::new();
    let readable = render::Readable { labels: &placeholders, index: &cx.index };
    let renderer = render::Renderer::new(&b, &pattern, pattern_yaml, &cx.opts.prefixes, readable, writes_as_0_20())?;
    renderer.check_readable()?;
    let options = render::RowOptions { restrict_axioms: Restrict::All, restrict_axioms_column: None, generate_defined_class: false };
    let (logical, annotation) = renderer.render(&row, &options)?;
    let mut generated: Vec<AC> = logical;
    generated.extend(annotation);
    let hashed = generated.len() > 4;

    let pattern_class = |ce: &CE<RcStr>| matches!(ce, CE::Class(c) if c.0.as_ref() == pattern_iri);
    let about_pattern = |aa: &AnnotationAssertion<RcStr>| matches!(&aa.subject, AnnotationSubject::IRI(s) if s.as_ref() == pattern_iri);
    let (mut names, mut definitions, mut annotations) = (Vec::new(), Vec::new(), Vec::new());
    let (mut equivalents, mut supers, mut others) = (Vec::new(), Vec::new(), Vec::new());
    for ac in &generated {
        match &ac.component {
            Component::AnnotationAssertion(aa) if about_pattern(aa) => match aa.ann.ap.0.as_ref() {
                RDFS_LABEL => names.push(ac),
                IAO_DEFINITION => definitions.push(ac),
                _ => annotations.push(ac),
            },
            Component::EquivalentClasses(x) if x.0.iter().any(pattern_class) => equivalents.push(ac),
            Component::SubClassOf(x) if pattern_class(&x.sub) => supers.push(ac),
            _ => others.push(ac),
        }
    }

    // An annotation value's untyped literal is an `xsd:string`; a logical
    // template's is a plain literal.
    let (typed, plain) = (NaturalOrder::new(true), NaturalOrder::new(false));
    let page_names = PageNames { labels: &cx.names };
    let ce_text = |ce: &CE<RcStr>| object_text(Object::Ce(ce), plain, &page_names, Layout::Frame).replace('\n', " ");
    let value_text = |av: &AnnotationValue<RcStr>| -> String {
        match av {
            AnnotationValue::Literal(l) => {
                page_names.literal(l, typed.literal_datatype(l)).unwrap_or_default().replace('\n', " ")
            }
            AnnotationValue::IRI(iri) => page_names.entity(iri.as_ref()),
            AnnotationValue::AnonymousIndividual(a) => crate::io::entities::node_id(a.0.as_ref()).to_string(),
        }
    };
    let assertion = |ac: &AC| match &ac.component {
        Component::AnnotationAssertion(aa) => aa.clone(),
        _ => unreachable!("an annotation section holds annotation assertions"),
    };

    let mut var_rows: Vec<String> = Vec::new();
    for v in vars.iter().chain(&list_vars) {
        let range = pattern.vars.get(v).or_else(|| pattern.list_vars.get(v)).map(String::as_str).unwrap_or_default();
        let ce = renderer.range_expression(range)?;
        var_rows.push(format!("| `{{{v}}}` | {} |", ce_text(&ce)));
    }
    for v in data_vars.iter().chain(&data_list_vars) {
        let range = pattern.data_vars.get(v).or_else(|| pattern.data_list_vars.get(v)).map(String::as_str).unwrap_or_default();
        var_rows.push(format!("| `{{{v}}}` | {range} |"));
    }

    let lines = |texts: Vec<String>| string_set(texts, hashed);
    let name_lines = lines(names.iter().map(|ac| value_text(&assertion(ac).ann.av)).collect());
    let definition_lines = lines(definitions.iter().map(|ac| value_text(&assertion(ac).ann.av)).collect());
    let annotation_lines = lines(
        annotations
            .iter()
            .map(|ac| {
                let aa = assertion(ac);
                format!("- {}: {}", page_names.entity(aa.ann.ap.0.as_ref()), value_text(&aa.ann.av))
            })
            .collect(),
    );

    // Each equivalence's classes other than the pattern's, as a hash set of
    // their texts iterates them.
    let mut equivalent_classes: Vec<&CE<RcStr>> = Vec::new();
    for ac in &equivalents {
        if let Component::EquivalentClasses(x) = &ac.component {
            for ce in x.0.iter().filter(|ce| !pattern_class(ce)) {
                if !equivalent_classes.contains(&ce) {
                    equivalent_classes.push(ce);
                }
            }
        }
    }
    let equivalent_prefix = if equivalent_classes.len() > 1 { "- " } else { "" };
    let mut equivalent_texts: Vec<String> = Vec::new();
    for ac in &equivalents {
        if let Component::EquivalentClasses(x) = &ac.component {
            let texts: Vec<String> = x.0.iter().filter(|ce| !pattern_class(ce)).map(&ce_text).collect();
            let texts = scala_mutable_set_order(texts, java_string_hash);
            let prefixed = texts.into_iter().map(|t| format!("{equivalent_prefix}{t}")).collect();
            equivalent_texts.extend(scala_mutable_set_order(prefixed, java_string_hash));
        }
    }
    let equivalent_lines = lines(equivalent_texts);

    let super_prefix = if supers.len() > 1 { "- " } else { "" };
    let super_texts = lines(
        supers
            .iter()
            .map(|ac| match &ac.component {
                Component::SubClassOf(x) => ce_text(&x.sup),
                _ => unreachable!("a superclass section holds subclass axioms"),
            })
            .collect(),
    );
    let super_lines = lines(super_texts.into_iter().map(|t| format!("{super_prefix}{t}")).collect());

    let other_texts = lines(
        others
            .iter()
            .map(|ac| {
                let order = if matches!(ac.component, Component::AnnotationAssertion(_)) { typed } else { plain };
                axiom_text(&ac.component, order, &page_names)
                    .map(|t| t.replace('\n', " "))
                    .ok_or_else(|| anyhow!("a page has no way to write the axiom {:?}", ac.component))
            })
            .collect::<Result<_>>()?,
    );
    let other_lines = lines(other_texts.into_iter().map(|t| format!("- {t}")).collect());

    // The data preview: the columns of the first row, as a row's map of them
    // iterates; the first five rows.
    let (_, rows) = table::read_generator_table(data, cx.opts.table_format)?;
    let columns: Vec<String> = rows.first().map(|r| r.cells.iter().map(|(c, _)| c.clone()).collect()).unwrap_or_default();
    let columns: Vec<&String> = scala_map_key_order(&columns).into_iter().map(|i| &columns[i]).collect();
    let cell = |v: &str| match cx.opts.prefixes.iri(v) {
        Some(iri) => format!("[{v}]({iri})"),
        None => v.to_string(),
    };
    let data_rows: Vec<String> = rows
        .iter()
        .take(5)
        .map(|r| {
            let cells: Vec<String> = columns.iter().map(|c| cell(r.get(c).unwrap_or(""))).collect();
            format!("| {} |", cells.join(" | "))
        })
        .collect();
    let header_row = format!("| {} |", columns.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(" | "));
    let separator: String = columns.iter().map(|_| "|:--").collect();

    let contributors = match &pattern.contributors {
        Some(list) => format!("## Contributors\n\n{}", list.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n")),
        None => "\n".to_string(),
    };
    let section = |heading: &str, lines: &[String]| if lines.is_empty() { String::new() } else { format!("{heading}\n") };
    Ok(format!(
        "# {name}\n\n[{iri}]({iri})\n\n## Description\n\n{description}\n\n{contributors}\n\n## Variables\n\n\
         | Variable name | Allowed type |\n|:--------------|:-------------|\n{vars}\n\n## Name\n\n{names}\n\n\
         ## Annotations\n\n{annotations}\n\n## Definition\n\n{definitions}\n\n## Equivalent to\n\n{equivalents}\n\n\
         {super_heading}\n{supers}\n\n{other_heading}\n{others}\n\n## Data preview\n\n*See full table [here]({location})*\n\n\
         {header_row}\n{separator}|\n{data_rows}\n\n",
        name = pattern.pattern_name.as_deref().unwrap_or_default(),
        iri = pattern_iri,
        description = pattern.description.as_deref().unwrap_or("*No description*"),
        vars = var_rows.join("\n"),
        names = name_lines.join("\n\n"),
        annotations = annotation_lines.join("\n"),
        definitions = definition_lines.join("\n\n"),
        equivalents = equivalent_lines.join("\n"),
        super_heading = section("## Subclass of", &super_lines),
        supers = super_lines.join("\n"),
        other_heading = section("## Other axioms", &other_lines),
        others = other_lines.join("\n"),
        data_rows = data_rows.join("\n"),
    ))
}

/// The distinct `texts` as a set of them iterates: in the order given, or as a
/// `hashed` set in hash-trie order over their hashes.
fn string_set(texts: Vec<String>, hashed: bool) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::new();
    for t in texts {
        if !distinct.contains(&t) {
            distinct.push(t);
        }
    }
    if !hashed {
        return distinct;
    }
    let keyed: Vec<(usize, i32)> = distinct.iter().enumerate().map(|(i, t)| (i, java_string_hash(t))).collect();
    crate::hash_trie::order(&keyed).into_iter().map(|i| distinct[i].clone()).collect()
}
