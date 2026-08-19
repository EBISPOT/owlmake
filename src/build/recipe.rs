//! Decompose a recipe line the plan recorded into declarative commands and
//! execute it in-process.
//!
//! Each recipe line is parsed and decomposed rather than replayed through a
//! shell: a `robot`/`jq`/`sssom` invocation becomes an explicit call to the
//! matching owlmake subcommand, and `cp`/`mv`/`rm`/`mkdir`/`touch` become
//! native, declarative [`FileOp`]s. Only genuine text processors and control
//! constructs (perl/grep/sed/awk, `if`/`for`/…) — and pipelines that mix them —
//! still reach a shell, and even there owlmake's own implementations are
//! substituted by explicit binary path, so a recipe cannot pick up a same-named
//! binary from the environment.
//!
//! The decomposed form is recorded in the plan ([`crate::plan::step::Step`]), so a
//! release is described declaratively: every tool invocation and file operation is
//! served by the `om` binary itself, so nothing else has to be installed for them,
//! and the only outside dependencies left are the shell and whatever interpreters
//! a repo's own scripts need.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::odk::robot::{self, ShellSep};

/// A native file-system operation lifted out of a recipe (so it runs without a
/// shell). The final argument of `cp`/`mv` is the destination; the rest sources.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FileOp {
    /// `cp [-r] SRC... DST`
    Copy {
        src: Vec<String>,
        dst: String,
        #[serde(default)]
        recursive: bool,
    },
    /// `mv SRC... DST`
    Move { src: Vec<String>, dst: String },
    /// `rm [-r] [-f] PATH...`
    Remove {
        paths: Vec<String>,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        force: bool,
    },
    /// `mkdir [-p] PATH...`
    Mkdir {
        paths: Vec<String>,
        #[serde(default)]
        parents: bool,
    },
    /// `touch PATH...`
    Touch { paths: Vec<String> },
    /// `cat SRC... >> DST` (append) or `cat SRC... > DST` (overwrite) — concatenate
    /// files into a destination natively.
    Concat {
        src: Vec<String>,
        dst: String,
        #[serde(default)]
        append: bool,
    },
    /// `sort [-u] [-o OUT] [IN]` (or `sort … > OUT`) — a deterministic,
    /// locale-independent (byte-wise) line sort, so output doesn't drift between
    /// platforms the way the system `sort` can.
    Sort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        output: String,
        #[serde(default)]
        unique: bool,
    },
    /// `wget URL -O DST` / `curl [-sSL] URL -o DST` (or either with a `> DST`
    /// redirect) — fetch a URL to a file, natively over owlmake's own HTTP client
    /// so a release does not depend on the system `wget`/`curl` being installed.
    /// MONDO's `reports/source-versions.tsv` is exactly this.
    Fetch { url: String, dst: String },
    /// `babelon merge A.tsv B.tsv … -o OUT` — concatenate babelon translation
    /// tables. A file-to-file op rather than a model op: the result is a TSV that
    /// a later `babelon convert` turns into OWL.
    BabelonMerge {
        inputs: Vec<String>,
        output: String,
        #[serde(default)]
        sort_tables: bool,
        #[serde(default)]
        drop_unknown_columns: bool,
        #[serde(default)]
        update_translations: bool,
    },
    /// `babelon prepare-translation IN.tsv --oak-adapter pronto:ONT.obo …` —
    /// reconcile a translation table against the current ontology, writing the
    /// updated profile plus the source-changed and not-translated reports.
    BabelonPrepare {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        /// The adapter handle verbatim (`pronto:hp.obo`).
        oak_adapter: String,
        language_code: String,
        fields: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        term_list: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_source_changed: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_not_translated: Option<String>,
        #[serde(default)]
        include_not_translated: bool,
        #[serde(default)]
        update_translation_status: bool,
        #[serde(default)]
        sort_tables: bool,
        #[serde(default)]
        drop_unknown_columns: bool,
    },
    /// `echo [-n] MSG [>|>> DST]` — print a message, optionally to a file.
    Print {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
        #[serde(default)]
        append: bool,
        #[serde(default)]
        newline: bool,
    },
}

/// Pull a trailing `> FILE` / `>> FILE` redirect out of an argument list,
/// returning the remaining operands and the optional `(target, append)`.
fn split_redirect(args: &[String]) -> (Vec<String>, Option<(String, bool)>) {
    let mut operands = Vec::new();
    let mut redirect = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            ">" => redirect = it.next().map(|t| (t.clone(), false)),
            ">>" => redirect = it.next().map(|t| (t.clone(), true)),
            _ => operands.push(a.clone()),
        }
    }
    (operands, redirect)
}

impl FileOp {
    /// Try to read a single (already-tokenized) command as a file operation.
    /// Returns `None` if the leading word is not a recognised file command — or
    /// if its flags go beyond the small, portable subset we model natively (so
    /// the caller can fall back to running it through a shell).
    pub fn parse(argv: &[String]) -> Option<FileOp> {
        let (head, rest) = argv.split_first()?;
        let base = head.rsplit('/').next().unwrap_or(head);
        match base {
            "cp" | "mv" => {
                let mut recursive = false;
                let mut operands = Vec::new();
                for a in rest {
                    match a.as_str() {
                        "-r" | "-R" | "--recursive" => recursive = true,
                        // `cp -a`/`-p`/`-f` etc.: still a plain copy for our needs.
                        "-a" | "-f" | "-p" | "-v" => {}
                        s if s.starts_with('-') => return None, // unmodelled flag
                        _ => operands.push(a.clone()),
                    }
                }
                if operands.len() < 2 {
                    return None;
                }
                let dst = operands.pop().unwrap();
                if base == "cp" {
                    Some(FileOp::Copy { src: operands, dst, recursive })
                } else {
                    Some(FileOp::Move { src: operands, dst })
                }
            }
            "rm" => {
                let mut recursive = false;
                let mut force = false;
                let mut paths = Vec::new();
                for a in rest {
                    match a.as_str() {
                        "-r" | "-R" | "--recursive" => recursive = true,
                        "-f" | "--force" => force = true,
                        "-rf" | "-fr" | "-Rf" | "-fR" => {
                            recursive = true;
                            force = true;
                        }
                        s if s.starts_with('-') => return None,
                        _ => paths.push(a.clone()),
                    }
                }
                if paths.is_empty() {
                    return None;
                }
                Some(FileOp::Remove { paths, recursive, force })
            }
            "mkdir" => {
                let mut parents = false;
                let mut paths = Vec::new();
                for a in rest {
                    match a.as_str() {
                        "-p" | "--parents" => parents = true,
                        s if s.starts_with('-') => return None,
                        _ => paths.push(a.clone()),
                    }
                }
                (!paths.is_empty()).then_some(FileOp::Mkdir { paths, parents })
            }
            "touch" => {
                let paths: Vec<String> = rest.iter().filter(|a| !a.starts_with('-')).cloned().collect();
                (!paths.is_empty()).then_some(FileOp::Touch { paths })
            }
            // `wget URL -O DST` / `curl URL -o DST`, or either with `> DST`. Only
            // the plain "one URL to one file" shape is modelled; anything else
            // (recursive mirroring, POST bodies, headers) falls through to the
            // shell.
            "wget" | "curl" => {
                let (operands, redirect) = split_redirect(rest);
                let mut url: Option<String> = None;
                let mut dst: Option<String> = redirect.map(|(d, _)| d);
                let mut it = operands.iter();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "-O" | "-o" | "--output" | "--output-document" => {
                            dst = Some(it.next()?.clone());
                        }
                        // Flags that take no value and do not change the shape.
                        "-s" | "-S" | "-L" | "-q" | "-f" | "--silent" | "--location"
                        | "--quiet" | "--fail" | "--show-error" | "--no-check-certificate"
                        // `--create-dirs` is implied: `FileOp::Fetch` makes the
                        // destination's parent itself.
                        | "--create-dirs" | "--compressed" | "-k" | "--insecure" => {}
                        // Transfer-tuning flags that take a VALUE. They affect how
                        // the download is retried, never what is downloaded or
                        // where it lands, so consume the value and carry on.
                        // MONDO's mirror rules carry `--retry 4 --max-time 400`;
                        // bailing on them would send all 23 of its mirrors to the
                        // shell as unparsed commands.
                        "--retry" | "--retry-delay" | "--retry-max-time" | "--max-time"
                        | "--connect-timeout" | "-m" | "--limit-rate" | "-t" | "--tries"
                        | "--timeout" => {
                            it.next()?;
                        }
                        // A combined short run (`-sSL`).
                        a if a.starts_with('-') && !a.starts_with("--") && a.len() > 1
                            && a[1..].chars().all(|c| "sSLqf".contains(c)) => {}
                        // Any other flag: not a shape we model.
                        a if a.starts_with('-') => return None,
                        a => {
                            if url.is_some() {
                                return None; // more than one URL
                            }
                            url = Some(a.to_string());
                        }
                    }
                }
                Some(FileOp::Fetch { url: url?, dst: dst? })
            }
            // `cat SRC... >|>> DST` — concatenate into a file. Without a redirect
            // `cat` just streams to stdout, which isn't a file operation.
            "cat" => {
                let (src, redirect) = split_redirect(rest);
                let (dst, append) = redirect?;
                (!src.is_empty()).then_some(FileOp::Concat { src, dst, append })
            }
            // `sort [-u] [-o OUT] [IN]` (or `sort … > OUT`). Modelled only when an
            // output target is given (a `-o`/redirect); a sort to stdout is not a
            // build file op. Bails on any sort flag we don't reproduce exactly
            // (numeric/field/reverse sorts), so those fall back to the shell.
            "sort" => {
                let mut unique = false;
                let mut output: Option<String> = None;
                let mut operands = Vec::new();
                let mut it = rest.iter();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "-u" | "--unique" => unique = true,
                        "-o" | "--output" => output = it.next().cloned(),
                        ">" | ">>" => output = it.next().cloned(),
                        s if s.starts_with("-o") => output = Some(s[2..].to_string()),
                        s if s.starts_with('-') => return None, // unmodelled sort flag
                        _ => operands.push(a.clone()),
                    }
                }
                let output = output?;
                Some(FileOp::Sort { input: operands.into_iter().next(), output, unique })
            }
            // `echo [-n] MSG... [>|>> DST]`.
            "echo" => {
                let mut newline = true;
                let mut words = Vec::new();
                for a in rest {
                    if a == "-n" {
                        newline = false;
                    } else {
                        words.push(a.clone());
                    }
                }
                let (words, redirect) = split_redirect(&words);
                let (dst, append) = match redirect {
                    Some((d, a)) => (Some(d), a),
                    None => (None, false),
                };
                Some(FileOp::Print { message: words.join(" "), dst, append, newline })
            }
            _ => None,
        }
    }

    /// Whether the operation changes the filesystem in a way later steps depend
    /// on, so it must run even inside the in-memory model pipeline, which cannot
    /// reproduce it by writing its model at the end. `Copy`/`Move` are excluded
    /// because they are output bookkeeping — a rule's closing `mv $@.tmp $@` says
    /// where the target goes, which the model write already handles.
    ///
    /// `Remove`/`Mkdir`/`Touch` belong here: steps are the only execution path,
    /// so skipping them leaks every intermediate a rule cleans up — EFO's mondo
    /// import would leave its `*.hgnc.tsv`/`*.hgnc.txt` behind.
    pub fn is_side_effect(&self) -> bool {
        matches!(
            self,
            FileOp::Concat { .. }
                | FileOp::Sort { .. }
                | FileOp::Print { .. }
                | FileOp::Fetch { .. }
                | FileOp::BabelonMerge { .. }
                | FileOp::BabelonPrepare { .. }
                | FileOp::Remove { .. }
                | FileOp::Mkdir { .. }
                | FileOp::Touch { .. }
        )
    }

    /// A short human-readable label for the plan view.
    pub fn label(&self) -> String {
        match self {
            FileOp::Copy { src, dst, recursive } => {
                format!("cp{} {} → {dst}", if *recursive { " -r" } else { "" }, src.join(" "))
            }
            FileOp::Move { src, dst } => format!("mv {} → {dst}", src.join(" ")),
            FileOp::Remove { paths, .. } => format!("rm {}", paths.join(" ")),
            FileOp::Mkdir { paths, parents } => {
                format!("mkdir{} {}", if *parents { " -p" } else { "" }, paths.join(" "))
            }
            FileOp::Touch { paths } => format!("touch {}", paths.join(" ")),
            FileOp::Fetch { url, dst } => format!("fetch {url} → {dst}"),
            FileOp::BabelonMerge { inputs, output, .. } => {
                format!("babelon merge {} file(s) → {output}", inputs.len())
            }
            FileOp::BabelonPrepare { language_code, output, .. } => format!(
                "babelon prepare-translation [{language_code}] → {}",
                output.as_deref().unwrap_or("-")
            ),
            FileOp::Concat { src, dst, append } => {
                format!("cat {} {} {dst}", src.join(" "), if *append { ">>" } else { ">" })
            }
            FileOp::Sort { input, output, unique } => format!(
                "sort{} {} → {output}",
                if *unique { " -u" } else { "" },
                input.as_deref().unwrap_or("-")
            ),
            FileOp::Print { message, dst, .. } => match dst {
                Some(d) => format!("echo … → {d}"),
                None => format!("echo {message}"),
            },
        }
    }

    /// Execute the operation, resolving relative paths against `dir`.
    pub fn run(&self, dir: &Path) -> Result<()> {
        let p = |s: &str| dir.join(s);
        match self {
            FileOp::BabelonMerge {
                inputs,
                output,
                sort_tables,
                drop_unknown_columns,
                update_translations,
            } => {
                let paths: Vec<std::path::PathBuf> = inputs.iter().map(|i| p(i)).collect();
                let table = crate::cmd::babelon_tsv::merge(
                    &paths,
                    *sort_tables,
                    *drop_unknown_columns,
                    *update_translations,
                )?;
                table.write(&p(output))?;
            }
            FileOp::BabelonPrepare {
                input,
                oak_adapter,
                language_code,
                fields,
                term_list,
                output,
                output_source_changed,
                output_not_translated,
                include_not_translated,
                update_translation_status,
                sort_tables,
                drop_unknown_columns,
            } => {
                let (scheme, ont) = oak_adapter.split_once(':').with_context(|| {
                    format!("babelon prepare-translation: malformed OAK handle `{oak_adapter}`")
                })?;
                // `pronto:` and `simpleobo:` are two handles for the same thing —
                // an OBO document — and owlmake reads the file either way. MONDO
                // uses `simpleobo:mondo-simple.obo`. Kept in step with the same
                // check in `crate::cmd::babelon`.
                if scheme != "pronto" && scheme != "simpleobo" {
                    anyhow::bail!(
                        "babelon prepare-translation: unsupported OAK adapter `{scheme}`                          (owlmake reads `pronto:<file.obo>` and `simpleobo:<file.obo>`)"
                    );
                }
                let ontology = crate::cmd::babelon_tsv::TermMeta::from_obo(&p(ont))?;
                let terms = match term_list {
                    Some(t) => Some(
                        std::fs::read_to_string(p(t))?
                            .lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>(),
                    ),
                    None => None,
                };
                let prepared = crate::cmd::babelon_tsv::prepare_translation(
                    input.as_ref().map(|i| p(i)).as_deref(),
                    &ontology,
                    language_code,
                    fields,
                    terms.as_deref(),
                    *include_not_translated,
                    *update_translation_status,
                )?;
                for (table, dst) in [
                    (prepared.profile, output),
                    (prepared.source_changed, output_source_changed),
                    (prepared.not_translated, output_not_translated),
                ] {
                    let Some(dst) = dst else { continue };
                    let mut t = table;
                    if *sort_tables {
                        t.sort();
                    }
                    if *drop_unknown_columns {
                        t.drop_unknown_columns();
                    }
                    t.write(&p(dst))?;
                }
            }
            FileOp::Copy { src, dst, recursive } => {
                let dst = p(dst);
                for s in src {
                    let from = p(s);
                    if *recursive && from.is_dir() {
                        copy_dir_recursive(&from, &dst.join(from.file_name().unwrap_or_default()))?;
                    } else {
                        let target = if dst.is_dir() {
                            dst.join(from.file_name().context("cp source has no file name")?)
                        } else {
                            dst.clone()
                        };
                        std::fs::copy(&from, &target)
                            .with_context(|| format!("cp {} {}", from.display(), target.display()))?;
                    }
                }
            }
            FileOp::Move { src, dst } => {
                let dst = p(dst);
                for s in src {
                    let from = p(s);
                    let target = if dst.is_dir() {
                        dst.join(from.file_name().context("mv source has no file name")?)
                    } else {
                        dst.clone()
                    };
                    // A `.ofn` RENAMED to something else stops being owlmake's own
                    // cache file and becomes a released artefact, so the `#…`
                    // marker lines the Functional writer uses to carry source
                    // state (xmlns block, shared blank nodes, cleared prefixes)
                    // have to go. OBA merges into `definitions.ofn` and renames it
                    // to `patterns/definitions.owl`, which must not ship a
                    // `#prefixes-cleared` first line; MONDO's `tmp/mondo.owl.ofn`
                    // keeps its name, and its markers, because `mondo.obo` is
                    // built by re-reading it.
                    let strip = from.extension().and_then(|e| e.to_str()) == Some("ofn")
                        && target.extension().and_then(|e| e.to_str()) != Some("ofn");
                    if strip {
                        if let Ok(text) = std::fs::read_to_string(&from) {
                            let body: String = text
                                .split_inclusive('\n')
                                .skip_while(|l| l.starts_with('#'))
                                .collect();
                            std::fs::write(&target, body).with_context(|| {
                                format!("mv {} {}", from.display(), target.display())
                            })?;
                            std::fs::remove_file(&from).ok();
                            continue;
                        }
                    }
                    // `rename` fails across filesystems; fall back to copy+remove.
                    if std::fs::rename(&from, &target).is_err() {
                        std::fs::copy(&from, &target)
                            .with_context(|| format!("mv {} {}", from.display(), target.display()))?;
                        std::fs::remove_file(&from).ok();
                    }
                }
            }
            FileOp::Remove { paths, recursive, force } => {
                for path in paths {
                    let path = p(path);
                    let r = if path.is_dir() {
                        if *recursive {
                            std::fs::remove_dir_all(&path)
                        } else {
                            std::fs::remove_dir(&path)
                        }
                    } else {
                        std::fs::remove_file(&path)
                    };
                    if let Err(e) = r {
                        if !(*force && e.kind() == std::io::ErrorKind::NotFound) {
                            return Err(e).with_context(|| format!("rm {}", path.display()));
                        }
                    }
                }
            }
            FileOp::Mkdir { paths, parents } => {
                for path in paths {
                    let path = p(path);
                    let r = if *parents {
                        std::fs::create_dir_all(&path)
                    } else {
                        std::fs::create_dir(&path)
                    };
                    if let Err(e) = r {
                        if !(*parents && e.kind() == std::io::ErrorKind::AlreadyExists) {
                            return Err(e).with_context(|| format!("mkdir {}", path.display()));
                        }
                    }
                }
            }
            FileOp::Fetch { url, dst } => {
                let (bytes, last_modified) = crate::io::http_get_dated(url)
                    .with_context(|| format!("fetching {url}"))?;
                let target = p(dst);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::write(&target, &bytes)
                    .with_context(|| format!("writing {}", target.display()))?;
                // Stamp the server's `Last-Modified` on the fetched file, because
                // up-to-date checks compare mtimes: MONDO's
                // `tmp/mondo-lastbase.owl` comes back dated months ago, which is
                // what keeps the committed
                // `reports/mondo_base_last_release-report.tsv` from being rebuilt.
                if let Some(t) = last_modified.as_deref().and_then(http_date_secs) {
                    let _ = set_file_mtime(&target, t);
                }
                status!("fetched {url} → {dst} ({} bytes)", bytes.len());
            }
            FileOp::Touch { paths } => {
                for path in paths {
                    let path = p(path);
                    if path.exists() {
                        // Bump mtime; ignore failures (we don't depend on it).
                        let _ = filetime_now(&path);
                    } else {
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                            .with_context(|| format!("touch {}", path.display()))?;
                    }
                }
            }
            FileOp::Concat { src, dst, append } => {
                let mut buf = Vec::new();
                for s in src {
                    let from = p(s);
                    buf.extend(
                        std::fs::read(&from)
                            .with_context(|| format!("cat {}", from.display()))?,
                    );
                }
                let dst = p(dst);
                if *append {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&dst)
                        .with_context(|| format!("append to {}", dst.display()))?;
                    f.write_all(&buf)?;
                } else {
                    std::fs::write(&dst, &buf)
                        .with_context(|| format!("write {}", dst.display()))?;
                }
            }
            FileOp::Sort { input, output, unique } => {
                let text = match input {
                    Some(i) => std::fs::read_to_string(p(i))
                        .with_context(|| format!("sort: reading {i}"))?,
                    None => String::new(),
                };
                // Deterministic byte-wise (LC_ALL=C-equivalent) sort, so output is
                // identical on every platform.
                let mut lines: Vec<&str> = text.lines().collect();
                lines.sort_unstable();
                if *unique {
                    lines.dedup();
                }
                let mut out = lines.join("\n");
                if !out.is_empty() {
                    out.push('\n');
                }
                std::fs::write(p(output), out).with_context(|| format!("sort → {output}"))?;
            }
            FileOp::Print { message, dst, append, newline } => {
                let mut text = message.clone();
                if *newline {
                    text.push('\n');
                }
                match dst {
                    Some(d) => {
                        let d = p(d);
                        if *append {
                            use std::io::Write;
                            let mut f = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&d)
                                .with_context(|| format!("append to {}", d.display()))?;
                            f.write_all(text.as_bytes())?;
                        } else {
                            std::fs::write(&d, text)
                                .with_context(|| format!("write {}", d.display()))?;
                        }
                    }
                    None => print!("{text}"),
                }
            }
        }
        Ok(())
    }
}

/// Seconds since the Unix epoch for an RFC 7231 HTTP date
/// (`Wed, 07 Jul 2026 14:00:00 GMT`), or `None` if it does not parse.
fn http_date_secs(s: &str) -> Option<i64> {
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    // `Day, DD Mon YYYY HH:MM:SS GMT`
    let rest = s.split_once(", ")?.1;
    let mut it = rest.split_whitespace();
    let day: i64 = it.next()?.parse().ok()?;
    let mon_name = it.next()?;
    let mon = MONTHS.iter().position(|m| *m == mon_name)? as i64 + 1;
    let year: i64 = it.next()?.parse().ok()?;
    let mut hms = it.next()?.split(':');
    let (h, mi, sec): (i64, i64, i64) = (
        hms.next()?.parse().ok()?,
        hms.next()?.parse().ok()?,
        hms.next()?.parse().ok()?,
    );
    // Days from the civil epoch (Howard Hinnant's algorithm).
    let y = if mon <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if mon > 2 { mon - 3 } else { mon + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + mi * 60 + sec)
}

/// Set a file's modification time to `secs` since the Unix epoch.
fn set_file_mtime(path: &Path, secs: i64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        // `utimensat(AT_FDCWD, path, times, 0)` via libc is not available here, so
        // use the portable `filetime`-free route: open and set via `File::set_times`
        // (stable since 1.75).
        let f = std::fs::OpenOptions::new().write(true).open(path)?;
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs.max(0) as u64);
        let _ = path.as_os_str().as_bytes();
        f.set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
    }
    #[cfg(not(unix))]
    {
        let f = std::fs::OpenOptions::new().write(true).open(path)?;
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs.max(0) as u64);
        f.set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
    }
}

fn filetime_now(path: &Path) -> std::io::Result<()> {
    // Re-opening for append with no write leaves contents intact; good enough to
    // mark the file present. (We don't rely on the exact mtime anywhere.)
    std::fs::OpenOptions::new().append(true).open(path).map(|_| ())
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Redirections parsed off a single command (`< in`, `> out`, `>> out`).
#[derive(Default)]
struct Redirects {
    stdin: Option<String>,
    stdout: Option<(String, bool)>, // (file, append)
    stderr: Option<(String, bool)>, // (file, append); a file of "&1" inherits stdout
}

/// Run one already-expanded recipe line in `dir`, dispatching a `robot`/`jq`/
/// `sssom` command to the matching owlmake subcommand via the `exe` binary and
/// performing file ops natively.
///
/// `robot_prefix` is the expanded launcher text a recipe puts at command
/// position (e.g. `robot`, or `java -jar robot.jar`), used to recognise — and
/// strip — such an invocation before its arguments reach that subcommand.
pub fn run_line(
    line: &str,
    dir: &Path,
    exe: &Path,
    robot_prefix: &str,
    env: &[(String, String)],
) -> Result<()> {
    RUN_ENV.with(|c| *c.borrow_mut() = env.to_vec());
    // Strip the per-line recipe prefixes: `@` (silent), `+` (always run), and a
    // leading `-` (ignore errors).
    let mut l = line.trim();
    let mut ignore_err = false;
    loop {
        match l.chars().next() {
            Some('@') | Some('+') => l = l[1..].trim_start(),
            Some('-') => {
                ignore_err = true;
                l = l[1..].trim_start();
            }
            _ => break,
        }
    }
    // A line containing a shell control construct (`if … then … fi`,
    // `for … do … done`, `case … esac`) must run as a whole through the shell;
    // splitting it on `;`/`&&`/`||` would tear the construct apart.
    if l.split_whitespace().any(is_control_keyword) {
        let r = run_shell(l, dir, exe, robot_prefix);
        if let Err(e) = r {
            if ignore_err {
                eprintln!("odk:   (ignored) {e:#}");
            } else {
                return Err(e);
            }
        }
        return Ok(());
    }

    // Honour `&&`/`||`/`;` short-circuiting so idioms like `cmd || true`
    // (ignore failure) and `a && b` (run b only if a succeeds) behave as in a
    // shell, instead of running every part unconditionally.
    // `exit` terminates the SHELL, so a line containing one cannot be decomposed and
    // re-sequenced here: `grep … && exit -1 || echo "No errors"` must die with 255
    // when the grep matches, but treating `exit -1` as an ordinary failing part
    // hands control to the `||` and the check reports success with errors present.
    // Hand the whole line to `sh` and let it apply its own short-circuiting.
    if robot::split_shell(l).iter().any(|p| {
        let p = p.trim();
        p == "exit" || p.starts_with("exit ")
    }) {
        let r = run_shell(l, dir, exe, robot_prefix);
        return match r {
            Err(e) if ignore_err => {
                eprintln!("odk:   (ignored) {e:#}");
                Ok(())
            }
            other => other,
        };
    }
    let seq = robot::split_shell_seq(l);
    let mut last_ok = true;
    let mut pending_err: Option<anyhow::Error> = None;
    for (idx, (cmd, _after)) in seq.iter().enumerate() {
        let should_run = match idx.checked_sub(1).and_then(|p| seq[p].1) {
            None | Some(ShellSep::Semi) => true,
            Some(ShellSep::And) => last_ok,
            Some(ShellSep::Or) => !last_ok,
        };
        if !should_run {
            continue; // last_ok / pending_err carry forward
        }
        let cmd = cmd.trim();
        if cmd.is_empty() {
            continue;
        }
        match run_sub(cmd, dir, exe, robot_prefix) {
            Ok(()) => {
                last_ok = true;
                pending_err = None;
            }
            Err(e) => {
                last_ok = false;
                pending_err = Some(e);
            }
        }
    }
    if let Some(e) = pending_err {
        if ignore_err {
            eprintln!("odk:   (ignored) {e:#}");
        } else {
            return Err(e);
        }
    }
    Ok(())
}

/// Execute one `;`/`&&`/`||`-delimited command.
fn run_sub(sub: &str, dir: &Path, exe: &Path, robot_prefix: &str) -> Result<()> {
    let stages = split_pipe(sub);
    let toks = robot::tokenize_quoted(stages[0].trim());
    let Some((head, _)) = toks.first() else { return Ok(()) };

    // Pipelines and shell control constructs keep shell semantics; run them
    // through `sh` with owlmake's own implementations substituted by explicit
    // path. Single text-processing commands likewise stay shell-native.
    if stages.len() > 1 || is_control_keyword(head) {
        return run_shell(sub, dir, exe, robot_prefix);
    }

    let (argv, redir) = strip_redirects(&toks);
    if argv.is_empty() {
        return Ok(());
    }

    // A `$(…)`/backtick substitution has to be evaluated by the shell — decomposing
    // such a command into a `FileOp` would use its text verbatim (see
    // [`has_shell_substitution`]).
    if has_shell_substitution(sub) {
        return run_shell(sub, dir, exe, robot_prefix);
    }

    if let Some(op) = FileOp::parse(&argv) {
        // `FileOp::parse` finds `echo MSG >> FILE`'s destination by scanning its
        // own words, but `argv` has already had every redirection lifted out of
        // it, so the destination has to be put back here or the message goes to
        // stdout. OBA's `tmp/simple_seed.txt` ends with two
        // `echo "…#SubsetProperty" >> $@` lines appending the two oboInOwl
        // metaclasses to the seed; losing them drops both from every `-basic`
        // artefact, because the seed is what `filter --term-file` keeps.
        let op = match (op, &redir.stdout) {
            (FileOp::Print { message, dst: None, newline, .. }, Some((f, append))) => {
                FileOp::Print { message, dst: Some(f.clone()), append: *append, newline }
            }
            (op, _) => op,
        };
        return op.run(dir);
    }
    if robot::is_robot(&argv, robot_prefix) {
        let args = robot::robot_subcommand_args(&argv, robot_prefix);
        return run_tool(exe, &args, dir, &redir);
    }
    if is_jq(head) {
        let mut args = vec!["jq".to_string()];
        args.extend_from_slice(&argv[1..]);
        return run_tool(exe, &args, dir, &redir);
    }
    if let Some(sssom_args) = as_sssom(&argv) {
        return run_tool(exe, &sssom_args, dir, &redir);
    }
    // A lone text processor (perl/grep/sed/echo/…): run it through the shell so
    // its own quoting/globbing/redirection semantics are preserved exactly.
    run_shell(sub, dir, exe, robot_prefix)
}

/// Environment names an owlmake child is allowed to inherit.
///
/// Every entry either has to be there for the process to work at all, or is
/// diagnostic. Nothing here can change what a step writes.
const CHILD_ENV_ALLOWED: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "TMP", "TEMP", "LANG", "LC_ALL", "LC_CTYPE",
    "TERM", "NO_COLOR", "OWLMAKE_COLOR", "COLUMNS",
    "OWLMAKE_PROGRESS", "OWLMAKE_TIMING", "OWLMAKE_TIMESTAMPS",
    "SYSTEMROOT", "USERPROFILE", "APPDATA", // Windows needs these to spawn
];

/// Spawn the bundled `exe` with `args`, honouring stdin/stdout redirections.
///
/// The child is an owlmake process — a decomposed `robot`/`jq`/`sssom` step — so
/// it gets a SEALED environment rather than the parent's. `om jq` exposes the
/// whole process environment to a filter as `$ENV`, as the filter language
/// requires, so an inherited variable could otherwise reach a plan step's data
/// transform. A recipe's own `sh` line still inherits — a shell step is a
/// declared escape hatch — but it receives this run's `VAR=value` assignments
/// explicitly, via `apply_run_env`.
fn run_tool(exe: &Path, args: &[String], dir: &Path, redir: &Redirects) -> Result<()> {
    // As in `run_shell`: the child writes to the inherited stderr.
    let _quiet = crate::progress::Suspend::new();
    let mut cmd = Command::new(exe);
    cmd.args(args).current_dir(dir);
    cmd.env_clear();
    for (k, v) in std::env::vars_os() {
        if k.to_str().is_some_and(|k| CHILD_ENV_ALLOWED.contains(&k)) {
            cmd.env(k, v);
        }
    }
    // This run's `VAR=value` assignments are an explicit run input, so they DO
    // reach the child: a variable given on the command line is exported into
    // every recipe environment it parameterises.
    apply_run_env(&mut cmd);
    // A replayed command needs no signal about how to write RDF/XML: every file
    // owlmake writes gets the same serialisation, in this process or a child.
    if let Some(infile) = &redir.stdin {
        let f = std::fs::File::open(dir.join(infile))
            .with_context(|| format!("opening < {infile}"))?;
        cmd.stdin(Stdio::from(f));
    }
    if let Some((outfile, append)) = &redir.stdout {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(*append)
            .truncate(!*append)
            .open(dir.join(outfile))
            .with_context(|| format!("opening > {outfile}"))?;
        cmd.stdout(Stdio::from(f));
    }
    if let Some((errfile, append)) = &redir.stderr {
        // `2>&1` merges into the stdout destination; otherwise open the file
        // (e.g. `2>/dev/null` discards). A bare `&1` with no stdout file inherits.
        if errfile == "&1" {
            if let Some((outfile, oappend)) = &redir.stdout {
                let f = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .append(*oappend)
                    .truncate(!*oappend)
                    .open(dir.join(outfile))
                    .with_context(|| format!("opening 2>&1 {outfile}"))?;
                cmd.stderr(Stdio::from(f));
            }
        } else {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .append(*append)
                .truncate(!*append)
                .open(dir.join(errfile))
                .with_context(|| format!("opening 2> {errfile}"))?;
            cmd.stderr(Stdio::from(f));
        }
    }
    let status = cmd
        .status()
        .with_context(|| format!("spawning {} {}", exe.display(), args.join(" ")))?;
    if !status.success() {
        bail!("command failed (exit {:?}): {} {}", status.code(), exe.display(), args.join(" "));
    }
    Ok(())
}

thread_local! {
    /// This invocation's `VAR=value` assignments, applied to every child this
    /// recipe spawns. Scoped to the run rather than written into owlmake's own
    /// environment, so one invocation's variables cannot reach a later
    /// invocation in the same process.
    static RUN_ENV: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Apply this run's command-line variable assignments to a child: a `VAR=value`
/// given on the command line is exported into every recipe environment, so the
/// commands a recipe spawns see it too.
fn apply_run_env(cmd: &mut Command) {
    RUN_ENV.with(|c| {
        for (k, v) in c.borrow().iter() {
            cmd.env(k, v);
        }
    });
}

/// Run a command line through `sh -c`. The tools named directly in the line are
/// rewritten to the explicit `exe`; in addition the bundled-tool shim directory
/// is prepended to the shell's `PATH`, so that any *external* script the command
/// invokes (e.g. `python3 build.py`, a project `*.sh`) which itself calls
/// `robot`/`jq`/`sssom`/`sed`/`grep`/`comm` still resolves to the bundled
/// engines — without requiring a system copy.
fn run_shell(sub: &str, dir: &Path, exe: &Path, robot_prefix: &str) -> Result<()> {
    // The child inherits stderr; stop the stage spinner redrawing while it writes.
    let _quiet = crate::progress::Suspend::new();
    let rewritten = rewrite_tools(sub, exe, robot_prefix);
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&rewritten).current_dir(dir);
    prepend_tool_path(&mut cmd, exe);
    apply_run_env(&mut cmd);
    let status = cmd
        .status()
        .with_context(|| format!("spawning recipe command: {rewritten}"))?;
    if !status.success() {
        bail!("recipe command failed (exit {:?}):\n  {sub}", status.code());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bundled-tool shims (for tools reached *inside* an external script).
// ---------------------------------------------------------------------------

/// Prepend the bundled-tool shim directory to `cmd`'s `PATH`, so a script the
/// recipe shells out to which calls bare `robot`/`jq`/`sssom`/`sed`/`grep`/`comm`
/// resolves to the owlmake binary `exe`. A no-op if the shim can't be installed (the command
/// then falls back to whatever the inherited `PATH` provides).
pub fn prepend_tool_path(cmd: &mut Command, exe: &Path) {
    let Some(dir) = shim_dir(exe) else { return };
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut path = std::ffi::OsString::from(dir);
    if !existing.is_empty() {
        path.push(":");
        path.push(&existing);
    }
    cmd.env("PATH", path);
}

/// Directory of bundled-tool shims for `exe`, created once per binary. (In a
/// release there is only ever one `exe` — the running owlmake binary — but
/// keying on it keeps the shims correct under tests, where the live process is
/// the test harness rather than owlmake.)
fn shim_dir(exe: &Path) -> Option<PathBuf> {
    static CACHE: OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(d) = cache.lock().ok()?.get(exe) {
        return Some(d.clone());
    }
    let dir = install_shims(exe).ok()?;
    cache.lock().ok()?.insert(exe.to_path_buf(), dir.clone());
    Some(dir)
}

/// Write tiny `robot`/`jq`/`sssom`/`sed`/`grep`/`comm` shell scripts that re-exec
/// `exe`'s matching subcommand (`robot <sub> …` maps to `owlmake <sub> …`, since
/// owlmake's chaining harness carries those subcommand names), and return their
/// directory.
fn install_shims(exe: &Path) -> std::io::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    exe.hash(&mut h);
    let dir = std::env::temp_dir()
        .join(format!("owlmake-shims-{}-{:x}", std::process::id(), h.finish()));
    std::fs::create_dir_all(&dir)?;
    let shims: [(&str, String); 22] = [
        ("robot", format!("#!/bin/sh\nexec {exe:?} \"$@\"\n")),
        ("jq", format!("#!/bin/sh\nexec {exe:?} jq \"$@\"\n")),
        // A command-line SPARQL runner: MONDO's `mirror-ncbigene` is the only
        // recipe that calls one, and owlmake answers it with its own engine so
        // the refreshed-imports chain needs nothing installed.
        ("arq", format!("#!/bin/sh\nexec {exe:?} arq \"$@\"\n")),
        ("sssom", format!("#!/bin/sh\nexec {exe:?} sssom \"$@\"\n")),
        // KGX: the `<ont>_nodes.tsv`/`_edges.tsv` release artefacts.
        ("kgx", format!("#!/bin/sh\nexec {exe:?} kgx \"$@\"\n")),
        ("dosdp-tools", format!("#!/bin/sh\nexec {exe:?} dosdp \"$@\"\n")),
        ("sssom-cli", format!("#!/bin/sh\nexec {exe:?} sssom transform \"$@\"\n")),
        ("owltools", format!("#!/bin/sh\nexec {exe:?} owltools \"$@\"\n")),
        ("sed", format!("#!/bin/sh\nexec {exe:?} sed \"$@\"\n")),
        ("grep", format!("#!/bin/sh\nexec {exe:?} grep \"$@\"\n")),
        ("comm", format!("#!/bin/sh\nexec {exe:?} comm \"$@\"\n")),
        // gzip: a release asset is published compressed, and the header a system
        // gzip writes carries the clock — so two builds of one database would
        // differ in their first bytes.
        ("gzip", format!("#!/bin/sh\nexec {exe:?} gzip \"$@\"\n")),
        ("gunzip", format!("#!/bin/sh\nexec {exe:?} gunzip \"$@\"\n")),
        ("zcat", format!("#!/bin/sh\nexec {exe:?} zcat \"$@\"\n")),
        // Helper commands a repo's recipes call by name, implemented natively.
        // Nothing else on the box provides them, so without these an `om make
        // test` dies with exit 127 on its first prerequisite.
        ("dicer-cli", format!("#!/bin/sh\nexec {exe:?} dicer-cli \"$@\"\n")),
        ("fastobo-validator", format!("#!/bin/sh\nexec {exe:?} fastobo-validator \"$@\"\n")),
        ("dosdp", format!("#!/bin/sh\nexec {exe:?} dosdp \"$@\"\n")),
        ("check-rdfxml", format!("#!/bin/sh\nexec {exe:?} check-rdfxml \"$@\"\n")),
        (
            "simple_pattern_tester.py",
            format!("#!/bin/sh\nexec {exe:?} simple_pattern_tester.py \"$@\"\n"),
        ),
        ("odk-info", format!("#!/bin/sh\nexec {exe:?} odk-info \"$@\"\n")),
        ("sha256sum", format!("#!/bin/sh\nexec {exe:?} sha256sum \"$@\"\n")),
        // The ontology SQL database (`semsql make <name>.db`), a release asset
        // for repos that publish one.
        ("semsql", format!("#!/bin/sh\nexec {exe:?} semsql \"$@\"\n")),
    ];
    for (name, body) in shims {
        let path = dir.join(name);
        std::fs::write(&path, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(dir)
}

/// Substitute the bundled tools into a shell command by explicit binary path:
/// the launcher text at command position becomes `<exe>`, and bare `jq`/`sssom`
/// words become `<exe> jq` / `<exe> sssom`. An explicit path rather than a `PATH`
/// shim, so a recipe cannot pick up a same-named binary from the environment.
pub fn rewrite_tools(sub: &str, exe: &Path, robot_prefix: &str) -> String {
    let exe = exe.display().to_string();
    let mut out = sub.to_string();
    let robot_prefix = robot_prefix.trim();
    if !robot_prefix.is_empty() {
        if robot_prefix.split_whitespace().count() > 1 {
            // A multi-token launcher is specific enough to substitute as a
            // substring — but only its LAUNCHER part. The launcher routinely
            // carries options: MONDO's is `robot --catalog catalog-v001.xml`, and
            // replacing the whole prefix with `<exe>` would drop the catalog, so a
            // shelled recipe would resolve `owl:imports` by downloading the
            // published file instead of reading the committed one. (`java -jar
            // robot.jar` has no such tail, so it is unaffected.)
            //
            // Keep every token after the launcher. The launcher is the last token
            // before the first option, so `java -jar robot.jar --catalog x` maps to
            // `<exe> --catalog x` and `robot --catalog x` to the same.
            if out.contains(robot_prefix) {
                let toks: Vec<&str> = robot_prefix.split_whitespace().collect();
                // The tail is the longest suffix beginning with a `--long` option.
                // The global options a launcher carries are all long-form, while
                // `java -jar x.jar` uses a single dash — so this keeps
                // `--catalog x` and leaves a JVM launcher alone.
                let start = toks.iter().position(|t| t.starts_with("--"));
                let repl = match start {
                    Some(i) if i > 0 => format!("{exe} {}", toks[i..].join(" ")),
                    _ => exe.clone(),
                };
                out = out.replace(robot_prefix, &repl);
            }
        } else {
            // A bare launcher word (`robot`) — only at command position, so a
            // filename like `robot.owl` is left untouched.
            out = replace_command_word(&out, robot_prefix, &exe);
        }
    }
    out = replace_command_word(&out, "jq", &format!("{exe} jq"));
    out = replace_command_word(&out, "kgx", &format!("{exe} kgx"));
    out = replace_command_word(&out, "dosdp-tools", &format!("{exe} dosdp"));
    // `sssom-cli` before `sssom` so the longer command word wins; a recipe's
    // `sssom-cli` call is served by owlmake's `sssom transform` engine.
    out = replace_command_word(&out, "sssom-cli", &format!("{exe} sssom transform"));
    out = replace_command_word(&out, "sssom", &format!("{exe} sssom"));
    // `owltools` — UBERON's composite `-basic`/`common-anatomy` recipes run it as
    // `OWLTOOLS_MEMORY=… owltools …`; the leading env assignment keeps `owltools`
    // at command position, so it is substituted here for the native engine.
    out = replace_command_word(&out, "owltools", &format!("{exe} owltools"));
    // Helper commands recipes call by name, so a replayed recipe reaches
    // owlmake's implementations without relying on the shim directory being on
    // PATH.
    out = replace_command_word(&out, "dicer-cli", &format!("{exe} dicer-cli"));
    out = replace_command_word(&out, "check-rdfxml", &format!("{exe} check-rdfxml"));
    out = replace_command_word(&out, "odk-info", &format!("{exe} odk-info"));
    out = replace_command_word(&out, "sha256sum", &format!("{exe} sha256sum"));
    out = replace_command_word(&out, "semsql", &format!("{exe} semsql"));
    // Recipe `sed`/`grep`/`comm` calls (in pipelines too) route to the in-binary
    // implementations, which accept the script dialect recipes are written in, so
    // builds don't rely on the machine's own text utilities (absent on Windows,
    // BSD-flavoured on macOS).
    for tool in ["sed", "grep", "comm"] {
        out = replace_command_word(&out, tool, &format!("{exe} {tool}"));
    }
    out
}

/// Replace `word` with `repl` only where it appears as a command name: at the
/// start of the string or right after a `|`, `;`, `&`, `(` (optionally with
/// whitespace), and followed by whitespace. This avoids rewriting `jq`/`sssom`
/// when they occur inside an argument or path.
/// Whether `tok` is a shell environment assignment `NAME=value` (`NAME` an
/// identifier). Such tokens only appear at command position, so a following word
/// is itself at command position.
fn is_env_assignment_word(tok: &str) -> bool {
    match tok.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

fn replace_command_word(s: &str, word: &str, repl: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        // At a command position?
        let at_cmd_pos = {
            // scan back over whitespace
            let mut j = i;
            while j > 0 && (bytes[j - 1] as char).is_whitespace() {
                j -= 1;
            }
            if j == 0 || matches!(bytes[j - 1], b'|' | b';' | b'&' | b'(') {
                true
            } else {
                // The word may follow one or more leading `NAME=value` environment
                // assignments (`OWLTOOLS_MEMORY=20G owltools …`), which keep it at
                // command position. Scan the preceding whitespace-delimited token
                // and accept it if it is such an assignment.
                let end = j;
                let mut k = j;
                while k > 0
                    && !(bytes[k - 1] as char).is_whitespace()
                    && !matches!(bytes[k - 1], b'|' | b';' | b'&' | b'(')
                {
                    k -= 1;
                }
                is_env_assignment_word(&s[k..end])
            }
        };
        if at_cmd_pos && s[i..].starts_with(word) {
            let after = i + word.len();
            let boundary = after >= bytes.len() || (bytes[after] as char).is_whitespace();
            if boundary {
                out.push_str(repl);
                i = after;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Pull trailing `< in`, `> out`, `>> out` redirections off a tokenized command,
/// returning the remaining argv plus the parsed redirections.
fn strip_redirects(toks: &[(String, bool)]) -> (Vec<String>, Redirects) {
    let mut argv = Vec::new();
    let mut redir = Redirects::default();
    let mut i = 0usize;
    // The token after a bare `<`/`>`/`2>` operator is its target, whatever it
    // looks like.
    let mut take_next = |i: &mut usize| -> Option<String> {
        *i += 1;
        toks.get(*i).map(|(t, _)| t.clone())
    };
    while i < toks.len() {
        let (t, quoted) = (&toks[i].0, toks[i].1);
        // A quoted token is never a redirection, however it starts — that is what
        // distinguishes a `--select "<…/BFO_*>"` argument from `< file`.
        if quoted {
            argv.push(t.clone());
            i += 1;
            continue;
        }
        match t.as_str() {
            "<" => redir.stdin = take_next(&mut i),
            ">" => redir.stdout = take_next(&mut i).map(|f| (f, false)),
            ">>" => redir.stdout = take_next(&mut i).map(|f| (f, true)),
            "2>" => redir.stderr = take_next(&mut i).map(|f| (f, false)),
            "2>>" => redir.stderr = take_next(&mut i).map(|f| (f, true)),
            // stderr forms glued to their target: `2>>log`, `2>/dev/null`, `2>&1`
            // (a target of "&1" means "inherit stdout" — see `run_tool`).
            s if s.starts_with("2>>") => redir.stderr = Some((s[3..].to_string(), true)),
            s if s.starts_with("2>") => redir.stderr = Some((s[2..].to_string(), false)),
            s if s.starts_with(">>") => redir.stdout = Some((s[2..].to_string(), true)),
            s if s.starts_with('>') => redir.stdout = Some((s[1..].to_string(), false)),
            s if s.starts_with('<') => redir.stdin = Some(s[1..].to_string()),
            _ => argv.push(t.clone()),
        }
        i += 1;
    }
    (argv, redir)
}

/// Whether a command line contains a top-level single `|` (a pipeline).
pub(crate) fn has_pipe(s: &str) -> bool {
    split_pipe(s).len() > 1
}

/// Split a command line on top-level `|` (single pipe), respecting quotes and
/// not splitting `||`.
pub(crate) fn split_pipe(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'|' if i + 1 < bytes.len() && bytes[i + 1] == b'|' => {
                    i += 2; // `||` is not a pipe
                    continue;
                }
                b'|' => {
                    parts.push(s[start..i].to_string());
                    i += 1;
                    start = i;
                    continue;
                }
                _ => {}
            },
        }
        i += 1;
    }
    parts.push(s[start..].to_string());
    parts
}

fn is_control_keyword(t: &str) -> bool {
    matches!(
        t,
        "if" | "then" | "else" | "elif" | "fi" | "for" | "while" | "until" | "do" | "done"
            | "case" | "esac" | "{" | "}" | "("
    )
}

fn is_jq(tok: &str) -> bool {
    tok == "jq" || tok.ends_with("/jq")
}

/// If `argv` is an `sssom …` or `sssom:<cmd> …` invocation, return the argv to
/// pass to the bundled `owlmake sssom` (the colon form keeps its `sssom:` head,
/// which the binary routes to the matching subcommand).
fn as_sssom(argv: &[String]) -> Option<Vec<String>> {
    let head = &argv[0];
    let base = head.rsplit('/').next().unwrap_or(head);
    if base == "sssom" {
        let mut out = vec!["sssom".to_string()];
        out.extend_from_slice(&argv[1..]);
        Some(out)
    } else if base.starts_with("sssom:") {
        let mut out = vec![base.to_string()];
        out.extend_from_slice(&argv[1..]);
        Some(out)
    } else {
        None
    }
}

/// Whether a command's text contains a shell substitution — `$(…)` or a
/// backtick — whose value only exists at run time. Such a command cannot be
/// decomposed into a declarative [`FileOp`] or dismissed as benign: it has to be
/// executed. (A `$$(…)` written in a recipe reaches this point already unescaped
/// as `$(…)`.)
pub fn has_shell_substitution(s: &str) -> bool {
    s.contains("$(") || s.contains('`')
}

/// The owlmake binary that runs the bundled tools (cached). Falls back to the
/// literal name if the current exe can't be resolved.
pub fn owlmake_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("owlmake"))
}

/// Run one recorded command line in `dir`, with the same decomposition
/// [`run_line`] applies. Exposed so the executor can run a shell-shaped *step*
/// where it sits in the pipeline, instead of abandoning the step list and
/// replaying a whole recipe: the step list is the complete description of the
/// build, so no recipe text has to travel alongside it.
pub fn run_step_command(
    cmd: &str,
    dir: &Path,
    robot_prefix: &str,
) -> Result<()> {
    run_line(cmd, dir, &owlmake_exe(), robot_prefix, &[])
}

/// Invoke the owlmake binary directly with `args` in argv order (no shell, no
/// redirection), for steps that recorded their tokens rather than a line:
/// `Jq`, `Sssom`, `CliRobot`.
pub fn run_owlmake_args(args: &[String], dir: &Path) -> Result<()> {
    run_tool(&owlmake_exe(), args, dir, &Redirects::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(s: &str) -> Vec<String> {
        robot::tokenize(s)
    }

    /// Marked tokens, as `run_sub` produces them for `strip_redirects`.
    fn qtok(s: &str) -> Vec<(String, bool)> {
        robot::tokenize_quoted(s)
    }

    #[test]
    fn a_quoted_iri_selector_is_not_a_redirection() {
        // MONDO's `mondo-base.owl` recipe passes
        // `--select "<http://purl.obolibrary.org/obo/BFO_*>"`. Unquoted that
        // WOULD be an input redirection; quoted it is an argument, and losing the
        // distinction fails the rule with "No such file or directory".
        let (argv, redir) =
            strip_redirects(&qtok("remove --select \"<http://purl.obolibrary.org/obo/BFO_*>\" --select classes"));
        assert_eq!(
            argv,
            vec!["remove", "--select", "<http://purl.obolibrary.org/obo/BFO_*>", "--select", "classes"]
        );
        assert!(redir.stdin.is_none());
    }

    #[test]
    fn strips_stderr_redirects() {
        // `2>/dev/null` (combined) and `2> file` (split) are pulled off the argv,
        // not passed through as bogus arguments to the dispatched tool.
        let (argv, redir) = strip_redirects(&qtok("query -i x.owl -q q.rq out.tsv 2>/dev/null"));
        assert_eq!(argv, tok("query -i x.owl -q q.rq out.tsv"));
        assert_eq!(redir.stderr.as_ref().map(|(f, _)| f.as_str()), Some("/dev/null"));

        let (argv, redir) = strip_redirects(&qtok("robot foo 2> errs.log"));
        assert_eq!(argv, tok("robot foo"));
        assert_eq!(redir.stderr, Some(("errs.log".to_string(), false)));
    }

    #[test]
    fn shell_seq_operators_and_shortcircuit() {
        use crate::odk::robot::{split_shell_seq, ShellSep};
        let seq = split_shell_seq("a 2>/dev/null || true");
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0].1, Some(ShellSep::Or));
        assert_eq!(seq[1].0.trim(), "true");

        let seq = split_shell_seq("make x && make y ; echo done");
        assert_eq!(seq.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
            vec![Some(ShellSep::And), Some(ShellSep::Semi), None]);
    }

    #[test]
    fn parses_file_ops() {
        assert!(matches!(
            FileOp::parse(&tok("cp a.owl b.owl")),
            Some(FileOp::Copy { .. })
        ));
        assert!(matches!(
            FileOp::parse(&tok("mv tmp/x.owl x.owl")),
            Some(FileOp::Move { .. })
        ));
        assert!(matches!(
            FileOp::parse(&tok("rm -rf build")),
            Some(FileOp::Remove { recursive: true, force: true, .. })
        ));
        assert!(matches!(
            FileOp::parse(&tok("mkdir -p a/b")),
            Some(FileOp::Mkdir { parents: true, .. })
        ));
        assert!(matches!(FileOp::parse(&tok("touch done")), Some(FileOp::Touch { .. })));
        // Not a file op.
        assert!(FileOp::parse(&tok("grep -v x f")).is_none());
        // A `cp` with an unmodelled flag falls back (None) rather than misfiring.
        assert!(FileOp::parse(&tok("cp --reflink=auto a b")).is_none());
    }

    #[test]
    fn runs_file_ops() {
        let dir = std::env::temp_dir().join(format!("owlmake-recipe-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();

        FileOp::parse(&tok("cp a.txt b.txt")).unwrap().run(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "hello");

        FileOp::parse(&tok("mkdir -p sub/deep")).unwrap().run(&dir).unwrap();
        assert!(dir.join("sub/deep").is_dir());

        FileOp::parse(&tok("mv b.txt sub/c.txt")).unwrap().run(&dir).unwrap();
        assert!(!dir.join("b.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("sub/c.txt")).unwrap(), "hello");

        FileOp::parse(&tok("touch marker")).unwrap().run(&dir).unwrap();
        assert!(dir.join("marker").exists());

        FileOp::parse(&tok("rm -rf sub")).unwrap().run(&dir).unwrap();
        assert!(!dir.join("sub").exists());
        // `rm -f` on a missing path is a no-op.
        FileOp::parse(&tok("rm -f gone.txt")).unwrap().run(&dir).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipe_and_redirect_parsing() {
        assert!(has_pipe("robot convert -i x | jq ."));
        assert!(!has_pipe("a || b"));
        assert!(!has_pipe("echo hi"));

        let (argv, redir) = strip_redirects(&qtok("jq -r .id < in.json > out.txt"));
        assert_eq!(argv, vec!["jq", "-r", ".id"]);
        assert_eq!(redir.stdin.as_deref(), Some("in.json"));
        assert_eq!(redir.stdout, Some(("out.txt".to_string(), false)));

        let (_, redir2) = strip_redirects(&qtok("echo x >> log"));
        assert_eq!(redir2.stdout, Some(("log".to_string(), true)));
    }

    #[test]
    fn rewrite_substitutes_robot_prefix() {
        let exe = Path::new("/opt/owlmake");
        let r = rewrite_tools("java -jar robot.jar merge -i a -o b", exe, "java -jar robot.jar");
        assert_eq!(r, "/opt/owlmake merge -i a -o b");
    }

    #[test]
    fn rewrite_bare_robot_only_at_command_position() {
        let exe = Path::new("/opt/owlmake");
        // The launcher word at the start is rewritten…
        let r = rewrite_tools("robot merge -i robot.owl -o out.owl", exe, "robot");
        assert_eq!(r, "/opt/owlmake merge -i robot.owl -o out.owl");
        // …but a filename containing the word is left intact.
        assert!(r.contains("robot.owl"));
    }

    #[test]
    fn rewrite_routes_bundled_text_utilities() {
        let exe = Path::new("/opt/owlmake");
        // `sed`/`grep`/`comm` at a command position become `<exe> <tool>`, in a
        // pipeline too — so recipe pipelines use the in-binary implementations.
        let r = rewrite_tools("grep foo a.txt | sed 's/x/y/' | comm -12 - b.txt", exe, "robot");
        assert_eq!(
            r,
            "/opt/owlmake grep foo a.txt | /opt/owlmake sed 's/x/y/' | /opt/owlmake comm -12 - b.txt"
        );
        // A filename that merely contains a tool word is left untouched.
        let r = rewrite_tools("cp sed.txt grep.bak", exe, "robot");
        assert_eq!(r, "cp sed.txt grep.bak");
    }

    #[test]
    fn shim_dir_has_bundled_tools() {
        let mut cmd = Command::new("sh");
        prepend_tool_path(&mut cmd, &owlmake_exe());
        let path = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("PATH"))
            .and_then(|(_, v)| v)
            .expect("PATH set")
            .to_string_lossy()
            .into_owned();
        let first = Path::new(path.split(':').next().unwrap());
        for tool in ["robot", "jq", "sssom", "sed", "grep", "comm", "dicer-cli", "dosdp", "check-rdfxml", "odk-info", "sha256sum"] {
            assert!(first.join(tool).is_file(), "shim dir missing {tool}");
        }
    }
}

#[cfg(test)]
mod robot_prefix_tests {
    /// A recipe's launcher usually carries options (`robot --catalog
    /// catalog-v001.xml`). Substituting the whole prefix with the owlmake binary
    /// must not drop them: without the catalog, `owl:imports` resolution goes to
    /// the network instead of the catalog's local files.
    #[test]
    fn robot_prefix_options_survive_substitution() {
        let exe = std::path::Path::new("/opt/om");
        let got = super::rewrite_tools(
            "robot --catalog catalog-v001.xml merge -i x.obo -o y.owl",
            exe,
            "robot --catalog catalog-v001.xml",
        );
        assert_eq!(got, "/opt/om --catalog catalog-v001.xml merge -i x.obo -o y.owl");
    }

    /// A JVM launcher has no option tail to keep (`-jar` is single-dash).
    #[test]
    fn jvm_launcher_has_no_option_tail() {
        let exe = std::path::Path::new("/opt/om");
        let got = super::rewrite_tools(
            "java -jar robot.jar merge -i x.obo",
            exe,
            "java -jar robot.jar",
        );
        assert_eq!(got, "/opt/om merge -i x.obo");
    }
}
