use std::fmt::Display;
use std::future::Future;
use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use console::Term;
use indicatif::{ProgressBar, ProgressStyle};

static VERBOSE: AtomicBool = AtomicBool::new(false);
static SUPPRESS: AtomicBool = AtomicBool::new(false);

/// While output is suppressed, most printing functions become no-ops and
/// inner spinners are skipped (the work still runs).
pub fn set_suppress(suppress: bool) {
    SUPPRESS.store(suppress, Ordering::Relaxed);
}

fn is_suppressed() -> bool {
    SUPPRESS.load(Ordering::Relaxed)
}

// Brand palette — RGB values matching dev/output.rs
const BRAND_TEAL: (u8, u8, u8) = (155, 196, 182); // #9BC4B6 — accent, prompts
#[allow(dead_code)]
const BRAND_CORAL: (u8, u8, u8) = (232, 135, 131); // #E88783 — primary emphasis
const BRAND_GREEN: (u8, u8, u8) = (155, 217, 179); // #9BD9B3 — success
const BRAND_AMBER: (u8, u8, u8) = (234, 211, 156); // #EAD39C — warning
const BRAND_RED: (u8, u8, u8) = (232, 163, 160); // #E8A3A0 — error

fn should_colorize() -> bool {
    if cfg!(test) {
        return false;
    }
    std::io::stdout().is_terminal()
}

fn rgb_fg<D: Display>(value: D, (r, g, b): (u8, u8, u8)) -> String {
    if should_colorize() {
        format!("\x1b[38;2;{r};{g};{b}m{value}\x1b[39m")
    } else {
        value.to_string()
    }
}

pub fn brand_accent<D: Display>(value: D) -> String {
    rgb_fg(value, BRAND_TEAL)
}

pub fn brand_secondary<D: Display>(value: D) -> String {
    rgb_fg(value, BRAND_TEAL)
}

pub fn brand_fg<D: Display>(value: D) -> String {
    value.to_string()
}

pub fn brand_muted<D: Display>(value: D) -> String {
    if should_colorize() {
        // Re-apply dim after any embedded bold-reset (\x1b[22m) so that
        // highlight()/bold() calls inside a muted() context don't cancel
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

pub fn bold(value: &str) -> String {
    if should_colorize() {
        format!("\x1b[1m{value}\x1b[22m")
    } else {
        value.to_string()
    }
}

/// Bold text for embedding inside a `muted()` string.
/// Re-applies dim after the bold-reset so surrounding muted style is preserved.
pub fn underline<D: Display>(value: D) -> String {
    if should_colorize() {
        format!("\x1b[4m{value}\x1b[24m")
    } else {
        value.to_string()
    }
}

pub fn bold_muted(value: &str) -> String {
    if should_colorize() {
        format!("\x1b[1m{value}\x1b[22m\x1b[2m")
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

pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

pub fn is_interactive() -> bool {
    #[cfg(test)]
    {
        false
    }

    #[cfg(not(test))]
    {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    }
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn section(title: &str) {
    if is_suppressed() {
        return;
    }
    println!();
    println!("{}", bold(&brand_accent(title)));
}

pub fn heading(title: &str) {
    if is_suppressed() {
        return;
    }
    println!();
    println!("{}", bold(title));
}

pub fn info(message: &str) {
    if is_suppressed() {
        return;
    }
    println!("{}", brand_fg(message));
}

pub fn step(message: &str) {
    info(message);
}

pub fn bullet(message: &str) {
    if is_suppressed() {
        return;
    }
    println!("  {} {}", bold(&brand_secondary("•")), brand_fg(message));
}

pub fn success(message: &str) {
    if is_suppressed() {
        return;
    }
    println!("{} {}", bold(&brand_secondary("✔")), brand_fg(message));
}

pub fn warning(message: &str) {
    if is_suppressed() {
        return;
    }
    println!("{} {}", bold(&brand_warning("!")), brand_fg(message));
}

pub fn error(message: &str) {
    if is_suppressed() {
        return;
    }
    println!("{} {}", bold(&brand_error("✗")), brand_fg(message));
}

pub fn error_stderr(message: &str) {
    eprintln!("{} {}", bold(&brand_error("✗")), brand_fg(message));
}

pub fn muted(message: &str) {
    if is_suppressed() {
        return;
    }
    println!("{}", brand_muted(message));
}

/// Print a hint line in default text color (not muted).
/// Use for actionable guidance like "Run X to do Y" where the command is highlight()'d.
pub fn hint(message: &str) {
    println!("{}", brand_fg(message));
}

/// Format a value in bold+accent. Use for dynamic names/values in output lines.
/// e.g. `highlight("tako-demo")` → bold teal "tako-demo"
pub fn highlight(value: &str) -> String {
    bold(&brand_accent(value))
}

// ---------------------------------------------------------------------------
// Spinner helpers
// ---------------------------------------------------------------------------

pub const SPINNER_TICKS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "];

fn teal_spinner_token() -> String {
    if should_colorize() {
        let (r, g, b) = BRAND_TEAL;
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
    let elapsed = if should_colorize() {
        "\x1b[2m({elapsed})\x1b[22m"
    } else {
        "({elapsed})"
    };
    ProgressStyle::with_template(&format!("{s} {{msg}} {elapsed}"))
        .unwrap()
        .tick_strings(SPINNER_TICKS)
}

/// Print a spinner result without elapsed (fast path — spinner was never shown).
fn print_ok(success: &str) {
    let check = brand_success("✓");
    println!("{check} {}", brand_fg(success));
}

fn print_err(loading: &str) {
    let x = bold(&brand_error("✗"));
    println!("{x} {loading}");
}

fn print_err_with_detail(loading: &str, detail: &dyn Display) {
    let x = bold(&brand_error("✗"));
    println!("{x} {loading}: {detail}");
}

/// Print a spinner result with elapsed (slow path — spinner was visible).
pub fn hide_cursor() {
    let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Hide);
}

pub fn show_cursor() {
    let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Show);
}

fn finish_spinner_ok(pb: &ProgressBar, success: &str, elapsed: Duration) {
    pb.finish_and_clear();
    show_cursor();
    let check = brand_success("✓");
    let elapsed_str = format_elapsed(elapsed);
    if elapsed_str.is_empty() {
        println!("{check} {}", brand_fg(success));
    } else {
        println!(
            "{check} {} {}",
            brand_fg(success),
            brand_muted(&elapsed_str)
        );
    }
}

fn finish_spinner_err(pb: &ProgressBar, loading: &str) {
    pb.finish_and_clear();
    show_cursor();
    let x = bold(&brand_error("✗"));
    println!("{x} {loading}");
}

fn finish_spinner_err_with_detail(pb: &ProgressBar, loading: &str, detail: &dyn Display) {
    pb.finish_and_clear();
    show_cursor();
    let x = bold(&brand_error("✗"));
    println!("{x} {loading}: {detail}");
}

/// Spinner that shows only if work takes >= 1s, then clears on completion.
///
/// - Fast (<1s):  prints result directly, no spinner, no elapsed
/// - Slow (≥1s):  `⠋ {loading}...` → `{success} (elapsed)` or `✗ {loading} failed`
pub fn with_spinner<T, E, F>(loading: &str, success: &str, work: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
{
    if !is_interactive() || is_suppressed() {
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
    error: &str,
    work: Fut,
) -> Result<T, E>
where
    Fut: Future<Output = Result<T, E>>,
{
    if !is_interactive() || is_suppressed() {
        return work.await;
    }

    let start = Instant::now();
    let mut work = std::pin::pin!(work);

    // Fast path: complete within 1s — no spinner needed.
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(1), work.as_mut()).await {
        match &result {
            Ok(_) => print_ok(success),
            Err(e) => print_err_with_detail(error, e),
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
        Err(e) => finish_spinner_err_with_detail(&pb, error, e),
    }

    result
}

/// Simple spinner — shows only if work takes >= 1s, then clears. No result line.
pub fn with_spinner_simple<T, F>(message: &str, work: F) -> T
where
    F: FnOnce() -> T,
{
    if !is_interactive() || is_suppressed() {
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
pub async fn with_spinner_async_simple<T, Fut>(message: &str, work: Fut) -> T
where
    Fut: Future<Output = T>,
{
    if !is_interactive() || is_suppressed() {
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

/// A phase spinner that suppresses all inner output while active.
/// On drop, restores output and clears the spinner (acts as a safety net
/// if the phase is exited via `?`).
pub struct PhaseSpinner {
    pb: Option<ProgressBar>,
    start: Instant,
    finished: bool,
}

impl PhaseSpinner {
    pub fn start(message: &str) -> Self {
        set_suppress(true);
        let pb = if is_interactive() {
            let pb = ProgressBar::new_spinner();
            pb.set_style(phase_spinner_style());
            pb.set_message(message.to_string());
            pb.enable_steady_tick(Duration::from_millis(80));
            hide_cursor();
            Some(pb)
        } else {
            None
        };
        Self {
            pb,
            start: Instant::now(),
            finished: false,
        }
    }

    pub fn finish(mut self, success: &str) {
        set_suppress(false);
        if let Some(ref pb) = self.pb {
            finish_spinner_ok(pb, success, self.start.elapsed());
        }
        self.finished = true;
    }

    pub fn finish_err(mut self, loading: &str, detail: &str) {
        set_suppress(false);
        if let Some(ref pb) = self.pb {
            finish_spinner_err_with_detail(pb, loading, &detail);
        }
        self.finished = true;
    }
}

impl Drop for PhaseSpinner {
    fn drop(&mut self) {
        set_suppress(false);
        if !self.finished {
            if let Some(ref pb) = self.pb {
                pb.finish_and_clear();
                show_cursor();
            }
        }
    }
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

    use crossterm::{
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
        terminal,
    };

    let term = Term::stderr();

    // Print description line first (if any)
    if let Some(desc) = description {
        let _ = term.write_line(desc);
    }

    // Print prompt with (Y/n) or (y/N) hint
    let separator = brand_muted("›");
    let styled_hint = brand_muted("(y/n)");
    let styled_prompt = format!("{} {styled_hint} {separator} ", brand_accent(prompt));
    eprint!("{styled_prompt}");

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
                    eprintln!("y");
                    break Ok(true);
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    terminal::disable_raw_mode()?;
                    eprintln!("n");
                    break Ok(false);
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
                _ => {} // ignore other keys
            }
        }
    };

    // Vanish the prompt (and description if present)
    let lines = if description.is_some() { 2 } else { 1 };
    let _ = term.clear_last_lines(lines);

    result
}

pub fn password_field(prompt: &str) -> std::io::Result<String> {
    TextField::new(prompt).password().prompt()
}

pub fn text_field(prompt: &str, default: Option<&str>) -> std::io::Result<String> {
    TextField::new(prompt).default_opt(default).prompt()
}

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
                    "Input prompt requires an interactive terminal",
                )),
            };
        }

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
    let (cr, cg, cb) = BRAND_TEAL;
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
            "Selection prompt requires an interactive terminal",
        ));
    }

    if options.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "No options available for selection",
        ));
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

    // Clear the list + prompt
    let prompt_lines = prompt.chars().filter(|c| *c == '\n').count() + 1;
    let total = labels.len() + prompt_lines;
    let _ = term.clear_last_lines(total);

    result
}

// ---------------------------------------------------------------------------
// Wizard — declarative multi-step prompt with pre-defined field layout
// ---------------------------------------------------------------------------

struct WizardField {
    label: String,
    value: Option<String>,
    subsection: bool,
    visible: bool,
}

pub struct Wizard {
    fields: Vec<WizardField>,
    term: Term,
    lines_on_screen: usize,
    confirmation: bool,
}

impl Wizard {
    pub fn new() -> Self {
        Self {
            fields: Vec::new(),
            term: Term::stderr(),
            lines_on_screen: 0,
            confirmation: false,
        }
    }

    /// Define all fields upfront with their order and subsection grouping.
    /// Each entry is `(label, subsection)`.
    pub fn with_fields(mut self, fields: &[(&str, bool)]) -> Self {
        self.fields = fields
            .iter()
            .map(|(label, subsection)| WizardField {
                label: label.to_string(),
                value: None,
                subsection: *subsection,
                visible: !subsection, // root fields visible by default
            })
            .collect();
        self
    }

    /// Enable a "Looks good?" confirmation prompt at the end of the wizard.
    pub fn with_confirmation(mut self) -> Self {
        self.confirmation = true;
        self
    }

    /// Set a field's value.
    pub fn set(&mut self, label: &str, value: &str) {
        if let Some(field) = self.fields.iter_mut().find(|f| f.label == label) {
            field.value = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        self.redraw();
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

    /// Remove the last visible answered field's value and redraw
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
        self.redraw();
    }

    fn redraw(&mut self) {
        if self.lines_on_screen > 0 {
            let _ = self.term.clear_last_lines(self.lines_on_screen);
        }

        let visible: Vec<&WizardField> = self.fields.iter().filter(|f| f.visible).collect();

        let max_label = visible.iter().map(|f| f.label.len()).max().unwrap_or(0);

        let mut first_sub = true;
        for field in &visible {
            let label = brand_muted(&format!("{:<width$}", field.label, width = max_label));
            if field.subsection {
                let branch = brand_muted(if first_sub { "└" } else { " " });
                first_sub = false;
                match &field.value {
                    Some(value) => {
                        let _ = self.term.write_line(&format!("{branch} {label}  {value}"));
                    }
                    None => {
                        let _ = self.term.write_line(&format!("{branch} {label}"));
                    }
                }
            } else {
                first_sub = true; // reset for next subsection group
                match &field.value {
                    Some(value) => {
                        let _ = self.term.write_line(&format!("{label}  {value}"));
                    }
                    None => {
                        let _ = self.term.write_line(&label.to_string());
                    }
                }
            }
        }

        self.lines_on_screen = visible.len();
        if !visible.is_empty() {
            let _ = self.term.write_line("");
            self.lines_on_screen += 1;
        }
    }

    pub fn input(
        &mut self,
        label: &str,
        default: Option<&str>,
        info: Option<&str>,
    ) -> std::io::Result<String> {
        self.redraw();
        if let Some(text) = info {
            let _ = self.term.write_line(&format!(
                "{} {}",
                bold(&brand_warning("!")),
                brand_warning(text)
            ));
            self.lines_on_screen += 1;
        }
        match text_field(label, default) {
            Ok(value) => {
                self.lines_on_screen += 1;
                self.set(label, &value);
                Ok(value)
            }
            Err(e) if is_wizard_back(&e) => {
                self.lines_on_screen += 1; // prompt line
                Err(wizard_back_error())
            }
            Err(e) => Err(e),
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
        self.redraw();

        let labels: Vec<&str> = options.iter().map(|(l, _)| l.as_str()).collect();
        match raw_select(&self.term, prompt, &labels, hints, default) {
            Ok(index) => {
                let display_label = options[index].0.clone();
                let value = options.into_iter().nth(index).unwrap().1;
                self.set(label, &display_label);
                Ok(value)
            }
            Err(e) if is_wizard_back(&e) => {
                // raw_select already cleaned up its own display
                Err(wizard_back_error())
            }
            Err(e) => Err(e),
        }
    }

    /// Accept a fully configured [`TextField`] builder and track the answer.
    pub fn text_field(&mut self, builder: TextField) -> std::io::Result<String> {
        self.redraw();
        let label = builder.label.to_string();
        match builder.prompt() {
            Ok(value) => {
                self.lines_on_screen += 1;
                self.set(&label, &value);
                Ok(value)
            }
            Err(e) if is_wizard_back(&e) => {
                self.lines_on_screen += 1; // prompt line
                Err(wizard_back_error())
            }
            Err(e) => Err(e),
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
        self.redraw();
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

    /// Track a line drawn by an external prompt (for proper clear on next redraw).
    pub fn track_line(&mut self) {
        self.lines_on_screen += 1;
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
    fn highlight_returns_plain_in_test() {
        assert_eq!(highlight("production"), "production");
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
        assert_eq!(brand_error("fail"), "fail");
    }
}
