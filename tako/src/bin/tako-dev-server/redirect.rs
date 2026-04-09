use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

pub(crate) fn normalize_redirect_host(host_header: &str) -> String {
    let host = host_header.trim();
    if host.is_empty() {
        return "localhost".to_string();
    }
    if let Some(stripped) = host.strip_suffix(":80") {
        return stripped.to_string();
    }
    host.to_string()
}

pub(crate) fn redirect_location(host_header: &str, path: &str) -> String {
    let host = normalize_redirect_host(host_header);
    let path = if path.starts_with('/') { path } else { "/" };
    format!("https://{}{}", host, path)
}

pub(crate) fn parse_http_redirect_target(request: &str) -> (String, String) {
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or_default();
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let mut host = "";
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Host:") {
            host = value.trim();
            break;
        }
        if let Some(value) = line.strip_prefix("host:") {
            host = value.trim();
            break;
        }
    }
    (host.to_string(), path.to_string())
}

async fn handle_http_redirect_connection(
    mut stream: TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await??;
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let (host, path) = parse_http_redirect_target(&req);
    let location = redirect_location(&host, &path);
    let response = format!(
        "HTTP/1.1 308 Permanent Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(response.as_bytes()).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

pub(crate) async fn start_http_redirect_server(
    listen_addr: &str,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(listen_addr).await?;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            tokio::spawn(async move {
                                if let Err(e) = handle_http_redirect_connection(stream).await {
                                    tracing::warn!(error = %e, "http redirect handler failed");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "http redirect accept failed");
                        }
                    }
                }
            }
        }
    });
    Ok(())
}
