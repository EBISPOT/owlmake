//! `rename` — give entities new IRIs.
//!
//! `--mappings` names a table, TSV or TXT or CSV, whose header row is followed
//! by rows of old IRI, new IRI and, optionally, a new label for the renamed
//! entity; `--mapping OLD NEW` adds one pair. `--prefix-mappings` names a table
//! of old and new bases. Every IRI is read with the command line's prefixes.
//!
//! The pairs are kept as a `java.util.HashMap` keyed on the old IRI as written,
//! and renamed one at a time in that map's order, each over the ontology the
//! renames before it left ([`Renamer::change_iri`]). The prefix renames come
//! after, over the same map with the prefix pairs put into it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use horned_owl::model::{
    AnnotatedComponent, Annotation, AnnotationAssertion, AnnotationProperty, AnnotationSubject,
    AnnotationValue, Build, Class, Component, DataProperty, Datatype, Literal, MutableOntology,
    NamedIndividual, ObjectProperty, RcStr, Variable, IRI,
};
use horned_owl::visitor::immutable::{Visit, Walk};
use horned_owl::visitor::mutable::{VisitMut, WalkMut};

use crate::cmd::select;
use crate::model::Model;
use crate::owlapi_hash::{hashset_order, iri_hash, java_hashset_capacity, java_string_hash};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub format: Option<String>,
    /// A single OLD NEW IRI/CURIE mapping. Repeatable.
    #[arg(long, num_args = 2, value_names = ["OLD", "NEW"])]
    pub mapping: Vec<String>,
    /// A table of `old`, `new` and optional `label` columns under a header row:
    /// TSV for a `.tsv` or `.txt` file, CSV for a `.csv` one.
    #[arg(short = 'm', long)]
    pub mappings: Option<PathBuf>,
    /// Allow mappings for entities that do not appear in the ontology
    /// (default false). `<bool>`.
    #[arg(short = 'M', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub allow_missing_entities: Option<bool>,
    /// Allow two or more rows of the `--mappings` table to give the same new
    /// IRI (default false). `<bool>`.
    #[arg(short = 'd', long, num_args = 1, default_missing_value = "true", value_parser = crate::cmd::BoolParser)]
    pub allow_duplicates: Option<bool>,
    /// A table of `old base`, `new base` columns under a header row: every
    /// entity whose IRI starts with an old base gets the IRI with each
    /// occurrence of that base replaced by the new one.
    #[arg(short = 'r', long = "prefix-mappings", value_name = "FILE")]
    pub prefix_mappings: Option<PathBuf>,

    #[command(flatten)]
    pub common: crate::cmd::CommonArgs,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    step(None, &args)?;
    Ok(())
}

pub fn step(piped: Option<Model>, args: &Args) -> anyhow::Result<Option<Model>> {
    let mut model = crate::cmd::take_or_load(piped, args.input.as_deref(), &args.common)?;
    args.common.apply(&mut model)?;
    if args.mappings.is_none() && args.prefix_mappings.is_none() && args.mapping.is_empty() {
        bail!("MISSING MAPPINGS ERROR either a --mappings or a --prefix-mappings file must be specified.");
    }
    let allow_duplicates = args.allow_duplicates.unwrap_or(false);
    let allow_missing = args.allow_missing_entities.unwrap_or(false);
    let mut mappings = JavaMap::default();
    if args.mappings.is_some() || !args.mapping.is_empty() {
        let mut labels: HashMap<String, String> = HashMap::new();
        if let Some(path) = &args.mappings {
            for row in read_table(path, allow_duplicates)? {
                mappings.put(&row.old, &row.new);
                if let Some(label) = row.label {
                    labels.insert(select::expand(&model, &row.new), label);
                }
            }
        }
        for pair in args.mapping.chunks(2) {
            if let [old, new] = pair {
                mappings.put(old, new);
            }
        }
        rename_full(&mut model, &mappings, &labels, allow_missing)?;
    }
    if let Some(path) = &args.prefix_mappings {
        for row in read_table(path, true)? {
            mappings.put(&row.old, &row.new);
        }
        rename_prefixes(&mut model, &mappings);
    }
    crate::cmd::maybe_save(&mut model, args.output.as_deref(), args.format.as_deref())?;
    Ok(Some(model))
}

/// Rename each pair's entity, in the map's order: the old IRI must name an
/// entity of the ontology, unless `allow_missing`; a pair with a label then
/// replaces the renamed entity's `rdfs:label`s with it.
fn rename_full(
    model: &mut Model,
    mappings: &JavaMap,
    labels: &HashMap<String, String>,
    allow_missing: bool,
) -> Result<()> {
    let mut renamer = Renamer::new(model);
    for (old, new) in mappings.entries() {
        let Some(old_iri) = select::iri(model, old) else {
            bail!("iri cannot be null");
        };
        if !renamer.names_entity(&old_iri) {
            if !allow_missing {
                bail!("MISSING ENTITY ERROR entity to rename ('{old_iri}') does not exist.");
            }
            status!("rename: entity {old_iri} is in the map, but does not exist in the ontology");
        }
        let Some(new_iri) = select::iri(model, new) else {
            bail!("NEW IRI ERROR failed to generate an IRI for '{new}'.");
        };
        renamer.change_iri(&old_iri, &new_iri);
        if let Some(label) = labels.get(&new_iri) {
            renamer.relabel(model, &new_iri, label)?;
        }
    }
    renamer.finish(model);
    Ok(())
}

/// Rename, for each pair of the map in its order, every entity whose IRI starts
/// with the old base as written: to the IRI with each occurrence of the base
/// replaced by the new one. The entities are those of the ontology before the
/// first of these renames, taken in the order of a `java.util.HashSet` of
/// their IRIs.
fn rename_prefixes(model: &mut Model, mappings: &JavaMap) {
    let mut renamer = Renamer::new(model);
    let all: Vec<String> = renamer.entity_iris();
    for (old_base, new_base) in mappings.entries() {
        let matched: Vec<&String> = all.iter().filter(|iri| iri.starts_with(old_base.as_str())).collect();
        if matched.is_empty() {
            status!("rename: no entities with prefix '{old_base}' to rename");
            continue;
        }
        for iri in iri_set_order(&matched, all.len()) {
            let new_iri = select::expand(model, &iri.replace(old_base.as_str(), new_base));
            renamer.change_iri(iri, &new_iri);
        }
    }
    renamer.finish(model);
}

/// `iris` in the order a `java.util.HashSet` collected from a set of `total`
/// IRIs holds them: by bucket of their IRI hashes, ties by the larger set's
/// bucket, then by IRI.
fn iri_set_order<'a>(iris: &[&'a String], total: usize) -> Vec<&'a String> {
    let hashes: Vec<i32> = iris.iter().map(|i| iri_hash(i)).collect();
    let small = java_hashset_capacity(iris.len()) as u32;
    let large = java_hashset_capacity(total) as u32;
    let spread = |h: i32| {
        let h = h as u32;
        h ^ (h >> 16)
    };
    let mut idx: Vec<usize> = (0..iris.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ha, hb) = (spread(hashes[a]), spread(hashes[b]));
        (ha & (small - 1), ha & (large - 1), iris[a]).cmp(&(hb & (small - 1), hb & (large - 1), iris[b]))
    });
    idx.into_iter().map(|i| iris[i]).collect()
}

/// The pairs of a `java.util.HashMap<String, String>`, in the order they were
/// first put: putting a key again replaces its value and keeps its place.
#[derive(Default)]
struct JavaMap {
    pairs: Vec<(String, String)>,
}

impl JavaMap {
    fn put(&mut self, key: &str, value: &str) {
        match self.pairs.iter_mut().find(|(k, _)| k == key) {
            Some(pair) => pair.1 = value.to_string(),
            None => self.pairs.push((key.to_string(), value.to_string())),
        }
    }

    /// The pairs in the map's iteration order: by bucket of the keys' string
    /// hashes, in the order they were put within a bucket.
    fn entries(&self) -> Vec<(&String, &String)> {
        let hashes: Vec<i32> = self.pairs.iter().map(|(k, _)| java_string_hash(k)).collect();
        hashset_order(&hashes).into_iter().map(|i| (&self.pairs[i].0, &self.pairs[i].1)).collect()
    }
}

/// One row of a mappings table.
struct Row {
    old: String,
    new: String,
    label: Option<String>,
}

/// The rows of a mappings table, in the order of a `java.util.HashMap` keyed
/// on their old IRIs. The header must have two or three values and every row
/// two or three; an old IRI given twice is refused, and so is a new IRI given
/// twice unless `allow_duplicates`.
fn read_table(path: &Path, allow_duplicates: bool) -> Result<Vec<Row>> {
    let name = path.to_string_lossy();
    let delim = if name.ends_with(".tsv") || name.ends_with(".txt") {
        '\t'
    } else if name.ends_with(".csv") {
        ','
    } else {
        bail!("FILE FORMAT ERROR the mappings file ('{name}') must be a CSV or TSV/TXT file.");
    };
    if !path.exists() {
        bail!("MISSING FILE ERROR mappings file '{name}' does not exist.");
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {name}"))?;
    let mut records = crate::table::read_with_blank_lines(&text, delim).into_iter();
    let header = records.next().unwrap_or_default();
    if !(2..=3).contains(&header.len()) {
        bail!("COLUMN COUNT ERROR file '{name}' must have 2 or 3 header values");
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut olds: std::collections::HashSet<String> = Default::default();
    let mut news: std::collections::HashSet<String> = Default::default();
    for (i, record) in records.enumerate() {
        let line = i + 2;
        if !(2..=3).contains(&record.len()) {
            bail!("COLUMN COUNT ERROR line {line} in file '{name}' must contain exactly {} values.", header.len());
        }
        let (old, new) = (record[0].clone(), record[1].clone());
        let label = record.get(2).filter(|l| !l.trim().is_empty()).cloned();
        if !olds.insert(old.clone()) {
            bail!("DUPLICATE MAPPING ERROR line {line} in file '{name}' contains a duplicate mapping for value '{old}'.");
        }
        if !news.insert(new.clone()) {
            if !allow_duplicates {
                bail!("DUPLICATE RENAME ERROR line {line} in file '{name}' contains a duplicate rename for value '{new}'");
            }
            status!("rename: IRI '{new}' will be used for two or more entities");
        }
        rows.push(Row { old, new, label });
    }
    let hashes: Vec<i32> = rows.iter().map(|r| java_string_hash(&r.old)).collect();
    let order = hashset_order(&hashes);
    let mut rows: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
    Ok(order.into_iter().filter_map(|i| rows[i].take()).collect())
}

/// The ontology's components, held while they are renamed one IRI at a time,
/// each in a slot, with the slots each IRI is named from.
///
/// Renaming an IRI rewrites the axioms that name an entity with it — in the
/// axiom or in an annotation on it, at any depth — and the annotation
/// assertions about it; and the ontology annotations that hold it anywhere. In
/// each, every occurrence of the IRI is replaced: as an entity, an annotation
/// subject or value, a literal's datatype or a rule variable. An axiom that
/// holds the IRI only as an annotation value keeps it. The ontology's IRI,
/// version IRI and imports are not renamed.
struct Renamer {
    slots: Vec<Option<AnnotatedComponent<RcStr>>>,
    /// The components no rename touches: the ontology's IRIs and imports.
    fixed: Vec<AnnotatedComponent<RcStr>>,
    /// For each IRI, the slots a rename of it rewrites.
    rewrites: HashMap<String, BTreeSet<usize>>,
    /// For each entity, as its kind and IRI, the slots that name it: the
    /// ontology's signature. An ontology annotation names only its annotation
    /// properties.
    signature: HashMap<(u8, String), BTreeSet<usize>>,
    /// Each rename made, in order.
    made: Vec<(String, String)>,
    build: Build<RcStr>,
    order: crate::io::natural_order::NaturalOrder,
}

impl Renamer {
    fn new(model: &mut Model) -> Renamer {
        let ont = std::mem::take(&mut model.ont);
        let mut renamer = Renamer {
            slots: Vec::new(),
            fixed: Vec::new(),
            rewrites: HashMap::new(),
            signature: HashMap::new(),
            made: Vec::new(),
            build: Build::new(),
            order: model.natural_order(),
        };
        for ac in ont.into_iter() {
            match ac.component {
                Component::OntologyID(_) | Component::DocIRI(_) | Component::Import(_) => renamer.fixed.push(ac),
                _ => renamer.add(ac),
            }
        }
        renamer
    }

    fn add(&mut self, ac: AnnotatedComponent<RcStr>) {
        let slot = self.slots.len();
        self.index(slot, &ac, true);
        self.slots.push(Some(ac));
    }

    /// Enter (or, without `enter`, withdraw) `slot`, holding `ac`, under every
    /// IRI and entity it is indexed by.
    fn index(&mut self, slot: usize, ac: &AnnotatedComponent<RcStr>, enter: bool) {
        let named = Named::of(ac);
        for iri in named.rewrites {
            let slots = self.rewrites.entry(iri).or_default();
            if enter {
                slots.insert(slot);
            } else {
                slots.remove(&slot);
            }
        }
        for entity in named.entities {
            let slots = self.signature.entry(entity).or_default();
            if enter {
                slots.insert(slot);
            } else {
                slots.remove(&slot);
            }
        }
    }

    /// Whether an entity of the ontology has the IRI.
    fn names_entity(&self, iri: &str) -> bool {
        self.kinds(iri) > 0
    }

    /// How many kinds of entity of the ontology have the IRI.
    fn kinds(&self, iri: &str) -> usize {
        use crate::sig::kind::*;
        [CLASS, OBJECT_PROPERTY, DATA_PROPERTY, NAMED_INDIVIDUAL, DATATYPE, ANNOTATION_PROPERTY]
            .into_iter()
            .filter(|&k| self.signature.get(&(k, iri.to_string())).is_some_and(|s| !s.is_empty()))
            .count()
    }

    /// The IRI of every entity of the ontology, each once.
    fn entity_iris(&self) -> Vec<String> {
        let iris: BTreeSet<&String> =
            self.signature.iter().filter(|(_, slots)| !slots.is_empty()).map(|((_, iri), _)| iri).collect();
        iris.into_iter().cloned().collect()
    }

    /// Rename `old` to `new`.
    fn change_iri(&mut self, old: &str, new: &str) {
        self.made.push((old.to_string(), new.to_string()));
        if old == new {
            return;
        }
        let slots: Vec<usize> = self.rewrites.get(old).map(|s| s.iter().copied().collect()).unwrap_or_default();
        let rename: HashMap<String, String> = HashMap::from([(old.to_string(), new.to_string())]);
        for slot in slots {
            let Some(mut ac) = self.slots[slot].take() else { continue };
            self.index(slot, &ac, false);
            replace_iris(&mut ac, &rename, &self.build);
            if let Component::Rule(rule) = &mut ac.component {
                rehash_atoms(&mut rule.body, self.order);
                rehash_atoms(&mut rule.head, self.order);
            }
            self.index(slot, &ac, true);
            self.slots[slot] = Some(ac);
        }
    }

    /// Replace the `rdfs:label`s of the entity `iri` with `label`. The IRI must
    /// name exactly one entity.
    fn relabel(&mut self, model: &Model, iri: &str, label: &str) -> Result<()> {
        match self.kinds(iri) {
            0 => bail!("MISSING ENTITY ERROR ontology does not contain entity: {iri}"),
            1 => {}
            _ => bail!("MULTIPLE ENTITIES ERROR multiple entities represented by: {iri}"),
        }
        const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
        let labels: Vec<usize> = self
            .rewrites
            .get(iri)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&slot| {
                matches!(
                    self.slots[slot].as_ref().map(|ac| &ac.component),
                    Some(Component::AnnotationAssertion(aa))
                        if aa.ann.ap.0.as_ref() == RDFS_LABEL
                            && matches!(&aa.subject, AnnotationSubject::IRI(s) if s.as_ref() == iri)
                )
            })
            .collect();
        for slot in labels {
            if let Some(ac) = self.slots[slot].take() {
                self.index(slot, &ac, false);
            }
        }
        let b = &model.build;
        let assertion = AnnotationAssertion {
            subject: AnnotationSubject::IRI(b.iri(iri)),
            ann: Annotation {
                ap: AnnotationProperty(b.iri(RDFS_LABEL)),
                av: AnnotationValue::Literal(Literal::Datatype {
                    literal: label.to_string(),
                    datatype_iri: b.iri("http://www.w3.org/2001/XMLSchema#string"),
                }),
                ann: Default::default(),
            },
        };
        self.add(AnnotatedComponent { component: Component::AnnotationAssertion(assertion), ann: Default::default() });
        Ok(())
    }

    /// Put the renamed components back, each once, and rename what the model
    /// keeps of its blank nodes by IRI with them.
    fn finish(self, model: &mut Model) {
        for ac in self.fixed.into_iter().chain(self.slots.into_iter().flatten()) {
            model.ont.insert(ac);
        }
        // The renames composed: where each IRI ends up after all of them.
        let mut after: HashMap<String, String> = HashMap::new();
        for (old, new) in self.made.iter().rev() {
            let end = after.get(new).cloned().unwrap_or_else(|| new.clone());
            after.insert(old.clone(), end);
        }
        rename_blank_node_keys(model, &after);
    }
}

/// The IRIs a component is indexed by.
#[derive(Default)]
struct Named {
    /// The IRIs a rename rewrites the component for.
    rewrites: BTreeSet<String>,
    /// The entities it names, by kind and IRI.
    entities: BTreeSet<(u8, String)>,
}

impl Named {
    fn of(ac: &AnnotatedComponent<RcStr>) -> Named {
        if let Component::OntologyAnnotation(oa) = &ac.component {
            // An ontology annotation is rewritten for any IRI it holds, and
            // names only its annotation properties.
            let mut walk = Walk::new(Every::default());
            walk.annotation(&oa.0);
            let every = walk.into_visit();
            let entities = every.properties.into_iter().map(|p| (crate::sig::kind::ANNOTATION_PROPERTY, p)).collect();
            return Named { rewrites: every.iris, entities };
        }
        let mut walk = Walk::new(Entities::default());
        walk.annotated_component(ac);
        let entities = walk.into_visit();
        let mut rewrites: BTreeSet<String> = entities.0.iter().map(|(_, iri)| iri.clone()).collect();
        if let Component::AnnotationAssertion(aa) = &ac.component {
            if let AnnotationSubject::IRI(s) = &aa.subject {
                rewrites.insert(s.as_ref().to_string());
            }
        }
        Named { rewrites, entities: entities.0 }
    }
}

/// The entities a component names, in it and in its annotations, by kind and
/// IRI: a literal names its datatype.
#[derive(Default)]
struct Entities(BTreeSet<(u8, String)>);

impl Visit<RcStr> for Entities {
    fn visit_class(&mut self, e: &Class<RcStr>) {
        self.0.insert((crate::sig::kind::CLASS, e.0.as_ref().to_string()));
    }
    fn visit_object_property(&mut self, e: &ObjectProperty<RcStr>) {
        self.0.insert((crate::sig::kind::OBJECT_PROPERTY, e.0.as_ref().to_string()));
    }
    fn visit_data_property(&mut self, e: &DataProperty<RcStr>) {
        self.0.insert((crate::sig::kind::DATA_PROPERTY, e.0.as_ref().to_string()));
    }
    fn visit_named_individual(&mut self, e: &NamedIndividual<RcStr>) {
        self.0.insert((crate::sig::kind::NAMED_INDIVIDUAL, e.0.as_ref().to_string()));
    }
    fn visit_datatype(&mut self, e: &Datatype<RcStr>) {
        self.0.insert((crate::sig::kind::DATATYPE, e.0.as_ref().to_string()));
    }
    fn visit_annotation_property(&mut self, e: &AnnotationProperty<RcStr>) {
        self.0.insert((crate::sig::kind::ANNOTATION_PROPERTY, e.0.as_ref().to_string()));
    }
    fn visit_literal(&mut self, l: &Literal<RcStr>) {
        if let Literal::Datatype { datatype_iri, .. } = l {
            self.0.insert((crate::sig::kind::DATATYPE, datatype_iri.as_ref().to_string()));
        }
    }
}

/// Every IRI a component or annotation holds, a rule variable's among them,
/// and its annotation properties.
#[derive(Default)]
struct Every {
    iris: BTreeSet<String>,
    properties: BTreeSet<String>,
}

impl Visit<RcStr> for Every {
    fn visit_iri(&mut self, iri: &IRI<RcStr>) {
        self.iris.insert(iri.as_ref().to_string());
    }
    fn visit_variable(&mut self, v: &Variable<RcStr>) {
        self.iris.insert(v.0.as_ref().to_string());
    }
    fn visit_annotation_property(&mut self, e: &AnnotationProperty<RcStr>) {
        self.properties.insert(e.0.as_ref().to_string());
    }
}

/// Put a rewritten rule's atoms in the order a `java.util.HashSet` holds the
/// copies of them: a rule keeps its atoms in the order of the set it is made
/// from.
fn rehash_atoms(atoms: &mut Vec<horned_owl::model::Atom<RcStr>>, order: crate::io::natural_order::NaturalOrder) {
    let hashes: Vec<i32> = atoms.iter().map(|a| crate::owlapi_hash::swrl_atom_hash(a, order)).collect();
    let mut taken: Vec<Option<horned_owl::model::Atom<RcStr>>> = std::mem::take(atoms).into_iter().map(Some).collect();
    *atoms = hashset_order(&hashes).into_iter().filter_map(|i| taken[i].take()).collect();
}

/// Replace every IRI of a component that `rename` maps, wherever it occurs.
fn replace_iris(ac: &mut AnnotatedComponent<RcStr>, rename: &HashMap<String, String>, build: &Build<RcStr>) {
    struct Replace<'a> {
        rename: &'a HashMap<String, String>,
        build: &'a Build<RcStr>,
    }
    impl VisitMut<RcStr> for Replace<'_> {
        fn visit_iri(&mut self, iri: &mut IRI<RcStr>) {
            if let Some(new) = self.rename.get(iri.as_ref()) {
                *iri = self.build.iri(new.as_str());
            }
        }
        fn visit_variable(&mut self, v: &mut Variable<RcStr>) {
            self.visit_iri(&mut v.0);
        }
    }
    let mut walk = WalkMut::new(Replace { rename, build });
    walk.annotated_component(ac);
}

/// Replace every IRI `map` maps, wherever it occurs in the ontology's axioms and
/// ontology annotations, all at once; the ontology's IRI, version IRI and
/// imports are kept. What the model keeps of its blank nodes by IRI is renamed
/// with them.
pub fn rename_model(mut model: Model, map: &HashMap<String, String>) -> Result<Model> {
    let map: HashMap<String, String> = map.iter().filter(|(old, new)| old != new).map(|(o, n)| (o.clone(), n.clone())).collect();
    if map.is_empty() {
        return Ok(model);
    }
    let renamed: Vec<AnnotatedComponent<RcStr>> = model
        .ont
        .iter()
        .filter(|ac| !matches!(ac.component, Component::OntologyID(_) | Component::DocIRI(_) | Component::Import(_)))
        .filter(|ac| {
            let mut walk = Walk::new(Every::default());
            walk.annotated_component(ac);
            walk.into_visit().iris.iter().any(|iri| map.contains_key(iri))
        })
        .cloned()
        .collect();
    for mut ac in renamed {
        model.ont.remove(&ac);
        replace_iris(&mut ac, &map, &model.build);
        model.ont.insert(ac);
    }
    rename_blank_node_keys(&mut model, &map);
    Ok(model)
}

/// Rename the model's blank-node evidence, which is keyed by IRIs — owners,
/// and the property and filler parts of shared-node keys — so that a renamed
/// entity's axioms keep the nodes they share.
fn rename_blank_node_keys(model: &mut Model, map: &HashMap<String, String>) {
    if map.is_empty() {
        return;
    }
    let ren = |s: &str| -> String { map.get(s).cloned().unwrap_or_else(|| s.to_string()) };
    let parts = |k: &str| -> String { k.split('\u{1}').map(&ren).collect::<Vec<_>>().join("\u{1}") };
    model.owl_shared_owners = std::mem::take(&mut model.owl_shared_owners)
        .into_iter()
        .map(|(owner, keys)| (ren(&owner), keys.into_iter().map(|k| parts(&k)).collect()))
        .collect();
    model.shared_anon = std::mem::take(&mut model.shared_anon).into_iter().map(|(owner, v)| (ren(&owner), v)).collect();
    model.cross_shared = std::mem::take(&mut model.cross_shared).into_iter().map(|(k, g)| (parts(&k), g)).collect();
}
