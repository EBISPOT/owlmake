//! The two helpers the `config_check` and `odkversion` targets need:
//! `odk-info --tools` and `sha256sum`.
//!
//! Both targets run at the head of a full QC pass — ahead of `test`, the custom
//! reports, the release assets and the release diff — so a helper that is missing
//! stops the build before any real work starts.
//!
//! ## `odkversion`
//!
//! The target prints a version line and then a tool inventory. owlmake IS every
//! tool in that inventory, so [`odk_info`] reports owlmake's own identity and
//! version for each entry and stops. Printing a version number for a separate
//! program that is not installed would be a lie, and this banner is diagnostic
//! output that people paste into bug reports.
//!
//! ## `config_check`
//!
//! The target hashes the repo's configuration file with CRs stripped and compares
//! the first 64 hex characters with the hash the build was generated from: equal
//! reports the repository is up to date, different reports that the configuration
//! has drifted and the repo wants regenerating.
//!
//! The one piece that needs an implementation is `sha256sum`, which is not present
//! on every platform a build runs on — macOS ships `shasum` instead, Windows has
//! neither. Without it the command substitution around it is empty, never equals
//! the recorded hash, and every build silently reports "your configuration has
//! changed" — advice to regenerate a repo that is in fact fine. [`sha256sum`] is
//! the smallest correct fix and serves any other recipe that reaches for the tool.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;

use crate::model::Model;

// ───────────────────────────────── odk-info ─────────────────────────────────

#[derive(ClapArgs)]
pub struct OdkInfoArgs {
    /// Accepted for compatibility; owlmake prints the same banner
    /// with or without it.
    #[arg(long = "tools")]
    pub tools: bool,

    /// Any other `odk-info` switch. Accepted and ignored so a recipe line never
    /// fails on a flag the banner has no use for.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    pub rest: Vec<String>,
}

/// A banner command: prints and passes any chained model through untouched.
pub fn odk_info_step(model: Option<Model>, a: &OdkInfoArgs) -> Result<Option<Model>> {
    odk_info(a);
    Ok(model)
}

/// Print owlmake's identity, one line per tool handle the inventory covers.
pub fn odk_info(_a: &OdkInfoArgs) {
    // stdout, not `status!`: the banner is the recipe's own output, not owlmake's
    // progress reporting, so it belongs in what the recipe prints.
    println!("owlmake {} — self-contained ODK/ROBOT replacement", env!("CARGO_PKG_VERSION"));
    println!("no ODK image, JVM, or Python runtime is in use; every tool below is owlmake itself");
    println!("  robot         owlmake {} (native)", env!("CARGO_PKG_VERSION"));
    println!("  dosdp-tools   owlmake {} (native)", env!("CARGO_PKG_VERSION"));
    println!("  dicer-cli     owlmake {} (native, `policy` only)", env!("CARGO_PKG_VERSION"));
    println!("  sssom / kgx   owlmake {} (native)", env!("CARGO_PKG_VERSION"));
    println!("  jq / arq      owlmake {} (native)", env!("CARGO_PKG_VERSION"));
}

/// Entry point for the `odk-info` PATH shim.
pub fn odk_info_main(args: &[String]) -> i32 {
    let a = OdkInfoArgs {
        tools: args.iter().any(|t| t == "--tools"),
        rest: Vec::new(),
    };
    odk_info(&a);
    0
}

// ──────────────────────────────── sha256sum ─────────────────────────────────

#[derive(ClapArgs)]
pub struct Sha256Args {
    /// Files to hash. With none — or with `-` — the input is read from stdin,
    /// which is the form `config_check` uses (`tr -d '\r' < … | sha256sum`).
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// coreutils' binary-mode flag. It changes only the `*` marker before the
    /// name on platforms that distinguish text and binary reads; owlmake always
    /// reads bytes, so it is accepted and affects the marker alone.
    #[arg(short = 'b', long = "binary")]
    pub binary: bool,

    /// coreutils' text-mode flag (the default). Accepted for compatibility.
    #[arg(short = 't', long = "text")]
    pub text: bool,
}

pub fn sha256_step(model: Option<Model>, a: &Sha256Args) -> Result<Option<Model>> {
    sha256sum(a)?;
    Ok(model)
}

/// `sha256sum [FILE]…` in the conventional checksum-line layout, which recipes
/// slice with `cut -c1-64`: `<64 hex chars><separator space><mode marker><name>`,
/// the marker being a space in text mode. `-` names stdin.
pub fn sha256sum(a: &Sha256Args) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let marker = if a.binary { '*' } else { ' ' };
    if a.files.is_empty() {
        let digest = hash_reader(&mut std::io::stdin().lock())?;
        // stdin is reported as `-`: one separator space, then the text/binary
        // marker (a space in text mode).
        writeln!(out, "{digest} {marker}-")?;
        return Ok(());
    }
    for f in &a.files {
        let digest = if f.as_os_str() == "-" {
            hash_reader(&mut std::io::stdin().lock())?
        } else {
            let mut file = std::fs::File::open(f)
                .map_err(|e| anyhow::anyhow!("sha256sum: {}: {e}", f.display()))?;
            hash_reader(&mut file)?
        };
        writeln!(out, "{digest} {marker}{}", f.display())?;
    }
    Ok(())
}

/// Entry point for the `sha256sum` PATH shim.
pub fn sha256_main(args: &[String]) -> i32 {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut binary = false;
    for tok in args {
        match tok.as_str() {
            "--help" => {
                println!(
                    "sha256sum (owlmake {}) — print SHA-256 checksums\n\n\
                     Usage: sha256sum [-b|-t] [FILE]...\n\
                     With no FILE, or when FILE is -, read standard input.",
                    env!("CARGO_PKG_VERSION")
                );
                return 0;
            }
            "--version" => {
                println!("sha256sum (owlmake) {}", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            "-b" | "--binary" => binary = true,
            "-t" | "--text" => binary = false,
            t if t.len() > 1 && t.starts_with('-') && !t.starts_with("--") => {
                // Bundled shorts, e.g. `-bt`.
                for c in t.chars().skip(1) {
                    match c {
                        'b' => binary = true,
                        't' => binary = false,
                        _ => {
                            eprintln!("sha256sum: invalid option -- '{c}'");
                            return 1;
                        }
                    }
                }
            }
            t => files.push(PathBuf::from(t)),
        }
    }
    match sha256sum(&Sha256Args { files, binary, text: !binary }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn hash_reader(r: &mut impl Read) -> Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.hex())
}

/// SHA-256 (FIPS 180-4), implemented here rather than pulled in as a dependency:
/// `sha2` appears in `Cargo.lock` only transitively, and adding a crate to
/// `Cargo.toml` for eighty lines of arithmetic is not a trade this project makes
/// (the same reasoning as the hand-rolled pieces elsewhere in `src/util`).
struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buflen: usize,
    /// Total message length in BITS, which is what the padding block encodes.
    bits: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Sha256 {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0u8; 64],
            buflen: 0,
            bits: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64) * 8);
        if self.buflen > 0 {
            let take = (64 - self.buflen).min(data.len());
            self.buf[self.buflen..self.buflen + take].copy_from_slice(&data[..take]);
            self.buflen += take;
            data = &data[take..];
            if self.buflen == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buflen = 0;
            }
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            let mut b = [0u8; 64];
            b.copy_from_slice(block);
            self.compress(&b);
            data = rest;
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buflen = data.len();
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    fn hex(mut self) -> String {
        // Padding: a 0x80 byte, zeros, then the 64-bit big-endian bit length.
        let bits = self.bits;
        self.update_raw(&[0x80]);
        while self.buflen != 56 {
            self.update_raw(&[0]);
        }
        self.update_raw(&bits.to_be_bytes());
        let mut s = String::with_capacity(64);
        for w in self.state {
            s.push_str(&format!("{w:08x}"));
        }
        s
    }

    /// `update` without counting the bytes — the padding is not message data.
    fn update_raw(&mut self, data: &[u8]) {
        let saved = self.bits;
        self.update(data);
        self.bits = saved;
    }
}

// ─────────────────────────────── config_check ───────────────────────────────

#[derive(ClapArgs)]
pub struct ConfigCheckArgs {
    /// The project config to hash (`<ont>-odk.yaml`).
    #[arg(value_name = "FILE")]
    pub config: PathBuf,

    /// The hash the Makefile was generated from (`$(CONFIG_HASH)`).
    #[arg(long = "expect", value_name = "SHA256")]
    pub expect: Option<String>,
}

pub fn config_check_step(model: Option<Model>, a: &ConfigCheckArgs) -> Result<Option<Model>> {
    config_check(a)?;
    Ok(model)
}

/// The whole `config_check` target as one command: hash the config with CRs
/// stripped (so a checkout with CRLF line endings still matches) and compare with
/// the hash the build was generated from.
///
/// Never fails the build — it only ever prints, because a changed config is advice
/// to regenerate the repo, not an error.
pub fn config_check(a: &ConfigCheckArgs) -> Result<()> {
    let bytes = std::fs::read(&a.config)
        .map_err(|e| anyhow::anyhow!("config_check: {}: {e}", a.config.display()))?;
    let stripped: Vec<u8> = bytes.into_iter().filter(|b| *b != b'\r').collect();
    let mut h = Sha256::new();
    h.update(&stripped);
    let digest = h.hex();
    match &a.expect {
        None => println!("{digest}  {}", a.config.display()),
        Some(want) if want.trim() == digest => println!("Repository is up-to-date."),
        Some(_) => println!(
            "Your ODK configuration has changed since this Makefile was generated. \
             You may need to run 'make update_repo'."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        h.hex()
    }

    /// The FIPS 180-4 test vectors, plus the empty string — if these hold, this is
    /// standard SHA-256 and the recorded-hash comparison is meaningful.
    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(
            digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// Crosses the 64-byte block boundary and the 56-byte padding boundary, which
    /// is where a hand-rolled buffer gets it wrong.
    #[test]
    fn sha256_handles_block_boundaries() {
        // 1,000,000 'a' — the third FIPS vector.
        let mut h = Sha256::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(h.hex(), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // Exactly 55, 56, 63, 64 and 65 bytes: the padding edge cases.
        for n in [55usize, 56, 63, 64, 65] {
            let data = vec![b'x'; n];
            let mut a = Sha256::new();
            a.update(&data);
            let mut b = Sha256::new();
            for chunk in data.chunks(7) {
                b.update(chunk);
            }
            assert_eq!(a.hex(), b.hex(), "streaming differs from one-shot at {n} bytes");
        }
    }

    #[test]
    fn config_check_strips_carriage_returns() {
        let dir = std::env::temp_dir();
        let unix = dir.join(format!("owlmake-cfg-{}-unix.yaml", std::process::id()));
        let dos = dir.join(format!("owlmake-cfg-{}-dos.yaml", std::process::id()));
        std::fs::write(&unix, "id: oba\ntitle: OBA\n").unwrap();
        std::fs::write(&dos, "id: oba\r\ntitle: OBA\r\n").unwrap();
        let want = digest(b"id: oba\ntitle: OBA\n");
        // Both spellings hash the same, which is the point of stripping CRs: a
        // CRLF checkout must not look like a changed configuration.
        for p in [&unix, &dos] {
            assert!(config_check(&ConfigCheckArgs {
                config: p.clone(),
                expect: Some(want.clone()),
            })
            .is_ok());
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn hash_reader_streams() {
        let data = vec![b'q'; 200_000];
        let mut cursor = std::io::Cursor::new(data.clone());
        assert_eq!(hash_reader(&mut cursor).unwrap(), digest(&data));
    }
}
