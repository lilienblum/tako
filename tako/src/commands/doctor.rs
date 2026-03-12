use crate::output;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    print_paths();
    print_certificate();
    print_dev_server().await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn print_paths() {
    println!("{}", output::strong("Paths"));

    let config_dir = crate::paths::tako_config_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(error)".into());
    let data_dir = crate::paths::tako_data_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(error)".into());

    let labels = [("Config", &config_dir), ("Data", &data_dir)];
    let max_label = labels.iter().map(|(l, _)| l.len()).max().unwrap_or(0);

    for (label, value) in &labels {
        println!(
            "{:<width$}  {}",
            output::brand_muted(label),
            value,
            width = max_label,
        );
    }
}

// ---------------------------------------------------------------------------
// Certificate
// ---------------------------------------------------------------------------

fn print_certificate() {
    println!();
    println!("{}", output::strong("Local CA"));

    let store = match crate::dev::LocalCAStore::new() {
        Ok(s) => s,
        Err(e) => {
            println!(
                "{:<width$}  {}",
                output::brand_muted("Status"),
                output::brand_error(format!("error: {e}")),
                width = 6,
            );
            return;
        }
    };

    let exists = store.ca_exists();
    let trusted = if exists { store.is_ca_trusted() } else { false };

    let (status_text, status_color) = if !exists {
        ("not created", output::brand_warning("not created"))
    } else if trusted {
        ("trusted", output::brand_success("trusted"))
    } else {
        ("untrusted", output::brand_warning("untrusted"))
    };
    let _ = status_text;

    println!(
        "{:<width$}  {}",
        output::brand_muted("Status"),
        status_color,
        width = 6,
    );
}

// ---------------------------------------------------------------------------
// Dev server (adapted from commands::dev::doctor)
// ---------------------------------------------------------------------------

async fn print_dev_server() {
    use super::dev::{
        LOCAL_DNS_PORT, doctor_dev_server_lines, is_dev_server_unavailable_error_message,
    };

    println!();
    println!("{}", output::strong("Development server"));

    let info = match crate::dev_server_client::info().await {
        Ok(info) => info,
        Err(e) => {
            let message = e.to_string();
            if is_dev_server_unavailable_error_message(&message) {
                println!(
                    "{:<width$}  {}",
                    output::brand_muted("Status"),
                    output::brand_warning("not running"),
                    width = 6,
                );
            } else {
                println!(
                    "{:<width$}  {}",
                    output::brand_muted("Status"),
                    output::brand_error(format!("error: {e}")),
                    width = 6,
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
    let (pf_active, https_tcp_ok, http_tcp_ok, local_443_forwarding, local_80_forwarding) = {
        use super::dev::{pf_rules_active, tcp_port_open};
        let pf_active = pf_rules_active();
        let https_tcp_ok = tcp_port_open(advertised_ip, 443, 150);
        let http_tcp_ok = tcp_port_open(advertised_ip, 80, 150);
        (
            pf_active,
            https_tcp_ok,
            http_tcp_ok,
            https_tcp_ok,
            http_tcp_ok,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let (local_443_forwarding, local_80_forwarding) = (false, false);

    for line in doctor_dev_server_lines(
        listen,
        port,
        local_443_forwarding,
        local_80_forwarding,
        local_dns_enabled,
        local_dns_port,
    ) {
        println!("  {line}");
    }

    #[cfg(target_os = "macos")]
    {
        use super::dev::doctor_local_forwarding_preflight_lines;
        println!();
        for line in doctor_local_forwarding_preflight_lines(
            advertised_ip,
            pf_active,
            https_tcp_ok,
            http_tcp_ok,
        ) {
            println!("  {line}");
        }
    }

    let apps = crate::dev_server_client::list_apps()
        .await
        .unwrap_or_default();
    if !apps.is_empty() {
        println!();
        println!("  apps:");
        for a in &apps {
            let hosts = if a.hosts.is_empty() {
                "(default)".to_string()
            } else {
                a.hosts.join(",")
            };
            if let Some(pid) = a.pid {
                println!(
                    "  - {}  hosts={}  port={}  pid={}",
                    a.app_name, hosts, a.upstream_port, pid
                );
            } else {
                println!(
                    "  - {}  hosts={}  port={}",
                    a.app_name, hosts, a.upstream_port
                );
            }
        }
    }

    // Best-effort local DNS checks (macOS only).
    #[cfg(target_os = "macos")]
    {
        use super::dev::{TAKO_RESOLVER_FILE, local_dns_resolver_values, system_resolver_ipv4};

        println!();
        println!("  local-dns:");

        match local_dns_resolver_values() {
            Some((nameserver, port)) if nameserver == "127.0.0.1" && port == local_dns_port => {
                println!(
                    "  - resolver {} -> nameserver {} port {} (ok)",
                    TAKO_RESOLVER_FILE, nameserver, port
                );
            }
            Some((nameserver, port)) => {
                println!(
                    "  - resolver {} -> nameserver {} port {} (conflict; expected 127.0.0.1:{})",
                    TAKO_RESOLVER_FILE, nameserver, port, local_dns_port
                );
            }
            None => {
                println!("  - resolver {} -> (missing)", TAKO_RESOLVER_FILE);
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
                        println!("  - {} -> {} (ok)", host, ip);
                    }
                    Some(ip) => {
                        println!("  - {} -> {} (conflict; expected 127.0.0.1)", host, ip);
                    }
                    None => {
                        println!("  - {} -> (no answer)", host);
                    }
                }
            }
        }
    }
}
