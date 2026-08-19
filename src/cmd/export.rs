//! `export` — export ontology entities to a spreadsheet (TSV, CSV or HTML): one
//! row per entity, one column per named field, e.g. `ID|LABEL|SubClass Of`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotationSubject, AnnotationValue, ClassExpression as CE, Component, Individual, Literal,
    ObjectPropertyExpression as OPE, RcStr,
};

use crate::io::obo::compress_iri;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const IAO_DEF: &str = "http://purl.obolibrary.org/obo/IAO_0000115";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output format: tsv (default) or csv.
    #[arg(short, long, default_value = "tsv")]
    pub format: String,
    /// Ordered, `|`-separated list of column names for the header. Supported:
    /// `ID`, `LABEL`, `Definition`, `SubClass Of` (anonymous expressions
    /// rendered in Manchester syntax), `Equivalent`, `Disjoint`, and any
    /// annotation property by CURIE or label. Default: ID, LABEL, Definition,
    /// SubClass Of.
    #[arg(short = 'c', long, value_name = "COLS")]
    pub header: Option<String>,
    /// Column name to sort rows on. Default: first column.
    #[arg(short = 's', long, value_name = "COL")]
    pub sort: Option<String>,
    /// Entity types to include: comma/space-separated `classes`,
    /// `properties`, `individuals`. Default: classes.
    #[arg(short = 'n', long, value_name = "TYPES")]
    pub include: Option<String>,
    /// Entity types to exclude (owlmake extension): same vocabulary as
    /// `--include`. Applied after `--include`.
    #[arg(long, value_name = "TYPES")]
    pub exclude: Option<String>,
    /// How to render entities in cells: one of `ID`/`CURIE` (compressed
    /// IRI, default), `IRI`, `LABEL`/`NAME`.
    #[arg(short = 'E', long = "entity-format", value_name = "ARG")]
    pub entity_format: Option<String>,
    /// Which entities to render: `NAMED` (default), `ANONYMOUS`, or `ANY`/`ALL`.
    /// Accepted for compatibility; owlmake only renders named entities.
    #[arg(short = 'l', long = "entity-select", value_name = "ARG")]
    pub entity_select: Option<String>,
    /// Alternative output path; alias of `--output`.
    #[arg(short = 'e', long = "export", value_name = "FILE")]
    pub export: Option<PathBuf>,
    /// Character to split multi-valued cells on (default `|`).
    #[arg(short = 'S', long = "split", value_name = "ARG")]
    pub split: Option<String>,
    /// If true and the output format is HTML, generate a standalone HTML file.
    /// Accepted for compatibility; HTML output is not produced.
    #[arg(long, num_args = 1, default_missing_value = "true")]
    pub standalone: Option<bool>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// How an entity IRI is rendered in a cell.
#[derive(Clone, Copy)]
enum EntityFormat {
    /// Compressed CURIE — the `ID`/`CURIE` selection.
    Id,
    /// Full IRI.
    Iri,
    /// rdfs:label, falling back to the CURIE when absent — the `LABEL`/`NAME`
    /// selection.
    Label,
}

fn parse_entity_format(spec: &str) -> EntityFormat {
    match spec.trim().to_ascii_uppercase().as_str() {
        "IRI" => EntityFormat::Iri,
        "LABEL" | "NAME" => EntityFormat::Label,
        "ID" | "CURIE" => EntityFormat::Id,
        other => {
            status!("export: unknown entity format '{other}'; using ID (compressed IRI)");
            EntityFormat::Id
        }
    }
}

/// The set of entity types `export` can render.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Class,
    Property,
    Individual,
}

/// Parse an include/exclude type spec like "classes properties" into a set.
fn parse_kinds(spec: &str) -> Vec<Kind> {
    let mut out = Vec::new();
    for tok in spec.split([',', ' ', '\t']).map(|t| t.trim()).filter(|t| !t.is_empty()) {
        match tok.to_ascii_lowercase().as_str() {
            "class" | "classes" => out.push(Kind::Class),
            "property" | "properties" => out.push(Kind::Property),
            "individual" | "individuals" => out.push(Kind::Individual),
            other => status!("export: ignoring unknown entity type '{other}'"),
        }
    }
    out
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut defs: BTreeMap<String, String> = BTreeMap::new();
    // All superclass expressions per named class (named AND anonymous).
    let mut supers: BTreeMap<String, Vec<CE<RcStr>>> = BTreeMap::new();
    let mut equivs: BTreeMap<String, Vec<CE<RcStr>>> = BTreeMap::new();
    let mut disjoints: BTreeMap<String, Vec<CE<RcStr>>> = BTreeMap::new();
    // entity IRI -> (annotation property IRI -> rendered values).
    let mut anns: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    // Lowercased label / CURIE of each annotation property -> its IRI, so a
    // column can name an annotation property either way round.
    let mut prop_name_to_iri: BTreeMap<String, String> = BTreeMap::new();
    // Entities partitioned by kind.
    // Every `rdfs:label` an entity carries, and how many annotation assertions
    // it has in total — together these decide which label the entity exports as.
    let mut label_candidates: BTreeMap<String, Vec<(i32, String)>> = BTreeMap::new();
    let mut subject_ann_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut classes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut properties: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut individuals: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for ac in model.ont.iter() {
        match &ac.component {
            Component::DeclareClass(dc) => {
                classes.insert(dc.0 .0.as_ref().to_string());
            }
            Component::DeclareObjectProperty(p) => {
                properties.insert(p.0 .0.as_ref().to_string());
            }
            Component::DeclareDataProperty(p) => {
                properties.insert(p.0 .0.as_ref().to_string());
            }
            Component::DeclareAnnotationProperty(p) => {
                properties.insert(p.0 .0.as_ref().to_string());
            }
            Component::DeclareNamedIndividual(i) => {
                individuals.insert(i.0 .0.as_ref().to_string());
            }
            Component::AnnotationAssertion(aa) => {
                if let AnnotationSubject::IRI(s) = &aa.subject {
                    let prop = aa.ann.ap.0.as_ref().to_string();
                    let value = match &aa.ann.av {
                        AnnotationValue::Literal(lit) => match lit {
                            Literal::Simple { literal }
                            | Literal::Language { literal, .. }
                            | Literal::Datatype { literal, .. } => literal.clone(),
                        },
                        AnnotationValue::IRI(i) => i.as_ref().to_string(),
                        AnnotationValue::AnonymousIndividual(a) => a.0.as_ref().to_string(),
                    };
                    *subject_ann_count.entry(s.as_ref().to_string()).or_insert(0) += 1;
                    if prop == RDFS_LABEL {
                        // An entity may carry several labels; the exported one is
                        // decided below, once the whole set is known.
                        label_candidates.entry(s.as_ref().to_string()).or_default().push((
                            crate::owlapi_hash::annotation_assertion_hash(
                                s.as_ref(),
                                &prop,
                                &aa.ann.av,
                                &ac.ann,
                            ),
                            value.clone(),
                        ));
                    } else if prop == IAO_DEF {
                        defs.insert(s.as_ref().to_string(), value.clone());
                    }
                    anns.entry(s.as_ref().to_string())
                        .or_default()
                        .entry(prop)
                        .or_default()
                        .push(value);
                }
            }
            Component::SubClassOf(sc) => {
                if let CE::Class(sub) = &sc.sub {
                    // The built-in bottom class is not an exported row, however it
                    // is mentioned: `owl:Nothing ⊑ owl:Nothing` travels with any
                    // ontology whose axioms name it.
                    if sub.0.as_ref() != "http://www.w3.org/2002/07/owl#Nothing" {
                        classes.insert(sub.0.as_ref().to_string());
                    }
                    supers
                        .entry(sub.0.as_ref().to_string())
                        .or_default()
                        .push(sc.sup.clone());
                }
            }
            Component::EquivalentClasses(eq) => {
                // Attribute the other operands to each named-class operand.
                for (i, ce) in eq.0.iter().enumerate() {
                    if let CE::Class(c) = ce {
                        let key = c.0.as_ref().to_string();
                        for (j, other) in eq.0.iter().enumerate() {
                            if i != j {
                                equivs.entry(key.clone()).or_default().push(other.clone());
                            }
                        }
                    }
                }
            }
            Component::DisjointClasses(dj) => {
                for (i, ce) in dj.0.iter().enumerate() {
                    if let CE::Class(c) = ce {
                        let key = c.0.as_ref().to_string();
                        for (j, other) in dj.0.iter().enumerate() {
                            if i != j {
                                disjoints.entry(key.clone()).or_default().push(other.clone());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // An entity's exported label is the first `rdfs:label` in the iteration order
    // of the entity's own annotation-assertion set — so which of several labels
    // wins is decided by their hashes, not by document order or by the values.
    for (subject, cands) in &label_candidates {
        let chosen = if cands.len() == 1 {
            &cands[0].1
        } else {
            let hashes: Vec<i32> = cands.iter().map(|(h, _)| *h).collect();
            let total = subject_ann_count.get(subject).copied().unwrap_or(cands.len());
            let order = crate::owlapi_hash::hashset_order_of(&hashes, total);
            &cands[order[0]].1
        };
        labels.insert(subject.clone(), chosen.clone());
    }

    // Index annotation-property names (label + CURIE) to IRIs, for columns named
    // after a property (e.g. a column `alternative term`).
    for p in &properties {
        prop_name_to_iri.insert(compress_iri(p).to_ascii_lowercase(), p.clone());
        if let Some(l) = labels.get(p) {
            prop_name_to_iri.insert(l.to_ascii_lowercase(), p.clone());
        }
    }

    // Resolve which entity kinds to render. Default: classes only.
    let include = args
        .include
        .as_deref()
        .map(parse_kinds)
        .unwrap_or_else(|| vec![Kind::Class]);
    let exclude = args.exclude.as_deref().map(parse_kinds).unwrap_or_default();
    let want = |k: Kind| include.contains(&k) && !exclude.contains(&k);

    let mut entities: Vec<String> = Vec::new();
    if want(Kind::Class) {
        entities.extend(classes.iter().cloned());
    }
    if want(Kind::Property) {
        entities.extend(properties.iter().cloned());
    }
    if want(Kind::Individual) {
        entities.extend(individuals.iter().cloned());
    }

    // Resolve the header columns.
    const ALL_COLS: [&str; 4] = ["ID", "LABEL", "Definition", "SubClass Of"];
    let cols: Vec<String> = match &args.header {
        Some(h) => h
            .split('|')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect(),
        None => ALL_COLS.iter().map(|c| c.to_string()).collect(),
    };

    // How to render entities, and the multi-value cell delimiter.
    let entity_format = args
        .entity_format
        .as_deref()
        .map(parse_entity_format)
        .unwrap_or(EntityFormat::Id);

    // A header cell may carry its own entity format in brackets — `ID [IRI]`,
    // `SubClass Of [LABEL]` — which overrides `--entity-format` for that column
    // alone. The bracket is part of the column's NAME in the emitted header (the
    // reference writes `ID [IRI]` verbatim), so it is stripped only for looking the
    // column up. UBERON's seven subset `.tsv` exports are
    // `--header "ID [IRI]|LABEL"`: with the suffix unparsed, `ID [IRI]` matched no
    // known column and no annotation property, so every row came out with an empty
    // first cell — 26,680 rows of them, and no error.
    let col_specs: Vec<(String, EntityFormat)> = cols
        .iter()
        .map(|c| match (c.rfind('['), c.ends_with(']')) {
            (Some(i), true) => (
                c[..i].trim().to_string(),
                parse_entity_format(c[i + 1..c.len() - 1].trim()),
            ),
            _ => (c.clone(), entity_format),
        })
        .collect();
    // --entity-select: owlmake's rows are named entities, so NAMED/ANY/ALL render
    // them and ANONYMOUS (no named entities qualify) yields an empty table.
    if let Some(sel) = &args.entity_select {
        match sel.trim().to_ascii_uppercase().as_str() {
            "NAMED" | "ANY" | "ALL" => {}
            "ANONYMOUS" => {
                entities.clear();
            }
            other => status!("export: unknown --entity-select '{other}'; rendering named entities"),
        }
    }
    let split = args.split.clone().unwrap_or_else(|| "|".to_string());

    // Render an entity IRI per the entity-format in force for the column.
    let render_as = |iri: &str, fmt: EntityFormat| -> String {
        match fmt {
            EntityFormat::Id => compress_iri(iri),
            EntityFormat::Iri => iri.to_string(),
            EntityFormat::Label => labels
                .get(iri)
                .cloned()
                .unwrap_or_else(|| compress_iri(iri)),
        }
    };

    // Render a class expression in Manchester syntax, using the entity-format for
    // every named entity it mentions (so anonymous superclass restrictions —
    // `'part of' some hand` — appear in cells rather than being dropped).
    let render_exprs = |exprs: Option<&Vec<CE<RcStr>>>, fmt: EntityFormat| -> String {
        let render_entity = |iri: &str| render_as(iri, fmt);
        exprs
            .map(|xs| {
                xs.iter()
                    .map(|x| render_ce_manchester(x, &render_entity))
                    .collect::<Vec<_>>()
                    .join(&split)
            })
            .unwrap_or_default()
    };

    let cell = |iri: &str, col: &str, fmt: EntityFormat| -> String {
        match col {
            "ID" => render_as(iri, fmt),
            "LABEL" => labels.get(iri).cloned().unwrap_or_default(),
            "Definition" => defs.get(iri).cloned().unwrap_or_default(),
            "SubClass Of" | "SubClasses" | "SC" => render_exprs(supers.get(iri), fmt),
            "Equivalent Class" | "Equivalent Classes" | "Equivalent" | "EC" => {
                render_exprs(equivs.get(iri), fmt)
            }
            "Disjoint Class" | "Disjoint Classes" | "Disjoint" | "DC" => {
                render_exprs(disjoints.get(iri), fmt)
            }
            other => {
                // Resolve an arbitrary annotation-property column by CURIE or label.
                if let Some(prop) = prop_name_to_iri
                    .get(&other.to_ascii_lowercase())
                    .or_else(|| {
                        let c = compress_iri(other);
                        prop_name_to_iri.get(&c.to_ascii_lowercase())
                    })
                {
                    anns.get(iri)
                        .and_then(|m| m.get(prop))
                        .map(|vs| vs.join(&split))
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
        }
    };

    // Build the rows, then sort.
    let mut rows: Vec<(String, Vec<String>)> = entities
        .iter()
        .map(|iri| {
            let values: Vec<String> =
                col_specs.iter().map(|(c, fmt)| cell(iri, c, *fmt)).collect();
            (iri.clone(), values)
        })
        .collect();

    // Sort: by the named column if given (and present), else the first column.
    let sort_idx = match &args.sort {
        // Either spelling: `--sort ID` finds a column headed `ID [IRI]`, since the
        // bracket names a rendering and not a different column.
        Some(name) => cols
            .iter()
            .position(|c| c == name)
            .or_else(|| col_specs.iter().position(|(c, _)| c == name)),
        None => Some(0),
    };
    if let Some(idx) = sort_idx {
        rows.sort_by(|a, b| a.1.get(idx).cmp(&b.1.get(idx)));
    }

    // --export is an alias for --output; --output wins if both are given.
    let dest = args.output.as_ref().or(args.export.as_ref());

    // HTML output (`--format html`, or an .html/.htm output path): render an
    // HTML table. `--standalone` (default true) wraps it in a full document;
    // `--standalone false` emits just the `<table>` fragment.
    let html = args.format.eq_ignore_ascii_case("html")
        || dest
            .map(|p| {
                let s = p.to_string_lossy().to_ascii_lowercase();
                s.ends_with(".html") || s.ends_with(".htm")
            })
            .unwrap_or(false);

    let out = if html {
        render_html(&cols, &rows, args.standalone.unwrap_or(true))
    } else {
        let csv = args.format.eq_ignore_ascii_case("csv");
        let sep = if csv { ',' } else { '\t' };
        let mut out = String::new();
        let col_refs: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
        out.push_str(&join(&col_refs, sep, csv));
        out.push('\n');
        for (_, values) in &rows {
            let refs: Vec<&str> = values.iter().map(|s| s.as_str()).collect();
            out.push_str(&join(&refs, sep, csv));
            out.push('\n');
        }
        out
    };

    match dest {
        Some(p) => std::fs::write(p, out)?,
        None => print!("{out}"),
    }
    Ok(Some(model))
}

/// Render a class expression in Manchester syntax, rendering each named entity
/// with `re` (the chosen entity-format), so an anonymous restriction lands in the
/// cell as readable text instead of being dropped from the table.
fn render_ce_manchester(ce: &CE<RcStr>, re: &dyn Fn(&str) -> String) -> String {
    match ce {
        CE::Class(c) => re(c.0.as_ref()),
        CE::ObjectIntersectionOf(v) => v
            .iter()
            .map(|x| paren(render_ce_manchester(x, re)))
            .collect::<Vec<_>>()
            .join(" and "),
        CE::ObjectUnionOf(v) => v
            .iter()
            .map(|x| paren(render_ce_manchester(x, re)))
            .collect::<Vec<_>>()
            .join(" or "),
        CE::ObjectComplementOf(b) => format!("not {}", paren(render_ce_manchester(b, re))),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            format!("{} some {}", render_ope(ope, re), paren(render_ce_manchester(bce, re)))
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            format!("{} only {}", render_ope(ope, re), paren(render_ce_manchester(bce, re)))
        }
        CE::ObjectHasValue { ope, i } => format!("{} value {}", render_ope(ope, re), render_ind(i, re)),
        CE::ObjectHasSelf(ope) => format!("{} Self", render_ope(ope, re)),
        CE::ObjectMinCardinality { n, ope, bce } => {
            format!("{} min {} {}", render_ope(ope, re), n, paren(render_ce_manchester(bce, re)))
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            format!("{} max {} {}", render_ope(ope, re), n, paren(render_ce_manchester(bce, re)))
        }
        CE::ObjectExactCardinality { n, ope, bce } => {
            format!("{} exactly {} {}", render_ope(ope, re), n, paren(render_ce_manchester(bce, re)))
        }
        CE::ObjectOneOf(v) => format!(
            "{{{}}}",
            v.iter().map(|i| render_ind(i, re)).collect::<Vec<_>>().join(", ")
        ),
        other => format!("{other:?}"),
    }
}

fn render_ope(ope: &OPE<RcStr>, re: &dyn Fn(&str) -> String) -> String {
    match ope {
        OPE::ObjectProperty(p) => re(p.0.as_ref()),
        OPE::InverseObjectProperty(p) => format!("inverse {}", re(p.0.as_ref())),
    }
}

fn render_ind(i: &Individual<RcStr>, re: &dyn Fn(&str) -> String) -> String {
    match i {
        Individual::Named(n) => re(n.0.as_ref()),
        Individual::Anonymous(a) => format!("_:{}", a.0.as_ref()),
    }
}

/// Parenthesize a rendered sub-expression when it contains a space (so nested
/// operators bind correctly); leave atomic names bare.
fn paren(s: String) -> String {
    if s.contains(' ') {
        format!("({s})")
    } else {
        s
    }
}

/// Render the export table as HTML (`--format html`). When `standalone`, wrap the
/// `<table>` in a complete HTML document.
fn render_html(cols: &[String], rows: &[(String, Vec<String>)], standalone: bool) -> String {
    let mut t = String::from("<table>\n<thead>\n<tr>");
    for c in cols {
        t.push_str(&format!("<th>{}</th>", html_escape(c)));
    }
    t.push_str("</tr>\n</thead>\n<tbody>\n");
    for (_, values) in rows {
        t.push_str("<tr>");
        for v in values {
            t.push_str(&format!("<td>{}</td>", html_escape(v)));
        }
        t.push_str("</tr>\n");
    }
    t.push_str("</tbody>\n</table>\n");
    if standalone {
        format!(
            "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>Ontology export</title>\n</head>\n<body>\n{t}</body>\n</html>\n"
        )
    } else {
        t
    }
}

/// Escape the five XML/HTML metacharacters in cell text.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn join(cells: &[&str], sep: char, csv: bool) -> String {
    cells
        .iter()
        .map(|c| {
            if csv && (c.contains(',') || c.contains('"')) {
                format!("\"{}\"", c.replace('"', "\"\""))
            } else {
                c.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(&sep.to_string())
}
