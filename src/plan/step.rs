//! The vocabulary a plan is written in: the operations a build step can be, and
//! the steps themselves.
//!
//! This is contract, not ingest. A plan names these whether it was written by
//! ingest ([`crate::odk`]) or by hand in `owlmake.yaml`, and the executor
//! ([`crate::build`]) knows only these — never where they came from.

use crate::build::recipe::FileOp;

/// A mapped, executable operation: one stage of a pipeline, threading the
/// in-flight ontology model.
#[derive(Debug, Clone)]
pub enum Op {
    /// `merge` — merge the explicit `--input` files (and their import closures,
    /// resolved via the catalog) into the current ontology.
    Merge {
        inputs: Vec<String>,
        /// `--collapse-import-closure` (default true): merge the import
        /// closure in and drop the `owl:imports` declarations. When set to false
        /// (MONDO's `filtered.owl`), the imports are kept as declarations and
        /// their axioms are NOT merged — they remain a read-only reasoning closure
        /// until a later collapsing merge.
        collapse_import_closure: Option<bool>,
        /// Whether this merge STARTS the pipeline: the model it leaves is its
        /// inputs alone, not its inputs merged into whatever came before.
        ///
        /// A recipe's every command line is its own invocation, so a line that
        /// opens with `merge --input` opens a new pipeline over that input.
        /// uPheno's `components/upheno-bridge.owl` is two of them — the first
        /// constructs `tmp/bridge.ttl` from the edit file and a mapping set, the
        /// second builds the component from `tmp/bridge.ttl` alone — and carrying
        /// the first line's model into the second put the whole edit file and its
        /// import closure into the component, 158 MB against 8.
        restart: bool,
    },
    /// `unmerge` — remove a second ontology's axioms from the current one.
    Unmerge {
        second_input: Option<String>,
    },
    Reason {
        reasoner: Option<String>,
        equivalent_classes_allowed: Option<String>,
        exclude_tautologies: Option<String>,
        annotate_inferred_axioms: Option<bool>,
        /// owlmake's own flag (default false): continue past unsatisfiable classes
        /// instead of failing. A release built over an incoherent ontology is
        /// silently wrong rather than obviously wrong — every subclass axiom of an
        /// unsatisfiable class is redundant, so a later `reduce` strips the
        /// hierarchy — so this must be opted into per recipe, never assumed.
        allow_incoherent: Option<bool>,
        /// `-X`/`--exclude-external-entities`: do not assert inferred axioms
        /// whose subject is an external (imported) entity. MONDO's `reasoned.owl`
        /// sets this so only MONDO classifications are added.
        exclude_external_entities: Option<bool>,
        /// `-T`/`--exclude-owl-thing`: do not assert `X ⊑ owl:Thing`
        /// subsumptions. MONDO's `reasoned.owl` sets `-T true`, so its reasoned
        /// hierarchy carries no trivial owl:Thing parents (~4.8k for MONDO).
        exclude_owl_thing: Option<bool>,
        /// `-s`/`--remove-redundant-subclass-axioms` (default true): run a
        /// `reduce` after asserting. EFO sets `-s true` explicitly.
        remove_redundant_subclass_axioms: Option<bool>,
        /// `-n`/`--create-new-ontology` (default false): output ONLY the
        /// inferred axioms in a fresh ontology.
        create_new_ontology: Option<bool>,
        /// `-m`/`--create-new-ontology-with-annotations` (default false):
        /// like `-n`, also copying entity annotations. EFO sets `-m false`.
        create_new_ontology_with_annotations: Option<bool>,
        /// `-x`/`--exclude-duplicate-axioms` (default false): do not assert
        /// an inferred axiom that is already present.
        exclude_duplicate_axioms: Option<bool>,
    },
    Relax {
        /// `relax --include-subclass-of` (default false): also weaken
        /// standalone `SubClassOf(C, R exactly|min n F)` axioms to existentials.
        /// The default relaxes ONLY EquivalentClasses, so this is off unless
        /// the recipe sets it (e.g. UBERON's `relax --include-subclass-of true`).
        include_subclass_of: bool,
    },
    Reduce {
        reasoner: Option<String>,
        /// `reduce --include-subproperties` (default false): also let a
        /// sub-property existential dominate (`R' ⊑ R` so `∃R'.F ⊑ ∃R.F`). Off
        /// unless the recipe sets it (e.g. UBERON's
        /// `REDUCE_OPTIONS = --include-subproperties true`).
        include_subproperties: Option<bool>,
    },
    Materialize {
        properties: Vec<String>,
        term_files: Vec<String>,
    },
    Remove(RemoveSpec),
    Filter(FilterSpec),
    Annotate(AnnotateSpec),
    Convert {
        format: Option<String>,
        clean_obo: Option<String>,
        /// The step's OWN `-o/--output`, when it names one. A single rule can build
        /// two files at once — MONDO's `mondo.owl` rule writes the artefact with
        /// `annotate … -o` and then a SECOND file with `convert -f ofn -o
        /// tmp/mondo.owl.ofn` — so an explicit `--format` may belong to a
        /// different output than the one being built.
        output: Option<String>,
        /// `--add-prefixes FILE` (repeatable): JSON-LD context files whose
        /// prefixes are added to the model's map, so the OFN/OBO output declares
        /// AND abbreviates with them (e.g. MONDO's `config/prefixes.jsonld` binds
        /// `Orphanet:` → `http://www.orpha.net/ORDO/Orphanet_`). Without them the
        /// OFN abbreviates `Orphanet:377788` while declaring no such prefix, and a
        /// downstream re-read expands it to `obo:Orphanet_377788`.
        add_prefixes: Vec<String>,
    },
    /// `query` — SPARQL `--update` (transforms the model) and/or `--query`/
    /// `--select`/`--construct FILE OUTPUT` (writes a result file). owlmake runs
    /// these through its in-memory SPARQL engine.
    Query {
        updates: Vec<String>,
        /// `(query_file, output_file)` for SELECT (`--query`/`--select`).
        selects: Vec<(String, String)>,
        /// `(query_file, output_file)` for `--construct`.
        constructs: Vec<(String, String)>,
        format: Option<String>,
        /// `-g,--use-graphs`: query the root ontology UNIONED with its
        /// import closure rather than the root's own axioms. It decides which
        /// triples the query sees, so it is part of what the recipe produces —
        /// without it ECTO's `custom_reports` yields 24 rows where the closure
        /// yields 19,677.
        use_graphs: bool,
        /// `-t,--tdb`: the query is evaluated in memory either way, so this decides
        /// only the ROW ORDER of a `SELECT` with no `ORDER BY` — set, the rows come
        /// back in each term's order of first appearance in the input document;
        /// unset, in the in-memory store's own order. It permutes all 36,080 rows
        /// of MONDO's `reports/mondo_base_current_release-report.tsv`, and that
        /// ordering propagates into the release artefact
        /// `reports/mondo_release_diff_changed_terms.tsv`.
        tdb: bool,
    },
    Repair {
        invalid_references: bool,
        merge_axiom_annotations: bool,
    },
    /// `upheno:extract-upheno-relations` — materialise uPheno's phenotype
    /// shortcut relations (`UPHENO:0000001`/`0000003`/`0000002`) from the EQ
    /// definitions of the classes the roots reach. An op rather than a CLI
    /// command because uPheno chains it between `merge` and `remove`, so it has
    /// to thread the model.
    ExtractUphenoRelations {
        relations: Vec<String>,
        terms: Vec<String>,
        term_files: Vec<String>,
        roots: Vec<String>,
        root_files: Vec<String>,
    },
    /// `mint` (KGCL `kgcl:mint`) — replace temporary IDs with definitive ones
    /// drawn from a named ID range, as an `allocate-definitive-ids` target does.
    Mint {
        temp_id_prefix: String,
        id_range_name: String,
        id_ranges: Option<String>,
    },
    /// `collapse` — remove intermediate classes with fewer than `threshold`
    /// named subclasses, bridging the hierarchy across them.
    Collapse {
        precious: Vec<String>,
        precious_files: Vec<String>,
        threshold: Option<usize>,
    },
    /// `normalize` (recipes spell it `odk:normalize`) — inject subset /
    /// synonym-type subproperty declarations.
    Normalize {
        base_iris: Vec<String>,
        subset_decls: bool,
        synonym_decls: bool,
        /// `--add-source`: annotate the ontology with `dc:source <version IRI>`.
        /// Every import module carries one, so without the flag a module loses its
        /// provenance annotation.
        add_source: bool,
    },
    /// `template` — generate axioms from template tables (a row of template
    /// strings over a table of terms) and merge them in.
    Template {
        templates: Vec<String>,
        merge: bool,
        /// ROBOT `--prefix "foo: http://bar"` bindings, in argv order. These
        /// resolve the CURIEs in the template's own header directives, so they
        /// change which IRI each column asserts.
        prefixes: Vec<String>,
    },
    /// `rename` — rewrite entity IRIs from a `old<TAB>new` (or prefix) mapping.
    Rename {
        mappings: Option<String>,
        prefix_mappings: Option<String>,
        allow_missing: bool,
    },
    /// `extract` — extract a module for a seed term set.
    Extract {
        method: String,
        terms: Vec<String>,
        term_files: Vec<String>,
        copy_ontology_annotations: bool,
        individuals: Option<String>,
        /// ROBOT MIREOT `--branch-from-term`/`--branch-from-terms`: each root
        /// pulls in its whole descendant subtree. UBERON's `tmp/xao-ls-bridged.owl`
        /// is `extract --method MIREOT --branch-from-term XAO:1000000`, and
        /// dropping the root left the extract with no seed at all.
        branch_from_terms: Vec<String>,
        branch_from_term_files: Vec<String>,
    },
    /// Write the in-flight model to `path` and read it back — the round trip a
    /// recipe performs whenever one command writes a file and the next command
    /// reads that file back.
    ///
    /// It is not bookkeeping: an RDF/XML round trip is lossy in ways that reach the
    /// bytes (the writer derives an `xmlns` block the in-memory model never had, and
    /// re-reading collapses it back). EFO's mondo import writes
    /// `mondo_import.owl.tmp.owl` between its `extract` and its `remove`, and
    /// eliding that write changes the module by 7 MB. The in-memory pipeline elides
    /// round trips by design, so the ones the recipe *depends on* have to be
    /// recorded — otherwise they live only in a command line.
    RoundTrip {
        path: String,
    },
    /// `merge-equivalent-sets` — collapse equivalent-class cliques by IRI-prefix
    /// priority. Raw `PREFIX=SCORE` argument strings.
    MergeEquivalentSets {
        set_prefix: Vec<String>,
        label_prefix: Vec<String>,
        definition_prefix: Vec<String>,
    },
    /// `babelon convert` — read a Babelon translation TSV (relative to the
    /// ontology dir) and emit OWL annotation axioms. A *source* op: it ignores
    /// any piped model and produces a fresh one.
    Babelon {
        input: String,
        /// The recipe's own `-o`. A babelon conversion is a step of its own, so a
        /// later step that names this path reads it back off disk — HPO's
        /// `hp-<lang>.babelon.owl` rule does exactly that, writing `<target>.tmp`
        /// and then merging that file in. The file is therefore written as well as
        /// the model threaded; without it that merge has nothing to read.
        output: Option<String>,
        /// The recipe's `--output-format`. `owl` (the default) converts the table
        /// into annotation axioms; `json` writes the babelon JSON profile, which
        /// is not an ontology at all. Without it, `<ont>-all.babelon.json` receives
        /// OBO Graphs JSON instead of the table.
        format: Option<String>,
    },
    /// `expand` — expand OBO/OWL macros (`IAO:0000424` expandExpressionToType).
    ///
    /// `expand_terms` is the ALLOW-list: with one, only those properties' macros
    /// run. CL builds its taxon views with `expand --expand-term RO:0002161`, and
    /// without the list recorded every macro in `cl-full.owl` ran — including
    /// `RO:0002175`'s, which mints a named witness class per taxon assertion and
    /// put 412 of them into `subsets/human-view.owl`.
    Expand {
        expand_terms: Vec<String>,
        expand_term_files: Vec<String>,
        no_expand_terms: Vec<String>,
        no_expand_term_files: Vec<String>,
    },
    /// `subset` (recipes spell it `odk:subset`) — extract an `oboInOwl:inSubset`
    /// slice (optionally bridging the hierarchy across dropped classes), in either
    /// of its two modes. `subset` alone is inSubset mode; `queries`/`terms`/
    /// `term_files` select QUERY mode, which UBERON uses for all fourteen of its
    /// `*-minimal` subsets (`--query "BFO:0000050 some UBERON:…"`). Recording only
    /// `subset` left those with an empty selector — a step that ran and produced
    /// the wrong ontology instead of failing.
    Subset {
        subset: String,
        #[allow(clippy::struct_field_names)]
        queries: Vec<String>,
        terms: Vec<String>,
        term_files: Vec<String>,
        reasoner: Option<String>,
        ancestors: Option<bool>,
        /// `None` lets the command apply the mode's own default: true in inSubset
        /// mode, false in query mode.
        fill_gaps: Option<bool>,
    },
    /// `uberon:merge-species` — fold species-specific classes into taxon-neutral
    /// ones to build a composite cross-species ontology.
    MergeSpecies {
        batch_file: Option<String>,
        extended: bool,
        gca_translate: bool,
        gca_delete: bool,
        remove_declarations: bool,
        taxon: Option<String>,
        suffix: Option<String>,
        properties: Vec<String>,
        included: Vec<String>,
    },
    /// `flybase:rewrite-def` — regenerate textual definitions (DOT genus–differentia
    /// prose and/or `$sub_PFX:1234` substitution). Used by the FlyBase-family
    /// preprocess step (dpo/fbbt/fbcv/cl).
    RewriteDef(RewriteDefSpec),
    /// The simple/"basic" release variant's core-class subset: keep only classes
    /// in the ontology's own OBO ID-space, dropping axioms that reference external
    /// classes. Every other stage of that release is expressible with the ops
    /// above — `merge → relax → reason → reduce [→ remove --axioms equivalent] →
    /// annotate`, which is what the plan-level `rewrite_oort` pass emits — so this
    /// subset is the one stage that needs an op of its own.
    SimpleSubset {
        /// The ontology's OBO ID-space (e.g. `wbbt`).
        ont_id: String,
    },
    /// `--extract-ontology-subset [--fill-gaps] --subset NAME`: the named
    /// `oboInOwl:inSubset` slice, extended to its full graph-ancestor closure when
    /// `fill_gaps` is set, with whatever the slice leaves dangling pruned (UBERON's
    /// `common-anatomy.owl`).
    ExtractOntologySubset {
        subset: String,
        fill_gaps: bool,
    },
    /// `--extract-mingraph`: reduce the ontology to a minimal graph — class
    /// hierarchy, class labels and the property ontology — dropping every axiom
    /// that references an obsolete class (UBERON's composite `-basic`).
    ExtractMingraph,
    /// `--remove-axiom-annotations`: strip the annotations carried on each axiom,
    /// keeping the axiom itself.
    RemoveAxiomAnnotations,
    /// `--make-subset-by-properties [-f] PROPS…`: drop every axiom using an object
    /// property outside `properties`, first weakening a named-subject existential
    /// `SubClassOf` onto each super-property inside the set (UBERON's composite
    /// `-basic`).
    MakeSubsetByProperties {
        properties: Vec<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct RemoveSpec {
    pub terms: Vec<String>,
    pub term_files: Vec<String>,
    pub axioms: Vec<String>,
    pub selects: Vec<String>,
    pub base_iri: Vec<String>,
    pub trim: Option<bool>,
    pub preserve_structure: Option<bool>,
    /// ROBOT `-e,--exclude-term` / `-E,--exclude-terms`: terms that must SURVIVE
    /// the removal whatever the selectors say. UBERON's `merged-partonomy.owl` is
    /// `remove --exclude-term BFO:0000050 --select object-properties` — drop every
    /// object property EXCEPT part_of. Dropping the exclusion removed part_of too,
    /// leaving 0 `BFO_0000050` restrictions against ROBOT's 15,088 and starving
    /// every `part_of some X` query that reads the file.
    pub exclude_terms: Vec<String>,
    pub exclude_term_files: Vec<String>,
    /// ROBOT `--signature`: match on the axiom's SIGNATURE rather than its terms.
    pub signature: Option<bool>,
    /// ROBOT `--drop-axiom-annotations <selector>` (`all`, `internal`, …).
    pub drop_axiom_annotations: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FilterSpec {
    pub terms: Vec<String>,
    pub term_files: Vec<String>,
    pub selects: Vec<String>,
    pub signature: Option<bool>,
    pub trim: Option<bool>,
    /// ROBOT `--axioms`: keep only these axiom TYPES. UBERON's
    /// `composite-*-basic.owl` is `filter --axioms "subclass equivalent
    /// annotation"`; recording the step with no axioms kept everything.
    pub axioms: Vec<String>,
    /// `--prefix "name: namespace"` bindings, which is how a `--select` CURIE
    /// resolves. UBERON's `cumbo` term list is
    /// `filter --prefix 'uberon: …/obo/uberon/core#' --select
    /// 'oboInOwl:inSubset=uberon:cumbo'`, and without the binding recorded the
    /// selector matched nothing — which, an empty seed meaning the whole
    /// ontology, exported all 16,417 terms instead of 14.
    pub prefixes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AnnotateSpec {
    pub ontology_iri: Option<String>,
    pub version_iri: Option<String>,
    pub annotations: Vec<(String, String)>,
    pub link_annotations: Vec<(String, String)>,
    pub remove_annotations: bool,
}

/// A parsed `flybase:rewrite-def` invocation.
#[derive(Debug, Clone, Default)]
pub struct RewriteDefSpec {
    pub sub: bool,
    pub dot: bool,
    pub null_definitions: bool,
    pub no_ids: bool,
    pub include_obsolete: bool,
    pub filter_prefix: Option<String>,
    /// Raw `"PROP VALUE"` strings for `--add-annotation` (literal value).
    pub add_annotation: Vec<String>,
    /// Raw `"PROP IRI"` strings for `--add-annotation-iri`.
    pub add_annotation_iri: Vec<String>,
}

/// A parsed whole-release invocation: one recipe line asking for the asserted,
/// relaxed and simple variants at once, written into `--outdir`. A planning
/// marker: the plan-level `rewrite_oort` pass turns the `oort` directory target
/// and its `cp oort/<file> <file>` consumers into artefacts built from ordinary
/// ops, so this never reaches the executor.
#[derive(Debug, Clone, Default)]
pub struct OortSpec {
    /// The source ontology (the rule's first prerequisite, e.g. `wbbt-edit.owl`).
    pub input: String,
    /// `--outdir` (e.g. `oort`).
    pub outdir: String,
    pub reasoner: String,
    pub simple: bool,
    pub relaxed: bool,
    pub asserted: bool,
}

/// Options of `remove` owlmake cannot execute (empty = fully covered). Factored
/// out so the same coverage rule applies whether a step came from ingest or was
/// hand-written in `owlmake.json`.
pub fn remove_gaps(spec: &RemoveSpec) -> Vec<String> {
    let mut gaps = vec![];
    // The categories `cmd::remove` really applies — the same list its classifier
    // reads, so a category cannot be executable but reported as a gap, or the
    // reverse. A category missing from it is reported as a gap, so the artefact is
    // never rebuilt: its committed copy is consumed as-is and goes stale
    // unnoticed. For UBERON that artefact is
    // `remove --axioms "equivalent disjoint type abox"`, the source of
    // `subsets/merged-partonomy.owl` and so of twenty of its twenty-three subsets.
    for a in spec.axioms.iter().flat_map(|a| a.split_whitespace()) {
        if !crate::cmd::select::is_axiom_category(a) {
            gaps.push(format!("remove --axioms {a}"));
        }
    }
    // Each `--select` value is a group of space-separated selectors. owlmake
    // covers the special `imports`/`complement`/`ontology` idioms, the typed
    // entity selectors, and IRI/CURIE patterns (`<…/BFO_*>`, `OBO:*`, full IRIs).
    for s in spec.selects.iter().flat_map(|s| s.split_whitespace()) {
        let covered = matches!(
            s,
            "imports" | "complement" | "ontology" | "anonymous" | "named" | "classes"
                | "properties" | "object-properties" | "object-property" | "data-properties"
                | "data-property" | "annotation-properties" | "annotation-property"
                | "individuals" | "named-individuals" | "instances" | "datatypes"
                // The RELATION selectors, which `cmd::remove` expands the removal
                // seed with (`select::direct_parents`/`ancestors`/`children`/
                // `descendants`/`equivalents`/`types`/`domains`/`ranges`). `self` is
                // the identity — the seed already holds the terms themselves — and
                // `remove --term MONDO:0005583 --select "self descendants"` is how
                // MONDO builds `subsets/mondo-clingen.owl`.
                | "self" | "parents" | "ancestors" | "children" | "descendants"
                | "equivalents" | "types" | "domains" | "ranges"
        ) || crate::cmd::select::is_pattern(s)
            // `PROP=VALUE` is covered too, and is NOT a pattern — `is_pattern`
            // excludes it precisely so it is not glob-matched against entity IRIs.
            // Both commands implement it, so leaving it out of this list reported
            // `remove --select owl:deprecated='true'^^xsd:boolean` as an uncovered
            // step and refused to build `composite-*-basic.owl` at all.
            || crate::cmd::select::parse_annotation_value(s).is_some();
        if !covered {
            gaps.push(format!("remove --select {s}"));
        }
    }
    gaps
}

/// Wrap a [`RemoveSpec`] into a [`Step`], downgrading to [`Step::Partial`] when
/// it uses uncovered options.
pub fn remove_step(spec: RemoveSpec) -> Step {
    let gaps = remove_gaps(&spec);
    if gaps.is_empty() { Step::Op(Op::Remove(spec)) } else { Step::Partial { op: Op::Remove(spec), gaps } }
}

/// Options of `filter` owlmake cannot execute (empty = fully covered).
pub fn filter_gaps(spec: &FilterSpec) -> Vec<String> {
    let mut gaps = vec![];
    // `filter --axioms` reads the same classifier `remove --axioms` does.
    for a in spec.axioms.iter().flat_map(|a| a.split_whitespace()) {
        if !crate::cmd::select::is_axiom_category(a) {
            gaps.push(format!("filter --axioms {a}"));
        }
    }
    // The OBO `-simple`/`-basic` signature filter is supported; only
    // unrecognised selectors are gaps.
    for s in spec.selects.iter().flat_map(|s| s.split_whitespace()) {
        let covered = matches!(
            s,
            "annotations" | "ontology" | "anonymous" | "named" | "self" | "complement"
                | "classes" | "properties" | "object-properties" | "data-properties"
                | "annotation-properties" | "individuals" | "named-individuals" | "datatypes"
                // The relation selectors `filter_core` expands the seed with, the
                // same set `remove` covers. MONDO's `tmp/hgnc_import.owl` is
                // `filter --term … --select "self descendants"`.
                | "parents" | "ancestors" | "children" | "descendants" | "equivalents"
                | "types" | "instances" | "domains" | "ranges"
        ) || crate::cmd::select::is_pattern(s)
            // `PROP=VALUE` is covered too, and is NOT a pattern — `is_pattern`
            // excludes it precisely so it is not glob-matched against entity IRIs.
            // Both commands implement it, so leaving it out of this list reported
            // `remove --select owl:deprecated='true'^^xsd:boolean` as an uncovered
            // step and refused to build `composite-*-basic.owl` at all.
            || crate::cmd::select::parse_annotation_value(s).is_some();
        if !covered {
            gaps.push(format!("filter --select {s}"));
        }
    }
    gaps
}

/// Wrap a [`FilterSpec`] into a [`Step`], downgrading to [`Step::Partial`] when
/// it uses uncovered options.
pub fn filter_step(spec: FilterSpec) -> Step {
    let gaps = filter_gaps(&spec);
    if gaps.is_empty() { Step::Op(Op::Filter(spec)) } else { Step::Partial { op: Op::Filter(spec), gaps } }
}

/// One element of a parsed recipe.
#[derive(Debug, Clone)]
pub enum Step {
    /// A fully-mapped, executable operation.
    Op(Op),
    /// A mapped operation that uses options owlmake can't execute yet.
    Partial { op: Op, gaps: Vec<String> },
    /// A subcommand named by a recipe that owlmake does not implement.
    UnknownRobot(String),
    /// A subcommand owlmake implements as a CLI command but does not model
    /// as a pipeline [`Op`] — `uberon:create-species-subset`, which writes two
    /// products (the tag set and the pruned view) rather than threading one model
    /// through. Executed by invoking the owlmake binary's matching subcommand, so
    /// it is covered, not a gap.
    ///
    /// `args` carries the invocation's own option tokens (flags and their values,
    /// flattened), so the step says both which subcommand to run and what to run
    /// it on. Without them it could only be executed by replaying the recipe line,
    /// and the plan would not be self-sufficient.
    CliRobot { name: String, args: Vec<String> },
    /// A recipe line with no observable effect (`echo`, `cd`, the move of a
    /// `.tmp` onto the target that the pipeline's closing write already
    /// performed). Internal to ingest: the executor skips these, so the planner
    /// drops them and they never reach a plan.
    Inert(String),
    /// A native file-system operation (cp/mv/rm/mkdir/touch), decomposed out of
    /// the recipe so it runs declaratively without a shell.
    File(FileOp),
    /// The bundled jq engine (`owlmake jq`), with its argument tokens (after the
    /// `jq` launcher).
    Jq(Vec<String>),
    /// The bundled SSSOM CLI (`owlmake sssom`), with its argument tokens
    /// (including the `sssom`/`sssom:<cmd>` launcher).
    Sssom(Vec<String>),
    /// A recipe line run as a command line: text processors, control flow, and
    /// anything else with an effect owlmake does not model as an op.
    ///
    /// `requires` names the command words owlmake cannot vouch for. Every command
    /// word owlmake implements itself is served from a PATH shim that re-execs the
    /// matching owlmake subcommand, and the POSIX text tools are expected to be
    /// present; anything else — `git`, `wget`, a project script — has to be in the
    /// environment, and saying so in the plan is the difference between a build
    /// that fails with "command not found" and one you can preflight.
    Shell { command: String, requires: Vec<String> },
    /// A shell command that runs ONLY IF the preceding step failed — the
    /// right-hand side of `||`.
    ///
    /// Recorded because recipes lean on it for their error paths and the separator
    /// is not recoverable downstream. A profile check is written
    /// `validate-profile … || { cat <report> && exit 1; }`: split into two
    /// UNCONDITIONAL steps the `exit 1` runs on every build, so HPO and OBA report
    /// a check as broken even when the report it writes shows it passed. `&&` and
    /// `;` need no variant: sequential steps that abort on failure already mean the
    /// same thing.
    Fallback { command: String, requires: Vec<String> },
    /// A shell `if … then … [else …] fi` construct decomposed into structured
    /// form: a `condition` (the shell test, e.g. `[ -s foo.tsv ]`) and the nested
    /// step lists run when it succeeds / fails. Sub-steps may themselves be
    /// branches (nested `if`s). Recipes containing a branch are executed by
    /// replaying the original recipe line through the shell, so the condition keeps
    /// exact shell semantics; the structured form is what the plan records, so
    /// `owlmake.json` shows the conditional logic rather than an opaque blob.
    Branch {
        condition: Condition,
        then_steps: Vec<Step>,
        else_steps: Vec<Step>,
    },
    /// A whole-release invocation, resolved by the plan-level `rewrite_oort` pass
    /// into artefacts built from ordinary ops (so it never reaches the executor).
    /// Until rewritten it is fully covered (no gap).
    Oort(OortSpec),
}

impl Step {
    /// Human-readable gaps contributed by this step (empty when fully covered).
    pub fn gaps(&self) -> Vec<String> {
        match self {
            Step::Op(_) | Step::Inert(_) | Step::Shell { .. } | Step::Fallback { .. } | Step::Oort(_) => vec![],
            Step::File(_) | Step::Jq(_) | Step::Sssom(_) | Step::CliRobot { .. } => vec![],
            Step::Partial { gaps, .. } => gaps.clone(),
            Step::UnknownRobot(name) => vec![format!("unsupported ROBOT command `{name}`")],

            // A branch is covered exactly when both of its bodies are.
            Step::Branch { then_steps, else_steps, .. } => then_steps
                .iter()
                .chain(else_steps)
                .flat_map(|s| s.gaps())
                .collect(),
        }
    }
    /// The subset of [`Step::gaps`] that owlmake genuinely cannot perform: an
    /// unimplemented subcommand, or a mapped op with options it can't honour.
    /// A shell command is deliberately excluded — those are executed, and fail
    /// loudly at run time if the tool they need is absent, rather than being a
    /// coverage gap.
    pub fn unrunnable_gaps(&self) -> Vec<String> {
        match self {
            Step::Partial { gaps, .. } => gaps.clone(),
            Step::UnknownRobot(name) => vec![format!("unsupported ROBOT command `{name}`")],
            Step::Branch { then_steps, else_steps, .. } => then_steps
                .iter()
                .chain(else_steps)
                .flat_map(|s| s.unrunnable_gaps())
                .collect(),
            _ => vec![],
        }
    }

    pub fn label(&self) -> String {
        match self {
            Step::Op(op) | Step::Partial { op, .. } => op_label(op),
            Step::UnknownRobot(n) => format!("robot {n} (UNSUPPORTED)"),
            Step::CliRobot { name, .. } => format!("robot {name}"),
            Step::File(f) => f.label(),
            Step::Jq(args) => format!("jq {}", args.join(" ")),
            Step::Sssom(args) => format!("sssom {}", args.iter().skip(1).cloned().collect::<Vec<_>>().join(" ")),
            Step::Inert(c) => format!("sh: {} (no effect)", first_word(c)),
            Step::Fallback { command, requires } => {
                let mut d = format!("|| {command}");
                if !requires.is_empty() {
                    d.push_str(&format!(" (requires {})", requires.join(", ")));
                }
                d
            }
            Step::Shell { command, requires } if requires.is_empty() => {
                format!("sh: {}", first_word(command))
            }
            Step::Shell { command, requires } => {
                format!("sh: {} (needs {})", first_word(command), requires.join(", "))
            }
            Step::Branch { condition, .. } => format!("if {}", condition.describe()),

            Step::Oort(s) => {
                let mut v = vec![];
                if s.asserted { v.push("asserted"); }
                if s.relaxed { v.push("relaxed"); }
                if s.simple { v.push("simple"); }
                format!("oort[{}]", v.join("+"))
            }
        }
    }
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

fn op_label(op: &Op) -> String {
    match op {
        Op::Merge { collapse_import_closure, .. } => {
            if *collapse_import_closure == Some(false) {
                "merge (keep imports)".into()
            } else {
                "merge (+resolve imports)".into()
            }
        }
        Op::Unmerge { .. } => "unmerge".into(),
        Op::Reason { reasoner, equivalent_classes_allowed, exclude_tautologies, .. } => format!(
            "reason[{}{}{}]",
            reasoner.clone().unwrap_or_else(|| "ELK".into()),
            equivalent_classes_allowed.as_ref().map(|e| format!(", eq={e}")).unwrap_or_default(),
            exclude_tautologies.as_ref().map(|e| format!(", tauto={e}")).unwrap_or_default(),
        ),
        Op::Relax { include_subclass_of } => {
            if *include_subclass_of {
                "relax[+subclass-of]".into()
            } else {
                "relax".into()
            }
        }
        Op::Reduce { reasoner, include_subproperties } => format!(
            "reduce[{}{}]",
            reasoner.clone().unwrap_or_else(|| "ELK".into()),
            if include_subproperties.unwrap_or(false) { ", +subproperties" } else { "" },
        ),
        Op::Materialize { properties, term_files } => {
            let mut p = properties.clone();
            for f in term_files { p.push(format!("@{f}")); }
            format!("materialize[{}]", p.join(" "))
        }
        Op::Remove(s) => {
            let mut bits = vec![];
            if !s.terms.is_empty() { bits.push(format!("term×{}", s.terms.len())); }
            if !s.term_files.is_empty() { bits.push(format!("term-file×{}", s.term_files.len())); }
            if !s.axioms.is_empty() { bits.push(format!("axioms={}", s.axioms.join("+"))); }
            if !s.selects.is_empty() { bits.push(format!("select={}", s.selects.join("+"))); }
            if !s.base_iri.is_empty() { bits.push(format!("base-iri={}", s.base_iri.join("|"))); }
            format!("remove[{}]", bits.join(", "))
        }
        Op::Filter(s) => {
            let mut bits = vec![];
            if !s.term_files.is_empty() { bits.push(format!("term-file×{}", s.term_files.len())); }
            if !s.selects.is_empty() { bits.push(format!("select={}", s.selects.join("+"))); }
            format!("filter[{}]", bits.join(", "))
        }
        Op::Annotate(s) => format!(
            "annotate[{}{}+{}ann]",
            s.ontology_iri.as_ref().map(|_| "ont-iri ").unwrap_or(""),
            s.version_iri.as_ref().map(|_| "ver-iri ").unwrap_or(""),
            s.annotations.len() + s.link_annotations.len()
        ),
        Op::Convert { format, .. } => format!("convert[{}]", format.clone().unwrap_or_else(|| "owl".into())),
        Op::Collapse { threshold, precious, .. } => {
            format!("collapse[t={}, {} precious]", threshold.unwrap_or(2), precious.len())
        }
        Op::Mint { id_range_name, .. } => format!("mint[{id_range_name}]"),
        Op::ExtractUphenoRelations { relations, roots, .. } => {
            format!("extract-upheno-relations[{} rel, {} roots]", relations.len(), roots.len())
        }
        Op::Normalize { subset_decls, synonym_decls, .. } => {
            let mut bits = vec![];
            if *subset_decls { bits.push("subset-decls"); }
            if *synonym_decls { bits.push("synonym-decls"); }
            format!("normalize[{}]", bits.join(", "))
        }
        Op::Template { templates, merge, prefixes } => {
            let p =
                if prefixes.is_empty() { String::new() } else { format!(", +{}pfx", prefixes.len()) };
            format!("template[×{}{}{}]", templates.len(), if *merge { ", merge" } else { "" }, p)
        }
        Op::Rename { mappings, prefix_mappings, .. } => {
            let m = if mappings.is_some() { "mappings" } else if prefix_mappings.is_some() { "prefix-mappings" } else { "" };
            format!("rename[{m}]")
        }
        Op::Extract { method, terms, term_files, .. } => {
            format!("extract[{}, term×{}, term-file×{}]", method, terms.len(), term_files.len())
        }
        Op::RoundTrip { path } => format!("round-trip[{path}]"),
        Op::Query { updates, selects, constructs, .. } => {
            let mut bits = vec![];
            if !updates.is_empty() { bits.push(format!("update×{}", updates.len())); }
            if !selects.is_empty() { bits.push(format!("select×{}", selects.len())); }
            if !constructs.is_empty() { bits.push(format!("construct×{}", constructs.len())); }
            format!("query[{}]", bits.join(", "))
        }
        Op::Repair { invalid_references, merge_axiom_annotations } => {
            let mut bits = vec![];
            if *invalid_references { bits.push("invalid-references"); }
            if *merge_axiom_annotations { bits.push("merge-axiom-annotations"); }
            if bits.is_empty() { bits.push("dedupe"); }
            format!("repair[{}]", bits.join(", "))
        }
        Op::MergeEquivalentSets { set_prefix, .. } => {
            format!("merge-equivalent-sets[{}]", set_prefix.join(","))
        }
        Op::Babelon { input, .. } => format!("babelon[{input}]"),
        Op::Expand { .. } => "expand[macros]".into(),
        Op::Subset { subset, queries, fill_gaps, .. } => {
            let mut bits: Vec<String> = Vec::new();
            if !subset.is_empty() {
                bits.push(subset.clone());
            }
            if !queries.is_empty() {
                bits.push(format!("query×{}", queries.len()));
            }
            // ROBOT's default: true in inSubset mode, false in query mode.
            if fill_gaps.unwrap_or(queries.is_empty()) {
                bits.push("fill-gaps".into());
            }
            format!("subset[{}]", bits.join(", "))
        }
        Op::MergeSpecies { batch_file, .. } => {
            format!("merge-species[{}]", batch_file.as_deref().unwrap_or("single"))
        }
        Op::RewriteDef(s) => {
            let mut bits = vec![];
            if s.dot { bits.push("dot"); }
            if s.sub { bits.push("sub"); }
            format!("rewrite-def[{}]", bits.join("+"))
        }
        Op::SimpleSubset { .. } => "simple-subset[native classes]".into(),
        Op::ExtractOntologySubset { subset, fill_gaps } => {
            format!("extract-ontology-subset[{subset}{}]", if *fill_gaps { ",fill-gaps" } else { "" })
        }
        Op::ExtractMingraph => "extract-mingraph".into(),
        Op::RemoveAxiomAnnotations => "remove-axiom-annotations".into(),
        Op::MakeSubsetByProperties { properties } => {
            format!("make-subset-by-properties[{}]", properties.join(" "))
        }
    }
}

/// A structured shell test used as a [`Step::Branch`] condition. The common
/// file-state tests are recognised and can be evaluated natively (no subshell);
/// anything else is kept verbatim as [`Condition::Shell`] and evaluated by `sh`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    /// `[ -e f ]` / `[ -f f ]` — the path exists.
    FileExists(String),
    /// `[ -s f ]` — the path exists and is non-empty.
    FileNonEmpty(String),
    /// `[ ! -e f ]` / `[ ! -f f ]` / `[ ! -s f ]` — the path does not exist.
    FileMissing(String),
    /// `[ -d f ]` — the path exists and is a directory.
    DirExists(String),
    /// Any other test, evaluated verbatim by the shell.
    Shell(String),
}

impl Condition {
    /// Parse a shell test (`[ -s foo.tsv ]`, `test -f foo`, …) into a typed
    /// condition, falling back to [`Condition::Shell`] for anything unrecognised.
    pub fn parse(raw: &str) -> Condition {
        let t = raw.trim();
        let inner = t
            .strip_prefix("[[")
            .and_then(|s| s.strip_suffix("]]"))
            .or_else(|| t.strip_prefix('[').and_then(|s| s.strip_suffix(']')))
            .map(str::trim)
            .or_else(|| t.strip_prefix("test ").map(str::trim))
            .unwrap_or(t);
        let toks: Vec<&str> = inner.split_whitespace().collect();
        match toks.as_slice() {
            ["-e", f] | ["-f", f] => Condition::FileExists(f.to_string()),
            ["-s", f] => Condition::FileNonEmpty(f.to_string()),
            ["-d", f] => Condition::DirExists(f.to_string()),
            ["!", "-e", f] | ["!", "-f", f] | ["!", "-s", f] => Condition::FileMissing(f.to_string()),
            _ => Condition::Shell(raw.to_string()),
        }
    }

    /// The value of a condition that is decidable without touching the system:
    /// a comparison of two literals (`[ true = true ]`), and `&&`/`||` over such.
    ///
    /// A recipe guard on a build-mode variable expands to exactly this once the
    /// variable's value is known, and a guard whose answer is already fixed does
    /// not belong in a plan: recorded as-is with the variable false it becomes
    /// `[ false = true ]`, permanently dead, and the steps beneath it can never run
    /// however the plan is later invoked. Callers fold such branches away.
    pub fn static_value(&self) -> Option<bool> {
        let Condition::Shell(raw) = self else { return None };
        fn atom(t: &str) -> Option<bool> {
            let t = t.trim();
            let inner = t
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .map(str::trim)
                .or_else(|| t.strip_prefix("test ").map(str::trim))?;
            match inner.split_whitespace().collect::<Vec<_>>().as_slice() {
                [a, "=", b] | [a, "==", b] => Some(a == b),
                [a, "!=", b] => Some(a != b),
                _ => emptiness(inner),
            }
        }
        // `[ "$(SOME_LIST)" ]`, `[ -n "…" ]`, `[ -z "…" ]`: a one-argument test,
        // true exactly when the argument is non-empty. Once the variable has been
        // expanded the answer is fixed, so the guard is as static as an `=` — and
        // this is the shape ODK uses to skip a step whose input list is empty
        // (`if [ "$(DOSDP_PATTERN_NAMES_DEFAULT)" ]; then …`). Anything still
        // holding a `$` or a command substitution is not decided yet.
        fn emptiness(inner: &str) -> Option<bool> {
            if inner.contains(['$', '`']) {
                return None; // still to be expanded — not decided
            }
            let words = shell_words(inner)?;
            match words.as_slice() {
                [] => Some(false),                   // `[ ]`
                [a] => Some(!a.is_empty()),          // one argument: non-empty?
                [op, a] if op == "-n" => Some(!a.is_empty()),
                [op, a] if op == "-z" => Some(a.is_empty()),
                _ => None, // `-f`, `-d`, `-s`, … are about the filesystem, not the plan
            }
        }
        /// Split on whitespace, keeping a quoted run together and dropping the
        /// quotes. `None` if a quote is left open.
        fn shell_words(s: &str) -> Option<Vec<String>> {
            let (mut out, mut cur, mut quote, mut started) = (Vec::new(), String::new(), None, false);
            for c in s.chars() {
                match (quote, c) {
                    (Some(q), _) if c == q => quote = None,
                    (Some(_), _) => cur.push(c),
                    (None, '"') | (None, '\'') => {
                        quote = Some(c);
                        started = true;
                    }
                    (None, _) if c.is_whitespace() => {
                        if started {
                            out.push(std::mem::take(&mut cur));
                            started = false;
                        }
                    }
                    (None, _) => {
                        cur.push(c);
                        started = true;
                    }
                }
            }
            quote.is_none().then(|| {
                if started {
                    out.push(cur);
                }
                out
            })
        }
        // Only one operator kind per expression: mixing `&&` and `||` without
        // parentheses is not something a recipe guard does, and guessing the
        // precedence would be worse than declining to fold.
        if raw.contains("&&") && raw.contains("||") {
            return None;
        }
        if raw.contains("&&") {
            return raw.split("&&").map(atom).try_fold(true, |acc, v| Some(acc && v?));
        }
        if raw.contains("||") {
            return raw.split("||").map(atom).try_fold(false, |acc, v| Some(acc || v?));
        }
        atom(raw)
    }

    /// Whether this condition can be evaluated natively (no shell subprocess).
    pub fn is_native(&self) -> bool {
        !matches!(self, Condition::Shell(_))
    }

    /// A human description for the plan/stage display.
    pub fn describe(&self) -> String {
        match self {
            Condition::FileExists(f) => format!("{f} exists"),
            Condition::FileNonEmpty(f) => format!("{f} exists and is non-empty"),
            Condition::FileMissing(f) => format!("{f} is missing"),
            Condition::DirExists(f) => format!("{f}/ exists"),
            Condition::Shell(c) => c.clone(),
        }
    }
}
