//! `remove` — drop selected axioms. Supports the term blacklist
//! (`--term`/`--term-file`), axiom-category selectors (`--axioms
//! external|equivalent|disjoint`), `--select imports`, the `--base-iri`
//! base-module selector, and the `--select complement --select
//! object-properties` relationship pruning a `-basic` release needs.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;
use horned_owl::model::{ClassExpression as CE, Component, ObjectPropertyExpression as OPE};

use crate::cmd::select;
use crate::model::Model;
use crate::sig;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// Terms (IRIs or CURIEs) to remove. Repeatable.
    #[arg(short = 't', long)]
    pub term: Vec<String>,
    /// Files listing terms to remove, one per line.
    #[arg(short = 'T', long)]
    pub term_file: Vec<PathBuf>,
    /// Selectors: `imports`, `complement`, `object-properties` (repeatable).
    #[arg(short = 's', long)]
    pub select: Vec<String>,
    /// Axiom categories to remove: `external`, `equivalent`, `disjoint`.
    #[arg(short = 'a', long)]
    pub axioms: Vec<String>,
    /// If false, do not preserve hierarchical relationships (`<bool>`).
    #[arg(short = 'p', long, num_args = 1, default_missing_value = "true")]
    pub preserve_structure: Option<bool>,
    /// If true, remove axioms containing any selected object (`<bool>`).
    #[arg(short = 'r', long, num_args = 1, default_missing_value = "true")]
    pub trim: Option<bool>,
    /// Terms to force-exclude from removal (never removed). Repeatable.
    #[arg(short = 'e', long = "exclude-term", value_name = "TERM")]
    pub exclude_term: Vec<String>,
    /// Files of terms to force-exclude from removal. Repeatable.
    #[arg(short = 'E', long = "exclude-terms", value_name = "FILE")]
    pub exclude_terms: Vec<PathBuf>,
    /// Terms to force-include in removal. Repeatable.
    #[arg(short = 'n', long = "include-term", value_name = "TERM")]
    pub include_term: Vec<String>,
    /// Files of terms to force-include in removal. Repeatable.
    #[arg(short = 'N', long = "include-terms", value_name = "FILE")]
    pub include_terms: Vec<PathBuf>,
    /// Drop axiom annotations involving a particular annotation property, or
    /// `all`/`true` to strip every annotation from kept axioms.
    #[arg(short = 'd', long = "drop-axiom-annotations", value_name = "ARG")]
    pub drop_axiom_annotations: Option<String>,
    /// If true, keep axioms with any selected entity in their signature when
    /// deciding what to remove (`<bool>`).
    #[arg(short = 'S', long, num_args = 1, default_missing_value = "true")]
    pub signature: Option<bool>,
    /// If true, allow selecting punned entities (widens IRI matching) (`<bool>`).
    #[arg(long = "allow-punning", num_args = 1, default_missing_value = "true")]
    pub allow_punning: Option<bool>,
    /// Base IRI(s) defining "internal" terms for `--axioms external`. Repeatable.
    #[arg(long = "base-iri", value_name = "IRI")]
    pub base_iri: Vec<String>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

/// Options for the `remove`/`filter` term-set machinery, shared so callers can
/// extend behaviour without growing positional argument lists.
#[derive(Default, Clone)]
pub struct TermOptions {
    /// Whether an annotation assertion's IRI VALUE counts as an object of the axiom.
    ///
    /// True (the default) is the command-line meaning: removing an entity takes the
    /// assertions that point AT it. The in-process subsetters set it false — they
    /// remove a set of classes structurally, and an assertion on a class they KEEP
    /// must survive even when it names a class they drop, which is how a species
    /// subset keeps its `RO_0002175 … NCBITaxon_9606` assertions.
    pub annotation_values: Option<bool>,
    /// Force-exclude terms (read from `--exclude-term`/`--exclude-terms`).
    pub exclude_term: Vec<String>,
    pub exclude_terms: Vec<PathBuf>,
    /// Force-include terms (read from `--include-term`/`--include-terms`).
    pub include_term: Vec<String>,
    pub include_terms: Vec<PathBuf>,
    /// `--drop-axiom-annotations` argument, if any.
    pub drop_axiom_annotations: Option<String>,
    /// `--signature`: match on any-signature-entity intersection.
    pub signature: Option<bool>,
    /// `--trim`: keep/remove axioms containing only/any selected objects.
    pub trim: Option<bool>,
    /// `--allow-punning`.
    pub allow_punning: Option<bool>,
    /// `--preserve-structure` (default true): bridge the hierarchy across
    /// removed classes so retained subclasses inherit retained superclass
    /// expressions of the removed ones.
    pub preserve_structure: Option<bool>,
}

impl Args {
    fn term_options(&self) -> TermOptions {
        TermOptions {
            annotation_values: None,
            exclude_term: self.exclude_term.clone(),
            exclude_terms: self.exclude_terms.clone(),
            include_term: self.include_term.clone(),
            include_terms: self.include_terms.clone(),
            drop_axiom_annotations: self.drop_axiom_annotations.clone(),
            signature: self.signature,
            trim: self.trim,
            allow_punning: self.allow_punning,
            preserve_structure: self.preserve_structure,
        }
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(
    piped: Option<crate::model::Model>,
    args: &Args,
) -> anyhow::Result<Option<crate::model::Model>> {
    // The whole closure is loaded, as it is for every command: what an import
    // declares is part of what the document says, and the render of the result
    // turns on it — an entity the closure declares gets no stub, so a module built
    // from an import-bearing source carries a different entity list depending on
    // whether the imports were followed. The result is still the ROOT ontology,
    // with its `owl:imports` intact (`maybe_save`).
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    let mut kept = remove_with(
        model,
        &args.term,
        &args.term_file,
        &args.select,
        &args.axioms,
        &args.base_iri,
        &args.term_options(),
    )?;
    crate::cmd::maybe_save(&mut kept, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(kept))
}

/// Entry point that takes the default term options (no exclude/include/drop).
pub fn remove(
    model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    axioms: &[String],
    base_iri: &[String],
) -> Result<crate::model::Model> {
    remove_with(model, term, term_file, select, axioms, base_iri, &TermOptions::default())
}

/// Apply the `remove` selectors to `model` (pure core).
pub fn remove_with(
    mut model: crate::model::Model,
    term: &[String],
    term_file: &[std::path::PathBuf],
    select: &[String],
    axioms: &[String],
    base_iri: &[String],
    opts: &TermOptions,
) -> Result<crate::model::Model> {
    // Effective removal set = (--term ∪ --include-term); --exclude-term is never
    // removed.
    // Each of the three lists is resolved to ENTITIES, so a punned or unknown IRI
    // silently drops out of all of them — see `select::resolve_entity_terms`.
    let ent = |t| select::resolve_entity_terms(&model, t);
    let mut terms = ent(select::collect_terms(&model, term, term_file)?);
    let included = ent(select::collect_terms(&model, &opts.include_term, &opts.include_terms)?);
    terms.extend(included);
    let excluded = ent(select::collect_terms(&model, &opts.exclude_term, &opts.exclude_terms)?);

    // `--term`/`--term-file` select ENTITIES, so an IRI that names no ENTITY of the
    // ontology selects nothing at all. Narrow the set that way, or a term that
    // appears only as an annotation VALUE still matches (annotation subjects/values
    // are folded into the match in `term_match_with`) and takes assertions that
    // belong in the output.
    //
    // EFO's mondo import is the case: `mondo_exclude.txt` lists `MONDO_0020186`
    // and `MONDO_0019590`, which the BOT module never declares — they occur only
    // as `mondo#excluded_subClassOf` values on retained subjects. Matching on the
    // unresolved IRI would delete those four assertions.
    {
        let ent = select::signature_entities(&model);
        let in_ontology = |t: &String| {
            ent.classes.contains(t)
                || ent.object_properties.contains(t)
                || ent.data_properties.contains(t)
                || ent.annotation_properties.contains(t)
                || ent.individuals.contains(t)
                || ent.datatypes.contains(t)
        };
        terms.retain(in_ontology);
    }

    // `--axioms` values may be space-separated within one argument
    // (e.g. `--axioms "DisjointClasses DisjointUnion"`).
    let axiom_toks: Vec<String> =
        axioms.iter().flat_map(|a| a.split_whitespace()).map(str::to_string).collect();
    let has_ax = |name: &str| axiom_toks.iter().any(|a| a == name);
    let rm_external = has_ax("external");
    let rm_annotation = has_ax("annotation");
    // `structural-tautologies`: `C ⊑ C` and `C ⊑ owl:Thing` (the ones owlmake's
    // reasoner already excludes under `--exclude-tautologies structural`).
    let rm_tautologies = has_ax("structural-tautologies");
    // Every other `--axioms` value goes through the shared classifier, which holds
    // each grouping category's axiom-type set and each single axiom type by name.
    // `external` and `annotation` are excluded because they keep bespoke,
    // term-aware behaviour here.
    let generic_axiom_cats: Vec<String> = axiom_toks
        .iter()
        .filter(|t| {
            !matches!(t.as_str(), "external" | "annotation" | "structural-tautologies")
                && select::is_axiom_category(t)
        })
        .cloned()
        .collect();
    let select_toks: Vec<String> = select
        .iter()
        .flat_map(|s| s.split_whitespace())
        .map(str::to_string)
        .collect();
    let rm_imports = select_toks.iter().any(|s| s == "imports");
    // `--select imports` asks for the root ontology WITHOUT its closure, so the
    // inlining done at load is undone here rather than at save: the axioms the
    // imports lent are dropped and the `owl:imports` declarations go with them,
    // and what the next step in the chain receives is the root alone.
    if rm_imports && !model.imported_components.is_empty() {
        use horned_owl::model::MutableOntology;
        let doomed: Vec<_> = model
            .ont
            .iter()
            .filter(|ac| model.imported_components.contains(*ac))
            .cloned()
            .collect();
        for ac in doomed {
            model.ont.remove(&ac);
        }
    }
    if rm_imports {
        model.detach_import_closure();
    }
    // `--select complement --select <type>` removes every entity of that TYPE that
    // is NOT in the term set. Both the object-property and the annotation-property
    // forms are handled: HPO's `imports/merged_import.owl` step — `remove --term
    // <the annotation properties to keep> --term-file … --select complement
    // --select annotation-properties` — needs the latter to strip
    // `uberon/core#HOMOLOGY` with its fourteen `SynonymTypeProperty` siblings and
    // `cl#added_for_HCA` before they reach `hp-full.owl`, `hp.owl` and
    // `hp-international.owl`.
    let is_complement = select_toks.iter().any(|s| s == "complement");
    let obj_complement = is_complement && select_toks.iter().any(|s| s == "object-properties");
    let ann_complement =
        is_complement && select_toks.iter().any(|s| s == "annotation-properties");
    let type_complement = obj_complement || ann_complement;
    // `--select ontology` selects the ontology itself, so its annotations are
    // removed.
    let rm_ontology = select_toks.iter().any(|s| s == "ontology");

    // Entity selection by IRI/CURIE/wildcard pattern and/or entity-type keyword
    // (`--select "<…BFO_*>" --select classes` → the BFO classes). These join the
    // removal set. Skipped for a TYPE complement, which has its own path below.
    if !type_complement {
        // The SIGNATURE, not just the declarations: the per-kind entity sets are
        // collected from axiom structure, so an entity that is only referenced still
        // selects. MONDO's `mondo-base.owl` build strips the imports that declared
        // `BFO_0000004`/`BFO_0000050` before the later
        // `remove --select "<BFO_*>" --select classes`, which must still see
        // `BFO_0000004` as a class and drop the `rdfs:subClassOf` naming it.
        let ent = select::signature_entities(&model);
        // The `--term`/`-T` set alone, before any `--select` type/pattern
        // selector widens it — the base a later `complement` group inverts.
        let term_only: HashSet<String> = terms.clone();
        let patterns: Vec<String> = select_toks
            .iter()
            .filter(|t| select::is_pattern(t))
            .map(|t| select::expand(&model, t))
            .collect();
        let type_sets: Vec<&HashSet<String>> =
            select_toks.iter().filter_map(|t| select::type_set(&ent, t)).collect();
        if !patterns.is_empty() {
            let all_kinds = [
                &ent.classes, &ent.object_properties, &ent.data_properties,
                &ent.annotation_properties, &ent.individuals, &ent.datatypes,
            ];
            let universe: Vec<&String> = if type_sets.is_empty() {
                all_kinds.iter().flat_map(|s| s.iter()).collect()
            } else {
                type_sets.iter().flat_map(|s| s.iter()).collect()
            };
            for e in universe {
                if patterns.iter().any(|p| select::glob_match(p, e)) {
                    terms.insert(e.clone());
                }
            }
        } else {
            // Type selector(s) with no pattern: select every entity of that type.
            // `category_members` also covers the multi-kind `properties` selector.
            for tok in &select_toks {
                if let Some(s) = select::category_members(&ent, tok) {
                    terms.extend(s);
                } else if let Some((p, v)) = select::parse_annotation_value(tok) {
                    // `PROP=VALUE` selects by annotation, the same selector `filter`
                    // takes — the two commands share one selector language, so a
                    // form one understands cannot be a silent no-op in the other.
                    terms.extend(select::annotation_value_members(&model, p, v));
                }
            }
        }

        // Relation selectors EXPAND the removal seed: `--select
        // descendants`/`children`/… widen a `--term`, as on the filter path.
        let mut extra: HashSet<String> = HashSet::new();
        for kw in &select_toks {
            match kw.as_str() {
                "parents" => extra.extend(select::direct_parents(&model, &terms)),
                "ancestors" => extra.extend(select::ancestors(&model, &terms)),
                "children" => extra.extend(select::direct_children(&model, &terms)),
                "descendants" => extra.extend(select::descendants(&model, &terms)),
                "equivalents" => extra.extend(select::equivalents_of(&model, &terms)),
                "types" => extra.extend(select::types_of(&model, &terms)),
                "instances" => extra.extend(select::instances_of(&model, &terms)),
                "domains" => extra.extend(select::domains_of(&model, &terms)),
                "ranges" => extra.extend(select::ranges_of(&model, &terms)),
                _ => {}
            }
        }
        terms.extend(extra);

        // General `--select complement` (without the `object-properties` special
        // path): invert the selection so the removal set becomes every declared
        // entity NOT currently selected.
        // Over the SIGNATURE, not the declarations, so an undeclared-but-used
        // property like `rdfs:comment` is in the complement and gets removed.
        // Complementing over declarations alone would leave MONDO's
        // `merged_import.owl` carrying 2,536 `rdfs:comment` assertions the step is
        // meant to strip entirely.
        //
        // …but only when something was actually SELECTED to complement. An empty
        // object set means the WHOLE ontology, so its complement is EMPTY and the
        // command removes nothing — such a step is there for its other modifiers.
        // Inverting an empty selection into "every entity" instead emptied the
        // artefact: UBERON's `composite-*-basic.owl` opens with `remove --select
        // complement --drop-axiom-annotations all`, whose whole purpose is the
        // second flag, and it came out at 859 bytes against a 52,085,326-byte
        // reference.
        if select_toks.iter().any(|s| s == "complement") && !terms.is_empty() {
            let inverted: HashSet<String> =
                ent.all().filter(|e| !terms.contains(*e)).cloned().collect();
            terms = inverted;
        }

        // Each `--select` occurrence is its own GROUP, applied in order, each
        // refining the previous group's result — the groups are not flattened.
        // `--select complement --select annotation-properties` therefore means
        // "everything outside the term set, then narrowed to the annotation
        // properties", i.e. every AP the `--term`/`-T` keep set does not name.
        // Flattening would union the APs into the keep set first, so the
        // complement would contain no AP at all and nothing would be stripped:
        // MONDO's `imports/merged_import.owl` would carry 229,686 annotation
        // assertions in place of 31,977.
        let groups: Vec<Vec<String>> = select
            .iter()
            .map(|s| s.split_whitespace().map(str::to_string).collect::<Vec<String>>())
            .collect();
        let ci = groups.iter().position(|g| g.len() == 1 && g[0] == "complement");
        if let Some(ci) = ci {
            let refining: Vec<&Vec<String>> = groups[ci + 1..]
                .iter()
                .filter(|g| g.iter().any(|t| select::category_members(&ent, t).is_some()))
                .collect();
            if !refining.is_empty() {
                // Re-invert from the term set alone: the flattened pass above
                // has already folded the refining groups' own members into
                // `terms`, which would make the complement (and so the
                // intersection) empty.
                terms = ent.all().filter(|e| !term_only.contains(*e)).cloned().collect();
                for g in refining {
                    let mut members: HashSet<String> = HashSet::new();
                    for tok in g {
                        if let Some(s) = select::category_members(&ent, tok) {
                            members.extend(s);
                        }
                    }
                    terms.retain(|e| members.contains(e));
                }
            }
        }
    }
    // `--exclude-term` subtracts from the REMOVAL SET, here at the end, once every
    // selector has finished widening it — that is the whole of its meaning. It does
    // NOT make an axiom that merely mentions the term immune: an excluded term is
    // one the command must not delete, not a shield over everything it touches.
    //
    // Immunity is what this used to do, and it inverted the step it matters most
    // to. UBERON's `merged-partonomy.owl` is `remove --exclude-term BFO:0000050
    // --select object-properties` — drop every object property but part_of — and
    // under immunity every axiom naming part_of survived, including the
    // `SubObjectPropertyOf(X, part_of)` of each property being dropped and every
    // restriction that mentioned part_of alongside another property. The writer
    // then re-declared each X the surviving axioms still referenced, so the file
    // kept 82 object properties and 35 distinct `onProperty` targets where the
    // reference has exactly 1 of each. All 14 `*-minimal` subsets are built from
    // it, so they inherited the same excess.
    for e in &excluded {
        terms.remove(e);
    }

    // Plain term removal applies only when no category selector is in play.
    // `!type_complement`, not `!obj_complement`: under a complement group the
    // `--term`/`--term-file` set is the KEEP set, and running a plain removal over it
    // would delete exactly the axioms the command exists to preserve. With
    // `--select complement --select annotation-properties` that inverts the whole
    // step, taking HPO's merged import down from 42.2 MB to 6.8 MB.
    let plain_terms = !terms.is_empty() && axiom_toks.is_empty() && !type_complement;
    // Whether the caller NAMED any term, before resolution. The object set is the
    // WHOLE ontology only when no `--term`/`--term-file` was given; when terms were
    // given but none of them is in the ontology the set is empty and the command
    // removes nothing at all.
    let named_terms = !term.is_empty() || !term_file.is_empty();
    let ann_values = opts.annotation_values.unwrap_or(true);
    // `--trim` defaults to true: remove an axiom if ANY of its signature
    // entities is selected. With `--trim false`, remove only axioms whose WHOLE
    // signature is selected (axioms mentioning a non-selected entity survive).
    //
    // `--signature` does NOT override that. It used to (`trim || signature`), and
    // the two flags are orthogonal: trim decides ANY-vs-ALL, signature decides what
    // counts as the axiom's objects. Conflating them inverted the one step that
    // keeps labels in the `-basic` composites —
    // `remove --term rdfs:label --select complement --axioms annotation
    // --trim false --signature true`, which means "drop every annotation axiom
    // EXCEPT the labels". Under any-entity semantics the label assertions went too:
    // `composite-metazoan-basic.owl` came out with 0 `rdfs:label` against the
    // reference's 81,374, and its `.obo` with no `name:` line at all — 77,644 of
    // them missing, most of an 83,000-line shortfall.
    let trim = opts.trim.unwrap_or(true);
    // `--allow-punning`: owlmake selects by IRI, which is punning-collapsed —
    // acting on an IRI hits every (class/property/individual) sense of it. When
    // punning is *not* allowed (the default) and a selected term is in fact punned,
    // warn that all senses are affected (the flag cannot scope to one sense here).
    if !opts.allow_punning.unwrap_or(false) {
        warn_punned(&model, &terms);
    }

    // `--preserve-structure` (default true): before deleting a plain term set,
    // bridge the class hierarchy across the removed classes so a retained subclass
    // `C ⊑ X` (X removed) gains `C ⊑ E` for each retained superclass expression `E`
    // of `X` (named or anonymous with fully-retained signature), traversing through
    // chains of removed named superclasses. That is what propagates e.g. PR's
    // `∃output_of.translation` / `∃has_gene_template.…` onto the proteins (and
    // CHEBI classes equivalent to protein) when the protein root `PR:000000001` is
    // removed.
    // The structure-preserving axioms are computed here, against the ontology as it
    // stands *before* the removal, and inserted below once the removal has been
    // applied, so the filter cannot strip the very axioms the pass exists to add.
    let mut span_shared: HashMap<String, u64> = HashMap::new();
    let mut cross_add_out: HashMap<String, u64> = HashMap::new();

    let base_iris: Vec<String> = base_iri.iter().map(|b| select::expand(&model, b)).collect();

    // For the object-property complement: the object properties to *drop* are
    // those used in the ontology but absent from the kept set (`terms`).
    let removed_props: HashSet<String> = if obj_complement {
        object_properties(&model).difference(&terms).cloned().collect()
    } else {
        HashSet::new()
    };
    // The removal set is every annotation property the keep set does not name —
    // and the keep set is far larger than the three `--term` properties, because the
    // `--term-file`s (HPO's ten `*_terms.txt` plus a 24,573-line `tmp/seed.txt`)
    // name `oboInOwl:hasDbXref`, `hasExactSynonym` and the rest. So what actually
    // goes is a handful of stragglers like `uberon/core#HOMOLOGY` and
    // `cl#added_for_HCA`, with their assertions.
    let removed_ann_props: HashSet<String> = if ann_complement {
        let ent = select::signature_entities(&model);
        // Minus the OTHER senses of the same IRI. owlmake selects by IRI, which is
        // punning-collapsed, so an IRI declared both an annotation property and an
        // object property (or a class) would be dropped in ALL its senses — taking
        // with it every `SubClassOf(… ObjectSomeValuesFrom(<that IRI> …))` that used
        // it logically. Only the annotation-property sense goes, which keeps 1,897
        // existential restrictions and 1,156 plain subclass axioms a collapsed
        // by-IRI test would discard.
        ent.annotation_properties
            .difference(&terms)
            .filter(|e| !ent.object_properties.contains(*e) && !ent.classes.contains(*e))
            .cloned()
            .collect()
    } else {
        HashSet::new()
    };

    // Gap spanning runs over the COMPLEMENT of the removal set — the entities that
    // SURVIVE — against the pre-removal ontology, so it re-asserts each retained
    // class's hierarchy whether or not a gap was opened next to it. Under a TYPE
    // complement `terms` is the KEEP set, so the removal set is the properties
    // computed above; passing `terms` there would bridge over everything the
    // command exists to preserve.
    //
    // MP's import modules depend on this. A generic import step ends
    // `remove … --select complement --select annotation-properties` with
    // preserve-structure at its default, and CL's `SubClassOf(Annotation(
    // oboInOwl:is_inferred "true") CL_0000015 CL_0000586)` goes with the
    // annotation property, so the unannotated re-assertion has to take its place —
    // 212 such axioms in `cl_import.owl` and 4,163 in `uberon_import.owl`.
    // A plain `--select complement` puts every ANONYMOUS object in the removal set
    // as well as the entities: the complement is taken over every OBJECT an axiom
    // yields — a `SubClassOf`'s two class expressions, an n-ary class axiom's
    // operands, a property axiom's nested class expressions, an assertion's
    // anonymous individuals and a HasKey's expressions, on top of the signature —
    // minus only the named term set. So every anonymous expression in the ontology
    // lands in the removal set, and the axiom carrying it is dropped whatever its
    // entities are (the `--signature false` default matches partial axioms,
    // anonymous objects included). MP's `nbo_import.owl` shows the scale: the module
    // holds NO anonymous class expression at all, where a signature-only reading
    // would keep every one whose entities are all seeded.
    //
    // A TYPE complement (`--select complement --select annotation-properties`)
    // narrows the complement back to entities of that type, so no anonymous object
    // survives into its removal set and the rule does not apply.
    let anon_selected = is_complement && !type_complement;
    let removal_set: HashSet<String> = if type_complement {
        removed_props.union(&removed_ann_props).cloned().collect()
    } else {
        terms.clone()
    };
    let keep_ac = |ac_full: &horned_owl::model::AnnotatedComponent<Rc>| {
        let comp: &Component<_> = &ac_full.component;
        if matches!(comp, Component::OntologyID(_) | Component::DocIRI(_)) {
            return true;
        }
        if matches!(comp, Component::OntologyAnnotation(_)) {
            // Kept unless `--select ontology` asked to strip ontology annotations.
            return !rm_ontology;
        }
        // An axiom-TYPE selector removes the intersection of the type and the
        // selected objects. Only `internal`/`external`/`tautologies` ignore the
        // objects, which is why `rm_external` above is not gated. uPheno's merged
        // mirror shows what is at stake: `remove --term owl:Nothing --axioms
        // logical` drops the logical axioms that mention `owl:Nothing`, where
        // removing every logical axiom instead would leave `mirror/merged.owl` with
        // 22 SubClassOf axioms in place of 798,674 — the whole phenotype hierarchy,
        // and with it every shortcut relation the step before had just added.
        let axiom_type_terms = !named_terms || term_match_ac(ac_full, &terms, trim, ann_values);
        let remove = (rm_external && is_external(comp, &base_iris))
            || (rm_tautologies && is_structural_tautology(comp))
            || (rm_imports && matches!(comp, Component::Import(_)))
            // The object-property complement removes every axiom whose SIGNATURE
            // meets a dropped property — the same partial-axiom test as a plain
            // `--term` removal, annotation-assertion subjects included. A narrower
            // "does this axiom USE the property" test would leave the property
            // itself behind: OBA's `oba-basic.owl` would keep `RO_0000052`'s
            // `FunctionalObjectProperty` axiom and all six of its annotation
            // assertions, so the property would still be an entity of the released
            // ontology even though every axiom that used it had gone.
            || (obj_complement && term_match_ac(ac_full, &removed_props, true, ann_values))
            || (ann_complement && term_match_ac(ac_full, &removed_ann_props, true, ann_values))
            || (rm_annotation && annotation_axiom_match(comp, &terms, trim, named_terms, ann_values))
            || (!generic_axiom_cats.is_empty()
                && generic_axiom_cats
                    .iter()
                    .any(|c| select::axiom_in_category(comp, c, &base_iris))
                && axiom_type_terms)
            || (plain_terms && term_match_ac(ac_full, &terms, trim, ann_values))
            || (plain_terms && anon_selected && has_anonymous_object(comp));
        !remove
    };

    // The gap-spanning object set is the complement of the removal set taken over
    // the ontology *as it stands after the removal*: the complement is unioned over
    // the axioms that are still there. So an entity survives into that set only if
    // it is BOTH kept and still mentioned by a surviving axiom, and a bridge is
    // emitted only when the whole superclass signature is inside it.
    //
    // MP's `nbo_import.owl` is exactly this case. `BFO_0000050` is in the import
    // seed, so it is never in the removal set — but the step before
    // (`remove --axioms external --base-iri …/NBO`) took its declaration, and the
    // `--select complement` step then removes `SubClassOf(NBO_0000447,
    // ObjectSomeValuesFrom(BFO_0000050, NBO_0000013))` along with every other axiom
    // carrying an anonymous expression. With no axiom left to mention it,
    // `BFO_0000050` drops out of the object set and nothing is rebuilt; bridging on
    // the keep set alone would re-assert the axiom and leave the property behind —
    // and, downstream, a `relationship: BFO:0000050 NBO:0000013` line in
    // `mp-full.obo` for a term the module does not carry.
    let surviving: HashSet<String> = model
        .ont
        .iter()
        .filter(|ac| keep_ac(ac))
        .flat_map(|ac| sig::typed_signature(&ac.component).into_iter().map(|(_, i)| i))
        .collect();

    // Gap spanning is gated on an object set having been SELECTED, not on the kind
    // of removal: `--axioms` narrows which axioms go, and leaves the pass to
    // re-assert the hierarchy over everything the selection did not name. FoodOn's
    // mirror recipe is the case — `remove --term FOODON_02010002 --axioms
    // equivalent` takes out one equivalence and re-links `animal egg`'s ten
    // subclasses to `UBERON_0002050` above it, and gives the two annotated
    // `RO_0002351` restrictions on `FOODON_03400324` their unannotated twins.
    let selected_objects = type_complement || !terms.is_empty();
    let bridges = if opts.preserve_structure.unwrap_or(true) && selected_objects {
        span_gaps_shared(
            &model,
            &removal_set,
            &excluded,
            Some(&surviving),
            &mut span_shared,
            &mut cross_add_out,
        )
    } else {
        Vec::new()
    };

    let mut kept = select::retain_ac(model, keep_ac);

    // Add the structure-preserving axioms now that the removal has been applied.
    {
        use horned_owl::model::MutableOntology;
        for b in bridges {
            kept.ont.insert(b);
        }
    }
    if let Some(drop) = &opts.drop_axiom_annotations {
        drop_axiom_annotations(&mut kept, drop);
    }
    if std::env::var("OM_SPAN_LOG").is_ok() {
        let mut out = String::new();
        let mut v: Vec<_> = span_shared.iter().collect();
        v.sort();
        for (k, g) in v {
            out.push_str(&format!("{g}\t{}\n", k.replace('\u{1}', " | ")));
        }
        std::fs::write("/tmp/om_span_groups.txt", out).ok();
    }
    // `--axioms external` takes the verbatim anonymous-subject blocks with it.
    // Those are annotation assertions horned-owl's RDF reader drops, replayed as
    // text by the writer, so no axiom predicate can reach them — but an
    // anonymous-subject assertion has an anonymous subject, which is in no base
    // namespace, so every one of them is external and must go with the rest.
    // EFO's `efo-base.owl` is `remove --base-iri …/EFO_ --axioms external`: leaving
    // the replay in place would give it a whole `Individuals` section — 95 lines of
    // obsolescence records for OBA terms — in a file that is meant to hold only
    // EFO's own axioms.
    if rm_external {
        kept.owl_anon_blocks.clear();
    }
    kept.span_shared.extend(span_shared);
    kept.cross_shared.extend(cross_add_out);
    Ok(kept)
}

/// `remove --axioms annotation`: an `AnnotationAssertion` is a candidate.
/// With a `--term` set, only those whose annotation property, subject, or
/// IRI-value is selected are removed (the annotation property is not in the
/// logical signature, so it is matched explicitly); with no terms, every
/// annotation assertion is removed.
/// Whether `--axioms annotation` should take this assertion, given the object set.
///
/// Honours `trim`, which it previously ignored — it matched on the subject alone,
/// so an assertion died with its subject however the caller had narrowed the set.
/// That inverted the step that keeps labels in the `-basic` composites: `remove
/// --term rdfs:label --select complement --axioms annotation --trim false` means
/// "drop every annotation axiom EXCEPT the labels", and matching on the subject
/// dropped the labels along with everything else.
fn annotation_axiom_match(
    comp: &Component<horned_owl::model::RcStr>,
    terms: &HashSet<String>,
    trim: bool,
    named_terms: bool,
    ann_values: bool,
) -> bool {
    use horned_owl::model::{AnnotationSubject, AnnotationValue};
    // The annotation-axiom FAMILY, not just assertions: `SubAnnotationPropertyOf`,
    // `AnnotationPropertyDomain` and `AnnotationPropertyRange` are annotation axioms
    // too, and `--axioms annotation` takes them under the same term test — their
    // objects being the properties they name. The `-basic` composites' last step
    // (`--term rdfs:label --select complement --axioms annotation --trim false`)
    // drops 131 `SubAnnotationPropertyOf` axioms this way.
    let family_objects: Option<Vec<String>> = match comp {
        Component::SubAnnotationPropertyOf(sp) => {
            Some(vec![sp.sub.0.to_string(), sp.sup.0.to_string()])
        }
        Component::AnnotationPropertyDomain(d) => Some(vec![d.ap.0.to_string()]),
        Component::AnnotationPropertyRange(r) => Some(vec![r.ap.0.to_string()]),
        _ => None,
    };
    if let Some(objs) = family_objects {
        if terms.is_empty() {
            return !named_terms;
        }
        return if trim {
            objs.iter().any(|o| terms.contains(o))
        } else {
            objs.iter().all(|o| terms.contains(o))
        };
    }
    let Component::AnnotationAssertion(aa) = comp else {
        return false;
    };
    if terms.is_empty() {
        // The object set is the whole ontology only when no term was NAMED. A term
        // that WAS named and is not in the ontology leaves an empty set, which
        // selects nothing — so the command removes nothing rather than everything.
        return !named_terms;
    }
    // An annotation assertion's objects are its property, its subject and — when the
    // value is an IRI — that value. A literal value names nothing, so it contributes
    // no object and never keeps the assertion alive.
    //
    // The value carries the `-basic` composites. Their last step selects the
    // complement of `rdfs:label` over annotation axioms with `--trim false`, so an
    // assertion goes only when ALL of its objects are selected. An IRI naming no
    // entity of the ontology is in no entity-derived set, so the assertion is not
    // wholly selected and is spared — which is exactly the set of `dcterms:contributor`
    // assertions the reference keeps.
    let property = Some(aa.ann.ap.0.to_string());
    let subject = match &aa.subject {
        AnnotationSubject::IRI(iri) => Some(iri.to_string()),
        AnnotationSubject::AnonymousIndividual(_) => None,
    };
    let value = match &aa.ann.av {
        AnnotationValue::IRI(iri) if ann_values => Some(iri.to_string()),
        _ => None,
    };
    if trim {
        // ANY: one selected object condemns the assertion, so removing an entity
        // takes the assertions that point AT it as well as the ones about it.
        return [&property, &subject, &value].into_iter().flatten().any(|p| terms.contains(p));
    }
    let parts = [property, subject, value];
    // ALL of the named parts must be selected, and there must be at least one —
    // the property alone is never enough to condemn an assertion whose subject the
    // caller kept.
    let mut seen = false;
    for p in parts.iter().flatten() {
        seen = true;
        if !terms.contains(p) {
            return false;
        }
    }
    seen
}

/// Warn (once, listing up to a few) when a selected term is punned — declared as
/// more than one entity kind — since owlmake's IRI-based selection acts on all
/// senses regardless of `--allow-punning`.
fn warn_punned(model: &Model, terms: &HashSet<String>) {
    let ent = select::entities(model);
    let kinds = [
        &ent.classes,
        &ent.object_properties,
        &ent.data_properties,
        &ent.annotation_properties,
        &ent.individuals,
        &ent.datatypes,
    ];
    let punned: Vec<&String> = terms
        .iter()
        .filter(|t| kinds.iter().filter(|k| k.contains(*t)).count() > 1)
        .collect();
    if !punned.is_empty() {
        status!(
            "note: {} selected term(s) are punned; owlmake selects by IRI so all senses are affected (e.g. <{}>)",
            punned.len(),
            punned[0]
        );
    }
}

/// Whether the objects of this axiom include an ANONYMOUS one — an unnamed class
/// expression, an anonymous individual, or an inverse property expression.
///
/// The object set starts from the signature (named entities only) and then, for a
/// listed axiom shape, adds the expressions themselves. Only those shapes can
/// contribute an anonymous object, so only those are checked here. Under a plain
/// `--select complement` every such object is in the removal set, so an axiom that
/// yields one is dropped.
fn has_anonymous_object(comp: &Component<Rc>) -> bool {
    use horned_owl::model::ClassExpression as CE;
    use horned_owl::model::Individual;
    use horned_owl::model::ObjectPropertyExpression as OPE;

    let anon_ce = |ce: &CE<Rc>| !matches!(ce, CE::Class(_));
    let anon_ind = |i: &Individual<Rc>| matches!(i, Individual::Anonymous(_));
    let anon_ope = |p: &OPE<Rc>| matches!(p, OPE::InverseObjectProperty(_));
    // A property axiom contributes its NESTED class expressions, which reach inside
    // a domain/range expression as well as being it.
    let nested_anon = |ce: &CE<Rc>| {
        let mut found = false;
        let mut stack = vec![ce];
        while let Some(c) = stack.pop() {
            if anon_ce(c) {
                found = true;
            }
            match c {
                CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => stack.extend(v.iter()),
                CE::ObjectComplementOf(b) => stack.push(b),
                CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => {
                    stack.push(bce)
                }
                CE::ObjectMinCardinality { bce, .. }
                | CE::ObjectMaxCardinality { bce, .. }
                | CE::ObjectExactCardinality { bce, .. } => stack.push(bce),
                _ => {}
            }
        }
        found
    };

    match comp {
        Component::SubClassOf(sc) => anon_ce(&sc.sub) || anon_ce(&sc.sup),
        Component::EquivalentClasses(e) => e.0.iter().any(anon_ce),
        Component::DisjointClasses(d) => d.0.iter().any(anon_ce),
        Component::DisjointUnion(d) => d.1.iter().any(anon_ce),
        Component::ClassAssertion(ca) => anon_ce(&ca.ce),
        Component::HasKey(hk) => {
            anon_ce(&hk.ce)
                || hk.vpe.iter().any(|p| {
                    matches!(p, horned_owl::model::PropertyExpression::ObjectPropertyExpression(o) if anon_ope(o))
                })
        }
        Component::SameIndividual(s) => s.0.iter().any(anon_ind),
        Component::DifferentIndividuals(d) => d.0.iter().any(anon_ind),
        Component::ObjectPropertyAssertion(a) => anon_ind(&a.from) || anon_ind(&a.to),
        Component::NegativeObjectPropertyAssertion(a) => anon_ind(&a.from) || anon_ind(&a.to),
        Component::ObjectPropertyDomain(d) => nested_anon(&d.ce),
        Component::ObjectPropertyRange(r) => nested_anon(&r.ce),
        _ => false,
    }
}

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

/// A structural tautology: `C ⊑ C` or `C ⊑ owl:Thing` (matching what the reasoner
/// excludes under `--exclude-tautologies structural`).
fn is_structural_tautology(comp: &Component<horned_owl::model::RcStr>) -> bool {
    use horned_owl::model::ClassExpression as CE;
    let Component::SubClassOf(sc) = comp else { return false };
    if sc.sub == sc.sup {
        return true;
    }
    matches!(&sc.sup, CE::Class(c) if c.0.as_ref() == OWL_THING)
}

type Rc = horned_owl::model::RcStr;

/// Collect the class/object-property IRIs referenced anywhere in a class
/// expression (individuals and data ranges are ignored — `remove --term` targets
/// classes/properties). Used to decide whether an expression's signature is
/// fully retained when bridging the hierarchy.
fn ce_iris(ce: &CE<Rc>, out: &mut HashSet<String>) {
    let role = |ope: &OPE<Rc>, out: &mut HashSet<String>| match ope {
        OPE::ObjectProperty(p) => {
            out.insert(p.0.to_string());
        }
        OPE::InverseObjectProperty(p) => {
            out.insert(p.0.to_string());
        }
    };
    match ce {
        CE::Class(c) => {
            out.insert(c.0.to_string());
        }
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => {
            for x in v {
                ce_iris(x, out);
            }
        }
        CE::ObjectComplementOf(b) => ce_iris(b, out),
        CE::ObjectSomeValuesFrom { ope, bce } | CE::ObjectAllValuesFrom { ope, bce } => {
            role(ope, out);
            ce_iris(bce, out);
        }
        CE::ObjectMinCardinality { ope, bce, .. }
        | CE::ObjectMaxCardinality { ope, bce, .. }
        | CE::ObjectExactCardinality { ope, bce, .. } => {
            role(ope, out);
            ce_iris(bce, out);
        }
        CE::ObjectHasValue { ope, .. } => role(ope, out),
        CE::ObjectHasSelf(ope) => role(ope, out),
        _ => {}
    }
}

/// Produce the `SubClassOf` axioms that bridge the hierarchy across removed
/// classes. For each retained class `C` with a direct removed
/// *named* superclass, walk up through chains of removed named superclasses and,
/// for every retained superclass expression `E` reached (named, or anonymous
/// with a fully-retained signature), emit `C ⊑ E`.
pub(crate) fn span_gaps(
    model: &Model,
    terms: &HashSet<String>,
    excluded: &HashSet<String>,
) -> Vec<Component<Rc>> {
    let mut ignored = HashMap::new();
    let mut ignored2 = HashMap::new();
    span_gaps_shared(model, terms, excluded, None, &mut ignored, &mut ignored2)
}

/// `surviving`, when given, is the signature of the axioms that outlive the
/// removal. `remove` derives the bridging object set from the ontology as it
/// stands *after* the removal, so an entity that is kept but no longer mentioned
/// anywhere is not in it and cannot appear in a bridge. `filter` bridges against
/// its selection directly, with no such restriction, so it passes `None`.
pub(crate) fn span_gaps_shared(
    model: &Model,
    terms: &HashSet<String>,
    excluded: &HashSet<String>,
    surviving: Option<&HashSet<String>>,
    shared_out: &mut HashMap<String, u64>,
    cross_out: &mut HashMap<String, u64>,
) -> Vec<Component<Rc>> {
    use horned_owl::model::SubClassOf;
    let is_removed = |iri: &str| {
        (terms.contains(iri) && !excluded.contains(iri))
            || surviving.is_some_and(|s| !s.contains(iri))
    };

    // Superclass expressions per named class IRI (from SubClassOf + the named
    // members of EquivalentClasses, flattening an intersection definition).
    let mut sup: HashMap<String, Vec<CE<Rc>>> = HashMap::new();
    // Equivalence map: named class -> the named classes asserted equivalent to it.
    // A superclass that is also asserted equivalent to the class is not a gap, so a
    // class equivalent to a removed one (e.g. `CHEBI_36080 ≡ PR:000000001`) is NOT
    // bridged — only its *subclasses* inherit the removed class's superclass
    // expressions.
    let mut equiv: HashMap<String, HashSet<String>> = HashMap::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubClassOf(sc) => {
                if let CE::Class(c) = &sc.sub {
                    sup.entry(c.0.to_string()).or_default().push(sc.sup.clone());
                }
            }
            Component::EquivalentClasses(eq) => {
                let named: Vec<&str> = eq
                    .0
                    .iter()
                    .filter_map(|m| match m {
                        CE::Class(c) => Some(c.0.as_ref()),
                        _ => None,
                    })
                    .collect();
                for a in &named {
                    for b in &named {
                        if a != b {
                            equiv.entry(a.to_string()).or_default().insert(b.to_string());
                        }
                    }
                }
                // NOTE: an EquivalentClasses definition is deliberately NOT a source
                // of superclasses. The superclass expressions are the SubClassOf
                // axioms alone; the equivalence only feeds the exclusion test above.
                // Flattening a genus-differentia definition into `sup` here would
                // invent bridges the asserted hierarchy does not license (and is why
                // MONDO's `intersection_of:` lines stay untouched by this pass).
            }
            _ => {}
        }
    }

    let ce_retained = |ce: &CE<Rc>| {
        let mut iris = HashSet::new();
        ce_iris(ce, &mut iris);
        !iris.iter().any(|i| is_removed(i))
    };

    let empty_eq: HashSet<String> = HashSet::new();
    // The asserted superclass expressions of `x`, minus any *named* one that is also
    // asserted equivalent to `x`, minus a self-loop. Anonymous expressions are
    // always kept.
    // Each superclass expression is carried with the identity of the source axiom
    // it came from — `(owning class, index)`. A re-link reuses that source
    // expression, so two re-links from one source are one blank node.
    // The expressions form a SET: two structurally-equal superclass expressions on
    // one class collapse to the first of them. The set is then walked in hash-bucket
    // order, which decides which source expression a re-link is attributed to when a
    // class can reach one structure through more than one removed ancestor — and so
    // which of the re-links this pass emits share a blank node. The order is a fixed
    // function of the expressions themselves, so one input always spends the same
    // blank nodes on the same bridges.
    let super_classes_of = |x: &str| -> Vec<(CE<Rc>, (String, String))> {
        let x_equiv = equiv.get(x).unwrap_or(&empty_eq);
        let Some(v) = sup.get(x) else { return Vec::new() };
        let mut seen: HashSet<String> = HashSet::new();
        let mut items: Vec<(CE<Rc>, (String, String))> = Vec::new();
        for (i, e) in v.iter().enumerate() {
            if let CE::Class(y) = e {
                if y.0.as_ref() == x || x_equiv.contains(y.0.as_ref()) {
                    continue;
                }
            }
            // A duplicate structure keeps the instance already present.
            if !seen.insert(crate::io::genid::ce_sig(e)) {
                continue;
            }
            items.push((e.clone(), (x.to_string(), i.to_string())));
        }
        let cap = crate::io::obo::owlapi_set_cap(items.len());
        let mut with_bucket: Vec<(usize, usize, (CE<Rc>, (String, String)))> = items
            .into_iter()
            .enumerate()
            .map(|(ins, it)| {
                let h = crate::io::obo::owlapi_ce_hash(&it.0);
                let spread = (h ^ ((h as u32) >> 16) as i32) as usize;
                (spread & (cap - 1), ins, it)
            })
            .collect();
        with_bucket.sort_by_key(|(b, ins, _)| (*b, *ins));
        with_bucket.into_iter().map(|(_, _, it)| it).collect()
    };

    // The (sub, super) pairs are deduped as they are built, keeping whichever
    // superclass EXPRESSION was reached first. So which re-links end up sharing a
    // blank node is decided by the order the classes are walked in, not by anything
    // structural. Walk them in hash-bucket order: bucket index
    // `(h ^ h>>>16) & (cap-1)`, buckets ascending, ties broken by insertion order.
    //
    // A Rust `HashMap`'s iteration order is randomised per process, so it cannot
    // stand in here: the same build would emit different blank-node numbering run
    // to run.
    let ordered_classes: Vec<String> = {
        let mut keys: Vec<&String> = sup.keys().collect();
        keys.sort();
        // Capacity comes from the count of RETAINED entities — the object set the
        // pass re-asserts the hierarchy over — not from `keys.len()`. The classes
        // that have superclasses are only a subset of that set, so sizing the table
        // to the subset would land them in different buckets and change which
        // re-links share a blank node.
        let retained = select::entities(model).all().filter(|e| !is_removed(e)).count();
        let cap = crate::io::obo::owlapi_set_cap(retained);
        let mut with_bucket: Vec<(usize, usize, &String)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                // A named class hashes from its own kind seed mixed with the IRI
                // hash, not from the bare IRI hash.
                let h = 2293i32
                    .wrapping_mul(31)
                    .wrapping_add(crate::io::obo::owlapi_iri_hash(k));
                let spread = (h ^ ((h as u32) >> 16) as i32) as usize;
                (spread & (cap - 1), i, *k)
            })
            .collect();
        with_bucket.sort_by_key(|(b, i, _)| (*b, *i));
        with_bucket.into_iter().map(|(_, _, k)| k.clone()).collect()
    };
    let mut out: Vec<Component<Rc>> = Vec::new();
    // signature -> the source expressions every re-link with that signature came from
    // source expression -> the `owner\u{1}signature` keys of the re-links made from it
    let mut by_sig: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut cross_add: HashMap<String, u64> = HashMap::new();
    // The `SubClassOf` axioms the model ALREADY holds, by `subject + super`. A
    // bridge equal to one of them is absorbed — axiom equality is structural and
    // the ontology is a set — so it costs no blank node, and neither should the
    // grouping. Six of UBERON's bridges are absorbed this way, which is why gap
    // spanning costs it nothing at all there.
    let mut already: HashSet<String> = HashSet::new();
    let mut bridge_dbg = String::new();
    let span_log = std::env::var("OM_SPAN_LOG").is_ok();
    // The run-wide set of (subject, superclass) pairs, compared structurally, which
    // both dedupes re-links and gates the recursion.
    let mut class_pairs: HashSet<String> = HashSet::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let CE::Class(c) = &sc.sub {
                // Only an axiom that SURVIVES absorbs a bridge: the bridges are
                // computed against the input but added to the filtered output, so a
                // duplicate of an axiom that is itself being dropped is still new.
                if is_removed(c.0.as_ref()) || !ce_retained(&sc.sup) {
                    continue;
                }
                already.insert(format!(
                    "{}\u{1}{}",
                    c.0.as_ref(),
                    crate::io::genid::ce_sig(&sc.sup)
                ));
            }
        }
    }
    for c in &ordered_classes {
        if is_removed(c) {
            continue;
        }
        // The hierarchy is re-asserted for EVERY retained class, not only those
        // sitting next to a gap: the walk visits each retained class's superclasses
        // and emits a PLAIN `SubClassOf`. Where the model already holds that same
        // axiom *annotated*, the plain copy is a distinct axiom, so every annotated
        // `is_a:`/`relationship:` gains an unannotated twin. That is not incidental —
        // it is why MONDO's `filtered.obo` goes from 46,491 to 91,265 `is_a:` lines
        // (the gain, 44,774, equals the number of annotated `is_a:` lines in the
        // input exactly).
        let csub = CE::Class(model.build.class(c.as_str()));
        // Walk the class's superclasses in bucket order, recursing IN PLACE at each
        // element. A superclass whose signature survives becomes a re-link — deduped
        // against the run-wide `class_pairs`, so the FIRST source reached wins and
        // any later path to the same pair is dropped outright. A removed named one is
        // stepped over carrying the SAME subject upwards, so the subject inherits
        // that ancestor's own superclass EXPRESSION. That is what decides which
        // re-links share a blank node: not the structure, but which originating axiom
        // the walk reached first.
        let mut stack: Vec<(CE<Rc>, (String, String), usize)> =
            super_classes_of(c).into_iter().rev().map(|(e, s)| (e, s, 0usize)).collect();
        // Path-local guard on the removed-ancestor step: without it a cycle among
        // removed classes would recurse forever.
        let mut path: Vec<HashSet<String>> = vec![HashSet::new()];
        while let Some((sc, src, depth)) = stack.pop() {
            path.truncate(depth + 1);
            if ce_retained(&sc) {
                if sc != csub {
                    let sig = crate::io::genid::ce_sig(&sc);
                    if !class_pairs.insert(format!("{c}\u{1}{sig}")) {
                        continue;
                    }
                    if !matches!(sc, CE::Class(_)) {
                        let bkey = format!("{}\u{1}{}", c, sig);
                        if span_log {
                            bridge_dbg
                                .push_str(&format!("BR\t{}\t{}.{}\t{}\n", c, src.0, src.1, sig));
                        }
                        if !already.contains(&bkey) {
                            by_sig.entry(src.clone()).or_insert_with(Vec::new).push(bkey);
                        }
                    }
                    out.push(Component::SubClassOf(SubClassOf { sub: csub.clone(), sup: sc }));
                }
            } else if let CE::Class(y) = &sc {
                // A removed named superclass: step over it and keep walking upwards.
                let mut seen = path[depth].clone();
                if !seen.insert(y.0.to_string()) {
                    continue;
                }
                path.push(seen);
                let d = path.len() - 1;
                stack.extend(
                    super_classes_of(y.0.as_ref()).into_iter().rev().map(|(e, s)| (e, s, d)),
                );
            }
            // An anonymous expression mentioning a removed entity is dropped
            // outright — it is not rebuilt.
        }
    }
    // The same bridging over the property hierarchies: span removed
    // object/data/annotation properties so retained sub-properties reconnect to
    // their nearest retained super-property.
    span_property_gaps(model, &is_removed, &mut out);
    // A signature is one shared object only when EVERY re-link carrying it traces
    // to the same source expression, and there is more than one of them.
    // Every re-link made from one source expression is that one object: give them a
    // group so the numbering pass can spend a single blank node on the set, without
    // touching any other occurrence of the same structure.
    // Deterministic order, and FIRST writer wins: the pair inserted first keeps the
    // expression. Iterating the map directly would leave the winner up to Rust's
    // randomised hash order whenever one (owner, structure) pair is reachable from
    // two sources.
    let mut group = 0u64;
    let mut span_dbg = String::new();
    let mut srcs: Vec<_> = by_sig.into_iter().collect();
    srcs.sort_by(|a, b| a.0.cmp(&b.0));
    for (_src, keys) in srcs {
        if keys.len() > 1 {
            group += 1;
            if std::env::var("OM_SPAN_LOG").is_ok() {
                span_dbg.push_str(&format!("group {group} source {:?} keys {}\n", _src, keys.len()));
            }
            for k in keys {
                shared_out.entry(k).or_insert(group);
            }
        }
    }
    for (k, g) in cross_add {
        cross_out.entry(k).or_insert(g);
    }
    if !span_dbg.is_empty() {
        std::fs::write("/tmp/om_span_sources.txt", &span_dbg).ok();
    }
    if !bridge_dbg.is_empty() {
        std::fs::write("/tmp/om_bridges.txt", &bridge_dbg).ok();
    }
    out
}

/// Bridge the object/data/annotation-property hierarchies across removed
/// properties (the property analogue of class gap-spanning). For each retained
/// property `c` with a direct removed named super-property, walk up through
/// chains of removed super-properties and emit `c ⊑ s` for every retained
/// super-property `s` reached.
fn span_property_gaps(
    model: &Model,
    is_removed: &dyn Fn(&str) -> bool,
    out: &mut Vec<Component<Rc>>,
) {
    use horned_owl::model::{
        SubAnnotationPropertyOf, SubDataPropertyOf, SubObjectPropertyOf,
        SubObjectPropertyExpression as SOPE,
    };

    // Build a direct super-property map (named → named) per property kind.
    let mut obj: HashMap<String, Vec<String>> = HashMap::new();
    let mut data: HashMap<String, Vec<String>> = HashMap::new();
    let mut ann: HashMap<String, Vec<String>> = HashMap::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::SubObjectPropertyOf(sp) => {
                if let (SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)), OPE::ObjectProperty(sup)) =
                    (&sp.sub, &sp.sup)
                {
                    obj.entry(sub.0.to_string()).or_default().push(sup.0.to_string());
                }
            }
            Component::SubDataPropertyOf(sp) => {
                data.entry(sp.sub.0.to_string()).or_default().push(sp.sup.0.to_string());
            }
            Component::SubAnnotationPropertyOf(sp) => {
                ann.entry(sp.sub.0.to_string()).or_default().push(sp.sup.0.to_string());
            }
            _ => {}
        }
    }

    // Generic bridge: for each retained `c`, collect retained super-properties
    // reachable through chains of removed ones.
    let bridges = |sup: &HashMap<String, Vec<String>>| -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        // Sorted: iterating the map directly takes Rust's randomised hash order, and
        // the resulting axioms would be inserted in that order, so the blank-node
        // numbering downstream would vary between runs of the same build.
        let mut props: Vec<(&String, &Vec<String>)> = sup.iter().collect();
        props.sort_by(|a, b| a.0.cmp(b.0));
        for (c, sups) in props {
            if is_removed(c) {
                continue;
            }
            if !sups.iter().any(|s| is_removed(s)) {
                continue;
            }
            let mut visited: HashSet<String> = HashSet::new();
            visited.insert(c.clone());
            let mut stack: Vec<String> = sups.clone();
            while let Some(s) = stack.pop() {
                if is_removed(&s) {
                    if visited.insert(s.clone()) {
                        if let Some(ss) = sup.get(&s) {
                            stack.extend(ss.iter().cloned());
                        }
                    }
                } else if s != *c {
                    pairs.push((c.clone(), s));
                }
            }
        }
        pairs
    };

    for (c, s) in bridges(&obj) {
        out.push(Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: SOPE::ObjectPropertyExpression(OPE::ObjectProperty(model.build.object_property(c.as_str()))),
            sup: OPE::ObjectProperty(model.build.object_property(s.as_str())),
        }));
    }
    for (c, s) in bridges(&data) {
        out.push(Component::SubDataPropertyOf(SubDataPropertyOf {
            sub: model.build.data_property(c.as_str()),
            sup: model.build.data_property(s.as_str()),
        }));
    }
    for (c, s) in bridges(&ann) {
        out.push(Component::SubAnnotationPropertyOf(SubAnnotationPropertyOf {
            sub: model.build.annotation_property(c.as_str()),
            sup: model.build.annotation_property(s.as_str()),
        }));
    }
}

/// Strip axiom annotations from kept axioms. `all`/`true`/empty drops every
/// annotation; any other value is treated as an annotation-property IRI/CURIE
/// and only matching annotations are dropped.
pub(crate) fn drop_axiom_annotations(model: &mut Model, arg: &str) {
    let arg = arg.trim();
    let drop_all = arg.is_empty() || arg.eq_ignore_ascii_case("all") || arg.eq_ignore_ascii_case("true");
    let prop = if drop_all { String::new() } else { select::expand(model, arg) };
    let kept: Vec<_> = model
        .ont
        .iter()
        .map(|ac| {
            let mut ac = ac.clone();
            if drop_all {
                ac.ann.clear();
            } else {
                ac.ann.retain(|a| a.ap.0.as_ref() != prop.as_str());
            }
            ac
        })
        .collect();
    let mut ont = horned_owl::ontology::set::SetOntology::new();
    use horned_owl::model::MutableOntology;
    for ac in kept {
        ont.insert(ac);
    }
    model.ont = ont;
}

/// Whether `comp` matches the term set under the given `trim` policy. `trim`
/// true → any signature entity is selected; false → the whole signature is
/// selected (so axioms touching a non-selected entity are spared).
/// [`term_match`] over a whole annotated component, so the axiom's own
/// annotations count toward its signature.
fn term_match_ac(
    ac: &horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>,
    terms: &HashSet<String>,
    trim: bool,
    ann_values: bool,
) -> bool {
    let mut extra: HashSet<String> = HashSet::new();
    for a in &ac.ann {
        if ann_values {
            extra.extend(sig::annotation_iris(a));
        } else {
            extra.insert(sig::annotation_property_iri(a));
        }
    }
    term_match_with(&ac.component, terms, trim, &extra, ann_values)
}

fn term_match(
    comp: &Component<horned_owl::model::RcStr>,
    terms: &HashSet<String>,
    trim: bool,
) -> bool {
    term_match_with(comp, terms, trim, &HashSet::new(), true)
}

fn term_match_with(
    comp: &Component<horned_owl::model::RcStr>,
    terms: &HashSet<String>,
    trim: bool,
    extra: &HashSet<String>,
    ann_values: bool,
) -> bool {
    let mut sig = sig::signature(comp);
    // The logical signature excludes an annotation assertion's subject, but a term
    // removal DOES take the `rdfs:label`/`rdfs:comment` of the entity it removes,
    // so the subject is folded in here.
    //
    // This was removed once, on the strength of `composite-metazoan-basic.owl`
    // keeping every deprecated class's annotations as a bare `rdf:Description`, and
    // it cost a full build to learn that those classes lose their DECLARATION to the
    // `filter --axioms` two steps earlier and their `SubClassOf … owl:Thing` to
    // `remove --axioms structural-tautologies` one step earlier. The deprecated
    // selector never had to touch them, so their survival says nothing about this
    // fold. What it does control is every `remove` in the build:
    // `odk:subset`'s own complement removal kept the annotations of every class it
    // dropped, and `common-anatomy.owl` came out at 65 MB against 1 MB.
    if let Component::AnnotationAssertion(ax) = comp {
        if let horned_owl::model::AnnotationSubject::IRI(i) = &ax.subject {
            sig.insert(i.to_string());
        }
        // …and an IRI value, so removing an entity also takes the assertions that
        // point AT it: `AnnotationAssertion(RO_0002175, UBERON_X, NCBITaxon_9606)`
        // goes with `NCBITaxon_9606`. A literal value names no entity and is not
        // folded in.
        if ann_values {
            if let horned_owl::model::AnnotationValue::IRI(i) = &ax.ann.av {
                sig.insert(i.to_string());
            }
        }
        // An AnnotationAssertion's signature includes the annotation property, so
        // `remove --term <ap>` (e.g. MONDO's
        // `remove-annotations-before-release.txt` listing
        // `mondo#excluded_from_qc_check`, …) drops every assertion that uses it.
        sig.insert(ax.ann.ap.0.to_string());
    }
    // An axiom's signature walks its ANNOTATIONS too, so a definition carrying
    // `Annotation(oboInOwl:hasDbXref "…")` has hasDbXref in its signature and
    // `remove --term hasDbXref --trim true` takes the whole axiom with it.
    // MONDO's `merged_import.owl` keeps 2,759 `IAO_0000115`
    // assertions out of 17,249 for exactly this reason: the other 14,490 carry an
    // xref annotation whose property is not in the keep set.
    sig.extend(sig::annotation_properties(comp));
    sig.extend(extra.iter().cloned());
    // The logical signature also excludes annotation properties, but
    // `remove --term <ap>` drops the property's own declaration and
    // sub-property axioms too — so an `mondo#doid` that is both a removed
    // provenance property AND a subsetdef vanishes completely (declaration +
    // `subPropertyOf SubsetProperty` + label), rather than being re-synthesised
    // from a surviving subsetdef header on the next load.
    match comp {
        Component::DeclareAnnotationProperty(d) => {
            sig.insert(d.0 .0.to_string());
        }
        Component::SubAnnotationPropertyOf(s) => {
            sig.insert(s.sub.0.to_string());
            sig.insert(s.sup.0.to_string());
        }
        _ => {}
    }
    if sig.is_empty() {
        return false;
    }
    if trim {
        sig.iter().any(|s| terms.contains(s))
    } else {
        sig.iter().all(|s| terms.contains(s))
    }
}

/// An axiom is "external" (relative to the base IRIs) when it is not *about* any
/// internal entity. For a named-subject axiom that is the subject's IRI; for a
/// General Class Inclusion with an anonymous subject (`(CL ⊓ ∃part_of.X) ⊑ Y`)
/// it is decided by the *subject expression* — external when no class in the
/// subject is internal. With a CL base that keeps `(CL_0000163 ⊓ …) ⊑ Y` and
/// strips both `(UBERON ⊓ …) ⊑ Y` and `(∃r.PATO) ⊑ CL_0000000` — an internal
/// *object* does not save a GCI whose subject is entirely external, so the test is
/// the subject's signature, not the axiom's.
fn is_external(comp: &Component<horned_owl::model::RcStr>, base: &[String]) -> bool {
    use horned_owl::model::Component as C;
    // An `owl:imports` is an ontology-level declaration, not an axiom, so no
    // `--axioms <category>` selector may reach it. Falling through would judge it
    // external — it has no signature to be internal by — and MONDO's `mirror-mfomd`
    // step (`remove --base-iri …/MFOMD --axioms external`) would strip mfomd's four
    // imports. The merge over the mirrors follows those imports, so the MF /
    // MD-core / MF-core closure would never reach `mirror/merged.owl` and
    // MFOMD_0000119 and friends would be missing from the import module.
    if matches!(comp, C::Import(_)) {
        return false;
    }
    // `subject_iri`/`subject_iris` below have no arm for these axiom types, so their
    // subject set comes out EMPTY — and an axiom with an empty subject set is
    // external, which `--axioms external` removes. Return that here rather than fall
    // through to the whole-signature test, which would keep them whenever any entity
    // they mention is internal. (Giving one of these types a subject means adding an
    // arm there and dropping it from this list.) BSPO states three SWRL rules;
    // keeping them would leave `mirror/bspo.owl` 8,431 bytes larger, carrying a whole
    // `<!-- Rules -->` section that a mirror stripped to its own base has no place
    // for.
    if matches!(
        comp,
        C::Rule(_)
            | C::HasKey(_)
            | C::DatatypeDefinition(_)
            | C::DifferentIndividuals(_)
            | C::SameIndividual(_)
            | C::NegativeObjectPropertyAssertion(_)
            | C::NegativeDataPropertyAssertion(_)
            | C::AnnotationPropertyDomain(_)
            | C::AnnotationPropertyRange(_)
            | C::DisjointUnion(_)
            | C::FunctionalDataProperty(_)
    ) {
        return true;
    }
    let internal = |iri: &str| base.iter().any(|b| iri.starts_with(b.as_str()));
    // The "primary" class of a subject expression: the named subject, or — for an
    // intersection subject — its first named-class conjunct, and none at all for a
    // subject with no named-class conjunct (`∃r.PATO`). The external test below does
    // NOT key a GCI on that genus; an anonymous subject is judged by its whole
    // signature, for the reason given there.
    fn primary_class(ce: &CE<horned_owl::model::RcStr>) -> Option<String> {
        match ce {
            CE::Class(c) => Some(c.0.to_string()),
            CE::ObjectIntersectionOf(v) => v.iter().find_map(|c| match c {
                CE::Class(cl) => Some(cl.0.to_string()),
                _ => None,
            }),
            _ => None,
        }
    }
    // An ANONYMOUS subclass contributes its whole SIGNATURE, and any internal
    // member keeps the axiom. Keying on the first named-class conjunct instead
    // would make a GCI with no named conjunct — `ObjectSomeValuesFrom(
    // RO_0000053 PATO_0010006) ⊑ CL_0000000` — external, so `mirror-pato` would
    // drop it and it would never reach the import module.
    if let C::SubClassOf(ax) = comp {
        if !matches!(&ax.sub, CE::Class(_)) {
            let sub_sig = sig::class_expression_signature(&ax.sub);
            return !sub_sig.iter().any(|iri| internal(iri));
        }
    }
    // The subjects are a SET, and the axiom is internal when ANY of them is. For
    // the n-ary axioms — disjoint/equivalent classes and properties — every member
    // is a subject, so an axiom that mentions one internal term is kept however the
    // members happen to be ordered. Taking only the first would make `mirror-uberon`
    // (`remove --base-iri …/UBERON --axioms external`) drop
    // `DisjointClasses(GO_0110165 UBERON_0000001)` — the one axiom whose first
    // member is the foreign one.
    let subjects = subject_iris(comp);
    if !subjects.is_empty() {
        return !subjects.iter().any(|iri| internal(iri));
    }
    // An n-ary CLASS axiom whose members are all anonymous has no subject at all:
    // only the NAMED members are subjects, and "external" is "no subject in the base
    // namespaces", which an empty set satisfies. Falling through to the
    // whole-signature test instead would keep PATO's `EquivalentClasses(
    // ∧(CARO_0000000, ∃RO_0000053.PATO_0001993) …)` — its signature holds a PATO
    // term — and with it a bare `<owl:ObjectProperty rdf:about="…/RO_0002180"/>`
    // that `mirror/pato.owl` has no business declaring.
    // (EquivalentClasses only: an all-anonymous `DisjointClasses` is KEPT — its
    // subjects do fall back to the signature.)
    if matches!(comp, C::EquivalentClasses(_)) {
        return true;
    }
    !sig::signature(comp).iter().any(|iri| internal(iri))
}

/// Every subject IRI of `comp`: one for a binary axiom, all members for an n-ary
/// one. Empty when the subject is anonymous (the caller then falls back to the
/// whole signature).
fn subject_iris(comp: &Component<horned_owl::model::RcStr>) -> Vec<String> {
    use horned_owl::model::Component as C;
    let class = |c: &CE<horned_owl::model::RcStr>| match c {
        CE::Class(cl) => Some(cl.0.to_string()),
        _ => None,
    };
    match comp {
        C::EquivalentClasses(ax) => ax.0.iter().filter_map(class).collect(),
        C::DisjointClasses(ax) => ax.0.iter().filter_map(class).collect(),
        C::DisjointObjectProperties(ax) => ax.0.iter().filter_map(ope_iri).collect(),
        C::EquivalentObjectProperties(ax) => ax.0.iter().filter_map(ope_iri).collect(),
        C::DisjointDataProperties(ax) => ax.0.iter().map(|p| p.0.to_string()).collect(),
        C::EquivalentDataProperties(ax) => ax.0.iter().map(|p| p.0.to_string()).collect(),
        // A property CHAIN's subjects are its chain members; the super-property is
        // not among them. So `BFO_0000051 ∘ RO_0000052 ⊑ UPHENO_…` is external to
        // `--base-iri …/UPHENO_` even though its super-property is internal, and a
        // base module states no chain built out of foreign properties. Without this
        // arm the subject set would be empty and the caller would fall back to the
        // whole signature, which does include the super-property.
        C::SubObjectPropertyOf(ax)
            if matches!(
                &ax.sub,
                horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(_)
            ) =>
        {
            match &ax.sub {
                horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(chain) => {
                    chain.iter().filter_map(ope_iri).collect()
                }
                _ => Vec::new(),
            }
        }
        _ => subject_iri(comp).into_iter().collect(),
    }
}

/// The "about" entity IRI of an axiom, for base-module selection.
fn subject_iri(comp: &Component<horned_owl::model::RcStr>) -> Option<String> {
    use horned_owl::model::Component as C;
    let class = |c: &CE<horned_owl::model::RcStr>| match c {
        CE::Class(cl) => Some(cl.0.to_string()),
        _ => None,
    };
    match comp {
        C::DeclareClass(d) => Some(d.0 .0.to_string()),
        C::DeclareObjectProperty(d) => Some(d.0 .0.to_string()),
        C::DeclareAnnotationProperty(d) => Some(d.0 .0.to_string()),
        C::DeclareDataProperty(d) => Some(d.0 .0.to_string()),
        C::DeclareNamedIndividual(d) => Some(d.0 .0.to_string()),
        C::DeclareDatatype(d) => Some(d.0 .0.to_string()),
        C::SubClassOf(ax) => class(&ax.sub),
        C::EquivalentClasses(ax) => ax.0.iter().find_map(class),
        C::DisjointClasses(ax) => ax.0.iter().find_map(class),
        C::AnnotationAssertion(ax) => match &ax.subject {
            horned_owl::model::AnnotationSubject::IRI(i) => Some(i.to_string()),
            _ => None,
        },
        C::SubObjectPropertyOf(ax) => match &ax.sub {
            horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(
                OPE::ObjectProperty(p),
            ) => Some(p.0.to_string()),
            _ => None,
        },
        // A sub-annotation-property axiom is about its sub-property. CL's subset
        // declarations (`cl#eye_upper_slim ⊑ oboInOwl:SubsetProperty`) are
        // internal and kept: without this arm the fallthrough signature test
        // finds no signature for the axiom and judges it external.
        C::SubAnnotationPropertyOf(ax) => Some(ax.sub.0.to_string()),
        C::TransitiveObjectProperty(ax) => ope_iri(&ax.0),
        // `InverseObjectProperties(P, Q)` renders as `P owl:inverseOf Q`, so P is
        // its subject. Falling through to the whole-signature test would keep the
        // axiom whenever EITHER property was internal, and EFO's base would carry
        // `IAO_0000136 owl:inverseOf EFO_0006351` — an external subject, where the
        // base is to keep only a bare `owl:ObjectProperty` declaration.
        C::InverseObjectProperties(ax) => ope_iri(&ax.0),
        C::ObjectPropertyDomain(ax) => ope_iri(&ax.ope),
        C::ObjectPropertyRange(ax) => ope_iri(&ax.ope),
        // A ClassAssertion is about its individual (`i type C`): CL's imported
        // CCN cell-set individuals carry an external `ClassAssertion`, which the
        // base strips along with the individual's declaration (the RDF writer only
        // re-declares it because the assertion keeps it in the signature). A named
        // individual is the subject; an anonymous one has no determinable subject.
        C::ClassAssertion(ax) => match &ax.i {
            horned_owl::model::Individual::Named(n) => Some(n.0.to_string()),
            horned_owl::model::Individual::Anonymous(_) => None,
        },
        _ => None,
    }
}

fn ope_iri(ope: &OPE<horned_owl::model::RcStr>) -> Option<String> {
    match ope {
        OPE::ObjectProperty(p) => Some(p.0.to_string()),
        OPE::InverseObjectProperty(p) => Some(p.0.to_string()),
    }
}

/// Object properties that appear anywhere in the ontology.
fn object_properties(model: &Model) -> HashSet<String> {
    // The SIGNATURE, as the writer computes it — not a bespoke walk. A piped model
    // can hold an object property with no declaration (a preceding `filter --axioms`
    // drops declarations) and no reachable use, and the two walkers then disagree:
    // the writer still gives it a frame while the removal never sees it, so a
    // `--select complement --select object-properties` step leaves the property
    // behind. Re-reading the same model from disk hid the bug, because the frame the
    // writer emitted comes back as a declaration.
    let mut set = crate::cmd::select::signature_entities(model).object_properties;
    for ac in model.ont.iter() {
        collect_object_properties(&ac.component, &mut set);
    }
    set
}

fn collect_object_properties(comp: &Component<horned_owl::model::RcStr>, out: &mut HashSet<String>) {
    use horned_owl::model::Component as C;
    match comp {
        C::DeclareObjectProperty(d) => {
            out.insert(d.0 .0.to_string());
        }
        C::SubClassOf(ax) => {
            collect_ce_props(&ax.sub, out);
            collect_ce_props(&ax.sup, out);
        }
        C::EquivalentClasses(ax) => ax.0.iter().for_each(|c| collect_ce_props(c, out)),
        C::SubObjectPropertyOf(ax) => {
            if let OPE::ObjectProperty(p) = &ax.sup {
                out.insert(p.0.to_string());
            }
            if let horned_owl::model::SubObjectPropertyExpression::ObjectPropertyExpression(
                OPE::ObjectProperty(p),
            ) = &ax.sub
            {
                out.insert(p.0.to_string());
            }
        }
        C::TransitiveObjectProperty(ax) => {
            if let Some(i) = ope_iri(&ax.0) {
                out.insert(i);
            }
        }
        _ => {}
    }
}

fn collect_ce_props(ce: &CE<horned_owl::model::RcStr>, out: &mut HashSet<String>) {
    match ce {
        CE::ObjectSomeValuesFrom { ope, bce } | CE::ObjectAllValuesFrom { ope, bce } => {
            if let OPE::ObjectProperty(p) = ope {
                out.insert(p.0.to_string());
            }
            collect_ce_props(bce, out);
        }
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => {
            v.iter().for_each(|c| collect_ce_props(c, out))
        }
        _ => {}
    }
}


#[cfg(test)]
mod external_axiom_tests {
    use super::*;
    use horned_owl::model::{
        Build, ObjectProperty, RcStr, SubObjectPropertyExpression as SOPE, SubObjectPropertyOf,
    };

    const UPHENO: &str = "http://purl.obolibrary.org/obo/UPHENO_";

    fn chain(members: &[&str], sup: &str) -> Component<RcStr> {
        let b: Build<RcStr> = Build::new();
        let op = |i: &str| OPE::ObjectProperty(ObjectProperty(b.iri(i)));
        Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: SOPE::ObjectPropertyChain(members.iter().map(|m| op(m)).collect()),
            sup: match op(sup) {
                OPE::ObjectProperty(p) => OPE::ObjectProperty(p),
                other => other,
            },
        })
    }

    /// A chain axiom's subjects are its chain members alone; the super-property is
    /// not among them. So a chain built out of foreign properties is external to
    /// `--base-iri …/UPHENO_` however internal its super-property is, and a base
    /// module states none of them.
    #[test]
    fn a_chain_of_foreign_properties_is_external_whatever_its_super_property() {
        let base = vec![UPHENO.to_string()];
        let c = chain(
            &[
                "http://purl.obolibrary.org/obo/BFO_0000051",
                "http://purl.obolibrary.org/obo/RO_0000052",
            ],
            "http://purl.obolibrary.org/obo/UPHENO_0000001",
        );
        assert!(is_external(&c, &base), "chain members are all foreign, so the axiom is external");
    }

    /// The converse: one internal chain member keeps it, because an axiom is
    /// internal when ANY of its subjects lies in the base namespace.
    #[test]
    fn a_chain_with_one_internal_member_is_kept() {
        let base = vec![UPHENO.to_string()];
        let c = chain(
            &[
                "http://purl.obolibrary.org/obo/UPHENO_0000001",
                "http://purl.obolibrary.org/obo/BFO_0000050",
            ],
            "http://purl.obolibrary.org/obo/UPHENO_0000001",
        );
        assert!(!is_external(&c, &base), "an internal chain member keeps the axiom");
    }
}
