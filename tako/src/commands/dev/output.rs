//! Streaming dev output for `tako dev`.
//!
//! Prints a branded header once at startup, then streams logs above a sticky
//! footer. The footer (bordered panel + right-aligned keymap) is erased and
//! reprinted below every log line so it stays pinned at the bottom.
//! No alternate screen — native terminal scrollback and search work normally.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use console::{Term, measure_text_width, truncate_str};
use crossterm::cursor;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::terminal::{self, ClearType, SetTitle};
use crossterm::{execute, queue};
use sysinfo::{Pid, ProcessesToUpdate, System};
use tokio::sync::mpsc;

use super::{DevEvent, LogLevel, ScopedLog};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const CANARY_SHA: Option<&str> = option_env!("TAKO_CANARY_SHA");

const METRICS_REFRESH_SECS: u64 = 2;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
// Brand teal border color (muted with dim so it's subtle, not screaming).
const BORDER: &str = "\x1b[2;38;2;155;196;182m";
// Primary coral for panel header text (22 = normal intensity, cancels inherited dim).
const PRIMARY: &str = "\x1b[22;38;2;232;135;131m";

// 3-row compact "tako.sh" logo.
const LOGO_ROWS: [&str; 3] = [
    "▀█▀ ▄▀█ █ █ █▀█   █▀ █ █",
    " █  █▀█ █▀▄ █ █   ▀█ █▀█",
    " ▀  ▀ ▀ ▀ ▀ ▀▀▀ ▀ ▀▀ ▀ ▀",
];

// Gradient endpoints: primary coral → secondary teal (applied per-character).
const LOGO_COLOR_START: (u8, u8, u8) = (232, 135, 131); // #E88783
const LOGO_COLOR_END: (u8, u8, u8) = (155, 196, 182); // #9BC4B6

const INDENT: &str = "  ";

// Below this column width the panel columns are stacked vertically.
const STACKED_THRESHOLD: usize = 76;

// Panel column widths (wide mode).
const COL1_W: usize = 20; // status · worktree · repo slug · repo path
const COL3_W: usize = 22; // cpu · ram · pid
const COL_SEP: usize = 2; // gap between columns
const BAR_W: usize = 8; // chars inside the progress bar
const ROUTES_LABEL_W: usize = 8; // "routes  " prefix

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCmd {
    Restart,
    Terminate,
}

/// Exit value returned by [`run_dev_output`].
pub enum DevOutputExit {
    /// The client terminated (Ctrl+C or supervisor-driven exit).
    Terminate,
    /// The user pressed `d` to detach the UI while keeping the app running.
    ///
    /// The caller receives the log/event receivers so it can keep draining
    /// them to the JSONL store while the process stays alive in the background.
    Detach {
        #[allow(dead_code)]
        log_rx: mpsc::Receiver<ScopedLog>,
        #[allow(dead_code)]
        event_rx: mpsc::Receiver<DevEvent>,
    },
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn ansi_rgb(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn build_version_string() -> String {
    match CANARY_SHA {
        Some(sha) if !sha.trim().is_empty() => {
            let short = &sha[..sha.len().min(7)];
            format!("{VERSION}-canary-{short}")
        }
        _ => VERSION.to_owned(),
    }
}

fn terminal_cols() -> usize {
    // Term::stdout().size() returns (rows, cols); falls back to (24, 80).
    Term::stdout().size().1 as usize
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

/// Visible display width, stripping ANSI codes and accounting for wide Unicode.
fn vlen(s: &str) -> usize {
    measure_text_width(s)
}

fn progress_bar(fraction: f32, fill_width: usize) -> String {
    let f = fraction.clamp(0.0, 1.0);
    let filled = (f * fill_width as f32).round() as usize;
    let empty = fill_width.saturating_sub(filled);
    let color = if f < 0.6 {
        "\x1b[38;2;155;217;179m" // green
    } else if f < 0.85 {
        "\x1b[38;2;234;211;156m" // amber
    } else {
        "\x1b[38;2;232;163;160m" // red
    };
    format!(
        "{color}{}{DIM}{}{RESET}",
        "━".repeat(filled),
        "─".repeat(empty)
    )
}

/// Colored dot for the current status.
fn status_dot(status: &str) -> (&'static str, &'static str) {
    match status {
        "running" => ("\x1b[38;2;155;217;179m", "●"),
        s if s.contains("launch") || s.contains("start") || s.contains("restart") => {
            ("\x1b[38;2;234;211;156m", "●")
        }
        "stopped" => ("\x1b[2m", "○"),
        s if s.contains("error") => ("\x1b[38;2;232;163;160m", "●"),
        _ => ("\x1b[2m", "●"),
    }
}

/// Shorten an absolute path by replacing the home directory with `~`.
fn fmt_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

/// Extract `user/repo` from a git remote URL (SSH or HTTPS).
fn extract_repo_slug(url: &str) -> String {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    // SSH format: git@github.com:user/repo  (no "://" scheme prefix)
    if !url.contains("://") {
        if let Some(colon_pos) = url.find(':') {
            return url[colon_pos + 1..].to_string();
        }
    }
    // HTTPS/HTTP: take last two non-empty path segments
    let parts: Vec<&str> = url.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 {
        format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        url.to_string()
    }
}

/// Derive display strings for the panel footer:
/// - `repo_slug`: `"user/repo (branch)"` from git remote, or `""` if unavailable.
/// - `repo_path`: path relative to git root (or `fmt_path(dir)` as fallback).
/// Returns `(repo_slug, repo_path, worktree_name)`.
fn git_info(dir: &std::path::Path) -> (String, String, Option<String>) {
    let dir_str = dir.to_string_lossy();

    // Find git root.
    let root_out = std::process::Command::new("git")
        .args(["-C", dir_str.as_ref(), "rev-parse", "--show-toplevel"])
        .output();

    let git_root = match root_out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => return (String::new(), fmt_path(&dir_str), None),
    };

    // Relative path from git root to project dir.
    let rel = dir
        .strip_prefix(&git_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Remote slug.
    let remote_out = std::process::Command::new("git")
        .args(["-C", dir_str.as_ref(), "remote", "get-url", "origin"])
        .output();

    let slug = match remote_out {
        Ok(out) if out.status.success() => extract_repo_slug(&String::from_utf8_lossy(&out.stdout)),
        _ => String::new(),
    };

    // Current branch.
    let branch = std::process::Command::new("git")
        .args(["-C", dir_str.as_ref(), "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let slug_with_branch = if !slug.is_empty() && !branch.is_empty() {
        format!("{slug} ({branch})")
    } else if !branch.is_empty() {
        format!("({branch})")
    } else {
        slug
    };

    // Detect worktree: --git-common-dir differs from --git-dir when inside a worktree.
    let worktree_name = detect_worktree(dir);

    (slug_with_branch, rel, worktree_name)
}

/// If the current directory is inside a git worktree, return the worktree
/// folder name (last path component of the worktree root).
///
/// In a linked worktree the toplevel `.git` is a *file* (containing
/// `gitdir: …`), whereas in the main working tree it is a directory.
fn detect_worktree(dir: &std::path::Path) -> Option<String> {
    let dir_str = dir.to_string_lossy();

    let toplevel = std::process::Command::new("git")
        .args(["-C", dir_str.as_ref(), "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !toplevel.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&toplevel.stdout).trim().to_string();
    let dot_git = std::path::Path::new(&root).join(".git");

    // In a linked worktree `.git` is a file, not a directory.
    if dot_git.is_file() {
        let folder = std::path::Path::new(&root)
            .file_name()?
            .to_string_lossy()
            .to_string();
        Some(folder)
    } else {
        None
    }
}

// ── Public formatters ─────────────────────────────────────────────────────────

/// Compact logo with left-to-right gradient. Version shown next to first row.
pub fn format_header() -> String {
    let version_str = format!("v{}", build_version_string());
    let char_count = LOGO_ROWS[0].chars().count();
    let mut lines = Vec::new();
    for (i, row) in LOGO_ROWS.iter().enumerate() {
        let mut buf = String::from(INDENT);
        for (j, ch) in row.chars().enumerate() {
            let t = if char_count <= 1 {
                0.0
            } else {
                j as f64 / (char_count - 1) as f64
            };
            let r = lerp_u8(LOGO_COLOR_START.0, LOGO_COLOR_END.0, t);
            let g = lerp_u8(LOGO_COLOR_START.1, LOGO_COLOR_END.1, t);
            let b = lerp_u8(LOGO_COLOR_START.2, LOGO_COLOR_END.2, t);
            buf.push_str(&ansi_rgb(r, g, b));
            buf.push(ch);
        }
        buf.push_str(RESET);
        if i == 0 {
            buf.push_str(&format!("  {DIM}{version_str}{RESET}"));
        }
        lines.push(buf);
    }
    lines.join("\n")
}

fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t).round() as u8
}

/// Bordered panel that adapts its layout to the terminal width.
///
/// Wide (≥ `STACKED_THRESHOLD` cols): three-column layout.
/// Narrow: vertically stacked columns.
pub fn format_panel(
    app_name: &str,
    status: &str,
    adapter_name: &str,
    repo_slug: &str,
    repo_path: &str,
    worktree_name: Option<&str>,
    hosts: &[String],
    port: u16,
    cpu: Option<f32>,
    mem_bytes: Option<u64>,
    pid: Option<usize>,
) -> String {
    let cols = terminal_cols().max(40);
    if cols < STACKED_THRESHOLD {
        format_panel_stacked(
            app_name,
            status,
            adapter_name,
            repo_slug,
            repo_path,
            worktree_name,
            hosts,
            port,
            cpu,
            mem_bytes,
            pid,
            cols,
        )
    } else {
        format_panel_wide(
            app_name,
            status,
            adapter_name,
            repo_slug,
            repo_path,
            worktree_name,
            hosts,
            port,
            cpu,
            mem_bytes,
            pid,
            cols,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn format_panel_wide(
    app_name: &str,
    status: &str,
    adapter_name: &str,
    repo_slug: &str,
    repo_path: &str,
    worktree_name: Option<&str>,
    hosts: &[String],
    port: u16,
    cpu: Option<f32>,
    mem_bytes: Option<u64>,
    pid: Option<usize>,
    cols: usize,
) -> String {
    // Warm amber — distinct from all log-level colors.
    let url_color = ansi_rgb(240, 175, 95);

    let urls: Vec<String> = hosts
        .iter()
        .map(|h| {
            if port == 443 {
                format!("https://{h}")
            } else {
                format!("https://{h}:{port}")
            }
        })
        .collect();

    // ── Column geometry ───────────────────────────────────────────────────────
    let inner_w = cols.saturating_sub(2);
    let col2_w = inner_w
        .saturating_sub(2 + COL1_W + COL_SEP + COL3_W + COL_SEP)
        .max(10);

    // ── Borders ───────────────────────────────────────────────────────────────
    let title_text = if adapter_name.trim().is_empty() {
        format!("{app_name}")
    } else {
        format!("{app_name} ({adapter_name})")
    };
    let title_seg = format!("─ {title_text} ");
    let tail = inner_w.saturating_sub(measure_text_width(&title_seg));
    let top = format!(
        "{BORDER}┌─ {PRIMARY}{title_text}{BORDER} {}┐{RESET}",
        "─".repeat(tail)
    );
    let bot = format!("{BORDER}└{}┘{RESET}", "─".repeat(inner_w));

    // ── Left column: status, worktree, repo slug, repo path ─────────────────
    let (dot_color, dot_char) = status_dot(status);
    let l0 = format!("{dot_color}{dot_char}{RESET} {DIM}{status}{RESET}");
    let mut left = vec![l0];
    if let Some(wt) = worktree_name {
        let wt_label = format!("worktree ({wt})");
        let wt_t = truncate_str(&wt_label, COL1_W, "…");
        left.push(format!("{DIM}{wt_t}{RESET}"));
    }
    if !repo_slug.is_empty() {
        let slug_t = truncate_str(repo_slug, COL1_W, "…");
        left.push(format!("{DIM}{slug_t}{RESET}"));
    }
    if !repo_path.is_empty() {
        let path_t = truncate_str(repo_path, COL1_W, "…");
        left.push(format!("{DIM}{path_t}{RESET}"));
    }

    // ── Middle column: routes ─────────────────────────────────────────────────
    let url_avail = col2_w.saturating_sub(ROUTES_LABEL_W);
    let mid: Vec<String> = urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            let url_t = truncate_str(url, url_avail, "…");
            if i == 0 {
                format!("{DIM}routes{RESET}  {url_color}{url_t}{RESET}")
            } else {
                format!("{}{url_color}{url_t}{RESET}", " ".repeat(ROUTES_LABEL_W))
            }
        })
        .collect();

    // ── Right column: cpu, ram, pid ───────────────────────────────────────────
    let r0 = if let Some(c) = cpu {
        let bar = progress_bar(c / 100.0, BAR_W);
        format!("{DIM}cpu  {RESET}{bar} {:.0}%", c)
    } else {
        format!("{DIM}cpu  {RESET}—")
    };
    let r1 = if let Some(m) = mem_bytes {
        let bar = progress_bar(m as f32 / (512.0 * 1024.0 * 1024.0), BAR_W);
        format!("{DIM}ram  {RESET}{bar} {}", fmt_bytes(m))
    } else {
        format!("{DIM}ram  {RESET}—")
    };
    let r2 = format!(
        "{DIM}pid  {RESET}{}",
        pid.map(|p| p.to_string())
            .unwrap_or_else(|| "—".to_string())
    );
    let right = vec![r0, r1, r2];

    // ── Assemble ──────────────────────────────────────────────────────────────
    let data_rows = left.len().max(mid.len()).max(right.len());
    let mut lines = vec![top];
    for i in 0..data_rows {
        let la = left.get(i).map(|s| s.as_str()).unwrap_or("");
        let ma = mid.get(i).map(|s| s.as_str()).unwrap_or("");
        let ra = right.get(i).map(|s| s.as_str()).unwrap_or("");
        lines.push(panel_row(la, ma, ra, col2_w));
    }
    lines.push(bot);
    lines.join("\n")
}

#[allow(clippy::too_many_arguments)]
fn format_panel_stacked(
    app_name: &str,
    status: &str,
    adapter_name: &str,
    repo_slug: &str,
    repo_path: &str,
    worktree_name: Option<&str>,
    hosts: &[String],
    port: u16,
    cpu: Option<f32>,
    mem_bytes: Option<u64>,
    pid: Option<usize>,
    cols: usize,
) -> String {
    let url_color = ansi_rgb(240, 175, 95);
    let inner_w = cols.saturating_sub(2);

    let title_text = if adapter_name.trim().is_empty() {
        format!("{app_name}")
    } else {
        format!("{app_name} ({adapter_name})")
    };
    let title_seg = format!("─ {title_text} ");
    let tail = inner_w.saturating_sub(measure_text_width(&title_seg));
    let top = format!(
        "{BORDER}┌─ {PRIMARY}{title_text}{BORDER} {}┐{RESET}",
        "─".repeat(tail)
    );
    let bot = format!("{BORDER}└{}┘{RESET}", "─".repeat(inner_w));

    let mut rows = vec![top];

    // Status
    let (dot_color, dot_char) = status_dot(status);
    rows.push(stacked_row(
        &format!("{dot_color}{dot_char}{RESET} {DIM}{status}{RESET}"),
        inner_w,
    ));

    // Worktree indicator
    let avail = inner_w.saturating_sub(2);
    if let Some(wt) = worktree_name {
        let wt_label = format!("worktree ({wt})");
        let wt_t = truncate_str(&wt_label, avail, "…");
        rows.push(stacked_row(&format!("{DIM}{wt_t}{RESET}"), inner_w));
    }

    // Repo slug + path
    if !repo_slug.is_empty() {
        let slug_t = truncate_str(repo_slug, avail, "…");
        rows.push(stacked_row(&format!("{DIM}{slug_t}{RESET}"), inner_w));
    }
    if !repo_path.is_empty() {
        let path_t = truncate_str(repo_path, avail, "…");
        rows.push(stacked_row(&format!("{DIM}{path_t}{RESET}"), inner_w));
    }

    // URLs
    let url_avail = inner_w.saturating_sub(2 + ROUTES_LABEL_W);
    for (i, host) in hosts.iter().enumerate() {
        let url = if port == 443 {
            format!("https://{host}")
        } else {
            format!("https://{host}:{port}")
        };
        let url_t = truncate_str(&url, url_avail, "…");
        let line = if i == 0 {
            format!("{DIM}routes{RESET}  {url_color}{url_t}{RESET}")
        } else {
            format!("{}{url_color}{url_t}{RESET}", " ".repeat(ROUTES_LABEL_W))
        };
        rows.push(stacked_row(&line, inner_w));
    }

    // Metrics: cpu + ram + pid on one line
    let cpu_str = if let Some(c) = cpu {
        format!("{DIM}cpu{RESET} {:.0}%", c)
    } else {
        format!("{DIM}cpu{RESET} —")
    };
    let ram_str = if let Some(m) = mem_bytes {
        format!("{DIM}ram{RESET} {}", fmt_bytes(m))
    } else {
        format!("{DIM}ram{RESET} —")
    };
    let pid_str = format!(
        "{DIM}pid{RESET} {}",
        pid.map(|p| p.to_string())
            .unwrap_or_else(|| "—".to_string())
    );
    rows.push(stacked_row(
        &format!("{cpu_str}  {ram_str}  {pid_str}"),
        inner_w,
    ));

    rows.push(bot);
    rows.join("\n")
}

/// One full-width stacked panel row: `│ content{padding} │`.
fn stacked_row(content: &str, inner_w: usize) -> String {
    // inner_w = cols - 2 (the two │ characters).
    // We pad: │ + space + content + padding + space + │.
    // content area = inner_w - 2 (the two spaces).
    let content_area = inner_w.saturating_sub(2);
    let pad = content_area.saturating_sub(vlen(content));
    format!(
        "{BORDER}│{RESET} {content}{} {BORDER}│{RESET}",
        " ".repeat(pad)
    )
}

/// Build one full-width wide-panel data row from three ANSI-styled cell strings.
fn panel_row(c1: &str, c2: &str, c3: &str, col2_w: usize) -> String {
    let p1 = measure_text_width(c1);
    let p2 = measure_text_width(c2);
    let p3 = measure_text_width(c3);
    format!(
        "{BORDER}│{RESET} {c1}{}{c2}{}{c3}{} {BORDER}│{RESET}",
        " ".repeat(COL1_W.saturating_sub(p1) + COL_SEP),
        " ".repeat(col2_w.saturating_sub(p2) + COL_SEP),
        " ".repeat(COL3_W.saturating_sub(p3)),
    )
}

/// Right-aligned keymap hint. Shorter variant for narrow terminals.
/// One trailing space so the text aligns with the panel's right `│`.
pub fn format_keymap() -> String {
    let cols = terminal_cols().max(20);
    let text = if cols < 52 {
        "r restart   d detach   ^c stop"
    } else {
        "r restart   d detach   ctrl+c stop"
    };
    let pad = cols.saturating_sub(measure_text_width(text) + 1);
    format!("{DIM}{}{text}{RESET} ", " ".repeat(pad))
}

/// Format a log line with ANSI color for the level token.
pub fn format_log(log: &ScopedLog) -> String {
    if log.scope == super::DIVIDER_SCOPE {
        return format!("{DIM}──── restarted ────{RESET}");
    }
    let color = level_color(&log.level);
    format!(
        "{DIM}{}{RESET} {color}{:<5}{RESET} {DIM}{}{RESET} {}",
        log.timestamp, log.level, log.scope, log.message
    )
}

fn level_color(level: &LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "\x1b[38;2;140;207;255m",
        LogLevel::Info => "\x1b[38;2;155;217;179m",
        LogLevel::Warn => "\x1b[38;2;234;211;156m",
        LogLevel::Error => "\x1b[38;2;232;163;160m",
        LogLevel::Fatal => "\x1b[38;2;200;166;242m",
    }
}

// ── Process metrics ───────────────────────────────────────────────────────────

fn collect_process_tree_pids(processes: &[(Pid, Option<Pid>)], root: Pid) -> Vec<Pid> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        out.push(pid);
        for (child_pid, parent) in processes {
            if *parent == Some(pid) {
                stack.push(*child_pid);
            }
        }
    }
    out
}

fn process_tree_metrics(sys: &System, root: Pid) -> Option<(f32, u64)> {
    sys.process(root)?;
    let index: Vec<(Pid, Option<Pid>)> = sys
        .processes()
        .iter()
        .map(|(p, pr)| (*p, pr.parent()))
        .collect();
    let pids = collect_process_tree_pids(&index, root);
    let mut cpu = 0.0_f32;
    let mut mem = 0u64;
    for pid in pids {
        if let Some(p) = sys.process(pid) {
            cpu += p.cpu_usage();
            mem += p.memory();
        }
    }
    Some((cpu, mem))
}

// ── Sticky footer ─────────────────────────────────────────────────────────────

struct StickyFooter {
    lines: Vec<String>,
    /// Terminal width at the time the footer was last drawn.
    drawn_cols: usize,
}

impl StickyFooter {
    fn new() -> Self {
        Self {
            lines: vec![],
            drawn_cols: terminal_cols().max(1),
        }
    }

    /// How many terminal rows `self.lines` occupy at the given column width.
    fn height_at_cols(&self, cols: usize) -> u16 {
        self.lines
            .iter()
            .map(|l| (vlen(l).max(1) + cols - 1) / cols)
            .sum::<usize>() as u16
    }

    fn erase(&self, out: &mut io::Stdout) {
        let current_cols = terminal_cols().max(1);
        // After a resize the terminal reflows content to the new width,
        // but the size query may still return the previous value.  Use
        // the larger of the drawn-width and current-width heights so we
        // always move up far enough to cover the old footer.
        let h = self
            .height_at_cols(current_cols)
            .max(self.height_at_cols(self.drawn_cols));
        if h > 0 {
            let _ = queue!(
                out,
                cursor::MoveUp(h),
                terminal::Clear(ClearType::FromCursorDown),
            );
        }
    }

    fn draw(&self, out: &mut io::Stdout) {
        for line in &self.lines {
            let _ = write!(out, "{}\r\n", line);
        }
        let _ = out.flush();
    }

    pub fn println(&mut self, msg: &str) {
        let mut out = io::stdout();
        self.erase(&mut out);
        let _ = write!(out, "{}\r\n", msg);
        self.drawn_cols = terminal_cols().max(1);
        self.draw(&mut out);
    }

    pub fn set(&mut self, new_lines: Vec<String>) {
        let mut out = io::stdout();
        self.erase(&mut out);
        self.lines = new_lines;
        self.drawn_cols = terminal_cols().max(1);
        self.draw(&mut out);
    }
}

// ── Shutdown signal ──────────────────────────────────────────────────────────

/// Resolves when a process-terminating signal (SIGTERM, SIGHUP) is received.
/// Used inside the output loop so that signals cause a clean exit with footer
/// cleanup instead of an abrupt process termination.
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).ok();
    let mut hup = signal(SignalKind::hangup()).ok();
    tokio::select! {
        _ = async { if let Some(s) = &mut term { s.recv().await } else { None } } => {}
        _ = async { if let Some(s) = &mut hup { s.recv().await } else { None } } => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    std::future::pending::<()>().await
}

// ── Terminal guard ────────────────────────────────────────────────────────────

struct TerminalGuard;

impl TerminalGuard {
    fn enter(app_name: &str) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Hide, SetTitle(format!("tako | {app_name}")),);
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Show, SetTitle("tako"));
    }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn rawln(s: &str) {
    let mut out = io::stdout();
    let _ = write!(out, "{}\r\n", s);
    let _ = out.flush();
}

fn spawn_key_reader(tx: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        loop {
            match crossterm::event::read() {
                Ok(event) => {
                    if tx.blocking_send(event).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

// ── Footer state ──────────────────────────────────────────────────────────────

struct FooterState {
    repo_slug: String,
    repo_path: String,
    worktree_name: Option<String>,
    status: String,
    cpu: Option<f32>,
    mem_bytes: Option<u64>,
    pid: Option<usize>,
}

impl FooterState {
    fn new(repo_slug: String, repo_path: String, worktree_name: Option<String>) -> Self {
        Self {
            repo_slug,
            repo_path,
            worktree_name,
            status: "starting...".to_string(),
            cpu: None,
            mem_bytes: None,
            pid: None,
        }
    }

    fn build_lines(
        &self,
        app_name: &str,
        adapter_name: &str,
        hosts: &[String],
        port: u16,
    ) -> Vec<String> {
        let mut lines = format_panel(
            app_name,
            &self.status,
            adapter_name,
            &self.repo_slug,
            &self.repo_path,
            self.worktree_name.as_deref(),
            hosts,
            port,
            self.cpu,
            self.mem_bytes,
            self.pid,
        )
        .lines()
        .map(|l| l.to_string())
        .collect::<Vec<_>>();
        lines.push(format_keymap());
        // Blank line above the panel separates it from the log stream.
        lines.insert(0, String::new());
        lines
    }

    fn refresh(
        &self,
        footer: &mut StickyFooter,
        app_name: &str,
        adapter_name: &str,
        hosts: &[String],
        port: u16,
    ) {
        footer.set(self.build_lines(app_name, adapter_name, hosts, port));
    }
}

// ── Loop exit tag (avoids moving channels inside select!) ─────────────────────

enum LoopExit {
    Terminate,
    Detach,
    Message(String),
}

// ── Main entry point ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run_dev_output(
    app_name: String,
    adapter_name: String,
    hosts: Vec<String>,
    port: u16,
    app_port: u16,
    mut log_rx: mpsc::Receiver<ScopedLog>,
    mut event_rx: mpsc::Receiver<DevEvent>,
    control_tx: mpsc::Sender<ControlCmd>,
    log_store_path: Option<PathBuf>,
) -> Result<DevOutputExit, Box<dyn std::error::Error>> {
    let _ = app_port;

    let _guard = TerminalGuard::enter(&app_name)?;

    rawln("");
    for line in format_header().lines() {
        rawln(line);
    }
    rawln("");

    let (repo_slug, repo_path, worktree_name) = std::env::current_dir()
        .map(|cwd| git_info(&cwd))
        .unwrap_or_default();

    let mut footer = StickyFooter::new();
    let mut fs = FooterState::new(repo_slug, repo_path, worktree_name);
    fs.refresh(&mut footer, &app_name, &adapter_name, &hosts, port);

    let (key_tx, mut key_rx) = mpsc::channel::<Event>(64);
    spawn_key_reader(key_tx);

    let mut sys = System::new();
    // 1-second ticker drives metrics refresh (every 2nd tick).
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    let mut tick_count = 0u64;
    let mut app_pid: Option<Pid> = None;

    // Catch SIGTERM / SIGHUP so the footer is cleaned up on signal-based exit.
    let sig = shutdown_signal();
    tokio::pin!(sig);

    // We break with a LoopExit tag to avoid moving log_rx/event_rx inside
    // the select! arms (they're borrowed by recv() arms).
    let loop_exit = loop {
        tokio::select! {
            _ = &mut sig => {
                break LoopExit::Terminate;
            }
            _ = ticker.tick() => {
                tick_count += 1;

                // Refresh metrics every 2 seconds; only redraw if values changed.
                if tick_count % METRICS_REFRESH_SECS == 0 {
                    if let Some(pid) = app_pid {
                        sys.refresh_processes(ProcessesToUpdate::All, false);
                        if let Some((cpu, mem)) = process_tree_metrics(&sys, pid) {
                            let changed = fs.cpu != Some(cpu) || fs.mem_bytes != Some(mem);
                            fs.cpu = Some(cpu);
                            fs.mem_bytes = Some(mem);
                            if changed {
                                fs.refresh(&mut footer, &app_name, &adapter_name, &hosts, port);
                            }
                        }
                    }
                }
            }
            Some(log) = log_rx.recv() => {
                if let Some(path) = log_store_path.as_ref() {
                    super::append_log_to_store(path, &log).await;
                }
                footer.println(&format_log(&log));
            }
            event = event_rx.recv() => {
                let Some(event) = event else {
                    // All event senders dropped — client ended.
                    break LoopExit::Terminate;
                };
                match event {
                    DevEvent::AppStarted => {
                        fs.status = "running".to_string();
                        if let Some(pid) = app_pid {
                            sys.refresh_processes(ProcessesToUpdate::All, false);
                            if let Some((cpu, mem)) = process_tree_metrics(&sys, pid) {
                                fs.cpu = Some(cpu);
                                fs.mem_bytes = Some(mem);
                            }
                        }
                        fs.refresh(&mut footer, &app_name, &adapter_name, &hosts, port);
                    }
                    DevEvent::AppLaunching => {
                        fs.status = "launching...".to_string();
                        fs.refresh(&mut footer, &app_name, &adapter_name, &hosts, port);
                    }
                    DevEvent::AppStopped => {
                        app_pid = None;
                        fs.cpu = None;
                        fs.mem_bytes = None;
                        fs.pid = None;
                        fs.status = "stopped".to_string();
                        fs.refresh(&mut footer, &app_name, &adapter_name, &hosts, port);
                    }
                    DevEvent::AppPid(pid) => {
                        app_pid = Some(Pid::from(pid as usize));
                        fs.pid = Some(pid as usize);
                        fs.refresh(&mut footer, &app_name, &adapter_name, &hosts, port);
                    }
                    DevEvent::AppError(ref e) => {
                        fs.status = "error".to_string();
                        fs.refresh(&mut footer, &app_name, &adapter_name, &hosts, port);
                        footer.println(&format!("\x1b[38;2;232;163;160merror:{RESET} {e}"));
                    }
                    DevEvent::ExitWithMessage(msg) => {
                        break LoopExit::Message(msg);
                    }
                    DevEvent::LogsCleared | DevEvent::LogsReady => {}
                }
            }
            Some(event) = key_rx.recv() => {
                match event {
                    Event::Key(key) => match key.code {
                        KeyCode::Char('c')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            let _ = control_tx.send(ControlCmd::Terminate).await;
                            break LoopExit::Terminate;
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            let _ = control_tx.send(ControlCmd::Restart).await;
                        }
                        KeyCode::Char('d') | KeyCode::Char('D') => {
                            break LoopExit::Detach;
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {
                        fs.refresh(&mut footer, &app_name, &adapter_name, &hosts, port);
                    }
                    _ => {}
                }
            }
        }
    };

    // Erase the footer before exiting so the terminal is clean.
    {
        let mut out = io::stdout();
        footer.erase(&mut out);
        let _ = out.flush();
    }

    // Restore terminal state *before* printing the exit line.
    // Dropping the guard here (rather than letting it drop at end-of-scope)
    // avoids the extra blank line that can appear when raw mode is disabled
    // after a \r\n has already moved the cursor down.
    drop(_guard);

    // Build the exit value (now that log_rx/event_rx are no longer borrowed).
    let exit = match loop_exit {
        LoopExit::Terminate => {
            println!("{DIM}{app_name} stopped{RESET}");
            DevOutputExit::Terminate
        }
        LoopExit::Detach => {
            println!("{DIM}{app_name} detached — run `tako dev` to re-attach{RESET}");
            DevOutputExit::Detach { log_rx, event_rx }
        }
        LoopExit::Message(msg) => {
            println!("{DIM}{app_name} {msg}{RESET}");
            DevOutputExit::Terminate
        }
    };

    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::super::LogLevel;
    use super::*;
    use console::strip_ansi_codes;

    #[test]
    fn collect_process_tree_pids_includes_descendants() {
        let root = Pid::from_u32(10);
        let child = Pid::from_u32(11);
        let grandchild = Pid::from_u32(12);
        let unrelated = Pid::from_u32(99);
        let got = collect_process_tree_pids(
            &[
                (root, None),
                (child, Some(root)),
                (grandchild, Some(child)),
                (unrelated, None),
            ],
            root,
        );
        assert!(got.contains(&root));
        assert!(got.contains(&child));
        assert!(got.contains(&grandchild));
        assert!(!got.contains(&unrelated));
    }

    #[test]
    fn collect_process_tree_pids_handles_parent_cycle() {
        let root = Pid::from_u32(1);
        let child = Pid::from_u32(2);
        let got = collect_process_tree_pids(&[(root, Some(child)), (child, Some(root))], root);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn format_log_fields() {
        let log = ScopedLog {
            timestamp: "12:34:56".to_string(),
            level: LogLevel::Info,
            scope: "app".to_string(),
            message: "hello".to_string(),
        };
        let out = format_log(&log);
        assert!(out.contains("12:34:56"));
        assert!(out.contains("INFO"));
        assert!(out.contains("app"));
        assert!(out.contains("hello"));
    }

    #[test]
    fn format_header_has_logo_and_version() {
        let h = format_header();
        // Logo contains block characters.
        assert!(h.contains('█'));
        // Version string is present on the first line (next to logo).
        let first_line = h.lines().next().unwrap();
        assert!(first_line.contains('v'));
    }

    #[test]
    fn format_header_has_per_char_gradient() {
        let h = format_header();
        assert_eq!(h.lines().count(), LOGO_ROWS.len());
        // The gradient uses different colors — check the first row
        // contains ANSI RGB escapes from both ends of the gradient.
        let first = h.lines().next().unwrap();
        assert!(first.contains(&format!(
            "\x1b[38;2;{};{};{}m",
            LOGO_COLOR_START.0, LOGO_COLOR_START.1, LOGO_COLOR_START.2
        )));
        assert!(first.contains(&format!(
            "\x1b[38;2;{};{};{}m",
            LOGO_COLOR_END.0, LOGO_COLOR_END.1, LOGO_COLOR_END.2
        )));
    }

    #[test]
    fn format_panel_has_border_and_app_name_with_runtime() {
        let panel = format_panel(
            "myapp",
            "running",
            "bun",
            "user/myapp",
            "apps/myapp",
            None,
            &["myapp.tako".to_string()],
            443,
            None,
            None,
            None,
        );
        assert!(panel.contains('┌'));
        assert!(panel.contains('└'));
        assert!(panel.contains("myapp (bun)"));
    }

    #[test]
    fn format_panel_shows_routes_label() {
        let panel = format_panel(
            "app",
            "running",
            "bun",
            "user/app",
            "apps/app",
            None,
            &["app.tako".to_string()],
            443,
            None,
            None,
            None,
        );
        let plain = strip_ansi(&panel);
        assert!(plain.contains("routes"));
        assert!(plain.contains("https://app.tako"));
    }

    #[test]
    fn format_panel_shows_all_urls() {
        let hosts = vec!["a.tako".to_string(), "b.tako".to_string()];
        let panel = format_panel(
            "app", "running", "bun", "u/r", "", None, &hosts, 443, None, None, None,
        );
        let plain = strip_ansi(&panel);
        assert!(plain.contains("https://a.tako"));
        assert!(plain.contains("https://b.tako"));
    }

    #[test]
    fn format_panel_shows_wildcard_and_path_routes() {
        let hosts = vec![
            "bun-example.tako".to_string(),
            "bun-example.tako/bun".to_string(),
            "*.bun-example.tako".to_string(),
        ];
        // Use wide terminal so URLs aren't truncated.
        let panel = format_panel_wide(
            "bun-example",
            "running",
            "bun",
            "u/r",
            "",
            None,
            &hosts,
            443,
            None,
            None,
            None,
            120,
        );
        let plain = strip_ansi(&panel);
        assert!(
            plain.contains("https://bun-example.tako/bun"),
            "missing /bun route"
        );
        assert!(
            plain.contains("https://*.bun-example.tako"),
            "missing wildcard route"
        );
        // Verify all 3 routes are present (default host + 2 configured).
        assert_eq!(
            plain.matches("https://").count(),
            3,
            "expected exactly 3 route URLs"
        );
    }

    #[test]
    fn format_panel_omits_443_port() {
        let panel = format_panel(
            "app",
            "running",
            "",
            "",
            "",
            None,
            &["app.tako".to_string()],
            443,
            None,
            None,
            None,
        );
        assert!(!strip_ansi(&panel).contains(":443"));
    }

    #[test]
    fn format_panel_includes_custom_port() {
        // Use wide layout directly — custom port URLs need more cols than 80.
        let panel = format_panel_wide(
            "app",
            "running",
            "",
            "",
            "",
            None,
            &["app.tako".to_string()],
            47831,
            None,
            None,
            None,
            120,
        );
        assert!(strip_ansi(&panel).contains(":47831"));
    }

    #[test]
    fn format_panel_shows_metrics() {
        let panel = format_panel(
            "app",
            "running",
            "",
            "",
            "",
            None,
            &["app.tako".to_string()],
            443,
            Some(50.0),
            Some(100 * 1024 * 1024),
            Some(9999),
        );
        let plain = strip_ansi(&panel);
        assert!(plain.contains("50%") || plain.contains("50"));
        assert!(plain.contains("100 MB"));
        assert!(plain.contains("9999"));
    }

    #[test]
    fn format_panel_shows_dash_without_metrics() {
        let panel = format_panel(
            "app",
            "running",
            "",
            "",
            "",
            None,
            &["app.tako".to_string()],
            443,
            None,
            None,
            None,
        );
        assert!(strip_ansi(&panel).contains('—'));
    }

    #[test]
    fn format_panel_shows_repo_info() {
        let panel = format_panel(
            "app",
            "running",
            "bun",
            "myorg/myrepo",
            "apps/myapp",
            None,
            &["app.tako".to_string()],
            443,
            None,
            None,
            None,
        );
        let plain = strip_ansi(&panel);
        assert!(plain.contains("myorg/myrepo"));
        assert!(plain.contains("apps/myapp"));
    }

    #[test]
    fn format_panel_stacked_has_border_and_content() {
        // Force stacked layout by using a narrow cols environment.
        // The stacked formatter is invoked directly at cols < STACKED_THRESHOLD.
        let panel = format_panel_stacked(
            "app",
            "running",
            "bun",
            "user/repo",
            "projects/app",
            None,
            &["app.tako".to_string()],
            443,
            Some(25.0),
            Some(50 * 1024 * 1024),
            Some(1234),
            60,
        );
        let plain = strip_ansi(&panel);
        assert!(plain.contains('┌'));
        assert!(plain.contains('└'));
        assert!(plain.contains("app"));
        assert!(plain.contains("routes"));
        assert!(plain.contains("https://app.tako"));
        assert!(plain.contains("cpu"));
        assert!(plain.contains("ram"));
        assert!(plain.contains("pid"));
        assert!(plain.contains("1234"));
    }

    #[test]
    fn format_keymap_has_restart_stop_detach() {
        let km = strip_ansi(&format_keymap());
        assert!(km.contains('r'));
        assert!(km.contains("restart"));
        assert!(km.contains("stop"));
        assert!(km.contains('d'));
        // No 'q' quit
        assert!(!km.contains("quit"));
    }

    #[test]
    fn progress_bar_extremes() {
        let full = strip_ansi(&progress_bar(1.0, 8));
        let empty = strip_ansi(&progress_bar(0.0, 8));
        assert!(full.contains("━━━━━━━━"));
        assert!(empty.contains("────────"));
    }

    #[test]
    fn vlen_strips_ansi() {
        assert_eq!(vlen(&format!("{DIM}hello{RESET}")), 5);
        // Wide Unicode chars count as 2 columns each.
        assert_eq!(vlen("AB"), 2);
    }

    #[test]
    fn trunc_at_limit() {
        assert_eq!(truncate_str("hello", 10, "…").as_ref(), "hello");
        assert_eq!(measure_text_width(&truncate_str("hello world", 7, "…")), 7);
    }

    #[test]
    fn height_at_cols_accounts_for_wrapping() {
        let mut footer = StickyFooter::new();
        footer.lines = vec!["a".repeat(10)];
        // 10-char line fits in 1 row at 80 cols.
        assert_eq!(footer.height_at_cols(80), 1);
        // 10-char line wraps to 2 rows at 6 cols.
        assert_eq!(footer.height_at_cols(6), 2);
        // Multiple lines.
        footer.lines = vec!["a".repeat(10), "b".repeat(20)];
        // At 80 cols: 1 + 1 = 2.
        assert_eq!(footer.height_at_cols(80), 2);
        // At 6 cols: ceil(10/6)=2 + ceil(20/6)=4 = 6.
        assert_eq!(footer.height_at_cols(6), 6);
    }

    #[test]
    fn extract_repo_slug_ssh_url() {
        assert_eq!(
            extract_repo_slug("git@github.com:user/repo.git"),
            "user/repo"
        );
        assert_eq!(
            extract_repo_slug("git@gitlab.com:org/project"),
            "org/project"
        );
    }

    #[test]
    fn extract_repo_slug_https_url() {
        assert_eq!(
            extract_repo_slug("https://github.com/user/repo.git"),
            "user/repo"
        );
        assert_eq!(
            extract_repo_slug("https://github.com/user/repo"),
            "user/repo"
        );
        assert_eq!(
            extract_repo_slug("https://github.com/user/repo/"),
            "user/repo"
        );
    }

    #[test]
    fn format_panel_shows_worktree_indicator() {
        let panel = format_panel(
            "app",
            "running",
            "bun",
            "user/repo",
            "apps/app",
            Some("wt1"),
            &["app.tako".to_string()],
            443,
            None,
            None,
            None,
        );
        let plain = strip_ansi(&panel);
        assert!(plain.contains("worktree (wt1)"));
    }

    #[test]
    fn format_panel_omits_worktree_when_none() {
        let panel = format_panel(
            "app",
            "running",
            "bun",
            "user/repo",
            "apps/app",
            None,
            &["app.tako".to_string()],
            443,
            None,
            None,
            None,
        );
        let plain = strip_ansi(&panel);
        assert!(!plain.contains("worktree"));
    }

    fn strip_ansi(s: &str) -> String {
        strip_ansi_codes(s).into_owned()
    }
}
