//! Tako server protocol types for management socket communication
//!
//! These types are shared between the CLI and tako-server for
//! communication via the Unix management socket.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const PROTOCOL_VERSION: u32 = 3;

/// Commands that can be sent to the server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Query protocol version and supported capabilities.
    Hello { protocol_version: u32 },

    /// Deploy a new version of an app
    Deploy {
        app: String,
        version: String,
        path: String,
        /// Route patterns for this app (host, wildcard, optional path).
        routes: Vec<String>,

        /// Minimum number of instances to keep running (0 = on-demand).
        instances: u8,

        /// Idle timeout in seconds (instances are stopped after this long with no requests).
        idle_timeout: u32,
    },

    /// Stop an app
    Stop { app: String },

    /// Delete an app from runtime state
    Delete { app: String },

    /// Get status of an app
    Status { app: String },

    /// List all apps
    List,

    /// List all configured routes (all apps)
    Routes,

    /// Reload configuration
    Reload { app: String },

    /// Update secrets for an app
    UpdateSecrets {
        app: String,
        secrets: HashMap<String, String>,
    },

    /// Get server runtime information (ports, data dir, upgrade mode).
    ServerInfo,

    /// Enter upgrading mode with a durable lock owner.
    EnterUpgrading { owner: String },

    /// Exit upgrading mode for the lock owner.
    ExitUpgrading { owner: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResponse {
    pub protocol_version: u32,
    pub server_version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeMode {
    Normal,
    Upgrading,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerRuntimeInfo {
    pub mode: UpgradeMode,
    pub socket: String,
    pub data_dir: String,
    pub http_port: u16,
    pub https_port: u16,
    pub no_acme: bool,
    pub acme_staging: bool,
    pub acme_email: Option<String>,
    pub renewal_interval_hours: u64,
    pub instance_port_offset: u16,
}

/// Response from the server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    /// Command succeeded
    Ok { data: serde_json::Value },

    /// Command failed
    Error { message: String },
}

impl Response {
    pub fn ok(data: impl Serialize) -> Self {
        Self::Ok {
            data: serde_json::to_value(data).unwrap_or(serde_json::Value::Null),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// Check if response is Ok
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// Get data from Ok response
    pub fn data(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Ok { data } => Some(data),
            Self::Error { .. } => None,
        }
    }

    /// Get error message from Error response
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Ok { .. } => None,
            Self::Error { message } => Some(message),
        }
    }
}

/// App status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStatus {
    pub name: String,
    pub version: String,
    pub instances: Vec<InstanceStatus>,
    #[serde(default)]
    pub builds: Vec<BuildStatus>,
    pub state: AppState,

    pub last_error: Option<String>,
}

/// Runtime status for a specific build/version of an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildStatus {
    pub version: String,
    pub state: AppState,
    pub instances: Vec<InstanceStatus>,
}

/// Instance status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub id: u32,
    pub state: InstanceState,
    pub port: u16,
    pub pid: Option<u32>,
    pub uptime_secs: u64,
    pub requests_total: u64,
}

/// App state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppState {
    Running,
    Idle,
    Deploying,
    Stopped,
    Error,
}

impl std::fmt::Display for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppState::Running => write!(f, "running"),
            AppState::Idle => write!(f, "idle"),
            AppState::Deploying => write!(f, "deploying"),
            AppState::Stopped => write!(f, "stopped"),
            AppState::Error => write!(f, "error"),
        }
    }
}

/// Instance state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    Starting,
    Ready,
    Healthy,
    Unhealthy,
    Draining,
    Stopped,
}

impl std::fmt::Display for InstanceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstanceState::Starting => write!(f, "starting"),
            InstanceState::Ready => write!(f, "ready"),
            InstanceState::Healthy => write!(f, "healthy"),
            InstanceState::Unhealthy => write!(f, "unhealthy"),
            InstanceState::Draining => write!(f, "draining"),
            InstanceState::Stopped => write!(f, "stopped"),
        }
    }
}

/// Server list response - list of app statuses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResponse {
    pub apps: Vec<AppStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_serialization() {
        let cmd = Command::Status {
            app: "my-app".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("status"));
        assert!(json.contains("my-app"));
    }

    #[test]
    fn test_deploy_command_serialization_includes_scaling() {
        let cmd = Command::Deploy {
            app: "my-app".to_string(),
            version: "v1".to_string(),
            path: "/opt/tako/apps/my-app/releases/v1".to_string(),
            routes: vec!["example.com".to_string()],
            instances: 0,
            idle_timeout: 300,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"deploy""#));
        assert!(json.contains(r#""instances":0"#));
        assert!(json.contains(r#""idle_timeout":300"#));
    }

    #[test]
    fn test_hello_roundtrip() {
        let cmd = Command::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::Hello { protocol_version } => assert_eq!(protocol_version, PROTOCOL_VERSION),
            _ => panic!("expected hello"),
        }
    }

    #[test]
    fn test_routes_command_serialization() {
        let cmd = Command::Routes;
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"routes""#));
    }

    #[test]
    fn test_server_info_command_serialization() {
        let cmd = Command::ServerInfo;
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"server_info""#));
    }

    #[test]
    fn test_enter_upgrading_command_serialization() {
        let cmd = Command::EnterUpgrading {
            owner: "controller-a".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"enter_upgrading""#));
        assert!(json.contains(r#""owner":"controller-a""#));
    }

    #[test]
    fn test_exit_upgrading_command_serialization() {
        let cmd = Command::ExitUpgrading {
            owner: "controller-a".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"exit_upgrading""#));
        assert!(json.contains(r#""owner":"controller-a""#));
    }

    #[test]
    fn test_delete_command_serialization() {
        let cmd = Command::Delete {
            app: "my-app".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"delete""#));
        assert!(json.contains(r#""app":"my-app""#));
    }

    #[test]
    fn test_response_ok() {
        let response = Response::ok(serde_json::json!({"name": "test"}));
        assert!(response.is_ok());
        assert!(response.data().is_some());
    }

    #[test]
    fn test_response_error() {
        let response = Response::error("Something went wrong");
        assert!(!response.is_ok());
        assert_eq!(response.error_message(), Some("Something went wrong"));
    }

    #[test]
    fn test_app_state_display() {
        assert_eq!(AppState::Running.to_string(), "running");
        assert_eq!(AppState::Deploying.to_string(), "deploying");
    }

    #[test]
    fn test_instance_state_display() {
        assert_eq!(InstanceState::Healthy.to_string(), "healthy");
        assert_eq!(InstanceState::Draining.to_string(), "draining");
    }

    #[test]
    fn test_app_status_deserializes_without_builds_field() {
        let value = serde_json::json!({
            "name": "demo",
            "version": "v1",
            "instances": [],
            "state": "running",
            "last_error": null
        });

        let status: AppStatus = serde_json::from_value(value).unwrap();
        assert!(status.builds.is_empty());
    }

    #[test]
    fn test_upgrade_mode_serialization() {
        let mode = UpgradeMode::Upgrading;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, r#""upgrading""#);
    }
}
