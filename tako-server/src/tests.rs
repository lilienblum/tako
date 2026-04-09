use super::boot::{
    install_rustls_crypto_provider, read_server_config, should_signal_parent_on_ready,
};
use super::release::{
    resolve_release_runtime, should_use_self_signed_route_cert, validate_app_name,
    validate_deploy_routes,
};
use super::{
    SIGNAL_PARENT_ON_READY_ENV, ServerRuntimeConfig, ServerState, extract_zstd_archive,
    run_extract_archive_mode,
};
use crate::instances::AppConfig;
use crate::runtime_events::handle_idle_event;
use crate::socket::{AppState, Command, InstanceState, Response};
use crate::tls::{CertManager, CertManagerConfig, ChallengeTokens};
use clap::Parser;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tako_core::UpgradeMode;
use tempfile::TempDir;

fn empty_challenge_tokens() -> ChallengeTokens {
    Arc::new(parking_lot::RwLock::new(HashMap::new()))
}

fn write_release_manifest(
    release_dir: &Path,
    runtime: &str,
    main: &str,
    start: &[&str],
    install: Option<&str>,
    idle_timeout: u32,
) {
    let mut manifest = serde_json::json!({
        "runtime": runtime,
        "main": main,
        "idle_timeout": idle_timeout,
    });
    if !start.is_empty() {
        manifest["start"] =
            serde_json::Value::Array(start.iter().map(|value| (*value).into()).collect());
    }
    if let Some(install) = install {
        manifest["install"] = install.into();
    }
    std::fs::write(
        release_dir.join("app.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn default_server_log_filter_is_warn() {
    assert_eq!(super::DEFAULT_SERVER_LOG_FILTER, "warn");
}

#[test]
fn extract_zstd_archive_unpacks_files() {
    let temp = TempDir::new().unwrap();
    let archive_path = temp.path().join("payload.tar.zst");
    let dest = temp.path().join("dest");

    let file = std::fs::File::create(&archive_path).unwrap();
    let encoder = zstd::stream::write::Encoder::new(file, 3).unwrap();
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    let payload = b"hello";
    header.set_size(payload.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "app/index.txt", &mut Cursor::new(payload))
        .unwrap();
    let encoder = archive.into_inner().unwrap();
    encoder.finish().unwrap();

    extract_zstd_archive(&archive_path, &dest).unwrap();
    assert_eq!(
        std::fs::read_to_string(dest.join("app/index.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn extract_zstd_archive_rejects_path_traversal() {
    let temp = TempDir::new().unwrap();
    let archive_path = temp.path().join("malicious.tar.zst");
    let dest = temp.path().join("dest");

    // Build a tar with a `../escape.txt` entry by writing raw header bytes,
    // bypassing the builder's own path validation.
    let file = std::fs::File::create(&archive_path).unwrap();
    let mut encoder = zstd::stream::write::Encoder::new(file, 3).unwrap();
    {
        use std::io::Write;
        let mut header = tar::Header::new_gnu();
        let payload = b"pwned";
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        // Write path directly into the header name field
        let path = b"../escape.txt";
        let bytes = header.as_mut_bytes();
        bytes[..path.len()].copy_from_slice(path);
        header.set_cksum();

        encoder.write_all(header.as_bytes()).unwrap();
        encoder.write_all(payload).unwrap();
        // Pad to 512-byte boundary
        let padding = 512 - (payload.len() % 512);
        if padding < 512 {
            encoder.write_all(&vec![0u8; padding]).unwrap();
        }
        // Two zero blocks to end archive
        encoder.write_all(&[0u8; 1024]).unwrap();
    }
    encoder.finish().unwrap();

    // tar crate silently skips entries with `..` (returns Ok)
    extract_zstd_archive(&archive_path, &dest).unwrap();
    assert!(
        !temp.path().join("escape.txt").exists(),
        "path traversal: file escaped dest"
    );
    assert!(
        !dest.join("escape.txt").exists(),
        "path traversal: file should be skipped entirely"
    );
}

#[test]
fn run_extract_archive_mode_requires_destination_flag() {
    let args = super::Args::try_parse_from([
        "tako-server",
        "--extract-zstd-archive",
        "/tmp/payload.tar.zst",
    ])
    .unwrap();
    let err = run_extract_archive_mode(&args).unwrap_err();
    assert!(err.contains("--extract-dest"));
}

#[test]
fn install_rustls_crypto_provider_is_idempotent() {
    install_rustls_crypto_provider();
    assert!(rustls::crypto::CryptoProvider::get_default().is_some());

    install_rustls_crypto_provider();
    assert!(rustls::crypto::CryptoProvider::get_default().is_some());
}

fn signal_parent_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn should_signal_parent_on_ready_defaults_to_false() {
    let _guard = signal_parent_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe {
        std::env::remove_var(SIGNAL_PARENT_ON_READY_ENV);
    }
    assert!(!should_signal_parent_on_ready());
}

#[test]
fn should_signal_parent_on_ready_reads_env_toggle() {
    let _guard = signal_parent_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    unsafe {
        std::env::set_var(SIGNAL_PARENT_ON_READY_ENV, "1");
    }
    assert!(should_signal_parent_on_ready());

    unsafe {
        std::env::set_var(SIGNAL_PARENT_ON_READY_ENV, "0");
    }
    assert!(!should_signal_parent_on_ready());

    unsafe {
        std::env::remove_var(SIGNAL_PARENT_ON_READY_ENV);
    }
}

#[test]
fn validate_deploy_routes_rejects_empty_routes() {
    let err = validate_deploy_routes(&[]).unwrap_err();
    assert!(err.contains("at least one route"));
}

#[test]
fn validate_deploy_routes_rejects_empty_route_entry() {
    let err = validate_deploy_routes(&["".to_string()]).unwrap_err();
    assert!(err.contains("non-empty"));
}

#[test]
fn validate_app_name_accepts_app_env_identifier() {
    assert!(validate_app_name("my-app/staging").is_ok());
}

#[tokio::test]
async fn deploy_rejects_invalid_app_name() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let response = state
        .handle_command(Command::Deploy {
            app: "../escape".to_string(),
            version: "v1".to_string(),
            path: temp.path().to_string_lossy().to_string(),
            routes: vec!["api.example.com".to_string()],
            secrets: Some(HashMap::new()),
        })
        .await;

    let Response::Error { message } = response else {
        panic!("expected invalid app name to be rejected");
    };
    assert!(message.contains("Invalid app name"), "got: {message}");
}

#[tokio::test]
async fn deploy_rejects_release_path_outside_managed_root() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let outside_release = temp.path().join("outside-release");
    std::fs::create_dir_all(&outside_release).unwrap();

    let response = state
        .handle_command(Command::Deploy {
            app: "demo-app".to_string(),
            version: "v1".to_string(),
            path: outside_release.to_string_lossy().to_string(),
            routes: vec!["api.example.com".to_string()],
            secrets: Some(HashMap::new()),
        })
        .await;

    let Response::Error { message } = response else {
        panic!("expected out-of-root deploy path to be rejected");
    };
    assert!(
        message.contains("Invalid release path"),
        "expected path validation error, got: {message}"
    );
}

#[tokio::test]
async fn deploy_rejects_invalid_release_version() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let release_dir = temp
        .path()
        .join("apps")
        .join("demo-app")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&release_dir).unwrap();

    let response = state
        .handle_command(Command::Deploy {
            app: "demo-app".to_string(),
            version: "../v1".to_string(),
            path: release_dir.to_string_lossy().to_string(),
            routes: vec!["api.example.com".to_string()],
            secrets: Some(HashMap::new()),
        })
        .await;

    let Response::Error { message } = response else {
        panic!("expected invalid release version to be rejected");
    };
    assert!(
        message.contains("Invalid release version"),
        "got: {message}"
    );
}

#[test]
fn private_route_domains_prefer_self_signed_certs() {
    assert!(should_use_self_signed_route_cert(
        "tako-bun-server.orb.local"
    ));
    assert!(should_use_self_signed_route_cert("localhost"));
    assert!(should_use_self_signed_route_cert("api.localhost"));
    assert!(should_use_self_signed_route_cert("my-service"));
}

#[test]
fn public_route_domains_do_not_prefer_self_signed_certs() {
    assert!(!should_use_self_signed_route_cert("api.example.com"));
    assert!(!should_use_self_signed_route_cert("example.com"));
}

#[test]
fn resolve_release_runtime_requires_manifest() {
    let temp = TempDir::new().unwrap();
    let err = resolve_release_runtime(temp.path()).unwrap_err();
    assert!(err.contains("failed to read deploy manifest"));
}

#[test]
fn resolve_release_runtime_reads_manifest_runtime() {
    let temp = TempDir::new().unwrap();
    write_release_manifest(temp.path(), "bun", "index.ts", &[], None, 300);
    assert_eq!(
        resolve_release_runtime(temp.path()).unwrap(),
        "bun".to_string()
    );
}

#[test]
fn bun_runtime_has_install_script() {
    let runtime = tako_runtime::runtime_def_for("bun", None).unwrap();
    let install = runtime.package_manager.install.as_deref().unwrap();
    assert!(install.contains("bun install --production"));
}

#[test]
fn node_runtime_uses_npm_install_script() {
    let runtime = tako_runtime::runtime_def_for("node", None).unwrap();
    let install = runtime.package_manager.install.as_deref().unwrap();
    assert!(install.contains("npm"));
}

#[test]
fn deno_runtime_embeds_deno_package_manager() {
    let runtime = tako_runtime::runtime_def_for("deno", None).unwrap();
    assert_eq!(runtime.package_manager.id, "deno");
    assert!(
        runtime
            .package_manager
            .lockfiles
            .contains(&"deno.lock".to_string())
    );
}

// Install flow tests are covered by e2e tests (e2e/fixtures/javascript/*).

fn python3_ok() -> bool {
    StdCommand::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn python3_can_bind_loopback_tcp() -> bool {
    let Some(port) = pick_free_port() else {
        return false;
    };
    StdCommand::new("python3")
        .args([
            "-c",
            "import socket, sys; s = socket.socket(); s.bind(('127.0.0.1', int(sys.argv[1]))); s.close()",
        ])
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn pick_free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok().map(|a| a.port()))
}

#[tokio::test]
async fn ensure_route_certificate_generates_self_signed_for_private_domain() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    cert_manager.init().unwrap();
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager.clone(),
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let cert = state
        .ensure_route_certificate("my-app", "tako-bun-server.orb.local")
        .await
        .expect("private domain should get a generated cert");
    assert!(cert.is_self_signed);
    assert_eq!(cert.domain, "tako-bun-server.orb.local");

    let cached = cert_manager
        .get_cert_for_host("tako-bun-server.orb.local")
        .expect("generated cert should be cached");
    assert!(cached.is_self_signed);
}

#[tokio::test]
async fn delete_command_removes_runtime_registration_and_routes() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let release_dir = temp
        .path()
        .join("apps")
        .join("my-app")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&release_dir).unwrap();

    let config = AppConfig {
        name: "my-app".to_string(),
        version: "v1".to_string(),
        path: release_dir.clone(),
        command: vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            "exit 0".to_string(),
        ],
        min_instances: 0,
        ..Default::default()
    };

    let app = state.app_manager.register_app(config);
    state.load_balancer.register_app(app);
    {
        let mut route_table = state.routes.write().await;
        route_table.set_app_routes("my-app".to_string(), vec!["api.example.com".to_string()]);
    }

    let response = state
        .handle_command(Command::Delete {
            app: "my-app".to_string(),
        })
        .await;
    assert!(matches!(response, Response::Ok { .. }));
    assert!(state.app_manager.get_app("my-app").is_none());

    let route_table = state.routes.read().await;
    assert!(route_table.routes_for_app("my-app").is_empty());
    assert_eq!(route_table.select("api.example.com", "/"), None);
}

#[tokio::test]
async fn delete_command_is_idempotent_for_missing_app() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let response = state
        .handle_command(Command::Delete {
            app: "missing-app".to_string(),
        })
        .await;
    assert!(matches!(response, Response::Ok { .. }));
    assert!(state.app_manager.get_app("missing-app").is_none());
}

#[tokio::test]
async fn delete_command_rejects_invalid_app_name() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let response = state
        .handle_command(Command::Delete {
            app: "../bad".to_string(),
        })
        .await;

    let Response::Error { message } = response else {
        panic!("expected invalid app name to be rejected");
    };
    assert!(message.contains("Invalid app name"), "got: {message}");
}

#[tokio::test]
async fn upgrading_mode_blocks_mutating_commands() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    state.set_server_mode(UpgradeMode::Upgrading).await.unwrap();

    let response = state
        .handle_command(Command::Delete {
            app: "my-app".to_string(),
        })
        .await;

    let Response::Error { message } = response else {
        panic!("expected blocked mutating command while upgrading");
    };
    assert!(message.contains("Server is upgrading"));
    assert!(message.contains("delete"));
}

#[tokio::test]
async fn server_mode_resets_upgrading_on_boot() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));

    let state_a = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager.clone(),
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    state_a
        .set_server_mode(UpgradeMode::Upgrading)
        .await
        .unwrap();
    // Simulate an upgrade lock left behind by a crashed CLI.
    assert!(state_a.try_enter_upgrading("crashed-cli").await.unwrap());
    drop(state_a);

    // On restart, stale Upgrading mode AND orphaned lock should be cleared.
    let state_b = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager.clone(),
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    assert_eq!(*state_b.server_mode.read().await, UpgradeMode::Normal);
    // A new owner should be able to acquire immediately (no 10-min stale wait).
    assert!(state_b.try_enter_upgrading("new-cli").await.unwrap());
}

#[tokio::test]
async fn upgrading_lock_allows_single_owner() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state_a = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager.clone(),
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    let state_b = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    assert!(state_a.try_enter_upgrading("controller-a").await.unwrap());
    assert!(!state_b.try_enter_upgrading("controller-b").await.unwrap());
    assert!(state_a.exit_upgrading("controller-a").await.unwrap());
    assert!(state_b.try_enter_upgrading("controller-b").await.unwrap());
}

#[tokio::test]
async fn server_info_command_reports_runtime_config() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let runtime = ServerRuntimeConfig {
        pid: std::process::id(),
        socket: "/var/run/tako/tako-custom.sock".to_string(),
        data_dir: temp.path().to_path_buf(),
        http_port: 8080,
        https_port: 8443,
        no_acme: true,
        acme_staging: false,
        renewal_interval_hours: 24,
        dns_provider: None,
        standby: false,
        metrics_port: Some(9898),
        server_name: Some("test-server".to_string()),
    };
    let state = ServerState::new_with_runtime(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
        runtime,
    )
    .unwrap();
    state
        .set_server_mode(UpgradeMode::Upgrading)
        .await
        .expect("mode set");

    let response = state.handle_command(Command::ServerInfo).await;
    let Response::Ok { data } = response else {
        panic!("expected server info response");
    };
    assert_eq!(
        data.get("pid").and_then(Value::as_u64),
        Some(std::process::id() as u64)
    );
    assert_eq!(data.get("mode").and_then(Value::as_str), Some("upgrading"));
    assert_eq!(
        data.get("socket").and_then(Value::as_str),
        Some("/var/run/tako/tako-custom.sock")
    );
    assert_eq!(data.get("http_port").and_then(Value::as_u64), Some(8080));
    assert_eq!(data.get("https_port").and_then(Value::as_u64), Some(8443));
    assert_eq!(data.get("no_acme").and_then(Value::as_bool), Some(true));
}

#[tokio::test]
async fn enter_and_exit_upgrading_commands_use_owner_lock() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let enter = state
        .handle_command(Command::EnterUpgrading {
            owner: "controller-a".to_string(),
        })
        .await;
    assert!(matches!(enter, Response::Ok { .. }));

    let reject = state
        .handle_command(Command::EnterUpgrading {
            owner: "controller-b".to_string(),
        })
        .await;
    let Response::Error { message } = reject else {
        panic!("expected lock owner rejection");
    };
    assert!(message.contains("already upgrading"));
    assert!(message.contains("controller-a"));

    let wrong_exit = state
        .handle_command(Command::ExitUpgrading {
            owner: "controller-b".to_string(),
        })
        .await;
    assert!(matches!(wrong_exit, Response::Error { .. }));

    let exit = state
        .handle_command(Command::ExitUpgrading {
            owner: "controller-a".to_string(),
        })
        .await;
    assert!(matches!(exit, Response::Ok { .. }));
}

#[tokio::test]
async fn get_secrets_hash_returns_hash_of_app_secrets() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    // No secrets file → hash of empty map
    let response = state
        .handle_command(Command::GetSecretsHash {
            app: "my-app".to_string(),
        })
        .await;
    let Response::Ok { data } = &response else {
        panic!("expected ok response: {response:?}");
    };
    let empty_hash = data.get("hash").and_then(Value::as_str).unwrap();
    assert_eq!(empty_hash, tako_core::compute_secrets_hash(&HashMap::new()));

    // Store secrets and check hash changes
    let secrets: HashMap<String, String> = [("KEY".to_string(), "val".to_string())]
        .into_iter()
        .collect();
    state.state_store.set_secrets("my-app", &secrets).unwrap();

    let response = state
        .handle_command(Command::GetSecretsHash {
            app: "my-app".to_string(),
        })
        .await;
    let Response::Ok { data } = &response else {
        panic!("expected ok response");
    };
    let with_secrets_hash = data.get("hash").and_then(Value::as_str).unwrap();
    assert_ne!(with_secrets_hash, empty_hash);
    assert_eq!(with_secrets_hash, tako_core::compute_secrets_hash(&secrets));
}

#[tokio::test]
async fn deploy_without_secrets_keeps_existing() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    // Pre-store secrets for the app
    let secrets: HashMap<String, String> = [("API_KEY".to_string(), "original".to_string())]
        .into_iter()
        .collect();
    state.state_store.set_secrets("keep-app", &secrets).unwrap();

    let release_dir = temp
        .path()
        .join("apps")
        .join("keep-app")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&release_dir).unwrap();
    write_release_manifest(
        &release_dir,
        "node",
        "index.js",
        &["/bin/sh", "-lc", "sleep 600"],
        Some("true"),
        300,
    );

    // Deploy with secrets: None — should keep existing
    let _response = state
        .handle_command(Command::Deploy {
            app: "keep-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.to_string_lossy().to_string(),
            routes: vec!["keep.localhost".to_string()],
            secrets: None,
        })
        .await;

    // Verify secrets still have original value
    let loaded = state.state_store.get_secrets("keep-app").unwrap();
    assert_eq!(loaded.get("API_KEY"), Some(&"original".to_string()));
}

#[tokio::test]
async fn restore_from_state_store_rehydrates_apps_routes_and_secrets() {
    let temp = TempDir::new().unwrap();
    let app_id = "my-app/production";
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));

    let state_a = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager.clone(),
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    let release_dir = temp
        .path()
        .join("apps")
        .join("my-app")
        .join("production")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&release_dir).unwrap();
    write_release_manifest(
        &release_dir,
        "node",
        "index.js",
        &["/bin/sh", "-lc", "sleep 600"],
        Some("true"),
        300,
    );

    let app_secrets: HashMap<String, String> =
        [("DATABASE_URL".to_string(), "postgres://db".to_string())]
            .into_iter()
            .collect();
    state_a
        .state_store
        .set_secrets(app_id, &app_secrets)
        .unwrap();

    let app = state_a.app_manager.register_app(AppConfig {
        name: "my-app".to_string(),
        environment: "production".to_string(),
        version: "v1".to_string(),
        path: release_dir.clone(),
        command: vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            "sleep 600".to_string(),
        ],
        min_instances: 0,
        max_instances: 4,
        idle_timeout: Duration::from_secs(300),
        ..Default::default()
    });
    state_a.load_balancer.register_app(app);
    {
        let mut route_table = state_a.routes.write().await;
        route_table.set_app_routes(
            app_id.to_string(),
            vec![
                "api.example.com".to_string(),
                "example.com/api/*".to_string(),
            ],
        );
    }
    state_a.persist_app_state(app_id).await;
    drop(state_a);

    let state_b = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    state_b.restore_from_state_store().await.unwrap();

    let restored = state_b.app_manager.get_app(app_id).expect("app restored");
    assert_eq!(restored.version(), "v1");
    assert_eq!(restored.state(), crate::socket::AppState::Idle);
    let route_table = state_b.routes.read().await;
    assert_eq!(
        route_table.routes_for_app(app_id),
        vec![
            "api.example.com".to_string(),
            "example.com/api/*".to_string()
        ]
    );
    let restored_secrets = restored.config.read().secrets.clone();
    assert_eq!(
        restored_secrets.get("DATABASE_URL"),
        Some(&"postgres://db".to_string())
    );
}

#[tokio::test]
async fn scale_command_persists_zero_instances_across_restore() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));

    let state_a = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager.clone(),
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    let release_dir = temp
        .path()
        .join("apps")
        .join("my-app")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&release_dir).unwrap();
    std::fs::write(
        release_dir.join("app.json"),
        r#"{"runtime":"node","main":"index.js","idle_timeout":300,"start":["/bin/sh","-lc","sleep 600"]}"#,
    )
    .unwrap();

    let app = state_a.app_manager.register_app(AppConfig {
        name: "my-app".to_string(),
        version: "v1".to_string(),
        path: release_dir.clone(),
        command: vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            "sleep 600".to_string(),
        ],
        min_instances: 2,
        max_instances: 4,
        idle_timeout: Duration::from_secs(300),
        ..Default::default()
    });
    state_a.load_balancer.register_app(app.clone());
    {
        let mut route_table = state_a.routes.write().await;
        route_table.set_app_routes("my-app".to_string(), vec!["api.example.com".to_string()]);
    }

    let first = app.allocate_instance();
    first.set_state(InstanceState::Healthy);
    let second = app.allocate_instance();
    second.set_state(InstanceState::Healthy);

    let response = state_a
        .handle_command(Command::Scale {
            app: "my-app".to_string(),
            instances: 0,
        })
        .await;
    assert!(matches!(response, Response::Ok { .. }));
    assert_eq!(app.config.read().min_instances, 0);
    assert!(app.get_instances().is_empty());

    drop(state_a);

    let state_b = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    state_b.restore_from_state_store().await.unwrap();

    let restored = state_b.app_manager.get_app("my-app").expect("app restored");
    assert_eq!(restored.config.read().min_instances, 0);
    assert_eq!(restored.state(), AppState::Idle);
}

#[tokio::test]
async fn deploy_preserves_scaled_instance_count() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let current_release = temp
        .path()
        .join("apps")
        .join("my-app")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&current_release).unwrap();
    std::fs::write(
        current_release.join("app.json"),
        r#"{"runtime":"node","main":"index.js","idle_timeout":300,"start":["/bin/sh","-lc","sleep 600"]}"#,
    )
    .unwrap();

    let app = state.app_manager.register_app(AppConfig {
        name: "my-app".to_string(),
        version: "v1".to_string(),
        path: current_release.clone(),
        command: vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            "sleep 600".to_string(),
        ],
        min_instances: 2,
        max_instances: 4,
        idle_timeout: Duration::from_secs(300),
        ..Default::default()
    });
    state.load_balancer.register_app(app.clone());
    {
        let mut route_table = state.routes.write().await;
        route_table.set_app_routes("my-app".to_string(), vec!["api.example.com".to_string()]);
    }

    let old_instance = app.allocate_instance();
    old_instance.set_state(InstanceState::Healthy);

    let broken_release = temp
        .path()
        .join("apps")
        .join("my-app")
        .join("releases")
        .join("v2");
    std::fs::create_dir_all(&broken_release).unwrap();
    std::fs::write(
        broken_release.join("app.json"),
        r#"{"runtime":"node","main":"index.js","idle_timeout":300,"start":["/bin/sh","-lc","exit 1"]}"#,
    )
    .unwrap();

    let response = state
        .handle_command(Command::Deploy {
            app: "my-app".to_string(),
            version: "v2".to_string(),
            path: broken_release.to_string_lossy().to_string(),
            routes: vec!["api.example.com".to_string()],
            secrets: Some(HashMap::new()),
        })
        .await;

    assert!(matches!(response, Response::Error { .. }));
    assert_eq!(app.config.read().min_instances, 2);
}

#[tokio::test]
async fn delete_command_removes_persisted_state_for_next_boot() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state_a = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager.clone(),
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let release_dir = temp
        .path()
        .join("apps")
        .join("my-app")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&release_dir).unwrap();
    write_release_manifest(
        &release_dir,
        "node",
        "index.js",
        &["/bin/sh", "-lc", "sleep 600"],
        Some("true"),
        300,
    );
    let app = state_a.app_manager.register_app(AppConfig {
        name: "my-app".to_string(),
        version: "v1".to_string(),
        path: release_dir.clone(),
        command: vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            "sleep 600".to_string(),
        ],
        min_instances: 0,
        ..Default::default()
    });
    state_a.load_balancer.register_app(app);
    {
        let mut route_table = state_a.routes.write().await;
        route_table.set_app_routes(
            "my-app/production".to_string(),
            vec!["api.example.com".to_string()],
        );
    }
    state_a.persist_app_state("my-app/production").await;

    let response = state_a
        .handle_command(Command::Delete {
            app: "my-app/production".to_string(),
        })
        .await;
    assert!(matches!(response, Response::Ok { .. }));

    let state_b = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();
    state_b.restore_from_state_store().await.unwrap();
    assert!(state_b.app_manager.get_app("my-app/production").is_none());
}

#[tokio::test]
async fn deploy_on_demand_validates_startup_and_fails_for_unhealthy_build() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let release_dir = temp
        .path()
        .join("apps")
        .join("broken-app")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&release_dir).unwrap();
    std::fs::write(
        release_dir.join("app.json"),
        r#"{"runtime":"node","main":"index.js","idle_timeout":300,"install":"true","start":["/bin/sh","-lc","exit 1"]}"#,
    )
    .unwrap();

    let response = state
        .handle_command(Command::Deploy {
            app: "broken-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.to_string_lossy().to_string(),
            routes: vec!["broken.localhost".to_string()],
            secrets: Some(HashMap::new()),
        })
        .await;

    assert!(
        matches!(response, Response::Error { .. }),
        "expected startup validation failure for on-demand deploy: {response:?}"
    );
}

// TODO: This test needs a rewrite to work with the plugin-derived launch
// command. The fake bun script exits immediately because the spawner's
// binary resolution doesn't find the fake bun via the manifest's PATH.
// The deploy lifecycle is fully covered by e2e tests (e2e/fixtures/).
#[tokio::test]
#[ignore = "needs rewrite for plugin architecture"]
async fn deploy_on_demand_keeps_one_warm_instance_after_successful_deploy() {
    if !python3_ok() || !python3_can_bind_loopback_tcp() {
        return;
    }

    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let runtime = ServerRuntimeConfig {
        socket: "/tmp/tako-warm.sock".to_string(),
        ..ServerRuntimeConfig::for_defaults(temp.path().to_path_buf())
    };
    let state = ServerState::new_with_runtime(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
        runtime,
    )
    .unwrap();

    let fake_bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    let fake_bun = fake_bin_dir.join("bun");
    let fake_server_py = temp.path().join("server.py");
    std::fs::write(
        &fake_server_py,
        r#"import os
from http.server import BaseHTTPRequestHandler, HTTPServer

port = int(os.environ.get("PORT") or "0")
internal_token = os.environ.get("TAKO_INTERNAL_TOKEN") or ""
if not port or not internal_token:
raise SystemExit("PORT and TAKO_INTERNAL_TOKEN are required")

class Handler(BaseHTTPRequestHandler):
def do_GET(self):
    if self.path == "/status" and (self.headers.get("Host") or "").split(":")[0].lower() == "tako":
        if self.headers.get("X-Tako-Internal-Token") != internal_token:
            self.send_response(403)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"error":"forbidden"}')
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("X-Tako-Internal-Token", internal_token)
        self.end_headers()
        self.wfile.write(b'{"status":"ok"}')
        return
    self.send_response(404)
    self.end_headers()

def log_message(self, format, *args):
    return

HTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#,
    )
    .unwrap();
    std::fs::write(
        &fake_bun,
        format!(
            "#!/bin/sh\ncase \"$1\" in install) exit 0;; esac\nexec python3 {}\n",
            fake_server_py.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&fake_bun).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_bun, permissions).unwrap();
    }

    let release_dir = temp
        .path()
        .join("apps")
        .join("warm-app")
        .join("releases")
        .join("v1");
    std::fs::create_dir_all(&release_dir).unwrap();
    std::fs::write(
        release_dir.join("package.json"),
        r#"{"name":"warm-app","scripts":{"dev":"bun run index.ts"}}"#,
    )
    .unwrap();
    std::fs::write(release_dir.join("index.ts"), "export default {};\n").unwrap();
    std::fs::create_dir_all(release_dir.join("node_modules/tako.sh/dist/entrypoints")).unwrap();
    std::fs::write(
        release_dir.join("node_modules/tako.sh/dist/entrypoints/bun.mjs"),
        "export default {};",
    )
    .unwrap();
    // Include PATH in the manifest env_vars so that the spawned instance
    // can find the fake bun binary.  Also set runtime_bin to the absolute
    // path so resolve_runtime_binary picks it up directly.
    let path_with_fake = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::fs::write(
        release_dir.join("app.json"),
        serde_json::json!({
            "runtime": "bun",
            "main": "index.ts",
            "idle_timeout": 300,
            "env_vars": { "PATH": &path_with_fake }
        })
        .to_string(),
    )
    .unwrap();

    let app = state.app_manager.register_app(AppConfig {
        name: "warm-app".to_string(),
        version: "v0".to_string(),
        path: release_dir.clone(),
        command: vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            "exit 0".to_string(),
        ],
        min_instances: 0,
        max_instances: 4,
        ..Default::default()
    });
    state.load_balancer.register_app(app);

    let response = state
        .handle_command(Command::Deploy {
            app: "warm-app".to_string(),
            version: "v1".to_string(),
            path: release_dir.to_string_lossy().to_string(),
            routes: vec!["warm.localhost".to_string()],
            secrets: Some(HashMap::new()),
        })
        .await;
    assert!(
        matches!(response, Response::Ok { .. }),
        "expected successful on-demand deploy: {response:?}"
    );

    let status = state
        .handle_command(Command::Status {
            app: "warm-app".to_string(),
        })
        .await;
    let Response::Ok { data } = status else {
        panic!("expected status response for warm-app");
    };

    assert_eq!(data.get("state").and_then(Value::as_str), Some("running"));
    let instances = data
        .get("instances")
        .and_then(Value::as_array)
        .expect("status should include instances");
    assert_eq!(instances.len(), 1);
}

#[tokio::test]
async fn instance_idle_event_resets_cold_start_when_app_scales_to_zero() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let app = state.app_manager.register_app(AppConfig {
        name: "idle-app".to_string(),
        version: "v1".to_string(),
        min_instances: 0,
        ..Default::default()
    });
    state.load_balancer.register_app(app.clone());
    app.set_state(AppState::Running);

    let instance = app.allocate_instance();
    instance.set_state(InstanceState::Healthy);

    // Simulate a prior successful cold start.
    state.cold_start.begin("idle-app");
    state.cold_start.mark_ready("idle-app");
    assert!(!state.cold_start.begin("idle-app").leader);

    handle_idle_event(
        &state,
        crate::scaling::IdleEvent::InstanceIdle {
            app: "idle-app".to_string(),
            instance_id: instance.id.clone(),
        },
    )
    .await;

    assert!(app.get_instances().is_empty());
    assert_eq!(app.state(), AppState::Idle);
    assert!(state.cold_start.begin("idle-app").leader);
}

#[tokio::test]
async fn status_includes_running_builds_for_each_version() {
    let temp = TempDir::new().unwrap();
    let cert_manager = Arc::new(CertManager::new(CertManagerConfig {
        cert_dir: temp.path().join("certs"),
        ..Default::default()
    }));
    let state = ServerState::new(
        temp.path().to_path_buf(),
        cert_manager,
        None,
        empty_challenge_tokens(),
    )
    .unwrap();

    let app = state.app_manager.register_app(AppConfig {
        name: "my-app".to_string(),
        version: "v1".to_string(),
        min_instances: 0,
        ..Default::default()
    });

    let old = app.allocate_instance();
    old.set_state(InstanceState::Healthy);

    let mut cfg = app.config.read().clone();
    cfg.version = "v2".to_string();
    app.update_config(cfg);

    let new = app.allocate_instance();
    new.set_state(InstanceState::Healthy);

    let response = state
        .handle_command(Command::Status {
            app: "my-app".to_string(),
        })
        .await;

    let Response::Ok { data } = response else {
        panic!("expected ok status response");
    };

    let builds = data
        .get("builds")
        .and_then(Value::as_array)
        .expect("status should include builds");
    let versions: Vec<&str> = builds
        .iter()
        .filter_map(|b| b.get("version").and_then(Value::as_str))
        .collect();
    assert!(
        versions.contains(&"v1") && versions.contains(&"v2"),
        "expected status to include both running builds: {data}"
    );
}

#[test]
fn read_server_config_from_json() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        r#"{"server_name":"prod","dns":{"provider":"cloudflare"}}"#,
    )
    .unwrap();
    let config = read_server_config(dir.path());
    assert_eq!(config.server_name.as_deref(), Some("prod"));
    assert_eq!(config.dns.as_ref().unwrap().provider, "cloudflare");
}

#[test]
fn read_server_config_returns_defaults_when_missing() {
    let dir = TempDir::new().unwrap();
    let config = read_server_config(dir.path());
    assert!(config.server_name.is_none());
    assert!(config.dns.is_none());
}
