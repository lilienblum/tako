use crate::output;

/// Label width for consistent column alignment across all sections.
const LABEL_WIDTH: usize = 14;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    print_paths();
    print_certificate();
    print_dev_server().await;

    Ok(())
}

/// Print a right-padded muted label + value row.
fn row(label: &str, value: &str, width: usize) {
    let padding = width.saturating_sub(label.len());
    eprintln!(
        "  {}{}  {}",
        output::brand_muted(label),
        " ".repeat(padding),
        value,
    );
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn print_paths() {
    eprintln!("{}", output::strong("Paths"));

    let config_dir = crate::paths::tako_config_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(error)".into());
    let data_dir = crate::paths::tako_data_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(error)".into());

    row("Config", &config_dir, LABEL_WIDTH);
    row("Data", &data_dir, LABEL_WIDTH);
}

// ---------------------------------------------------------------------------
// Certificate
// ---------------------------------------------------------------------------

fn print_certificate() {
    eprintln!();
    eprintln!("{}", output::strong("Local CA"));

    let store = match crate::dev::LocalCAStore::new() {
        Ok(s) => s,
        Err(e) => {
            row("Status", &output::brand_error(format!("error: {e}")), LABEL_WIDTH);
            return;
        }
    };

    let exists = store.ca_exists();
    let trusted = if exists { store.is_ca_trusted() } else { false };

    let status_color = if !exists {
        output::brand_warning("not created")
    } else if trusted {
        output::brand_success("trusted")
    } else {
        output::brand_warning("untrusted")
    };

    row("Status", &status_color, LABEL_WIDTH);
}

async fn print_dev_server() {
    use super::dev::{LOCAL_DNS_PORT, is_dev_server_unavailable_error_message};

    eprintln!();
    eprintln!("{}", output::strong("Development server"));

    let info = match crate::dev_server_client::info().await {
        Ok(info) => info,
        Err(e) => {
            let message = e.to_string();
            if is_dev_server_unavailable_error_message(&message) {
                row("Status", &output::brand_warning("not running"), LABEL_WIDTH);
            } else {
                row(
                    "Status",
                    &output::brand_error(format!("error: {e}")),
                    LABEL_WIDTH,
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
    let advertised_ip = i
        .get("advertised_ip")
        .and_then(|v| v.as_str())
        .unwrap_or("127.0.0.1");

    #[cfg(target_os = "macos")]
    let (pf_active, https_tcp_ok, http_tcp_ok) = {
        use super::dev::{pf_rules_active, tcp_port_open};
        let pf_active = pf_rules_active();
        let https_tcp_ok = tcp_port_open(advertised_ip, 443, 150);
        let http_tcp_ok = tcp_port_open(advertised_ip, 80, 150);
        (pf_active, https_tcp_ok, http_tcp_ok)
    };

    // Dev-server info rows
    row("Listen", listen, LABEL_WIDTH);

    let port_is_duplicate = u16::try_from(port)
        .ok()
        .zip(super::dev::port_from_listen(listen))
        .is_some_and(|(reported, from_listen)| reported == from_listen);
    if port > 0 && !port_is_duplicate {
        row("Port", &port.to_string(), LABEL_WIDTH);
    }

    row("Local DNS", &format_bool_status(local_dns_enabled), LABEL_WIDTH);
    row("Local DNS port", &local_dns_port.to_string(), LABEL_WIDTH);

    // pf forwarding (macOS only)
    #[cfg(target_os = "macos")]
    {
        let tcp_443 = format!("tcp {advertised_ip}:443");
        let tcp_80 = format!("tcp {advertised_ip}:80");
        let fwd_width = ["pf rules", &tcp_443, &tcp_80]
            .iter()
            .map(|s| s.len())
            .max()
            .unwrap_or(0);

        eprintln!();
        eprintln!("{}", output::strong("Forwarding"));
        row(
            "pf rules",
            &format_active_status(pf_active, "active", "not configured"),
            fwd_width,
        );
        row(
            &tcp_443,
            &format_active_status(https_tcp_ok, "ok", "unreachable"),
            fwd_width,
        );
        row(
            &tcp_80,
            &format_active_status(http_tcp_ok, "ok", "unreachable"),
            fwd_width,
        );
    }

    // Running apps
    let apps = crate::dev_server_client::list_apps()
        .await
        .unwrap_or_default();
    if !apps.is_empty() {
        eprintln!();
        eprintln!("{}", output::strong("Apps"));
        for a in &apps {
            let hosts = if a.hosts.is_empty() {
                "(default)".to_string()
            } else {
                a.hosts.join(", ")
            };
            let pid_str = a
                .pid
                .map(|p| format!("  {}", output::brand_muted(&format!("pid {p}"))))
                .unwrap_or_default();
            eprintln!(
                "  {}  {}  port {}{}",
                output::strong(&a.app_name),
                output::brand_muted(&hosts),
                a.upstream_port,
                pid_str,
            );
        }
    }

    // Local DNS checks (macOS only)
    #[cfg(target_os = "macos")]
    {
        use super::dev::{TAKO_RESOLVER_FILE, local_dns_resolver_values, system_resolver_ipv4};

        eprintln!();
        eprintln!("{}", output::strong("Local DNS"));

        match local_dns_resolver_values() {
            Some((nameserver, port)) if nameserver == "127.0.0.1" && port == local_dns_port => {
                row(
                    "Resolver",
                    &format!(
                        "{} {} {}",
                        TAKO_RESOLVER_FILE,
                        output::brand_muted("→"),
                        format!("{nameserver}:{port}")
                    ),
                    LABEL_WIDTH,
                );
            }
            Some((nameserver, port)) => {
                row(
                    "Resolver",
                    &format!(
                        "{} {} {} {}",
                        TAKO_RESOLVER_FILE,
                        output::brand_muted("→"),
                        format!("{nameserver}:{port}"),
                        output::brand_warning(&format!("(expected 127.0.0.1:{local_dns_port})"))
                    ),
                    LABEL_WIDTH,
                );
            }
            None => {
                row(
                    "Resolver",
                    &format!(
                        "{} {} {}",
                        TAKO_RESOLVER_FILE,
                        output::brand_muted("→"),
                        output::brand_warning("missing")
                    ),
                    LABEL_WIDTH,
                );
            }
        }

        for a in &apps {
            let hosts = if a.hosts.is_empty() {
                vec![crate::dev::get_tako_domain(&a.app_name)]
            } else {
                a.hosts.clone()
            };
            for host in hosts.into_iter().filter(|h| h.ends_with(".tako.test")) {
                match system_resolver_ipv4(&host) {
                    Some(ip) if ip == "127.0.0.1" => {
                        row(
                            &host,
                            &format!("{} {}", output::brand_muted("→"), ip),
                            LABEL_WIDTH,
                        );
                    }
                    Some(ip) => {
                        row(
                            &host,
                            &format!(
                                "{} {} {}",
                                output::brand_muted("→"),
                                ip,
                                output::brand_warning("(expected 127.0.0.1)")
                            ),
                            LABEL_WIDTH,
                        );
                    }
                    None => {
                        row(
                            &host,
                            &format!("{} {}", output::brand_muted("→"), output::brand_warning("no answer")),
                            LABEL_WIDTH,
                        );
                    }
                }
            }
        }
    }
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
