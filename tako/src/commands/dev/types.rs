use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Fatal => "FATAL",
        };
        f.pad(s)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopedLog {
    pub timestamp: String,
    pub level: LogLevel,
    pub scope: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedLogSerde {
    timestamp: String,
    level: LogLevel,
    scope: String,
    message: String,
}

fn hms_timestamp(h: u8, m: u8, s: u8) -> String {
    format!("{:02}:{:02}:{:02}", h, m, s)
}

impl<'de> Deserialize<'de> for ScopedLog {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = ScopedLogSerde::deserialize(deserializer)?;
        let timestamp = if raw.timestamp.trim().is_empty() {
            "00:00:00".to_string()
        } else {
            raw.timestamp
        };

        Ok(Self {
            timestamp,
            level: raw.level,
            scope: raw.scope,
            message: raw.message,
        })
    }
}

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

fn local_offset() -> UtcOffset {
    *LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

impl ScopedLog {
    pub fn at(level: LogLevel, scope: impl Into<String>, message: impl Into<String>) -> Self {
        let now = OffsetDateTime::now_utc().to_offset(local_offset());
        Self {
            timestamp: hms_timestamp(now.hour() as u8, now.minute() as u8, now.second() as u8),
            level,
            scope: scope.into(),
            message: message.into(),
        }
    }

    pub fn info(scope: impl Into<String>, message: impl Into<String>) -> Self {
        Self::at(LogLevel::Info, scope, message)
    }

    pub fn warn(scope: impl Into<String>, message: impl Into<String>) -> Self {
        Self::at(LogLevel::Warn, scope, message)
    }

    pub fn error(scope: impl Into<String>, message: impl Into<String>) -> Self {
        Self::at(LogLevel::Error, scope, message)
    }

    #[allow(dead_code)]
    pub fn divider(label: &str) -> Self {
        Self {
            timestamp: String::new(),
            level: LogLevel::Info,
            scope: DIVIDER_SCOPE.to_string(),
            message: label.to_string(),
        }
    }
}

#[cfg(test)]
const APP_SCOPE: &str = "app";
pub const DIVIDER_SCOPE: &str = "__divider__";

#[cfg(test)]
pub(super) fn app_log_scope() -> String {
    APP_SCOPE.to_string()
}

/// Events from the dev server
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum DevEvent {
    AppLaunching,
    AppStarted,
    AppStopped,
    AppProcessExited(String),
    AppPid(u32),
    AppError(String),
    ClientConnected {
        is_self: bool,
        client_id: u32,
    },
    ClientDisconnected {
        client_id: u32,
    },
    LanModeChanged {
        enabled: bool,
        lan_ip: Option<String>,
        ca_url: Option<String>,
    },
    ExitWithMessage(String),
}

#[cfg(test)]
fn strip_ascii_case_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() < prefix.len() {
        return None;
    }
    let (head, tail) = s.split_at(prefix.len());
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

#[cfg(test)]
fn prefixed_child_log_level_and_message(line: &str) -> Option<(LogLevel, String)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    let candidates = [
        ("[TRACE]", LogLevel::Debug),
        ("[DEBUG]", LogLevel::Debug),
        ("[INFO]", LogLevel::Info),
        ("[WARN]", LogLevel::Warn),
        ("[WARNING]", LogLevel::Warn),
        ("[ERROR]", LogLevel::Error),
        ("[FATAL]", LogLevel::Fatal),
        ("TRACE", LogLevel::Debug),
        ("DEBUG", LogLevel::Debug),
        ("INFO", LogLevel::Info),
        ("WARN", LogLevel::Warn),
        ("WARNING", LogLevel::Warn),
        ("ERROR", LogLevel::Error),
        ("FATAL", LogLevel::Fatal),
    ];

    for (prefix, level) in candidates {
        let Some(rest) = strip_ascii_case_prefix(trimmed, prefix) else {
            continue;
        };

        if !prefix.starts_with('[')
            && rest
                .chars()
                .next()
                .is_some_and(|ch| !ch.is_whitespace() && ch != ':' && ch != '-' && ch != '|')
        {
            continue;
        }

        let message = rest.trim_start_matches(|ch: char| {
            ch.is_whitespace() || ch == ':' || ch == '-' || ch == '|'
        });
        let message = if message.is_empty() { trimmed } else { message };
        return Some((level.clone(), message.to_string()));
    }

    None
}

#[cfg(test)]
pub(super) fn child_log_level_and_message(
    default_level: LogLevel,
    line: &str,
) -> (LogLevel, String) {
    prefixed_child_log_level_and_message(line).unwrap_or((default_level, line.to_string()))
}

#[cfg(test)]
pub(super) fn should_drop_child_log_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some(rest) = trimmed.strip_prefix("$ ") else {
        return false;
    };
    rest.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '/' || ch == '@')
}

#[cfg(test)]
pub(super) fn trim_child_log_message(message: &str) -> Option<String> {
    let trimmed_end = message.trim_end();
    if trimmed_end.trim().is_empty() {
        None
    } else {
        Some(trimmed_end.to_string())
    }
}
