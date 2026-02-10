//! Management socket for receiving commands from tako CLI
//!
//! Commands:
//! - deploy: Rolling update of an app
//! - stop: Stop an app
//! - status: Get app status
//! - list: List all apps
//! - reload: Reload configuration

use std::future::Future;
use std::path::Path;
use tokio::net::{UnixListener, UnixStream};

use tako_socket::serve_jsonl_connection;

// Re-export protocol types from tako-core for shared use
pub use tako_core::{AppState, AppStatus, Command, InstanceState, InstanceStatus, Response};

/// Management socket server
pub struct SocketServer {
    path: String,
}

fn prepare_socket_path(path: &Path) -> Result<(), std::io::Error> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    Ok(())
}

impl SocketServer {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    /// Start listening for commands
    pub async fn run<F, Fut>(&self, handler: F) -> Result<(), std::io::Error>
    where
        F: Fn(Command) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let path = Path::new(&self.path);
        prepare_socket_path(path)?;

        let listener = UnixListener::bind(&self.path)?;
        tracing::info!("Management socket listening on {}", self.path);

        let handler = std::sync::Arc::new(handler);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let handler = handler.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, handler).await {
                            tracing::error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }
}

async fn handle_connection<F, Fut>(
    stream: UnixStream,
    handler: std::sync::Arc<F>,
) -> Result<(), std::io::Error>
where
    F: Fn(Command) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    serve_jsonl_connection(
        stream,
        move |cmd| {
            let handler = handler.clone();
            async move {
                tracing::debug!("Received command: {:?}", cmd);
                handler(cmd).await
            }
        },
        |e| Response::error(format!("Invalid command: {}", e)),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::sleep;

    #[test]
    fn test_parse_deploy_command() {
        let json = r#"{"command": "deploy", "app": "my-app", "version": "1.0.0", "path": "/var/lib/tako/my-app/releases/1.0.0", "routes": ["api.example.com", "example.com/api/*"], "instances": 0, "idle_timeout": 300}"#;
        let cmd: Command = serde_json::from_str(json).unwrap();

        match cmd {
            Command::Deploy {
                app,
                version,
                path,
                routes,
                instances,
                idle_timeout,
            } => {
                assert_eq!(app, "my-app");
                assert_eq!(version, "1.0.0");
                assert!(path.contains("releases"));
                assert_eq!(routes.len(), 2);
                assert_eq!(instances, 0);
                assert_eq!(idle_timeout, 300);
            }
            _ => panic!("Expected Deploy command"),
        }
    }

    #[test]
    fn test_parse_stop_command() {
        let json = r#"{"command": "stop", "app": "my-app"}"#;
        let cmd: Command = serde_json::from_str(json).unwrap();

        match cmd {
            Command::Stop { app } => {
                assert_eq!(app, "my-app");
            }
            _ => panic!("Expected Stop command"),
        }
    }

    #[test]
    fn test_parse_list_command() {
        let json = r#"{"command": "list"}"#;
        let cmd: Command = serde_json::from_str(json).unwrap();

        assert!(matches!(cmd, Command::List));
    }

    #[test]
    fn test_parse_hello_command() {
        let json = r#"{"command": "hello", "protocol_version": 2}"#;
        let cmd: Command = serde_json::from_str(json).unwrap();
        match cmd {
            Command::Hello { protocol_version } => assert_eq!(protocol_version, 2),
            _ => panic!("Expected Hello command"),
        }
    }

    #[test]
    fn test_parse_routes_command() {
        let json = r#"{"command": "routes"}"#;
        let cmd: Command = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, Command::Routes));
    }

    #[test]
    fn test_serialize_ok_response() {
        let response = Response::ok(serde_json::json!({"name": "my-app", "status": "running"}));
        let json = serde_json::to_string(&response).unwrap();

        assert!(json.contains("ok"));
        assert!(json.contains("my-app"));
    }

    #[test]
    fn test_serialize_error_response() {
        let response = Response::error("App not found");
        let json = serde_json::to_string(&response).unwrap();

        assert!(json.contains("error"));
        assert!(json.contains("App not found"));
    }

    #[test]
    fn test_app_state_serialization() {
        let state = AppState::Running;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, r#""running""#);
    }

    #[test]
    fn test_instance_state_serialization() {
        let state = InstanceState::Healthy;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, r#""healthy""#);
    }

    #[test]
    fn test_prepare_socket_path_removes_stale_file_and_creates_parent() {
        let temp = TempDir::new().unwrap();
        let socket_path = temp.path().join("nested").join("tako.sock");
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        std::fs::write(&socket_path, b"stale").unwrap();

        prepare_socket_path(&socket_path).unwrap();

        assert!(socket_path.parent().unwrap().exists());
        assert!(!socket_path.exists(), "stale socket file should be removed");
    }

    #[test]
    fn test_prepare_socket_path_without_parent_is_ok() {
        let path = std::path::PathBuf::from("tako.sock");
        prepare_socket_path(&path).unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn test_handle_connection_returns_error_for_invalid_json() {
        let (mut client, server) = UnixStream::pair().unwrap();

        let handler = Arc::new(|_cmd: Command| async move { Response::ok(serde_json::json!({})) });
        let server_task = tokio::spawn(handle_connection(server, handler));

        client.write_all(b"not-json\n").await.unwrap();
        client.shutdown().await.unwrap();

        let mut raw = Vec::new();
        client.read_to_end(&mut raw).await.unwrap();
        let response = String::from_utf8(raw).unwrap();
        assert!(response.contains("\"status\":\"error\""), "{}", response);
        assert!(response.contains("Invalid command"), "{}", response);

        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_run_replaces_stale_socket_file() {
        let temp = TempDir::new().unwrap();
        let probe_path = temp.path().join("probe.sock");
        if std::os::unix::net::UnixListener::bind(&probe_path).is_err() {
            return;
        }
        let _ = std::fs::remove_file(&probe_path);

        let socket_path = temp.path().join("sockdir").join("tako.sock");
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        std::fs::write(&socket_path, b"stale-file").unwrap();

        let path_str = socket_path.to_string_lossy().to_string();
        let server = SocketServer::new(path_str.clone());
        let server_task = tokio::spawn(async move {
            let _ = server
                .run(|cmd| async move {
                    match cmd {
                        Command::List => Response::ok(serde_json::json!({"ok": true})),
                        _ => Response::error("unexpected command"),
                    }
                })
                .await;
        });

        let mut ready = false;
        for _ in 0..100 {
            if let Ok(meta) = std::fs::metadata(&socket_path)
                && meta.file_type().is_socket()
            {
                ready = true;
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        assert!(ready, "socket was not created at {}", socket_path.display());

        let mut client = UnixStream::connect(&path_str).await.unwrap();
        client.write_all(br#"{"command":"list"}"#).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.shutdown().await.unwrap();

        let mut raw = Vec::new();
        client.read_to_end(&mut raw).await.unwrap();
        let response = String::from_utf8(raw).unwrap();
        assert!(response.contains("\"status\":\"ok\""), "{}", response);

        server_task.abort();
        let _ = server_task.await;
    }
}
