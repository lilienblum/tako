//! Tako server protocol types for management socket communication
//!
//! These types are shared between the CLI and tako-server for
//! communication via the Unix management socket.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

pub const PROTOCOL_VERSION: u32 = 0;
const DEPLOYMENT_APP_ID_SEPARATOR: char = '/';

pub fn deployment_app_id(app_name: &str, env_name: &str) -> String {
    format!("{app_name}{DEPLOYMENT_APP_ID_SEPARATOR}{env_name}")
}

pub fn split_deployment_app_id(app_id: &str) -> Option<(&str, &str)> {
    let (app_name, env_name) = app_id.split_once(DEPLOYMENT_APP_ID_SEPARATOR)?;
    if app_name.is_empty() || env_name.is_empty() || env_name.contains(DEPLOYMENT_APP_ID_SEPARATOR)
    {
        return None;
    }
    Some((app_name, env_name))
}

pub fn deployment_app_id_filename(app_id: &str) -> String {
    app_id.replace(DEPLOYMENT_APP_ID_SEPARATOR, "%2F")
}

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

        /// Secret environment variables injected into app processes at spawn time.
        /// Non-secret env vars are read by the server from app.json in the release dir.
        /// When `None`, the server keeps existing secrets for this app.
        #[serde(default)]
        secrets: Option<HashMap<String, String>>,
    },

    /// Update the desired minimum number of instances for an app.
    Scale { app: String, instances: u8 },

    /// Stop an app
    Stop { app: String },

    /// Delete an app from runtime state
    Delete { app: String },

    /// Get status of an app
    Status { app: String },

    /// List all apps
    List,

    /// List release/build history for an app
    ListReleases { app: String },

    /// Roll back an app to a previously deployed release/build
    Rollback { app: String, version: String },

    /// List all configured routes (all apps)
    Routes,

    /// Update secrets for an app
    UpdateSecrets {
        app: String,
        secrets: HashMap<String, String>,
    },

    /// Get the SHA-256 hash of an app's current secrets
    GetSecretsHash { app: String },

    /// Get server runtime information (ports, data dir, upgrade mode).
    ServerInfo,

    /// Enter upgrading mode with a durable lock owner.
    EnterUpgrading { owner: String },

    /// Exit upgrading mode for the lock owner.
    ExitUpgrading { owner: String },

    /// Inject an ACME challenge token (for testing HTTP-01 challenge serving).
    InjectChallengeToken {
        token: String,
        key_authorization: String,
    },
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
    pub pid: u32,
    pub mode: UpgradeMode,
    pub socket: String,
    pub data_dir: String,
    pub http_port: u16,
    pub https_port: u16,
    pub no_acme: bool,
    pub acme_staging: bool,
    pub acme_email: Option<String>,
    pub renewal_interval_hours: u64,
    #[serde(default)]
    pub dns_provider: Option<String>,
    #[serde(default)]
    pub worker: bool,
    #[serde(default)]
    pub metrics_port: Option<u16>,
    #[serde(default)]
    pub server_name: Option<String>,
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
    pub id: String,
    pub state: InstanceState,
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

/// Release/build metadata for an app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub current: bool,
    pub deployed_at_unix_secs: Option<i64>,
    #[serde(default)]
    pub commit_message: Option<String>,
    #[serde(default)]
    pub git_dirty: Option<bool>,
}

/// Response payload for `list_releases`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListReleasesResponse {
    pub app: String,
    pub releases: Vec<ReleaseInfo>,
}

/// Compute a stable SHA-256 hash of a secrets map.
///
/// The hash is computed over sorted key-value pairs to ensure deterministic
/// output regardless of HashMap iteration order. Returns a hex-encoded digest.
/// An empty map produces a distinct hash (the SHA-256 of an empty input).
pub fn compute_secrets_hash(secrets: &HashMap<String, String>) -> String {
    let sorted: BTreeMap<&String, &String> = secrets.iter().collect();
    let mut hasher = Sha256::new();
    for (key, value) in &sorted {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
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
            secrets: Some(HashMap::from([(
                "API_KEY".to_string(),
                "secret123".to_string(),
            )])),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"deploy""#));
        assert!(json.contains(r#""secrets":{"API_KEY":"secret123"}"#));
        assert!(!json.contains(r#""idle_timeout":"#));
    }

    #[test]
    fn test_deploy_command_deserialization_defaults_secrets_when_missing() {
        let json = r#"{
            "command":"deploy",
            "app":"my-app",
            "version":"v1",
            "path":"/opt/tako/apps/my-app/releases/v1",
            "routes":["example.com"]
        }"#;
        let cmd: Command = serde_json::from_str(json).unwrap();
        match cmd {
            Command::Deploy { secrets, .. } => assert!(secrets.is_none()),
            _ => panic!("Expected deploy command"),
        }
    }

    #[test]
    fn test_deployment_app_id_round_trip() {
        let app_id = deployment_app_id("my-app", "staging");
        assert_eq!(app_id, "my-app/staging");
        assert_eq!(
            split_deployment_app_id(&app_id),
            Some(("my-app", "staging"))
        );
    }

    #[test]
    fn test_split_deployment_app_id_rejects_invalid_values() {
        assert_eq!(split_deployment_app_id("my-app"), None);
        assert_eq!(split_deployment_app_id("/staging"), None);
        assert_eq!(split_deployment_app_id("my-app/"), None);
        assert_eq!(split_deployment_app_id("my-app/staging/blue"), None);
    }

    #[test]
    fn test_deployment_app_id_filename_encodes_separator() {
        assert_eq!(
            deployment_app_id_filename("my-app/staging"),
            "my-app%2Fstaging"
        );
    }

    #[test]
    fn test_scale_command_serialization() {
        let cmd = Command::Scale {
            app: "my-app".to_string(),
            instances: 3,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"scale""#));
        assert!(json.contains(r#""app":"my-app""#));
        assert!(json.contains(r#""instances":3"#));
    }

    #[test]
    fn test_scale_command_deserialization() {
        let json = r#"{"command":"scale","app":"my-app","instances":2}"#;
        let cmd: Command = serde_json::from_str(json).unwrap();
        match cmd {
            Command::Scale { app, instances } => {
                assert_eq!(app, "my-app");
                assert_eq!(instances, 2);
            }
            _ => panic!("Expected scale command"),
        }
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
    fn test_list_releases_command_serialization() {
        let cmd = Command::ListReleases {
            app: "my-app".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"list_releases""#));
        assert!(json.contains(r#""app":"my-app""#));
    }

    #[test]
    fn test_rollback_command_serialization() {
        let cmd = Command::Rollback {
            app: "my-app".to_string(),
            version: "abc1234".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"rollback""#));
        assert!(json.contains(r#""app":"my-app""#));
        assert!(json.contains(r#""version":"abc1234""#));
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

    #[test]
    fn test_get_secrets_hash_command_serialization() {
        let cmd = Command::GetSecretsHash {
            app: "my-app".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"get_secrets_hash""#));
        assert!(json.contains(r#""app":"my-app""#));
    }

    #[test]
    fn test_compute_secrets_hash_deterministic() {
        let secrets = HashMap::from([
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "1".to_string()),
        ]);
        let hash1 = compute_secrets_hash(&secrets);
        let hash2 = compute_secrets_hash(&secrets);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_secrets_hash_order_independent() {
        let mut a = HashMap::new();
        a.insert("X".to_string(), "1".to_string());
        a.insert("Y".to_string(), "2".to_string());

        let mut b = HashMap::new();
        b.insert("Y".to_string(), "2".to_string());
        b.insert("X".to_string(), "1".to_string());

        assert_eq!(compute_secrets_hash(&a), compute_secrets_hash(&b));
    }

    #[test]
    fn test_compute_secrets_hash_differs_for_different_values() {
        let a = HashMap::from([("KEY".to_string(), "value1".to_string())]);
        let b = HashMap::from([("KEY".to_string(), "value2".to_string())]);
        assert_ne!(compute_secrets_hash(&a), compute_secrets_hash(&b));
    }

    #[test]
    fn test_compute_secrets_hash_empty_map() {
        let empty = HashMap::new();
        let hash = compute_secrets_hash(&empty);
        assert!(!hash.is_empty());
        // Empty map should produce a consistent hash
        assert_eq!(hash, compute_secrets_hash(&HashMap::new()));
    }

    #[test]
    fn test_deploy_with_none_secrets_keeps_existing() {
        let cmd = Command::Deploy {
            app: "my-app".to_string(),
            version: "v1".to_string(),
            path: "/opt/tako/apps/my-app/releases/v1".to_string(),
            routes: vec!["example.com".to_string()],
            secrets: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::Deploy { secrets, .. } => assert!(secrets.is_none()),
            _ => panic!("Expected deploy command"),
        }
    }

    #[test]
    fn test_server_runtime_info_pid_roundtrip() {
        let info = ServerRuntimeInfo {
            pid: 42,
            mode: UpgradeMode::Normal,
            socket: "/var/run/tako/tako.sock".to_string(),
            data_dir: "/var/lib/tako".to_string(),
            http_port: 80,
            https_port: 443,
            no_acme: false,
            acme_staging: false,
            acme_email: None,
            renewal_interval_hours: 12,
            dns_provider: None,
            worker: false,
            metrics_port: Some(9898),
            server_name: Some("la".to_string()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: ServerRuntimeInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pid, 42);
        assert!(parsed.dns_provider.is_none());
        assert_eq!(parsed.server_name.as_deref(), Some("la"));
    }
}
