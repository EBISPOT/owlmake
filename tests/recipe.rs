//! End-to-end tests for the recipe interpreter ([`owlmake::build::recipe`]): a
//! recipe line, with its variables already expanded, is decomposed and executed
//! in-process. A tool name such as `robot` or `jq` at command position is
//! rewritten to the owlmake binary's matching subcommand, and file operations
//! run natively, so such a name never resolves to a system `robot` or `jq`. A
//! script the recipe shells out to reaches those same subcommands through a
//! shim directory prepended to the shell's `PATH`.

use std::path::{Path, PathBuf};

use owlmake::build::recipe;

const BIN: &str = env!("CARGO_BIN_EXE_om");

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owlmake_recipe_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(line: &str, dir: &Path) {
    recipe::run_line(line, dir, Path::new(BIN), "robot", &[]).expect(line);
}

/// A small mixed recipe: a `robot` chain (dispatched to the owlmake binary), a
/// native file copy, and a `jq` pipeline with a redirect (shell-rewritten to
/// owlmake's own `jq`). All three must run end-to-end.
#[test]
fn mixed_recipe_runs_end_to_end() {
    let dir = workdir("mixed");
    std::fs::write(
        dir.join("in.ofn"),
        "Prefix(:=<http://x/>)\nOntology(\nDeclaration(Class(:A))\nDeclaration(Class(:B))\nSubClassOf(:A :B)\n)\n",
    )
    .unwrap();

    // `robot` chain → owlmake binary (file in / file out, no PATH dependency).
    run("robot convert -i in.ofn -o out.ofn --format ofn", &dir);
    assert!(dir.join("out.ofn").is_file(), "robot convert produced no output");
    assert!(std::fs::read_to_string(dir.join("out.ofn")).unwrap().contains("SubClassOf"));

    // Native file copy (no shell).
    run("cp out.ofn final.ofn", &dir);
    assert_eq!(
        std::fs::read_to_string(dir.join("final.ofn")).unwrap(),
        std::fs::read_to_string(dir.join("out.ofn")).unwrap()
    );

    // jq pipeline with a redirect → owlmake's own `jq` via explicit-path rewrite.
    run("echo '{\"v\":42}' | jq -r .v > num.txt", &dir);
    assert_eq!(std::fs::read_to_string(dir.join("num.txt")).unwrap().trim(), "42");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A recipe that shells out to an *external script* which itself calls bare
/// `jq` must still resolve `jq` to owlmake's own — via the shim dir the
/// interpreter prepends to the shell's PATH (the case the in-line rewrite can't
/// cover, since the call lives inside the script, not the recipe line).
#[test]
fn external_script_resolves_bundled_tool_via_shim() {
    let dir = workdir("shim");
    std::fs::write(
        dir.join("nested.sh"),
        "#!/bin/sh\necho '{\"k\":7}' | jq -r .k > out.txt\n",
    )
    .unwrap();
    // The shim dir is prepended to PATH, so it wins even if a system jq exists.
    run("sh nested.sh", &dir);
    assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap().trim(), "7");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `&&`/`;`-separated commands run in sequence, and a leading `-` makes a failing
/// line non-fatal (the ignore-errors prefix a recipe line may carry).
#[test]
fn sequencing_and_ignore_errors() {
    let dir = workdir("seq");
    run("mkdir -p a/b && touch a/b/c.txt ; touch a/d.txt", &dir);
    assert!(dir.join("a/b/c.txt").exists());
    assert!(dir.join("a/d.txt").exists());

    // A bare failing line aborts…
    assert!(recipe::run_line("rm does-not-exist", &dir, Path::new(BIN), "robot", &[]).is_err());
    // …but with the `-` prefix it is ignored.
    recipe::run_line("-rm does-not-exist", &dir, Path::new(BIN), "robot", &[]).unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}

/// A line's parts share one shell, as make runs them: a `cd` reaches every
/// part after it, and so do a variable set or exported, and the status `$?`
/// reads.
#[test]
fn the_parts_of_a_line_share_one_shell() {
    let dir = workdir("one-shell");
    run("mkdir -p a/b && cd a && pwd > b/where.txt", &dir);
    let where_ = std::fs::read_to_string(dir.join("a/b/where.txt")).unwrap();
    assert_eq!(Path::new(where_.trim()), dir.join("a").canonicalize().unwrap());
    run("mkdir -p c; cd c; touch here.txt", &dir);
    assert!(dir.join("c/here.txt").is_file());
    run("export x=1 && sh -c 'echo x=$x' > exported.txt", &dir);
    assert_eq!(std::fs::read_to_string(dir.join("exported.txt")).unwrap().trim(), "x=1");
    run("y=2; echo y=$y > assigned.txt", &dir);
    assert_eq!(std::fs::read_to_string(dir.join("assigned.txt")).unwrap().trim(), "y=2");
    run("false; echo status=$? > status.txt", &dir);
    assert_eq!(std::fs::read_to_string(dir.join("status.txt")).unwrap().trim(), "status=1");
    // A parameter is expanded where it stands, as the shell expands it.
    recipe::run_line("echo x=$X > x.txt", &dir, Path::new(BIN), "robot", &[("X".into(), "1".into())])
        .unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("x.txt")).unwrap().trim(), "x=1");
    run("echo '$X' > literal.txt", &dir);
    assert_eq!(std::fs::read_to_string(dir.join("literal.txt")).unwrap().trim(), "$X");
    // A part that fails still fails the line.
    assert!(recipe::run_line("cd a && ls not-in-a", &dir, Path::new(BIN), "robot", &[]).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `make` the shell reaches inside a control construct — UBERON's
/// `if [ ! -f mirror/ncbitaxondisjoints.owl ]; then make mirror/ncbitaxondisjoints.owl
/// MIR=true IMP=true ; fi` — is owlmake's own, and builds the target from the
/// repository's plan. There is no Makefile for any other `make` to read.
#[test]
fn a_make_inside_a_shell_construct_builds_from_the_plan() {
    let root = workdir("submake");
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        "emulate_odk_version: 1.6.1\n\
         id: tiny\n\
         uribase: http://example.org\n\
         edit_format: ofn\n\
         targets:\n\
         - target: src/ontology/made.txt\n\
         \x20 steps:\n\
         \x20 - op: print\n\
         \x20   message: made-by-the-plan\n\
         \x20   dst: src/ontology/made.txt\n",
    )
    .unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(:=<http://example.org/tiny/>)\nOntology(<http://example.org/tiny.owl>\n\
         Declaration(Class(<http://example.org/TINY_0000001>))\n)\n",
    )
    .unwrap();
    run("if [ ! -f made.txt ]; then make made.txt ; fi && cp made.txt copy.txt", &ont);
    assert_eq!(
        std::fs::read_to_string(ont.join("copy.txt")).unwrap().trim(),
        "made-by-the-plan"
    );
    let _ = std::fs::remove_dir_all(&root);
}


/// A plan-only repository whose one target is the shell command `command`, run
/// with `om make linked.owl` in its ontology directory next to `source.owl`.
fn build_linked(name: &str, command: &str) -> (PathBuf, bool) {
    let root = workdir(name);
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        format!(
            "emulate_odk_version: 1.6.1\n\
             id: tiny\n\
             uribase: http://example.org\n\
             edit_format: ofn\n\
             targets:\n\
             - target: src/ontology/linked.owl\n\
             \x20 steps:\n\
             \x20 - op: shell\n\
             \x20   command: {command}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(:=<http://example.org/tiny/>)\nOntology(<http://example.org/tiny.owl>\n)\n",
    )
    .unwrap();
    std::fs::write(ont.join("source.owl"), SOURCE).unwrap();
    let ok = std::process::Command::new(BIN)
        .args(["make", "linked.owl"])
        .current_dir(&ont)
        .output()
        .expect("running om")
        .status
        .success();
    (root, ok)
}

const SOURCE: &str = "Prefix(:=<http://example.org/src/>)\nOntology(<http://example.org/src.owl>\n\
                      Declaration(Class(:A))\n)\n";

/// A rule whose command names its target and has no ontology before it — it
/// writes the target itself — leaves nothing behind when the command fails, so
/// the next build does not find the target made.
#[test]
fn a_failed_command_leaves_no_target_behind() {
    let (root, ok) = build_linked("failed-link", "false && ln -f -s source.owl linked.owl");
    assert!(!ok, "the build succeeded");
    let linked = root.join("src/ontology/linked.owl");
    assert!(
        std::fs::symlink_metadata(&linked).is_err(),
        "a failed command left `linked.owl` behind"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// …and when it succeeds, what it made is the target: the link, with the file it
/// links to untouched.
#[test]
fn a_command_that_makes_its_target_keeps_what_it_made() {
    let (root, ok) = build_linked("made-link", "ln -f -s source.owl linked.owl");
    assert!(ok, "the build failed");
    let ont = root.join("src/ontology");
    assert_eq!(std::fs::read_link(ont.join("linked.owl")).unwrap(), Path::new("source.owl"));
    assert_eq!(std::fs::read_to_string(ont.join("source.owl")).unwrap(), SOURCE);
    let _ = std::fs::remove_dir_all(&root);
}

/// A class with an annotated definition, and an individual asserted to be one.
const EMULATED_SOURCE: &str = "Prefix(obo:=<http://purl.obolibrary.org/obo/>)\n\
    Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
    Ontology(<http://example.org/source.owl>\n\
    Declaration(Class(obo:TINY_0000001))\n\
    Declaration(NamedIndividual(obo:TINY_0000002))\n\
    Declaration(AnnotationProperty(obo:IAO_0000115))\n\
    AnnotationAssertion(Annotation(rdfs:comment \"said so\") obo:IAO_0000115 obo:TINY_0000001 \"a thing\")\n\
    ClassAssertion(obo:TINY_0000001 obo:TINY_0000002)\n)\n";

/// An owlmake process a build starts writes as the build does.
///
/// A standard build emulates ODK 1.6.1 and so ROBOT 1.9.10: its OBO has no
/// `[Instance]` frame, and its OBO Graphs JSON nests a definition's axiom
/// annotations as the definition's own `meta`. Each target here converts
/// `source.ofn` in an owlmake process of its own, started by one of the three
/// routes a build has: the build spawning the command itself, the shell running
/// a line whose launcher is rewritten to the owlmake binary, and the shell
/// finding `robot` on `PATH`.
#[test]
fn a_process_the_build_starts_writes_as_the_plan_emulates() {
    const ROUTES: [(&str, &str); 3] = [
        ("spawned", "robot convert --input source.ofn -f FORMAT -o spawned.FORMAT"),
        (
            "rewritten",
            "om --catalog catalog-v001.xml convert --input source.ofn -f FORMAT -o rewritten.FORMAT | cat",
        ),
        ("on-path", "if true; then robot convert --input source.ofn -f FORMAT -o on-path.FORMAT; fi"),
    ];
    let root = workdir("emulated-children");
    let ont = root.join("src/ontology");
    std::fs::create_dir_all(&ont).unwrap();
    let mut plan = String::from(
        "emulate_odk_version: 1.6.1\nid: tiny\nuribase: http://example.org\nedit_format: ofn\ntargets:\n",
    );
    let mut goals = Vec::new();
    for (route, command) in ROUTES {
        for format in ["obo", "json"] {
            plan.push_str(&format!(
                "- target: src/ontology/{route}.{format}\n  steps:\n  - op: shell\n    command: {}\n",
                command.replace("FORMAT", format)
            ));
            goals.push(format!("{route}.{format}"));
        }
    }
    std::fs::write(root.join("owlmake.yaml"), plan).unwrap();
    std::fs::write(
        ont.join("tiny-edit.ofn"),
        "Prefix(:=<http://example.org/tiny/>)\nOntology(<http://example.org/tiny.owl>\n)\n",
    )
    .unwrap();
    std::fs::write(ont.join("source.ofn"), EMULATED_SOURCE).unwrap();
    let out = std::process::Command::new(BIN)
        .arg("make")
        .args(&goals)
        .current_dir(&ont)
        .output()
        .expect("running om");
    assert!(out.status.success(), "the build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    for (route, _) in ROUTES {
        let obo = std::fs::read_to_string(ont.join(format!("{route}.obo"))).unwrap();
        assert!(
            obo.contains("[Term]") && !obo.contains("[Instance]"),
            "{route}: the OBO is not what ODK 1.6.1 writes:\n{obo}"
        );
        let json = std::fs::read_to_string(ont.join(format!("{route}.json"))).unwrap();
        assert!(
            json.contains("said so"),
            "{route}: the definition's axiom annotation is not nested as ROBOT 1.9.10 nests it:\n{json}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A plan of the repository's own (`use_builtin_rules: false`) with `targets`,
/// written at `root` beside `source.ofn` and built there with `om make <goals>`;
/// whether the build succeeded, and what it reported.
fn make_own_plan(root: &Path, targets: &str, goals: &[&str]) -> (bool, String) {
    std::fs::write(
        root.join("owlmake.yaml"),
        format!(
            "id: ex\n\
             version: '2026-10-05'\n\
             reasoner: elk\n\
             ontology_iri: http://example.org/ex.owl\n\
             use_builtin_rules: false\n\
             targets:\n{targets}"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("source.ofn"),
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/source.owl>\n\
         Declaration(Class(<http://example.org/A>))\n\
         AnnotationAssertion(rdfs:label <http://example.org/A> \"a thing\")\n)\n",
    )
    .unwrap();
    let out = std::process::Command::new(BIN)
        .args(["make", "-B"])
        .args(goals)
        .current_dir(root)
        .output()
        .expect("running om");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A target made by shell commands alone is the file they leave, whatever its
/// extension. A `.json` names OBO Graphs JSON as an ontology format, but a later
/// command that merely names the file (`cat`, `ls`) gives nothing a model to read,
/// so the file is not parsed as one — with `side_effect_only` as without — and a
/// file that would parse is not cached as an ontology either.
#[test]
fn a_shell_built_data_file_is_not_read_as_an_ontology() {
    let root = workdir("shell-json");
    let write = |file: &str, json: &str| format!("\"echo '{json}' > {file}\"");
    let targets = format!(
        "  - target: stats.json\n    steps:\n\
         \x20     - op: shell\n        command: {}\n\
         \x20     - op: shell\n        command: cat stats.json\n\
         \x20 - target: listed.json\n    steps:\n\
         \x20     - op: shell\n        command: {}\n\
         \x20     - op: shell\n        command: ls listed.json\n\
         \x20 - target: marked.json\n    side_effect_only: true\n    steps:\n\
         \x20     - op: shell\n        command: {}\n\
         \x20     - op: shell\n        command: cat marked.json\n\
         \x20 - target: graph.json\n    steps:\n\
         \x20     - op: shell\n        command: {}\n\
         \x20     - op: shell\n        command: echo s2\n",
        write("stats.json", r#"{\"widgets\": 3}"#),
        write("listed.json", r#"{\"widgets\": 4}"#),
        write("marked.json", r#"{\"widgets\": 5}"#),
        write("graph.json", r#"{\"graphs\": []}"#),
    );
    let (ok, err) = make_own_plan(&root, &targets, &["stats.json", "listed.json", "marked.json", "graph.json"]);
    assert!(ok, "the build failed:\n{err}");
    for (file, json) in [
        ("stats.json", r#"{"widgets": 3}"#),
        ("listed.json", r#"{"widgets": 4}"#),
        ("marked.json", r#"{"widgets": 5}"#),
        ("graph.json", r#"{"graphs": []}"#),
    ] {
        assert_eq!(std::fs::read_to_string(root.join(file)).unwrap().trim(), json, "{file}");
    }
    let cached: Vec<_> = std::fs::read_dir(root.join(".owlmake-odk-tmp"))
        .map(|d| d.filter_map(Result::ok).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(cached.is_empty(), "the build cached the data files as ontologies: {cached:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The prefix options a chain states before its first command bind for every
/// command of it, as a command line's own do: what `--add-prefixes` (read from
/// its file as the step runs) and `--add-prefix` bind is declared by the
/// document written — by a functional-syntax document, and not as an OBO
/// document's `idspace` — and what `--prefix` binds is not. A command that
/// builds its ontology afresh, as `filter` does, hands them on. As ROBOT 1.9.11
/// runs `robot --add-prefixes ctx.json --add-prefix "baz: …" --prefix "bar: …"
/// annotate -i source.ofn --annotation foo:p x --annotation bar:q y
/// --annotation baz:r z`, and `robot --add-prefixes ctx.json --add-prefix
/// "baz: …" filter -i source.ofn --term http://example.org/A annotate
/// --annotation foo:p x --annotation baz:r z`.
#[test]
fn a_chains_prefix_options_bind_for_each_of_its_commands() {
    let root = workdir("chain-prefixes");
    std::fs::write(root.join("ctx.json"), "{\"@context\": {\"foo\": \"http://example.org/foo#\"}}\n").unwrap();
    let mut targets: String = ["ofn", "obo"]
        .iter()
        .map(|format| {
            format!(
                "  - target: stamped.{format}\n    input: source.ofn\n    needs: [source.ofn, ctx.json]\n    steps:\n\
                 \x20     - op: prefixes\n        add_prefixes: [ctx.json]\n\
                 \x20       add_prefix: ['baz: http://example.org/baz#']\n\
                 \x20       prefix: ['bar: http://example.org/bar#']\n\
                 \x20     - op: annotate\n        annotations:\n\
                 \x20         - {{property: 'foo:p', value: x}}\n\
                 \x20         - {{property: 'bar:q', value: y}}\n\
                 \x20         - {{property: 'baz:r', value: z}}\n"
            )
        })
        .collect();
    targets.push_str(
        "  - target: filtered.ofn\n    input: source.ofn\n    needs: [source.ofn, ctx.json]\n    steps:\n\
         \x20     - op: prefixes\n        add_prefixes: [ctx.json]\n\
         \x20       add_prefix: ['baz: http://example.org/baz#']\n\
         \x20     - op: filter\n        terms: ['http://example.org/A']\n\
         \x20     - op: annotate\n        annotations:\n\
         \x20         - {property: 'foo:p', value: x}\n\
         \x20         - {property: 'baz:r', value: z}\n",
    );
    let built = ["stamped.ofn", "stamped.obo", "filtered.ofn"];
    let (ok, err) = make_own_plan(&root, &targets, &built);
    assert!(ok, "the build failed:\n{err}");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11");
    for (target, robot) in built.iter().zip([
        "chain-prefixes.robot.ofn",
        "chain-prefixes.robot.obo",
        "chain-prefixes-filtered.robot.ofn",
    ]) {
        assert_eq!(
            std::fs::read_to_string(root.join(target)).unwrap(),
            std::fs::read_to_string(fixtures.join(robot)).unwrap(),
            "{target}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A `prefixes` step makes the context afresh: what one command's own options
/// added is gone once the step before the next command restates the chain's,
/// and stays when the chain stated it. As ROBOT 1.9.11 writes
/// `template --add-prefix "zz: …" --template own-prefix.tsv annotate
/// --annotation rdfs:comment x` and the same with `--add-prefix` stated first.
#[test]
fn a_prefixes_step_makes_the_context_afresh() {
    let root = workdir("prefix-scope");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11");
    std::fs::copy(fixtures.join("own-prefix.tsv"), root.join("own-prefix.tsv")).unwrap();
    let target = |name: &str, between: &str| {
        format!(
            "  - target: {name}\n    needs: [own-prefix.tsv]\n    steps:\n\
             \x20     - op: prefixes\n        add_prefix: ['zz: http://example.org/zz#']\n\
             \x20     - op: template\n        templates: [own-prefix.tsv]\n\
             {between}\
             \x20     - op: annotate\n        annotations:\n\
             \x20         - {{property: 'rdfs:comment', value: x}}\n"
        )
    };
    let targets = target("own.ofn", "      - op: prefixes\n") + &target("chain.ofn", "");
    let (ok, err) = make_own_plan(&root, &targets, &["own.ofn", "chain.ofn"]);
    assert!(ok, "the build failed:\n{err}");
    for (built, robot) in [("own.ofn", "own-add-prefix.robot.ofn"), ("chain.ofn", "chain-add-prefix.robot.ofn")] {
        assert_eq!(
            std::fs::read_to_string(root.join(built)).unwrap(),
            std::fs::read_to_string(fixtures.join(robot)).unwrap(),
            "{built}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A template step fails on a row it cannot read unless the recipe says
/// `--force true`, and with it reports the row and leaves it out. As ROBOT
/// 1.9.11 refuses `template --template template-force.tsv`, whose `ex:2` names
/// nothing, and writes the other two rows with `--force true`.
#[test]
fn a_template_step_fails_on_an_unreadable_row_unless_forced() {
    let root = workdir("template-force");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11");
    std::fs::copy(fixtures.join("template-force.tsv"), root.join("template-force.tsv")).unwrap();
    let targets = "  - target: unforced.ofn\n    needs: [template-force.tsv]\n    steps:\n\
                   \x20     - op: template\n        templates: [template-force.tsv]\n\
                   \x20 - target: forced.ofn\n    needs: [template-force.tsv]\n    steps:\n\
                   \x20     - op: template\n        templates: [template-force.tsv]\n        force: true\n";
    let (ok, err) = make_own_plan(&root, targets, &["unforced.ofn"]);
    assert!(!ok && err.contains("could not interpret 'ex:2'"), "the build should refuse the row:\n{err}");
    let (ok, err) = make_own_plan(&root, targets, &["forced.ofn"]);
    assert!(ok, "the build failed:\n{err}");
    assert_eq!(
        std::fs::read_to_string(root.join("forced.ofn")).unwrap(),
        std::fs::read_to_string(fixtures.join("template-force.robot.ofn")).unwrap()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A planned annotate step does everything its options say, as ROBOT 1.9.11
/// does it: merge an annotation file, annotate the axioms with where they are
/// derived from, assert what defines each entity, and replace the annotations
/// of the axioms.
#[test]
fn a_planned_annotate_does_what_every_option_says() {
    let root = workdir("annotate-options");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11");
    for f in ["annotate-options.ofn", "annotate-options-extra.ofn", "annotate-options-subclasses.ofn"] {
        std::fs::copy(fixtures.join(f), root.join(f)).unwrap();
    }
    let targets = "  - target: all.ofn\n    input: annotate-options.ofn\n\
                   \x20   needs: [annotate-options.ofn, annotate-options-extra.ofn]\n    steps:\n\
                   \x20     - op: annotate\n        annotations:\n\
                   \x20         - {property: 'dc:creator', value: me}\n\
                   \x20       annotation_files: [annotate-options-extra.ofn]\n\
                   \x20       annotate_derived_from: true\n        annotate_defined_by: true\n\
                   \x20 - target: axiom.ofn\n    input: annotate-options-subclasses.ofn\n\
                   \x20   needs: [annotate-options-subclasses.ofn]\n    steps:\n\
                   \x20     - op: annotate\n\
                   \x20       axiom_annotations: ['rdfs:comment', x, 'rdfs:comment', y, 'dc:source', s]\n";
    let (ok, err) = make_own_plan(&root, targets, &["all.ofn", "axiom.ofn"]);
    assert!(ok, "the build failed:\n{err}");
    for (target, robot) in [
        ("all.ofn", "annotate-options.all.robot.ofn"),
        ("axiom.ofn", "annotate-options-subclasses.axiom.robot.ofn"),
    ] {
        assert_eq!(
            std::fs::read_to_string(root.join(target)).unwrap(),
            std::fs::read_to_string(fixtures.join(robot)).unwrap(),
            "{target}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A planned remove and filter do everything their options say, as ROBOT 1.9.11
/// does: a punned term selects both its entities, include and exclude terms
/// and files change the selection, the hierarchy is not re-linked across what
/// goes, and the annotations a property names come off the axioms kept.
#[test]
fn a_planned_remove_and_filter_do_what_every_option_says() {
    let root = workdir("selection-options");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11");
    for f in ["selection-options.ofn", "selection-options-include.txt", "selection-options-exclude.txt"] {
        std::fs::copy(fixtures.join(f), root.join(f)).unwrap();
    }
    let step = |op: &str, term: &str, exclude: &str, selects: &str| {
        format!(
            "    input: selection-options.ofn\n\
             \x20   needs: [selection-options.ofn, selection-options-include.txt, selection-options-exclude.txt]\n\
             \x20   steps:\n\
             \x20     - op: {op}\n\
             \x20       terms: ['http://example.org/s#{term}']\n\
             \x20       include_terms: ['http://example.org/s#P']\n\
             \x20       include_term_files: [selection-options-include.txt]\n\
             \x20       exclude_terms: ['http://example.org/s#{exclude}']\n\
             \x20       exclude_term_files: [selection-options-exclude.txt]\n\
             \x20       selects: ['{selects}']\n\
             \x20       allow_punning: true\n\
             \x20       preserve_structure: false\n\
             \x20       drop_axiom_annotations: ['oboInOwl:source']\n"
        )
    };
    let targets = format!(
        "  - target: removed.ofn\n{}  - target: filtered.ofn\n{}",
        step("remove-terms", "C", "E", "self descendants"),
        step("filter", "E", "C", "self ancestors annotations"),
    );
    let (ok, err) = make_own_plan(&root, &targets, &["removed.ofn", "filtered.ofn"]);
    assert!(ok, "the build failed:\n{err}");
    for (target, robot) in
        [("removed.ofn", "selection-options.remove.robot.ofn"), ("filtered.ofn", "selection-options.filter.robot.ofn")]
    {
        assert_eq!(
            std::fs::read_to_string(root.join(target)).unwrap(),
            std::fs::read_to_string(fixtures.join(robot)).unwrap(),
            "{target}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A planned extract and reduce do what their switches say, as ROBOT 1.9.11
/// does: a forced extract builds the module of a term the ontology does not
/// name, and a reduce keeps a redundant axiom that carries an annotation, or
/// reduces only between named classes.
#[test]
fn a_planned_extract_and_reduce_do_what_their_switches_say() {
    let root = workdir("switches");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11");
    for f in ["selection-options.ofn", "reduce-annotated.ofn"] {
        std::fs::copy(fixtures.join(f), root.join(f)).unwrap();
    }
    let reduce = |target: &str, switch: &str| {
        format!(
            "  - target: {target}\n    input: reduce-annotated.ofn\n    needs: [reduce-annotated.ofn]\n    steps:\n\
             \x20     - op: reduce\n        {switch}: true\n"
        )
    };
    let targets = format!(
        "  - target: extracted.ofn\n    input: selection-options.ofn\n    needs: [selection-options.ofn]\n    steps:\n\
         \x20     - op: extract\n        method: BOT\n        terms: ['http://example.org/s#Z']\n        force: true\n{}{}",
        reduce("preserved.ofn", "preserve_annotated_axioms"),
        reduce("named.ofn", "named_classes_only"),
    );
    let (ok, err) = make_own_plan(&root, &targets, &["extracted.ofn", "preserved.ofn", "named.ofn"]);
    assert!(ok, "the build failed:\n{err}");
    for (target, robot) in [
        ("extracted.ofn", "extract-missing.force.robot.ofn"),
        ("preserved.ofn", "reduce-annotated.preserve.robot.ofn"),
        ("named.ofn", "reduce-annotated.named.robot.ofn"),
    ] {
        assert_eq!(
            std::fs::read_to_string(root.join(target)).unwrap(),
            std::fs::read_to_string(fixtures.join(robot)).unwrap(),
            "{target}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A planned remove reads its switches where the command does: a `trim` that
/// is neither `true` nor `false` fails the step once something is selected,
/// and says nothing when nothing is, as ROBOT 1.9.11 does.
#[test]
fn a_planned_remove_reads_a_switch_where_the_command_does() {
    let root = workdir("switch-text");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11");
    std::fs::copy(fixtures.join("selection-options.ofn"), root.join("selection-options.ofn")).unwrap();
    let step = |target: &str, term: &str| {
        format!(
            "  - target: {target}\n    input: selection-options.ofn\n    needs: [selection-options.ofn]\n    steps:\n\
             \x20     - op: remove-terms\n        terms: ['http://example.org/s#{term}']\n        trim: 'TRUE'\n"
        )
    };
    let (ok, err) = make_own_plan(&root, &step("kept.ofn", "Z"), &["kept.ofn"]);
    assert!(ok, "the build failed:\n{err}");
    assert_eq!(
        std::fs::read_to_string(root.join("kept.ofn")).unwrap(),
        std::fs::read_to_string(fixtures.join("switch-remove-empty.robot.ofn")).unwrap()
    );
    let (ok, err) = make_own_plan(&root, &step("removed.ofn", "C"), &["removed.ofn"]);
    assert!(!ok, "a step with `trim: TRUE` built");
    assert!(err.contains("BOOLEAN VALUE ERROR arg for trim must be true or false"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A planned merge attributes what it merges and keeps the inputs' ontology
/// annotations as its step says, through each input's imports closure as the
/// plan's catalog resolves it. The expected files are ROBOT 1.9.11's merge
/// with the same options.
#[test]
fn a_planned_merge_attributes_what_it_merges() {
    let root = workdir("merge-provenance");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot-1.9.11");
    for f in ["merge-prov-p.ofn", "merge-prov-i.ofn", "merge-prov-s.ofn", "merge-prov-j.ofn", "merge-prov-catalog.xml"] {
        std::fs::copy(fixtures.join(f), root.join(f)).unwrap();
    }
    let merge = |target: &str, options: &str| {
        format!(
            "  - target: {target}\n    input: merge-prov-p.ofn\n    needs: [merge-prov-p.ofn, merge-prov-s.ofn]\n    steps:\n\
             \x20     - op: merge\n        inputs: [merge-prov-p.ofn, merge-prov-s.ofn]\n{options}"
        )
    };
    std::fs::write(
        root.join("owlmake.yaml"),
        format!(
            "id: ex\nversion: '2026-10-05'\nreasoner: elk\nontology_iri: http://example.org/ex.owl\n\
             use_builtin_rules: false\ncatalog_file: merge-prov-catalog.xml\ntargets:\n{}{}",
            merge("defined-by.ofn", "        annotate_defined_by: true\n"),
            merge("kept.ofn", "        collapse_import_closure: false\n        include_annotations: true\n"),
        ),
    )
    .unwrap();
    let out = std::process::Command::new(BIN)
        .args(["make", "-B", "defined-by.ofn", "kept.ofn"])
        .current_dir(&root)
        .output()
        .expect("running om");
    assert!(out.status.success(), "the build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    for (target, robot) in [
        ("defined-by.ofn", "merge-prov.defined-by.robot.ofn"),
        ("kept.ofn", "merge-prov.keep-imports.annotations.robot.ofn"),
    ] {
        assert_eq!(
            std::fs::read_to_string(root.join(target)).unwrap(),
            std::fs::read_to_string(fixtures.join(robot)).unwrap(),
            "{target}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A build reads an input's imports closure at every depth by one rule: the
/// catalog the plan names, else the IRI itself — the file a `file:` IRI names.
/// An import nested in an imported module is no exception: when it exists, its
/// labels head the sections a functional write gives the entities it names, and
/// when it resolves nowhere, the build fails, naming it.
#[test]
fn a_build_resolves_every_import_of_a_closure_or_fails() {
    let root = workdir("closure-depth");
    std::fs::write(
        root.join("sub.ofn"),
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/sub>\n\
         Declaration(Class(<http://example.org/sub#S>))\n\
         AnnotationAssertion(rdfs:label <http://example.org/sub#S> \"from a nested import\")\n)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         \x20 <uri name=\"http://example.org/mid.owl\" uri=\"mid.ofn\"/>\n</catalog>\n",
    )
    .unwrap();
    std::fs::write(
        root.join("edit.ofn"),
        "Ontology(<http://example.org/edit>\n\
         Import(<http://example.org/mid.owl>)\n\
         SubClassOf(<http://example.org/sub#S> <http://example.org/edit#A>)\n)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        "id: ex\nversion: '2026-10-05'\nreasoner: elk\nontology_iri: http://example.org/ex.owl\n\
         use_builtin_rules: false\ncatalog_file: catalog-v001.xml\ntargets:\n\
         \x20 - target: out.ofn\n    input: edit.ofn\n    needs: [edit.ofn]\n    steps:\n\
         \x20     - op: convert\n",
    )
    .unwrap();
    let mid = |import: &str| format!("Ontology(<http://example.org/mid.owl>\nImport(<{import}>)\n)\n");
    let build = || {
        std::process::Command::new(BIN)
            .args(["make", "-B", "out.ofn"])
            .current_dir(&root)
            .output()
            .expect("running om")
    };
    let sub = root.join("sub.ofn").canonicalize().unwrap();
    std::fs::write(root.join("mid.ofn"), mid(&format!("file://{}", sub.display()))).unwrap();
    let out = build();
    assert!(out.status.success(), "the build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let written = std::fs::read_to_string(root.join("out.ofn")).unwrap();
    assert!(written.contains("# Class: <http://example.org/sub#S> (from a nested import)"), "{written}");
    let nowhere = "file:///nonexistent-owlmake-fixture/unresolvable-import.owl";
    std::fs::write(root.join("mid.ofn"), mid(nowhere)).unwrap();
    let out = build();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && stderr.contains(nowhere), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A planned conversion to OBO Graphs JSON writes a graph for each ontology of
/// its input's imports closure, read through the catalog the plan names, at
/// every depth. As ROBOT 1.9.11 converts the same three documents.
#[test]
fn a_planned_json_conversion_writes_a_graph_for_each_ontology_of_the_closure() {
    let root = workdir("json-closure");
    std::fs::write(
        root.join("sub.ofn"),
        "Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Ontology(<http://example.org/sub>\n\
         Declaration(Class(<http://example.org/sub#S>))\n\
         AnnotationAssertion(rdfs:label <http://example.org/sub#S> \"from a nested import\")\n)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         \x20 <uri name=\"http://example.org/mid.owl\" uri=\"mid.ofn\"/>\n</catalog>\n",
    )
    .unwrap();
    std::fs::write(
        root.join("edit.ofn"),
        "Ontology(<http://example.org/edit>\n\
         Import(<http://example.org/mid.owl>)\n\
         SubClassOf(<http://example.org/sub#S> <http://example.org/edit#A>)\n)\n",
    )
    .unwrap();
    let sub = root.join("sub.ofn").canonicalize().unwrap();
    std::fs::write(
        root.join("mid.ofn"),
        format!("Ontology(<http://example.org/mid.owl>\nImport(<file://{}>)\n)\n", sub.display()),
    )
    .unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        "id: ex\nversion: '2026-10-05'\nreasoner: elk\nontology_iri: http://example.org/ex.owl\n\
         use_builtin_rules: false\ncatalog_file: catalog-v001.xml\ntargets:\n\
         \x20 - target: out.json\n    input: edit.ofn\n    needs: [edit.ofn]\n    steps:\n\
         \x20     - op: convert\n",
    )
    .unwrap();
    let out = std::process::Command::new(BIN)
        .args(["make", "-B", "out.json"])
        .current_dir(&root)
        .output()
        .expect("running om");
    assert!(out.status.success(), "the build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let expected = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/robot-1.9.11/obographs-closure-build.robot.json");
    assert_eq!(
        std::fs::read_to_string(root.join("out.json")).unwrap(),
        std::fs::read_to_string(expected).unwrap()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A planned `query --use-graphs` reads its input's imports as every step of a
/// build does: through the catalog the plan names, else the IRI itself. A
/// document in the repository that merely shares an import's name is a path
/// the plan never names, and is not read.
#[test]
fn a_planned_union_query_reads_imports_through_the_plan_alone() {
    let root = workdir("use-graphs-imports");
    std::fs::write(
        root.join("sibling.owl"),
        "Ontology(<http://example.org/sibling>\nDeclaration(Class(<http://example.org/sibling#S>))\n)\n",
    )
    .unwrap();
    let import = "urn:example:imports/sibling.owl";
    std::fs::write(
        root.join("edit.ofn"),
        format!(
            "Ontology(<http://example.org/edit>\nImport(<{import}>)\n\
             Declaration(Class(<http://example.org/edit#A>))\n)\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n</catalog>\n",
    )
    .unwrap();
    std::fs::write(
        root.join("classes.rq"),
        "SELECT ?c WHERE { ?c a <http://www.w3.org/2002/07/owl#Class> } ORDER BY ?c\n",
    )
    .unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        "id: ex\nversion: '2026-10-05'\nreasoner: elk\nontology_iri: http://example.org/ex.owl\n\
         use_builtin_rules: false\ncatalog_file: catalog-v001.xml\ntargets:\n\
         \x20 - target: classes.csv\n    input: edit.ofn\n    needs: [edit.ofn, classes.rq]\n    steps:\n\
         \x20     - op: query\n        use_graphs: true\n        selects:\n\
         \x20         - {query: classes.rq, output: classes.csv}\n",
    )
    .unwrap();
    let out = std::process::Command::new(BIN)
        .args(["make", "-B", "classes.csv"])
        .current_dir(&root)
        .output()
        .expect("running om");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && stderr.contains(import), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A planned query reads its input with the input's imports closure, through
/// the catalog the plan names, as ROBOT's query does: an entity the input names
/// without declaring is typed unless an ontology it imports has it, and an
/// import that resolves nowhere fails the step, naming it.
#[test]
fn a_planned_query_reads_its_input_with_its_imports_closure() {
    let root = workdir("query-closure");
    std::fs::write(
        root.join("imp.ofn"),
        "Prefix(:=<http://example.org/c#>)\nOntology(<http://example.org/imp.owl>\n\
         Declaration(ObjectProperty(:bfo50))\nDeclaration(ObjectProperty(:bfo51))\n)\n",
    )
    .unwrap();
    let edit = |extra: &str| {
        format!(
            "Prefix(:=<http://example.org/c#>)\nOntology(<http://example.org/edit>\n\
             Import(<http://example.org/imp.owl>)\n{extra}Declaration(Class(:A))\nDeclaration(ObjectProperty(:p))\n\
             SubObjectPropertyOf(ObjectPropertyChain(:bfo50 :bfo51) :p)\nSubClassOf(:A ObjectSomeValuesFrom(:q :A))\n)\n"
        )
    };
    std::fs::write(root.join("edit.ofn"), edit("")).unwrap();
    std::fs::write(
        root.join("catalog-v001.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <catalog prefer=\"public\" xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n\
         \x20   <uri name=\"http://example.org/imp.owl\" uri=\"imp.ofn\"/>\n</catalog>\n",
    )
    .unwrap();
    std::fs::write(
        root.join("properties.rq"),
        "SELECT ?p WHERE { ?p a <http://www.w3.org/2002/07/owl#ObjectProperty> } ORDER BY ?p\n",
    )
    .unwrap();
    std::fs::write(
        root.join("owlmake.yaml"),
        "id: ex\nversion: '2026-10-05'\nreasoner: elk\nontology_iri: http://example.org/ex.owl\n\
         use_builtin_rules: false\ncatalog_file: catalog-v001.xml\ntargets:\n\
         \x20 - target: properties.csv\n    input: edit.ofn\n    needs: [edit.ofn, properties.rq]\n    steps:\n\
         \x20     - op: query\n        selects:\n\
         \x20         - {query: properties.rq, output: properties.csv}\n",
    )
    .unwrap();
    let make = || {
        std::process::Command::new(BIN)
            .args(["make", "-B", "properties.csv"])
            .current_dir(&root)
            .output()
            .expect("running om")
    };
    let out = make();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        std::fs::read_to_string(root.join("properties.csv")).unwrap(),
        "p\r\nhttp://example.org/c#p\r\nhttp://example.org/c#q\r\n"
    );
    let import = "urn:example:imports/missing.owl";
    std::fs::write(root.join("edit.ofn"), edit(&format!("Import(<{import}>)\n"))).unwrap();
    let out = make();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && stderr.contains(import), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A `VAR=value` given to `om make` is in the environment of every command a
/// plan's step spawns: the shell a shell step runs, and the owlmake command a
/// step runs in its place.
#[test]
fn a_command_line_variable_reaches_every_command_a_step_spawns() {
    let root = workdir("run-env");
    let targets = "  - target: shell.txt\n    steps:\n\
         \x20     - op: shell\n        command: \"echo robot_env=$ROBOT_ENV > shell.txt\"\n\
         \x20 - target: jq.txt\n    steps:\n\
         \x20     - op: shell\n        command: \"jq -n -r '$ENV.ROBOT_ENV' > jq.txt\"\n";
    let (ok, err) =
        make_own_plan(&root, targets, &["shell.txt", "jq.txt", "ROBOT_ENV=ROBOT_JAVA_ARGS=-Xmx6G"]);
    assert!(ok, "the build failed:\n{err}");
    let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap().trim().to_string();
    assert_eq!(read("shell.txt"), "robot_env=ROBOT_JAVA_ARGS=-Xmx6G");
    assert_eq!(read("jq.txt"), "ROBOT_JAVA_ARGS=-Xmx6G");
    let _ = std::fs::remove_dir_all(&root);
}

/// A plan's shell step runs its command line as one shell, so a `cd` in it
/// reaches the commands after it.
#[test]
fn a_shell_step_changes_directory_for_the_rest_of_its_line() {
    let root = workdir("shell-cd");
    let targets = "  - target: a/b/where.txt\n    steps:\n\
         \x20     - op: shell\n        command: \"mkdir -p a/b && cd a && pwd > b/where.txt\"\n";
    let (ok, err) = make_own_plan(&root, targets, &["a/b/where.txt"]);
    assert!(ok, "the build failed:\n{err}");
    let where_ = std::fs::read_to_string(root.join("a/b/where.txt")).unwrap();
    assert_eq!(Path::new(where_.trim()), root.join("a").canonicalize().unwrap());
    let _ = std::fs::remove_dir_all(&root);
}

/// …while a target a command edits in place, with an op after it that reads the
/// model, is read back: the op sees the edit. One target is put on disk from the
/// rule's input, the other by a command of its own.
#[test]
fn a_target_edited_by_a_command_is_read_back_for_the_op_after_it() {
    let root = workdir("shell-edit");
    let targets = "  - target: edited.ofn\n    input: source.ofn\n    needs: [source.ofn]\n    steps:\n\
         \x20     - op: shell\n        command: \"sed -i 's/a thing/a widget/' edited.ofn\"\n\
         \x20     - op: annotate\n        version_iri: http://example.org/ex/v1/edited.ofn\n\
         \x20 - target: copied.ofn\n    steps:\n\
         \x20     - op: shell\n        command: cp source.ofn copied.ofn\n\
         \x20     - op: shell\n        command: \"sed -i 's/a thing/a widget/' copied.ofn\"\n\
         \x20     - op: annotate\n        version_iri: http://example.org/ex/v1/copied.ofn\n";
    let (ok, err) = make_own_plan(&root, targets, &["edited.ofn", "copied.ofn"]);
    assert!(ok, "the build failed:\n{err}");
    for file in ["edited.ofn", "copied.ofn"] {
        let written = std::fs::read_to_string(root.join(file)).unwrap();
        assert!(
            written.contains("rdfs:label <http://example.org/A> \"a widget\")")
                && written.contains(&format!("<http://example.org/ex/v1/{file}>")),
            "{file} lost the edit or the annotation:\n{written}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
