use std::fmt::Display;
use std::future::Future;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dialoguer::console::{Term, colors_enabled};
use dialoguer::theme::ColorfulTheme;
use indicatif::{ProgressBar, ProgressStyle};

static VERBOSE: AtomicBool = AtomicBool::new(false);

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
    std::io::stdout().is_terminal() && colors_enabled()
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
        format!("\x1b[2m{value}\x1b[22m")
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
    println!();
    println!("{}", bold(&brand_accent(title)));
}

pub fn info(message: &str) {
    println!("{}", brand_fg(message));
}

pub fn step(message: &str) {
    info(message);
}

pub fn bullet(message: &str) {
    println!("  {} {}", bold(&brand_secondary("•")), brand_fg(message));
}

pub fn success(message: &str) {
    println!("{} {}", bold(&brand_success("✓")), brand_fg(message));
}

pub fn warning(message: &str) {
    println!("{} {}", bold(&brand_warning("!")), brand_fg(message));
}

pub fn error(message: &str) {
    println!("{} {}", bold(&brand_error("✗")), brand_fg(message));
}

pub fn error_stderr(message: &str) {
    eprintln!("{} {}", bold(&brand_error("✗")), brand_fg(message));
}

pub fn muted(message: &str) {
    println!("{}", brand_muted(message));
}

pub fn emphasized(value: &str) -> String {
    if cfg!(test) {
        value.to_string()
    } else if std::io::stdout().is_terminal() && colors_enabled() {
        format!("\x1b[3m{}\x1b[23m", value)
    } else {
        value.to_string()
    }
}

// ---------------------------------------------------------------------------
// Spinner helpers
// ---------------------------------------------------------------------------

fn spinner_style() -> ProgressStyle {
    let teal_spinner = if should_colorize() {
        let (r, g, b) = BRAND_TEAL;
        format!("\x1b[38;2;{r};{g};{b}m{{spinner}}\x1b[39m")
    } else {
        "{spinner}".to_string()
    };
    ProgressStyle::with_template(&format!("{teal_spinner} {{msg}}"))
        .unwrap()
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "])
}

fn finish_spinner_ok(pb: &ProgressBar, success: &str, elapsed: Duration) {
    let elapsed_str = format_elapsed(elapsed);
    let check = bold(&brand_success("✓"));
    if elapsed_str.is_empty() {
        pb.finish_with_message(format!("{check} {success}"));
    } else {
        pb.finish_with_message(format!("{check} {success} {}", brand_muted(&elapsed_str)));
    }
}

fn finish_spinner_err(pb: &ProgressBar, loading: &str) {
    let x = bold(&brand_error("✗"));
    pb.finish_with_message(format!("{x} {loading} failed"));
}

/// Spinner that transforms in-place.
///
/// - While running: `⠋ {loading}...`
/// - On `Ok`:       `✓ {success} (elapsed)`
/// - On `Err`:      `✗ {loading} failed`
pub fn with_spinner<T, E, F>(loading: &str, success: &str, work: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
{
    if !is_interactive() {
        return work();
    }

    let start = Instant::now();
    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());
    pb.set_message(format!("{loading}..."));
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = work();

    match &result {
        Ok(_) => finish_spinner_ok(&pb, success, start.elapsed()),
        Err(_) => finish_spinner_err(&pb, loading),
    }

    result
}

/// Async spinner that transforms in-place.
pub async fn with_spinner_async<T, E, Fut>(loading: &str, success: &str, work: Fut) -> Result<T, E>
where
    Fut: Future<Output = Result<T, E>>,
{
    if !is_interactive() {
        return work.await;
    }

    let start = Instant::now();
    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());
    pb.set_message(format!("{loading}..."));
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = work.await;

    match &result {
        Ok(_) => finish_spinner_ok(&pb, success, start.elapsed()),
        Err(_) => finish_spinner_err(&pb, loading),
    }

    result
}

/// Simple spinner — shows while running, clears when done. No transform.
/// For callers whose work doesn't return Result.
pub fn with_spinner_simple<T, F>(message: &str, work: F) -> T
where
    F: FnOnce() -> T,
{
    if !is_interactive() {
        return work();
    }

    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());
    pb.set_message(format!("{message}..."));
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = work();
    pb.finish_and_clear();
    result
}

/// Async simple spinner — shows while running, clears when done.
pub async fn with_spinner_async_simple<T, Fut>(message: &str, work: Fut) -> T
where
    Fut: Future<Output = T>,
{
    if !is_interactive() {
        return work.await;
    }

    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());
    pb.set_message(format!("{message}..."));
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = work.await;
    pb.finish_and_clear();
    result
}

// ---------------------------------------------------------------------------
// Dialoguer theme
// ---------------------------------------------------------------------------

fn tako_theme() -> ColorfulTheme {
    use dialoguer::console::{Color, Style, style};
    ColorfulTheme {
        prompt_style: Style::new().color256(152), // teal approx
        prompt_prefix: style("?".to_string()).color256(152),
        prompt_suffix: style("›".to_string()).color256(152),
        success_prefix: style("✓".to_string()).color256(114), // green approx
        success_suffix: style("·".to_string()).color256(152),
        error_prefix: style("✗".to_string()).color256(174), // red approx
        error_style: Style::new().color256(174),
        hint_style: Style::new().color256(250),
        values_style: Style::new().color256(152),
        active_item_style: Style::new().color256(174), // coral approx
        active_item_prefix: style("❯".to_string()).color256(174),
        inactive_item_prefix: style(" ".to_string()).fg(Color::White),
        ..ColorfulTheme::default()
    }
}

// ---------------------------------------------------------------------------
// Prompts — wizards vanish after the user answers
// ---------------------------------------------------------------------------

fn dialoguer_err(e: dialoguer::Error) -> std::io::Error {
    match e {
        dialoguer::Error::IO(io_err) => io_err,
    }
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

    let term = Term::stderr();
    let full_prompt = match description {
        Some(desc) => format!("{prompt}\n{desc}"),
        None => prompt.to_string(),
    };
    let result = dialoguer::Confirm::with_theme(&tako_theme())
        .with_prompt(&full_prompt)
        .default(default)
        .interact_on(&term)
        .map_err(dialoguer_err)?;
    let lines = if description.is_some() { 2 } else { 1 };
    let _ = term.clear_last_lines(lines);
    Ok(result)
}

pub fn prompt_password(prompt: &str, allow_empty: bool) -> std::io::Result<String> {
    if !is_interactive() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Password prompt requires an interactive terminal",
        ));
    }

    let term = Term::stderr();
    let value = dialoguer::Password::with_theme(&tako_theme())
        .with_prompt(prompt)
        .allow_empty_password(allow_empty)
        .interact_on(&term)
        .map_err(dialoguer_err)?;
    let _ = term.clear_last_lines(1);
    if !allow_empty && value.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Input cannot be empty",
        ));
    }
    Ok(value)
}

pub fn prompt_input(
    prompt: &str,
    allow_empty: bool,
    default: Option<&str>,
) -> std::io::Result<String> {
    prompt_input_with_suggestions(prompt, allow_empty, default, &[])
}

/// Suggestion completer for dialoguer tab-completion.
struct SuggestionCompleter {
    suggestions: Vec<String>,
}

impl dialoguer::Completion for SuggestionCompleter {
    fn get(&self, input: &str) -> Option<String> {
        let needle = input.to_lowercase();
        self.suggestions
            .iter()
            .find(|s| {
                let lower = s.to_lowercase();
                lower.starts_with(&needle) && lower != needle
            })
            .cloned()
    }
}

pub fn prompt_input_with_suggestions(
    prompt: &str,
    allow_empty: bool,
    default: Option<&str>,
    suggestions: &[String],
) -> std::io::Result<String> {
    if !is_interactive() {
        return match default {
            Some(value) => Ok(value.to_string()),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Input prompt requires an interactive terminal",
            )),
        };
    }

    let term = Term::stderr();
    let theme = tako_theme();
    let completer = SuggestionCompleter {
        suggestions: suggestions.to_vec(),
    };
    let mut input = dialoguer::Input::<String>::with_theme(&theme).with_prompt(prompt);
    if let Some(default_value) = default {
        input = input.default(default_value.to_string());
    }
    if allow_empty {
        input = input.allow_empty(true);
    }
    if !suggestions.is_empty() {
        input = input.completion_with(&completer);
    }

    let value = input.interact_on(&term).map_err(dialoguer_err)?;
    let _ = term.clear_last_lines(1);
    if !allow_empty && value.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Input cannot be empty",
        ));
    }

    Ok(value)
}

pub fn select<T>(
    title: &str,
    description: Option<&str>,
    options: Vec<(String, T)>,
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

    let index = if options.len() >= 8 {
        dialoguer::FuzzySelect::with_theme(&tako_theme())
            .with_prompt(&full_prompt)
            .items(&labels)
            .interact_on(&term)
            .map_err(dialoguer_err)?
    } else {
        dialoguer::Select::with_theme(&tako_theme())
            .with_prompt(&full_prompt)
            .items(&labels)
            .interact_on(&term)
            .map_err(dialoguer_err)?
    };

    // Vanish — clear the summary line left by dialoguer
    let _ = term.clear_last_lines(1);

    Ok(options.into_iter().nth(index).unwrap().1)
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
    fn prompt_input_uses_default_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let value = prompt_input("Server host", false, Some("localhost")).unwrap();
        assert_eq!(value, "localhost");
    }

    #[test]
    fn emphasized_falls_back_to_plain_text_in_non_tty_context() {
        assert_eq!(emphasized("production"), "production");
    }

    #[test]
    fn prompt_input_without_default_errors_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let err = prompt_input("Server host", false, None).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn prompt_input_with_suggestions_uses_default_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let suggestions = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let value =
            prompt_input_with_suggestions("Server host", false, Some("localhost"), &suggestions)
                .unwrap();
        assert_eq!(value, "localhost");
    }

    #[test]
    fn prompt_input_with_suggestions_without_default_errors_in_non_tty_context() {
        if is_interactive() {
            return;
        }
        let suggestions = vec!["localhost".to_string()];
        let err =
            prompt_input_with_suggestions("Server host", false, None, &suggestions).unwrap_err();
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
