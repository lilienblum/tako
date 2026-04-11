use serde::{Deserialize, Serialize};

pub const CONTROL_SOCKET_PATH: &str = "/tmp/tako-dev-proxy.sock";

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ProxyCommand {
    EnableLan { bind_addr: Option<String> },
    DisableLan,
    Status,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProxyResponse {
    LanEnabled {
        addr: String,
    },
    LanDisabled,
    Status {
        lan_enabled: bool,
        lan_addr: Option<String>,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_lan_deserializes_with_default_bind() {
        let cmd: ProxyCommand = serde_json::from_str(r#"{"command":"enable_lan"}"#).unwrap();
        assert!(matches!(cmd, ProxyCommand::EnableLan { bind_addr: None }));
    }

    #[test]
    fn enable_lan_deserializes_with_explicit_bind() {
        let cmd: ProxyCommand =
            serde_json::from_str(r#"{"command":"enable_lan","bind_addr":"0.0.0.0"}"#).unwrap();
        match cmd {
            ProxyCommand::EnableLan { bind_addr } => {
                assert_eq!(bind_addr.as_deref(), Some("0.0.0.0"));
            }
            _ => panic!("expected EnableLan"),
        }
    }

    #[test]
    fn disable_lan_deserializes() {
        let cmd: ProxyCommand = serde_json::from_str(r#"{"command":"disable_lan"}"#).unwrap();
        assert!(matches!(cmd, ProxyCommand::DisableLan));
    }

    #[test]
    fn status_deserializes() {
        let cmd: ProxyCommand = serde_json::from_str(r#"{"command":"status"}"#).unwrap();
        assert!(matches!(cmd, ProxyCommand::Status));
    }

    #[test]
    fn lan_enabled_response_serializes() {
        let json = serde_json::to_string(&ProxyResponse::LanEnabled {
            addr: "0.0.0.0".into(),
        })
        .unwrap();
        assert!(json.contains("lan_enabled"));
        assert!(json.contains("0.0.0.0"));
    }

    #[test]
    fn status_response_serializes() {
        let json = serde_json::to_string(&ProxyResponse::Status {
            lan_enabled: true,
            lan_addr: Some("0.0.0.0".into()),
        })
        .unwrap();
        assert!(json.contains(r#""lan_enabled":true"#));
    }

    #[test]
    fn error_response_serializes() {
        let json = serde_json::to_string(&ProxyResponse::Error {
            message: "something broke".into(),
        })
        .unwrap();
        assert!(json.contains("something broke"));
    }
}
