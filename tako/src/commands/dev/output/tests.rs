use super::super::LogLevel;
use super::*;
use crate::commands::dev::output_render::{
    extract_repo_slug, fit_scope, format_lan_block, format_panel_stacked, format_panel_wide,
    progress_bar, vlen,
};
use crate::output::LOGO_ROWS;
use console::{measure_text_width, strip_ansi_codes, truncate_str};

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
fn fit_scope_pads_short_scopes() {
    assert_eq!(fit_scope("app"), "app ");
    assert_eq!(fit_scope("up"), "up  ");
}

#[test]
fn fit_scope_keeps_exact_min() {
    assert_eq!(fit_scope("tako"), "tako");
}

#[test]
fn fit_scope_keeps_mid_length() {
    assert_eq!(fit_scope("myservice"), "myservice");
}

#[test]
fn fit_scope_truncates_long_scopes() {
    assert_eq!(fit_scope("longservicenm"), "longservice\u{2026}");
}

#[test]
fn format_header_has_logo_and_version() {
    let h = format_header();
    assert!(h.contains('█'));
    let first_line = h.lines().next().unwrap();
    assert!(first_line.contains('v'));
}

#[test]
fn format_header_has_all_logo_rows() {
    let h = format_header();
    assert_eq!(h.lines().count(), LOGO_ROWS.len());
    for (line, row) in h.lines().zip(LOGO_ROWS.iter()) {
        assert!(line.contains(row));
    }
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
        &["myapp.tako.test".to_string()],
        443,
        3000,
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
        &["app.tako.test".to_string()],
        443,
        3000,
        None,
        None,
    );
    let plain = strip_ansi(&panel);
    assert!(plain.contains("routes"));
    assert!(plain.contains("https://app.tako.test"));
}

#[test]
fn format_panel_shows_all_urls() {
    let hosts = vec!["a.tako.test".to_string(), "b.tako.test".to_string()];
    let panel = format_panel(
        "app", "running", "bun", "u/r", "", None, &hosts, 443, 3000, None, None,
    );
    let plain = strip_ansi(&panel);
    assert!(plain.contains("https://a.tako.test"));
    assert!(plain.contains("https://b.tako.test"));
}

#[test]
fn format_panel_shows_wildcard_and_path_routes() {
    let hosts = vec![
        "bun-example.tako.test".to_string(),
        "bun-example.tako.test/bun".to_string(),
        "*.bun-example.tako.test".to_string(),
    ];
    let panel = format_panel_wide(
        "bun-example",
        "running",
        "bun",
        "u/r",
        "",
        None,
        &hosts,
        443,
        3000,
        None,
        None,
        120,
    );
    let plain = strip_ansi(&panel);
    assert!(
        plain.contains("https://bun-example.tako.test/bun"),
        "missing /bun route"
    );
    assert!(
        plain.contains("https://*.bun-example.tako.test"),
        "missing wildcard route"
    );
    assert_eq!(
        plain.matches("https://").count(),
        3,
        "expected exactly 3 route URLs"
    );
}

#[test]
fn format_lan_block_rewrites_host_only_and_preserves_paths() {
    let lines = format_lan_block(
        &[
            "bun-example.tako.test".to_string(),
            "bun-example.tako.test/bun".to_string(),
            "*.bun-example.tako.test/api/*".to_string(),
        ],
        "http://192.168.1.2/ca.pem",
    );
    let plain = strip_ansi(&lines.join("\n"));

    assert!(!plain.contains("LAN mode enabled"));
    assert!(plain.contains("Your app is now available on your local network at these routes"));
    assert!(plain.contains("https://bun-example.local"));
    assert!(plain.contains("https://bun-example.local/bun"));
    assert!(plain.contains("https://*.bun-example.local/api/*"));
}

#[test]
fn format_log_dims_lan_mode_ip_suffix() {
    let enabled = strip_ansi(&format_log(&ScopedLog {
        timestamp: "12:34:56".to_string(),
        level: LogLevel::Info,
        scope: "tako".to_string(),
        message: "LAN mode enabled (192.168.1.2)".to_string(),
    }));
    assert!(enabled.contains("INFO"));
    assert!(enabled.contains("tako"));
    assert!(enabled.contains("LAN mode enabled (192.168.1.2)"));

    let disabled = strip_ansi(&format_log(&ScopedLog {
        timestamp: "12:34:56".to_string(),
        level: LogLevel::Info,
        scope: "tako".to_string(),
        message: "LAN mode disabled".to_string(),
    }));
    assert!(disabled.contains("INFO"));
    assert!(disabled.contains("tako"));
    assert!(disabled.contains("LAN mode disabled"));
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
        &["app.tako.test".to_string()],
        443,
        3000,
        None,
        None,
    );
    assert!(!strip_ansi(&panel).contains(":443"));
}

#[test]
fn format_panel_includes_custom_port() {
    let panel = format_panel_wide(
        "app",
        "running",
        "",
        "",
        "",
        None,
        &["app.tako.test".to_string()],
        47831,
        3000,
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
        &["app.tako.test".to_string()],
        443,
        3001,
        Some(50.0),
        Some(100 * 1024 * 1024),
    );
    let plain = strip_ansi(&panel);
    assert!(plain.contains("50%") || plain.contains("50"));
    assert!(plain.contains("100 MB"));
    assert!(plain.contains("port"));
    assert!(plain.contains("3001"));
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
        &["app.tako.test".to_string()],
        443,
        3000,
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
        &["app.tako.test".to_string()],
        443,
        3000,
        None,
        None,
    );
    let plain = strip_ansi(&panel);
    assert!(plain.contains("myorg/myrepo"));
    assert!(plain.contains("apps/myapp"));
}

#[test]
fn format_panel_stacked_has_border_and_content() {
    let panel = format_panel_stacked(
        "app",
        "running",
        "bun",
        "user/repo",
        "projects/app",
        None,
        &["app.tako.test".to_string()],
        443,
        3000,
        Some(25.0),
        Some(50 * 1024 * 1024),
        60,
    );
    let plain = strip_ansi(&panel);
    assert!(plain.contains('┌'));
    assert!(plain.contains('└'));
    assert!(plain.contains("app"));
    assert!(plain.contains("routes"));
    assert!(plain.contains("https://app.tako.test"));
    assert!(plain.contains("cpu"));
    assert!(plain.contains("ram"));
    assert!(plain.contains("port 3000"));
    assert!(plain.contains("3000"));
}

#[test]
fn format_keymap_has_restart_stop_background() {
    let km = strip_ansi(&format_keymap());
    assert!(km.contains('r'));
    assert!(km.contains("restart"));
    assert!(km.contains("stop"));
    assert!(km.contains('b'));
    assert!(km.contains("background"));
    assert!(!km.contains("quit"));
}

#[test]
fn progress_bar_extremes() {
    let full = strip_ansi(&progress_bar(1.0, 8));
    let empty = strip_ansi(&progress_bar(0.0, 8));
    assert!(full.contains("████████"));
    assert!(empty.contains("⣿⣿⣿⣿⣿⣿⣿⣿"));
}

#[test]
fn vlen_strips_ansi() {
    assert_eq!(vlen(&format!("{DIM}hello{RESET}")), 5);
    assert_eq!(vlen("AB"), 2);
}

#[test]
fn trunc_at_limit() {
    assert_eq!(truncate_str("hello", 10, "…").as_ref(), "hello");
    assert_eq!(measure_text_width(&truncate_str("hello world", 7, "…")), 7);
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
        &["app.tako.test".to_string()],
        443,
        3000,
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
        &["app.tako.test".to_string()],
        443,
        3000,
        None,
        None,
    );
    let plain = strip_ansi(&panel);
    assert!(!plain.contains("worktree"));
}

fn strip_ansi(s: &str) -> String {
    strip_ansi_codes(s).into_owned()
}
