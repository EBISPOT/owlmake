//! Bundled GNU-compatible text utilities (`sed`, `comm`, `grep`).
//!
//! Ontology build recipes routinely shell out to `sed` and `comm` — EFO's and
//! OBA's alone use each of them in dozens of steps. `grep` is bundled alongside
//! them so a recipe that reaches for it is not a wall either. Satisfying them
//! from inside the single static binary — exactly as the embedded `jq` engine
//! does — removes the dependency on system coreutils, which is both a
//! portability win (no `sed`/`comm` on a bare Windows box) and a *correctness*
//! win (macOS ships BSD `sed`/`comm`, whose behaviour differs from the GNU
//! tools the recipes were written against).
//!
//! Every backend here is pure Rust, preserving the no-C-toolchain,
//! single-static-binary property the crate is built around:
//!
//!   * `sed`  — uutils' POSIX/GNU `sed` (`sed` crate). BRE-compatible, which
//!     matters: the `sed` scripts in build recipes use basic regular expressions.
//!   * `comm` — uutils' `comm` (`uu_comm` crate).
//!   * `grep` — a thin CLI (see [`grep`]) over ripgrep's pure-Rust matcher /
//!     searcher / printer libraries. We deliberately avoid uutils' `uu_grep`,
//!     which links the Oniguruma C library and would break the static binary.
//!
//! The uutils backends expose a `uumain(args: impl uucore::Args) -> i32` entry
//! point (the `#[uucore::main]` proc-macro wraps their `UResult`-returning body
//! into an exit-code-returning function), so driving them is just a matter of
//! handing over an argv whose first element is the program name.

use std::ffi::OsString;

pub mod grep;

/// Run the bundled `sed` (uutils' POSIX/GNU sed). `args` are the arguments
/// *after* the `sed` word. Returns the process exit code.
pub fn sed_main(args: &[String]) -> i32 {
    // (The published `sed` 0.1.1 carries its own older `uucore`, separate from
    // the `uucore` this crate depends on, and does not need the localization
    // setup `comm` does below — driving its `uumain` directly is enough.)
    sed::sed::uumain(argv("sed", &normalize_in_place(args)))
}

/// GNU `sed`'s `-i` takes an OPTIONAL suffix, and only when ATTACHED (`-i.bak`);
/// a following token is the script. The bundled sed 0.1.1 instead wants the
/// suffix as a separate value, so `sed -i 's/x//' f` would swallow the script as
/// the suffix and then read stdin, failing with "Reading metadata of '-' for
/// in-place edit". Give the parser the empty suffix it is looking for.
fn normalize_in_place(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len() + 1);
    let mut past_options = false;
    for a in args {
        // Only rewrite while still in the option section; a literal `-i` operand
        // after `--` (or after the script) is a filename, not a flag.
        if !past_options && (a == "-i" || a == "--in-place") {
            out.push(a.clone());
            out.push(String::new());
            continue;
        }
        if a == "--" || !a.starts_with('-') {
            past_options = true;
        }
        out.push(a.clone());
    }
    out
}

#[cfg(test)]
mod sed_tests {
    #[test]
    fn bare_in_place_gains_an_empty_suffix() {
        let a: Vec<String> = ["-i", "s/x//", "f.txt"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::normalize_in_place(&a), vec!["-i", "", "s/x//", "f.txt"]);
    }
    /// An ATTACHED suffix is already the form the parser wants.
    #[test]
    fn attached_suffix_is_untouched() {
        let a: Vec<String> = ["-i.bak", "s/x//", "f.txt"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::normalize_in_place(&a), a);
    }
    /// A file literally named `-i` (after the script) is an operand.
    #[test]
    fn operand_named_dash_i_is_untouched() {
        let a: Vec<String> = ["s/x//", "-i"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::normalize_in_place(&a), a);
    }
}

/// Run the bundled `comm` (uutils). `args` are the arguments after `comm`.
pub fn comm_main(args: &[String]) -> i32 {
    // uutils 0.9 localizes help/diagnostic text via embedded Fluent resources;
    // the `bin!` macro normally initializes this. We bypass `bin!`, so set it up
    // best-effort — functionality does not depend on it, only message wording.
    let _ = uucore::locale::setup_localization("comm");
    uu_comm::uumain(argv("comm", args))
}

/// Run the bundled `grep`. `args` are the arguments after `grep`.
pub fn grep_main(args: &[String]) -> i32 {
    grep::main(args)
}

/// Run the bundled `gzip`. `args` are the arguments after `gzip`.
///
/// `gzip <file>` writes `<file>.gz` and removes the original; `-d` decompresses
/// the other way, `-c` writes to stdout and keeps the input, `-k` keeps it, and
/// with no operand the data comes from stdin and goes to stdout. A `-1`..`-9`
/// level is accepted and ignored — the compressed bytes are the same for the
/// same input whatever the caller asks for, which is the point.
///
/// The header records no modification time and no original file name, so
/// compressing the same bytes twice gives the same file.
pub fn gzip_main(args: &[String]) -> i32 {
    use std::io::{Read, Write};
    let mut decompress = false;
    let mut to_stdout = false;
    let mut keep = false;
    let mut files: Vec<&str> = Vec::new();
    let mut operands_only = false;
    for a in args {
        if operands_only || !a.starts_with('-') || a == "-" {
            files.push(a);
            continue;
        }
        match a.as_str() {
            "--" => operands_only = true,
            "-d" | "--decompress" | "--uncompress" => decompress = true,
            "-c" | "--stdout" | "--to-stdout" => to_stdout = true,
            "-k" | "--keep" => keep = true,
            "-f" | "--force" | "-n" | "--no-name" | "-q" | "--quiet" | "-v" | "--verbose" => {}
            _ if a.len() == 2 && a.as_bytes()[1].is_ascii_digit() => {}
            _ => {
                eprintln!("gzip: unrecognized option `{a}`");
                return 2;
            }
        }
    }
    let convert = |data: &[u8]| -> std::io::Result<Vec<u8>> {
        if decompress {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(data).read_to_end(&mut out)?;
            Ok(out)
        } else {
            let mut enc = flate2::GzBuilder::new()
                .mtime(0)
                .write(Vec::new(), flate2::Compression::default());
            enc.write_all(data)?;
            enc.finish()
        }
    };
    if files.is_empty() || files == ["-"] {
        let mut data = Vec::new();
        if std::io::stdin().read_to_end(&mut data).is_err() {
            return 1;
        }
        match convert(&data) {
            Ok(out) => {
                let _ = std::io::stdout().write_all(&out);
                0
            }
            Err(e) => {
                eprintln!("gzip: {e}");
                1
            }
        }
    } else {
        for f in files {
            let data = match std::fs::read(f) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("gzip: {f}: {e}");
                    return 1;
                }
            };
            let out = match convert(&data) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("gzip: {f}: {e}");
                    return 1;
                }
            };
            if to_stdout {
                let _ = std::io::stdout().write_all(&out);
                continue;
            }
            let dst = if decompress {
                f.strip_suffix(".gz").unwrap_or(f).to_string()
            } else {
                format!("{f}.gz")
            };
            if let Err(e) = std::fs::write(&dst, &out) {
                eprintln!("gzip: {dst}: {e}");
                return 1;
            }
            if !keep {
                let _ = std::fs::remove_file(f);
            }
        }
        0
    }
}

#[cfg(test)]
mod gzip_tests {
    use std::io::Read;

    /// The same bytes compress to the same file: no clock in the header, and no
    /// original name.
    #[test]
    fn the_same_input_gives_the_same_file() {
        let dir = std::env::temp_dir().join(format!("om-gzip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, b"the same bytes\n").unwrap();
        std::fs::write(&b, b"the same bytes\n").unwrap();
        let run = |p: &std::path::Path| {
            assert_eq!(super::gzip_main(&[p.display().to_string()]), 0);
        };
        run(&a);
        run(&b);
        let ga = std::fs::read(dir.join("a.txt.gz")).unwrap();
        let gb = std::fs::read(dir.join("b.txt.gz")).unwrap();
        assert_eq!(ga, gb, "two compressions of one input differ");
        assert_eq!(&ga[4..8], &[0, 0, 0, 0], "the header carries a modification time");
        // …and the file is real gzip, readable back.
        let mut out = String::new();
        flate2::read::GzDecoder::new(&ga[..]).read_to_string(&mut out).unwrap();
        assert_eq!(out, "the same bytes\n");
        // The original is gone, as `gzip <file>` leaves it.
        assert!(!a.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Build a uutils-style argv iterator: the program `name` followed by `args`.
fn argv(name: &str, args: &[String]) -> std::vec::IntoIter<OsString> {
    std::iter::once(OsString::from(name))
        .chain(args.iter().map(OsString::from))
        .collect::<Vec<_>>()
        .into_iter()
}
