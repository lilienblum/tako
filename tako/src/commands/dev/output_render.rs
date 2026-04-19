use console::{Term, measure_text_width, truncate_str};

use super::{LogLevel, ScopedLog};

pub(super) const RESET: &str = "\x1b[0m";
pub(super) const DIM: &str = "\x1b[2m";
#[allow(dead_code)]
const XDIM: &str = "\x1b[2;38;5;242m";
const BORDER: &str = "\x1b[2;38;2;79;107;122m";

const STACKED_THRESHOLD: usize = 76;
const COL3_W: usize = 22;
const COL_SEP: usize = 2;
const BAR_W: usize = 8;
const ROUTES_LABEL_W: usize = 8;
const SCOPE_MIN: usize = 4;
const SCOPE_MAX: usize = 12;

fn muted(s: &str) -> String {
    format!("{DIM}{s}{RESET}")
}

fn ansi_rgb(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn split_route_pattern(route: &str) -> (&str, Option<&str>) {
    match route.find('/') {
        Some(idx) => (&route[..idx], Some(&route[idx..])),
        None => (route, None),
    }
}

fn terminal_cols() -> usize {
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

pub(super) fn vlen(s: &str) -> usize {
    measure_text_width(s)
}

pub(super) fn progress_bar(fraction: f32, fill_width: usize) -> String {
    let f = fraction.clamp(0.0, 1.0);
    let filled = (f * fill_width as f32).round() as usize;
    let empty = fill_width.saturating_sub(filled);

    let (r, g, b) = if f < 0.5 {
        let t = f / 0.5;
        (
            (155.0 + t * 79.0) as u8,
            (217.0 - t * 6.0) as u8,
            (179.0 - t * 23.0) as u8,
        )
    } else {
        let t = (f - 0.5) / 0.5;
        (
            (234.0 - t * 2.0) as u8,
            (211.0 - t * 48.0) as u8,
            (156.0 + t * 4.0) as u8,
        )
    };

    let mut buf = String::with_capacity(fill_width * 20);
    if filled > 0 {
        buf.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
        for _ in 0..filled {
            buf.push('█');
        }
    }
    if empty > 0 {
        buf.push_str(DIM);
        for _ in 0..empty {
            buf.push('⣿');
        }
    }
    buf.push_str(RESET);
    buf
}

fn status_dot(status: &str) -> (&'static str, &'static str) {
    match status {
        "running" => ("\x1b[38;2;155;217;179m", "●"),
        s if s.contains("launch") || s.contains("start") || s.contains("restart") => {
            ("\x1b[38;2;234;211;156m", "●")
        }
        "stopped" => ("\x1b[2m", "○"),
        "exited" => ("\x1b[38;2;232;163;160m", "●"),
        s if s.contains("error") => ("\x1b[38;2;232;163;160m", "●"),
        _ => ("\x1b[2m", "●"),
    }
}

fn fmt_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

pub(super) fn extract_repo_slug(url: &str) -> String {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    if !url.contains("://")
        && let Some(colon_pos) = url.find(':')
    {
        return url[colon_pos + 1..].to_string();
    }
    let parts: Vec<&str> = url.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 {
        format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        url.to_string()
    }
}

pub(super) fn git_info(dir: &std::path::Path) -> (String, String, String, Option<String>) {
    let dir_str = dir.to_string_lossy();

    let root_out = std::process::Command::new("git")
        .args(["-C", dir_str.as_ref(), "rev-parse", "--show-toplevel"])
        .output();

    let git_root = match root_out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => return (String::new(), String::new(), fmt_path(&dir_str), None),
    };

    let rel = dir
        .strip_prefix(&git_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let remote_out = std::process::Command::new("git")
        .args(["-C", dir_str.as_ref(), "remote", "get-url", "origin"])
        .output();

    let slug = match remote_out {
        Ok(out) if out.status.success() => extract_repo_slug(&String::from_utf8_lossy(&out.stdout)),
        _ => String::new(),
    };

    let branch = std::process::Command::new("git")
        .args(["-C", dir_str.as_ref(), "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let worktree_name = detect_worktree(dir);

    (slug, branch, rel, worktree_name)
}

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

pub(super) fn format_header() -> String {
    crate::output::format_logo_header()
}

/// Build the panel title. Returns (visible_text, rendered_text); the visible
/// form is used for column-width math, the rendered form carries ANSI styling.
/// Combines the repo slug and folder path into a single locator so monorepo
/// subprojects are unambiguous.
fn panel_title(app_name: &str, repo_slug: &str, repo_path: &str) -> (String, String) {
    let locator = match (repo_slug.is_empty(), repo_path.is_empty()) {
        (true, true) => return (app_name.to_string(), app_name.to_string()),
        (false, true) => repo_slug.to_string(),
        (true, false) => repo_path.to_string(),
        (false, false) => format!("{repo_slug}/{repo_path}"),
    };
    (
        format!("{app_name} ({locator})"),
        format!("{app_name} {DIM}({locator}){RESET}"),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn format_panel(
    app_name: &str,
    status: &str,
    adapter_name: &str,
    repo_slug: &str,
    repo_branch: &str,
    repo_path: &str,
    worktree_name: Option<&str>,
    hosts: &[String],
    port: u16,
    app_port: u16,
    cpu: Option<f32>,
    mem_bytes: Option<u64>,
) -> String {
    let cols = terminal_cols().max(40);
    if cols < STACKED_THRESHOLD {
        format_panel_stacked(
            app_name,
            status,
            adapter_name,
            repo_slug,
            repo_branch,
            repo_path,
            worktree_name,
            hosts,
            port,
            app_port,
            cpu,
            mem_bytes,
            cols,
        )
    } else {
        format_panel_wide(
            app_name,
            status,
            adapter_name,
            repo_slug,
            repo_branch,
            repo_path,
            worktree_name,
            hosts,
            port,
            app_port,
            cpu,
            mem_bytes,
            cols,
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn format_panel_wide(
    app_name: &str,
    status: &str,
    _adapter_name: &str,
    repo_slug: &str,
    repo_branch: &str,
    repo_path: &str,
    worktree_name: Option<&str>,
    hosts: &[String],
    port: u16,
    app_port: u16,
    cpu: Option<f32>,
    mem_bytes: Option<u64>,
    cols: usize,
) -> String {
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

    let inner_w = cols.saturating_sub(2);
    let shared = inner_w.saturating_sub(2 + COL3_W + 2 * COL_SEP);
    let col1_w = (shared / 3).max(10);
    let col2_w = shared.saturating_sub(col1_w).max(10);

    let (title_visible, title_render) = panel_title(app_name, repo_slug, repo_path);
    let title_seg = format!("─ {title_visible} ");
    let tail = inner_w.saturating_sub(measure_text_width(&title_seg));
    let top = format!(
        "{BORDER}┌─ {RESET}{title_render}{BORDER} {}┐{RESET}",
        "─".repeat(tail)
    );
    let bot = format!("{BORDER}└{}┘{RESET}", "─".repeat(inner_w));

    let (dot_color, dot_char) = status_dot(status);
    let l0 = format!("{dot_color}{dot_char} {status}{RESET}");
    let mut left = vec![l0];
    if let Some(wt) = worktree_name {
        let wt_label = format!("worktree ({wt})");
        let wt_t = truncate_str(&wt_label, col1_w, "…");
        left.push(muted(&wt_t));
    }
    if !repo_branch.is_empty() {
        left.push(format!("{} {repo_branch}", muted("\u{e0a0}")));
    }

    let url_avail = col2_w.saturating_sub(ROUTES_LABEL_W);
    let mid: Vec<String> = urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            let url_t = truncate_str(url, url_avail, "…");
            if i == 0 {
                format!("{}  {url_color}{url_t}{RESET}", muted("routes"))
            } else {
                format!("{}{url_color}{url_t}{RESET}", " ".repeat(ROUTES_LABEL_W))
            }
        })
        .collect();

    let r0 = if let Some(c) = cpu {
        let bar = progress_bar(c / 100.0, BAR_W);
        format!("{}  {bar} {:.0}%", muted("cpu"), c)
    } else {
        format!("{}  —", muted("cpu"))
    };
    let r1 = if let Some(m) = mem_bytes {
        format!("{}  {}", muted("ram"), fmt_bytes(m))
    } else {
        format!("{}  —", muted("ram"))
    };
    let r2 = format!("{} {app_port}", muted("port"));
    let right = [r0, r1, r2];

    let data_rows = left.len().max(mid.len()).max(right.len());
    let mut lines = vec![top];
    for i in 0..data_rows {
        let la = left.get(i).map(|s| s.as_str()).unwrap_or("");
        let ma = mid.get(i).map(|s| s.as_str()).unwrap_or("");
        let ra = right.get(i).map(|s| s.as_str()).unwrap_or("");
        lines.push(panel_row(la, ma, ra, col1_w, col2_w));
    }
    lines.push(bot);
    lines.join("\n")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn format_panel_stacked(
    app_name: &str,
    status: &str,
    _adapter_name: &str,
    repo_slug: &str,
    repo_branch: &str,
    repo_path: &str,
    worktree_name: Option<&str>,
    hosts: &[String],
    port: u16,
    app_port: u16,
    cpu: Option<f32>,
    mem_bytes: Option<u64>,
    cols: usize,
) -> String {
    let url_color = ansi_rgb(240, 175, 95);
    let inner_w = cols.saturating_sub(2);

    let (title_visible, title_render) = panel_title(app_name, repo_slug, repo_path);
    let title_seg = format!("─ {title_visible} ");
    let tail = inner_w.saturating_sub(measure_text_width(&title_seg));
    let top = format!(
        "{BORDER}┌─ {RESET}{title_render}{BORDER} {}┐{RESET}",
        "─".repeat(tail)
    );
    let bot = format!("{BORDER}└{}┘{RESET}", "─".repeat(inner_w));

    let mut rows = vec![top];

    let (dot_color, dot_char) = status_dot(status);
    rows.push(stacked_row(
        &format!("{dot_color}{dot_char} {status}{RESET}"),
        inner_w,
    ));

    let avail = inner_w.saturating_sub(2);
    if let Some(wt) = worktree_name {
        let wt_label = format!("worktree ({wt})");
        let wt_t = truncate_str(&wt_label, avail, "…");
        rows.push(stacked_row(&muted(&wt_t), inner_w));
    }

    if !repo_branch.is_empty() {
        rows.push(stacked_row(
            &format!("{} {repo_branch}", muted("\u{e0a0}")),
            inner_w,
        ));
    }
    let url_avail = inner_w.saturating_sub(2 + ROUTES_LABEL_W);
    for (i, host) in hosts.iter().enumerate() {
        let url = if port == 443 {
            format!("https://{host}")
        } else {
            format!("https://{host}:{port}")
        };
        let url_t = truncate_str(&url, url_avail, "…");
        let line = if i == 0 {
            format!("{}  {url_color}{url_t}{RESET}", muted("routes"))
        } else {
            format!("{}{url_color}{url_t}{RESET}", " ".repeat(ROUTES_LABEL_W))
        };
        rows.push(stacked_row(&line, inner_w));
    }

    let cpu_str = if let Some(c) = cpu {
        format!("{} {:.0}%", muted("cpu"), c)
    } else {
        format!("{} —", muted("cpu"))
    };
    let ram_str = if let Some(m) = mem_bytes {
        format!("{} {}", muted("ram"), fmt_bytes(m))
    } else {
        format!("{} —", muted("ram"))
    };
    rows.push(stacked_row(&format!("{cpu_str}  {ram_str}"), inner_w));
    rows.push(stacked_row(
        &format!("{} {app_port}", muted("port")),
        inner_w,
    ));

    rows.push(bot);
    rows.join("\n")
}

fn stacked_row(content: &str, inner_w: usize) -> String {
    let content_area = inner_w.saturating_sub(2);
    let pad = content_area.saturating_sub(vlen(content));
    format!(
        "{BORDER}│{RESET} {content}{} {BORDER}│{RESET}",
        " ".repeat(pad)
    )
}

fn panel_row(c1: &str, c2: &str, c3: &str, col1_w: usize, col2_w: usize) -> String {
    let p1 = measure_text_width(c1);
    let p2 = measure_text_width(c2);
    let p3 = measure_text_width(c3);
    format!(
        "{BORDER}│{RESET} {c1}{}{c2}{}{c3}{} {BORDER}│{RESET}",
        " ".repeat(col1_w.saturating_sub(p1) + COL_SEP),
        " ".repeat(col2_w.saturating_sub(p2) + COL_SEP),
        " ".repeat(COL3_W.saturating_sub(p3)),
    )
}

pub(super) fn format_keymap() -> String {
    let cols = terminal_cols().max(20);
    let text = if cols < 60 {
        format!(
            "l {}   r {}   b {}   ^c/q {}",
            muted("lan"),
            muted("restart"),
            muted("background"),
            muted("stop")
        )
    } else {
        format!(
            "l {}   r {}   b {}   ctrl+c/q {}",
            muted("lan"),
            muted("restart"),
            muted("background"),
            muted("stop")
        )
    };
    let plain = if cols < 60 {
        "l lan   r restart   b background   ^c/q stop"
    } else {
        "l lan   r restart   b background   ctrl+c/q stop"
    };
    let pad = cols.saturating_sub(measure_text_width(plain) + 1);
    format!("{}{text} ", " ".repeat(pad))
}

pub(super) fn fit_scope(scope: &str) -> String {
    let len = scope.len();
    if len <= SCOPE_MAX {
        format!("{scope:<SCOPE_MIN$}")
    } else {
        format!("{}\u{2026}", &scope[..SCOPE_MAX - 1])
    }
}

pub(super) fn format_log(log: &ScopedLog) -> String {
    if let Some(kind) = log.kind.as_deref() {
        let label = kind.replace('_', " ");
        return muted(&format!("──── {label} ────"));
    }
    if let Some(ip) = log
        .scope
        .eq("tako")
        .then(|| log.message.strip_prefix("LAN mode enabled ("))
        .flatten()
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let color = level_color(&log.level);
        let scope = fit_scope(&log.scope);
        let rendered_scope = render_scope(&log.scope, &scope);
        return format!(
            "{DIM}{}{RESET} {color}{:>5}{RESET} {rendered_scope} LAN mode enabled {DIM}({ip}){RESET}",
            log.timestamp, log.level
        );
    }
    if matches!(log.level, LogLevel::Debug) {
        let scope = fit_scope(&log.scope);
        let rendered_scope = render_scope(&log.scope, &scope);
        let pad_width = message_column_width(scope.len());
        let mut lines = log.message.split('\n');
        let first = lines.next().unwrap_or("");
        let mut out = format!(
            "{DIM}{} {:>5}{RESET} {rendered_scope} {DIM}{first}{RESET}",
            log.timestamp, log.level
        );
        for line in lines {
            out.push('\n');
            out.push_str(&" ".repeat(pad_width));
            out.push_str(&format!("{DIM}{line}{RESET}"));
        }
        return out;
    }
    let color = level_color(&log.level);
    let scope = fit_scope(&log.scope);
    let rendered_scope = render_scope(&log.scope, &scope);
    let pad_width = message_column_width(scope.len());
    let mut lines = log.message.split('\n');
    let first = lines.next().unwrap_or("");
    let mut out = format!(
        "{DIM}{}{RESET} {color}{:>5}{RESET} {rendered_scope} {first}",
        log.timestamp, log.level
    );
    for line in lines {
        out.push('\n');
        out.push_str(&" ".repeat(pad_width));
        out.push_str(line);
    }
    out
}

/// Visible width of the timestamp/level/scope prefix up to where the message
/// starts, used to indent continuation lines of multi-line messages so they
/// align under the first line's message column.
fn message_column_width(scope_visible_width: usize) -> usize {
    // "HH:MM:SS" (8) + " " + level right-aligned to 5 + " " + scope + " "
    8 + 1 + 5 + 1 + scope_visible_width + 1
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

const SCOPE_PALETTE: &[(u8, u8, u8)] = &[
    (138, 198, 209),
    (194, 178, 128),
    (176, 186, 140),
    (190, 168, 206),
    (140, 195, 174),
    (209, 170, 160),
    (160, 190, 210),
    (200, 180, 170),
];

static APP_RUNTIME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record the project runtime (e.g. "bun", "node", "deno", "go") so the "app"
/// scope can be tinted with the runtime's brand color. Idempotent; first call wins.
pub(super) fn set_app_runtime(runtime: impl Into<String>) {
    let _ = APP_RUNTIME.set(runtime.into());
}

fn app_runtime() -> Option<&'static str> {
    APP_RUNTIME.get().map(String::as_str)
}

/// Render the padded scope label with ANSI color. Known scopes get gradients;
/// everything else falls back to a hash-derived solid color from the palette.
fn render_scope(raw: &str, padded: &str) -> String {
    if let Some(stops) = scope_gradient(raw) {
        return apply_gradient(padded, raw, stops);
    }
    let (r, g, b) = scope_solid(raw);
    format!("\x1b[38;2;{r};{g};{b}m{padded}{RESET}")
}

fn scope_gradient(scope: &str) -> Option<&'static [(u8, u8, u8)]> {
    match scope {
        "tako" => Some(&[(232, 135, 131), (240, 195, 160)]),
        "vite" => Some(&[(143, 90, 200), (189, 132, 230)]),
        "app" => app_runtime().and_then(runtime_gradient),
        _ => None,
    }
}

fn runtime_gradient(runtime: &str) -> Option<&'static [(u8, u8, u8)]> {
    match runtime {
        "bun" => Some(&[(251, 240, 223), (244, 113, 181)]),
        "node" => Some(&[(60, 135, 58), (140, 200, 75)]),
        "deno" => Some(&[(60, 200, 140), (112, 255, 175)]),
        "go" => Some(&[(0, 173, 216), (93, 201, 226)]),
        _ => None,
    }
}

fn scope_solid(scope: &str) -> (u8, u8, u8) {
    match scope {
        "app" => (200, 200, 190),
        _ => {
            let hash = scope
                .bytes()
                .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
            SCOPE_PALETTE[hash as usize % SCOPE_PALETTE.len()]
        }
    }
}

/// Apply per-character gradient interpolation across the visible chars of the
/// scope name. Padding chars (after the raw name) are emitted uncolored.
fn apply_gradient(padded: &str, raw: &str, stops: &[(u8, u8, u8)]) -> String {
    let visible = raw.chars().count();
    let mut out = String::with_capacity(padded.len() + visible * 20);
    for (i, ch) in padded.chars().enumerate() {
        if i < visible {
            let t = if visible > 1 {
                i as f32 / (visible - 1) as f32
            } else {
                0.0
            };
            let (r, g, b) = sample_stops(stops, t);
            out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out.push_str(RESET);
    out
}

fn sample_stops(stops: &[(u8, u8, u8)], t: f32) -> (u8, u8, u8) {
    if stops.len() == 1 {
        return stops[0];
    }
    let scaled = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let idx = (scaled.floor() as usize).min(stops.len() - 2);
    let local = scaled - idx as f32;
    let (r0, g0, b0) = stops[idx];
    let (r1, g1, b1) = stops[idx + 1];
    (
        lerp_u8(r0, r1, local),
        lerp_u8(g0, g1, local),
        lerp_u8(b0, b1, local),
    )
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Render a URL as a terminal-friendly QR code using Unicode block characters.
/// Each row of the QR code uses upper/lower half-block characters to pack two
/// rows of modules into one terminal line, giving a compact square appearance.
fn format_qr_code(url: &str) -> Vec<String> {
    use qrcode::QrCode;

    let code = match QrCode::new(url.as_bytes()) {
        Ok(c) => c,
        Err(_) => return vec![format!("(QR code generation failed for {url})")],
    };

    let matrix = code.to_colors();
    let width = code.width();
    let height = matrix.len() / width;

    // Two module rows per terminal line via upper/lower half-blocks.
    let mut lines = Vec::new();

    let mut y = 0;
    while y < height {
        let mut line = String::new();
        for x in 0..width {
            let top = matrix[y * width + x];
            let bottom = if y + 1 < height {
                matrix[(y + 1) * width + x]
            } else {
                qrcode::Color::Light
            };
            match (top, bottom) {
                (qrcode::Color::Dark, qrcode::Color::Dark) => line.push('█'),
                (qrcode::Color::Dark, qrcode::Color::Light) => line.push('▀'),
                (qrcode::Color::Light, qrcode::Color::Dark) => line.push('▄'),
                (qrcode::Color::Light, qrcode::Color::Light) => line.push(' '),
            }
        }
        lines.push(line);
        y += 2;
    }

    lines
}

/// Convert a `.test` / `.tako.test` route to its `.local` LAN equivalent.
fn to_local_route(route: &str) -> String {
    let (host, path) = split_route_pattern(route);
    let (wildcard, host) = if let Some(rest) = host.strip_prefix("*.") {
        ("*.", rest)
    } else {
        ("", host)
    };
    let base = host
        .strip_suffix(".tako.test")
        .or_else(|| host.strip_suffix(".test"))
        .unwrap_or(host);
    match path {
        Some(path) => format!("{wildcard}{base}.local{path}"),
        None => format!("{wildcard}{base}.local"),
    }
}

/// Render a LAN mode block: routes + QR code as a single visual unit.
pub(super) fn format_lan_block(hosts: &[String], ca_url: &str) -> Vec<String> {
    let url_color = ansi_rgb(240, 175, 95);
    let warn_color = ansi_rgb(234, 211, 156);
    let mut out = Vec::new();
    out.push(String::new());

    // Wildcard routes cannot be advertised via mDNS (Bonjour/Avahi) — each
    // concrete subdomain needs its own record — so they are excluded from
    // the LAN route list (which would otherwise mislead the user into
    // trying an unreachable URL). Only concrete hostnames are listed.
    let concrete_hosts: Vec<&String> = hosts
        .iter()
        .filter(|h| !split_route_pattern(h).0.starts_with("*."))
        .collect();
    let wildcard_host = hosts
        .iter()
        .map(|h| split_route_pattern(h).0)
        .find(|h| h.starts_with("*."));

    if concrete_hosts.is_empty() {
        out.push(format!(
            "  {}",
            muted("No routes are reachable on your local network")
        ));
    } else {
        out.push(format!(
            "  {}",
            muted("Your app is now available on your local network at these routes")
        ));
        out.push(String::new());
        for host in &concrete_hosts {
            let local = to_local_route(host);
            out.push(format!("  {url_color}https://{local}{RESET}"));
        }
    }

    // If there are any wildcard routes, explain why they were excluded and
    // suggest a concrete example derived from one of them. `!` is flush-left
    // so the body text column lines up with the URL text column above.
    if let Some(wildcard_host) = wildcard_host {
        let example = wildcard_host.replacen('*', "tenant", 1);
        out.push(String::new());
        out.push(format!(
            "{warn_color}! Wildcard routes can't be advertised to devices via mDNS{RESET}"
        ));
        out.push(format!(
            "  {warn_color}Use non-wildcard routes (e.g. {example}) to reach it from your phone{RESET}"
        ));
    }

    out.push(String::new());
    for line in format_qr_code(ca_url) {
        out.push(format!("  {line}"));
    }
    out.push(format!(
        "  {}",
        muted("Scan to install the CA certificate on your device")
    ));
    out.push(format!(
        "  {}",
        muted(
            "If the page doesn't load, your Wi-Fi may use client isolation and LAN mode won't work"
        )
    ));
    out.push(String::new());
    out
}
