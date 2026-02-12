use std::fmt::Display;
use std::future::Future;
use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use console::{colors_enabled, style};
use demand::{
    Confirm as DemandConfirm, DemandOption, Input as DemandInput, Select as DemandSelect,
    Spinner as DemandSpinner, Theme as DemandTheme,
};
use termcolor::{Color, ColorSpec};

static VERBOSE: AtomicBool = AtomicBool::new(false);
static DEMAND_THEME: LazyLock<DemandTheme> = LazyLock::new(tako_brand_demand_theme);

pub fn brand_accent<D: Display>(value: D) -> console::StyledObject<D> {
    style(value).cyan()
}

pub fn brand_secondary<D: Display>(value: D) -> console::StyledObject<D> {
    style(value).cyan()
}

pub fn brand_fg<D: Display>(value: D) -> console::StyledObject<D> {
    style(value)
}

pub fn brand_muted<D: Display>(value: D) -> console::StyledObject<D> {
    style(value).dim()
}

pub fn brand_success<D: Display>(value: D) -> console::StyledObject<D> {
    style(value).green()
}

pub fn brand_warning<D: Display>(value: D) -> console::StyledObject<D> {
    style(value).yellow()
}

pub fn brand_error<D: Display>(value: D) -> console::StyledObject<D> {
    style(value).red()
}

fn fg(color: Color) -> ColorSpec {
    let mut spec = ColorSpec::new();
    spec.set_fg(Some(color));
    spec
}

fn tako_brand_demand_theme() -> DemandTheme {
    let fg_color = Color::White;
    let secondary = Color::Cyan;
    let muted = Color::White;
    let label_muted = Color::Blue;
    let accent = Color::Cyan;
    let error = Color::Red;

    let mut title = fg(accent);
    title.set_bold(true);

    let focused_button = fg(accent);
    let blurred_button = fg(fg_color);

    let mut cursor_style = ColorSpec::new();
    cursor_style.set_fg(Some(fg_color));

    let mut theme = DemandTheme::new();
    theme.title = title;
    theme.description = fg(secondary);
    theme.cursor = fg(accent);
    theme.cursor_str = String::from("❯");
    theme.selected_prefix = String::from(" •");
    theme.selected_prefix_fg = fg(accent);
    theme.selected_option = fg(fg_color);
    theme.unselected_prefix = String::from(" •");
    theme.unselected_prefix_fg = fg(muted);
    theme.unselected_option = fg(fg_color);
    theme.input_cursor = fg(accent);
    theme.input_placeholder = fg(muted);
    theme.input_prompt = fg(accent);
    theme.help_key = fg(label_muted);
    theme.help_desc = fg(label_muted);
    theme.help_sep = fg(label_muted);
    theme.focused_button = focused_button;
    theme.blurred_button = blurred_button;
    theme.error_indicator = fg(error);
    theme.cursor_style = cursor_style;
    theme.force_style = true;
    theme
}

pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn section(title: &str) {
    println!();
    println!("{}", brand_accent(title).bold());
}

pub fn step(message: &str) {
    println!("{} {}", brand_secondary("•").bold(), brand_fg(message));
}

pub fn success(message: &str) {
    println!("{} {}", brand_success("✓").bold(), brand_fg(message));
}

pub fn warning(message: &str) {
    println!("{} {}", brand_warning("!").bold(), brand_fg(message));
}

pub fn error(message: &str) {
    println!("{} {}", brand_error("✗").bold(), brand_fg(message));
}

pub fn error_stderr(message: &str) {
    eprintln!("{} {}", brand_error("✗").bold(), brand_fg(message));
}

pub fn muted(message: &str) {
    println!("{}", brand_muted(message));
}

pub fn emphasized(value: &str) -> String {
    if std::io::stdout().is_terminal() && colors_enabled() {
        // Use italic on/off (3/23) instead of a full reset so surrounding styles
        // (like dim hints) stay active after emphasized text.
        format!("\x1b[3m{}\x1b[23m", value)
    } else {
        format!("'{}'", value)
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

    let mut confirm = DemandConfirm::new(prompt)
        .theme(&DEMAND_THEME)
        .selected(default);
    if let Some(description) = description {
        confirm = confirm.description(description);
    }

    confirm.run()
}

pub fn prompt_password(prompt: &str, allow_empty: bool) -> std::io::Result<String> {
    if !is_interactive() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Password prompt requires an interactive terminal",
        ));
    }

    let value = DemandInput::new(prompt)
        .theme(&DEMAND_THEME)
        .password(true)
        .run()?;
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

    let mut input = DemandInput::new(prompt).theme(&DEMAND_THEME);
    if let Some(default_value) = default {
        input = input.default_value(default_value);
    }
    if !suggestions.is_empty() {
        let candidates = Arc::new(suggestions.to_vec());
        input = input
            .autocomplete_fn(move |current_input| {
                Ok(filter_suggestions(candidates.as_ref(), current_input))
            })
            .max_suggestions_display(8);
    }

    let value = input.run()?;
    if !allow_empty && value.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Input cannot be empty",
        ));
    }

    Ok(value)
}

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

    let mut select = DemandSelect::new(title).theme(&DEMAND_THEME);
    if let Some(description) = description {
        select = select.description(description);
    }

    let demand_options: Vec<DemandOption<T>> = options
        .into_iter()
        .map(|(label, value)| DemandOption::with_label(label, value))
        .collect();

    if demand_options.len() >= 8 {
        select = select.filterable(true);
    }

    select.options(demand_options).run()
}

pub fn with_spinner<T, F, S>(message: S, work: F) -> std::io::Result<T>
where
    F: FnOnce() -> T + Send,
    T: Send,
    S: Into<String>,
{
    if !(std::io::stdout().is_terminal() && std::io::stderr().is_terminal()) {
        return Ok(work());
    }

    DemandSpinner::new(message)
        .theme(&DEMAND_THEME)
        .run(|_| work())
}

pub async fn with_spinner_async<T, Fut, S>(message: S, work: Fut) -> std::io::Result<T>
where
    Fut: Future<Output = T> + Send,
    T: Send,
    S: Into<String>,
{
    if !(std::io::stdout().is_terminal() && std::io::stderr().is_terminal()) {
        return Ok(work.await);
    }

    let handle = tokio::runtime::Handle::current();
    DemandSpinner::new(message)
        .theme(&DEMAND_THEME)
        .run(move |_| handle.block_on(work))
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
        let answer = confirm("Proceed?", false).unwrap();
        assert!(!answer);
    }

    #[test]
    fn prompt_input_uses_default_in_non_tty_context() {
        let value = prompt_input("Server host", false, Some("localhost")).unwrap();
        assert_eq!(value, "localhost");
    }

    #[test]
    fn emphasized_falls_back_to_quoted_text_in_non_tty_context() {
        assert_eq!(emphasized("production"), "'production'");
    }

    #[test]
    fn prompt_input_without_default_errors_in_non_tty_context() {
        let err = prompt_input("Server host", false, None).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn prompt_input_with_suggestions_uses_default_in_non_tty_context() {
        let suggestions = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let value =
            prompt_input_with_suggestions("Server host", false, Some("localhost"), &suggestions)
                .unwrap();
        assert_eq!(value, "localhost");
    }

    #[test]
    fn prompt_input_with_suggestions_without_default_errors_in_non_tty_context() {
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
        let err = select("Pick one", None, vec![("server-a".to_string(), 1)]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn demand_theme_uses_terminal_color_semantics() {
        assert_eq!(DEMAND_THEME.title.fg(), Some(&Color::Cyan));
        assert_eq!(DEMAND_THEME.description.fg(), Some(&Color::Cyan));
        assert_eq!(DEMAND_THEME.input_prompt.fg(), Some(&Color::Cyan));
        assert_eq!(DEMAND_THEME.blurred_button.bg(), None);
        assert_eq!(DEMAND_THEME.error_indicator.fg(), Some(&Color::Red));
    }

    #[test]
    fn with_spinner_async_runs_future_in_non_tty_context() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let value = rt
            .block_on(with_spinner_async("Working...", async { 42usize }))
            .expect("spinner result");
        assert_eq!(value, 42);
    }
}
