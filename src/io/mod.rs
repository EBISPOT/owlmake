//! Multi-format ontology serialization: load and save across every syntax an
//! ontology build exchanges.
//!
//! Reading: horned-owl parses RDF/XML, OWL/XML and OWL Functional Syntax;
//! oxigraph parses Turtle and N-Triples; OBO format, OBO Graphs JSON and
//! Manchester are owlmake's own parsers.
//!
//! Writing: horned-owl emits OWL/XML and Functional Syntax, oxigraph the RDF
//! syntaxes, and owlmake's own writers cover OBO format, OBO Graphs JSON,
//! Manchester and RDF/XML. RDF/XML has two writers — every file goes through
//! `owlrdf.rs`, and horned-owl's serves only the internal buffers owlmake parses
//! straight back itself (see [`RdfXmlWriter`]).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use horned_owl::curie::PrefixMapping;

use horned_owl::io::ParserConfiguration;

use crate::model::{CmOnto, Model, Onto};

/// The two global switches that change what a run PRODUCES, held for the run
/// rather than passed to every one of the ~40 `load`/`save_as` call sites.
///
/// There are exactly two writers, both of which say where the value came from,
/// and no reader outside this module:
///
///   * [`set_run_options`] from `crate::build::execute_plan`, out of the PLAN
///     (`Plan::strict` / `Plan::xml_entities`) — the build path;
///   * [`latch_run_options`] from `crate::cmd::CommonArgs::activate`, out of the
///     user's explicit flag, latching so a chain cannot turn itself off.
///
/// `om make` refuses both flags outright, so the two writers can never contend.
///
/// A per-call parameter would be the obvious alternative and is deliberately not
/// used: both values are properties of the RUN — one plan carries one value, one
/// invocation carries one value — so threading a run-scoped constant through
/// every loader would add a parameter that is never allowed to differ between two
/// calls, while `io::load(path)` keeping its signature would silently DROP
/// `Plan::strict` at any site that had not been converted.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunOptions {
    /// `--strict`: reject structurally-broken RDF instead of repairing it. Off by
    /// default — owlmake parses laxly so import-sourced, locally-undeclared object
    /// properties survive a round-trip (see `load_from_raw`).
    pub strict: bool,
    /// `-x`/`--xml-entities`: emit `&prefix;` entity references for namespaces in
    /// RDF/XML output.
    pub xml_entities: bool,
}

static STRICT: AtomicBool = AtomicBool::new(false);
static XML_ENTITIES: AtomicBool = AtomicBool::new(false);

/// Set this run's options from the plan. The build path's single writer.
pub fn set_run_options(o: RunOptions) {
    STRICT.store(o.strict, Ordering::Relaxed);
    XML_ENTITIES.store(o.xml_entities, Ordering::Relaxed);
}

/// Latch a flag the user gave on the command line. LATCH, not assign: `activate`
/// runs once per subcommand, so assigning would let the second command of a chain
/// (`om merge --strict -i x.owl reason -o y.owl`) reset the flag mid-run.
pub fn latch_run_options(o: RunOptions) {
    if o.strict {
        STRICT.store(true, Ordering::Relaxed);
    }
    if o.xml_entities {
        XML_ENTITIES.store(true, Ordering::Relaxed);
    }
}

/// This run's options, as the loaders and writers read them.
pub fn run_options() -> RunOptions {
    RunOptions {
        strict: STRICT.load(Ordering::Relaxed),
        xml_entities: XML_ENTITIES.load(Ordering::Relaxed),
    }
}

pub mod entities;
pub mod manchester;
pub mod manchester_parse;
pub mod manchester_write;
pub mod genid;
pub mod natural_order;
pub mod obo;
pub mod obograph;
pub mod ofncache;
pub mod frame_twins;
pub mod owlfunc;
pub mod owlapi_ttl;
pub mod owlrdf;
pub mod owx;
pub(crate) mod rdfxml;
pub mod turtle;
pub mod jena_ttl;

/// A serialization format for OWL ontologies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// RDF/XML — the OBO default exchange syntax (`.owl`, `.rdf`).
    RdfXml,
    /// OWL/XML (`.owx`).
    OwlXml,
    /// OWL 2 Functional Syntax (`.ofn`, `.ofn`).
    Functional,
    /// OBO 1.4 flat-file format (`.obo`).
    Obo,
    /// OBO Graphs JSON (`.json`).
    OboGraph,
    /// OWL 2 Manchester Syntax (`.omn`).
    Manchester,
    /// Turtle (`.ttl`).
    Turtle,
    /// N-Triples (`.nt`) — MONDO mirrors `hgnc_gene.nt` / `ncbi_gene.nt` and feeds
    /// them straight into a merge.
    NTriples,
}

impl std::str::FromStr for Format {
    type Err = anyhow::Error;
    /// Parse a format name (`"ofn"`, `"owl"`, `"obo"`, `"ttl"`, …); see
    /// [`Format::from_name`].
    fn from_str(s: &str) -> Result<Format> {
        Format::from_name(s)
    }
}

impl std::fmt::Display for Format {
    /// The canonical format name (round-trips through [`Format::from_name`]).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Format::RdfXml => "owl",
            Format::OwlXml => "owx",
            Format::Functional => "ofn",
            Format::Obo => "obo",
            Format::OboGraph => "json",
            Format::Manchester => "omn",
            Format::Turtle => "ttl",
            Format::NTriples => "nt",
        })
    }
}

impl Format {
    /// Parse the canonical format name as accepted by `--format`.
    pub fn from_name(name: &str) -> Result<Format> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "owl" | "rdf" | "rdfxml" | "rdf/xml" => Format::RdfXml,
            "owx" | "owlxml" | "owl/xml" => Format::OwlXml,
            "ofn" | "fss" | "functional" => Format::Functional,
            "obo" => Format::Obo,
            "json" | "obograph" | "obojson" => Format::OboGraph,
            "omn" | "manchester" => Format::Manchester,
            "nt" | "ntriples" | "n-triples" => Format::NTriples,
            "ttl" | "turtle" => Format::Turtle,
            other => bail!("unknown format: {other}"),
        })
    }

    /// Infer the format from a file extension (after stripping any `.gz`).
    pub fn from_path(path: &Path) -> Result<Format> {
        let s = path.to_string_lossy();
        let s = s.strip_suffix(".gz").unwrap_or(&s);
        let ext = Path::new(s)
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        Format::from_name(&ext)
            .with_context(|| format!("cannot infer ontology format from path: {}", path.display()))
    }
}

/// Whether `path` names the null sink — a caller asking for the output to be
/// thrown away. `om reason -i x.owl -r elk -o /dev/null` is a *check*: the run is
/// wanted for its verdict, the document is not. There is nothing to infer a
/// format from and nothing to serialize, so a discard path skips the write
/// entirely rather than failing on the extension it does not have.
pub fn is_discard_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    // Only the platform's own null device counts: `nul` is a device name on
    // Windows and an ordinary file name everywhere else, and a repo that names a
    // target `nul` must still get its bytes.
    #[cfg(windows)]
    {
        s.eq_ignore_ascii_case("nul") || s.eq_ignore_ascii_case(r"\\.\nul")
    }
    #[cfg(not(windows))]
    {
        s == "/dev/null"
    }
}

/// Whether `path` is an *empty* file — zero bytes, or only whitespace. Such a
/// file denotes an empty ontology (no axioms): notably a build *stamp*, `touch`ed
/// as a marker whose real outputs are written elsewhere (e.g. UBERON's
/// `tmp/bridges`, whose step emits the bridge modules then touches the stamp). The
/// stamp still appears in a `merge`'s input list — merging it contributes nothing
/// — so merge callers skip it rather than failing to determine its format.
pub fn is_empty_ontology_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) if m.len() == 0 => true,
        Ok(m) if m.len() <= 64 => {
            std::fs::read(path).is_ok_and(|b| b.iter().all(|c| c.is_ascii_whitespace()))
        }
        _ => false,
    }
}

/// Load an ontology from `path`. The format is inferred from the extension, but
/// the `.owl`/`.rdf` extensions are ambiguous — such a file may hold RDF/XML *or*
/// Functional Syntax — so the file's leading bytes are sniffed to pick the right
/// parser.
/// Whether `path` names a gzipped file (`x.owl.gz`, `x.ofn.gz`, …). The format
/// is taken from the extension inside the `.gz`, as [`Format::from_path`] does.
pub fn is_gzipped_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("gz"))
}

/// Read an ontology file, transparently gunzipping a `.gz` one (or any file that
/// starts with the gzip magic, whatever it is called). GitHub refuses files over
/// 100 MB, so a large import module is committed gzipped: EFO's untrimmed OBA
/// module is 106 MB as RDF/XML and 2 MB gzipped, and the OWL API (Protégé,
/// ROBOT) resolves a catalog entry to a `.gz` module transparently.
fn read_ontology_bytes(path: &Path) -> Result<Vec<u8>> {
    let raw = std::fs::read(path).with_context(|| format!("opening {}", path.display()))?;
    if !(is_gzipped_path(path) || raw.starts_with(&[0x1f, 0x8b])) {
        return Ok(raw);
    }
    use std::io::Read;
    let mut out = Vec::with_capacity(raw.len() * 8);
    flate2::read::GzDecoder::new(&raw[..])
        .read_to_end(&mut out)
        .with_context(|| format!("gunzipping {}", path.display()))?;
    Ok(out)
}

pub fn load(path: &Path) -> Result<Model> {
    let bytes = read_ontology_bytes(path)?;
    let fmt = match Format::from_path(path) {
        Ok(f) => disambiguate(f, &bytes),
        Err(_) => sniff(&bytes)
            .with_context(|| format!("cannot determine ontology format of {}", path.display()))?,
    };
    IN_PATH.with(|c| *c.borrow_mut() = Some(path.to_path_buf()));
    let r = parse_bytes(bytes, fmt, &display_name(path))
        .with_context(|| format!("parsing {}", path.display()));
    IN_PATH.with(|c| *c.borrow_mut() = None);
    r
}

/// A short label for a path used in progress lines — the file name alone (the
/// directory is usually noise on a one-line bar), falling back to the full path.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Parse already-read bytes. For large inputs, runs a background heartbeat that
/// reports progress through BOTH the byte-read and the subsequent (byte-silent)
/// triple→axiom mapping phase of RDF parsing — the mapping is where a big file
/// spends most of its time, and a plain byte bar would sit at 100% during it.
fn parse_bytes(bytes: Vec<u8>, fmt: Format, name: &str) -> Result<Model> {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    let total = bytes.len() as u64;
    if total <= 20_000_000 || !crate::progress::enabled() {
        return load_from(std::io::Cursor::new(bytes), fmt);
    }
    let name = name.to_string();
    let count = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let hb = {
        let (count, done) = (count.clone(), done.clone());
        std::thread::spawn(move || {
            let mut bar = crate::progress::Progress::new(format!("parse {name}"), total);
            let start = crate::time::Instant::now();
            while !done.load(Ordering::Relaxed) {
                let c = count.load(Ordering::Relaxed);
                if c < total {
                    bar.set(c); // byte-read bar
                } else {
                    // Bytes consumed; horned-owl is now mapping triples → axioms.
                    bar.line(&format!(
                        "parse {name}: {:.0} MB read, mapping triples → axioms…  {:.0}s",
                        total as f64 / 1.0e6,
                        start.elapsed().as_secs_f64(),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            bar.finish_line(&format!(
                "parse {name}: {:.0} MB, done in {:.0}s",
                total as f64 / 1.0e6,
                start.elapsed().as_secs_f64()
            ));
        })
    };
    let reader = crate::progress::CountReader::new(std::io::Cursor::new(bytes), count);
    let result = load_from(std::io::BufReader::new(reader), fmt);
    done.store(true, Ordering::Relaxed);
    let _ = hb.join();
    result
}

/// The format a file's bytes are read as: what the content says where that is
/// unambiguous, and what the extension says where it is not.
///
/// A build writes files whose name and content disagree, because a recipe names
/// its output for the target it is on the way to rather than for the syntax it
/// holds. ECTO's `tmp/ecto-main.obo` is `git show <the edit file> > $@` — an
/// `.obo` name over functional syntax — and reading it with the OBO parser lost
/// the whole root ontology, leaving the artefact with its import closure and none
/// of its own terms. The same rule covers `-o $@.owl && mv $@.owl $@`, where a
/// `.obo` target holds RDF/XML.
///
/// `sniff` answers only when the leading bytes settle it, so an extension is
/// overridden by evidence and never by a guess.
fn disambiguate(ext_fmt: Format, bytes: &[u8]) -> Format {
    sniff(bytes).unwrap_or(ext_fmt)
}

/// Load an ontology from `path`, optionally forcing the parser format
/// (`--input-format`) instead of inferring it from the extension/content.
pub fn load_with(path: &Path, format: Option<&str>) -> Result<Model> {
    match format {
        Some(name) => {
            let fmt = Format::from_name(name)?;
            let bytes = read_ontology_bytes(path)?;
            IN_PATH.with(|c| *c.borrow_mut() = Some(path.to_path_buf()));
            let r = parse_bytes(bytes, fmt, &display_name(path))
                .with_context(|| format!("parsing {}", path.display()));
            IN_PATH.with(|c| *c.borrow_mut() = None);
            r
        }
        None => load(path),
    }
}

/// Load an ontology directly from an IRI (`--input-iri`), optionally forcing the
/// parser format. The document is fetched over HTTP(S).
pub fn load_iri(iri: &str, format: Option<&str>) -> Result<Model> {
    let bytes = http_get(iri).with_context(|| format!("fetching {iri}"))?;
    let fmt = match format {
        Some(name) => Format::from_name(name)?,
        None => Format::from_name(
            Path::new(iri)
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .as_deref()
                .unwrap_or(""),
        )
        .ok()
        .map(|f| disambiguate(f, &bytes))
        .or_else(|| sniff(&bytes))
        .with_context(|| format!("cannot determine ontology format of {iri}"))?,
    };
    IN_IRI.with(|c| *c.borrow_mut() = Some(iri.to_string()));
    let r = load_from(std::io::Cursor::new(bytes), fmt).with_context(|| format!("parsing {iri}"));
    IN_IRI.with(|c| *c.borrow_mut() = None);
    r
}

/// Fetch a URL's bytes over HTTP(S), following redirects.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn http_get(url: &str) -> Result<Vec<u8>> {
    http_get_dated(url).map(|(b, _)| b)
}

/// [`http_get`], also returning the response's `Last-Modified` header verbatim.
///
/// A downloaded file's MTIME is load-bearing in a timestamp-driven build: the
/// server's `Last-Modified` is stamped onto the file, so MONDO's
/// `tmp/mondo-lastbase.owl` lands with a date months in the past and
/// `reports/mondo_base_last_release-report.tsv` — committed, and newer — is left
/// alone. Stamping "now" instead would make every downstream report look stale and
/// rebuild it.
///
/// Retried, because every mirror fetch depends on it and the PURLs really do
/// flake: a bare `503` for `envo.owl` on one request is served fine by the next.
/// Only a transport error or a 5xx is retried; a 404 is an answer, and so is a
/// URL no request can be made for.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn http_get_dated(url: &str) -> Result<(Vec<u8>, Option<String>)> {
    use std::io::Read as _;
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..5u32 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1)));
        }
        // A READ timeout, not just a connect one. ureq waits forever by default,
        // so a mirror fetch that stalls mid-body hangs the whole build with nothing
        // on stdout, one idle socket open and no CPU. The retry above turns a stall
        // into another attempt rather than a failure.
        let agent = ureq::builder()
            .timeout_connect(std::time::Duration::from_secs(60))
            .timeout_read(std::time::Duration::from_secs(300))
            .build();
        match agent.get(url).call() {
            Ok(resp) => {
                let last_modified = resp.header("Last-Modified").map(str::to_string);
                let mut buf = Vec::new();
                resp.into_reader().read_to_end(&mut buf)?;
                return Ok((buf, last_modified));
            }
            Err(ureq::Error::Status(code, _)) if !(500..600).contains(&code) => {
                return Err(anyhow::anyhow!("HTTP GET {url}: status code {code}"));
            }
            Err(ureq::Error::Transport(t))
                if matches!(t.kind(), ureq::ErrorKind::InvalidUrl | ureq::ErrorKind::UnknownScheme) =>
            {
                return Err(anyhow::Error::new(ureq::Error::Transport(t)).context(format!("HTTP GET {url}")));
            }
            Err(e) => last = Some(anyhow::Error::new(e).context(format!("HTTP GET {url}"))),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("HTTP GET {url}: no attempt made")))
}

/// [`http_get`], but a non-2xx RESPONSE BODY is returned instead of raised.
///
/// `requests.get(url).text` is the body whatever the status, and ODK's
/// `simple_pattern_tester.py` hands exactly that to its YAML parser without
/// looking at `status_code` — which is why its schema fetch, now a 404, yields
/// the mapping `{404: 'Not Found'}` rather than an error. A 5xx or a transport
/// failure is still retried, because those are the flakes the retry exists for.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn http_get_body_any_status(url: &str) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..5u32 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1)));
        }
        let agent = ureq::builder()
            .timeout_connect(std::time::Duration::from_secs(60))
            .timeout_read(std::time::Duration::from_secs(300))
            .build();
        let resp = match agent.get(url).call() {
            Ok(resp) => Some(resp),
            Err(ureq::Error::Status(code, resp)) if !(500..600).contains(&code) => Some(resp),
            Err(e) => {
                last = Some(anyhow::Error::new(e).context(format!("HTTP GET {url}")));
                None
            }
        };
        if let Some(resp) = resp {
            let mut buf = Vec::new();
            resp.into_reader().read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("HTTP GET {url}: no attempt made")))
}

/// wasm has no network — see [`http_get`].
#[cfg(target_arch = "wasm32")]
pub(crate) fn http_get_body_any_status(url: &str) -> Result<Vec<u8>> {
    anyhow::bail!("fetching <{url}> over the network is not supported in the wasm build")
}

/// wasm has no network — see [`http_get`].
#[cfg(target_arch = "wasm32")]
pub(crate) fn http_get_dated(url: &str) -> Result<(Vec<u8>, Option<String>)> {
    anyhow::bail!("fetching <{url}> over the network is not supported in the wasm build")
}

/// wasm has no `std::net` / `ureq`; network loads (`--input-iri`, remote
/// `owl:imports`) are unavailable. Callers get a clear error rather than the
/// build failing to link a networking stack that can't exist on wasm.
#[cfg(target_arch = "wasm32")]
pub(crate) fn http_get(url: &str) -> Result<Vec<u8>> {
    anyhow::bail!("fetching <{url}> over the network is not supported in the wasm build")
}

/// POST a JSON body and return `(status_code, body_bytes)`. Unlike ureq's default,
/// a 4xx/5xx is returned (not raised) so callers can implement their own retry /
/// batch-splitting (used by `embeddings` for the OpenAI embeddings API). `bearer`,
/// when set, is sent as `Authorization: Bearer <token>`. Only transport failures
/// are `Err`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn http_post_json(url: &str, bearer: Option<&str>, body: &[u8]) -> Result<(u16, Vec<u8>)> {
    use std::io::Read as _;
    let mut req = ureq::post(url).set("Content-Type", "application/json");
    if let Some(token) = bearer {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    let resp = match req.send_bytes(body) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let mut buf = Vec::new();
            r.into_reader().read_to_end(&mut buf).ok();
            return Ok((code, buf));
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("HTTP POST {url}"))),
    };
    let code = resp.status();
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf)?;
    Ok((code, buf))
}

/// Sniff a serialization format from the leading non-whitespace content.
fn sniff(bytes: &[u8]) -> Option<Format> {
    let head: String = bytes
        .iter()
        .take(4096)
        .map(|&b| b as char)
        .collect::<String>();
    let trimmed = skip_comment_lines(&head);
    // A leading `<` is usually XML, but Turtle/N-Triples subjects are also
    // angle-bracketed IRIs — `<http://identifiers.org/hgnc/915> <…> "B2MR" .` is
    // exactly what `query --format ttl` writes when the constructed graph declares
    // no prefixes, and MONDO's `mirror/hgnc.owl` (a `.owl` name holding Turtle) is
    // then re-read by the next step.
    if trimmed.starts_with('<') && leading_iri_term(trimmed) {
        return Some(Format::Turtle);
    }
    if trimmed.starts_with("_:") {
        return Some(Format::Turtle);
    }
    if trimmed.starts_with("<?xml") || trimmed.starts_with("<rdf:") || trimmed.starts_with('<') {
        // Could be RDF/XML or OWL/XML; distinguish by the root element.
        if trimmed.contains("<Ontology") && !trimmed.contains("rdf:RDF") {
            return Some(Format::OwlXml);
        }
        return Some(Format::RdfXml);
    }
    if trimmed.starts_with("Prefix(") || trimmed.starts_with("Ontology(") {
        return Some(Format::Functional);
    }
    // Manchester syntax opens with its prefix declarations or its ontology
    // frame, whatever the file is called.
    if trimmed.starts_with("Prefix:") || trimmed.starts_with("Ontology:") {
        return Some(Format::Manchester);
    }
    if trimmed.starts_with("format-version:")
        || trimmed.starts_with("[Term]")
        || trimmed.starts_with("[Typedef]")
        || trimmed.starts_with("ontology:")
    {
        return Some(Format::Obo);
    }
    if trimmed.starts_with('{') {
        return Some(Format::OboGraph);
    }
    if trimmed.starts_with("@prefix") || trimmed.starts_with("@base") || trimmed.starts_with("PREFIX") {
        return Some(Format::Turtle);
    }
    None
}

/// `text` from its first line that is neither blank nor a `#` comment, the
/// comment of Turtle, N-Triples, functional and Manchester syntax.
fn skip_comment_lines(text: &str) -> &str {
    let mut rest = text.trim_start();
    while rest.starts_with('#') {
        rest = rest.split_once('\n').map_or("", |(_, next)| next).trim_start();
    }
    rest
}

/// Does `trimmed` open with an RDF term (`<IRI>`) rather than an XML tag? An
/// XML start tag has a name first and any IRI only inside an attribute value,
/// so the giveaway is a scheme separator with no whitespace or `=` before the
/// closing `>`.
fn leading_iri_term(trimmed: &str) -> bool {
    match trimmed[1..].find('>') {
        Some(end) => {
            let inner = &trimmed[1..1 + end];
            inner.contains("://") && !inner.contains(char::is_whitespace) && !inner.contains('=')
        }
        None => false,
    }
}

/// Whether a namespace can stand behind an OBO `idspace:`. The OWL, RDF, RDFS,
/// XSD and XML namespaces cannot; nor can the OBO PURL space, whose ids the
/// OBO id rules already shorten, or any namespace that encloses it.
pub(crate) fn idspace_namespace(ns: &str) -> bool {
    const OBO: &str = "http://purl.obolibrary.org/obo/";
    !(ns.starts_with(OBO)
        || OBO.starts_with(ns)
        || ns.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
        || ns.starts_with("http://www.w3.org/2000/01/rdf-schema#")
        || ns.starts_with("http://www.w3.org/2001/XMLSchema#")
        || ns.starts_with("http://www.w3.org/2002/07/owl#")
        || ns.starts_with("http://www.w3.org/XML/1998/namespace"))
}

/// The `idspace:` declarations an OBO rendering of `model` carries: the prefixes
/// the source document bound, less the namespaces no idspace may name. An
/// RDF/XML source binds them as `xmlns:` attributes, which `idspaces` holds
/// already; a functional-syntax source binds them with its `Prefix(…)` lines,
/// which reach here as `rdf_prefixes`.
pub(crate) fn declared_idspaces(model: &Model) -> Vec<(String, String)> {
    if !model.idspaces.is_empty() {
        return model.idspaces.clone();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for (prefix, ns) in &model.rdf_prefixes {
        if prefix.is_empty() || !idspace_namespace(ns) || out.iter().any(|(p, _)| p == prefix) {
            continue;
        }
        out.push((prefix.clone(), ns.clone()));
    }
    out
}

/// The format prefixes an RDF/XML document declares: the entities of its
/// internal subset and the namespace declarations of all its elements, in
/// document order, a later binding of a name replacing an earlier one where
/// the name was first bound. The default namespace binds no prefix, and the
/// RDF namespace is always `rdf`, whatever the document calls it.
fn rdfxml_prefixes(declared: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (prefix, ns) in declared {
        if prefix.is_empty() {
            continue;
        }
        let prefix = if ns == "http://www.w3.org/1999/02/22-rdf-syntax-ns#" { "rdf" } else { prefix.as_str() };
        match out.iter_mut().find(|(p, _)| p == prefix) {
            Some(bound) => bound.1 = ns.clone(),
            None => out.push((prefix.to_string(), ns.clone())),
        }
    }
    out
}

/// The `idspace:` set an RDF/XML document declares: its format prefixes
/// ([`rdfxml_prefixes`]) whose namespace an idspace may name, one prefix per
/// namespace, the first.
///
/// A declared prefix earns an idspace even when no id is ever shortened with it —
/// UBERON writes its `foaf`/`doap` IRIs out in full yet still declares both.
fn rdfxml_idspaces(prefixes: &[(String, String)]) -> Vec<(String, String)> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    prefixes.iter().filter(|(_, ns)| idspace_namespace(ns) && seen.insert(ns.as_str())).cloned().collect()
}

/// Owning class IRIs whose body references the SAME `rdf:nodeID` more than once —
/// a blank node genuinely shared between, say, an `intersectionOf` operand and an
/// `rdfs:subClassOf` edge. That is the only positive evidence a document carries
/// that two structurally-equal anonymous expressions are ONE node; without it each
/// occurrence takes a node of its own (EFO asserts the pair separately, and the
/// artefact then carries two `owl:Restriction` blocks). `scan_owl_body_genids`
/// dedups the ids and so cannot answer this.
pub(crate) fn scan_owl_shared_owners(
    bytes: &[u8],
) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let text = String::from_utf8_lossy(bytes);
    // `rdf:nodeID` -> "property\u{1}filler" for every top-level restriction block,
    // so a repeated id can be matched back to the class expression it stands for.
    let mut defs: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let ropen = "<owl:Restriction rdf:nodeID=\"";
    let mut i = 0usize;
    while let Some(r) = text[i..].find(ropen) {
        let s0 = i + r + ropen.len();
        let Some(qe) = text[s0..].find('"') else { break };
        let id = &text[s0..s0 + qe];
        let blk_end = text[s0..].find("</owl:Restriction>").map(|e| s0 + e).unwrap_or(text.len());
        let blk = &text[s0..blk_end];
        let grab = |tag: &str| -> Option<&str> {
            let pat = format!("<owl:{tag} rdf:resource=\"");
            let a = blk.find(&pat)? + pat.len();
            let b = blk[a..].find('"')? + a;
            Some(&blk[a..b])
        };
        if let (Some(p), Some(f)) = (grab("onProperty"), grab("someValuesFrom")) {
            defs.insert(id, format!("{p}\u{1}{f}"));
        }
        i = blk_end.max(s0 + qe + 1);
    }

    // How many times each id is REFERENCED (total `rdf:nodeID` occurrences minus
    // the one that DEFINES it). Two or more references means one blank node stood
    // in several places — the class body alone is too narrow a window, because an
    // annotated axiom's `owl:Axiom` reification sits outside it.
    let mut refs: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let nid_pat = "rdf:nodeID=\"";
    let mut q = 0usize;
    while let Some(r) = text[q..].find(nid_pat) {
        let a = q + r + nid_pat.len();
        let Some(be) = text[a..].find('"') else { break };
        let id = &text[a..a + be];
        // A definition opens a typed node: `<owl:Restriction rdf:nodeID="X">` or
        // `<owl:Class rdf:nodeID="X">`. Everything else is a reference.
        let line_start = text[..q + r].rfind('<').map(|i| i + 1).unwrap_or(0);
        let tag = &text[line_start..q + r];
        let is_def = tag.starts_with("owl:Restriction ") || tag.starts_with("owl:Class ");
        // An `owl:Axiom`'s back-reference to the axiom it reifies is not a second
        // PLACE the node stands in — every annotated axiom has one, and such a node
        // still takes an id of its own. Only references from real axiom positions
        // count towards sharing.
        let is_reif = tag.starts_with("owl:annotatedTarget")
            || tag.starts_with("owl:annotatedSource")
            || tag.starts_with("owl:annotatedProperty");
        if !is_def && !is_reif {
            *refs.entry(id).or_default() += 1;
        }
        q = a + be;
    }

    let mut out: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let open = "    <owl:Class rdf:about=\"";
    let close = "\n    </owl:Class>\n";
    let mut idx = 0usize;
    while let Some(rel) = text[idx..].find(open) {
        let s0 = idx + rel + open.len();
        let Some(qe) = text[s0..].find('"') else { break };
        let iri = text[s0..s0 + qe].to_string();
        if text[s0 + qe..].starts_with("\"/>") {
            idx = s0 + qe + 1;
            continue;
        }
        let body_end = text[s0 + qe..].find(close).map(|e| s0 + qe + e).unwrap_or(text.len());
        let body = &text[s0 + qe..body_end];
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let nid = "rdf:nodeID=\"";
        let mut p = 0usize;
        while let Some(r) = body[p..].find(nid) {
            let gs0 = p + r + nid.len();
            let Some(ge) = body[gs0..].find('"') else { break };
            let id = &body[gs0..gs0 + ge];
            let _ = &seen;
            if refs.get(id).copied().unwrap_or(0) >= 2 {
                // A repeated node whose definition is not the plain
                // `R some NamedClass` shape (a nested restriction, an anonymous
                // intersection) cannot be keyed. The evidence that this class
                // shares a node still stands, so record the wildcard rather than
                // lose it.
                match defs.get(id) {
                    Some(key) => {
                        out.entry(iri.clone()).or_default().insert(key.clone());
                    }
                    None => {
                        out.entry(iri.clone()).or_default().insert("*".to_string());
                    }
                }
            }
            p = gs0 + ge;
        }
        idx = (body_end + 1).min(text.len());
    }
    out
}

/// Blank nodes the source document shares between SEVERAL classes: one node, one
/// id, referenced from each. `scan_owl_shared_owners` records only that a class
/// HAS a shared node, and the numbering pass interns per entity, so it can reuse
/// within one class but never across two — UBERON's `uberon_bot.owl` makes 2,578
/// `rdfs:subClassOf` nodeID references to 2,164 distinct nodes. Returns
/// `owner\u{1}property\u{1}filler -> group`.
pub(crate) fn scan_cross_owner_shared(bytes: &[u8]) -> std::collections::HashMap<String, u64> {
    let text = String::from_utf8_lossy(bytes);
    let mut defs: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let ropen = "<owl:Restriction rdf:nodeID=\"";
    let mut i = 0usize;
    while let Some(r) = text[i..].find(ropen) {
        let s0 = i + r + ropen.len();
        let Some(qe) = text[s0..].find('"') else { break };
        let id = &text[s0..s0 + qe];
        let blk_end = text[s0..].find("</owl:Restriction>").map(|e| s0 + e).unwrap_or(text.len());
        let blk = &text[s0..blk_end];
        let grab = |tag: &str| -> Option<&str> {
            let pat = format!("<owl:{tag} rdf:resource=\"");
            let a = blk.find(&pat)? + pat.len();
            let b = blk[a..].find('"')? + a;
            Some(&blk[a..b])
        };
        if let (Some(p), Some(f)) = (grab("onProperty"), grab("someValuesFrom")) {
            defs.insert(id, format!("{p}\u{1}{f}"));
        }
        i = blk_end.max(s0 + qe + 1);
    }
    let mut owners: std::collections::HashMap<&str, Vec<String>> = Default::default();
    let open = "    <owl:Class rdf:about=\"";
    let close = "\n    </owl:Class>\n";
    let mut idx = 0usize;
    while let Some(rel) = text[idx..].find(open) {
        let s0 = idx + rel + open.len();
        let Some(qe) = text[s0..].find('"') else { break };
        let iri = text[s0..s0 + qe].to_string();
        if text[s0 + qe..].starts_with("\"/>") {
            idx = s0 + qe + 1;
            continue;
        }
        let body_end = text[s0 + qe..].find(close).map(|e| s0 + qe + e).unwrap_or(text.len());
        let body = &text[s0 + qe..body_end];
        let nid = "rdf:nodeID=\"";
        let mut p = 0usize;
        while let Some(r) = body[p..].find(nid) {
            let gs0 = p + r + nid.len();
            let Some(ge) = body[gs0..].find('"') else { break };
            let id = &body[gs0..gs0 + ge];
            let line_start = body[..p + r].rfind('<').map(|k| k + 1).unwrap_or(0);
            let tag = &body[line_start..p + r];
            let is_reif = tag.starts_with("owl:annotatedTarget")
                || tag.starts_with("owl:annotatedSource")
                || tag.starts_with("owl:annotatedProperty");
            if !is_reif {
                let e = owners.entry(id).or_default();
                if !e.contains(&iri) {
                    e.push(iri.clone());
                }
            }
            p = gs0 + ge;
        }
        idx = (body_end + 1).min(text.len());
    }
    let mut out = std::collections::HashMap::new();
    let mut group = 0u64;
    // Sorted, and first writer wins: two distinct shared nodes on one class can
    // carry the same property/filler key, so iterating the map directly would leave
    // the winner to Rust's randomised hash order and make the blank-node numbering
    // of the whole build vary between runs.
    let mut ids: Vec<_> = owners.into_iter().collect();
    ids.sort_by(|a, b| a.0.cmp(b.0));
    for (id, os) in ids {
        if os.len() < 2 {
            continue;
        }
        let Some(key) = defs.get(id) else { continue };
        // Offset: `span_shared` and `cross_shared` are looked up into the SAME
        // `span_intern` keyed by group id, so their id spaces must not overlap.
        group += 1;
        for o in os {
            out.entry(format!("{o}\u{1}{key}")).or_insert(group + 1_000_000);
        }
    }
    out
}

/// Per class IRI, the `genidN` blank-node ids referenced in the class body (the
/// annotated anonymous superclasses), in document order — so the RDF/XML writer can
/// reproduce the source's parse-time blank-node numbering, which is not
/// reconstructible from horned's model.
fn scan_owl_body_genids(bytes: &[u8]) -> std::collections::HashMap<String, Vec<String>> {
    let text = String::from_utf8_lossy(bytes);
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let open = "    <owl:Class rdf:about=\"";
    let close = "\n    </owl:Class>\n";
    let mut idx = 0usize;
    while let Some(rel) = text[idx..].find(open) {
        let s = idx + rel + open.len();
        let Some(qe) = text[s..].find('"') else { break };
        let iri = text[s..s + qe].to_string();
        // A self-closing declaration (`… "/>`) has no body.
        let after_iri = &text[s + qe..];
        if after_iri.starts_with("\"/>") {
            idx = s + qe + 1;
            continue;
        }
        // Body runs to the first top-level (4-space) `</owl:Class>`. When the
        // document's last class is not closed by that exact byte pattern the
        // fallback puts `body_end` at the end of the text, so the `body_end + 1`
        // below is clamped — unclamped it indexes one past the end and panics on
        // every document ending that way.
        let body_end = text[s + qe..].find(close).map(|e| s + qe + e).unwrap_or(text.len());
        let body = &text[s + qe..body_end];
        // Distinct nodeIDs in first-appearance order — a blank node shared
        // between an intersection operand and a subClassOf appears more than once.
        let mut gs: Vec<String> = Vec::new();
        let nid = "rdf:nodeID=\"";
        let mut p = 0usize;
        while let Some(r) = body[p..].find(nid) {
            let gs0 = p + r + nid.len();
            if let Some(ge) = body[gs0..].find('"') {
                let g = body[gs0..gs0 + ge].to_string();
                if !gs.contains(&g) {
                    gs.push(g);
                }
                p = gs0 + ge;
            } else {
                break;
            }
        }
        if !gs.is_empty() {
            out.insert(iri, gs);
        }
        idx = (body_end + 1).min(text.len());
    }
    out
}

/// Per subject IRI, the `rdfs:label` values in the order the source RDF/XML
/// carries them — plain assertions inside the subject's own element first, then
/// any carried by an `<owl:Axiom>` block further down the document.
///
/// A subject with two labels names one of them in the `! …` comments that
/// reference it, and where the two land in the same slot of the assertion set the
/// choice falls to the order they were read in. horned's model is unordered, so
/// that order is scanned here.
fn scan_label_order(bytes: &[u8]) -> std::collections::HashMap<String, Vec<String>> {
    let text = String::from_utf8_lossy(bytes);
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut subject: Option<String> = None;
    // Depth matters: a node element nested inside another — the
    // `<rdf:Description rdf:about="…"/>` cells of an `owl:propertyChainAxiom` —
    // names a referent, not a new subject.
    for line in text.lines() {
        if line.starts_with("    <") && !line.starts_with("     ") {
            subject = attr_after(line, "rdf:about=\"");
        } else if line.starts_with("        <") && !line.starts_with("         ") {
            if let Some(iri) = line
                .strip_prefix("        <owl:annotatedSource ")
                .and_then(|rest| attr_after(rest, "rdf:resource=\""))
            {
                subject = Some(iri);
            }
            if let (Some(subj), Some(v)) = (subject.as_ref(), label_text(line.trim_start())) {
                out.entry(subj.clone()).or_default().push(v);
            }
        }
    }
    out
}

/// The value of the named attribute, unescaped.
fn attr_after(s: &str, attr: &str) -> Option<String> {
    let at = s.find(attr)? + attr.len();
    let end = s[at..].find('"')?;
    Some(unescape_attr(&s[at..at + end]))
}

/// The text of a one-line `<rdfs:label …>value</rdfs:label>` element.
fn label_text(t: &str) -> Option<String> {
    let body = t.strip_prefix("<rdfs:label")?;
    let open = body.find('>')?;
    let close = body.rfind("</rdfs:label>")?;
    (close >= open).then(|| unescape_attr(&body[open + 1..close]))
}

/// XML entity references, in attribute values and one-line element text.
fn unescape_attr(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

thread_local! {
    /// The blank-node counter. Anonymous individuals are numbered upwards from
    /// 2^31, so `_:genid2147483648` is the first one a parse mints. One counter
    /// per thread: a target's recipe lines run on one thread and number on from
    /// each other, and targets built beside each other on other threads do not
    /// change what this one mints.
    static ANON_COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(2_147_483_648) };
}

/// Reserve `n` consecutive blank-node ids and return the first.
fn mint_anon_ids(n: usize) -> u64 {
    let first = ANON_COUNTER.with(|c| {
        let first = c.get();
        c.set(first + n as u64);
        first
    });
    if std::env::var_os("OM_ANON_DEBUG").is_some() {
        eprintln!("[anon] mint {n} from {first} ({})", out_name());
    }
    first
}

/// The id the next blank node takes.
pub(crate) fn anon_counter() -> u64 {
    ANON_COUNTER.with(|c| c.get())
}

/// Carry the counter forward to where a parse left it, so the next document
/// numbers on from there rather than over the top of it.
pub(crate) fn set_anon_counter(n: u64) {
    if std::env::var_os("OM_ANON_DEBUG").is_some() {
        eprintln!("[anon] carry {} -> {n}", anon_counter());
    }
    ANON_COUNTER.with(|c| c.set(n));
}

/// Start the blank-node counter over.
///
/// The counter's span is one recipe line: everything a single line does — a
/// merge, its reasoning, its writes — numbers from 2^31, and the next line starts
/// again. A build that ran the whole release off one counter would give the second
/// artefact ids the first one's parses had already used up.
pub fn reset_anon_counter() {
    if std::env::var_os("OM_ANON_DEBUG").is_some() {
        eprintln!("[anon] reset from {}", anon_counter());
    }
    ANON_COUNTER.with(|c| c.set(2_147_483_648));
}

/// The byte spans of the `_:label` node ids a functional-syntax document states,
/// in document order, skipping the three places a `_:` is not one: inside an
/// `<IRI>`, inside a string literal, and after a `#` to end of line.
fn anon_label_spans(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'<' => {
                while i < b.len() && b[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'_' if i + 1 < b.len() && b[i + 1] == b':' => {
                let s = i;
                let mut e = s + 2;
                while e < b.len()
                    && (b[e].is_ascii_alphanumeric() || b[e] == b'_' || b[e] == b'-' || b[e] == b'.')
                {
                    e += 1;
                }
                // A trailing `.` is sentence punctuation, not part of an NCName.
                while e > s + 2 && b[e - 1] == b'.' {
                    e -= 1;
                }
                if e > s + 2 {
                    out.push((s, e));
                }
                i = e.max(s + 2);
            }
            _ => i += 1,
        }
    }
    out
}

/// Re-mint a functional-syntax document's anonymous individuals, returning the
/// rewritten document and its new labels in first-mention order.
///
/// A node id is local to the document that states it: two files both naming
/// `_:genid1` mean two different individuals, so each parse takes a fresh
/// contiguous block from [`ANON_COUNTER`] and the label a document carried is
/// never the label it is read back under. Merging an ontology with a copy of one
/// of its own imports therefore keeps BOTH sets of anonymous axioms: uPheno's
/// source merge carries 224,000 anonymous individuals over the 112,000 in the
/// document it opens from.
///
/// The order is first mention, because that is the order the ids are taken in,
/// and the Individuals section renders in id order: a document naming `_:zzz`,
/// `_:aaa`, `_:mmm` in that order comes out `zzz aaa mmm`, and reversing the three
/// assertions reverses the output — the labels themselves order nothing.
pub(crate) fn remint_anon_labels(text: &str) -> (std::borrow::Cow<'_, str>, Vec<String>) {
    let spans = anon_label_spans(text);
    if spans.is_empty() {
        return (std::borrow::Cow::Borrowed(text), Vec::new());
    }
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut count = 0usize;
    for &(s, e) in &spans {
        let label = &text[s + 2..e];
        if !index.contains_key(label) {
            index.insert(label, count);
            count += 1;
        }
    }
    let base = mint_anon_ids(count);
    let labels: Vec<String> = (0..count).map(|k| format!("genid{}", base + k as u64)).collect();
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for &(s, e) in &spans {
        out.push_str(&text[last..s]);
        out.push_str("_:");
        out.push_str(&labels[index[&text[s + 2..e]]]);
        last = e;
    }
    out.push_str(&text[last..]);
    (std::borrow::Cow::Owned(out), labels)
}

/// The anonymous individuals of `ont`, by the number their `genid` id carries,
/// or `None` when one carries another kind of id.
pub(crate) fn numbered_individuals(ont: &Onto) -> Option<Vec<(u64, String)>> {
    use horned_owl::model::{AnonymousIndividual, RcStr};
    use horned_owl::visitor::immutable::{Visit, Walk};

    struct Ids(std::collections::BTreeSet<String>);
    impl Visit<RcStr> for Ids {
        fn visit_anonymous_individual(&mut self, a: &AnonymousIndividual<RcStr>) {
            self.0.insert(a.0.to_string());
        }
    }
    let mut walk = Walk::new(Ids(std::collections::BTreeSet::new()));
    for ac in ont.iter() {
        walk.annotated_component(ac);
    }
    let mut ids: Vec<(u64, String)> = Vec::new();
    for label in walk.into_visit().0 {
        let n: u64 = label.strip_prefix("genid")?.parse().ok()?;
        ids.push((n, label));
    }
    ids.sort();
    Some(ids)
}

/// Number the anonymous individuals `ids` (from [`numbered_individuals`])
/// `genid<first>` onwards, one id each, in the order of the numbers they carry,
/// and return the id after the last.
pub(crate) fn renumber_individuals(ont: &mut Onto, ids: Vec<(u64, String)>, first: u64) -> u64 {
    use horned_owl::model::{AnonymousIndividual, MutableOntology, RcStr};
    use horned_owl::visitor::mutable::{VisitMut, WalkMut};

    let next = first + ids.len() as u64;
    let renamed: std::collections::HashMap<String, RcStr> = ids
        .into_iter()
        .enumerate()
        .filter(|(k, (n, _))| *n != first + *k as u64)
        .map(|(k, (_, label))| (label, RcStr::from(format!("genid{}", first + k as u64))))
        .collect();
    if renamed.is_empty() {
        return next;
    }
    struct Rename(std::collections::HashMap<String, RcStr>);
    impl VisitMut<RcStr> for Rename {
        fn visit_anonymous_individual(&mut self, a: &mut AnonymousIndividual<RcStr>) {
            if let Some(to) = self.0.get(&*a.0) {
                a.0 = to.clone();
            }
        }
    }
    let mut rename = WalkMut::new(Rename(renamed));
    let mut out: Onto = horned_owl::ontology::set::SetOntology::new();
    for mut ac in std::mem::take(ont) {
        rename.annotated_component(&mut ac);
        out.insert(ac);
    }
    *ont = out;
    next
}

/// An `owl:versionIRI` statement about the ontology is its version IRI wherever
/// the document makes it. The parse takes one stated before the ontology's
/// `rdf:type owl:Ontology` for an ontology annotation, and declares
/// `owl:versionIRI` an annotation property for it; both are put back here.
fn version_iri_statement(ont: &mut Onto) {
    use horned_owl::model::{
        AnnotatedComponent, AnnotationValue, Component, DeclareAnnotationProperty, MutableOntology,
        OntologyAnnotation, OntologyID, RcStr,
    };
    const VERSION_IRI: &str = "http://www.w3.org/2002/07/owl#versionIRI";
    let id = ont.iter().find_map(|ac| match &ac.component {
        Component::OntologyID(id) => Some(id.clone()),
        _ => None,
    });
    let Some(id) = id else { return };
    if id.iri.is_none() || id.viri.is_some() {
        return;
    }
    let stated: Vec<AnnotatedComponent<RcStr>> = ont
        .iter()
        .filter(|ac| {
            matches!(&ac.component, Component::OntologyAnnotation(OntologyAnnotation(a))
                if a.ap.0.as_ref() == VERSION_IRI && matches!(a.av, AnnotationValue::IRI(_)))
        })
        .cloned()
        .collect();
    let [statement] = stated.as_slice() else { return };
    let Component::OntologyAnnotation(OntologyAnnotation(a)) = &statement.component else { return };
    let AnnotationValue::IRI(viri) = &a.av else { return };
    let viri = viri.clone();
    ont.remove(statement);
    let declaration = ont
        .iter()
        .find(|ac| matches!(&ac.component, Component::DeclareAnnotationProperty(DeclareAnnotationProperty(p)) if p.0.as_ref() == VERSION_IRI))
        .cloned();
    if let Some(declaration) = declaration {
        ont.remove(&declaration);
    }
    ont.remove(&AnnotatedComponent { component: Component::OntologyID(id.clone()), ann: Default::default() });
    ont.insert(AnnotatedComponent {
        component: Component::OntologyID(OntologyID { iri: id.iri, viri: Some(viri) }),
        ann: Default::default(),
    });
}

/// Load an ontology of the given format from any buffered reader.
pub fn load_from<R: BufRead>(reader: R, fmt: Format) -> Result<Model> {
    let mut model = guard_parse(fmt, move || load_from_raw(reader, fmt))?;
    literals_as_made(&mut model.ont);
    canonicalize_rules(&mut model);
    Ok(model)
}

/// Read back a document written from a model, whose literals are made already
/// and stay as they are.
pub(crate) fn reload<R: BufRead>(reader: R, fmt: Format) -> Result<Model> {
    let mut model = guard_parse(fmt, move || load_from_raw(reader, fmt))?;
    canonicalize_rules(&mut model);
    Ok(model)
}

/// Each literal a document states, as it is made
/// ([`crate::model::literal_as_made`]). An axiom that differs from another only
/// in a literal made the same becomes that axiom.
fn literals_as_made(ont: &mut Onto) {
    use crate::model::remade_literal;
    use horned_owl::model::{Literal, MutableOntology, RcStr};
    use horned_owl::visitor::immutable::{Visit, Walk};
    use horned_owl::visitor::mutable::{VisitMut, WalkMut};

    struct Unmade(bool);
    impl Visit<RcStr> for Unmade {
        fn visit_literal(&mut self, l: &Literal<RcStr>) {
            self.0 = self.0 || remade_literal(l).is_some();
        }
    }
    let unmade: Vec<_> = ont
        .iter()
        .filter(|ac| {
            let mut walk = Walk::new(Unmade(false));
            walk.annotated_component(ac);
            walk.into_visit().0
        })
        .cloned()
        .collect();
    struct Make;
    impl VisitMut<RcStr> for Make {
        fn visit_literal(&mut self, l: &mut Literal<RcStr>) {
            if let Some(made) = remade_literal(l) {
                *l = made;
            }
        }
    }
    let mut make = WalkMut::new(Make);
    for mut ac in unmade {
        ont.remove(&ac);
        make.annotated_component(&mut ac);
        ont.insert(ac);
    }
}

/// Run a parser closure, turning a panic into a clean error. The vendored
/// parsers can `unwrap`/`panic!` on malformed or non-ontology input — e.g.
/// horned-owl's RDF/XML reader unwraps an `oxrdfio` error on garbage or binary
/// data — which would otherwise abort owlmake with a backtrace instead of a
/// usable message. Parsing runs single-threaded at load, so the default panic
/// hook is silenced for the guarded call and restored immediately afterwards.
fn guard_parse<F: FnOnce() -> Result<Model>>(fmt: Format, parse: F) -> Result<Model> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = catch_unwind(AssertUnwindSafe(parse));
    std::panic::set_hook(prev);
    result.unwrap_or_else(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        anyhow::bail!(
            "could not parse input as {fmt:?}: it does not appear to be a valid ontology in \
             that format (use --input-format to override the auto-detected format) [panic: {detail}]"
        )
    })
}

/// Collapse SWRL rules that differ only in the order of their atoms.
///
/// A `DLSafeRule`'s body and head are *sets* for the purpose of axiom identity, so
/// the same rule serialized with its atoms in a different order (as ECTO's
/// RO/PATO/UBERON imports each do for the shared RO property-chain rules) is ONE
/// axiom and a merge must collapse the variants. owlmake's model keys a rule on the
/// atom `Vec`s in order, so without this they survive as distinct rules — 53 of them
/// in ECTO where the ontology has 28.
///
/// Deduplicate, but do NOT reorder the survivor's atoms: the winning instance keeps
/// the order it was parsed in, and that is the order it is rendered in. Sorting the
/// atoms here instead makes every rendered rule come out atom-sorted, when OBA's
/// first rule is `ObjectPropertyAtom(RO_0002180 …) ClassAtom(BFO_0000015 …)
/// ClassAtom(…)` in `imports/merged_import.owl` and in the released `oba-full.owl`
/// — property atom first, class atoms after.
fn canonicalize_rules(model: &mut Model) {
    use horned_owl::model::{Component, MutableOntology};
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    let mut dups = Vec::new();
    for ac in model.ont.iter() {
        if let Component::Rule(r) = &ac.component {
            let mut body = r.body.clone();
            let mut head = r.head.clone();
            body.sort();
            head.sort();
            let key = format!("{:?}\u{1}{:?}\u{1}{:?}", body, head, ac.ann);
            if !seen.insert(key) {
                dups.push(ac.clone());
            }
        }
    }
    for ac in dups {
        model.ont.remove(&ac);
    }
}

fn load_from_raw<R: BufRead>(mut reader: R, fmt: Format) -> Result<Model> {
    // Read RDF in lax mode: an undeclared property used in a restriction
    // (`someValuesFrom`/`allValuesFrom`/`hasValue`) is taken as an object property,
    // and leftover simple triples default to annotations. Strict mode silently drops
    // such restrictions (and the axioms that contain them), so a file with
    // import-sourced, locally-undeclared object properties (e.g. genus-differentia
    // equivalences over RO_* relations) would lose those class expressions on
    // re-read. `--strict` turns the RDF reader's lax repair off, so
    // structurally-broken triples error instead of being defaulted/dropped.
    let lax = !run_options().strict;
    let mut cfg = ParserConfiguration::default();
    cfg.lax = lax;
    match fmt {
        Format::RdfXml => {
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf)?;
            // Read unconditionally, for the same reason the SHARING scan below is:
            // these describe the SOURCE, and every RDF/XML file owlmake writes goes
            // through the writer that consumes them. Anything that made the scans
            // conditional would leave that writer with no record of the source on an
            // ordinary build.
            let (owl_genid_refs, owl_label_order) = (scan_owl_body_genids(&buf), scan_label_order(&buf));
            // The SHARING scan is read on the same terms: blank-node sharing is a
            // property of the source that every later step needs. `om make` turns the
            // owlrdf writer on per artefact through a thread-local, so a scan that
            // only ran when that writer was already on would reach it with no record
            // of which expressions were one node.
            let owl_shared_owners = scan_owl_shared_owners(&buf);
            // The reader numbers this document's blank nodes from the run's own
            // counter, so an anonymous individual carries the name it is
            // written with — `_:genid2147483648` onwards — and two documents
            // merged in one step keep their nodes apart. The counter comes back
            // out where the reading left it.
            let trace = std::env::var_os("OM_RDFXML_TRACE").is_some();
            let read = rdfxml::read(&buf, anon_counter(), trace, document_iri())?;
            let rdf_prefixes = rdfxml_prefixes(&read.prefixes);
            let idspaces = rdfxml_idspaces(&rdf_prefixes);
            let b = horned_owl::model::Build::new_rc();
            let mut rdf_cfg = ParserConfiguration::new(&b);
            rdf_cfg.lax = lax;
            let (rdfo, _): (horned_owl::io::rdf::reader::ConcreteRcRDFOntology, _) =
                horned_owl::io::rdf::reader::read_statements(read.statements, rdf_cfg, &read.order)
                    .map_err(|e| anyhow::anyhow!("RDF/XML parse error: {e}"))?;
            // Move components out of the parser's Rc set rather than deep-cloning
            // every one (the naive From<ConcreteRDFOntology>).
            let mut ont: Onto = rdfo.into_set_ontology_fast();
            set_anon_counter(read.next);
            version_iri_statement(&mut ont);
            let mut model = Model::from_parts(ont, crate::model::default_prefixes());
            model.idspaces = idspaces;
            model.rdf_prefixes = rdf_prefixes;
            model.owl_genid_refs = owl_genid_refs;
            model.owl_label_order = owl_label_order;
            model.owl_shared_owners = owl_shared_owners;
            // An RDF/XML document states blank-node identity, whether or not it
            // happens to share any node. Recording the CAPABILITY separately from
            // the observed sharing is what stops a module with no shared node
            // being treated like an OBO source and falling back to structural
            // equality.
            model.rdf_blank_node_identity = true;
            model.cross_shared = scan_cross_owner_shared(&buf);
            Ok(model)
        }
        Format::OwlXml => {
            // Every IRI is made absolute before the document is parsed, so the
            // parser has no prefixes to expand an `IRI` value with: only an
            // `abbreviatedIRI` names its IRI by prefix.
            let mut text = String::new();
            reader.read_to_string(&mut text)?;
            let (text, declared) = owx::normalise_iris(&text)?;
            let (ont, _): (Onto, PrefixMapping) =
                horned_owl::io::owx::reader::read(&mut text.as_bytes(), cfg)
                    .map_err(|e| anyhow::anyhow!("OWL/XML parse error: {e}"))?;
            let mut prefixes = PrefixMapping::default();
            for (name, ns) in &declared {
                let _ = prefixes.add_prefix(name, ns);
            }
            let mut model = Model::from_parts(ont, prefixes);
            // The document's `Prefix` elements are its format prefixes, which a
            // write in another format carries over.
            model.rdf_prefixes = declared;
            Ok(model)
        }
        Format::Functional => {
            // The standard prefixes are predefined in functional syntax, but
            // horned-owl does not seed them, so a document that uses
            // `xsd:`/`rdf:`/`rdfs:`/`owl:` without declaring them fails to parse.
            // Inject any that are missing.
            let mut text = String::new();
            reader.read_to_string(&mut text)?;
            // Recover the RDF/XML document prefixes carried by the `#rdfxmlns`
            // comment (see the Functional writer), then strip that line so horned's
            // parser never sees it.
            let parse_prefix_comment = |tag: &str| -> Vec<(String, String)> {
                text.lines()
                    .find(|l| l.starts_with(tag))
                    .map(|l| {
                        l[tag.len()..]
                            .split_whitespace()
                            .filter_map(|kv| {
                                kv.split_once('=').map(|(a, b)| (a.to_string(), b.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let shared_anon: std::collections::HashMap<String, std::collections::HashSet<u64>> =
                text.lines()
                    .filter_map(|l| l.strip_prefix("#anonshare "))
                    .filter_map(|rest| {
                        let mut it = rest.split_whitespace();
                        let owner = it.next()?.to_string();
                        let set: std::collections::HashSet<u64> =
                            it.filter_map(|h| u64::from_str_radix(h, 16).ok()).collect();
                        Some((owner, set))
                    })
                    .collect();
            let owl_shared_owners: std::collections::HashMap<
                String,
                std::collections::HashSet<String>,
            > = text
                .lines()
                .filter_map(|l| l.strip_prefix("#sharedowner "))
                .filter_map(|rest| {
                    let mut it = rest.split_whitespace();
                    let owner = it.next()?.to_string();
                    let set: std::collections::HashSet<String> =
                        it.map(|k| k.replace('\u{2}', "\u{1}")).collect();
                    Some((owner, set))
                })
                .collect();
            // `#prefixes-cleared`: this document's prefix map is the bare default
            // set because the ontology it stands for came out of a `query --update`
            // (see `Model::format_prefixes_cleared`). Without carrying the flag, a
            // round trip through the OFN cache would look like an ordinary 6-prefix
            // document and the RDF/XML writer would rebuild an xmlns block the
            // artefact must not carry.
            let mut prefixes_cleared = text.lines().any(|l| l.trim_end() == "#prefixes-cleared");
            let mut rdf_prefixes = parse_prefix_comment("#rdfxmlns ");
            let mut explicit_prefixes = parse_prefix_comment("#explicit-prefixes ");
            let mut idspaces = parse_prefix_comment("#idspaces ");
            let mut shared_anon = shared_anon;
            let mut owl_shared_owners = owl_shared_owners;
            // The companion, where this document has one that describes exactly
            // these bytes. It supersedes the inline form, which is still read so
            // that a `.ofn` written by an older binary — or by a build already in
            // flight — keeps working.
            if let Some(mk) = in_path().and_then(|p| ofncache::read(&p, text.as_bytes())) {
                prefixes_cleared = mk.prefixes_cleared;
                rdf_prefixes = mk.rdf_prefixes;
                explicit_prefixes = mk.explicit_prefixes;
                idspaces = mk.idspaces;
                shared_anon = mk.anonshare.into_iter().collect();
                owl_shared_owners = mk.sharedowner.into_iter().collect();
            }
            let text: String =
                if rdf_prefixes.is_empty()
                    && explicit_prefixes.is_empty()
                    && idspaces.is_empty()
                    && shared_anon.is_empty()
                    && !prefixes_cleared
                {
                    text
                } else {
                    text.lines()
                        .filter(|l| {
                            !l.starts_with("#rdfxmlns ")
                                && !l.starts_with("#explicit-prefixes ")
                                && !l.starts_with("#idspaces ")
                                && !l.starts_with("#anonshare ")
                                && !l.starts_with("#sharedowner ")
                                && l.trim_end() != "#prefixes-cleared"
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
            let original_text = text.clone();
            let text = format!("{}{}", standard_prefix_prelude(&text), text);
            let text = resolve_relative_iris(&text);
            // Node ids are re-minted before the parse, not after: every position a
            // label can occupy — an assertion's subject, an annotation's value, a
            // `SameIndividual` operand — is rewritten at once, and the parsed model
            // needs no second walk to agree with the labels this document is read
            // back under.
            let (text, anon_labels) = remint_anon_labels(&text);
            let (ont, prefixes): (Onto, PrefixMapping) =
                horned_owl::io::ofn::reader::read(&mut text.as_bytes(), cfg)
                    .map_err(|e| anyhow::anyhow!("Functional Syntax parse error: {e}"))?;
            let mut model = Model::from_parts(ont, prefixes);
            model.anon_doc_order = anon_labels;
            model.idspaces = idspaces;
            // A document owlmake wrote carries its source's xmlns in `#rdfxmlns`.
            // A GENUINE Functional document (HPO edits `hp-edit.owl` in OFN) has no
            // such comment, and its own `Prefix(…)` lines ARE its format prefix map:
            // the input format's prefixes carry onto the output format and become
            // the xmlns block, with the writer generating the rest for namespaces
            // actually used. Leaving them behind falls through to the full built-in
            // CURIE map, declaring 43 prefixes where the document needs 22 —
            // including ones HPO never mentions.
            model.rdf_prefixes = if rdf_prefixes.is_empty() {
                document_ofn_prefixes(&original_text)
            } else {
                rdf_prefixes
            };
            model.explicit_prefixes = explicit_prefixes;
            model.shared_anon = shared_anon;
            model.owl_shared_owners = owl_shared_owners;
            model.format_prefixes_cleared = prefixes_cleared;
            Ok(model)
        }
        Format::Obo => obo::load(reader),
        Format::OboGraph => obograph::load(reader),
        Format::Manchester => manchester::load(reader, cfg),
        Format::Turtle => turtle::load(reader),
        Format::NTriples => turtle::load_as(reader, oxigraph::io::RdfFormat::NTriples),
    }
}

/// Save `model` to `path`, inferring the format from its extension.
pub fn save(model: &mut Model, path: &Path) -> Result<()> {
    if is_discard_path(path) {
        return Ok(());
    }
    let fmt = Format::from_path(path)?;
    save_as(model, path, fmt)
}

/// Put every SET-valued operand list into canonical order, so two axioms that OWL
/// considers identical are identical here too.
///
/// OWL 2 makes the operands of `ObjectIntersectionOf`, `ObjectUnionOf`,
/// `EquivalentClasses`, `DisjointClasses`, … a SET, but horned-owl stores them as a
/// `Vec`. So `C ≡ (A ⊓ ∃R.X ⊓ ∃S.Y)` and `C ≡ (A ⊓ ∃S.Y ⊓ ∃R.X)` are ONE axiom in
/// OWL and TWO in the model — MONDO's `MONDO_0024642` (and three siblings) carries
/// exactly that pair, which would otherwise render the equivalence, its blank node
/// and its `owl:Axiom` reification twice each. Sorting the operands lets
/// `SetOntology`'s own deduplication collapse them.
///
/// The same holds of the members of the other set-valued axioms —
/// equivalent and disjoint object and data properties, same and different
/// individuals, the two properties an inverse-properties axiom relates — and
/// of a key's properties: an RDF document stating `p owl:equivalentProperty q`
/// and `q owl:equivalentProperty p`, or `p owl:inverseOf q` and
/// `q owl:inverseOf p`, reads as two axioms, which would otherwise be written
/// twice in every syntax.
///
/// The canonical order is the order OWL's object model keeps a set in. The
/// functional writer emits operands in it; the other writers sort operands as
/// they emit them. (`SubObjectPropertyOf`'s chain is genuinely ordered and is
/// never sorted.)
pub fn normalize_set_operands(model: &mut Model) {
    use horned_owl::model::{AnnotatedComponent, MutableOntology, RcStr};
    let mut replace: Vec<(AnnotatedComponent<RcStr>, AnnotatedComponent<RcStr>)> = Vec::new();
    for ac in model.ont.iter() {
        if let Some(component) = canonical_component(&ac.component).filter(|c| *c != ac.component) {
            replace.push((ac.clone(), AnnotatedComponent { component, ann: ac.ann.clone() }));
        }
    }
    for (old, new) in replace {
        model.ont.remove(&old);
        model.ont.insert(new);
    }
}

/// `c` with its set-valued operand lists in canonical order (see
/// [`normalize_set_operands`]), or `None` when it has none.
pub(crate) fn canonical_component(
    c: &horned_owl::model::Component<horned_owl::model::RcStr>,
) -> Option<horned_owl::model::Component<horned_owl::model::RcStr>> {
    use crate::io::owlfunc::{cmp_individual, cmp_ope};
    use horned_owl::model::{self as m, Component};
    Some(match c {
        Component::EquivalentClasses(ax) => Component::EquivalentClasses(m::EquivalentClasses(sorted_ces(&ax.0))),
        Component::DisjointClasses(ax) => Component::DisjointClasses(m::DisjointClasses(sorted_ces(&ax.0))),
        Component::SubClassOf(ax) => {
            Component::SubClassOf(m::SubClassOf { sub: canonical_ce(&ax.sub), sup: canonical_ce(&ax.sup) })
        }
        Component::DisjointUnion(ax) => Component::DisjointUnion(m::DisjointUnion(ax.0.clone(), sorted_ces(&ax.1))),
        Component::ClassAssertion(ax) => {
            Component::ClassAssertion(m::ClassAssertion { ce: canonical_ce(&ax.ce), i: ax.i.clone() })
        }
        Component::ObjectPropertyDomain(ax) => {
            Component::ObjectPropertyDomain(m::ObjectPropertyDomain { ope: ax.ope.clone(), ce: canonical_ce(&ax.ce) })
        }
        Component::ObjectPropertyRange(ax) => {
            Component::ObjectPropertyRange(m::ObjectPropertyRange { ope: ax.ope.clone(), ce: canonical_ce(&ax.ce) })
        }
        Component::EquivalentObjectProperties(ax) => {
            Component::EquivalentObjectProperties(m::EquivalentObjectProperties(sorted_by(&ax.0, cmp_ope)))
        }
        Component::DisjointObjectProperties(ax) => {
            Component::DisjointObjectProperties(m::DisjointObjectProperties(sorted_by(&ax.0, cmp_ope)))
        }
        Component::EquivalentDataProperties(ax) => {
            Component::EquivalentDataProperties(m::EquivalentDataProperties(sorted_by(&ax.0, cmp_dp)))
        }
        Component::DisjointDataProperties(ax) => {
            Component::DisjointDataProperties(m::DisjointDataProperties(sorted_by(&ax.0, cmp_dp)))
        }
        Component::InverseObjectProperties(ax) => {
            let (first, second) = if cmp_ope(&ax.1, &ax.0).is_lt() { (&ax.1, &ax.0) } else { (&ax.0, &ax.1) };
            Component::InverseObjectProperties(m::InverseObjectProperties(first.clone(), second.clone()))
        }
        Component::SameIndividual(ax) => Component::SameIndividual(m::SameIndividual(sorted_by(&ax.0, cmp_individual))),
        Component::DifferentIndividuals(ax) => {
            Component::DifferentIndividuals(m::DifferentIndividuals(sorted_by(&ax.0, cmp_individual)))
        }
        Component::HasKey(ax) => Component::HasKey(m::HasKey { ce: canonical_ce(&ax.ce), vpe: sorted_by(&ax.vpe, cmp_pe) }),
        Component::DataPropertyDomain(ax) => {
            Component::DataPropertyDomain(m::DataPropertyDomain { dp: ax.dp.clone(), ce: canonical_ce(&ax.ce) })
        }
        Component::DataPropertyRange(ax) => {
            Component::DataPropertyRange(m::DataPropertyRange { dp: ax.dp.clone(), dr: canonical_dr(&ax.dr) })
        }
        Component::DatatypeDefinition(ax) => Component::DatatypeDefinition(m::DatatypeDefinition {
            kind: ax.kind.clone(),
            range: canonical_dr(&ax.range),
        }),
        _ => return None,
    })
}

fn sorted_by<T: Clone + PartialEq>(v: &[T], cmp: impl Fn(&T, &T) -> std::cmp::Ordering) -> Vec<T> {
    let mut v = v.to_vec();
    v.sort_by(|a, b| cmp(a, b));
    v.dedup();
    v
}

fn cmp_dp(
    a: &horned_owl::model::DataProperty<horned_owl::model::RcStr>,
    b: &horned_owl::model::DataProperty<horned_owl::model::RcStr>,
) -> std::cmp::Ordering {
    crate::owlapi_hash::iri_cmp(a.0.as_ref(), b.0.as_ref())
}

/// A key's object properties before its data properties, each in their own
/// order.
fn cmp_pe(
    a: &horned_owl::model::PropertyExpression<horned_owl::model::RcStr>,
    b: &horned_owl::model::PropertyExpression<horned_owl::model::RcStr>,
) -> std::cmp::Ordering {
    use horned_owl::model::PropertyExpression as PE;
    let rank = |p: &PE<horned_owl::model::RcStr>| match p {
        PE::ObjectPropertyExpression(_) => 0,
        PE::DataProperty(_) => 1,
        PE::AnnotationProperty(_) => 2,
    };
    rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
        (PE::ObjectPropertyExpression(x), PE::ObjectPropertyExpression(y)) => crate::io::owlfunc::cmp_ope(x, y),
        (PE::DataProperty(x), PE::DataProperty(y)) => cmp_dp(x, y),
        (PE::AnnotationProperty(x), PE::AnnotationProperty(y)) => {
            crate::owlapi_hash::iri_cmp(x.0.as_ref(), y.0.as_ref())
        }
        _ => std::cmp::Ordering::Equal,
    })
}

/// A class expression with its set-valued operands, at any depth, in
/// canonical order.
fn canonical_ce(
    ce: &horned_owl::model::ClassExpression<horned_owl::model::RcStr>,
) -> horned_owl::model::ClassExpression<horned_owl::model::RcStr> {
    use horned_owl::model::ClassExpression as CE;
    match ce {
        CE::ObjectIntersectionOf(ops) => CE::ObjectIntersectionOf(sorted_ces(ops)),
        CE::ObjectUnionOf(ops) => CE::ObjectUnionOf(sorted_ces(ops)),
        CE::ObjectComplementOf(b) => CE::ObjectComplementOf(Box::new(canonical_ce(b))),
        CE::ObjectSomeValuesFrom { ope, bce } => {
            CE::ObjectSomeValuesFrom { ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectAllValuesFrom { ope, bce } => {
            CE::ObjectAllValuesFrom { ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectMinCardinality { n, ope, bce } => {
            CE::ObjectMinCardinality { n: *n, ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectMaxCardinality { n, ope, bce } => {
            CE::ObjectMaxCardinality { n: *n, ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectExactCardinality { n, ope, bce } => {
            CE::ObjectExactCardinality { n: *n, ope: ope.clone(), bce: Box::new(canonical_ce(bce)) }
        }
        CE::ObjectOneOf(inds) => CE::ObjectOneOf(sorted_by(inds, crate::io::owlfunc::cmp_individual)),
        CE::DataSomeValuesFrom { dp, dr } => CE::DataSomeValuesFrom { dp: dp.clone(), dr: canonical_dr(dr) },
        CE::DataAllValuesFrom { dp, dr } => CE::DataAllValuesFrom { dp: dp.clone(), dr: canonical_dr(dr) },
        CE::DataMinCardinality { n, dp, dr } => {
            CE::DataMinCardinality { n: *n, dp: dp.clone(), dr: canonical_dr(dr) }
        }
        CE::DataMaxCardinality { n, dp, dr } => {
            CE::DataMaxCardinality { n: *n, dp: dp.clone(), dr: canonical_dr(dr) }
        }
        CE::DataExactCardinality { n, dp, dr } => {
            CE::DataExactCardinality { n: *n, dp: dp.clone(), dr: canonical_dr(dr) }
        }
        other => other.clone(),
    }
}

/// A data range with its set-valued operands — a union's or intersection's
/// ranges, a one-of's literals, a restriction's facets — at any depth, in
/// canonical order.
fn canonical_dr(
    dr: &horned_owl::model::DataRange<horned_owl::model::RcStr>,
) -> horned_owl::model::DataRange<horned_owl::model::RcStr> {
    use crate::io::owlfunc::{cmp_dr, cmp_literal};
    use horned_owl::model::DataRange as DR;
    let ranges = |ops: &[DR<horned_owl::model::RcStr>]| {
        let ops: Vec<_> = ops.iter().map(canonical_dr).collect();
        sorted_by(&ops, cmp_dr)
    };
    match dr {
        DR::DataIntersectionOf(ops) => DR::DataIntersectionOf(ranges(ops)),
        DR::DataUnionOf(ops) => DR::DataUnionOf(ranges(ops)),
        DR::DataComplementOf(b) => DR::DataComplementOf(Box::new(canonical_dr(b))),
        DR::DataOneOf(lits) => DR::DataOneOf(sorted_by(lits, cmp_literal)),
        DR::DatatypeRestriction(dt, facets) => DR::DatatypeRestriction(
            dt.clone(),
            sorted_by(facets, |p, q| p.f.cmp(&q.f).then_with(|| cmp_literal(&p.l, &q.l))),
        ),
        DR::Datatype(_) => dr.clone(),
    }
}

fn sorted_ces(
    ops: &[horned_owl::model::ClassExpression<horned_owl::model::RcStr>],
) -> Vec<horned_owl::model::ClassExpression<horned_owl::model::RcStr>> {
    let mut v: Vec<_> = ops.iter().map(canonical_ce).collect();
    v.sort_by(crate::io::owlfunc::cmp_ce);
    v.dedup();
    v
}

/// Save `model` to `path` in the explicitly given format. Shows a byte heartbeat
/// for the (potentially multi-GB) serialization, which is otherwise silent.
///
/// Takes `&mut Model` so the XML writers (which require an owned
/// `ComponentMappedOntology`) can *move* the components in and back out rather
/// than deep-cloning the whole ontology — a multi-GB copy on phenio-scale
/// inputs. The model is left unchanged once the write returns.
/// The ontologies `model` imports, directly or not, read again from where its
/// closure was read: a writer that renders each of them on its own needs them
/// whole. A model that imports and whose closure was never read has none to
/// give, and that is an error rather than a document written without them.
fn closure_documents(model: &Model) -> Result<Vec<Model>> {
    let imports: Vec<String> = model
        .ont
        .iter()
        .filter_map(|ac| match &ac.component {
            horned_owl::model::Component::Import(i) => Some(i.0.to_string()),
            _ => None,
        })
        .collect();
    if imports.is_empty() {
        return Ok(Vec::new());
    }
    let documents = model.imports_closure.as_ref().map(|c| c.documents.as_slice()).unwrap_or_default();
    if documents.is_empty() {
        anyhow::bail!(
            "the ontology imports <{}>, and its imports closure was not read, so the closure's graphs cannot be written",
            imports.join(">, <")
        );
    }
    documents
        .iter()
        .map(|source| match &source.path {
            Some(path) => load(path).with_context(|| format!("reading import <{}> from {}", source.iri, path.display())),
            None => load_iri(&source.iri, None),
        })
        .collect()
}

pub fn save_as(model: &mut Model, path: &Path, fmt: Format) -> Result<()> {
    if is_discard_path(path) {
        return Ok(());
    }
    // Create the output's parent directory if needed — steps write into `subsets/`,
    // `tmp/`, `reports/` etc. which a fresh checkout may not contain and which no
    // earlier step is required to have made.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    // Set-valued operand lists are canonicalised at the one boundary every
    // serialization passes through, so two spellings of one OWL axiom —
    // `DisjointClasses(A B)` in one source and `DisjointClasses(B A)` in
    // another — leave as the single axiom they are. Doing this on one build
    // route and not the others rendered UBERON's
    // `DisjointClasses(UBERON_0000001 GO_0110165)` twice, once per member's
    // frame, in every artefact of the recipe route.
    normalize_set_operands(model);
    // The `#…` marker lines the Functional writer uses to carry source state
    // (xmlns block, shared blank nodes, cleared prefixes) belong to owlmake's
    // own `*.ofn` cache files, never to a released artefact: MONDO's
    // `imports/merged_import.owl` is written by `convert -f ofn` under a `.owl`
    // name, and a released document carries no such lines.
    // …and only for owlmake's OWN cache, which is named for the target it stands
    // in for: the cache for `mondo.owl` is `mondo.owl.ofn`, so the stem still
    // carries an ontology extension. A `.ofn` a REPO names as a target has no
    // such second extension and is that repo's own artefact — uPheno's
    // `$(SRCMERGED)` is `tmp/merged-upheno-edit.ofn`, written by `convert -f ofn`
    // and read back by the term queries and the edit report, and OBA builds
    // `patterns/definitions.owl` through a `definitions.ofn` in `src/ontology`.
    // Neither may gain a marker line. Anything under `.owlmake-odk-tmp` is
    // owlmake's by construction, whatever it is called.
    let cache_name = path
        .file_stem()
        .map(Path::new)
        .and_then(|s| s.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "owl" | "obo" | "ofn" | "omn" | "owx" | "ttl" | "json"));
    OFN_CACHE.with(|c| {
        c.set(
            path.extension().and_then(|e| e.to_str()) == Some("ofn")
                && (cache_name
                    || path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str())
                        == Some(".owlmake-odk-tmp")),
        )
    });
    OUT_NAME.with(|c| *c.borrow_mut() = display_name(path));
    if is_gzipped_path(path) {
        // Serialise in memory, then gzip to disk. The serialisers stream through
        // a `Write`, so the gzip could wrap the file directly — but the OFN cache
        // markers below read the written bytes back, and a gzipped output carries
        // none, so the plain bytes are kept in hand instead.
        PENDING_MARKERS.with(|c| *c.borrow_mut() = None);
        let mut buf: Vec<u8> = Vec::new();
        let mut pw = crate::progress::ProgressWriter::new(&mut buf, format!("write {}", display_name(path)));
        write_to_with(model, &mut pw, fmt, RdfXmlWriter::Owlapi)
            .with_context(|| format!("writing {}", path.display()))?;
        pw.finish().with_context(|| format!("writing {}", path.display()))?;
        let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut enc = flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::default());
        enc.write_all(&buf).with_context(|| format!("gzipping {}", path.display()))?;
        enc.finish()
            .and_then(|mut w| w.flush())
            .with_context(|| format!("gzipping {}", path.display()))?;
        PENDING_MARKERS.with(|c| c.borrow_mut().take());
        return Ok(());
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut pw =
        crate::progress::ProgressWriter::new(BufWriter::new(file), format!("write {}", display_name(path)));
    // A file on disk is read by the next build step and shipped as a release, so
    // RDF/XML written to one always gets the full-fidelity bytes — there is no
    // build in which a file should differ from the artefact shape a release
    // carries.
    PENDING_MARKERS.with(|c| *c.borrow_mut() = None);
    write_to_with(model, &mut pw, fmt, RdfXmlWriter::Owlapi)
        .with_context(|| format!("writing {}", path.display()))?;
    pw.finish().with_context(|| format!("writing {}", path.display()))?;
    // The companion, once the bytes it describes are on disk. Its fingerprint is
    // over those bytes, so anything that rewrites this path afterwards — another
    // owlmake step, ROBOT, a shell redirect — withdraws it automatically.
    if let Some(mk) = PENDING_MARKERS.with(|c| c.borrow_mut().take()) {
        match std::fs::read(path) {
            Ok(bytes) => ofncache::write(path, &bytes, &mk),
            // Unreadable straight after writing it: no companion, so the next
            // read of this file misses the cache rather than trusting a guess.
            Err(_) => {}
        }
    }
    Ok(())
}

/// The model's RDF rendering, which is what a query reads: the RDF/XML a file
/// of the model holds, written as [`save_as`] writes it, read back as a graph
/// (every XML literal a typed literal — see [`owlrdf::read_as_graph`]).
pub(crate) fn rendering(model: &Model) -> Result<Vec<u8>> {
    let mut copy = model.clone();
    normalize_set_operands(&mut copy);
    let mut rdf = Vec::new();
    owlrdf::read_as_graph(|| write_to_with(&mut copy, &mut rdf, Format::RdfXml, RdfXmlWriter::Owlapi))?;
    Ok(rdf)
}

/// Build a `CmOnto` view for the XML/functional writers WITHOUT emptying the
/// model. It CLONES rather than moving: `SetOntology → ComponentMappedOntology`
/// is lossless (the writers see every axiom), but the reverse
/// `ComponentMappedOntology → SetOntology` a move-based restore needs
/// DROPS a `Declaration(NamedIndividual)` that is also named in a
/// `DifferentIndividuals` axiom — silently stripping those individuals from the
/// model (and any OFN cache written afterwards). Cloning leaves `model.ont`
/// untouched, so the model is exactly as it was; [`restore_cm`] is a no-op.
/// (Components are `Rc`-shared, so the clone is a pointer copy, not a deep copy.)
fn take_cm(model: &mut Model) -> CmOnto {
    model.ont.clone().into()
}

/// No-op: [`take_cm`] cloned, so the model was never emptied.
fn restore_cm(_model: &mut Model, _cm: CmOnto) {}

/// Serialize from a shared `&Model` by cloning into a scratch model first. Used
/// by the internal buffer-serialization paths (turtle/rename round-trips) that
/// only hold an immutable borrow. Like [`write_to`] it selects
/// [`RdfXmlWriter::Horned`], so both are for buffers owlmake parses straight back
/// itself; a file is written by [`save_as`], which selects the full-fidelity
/// RDF/XML writer instead.
pub fn write_to_ref<W: Write>(model: &Model, writer: W, fmt: Format) -> Result<()> {
    let mut tmp = Model::from_parts(model.ont.clone(), crate::model::clone_prefixes(&model.prefixes));
    tmp.obo_structure_check = model.obo_structure_check;
    write_to_with(&mut tmp, writer, fmt, RdfXmlWriter::Horned)
}

/// FNV-1a over a `ce_sig` string. Stable across builds (unlike the default
/// hasher), which matters because these values are written into the OFN cache and
/// read back by a later process.
pub fn anon_sig_hash(sig: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in sig.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

thread_local! {
    /// Shared-blank-node signatures from the most recent RDF/XML write, so the OFN
    /// cache written straight afterwards can carry them forward.
    static LAST_SHARED_ANON: std::cell::RefCell<
        std::collections::HashMap<String, std::collections::HashSet<u64>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

#[allow(dead_code)]
fn last_shared_anon_removed(
) -> std::collections::HashMap<String, std::collections::HashSet<u64>> {
    LAST_SHARED_ANON.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Serialize `model` of the given format to any writer. The XML formats borrow
/// the model's components by moving them through a `CmOnto` and back, so this
/// needs `&mut Model`; it is value-preserving across the call.
pub fn write_to<W: Write>(model: &mut Model, writer: W, fmt: Format) -> Result<()> {
    write_to_with(model, writer, fmt, RdfXmlWriter::Horned)
}

/// Which RDF/XML serializer a write uses. This is a property of the write's
/// DESTINATION, not ambient state: a file is read by the next build step and
/// shipped as a release, so it gets the full-fidelity bytes, and so does the
/// graph a query reads ([`rendering`]), which is what a file states; a buffer
/// owlmake parses straight back for the axioms alone (a rename's round trip, in
/// `write_to_ref`) only has to be valid RDF.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RdfXmlWriter {
    /// horned-owl's `pretty_rdf` — valid RDF/XML, for internal transport.
    Horned,
    /// owlrdf.rs — the exact RDF/XML layout a released artefact carries, for files.
    Owlapi,
}

fn write_to_with<W: Write>(
    model: &mut Model,
    mut writer: W,
    fmt: Format,
    rdfxml: RdfXmlWriter,
) -> Result<()> {
    match fmt {
        Format::RdfXml if rdfxml == RdfXmlWriter::Owlapi => {
            // An axiom the layout cannot state sends the whole document through
            // the general writer, so it is never written without one.
            let unstated = crate::io::owlrdf::try_save(model, &mut writer)?;
            if unstated.is_empty() {
                return Ok(());
            }
            crate::io::owlrdf::warn_unstated("RDF/XML", &unstated);
            return write_to_with(model, writer, fmt, RdfXmlWriter::Horned);
        }
        Format::RdfXml => {
            // Declare every document prefix on `rdf:RDF`, so a re-reader recovers
            // the same `idspace:` set (e.g. `terms:` for dc/terms/, not the injected
            // `dcterms:`). Prefer the scanned `idspaces` (the source's own `xmlns:`
            // bindings) over `model.prefixes` (which carries owlmake's default
            // `dcterms`/`obo`/… injections).
            let doc_prefixes: PrefixMapping = if !model.idspaces.is_empty() {
                let mut pm = PrefixMapping::default();
                for (p, ns) in &model.idspaces {
                    let _ = pm.add_prefix(p, ns);
                }
                pm
            } else {
                crate::model::clone_prefixes(&model.prefixes)
            };
            let doc_prefixes = xml_legal_prefixes(&doc_prefixes);
            if run_options().xml_entities {
                // Capture the writer output, then rewrite namespaces as `&entity;`
                // references with a matching DOCTYPE (`--xml-entities`).
                let prefixes: Vec<(String, String)> = model
                    .prefixes
                    .mappings()
                    .map(|(p, n)| (p.to_string(), n.to_string()))
                    .collect();
                let cm = take_cm(model);
                let mut buf: Vec<u8> = Vec::new();
                let r = horned_owl::io::rdf::writer::write(
                    &mut buf,
                    &cm,
                    Some(&doc_prefixes),
                )
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("RDF/XML write error: {e}"));
                restore_cm(model, cm);
                r?;
                let out = xml_entities_transform(&buf, &prefixes);
                writer
                    .write_all(&out)
                    .map_err(|e| anyhow::anyhow!("RDF/XML write error: {e}"))?;
            } else {
                let cm = take_cm(model);
                let r = horned_owl::io::rdf::writer::write(
                    &mut writer,
                    &cm,
                    Some(&doc_prefixes),
                )
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("RDF/XML write error: {e}"));
                restore_cm(model, cm);
                r?;
            }
        }
        Format::OwlXml => {
            let prefixes = written_prefixes(model);
            owx::save(model, &prefixes, &mut writer)?;
        }
        Format::Functional => {
            // Hand the writer the document's OWN prefixes, in document order: a
            // functional document declares every prefix it has and abbreviates by
            // longest valid match. The writer falls back to a full <IRI> for any IRI
            // no declared prefix can validly abbreviate, so passing all prefixes
            // always round-trips.
            let document = format_prefixes(model);
            // A saved prefix format always binds the DEFAULT prefix to the ontology
            // IRI plus `#`, so every functional file opens `Prefix(:=<…#>)`.
            // owlmake's own DOSDP modules are anonymous ontologies, so they carry no
            // `:`; without this the released `patterns/definitions.owl` — merged
            // from them and then annotated with an ontology IRI — loses its first
            // line.
            let default_ns = prefixes_default_ns(model, &document);
            let prefixes = ofn_prefix_block(&document, default_ns.as_deref());
            // A document merged in after the pipeline opened is one more the
            // banners draw from, so the labels are settled now, under the
            // identity the document is written with.
            let labels = if !model.banner_docs.is_empty() {
                let (iri, version) = crate::build::model_ontology_id(model);
                let own = crate::cmd::rdfs_labels(model);
                crate::cmd::fold_banner_docs(&model.banner_docs, iri.as_deref(), version.as_deref(), &own)
            } else {
                model.banner_labels.clone()
            };
            if let Ok(dbg) = std::env::var("OM_BANNER_DEBUG") {
                eprintln!(
                    "[banner-write] docs={} labels={} {dbg}={:?}",
                    model.banner_docs.len(),
                    labels.len(),
                    labels.get(&dbg)
                );
            }
            let labels_opt = if labels.is_empty() { None } else { Some(&labels) };
            let order = model.import_order.clone();
            let order_opt = if order.is_empty() { None } else { Some(order.as_slice()) };
            // Carry the RDF/XML document prefixes (the verbatim input xmlns, incl.
            // unused ones like `its`/`swrl`) through the OFN cache as a leading
            // `#rdfxmlns` comment. The owlrdf writer builds its xmlns block from
            // `model.rdf_prefixes`, which is only populated when reading RDF/XML
            // directly; without this, a pipeline hop through OFN (e.g. mondo-base.owl
            // fed by reasoned.owl.ofn) loses them. The reader strips this line before
            // parsing, so horned never sees it and nothing downstream is affected.
            // The cache state this document stands in for does NOT go into the
            // document. It is collected here and put in a companion file by
            // `save_as`, which has the path and the finished bytes: a repo's own
            // declared intermediates live in `tmp/` next to owlmake's caches, and
            // a `#…` line in one of those is a byte the reference never writes.
            if ofn_cache() {
                let mut mk = ofncache::Markers {
                    prefixes_cleared: model.format_prefixes_cleared,
                    rdf_prefixes: model.rdf_prefixes.clone(),
                    explicit_prefixes: model.explicit_prefixes.clone(),
                    idspaces: model.idspaces.clone(),
                    ..Default::default()
                };
                // Precedence: an explicit `shared_anon` wins, else whatever the
                // RDF/XML write this cache stands in for recorded on the model.
                let shared_anon = if model.shared_anon.is_empty() {
                    model.rdf_shared_anon.clone()
                } else {
                    model.shared_anon.clone()
                };
                mk.anonshare = shared_anon.into_iter().collect();
                mk.sharedowner = model.owl_shared_owners.clone().into_iter().collect();
                PENDING_MARKERS.with(|c| *c.borrow_mut() = Some(mk));
            }
            // The entities the document declares although the ontology does
            // not, the same ones an OWL/XML document declares.
            let declare: Vec<(horned_owl::model::NamedOWLEntityKind, String)> = entities::missing_declarations(model)
                .into_iter()
                .map(|(kind, iri)| (kind.named(), iri))
                .collect();
            let cm = take_cm(model);
            // Which class this document's untyped literals take, which decides where
            // they sort against `xsd:anyURI` — see `Model::plain_literals_typed`.
            // Set per WRITE, from the model, the same way `owlrdf` does it.
            horned_owl::io::ofn::writer::set_plain_literals_typed(model.plain_literals_typed);
            let r = horned_owl::io::ofn::writer::write_full(
                &mut writer,
                &cm,
                Some(&prefixes),
                labels_opt,
                order_opt,
                Some(&declare),
            )
            .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"));
            restore_cm(model, cm);
            r?;
        }
        Format::Obo => obo::save(model, &mut writer)?,
        Format::OboGraph => {
            let imports = closure_documents(model)?;
            obograph::save_closure(model, &imports, &mut writer)?
        }
        Format::Manchester => {
            let prefixes = written_prefixes(model);
            manchester_write::save(model, &prefixes, &mut writer)?
        }
        Format::Turtle => {
            let prefixes = written_prefixes(model);
            owlapi_ttl::save(model, &prefixes, &mut writer)?
        }
        Format::NTriples => turtle::save_ntriples(model, &mut writer)?,
    }
    Ok(())
}

/// The `Prefix(p:=<ns>)` declarations a Functional document makes for itself, in
/// declaration order, the default (`Prefix(:=<…>)`) among them: a file whose only
/// declaration is the default is still a file that declares prefixes. White space
/// may stand between the parts of a declaration, as anywhere in the syntax.
fn document_ofn_prefixes(text: &str) -> Vec<(String, String)> {
    static DECLARATION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let declaration = DECLARATION.get_or_init(|| {
        regex::Regex::new(r"Prefix\s*\(\s*([^\s:=()<>]*):\s*=\s*<([^>]*)>\s*\)").expect("prefix declaration")
    });
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // Prefix declarations form the document's head; the first axiom ends them.
        if line.starts_with("Ontology") {
            break;
        }
        if line.starts_with('#') {
            continue;
        }
        for decl in declaration.captures_iter(line) {
            let (name, iri) = (&decl[1], &decl[2]);
            if !iri.is_empty() {
                out.push((name.to_string(), iri.to_string()));
            }
        }
    }
    out
}

/// Resolve a functional document's RELATIVE IRIs against its default prefix.
///
/// `<pattern.yaml>` inside `<…>` is a relative reference, and its base is the
/// document's `Prefix(:=<…>)` binding — concatenated, not merged as a path, so a
/// non-hierarchical base such as `urn:unnamed:ontology#ont1` yields
/// `urn:unnamed:ontology#ont1pattern.yaml`.
///
/// A DOSDP prototype carries one per template that declares no `pattern_iri`, so
/// without this `pattern.owl` cannot be read at all — including by the very build
/// that just wrote it. With no default prefix bound there is no base, and the
/// reference is left alone for the parser to reject.
///
/// String literals are skipped: `"a <b> c"` is text, not an IRI.
fn resolve_relative_iris(text: &str) -> std::borrow::Cow<'_, str> {
    let declared = document_ofn_prefixes(text);
    let Some(base) = declared.iter().find(|(name, _)| name.is_empty()).map(|(_, iri)| iri.as_str()) else {
        return text.into();
    };
    let (bytes, mut out, mut last, mut in_string, mut escaped) =
        (text.as_bytes(), String::new(), 0usize, false, false);
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            match c {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'<' => {
                // Up to the closing `>`; another `<` first means this was not an
                // IRI at all, and the scan simply carries on from the next byte.
                if let Some(end) = bytes[i + 1..].iter().position(|&b| b == b'>' || b == b'<') {
                    if bytes[i + 1 + end] == b'>' {
                        if !text[i + 1..i + 1 + end].contains(':') {
                            out.push_str(&text[last..=i]);
                            out.push_str(base);
                            last = i + 1;
                        }
                        i += end + 1;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    if out.is_empty() {
        return text.into();
    }
    out.push_str(&text[last..]);
    out.into()
}

/// `Prefix(...)` declarations for the standard namespaces (`rdf`, `rdfs`, `xsd`,
/// `owl`) not already bound in `text`, so a functional-syntax document that uses
/// them without declaring them still parses.
fn standard_prefix_prelude(text: &str) -> String {
    const STD: [(&str, &str); 4] = [
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ("owl", "http://www.w3.org/2002/07/owl#"),
    ];
    let mut out = String::new();
    for (p, ns) in STD {
        // Already declared if its namespace IRI appears in any Prefix(...) line.
        if !text.contains(&format!("<{ns}>")) {
            out.push_str(&format!("Prefix({p}:=<{ns}>)\n"));
        }
    }
    out
}

/// Build the fixed prefix map owlmake gives a module it has just built from
/// scratch — a DOSDP pattern file, the merged `patterns/definitions.owl`, an
/// extracted import module: the default `:` prefix bound to the ontology IRI
/// ([`with_terminating_hash`]), then `owl`, `rdf`, `xml`, `xsd`, `rdfs`.
/// Nothing else — OBO CURIEs like `obo:` are deliberately absent so entity IRIs
/// render as full `<IRI>`, which is the shape released pattern and module files
/// carry. A functional document that
/// came off disk is not this case: it declares its own prefixes, which
/// `document_ofn_prefixes` reads back and the write path passes through unchanged.
/// The horned-owl writer emits these in this canonical order and uses them (only
/// them) for abbreviation, so the sole CURIEs that appear are `rdfs:label` and
/// `owl:versionInfo`.
///
/// The `:` prefix is dropped if any IRI in the ontology falls under the
/// ontology-self namespace but would abbreviate to an illegal CURIE local part
/// (e.g. one containing `/`), so output always round-trips.
pub fn robot_ofn_prefixes(model: &Model) -> PrefixMapping {
    use horned_owl::visitor::immutable::{entity::IRIExtract, Walk};

    let mut out = PrefixMapping::default();

    // Default `:`: keep the ontology's own default prefix if it declared one (a
    // load/convert round-trip preserves it); otherwise synthesize it from the
    // ontology IRI ([`with_terminating_hash`]), which is what a freshly built
    // pattern module gets.
    let mut self_ns: Option<String> = model
        .prefixes
        .mappings()
        .find(|(name, _)| name.is_empty())
        .map(|(_, ns)| ns.clone());
    if self_ns.is_none() {
        for ac in model.ont.iter() {
            if let horned_owl::model::Component::OntologyID(id) = &ac.component {
                if let Some(iri) = &id.iri {
                    self_ns = Some(with_terminating_hash(iri.as_ref()));
                }
                break;
            }
        }
    }
    if let Some(ns) = &self_ns {
        let mut walk = Walk::new(IRIExtract::default());
        walk.set_ontology(&model.ont);
        let safe = walk
            .into_visit()
            .into_set()
            .into_iter()
            .map(|i| i.as_ref().to_string())
            .filter(|iri| iri.starts_with(ns.as_str()))
            .all(|iri| is_valid_curie_local(&iri[ns.len()..]));
        if safe {
            let _ = out.add_prefix("", ns);
        }
    }

    let _ = out.add_prefix("owl", "http://www.w3.org/2002/07/owl#");
    let _ = out.add_prefix("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#");
    let _ = out.add_prefix("xml", "http://www.w3.org/XML/1998/namespace");
    let _ = out.add_prefix("xsd", "http://www.w3.org/2001/XMLSchema#");
    let _ = out.add_prefix("rdfs", "http://www.w3.org/2000/01/rdf-schema#");
    out
}

thread_local! {
    /// Whether the Functional writer is currently producing owlmake's own
    /// `*.ofn` cache (rather than a released `.owl`/stdout document), and may
    /// therefore prepend its `#…` state markers. Set by [`save_as`].
    static OFN_CACHE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ofn_cache() -> bool {
    OFN_CACHE.with(|c| c.get())
}

thread_local! {
    /// The file currently being written, for `OM_MODEL_DEBUG` (see
    /// [`crate::io::owlrdf::try_save`]). Set by [`save_as`].
    static OUT_NAME: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

thread_local! {
    /// The cache state the Functional writer collected for the document it is
    /// writing, waiting for [`save_as`] to put it in the companion file. The
    /// writer streams to a generic sink and has no path; the companion needs
    /// one, and needs the finished bytes to fingerprint.
    static PENDING_MARKERS: std::cell::RefCell<Option<ofncache::Markers>> =
        const { std::cell::RefCell::new(None) };
}

thread_local! {
    /// The file currently being READ, so the Functional reader can look for its
    /// companion. Set by [`parse_bytes`]; empty for input that has no path of
    /// its own (an IRI, a pipe).
    static IN_PATH: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// The path of the document being read, when it has one.
fn in_path() -> Option<std::path::PathBuf> {
    IN_PATH.with(|c| c.borrow().clone())
}

thread_local! {
    /// The IRI of the document currently being read when it was fetched by IRI
    /// rather than read from a file. Set by [`load_iri`].
    static IN_IRI: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The IRI of the document being read, which its relative IRIs resolve
/// against when it states no base of its own: the IRI it was fetched by, or
/// its file's IRI.
pub(crate) fn document_iri() -> Option<String> {
    IN_IRI.with(|c| c.borrow().clone()).or_else(|| in_path().and_then(|p| file_iri(&p)))
}

/// The IRI of the file at `path`: `file:` and the path made absolute against
/// the working directory as written, `.` and `..` included, with repeated and
/// trailing separators dropped and every character a URI path cannot hold
/// percent-escaped.
fn file_iri(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let joined = if text.starts_with('/') {
        text.to_string()
    } else {
        format!("{}/{text}", std::env::current_dir().ok()?.to_str()?)
    };
    let mut absolute = String::with_capacity(joined.len());
    for c in joined.chars() {
        if !(c == '/' && absolute.ends_with('/')) {
            absolute.push(c);
        }
    }
    if absolute.len() > 1 && absolute.ends_with('/') {
        absolute.pop();
    }
    let mut iri = String::from("file:");
    for c in absolute.chars() {
        let legal = c.is_ascii_alphanumeric()
            || "-_.!~*'();:@&=+$,/".contains(c)
            || (!c.is_ascii() && !c.is_control() && !c.is_whitespace());
        if legal {
            iri.push(c);
        } else {
            let mut utf8 = [0u8; 4];
            for b in c.encode_utf8(&mut utf8).bytes() {
                iri.push_str(&format!("%{b:02X}"));
            }
        }
    }
    Some(iri)
}

/// The name of the file being written, for diagnostics.
pub(crate) fn out_name() -> String {
    OUT_NAME.with(|c| c.borrow().clone())
}

/// The default (`:`) namespace a functional-syntax write binds: the ontology IRI
/// plus `#`, unless the document already binds one.
fn prefixes_default_ns(model: &Model, document: &PrefixMapping) -> Option<String> {
    if document.mappings().any(|(p, _)| p.is_empty()) {
        return None;
    }
    crate::cmd::merge::ontology_iri(model).map(|iri| with_terminating_hash(&iri))
}

/// The namespace an ontology IRI gives the default prefix: the IRI itself when
/// it ends in `/` or `#` or holds a `#` anywhere, and the IRI with `#` appended
/// otherwise.
pub(crate) fn with_terminating_hash(iri: &str) -> String {
    if iri.ends_with('/') || iri.contains('#') {
        iri.to_string()
    } else {
        format!("{iri}#")
    }
}

/// The `Prefix(…)` block of a functional-syntax document, in the order such a
/// document carries it.
///
/// The block is keyed on the prefix name INCLUDING its colon and ordered by length
/// first, then lexicographically. owl/rdfs/rdf/xsd/xml are seeded and the
/// document's own prefixes are copied over the top, so a document that rebinds
/// `xml:` wins and one that binds none of the five still declares all five.
///
/// So `Prefix(:=…)` leads (one character), the four-character names follow in
/// alphabetical order, and a long name is last. EFO's `components/gwas_import.owl`
/// is a Turtle graph a CONSTRUCT produced, and its block ends
/// `Prefix(oboInOwl:=…)` then `Prefix(gwas_trait:=…)`.
pub(crate) fn ofn_prefix_block(document: &PrefixMapping, default_ns: Option<&str>) -> PrefixMapping {
    let mut by_name: BTreeMap<(usize, String), String> = BTreeMap::new();
    let mut put = |name: &str, ns: &str| {
        by_name.insert((name.len() + 1, format!("{name}:")), ns.to_string());
    };
    for (p, ns) in [
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ("xml", "http://www.w3.org/XML/1998/namespace"),
    ] {
        put(p, ns);
    }
    for (p, ns) in document.mappings() {
        put(p, ns);
    }
    if let Some(ns) = default_ns {
        put("", ns);
    }
    let mut out = PrefixMapping::default();
    for ((_, name), ns) in &by_name {
        let name: &str = name.trim_end_matches(':');
        let _ = out.add_prefix(name, ns.as_str());
    }
    out
}

/// The prefix map a functional-syntax save of an RDF/XML-sourced ontology declares:
/// the input format's prefixes are copied onto the output format, so it is the five
/// predefined bindings plus every `xmlns:` the document declared.
///
/// That is NOT owlmake's `default_prefixes()`, which is the map the OBO and
/// RDF/XML writers need. Using it here declares `dc`/`terms` a document never
/// mentions and drops the ones it does use: `convert -i ro.owl -f ofn` has to
/// write `skos:narrowMatch`, `foaf:homepage` and `cito:citesAsAuthority` as
/// CURIEs, not in full.
fn rdfxml_format_prefixes(model: &Model) -> PrefixMapping {
    let mut out = PrefixMapping::default();
    for (p, ns) in [
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("xml", "http://www.w3.org/XML/1998/namespace"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ] {
        let _ = out.add_prefix(p, ns);
    }
    for (p, ns) in &model.rdf_prefixes {
        // NOT the default: `saveOntology` follows `copyPrefixesFrom` with
        // `setDefaultPrefix(<the OUTPUT format's>)`, so whatever `:` the input
        // bound is overwritten — with nothing at all when the ontology is
        // anonymous. `prefixes_default_ns` supplies the output's own.
        if p.is_empty() {
            continue;
        }
        let _ = out.add_prefix(p, ns);
    }
    // `--add-prefixes` is applied to the output format AFTER the input's own are
    // copied across, so every prefix it names lands in the output whatever the
    // input declared. MONDO's `tmp/mondo.owl.ofn` is `convert --add-prefixes
    // config/prefixes.jsonld -f ofn` over an RDF/XML input, and opens with all
    // 27 of them.
    for (p, ns) in &model.explicit_prefixes {
        let _ = out.add_prefix(p, ns);
    }
    out
}

/// The prefixes a prefix-format document — functional syntax, OWL/XML,
/// Manchester — carries over from its source: the source document's own
/// bindings over the five built-in ones, then every prefix the command line
/// adds (`Model::added_prefixes`), a later binding of a name replacing an
/// earlier one.
fn format_prefixes(model: &Model) -> PrefixMapping {
    let mut pm = if model.format_prefixes_cleared {
        default_ofn_prefixes(model)
    } else if !model.rdf_prefixes.is_empty() {
        rdfxml_format_prefixes(model)
    } else {
        model.prefixes.clone()
    };
    for (p, ns) in &model.added_prefixes {
        let _ = pm.add_prefix(p, ns);
    }
    pm
}

/// The prefixes an OWL/XML, Manchester or Turtle document declares: the format
/// prefixes without the default (`:`) binding, shortest name first (in UTF-16
/// code units), then in UTF-16 order.
fn written_prefixes(model: &Model) -> Vec<(String, String)> {
    use crate::io::natural_order::str_cmp;
    let utf16_len = |s: &str| s.encode_utf16().count();
    let mut v: Vec<(String, String)> = format_prefixes(model)
        .mappings()
        .filter(|(p, _)| !p.is_empty())
        .map(|(p, ns)| (p.clone(), ns.clone()))
        .collect();
    v.sort_by(|a, b| utf16_len(&a.0).cmp(&utf16_len(&b.0)).then_with(|| str_cmp(&a.0, &b.0)));
    v
}

/// The prefix map for an ontology whose document format carries no prefixes — i.e.
/// one built by `query --update`, which hands the result a fresh ontology (see
/// `Model::format_prefixes_cleared`). All that survives is the default `:` bound to
/// the ontology IRI ([`with_terminating_hash`]), plus the seed set below in its
/// own insertion order.
/// MONDO's `imports/merged_import.owl` ends in three `--update`s and comes out with
/// exactly these six lines and every other IRI written in full.
fn default_ofn_prefixes(model: &Model) -> PrefixMapping {
    let mut out = PrefixMapping::default();
    if let Some(iri) = crate::cmd::merge::ontology_iri(model) {
        let _ = out.add_prefix("", &with_terminating_hash(&iri));
    }
    for (p, ns) in [
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("xml", "http://www.w3.org/XML/1998/namespace"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ] {
        let _ = out.add_prefix(p, ns);
    }
    // What the ontology's own construction bound is declared all the same (see
    // `Model::built_prefixes`).
    for (p, ns) in &model.built_prefixes {
        let _ = out.add_prefix(p, ns);
    }
    out
}

/// Drop prefix bindings XML forbids, so an RDF/XML write is always re-readable.
///
/// XML Names §3 reserves two prefixes: `xml` may be bound only to
/// `http://www.w3.org/XML/1998/namespace`, and `xmlns` may not be bound at all.
/// An OWL prefix map is under no such constraint, and real ontologies carry
/// illegal ones — EFO's `components/gwas_import.owl` declares
/// `Prefix(xml:=<https://www.w3.org/TR/xml#>)`. Emitting that verbatim as an
/// `xmlns:xml` produces a document no parser will take back, owlmake's own rename
/// round-trip included ("the namespace prefix 'xml' cannot be bound to …"), so
/// `mint` over EFO's import closure would fail on it. Dropping the binding costs
/// nothing: `xml:` is implicitly bound in every XML document.
fn xml_legal_prefixes(pm: &PrefixMapping) -> PrefixMapping {
    const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
    let mut out = PrefixMapping::default();
    for (prefix, ns) in pm.mappings() {
        if prefix == "xmlns" || (prefix == "xml" && ns != XML_NS) {
            continue;
        }
        let _ = out.add_prefix(prefix, ns);
    }
    out
}

/// Rewrite RDF/XML so each prefix namespace is declared as an XML entity and
/// referenced as `&prefix;` in IRIs (`--xml-entities`). Adds a
/// `<!DOCTYPE rdf:RDF [ <!ENTITY pfx "ns"> … ]>` after the XML declaration and
/// replaces `"<ns>` with `"&pfx;` in attribute values (`rdf:about`,
/// `rdf:resource`, `rdf:datatype`, and the `xmlns:` declarations themselves).
fn xml_entities_transform(body: &[u8], prefixes: &[(String, String)]) -> Vec<u8> {
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        // Not valid UTF-8 (shouldn't happen for RDF/XML) — leave untouched.
        Err(_) => return body.to_vec(),
    };

    // Only entity-able prefixes: non-empty name made of name-safe chars, and a
    // non-empty namespace. Longest namespace first so a namespace that is a
    // prefix of another (e.g. `…/obo/` vs `…/obo/RO_`) does not steal the match.
    let mut usable: Vec<(&str, &str)> = prefixes
        .iter()
        .filter(|(p, n)| {
            !p.is_empty()
                && !n.is_empty()
                // `xml` is reserved — it cannot be redeclared via `xmlns:xml`.
                && p != "xml"
                && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        })
        .map(|(p, n)| (p.as_str(), n.as_str()))
        .collect();
    if usable.is_empty() {
        return body.to_vec();
    }
    usable.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    // Split off the `<?xml … ?>` declaration so the DOCTYPE can follow it.
    let (decl, rest) = match text.find("?>") {
        Some(i) if text.trim_start().starts_with("<?xml") => text.split_at(i + 2),
        _ => ("", text),
    };

    let mut replaced = rest.to_string();
    for (pfx, ns) in &usable {
        replaced = replaced.replace(&format!("\"{ns}"), &format!("\"&{pfx};"));
    }

    let mut entities = String::new();
    for (pfx, ns) in &usable {
        // `%`/`&` cannot appear literally in an entity value; OBO namespaces never
        // contain them, but guard anyway.
        let safe_ns = ns.replace('&', "&amp;").replace('%', "&#37;");
        entities.push_str(&format!("    <!ENTITY {pfx} \"{safe_ns}\">\n"));
    }

    let mut out = String::with_capacity(text.len() + entities.len() + 64);
    if !decl.is_empty() {
        out.push_str(decl.trim_end());
        out.push('\n');
    }
    out.push_str("<!DOCTYPE rdf:RDF [\n");
    out.push_str(&entities);
    out.push_str("]>\n");
    out.push_str(replaced.trim_start_matches('\n'));
    out.into_bytes()
}

/// A conservative check that `local` is a legal CURIE local part for the
/// functional-syntax writer: no characters that would break re-parsing.
fn is_valid_curie_local(local: &str) -> bool {
    !local.is_empty()
        && !local.contains('/')
        && !local.contains('#')
        && !local.contains(' ')
        && !local.contains(':')
        && !local.starts_with('-')
        && !local.starts_with('.')
}

