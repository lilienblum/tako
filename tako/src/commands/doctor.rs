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
    buf.push(format!(
        "  {}{}  {}",
        label,
        " ".repeat(padding),
        value,
    ));
}

fn format_bool_status(enabled: bool) -> String {
    if enabled {
        output::brand_success("enabled")
    } else {
        output::brand_warning("disabled")
    }
}

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
    pf_active: bool,
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
        LOCAL_DNS_PORT, local_dns_resolver_values, pf_rules_active, system_resolver_ipv4,
        tcp_port_open,
    };

    let (advertised_ip, local_dns_port) = match dev_info {
        Ok(info) => {
            let i = info.get("info").unwrap_or(&serde_json::Value::Null);
            let ip = i
                .get("advertised_ip")
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1")
                .to_string();
            let port = i
                .get("local_dns_port")
                .and_then(|v| v.as_u64())
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(LOCAL_DNS_PORT);
            (ip, port)
        }
        Err(_) => ("127.0.0.1".to_string(), LOCAL_DNS_PORT),
    };

    let pf_active = pf_rules_active();
    let https_tcp_ok = tcp_port_open(&advertised_ip, 443, 150);
    let http_tcp_ok = tcp_port_open(&advertised_ip, 80, 150);
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
        pf_active,
        https_tcp_ok,
        http_tcp_ok,
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
    row(buf, "Config", config_dir, w);
    row(buf, "Data", data_dir, w);
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
    row(buf, "Status", &value, w);
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
            if is_dev_server_unavailable_error_message(&message) {
                row(buf, "Status", &output::brand_warning("not running"), w);
            } else {
                row(
                    buf,
                    "Status",
                    &output::brand_error(format!("error: {e}")),
                    w,
                );
            }
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

    row(buf, "Listen", listen, w);

    let port_is_duplicate = u16::try_from(port)
        .ok()
        .zip(super::dev::port_from_listen(listen))
        .is_some_and(|(reported, from_listen)| reported == from_listen);
    if port > 0 && !port_is_duplicate {
        row(buf, "Port", &port.to_string(), w);
    }

    row(
        buf,
        "Local DNS",
        &format_bool_status(local_dns_enabled),
        w,
    );
    row(buf, "Local DNS port", &local_dns_port.to_string(), w);
}

#[cfg(target_os = "macos")]
fn format_macos_sections(
    buf: &mut Vec<String>,
    dev_info: &Result<serde_json::Value, Box<dyn std::error::Error>>,
    _apps: &[crate::dev_server_client::ListedApp],
    macos: &MacosData,
) {
    if dev_info.is_err() {
        return;
    }

    let tcp_443 = format!("tcp {}:443", macos.advertised_ip);
    let tcp_80 = format!("tcp {}:80", macos.advertised_ip);
    let fwd_width = label_width(&["pf rules", &tcp_443, &tcp_80]);

    heading(buf, "Forwarding");
    row(
        buf,
        "pf rules",
        &format_active_status(macos.pf_active, "active", "not configured"),
        fwd_width,
    );
    row(
        buf,
        &tcp_443,
        &format_active_status(macos.https_tcp_ok, "ok", "unreachable"),
        fwd_width,
    );
    row(
        buf,
        &tcp_80,
        &format_active_status(macos.http_tcp_ok, "ok", "unreachable"),
        fwd_width,
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
            .map(|p| format!("  {}", output::brand_muted(&format!("pid {p}"))))
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
    dev_info: &Result<serde_json::Value, Box<dyn std::error::Error>>,
    _apps: &[crate::dev_server_client::ListedApp],
    macos: &MacosData,
) {
    use super::dev::TAKO_RESOLVER_FILE;

    if dev_info.is_err() {
        return;
    }

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
        Some((nameserver, port))
            if nameserver == "127.0.0.1" && *port == macos.local_dns_port =>
        {
            row(
                buf,
                "Resolver",
                &format!(
                    "{} {} {}",
                    TAKO_RESOLVER_FILE,
                    output::brand_muted("→"),
                    format!("{nameserver}:{port}")
                ),
                dns_w,
            );
        }
        Some((nameserver, port)) => {
            row(
                buf,
                "Resolver",
                &format!(
                    "{} {} {} {}",
                    TAKO_RESOLVER_FILE,
                    output::brand_muted("→"),
                    format!("{nameserver}:{port}"),
                    output::brand_warning(&format!(
                        "(expected 127.0.0.1:{})",
                        macos.local_dns_port
                    ))
                ),
                dns_w,
            );
        }
        None => {
            row(
                buf,
                "Resolver",
                &format!(
                    "{} {} {}",
                    TAKO_RESOLVER_FILE,
                    output::brand_muted("→"),
                    output::brand_warning("missing")
                ),
                dns_w,
            );
        }
    }

    for (host, ip) in &macos.host_dns_results {
        match ip {
            Some(ip) if ip == "127.0.0.1" => {
                row(
                    buf,
                    host,
                    &format!("{} {}", output::brand_muted("→"), ip),
                    dns_w,
                );
            }
            Some(ip) => {
                row(
                    buf,
                    host,
                    &format!(
                        "{} {} {}",
                        output::brand_muted("→"),
                        ip,
                        output::brand_warning("(expected 127.0.0.1)")
                    ),
                    dns_w,
                );
            }
            None => {
                row(
                    buf,
                    host,
                    &format!(
                        "{} {}",
                        output::brand_muted("→"),
                        output::brand_warning("no answer")
                    ),
                    dns_w,
                );
            }
        }
    }
}
