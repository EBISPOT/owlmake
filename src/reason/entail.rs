//! OWL 2 **entailment** checking — the capability that distinguishes a finished
//! DL reasoner from a bare classifier.
//!
//! [`entails`] decides whether `premise ⊨ conclusion`: every axiom of the
//! conclusion ontology follows from the premise under OWL 2 Direct Semantics. It
//! is **sound and complete** for the SROIQ(D) fragment the [`DlReasoner`]
//! decides, because each conclusion axiom is reduced to a (in)consistency test:
//!
//! > `P ⊨ α`  iff  `P ∪ ¬α` is inconsistent,
//!
//! where `¬α` is a *witness* built from fresh individuals. For example
//! `P ⊨ C ⊑ D` iff `P` together with an individual that is `C` and `¬D` is
//! inconsistent; `P ⊨ r(a,b)` iff `P ∪ {¬r(a,b)}` is inconsistent. Axioms whose
//! negation needs several witness facts (transitivity, functionality, property
//! chains) expand to a small ABox. Because the reasoner's consistency check is
//! sound, every entailment reported here genuinely holds; because it is complete
//! on this fragment, every genuine entailment is found.
//!
//! Multi-part axioms (an `EquivalentClasses` over a list, a `DifferentIndividuals`
//! set) decompose into several independent obligations, *all* of which must be
//! inconsistent for the axiom to be entailed. Non-logical axioms (declarations,
//! annotations, imports) are entailed by every ontology.

use horned_owl::model as m;
use horned_owl::model::{
    Build, ClassExpression as CE, Component, DataProperty, DataRange, Individual, Literal,
    MutableOntology, ObjectPropertyExpression as OPE, RcStr,
    SubObjectPropertyExpression as SOPE,
};
use horned_owl::ontology::set::SetOntology;

use crate::model::{clone_prefixes, Model};
// The DL consistency check below uses hermit-rs, which is not compiled into the
// wasm build; there it falls back to the built-in EL reasoner's consistency.
#[cfg(not(target_arch = "wasm32"))]
use crate::reason::DlReasoner;
#[cfg(target_arch = "wasm32")]
use crate::reason::Reasoner as DlReasoner;

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDFS_LITERAL: &str = "http://www.w3.org/2000/01/rdf-schema#Literal";

/// Whether `premise ⊨ conclusion` — every axiom of `conclusion` is entailed by
/// `premise`. Sound and complete for the reasoner's SROIQ(D) fragment (see the
/// module docs). An axiom the reduction cannot express conservatively counts as
/// *not proven* (so a `false` here may be "unknown" only for out-of-fragment
/// conclusion axioms such as keys or SWRL rules, which the OWL 2 entailment
/// fragment does not use as conclusions).
pub fn entails(premise: &Model, conclusion: &Model) -> bool {
    let build = Build::new_rc();
    let mut fresh = Fresh { build: &build, n: 0 };
    for ac in conclusion.ont.iter() {
        let obls = match obligations(&build, &mut fresh, &ac.component) {
            Some(o) => o,
            None => return false, // out-of-fragment conclusion axiom; cannot prove
        };
        // The axiom is entailed iff *every* witness ontology is inconsistent.
        for extra in &obls {
            if consistent_after(premise, extra) {
                return false;
            }
        }
    }
    true
}

/// Whether named individual `ind` is entailed to be an instance of named class
/// `class` by `premise` (instance checking).
pub fn is_instance(premise: &Model, ind: &str, class: &str) -> bool {
    let build = Build::new_rc();
    let i = Individual::Named(build.named_individual(ind));
    let ce = CE::Class(build.class(class));
    !consistent_after(premise, &[ca(complement(&ce), i)])
}

/// The named classes `premise` entails `ind` to be an instance of (realization
/// of a single individual). Restricted to the classes that appear in `premise`.
pub fn types(premise: &Model, ind: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in premise_class_iris(premise) {
        if is_instance(premise, ind, &c) {
            out.push(c);
        }
    }
    out
}

/// The named individuals `premise` entails to be instances of named class
/// `class` (instance retrieval). Restricted to the individuals named in
/// `premise`.
pub fn instances(premise: &Model, class: &str) -> Vec<String> {
    let mut out = Vec::new();
    for i in premise_individual_iris(premise) {
        if is_instance(premise, &i, class) {
            out.push(i);
        }
    }
    out
}

/// Run the reasoner on `premise` extended with `extra`, returning consistency.
fn consistent_after(premise: &Model, extra: &[Component<RcStr>]) -> bool {
    let mut ont = SetOntology::new();
    for ac in premise.ont.iter() {
        ont.insert(ac.clone());
    }
    for c in extra {
        ont.insert(c.clone());
    }
    let model = Model::from_parts(ont, clone_prefixes(&premise.prefixes));
    DlReasoner::classify(&model).is_consistent()
}

/// Fresh-individual generator (`urn:owlmake:entail:iN`), used to build the
/// witness ABox that negates a conclusion axiom.
struct Fresh<'a> {
    build: &'a Build<RcStr>,
    n: u32,
}

impl Fresh<'_> {
    fn ind(&mut self) -> Individual<RcStr> {
        let i = Individual::Named(
            self.build
                .named_individual(format!("urn:owlmake:entail:i{}", self.n)),
        );
        self.n += 1;
        i
    }
}

fn complement(ce: &CE<RcStr>) -> CE<RcStr> {
    CE::ObjectComplementOf(Box::new(ce.clone()))
}

fn ca(ce: CE<RcStr>, i: Individual<RcStr>) -> Component<RcStr> {
    Component::ClassAssertion(m::ClassAssertion { ce, i })
}

fn opa(ope: OPE<RcStr>, from: Individual<RcStr>, to: Individual<RcStr>) -> Component<RcStr> {
    Component::ObjectPropertyAssertion(m::ObjectPropertyAssertion { ope, from, to })
}

fn nopa(ope: OPE<RcStr>, from: Individual<RcStr>, to: Individual<RcStr>) -> Component<RcStr> {
    Component::NegativeObjectPropertyAssertion(m::NegativeObjectPropertyAssertion { ope, from, to })
}

fn dpa(dp: DataProperty<RcStr>, from: Individual<RcStr>, to: Literal<RcStr>) -> Component<RcStr> {
    Component::DataPropertyAssertion(m::DataPropertyAssertion { dp, from, to })
}

fn ndpa(dp: DataProperty<RcStr>, from: Individual<RcStr>, to: Literal<RcStr>) -> Component<RcStr> {
    Component::NegativeDataPropertyAssertion(m::NegativeDataPropertyAssertion { dp, from, to })
}

fn same(a: Individual<RcStr>, b: Individual<RcStr>) -> Component<RcStr> {
    Component::SameIndividual(m::SameIndividual(vec![a, b]))
}

fn different(a: Individual<RcStr>, b: Individual<RcStr>) -> Component<RcStr> {
    Component::DifferentIndividuals(m::DifferentIndividuals(vec![a, b]))
}

fn witness_lit(build: &Build<RcStr>) -> Literal<RcStr> {
    Literal::Datatype {
        literal: "owlmake-entail-witness".to_string(),
        datatype_iri: build.iri(XSD_STRING),
    }
}

/// The set of witness ontologies (each a list of axioms to add to the premise)
/// whose collective inconsistency means `comp` is entailed. `Some(vec![])` means
/// trivially entailed; `None` means out of the reducible fragment.
fn obligations(
    build: &Build<RcStr>,
    f: &mut Fresh,
    comp: &Component<RcStr>,
) -> Option<Vec<Vec<Component<RcStr>>>> {
    // `C ⊑ D` obligation: a fresh individual that is `C` and `¬D`.
    let subclass = |f: &mut Fresh, sub: &CE<RcStr>, sup: &CE<RcStr>| {
        let x = f.ind();
        vec![ca(sub.clone(), x.clone()), ca(complement(sup), x)]
    };
    // `r ⊑ s` (simple) obligation: a fresh `r`-edge that is not an `s`-edge.
    let subprop = |f: &mut Fresh, sub: &OPE<RcStr>, sup: &OPE<RcStr>| {
        let a = f.ind();
        let b = f.ind();
        vec![opa(sub.clone(), a.clone(), b.clone()), nopa(sup.clone(), a, b)]
    };
    let lit = witness_lit(build);

    Some(match comp {
        // Non-logical axioms: entailed by every ontology.
        Component::DeclareClass(_)
        | Component::DeclareObjectProperty(_)
        | Component::DeclareDataProperty(_)
        | Component::DeclareAnnotationProperty(_)
        | Component::DeclareNamedIndividual(_)
        | Component::DeclareDatatype(_)
        | Component::AnnotationAssertion(_)
        | Component::SubAnnotationPropertyOf(_)
        | Component::AnnotationPropertyDomain(_)
        | Component::AnnotationPropertyRange(_)
        | Component::OntologyAnnotation(_)
        | Component::Import(_)
        | Component::OntologyID(_)
        | Component::DocIRI(_) => vec![],

        Component::SubClassOf(ax) => vec![subclass(f, &ax.sub, &ax.sup)],
        Component::EquivalentClasses(ax) => {
            let mut obls = Vec::new();
            for w in ax.0.windows(2) {
                obls.push(subclass(f, &w[0], &w[1]));
                obls.push(subclass(f, &w[1], &w[0]));
            }
            obls
        }
        Component::DisjointClasses(ax) => {
            let mut obls = Vec::new();
            for i in 0..ax.0.len() {
                for j in (i + 1)..ax.0.len() {
                    let x = f.ind();
                    obls.push(vec![ca(ax.0[i].clone(), x.clone()), ca(ax.0[j].clone(), x)]);
                }
            }
            obls
        }
        Component::DisjointUnion(ax) => {
            let c = CE::Class(ax.0.clone());
            let union = CE::ObjectUnionOf(ax.1.clone());
            let mut obls = vec![subclass(f, &c, &union), subclass(f, &union, &c)];
            for i in 0..ax.1.len() {
                for j in (i + 1)..ax.1.len() {
                    let x = f.ind();
                    obls.push(vec![ca(ax.1[i].clone(), x.clone()), ca(ax.1[j].clone(), x)]);
                }
            }
            obls
        }

        Component::ClassAssertion(ax) => vec![vec![ca(complement(&ax.ce), ax.i.clone())]],
        Component::ObjectPropertyAssertion(ax) => {
            vec![vec![nopa(ax.ope.clone(), ax.from.clone(), ax.to.clone())]]
        }
        Component::NegativeObjectPropertyAssertion(ax) => {
            vec![vec![opa(ax.ope.clone(), ax.from.clone(), ax.to.clone())]]
        }
        Component::DataPropertyAssertion(ax) => {
            vec![vec![ndpa(ax.dp.clone(), ax.from.clone(), ax.to.clone())]]
        }
        Component::NegativeDataPropertyAssertion(ax) => {
            vec![vec![dpa(ax.dp.clone(), ax.from.clone(), ax.to.clone())]]
        }
        Component::SameIndividual(ax) => {
            let mut obls = Vec::new();
            for w in ax.0.windows(2) {
                obls.push(vec![different(w[0].clone(), w[1].clone())]);
            }
            obls
        }
        Component::DifferentIndividuals(ax) => {
            let mut obls = Vec::new();
            for i in 0..ax.0.len() {
                for j in (i + 1)..ax.0.len() {
                    obls.push(vec![same(ax.0[i].clone(), ax.0[j].clone())]);
                }
            }
            obls
        }

        Component::SubObjectPropertyOf(ax) => match &ax.sub {
            SOPE::ObjectPropertyExpression(sub) => vec![subprop(f, sub, &ax.sup)],
            SOPE::ObjectPropertyChain(chain) => {
                // A fresh chain a₀ -r₁-> a₁ … -rₙ-> aₙ that must not be an `s`-edge.
                let mut nodes = vec![f.ind()];
                let mut comps = Vec::new();
                for r in chain {
                    let next = f.ind();
                    comps.push(opa(r.clone(), nodes.last().unwrap().clone(), next.clone()));
                    nodes.push(next);
                }
                comps.push(nopa(
                    ax.sup.clone(),
                    nodes.first().unwrap().clone(),
                    nodes.last().unwrap().clone(),
                ));
                vec![comps]
            }
        },
        Component::EquivalentObjectProperties(ax) => {
            let mut obls = Vec::new();
            for w in ax.0.windows(2) {
                obls.push(subprop(f, &w[0], &w[1]));
                obls.push(subprop(f, &w[1], &w[0]));
            }
            obls
        }
        Component::InverseObjectProperties(ax) => {
            let (a, b) = (f.ind(), f.ind());
            let (c, d) = (f.ind(), f.ind());
            vec![
                vec![opa(ax.0.clone(), a.clone(), b.clone()), nopa(ax.1.clone(), b, a)],
                vec![opa(ax.1.clone(), c.clone(), d.clone()), nopa(ax.0.clone(), d, c)],
            ]
        }
        Component::DisjointObjectProperties(ax) => {
            let mut obls = Vec::new();
            for i in 0..ax.0.len() {
                for j in (i + 1)..ax.0.len() {
                    let (a, b) = (f.ind(), f.ind());
                    obls.push(vec![
                        opa(ax.0[i].clone(), a.clone(), b.clone()),
                        opa(ax.0[j].clone(), a, b),
                    ]);
                }
            }
            obls
        }
        Component::ObjectPropertyDomain(ax) => {
            let (a, b) = (f.ind(), f.ind());
            vec![vec![
                opa(ax.ope.clone(), a.clone(), b),
                ca(complement(&ax.ce), a),
            ]]
        }
        Component::ObjectPropertyRange(ax) => {
            let (a, b) = (f.ind(), f.ind());
            vec![vec![
                opa(ax.ope.clone(), a, b.clone()),
                ca(complement(&ax.ce), b),
            ]]
        }
        Component::FunctionalObjectProperty(ax) => {
            let (a, b, c) = (f.ind(), f.ind(), f.ind());
            vec![vec![
                opa(ax.0.clone(), a.clone(), b.clone()),
                opa(ax.0.clone(), a, c.clone()),
                different(b, c),
            ]]
        }
        Component::InverseFunctionalObjectProperty(ax) => {
            let (a, b, c) = (f.ind(), f.ind(), f.ind());
            vec![vec![
                opa(ax.0.clone(), b.clone(), a.clone()),
                opa(ax.0.clone(), c.clone(), a),
                different(b, c),
            ]]
        }
        Component::ReflexiveObjectProperty(ax) => {
            let a = f.ind();
            vec![vec![nopa(ax.0.clone(), a.clone(), a)]]
        }
        Component::IrreflexiveObjectProperty(ax) => {
            let a = f.ind();
            vec![vec![opa(ax.0.clone(), a.clone(), a)]]
        }
        Component::SymmetricObjectProperty(ax) => {
            let (a, b) = (f.ind(), f.ind());
            vec![vec![
                opa(ax.0.clone(), a.clone(), b.clone()),
                nopa(ax.0.clone(), b, a),
            ]]
        }
        Component::AsymmetricObjectProperty(ax) => {
            let (a, b) = (f.ind(), f.ind());
            vec![vec![
                opa(ax.0.clone(), a.clone(), b.clone()),
                opa(ax.0.clone(), b, a),
            ]]
        }
        Component::TransitiveObjectProperty(ax) => {
            let (a, b, c) = (f.ind(), f.ind(), f.ind());
            vec![vec![
                opa(ax.0.clone(), a.clone(), b.clone()),
                opa(ax.0.clone(), b, c.clone()),
                nopa(ax.0.clone(), a, c),
            ]]
        }

        Component::SubDataPropertyOf(ax) => {
            let a = f.ind();
            vec![vec![
                dpa(ax.sub.clone(), a.clone(), lit.clone()),
                ndpa(ax.sup.clone(), a, lit.clone()),
            ]]
        }
        Component::EquivalentDataProperties(ax) => {
            let mut obls = Vec::new();
            for w in ax.0.windows(2) {
                let a = f.ind();
                obls.push(vec![
                    dpa(w[0].clone(), a.clone(), lit.clone()),
                    ndpa(w[1].clone(), a, lit.clone()),
                ]);
                let b = f.ind();
                obls.push(vec![
                    dpa(w[1].clone(), b.clone(), lit.clone()),
                    ndpa(w[0].clone(), b, lit.clone()),
                ]);
            }
            obls
        }
        Component::DisjointDataProperties(ax) => {
            let mut obls = Vec::new();
            for i in 0..ax.0.len() {
                for j in (i + 1)..ax.0.len() {
                    let a = f.ind();
                    obls.push(vec![
                        dpa(ax.0[i].clone(), a.clone(), lit.clone()),
                        dpa(ax.0[j].clone(), a, lit.clone()),
                    ]);
                }
            }
            obls
        }
        Component::DataPropertyDomain(ax) => {
            let a = f.ind();
            vec![vec![
                dpa(ax.dp.clone(), a.clone(), lit.clone()),
                ca(complement(&ax.ce), a),
            ]]
        }
        Component::DataPropertyRange(ax) => {
            // A fresh individual with a dp-value forced *outside* the range.
            let a = f.ind();
            let outside = CE::DataSomeValuesFrom {
                dp: ax.dp.clone(),
                dr: DataRange::DataComplementOf(Box::new(ax.dr.clone())),
            };
            vec![vec![ca(outside, a)]]
        }
        Component::FunctionalDataProperty(ax) => {
            // A fresh individual with two distinct dp-values.
            let a = f.ind();
            let two = CE::DataMinCardinality {
                n: 2,
                dp: ax.0.clone(),
                dr: DataRange::Datatype(build.datatype(RDFS_LITERAL)),
            };
            vec![vec![ca(two, a)]]
        }

        // Out of the reducible entailment fragment (keys, datatype definitions,
        // SWRL rules); never used as OWL 2 entailment-test conclusions.
        _ => return None,
    })
}

fn premise_class_iris(premise: &Model) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for ac in premise.ont.iter() {
        if let Component::DeclareClass(dc) = &ac.component {
            set.insert(dc.0 .0.as_ref().to_string());
        }
        if let Component::ClassAssertion(ax) = &ac.component {
            if let CE::Class(c) = &ax.ce {
                set.insert(c.0.as_ref().to_string());
            }
        }
        if let Component::SubClassOf(ax) = &ac.component {
            if let CE::Class(c) = &ax.sub {
                set.insert(c.0.as_ref().to_string());
            }
            if let CE::Class(c) = &ax.sup {
                set.insert(c.0.as_ref().to_string());
            }
        }
    }
    set.into_iter().collect()
}

fn premise_individual_iris(premise: &Model) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    let note = |i: &Individual<RcStr>, set: &mut std::collections::BTreeSet<String>| {
        if let Individual::Named(n) = i {
            set.insert(n.0.as_ref().to_string());
        }
    };
    for ac in premise.ont.iter() {
        match &ac.component {
            Component::DeclareNamedIndividual(d) => {
                set.insert(d.0 .0.as_ref().to_string());
            }
            Component::ClassAssertion(ax) => note(&ax.i, &mut set),
            Component::ObjectPropertyAssertion(ax) => {
                note(&ax.from, &mut set);
                note(&ax.to, &mut set);
            }
            _ => {}
        }
    }
    set.into_iter().collect()
}
