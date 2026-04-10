use console::{Term, measure_text_width, truncate_str};

use super::{LogLevel, ScopedLog};

pub(super) const RESET: &str = "\x1b[0m";
pub(super) const DIM: &str = "\x1b[2m";
#[allow(dead_code)]
const XDIM: &str = "\x1b[2;38;5;242m";
const BORDER: &str = "\x1b[2;38;2;79;107;122m";
const PRIMARY: &str = "\x1b[22;38;2;125;196;228m";

const STACKED_THRESHOLD: usize = 76;
const COL3_W: usize = 22;
const COL_SEP: usize = 2;
const BAR_W: usize = 8;
const ROUTES_LABEL_W: usize = 8;
const SCOPE_MIN: usize = 4;
const SCOPE_MAX: usize = 12;

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

pub(super) fn git_info(dir: &std::path::Path) -> (String, String, Option<String>) {
    let dir_str = dir.to_string_lossy();

    let root_out = std::process::Command::new("git")
        .args(["-C", dir_str.as_ref(), "rev-parse", "--show-toplevel"])
        .output();

    let git_root = match root_out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => return (String::new(), fmt_path(&dir_str), None),
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

    let slug_with_branch = if !slug.is_empty() && !branch.is_empty() {
        format!("{slug} ({branch})")
    } else if !branch.is_empty() {
        format!("({branch})")
    } else {
        slug
    };

    let worktree_name = detect_worktree(dir);

    (slug_with_branch, rel, worktree_name)
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

#[allow(clippy::too_many_arguments)]
pub(super) fn format_panel(
    app_name: &str,
    status: &str,
    adapter_name: &str,
    repo_slug: &str,
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
    adapter_name: &str,
    repo_slug: &str,
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

    let title_text = if adapter_name.trim().is_empty() {
        app_name.to_string()
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

    let (dot_color, dot_char) = status_dot(status);
    let l0 = format!("{dot_color}{dot_char}{RESET} {DIM}{status}{RESET}");
    let mut left = vec![l0];
    if let Some(wt) = worktree_name {
        let wt_label = format!("worktree ({wt})");
        let wt_t = truncate_str(&wt_label, col1_w, "…");
        left.push(format!("{DIM}{wt_t}{RESET}"));
    }
    if !repo_slug.is_empty() {
        let slug_t = truncate_str(repo_slug, col1_w, "…");
        left.push(format!("{DIM}{slug_t}{RESET}"));
    }
    if !repo_path.is_empty() {
        let path_t = truncate_str(repo_path, col1_w, "…");
        left.push(format!("{DIM}{path_t}{RESET}"));
    }

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

    let r0 = if let Some(c) = cpu {
        let bar = progress_bar(c / 100.0, BAR_W);
        format!("{DIM}cpu  {RESET}{bar} {:.0}%", c)
    } else {
        format!("{DIM}cpu  {RESET}—")
    };
    let r1 = if let Some(m) = mem_bytes {
        format!("{DIM}ram  {RESET}{}", fmt_bytes(m))
    } else {
        format!("{DIM}ram  {RESET}—")
    };
    let r2 = format!("{DIM}port {RESET}{app_port}");
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
    adapter_name: &str,
    repo_slug: &str,
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

    let title_text = if adapter_name.trim().is_empty() {
        app_name.to_string()
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

    let (dot_color, dot_char) = status_dot(status);
    rows.push(stacked_row(
        &format!("{dot_color}{dot_char}{RESET} {DIM}{status}{RESET}"),
        inner_w,
    ));

    let avail = inner_w.saturating_sub(2);
    if let Some(wt) = worktree_name {
        let wt_label = format!("worktree ({wt})");
        let wt_t = truncate_str(&wt_label, avail, "…");
        rows.push(stacked_row(&format!("{DIM}{wt_t}{RESET}"), inner_w));
    }

    if !repo_slug.is_empty() {
        let slug_t = truncate_str(repo_slug, avail, "…");
        rows.push(stacked_row(&format!("{DIM}{slug_t}{RESET}"), inner_w));
    }
    if !repo_path.is_empty() {
        let path_t = truncate_str(repo_path, avail, "…");
        rows.push(stacked_row(&format!("{DIM}{path_t}{RESET}"), inner_w));
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
            format!("{DIM}routes{RESET}  {url_color}{url_t}{RESET}")
        } else {
            format!("{}{url_color}{url_t}{RESET}", " ".repeat(ROUTES_LABEL_W))
        };
        rows.push(stacked_row(&line, inner_w));
    }

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
    rows.push(stacked_row(&format!("{cpu_str}  {ram_str}"), inner_w));
    rows.push(stacked_row(
        &format!("{DIM}port{RESET} {app_port}"),
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
            "l {DIM}lan{RESET}   r {DIM}restart{RESET}   b {DIM}background{RESET}   ^c/q {DIM}stop{RESET}"
        )
    } else {
        format!(
            "l {DIM}lan{RESET}   r {DIM}restart{RESET}   b {DIM}background{RESET}   ctrl+c/q {DIM}stop{RESET}"
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
    if log.scope == super::DIVIDER_SCOPE {
        let label = if log.message.is_empty() {
            "restarted"
        } else {
            &log.message
        };
        return format!("{DIM}──── {label} ────{RESET}");
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
        let scope_color = scope_color(&log.scope);
        return format!(
            "{DIM}{}{RESET} {color}{:>5}{RESET} {scope_color}{scope}{RESET} LAN mode enabled {DIM}({ip}){RESET}",
            log.timestamp, log.level
        );
    }
    if matches!(log.level, LogLevel::Debug) {
        let scope = fit_scope(&log.scope);
        let scope_color = scope_color(&log.scope);
        return format!(
            "{DIM}{} {:>5}{RESET} {scope_color}{scope}{RESET} {DIM}{}{RESET}",
            log.timestamp, log.level, log.message
        );
    }
    let color = level_color(&log.level);
    let scope = fit_scope(&log.scope);
    let scope_color = scope_color(&log.scope);
    format!(
        "{DIM}{}{RESET} {color}{:>5}{RESET} {scope_color}{scope}{RESET} {}",
        log.timestamp, log.level, log.message
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

const SCOPE_TAKO: &str = "\x1b[38;2;232;135;131m";
const SCOPE_APP: &str = "\x1b[38;2;200;200;190m";

fn scope_color(scope: &str) -> String {
    match scope {
        "tako" => SCOPE_TAKO.to_string(),
        "app" => SCOPE_APP.to_string(),
        _ => {
            let hash = scope
                .bytes()
                .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
            let (r, g, b) = SCOPE_PALETTE[hash as usize % SCOPE_PALETTE.len()];
            format!("\x1b[38;2;{r};{g};{b}m")
        }
    }
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
    let mut out = Vec::new();
    out.push(String::new());
    out.push(format!(
        "  {DIM}Your app is now available on your local network at these routes{RESET}"
    ));
    out.push(String::new());
    for host in hosts {
        let local = to_local_route(host);
        out.push(format!("  {url_color}https://{local}{RESET}"));
    }
    out.push(String::new());
    for line in format_qr_code(ca_url) {
        out.push(format!("  {line}"));
    }
    out.push(format!(
        "  {DIM}Scan to install the CA certificate on your device{RESET}"
    ));
    out.push(String::new());
    out
}
