mod cli;
mod commands;
mod dev_server_client;
mod output;
mod paths;

// Internal modules (moved from tako-core)
pub mod app;
pub mod build;
pub mod config;
pub mod crypto;
pub mod dev;
pub mod runtime;
pub mod ssh;
pub mod validation;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::Cli;

fn main() {
    // Parse CLI arguments early so we can configure logging/output.
    let cli = Cli::parse();

    crate::output::set_verbose(cli.verbose);

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            if cli.verbose {
                EnvFilter::new("info")
            } else {
                EnvFilter::new("warn")
            }
        }))
        .with_target(false)
        .init();

    // Run the command
    if let Err(e) = cli.run() {
        crate::output::error_stderr(&e.to_string());
        std::process::exit(1);
    }
}
