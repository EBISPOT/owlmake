//! Tests for the bundled `owlmake jq` engine and its recipe routing.
//!
//! These exercise the real binary end-to-end: the `jq` subcommand interception,
//! the `jaq`-backed engine, and the recipe interpreter's rewriting of a bare
//! `jq` command word to an explicit `<owlmake> jq` invocation by binary path.
//! One further case runs a corpus of filters — projections, `map`/`select`,
//! `to_entries`/`from_entries`, string interpolation, `@csv` — through the
//! engine, and where the machine has a `jq` binary to run them through as well,
//! checks the two results against each other; it is skipped where there is none.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_om");

/// Run `owlmake jq <args>` with `input` on stdin; return (stdout, exit code).
fn owl_jq(input: &str, args: &[&str]) -> (String, i32) {
    run(BIN, &[&["jq"], args].concat(), input)
}

fn run(prog: &str, args: &[&str], input: &str) -> (String, i32) {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn basic_filters() {
    assert_eq!(owl_jq("[{\"id\":\"A\"},{\"id\":\"B\"}]", &["-r", ".[] | .id"]).0, "A\nB\n");
    assert_eq!(owl_jq("{\"b\":2,\"a\":1}", &["-cS", "."]).0, "{\"a\":1,\"b\":2}\n");
    assert_eq!(owl_jq("1\n2\n3", &["-s", "add"]).0, "6\n"); // slurp 3 inputs -> [1,2,3] -> 6
    assert_eq!(owl_jq("null", &["-r", "--arg", "x", "hi", "$x"]).0, "hi\n");
    assert_eq!(owl_jq("{\"a\":[1,2]}", &["."]).0, "{\n  \"a\": [\n    1,\n    2\n  ]\n}\n");
}

#[test]
fn args_and_env_bindings() {
    // $ENV
    let out = Command::new(BIN)
        .args(["jq", "-nc", "$ENV.OWLMAKE_JQ_TEST"])
        .env("OWLMAKE_JQ_TEST", "yes")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\"yes\"\n");

    // $ARGS.positional via --args
    assert_eq!(
        owl_jq("null", &["-nc", "--args", "$ARGS.positional", "a", "b"]).0,
        "[\"a\",\"b\"]\n"
    );
}

#[test]
fn exit_status_semantics() {
    assert_eq!(owl_jq("{\"a\":1}", &["-e", ".a"]).1, 0);
    assert_eq!(owl_jq("{\"a\":false}", &["-e", ".a"]).1, 1);
    assert_eq!(owl_jq("{}", &["-e", ".x"]).1, 1); // null
    assert_eq!(owl_jq("[]", &["-e", ".[]"]).1, 4); // no output
    assert_eq!(owl_jq("", &["this is not valid jq ("]).1, 3); // compile error
}

/// Recipe `jq` routing: the interpreter rewrites a bare `jq` (and `sssom`)
/// command word to an explicit `<owlmake> jq` invocation given by binary path,
/// so a recipe-style `'… | jq …'` reaches the bundled engine without depending
/// on `PATH`, and cannot pick up a same-named binary from the environment. The
/// same rewrite runs on every recipe line that reaches a shell during a build.
#[test]
fn recipe_rewrites_jq_to_owlmake() {
    let exe = std::path::Path::new(BIN);
    let rewritten = owlmake::build::recipe::rewrite_tools(
        "echo '[{\"id\":\"Z\"}]' | jq -r '.[].id'",
        exe,
        "",
    );
    // The bare `jq` command word became an explicit owlmake invocation.
    assert!(rewritten.contains(&format!("{BIN} jq")), "jq not rewritten: {rewritten}");

    let out = Command::new("sh")
        .arg("-c")
        .arg(&rewritten)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "rewritten jq pipeline did not succeed");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Z\n");
}

/// `rewrite_tools` must only touch `jq`/`sssom` in command position, not when
/// they appear as an argument or substring (e.g. a filename like `jq.json`).
#[test]
fn rewrite_tools_only_at_command_position() {
    let exe = std::path::Path::new("/opt/owlmake");
    let r = owlmake::build::recipe::rewrite_tools("cat jq.json && jq .a", exe, "");
    assert!(r.contains("cat jq.json"), "argument `jq.json` must be untouched: {r}");
    assert!(r.contains("/opt/owlmake jq .a"), "command `jq` must be rewritten: {r}");
}

/// Filter corpus over the constructs recipes use. Those filters arrive verbatim
/// in recipe text, so the engine has to evaluate the language as written and
/// return the conventional exit status. Each case runs through the engine, and
/// through a `jq` binary as well where the machine has one, so a divergence in
/// stdout bytes or exit status fails; skipped automatically where there is none.
#[test]
fn differential_vs_system_jq() {
    if Command::new("jq").arg("--version").stdout(Stdio::null()).status().is_err() {
        eprintln!("system jq not found — skipping differential test");
        return;
    }
    // (input, args…) — args' last element is the filter.
    let cases: &[(&str, &[&str])] = &[
        ("{\"a\":1,\"b\":2}", &[".a"]),
        ("[{\"id\":\"X:1\",\"label\":\"foo\"}]", &["-r", ".[] | \"\\(.id)\\t\\(.label)\""]),
        ("{\"graphs\":[{\"nodes\":[{\"id\":\"A\"},{\"id\":\"B\"}]}]}", &["-r", ".graphs[].nodes[].id"]),
        ("[1,2,3,4]", &["-c", "map(select(. % 2 == 0))"]),
        ("[{\"k\":\"a\",\"v\":1}]", &["-c", "map({(.k):.v}) | add"]),
        ("{\"a\":1,\"b\":2,\"c\":3}", &["-c", "to_entries | map(.key)"]),
        ("\"HELLO\"", &["-r", "ascii_downcase"]),
        ("[3,1,2]", &["-c", "sort"]),
        ("[{\"id\":1},{\"id\":2},{\"id\":1}]", &["-c", "group_by(.id) | map(.[0])"]),
        ("{\"a\":\"1.5\"}", &[".a | tonumber"]),
        ("[[\"a\",1],[\"b\",2]]", &["-cS", "map({key:.[0],value:.[1]}) | from_entries"]),
        ("{\"nested\":[1,[2,[3]]]}", &["-c", "[.. | numbers]"]),
        ("{}", &["-r", ".missing // \"default\""]),
        ("[1,2,3]", &["-r", "@csv"]),
        ("{\"a\":1}", &["-c", "keys"]),
    ];
    for (input, args) in cases {
        let mine = owl_jq(input, args);
        let theirs = run("jq", args, input);
        assert_eq!(mine, theirs, "mismatch for args {args:?} on input {input}");
    }
}
