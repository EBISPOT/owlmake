//! Command chaining.
//!
//! Several operations can share one in-memory ontology in a single invocation,
//! e.g. `om merge -i a.owl reason --reasoner ELK reduce -o out`: the argv is
//! split into per-subcommand segments, each segment is parsed with clap, and a
//! single `Model` is threaded left-to-right through every command's `step()`.
//! The first command with neither a piped model nor `--input` errors; any
//! command with `--output` writes at that point (so intermediate dumps work
//! too); side-output commands (query/report/…) pass the ontology through
//! unchanged.
//!
//! Segmentation is data-driven: we introspect the clap `Command` tree to learn,
//! per subcommand, which flags consume values (and how many), so a value that
//! happens to match a subcommand name (`--reasoner reason`) is not mistaken for a
//! command boundary. This stays correct as flags are added — no hand table.

use std::collections::HashMap;

use anyhow::{bail, Result};
use clap::{ArgAction, CommandFactory, Parser, Subcommand};

use crate::cmd;
use crate::model::Model;

/// The owlmake command-line interface (the clap tree the binary parses and the
/// bindings introspect). Defined here in the library so both `src/main.rs` and
/// the Python extension dispatch through the same definitions.
#[derive(Parser)]
#[command(
    name = "om",
    version,
    about = "Self-contained OWL/OBO ontology build and QC toolkit",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Convert an ontology between serialization formats.
    Convert(cmd::convert::Args),
    /// Merge multiple ontologies into one.
    Merge(cmd::merge::Args),
    /// Collapse cliques of equivalent classes into a prefix-priority leader.
    MergeEquivalentSets(cmd::merge_equivalent_sets::Args),
    /// Remove the axioms of a second ontology from the first.
    Unmerge(cmd::unmerge::Args),
    /// Allocate definitive IDs for temporary ones from a named ID range
    /// (KGCL `kgcl:mint`; the `allocate-definitive-ids` target).
    Mint(cmd::mint::Args),
    /// Print every axiom mentioning an entity that matches a pattern — the
    /// shorthand for `filter --term <IRI> --trim false`, in any format.
    Ogrep(cmd::ogrep::Args),
    /// Download an ontology's import closure and write an XML catalog.
    Mirror(cmd::mirror::Args),
    /// Generate an import module (signature of edit ontology → module of source).
    Import(cmd::import_module::Args),
    /// Classify with the EL reasoner and assert inferred subsumptions.
    Reason(cmd::reason::Args),
    /// Remove redundant SubClassOf axioms (transitive reduction).
    Reduce(cmd::reduce::Args),
    /// Relax equivalence axioms into weaker SubClassOf existentials.
    Relax(cmd::relax::Args),
    /// Materialize inferred existential restrictions.
    Materialize(cmd::materialize::Args),
    /// Inject subset / synonym-type subproperty declarations (`odk:normalize`).
    Normalize(cmd::normalize::Args),
    /// Convert a Babelon translation TSV into OWL annotation axioms.
    Babelon(cmd::babelon::Args),
    /// Regenerate textual definitions (FlyBase `rewrite-def`: DOT/SUB definitions).
    RewriteDef(cmd::rewrite_def::Args),
    /// Compare two ontologies and report differences.
    Diff(cmd::diff::Args),
    /// Add ontology annotations / set ontology and version IRIs.
    Annotate(cmd::annotate::Args),
    /// Keep only axioms mentioning the selected terms.
    Filter(cmd::filter::Args),
    /// Remove axioms mentioning the selected terms.
    Remove(cmd::remove::Args),
    /// Extract a module for a seed term set (BOT/TOP/STAR/MIREOT).
    Extract(cmd::extract::Args),
    /// Extract per-entity label / exact-synonym strings for embedding, in the byte form
    /// an OLS4 index keys on (cl100k truncation + SHA-1 content hash). One row per string.
    #[command(name = "extract-strings")]
    ExtractStrings(cmd::extract_strings::Args),
    /// Materialize uPheno's phenotype shortcut relations from EQ definitions
    /// (`upheno:extract-upheno-relations`).
    ExtractUphenoRelations(cmd::extract_upheno_relations::Args),
    /// Build/update, average, semantic-similarity, and search over term embeddings
    /// (OLS4-compatible; OpenAI-backed). See `om embeddings <embed|average|semsim|search>`.
    Embeddings(cmd::embeddings::Args),
    /// Tag free text against an ontology term DB: `build` the Aho-Corasick DB, or
    /// `stream` to tag text against one (raw `.bin` or published `.bin.gz`).
    #[command(name = "text-tagger")]
    TextTagger(cmd::text_tagger::Args),
    /// Map free-text strings to ontology terms (hybrid: tagger lexical + embedding +
    /// fuzzy, fused and ranked).
    Map(cmd::map::Args),
    /// Lexical matching between ontology entities → SSSOM (normalized label/synonym
    /// key equality; base-2 logit confidence from a rules file).
    Lexmatch(cmd::lexmatch::Args),
    /// KGX graph exchange (`kgx transform`): OBO Graph JSON → KGX node/edge TSV,
    /// a release's `<ont>_nodes.tsv` / `<ont>_edges.tsv` artefacts.
    Kgx(cmd::kgx::Args),
    /// Report ontology metrics.
    Measure(cmd::measure::Args),
    /// Annotate every class with a structural information-content (IC) score
    /// derived from the subclass hierarchy (leaves score 100, the root 0).
    InformationContent(cmd::information_content::Args),
    /// Validate an ontology against an OWL 2 profile (EL/QL/RL/DL).
    ValidateProfile(cmd::validate_profile::Args),
    /// Check an OBO ID-policy file (`<ont>-idranges.owl`) — the ID-policy check a
    /// repo's `test` target runs.
    #[command(name = "validate-id-ranges")]
    ValidateIdRanges(cmd::validate_id_ranges::Args),
    /// Schema-validate DOSDP design patterns (`dosdp validate -i`) — the check a
    /// repo's `dosdp_validation` target runs.
    #[command(name = "validate-patterns")]
    ValidatePatterns(cmd::validate_patterns::Args),
    /// Verify that a file parses as RDF/XML — run over every release product by
    /// the `all_assets` target.
    #[command(name = "check-rdfxml")]
    CheckRdfxml(cmd::check_rdfxml::Args),
    /// Build an ontology's SQL database (`semsql make <name>.db`): its triples,
    /// its reasoned relation graph, and the views over both.
    ///
    /// Handled before chaining (see `run_argv`): its `make` subcommand shares a
    /// name with owlmake's own, so the harness must not split the line there.
    #[command(name = "semsql", disable_help_flag = true)]
    Semsql(cmd::semsql::Args),
    /// Print SHA-256 checksums in the usual `<hash>  <name>` layout (`config_check`).
    #[command(name = "sha256sum")]
    Sha256Sum(cmd::config_check::Sha256Args),
    /// Compare a repo config's hash with the one the build was generated from
    /// (the whole `config_check` recipe, in one command).
    #[command(name = "config-check")]
    ConfigCheck(cmd::config_check::ConfigCheckArgs),
    /// Print owlmake's identity and version for every tool handle a build's
    /// `odk-info --tools` banner lists.
    #[command(name = "odk-info")]
    OdkInfo(cmd::config_check::OdkInfoArgs),
    /// Run a SPARQL SELECT/ASK/CONSTRUCT query over the ontology.
    Query(cmd::query::Args),
    /// Run SPARQL QC checks that must each return zero rows.
    Verify(cmd::verify::Args),
    /// Run the QC report profile and emit violations.
    Report(cmd::report::Args),
    /// Generate OWL from a template table (TSV/CSV).
    Template(cmd::template::Args),
    /// Bulk-rename entity IRIs.
    Rename(cmd::rename::Args),
    /// Export ontology entities to a spreadsheet (TSV/CSV).
    Export(cmd::export::Args),
    /// Dump the prefix map as a JSON-LD context.
    ExportPrefixes(cmd::export_prefixes::Args),
    /// Run the release pipeline (relax→reason→reduce) and emit artefacts.
    Release(cmd::release::Args),
    /// Build the Ubergraph data product (merged + redundant/non-redundant relation
    /// graphs + IC) as named-graph N-Quads.
    Ubergraph(cmd::ubergraph::Args),
    /// Build an ontology's release artefacts: resolve the repo's plan (a committed
    /// `owlmake.yaml`, or one regenerated from its build configuration), then run
    /// the requested targets. Defaults to the current directory.
    Make(cmd::make::Args),
    /// Build every release artefact (`prepare_release`).
    #[command(visible_aliases = ["prepare_release", "all"])]
    PrepareRelease(cmd::make::TargetArgs),
    /// Rebuild the import modules from upstream (`refresh-imports`).
    #[command(visible_alias = "refresh_imports")]
    RefreshImports(cmd::make::RefreshArgs),
    /// Rebuild every individual import module from upstream (`all_imports`).
    #[command(visible_alias = "all_imports")]
    AllImports(cmd::make::RepoArgs),
    /// Run the repository's QC checks (`test`).
    Test(cmd::make::RepoArgs),
    /// Scaffold a starter `owlmake.json` for a new ontology.
    Seed(cmd::seed::Args),
    /// Print the JSON Schema for `owlmake.json` (for editor/CI validation).
    Schema(cmd::schema::Args),
    /// Explain why a subsumption is entailed (compute a justification).
    Explain(cmd::explain::Args),
    /// Fix common mechanical problems (duplicates, dangling references).
    Repair(cmd::repair::Args),
    /// Collapse the hierarchy to a set of precious terms.
    Collapse(cmd::collapse::Args),
    /// Expand OBO/OWL macros (IAO:0000424 expandExpressionTo).
    Expand(cmd::expand::Args),
    /// Extract an `oboInOwl:inSubset` slice (`odk:subset`).
    Subset(cmd::subset::Args),
    /// Extract a named `oboInOwl:inSubset` slice, optionally extended to its full
    /// graph-ancestor closure (`--fill-gaps`; UBERON `common-anatomy.owl`).
    ExtractOntologySubset(cmd::owltools_ops::SubsetArgs),
    /// Reduce to the class hierarchy + labels + property ontology (UBERON composite
    /// `-basic`).
    ExtractMingraph(cmd::owltools_ops::SimpleArgs),
    /// Strip annotations from every axiom.
    RemoveAxiomAnnotations(cmd::owltools_ops::SimpleArgs),
    /// Keep only axioms whose object properties are in the given list (UBERON
    /// composite `-basic`).
    MakeSubsetByProperties(cmd::owltools_ops::MakeSubsetByPropsArgs),
    /// Fold species-specific classes into a composite ontology (`uberon:merge-species`).
    MergeSpecies(cmd::merge_species::Args),
    /// Compute a taxon-specific subset, tagging/removing classes (`uberon:create-species-subset`).
    CreateSpeciesSubset(cmd::create_species_subset::Args),
    /// Generate OWL from a DOSDP pattern + TSV data table.
    Dosdp(cmd::dosdp::Args),
    /// Run the bundled jq engine over JSON/YAML/… input.
    ///
    /// Accepts jq flags, a filter and input files verbatim. Handled before command
    /// chaining (see `run_argv`), so this variant exists only so the command appears
    /// in `--help`; its args are never parsed by clap.
    #[command(disable_help_flag = true, hide = true)]
    Jq(JqArgs),
    /// SSSOM mapping-set toolkit.
    ///
    /// Carries its own subcommand surface (convert, parse, validate, merge, sort,
    /// filter, annotate, invert, …). Handled before chaining (see `run_argv`)
    /// because that grammar takes positional inputs.
    #[command(disable_help_flag = true)]
    Sssom(SssomArgs),
    /// Run the bundled `sed` (uutils' POSIX/GNU stream editor).
    ///
    /// Satisfies the `sed` invocations in build recipes from inside the binary.
    /// Handled before chaining (see `run_argv`); this variant only makes the
    /// command appear in `--help`. Its args are never parsed by clap.
    #[command(disable_help_flag = true, hide = true)]
    Sed(UtilArgs),
    /// Search input for lines matching a pattern (`grep` flags; Rust-regex syntax).
    #[command(disable_help_flag = true, hide = true)]
    Grep(UtilArgs),
    /// Compare two sorted files line by line (uutils' `comm`).
    #[command(disable_help_flag = true, hide = true)]
    Comm(UtilArgs),
}

/// Placeholder args for the bundled text utilities `sed`/`grep`/`comm`
/// (intercepted before clap parses them, like `jq`).
#[derive(clap::Args)]
pub struct UtilArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    #[allow(dead_code)]
    args: Vec<String>,
}

/// Placeholder args for `owlmake sssom` (intercepted before clap parses them).
#[derive(clap::Args)]
pub struct SssomArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    #[allow(dead_code)]
    args: Vec<String>,
}

/// Placeholder args for `owlmake jq` (intercepted before clap parses them).
#[derive(clap::Args)]
pub struct JqArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    #[allow(dead_code)]
    args: Vec<String>,
}

/// Whether a token is a help request (`--help`, `-h`, or a bare `help`).
fn is_help_token(tok: Option<&String>) -> bool {
    matches!(tok.map(String::as_str), Some("--help" | "-h" | "help"))
}

/// In-process owlmake entry point: dispatch one full argv (without the program
/// name) and return the process exit code. This is what both the binary
/// (`src/main.rs`) and the Python extension call — the single place command
/// dispatch lives.
///
/// It mirrors a normal CLI run: the standalone sub-CLIs (`jq`, `sssom`, the
/// bundled `sed`/`grep`/`comm`, `dosdp`) are intercepted first and return their
/// own exit code; everything else flows through the command-chaining harness.
/// Errors from the chain are printed to stderr (as the binary would)
/// and mapped to exit code 1. Unlike a bare binary, nothing here calls
/// `process::exit`, so an embedding host (Python) keeps control.
pub fn run_argv(mut argv: Vec<String>) -> i32 {
    // One invocation starts from the defaults, always. The options a run settles
    // on — `--strict` and `--xml-entities` latched off the command line, the
    // serialization conventions `make` takes from the plan — are process-wide, so
    // they last exactly as long as the process. That is invisible to the binary,
    // which exits; a host that calls in-process runs many invocations in one
    // process, and there a flag given to one command would go on applying to
    // every later one, and a plan's conventions would outlive the build that
    // chose them. Both are results decided by something other than the run that
    // produced them.
    crate::cmd::reset_invocation_options();
    // A leading `robot` token is accepted and dropped, so an existing invocation
    // spelled `owlmake robot reason …` behaves exactly like `owlmake reason …`.
    if argv.first().map(String::as_str) == Some("robot") {
        let rest = &argv[1..];
        if rest.is_empty() || is_help_token(rest.first()) {
            print_robot_help();
            return 0;
        }
        argv.remove(0);
    }
    // `owlmake __cli-spec` prints the full command/flag tree as JSON — the
    // machine-readable source of truth the bindings are generated from.
    if argv.first().map(String::as_str) == Some("__cli-spec") {
        print!("{}", dump_cli_spec());
        return 0;
    }
    // The standalone sub-CLIs carry their own flag grammar and must not pass
    // through the command-chaining harness; intercept them here and return
    // their exit code directly.
    if argv.first().map(String::as_str) == Some("jq") {
        return crate::jq::main(&argv[1..]);
    }
    if argv.first().map(String::as_str) == Some("arq") {
        return crate::arq::main(&argv[1..]);
    }
    if argv.first().map(String::as_str) == Some("sssom") {
        return crate::sssom::main(&argv[1..]);
    }
    // `owltools` as an argv token: accepted so a recipe that spells these steps that
    // way keeps working. It runs the subset / mingraph / axiom-annotation operations
    // UBERON's composite `-basic` and `common-anatomy` products need. Such a line
    // arrives here whole when its artefact is built through the shell path, which is
    // where it runs because it also greps and annotates.
    if argv.first().map(String::as_str) == Some("owltools") {
        return crate::cmd::owltools_ops::owltools_main(&argv[1..]);
    }
    match argv.first().map(String::as_str) {
        Some("sed") => return crate::util::sed_main(&argv[1..]),
        Some("grep") => return crate::util::grep_main(&argv[1..]),
        Some("comm") => return crate::util::comm_main(&argv[1..]),
        Some("gzip") => return crate::util::gzip_main(&argv[1..]),
        Some("gunzip") => {
            let mut a = vec!["-d".to_string()];
            a.extend_from_slice(&argv[1..]);
            return crate::util::gzip_main(&a);
        }
        Some("zcat") => {
            let mut a = vec!["-d".to_string(), "-c".to_string()];
            a.extend_from_slice(&argv[1..]);
            return crate::util::gzip_main(&a);
        }
        _ => {}
    }
    // Helper commands a build recipe invokes by bare name. Answering to these
    // names is what keeps a repo's `test` / `all_odk` targets from dying at exit
    // 127 on their first prerequisite. Each carries its own flag grammar
    // (`--assume-manchester`, `dosdp validate -i`, bundled short flags) rather
    // than the chained-command one, so like `jq`/`sssom` they are
    // intercepted before the chaining harness.
    match argv.first().map(String::as_str) {
        // `dicer-cli policy … <idranges.owl>` — the ID-policy check OBA and CL run.
        Some("dicer-cli") => return crate::cmd::validate_id_ranges::dicer_main(&argv[1..]),
        // `fastobo-validator <ont>.obo` — the OBO 1.4 syntax check in HPO's
        // `test` target.
        Some("fastobo-validator") => return crate::cmd::fastobo_validator::main(&argv[1..]),
        // `check-rdfxml <product>.owl` — the RDF/XML parse check over a product.
        Some("check-rdfxml") => return crate::cmd::check_rdfxml::main(&argv[1..]),
        // MONDO's `pattern_schema_checks`, a member of its `test` target.
        Some("simple_pattern_tester.py") | Some("simple-pattern-tester") => {
            return crate::cmd::pattern_tester::main(&argv[1..])
        }
        // OBA's first prerequisite of both `test` and `all_odk`.
        Some("odk-info") => return crate::cmd::config_check::odk_info_main(&argv[1..]),
        // `tr -d '\r' < … | sha256sum | cut -c1-64`, as the `config_check` recipe
        // spells it. A system `sha256sum` is absent on macOS and Windows; without
        // this arm the recipe compares an empty string and always claims the
        // config has changed.
        Some("sha256sum") => return crate::cmd::config_check::sha256_main(&argv[1..]),
        // `semsql make <name>.db` — the ontology SQL database. Its own
        // subcommand is spelled `make`, which is also one of owlmake's, so the
        // line has to be read whole rather than split at the chaining harness.
        Some("semsql") => return crate::cmd::semsql::main(&argv[1..]),
        _ => {}
    }
    // `owlmake dosdp <generate|terms|…>` uses the subcommand grammar; the bare
    // `dosdp --pattern … --data …` form falls through to clap below.
    if argv.first().map(String::as_str) == Some("dosdp") {
        let rest = &argv[1..];
        if rest.is_empty() || is_help_token(rest.first()) {
            crate::dosdp::print_cli_help();
            return 0;
        }
        if let Some(sub) = argv.get(1) {
            if crate::dosdp::is_subcommand(sub) {
                return crate::dosdp::cli_main(&argv[1..]);
            }
        }
    }
    // SSSOM steps are spelled `sssom:<command>`; route a *standalone* call to the
    // SSSOM CLI. The three commands that act on the ontology flowing through a
    // chain — `sssom:rename`, `sssom:inject`, `sssom:xref-extract` — are handled
    // inside `run_chain`, which runs the clap commands ahead of them and hands the
    // resulting model over. A global option written in front of such a step
    // (`--catalog catalog-v001.xml sssom:xref-extract …`) is *not* hoisted onto it:
    // hoisting fires only when a clap subcommand name follows the globals, and a
    // `sssom:` token keeps its prefix. Write the option on a clap command earlier in
    // the chain if the step needs it.
    if let Some(sub) = argv.first().and_then(|a| a.strip_prefix("sssom:")) {
        if !PLUGIN_CHAIN_STEPS.contains(&sub) {
            let mut args = vec![sub.to_string()];
            args.extend_from_slice(&argv[1..]);
            return crate::sssom::main(&args);
        }
    }
    match run_chain(&argv) {
        Ok(()) => 0,
        Err(e) => {
            // Match the binary's anyhow error rendering on stderr.
            eprintln!("Error: {e:?}");
            1
        }
    }
}

/// [`run_argv`] on a worker thread with a large stack.
///
/// The DL tableau's model search recurses one frame per nondeterministic branch,
/// which on large ontologies can exceed the default 8 MiB stack; the binary runs
/// everything on a generously-sized stack for this reason, and so must any other
/// embedding (the Python extension) that may invoke `reason`/`explain`. The
/// stack is virtual (only paged in as used), so the reservation is cheap, but
/// some environments refuse a very large one — fall back through smaller stacks
/// and finally the current thread. All concept interning / Rc state is
/// thread-local, so the whole run stays on whichever single thread is used. The
/// ceiling is overridable with `OWLMAKE_STACK_GIB`.
pub fn run_argv_main(argv: Vec<String>) -> i32 {
    const GIB: usize = 1024 * 1024 * 1024;
    let want = std::env::var("OWLMAKE_STACK_GIB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|g| g.saturating_mul(GIB))
        .unwrap_or(8 * GIB);
    for size in [want, 4 * GIB, 2 * GIB, GIB] {
        if size == 0 || size > want {
            continue;
        }
        let argv = argv.clone();
        match std::thread::Builder::new()
            .stack_size(size)
            .spawn(move || run_argv(argv))
        {
            Ok(handle) => return handle.join().expect("owlmake worker thread panicked"),
            // Reservation refused (e.g. EAGAIN) — try a smaller stack.
            Err(_) => continue,
        }
    }
    // Last resort: run on the current thread with whatever stack the OS gave us.
    run_argv(argv)
}

/// How many values a flag consumes when splitting a chained command line.
/// `max` is the maximum (`usize::MAX` for an unbounded variadic). `greedy` is
/// set for variadic (`num_args = 1..`) and *ranged* (`num_args = 1..=2`, e.g.
/// `--query <FILE> [OUTPUT]`) flags: they consume up to `max` following tokens
/// but stop at the next flag or subcommand name. Strict fixed-N flags
/// (`--annotation PROP VALUE`) are not greedy — they take exactly `max` tokens,
/// even if a value happens to look like a flag.
#[derive(Clone, Copy)]
struct FlagArity {
    max: usize,
    greedy: bool,
}

/// Per-subcommand map: flag token (e.g. `--reasoner`, `-r`) → value arity.
type CommandFlags = HashMap<String, FlagArity>;

/// The ontology-operation subcommands `owlmake robot` summarises, printed in the
/// order given here. Entries are kebab-case clap subcommand names held in
/// alphabetical order, so a new one goes at its alphabetical position. The
/// repository-build commands (make, release, import, seed, …) are intentionally
/// omitted — they show in the full `owlmake --help`.
const ROBOT_COMMANDS: &[&str] = &[
    "annotate",
    "collapse",
    "convert",
    "diff",
    "expand",
    "explain",
    "export",
    "export-prefixes",
    "extract",
    "filter",
    "materialize",
    "measure",
    "merge",
    "merge-equivalent-sets",
    "mirror",
    "query",
    "reason",
    "reduce",
    "relax",
    "remove",
    "rename",
    "repair",
    "report",
    "template",
    "unmerge",
    "validate-profile",
    "verify",
];

/// Print the command summary for `owlmake robot` (and `owlmake robot --help`):
/// the names listed in `ROBOT_COMMANDS`, each with the description looked up in
/// the clap command tree, so a description can never drift from the command's own
/// definition. The set of names does not come from the tree: a name that no longer
/// matches a subcommand is skipped silently, and a newly added command stays out
/// of the summary until it is added to the const as well.
pub fn print_robot_help() {
    use clap::CommandFactory;
    use owo_colors::{OwoColorize, Style};

    let color = crate::progress::use_color();
    let paint = |s: &str, style: Style| -> String {
        if color {
            s.style(style).to_string()
        } else {
            s.to_string()
        }
    };

    let root = Cli::command();
    let about: HashMap<String, String> = root
        .get_subcommands()
        .map(|c| {
            (
                c.get_name().to_string(),
                c.get_about().map(|a| a.to_string()).unwrap_or_default(),
            )
        })
        .collect();

    println!(
        "{}",
        paint("om robot — ROBOT-compatible OWL command-line operations", Style::new().bold())
    );
    println!("a native Rust reimplementation of ROBOT's commands (not the original Java tool)\n");
    println!("Usage: om <command> [options]");
    println!("       om robot <command> [options]   (the leading `robot` is optional)\n");
    println!("Commands ROBOT users will recognise:");
    for name in ROBOT_COMMANDS {
        if let Some(desc) = about.get(*name) {
            println!("  {:<22} {desc}", paint(name, Style::new().cyan().bold()));
        }
    }
    println!("\nCommands can be chained, sharing one in-memory ontology, e.g.:");
    println!("  om merge -i a.owl -i b.owl reason --reasoner ELK reduce -o out.owl\n");
    println!("Run `om <command> --help` for a command's options.");
    println!("Run `om --help` for every command, including owlmake/ODK extensions.");
}

/// Entry point: split `argv` (without the program name) into chained commands and
/// execute them, threading one `Model`.
pub fn run_chain(argv: &[String]) -> Result<()> {
    let (names, flags) = introspect();

    // Plugin-style commands arrive written `prefix:command` (`odk:subset`,
    // `uberon:create-species-subset`, `kgcl:mint`, `upheno:extract-upheno-relations`);
    // owlmake exposes them un-prefixed (the bare command). Rewrite such a command
    // token to its bare form when the bare name is a real subcommand, so a chained
    // recipe line like `uberon:create-species-subset … reason … convert …`
    // segments correctly. The "must be a known command" guard keeps values such as
    // `--prefix 'uberon: …'` or `--subset-name uberon:human_subset` untouched.
    // (`sssom:` keeps its prefix — it routes to the bundled SSSOM engine, handled
    // separately.)
    let normalized: Vec<String> = argv
        .iter()
        .map(|tok| {
            tok.split_once(':')
                .filter(|(p, bare)| {
                    matches!(*p, "odk" | "uberon" | "kgcl" | "upheno") && names.contains_key(*bare)
                })
                .map(|(_, bare)| bare.to_string())
                .unwrap_or_else(|| tok.clone())
        })
        .collect();
    let argv: &[String] = &normalized;

    // GLOBAL option placement — the option written ahead of the first command:
    // `--catalog x.xml merge -i … reason …`. Recipes are written that way, and
    // owlmake's chained parser expects the options on the command they belong to —
    // so without this `om --catalog … merge …` reads as `om make --catalog …` and
    // fails, or (when the recipe is shelled out) silently loses the catalog and
    // lets `resolve_import_closure` fall through to the NETWORK. MONDO's
    // `tmp/rare-subset-pre.owl` then builds against the *published*
    // `merged_import.owl` instead of the committed one, and every
    // `subsets/mondo-rare.*` artefact differs.
    //
    // Hoist the leading globals onto the first command instead. Only the tokens
    // listed in `GLOBAL_OPTIONS` move — the long forms and the `-v`/`-vv`/`-vvv`
    // short forms alike — and only when a recognised command follows them, so
    // `om --help` / `om --version` and a bare `om` are left alone.
    let hoisted: Vec<String>;
    let argv: &[String] = match hoist_global_options(argv, &names) {
        Some(v) => {
            hoisted = v;
            &hoisted
        }
        None => argv,
    };

    // Bare `om` outside a buildable ontology directory: show the full command help
    // (the same as `om --help`, listing every command) rather than erroring on a
    // missing build setup. (Running `om` inside an ontology repo still defaults to
    // `make` and builds it — see below.)
    if argv.is_empty() && !crate::odk::has_buildable_setup(std::path::Path::new(".")) {
        let _ = <Cli as clap::CommandFactory>::command().print_help();
        return Ok(());
    }

    // Bare `owlmake` — and `owlmake <args>` where the first token is neither a
    // known subcommand nor a top-level `--help`/`--version` — defaults to the
    // `make` builder (whose `repo` defaults to `.`), so running owlmake from
    // inside an ontology repo just builds it. An explicit subcommand, and the
    // global help/version flags, are left untouched.
    let owned: Vec<String>;
    let argv: &[String] = if default_to_make(argv, &names) {
        let mut v = Vec::with_capacity(argv.len() + 1);
        v.push("make".to_string());
        v.extend_from_slice(argv);
        owned = v;
        &owned
    } else {
        argv
    };

    // An SSSOM chain command (`sssom:inject`, `sssom:xref-extract`) chained
    // after ontology commands (`merge … sssom:inject …`): run the clap commands
    // before it to produce the in-flight ontology, then hand that model to the
    // SSSOM OWL-chain handler. These commands are terminal in the recipes that use
    // them (they write a mapping file or bridge ontologies and/or `-o` the result),
    // so everything from the `sssom:` token onward is the SSSOM segment.
    if let Some(k) = argv.iter().position(|t| t.starts_with("sssom:")) {
        let sub = argv[k].trim_start_matches("sssom:").to_string();
        // A standalone SSSOM CLI command that reached here because globals
        // preceded it — they have just been hoisted onto it, so it now leads the
        // argv exactly as the bare spelling does.
        if k == 0 && !PLUGIN_CHAIN_STEPS.contains(&sub.as_str()) {
            let mut args = vec![sub.clone()];
            args.extend_from_slice(&argv[1..]);
            if crate::sssom::main(&args) != 0 {
                bail!("sssom {sub} failed");
            }
            return Ok(());
        }
        // `sssom:rename` is a *producing* step: it reads/transforms the ontology
        // and the chain continues (`sssom:rename … remove … convert …`). Split its
        // own option segment off (up to the next chained command), apply it, then
        // run the remainder with the renamed model as the initial state.
        if sub == "rename" {
            let pre = if k > 0 { run_clap_chain(&argv[..k], &names, &flags, None)? } else { None };
            let mut end = k + 1;
            while end < argv.len()
                && !names.contains_key(&argv[end])
                && !argv[end].starts_with("sssom:")
            {
                end += 1;
            }
            if end >= argv.len() {
                // Terminal rename: `chain_step` applies it and honours its own -o.
                return crate::sssom::owl::chain_step(pre, &sub, &argv[k + 1..]);
            }
            let renamed = crate::sssom::owl::rename(pre, &argv[k + 1..end])?;
            return run_clap_chain(&argv[end..], &names, &flags, Some(renamed)).map(|_| ());
        }
        // Terminal SSSOM step (`sssom:inject`, `sssom:xref-extract`), possibly
        // preceded by ontology commands (`merge … sssom:inject …`); k may be 0
        // for a standalone `sssom:inject -i …`.
        let model = if k > 0 { run_clap_chain(&argv[..k], &names, &flags, None)? } else { None };
        return crate::sssom::owl::chain_step(model, &sub, &argv[k + 1..]);
    }

    run_clap_chain(argv, &names, &flags, None).map(|_| ())
}

/// Segment `argv` into chained command segments, parse each with clap, and
/// run them threading the ontology model through. Returns the final model (the
/// value a following chained command — e.g. a trailing SSSOM-plugin step — would
/// consume).
fn run_clap_chain(
    argv: &[String],
    names: &HashMap<String, ()>,
    flags: &HashMap<String, CommandFlags>,
    initial: Option<Model>,
) -> Result<Option<Model>> {
    let segments = segment(argv, names, flags)?;
    if segments.is_empty() {
        // No subcommand: either thread through a producing predecessor's model
        // (e.g. a leading `sssom:rename`) or let clap render its help/error.
        if initial.is_some() {
            return Ok(initial);
        }
        Cli::parse();
        return Ok(None);
    }

    let mut state: Option<Model> = initial;
    for seg in segments {
        // `--help`/`--version` inside a segment: let clap print to the right
        // stream and exit with the right code (0 for help/version), exactly as a
        // normal clap CLI would, instead of surfacing them as errors.
        let cli = match Cli::try_parse_from(std::iter::once("owlmake".to_string()).chain(seg)) {
            Ok(cli) => cli,
            Err(e) => e.exit(),
        };
        state = dispatch(state, cli.command)?;
    }
    Ok(state)
}

/// Emit the complete CLI surface (every subcommand and every flag, with arity,
/// value names, possible values, defaults and help) as a JSON document. This is
/// the single source of truth the language bindings (Python, and later the
/// WASM/JS layer) are generated from, so it can never drift from the real clap
/// definitions: it is produced *by introspecting the same `clap::Command` tree*
/// the binary parses with. Printed by the hidden `owlmake __cli-spec` command.
pub fn dump_cli_spec() -> String {
    use clap::ArgAction;
    use serde_json::{json, Value};

    let root = Cli::command();
    let mut commands: Vec<Value> = Vec::new();

    for sub in root.get_subcommands() {
        // `jq` and `sssom` are intercepted before clap and carry only an opaque
        // trailing-var-arg placeholder; their real surfaces are described
        // separately (sssom by the binding generator, jq as a raw passthrough),
        // so emit them as passthrough markers rather than a misleading single arg.
        let name = sub.get_name().to_string();
        let passthrough = matches!(name.as_str(), "jq" | "sssom");

        let mut args: Vec<Value> = Vec::new();
        if !passthrough {
            for arg in sub.get_arguments() {
                let action = match arg.get_action() {
                    ArgAction::SetTrue => "set_true",
                    ArgAction::SetFalse => "set_false",
                    ArgAction::Count => "count",
                    ArgAction::Append => "append",
                    ArgAction::Set => "set",
                    ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong => "help",
                    ArgAction::Version => "version",
                    _ => "set",
                };
                // Auto-generated --help/--version aren't part of the API surface.
                if action == "help" || action == "version" {
                    continue;
                }

                let longs: Vec<String> = arg
                    .get_long_and_visible_aliases()
                    .map(|v| v.into_iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default();
                let shorts: Vec<String> = arg
                    .get_short_and_visible_aliases()
                    .map(|v| v.into_iter().map(|c| c.to_string()).collect())
                    .unwrap_or_default();
                let value_names: Vec<String> = arg
                    .get_value_names()
                    .map(|v| v.iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default();
                let (min_values, max_values) = arg
                    .get_num_args()
                    .map(|r| (r.min_values(), r.max_values()))
                    .unwrap_or((1, 1));
                let possible_values: Vec<String> = arg
                    .get_possible_values()
                    .iter()
                    .map(|p| p.get_name().to_string())
                    .collect();
                let defaults: Vec<String> = arg
                    .get_default_values()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect();
                let help = arg.get_help().map(|h| h.to_string());

                args.push(json!({
                    "id": arg.get_id().as_str(),
                    "longs": longs,
                    "shorts": shorts,
                    "action": action,
                    "value_names": value_names,
                    "min_values": min_values,
                    // usize::MAX marks an unbounded (variadic) flag.
                    "max_values": max_values,
                    "variadic": max_values == usize::MAX,
                    "required": arg.is_required_set(),
                    "possible_values": possible_values,
                    "defaults": defaults,
                    "hidden": arg.is_hide_set(),
                    "help": help,
                }));
            }
        }

        let aliases: Vec<String> =
            sub.get_visible_aliases().map(|s| s.to_string()).collect();
        commands.push(json!({
            "name": name,
            "aliases": aliases,
            "about": sub.get_about().map(|a| a.to_string()),
            "passthrough": passthrough,
            "args": args,
        }));
    }

    let doc = json!({
        "program": "owlmake",
        "version": root.get_version().unwrap_or(env!("CARGO_PKG_VERSION")),
        "about": root.get_about().map(|a| a.to_string()),
        "commands": commands,
    });
    serde_json::to_string_pretty(&doc).expect("serialize cli spec")
}

/// Build the set of subcommand names and, for each, its flag→arity map by
/// introspecting the derived clap command tree.
fn introspect() -> (HashMap<String, ()>, HashMap<String, CommandFlags>) {
    let mut names: HashMap<String, ()> = HashMap::new();
    let mut per_cmd: HashMap<String, CommandFlags> = HashMap::new();

    let root = Cli::command();
    for sub in root.get_subcommands() {
        let mut flags: CommandFlags = HashMap::new();
        for arg in sub.get_arguments() {
            let arity: FlagArity = match arg.get_action() {
                ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count => {
                    FlagArity { max: 0, greedy: false }
                }
                ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong => {
                    FlagArity { max: 0, greedy: false }
                }
                ArgAction::Version => FlagArity { max: 0, greedy: false },
                _ => {
                    let (min, max) = arg
                        .get_num_args()
                        .map(|r| (r.min_values(), r.max_values()))
                        .unwrap_or((1, 1));
                    let max = if max == 0 { 1 } else { max };
                    // Variadic (1..) and ranged (1..=2) flags stop at the next
                    // flag/subcommand; strict fixed-N take exactly N.
                    FlagArity { max, greedy: min != max }
                }
            };
            if let Some(longs) = arg.get_long_and_visible_aliases() {
                for l in longs {
                    flags.insert(format!("--{l}"), arity);
                }
            }
            if let Some(shorts) = arg.get_short_and_visible_aliases() {
                for s in shorts {
                    flags.insert(format!("-{s}"), arity);
                }
            }
        }
        let cmd_names = std::iter::once(sub.get_name().to_string())
            .chain(sub.get_visible_aliases().map(|s| s.to_string()));
        for n in cmd_names {
            names.insert(n.clone(), ());
            per_cmd.insert(n, flags.clone());
        }
    }
    (names, per_cmd)
}

/// Whether to implicitly route `argv` to the `make` subcommand. True for bare
/// `owlmake` and for `owlmake <args>` whose first token is not a known
/// subcommand — except the top-level help/version flags, which clap should
/// handle so users still get the full command listing.
fn default_to_make(argv: &[String], names: &HashMap<String, ()>) -> bool {
    match argv.first() {
        None => true,
        Some(first) => {
            !matches!(first.as_str(), "-h" | "--help" | "-V" | "--version")
                && !names.contains_key(first)
                // A leading `sssom:` plugin step (`sssom:rename … remove …`,
                // `sssom:inject …`) is a chain, not a reason to default to `make`.
                && !first.starts_with("sssom:")
        }
    }
}

/// The SSSOM steps that act on the ontology flowing through a ROBOT chain, so
/// they are run by [`run_chain`] rather than by the standalone SSSOM CLI.
const PLUGIN_CHAIN_STEPS: &[&str] = &["rename", "inject", "xref-extract"];

/// The global options, which may precede the first command. Each maps to a
/// field of owlmake's per-command `CommonArgs`, so hoisting them onto the first
/// command is exactly equivalent.
const GLOBAL_OPTIONS: &[(&str, bool)] = &[
    ("--catalog", true),
    ("--prefix", true),
    ("--prefixes", true),
    ("--add-prefix", true),
    ("--add-prefixes", true),
    ("--noprefixes", false),
    ("--strict", false),
    ("--xml-entities", false),
    ("--verbose", false),
    ("-v", false),
    ("-vv", false),
    ("-vvv", false),
];

/// Move any global options that precede the first command onto that
/// command. Returns `None` when there is nothing to hoist (so the caller keeps the
/// original slice), including when no recognised command follows them.
fn hoist_global_options(argv: &[String], names: &HashMap<String, ()>) -> Option<Vec<String>> {
    let mut globals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let tok = argv[i].as_str();
        let (name, inline) = match tok.split_once('=') {
            Some((n, _)) => (n, true),
            None => (tok, false),
        };
        let Some((_, takes_value)) = GLOBAL_OPTIONS.iter().find(|(g, _)| *g == name) else {
            break;
        };
        globals.push(argv[i].clone());
        i += 1;
        if *takes_value && !inline {
            if i >= argv.len() {
                return None; // malformed — let clap report it
            }
            globals.push(argv[i].clone());
            i += 1;
        }
    }
    // A `sssom:` step is a command here too: `$(ROBOT)` expands to
    // `robot --catalog catalog-v001.xml`, so every recipe line that calls one
    // arrives with the globals in front of it, and `load_model` reads the
    // catalog off the step's own arguments.
    if globals.is_empty() || i >= argv.len() {
        return None;
    }
    let is_command = names.contains_key(&argv[i]) || argv[i].starts_with("sssom:");
    if !is_command {
        return None;
    }
    // `<command> <hoisted globals> <the command's own arguments> …`
    let mut out: Vec<String> = vec![argv[i].clone()];
    out.extend(globals);
    out.extend(argv[i + 1..].iter().cloned());
    Some(out)
}

/// Split argv into one token list per chained command.
fn segment(
    argv: &[String],
    names: &HashMap<String, ()>,
    flags: &HashMap<String, CommandFlags>,
) -> Result<Vec<Vec<String>>> {
    // A leading global flag (`--help`, `--version`) is not a chain; defer to clap.
    if argv.is_empty() || argv[0].starts_with('-') {
        return Ok(Vec::new());
    }

    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let name = &argv[i];
        if !names.contains_key(name) {
            bail!("expected a command, found '{name}'");
        }
        let cmd_flags = &flags[name];
        // `make` is a terminal builder (it does not thread a model to a following
        // command) and takes positional TARGETS that may coincide with subcommand
        // names (`refresh-imports`, `test`, `patterns`, …). So once in a `make`
        // segment, consume the rest as its targets rather than starting a new
        // command — otherwise `make refresh-imports` would split into `make` (no
        // targets → full release) + `refresh-imports`.
        let greedy_targets = name == "make";
        let mut seg = vec![name.clone()];
        i += 1;

        while i < argv.len() {
            let tok = &argv[i];
            if tok.starts_with('-') && tok.as_str() != "-" {
                seg.push(tok.clone());
                i += 1;
                // `--flag=value` carries its own value; nothing to consume.
                if tok.contains('=') {
                    continue;
                }
                let arity = flag_arity(cmd_flags, tok);
                if arity.greedy {
                    // Variadic (1..) or ranged (1..=N, e.g. `--query <FILE> [OUTPUT]`):
                    // take up to `max` following tokens, stopping at the next flag
                    // (other than a bare `-`) or a command name.
                    let mut taken = 0;
                    while taken < arity.max
                        && i < argv.len()
                        && (argv[i] == "-" || !argv[i].starts_with('-'))
                        && !names.contains_key(&argv[i])
                    {
                        seg.push(argv[i].clone());
                        i += 1;
                        taken += 1;
                    }
                } else {
                    // Strict fixed arity: take exactly that many (clap validates the
                    // rest) — values may legitimately look like flags here.
                    let mut taken = 0;
                    while taken < arity.max && i < argv.len() {
                        seg.push(argv[i].clone());
                        i += 1;
                        taken += 1;
                    }
                }
            } else if names.contains_key(tok) && !greedy_targets {
                break; // start of the next command
            } else {
                // owlmake commands have no positionals; a bare token here is a
                // stray value (e.g. an extra variadic value) — keep it with this
                // command and let clap accept or reject it.
                seg.push(tok.clone());
                i += 1;
            }
        }
        segments.push(seg);
    }
    Ok(segments)
}

/// Look up a flag's value arity, normalizing combined short forms (`-rELK`) and
/// unknown flags to "consumes no separate value".
fn flag_arity(cmd_flags: &CommandFlags, tok: &str) -> FlagArity {
    if let Some(&a) = cmd_flags.get(tok) {
        return a;
    }
    // `-rELK` → short flag with attached value: consumes nothing extra.
    FlagArity { max: 0, greedy: false }
}

/// Run one parsed command on the threaded state, returning the model to pass on.
fn dispatch(state: Option<Model>, command: Command) -> Result<Option<Model>> {
    Ok(match command {
        Command::Convert(a) => cmd::convert::step(state, &a)?,
        Command::Merge(a) => cmd::merge::step(state, &a)?,
        Command::MergeEquivalentSets(a) => cmd::merge_equivalent_sets::step(state, &a)?,
        Command::Unmerge(a) => cmd::unmerge::step(state, &a)?,
        Command::Mint(a) => cmd::mint::step(state, &a)?,
        Command::Ogrep(a) => cmd::ogrep::step(state, &a)?,
        Command::Mirror(a) => cmd::mirror::step(state, &a)?,
        Command::Import(a) => cmd::import_module::step(state, &a)?,
        Command::Reason(a) => cmd::reason::step(state, &a)?,
        Command::Reduce(a) => cmd::reduce::step(state, &a)?,
        Command::Relax(a) => cmd::relax::step(state, &a)?,
        Command::Materialize(a) => cmd::materialize::step(state, &a)?,
        Command::Normalize(a) => cmd::normalize::step(state, &a)?,
        Command::Babelon(a) => cmd::babelon::step(state, &a)?,
        Command::RewriteDef(a) => cmd::rewrite_def::step(state, &a)?,
        Command::Diff(a) => cmd::diff::step(state, &a)?,
        Command::Annotate(a) => cmd::annotate::step(state, &a)?,
        Command::Filter(a) => cmd::filter::step(state, &a)?,
        Command::Remove(a) => cmd::remove::step(state, &a)?,
        Command::Extract(a) => cmd::extract::step(state, &a)?,
        Command::ExtractStrings(a) => cmd::extract_strings::step(state, &a)?,
        Command::ExtractUphenoRelations(a) => cmd::extract_upheno_relations::step(state, &a)?,
        Command::Embeddings(a) => cmd::embeddings::step(state, &a)?,
        Command::TextTagger(a) => cmd::text_tagger::step(state, &a)?,
        Command::Map(a) => cmd::map::step(state, &a)?,
        Command::Lexmatch(a) => cmd::lexmatch::step(state, &a)?,
        Command::Kgx(a) => cmd::kgx::step(state, &a)?,
        Command::Measure(a) => cmd::measure::step(state, &a)?,
        Command::InformationContent(a) => cmd::information_content::step(state, &a)?,
        Command::ValidateProfile(a) => cmd::validate_profile::step(state, &a)?,
        Command::ValidateIdRanges(a) => cmd::validate_id_ranges::step(state, &a)?,
        Command::ValidatePatterns(a) => cmd::validate_patterns::step(state, &a)?,
        Command::CheckRdfxml(a) => cmd::check_rdfxml::step(state, &a)?,
        Command::Semsql(_) => unreachable!("`owlmake semsql` is handled before chaining"),
        Command::Sha256Sum(a) => cmd::config_check::sha256_step(state, &a)?,
        Command::ConfigCheck(a) => cmd::config_check::config_check_step(state, &a)?,
        Command::OdkInfo(a) => cmd::config_check::odk_info_step(state, &a)?,
        Command::Query(a) => cmd::query::step(state, &a)?,
        Command::Verify(a) => cmd::verify::step(state, &a)?,
        Command::Report(a) => cmd::report::step(state, &a)?,
        Command::Template(a) => cmd::template::step(state, &a)?,
        Command::Rename(a) => cmd::rename::step(state, &a)?,
        Command::Export(a) => cmd::export::step(state, &a)?,
        Command::ExportPrefixes(a) => cmd::export_prefixes::step(state, &a)?,
        Command::Release(a) => cmd::release::step(state, &a)?,
        Command::Ubergraph(a) => cmd::ubergraph::step(state, &a)?,
        Command::Make(a) => cmd::make::step(state, &a)?,
        Command::PrepareRelease(a) => {
            cmd::make::prepare_release(&a)?;
            None
        }
        Command::RefreshImports(a) => {
            cmd::make::refresh_imports(&a)?;
            None
        }
        Command::AllImports(a) => {
            cmd::make::all_imports(&a)?;
            None
        }
        Command::Test(a) => {
            cmd::make::test(&a)?;
            None
        }
        Command::Seed(a) => cmd::seed::step(state, &a)?,
        Command::Schema(a) => cmd::schema::step(state, &a)?,
        Command::Explain(a) => cmd::explain::step(state, &a)?,
        Command::Repair(a) => cmd::repair::step(state, &a)?,
        Command::Collapse(a) => cmd::collapse::step(state, &a)?,
        Command::Expand(a) => cmd::expand::step(state, &a)?,
        Command::Subset(a) => cmd::subset::step(state, &a)?,
        Command::ExtractOntologySubset(a) => cmd::owltools_ops::step_subset(state, &a)?,
        Command::ExtractMingraph(a) => cmd::owltools_ops::step_mingraph(state, &a)?,
        Command::RemoveAxiomAnnotations(a) => cmd::owltools_ops::step_remove_axiom_annotations(state, &a)?,
        Command::MakeSubsetByProperties(a) => cmd::owltools_ops::step_make_subset_by_properties(state, &a)?,
        Command::MergeSpecies(a) => cmd::merge_species::step(state, &a)?,
        Command::CreateSpeciesSubset(a) => cmd::create_species_subset::step(state, &a)?,
        Command::Dosdp(a) => cmd::dosdp::step(state, &a)?,
        // `jq` is intercepted in `main` before the chain harness ever runs, so
        // this arm is never reached; it exists only to satisfy the match.
        Command::Jq(_) => unreachable!("`owlmake jq` is handled before chaining"),
        // Likewise `sssom` is intercepted in `main`; this arm only satisfies the match.
        Command::Sssom(_) => unreachable!("`owlmake sssom` is handled before chaining"),
        // The bundled text utilities are intercepted in `main` too; these arms
        // exist only to satisfy the match.
        Command::Sed(_) => unreachable!("`owlmake sed` is handled before chaining"),
        Command::Grep(_) => unreachable!("`owlmake grep` is handled before chaining"),
        Command::Comm(_) => unreachable!("`owlmake comm` is handled before chaining"),
    })
}
