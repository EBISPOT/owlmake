//! The vocabulary a plan is written in: the operations a build step can be, and
//! the steps themselves.
//!
//! This is contract, not ingest. A plan names these whether it was written by
//! ingest ([`crate::odk`]) or by hand in `owlmake.yaml`, and the executor
//! ([`crate::build`]) knows only these — never where they came from.

use crate::build::recipe::FileOp;
use crate::cmd::Switch;

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
        /// the current ontology keeps its imports as declarations and their
        /// axioms are NOT merged — they remain a read-only reasoning closure
        /// until a later collapsing merge — and each input gives its own axioms.
        collapse_import_closure: Option<bool>,
        /// `--include-annotations`: the inputs' ontology annotations join the
        /// current ontology's.
        include_annotations: bool,
        /// `--annotate-defined-by`: each entity with no `rdfs:isDefinedBy` is
        /// defined by the ontology that names it.
        annotate_defined_by: bool,
        /// `--annotate-derived-from`: each axiom says with `prov:wasDerivedFrom`
        /// which ontology it came from.
        annotate_derived_from: bool,
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
        /// `-A`/`--axiom-generators`: the inference types to assert, as the
        /// recipe spells them; empty is the command's default, `SubClass`.
        axiom_generators: Vec<String>,
        /// `--properties`: the object properties the `PropertyAssertion`
        /// generator is restricted to; empty is every named object property.
        properties: Vec<String>,
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
        /// `--preserve-annotated-axioms`: keep a redundant axiom that carries
        /// annotations.
        preserve_annotated_axioms: bool,
        /// `--named-classes-only`: reduce only axioms between named classes.
        named_classes_only: bool,
    },
    Materialize {
        /// `-r/--reasoner`: the reasoner the ontology is checked with.
        reasoner: Option<String>,
        /// `-n/--create-new-ontology`: check only, and keep the input as it was.
        create_new_ontology: Option<bool>,
        properties: Vec<String>,
        term_files: Vec<String>,
    },
    Remove(SelectionSpec),
    Filter(SelectionSpec),
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
        /// `--check false`: write an OBO document whose frames repeat a
        /// single-valued tag, instead of refusing it.
        check: Option<bool>,
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
    /// `repair`: merge the annotations of axioms that are otherwise the same,
    /// and migrate every reference to a deprecated entity to its replacement.
    Repair {
        invalid_references: bool,
        merge_axiom_annotations: bool,
        /// The annotation properties, as written, whose assertions on a
        /// deprecated entity move to its replacement.
        annotation_properties: Vec<String>,
        /// A file listing more of them, one per line.
        annotation_properties_file: Option<String>,
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
    /// named subclasses, bridging the hierarchy across them. `threshold` is the
    /// recipe's text, read as an integer when the step runs.
    Collapse {
        precious: Vec<String>,
        precious_files: Vec<String>,
        threshold: Option<String>,
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
    /// The prefix options in force from here. The commands that follow read their
    /// CURIEs with a context made afresh from them ([`crate::context`]): the
    /// built-in map, or the `--prefixes FILE` in its place, or none with
    /// `--noprefixes`, then each `--add-prefixes FILE`, `--prefix` and
    /// `--add-prefix "foo: http://bar"`. What they ADD is declared by the document
    /// written next, whether or not any axiom uses it, and nothing else they bind
    /// is.
    ///
    /// A chain gives each of its commands the options its command line states
    /// before the first command, and the command's own, so a step stands wherever
    /// that set changes: at the head of a chain that states any, before a command
    /// with options of its own, and before the command after it. CL's
    /// `components/hra_subset.owl` is `robot --add-prefix "obo: …" annotate …`; a
    /// repository that uses its context runs every command as
    /// `robot --add-prefixes config/context.json …`, and ODK's component rule
    /// runs `template --add-prefixes config/context.json …`, whose bindings the
    /// `annotate` and `convert` after it do not have. A file is read when the
    /// step runs, from the repository's directory.
    Prefixes {
        prefixes: Option<String>,
        noprefixes: bool,
        add_prefixes: Vec<String>,
        prefix: Vec<String>,
        add_prefix: Vec<String>,
    },
    /// `template` — generate axioms from template tables (a row of template
    /// strings over a table of terms). The step yields the generated axioms'
    /// own ontology, or under `merge` its input with the axioms added.
    Template {
        templates: Vec<String>,
        /// The input, keeping its IRIs, annotations and prefixes, with the
        /// generated axioms added, counts as changed whatever they add.
        merge: bool,
        /// Under `merge`, the input's imports are taken out of it: the merged
        /// ontology imports nothing, and the imports' axioms stay out.
        collapse_import_closure: bool,
        /// `--ancestors`: the generated axioms' terms the input names come
        /// with the ancestors the input gives them, and their labels.
        ancestors: bool,
        /// `--force true`: a row the tables cannot be read into is reported and
        /// skipped. Without it such a row fails the step.
        force: bool,
    },
    /// `rename` — give entities new IRIs, from a mappings table, `--mapping`
    /// pairs and a prefix-mappings table.
    Rename {
        mappings: Option<String>,
        /// The `--mapping OLD NEW` pairs, in recipe order.
        mapping: Vec<(String, String)>,
        prefix_mappings: Option<String>,
        allow_missing: bool,
        /// `--allow-duplicates true`: two rows of the mappings table may give
        /// the same new IRI.
        allow_duplicates: bool,
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
        /// MIREOT `--lower-term`/`--lower-terms`: the terms whose ancestors
        /// are extracted.
        lower_terms: Vec<String>,
        lower_term_files: Vec<String>,
        /// MIREOT `--upper-term`/`--upper-terms`: the terms a climb stops at.
        upper_terms: Vec<String>,
        upper_term_files: Vec<String>,
        /// `--intermediates`: `all`, `minimal` or `none`; `all` when unset.
        intermediates: Option<String>,
        /// `--force true`: extract even when the ontology names none of the
        /// terms, which otherwise fails the step.
        force: bool,
    },
    /// Write the in-flight model to `path`, in the format the path's extension
    /// names, and go on with it: a command's `-o` part way through a chain. What
    /// the next command sees is the model, not the file.
    ///
    /// It is not bookkeeping: the file is the recipe's, and a later invocation
    /// may read it. EFO's mondo import writes `mondo_import.owl.tmp.owl` with its
    /// `extract`, and the invocations after it open that file.
    Write {
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
    /// `expand` — expand `OMO:0002000` (defined by construct) macros.
    ///
    /// `expand_terms` is the ALLOW-list: with one, only those terms' macros run.
    /// CL builds its taxon views with `expand --expand-term RO:0002161`, which
    /// leaves `RO:0002175`'s macro, minting a named witness class per taxon
    /// assertion, unexpanded.
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

impl Op {
    /// A `merge` of `inputs` with every option at its default.
    pub fn plain_merge(inputs: Vec<String>) -> Op {
        Op::Merge {
            inputs,
            collapse_import_closure: None,
            include_annotations: false,
            annotate_defined_by: false,
            annotate_derived_from: false,
        }
    }
}

/// What `remove` and `filter` select, and how each judges an axiom against
/// the selection: every option the two commands share, as the recipe gives it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SelectionSpec {
    /// `--term`, `--term-file`: the terms the object set starts from.
    pub terms: Vec<String>,
    pub term_files: Vec<String>,
    /// `--include-term`, `--include-terms`: terms added to the selection.
    pub include_terms: Vec<String>,
    pub include_term_files: Vec<String>,
    /// `--exclude-term`, `--exclude-terms`: terms taken out of the selection.
    /// UBERON's `merged-partonomy.owl` is `remove --exclude-term BFO:0000050
    /// --select object-properties`: every object property but part_of goes.
    pub exclude_terms: Vec<String>,
    pub exclude_term_files: Vec<String>,
    /// `--select`: the selector groups, each mapping the set it is given.
    pub selects: Vec<String>,
    /// `--axioms`: the axiom types acted on. UBERON's `composite-*-basic.owl`
    /// is `filter --axioms "subclass equivalent annotation"`.
    pub axioms: Vec<String>,
    /// `--base-iri`: the namespaces `--axioms internal|external` judge an
    /// axiom's subjects by.
    pub base_iri: Vec<String>,
    /// `--trim`: whether any selected object takes an axiom (`remove`'s
    /// default) or every one must be selected (`filter`'s).
    pub trim: Option<Switch>,
    /// `--signature`: judge an axiom by the IRIs it names alone.
    pub signature: Option<Switch>,
    /// `--preserve-structure`: re-assert the hierarchy across what goes.
    pub preserve_structure: Option<Switch>,
    /// `--allow-punning`: a term naming entities of several kinds selects them
    /// all.
    pub allow_punning: Option<Switch>,
    /// `--drop-axiom-annotations`, every value in recipe order.
    pub drop_axiom_annotations: Vec<String>,
}

impl SelectionSpec {
    /// Every term file the step reads.
    pub fn files(&self) -> impl Iterator<Item = &String> {
        self.term_files.iter().chain(&self.include_term_files).chain(&self.exclude_term_files)
    }
}

#[derive(Debug, Clone, Default)]
pub struct AnnotateSpec {
    pub ontology_iri: Option<String>,
    pub version_iri: Option<String>,
    /// `--annotation PROP VALUE`.
    pub annotations: Vec<(String, String)>,
    /// `--link-annotation PROP IRI`.
    pub link_annotations: Vec<(String, String)>,
    /// `--language-annotation PROP VALUE LANG`.
    pub language_annotations: Vec<(String, String, String)>,
    /// `--typed-annotation PROP VALUE TYPE`.
    pub typed_annotations: Vec<(String, String, String)>,
    /// `--axiom-annotation`: its values as the recipe gives them, read in
    /// `PROP VALUE` pairs.
    pub axiom_annotations: Vec<String>,
    /// `--annotation-file`: ontologies whose axioms and ontology annotations are
    /// merged in.
    pub annotation_files: Vec<String>,
    pub remove_annotations: bool,
    /// `--interpolate true`.
    pub interpolate: bool,
    /// `--annotate-defined-by true`.
    pub annotate_defined_by: bool,
    /// `--annotate-derived-from true`.
    pub annotate_derived_from: bool,
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
pub fn remove_gaps(spec: &SelectionSpec) -> Vec<String> {
    axiom_gaps("remove", &spec.axioms)
}

/// The `--axioms` values the classifier does not know
/// ([`crate::cmd::select::is_axiom_category`]): `tautologies`, which asks a
/// reasoner whether each axiom holds of every ontology, and any value that is
/// no axiom type. Every `--select` value runs — one that is no selector selects
/// nothing ([`crate::cmd::objects`]) — so none is a gap.
fn axiom_gaps(command: &str, axioms: &[String]) -> Vec<String> {
    axioms
        .iter()
        .flat_map(|a| a.split_whitespace())
        .filter(|a| !crate::cmd::select::is_axiom_category(a))
        .map(|a| format!("{command} --axioms {a}"))
        .collect()
}

/// Wrap a `remove` into a [`Step`], downgrading to [`Step::Partial`] when it
/// uses uncovered options.
pub fn remove_step(spec: SelectionSpec) -> Step {
    let gaps = remove_gaps(&spec);
    if gaps.is_empty() { Step::Op(Op::Remove(spec)) } else { Step::Partial { op: Op::Remove(spec), gaps } }
}

/// Options of `filter` owlmake cannot execute (empty = fully covered).
pub fn filter_gaps(spec: &SelectionSpec) -> Vec<String> {
    axiom_gaps("filter", &spec.axioms)
}

/// Wrap a `filter` into a [`Step`], downgrading to [`Step::Partial`] when it
/// uses uncovered options.
pub fn filter_step(spec: SelectionSpec) -> Step {
    let gaps = filter_gaps(&spec);
    if gaps.is_empty() { Step::Op(Op::Filter(spec)) } else { Step::Partial { op: Op::Filter(spec), gaps } }
}

/// One element of a parsed recipe.
#[derive(Debug, Clone)]
pub enum Step {
    /// The start of a new tool invocation.
    ///
    /// A recipe's every command line is its own process, and processes share
    /// nothing but files. So a line does NOT continue the previous line's
    /// in-memory ontology: it opens a new pipeline over whatever its own
    /// `--input` names, and if it names none it starts from nothing.
    ///
    /// Losing that boundary is losing the shell that stood between the two
    /// invocations, and it has written a wrong artefact three times over.
    /// uPheno's `components/upheno-bridge.owl` carried the edit file and its
    /// whole import closure into a component built from `tmp/bridge.ttl` alone,
    /// 158 MB against 8. OBA's `construct_pato_attribute` serialised the whole of
    /// PATO instead of the CONSTRUCT its second invocation reads back from disk,
    /// 8,981 nodes against 488. EFO's `components/legal_diseases.txt` ran its
    /// second invocation's query against the merged edit ontology rather than the
    /// `disease_to_phenotype_merged.owl` that invocation names — which also hid
    /// that the file was missing, because nothing ever opened it.
    ///
    /// `input` is the invocation's own `--input`, and the model is re-established
    /// from it. `None` means the invocation names no input of its own and starts
    /// empty; the steps that follow build the model themselves.
    Boundary { input: Option<String> },
    /// A fully-mapped, executable operation.
    Op(Op),
    /// A mapped operation that uses options owlmake can't execute yet.
    Partial { op: Op, gaps: Vec<String> },
    /// A subcommand named by a recipe that owlmake does not implement.
    UnsupportedSubcommand(String),
    /// Options a recipe gives a command that owlmake does not read, each with
    /// its values: the plan refuses the command rather than run it without
    /// them.
    UnsupportedOptions { command: String, options: Vec<String> },
    /// A command line its commands cannot read, with the message that says
    /// why: the step fails as running the line fails.
    Refused { message: String },
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
    OwlmakeCli { name: String, args: Vec<String> },
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
    /// A step whose FAILURE is not an error — the shell's `cmd || true`.
    ///
    /// The tolerance belongs to the ONE step, not to what precedes it. A recipe's
    /// steps are the concatenation of all its lines, so a tolerance expressed as a
    /// trailing [`Step::Fallback`] would reach back over every earlier line:
    /// EFO's mondo import extracts, writes a temp copy, and only then runs a
    /// tolerated query, and a failed extract has to stop the build rather than
    /// yield an empty import.
    ///
    /// A failed step leaves the pipeline's model as it was — which is what makes
    /// the tolerance safe to express here at all: the step's effects are exactly
    /// what the recipe is prepared to do without.
    MayFail(Box<Step>),
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
    /// The step itself, with any [`Step::MayFail`] wrapper peeled off.
    ///
    /// Tolerating failure says nothing about WHAT the step does, so everything
    /// that asks what a step is — which files it writes, which term files it
    /// reads, whether it is covered — asks the step inside.
    pub fn effective(&self) -> &Step {
        match self {
            Step::MayFail(inner) => inner.effective(),
            other => other,
        }
    }

    /// Human-readable gaps contributed by this step (empty when fully covered).
    pub fn gaps(&self) -> Vec<String> {
        match self {
            Step::Op(_) | Step::Inert(_) | Step::Shell { .. } | Step::Fallback { .. } | Step::Oort(_) => vec![],
            // A boundary is bookkeeping about where one invocation ends, not work.
            Step::Boundary { .. } => vec![],
            Step::File(_) | Step::Jq(_) | Step::Sssom(_) | Step::OwlmakeCli { .. } => vec![],
            Step::Partial { gaps, .. } => gaps.clone(),
            Step::UnsupportedSubcommand(name) => vec![format!("unsupported ontology subcommand `{name}`")],
            Step::UnsupportedOptions { command, options } => unsupported_options(command, options),
            // A refusal is the command's own failure, run when the step is.
            Step::Refused { .. } => vec![],
            Step::MayFail(inner) => inner.gaps(),

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
            Step::UnsupportedSubcommand(name) => vec![format!("unsupported ontology subcommand `{name}`")],
            Step::UnsupportedOptions { command, options } => unsupported_options(command, options),
            Step::MayFail(inner) => inner.unrunnable_gaps(),
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
            Step::Boundary { input } => match input {
                Some(i) => format!("── new invocation, from {i}"),
                None => "── new invocation".to_string(),
            },
            Step::UnsupportedSubcommand(n) => format!("{n} (UNSUPPORTED)"),
            Step::UnsupportedOptions { command, options } => format!("{command} {} (UNSUPPORTED)", options.join(" ")),
            Step::Refused { message } => format!("refused: {message}"),
            Step::OwlmakeCli { name, .. } => format!("om {name}"),
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
            Step::MayFail(inner) => format!("{} (may fail)", inner.label()),
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

/// One gap per option of `command` owlmake does not read.
fn unsupported_options(command: &str, options: &[String]) -> Vec<String> {
    options.iter().map(|o| format!("unsupported option `{command} {o}`")).collect()
}

/// What a `remove` or `filter` asks for, option by option, for its label.
fn selection_label(s: &SelectionSpec) -> Vec<String> {
    let mut bits = vec![];
    for (name, values) in [
        ("term", &s.terms),
        ("term-file", &s.term_files),
        ("include-term", &s.include_terms),
        ("include-terms", &s.include_term_files),
        ("exclude-term", &s.exclude_terms),
        ("exclude-terms", &s.exclude_term_files),
    ] {
        if !values.is_empty() { bits.push(format!("{name}×{}", values.len())); }
    }
    if !s.selects.is_empty() { bits.push(format!("select={}", s.selects.join("+"))); }
    if !s.axioms.is_empty() { bits.push(format!("axioms={}", s.axioms.join("+"))); }
    if !s.base_iri.is_empty() { bits.push(format!("base-iri={}", s.base_iri.join("|"))); }
    for (name, value) in [
        ("trim", &s.trim),
        ("signature", &s.signature),
        ("preserve-structure", &s.preserve_structure),
        ("allow-punning", &s.allow_punning),
    ] {
        if let Some(v) = value { bits.push(format!("{name}={v}")); }
    }
    if !s.drop_axiom_annotations.is_empty() {
        bits.push(format!("drop-axiom-annotations={}", s.drop_axiom_annotations.join("+")));
    }
    bits
}

fn op_label(op: &Op) -> String {
    match op {
        Op::Merge { collapse_import_closure, include_annotations, annotate_defined_by, annotate_derived_from, .. } => {
            let mut label = if *collapse_import_closure == Some(false) {
                "merge (keep imports".to_string()
            } else {
                "merge (+resolve imports".to_string()
            };
            for (on, what) in [
                (*include_annotations, "+annotations"),
                (*annotate_defined_by, "defined-by"),
                (*annotate_derived_from, "derived-from"),
            ] {
                if on {
                    label.push_str(", ");
                    label.push_str(what);
                }
            }
            label.push(')');
            label
        }
        Op::Unmerge { .. } => "unmerge".into(),
        Op::Reason {
            reasoner, equivalent_classes_allowed, exclude_tautologies, axiom_generators, ..
        } => format!(
            "reason[{}{}{}{}]",
            reasoner.clone().unwrap_or_else(|| "ELK".into()),
            equivalent_classes_allowed.as_ref().map(|e| format!(", eq={e}")).unwrap_or_default(),
            exclude_tautologies.as_ref().map(|e| format!(", tauto={e}")).unwrap_or_default(),
            if axiom_generators.is_empty() {
                String::new()
            } else {
                format!(", generators={}", axiom_generators.join(" "))
            },
        ),
        Op::Relax { include_subclass_of } => {
            if *include_subclass_of {
                "relax[+subclass-of]".into()
            } else {
                "relax".into()
            }
        }
        Op::Reduce { reasoner, include_subproperties, preserve_annotated_axioms, named_classes_only } => format!(
            "reduce[{}{}{}{}]",
            reasoner.clone().unwrap_or_else(|| "ELK".into()),
            if include_subproperties.unwrap_or(false) { ", +subproperties" } else { "" },
            if *preserve_annotated_axioms { ", preserve-annotated" } else { "" },
            if *named_classes_only { ", named-classes-only" } else { "" },
        ),
        Op::Materialize { reasoner, create_new_ontology, properties, term_files } => {
            let mut p = properties.clone();
            for f in term_files { p.push(format!("@{f}")); }
            if let Some(r) = reasoner { p.push(format!("reasoner={r}")); }
            if create_new_ontology == &Some(true) { p.push("new-ontology".into()); }
            format!("materialize[{}]", p.join(" "))
        }
        Op::Remove(s) => format!("remove[{}]", selection_label(s).join(", ")),
        Op::Filter(s) => format!("filter[{}]", selection_label(s).join(", ")),
        Op::Annotate(s) => {
            let mut bits = vec![];
            if s.remove_annotations { bits.push("remove".to_string()); }
            if s.ontology_iri.is_some() { bits.push("ont-iri".to_string()); }
            if s.version_iri.is_some() { bits.push("ver-iri".to_string()); }
            let header = s.annotations.len()
                + s.link_annotations.len()
                + s.language_annotations.len()
                + s.typed_annotations.len();
            if header > 0 { bits.push(format!("+{header}ann")); }
            if !s.axiom_annotations.is_empty() { bits.push(format!("axiom-ann×{}", s.axiom_annotations.len() / 2)); }
            if !s.annotation_files.is_empty() { bits.push(format!("file×{}", s.annotation_files.len())); }
            if s.interpolate { bits.push("interpolate".to_string()); }
            if s.annotate_derived_from { bits.push("derived-from".to_string()); }
            if s.annotate_defined_by { bits.push("defined-by".to_string()); }
            format!("annotate[{}]", bits.join(", "))
        }
        Op::Convert { format, .. } => format!("convert[{}]", format.clone().unwrap_or_else(|| "owl".into())),
        Op::Collapse { threshold, precious, .. } => {
            format!("collapse[t={}, {} precious]", threshold.as_deref().unwrap_or("2"), precious.len())
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
        Op::Prefixes { prefixes, noprefixes, add_prefixes, prefix, add_prefix } => {
            let mut bits = vec![];
            if let Some(file) = prefixes {
                bits.push(format!("prefixes={file}"));
            }
            if *noprefixes {
                bits.push("noprefixes".to_string());
            }
            bits.extend(add_prefixes.iter().map(|f| format!("add-prefixes={f}")));
            if !prefix.is_empty() {
                bits.push(format!("{} bound", prefix.len()));
            }
            if !add_prefix.is_empty() {
                bits.push(format!("{} added", add_prefix.len()));
            }
            format!("prefixes[{}]", bits.join(", "))
        }
        Op::Template { templates, merge, collapse_import_closure, ancestors, force } => format!(
            "template[×{}{}{}{}{}]",
            templates.len(),
            if *merge { ", merge" } else { "" },
            if *collapse_import_closure { ", collapse imports" } else { "" },
            if *ancestors { ", ancestors" } else { "" },
            if *force { ", force" } else { "" }
        ),
        Op::Rename { mappings, prefix_mappings, .. } => {
            let m = if mappings.is_some() { "mappings" } else if prefix_mappings.is_some() { "prefix-mappings" } else { "" };
            format!("rename[{m}]")
        }
        Op::Extract { method, terms, term_files, force, .. } => format!(
            "extract[{}, term×{}, term-file×{}{}]",
            method,
            terms.len(),
            term_files.len(),
            if *force { ", forced" } else { "" }
        ),
        Op::Write { path } => format!("write[{path}]"),
        Op::Query { updates, selects, constructs, .. } => {
            let mut bits = vec![];
            if !updates.is_empty() { bits.push(format!("update×{}", updates.len())); }
            if !selects.is_empty() { bits.push(format!("select×{}", selects.len())); }
            if !constructs.is_empty() { bits.push(format!("construct×{}", constructs.len())); }
            format!("query[{}]", bits.join(", "))
        }
        Op::Repair { invalid_references, merge_axiom_annotations, annotation_properties, annotation_properties_file } => {
            let mut bits: Vec<String> = vec![];
            if *merge_axiom_annotations { bits.push("merge-axiom-annotations".into()); }
            if *invalid_references { bits.push("invalid-references".into()); }
            if !annotation_properties.is_empty() { bits.push(format!("annotation-property×{}", annotation_properties.len())); }
            if let Some(f) = annotation_properties_file { bits.push(format!("annotation-properties-file {f}")); }
            if bits.is_empty() { bits.push("nothing".into()); }
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
