use crate::output;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // ── Gather all data upfront ──────────────────────────────────────────

    let config_dir = crate::paths::tako_config_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(error)".into());
    let data_dir = crate::paths::tako_data_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(error)".into());

    let ca_status = gather_ca_status();

    let dev_info = crate::dev_server_client::info().await;
    let apps = crate::dev_server_client::list_apps()
        .await
        .unwrap_or_default();

    #[cfg(target_os = "macos")]
    let macos_data = gather_macos_data(&dev_info, &apps);

    // ── Format output ────────────────────────────────────────────────────

    let mut buf = Vec::new();

    format_paths(&mut buf, &config_dir, &data_dir);
    format_certificate(&mut buf, &ca_status);
    format_dev_server(&mut buf, &dev_info);

    #[cfg(target_os = "macos")]
    format_macos_sections(&mut buf, &dev_info, &apps, &macos_data);

    format_apps(&mut buf, &apps);

    #[cfg(target_os = "macos")]
    format_local_dns(&mut buf, &dev_info, &apps, &macos_data);

    for line in &buf {
        eprintln!("{line}");
    }

    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn heading(buf: &mut Vec<String>, title: &str) {
    if output::is_pretty() {
        buf.push(String::new());
        buf.push(output::strong(title));
    }
}

fn label_width(labels: &[&str]) -> usize {
    labels.iter().map(|s| s.len()).max().unwrap_or(0)
}

fn row(buf: &mut Vec<String>, label: &str, value: &str, width: usize) {
    let padding = width.saturating_sub(label.len());
    buf.push(format!("  {}{}  {}", label, " ".repeat(padding), value,));
}

fn hint(buf: &mut Vec<String>, text: &str) {
    buf.push(format!("    {}", output::brand_muted(text)));
}

fn hinted_row(buf: &mut Vec<String>, label: &str, value: &str, width: usize, text: &str) {
    row(buf, label, value, width);
    hint(buf, text);
}

fn format_bool_status(enabled: bool) -> String {
    if enabled {
        output::brand_success("enabled")
    } else {
        output::brand_warning("disabled")
    }
}

#[cfg(target_os = "macos")]
fn format_active_status(ok: bool, ok_label: &str, fail_label: &str) -> String {
    if ok {
        output::brand_success(ok_label)
    } else {
        output::brand_error(fail_label)
    }
}

// ─── Data gathering ──────────────────────────────────────────────────────────

enum CaStatus {
    Error(String),
    NotCreated,
    Trusted,
    Untrusted,
}

fn gather_ca_status() -> CaStatus {
    let store = match crate::dev::LocalCAStore::new() {
        Ok(s) => s,
        Err(e) => return CaStatus::Error(e.to_string()),
    };
    if !store.ca_exists() {
        CaStatus::NotCreated
    } else if store.is_ca_trusted() {
        CaStatus::Trusted
    } else {
        CaStatus::Untrusted
    }
}

#[cfg(target_os = "macos")]
struct MacosData {
    loopback_proxy: super::dev::LoopbackProxyStatus,
    https_tcp_ok: bool,
    http_tcp_ok: bool,
    advertised_ip: String,
    local_dns_port: u16,
    resolver_values: Option<(String, u16)>,
    host_dns_results: Vec<(String, Option<String>)>,
}

#[cfg(target_os = "macos")]
fn gather_macos_data(
    dev_info: &Result<serde_json::Value, Box<dyn std::error::Error>>,
    apps: &[crate::dev_server_client::ListedApp],
) -> MacosData {
    use super::dev::{
        DEV_LOOPBACK_ADDR, LOCAL_DNS_PORT, local_dns_resolver_values, loopback_proxy_status,
        system_resolver_ipv4,
    };

    let local_dns_port = match dev_info {
        Ok(info) => {
            let i = info.get("info").unwrap_or(&serde_json::Value::Null);
            i.get("local_dns_port")
                .and_then(|v| v.as_u64())
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(LOCAL_DNS_PORT)
        }
        Err(_) => LOCAL_DNS_PORT,
    };

    let loopback_proxy = loopback_proxy_status();
    let advertised_ip = DEV_LOOPBACK_ADDR.to_string();
    let resolver_values = local_dns_resolver_values();

    let host_dns_results: Vec<(String, Option<String>)> = apps
        .iter()
        .flat_map(|a| {
            if a.hosts.is_empty() {
                vec![crate::dev::get_tako_domain(&a.app_name)]
            } else {
                a.hosts.clone()
            }
        })
        .filter(|h| h.ends_with(".tako.test"))
        .map(|host| {
            let ip = system_resolver_ipv4(&host);
            (host, ip)
        })
        .collect();

    MacosData {
        https_tcp_ok: loopback_proxy.https_ready,
        http_tcp_ok: loopback_proxy.http_ready,
        loopback_proxy,
        advertised_ip,
        local_dns_port,
        resolver_values,
        host_dns_results,
    }
}

// ─── Formatting ──────────────────────────────────────────────────────────────

fn format_paths(buf: &mut Vec<String>, config_dir: &str, data_dir: &str) {
    heading(buf, "Paths");
    let w = label_width(&["Config", "Data"]);
    hinted_row(
        buf,
        "Config",
        config_dir,
        w,
        "Directory where Tako stores local configuration files",
    );
    hinted_row(
        buf,
        "Data",
        data_dir,
        w,
        "Directory where Tako stores runtime state and cached assets",
    );
}

fn format_certificate(buf: &mut Vec<String>, status: &CaStatus) {
    heading(buf, "Local CA");
    let w = label_width(&["Status"]);
    let value = match status {
        CaStatus::Error(e) => output::brand_error(format!("error: {e}")),
        CaStatus::NotCreated => output::brand_warning("not created"),
        CaStatus::Trusted => output::brand_success("trusted"),
        CaStatus::Untrusted => output::brand_warning("untrusted"),
    };
    hinted_row(
        buf,
        "Status",
        &value,
        w,
        "Trust state of the Tako local certificate authority for https://*.tako.test",
    );
}

fn format_dev_server(
    buf: &mut Vec<String>,
    dev_info: &Result<serde_json::Value, Box<dyn std::error::Error>>,
) {
    use super::dev::{LOCAL_DNS_PORT, is_dev_server_unavailable_error_message};

    heading(buf, "Development server");

    let w = label_width(&["Status", "Listen", "Port", "Local DNS", "Local DNS port"]);

    let info = match dev_info {
        Ok(info) => info,
        Err(e) => {
            let message = e.to_string();
            let status = if is_dev_server_unavailable_error_message(&message) {
                output::brand_warning("not running")
            } else {
                output::brand_error(format!("error: {e}"))
            };
            hinted_row(
                buf,
                "Status",
                &status,
                w,
                "Current health of the local Tako development server process",
            );
            return;
        }
    };

    let i = info.get("info").unwrap_or(&serde_json::Value::Null);
    let listen = i
        .get("listen")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let port = i.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
    let local_dns_enabled = i
        .get("local_dns_enabled")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let local_dns_port = i
        .get("local_dns_port")
        .and_then(|v| v.as_u64())
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(LOCAL_DNS_PORT);

    hinted_row(
        buf,
        "Listen",
        listen,
        w,
        "Address where the Tako development server listens for local proxy traffic",
    );

    let port_is_duplicate = u16::try_from(port)
        .ok()
        .zip(super::dev::port_from_listen(listen))
        .is_some_and(|(reported, from_listen)| reported == from_listen);
    if port > 0 && !port_is_duplicate {
        hinted_row(
            buf,
            "Port",
            &port.to_string(),
            w,
            "Public HTTPS port currently reported by the Tako development server",
        );
    }

    hinted_row(
        buf,
        "Local DNS",
        &format_bool_status(local_dns_enabled),
        w,
        "Whether the Tako development server has its local DNS responder enabled",
    );
    hinted_row(
        buf,
        "Local DNS port",
        &local_dns_port.to_string(),
        w,
        "UDP port used by the local Tako DNS responder",
    );
}

#[cfg(target_os = "macos")]
fn format_macos_sections(
    buf: &mut Vec<String>,
    _dev_info: &Result<serde_json::Value, Box<dyn std::error::Error>>,
    _apps: &[crate::dev_server_client::ListedApp],
    macos: &MacosData,
) {
    let tcp_443 = format!("TCP {}:443", macos.advertised_ip);
    let tcp_80 = format!("TCP {}:80", macos.advertised_ip);
    let fwd_width = label_width(&[
        "Installed",
        "Boot Helper",
        "Alias",
        "Launchd",
        &tcp_443,
        &tcp_80,
    ]);

    heading(buf, "Loopback Proxy");
    hinted_row(
        buf,
        "Installed",
        &format_active_status(macos.loopback_proxy.installed, "ok", "missing"),
        fwd_width,
        "Binary and support files are present on disk",
    );
    hinted_row(
        buf,
        "Boot Helper",
        &format_active_status(
            macos.loopback_proxy.bootstrap_loaded,
            "loaded",
            "not loaded",
        ),
        fwd_width,
        "Boot-time helper is loaded so Tako can restore loopback proxy setup",
    );
    hinted_row(
        buf,
        "Alias",
        &format_active_status(macos.loopback_proxy.alias_ready, "ok", "missing"),
        fwd_width,
        "127.77.0.1 is assigned on the lo0 loopback interface",
    );
    hinted_row(
        buf,
        "Launchd",
        &format_active_status(macos.loopback_proxy.launchd_loaded, "loaded", "not loaded"),
        fwd_width,
        "macOS launchd has loaded the proxy service definition",
    );
    hinted_row(
        buf,
        &tcp_443,
        &format_active_status(macos.https_tcp_ok, "ok", "unreachable"),
        fwd_width,
        "HTTPS proxy is listening on the loopback address and accepts connections",
    );
    hinted_row(
        buf,
        &tcp_80,
        &format_active_status(macos.http_tcp_ok, "ok", "unreachable"),
        fwd_width,
        "HTTP proxy is listening on the loopback address and accepts connections",
    );
}

fn format_apps(buf: &mut Vec<String>, apps: &[crate::dev_server_client::ListedApp]) {
    if apps.is_empty() {
        return;
    }
    heading(buf, "Apps");
    for a in apps {
        let hosts = if a.hosts.is_empty() {
            "(default)".to_string()
        } else {
            a.hosts.join(", ")
        };
        let pid_str = a
            .pid
            .map(|p| format!("  {}", output::brand_muted(format!("pid {p}"))))
            .unwrap_or_default();
        buf.push(format!(
            "  {}  {}  port {}{}",
            output::strong(&a.app_name),
            output::brand_muted(&hosts),
            a.upstream_port,
            pid_str,
        ));
    }
}

#[cfg(target_os = "macos")]
fn format_local_dns(
    buf: &mut Vec<String>,
    _dev_info: &Result<serde_json::Value, Box<dyn std::error::Error>>,
    _apps: &[crate::dev_server_client::ListedApp],
    macos: &MacosData,
) {
    use super::dev::TAKO_RESOLVER_FILE;

    heading(buf, "Local DNS");

    let mut dns_labels: Vec<&str> = vec!["Resolver"];
    let host_strs: Vec<&str> = macos
        .host_dns_results
        .iter()
        .map(|(h, _)| h.as_str())
        .collect();
    dns_labels.extend_from_slice(&host_strs);
    let dns_w = label_width(&dns_labels);

    match &macos.resolver_values {
        Some((nameserver, port)) if nameserver == "127.0.0.1" && *port == macos.local_dns_port => {
            hinted_row(
                buf,
                "Resolver",
                &format!(
                    "{} {} {}",
                    TAKO_RESOLVER_FILE,
                    output::brand_muted("→"),
                    format_args!("{nameserver}:{port}")
                ),
                dns_w,
                "Resolver file that should direct *.tako.test lookups to the local DNS server",
            );
        }
        Some((nameserver, port)) => {
            hinted_row(
                buf,
                "Resolver",
                &format!(
                    "{} {} {} {}",
                    TAKO_RESOLVER_FILE,
                    output::brand_muted("→"),
                    format_args!("{nameserver}:{port}"),
                    output::brand_warning(format!("(expected 127.0.0.1:{})", macos.local_dns_port))
                ),
                dns_w,
                "Resolver file that should direct *.tako.test lookups to the local DNS server",
            );
        }
        None => {
            hinted_row(
                buf,
                "Resolver",
                &format!(
                    "{} {} {}",
                    TAKO_RESOLVER_FILE,
                    output::brand_muted("→"),
                    output::brand_warning("missing")
                ),
                dns_w,
                "Resolver file that should direct *.tako.test lookups to the local DNS server",
            );
        }
    }

    for (host, ip) in &macos.host_dns_results {
        match ip {
            Some(ip) if ip == &macos.advertised_ip => {
                hinted_row(
                    buf,
                    host,
                    &format!("{} {}", output::brand_muted("→"), ip),
                    dns_w,
                    "Current system DNS answer for this app hostname",
                );
            }
            Some(ip) => {
                hinted_row(
                    buf,
                    host,
                    &format!(
                        "{} {} {}",
                        output::brand_muted("→"),
                        ip,
                        output::brand_warning(format!("(expected {})", macos.advertised_ip))
                    ),
                    dns_w,
                    "Current system DNS answer for this app hostname",
                );
            }
            None => {
                hinted_row(
                    buf,
                    host,
                    &format!(
                        "{} {}",
                        output::brand_muted("→"),
                        output::brand_warning("no answer")
                    ),
                    dns_w,
                    "Current system DNS answer for this app hostname",
                );
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_static_doctor_rows_include_hints() {
        let mut buf = Vec::new();
        let dev_info = Ok(json!({
            "info": {
                "listen": "127.0.0.1:47831",
                "port": 47831,
                "local_dns_enabled": true,
                "local_dns_port": 53535
            }
        }));
        let dns_info = Ok(json!({
            "info": {
                "listen": "127.0.0.1:47831",
                "port": 47831,
                "local_dns_enabled": true,
                "local_dns_port": 53535
            }
        }));
        let macos = MacosData {
            loopback_proxy: super::super::dev::LoopbackProxyStatus {
                installed: true,
                bootstrap_loaded: true,
                alias_ready: true,
                launchd_loaded: true,
                https_ready: true,
                http_ready: true,
            },
            https_tcp_ok: true,
            http_tcp_ok: true,
            advertised_ip: "127.77.0.1".to_string(),
            local_dns_port: 53535,
            resolver_values: Some(("127.0.0.1".to_string(), 53535)),
            host_dns_results: vec![(
                "bun-example.tako.test".to_string(),
                Some("127.77.0.1".to_string()),
            )],
        };

        format_paths(&mut buf, "/tmp/tako-config", "/tmp/tako-data");
        format_certificate(&mut buf, &CaStatus::Trusted);
        format_dev_server(&mut buf, &dev_info);
        format_local_dns(&mut buf, &dns_info, &[], &macos);

        assert!(
            buf.iter()
                .any(|line| line.contains("Directory where Tako stores local configuration files"))
        );
        assert!(buf.iter().any(|line| {
            line.contains("Directory where Tako stores runtime state and cached assets")
        }));
        assert!(buf.iter().any(|line| line.contains(
            "Trust state of the Tako local certificate authority for https://*.tako.test"
        )));
        assert!(buf.iter().any(|line| {
            line.contains(
                "Address where the Tako development server listens for local proxy traffic",
            )
        }));
        assert!(buf.iter().any(|line| {
            line.contains("Whether the Tako development server has its local DNS responder enabled")
        }));
        assert!(
            buf.iter()
                .any(|line| line.contains("UDP port used by the local Tako DNS responder"))
        );
        assert!(buf.iter().any(|line| line.contains(
            "Resolver file that should direct *.tako.test lookups to the local DNS server"
        )));
    }

    #[test]
    fn format_dev_server_uses_single_status_hint_for_unavailable_state() {
        let mut buf = Vec::new();
        let dev_info: Result<serde_json::Value, Box<dyn std::error::Error>> = Err(
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused").into(),
        );

        format_dev_server(&mut buf, &dev_info);

        assert!(buf.iter().any(|line| line.contains("Status")));
        assert!(buf.iter().any(|line| line.contains("not running")));
        assert_eq!(
            buf.iter()
                .filter(|line| line
                    .contains("Current health of the local Tako development server process"))
                .count(),
            1
        );
    }

    #[test]
    fn format_macos_sections_capitalizes_loopback_proxy_labels() {
        let mut buf = Vec::new();
        let macos = MacosData {
            loopback_proxy: super::super::dev::LoopbackProxyStatus {
                installed: true,
                bootstrap_loaded: true,
                alias_ready: true,
                launchd_loaded: true,
                https_ready: true,
                http_ready: true,
            },
            https_tcp_ok: true,
            http_tcp_ok: true,
            advertised_ip: "127.77.0.1".to_string(),
            local_dns_port: 53535,
            resolver_values: Some(("127.0.0.1".to_string(), 53535)),
            host_dns_results: Vec::new(),
        };

        let dev_info = Err(std::io::Error::other("offline").into());
        format_macos_sections(&mut buf, &dev_info, &[], &macos);

        assert!(buf.iter().any(|line| line.contains("Installed")));
        assert!(buf.iter().any(|line| line.contains("Boot Helper")));
        assert!(buf.iter().any(|line| line.contains("Alias")));
        assert!(buf.iter().any(|line| line.contains("Launchd")));
        assert!(buf.iter().any(|line| line.contains("TCP 127.77.0.1:443")));
        assert!(buf.iter().any(|line| line.contains("TCP 127.77.0.1:80")));
        assert!(
            buf.iter()
                .any(|line| line.contains("Binary and support files are present on disk"))
        );
        assert!(buf.iter().any(|line| {
            line.contains("Boot-time helper is loaded so Tako can restore loopback proxy setup")
        }));
        assert!(
            buf.iter()
                .any(|line| line.contains("127.77.0.1 is assigned on the lo0 loopback interface"))
        );
        assert!(
            buf.iter()
                .any(|line| line.contains("macOS launchd has loaded the proxy service definition"))
        );
        assert!(buf.iter().any(|line| {
            line.contains(
                "HTTPS proxy is listening on the loopback address and accepts connections",
            )
        }));
        assert!(buf.iter().any(|line| {
            line.contains("HTTP proxy is listening on the loopback address and accepts connections")
        }));
    }

    #[test]
    fn format_local_dns_expects_macos_loopback_ip_for_app_hosts() {
        let mut buf = Vec::new();
        let macos = MacosData {
            loopback_proxy: super::super::dev::LoopbackProxyStatus {
                installed: true,
                bootstrap_loaded: true,
                alias_ready: true,
                launchd_loaded: true,
                https_ready: true,
                http_ready: true,
            },
            https_tcp_ok: true,
            http_tcp_ok: true,
            advertised_ip: "127.77.0.1".to_string(),
            local_dns_port: 53535,
            resolver_values: Some(("127.0.0.1".to_string(), 53535)),
            host_dns_results: vec![(
                "bun-example.tako.test".to_string(),
                Some("127.0.0.1".to_string()),
            )],
        };

        let dev_info = Err(std::io::Error::other("offline").into());
        format_local_dns(&mut buf, &dev_info, &[], &macos);

        assert!(
            buf.iter()
                .any(|line| line.contains("(expected 127.77.0.1)")),
            "expected loopback mismatch warning in output: {buf:?}"
        );
    }

    #[test]
    fn format_local_dns_accepts_advertised_loopback_ip_for_app_hosts() {
        let mut buf = Vec::new();
        let macos = MacosData {
            loopback_proxy: super::super::dev::LoopbackProxyStatus {
                installed: true,
                bootstrap_loaded: true,
                alias_ready: true,
                launchd_loaded: true,
                https_ready: true,
                http_ready: true,
            },
            https_tcp_ok: true,
            http_tcp_ok: true,
            advertised_ip: "127.77.0.1".to_string(),
            local_dns_port: 53535,
            resolver_values: Some(("127.0.0.1".to_string(), 53535)),
            host_dns_results: vec![(
                "bun-example.tako.test".to_string(),
                Some("127.77.0.1".to_string()),
            )],
        };

        let dev_info = Err(std::io::Error::other("offline").into());
        format_local_dns(&mut buf, &dev_info, &[], &macos);

        assert!(
            buf.iter()
                .any(|line| line.contains("bun-example.tako.test") && line.contains("127.77.0.1")),
            "expected successful loopback resolution in output: {buf:?}"
        );
        assert!(
            !buf.iter()
                .any(|line| line.contains("(expected 127.77.0.1)")),
            "did not expect mismatch warning in output: {buf:?}"
        );
    }
}
