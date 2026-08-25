//! `diff` — compare two ontologies and report added/removed components.
//!
//! Two report shapes are produced: `-f plain`/`-f pretty` render two flat counted
//! sections, `-f markdown` renders one frame per axiom subject. Both matter
//! because ontology repos COMMIT the output — OBA, CL and UBERON keep
//! `reports/release-diff.md` under version control, and EFO keeps
//! `reports/robot_diff.txt` plus `qc/diff_*_latest_release.txt` — so the layout
//! has to stay fixed, or every release diff churns on formatting alone.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use clap::Args as ClapArgs;
use horned_owl::model::{AnnotatedComponent, Component, RcStr};

use crate::diff;
use crate::io;

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
    /// Catalog for resolving the left ontology's imports. Accepted for
    /// compatibility.
    #[arg(long = "left-catalog")]
    pub left_catalog: Option<PathBuf>,
    /// Catalog for resolving the right ontology's imports. Accepted for
    /// compatibility.
    #[arg(long = "right-catalog")]
    pub right_catalog: Option<PathBuf>,
    /// Output file for the diff report (defaults to stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Diff output format: plain (default), pretty, or markdown. (html is
    /// accepted but rendered as markdown.)
    #[arg(short = 'f', long = "format", default_value = "plain")]
    pub format: String,
    /// The ontology to diff as the LEFT side when `--left`/`--left-iri` is absent.
    /// `diff` is chainable — `om merge -i a.owl diff --right b.owl` compares the
    /// merged ontology against `b.owl` — so the piped or `--input` ontology stands
    /// in for the left. Accepted and unused when `--left` is given, which is how a
    /// release diff invokes it.
    #[arg(short = 'i', long)]
    pub input: Option<PathBuf>,
    /// Append rdfs:label after entity IRIs in the report.
    #[arg(long = "labels", num_args = 1, default_missing_value = "true")]
    pub labels: Option<bool>,
    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

/// Load one side of the diff from either a file (`--left`/`--right`) or an IRI
/// (`--left-iri`/`--right-iri`). Exactly one of the two must be given.
fn load_side(
    path: Option<&std::path::Path>,
    iri: Option<&str>,
    which: &str,
) -> anyhow::Result<crate::model::Model> {
    match (path, iri) {
        (Some(p), None) => io::load(p),
        (None, Some(i)) => io::load_iri(i, None),
        (Some(_), Some(_)) => {
            anyhow::bail!("diff: provide only one of --{which} or --{which}-iri")
        }
        (None, None) => anyhow::bail!("diff: --{which} or --{which}-iri is required"),
    }
}

/// The document IRI reported as `Loaded from:`. For a file it is `file:` plus
/// the ABSOLUTE path with a single slash, not three (`` `file:/work/src/ontology/cl.owl` ``),
/// which is the spelling committed diff reports carry; a side loaded from an IRI
/// reports that IRI unchanged.
fn document_iri(path: Option<&std::path::Path>, iri: Option<&str>) -> String {
    match (path, iri) {
        (Some(p), _) => {
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            format!("file:{}", abs.display())
        }
        (None, Some(i)) => i.to_string(),
        (None, None) => String::new(),
    }
}

/// Build an IRI -> label map for label annotation in the report.
fn label_map(model: &crate::model::Model) -> anyhow::Result<HashMap<String, String>> {
    // The same label set the rest of the build names entities by — an entity with
    // competing labels must not be called one thing in a banner and another in a
    // diff report.
    Ok(crate::cmd::rdfs_labels(model))
}

/// Append known labels after IRIs appearing in `text`.
fn annotate_labels(text: &str, labels: &HashMap<String, String>) -> String {
    let mut out = text.to_string();
    for (iri, label) in labels {
        if out.contains(iri) {
            out = out.replace(iri, &format!("{iri} \"{label}\""));
        }
    }
    out
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
    let mut left = if args.left.is_none() && args.left_iri.is_none() {
        // Cloned, not taken: `diff` leaves the chained ontology in place for
        // whatever follows it, and `step` returns `piped` unchanged below.
        match (&piped, &args.input) {
            (Some(m), _) => m.clone(),
            (None, Some(p)) => io::load(p)?,
            (None, None) => load_side(None, None, "left")?,
        }
    } else {
        load_side(args.left.as_deref(), args.left_iri.as_deref(), "left")?
    };
    let mut right = load_side(args.right.as_deref(), args.right_iri.as_deref(), "right")?;

    // --left-catalog / --right-catalog: resolve each side's import closure through
    // its catalog before comparing, so the diff is over the loaded closures.
    if let Some(cat) = &args.left_catalog {
        crate::cmd::merge_import_closure(&mut left, cat, args.left.as_deref())?;
    }
    if let Some(cat) = &args.right_catalog {
        crate::cmd::merge_import_closure(&mut right, cat, args.right.as_deref())?;
    }

    let d = diff::diff(&left, &right);
    // Two ontologies are identical only when their IDs match AND neither side has
    // unique content, so an ID/version change alone is a difference. The ontology
    // ID is kept out of the component set (version stamps must not read as content
    // changes elsewhere), so it is compared separately here.
    let id_differs = diff::ontology_id_change(&left, &right).is_some();

    let use_labels = args.labels.unwrap_or(false);
    let mut fmt = args.format.to_ascii_lowercase();
    // `--labels true` on the DEFAULT `plain` format upgrades to `pretty`: asking
    // for labels asks for the pretty layout. A repo that passes `--labels true`
    // with no `-f` — EFO's committed `reports/robot_diff.txt` — therefore holds a
    // PRETTY report, not a plain one.
    if use_labels && fmt == "plain" {
        fmt = "pretty".to_string();
    }

    let mut report = if d.is_empty() && !id_differs {
        // The whole report when nothing differs, in either format.
        "Ontologies are identical\n".to_string()
    } else if matches!(fmt.as_str(), "markdown" | "html") {
        render_markdown(args, &left, &right, &d)?
    } else {
        render_basic(&left, &right, &d, id_differs)
    };

    // --labels: append labels (from both sides) after entity IRIs. The markdown
    // renderer resolves labels itself regardless of `--labels`, so this only
    // applies to plain/pretty.
    if use_labels && !matches!(fmt.as_str(), "markdown" | "html") {
        let mut labels = label_map(&left)?;
        for (k, v) in label_map(&right)? {
            labels.entry(k).or_insert(v);
        }
        report = annotate_labels(&report, &labels);
    }

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
/// one blank line between them.
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
/// `0 axioms in left ontology but not in right ontology:`.
fn render_basic(
    left: &crate::model::Model,
    right: &crate::model::Model,
    d: &diff::Diff,
    id_differs: bool,
) -> String {
    let mut removed: Vec<String> = d.only_left.iter().map(render_line).collect();
    let mut added: Vec<String> = d.only_right.iter().map(render_line).collect();
    // When the IDs differ, each side's ID string joins its own set and counts
    // toward that section's total.
    if id_differs {
        removed.push(ontology_id_string(left));
        added.push(ontology_id_string(right));
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{} axioms in left ontology but not in right ontology:\n",
        removed.len()
    ));
    let mut removed: Vec<String> = removed.iter().map(|a| format!("- {a}")).collect();
    removed.sort();
    for line in &removed {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&format!(
        "{} axioms in right ontology but not in left ontology:\n",
        added.len()
    ));
    let mut added: Vec<String> = added.iter().map(|a| format!("+ {a}")).collect();
    added.sort();
    for line in &added {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// One axiom line, with newlines escaped — a multi-line literal must not break
/// the one-axiom-per-line contract.
fn render_line(ac: &AnnotatedComponent<RcStr>) -> String {
    diff::describe(ac).replace('\n', "\\n")
}

/// The ontology ID as a diff line: `OntologyID(OntologyIRI(<iri>)
/// VersionIRI(<viri>))`, with `Anonymous` for an unnamed ontology and `<null>`
/// for an absent version IRI.
fn ontology_id_string(model: &crate::model::Model) -> String {
    let (iri, viri) = diff::ontology_id(model);
    let head = match iri {
        Some(i) => format!("OntologyIRI(<{i}>)"),
        None => "Anonymous".to_string(),
    };
    let ver = viri.map(|v| format!("<{v}>")).unwrap_or_else(|| "<null>".to_string());
    format!("OntologyID({head} VersionIRI({ver}))")
}

// ---------------------------------------------------------------------------
// markdown — one frame per axiom subject
// ---------------------------------------------------------------------------

/// The frame a change is bucketed into. Imports and ontology annotations always
/// lead; everything else is keyed by the axiom's subject.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
enum Grouping {
    OntologyImport,
    OntologyAnnotation,
    /// The axiom's subject is a named object or a bare IRI.
    Iri(String),
    /// A general class inclusion (the subject is an anonymous class expression).
    Gci,
    /// Anything else — a blank-node subject, say.
    NonIri(String),
}

/// Render the whole document: a header block for each side, then one frame per
/// grouping.
///
/// The whitespace is load-bearing, because repos commit these reports and any
/// drift rewrites the whole file: a trailing space follows every rendered object,
/// every axiom bullet is followed by a blank line even when it carries no
/// annotations, and two blank lines separate frames.
fn render_markdown(
    args: &Args,
    left: &crate::model::Model,
    right: &crate::model::Model,
    d: &diff::Diff,
) -> anyhow::Result<String> {
    // The markdown renderer ALWAYS resolves labels, over both ontologies,
    // independent of `--labels`.
    let mut labels = label_map(left)?;
    for (k, v) in label_map(right)? {
        labels.entry(k).or_insert(v);
    }

    let (liri, lver) = diff::ontology_id(left);
    let (riri, rver) = diff::ontology_id(right);
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

    // Bucket every change, keeping removed and added apart.
    let mut groups: BTreeMap<Grouping, (Vec<&AnnotatedComponent<RcStr>>, Vec<&AnnotatedComponent<RcStr>>)> =
        BTreeMap::new();
    for ac in &d.only_left {
        groups.entry(grouping_of(&ac.component)).or_default().0.push(ac);
    }
    for ac in &d.only_right {
        groups.entry(grouping_of(&ac.component)).or_default().1.push(ac);
    }
    // These two keys are always in the map, so their frames are emitted even when
    // empty: a report opens with an `### Ontology imports` frame whether or not
    // any import changed.
    groups.entry(Grouping::OntologyImport).or_default();
    groups.entry(Grouping::OntologyAnnotation).or_default();

    let header_of = |g: &Grouping| -> String {
        match g {
            Grouping::OntologyImport => "Ontology imports".to_string(),
            Grouping::OntologyAnnotation => "Ontology annotations".to_string(),
            Grouping::Gci => "GCIs".to_string(),
            Grouping::Iri(iri) => short_form(iri, &labels),
            Grouping::NonIri(s) => s.clone(),
        }
    };

    // Imports first, then ontology annotations, then every other frame sorted by
    // its header label.
    let mut rest: Vec<&Grouping> = groups
        .keys()
        .filter(|g| !matches!(g, Grouping::OntologyImport | Grouping::OntologyAnnotation))
        .collect();
    rest.sort_by_key(|g| header_of(g));
    let ordered: Vec<&Grouping> = [&Grouping::OntologyImport, &Grouping::OntologyAnnotation]
        .into_iter()
        .chain(rest)
        .collect();

    for g in ordered {
        let (removed, added) = groups.get(g).map(|(r, a)| (r.clone(), a.clone())).unwrap_or_default();
        let mut removed = removed;
        let mut added = added;
        removed.sort_by_key(|ac| sort_key(ac));
        added.sort_by_key(|ac| sort_key(ac));

        let removed_list = change_list("Removed", &removed, &labels);
        let added_list = change_list("Added", &added, &labels);
        let iri = match g {
            Grouping::Iri(iri) => format!("`{iri}`"),
            _ => String::new(),
        };
        // A blank line, then the frame itself: a `### <header> <iri>` line, the
        // removed list and the added list.
        out.push('\n');
        out.push_str(&format!("### {} {iri}\n{removed_list}\n{added_list}\n", header_of(g)));
    }
    Ok(out)
}

/// A backticked IRI, or `*None*` when the side carries none.
fn optional_iri(iri: Option<&str>) -> String {
    iri.map(|i| format!("`{i}`")).unwrap_or_else(|| "*None*".to_string())
}

/// A `#### <header>` block over the rendered items — empty when there is nothing
/// to list, so the frame collapses to a bare blank line.
fn change_list(
    header: &str,
    items: &[&AnnotatedComponent<RcStr>],
    labels: &HashMap<String, String>,
) -> String {
    if items.is_empty() {
        return String::new();
    }
    let rendered: Vec<String> = items.iter().map(|ac| markdown_for_axiom(ac, labels)).collect();
    format!("#### {header}\n{}", rendered.join("\n"))
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

/// The axiom line plus one nested bullet per axiom annotation. EVERY item ends in
/// a newline even when it has no annotations — that is the blank line after each
/// bullet in the committed reports.
fn markdown_for_axiom(ac: &AnnotatedComponent<RcStr>, labels: &HashMap<String, String>) -> String {
    let body = render_axiom_md(&ac.component, labels);
    let inner: Vec<String> = ac
        .ann
        .iter()
        .map(|a| {
            format!(
                "  - {} {} \n",
                md_iri(a.ap.0.as_ref(), labels),
                render_annval_md(&a.av, labels)
            )
        })
        .collect();
    format!("- {body} \n{}", inner.join("\n"))
}

/// The frame a component belongs to, keyed by the axiom's subject.
fn grouping_of(c: &Component<RcStr>) -> Grouping {
    use Component::*;
    let iri = match c {
        Import(_) => return Grouping::OntologyImport,
        OntologyAnnotation(_) => return Grouping::OntologyAnnotation,
        // Keyed by the sub-class; an anonymous sub-class is a GCI.
        SubClassOf(a) => return match &a.sub {
            horned_owl::model::ClassExpression::Class(c) => Grouping::Iri(c.0.as_ref().to_string()),
            _ => Grouping::Gci,
        },
        DeclareClass(d) => d.0 .0.as_ref().to_string(),
        DeclareObjectProperty(d) => d.0 .0.as_ref().to_string(),
        DeclareDataProperty(d) => d.0 .0.as_ref().to_string(),
        DeclareAnnotationProperty(d) => d.0 .0.as_ref().to_string(),
        DeclareNamedIndividual(d) => d.0 .0.as_ref().to_string(),
        DeclareDatatype(d) => d.0 .0.as_ref().to_string(),
        AnnotationAssertion(a) => match &a.subject {
            horned_owl::model::AnnotationSubject::IRI(i) => i.as_ref().to_string(),
            horned_owl::model::AnnotationSubject::AnonymousIndividual(b) => {
                return Grouping::NonIri(format!("_:{}", b.0.as_ref()))
            }
        },
        // Keyed by the FIRST operand, which horned-owl keeps in document order.
        EquivalentClasses(a) => return first_ce_grouping(&a.0),
        DisjointClasses(a) => return first_ce_grouping(&a.0),
        DisjointUnion(a) => a.0 .0.as_ref().to_string(),
        SubObjectPropertyOf(a) => return match &a.sub {
            horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(ope) => {
                Grouping::Iri(ope_iri(ope))
            }
            // A property chain has no single sub-property, so it groups under
            // its SUPER property.
            horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(_) => {
                Grouping::Iri(ope_iri(&a.sup))
            }
        },
        SubAnnotationPropertyOf(a) => a.sub.0.as_ref().to_string(),
        SubDataPropertyOf(a) => a.sub.0.as_ref().to_string(),
        ObjectPropertyDomain(a) => ope_iri(&a.ope),
        ObjectPropertyRange(a) => ope_iri(&a.ope),
        DataPropertyDomain(a) => a.dp.0.as_ref().to_string(),
        DataPropertyRange(a) => a.dp.0.as_ref().to_string(),
        AnnotationPropertyDomain(a) => a.ap.0.as_ref().to_string(),
        AnnotationPropertyRange(a) => a.ap.0.as_ref().to_string(),
        FunctionalObjectProperty(a) => ope_iri(&a.0),
        InverseFunctionalObjectProperty(a) => ope_iri(&a.0),
        ReflexiveObjectProperty(a) => ope_iri(&a.0),
        IrreflexiveObjectProperty(a) => ope_iri(&a.0),
        SymmetricObjectProperty(a) => ope_iri(&a.0),
        AsymmetricObjectProperty(a) => ope_iri(&a.0),
        TransitiveObjectProperty(a) => ope_iri(&a.0),
        InverseObjectProperties(a) => ope_iri(&a.0),
        FunctionalDataProperty(a) => a.0 .0.as_ref().to_string(),
        EquivalentObjectProperties(a) => match a.0.first() {
            Some(ope) => ope_iri(ope),
            None => return Grouping::Gci,
        },
        DisjointObjectProperties(a) => match a.0.first() {
            Some(ope) => ope_iri(ope),
            None => return Grouping::Gci,
        },
        EquivalentDataProperties(a) => match a.0.first() {
            Some(dp) => dp.0.as_ref().to_string(),
            None => return Grouping::Gci,
        },
        DisjointDataProperties(a) => match a.0.first() {
            Some(dp) => dp.0.as_ref().to_string(),
            None => return Grouping::Gci,
        },
        ClassAssertion(a) => return individual_grouping(&a.i),
        ObjectPropertyAssertion(a) => return individual_grouping(&a.from),
        NegativeObjectPropertyAssertion(a) => return individual_grouping(&a.from),
        DataPropertyAssertion(a) => return individual_grouping(&a.from),
        NegativeDataPropertyAssertion(a) => return individual_grouping(&a.from),
        SameIndividual(a) => match a.0.first() {
            Some(i) => return individual_grouping(i),
            None => return Grouping::Gci,
        },
        DifferentIndividuals(a) => match a.0.first() {
            Some(i) => return individual_grouping(i),
            None => return Grouping::Gci,
        },
        DatatypeDefinition(a) => a.kind.0.as_ref().to_string(),
        HasKey(a) => return match &a.ce {
            horned_owl::model::ClassExpression::Class(c) => Grouping::Iri(c.0.as_ref().to_string()),
            _ => Grouping::Gci,
        },
        Rule(_) => return Grouping::NonIri("Rules".to_string()),
        _ => return Grouping::Gci,
    };
    Grouping::Iri(iri)
}

/// The first operand of an n-ary class axiom, which is a GCI grouping when it is
/// anonymous.
fn first_ce_grouping(v: &[horned_owl::model::ClassExpression<RcStr>]) -> Grouping {
    match v.first() {
        Some(horned_owl::model::ClassExpression::Class(c)) => {
            Grouping::Iri(c.0.as_ref().to_string())
        }
        _ => Grouping::Gci,
    }
}

fn individual_grouping(i: &horned_owl::model::Individual<RcStr>) -> Grouping {
    match i {
        horned_owl::model::Individual::Named(n) => Grouping::Iri(n.0.as_ref().to_string()),
        horned_owl::model::Individual::Anonymous(a) => {
            Grouping::NonIri(format!("_:{}", a.0.as_ref()))
        }
    }
}

fn ope_iri(ope: &horned_owl::model::ObjectPropertyExpression<RcStr>) -> String {
    use horned_owl::model::ObjectPropertyExpression as OPE;
    match ope {
        OPE::ObjectProperty(p) => p.0.as_ref().to_string(),
        OPE::InverseObjectProperty(p) => p.0.as_ref().to_string(),
    }
}

/// An IRI as a markdown link: `[short form](iri)`.
fn md_iri(iri: &str, labels: &HashMap<String, String>) -> String {
    format!("[{}]({iri})", short_form(iri, labels))
}

/// The term's `rdfs:label` when one is known, otherwise the IRI's fragment, or
/// failing that its last path segment.
fn short_form(iri: &str, labels: &HashMap<String, String>) -> String {
    if let Some(l) = labels.get(iri) {
        return l.clone();
    }
    if let Some(s) = ncname_suffix(iri) {
        return s.to_string();
    }
    // No NCName suffix — an ORCID that ends in a digit has none, since an NCName
    // may not begin with one. The last path segment stands in.
    match iri.rsplit_once('#') {
        Some((_, frag)) if !frag.is_empty() => frag.to_string(),
        _ => match iri.rsplit_once('/') {
            Some((_, seg)) if !seg.is_empty() => seg.to_string(),
            // …and where there is no segment either, because the IRI ends in a
            // separator, the whole IRI stands, in angle brackets, so a reader can
            // see it is the short form rather than a truncation of one.
            _ => format!("<{iri}>"),
        },
    }
}

/// The IRI's local name: its longest suffix that is a valid XML NCName.
///
/// This is what makes `…/ECTO_0000985` shorten to `ECTO_0000985` while
/// `https://orcid.org/0000-0002-2996-719X` shortens to `X` — an NCName may not
/// begin with a digit or a hyphen, so the local name starts at the last character
/// that can begin one. Scanning stops at the first character that cannot appear in
/// an NCName at all (`/`, `:`), so an IRI ending in a separator has no local name.
fn ncname_suffix(iri: &str) -> Option<&str> {
    let mut start: Option<usize> = None;
    for (i, c) in iri.char_indices().rev() {
        if !is_ncname_char(c) {
            break;
        }
        if is_ncname_start_char(c) {
            start = Some(i);
        }
    }
    start.map(|i| &iri[i..])
}

/// `NCNameStartChar` from XML 1.0 (5th ed.), less `:`.
fn is_ncname_start_char(c: char) -> bool {
    matches!(c,
        'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}'
        | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}'
        | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}'
        | '\u{10000}'..='\u{EFFFF}')
}

/// `NCNameChar`: a start character, or one of the characters that may follow one.
fn is_ncname_char(c: char) -> bool {
    is_ncname_start_char(c)
        || matches!(c,
            '-' | '.' | '0'..='9' | '\u{B7}'
            | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// Manchester syntax with every IRI rendered as a markdown link, e.g.
/// `[1st arch mandibular mesenchyme](…) SubClassOf [part of](…) some [1st arch
/// mandibular component](…)`.
fn render_axiom_md(c: &Component<RcStr>, labels: &HashMap<String, String>) -> String {
    use Component::*;
    let ce = |x| render_ce_md(x, labels);
    match c {
        Import(i) => md_iri(i.0.as_ref(), labels),
        OntologyAnnotation(a) => format!(
            "{} {}",
            md_iri(a.0.ap.0.as_ref(), labels),
            render_annval_md(&a.0.av, labels)
        ),
        DeclareClass(d) => format!("Class: {}", md_iri(d.0 .0.as_ref(), labels)),
        DeclareObjectProperty(d) => format!("ObjectProperty: {}", md_iri(d.0 .0.as_ref(), labels)),
        DeclareDataProperty(d) => format!("DataProperty: {}", md_iri(d.0 .0.as_ref(), labels)),
        DeclareAnnotationProperty(d) => {
            format!("AnnotationProperty: {}", md_iri(d.0 .0.as_ref(), labels))
        }
        DeclareNamedIndividual(d) => format!("Individual: {}", md_iri(d.0 .0.as_ref(), labels)),
        DeclareDatatype(d) => format!("Datatype: {}", md_iri(d.0 .0.as_ref(), labels)),
        SubClassOf(a) => format!("{} SubClassOf {}", ce(&a.sub), ce(&a.sup)),
        EquivalentClasses(a) => {
            a.0.iter().map(ce).collect::<Vec<_>>().join(" EquivalentTo ")
        }
        DisjointClasses(a) => {
            format!("DisjointClasses: {}", a.0.iter().map(ce).collect::<Vec<_>>().join(", "))
        }
        AnnotationAssertion(a) => format!(
            "{} {} {}",
            match &a.subject {
                horned_owl::model::AnnotationSubject::IRI(i) => md_iri(i.as_ref(), labels),
                horned_owl::model::AnnotationSubject::AnonymousIndividual(b) =>
                    format!("_:{}", b.0.as_ref()),
            },
            md_iri(a.ann.ap.0.as_ref(), labels),
            render_annval_md(&a.ann.av, labels)
        ),
        SubObjectPropertyOf(a) => format!(
            "{} SubPropertyOf {}",
            match &a.sub {
                horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(ope) =>
                    render_ope_md(ope, labels),
                horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(chain) =>
                    chain.iter().map(|p| render_ope_md(p, labels)).collect::<Vec<_>>().join(" o "),
            },
            render_ope_md(&a.sup, labels)
        ),
        SubAnnotationPropertyOf(a) => format!(
            "{} SubPropertyOf {}",
            md_iri(a.sub.0.as_ref(), labels),
            md_iri(a.sup.0.as_ref(), labels)
        ),
        ObjectPropertyDomain(a) => {
            format!("{} Domain {}", render_ope_md(&a.ope, labels), ce(&a.ce))
        }
        ObjectPropertyRange(a) => {
            format!("{} Range {}", render_ope_md(&a.ope, labels), ce(&a.ce))
        }
        ClassAssertion(a) => format!("{} Type {}", render_individual_md(&a.i, labels), ce(&a.ce)),
        ObjectPropertyAssertion(a) => format!(
            "{} {} {}",
            render_individual_md(&a.from, labels),
            render_ope_md(&a.ope, labels),
            render_individual_md(&a.to, labels)
        ),
        TransitiveObjectProperty(a) => {
            format!("{} Characteristics: Transitive", render_ope_md(&a.0, labels))
        }
        // The long tail keeps owlmake's shared functional-style description, with
        // IRIs linked so the report stays navigable.
        other => link_iris(&crate::diff::describe(&AnnotatedComponent {
            component: other.clone(),
            ann: Default::default(),
        }), labels),
    }
}

/// Turn every bare `http(s)://…` token in a fallback rendering into a markdown
/// link, so the long-tail axiom types still read like the rest of the document.
fn link_iris(text: &str, labels: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("http") {
        if !rest[i..].starts_with("http://") && !rest[i..].starts_with("https://") {
            out.push_str(&rest[..i + 4]);
            rest = &rest[i + 4..];
            continue;
        }
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ',' | '"' | '>'))
            .unwrap_or(tail.len());
        out.push_str(&md_iri(&tail[..end], labels));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// One operand of an intersection or union: bracketed when it is a compound
/// expression, bare when it is a name.
fn bracket_operand_md(
    ce: &horned_owl::model::ClassExpression<RcStr>,
    labels: &HashMap<String, String>,
) -> String {
    use horned_owl::model::ClassExpression as CE;
    let body = render_ce_md(ce, labels);
    match ce {
        CE::Class(_) | CE::ObjectOneOf(_) => body,
        _ => format!("({body})"),
    }
}

fn render_ce_md(
    ce: &horned_owl::model::ClassExpression<RcStr>,
    labels: &HashMap<String, String>,
) -> String {
    use horned_owl::model::ClassExpression as CE;
    let rec = |x| render_ce_md(x, labels);
    match ce {
        CE::Class(c) => md_iri(c.0.as_ref(), labels),
        // `A and (R some B) and (S some C)`: the operands carry the brackets, not
        // the intersection. A named class needs none — the brackets are there to
        // keep a restriction's own operand from reading as another operand of the
        // intersection, and around a whole list there is nothing to disambiguate.
        CE::ObjectIntersectionOf(v) => {
            v.iter().map(|x| bracket_operand_md(x, labels)).collect::<Vec<_>>().join(" and ")
        }
        CE::ObjectUnionOf(v) => {
            v.iter().map(|x| bracket_operand_md(x, labels)).collect::<Vec<_>>().join(" or ")
        }
        CE::ObjectComplementOf(b) => format!("not {}", rec(b)),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            format!("{} some {}", render_ope_md(ope, labels), rec(bce))
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            format!("{} only {}", render_ope_md(ope, labels), rec(bce))
        }
        CE::ObjectHasValue { ope, i } => format!(
            "{} value {}",
            render_ope_md(ope, labels),
            render_individual_md(i, labels)
        ),
        CE::ObjectMinCardinality { n, ope, bce } => {
            format!("{} min {n} {}", render_ope_md(ope, labels), rec(bce))
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            format!("{} max {n} {}", render_ope_md(ope, labels), rec(bce))
        }
        CE::ObjectExactCardinality { n, ope, bce } => {
            format!("{} exactly {n} {}", render_ope_md(ope, labels), rec(bce))
        }
        CE::ObjectHasSelf(ope) => format!("{} Self", render_ope_md(ope, labels)),
        CE::ObjectOneOf(v) => format!(
            "{{{}}}",
            v.iter().map(|i| render_individual_md(i, labels)).collect::<Vec<_>>().join(", ")
        ),
        CE::DataSomeValuesFrom { dp, .. } => {
            format!("{} some ...", md_iri(dp.0.as_ref(), labels))
        }
        CE::DataAllValuesFrom { dp, .. } => {
            format!("{} only ...", md_iri(dp.0.as_ref(), labels))
        }
        CE::DataHasValue { dp, l } => {
            format!("{} value {}", md_iri(dp.0.as_ref(), labels), render_literal_md(l, labels))
        }
        other => format!("{other:?}"),
    }
}

fn render_ope_md(
    ope: &horned_owl::model::ObjectPropertyExpression<RcStr>,
    labels: &HashMap<String, String>,
) -> String {
    use horned_owl::model::ObjectPropertyExpression as OPE;
    match ope {
        OPE::ObjectProperty(p) => md_iri(p.0.as_ref(), labels),
        OPE::InverseObjectProperty(p) => format!("inverse {}", md_iri(p.0.as_ref(), labels)),
    }
}

fn render_individual_md(
    i: &horned_owl::model::Individual<RcStr>,
    labels: &HashMap<String, String>,
) -> String {
    match i {
        horned_owl::model::Individual::Named(n) => md_iri(n.0.as_ref(), labels),
        horned_owl::model::Individual::Anonymous(a) => format!("_:{}", a.0.as_ref()),
    }
}

fn render_annval_md(
    av: &horned_owl::model::AnnotationValue<RcStr>,
    labels: &HashMap<String, String>,
) -> String {
    use horned_owl::model::AnnotationValue;
    match av {
        AnnotationValue::Literal(l) => render_literal_md(l, labels),
        AnnotationValue::IRI(i) => md_iri(i.as_ref(), labels),
        AnnotationValue::AnonymousIndividual(a) => format!("_:{}", a.0.as_ref()),
    }
}

/// `"lex"`, `"lex"@lang`, or `"lex"^^[short](datatype)` — the datatype is itself
/// a markdown link, e.g. `"2026-06-08"^^[string](http://www.w3.org/2001/XMLSchema#string)`.
fn render_literal_md(
    l: &horned_owl::model::Literal<RcStr>,
    labels: &HashMap<String, String>,
) -> String {
    use horned_owl::model::Literal;
    match l {
        Literal::Simple { literal } => format!("{:?}", literal),
        Literal::Language { literal, lang } => format!("{:?}@{lang}", literal),
        Literal::Datatype { literal, datatype_iri } => {
            format!("{:?}^^{}", literal, md_iri(datatype_iri.as_ref(), labels))
        }
    }
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
        let out = render_basic(&left, &right, &d, false);
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
        let out = render_basic(&left, &right, &d, true);
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
            labels: None,
            common: Default::default(),
        };
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

    #[test]
    fn short_form_prefers_a_label_then_the_ncname_suffix() {
        let mut labels = HashMap::new();
        labels.insert("http://x/A".to_string(), "alpha".to_string());
        assert_eq!(short_form("http://x/A", &labels), "alpha");
        assert_eq!(short_form("http://x/ns#B", &labels), "B");
        assert_eq!(short_form("http://purl.obolibrary.org/obo/CL_0000000", &labels), "CL_0000000");
        // An NCName cannot begin with a digit or a hyphen, so an ORCID ending in a
        // letter has that letter alone as its local name…
        assert_eq!(short_form("https://orcid.org/0000-0002-2996-719X", &labels), "X");
        // …while one ending in a digit has no NCName suffix at all, and falls back
        // to the last path segment.
        assert_eq!(
            short_form("https://orcid.org/0000-0002-2996-7190", &labels),
            "0000-0002-2996-7190"
        );
        assert_eq!(short_form("http://example.org/2026-08-19", &labels), "2026-08-19");
        // Only where there is no segment either does the whole IRI stand, bracketed.
        assert_eq!(
            short_form("https://example.org/a/b/", &labels),
            "<https://example.org/a/b/>"
        );
    }

    /// Frames are keyed by the axiom's SUBJECT, so both a declaration and a
    /// subClassOf on the same term land in one frame.
    #[test]
    fn grouping_follows_the_axiom_subject() {
        let b: Build<RcStr> = Build::new();
        let decl = Component::DeclareClass(horned_owl::model::DeclareClass(b.class("http://x/A")));
        let sub = Component::SubClassOf(horned_owl::model::SubClassOf {
            sub: b.class("http://x/A").into(),
            sup: b.class("http://x/B").into(),
        });
        assert_eq!(grouping_of(&decl), Grouping::Iri("http://x/A".into()));
        assert_eq!(grouping_of(&sub), Grouping::Iri("http://x/A".into()));
        assert_eq!(
            grouping_of(&Component::Import(horned_owl::model::Import(b.iri("http://x/i.owl")))),
            Grouping::OntologyImport
        );
    }
}
