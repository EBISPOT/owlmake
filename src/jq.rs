//! Embedded `jq` engine — the hidden `owlmake __jq …` subcommand.
//!
//! Release recipes filter JSON with `jq` from inside their recipe lines. When
//! owlmake runs such a line ([`super::recipe`]) it dispatches each `jq`
//! invocation to this bundled engine — by spawning `owlmake jq …`, since the
//! recipe interpreter substitutes the bundled tools by explicit binary path —
//! rather than requiring a `jq` on `PATH`. That keeps owlmake a single
//! self-contained binary.
//!
//! The engine is the `jaq-all` crate behind a CLI that accepts the jq command
//! line, since that is the syntax a recipe line is written in and owlmake has
//! to take it as it stands. The whole flag surface is implemented, not just the
//! part a release recipe happens to use: JSON/raw I/O and the multi-format
//! `--from`/`--to` conversions (YAML, TOML, XML, CBOR, CSV, TSV), in-place
//! editing, `--tab`/`--indent`/`--sort-keys`, `--raw-input0`/`--raw-output0`,
//! `--args`/`--jsonargs`, the `--arg*` variable bindings with `$ARGS`/`$ENV`,
//! and the exit-status codes a recipe may test. Three things are absent — an
//! interactive REPL, `--run-tests`, and library-path `import`/`include` — and
//! the colour flags parse but do nothing, since output always goes to a pipe.

use std::io;
use std::path::{Path, PathBuf};

use jaq_all::data::{self, Filter, Runner};
use jaq_all::fmts::read;
use jaq_all::fmts::write::{with_stdout, write, Writer};
use jaq_all::fmts::Format;
use jaq_all::jaq_core::{ValT, Vars};
use jaq_all::json::write::{Pp, Styles};
use jaq_all::json::Val;
use jaq_all::load::FileReportsDisp;

/// Entry point for `owlmake __jq <args…>`. Returns the process exit code a
/// recipe line may test: `0` success, `1` last output false/null under `-e`,
/// `2` usage error, `3` compile error, `4` no output under `-e`, `5`
/// parse/runtime error.
pub fn main(args: &[String]) -> i32 {
    let cli = match Cli::parse(args) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("Error: {e}");
            return 2;
        }
    };
    if cli.version {
        println!("owlmake jq (jaq engine) {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if cli.help {
        println!("{USAGE}");
        return 0;
    }
    match real_main(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprint!("{e}");
            e.code()
        }
    }
}

/// Run a jq `filter` over a single JSON `input` string and return the JSON
/// output (one value per line, pretty-printed). The in-memory, stream-free
/// entry point the language bindings use — no files, no stdin/stdout — built on
/// the same jaq engine as the CLI [`main`]. Errors (compile or runtime) are
/// returned as a message string.
pub fn run_string(filter_src: &str, input: &str) -> Result<String, String> {
    let var_names: Vec<String> = Vec::new();
    let filter = jaq_all::compile_with(filter_src, jaq_all::defs(), data::funs(), &var_names)
        .map_err(|reports| {
            let mut msg = String::new();
            for fr in &reports {
                msg.push_str(&format!("{}", FileReportsDisp::new(fr)));
            }
            if msg.is_empty() {
                msg.push_str("jq: filter failed to compile");
            }
            msg
        })?;

    let runner = Runner {
        null_input: false,
        color_err: false,
        writer: Writer {
            pp: Pp {
                indent: Some("  ".to_string()),
                sort_keys: false,
                styles: Styles::default(),
                sep_space: true,
            },
            format: Format::Json,
            join: false,
        },
    };

    let fmt = Format::Json;
    let bytes = input.as_bytes();
    let s = read::bytes_str(fmt, bytes).map_err(|e| e.to_string())?;
    let inputs = read::read(fmt, bytes, s, false);

    let mut out: Vec<u8> = Vec::new();
    run(&runner, &filter, Vars::new(Vec::new()), inputs, |v| {
        write(&mut out, &runner.writer, &v)
    })
    .map_err(|e| format!("{e}"))?;
    String::from_utf8(out).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Argument parsing.
// ---------------------------------------------------------------------------

/// A jq filter, given inline or via `-f FILE`.
enum FilterSrc {
    Inline(String),
    FromFile(PathBuf),
}

/// Where trailing positional arguments go once the filter is set.
enum Mode {
    Files,
    Args,
    JsonArgs,
}

#[derive(Default)]
struct Cli {
    // input
    from: Option<Format>,
    null_input: bool,
    slurp: bool,
    // output
    to: Option<Format>,
    compact_output: bool,
    join_output: bool,
    in_place: bool,
    sort_keys: bool,
    tab: bool,
    indent: Option<usize>,
    // compilation
    from_file: bool,
    // key/value
    arg: Vec<(String, String)>,
    argjson: Vec<(String, String)>,
    slurpfile: Vec<(String, PathBuf)>,
    rawfile: Vec<(String, PathBuf)>,
    // positionals
    filter: Option<FilterSrc>,
    files: Vec<PathBuf>,
    args: Vec<String>,
    jsonargs: Vec<String>,
    exit_status: bool,
    help: bool,
    version: bool,
}

const USAGE: &str = "\
owlmake jq — bundled jq engine (a pure-Rust jq clone, via jaq)

Usage: owlmake jq [OPTIONS] [FILTER] [FILES...]

Behaves like jq: applies FILTER to JSON read from FILES (or stdin). Common
options: -r/--raw-output, -c/--compact-output, -n/--null-input, -s/--slurp,
-S/--sort-keys, -e/--exit-status, -R/--raw-input, --tab, --indent N,
--arg NAME VAL, --argjson NAME VAL, --slurpfile/--rawfile NAME FILE,
-f/--from-file FILE, --args/--jsonargs. Also reads YAML/TOML/XML/CBOR/CSV/TSV
via --from/--to.";

/// Argument-parsing error.
enum ArgError {
    Flag(String),
    KeyValue(&'static str),
    Int(&'static str),
    Format(&'static str),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Flag(s) => write!(f, "unknown flag: {s}"),
            Self::KeyValue(o) => write!(f, "{o} expects a key and a value"),
            Self::Int(o) => write!(f, "{o} expects an integer"),
            Self::Format(o) => write!(f, "{o} expects a data format"),
        }
    }
}

impl Cli {
    fn parse(argv: &[String]) -> Result<Self, ArgError> {
        let mut cli = Self::default();
        let mut mode = Mode::Files;
        let mut it = argv.iter();
        while let Some(arg) = it.next() {
            if let Some(rest) = arg.strip_prefix("--") {
                if rest.is_empty() {
                    // everything after a bare `--` is positional
                    for a in it.by_ref() {
                        cli.positional(&mode, a.clone());
                    }
                    break;
                }
                cli.long(&mut mode, rest, &mut it)?;
            } else if arg.len() > 1 && arg.starts_with('-') {
                for c in arg[1..].chars() {
                    cli.short(c)?;
                }
            } else {
                cli.positional(&mode, arg.clone());
            }
        }
        Ok(cli)
    }

    fn positional(&mut self, mode: &Mode, arg: String) {
        if self.filter.is_none() {
            self.filter = Some(if self.from_file {
                FilterSrc::FromFile(arg.into())
            } else {
                FilterSrc::Inline(arg)
            });
        } else {
            match mode {
                Mode::Files => self.files.push(arg.into()),
                Mode::Args => self.args.push(arg),
                Mode::JsonArgs => self.jsonargs.push(arg),
            }
        }
    }

    fn long<'a>(
        &mut self,
        mode: &mut Mode,
        arg: &str,
        it: &mut impl Iterator<Item = &'a String>,
    ) -> Result<(), ArgError> {
        match arg {
            "from" => self.from = Some(parse_format("--from", it)?),
            "null-input" => self.null_input = true,
            "raw-input" => self.from = Some(Format::Raw),
            "raw-input0" => self.from = Some(Format::Raw0),
            "slurp" => self.slurp = true,

            "to" => self.to = Some(parse_format("--to", it)?),
            "compact-output" => self.compact_output = true,
            "raw-output" => self.to = Some(Format::Raw),
            "raw-output0" => self.to = Some(Format::Raw0),
            "join-output" => {
                self.join_output = true;
                self.to.get_or_insert(Format::Raw);
            }
            "in-place" => self.in_place = true,
            "sort-keys" => self.sort_keys = true,
            // colour flags are accepted but ignored: `__jq` always writes to a
            // pipe inside a recipe, so monochrome is the only sensible mode.
            "color-output" | "monochrome-output" => {}
            "tab" => self.tab = true,
            "indent" => {
                let n = it.next().and_then(|s| s.parse().ok());
                self.indent = Some(n.ok_or(ArgError::Int("--indent"))?);
            }
            "from-file" => self.from_file = true,

            "arg" => self.arg.push(parse_key_val("--arg", it)?),
            "argjson" => self.argjson.push(parse_key_val("--argjson", it)?),
            "slurpfile" => {
                let (k, v) = parse_key_val("--slurpfile", it)?;
                self.slurpfile.push((k, v.into()));
            }
            "rawfile" => {
                let (k, v) = parse_key_val("--rawfile", it)?;
                self.rawfile.push((k, v.into()));
            }

            "args" => *mode = Mode::Args,
            "jsonargs" => *mode = Mode::JsonArgs,
            "exit-status" => self.exit_status = true,
            "help" => self.help = true,
            "version" => self.version = true,

            other => return Err(ArgError::Flag(format!("--{other}"))),
        }
        Ok(())
    }

    fn short(&mut self, c: char) -> Result<(), ArgError> {
        match c {
            'R' => self.from = Some(Format::Raw),
            'n' => self.null_input = true,
            's' => self.slurp = true,
            'r' => self.to = Some(Format::Raw),
            'c' => self.compact_output = true,
            'j' => {
                self.join_output = true;
                self.to.get_or_insert(Format::Raw);
            }
            'i' => self.in_place = true,
            'S' => self.sort_keys = true,
            'C' | 'M' => {}
            'f' => self.from_file = true,
            'e' => self.exit_status = true,
            'h' => self.help = true,
            'V' => self.version = true,
            other => return Err(ArgError::Flag(format!("-{other}"))),
        }
        Ok(())
    }

    fn indent_str(&self) -> String {
        if self.tab {
            "\t".into()
        } else {
            " ".repeat(self.indent.unwrap_or(2))
        }
    }

    fn pp(&self) -> Pp {
        Pp {
            indent: (!self.compact_output).then(|| self.indent_str()),
            sort_keys: self.sort_keys,
            styles: Styles::default(),
            sep_space: !self.compact_output || matches!(self.to, Some(Format::Yaml)),
        }
    }

    fn writer(&self) -> Writer {
        Writer {
            pp: self.pp(),
            format: self.to.unwrap_or_default(),
            join: self.join_output,
        }
    }

    fn runner(&self) -> Runner {
        Runner {
            null_input: self.null_input,
            color_err: false,
            writer: self.writer(),
        }
    }
}

fn parse_format<'a>(
    flag: &'static str,
    it: &mut impl Iterator<Item = &'a String>,
) -> Result<Format, ArgError> {
    let s = it.next().ok_or(ArgError::Format(flag))?;
    Format::parse(s).ok_or(ArgError::Format(flag))
}

fn parse_key_val<'a>(
    flag: &'static str,
    it: &mut impl Iterator<Item = &'a String>,
) -> Result<(String, String), ArgError> {
    let key = it.next().ok_or(ArgError::KeyValue(flag))?;
    let val = it.next().ok_or(ArgError::KeyValue(flag))?;
    Ok((key.clone(), val.clone()))
}

// ---------------------------------------------------------------------------
// Execution.
// ---------------------------------------------------------------------------

/// Runtime error, carrying the jq exit code to report.
enum RunError {
    Io(Option<String>, io::Error),
    Compile,        // reports already printed
    Parse(String),
    Jaq(jaq_all::json::Error),
    FalseOrNull,
    NoOutput,
}

impl RunError {
    fn code(&self) -> i32 {
        match self {
            Self::FalseOrNull => 1,
            Self::Io(..) => 2,
            Self::Compile => 3,
            Self::NoOutput => 4,
            Self::Parse(_) | Self::Jaq(_) => 5,
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::FalseOrNull | Self::NoOutput | Self::Compile => Ok(()),
            Self::Io(Some(p), e) => writeln!(f, "Error: {p}: {e}"),
            Self::Io(None, e) => writeln!(f, "Error: {e}"),
            Self::Parse(e) => writeln!(f, "Error: failed to parse: {e}"),
            Self::Jaq(e) => writeln!(f, "Error: {e}"),
        }
    }
}

impl From<io::Error> for RunError {
    fn from(e: io::Error) -> Self {
        Self::Io(None, e)
    }
}

fn real_main(cli: &Cli) -> Result<i32, RunError> {
    let (var_names, vars): (Vec<String>, Vec<Val>) = binds(cli)?.into_iter().unzip();

    // Compile the filter (default `.` when none was given). External
    // `import`/`include` is unsupported — release recipes never use it.
    let code = match &cli.filter {
        None => ".".to_string(),
        Some(FilterSrc::Inline(s)) => s.clone(),
        Some(FilterSrc::FromFile(p)) => std::fs::read_to_string(p)
            .map_err(|e| RunError::Io(Some(p.display().to_string()), e))?,
    };
    let filter = jaq_all::compile_with(&code, jaq_all::defs(), data::funs(), &var_names)
        .map_err(|reports| {
            for fr in &reports {
                eprint!("{}", FileReportsDisp::new(fr));
            }
            RunError::Compile
        })?;

    let vars = Vars::new(vars);
    let runner = cli.runner();
    let writer = &runner.writer;

    let last = if cli.files.is_empty() {
        let format = cli.from.unwrap_or_default();
        let s = read::read_string(format, io::stdin().lock())?;
        let inputs = read::read(format, io::stdin().lock(), &s, cli.slurp);
        with_stdout(|out| run(&runner, &filter, vars, inputs, |v| write(out, writer, &v)))?
    } else {
        let mut last = None;
        for file in &cli.files {
            let path = Path::new(file);
            let bytes = read::load_file(path)
                .map_err(|e| RunError::Io(Some(path.display().to_string()), e))?;
            let format = cli
                .from
                .or_else(|| Format::determine(path))
                .unwrap_or_default();
            let s = read::bytes_str(format, &bytes)?;
            let inputs = read::parse(format, &bytes, s, cli.slurp);

            if cli.in_place {
                // Buffer the whole output, then overwrite the file once the
                // mmap'd input has been dropped (no temp-file crate needed).
                let mut buf = Vec::new();
                last = run(&runner, &filter, vars.clone(), inputs, |v| {
                    write(&mut buf, writer, &v)
                })?;
                drop(bytes);
                std::fs::write(path, &buf)
                    .map_err(|e| RunError::Io(Some(path.display().to_string()), e))?;
            } else {
                last = with_stdout(|out| {
                    run(&runner, &filter, vars.clone(), inputs, |v| write(out, writer, &v))
                })?;
            }
        }
        last
    };

    if cli.exit_status {
        match last {
            None => Err(RunError::NoOutput),
            Some(true) => Ok(0),
            Some(false) => Err(RunError::FalseOrNull),
        }
    } else {
        Ok(0)
    }
}

/// Run `filter` over `inputs`, calling `f` for each output, and return the
/// boolean value of the last output (for `--exit-status`).
fn run(
    runner: &Runner,
    filter: &Filter,
    vars: Vars<Val>,
    inputs: impl Iterator<Item = io::Result<Val>>,
    mut f: impl FnMut(Val) -> io::Result<()>,
) -> Result<Option<bool>, RunError> {
    let mut last = None;
    data::run(runner, filter, vars, inputs, RunError::Parse, |v| {
        let v = v.map_err(RunError::Jaq)?;
        last = Some(v.as_bool());
        f(v).map_err(RunError::from)
    })?;
    Ok(last)
}

/// Build the `--arg*`/`--*file` variable bindings, plus the `$ARGS` and `$ENV`
/// variables every filter may reference.
fn binds(cli: &Cli) -> Result<Vec<(String, Val)>, RunError> {
    let mut var_val: Vec<(String, Val)> = Vec::new();

    for (k, s) in &cli.arg {
        var_val.push((k.clone(), Val::utf8_str(s.clone())));
    }
    for (k, path) in &cli.rawfile {
        let s = read::load_file(path)
            .map_err(|e| RunError::Io(Some(path.display().to_string()), e))?;
        var_val.push((k.clone(), Val::utf8_str(s)));
    }
    for (k, path) in &cli.slurpfile {
        let v = read::json_array(path)
            .map_err(|e| RunError::Io(Some(path.display().to_string()), e))?;
        var_val.push((k.clone(), v));
    }
    for (k, s) in &cli.argjson {
        let v = read::json::parse_single(s.as_bytes())
            .map_err(|e| RunError::Parse(format!("{e} (for value passed to `--argjson {k}`)")))?;
        var_val.push((k.clone(), v));
    }

    let positional: Vec<Val> = cli
        .args
        .iter()
        .cloned()
        .map(Val::from)
        .chain(
            cli.jsonargs
                .iter()
                .map(|s| read::json::parse_single(s.as_bytes()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| RunError::Parse(e.to_string()))?,
        )
        .collect();

    var_val.push(("ARGS".to_string(), args_obj(&positional, &var_val)));
    let env = std::env::vars().map(|(k, v)| (Val::from(k), Val::from(v)));
    var_val.push(("ENV".to_string(), Val::obj(env.collect())));

    Ok(var_val)
}

/// `$ARGS` is `{ positional: [...], named: {...} }`.
fn args_obj(positional: &[Val], named: &[(String, Val)]) -> Val {
    let positional: Val = positional.iter().cloned().collect();
    let named: Val = Val::obj(
        named
            .iter()
            .map(|(k, v)| (Val::from(k.clone()), v.clone()))
            .collect(),
    );
    Val::obj(
        [
            (Val::from("positional".to_string()), positional),
            (Val::from("named".to_string()), named),
        ]
        .into_iter()
        .collect(),
    )
}
