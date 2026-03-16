use std::fmt::Display;
use std::future::Future;
use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use console::Term;
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{self, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

static VERBOSE: AtomicBool = AtomicBool::new(false);
static CI: AtomicBool = AtomicBool::new(false);

// Brand palette
#[allow(dead_code)]
const BRAND_TEAL: (u8, u8, u8) = (155, 196, 182); // #9BC4B6
#[allow(dead_code)]
const BRAND_CORAL: (u8, u8, u8) = (232, 135, 131); // #E88783
const BRAND_GREEN: (u8, u8, u8) = (155, 217, 179); // #9BD9B3 — success
const BRAND_AMBER: (u8, u8, u8) = (234, 211, 156); // #EAD39C — warning
const BRAND_RED: (u8, u8, u8) = (232, 163, 160); // #E8A3A0 — error

// Terminal accent colors (distinct from brand palette)
const ACCENT: (u8, u8, u8) = (125, 196, 228); // #7DC4E4
const ACCENT_DIM: (u8, u8, u8) = (79, 107, 122); // #4F6B7A

fn should_colorize() -> bool {
    if cfg!(test) {
        return false;
    }
    !is_ci() && std::io::stderr().is_terminal()
}

fn rgb_fg<D: Display>(value: D, (r, g, b): (u8, u8, u8)) -> String {
    if should_colorize() {
        format!("\x1b[38;2;{r};{g};{b}m{value}\x1b[39m")
    } else {
        value.to_string()
    }
}

pub fn brand_accent<D: Display>(value: D) -> String {
    rgb_fg(value, ACCENT)
}

pub fn brand_secondary<D: Display>(value: D) -> String {
    rgb_fg(value, ACCENT)
}

pub fn brand_fg<D: Display>(value: D) -> String {
    value.to_string()
}

pub fn brand_muted<D: Display>(value: D) -> String {
    if should_colorize() {
        // Re-apply dim after any embedded bold-reset (\x1b[22m) so that
        // strong()/bold() calls inside a muted() context don't cancel
        // the dim styling for the surrounding text.
        let s = value.to_string().replace("\x1b[22m", "\x1b[22m\x1b[2m");
        format!("\x1b[2m{s}\x1b[22m")
    } else {
        value.to_string()
    }
}

pub fn brand_dim<D: Display>(value: D) -> String {
    if should_colorize() {
        format!("\x1b[38;2;100;100;100m{value}\x1b[39m")
    } else {
        value.to_string()
    }
}

pub fn brand_success<D: Display>(value: D) -> String {
    rgb_fg(value, BRAND_GREEN)
}

pub fn brand_warning<D: Display>(value: D) -> String {
    rgb_fg(value, BRAND_AMBER)
}

pub fn brand_error<D: Display>(value: D) -> String {
    rgb_fg(value, BRAND_RED)
}

fn bold(value: &str) -> String {
    if should_colorize() {
        format!("\x1b[1m{value}\x1b[22m")
    } else {
        value.to_string()
    }
}

pub fn underline<D: Display>(value: D) -> String {
    if should_colorize() {
        format!("\x1b[4m{value}\x1b[24m")
    } else {
        value.to_string()
    }
}

pub fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs < 0.1 {
        String::new()
    } else if secs < 10.0 {
        format!("({:.1}s)", secs)
    } else if secs < 60.0 {
        format!("({}s)", secs as u64)
    } else {
        let mins = secs as u64 / 60;
        let remaining = secs as u64 % 60;
        format!("({}m{}s)", mins, remaining)
    }
}

/// Format elapsed for TRACE log lines. Always shows a value (even sub-100ms).
/// Uses human-friendly units: `(3ms)`, `(1.2s)`, `(5s)`, `(1m10s)`.
pub fn format_elapsed_trace(duration: Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1000 {
        format!("({ms}ms)")
    } else {
        let secs = duration.as_secs_f64();
        if secs < 10.0 {
            format!("({:.1}s)", secs)
        } else if secs < 60.0 {
            format!("({}s)", secs as u64)
        } else {
            let mins = secs as u64 / 60;
            let remaining = secs as u64 % 60;
            format!("({}m{}s)", mins, remaining)
        }
    }
}

/// Format elapsed for inline spinner display (no parens), e.g. `"1m10s"`.
fn format_elapsed_inline(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("({secs}s)")
    } else {
        let mins = secs / 60;
        let remaining = secs % 60;
        format!("({mins}m{remaining}s)")
    }
}

/// Format a muted elapsed-time string, e.g. `"(3.2s)"` rendered in muted style.
/// Returns empty string if duration is below display threshold.
pub fn muted_elapsed(duration: Duration) -> String {
    let s = format_elapsed(duration);
    if s.is_empty() { s } else { brand_muted(&s) }
}

/// Format a muted progress counter, e.g. `"[2/5]"` rendered in muted style.
pub fn muted_progress(done: usize, total: usize) -> String {
    brand_muted(format!("[{done}/{total}]"))
}

/// Format a byte count as a human-readable size string.
///
/// Examples: `"999 bytes"`, `"1.00 KB"`, `"4.56 MB"`, `"1.23 GB"`.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

pub fn set_ci(ci: bool) {
    CI.store(ci, Ordering::Relaxed);
}

pub fn is_interactive() -> bool {
    #[cfg(test)]
    {
        false
    }

    #[cfg(not(test))]
    {
        !is_ci() && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn is_ci() -> bool {
    CI.load(Ordering::Relaxed)
}

/// True when pretty output should render (normal interactive mode).
/// False in verbose or CI mode, where tracing handles all output.
pub fn is_pretty() -> bool {
    !is_verbose() && !is_ci()
}

/// Start a timed span for verbose logging. Returns a guard that logs elapsed
/// time at TRACE level when dropped or explicitly finished.
pub fn timed(label: &str) -> TimedSpan {
    TimedSpan::new(label)
}

pub struct TimedSpan {
    label: String,
    start: std::time::Instant,
}

impl TimedSpan {
    fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            start: std::time::Instant::now(),
        }
    }
}

impl Drop for TimedSpan {
    fn drop(&mut self) {
        let time = format_elapsed_trace(self.start.elapsed());
        tracing::trace!("{} {time}", self.label);
    }
}

// ── Tracing scope support ───────────────────────────────────────────────────

/// Custom timer that formats as local HH:MM:SS.mmm.
pub struct LocalTimer;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        #[cfg(unix)]
        {
            let dur = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let epoch = dur.as_secs() as libc::time_t;
            let millis = dur.subsec_millis();
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&epoch, &mut tm) };
            write!(
                w,
                "{:02}:{:02}:{:02}.{:03}",
                tm.tm_hour, tm.tm_min, tm.tm_sec, millis
            )
        }
        #[cfg(not(unix))]
        {
            let dur = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let total_secs = dur.as_secs();
            let millis = dur.subsec_millis();
            let hours = (total_secs % 86400) / 3600;
            let minutes = (total_secs % 3600) / 60;
            let seconds = total_secs % 60;
            write!(
                w,
                "{:02}:{:02}:{:02}.{:03}",
                hours, minutes, seconds, millis
            )
        }
    }
}

/// Data stored in span extensions to carry the scope label.
struct SpanScope(String);

/// Visitor that extracts the `scope` field from span attributes.
struct ScopeVisitor(Option<String>);

impl tracing::field::Visit for ScopeVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "scope" {
            self.0 = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "scope" {
            self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

/// Layer that captures `scope` fields from spans into extensions.
pub struct ScopeLayer;

impl<S> tracing_subscriber::Layer<S> for ScopeLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = ScopeVisitor(None);
        attrs.record(&mut visitor);
        if let Some(scope) = visitor.0 {
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(SpanScope(scope));
            }
        }
    }
}

/// Custom event format: `HH:MM:SS.mmm LEVEL [scope] message`
/// In CI mode: no timestamp (CI adds its own), no ANSI colors.
pub struct ScopeFormat;

impl<S, N> FormatEvent<S, N> for ScopeFormat
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        use tracing_subscriber::fmt::time::FormatTime;

        let ci = is_ci();

        // Timestamp (skipped in CI — CI systems add their own)
        if !ci {
            LocalTimer.format_time(&mut writer)?;
        }

        // Level (right-aligned, 5 chars)
        let level = *event.metadata().level();
        if should_colorize() {
            let color = match level {
                tracing::Level::ERROR => "\x1b[31m",
                tracing::Level::WARN => "\x1b[33m",
                tracing::Level::INFO => "\x1b[32m",
                tracing::Level::DEBUG => "\x1b[34m",
                tracing::Level::TRACE => "\x1b[35m",
            };
            write!(writer, " {color}{level:>5}\x1b[0m ")?;
        } else {
            write!(writer, " {level:>5} ")?;
        }

        // Scope from innermost span (leaf → root, take first match)
        if let Some(scope) = ctx.event_scope() {
            for span in scope {
                if let Some(data) = span.extensions().get::<SpanScope>() {
                    write!(writer, "[{}] ", data.0)?;
                    break;
                }
            }
        }

        // Message
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Create a tracing span that provides `[name]` scope prefix in verbose output.
pub fn scope(name: &str) -> tracing::Span {
    tracing::info_span!("", scope = %name)
}

pub fn section(title: &str) {
    if is_pretty() {
        eprintln!();
        eprintln!("{}", bold(&brand_accent(title)));
    }
}

pub fn heading(title: &str) {
    if is_pretty() {
        eprintln!();
        eprintln!("{}", bold(title));
    }
}

pub fn heading_no_gap(title: &str) {
    if is_pretty() {
        eprintln!("{}", bold(title));
    }
}

pub fn info(message: &str) {
    if is_pretty() {
        eprintln!("{}", brand_fg(message));
    }
}

pub fn bullet(message: &str) {
    if is_pretty() {
        eprintln!("  {} {}", bold(&brand_secondary("•")), brand_fg(message));
    }
}

fn format_warning_full_line(message: &str) -> String {
    format!(
        "{} {}",
        brand_warning(brand_muted("┃")),
        brand_warning(message)
    )
}

fn format_warning_bullet_line(message: &str) -> String {
    format!(
        "{} {}",
        brand_warning(brand_muted("┃")),
        brand_warning(format!("• {message}"))
    )
}

pub fn success(message: &str) {
    if is_pretty() {
        eprintln!("{} {}", brand_success("✓"), brand_fg(message));
    }
}

pub fn warning(message: &str) {
    if is_pretty() {
        eprintln!("{} {}", bold(&brand_warning("!")), brand_fg(message));
    }
}

pub fn warning_full(message: &str) {
    if is_pretty() {
        eprintln!("{}", format_warning_full_line(message));
    }
}

pub fn warning_bullet(message: &str) {
    if is_pretty() {
        eprintln!("{}", format_warning_bullet_line(message));
    }
}

pub fn error(message: &str) {
    if is_pretty() {
        eprintln!("{} {}", bold(&brand_error("✗")), brand_fg(message));
    }
}

/// Always prints — used for fatal errors in main.rs.
pub fn error_stderr(message: &str) {
    eprintln!("{} {}", bold(&brand_error("✗")), brand_fg(message));
}

pub fn muted(message: &str) {
    if is_pretty() {
        eprintln!("{}", brand_muted(message));
    }
}

/// Print a hint line in default text color (not muted).
/// Use for actionable guidance like "Run X to do Y" where the command is strong()'d.
pub fn hint(message: &str) {
    if is_pretty() {
        eprintln!("{}", brand_dim(message));
    } else {
        tracing::info!("{}", message);
    }
}

/// Print a server heading: `Server {name}` with the name in strong (bold).
/// Indentation prefix for lines under a heading (2 spaces).
pub const INDENT: &str = "  ";

/// Bold only (no color). The one thing you want the eye to catch.
pub fn strong(value: &str) -> String {
    bold(value)
}

/// Accent color only (no bold). Secondary emphasis.
pub fn accent(value: &str) -> String {
    brand_accent(value)
}

/// A block of context lines with a left border, printed together.
///
/// ```text
/// ┃ Using production environment
/// ┃ You're on canary channel
/// ```
///
/// Stores raw data; formatting is applied at print time per output mode.
pub struct ContextBlock {
    entries: Vec<ContextEntry>,
}

enum ContextEntry {
    Env(String),
    Channel(String),
}

impl ContextBlock {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add "Using {value} environment".
    pub fn env(mut self, env: &str) -> Self {
        self.entries.push(ContextEntry::Env(env.to_string()));
        self
    }

    /// Add "You're on {value} channel".
    pub fn channel(mut self, channel: &str) -> Self {
        self.entries
            .push(ContextEntry::Channel(channel.to_string()));
        self
    }

    /// Print the block (with trailing blank line). No-op if empty.
    pub fn print(self) {
        if self.entries.is_empty() {
            return;
        }
        if is_pretty() {
            let border = rgb_fg("┃", ACCENT_DIM);
            for entry in &self.entries {
                let line = match entry {
                    ContextEntry::Env(v) => format!("Using {} environment", accent(v)),
                    ContextEntry::Channel(v) => format!("You're on {} channel", accent(v)),
                };
                eprintln!("{border} {line}");
            }
            eprintln!();
        } else {
            for entry in &self.entries {
                let line = match entry {
                    ContextEntry::Env(v) => format!("Using {v} environment"),
                    ContextEntry::Channel(v) => format!("You're on {v} channel"),
                };
                tracing::info!("{}", line);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Spinner helpers
// ---------------------------------------------------------------------------

pub const SPINNER_TICKS: &[&str] = &["⣼", "⣹", "⢻", "⠿", "⡟", "⣏", "⣧", "⣶", " "];

fn teal_spinner_token() -> String {
    if should_colorize() {
        let (r, g, b) = ACCENT;
        format!("\x1b[38;2;{r};{g};{b}m{{spinner}}\x1b[39m")
    } else {
        "{spinner}".to_string()
    }
}

pub fn spinner_style() -> ProgressStyle {
    let s = teal_spinner_token();
    ProgressStyle::with_template(&format!("{s} {{msg}}"))
        .unwrap()
        .tick_strings(SPINNER_TICKS)
}

pub fn phase_spinner_style() -> ProgressStyle {
    let s = teal_spinner_token();
    ProgressStyle::with_template(&format!("{s} {{msg}}"))
        .unwrap()
        .tick_strings(SPINNER_TICKS)
}

fn phase_spinner_style_indented() -> ProgressStyle {
    let s = teal_spinner_token();
    ProgressStyle::with_template(&format!("{INDENT}{s} {{msg}}"))
        .unwrap()
        .tick_strings(SPINNER_TICKS)
}

/// Print a spinner result without elapsed (fast path — spinner was never shown).
fn print_ok(success_msg: &str) {
    if is_pretty() {
        let check = brand_success("✓");
        eprintln!("{check} {}", brand_fg(success_msg));
    } else {
        tracing::info!("{}", success_msg);
    }
}

fn print_err(loading: &str) {
    if is_pretty() {
        let x = bold(&brand_error("✗"));
        eprintln!("{x} {loading}");
    } else {
        tracing::error!("{}", loading);
    }
}

fn print_err_with_detail(loading: &str, detail: &dyn Display) {
    if is_pretty() {
        let x = bold(&brand_error("✗"));
        eprintln!("{x} {loading}: {detail}");
    } else {
        tracing::error!("{}: {}", loading, detail);
    }
}

/// Hide cursor and suppress keyboard echo while keeping signal handling
/// (Ctrl+C etc.) intact. Call `show_cursor()` to restore.
pub fn hide_cursor() {
    suppress_echo(true);
    let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Hide);
}

/// Show cursor and restore normal terminal input.
pub fn show_cursor() {
    let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Show);
    suppress_echo(false);
}

/// Toggle the terminal ECHO flag without touching ISIG, so Ctrl+C still
/// generates SIGINT.
fn suppress_echo(suppress: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = std::io::stdin().as_raw_fd();
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut termios) != 0 {
                return;
            }
            if suppress {
                termios.c_lflag &= !(libc::ECHO | libc::ICANON);
            } else {
                termios.c_lflag |= libc::ECHO | libc::ICANON;
            }
            let _ = libc::tcsetattr(fd, libc::TCSANOW, &termios);
        }
    }
}

fn finish_spinner_ok(pb: &ProgressBar, success_msg: &str, elapsed: Duration) {
    pb.finish_and_clear();
    show_cursor();
    let check = brand_success("✓");
    let time = muted_elapsed(elapsed);
    if time.is_empty() {
        eprintln!("{check} {}", brand_fg(success_msg));
    } else {
        eprintln!("{check} {} {time}", brand_fg(success_msg));
    }
}

fn finish_spinner_err(pb: &ProgressBar, loading: &str) {
    pb.finish_and_clear();
    show_cursor();
    let x = bold(&brand_error("✗"));
    eprintln!("{x} {loading}");
}

fn finish_spinner_err_with_detail(pb: &ProgressBar, loading: &str, detail: &dyn Display) {
    pb.finish_and_clear();
    show_cursor();
    let x = bold(&brand_error("✗"));
    eprintln!("{x} {loading}: {detail}");
}

/// Spinner that shows only if work takes >= 1s, then clears on completion.
///
/// - Fast (<1s):  prints result directly, no spinner, no elapsed
/// - Slow (≥1s):  `⠋ {loading}...` → `{success} (elapsed)` or `✗ {loading} failed`
///
/// In verbose mode: prints start/end log lines instead of spinner.
pub fn with_spinner<T, E, F>(loading: &str, success: &str, work: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
{
    // Verbose/CI mode: tracing for start/completion.
    if !is_pretty() {
        tracing::info!("{}", loading);
        let start = Instant::now();
        let result = work();
        let elapsed = start.elapsed();
        match &result {
            Ok(_) => {
                let time = format_elapsed(elapsed);
                if time.is_empty() {
                    tracing::info!("{}", success);
                } else {
                    tracing::info!("{} {}", success, time);
                }
            }
            Err(_) => tracing::error!("{}", loading),
        }
        return result;
    }

    if !is_interactive() {
        return work();
    }

    let start = Instant::now();
    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());

    // Enable spinner after 1s if work is still running.
    let pb_clone = pb.clone();
    let loading_str = loading.to_string();
    let spinner_shown = Arc::new(AtomicBool::new(false));
    let shown_clone = spinner_shown.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        if !pb_clone.is_finished() {
            shown_clone.store(true, Ordering::Relaxed);
            hide_cursor();
            pb_clone.set_message(format!("{loading_str}..."));
            pb_clone.enable_steady_tick(Duration::from_millis(80));
        }
    });

    let result = work();
    let elapsed = start.elapsed();

    if spinner_shown.load(Ordering::Relaxed) {
        match &result {
            Ok(_) => finish_spinner_ok(&pb, success, elapsed),
            Err(_) => finish_spinner_err(&pb, loading),
        }
    } else {
        pb.finish_and_clear();
        match &result {
            Ok(_) => print_ok(success),
            Err(_) => print_err(loading),
        }
    }

    result
}

/// Async spinner that shows only if work takes >= 1s, then clears on completion.
pub async fn with_spinner_async<T, E: Display, Fut>(
    loading: &str,
    success: &str,
    work: Fut,
) -> Result<T, E>
where
    Fut: Future<Output = Result<T, E>>,
{
    with_spinner_async_err(loading, success, loading, work).await
}

pub async fn with_spinner_async_err<T, E: Display, Fut>(
    loading: &str,
    success: &str,
    error_label: &str,
    work: Fut,
) -> Result<T, E>
where
    Fut: Future<Output = Result<T, E>>,
{
    // Verbose/CI mode: tracing for start/completion.
    if !is_pretty() {
        tracing::info!("{}", loading);
        let start = Instant::now();
        let result = work.await;
        let elapsed = start.elapsed();
        match &result {
            Ok(_) => {
                let time = format_elapsed(elapsed);
                if time.is_empty() {
                    tracing::info!("{}", success);
                } else {
                    tracing::info!("{} {}", success, time);
                }
            }
            Err(e) => tracing::error!("{}: {}", error_label, e),
        }
        return result;
    }

    if !is_interactive() {
        return work.await;
    }

    let start = Instant::now();
    let mut work = std::pin::pin!(work);

    // Fast path: complete within 1s — no spinner needed.
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(1), work.as_mut()).await {
        match &result {
            Ok(_) => print_ok(success),
            Err(e) => print_err_with_detail(error_label, e),
        }
        return result;
    }

    // Slow path: show spinner for the remainder.
    hide_cursor();
    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());
    pb.set_message(format!("{loading}..."));
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = work.await;

    match &result {
        Ok(_) => finish_spinner_ok(&pb, success, start.elapsed()),
        Err(e) => finish_spinner_err_with_detail(&pb, error_label, e),
    }

    result
}

/// Simple spinner — shows only if work takes >= 1s, then clears. No result line.
/// In verbose/CI mode: prints a tracing line for the action.
pub fn with_spinner_simple<T, F>(message: &str, work: F) -> T
where
    F: FnOnce() -> T,
{
    if !is_pretty() {
        tracing::info!("{}", message);
        return work();
    }

    if !is_interactive() {
        return work();
    }

    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());

    let pb_clone = pb.clone();
    let msg = message.to_string();
    let spinner_shown = Arc::new(AtomicBool::new(false));
    let shown_clone = spinner_shown.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        if !pb_clone.is_finished() {
            shown_clone.store(true, Ordering::Relaxed);
            hide_cursor();
            pb_clone.set_message(format!("{msg}..."));
            pb_clone.enable_steady_tick(Duration::from_millis(80));
        }
    });

    let result = work();
    pb.finish_and_clear();
    if spinner_shown.load(Ordering::Relaxed) {
        show_cursor();
    }
    result
}

/// Async simple spinner — shows only if work takes >= 1s, then clears. No result line.
/// In verbose/CI mode: prints a tracing line for the action.
pub async fn with_spinner_async_simple<T, Fut>(message: &str, work: Fut) -> T
where
    Fut: Future<Output = T>,
{
    if !is_pretty() {
        tracing::info!("{}", message);
        return work.await;
    }

    if !is_interactive() {
        return work.await;
    }

    let mut work = std::pin::pin!(work);

    // Fast path: no spinner needed.
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(1), work.as_mut()).await {
        return result;
    }

    // Slow path: show spinner.
    hide_cursor();
    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());
    pb.set_message(format!("{message}..."));
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = work.await;
    pb.finish_and_clear();
    show_cursor();
    result
}

/// A spinner for major phases (Build, Deploy). Shows elapsed time after 1s.
/// Inner output is NOT suppressed — it flows normally above the spinner.
///
/// In verbose mode: no spinner animation, just INFO log lines.
pub struct PhaseSpinner {
    pb: Option<ProgressBar>,
    start: Instant,
    finished: bool,
    verbose: bool,
    _elapsed_task: Option<tokio::task::JoinHandle<()>>,
}

impl PhaseSpinner {
    pub fn start(message: &str) -> Self {
        Self::new(message, false)
    }

    /// Start an indented phase spinner (prefixed with INDENT).
    pub fn start_indented(message: &str) -> Self {
        Self::new(message, true)
    }

    fn new(message: &str, indented: bool) -> Self {
        let verbose = !is_pretty();

        if verbose {
            tracing::info!("{}", message);
            return Self {
                pb: None,
                start: Instant::now(),
                finished: false,
                verbose: true,
                _elapsed_task: None,
            };
        }

        let style = if indented {
            phase_spinner_style_indented()
        } else {
            phase_spinner_style()
        };
        let pb = if is_interactive() {
            let pb = ProgressBar::new_spinner();
            pb.set_style(style);
            pb.set_message(message.to_string());
            pb.enable_steady_tick(Duration::from_millis(80));
            hide_cursor();
            Some(pb)
        } else {
            None
        };

        // Spawn a task that updates the message with elapsed time every second.
        let elapsed_task = pb.as_ref().map(|pb| {
            let pb = pb.clone();
            let base = message.to_string();
            let start = Instant::now();
            tokio::spawn(async move {
                // Wait 1s before showing elapsed at all.
                tokio::time::sleep(Duration::from_secs(1)).await;
                loop {
                    let elapsed = start.elapsed();
                    let time = format_elapsed_inline(elapsed);
                    pb.set_message(format!("{base} {}", brand_muted(&time)));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            })
        });

        Self {
            pb,
            start: Instant::now(),
            finished: false,
            verbose: false,
            _elapsed_task: elapsed_task,
        }
    }

    pub fn pb(&self) -> Option<&ProgressBar> {
        self.pb.as_ref()
    }

    pub fn finish(mut self, success_msg: &str) {
        self.abort_elapsed_task();
        if self.verbose {
            // In verbose mode the start message already persists — no result line needed.
        } else {
            if let Some(ref pb) = self.pb {
                finish_spinner_ok(pb, success_msg, self.start.elapsed());
            }
        }
        self.finished = true;
    }

    pub fn finish_err(mut self, loading: &str, detail: &str) {
        self.abort_elapsed_task();
        if self.verbose {
            tracing::error!("{}: {}", loading, detail);
        } else {
            if let Some(ref pb) = self.pb {
                finish_spinner_err_with_detail(pb, loading, &detail);
            }
        }
        self.finished = true;
    }

    /// Finish indented spinner with success: `  ✓ message (elapsed)`
    pub fn finish_ok_indented(mut self, success_msg: &str) {
        self.abort_elapsed_task();
        if self.verbose {
            // In verbose mode the start message already persists — no result line needed.
        } else if let Some(ref pb) = self.pb {
            pb.finish_and_clear();
            show_cursor();
            let check = brand_success("✓");
            let time = muted_elapsed(self.start.elapsed());
            if time.is_empty() {
                eprintln!("{INDENT}{check} {}", brand_fg(success_msg));
            } else {
                eprintln!("{INDENT}{check} {} {time}", brand_fg(success_msg));
            }
        }
        self.finished = true;
    }

    /// Finish indented spinner with error: `  ✗ message`
    pub fn finish_err_indented(mut self, detail: &str) {
        self.abort_elapsed_task();
        if self.verbose {
            tracing::error!("{}", detail);
        } else if let Some(ref pb) = self.pb {
            pb.finish_and_clear();
            show_cursor();
            let x = bold(&brand_error("✗"));
            eprintln!("{INDENT}{x} {}", brand_error(detail));
        }
        self.finished = true;
    }

    fn abort_elapsed_task(&mut self) {
        if let Some(handle) = self._elapsed_task.take() {
            handle.abort();
        }
    }
}

impl Drop for PhaseSpinner {
    fn drop(&mut self) {
        self.abort_elapsed_task();
        if !self.finished {
            if let Some(ref pb) = self.pb {
                pb.finish_and_clear();
                show_cursor();
            }
        }
    }
}

/// A spinner whose message can be updated while running.
/// Does NOT suppress other output (unlike PhaseSpinner).
///
/// In verbose/CI mode: logs the initial message only; `set_message()` updates
/// are a no-op (progress counts are normal-mode only). Per-scope completion
/// should be logged by the calling code via `tracing::debug!()`.
pub struct TrackedSpinner {
    pb: Option<ProgressBar>,
}

impl TrackedSpinner {
    pub fn start(message: &str) -> Self {
        if !is_pretty() {
            tracing::info!("{}", message);
            return Self { pb: None };
        }
        let pb = if is_interactive() {
            let pb = ProgressBar::new_spinner();
            pb.set_style(spinner_style());
            pb.set_message(message.to_string());
            pb.enable_steady_tick(Duration::from_millis(80));
            hide_cursor();
            Some(pb)
        } else {
            None
        };
        Self { pb }
    }

    pub fn set_message(&self, message: &str) {
        if let Some(ref pb) = self.pb {
            pb.set_message(message.to_string());
        }
    }

    pub fn finish(&self) {
        if let Some(ref pb) = self.pb {
            pb.finish_and_clear();
            show_cursor();
        }
    }
}

impl Drop for TrackedSpinner {
    fn drop(&mut self) {
        if let Some(ref pb) = self.pb {
            pb.finish_and_clear();
            show_cursor();
        }
    }
}

// ---------------------------------------------------------------------------
// Transfer progress — two-line download/upload bar with gradient
// ---------------------------------------------------------------------------

/// Bar width in characters.
const BAR_WIDTH: usize = 24;

/// Two-line transfer progress:
///
/// ```text
/// ⣼ Downloading… 1.23 MB / 4.56 MB (3s)
///   ████████████·············
/// ```
///
/// On completion the spinner line becomes a checkbox and the bar stays filled:
///
/// ```text
/// ✓ Download complete (3.2s)
///   ████████████████████████
/// ```
pub struct TransferProgress {
    pb: Option<ProgressBar>,
    start: Instant,
    loading_label: String,
    success_msg: String,
    total: u64,
    finished: std::sync::atomic::AtomicBool,
}

impl TransferProgress {
    /// Create a new transfer progress bar.
    ///
    /// - `loading` — verb phrase shown while in progress, e.g. `"Downloading"`
    /// - `success` — message shown on finish, e.g. `"Download complete"`
    /// - `total` — total byte count (0 if unknown)
    pub fn new(loading: &str, success: &str, total: u64) -> Self {
        let start = Instant::now();
        let label = format!("{loading}…");
        let pb = if is_pretty() && is_interactive() {
            let pb = ProgressBar::new_spinner();
            pb.set_style(spinner_style());
            pb.set_message(format!("{label}\n{INDENT}{}", render_gradient_bar(0.0)));
            pb.enable_steady_tick(Duration::from_millis(80));
            hide_cursor();
            Some(pb)
        } else {
            tracing::info!("{loading}");
            None
        };
        Self {
            pb,
            start,
            loading_label: label,
            success_msg: success.to_string(),
            total,
            finished: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Update bytes transferred. Call this from the transfer loop.
    pub fn set_position(&self, bytes: u64) {
        if let Some(ref pb) = self.pb {
            let fraction = if self.total > 0 {
                (bytes as f64 / self.total as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let elapsed = self.start.elapsed();
            let time = format_elapsed_inline(elapsed);
            let size_text = if self.total > 0 {
                format!("{} / {}", format_size(bytes), format_size(self.total))
            } else {
                format_size(bytes)
            };
            let bar = render_gradient_bar(fraction);
            pb.set_message(format!(
                "{} {}\n{INDENT}{} {}",
                self.loading_label,
                brand_muted(&time),
                bar,
                brand_muted(&size_text),
            ));
        }
    }

    /// Finish with success — shows `✓ <success_msg> (<size>, <time>)`.
    ///
    /// In pretty mode the progress bar is cleared so only the single summary
    /// line remains in scrollback.
    pub fn finish(&self) {
        if self.finished.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if let Some(ref pb) = self.pb {
            pb.finish_and_clear();
            show_cursor();
            let check = brand_success("✓");
            let elapsed = self.start.elapsed();
            let mut details = Vec::new();
            if self.total > 0 {
                details.push(format_size(self.total));
            }
            let time = format_elapsed_inline(elapsed);
            if !time.is_empty() {
                details.push(time);
            }
            if details.is_empty() {
                eprintln!("{check} {}", brand_fg(&self.success_msg));
            } else {
                eprintln!(
                    "{check} {} {}",
                    brand_fg(&self.success_msg),
                    brand_muted(&format!("({})", details.join(", ")))
                );
            }
        } else {
            tracing::info!("{}", &self.success_msg);
        }
    }
}

impl Drop for TransferProgress {
    fn drop(&mut self) {
        if !*self.finished.get_mut() {
            if let Some(ref pb) = self.pb {
                pb.finish_and_clear();
                show_cursor();
            }
        }
    }
}

/// Render a progress bar string.
///
/// Filled portion uses solid accent color (`█`); unfilled uses dim braille dots (`⣀`).
fn render_gradient_bar(fraction: f64) -> String {
    let f = fraction.clamp(0.0, 1.0);
    let filled = (f * BAR_WIDTH as f64).round() as usize;
    let empty = BAR_WIDTH.saturating_sub(filled);

    let mut buf = String::with_capacity(BAR_WIDTH * 20);
    let colorize = should_colorize();

    // Filled blocks in accent color
    if filled > 0 {
        if colorize {
            let (r, g, b) = ACCENT;
            buf.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
        }
        for _ in 0..filled {
            buf.push('█');
        }
    }

    // Unfilled: dim braille dot pattern
    if empty > 0 {
        if colorize {
            buf.push_str("\x1b[2m");
        }
        for _ in 0..empty {
            buf.push('⣿');
        }
    }

    if colorize {
        buf.push_str("\x1b[0m");
    }
    buf
}

// ---------------------------------------------------------------------------
// Prompts — wizards vanish after the user answers
// ---------------------------------------------------------------------------

/// Check if an error signals "go back" (ESC pressed in a wizard prompt).
pub fn is_wizard_back(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::Interrupted && err.to_string() == "wizard_back"
}

fn wizard_back_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "wizard_back")
}

pub fn confirm(prompt: &str, default: bool) -> std::io::Result<bool> {
    confirm_with_description(prompt, None, default)
}

pub fn confirm_with_description(
    prompt: &str,
    description: Option<&str>,
    default: bool,
) -> std::io::Result<bool> {
    if !is_interactive() {
        return Ok(default);
    }

    // Verbose mode: transcript-style confirm (still interactive, no screen erasing).
    // Prompts are NOT wrapped in tracing log lines — they print as plain text.
    if !is_pretty() {
        use crossterm::{
            event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
            terminal,
        };
        let hint = if default { "[Y/n]" } else { "[y/N]" };
        eprint!("{} {} ", brand_accent(prompt), brand_muted(hint));
        let _ = std::io::Write::flush(&mut std::io::stderr());
        terminal::enable_raw_mode()?;
        let result = loop {
            if let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = event::read()?
            {
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        terminal::disable_raw_mode()?;
                        eprintln!("yes");
                        break Ok(true);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        terminal::disable_raw_mode()?;
                        eprintln!("no");
                        break Ok(false);
                    }
                    KeyCode::Enter => {
                        terminal::disable_raw_mode()?;
                        eprintln!("{}", if default { "yes" } else { "no" });
                        break Ok(default);
                    }
                    KeyCode::Esc => {
                        terminal::disable_raw_mode()?;
                        break Err(wizard_back_error());
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        terminal::disable_raw_mode()?;
                        break Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "Operation interrupted",
                        ));
                    }
                    _ => {}
                }
            }
        };
        return result;
    }

    use crossterm::{
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
        terminal,
    };

    let term = Term::stderr();

    // Print description line first (if any)
    if let Some(desc) = description {
        let _ = term.write_line(desc);
    }

    // Print prompt with accent color while active
    let hint_text = if default { "[Y/n]" } else { "[y/N]" };
    let hint = brand_muted(hint_text);
    let separator = brand_muted("›");
    eprint!("{} {hint} {separator} ", brand_accent(prompt));

    // Raw mode: read single keypress
    terminal::enable_raw_mode()?;
    let result = loop {
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    terminal::disable_raw_mode()?;
                    break Ok(true);
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    terminal::disable_raw_mode()?;
                    break Ok(false);
                }
                KeyCode::Enter => {
                    terminal::disable_raw_mode()?;
                    break Ok(default);
                }
                KeyCode::Esc => {
                    terminal::disable_raw_mode()?;
                    eprintln!();
                    break Err(wizard_back_error());
                }
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    terminal::disable_raw_mode()?;
                    eprintln!();
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "Operation interrupted",
                    ));
                }
                _ => continue,
            }
        }
    };

    // Calculate how many terminal lines the prompt occupied (may wrap)
    let prompt_visual = format!("{prompt} › ");
    let prompt_width = console::measure_text_width(&prompt_visual);
    let term_width = term.size().1.max(1) as usize;
    let prompt_rows = (prompt_width + term_width - 1) / term_width;
    let mut total_rows = prompt_rows;
    if description.is_some() {
        total_rows += 1;
    }

    // Move to next line so cursor is at a known position, then clear
    eprintln!();
    let _ = term.clear_last_lines(total_rows);
    if let Ok(answer) = &result {
        let answer_text = if *answer { "yes" } else { "no" };
        let _ = term.write_line(&format!("{prompt} {separator} {answer_text}"));
    }

    result
}

pub fn password_field(prompt: &str) -> std::io::Result<String> {
    TextField::new(prompt).password().prompt()
}

pub fn text_field(prompt: &str, default: Option<&str>) -> std::io::Result<String> {
    TextField::new(prompt).default_opt(default).prompt()
}

#[derive(Clone)]
pub struct TextField<'a> {
    label: &'a str,
    hint: Option<&'a str>,
    placeholder: Option<&'a str>,
    required: bool,
    trimmed: bool,
    default: Option<&'a str>,
    suggestions: &'a [String],
    password: bool,
}

impl<'a> TextField<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            hint: None,
            placeholder: None,
            required: true,
            trimmed: true,
            default: None,
            suggestions: &[],
            password: false,
        }
    }

    pub fn with_hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }

    /// Dimmed text shown when input is empty. Falls back to first suggestion.
    pub fn with_placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = Some(placeholder);
        self
    }

    /// Allow empty input (Enter with no text). Fields are required by default.
    /// Sets hint to "optional" unless already overridden by `.with_hint()`.
    pub fn optional(mut self) -> Self {
        self.required = false;
        if self.hint.is_none() {
            self.hint = Some("optional");
        }
        self
    }

    pub fn with_default(mut self, default: &'a str) -> Self {
        self.default = Some(default);
        self
    }

    pub fn default_opt(mut self, default: Option<&'a str>) -> Self {
        self.default = default;
        self
    }

    pub fn suggestions(mut self, suggestions: &'a [String]) -> Self {
        self.suggestions = suggestions;
        self
    }

    pub fn password(mut self) -> Self {
        self.password = true;
        self.trimmed = false;
        self
    }

    pub fn prompt(self) -> std::io::Result<String> {
        if !is_interactive() {
            if self.password {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "Password prompt requires an interactive terminal",
                ));
            }
            return match self.default {
                Some(value) => Ok(value.to_string()),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!(
                        "Missing required value: {}. In --ci mode, pass the value via a CLI flag or config.",
                        self.label
                    ),
                )),
            };
        }

        // Verbose mode: same interactive input, no screen erasing after.
        if !is_pretty() {
            let display_label = match self.hint {
                Some(hint) => format!(
                    "{} {}",
                    brand_accent(self.label),
                    brand_muted(&format!("({hint})"))
                ),
                None => brand_accent(self.label),
            };
            let value = raw_text_input(
                &display_label,
                self.default,
                self.suggestions,
                self.password,
                self.placeholder,
                self.required,
                self.trimmed,
            )?;
            return Ok(value);
        }

        let active_label = match self.hint {
            Some(hint) => format!(
                "{} {}",
                brand_accent(self.label),
                brand_muted(&format!("({hint})"))
            ),
            None => brand_accent(self.label),
        };

        let value = raw_text_input(
            &active_label,
            self.default,
            self.suggestions,
            self.password,
            self.placeholder,
            self.required,
            self.trimmed,
        )?;

        // Replace the colored prompt with a plain one
        let term = Term::stderr();
        let _ = term.clear_last_lines(1);
        let separator = brand_muted("›");
        let plain_label = match self.hint {
            Some(hint) => format!("{} {}", self.label, brand_muted(&format!("({hint})"))),
            None => self.label.to_string(),
        };
        if self.password {
            let _ = term.write_line(&format!("{plain_label} {separator} ••••••"));
        } else {
            let _ = term.write_line(&format!("{plain_label} {separator} {value}"));
        }

        Ok(value)
    }
}

/// Custom text input using crossterm. Supports cursor movement, word deletion,
/// tab-completion from suggestions, inline auto-suggest, password masking, and placeholder text.
fn raw_text_input(
    prompt: &str,
    initial: Option<&str>,
    suggestions: &[String],
    password: bool,
    placeholder_override: Option<&str>,
    required: bool,
    trimmed: bool,
) -> std::io::Result<String> {
    use crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
        terminal::{self, Clear, ClearType},
    };
    use std::io::Write;

    let mut out = std::io::stderr();
    let mut buf: Vec<char> = initial.unwrap_or("").chars().collect();
    let mut pos: usize = buf.len(); // cursor position in chars
    let mut suggestion_idx: Option<usize> = None;

    // Placeholder: explicit override > first suggestion > dots for password
    let placeholder: Option<String> = if initial.is_some() {
        None
    } else if password {
        Some("••••••".to_string())
    } else if let Some(ph) = placeholder_override {
        Some(ph.to_string())
    } else {
        suggestions.first().cloned()
    };

    let separator = brand_muted("›");

    // Find the best starts-with match for inline auto-suggest (fish-shell style).
    let inline_suffix = |buf: &[char]| -> String {
        if buf.is_empty() || suggestions.is_empty() || password {
            return String::new();
        }
        let current: String = buf.iter().collect();
        let lower = current.to_lowercase();
        for s in suggestions {
            if s.to_lowercase().starts_with(&lower) && s.len() > current.len() {
                // Use char-based slicing for multi-byte safety
                return s.chars().skip(current.chars().count()).collect();
            }
        }
        String::new()
    };

    let draw = |buf: &[char],
                pos: usize,
                out: &mut std::io::Stderr,
                password: bool,
                placeholder: &Option<String>,
                suffix: &str| {
        let _ = write!(out, "\r");
        let _ = crossterm::execute!(*out, Clear(ClearType::CurrentLine));
        if buf.is_empty() {
            if let Some(ph) = placeholder {
                let dimmed = brand_dim(ph);
                let _ = write!(out, "{prompt} {separator} {dimmed}");
            } else {
                let _ = write!(out, "{prompt} {separator} ");
            }
        } else {
            let display: String = if password {
                "•".repeat(buf.len())
            } else {
                buf.iter().collect()
            };
            let _ = write!(out, "{prompt} {separator} {display}");
            // Show inline suggestion suffix dimmed (only when cursor is at end)
            if !suffix.is_empty() && pos == buf.len() {
                let _ = write!(out, "{}", brand_dim(suffix));
            }
        }
        // Position cursor: prompt + " › " + chars before cursor
        let prompt_width = console::measure_text_width(prompt);
        let sep_width = 3; // " › "
        let cursor_offset = if password {
            pos
        } else {
            buf[..pos].iter().collect::<String>().len()
        };
        let col = prompt_width + sep_width + cursor_offset;
        let _ = crossterm::execute!(*out, cursor::MoveToColumn(col as u16));
        let _ = out.flush();
    };

    // Accept the current inline suggestion into the buffer.
    let accept_inline = |buf: &mut Vec<char>, pos: &mut usize, suggestions: &[String]| -> bool {
        if buf.is_empty() || suggestions.is_empty() {
            return false;
        }
        let current: String = buf.iter().collect();
        let lower = current.to_lowercase();
        if let Some(sugg) = suggestions
            .iter()
            .find(|s| s.to_lowercase().starts_with(&lower) && s.len() > current.len())
        {
            *buf = sugg.chars().collect();
            *pos = buf.len();
            true
        } else {
            false
        }
    };

    // Draw initial state
    terminal::enable_raw_mode()?;

    // Set cursor color to brand teal
    let (cr, cg, cb) = ACCENT;
    let _ = write!(out, "\x1b]12;rgb:{cr:02x}/{cg:02x}/{cb:02x}\x1b\\");
    let _ = out.flush();

    let suf = inline_suffix(&buf);
    draw(&buf, pos, &mut out, password, &placeholder, &suf);

    let result = loop {
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Enter => {
                    let mut result: String = buf.iter().collect();
                    if trimmed {
                        result = result.trim().to_string();
                    }
                    if required && result.is_empty() {
                        continue;
                    }
                    break Ok(result);
                }
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "Operation interrupted",
                    ));
                }
                KeyCode::Esc => {
                    break Err(wizard_back_error());
                }
                // Character input
                KeyCode::Char(c)
                    if !modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    // Reject leading whitespace when trimmed
                    if trimmed
                        && c.is_whitespace()
                        && buf[..pos].iter().all(|ch| ch.is_whitespace())
                    {
                        continue;
                    }
                    buf.insert(pos, c);
                    pos += 1;
                    suggestion_idx = None;
                }
                // Backspace
                KeyCode::Backspace
                    if modifiers.contains(KeyModifiers::SUPER)
                        || modifiers.contains(KeyModifiers::ALT) =>
                {
                    // Word/line delete backward
                    if pos > 0 {
                        let old_pos = pos;
                        while pos > 0 && buf[pos - 1].is_whitespace() {
                            pos -= 1;
                        }
                        while pos > 0 && !buf[pos - 1].is_whitespace() {
                            pos -= 1;
                        }
                        buf.drain(pos..old_pos);
                        suggestion_idx = None;
                    }
                }
                KeyCode::Backspace => {
                    if pos > 0 {
                        pos -= 1;
                        buf.remove(pos);
                        suggestion_idx = None;
                    }
                }
                KeyCode::Delete => {
                    if pos < buf.len() {
                        buf.remove(pos);
                    }
                }
                // Cursor movement
                KeyCode::Left
                    if modifiers.contains(KeyModifiers::SUPER)
                        || modifiers.contains(KeyModifiers::ALT) =>
                {
                    while pos > 0 && buf[pos - 1].is_whitespace() {
                        pos -= 1;
                    }
                    while pos > 0 && !buf[pos - 1].is_whitespace() {
                        pos -= 1;
                    }
                }
                KeyCode::Left => {
                    if pos > 0 {
                        pos -= 1;
                    }
                }
                KeyCode::Right
                    if modifiers.contains(KeyModifiers::SUPER)
                        || modifiers.contains(KeyModifiers::ALT) =>
                {
                    while pos < buf.len() && !buf[pos].is_whitespace() {
                        pos += 1;
                    }
                    while pos < buf.len() && buf[pos].is_whitespace() {
                        pos += 1;
                    }
                }
                KeyCode::Right => {
                    if pos < buf.len() {
                        pos += 1;
                    } else {
                        // At end of buffer: accept inline suggestion
                        accept_inline(&mut buf, &mut pos, suggestions);
                        suggestion_idx = None;
                    }
                }
                KeyCode::Home | KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                    pos = 0;
                }
                KeyCode::End | KeyCode::Char('e') if modifiers.contains(KeyModifiers::CONTROL) => {
                    pos = buf.len();
                }
                // Kill to end of line
                KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => {
                    buf.truncate(pos);
                }
                // Kill to start of line
                KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                    buf.drain(..pos);
                    pos = 0;
                }
                // Tab completion: cycle through matching suggestions
                KeyCode::Tab | KeyCode::BackTab if !suggestions.is_empty() && !password => {
                    let current: String = buf.iter().collect();
                    let needle = current.to_lowercase();
                    let matches: Vec<&String> = suggestions
                        .iter()
                        .filter(|s| needle.is_empty() || s.to_lowercase().contains(&needle))
                        .collect();
                    if !matches.is_empty() {
                        let idx = match suggestion_idx {
                            Some(i) => {
                                if code == KeyCode::BackTab {
                                    if i == 0 { matches.len() - 1 } else { i - 1 }
                                } else {
                                    (i + 1) % matches.len()
                                }
                            }
                            None => 0,
                        };
                        suggestion_idx = Some(idx);
                        buf = matches[idx].chars().collect();
                        pos = buf.len();
                    }
                }
                _ => {}
            }
            let suf = inline_suffix(&buf);
            draw(&buf, pos, &mut out, password, &placeholder, &suf);
        }
    };

    terminal::disable_raw_mode()?;

    // Restore default cursor color
    let _ = write!(out, "\x1b]112\x1b\\");

    // Move to next line
    let _ = write!(out, "\r\n");
    let _ = out.flush();

    result
}

pub fn select<T>(
    title: &str,
    description: Option<&str>,
    options: Vec<(String, T)>,
) -> std::io::Result<T> {
    select_with_default(title, description, options, 0)
}

pub fn select_with_default<T>(
    title: &str,
    description: Option<&str>,
    options: Vec<(String, T)>,
    default: usize,
) -> std::io::Result<T> {
    if !is_interactive() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "Missing required selection: {title}. In --ci mode, pass the value via a CLI flag or config."
            ),
        ));
    }

    if options.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "No options available for selection",
        ));
    }

    // Verbose mode: numbered list with simple input.
    // Prompts are NOT wrapped in tracing log lines — they print as plain text.
    if !is_pretty() {
        let labels: Vec<&str> = options.iter().map(|(label, _)| label.as_str()).collect();
        let term = Term::stderr();
        let full_prompt = match description {
            Some(desc) => format!("{title}\n{desc}"),
            None => title.to_string(),
        };
        let index = raw_select(&term, &full_prompt, &labels, &[], default)?;
        return Ok(options.into_iter().nth(index).unwrap().1);
    }

    let labels: Vec<&str> = options.iter().map(|(label, _)| label.as_str()).collect();
    let term = Term::stderr();
    let full_prompt = match description {
        Some(desc) => format!("{title}\n{desc}"),
        None => title.to_string(),
    };

    let index = raw_select(&term, &full_prompt, &labels, &[], default)?;

    Ok(options.into_iter().nth(index).unwrap().1)
}

/// Minimal select using crossterm — no cursor, no filter input, just arrow keys.
/// `hints` provides optional muted text after each label (e.g. "detected").
fn raw_select(
    term: &Term,
    prompt: &str,
    labels: &[&str],
    hints: &[&str],
    default: usize,
) -> std::io::Result<usize> {
    use crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
        terminal::{self, Clear, ClearType},
    };
    use std::io::Write;

    let mut selected = default;
    let mut out = std::io::stderr();

    // Print prompt (before raw mode)
    let _ = term.write_line(&brand_accent(prompt));

    let draw = |sel: usize, out: &mut std::io::Stderr| {
        for (i, label) in labels.iter().enumerate() {
            let hint = hints.get(i).filter(|h| !h.is_empty());
            if i == sel {
                let _ = write!(out, "{} {}", brand_accent("❯"), underline(label));
                if let Some(h) = hint {
                    let _ = write!(out, " {}", brand_muted(&format!("({h})")));
                }
            } else {
                let _ = write!(out, "  {label}");
                if let Some(h) = hint {
                    let _ = write!(out, " {}", brand_muted(&format!("({h})")));
                }
            }
            if i < labels.len() - 1 {
                let _ = write!(out, "\r\n");
            }
        }
        let _ = out.flush();
    };

    // Enter raw mode + hide cursor, then draw
    terminal::enable_raw_mode()?;
    crossterm::execute!(out, cursor::Hide)?;
    draw(selected, &mut out);

    let result = loop {
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if selected > 0 {
                        selected -= 1;
                    } else {
                        selected = labels.len() - 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if selected < labels.len() - 1 {
                        selected += 1;
                    } else {
                        selected = 0;
                    }
                }
                KeyCode::Enter => {
                    break Ok(selected);
                }
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "Operation interrupted",
                    ));
                }
                KeyCode::Esc => {
                    break Err(wizard_back_error());
                }
                _ => continue,
            }
            // Move cursor up to first option, clear, and redraw
            if labels.len() > 1 {
                crossterm::execute!(out, cursor::MoveUp((labels.len() - 1) as u16),)?;
            }
            crossterm::execute!(out, cursor::MoveToColumn(0))?;
            for _ in 0..labels.len() {
                crossterm::execute!(out, Clear(ClearType::CurrentLine))?;
                let _ = write!(out, "\r\n");
            }
            // Move back up
            crossterm::execute!(
                out,
                cursor::MoveUp(labels.len() as u16),
                cursor::MoveToColumn(0),
            )?;
            draw(selected, &mut out);
        }
    };

    // Move cursor below the last option so we're on a clean line
    let _ = write!(out, "\r\n");
    let _ = out.flush();

    // Restore terminal
    terminal::disable_raw_mode()?;
    crossterm::execute!(out, cursor::Show)?;

    if is_pretty() {
        // Clear the list + prompt, replace with a summary line
        let prompt_lines = prompt.chars().filter(|c| *c == '\n').count() + 1;
        let total = labels.len() + prompt_lines;
        let _ = term.clear_last_lines(total);

        if let Ok(idx) = &result {
            let title = prompt.lines().next().unwrap_or(prompt);
            let separator = brand_muted("›");
            let _ = term.write_line(&format!("{title} {separator} {}", labels[*idx]));
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Wizard — declarative multi-step prompt with pre-defined field layout
// ---------------------------------------------------------------------------

struct WizardField {
    label: String,
    value: Option<String>,
    visible: bool,
    /// How many terminal lines this field occupies when answered (answer + info lines).
    lines: usize,
}

pub struct Wizard {
    fields: Vec<WizardField>,
    term: Term,
    confirmation: bool,
}

impl Wizard {
    pub fn new() -> Self {
        Self {
            fields: Vec::new(),
            term: Term::stderr(),
            confirmation: false,
        }
    }

    /// Define all fields upfront with their order and subsection grouping.
    /// Each entry is `(label, subsection)`. Subsection fields start hidden.
    pub fn with_fields(mut self, fields: &[(&str, bool)]) -> Self {
        self.fields = fields
            .iter()
            .map(|(label, subsection)| WizardField {
                label: label.to_string(),
                value: None,
                visible: !subsection,
                lines: 0,
            })
            .collect();
        self
    }

    /// Enable a "Looks good?" confirmation prompt at the end of the wizard.
    pub fn with_confirmation(mut self) -> Self {
        self.confirmation = true;
        self
    }

    /// Set a field's value and how many screen lines it occupies.
    fn set_with_lines(&mut self, label: &str, value: &str, lines: usize) {
        if let Some(field) = self.fields.iter_mut().find(|f| f.label == label) {
            field.value = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            field.lines = lines;
        }
    }

    /// Set a field's value (1 screen line).
    pub fn set(&mut self, label: &str, value: &str) {
        self.set_with_lines(label, value, 1);
    }

    /// Clear a field's value back to pending.
    #[allow(dead_code)]
    pub fn clear_value(&mut self, label: &str) {
        if let Some(field) = self.fields.iter_mut().find(|f| f.label == label) {
            field.value = None;
        }
    }

    /// Set visibility for a field.
    pub fn set_visible(&mut self, label: &str, visible: bool) {
        if let Some(field) = self.fields.iter_mut().find(|f| f.label == label) {
            field.visible = visible;
            if !visible {
                field.value = None;
            }
        }
    }

    /// Remove the last visible answered field's value
    /// (for correcting invalid input like bad port numbers).
    pub fn undo_last(&mut self) {
        if let Some(field) = self
            .fields
            .iter_mut()
            .rev()
            .find(|f| f.visible && f.value.is_some())
        {
            field.value = None;
        }
    }

    /// Whether this is the first visible field (no previous answered fields).
    fn is_first_field(&self, label: &str) -> bool {
        let idx = self
            .fields
            .iter()
            .position(|f| f.label == label)
            .unwrap_or(0);
        !self.fields[..idx]
            .iter()
            .any(|f| f.visible && f.value.is_some())
    }

    /// In pretty mode on ESC: clear `current_lines` (the current prompt) plus
    /// the previous answered field's lines (if any). In verbose mode: no-op.
    fn clear_back(&self, current_label: &str, current_lines: usize) {
        if !is_pretty() {
            return;
        }
        let idx = self
            .fields
            .iter()
            .position(|f| f.label == current_label)
            .unwrap_or(0);
        let prev_lines = self.fields[..idx]
            .iter()
            .rev()
            .find(|f| f.visible && f.value.is_some())
            .map_or(0, |f| f.lines);
        let _ = self.term.clear_last_lines(current_lines + prev_lines);
    }

    pub fn input(
        &mut self,
        label: &str,
        default: Option<&str>,
        info: Option<&str>,
    ) -> std::io::Result<String> {
        let first = self.is_first_field(label);
        loop {
            if let Some(text) = info {
                let _ = self.term.write_line(&format!(
                    "{} {}",
                    bold(&brand_warning("!")),
                    brand_warning(text)
                ));
            }
            match text_field(label, default) {
                Ok(value) => {
                    let lines = if info.is_some() { 2 } else { 1 };
                    self.set_with_lines(label, &value, lines);
                    return Ok(value);
                }
                Err(e) if is_wizard_back(&e) => {
                    let lines = if info.is_some() { 2 } else { 1 };
                    if first {
                        // First field: clear prompt and re-show it
                        if is_pretty() {
                            let _ = self.term.clear_last_lines(lines);
                        }
                        continue;
                    }
                    self.clear_back(label, lines);
                    return Err(wizard_back_error());
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn select<T: Clone>(
        &mut self,
        label: &str,
        prompt: &str,
        options: Vec<(String, T)>,
        hints: &[&str],
        default: usize,
    ) -> std::io::Result<T> {
        if options.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "No options available for selection",
            ));
        }

        let first = self.is_first_field(label);
        loop {
            let labels: Vec<&str> = options.iter().map(|(l, _)| l.as_str()).collect();
            match raw_select(&self.term, prompt, &labels, hints, default) {
                Ok(index) => {
                    let display_label = options[index].0.clone();
                    let value = options.into_iter().nth(index).unwrap().1;
                    self.set(label, &display_label);
                    return Ok(value);
                }
                Err(e) if is_wizard_back(&e) => {
                    // raw_select already cleared its own display
                    if first {
                        continue;
                    }
                    self.clear_back(label, 0);
                    return Err(wizard_back_error());
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Accept a fully configured [`TextField`] builder and track the answer.
    pub fn text_field(&mut self, builder: TextField) -> std::io::Result<String> {
        let label = builder.label.to_string();
        let first = self.is_first_field(&label);
        loop {
            match builder.clone().prompt() {
                Ok(value) => {
                    self.set(&label, &value);
                    return Ok(value);
                }
                Err(e) if is_wizard_back(&e) => {
                    if first {
                        if is_pretty() {
                            let _ = self.term.clear_last_lines(1);
                        }
                        continue;
                    }
                    self.clear_back(&label, 1);
                    return Err(wizard_back_error());
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn confirm(&mut self, prompt: &str) -> std::io::Result<bool> {
        self.confirm_with_description(prompt, None)
    }

    pub fn confirm_with_description(
        &mut self,
        prompt: &str,
        description: Option<&str>,
    ) -> std::io::Result<bool> {
        match confirm_with_description(prompt, description, true) {
            Err(e) if is_wizard_back(&e) => Err(wizard_back_error()),
            result => result,
        }
    }

    /// Finalize the wizard. If confirmation is enabled, shows a "Looks good?" prompt.
    /// Returns `Ok(true)` to proceed, `Ok(false)` to restart from step 0.
    /// ESC goes back one step via `wizard_back`.
    pub fn finish(&mut self) -> std::io::Result<bool> {
        if !self.confirmation {
            return Ok(true);
        }
        match self.confirm("Looks good?") {
            Ok(true) => Ok(true),
            Ok(false) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Used internally by filter_suggestions tests — kept for test compatibility.
#[cfg(test)]
fn filter_suggestions(suggestions: &[String], current_input: &str) -> Vec<String> {
    let needle = current_input.to_lowercase();
    let mut filtered = Vec::new();

    for candidate in suggestions {
        if !needle.is_empty() && !candidate.to_lowercase().contains(&needle) {
            continue;
        }
        if !filtered.iter().any(|existing| existing == candidate) {
            filtered.push(candidate.clone());
        }
    }

    filtered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_round_trip() {
        set_verbose(false);
        assert!(!is_verbose());

        set_verbose(true);
        assert!(is_verbose());
    }

    #[test]
    fn confirm_returns_default_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let answer = confirm("Proceed?", false).unwrap();
        assert!(!answer);
    }

    #[test]
    fn text_field_uses_default_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let value = text_field("Server host", Some("localhost")).unwrap();
        assert_eq!(value, "localhost");
    }

    #[test]
    fn strong_returns_plain_in_test() {
        assert_eq!(strong("production"), "production");
    }

    #[test]
    fn text_field_without_default_errors_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let err = text_field("Server host", None).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn text_field_with_suggestions_uses_default_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let suggestions = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let value = TextField::new("Server host")
            .with_default("localhost")
            .suggestions(&suggestions)
            .prompt()
            .unwrap();
        assert_eq!(value, "localhost");
    }

    #[test]
    fn text_field_with_suggestions_without_default_errors_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let suggestions = vec!["localhost".to_string()];
        let err = TextField::new("Server host")
            .suggestions(&suggestions)
            .prompt()
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn filter_suggestions_preserves_input_order() {
        let suggestions = vec![
            "related-2".to_string(),
            "related-1".to_string(),
            "global-a".to_string(),
            "related-2".to_string(),
        ];
        let filtered = filter_suggestions(&suggestions, "");
        assert_eq!(
            filtered,
            vec![
                "related-2".to_string(),
                "related-1".to_string(),
                "global-a".to_string()
            ]
        );
    }

    #[test]
    fn filter_suggestions_matches_case_insensitive_substring() {
        let suggestions = vec![
            "Prod-EU".to_string(),
            "staging-us".to_string(),
            "prod-us".to_string(),
        ];
        let filtered = filter_suggestions(&suggestions, "PROD");
        assert_eq!(filtered, vec!["Prod-EU".to_string(), "prod-us".to_string()]);
    }

    #[test]
    fn select_errors_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let err = select("Pick one", None, vec![("server-a".to_string(), 1)]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn with_spinner_async_runs_future_in_non_tty_context() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let value: Result<usize, String> =
            rt.block_on(with_spinner_async("Working", "Done", async { Ok(42usize) }));
        assert_eq!(value.unwrap(), 42);
    }

    #[test]
    fn format_elapsed_omits_below_threshold() {
        assert_eq!(format_elapsed(Duration::from_millis(50)), "");
        assert_eq!(format_elapsed(Duration::from_millis(99)), "");
    }

    #[test]
    fn format_elapsed_one_decimal_under_ten_seconds() {
        assert_eq!(format_elapsed(Duration::from_millis(3200)), "(3.2s)");
        assert_eq!(format_elapsed(Duration::from_millis(100)), "(0.1s)");
    }

    #[test]
    fn format_elapsed_whole_seconds_under_sixty() {
        assert_eq!(format_elapsed(Duration::from_secs(42)), "(42s)");
        assert_eq!(format_elapsed(Duration::from_secs(10)), "(10s)");
    }

    #[test]
    fn format_elapsed_minutes_and_seconds() {
        assert_eq!(format_elapsed(Duration::from_secs(125)), "(2m5s)");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "(1m0s)");
    }

    #[test]
    fn brand_accent_returns_plain_in_test() {
        assert_eq!(brand_accent("hello"), "hello");
        assert_eq!(brand_success("ok"), "ok");
        assert_eq!(brand_warning("warn"), "warn");
        assert_eq!(brand_error("fail"), "fail");
    }

    #[test]
    fn warning_formatters_render_plain_text_in_tests() {
        assert_eq!(
            format_warning_full_line("One-time sudo required"),
            "┃ One-time sudo required"
        );
        assert_eq!(
            format_warning_bullet_line("Configure local DNS for *.tako.test"),
            "┃ • Configure local DNS for *.tako.test"
        );
    }

    #[test]
    fn ci_round_trip() {
        set_ci(false);
        assert!(!is_ci());
        set_ci(true);
        assert!(is_ci());
        set_ci(false);
    }

    #[test]
    fn with_spinner_runs_work_in_non_tty() {
        // Non-interactive: work runs directly, result returned
        let result: Result<usize, String> = with_spinner("Loading", "Done", || Ok(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn format_size_uses_expected_units() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn transfer_progress_in_non_tty() {
        // Non-interactive: creates TransferProgress without a progress bar
        let tp = TransferProgress::new("Downloading", "Download complete", 1024);
        tp.set_position(512);
        tp.finish();
    }

    #[test]
    fn format_elapsed_trace_always_shows_value() {
        assert_eq!(format_elapsed_trace(Duration::from_millis(3)), "(3ms)");
        assert_eq!(format_elapsed_trace(Duration::from_millis(50)), "(50ms)");
        assert_eq!(format_elapsed_trace(Duration::from_millis(999)), "(999ms)");
        assert_eq!(format_elapsed_trace(Duration::from_millis(1200)), "(1.2s)");
        assert_eq!(format_elapsed_trace(Duration::from_secs(42)), "(42s)");
        assert_eq!(format_elapsed_trace(Duration::from_secs(125)), "(2m5s)");
    }
}
