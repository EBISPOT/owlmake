//! Lightweight terminal progress reporting for the long phases of `reason`
//! (RDF parse, saturation). Renders an animated bar with `\r` when stderr is a
//! TTY, and throttled one-line snapshots otherwise (so a redirected log stays
//! readable). Disabled by `OWLMAKE_PROGRESS=0`.

use std::io::{IsTerminal, Read, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use crate::time::Instant;

use owo_colors::{OwoColorize, Style};

pub fn enabled() -> bool {
    !matches!(std::env::var("OWLMAKE_PROGRESS").ok().as_deref(), Some("0"))
}

// ─────────────────────────── colour + timestamps ────────────────────────────
//
// Every status / progress line owlmake prints to stderr flows through here so
// it can carry an optional wall-clock timestamp and ANSI colour. Both are
// auto-enabled only when stderr is a terminal and can be forced/suppressed:
//   NO_COLOR (any value)      → no colour (https://no-color.org)
//   OWLMAKE_COLOR=0/1         → force colour off / on
//   OWLMAKE_TIMESTAMPS=0      → no `[HH:MM:SS]` prefix

/// Whether to emit ANSI colour on stderr (cached; evaluated once).
pub fn use_color() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        match std::env::var("OWLMAKE_COLOR").ok().as_deref() {
            Some("0") | Some("never") | Some("off") => false,
            Some("1") | Some("always") | Some("on") => true,
            _ => std::io::stderr().is_terminal(),
        }
    })
}

/// Whether to prefix status/progress lines with a `[HH:MM:SS]` wall-clock stamp.
pub fn use_timestamps() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| {
        !matches!(
            std::env::var("OWLMAKE_TIMESTAMPS").ok().as_deref(),
            Some("0") | Some("off") | Some("never")
        )
    })
}

/// Format a duration (seconds) compactly as `M:SS` (or `H:MM:SS` past an hour),
/// for the elapsed/ETA fields of long-running progress lines.
pub fn fmt_hms(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// Apply `style` to `text` only when colour is enabled, else return it plain.
fn styled(text: &str, style: Style) -> String {
    if use_color() {
        text.style(style).to_string()
    } else {
        text.to_string()
    }
}

/// The dimmed `[HH:MM:SS] ` prefix (empty string when timestamps are disabled).
pub fn timestamp_prefix() -> String {
    if !use_timestamps() {
        return String::new();
    }
    // jiff (not chrono): its `tz-system` support reads the local zone from
    // `/etc/localtime` directly, giving real local time on every target without
    // linking Apple's CoreFoundation framework (chrono's iana-time-zone does,
    // which the SDK-free macOS cross-build can't link). See Cargo.toml.
    let now = jiff::Zoned::now().strftime("%H:%M:%S").to_string();
    styled(&format!("[{now}] "), Style::new().dimmed())
}

/// Pick a colour for a leading `label:` token by its apparent severity.
fn label_style(label: &str) -> Style {
    let l = label.to_ascii_lowercase();
    if l.starts_with("error") || l == "err" {
        Style::new().red().bold()
    } else if l.starts_with("warn") {
        Style::new().yellow().bold()
    } else if l == "note" {
        Style::new().dimmed()
    } else {
        Style::new().cyan().bold()
    }
}

/// Colour the leading `label:` token of a status line (the convention owlmake's
/// status output follows — `reason:`, `make:`, `WARNING:`, …). Lines without a
/// short label-like prefix are returned unchanged.
fn paint(msg: &str) -> String {
    if !use_color() {
        return msg.to_string();
    }
    let head_end = msg.find('\n').unwrap_or(msg.len());
    let head = &msg[..head_end];
    // A status line carries a `label:` somewhere in its first line (`reason:`,
    // `WARNING:`, `write out.owl:`); free prose without a colon is left plain.
    let Some(colon) = head.find(':') else {
        return msg.to_string();
    };
    // Colour only the leading phase *word* (the token up to the first space or
    // the colon), so `write out.owl:` colours `write` and leaves the filename.
    let trimmed = head.trim_start();
    let indent_len = head.len() - trimmed.len();
    let word_end = trimmed
        .find(|c: char| c.is_whitespace() || c == ':')
        .unwrap_or(trimmed.len());
    let word = &trimmed[..word_end];
    if word.is_empty() || word.len() > 24 {
        return msg.to_string();
    }
    // Guard against painting the wrong span when the word starts past the colon
    // (a colon inside an indented sub-bullet, say).
    if indent_len + word_end > colon + 1 {
        return msg.to_string();
    }
    let coloured = styled(word, label_style(word));
    format!("{}{coloured}{}", &msg[..indent_len], &msg[indent_len + word_end..])
}

/// Render a full status line — `[HH:MM:SS] ` prefix + colourised label — without
/// the trailing newline. Used by both [`status_emit`] and the [`Progress`] bar.
fn decorate(msg: &str) -> String {
    format!("{}{}", timestamp_prefix(), paint(msg))
}

/// Print one decorated status line to stderr. Backs the `status!` macro; not
/// throttled (unlike the [`Progress`] heartbeats), since each call is a discrete
/// event the caller meant to surface.
pub fn status_emit(args: std::fmt::Arguments) {
    let msg = format!("{args}");
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{}", decorate(&msg));
}

/// Diagnostic verbosity (`-v`/`-vv`/`-vvv`), set once per command from
/// [`crate::cmd::CommonArgs`]. 0 = quiet (default); higher prints load/save and
/// per-step diagnostics to stderr. Process-global because the whole run executes
/// on a single worker thread (see `main`).
static VERBOSITY: AtomicU8 = AtomicU8::new(0);

/// Record the requested verbosity. Called from `CommonArgs::activate`.
pub fn set_verbosity(v: u8) {
    VERBOSITY.store(v, Ordering::Relaxed);
}

/// Current verbosity level (0 unless `-v`/`-vv`/`-vvv` was given).
pub fn verbosity() -> u8 {
    VERBOSITY.load(Ordering::Relaxed)
}

/// A `Read` wrapper that silently counts bytes into a shared atomic, so a
/// separate heartbeat thread can report progress through both the byte-read and
/// the (subsequent, byte-silent) triple→axiom mapping phases of RDF parsing.
pub struct CountReader<R> {
    inner: R,
    count: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl<R: Read> CountReader<R> {
    pub fn new(inner: R, count: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
        CountReader { inner, count }
    }
}

impl<R: Read> Read for CountReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

/// A background ticking status line for a single blocking phase whose internal
/// progress isn't observable from the outside (e.g. a whole-ontology format
/// round-trip). Spawns a thread that prints `label…  <elapsed>` on the throttle
/// until the guard is dropped, so a long opaque step isn't a silent freeze.
pub struct Heartbeat {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    /// Start ticking `label` (a full status prefix, e.g.
    /// `"reason: hermit-rs converting model"`). A no-op when progress is disabled.
    pub fn start(label: impl Into<String>) -> Self {
        let label = label.into();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // A higher-level stage spinner owns the line; `Progress` publishes into it
        // rather than writing, so the heartbeat still ticks.
        if !enabled() {
            return Heartbeat { done, handle: None };
        }
        let handle = {
            let done = done.clone();
            std::thread::spawn(move || {
                let mut bar = Progress::new(label.clone(), 0);
                let start = Instant::now();
                while !done.load(Ordering::Relaxed) {
                    bar.line(&format!("{label}…  {}", fmt_hms(start.elapsed().as_secs_f64())));
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            })
        };
        Heartbeat { done, handle: Some(handle) }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        clear_detail();
    }
}

/// A single high-level build stage, rendered as a numbered headline
///
/// ```text
/// [3/14] Importing 1184 terms from uberon.owl using the BOT-module strategy.
///        <dimmed explanation of what the step does, wrapped to the terminal>
///        ⠹ 2s        ← live spinner (replaced by ✓/✗ when the stage finishes)
/// ```
///
/// On a TTY the spinner animates in place and is rewritten as a green `✓` (or a
/// red `✗`) with the elapsed time once the stage ends. On a non-TTY (a redirected
/// log) the headline + explanation are printed once and a single `done`/`failed`
/// line is appended — no cursor games. Honours `OWLMAKE_PROGRESS=0`.
///
/// A stage may carry an optional shared byte counter (e.g. a mirror download), in
/// which case the spinner line also shows `N MB`; otherwise it is a bare spinner.
pub struct Stage {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    start: Instant,
    tty: bool,
    on: bool,
    finished: bool,
}

/// Braille spinner frames (the de-facto cargo/npm spinner).
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Set while a [`Stage`] spinner is animating on a TTY, so per-step diagnostics
/// can stay quiet rather than scribbling over the live spinner line.
static STAGE_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a live stage spinner currently owns the terminal's last line.
pub fn stage_active() -> bool {
    STAGE_ACTIVE.load(Ordering::Relaxed)
}

/// The innermost live progress text, shown on the stage spinner's line.
///
/// A stage spinner owns the last line, so the finer-grained bars underneath it
/// (`reason: hermit-rs … 1403/99220 classes`) publish their text here instead of
/// writing it themselves and fighting over `\r`. The spinner renders it, so
/// there is one animation and a multi-hour classification says what it is doing
/// rather than showing a bare elapsed counter.
static DETAIL: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Publish the innermost progress text (see [`DETAIL`]).
pub fn set_detail(text: &str) {
    if let Ok(mut d) = DETAIL.lock() {
        d.clear();
        d.push_str(text);
    }
}

/// Drop the innermost progress text — the step producing it has finished.
pub fn clear_detail() {
    if let Ok(mut d) = DETAIL.lock() {
        d.clear();
    }
}

fn detail_text() -> String {
    DETAIL.lock().map(|d| d.clone()).unwrap_or_default()
}

/// Set while something else is writing to the terminal (a subprocess inheriting
/// stderr, say), so the spinner stops redrawing instead of interleaving with it.
static SUSPENDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Stop the stage spinner redrawing for as long as the guard lives, and clear the
/// line it owns so whatever writes next starts clean.
///
/// A replayed shell step inherits stderr, and without this guard its output
/// lands on top of the spinner (`⠸ 0:17Auto-excluding …`). Hold one of these
/// around anything that writes to the terminal itself.
pub struct Suspend(bool);

impl Suspend {
    pub fn new() -> Self {
        let was = SUSPENDED.swap(true, Ordering::Relaxed);
        if !was && stage_active() && std::io::stderr().is_terminal() {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[K");
            let _ = err.flush();
        }
        Suspend(was)
    }
}

impl Default for Suspend {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Suspend {
    fn drop(&mut self) {
        SUSPENDED.store(self.0, Ordering::Relaxed);
    }
}

impl Stage {
    /// Begin stage `idx` of `total`. `headline` is the one-line summary; `detail`
    /// is a (possibly multi-sentence) explanation shown dimmed below it. Pass an
    /// optional `bytes` counter to surface live download/IO progress on the
    /// spinner line.
    pub fn start(
        idx: usize,
        total: usize,
        headline: &str,
        detail: &str,
        bytes: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    ) -> Self {
        let on = enabled();
        let tty = std::io::stderr().is_terminal();
        // The numbered headline always prints (it's the durable record of the
        // plan); colour the `[X/Y]` marker like a label.
        let marker = styled(&format!("[{idx}/{total}]"), Style::new().cyan().bold());
        status_emit(format_args!("{marker} {headline}"));
        if !detail.is_empty() {
            for line in wrap_indent(detail, "       ") {
                let dim = styled(&line, Style::new().dimmed());
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "{}{dim}", timestamp_prefix());
            }
        }

        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // The animated spinner only makes sense on a TTY; off-TTY we stay silent
        // until `finish` appends the result line.
        let handle = if on && tty {
            STAGE_ACTIVE.store(true, Ordering::Relaxed);
            let done = done.clone();
            let start = Instant::now();
            Some(std::thread::spawn(move || {
                let mut frame = 0usize;
                while !done.load(Ordering::Relaxed) {
                    if SUSPENDED.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                    let glyph = styled(SPINNER[frame % SPINNER.len()], Style::new().cyan());
                    let el = fmt_hms(start.elapsed().as_secs_f64());
                    let suffix = match &bytes {
                        Some(b) => {
                            let mb = b.load(Ordering::Relaxed) as f64 / 1.0e6;
                            if mb >= 0.05 { format!("  {mb:.0} MB") } else { String::new() }
                        }
                        None => String::new(),
                    };
                    // The innermost bar's text, when there is one.
                    let detail = detail_text();
                    let detail = if detail.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", styled(&detail, Style::new().dimmed()))
                    };
                    let mut err = std::io::stderr().lock();
                    let _ = write!(err, "\r       {glyph} {el}{suffix}{detail}\x1b[K");
                    let _ = err.flush();
                    drop(err);
                    frame += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }))
        } else {
            None
        };

        Stage { done, handle, start: Instant::now(), tty, on, finished: false }
    }

    /// End the stage successfully — a green `✓` and the elapsed time.
    pub fn finish_ok(mut self) {
        clear_detail();
        self.finish(true);
    }

    /// End the stage in failure — a red `✗` and the elapsed time.
    pub fn finish_err(mut self) {
        self.finish(false);
    }

    fn finish(&mut self, ok: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.done.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        STAGE_ACTIVE.store(false, Ordering::Relaxed);
        if !self.on {
            return;
        }
        let el = fmt_hms(self.start.elapsed().as_secs_f64());
        let (glyph, word) = if ok {
            (styled("✓", Style::new().green().bold()), "done")
        } else {
            (styled("✗", Style::new().red().bold()), "failed")
        };
        let mut err = std::io::stderr().lock();
        if self.tty {
            // Overwrite the spinner line in place.
            let _ = writeln!(err, "\r       {glyph} {word} ({el})\x1b[K");
        } else {
            let _ = writeln!(err, "{}       {glyph} {word} ({el})", timestamp_prefix());
        }
        let _ = err.flush();
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        // A stage dropped without an explicit finish (e.g. via `?` propagating an
        // error out of the stage body) is reported as failed so the spinner thread
        // is always joined and the line is closed off.
        self.finish(false);
    }
}

/// Wrap `text` to the terminal width, prefixing every line with `indent`. Falls
/// back to 80 columns when the width can't be determined. Word-wraps on spaces.
fn wrap_indent(text: &str, indent: &str) -> Vec<String> {
    let cols = term_cols().max(40);
    let avail = cols.saturating_sub(indent.len()).max(20);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > avail {
            out.push(format!("{indent}{line}"));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(format!("{indent}{line}"));
    }
    out
}

/// Best-effort terminal column count (the `COLUMNS` env var, else 80).
fn term_cols() -> usize {
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(80)
}

/// A progress indicator. With a known `total` it shows a percentage bar; with
/// `total == 0` it shows a "heartbeat" (count + rate + elapsed) for phases whose
/// size isn't known ahead of time.
pub struct Progress {
    label: String,
    total: u64,
    start: Instant,
    last: Instant,
    tty: bool,
    on: bool,
    finished: bool,
}

impl Progress {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        let now = Instant::now();
        Progress {
            label: label.into(),
            total,
            start: now,
            // Force the first render by backdating `last` well past the throttle
            // window. `checked_sub` (not `-`) so this can't underflow on the wasm
            // clock, whose epoch is near zero — there it simply starts at `now`.
            last: now
                .checked_sub(std::time::Duration::from_secs(3600))
                .unwrap_or(now),
            tty: std::io::stderr().is_terminal(),
            on: enabled(),
            finished: false,
        }
    }

    /// Report current progress, throttled (100 ms on a TTY, 2 s to a file).
    pub fn set(&mut self, done: u64) {
        if !self.on || self.finished {
            return;
        }
        let now = Instant::now();
        let every = if self.tty { 0.1 } else { 2.0 };
        if now.duration_since(self.last).as_secs_f64() < every {
            return;
        }
        self.last = now;
        self.render(done, false);
    }

    /// Render an arbitrary status line (throttled), for phases whose progress
    /// isn't a single count — e.g. the saturation heartbeat showing rate + queue.
    pub fn line(&mut self, text: &str) {
        if !self.on || self.finished {
            return;
        }
        let now = Instant::now();
        let every = if self.tty { 0.1 } else { 2.0 };
        if now.duration_since(self.last).as_secs_f64() < every {
            return;
        }
        self.last = now;
        if stage_active() {
            set_detail(text);
            return;
        }
        let text = decorate(text);
        let mut err = std::io::stderr().lock();
        if self.tty {
            let _ = write!(err, "\r{text}\x1b[K");
        } else {
            let _ = writeln!(err, "{text}");
        }
        let _ = err.flush();
    }

    /// Final arbitrary status line (newline on a TTY), ignoring the throttle.
    pub fn finish_line(&mut self, text: &str) {
        if !self.on || self.finished {
            return;
        }
        self.finished = true;
        let text = decorate(text);
        let mut err = std::io::stderr().lock();
        if self.tty {
            let _ = writeln!(err, "\r{text}\x1b[K");
        } else {
            let _ = writeln!(err, "{text}");
        }
        let _ = err.flush();
    }

    /// Final render (newline on a TTY so the next output starts cleanly).
    pub fn finish(&mut self, done: u64) {
        if !self.on || self.finished {
            return;
        }
        self.finished = true;
        self.render(done, true);
    }

    fn render(&self, done: u64, last: bool) {
        let el = self.start.elapsed().as_secs_f64();
        let line = if self.total > 0 {
            let frac = (done as f64 / self.total as f64).min(1.0);
            let w = 28usize;
            let filled = (frac * w as f64).round() as usize;
            format!(
                "{}: [{}{}] {:>3.0}%  {:.0}/{:.0} MB  {:.0}s",
                self.label,
                "#".repeat(filled),
                "-".repeat(w - filled),
                frac * 100.0,
                done as f64 / 1.0e6,
                self.total as f64 / 1.0e6,
                el,
            )
        } else {
            format!(
                "{}: {:.1}M done  {:.0}k/s  {:.0}s",
                self.label,
                done as f64 / 1.0e6,
                (done as f64 / el.max(1e-3)) / 1.0e3,
                el,
            )
        };
        // Under a stage spinner the line belongs to the stage: publish the text
        // there rather than writing (and fighting over `\r`).
        if stage_active() {
            set_detail(&line);
            return;
        }
        let line = decorate(&line);
        let mut err = std::io::stderr().lock();
        if self.tty {
            let _ = write!(err, "\r{line}\x1b[K");
            if last {
                let _ = writeln!(err);
            }
        } else {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
    }
}

/// A `Read` wrapper that drives a byte-progress bar as bytes are pulled through
/// it — used to show progress of the (in-memory) RDF/XML parse.
pub struct ProgressReader<R> {
    inner: R,
    bar: Progress,
    done: u64,
}

impl<R: Read> ProgressReader<R> {
    pub fn new(inner: R, total: u64, label: &'static str) -> Self {
        ProgressReader {
            inner,
            bar: Progress::new(label, total),
            done: 0,
        }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 {
            self.bar.finish(self.done);
        } else {
            self.done += n as u64;
            self.bar.set(self.done);
        }
        Ok(n)
    }
}

/// A `Write` wrapper that drives a byte heartbeat (MB written + rate) as bytes
/// flow through it — for the otherwise-silent serialization of large outputs.
pub struct ProgressWriter<W> {
    inner: W,
    bar: Progress,
    label: String,
    start: Instant,
    done: u64,
    next: u64,
}

impl<W: Write> ProgressWriter<W> {
    pub fn new(inner: W, label: impl Into<String>) -> Self {
        ProgressWriter {
            inner,
            // The bar's own `&'static` label is unused here — every line is
            // rendered through `bar.line()` with the owned `self.label`.
            bar: Progress::new("write", 0),
            label: label.into(),
            start: Instant::now(),
            done: 0,
            next: 0,
        }
    }
    pub fn finish(mut self) -> std::io::Result<()> {
        self.inner.flush()?;
        let el = self.start.elapsed().as_secs_f64();
        self.bar
            .finish_line(&format!("{}: {:.0} MB in {:.0}s", self.label, self.done as f64 / 1.0e6, el));
        Ok(())
    }
}

impl<W: Write> Write for ProgressWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.done += n as u64;
        // Only touch the clock / render every 8 MB, so per-write overhead stays
        // negligible even when the serializer emits tiny per-triple writes.
        if self.done >= self.next {
            self.next = self.done + 8 * 1024 * 1024;
            let el = self.start.elapsed().as_secs_f64();
            self.bar.line(&format!(
                "{}: {:.0} MB  {:.0} MB/s  {:.0}s",
                self.label,
                self.done as f64 / 1.0e6,
                (self.done as f64 / 1.0e6) / el.max(1e-3),
                el,
            ));
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
