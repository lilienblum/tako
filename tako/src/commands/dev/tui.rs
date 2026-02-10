//! Dev TUI (Ratatui)
//!
//! Interactive dashboard for `tako dev`.

use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton,
    MouseEventKind,
};
use crossterm::terminal::SetTitle;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::block::Padding;
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

use sysinfo::{Pid, ProcessesToUpdate, System};

use super::DevEvent;
use super::LogLevel;
use super::ScopedLog;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCmd {
    Restart,
    Terminate,
    ClearLogs,
}

struct TuiGuard {
    stdout: Stdout,
}

impl TuiGuard {
    fn enter() -> io::Result<Self> {
        let mut stdout = io::stdout();
        terminal::enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self { stdout })
    }

    fn backend(&mut self) -> CrosstermBackend<&mut Stdout> {
        CrosstermBackend::new(&mut self.stdout)
    }

    fn set_title(&mut self, title: &str) {
        let _ = execute!(self.stdout, SetTitle(title));
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        let _ = execute!(self.stdout, SetTitle("tako"));
        let _ = execute!(self.stdout, DisableMouseCapture, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Debug, Clone, Copy)]
enum AppState {
    Starting,
    Launching,
    Running,
    Stopped,
    Error,
    Restarting,
}

#[derive(Debug, Clone, Copy)]
struct Theme {
    bg: Color,
    panel_bg: Color,
    fg: Color,
    secondary: Color,
    muted: Color,
    label_muted: Color,
    accent: Color,
    selection_bg: Color,
    cursor_bg: Color,
    log_debug: Color,
    log_info: Color,
    log_warn: Color,
    log_error: Color,
    log_fatal: Color,
}

// Default theme.
const TAKO_BRAND: Theme = Theme {
    bg: Color::Rgb(0x13, 0x15, 0x17),
    panel_bg: Color::Rgb(0x1B, 0x1F, 0x22),
    fg: Color::Rgb(0xF2, 0xEC, 0xEA),
    secondary: Color::Rgb(0x9B, 0xC4, 0xB6),
    muted: Color::Rgb(0x94, 0xA3, 0xB8),
    label_muted: Color::Rgb(0x64, 0x74, 0x8B),
    accent: Color::Rgb(0xE8, 0x87, 0x83),
    selection_bg: Color::Rgb(0x2B, 0x3A, 0x37),
    cursor_bg: Color::Rgb(0x4C, 0x35, 0x35),
    // Pastel log-level colors derived to fit Tako's dark UI palette.
    log_debug: Color::Rgb(0x8C, 0xCF, 0xFF), // electric blue
    log_info: Color::Rgb(0x9B, 0xD9, 0xB3),  // green
    log_warn: Color::Rgb(0xEA, 0xD3, 0x9C),  // yellow
    log_error: Color::Rgb(0xE8, 0xA3, 0xA0), // red
    log_fatal: Color::Rgb(0xC8, 0xA6, 0xF2), // purple
};

const STARTUP_TAKO_ASCII: [&str; 3] = ["███ ███ █ █ ███", " █  █▄█ ██  █ █", " █  █ █ █ █ ███"];
const STARTUP_TAKO_ASCII_FG: [Color; 3] = [
    Color::Rgb(0xF2, 0xF2, 0xF2),
    Color::Rgb(0xCD, 0xCD, 0xCD),
    Color::Rgb(0xA6, 0xA6, 0xA6),
];

const LOADING_SPINNER_FRAMES: [char; 4] = ['|', '/', '-', '\\'];
const BANNER_ANIMATION_FRAME_MS: u64 = 120;
const TOP_LEFT_STATUS_LINES: u16 = 3;
const HEADER_PANEL_VERTICAL_PADDING: u16 = 2;
const LAYOUT_SPACING: u16 = 1;
const PANEL_GAP_WIDTH: u16 = LAYOUT_SPACING;
const GLOBAL_PADDING_HORIZONTAL: u16 = LAYOUT_SPACING;
const GLOBAL_PADDING_VERTICAL: u16 = LAYOUT_SPACING;
const TOP_LEFT_BANNER_GAP_WIDTH: u16 = LAYOUT_SPACING;
const TOP_LEFT_INFO_MIN_WIDTH_WITH_LOGO: u16 = 24;
const PANEL_INNER_PADDING: u16 = LAYOUT_SPACING;
const LOGS_CAPTION: &str = "Logs";
const LOGS_CAPTION_ALIGNMENT: Alignment = Alignment::Left;
const LOGS_HEADER_PADDING_ROWS: usize = 1;
const HEADER_CONTROL_CLIENTS_LABEL: &str = "Sessions";

fn show_startup_tako_banner(_state: AppState) -> bool {
    true
}

fn banner_animation_frame(started_at: Instant) -> usize {
    let elapsed_ms = Instant::now()
        .saturating_duration_since(started_at)
        .as_millis() as usize;
    elapsed_ms / BANNER_ANIMATION_FRAME_MS as usize
}

fn loading_logs_hint(log_count: usize, logs_loading: bool, frame: usize) -> Option<String> {
    if log_count != 0 {
        return None;
    }

    if logs_loading {
        Some(format!(
            "Loading logs... {}",
            LOADING_SPINNER_FRAMES[frame % LOADING_SPINNER_FRAMES.len()]
        ))
    } else {
        Some("Waiting for logs...".to_string())
    }
}

fn format_tui_public_url(host: &str, port: u16) -> String {
    if port == 443 {
        format!("https://{}", host)
    } else {
        format!("https://{}:{}", host, port)
    }
}

fn format_tui_local_url(port: u16) -> String {
    format!("http://localhost:{}", port)
}

fn logs_fixed_rows(show_loading_hint: bool) -> usize {
    // Caption row + optional loading hint row.
    LOGS_HEADER_PADDING_ROWS + usize::from(show_loading_hint)
}

fn top_header_height(show_banner: bool) -> u16 {
    let banner_lines = if show_banner {
        STARTUP_TAKO_ASCII.len() as u16
    } else {
        0
    };

    banner_lines.max(TOP_LEFT_STATUS_LINES) + HEADER_PANEL_VERTICAL_PADDING
}

fn header_status_text(app_state: AppState) -> &'static str {
    match app_state {
        AppState::Starting => "not started",
        AppState::Launching => "starting...",
        AppState::Stopped => "stopped (idle)",
        AppState::Running => "running",
        AppState::Restarting => "restarting",
        AppState::Error => "failed",
    }
}

fn header_status_style(theme: &Theme, app_state: AppState) -> Style {
    match app_state {
        AppState::Running | AppState::Restarting | AppState::Error => Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
        AppState::Starting | AppState::Launching | AppState::Stopped => {
            Style::default().fg(theme.fg)
        }
    }
}

fn header_adapter_text(adapter_name: &str) -> String {
    let trimmed = adapter_name.trim();
    if trimmed.is_empty() {
        "application".to_string()
    } else {
        format!("{trimmed} application")
    }
}

fn banner_logo_width() -> u16 {
    STARTUP_TAKO_ASCII
        .iter()
        .map(|line| line.chars().count() as u16)
        .max()
        .unwrap_or(0)
}

fn should_hide_banner_logo(area_width: u16) -> bool {
    let min_width_with_logo = banner_logo_width()
        .saturating_add(TOP_LEFT_BANNER_GAP_WIDTH)
        .saturating_add(TOP_LEFT_INFO_MIN_WIDTH_WITH_LOGO);
    area_width < min_width_with_logo
}

fn top_left_panel_columns(area: Rect, show_banner: bool) -> [Rect; 3] {
    if !show_banner || should_hide_banner_logo(area.width) {
        return [Rect::default(), Rect::default(), area];
    }

    let logo_width = banner_logo_width();
    let gap_width = TOP_LEFT_BANNER_GAP_WIDTH;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(logo_width),
            Constraint::Length(gap_width),
            Constraint::Min(1),
        ])
        .split(area);

    [chunks[0], chunks[1], chunks[2]]
}

#[derive(Debug, Clone, Copy, Default)]
struct Metrics {
    app_cpu: Option<f32>,
    app_mem_bytes: Option<u64>,
    control_clients: Option<u32>,
}

fn fmt_bytes(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{} MB", bytes / MB)
    }
}

fn fmt_cpu(cpu: f32) -> String {
    // sysinfo already returns percentage in 0..100-ish
    format!("{:.0}%", cpu)
}

fn fmt_control_clients(control_clients: Option<u32>) -> String {
    control_clients
        .map(|count| count.to_string())
        .unwrap_or_else(|| "—".to_string())
}

fn parse_control_clients_from_info(info: &serde_json::Value) -> Option<u32> {
    info.get("info")
        .and_then(|v| v.get("control_clients"))
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
}

fn right_aligned_metric_row(
    label: &str,
    value: &str,
    content_width: u16,
    label_style: Style,
    value_style: Style,
) -> Line<'static> {
    let label_width: u16 = label.chars().map(char_width).sum();
    let value_width: u16 = value.chars().map(char_width).sum();
    let gap_width = if content_width > label_width.saturating_add(value_width) {
        content_width - label_width - value_width
    } else {
        1
    };

    Line::from(vec![
        Span::styled(label.to_string(), label_style),
        Span::raw(" ".repeat(gap_width as usize)),
        Span::styled(value.to_string(), value_style),
    ])
}

#[derive(Debug, Clone)]
struct DisplayLogLine {
    text: String,
    level: Option<LogLevel>,
    source_log_idx: usize,
    muted: bool,
}

fn format_scoped_log_prefix(l: &ScopedLog) -> String {
    format!(
        "{:02}:{:02}:{:02} {:<5} [{}] ",
        l.h, l.m, l.s, l.level, l.scope
    )
}

fn format_scoped_log_line(l: &ScopedLog) -> String {
    // Current time (local).
    format!("{}{}", format_scoped_log_prefix(l), l.message)
}

fn repeated_occurrence_summary(extra_count: usize) -> String {
    if extra_count == 1 {
        "also 1 more time".to_string()
    } else {
        format!("also {} more times", extra_count)
    }
}

fn same_log_record(a: &ScopedLog, b: &ScopedLog) -> bool {
    std::mem::discriminant(&a.level) == std::mem::discriminant(&b.level)
        && a.scope == b.scope
        && a.message == b.message
}

fn build_display_log_lines(logs: &[&ScopedLog]) -> Vec<DisplayLogLine> {
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < logs.len() {
        let first = logs[i];
        let mut j = i + 1;
        while j < logs.len() && same_log_record(first, logs[j]) {
            j += 1;
        }

        out.push(DisplayLogLine {
            text: format_scoped_log_line(first),
            level: Some(first.level.clone()),
            source_log_idx: i,
            muted: false,
        });

        let extra_count = j.saturating_sub(i + 1);
        if extra_count > 0 {
            let indent = " ".repeat(format_scoped_log_prefix(first).chars().count());
            out.push(DisplayLogLine {
                text: format!("{indent}{}", repeated_occurrence_summary(extra_count)),
                level: None,
                source_log_idx: i,
                muted: true,
            });
        }

        i = j;
    }

    out
}

fn visible_logs_for_display(state: &UiState) -> Vec<&ScopedLog> {
    state.logs.iter().rev().take(1_000).rev().collect()
}

fn log_level_color(level: &LogLevel, t: Theme) -> Color {
    match level {
        LogLevel::Debug => t.log_debug,
        LogLevel::Info => t.log_info,
        LogLevel::Warn => t.log_warn,
        LogLevel::Error => t.log_error,
        LogLevel::Fatal => t.log_fatal,
    }
}

fn log_level_field_range(line: &str) -> Option<(usize, usize)> {
    let first_space = line.find(' ')?;
    let mut level_start = None;
    let after_first_space = first_space + 1;

    for (offset, ch) in line[after_first_space..].char_indices() {
        let idx = after_first_space + offset;
        if level_start.is_none() {
            if !ch.is_whitespace() {
                level_start = Some(idx);
            }
            continue;
        }

        if ch.is_whitespace() {
            return level_start.map(|start| (start, idx));
        }
    }

    level_start.map(|start| (start, line.len()))
}

fn log_timestamp_field_range(line: &str) -> Option<(usize, usize)> {
    let first_space = line.find(' ')?;
    (first_space > 0).then_some((0, first_space))
}

fn intersect_ranges(a: (usize, usize), b: (usize, usize)) -> Option<(usize, usize)> {
    let start = a.0.max(b.0);
    let end = a.1.min(b.1);
    (start < end).then_some((start, end))
}

fn build_log_line_spans(
    line: &str,
    row_range: (usize, usize),
    timestamp_range: Option<(usize, usize)>,
    level_range: Option<(usize, usize)>,
    base_fg: Color,
    timestamp_fg: Color,
    level_fg: Color,
    highlight_range: Option<(usize, usize)>,
    highlight_bg: Option<Color>,
) -> Vec<Span<'static>> {
    let timestamp_in_row = timestamp_range.and_then(|r| intersect_ranges(r, row_range));
    let level_in_row = level_range.and_then(|r| intersect_ranges(r, row_range));
    let highlight_in_row = highlight_range.and_then(|r| intersect_ranges(r, row_range));

    let mut cuts = vec![row_range.0, row_range.1];
    if let Some((start, end)) = timestamp_in_row {
        cuts.push(start);
        cuts.push(end);
    }
    if let Some((start, end)) = level_in_row {
        cuts.push(start);
        cuts.push(end);
    }
    if let Some((start, end)) = highlight_in_row {
        cuts.push(start);
        cuts.push(end);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut spans = Vec::new();
    for pair in cuts.windows(2) {
        let seg_start = pair[0];
        let seg_end = pair[1];
        if seg_start >= seg_end {
            continue;
        }

        let text = line.get(seg_start..seg_end).unwrap_or("");
        if text.is_empty() {
            continue;
        }

        let in_timestamp = timestamp_in_row
            .map(|(start, end)| seg_start >= start && seg_end <= end)
            .unwrap_or(false);
        let in_level = level_in_row
            .map(|(start, end)| seg_start >= start && seg_end <= end)
            .unwrap_or(false);
        let fg = if in_level {
            level_fg
        } else if in_timestamp {
            timestamp_fg
        } else {
            base_fg
        };
        let mut style = Style::default().fg(fg);

        if let (Some((start, end)), Some(bg)) = (highlight_in_row, highlight_bg)
            && seg_start >= start
            && seg_end <= end
        {
            style = style.bg(bg);
        }

        spans.push(Span::styled(text.to_string(), style));
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), Style::default().fg(base_fg)));
    }

    spans
}

const METRICS_REFRESH_SECS: u64 = 2;
const TERMINATE_CONFIRM_SECS: u64 = 3;

struct UiState {
    app_state: AppState,
    logs: VecDeque<ScopedLog>,
    logs_loading: bool,
    // Scroll is in visual rows (after wrapping), not original log lines.
    log_scroll: usize,
    // Cursor/selection is also tracked in visual row space.
    log_selected: usize,
    metrics: Metrics,

    app_pid: Option<Pid>,

    public_urls: Vec<String>,
    local_url: String,

    public_url_flash: Option<(usize, Instant)>,

    last_visible_rows: usize,
    last_total_rows: usize,

    follow_end: bool,

    mouse_selecting: bool,
    // Anchor/head positions are in (visual_row_index, col) space.
    sel_anchor: Option<(usize, u16)>,
    sel_head: Option<(usize, u16)>,
    last_mouse: Option<(u16, u16)>,

    resize_pending: bool,
    last_resize_at: Option<Instant>,

    status_msg: Option<(String, Instant)>,
    terminate_confirm_until: Option<Instant>,
    banner_started_at: Instant,
}

impl UiState {
    fn new() -> Self {
        Self {
            app_state: AppState::Starting,
            logs: VecDeque::new(),
            logs_loading: true,
            log_scroll: 0,
            log_selected: 0,
            metrics: Metrics::default(),

            app_pid: None,

            public_urls: Vec::new(),
            local_url: String::new(),

            public_url_flash: None,

            last_visible_rows: 0,
            last_total_rows: 0,

            follow_end: true,

            mouse_selecting: false,
            sel_anchor: None,
            sel_head: None,
            last_mouse: None,

            resize_pending: false,
            last_resize_at: None,
            status_msg: None,
            terminate_confirm_until: None,
            banner_started_at: Instant::now(),
        }
    }

    fn flash_public_url(&mut self, idx: usize, ttl: Duration) {
        self.public_url_flash = Some((idx, Instant::now() + ttl));
    }

    fn clear_expired_flash(&mut self) {
        if let Some((_, until)) = &self.public_url_flash
            && Instant::now() >= *until
        {
            self.public_url_flash = None;
        }
    }

    fn push_log(&mut self, line: ScopedLog) {
        const MAX: usize = 2_000;
        let was_at_bottom = self.follow_end && self.log_selected + 1 >= self.last_total_rows;
        if self.logs.len() >= MAX {
            self.logs.pop_front();
            // Keep the visual cursor approximately stable.
            self.log_selected = self.log_selected.saturating_sub(1);
            self.log_scroll = self.log_scroll.saturating_sub(1);
        }
        self.logs.push_back(line);
        if was_at_bottom {
            // We'll clamp to the real bottom in the next render when we know total visual rows.
            self.follow_end = true;
        }
    }

    fn set_status(&mut self, msg: impl Into<String>, ttl: Duration) {
        self.status_msg = Some((msg.into(), Instant::now() + ttl));
    }

    fn clear_expired_status(&mut self) {
        if let Some((_, until)) = &self.status_msg
            && Instant::now() >= *until
        {
            self.status_msg = None;
        }
    }

    fn terminate_confirmation_pending(&mut self) -> bool {
        if let Some(until) = self.terminate_confirm_until {
            if Instant::now() < until {
                return true;
            }
            self.terminate_confirm_until = None;
        }
        false
    }
}

fn spawn_event_reader(tx: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if tx.blocking_send(ev).is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
fn layout_chunks(area: Rect) -> [Rect; 4] {
    layout_chunks_for_banner(area, false)
}

fn layout_chunks_for_banner(area: Rect, show_banner: bool) -> [Rect; 4] {
    // Global padding so the UI doesn't feel glued to the terminal edges.
    let inner = area.inner(Margin {
        horizontal: GLOBAL_PADDING_HORIZONTAL,
        vertical: GLOBAL_PADDING_VERTICAL,
    });

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        // Header + gap + logs + footer.
        .constraints([
            Constraint::Length(top_header_height(show_banner)),
            Constraint::Length(PANEL_GAP_WIDTH),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(inner);

    [chunks[0], chunks[1], chunks[2], chunks[3]]
}

fn panel_padding_all_sides() -> Padding {
    Padding::new(
        PANEL_INNER_PADDING,
        PANEL_INNER_PADDING,
        PANEL_INNER_PADDING,
        PANEL_INNER_PADDING,
    )
}

fn panel_inner_margin() -> Margin {
    Margin {
        horizontal: PANEL_INNER_PADDING,
        vertical: PANEL_INNER_PADDING,
    }
}

fn header_left_inner_area(area: Rect) -> Rect {
    area.inner(panel_inner_margin())
}

const HEADER_METRICS_COL_WIDTH: u16 = 16;

fn header_panel_chunks(area: Rect) -> [Rect; 3] {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(PANEL_GAP_WIDTH),
            Constraint::Length(HEADER_METRICS_COL_WIDTH),
        ])
        .split(area);
    [chunks[0], chunks[1], chunks[2]]
}

const FOOTER_HINTS_PREFERRED_WIDTH: u16 = 64;

fn footer_right_width(total_width: u16) -> u16 {
    total_width
        .saturating_sub(1)
        .min(FOOTER_HINTS_PREFERRED_WIDTH)
}

fn footer_chunks(area: Rect) -> [Rect; 2] {
    let right_width = footer_right_width(area.width);
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(right_width)])
        .split(area);
    [chunks[0], chunks[1]]
}

fn log_panel_chunks(area: Rect) -> [Rect; 2] {
    // Keep a 2-way split to avoid re-threading all layout helpers, but make the
    // left rail zero-width (no visible border).
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(0), Constraint::Min(1)])
        .split(area);
    [chunks[0], chunks[1]]
}

fn logs_panel_block(t: Theme) -> Block<'static> {
    Block::default()
        .padding(Padding::new(PANEL_INNER_PADDING, PANEL_INNER_PADDING, 0, 0))
        .style(Style::default().bg(t.panel_bg))
}

fn logs_caption_area(text_area: Rect) -> Rect {
    Rect::new(
        text_area.x,
        text_area.y,
        text_area.width,
        text_area.height.min(1),
    )
}

fn clamp_scroll(total: usize, scroll: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    let max_scroll = total.saturating_sub(height);
    scroll.min(max_scroll)
}

fn ensure_visible(selected: usize, scroll: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    if selected < scroll {
        return selected;
    }
    if selected >= scroll + height {
        return selected + 1 - height;
    }
    scroll
}

#[derive(Debug, Clone)]
struct VisualRow {
    log_idx: usize,
    start: usize,
    end: usize,
}

fn char_width(ch: char) -> u16 {
    UnicodeWidthChar::width(ch).unwrap_or(0).max(1) as u16
}

fn wrap_ranges(s: &str, max_width: u16) -> Vec<(usize, usize)> {
    if max_width == 0 {
        return vec![(0, s.len())];
    }

    let mut out = Vec::new();
    let mut start = 0usize;
    let mut width = 0u16;
    for (i, ch) in s.char_indices() {
        let w = char_width(ch);
        if width.saturating_add(w) > max_width && i > start {
            out.push((start, i));
            start = i;
            width = 0;
        }
        width = width.saturating_add(w);
    }

    out.push((start, s.len()));
    out
}

fn build_visual_rows(logs: &[&str], max_width: u16) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    for (idx, line) in logs.iter().enumerate() {
        for (start, end) in wrap_ranges(line, max_width) {
            rows.push(VisualRow {
                log_idx: idx,
                start,
                end,
            });
        }
    }
    rows
}

fn col_to_byte(s: &str, col: u16) -> usize {
    let mut w = 0u16;
    for (i, ch) in s.char_indices() {
        if w >= col {
            return i;
        }
        w = w.saturating_add(char_width(ch));
    }
    s.len()
}

fn pos_to_text(logs: &[&str], rows: &[VisualRow], vrow: usize, col: u16) -> Option<(usize, usize)> {
    let r = rows.get(vrow)?;
    let line = *logs.get(r.log_idx)?;
    let slice = &line[r.start..r.end];
    let off = col_to_byte(slice, col);
    Some((r.log_idx, r.start + off))
}

fn normalize_text_range(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if a.0 < b.0 {
        (a, b)
    } else if a.0 > b.0 {
        (b, a)
    } else if a.1 <= b.1 {
        (a, b)
    } else {
        (b, a)
    }
}

fn selection_for_row(
    row: &VisualRow,
    start: (usize, usize),
    end: (usize, usize),
) -> Option<(usize, usize)> {
    if row.log_idx < start.0 || row.log_idx > end.0 {
        return None;
    }

    let mut s = row.start;
    let mut e = row.end;

    if row.log_idx == start.0 {
        s = s.max(start.1);
    }
    if row.log_idx == end.0 {
        e = e.min(end.1);
    }

    if s >= e { None } else { Some((s, e)) }
}

fn extract_selection_text(logs: &[&str], start: (usize, usize), end: (usize, usize)) -> String {
    let (start, end) = normalize_text_range(start, end);
    if start.0 == end.0 {
        let line = *logs.get(start.0).unwrap_or(&"");
        return line.get(start.1..end.1).unwrap_or("").to_string();
    }

    let mut out = String::new();
    for idx in start.0..=end.0 {
        let line = *logs.get(idx).unwrap_or(&"");
        if idx == start.0 {
            out.push_str(line.get(start.1..).unwrap_or(""));
        } else if idx == end.0 {
            out.push_str(line.get(..end.1).unwrap_or(""));
        } else {
            out.push_str(line);
        }
        if idx != end.0 {
            out.push('\n');
        }
    }
    out
}

#[cfg(not(test))]
fn copy_to_clipboard(text: &str) {
    if text.is_empty() {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        let mut child = match std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }

    #[cfg(target_os = "linux")]
    {
        for (cmd, args) in [
            ("wl-copy", &[][..]),
            ("xclip", &["-selection", "clipboard"][..]),
        ] {
            let mut child = match std::process::Command::new(cmd)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => continue,
            };
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().ok().is_some() {
                break;
            }
        }
        return;
    }
}

#[cfg(test)]
fn copy_to_clipboard(_text: &str) {}

fn public_url_style(theme: &Theme, is_flashing: bool) -> Style {
    let mut style = Style::default()
        .fg(theme.secondary)
        .add_modifier(Modifier::UNDERLINED);

    if is_flashing {
        style = style.bg(theme.selection_bg).fg(theme.fg);
    }

    style
}

fn footer_hint_spans(theme: Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            "q",
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" quit   ", Style::default().fg(theme.muted)),
        Span::styled(
            "r",
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" restart   ", Style::default().fg(theme.muted)),
        Span::styled(
            "t",
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" terminate   ", Style::default().fg(theme.muted)),
        Span::styled(
            "e",
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" end   ", Style::default().fg(theme.muted)),
        Span::styled(
            "enter",
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" copy   ", Style::default().fg(theme.muted)),
        Span::styled(
            "c",
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" clear", Style::default().fg(theme.muted)),
    ]
}

#[cfg(test)]
fn footer_hint_text() -> String {
    footer_hint_spans(TAKO_BRAND)
        .into_iter()
        .map(|s| s.content.into_owned())
        .collect()
}

fn matches_host_in_url(url: &str, host: &str) -> bool {
    let Some(without_scheme) = url.strip_prefix("https://") else {
        return false;
    };
    without_scheme == host
        || without_scheme
            .strip_prefix(host)
            .is_some_and(|rest| rest.starts_with(':') || rest.starts_with('/'))
}

fn selected_public_url_index(urls: &[String], app_name: &str) -> Option<usize> {
    if urls.is_empty() {
        return None;
    }

    let default_host = format!("{}.tako.local", app_name);
    urls.iter()
        .position(|url| matches_host_in_url(url, &default_host))
        .or(Some(0))
}

fn render(
    terminal: &mut Terminal<CrosstermBackend<&mut Stdout>>,
    app_name: &str,
    adapter_name: &str,
    state: &mut UiState,
) -> io::Result<()> {
    terminal.draw(|f| {
        let t = TAKO_BRAND;
        let show_banner = show_startup_tako_banner(state.app_state);

        let [header_area, gap_area, body_area, footer_area] =
            layout_chunks_for_banner(f.area(), show_banner);

        f.render_widget(Block::default().style(Style::default().bg(t.bg)), f.area());
        f.render_widget(Clear, header_area);
        f.render_widget(Block::default().style(Style::default().bg(t.bg)), gap_area);

        let [header_left_area, header_gap_area, header_right_area] =
            header_panel_chunks(header_area);
        f.render_widget(
            Block::default().style(Style::default().bg(t.bg)),
            header_gap_area,
        );
        let spinner_frame = banner_animation_frame(state.banner_started_at);

        let flashing_idx = state
            .public_url_flash
            .and_then(|(idx, until)| (Instant::now() < until).then_some(idx));

        let mut public_url_spans: Vec<Span> = Vec::new();
        if let Some(primary_idx) = selected_public_url_index(&state.public_urls, app_name) {
            let primary = state
                .public_urls
                .get(primary_idx)
                .cloned()
                .unwrap_or_default();
            let style = public_url_style(&t, flashing_idx == Some(primary_idx));
            public_url_spans.push(Span::styled(primary, style));

            let more_count = state.public_urls.len().saturating_sub(1);
            if more_count > 0 {
                public_url_spans.push(Span::raw(" "));
                public_url_spans.push(Span::styled(
                    format!("(and {} more)", more_count),
                    Style::default().fg(t.muted),
                ));
            }
        }
        let public_urls_line = if public_url_spans.is_empty() {
            Line::from(Span::raw(""))
        } else {
            Line::from(public_url_spans)
        };

        let app_title = Span::styled(
            app_name,
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        );

        let app_cpu = state
            .metrics
            .app_cpu
            .map(fmt_cpu)
            .unwrap_or_else(|| "—".to_string());
        let app_mem = state
            .metrics
            .app_mem_bytes
            .map(fmt_bytes)
            .unwrap_or_else(|| "—".to_string());
        let control_clients = fmt_control_clients(state.metrics.control_clients);

        let app_line = Line::from(vec![
            app_title,
            Span::raw(" "),
            Span::styled(
                header_status_text(state.app_state),
                header_status_style(&t, state.app_state),
            ),
        ]);
        let adapter_line = Line::from(vec![Span::styled(
            header_adapter_text(adapter_name),
            Style::default().fg(t.muted),
        )]);

        let header_left_block = Block::default()
            .padding(panel_padding_all_sides())
            .style(Style::default().bg(t.panel_bg));
        let header_left_inner = header_left_inner_area(header_left_area);
        f.render_widget(header_left_block, header_left_area);

        let [logo_area, logo_gap_area, info_area] =
            top_left_panel_columns(header_left_inner, show_banner);
        if show_banner {
            f.render_widget(
                Block::default().style(Style::default().bg(t.panel_bg)),
                logo_gap_area,
            );

            let logo_lines: Vec<Line> = STARTUP_TAKO_ASCII
                .iter()
                .enumerate()
                .map(|(row, line)| {
                    let fg = STARTUP_TAKO_ASCII_FG.get(row).copied().unwrap_or(t.fg);
                    Line::from(Span::styled(
                        *line,
                        Style::default().fg(fg).add_modifier(Modifier::BOLD),
                    ))
                })
                .collect();
            let logo = Paragraph::new(Text::from(logo_lines)).alignment(Alignment::Left);
            f.render_widget(logo, logo_area);
        }

        let info_box = Paragraph::new(Text::from(vec![app_line, adapter_line, public_urls_line]));
        f.render_widget(info_box, info_area);

        let metrics_block = Block::default()
            .padding(panel_padding_all_sides())
            .style(Style::default().bg(t.panel_bg));
        let metrics_content_width = metrics_block.inner(header_right_area).width;
        let metrics_box = Paragraph::new(Text::from(vec![
            right_aligned_metric_row(
                "CPU:",
                &app_cpu,
                metrics_content_width,
                Style::default().fg(t.label_muted),
                Style::default().fg(t.fg),
            ),
            right_aligned_metric_row(
                "RAM:",
                &app_mem,
                metrics_content_width,
                Style::default().fg(t.label_muted),
                Style::default().fg(t.fg),
            ),
            right_aligned_metric_row(
                &format!("{HEADER_CONTROL_CLIENTS_LABEL}:"),
                &control_clients,
                metrics_content_width,
                Style::default().fg(t.label_muted),
                Style::default().fg(t.fg),
            ),
        ]))
        .block(metrics_block);
        f.render_widget(metrics_box, header_right_area);

        // Logs panel
        f.render_widget(
            Block::default().style(Style::default().bg(t.panel_bg)),
            body_area,
        );

        let [_logs_bar, logs_content] = log_panel_chunks(body_area);
        let logs_block = logs_panel_block(t);
        let logs_text_area = logs_block.inner(logs_content);

        let visible_logs = visible_logs_for_display(state);
        let display_logs = build_display_log_lines(&visible_logs);

        let loading_hint = loading_logs_hint(display_logs.len(), state.logs_loading, spinner_frame);

        let content_w = logs_text_area.width;
        let content_h = logs_text_area.height as usize;
        let fixed_rows = logs_fixed_rows(loading_hint.is_some());
        let visible_rows = content_h.saturating_sub(fixed_rows);

        let all_logs_ref: Vec<&str> = display_logs.iter().map(|l| l.text.as_str()).collect();
        let rows = build_visual_rows(&all_logs_ref, content_w);
        let total_rows = rows.len();

        let mut selected = state.log_selected.min(total_rows.saturating_sub(1));
        let mut scroll = clamp_scroll(total_rows, state.log_scroll, visible_rows);

        if state.follow_end && total_rows > 0 {
            selected = total_rows - 1;
            scroll = clamp_scroll(
                total_rows,
                total_rows.saturating_sub(visible_rows),
                visible_rows,
            );
        }
        scroll = ensure_visible(selected, scroll, visible_rows);

        state.last_visible_rows = visible_rows;
        state.last_total_rows = total_rows;
        state.log_scroll = scroll;
        state.log_selected = selected;

        let mut lines: Vec<Line> = Vec::new();
        for _ in 0..LOGS_HEADER_PADDING_ROWS {
            lines.push(Line::from(Span::raw("")));
        }
        if let Some(hint) = loading_hint {
            lines.push(Line::from(Span::styled(hint, Style::default().fg(t.muted))));
        }

        // Selection (mouse drag) highlighting.
        let selection = match (state.sel_anchor, state.sel_head) {
            (Some(a), Some(b)) => {
                let a = pos_to_text(
                    &all_logs_ref,
                    &rows,
                    a.0.min(total_rows.saturating_sub(1)),
                    a.1,
                );
                let b = pos_to_text(
                    &all_logs_ref,
                    &rows,
                    b.0.min(total_rows.saturating_sub(1)),
                    b.1,
                );
                match (a, b) {
                    (Some(a), Some(b)) => Some(normalize_text_range(a, b)),
                    _ => None,
                }
            }
            _ => None,
        };

        let end = (scroll + visible_rows).min(total_rows);
        for (vrow_idx, row) in rows.iter().enumerate().take(end).skip(scroll) {
            let line = all_logs_ref.get(row.log_idx).copied().unwrap_or("");
            let line_meta = display_logs.get(row.log_idx);
            let base_fg = if line_meta.is_some_and(|meta| meta.muted) {
                t.muted
            } else {
                t.fg
            };
            let level_fg = line_meta
                .and_then(|meta| meta.level.as_ref())
                .map(|level| log_level_color(level, t))
                .unwrap_or(base_fg);
            let timestamp_range = line_meta
                .and_then(|meta| meta.level.as_ref())
                .and_then(|_| log_timestamp_field_range(line));
            let level_range = line_meta
                .and_then(|meta| meta.level.as_ref())
                .and_then(|_| log_level_field_range(line));
            let row_range = (row.start, row.end);

            let spans: Vec<Span> = if let Some((sel_start, sel_end)) = selection {
                build_log_line_spans(
                    line,
                    row_range,
                    timestamp_range,
                    level_range,
                    base_fg,
                    t.muted,
                    level_fg,
                    selection_for_row(row, sel_start, sel_end),
                    Some(t.selection_bg),
                )
            } else {
                let is_cursor = vrow_idx == selected;
                build_log_line_spans(
                    line,
                    row_range,
                    timestamp_range,
                    level_range,
                    base_fg,
                    t.muted,
                    level_fg,
                    is_cursor.then_some(row_range),
                    Some(t.cursor_bg),
                )
            };

            lines.push(Line::from(spans));
        }

        let logs = Paragraph::new(Text::from(lines)).block(logs_block);
        f.render_widget(logs, logs_content);

        let caption_area = logs_caption_area(logs_text_area);
        if caption_area.height > 0 {
            let caption = Paragraph::new(Line::from(Span::styled(
                LOGS_CAPTION,
                Style::default()
                    .fg(t.accent)
                    .bg(t.panel_bg)
                    .add_modifier(Modifier::BOLD),
            )))
            .alignment(LOGS_CAPTION_ALIGNMENT);
            f.render_widget(caption, caption_area);
        }

        // Footer helper (status left, hotkeys right)
        let [footer_left_area, footer_right_area] = footer_chunks(footer_area);
        f.render_widget(
            Block::default().style(Style::default().bg(t.bg)),
            footer_area,
        );

        let left = state
            .status_msg
            .as_ref()
            .map(|(m, _)| m.as_str())
            .unwrap_or("");

        let footer_left = Paragraph::new(Span::styled(left.to_string(), Style::default().fg(t.fg)))
            .wrap(Wrap { trim: true });
        f.render_widget(footer_left, footer_left_area);

        let footer_right = Paragraph::new(Line::from(footer_hint_spans(t)))
            .alignment(Alignment::Right)
            .wrap(Wrap { trim: true });
        f.render_widget(footer_right, footer_right_area);
    })?;

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuiAction {
    None,
    Quit,
    Restart,
    Terminate,
    ClearLogs,
}

fn clear_selection(state: &mut UiState) {
    state.sel_anchor = None;
    state.sel_head = None;
    state.mouse_selecting = false;
}

fn jump_to_log_end(state: &mut UiState) {
    clear_selection(state);
    state.follow_end = true;
    state.log_selected = state.last_total_rows.saturating_sub(1);
    state.log_scroll = ensure_visible(
        state.log_selected,
        state.log_scroll,
        state.last_visible_rows,
    );
}

fn clear_logs_state(state: &mut UiState) {
    state.logs.clear();
    state.log_scroll = 0;
    state.log_selected = 0;
    clear_selection(state);
    state.follow_end = true;
}

fn logs_content_rect(area: Rect, show_banner: bool) -> Rect {
    let [_header, _gap, logs_area, _footer] = layout_chunks_for_banner(area, show_banner);
    let [_rail, content] = log_panel_chunks(logs_area);
    logs_panel_block(TAKO_BRAND).inner(content)
}

fn hit_test_public_url(
    state: &UiState,
    area: Rect,
    col: u16,
    row: u16,
    app_name: &str,
) -> Option<usize> {
    if state.public_urls.is_empty() {
        return None;
    }

    let show_banner = show_startup_tako_banner(state.app_state);
    let [header_area, _gap, _body, _footer] = layout_chunks_for_banner(area, show_banner);
    let [header_left_area, _header_gap_area, _header_right_area] = header_panel_chunks(header_area);
    if header_left_area.width < 3 || header_left_area.height < 3 {
        return None;
    }
    let inner = header_left_inner_area(header_left_area);
    if inner.height < 3 {
        return None;
    }

    let [_logo_area, _logo_gap_area, info_area] = top_left_panel_columns(inner, show_banner);
    if info_area.width == 0 || info_area.height < 3 {
        return None;
    }

    // URL line is rendered after app/status and adapter lines in the info column.
    let url_y = info_area.y + 2;
    if row != url_y {
        return None;
    }
    if col < info_area.x || col >= info_area.x + info_area.width {
        return None;
    }
    let rel_x = col - info_area.x;
    let primary_idx = selected_public_url_index(&state.public_urls, app_name)?;
    let primary_url = state.public_urls.get(primary_idx)?;
    let primary_width = primary_url.chars().map(char_width).sum::<u16>();
    (rel_x < primary_width).then_some(primary_idx)
}

fn hit_test_log_pos(
    state: &UiState,
    area: Rect,
    col: u16,
    row: u16,
    show_banner: bool,
) -> Option<(usize, u16)> {
    let content = logs_content_rect(area, show_banner);
    if content.width == 0 || content.height == 0 {
        return None;
    }

    let text_x = content.x;
    let text_w = content.width;
    let title_y = content.y;
    let first_row_y = title_y + logs_fixed_rows(state.logs.is_empty()) as u16;

    if row < first_row_y || row >= content.y + content.height {
        return None;
    }

    let rel_row = (row - first_row_y) as usize;
    let vrow = state.log_scroll.saturating_add(rel_row);
    if vrow >= state.last_total_rows {
        return None;
    }

    let rel_col = col.saturating_sub(text_x).min(text_w.saturating_sub(1));
    Some((vrow, rel_col))
}

fn clamp_log_pos(
    state: &UiState,
    area: Rect,
    col: u16,
    row: u16,
    show_banner: bool,
) -> Option<(usize, u16)> {
    let content = logs_content_rect(area, show_banner);
    if content.width == 0 || content.height == 0 {
        return None;
    }

    let text_x = content.x;
    let text_w = content.width;
    let title_y = content.y;
    let first_row_y = title_y + logs_fixed_rows(state.logs.is_empty()) as u16;
    let last_row_y = (content.y + content.height).saturating_sub(1);

    let clamped_row = row.clamp(first_row_y, last_row_y);
    let rel_row = (clamped_row - first_row_y) as usize;
    let vrow = state
        .log_scroll
        .saturating_add(rel_row)
        .min(state.last_total_rows.saturating_sub(1));

    let clamped_col = col.clamp(text_x, text_x + text_w.saturating_sub(1));
    let rel_col = clamped_col
        .saturating_sub(text_x)
        .min(text_w.saturating_sub(1));
    Some((vrow, rel_col))
}

fn focused_log_message_for_copy(state: &UiState, area: Rect, show_banner: bool) -> Option<String> {
    if state.logs.is_empty() {
        return None;
    }

    let content = logs_content_rect(area, show_banner);
    let content_w = content.width;
    if content_w == 0 {
        return None;
    }

    let visible_logs = visible_logs_for_display(state);
    let display_logs = build_display_log_lines(&visible_logs);
    let all_logs_ref: Vec<&str> = display_logs.iter().map(|line| line.text.as_str()).collect();
    let rows = build_visual_rows(&all_logs_ref, content_w);
    let row = rows.get(state.log_selected.min(rows.len().saturating_sub(1)))?;
    let source_idx = display_logs.get(row.log_idx)?.source_log_idx;
    visible_logs.get(source_idx).map(|log| log.message.clone())
}

fn handle_key(state: &mut UiState, key: KeyEvent) -> TuiAction {
    // Quit with `q` or Ctrl+c.
    if matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q')) {
        return TuiAction::Quit;
    }

    if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return TuiAction::Quit;
    }

    if state.terminate_confirmation_pending() {
        match key.code {
            KeyCode::Char('t') | KeyCode::Char('y') => {
                clear_selection(state);
                state.follow_end = false;
                state.terminate_confirm_until = None;
                state.set_status("terminating", Duration::from_secs(2));
                return TuiAction::Terminate;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                state.terminate_confirm_until = None;
                state.set_status("terminate cancelled", Duration::from_secs(2));
                return TuiAction::None;
            }
            _ => {
                // Any other key cancels pending terminate confirmation.
                state.terminate_confirm_until = None;
            }
        }
    }

    match key.code {
        KeyCode::Char('r') => {
            clear_selection(state);
            if state.app_pid.is_none() {
                state.push_log(ScopedLog::info(
                    "tako",
                    "Restart won't take effect (0 instances)",
                ));
                state.set_status(
                    "restart won't take effect (0 instances)",
                    Duration::from_secs(2),
                );
                TuiAction::None
            } else {
                state.follow_end = false;
                state.set_status("restarting", Duration::from_secs(2));
                TuiAction::Restart
            }
        }

        KeyCode::Char('c') if key.modifiers.is_empty() => {
            clear_logs_state(state);
            state.set_status("cleared", Duration::from_secs(2));
            TuiAction::ClearLogs
        }

        KeyCode::Char('t') => {
            clear_selection(state);
            state.follow_end = false;
            state.terminate_confirm_until =
                Some(Instant::now() + Duration::from_secs(TERMINATE_CONFIRM_SECS));
            state.set_status(
                "press t again to confirm terminate",
                Duration::from_secs(TERMINATE_CONFIRM_SECS),
            );
            TuiAction::None
        }

        KeyCode::Up => {
            clear_selection(state);
            state.follow_end = false;
            state.log_selected = state.log_selected.saturating_sub(1);
            state.log_scroll = ensure_visible(
                state.log_selected,
                state.log_scroll,
                state.last_visible_rows,
            );
            TuiAction::None
        }

        KeyCode::Down => {
            clear_selection(state);
            state.follow_end = false;
            let max = state.last_total_rows.saturating_sub(1);
            state.log_selected = state.log_selected.min(max);
            state.log_selected = (state.log_selected + 1).min(max);
            state.log_scroll = ensure_visible(
                state.log_selected,
                state.log_scroll,
                state.last_visible_rows,
            );
            TuiAction::None
        }

        KeyCode::PageUp => {
            clear_selection(state);
            state.follow_end = false;
            let step = state.last_visible_rows.max(1);
            state.log_selected = state.log_selected.saturating_sub(step);
            state.log_scroll = state.log_scroll.saturating_sub(step);
            state.log_scroll = ensure_visible(
                state.log_selected,
                state.log_scroll,
                state.last_visible_rows,
            );
            TuiAction::None
        }

        KeyCode::PageDown => {
            clear_selection(state);
            state.follow_end = false;
            let max = state.last_total_rows.saturating_sub(1);
            let step = state.last_visible_rows.max(1);
            state.log_selected = (state.log_selected + step).min(max);
            state.log_scroll = state.log_scroll.saturating_add(step);
            state.log_scroll = ensure_visible(
                state.log_selected,
                state.log_scroll,
                state.last_visible_rows,
            );
            TuiAction::None
        }

        KeyCode::Home => {
            clear_selection(state);
            state.follow_end = false;
            state.log_scroll = 0;
            state.log_selected = 0;
            TuiAction::None
        }

        KeyCode::Char('e') | KeyCode::Char('E') => {
            jump_to_log_end(state);
            TuiAction::None
        }

        KeyCode::End => {
            jump_to_log_end(state);
            TuiAction::None
        }

        _ => TuiAction::None,
    }
}

fn is_mouse_over_logs(area: Rect, col: u16, row: u16, show_banner: bool) -> bool {
    let content = logs_content_rect(area, show_banner);
    col >= content.x
        && col < content.x + content.width
        && row >= content.y
        && row < content.y + content.height
}

fn scroll_logs(state: &mut UiState, delta: isize) {
    if state.last_total_rows == 0 {
        return;
    }

    clear_selection(state);
    state.follow_end = false;

    // Keep it consistent with common terminal UIs: a few rows per wheel tick.
    let step = 3usize;
    let visible = state.last_visible_rows.max(1);
    let max = state.last_total_rows.saturating_sub(1);

    if delta < 0 {
        state.log_scroll = state.log_scroll.saturating_sub(step);
        state.log_selected = state.log_selected.saturating_sub(step);
    } else if delta > 0 {
        state.log_scroll = state.log_scroll.saturating_add(step);
        state.log_selected = (state.log_selected + step).min(max);
    }

    state.log_scroll = clamp_scroll(state.last_total_rows, state.log_scroll, visible);
    state.log_selected = state.log_selected.min(max);
    state.log_scroll = ensure_visible(state.log_selected, state.log_scroll, visible);
}

pub async fn run_dev_tui(
    app_name: String,
    adapter_name: String,
    hosts: Vec<String>,
    port: u16,
    app_port: u16,
    mut log_rx: mpsc::Receiver<ScopedLog>,
    mut event_rx: mpsc::Receiver<DevEvent>,
    control_tx: mpsc::Sender<ControlCmd>,
    log_store_path: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut guard = TuiGuard::enter()?;
    guard.set_title(&format!("tako | {}", app_name));
    let backend = guard.backend();
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let (event_tx, mut event_rx_ui) = mpsc::channel::<Event>(256);
    spawn_event_reader(event_tx);

    let mut state = UiState::new();

    state.public_urls = hosts
        .iter()
        .map(|h| format_tui_public_url(h, port))
        .collect();
    state.local_url = format_tui_local_url(app_port);
    let mut sys = System::new();
    // Keep idle CPU low: refresh metrics at a slower cadence.
    let mut ticker = tokio::time::interval(Duration::from_secs(METRICS_REFRESH_SECS));
    let mut drag_ticker = tokio::time::interval(Duration::from_millis(50));
    let mut loading_spinner_ticker =
        tokio::time::interval(Duration::from_millis(BANNER_ANIMATION_FRAME_MS));
    let mut last_draw = Instant::now();

    // First draw.
    render(&mut terminal, &app_name, &adapter_name, &mut state)?;

    let mut maybe_draw = |terminal: &mut Terminal<CrosstermBackend<&mut Stdout>>,
                          state: &mut UiState,
                          force: bool|
     -> io::Result<()> {
        let now = Instant::now();
        // Logs can arrive at high volume; cap non-forced redraw.
        if !force && now.duration_since(last_draw) < Duration::from_millis(33) {
            return Ok(());
        }
        render(terminal, &app_name, &adapter_name, state)?;
        last_draw = now;
        Ok(())
    };

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // Expire status message.
                state.clear_expired_status();
                state.clear_expired_flash();

                // Refresh CPU/memory metrics (only for the processes we care about).
                if let Some(app_pid) = state.app_pid {
                    sys.refresh_processes(ProcessesToUpdate::Some(&[app_pid]), false);
                } else {
                    sys.refresh_processes(ProcessesToUpdate::All, false);
                }

                if let Some(app_pid) = state.app_pid {
                    if let Some(proc) = sys.process(app_pid) {
                        state.metrics.app_cpu = Some(proc.cpu_usage());
                        state.metrics.app_mem_bytes = Some(proc.memory());
                    } else {
                        state.metrics.app_cpu = None;
                        state.metrics.app_mem_bytes = None;
                    }
                } else {
                    state.metrics.app_cpu = None;
                    state.metrics.app_mem_bytes = None;
                }
                state.metrics.control_clients = crate::dev_server_client::info()
                    .await
                    .ok()
                    .and_then(|info| parse_control_clients_from_info(&info));

                maybe_draw(&mut terminal, &mut state, false)?;
            }
            Some(line) = log_rx.recv() => {
                if let Some(path) = log_store_path.as_ref() {
                    super::append_log_to_store(path, &line).await;
                }
                state.push_log(line);
                maybe_draw(&mut terminal, &mut state, false)?;
            }
            Some(event) = event_rx.recv() => {
                match event {
                    DevEvent::AppStarted => {
                        state.app_state = AppState::Running;
                        // Don't spam status/logs on normal startup; metrics + state are enough.
                    }
                    DevEvent::AppLaunching => {
                        state.app_state = AppState::Launching;
                        state.set_status("starting...", Duration::from_secs(2));
                    }
                    DevEvent::AppStopped => {
                        state.app_state = AppState::Stopped;
                        state.app_pid = None;
                        state.push_log(ScopedLog::info("tako", "○ App stopped (idle)"));
                        state.set_status("app stopped", Duration::from_secs(2));
                    }
                    DevEvent::AppPid(pid) => {
                        state.app_pid = Some(Pid::from_u32(pid));
                    }
                    DevEvent::AppError(e) => {
                        state.app_state = AppState::Error;
                        state.push_log(ScopedLog::error(
                            "tako",
                            format!("✗ App error: {}", e),
                        ));
                        state.set_status("app error", Duration::from_secs(2));
                    }
                    DevEvent::LogsCleared => {
                        clear_logs_state(&mut state);
                        state.set_status("cleared", Duration::from_secs(2));
                    }
                    DevEvent::LogsReady => {
                        state.logs_loading = false;
                    }
                }

                maybe_draw(&mut terminal, &mut state, false)?;
            }
            _ = loading_spinner_ticker.tick() => {
                if state.logs.is_empty() && state.logs_loading {
                    maybe_draw(&mut terminal, &mut state, false)?;
                }
            }
            Some(ev) = event_rx_ui.recv() => {
                let action = match ev {
                    Event::Key(key) => {
                        let size = terminal.size().unwrap_or_default();
                        let area = Rect::new(0, 0, size.width, size.height);
                        let show_banner = show_startup_tako_banner(state.app_state);

                        if key.code == KeyCode::Enter {
                            if let Some(message) =
                                focused_log_message_for_copy(&state, area, show_banner)
                            {
                                copy_to_clipboard(&message);
                                state.set_status("message copied", Duration::from_secs(2));
                            }
                            TuiAction::None
                        } else {
                            handle_key(&mut state, key)
                        }
                    }
                    Event::Resize(_, _) => {
                        state.resize_pending = true;
                        state.last_resize_at = Some(Instant::now());
                        TuiAction::None
                    }
                    Event::Mouse(mouse) => {
                        let size = terminal.size().unwrap_or_default();
                        let area = Rect::new(0, 0, size.width, size.height);
                        let show_banner = show_startup_tako_banner(state.app_state);
                        state.last_mouse = Some((mouse.column, mouse.row));
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                if is_mouse_over_logs(area, mouse.column, mouse.row, show_banner) {
                                    scroll_logs(&mut state, -1);
                                }
                                TuiAction::None
                            }
                            MouseEventKind::ScrollDown => {
                                if is_mouse_over_logs(area, mouse.column, mouse.row, show_banner) {
                                    scroll_logs(&mut state, 1);
                                }
                                TuiAction::None
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                if let Some(idx) =
                                    hit_test_public_url(&state, area, mouse.column, mouse.row, &app_name)
                                {
                                    let url = state.public_urls.get(idx).cloned().unwrap_or_default();
                                    if !url.is_empty() {
                                        copy_to_clipboard(&url);
                                        state.flash_public_url(idx, Duration::from_millis(160));
                                        state.set_status("url copied", Duration::from_millis(800));
                                    }
                                    TuiAction::None
                                } else if let Some(pos) =
                                    hit_test_log_pos(
                                        &state,
                                        area,
                                        mouse.column,
                                        mouse.row,
                                        show_banner,
                                    )
                                {
                                    state.mouse_selecting = true;
                                    state.sel_anchor = Some(pos);
                                    state.sel_head = Some(pos);
                                    state.log_selected = pos.0;
                                    TuiAction::None
                                } else {
                                    TuiAction::None
                                }
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                if state.mouse_selecting
                                    && let Some(pos) =
                                        clamp_log_pos(
                                            &state,
                                            area,
                                            mouse.column,
                                            mouse.row,
                                            show_banner,
                                        )
                                {
                                    state.sel_head = Some(pos);
                                    state.log_selected = pos.0;
                                }
                                TuiAction::None
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                if state.mouse_selecting {
                                    if let Some(pos) = clamp_log_pos(
                                        &state,
                                        area,
                                        mouse.column,
                                        mouse.row,
                                        show_banner,
                                    ) {
                                        state.sel_head = Some(pos);
                                        state.log_selected = pos.0;
                                    }

                                    // Copy selection.
                                    let content = logs_content_rect(area, show_banner);
                                    let content_w = content.width.saturating_sub(2);
                                    let visible_logs = visible_logs_for_display(&state);
                                    let display_logs = build_display_log_lines(&visible_logs);
                                    let all_logs_ref: Vec<&str> =
                                        display_logs.iter().map(|line| line.text.as_str()).collect();
                                    let rows = build_visual_rows(&all_logs_ref, content_w);
                                    if let (Some(a), Some(b)) = (state.sel_anchor, state.sel_head)
                                        && let (Some(a), Some(b)) = (
                                            pos_to_text(&all_logs_ref, &rows, a.0.min(rows.len().saturating_sub(1)), a.1),
                                            pos_to_text(&all_logs_ref, &rows, b.0.min(rows.len().saturating_sub(1)), b.1),
                                        )
                                    {
                                        let text = extract_selection_text(&all_logs_ref, a, b);
                                        if !text.trim().is_empty() {
                                            copy_to_clipboard(&text);
                                            state.set_status(
                                                "selected text copied",
                                                Duration::from_millis(800),
                                            );
                                        }
                                    }

                                    state.mouse_selecting = false;
                                }
                                TuiAction::None
                            }
                            _ => TuiAction::None,
                        }
                    }
                    _ => TuiAction::None,
                };

                match action {
                    TuiAction::None => {}
                    TuiAction::Quit => break,
                    TuiAction::Restart => {
                        state.app_state = AppState::Restarting;
                        state.push_log(ScopedLog::info("tako", "Restarting app..."));
                        let _ = control_tx.send(ControlCmd::Restart).await;
                    }
                    TuiAction::Terminate => {
                        state.push_log(ScopedLog::info("tako", "Terminating app..."));
                        let _ = control_tx.send(ControlCmd::Terminate).await;
                        break;
                    }
                    TuiAction::ClearLogs => {
                        let _ = control_tx.send(ControlCmd::ClearLogs).await;
                    }
                }

                // Inputs should feel instant, except during active resize.
                if !state.resize_pending {
                    maybe_draw(&mut terminal, &mut state, true)?;
                }
            }
            _ = drag_ticker.tick() => {
                // Coalesce resize redraws to avoid flicker while the user is resizing.
                if state.resize_pending {
                    let ready = state
                        .last_resize_at
                        .map(|t| Instant::now().duration_since(t) > Duration::from_millis(120))
                        .unwrap_or(false);
                    if ready {
                        state.resize_pending = false;
                        maybe_draw(&mut terminal, &mut state, true)?;
                    }
                }

                if state.mouse_selecting {
                    let size = terminal.size().unwrap_or_default();
                    let area = Rect::new(0, 0, size.width, size.height);
                    let show_banner = show_startup_tako_banner(state.app_state);
                    let content = logs_content_rect(area, show_banner);
                    if content.height >= 2 {
                        let first_row_y = content.y + 1;
                        let last_row_y = (content.y + content.height).saturating_sub(1);
                        if let Some((mx, my)) = state.last_mouse {
                            let mut scrolled = false;
                            if my <= first_row_y {
                                if state.log_scroll > 0 {
                                    state.log_scroll -= 1;
                                    scrolled = true;
                                }
                            } else if my >= last_row_y {
                                let max_scroll = state.last_total_rows.saturating_sub(state.last_visible_rows.max(1));
                                if state.log_scroll < max_scroll {
                                    state.log_scroll += 1;
                                    scrolled = true;
                                }
                            }

                            if scrolled {
                                if let Some(pos) = clamp_log_pos(&state, area, mx, my, show_banner)
                                {
                                    state.sel_head = Some(pos);
                                    state.log_selected = pos.0;
                                }
                                maybe_draw(&mut terminal, &mut state, true)?;
                            }
                        }
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            },
        }
    }

    let _ = terminal.clear();
    Ok(())
}

#[cfg(test)]
mod key_tests {
    use super::super::LogLevel;
    use super::*;
    use crossterm::event::MouseEvent;

    #[test]
    fn format_scoped_log_line_is_mm_ss() {
        let l = ScopedLog {
            h: 12,
            m: 3,
            s: 7,
            level: LogLevel::Info,
            scope: "tako".to_string(),
            message: "hello".to_string(),
        };
        assert_eq!(format_scoped_log_line(&l), "12:03:07 INFO  [tako] hello");
    }

    #[test]
    fn header_status_value_is_plain_without_caption() {
        assert_eq!(header_status_text(AppState::Starting), "not started");
        assert_eq!(header_status_text(AppState::Launching), "starting...");
        assert_eq!(header_status_text(AppState::Stopped), "stopped (idle)");
        assert_eq!(header_status_text(AppState::Running), "running");
        assert_eq!(header_status_text(AppState::Restarting), "restarting");
        assert_eq!(header_status_text(AppState::Error), "failed");
    }

    #[test]
    fn header_adapter_line_formats_runtime_application_label() {
        assert_eq!(header_adapter_text("bun"), "bun application");
        assert_eq!(header_adapter_text("  bun  "), "bun application");
        assert_eq!(header_adapter_text(""), "application");
    }

    #[test]
    fn fmt_bytes_prefers_mb_and_switches_to_one_decimal_gb_at_one_gib() {
        let mib = 1024_u64 * 1024;
        let gib = mib * 1024;

        assert_eq!(fmt_bytes(900 * mib), "900 MB");
        assert_eq!(fmt_bytes(gib - 1), "1023 MB");
        assert_eq!(fmt_bytes(gib), "1.0 GB");
        assert_eq!(fmt_bytes((gib * 6) / 5), "1.2 GB");
    }

    #[test]
    fn fmt_control_clients_uses_dash_when_unknown() {
        assert_eq!(fmt_control_clients(Some(3)), "3");
        assert_eq!(fmt_control_clients(None), "—");
    }

    #[test]
    fn right_aligned_metric_row_aligns_value_to_row_end_when_space_allows() {
        let line = right_aligned_metric_row("CPU:", "9%", 12, Style::default(), Style::default());
        let text: String = line
            .spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect();
        assert_eq!(text, "CPU:      9%");
        let width: u16 = text.chars().map(char_width).sum();
        assert_eq!(width, 12);
    }

    #[test]
    fn right_aligned_metric_row_falls_back_to_single_gap_when_too_narrow() {
        let line =
            right_aligned_metric_row("Sessions:", "12345", 8, Style::default(), Style::default());
        let text: String = line
            .spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect();
        assert_eq!(text, "Sessions: 12345");
    }

    #[test]
    fn parse_control_clients_from_info_reads_nested_info_value() {
        let info = serde_json::json!({
            "type": "Info",
            "info": { "control_clients": 7 }
        });
        assert_eq!(parse_control_clients_from_info(&info), Some(7));
    }

    #[test]
    fn parse_control_clients_from_info_returns_none_when_missing() {
        let info = serde_json::json!({
            "type": "Info",
            "info": {}
        });
        assert_eq!(parse_control_clients_from_info(&info), None);
    }

    #[test]
    fn header_control_clients_label_is_readable_and_fits_compact_metrics_panel() {
        assert_eq!(HEADER_CONTROL_CLIENTS_LABEL, "Sessions");
        assert!(format!("{HEADER_CONTROL_CLIENTS_LABEL}: 9999").len() <= 14);
    }

    #[test]
    fn header_metrics_column_width_is_compact() {
        assert_eq!(HEADER_METRICS_COL_WIDTH, 16);
    }

    #[test]
    fn hit_test_public_url_picks_correct_url() {
        let mut state = UiState::new();
        state.app_state = AppState::Running;
        state.public_urls = vec![
            "https://a.tako.local/".to_string(),
            "https://b.tako.local/".to_string(),
        ];

        let area = Rect::new(0, 0, 120, 40);
        let [header_area, _gap, _body, _footer] =
            layout_chunks_for_banner(area, show_startup_tako_banner(state.app_state));
        let [header_left_area, _header_gap_area, _header_right_area] =
            header_panel_chunks(header_area);
        let inner = header_left_inner_area(header_left_area);
        let [_logo_area, _logo_gap_area, info_area] =
            top_left_panel_columns(inner, show_startup_tako_banner(state.app_state));
        let y = info_area.y + 2;

        // status line should not be clickable
        assert_eq!(
            hit_test_public_url(&state, area, info_area.x, y - 1, "b"),
            None
        );

        // Default host (b.tako.local) is shown and clickable first.
        assert_eq!(
            hit_test_public_url(&state, area, info_area.x, y, "b"),
            Some(1)
        );

        let shown_width: u16 = state.public_urls[1].chars().map(char_width).sum();
        let x_more = info_area.x + shown_width + 1;
        // "(and N more)" hint is muted text and not clickable.
        assert_eq!(hit_test_public_url(&state, area, x_more, y, "b"), None);
    }

    #[test]
    fn hit_test_public_url_uses_first_when_default_missing() {
        let mut state = UiState::new();
        state.app_state = AppState::Running;
        state.public_urls = vec![
            "https://x.tako.local/".to_string(),
            "https://y.tako.local/".to_string(),
        ];

        let area = Rect::new(0, 0, 120, 40);
        let [header_area, _gap, _body, _footer] =
            layout_chunks_for_banner(area, show_startup_tako_banner(state.app_state));
        let [header_left_area, _header_gap_area, _header_right_area] =
            header_panel_chunks(header_area);
        let inner = header_left_inner_area(header_left_area);
        let [_logo_area, _logo_gap_area, info_area] =
            top_left_panel_columns(inner, show_startup_tako_banner(state.app_state));
        let y = info_area.y + 2;

        assert_eq!(
            hit_test_public_url(&state, area, info_area.x, y, "app"),
            Some(0)
        );
    }

    #[test]
    fn hit_test_public_url_ignores_right_metrics_column() {
        let mut state = UiState::new();
        state.app_state = AppState::Running;
        state.public_urls = vec![format!("https://{}.tako.local/", "a".repeat(160))];

        let area = Rect::new(0, 0, 120, 40);
        let [header_area, _gap, _body, _footer] =
            layout_chunks_for_banner(area, show_startup_tako_banner(state.app_state));
        let [header_left_area, _header_gap_area, _header_right_area] =
            header_panel_chunks(header_area);
        let inner = header_left_inner_area(header_left_area);
        let [_logo_area, _logo_gap_area, info_area] =
            top_left_panel_columns(inner, show_startup_tako_banner(state.app_state));
        let y = info_area.y + 2;

        // Right metrics column uses fixed width, so URL hit testing must ignore it.
        let right_column_width = HEADER_METRICS_COL_WIDTH;
        let x_in_right_column = header_area
            .x
            .saturating_add(header_area.width.saturating_sub(right_column_width));

        assert_eq!(
            hit_test_public_url(&state, area, x_in_right_column, y, "a"),
            None
        );
    }

    #[test]
    fn hit_test_public_url_works_when_logo_is_hidden_on_narrow_width() {
        let mut state = UiState::new();
        state.app_state = AppState::Running;
        state.public_urls = vec!["https://app.tako.local/".to_string()];

        let area = Rect::new(0, 0, 30, 40);
        let [header_area, _gap, _body, _footer] =
            layout_chunks_for_banner(area, show_startup_tako_banner(state.app_state));
        let [header_left_area, _header_gap_area, _header_right_area] =
            header_panel_chunks(header_area);
        let inner = header_left_inner_area(header_left_area);
        let [_logo_area, _logo_gap_area, info_area] =
            top_left_panel_columns(inner, show_startup_tako_banner(state.app_state));
        let y = info_area.y + 2;

        assert_eq!(
            hit_test_public_url(&state, area, info_area.x + 2, y, "app"),
            Some(0)
        );
    }

    #[test]
    fn header_panels_use_visible_gap_and_fixed_right_width() {
        let area = Rect::new(0, 0, 120, 5);
        let [left, gap, right] = header_panel_chunks(area);

        assert_eq!(gap.width, PANEL_GAP_WIDTH);
        assert_eq!(right.width, HEADER_METRICS_COL_WIDTH);
        assert_eq!(
            left.width + gap.width + right.width,
            area.width,
            "header panels should fill available width"
        );
    }

    #[test]
    fn header_left_inner_area_matches_header_panel_padding() {
        let area = Rect::new(0, 0, 120, 12);
        let block = Block::default().padding(panel_padding_all_sides());
        assert_eq!(header_left_inner_area(area), block.inner(area));
    }

    #[test]
    fn top_left_logo_gap_matches_panel_gap_width() {
        let area = Rect::new(0, 0, 120, 40);
        let [header_area, _gap, _body, _footer] = layout_chunks_for_banner(area, true);
        let [header_left_area, _header_gap, _header_right] = header_panel_chunks(header_area);
        let inner = header_left_inner_area(header_left_area);
        let [_logo, logo_gap, _info] = top_left_panel_columns(inner, true);

        assert_eq!(logo_gap.width, PANEL_GAP_WIDTH);
    }

    #[test]
    fn top_left_panel_hides_logo_when_width_is_too_small() {
        let area = Rect::new(0, 0, 5, 3);
        let [logo, logo_gap, info] = top_left_panel_columns(area, true);

        assert_eq!(logo.width, 0);
        assert_eq!(logo_gap.width, 0);
        assert_eq!(info, area);
    }

    #[test]
    fn global_padding_matches_panel_gap_width() {
        let area = Rect::new(0, 0, 120, 40);
        let [header, _gap, _body, footer] = layout_chunks_for_banner(area, true);

        assert_eq!(header.x.saturating_sub(area.x), PANEL_GAP_WIDTH);
        assert_eq!(header.y.saturating_sub(area.y), PANEL_GAP_WIDTH);

        let right_padding = area
            .x
            .saturating_add(area.width)
            .saturating_sub(header.x.saturating_add(header.width));
        let bottom_padding = area
            .y
            .saturating_add(area.height)
            .saturating_sub(footer.y.saturating_add(footer.height));

        assert_eq!(right_padding, PANEL_GAP_WIDTH);
        assert_eq!(bottom_padding, PANEL_GAP_WIDTH);
    }

    #[test]
    fn mouse_wheel_scrolls_when_over_logs() {
        let mut state = UiState::new();
        state.last_total_rows = 100;
        state.last_visible_rows = 10;
        state.log_scroll = 20;
        state.log_selected = 25;

        let area = Rect::new(0, 0, 120, 40);
        let content = logs_content_rect(area, false);
        let m = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: content.x,
            row: content.y,
            modifiers: KeyModifiers::empty(),
        };

        assert!(is_mouse_over_logs(area, m.column, m.row, false));
        scroll_logs(&mut state, -1);
        assert_eq!(state.log_scroll, 17);
        assert_eq!(state.log_selected, 22);
    }

    #[test]
    fn mouse_wheel_does_not_scroll_outside_logs() {
        let mut state = UiState::new();
        state.last_total_rows = 100;
        state.last_visible_rows = 10;
        state.log_scroll = 20;
        state.log_selected = 25;

        let area = Rect::new(0, 0, 120, 40);
        let m = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        };
        assert!(!is_mouse_over_logs(area, m.column, m.row, false));

        let before = (state.log_scroll, state.log_selected);
        // Simulate what the event handler does: it won't call scroll_logs.
        if is_mouse_over_logs(area, m.column, m.row, false) {
            scroll_logs(&mut state, -1);
        }
        assert_eq!(before, (state.log_scroll, state.log_selected));
    }
}

#[cfg(test)]
mod tests {
    use super::super::LogLevel;
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::widgets::Widget;

    #[test]
    fn default_theme_uses_tako_brand_palette() {
        assert_eq!(TAKO_BRAND.bg, Color::Rgb(0x13, 0x15, 0x17));
        assert_eq!(TAKO_BRAND.panel_bg, Color::Rgb(0x1B, 0x1F, 0x22));
        assert_eq!(TAKO_BRAND.fg, Color::Rgb(0xF2, 0xEC, 0xEA));
        assert_eq!(TAKO_BRAND.secondary, Color::Rgb(0x9B, 0xC4, 0xB6));
        assert_eq!(TAKO_BRAND.muted, Color::Rgb(0x94, 0xA3, 0xB8));
        assert_eq!(TAKO_BRAND.label_muted, Color::Rgb(0x64, 0x74, 0x8B));
        assert_eq!(TAKO_BRAND.accent, Color::Rgb(0xE8, 0x87, 0x83));
        assert_eq!(TAKO_BRAND.log_debug, Color::Rgb(0x8C, 0xCF, 0xFF));
        assert_eq!(TAKO_BRAND.log_info, Color::Rgb(0x9B, 0xD9, 0xB3));
        assert_eq!(TAKO_BRAND.log_warn, Color::Rgb(0xEA, 0xD3, 0x9C));
        assert_eq!(TAKO_BRAND.log_error, Color::Rgb(0xE8, 0xA3, 0xA0));
        assert_eq!(TAKO_BRAND.log_fatal, Color::Rgb(0xC8, 0xA6, 0xF2));
    }

    #[test]
    fn log_level_color_map_matches_requested_levels() {
        assert_eq!(
            log_level_color(&LogLevel::Debug, TAKO_BRAND),
            TAKO_BRAND.log_debug
        );
        assert_eq!(
            log_level_color(&LogLevel::Info, TAKO_BRAND),
            TAKO_BRAND.log_info
        );
        assert_eq!(
            log_level_color(&LogLevel::Warn, TAKO_BRAND),
            TAKO_BRAND.log_warn
        );
        assert_eq!(
            log_level_color(&LogLevel::Error, TAKO_BRAND),
            TAKO_BRAND.log_error
        );
        assert_eq!(
            log_level_color(&LogLevel::Fatal, TAKO_BRAND),
            TAKO_BRAND.log_fatal
        );
    }

    #[test]
    fn selected_public_url_index_prefers_default_host() {
        let urls = vec![
            "https://api.app.tako.local".to_string(),
            "https://app.tako.local".to_string(),
        ];
        assert_eq!(selected_public_url_index(&urls, "app"), Some(1));
    }

    #[test]
    fn selected_public_url_index_falls_back_to_first() {
        let urls = vec![
            "https://api.other.tako.local".to_string(),
            "https://other.tako.local".to_string(),
        ];
        assert_eq!(selected_public_url_index(&urls, "app"), Some(0));
    }

    #[test]
    fn selected_public_url_index_matches_default_host_with_port() {
        let urls = vec![
            "https://api.app.tako.local:47831".to_string(),
            "https://app.tako.local:47831".to_string(),
        ];
        assert_eq!(selected_public_url_index(&urls, "app"), Some(1));
    }

    #[test]
    fn footer_hints_group_lifecycle_commands_together() {
        let text = footer_hint_text();
        assert!(text.contains("q quit   r restart   t terminate"));
    }

    #[test]
    fn display_log_lines_collapse_consecutive_duplicates_with_muted_summary() {
        let first = ScopedLog {
            h: 12,
            m: 58,
            s: 33,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "some message".to_string(),
        };
        let second = ScopedLog {
            h: 12,
            m: 58,
            s: 34,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "some message".to_string(),
        };
        let third = ScopedLog {
            h: 12,
            m: 58,
            s: 35,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "some message".to_string(),
        };
        let logs = vec![&first, &second, &third];

        let display = build_display_log_lines(&logs);
        assert_eq!(display.len(), 2);
        assert_eq!(display[0].text, "12:58:33 INFO  [app] some message");
        assert!(matches!(display[0].level, Some(LogLevel::Info)));
        assert!(!display[0].muted);
        assert_eq!(display[0].source_log_idx, 0);

        let indent = " ".repeat(format_scoped_log_prefix(&first).chars().count());
        assert_eq!(display[1].text, format!("{indent}also 2 more times"));
        assert!(display[1].level.is_none());
        assert!(display[1].muted);
        assert_eq!(display[1].source_log_idx, 0);
    }

    #[test]
    fn display_log_lines_do_not_collapse_nonconsecutive_duplicates() {
        let first = ScopedLog {
            h: 12,
            m: 58,
            s: 33,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "some message".to_string(),
        };
        let middle = ScopedLog {
            h: 12,
            m: 58,
            s: 34,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "other message".to_string(),
        };
        let last = ScopedLog {
            h: 12,
            m: 58,
            s: 35,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "some message".to_string(),
        };
        let logs = vec![&first, &middle, &last];

        let display = build_display_log_lines(&logs);
        assert_eq!(display.len(), 3);
        assert!(display.iter().all(|line| !line.muted));
    }

    #[test]
    fn focused_copy_from_summary_row_returns_original_message() {
        let mut state = UiState::new();
        state.push_log(ScopedLog {
            h: 12,
            m: 58,
            s: 33,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "same message".to_string(),
        });
        state.push_log(ScopedLog {
            h: 12,
            m: 58,
            s: 34,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "same message".to_string(),
        });
        state.push_log(ScopedLog {
            h: 12,
            m: 58,
            s: 35,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "same message".to_string(),
        });

        // Row 0 is the first full line, row 1 is the muted summary.
        state.log_selected = 1;
        let area = Rect::new(0, 0, 120, 40);
        let copied = focused_log_message_for_copy(&state, area, false).expect("copied message");
        assert_eq!(copied, "same message");
    }

    #[test]
    fn log_level_field_range_extracts_token_only() {
        let line = "11:24:55 INFO  [tako] Ready";
        assert_eq!(log_level_field_range(line), Some((9, 13)));
    }

    #[test]
    fn log_timestamp_field_range_extracts_time_token_only() {
        let line = "11:24:55 INFO  [tako] Ready";
        assert_eq!(log_timestamp_field_range(line), Some((0, 8)));
    }

    #[test]
    fn log_line_spans_color_only_level_token() {
        let line = "11:24:55 INFO  [tako] Ready";
        let spans = build_log_line_spans(
            line,
            (0, line.len()),
            log_timestamp_field_range(line),
            log_level_field_range(line),
            TAKO_BRAND.fg,
            TAKO_BRAND.muted,
            TAKO_BRAND.log_info,
            None,
            None,
        );

        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content.as_ref(), "11:24:55");
        assert_eq!(spans[0].style.fg, Some(TAKO_BRAND.muted));
        assert_eq!(spans[1].content.as_ref(), " ");
        assert_eq!(spans[1].style.fg, Some(TAKO_BRAND.fg));
        assert_eq!(spans[2].content.as_ref(), "INFO");
        assert_eq!(spans[2].style.fg, Some(TAKO_BRAND.log_info));
        assert_eq!(spans[3].content.as_ref(), "  [tako] Ready");
        assert_eq!(spans[3].style.fg, Some(TAKO_BRAND.fg));
    }

    #[test]
    fn log_line_spans_keep_cursor_background_on_all_segments() {
        let line = "11:24:55 INFO  [tako] Ready";
        let spans = build_log_line_spans(
            line,
            (0, line.len()),
            log_timestamp_field_range(line),
            log_level_field_range(line),
            TAKO_BRAND.fg,
            TAKO_BRAND.muted,
            TAKO_BRAND.log_info,
            Some((0, line.len())),
            Some(TAKO_BRAND.cursor_bg),
        );

        assert_eq!(spans[0].style.bg, Some(TAKO_BRAND.cursor_bg));
        assert_eq!(spans[1].style.bg, Some(TAKO_BRAND.cursor_bg));
        assert_eq!(spans[2].style.bg, Some(TAKO_BRAND.cursor_bg));
        assert_eq!(spans[3].style.bg, Some(TAKO_BRAND.cursor_bg));
        assert_eq!(spans[0].style.fg, Some(TAKO_BRAND.muted));
        assert_eq!(spans[2].style.fg, Some(TAKO_BRAND.log_info));
    }

    #[test]
    fn public_url_style_uses_secondary_color() {
        let style = public_url_style(&TAKO_BRAND, false);
        assert_eq!(style.fg, Some(TAKO_BRAND.secondary));
    }

    #[test]
    fn tako_banner_is_visible_in_all_states() {
        assert!(show_startup_tako_banner(AppState::Starting));
        assert!(show_startup_tako_banner(AppState::Launching));
        assert!(show_startup_tako_banner(AppState::Running));
        assert!(show_startup_tako_banner(AppState::Stopped));
        assert!(show_startup_tako_banner(AppState::Error));
        assert!(show_startup_tako_banner(AppState::Restarting));
    }

    #[test]
    fn startup_banner_uses_solid_block_glyphs() {
        assert!(
            STARTUP_TAKO_ASCII.iter().all(|line| line.contains('█')),
            "logo should use solid block glyphs"
        );
        assert!(
            STARTUP_TAKO_ASCII
                .iter()
                .all(|line| !line.chars().any(|c| c.is_ascii_alphabetic())),
            "logo should not be alphabetic ASCII art"
        );
    }

    #[test]
    fn startup_banner_is_compact_with_tight_letter_spacing() {
        let max_width = STARTUP_TAKO_ASCII
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        assert!(
            max_width <= 16,
            "logo should remain compact; got width {max_width}"
        );
        assert!(
            STARTUP_TAKO_ASCII.iter().all(|line| !line.contains("   ")),
            "logo should not use wide inter-letter gaps"
        );
    }

    #[test]
    fn banner_header_height_matches_three_row_status_block() {
        let area = Rect::new(0, 0, 120, 40);
        let [header_default, gap_default, _body_default, _footer_default] = layout_chunks(area);
        let [header_banner, gap_banner, _body_banner, _footer_banner] =
            layout_chunks_for_banner(area, true);

        assert_eq!(header_default.height, top_header_height(false));
        assert_eq!(header_banner.height, top_header_height(true));
        assert_eq!(header_banner.height, header_default.height);
        assert_eq!(gap_default.height, PANEL_GAP_WIDTH);
        assert_eq!(gap_banner.height, PANEL_GAP_WIDTH);
    }

    #[test]
    fn logs_panel_block_renders_without_border_line() {
        let area = Rect::new(0, 0, 20, 4);
        let mut buffer = Buffer::empty(area);

        logs_panel_block(TAKO_BRAND).render(area, &mut buffer);

        for x in area.x..area.x + area.width {
            assert_eq!(buffer[(x, area.y)].symbol(), " ");
        }
    }

    #[test]
    fn logs_panel_block_does_not_reserve_top_header_row() {
        let area = Rect::new(5, 7, 30, 10);
        let inner = logs_panel_block(TAKO_BRAND).inner(area);

        assert_eq!(inner.y, area.y);
        assert_eq!(inner.height, area.height);
    }

    #[test]
    fn logs_caption_area_uses_top_inner_row() {
        let panel_area = Rect::new(5, 7, 30, 10);
        let text_area = logs_panel_block(TAKO_BRAND).inner(panel_area);
        assert_eq!(
            logs_caption_area(text_area),
            Rect::new(text_area.x, text_area.y, text_area.width, 1)
        );
    }

    #[test]
    fn logs_caption_is_plain_label() {
        assert_eq!(LOGS_CAPTION, "Logs");
    }

    #[test]
    fn logs_caption_is_left_aligned() {
        assert_eq!(LOGS_CAPTION_ALIGNMENT, Alignment::Left);
    }

    #[test]
    fn loading_logs_hint_shows_only_when_no_logs_rendered() {
        assert_eq!(loading_logs_hint(1, true, 0), None);
        assert_eq!(loading_logs_hint(1, false, 0), None);
    }

    #[test]
    fn logs_fixed_rows_include_header_padding() {
        assert_eq!(logs_fixed_rows(false), 1);
        assert_eq!(logs_fixed_rows(true), 2);
    }

    #[test]
    fn loading_logs_hint_switches_to_waiting_after_initial_load() {
        assert_eq!(
            loading_logs_hint(0, false, 0).as_deref(),
            Some("Waiting for logs...")
        );
    }

    #[test]
    fn loading_logs_hint_rotates_spinner_frames() {
        assert_eq!(
            loading_logs_hint(0, true, 0).as_deref(),
            Some("Loading logs... |")
        );
        assert_eq!(
            loading_logs_hint(0, true, 1).as_deref(),
            Some("Loading logs... /")
        );
        assert_eq!(
            loading_logs_hint(0, true, 2).as_deref(),
            Some("Loading logs... -")
        );
        assert_eq!(
            loading_logs_hint(0, true, 3).as_deref(),
            Some("Loading logs... \\")
        );
        assert_eq!(
            loading_logs_hint(0, true, 4).as_deref(),
            Some("Loading logs... |")
        );
    }

    #[test]
    fn format_tui_public_url_omits_trailing_slash() {
        assert_eq!(
            format_tui_public_url("app.tako.local", 443),
            "https://app.tako.local"
        );
        assert_eq!(
            format_tui_public_url("app.tako.local", 47831),
            "https://app.tako.local:47831"
        );
    }

    #[test]
    fn q_quits_immediately() {
        let mut state = UiState::new();
        let ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut state, ev), TuiAction::Quit);
    }

    #[test]
    fn ctrl_c_quits_on_all_platforms() {
        let mut state = UiState::new();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut state, ev), TuiAction::Quit);
    }

    #[test]
    fn ctrl_c_does_not_clear_logs() {
        let mut state = UiState::new();
        state.push_log(ScopedLog::info("app", "keep me"));
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        assert_eq!(handle_key(&mut state, ev), TuiAction::Quit);
        assert_eq!(state.logs.len(), 1);
    }

    #[test]
    fn c_key_clears_logs_and_requests_shared_clear() {
        let mut state = UiState::new();
        state.push_log(ScopedLog::info("app", "one"));
        state.push_log(ScopedLog::info("app", "two"));
        state.log_scroll = 4;
        state.log_selected = 5;
        state.follow_end = false;
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, ev), TuiAction::ClearLogs);
        assert!(state.logs.is_empty());
        assert_eq!(state.log_scroll, 0);
        assert_eq!(state.log_selected, 0);
        assert!(state.follow_end);
        assert_eq!(
            state.status_msg.as_ref().map(|(msg, _)| msg.as_str()),
            Some("cleared")
        );
    }

    #[test]
    fn e_key_jumps_to_log_end() {
        let mut state = UiState::new();
        state.last_total_rows = 100;
        state.last_visible_rows = 10;
        state.log_selected = 20;
        state.log_scroll = 15;
        state.follow_end = false;
        let ev = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, ev), TuiAction::None);
        assert!(state.follow_end);
        assert_eq!(state.log_selected, 99);
        assert_eq!(state.log_scroll, 90);
    }

    #[test]
    fn uppercase_e_key_jumps_to_log_end() {
        let mut state = UiState::new();
        state.last_total_rows = 42;
        state.last_visible_rows = 8;
        state.log_selected = 3;
        state.log_scroll = 1;
        let ev = KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT);

        assert_eq!(handle_key(&mut state, ev), TuiAction::None);
        assert!(state.follow_end);
        assert_eq!(state.log_selected, 41);
        assert_eq!(state.log_scroll, 34);
    }

    #[test]
    fn focused_log_message_for_copy_returns_only_message() {
        let mut state = UiState::new();
        state.push_log(ScopedLog {
            h: 1,
            m: 2,
            s: 3,
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "hello from focused row".to_string(),
        });
        state.log_selected = 0;

        let area = Rect::new(0, 0, 120, 40);
        let copied = focused_log_message_for_copy(&state, area, false).expect("copied message");
        assert_eq!(copied, "hello from focused row");
    }

    #[test]
    fn restart_key_is_noop_when_no_instances_running() {
        let mut state = UiState::new();
        let ev = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, ev), TuiAction::None);
        assert_eq!(
            state.status_msg.as_ref().map(|(msg, _)| msg.as_str()),
            Some("restart won't take effect (0 instances)")
        );
        assert_eq!(state.logs.len(), 1);
        assert_eq!(
            state.logs.back().map(|log| log.message.as_str()),
            Some("Restart won't take effect (0 instances)")
        );
    }

    #[test]
    fn restart_key_requests_restart_when_instance_running() {
        let mut state = UiState::new();
        state.app_pid = Some(Pid::from_u32(42));
        let ev = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, ev), TuiAction::Restart);
        assert_eq!(
            state.status_msg.as_ref().map(|(msg, _)| msg.as_str()),
            Some("restarting")
        );
        assert!(state.logs.is_empty());
    }

    #[test]
    fn terminate_key_asks_for_confirmation_first() {
        let mut state = UiState::new();
        let ev = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, ev), TuiAction::None);
        assert_eq!(
            state.status_msg.as_ref().map(|(msg, _)| msg.as_str()),
            Some("press t again to confirm terminate")
        );
    }

    #[test]
    fn terminate_confirmation_timeout_is_three_seconds() {
        assert_eq!(TERMINATE_CONFIRM_SECS, 3);
    }

    #[test]
    fn terminate_key_confirms_on_second_t_press() {
        let mut state = UiState::new();
        let ev = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, ev), TuiAction::None);
        assert_eq!(handle_key(&mut state, ev), TuiAction::Terminate);
        assert_eq!(
            state.status_msg.as_ref().map(|(msg, _)| msg.as_str()),
            Some("terminating")
        );
    }

    #[test]
    fn terminate_confirmation_can_be_cancelled() {
        let mut state = UiState::new();
        let terminate = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);
        let cancel = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, terminate), TuiAction::None);
        assert_eq!(handle_key(&mut state, cancel), TuiAction::None);
        assert_eq!(
            state.status_msg.as_ref().map(|(msg, _)| msg.as_str()),
            Some("terminate cancelled")
        );
    }

    #[test]
    fn terminate_confirmation_accepts_y() {
        let mut state = UiState::new();
        let terminate = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);
        let confirm = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, terminate), TuiAction::None);
        assert_eq!(handle_key(&mut state, confirm), TuiAction::Terminate);
    }

    #[test]
    fn terminate_confirmation_esc_cancels() {
        let mut state = UiState::new();
        let terminate = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);
        let cancel = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(handle_key(&mut state, terminate), TuiAction::None);
        assert_eq!(handle_key(&mut state, cancel), TuiAction::None);
        assert_eq!(
            state.status_msg.as_ref().map(|(msg, _)| msg.as_str()),
            Some("terminate cancelled")
        );
    }

    #[test]
    fn wrap_ranges_splits_long_line() {
        let s = "hello world";
        let r = wrap_ranges(s, 5);
        assert!(r.len() >= 2);
        assert_eq!(&s[r[0].0..r[0].1], "hello");
    }

    #[test]
    fn extract_selection_across_lines_inserts_newlines() {
        let logs = vec!["abc", "def", "ghi"];
        let text = extract_selection_text(&logs, (0, 1), (2, 2));
        assert_eq!(text, "bc\ndef\ngh");
    }
}
