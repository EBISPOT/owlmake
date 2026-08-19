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

use crate::model::{AnonBlock, CmOnto, Model, Onto};

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

pub mod manchester;
pub mod manchester_parse;
pub mod genid;
pub mod obo;
pub mod obograph;
pub mod owlfunc;
pub mod owlrdf;
pub mod turtle;

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
pub fn load(path: &Path) -> Result<Model> {
    let bytes = std::fs::read(path).with_context(|| format!("opening {}", path.display()))?;
    let fmt = match Format::from_path(path) {
        Ok(f) => disambiguate(f, &bytes),
        Err(_) => sniff(&bytes)
            .with_context(|| format!("cannot determine ontology format of {}", path.display()))?,
    };
    parse_bytes(bytes, fmt, &display_name(path)).with_context(|| format!("parsing {}", path.display()))
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

/// Refine an extension-derived format using content when the extension is
/// ambiguous between RDF/XML and a text syntax.
fn disambiguate(ext_fmt: Format, bytes: &[u8]) -> Format {
    match ext_fmt {
        Format::RdfXml => sniff(bytes).unwrap_or(Format::RdfXml),
        other => other,
    }
}

/// Load an ontology from `path`, optionally forcing the parser format
/// (`--input-format`) instead of inferring it from the extension/content.
pub fn load_with(path: &Path, format: Option<&str>) -> Result<Model> {
    match format {
        Some(name) => {
            let fmt = Format::from_name(name)?;
            let bytes = std::fs::read(path).with_context(|| format!("opening {}", path.display()))?;
            parse_bytes(bytes, fmt, &display_name(path))
                .with_context(|| format!("parsing {}", path.display()))
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
    load_from(std::io::Cursor::new(bytes), fmt).with_context(|| format!("parsing {iri}"))
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
/// Only a transport error or a 5xx is retried; a 404 is an answer.
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
    let trimmed = head.trim_start();
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

/// The `idspace:` set an OWL document declares: every `xmlns:`-declared prefix
/// whose namespace is not a built-in one (the OBO base, RDF, RDFS, XSD, OWL, XML),
/// in declaration order, one prefix per distinct namespace. Scanned from the raw
/// bytes because RDF/XML keeps no formal prefix map that horned-owl surfaces.
///
/// A declared prefix earns an idspace even when no id is ever shortened with it —
/// UBERON writes its `foaf`/`doap` IRIs out in full yet still declares both.
fn scan_owl_idspaces(bytes: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(bytes);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen_ns: std::collections::HashSet<String> = std::collections::HashSet::new();
    // The `idspace:` set and CURIE shortening are driven by the document's declared
    // `xmlns:PREFIX` bindings — NOT by which namespaces merely occur in the body. A
    // namespace used only via a default `xmlns="…"` on an element (as cl-full.owl
    // declares dc/terms/skos/foaf) is not a prefix, so it gets no idspace and
    // shortens nothing; ids under it fall to the mechanical local-name rule instead.
    // So scan only the `xmlns:PREFIX="NS"` declarations below; do not pre-seed
    // well-known namespaces.
    let is_builtin = |ns: &str| {
        ns.starts_with("http://purl.obolibrary.org/obo/")
            || ns.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
            || ns.starts_with("http://www.w3.org/2000/01/rdf-schema#")
            || ns.starts_with("http://www.w3.org/2001/XMLSchema#")
            || ns.starts_with("http://www.w3.org/2002/07/owl#")
            || ns.starts_with("http://www.w3.org/XML/1998/namespace")
    };
    // Parse `xmlns:PREFIX="NS"` declarations by hand (RDF/XML, no dependency on a
    // full XML parse). The default `xmlns=` (no prefix) never becomes an idspace.
    for decl in text.split("xmlns:").skip(1) {
        let Some(eq) = decl.find('=') else { continue };
        let prefix = decl[..eq].trim();
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            continue;
        }
        let rest = decl[eq + 1..].trim_start();
        let quote = match rest.bytes().next() {
            Some(b @ (b'"' | b'\'')) => b as char,
            _ => continue,
        };
        let Some(end) = rest[1..].find(quote) else { continue };
        // As in `scan_all_prefixes`: the raw attribute text still spells its
        // entity references out, and a namespace is what they stand for.
        let ns = unescape_xml(&rest[1..1 + end]);
        if is_builtin(&ns) || seen_ns.contains(&ns) {
            continue;
        }
        seen_ns.insert(ns.clone());
        out.push((prefix.to_string(), ns));
    }
    out
}

/// Resolve the XML predefined entity references and character references in an
/// attribute value. An entity the document declares itself is left as written:
/// nothing here reads a DTD.
fn unescape_xml(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let name = &tail[1..semi];
        let resolved = match name {
            "amp" => Some("&".to_string()),
            "lt" => Some("<".to_string()),
            "gt" => Some(">".to_string()),
            "quot" => Some("\"".to_string()),
            "apos" => Some("'".to_string()),
            _ => name.strip_prefix('#').and_then(|n| {
                let code = match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse::<u32>().ok(),
                }?;
                char::from_u32(code).map(|c| c.to_string())
            }),
        };
        match resolved {
            Some(text) => out.push_str(&text),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

/// Scan every `xmlns:PREFIX="NS"` declaration in an RDF/XML document, in order,
/// keeping built-in prefixes (unlike [`scan_owl_idspaces`]) — the full prefix map
/// the RDF/XML writer re-declares on `rdf:RDF`.
fn scan_all_prefixes(bytes: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(bytes);
    // Only the `rdf:RDF` opening tag carries the document prefixes; stop at its
    // close `>` so body attributes named `xmlns:…` (there are none in practice)
    // can't leak in.
    let root_tag = format!("<{}RDF", rdf_prefix(&text));
    let head = match text.find(root_tag.as_str()) {
        Some(i) => {
            let rest = &text[i..];
            let end = rest.find('>').map(|e| i + e + 1).unwrap_or(text.len());
            &text[i..end]
        }
        None => &text[..text.len().min(8192)],
    };
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for decl in head.split("xmlns:").skip(1) {
        let Some(eq) = decl.find('=') else { continue };
        let prefix = decl[..eq].trim();
        if prefix.is_empty()
            || !prefix.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            continue;
        }
        let rest = decl[eq + 1..].trim_start();
        let quote = match rest.bytes().next() {
            Some(b @ (b'"' | b'\'')) => b as char,
            _ => continue,
        };
        let Some(end) = rest[1..].find(quote) else { continue };
        let ns = &rest[1..1 + end];
        // The scan reads the attribute's raw text, so its entity references are
        // still spelled out; a namespace is the value they stand for. FoodOn
        // declares `xmlns:itis="…?search_topic=TSN&amp;search_value="`, and the
        // prefix it binds ends in a bare `&`.
        let ns = unescape_xml(ns);
        // The RDF namespace is always written back as `rdf`, whatever the source
        // called it, so a document declaring `xmlns:r=` must not put `xmlns:r=` in
        // the artefact.
        let prefix = if ns == "http://www.w3.org/1999/02/22-rdf-syntax-ns#" { "rdf" } else { prefix };
        if seen.insert(prefix.to_string()) {
            out.push((prefix.to_string(), ns));
        }
    }
    out
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
/// that order is scanned here (the analog of [`scan_owl_reif_order`]).
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

/// Per subject IRI, the ordered [`crate::io::owlrdf::reif_signature`]s of the
/// `<owl:Axiom>` reification blocks in the source RDF/XML — so the writer can
/// replay the order the source carried them in, which is not reconstructible from
/// horned's unordered model.
fn scan_owl_reif_order(bytes: &[u8]) -> std::collections::HashMap<String, Vec<String>> {
    let text = String::from_utf8_lossy(bytes);
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let open = "    <owl:Axiom>\n";
    let close = "    </owl:Axiom>\n";
    let mut idx = 0usize;
    while let Some(rel) = text[idx..].find(open) {
        let s = idx + rel;
        let Some(e_rel) = text[s..].find(close) else { break };
        let end = s + e_rel + close.len();
        let block = &text[s..end];
        let src = "<owl:annotatedSource rdf:resource=\"";
        if let Some(a) = block.find(src) {
            let a = a + src.len();
            if let Some(q) = block[a..].find('"') {
                let subject = block[a..a + q].to_string();
                let sig = crate::io::owlrdf::reif_signature(block);
                out.entry(subject).or_default().push(sig);
            }
        }
        idx = end;
    }
    out
}

/// The bare `<rdf:Description>` blocks (no `rdf:about`) an RDF/XML document carries
/// in its Individuals section for annotation assertions on ANONYMOUS individuals
/// (EFO's obsolescence records). horned's RDF reader discards anonymous-subject
/// assertions, so these are captured verbatim and passed through.
///
/// A bare `<rdf:Description>` is NOT always an anonymous individual, though: the
/// n-ary collections in the General axioms section have the same shape. horned
/// reads every one of those into a real axiom, so the model already carries them
/// and the writer already renders them — capturing them here too would mean a
/// second, unfilterable copy. In EFO's `fbbt_import.owl`,
/// `AllDisjointClasses(CARO_0000003 CARO_0000004 CARO_0020000)` is dropped by
/// `filter --trim true` (none of the three is in `fbbt_terms.txt`), and a verbatim
/// block would put it straight back under an Individuals banner it does not belong
/// in.
///
/// Identify them by the collection predicate: `owl:distinctMembers` is
/// `AllDifferent`, and `owl:members` is `AllDisjointClasses` /
/// `AllDisjointProperties` / `AllDifferent`. An anonymous individual's assertions
/// use neither.
///
/// SWRL is the same trap in a louder form. A `swrl:Imp`, its `swrl:AtomList`
/// cells and its `swrl:Variable`s are all bare `<rdf:Description>`s, and horned
/// reads every one into a `Component::Rule` the writer renders in its own section.
/// Captured here as well, EFO's `uberon_import.owl` would carry back three rules
/// that `filter --trim true` dropped (their `BSPO_0000120` is not in
/// `uberon_terms.txt`) — and with no rule left in the model the writer declares no
/// `xmlns:swrl`, so the file would not even be readable: "Unknown prefix swrl:".
fn scan_owl_anon_individual_blocks(bytes: &[u8]) -> Vec<AnonBlock> {
    let text = String::from_utf8_lossy(bytes);
    let allocs = anon_allocations(&text);
    let pfx = rdf_prefix(&text);
    let mut out: Vec<AnonBlock> = Vec::new();
    for (s, end) in top_level_anon_descriptions(&text) {
        let block = &text[s..end];
        let renders_from_model = block.contains("owl:distinctMembers")
            || block.contains("owl:members")
            || block.contains("http://www.w3.org/2003/11/swrl#");
        if !renders_from_model {
            // The block's own parse-time blank node is the allocation made AT its
            // offset — `partition_point` finds how many were made strictly before.
            let alloc = allocs.partition_point(|&o| o < s) as u64;
            // Replayed verbatim, but under the prefix the OUTPUT binds the RDF
            // namespace to. Output always says `rdf:`, whatever the source used, so
            // a document that says `r:Description` must not have that spelling
            // copied into the artefact.
            let text = if pfx == "rdf:" {
                block.to_string()
            } else {
                block
                    .replace(&format!("<{pfx}"), "<rdf:")
                    .replace(&format!("</{pfx}"), "</rdf:")
                    .replace(&format!(" {pfx}"), " rdf:")
            };
            out.push(AnonBlock { offset: s as u64, alloc, text });
        }
    }
    out
}

/// The prefix a document binds the RDF namespace to, with its colon — normally
/// `rdf:`, but nothing in XML requires that, so a document is read under whatever
/// prefix it declares.
///
/// Everything the two scanners look for is a prefixed name, so hard-coding the
/// literal string `rdf:…` breaks on a document binding the namespace to something
/// else: the root element goes unrecognised, node and property elements are read
/// one level out of phase, and every `<rdfs:subClassOf r:resource=…/>` looks like
/// an anonymous node — the same 5.5x over-count the alternation comment describes
/// — while the anonymous-individual blocks are missed entirely and the Individuals
/// section comes out empty.
///
/// A default binding (`xmlns="…rdf-syntax-ns#"`) names ELEMENTS only; an
/// unprefixed attribute is in no namespace, so such a document cannot write
/// `rdf:about` at all and the attribute needles stay `rdf:`.
pub(crate) fn rdf_prefix(text: &str) -> String {
    const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let head = &text[..text.len().min(65536)];
    for m in ["\"", "'"] {
        let needle = format!("={m}{RDF_NS}{m}");
        let mut from = 0usize;
        while let Some(rel) = head[from..].find(&needle) {
            let at = from + rel;
            let decl = head[..at].rsplit(char::is_whitespace).next().unwrap_or("");
            if let Some(p) = decl.strip_prefix("xmlns:") {
                if !p.is_empty() {
                    return format!("{p}:");
                }
            }
            from = at + needle.len();
        }
    }
    "rdf:".to_string()
}

/// Byte ranges of the TOP-LEVEL `<rdf:Description>` elements — the shape an
/// anonymous individual takes, indented four spaces as a direct child of `rdf:RDF`.
///
/// Both halves need care. The open tag must be anchored to a line start, or the
/// four-space needle also matches inside an eight-space indent — a nested
/// anonymous individual, which is what an anonymous annotation VALUE renders as.
/// The close must be the MATCHING one, not the first following: for a nested block
/// the first is the INNER one, and the captured text is then unbalanced XML that
/// reaches the Individuals section verbatim as a file no parser would take back,
/// handed to the next build step. Matching the first close instead truncates a
/// large share of the elements in real documents; those truncated blocks happen to
/// carry the SWRL namespace, so the caller's content filter discards them and the
/// net count comes out right by accident — nothing structural is doing that job.
///
/// Neither needle may embed `\n`: line boundaries are tested separately, and `\r`
/// counts as one, so a CRLF document matches too rather than losing every anonymous
/// individual without a diagnostic.
fn top_level_anon_descriptions(text: &str) -> Vec<(usize, usize)> {
    let pfx = rdf_prefix(text);
    let open = format!("    <{pfx}Description>");
    let close = format!("</{pfx}Description>");
    let open_tag = format!("<{pfx}Description");
    let bytes = text.as_bytes();
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = text[i..].find(&open) {
        let s = i + rel;
        let after = s + open.len();
        // Exactly four spaces of indent: the needle must begin a line. A deeper
        // indent puts non-newline bytes before it and is skipped — it belongs to
        // whichever top-level element encloses it.
        let starts_line = s == 0 || bytes[s - 1] == b'\n';
        // …and the tag must end its line, so `<rdf:Description rdf:about=…>` (a
        // NAMED node, and not an individual at all) cannot match.
        let ends_line = matches!(text.as_bytes().get(after), Some(b'\n') | Some(b'\r'));
        if !starts_line || !ends_line {
            i = after;
            continue;
        }
        match anon_description_end(text, after, &open_tag, &close) {
            Some(end) => {
                out.push((s, end));
                i = end;
            }
            None => break,
        }
    }
    out
}

/// The offset just past the `</rdf:Description>` that closes the element opened
/// before `from`, counting nested opens so an anonymous annotation value does not
/// end its parent early. Includes the close tag's line terminator (`\n` or
/// `\r\n`) so the captured text is whole lines.
fn anon_description_end(text: &str, from: usize, open_tag: &str, close: &str) -> Option<usize> {
    let mut depth = 1i32;
    let mut i = from;
    loop {
        let open_at = text[i..].find(open_tag).map(|r| i + r);
        let close_at = text[i..].find(close).map(|r| i + r);
        match (open_at, close_at) {
            (Some(o), Some(c)) if o < c => {
                let tag_end = o + text[o..].find('>')?;
                // `<rdf:Descriptions…` is not this element; and a self-closing
                // `<rdf:Description …/>` opens nothing to close.
                let name_end = o + open_tag.len();
                let is_element = matches!(
                    text.as_bytes().get(name_end),
                    Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
                );
                if is_element && !text[o..tag_end].ends_with('/') {
                    depth += 1;
                }
                i = tag_end + 1;
            }
            (_, Some(c)) => {
                depth -= 1;
                let mut end = c + close.len();
                if text[end..].starts_with("\r\n") {
                    end += 2;
                } else if text[end..].starts_with('\n') {
                    end += 1;
                }
                if depth == 0 {
                    return Some(end);
                }
                i = end;
            }
            _ => return None,
        }
    }
}

/// The blank-node counter. Anonymous individuals are numbered upwards from 2^31
/// for the life of the process, so `_:genid2147483648` is the first one any parse
/// in this run mints.
static ANON_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(2_147_483_648);

/// Reserve `n` consecutive blank-node ids and return the first.
fn mint_anon_ids(n: usize) -> u64 {
    ANON_COUNTER.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed)
}

/// Start the blank-node counter over.
///
/// The counter's span is one recipe line: everything a single line does — a
/// merge, its reasoning, its writes — numbers from 2^31, and the next line starts
/// again. A build that ran the whole release off one counter would give the second
/// artefact ids the first one's parses had already used up.
pub fn reset_anon_counter() {
    ANON_COUNTER.store(2_147_483_648, std::sync::atomic::Ordering::Relaxed);
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

/// The verbatim anonymous-individual blocks, in the order a released RDF/XML file
/// carries them.
///
/// That order is neither document order nor label order. An anonymous individual is
/// RE-NUMBERED when the section is rendered, and the renumbering visits them in
/// hash order of their PARSE-TIME `_:genid<N>` label: buckets ascending, document
/// order within a bucket. Reproducing it is what keeps a re-serialized file from
/// reshuffling a section whose content has not changed, so a release diff shows
/// real edits and nothing else.
///
/// `N` is `anon_alloc_base` (everything the import closure consumed first) plus
/// the block's own document-relative position, over the counter seed below. EFO's
/// edit file puts its fourteen obsolescence records at 2148125419… .
///
/// The bucket mask is the hash table's capacity, which owlmake does NOT model: for
/// fourteen entries the answer is the same at every capacity from 256 up, and a
/// document with few enough anonymous individuals to sit below that has too few
/// for the mask to separate them differently. `HASH_CAPACITY` is therefore fixed
/// well above any real count.
///
/// A block that is an OWL CONSTRUCT rather than an individual is dropped, not
/// ordered: an `owl:inverseOf` renders inline within its property frame and a
/// `NegativePropertyAssertion` after the individual it is about, both from the
/// model. The input scan sees only `<rdf:Description>` and mis-collects them.
pub(crate) fn anon_individual_order(
    blocks: &[AnonBlock],
    base: u64,
    capacity: u64,
    imports_end: u64,
) -> Vec<&String> {
    /// The fallback when the document's own capacity is not known — a non-RDF/XML
    /// source, or a model assembled rather than parsed. Taken larger than any real
    /// anonymous-individual count so the mask at least never splits a small set.
    const HASH_CAPACITY: u64 = 1 << 20;
    let capacity = if capacity == 0 { HASH_CAPACITY } else { capacity };
    /// Blank-node ids run upwards from 2^31, so the first one allocated is
    /// `_:genid2147483648`. The seed is part of the hashed STRING, so it cannot
    /// be dropped as a common offset.
    const COUNTER_SEED: u64 = 2_147_483_648;
    // The type is named as an IRI in an `rdf:resource`, not as an element, so it
    // is matched on the local name alone.
    let is_construct =
        |t: &str| t.contains("owl:inverseOf") || t.contains("NegativePropertyAssertion");
    let mut kept: Vec<&AnonBlock> =
        blocks.iter().filter(|b| !is_construct(&b.text)).collect();
    if std::env::var("OM_ANON_DEBUG").is_ok() {
        eprintln!("[anon] base={base} capacity={capacity} blocks={}", kept.len());
        for b in &kept {
            let bb = if b.offset > imports_end { base } else { 0 };
            eprintln!("[anon]   alloc={} id=_:genid{}", b.alloc, COUNTER_SEED + bb + b.alloc);
        }
    }
    // A stable sort by bucket leaves same-bucket blocks in document order, which
    // is the insertion order within a bucket.
    // A block the document allocates BEFORE its `owl:imports` is numbered without
    // the closure — an import is loaded when its triple streams past, so a header at
    // the bottom of the file charges nothing to what precedes it. For one document
    // written both ways, header first gives base 3 and header last gives base 0.
    let base_for = |b: &AnonBlock| if b.offset > imports_end { base } else { 0 };
    kept.sort_by_key(|b| {
        java_hash_bucket(&format!("_:genid{}", COUNTER_SEED + base_for(b) + b.alloc), capacity)
    });
    kept.into_iter().map(|b| &b.text).collect()
}

/// The hash-table capacity the anonymous-individual ordering masks against after
/// `n` distinct keys — see [`anon_individual_order`]. Sizing is capacity 16, load
/// factor 0.75, doubling whenever the size exceeds three quarters of it. The table
/// never shrinks, so this is the capacity at iteration time even though the parser
/// removes triples as it consumes them.
///
/// The rendered order of two anonymous individuals flips at exactly 13, 22, 42,
/// 82, 202, 402, 3002 and 12002 literal-bearing subjects — every one a resize
/// point of this rule.
pub(crate) fn hash_map_capacity(n: u64) -> u64 {
    let mut cap = 16u64;
    while n > cap * 3 / 4 {
        cap <<= 1;
    }
    cap
}

/// The bucket a string key falls in: the 31-multiplier hash over its UTF-16 code
/// units, spread by `h ^ (h >> 16)` across the 32-bit value, masked to the table
/// size.
fn java_hash_bucket(key: &str, capacity: u64) -> u64 {
    let mut h: u32 = 0;
    for c in key.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(c as u32);
    }
    ((h ^ (h >> 16)) as u64) & (capacity - 1)
}

/// The byte offset of every blank node an RDF/XML document allocates, in document
/// order, plus (as `len()`) the total the document consumes.
///
/// These are numbered `_:genid<N>` from a counter that runs across the whole import
/// closure, and that number decides the order the document's anonymous INDIVIDUALS
/// come out in — see [`anon_individual_order`]. Two sites allocate:
///
/// * a NODE element carrying none of `rdf:about` / `rdf:ID` / `rdf:nodeID`;
/// * each cell of an `rdf:parseType="Collection"` list.
///
/// Node and property elements strictly ALTERNATE — the children of `rdf:RDF` are
/// node elements, theirs are property elements, and so on. Getting that backwards
/// over-counts by 5.5× on a real ontology, because every
/// `<rdfs:subClassOf rdf:resource="…"/>` then looks like an anonymous node.
///
/// The count has to be exact to the unit: it is the offset every later allocation
/// is made from, so one node too many in an imported file shifts the base of every
/// document that imports it and reorders its whole Individuals section. EFO's
/// 23-file import closure plus its edit file consumes 641,861 of them.
fn anon_allocations(text: &str) -> Vec<usize> {
    rdfxml_scan(text).allocs
}

/// What one pass over an RDF/XML document tells us about its blank-node structure.
pub(crate) struct RdfXmlScan {
    /// Byte offset of every blank node the document allocates.
    pub allocs: Vec<usize>,
    /// Distinct subjects that carry at least one LITERAL triple — the number of
    /// keys the anonymous-individual ordering hashes over, and therefore the
    /// capacity that decides which anonymous individual is re-numbered first.
    /// Resource-valued triples do not size it: adding 300 classes carrying only
    /// `rdf:resource` properties leaves the rendered order at the smaller capacity,
    /// while adding 10 carrying `rdfs:label` moves it.
    pub literal_subjects: u64,
}

fn rdfxml_scan(text: &str) -> RdfXmlScan {
    /// What an open element is, which decides what its CHILDREN are: the
    /// children of the document element and of a property element are node
    /// elements, and the children of a node element are property elements.
    #[derive(Clone, PartialEq)]
    enum Pos {
        Root,
        /// A node element, carrying the key that identifies it as a triple
        /// SUBJECT — its `rdf:about`/`rdf:ID`/`rdf:nodeID`, or its own allocation
        /// index when it is anonymous.
        Node(String),
        /// …and whether it is a `parseType="Collection"`, whose children each
        /// also take an `rdf:List` cell.
        Prop(bool),
    }
    let pfx = rdf_prefix(text);
    let (root, about, id_at, node_id, parse_type, resource) = (
        format!("{pfx}RDF"),
        format!("{pfx}about"),
        format!("{pfx}ID"),
        format!("{pfx}nodeID"),
        format!("{pfx}parseType"),
        format!("{pfx}resource"),
    );
    let b = text.as_bytes();
    let mut out: Vec<usize> = Vec::new();
    let mut stack: Vec<Pos> = Vec::new();
    let mut lit_subjects: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut i = 0usize;
    while i < b.len() {
        let Some(rel) = text[i..].find('<') else { break };
        let at = i + rel;
        let rest = &text[at..];
        // Comments, CDATA, doctype and processing instructions carry no elements.
        if let Some(skip) = skip_non_element(rest) {
            i = at + skip;
            continue;
        }
        if rest.starts_with("</") {
            stack.pop();
            i = at + rest.find('>').map_or(rest.len(), |e| e + 1);
            continue;
        }
        let Some(e) = rest.find('>') else { break };
        let tag = &rest[1..e];
        let self_closing = tag.ends_with('/');
        let name_end = tag.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(tag.len());
        let name = &tag[..name_end];
        // The document element is neither a node nor a property element; after it
        // the two alternate.
        if name == root {
            stack.push(Pos::Root);
        } else if !matches!(stack.last(), Some(Pos::Node(_))) {
            if let Some(Pos::Prop(true)) = stack.last() {
                out.push(at); // the rdf:List cell
            }
            let named = [&about, &id_at, &node_id]
                .iter()
                .find_map(|a| attr_value(tag, a).map(|v| v.to_string()));
            let key = match &named {
                Some(v) => v.clone(),
                None => {
                    out.push(at); // the node itself
                    format!("\u{1}anon{at}")
                }
            };
            // A property ATTRIBUTE on a node element (`<owl:Class rdf:about="…"
            // rdfs:label="x"/>`) is a literal triple on that node, the same as the
            // element form.
            if has_property_attribute(tag, &pfx) {
                lit_subjects.insert(key.clone());
            }
            stack.push(Pos::Node(key));
        } else {
            // `rdf:parseType="Resource"` is an abbreviation for a nested anonymous
            // node element, and one is materialised here, taking an id of its own.
            // Its children are that node's PROPERTY elements. (EFO uses none; other
            // repos do.)
            match attr_value(tag, &parse_type) {
                Some("Resource") => {
                    out.push(at);
                    stack.push(Pos::Node(format!("\u{1}anon{at}")));
                }
                // `rdf:parseType="Literal"` makes the element's content an XML
                // LITERAL: its children are markup in the literal's value, not RDF
                // node elements, and nothing is allocated for them. Walked as RDF, a
                // rich-text `<rdfs:comment
                // rdf:parseType="Literal"><span>…<b>…</b></span></rdfs:comment>`
                // would count its `<span>` as an anonymous node, making the
                // document's total too large and every importer's base with it.
                Some("Literal") if !self_closing => {
                    let close = format!("</{name}>");
                    match text[at + e..].find(&close) {
                        Some(rel) => {
                            i = at + e + rel + close.len();
                            continue;
                        }
                        None => break,
                    }
                }
                pt => {
                    // A property element whose value is a LITERAL — text, or an
                    // XML literal, or an empty one. Anything carrying
                    // `rdf:resource`/`rdf:nodeID`, or a `parseType` that makes its
                    // content RDF, points at a resource instead.
                    if pt.is_none_or(|p| p == "Literal")
                        && !has_attr(tag, &resource)
                        && !has_attr(tag, &node_id)
                        && (self_closing || !property_content_is_element(&rest[e + 1..]))
                    {
                        if let Some(Pos::Node(key)) = stack.last() {
                            lit_subjects.insert(key.clone());
                        }
                    }
                    stack.push(Pos::Prop(pt == Some("Collection")))
                }
            }
        }
        if self_closing {
            stack.pop();
        }
        i = at + e + 1;
    }
    RdfXmlScan { allocs: out, literal_subjects: lit_subjects.len() as u64 }
}

/// Whether a node element's start tag carries a property ATTRIBUTE — an
/// attribute that is a predicate rather than RDF bookkeeping or a namespace
/// declaration. `<owl:Class rdf:about="…" rdfs:label="x"/>` asserts a literal
/// triple exactly as the element form does.
fn has_property_attribute(tag: &str, pfx: &str) -> bool {
    let reserved: [String; 7] = [
        format!("{pfx}about"),
        format!("{pfx}ID"),
        format!("{pfx}nodeID"),
        format!("{pfx}type"),
        format!("{pfx}datatype"),
        format!("{pfx}parseType"),
        format!("{pfx}resource"),
    ];
    let mut rest = tag;
    while let Some(eq) = rest.find('=') {
        let name = rest[..eq].trim().rsplit(char::is_whitespace).next().unwrap_or("").trim();
        let after = &rest[eq + 1..];
        let Some(q) = after.trim_start().chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let vstart = after.find(q).map(|p| p + 1).unwrap_or(after.len());
        let vend = after[vstart..].find(q).map(|p| vstart + p).unwrap_or(after.len());
        if !name.is_empty()
            && name != "xmlns"
            && !name.starts_with("xmlns:")
            && !name.starts_with("xml:")
            && !reserved.iter().any(|r| r == name)
        {
            return true;
        }
        rest = &after[vend.min(after.len())..];
        if rest.is_empty() {
            break;
        }
        rest = &rest[1.min(rest.len())..];
    }
    false
}

/// Whether a property element's content begins with a child ELEMENT (so its value
/// is a resource) rather than text (so its value is a literal). Comments,
/// processing instructions and whitespace are skipped.
fn property_content_is_element(content: &str) -> bool {
    let mut rest = content;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return false;
        }
        if !trimmed.starts_with('<') {
            return false;
        }
        if trimmed.starts_with("</") {
            return false; // empty element content: an empty literal
        }
        match skip_non_element(trimmed) {
            Some(skip) => rest = &trimmed[skip..],
            None => return true,
        }
    }
}

/// How far to skip past a `<!-- … -->`, `<![CDATA[ … ]]>`, `<!DOCTYPE …>` or
/// `<? … ?>`, none of which is an element. `None` when `rest` opens an element.
fn skip_non_element(rest: &str) -> Option<usize> {
    for (open, close) in [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>")] {
        if let Some(body) = rest.strip_prefix(open) {
            return Some(open.len() + body.find(close).map_or(body.len(), |e| e + close.len()));
        }
    }
    if rest.starts_with("<!") {
        return Some(rest.find('>').map_or(rest.len(), |e| e + 1));
    }
    None
}

/// Whether a start tag's attribute list carries `name`, matched on the whole
/// attribute name so `rdf:ID` does not also match `rdf:IDX`.
fn has_attr(tag: &str, name: &str) -> bool {
    attr_value(tag, name).is_some()
}

fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0usize;
    while let Some(rel) = tag[from..].find(name) {
        let at = from + rel;
        from = at + name.len();
        let before_ok = at == 0 || tag.as_bytes()[at - 1].is_ascii_whitespace();
        let after = tag[from..].trim_start();
        if before_ok && after.starts_with('=') {
            let after = after[1..].trim_start();
            let quote = after.chars().next()?;
            if quote == '"' || quote == '\'' {
                let body = &after[1..];
                return Some(&body[..body.find(quote).unwrap_or(body.len())]);
            }
        }
    }
    None
}

#[cfg(test)]
mod anon_alloc_tests {
    use super::anon_allocations;

    fn n(body: &str) -> usize {
        anon_allocations(&format!(
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">{body}</rdf:RDF>"
        ))
        .len()
    }

    #[test]
    fn a_named_node_and_its_resource_properties_allocate_nothing() {
        // The trap: node and property elements ALTERNATE. Read the other way
        // round, `<rdfs:subClassOf rdf:resource=…/>` looks like an anonymous node
        // and a real ontology over-counts by 5.5x.
        assert_eq!(
            n(r#"<owl:Class rdf:about="http://x/A"><rdfs:subClassOf rdf:resource="http://x/B"/></owl:Class>"#),
            0
        );
    }

    #[test]
    fn a_bare_node_element_allocates_one() {
        assert_eq!(n("<rdf:Description><x:p>v</x:p></rdf:Description>"), 1);
        assert_eq!(n(r#"<rdf:Description rdf:about="http://x/A"><x:p>v</x:p></rdf:Description>"#), 0);
        assert_eq!(n(r#"<rdf:Description rdf:nodeID="g1"><x:p>v</x:p></rdf:Description>"#), 0);
    }

    #[test]
    fn an_anonymous_restriction_allocates_one() {
        assert_eq!(
            n(r#"<owl:Class rdf:about="http://x/A"><rdfs:subClassOf><owl:Restriction>
                   <owl:onProperty rdf:resource="http://x/p"/>
                   <owl:someValuesFrom rdf:resource="http://x/B"/>
                 </owl:Restriction></rdfs:subClassOf></owl:Class>"#),
            1
        );
    }

    #[test]
    fn a_collection_allocates_a_cell_per_member() {
        // Two named members: two list cells, no member nodes.
        assert_eq!(
            n(r#"<owl:Class rdf:about="http://x/A"><owl:intersectionOf rdf:parseType="Collection">
                   <rdf:Description rdf:about="http://x/B"/>
                   <rdf:Description rdf:about="http://x/C"/>
                 </owl:intersectionOf></owl:Class>"#),
            2
        );
        // An anonymous member takes its own id as well as its cell.
        assert_eq!(
            n(r#"<owl:Class rdf:about="http://x/A"><owl:intersectionOf rdf:parseType="Collection">
                   <rdf:Description rdf:about="http://x/B"/>
                   <owl:Restriction><owl:onProperty rdf:resource="http://x/p"/></owl:Restriction>
                 </owl:intersectionOf></owl:Class>"#),
            3
        );
    }

    #[test]
    fn parse_type_resource_materialises_a_node() {
        assert_eq!(
            n(r#"<rdf:Description rdf:about="http://x/A"><x:p rdf:parseType="Resource">
                   <x:q rdf:resource="http://x/B"/>
                 </x:p></rdf:Description>"#),
            1
        );
    }

    #[test]
    fn comments_and_declarations_are_not_elements() {
        assert_eq!(n("<!-- <rdf:Description> --><?pi <rdf:Description> ?>"), 0);
    }

    #[test]
    fn parse_type_literal_content_is_markup_not_rdf() {
        // The literal's `<span>`/`<b>` are characters in a value, not node
        // elements: nothing is allocated for them. Counted as RDF, the document's
        // total comes out too large and every importer's base with it.
        assert_eq!(
            n(r#"<rdf:Description rdf:about="http://x/A">
                   <rdfs:comment rdf:parseType="Literal"><span>hi <b>there</b></span></rdfs:comment>
                 </rdf:Description>"#),
            0
        );
        // …and the element after it is still read in the right phase.
        assert_eq!(
            n(r#"<rdf:Description rdf:about="http://x/A">
                   <rdfs:comment rdf:parseType="Literal"><span>x</span></rdfs:comment>
                   <rdfs:subClassOf><owl:Restriction/></rdfs:subClassOf>
                 </rdf:Description>"#),
            1
        );
    }
}

#[cfg(test)]
mod anon_capacity_tests {
    use super::{hash_map_capacity, rdfxml_scan};

    fn subjects(body: &str) -> u64 {
        rdfxml_scan(&format!(
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">{body}</rdf:RDF>"
        ))
        .literal_subjects
    }

    /// Table sizing: capacity 16, load factor 0.75, double when the size passes
    /// three quarters. The rendered order of two anonymous individuals flips at
    /// exactly these counts.
    #[test]
    fn capacity_doubles_at_three_quarters_load() {
        assert_eq!(hash_map_capacity(0), 16);
        assert_eq!(hash_map_capacity(12), 16);
        assert_eq!(hash_map_capacity(13), 32);
        assert_eq!(hash_map_capacity(24), 32);
        assert_eq!(hash_map_capacity(25), 64);
        assert_eq!(hash_map_capacity(48), 64);
        assert_eq!(hash_map_capacity(49), 128);
    }

    #[test]
    fn only_literal_valued_triples_size_the_map() {
        // A resource-valued property does not put its subject in the map — 300 of
        // them leave the rendered order at the smaller capacity.
        assert_eq!(
            subjects(r#"<owl:Class rdf:about="http://x/A"><rdfs:subClassOf rdf:resource="http://x/B"/></owl:Class>"#),
            0
        );
        // A literal-valued one does…
        assert_eq!(
            subjects(r#"<owl:Class rdf:about="http://x/A"><rdfs:label>a</rdfs:label></owl:Class>"#),
            1
        );
        // …once per SUBJECT, however many literals it carries.
        assert_eq!(
            subjects(r#"<owl:Class rdf:about="http://x/A"><rdfs:label>a</rdfs:label><rdfs:comment>c</rdfs:comment></owl:Class>"#),
            1
        );
        // A property ATTRIBUTE is the same triple written shorter.
        assert_eq!(subjects(r#"<owl:Class rdf:about="http://x/A" rdfs:label="a"/>"#), 1);
        // …but `rdf:about` and the namespace declarations are not predicates.
        assert_eq!(
            subjects(r#"<owl:Class xmlns:owl="http://www.w3.org/2002/07/owl#" rdf:about="http://x/A"/>"#),
            0
        );
        // An anonymous subject counts too, and separately from a named one.
        assert_eq!(
            subjects(r#"<owl:Class rdf:about="http://x/A"><rdfs:label>a</rdfs:label></owl:Class>
                        <rdf:Description><rdfs:label>b</rdfs:label></rdf:Description>"#),
            2
        );
    }
}

/// Load an ontology of the given format from any buffered reader.
pub fn load_from<R: BufRead>(reader: R, fmt: Format) -> Result<Model> {
    let mut model = guard_parse(fmt, move || load_from_raw(reader, fmt))?;
    canonicalize_rules(&mut model);
    Ok(model)
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
    let mut cfg = ParserConfiguration::default();
    cfg.lax = !run_options().strict;
    match fmt {
        Format::RdfXml => {
            // RDF/XML carries no formal prefix map, so buffer the bytes and scan the
            // `xmlns:` declarations for the document's own prefix map — the set an
            // OBO write emits as `idspace:` lines.
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf)?;
            let idspaces = scan_owl_idspaces(&buf);
            let rdf_prefixes = scan_all_prefixes(&buf);
            // Read unconditionally, for the same reason the SHARING scan below is:
            // these describe the SOURCE, and every RDF/XML file owlmake writes goes
            // through the writer that consumes them. Anything that made the scans
            // conditional would leave that writer with no record of the source on an
            // ordinary build.
            let (owl_genid_refs, owl_reif_order, owl_anon_blocks, owl_label_order) = (
                scan_owl_body_genids(&buf),
                scan_owl_reif_order(&buf),
                scan_owl_anon_individual_blocks(&buf),
                scan_label_order(&buf),
            );
            // The SHARING scan is read on the same terms: blank-node sharing is a
            // property of the source that every later step needs. `om make` turns the
            // owlrdf writer on per artefact through a thread-local, so a scan that
            // only ran when that writer was already on would reach it with no record
            // of which expressions were one node.
            let owl_shared_owners = scan_owl_shared_owners(&buf);
            let (rdfo, _incomplete): (horned_owl::io::rdf::reader::ConcreteRcRDFOntology, _) =
                horned_owl::io::rdf::reader::read(&mut buf.as_slice(), cfg)
                    .map_err(|e| anyhow::anyhow!("RDF/XML parse error: {e}"))?;
            // Move components out of the parser's Rc set rather than deep-cloning
            // every one (the naive From<ConcreteRDFOntology>).
            let ont: Onto = rdfo.into_set_ontology_fast();
            let mut model = Model::from_parts(ont, crate::model::default_prefixes());
            model.idspaces = idspaces;
            model.rdf_prefixes = rdf_prefixes;
            model.owl_genid_refs = owl_genid_refs;
            model.owl_reif_order = owl_reif_order;
            model.owl_label_order = owl_label_order;
            model.owl_anon_blocks = owl_anon_blocks;
            let raw = String::from_utf8_lossy(&buf);
            let scan = rdfxml_scan(&raw);
            model.anon_alloc_total = scan.allocs.len() as u64;
            model.anon_hash_capacity = hash_map_capacity(scan.literal_subjects);
            model.anon_imports_end = raw.rfind("owl:imports").map_or(0, |p| p as u64);
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
            let (ont, prefixes): (Onto, PrefixMapping) =
                horned_owl::io::owx::reader::read(&mut reader, cfg)
                    .map_err(|e| anyhow::anyhow!("OWL/XML parse error: {e}"))?;
            Ok(Model::from_parts(ont, prefixes))
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
            let prefixes_cleared = text.lines().any(|l| l.trim_end() == "#prefixes-cleared");
            let rdf_prefixes = parse_prefix_comment("#rdfxmlns ");
            let explicit_prefixes = parse_prefix_comment("#explicit-prefixes ");
            let idspaces = parse_prefix_comment("#idspaces ");
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
            // The SAME blank-node counter advances for a functional-syntax parse, so
            // an `owl:imports` naming a `.ofn` moves the importing document's base
            // exactly as an RDF/XML one does. A file with one trailing anonymous
            // `<rdf:Description>` re-numbers to `_:genid2147483649`, and the same
            // file importing an OFN with three anonymous individuals re-numbers to
            // `_:genid2147483652` — three slots. Left at 0, every importer's base is
            // short and its anonymous individuals come out in the wrong order.
            //
            // It is the distinct `_:` LABELS, not the anonymous expressions: an
            // ontology whose only anonymous nodes are class expressions costs 0
            // slots in OFN, OWL/XML, Turtle and OBO alike, because those are written
            // inline and never named.
            model.anon_alloc_total = model.anon_doc_order.len() as u64;
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
        Format::Manchester => manchester::load(reader),
        Format::Turtle => turtle::load(reader),
        Format::NTriples => turtle::load_as(reader, oxigraph::io::RdfFormat::NTriples),
    }
}

/// Save `model` to `path`, inferring the format from its extension.
pub fn save(model: &mut Model, path: &Path) -> Result<()> {
    let fmt = Format::from_path(path)?;
    save_as(model, path, fmt)
}

/// Save `model` to `path` in the explicitly given format. Shows a byte heartbeat
/// for the (potentially multi-GB) serialization, which is otherwise silent.
///
/// Takes `&mut Model` so the XML writers (which require an owned
/// `ComponentMappedOntology`) can *move* the components in and back out rather
/// than deep-cloning the whole ontology — a multi-GB copy on phenio-scale
/// inputs. The model is left unchanged once the write returns.
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
/// This does not change rendering order: every writer already sorts operands as
/// it emits them. It only merges duplicates.
///
/// Scope is the class-expression axioms MONDO actually exercises. The other
/// set-valued axioms — `SameIndividual`, `DifferentIndividuals`,
/// `EquivalentObjectProperties`, `DisjointObjectProperties`, `HasKey`, … — have
/// the same Vec-vs-Set mismatch and are deliberately left alone until an artefact
/// shows they matter. (`SubObjectPropertyOf`'s chain is genuinely ordered and must
/// never be sorted.)
pub fn normalize_set_operands(model: &mut Model) {
    use horned_owl::model::{
        AnnotatedComponent, ClassExpression as CE, Component, MutableOntology, RcStr,
    };
    use crate::io::owlfunc::{cmp_ce, cmp_individual};

    fn norm_ce(ce: &CE<RcStr>) -> CE<RcStr> {
        match ce {
            CE::ObjectIntersectionOf(ops) => CE::ObjectIntersectionOf(sorted(ops)),
            CE::ObjectUnionOf(ops) => CE::ObjectUnionOf(sorted(ops)),
            CE::ObjectComplementOf(b) => CE::ObjectComplementOf(Box::new(norm_ce(b))),
            CE::ObjectSomeValuesFrom { ope, bce } => CE::ObjectSomeValuesFrom {
                ope: ope.clone(),
                bce: Box::new(norm_ce(bce)),
            },
            CE::ObjectAllValuesFrom { ope, bce } => CE::ObjectAllValuesFrom {
                ope: ope.clone(),
                bce: Box::new(norm_ce(bce)),
            },
            CE::ObjectMinCardinality { n, ope, bce } => CE::ObjectMinCardinality {
                n: *n,
                ope: ope.clone(),
                bce: Box::new(norm_ce(bce)),
            },
            CE::ObjectMaxCardinality { n, ope, bce } => CE::ObjectMaxCardinality {
                n: *n,
                ope: ope.clone(),
                bce: Box::new(norm_ce(bce)),
            },
            CE::ObjectExactCardinality { n, ope, bce } => CE::ObjectExactCardinality {
                n: *n,
                ope: ope.clone(),
                bce: Box::new(norm_ce(bce)),
            },
            CE::ObjectOneOf(inds) => {
                let mut v = inds.clone();
                v.sort_by(cmp_individual);
                v.dedup();
                CE::ObjectOneOf(v)
            }
            other => other.clone(),
        }
    }

    fn sorted(ops: &[CE<RcStr>]) -> Vec<CE<RcStr>> {
        let mut v: Vec<CE<RcStr>> = ops.iter().map(norm_ce).collect();
        v.sort_by(cmp_ce);
        v.dedup();
        v
    }

    let mut replace: Vec<(AnnotatedComponent<RcStr>, AnnotatedComponent<RcStr>)> = Vec::new();
    for ac in model.ont.iter() {
        let new_c = match &ac.component {
            Component::EquivalentClasses(ax) => {
                Component::EquivalentClasses(horned_owl::model::EquivalentClasses(sorted(&ax.0)))
            }
            Component::DisjointClasses(ax) => {
                Component::DisjointClasses(horned_owl::model::DisjointClasses(sorted(&ax.0)))
            }
            Component::SubClassOf(ax) => Component::SubClassOf(horned_owl::model::SubClassOf {
                sub: norm_ce(&ax.sub),
                sup: norm_ce(&ax.sup),
            }),
            Component::DisjointUnion(ax) => Component::DisjointUnion(
                horned_owl::model::DisjointUnion(ax.0.clone(), sorted(&ax.1)),
            ),
            Component::ClassAssertion(ax) => {
                Component::ClassAssertion(horned_owl::model::ClassAssertion {
                    ce: norm_ce(&ax.ce),
                    i: ax.i.clone(),
                })
            }
            Component::ObjectPropertyDomain(ax) => {
                Component::ObjectPropertyDomain(horned_owl::model::ObjectPropertyDomain {
                    ope: ax.ope.clone(),
                    ce: norm_ce(&ax.ce),
                })
            }
            Component::ObjectPropertyRange(ax) => {
                Component::ObjectPropertyRange(horned_owl::model::ObjectPropertyRange {
                    ope: ax.ope.clone(),
                    ce: norm_ce(&ax.ce),
                })
            }
            _ => continue,
        };
        if new_c != ac.component {
            replace.push((ac.clone(), AnnotatedComponent { component: new_c, ann: ac.ann.clone() }));
        }
    }
    for (old, new) in replace {
        model.ont.remove(&old);
        model.ont.insert(new);
    }
}

pub fn save_as(model: &mut Model, path: &Path, fmt: Format) -> Result<()> {
    // Create the output's parent directory if needed — steps write into `subsets/`,
    // `tmp/`, `reports/` etc. which a fresh checkout may not contain and which no
    // earlier step is required to have made.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    // The `#…` marker lines the Functional writer uses to carry source state
    // (xmlns block, shared blank nodes, cleared prefixes) belong to owlmake's
    // own `*.ofn` cache files, never to a released artefact: MONDO's
    // `imports/merged_import.owl` is written by `convert -f ofn` under a `.owl`
    // name, and a released document carries no such lines.
    // …and only for a `.ofn` living in a BUILD directory. Intermediates are parked
    // in the build's temporary directory and owlmake's own cache in
    // `.owlmake-odk-tmp`; a `.ofn` written anywhere else is on its way to being a
    // released artefact. OBA builds `patterns/definitions.owl` by merging into
    // `definitions.ofn` and moving it into place — in `src/ontology`, not `tmp` —
    // so it must not gain a `#prefixes-cleared` first line, while MONDO's
    // `tmp/mondo.owl.ofn` needs its markers because `mondo.obo` re-reads it.
    // The directory the file is written INTO decides this, not any directory the
    // path happens to pass through: a temp-dir `x.ofn` and a `<work>/x.ofn` both
    // put the cache directly in the build dir, whereas matching any path component
    // would tag every `.ofn` a repo checked out under a path containing `tmp` ever
    // wrote — including its released artefacts.
    OFN_CACHE.with(|c| {
        c.set(
            path.extension().and_then(|e| e.to_str()) == Some("ofn")
                && matches!(
                    path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()),
                    Some("tmp" | ".owlmake-odk-tmp")
                ),
        )
    });
    OUT_NAME.with(|c| *c.borrow_mut() = display_name(path));
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut pw =
        crate::progress::ProgressWriter::new(BufWriter::new(file), format!("write {}", display_name(path)));
    // A file on disk is read by the next build step and shipped as a release, so
    // RDF/XML written to one always gets the full-fidelity bytes — there is no
    // build in which a file should differ from the artefact shape a release
    // carries.
    write_to_with(model, &mut pw, fmt, RdfXmlWriter::Owlapi)
        .with_context(|| format!("writing {}", path.display()))?;
    pw.finish().with_context(|| format!("writing {}", path.display()))?;
    Ok(())
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
/// by the internal buffer-serialization paths (turtle/sparql/rename round-trips)
/// that only hold an immutable borrow. Like [`write_to`] it selects
/// [`RdfXmlWriter::Horned`], so both are for buffers owlmake parses straight back
/// itself; a file is written by [`save_as`], which selects the full-fidelity
/// RDF/XML writer instead.
pub fn write_to_ref<W: Write>(model: &Model, writer: W, fmt: Format) -> Result<()> {
    let mut tmp = Model::from_parts(model.ont.clone(), crate::model::clone_prefixes(&model.prefixes));
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
/// shipped as a release, so it gets the full-fidelity bytes; a buffer owlmake is
/// about to parse itself (the SPARQL/rename round-trips in `write_to_ref`) only has
/// to be valid RDF, and putting it through the full writer would make every query
/// pay for byte-fidelity nothing reads.
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
            // WIP full-fidelity RDF/XML writer (see owlrdf.rs).
            crate::io::owlrdf::save(model, &mut writer)?;
            return Ok(());
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
                let r = horned_owl::io::rdf::writer::write_with_prefixes(
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
                let r = horned_owl::io::rdf::writer::write_with_prefixes(
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
            let prefixes = safe_prefixes(model);
            let cm = take_cm(model);
            let r = horned_owl::io::owx::writer::write(&mut writer, &cm, Some(&prefixes))
                .map_err(|e| anyhow::anyhow!("OWL/XML write error: {e}"));
            restore_cm(model, cm);
            r?;
        }
        Format::Functional => {
            // Hand the writer the document's OWN prefixes, in document order: a
            // functional document declares every prefix it has and abbreviates by
            // longest valid match. The writer falls back to a full <IRI> for any IRI
            // no declared prefix can validly abbreviate, so passing all prefixes
            // always round-trips.
            let document = if model.format_prefixes_cleared {
                default_ofn_prefixes(model)
            } else if !model.rdf_prefixes.is_empty() {
                rdfxml_format_prefixes(model)
            } else {
                model.prefixes.clone()
            };
            // A saved prefix format always binds the DEFAULT prefix to the ontology
            // IRI plus `#`, so every functional file opens `Prefix(:=<…#>)`.
            // owlmake's own DOSDP modules are anonymous ontologies, so they carry no
            // `:`; without this the released `patterns/definitions.owl` — merged
            // from them and then annotated with an ontology IRI — loses its first
            // line.
            let default_ns = prefixes_default_ns(model, &document);
            let prefixes = ofn_prefix_block(&document, default_ns.as_deref());
            let labels = model.banner_labels.clone();
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
            if model.format_prefixes_cleared && ofn_cache() {
                write!(writer, "#prefixes-cleared\n")
                    .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"))?;
            }
            if !model.rdf_prefixes.is_empty() && ofn_cache() {
                let joined: Vec<String> = model
                    .rdf_prefixes
                    .iter()
                    .filter(|(p, ns)| !p.is_empty() && !p.contains(' ') && !ns.contains(' '))
                    .map(|(p, ns)| format!("{p}={ns}"))
                    .collect();
                if !joined.is_empty() {
                    write!(writer, "#rdfxmlns {}\n", joined.join(" "))
                        .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"))?;
                }
            }
            // Carry the shared-blank-node identity of the RDF/XML this cache stands
            // in for. OFN has no way to express that two anonymous expressions were
            // ONE node, and the `rdf:nodeID`-vs-inline choice depends on exactly
            // that, so without this the numbering pass downstream cannot reproduce it.
            // Precedence: an explicit `shared_anon` wins, else whatever the RDF/XML
            // write this cache stands in for recorded on the model.
            let shared_anon = if model.shared_anon.is_empty() {
                model.rdf_shared_anon.clone()
            } else {
                model.shared_anon.clone()
            };
            for (owner, sigs) in shared_anon.iter().filter(|_| ofn_cache()) {
                if owner.contains(' ') || sigs.is_empty() {
                    continue;
                }
                let mut v: Vec<String> = sigs.iter().map(|h| format!("{h:x}")).collect();
                v.sort();
                write!(writer, "#anonshare {owner} {}\n", v.join(" "))
                    .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"))?;
            }
            // Carry the source's blank-node SHARING evidence too. `#anonshare` holds
            // structure hashes; this holds the `(class, property, filler)` keys the
            // RDF scan derived from repeated `rdf:nodeID`s. Without it a pipeline
            // that hops through the OFN cache loses the only record of which
            // anonymous expressions were ONE node, and every annotated axiom starts
            // taking a node of its own.
            for (owner, keys) in model.owl_shared_owners.iter().filter(|_| ofn_cache()) {
                if owner.contains(' ') || keys.is_empty() {
                    continue;
                }
                let mut v: Vec<String> =
                    keys.iter().map(|k| k.replace('\u{1}', "\u{2}")).collect();
                v.sort();
                if v.iter().any(|k| k.contains(' ')) {
                    continue;
                }
                write!(writer, "#sharedowner {owner} {}\n", v.join(" "))
                    .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"))?;
            }
            // Likewise carry the explicitly-provided (`--add-prefixes`) prefixes, so
            // a downstream OBO write from this OFN emits their idspaces (one per
            // explicit prefix, whether or not it is used).
            if !model.explicit_prefixes.is_empty() && ofn_cache() {
                let joined: Vec<String> = model
                    .explicit_prefixes
                    .iter()
                    .filter(|(p, ns)| !p.is_empty() && !p.contains(' ') && !ns.contains(' '))
                    .map(|(p, ns)| format!("{p}={ns}"))
                    .collect();
                if !joined.is_empty() {
                    write!(writer, "#explicit-prefixes {}\n", joined.join(" "))
                        .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"))?;
                }
            }
            // And the document's declared idspaces — for an OBO-sourced pipeline this
            // is the prefix set owlrdf falls back to when there is no input xmlns
            // (`rdf_prefixes` empty), so it must survive the OFN cache too.
            if !model.idspaces.is_empty() && ofn_cache() {
                let joined: Vec<String> = model
                    .idspaces
                    .iter()
                    .filter(|(p, ns)| !p.is_empty() && !p.contains(' ') && !ns.contains(' '))
                    .map(|(p, ns)| format!("{p}={ns}"))
                    .collect();
                if !joined.is_empty() {
                    write!(writer, "#idspaces {}\n", joined.join(" "))
                        .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"))?;
                }
            }
            let cm = take_cm(model);
            // Which class this document's untyped literals take, which decides where
            // they sort against `xsd:anyURI` — see `Model::plain_literals_typed`.
            // Set per WRITE, from the model, the same way `owlrdf` does it.
            horned_owl::io::ofn::writer::set_plain_literals_typed(model.plain_literals_typed);
            let r = horned_owl::io::ofn::writer::write_with_labels(
                &mut writer,
                &cm,
                Some(&prefixes),
                labels_opt,
                order_opt,
            )
            .map_err(|e| anyhow::anyhow!("Functional Syntax write error: {e}"));
            restore_cm(model, cm);
            r?;
        }
        Format::Obo => obo::save(model, &mut writer)?,
        Format::OboGraph => obograph::save(model, &mut writer)?,
        Format::Manchester => manchester::save(model, &mut writer)?,
        Format::Turtle => turtle::save(model, &mut writer)?,
        Format::NTriples => {
            turtle::save_as(model, &mut writer, oxigraph::io::RdfFormat::NTriples)?
        }
    }
    Ok(())
}

/// The `Prefix(p:=<ns>)` declarations a Functional document makes for itself, in
/// declaration order. The empty prefix (`Prefix(:=<…>)`) is the document's default
/// namespace rather than an xmlns binding re-declared by name, so it is skipped
/// here.
fn document_ofn_prefixes(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Prefix(") else {
            // Prefix declarations form the document's head; the first axiom ends them.
            if line.starts_with("Ontology(") {
                break;
            }
            continue;
        };
        let Some(rest) = rest.strip_suffix(')') else { continue };
        let Some((name, iri)) = rest.split_once(":=") else { continue };
        let iri = iri.trim().trim_start_matches('<').trim_end_matches('>');
        if iri.is_empty() {
            continue;
        }
        // The DEFAULT binding counts. Dropping it made "this document declared no
        // prefixes" indistinguishable from "it declared only a default", and the
        // OFN writer takes a different branch for the two: a file whose only
        // `Prefix(…)` line is `:` fell through to the full CURIE map and kept the
        // `:` that `saveOntology` overwrites. `rdfxml_format_prefixes` skips it.
        out.push((name.to_string(), iri.to_string()));
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
    let Some(base) = text
        .split("Prefix(:=<")
        .nth(1)
        .and_then(|rest| rest.split('>').next())
        .filter(|b| !b.is_empty())
    else {
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
/// extracted import module: the default `:` prefix bound to the ontology IRI plus
/// `#`, then `owl`, `rdf`, `xml`, `xsd`, `rdfs`. Nothing else — OBO CURIEs like
/// `obo:` are deliberately absent so entity IRIs render as full `<IRI>`, which is
/// the shape released pattern and module files carry. A functional document that
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
    // ontology IRI + '#', which is what a freshly built pattern module gets.
    let mut self_ns: Option<String> = model
        .prefixes
        .mappings()
        .find(|(name, _)| name.is_empty())
        .map(|(_, ns)| ns.clone());
    if self_ns.is_none() {
        for ac in model.ont.iter() {
            if let horned_owl::model::Component::OntologyID(id) = &ac.component {
                if let Some(iri) = &id.iri {
                    self_ns = Some(format!("{}#", iri.as_ref()));
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
    /// [`crate::io::owlrdf::save`]). Set by [`save_as`].
    static OUT_NAME: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
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
    crate::cmd::merge::ontology_iri(model).map(|iri| format!("{iri}#"))
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
fn ofn_prefix_block(document: &PrefixMapping, default_ns: Option<&str>) -> PrefixMapping {
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

/// The prefix map for an ontology whose document format carries no prefixes — i.e.
/// one built by `query --update`, which hands the result a fresh ontology (see
/// `Model::format_prefixes_cleared`). All that survives is the default `:` bound to
/// the ontology IRI with a `#`, plus the seed set below in its own insertion order.
/// MONDO's `imports/merged_import.owl` ends in three `--update`s and comes out with
/// exactly these six lines and every other IRI written in full.
fn default_ofn_prefixes(model: &Model) -> PrefixMapping {
    let mut out = PrefixMapping::default();
    if let Some(iri) = crate::cmd::merge::ontology_iri(model) {
        let _ = out.add_prefix("", &format!("{iri}#"));
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

/// horned-owl's functional/OWL-XML writers abbreviate any IRI whose namespace
/// matches a prefix, even when the resulting local part is not a legal CURIE
/// (e.g. it contains `/`, as in `http://purl.obolibrary.org/obo/ro/subsets#x`).
/// Such output cannot be re-parsed, so any prefix that would do this is dropped
/// and the affected IRIs fall back to a full `<IRI>`.
fn safe_prefixes(model: &Model) -> PrefixMapping {
    use horned_owl::visitor::immutable::{entity::IRIExtract, Walk};

    let mut walk = Walk::new(IRIExtract::default());
    walk.set_ontology(&model.ont);
    let iris: Vec<String> = walk
        .into_visit()
        .into_set()
        .into_iter()
        .map(|i| i.as_ref().to_string())
        .collect();

    let mut out = PrefixMapping::default();
    for (prefix, ns) in model.prefixes.mappings() {
        let safe = iris
            .iter()
            .filter(|iri| iri.starts_with(ns.as_str()))
            .all(|iri| is_valid_curie_local(&iri[ns.len()..]));
        if safe {
            let _ = out.add_prefix(prefix, ns);
        }
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
/// functional/OWL-XML writers: no characters that would break re-parsing.
fn is_valid_curie_local(local: &str) -> bool {
    !local.is_empty()
        && !local.contains('/')
        && !local.contains('#')
        && !local.contains(' ')
        && !local.contains(':')
        && !local.starts_with('-')
        && !local.starts_with('.')
}
