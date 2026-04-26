// This crate contains runtime components that are exercised indirectly in integration tests.
#![allow(dead_code)]

#[cfg(not(unix))]
compile_error!("tako-server requires Unix (management commands use Unix sockets).");

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod app_command;
mod boot;
mod channels;
mod channels_ws;
mod defaults;
mod instances;
mod lb;
mod metrics;
mod operations;
mod paths;
mod protocol;
mod proxy;
mod release;
mod release_command;
mod routing;
mod runtime_events;
mod scaling;
mod server_state;
mod socket;
mod startup;
mod state_store;
mod tls;
mod version_manager;

use tako_workflows as workflows;

use crate::boot::install_rustls_crypto_provider;
use clap::Parser;
use std::path::Path;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub(crate) use crate::release::is_private_local_hostname;
pub use server_state::{ServerRuntimeConfig, ServerState};

const DEFAULT_SERVER_LOG_FILTER: &str = "warn";
const SIGNAL_PARENT_ON_READY_ENV: &str = "TAKO_SIGNAL_PARENT_ON_READY";

fn server_version() -> &'static str {
    static VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let base = env!("CARGO_PKG_VERSION");
        match option_env!("TAKO_BUILD_SHA") {
            Some(sha) if !sha.trim().is_empty() => {
                let short = &sha.trim()[..sha.trim().len().min(7)];
                format!("{base}-{short}")
            }
            _ => base.to_string(),
        }
    });
    &VERSION
}

/// Tako Server - Application runtime and proxy
#[derive(Parser)]
#[command(name = "tako-server")]
#[command(version = server_version())]
#[command(about = "Tako Server - Application runtime and proxy")]
pub struct Args {
    /// Unix socket path for management commands
    #[arg(long)]
    pub socket: Option<String>,

    /// HTTP port
    #[arg(long, default_value_t = 80)]
    pub port: u16,

    /// HTTPS port
    #[arg(long, default_value_t = 443)]
    pub tls_port: u16,

    /// Use Let's Encrypt staging environment
    #[arg(long)]
    pub acme_staging: bool,

    /// Data directory for apps and certificates
    #[arg(long)]
    pub data_dir: Option<String>,

    /// Disable ACME (use self-signed or manual certificates only)
    #[arg(long)]
    pub no_acme: bool,

    /// Certificate renewal check interval in hours (default: 12)
    #[arg(long, default_value_t = 12)]
    pub renewal_interval_hours: u64,

    /// Run as a hot standby: serve traffic with minimal scaling (max 1 instance
    /// per app), skip management socket and ACME. Monitors the primary
    /// server's socket — promotes to full mode if primary is unavailable,
    /// shuts down gracefully when primary comes back.
    #[arg(long)]
    pub standby: bool,

    /// Prometheus metrics port (default: 9898, set to 0 to disable)
    #[arg(long, default_value_t = 9898)]
    pub metrics_port: u16,

    /// Extract a `.tar.zst` archive into a destination directory and exit.
    #[arg(long, hide = true)]
    pub extract_zstd_archive: Option<String>,

    /// Destination directory used with `--extract-zstd-archive`.
    #[arg(long, hide = true)]
    pub extract_dest: Option<String>,
}

fn extract_zstd_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("create extraction dir {}: {}", dest_dir.display(), e))?;
    let file = std::fs::File::open(archive_path)
        .map_err(|e| format!("open archive {}: {}", archive_path.display(), e))?;
    let decoder = zstd::stream::read::Decoder::new(file).map_err(|e| {
        format!(
            "initialize zstd decoder for {}: {}",
            archive_path.display(),
            e
        )
    })?;
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dest_dir).map_err(|e| {
        format!(
            "extract archive {} into {}: {}",
            archive_path.display(),
            dest_dir.display(),
            e
        )
    })?;
    Ok(())
}

fn run_extract_archive_mode(args: &Args) -> Result<(), String> {
    let archive = args
        .extract_zstd_archive
        .as_deref()
        .ok_or_else(|| "Extraction mode requires --extract-zstd-archive <path>".to_string())?;
    let dest = args
        .extract_dest
        .as_deref()
        .ok_or_else(|| "Extraction mode requires --extract-dest <dir>".to_string())?;
    extract_zstd_archive(Path::new(archive), Path::new(dest))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_rustls_crypto_provider();

    // Initialize tracing with a non-blocking writer so log I/O never stalls
    // Tokio worker threads (critical under high request volume / DDoS).
    let (non_blocking, _guard) = tracing_appender::non_blocking(std::io::stdout());
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_SERVER_LOG_FILTER)),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_target(false)
                .with_writer(non_blocking),
        )
        .init();

    let args = Args::parse();
    if args.extract_zstd_archive.is_some() || args.extract_dest.is_some() {
        run_extract_archive_mode(&args)?;
        return Ok(());
    }
    startup::run(args)
}

#[cfg(test)]
mod tests;
