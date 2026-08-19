//! OWL 2 profile validation (EL / QL / RL / DL).
//!
//! For each axiom we check whether its class/property expressions stay within
//! the syntactic restrictions of the chosen profile, and report the offending
//! axioms. EL is implemented precisely; QL and RL cover their characteristic
//! restrictions.
//!
//! DL is the profile an ontology release is normally validated against, and it
//! enforces four global restrictions:
//!
//! 1. **Typing** — every IRI used as a class / object property / data property /
//!    annotation property / datatype must be *declared* as one. This is why a
//!    release has to merge its imports before it is validated: an unmerged import
//!    leaves the terms it contributes undeclared, and the ontology drops out of
//!    OWL 2 DL.
//! 2. **Punning** — the class and datatype IRIs must be disjoint, and the object
//!    / data / annotation property IRIs pairwise disjoint.
//! 3. **Simple roles** — non-simple object properties may not appear in number
//!    restrictions, `ObjectHasSelf`, or functional / inverse-functional /
//!    irreflexive / asymmetric / disjoint property axioms.
//! 4. **Regularity** — the property-chain axioms must admit a strict partial
//!    order on the properties they mention.

use std::collections::{HashMap, HashSet};

use horned_owl::model::{
    AnnotationProperty, Class, ClassExpression as CE, Component, DataProperty, Datatype, Kinded,
    ObjectProperty, ObjectPropertyExpression as OPE, RcStr, SubObjectPropertyExpression as SOPE,
};
use horned_owl::visitor::immutable::{Visit, Walk};

use crate::model::Model;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
    El,
    Ql,
    Rl,
    Dl,
}

impl Profile {
    pub fn parse(s: &str) -> Option<Profile> {
        match s.to_ascii_uppercase().as_str() {
            "EL" => Some(Profile::El),
            "QL" => Some(Profile::Ql),
            "RL" => Some(Profile::Rl),
            "DL" => Some(Profile::Dl),
            _ => None,
        }
    }

    /// The profile's display name, used verbatim as the heading of the report
    /// `om validate-profile` writes (`"OWL 2 DL Profile Report: …"`). That report
    /// is what a curator reads when the build fails, so the name is the spelling
    /// the OWL 2 spec gives the profile.
    pub fn name(self) -> &'static str {
        match self {
            Profile::El => "OWL 2 EL",
            Profile::Ql => "OWL 2 QL",
            Profile::Rl => "OWL 2 RL",
            Profile::Dl => "OWL 2 DL",
        }
    }
}

/// The axiom's kind as a bare name (`SubClassOf`), not horned-owl's
/// `ComponentKind::SubClassOf` Debug spelling. The profile report is read by
/// curators — the build prints it straight into the log — so it carries the
/// OWL axiom name they know.
fn kind_label(comp: &Component<RcStr>) -> String {
    let d = format!("{:?}", comp.kind());
    d.rsplit("::").next().unwrap_or(d.as_str()).to_string()
}

/// A single profile violation.
pub struct Violation {
    pub axiom_kind: String,
    pub reason: String,
}

/// Validate `model` against `profile`, returning the list of violations (empty
/// means the ontology is in the profile).
pub fn validate(model: &Model, profile: Profile) -> Vec<Violation> {
    // DL needs a global pass first (which object properties are non-simple) before
    // the per-axiom check, so handle it separately.
    if profile == Profile::Dl {
        return validate_dl(model);
    }
    let mut out = Vec::new();
    for ac in model.ont.iter() {
        if let Some(reason) = axiom_violation(&ac.component, profile) {
            out.push(Violation {
                axiom_kind: kind_label(&ac.component),
                reason,
            });
        }
    }
    out
}

fn axiom_violation(comp: &Component<RcStr>, profile: Profile) -> Option<String> {
    match profile {
        Profile::El => el_axiom_violation(comp),
        Profile::Ql => ql_axiom_violation(comp),
        Profile::Rl => rl_axiom_violation(comp),
        Profile::Dl => None, // handled by validate_dl
    }
}

// --- DL (global simple-role restriction) ---------------------------------

/// Base IRI of an object-property expression (ignoring inverse direction).
fn ope_iri(ope: &OPE<RcStr>) -> &str {
    match ope {
        OPE::ObjectProperty(p) => p.0.as_ref(),
        OPE::InverseObjectProperty(p) => p.0.as_ref(),
    }
}

/// The set of *non-simple* object properties: transitive properties, the
/// super-property of any property chain, and anything above a non-simple property
/// in the sub-property hierarchy (closed to a fixpoint). Using non-simple roles in
/// number restrictions, `ObjectHasSelf`, functional / irreflexive / asymmetric /
/// disjoint property axioms makes an ontology fall outside OWL 2 DL.
fn non_simple_roles(model: &Model) -> HashSet<String> {
    let mut non_simple: HashSet<String> = HashSet::new();
    let mut sub_edges: Vec<(String, String)> = Vec::new();
    // `InverseObjectProperties(p q)` makes p and q *inverses of one another*, and
    // OWL 2 defines simplicity over an object property expression together with
    // its inverse: if `q ≡ inv(p)` and p is non-simple then q is non-simple too.
    // Without that, a non-simple role slips into a cardinality restriction through
    // its named inverse: the ontology is OWL 2 Full and the check says nothing.
    // Recorded as an undirected edge, propagated to a fixpoint below.
    let mut inverse_edges: Vec<(String, String)> = Vec::new();
    for ac in model.ont.iter() {
        match &ac.component {
            Component::TransitiveObjectProperty(ax) => {
                non_simple.insert(ope_iri(&ax.0).to_string());
            }
            Component::SubObjectPropertyOf(ax) => match &ax.sub {
                SOPE::ObjectPropertyChain(_) => {
                    non_simple.insert(ope_iri(&ax.sup).to_string());
                }
                SOPE::ObjectPropertyExpression(sub) => {
                    sub_edges.push((ope_iri(sub).to_string(), ope_iri(&ax.sup).to_string()));
                }
            },
            Component::InverseObjectProperties(ax) => {
                inverse_edges.push((ope_iri(&ax.0).to_string(), ope_iri(&ax.1).to_string()));
            }
            _ => {}
        }
    }
    // Propagate: if a sub-property is non-simple, its super-properties are too;
    // and a property is non-simple whenever its declared inverse is.
    let mut changed = true;
    while changed {
        changed = false;
        for (sub, sup) in &sub_edges {
            if non_simple.contains(sub) && non_simple.insert(sup.clone()) {
                changed = true;
            }
        }
        for (p, q) in &inverse_edges {
            if non_simple.contains(p) && non_simple.insert(q.clone()) {
                changed = true;
            }
            if non_simple.contains(q) && non_simple.insert(p.clone()) {
                changed = true;
            }
        }
    }
    non_simple
}

fn validate_dl(model: &Model) -> Vec<Violation> {
    let non_simple = non_simple_roles(model);
    let mut out = Vec::new();
    // Declarations first — the cheapest check, and the one an unmerged import
    // trips. Both undeclared-entity and punning violations are read off the same
    // single signature walk.
    let sig = Signature::collect(model);
    sig.undeclared_violations(&mut out);
    sig.punning_violations(&mut out);
    let mut push = |comp: &Component<RcStr>, reason: String| {
        out.push(Violation {
            axiom_kind: kind_label(comp),
            reason,
        });
    };
    for ac in model.ont.iter() {
        let comp = &ac.component;
        // Property axioms that require a simple property.
        let simple_axiom: Option<Vec<&str>> = match comp {
            Component::FunctionalObjectProperty(ax) => Some(vec![ope_iri(&ax.0)]),
            Component::InverseFunctionalObjectProperty(ax) => Some(vec![ope_iri(&ax.0)]),
            Component::IrreflexiveObjectProperty(ax) => Some(vec![ope_iri(&ax.0)]),
            Component::AsymmetricObjectProperty(ax) => Some(vec![ope_iri(&ax.0)]),
            Component::DisjointObjectProperties(ax) => Some(ax.0.iter().map(ope_iri).collect()),
            _ => None,
        };
        if let Some(props) = simple_axiom {
            if let Some(p) = props.into_iter().find(|p| non_simple.contains(*p)) {
                push(
                    comp,
                    format!("non-simple object property <{p}> used where OWL 2 DL requires a simple property"),
                );
                continue;
            }
        }
        // Class expressions: number restrictions and ObjectHasSelf must be simple.
        if let Some(reason) = component_ces(comp)
            .into_iter()
            .find_map(|ce| dl_ce(ce, &non_simple))
        {
            push(comp, reason);
        }
    }
    chain_regularity_violations(model, &mut out);
    out
}

// --- DL: typing (declarations) and punning --------------------------------

/// The four namespaces whose IRIs OWL 2 predeclares. Anything under them is
/// built-in vocabulary (`owl:Thing`, `owl:topObjectProperty`, `rdfs:label`,
/// `xsd:string`, `rdf:PlainLiteral`, …) and needs no declaration: OWL 2 declares
/// it, so using it is never an undeclared-entity violation.
const BUILTIN_NAMESPACES: [&str; 4] = [
    "http://www.w3.org/2002/07/owl#",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "http://www.w3.org/2000/01/rdf-schema#",
    "http://www.w3.org/2001/XMLSchema#",
];

fn is_builtin(iri: &str) -> bool {
    BUILTIN_NAMESPACES.iter().any(|ns| iri.starts_with(ns))
}

/// What each IRI is *used* as, and what it is *declared* as.
///
/// Collected in one pass with horned-owl's immutable visitor, which walks an
/// axiom's whole structure and calls `visit_class`/`visit_object_property`/…
/// only at genuine entity positions — an `AnnotationValue::IRI` reaches
/// `visit_iri` and is therefore (correctly) not an entity use, so an ontology
/// that points `rdfs:seeAlso` at an arbitrary URL is not accused of using an
/// undeclared class.
#[derive(Default)]
struct Signature {
    /// Per kind: used IRI -> the kind of the first axiom that used it (for the
    /// violation message). Only the first is kept, so the map is bounded by the
    /// entity count rather than the axiom count.
    used_classes: HashMap<String, String>,
    used_object_properties: HashMap<String, String>,
    used_data_properties: HashMap<String, String>,
    used_annotation_properties: HashMap<String, String>,
    used_datatypes: HashMap<String, String>,
    declared_classes: HashSet<String>,
    declared_object_properties: HashSet<String>,
    declared_data_properties: HashSet<String>,
    declared_annotation_properties: HashSet<String>,
    declared_datatypes: HashSet<String>,
    /// Kind label of the axiom currently being walked.
    current: String,
}

impl Signature {
    fn collect(model: &Model) -> Signature {
        let mut walk = Walk::new(Signature::default());
        for ac in model.ont.iter() {
            // Declarations are read directly (not off the walk) so an entity
            // counts as declared even though the walk also records it as "used".
            match &ac.component {
                Component::DeclareClass(d) => {
                    walk.as_mut_visit().declared_classes.insert(d.0 .0.as_ref().to_string());
                }
                Component::DeclareObjectProperty(d) => {
                    walk.as_mut_visit()
                        .declared_object_properties
                        .insert(d.0 .0.as_ref().to_string());
                }
                Component::DeclareDataProperty(d) => {
                    walk.as_mut_visit()
                        .declared_data_properties
                        .insert(d.0 .0.as_ref().to_string());
                }
                Component::DeclareAnnotationProperty(d) => {
                    walk.as_mut_visit()
                        .declared_annotation_properties
                        .insert(d.0 .0.as_ref().to_string());
                }
                Component::DeclareDatatype(d) => {
                    walk.as_mut_visit().declared_datatypes.insert(d.0 .0.as_ref().to_string());
                }
                _ => {}
            }
            walk.as_mut_visit().current = kind_label(&ac.component);
            walk.annotated_component(ac);
        }
        walk.into_visit()
    }

    /// Record the first use of `iri` (skipping built-ins, which never need a
    /// declaration and would otherwise dominate the maps).
    fn note(map: &mut HashMap<String, String>, iri: &str, axiom_kind: &str) {
        if !is_builtin(iri) && !map.contains_key(iri) {
            map.insert(iri.to_string(), axiom_kind.to_string());
        }
    }

    /// OWL 2 DL's typing constraint: every IRI used in an entity position must
    /// be declared with that entity's kind. Each kind — class, object property,
    /// data property, annotation property, datatype — is reported separately, so
    /// the message names the declaration that is missing. (Individuals are
    /// exempt: OWL 2 makes individual declarations optional.)
    fn undeclared_violations(&self, out: &mut Vec<Violation>) {
        let kinds: [(&str, &HashMap<String, String>, &HashSet<String>); 5] = [
            ("class", &self.used_classes, &self.declared_classes),
            (
                "object property",
                &self.used_object_properties,
                &self.declared_object_properties,
            ),
            (
                "data property",
                &self.used_data_properties,
                &self.declared_data_properties,
            ),
            (
                "annotation property",
                &self.used_annotation_properties,
                &self.declared_annotation_properties,
            ),
            ("datatype", &self.used_datatypes, &self.declared_datatypes),
        ];
        for (label, used, declared) in kinds {
            let mut missing: Vec<(&String, &String)> =
                used.iter().filter(|(iri, _)| !declared.contains(*iri)).collect();
            // HashMap order is not stable across runs; the report is a build
            // artefact that gets diffed, so sort it.
            missing.sort();
            for (iri, axiom_kind) in missing {
                out.push(Violation {
                    axiom_kind: axiom_kind.clone(),
                    reason: format!("Use of undeclared {label}: {iri}"),
                });
            }
        }
    }

    /// OWL 2 DL's punning constraint: the class and datatype IRIs must be
    /// disjoint, and the object / data / annotation property IRIs pairwise
    /// disjoint. (A class may share an IRI with a named individual — that is the
    /// one pun OWL 2 DL permits, and OBO ontologies rely on it.)
    fn punning_violations(&self, out: &mut Vec<Violation>) {
        let classes = self.entity_set(&self.used_classes, &self.declared_classes);
        let datatypes = self.entity_set(&self.used_datatypes, &self.declared_datatypes);
        let obj = self.entity_set(&self.used_object_properties, &self.declared_object_properties);
        let data = self.entity_set(&self.used_data_properties, &self.declared_data_properties);
        let ann = self.entity_set(
            &self.used_annotation_properties,
            &self.declared_annotation_properties,
        );

        let mut clash: Vec<(String, String)> = Vec::new();
        for iri in classes.intersection(&datatypes) {
            clash.push((
                iri.clone(),
                format!("Datatype IRI also used as class IRI: {iri}"),
            ));
        }
        for (a, b, an, bn) in [
            (&obj, &data, "object property", "data property"),
            (&obj, &ann, "object property", "annotation property"),
            (&data, &ann, "data property", "annotation property"),
        ] {
            for iri in a.intersection(b) {
                clash.push((
                    iri.clone(),
                    format!("Illegal punning: {iri} is used as both an {an} and an {bn}"),
                ));
            }
        }
        clash.sort();
        for (_, reason) in clash {
            out.push(Violation {
                axiom_kind: "Declaration".to_string(),
                reason,
            });
        }
    }

    fn entity_set(&self, used: &HashMap<String, String>, declared: &HashSet<String>) -> HashSet<String> {
        used.keys().chain(declared.iter()).cloned().collect()
    }
}

impl Visit<RcStr> for Signature {
    fn visit_class(&mut self, e: &Class<RcStr>) {
        Signature::note(&mut self.used_classes, e.0.as_ref(), &self.current);
    }
    fn visit_object_property(&mut self, e: &ObjectProperty<RcStr>) {
        Signature::note(&mut self.used_object_properties, e.0.as_ref(), &self.current);
    }
    fn visit_data_property(&mut self, e: &DataProperty<RcStr>) {
        Signature::note(&mut self.used_data_properties, e.0.as_ref(), &self.current);
    }
    fn visit_annotation_property(&mut self, e: &AnnotationProperty<RcStr>) {
        Signature::note(
            &mut self.used_annotation_properties,
            e.0.as_ref(),
            &self.current,
        );
    }
    fn visit_datatype(&mut self, e: &Datatype<RcStr>) {
        Signature::note(&mut self.used_datatypes, e.0.as_ref(), &self.current);
    }
}

// --- DL: property-chain regularity ----------------------------------------

/// OWL 2 DL requires the object-property axioms to be **regular**: there must be
/// a strict partial order ≺ on object properties under which every
/// property-chain axiom has one of the shapes permitted by the OWL 2 Structural
/// Specification §11.1 —
///
/// * `r ∘ r ⊑ r`
/// * `p₁ ∘ … ∘ pₙ ⊑ r`  with every `pᵢ ≺ r`
/// * `r ∘ p₁ ∘ … ∘ pₙ ⊑ r`  with every `pᵢ ≺ r`
/// * `p₁ ∘ … ∘ pₙ ∘ r ⊑ r`  with every `pᵢ ≺ r`
///
/// Such an order exists iff the relation the axioms force is acyclic, so the
/// check is: build the forced `p ≺ r` edges, then look for a cycle and report
/// every property sitting on one. Simplicity is judged on the base property
/// (`inv(s) ≺ r` iff `s ≺ r`), which is why [`ope_iri`] discards the inverse
/// marker.
fn chain_regularity_violations(model: &Model, out: &mut Vec<Violation>) {
    let mut prec: HashMap<&str, HashSet<&str>> = HashMap::new();
    for ac in model.ont.iter() {
        let Component::SubObjectPropertyOf(ax) = &ac.component else {
            continue;
        };
        let SOPE::ObjectPropertyChain(chain) = &ax.sub else {
            continue;
        };
        let r = ope_iri(&ax.sup);
        let ps: Vec<&str> = chain.iter().map(ope_iri).collect();
        let n = ps.len();
        if n == 0 {
            continue;
        }
        // `r ∘ r ⊑ r` — transitivity written as a chain — is explicitly allowed
        // and imposes no ordering at all.
        if n == 2 && ps[0] == r && ps[1] == r {
            continue;
        }
        // `r` is exempt at the head OR the tail, never both: `r ∘ p ∘ r ⊑ r`
        // fits neither permitted shape, and leaving the second `r` in place is
        // what makes it show up as the `r ≺ r` self-cycle it is.
        let (skip_first, skip_last) = if ps[0] == r {
            (true, false)
        } else if ps[n - 1] == r {
            (false, true)
        } else {
            (false, false)
        };
        for (i, p) in ps.iter().enumerate() {
            if (i == 0 && skip_first) || (i + 1 == n && skip_last) {
                continue;
            }
            prec.entry(p).or_default().insert(r);
        }
    }
    if prec.is_empty() {
        return;
    }
    // Iterative DFS with a colouring (0 = unvisited, 1 = on stack, 2 = done);
    // recursion would blow the stack on a pathological chain graph.
    let mut colour: HashMap<&str, u8> = HashMap::new();
    let mut on_cycle: HashSet<&str> = HashSet::new();
    let roots: Vec<&str> = prec.keys().copied().collect();
    for root in roots {
        if colour.get(root).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut stack: Vec<(&str, std::vec::IntoIter<&str>)> = Vec::new();
        colour.insert(root, 1);
        stack.push((root, succs(&prec, root)));
        while let Some((node, iter)) = stack.last_mut() {
            let node = *node;
            match iter.next() {
                Some(next) => match colour.get(next).copied().unwrap_or(0) {
                    // Back-edge: `next` is still on the stack, so everything from
                    // `next` up to `node` sits on a cycle.
                    1 => {
                        on_cycle.insert(next);
                        on_cycle.insert(node);
                        for (n, _) in stack.iter().rev() {
                            on_cycle.insert(n);
                            if *n == next {
                                break;
                            }
                        }
                    }
                    0 => {
                        colour.insert(next, 1);
                        let it = succs(&prec, next);
                        stack.push((next, it));
                    }
                    _ => {}
                },
                None => {
                    colour.insert(node, 2);
                    stack.pop();
                }
            }
        }
    }
    let mut offenders: Vec<&str> = on_cycle.into_iter().collect();
    offenders.sort_unstable();
    for p in offenders {
        out.push(Violation {
            axiom_kind: "SubObjectPropertyOf".to_string(),
            reason: format!("Use of property in chain causes cycle: {p}"),
        });
    }
}

fn succs<'a>(prec: &HashMap<&'a str, HashSet<&'a str>>, node: &str) -> std::vec::IntoIter<&'a str> {
    prec.get(node)
        .map(|s| s.iter().copied().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
}

/// The top-level class expressions a component carries (for DL CE checking).
fn component_ces(comp: &Component<RcStr>) -> Vec<&CE<RcStr>> {
    match comp {
        Component::SubClassOf(ax) => vec![&ax.sub, &ax.sup],
        Component::EquivalentClasses(ax) => ax.0.iter().collect(),
        Component::DisjointClasses(ax) => ax.0.iter().collect(),
        Component::DisjointUnion(ax) => ax.1.iter().collect(),
        Component::ObjectPropertyDomain(ax) => vec![&ax.ce],
        Component::ObjectPropertyRange(ax) => vec![&ax.ce],
        Component::ClassAssertion(ax) => vec![&ax.ce],
        Component::HasKey(ax) => vec![&ax.ce],
        _ => Vec::new(),
    }
}

/// Recursively check a class expression for non-simple roles in number
/// restrictions or `ObjectHasSelf` (the OWL 2 DL global restriction).
fn dl_ce(ce: &CE<RcStr>, non_simple: &HashSet<String>) -> Option<String> {
    match ce {
        CE::ObjectMinCardinality { ope, bce, .. }
        | CE::ObjectMaxCardinality { ope, bce, .. }
        | CE::ObjectExactCardinality { ope, bce, .. } => {
            if non_simple.contains(ope_iri(ope)) {
                return Some(format!(
                    "number restriction on non-simple object property <{}> is not in OWL 2 DL",
                    ope_iri(ope)
                ));
            }
            dl_ce(bce, non_simple)
        }
        CE::ObjectHasSelf(ope) => {
            if non_simple.contains(ope_iri(ope)) {
                Some(format!(
                    "ObjectHasSelf on non-simple object property <{}> is not in OWL 2 DL",
                    ope_iri(ope)
                ))
            } else {
                None
            }
        }
        CE::ObjectIntersectionOf(v) | CE::ObjectUnionOf(v) => v.iter().find_map(|c| dl_ce(c, non_simple)),
        CE::ObjectComplementOf(b) => dl_ce(b, non_simple),
        CE::ObjectSomeValuesFrom { bce, .. } | CE::ObjectAllValuesFrom { bce, .. } => {
            dl_ce(bce, non_simple)
        }
        _ => None,
    }
}

// --- EL ------------------------------------------------------------------

fn el_axiom_violation(comp: &Component<RcStr>) -> Option<String> {
    match comp {
        Component::SubClassOf(ax) => el_ce(&ax.sub).or_else(|| el_ce(&ax.sup)),
        Component::EquivalentClasses(ax) => ax.0.iter().find_map(el_ce),
        Component::DisjointClasses(ax) => ax.0.iter().find_map(el_ce),
        Component::ObjectPropertyDomain(ax) => el_ce(&ax.ce),
        Component::ObjectPropertyRange(ax) => el_ce(&ax.ce),
        Component::ClassAssertion(ax) => el_ce(&ax.ce),
        // Constructs not allowed in EL at all.
        Component::DisjointUnion(_) => Some("DisjointUnion is not in OWL 2 EL".into()),
        Component::InverseObjectProperties(_) => {
            Some("inverse object properties are not in OWL 2 EL".into())
        }
        Component::SymmetricObjectProperty(_) => {
            Some("symmetric object properties are not in OWL 2 EL".into())
        }
        Component::AsymmetricObjectProperty(_) => {
            Some("asymmetric object properties are not in OWL 2 EL".into())
        }
        Component::IrreflexiveObjectProperty(_) => {
            Some("irreflexive object properties are not in OWL 2 EL".into())
        }
        Component::FunctionalObjectProperty(_)
        | Component::InverseFunctionalObjectProperty(_) => {
            Some("functional object properties are not in OWL 2 EL".into())
        }
        _ => None,
    }
}

/// EL allows: class names, owl:Thing/Nothing, ObjectIntersectionOf,
/// ObjectSomeValuesFrom, ObjectHasValue, ObjectHasSelf, ObjectOneOf (singleton),
/// data some-values/has-value. Everything else is a violation.
fn el_ce(ce: &CE<RcStr>) -> Option<String> {
    match ce {
        CE::Class(_) => None,
        CE::ObjectIntersectionOf(parts) => parts.iter().find_map(el_ce),
        CE::ObjectSomeValuesFrom { bce, .. } => el_ce(bce),
        CE::ObjectHasValue { .. } => None,
        CE::ObjectHasSelf(_) => None,
        CE::ObjectOneOf(inds) if inds.len() == 1 => None,
        CE::DataSomeValuesFrom { .. } | CE::DataHasValue { .. } => None,
        CE::ObjectUnionOf(_) => Some("ObjectUnionOf is not in OWL 2 EL".into()),
        CE::ObjectComplementOf(_) => Some("ObjectComplementOf is not in OWL 2 EL".into()),
        CE::ObjectAllValuesFrom { .. } => {
            Some("ObjectAllValuesFrom is not in OWL 2 EL".into())
        }
        CE::ObjectMinCardinality { .. }
        | CE::ObjectMaxCardinality { .. }
        | CE::ObjectExactCardinality { .. } => {
            Some("cardinality restrictions are not in OWL 2 EL".into())
        }
        CE::ObjectOneOf(_) => Some("ObjectOneOf with multiple individuals is not in OWL 2 EL".into()),
        _ => Some("class expression is not in OWL 2 EL".into()),
    }
}

// --- QL (characteristic restrictions) ------------------------------------

fn ql_axiom_violation(comp: &Component<RcStr>) -> Option<String> {
    match comp {
        // QL subclass expressions must be a class or ∃r.⊤ ("some"); superclass
        // may be a class, ¬C, or ∃r.C. We check the most common offenders.
        Component::SubClassOf(ax) => ql_sub(&ax.sub).or_else(|| ql_super(&ax.sup)),
        Component::TransitiveObjectProperty(_) => {
            Some("transitive properties are not in OWL 2 QL".into())
        }
        Component::SubObjectPropertyOf(ax) => match &ax.sub {
            horned_owl::model::SubObjectPropertyExpression::ObjectPropertyChain(_) => {
                Some("property chains are not in OWL 2 QL".into())
            }
            _ => None,
        },
        Component::FunctionalObjectProperty(_)
        | Component::InverseFunctionalObjectProperty(_) => {
            Some("functional properties are not in OWL 2 QL".into())
        }
        _ => None,
    }
}

fn ql_sub(ce: &CE<RcStr>) -> Option<String> {
    match ce {
        CE::Class(_) => None,
        CE::ObjectSomeValuesFrom { bce, .. } => match bce.as_ref() {
            CE::Class(c) if c.0.as_ref() == "http://www.w3.org/2002/07/owl#Thing" => None,
            _ => Some("QL subclass ∃ must have owl:Thing filler".into()),
        },
        _ => Some("class expression is not a valid OWL 2 QL subclass".into()),
    }
}

fn ql_super(ce: &CE<RcStr>) -> Option<String> {
    match ce {
        CE::Class(_) => None,
        CE::ObjectComplementOf(_) => None,
        CE::ObjectSomeValuesFrom { .. } => None,
        CE::ObjectIntersectionOf(parts) => parts.iter().find_map(ql_super),
        _ => Some("class expression is not a valid OWL 2 QL superclass".into()),
    }
}

// --- RL (characteristic restrictions) ------------------------------------

fn rl_axiom_violation(comp: &Component<RcStr>) -> Option<String> {
    match comp {
        Component::SubClassOf(ax) => rl_sub(&ax.sub).or_else(|| rl_super(&ax.sup)),
        _ => None,
    }
}

fn rl_sub(ce: &CE<RcStr>) -> Option<String> {
    match ce {
        CE::Class(c) if c.0.as_ref() == "http://www.w3.org/2002/07/owl#Thing" => {
            Some("owl:Thing is not a valid OWL 2 RL subclass".into())
        }
        CE::Class(_) => None,
        CE::ObjectIntersectionOf(parts) => parts.iter().find_map(rl_sub),
        CE::ObjectUnionOf(parts) => parts.iter().find_map(rl_sub),
        CE::ObjectSomeValuesFrom { .. } => None,
        CE::ObjectOneOf(_) => None,
        _ => Some("class expression is not a valid OWL 2 RL subclass".into()),
    }
}

fn rl_super(ce: &CE<RcStr>) -> Option<String> {
    match ce {
        CE::Class(c) if c.0.as_ref() == "http://www.w3.org/2002/07/owl#Thing" => {
            Some("owl:Thing is not a valid OWL 2 RL superclass".into())
        }
        CE::Class(_) => None,
        CE::ObjectIntersectionOf(parts) => parts.iter().find_map(rl_super),
        CE::ObjectComplementOf(_) => None,
        CE::ObjectAllValuesFrom { .. } => None,
        CE::ObjectMaxCardinality { n, .. } if *n <= 1 => None,
        _ => Some("class expression is not a valid OWL 2 RL superclass".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::{
        AnnotationAssertion, AnnotationSubject, AnnotationValue, Build, DeclareAnnotationProperty,
        DeclareClass, DeclareDatatype, DeclareObjectProperty, FunctionalObjectProperty,
        InverseObjectProperties, Literal, MutableOntology, SubClassOf, SubObjectPropertyOf,
        TransitiveObjectProperty,
    };
    use horned_owl::ontology::set::SetOntology;

    const NS: &str = "http://example.org/";

    fn model_of(comps: Vec<Component<RcStr>>) -> Model {
        let mut ont: SetOntology<RcStr> = SetOntology::new();
        for c in comps {
            ont.insert(c);
        }
        Model::from_parts(ont, crate::model::default_prefixes())
    }

    fn b() -> Build<RcStr> {
        Build::new_rc()
    }

    fn cls(b: &Build<RcStr>, n: &str) -> CE<RcStr> {
        CE::Class(b.class(format!("{NS}{n}")))
    }

    fn op(b: &Build<RcStr>, n: &str) -> OPE<RcStr> {
        OPE::ObjectProperty(b.object_property(format!("{NS}{n}")))
    }

    /// The violation messages, so a test can assert on the one it is about.
    fn reasons(model: &Model) -> Vec<String> {
        validate(model, Profile::Dl).into_iter().map(|v| v.reason).collect()
    }

    fn has(model: &Model, needle: &str) -> bool {
        reasons(model).iter().any(|r| r.contains(needle))
    }

    // --- typing (declarations) --------------------------------------------

    #[test]
    fn undeclared_entities_are_reported_per_kind() {
        let b = b();
        // `A` is declared, `B` is not; `r` is used but never declared.
        let m = model_of(vec![
            Component::DeclareClass(DeclareClass(b.class(format!("{NS}A")))),
            Component::SubClassOf(SubClassOf { sub: cls(&b, "A"), sup: cls(&b, "B") }),
            Component::SubClassOf(SubClassOf {
                sub: cls(&b, "A"),
                sup: CE::ObjectSomeValuesFrom {
                    ope: op(&b, "r"),
                    bce: Box::new(cls(&b, "A")),
                },
            }),
        ]);
        assert!(has(&m, &format!("Use of undeclared class: {NS}B")), "{:?}", reasons(&m));
        assert!(
            has(&m, &format!("Use of undeclared object property: {NS}r")),
            "{:?}",
            reasons(&m)
        );
        assert!(!has(&m, &format!("Use of undeclared class: {NS}A")));
    }

    #[test]
    fn builtin_vocabulary_needs_no_declaration() {
        // Ontology edit files are full of `owl:Thing`, `rdfs:label` and
        // `xsd:string`; none of them is ever declared, and none is a violation.
        let b = b();
        let label = b.annotation_property("http://www.w3.org/2000/01/rdf-schema#label");
        let m = model_of(vec![
            Component::DeclareClass(DeclareClass(b.class(format!("{NS}A")))),
            Component::SubClassOf(SubClassOf {
                sub: cls(&b, "A"),
                sup: CE::Class(b.class("http://www.w3.org/2002/07/owl#Thing")),
            }),
            Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(format!("{NS}A"))),
                ann: horned_owl::model::Annotation {
                    ann: Default::default(),
                    ap: label,
                    av: AnnotationValue::Literal(Literal::Simple { literal: "a".into() }),
                },
            }),
        ]);
        assert!(
            !reasons(&m).iter().any(|r| r.starts_with("Use of undeclared")),
            "{:?}",
            reasons(&m)
        );
    }

    #[test]
    fn an_annotation_value_iri_is_not_an_entity_use() {
        // `rdfs:seeAlso <http://example.org/whatever>` must not be read as a use
        // of an undeclared class — that would flag most OBO ontologies.
        let b = b();
        let see_also = b.annotation_property("http://www.w3.org/2000/01/rdf-schema#seeAlso");
        let m = model_of(vec![
            Component::DeclareClass(DeclareClass(b.class(format!("{NS}A")))),
            Component::AnnotationAssertion(AnnotationAssertion {
                subject: AnnotationSubject::IRI(b.iri(format!("{NS}A"))),
                ann: horned_owl::model::Annotation {
                    ann: Default::default(),
                    ap: see_also,
                    av: AnnotationValue::IRI(b.iri("http://example.org/not-a-class")),
                },
            }),
        ]);
        assert!(
            !reasons(&m).iter().any(|r| r.starts_with("Use of undeclared")),
            "{:?}",
            reasons(&m)
        );
    }

    // --- punning -----------------------------------------------------------

    #[test]
    fn a_class_iri_reused_as_a_datatype_is_illegal() {
        let b = b();
        let m = model_of(vec![
            Component::DeclareClass(DeclareClass(b.class(format!("{NS}A")))),
            Component::DeclareDatatype(DeclareDatatype(b.datatype(format!("{NS}A")))),
        ]);
        assert!(
            has(&m, &format!("Datatype IRI also used as class IRI: {NS}A")),
            "{:?}",
            reasons(&m)
        );
    }

    #[test]
    fn object_and_annotation_property_punning_is_illegal() {
        let b = b();
        let m = model_of(vec![
            Component::DeclareObjectProperty(DeclareObjectProperty(
                b.object_property(format!("{NS}p")),
            )),
            Component::DeclareAnnotationProperty(DeclareAnnotationProperty(
                b.annotation_property(format!("{NS}p")),
            )),
        ]);
        assert!(has(&m, "Illegal punning"), "{:?}", reasons(&m));
    }

    #[test]
    fn a_class_may_share_an_iri_with_an_individual() {
        // The one pun OWL 2 DL allows, and OBO ontologies use it.
        let b = b();
        let m = model_of(vec![
            Component::DeclareClass(DeclareClass(b.class(format!("{NS}A")))),
            Component::DeclareNamedIndividual(horned_owl::model::DeclareNamedIndividual(
                b.named_individual(format!("{NS}A")),
            )),
        ]);
        assert!(!has(&m, "punning"), "{:?}", reasons(&m));
    }

    // --- non-simplicity across InverseObjectProperties ---------------------

    #[test]
    fn non_simplicity_propagates_across_an_inverse_axiom() {
        // `p` is transitive (hence non-simple) and `q` is its inverse, so `q` is
        // non-simple too and may not be functional.
        let b = b();
        let comps = vec![
            Component::TransitiveObjectProperty(TransitiveObjectProperty(op(&b, "p"))),
            Component::InverseObjectProperties(InverseObjectProperties(
                op(&b, "p"),
                op(&b, "q"),
            )),
            Component::FunctionalObjectProperty(FunctionalObjectProperty(op(&b, "q"))),
        ];
        let m = model_of(comps);
        let ns = non_simple_roles(&m);
        assert!(ns.contains(&format!("{NS}p")));
        assert!(ns.contains(&format!("{NS}q")), "inverse of a non-simple role is non-simple");
        assert!(has(&m, "non-simple object property"), "{:?}", reasons(&m));
    }

    #[test]
    fn a_simple_inverse_stays_simple() {
        let b = b();
        let m = model_of(vec![
            Component::InverseObjectProperties(InverseObjectProperties(
                op(&b, "p"),
                op(&b, "q"),
            )),
            Component::FunctionalObjectProperty(FunctionalObjectProperty(op(&b, "q"))),
        ]);
        assert!(!has(&m, "non-simple object property"), "{:?}", reasons(&m));
    }

    #[test]
    fn a_non_simple_role_in_a_cardinality_restriction_is_a_violation() {
        let b = b();
        let m = model_of(vec![
            Component::TransitiveObjectProperty(TransitiveObjectProperty(op(&b, "p"))),
            Component::SubClassOf(SubClassOf {
                sub: cls(&b, "A"),
                sup: CE::ObjectMinCardinality {
                    n: 1,
                    ope: op(&b, "p"),
                    bce: Box::new(cls(&b, "B")),
                },
            }),
        ]);
        assert!(has(&m, "number restriction on non-simple"), "{:?}", reasons(&m));
    }

    // --- property-chain regularity -----------------------------------------

    #[test]
    fn a_chain_cycle_is_reported() {
        // `p ∘ q ⊑ r` forces p ≺ r; `r ∘ s ⊑ p` forces r ≺ p — no strict partial
        // order exists, so the ontology is outside OWL 2 DL.
        let b = b();
        let chain = |ps: Vec<OPE<RcStr>>, sup: OPE<RcStr>| {
            Component::SubObjectPropertyOf(SubObjectPropertyOf {
                sub: SOPE::ObjectPropertyChain(ps),
                sup,
            })
        };
        let m = model_of(vec![
            chain(vec![op(&b, "p"), op(&b, "q")], op(&b, "r")),
            chain(vec![op(&b, "r"), op(&b, "s")], op(&b, "p")),
        ]);
        assert!(has(&m, "Use of property in chain causes cycle"), "{:?}", reasons(&m));
    }

    #[test]
    fn the_permitted_chain_shapes_are_regular() {
        // `p ∘ p ⊑ p` (transitivity) and `r ∘ p ⊑ r` (r at the head) are both
        // legal and must not be flagged — UBERON and CL are full of them.
        let b = b();
        let chain = |ps: Vec<OPE<RcStr>>, sup: OPE<RcStr>| {
            Component::SubObjectPropertyOf(SubObjectPropertyOf {
                sub: SOPE::ObjectPropertyChain(ps),
                sup,
            })
        };
        let m = model_of(vec![
            chain(vec![op(&b, "p"), op(&b, "p")], op(&b, "p")),
            chain(vec![op(&b, "r"), op(&b, "p")], op(&b, "r")),
            chain(vec![op(&b, "p"), op(&b, "r")], op(&b, "r")),
        ]);
        assert!(!has(&m, "causes cycle"), "{:?}", reasons(&m));
    }

    #[test]
    fn a_property_in_the_middle_of_its_own_chain_is_a_cycle() {
        // `r ∘ p ∘ r ⊑ r` matches neither permitted shape: `r` is exempt at one
        // end only.
        let b = b();
        let m = model_of(vec![Component::SubObjectPropertyOf(SubObjectPropertyOf {
            sub: SOPE::ObjectPropertyChain(vec![op(&b, "r"), op(&b, "p"), op(&b, "r")]),
            sup: op(&b, "r"),
        })]);
        assert!(has(&m, "Use of property in chain causes cycle"), "{:?}", reasons(&m));
    }

    // --- the profile names used in the report header ------------------------

    #[test]
    fn profile_names_match_owlapi() {
        assert_eq!(Profile::Dl.name(), "OWL 2 DL");
        assert_eq!(Profile::El.name(), "OWL 2 EL");
        assert_eq!(Profile::Ql.name(), "OWL 2 QL");
        assert_eq!(Profile::Rl.name(), "OWL 2 RL");
    }
}
