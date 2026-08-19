//! Module extraction: syntactic locality modules (⊥, ⊤, ⊥⊤*) and MIREOT.
//!
//! Locality-based module extraction (Cuenca Grau, Horrocks, Kazakov, Sattler)
//! computes a subset M of an ontology O that preserves all entailments over a
//! seed signature Σ. An axiom is added to the module iff it is *not local*
//! w.r.t. the current signature; the signature then grows with that axiom's
//! own entities, and the process iterates to a fixpoint.
//!
//! `om extract --method {BOT,TOP,STAR,MIREOT}` chooses between them.

use std::collections::HashSet;

use horned_owl::model::{
    Annotation, AnnotationAssertion, AnnotationSubject, AnnotationValue, ClassExpression as CE,
    Component, MutableOntology, ObjectPropertyExpression as OPE, OntologyID, RcStr,
};
use horned_owl::ontology::set::SetOntology;

use crate::model::Model;
use crate::sig;

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// A signature: TYPED entities, keyed by IRI with a bitmask of the kinds that
/// IRI carries. See [`crate::sig::kind`] for why the typing is load-bearing.
///
/// A map of masks rather than a set of `(kind, IRI)` pairs so that a locality
/// test — millions of them on a CHEBI-sized mirror — is one hash lookup on a
/// borrowed `&str` and allocates nothing.
#[derive(Default, Clone)]
struct Sigma(std::collections::HashMap<String, u8>);

impl Sigma {
    fn has(&self, kind: u8, iri: &str) -> bool {
        self.0.get(iri).is_some_and(|k| k & kind != 0)
    }
    fn add(&mut self, kind: u8, iri: &str) {
        match self.0.get_mut(iri) {
            Some(k) => *k |= kind,
            None => {
                self.0.insert(iri.to_string(), kind);
            }
        }
    }
    fn add_component(&mut self, comp: &Component<RcStr>) {
        for (k, iri) in sig::typed_signature(comp) {
            self.add(k, &iri);
        }
    }
    /// Every IRI in the signature, whatever its kind. Annotation assertions are
    /// matched by subject IRI with no typing, so this untyped view is what
    /// decides which of them the module keeps.
    fn iris(&self) -> HashSet<String> {
        self.0.keys().cloned().collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    /// ⊥-locality (bottom): the smallest module preserving subsumption of the
    /// seed terms by everything; the standard import module.
    Bot,
    /// ⊤-locality (top).
    Top,
    /// ⊥⊤*-locality (star): the nested module, usually the smallest.
    Star,
}

impl Method {
    pub fn parse(s: &str) -> Option<Method> {
        match s.to_ascii_uppercase().as_str() {
            "BOT" | "BOTTOM" => Some(Method::Bot),
            "TOP" => Some(Method::Top),
            "STAR" | "BOT-TOP" => Some(Method::Star),
            _ => None,
        }
    }
}

/// How to treat OWL individuals when assembling a module (`-n,--individuals`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Individuals {
    /// Keep individuals and their assertions (the default).
    Include,
    /// Drop all individual declarations and assertions.
    Exclude,
    /// Keep only individuals reachable from the module signature (best-effort;
    /// treated like `definitions` here).
    Minimal,
    /// Keep individuals that have type/definition assertions in the module.
    Definitions,
}

impl Individuals {
    pub fn parse(s: &str) -> Option<Individuals> {
        match s.to_ascii_lowercase().as_str() {
            "include" => Some(Individuals::Include),
            "exclude" => Some(Individuals::Exclude),
            "minimal" => Some(Individuals::Minimal),
            "definitions" => Some(Individuals::Definitions),
            _ => None,
        }
    }
}

/// How to handle intermediate (non-seed) terms in the module
/// (`-N,--intermediates`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intermediates {
    /// Keep every term the locality module pulls in (the default).
    All,
    /// Collapse the hierarchy so only seed terms remain as named nodes,
    /// re-linking each seed to its nearest seed ancestor (best-effort).
    Minimal,
    /// Drop every non-seed named class, keeping only the seed terms.
    None,
}

impl Intermediates {
    pub fn parse(s: &str) -> Option<Intermediates> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Some(Intermediates::All),
            "minimal" => Some(Intermediates::Minimal),
            "none" => Some(Intermediates::None),
            _ => None,
        }
    }
}

/// Options controlling how a module is assembled, one per post-extraction flag
/// of `om extract`. The defaults are those flags' defaults.
#[derive(Clone)]
pub struct ExtractOptions {
    /// Copy the source ontology's ontology-level annotations into the module
    /// (`-c,--copy-ontology-annotations`).
    pub copy_ontology_annotations: bool,
    /// Annotate every extracted term with `rdfs:isDefinedBy` (and
    /// `oboInOwl:source`) = its source ontology IRI (`-a,--annotate-with-source`).
    pub annotate_with_source: bool,
    /// How to treat individuals.
    pub individuals: Individuals,
    /// How to treat intermediate terms.
    pub intermediates: Intermediates,
    /// Override the module's ontology IRI (`-O,--output-iri`).
    pub output_iri: Option<String>,
    /// Per-term source ontology IRI, from a `-s,--sources` mapping file. When a
    /// term is absent the source falls back to the input ontology IRI.
    pub sources: std::collections::HashMap<String, String>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            copy_ontology_annotations: false,
            annotate_with_source: false,
            individuals: Individuals::Include,
            intermediates: Intermediates::All,
            output_iri: None,
            sources: std::collections::HashMap::new(),
        }
    }
}

/// Extract a locality-based module for `seed` from `model` with default options.
pub fn extract(model: &Model, seed: &HashSet<String>, method: Method) -> Model {
    extract_with(model, seed, method, &ExtractOptions::default())
}

/// Extract a locality-based module, honoring the post-extraction `opts`.
pub fn extract_with(
    model: &Model,
    seed: &HashSet<String>,
    method: Method,
    opts: &ExtractOptions,
) -> Model {
    let comps: Vec<Component<RcStr>> = model.ont.iter().map(|ac| ac.component.clone()).collect();

    // `--individuals exclude` is not a post-filter: every ABox axiom leaves the
    // candidate set BEFORE locality is evaluated. That changes the module, not just
    // its output: a `ClassAssertion(C, a)` with C outside the seed is NOT ⊥-local,
    // so leaving it in pulls C into the signature, and then C's own superclasses
    // follow. Filtering afterwards would give MONDO's `imports/merged_import.owl` a
    // whole IAO fragment — `SubClassOf(IAO_0000027 IAO_0000030)` with neither term
    // in the 27,535-term seed — off the curation-status individuals' type
    // assertions.
    let candidate: Vec<usize> = if opts.individuals == Individuals::Exclude {
        (0..comps.len()).filter(|&i| !is_abox(&comps[i])).collect()
    } else {
        all_indices(&comps)
    };

    // The seed as a TYPED signature: a seed IRI enters as every entity the SOURCE
    // has under it — and an IRI the source never mentions enters as nothing at
    // all.
    let mut source_kinds: Sigma = Sigma::default();
    for c in &comps {
        source_kinds.add_component(c);
    }
    let mut seed_sig = Sigma::default();
    for iri in seed {
        if let Some(&k) = source_kinds.0.get(iri) {
            seed_sig.add(k, iri);
        }
    }

    let module_idx: Vec<usize> = match method {
        Method::Bot => locality_module(&comps, &seed_sig, Locality::Bot, &candidate),
        Method::Top => locality_module(&comps, &seed_sig, Locality::Top, &candidate),
        Method::Star => star_module(&comps, &seed_sig, &candidate),
    };
    let module: HashSet<usize> = module_idx.into_iter().collect();

    let out = build_output(model, &comps, &module, &seed_sig);
    post_process(out, model, seed, opts)
}

fn all_indices(comps: &[Component<RcStr>]) -> Vec<usize> {
    (0..comps.len()).collect()
}

/// The named individuals in a module's LOGICAL signature, ignoring individual
/// declarations (which are what the caller is deciding) and annotation
/// subjects/values (an annotation subject is an `IRI`, not an entity).
fn individual_signature_of(module: &Model) -> HashSet<String> {
    use horned_owl::model::NamedIndividual;
    use horned_owl::visitor::immutable::{Visit, Walk};

    #[derive(Default)]
    struct Inds(HashSet<String>);
    impl Visit<RcStr> for Inds {
        fn visit_named_individual(&mut self, i: &NamedIndividual<RcStr>) {
            self.0.insert(i.0.as_ref().to_string());
        }
    }
    let mut walk = Walk::new(Inds::default());
    for ac in module.ont.iter() {
        if matches!(ac.component, Component::DeclareNamedIndividual(_)) {
            continue;
        }
        walk.component(&ac.component);
    }
    walk.into_visit().0
}

/// The assertional (ABox) axioms — the ones `--individuals exclude` removes from
/// the candidate set before locality runs.
fn is_abox(comp: &Component<RcStr>) -> bool {
    use horned_owl::model::Component as C;
    matches!(
        comp,
        C::ClassAssertion(_)
            | C::SameIndividual(_)
            | C::DifferentIndividuals(_)
            | C::ObjectPropertyAssertion(_)
            | C::NegativeObjectPropertyAssertion(_)
            | C::DataPropertyAssertion(_)
            | C::NegativeDataPropertyAssertion(_)
    )
}

/// The ⊥⊤*-module: alternate ⊥ and ⊤ extraction over the shrinking candidate
/// set until it stabilizes.
fn star_module(comps: &[Component<RcStr>], seed: &Sigma, start: &[usize]) -> Vec<usize> {
    let mut candidate: Vec<usize> = start.to_vec();
    loop {
        let after_bot = locality_module(comps, seed, Locality::Bot, &candidate);
        let after_top = locality_module(comps, seed, Locality::Top, &after_bot);
        if after_top.len() == candidate.len() {
            return after_top;
        }
        candidate = after_top;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Locality {
    Bot,
    Top,
}

/// Compute the locality module: from `candidate` indices, keep iterating,
/// adding any non-local axiom (growing Σ) until fixpoint.
fn locality_module(
    comps: &[Component<RcStr>],
    seed: &Sigma,
    loc: Locality,
    candidate: &[usize],
) -> Vec<usize> {
    let mut sigma: Sigma = seed.clone();
    let mut in_module = vec![false; comps.len()];
    loop {
        let mut changed = false;
        for &i in candidate {
            if in_module[i] {
                continue;
            }
            if !is_local(&comps[i], &sigma, loc) {
                in_module[i] = true;
                sigma.add_component(&comps[i]);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    candidate.iter().copied().filter(|&i| in_module[i]).collect()
}

/// Assemble the output ontology from the module axioms, plus declarations and
/// annotation assertions for every entity in the module signature and seed.
fn build_output(
    model: &Model,
    comps: &[Component<RcStr>],
    module: &HashSet<usize>,
    seed: &Sigma,
) -> Model {
    // Module signature: seed + the signature of every locality axiom. The signature
    // of an *annotated* axiom includes the annotation properties of its axiom
    // annotations, so add those too (e.g. `editor note` used as an axiom annotation
    // on a kept domain/range axiom enters the signature, and the annotations
    // describing it are then kept). Drives the declarations of logical entities and
    // the annotations copied onto signature entities.
    let acs: Vec<&horned_owl::model::AnnotatedComponent<RcStr>> = model.ont.iter().collect();
    let mut sigma: Sigma = seed.clone();
    for &i in module {
        sigma.add_component(&comps[i]);
        for a in acs[i].ann.iter() {
            sigma.add(sig::kind::ANNOTATION_PROPERTY, a.ap.0.as_ref());
            for d in literal_datatypes(&a.av) {
                sigma.add(sig::kind::DATATYPE, &d);
            }
        }
        for d in component_literal_datatypes(&comps[i]) {
            sigma.add(sig::kind::DATATYPE, &d);
        }
    }
    // Annotation assertions are matched by SUBJECT IRI, untyped.
    let module_sig: HashSet<String> = sigma.iris();

    let build: horned_owl::model::Build<RcStr> = horned_owl::model::Build::new();
    let mut ont = SetOntology::new();
    // Every annotation property in the retained module's signature is declared (even
    // ones the source left undeclared), but NOT the OWL/RDFS built-in annotation
    // properties. Collect the properties referenced by the *kept* axioms: each kept
    // axiom's inline (axiom) annotations plus the asserted property of each kept
    // annotation assertion.
    let mut declare_props: HashSet<String> = HashSet::new();
    let mut module_sig_props: HashSet<String> = HashSet::new();
    for (i, ac) in model.ont.iter().enumerate() {
        // Keep the ontology IRI, but drop the versionIRI: a module is a subset of one
        // release of its source, not a version of it, so it is emitted un-versioned.
        if let Component::OntologyID(id) = &ac.component {
            ont.insert(Component::OntologyID(OntologyID {
                iri: id.iri.clone(),
                viri: None,
            }));
            continue;
        }
        let keep = module.contains(&i)
            || is_declaration_of(&ac.component, &sigma)
            // Annotations whose subject is a retained module-signature entity are kept
            // (incl. annotations describing an IAO/RO entity that the logical module
            // references), but not annotations on annotation properties that appear
            // only as properties — those are never in the logical signature.
            || is_annotation_on(&ac.component, &module_sig)
            // The last pass over Σ: for every named individual in it, every
            // same- and different-individual axiom naming it comes back.
            || is_individual_identity_on(&ac.component, &sigma)
            || matches!(ac.component, Component::DocIRI(_));
        if keep {
            for a in ac.ann.iter() {
                declare_props.insert(a.ap.0.as_ref().to_string());
                // The MODULE signature, which is what decides whether a built-in
                // annotation property keeps its declaration. The signature grows only
                // from the signature of each non-local LOGICAL axiom — which includes
                // the properties of that axiom's own annotations — and the source's
                // declarations are added afterwards for the entities in it.
                // Annotation ASSERTIONS come back in that same later pass and never
                // extend the signature, so a property used only in assertions does
                // not qualify.
                if module.contains(&i) {
                    module_sig_props.insert(a.ap.0.as_ref().to_string());
                }
            }
            if let Component::AnnotationAssertion(aa) = &ac.component {
                declare_props.insert(aa.ann.ap.0.as_ref().to_string());
            }
            ont.insert(ac.clone());
        }
    }
    // A built-in annotation property is declared in the module exactly when the
    // SOURCE declares it: no declaration is synthesised for the OWL/RDFS vocabulary,
    // but one the source ontology already made is carried through. OBA declares
    // `rdfs:label` and `rdfs:comment` (and not `rdfs:seeAlso`), and its BOT module
    // keeps precisely those two; exempting the built-ins outright would drop both.
    let source_declares: HashSet<&str> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::DeclareAnnotationProperty(d) => Some(d.0 .0.as_ref()),
            _ => None,
        })
        .collect();
    // A built-in annotation property is declared only when the source declares it
    // AND it is in the module signature (see above): OBA declares `rdfs:label`,
    // `rdfs:comment` and `owl:deprecated`, and its BOT module keeps the first two —
    // `owl:deprecated` appears only in annotation assertions. UBERON declares
    // `rdfs:label` too, and its module drops it for the same reason.
    for ap in declare_props.iter().filter(|p| {
        !is_builtin_annotation_property(p)
            || (source_declares.contains(p.as_str()) && module_sig_props.contains(p.as_str()))
    }) {
        ont.insert(Component::DeclareAnnotationProperty(
            horned_owl::model::DeclareAnnotationProperty(build.annotation_property(ap.as_str())),
        ));
    }
    let mut out = Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes));
    carry_subset_meta(&mut out, model);
    out
}

/// Carry the source state a MODULE inherits. A module is a subset of the source,
/// so the source's blank-node identity evidence still describes the axioms it
/// keeps; without it the numbering pass has nothing but structural equality to go
/// on.
///
/// `owl_anon_blocks` is deliberately NOT carried: it is verbatim source text
/// replayed unconditionally by the writer, and a subset may well have dropped the
/// individuals it describes. Carrying it would give a module like EFO's
/// `obi_import.owl` an `Individuals` section for anonymous individuals the
/// extraction removed.
fn carry_subset_meta(out: &mut Model, src: &Model) {
    let blocks = std::mem::take(&mut out.owl_anon_blocks);
    out.carry_meta_from(src);
    out.owl_anon_blocks = blocks;
}

/// Apply the post-extraction options to an assembled module: individual
/// handling, intermediate collapsing, ontology-annotation copying, per-term
/// source provenance, and the output IRI override.
fn post_process(
    mut module: Model,
    source: &Model,
    seed: &HashSet<String>,
    opts: &ExtractOptions,
) -> Model {
    // A fresh IRI builder; interned IRIs compare by value across `Build`s.
    let build: horned_owl::model::Build<RcStr> = horned_owl::model::Build::new();

    // --individuals: drop or filter individual declarations/assertions.
    if opts.individuals != Individuals::Include {
        let keep_defined = opts.individuals == Individuals::Definitions
            || opts.individuals == Individuals::Minimal;
        // Individuals that have a ClassAssertion (a "definition") in the module.
        let defined: HashSet<String> = if keep_defined {
            module
                .ont
                .iter()
                .filter_map(|ac| match &ac.component {
                    Component::ClassAssertion(ca) => {
                        Some(individual_iri(&ca.i))
                    }
                    _ => None,
                })
                .flatten()
                .collect()
        } else {
            HashSet::new()
        };
        module = crate::cmd::select::retain(module, |comp| {
            if !mentions_individual(comp) {
                return true;
            }
            if !keep_defined {
                // A DECLARATION is not an assertion: `--individuals exclude` strips
                // ABox AXIOMS, and everything left in the module's signature is then
                // declared. So an individual still named by a retained TBox axiom
                // keeps its declaration — ECTO's `IAO_0000078 ≡ ObjectOneOf(
                // IAO_0000002 …)` survives `--individuals exclude`, so its merged
                // import declares all nine members — while an individual whose only
                // mention was the dropped `ClassAssertion` gets nothing. Decided
                // below, once the assertions are gone and the surviving signature is
                // known.
                return matches!(comp, Component::DeclareNamedIndividual(_));
            }
            // definitions/minimal: keep only assertions whose individual is defined.
            individual_signature(comp).iter().any(|i| defined.contains(i))
        });
        if !keep_defined {
            // The LOGICAL signature, not every IRI the module mentions: an
            // individual declaration survives exactly when some retained axiom still
            // names that individual AS an individual. An individual named only as an
            // ANNOTATION ASSERTION'S SUBJECT is not in that signature (an annotation
            // subject is an `IRI`, not an entity), so ENVO_01001862 — which reaches
            // ECTO's merged import through three annotation assertions and nothing
            // else — keeps no declaration. Keying on every IRI the module mentions
            // would give it one.
            let sig: HashSet<String> = individual_signature_of(&module);
            module = crate::cmd::select::retain(module, |comp| match comp {
                Component::DeclareNamedIndividual(d) => sig.contains(d.0 .0.as_ref()),
                _ => true,
            });
        }
    }

    // --intermediates: collapse non-seed named classes.
    if opts.intermediates != Intermediates::All {
        module = collapse_intermediates(module, seed, opts.intermediates);
    }

    // -c,--copy-ontology-annotations: copy the source's ontology annotations.
    if opts.copy_ontology_annotations {
        for ac in source.ont.iter() {
            if matches!(ac.component, Component::OntologyAnnotation(_)) {
                module.ont.insert(ac.clone());
            }
        }
    }

    // -a,--annotate-with-source: annotate each extracted term with
    // rdfs:isDefinedBy (and oboInOwl:source) = its source ontology IRI. The source
    // is looked up per-term (from -s,--sources), falling back to the input ontology
    // IRI, so a module built from a merged mirror still records where each term came
    // from.
    if opts.annotate_with_source {
        let default_source = ontology_iri(source);
        let is_defined_by = "http://www.w3.org/2000/01/rdf-schema#isDefinedBy";
        let oio_source = "http://www.geneontology.org/formats/oboInOwl#source";
        // Terms that ended up in the module (named entities by declaration).
        let mut terms: Vec<String> = Vec::new();
        for ac in module.ont.iter() {
            if is_declaration(&ac.component) {
                terms.extend(crate::sig::signature(&ac.component));
            }
        }
        terms.sort();
        terms.dedup();
        let mut adds = Vec::new();
        for t in terms {
            let src = opts
                .sources
                .get(&t)
                .cloned()
                .or_else(|| default_source.clone());
            let Some(src) = src else { continue };
            for prop in [is_defined_by, oio_source] {
                adds.push(Component::AnnotationAssertion(AnnotationAssertion {
                    subject: AnnotationSubject::IRI(build.iri(t.as_str())),
                    ann: Annotation { ann: Default::default(),
                        ap: build.annotation_property(prop),
                        av: AnnotationValue::IRI(build.iri(src.as_str())),
                    },
                }));
            }
        }
        for c in adds {
            module.ont.insert(c);
        }
    }

    // -O,--output-iri: set the module's ontology IRI.
    if let Some(iri) = &opts.output_iri {
        let kept: Vec<_> = module
            .ont
            .iter()
            .filter(|ac| !matches!(ac.component, Component::OntologyID(_)))
            .cloned()
            .collect();
        let mut ont = SetOntology::new();
        for ac in kept {
            ont.insert(ac);
        }
        ont.insert(Component::OntologyID(OntologyID {
            iri: Some(build.iri(iri.as_str())),
            viri: None,
        }));
        let carried = std::mem::replace(&mut module, Model::new());
        module = Model::from_parts(ont, crate::model::clone_prefixes(&carried.prefixes));
        // Setting `-O` rebuilds the ontology set; it must not also discard the
        // carried state (blank-node sharing, reification order, prefixes) that the
        // module reached here with.
        carry_subset_meta(&mut module, &carried);
    }

    module
}

/// The ontology IRI of `model`, if it declares one.
fn ontology_iri(model: &Model) -> Option<String> {
    for ac in model.ont.iter() {
        if let Component::OntologyID(id) = &ac.component {
            if let Some(iri) = &id.iri {
                return Some(iri.as_ref().to_string());
            }
        }
    }
    None
}

/// Collapse intermediate (non-seed) named classes. For `none`, drop SubClassOf
/// edges to/from non-seed classes and their declarations, keeping only seed
/// terms. For `minimal`, re-link each kept class to its nearest seed ancestor so
/// the seed hierarchy is preserved without the intermediates (best-effort: the
/// rewiring follows the asserted hierarchy only).
fn collapse_intermediates(model: Model, seed: &HashSet<String>, mode: Intermediates) -> Model {
    // Build the asserted named-superclass map.
    let mut parents: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(sub), CE::Class(sup)) = (&sc.sub, &sc.sup) {
                parents
                    .entry(sub.0.as_ref().to_string())
                    .or_default()
                    .push(sup.0.as_ref().to_string());
            }
        }
    }

    // For `minimal`, compute, for each seed term, its nearest seed ancestors.
    let mut new_edges: HashSet<(String, String)> = HashSet::new();
    if mode == Intermediates::Minimal {
        for s in seed {
            let mut stack: Vec<String> = parents.get(s).cloned().unwrap_or_default();
            let mut visited: HashSet<String> = HashSet::new();
            while let Some(p) = stack.pop() {
                if !visited.insert(p.clone()) {
                    continue;
                }
                if seed.contains(&p) {
                    new_edges.insert((s.clone(), p.clone()));
                } else if let Some(gps) = parents.get(&p) {
                    stack.extend(gps.iter().cloned());
                }
            }
        }
    }

    let build: horned_owl::model::Build<RcStr> = horned_owl::model::Build::new();
    let keep_class = |iri: &str| seed.contains(iri);
    let mut out = crate::cmd::select::retain(model, |comp| match comp {
        Component::DeclareClass(dc) => keep_class(dc.0 .0.as_ref()),
        Component::SubClassOf(sc) => match (&sc.sub, &sc.sup) {
            (CE::Class(a), CE::Class(b)) => {
                keep_class(a.0.as_ref()) && keep_class(b.0.as_ref())
            }
            // Keep class-to-expression axioms only if the subject is a seed term.
            (CE::Class(a), _) => keep_class(a.0.as_ref()),
            _ => true,
        },
        // Keep annotation assertions; declarations for dropped classes are gone
        // but their labels are harmless, and a module keeps its term metadata.
        _ => true,
    });

    // Re-link seed terms to their nearest seed ancestors (minimal only).
    for (sub, sup) in new_edges {
        out.ont.insert(Component::SubClassOf(horned_owl::model::SubClassOf {
            sub: CE::Class(build.class(sub.as_str())),
            sup: CE::Class(build.class(sup.as_str())),
        }));
    }
    out
}

/// The datatype IRI of a typed literal annotation value. A literal's datatype is
/// part of its axiom's signature, so a datatype the source DECLARES and then uses
/// only as the type of an annotation literal — `xsd:boolean` on
/// `owl:deprecated "true"` — belongs in the module signature and keeps its
/// declaration. Without it the module drops the declaration and its Datatypes
/// section loses that entry — and in EFO's import modules `xsd:boolean` is the
/// only declared datatype, so the section and its banner go with it.
fn literal_datatypes(av: &horned_owl::model::AnnotationValue<RcStr>) -> Vec<String> {
    match av {
        horned_owl::model::AnnotationValue::Literal(
            horned_owl::model::Literal::Datatype { datatype_iri, .. },
        ) => vec![datatype_iri.as_ref().to_string()],
        _ => Vec::new(),
    }
}

/// The same, over a component's own annotation assertions.
fn component_literal_datatypes(comp: &Component<RcStr>) -> Vec<String> {
    match comp {
        Component::AnnotationAssertion(aa) => literal_datatypes(&aa.ann.av),
        _ => Vec::new(),
    }
}

/// Whether `comp` is an entity declaration.
fn is_declaration(comp: &Component<RcStr>) -> bool {
    matches!(
        comp,
        Component::DeclareClass(_)
            | Component::DeclareObjectProperty(_)
            | Component::DeclareDataProperty(_)
            | Component::DeclareAnnotationProperty(_)
            | Component::DeclareNamedIndividual(_)
            | Component::DeclareDatatype(_)
    )
}

/// Whether a component is about an OWL individual (declaration or assertion).
fn mentions_individual(comp: &Component<RcStr>) -> bool {
    matches!(
        comp,
        Component::DeclareNamedIndividual(_)
            | Component::ClassAssertion(_)
            | Component::ObjectPropertyAssertion(_)
            | Component::NegativeObjectPropertyAssertion(_)
            | Component::DataPropertyAssertion(_)
            | Component::NegativeDataPropertyAssertion(_)
            | Component::SameIndividual(_)
            | Component::DifferentIndividuals(_)
    )
}

/// IRIs of named individuals mentioned by an individual-related component.
fn individual_signature(comp: &Component<RcStr>) -> HashSet<String> {
    // Reuse the general signature; for individual components this is the set of
    // entities involved, which is sufficient for the definitions/minimal filter.
    crate::sig::signature(comp)
}

/// The named individuals a component actually mentions AS individuals — the
/// individual part of its typed signature, which `individual_signature` (the
/// whole signature of an ABox axiom) deliberately over-approximates.
fn named_individuals(comp: &Component<RcStr>) -> Vec<String> {
    use horned_owl::model::Component as C;
    let one = |i: &horned_owl::model::Individual<RcStr>| individual_iri(i).into_iter().collect();
    match comp {
        C::DeclareNamedIndividual(d) => vec![d.0 .0.as_ref().to_string()],
        C::ClassAssertion(ax) => one(&ax.i),
        C::ObjectPropertyAssertion(ax) => {
            individual_iri(&ax.from).into_iter().chain(individual_iri(&ax.to)).collect()
        }
        C::NegativeObjectPropertyAssertion(ax) => {
            individual_iri(&ax.from).into_iter().chain(individual_iri(&ax.to)).collect()
        }
        C::DataPropertyAssertion(ax) => one(&ax.from),
        C::NegativeDataPropertyAssertion(ax) => one(&ax.from),
        C::SameIndividual(ax) => ax.0.iter().filter_map(individual_iri).collect(),
        C::DifferentIndividuals(ax) => ax.0.iter().filter_map(individual_iri).collect(),
        _ => Vec::new(),
    }
}

/// The IRI of a named individual (anonymous individuals yield none).
fn individual_iri(i: &horned_owl::model::Individual<RcStr>) -> Option<String> {
    match i {
        horned_owl::model::Individual::Named(n) => Some(n.0.as_ref().to_string()),
        horned_owl::model::Individual::Anonymous(_) => None,
    }
}

/// The OWL 2 / RDFS built-in annotation properties, which never get a synthesised
/// `Declaration` — they are part of the language, not the ontology's vocabulary.
/// An extracted module declares every *other* annotation property it references
/// and skips these.
fn is_builtin_annotation_property(iri: &str) -> bool {
    matches!(
        iri,
        "http://www.w3.org/2000/01/rdf-schema#label"
            | "http://www.w3.org/2000/01/rdf-schema#comment"
            | "http://www.w3.org/2000/01/rdf-schema#seeAlso"
            | "http://www.w3.org/2000/01/rdf-schema#isDefinedBy"
            | "http://www.w3.org/2002/07/owl#deprecated"
            | "http://www.w3.org/2002/07/owl#versionInfo"
            | "http://www.w3.org/2002/07/owl#backwardCompatibleWith"
            | "http://www.w3.org/2002/07/owl#incompatibleWith"
            | "http://www.w3.org/2002/07/owl#priorVersion"
    )
}

fn is_declaration_of(comp: &Component<RcStr>, sigma: &Sigma) -> bool {
    if !matches!(
        comp,
        Component::DeclareClass(_)
            | Component::DeclareObjectProperty(_)
            | Component::DeclareDataProperty(_)
            | Component::DeclareAnnotationProperty(_)
            | Component::DeclareNamedIndividual(_)
            | Component::DeclareDatatype(_)
    ) {
        return false;
    }
    // Declarations are taken from the SOURCE per TYPED entity of Σ, so
    // `Declaration(Class(X))` comes back only where X is in Σ as a class — not
    // where X reached Σ as the individual it is also punned as.
    sig::typed_signature(comp).iter().any(|(k, iri)| sigma.has(*k, iri))
}

fn is_annotation_on(comp: &Component<RcStr>, sig: &HashSet<String>) -> bool {
    match comp {
        Component::AnnotationAssertion(aa) => match &aa.subject {
            horned_owl::model::AnnotationSubject::IRI(iri) => sig.contains(iri.as_ref()),
            _ => false,
        },
        _ => false,
    }
}

/// Whether `comp` is a same/different-individual axiom naming an individual of
/// the module signature. Locality ignores every such axiom, so these are added
/// back once the signature is known.
fn is_individual_identity_on(comp: &Component<RcStr>, sigma: &Sigma) -> bool {
    matches!(
        comp,
        Component::SameIndividual(_) | Component::DifferentIndividuals(_)
    ) && named_individuals(comp)
        .iter()
        .any(|i| sigma.has(sig::kind::NAMED_INDIVIDUAL, i))
}

// --- Locality checking ---------------------------------------------------

/// Whether `comp` is local (trivially satisfied) w.r.t. signature `sigma` under
/// the given locality. Local axioms are excluded from the module.
fn is_local(comp: &Component<RcStr>, sigma: &Sigma, loc: Locality) -> bool {
    match comp {
        // Declarations / metadata / annotations are not module-forming logical
        // axioms; treat as local so they don't pull terms in (they are
        // re-added for the module signature afterwards).
        Component::DeclareClass(_)
        | Component::DeclareObjectProperty(_)
        | Component::DeclareDataProperty(_)
        | Component::DeclareAnnotationProperty(_)
        | Component::DeclareNamedIndividual(_)
        | Component::DeclareDatatype(_)
        | Component::AnnotationAssertion(_)
        | Component::OntologyAnnotation(_)
        | Component::OntologyID(_)
        | Component::DocIRI(_)
        | Component::Import(_)
        | Component::SubAnnotationPropertyOf(_)
        | Component::AnnotationPropertyDomain(_)
        | Component::AnnotationPropertyRange(_)
        // A datatype definition is UNCONDITIONALLY local: syntactic locality gives
        // it no signature test at all, so no module ever contains one. Letting it
        // fall through to the non-local catch-all would put NCIT's nineteen
        // `*-enum` datatypes and their declarations into MONDO's
        // `imports/merged_import.owl`.
        | Component::DatatypeDefinition(_) => true,

        Component::SubClassOf(ax) => {
            is_bot_ce(&ax.sub, sigma, loc) || is_top_ce(&ax.sup, sigma, loc)
        }
        Component::EquivalentClasses(ax) => {
            ax.0.iter().all(|c| is_bot_ce(c, sigma, loc))
                || ax.0.iter().all(|c| is_top_ce(c, sigma, loc))
        }
        Component::DisjointClasses(ax) => {
            // Local if at most one disjunct is non-bottom.
            ax.0.iter().filter(|c| !is_bot_ce(c, sigma, loc)).count() <= 1
        }
        // DisjointUnion(C, D1..Dn) ≡ EquivalentClasses(C, ObjectUnionOf(D1..Dn))
        // ⊓ DisjointClasses(D1..Dn). Local iff BOTH constituents are local, so an
        // external class's disjoint-union definition is dropped — without this arm
        // the catch-all below treats it as non-local and pulls the whole
        // {C, D1..Dn} block into the module.
        Component::DisjointUnion(ax) => {
            let c = CE::Class(ax.0.clone());
            let union = CE::ObjectUnionOf(ax.1.clone());
            let equiv_local = (is_bot_ce(&c, sigma, loc) && is_bot_ce(&union, sigma, loc))
                || (is_top_ce(&c, sigma, loc) && is_top_ce(&union, sigma, loc));
            let disjoint_local =
                ax.1.iter().filter(|d| !is_bot_ce(d, sigma, loc)).count() <= 1;
            equiv_local && disjoint_local
        }
        Component::SubObjectPropertyOf(ax) => {
            is_top_ope(&ax.sup, sigma, loc) || sub_property_is_bot(&ax.sub, sigma, loc)
        }
        // InverseObjectProperties(p, q) ≡ p ≡ q⁻: a tautology under the locality
        // substitution iff BOTH properties vanish (both external). Without this arm
        // the catch-all treats it as non-local and drags both properties into Σ,
        // cascading to their domain/range, sub-property, characteristic and chain
        // axioms.
        Component::InverseObjectProperties(ax) => match loc {
            Locality::Bot => is_bot_ope(&ax.0, sigma, loc) && is_bot_ope(&ax.1, sigma, loc),
            Locality::Top => is_top_ope(&ax.0, sigma, loc) && is_top_ope(&ax.1, sigma, loc),
        },
        Component::ObjectPropertyDomain(ax) => {
            is_bot_ope(&ax.ope, sigma, loc) || is_top_ce(&ax.ce, sigma, loc)
        }
        Component::ObjectPropertyRange(ax) => {
            is_bot_ope(&ax.ope, sigma, loc) || is_top_ce(&ax.ce, sigma, loc)
        }
        // Property characteristics are local iff the property is bottom.
        Component::TransitiveObjectProperty(ax) => is_bot_ope(&ax.0, sigma, loc),
        // Reflexive(R) with R external is a tautology only under TOP-locality
        // (R→top property, which is reflexive); under BOT it is non-trivial.
        Component::ReflexiveObjectProperty(ax) => is_top_ope(&ax.0, sigma, loc),
        Component::SymmetricObjectProperty(ax) => is_bot_ope(&ax.0, sigma, loc),
        Component::AsymmetricObjectProperty(ax) => is_bot_ope(&ax.0, sigma, loc),
        Component::FunctionalObjectProperty(ax) => is_bot_ope(&ax.0, sigma, loc),
        Component::InverseFunctionalObjectProperty(ax) => is_bot_ope(&ax.0, sigma, loc),
        Component::IrreflexiveObjectProperty(ax) => is_bot_ope(&ax.0, sigma, loc),
        Component::EquivalentObjectProperties(ax) => {
            ax.0.iter().all(|p| is_bot_ope(p, sigma, loc))
        }
        Component::DisjointObjectProperties(ax) => {
            ax.0.iter().filter(|p| !is_bot_ope(p, sigma, loc)).count() <= 1
        }

        // HasKey is UNCONDITIONALLY local: syntactic locality gives it no signature
        // test, like the datatype-definition and annotation arms above. Local under
        // every signature means it never enters a module, so no extracted module
        // carries a HasKey axiom — not even when its class and every one of its
        // properties is already in Σ.
        Component::HasKey(_) => true,

        // The ABox. Each assertion form gets its own locality rule, because a
        // catch-all `false` (non-local) over-includes: one non-local assertion
        // puts its individuals into Σ, the module then pulls in every annotation
        // and declaration they carry, and their class assertions drag classes in
        // after them. GSSO's single `owl:differentFrom` would grow its BOT module
        // by 97 individuals and 225 classes that way.
        //
        // `ClassAssertion(C, a)` is local iff C is ⊤-equivalent — under ⊥-locality
        // that means C is literally `owl:Thing`.
        Component::ClassAssertion(ax) => is_top_ce(&ax.ce, sigma, loc),
        // A positive property assertion is never local under ⊥; under ⊤ it is
        // local exactly when its property is external.
        Component::ObjectPropertyAssertion(ax) => match loc {
            Locality::Bot => false,
            Locality::Top => !named_ope_in(&ax.ope, sigma),
        },
        Component::DataPropertyAssertion(ax) => match loc {
            Locality::Bot => false,
            Locality::Top => !sigma.has(sig::kind::DATA_PROPERTY, ax.dp.0.as_ref()),
        },
        // The negative forms are exactly dual.
        Component::NegativeObjectPropertyAssertion(ax) => match loc {
            Locality::Bot => !named_ope_in(&ax.ope, sigma),
            Locality::Top => false,
        },
        Component::NegativeDataPropertyAssertion(ax) => match loc {
            Locality::Bot => !sigma.has(sig::kind::DATA_PROPERTY, ax.dp.0.as_ref()),
            Locality::Top => false,
        },
        // Same/different individuals are local under every locality: locality does
        // not reason about them at all, and the ones whose individuals ended up in
        // the signature are added back afterwards anyway.
        Component::SameIndividual(_) | Component::DifferentIndividuals(_) => true,

        // Data-property axioms — the object-property locality rules, run over the
        // data-property ⊥/⊤ substitution. Without these arms the catch-all below
        // treats every data axiom as non-local and includes it.
        Component::SubDataPropertyOf(ax) => {
            is_top_dp(&ax.sup, sigma, loc) || is_bot_dp(&ax.sub, sigma, loc)
        }
        Component::DataPropertyDomain(ax) => {
            is_bot_dp(&ax.dp, sigma, loc) || is_top_ce(&ax.ce, sigma, loc)
        }
        Component::DataPropertyRange(ax) => is_bot_dp(&ax.dp, sigma, loc),
        Component::FunctionalDataProperty(ax) => is_bot_dp(&ax.0, sigma, loc),
        Component::EquivalentDataProperties(ax) => {
            ax.0.iter().all(|p| is_bot_dp(p, sigma, loc))
        }
        Component::DisjointDataProperties(ax) => {
            ax.0.iter().filter(|p| !is_bot_dp(p, sigma, loc)).count() <= 1
        }

        // Anything not explicitly modeled: conservatively non-local (included),
        // which keeps the module a sound (possibly larger) superset.
        _ => false,
    }
}

/// Whether a class expression is ⊥ under the locality substitution.
fn is_bot_ce(ce: &CE<RcStr>, sigma: &Sigma, loc: Locality) -> bool {
    match ce {
        CE::Class(c) => {
            let iri = c.0.as_ref();
            if iri == OWL_NOTHING {
                return true;
            }
            if iri == OWL_THING {
                return false;
            }
            // A class outside Σ becomes ⊥ under bottom-locality, ⊤ under top.
            !sigma.has(sig::kind::CLASS, iri) && loc == Locality::Bot
        }
        CE::ObjectIntersectionOf(parts) => parts.iter().any(|p| is_bot_ce(p, sigma, loc)),
        CE::ObjectUnionOf(parts) => {
            !parts.is_empty() && parts.iter().all(|p| is_bot_ce(p, sigma, loc))
        }
        CE::ObjectComplementOf(inner) => is_top_ce(inner, sigma, loc),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            is_bot_ope(ope, sigma, loc) || is_bot_ce(bce, sigma, loc)
        }
        CE::ObjectHasValue { ope, .. } => is_bot_ope(ope, sigma, loc),
        // ObjectHasSelf(R) (things R-related to themselves) is ⊥-equivalent when R
        // maps to the empty property (external R under ⊥-locality). Without this
        // arm the catch-all returns false, so `EquivalentClasses(C, hasSelf R)` is
        // wrongly non-local and drags R (and its domain/range/annotations) in.
        CE::ObjectHasSelf(ope) => is_bot_ope(ope, sigma, loc),
        CE::ObjectMinCardinality { ope, bce, n } => {
            *n > 0 && (is_bot_ope(ope, sigma, loc) || is_bot_ce(bce, sigma, loc))
        }
        CE::ObjectExactCardinality { ope, bce, n } => {
            *n > 0 && (is_bot_ope(ope, sigma, loc) || is_bot_ce(bce, sigma, loc))
        }
        // Data restrictions: an external data property maps to the empty property
        // under ⊥-locality, making `∃`, `value`, and `≥n (n>0)` unsatisfiable.
        CE::DataSomeValuesFrom { dp, .. } => is_bot_dp(dp, sigma, loc),
        CE::DataHasValue { dp, .. } => is_bot_dp(dp, sigma, loc),
        CE::DataMinCardinality { dp, n, .. } => *n > 0 && is_bot_dp(dp, sigma, loc),
        CE::DataExactCardinality { dp, n, .. } => *n > 0 && is_bot_dp(dp, sigma, loc),
        _ => false,
    }
}

/// Whether a class expression is ⊤ under the locality substitution.
fn is_top_ce(ce: &CE<RcStr>, sigma: &Sigma, loc: Locality) -> bool {
    match ce {
        CE::Class(c) => {
            let iri = c.0.as_ref();
            if iri == OWL_THING {
                return true;
            }
            if iri == OWL_NOTHING {
                return false;
            }
            !sigma.has(sig::kind::CLASS, iri) && loc == Locality::Top
        }
        CE::ObjectIntersectionOf(parts) => {
            !parts.is_empty() && parts.iter().all(|p| is_top_ce(p, sigma, loc))
        }
        CE::ObjectUnionOf(parts) => parts.iter().any(|p| is_top_ce(p, sigma, loc)),
        CE::ObjectComplementOf(inner) => is_bot_ce(inner, sigma, loc),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            // ∃r.C is ⊤ only in degenerate top-locality with a top role and top
            // filler; conservatively false otherwise.
            is_top_ope(ope, sigma, loc) && is_top_ce(bce, sigma, loc)
        }
        CE::ObjectMaxCardinality { ope, .. } => is_bot_ope(ope, sigma, loc),
        // ObjectHasSelf(R) is ⊤-equivalent when R maps to the universal property
        // (external R under ⊤-locality: everything is R-related to itself).
        CE::ObjectHasSelf(ope) => is_top_ope(ope, sigma, loc),
        // ∀R.C is ⊤ when R maps to the empty property (external R under ⊥) or the
        // filler is ⊤.
        CE::ObjectAllValuesFrom { ope, bce } => {
            is_bot_ope(ope, sigma, loc) || is_top_ce(bce, sigma, loc)
        }
        // Data universals/max over an external (⊥) data property are ⊤.
        CE::DataAllValuesFrom { dp, .. } => is_bot_dp(dp, sigma, loc),
        CE::DataMaxCardinality { dp, .. } => is_bot_dp(dp, sigma, loc),
        _ => false,
    }
}

/// A data property is ⊥ under bottom-locality if it is outside Σ.
fn is_bot_dp(dp: &horned_owl::model::DataProperty<RcStr>, sigma: &Sigma, loc: Locality) -> bool {
    !sigma.has(sig::kind::DATA_PROPERTY, dp.0.as_ref()) && loc == Locality::Bot
}

/// A data property is ⊤ under top-locality if it is outside Σ.
fn is_top_dp(dp: &horned_owl::model::DataProperty<RcStr>, sigma: &Sigma, loc: Locality) -> bool {
    !sigma.has(sig::kind::DATA_PROPERTY, dp.0.as_ref()) && loc == Locality::Top
}

/// A role is ⊥ under bottom-locality if it is outside Σ.
fn is_bot_ope(ope: &OPE<RcStr>, sigma: &Sigma, loc: Locality) -> bool {
    !named_ope_in(ope, sigma) && loc == Locality::Bot
}

/// Whether an object-property expression's NAMED property is in Σ; an inverse is
/// unwrapped to the property it names.
fn named_ope_in(ope: &OPE<RcStr>, sigma: &Sigma) -> bool {
    match ope {
        OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => {
            sigma.has(sig::kind::OBJECT_PROPERTY, p.0.as_ref())
        }
    }
}

/// A role is ⊤ under top-locality if it is outside Σ.
fn is_top_ope(ope: &OPE<RcStr>, sigma: &Sigma, loc: Locality) -> bool {
    !named_ope_in(ope, sigma) && loc == Locality::Top
}

// --- MIREOT --------------------------------------------------------------

/// MIREOT extraction: keep the named-class SubClassOf hierarchy connecting the
/// `lower` seed terms up to the `upper` boundary terms (or to roots when no
/// upper bound is given), plus declarations and annotations on those terms.
pub fn mireot(model: &Model, lower: &HashSet<String>, upper: &HashSet<String>) -> Model {
    mireot_with(model, lower, upper, &HashSet::new(), &ExtractOptions::default())
}

/// MIREOT extraction honoring the post-extraction `opts`. `lower`/`upper` bound
/// an ancestor climb; `branch` terms contribute themselves, their descendants,
/// and the subclass edges among that set — no climb.
pub fn mireot_with(
    model: &Model,
    lower: &HashSet<String>,
    upper: &HashSet<String>,
    branch: &HashSet<String>,
    opts: &ExtractOptions,
) -> Model {
    let out = mireot_core(model, lower, upper, branch);
    let mut seeds = lower.clone();
    seeds.extend(branch.iter().cloned());
    post_process(out, model, &seeds, opts)
}

fn mireot_core(
    model: &Model,
    lower: &HashSet<String>,
    upper: &HashSet<String>,
    branch: &HashSet<String>,
) -> Model {
    // Asserted named superclass edges.
    let mut parents: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for ac in model.ont.iter() {
        if let Component::SubClassOf(sc) = &ac.component {
            if let (CE::Class(sub), CE::Class(sup)) = (&sc.sub, &sc.sup) {
                parents
                    .entry(sub.0.as_ref().to_string())
                    .or_default()
                    .push(sup.0.as_ref().to_string());
            }
        }
    }

    // Walk up from each lower term, collecting terms and edges until reaching an
    // upper-boundary term.
    let mut keep_terms: HashSet<String> = HashSet::new();
    let mut keep_edges: HashSet<(String, String)> = HashSet::new();
    let mut stack: Vec<String> = lower.iter().cloned().collect();
    keep_terms.extend(lower.iter().cloned());
    while let Some(t) = stack.pop() {
        if upper.contains(&t) {
            continue; // boundary: include the term, but stop ascending
        }
        if let Some(sups) = parents.get(&t) {
            for s in sups {
                keep_edges.insert((t.clone(), s.clone()));
                if keep_terms.insert(s.clone()) {
                    stack.push(s.clone());
                }
            }
        }
    }

    // Branch terms: the set itself, with the subclass edges INSIDE it. A branch
    // member's superclass outside the set stays out — no ancestor climb.
    keep_terms.extend(branch.iter().cloned());
    for (sub, sups) in &parents {
        if branch.contains(sub) {
            for s in sups {
                if branch.contains(s) {
                    keep_edges.insert((sub.clone(), s.clone()));
                }
            }
        }
    }

    let mut ont = SetOntology::new();
    for ac in model.ont.iter() {
        let keep = match &ac.component {
            Component::SubClassOf(sc) => match (&sc.sub, &sc.sup) {
                (CE::Class(sub), CE::Class(sup)) => keep_edges.contains(&(
                    sub.0.as_ref().to_string(),
                    sup.0.as_ref().to_string(),
                )),
                _ => false,
            },
            Component::DeclareClass(_) => {
                sig::signature(&ac.component).iter().any(|s| keep_terms.contains(s))
            }
            Component::AnnotationAssertion(_) => is_annotation_on(&ac.component, &keep_terms),
            // The module is a NEW, anonymous ontology: no ontology IRI, no
            // version, no document IRI travels from the source.
            Component::OntologyID(_) | Component::DocIRI(_) => false,
            _ => false,
        };
        if keep {
            ont.insert(ac.clone());
        }
    }

    // The annotation PROPERTIES the module uses come with their own frames: any
    // assertion in the source whose subject is a used property is copied, to a
    // fixpoint (a property frame can use further properties, which then bring
    // their frames too). Properties the source says nothing about stay bare.
    let mut used_props: HashSet<String> = ont
        .iter()
        .filter_map(|ac| match &ac.component {
            Component::AnnotationAssertion(aa) => Some(aa.ann.ap.0.as_ref().to_string()),
            _ => None,
        })
        .collect();
    loop {
        let mut added = false;
        for ac in model.ont.iter() {
            if let Component::AnnotationAssertion(aa) = &ac.component {
                if let horned_owl::model::AnnotationSubject::IRI(s) = &aa.subject {
                    if used_props.contains(s.as_ref()) && ont.insert(ac.clone()) {
                        added = true;
                        used_props.insert(aa.ann.ap.0.as_ref().to_string());
                    }
                }
            }
        }
        if !added {
            break;
        }
    }
    // Every annotation property the module uses is DECLARED — built-ins
    // included; the module stands alone.
    let build: horned_owl::model::Build<RcStr> = horned_owl::model::Build::new();
    for p in &used_props {
        ont.insert(Component::DeclareAnnotationProperty(
            horned_owl::model::DeclareAnnotationProperty(build.annotation_property(p.as_str())),
        ));
    }
    Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes))
}

fn sub_property_is_bot(
    sub: &horned_owl::model::SubObjectPropertyExpression<RcStr>,
    sigma: &Sigma,
    loc: Locality,
) -> bool {
    use horned_owl::model::SubObjectPropertyExpression as S;
    match sub {
        S::ObjectPropertyExpression(ope) => is_bot_ope(ope, sigma, loc),
        S::ObjectPropertyChain(chain) => chain.iter().any(|o| is_bot_ope(o, sigma, loc)),
    }
}
