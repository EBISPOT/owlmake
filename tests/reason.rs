//! Correctness tests for the EL reasoner against hand-built ontologies whose
//! entailments are known from the description-logic literature.

use horned_owl::model::{
    Build, ClassExpression as CE, Component, ObjectPropertyExpression as OPE,
    SubObjectPropertyExpression as SOPE,
};
use horned_owl::ontology::set::SetOntology;
use horned_owl::model::MutableOntology;

use owlmake::model::Model;
use owlmake::reason::{Reasoner, WhelkClassification};

const NS: &str = "http://example.org/";

fn model_from(components: Vec<Component<horned_owl::model::RcStr>>) -> Model {
    let mut ont: SetOntology<_> = SetOntology::new();
    for c in components {
        ont.insert(c);
    }
    Model::from_parts(ont, owlmake::model::default_prefixes())
}

#[test]
fn subsumption_is_transitive() {
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });

    let m = model_from(vec![sub(c("A"), c("B")), sub(c("B"), c("D"))]);
    let r = Reasoner::classify(&m);
    assert!(r.is_subsumed(&format!("{NS}A"), &format!("{NS}D")), "A ⊑ D via transitivity");
    assert!(r.is_consistent());
}

#[test]
fn conjunction_and_existential_with_role_chain() {
    // The canonical EL classification example:
    //   Endocardium ⊑ Tissue
    //   Endocardium ⊑ ∃part_of.HeartWall
    //   HeartWall   ⊑ ∃part_of.Heart
    //   part_of ∘ part_of ⊑ part_of           (transitive)
    //   Tissue ⊓ ∃part_of.Heart ⊑ HeartTissue
    // Entails: Endocardium ⊑ HeartTissue
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let part_of = b.object_property(format!("{NS}part_of"));
    let some = |p: &horned_owl::model::ObjectProperty<_>, filler: CE<_>| CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(p.clone()),
        bce: Box::new(filler),
    };
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });

    let m = model_from(vec![
        sub(c("Endocardium"), c("Tissue")),
        sub(c("Endocardium"), some(&part_of, c("HeartWall"))),
        sub(c("HeartWall"), some(&part_of, c("Heart"))),
        Component::SubObjectPropertyOf(horned_owl::model::SubObjectPropertyOf {
            sub: SOPE::ObjectPropertyChain(vec![
                OPE::ObjectProperty(part_of.clone()),
                OPE::ObjectProperty(part_of.clone()),
            ]),
            sup: OPE::ObjectProperty(part_of.clone()),
        }),
        sub(
            CE::ObjectIntersectionOf(vec![c("Tissue"), some(&part_of, c("Heart"))]),
            c("HeartTissue"),
        ),
    ]);

    let r = Reasoner::classify(&m);
    assert!(r.is_consistent());
    assert!(
        r.is_subsumed(&format!("{NS}Endocardium"), &format!("{NS}HeartTissue")),
        "Endocardium ⊑ HeartTissue must be entailed via existential + chain + conjunction"
    );
    assert!(r.is_subsumed(&format!("{NS}Endocardium"), &format!("{NS}Tissue")));
}

#[test]
fn equivalence_classification() {
    // A ≡ B; B ⊑ C  ⟹  A ⊑ C and B ⊑ A.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });
    let m = model_from(vec![
        Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![c("A"), c("B")])),
        sub(c("B"), c("C")),
    ]);
    let r = Reasoner::classify(&m);
    assert!(r.is_subsumed(&format!("{NS}A"), &format!("{NS}C")));
    assert!(r.is_subsumed(&format!("{NS}B"), &format!("{NS}A")));
    assert!(r.is_subsumed(&format!("{NS}A"), &format!("{NS}B")));
}

#[test]
fn disjointness_yields_unsatisfiable() {
    // A ⊑ B, A ⊑ C, B disjoint C  ⟹  A unsatisfiable.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });
    let m = model_from(vec![
        sub(c("A"), c("B")),
        sub(c("A"), c("C")),
        Component::DisjointClasses(horned_owl::model::DisjointClasses(vec![c("B"), c("C")])),
    ]);
    let r = Reasoner::classify(&m);
    let unsat = r.unsatisfiable();
    assert!(
        unsat.contains(&format!("{NS}A")),
        "A must be unsatisfiable, got: {unsat:?}"
    );
}

#[test]
fn materialize_existentials_over_transitive_chain() {
    // Endocardium ⊑ ∃part_of.HeartWall, HeartWall ⊑ ∃part_of.Heart,
    // part_of ∘ part_of ⊑ part_of. `∃part_of.Heart` is entailed for Endocardium
    // but is NOT direct — `∃part_of.HeartWall` sits below it along the chain —
    // so materialize keeps the direct edge and does not assert the ancestor.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let part_of = b.object_property(format!("{NS}part_of"));
    let some = |filler: CE<_>| CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(part_of.clone()),
        bce: Box::new(filler),
    };
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });
    let m = model_from(vec![
        sub(c("Endocardium"), some(c("HeartWall"))),
        sub(c("HeartWall"), some(c("Heart"))),
        Component::SubObjectPropertyOf(horned_owl::model::SubObjectPropertyOf {
            sub: SOPE::ObjectPropertyChain(vec![
                OPE::ObjectProperty(part_of.clone()),
                OPE::ObjectProperty(part_of.clone()),
            ]),
            sup: OPE::ObjectProperty(part_of.clone()),
        }),
    ]);
    let props = std::collections::HashSet::new(); // all properties
    let out = owlmake::cmd::materialize::materialize(m, &props);
    let direct = Component::SubClassOf(horned_owl::model::SubClassOf {
        sub: c("Endocardium"),
        sup: some(c("HeartWall")),
    });
    let ancestor = Component::SubClassOf(horned_owl::model::SubClassOf {
        sub: c("Endocardium"),
        sup: some(c("Heart")),
    });
    assert!(
        out.ont.iter().any(|ac| ac.component == direct),
        "the direct edge Endocardium ⊑ part_of some HeartWall survives"
    );
    assert!(
        !out.ont.iter().any(|ac| ac.component == ancestor),
        "the chain ancestor Endocardium ⊑ part_of some Heart is not asserted"
    );
}

#[test]
fn domain_propagation() {
    // domain(r) = D ; A ⊑ ∃r.B  ⟹  A ⊑ D.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r_prop = b.object_property(format!("{NS}r"));
    let m = model_from(vec![
        Component::SubClassOf(horned_owl::model::SubClassOf {
            sub: c("A"),
            sup: CE::ObjectSomeValuesFrom {
                ope: OPE::ObjectProperty(r_prop.clone()),
                bce: Box::new(c("B")),
            },
        }),
        Component::ObjectPropertyDomain(horned_owl::model::ObjectPropertyDomain {
            ope: OPE::ObjectProperty(r_prop.clone()),
            ce: c("D"),
        }),
    ]);
    let r = Reasoner::classify(&m);
    assert!(r.is_subsumed(&format!("{NS}A"), &format!("{NS}D")), "domain should give A ⊑ D");
}

#[test]
fn defined_class_through_existential_subsumed_filler() {
    // D ≡ G ⊓ ∃r.F ; A ⊑ G ; A ⊑ ∃r.F2 ; F2 ⊑ F  ⟹  A ⊑ D.
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let some = |filler: CE<_>| CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(r.clone()),
        bce: Box::new(filler),
    };
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });
    let m = model_from(vec![
        Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
            c("D"),
            CE::ObjectIntersectionOf(vec![c("G"), some(c("F"))]),
        ])),
        sub(c("A"), c("G")),
        sub(c("A"), some(c("F2"))),
        sub(c("F2"), c("F")),
    ]);
    let rz = Reasoner::classify(&m);
    assert!(rz.is_subsumed(&format!("{NS}A"), &format!("{NS}D")), "A ⊑ D via defined-class recognition through subsumed filler");
}

#[test]
fn union_elim_through_defined_class() {
    // D ≡ G ⊓ ∃r.F ; A ⊑ G,∃r.F ; B ⊑ G,∃r.F ; U ≡ A ⊔ B  ⟹  U ⊑ D.
    // Union-elimination is on only in the `owlmake` reasoner config;
    // `--reasoner elk` leaves it off.
    owlmake::reason::el::set_whelk_mode(true);
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let some = |filler: CE<_>| CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(r.clone()),
        bce: Box::new(filler),
    };
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });
    let m = model_from(vec![
        Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
            c("D"),
            CE::ObjectIntersectionOf(vec![c("G"), some(c("F"))]),
        ])),
        sub(c("A"), c("G")),
        sub(c("A"), some(c("F"))),
        sub(c("B"), c("G")),
        sub(c("B"), some(c("F"))),
        Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
            c("U"),
            CE::ObjectUnionOf(vec![c("A"), c("B")]),
        ])),
    ]);
    let rz = Reasoner::classify(&m);
    assert!(rz.is_subsumed(&format!("{NS}A"), &format!("{NS}D")), "A ⊑ D");
    assert!(rz.is_subsumed(&format!("{NS}B"), &format!("{NS}D")), "B ⊑ D");
    assert!(rz.is_subsumed(&format!("{NS}U"), &format!("{NS}D")), "U ⊑ D via union elim over defined class");
}

#[test]
fn union_elim_over_existential_with_subsumed_filler_into_defined_class() {
    // D ≡ G ⊓ ∃r.F ; A,B ⊑ G ; A,B ⊑ ∃r.F2 ; F2 ⊑ F ; U ≡ A ⊔ B  ⟹  U ⊑ D.
    // Union-elimination is on only in the `owlmake` reasoner config;
    // `--reasoner elk` leaves it off.
    owlmake::reason::el::set_whelk_mode(true);
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let some = |filler: CE<_>| CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(r.clone()),
        bce: Box::new(filler),
    };
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });
    let m = model_from(vec![
        Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
            c("D"),
            CE::ObjectIntersectionOf(vec![c("G"), some(c("F"))]),
        ])),
        sub(c("A"), c("G")),
        sub(c("A"), some(c("F2"))),
        sub(c("B"), c("G")),
        sub(c("B"), some(c("F2"))),
        sub(c("F2"), c("F")),
        Component::EquivalentClasses(horned_owl::model::EquivalentClasses(vec![
            c("U"),
            CE::ObjectUnionOf(vec![c("A"), c("B")]),
        ])),
    ]);
    let rz = Reasoner::classify(&m);
    assert!(rz.is_subsumed(&format!("{NS}A"), &format!("{NS}D")), "A ⊑ D");
    assert!(rz.is_subsumed(&format!("{NS}U"), &format!("{NS}D")), "U ⊑ D");
}

/// The `--reasoner whelk` backend (the whelk-rs crate) classifies the canonical
/// EL example identically to the built-in EL reasoner: same direct edges, same
/// consistency verdict.
#[test]
fn whelk_matches_builtin_on_endocarditis() {
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let part_of = b.object_property(format!("{NS}part_of"));
    let some = |p: &horned_owl::model::ObjectProperty<_>, filler: CE<_>| CE::ObjectSomeValuesFrom {
        ope: OPE::ObjectProperty(p.clone()),
        bce: Box::new(filler),
    };
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });

    let m = model_from(vec![
        sub(c("Endocardium"), c("Tissue")),
        sub(c("Endocardium"), some(&part_of, c("HeartWall"))),
        sub(c("HeartWall"), some(&part_of, c("Heart"))),
        Component::SubObjectPropertyOf(horned_owl::model::SubObjectPropertyOf {
            sub: SOPE::ObjectPropertyChain(vec![
                OPE::ObjectProperty(part_of.clone()),
                OPE::ObjectProperty(part_of.clone()),
            ]),
            sup: OPE::ObjectProperty(part_of.clone()),
        }),
        sub(
            CE::ObjectIntersectionOf(vec![c("Tissue"), some(&part_of, c("Heart"))]),
            c("HeartTissue"),
        ),
    ]);

    let elk = Reasoner::classify(&m);
    let whelk = WhelkClassification::classify(&m);

    assert!(whelk.is_consistent());
    assert_eq!(whelk.is_consistent(), elk.is_consistent());
    assert_eq!(
        whelk.direct_subsumptions(),
        elk.direct_subsumptions(),
        "whelk-rs and the built-in reasoner must agree on the direct subsumptions"
    );
    // The signature entailment must survive the transitive reduction either way.
    let edge = (format!("{NS}Endocardium"), format!("{NS}HeartTissue"));
    assert!(whelk.direct_subsumptions().contains(&edge), "Endocardium ⊑ HeartTissue (direct)");
}

/// whelk-rs detects incoherence from a disjointness clash: A ⊑ B, A ⊑ C,
/// B disjoint C ⟹ A unsatisfiable, ontology still consistent (⊤ satisfiable).
#[test]
fn whelk_detects_unsatisfiable_class() {
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let sub = |x: CE<_>, y: CE<_>| Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y });
    let m = model_from(vec![
        sub(c("A"), c("B")),
        sub(c("A"), c("C")),
        Component::DisjointClasses(horned_owl::model::DisjointClasses(vec![c("B"), c("C")])),
    ]);
    let whelk = WhelkClassification::classify(&m);
    assert!(whelk.is_consistent(), "owl:Thing remains satisfiable");
    assert!(
        whelk.unsatisfiable().contains(&format!("{NS}A")),
        "A must be unsatisfiable, got: {:?}",
        whelk.unsatisfiable()
    );
}

#[test]
fn elk_mode_drops_non_el_conjunct_owlmake_salvages() {
    // `A ⊑ B ⊓ ∀r.C` — the super-side conjunction mixes an EL conjunct (`B`)
    // with a non-EL one (`∀r.C`). `--reasoner elk` drops the WHOLE axiom at the
    // first sub-expression outside the indexable EL fragment, so it never derives
    // `A ⊑ B` and leaves published taxonomies unchanged; `--reasoner owlmake`
    // salvages the EL conjunct (sound, more complete). This is the one
    // reasoner-level difference between the two modes.
    use owlmake::reason::el::set_whelk_mode;
    let b = Build::new_rc();
    let c = |n: &str| CE::Class(b.class(format!("{NS}{n}")));
    let r = b.object_property(format!("{NS}r"));
    let sub = |x: CE<_>, y: CE<_>| {
        Component::SubClassOf(horned_owl::model::SubClassOf { sub: x, sup: y })
    };
    let sup = CE::ObjectIntersectionOf(vec![
        c("B"),
        CE::ObjectAllValuesFrom {
            ope: OPE::ObjectProperty(r.clone()),
            bce: Box::new(c("C")),
        },
    ]);
    let m = model_from(vec![sub(c("A"), sup)]);
    let a = format!("{NS}A");
    let bb = format!("{NS}B");

    // elk mode (the default): the whole axiom is dropped.
    set_whelk_mode(false);
    let elk = Reasoner::classify(&m);
    assert!(
        !elk.is_subsumed(&a, &bb),
        "elk mode must drop `A ⊑ B ⊓ ∀r.C` whole (ROBOT-ELK parity), so A ⊑ B is NOT entailed"
    );

    // owlmake mode: the EL conjunct survives.
    set_whelk_mode(true);
    let owl = Reasoner::classify(&m);
    assert!(
        owl.is_subsumed(&a, &bb),
        "owlmake mode must salvage the EL conjunct A ⊑ B"
    );

    // Reset so other tests sharing this OS thread see the default (elk) mode.
    set_whelk_mode(false);
}

/// A bare `reason` asserts `X ⊑ owl:Thing` for each root class; any of
/// `--exclude-owl-thing`, `--exclude-duplicate-axioms` or `--exclude-tautologies`
/// suppresses the trivial subsumptions. UBERON's `tmp/uberon.owl` runs
/// `reason -r elk --exclude-duplicate-axioms true` with no `-T`, and carries
/// none; a `reason` with no flags at all keeps them.
#[test]
fn owl_thing_subsumptions_follow_the_exclusion_flags() {
    let dir = std::env::temp_dir().join(format!("om_thing_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("in.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/t>\n\
         Declaration(Class(<http://x.org/Root>))\n\
         Declaration(Class(<http://x.org/Child>))\n\
         SubClassOf(<http://x.org/Child> <http://x.org/Root>)\n\
         )\n",
    )
    .unwrap();

    let run = |flags: &[&str], out: &std::path::Path| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_om"));
        c.args(["reason", "-i"]).arg(&src).args(["-r", "elk"]).args(flags);
        assert!(c.arg("-o").arg(out).status().unwrap().success());
        std::fs::read_to_string(out).unwrap()
    };

    let d = run(&[], &dir.join("default.ofn"));
    assert!(d.contains("SubClassOf(<http://x.org/Root> owl:Thing)"),
        "a bare reason asserts the trivial subsumption for the root:\n{d}");
    assert!(d.contains("SubClassOf(<http://x.org/Child> <http://x.org/Root>)"),
        "the real hierarchy survives:\n{d}");

    let e = run(&["--exclude-duplicate-axioms", "true"], &dir.join("nodup.ofn"));
    assert!(!e.contains("owl:Thing"),
        "--exclude-duplicate-axioms also suppresses owl:Thing subsumptions:\n{e}");

    let t = run(&["--exclude-owl-thing", "true"], &dir.join("nothing.ofn"));
    assert!(!t.contains("owl:Thing"), "-T true suppresses them directly:\n{t}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The same law through the library surface. `ReasonOptions::default()` IS a
/// bare `reason`, so it asserts the trivial subsumptions too.
///
/// The test above drives the binary, which resolves its own defaults from the
/// command line — so it stays green even if the `Default` impl drifts away from
/// them. The bindings reason through `Default`, and this is what pins it.
#[test]
fn default_options_are_a_bare_reason() {
    use owlmake::api::{self, ReasonOptions};
    use owlmake::io::Format;

    const ONT: &[u8] = b"Prefix(:=<http://x.org/>)\n\
Ontology(<http://x.org/t>\n\
Declaration(Class(<http://x.org/Root>))\n\
Declaration(Class(<http://x.org/Child>))\n\
SubClassOf(<http://x.org/Child> <http://x.org/Root>)\n\
)\n";

    let model = api::parse(ONT, Format::Functional).expect("parse");
    let reasoned = api::reason(model, "elk", &ReasonOptions::default()).expect("reason");
    let text = String::from_utf8(api::serialize(&reasoned, Format::Functional).expect("serialize"))
        .expect("utf-8 output");

    assert!(
        text.contains("SubClassOf(:Root owl:Thing)"),
        "the default options assert the root's trivial subsumption:\n{text}"
    );
    assert!(
        text.contains("SubClassOf(:Child :Root)"),
        "the real hierarchy survives:\n{text}"
    );
}


/// `materialize` states the REQUESTED property explicitly, even when a
/// sub-property already provides the same filler. Suppressing `X ⊑ ∃r.D` because
/// `X ⊑ ∃r2.D` holds for some `r2 ⊑ r` defeats the command.
///
/// UBERON asserts `pituitary gland immediate_transformation_of future pituitary
/// gland`, and with `RO_0002495 ⊑ RO_0002494 ⊑ RO_0002202` the requested
/// `develops_from` edge was suppressed — 199 such inferences missing from
/// `tmp/uberon.owl`, across all three properties the recipe materializes.
#[test]
fn materialize_states_the_requested_property_over_a_subproperty() {
    let dir = std::env::temp_dir().join(format!("om_subrole_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("in.ofn");
    std::fs::write(
        &src,
        "Prefix(:=<http://x.org/>)\n\
         Ontology(<http://x.org/s>\n\
         Declaration(Class(<http://x.org/A>))\n\
         Declaration(Class(<http://x.org/D>))\n\
         Declaration(ObjectProperty(<http://x.org/df>))\n\
         Declaration(ObjectProperty(<http://x.org/mid>))\n\
         Declaration(ObjectProperty(<http://x.org/imm>))\n\
         SubObjectPropertyOf(<http://x.org/imm> <http://x.org/mid>)\n\
         SubObjectPropertyOf(<http://x.org/mid> <http://x.org/df>)\n\
         SubClassOf(<http://x.org/A> ObjectSomeValuesFrom(<http://x.org/imm> <http://x.org/D>))\n\
         )\n",
    )
    .unwrap();
    let props = dir.join("props.txt");
    std::fs::write(&props, "http://x.org/df\n").unwrap();
    let out = dir.join("out.ofn");
    assert!(std::process::Command::new(env!("CARGO_BIN_EXE_om"))
        .args(["materialize", "-i"]).arg(&src)
        .arg("-T").arg(&props).args(["-r", "elk", "-o"]).arg(&out)
        .status().unwrap().success());
    let r = std::fs::read_to_string(&out).unwrap();

    // Two levels of sub-property between the asserted edge and the requested one.
    assert!(r.contains("ObjectSomeValuesFrom(<http://x.org/df> <http://x.org/D>)"),
        "the requested property is stated:\n{r}");
    // The original assertion is untouched, so this is an addition not a rewrite.
    assert!(r.contains("ObjectSomeValuesFrom(<http://x.org/imm> <http://x.org/D>)"),
        "the asserted sub-property edge survives:\n{r}");

    let _ = std::fs::remove_dir_all(&dir);
}
