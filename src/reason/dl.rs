//! OWL 2 DL reasoning — the `--reasoner hermit`/`jfact` backend.
//!
//! Delegates to [hermit-rs](https://github.com/EBISPOT/hermit-rs), an OWL 2 DL
//! reasoner (hypertableau calculus with validated blocking) built on the same
//! horned-owl object model as owlmake. It covers full SROIQ(D): nominals,
//! cardinality restrictions, role chains, datatypes — so an ontology that needs
//! more than the EL profile still classifies.
//!
//! This module is a thin adapter: it presents the [`DlReasoner`] query API used
//! by `reason`/`explain`/`measure` and the entailment checker, and answers each
//! query with the corresponding hermit-rs reasoner service. hermit-rs is
//! hardcoded to horned-owl's `ArcStr` IRI backing type (its tableau is `Send`),
//! while owlmake standardizes on `RcStr`, so [`DlReasoner::classify`] converts
//! the ontology once at the boundary, rebuilding every component through an
//! `ArcStr` build.
//!
//! Unlike a plain ALC tableau, which can only drop and count the axioms it
//! does not understand, hermit-rs *rejects* ontologies outside OWL 2 DL, e.g.
//! a Self restriction on a non-simple role, instead of silently weakening them.
//! Those errors surface as a panic with the hermit-rs message; every answer the
//! reasoner does give is sound and complete.

use std::sync::OnceLock;

use horned_owl::model::{ArcStr, Build, Class, ClassExpression, RcStr};
use horned_owl::ontology::set::SetOntology;

// The direct RcStr→ArcStr AST transform (`to_arc`) names a lot of horned-owl
// model types; alias the module rather than glob-import to keep them grouped.
use horned_owl::model as ho;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use hermit_rs::hierarchy::{ClassificationProgressMonitor, Hierarchy};
use hermit_rs::reasoner as hermit;

use crate::model::Model;

/// Shared progress counters between the classification thread (which updates them
/// through [`ProgressMonitor`]) and a heartbeat thread (which renders them).
/// `total == 0` means the per-concept loop hasn't started yet — the classifier is
/// still in its silent setup phase (clausify + automata compile + the
/// consistency-check tableau), which on a large ontology is where most of the
/// time goes, so the heartbeat shows a plain elapsed timer there rather than a
/// frozen 0% bar.
#[derive(Default)]
struct ProgressState {
    done: AtomicUsize,
    total: AtomicUsize,
    /// Current setup phase, as a small code the heartbeat maps to a label:
    /// 0 = starting, 1 = clausify, 2 = compile, 3 = consistency, 4 = classify.
    phase: AtomicUsize,
}

/// Map a hermit-rs phase label to a [`ProgressState::phase`] code.
fn phase_code(phase: &str) -> usize {
    match phase {
        "clausify" => 1,
        "compile" => 2,
        "consistency" => 3,
        "classify" => 4,
        _ => 0,
    }
}

/// Human-readable description of a [`ProgressState::phase`] code.
fn phase_label(code: usize) -> &'static str {
    match code {
        1 => "clausifying axioms",
        2 => "compiling clauses + automata",
        3 => "consistency check",
        4 => "classifying",
        _ => "preparing",
    }
}

/// Forwards hermit-rs's `classification_progress(done, total)` callbacks into the
/// shared [`ProgressState`]. Cheap (atomic stores only) — the heartbeat thread
/// does the throttled rendering, so the per-concept loop is never slowed by I/O.
struct ProgressMonitor {
    state: Arc<ProgressState>,
}

impl ClassificationProgressMonitor<Class<ArcStr>> for ProgressMonitor {
    fn element_classified(&mut self, _element: &Class<ArcStr>) {}

    fn classification_progress(&mut self, done: usize, total: usize) {
        self.state.total.store(total, Ordering::Relaxed);
        self.state.done.store(done, Ordering::Relaxed);
    }

    fn classification_phase(&mut self, phase: &str) {
        self.state.phase.store(phase_code(phase), Ordering::Relaxed);
    }
}

/// Run `classify` under a live terminal progress display. A heartbeat thread
/// renders elapsed time through the silent setup phase and a `done/total` bar
/// with ETA once per-concept classification begins; the final summary line is
/// printed when classification returns. Falls back to a plain classify when
/// progress is disabled.
fn classify_with_progress(ont: &SetOntology<ArcStr>) -> Hierarchy<Class<ArcStr>> {
    if !crate::progress::enabled() {
        return hermit::classify(ont).unwrap_or_else(|e| die(e));
    }

    let state = Arc::new(ProgressState::default());
    let finished = Arc::new(AtomicBool::new(false));
    let hb = {
        let (state, finished) = (state.clone(), finished.clone());
        std::thread::spawn(move || {
            let mut bar = crate::progress::Progress::new("reason", 0);
            let start = crate::time::Instant::now();
            while !finished.load(Ordering::Relaxed) {
                let el = start.elapsed().as_secs_f64();
                let total = state.total.load(Ordering::Relaxed);
                let done = state.done.load(Ordering::Relaxed);
                if total == 0 {
                    // Setup phase (no per-concept count yet): name the current
                    // step so the long, otherwise-silent clausify/compile/
                    // consistency work is visible, with a live elapsed timer.
                    let phase = phase_label(state.phase.load(Ordering::Relaxed));
                    bar.line(&format!(
                        "reason: hermit-rs {phase}…  {}",
                        crate::progress::fmt_hms(el),
                    ));
                } else {
                    let frac = (done as f64 / total as f64).min(1.0);
                    let w = 20usize;
                    let filled = (frac * w as f64).round() as usize;
                    let eta = if done > 0 {
                        el * (total as f64 - done as f64) / done as f64
                    } else {
                        0.0
                    };
                    bar.line(&format!(
                        "reason: hermit-rs  [{}{}] {:>3.0}%  {}/{} classes  {}  ~{} left",
                        "#".repeat(filled),
                        "-".repeat(w - filled),
                        frac * 100.0,
                        done,
                        total,
                        crate::progress::fmt_hms(el),
                        crate::progress::fmt_hms(eta),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            let el = crate::progress::fmt_hms(start.elapsed().as_secs_f64());
            let n = state.total.load(Ordering::Relaxed).max(state.done.load(Ordering::Relaxed));
            bar.finish_line(&format!("reason: hermit-rs classified {n} classes in {el}"));
        })
    };

    let mut monitor = ProgressMonitor { state };
    let result = hermit::classify_with_monitor(ont, &mut monitor);
    finished.store(true, Ordering::Relaxed);
    let _ = hb.join();
    result.unwrap_or_else(|e| die(e))
}

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The hermit-rs-backed DL reasoner over a snapshot of a [`Model`].
///
/// Construction ([`classify`](DlReasoner::classify)) only converts the
/// ontology; the expensive work runs lazily per query and is cached:
/// consistency and the class hierarchy each compute at most once, and
/// [`is_subsumed`](DlReasoner::is_subsumed) is answered with a single
/// satisfiability test, so it never pays for a full classification.
/// [`unsatisfiable`](DlReasoner::unsatisfiable) reads the ⊥-node off the
/// hierarchy and therefore does classify (once).
pub struct DlReasoner {
    ont: SetOntology<ArcStr>,
    consistent: OnceLock<bool>,
    hierarchy: OnceLock<Hierarchy<Class<ArcStr>>>,
}

/// hermit-rs reports unsupported/out-of-DL input as `Err(String)`. The
/// `DlReasoner` API is infallible, so fail loudly here rather than return a
/// silently-unsound answer.
fn die(e: String) -> ! {
    panic!("dl (hermit-rs): {e}")
}

/// Convert owlmake's `SetOntology<RcStr>` into the `SetOntology<ArcStr>`
/// hermit-rs requires by a **direct AST transform**: walk every component and
/// rebuild it through an `ArcStr` [`Build`], re-interning each IRI/anonymous
/// individual as it goes. horned-owl has no cross-backing retyping of its own
/// (its visitors are fixed to a single backing type `A`), so this maps the
/// object model component-by-component.
///
/// The copy is structural: rebuilding the components directly yields a
/// logically identical ontology without parsing the ontology a second time,
/// which on an ontology the size of EFO would cost ~15 s of pure overhead
/// before any reasoning starts.
fn to_arc(model: &Model) -> SetOntology<ArcStr> {
    let cv = ArcConv {
        build: Build::new_arc(),
    };
    model
        .ont
        .iter()
        // DocIRI is horned-owl bookkeeping (where the document was loaded
        // from), not an OWL axiom, and an OFN round trip drops it — so drop it
        // here too and reason over the same axiom set either way.
        .filter(|ac| !matches!(ac.component, ho::Component::DocIRI(_)))
        .map(|ac| cv.annotated_component(ac))
        .collect()
}

/// Re-interns an `RcStr`-backed object model into an `ArcStr`-backed one.
///
/// Every `IRI`/anonymous individual is rebuilt through `build`, which dedups
/// identical strings, so the result shares backing allocations exactly as a
/// freshly parsed ontology would.
struct ArcConv {
    build: Build<ArcStr>,
}

// The transform is exhaustively mechanical: one method per horned-owl model
// type, each reconstructing the `ArcStr` value from the `RcStr` one. Using the
// real horned-owl type names (rather than glob-importing) keeps every variant
// checked by the compiler — if horned-owl adds a component, this stops building.
impl ArcConv {
    fn iri(&self, i: &ho::IRI<RcStr>) -> ho::IRI<ArcStr> {
        self.build.iri(&**i)
    }

    fn class(&self, c: &ho::Class<RcStr>) -> ho::Class<ArcStr> {
        ho::Class(self.iri(&c.0))
    }
    fn datatype(&self, d: &ho::Datatype<RcStr>) -> ho::Datatype<ArcStr> {
        ho::Datatype(self.iri(&d.0))
    }
    fn object_property(&self, p: &ho::ObjectProperty<RcStr>) -> ho::ObjectProperty<ArcStr> {
        ho::ObjectProperty(self.iri(&p.0))
    }
    fn data_property(&self, p: &ho::DataProperty<RcStr>) -> ho::DataProperty<ArcStr> {
        ho::DataProperty(self.iri(&p.0))
    }
    fn annotation_property(
        &self,
        p: &ho::AnnotationProperty<RcStr>,
    ) -> ho::AnnotationProperty<ArcStr> {
        ho::AnnotationProperty(self.iri(&p.0))
    }
    fn named_individual(&self, n: &ho::NamedIndividual<RcStr>) -> ho::NamedIndividual<ArcStr> {
        ho::NamedIndividual(self.iri(&n.0))
    }
    fn variable(&self, v: &ho::Variable<RcStr>) -> ho::Variable<ArcStr> {
        ho::Variable(self.iri(&v.0))
    }

    fn anon(&self, a: &ho::AnonymousIndividual<RcStr>) -> ho::AnonymousIndividual<ArcStr> {
        self.build.anon(&**a)
    }

    fn individual(&self, i: &ho::Individual<RcStr>) -> ho::Individual<ArcStr> {
        match i {
            ho::Individual::Anonymous(a) => ho::Individual::Anonymous(self.anon(a)),
            ho::Individual::Named(n) => ho::Individual::Named(self.named_individual(n)),
        }
    }

    fn literal(&self, l: &ho::Literal<RcStr>) -> ho::Literal<ArcStr> {
        match l {
            ho::Literal::Simple { literal } => ho::Literal::Simple {
                literal: literal.clone(),
            },
            ho::Literal::Language { literal, lang } => ho::Literal::Language {
                literal: literal.clone(),
                lang: lang.clone(),
            },
            ho::Literal::Datatype {
                literal,
                datatype_iri,
            } => ho::Literal::Datatype {
                literal: literal.clone(),
                datatype_iri: self.iri(datatype_iri),
            },
        }
    }

    fn annotation_subject(&self, s: &ho::AnnotationSubject<RcStr>) -> ho::AnnotationSubject<ArcStr> {
        match s {
            ho::AnnotationSubject::IRI(i) => ho::AnnotationSubject::IRI(self.iri(i)),
            ho::AnnotationSubject::AnonymousIndividual(a) => {
                ho::AnnotationSubject::AnonymousIndividual(self.anon(a))
            }
        }
    }

    fn annotation_value(&self, v: &ho::AnnotationValue<RcStr>) -> ho::AnnotationValue<ArcStr> {
        match v {
            ho::AnnotationValue::Literal(l) => ho::AnnotationValue::Literal(self.literal(l)),
            ho::AnnotationValue::IRI(i) => ho::AnnotationValue::IRI(self.iri(i)),
            ho::AnnotationValue::AnonymousIndividual(a) => {
                ho::AnnotationValue::AnonymousIndividual(self.anon(a))
            }
        }
    }

    fn annotation(&self, a: &ho::Annotation<RcStr>) -> ho::Annotation<ArcStr> {
        ho::Annotation { ann: Default::default(),
            ap: self.annotation_property(&a.ap),
            av: self.annotation_value(&a.av),
        }
    }

    fn object_property_expression(
        &self,
        e: &ho::ObjectPropertyExpression<RcStr>,
    ) -> ho::ObjectPropertyExpression<ArcStr> {
        match e {
            ho::ObjectPropertyExpression::ObjectProperty(p) => {
                ho::ObjectPropertyExpression::ObjectProperty(self.object_property(p))
            }
            ho::ObjectPropertyExpression::InverseObjectProperty(p) => {
                ho::ObjectPropertyExpression::InverseObjectProperty(self.object_property(p))
            }
        }
    }

    fn sub_object_property_expression(
        &self,
        e: &ho::SubObjectPropertyExpression<RcStr>,
    ) -> ho::SubObjectPropertyExpression<ArcStr> {
        match e {
            ho::SubObjectPropertyExpression::ObjectPropertyChain(v) => {
                ho::SubObjectPropertyExpression::ObjectPropertyChain(
                    v.iter().map(|e| self.object_property_expression(e)).collect(),
                )
            }
            ho::SubObjectPropertyExpression::ObjectPropertyExpression(e) => {
                ho::SubObjectPropertyExpression::ObjectPropertyExpression(
                    self.object_property_expression(e),
                )
            }
        }
    }

    fn property_expression(
        &self,
        e: &ho::PropertyExpression<RcStr>,
    ) -> ho::PropertyExpression<ArcStr> {
        match e {
            ho::PropertyExpression::ObjectPropertyExpression(e) => {
                ho::PropertyExpression::ObjectPropertyExpression(
                    self.object_property_expression(e),
                )
            }
            ho::PropertyExpression::DataProperty(p) => {
                ho::PropertyExpression::DataProperty(self.data_property(p))
            }
            ho::PropertyExpression::AnnotationProperty(p) => {
                ho::PropertyExpression::AnnotationProperty(self.annotation_property(p))
            }
        }
    }

    fn facet_restriction(
        &self,
        f: &ho::FacetRestriction<RcStr>,
    ) -> ho::FacetRestriction<ArcStr> {
        ho::FacetRestriction {
            f: f.f.clone(),
            l: self.literal(&f.l),
        }
    }

    fn data_range(&self, r: &ho::DataRange<RcStr>) -> ho::DataRange<ArcStr> {
        match r {
            ho::DataRange::Datatype(d) => ho::DataRange::Datatype(self.datatype(d)),
            ho::DataRange::DataIntersectionOf(v) => {
                ho::DataRange::DataIntersectionOf(v.iter().map(|r| self.data_range(r)).collect())
            }
            ho::DataRange::DataUnionOf(v) => {
                ho::DataRange::DataUnionOf(v.iter().map(|r| self.data_range(r)).collect())
            }
            ho::DataRange::DataComplementOf(b) => {
                ho::DataRange::DataComplementOf(Box::new(self.data_range(b)))
            }
            ho::DataRange::DataOneOf(v) => {
                ho::DataRange::DataOneOf(v.iter().map(|l| self.literal(l)).collect())
            }
            ho::DataRange::DatatypeRestriction(d, v) => ho::DataRange::DatatypeRestriction(
                self.datatype(d),
                v.iter().map(|f| self.facet_restriction(f)).collect(),
            ),
        }
    }

    fn class_expression(&self, c: &ho::ClassExpression<RcStr>) -> ho::ClassExpression<ArcStr> {
        use ho::ClassExpression as Ce;
        match c {
            Ce::Class(c) => Ce::Class(self.class(c)),
            Ce::ObjectIntersectionOf(v) => {
                Ce::ObjectIntersectionOf(v.iter().map(|c| self.class_expression(c)).collect())
            }
            Ce::ObjectUnionOf(v) => {
                Ce::ObjectUnionOf(v.iter().map(|c| self.class_expression(c)).collect())
            }
            Ce::ObjectComplementOf(b) => {
                Ce::ObjectComplementOf(Box::new(self.class_expression(b)))
            }
            Ce::ObjectOneOf(v) => {
                Ce::ObjectOneOf(v.iter().map(|i| self.individual(i)).collect())
            }
            Ce::ObjectSomeValuesFrom { ope, bce } => Ce::ObjectSomeValuesFrom {
                ope: self.object_property_expression(ope),
                bce: Box::new(self.class_expression(bce)),
            },
            Ce::ObjectAllValuesFrom { ope, bce } => Ce::ObjectAllValuesFrom {
                ope: self.object_property_expression(ope),
                bce: Box::new(self.class_expression(bce)),
            },
            Ce::ObjectHasValue { ope, i } => Ce::ObjectHasValue {
                ope: self.object_property_expression(ope),
                i: self.individual(i),
            },
            Ce::ObjectHasSelf(e) => Ce::ObjectHasSelf(self.object_property_expression(e)),
            Ce::ObjectMinCardinality { n, ope, bce } => Ce::ObjectMinCardinality {
                n: *n,
                ope: self.object_property_expression(ope),
                bce: Box::new(self.class_expression(bce)),
            },
            Ce::ObjectMaxCardinality { n, ope, bce } => Ce::ObjectMaxCardinality {
                n: *n,
                ope: self.object_property_expression(ope),
                bce: Box::new(self.class_expression(bce)),
            },
            Ce::ObjectExactCardinality { n, ope, bce } => Ce::ObjectExactCardinality {
                n: *n,
                ope: self.object_property_expression(ope),
                bce: Box::new(self.class_expression(bce)),
            },
            Ce::DataSomeValuesFrom { dp, dr } => Ce::DataSomeValuesFrom {
                dp: self.data_property(dp),
                dr: self.data_range(dr),
            },
            Ce::DataAllValuesFrom { dp, dr } => Ce::DataAllValuesFrom {
                dp: self.data_property(dp),
                dr: self.data_range(dr),
            },
            Ce::DataHasValue { dp, l } => Ce::DataHasValue {
                dp: self.data_property(dp),
                l: self.literal(l),
            },
            Ce::DataMinCardinality { n, dp, dr } => Ce::DataMinCardinality {
                n: *n,
                dp: self.data_property(dp),
                dr: self.data_range(dr),
            },
            Ce::DataMaxCardinality { n, dp, dr } => Ce::DataMaxCardinality {
                n: *n,
                dp: self.data_property(dp),
                dr: self.data_range(dr),
            },
            Ce::DataExactCardinality { n, dp, dr } => Ce::DataExactCardinality {
                n: *n,
                dp: self.data_property(dp),
                dr: self.data_range(dr),
            },
        }
    }

    fn iarg(&self, a: &ho::IArgument<RcStr>) -> ho::IArgument<ArcStr> {
        match a {
            ho::IArgument::Individual(i) => ho::IArgument::Individual(self.individual(i)),
            ho::IArgument::Variable(v) => ho::IArgument::Variable(self.variable(v)),
        }
    }

    fn darg(&self, a: &ho::DArgument<RcStr>) -> ho::DArgument<ArcStr> {
        match a {
            ho::DArgument::Literal(l) => ho::DArgument::Literal(self.literal(l)),
            ho::DArgument::Variable(v) => ho::DArgument::Variable(self.variable(v)),
        }
    }

    fn atom(&self, a: &ho::Atom<RcStr>) -> ho::Atom<ArcStr> {
        match a {
            ho::Atom::BuiltInAtom { pred, args } => ho::Atom::BuiltInAtom {
                pred: self.iri(pred),
                args: args.iter().map(|a| self.darg(a)).collect(),
            },
            ho::Atom::ClassAtom { pred, arg } => ho::Atom::ClassAtom {
                pred: self.class_expression(pred),
                arg: self.iarg(arg),
            },
            ho::Atom::DataPropertyAtom { pred, args } => ho::Atom::DataPropertyAtom {
                pred: self.data_property(pred),
                args: (self.darg(&args.0), self.darg(&args.1)),
            },
            ho::Atom::DataRangeAtom { pred, arg } => ho::Atom::DataRangeAtom {
                pred: self.data_range(pred),
                arg: self.darg(arg),
            },
            ho::Atom::DifferentIndividualsAtom(a, b) => {
                ho::Atom::DifferentIndividualsAtom(self.iarg(a), self.iarg(b))
            }
            ho::Atom::ObjectPropertyAtom { pred, args } => ho::Atom::ObjectPropertyAtom {
                pred: self.object_property_expression(pred),
                args: (self.iarg(&args.0), self.iarg(&args.1)),
            },
            ho::Atom::SameIndividualAtom(a, b) => {
                ho::Atom::SameIndividualAtom(self.iarg(a), self.iarg(b))
            }
        }
    }

    fn component(&self, c: &ho::Component<RcStr>) -> ho::Component<ArcStr> {
        use ho::Component as C;
        match c {
            C::OntologyID(x) => C::OntologyID(ho::OntologyID {
                iri: x.iri.as_ref().map(|i| self.iri(i)),
                viri: x.viri.as_ref().map(|i| self.iri(i)),
            }),
            C::DocIRI(x) => C::DocIRI(ho::DocIRI(self.iri(&x.0))),
            C::OntologyAnnotation(x) => C::OntologyAnnotation(ho::OntologyAnnotation(
                self.annotation(&x.0),
            )),
            C::Import(x) => C::Import(ho::Import(self.iri(&x.0))),
            C::DeclareClass(x) => C::DeclareClass(ho::DeclareClass(self.class(&x.0))),
            C::DeclareObjectProperty(x) => {
                C::DeclareObjectProperty(ho::DeclareObjectProperty(self.object_property(&x.0)))
            }
            C::DeclareAnnotationProperty(x) => C::DeclareAnnotationProperty(
                ho::DeclareAnnotationProperty(self.annotation_property(&x.0)),
            ),
            C::DeclareDataProperty(x) => {
                C::DeclareDataProperty(ho::DeclareDataProperty(self.data_property(&x.0)))
            }
            C::DeclareNamedIndividual(x) => C::DeclareNamedIndividual(
                ho::DeclareNamedIndividual(self.named_individual(&x.0)),
            ),
            C::DeclareDatatype(x) => {
                C::DeclareDatatype(ho::DeclareDatatype(self.datatype(&x.0)))
            }
            C::SubClassOf(x) => C::SubClassOf(ho::SubClassOf {
                sup: self.class_expression(&x.sup),
                sub: self.class_expression(&x.sub),
            }),
            C::EquivalentClasses(x) => C::EquivalentClasses(ho::EquivalentClasses(
                x.0.iter().map(|c| self.class_expression(c)).collect(),
            )),
            C::DisjointClasses(x) => C::DisjointClasses(ho::DisjointClasses(
                x.0.iter().map(|c| self.class_expression(c)).collect(),
            )),
            C::DisjointUnion(x) => C::DisjointUnion(ho::DisjointUnion(
                self.class(&x.0),
                x.1.iter().map(|c| self.class_expression(c)).collect(),
            )),
            C::SubObjectPropertyOf(x) => C::SubObjectPropertyOf(ho::SubObjectPropertyOf {
                sup: self.object_property_expression(&x.sup),
                sub: self.sub_object_property_expression(&x.sub),
            }),
            C::EquivalentObjectProperties(x) => {
                C::EquivalentObjectProperties(ho::EquivalentObjectProperties(
                    x.0.iter().map(|e| self.object_property_expression(e)).collect(),
                ))
            }
            C::DisjointObjectProperties(x) => {
                C::DisjointObjectProperties(ho::DisjointObjectProperties(
                    x.0.iter().map(|e| self.object_property_expression(e)).collect(),
                ))
            }
            C::InverseObjectProperties(x) => C::InverseObjectProperties(
                ho::InverseObjectProperties(
                    self.object_property_expression(&x.0),
                    self.object_property_expression(&x.1),
                ),
            ),
            C::ObjectPropertyDomain(x) => C::ObjectPropertyDomain(ho::ObjectPropertyDomain {
                ope: self.object_property_expression(&x.ope),
                ce: self.class_expression(&x.ce),
            }),
            C::ObjectPropertyRange(x) => C::ObjectPropertyRange(ho::ObjectPropertyRange {
                ope: self.object_property_expression(&x.ope),
                ce: self.class_expression(&x.ce),
            }),
            C::FunctionalObjectProperty(x) => C::FunctionalObjectProperty(
                ho::FunctionalObjectProperty(self.object_property_expression(&x.0)),
            ),
            C::InverseFunctionalObjectProperty(x) => C::InverseFunctionalObjectProperty(
                ho::InverseFunctionalObjectProperty(self.object_property_expression(&x.0)),
            ),
            C::ReflexiveObjectProperty(x) => C::ReflexiveObjectProperty(
                ho::ReflexiveObjectProperty(self.object_property_expression(&x.0)),
            ),
            C::IrreflexiveObjectProperty(x) => C::IrreflexiveObjectProperty(
                ho::IrreflexiveObjectProperty(self.object_property_expression(&x.0)),
            ),
            C::SymmetricObjectProperty(x) => C::SymmetricObjectProperty(
                ho::SymmetricObjectProperty(self.object_property_expression(&x.0)),
            ),
            C::AsymmetricObjectProperty(x) => C::AsymmetricObjectProperty(
                ho::AsymmetricObjectProperty(self.object_property_expression(&x.0)),
            ),
            C::TransitiveObjectProperty(x) => C::TransitiveObjectProperty(
                ho::TransitiveObjectProperty(self.object_property_expression(&x.0)),
            ),
            C::SubDataPropertyOf(x) => C::SubDataPropertyOf(ho::SubDataPropertyOf {
                sup: self.data_property(&x.sup),
                sub: self.data_property(&x.sub),
            }),
            C::EquivalentDataProperties(x) => {
                C::EquivalentDataProperties(ho::EquivalentDataProperties(
                    x.0.iter().map(|p| self.data_property(p)).collect(),
                ))
            }
            C::DisjointDataProperties(x) => {
                C::DisjointDataProperties(ho::DisjointDataProperties(
                    x.0.iter().map(|p| self.data_property(p)).collect(),
                ))
            }
            C::DataPropertyDomain(x) => C::DataPropertyDomain(ho::DataPropertyDomain {
                dp: self.data_property(&x.dp),
                ce: self.class_expression(&x.ce),
            }),
            C::DataPropertyRange(x) => C::DataPropertyRange(ho::DataPropertyRange {
                dp: self.data_property(&x.dp),
                dr: self.data_range(&x.dr),
            }),
            C::FunctionalDataProperty(x) => C::FunctionalDataProperty(
                ho::FunctionalDataProperty(self.data_property(&x.0)),
            ),
            C::DatatypeDefinition(x) => C::DatatypeDefinition(ho::DatatypeDefinition {
                kind: self.datatype(&x.kind),
                range: self.data_range(&x.range),
            }),
            C::HasKey(x) => C::HasKey(ho::HasKey {
                ce: self.class_expression(&x.ce),
                vpe: x.vpe.iter().map(|p| self.property_expression(p)).collect(),
            }),
            C::SameIndividual(x) => C::SameIndividual(ho::SameIndividual(
                x.0.iter().map(|i| self.individual(i)).collect(),
            )),
            C::DifferentIndividuals(x) => C::DifferentIndividuals(ho::DifferentIndividuals(
                x.0.iter().map(|i| self.individual(i)).collect(),
            )),
            C::ClassAssertion(x) => C::ClassAssertion(ho::ClassAssertion {
                ce: self.class_expression(&x.ce),
                i: self.individual(&x.i),
            }),
            C::ObjectPropertyAssertion(x) => {
                C::ObjectPropertyAssertion(ho::ObjectPropertyAssertion {
                    ope: self.object_property_expression(&x.ope),
                    from: self.individual(&x.from),
                    to: self.individual(&x.to),
                })
            }
            C::NegativeObjectPropertyAssertion(x) => {
                C::NegativeObjectPropertyAssertion(ho::NegativeObjectPropertyAssertion {
                    ope: self.object_property_expression(&x.ope),
                    from: self.individual(&x.from),
                    to: self.individual(&x.to),
                })
            }
            C::DataPropertyAssertion(x) => {
                C::DataPropertyAssertion(ho::DataPropertyAssertion {
                    dp: self.data_property(&x.dp),
                    from: self.individual(&x.from),
                    to: self.literal(&x.to),
                })
            }
            C::NegativeDataPropertyAssertion(x) => {
                C::NegativeDataPropertyAssertion(ho::NegativeDataPropertyAssertion {
                    dp: self.data_property(&x.dp),
                    from: self.individual(&x.from),
                    to: self.literal(&x.to),
                })
            }
            C::AnnotationAssertion(x) => C::AnnotationAssertion(ho::AnnotationAssertion {
                subject: self.annotation_subject(&x.subject),
                ann: self.annotation(&x.ann),
            }),
            C::SubAnnotationPropertyOf(x) => {
                C::SubAnnotationPropertyOf(ho::SubAnnotationPropertyOf {
                    sup: self.annotation_property(&x.sup),
                    sub: self.annotation_property(&x.sub),
                })
            }
            C::AnnotationPropertyDomain(x) => {
                C::AnnotationPropertyDomain(ho::AnnotationPropertyDomain {
                    ap: self.annotation_property(&x.ap),
                    iri: self.iri(&x.iri),
                })
            }
            C::AnnotationPropertyRange(x) => {
                C::AnnotationPropertyRange(ho::AnnotationPropertyRange {
                    ap: self.annotation_property(&x.ap),
                    iri: self.iri(&x.iri),
                })
            }
            C::Rule(x) => C::Rule(ho::Rule {
                head: x.head.iter().map(|a| self.atom(a)).collect(),
                body: x.body.iter().map(|a| self.atom(a)).collect(),
            }),
        }
    }

    fn annotated_component(
        &self,
        ac: &ho::AnnotatedComponent<RcStr>,
    ) -> ho::AnnotatedComponent<ArcStr> {
        ho::AnnotatedComponent {
            component: self.component(&ac.component),
            ann: ac.ann.iter().map(|a| self.annotation(a)).collect(),
        }
    }
}

/// A queryable IRI: a real named class, not ⊤/⊥ or a clausification helper.
fn is_named(iri: &str) -> bool {
    iri != OWL_THING && iri != OWL_NOTHING && !iri.starts_with("internal:")
}

impl DlReasoner {
    /// Snapshot `model` for DL reasoning. Cheap: the classification itself
    /// runs lazily on the first query that needs it.
    pub fn classify(model: &Model) -> DlReasoner {
        // The RcStr→ArcStr conversion rebuilds every component of the whole
        // ontology, which on a large input takes seconds before any reasoning
        // even starts; tick a heartbeat so it isn't a silent gap.
        let ont = {
            let _hb = crate::progress::Heartbeat::start("reason: hermit-rs converting model");
            to_arc(model)
        };
        DlReasoner {
            ont,
            consistent: OnceLock::new(),
            hierarchy: OnceLock::new(),
        }
    }

    /// The classified taxonomy (computed once). hermit-rs returns its
    /// single-node "empty" hierarchy for an inconsistent ontology, where every
    /// class sits in the collapsed ⊤/⊥ node.
    fn hierarchy(&self) -> &Hierarchy<Class<ArcStr>> {
        self.hierarchy
            .get_or_init(|| classify_with_progress(&self.ont))
    }

    /// Whether `sub ⊑ sup` is entailed, for two named-class IRIs — a single
    /// tableau test (`sub ⊓ ¬sup` unsatisfiable), no classification.
    pub fn is_subsumed(&self, sub: &str, sup: &str) -> bool {
        let build = Build::new_arc();
        hermit::is_subsumed_by(
            &self.ont,
            ClassExpression::Class(build.class(sub)),
            ClassExpression::Class(build.class(sup)),
        )
        .unwrap_or_else(|e| die(e))
    }

    pub fn is_consistent(&self) -> bool {
        *self.consistent.get_or_init(|| {
            // If the class hierarchy has already been computed, consistency is a
            // free read-off rather than a second consistency tableau: hermit
            // collapses ⊤ and ⊥ into one node (`empty_hierarchy`) exactly when the
            // ontology is inconsistent, so `top != bottom` iff consistent. The
            // dedicated `is_ontology_consistent` check is kept for callers that
            // want consistency *without* paying for a full classification (e.g.
            // `release`, `ubergraph`, `entail`), where the hierarchy isn't cached.
            if let Some(h) = self.hierarchy.get() {
                return h.top_node() != h.bottom_node();
            }
            let _hb = crate::progress::Heartbeat::start("reason: hermit-rs consistency check");
            hermit::is_ontology_consistent(&self.ont).unwrap_or_else(|e| die(e))
        })
    }

    /// IRIs of the named classes that are unsatisfiable (≡ `owl:Nothing`),
    /// sorted. Read straight off the classified taxonomy: the unsatisfiable
    /// named classes are exactly the members of the hierarchy's ⊥-node, the
    /// classes the reasoner found equivalent to `owl:Nothing` — one
    /// classification, not a per-class satisfiability test. (Asking per class
    /// instead means a full tableau consistency check each time — O(n) model
    /// builds, catastrophic on ontologies the size of EFO; the single
    /// classification already computes this set.) In an inconsistent ontology
    /// hermit-rs collapses every class into the ⊥-node, so every named class is
    /// reported — which is right, since nothing is satisfiable there.
    pub fn unsatisfiable(&self) -> Vec<String> {
        let h = self.hierarchy();
        let bottom = h.bottom_node();
        let mut out: Vec<String> = h
            .node(bottom)
            .equivalent_elements()
            .iter()
            .map(|c| c.0.to_string())
            .filter(|iri| is_named(iri))
            .collect();
        out.sort();
        out
    }

    /// Direct (transitively-reduced) subsumptions between satisfiable named
    /// classes, read off the classified taxonomy: for each class, the other
    /// members of its equivalence node (mutual subsumers) plus every member of
    /// its node's direct parent nodes. Edges to ⊤ and the ⊥-node (the
    /// unsatisfiable classes) are not reported.
    pub fn direct_subsumptions(&self) -> Vec<(String, String)> {
        let h = self.hierarchy();
        let bottom = h.bottom_node();
        let mut out: Vec<(String, String)> = Vec::new();
        for node_ref in h.all_nodes() {
            if node_ref == bottom {
                continue;
            }
            let node = h.node(node_ref);
            let members: Vec<String> = node
                .equivalent_elements()
                .iter()
                .map(|c| c.0.to_string())
                .filter(|iri| is_named(iri))
                .collect();
            let parent_members: Vec<String> = node
                .parent_nodes()
                .iter()
                .flat_map(|&p| h.node(p).equivalent_elements().iter())
                .map(|c| c.0.to_string())
                .filter(|iri| is_named(iri))
                .collect();
            for a in &members {
                for m in &members {
                    if m != a {
                        out.push((a.clone(), m.clone()));
                    }
                }
                for p in &parent_members {
                    out.push((a.clone(), p.clone()));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The inferred equivalent-class pairs (`a ≡ b`, `a < b` by IRI): the pairs
    /// drawn from each taxonomy node's equivalence set. Mirrors
    /// [`super::el::Reasoner::equivalent_class_pairs`] so `reason`'s
    /// `--equivalent-classes-allowed` policy reads the same shape from every
    /// backend, without materialising the O(n·ancestors) full closure. The
    /// ⊥-node is skipped: those classes are unsatisfiable, which `reason`
    /// reports as incoherence rather than as an equivalence.
    pub fn equivalent_class_pairs(&self) -> Vec<(String, String)> {
        let h = self.hierarchy();
        let bottom = h.bottom_node();
        let mut out: Vec<(String, String)> = Vec::new();
        for node_ref in h.all_nodes() {
            if node_ref == bottom {
                continue;
            }
            let members: Vec<String> = h
                .node(node_ref)
                .equivalent_elements()
                .iter()
                .map(|c| c.0.to_string())
                .filter(|iri| is_named(iri))
                .collect();
            for a in &members {
                for b in &members {
                    if a < b {
                        out.push((a.clone(), b.clone()));
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The full subsumption closure over named classes (every entailed
    /// `a ⊑ b`, `a ≠ b`, excluding ⊤/⊥ themselves): equivalence-node members
    /// plus all ancestor-node members for each satisfiable class, and — since
    /// an unsatisfiable class is ≡ ⊥ — every other named class for each
    /// unsatisfiable one.
    pub fn all_subsumptions(&self) -> Vec<(String, String)> {
        let h = self.hierarchy();
        let bottom = h.bottom_node();
        let all_named: Vec<String> = h
            .all_elements()
            .map(|c| c.0.to_string())
            .filter(|iri| is_named(iri))
            .collect();
        let mut out: Vec<(String, String)> = Vec::new();
        for node_ref in h.all_nodes() {
            let node = h.node(node_ref);
            let members: Vec<String> = node
                .equivalent_elements()
                .iter()
                .map(|c| c.0.to_string())
                .filter(|iri| is_named(iri))
                .collect();
            if node_ref == bottom {
                // ⊥-node classes are subsumed by every named class.
                for a in &members {
                    for b in &all_named {
                        if b != a {
                            out.push((a.clone(), b.clone()));
                        }
                    }
                }
                continue;
            }
            let mut ancestors = h.ancestor_nodes(node_ref);
            ancestors.remove(&node_ref);
            let ancestor_members: Vec<String> = ancestors
                .iter()
                .flat_map(|&n| h.node(n).equivalent_elements().iter())
                .map(|c| c.0.to_string())
                .filter(|iri| is_named(iri))
                .collect();
            for a in &members {
                for m in &members {
                    if m != a {
                        out.push((a.clone(), m.clone()));
                    }
                }
                for s in &ancestor_members {
                    out.push((a.clone(), s.clone()));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}
