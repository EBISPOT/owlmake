//! Shared term-selection logic for `filter` and `remove`.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::model::Model;

/// Trim only code points `<= U+0020` — NOT Unicode whitespace.
///
/// A NO-BREAK SPACE (U+00A0) is whitespace to Unicode and is not trimmed here, so
/// a term carrying one keeps it and resolves to nothing.
pub(crate) fn ascii_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= '\u{20}')
}

/// One line of a `--term-file`, or `None` if it contributes no term.
///
/// The parse is three rules and no more: carriage returns are stripped; a line
/// whose first non-blank character is `#` contributes nothing; and a `#` at the
/// start of the line, or preceded by ASCII whitespace, ends the term, with what
/// remains trimmed.
///
/// Two details decide real seeds. It does NOT take the first whitespace token —
/// only a `#` comment is stripped, so `GO:1 ! label` stays whole and then fails
/// to resolve. And the trim strips only chars `<= U+0020`, so a NO-BREAK SPACE
/// survives: HPO's `chebi_terms.txt` ends seven lines with `U+00A0`, which read
/// as `CHEBI:15843\u{a0}` — a CURIE that does not expand, so (under
/// `--force true`) those lines drop out. Rust's `split_whitespace`/`trim` treat
/// U+00A0 as whitespace, so a naive read keeps those seven and pulls 34 extra
/// CHEBI classes into `merged_import.owl`.
pub(crate) fn term_line(line: &str) -> Option<&str> {
    let line = line.trim_end_matches('\r');
    let jtrim = ascii_trim;
    if jtrim(line).starts_with('#') {
        return None;
    }
    // A `#` at the start, or preceded by ASCII whitespace — `[ \t\n\x0B\f\r]`,
    // which excludes U+00A0.
    let is_ascii_ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r');
    let mut cut = None;
    for (i, c) in line.char_indices() {
        if c != '#' {
            continue;
        }
        if i == 0 {
            cut = Some(0);
            break;
        }
        if line[..i].chars().next_back().is_some_and(is_ascii_ws) {
            cut = Some(i);
            break;
        }
    }
    let body = jtrim(&line[..cut.unwrap_or(line.len())]);
    (!body.is_empty()).then_some(body)
}

/// Gather the seed term set from `--term` values and `--term-file` files,
/// expanding CURIEs against the model's prefix map.
pub fn collect_terms(
    model: &Model,
    terms: &[String],
    term_files: &[PathBuf],
) -> Result<HashSet<String>> {
    let mut set = HashSet::new();
    for t in terms {
        set.insert(expand(model, t));
    }
    for f in term_files {
        let content =
            std::fs::read_to_string(f).with_context(|| format!("reading term file {}", f.display()))?;
        for line in content.lines() {
            let Some(line) = term_line(line) else { continue };
            set.insert(expand(model, line));
        }
    }
    Ok(set)
}

/// The terms that actually SELECT something in `filter`/`remove`.
///
/// A `--term`/`--term-file` entry resolves only when the IRI names an entity of
/// EXACTLY ONE kind in the ontology's signature. An IRI the ontology never
/// mentions resolves to nothing, and so does a PUNNED one: an IRI used as more
/// than one kind of entity is ambiguous, and an ambiguous term selects nothing.
///
/// GSSO puns `GSSO_000699` as a class and an individual and EFO's import seed
/// lists it, so `gsso_import.owl` loses the class while keeping every unpunned
/// neighbour. `extract` is deliberately NOT this: it seeds from every entity an
/// IRI names, which is why the same term survives into `gsso_bot.owl`.
pub fn resolve_entity_terms(model: &Model, terms: HashSet<String>) -> HashSet<String> {
    let sig = signature_entities(model);
    let count = |iri: &String| {
        [
            &sig.classes,
            &sig.object_properties,
            &sig.data_properties,
            &sig.annotation_properties,
            &sig.datatypes,
            &sig.individuals,
        ]
        .iter()
        .filter(|k| k.contains(iri))
        .count()
    };
    terms.into_iter().filter(|t| count(t) == 1).collect()
}

/// Expand a CURIE against the model's prefix map; pass full IRIs through. Also
/// strips surrounding angle brackets. A `PREFIX:LOCAL` the model's map does not
/// cover is expanded only when `obo_context.jsonld` binds `PREFIX`; a prefix bound
/// nowhere is returned unchanged rather than assumed to follow the OBO PURL
/// convention — see the note in the body for why.
pub fn expand(model: &Model, s: &str) -> String {
    // The same trim a term-file line gets (see [`term_line`]): code points
    // `<= U+0020` only. A term carrying a NO-BREAK SPACE keeps it, so the CURIE
    // expands to an IRI that names no entity and the term selects nothing —
    // `imports/chebi_terms.txt` ends seven of its lines with one.
    let s = ascii_trim(s);
    let s = s.strip_prefix('<').and_then(|x| x.strip_suffix('>')).unwrap_or(s);
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("urn:") {
        return s.to_string();
    }
    if let Ok(expanded) = model.prefixes.expand_curie_string(s) {
        return expanded;
    }
    // A COMMAND-LINE term is expanded only by a prefix that is actually BOUND: in
    // the ontology's own map, in whatever `--prefix` added, or in the bundled
    // `obo_context.jsonld`. A prefix bound in none of those is not expanded at all —
    // `MGPO:0001001` stays an IRI whose scheme is `MGPO`, which names no entity, so
    // the term selects nothing.
    //
    // uPheno's merged mirror is where that matters. It passes fourteen
    // `--root-phenotype` roots, and MGPO is the one prefix of the fourteen that
    // `obo_context.jsonld` does not bind; a caller that wants it in scope binds it
    // with an explicit `--prefix "MGPO: …"`. Expanding it by the OBO convention
    // regardless would pull every MGPO phenotype into scope and add `UPHENO:0000001`
    // and `UPHENO:0000003` axioms the merged mirror must not carry.
    //
    // This is the term-resolution rule only. An OBO document's own ids really do
    // follow the `PREFIX:LOCAL` → `obo/PREFIX_LOCAL` convention whatever any
    // context says, and [`crate::io::obo::expand_id`] still does that.
    if let Some((pre, local)) = s.split_once(':') {
        if let Some(ns) = obo_context_namespace(pre) {
            return format!("{ns}{local}");
        }
    }
    s.to_string()
}

/// The namespace `obo_context.jsonld` binds `prefix` to, if any.
fn obo_context_namespace(prefix: &str) -> Option<&'static str> {
    static MAP: std::sync::OnceLock<std::collections::HashMap<String, String>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| crate::report::obo_context_prefixes().into_iter().collect())
        .get(prefix)
        .map(|s| s.as_str())
}

/// The keyword `--select` selectors owlmake recognises (so any other token is an
/// IRI / CURIE / wildcard entity pattern).
const SELECT_KEYWORDS: &[&str] = &[
    "imports", "complement", "ontology", "anonymous", "named", "self", "annotations",
    "classes", "object-properties", "data-properties", "annotation-properties", "individuals",
    "named-individuals", "datatypes", "properties", "parents", "ancestors", "children",
    "descendants", "equivalents", "instances", "types", "domains", "ranges",
];

/// Whether a `--select` token is an entity pattern (an IRI/CURIE/wildcard) rather
/// than one of the known keyword selectors. A `PROP=VALUE` annotation-value
/// selector is neither — it has its own resolution, and glob-matching it against
/// entity IRIs would select nothing.
pub fn is_pattern(tok: &str) -> bool {
    !SELECT_KEYWORDS.contains(&tok) && parse_annotation_value(tok).is_none()
}

/// Split a `PROP=VALUE` annotation-value selector into its two halves.
///
/// Only a token whose left side is a CURIE or IRI qualifies, so a wildcard like
/// `<…/UBERON_*>` — which has no `=` — and an ordinary keyword are both left
/// alone.
pub fn parse_annotation_value(tok: &str) -> Option<(&str, &str)> {
    let (p, v) = tok.split_once('=')?;
    let (p, v) = (p.trim(), v.trim());
    if p.is_empty() || v.is_empty() {
        return None;
    }
    // A left side that is a CURIE (`oboInOwl:inSubset`) or a full IRI. Anything
    // else — an `=` inside a wildcard, say — is not this selector.
    let looks_like_property =
        p.starts_with('<') || p.starts_with("http://") || p.starts_with("https://") || {
            let mut it = p.splitn(2, ':');
            matches!((it.next(), it.next()), (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() && !b.contains('/'))
        };
    looks_like_property.then_some((p, v))
}

/// Entities carrying `AnnotationAssertion(PROP, entity, VALUE)`, for a
/// `--select 'PROP=VALUE'` selector.
///
/// Both halves are expanded against the ontology's prefix map and whatever
/// `--prefix` bound, so `oboInOwl:inSubset=uberon:cumbo` resolves once the recipe
/// has passed `--prefix 'uberon: http://purl.obolibrary.org/obo/uberon/core#'`.
/// The value matches an IRI-valued annotation by IRI and a literal-valued one by
/// its lexical form, since a subset tag is written both ways across OBO.
pub fn annotation_value_members(model: &Model, prop: &str, value: &str) -> HashSet<String> {
    use horned_owl::model::{AnnotationSubject, AnnotationValue, Component};
    let prop_iri = expand(model, prop);
    // A literal value may be written the way a recipe writes it — quoted, with a
    // datatype: UBERON's `composite-vertebrate-basic.owl` is
    // `remove --select owl:deprecated='true'^^xsd:boolean`. Compare on the LEXICAL
    // form, so that spelling and a bare `true` both match the same assertion.
    let lexical = {
        let v = match value.find("^^") {
            Some(i) => &value[..i],
            None => value,
        }
        .trim();
        v.strip_prefix('\'')
            .and_then(|x| x.strip_suffix('\''))
            .or_else(|| v.strip_prefix('"').and_then(|x| x.strip_suffix('"')))
            .unwrap_or(v)
    };
    let want = expand(model, lexical);
    // A selector selects ENTITIES. An IRI that carries the annotation but is no
    // longer declared and appears in no logical axiom is not one, so it is not
    // selected and the axioms about it stay: a `-basic` composite reaches this
    // selector after a `filter --axioms` has dropped every declaration, and its
    // deprecated classes keep their annotations as a bare `rdf:Description`.
    let entities: HashSet<String> = signature_entities(model).all().cloned().collect();
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        let Component::AnnotationAssertion(ax) = &ac.component else { continue };
        if ax.ann.ap.0.as_ref() != prop_iri.as_str() {
            continue;
        }
        let hit = match &ax.ann.av {
            AnnotationValue::IRI(i) => i.as_ref() == want.as_str(),
            AnnotationValue::Literal(l) => {
                let lex = l.literal();
                lex == want.as_str() || lex == value
            }
            AnnotationValue::AnonymousIndividual(_) => false,
        };
        if hit {
            if let AnnotationSubject::IRI(s) = &ax.subject {
                let s = s.to_string();
                if entities.contains(&s) {
                    out.insert(s);
                }
            }
        }
    }
    out
}

/// Minimal glob match supporting `*` (any run) over an entity IRI.
pub fn glob_match(pat: &str, s: &str) -> bool {
    if !pat.contains('*') {
        return pat == s;
    }
    let parts: Vec<&str> = pat.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !s[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if let Some(idx) = s[pos..].find(part) {
            pos += idx + part.len();
        } else {
            return false;
        }
    }
    // A trailing non-`*` part must reach the end.
    if let Some(last) = parts.last() {
        if !last.is_empty() && !pat.ends_with('*') && !s.ends_with(last) {
            return false;
        }
    }
    true
}

/// Declared entities grouped by kind (from `Declaration` axioms).
#[derive(Default)]
pub struct Entities {
    pub classes: HashSet<String>,
    pub object_properties: HashSet<String>,
    pub data_properties: HashSet<String>,
    pub annotation_properties: HashSet<String>,
    pub individuals: HashSet<String>,
    pub datatypes: HashSet<String>,
    /// Datatypes named as a LITERAL's type rather than in an entity position —
    /// `"2010-01-01"^^xsd:date` puts `xsd:date` here. They belong to the signature
    /// (a document that types a literal mentions that datatype) but not to the set
    /// a selector means by "datatypes", so they are kept apart.
    pub literal_datatypes: HashSet<String>,
}

impl Entities {
    /// Every declared entity IRI, across all kinds.
    pub fn all(&self) -> impl Iterator<Item = &String> {
        self.classes
            .iter()
            .chain(&self.object_properties)
            .chain(&self.data_properties)
            .chain(&self.annotation_properties)
            .chain(&self.individuals)
            .chain(&self.datatypes)
    }
}

/// Entities by kind as the OWL *signature* defines them: everything the ontology
/// mentions in an entity position, declared or not.
///
/// `entities` reads `Declaration` axioms only, whereas a signature is collected
/// from axiom structure, so an entity that is merely *referenced* still belongs to
/// it. The difference is not academic: MONDO's `mondo-base.owl` runs
/// `remove --input reasoned.owl --select imports` first, which strips the imports
/// that declared `BFO_0000004`/`BFO_0000050`; the later
/// `remove --select "<BFO_*>" --select classes` must still see `BFO_0000004` as a
/// class and drop the `rdfs:subClassOf` referencing it. Where the only mention of
/// those two is inside a `SubClassOf`, the class reference goes and
/// `Declaration(ObjectProperty(BFO_0000050))` is materialised for the retained
/// property.
pub fn signature_entities(model: &Model) -> Entities {
    use horned_owl::model::{
        AnnotationProperty, Class, DataProperty, Datatype, Literal, NamedIndividual, ObjectProperty,
        RcStr,
    };
    use horned_owl::visitor::immutable::{Visit, Walk};

    #[derive(Default)]
    struct TypedSig {
        e: Entities,
    }
    impl Visit<RcStr> for TypedSig {
        fn visit_class(&mut self, c: &Class<RcStr>) {
            self.e.classes.insert(c.0.as_ref().to_string());
        }
        fn visit_object_property(&mut self, p: &ObjectProperty<RcStr>) {
            self.e.object_properties.insert(p.0.as_ref().to_string());
        }
        fn visit_data_property(&mut self, p: &DataProperty<RcStr>) {
            self.e.data_properties.insert(p.0.as_ref().to_string());
        }
        fn visit_annotation_property(&mut self, p: &AnnotationProperty<RcStr>) {
            self.e.annotation_properties.insert(p.0.as_ref().to_string());
        }
        fn visit_named_individual(&mut self, i: &NamedIndividual<RcStr>) {
            self.e.individuals.insert(i.0.as_ref().to_string());
        }
        fn visit_datatype(&mut self, d: &Datatype<RcStr>) {
            self.e.datatypes.insert(d.0.as_ref().to_string());
        }
        // A typed literal's datatype is reached as a bare IRI, not as a datatype
        // entity, so it is collected here.
        fn visit_literal(&mut self, l: &Literal<RcStr>) {
            if let Literal::Datatype { datatype_iri, .. } = l {
                self.e.literal_datatypes.insert(datatype_iri.as_ref().to_string());
            }
        }
    }

    let mut walk = Walk::new(TypedSig::default());
    for ac in model.ont.iter() {
        // The whole ANNOTATED component: an axiom's signature includes the entities
        // named in its own annotations. In MONDO's extracted import module
        // `oboInOwl:hasDbXref` appears nowhere else — only as an annotation on the
        // definitions — so walking `ac.component` alone would leave it out of the
        // annotation-property signature and `--select complement` could not reach it.
        walk.annotated_component(ac);
    }
    walk.into_visit().e
}

/// Collect declared entities by kind.
pub fn entities(model: &Model) -> Entities {
    use horned_owl::model::Component as C;
    let mut e = Entities::default();
    for ac in model.ont.iter() {
        match &ac.component {
            C::DeclareClass(d) => {
                e.classes.insert(d.0 .0.to_string());
            }
            C::DeclareObjectProperty(d) => {
                e.object_properties.insert(d.0 .0.to_string());
            }
            C::DeclareDataProperty(d) => {
                e.data_properties.insert(d.0 .0.to_string());
            }
            C::DeclareAnnotationProperty(d) => {
                e.annotation_properties.insert(d.0 .0.to_string());
            }
            C::DeclareNamedIndividual(d) => {
                e.individuals.insert(d.0 .0.to_string());
            }
            C::DeclareDatatype(d) => {
                e.datatypes.insert(d.0 .0.to_string());
            }
            _ => {}
        }
    }
    e
}

/// The declared entities of a keyword type selector (`classes`,
/// `object-properties`, …), if it names one. The `properties` selector spans
/// object/data/annotation properties.
pub fn type_set<'a>(ent: &'a Entities, kw: &str) -> Option<&'a HashSet<String>> {
    match kw {
        "classes" => Some(&ent.classes),
        "object-properties" => Some(&ent.object_properties),
        "data-properties" => Some(&ent.data_properties),
        "annotation-properties" => Some(&ent.annotation_properties),
        "individuals" | "named-individuals" => Some(&ent.individuals),
        "datatypes" => Some(&ent.datatypes),
        _ => None,
    }
}

/// The set of entity IRIs belonging to a category selector, expanded to handle
/// the multi-kind `properties` (object ∪ data ∪ annotation) selector. Unlike
/// [`type_set`], this owns the result and covers `properties`.
pub fn category_members(ent: &Entities, kw: &str) -> Option<HashSet<String>> {
    match kw {
        "properties" => {
            let mut s: HashSet<String> = HashSet::new();
            s.extend(ent.object_properties.iter().cloned());
            s.extend(ent.data_properties.iter().cloned());
            s.extend(ent.annotation_properties.iter().cloned());
            Some(s)
        }
        _ => type_set(ent, kw).map(|s| s.clone()),
    }
}

/// Direct named superclasses of the classes in `seed`, plus direct
/// super-properties of the properties in `seed` (`--select parents`).
pub fn direct_parents(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    use horned_owl::model::{ClassExpression as CE, Component as C, ObjectPropertyExpression as OPE,
        SubObjectPropertyExpression as SOPE};
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        match &ac.component {
            C::SubClassOf(sc) => {
                if let (CE::Class(sub), CE::Class(sup)) = (&sc.sub, &sc.sup) {
                    if seed.contains(&sub.0.to_string()) {
                        out.insert(sup.0.to_string());
                    }
                }
            }
            C::SubObjectPropertyOf(sp) => {
                if let (SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)), OPE::ObjectProperty(sup)) =
                    (&sp.sub, &sp.sup)
                {
                    if seed.contains(&sub.0.to_string()) {
                        out.insert(sup.0.to_string());
                    }
                }
            }
            C::SubDataPropertyOf(sp) => {
                if seed.contains(&sp.sub.0.to_string()) {
                    out.insert(sp.sup.0.to_string());
                }
            }
            C::SubAnnotationPropertyOf(sp) => {
                if seed.contains(&sp.sub.0.to_string()) {
                    out.insert(sp.sup.0.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// Direct named subclasses of the classes in `seed`, plus direct sub-properties
/// of the properties in `seed` (`--select children`).
pub fn direct_children(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    use horned_owl::model::{ClassExpression as CE, Component as C, ObjectPropertyExpression as OPE,
        SubObjectPropertyExpression as SOPE};
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        match &ac.component {
            C::SubClassOf(sc) => {
                if let (CE::Class(sub), CE::Class(sup)) = (&sc.sub, &sc.sup) {
                    if seed.contains(&sup.0.to_string()) {
                        out.insert(sub.0.to_string());
                    }
                }
            }
            C::SubObjectPropertyOf(sp) => {
                if let (SOPE::ObjectPropertyExpression(OPE::ObjectProperty(sub)), OPE::ObjectProperty(sup)) =
                    (&sp.sub, &sp.sup)
                {
                    if seed.contains(&sup.0.to_string()) {
                        out.insert(sub.0.to_string());
                    }
                }
            }
            C::SubDataPropertyOf(sp) => {
                if seed.contains(&sp.sup.0.to_string()) {
                    out.insert(sp.sub.0.to_string());
                }
            }
            C::SubAnnotationPropertyOf(sp) => {
                if seed.contains(&sp.sup.0.to_string()) {
                    out.insert(sp.sub.0.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// Transitive closure of a one-step expansion `step` starting from `seed`,
/// returning only the *newly reached* terms (not the seed itself). Loop-safe: a
/// term already reached is never expanded again, so a cycle terminates.
fn closure<F>(seed: &HashSet<String>, step: F) -> HashSet<String>
where
    F: Fn(&HashSet<String>) -> HashSet<String>,
{
    let mut acc: HashSet<String> = HashSet::new();
    let mut frontier = seed.clone();
    loop {
        let next = step(&frontier);
        let new: HashSet<String> =
            next.into_iter().filter(|n| !acc.contains(n) && !seed.contains(n)).collect();
        if new.is_empty() {
            break;
        }
        acc.extend(new.iter().cloned());
        frontier = new;
    }
    acc
}

/// All ancestors (transitive superclasses/super-properties) of `seed`.
pub fn ancestors(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    closure(seed, |f| direct_parents(model, f))
}

/// All descendants (transitive subclasses/sub-properties) of `seed`.
pub fn descendants(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    closure(seed, |f| direct_children(model, f))
}

/// Named classes/properties asserted equivalent to any member of `seed`
/// (`--select equivalents`).
pub fn equivalents_of(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    use horned_owl::model::{ClassExpression as CE, Component as C, ObjectPropertyExpression as OPE};
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        match &ac.component {
            C::EquivalentClasses(eq) => {
                let named: Vec<String> = eq
                    .0
                    .iter()
                    .filter_map(|m| match m {
                        CE::Class(c) => Some(c.0.to_string()),
                        _ => None,
                    })
                    .collect();
                if named.iter().any(|n| seed.contains(n)) {
                    out.extend(named);
                }
            }
            C::EquivalentObjectProperties(eq) => {
                let named: Vec<String> = eq
                    .0
                    .iter()
                    .filter_map(|m| match m {
                        OPE::ObjectProperty(p) => Some(p.0.to_string()),
                        _ => None,
                    })
                    .collect();
                if named.iter().any(|n| seed.contains(n)) {
                    out.extend(named);
                }
            }
            C::EquivalentDataProperties(eq) => {
                let named: Vec<String> = eq.0.iter().map(|p| p.0.to_string()).collect();
                if named.iter().any(|n| seed.contains(n)) {
                    out.extend(named);
                }
            }
            _ => {}
        }
    }
    // Don't re-add the seed itself.
    out.retain(|n| !seed.contains(n));
    out
}

/// For individuals in `seed`, their asserted named class types
/// (`--select types`).
pub fn types_of(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    use horned_owl::model::{ClassExpression as CE, Component as C, Individual};
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if let C::ClassAssertion(ca) = &ac.component {
            let ind = match &ca.i {
                Individual::Named(n) => n.0.to_string(),
                Individual::Anonymous(_) => continue,
            };
            if seed.contains(&ind) {
                if let CE::Class(c) = &ca.ce {
                    out.insert(c.0.to_string());
                }
            }
        }
    }
    out
}

/// For classes in `seed`, the named individuals asserted to be instances of them
/// (`--select instances`).
pub fn instances_of(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    use horned_owl::model::{ClassExpression as CE, Component as C, Individual};
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if let C::ClassAssertion(ca) = &ac.component {
            if let CE::Class(c) = &ca.ce {
                if seed.contains(&c.0.to_string()) {
                    if let Individual::Named(n) = &ca.i {
                        out.insert(n.0.to_string());
                    }
                }
            }
        }
    }
    out
}

/// For properties in `seed`, the named classes used as their domains
/// (`--select domains`).
pub fn domains_of(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    use horned_owl::model::{ClassExpression as CE, Component as C, ObjectPropertyExpression as OPE};
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        match &ac.component {
            C::ObjectPropertyDomain(d) => {
                let p = match &d.ope {
                    OPE::ObjectProperty(p) => p.0.to_string(),
                    OPE::InverseObjectProperty(p) => p.0.to_string(),
                };
                if seed.contains(&p) {
                    if let CE::Class(c) = &d.ce {
                        out.insert(c.0.to_string());
                    }
                }
            }
            C::DataPropertyDomain(d) => {
                if seed.contains(&d.dp.0.to_string()) {
                    if let CE::Class(c) = &d.ce {
                        out.insert(c.0.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// For properties in `seed`, the named classes used as their ranges
/// (`--select ranges`). Data-property ranges are datatypes (not part of
/// the class/entity seed) so only object-property ranges contribute named
/// classes.
pub fn ranges_of(model: &Model, seed: &HashSet<String>) -> HashSet<String> {
    use horned_owl::model::{ClassExpression as CE, Component as C, ObjectPropertyExpression as OPE};
    let mut out = HashSet::new();
    for ac in model.ont.iter() {
        if let C::ObjectPropertyRange(r) = &ac.component {
            let p = match &r.ope {
                OPE::ObjectProperty(p) => p.0.to_string(),
                OPE::InverseObjectProperty(p) => p.0.to_string(),
            };
            if seed.contains(&p) {
                if let CE::Class(c) = &r.ce {
                    out.insert(c.0.to_string());
                }
            }
        }
    }
    out
}

type Rc = horned_owl::model::RcStr;

/// Any of the OWL entity declaration components.
pub fn is_declaration(comp: &horned_owl::model::Component<Rc>) -> bool {
    use horned_owl::model::Component as C;
    matches!(
        comp,
        C::DeclareClass(_)
            | C::DeclareObjectProperty(_)
            | C::DeclareDataProperty(_)
            | C::DeclareAnnotationProperty(_)
            | C::DeclareNamedIndividual(_)
            | C::DeclareDatatype(_)
    )
}

/// A logical (non-annotation, non-declaration, non-ontology) axiom.
pub fn is_logical(comp: &horned_owl::model::Component<Rc>) -> bool {
    use horned_owl::model::Component as C;
    !matches!(
        comp,
        C::AnnotationAssertion(_)
            | C::OntologyAnnotation(_)
            | C::OntologyID(_)
            | C::DocIRI(_)
            | C::Import(_)
    ) && !is_declaration(comp)
}

/// The IRIs `comp` is about. `--axioms internal` selects an axiom when one of
/// them lies in a base namespace, and `--axioms external` every other axiom.
///
/// A declaration is about its entity; an assertion about its named individual or
/// annotation subject; a sub-class or sub-property axiom about its sub-class or
/// sub-property; a property characteristic, domain or range about the property;
/// and an inverse pair about its first property. An n-ary class or property
/// axiom is about each of its members, save that an equivalence of classes counts
/// its named members alone and an equivalence of properties needs two members to
/// be about any. Where that subject is an anonymous expression, each entity in it
/// is a subject: a property chain is about its links, and
/// `ObjectSomeValuesFrom(p C) ⊑ D` about `p` and `C`.
///
/// An assertion about an anonymous individual is about no IRI, and nor is an
/// axiom with no subject to give: a key, a disjoint union, a datatype definition,
/// sameness or difference of individuals, a negative assertion, a functional data
/// property, an annotation property's domain or range, and a rule. Each is
/// therefore external to any namespace.
pub fn axiom_subjects(comp: &horned_owl::model::Component<Rc>) -> Vec<String> {
    use horned_owl::model::{
        AnnotationSubject, ClassExpression as CE, Component as C, Individual,
        ObjectPropertyExpression as OPE, SubObjectPropertyExpression as SOPE,
    };
    let ope = |o: &OPE<Rc>| match o {
        OPE::ObjectProperty(p) | OPE::InverseObjectProperty(p) => p.0.to_string(),
    };
    let ce = |c: &CE<Rc>| match c {
        CE::Class(cl) => vec![cl.0.to_string()],
        _ => crate::sig::class_expression_signature(c).into_iter().collect(),
    };
    let named = |i: &Individual<Rc>| match i {
        Individual::Named(n) => vec![n.0.to_string()],
        Individual::Anonymous(_) => Vec::new(),
    };
    // An equivalence of properties is about the sub-property of each pairwise
    // inclusion it implies, which one member alone does not give.
    let pairwise = |members: Vec<String>| if members.len() < 2 { Vec::new() } else { members };
    match comp {
        C::DeclareClass(d) => vec![d.0.0.to_string()],
        C::DeclareObjectProperty(d) => vec![d.0.0.to_string()],
        C::DeclareAnnotationProperty(d) => vec![d.0.0.to_string()],
        C::DeclareDataProperty(d) => vec![d.0.0.to_string()],
        C::DeclareNamedIndividual(d) => vec![d.0.0.to_string()],
        C::DeclareDatatype(d) => vec![d.0.0.to_string()],
        C::SubClassOf(ax) => ce(&ax.sub),
        C::EquivalentClasses(ax) => ax
            .0
            .iter()
            .filter_map(|c| match c {
                CE::Class(cl) => Some(cl.0.to_string()),
                _ => None,
            })
            .collect(),
        C::DisjointClasses(ax) => ax.0.iter().flat_map(ce).collect(),
        C::SubObjectPropertyOf(ax) => match &ax.sub {
            SOPE::ObjectPropertyExpression(o) => vec![ope(o)],
            SOPE::ObjectPropertyChain(chain) => chain.iter().map(ope).collect(),
        },
        C::SubDataPropertyOf(ax) => vec![ax.sub.0.to_string()],
        C::SubAnnotationPropertyOf(ax) => vec![ax.sub.0.to_string()],
        C::EquivalentObjectProperties(ax) => pairwise(ax.0.iter().map(ope).collect()),
        C::EquivalentDataProperties(ax) => pairwise(ax.0.iter().map(|p| p.0.to_string()).collect()),
        C::DisjointObjectProperties(ax) => ax.0.iter().map(ope).collect(),
        C::DisjointDataProperties(ax) => ax.0.iter().map(|p| p.0.to_string()).collect(),
        C::AnnotationAssertion(ax) => match &ax.subject {
            AnnotationSubject::IRI(i) => vec![i.to_string()],
            AnnotationSubject::AnonymousIndividual(_) => Vec::new(),
        },
        C::ClassAssertion(ax) => named(&ax.i),
        C::ObjectPropertyAssertion(ax) => named(&ax.from),
        C::DataPropertyAssertion(ax) => named(&ax.from),
        C::FunctionalObjectProperty(ax) => vec![ope(&ax.0)],
        C::InverseFunctionalObjectProperty(ax) => vec![ope(&ax.0)],
        C::ReflexiveObjectProperty(ax) => vec![ope(&ax.0)],
        C::IrreflexiveObjectProperty(ax) => vec![ope(&ax.0)],
        C::SymmetricObjectProperty(ax) => vec![ope(&ax.0)],
        C::AsymmetricObjectProperty(ax) => vec![ope(&ax.0)],
        C::TransitiveObjectProperty(ax) => vec![ope(&ax.0)],
        C::ObjectPropertyDomain(ax) => vec![ope(&ax.ope)],
        C::ObjectPropertyRange(ax) => vec![ope(&ax.ope)],
        C::DataPropertyDomain(ax) => vec![ax.dp.0.to_string()],
        C::DataPropertyRange(ax) => vec![ax.dp.0.to_string()],
        C::InverseObjectProperties(ax) => vec![ope(&ax.0)],
        _ => Vec::new(),
    }
}

/// Whether `comp` is an axiom internal to `base_iris`: one of its subjects
/// ([`axiom_subjects`]) lies in one of those namespaces.
fn is_internal(comp: &horned_owl::model::Component<Rc>, base_iris: &[String]) -> bool {
    axiom_subjects(comp)
        .iter()
        .any(|iri| base_iris.iter().any(|b| iri.starts_with(b.as_str())))
}

/// Whether `comp` is an axiom at all. The ontology's IRIs, its annotations and
/// its imports belong to the ontology rather than stating anything in it, so no
/// axiom selector reaches them. MONDO's `remove --base-iri …/MFOMD --axioms
/// external` keeps mfomd's four imports that way, and the merge over the mirrors
/// follows them to the terms its import module needs.
fn is_axiom(comp: &horned_owl::model::Component<Rc>) -> bool {
    use horned_owl::model::Component as C;
    !matches!(comp, C::OntologyID(_) | C::DocIRI(_) | C::OntologyAnnotation(_) | C::Import(_))
}

/// The namespace selectors among the `--axioms` values that take effect. An
/// `internal` named after an `external` is ignored, with a warning; an
/// `external` named after an `internal` applies with it, and the two together
/// select every axiom.
pub fn namespace_selectors(toks: &[String]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for t in toks {
        match t.as_str() {
            "internal" if out.contains(&"external") => status!(
                "warning: ignoring the 'internal' axiom selector named after 'external': the two together would select every axiom"
            ),
            "internal" if !out.contains(&"internal") => out.push("internal"),
            "external" if !out.contains(&"external") => {
                if out.contains(&"internal") {
                    status!("warning: 'internal' and 'external' together select every axiom");
                }
                out.push("external");
            }
            _ => {}
        }
    }
    out
}

/// Axiom-type classification: does `comp` belong to the named category?
/// Covers `all`, `logical`, `annotation`, `subclass`, `subproperty`,
/// `equivalent`, `disjoint`, `type`, `tbox`, `abox`, `rbox`, `declaration`,
/// `internal`, `external`. `base_iris` defines the internal namespace(s).
pub fn axiom_in_category(
    comp: &horned_owl::model::Component<Rc>,
    cat: &str,
    base_iris: &[String],
) -> bool {
    use horned_owl::model::Component as C;
    match cat {
        "all" => true,
        "logical" => is_logical(comp),
        // The annotation-axiom family, not assertions alone: sub-annotation-
        // property axioms and annotation-property domains/ranges belong to it,
        // and a filter keeping "annotation" keeps a subsetdef's
        // `⊑ oboInOwl:SubsetProperty` — which is what keeps the subset VALUES
        // declared, so a later label-keeper can remove the `inSubset`
        // assertions pointing at them (12,928 `subset:` lines of the `-basic`
        // composites turned on exactly this).
        "annotation" => matches!(
            comp,
            C::AnnotationAssertion(_)
                | C::SubAnnotationPropertyOf(_)
                | C::AnnotationPropertyDomain(_)
                | C::AnnotationPropertyRange(_)
        ),
        "declaration" => is_declaration(comp),
        // `subclass` is the whole SUBSUMPTION family, not just class subsumption:
        // a filter keeping `subclass` keeps `SubObjectPropertyOf`,
        // `SubDataPropertyOf` and `SubAnnotationPropertyOf` with it. The `-basic`
        // composites turn on this — their `filter --axioms "subclass equivalent
        // annotation"` is what carries the property hierarchy (1,098 lines) into
        // the result, and with it the declarations that let the NEXT step's
        // object-property complement remove the unused properties outright.
        // The subsumption family: one thing is under another. A property CHAIN
        // says a composition implies a property, not that one property is under
        // another, so it is not in it.
        "subclass" => match comp {
            C::SubClassOf(_) | C::SubDataPropertyOf(_) | C::SubAnnotationPropertyOf(_) => true,
            C::SubObjectPropertyOf(ax) => !matches!(
                ax.sub,
                horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(_)
            ),
            _ => false,
        },
        "subproperty" => matches!(
            comp,
            C::SubObjectPropertyOf(_) | C::SubDataPropertyOf(_) | C::SubAnnotationPropertyOf(_)
        ),
        "equivalent" => matches!(
            comp,
            C::EquivalentClasses(_)
                | C::EquivalentObjectProperties(_)
                | C::EquivalentDataProperties(_)
        ),
        "disjoint" => matches!(
            comp,
            C::DisjointClasses(_)
                | C::DisjointObjectProperties(_)
                | C::DisjointDataProperties(_)
                | C::DisjointUnion(_)
        ),
        "type" => matches!(comp, C::ClassAssertion(_)),
        // `tbox`/`abox`/`rbox` group axioms by axiom TYPE, and that grouping is not
        // the conceptual partition the names suggest: the property *characteristic*
        // axioms (`FunctionalObjectProperty`, the domain/range axioms) sit in TBox,
        // `SubObjectPropertyOf` — chain form included — sits in RBox, and
        // DECLARATIONS are in none of the three. uPheno's merged mirror is where the
        // difference shows: `remove --term RO:0000052 --term RO:0002314 --axioms
        // tbox` must take `FunctionalObjectProperty(RO_0000052)`, which a class-level
        // reading of "tbox" leaves behind.
        "tbox" => matches!(
            comp,
            C::SubClassOf(_)
                | C::EquivalentClasses(_)
                | C::DisjointClasses(_)
                | C::ObjectPropertyDomain(_)
                | C::ObjectPropertyRange(_)
                | C::FunctionalObjectProperty(_)
                | C::InverseFunctionalObjectProperty(_)
                | C::DataPropertyDomain(_)
                | C::DataPropertyRange(_)
                | C::FunctionalDataProperty(_)
                | C::DatatypeDefinition(_)
                | C::DisjointUnion(_)
                | C::HasKey(_)
        ),
        "abox" => matches!(
            comp,
            C::ClassAssertion(_)
                | C::SameIndividual(_)
                | C::DifferentIndividuals(_)
                | C::ObjectPropertyAssertion(_)
                | C::NegativeObjectPropertyAssertion(_)
                | C::DataPropertyAssertion(_)
                | C::NegativeDataPropertyAssertion(_)
        ),
        "rbox" => matches!(
            comp,
            C::TransitiveObjectProperty(_)
                | C::DisjointDataProperties(_)
                | C::SubDataPropertyOf(_)
                | C::EquivalentDataProperties(_)
                | C::DisjointObjectProperties(_)
                | C::SubObjectPropertyOf(_)
                | C::EquivalentObjectProperties(_)
                | C::InverseObjectProperties(_)
                | C::SymmetricObjectProperty(_)
                | C::AsymmetricObjectProperty(_)
                | C::ReflexiveObjectProperty(_)
                | C::IrreflexiveObjectProperty(_)
        ),
        // Each axiom is internal or external to the base namespaces by its
        // subjects (see `axiom_subjects`). With no base IRIs, every axiom is
        // external.
        "internal" => is_axiom(comp) && is_internal(comp, base_iris),
        "external" => is_axiom(comp) && !is_internal(comp, base_iris),
        // A single axiom type, named the way the OWL object model names it.
        // uPheno's `upheno-old-model.owl` asks for ten of these one per step
        // (`--axioms FunctionalObjectProperty`, `--axioms DisjointDataProperties`,
        // …) where the grouping categories above would take too much.
        other => axiom_type_matches(comp, other),
    }
}

/// Does `comp` have the single axiom type spelled `name`?
///
/// The names are the object model's own, which is why `IrrefexiveObjectProperty`
/// is spelled without its second `l` and an annotation-property range is
/// `AnnotationPropertyRangeOf`. They are matched exactly: a near miss is not an
/// axiom type, and [`is_axiom_category`] reports it as one owlmake cannot run.
///
/// `Declaration` is one type covering all six entity kinds. `SubObjectPropertyOf`
/// and `SubPropertyChainOf` are two types over one component: a property chain is
/// the latter and never the former.
fn axiom_type_matches(comp: &horned_owl::model::Component<Rc>, name: &str) -> bool {
    use horned_owl::model::Component as C;
    use horned_owl::model::SubObjectPropertyExpression as SubOPE;
    match name {
        "Declaration" => is_declaration(comp),
        "EquivalentClasses" => matches!(comp, C::EquivalentClasses(_)),
        "SubClassOf" => matches!(comp, C::SubClassOf(_)),
        "DisjointClasses" => matches!(comp, C::DisjointClasses(_)),
        "DisjointUnion" => matches!(comp, C::DisjointUnion(_)),
        "ClassAssertion" => matches!(comp, C::ClassAssertion(_)),
        "SameIndividual" => matches!(comp, C::SameIndividual(_)),
        "DifferentIndividuals" => matches!(comp, C::DifferentIndividuals(_)),
        "ObjectPropertyAssertion" => matches!(comp, C::ObjectPropertyAssertion(_)),
        "NegativeObjectPropertyAssertion" => {
            matches!(comp, C::NegativeObjectPropertyAssertion(_))
        }
        "DataPropertyAssertion" => matches!(comp, C::DataPropertyAssertion(_)),
        "NegativeDataPropertyAssertion" => matches!(comp, C::NegativeDataPropertyAssertion(_)),
        "EquivalentObjectProperties" => matches!(comp, C::EquivalentObjectProperties(_)),
        "SubObjectPropertyOf" => match comp {
            C::SubObjectPropertyOf(ax) => !matches!(ax.sub, SubOPE::ObjectPropertyChain(_)),
            _ => false,
        },
        "SubPropertyChainOf" => match comp {
            C::SubObjectPropertyOf(ax) => matches!(ax.sub, SubOPE::ObjectPropertyChain(_)),
            _ => false,
        },
        "InverseObjectProperties" => matches!(comp, C::InverseObjectProperties(..)),
        "FunctionalObjectProperty" => matches!(comp, C::FunctionalObjectProperty(_)),
        "InverseFunctionalObjectProperty" => {
            matches!(comp, C::InverseFunctionalObjectProperty(_))
        }
        "SymmetricObjectProperty" => matches!(comp, C::SymmetricObjectProperty(_)),
        "AsymmetricObjectProperty" => matches!(comp, C::AsymmetricObjectProperty(_)),
        "TransitiveObjectProperty" => matches!(comp, C::TransitiveObjectProperty(_)),
        "ReflexiveObjectProperty" => matches!(comp, C::ReflexiveObjectProperty(_)),
        "IrrefexiveObjectProperty" => matches!(comp, C::IrreflexiveObjectProperty(_)),
        "ObjectPropertyDomain" => matches!(comp, C::ObjectPropertyDomain { .. }),
        "ObjectPropertyRange" => matches!(comp, C::ObjectPropertyRange { .. }),
        "DisjointObjectProperties" => matches!(comp, C::DisjointObjectProperties(_)),
        "EquivalentDataProperties" => matches!(comp, C::EquivalentDataProperties(_)),
        "SubDataPropertyOf" => matches!(comp, C::SubDataPropertyOf { .. }),
        "FunctionalDataProperty" => matches!(comp, C::FunctionalDataProperty(_)),
        "DataPropertyDomain" => matches!(comp, C::DataPropertyDomain { .. }),
        "DataPropertyRange" => matches!(comp, C::DataPropertyRange { .. }),
        "DisjointDataProperties" => matches!(comp, C::DisjointDataProperties(_)),
        "DatatypeDefinition" => matches!(comp, C::DatatypeDefinition { .. }),
        "HasKey" => matches!(comp, C::HasKey { .. }),
        "Rule" => matches!(comp, C::Rule(_)),
        "AnnotationAssertion" => matches!(comp, C::AnnotationAssertion(_)),
        "SubAnnotationPropertyOf" => matches!(comp, C::SubAnnotationPropertyOf { .. }),
        "AnnotationPropertyRangeOf" => matches!(comp, C::AnnotationPropertyRange { .. }),
        "AnnotationPropertyDomain" => matches!(comp, C::AnnotationPropertyDomain { .. }),
        _ => false,
    }
}

/// Every value `--axioms` accepts: the grouping categories, the two selectors
/// that are namespace tests rather than type tests, `structural-tautologies`,
/// and each single axiom type by its object-model name.
///
/// One list serves the classifier and the plan's coverage check, so a category
/// cannot be executable but reported as a gap, or the reverse.
pub fn is_axiom_category(name: &str) -> bool {
    matches!(
        name,
        "all" | "logical" | "annotation" | "declaration" | "subclass" | "subproperty"
            | "equivalent" | "disjoint" | "type" | "tbox" | "abox" | "rbox"
            | "internal" | "external" | "structural-tautologies"
    ) || AXIOM_TYPE_NAMES.contains(&name)
}

/// The axiom-type names, exactly as the OWL object model spells them.
const AXIOM_TYPE_NAMES: &[&str] = &[
    "Declaration",
    "EquivalentClasses",
    "SubClassOf",
    "DisjointClasses",
    "DisjointUnion",
    "ClassAssertion",
    "SameIndividual",
    "DifferentIndividuals",
    "ObjectPropertyAssertion",
    "NegativeObjectPropertyAssertion",
    "DataPropertyAssertion",
    "NegativeDataPropertyAssertion",
    "EquivalentObjectProperties",
    "SubObjectPropertyOf",
    "InverseObjectProperties",
    "FunctionalObjectProperty",
    "InverseFunctionalObjectProperty",
    "SymmetricObjectProperty",
    "AsymmetricObjectProperty",
    "TransitiveObjectProperty",
    "ReflexiveObjectProperty",
    "IrrefexiveObjectProperty",
    "ObjectPropertyDomain",
    "ObjectPropertyRange",
    "DisjointObjectProperties",
    "SubPropertyChainOf",
    "EquivalentDataProperties",
    "SubDataPropertyOf",
    "FunctionalDataProperty",
    "DataPropertyDomain",
    "DataPropertyRange",
    "DisjointDataProperties",
    "DatatypeDefinition",
    "HasKey",
    "Rule",
    "AnnotationAssertion",
    "SubAnnotationPropertyOf",
    "AnnotationPropertyRangeOf",
    "AnnotationPropertyDomain",
];

/// Rebuild a model keeping only components for which `keep` returns true,
/// preserving the prefix map.
pub fn retain<F>(model: Model, keep: F) -> Model
where
    F: Fn(&horned_owl::model::Component<horned_owl::model::RcStr>) -> bool,
{
    retain_ac(model, |ac| keep(&ac.component))
}

/// [`retain`] with the whole annotated component in hand — for predicates that
/// must see an axiom's own annotations, which belong to its signature.
pub fn retain_ac<F>(model: Model, keep: F) -> Model
where
    F: Fn(&horned_owl::model::AnnotatedComponent<horned_owl::model::RcStr>) -> bool,
{
    use horned_owl::model::MutableOntology;
    use horned_owl::ontology::set::SetOntology;
    let mut ont = SetOntology::new();
    for ac in model.ont.iter() {
        if keep(ac) {
            ont.insert(ac.clone());
        }
    }
    // Preserve document-level metadata that isn't derivable from the axioms — as
    // Model::clone does — so a filtered model still carries e.g. `rdf_prefixes`
    // (the verbatim RDF/XML xmlns the owlrdf writer reproduces). Without this, a
    // `remove`/`filter` hop silently drops the input's prefix map.
    //
    // `carry_meta_from` is the one canonical copier, and copying a hand-kept field
    // list here instead is not equivalent: any field the list omits is silently
    // reverted by the hop. Losing `plain_literals_typed` and the shared-blank-node
    // state a preceding `query --update` established, for instance, un-types the
    // literals again and reorders every subject that mixes a plain and a typed
    // literal.
    let mut out = Model::from_parts(ont, crate::model::clone_prefixes(&model.prefixes));
    out.carry_meta_from(&model);
    out
}

#[cfg(test)]
mod namespace_tests {
    use super::*;
    use horned_owl::model::{
        Build, Component, ObjectProperty, ObjectPropertyExpression as OPE, RcStr,
        SubObjectPropertyExpression as SOPE, SubObjectPropertyOf,
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
        assert!(axiom_in_category(&c, "external", &base), "chain members are all foreign, so the axiom is external");
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
        assert!(!axiom_in_category(&c, "external", &base), "an internal chain member keeps the axiom");
    }
}
