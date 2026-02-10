//! Tako Protocol Handler
//!
//! Handles special Tako protocol messages from apps:
//! - x-tako-ready: Instance is ready to receive traffic
//! - x-tako-shutdown: Instance is shutting down gracefully
//! - x-tako-metrics: Custom metrics from the app

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Tako protocol message types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TakoMessage {
    /// Instance is ready to receive traffic
    Ready {
        /// Optional message
        message: Option<String>,
    },

    /// Instance is shutting down
    Shutdown {
        /// Reason for shutdown
        reason: Option<String>,
        /// How long to wait for graceful shutdown (ms)
        timeout_ms: Option<u64>,
    },

    /// Custom metrics from the app
    Metrics {
        /// Metric values
        values: HashMap<String, MetricValue>,
    },

    /// Log message from the app
    Log {
        /// Log level
        level: LogLevel,
        /// Log message
        message: String,
        /// Additional context
        context: Option<HashMap<String, String>>,
    },
}

/// Metric value types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetricValue {
    Counter(u64),
    Gauge(f64),
    Histogram(Vec<f64>),
}

/// Log levels
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Header names for Tako protocol
pub mod headers {
    /// Header to signal ready state
    pub const TAKO_READY: &str = "x-tako-ready";
    /// Header to signal shutdown
    pub const TAKO_SHUTDOWN: &str = "x-tako-shutdown";
}

/// Parse a Tako message from a header value
pub fn parse_header_message(header_name: &str, value: &str) -> Option<TakoMessage> {
    match header_name {
        headers::TAKO_READY => Some(TakoMessage::Ready {
            message: if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            },
        }),
        headers::TAKO_SHUTDOWN => Some(TakoMessage::Shutdown {
            reason: if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            },
            timeout_ms: None,
        }),
        _ => None,
    }
}

/// Parse a Tako message from JSON body
pub fn parse_json_message(json: &str) -> Result<TakoMessage, serde_json::Error> {
    serde_json::from_str(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ready_header() {
        let msg = parse_header_message(headers::TAKO_READY, "").unwrap();
        match msg {
            TakoMessage::Ready { message } => assert!(message.is_none()),
            _ => panic!("Expected Ready message"),
        }
    }

    #[test]
    fn test_parse_ready_header_with_message() {
        let msg = parse_header_message(headers::TAKO_READY, "App initialized").unwrap();
        match msg {
            TakoMessage::Ready { message } => {
                assert_eq!(message.unwrap(), "App initialized");
            }
            _ => panic!("Expected Ready message"),
        }
    }

    #[test]
    fn test_parse_shutdown_header() {
        let msg = parse_header_message(headers::TAKO_SHUTDOWN, "graceful").unwrap();
        match msg {
            TakoMessage::Shutdown { reason, .. } => {
                assert_eq!(reason.unwrap(), "graceful");
            }
            _ => panic!("Expected Shutdown message"),
        }
    }

    #[test]
    fn test_parse_json_metrics() {
        let json = r#"{"type": "metrics", "values": {"requests": 100, "latency_ms": 45.5}}"#;
        let msg = parse_json_message(json).unwrap();

        match msg {
            TakoMessage::Metrics { values } => {
                assert!(values.contains_key("requests"));
                assert!(values.contains_key("latency_ms"));
            }
            _ => panic!("Expected Metrics message"),
        }
    }

    #[test]
    fn test_parse_json_log() {
        let json = r#"{"type": "log", "level": "info", "message": "Server started"}"#;
        let msg = parse_json_message(json).unwrap();

        match msg {
            TakoMessage::Log { level, message, .. } => {
                assert!(matches!(level, LogLevel::Info));
                assert_eq!(message, "Server started");
            }
            _ => panic!("Expected Log message"),
        }
    }

    #[test]
    fn test_serialize_message() {
        let msg = TakoMessage::Ready {
            message: Some("Ready to go".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("ready"));
        assert!(json.contains("Ready to go"));
    }
}
