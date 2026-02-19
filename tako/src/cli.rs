use clap::{Parser, Subcommand};

use crate::commands::{self, delete, releases, secret, server, upgrade};
use clap::CommandFactory;

const DEV_PUBLIC_PORT: u16 = 47831;

/// Tako - Modern application development, deployment, and runtime platform
#[derive(Parser)]
#[command(name = "tako")]
#[command(version, disable_version_flag = true)]
#[command(about = "Tako - Modern application development, deployment, and runtime platform")]
#[command(propagate_version = true)]
pub struct Cli {
    /// Show version
    #[arg(long, global = true)]
    pub version: bool,

    /// Show verbose output
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::secret::SecretKeyCommands;
    use clap::Parser;

    #[test]
    fn servers_add_defaults_to_tako_user() {
        let cli = Cli::try_parse_from(["tako", "servers", "add", "example.com"]).unwrap();
        let Commands::Servers(server::ServerCommands::Add { host, .. }) =
            cli.command.expect("command")
        else {
            panic!("expected Servers::Add");
        };
        assert_eq!(host.as_deref(), Some("example.com"));
    }

    #[test]
    fn servers_add_without_host_parses_for_wizard() {
        let cli = Cli::try_parse_from(["tako", "servers", "add"]).unwrap();
        let Commands::Servers(server::ServerCommands::Add { host, .. }) =
            cli.command.expect("command")
        else {
            panic!("expected Servers::Add");
        };
        assert!(host.is_none());
    }

    #[test]
    fn servers_add_parses_optional_description() {
        let cli = Cli::try_parse_from([
            "tako",
            "servers",
            "add",
            "example.com",
            "--description",
            "Edge node",
        ])
        .unwrap();
        let Commands::Servers(server::ServerCommands::Add { description, .. }) =
            cli.command.expect("command")
        else {
            panic!("expected Servers::Add");
        };
        assert_eq!(description.as_deref(), Some("Edge node"));
    }

    #[test]
    fn servers_add_rejects_user_flag() {
        let res = Cli::try_parse_from(["tako", "servers", "add", "example.com", "--user", "root"]);
        match res {
            Ok(_) => panic!("expected parse failure"),
            Err(err) => assert!(
                err.to_string().contains("unexpected argument '--user'"),
                "unexpected error: {err}"
            ),
        }
    }

    #[test]
    fn secrets_key_import_parses() {
        let cli = Cli::try_parse_from(["tako", "secrets", "key", "import"]).unwrap();

        let Some(Commands::Secrets(secret::SecretCommands::Key(SecretKeyCommands::Import { env }))) =
            cli.command
        else {
            panic!("expected Secrets::Key::Import");
        };

        assert_eq!(env, None);
    }

    #[test]
    fn secrets_key_import_parses_with_env() {
        let cli =
            Cli::try_parse_from(["tako", "secrets", "key", "import", "--env", "staging"]).unwrap();

        let Some(Commands::Secrets(secret::SecretCommands::Key(SecretKeyCommands::Import { env }))) =
            cli.command
        else {
            panic!("expected Secrets::Key::Import");
        };

        assert_eq!(env.as_deref(), Some("staging"));
    }

    #[test]
    fn secrets_key_export_parses_with_env() {
        let cli = Cli::try_parse_from(["tako", "secrets", "key", "export", "--env", "production"])
            .unwrap();

        let Some(Commands::Secrets(secret::SecretCommands::Key(SecretKeyCommands::Export { env }))) =
            cli.command
        else {
            panic!("expected Secrets::Key::Export");
        };

        assert_eq!(env.as_deref(), Some("production"));
    }

    #[test]
    fn servers_remove_aliases_parse() {
        let cli = Cli::try_parse_from(["tako", "servers", "remove", "prod"]).unwrap();
        let Commands::Servers(server::ServerCommands::Rm { name }) = cli.command.expect("command")
        else {
            panic!("expected Servers::Rm");
        };
        assert_eq!(name.as_deref(), Some("prod"));

        let cli = Cli::try_parse_from(["tako", "servers", "delete", "prod"]).unwrap();
        let Commands::Servers(server::ServerCommands::Rm { name }) = cli.command.expect("command")
        else {
            panic!("expected Servers::Rm");
        };
        assert_eq!(name.as_deref(), Some("prod"));
    }

    #[test]
    fn servers_rm_without_name_parses_for_selector() {
        let cli = Cli::try_parse_from(["tako", "servers", "rm"]).unwrap();
        let Commands::Servers(server::ServerCommands::Rm { name }) = cli.command.expect("command")
        else {
            panic!("expected Servers::Rm");
        };
        assert!(name.is_none());
    }

    #[test]
    fn servers_list_alias_parses() {
        let cli = Cli::try_parse_from(["tako", "servers", "list"]).unwrap();
        let Commands::Servers(server::ServerCommands::Ls) = cli.command.expect("command") else {
            panic!("expected Servers::Ls");
        };
    }

    #[test]
    fn servers_status_without_name_parses() {
        let cli = Cli::try_parse_from(["tako", "servers", "status"]).unwrap();
        let Commands::Servers(server::ServerCommands::Status) = cli.command.expect("command")
        else {
            panic!("expected Servers::Status");
        };
    }

    #[test]
    fn servers_status_with_name_is_rejected() {
        let res = Cli::try_parse_from(["tako", "servers", "status", "prod"]);
        match res {
            Ok(_) => panic!("expected parse failure"),
            Err(err) => assert!(
                err.to_string().contains("unexpected argument 'prod'"),
                "unexpected error: {err}"
            ),
        }
    }

    #[test]
    fn servers_upgrade_parses_with_name() {
        let cli = Cli::try_parse_from(["tako", "servers", "upgrade", "prod"]).unwrap();
        let Commands::Servers(server::ServerCommands::Upgrade { name }) =
            cli.command.expect("command")
        else {
            panic!("expected Servers::Upgrade");
        };
        assert_eq!(name, "prod");
    }

    #[test]
    fn top_level_status_command_is_not_available() {
        let res = Cli::try_parse_from(["tako", "status"]);
        match res {
            Ok(_) => panic!("expected parse failure"),
            Err(err) => assert!(
                err.to_string().contains("unrecognized subcommand 'status'"),
                "unexpected error: {err}"
            ),
        }
    }

    #[test]
    fn secrets_remove_aliases_parse() {
        let cli = Cli::try_parse_from(["tako", "secrets", "remove", "API_KEY"]).unwrap();
        let Some(Commands::Secrets(secret::SecretCommands::Rm { name, env })) = cli.command else {
            panic!("expected Secrets::Rm");
        };
        assert_eq!(name, "API_KEY");
        assert!(env.is_none());

        let cli = Cli::try_parse_from(["tako", "secrets", "delete", "API_KEY"]).unwrap();
        let Some(Commands::Secrets(secret::SecretCommands::Rm { name, env })) = cli.command else {
            panic!("expected Secrets::Rm");
        };
        assert_eq!(name, "API_KEY");
        assert!(env.is_none());
    }

    #[test]
    fn secrets_list_alias_parses() {
        let cli = Cli::try_parse_from(["tako", "secrets", "list"]).unwrap();
        let Some(Commands::Secrets(secret::SecretCommands::Ls)) = cli.command else {
            panic!("expected Secrets::Ls");
        };
    }

    #[test]
    fn deploy_without_env_parses_env_as_none() {
        let cli = Cli::try_parse_from(["tako", "deploy"]).unwrap();
        let Some(Commands::Deploy { env, yes, .. }) = cli.command else {
            panic!("expected Deploy");
        };
        assert!(env.is_none());
        assert!(!yes);
    }

    #[test]
    fn deploy_with_env_parses_env_value() {
        let cli = Cli::try_parse_from(["tako", "deploy", "--env", "staging"]).unwrap();
        let Some(Commands::Deploy { env, .. }) = cli.command else {
            panic!("expected Deploy");
        };
        assert_eq!(env.as_deref(), Some("staging"));
    }

    #[test]
    fn deploy_parses_yes_flag() {
        let cli = Cli::try_parse_from(["tako", "deploy", "--yes"]).unwrap();
        let Some(Commands::Deploy { yes, .. }) = cli.command else {
            panic!("expected Deploy");
        };
        assert!(yes);
    }

    #[test]
    fn deploy_parses_yes_short_flag() {
        let cli = Cli::try_parse_from(["tako", "deploy", "-y"]).unwrap();
        let Some(Commands::Deploy { yes, .. }) = cli.command else {
            panic!("expected Deploy");
        };
        assert!(yes);
    }

    #[test]
    fn releases_list_parses() {
        let cli = Cli::try_parse_from(["tako", "releases", "ls"]).unwrap();
        let Some(Commands::Releases(releases::ReleaseCommands::Ls { env })) = cli.command else {
            panic!("expected Releases::Ls");
        };
        assert!(env.is_none());
    }

    #[test]
    fn releases_list_parses_with_env() {
        let cli = Cli::try_parse_from(["tako", "releases", "ls", "--env", "staging"]).unwrap();
        let Some(Commands::Releases(releases::ReleaseCommands::Ls { env })) = cli.command else {
            panic!("expected Releases::Ls");
        };
        assert_eq!(env.as_deref(), Some("staging"));
    }

    #[test]
    fn releases_rollback_parses_release_id_and_yes_flag() {
        let cli =
            Cli::try_parse_from(["tako", "releases", "rollback", "abc1234", "--yes"]).unwrap();
        let Some(Commands::Releases(releases::ReleaseCommands::Rollback { release, env, yes })) =
            cli.command
        else {
            panic!("expected Releases::Rollback");
        };
        assert_eq!(release, "abc1234");
        assert!(env.is_none());
        assert!(yes);
    }

    #[test]
    fn delete_without_env_parses_env_as_none() {
        let cli = Cli::try_parse_from(["tako", "delete"]).unwrap();
        let Some(Commands::Delete { env, yes, .. }) = cli.command else {
            panic!("expected Delete");
        };
        assert!(env.is_none());
        assert!(!yes);
    }

    #[test]
    fn delete_aliases_parse() {
        let cli = Cli::try_parse_from(["tako", "rm", "--env", "staging"]).unwrap();
        let Some(Commands::Delete { env, .. }) = cli.command else {
            panic!("expected Delete");
        };
        assert_eq!(env.as_deref(), Some("staging"));

        let cli = Cli::try_parse_from(["tako", "remove", "--env", "staging"]).unwrap();
        let Some(Commands::Delete { env, .. }) = cli.command else {
            panic!("expected Delete");
        };
        assert_eq!(env.as_deref(), Some("staging"));
    }

    #[test]
    fn upgrade_command_parses() {
        let cli = Cli::try_parse_from(["tako", "upgrade"]).unwrap();
        let Some(Commands::Upgrade) = cli.command else {
            panic!("expected Upgrade");
        };
    }

    #[test]
    fn dev_rejects_port_flag() {
        let res = Cli::try_parse_from(["tako", "dev", "--port", "47831"]);
        match res {
            Ok(_) => panic!("expected parse failure"),
            Err(err) => assert!(
                err.to_string().contains("unexpected argument '--port'"),
                "unexpected error: {err}"
            ),
        }
    }

    #[test]
    fn init_parses_runtime_flag() {
        let cli = Cli::try_parse_from(["tako", "init", "--runtime", "deno"]).unwrap();
        let Commands::Init { runtime, .. } = cli.command.expect("command") else {
            panic!("expected Init");
        };
        assert_eq!(runtime.as_deref(), Some("deno"));
    }

    #[test]
    fn init_rejects_unknown_runtime_flag_value() {
        let res = Cli::try_parse_from(["tako", "init", "--runtime", "python"]);
        match res {
            Ok(_) => panic!("expected parse failure"),
            Err(err) => assert!(
                err.to_string().contains("invalid value 'python'"),
                "unexpected error: {err}"
            ),
        }
    }
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new tako project
    Init {
        /// Force overwrite of existing tako.toml
        #[arg(long)]
        force: bool,

        /// Override detected runtime (bun, node, deno)
        #[arg(long, value_parser = ["bun", "node", "deno"])]
        runtime: Option<String>,

        /// Run in this directory (defaults to current directory)
        #[arg(value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
    },

    /// View remote logs
    Logs {
        /// Environment to view logs from
        #[arg(long, default_value = "production")]
        env: String,
    },

    /// Start development server with hot reloading
    Dev {
        /// Force-enable the interactive TUI dashboard (Ratatui)
        #[arg(long, conflicts_with = "no_tui")]
        tui: bool,

        /// Disable the interactive TUI dashboard
        #[arg(long, conflicts_with = "tui")]
        no_tui: bool,

        /// Run as if invoked from this directory (alternative to global `--dir`)
        #[arg(value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
    },

    /// Print a diagnostic report about the local dev server (socket, listener, leases)
    Doctor,

    /// Server management commands
    #[command(subcommand)]
    Servers(server::ServerCommands),

    /// Secret management commands
    #[command(subcommand)]
    Secrets(secret::SecretCommands),

    /// Release history and rollback commands
    #[command(subcommand)]
    Releases(releases::ReleaseCommands),

    /// Upgrade the local tako CLI to the latest version
    Upgrade,

    /// Deploy to an environment
    Deploy {
        /// Environment to deploy to
        #[arg(long)]
        env: Option<String>,

        /// Skip confirmation prompts
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// Run in this directory (defaults to current directory)
        #[arg(value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
    },

    /// Delete a deployed app from an environment
    #[command(visible_aliases = ["rm", "remove", "undeploy", "destroy"])]
    Delete {
        /// Environment to delete from
        #[arg(long)]
        env: Option<String>,

        /// Skip confirmation prompts
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// Run in this directory (defaults to current directory)
        #[arg(value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
    },
}

impl Cli {
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        if self.version {
            println!("{}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }

        let Some(command) = self.command else {
            Cli::command().print_help()?;
            println!();
            return Ok(());
        };

        match command {
            Commands::Init {
                force,
                runtime,
                dir,
            } => {
                if let Some(dir) = dir {
                    std::env::set_current_dir(dir)?;
                }
                commands::init::run(force, runtime.as_deref())
            }
            Commands::Logs { env } => commands::logs::run(&env),
            Commands::Dev { tui, no_tui, dir } => {
                // Dev command needs async runtime
                let rt = tokio::runtime::Runtime::new()?;

                if let Some(dir) = dir {
                    std::env::set_current_dir(dir)?;
                }

                rt.block_on(commands::dev::run(DEV_PUBLIC_PORT, tui, no_tui))
            }
            Commands::Doctor => {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(commands::doctor::run())
            }
            Commands::Servers(cmd) => server::run(cmd),
            Commands::Secrets(cmd) => secret::run(cmd),
            Commands::Releases(cmd) => releases::run(cmd),
            Commands::Upgrade => upgrade::run(),
            Commands::Deploy { env, yes, dir } => {
                if let Some(dir) = dir {
                    std::env::set_current_dir(dir)?;
                }
                commands::deploy::run(env.as_deref(), yes)
            }
            Commands::Delete { env, yes, dir } => {
                if let Some(dir) = dir {
                    std::env::set_current_dir(dir)?;
                }
                delete::run(env.as_deref(), yes)
            }
        }
    }
}
