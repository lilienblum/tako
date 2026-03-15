mod cli;
mod commands;
mod dev_server_client;
mod output;
mod paths;
pub mod shell;

// Internal modules (moved from tako-core)
pub mod app;
pub mod build;
pub mod config;
pub mod crypto;
pub mod dev;
pub mod ssh;
pub mod validation;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use cli::Cli;

fn main() {
    // Parse CLI arguments early so we can configure logging/output.
    let cli = Cli::parse();

    crate::output::set_verbose(cli.verbose);
    crate::output::set_ci(cli.ci);

    // Tracing subscriber: only installed in verbose/CI mode.
    // In normal mode, tracing calls are no-ops (no subscriber).
    if cli.verbose || cli.ci {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_writer(std::io::stderr)
            .event_format(output::ScopeFormat);

        tracing_subscriber::registry()
            .with(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("tako=trace,warn")),
            )
            .with(output::ScopeLayer)
            .with(fmt_layer)
            .init();
    }

    // Run the command
    if let Err(e) = cli.run() {
        // Ctrl+C / ESC — exit silently
        if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
            if io_err.kind() == std::io::ErrorKind::Interrupted {
                std::process::exit(130);
            }
        }
        crate::output::error_stderr(&e.to_string());
        std::process::exit(1);
    }
}
