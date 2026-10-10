//! `diff` — compare two ontologies and report added/removed components.
//!
//! Two report shapes are produced: `-f plain`/`-f pretty` render two flat counted
//! sections, `-f markdown` renders one frame per axiom subject. Both matter
//! because ontology repos COMMIT the output — OBA, CL and UBERON keep
//! `reports/release-diff.md` under version control, and EFO keeps
//! `reports/robot_diff.txt` plus `qc/diff_*_latest_release.txt` — so the layout
//! has to stay fixed, or every release diff churns on formatting alone.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use horned_owl::model::{AnnotatedComponent, Component, RcStr};

use super::manchester_markdown::{Dialect, Renderer};
use crate::diff;
use crate::io;
use crate::model::DocLabel;

#[derive(ClapArgs)]
pub struct Args {
    /// Left ontology file.
    #[arg(short = 'l', long)]
    pub left: Option<PathBuf>,
    /// Right ontology file.
    #[arg(short = 'r', long)]
    pub right: Option<PathBuf>,
    /// Load the left ontology from an IRI instead of a file.
    #[arg(short = 'L', long = "left-iri")]
    pub left_iri: Option<String>,
    /// Load the right ontology from an IRI instead of a file.
    #[arg(short = 'R', long = "right-iri")]
    pub right_iri: Option<String>,
    /// Catalog for resolving the left ontology's imports.
    #[arg(long = "left-catalog")]
    pub left_catalog: Option<PathBuf>,
    /// Catalog for resolving the right ontology's imports.
    #[arg(long = "right-catalog")]
    pub right_catalog: Option<PathBuf>,
    /// Output file for the diff report (defaults to stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Diff output format: plain (default), pretty, or markdown. (html is
    /// accepted but rendered as markdown.)
    #[arg(short = 'f', long = "format", default_value = "plain")]
    pub format: String,
    /// Comma-separated language tags, in priority order, for choosing the
    /// label a `pretty` report names an entity by (e.g. `en-GB,en,none`);
    /// `none` stands for a label with no language tag and `*` for any.
    #[arg(long = "label-langs-priority")]
    pub label_langs_priority: Option<String>,
    /// The ontology to diff as the LEFT side when `--left`/`--left-iri` is absent.
    /// `diff` is chainable — `om merge -i a.owl diff --right b.owl` compares the
    /// merged ontology against `b.owl` — so the piped or `--input` ontology stands
    /// in for the left. Accepted and unused when `--left` is given, which is how a
    /// release diff invokes it.
    #[arg(short = 'i', long)]
    pub input: Option<PathBuf>,
    /// Write the report in the `pretty` format, which names entities by their
    /// labels, where the `plain` format would be written (`true` or `yes` in
    /// any case).
    #[arg(long = "labels", num_args = 1, default_missing_value = "true", value_parser = crate::cmd::parse_option_true)]
    pub labels: Option<bool>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

/// One side of the comparison: the ontology compared, whose own axioms,
/// annotations and import declarations are what differs, and the ontologies
/// its imports closure read, which name entities too.
struct Side {
    model: crate::model::Model,
    /// The closure merged, with a banner document for each of its ontologies;
    /// `None` when the ontology imports nothing.
    imports: Option<crate::model::Model>,
    source: Source,
}

/// Where a side's ontology was read from.
enum Source {
    File(PathBuf),
    Iri(String),
    /// The ontology the pipeline hands the command.
    Piped,
}

impl Side {
    /// The label each ontology of the side gives an entity, the compared
    /// ontology's first.
    fn doc_labels(&self) -> Vec<std::sync::Arc<HashMap<String, DocLabel>>> {
        let mut docs = vec![std::sync::Arc::new(crate::cmd::doc_labels(&self.model))];
        if let Some(imports) = &self.imports {
            docs.extend(imports.banner_docs.iter().filter(|d| !d.root).map(|d| d.labels.clone()));
        }
        docs
    }

    /// The ontologies of the side, the compared one first.
    fn ontologies(&self) -> impl Iterator<Item = &crate::model::Model> {
        std::iter::once(&self.model).chain(self.imports.as_ref())
    }

    /// The format the side's ontology was read in, where it is known.
    fn format(&self) -> Option<io::Format> {
        match &self.source {
            Source::File(path) => io::format_of(path).ok(),
            Source::Iri(iri) => match io::file_iri_path(iri) {
                Some(path) => io::format_of(&path).ok(),
                None => io::Format::from_path(Path::new(iri)).ok(),
            },
            Source::Piped => None,
        }
    }

    /// How many ontology IDs reading the side mints: three for its own
    /// document and two for each document its imports read, with what reading
    /// each one mints itself ([`ids_read`]).
    fn ids_minted(&self) -> u32 {
        let mut minted = 3 + ids_read(self.format(), diff::ontology_id(&self.model).0.is_none());
        if let Some(imports) = &self.imports {
            let docs = imports.banner_docs.iter().filter(|d| !d.root);
            for (source, doc) in imports.import_sources.iter().zip(docs) {
                let format = source.path.as_deref().and_then(|p| io::format_of(p).ok());
                minted += 2 + ids_read(format, doc.iri.is_none());
            }
        }
        minted
    }

    /// The side's ontology ID as a report line: `OntologyID(OntologyIRI(<iri>)
    /// VersionIRI(<viri>))`, `<null>` standing for an absent version IRI, or
    /// `OntologyID(Anonymous-N)` for an unnamed ontology, `N` being the last ID
    /// reading its own document minted when `before` IDs had been minted. A
    /// piped ontology is numbered as if read by the command, in a format that
    /// mints none itself.
    fn id_line(&self, before: u32) -> String {
        match diff::ontology_id(&self.model) {
            (Some(iri), viri) => {
                let ver = viri.map(|v| format!("<{v}>")).unwrap_or_else(|| "<null>".to_string());
                format!("OntologyID(OntologyIRI(<{iri}>) VersionIRI({ver}))")
            }
            (None, _) => format!("OntologyID(Anonymous-{})", before + 2 + ids_read(self.format(), true)),
        }
    }
}

/// How many ontology IDs reading a document of `format` mints itself, the last
/// of them naming an unnamed ontology: functional syntax mints one for an
/// unnamed ontology, Manchester syntax one for any ontology and one more for an
/// unnamed one, and every other format none, an unnamed ontology keeping the
/// last ID its load minted.
fn ids_read(format: Option<io::Format>, unnamed: bool) -> u32 {
    match format {
        Some(io::Format::Functional) => u32::from(unnamed),
        Some(io::Format::Manchester) => 1 + u32::from(unnamed),
        _ => 0,
    }
}

/// Load one side of the diff from either a file (`--left`/`--right`) or an IRI
/// (`--left-iri`/`--right-iri`). Exactly one of the two must be given.
///
/// The side's imports closure is read where its own catalog option
/// (`--left-catalog`/`--right-catalog`) resolves each import, else the
/// catalog beside it, so an import that resolves nowhere fails the load.
fn load_side(
    path: Option<&std::path::Path>,
    iri: Option<&str>,
    which: &str,
    catalog: Option<&std::path::Path>,
    common: &crate::cmd::CommonArgs,
) -> anyhow::Result<Side> {
    let mut model = match (path, iri) {
        (Some(p), None) => io::load(p)?,
        (None, Some(i)) => io::load_iri(i, None)?,
        (Some(_), Some(_)) => {
            anyhow::bail!("diff: provide only one of --{which} or --{which}-iri")
        }
        (None, None) => anyhow::bail!("diff: --{which} or --{which}-iri is required"),
    };
    let imports = crate::cmd::read_imports(&mut model, path, catalog, common)?;
    let source = match (path, iri) {
        (Some(p), _) => Source::File(p.to_path_buf()),
        (None, Some(i)) => Source::Iri(i.to_string()),
        (None, None) => Source::Piped,
    };
    Ok(Side { model, imports, source })
}

/// The document IRI reported as `Loaded from:`. For a file it is `file:` plus
/// the ABSOLUTE path with a single slash, not three (`` `file:/work/src/ontology/cl.owl` ``),
/// which is the spelling committed diff reports carry; a side loaded from an IRI
/// reports that IRI unchanged.
fn document_iri(path: Option<&std::path::Path>, iri: Option<&str>) -> String {
    match (path, iri) {
        (Some(p), _) => {
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            match odk_work_path(&abs) {
                Some(w) => format!("file:{w}"),
                None => format!("file:{}", abs.display()),
            }
        }
        (None, Some(i)) => i.to_string(),
        (None, None) => String::new(),
    }
}

/// The path a document has inside the ODK container, where the repository is
/// mounted at `/work`: for a file under a repository whose plan emulates an
/// ODK release, `/work/<path from the repository root>`.
fn odk_work_path(abs: &std::path::Path) -> Option<String> {
    let mut dir = abs.parent()?;
    loop {
        let plan = dir.join("owlmake.yaml");
        if plan.is_file() {
            let text = std::fs::read_to_string(&plan).ok()?;
            let emulates = text.lines().any(|l| l.starts_with("emulate_odk_version:"));
            if !emulates {
                return None;
            }
            let rel = abs.strip_prefix(dir).ok()?;
            return Some(format!("/work/{}", rel.display()));
        }
        dir = dir.parent()?;
    }
}

/// The labels of the ontologies both sides read, as the report consults them:
/// the left side's, the compared ontology first, then the right side's.
fn label_docs(left: &Side, right: &Side) -> Vec<std::sync::Arc<HashMap<String, DocLabel>>> {
    let mut docs = left.doc_labels();
    docs.extend(right.doc_labels());
    docs
}

/// The label each entity is named by in a report, as a label provider over
/// every ontology both sides read gives it: the first literal label any of
/// them holds, else the last IRI value (see [`crate::cmd::fold_labels`]),
/// written as `iri_label` writes it.
fn label_map(left: &Side, right: &Side, iri_label: impl Fn(&str) -> String) -> HashMap<String, String> {
    let docs = label_docs(left, right);
    crate::cmd::fold_labels(docs.iter().map(|d| &**d))
        .into_iter()
        .map(|(subj, label)| {
            let text = match label {
                DocLabel::Literal(text) => text,
                DocLabel::Iri(iri) => iri_label(&iri),
            };
            (subj, text)
        })
        .collect()
}

/// The language tags `--label-langs-priority` gives, in priority order: each
/// comma-separated tag trimmed, an empty one dropped, and `none`, in any case,
/// the tag of a label with none.
fn parse_label_langs(csv: Option<&str>) -> Vec<String> {
    csv.into_iter()
        .flat_map(|csv| csv.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| if t.eq_ignore_ascii_case("none") { String::new() } else { t.to_string() })
        .collect()
}

/// Every literal `rdfs:label` of the ontologies both sides read, by subject:
/// each label's language tag (empty for none) and text.
fn literal_labels(left: &Side, right: &Side) -> HashMap<String, Vec<(String, String)>> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Literal};
    const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
    let mut out: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for model in left.ontologies().chain(right.ontologies()) {
        for ac in model.ont.iter() {
            let Component::AnnotationAssertion(aa) = &ac.component else { continue };
            let (AnnotationSubject::IRI(subj), AnnotationValue::Literal(lit)) = (&aa.subject, &aa.ann.av) else {
                continue;
            };
            if aa.ann.ap.0.as_ref() != RDFS_LABEL {
                continue;
            }
            let lang = match lit {
                Literal::Language { lang, .. } => lang.to_string(),
                Literal::Simple { .. } | Literal::Datatype { .. } => String::new(),
            };
            out.entry(subj.to_string()).or_default().push((lang, lit.literal().to_string()));
        }
    }
    out
}

/// The label `langs` picks among an entity's literal labels `candidates`
/// (language tag, text): the one whose tag best matches the earliest
/// preference — an exact match before a match of the tag's prefix, a longer
/// preference before a shorter, `*` matching any tag last — then the
/// smallest text; with no label matching, the smallest text of them all.
fn preferred_label(candidates: &[(String, String)], langs: &[String]) -> Option<String> {
    use crate::io::natural_order::str_cmp;
    // A tag's rank under the preferences: (index of the preference it best
    // matches, 0 for an exact match or 1 for a prefix or `*`).
    let rank = |lang: &str| -> Option<(usize, u8)> {
        let lang = lang.to_lowercase();
        let mut best: Option<(usize, u8, i64)> = None;
        for (i, pref) in langs.iter().enumerate() {
            let pref = pref.to_lowercase();
            let (kind, specificity) = if pref == "*" {
                (1, -1)
            } else if lang == pref {
                (0, pref.encode_utf16().count() as i64)
            } else if !pref.is_empty() && lang.starts_with(&format!("{pref}-")) {
                (1, pref.encode_utf16().count() as i64)
            } else {
                continue;
            };
            let better = match best {
                None => true,
                Some((_, best_kind, best_specificity)) => {
                    specificity > best_specificity || (specificity == best_specificity && kind < best_kind)
                }
            };
            if better {
                best = Some((i, kind, specificity));
            }
        }
        best.map(|(i, kind, _)| (i, kind))
    };
    let preferred = candidates
        .iter()
        .filter_map(|(lang, text)| rank(lang).map(|r| (r, text)))
        .min_by(|(ra, a), (rb, b)| ra.cmp(rb).then_with(|| str_cmp(a, b)));
    if let Some((_, text)) = preferred {
        return Some(text.clone());
    }
    candidates.iter().map(|(_, text)| text).min_by(|a, b| str_cmp(a, b)).cloned()
}

/// What a pretty report names an entity by. `short` is the short form the
/// command line's prefixes give an IRI, `<IRI>` failing any, and `label` the
/// entity's label, the short form standing in for one it lacks. The name is
/// the short form, an OBO PURL's id written as a CURIE, in angle brackets,
/// followed by the label in square brackets unless the label repeats it or
/// the entity's own plain rendering.
fn pretty_name(iri: &str, short: &str, label: Option<&str>) -> String {
    const OBO: &str = "http://purl.obolibrary.org/obo/";
    let main = match short.strip_prefix("obo:").or_else(|| short.strip_prefix(OBO)) {
        Some(id) => match id.rfind('_') {
            Some(i) => format!("{}:{}", &id[..i], &id[i + 1..]),
            None => id.to_string(),
        },
        None => short.to_string(),
    };
    let name = if main.starts_with('<') && main.ends_with('>') { main.clone() } else { format!("<{main}>") };
    let label = label.unwrap_or(short);
    let plain = crate::io::manchester_write::ShortForms::new(&[]).prefixed_or_quoted(iri);
    if label == plain || label == main {
        name
    } else {
        format!("{name}[{label}]")
    }
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    // diff loads directly (not via take_or_load), so activate the shared
    // `--strict`/`-v` options before parsing.
    args.common.activate();
    // A chained `diff` takes its LEFT side from the pipeline, then `--input`, then
    // `--left`/`--left-iri`.
    let left = if args.left.is_none() && args.left_iri.is_none() {
        // Cloned, not taken: `diff` leaves the chained ontology in place for
        // whatever follows it, and `step` returns `piped` unchanged below.
        match (&piped, &args.input) {
            (Some(m), _) => Side { model: m.clone(), imports: None, source: Source::Piped },
            (None, Some(p)) => {
                load_side(Some(p), None, "left", args.left_catalog.as_deref(), &args.common)?
            }
            (None, None) => load_side(None, None, "left", None, &args.common)?,
        }
    } else {
        load_side(
            args.left.as_deref(),
            args.left_iri.as_deref(),
            "left",
            args.left_catalog.as_deref(),
            &args.common,
        )?
    };
    let right = load_side(
        args.right.as_deref(),
        args.right_iri.as_deref(),
        "right",
        args.right_catalog.as_deref(),
        &args.common,
    )?;

    let d = diff::diff(&left.model, &right.model);
    // Two ontologies are identical only when their IDs match AND neither side has
    // unique content, so an ID/version change alone is a difference. The ontology
    // ID is kept out of the component set (version stamps must not read as content
    // changes elsewhere), so it is compared separately here.
    let id_differs = diff::ontology_id_change(&left.model, &right.model).is_some();
    // The left side is read first, so the right's IDs follow the left's.
    let ids = id_differs.then(|| (left.id_line(0), right.id_line(left.ids_minted())));

    let use_labels = args.labels.unwrap_or(false);
    let mut fmt = args.format.to_lowercase();
    // `--labels true` on the DEFAULT `plain` format upgrades to `pretty`: asking
    // for labels asks for the pretty layout. A repo that passes `--labels true`
    // with no `-f` — EFO's committed `reports/robot_diff.txt` — therefore holds a
    // PRETTY report, not a plain one.
    if use_labels && fmt == "plain" {
        fmt = "pretty".to_string();
    }
    let langs = parse_label_langs(args.label_langs_priority.as_deref());
    if !langs.is_empty() && fmt != "pretty" && crate::progress::verbosity() >= 1 {
        crate::cmd::reason::log_warn(
            "org.obolibrary.robot.DiffOperation",
            &format!(
                "The --label-langs-priority option only affects the 'pretty' diff format; it is ignored for format '{fmt}'."
            ),
        );
    }

    let report = if d.is_empty() && !id_differs {
        // The whole report when nothing differs, whatever the format.
        "Ontologies are identical\n".to_string()
    } else {
        match fmt.as_str() {
            "plain" => render_basic(&d, ids.as_ref(), diff::describe),
            "pretty" => {
                // The command line's prefixes give each IRI its short form.
                let mut context = crate::context::Context::default();
                args.common.bind(&mut context)?;
                let prefixes = crate::io::manchester_write::ShortForms::new(&context.entries());
                let labels: HashMap<String, String> = if langs.is_empty() {
                    label_map(&left, &right, |iri| prefixes.prefixed_or_quoted(iri))
                } else {
                    literal_labels(&left, &right)
                        .into_iter()
                        .filter_map(|(subj, candidates)| preferred_label(&candidates, &langs).map(|l| (subj, l)))
                        .collect()
                };
                let names = |iri: &str| {
                    pretty_name(iri, &prefixes.prefixed_or_quoted(iri), labels.get(iri).map(String::as_str))
                };
                render_basic(&d, ids.as_ref(), |ac| {
                    crate::io::owlfunc::render_component_named(ac, &names)
                })
            }
            "markdown" | "html" => render_markdown(args, &left, &right, &d)?,
            other => anyhow::bail!("Unknown diff format: {other}"),
        }
    };

    match &args.output {
        Some(path) => std::fs::write(path, report)?,
        None => print!("{report}"),
    }
    Ok(piped)
}

// ---------------------------------------------------------------------------
// plain / pretty
// ---------------------------------------------------------------------------

/// Two counted sections, each line prefixed and the prefixed lines SORTED, with
/// one blank line between them. `render` writes each axiom.
///
/// ```text
/// 1 axioms in left ontology but not in right ontology:
/// - OntologyID(OntologyIRI(<…/simple.owl>) VersionIRI(<null>))
///
/// 2 axioms in right ontology but not in left ontology:
/// + AnnotationAssertion(rdfs:label <…#test1> "TEST #1"^^xsd:string)
/// + OntologyID(OntologyIRI(<…/simple1.owl>) VersionIRI(<null>))
/// ```
///
/// Both headers are emitted unconditionally, even at zero: a report with nothing
/// removed still opens with
/// `0 axioms in left ontology but not in right ontology:`. A section lists, and
/// counts, each rendering once: two axioms written alike are one line.
fn render_basic(
    d: &diff::Diff,
    ids: Option<&(String, String)>,
    render: impl Fn(&AnnotatedComponent<RcStr>) -> String,
) -> String {
    let mut removed: std::collections::HashSet<String> = d.only_left.iter().map(&render).collect();
    let mut added: std::collections::HashSet<String> = d.only_right.iter().map(&render).collect();
    // When the IDs differ, each side's ID line joins its own set and counts
    // toward that section's total.
    if let Some((left, right)) = ids {
        removed.insert(left.clone());
        added.insert(right.clone());
    }
    let mut out = String::new();
    for (count, header, sign, lines) in [
        (removed.len(), "left ontology but not in right ontology", '-', &removed),
        (added.len(), "right ontology but not in left ontology", '+', &added),
    ] {
        if sign == '+' {
            out.push('\n');
        }
        out.push_str(&format!("{count} axioms in {header}:\n"));
        // A multi-line literal must not break the one-axiom-per-line contract.
        let mut lines: Vec<String> = lines.iter().map(|l| format!("{sign} {}", l.replace('\n', "\\n"))).collect();
        lines.sort_by(|a, b| crate::io::natural_order::str_cmp(a, b));
        lines.dedup();
        for line in &lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// markdown — one frame per axiom subject
// ---------------------------------------------------------------------------

/// The frame a change is listed under.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Grouping {
    /// The ontology's import declarations.
    Imports,
    /// The ontology's own annotations.
    Annotations,
    /// An axiom whose subject is an entity or an IRI.
    Iri(String),
    /// An axiom whose subject is an anonymous class expression.
    Gci,
    /// A rule.
    Rules,
    /// An axiom whose subject is anything else.
    Other(Subject),
}

/// A subject that is neither an entity, an IRI, a class expression nor a rule.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Subject {
    /// An anonymous individual.
    Individual(horned_owl::model::Individual<RcStr>),
    /// An inverse property.
    Property(horned_owl::model::ObjectPropertyExpression<RcStr>),
    /// A data range other than a datatype.
    DataRange(horned_owl::model::DataRange<RcStr>),
}

/// Render the whole document: a header block for each side, then one frame per
/// grouping.
///
/// The whitespace is load-bearing, because repos commit these reports and any
/// drift rewrites the whole file: a trailing space follows every rendered object,
/// every axiom bullet is followed by a blank line even when it carries no
/// annotations, and two blank lines separate frames.
fn render_markdown(args: &Args, left: &Side, right: &Side, d: &diff::Diff) -> anyhow::Result<String> {
    // The markdown renderer ALWAYS resolves labels, over every ontology both
    // sides read, independent of `--labels`.
    let labels = label_map(left, right, crate::owlapi_hash::iri_short_form);
    let order = crate::io::natural_order::NaturalOrder::default();
    let links = Renderer { labels: &labels, order, dialect: Dialect::Diff, links: true };
    let names = Renderer { labels: &labels, order, dialect: Dialect::Diff, links: false };

    let (liri, lver) = diff::ontology_id(&left.model);
    let (riri, rver) = diff::ontology_id(&right.model);
    let mut out = String::new();
    out.push_str("# Ontology comparison\n\n");
    out.push_str("## Left\n");
    out.push_str(&format!("- Ontology IRI: {}\n", optional_iri(liri.as_deref())));
    out.push_str(&format!("- Version IRI: {}\n", optional_iri(lver.as_deref())));
    out.push_str(&format!(
        "- Loaded from: `{}`\n\n",
        document_iri(args.left.as_deref(), args.left_iri.as_deref())
    ));
    out.push_str("## Right\n");
    out.push_str(&format!("- Ontology IRI: {}\n", optional_iri(riri.as_deref())));
    out.push_str(&format!("- Version IRI: {}\n", optional_iri(rver.as_deref())));
    out.push_str(&format!(
        "- Loaded from: `{}`\n",
        document_iri(args.right.as_deref(), args.right_iri.as_deref())
    ));

    // A rule with an empty head has no subject, and so no frame to list it in.
    let headless = |ac: &&AnnotatedComponent<RcStr>| matches!(&ac.component, Component::Rule(r) if r.head.is_empty());
    if d.only_left.iter().chain(&d.only_right).any(|ac| headless(&ac)) {
        anyhow::bail!("diff: a rule with an empty head has no subject to list it under in a markdown report");
    }

    // Bucket every change, keeping removed and added apart.
    type Changes<'a> = (Vec<&'a AnnotatedComponent<RcStr>>, Vec<&'a AnnotatedComponent<RcStr>>);
    let mut groups: BTreeMap<Grouping, Changes> = BTreeMap::new();
    for ac in &d.only_left {
        groups.entry(grouping(&ac.component, order)).or_default().0.push(ac);
    }
    for ac in &d.only_right {
        groups.entry(grouping(&ac.component, order)).or_default().1.push(ac);
    }
    // A report opens with an imports frame and an ontology annotations frame
    // whether or not either changed.
    groups.entry(Grouping::Imports).or_default();
    groups.entry(Grouping::Annotations).or_default();

    let header = |g: &Grouping| -> String {
        match g {
            Grouping::Imports => "Ontology imports".to_string(),
            Grouping::Annotations => "Ontology annotations".to_string(),
            Grouping::Iri(iri) => names.link(iri),
            Grouping::Gci => "GCIs".to_string(),
            Grouping::Rules => "Rules".to_string(),
            Grouping::Other(Subject::Individual(i)) => {
                let mut s = String::new();
                names.ind(i, &mut s);
                s
            }
            Grouping::Other(Subject::Property(p)) => {
                let mut s = String::new();
                names.ope(p, &mut s);
                s
            }
            Grouping::Other(Subject::DataRange(r)) => {
                let mut s = String::new();
                names.data_range(r, &mut s);
                s
            }
        }
    };

    // Imports first, then ontology annotations, then every other frame sorted
    // by its header; frames with the same header keep the order a hash map
    // keyed by their groupings iterates ([`hash_map_order`]).
    let rest: Vec<&Grouping> = groups
        .keys()
        .filter(|g| !matches!(g, Grouping::Imports | Grouping::Annotations))
        .collect();
    let hashes: Vec<i32> = rest.iter().map(|g| grouping_hash(g, order)).collect();
    let mut rest: Vec<(&Grouping, Utf16Order)> = hash_map_order(&hashes)
        .into_iter()
        .map(|i| (rest[i], Utf16Order(header(rest[i]))))
        .collect();
    rest.sort_by(|a, b| a.1.cmp(&b.1));
    let ordered = [&Grouping::Imports, &Grouping::Annotations].into_iter().chain(rest.into_iter().map(|(g, _)| g));

    for g in ordered {
        let (removed, added) = &groups[g];
        let list = |name: &str, items: &[&AnnotatedComponent<RcStr>]| -> String {
            if items.is_empty() {
                return String::new();
            }
            let mut items = items.to_vec();
            items.sort_by_cached_key(|ac| Utf16Order(sort_key(ac)));
            let rendered: Vec<String> = items.iter().map(|ac| markdown_item(&links, ac)).collect();
            format!("#### {name}\n{}", rendered.join("\n"))
        };
        let iri = match g {
            Grouping::Iri(iri) => format!("`{iri}`"),
            _ => String::new(),
        };
        // A blank line, then the frame itself: a `### <header> <iri>` line, the
        // removed list and the added list.
        out.push('\n');
        out.push_str(&format!("### {} {iri}\n{}\n{}\n", header(g), list("Removed", removed), list("Added", added)));
    }
    Ok(out)
}

/// A string ordered by UTF-16 code unit.
#[derive(PartialEq, Eq)]
struct Utf16Order(String);

impl PartialOrd for Utf16Order {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Utf16Order {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        crate::io::natural_order::str_cmp(&self.0, &other.0)
    }
}

/// A backticked IRI, or `*None*` when the side carries none.
fn optional_iri(iri: Option<&str>) -> String {
    iri.map(|i| format!("`{i}`")).unwrap_or_else(|| "*None*".to_string())
}

/// The within-frame sort key: declarations first (`1-`), everything else after
/// (`2-`).
fn sort_key(ac: &AnnotatedComponent<RcStr>) -> String {
    let is_declaration = matches!(
        ac.component,
        Component::DeclareClass(_)
            | Component::DeclareObjectProperty(_)
            | Component::DeclareDataProperty(_)
            | Component::DeclareAnnotationProperty(_)
            | Component::DeclareNamedIndividual(_)
            | Component::DeclareDatatype(_)
    );
    // Imports and ontology annotations key on the item itself, with no prefix.
    match ac.component {
        Component::Import(_) | Component::OntologyAnnotation(_) => diff::describe(ac),
        _ if is_declaration => format!("1-{}", diff::describe(ac)),
        _ => format!("2-{}", diff::describe(ac)),
    }
}

/// One change as a bullet. Every bullet but an import's ends in a newline,
/// which leaves a blank line after it in the list, and is followed by a nested
/// bullet for each of its annotations ([`annotated_item`]).
fn markdown_item(r: &Renderer, ac: &AnnotatedComponent<RcStr>) -> String {
    let body = r.axiom(&ac.component);
    match &ac.component {
        Component::Import(_) => format!("- {body} "),
        // An ontology annotation's own annotations are on the annotation.
        Component::OntologyAnnotation(oa) => annotated_item(r, 0, &body, &oa.0.ann),
        _ => annotated_item(r, 0, &body, &ac.ann),
    }
}

/// A bullet `level` deep for `body`, followed by a bullet a level deeper for
/// each of its annotations, each of those followed by its own annotations in
/// turn. The annotations' bullets come in the order a hash set of them
/// iterates in ([`hash_set_order`]), filled in the annotations' natural order.
fn annotated_item(
    r: &Renderer,
    level: usize,
    body: &str,
    anns: &std::collections::BTreeSet<horned_owl::model::Annotation<RcStr>>,
) -> String {
    let nested: Vec<String> =
        r.order.sorted_annotations(anns).into_iter().map(|a| annotated_item(r, level + 1, &r.annotation(a), &a.ann)).collect();
    format!("{}- {body} \n{}", "  ".repeat(level), hash_set_order(nested).join("\n"))
}

/// The order a Scala mutable hash set filled with `items`, in turn, iterates
/// in, each distinct string once: by the bucket its improved hash falls in, in
/// a table of 16 buckets doubled whenever the set is about to fill three
/// quarters of it, then by the improved hash, then by when it was added.
fn hash_set_order(items: Vec<String>) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::with_capacity(items.len());
    for s in items {
        if !distinct.contains(&s) {
            distinct.push(s);
        }
    }
    let mut buckets = 16;
    for count in 0..distinct.len() {
        if count + 1 >= buckets * 3 / 4 {
            buckets *= 2;
        }
    }
    let mut keyed: Vec<(usize, i32, usize, String)> = distinct
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let h = crate::owlapi_hash::java_string_hash(&s) as u32;
            let improved = h ^ (h >> 16);
            (improved as usize & (buckets - 1), improved as i32, i, s)
        })
        .collect();
    keyed.sort();
    keyed.into_iter().map(|(.., s)| s).collect()
}

/// The frame an axiom is listed under, named by its subject: a sub-class, a
/// sub-property, a key's class, a property's domain, range or characteristic,
/// an assertion's individual and an annotation's subject; for a class
/// equivalence or disjointness its first named member, else its first member,
/// and for any other set axiom or an inverse pair its first member, in the
/// natural order; a property chain's super-property; a datatype definition's
/// data range. A subject that is a class expression lists the axiom as a GCI.
fn grouping(c: &Component<RcStr>, order: crate::io::natural_order::NaturalOrder) -> Grouping {
    use crate::io::natural_order::{iri_cmp, sorted_set};
    use horned_owl::model::{
        AnnotationSubject, ClassExpression as CE, DataRange as DR, Individual, ObjectPropertyExpression as OPE,
        SubObjectPropertyExpression as SOPE,
    };
    use Component as C;
    let iri = |iri: &str| Grouping::Iri(iri.to_string());
    let ce = |x: &CE<RcStr>| match x {
        CE::Class(c) => iri(c.0.as_ref()),
        _ => Grouping::Gci,
    };
    let ope = |o: &OPE<RcStr>| match o {
        OPE::ObjectProperty(p) => iri(p.0.as_ref()),
        OPE::InverseObjectProperty(_) => Grouping::Other(Subject::Property(o.clone())),
    };
    let ind = |i: &Individual<RcStr>| match i {
        Individual::Named(n) => iri(n.0.as_ref()),
        Individual::Anonymous(_) => Grouping::Other(Subject::Individual(i.clone())),
    };
    let classes = |v: &[CE<RcStr>]| {
        let sorted = sorted_set(v, |a, b| order.ce(a, b));
        let named = sorted.iter().find(|x| matches!(x, CE::Class(_))).or(sorted.first());
        named.map_or(Grouping::Gci, |x| ce(x))
    };
    let opes = |v: &[OPE<RcStr>]| sorted_set(v, |a, b| order.ope(a, b)).first().map_or(Grouping::Gci, |o| ope(o));
    let inds =
        |v: &[Individual<RcStr>]| sorted_set(v, |a, b| order.individual(a, b)).first().map_or(Grouping::Gci, |i| ind(i));
    let dps = |v: &[horned_owl::model::DataProperty<RcStr>]| {
        sorted_set(v, |a, b| iri_cmp(a.0.as_ref(), b.0.as_ref())).first().map_or(Grouping::Gci, |p| iri(p.0.as_ref()))
    };
    match c {
        C::OntologyID(_) | C::DocIRI(_) => unreachable!("a diff compares no ontology ID or document IRI"),
        C::Import(_) => Grouping::Imports,
        C::OntologyAnnotation(_) => Grouping::Annotations,
        C::DeclareClass(x) => iri(x.0 .0.as_ref()),
        C::DeclareObjectProperty(x) => iri(x.0 .0.as_ref()),
        C::DeclareAnnotationProperty(x) => iri(x.0 .0.as_ref()),
        C::DeclareDataProperty(x) => iri(x.0 .0.as_ref()),
        C::DeclareNamedIndividual(x) => iri(x.0 .0.as_ref()),
        C::DeclareDatatype(x) => iri(x.0 .0.as_ref()),
        C::SubClassOf(x) => ce(&x.sub),
        C::EquivalentClasses(x) => classes(&x.0),
        C::DisjointClasses(x) => classes(&x.0),
        C::DisjointUnion(x) => iri(x.0 .0.as_ref()),
        C::SubObjectPropertyOf(x) => match &x.sub {
            SOPE::ObjectPropertyExpression(o) => ope(o),
            SOPE::ObjectPropertyChain(_) => ope(&x.sup),
        },
        C::EquivalentObjectProperties(x) => opes(&x.0),
        C::DisjointObjectProperties(x) => opes(&x.0),
        C::InverseObjectProperties(x) => ope(if order.ope(&x.1, &x.0).is_lt() { &x.1 } else { &x.0 }),
        C::ObjectPropertyDomain(x) => ope(&x.ope),
        C::ObjectPropertyRange(x) => ope(&x.ope),
        C::FunctionalObjectProperty(x) => ope(&x.0),
        C::InverseFunctionalObjectProperty(x) => ope(&x.0),
        C::ReflexiveObjectProperty(x) => ope(&x.0),
        C::IrreflexiveObjectProperty(x) => ope(&x.0),
        C::SymmetricObjectProperty(x) => ope(&x.0),
        C::AsymmetricObjectProperty(x) => ope(&x.0),
        C::TransitiveObjectProperty(x) => ope(&x.0),
        C::SubDataPropertyOf(x) => iri(x.sub.0.as_ref()),
        C::EquivalentDataProperties(x) => dps(&x.0),
        C::DisjointDataProperties(x) => dps(&x.0),
        C::DataPropertyDomain(x) => iri(x.dp.0.as_ref()),
        C::DataPropertyRange(x) => iri(x.dp.0.as_ref()),
        C::FunctionalDataProperty(x) => iri(x.0 .0.as_ref()),
        C::DatatypeDefinition(x) => match &x.range {
            DR::Datatype(t) => iri(t.0.as_ref()),
            r => Grouping::Other(Subject::DataRange(r.clone())),
        },
        C::HasKey(x) => ce(&x.ce),
        C::SameIndividual(x) => inds(&x.0),
        C::DifferentIndividuals(x) => inds(&x.0),
        C::ClassAssertion(x) => ind(&x.i),
        C::ObjectPropertyAssertion(x) => ind(&x.from),
        C::NegativeObjectPropertyAssertion(x) => ind(&x.from),
        C::DataPropertyAssertion(x) => ind(&x.from),
        C::NegativeDataPropertyAssertion(x) => ind(&x.from),
        C::AnnotationAssertion(x) => match &x.subject {
            AnnotationSubject::IRI(i) => iri(i.as_ref()),
            AnnotationSubject::AnonymousIndividual(a) => {
                Grouping::Other(Subject::Individual(Individual::Anonymous(a.clone())))
            }
        },
        C::SubAnnotationPropertyOf(x) => iri(x.sub.0.as_ref()),
        C::AnnotationPropertyDomain(x) => iri(x.ap.0.as_ref()),
        C::AnnotationPropertyRange(x) => iri(x.ap.0.as_ref()),
        C::Rule(_) => Grouping::Rules,
    }
}

/// A grouping's hash as the report's frame map keys it: an IRI or another
/// subject hashed with the name of its kind of grouping, by Scala's product
/// hash; GCIs and rules by fixed values.
fn grouping_hash(g: &Grouping, order: crate::io::natural_order::NaturalOrder) -> i32 {
    use crate::owlapi_hash::{data_range_hash, individual_hash, iri_hash, java_string_hash, ope_hash};
    let product = |name: &str, field: i32| -> i32 {
        let mut h = scala_mix(0xcafe_babe_u32 as i32, java_string_hash(name));
        h = scala_mix(h, field);
        scala_finalize(h, 1)
    };
    match g {
        Grouping::Iri(iri) => product("IRIGrouping", iri_hash(iri)),
        Grouping::Gci => java_string_hash("GCIGrouping"),
        Grouping::Rules => java_string_hash("RuleGrouping"),
        Grouping::Other(Subject::Individual(i)) => product("NonIRIGrouping", individual_hash(i)),
        Grouping::Other(Subject::Property(p)) => product("NonIRIGrouping", ope_hash(p)),
        Grouping::Other(Subject::DataRange(r)) => product("NonIRIGrouping", data_range_hash(r, order)),
        Grouping::Imports => java_string_hash("OntologyImportGrouping"),
        Grouping::Annotations => java_string_hash("OntologyAnnotationGrouping"),
    }
}

/// One round of MurmurHash3 as Scala's hashing mixes a value in.
fn scala_mix(hash: i32, data: i32) -> i32 {
    let mut k = data.wrapping_mul(0xcc9e_2d51_u32 as i32);
    k = k.rotate_left(15);
    k = k.wrapping_mul(0x1b87_3593);
    let h = (hash ^ k).rotate_left(13);
    h.wrapping_mul(5).wrapping_add(0xe654_6b64_u32 as i32)
}

/// MurmurHash3's finalization, over `length` mixed values.
fn scala_finalize(hash: i32, length: i32) -> i32 {
    let mut h = (hash ^ length) as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h as i32
}

/// The order a Scala immutable hash map iterates keys with these hashes in: a
/// trie over the improved hash five bits at a time, lowest bits first, each
/// node listing the keys that end at it, by their five bits, before its
/// sub-tries, by theirs. Keys whose hashes are equal stay in the order given.
fn hash_map_order(hashes: &[i32]) -> Vec<usize> {
    fn improve(h: i32) -> u32 {
        let mut h = h as u32;
        h = h.wrapping_add(!(h << 9));
        h ^= h >> 14;
        h = h.wrapping_add(h << 4);
        h ^ (h >> 10)
    }
    fn walk(keys: Vec<(u32, usize)>, shift: u32, out: &mut Vec<usize>) {
        if shift >= 32 {
            out.extend(keys.into_iter().map(|(_, i)| i));
            return;
        }
        let mut by_bits: BTreeMap<u32, Vec<(u32, usize)>> = BTreeMap::new();
        for k in keys {
            by_bits.entry((k.0 >> shift) & 31).or_default().push(k);
        }
        for v in by_bits.values() {
            if let [(_, i)] = v.as_slice() {
                out.push(*i);
            }
        }
        for v in by_bits.into_values() {
            if v.len() > 1 {
                walk(v, shift + 5, out);
            }
        }
    }
    let mut out = Vec::with_capacity(hashes.len());
    walk(hashes.iter().enumerate().map(|(i, &h)| (improve(h), i)).collect(), 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::{Build, MutableOntology};

    fn model_with(f: impl Fn(&mut crate::model::Model, &Build<RcStr>)) -> crate::model::Model {
        let mut m = crate::model::Model::new();
        let b: Build<RcStr> = Build::new();
        f(&mut m, &b);
        m
    }

    /// The plain report shape: both headers, always, even at zero; `- ` on the
    /// left and `+ ` on the right; one blank line between the sections.
    #[test]
    fn plain_matches_basic_diff_renderer() {
        let left = model_with(|_, _| {});
        let right = model_with(|m, b| {
            m.ont.declare(b.class("http://x/B"));
            m.ont.declare(b.class("http://x/A"));
        });
        let d = diff::diff(&left, &right);
        let out = render_basic(&d, None, diff::describe);
        assert_eq!(
            out,
            "0 axioms in left ontology but not in right ontology:\n\
             \n\
             2 axioms in right ontology but not in left ontology:\n\
             + Declaration(Class(<http://x/A>))\n\
             + Declaration(Class(<http://x/B>))\n"
        );
    }

    /// When the ontology IDs differ, each side's ID line joins that side's list
    /// and counts toward the total.
    #[test]
    fn plain_counts_the_ontology_id_change() {
        let left = model_with(|m, b| {
            m.ont.insert(horned_owl::model::OntologyID {
                iri: Some(b.iri("http://x/left.owl")),
                viri: None,
            });
        });
        let right = model_with(|m, b| {
            m.ont.insert(horned_owl::model::OntologyID {
                iri: Some(b.iri("http://x/right.owl")),
                viri: None,
            });
        });
        let d = diff::diff(&left, &right);
        let side = |model| Side { model, imports: None, source: Source::Piped };
        let ids = (side(left).id_line(0), side(right).id_line(3));
        let out = render_basic(&d, Some(&ids), diff::describe);
        assert!(out.starts_with("1 axioms in left ontology but not in right ontology:\n"), "{out}");
        assert!(
            out.contains("- OntologyID(OntologyIRI(<http://x/left.owl>) VersionIRI(<null>))\n"),
            "{out}"
        );
        assert!(
            out.contains("+ OntologyID(OntologyIRI(<http://x/right.owl>) VersionIRI(<null>))\n"),
            "{out}"
        );
    }

    /// The document skeleton of CL's / OBA's / UBERON's committed
    /// `reports/release-diff.md`, down to the empty imports frame and the blank
    /// line every bullet leaves behind.
    #[test]
    fn markdown_matches_the_grouped_renderer_shape() {
        let left = model_with(|_, _| {});
        let right = model_with(|m, b| {
            m.ont.declare(b.class("http://x/A"));
        });
        let d = diff::diff(&left, &right);
        let args = Args {
            left: None,
            right: None,
            input: None,
            left_iri: Some("http://x/left.owl".into()),
            right_iri: Some("http://x/right.owl".into()),
            left_catalog: None,
            right_catalog: None,
            output: None,
            format: "markdown".into(),
            label_langs_priority: None,
            labels: None,
            common: Default::default(),
        };
        let side = |model| Side { model, imports: None, source: Source::Piped };
        let (left, right) = (side(left), side(right));
        let out = render_markdown(&args, &left, &right, &d).unwrap();
        assert_eq!(
            out,
            "# Ontology comparison\n\
             \n\
             ## Left\n\
             - Ontology IRI: *None*\n\
             - Version IRI: *None*\n\
             - Loaded from: `http://x/left.owl`\n\
             \n\
             ## Right\n\
             - Ontology IRI: *None*\n\
             - Version IRI: *None*\n\
             - Loaded from: `http://x/right.owl`\n\
             \n\
             ### Ontology imports \n\
             \n\
             \n\
             \n\
             ### Ontology annotations \n\
             \n\
             \n\
             \n\
             ### A `http://x/A`\n\
             \n\
             #### Added\n\
             - Class: [A](http://x/A) \n\
             \n"
        );
    }

    /// Frames are keyed by the axiom's SUBJECT, so both a declaration and a
    /// subClassOf on the same term land in one frame; an inverse property is a
    /// subject of its own.
    #[test]
    fn grouping_follows_the_axiom_subject() {
        use horned_owl::model::ObjectPropertyExpression as OPE;
        let b: Build<RcStr> = Build::new();
        let order = crate::io::natural_order::NaturalOrder::default();
        let decl = Component::DeclareClass(horned_owl::model::DeclareClass(b.class("http://x/A")));
        let sub = Component::SubClassOf(horned_owl::model::SubClassOf {
            sub: b.class("http://x/A").into(),
            sup: b.class("http://x/B").into(),
        });
        assert_eq!(grouping(&decl, order), Grouping::Iri("http://x/A".into()));
        assert_eq!(grouping(&sub, order), Grouping::Iri("http://x/A".into()));
        assert_eq!(
            grouping(&Component::Import(horned_owl::model::Import(b.iri("http://x/i.owl"))), order),
            Grouping::Imports
        );
        let inverse = OPE::InverseObjectProperty(b.object_property("http://x/p"));
        let functional = Component::FunctionalObjectProperty(horned_owl::model::FunctionalObjectProperty(inverse.clone()));
        assert_eq!(grouping(&functional, order), Grouping::Other(Subject::Property(inverse)));
    }

    /// An object's annotation bullets come in the order a Scala mutable hash
    /// set of them iterates in: as the set gives these strings, filled in this
    /// order, and with its table doubled past eleven of them.
    #[test]
    fn annotation_bullets_follow_a_hash_set() {
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            hash_set_order(strings(&["a", "b", "c", "b"])),
            strings(&["a", "b", "c"])
        );
        let many: Vec<String> = (0..13).map(|i| format!("  - [comment](http://x/c) \"{i}\" ")).collect();
        let order: Vec<usize> =
            hash_set_order(many.clone()).iter().map(|s| many.iter().position(|m| m == s).unwrap()).collect();
        assert_eq!(order, [11, 10, 12, 0, 5, 6, 7, 8, 1, 2, 3, 4, 9]);
    }

    /// Frames with the same header come in the order the report's frame map
    /// iterates their groupings in: hashes and order as a Scala 2.13 immutable
    /// hash map gives them for these keys.
    #[test]
    fn tied_frames_follow_the_frame_map() {
        let order = crate::io::natural_order::NaturalOrder::default();
        let keys = [
            Grouping::Iri("http://example.org/md#A".into()),
            Grouping::Iri("http://example.org/md#B".into()),
            Grouping::Iri("http://example.org/md#C".into()),
            Grouping::Gci,
            Grouping::Rules,
        ];
        let hashes: Vec<i32> = keys.iter().map(|g| grouping_hash(g, order)).collect();
        assert_eq!(hashes, [-972444333, -1471248547, -302999237, -1540172016, 863055935]);
        assert_eq!(hash_map_order(&hashes), [0, 2, 3, 4, 1]);
    }
}
