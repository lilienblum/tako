use clap::{Parser, Subcommand};

use crate::commands::{self, delete, releases, scale, secret, server, upgrade};
use clap::CommandFactory;

const DEV_PUBLIC_PORT: u16 = 47831;
const VERSION_BASE: &str = env!("CARGO_PKG_VERSION");
const VERSION_CANARY_SHA: Option<&str> = option_env!("TAKO_CANARY_SHA");

pub fn display_version() -> String {
    format_display_version(VERSION_BASE, VERSION_CANARY_SHA)
}

fn format_display_version(base: &str, canary_sha: Option<&str>) -> String {
    let Some(raw_sha) = canary_sha else {
        return base.to_owned();
    };
    let sha = raw_sha.trim();
    if sha.is_empty() {
        return base.to_owned();
    }
    let short_sha = &sha[..sha.len().min(7)];
    format!("canary-{short_sha}")
}

/// Tako - Modern application development, deployment, and runtime platform
#[derive(Parser)]
#[command(name = "tako")]
#[command(version, disable_version_flag = true)]
#[command(about = "Tako - Modern application development, deployment, and runtime platform")]
pub struct Cli {
    /// Show version
    #[arg(long)]
    pub version: bool,

    /// Show verbose output
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Deterministic non-interactive output (no colors, no spinners, no prompts)
    #[arg(long, global = true)]
    pub ci: bool,

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
    fn servers_upgrade_parses_without_name() {
        let cli = Cli::try_parse_from(["tako", "servers", "upgrade"]).unwrap();
        let Commands::Servers(server::ServerCommands::Upgrade {
            name,
            canary,
            stable,
        }) = cli.command.expect("command")
        else {
            panic!("expected Servers::Upgrade");
        };
        assert_eq!(name, None);
        assert!(!canary);
        assert!(!stable);
    }

    #[test]
    fn servers_upgrade_parses_with_name() {
        let cli = Cli::try_parse_from(["tako", "servers", "upgrade", "prod"]).unwrap();
        let Commands::Servers(server::ServerCommands::Upgrade {
            name,
            canary,
            stable,
        }) = cli.command.expect("command")
        else {
            panic!("expected Servers::Upgrade");
        };
        assert_eq!(name, Some("prod".to_string()));
        assert!(!canary);
        assert!(!stable);
    }

    #[test]
    fn servers_upgrade_parses_with_canary_flag() {
        let cli = Cli::try_parse_from(["tako", "servers", "upgrade", "prod", "--canary"]).unwrap();
        let Commands::Servers(server::ServerCommands::Upgrade {
            name,
            canary,
            stable,
        }) = cli.command.expect("command")
        else {
            panic!("expected Servers::Upgrade");
        };
        assert_eq!(name, Some("prod".to_string()));
        assert!(canary);
        assert!(!stable);
    }

    #[test]
    fn servers_upgrade_parses_with_stable_flag() {
        let cli = Cli::try_parse_from(["tako", "servers", "upgrade", "prod", "--stable"]).unwrap();
        let Commands::Servers(server::ServerCommands::Upgrade {
            name,
            canary,
            stable,
        }) = cli.command.expect("command")
        else {
            panic!("expected Servers::Upgrade");
        };
        assert_eq!(name, Some("prod".to_string()));
        assert!(!canary);
        assert!(stable);
    }

    #[test]
    fn servers_upgrade_rejects_both_channel_flags() {
        let res =
            Cli::try_parse_from(["tako", "servers", "upgrade", "prod", "--canary", "--stable"]);
        match res {
            Ok(_) => panic!("expected parse failure"),
            Err(err) => assert!(
                err.to_string().contains("cannot be used with"),
                "unexpected error: {err}"
            ),
        }
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
        let Some(Commands::Secrets(secret::SecretCommands::Rm { name, env, .. })) = cli.command else {
            panic!("expected Secrets::Rm");
        };
        assert_eq!(name, "API_KEY");
        assert!(env.is_none());

        let cli = Cli::try_parse_from(["tako", "secrets", "delete", "API_KEY"]).unwrap();
        let Some(Commands::Secrets(secret::SecretCommands::Rm { name, env, .. })) = cli.command else {
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
    fn scale_parses_instances_and_env() {
        let cli = Cli::try_parse_from(["tako", "scale", "3", "--env", "staging"]).unwrap();
        let Some(Commands::Scale {
            instances,
            env,
            server,
            app,
        }) = cli.command
        else {
            panic!("expected Scale");
        };
        assert_eq!(instances, 3);
        assert_eq!(env.as_deref(), Some("staging"));
        assert!(server.is_none());
        assert!(app.is_none());
    }

    #[test]
    fn scale_parses_server_env_and_app() {
        let cli = Cli::try_parse_from([
            "tako",
            "scale",
            "2",
            "--server",
            "la-1",
            "--env",
            "production",
            "--app",
            "my-app",
        ])
        .unwrap();
        let Some(Commands::Scale {
            instances,
            env,
            server,
            app,
        }) = cli.command
        else {
            panic!("expected Scale");
        };
        assert_eq!(instances, 2);
        assert_eq!(env.as_deref(), Some("production"));
        assert_eq!(server.as_deref(), Some("la-1"));
        assert_eq!(app.as_deref(), Some("my-app"));
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
        let Some(Commands::Upgrade { canary, stable }) = cli.command else {
            panic!("expected Upgrade");
        };
        assert!(!canary);
        assert!(!stable);
    }

    #[test]
    fn upgrade_command_parses_canary_flag() {
        let cli = Cli::try_parse_from(["tako", "upgrade", "--canary"]).unwrap();
        let Some(Commands::Upgrade { canary, stable }) = cli.command else {
            panic!("expected Upgrade");
        };
        assert!(canary);
        assert!(!stable);
    }

    #[test]
    fn upgrade_command_parses_stable_flag() {
        let cli = Cli::try_parse_from(["tako", "upgrade", "--stable"]).unwrap();
        let Some(Commands::Upgrade { canary, stable }) = cli.command else {
            panic!("expected Upgrade");
        };
        assert!(!canary);
        assert!(stable);
    }

    #[test]
    fn upgrade_command_rejects_both_channel_flags() {
        let result = Cli::try_parse_from(["tako", "upgrade", "--canary", "--stable"]);
        match result {
            Ok(_) => panic!("expected parse failure"),
            Err(err) => assert!(
                err.to_string().contains("cannot be used with"),
                "unexpected error: {err}"
            ),
        }
    }

    #[test]
    fn upgrade_command_rejects_removed_scope_flags() {
        let result = Cli::try_parse_from(["tako", "upgrade", "--servers-only"]);
        match result {
            Ok(_) => panic!("expected parse failure"),
            Err(err) => assert!(
                err.to_string()
                    .contains("unexpected argument '--servers-only'"),
                "unexpected error: {err}"
            ),
        }
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
    fn dev_default_parses_without_subcommand() {
        let cli = Cli::try_parse_from(["tako", "dev"]).unwrap();
        let Commands::Dev { command, args } = cli.command.expect("command") else {
            panic!("expected Dev");
        };
        assert!(command.is_none());
        assert!(args.dir.is_none());
        assert!(args.name.is_none());
    }

    #[test]
    fn dev_parses_name_flag() {
        let cli = Cli::try_parse_from(["tako", "dev", "--name", "my-app"]).unwrap();
        let Commands::Dev { command, args } = cli.command.expect("command") else {
            panic!("expected Dev");
        };
        assert!(command.is_none());
        assert_eq!(args.name.as_deref(), Some("my-app"));
    }

    #[test]
    fn dev_stop_parses() {
        let cli = Cli::try_parse_from(["tako", "dev", "stop"]).unwrap();
        let Commands::Dev { command, .. } = cli.command.expect("command") else {
            panic!("expected Dev");
        };
        match command {
            Some(DevSubcommands::Stop { name, all }) => {
                assert!(name.is_none());
                assert!(!all);
            }
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn dev_stop_with_name_parses() {
        let cli = Cli::try_parse_from(["tako", "dev", "stop", "my-app"]).unwrap();
        let Commands::Dev { command, .. } = cli.command.expect("command") else {
            panic!("expected Dev");
        };
        match command {
            Some(DevSubcommands::Stop { name, all }) => {
                assert_eq!(name.as_deref(), Some("my-app"));
                assert!(!all);
            }
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn dev_stop_all_parses() {
        let cli = Cli::try_parse_from(["tako", "dev", "stop", "--all"]).unwrap();
        let Commands::Dev { command, .. } = cli.command.expect("command") else {
            panic!("expected Dev");
        };
        match command {
            Some(DevSubcommands::Stop { name, all }) => {
                assert!(name.is_none());
                assert!(all);
            }
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn dev_ls_parses() {
        let cli = Cli::try_parse_from(["tako", "dev", "ls"]).unwrap();
        let Commands::Dev { command, .. } = cli.command.expect("command") else {
            panic!("expected Dev");
        };
        assert!(matches!(command, Some(DevSubcommands::Ls)));
    }

    #[test]
    fn dev_list_alias_parses() {
        let cli = Cli::try_parse_from(["tako", "dev", "list"]).unwrap();
        let Commands::Dev { command, .. } = cli.command.expect("command") else {
            panic!("expected Dev");
        };
        assert!(matches!(command, Some(DevSubcommands::Ls)));
    }

    #[test]
    fn init_parses_without_runtime_flag() {
        let cli = Cli::try_parse_from(["tako", "init"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Init { .. })));
    }

    #[test]
    fn display_version_without_canary_sha_uses_base_version() {
        let version = format_display_version("1.2.3", None);
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn display_version_with_full_canary_sha_uses_short_hash() {
        let version = format_display_version("1.2.3", Some("0123456789abcdef"));
        assert_eq!(version, "canary-0123456");
    }

    #[test]
    fn display_version_with_short_canary_sha_keeps_full_value() {
        let version = format_display_version("1.2.3", Some("abc"));
        assert_eq!(version, "canary-abc");
    }

    #[test]
    fn display_version_with_blank_canary_sha_uses_base_version() {
        let version = format_display_version("1.2.3", Some("   "));
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn ci_flag_parses_globally() {
        let cli = Cli::try_parse_from(["tako", "--ci", "deploy"]).unwrap();
        assert!(cli.ci);
    }

    #[test]
    fn ci_and_verbose_flags_combine() {
        let cli = Cli::try_parse_from(["tako", "--ci", "-v", "deploy"]).unwrap();
        assert!(cli.ci);
        assert!(cli.verbose);
    }

    #[test]
    fn ci_flag_after_subcommand_parses() {
        let cli = Cli::try_parse_from(["tako", "deploy", "--ci"]).unwrap();
        assert!(cli.ci);
    }
}

#[derive(clap::Args, Debug)]
pub struct DevArgs {
    /// Run as if invoked from this directory
    #[arg(value_name = "DIR")]
    pub dir: Option<std::path::PathBuf>,

    /// Override app name (used for domain and display)
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum DevSubcommands {
    /// Stop a running dev app
    Stop {
        /// App name (defaults to current directory's app)
        name: Option<String>,
        /// Stop all registered apps
        #[arg(long)]
        all: bool,
    },
    /// List registered dev apps
    #[command(visible_alias = "list")]
    Ls,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new tako project
    Init {
        /// Run in this directory (defaults to current directory)
        #[arg(value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
    },

    /// View remote logs
    Logs {
        /// Environment to view logs from (defaults to production)
        #[arg(long)]
        env: Option<String>,

        /// Stream logs continuously
        #[arg(long, conflicts_with = "days")]
        tail: bool,

        /// Number of days of history to show (default: 3)
        #[arg(long, default_value = "3")]
        days: u32,

        /// Run in this directory (defaults to current directory)
        #[arg(value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
    },

    /// Start development server with hot reloading
    #[command(args_conflicts_with_subcommands = true)]
    Dev {
        #[command(subcommand)]
        command: Option<DevSubcommands>,

        #[command(flatten)]
        args: DevArgs,
    },

    /// Print a local diagnostic report
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
    Upgrade {
        /// Install latest canary build instead of stable release
        #[arg(long, conflicts_with = "stable")]
        canary: bool,

        /// Install latest stable build and set default channel to stable
        #[arg(long, conflicts_with = "canary")]
        stable: bool,
    },

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

    /// Change the desired instance count for a deployed app
    Scale {
        /// Desired instance count per targeted server
        instances: u8,

        /// Environment to scale
        #[arg(long)]
        env: Option<String>,

        /// Specific server to scale
        #[arg(long)]
        server: Option<String>,

        /// App name (required outside a project directory)
        #[arg(long)]
        app: Option<String>,
    },
}

impl Cli {
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        if self.version {
            println!("{}", display_version());
            return Ok(());
        }

        let Some(command) = self.command else {
            Cli::command().print_help()?;
            println!();
            return Ok(());
        };

        match command {
            Commands::Init { dir } => {
                if let Some(dir) = dir {
                    std::env::set_current_dir(dir)?;
                }
                commands::init::run()
            }
            Commands::Logs {
                env,
                tail,
                days,
                dir,
            } => {
                if let Some(dir) = dir {
                    std::env::set_current_dir(dir)?;
                }
                commands::logs::run(env.as_deref(), tail, days)
            }
            Commands::Dev { command, args } => {
                let rt = tokio::runtime::Runtime::new()?;

                if let Some(dir) = args.dir {
                    std::env::set_current_dir(dir)?;
                }

                match command {
                    None => rt.block_on(commands::dev::run(DEV_PUBLIC_PORT, args.name)),
                    Some(DevSubcommands::Stop { name, all }) => {
                        rt.block_on(commands::dev::stop(name, all))
                    }
                    Some(DevSubcommands::Ls) => rt.block_on(commands::dev::ls()),
                }
            }
            Commands::Doctor => {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(commands::doctor::run())
            }
            Commands::Servers(cmd) => server::run(cmd),
            Commands::Secrets(cmd) => secret::run(cmd),
            Commands::Releases(cmd) => releases::run(cmd),
            Commands::Upgrade { canary, stable } => upgrade::run(canary, stable),
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
            Commands::Scale {
                instances,
                env,
                server,
                app,
            } => scale::run(instances, env.as_deref(), server.as_deref(), app.as_deref()),
        }
    }
}
