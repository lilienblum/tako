use std::env::current_dir;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::app::require_app_name_from_config;
use crate::commands::server;
use crate::config::{ServersToml, TakoToml};
use crate::output;
use crate::ssh::{SshClient, SshConfig};

const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub fn run(env: &str, tail: bool, days: u32) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(env, tail, days))
}

async fn run_async(
    env: &str,
    tail: bool,
    days: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;

    let tako_config = TakoToml::load_from_dir(&project_dir)?;
    let mut servers = ServersToml::load()?;

    let app_name = require_app_name_from_config(&project_dir)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

    if !tako_config.envs.contains_key(env) {
        let available: Vec<_> = tako_config.envs.keys().collect();
        return Err(format!(
            "Environment '{}' not found. Available: {}",
            env,
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )
        .into());
    }

    let server_names = resolve_log_server_names(&tako_config, &mut servers, env).await?;

    let colorize = output::is_interactive();
    let show_prefix = server_names.len() > 1;

    if tail {
        stream_logs(&server_names, &servers, &app_name, show_prefix, colorize).await
    } else {
        fetch_logs(
            &server_names,
            &servers,
            &app_name,
            days,
            show_prefix,
            colorize,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Tail mode: stream to stdout with live dedup
// ---------------------------------------------------------------------------

async fn stream_logs(
    server_names: &[String],
    servers: &ServersToml,
    app_name: &str,
    show_prefix: bool,
    colorize: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    output::step(&format!(
        "Streaming logs for {} {}",
        output::highlight(app_name),
        output::brand_muted("(Ctrl+c to stop)")
    ));

    let writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(Box::new(std::io::stdout())));

    let mut tasks = Vec::new();
    for server_name in server_names {
        let server = servers
            .get(server_name)
            .ok_or_else(|| server_not_found_error(server_name))?;

        let host = server.host.clone();
        let port = server.port;
        let app_name = app_name.to_string();
        let writer = writer.clone();
        let prefix = format_prefix(server_name, show_prefix, colorize);

        tasks.push(tokio::spawn(async move {
            let ssh_config = SshConfig::from_server(&host, port);
            let mut ssh = SshClient::new(ssh_config);
            ssh.connect().await?;

            let log_cmd = format!(
                "sudo journalctl -u tako-server -f --no-pager -o cat 2>/dev/null \
                 | grep --line-buffered '\"app\":\"{app}\"' \
                 || tail -f /opt/tako/apps/{app}/shared/logs/*.log 2>/dev/null \
                 || echo 'No logs available'",
                app = app_name
            );

            let lw = Arc::new(Mutex::new(LogWriter::new(writer, prefix, colorize)));
            let lw_out = lw.clone();
            let lw_err = lw.clone();

            let _ = ssh
                .exec_streaming(
                    &log_cmd,
                    move |data| {
                        if let Ok(mut w) = lw_out.lock() {
                            w.push(data);
                        }
                    },
                    move |data| {
                        if let Ok(mut w) = lw_err.lock() {
                            w.push(data);
                        }
                    },
                )
                .await?;

            if let Ok(mut w) = lw.lock() {
                w.flush();
            }
            ssh.disconnect().await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        }));
    }

    for t in tasks {
        let _ = t.await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Non-tail mode: fetch → sort → dedup → pager
// ---------------------------------------------------------------------------

async fn fetch_logs(
    server_names: &[String],
    servers: &ServersToml,
    app_name: &str,
    days: u32,
    show_prefix: bool,
    colorize: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let phase = output::PhaseSpinner::start("Fetching logs…");

    let collected: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let mut tasks = Vec::new();
    for server_name in server_names {
        let server = servers
            .get(server_name)
            .ok_or_else(|| server_not_found_error(server_name))?;

        let host = server.host.clone();
        let port = server.port;
        let app_name = app_name.to_string();
        let server_name = server_name.to_string();
        let collected = collected.clone();

        tasks.push(tokio::spawn(async move {
            let ssh_config = SshConfig::from_server(&host, port);
            let mut ssh = SshClient::new(ssh_config);
            ssh.connect().await?;

            let log_cmd = format!(
                "sudo journalctl -u tako-server --since '{days} days ago' --no-pager -o cat 2>/dev/null \
                 | grep '\"app\":\"{app}\"' \
                 || cat /opt/tako/apps/{app}/shared/logs/*.log 2>/dev/null",
                app = app_name,
                days = days
            );

            let collector = Arc::new(Mutex::new(LineCollector::new(
                server_name,
                collected,
            )));
            let c_out = collector.clone();
            let c_err = collector.clone();

            let _ = ssh
                .exec_streaming(
                    &log_cmd,
                    move |data| {
                        if let Ok(mut c) = c_out.lock() {
                            c.push(data);
                        }
                    },
                    move |data| {
                        if let Ok(mut c) = c_err.lock() {
                            c.push(data);
                        }
                    },
                )
                .await?;

            if let Ok(mut c) = collector.lock() {
                c.flush();
            }
            ssh.disconnect().await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        }));
    }

    for t in tasks {
        let _ = t.await;
    }

    // Sort by timestamp across all servers.
    let mut lines = match Arc::try_unwrap(collected) {
        Ok(m) => m.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    };
    lines.sort_by(|a, b| extract_timestamp(&a.1).cmp(extract_timestamp(&b.1)));

    if lines.is_empty() {
        phase.finish("No logs found");
        output::warning(&format!(
            "No logs in the last {} days. Try --tail to stream live logs.",
            days
        ));
        return Ok(());
    }

    phase.finish(&format!("{} lines fetched", lines.len()));

    // Format and dedup.
    let formatted = format_and_dedup(&lines, show_prefix, colorize);

    // Show in pager or print directly.
    if output::is_interactive() {
        if let Some(mut child) = spawn_pager() {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(formatted.as_bytes());
            }
            drop(child.stdin.take());
            let _ = child.wait();
        } else {
            print!("{formatted}");
        }
    } else {
        print!("{formatted}");
    }

    Ok(())
}

fn format_and_dedup(
    lines: &[(String, String)],
    show_prefix: bool,
    colorize: bool,
) -> String {
    let mut out = String::new();
    let mut last_key = String::new();
    let mut repeat_count: u32 = 0;

    for (server, raw) in lines {
        let (key, formatted) = format_log_entry(raw, colorize);
        if !key.is_empty() && key == last_key {
            repeat_count += 1;
        } else {
            push_repeat(&mut out, repeat_count, colorize);
            let prefix = format_prefix(server, show_prefix, colorize);
            out.push_str(&prefix);
            out.push_str(&formatted);
            out.push('\n');
            last_key = key;
            repeat_count = 0;
        }
    }
    push_repeat(&mut out, repeat_count, colorize);
    out
}

fn push_repeat(out: &mut String, count: u32, colorize: bool) {
    if count > 0 {
        if colorize {
            out.push_str(&format!(
                "         {DIM}… and {count} more{RESET}\n"
            ));
        } else {
            out.push_str(&format!("         … and {count} more\n"));
        }
    }
}

// ---------------------------------------------------------------------------
// Line collection (non-tail)
// ---------------------------------------------------------------------------

struct LineCollector {
    buf: String,
    server: String,
    lines: Arc<Mutex<Vec<(String, String)>>>,
}

impl LineCollector {
    fn new(server: String, lines: Arc<Mutex<Vec<(String, String)>>>) -> Self {
        Self {
            buf: String::new(),
            server,
            lines,
        }
    }

    fn push(&mut self, data: &[u8]) {
        self.buf.push_str(&String::from_utf8_lossy(data));
        while let Some(nl) = self.buf.find('\n') {
            let line = self.buf[..nl].to_string();
            self.buf = self.buf[nl + 1..].to_string();
            if !line.is_empty() {
                self.lines
                    .lock()
                    .unwrap()
                    .push((self.server.clone(), line));
            }
        }
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.lines
                .lock()
                .unwrap()
                .push((self.server.clone(), line));
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming writer (tail mode)
// ---------------------------------------------------------------------------

struct LogWriter {
    buf: String,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    prefix: String,
    colorize: bool,
    last_msg_key: String,
    repeat_count: u32,
}

impl LogWriter {
    fn new(
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
        prefix: String,
        colorize: bool,
    ) -> Self {
        Self {
            buf: String::new(),
            writer,
            prefix,
            colorize,
            last_msg_key: String::new(),
            repeat_count: 0,
        }
    }

    fn push(&mut self, data: &[u8]) {
        self.buf.push_str(&String::from_utf8_lossy(data));
        while let Some(nl) = self.buf.find('\n') {
            let line = self.buf[..nl].to_string();
            self.buf = self.buf[nl + 1..].to_string();
            self.process_line(&line);
        }
    }

    fn process_line(&mut self, line: &str) {
        let (key, formatted) = format_log_entry(line, self.colorize);
        if !key.is_empty() && key == self.last_msg_key {
            self.repeat_count += 1;
        } else {
            self.flush_repeat();
            self.write_line(&formatted);
            self.last_msg_key = key;
            self.repeat_count = 0;
        }
    }

    fn flush_repeat(&mut self) {
        if self.repeat_count > 0 {
            let msg = if self.colorize {
                format!(
                    "         {DIM}… and {} more{RESET}",
                    self.repeat_count
                )
            } else {
                format!("         … and {} more", self.repeat_count)
            };
            self.write_line(&msg);
        }
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.process_line(&line);
        }
        self.flush_repeat();
    }

    fn write_line(&self, formatted: &str) {
        let Ok(mut w) = self.writer.lock() else {
            return;
        };
        let _ = writeln!(w, "{}{formatted}", self.prefix);
    }
}

// ---------------------------------------------------------------------------
// Shared formatting
// ---------------------------------------------------------------------------

fn format_prefix(server: &str, show: bool, colorize: bool) -> String {
    if !show {
        return String::new();
    }
    if colorize {
        format!("{DIM}[{server}]{RESET} ")
    } else {
        format!("[{server}] ")
    }
}

fn format_log_entry(line: &str, colorize: bool) -> (String, String) {
    if let Some((hms, level, message)) = parse_json_log(line) {
        let key = format!("{level} {message}");
        let formatted = if colorize {
            let color = level_color(&level);
            format!("{DIM}{hms}{RESET} {color}{level:<5}{RESET} {message}")
        } else {
            format!("{hms} {level:<5} {message}")
        };
        (key, formatted)
    } else {
        // Non-JSON line (e.g., from app log files): show as-is.
        (String::new(), line.to_string())
    }
}

/// Parse a JSON log line from tracing-subscriber `.json()` format.
///
/// Expected: `{"timestamp":"...","level":"INFO","fields":{"message":"...","app":"..."}}`
fn parse_json_log(line: &str) -> Option<(String, String, String)> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let timestamp = v["timestamp"].as_str()?;
    let level = v["level"].as_str()?;
    let fields = v.get("fields")?.as_object()?;
    let message = fields
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("");

    // Collect structured fields (skip "message") into "key=value" pairs.
    let mut parts = vec![message.to_string()];
    for (k, val) in fields {
        if k == "message" {
            continue;
        }
        let v_str = val
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| val.to_string());
        parts.push(format!("{k}={v_str}"));
    }

    let hms = if timestamp.len() >= 19 {
        timestamp[11..19].to_string()
    } else {
        timestamp.to_string()
    };

    Some((hms, level.to_string(), parts.join(" ")))
}

fn extract_timestamp(line: &str) -> &str {
    // JSON: look for "timestamp":"..." field.
    if let Some(pos) = line.find("\"timestamp\":\"") {
        let start = pos + 13;
        if let Some(end) = line[start..].find('"') {
            return &line[start..start + end];
        }
    }
    "\x7f" // sort unparseable lines last
}

fn level_color(level: &str) -> &'static str {
    match level {
        "DEBUG" | "TRACE" => "\x1b[38;2;140;207;255m",
        "INFO" => "\x1b[38;2;155;217;179m",
        "WARN" => "\x1b[38;2;234;211;156m",
        "ERROR" => "\x1b[38;2;232;163;160m",
        _ => "",
    }
}

fn spawn_pager() -> Option<std::process::Child> {
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());
    let parts: Vec<&str> = pager.split_whitespace().collect();
    let (cmd, args) = parts.split_first()?;
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .ok()
}

fn server_not_found_error(name: &str) -> Box<dyn std::error::Error> {
    format!(
        "Server '{}' not found in ~/.tako/config.toml [[servers]]. Run 'tako servers add --name {} <host>'.",
        name, name
    )
    .into()
}

// ---------------------------------------------------------------------------
// Server resolution
// ---------------------------------------------------------------------------

async fn resolve_log_server_names(
    tako_config: &TakoToml,
    servers: &mut ServersToml,
    env: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mapped: Vec<String> = tako_config
        .get_servers_for_env(env)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    if !mapped.is_empty() {
        return Ok(mapped);
    }

    if env == "production" && servers.len() == 1 {
        let only = servers.names().into_iter().next().unwrap_or("<server>");
        let confirmed = output::confirm(
            &format!(
                "No [servers.*] mapping for 'production'. Stream logs from the only configured server ('{}')?",
                only
            ),
            true,
        )?;
        if confirmed {
            return Ok(vec![only.to_string()]);
        }
        return Err(
            "Logs cancelled. Add [servers.<name>] with env = \"production\" to tako.toml.".into(),
        );
    }

    if env == "production" && servers.is_empty() {
        if server::prompt_to_add_server(
            "No servers have been added. Logs need at least one server.",
        )
        .await?
        .is_some()
        {
            *servers = ServersToml::load()?;
            if servers.len() == 1 {
                let only = servers.names().into_iter().next().unwrap_or("<server>");
                return Ok(vec![only.to_string()]);
            }
        }
        return Err(
            "No servers have been added. Run 'tako servers add <host>' first, then map it in tako.toml with [servers.<name>] env = \"production\"."
                .into(),
        );
    }

    Err(format!(
        "No servers configured for environment '{}'. Add [servers.<name>] with env = \"{}\" to tako.toml.",
        env, env
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerEntry;

    #[test]
    fn parse_json_log_info() {
        let line = r#"{"timestamp":"2026-03-10T12:34:56.789012Z","level":"INFO","fields":{"message":"Instance is healthy","app":"bun-example","instance":"abc123"}}"#;
        let (hms, level, msg) = parse_json_log(line).unwrap();
        assert_eq!(hms, "12:34:56");
        assert_eq!(level, "INFO");
        assert!(msg.contains("Instance is healthy"));
        assert!(msg.contains("app=bun-example"));
        assert!(msg.contains("instance=abc123"));
    }

    #[test]
    fn parse_json_log_warn() {
        let line = r#"{"timestamp":"2026-03-10T08:00:00.000Z","level":"WARN","fields":{"message":"timeout","app":"foo"}}"#;
        let (hms, level, msg) = parse_json_log(line).unwrap();
        assert_eq!(hms, "08:00:00");
        assert_eq!(level, "WARN");
        assert!(msg.starts_with("timeout"));
        assert!(msg.contains("app=foo"));
    }

    #[test]
    fn parse_json_log_non_json() {
        assert!(parse_json_log("just some random text").is_none());
        assert!(parse_json_log("").is_none());
    }

    #[test]
    fn dedup_consecutive_lines() {
        let lines = vec![
            (
                "s1".to_string(),
                r#"{"timestamp":"2026-03-10T12:00:00.000Z","level":"INFO","fields":{"message":"hello","app":"x"}}"#.to_string(),
            ),
            (
                "s1".to_string(),
                r#"{"timestamp":"2026-03-10T12:00:01.000Z","level":"INFO","fields":{"message":"hello","app":"x"}}"#.to_string(),
            ),
            (
                "s1".to_string(),
                r#"{"timestamp":"2026-03-10T12:00:02.000Z","level":"INFO","fields":{"message":"hello","app":"x"}}"#.to_string(),
            ),
            (
                "s1".to_string(),
                r#"{"timestamp":"2026-03-10T12:00:03.000Z","level":"WARN","fields":{"message":"different","app":"x"}}"#.to_string(),
            ),
        ];
        let output = format_and_dedup(&lines, false, false);
        let result: Vec<&str> = output.trim().lines().collect();
        assert_eq!(result.len(), 3);
        assert!(result[0].contains("hello"));
        assert!(result[1].contains("… and 2 more"));
        assert!(result[2].contains("different"));
    }

    #[test]
    fn extract_timestamp_from_json() {
        let line = r#"{"timestamp":"2026-03-10T12:34:56.789Z","level":"INFO","fields":{"message":"hi"}}"#;
        assert_eq!(
            extract_timestamp(line),
            "2026-03-10T12:34:56.789Z"
        );
    }

    #[test]
    fn extract_timestamp_non_json() {
        assert_eq!(extract_timestamp("random text"), "\x7f");
    }

    #[test]
    fn sort_by_timestamp() {
        let a = r#"{"timestamp":"2026-03-10T12:00:02.000Z","level":"INFO","fields":{"message":"second"}}"#;
        let b = r#"{"timestamp":"2026-03-10T12:00:01.000Z","level":"INFO","fields":{"message":"first"}}"#;
        assert!(extract_timestamp(b) < extract_timestamp(a));
    }

    #[tokio::test]
    async fn resolve_log_server_names_uses_single_production_server_fallback() {
        let tako_config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let names = resolve_log_server_names(&tako_config, &mut servers, "production")
            .await
            .expect("should resolve with fallback");
        assert_eq!(names, vec!["solo".to_string()]);
    }

    #[tokio::test]
    async fn resolve_log_server_names_errors_for_non_production_without_mapping() {
        let tako_config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let err = resolve_log_server_names(&tako_config, &mut servers, "staging")
            .await
            .expect_err("should fail for non-production");
        assert!(
            err.to_string()
                .contains("No servers configured for environment 'staging'")
        );
    }
}
